use crate::ConformanceHarnessError;
use axiusflow_desktop_provider_runtime::{
    AuthenticationState, ProviderSessionEvent, SessionGeneration,
};
use axiusflow_market_data::{BarUpdate, MarketEvent, OrderBook, OrderBookApplyOutcome};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
};

/// Provider-neutral semantic fixture consumed by the shared adapter harness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderAdapterFixture {
    pub events: Vec<ProviderSessionEvent>,
    pub bar_updates: Vec<BarUpdate>,
}

/// Evidence produced by one successful provider adapter conformance run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderAdapterConformance {
    pub market_events: usize,
    pub depth_publications: usize,
    pub forming_bars: usize,
    pub completed_bars: usize,
}

struct SessionEvidence {
    market_events: usize,
    depth_publications: usize,
    generation: SessionGeneration,
    provider_ids: BTreeSet<String>,
    instrument_ids: BTreeSet<String>,
}

#[derive(Clone, Copy)]
struct BarSeriesState {
    source_sequence: u64,
    exchange_timestamp_seconds: i64,
    completed: bool,
}

/// Validates lifecycle ordering, bounded contracts, generation fencing, bars, and depth.
///
/// # Errors
///
/// Returns a conformance failure for malformed or out-of-order semantic output.
pub fn run_provider_adapter_conformance(
    fixture: &ProviderAdapterFixture,
) -> Result<ProviderAdapterConformance, ConformanceHarnessError> {
    let evidence = validate_session_events(&fixture.events)?;
    let (forming_bars, completed_bars) = validate_bar_updates(&fixture.bar_updates, &evidence)?;
    Ok(ProviderAdapterConformance {
        market_events: evidence.market_events,
        depth_publications: evidence.depth_publications,
        forming_bars,
        completed_bars,
    })
}

fn validate_session_events(
    events: &[ProviderSessionEvent],
) -> Result<SessionEvidence, ConformanceHarnessError> {
    let (maximum_systems, maximum_instruments, maximum_depth) = conformance_limits();
    validate_lifecycle_endpoints(events)?;
    let mut systems_discovered = false;
    let mut authenticated = false;
    let mut instruments_discovered = false;
    let mut session_generation = None;
    let mut heartbeat_seen = false;
    let mut market_events = 0_usize;
    let mut depth_publications = 0_usize;
    let mut market_sequences = BTreeMap::new();
    let mut books = BTreeMap::new();
    let mut provider_ids = BTreeSet::new();
    let mut instrument_ids = BTreeSet::new();
    let mut invalidated = false;

    for (index, event) in events.iter().enumerate() {
        if invalidated && !matches!(event, ProviderSessionEvent::Stopped) {
            return Err(driver_error("fixture emitted output after invalidation"));
        }
        event
            .validate(maximum_systems, maximum_instruments, maximum_depth.get())
            .map_err(|error| driver_error(error.to_string()))?;
        let event_generation = session_event_generation(event);
        if let Some(event_generation) = event_generation {
            record_session_generation(&mut session_generation, event_generation)?;
        }
        match event {
            ProviderSessionEvent::SystemsDiscovered { environments }
                if !systems_discovered && !environments.is_empty() =>
            {
                provider_ids.extend(
                    environments
                        .iter()
                        .map(|environment| environment.provider_id.clone()),
                );
                systems_discovered = true;
            }
            ProviderSessionEvent::AuthenticationChanged {
                state: AuthenticationState::Accepted,
                ..
            } if systems_discovered && !authenticated && !instruments_discovered => {
                authenticated = true;
            }
            ProviderSessionEvent::InstrumentsDiscovered { instruments, .. }
                if authenticated && !instruments_discovered && !instruments.is_empty() =>
            {
                instrument_ids.extend(
                    instruments
                        .iter()
                        .map(|instrument| instrument.instrument_id.clone()),
                );
                instruments_discovered = true;
            }
            ProviderSessionEvent::Market { event, .. } if instruments_discovered => {
                let published_depth = validate_market_event(
                    event,
                    &provider_ids,
                    &instrument_ids,
                    &mut market_sequences,
                    &mut books,
                    maximum_depth,
                )?;
                market_events += 1;
                if published_depth {
                    depth_publications += 1;
                }
            }
            ProviderSessionEvent::Heartbeat { .. } if instruments_discovered => {
                heartbeat_seen = true;
            }
            ProviderSessionEvent::DiscoveryStarted if index == 0 => {}
            ProviderSessionEvent::Stopped if index + 1 == events.len() => {}
            ProviderSessionEvent::Invalidated { .. } => invalidated = true,
            ProviderSessionEvent::Market { .. }
            | ProviderSessionEvent::Heartbeat { .. }
            | ProviderSessionEvent::InstrumentsDiscovered { .. }
            | ProviderSessionEvent::AuthenticationChanged { .. }
            | ProviderSessionEvent::DiscoveryStarted
            | ProviderSessionEvent::SystemsDiscovered { .. }
            | ProviderSessionEvent::Stopped => {
                return Err(driver_error("fixture lifecycle event is out of order"));
            }
        }
    }
    if !(systems_discovered
        && authenticated
        && instruments_discovered
        && heartbeat_seen
        && market_events > 0)
    {
        return Err(driver_error("fixture omits required lifecycle evidence"));
    }
    let generation = session_generation
        .ok_or_else(|| driver_error("fixture omits provider session generation evidence"))?;
    Ok(SessionEvidence {
        market_events,
        depth_publications,
        generation,
        provider_ids,
        instrument_ids,
    })
}

