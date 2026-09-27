//! Nucleus engine installation and fixed-point conversion helpers.

use crate::bridge::MergedChartData;
use crate::view::ChartType;
use aeris_application::{ProvenancedMarketBar, ReplaySnapshot};
use aeris_charts_engine::{ChartEngine, PriceScaleTarget, SeriesKind};
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
    /// Resolve a fractional bar index to exchange time, including points between bars.
    /// The edge segment also places offscreen anchors without pinning them to the
    /// first or last loaded candle.
    pub(crate) fn time_at_logical(&self, logical: f64) -> Option<f64> {
        if !logical.is_finite() {
            return None;
        }
        if self.times.len() == 1 && logical == 0.0 {
            return self.times.first().copied();
        }
        if self.times.len() < 2 {
            return None;
        }
        let segment = logical
            .floor()
            .max(0.0)
            .to_usize()
            .unwrap_or(usize::MAX)
            .min(self.times.len() - 2);
        let start = self.times[segment];
        let end = self.times[segment + 1];
        let time = (end - start).mul_add(logical - segment.to_f64()?, start);
        time.is_finite().then_some(time)
    }

    /// Project exchange time onto the current bar series, preserving fractional
    /// placement rather than snapping to a candle on a timeframe change.
    pub(crate) fn logical_at_time(&self, time: f64) -> Option<f64> {
        if !time.is_finite() {
            return None;
        }
        if self.times.len() == 1 && self.times[0].to_bits() == time.to_bits() {
            return Some(0.0);
        }
        if self.times.len() < 2 {
            return None;
        }
        let segment = self
            .times
            .partition_point(|&bar_time| bar_time <= time)
            .saturating_sub(1)
            .min(self.times.len() - 2);
        let start = self.times[segment];
        let span = self.times[segment + 1] - start;
        if !span.is_finite() || span <= 0.0 {
            return None;
        }
        let logical = (time - start) / span + segment.to_f64()?;
        logical.is_finite().then_some(logical)
    }

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

pub(crate) fn replay_quantity_divisor(replay: &ReplaySnapshot) -> f64 {
    10_f64.powi(i32::from(replay.instrument().precision.quantity_scale()))
}

/// Presentation precision, not a price tick or a change to fixed-point storage.
/// Keep at least two places where the storage scale permits it, and never hide
/// a significant decimal in the displayed OHLC data.
pub(crate) fn price_display_precision(values: impl IntoIterator<Item = i64>, scale: u8) -> u8 {
    values
        .into_iter()
        .fold(scale.min(2), |precision, mut value| {
            let mut places = scale;
            while places > 0 && value % 10 == 0 {
                value /= 10;
                places -= 1;
            }
            precision.max(places)
        })
}

pub(crate) fn replay_display_precision(replay: &ReplaySnapshot) -> u8 {
    price_display_precision(
        replay.bars().iter().flat_map(|item| {
            let bar = item.value();
            [bar.open, bar.high, bar.low, bar.close]
        }),
        replay.instrument().precision.price_scale(),
    )
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
        // Volume is the one catalog indicator built here rather than by Nucleus's indicator
        // factory, so it has to opt out of the price-series countdown default itself: the bar
        // close it would count down to is the price series' own, already shown under the price.
        series.countdown_visible = false;
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
    quantity_divisor: &mut f64,
    chart_type: ChartType,
    product_bars: &mut ProductPriceBars,
    update: &MergedChartData,
) {
    if let Some(snapshot) = update.snapshot() {
        *price_divisor = replay_price_divisor(snapshot);
        *quantity_divisor = replay_quantity_divisor(snapshot);
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
                volume_row(bar.volume, *quantity_divisor, time),
            )
        })
        .collect::<Vec<_>>();
    for (time, ohlc, _) in &rows {
        product_bars.update_bar(*time, *ohlc);
    }
    if chart_type != ChartType::Footprint {
        let accepted =
            engine.update_series_bars(0, rows.iter().map(|(time, ohlc, _)| (*time, *ohlc)));
        debug_assert_eq!(accepted, update.accepted_deltas().len());
    }
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
    if chart_type == ChartType::Footprint {
        return;
    }
    apply_product_series_kind(engine, chart_type);
    if product_bars.is_empty() {
        return;
    }
    let _ = engine.set_series_data(
        0,
        &product_bars.times,
        &product_bars.open,
        &product_bars.high,
        &product_bars.low,
        &product_bars.close,
    );
}

