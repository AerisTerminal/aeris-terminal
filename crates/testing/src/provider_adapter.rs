use crate::ConformanceHarnessError;
use axiusflow_application::{
    MarketBarClientModel, MarketBarModelOutcome, MarketStreamPublication, Provenanced,
    ReplayProvenance, ReplaySnapshot, ReplayStreamUpdate, ResnapshotReason, StreamDelta,
};
use axiusflow_desktop_provider_runtime::{
    AuthenticationState, InstrumentDescriptor, ProviderEnvironment, ProviderFeedDiagnostics,
    ProviderSessionEvent, SessionGeneration,
};
use axiusflow_instruments::InstrumentRevision;
use axiusflow_market_data::{
    BarDefinition, BarUpdate, MarketEvent, OrderBook, OrderBookApplyOutcome,
    OrderBookRecoveryReason, OrderBookState,
};
use axiusflow_protocols::MarketEventProvenance;
use std::{
    collections::{BTreeMap, BTreeSet},
    num::{NonZeroU64, NonZeroUsize},
};

/// Provider-neutral semantic fixture consumed by the shared adapter harness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderAdapterFixture {
    pub events: Vec<ProviderSessionEvent>,
    pub selected_environment: ProviderEnvironment,
    pub bar_updates: Vec<BarUpdate>,
    pub book_recovery_events: Vec<MarketEvent>,
    pub publication_instrument: InstrumentRevision,
    pub bar_definition: BarDefinition,
}

/// Evidence produced by one successful provider adapter conformance run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderAdapterConformance {
    pub market_events: usize,
    pub depth_publications: usize,
    pub forming_bars: usize,
    pub completed_bars: usize,
    pub book_recoveries: usize,
    pub covering_snapshots: usize,
    pub stream_publications: usize,
    pub stream_recoveries: usize,
}

struct SessionEvidence {
    market_events: usize,
    depth_publications: usize,
    generation: SessionGeneration,
    provider_ids: BTreeSet<String>,
    instruments: InstrumentEvidence,
}

#[derive(Default)]
struct InstrumentEvidence {
    ids: BTreeSet<String>,
    descriptors: BTreeMap<String, InstrumentDescriptor>,
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
    validate_session_diagnostics(&fixture.events, &fixture.selected_environment, &evidence)?;
    let (forming_bars, completed_bars) = validate_bar_updates(&fixture.bar_updates, &evidence)?;
    let (book_recoveries, covering_snapshots) =
        validate_book_recovery(&fixture.book_recovery_events, &evidence)?;
    let (stream_publications, stream_recoveries) = validate_stream_publications(
        &fixture.bar_updates,
        &fixture.publication_instrument,
        &fixture.bar_definition,
        &evidence,
    )?;
    Ok(ProviderAdapterConformance {
        market_events: evidence.market_events,
        depth_publications: evidence.depth_publications,
        forming_bars,
        completed_bars,
        book_recoveries,
        covering_snapshots,
        stream_publications,
        stream_recoveries,
    })
}

