//! Runtime-owned sweep classification over the retained trade tape.
//!
//! A sweep is a run of consecutive same-aggressor prints that crosses at least
//! two price levels inside a 100 ms window and together outsizes the 90th
//! percentile of the trailing [`SWEEP_CLASSIFICATION_WINDOW`] prints. The
//! annotation is recomputed once per ingested trade over that trailing window,
//! so a published tape carries its sweeps instead of every consumer
//! reclassifying it. Provider order is preserved; no opaque trade identifier is
//! interpreted as sequence evidence.
//!
//! The classifier works on the same scaled presentation values the desktop
//! time-and-sales used (`f64` price and volume); the raw fixed-point trade
//! stays authoritative in [`crate::RetainedMarketTrade`].

use std::collections::VecDeque;
use std::sync::Arc;

use num_traits::ToPrimitive;

use crate::{MarketTradeSweep, RetainedMarketTrade};

/// The trailing prints the sweep classifier reads; every tape consumer used it.
pub(crate) const SWEEP_CLASSIFICATION_WINDOW: usize = 512;

const SWEEP_AGGREGATION_WINDOW_MICROS: i64 = 100_000;

/// One retained print reduced to the exact values the classifier reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SweepClassifierTrade {
    pub(crate) ingestion_ordinal: u64,
    pub(crate) timestamp_micros: i64,
    pub(crate) price: f64,
    pub(crate) volume: f64,
    pub(crate) aggressor: aeris_market_data::AggressorSide,
}

fn scaled_market_value(value: i64, scale: u8) -> Option<f64> {
    let divisor = 10_f64.powi(i32::from(scale));
    let converted = value.to_f64()? / divisor;
    converted.is_finite().then_some(converted)
}

/// The classifier view of one retained trade, or `None` when its price or
/// size cannot be represented finitely. Such a print stays on the tape but
/// takes no part in sweep detection, mirroring the desktop projection that
/// skipped an unrepresentable print.
fn classifier_trade(
    retained: &RetainedMarketTrade,
    price_scale: u8,
    quantity_scale: u8,
) -> Option<SweepClassifierTrade> {
    let trade = retained.trade.as_ref();
    let timestamps = &trade.metadata.timestamps;
    let timestamp_nanos = timestamps
        .exchange_unix_nanos
        .or(timestamps.provider_unix_nanos)
        .unwrap_or(timestamps.received_unix_nanos);
    Some(SweepClassifierTrade {
        ingestion_ordinal: retained.ingestion_ordinal,
        timestamp_micros: timestamp_nanos.div_euclid(1_000),
        price: scaled_market_value(trade.price, price_scale)?,
        volume: scaled_market_value(trade.quantity, quantity_scale)?,
        aggressor: trade.aggressor,
    })
}

/// Classifies the trailing [`SWEEP_CLASSIFICATION_WINDOW`] of the retained
/// tape. Called once per ingested trade and whenever retention evicts prints,
/// so the published sweeps always describe exactly the retained window.
pub(crate) fn classify_recent_trade_sweeps(
    recent_trades: &VecDeque<RetainedMarketTrade>,
    price_scale: u8,
    quantity_scale: u8,
) -> Arc<[MarketTradeSweep]> {
    let start = recent_trades
        .len()
        .saturating_sub(SWEEP_CLASSIFICATION_WINDOW);
    let trades = recent_trades
        .iter()
        .skip(start)
        .filter_map(|retained| classifier_trade(retained, price_scale, quantity_scale))
        .collect::<Vec<_>>();
    classify_trade_sweeps(&trades).into()
}

/// The 90th percentile of the positive print volumes, so a sweep is a run that together outsizes
/// nine in ten recent prints. `f64::MAX` when there is no positive print.
fn sweep_volume_threshold(trades: &[SweepClassifierTrade]) -> f64 {
    let mut volumes = trades
        .iter()
        .map(|trade| trade.volume)
        .filter(|volume| volume.is_finite() && *volume > 0.0)
        .collect::<Vec<_>>();
    if volumes.is_empty() {
        return f64::MAX;
    }
    let index = volumes.len().saturating_mul(9).saturating_sub(1) / 10;
    volumes.select_nth_unstable_by(index, f64::total_cmp);
    volumes[index]
}

