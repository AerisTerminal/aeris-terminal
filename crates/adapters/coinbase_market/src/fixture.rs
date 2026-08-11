use crate::{CoinbaseDecoder, CoinbaseError};
use axiusflow_desktop_provider_runtime::{
    AuthenticationState, InstrumentDescriptor, ProviderEnvironment, ProviderInvalidationReason,
    ProviderSessionEvent, SessionGeneration,
};
use axiusflow_market_data::{
    BarPeriod, BarSeriesKey, BarUpdate, BookSide, DepthDelta, DepthLevel, DepthSnapshot,
    EventMetadata, MarketBar, MarketEvent, QualifiedTimestamp,
};

/// Canonical deterministic Coinbase output consumed by provider-neutral conformance tests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoinbaseFixtureSession {
    pub events: Vec<ProviderSessionEvent>,
    pub bar_updates: Vec<BarUpdate>,
    pub book_recovery_events: Vec<MarketEvent>,
}

/// Decodes one reviewed public trade and adds canonical depth, recovery, and bar evidence.
///
/// # Errors
///
/// Returns an adapter error if the reviewed trade fixture no longer decodes or projects.
pub fn deterministic_fixture_session(
    generation: SessionGeneration,
) -> Result<CoinbaseFixtureSession, CoinbaseError> {
    let generation_value = generation.get();
    let mut decoder = CoinbaseDecoder::new();
    let trades = decoder.decode(
        br#"{"channel":"market_trades","timestamp":"2023-02-09T20:19:35.39625135Z","sequence_num":0,"events":[{"type":"update","trades":[{"trade_id":"fixture-coinbase-1","product_id":"BTC-USD","price":"37000.00","size":"0.50000000","side":"SELL","time":"2023-02-09T20:19:34.265Z"}]}]}"#,
    )?;
    let trade = trades
        .first()
        .ok_or(CoinbaseError::InvalidMessage)?
        .to_market_trade(2, 8, generation_value, 1_800_000_002_000_000_000)?;
    let series = BarSeriesKey {
        provider_id: "coinbase".to_string(),
        instrument_id: "instrument:coinbase:btc:usd".to_string(),
        entitlement_id: "crypto_public_realtime".to_string(),
        period: BarPeriod::Time { seconds: 60 },
        definition_version: 1,
    };
    Ok(CoinbaseFixtureSession {
        events: vec![
            ProviderSessionEvent::DiscoveryStarted,
            ProviderSessionEvent::SystemsDiscovered {
                environments: vec![ProviderEnvironment {
                    provider_id: "coinbase".to_string(),
                    system_id: "advanced_trade_public".to_string(),
                    environment: "production".to_string(),
                }],
            },
            ProviderSessionEvent::AuthenticationChanged {
                generation,
                state: AuthenticationState::Accepted,
            },
            ProviderSessionEvent::InstrumentsDiscovered {
                generation,
                instruments: vec![InstrumentDescriptor {
                    instrument_id: series.instrument_id.clone(),
                    provider_symbol: "BTC-USD".to_string(),
                    display_symbol: "BTC/USD".to_string(),
                    venue_id: "COINBASE".to_string(),
                    price_scale: 2,
                    quantity_scale: 8,
                }],
            },
            ProviderSessionEvent::Market {
                generation,
                event: MarketEvent::Trade(trade),
            },
            ProviderSessionEvent::Market {
                generation,
                event: MarketEvent::DepthSnapshot(DepthSnapshot {
                    metadata: metadata(2, generation_value),
                    bids: vec![level(3_700_000, 80_000_000), level(3_699_900, 60_000_000)],
                    asks: vec![level(3_700_100, 70_000_000), level(3_700_200, 50_000_000)],
                }),
            },
            ProviderSessionEvent::Market {
                generation,
                event: MarketEvent::DepthDelta(DepthDelta {
                    metadata: metadata(3, generation_value),
                    side: BookSide::Bid,
                    level: level(3_700_000, 100_000_000),
                }),
            },
            ProviderSessionEvent::Heartbeat {
                generation,
                received_unix_nanos: 1_800_000_003_000_000_000,
            },
            ProviderSessionEvent::Invalidated {
                generation: Some(generation),
                reason: ProviderInvalidationReason::Transport,
            },
            ProviderSessionEvent::Stopped,
        ],
        bar_updates: vec![
            bar_update(series.clone(), generation_value, 4, false),
            bar_update(series.clone(), generation_value, 5, true),
            next_bar_update(series, generation_value, 6),
        ],
        book_recovery_events: book_recovery_events(generation_value),
    })
}

