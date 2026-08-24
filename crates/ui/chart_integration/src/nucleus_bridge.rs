//! Nucleus engine installation and fixed-point conversion helpers.

use crate::bridge::MergedChartData;
use crate::view::ChartType;
use axiusflow_application::{ProvenancedMarketBar, ReplaySnapshot};
use nucleuscharts_engine::{
    ChartEngine, FeatureDataPoint, FeatureSeriesKind, FeatureSeriesOptionsPatch, FeatureValue,
    PriceScaleTarget, SeriesKind,
};
use num_traits::ToPrimitive;
use std::num::NonZeroUsize;

const DEFAULT_CHART_DATA_QUEUE_CAPACITY: usize = 64;

#[derive(Clone, Debug, Default)]
pub(crate) struct ProductPriceBars {
    times: Vec<f64>,
    open: Vec<f64>,
    high: Vec<f64>,
    low: Vec<f64>,
    close: Vec<f64>,
}

impl ProductPriceBars {
    fn is_empty(&self) -> bool {
        self.times.is_empty()
    }

    fn replace(
        &mut self,
        times: Vec<f64>,
        open: Vec<f64>,
        high: Vec<f64>,
        low: Vec<f64>,
        close: Vec<f64>,
    ) {
        self.times = times;
        self.open = open;
        self.high = high;
        self.low = low;
        self.close = close;
    }

    fn update_bar(&mut self, time: f64, ohlc: [f64; 4]) {
        match self
            .times
            .iter()
            .position(|&existing| existing.to_bits() >= time.to_bits())
        {
            Some(index) if self.times[index].to_bits() == time.to_bits() => {
                self.open[index] = ohlc[0];
                self.high[index] = ohlc[1];
                self.low[index] = ohlc[2];
                self.close[index] = ohlc[3];
            }
            Some(index) => {
                self.times.insert(index, time);
                self.open.insert(index, ohlc[0]);
                self.high.insert(index, ohlc[1]);
                self.low.insert(index, ohlc[2]);
                self.close.insert(index, ohlc[3]);
            }
            None => {
                self.times.push(time);
                self.open.push(ohlc[0]);
                self.high.push(ohlc[1]);
                self.low.push(ohlc[2]);
                self.close.push(ohlc[3]);
            }
        }
    }
}

pub(crate) fn chart_data_queue_capacity() -> NonZeroUsize {
    NonZeroUsize::new(DEFAULT_CHART_DATA_QUEUE_CAPACITY).unwrap_or(NonZeroUsize::MIN)
}

pub(crate) fn replay_price_divisor(replay: &ReplaySnapshot) -> f64 {
    10_f64.powi(i32::from(replay.instrument().precision.price_scale()))
}