fn validate_session_diagnostics(
    events: &[ProviderSessionEvent],
    selected_environment: &ProviderEnvironment,
    evidence: &SessionEvidence,
) -> Result<(), ConformanceHarnessError> {
    let selected_was_discovered = events.iter().any(|event| match event {
        ProviderSessionEvent::SystemsDiscovered { environments } => {
            environments.contains(selected_environment)
        }
        _ => false,
    });
    if !selected_was_discovered {
        return Err(driver_error(
            "fixture selected an undiscovered diagnostics environment",
        ));
    }
    let mut diagnostics = ProviderFeedDiagnostics::try_new(
        selected_environment.provider_id.clone(),
        selected_environment.system_id.clone(),
        selected_environment.environment.clone(),
        None,
    )
    .map_err(|error| driver_error(error.to_string()))?;
    let mut monotonic_nanos = 0_u64;
    for event in events {
        monotonic_nanos = monotonic_nanos.saturating_add(100_000_000);
        diagnostics
            .observe_event(event, monotonic_nanos)
            .map_err(|error| driver_error(error.to_string()))?;
        if matches!(event, ProviderSessionEvent::Market { .. }) {
            diagnostics
                .record_publication(evidence.generation, monotonic_nanos)
                .map_err(|error| driver_error(error.to_string()))?;
        }
    }
    let snapshot = diagnostics
        .try_snapshot(
            monotonic_nanos.saturating_add(250_000_000),
            1_900_000_000_000_000_000,
        )
        .map_err(|error| driver_error(error.to_string()))?
        .ok_or_else(|| driver_error("fixture diagnostics snapshot was suppressed"))?;
    let market_events = snapshot
        .counters
        .trades
        .saturating_add(snapshot.counters.quotes)
        .saturating_add(snapshot.counters.depth_snapshots)
        .saturating_add(snapshot.counters.depth_deltas);
    let depth_events = snapshot
        .counters
        .depth_snapshots
        .saturating_add(snapshot.counters.depth_deltas);
    if snapshot.identity.provider() != selected_environment.provider_id
        || snapshot.identity.system() != selected_environment.system_id
        || snapshot.identity.environment() != selected_environment.environment
        || snapshot.session_generation.map(NonZeroU64::get) != Some(evidence.generation.get())
        || market_events != u64::try_from(evidence.market_events).unwrap_or(u64::MAX)
        || depth_events != u64::try_from(evidence.depth_publications).unwrap_or(u64::MAX)
        || snapshot.counters.publications
            != u64::try_from(evidence.market_events).unwrap_or(u64::MAX)
        || snapshot.heartbeat_age_nanos.is_none()
        || snapshot.last_message_age_nanos.is_none()
    {
        return Err(driver_error(
            "fixture diagnostics diverged from provider session evidence",
        ));
    }
    Ok(())
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
    let mut instrument_evidence = InstrumentEvidence::default();
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
                record_provider_ids(&mut provider_ids, environments);
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
                record_instruments(&mut instrument_evidence, instruments)?;
                instruments_discovered = true;
            }
            ProviderSessionEvent::Market { event, .. } if instruments_discovered => {
                let published_depth = validate_market_event(
                    event,
                    &provider_ids,
                    &instrument_evidence.ids,
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
            ProviderSessionEvent::Invalidated { generation, .. } => {
                invalidated = terminal_invalidation_matches(*generation, session_generation)?;
            }
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
    validate_required_session_evidence(
        market_events,
        [
            systems_discovered,
            authenticated,
            instruments_discovered,
            heartbeat_seen,
            invalidated,
        ],
    )?;
    let generation = session_generation
        .ok_or_else(|| driver_error("fixture omits provider session generation evidence"))?;
    Ok(SessionEvidence {
        market_events,
        depth_publications,
        generation,
        provider_ids,
        instruments: instrument_evidence,
    })
}

fn validate_required_session_evidence(
    market_events: usize,
    phases: [bool; 5],
) -> Result<(), ConformanceHarnessError> {
    if market_events == 0 || phases.contains(&false) {
        return Err(driver_error("fixture omits required lifecycle evidence"));
    }
    Ok(())
}

fn record_provider_ids(provider_ids: &mut BTreeSet<String>, environments: &[ProviderEnvironment]) {
    provider_ids.extend(
        environments
            .iter()
            .map(|environment| environment.provider_id.clone()),
    );
}

fn record_instruments(
    evidence: &mut InstrumentEvidence,
    instruments: &[InstrumentDescriptor],
) -> Result<(), ConformanceHarnessError> {
    for instrument in instruments {
        if !evidence.ids.insert(instrument.instrument_id.clone())
            || evidence
                .descriptors
                .insert(instrument.instrument_id.clone(), instrument.clone())
                .is_some()
        {
            return Err(driver_error("fixture discovered a duplicate instrument"));
        }
    }
    Ok(())
}

fn terminal_invalidation_matches(
    invalidated: Option<SessionGeneration>,
    active: Option<SessionGeneration>,
) -> Result<bool, ConformanceHarnessError> {
    if invalidated.is_some() && invalidated == active {
        Ok(true)
    } else {
        Err(driver_error(
            "terminal invalidation did not fence the active generation",
        ))
    }
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
                .instruments
                .ids
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

fn validate_book_recovery(
    events: &[MarketEvent],
    evidence: &SessionEvidence,
) -> Result<(usize, usize), ConformanceHarnessError> {
    if events.len() != 6 {
        return Err(driver_error(
            "book recovery corpus must contain six canonical transitions",
        ));
    }
    let maximum_depth = conformance_limits().2;
    let mut book = OrderBook::new(maximum_depth);
    let mut immutable_baseline = None;
    for (index, event) in events.iter().enumerate() {
        if !evidence
            .provider_ids
            .contains(&event.metadata().provider_id)
            || !evidence
                .instruments
                .ids
                .contains(&event.metadata().instrument_id)
            || event.metadata().session_generation != evidence.generation.get()
        {
            return Err(driver_error(
                "book recovery event identity or generation was not discovered",
            ));
        }
        event
            .validate(maximum_depth.get())
            .map_err(|error| driver_error(error.to_string()))?;
        match (index, event) {
            (0, MarketEvent::DepthSnapshot(snapshot)) => {
                let OrderBookApplyOutcome::Published(publication) = book
                    .install_snapshot(snapshot)
                    .map_err(|error| driver_error(error.to_string()))?
                else {
                    return Err(driver_error("book recovery baseline did not publish"));
                };
                immutable_baseline = Some(publication);
            }
            (1, MarketEvent::DepthDelta(delta)) => {
                if !matches!(
                    book.apply_delta(delta)
                        .map_err(|error| driver_error(error.to_string()))?,
                    OrderBookApplyOutcome::Published(_)
                ) {
                    return Err(driver_error("ordered pre-gap delta did not publish"));
                }
            }
            (2, MarketEvent::DepthDelta(delta)) => {
                let Err(error) = book.apply_delta(delta) else {
                    return Err(driver_error("depth gap was accepted"));
                };
                if !matches!(
                    error,
                    axiusflow_market_data::MarketDataValidationError::DepthGap { .. }
                ) || book.state()
                    != OrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap)
                    || !book.publication().bids.is_empty()
                    || !book.publication().asks.is_empty()
                {
                    return Err(driver_error("depth gap did not fail closed"));
                }
            }
            (3, MarketEvent::DepthSnapshot(snapshot)) => {
                if book
                    .install_snapshot(snapshot)
                    .map_err(|error| driver_error(error.to_string()))?
                    != OrderBookApplyOutcome::IgnoredStale
                {
                    return Err(driver_error("non-covering recovery snapshot was accepted"));
                }
            }
            (4, MarketEvent::DepthSnapshot(snapshot)) => {
                if !matches!(
                    book.install_snapshot(snapshot)
                        .map_err(|error| driver_error(error.to_string()))?,
                    OrderBookApplyOutcome::Published(_)
                ) || book.state() != OrderBookState::Ready
                {
                    return Err(driver_error("covering recovery snapshot did not publish"));
                }
            }
            (5, MarketEvent::DepthDelta(delta)) => {
                if !matches!(
                    book.apply_delta(delta)
                        .map_err(|error| driver_error(error.to_string()))?,
                    OrderBookApplyOutcome::Published(_)
                ) || book.publication().source_watermark != delta.metadata.source_sequence
                {
                    return Err(driver_error("post-recovery delta did not publish"));
                }
            }
            _ => return Err(driver_error("book recovery corpus shape is invalid")),
        }
    }
    validate_immutable_book_baseline(immutable_baseline, events, &book)?;
    Ok((1, 1))
}

fn validate_immutable_book_baseline(
    baseline: Option<axiusflow_market_data::OrderBookPublication>,
    events: &[MarketEvent],
    book: &OrderBook,
) -> Result<(), ConformanceHarnessError> {
    let baseline = baseline
        .ok_or_else(|| driver_error("book recovery corpus omitted its immutable baseline"))?;
    if baseline.source_watermark == events[0].metadata().source_sequence
        && baseline.state == OrderBookState::Ready
        && baseline != book.publication()
    {
        Ok(())
    } else {
        Err(driver_error(
            "book publication was mutated or recovery did not replace it",
        ))
    }
}

fn validate_stream_publications(
    updates: &[BarUpdate],
    instrument: &InstrumentRevision,
    bar_definition: &BarDefinition,
    evidence: &SessionEvidence,
) -> Result<(usize, usize), ConformanceHarnessError> {
    let completed_index = updates
        .iter()
        .position(|update| matches!(update, BarUpdate::Completed { .. }))
        .ok_or_else(|| driver_error("publication corpus omits a completed baseline bar"))?;
    let baseline = &updates[completed_index];
    let next = updates
        .iter()
        .skip(completed_index + 1)
        .find(|update| {
            matches!(update, BarUpdate::Forming { .. })
                && update.series() == baseline.series()
                && update.bar().exchange_timestamp_seconds
                    > baseline.bar().exchange_timestamp_seconds
        })
        .ok_or_else(|| driver_error("publication corpus omits the next forming bar"))?;
    let descriptor = evidence
        .instruments
        .descriptors
        .get(&baseline.series().instrument_id)
        .ok_or_else(|| driver_error("publication instrument was not discovered"))?;
    validate_publication_identity(baseline, instrument, descriptor, bar_definition)?;

    let baseline_item = provenanced_bar(baseline, evidence.generation)?;
    let next_item = provenanced_bar(next, evidence.generation)?;
    let snapshot = ReplaySnapshot::try_from_provenanced_values(
        instrument.clone(),
        ReplayProvenance::EmbeddedFixture,
        bar_definition.clone(),
        1,
        vec![baseline_item],
    )
    .map_err(|error| driver_error(error.to_string()))?;
    let capacity = NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN);
    let mut model = MarketBarClientModel::new(capacity);
    let snapshot_publication = publish_update(
        &mut model,
        ReplayStreamUpdate::Snapshot(snapshot),
        baseline.series().provider_id.as_str(),
    )?;
    let delta = StreamDelta::try_new(
        baseline.bar().source_sequence,
        next.bar().source_sequence,
        next_item.clone(),
    )
    .map_err(|error| driver_error(error.to_string()))?;
    let delta_publication = publish_update(
        &mut model,
        ReplayStreamUpdate::Delta(delta),
        baseline.series().provider_id.as_str(),
    )?;
    let frozen_generation = delta_publication.generation().clone();

    let gap_item = gap_item_after(next_item)?;
    let gap_sequence = gap_item.value().source_sequence;
    let gap_previous = gap_sequence
        .checked_sub(1)
        .ok_or_else(|| driver_error("publication gap sequence underflowed"))?;
    let gap_delta = StreamDelta::try_new(gap_previous, gap_sequence, gap_item.clone())
        .map_err(|error| driver_error(error.to_string()))?;
    let gap_outcome = model
        .apply_update(ReplayStreamUpdate::Delta(gap_delta))
        .map_err(|error| driver_error(error.to_string()))?;
    if gap_outcome != MarketBarModelOutcome::ResnapshotRequired(ResnapshotReason::SequenceGap)
        || model.current_generation() != Some(&frozen_generation)
        || !model.requires_snapshot()
    {
        return Err(driver_error(
            "application publication model did not freeze on a sequence gap",
        ));
    }

    let recovery = ReplaySnapshot::try_from_provenanced_values(
        instrument.clone(),
        ReplayProvenance::EmbeddedFixture,
        bar_definition.clone(),
        frozen_generation.generation().saturating_add(1),
        vec![gap_item],
    )
    .map_err(|error| driver_error(error.to_string()))?;
    let recovery_publication = publish_update(
        &mut model,
        ReplayStreamUpdate::Snapshot(recovery),
        baseline.series().provider_id.as_str(),
    )?;
    validate_publication_evidence(
        &snapshot_publication,
        &delta_publication,
        &recovery_publication,
        baseline.bar().source_sequence,
        next.bar().source_sequence,
        gap_sequence,
        &model,
    )?;
    Ok((3, 1))
}