fn apply_product_series_kind(engine: &mut ChartEngine, chart_type: ChartType) {
    engine.convert_series_kind(0, chart_type.series_kind());
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
    let quantity_divisor = replay_quantity_divisor(replay);

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
        volume.push(fixed_value(bar.volume, quantity_divisor));
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

pub(crate) fn replay_legend_title(replay: &ReplaySnapshot) -> String {
    let instrument = replay.instrument();
    format!(
        "{} · {} · {}",
        instrument.symbol,
        replay_timeframe_label(replay),
        replay_venue_label(&instrument.venue_id)
    )
}

fn replay_timeframe_label(replay: &ReplaySnapshot) -> String {
    let definition = replay.bar_definition();
    if let Some(trades) = definition.trades_per_bar {
        return format!("{trades}t");
    }
    if let Some(months) = definition.calendar_months {
        return format!("{months}M");
    }
    let seconds = definition.interval_seconds;
    if seconds.is_multiple_of(86_400) {
        format!("{}D", seconds / 86_400)
    } else if seconds.is_multiple_of(3_600) {
        format!("{}h", seconds / 3_600)
    } else if seconds.is_multiple_of(60) {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

fn replay_venue_label(venue: &str) -> &str {
    if venue.eq_ignore_ascii_case("rithmic") {
        "Rithmic"
    } else {
        venue
    }
}

fn volume_row(volume: i64, divisor: f64, time: f64) -> (f64, [f64; 4]) {
    let volume = fixed_value(volume, divisor);
    (time, [volume; 4])
}

pub(crate) fn fixed_price(value: i64, divisor: f64) -> f64 {
    fixed_value(value, divisor)
}

fn fixed_value(value: i64, divisor: f64) -> f64 {
    value.to_f64().unwrap_or_default() / divisor
}

#[cfg(test)]
mod tests {
    use super::{ProductPriceBars, fixed_value, price_display_precision};

    #[test]
    fn drawing_anchor_keeps_its_time_across_bar_intervals() {
        let minute = ProductPriceBars {
            times: vec![0.0, 60.0, 120.0, 180.0, 240.0, 300.0],
            ..ProductPriceBars::default()
        };
        let five_minute = ProductPriceBars {
            times: vec![0.0, 300.0, 600.0],
            ..ProductPriceBars::default()
        };
        let time = minute.time_at_logical(2.5).expect("source anchor time");
        assert!((time - 150.0).abs() < f64::EPSILON);
        assert_eq!(five_minute.logical_at_time(time), Some(0.5));
        assert_eq!(minute.logical_at_time(time), Some(2.5));
        assert_eq!(five_minute.logical_at_time(900.0), Some(3.0));
    }

    #[test]
    fn display_precision_removes_padding_without_hiding_significant_digits() {
        assert_eq!(price_display_precision([7_978_500_000_000], 8), 2);
        assert_eq!(price_display_precision([12_345, 0], 8), 8);
        assert_eq!(price_display_precision([12_340_000], 8), 4);
        assert_eq!(price_display_precision([-12_340_000], 8), 4);
        assert_eq!(price_display_precision([123], 0), 0);
        assert_eq!(price_display_precision([123], 2), 2);
        assert_eq!(
            price_display_precision([7_978_500_000_000, 7_978_500_000_001], 8),
            8
        );
    }

    #[test]
    fn fixed_point_volume_uses_quantity_precision() {
        assert!((fixed_value(125_000_000, 100_000_000.0) - 1.25).abs() < f64::EPSILON);
    }
}