pub(crate) fn install_volume_series(engine: &mut ChartEngine) -> u32 {
    let id = engine.add_series(SeriesKind::Histogram);
    if let Some(series) = engine
        .series
        .iter_mut()
        .find(|series| series.id == id && !series.removed)
    {
        series.visible = false;
        series.histogram_updown = true;
        series.title = "Volume".to_string();
        series.title_visible = true;
    } else {
        debug_assert!(false, "new Nucleus series identity must resolve");
    }
    engine.set_series_price_scale(id, PriceScaleTarget::Overlay);
    engine.set_price_scale_margins_for(0, PriceScaleTarget::Overlay, 0.8, 0.0);
    let applied = engine.series_apply_price_format_json(id, r#"{"type":"volume"}"#);
    debug_assert!(applied);
    id
}

pub(crate) fn apply_merged_chart_data(
    engine: &mut ChartEngine,
    volume_series: u32,
    price_divisor: &mut f64,
    chart_type: ChartType,
    product_bars: &mut ProductPriceBars,
    update: &MergedChartData,
) {
    if let Some(snapshot) = update.snapshot() {
        *price_divisor = replay_price_divisor(snapshot);
        install_replay_with_deltas(
            engine,
            volume_series,
            snapshot,
            update.accepted_deltas(),
            chart_type,
            product_bars,
        );
        return;
    }

    let rows = update
        .accepted_deltas()
        .iter()
        .map(|item| {
            let bar = *item.value();
            let time = item
                .provenance()
                .exchange_timestamp_unix_nanos
                .to_f64()
                .unwrap_or_default()
                / 1_000_000_000.0;
            (
                time,
                [
                    fixed_price(bar.open, *price_divisor),
                    fixed_price(bar.high, *price_divisor),
                    fixed_price(bar.low, *price_divisor),
                    fixed_price(bar.close, *price_divisor),
                ],
                volume_row(bar.volume, time),
            )
        })
        .collect::<Vec<_>>();
    for (time, ohlc, _) in &rows {
        product_bars.update_bar(*time, *ohlc);
    }
    let accepted = if chart_type == ChartType::BrushableArea {
        rows.iter()
            .filter(|(time, ohlc, _)| {
                engine
                    .update_feature_series_data(
                        0,
                        FeatureDataPoint {
                            time: *time,
                            value: Some(FeatureValue::BrushableArea { value: ohlc[3] }),
                        },
                    )
                    .is_ok()
            })
            .count()
    } else {
        engine.update_series_bars(0, rows.iter().map(|(time, ohlc, _)| (*time, *ohlc)))
    };
    debug_assert_eq!(accepted, update.accepted_deltas().len());
    let accepted_volume =
        engine.update_series_bars(volume_series, rows.into_iter().map(|(_, _, volume)| volume));
    debug_assert_eq!(accepted_volume, update.accepted_deltas().len());
}

pub(crate) fn install_replay(
    engine: &mut ChartEngine,
    volume_series: u32,
    replay: &ReplaySnapshot,
    chart_type: ChartType,
    product_bars: &mut ProductPriceBars,
) {
    install_replay_with_deltas(engine, volume_series, replay, &[], chart_type, product_bars);
}

pub(crate) fn install_product_price_series(
    engine: &mut ChartEngine,
    chart_type: ChartType,
    product_bars: &ProductPriceBars,
) {
    if product_bars.is_empty() {
        if chart_type == ChartType::BrushableArea {
            engine.configure_feature_series(
                0,
                FeatureSeriesKind::BrushableArea,
                FeatureSeriesOptionsPatch::default(),
            );
        } else {
            apply_product_series_kind(engine, chart_type);
        }
        return;
    }
    if chart_type == ChartType::BrushableArea {
        engine.configure_feature_series(
            0,
            FeatureSeriesKind::BrushableArea,
            FeatureSeriesOptionsPatch::default(),
        );
        let points = product_bars
            .times
            .iter()
            .zip(product_bars.close.iter())
            .map(|(&time, &value)| FeatureDataPoint {
                time,
                value: Some(FeatureValue::BrushableArea { value }),
            })
            .collect();
        let _ = engine.set_feature_series_data(0, points);
    } else {
        apply_product_series_kind(engine, chart_type);
        let _ = engine.set_series_data(
            0,
            &product_bars.times,
            &product_bars.open,
            &product_bars.high,
            &product_bars.low,
            &product_bars.close,
        );
    }
}

fn apply_product_series_kind(engine: &mut ChartEngine, chart_type: ChartType) {
    if let Some(kind) = chart_type.series_kind() {
        engine.convert_series_kind(0, kind);
    }
}

pub(crate) fn install_replay_with_deltas(
    engine: &mut ChartEngine,
    volume_series: u32,
    replay: &ReplaySnapshot,
    deltas: &[ProvenancedMarketBar],
    chart_type: ChartType,
    product_bars: &mut ProductPriceBars,
) {
    let item_count = replay.bars().len().saturating_add(deltas.len());
    let mut times = Vec::with_capacity(item_count);
    let mut open = Vec::with_capacity(item_count);
    let mut high = Vec::with_capacity(item_count);
    let mut low = Vec::with_capacity(item_count);
    let mut close = Vec::with_capacity(item_count);
    let mut volume = Vec::with_capacity(item_count);
    let price_divisor = replay_price_divisor(replay);

    for item in replay.bars().iter().chain(deltas) {
        let bar = *item.value();
        times.push(
            item.provenance()
                .exchange_timestamp_unix_nanos
                .to_f64()
                .unwrap_or_default()
                / 1_000_000_000.0,
        );
        open.push(fixed_price(bar.open, price_divisor));
        high.push(fixed_price(bar.high, price_divisor));
        low.push(fixed_price(bar.low, price_divisor));
        close.push(fixed_price(bar.close, price_divisor));
        volume.push(bar.volume.to_f64().unwrap_or_default());
    }

    product_bars.replace(times, open, high, low, close);
    install_product_price_series(engine, chart_type, product_bars);
    apply_price_series_chrome(engine, replay.instrument().symbol.as_str());
    let _ = engine.set_series_data(
        volume_series,
        &product_bars.times,
        &volume,
        &volume,
        &volume,
        &volume,
    );
}

fn apply_price_series_chrome(engine: &mut ChartEngine, title: &str) {
    let Some(series) = engine
        .series
        .iter_mut()
        .find(|series| series.id == 0 && !series.removed)
    else {
        debug_assert!(false, "product-owned price series identity must resolve");
        return;
    };
    if series.title != title {
        series.title = title.to_string();
    }
}

fn volume_row(volume: i64, time: f64) -> (f64, [f64; 4]) {
    let volume = volume.to_f64().unwrap_or_default();
    (time, [volume; 4])
}

pub(crate) fn fixed_price(value: i64, divisor: f64) -> f64 {
    value.to_f64().unwrap_or_default() / divisor
}