fn validate_publication_identity(
    baseline: &BarUpdate,
    instrument: &InstrumentRevision,
    descriptor: &InstrumentDescriptor,
    bar_definition: &BarDefinition,
) -> Result<(), ConformanceHarnessError> {
    let interval_matches = match baseline.series().period {
        axiusflow_market_data::BarPeriod::Time { seconds } => {
            bar_definition.interval_seconds == seconds
        }
        axiusflow_market_data::BarPeriod::Daily => bar_definition.interval_seconds == 86_400,
        axiusflow_market_data::BarPeriod::Tick { .. } => false,
    };
    if instrument.instrument_id.as_str() == baseline.series().instrument_id
        && instrument.symbol == descriptor.display_symbol
        && instrument.venue_id == descriptor.venue_id
        && instrument.precision.price_scale() == descriptor.price_scale
        && instrument.precision.quantity_scale() == descriptor.quantity_scale
        && bar_definition.definition_id == publication_definition_id(baseline)
        && bar_definition.version == baseline.series().definition_version
        && interval_matches
    {
        Ok(())
    } else {
        Err(driver_error(
            "publication reference identity does not match the adapter series",
        ))
    }
}

fn validate_publication_evidence(
    snapshot: &MarketStreamPublication,
    delta: &MarketStreamPublication,
    recovery: &MarketStreamPublication,
    baseline_sequence: u64,
    next_sequence: u64,
    recovery_sequence: u64,
    model: &MarketBarClientModel,
) -> Result<(), ConformanceHarnessError> {
    let immutable_snapshot = snapshot.generation().sequence_range()
        == (baseline_sequence, baseline_sequence)
        && snapshot.predecessor_generation().is_none();
    let ordered_delta = delta.predecessor_generation() == Some(snapshot.generation().generation())
        && delta.generation().sequence_range().1 == next_sequence;
    let recovered = recovery.predecessor_generation().is_none()
        && recovery.generation().sequence_range() == (recovery_sequence, recovery_sequence)
        && !model.requires_snapshot();
    if immutable_snapshot && ordered_delta && recovered {
        Ok(())
    } else {
        Err(driver_error(
            "immutable publication provenance or recovery evidence is incomplete",
        ))
    }
}

