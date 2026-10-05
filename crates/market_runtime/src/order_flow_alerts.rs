use aeris_market_data::{BarPeriod, MarketBar};

use crate::{CanonicalMarketSeriesSnapshot, RetainedMarketTrade};

/// Direction of a completed-bar price/CVD disagreement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeltaDivergenceDirection {
    Bullish,
    Bearish,
}

/// Deterministic evidence produced by the runtime-owned delta-divergence rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeltaDivergenceEvidence {
    pub direction: DeltaDivergenceDirection,
    pub previous_bar_source_sequence: u64,
    pub completed_bar_source_sequence: u64,
    pub previous_close: i64,
    pub completed_close: i64,
    pub previous_cumulative_delta: i128,
    pub completed_cumulative_delta: i128,
    pub observed_unix_nanos: i64,
}

/// Source sequence of the completed bar the divergence rule would evaluate, so a
/// publisher can skip the tape scan for a bar whose trigger already fired.
pub(crate) fn delta_divergence_completed_bar_sequence(
    series: &CanonicalMarketSeriesSnapshot,
) -> Option<u64> {
    divergence_bars(series).map(|(_, _, completed)| completed.source_sequence)
}

fn divergence_bars(series: &CanonicalMarketSeriesSnapshot) -> Option<(u32, MarketBar, MarketBar)> {
    let BarPeriod::Time { seconds } = series.series.period else {
        return None;
    };
    let completed_len = series
        .bars
        .len()
        .saturating_sub(usize::from(series.forming));
    let previous = *series.bars.get(completed_len.checked_sub(2)?)?;
    let completed = *series.bars.get(completed_len.checked_sub(1)?)?;
    Some((seconds, previous, completed))
}

/// Evaluates the latest two completed fixed-time bars against the canonical
/// classified tape. `None` means either no divergence or insufficient tape
/// coverage; the rule never fills a missing interval with invented flow.
#[must_use]
pub fn detect_delta_divergence(
    series: &CanonicalMarketSeriesSnapshot,
    trades: &[RetainedMarketTrade],
) -> Option<DeltaDivergenceEvidence> {
    let (seconds, previous, completed) = divergence_bars(series)?;
    let interval_nanos = i64::from(seconds).checked_mul(1_000_000_000)?;
    let window_start = previous.exchange_timestamp_unix_nanos;
    let previous_end = previous
        .exchange_timestamp_unix_nanos
        .checked_add(interval_nanos)?;
    let completed_end = completed
        .exchange_timestamp_unix_nanos
        .checked_add(interval_nanos)?;

    let first_timestamp = trade_timestamp_nanos(trades.first()?.trade.as_ref());
    let last_timestamp = trade_timestamp_nanos(trades.last()?.trade.as_ref());
    if first_timestamp > window_start || last_timestamp < completed_end {
        return None;
    }

    let mut previous_delta = 0_i128;
    let mut completed_delta = 0_i128;
    for retained in trades {
        let trade = retained.trade.as_ref();
        let timestamp = trade_timestamp_nanos(trade);
        if timestamp < window_start || timestamp >= completed_end {
            continue;
        }
        let signed = match trade.aggressor {
            aeris_market_data::AggressorSide::Buy => i128::from(trade.quantity),
            aeris_market_data::AggressorSide::Sell => -i128::from(trade.quantity),
            aeris_market_data::AggressorSide::Unknown => 0,
        };
        completed_delta = completed_delta.checked_add(signed)?;
        if timestamp < previous_end {
            previous_delta = previous_delta.checked_add(signed)?;
        }
    }

    let direction = if completed.close > previous.close && completed_delta < previous_delta {
        DeltaDivergenceDirection::Bearish
    } else if completed.close < previous.close && completed_delta > previous_delta {
        DeltaDivergenceDirection::Bullish
    } else {
        return None;
    };
    Some(DeltaDivergenceEvidence {
        direction,
        previous_bar_source_sequence: previous.source_sequence,
        completed_bar_source_sequence: completed.source_sequence,
        previous_close: previous.close,
        completed_close: completed.close,
        previous_cumulative_delta: previous_delta,
        completed_cumulative_delta: completed_delta,
        observed_unix_nanos: trades.last()?.observed_unix_nanos,
    })
}