/// Groups consecutive aggressor-side prints that cross at least two price
/// levels inside the bounded sweep window and outsize the recent prints.
/// Provider order is preserved; no opaque trade identifier is interpreted as
/// sequence evidence.
pub(crate) fn classify_trade_sweeps(trades: &[SweepClassifierTrade]) -> Vec<MarketTradeSweep> {
    let threshold = sweep_volume_threshold(trades);
    let mut sweeps = Vec::new();
    let mut start = 0;
    while start < trades.len() {
        let first = &trades[start];
        if first.aggressor == aeris_market_data::AggressorSide::Unknown {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        let mut total_volume = first.volume;
        let mut levels = 1_u32;
        let mut previous_price = first.price;
        while let Some(next) = trades.get(end) {
            let previous = &trades[end - 1];
            let within_window = next.timestamp_micros >= previous.timestamp_micros
                && next
                    .timestamp_micros
                    .saturating_sub(previous.timestamp_micros)
                    <= SWEEP_AGGREGATION_WINDOW_MICROS;
            let price_continues = match first.aggressor {
                aeris_market_data::AggressorSide::Buy => next.price >= previous_price,
                aeris_market_data::AggressorSide::Sell => next.price <= previous_price,
                aeris_market_data::AggressorSide::Unknown => false,
            };
            if next.aggressor != first.aggressor || !within_window || !price_continues {
                break;
            }
            if next.price.to_bits() != previous_price.to_bits() {
                levels = levels.saturating_add(1);
            }
            total_volume += next.volume;
            previous_price = next.price;
            end += 1;
        }
        if levels >= 2 && total_volume >= threshold {
            let terminal = &trades[end - 1];
            sweeps.push(MarketTradeSweep {
                first_ingestion_ordinal: first.ingestion_ordinal,
                last_ingestion_ordinal: terminal.ingestion_ordinal,
                timestamp_micros: terminal.timestamp_micros,
                terminal_price: terminal.price,
                total_volume,
                price_levels: levels,
                aggressor: first.aggressor,
            });
        }
        start = end.max(start + 1);
    }
    sweeps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifier_trade(ordinal: u64, timestamp_micros: i64, volume: f64) -> SweepClassifierTrade {
        SweepClassifierTrade {
            ingestion_ordinal: ordinal,
            timestamp_micros,
            price: 100.0 + ordinal.to_f64().expect("small ordinal") * 0.25,
            volume,
            aggressor: if ordinal.is_multiple_of(2) {
                aeris_market_data::AggressorSide::Sell
            } else {
                aeris_market_data::AggressorSide::Buy
            },
        }
    }

    #[test]
    fn sweep_classifier_requires_same_side_time_proximity_and_multiple_levels() {
        let mut trades = vec![
            classifier_trade(1, 1_000_000, 6.0),
            classifier_trade(3, 1_020_000, 7.0),
            classifier_trade(5, 1_040_000, 8.0),
        ];
        for (index, trade) in trades.iter_mut().enumerate() {
            trade.ingestion_ordinal = u64::try_from(index + 1).expect("small index");
            trade.aggressor = aeris_market_data::AggressorSide::Buy;
            trade.price = 100.0 + index.to_f64().expect("small index") * 0.25;
        }
        let sweeps = classify_trade_sweeps(&trades);
        assert_eq!(sweeps.len(), 1);
        assert_eq!(sweeps[0].price_levels, 3);
        assert!((sweeps[0].total_volume - 21.0).abs() < f64::EPSILON);

        trades[1].aggressor = aeris_market_data::AggressorSide::Sell;
        assert_eq!(classify_trade_sweeps(&trades), [] as [MarketTradeSweep; 0]);
    }

    #[test]
    fn sweeps_must_outsize_nine_in_ten_recent_prints() {
        // Nine one-lot prints and a two-level buy run of 2 + 3 lots.
        let mut trades = (1..=9)
            .map(|ordinal| {
                classifier_trade(
                    ordinal,
                    i64::try_from(ordinal).expect("small") * 1_000_000,
                    1.0,
                )
            })
            .collect::<Vec<_>>();
        for (offset, volume) in [(0, 2.0), (1, 3.0)] {
            let mut trade = classifier_trade(
                10 + offset,
                20_000_000 + i64::try_from(offset).expect("small"),
                volume,
            );
            trade.aggressor = aeris_market_data::AggressorSide::Buy;
            trade.price = 200.0 + offset.to_f64().expect("small") * 0.25;
            trades.push(trade);
        }
        let sweeps = classify_trade_sweeps(&trades);
        assert_eq!(
            sweeps.len(),
            1,
            "a run above the 90th percentile print is a sweep"
        );
        assert!((sweeps[0].total_volume - 5.0).abs() < f64::EPSILON);

        // The same run among larger prints no longer stands out.
        for trade in &mut trades[..9] {
            trade.volume = 8.0;
        }
        assert_eq!(classify_trade_sweeps(&trades), [] as [MarketTradeSweep; 0]);
        assert_eq!(classify_trade_sweeps(&[]), [] as [MarketTradeSweep; 0]);
    }

    fn retained_trade(ordinal: u64, nanos: i64, price: i64, quantity: i64) -> RetainedMarketTrade {
        RetainedMarketTrade {
            ingestion_ordinal: ordinal,
            observed_unix_nanos: nanos,
            trading_day: None,
            trade: Arc::new(aeris_market_data::MarketTrade {
                metadata: aeris_market_data::EventMetadata {
                    provider_id: "provider".into(),
                    instrument_id: "instrument".into(),
                    entitlement_id: "entitlement".into(),
                    session_generation: 1,
                    source_sequence: ordinal,
                    timestamps: aeris_market_data::QualifiedTimestamp {
                        exchange_unix_nanos: Some(nanos),
                        provider_unix_nanos: None,
                        received_unix_nanos: nanos,
                    },
                },
                trade_id: format!("trade-{ordinal}"),
                price,
                quantity,
                aggressor: aeris_market_data::AggressorSide::Buy,
            }),
        }
    }

    #[test]
    fn retained_trades_classify_over_the_trailing_window_in_scaled_units() {
        let mut recent = (1_i64..=600)
            .map(|ordinal| {
                retained_trade(
                    u64::try_from(ordinal).expect("positive ordinal"),
                    ordinal * 1_000_000,
                    10_000 + ordinal,
                    100,
                )
            })
            .collect::<VecDeque<_>>();
        let sweeps = classify_recent_trade_sweeps(&recent, 2, 2);
        // 600 one-lot buy prints each one tick apart: the trailing 512 form
        // runs well above the 90th percentile, and the swept ordinals are the
        // retained ones rather than window indexes.
        assert!(!sweeps.is_empty());
        assert!(
            sweeps
                .iter()
                .all(|sweep| sweep.first_ingestion_ordinal >= 89)
        );
        assert_eq!(
            sweeps.last().map(|sweep| sweep.last_ingestion_ordinal),
            Some(600)
        );

        recent.pop_front();
        let sweeps = classify_recent_trade_sweeps(&recent, 2, 2);
        assert!(
            sweeps
                .iter()
                .all(|sweep| sweep.first_ingestion_ordinal >= 88),
            "599 retained prints classify from ordinal 88, not from a window index"
        );

        // A continuing print one window step later extends the same run.
        recent.push_back(retained_trade(601, 601_i64 * 1_000_000, 10_601, 100));
        let sweeps = classify_recent_trade_sweeps(&recent, 2, 2);
        assert_eq!(
            sweeps.last().map(|sweep| sweep.last_ingestion_ordinal),
            Some(601)
        );
    }
}