fn publication_definition_id(update: &BarUpdate) -> String {
    format!(
        "{}:{}:{:?}",
        update.series().provider_id,
        update.series().instrument_id,
        update.series().period
    )
}

fn provenanced_bar(
    update: &BarUpdate,
    generation: SessionGeneration,
) -> Result<axiusflow_application::ProvenancedMarketBar, ConformanceHarnessError> {
    if update.metadata().session_generation != generation.get() {
        return Err(driver_error(
            "publication bar crossed the provider session generation",
        ));
    }
    let exchange_timestamp = update
        .metadata()
        .timestamps
        .exchange_unix_nanos
        .ok_or_else(|| driver_error("publication bar omits exchange time"))?;
    let provider_timestamp = update
        .metadata()
        .timestamps
        .provider_unix_nanos
        .unwrap_or(update.metadata().timestamps.received_unix_nanos);
    if provider_timestamp < exchange_timestamp
        || update.metadata().timestamps.received_unix_nanos < provider_timestamp
    {
        return Err(driver_error(
            "publication bar timestamp chronology is invalid",
        ));
    }
    Ok(Provenanced::new(
        update.bar(),
        MarketEventProvenance {
            event_id: format!(
                "{}-bar-{}",
                update.series().provider_id,
                update.bar().source_sequence
            ),
            event_time_unix_nanos: exchange_timestamp,
            publication_time_unix_nanos: update.metadata().timestamps.received_unix_nanos,
            producer: format!("{}_fixture_adapter", update.series().provider_id),
            schema_version: 1,
            correlation_id: format!("{}-fixture", update.series().provider_id),
            causation_id: String::new(),
            entitlement_revision: update.series().entitlement_id.clone(),
            partition_id: 1,
            ownership_epoch: generation.get(),
            source_id: update.series().provider_id.clone(),
            source_sequence: update.bar().source_sequence,
            exchange_timestamp_unix_nanos: exchange_timestamp,
            provider_receive_timestamp_unix_nanos: provider_timestamp,
            nic_receive_timestamp_unix_nanos: None,
            axiusflow_receive_timestamp_unix_nanos: update
                .metadata()
                .timestamps
                .received_unix_nanos,
            normalized_timestamp_unix_nanos: update.metadata().timestamps.received_unix_nanos,
            fanout_enqueue_timestamp_unix_nanos: None,
            correction_flags: 0,
            quality_flags: 0,
            nic_timestamp_source: 0,
            semantic_class: 0,
        },
    ))
}

