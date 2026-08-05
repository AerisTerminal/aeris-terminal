//! Deterministic semantic fixture for the blocked-external Rithmic adapter boundary.

use axiusflow_desktop_provider_runtime::{
    AuthenticationState, InstrumentDescriptor, ProviderEnvironment, ProviderSessionEvent,
    SessionGeneration,
};
use axiusflow_market_data::{
    AggressorSide, BarPeriod, BarSeriesKey, BarUpdate, BookSide, DepthDelta, DepthLevel,
    DepthSnapshot, EventMetadata, MarketBar, MarketEvent, MarketTrade, QualifiedTimestamp,
};

/// Canonical deterministic output used before authorized protocol-kit access exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicFixtureSession {
    pub events: Vec<ProviderSessionEvent>,
    pub bar_updates: Vec<BarUpdate>,
    pub book_recovery_events: Vec<MarketEvent>,
}

/// Produces one bounded read-only Test-system session with trade, depth, and bars.
#[must_use]
pub fn deterministic_session(generation: SessionGeneration) -> RithmicFixtureSession {
    let generation_value = generation.get();
    let series = BarSeriesKey {
        provider_id: "rithmic".to_string(),
        instrument_id: "instrument:rithmic:cme:es".to_string(),
        entitlement_id: "fixture_test_realtime".to_string(),
        period: BarPeriod::Time { seconds: 60 },
        definition_version: 1,
    };
    let events = vec![
        ProviderSessionEvent::DiscoveryStarted,
        ProviderSessionEvent::SystemsDiscovered {
            environments: vec![ProviderEnvironment {
                provider_id: "rithmic".to_string(),
                system_id: "RITHMIC_TEST".to_string(),
                environment: "Test".to_string(),
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
                provider_symbol: "ESM7".to_string(),
                display_symbol: "ES".to_string(),
                venue_id: "CME".to_string(),
                price_scale: 2,
                quantity_scale: 0,
            }],
        },
        ProviderSessionEvent::Market {
            generation,
            event: MarketEvent::Trade(MarketTrade {
                metadata: metadata(1, generation_value),
                trade_id: "fixture-rithmic-trade-1".to_string(),
                price: 525_025,
                quantity: 3,
                aggressor: AggressorSide::Buy,
            }),
        },
        ProviderSessionEvent::Market {
            generation,
            event: MarketEvent::DepthSnapshot(DepthSnapshot {
                metadata: metadata(2, generation_value),
                bids: vec![level(525_000, 8), level(524_975, 6)],
                asks: vec![level(525_025, 7), level(525_050, 5)],
            }),
        },
        ProviderSessionEvent::Market {
            generation,
            event: MarketEvent::DepthDelta(DepthDelta {
                metadata: metadata(3, generation_value),
                side: BookSide::Bid,
                level: level(525_000, 10),
            }),
        },
        ProviderSessionEvent::Heartbeat {
            generation,
            received_unix_nanos: 1_800_000_003_000_000_000,
        },
        ProviderSessionEvent::Invalidated {
            generation: Some(generation),
            reason: axiusflow_desktop_provider_runtime::ProviderInvalidationReason::Transport,
        },
        ProviderSessionEvent::Stopped,
    ];
    let forming = bar_update(series.clone(), generation_value, 4, false);
    let completed = bar_update(series.clone(), generation_value, 5, true);
    let next_forming = next_bar_update(series, generation_value, 6);
    RithmicFixtureSession {
        events,
        bar_updates: vec![forming, completed, next_forming],
        book_recovery_events: book_recovery_events(generation_value),
    }
}

fn book_recovery_events(generation: u64) -> Vec<MarketEvent> {
    vec![
        MarketEvent::DepthSnapshot(DepthSnapshot {
            metadata: metadata(10, generation),
            bids: vec![level(525_000, 8), level(524_975, 6)],
            asks: vec![level(525_025, 7), level(525_050, 5)],
        }),
        MarketEvent::DepthDelta(DepthDelta {
            metadata: metadata(11, generation),
            side: BookSide::Bid,
            level: level(525_000, 10),
        }),
        MarketEvent::DepthDelta(DepthDelta {
            metadata: metadata(13, generation),
            side: BookSide::Ask,
            level: level(525_025, 9),
        }),
        MarketEvent::DepthSnapshot(DepthSnapshot {
            metadata: metadata(12, generation),
            bids: vec![level(525_000, 9), level(524_975, 6)],
            asks: vec![level(525_025, 8), level(525_050, 5)],
        }),
        MarketEvent::DepthSnapshot(DepthSnapshot {
            metadata: metadata(13, generation),
            bids: vec![level(525_000, 11), level(524_975, 7)],
            asks: vec![level(525_025, 9), level(525_050, 6)],
        }),
        MarketEvent::DepthDelta(DepthDelta {
            metadata: metadata(14, generation),
            side: BookSide::Ask,
            level: level(525_025, 12),
        }),
    ]
}

fn metadata(sequence: u64, generation: u64) -> EventMetadata {
    let offset = i64::try_from(sequence).unwrap_or(i64::MAX);
    EventMetadata {
        provider_id: "rithmic".to_string(),
        instrument_id: "instrument:rithmic:cme:es".to_string(),
        entitlement_id: "fixture_test_realtime".to_string(),
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
    let offset = i64::try_from(sequence).unwrap_or(i64::MAX);
    let bar = MarketBar {
        source_sequence: sequence,
        exchange_timestamp_seconds: 1_800_000_000,
        open: 525_000,
        high: 525_050,
        low: 524_975,
        close: if completed { 525_025 } else { 525_000 },
        volume: if completed { 18 } else { 11 },
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
            received_unix_nanos: 1_800_000_002_000_000_000 + offset,
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
        let session = deterministic_session(SessionGeneration::new(NonZeroU64::MIN));
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
