//! Origin engine installation and fixed-point conversion helpers.

use crate::bridge::MergedChartData;
use axiusflow_application::{ProvenancedMarketBar, ReplaySnapshot};
use num_traits::ToPrimitive;
use origin_engine::{ChartEngine, SeriesKind};
use std::num::NonZeroUsize;

const DEFAULT_CHART_DATA_QUEUE_CAPACITY: usize = 64;

pub(crate) fn chart_data_queue_capacity() -> NonZeroUsize {
    NonZeroUsize::new(DEFAULT_CHART_DATA_QUEUE_CAPACITY).unwrap_or(NonZeroUsize::MIN)
}

pub(crate) fn replay_price_divisor(replay: &ReplaySnapshot) -> f64 {
    10_f64.powi(i32::from(replay.instrument().precision.price_scale()))
}

pub(crate) fn apply_merged_chart_data(
    engine: &mut ChartEngine,
    price_divisor: &mut f64,
    update: &MergedChartData,
) {
    if let Some(snapshot) = update.snapshot() {
        *price_divisor = replay_price_divisor(snapshot);
        install_replay_with_deltas(engine, snapshot, update.accepted_deltas());
        return;
    }

    let rows = update.accepted_deltas().iter().map(|item| {
        let bar = *item.value();
        (
            item.provenance()
                .exchange_timestamp_unix_nanos
                .to_f64()
                .expect("validated replay timestamps fit f64")
                / 1_000_000_000.0,
            [
                fixed_price(bar.open, *price_divisor),
                fixed_price(bar.high, *price_divisor),
                fixed_price(bar.low, *price_divisor),
                fixed_price(bar.close, *price_divisor),
            ],
        )
    });
    let accepted = engine.update_series_bars(0, rows);
    debug_assert_eq!(accepted, update.accepted_deltas().len());
}

pub(crate) fn install_replay(engine: &mut ChartEngine, replay: &ReplaySnapshot) {
    install_replay_with_deltas(engine, replay, &[]);
}

pub(crate) fn install_replay_with_deltas(
    engine: &mut ChartEngine,
    replay: &ReplaySnapshot,
    deltas: &[ProvenancedMarketBar],
) {
    let item_count = replay.bars().len().saturating_add(deltas.len());
    let mut times = Vec::with_capacity(item_count);
    let mut open = Vec::with_capacity(item_count);
    let mut high = Vec::with_capacity(item_count);
    let mut low = Vec::with_capacity(item_count);
    let mut close = Vec::with_capacity(item_count);
    let price_divisor = replay_price_divisor(replay);

    for item in replay.bars().iter().chain(deltas) {
        let bar = *item.value();
        times.push(
            item.provenance()
                .exchange_timestamp_unix_nanos
                .to_f64()
                .expect("validated replay timestamps fit f64")
                / 1_000_000_000.0,
        );
        open.push(fixed_price(bar.open, price_divisor));
        high.push(fixed_price(bar.high, price_divisor));
        low.push(fixed_price(bar.low, price_divisor));
        close.push(fixed_price(bar.close, price_divisor));
    }

    engine
        .set_series_data(0, &times, &open, &high, &low, &close)
        .expect("validated replay columns satisfy Origin's data contract");
    engine.series[0].kind = SeriesKind::Candlestick;
}

pub(crate) fn fixed_price(value: i64, divisor: f64) -> f64 {
    value
        .to_f64()
        .expect("validated fixed-point price fits f64")
        / divisor
}