fn gap_item_after(
    item: axiusflow_application::ProvenancedMarketBar,
) -> Result<axiusflow_application::ProvenancedMarketBar, ConformanceHarnessError> {
    let (mut bar, mut provenance) = item.into_parts();
    bar.source_sequence = bar
        .source_sequence
        .checked_add(2)
        .ok_or_else(|| driver_error("publication gap sequence overflowed"))?;
    bar.exchange_timestamp_seconds = bar
        .exchange_timestamp_seconds
        .checked_add(120)
        .ok_or_else(|| driver_error("publication gap timestamp overflowed"))?;
    let exchange_timestamp = bar
        .exchange_timestamp_seconds
        .checked_mul(1_000_000_000)
        .ok_or_else(|| driver_error("publication gap timestamp overflowed"))?;
    provenance.event_id = format!("{}-gap-{}", provenance.source_id, bar.source_sequence);
    provenance.source_sequence = bar.source_sequence;
    provenance.event_time_unix_nanos = exchange_timestamp;
    provenance.exchange_timestamp_unix_nanos = exchange_timestamp;
    provenance.publication_time_unix_nanos = exchange_timestamp.saturating_add(1_000);
    provenance.provider_receive_timestamp_unix_nanos = exchange_timestamp.saturating_add(500);
    provenance.axiusflow_receive_timestamp_unix_nanos = exchange_timestamp.saturating_add(1_000);
    provenance.normalized_timestamp_unix_nanos = exchange_timestamp.saturating_add(1_000);
    Ok(Provenanced::new(bar, provenance))
}