fn validate_lifecycle_endpoints(
    events: &[ProviderSessionEvent],
) -> Result<(), ConformanceHarnessError> {
    if matches!(events.first(), Some(ProviderSessionEvent::DiscoveryStarted))
        && matches!(events.last(), Some(ProviderSessionEvent::Stopped))
    {
        Ok(())
    } else {
        Err(driver_error("fixture lifecycle endpoints are incomplete"))
    }
}

fn conformance_limits() -> (NonZeroUsize, NonZeroUsize, NonZeroUsize) {
    (
        NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN),
        NonZeroUsize::new(16).unwrap_or(NonZeroUsize::MIN),
        NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN),
    )
}

fn record_session_generation(
    session_generation: &mut Option<SessionGeneration>,
    event_generation: SessionGeneration,
) -> Result<(), ConformanceHarnessError> {
    if session_generation.is_some_and(|generation| generation != event_generation) {
        return Err(driver_error("fixture crossed provider session generations"));
    }
    *session_generation = Some(event_generation);
    Ok(())
}

fn validate_market_event(
    event: &MarketEvent,
    provider_ids: &BTreeSet<String>,
    instrument_ids: &BTreeSet<String>,
    market_sequences: &mut BTreeMap<(String, String, String), u64>,
    books: &mut BTreeMap<(String, String, String), OrderBook>,
    maximum_depth: NonZeroUsize,
) -> Result<bool, ConformanceHarnessError> {
    if !provider_ids.contains(&event.metadata().provider_id)
        || !instrument_ids.contains(&event.metadata().instrument_id)
    {
        return Err(driver_error(
            "market event identity was not discovered for this adapter",
        ));
    }
    let identity = market_identity(event);
    let sequence = event.metadata().source_sequence;
    let last_sequence = market_sequences.entry(identity.clone()).or_default();
    if sequence <= *last_sequence {
        return Err(driver_error("market source sequence did not advance"));
    }
    *last_sequence = sequence;
    match event {
        MarketEvent::DepthSnapshot(snapshot) => match books
            .entry(identity)
            .or_insert_with(|| OrderBook::new(maximum_depth))
            .install_snapshot(snapshot)
        {
            Ok(OrderBookApplyOutcome::Published(_)) => Ok(true),
            _ => Err(driver_error("depth snapshot did not publish")),
        },
        MarketEvent::DepthDelta(delta) => match books
            .entry(identity)
            .or_insert_with(|| OrderBook::new(maximum_depth))
            .apply_delta(delta)
        {
            Ok(OrderBookApplyOutcome::Published(_)) => Ok(true),
            _ => Err(driver_error("depth delta did not publish")),
        },
        MarketEvent::Trade(_) | MarketEvent::Quote(_) => Ok(false),
    }
}