fn trade_timestamp_nanos(trade: &aeris_market_data::MarketTrade) -> i64 {
    trade
        .metadata
        .timestamps
        .exchange_unix_nanos
        .or(trade.metadata.timestamps.provider_unix_nanos)
        .unwrap_or(trade.metadata.timestamps.received_unix_nanos)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aeris_market_data::{
        AggressorSide, BarSeriesKey, EventMetadata, MarketBar, MarketTrade, QualifiedTimestamp,
    };
    use aeris_market_engine::{ProviderGeneration, SeriesSnapshot};

    use super::*;

    fn bar(sequence: u64, start_seconds: i64, close: i64) -> MarketBar {
        MarketBar {
            source_sequence: sequence,
            exchange_timestamp_seconds: start_seconds,
            exchange_timestamp_unix_nanos: start_seconds * 1_000_000_000,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1,
        }
    }

    fn trade(sequence: u64, timestamp_seconds: i64, side: AggressorSide) -> RetainedMarketTrade {
        RetainedMarketTrade {
            ingestion_ordinal: sequence,
            observed_unix_nanos: timestamp_seconds * 1_000_000_000,
            trade: Arc::new(MarketTrade {
                metadata: EventMetadata {
                    provider_id: "hyperliquid".to_string(),
                    instrument_id: "hyperliquid:perp:BTC".to_string(),
                    entitlement_id: "hyperliquid-public".to_string(),
                    session_generation: 1,
                    source_sequence: sequence,
                    timestamps: QualifiedTimestamp {
                        exchange_unix_nanos: Some(timestamp_seconds * 1_000_000_000),
                        provider_unix_nanos: None,
                        received_unix_nanos: timestamp_seconds * 1_000_000_000,
                    },
                },
                trade_id: format!("trade-{sequence}"),
                price: 100,
                quantity: 10,
                aggressor: side,
            }),
        }
    }

    fn series(previous_close: i64, completed_close: i64) -> SeriesSnapshot {
        SeriesSnapshot {
            series: BarSeriesKey {
                provider_id: "hyperliquid".to_string(),
                instrument_id: "hyperliquid:perp:BTC".to_string(),
                entitlement_id: "hyperliquid-public".to_string(),
                period: BarPeriod::Time { seconds: 60 },
                definition_version: 1,
            },
            provider_generation: ProviderGeneration(
                std::num::NonZeroU64::new(1).expect("generation"),
            ),
            publication_generation: 1,
            price_scale: 2,
            quantity_scale: 2,
            forming: true,
            bars: vec![
                bar(1, 0, previous_close),
                bar(2, 60, completed_close),
                bar(3, 120, completed_close),
            ]
            .into(),
        }
    }

    #[test]
    fn completed_price_rise_with_falling_cvd_is_bearish() {
        let trades = vec![
            trade(1, 0, AggressorSide::Buy),
            trade(2, 30, AggressorSide::Buy),
            trade(3, 60, AggressorSide::Sell),
            trade(4, 90, AggressorSide::Sell),
            trade(5, 120, AggressorSide::Buy),
        ];
        let evidence = detect_delta_divergence(&series(100, 101), &trades).expect("divergence");
        assert_eq!(evidence.direction, DeltaDivergenceDirection::Bearish);
        assert_eq!(evidence.previous_cumulative_delta, 20);
        assert_eq!(evidence.completed_cumulative_delta, 0);
    }

    #[test]
    fn incomplete_tape_never_invents_divergence() {
        let trades = vec![
            trade(1, 30, AggressorSide::Buy),
            trade(2, 60, AggressorSide::Sell),
            trade(3, 120, AggressorSide::Buy),
        ];
        assert!(detect_delta_divergence(&series(100, 101), &trades).is_none());
    }

    #[test]
    fn watermark_precheck_names_the_bar_the_rule_evaluates() {
        let trades = vec![
            trade(1, 0, AggressorSide::Buy),
            trade(2, 30, AggressorSide::Buy),
            trade(3, 60, AggressorSide::Sell),
            trade(4, 90, AggressorSide::Sell),
            trade(5, 120, AggressorSide::Buy),
        ];
        let forming = series(100, 101);
        let evidence = detect_delta_divergence(&forming, &trades).expect("divergence");
        assert_eq!(
            delta_divergence_completed_bar_sequence(&forming),
            Some(evidence.completed_bar_source_sequence)
        );
        let closed = SeriesSnapshot {
            forming: false,
            ..forming
        };
        assert_eq!(delta_divergence_completed_bar_sequence(&closed), Some(3));
    }
}