fn book_recovery_events(generation: u64) -> Vec<MarketEvent> {
    vec![
        MarketEvent::DepthSnapshot(DepthSnapshot {
            metadata: metadata(10, generation),
            bids: vec![level(3_700_000, 80_000_000), level(3_699_900, 60_000_000)],
            asks: vec![level(3_700_100, 70_000_000), level(3_700_200, 50_000_000)],
        }),
        MarketEvent::DepthDelta(DepthDelta {
            metadata: metadata(11, generation),
            side: BookSide::Bid,
            level: level(3_700_000, 100_000_000),
        }),
        MarketEvent::DepthDelta(DepthDelta {
            metadata: metadata(13, generation),
            side: BookSide::Ask,
            level: level(3_700_100, 90_000_000),
        }),
        MarketEvent::DepthSnapshot(DepthSnapshot {
            metadata: metadata(12, generation),
            bids: vec![level(3_700_000, 90_000_000), level(3_699_900, 60_000_000)],
            asks: vec![level(3_700_100, 80_000_000), level(3_700_200, 50_000_000)],
        }),
        MarketEvent::DepthSnapshot(DepthSnapshot {
            metadata: metadata(13, generation),
            bids: vec![level(3_700_000, 110_000_000), level(3_699_900, 70_000_000)],
            asks: vec![level(3_700_100, 90_000_000), level(3_700_200, 60_000_000)],
        }),
        MarketEvent::DepthDelta(DepthDelta {
            metadata: metadata(14, generation),
            side: BookSide::Ask,
            level: level(3_700_100, 120_000_000),
        }),
    ]
}

fn metadata(sequence: u64, generation: u64) -> EventMetadata {
    let offset = i64::try_from(sequence).unwrap_or(i64::MAX);
    EventMetadata {
        provider_id: "coinbase".to_string(),
        instrument_id: "instrument:coinbase:btc:usd".to_string(),
        entitlement_id: "crypto_public_realtime".to_string(),
        source_sequence: sequence,
        session_generation: generation,
        timestamps: QualifiedTimestamp {
            exchange_unix_nanos: Some(1_800_000_000_000_000_000 + offset),
            provider_unix_nanos: Some(1_800_000_001_000_000_000 + offset),
            received_unix_nanos: 1_800_000_002_000_000_000 + offset,
        },
    }
}

const fn level(price: i64, quantity: i64) -> DepthLevel {
    DepthLevel {
        price,
        quantity,
        order_count: Some(1),
    }
}

fn bar_update(series: BarSeriesKey, generation: u64, sequence: u64, completed: bool) -> BarUpdate {
    let bar = MarketBar {
        source_sequence: sequence,
        exchange_timestamp_seconds: 1_800_000_000,
        exchange_timestamp_unix_nanos: 1_800_000_000_000_000_000,
        open: 3_700_000,
        high: 3_701_000,
        low: 3_699_000,
        close: 3_700_500,
        volume: 50_000_000,
    };
    let metadata = EventMetadata {
        provider_id: series.provider_id.clone(),
        instrument_id: series.instrument_id.clone(),
        entitlement_id: series.entitlement_id.clone(),
        source_sequence: sequence,
        session_generation: generation,
        timestamps: QualifiedTimestamp {
            exchange_unix_nanos: Some(1_800_000_000_000_000_000),
            provider_unix_nanos: Some(1_800_000_001_000_000_000),
            received_unix_nanos: 1_800_000_002_000_000_000,
        },
    };
    if completed {
        BarUpdate::Completed {
            series,
            metadata,
            bar,
        }
    } else {
        BarUpdate::Forming {
            series,
            metadata,
            bar,
        }
    }
}

fn next_bar_update(series: BarSeriesKey, generation: u64, sequence: u64) -> BarUpdate {
    let mut update = bar_update(series, generation, sequence, false);
    let BarUpdate::Forming { metadata, bar, .. } = &mut update else {
        unreachable!("requested a forming fixture bar");
    };
    bar.exchange_timestamp_seconds += 60;
    bar.exchange_timestamp_unix_nanos += 60_000_000_000;
    metadata.timestamps.exchange_unix_nanos = Some(1_800_000_060_000_000_000);
    metadata.timestamps.provider_unix_nanos = Some(1_800_000_061_000_000_000);
    metadata.timestamps.received_unix_nanos = 1_800_000_062_000_000_000;
    update
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{num::NonZeroU64, num::NonZeroUsize};

    #[test]
    fn deterministic_fixture_is_bounded_and_canonical() {
        let session = deterministic_fixture_session(SessionGeneration::new(NonZeroU64::MIN))
            .expect("fixture builds");
        for event in &session.events {
            event
                .validate(NonZeroUsize::MIN, NonZeroUsize::MIN, 2)
                .expect("fixture event validates");
        }
        for update in &session.bar_updates {
            update.validate().expect("fixture bar update validates");
        }
        for event in &session.book_recovery_events {
            event.validate(2).expect("fixture recovery event validates");
        }
    }
}