fn market_identity(event: &MarketEvent) -> (String, String, String) {
    let metadata = event.metadata();
    (
        metadata.provider_id.clone(),
        metadata.instrument_id.clone(),
        metadata.entitlement_id.clone(),
    )
}

fn session_event_generation(event: &ProviderSessionEvent) -> Option<SessionGeneration> {
    match event {
        ProviderSessionEvent::AuthenticationChanged { generation, .. }
        | ProviderSessionEvent::InstrumentsDiscovered { generation, .. }
        | ProviderSessionEvent::Market { generation, .. }
        | ProviderSessionEvent::Heartbeat { generation, .. } => Some(*generation),
        ProviderSessionEvent::Invalidated { generation, .. } => *generation,
        ProviderSessionEvent::DiscoveryStarted
        | ProviderSessionEvent::SystemsDiscovered { .. }
        | ProviderSessionEvent::Stopped => None,
    }
}

fn validate_bar_updates(
    updates: &[BarUpdate],
    evidence: &SessionEvidence,
) -> Result<(usize, usize), ConformanceHarnessError> {
    let mut forming_bars = 0_usize;
    let mut completed_bars = 0_usize;
    let mut series_states = BTreeMap::new();
    for update in updates {
        update
            .validate()
            .map_err(|error| driver_error(error.to_string()))?;
        if update.metadata().session_generation != evidence.generation.get() {
            return Err(driver_error(
                "bar update crossed the provider session generation",
            ));
        }
        if !evidence.provider_ids.contains(&update.series().provider_id)
            || !evidence
                .instrument_ids
                .contains(&update.series().instrument_id)
        {
            return Err(driver_error(
                "bar update identity was not discovered for this adapter",
            ));
        }
        validate_bar_progression(update, &mut series_states)?;
        match update {
            BarUpdate::Forming { .. } => forming_bars += 1,
            BarUpdate::Completed { .. } => completed_bars += 1,
        }
    }
    if forming_bars == 0 || completed_bars == 0 {
        return Err(driver_error(
            "fixture omits forming or completed bar evidence",
        ));
    }
    Ok((forming_bars, completed_bars))
}

fn validate_bar_progression(
    update: &BarUpdate,
    series_states: &mut BTreeMap<axiusflow_market_data::BarSeriesKey, BarSeriesState>,
) -> Result<(), ConformanceHarnessError> {
    let sequence = update.metadata().source_sequence;
    let timestamp = update.bar().exchange_timestamp_seconds;
    let completed = matches!(update, BarUpdate::Completed { .. });
    let Some(state) = series_states.get_mut(update.series()) else {
        if completed {
            return Err(driver_error("bar series completed before forming"));
        }
        series_states.insert(
            update.series().clone(),
            BarSeriesState {
                source_sequence: sequence,
                exchange_timestamp_seconds: timestamp,
                completed: false,
            },
        );
        return Ok(());
    };
    if sequence <= state.source_sequence {
        return Err(driver_error("bar source sequence did not advance"));
    }
    let valid_transition = match (state.completed, completed) {
        (false, false | true) => timestamp == state.exchange_timestamp_seconds,
        (true, false) => timestamp > state.exchange_timestamp_seconds,
        (true, true) => false,
    };
    if !valid_transition {
        return Err(driver_error("bar forming/completed progression is invalid"));
    }
    state.source_sequence = sequence;
    state.exchange_timestamp_seconds = timestamp;
    state.completed = completed;
    Ok(())
}