fn publish_update(
    model: &mut MarketBarClientModel,
    update: ReplayStreamUpdate,
    provider_id: &str,
) -> Result<MarketStreamPublication, ConformanceHarnessError> {
    let generation = match model
        .apply_update(update.clone())
        .map_err(|error| driver_error(error.to_string()))?
    {
        MarketBarModelOutcome::Published(generation) => generation,
        outcome => {
            return Err(driver_error(format!(
                "publication update did not publish: {outcome:?}"
            )));
        }
    };
    MarketStreamPublication::try_new(format!("{provider_id}-fixture"), update, generation)
        .map_err(|error| driver_error(error.to_string()))
}

fn driver_error(message: impl Into<String>) -> ConformanceHarnessError {
    ConformanceHarnessError::Driver(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_desktop_provider_runtime::{ProviderInvalidationReason, SessionGeneration};
    use axiusflow_instruments::{
        AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
    };
    use std::num::NonZeroU64;

    #[test]
    fn coinbase_and_rithmic_pass_the_shared_semantic_harness() {
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let coinbase = coinbase_fixture(generation);
        let coinbase_report =
            run_provider_adapter_conformance(&coinbase).expect("Coinbase fixture conforms");
        assert_eq!(coinbase_report.market_events, 3);
        assert_eq!(coinbase_report.depth_publications, 2);

        let rithmic_report = run_provider_adapter_conformance(&rithmic_fixture(generation))
            .expect("Rithmic fixture conforms");
        assert_eq!(rithmic_report.market_events, 3);
        assert_eq!(rithmic_report.depth_publications, 2);
        assert_eq!(rithmic_report, coinbase_report);
        assert_eq!(coinbase_report.book_recoveries, 1);
        assert_eq!(coinbase_report.covering_snapshots, 1);
        assert_eq!(coinbase_report.stream_publications, 3);
        assert_eq!(coinbase_report.stream_recoveries, 1);
    }

    #[test]
    fn diagnostics_bind_to_the_explicit_selected_environment() {
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut fixture = rithmic_fixture(generation);
        let ProviderSessionEvent::SystemsDiscovered { environments } = &mut fixture.events[1]
        else {
            panic!("fixture systems event is stable");
        };
        environments.insert(
            0,
            ProviderEnvironment {
                provider_id: "rithmic".to_string(),
                system_id: "RITHMIC_PAPER".to_string(),
                environment: "Paper".to_string(),
            },
        );
        run_provider_adapter_conformance(&fixture)
            .expect("selected Test environment remains authoritative");

        fixture.selected_environment.system_id = "UNDISCOVERED".to_string();
        assert!(run_provider_adapter_conformance(&fixture).is_err());
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
        let mut fixture = rithmic_fixture(generation);
        let ProviderSessionEvent::InstrumentsDiscovered { instruments, .. } =
            &mut fixture.events[3]
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
        let mut second_snapshot = fixture.events[5].clone();
        let ProviderSessionEvent::Market {
            event: MarketEvent::DepthSnapshot(snapshot),
            ..
        } = &mut second_snapshot
        else {
            panic!("fixture depth snapshot event is stable");
        };
        snapshot.metadata.instrument_id = "instrument:rithmic:cme:nq".to_string();
        snapshot.metadata.source_sequence = 1;
        fixture.events.insert(7, second_snapshot);

        let report = run_provider_adapter_conformance(&fixture)
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

    #[test]
    fn shared_harness_rejects_noncovering_book_recovery() {
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut fixture = coinbase_fixture(generation);
        let MarketEvent::DepthSnapshot(snapshot) = &mut fixture.book_recovery_events[4] else {
            panic!("fixture covering snapshot is stable");
        };
        snapshot.metadata.source_sequence = 12;

        assert!(run_provider_adapter_conformance(&fixture).is_err());
    }

    #[test]
    fn shared_harness_rejects_mismatched_publication_identity() {
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut fixture = rithmic_fixture(generation);
        fixture.bar_definition.definition_id = "wrong-series".to_string();

        assert!(run_provider_adapter_conformance(&fixture).is_err());
    }

    #[test]
    fn shared_harness_rejects_publication_precision_mismatch() {
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut fixture = coinbase_fixture(generation);
        fixture.publication_instrument.precision =
            InstrumentPrecision::try_new(4, 8).expect("mismatched fixture precision");

        assert!(run_provider_adapter_conformance(&fixture).is_err());
    }

    #[test]
    fn shared_harness_rejects_unfenced_terminal_invalidation() {
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut fixture = coinbase_fixture(generation);
        let ProviderSessionEvent::Invalidated {
            generation: invalidation_generation,
            ..
        } = &mut fixture.events[8]
        else {
            panic!("fixture invalidation event is stable");
        };
        *invalidation_generation = None;

        assert!(run_provider_adapter_conformance(&fixture).is_err());
    }

    #[test]
    fn shared_harness_rejects_bar_interval_mismatch() {
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut fixture = rithmic_fixture(generation);
        fixture.bar_definition.interval_seconds = 300;

        assert!(run_provider_adapter_conformance(&fixture).is_err());
    }

    #[test]
    fn shared_harness_rejects_reversed_publication_timestamps() {
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut fixture = coinbase_fixture(generation);
        let BarUpdate::Forming { metadata, .. } = &mut fixture.bar_updates[2] else {
            panic!("fixture next forming bar is stable");
        };
        metadata.timestamps.received_unix_nanos = 1_800_000_000_000_000_000;

        assert!(run_provider_adapter_conformance(&fixture).is_err());
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
        let session = axiusflow_coinbase_market_adapter::deterministic_fixture_session(generation)
            .expect("Coinbase fixture builds");
        ProviderAdapterFixture {
            events: session.events,
            selected_environment: ProviderEnvironment {
                provider_id: "coinbase".to_string(),
                system_id: "advanced_trade_public".to_string(),
                environment: "production".to_string(),
            },
            bar_updates: session.bar_updates,
            book_recovery_events: session.book_recovery_events,
            publication_instrument: fixture_instrument(
                "instrument:coinbase:btc:usd",
                AssetClass::CryptoAsset,
                "BTC/USD",
                "COINBASE",
                "USD",
                2,
                8,
            ),
            bar_definition: fixture_bar_definition("coinbase", "instrument:coinbase:btc:usd"),
        }
    }

    fn rithmic_fixture(generation: SessionGeneration) -> ProviderAdapterFixture {
        let session = axiusflow_rithmic_fixture_adapter::deterministic_session(generation);
        ProviderAdapterFixture {
            events: session.events,
            selected_environment: ProviderEnvironment {
                provider_id: "rithmic".to_string(),
                system_id: "RITHMIC_TEST".to_string(),
                environment: "Test".to_string(),
            },
            bar_updates: session.bar_updates,
            book_recovery_events: session.book_recovery_events,
            publication_instrument: fixture_instrument(
                "instrument:rithmic:cme:es",
                AssetClass::Future,
                "ES",
                "CME",
                "USD",
                2,
                0,
            ),
            bar_definition: fixture_bar_definition("rithmic", "instrument:rithmic:cme:es"),
        }
    }

    fn fixture_instrument(
        instrument_id: &str,
        asset_class: AssetClass,
        symbol: &str,
        venue_id: &str,
        trading_currency: &str,
        price_scale: u8,
        quantity_scale: u8,
    ) -> InstrumentRevision {
        InstrumentRevision {
            instrument_id: InstrumentId::try_new(instrument_id).expect("fixture instrument id"),
            revision: 1,
            asset_class,
            symbol: symbol.to_string(),
            venue_id: venue_id.to_string(),
            trading_currency: trading_currency.to_string(),
            precision: InstrumentPrecision::try_new(price_scale, quantity_scale)
                .expect("fixture precision"),
            lifecycle: InstrumentLifecycle::Active,
        }
    }

    fn fixture_bar_definition(provider_id: &str, instrument_id: &str) -> BarDefinition {
        BarDefinition {
            definition_id: format!(
                "{provider_id}:{instrument_id}:{:?}",
                axiusflow_market_data::BarPeriod::Time { seconds: 60 }
            ),
            version: 1,
            interval_seconds: 60,
            trades_per_bar: None,
        }
    }
}
