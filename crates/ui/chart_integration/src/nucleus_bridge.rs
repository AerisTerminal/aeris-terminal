//! Nucleus engine installation and fixed-point conversion helpers.

use crate::bridge::MergedChartData;
use axiusflow_application::{ProvenancedMarketBar, ReplaySnapshot};
use nucleuscharts_engine::{ChartEngine, PriceScaleTarget, SeriesKind};
use num_traits::ToPrimitive;
use std::num::NonZeroUsize;

const DEFAULT_CHART_DATA_QUEUE_CAPACITY: usize = 64;

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
    update: &MergedChartData,
) {
    if let Some(snapshot) = update.snapshot() {
        *price_divisor = replay_price_divisor(snapshot);
        install_replay_with_deltas(engine, volume_series, snapshot, update.accepted_deltas());
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
    let accepted = engine.update_series_bars(0, rows.iter().map(|(time, ohlc, _)| (*time, *ohlc)));
    debug_assert_eq!(accepted, update.accepted_deltas().len());
    let accepted_volume =
        engine.update_series_bars(volume_series, rows.into_iter().map(|(_, _, volume)| volume));
    debug_assert_eq!(accepted_volume, update.accepted_deltas().len());
}

pub(crate) fn install_replay(
    engine: &mut ChartEngine,
    volume_series: u32,
    replay: &ReplaySnapshot,
) {
    install_replay_with_deltas(engine, volume_series, replay, &[]);
}

pub(crate) fn install_replay_with_deltas(
    engine: &mut ChartEngine,
    volume_series: u32,
    replay: &ReplaySnapshot,
    deltas: &[ProvenancedMarketBar],
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

    // A rejected install keeps the previous series rather than panicking the
    // UI thread; upstream validation makes this unreachable.
    if engine
        .set_series_data(0, &times, &open, &high, &low, &close)
        .is_err()
    {
        return;
    }
    engine.series[0].kind = SeriesKind::Candlestick;
    apply_price_series_chrome(engine, replay.instrument().symbol.as_str());
    let _ = engine.set_series_data(volume_series, &times, &volume, &volume, &volume, &volume);
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