fn driver_error(message: impl Into<String>) -> ConformanceHarnessError {
    ConformanceHarnessError::Driver(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_coinbase_market_adapter::CoinbaseDecoder;
    use axiusflow_desktop_provider_runtime::{
        InstrumentDescriptor, ProviderEnvironment, ProviderInvalidationReason, SessionGeneration,
    };
    use axiusflow_market_data::{
        BarPeriod, BarSeriesKey, EventMetadata, MarketBar, QualifiedTimestamp,
    };
    use std::num::NonZeroU64;

    #[test]
    fn coinbase_and_rithmic_pass_the_shared_semantic_harness() {
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let coinbase = coinbase_fixture(generation);
        let coinbase_report =
            run_provider_adapter_conformance(&coinbase).expect("Coinbase fixture conforms");
        assert_eq!(coinbase_report.market_events, 1);
        assert_eq!(coinbase_report.depth_publications, 0);

        let rithmic = axiusflow_rithmic_fixture_adapter::deterministic_session(generation);
        let rithmic_report = run_provider_adapter_conformance(&ProviderAdapterFixture {
            events: rithmic.events,
            bar_updates: rithmic.bar_updates,
        })
        .expect("Rithmic fixture conforms");
        assert_eq!(rithmic_report.market_events, 3);
        assert_eq!(rithmic_report.depth_publications, 2);
        assert_eq!(rithmic_report.forming_bars, coinbase_report.forming_bars);
        assert_eq!(
            rithmic_report.completed_bars,
            coinbase_report.completed_bars
        );
    }

    #[test]
    fn shared_harness_rejects_cross_generation_lifecycle_output() {
        let first_generation = SessionGeneration::new(NonZeroU64::MIN);
        let second_generation = SessionGeneration::new(NonZeroU64::new(2).expect("nonzero"));
        let mut fixture = coinbase_fixture(first_generation);
        let ProviderSessionEvent::InstrumentsDiscovered { generation, .. } = &mut fixture.events[3]
        else {
            panic!("fixture instrument event is stable");
        };
        *generation = second_generation;

        assert!(run_provider_adapter_conformance(&fixture).is_err());
    }

    #[test]
    fn shared_harness_rejects_events_after_stop_and_empty_discovery() {
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut stopped_early = coinbase_fixture(generation);
        stopped_early
            .events
            .insert(4, ProviderSessionEvent::Stopped);
        assert!(run_provider_adapter_conformance(&stopped_early).is_err());

        let mut empty_systems = coinbase_fixture(generation);
        let ProviderSessionEvent::SystemsDiscovered { environments } = &mut empty_systems.events[1]
        else {
            panic!("fixture systems event is stable");
        };
        environments.clear();
        assert!(run_provider_adapter_conformance(&empty_systems).is_err());

        let mut empty_instruments = coinbase_fixture(generation);
        let ProviderSessionEvent::InstrumentsDiscovered { instruments, .. } =
            &mut empty_instruments.events[3]
        else {
            panic!("fixture instrument event is stable");
        };
        instruments.clear();
        assert!(run_provider_adapter_conformance(&empty_instruments).is_err());
    }

    #[test]
    fn shared_harness_rejects_bars_for_an_undiscovered_adapter_identity() {
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut fixture = coinbase_fixture(generation);
        for update in &mut fixture.bar_updates {
            let (series, metadata) = match update {
                BarUpdate::Forming {
                    series, metadata, ..
                }
                | BarUpdate::Completed {
                    series, metadata, ..
                } => (series, metadata),
            };
            series.provider_id = "rithmic".to_string();
            series.instrument_id = "instrument:rithmic:cme:es".to_string();
            metadata.provider_id = series.provider_id.clone();
            metadata.instrument_id = series.instrument_id.clone();
        }

        assert!(run_provider_adapter_conformance(&fixture).is_err());
    }

    #[test]
    fn shared_harness_rejects_output_after_invalidation() {
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut fixture = coinbase_fixture(generation);
        fixture.events.insert(
            5,
            ProviderSessionEvent::Invalidated {
                generation: Some(generation),
                reason: ProviderInvalidationReason::Transport,
            },
        );

        assert!(run_provider_adapter_conformance(&fixture).is_err());
    }

    #[test]
    fn shared_harness_rejects_repeated_lifecycle_phases() {
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut fixture = coinbase_fixture(generation);
        fixture.events.insert(
            5,
            ProviderSessionEvent::AuthenticationChanged {
                generation,
                state: AuthenticationState::Accepted,
            },
        );

        assert!(run_provider_adapter_conformance(&fixture).is_err());
    }

    #[test]
    fn shared_harness_tracks_depth_books_per_instrument() {
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut session = axiusflow_rithmic_fixture_adapter::deterministic_session(generation);
        let ProviderSessionEvent::InstrumentsDiscovered { instruments, .. } =
            &mut session.events[3]
        else {
            panic!("fixture instrument event is stable");
        };
        instruments.push(InstrumentDescriptor {
            instrument_id: "instrument:rithmic:cme:nq".to_string(),
            provider_symbol: "NQM7".to_string(),
            display_symbol: "NQ".to_string(),
            venue_id: "CME".to_string(),
            price_scale: 2,
            quantity_scale: 0,
        });
        let mut second_snapshot = session.events[5].clone();
        let ProviderSessionEvent::Market {
            event: MarketEvent::DepthSnapshot(snapshot),
            ..
        } = &mut second_snapshot
        else {
            panic!("fixture depth snapshot event is stable");
        };
        snapshot.metadata.instrument_id = "instrument:rithmic:cme:nq".to_string();
        snapshot.metadata.source_sequence = 1;
        session.events.insert(7, second_snapshot);

        let report = run_provider_adapter_conformance(&ProviderAdapterFixture {
            events: session.events,
            bar_updates: session.bar_updates,
        })
        .expect("independent instrument books conform");
        assert_eq!(report.market_events, 4);
        assert_eq!(report.depth_publications, 3);
    }

    #[test]
    fn shared_harness_rejects_invalid_bar_sequence_and_progression() {
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut duplicate = coinbase_fixture(generation);
        set_bar_sequence(&mut duplicate.bar_updates[1], 2);
        assert!(run_provider_adapter_conformance(&duplicate).is_err());

        let mut reversed = coinbase_fixture(generation);
        reversed.bar_updates.swap(0, 1);
        set_bar_sequence(&mut reversed.bar_updates[0], 2);
        set_bar_sequence(&mut reversed.bar_updates[1], 3);
        assert!(run_provider_adapter_conformance(&reversed).is_err());
    }

    fn set_bar_sequence(update: &mut BarUpdate, sequence: u64) {
        match update {
            BarUpdate::Forming { metadata, bar, .. }
            | BarUpdate::Completed { metadata, bar, .. } => {
                metadata.source_sequence = sequence;
                bar.source_sequence = sequence;
            }
        }
    }

    fn coinbase_fixture(generation: SessionGeneration) -> ProviderAdapterFixture {
        let mut decoder = CoinbaseDecoder::new();
        let trades = decoder
            .decode(
                br#"{"channel":"market_trades","timestamp":"2023-02-09T20:19:35.39625135Z","sequence_num":0,"events":[{"type":"update","trades":[{"trade_id":"fixture-coinbase-1","product_id":"BTC-USD","price":"37000.00","size":"0.50000000","side":"SELL","time":"2023-02-09T20:19:34.265Z"}]}]}"#,
            )
            .expect("fixture decodes");
        let trade = trades[0]
            .to_market_trade(2, 8, generation.get(), 1_800_000_002_000_000_000)
            .expect("fixture projects");
        let series = BarSeriesKey {
            provider_id: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            entitlement_id: "crypto_public_realtime".to_string(),
            period: BarPeriod::Time { seconds: 60 },
            definition_version: 1,
        };
        ProviderAdapterFixture {
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
                ProviderSessionEvent::Heartbeat {
                    generation,
                    received_unix_nanos: 1_800_000_003_000_000_000,
                },
                ProviderSessionEvent::Stopped,
            ],
            bar_updates: vec![
                bar_update(series.clone(), generation.get(), 2, false),
                bar_update(series, generation.get(), 3, true),
            ],
        }
    }

    fn bar_update(
        series: BarSeriesKey,
        generation: u64,
        sequence: u64,
        completed: bool,
    ) -> BarUpdate {
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
        let bar = MarketBar {
            source_sequence: sequence,
            exchange_timestamp_seconds: 1_800_000_000,
            open: 3_700_000,
            high: 3_701_000,
            low: 3_699_000,
            close: 3_700_500,
            volume: 50_000_000,
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
}
