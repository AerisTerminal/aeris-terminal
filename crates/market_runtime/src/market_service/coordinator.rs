use super::lifecycle::{self, SeriesLifecycles};
use super::{
    Arc, AtomicBool, BTreeMap, BTreeSet, BarSeriesKey, COORDINATOR_TICK, ClientId, Command,
    ConsumerEvents, ConsumerId, ConsumerIdentity, ConsumerResourceClass, DemandWaiter, Duration,
    EngineError, GenerationId, InstallProviderInstrument, Instant, MAXIMUM_STUDIES,
    MAXIMUM_STUDY_DEPENDENCIES, MAXIMUM_STUDY_OUTPUTS, MAXIMUM_STUDY_POINTS_PER_OUTPUT,
    MAXIMUM_STUDY_STATE_BYTES_PER_INSTANCE, MAXIMUM_STUDY_TOTAL_OUTPUT_POINTS,
    MAXIMUM_STUDY_TOTAL_STATE_BYTES, MarketDataLeaseId, MarketEngine, MarketServiceStatus,
    MarketStream, Mutex, NonZeroUsize, Ordering, PriceAlertRegistry, ProviderConnectionState,
    ProviderDispatch, ProviderEpochRegistry, ProviderGeneration, ProviderHealth, ProviderOrderBook,
    ProviderRuntimeEvent, ProviderRuntimeRegistry, ProviderState, REALTIME_CAPACITY,
    REALTIME_DRAIN_BUDGET, Receiver, RecvTimeoutError, Reply, StreamRequirements,
    StudyMarketLeaseChangeKind, StudyRuntime, StudyRuntimeConfig, authorize_consumer,
    publish_state, thread,
};
#[cfg(test)]
use super::{
    TradeModel,
    lifecycle::{SeriesLifecycle, SeriesState},
};
use crate::{
    MarketRuntimeEvent, MarketStudyOutputSnapshot,
    study::{
        StudyExecutionBatch, StudyLiveMarketData, StudyMarketInput, StudyNonBarChange,
        StudyRuntimeError,
    },
};
use aeris_market_engine::fence::{ConsumerTag, Epoch, EpochSequence};
use aeris_observability::diagnostic;

pub(super) struct OwnedCoordinatorChannels {
    pub(super) commands: Receiver<Command>,
    pub(super) providers: ProviderRuntimeRegistry,
    pub(super) quote_releases: Arc<Mutex<Vec<MarketDataLeaseId>>>,
    pub(super) quotes: Arc<Mutex<super::quotes::QuoteBook>>,
    pub(super) trading_feed: Arc<super::trading_feed::TradingFeedOutbox>,
    pub(super) outbox: Arc<super::outbox::ConsumerOutbox>,
}

struct FeedChannels {
    releases: Arc<Mutex<Vec<MarketDataLeaseId>>>,
    quotes: Arc<Mutex<super::quotes::QuoteBook>>,
    trading_feed: Arc<super::trading_feed::TradingFeedOutbox>,
    outbox: Arc<super::outbox::ConsumerOutbox>,
}

#[derive(Clone, Copy, Default)]
struct TimerKey(u8);

impl TimerKey {
    const WATCHDOG: u8 = 1;
    const HISTORY_RETRY: u8 = 2;
    const SESSION_TRANSITION: u8 = 4;
    const TRADE_TAPE: u8 = 8;
    const CONTROL_RETRY: u8 = 16;

    fn contains(self, timer: u8) -> bool {
        self.0 & timer != 0
    }
}

#[derive(Clone, Copy)]
pub(super) enum StudyChange<'a> {
    Ready(super::StudyInstanceId),
    ReadySubtree(super::StudyInstanceId),
    Market(&'a BarSeriesKey),
    Bar(&'a BarSeriesKey, i64),
    History(&'a BarSeriesKey, i64, i64),
    NonBar(StudyNonBarChange<'a>),
}

pub(super) fn spawn_coordinator(
    engine: MarketEngine,
    channels: OwnedCoordinatorChannels,
    shutdown: Arc<AtomicBool>,
    clock: Arc<dyn super::clock::Clock>,
) -> Result<thread::JoinHandle<()>, String> {
    thread::Builder::new()
        .name("aeris-market-engine".to_string())
        .spawn(move || {
            let OwnedCoordinatorChannels {
                commands,
                mut providers,
                quote_releases,
                quotes,
                trading_feed,
                outbox,
            } = channels;
            {
                let dispatch = providers.dispatch();
                run_coordinator(
                    engine,
                    &commands,
                    dispatch,
                    &shutdown,
                    clock,
                    FeedChannels {
                        releases: quote_releases,
                        quotes,
                        trading_feed,
                        outbox,
                    },
                );
            }
            // A history worker may be blocked on a full bounded completion
            // channel. Close that receiver before joining provider workers.
            drop(commands);
            let panicked = providers.cancel_and_join();
            assert!(
                panicked.is_empty(),
                "provider runtime workers panicked during shutdown: {}",
                panicked.join(", ")
            );
        })
        .map_err(|error| error.to_string())
}

fn run_coordinator(
    engine: MarketEngine,
    commands: &Receiver<Command>,
    providers: ProviderDispatch<'_>,
    shutdown: &AtomicBool,
    clock: Arc<dyn super::clock::Clock>,
    feeds: FeedChannels,
) {
    let mut coordinator = Coordinator::with_clock(engine, providers, clock);
    coordinator.quote_releases = Some(feeds.releases);
    coordinator.quotes = feeds.quotes;
    coordinator.trading_feed = feeds.trading_feed;
    coordinator.outbox = feeds.outbox;
    let demand_flushers = coordinator
        .providers
        .records
        .values()
        .map(|record| record.descriptor.flush_demand)
        .collect::<Vec<_>>();
    loop {
        if shutdown.load(Ordering::Acquire) {
            drain_shutdown_provider_events(&mut coordinator);
            coordinator.begin_shutdown();
            return;
        }
        if !coordinator_turn(&mut coordinator, commands, &demand_flushers, shutdown, None) {
            if shutdown.load(Ordering::Acquire) {
                drain_shutdown_provider_events(&mut coordinator);
                coordinator.begin_shutdown();
            }
            return;
        }
    }
}

/// Runs one production coordinator turn through the bounded provider lanes and
/// command receiver. A quiet coordinator blocks until a command or a real deadline.
fn coordinator_turn(
    coordinator: &mut Coordinator<'_>,
    commands: &Receiver<Command>,
    demand_flushers: &[for<'a> fn(&mut Coordinator<'a>)],
    shutdown: &AtomicBool,
    test_wait: Option<Duration>,
) -> bool {
    coordinator.studies.begin_turn();
    if coordinator.trading_feed.attached() && !coordinator.trading_feed_snapshot_sent {
        coordinator.trading_feed_snapshot_sent = true;
        coordinator.publish_trading_feed_snapshot();
    }
    if let Some(releases) = &coordinator.quote_releases {
        let released = std::mem::take(
            &mut *releases
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for id in released {
            if coordinator.engine.release_data_lease(id) {
                coordinator.reconcile_order_books();
                coordinator.mark_alert_demand_changed();
                coordinator.pending_activity = true;
            }
        }
    }
    let drained = drain_coordinator_events(coordinator);
    let now = coordinator.clock.monotonic();
    let due = coordinator.take_due_timers(now);
    let active = coordinator.pending_activity || drained > 0;
    coordinator.pending_activity = false;
    if active || due.contains(TimerKey::TRADE_TAPE) {
        coordinator.broadcast_dirty_trade_tapes();
    }
    if active || !coordinator.dirty_series.is_empty() {
        coordinator.publish_lifecycle_live();
        coordinator.recover_overflowed_series_queues();
    }
    if active {
        coordinator.control_retry = false;
        for flush in demand_flushers {
            flush(coordinator);
        }
        coordinator.release_idle();
    }
    if due.contains(TimerKey::CONTROL_RETRY) && !active {
        coordinator.control_retry = false;
        coordinator.connect_due_providers();
        for flush in demand_flushers {
            flush(coordinator);
        }
        coordinator.release_idle();
    }
    if due.contains(TimerKey::WATCHDOG | TimerKey::SESSION_TRANSITION) {
        coordinator.recover_stalled_live_series();
    }
    if due.contains(TimerKey::SESSION_TRANSITION) {
        let keys = coordinator
            .engine
            .instrument_subscriptions()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in &keys {
            coordinator.publish_trading_session(key);
        }
    }
    if due.contains(TimerKey::HISTORY_RETRY) {
        coordinator.retry_history();
    }
    coordinator.publish_failed_provider_controls();
    if active || !coordinator.dirty_series.is_empty() || due.0 != 0 {
        coordinator.refresh_deadlines();
    }
    coordinator.signal_pending_consumers();
    let wait = if drained >= REALTIME_DRAIN_BUDGET {
        Some(Duration::ZERO)
    } else {
        match (coordinator.next_wait(), test_wait) {
            (Some(deadline), Some(cap)) => Some(deadline.min(cap)),
            (None, cap) => cap,
            (deadline, None) => deadline,
        }
    };
    let received = match wait {
        Some(timeout) => commands.recv_timeout(timeout),
        None => commands.recv().map_err(|_| RecvTimeoutError::Disconnected),
    };
    match received {
        Ok(command) if !shutdown.load(Ordering::Acquire) => {
            coordinator.handle_command(command);
            coordinator.pending_activity = true;
        }
        Ok(_) | Err(RecvTimeoutError::Disconnected) => return false,
        Err(RecvTimeoutError::Timeout) => {}
    }
    true
}

impl<'a> Coordinator<'a> {
    fn new(engine: MarketEngine, providers: ProviderDispatch<'a>) -> Self {
        Self::with_clock(engine, providers, Arc::new(super::clock::SystemClock))
    }

    fn with_clock(
        engine: MarketEngine,
        providers: ProviderDispatch<'a>,
        clock: Arc<dyn super::clock::Clock>,
    ) -> Self {
        let mut provider_registry = super::provider_session::ProviderRegistry::new();
        for (&id, record) in &providers.records {
            provider_registry.register_control(id);
            if let Some(channels) = record.hyperliquid {
                provider_registry.register(
                    id,
                    Box::new(super::provider_session::HyperliquidSession {
                        controls: channels.controls.clone(),
                        display_controls: channels.display_controls.clone(),
                        history: record.history.clone(),
                    }),
                    record.descriptor.capabilities.reconnect,
                    clock.as_ref(),
                );
            }
            if let Some(channels) = record.ctrader {
                provider_registry.register(
                    id,
                    Box::new(super::provider_session::CtraderSession {
                        controls: channels.controls.clone(),
                        history: record.history.clone(),
                    }),
                    record.descriptor.capabilities.reconnect,
                    clock.as_ref(),
                );
            }
            if let Some(channels) = record.tastytrade {
                provider_registry.register(
                    id,
                    Box::new(super::provider_session::TastytradeSession {
                        controls: channels.controls.clone(),
                        history: record.history.clone(),
                    }),
                    record.descriptor.capabilities.reconnect,
                    clock.as_ref(),
                );
            }
            if let Some(channels) = record.rithmic {
                provider_registry.register(
                    id,
                    Box::new(super::provider_session::RithmicSession {
                        controls: channels.controls.clone(),
                        history: record.history.clone(),
                        binding: Arc::clone(&channels.binding),
                    }),
                    record.descriptor.capabilities.reconnect,
                    clock.as_ref(),
                );
            }
        }
        let mut coordinator = Self {
            engine,
            studies: configured_studies(),
            providers,
            clock,
            calendar: super::calendar::MarketCalendarService::default(),
            provider_epochs: ProviderEpochRegistry::default(),
            provider_registry,
            attached: BTreeSet::new(),
            consumer_clients: BTreeMap::new(),
            consumer_epochs: BTreeMap::new(),
            consumer_epoch_sequence: EpochSequence::default(),
            pending: BTreeMap::new(),
            history: super::history::tracker::HistoryTracker::default(),
            events: BTreeMap::new(),
            series: lifecycle::SeriesLifecycles::default(),
            dirty_series: BTreeSet::new(),
            deadlines: BTreeMap::new(),
            pending_activity: true,
            quote_releases: None,
            quotes: Arc::new(Mutex::new(super::quotes::QuoteBook::default())),
            trading_feed: Arc::new(super::trading_feed::TradingFeedOutbox::default()),
            outbox: Arc::new(super::outbox::ConsumerOutbox::default()),
            trading_feed_snapshot_sent: false,
            trading_interest_revision: 0,
            control_retry: false,
            series_effects: super::lifecycle::EffectBuf::default(),
            order_books: BTreeMap::new(),
            display_depth: BTreeMap::new(),
            display_generations: BTreeMap::new(),
            price_alerts: PriceAlertRegistry::default(),
            catalog: BTreeMap::new(),
            catalog_sessions: BTreeMap::new(),
            command_fences: super::CommandFences::default(),
            pending_selections: BTreeMap::new(),
            selection_epochs: BTreeMap::new(),
        };
        for provider in coordinator.provider_registry.catalog_connect_ids() {
            let _ = coordinator.ensure_provider_connection(provider);
        }
        coordinator
    }
}

fn configured_studies() -> StudyRuntime {
    StudyRuntime::new(StudyRuntimeConfig {
        maximum_studies: NonZeroUsize::new(MAXIMUM_STUDIES).unwrap_or(NonZeroUsize::MIN),
        maximum_dependencies_per_study: NonZeroUsize::new(MAXIMUM_STUDY_DEPENDENCIES)
            .unwrap_or(NonZeroUsize::MIN),
        maximum_outputs_per_study: NonZeroUsize::new(MAXIMUM_STUDY_OUTPUTS)
            .unwrap_or(NonZeroUsize::MIN),
        maximum_points_per_output: NonZeroUsize::new(MAXIMUM_STUDY_POINTS_PER_OUTPUT)
            .unwrap_or(NonZeroUsize::MIN),
        maximum_total_output_points: NonZeroUsize::new(MAXIMUM_STUDY_TOTAL_OUTPUT_POINTS)
            .unwrap_or(NonZeroUsize::MIN),
        maximum_state_bytes_per_study: NonZeroUsize::new(MAXIMUM_STUDY_STATE_BYTES_PER_INSTANCE)
            .unwrap_or(NonZeroUsize::MIN),
        maximum_total_state_bytes: NonZeroUsize::new(MAXIMUM_STUDY_TOTAL_STATE_BYTES)
            .unwrap_or(NonZeroUsize::MIN),
    })
}

fn drain_coordinator_events(coordinator: &mut Coordinator<'_>) -> usize {
    if let Some(wake) = coordinator.providers.wake {
        for (provider, slot) in wake.slots.iter() {
            if slot.catalog_overflow.swap(false, Ordering::AcqRel) {
                coordinator.reject_overflowed_catalog(provider);
            }
        }
    }
    let mut drained = 0;
    let started = Instant::now();
    let first_lane = coordinator
        .providers
        .wake
        .map_or(0, |wake| wake.drain_cursor.load(Ordering::Relaxed));
    let lane_count = coordinator.providers.lanes.len();
    'drain: for _ in 0..REALTIME_DRAIN_BUDGET {
        for offset in 0..lane_count {
            let lane = (first_lane + offset) % lane_count;
            if let Some(wake) = coordinator.providers.wake {
                wake.drain_cursor
                    .store((lane + 1) % lane_count, Ordering::Relaxed);
                if started.elapsed() >= Duration::from_millis(4) {
                    wake.notify();
                    break 'drain;
                }
            }
            let Some(event) = coordinator.providers.take_event(lane) else {
                continue;
            };
            if event.metadata().is_some_and(|metadata| {
                coordinator
                    .providers
                    .wake
                    .is_some_and(|wake| wake.overflowed(metadata.provider, metadata.generation))
            }) {
                continue;
            }
            drained += 1;
            match event {
                #[cfg(test)]
                ProviderRuntimeEvent::Realtime(event) => coordinator.handle_provider_event(event),
                ProviderRuntimeEvent::BoundRealtime(event) => {
                    coordinator.handle_bound_provider_event(event);
                }
                ProviderRuntimeEvent::DisplayDepth(provider, event) => {
                    coordinator.handle_provider_display_depth(provider, event);
                }
                ProviderRuntimeEvent::Catalog(provider, event) => {
                    coordinator.handle_provider_catalog(provider, event);
                }
            }
        }
    }
    if let Some(wake) = coordinator.providers.wake {
        for (provider, slot) in wake.slots.iter() {
            let generation = slot.pending_overflow.swap(0, Ordering::AcqRel);
            if generation == 0 {
                continue;
            }
            let Some(descriptor) = coordinator.providers.descriptor(provider) else {
                continue;
            };
            coordinator.mark_lifecycle_recovering(
                provider,
                generation,
                descriptor.capabilities.live_model,
                descriptor.capabilities.overflow_recovery_detail,
            );
            if let Some(epoch) = coordinator.provider_epochs.current(provider, generation)
                && coordinator.provider_registry.contains(provider)
            {
                coordinator.provider_registry.disconnected(
                    provider,
                    epoch,
                    coordinator.clock.monotonic(),
                );
                coordinator.session_mut(provider).accepted = None;
            }
            diagnostic!(
                "Aeris market {provider} event queue overflowed at generation {generation}"
            );
        }
        for (provider, slot) in wake.slots.iter() {
            let generation = slot.pending_failure.swap(0, Ordering::AcqRel);
            if generation != 0 {
                coordinator.mark_lifecycle_failed(
                    provider,
                    generation,
                    "Adapter transport retry scheduler exhausted; reconnect in Accounts",
                );
                diagnostic!(
                    "Aeris market {provider} stopped after retry limit at generation {generation}"
                );
            }
        }
    }
    drained
}

/// Offline, bounded coordinator workload. Idle work is zero when no deadline
/// or command exists; loaded work uses 63 batches of 64 canonical series.
/// CPU is active coordinator work, not process-wide CPU.
pub(super) fn performance_baseline() -> Result<serde_json::Value, String> {
    use serde_json::json;
    use std::num::NonZeroU64;

    const SERIES: usize = 64;
    const TURNS: usize = 63;
    let (mut coordinator, keys) = benchmark_coordinator()?;
    let capacity = super::CONSUMER_SERIES_QUEUE_CAPACITY;
    let generation = ProviderGeneration(NonZeroU64::MIN);
    let idle_started = Instant::now();
    // No coordinator turns occur in a simulated second with no deadlines.
    debug_assert!(coordinator.next_wait().is_none());
    let idle_nanos = idle_started.elapsed().as_nanos();
    let loaded_started = Instant::now();
    for revision in 0..TURNS {
        for key in &keys {
            let mut bar = benchmark_bar(1);
            bar.close = 100 + i64::try_from(revision).map_err(|error| error.to_string())?;
            for update in coordinator
                .engine
                .install_realtime_tail(generation, key, 2, 0, bar, true)
                .map_err(|error| error.to_string())?
            {
                let events = coordinator
                    .events
                    .get_mut(&update.consumer_id)
                    .ok_or("missing consumer outbox")?;
                events.publish_series_update(MarketRuntimeEvent::SeriesUpdate(update));
            }
        }
        benchmark_active_turn(&mut coordinator);
        for events in coordinator.events.values_mut() {
            while events.pop().is_some() {}
        }
    }
    let loaded_nanos = loaded_started.elapsed().as_nanos();
    let overflow_nanos = benchmark_overflow(&mut coordinator, &keys[0], capacity)?;
    Ok(json!({
        "series": SERIES,
        "simulated_seconds": 1,
        "idle_coordinator_cpu_nanos_per_second": idle_nanos,
        "loaded_coordinator_cpu_nanos_per_second": loaded_nanos,
        "idle_wakeups_per_second": 0,
        "loaded_wakeups_per_second": TURNS,
        "overflow_capacity": capacity,
        "covering_snapshot_nanos": overflow_nanos,
        "overflow_events_dropped": capacity,
        "cpu_measurement": "active single-thread coordinator workload: zero idle turns; 63 loaded batches of 64 series",
    }))
}

pub(super) fn bridge_wake_performance() -> Result<serde_json::Value, String> {
    use std::{num::NonZeroU64, sync::mpsc};

    const SAMPLES: usize = 256;
    const IDLE_WINDOW: Duration = Duration::from_secs(30);
    let (mut coordinator, keys) = benchmark_coordinator()?;
    let consumer = ConsumerId(NonZeroU64::MIN);
    let client = ClientId(NonZeroU64::MIN);
    coordinator.attached.insert(client);
    coordinator.consumer_clients.insert(consumer, client);
    coordinator.outbox.attach(client);
    let outbox = Arc::clone(&coordinator.outbox);
    let (received, times) = mpsc::sync_channel::<Instant>(1);
    let bridge = thread::spawn(move || -> Result<(Vec<u128>, usize), String> {
        let mut latencies = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            if !outbox.wait(client, Duration::from_secs(5)) {
                return Err("bridge publication wake timed out".into());
            }
            let started = times.recv().map_err(|error| error.to_string())?;
            latencies.push(started.elapsed().as_nanos());
        }
        let idle_start = Instant::now();
        let mut idle_wakeups = 0;
        while let Some(remaining) = IDLE_WINDOW.checked_sub(idle_start.elapsed()) {
            if outbox.wait(client, remaining) {
                idle_wakeups += 1;
            }
        }
        Ok((latencies, idle_wakeups))
    });
    for _ in 0..SAMPLES {
        let started = Instant::now();
        coordinator
            .events
            .get_mut(&consumer)
            .ok_or("benchmark consumer outbox missing")?
            .series_state = Some(super::series_state(
            consumer,
            GenerationId(NonZeroU64::MIN),
            keys[0].clone(),
            super::SeriesLoadState::Live,
            None,
        ));
        received.send(started).map_err(|error| error.to_string())?;
        coordinator.signal_pending_consumers();
        // The event remains coordinator-owned until the bridge has received
        // the wake; no repeated notification is generated while it is idle.
        while coordinator
            .events
            .get_mut(&consumer)
            .and_then(ConsumerEvents::pop)
            .is_none()
        {
            thread::yield_now();
        }
        // A rendezvous on the next single-slot timestamp prevents sample
        // replacement; receipt timing includes only wake and scheduling.
    }
    let (mut latencies, idle_wakeups) = bridge
        .join()
        .map_err(|_| "bridge benchmark worker panicked".to_string())??;
    latencies.sort_unstable();
    Ok(serde_json::json!({
        "bridge_wake_latency_p50_nanos": latencies[SAMPLES / 2],
        "bridge_wake_latency_p99_nanos": latencies[SAMPLES * 99 / 100],
        "bridge_idle_wakeups_per_second": f64::from(u32::try_from(idle_wakeups).unwrap_or(u32::MAX)) / IDLE_WINDOW.as_secs_f64(),
        "bridge_idle_window_seconds": IDLE_WINDOW.as_secs(),
        "bridge_wake_samples": SAMPLES,
    }))
}

fn benchmark_coordinator() -> Result<(Coordinator<'static>, Vec<BarSeriesKey>), String> {
    use aeris_market_data::BarPeriod;
    use aeris_market_engine::{ProviderCapabilities, ProviderConfig, WorkspaceId};
    use std::num::NonZeroU64;

    const SERIES: usize = 64;
    let mut engine =
        super::configured_engine_from_descriptors(super::BUILT_IN_PROVIDER_DESCRIPTORS)?;
    let generation = ProviderGeneration(NonZeroU64::MIN);
    engine
        .register_provider(
            "synthetic".into(),
            ProviderConfig {
                account_id: "synthetic:public".into(),
                capabilities: ProviderCapabilities {
                    historical_bars: true,
                    realtime_bars: true,
                    streams: StreamRequirements::BARS,
                },
                reconnect_delay: Duration::from_millis(250),
            },
        )
        .map_err(|error| error.to_string())?;
    engine
        .begin_provider_session("synthetic", generation)
        .map_err(|error| error.to_string())?;
    let mut keys = Vec::with_capacity(SERIES);
    for index in 0..SERIES {
        let consumer = ConsumerId(NonZeroU64::new(index as u64 + 1).ok_or("zero consumer")?);
        let key = BarSeriesKey {
            provider_id: "synthetic".into(),
            instrument_id: format!("benchmark:{index}"),
            entitlement_id: "public".into(),
            period: BarPeriod::Time { seconds: 60 },
            definition_version: 1,
        };
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(NonZeroU64::MIN),
                    workspace_id: WorkspaceId(NonZeroU64::MIN),
                    consumer_id: consumer,
                },
                true,
            )
            .map_err(|error| error.to_string())?;
        engine
            .set_series_demand(consumer, GenerationId(NonZeroU64::MIN), &key)
            .map_err(|error| error.to_string())?;
        engine
            .install_realtime(generation, &key, 2, 0, vec![benchmark_bar(1)], true)
            .map_err(|error| error.to_string())?;
        keys.push(key);
    }
    let providers = ProviderDispatch {
        wake: None,
        records: BTreeMap::new(),
        lanes: Vec::new(),
    };
    let mut coordinator = Coordinator::new(engine, providers);
    for index in 0..SERIES {
        coordinator.events.insert(
            ConsumerId(NonZeroU64::new(index as u64 + 1).ok_or("zero consumer")?),
            ConsumerEvents::default(),
        );
    }
    Ok((coordinator, keys))
}

fn benchmark_overflow(
    coordinator: &mut Coordinator<'_>,
    key: &BarSeriesKey,
    capacity: usize,
) -> Result<u128, String> {
    use std::num::NonZeroU64;

    let generation = ProviderGeneration(NonZeroU64::MIN);
    let consumer = ConsumerId(NonZeroU64::MIN);
    let overflow_started = Instant::now();
    for revision in 0..=capacity {
        let bar = benchmark_bar(u64::try_from(revision).map_err(|error| error.to_string())? + 2);
        let update = coordinator
            .engine
            .install_realtime_tail(generation, key, 2, 0, bar, true)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|update| update.consumer_id == consumer)
            .ok_or("missing benchmark publication")?;
        coordinator
            .events
            .get_mut(&consumer)
            .ok_or("missing overflow outbox")?
            .publish_series_update(MarketRuntimeEvent::SeriesUpdate(update));
    }
    coordinator.recover_overflowed_series_queues();
    let overflow_nanos = overflow_started.elapsed().as_nanos();
    let first = coordinator
        .events
        .get_mut(&consumer)
        .and_then(ConsumerEvents::pop)
        .ok_or("overflow produced no covering publication")?;
    let MarketRuntimeEvent::SeriesSnapshot(_) = first else {
        return Err("overflow did not produce a covering snapshot".into());
    };
    Ok(overflow_nanos)
}

fn benchmark_active_turn(coordinator: &mut Coordinator<'_>) {
    coordinator.studies.begin_turn();
    drain_coordinator_events(coordinator);
    coordinator.broadcast_dirty_trade_tapes();
    coordinator.publish_lifecycle_live();
    coordinator.recover_overflowed_series_queues();
    coordinator.release_idle();
    coordinator.refresh_deadlines();
}

fn benchmark_bar(sequence: u64) -> aeris_market_data::MarketBar {
    let seconds = 1_700_000_000 + sequence.cast_signed() * 60;
    aeris_market_data::MarketBar {
        source_sequence: sequence,
        exchange_timestamp_seconds: seconds,
        exchange_timestamp_unix_nanos: seconds * 1_000_000_000,
        open: 100,
        high: 5_000,
        low: 90,
        close: 100,
        volume: 7,
    }
}

#[cfg(test)]
mod performance_tests {
    #[test]
    fn scripted_idle_coordinator_has_no_publication_tick() {
        let (mut coordinator, _) = super::benchmark_coordinator().expect("fixture");
        let clock = std::sync::Arc::new(super::super::clock::ScriptedClock::new(0));
        coordinator.clock = clock.clone();
        coordinator.refresh_deadlines();
        assert_eq!(coordinator.next_wait(), None);
        clock.advance(std::time::Duration::from_secs(60));
        assert_eq!(coordinator.next_wait(), None);
    }

    #[test]
    fn publication_cap_exists_only_while_series_is_dirty() {
        let (mut coordinator, keys) = super::benchmark_coordinator().expect("fixture");
        assert_eq!(coordinator.next_wait(), None);
        coordinator.dirty_series.insert(keys[0].clone());
        assert_eq!(coordinator.next_wait(), Some(super::COORDINATOR_TICK));
        coordinator.dirty_series.clear();
        assert_eq!(coordinator.next_wait(), None);
    }

    #[test]
    fn scripted_watchdog_deadline_waits_without_periodic_ticks() {
        use super::super::clock::{Clock, ScriptedClock};
        use super::super::lifecycle::{SeriesLifecycle, SeriesState};
        let (mut coordinator, keys) = super::benchmark_coordinator().expect("fixture");
        let clock = std::sync::Arc::new(ScriptedClock::new(0));
        coordinator.clock = clock.clone();
        coordinator
            .engine
            .set_provider_health(
                "synthetic",
                super::ProviderGeneration(std::num::NonZeroU64::MIN),
                super::ProviderHealth::Online,
            )
            .expect("provider online");
        let mut live = SeriesLifecycle::with_clock(
            keys[0].clone(),
            super::ProviderGeneration(std::num::NonZeroU64::MIN),
            "",
            "",
            super::super::CandleGapPolicy::Contiguous,
            clock.clone(),
        );
        live.connected = true;
        live.state = SeriesState::Live {
            last_progress: clock.monotonic(),
            observed_live: true,
        };
        coordinator.series.insert_candle(keys[0].clone(), live);
        coordinator.refresh_deadlines();
        assert_eq!(
            coordinator.next_wait(),
            Some(std::time::Duration::from_secs(300))
        );
        clock.advance(std::time::Duration::from_secs(299));
        assert_eq!(
            coordinator.next_wait(),
            Some(std::time::Duration::from_secs(1))
        );
        clock.advance(std::time::Duration::from_secs(1));
        assert!(
            coordinator
                .take_due_timers(clock.monotonic())
                .contains(super::TimerKey::WATCHDOG)
        );
    }

    #[test]
    fn one_dirty_series_does_not_publish_untouched_series() {
        use super::super::clock::{Clock, ScriptedClock};
        use super::super::lifecycle::{SeriesLifecycle, SeriesState};
        let (mut coordinator, keys) = super::benchmark_coordinator().expect("fixture");
        let clock = std::sync::Arc::new(ScriptedClock::new(0));
        for (index, key) in keys.iter().take(2).enumerate() {
            let mut live = SeriesLifecycle::with_clock(
                key.clone(),
                super::ProviderGeneration(std::num::NonZeroU64::MIN),
                "",
                "",
                super::super::CandleGapPolicy::Contiguous,
                clock.clone(),
            );
            live.connected = true;
            live.price_scale = 2;
            live.state = SeriesState::Live {
                last_progress: clock.monotonic(),
                observed_live: true,
            };
            if index == 0 {
                let mut bar = super::benchmark_bar(2);
                bar.close = 200;
                live.pending_publications.push_back(bar);
                live.dirty = true;
            }
            coordinator.series.insert_candle(key.clone(), live);
        }
        coordinator.publish_lifecycle_live();
        let published = coordinator
            .events
            .get_mut(&super::ConsumerId(std::num::NonZeroU64::MIN))
            .and_then(super::ConsumerEvents::pop);
        assert!(matches!(
            published,
            Some(super::MarketRuntimeEvent::SeriesUpdate(_))
        ));
        assert!(
            coordinator
                .events
                .get_mut(&super::ConsumerId(
                    std::num::NonZeroU64::new(2).expect("second consumer")
                ))
                .and_then(super::ConsumerEvents::pop)
                .is_none()
        );
    }

    #[test]
    fn offline_coordinator_overflow_replaces_updates_with_covering_snapshot() {
        let profile = super::performance_baseline().expect("offline coordinator baseline");
        assert_eq!(profile["series"], 64);
        assert_eq!(profile["overflow_capacity"], 1024);
        assert_eq!(profile["overflow_events_dropped"], 1024);
        assert!(profile["covering_snapshot_nanos"].as_u64().is_some());
    }
}

/// Drains only work already bounded by provider event lanes before stopping
/// their workers. This preserves a final trade/candle rollover and its
/// accepted provider events without turning shutdown into an unbounded wait for a live feed.
pub(super) fn drain_shutdown_provider_events(coordinator: &mut Coordinator<'_>) {
    let maximum_rounds = REALTIME_CAPACITY.div_ceil(REALTIME_DRAIN_BUDGET).max(1);
    for _ in 0..maximum_rounds {
        if drain_coordinator_events(coordinator) == 0 {
            break;
        }
    }
}

fn study_market_series_live_ready(
    engine: &MarketEngine,
    series_live: &SeriesLifecycles,
    input: &StudyMarketInput,
) -> bool {
    let Some(provider) = engine.provider_status(&input.series.provider_id) else {
        return false;
    };
    if provider.health != ProviderHealth::Online {
        return false;
    }
    let Some(provider_generation) = provider.generation else {
        return false;
    };
    if let Some(live) = series_live.trade_lifecycle(&input.series) {
        live.generation == provider_generation && live.connected && live.history_ready()
    } else {
        series_live.candle(&input.series).is_some_and(|live| {
            live.generation == provider_generation && live.connected && live.history_ready()
        })
    }
}

fn study_live_market_data<'a>(
    engine: &MarketEngine,
    order_books: &'a BTreeMap<(String, String), ProviderOrderBook>,
    series_live: &SeriesLifecycles,
    input: &StudyMarketInput,
) -> Option<StudyLiveMarketData<'a>> {
    if !study_market_series_live_ready(engine, series_live, input) {
        return None;
    }
    let provider = engine.provider_status(&input.series.provider_id)?;
    let provider_generation = provider.generation?;
    let generation = provider_generation.0.get();
    order_books
        .values()
        .find_map(|book| book.study_live_market_data(input, generation))
}

type OrderBookIdentity = (String, String, String);

pub(super) struct Coordinator<'a> {
    pub(super) engine: MarketEngine,
    pub(super) studies: StudyRuntime,
    pub(super) providers: ProviderDispatch<'a>,
    pub(super) clock: Arc<dyn super::clock::Clock>,
    pub(super) calendar: super::calendar::MarketCalendarService,
    pub(super) provider_epochs: ProviderEpochRegistry,
    pub(super) provider_registry: super::provider_session::ProviderRegistry,
    pub(super) attached: BTreeSet<ClientId>,
    pub(super) consumer_clients: BTreeMap<ConsumerId, ClientId>,
    pub(super) consumer_epochs: BTreeMap<ConsumerId, Epoch<ConsumerTag>>,
    consumer_epoch_sequence: EpochSequence<ConsumerTag>,
    pub(super) pending: BTreeMap<BarSeriesKey, Vec<DemandWaiter>>,
    pub(super) history: super::history::tracker::HistoryTracker,
    pub(super) events: BTreeMap<ConsumerId, ConsumerEvents>,
    pub(super) outbox: Arc<super::outbox::ConsumerOutbox>,
    pub(super) series: lifecycle::SeriesLifecycles,
    pub(super) dirty_series: BTreeSet<BarSeriesKey>,
    deadlines: BTreeMap<Instant, TimerKey>,
    pending_activity: bool,
    quote_releases: Option<Arc<Mutex<Vec<MarketDataLeaseId>>>>,
    pub(super) quotes: Arc<Mutex<super::quotes::QuoteBook>>,
    pub(super) trading_feed: Arc<super::trading_feed::TradingFeedOutbox>,
    trading_feed_snapshot_sent: bool,
    trading_interest_revision: u64,
    pub(super) control_retry: bool,
    pub(super) series_effects: super::lifecycle::EffectBuf,
    pub(super) order_books: BTreeMap<(String, String), ProviderOrderBook>,
    /// Provider-aggregated depth used only for DOM presentation.
    /// Canonical `order_books` remain sourced from the standard full-precision
    /// L2 subscription.
    pub(super) display_depth: BTreeMap<(String, String), crate::MarketDisplayDepth>,
    pub(super) display_generations: BTreeMap<String, u64>,
    /// Bounded alert definitions, baselines, and trigger evaluation. Provider
    /// workers remain shared with ordinary market demand.
    pub(super) price_alerts: PriceAlertRegistry,
    pub(super) catalog: BTreeMap<(String, String), InstallProviderInstrument>,
    pub(super) catalog_sessions: BTreeMap<String, u64>,
    /// Latest accepted provider-catalog command generation per consumer/provider.
    /// Provider callbacks are fenced here before entering the canonical consumer
    /// outbox, so desktop presentation code never needs a second pending-command map.
    pub(super) command_fences: super::CommandFences,
    pub(super) pending_selections:
        BTreeMap<ConsumerId, (ClientId, super::SelectionRequest, String)>,
    pub(super) selection_epochs: BTreeMap<ConsumerId, std::collections::BTreeSet<u64>>,
}

impl Coordinator<'_> {
    pub(super) fn signal_pending_consumers(&self) {
        for (consumer, events) in &self.events {
            if events.has_pending()
                && let Some(client) = self.consumer_clients.get(consumer)
                && self.attached.contains(client)
            {
                self.outbox.notify(*client);
            }
        }
    }

    pub(super) fn ensure_provider_connection(&mut self, id: &str) -> bool {
        if !self.provider_registry.contains(id) {
            return true;
        }
        if self.provider_registry.is_failed(id) {
            return false;
        }
        if self.provider_registry.epoch(id).is_some() {
            return true;
        }
        if let Ok(Some(epoch)) = self.provider_registry.connect(id, self.clock.monotonic()) {
            self.begin_reserved_provider_epoch(id, epoch);
            true
        } else {
            self.control_retry = true;
            false
        }
    }

    fn begin_reserved_provider_epoch(
        &mut self,
        id: &str,
        epoch: aeris_market_engine::fence::Epoch<aeris_market_engine::fence::ProviderTag>,
    ) {
        self.quotes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear_provider(id);
        self.provider_epochs.bind(id, epoch);
        if let Some(descriptor) = self.providers.descriptor(id) {
            // The worker's Connecting event is asynchronous. History tickets
            // must not capture the preceding catalog generation meanwhile.
            self.begin_lifecycle_session(id, epoch.get().get(), descriptor.capabilities.live_model);
        }
    }

    fn connect_due_providers(&mut self) {
        let due = self.provider_registry.due_ids(self.clock.monotonic());
        for id in due {
            if !self.has_provider_live_demand(id)
                && !self.provider_registry.catalog_uses_session(id)
                && !self
                    .provider_registry
                    .release_idle(id)
                    .is_ok_and(|stopped| stopped)
            {
                self.control_retry = true;
            }
        }
        for (id, epoch) in self.provider_registry.retry_due(self.clock.monotonic()) {
            self.begin_reserved_provider_epoch(id, epoch);
            self.session_mut(id).demand_dirty = true;
            self.session_mut(id).accepted = None;
        }
    }

    fn publish_failed_provider_controls(&mut self) {
        for (id, epoch, reason) in self.provider_registry.take_failures() {
            self.session_mut(id).demand_dirty = false;
            self.session_mut(id).pending = None;
            self.session_mut(id).authorization = None;
            self.provider_epochs.bind(id, epoch);
            self.mark_lifecycle_failed(id, epoch.get().get(), reason);
        }
    }

    pub(super) fn collect_dirty_series(&mut self) {
        self.dirty_series.clear();
        self.dirty_series
            .extend(self.series.iter().filter_map(|(key, live)| {
                let dirty = live.trade_model().map_or(
                    live.dirty || !live.pending_publications.is_empty(),
                    |model| {
                        model.dirty
                            || !model.pending_publications.is_empty()
                            || !model.pending_corrections.is_empty()
                    },
                );
                dirty.then(|| key.clone())
            }));
    }

    fn take_due_timers(&mut self, now: Instant) -> TimerKey {
        let mut due = TimerKey::default();
        while let Some((&at, _)) = self.deadlines.first_key_value() {
            if at > now {
                break;
            }
            if let Some(timer) = self.deadlines.remove(&at) {
                due.0 |= timer.0;
            }
        }
        due
    }

    fn next_wait(&self) -> Option<Duration> {
        let now = self.clock.monotonic();
        let deadline = self
            .deadlines
            .first_key_value()
            .map(|(at, _)| at.saturating_duration_since(now));
        if self.dirty_series.is_empty() {
            deadline
        } else {
            Some(deadline.map_or(COORDINATOR_TICK, |wait| wait.min(COORDINATOR_TICK)))
        }
    }

    fn refresh_deadlines(&mut self) {
        let now = self.clock.monotonic();
        let wall_ms = self.clock.wall_nanos().map(|at| at.div_euclid(1_000_000));
        let mut deadlines = BTreeMap::<Instant, TimerKey>::new();
        if let Some(at) = self.history.next_retry_deadline() {
            deadlines.entry(at).or_default().0 |= TimerKey::HISTORY_RETRY;
        }
        if let Some(at) = self.provider_registry.next_retry() {
            deadlines.entry(at).or_default().0 |= TimerKey::CONTROL_RETRY;
        }
        for (series, live) in self.series.iter() {
            if !live.connected || !live.history_ready() {
                continue;
            }
            if let Some(at_ms) = wall_ms
                && let Some((transition, _)) =
                    self.calendar.next_transition(&series.instrument_id, at_ms)
                && let Some(delay_ms) = transition.checked_sub(at_ms)
                && let Ok(delay_ms) = u64::try_from(delay_ms)
            {
                deadlines
                    .entry(now + Duration::from_millis(delay_ms))
                    .or_default()
                    .0 |= TimerKey::SESSION_TRANSITION;
            }
            if self
                .engine
                .provider_status(&series.provider_id)
                .is_none_or(|status| {
                    status.generation != Some(live.generation)
                        || status.health != ProviderHealth::Online
                })
            {
                continue;
            }
            let phase = wall_ms.map_or(aeris_instruments::SessionPhase::Unknown, |at| {
                self.calendar.phase(&series.instrument_id, at)
            });
            let threshold = match phase {
                aeris_instruments::SessionPhase::Open
                | aeris_instruments::SessionPhase::Extended
                | aeris_instruments::SessionPhase::AlwaysOpen => {
                    super::watchdog::OPEN_STALL_THRESHOLD
                }
                aeris_instruments::SessionPhase::Unknown => {
                    super::watchdog::UNKNOWN_STALL_THRESHOLD
                }
                _ => continue,
            };
            let last = live.trade_model().map_or_else(
                || live.last_progress(),
                |model| Some(model.last_publication_at),
            );
            if let Some(last) = last {
                deadlines.entry(last + threshold).or_default().0 |= TimerKey::WATCHDOG;
            }
        }
        if self.trading_feed.attached() {
            for (key, _) in self.engine.instrument_subscriptions() {
                if let Some(at_ms) = wall_ms
                    && let Some((transition, _)) = self
                        .calendar
                        .next_transition(key.instrument.as_str(), at_ms)
                    && let Some(delay_ms) = transition.checked_sub(at_ms)
                    && let Ok(delay_ms) = u64::try_from(delay_ms)
                {
                    deadlines
                        .entry(now + Duration::from_millis(delay_ms))
                        .or_default()
                        .0 |= TimerKey::SESSION_TRANSITION;
                }
            }
        }
        for book in self.order_books.values() {
            if book.trade_tape_dirty {
                let at = book
                    .trade_tape_published_at
                    .map_or(now, |last| last + COORDINATOR_TICK);
                deadlines.entry(at).or_default().0 |= TimerKey::TRADE_TAPE;
            }
        }
        if self.control_retry
            || self.provider_registry.controls().any(|session| {
                session.pending.is_some() || session.demand_dirty || session.authorization.is_some()
            })
        {
            deadlines
                .entry(now + Duration::from_millis(100))
                .or_default()
                .0 |= TimerKey::CONTROL_RETRY;
        }
        self.deadlines = deadlines;
    }

    pub(super) fn session(&self, provider: &str) -> &super::ProviderControlState {
        self.provider_registry.control(provider)
    }

    pub(super) fn session_mut(&mut self, provider: &str) -> &mut super::ProviderControlState {
        self.provider_registry.control_mut(provider)
    }

    pub(super) fn detach_client(&mut self, client_id: ClientId) {
        self.outbox.retire(client_id);
        let mut removed_consumer = false;
        let mut removed_study = false;
        for consumer_id in self.engine.detach_client(client_id) {
            removed_consumer = true;
            removed_study |= super::consumer_registry::ConsumerRegistry::new(self)
                .remove_consumer(consumer_id, false);
        }
        if removed_study && let Err(error) = self.reconcile_study_market_data() {
            diagnostic!("Aeris study cleanup failed during client detach: {error}");
        }
        if removed_consumer {
            self.mark_alert_demand_changed();
            self.reconcile_order_books();
        }
    }

    pub(super) fn begin_shutdown(&mut self) {
        self.publish_lifecycle_live();
        self.history.cancel_all();
        for provider in self.providers.records.keys().copied().collect::<Vec<_>>() {
            let _ = self.provider_registry.release_idle(provider);
        }
    }

    pub(super) fn handle_command(&mut self, command: Command) {
        match command {
            Command::Request(command, state) => {
                if state.start() {
                    self.handle_command(*command);
                }
            }
            Command::ProviderWake => {
                if let Some(wake) = self.providers.wake {
                    wake.pending.store(false, Ordering::Release);
                }
            }
            Command::HistoryCompleted(series, generation, range, result, ticket) => {
                if !self
                    .provider_epochs
                    .accepts(&series.provider_id, ticket.provider)
                    || self
                        .engine
                        .provider_status(&series.provider_id)
                        .and_then(|status| status.generation)
                        != Some(generation)
                    || ticket.series != series
                {
                    self.provider_epochs.drop_history();
                } else {
                    self.history_completed(&ticket, generation, range, result);
                }
            }
            Command::SelectAndDemand(client_id, request, reply) => {
                let _ = reply.send(self.begin_selection(client_id, request));
            }
            #[cfg(feature = "probe")]
            command @ Command::ProbeFailWebSocket(..) => self.handle_service_command(command),
            command @ (Command::BrokerAuthorizationChanged(..)
            | Command::AvailableStreams(..)
            | Command::InstalledInstrument(..)
            | Command::Status(..)
            | Command::Attach(..)
            | Command::Detach(..)
            | Command::DrainClient(..)) => self.handle_service_command(command),
            command => self.handle_consumer_command(command),
        }
    }

    pub(super) fn handle_service_command(&mut self, command: Command) {
        match command {
            Command::BrokerAuthorizationChanged(provider, ready, reply) => {
                let Some(descriptor) = self.providers.descriptor(provider).filter(|descriptor| {
                    descriptor.capabilities.connection_kind
                        == super::ProviderConnectionKind::BrokerCapability
                }) else {
                    let _ = reply.send(Err("Broker market-data provider is unavailable".into()));
                    return;
                };
                self.session_mut(provider).suspended = !ready;
                self.session_mut(provider).authorization = Some(ready);
                if !ready
                    && let Some(generation) = self
                        .engine
                        .provider_status(provider)
                        .and_then(|s| s.generation)
                {
                    self.session_mut(provider).generation_floor =
                        generation.0.get().saturating_add(1);
                    self.mark_lifecycle_recovering(
                        provider,
                        generation.0.get(),
                        descriptor.capabilities.live_model,
                        descriptor.capabilities.authorization_revoked_detail,
                    );
                    let _ = self.engine.end_provider_session(provider, generation);
                }
                self.flush_session_managed_demand(provider);
                self.broadcast_provider_for(
                    provider,
                    if ready {
                        None
                    } else {
                        Some(descriptor.capabilities.authorization_revoked_detail)
                    },
                );
                let _ = reply.send(Ok(()));
            }
            Command::AvailableStreams(provider, requested, reply) => {
                let mut supported = StreamRequirements::NONE;
                for stream in [
                    MarketStream::Bars,
                    MarketStream::Trades,
                    MarketStream::Quotes,
                    MarketStream::Depth,
                ] {
                    if requested.contains(stream)
                        && self
                            .engine
                            .verify_provider_stream_requirements(
                                &provider,
                                StreamRequirements::NONE.with(stream),
                            )
                            .is_ok()
                    {
                        supported = supported.with(stream);
                    }
                }
                let _ = reply.send(Ok(supported));
            }
            Command::Status(reply) => {
                let _ = reply.send(Ok(self.status()));
            }
            #[cfg(feature = "probe")]
            Command::ProbeFailWebSocket(count, reply) => {
                let result = match self
                    .provider_registry
                    .probe_fail_websocket(super::HYPERLIQUID_DESCRIPTOR.id, count)
                {
                    Ok(true) => Ok(()),
                    _ => Err("WebSocket probe control unavailable".to_string()),
                };
                let _ = reply.send(result);
            }
            Command::InstalledInstrument(provider, instrument, reply) => {
                let installed = self.catalog.get(&(provider, instrument)).cloned();
                let _ = reply.send(Ok(installed));
            }
            Command::Attach(client_id, reply) => {
                self.handle_attach(client_id, &reply);
            }
            Command::Detach(client_id, reply) => {
                self.attached.remove(&client_id);
                self.detach_client(client_id);
                self.release_unused_live_market_data();
                let _ = reply.send(Ok(()));
            }
            Command::DrainClient(client_id, consumer_budgets, reply) => {
                self.handle_drain_client(client_id, consumer_budgets, &reply);
            }
            _ => unreachable!("only service commands reach service dispatch"),
        }
    }

    pub(super) fn handle_consumer_command(&mut self, command: Command) {
        match command {
            Command::Register(identity, reply) => {
                let _ = reply.send(self.handle_register(identity));
            }
            Command::Remove(client_id, consumer_id, reply) => {
                let _ = reply.send(self.handle_remove(client_id, consumer_id));
            }
            Command::Viewport(client_id, consumer_id, generation, viewport, reply) => {
                let result =
                    authorize_consumer(&self.engine, client_id, consumer_id).and_then(|()| {
                        match self.engine.set_viewport(consumer_id, generation, viewport) {
                            Ok(()) => {
                                self.request_viewport_history(consumer_id, generation, viewport)
                            }
                            Err(EngineError::StaleConsumerGeneration { .. }) => Ok(()),
                            Err(error) => Err(error.to_string()),
                        }
                    });
                let _ = reply.send(result);
            }
            Command::ResourceClass(client_id, consumer_id, resource_class, reply) => {
                self.handle_resource_class(client_id, consumer_id, resource_class, &reply);
            }
            Command::Streams(client_id, consumer_id, generation, streams, reply) => {
                self.handle_streams(client_id, consumer_id, generation, streams, &reply);
            }
            Command::Demand(client_id, consumer_id, generation, series, streams, reply) => {
                self.handle_demand(
                    client_id,
                    &series,
                    streams,
                    DemandWaiter {
                        consumer_id,
                        generation,
                        started_at: Instant::now(),
                    },
                    &reply,
                );
            }
            Command::RegisterStudy(client_id, consumer_id, registration, reply) => {
                let result = self.handle_register_study(client_id, consumer_id, registration);
                let _ = reply.send(result);
            }
            Command::ReinitializeStudy(client_id, study_id, registration, reply) => {
                let result = self.handle_reinitialize_study(client_id, study_id, registration);
                let _ = reply.send(result);
            }
            Command::RemoveStudy(client_id, study_id, reply) => {
                let result = self.handle_remove_study(client_id, study_id);
                let _ = reply.send(result);
            }
            Command::SearchProviderInstruments(client_id, search, reply) => {
                self.handle_provider_search(client_id, search, &reply);
            }
            Command::SelectProviderInstrument(client_id, selection, reply) => {
                self.handle_provider_selection(client_id, selection, &reply);
            }
            Command::ScreenProviderMarkets(client_id, screen, reply) => {
                self.handle_provider_screen(client_id, screen, &reply);
            }
            Command::InstallProviderInstrument(instrument, reply) => {
                let _ = reply.send(self.install_provider_instrument(&instrument));
            }
            Command::ReplacePriceAlerts(client_id, consumer_id, alerts, reply) => {
                let result = authorize_consumer(&self.engine, client_id, consumer_id)
                    .and_then(|()| self.replace_price_alerts(consumer_id, alerts));
                if result.is_ok() {
                    self.mark_alert_demand_changed();
                    self.release_idle();
                }
                let _ = reply.send(result);
            }
            Command::AcquireQuote(key, reply) => {
                self.acquire_quote(&key, &reply);
            }
            Command::SetTradingInterest(interest, reply) => {
                let _ = reply.send(self.install_trading_interest(&interest));
            }
            Command::Drain(client_id, consumer_id, reply) => {
                self.handle_drain(client_id, consumer_id, &reply);
            }
            #[cfg(feature = "probe")]
            Command::ProbeFailWebSocket(..) => unreachable!("probe control is a service command"),
            Command::Request(..)
            | Command::SelectAndDemand(..)
            | Command::HistoryCompleted(..)
            | Command::ProviderWake
            | Command::Status(..)
            | Command::Attach(..)
            | Command::Detach(..)
            | Command::DrainClient(..)
            | Command::AvailableStreams(..)
            | Command::InstalledInstrument(..)
            | Command::BrokerAuthorizationChanged(..) => {
                unreachable!("command was routed to the wrong dispatcher")
            }
        }
    }

    fn acquire_quote(
        &mut self,
        key: &super::InstrumentStreamKey,
        reply: &Reply<MarketDataLeaseId>,
    ) {
        match self
            .engine
            .acquire_instrument_lease(key, StreamRequirements::NONE.with(MarketStream::Quotes))
        {
            Ok(id) => {
                self.reconcile_order_books();
                self.mark_alert_demand_changed();
                self.pending_activity = true;
                self.publish_trading_session(key);
                if reply.send(Ok(id)).is_err() {
                    let _ = self.engine.release_data_lease(id);
                    self.reconcile_order_books();
                }
            }
            Err(error) => {
                let _ = reply.send(Err(error.to_string()));
            }
        }
    }

    fn install_trading_interest(
        &mut self,
        interest: &aeris_trading::market::QuoteInterest,
    ) -> Result<(), String> {
        interest.validate().map_err(|error| error.to_string())?;
        if interest.revision <= self.trading_interest_revision {
            return Err("trading quote interest revision is stale".into());
        }
        let keys = interest
            .routes
            .iter()
            .map(|route| super::InstrumentStreamKey {
                provider: route.provider_id.clone(),
                instrument: route.instrument_id.clone(),
                entitlement: route.entitlement_id.clone(),
            })
            .collect::<BTreeSet<_>>();
        self.engine
            .set_instrument_leases(
                aeris_market_engine::LeaseOwner::Trading,
                keys,
                StreamRequirements::NONE.with(MarketStream::Quotes),
            )
            .map_err(|error| error.to_string())?;
        self.trading_interest_revision = interest.revision;
        self.reconcile_order_books();
        for route in &interest.routes {
            self.publish_trading_session(&super::InstrumentStreamKey {
                provider: route.provider_id.clone(),
                instrument: route.instrument_id.clone(),
                entitlement: route.entitlement_id.clone(),
            });
        }
        self.mark_alert_demand_changed();
        self.pending_activity = true;
        Ok(())
    }

    pub(super) fn publish_trading_session(&self, key: &super::InstrumentStreamKey) {
        use aeris_instruments::CalendarSourceKind;
        use aeris_trading::market::{InstrumentSession, MarketFeedEvent};

        if !self.trading_feed.attached() {
            return;
        }
        let Some(at) = self.clock.wall_nanos() else {
            return;
        };
        let status = self
            .calendar
            .status(key.instrument.as_str(), at.div_euclid(1_000_000));
        let source = match status.source {
            aeris_contracts::MarketSessionSource::ProviderCalendar => {
                CalendarSourceKind::ProviderCalendar
            }
            aeris_contracts::MarketSessionSource::ProviderHours => {
                CalendarSourceKind::ContractHours
            }
            aeris_contracts::MarketSessionSource::Continuous => CalendarSourceKind::AlwaysOpen,
            aeris_contracts::MarketSessionSource::Unavailable => CalendarSourceKind::Unknown,
        };
        let session = InstrumentSession {
            instrument_id: key.instrument.clone(),
            phase: self
                .calendar
                .phase(key.instrument.as_str(), at.div_euclid(1_000_000)),
            source,
            session_end_unix_nanos: status.session_end_unix_nanos,
            next_open_unix_nanos: status.next_open_unix_nanos,
        };
        self.trading_feed.publish(MarketFeedEvent::Session(session));
    }

    fn publish_trading_feed_snapshot(&self) {
        for instrument in self.catalog.values() {
            if let Ok(terms) = super::instrument_terms_from_install(instrument) {
                self.trading_feed
                    .publish(aeris_trading::market::MarketFeedEvent::Terms(terms));
            }
        }
        for (key, _) in self.engine.instrument_subscriptions() {
            self.publish_trading_session(&key);
            if let Some(generation) = self
                .engine
                .provider_status(&key.provider)
                .and_then(|status| status.generation)
            {
                let state = if self
                    .engine
                    .provider_status(&key.provider)
                    .is_some_and(|status| status.health == ProviderHealth::Online)
                {
                    aeris_trading::market::FeedState::Live
                } else {
                    aeris_trading::market::FeedState::Stale {
                        reason: aeris_trading::market::FeedReason::Disconnected,
                    }
                };
                self.trading_feed
                    .publish(aeris_trading::market::MarketFeedEvent::Feed {
                        instrument_id: key.instrument.clone(),
                        state,
                        provider_generation: generation.0.get(),
                    });
            }
        }
    }

    pub(super) fn publish_provider_feed(
        &self,
        provider: &str,
        generation: u64,
        state: aeris_trading::market::FeedState,
    ) {
        if !self.trading_feed.attached() {
            return;
        }
        for (key, _) in self.engine.instrument_subscriptions() {
            if key.provider == provider {
                self.trading_feed
                    .publish(aeris_trading::market::MarketFeedEvent::Feed {
                        instrument_id: key.instrument.clone(),
                        state,
                        provider_generation: generation,
                    });
            }
        }
    }

    fn replace_price_alerts(
        &mut self,
        consumer_id: ConsumerId,
        alerts: Vec<crate::MarketPriceAlert>,
    ) -> Result<(), String> {
        let mut replacement = self.price_alerts.clone();
        replacement.replace_consumer_alerts(consumer_id, alerts)?;
        let keys = replacement.active_keys()?;
        self.engine
            .set_instrument_leases(
                aeris_market_engine::LeaseOwner::PriceAlerts,
                keys,
                StreamRequirements::NONE
                    .with(MarketStream::Trades)
                    .with(MarketStream::Quotes),
            )
            .map_err(|error| error.to_string())?;
        self.price_alerts = replacement;
        self.reconcile_order_books();
        self.pending_activity = true;
        Ok(())
    }

    pub(super) fn reconcile_price_alert_leases(&mut self) -> Result<(), String> {
        self.engine
            .set_instrument_leases(
                aeris_market_engine::LeaseOwner::PriceAlerts,
                self.price_alerts.active_keys()?,
                StreamRequirements::NONE
                    .with(MarketStream::Trades)
                    .with(MarketStream::Quotes),
            )
            .map_err(|error| error.to_string())?;
        self.reconcile_order_books();
        self.mark_alert_demand_changed();
        self.pending_activity = true;
        Ok(())
    }

    fn handle_resource_class(
        &mut self,
        client_id: ClientId,
        consumer_id: ConsumerId,
        resource_class: ConsumerResourceClass,
        reply: &Reply<()>,
    ) {
        let result = authorize_consumer(&self.engine, client_id, consumer_id).and_then(|()| {
            let mut needs_covering_repair = false;
            let retained_before = self
                .engine
                .current_demand(consumer_id)
                .is_some_and(|demand| demand.resource_class.retains_subscription());
            let publication = self
                .engine
                .set_resource_class(consumer_id, resource_class)
                .map_err(|error| error.to_string())?;
            if !retained_before
                && resource_class.retains_subscription()
                && let Some((series, streams)) = self
                    .engine
                    .current_demand(consumer_id)
                    .and_then(|demand| demand.series.clone().zip(demand.streams))
            {
                // Background consumers intentionally release provider demand and
                // `release_unused_live_market_data` prunes their live handoff.
                // When the same consumer returns to Foreground the engine restores
                // its subscription from retained demand; rebuild the runtime handoff
                // in the same transition so cached publication cannot leave the
                // chart detached from upstream live data.
                self.ensure_realtime(&series)?;
                if streams.contains(MarketStream::Bars) {
                    let provider_generation = self.provider_generation_for_series(&series)?;
                    if let Some(snapshot) = publication
                        .as_ref()
                        .map(|publication| Arc::clone(&publication.snapshot))
                        .or_else(|| self.engine.series_snapshot(&series))
                    {
                        needs_covering_repair =
                            self.prepare_cached_demand(&series, provider_generation, &snapshot)?;
                    } else if self
                        .engine
                        .provider_status(&series.provider_id)
                        .is_some_and(|status| status.health != ProviderHealth::Disconnected)
                    {
                        self.enqueue_history_recovery(&series, provider_generation)
                            .map_err(str::to_string)?;
                    }
                }
            }
            if let Some(publication) = publication {
                if needs_covering_repair {
                    let (state, detail) = self.covering_repair_state(&publication.snapshot.series);
                    if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                        publish_state(events, &publication, state, Some(detail));
                    }
                } else {
                    self.publish_current_snapshot(&publication);
                }
            }
            if let Some(events) = self.events.get_mut(&consumer_id)
                && !resource_class.publishes_ui()
            {
                // A full snapshot queued just before a pane hid must not leak
                // through after the class change. Canonical depth remains
                // runtime-owned and is republished on Foreground restore.
                events.order_book = None;
                events.trade_tape = None;
                events.study_outputs.clear();
            }
            Ok(())
        });
        self.reconcile_order_books();
        if resource_class.publishes_ui() {
            self.publish_order_book_to_consumer(consumer_id);
            self.publish_trade_tape_to_consumer(consumer_id);
            let studies = self.studies.owned_studies(consumer_id);
            self.publish_study_outputs(&studies);
        }
        self.release_unused_live_market_data();
        let _ = reply.send(result);
    }

    fn handle_streams(
        &mut self,
        client_id: ClientId,
        consumer_id: ConsumerId,
        generation: GenerationId,
        streams: StreamRequirements,
        reply: &Reply<()>,
    ) {
        let result = authorize_consumer(&self.engine, client_id, consumer_id).and_then(|()| {
            let changed =
                match self
                    .engine
                    .set_stream_requirements(consumer_id, generation, streams)
                {
                    Ok(changed) => changed,
                    Err(EngineError::StaleConsumerGeneration { .. }) => return Ok(()),
                    Err(error) => return Err(error.to_string()),
                };
            if !changed {
                return Ok(());
            }
            let series = self
                .engine
                .current_demand(consumer_id)
                .and_then(|demand| demand.series.clone())
                .ok_or_else(|| "market consumer has no selected series".to_string())?;
            self.reconcile_order_books();
            if !streams.contains(MarketStream::Depth)
                && let Some(events) = self.events.get_mut(&consumer_id)
            {
                events.order_book = None;
            }
            if !streams.contains(MarketStream::Trades)
                && let Some(events) = self.events.get_mut(&consumer_id)
            {
                events.trade_tape = None;
            }
            self.ensure_realtime(&series)?;
            if streams.contains(MarketStream::Depth) {
                self.publish_order_book_to_consumer(consumer_id);
            }
            if streams.contains(MarketStream::Trades) {
                self.publish_trade_tape_to_consumer(consumer_id);
            }
            Ok(())
        });
        self.release_unused_live_market_data();
        let _ = reply.send(result);
    }

    pub(super) fn handle_register(&mut self, identity: ConsumerIdentity) -> Result<(), String> {
        if !self.attached.contains(&identity.client_id) {
            return Err("client must attach before registering consumers".to_string());
        }
        self.engine
            .register_consumer(identity, true)
            .map_err(|error| error.to_string())?;
        self.events
            .insert(identity.consumer_id, ConsumerEvents::default());
        self.consumer_clients
            .insert(identity.consumer_id, identity.client_id);
        self.consumer_epochs
            .insert(identity.consumer_id, self.consumer_epoch_sequence.next());
        Ok(())
    }

    pub(super) fn handle_remove(
        &mut self,
        client_id: ClientId,
        consumer_id: ConsumerId,
    ) -> Result<(), String> {
        authorize_consumer(&self.engine, client_id, consumer_id)?;
        let removed_studies = super::consumer_registry::ConsumerRegistry::new(self)
            .remove_consumer(consumer_id, true);
        if removed_studies {
            self.reconcile_study_market_data()?;
        }
        self.mark_alert_demand_changed();
        self.reconcile_order_books();
        for session in self.provider_registry.controls_mut() {
            session.demand_dirty = true;
        }
        self.release_unused_live_market_data();
        Ok(())
    }

    fn handle_register_study(
        &mut self,
        client_id: ClientId,
        consumer_id: ConsumerId,
        registration: super::NativeStudyRegistration,
    ) -> Result<super::StudyInstanceId, String> {
        authorize_consumer(&self.engine, client_id, consumer_id)?;
        let study_id = self
            .studies
            .register_native_for_consumer(consumer_id, registration)
            .map_err(|error| error.to_string())?;
        let prepared = self.reconcile_study_market_data().and_then(|()| {
            self.execute_study_change(StudyChange::Ready(study_id))
                .map(|batch| !batch.executed.is_empty())
                .map_err(|error| error.to_string())
        });
        match prepared {
            Ok(executed) => {
                if executed {
                    self.publish_study_outputs(&[study_id]);
                }
            }
            Err(error) => {
                let _ = self.studies.remove_subtree(study_id);
                let _ = self.reconcile_study_market_data();
                return Err(error);
            }
        }
        Ok(study_id)
    }

    fn handle_remove_study(
        &mut self,
        client_id: ClientId,
        study_id: super::StudyInstanceId,
    ) -> Result<Vec<super::StudyInstanceId>, String> {
        let owner = self
            .studies
            .owner(study_id)
            .ok_or_else(|| "study instance is unavailable".to_string())?;
        authorize_consumer(&self.engine, client_id, owner)?;
        let removed = self
            .studies
            .remove_subtree(study_id)
            .map_err(|error| error.to_string())?;
        if let Some(events) = self.events.get_mut(&owner) {
            events.publish_study_removed(
                owner,
                self.engine
                    .current_demand(owner)
                    .and_then(|demand| demand.generation),
                &removed,
            );
        }
        self.reconcile_study_market_data()?;
        Ok(removed)
    }

    fn handle_reinitialize_study(
        &mut self,
        client_id: ClientId,
        study_id: super::StudyInstanceId,
        registration: super::NativeStudyRegistration,
    ) -> Result<Vec<super::StudyInstanceId>, String> {
        let owner = self
            .studies
            .owner(study_id)
            .ok_or_else(|| "study instance is unavailable".to_string())?;
        authorize_consumer(&self.engine, client_id, owner)?;
        let checkpoint = self
            .studies
            .checkpoint_subtree(study_id)
            .map_err(|error| error.to_string())?;
        let affected = self
            .studies
            .reinitialize_native_for_consumer(owner, study_id, registration)
            .map_err(|error| error.to_string())?;

        let prepared = self.reconcile_study_market_data().and_then(|()| {
            self.execute_study_change(StudyChange::ReadySubtree(study_id))
                .map(|batch| batch.executed)
                .map_err(|error| error.to_string())
        });
        match prepared {
            Ok(executed) => {
                if let Some(events) = self.events.get_mut(&owner) {
                    events.publish_study_invalidated(
                        owner,
                        self.engine
                            .current_demand(owner)
                            .and_then(|demand| demand.generation),
                        &affected,
                    );
                }
                self.publish_study_outputs(&executed);
                Ok(affected)
            }
            Err(error) => {
                self.studies.restore_subtree_checkpoint(checkpoint);
                if let Err(rollback_error) = self.reconcile_study_market_data() {
                    diagnostic!("Aeris study reinitialization rollback failed: {rollback_error}");
                }
                Err(error)
            }
        }
    }

    pub(super) fn publish_study_outputs(&mut self, study_ids: &[super::StudyInstanceId]) {
        for &study_id in study_ids {
            let Some(owner) = self.studies.owner(study_id) else {
                continue;
            };
            if !self
                .engine
                .current_demand(owner)
                .is_some_and(|demand| demand.resource_class.publishes_ui())
            {
                continue;
            }
            let Some(definition) = self.studies.definition(study_id) else {
                continue;
            };
            let study_identifier = definition.identifier.clone();
            let outputs = definition.outputs.clone();
            for (output_index, output) in outputs.into_iter().enumerate() {
                let output_id = super::StudyOutputId {
                    study_id,
                    output_index,
                };
                let Some(series) = self.studies.output_series(output_id).cloned() else {
                    continue;
                };
                if let Some(events) = self.events.get_mut(&owner) {
                    events.publish_study_output(MarketRuntimeEvent::StudyOutputSnapshot(
                        MarketStudyOutputSnapshot {
                            consumer_id: owner,
                            generation: self
                                .engine
                                .current_demand(owner)
                                .and_then(|demand| demand.generation),
                            study_id,
                            output_id,
                            study_identifier: study_identifier.clone(),
                            output,
                            stream_requirements: self
                                .studies
                                .input_stream_requirements(study_id)
                                .unwrap_or(StreamRequirements::NONE),
                            series,
                        },
                    ));
                }
            }
        }
    }

    pub(super) fn execute_study_change(
        &mut self,
        change: StudyChange<'_>,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError> {
        let engine = &self.engine;
        let order_books = &self.order_books;
        let series_live = &self.series;
        let studies = &mut self.studies;
        let mut live_market = |input: &StudyMarketInput| {
            study_live_market_data(engine, order_books, series_live, input)
        };
        match change {
            StudyChange::Ready(study_id) => {
                let executed =
                    studies.execute_ready_with_live(engine, study_id, &mut live_market)?;
                Ok(StudyExecutionBatch {
                    executed: if executed { vec![study_id] } else { Vec::new() },
                    errors: Vec::new(),
                })
            }
            StudyChange::ReadySubtree(study_id) => Ok(StudyExecutionBatch {
                executed: studies.execute_ready_subtree_with_live(
                    engine,
                    study_id,
                    &mut live_market,
                )?,
                errors: Vec::new(),
            }),
            StudyChange::Market(series) => {
                studies.execute_ready_for_market_with_live(engine, series, &mut live_market)
            }
            StudyChange::Bar(series, timestamp) => studies.execute_live_market_change_with_live(
                engine,
                series,
                timestamp,
                &mut live_market,
            ),
            StudyChange::History(series, first, last) => studies
                .execute_history_range_change_with_live(
                    engine,
                    series,
                    first,
                    last,
                    &mut live_market,
                ),
            StudyChange::NonBar(change) => {
                let mut market_ready = |input: &StudyMarketInput| {
                    study_market_series_live_ready(engine, series_live, input)
                };
                studies.execute_live_non_bar_change_with_live(
                    engine,
                    change,
                    &mut live_market,
                    &mut market_ready,
                )
            }
        }
    }

    fn reconcile_study_market_data(&mut self) -> Result<(), String> {
        let changes = self
            .studies
            .reconcile_market_leases(&mut self.engine)
            .map_err(|error| error.to_string())?;
        self.reconcile_order_books();

        for change in &changes {
            match change.kind {
                StudyMarketLeaseChangeKind::Acquired => {
                    self.prepare_study_market_series(&change.series, change.streams)?;
                }
                StudyMarketLeaseChangeKind::Updated => {
                    self.ensure_realtime(&change.series)?;
                }
                StudyMarketLeaseChangeKind::Released => {}
            }
        }
        self.release_unused_live_market_data();
        Ok(())
    }

    fn prepare_study_market_series(
        &mut self,
        series: &BarSeriesKey,
        streams: StreamRequirements,
    ) -> Result<(), String> {
        if !self.ensure_provider_connection(&series.provider_id) {
            return Err("provider connection is not ready".into());
        }
        let provider_generation = self.provider_generation_for_series(series)?;
        self.ensure_realtime(series)?;
        if !streams.contains(MarketStream::Bars) {
            return Ok(());
        }
        if let Some(snapshot) = self.engine.series_snapshot(series) {
            self.prepare_cached_demand(series, provider_generation, &snapshot)?;
            return Ok(());
        }
        self.enqueue_history_recovery(series, provider_generation)
            .map_err(str::to_string)
    }

    pub(super) fn reconcile_order_books(&mut self) {
        let (required_identities, depth_identities) = self.required_order_book_identities();
        let mut retained_keys = self
            .engine
            .instrument_subscriptions()
            .map(|(key, _)| key)
            .collect::<BTreeSet<_>>();
        if self.trading_feed.attached() {
            for (series, status) in self.engine.subscriptions() {
                if status.streams.contains(MarketStream::Quotes)
                    && let Ok(instrument) =
                        aeris_instruments::InstrumentId::try_new(series.instrument_id)
                {
                    retained_keys.insert(super::InstrumentStreamKey {
                        provider: series.provider_id,
                        instrument,
                        entitlement: Some(series.entitlement_id),
                    });
                }
            }
        }
        self.quotes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(&retained_keys);
        let required = self
            .catalog
            .values()
            .filter(|instrument| {
                required_identities.contains(&(
                    instrument.provider.clone(),
                    instrument.instrument_id.clone(),
                    instrument.entitlement_id.clone(),
                ))
            })
            .cloned()
            .collect::<Vec<_>>();
        self.order_books.retain(|identity, book| {
            required.iter().any(|instrument| {
                (
                    instrument.provider.clone(),
                    instrument.instrument_id.clone(),
                ) == *identity
                    && instrument.entitlement_id == book.instrument.entitlement_id
                    && instrument.price_scale == book.instrument.price_scale
                    && instrument.quantity_scale == book.instrument.quantity_scale
                    && instrument.provider_symbol == book.instrument.provider_symbol
            })
        });
        for instrument in required {
            let identity = (
                instrument.provider.clone(),
                instrument.instrument_id.clone(),
            );
            let depth_subscribed = depth_identities.contains(&(
                instrument.provider.clone(),
                instrument.instrument_id.clone(),
                instrument.entitlement_id.clone(),
            ));
            if let Some(book) = self.order_books.get_mut(&identity) {
                // Catalog selection counters describe consumers, not a new book.
                // Keep canonical revisions monotonic across metadata refreshes;
                // transport generations are fenced by provider_depth itself.
                book.update_instrument(instrument);
                book.calendar = self.calendar.for_instrument(&book.instrument.instrument_id);
                book.set_depth_subscribed(depth_subscribed);
            } else {
                let Some(descriptor) = self.providers.descriptor(&instrument.provider) else {
                    diagnostic!("Aeris market instrument references an unregistered provider");
                    continue;
                };
                let mut book = ProviderOrderBook::new(
                    instrument,
                    descriptor.capabilities.trade_continuity,
                    descriptor.capabilities.depth.is_none(),
                );
                book.calendar = self.calendar.for_instrument(&book.instrument.instrument_id);
                book.set_depth_subscribed(depth_subscribed);
                self.order_books.insert(identity, book);
            }
        }
        // Like the canonical image, a display image stops being current once nothing
        // subscribes to depth; keeping it would republish a stale book on resubscription.
        self.display_depth.retain(|identity, display| {
            self.engine
                .provider_status(&identity.0)
                .and_then(|status| status.generation)
                .is_some_and(|generation| generation.0.get() == display.provider_generation)
                && self
                    .order_books
                    .get(identity)
                    .is_some_and(|book| !book.publishes_top_of_book_only())
        });
    }

    fn required_order_book_identities(
        &self,
    ) -> (BTreeSet<OrderBookIdentity>, BTreeSet<OrderBookIdentity>) {
        let mut required_identities = BTreeSet::new();
        let mut depth_identities = BTreeSet::new();
        for (series, subscription) in self.engine.subscriptions() {
            let identity = (
                series.provider_id.clone(),
                series.instrument_id.clone(),
                series.entitlement_id.clone(),
            );
            if subscription.streams.contains(MarketStream::Depth) {
                depth_identities.insert(identity.clone());
            }
            if subscription.streams.contains(MarketStream::Depth)
                || subscription.streams.contains(MarketStream::Trades)
                || subscription.streams.contains(MarketStream::Quotes)
            {
                required_identities.insert(identity);
            }
        }
        for (key, streams) in self.engine.instrument_subscriptions() {
            let identity = (
                key.provider,
                key.instrument.as_str().to_string(),
                key.entitlement.unwrap_or_default(),
            );
            if streams.contains(MarketStream::Depth) {
                depth_identities.insert(identity.clone());
            }
            if streams.contains(MarketStream::Quotes)
                || streams.contains(MarketStream::Depth)
                || streams.contains(MarketStream::Trades)
            {
                required_identities.insert(identity);
            }
        }
        (required_identities, depth_identities)
    }

    pub(super) fn status(&self) -> MarketServiceStatus {
        let metrics = self.engine.metrics();
        MarketServiceStatus {
            connected_desktop_clients: self.attached.len(),
            study_execution_failed: self.studies.execution_failed(),
            providers: self
                .providers
                .records
                .keys()
                .copied()
                .filter_map(|provider| self.provider_state(provider))
                .collect(),
            retained_series: metrics.stored_series,
            retained_bars: metrics.stored_bars,
            approximate_series_bytes: metrics.approximate_series_bytes,
            #[cfg(feature = "probe")]
            probe: {
                let (stale_events, stale_history) = self.provider_epochs.stale_counts();
                super::ProbeStatus {
                    hyperliquid_sessions: self
                        .provider_registry
                        .successful_connections(super::HYPERLIQUID_DESCRIPTOR.id),
                    stale_events,
                    stale_history,
                    maximum_queued_series_updates: self
                        .events
                        .values()
                        .map(|events| events.maximum_series_depth)
                        .max()
                        .unwrap_or(0),
                    retry_attempts: self.provider_registry.probe_retries(self.clock.monotonic()),
                }
            },
        }
    }

    pub(super) fn evict_unreferenced_series(&mut self) -> Vec<BarSeriesKey> {
        let protected = self.engine.demanded_series();
        self.engine.evict_unsubscribed_series(0, 0, &protected)
    }

    pub(super) fn provider_state(&self, provider: &str) -> Option<ProviderState> {
        let status = self.engine.provider_status(provider)?;
        let state = match status.health {
            ProviderHealth::Disconnected => ProviderConnectionState::Disconnected,
            ProviderHealth::Connecting => ProviderConnectionState::Connecting,
            ProviderHealth::Online => ProviderConnectionState::Online,
            ProviderHealth::Recovering => ProviderConnectionState::Recovering,
            ProviderHealth::Failed => ProviderConnectionState::Failed,
        };
        Some(ProviderState {
            provider: provider.to_string(),
            state,
            generation: status.generation.map_or(0, |generation| generation.0.get()),
            detail: self.providers.detail(provider),
            transport_rtt_nanos: self.providers.transport_rtt_nanos(provider),
        })
    }

    pub(super) fn accept_series_demand(
        &mut self,
        client_id: ClientId,
        series: &BarSeriesKey,
        streams: StreamRequirements,
        waiter: &DemandWaiter,
    ) -> Result<
        (
            ProviderGeneration,
            Option<aeris_market_engine::ConsumerPublication>,
        ),
        String,
    > {
        authorize_consumer(&self.engine, client_id, waiter.consumer_id)?;
        if !self.ensure_provider_connection(&series.provider_id) {
            return Err("provider connection is not ready".into());
        }
        let provider_generation = self.provider_generation_for_series(series)?;
        if matches!(
            series.period,
            aeris_market_data::BarPeriod::Week { .. } | aeris_market_data::BarPeriod::Month { .. }
        ) && self
            .catalog
            .get(&(series.provider_id.clone(), series.instrument_id.clone()))
            .is_some_and(|instrument| {
                self.calendar.source_kind(&instrument.instrument_id)
                    == aeris_instruments::CalendarSourceKind::Unknown
            })
        {
            let error = "weekly or monthly live series requires a supported instrument calendar";
            self.report_unsupported_calendar(waiter, series, error);
            return Err(error.into());
        }
        let publication = self
            .engine
            .set_series_demand_with_streams(waiter.consumer_id, waiter.generation, series, streams)
            .map_err(|error| error.to_string())?;
        self.reconcile_order_books();
        self.remove_waiter(waiter.consumer_id);
        let provider_state = self
            .provider_state(&series.provider_id)
            .map(MarketRuntimeEvent::ProviderState);
        if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
            events.clear_series();
            events.series_state = None;
            events.demand_error = None;
            // Demand can attach to an already-online provider session. Publish
            // its current state immediately instead of waiting for a transition.
            events.provider = provider_state;
        }
        Ok((provider_generation, publication))
    }

    pub(super) fn handle_demand(
        &mut self,
        client_id: ClientId,
        series: &BarSeriesKey,
        streams: StreamRequirements,
        waiter: DemandWaiter,
        reply: &Reply<()>,
    ) {
        let (provider_generation, publication) =
            match self.accept_series_demand(client_id, series, streams, &waiter) {
                Ok(accepted) => accepted,
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            };
        self.prune_unused_live_series();
        self.prune_history_tracking();
        self.evict_unreferenced_series();
        let retains_subscription = self
            .engine
            .current_demand(waiter.consumer_id)
            .is_some_and(|demand| demand.resource_class.retains_subscription());
        if !retains_subscription {
            // Background/detached consumers retain their requested series identity
            // but intentionally own no upstream subscription. Park that demand
            // without starting provider/history work; a later Foreground resource
            // transition restores the engine subscription and rebuilds realtime
            // through `handle_resource_class`.
            self.release_idle();
            let _ = reply.send(Ok(()));
            return;
        }
        // The live handoff, and the bounded trade buffer inside it, must exist
        // before history starts. A Rithmic history fetch can take tens of
        // seconds; deferring the handoff until it returned dropped every trade
        // that arrived in between, and left `rithmic_live` empty across a symbol
        // switch, so the idle check below tore down the provider session that a
        // presentation change is never allowed to touch.
        let realtime = self.ensure_realtime(series);
        self.release_idle();
        if let Err(error) = realtime {
            let _ = reply.send(Err(error));
            return;
        }
        self.publish_order_book_to_consumer(waiter.consumer_id);
        let result = match publication {
            None => {
                if let Some(snapshot) = self.engine.series_snapshot(series) {
                    self.prepare_cached_demand(series, provider_generation, &snapshot)
                        .map(|_| ())
                } else {
                    self.start_uncached_demand(series, provider_generation, waiter);
                    Ok(())
                }
            }
            Some(publication) => {
                self.publish_cached_demand(series, provider_generation, &waiter, &publication)
            }
        };
        let _ = reply.send(result);
    }

    fn report_unsupported_calendar(
        &mut self,
        waiter: &DemandWaiter,
        series: &BarSeriesKey,
        detail: &str,
    ) {
        if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
            events.demand_error = Some(MarketRuntimeEvent::DemandError(crate::MarketDemandError {
                consumer_id: waiter.consumer_id,
                generation: waiter.generation,
                scope: crate::MarketDemandErrorScope::Series,
                code: aeris_contracts::EngineFaultCode::Permanent,
                stage: aeris_contracts::FailureStage::Handoff,
                detail: detail.to_string(),
                series: Some(series.clone()),
                cause: "calendar unavailable for live series".into(),
                elapsed_millis: None,
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    mod boundary {
        include!("tests/boundary.rs");
    }
    mod replay {
        include!("replay/mod.rs");
    }
    mod characterization {
        include!("tests/characterization.rs");
    }
    mod recovery {
        include!("tests/recovery.rs");
    }

    use super::super::lifecycle::RecoveryReason;
    use super::*;
    use crate::market_service::{
        FormingBar, HISTORY_SERIES_HIGH_WATERMARK, HISTORY_SERIES_TARGET_BARS, HistoryRange,
        HistorySnapshot, INITIAL_HISTORY_BARS, MAXIMUM_HISTORY_RETRIES, MAXIMUM_STORED_BARS,
        ProviderDispatchRecord, ProviderDisplayDepthEvent, SeriesLoadState,
    };
    use crate::study::{
        NativeStudyProgram, NativeStudyRegistration, StudyDefinition, StudyDependency,
        StudyExecutionContext, StudyInstanceId, StudyInvalidationPolicy, StudyMarketInput,
        StudyOutputSpec, StudyPaneTarget, StudyPlotKind, StudyPointStyle, StudyScaleTarget,
        StudySettings,
    };
    use crate::{
        hyperliquid_realtime::HyperliquidRealtimeEvent, rithmic_realtime::RithmicRealtimeEvent,
    };
    use aeris_contracts::{ProviderInstrumentSearchResult, ProviderMarketScreen};
    use aeris_market_data::{
        AggressorSide, BarPeriod, DepthLevel, DepthSnapshot, EventMetadata, MarketBar, MarketTrade,
        OrderBookState, QualifiedTimestamp, TopOfBookQuote,
    };
    use aeris_market_engine::{ConsumerResourceClass, GenerationId, Viewport, WorkspaceId};
    use std::num::NonZeroU64;

    const _: () = {
        assert!(HISTORY_SERIES_TARGET_BARS < HISTORY_SERIES_HIGH_WATERMARK);
        assert!(MAXIMUM_STORED_BARS < usize::MAX);
        assert!(
            MAXIMUM_STORED_BARS == super::super::MAXIMUM_SERIES * HISTORY_SERIES_HIGH_WATERMARK
        );
    };

    fn nonzero(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("test identity is nonzero")
    }

    fn client(value: u64) -> ClientId {
        ClientId(nonzero(value))
    }

    fn consumer(value: u64) -> ConsumerId {
        ConsumerId(nonzero(value))
    }

    fn generation(value: u64) -> GenerationId {
        GenerationId(nonzero(value))
    }

    fn series() -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            period: BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        }
    }

    fn instrument() -> InstallProviderInstrument {
        InstallProviderInstrument {
            provider: "rithmic".to_string(),
            session_generation: 1,
            selection_generation: 1,
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            provider_symbol: "MNQU6".to_string(),
            display_symbol: "MNQ Sep 2026".to_string(),
            venue_id: "CME".to_string(),
            price_scale: 2,
            quantity_scale: 0,
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            price_increment: Some(25),
            ..Default::default()
        }
    }

    fn hyperliquid_series() -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "hyperliquid".to_string(),
            instrument_id: "instrument:hyperliquid:BTC".to_string(),
            entitlement_id: "hyperliquid-public".to_string(),
            period: BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        }
    }

    fn hyperliquid_instrument() -> InstallProviderInstrument {
        InstallProviderInstrument {
            provider: "hyperliquid".to_string(),
            session_generation: 1,
            selection_generation: 1,
            instrument_id: "instrument:hyperliquid:BTC".to_string(),
            provider_symbol: "BTC".to_string(),
            display_symbol: "BTC-USDC".to_string(),
            venue_id: "Hyperliquid".to_string(),
            price_scale: 2,
            quantity_scale: 8,
            entitlement_id: "hyperliquid-public".to_string(),
            price_increment: None,
            ..Default::default()
        }
    }

    #[test]
    fn trading_feed_quote_lease_without_chart_drives_provider_and_book_demand() {
        let mut coordinator = coordinator();
        coordinator.trading_feed.attach();
        let instrument = hyperliquid_instrument();
        coordinator.catalog.insert(
            (
                instrument.provider.clone(),
                instrument.instrument_id.clone(),
            ),
            instrument.clone(),
        );
        let key = super::super::InstrumentStreamKey {
            provider: instrument.provider.clone(),
            instrument: aeris_instruments::InstrumentId::try_new(instrument.instrument_id.clone())
                .expect("valid instrument id"),
            entitlement: Some(instrument.entitlement_id.clone()),
        };
        let lease = coordinator
            .engine
            .acquire_instrument_lease(&key, StreamRequirements::NONE.with(MarketStream::Quotes))
            .expect("quotes-only lease");
        assert_eq!(coordinator.engine.subscriptions().len(), 0);
        let demand = coordinator.provider_demand(&instrument.provider);
        assert_eq!(demand.instruments.len(), 1);
        assert!(demand.instruments[0].streams.contains(MarketStream::Quotes));
        let wire = demand.hyperliquid_wire();
        assert_eq!(wire.quotes.len(), 1);
        assert_eq!(wire.candles.len(), 0);
        coordinator.reconcile_order_books();
        assert!(coordinator.order_books.contains_key(&(
            instrument.provider.clone(),
            instrument.instrument_id.clone()
        )));
        let generation = ProviderGeneration(nonzero(2));
        coordinator
            .engine
            .begin_provider_session(&instrument.provider, generation)
            .expect("provider session");
        coordinator.provider_quote(
            &instrument.provider,
            generation.0.get(),
            &aeris_market_data::TopOfBookQuote {
                metadata: aeris_market_data::EventMetadata {
                    provider_id: instrument.provider.clone(),
                    instrument_id: instrument.instrument_id.clone(),
                    entitlement_id: instrument.entitlement_id.clone(),
                    source_sequence: 1,
                    session_generation: generation.0.get(),
                    timestamps: aeris_market_data::QualifiedTimestamp {
                        exchange_unix_nanos: Some(10),
                        provider_unix_nanos: None,
                        received_unix_nanos: 11,
                    },
                },
                bid: Some(aeris_market_data::QuoteLevel {
                    price: 100,
                    quantity: Some(1),
                    order_count: None,
                }),
                ask: Some(aeris_market_data::QuoteLevel {
                    price: 101,
                    quantity: Some(2),
                    order_count: None,
                }),
            },
        );
        let state = coordinator
            .quotes
            .lock()
            .unwrap()
            .latest(&key)
            .expect("leased quote reaches the read projection");
        assert_eq!(state.quote.bid.units(), 100);
        assert_eq!(state.quote.ask.units(), 101);
        assert_eq!(state.quote.provider_generation, generation.0.get());
        assert!(matches!(
            coordinator.trading_feed.receive(Duration::ZERO),
            Some(aeris_trading::market::MarketFeedEvent::Quote(quote))
                if quote.bid.units() == 100 && quote.ask.units() == 101
        ));
        assert!(coordinator.engine.release_data_lease(lease));
        coordinator.reconcile_order_books();
        assert!(coordinator.order_books.is_empty());
        assert!(coordinator.quotes.lock().unwrap().latest(&key).is_none());
        assert_eq!(
            coordinator
                .provider_demand(&instrument.provider)
                .instruments
                .len(),
            0
        );
    }

    #[test]
    fn trading_feed_reconnect_and_release_emit_only_leased_instruments() {
        let mut coordinator = coordinator();
        coordinator.trading_feed.attach();
        let instrument = hyperliquid_instrument();
        let key = super::super::InstrumentStreamKey {
            provider: instrument.provider.clone(),
            instrument: aeris_instruments::InstrumentId::try_new(instrument.instrument_id).unwrap(),
            entitlement: Some(instrument.entitlement_id),
        };
        let lease = coordinator
            .engine
            .acquire_instrument_lease(&key, StreamRequirements::NONE.with(MarketStream::Quotes))
            .unwrap();
        coordinator.publish_provider_feed(
            &key.provider,
            2,
            aeris_trading::market::FeedState::Stale {
                reason: aeris_trading::market::FeedReason::Disconnected,
            },
        );
        assert!(matches!(
            coordinator.trading_feed.receive(Duration::ZERO),
            Some(aeris_trading::market::MarketFeedEvent::Feed {
                state: aeris_trading::market::FeedState::Stale { .. },
                ..
            })
        ));
        coordinator.publish_provider_feed(&key.provider, 3, aeris_trading::market::FeedState::Live);
        assert!(matches!(
            coordinator.trading_feed.receive(Duration::ZERO),
            Some(aeris_trading::market::MarketFeedEvent::Feed {
                state: aeris_trading::market::FeedState::Live,
                provider_generation: 3,
                ..
            })
        ));
        assert!(coordinator.engine.release_data_lease(lease));
        coordinator.publish_provider_feed(&key.provider, 4, aeris_trading::market::FeedState::Live);
        assert!(coordinator.trading_feed.receive(Duration::ZERO).is_none());
    }

    #[test]
    fn trading_feed_chart_quote_stream_emits_without_a_separate_instrument_lease() {
        let mut coordinator = coordinator();
        coordinator.trading_feed.attach();
        let instrument = hyperliquid_instrument();
        coordinator.catalog.insert(
            (
                instrument.provider.clone(),
                instrument.instrument_id.clone(),
            ),
            instrument.clone(),
        );
        let series = hyperliquid_series();
        let (lease, _) = coordinator
            .engine
            .acquire_data_lease(&series, StreamRequirements::BARS.with(MarketStream::Quotes))
            .unwrap();
        assert_eq!(coordinator.engine.instrument_subscriptions().count(), 0);
        coordinator.reconcile_order_books();
        let generation = ProviderGeneration(nonzero(2));
        coordinator
            .engine
            .begin_provider_session(&instrument.provider, generation)
            .unwrap();
        coordinator.provider_quote(
            &instrument.provider,
            2,
            &TopOfBookQuote {
                metadata: EventMetadata {
                    provider_id: instrument.provider.clone(),
                    instrument_id: instrument.instrument_id.clone(),
                    entitlement_id: instrument.entitlement_id.clone(),
                    session_generation: 2,
                    source_sequence: 1,
                    timestamps: QualifiedTimestamp {
                        exchange_unix_nanos: Some(10),
                        provider_unix_nanos: None,
                        received_unix_nanos: 11,
                    },
                },
                bid: Some(aeris_market_data::QuoteLevel {
                    price: 100,
                    quantity: None,
                    order_count: None,
                }),
                ask: Some(aeris_market_data::QuoteLevel {
                    price: 101,
                    quantity: None,
                    order_count: None,
                }),
            },
        );
        assert!(matches!(
            coordinator.trading_feed.receive(Duration::ZERO),
            Some(aeris_trading::market::MarketFeedEvent::Quote(quote))
                if quote.bid.units() == 100 && quote.ask.units() == 101
        ));
        assert!(coordinator.engine.release_data_lease(lease));
        coordinator.reconcile_order_books();
        assert!(coordinator.order_books.is_empty());
    }

    #[test]
    fn trading_feed_catalog_terms_and_lease_session_transition() {
        use aeris_trading::market::MarketFeedEvent;
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-18T23:59:59Z")
            .unwrap()
            .timestamp_nanos_opt()
            .unwrap();
        let clock = Arc::new(super::super::clock::ScriptedClock::new(at));
        let mut coordinator = coordinator();
        coordinator.clock = clock.clone();
        coordinator.trading_feed.attach();
        let mut instrument = instrument();
        instrument.contract_metadata = Some(Box::new(aeris_contracts::ProviderContractMetadata {
            currency: Some("USD".into()),
            point_value: Some(5),
            point_value_scale: Some(0),
            session_hours: vec![aeris_contracts::ProviderSessionHours {
                weekday: 1,
                open_seconds: 0,
                close_seconds: 3600,
                timezone: "UTC".into(),
            }],
            ..Default::default()
        }));
        coordinator
            .install_provider_instrument(&instrument)
            .unwrap();
        assert!(matches!(
            coordinator.trading_feed.receive(Duration::ZERO),
            Some(MarketFeedEvent::Terms(terms))
                if terms.price_grid == aeris_instruments::PriceGrid::Fixed(
                    aeris_instruments::InstrumentDecimal::try_new(25, 2).unwrap()
                )
        ));
        let key = super::super::InstrumentStreamKey {
            provider: instrument.provider,
            instrument: aeris_instruments::InstrumentId::try_new(instrument.instrument_id).unwrap(),
            entitlement: Some(instrument.entitlement_id),
        };
        coordinator
            .engine
            .acquire_instrument_lease(&key, StreamRequirements::NONE.with(MarketStream::Quotes))
            .unwrap();
        coordinator.publish_trading_session(&key);
        assert!(matches!(
            coordinator.trading_feed.receive(Duration::ZERO),
            Some(MarketFeedEvent::Session(session))
                if session.phase == aeris_instruments::SessionPhase::Closed
        ));
        coordinator.refresh_deadlines();
        assert!(
            coordinator
                .deadlines
                .values()
                .any(|timer| timer.contains(TimerKey::SESSION_TRANSITION))
        );
        clock.advance(Duration::from_secs(1));
        coordinator.publish_trading_session(&key);
        assert!(matches!(
            coordinator.trading_feed.receive(Duration::ZERO),
            Some(MarketFeedEvent::Session(session))
                if session.phase == aeris_instruments::SessionPhase::Open
        ));
    }

    #[test]
    fn trading_feed_interest_revisions_replace_shared_quote_leases_atomically() {
        let mut coordinator = coordinator();
        let instrument = hyperliquid_instrument();
        coordinator.catalog.insert(
            (
                instrument.provider.clone(),
                instrument.instrument_id.clone(),
            ),
            instrument.clone(),
        );
        let route = aeris_trading::market::MarketRoute {
            provider_id: instrument.provider.clone(),
            instrument_id: aeris_instruments::InstrumentId::try_new(
                instrument.instrument_id.clone(),
            )
            .unwrap(),
            provider_symbol: instrument.provider_symbol.clone(),
            display_symbol: instrument.display_symbol.clone(),
            venue_id: instrument.venue_id.clone(),
            entitlement_id: Some(instrument.entitlement_id.clone()),
            price_scale: u8::try_from(instrument.price_scale).unwrap(),
            quantity_scale: u8::try_from(instrument.quantity_scale).unwrap(),
        };
        let interest = aeris_trading::market::QuoteInterest {
            revision: 1,
            routes: vec![route],
        };
        coordinator.install_trading_interest(&interest).unwrap();
        assert_eq!(coordinator.engine.instrument_subscriptions().count(), 1);
        assert_eq!(
            coordinator
                .provider_demand(&instrument.provider)
                .instruments
                .len(),
            1
        );
        assert!(coordinator.install_trading_interest(&interest).is_err());
        assert_eq!(coordinator.engine.instrument_subscriptions().count(), 1);
        coordinator
            .install_trading_interest(&aeris_trading::market::QuoteInterest {
                revision: 2,
                routes: vec![],
            })
            .unwrap();
        assert_eq!(coordinator.engine.instrument_subscriptions().count(), 0);
        assert_eq!(
            coordinator
                .provider_demand(&instrument.provider)
                .instruments
                .len(),
            0
        );
    }

    #[test]
    fn trading_feed_price_alerts_use_engine_leases_instead_of_outside_demand() {
        let mut coordinator = coordinator();
        let instrument = hyperliquid_instrument();
        coordinator.catalog.insert(
            (
                instrument.provider.clone(),
                instrument.instrument_id.clone(),
            ),
            instrument.clone(),
        );
        let alert = crate::MarketPriceAlert {
            id: "test-alert".into(),
            instrument: instrument.clone(),
            price: 100,
            condition: aeris_contracts::PriceAlertCondition::CrossingUp,
            frequency: aeris_contracts::PriceAlertFrequency::OnlyOnce,
            active: true,
        };
        coordinator
            .replace_price_alerts(consumer(1), vec![alert])
            .expect("active alert installs the shared engine lease");
        let demand = coordinator.provider_demand(&instrument.provider);
        assert_eq!(demand.instruments.len(), 1);
        assert!(demand.instruments[0].streams.contains(MarketStream::Trades));
        assert_eq!(demand.hyperliquid_wire().trades.len(), 1);
        assert_eq!(coordinator.engine.instrument_subscriptions().count(), 1);
        coordinator
            .replace_price_alerts(consumer(1), vec![])
            .expect("removing the last alert releases the shared lease");
        assert_eq!(coordinator.engine.instrument_subscriptions().count(), 0);
        assert_eq!(
            coordinator
                .provider_demand(&instrument.provider)
                .instruments
                .len(),
            0
        );
    }

    fn study_definition_with_streams(
        series: BarSeriesKey,
        streams: StreamRequirements,
    ) -> StudyDefinition {
        StudyDefinition {
            identifier: "test.study".to_string(),
            dependencies: vec![StudyDependency::Market(StudyMarketInput {
                series,
                streams,
            })],
            settings: Vec::new(),
            outputs: vec![StudyOutputSpec {
                identifier: "value".to_string(),
                title: "Test Study".to_string(),
                legend_label: None,
                plot: StudyPlotKind::Line,
                pane: StudyPaneTarget::Price,
                scale: StudyScaleTarget::Primary,
                threshold_region: None,
                point_style: StudyPointStyle::default(),
            }],
            invalidation: StudyInvalidationPolicy::SameRange,
        }
    }

    fn study_definition(series: BarSeriesKey) -> StudyDefinition {
        study_definition_with_streams(series, StreamRequirements::BARS)
    }

    fn calculate_test_study(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let output = context
            .output(0)
            .ok_or_else(|| "test study output is unavailable".to_string())?;
        if !output.is_empty() {
            output.set(0, None)?;
        }
        Ok(())
    }

    fn calculate_failing_study(_context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        Err("intentional reinitialization failure".to_string())
    }

    fn study_registration(series: BarSeriesKey) -> NativeStudyRegistration {
        let definition = study_definition(series);
        NativeStudyRegistration {
            settings: StudySettings::defaults(&definition.settings).expect("valid defaults"),
            definition,
            program: NativeStudyProgram {
                calculate: calculate_test_study,
                state_factory: None,
            },
        }
    }

    fn study_registration_with_streams(
        series: BarSeriesKey,
        streams: StreamRequirements,
    ) -> NativeStudyRegistration {
        native_study_registration(study_definition_with_streams(series, streams))
    }

    fn native_study_registration(definition: StudyDefinition) -> NativeStudyRegistration {
        NativeStudyRegistration {
            settings: StudySettings::defaults(&definition.settings).expect("valid defaults"),
            definition,
            program: NativeStudyProgram {
                calculate: calculate_test_study,
                state_factory: None,
            },
        }
    }

    fn failing_study_registration(series: BarSeriesKey) -> NativeStudyRegistration {
        let mut registration = study_registration(series);
        registration.program.calculate = calculate_failing_study;
        registration
    }

    fn coordinator() -> Coordinator<'static> {
        Coordinator {
            engine: super::super::configured_engine().expect("configured engine"),
            studies: StudyRuntime::new(StudyRuntimeConfig {
                maximum_studies: NonZeroUsize::new(MAXIMUM_STUDIES).expect("study bound"),
                maximum_dependencies_per_study: NonZeroUsize::new(MAXIMUM_STUDY_DEPENDENCIES)
                    .expect("dependency bound"),
                maximum_outputs_per_study: NonZeroUsize::new(MAXIMUM_STUDY_OUTPUTS)
                    .expect("output bound"),
                maximum_points_per_output: NonZeroUsize::new(MAXIMUM_STUDY_POINTS_PER_OUTPUT)
                    .expect("output point bound"),
                maximum_total_output_points: NonZeroUsize::new(MAXIMUM_STUDY_TOTAL_OUTPUT_POINTS)
                    .expect("total output point bound"),
                maximum_state_bytes_per_study: NonZeroUsize::new(
                    MAXIMUM_STUDY_STATE_BYTES_PER_INSTANCE,
                )
                .expect("study state bound"),
                maximum_total_state_bytes: NonZeroUsize::new(MAXIMUM_STUDY_TOTAL_STATE_BYTES)
                    .expect("total study state bound"),
            }),
            providers: ProviderDispatch {
                wake: None,
                records: BTreeMap::new(),
                lanes: Vec::new(),
            },
            clock: Arc::new(super::super::clock::SystemClock),
            calendar: super::super::calendar::MarketCalendarService::default(),
            provider_epochs: ProviderEpochRegistry::default(),
            provider_registry: {
                let mut registry = super::super::provider_session::ProviderRegistry::new();
                for descriptor in super::super::BUILT_IN_PROVIDER_DESCRIPTORS {
                    registry.register_control(descriptor.id);
                }
                registry
            },
            attached: BTreeSet::new(),
            consumer_clients: BTreeMap::new(),
            consumer_epochs: BTreeMap::new(),
            consumer_epoch_sequence: EpochSequence::default(),
            pending: BTreeMap::new(),
            history: super::super::history::tracker::HistoryTracker::default(),
            events: BTreeMap::new(),
            outbox: Arc::new(super::super::outbox::ConsumerOutbox::default()),
            series: super::super::lifecycle::SeriesLifecycles::default(),
            dirty_series: BTreeSet::new(),
            deadlines: BTreeMap::new(),
            pending_activity: true,
            quote_releases: None,
            quotes: Arc::new(Mutex::new(super::super::quotes::QuoteBook::default())),
            trading_feed: Arc::new(super::super::trading_feed::TradingFeedOutbox::default()),
            trading_feed_snapshot_sent: false,
            trading_interest_revision: 0,
            control_retry: false,
            series_effects: super::super::lifecycle::EffectBuf::default(),
            order_books: BTreeMap::new(),
            display_depth: BTreeMap::new(),
            display_generations: BTreeMap::new(),
            price_alerts: PriceAlertRegistry::default(),
            catalog: BTreeMap::new(),
            catalog_sessions: BTreeMap::new(),
            command_fences: super::super::CommandFences::default(),
            pending_selections: BTreeMap::new(),
            selection_epochs: BTreeMap::new(),
        }
    }

    fn register(coordinator: &mut Coordinator<'_>, consumer_id: ConsumerId) {
        coordinator
            .engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: client(1),
                    workspace_id: WorkspaceId(nonzero(1)),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
    }

    #[test]
    fn consumer_outbox_wakes_only_after_publication_and_retires_with_client() {
        let mut coordinator = coordinator();
        let owner = client(1);
        let consumer_id = consumer(1);
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_attach(owner, &reply);
        received.recv().expect("attach reply").expect("attached");
        coordinator
            .handle_register(ConsumerIdentity {
                client_id: owner,
                workspace_id: WorkspaceId(nonzero(1)),
                consumer_id,
            })
            .expect("register");
        assert!(!coordinator.outbox.wait(owner, Duration::ZERO));
        coordinator
            .events
            .get_mut(&consumer_id)
            .expect("outbox")
            .provider = Some(MarketRuntimeEvent::ProviderState(
            aeris_contracts::ProviderState {
                provider: "fixture".to_string(),
                state: ProviderConnectionState::Online,
                generation: 1,
                detail: None,
                transport_rtt_nanos: None,
            },
        ));
        coordinator.signal_pending_consumers();
        assert!(coordinator.outbox.wait(owner, Duration::ZERO));
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_drain_client(owner, vec![(consumer_id, 1)], &reply);
        assert_eq!(
            received.recv().expect("drain").expect("owner batch").len(),
            1
        );
        assert!(!coordinator.outbox.wait(owner, Duration::ZERO));
        coordinator.outbox.notify(owner);
        coordinator.detach_client(owner);
        assert!(!coordinator.outbox.wait(owner, Duration::ZERO));
    }

    #[test]
    fn consumer_registration_mints_new_epoch_after_retirement() {
        let mut coordinator = coordinator();
        let owner = client(1);
        let consumer_id = consumer(1);
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_attach(owner, &reply);
        received.recv().expect("attach reply").expect("attached");
        let identity = ConsumerIdentity {
            client_id: owner,
            workspace_id: WorkspaceId(nonzero(1)),
            consumer_id,
        };
        coordinator
            .handle_register(identity)
            .expect("first registration");
        let first = coordinator.consumer_epochs[&consumer_id];
        coordinator
            .handle_remove(owner, consumer_id)
            .expect("retirement");
        assert!(!coordinator.consumer_epochs.contains_key(&consumer_id));
        coordinator
            .handle_register(identity)
            .expect("second registration");
        assert_ne!(coordinator.consumer_epochs[&consumer_id], first);
    }

    fn history_epoch(
        coordinator: &mut Coordinator<'_>,
        provider: &str,
        generation: ProviderGeneration,
    ) -> aeris_market_engine::fence::Epoch<aeris_market_engine::fence::ProviderTag> {
        coordinator
            .provider_epochs
            .stamp(
                provider,
                generation.0.get(),
                &mut coordinator.provider_registry,
            )
            .expect("provider epoch")
    }

    fn history_ticket(
        coordinator: &mut Coordinator<'_>,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
    ) -> super::super::history::tracker::HistoryTicket {
        let epoch = history_epoch(coordinator, &series.provider_id, generation);
        if let Some((ticket, active_range)) = coordinator.history.inflight(series, epoch)
            && active_range == range
        {
            return ticket.clone();
        }
        coordinator.history.begin(
            series.clone(),
            epoch,
            if range.is_some() {
                super::super::history::tracker::LaneKind::Backfill
            } else {
                super::super::history::tracker::LaneKind::Current
            },
            range,
            coordinator.clock.as_ref(),
        )
    }

    fn finish_history(
        coordinator: &mut Coordinator<'_>,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
        result: Result<super::super::HistorySnapshot, String>,
    ) {
        let ticket = history_ticket(coordinator, series, generation, range);
        coordinator.history_completed(&ticket, generation, range, result);
    }

    #[test]
    fn exhausted_current_history_keeps_one_cooldown_recovery_ticket() {
        let mut coordinator = coordinator();
        let selected = series();
        let generation = ProviderGeneration(nonzero(1));
        let epoch = history_epoch(&mut coordinator, &selected.provider_id, generation);
        for _ in 0..super::super::MAXIMUM_HISTORY_RETRIES {
            assert!(coordinator.schedule_history_retry(&selected, generation, None, "transient"));
        }
        assert!(!coordinator.schedule_history_retry(&selected, generation, None, "transient"));
        let ticket = coordinator
            .history
            .pending(
                &selected,
                epoch,
                super::super::history::tracker::LaneKind::Current,
            )
            .unwrap();
        assert_eq!(ticket.2, 0);
        assert!(ticket.1.is_none());
        assert!(ticket.0 > Instant::now());
        assert_eq!(coordinator.history.count(), 1);
    }

    #[test]
    fn history_retry_uses_scripted_clock_without_waiting() {
        use super::super::clock::Clock;
        let clock = Arc::new(super::super::clock::ScriptedClock::new(1_000_000_000));
        let mut coordinator = coordinator();
        coordinator.clock = clock.clone();
        let selected = series();
        let generation = ProviderGeneration(nonzero(1));
        let epoch = history_epoch(&mut coordinator, &selected.provider_id, generation);
        assert!(coordinator.schedule_history_retry(&selected, generation, None, "transient"));
        let deadline = coordinator
            .history
            .pending(
                &selected,
                epoch,
                super::super::history::tracker::LaneKind::Current,
            )
            .unwrap()
            .0;
        assert!(deadline > clock.monotonic());
        clock.advance(Duration::from_millis(250));
        assert_eq!(
            coordinator
                .history
                .pending(
                    &selected,
                    epoch,
                    super::super::history::tracker::LaneKind::Current
                )
                .unwrap()
                .0,
            deadline
        );
        clock.advance(deadline.saturating_duration_since(clock.monotonic()));
        assert_eq!(clock.monotonic(), deadline);
    }

    #[test]
    fn terminal_failure_report_is_delivered_without_a_realtime_lane() {
        let mut coordinator = coordinator();
        let generation = ProviderGeneration(nonzero(1));
        coordinator
            .engine
            .begin_provider_session("tastytrade", generation)
            .expect("provider session begins");
        let wake = Box::leak(Box::new(super::super::ProviderCoordinatorWake::for_tests()));
        coordinator.providers.wake = Some(wake);
        wake.report_failure("tastytrade", 1);
        drain_coordinator_events(&mut coordinator);
        assert_eq!(
            coordinator
                .engine
                .provider_status("tastytrade")
                .map(|status| status.health),
            Some(ProviderHealth::Failed)
        );
    }

    #[test]
    fn terminal_failure_before_connected_is_visible() {
        let mut coordinator = coordinator();
        let wake = Box::leak(Box::new(super::super::ProviderCoordinatorWake::for_tests()));
        coordinator.providers.wake = Some(wake);
        wake.report_failure("tastytrade", 1);
        drain_coordinator_events(&mut coordinator);
        let status = coordinator
            .engine
            .provider_status("tastytrade")
            .expect("failed provider status");
        assert_eq!(status.generation, Some(ProviderGeneration(nonzero(1))));
        assert_eq!(status.health, ProviderHealth::Failed);
    }

    #[test]
    fn dead_provider_worker_publishes_failure_and_removes_retry_timer() {
        use super::super::clock::Clock;
        let clock = Arc::new(super::super::clock::ScriptedClock::new(51));
        let mut coordinator = coordinator();
        coordinator.clock = clock.clone();
        let (controls, receiver) = std::sync::mpsc::sync_channel(1);
        let (history, _history_receiver) = std::sync::mpsc::sync_channel(1);
        coordinator.provider_registry.register(
            "ctrader",
            Box::new(super::super::provider_session::CtraderSession { controls, history }),
            super::super::reconnect::ReconnectPolicy::ctrader(),
            clock.as_ref(),
        );
        let first = coordinator
            .provider_registry
            .connect("ctrader", clock.monotonic())
            .unwrap()
            .unwrap();
        coordinator.begin_reserved_provider_epoch("ctrader", first);
        coordinator
            .provider_registry
            .disconnected("ctrader", first, clock.monotonic());
        drop(receiver);
        let due = coordinator.provider_registry.next_retry().unwrap();
        clock.advance(due.duration_since(clock.monotonic()));
        coordinator.connect_due_providers();
        coordinator.publish_failed_provider_controls();
        coordinator.refresh_deadlines();
        assert_eq!(
            coordinator
                .engine
                .provider_status("ctrader")
                .map(|status| status.health),
            Some(ProviderHealth::Failed)
        );
        assert_eq!(coordinator.provider_registry.next_retry(), None);
        assert!(
            coordinator
                .deadlines
                .keys()
                .all(|at| *at > clock.monotonic())
        );
    }

    #[test]
    fn full_provider_worker_bounds_coordinator_retry_turns() {
        use super::super::clock::Clock;
        let clock = Arc::new(super::super::clock::ScriptedClock::new(54));
        let mut coordinator = coordinator();
        coordinator.clock = clock.clone();
        let (controls, _receiver) = std::sync::mpsc::sync_channel(1);
        let (history, _history_receiver) = std::sync::mpsc::sync_channel(1);
        coordinator.provider_registry.register(
            "ctrader",
            Box::new(super::super::provider_session::CtraderSession { controls, history }),
            super::super::reconnect::ReconnectPolicy::ctrader(),
            clock.as_ref(),
        );
        let first = coordinator
            .provider_registry
            .connect("ctrader", clock.monotonic())
            .unwrap()
            .unwrap();
        coordinator.begin_reserved_provider_epoch("ctrader", first);
        coordinator
            .provider_registry
            .disconnected("ctrader", first, clock.monotonic());
        let end = clock.monotonic() + Duration::from_secs(60);
        let mut turns = 0;
        while clock.monotonic() < end {
            coordinator.refresh_deadlines();
            let Some(wait) = coordinator.next_wait() else {
                break;
            };
            assert!(
                wait > Duration::ZERO,
                "coordinator may not schedule an immediate turn"
            );
            turns += 1;
            assert!(turns <= 10, "retry budget must bound coordinator turns");
            clock.advance(wait.min(end.duration_since(clock.monotonic())));
            if clock.monotonic() < end {
                coordinator.connect_due_providers();
                coordinator.publish_failed_provider_controls();
            }
        }
        assert_eq!(
            coordinator
                .engine
                .provider_status("ctrader")
                .map(|status| status.health),
            Some(ProviderHealth::Failed)
        );
        assert_eq!(coordinator.provider_registry.next_retry(), None);
    }

    #[test]
    fn tastytrade_logout_rejects_late_events_after_reauthorization() {
        let mut coordinator = coordinator();
        coordinator.handle_provider_event(super::super::ProviderEvent::tastytrade(
            super::super::tastytrade::RealtimeEvent::Connecting(1),
        ));
        coordinator.handle_provider_event(super::super::ProviderEvent::tastytrade(
            super::super::tastytrade::RealtimeEvent::Connected(1),
        ));
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_command(Command::BrokerAuthorizationChanged(
            "tastytrade",
            false,
            reply,
        ));
        result.recv().unwrap().unwrap();
        assert!(coordinator.session("tastytrade").suspended);
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_command(Command::BrokerAuthorizationChanged(
            "tastytrade",
            true,
            reply,
        ));
        result.recv().unwrap().unwrap();
        coordinator.handle_provider_event(super::super::ProviderEvent::tastytrade(
            super::super::tastytrade::RealtimeEvent::Connected(1),
        ));
        assert_ne!(
            coordinator
                .engine
                .provider_status("tastytrade")
                .unwrap()
                .health,
            ProviderHealth::Online
        );
        coordinator.handle_provider_event(super::super::ProviderEvent::tastytrade(
            super::super::tastytrade::RealtimeEvent::Connecting(2),
        ));
        coordinator.handle_provider_event(super::super::ProviderEvent::tastytrade(
            super::super::tastytrade::RealtimeEvent::Connected(2),
        ));
        assert_eq!(
            coordinator
                .engine
                .provider_status("tastytrade")
                .unwrap()
                .generation,
            Some(ProviderGeneration(std::num::NonZeroU64::new(2).unwrap()))
        );
    }

    #[test]
    fn tastytrade_series_recovery_does_not_downgrade_provider_health() {
        let mut coordinator = coordinator();
        coordinator.handle_provider_event(super::super::ProviderEvent::tastytrade(
            super::super::tastytrade::RealtimeEvent::Connecting(1),
        ));
        coordinator.handle_provider_event(super::super::ProviderEvent::tastytrade(
            super::super::tastytrade::RealtimeEvent::Connected(1),
        ));
        let mut selected = instrument();
        selected.provider = "tastytrade".into();
        selected.instrument_id = "tastytrade:Future:/ESZ6".into();
        selected.entitlement_id = super::super::tastytrade::ENTITLEMENT.into();
        coordinator.handle_provider_event(super::super::ProviderEvent::tastytrade(
            super::super::tastytrade::RealtimeEvent::TradeRecovery(1, selected),
        ));
        assert_eq!(
            coordinator
                .engine
                .provider_status("tastytrade")
                .expect("tastytrade status")
                .health,
            ProviderHealth::Online
        );
    }

    #[test]
    fn cancelled_request_cannot_mutate_the_coordinator() {
        let mut coordinator = coordinator();
        let state = Arc::new(super::super::RequestState(
            std::sync::atomic::AtomicU8::new(0),
        ));
        assert!(state.cancel());
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_command(Command::Request(
            Box::new(Command::Attach(client(99), reply)),
            state,
        ));
        assert!(!coordinator.attached.contains(&client(99)));
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn initial_background_demand_parks_without_realtime_or_history_error() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator
            .engine
            .set_resource_class(consumer, ConsumerResourceClass::Background)
            .expect("consumer starts background");
        coordinator
            .install_provider_instrument(&instrument())
            .expect("instrument installs");
        let selected = series();
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);

        coordinator.handle_demand(
            client(1),
            &selected,
            StreamRequirements::BARS,
            DemandWaiter {
                consumer_id: consumer,
                generation: generation(1),
                started_at: Instant::now(),
            },
            &reply_tx,
        );

        assert_eq!(reply_rx.recv().expect("demand reply"), Ok(()));
        let demand = coordinator
            .engine
            .current_demand(consumer)
            .expect("background demand remains installed");
        assert_eq!(demand.series.as_ref(), Some(&selected));
        assert_eq!(demand.resource_class, ConsumerResourceClass::Background);
        assert!(!coordinator.engine.has_subscription(&selected));
        assert!(!coordinator.series.trade_contains_key(&selected));
        assert!(!coordinator.pending.contains_key(&selected));
        assert_eq!(coordinator.history.count(), 0);
    }

    fn minute_bars(start_minute: i64, count: usize) -> Vec<MarketBar> {
        (0..count)
            .map(|index| {
                let minute = start_minute + i64::try_from(index).expect("bounded minute index");
                MarketBar {
                    source_sequence: u64::try_from(index + 1).expect("bounded sequence"),
                    exchange_timestamp_seconds: minute * 60,
                    exchange_timestamp_unix_nanos: minute * 60 * 1_000_000_000,
                    open: 100,
                    high: 101,
                    low: 99,
                    close: 100,
                    volume: 1,
                }
            })
            .collect()
    }

    fn rithmic_trade_for_series(
        series: &BarSeriesKey,
        sequence: u64,
        timestamp: i64,
    ) -> MarketTrade {
        MarketTrade {
            metadata: EventMetadata {
                provider_id: series.provider_id.clone(),
                instrument_id: series.instrument_id.clone(),
                entitlement_id: series.entitlement_id.clone(),
                source_sequence: sequence,
                session_generation: 1,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(timestamp),
                    provider_unix_nanos: None,
                    received_unix_nanos: timestamp,
                },
            },
            trade_id: format!("bounded-history-{sequence}"),
            price: 102,
            quantity: 1,
            aggressor: AggressorSide::Buy,
        }
    }

    fn seeded_study_order_book(
        selected: &InstallProviderInstrument,
        selected_series: &BarSeriesKey,
    ) -> ProviderOrderBook {
        let mut order_book = ProviderOrderBook::new(
            selected.clone(),
            super::super::TradeContinuity::Sequence,
            false,
        );
        let quote = TopOfBookQuote {
            metadata: EventMetadata {
                provider_id: selected.provider.clone(),
                instrument_id: selected.instrument_id.clone(),
                entitlement_id: selected.entitlement_id.clone(),
                source_sequence: 1,
                session_generation: 1,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(1),
                    provider_unix_nanos: None,
                    received_unix_nanos: 1,
                },
            },
            bid: Some(aeris_market_data::QuoteLevel {
                price: 100,
                quantity: Some(2),
                order_count: None,
            }),
            ask: Some(aeris_market_data::QuoteLevel {
                price: 101,
                quantity: Some(2),
                order_count: None,
            }),
        };
        assert!(order_book.install_top_of_book(&quote));
        assert!(order_book.accept_recent_trade(&rithmic_trade_for_series(selected_series, 2, 2)));
        order_book
            .book
            .install_snapshot(&DepthSnapshot {
                metadata: EventMetadata {
                    source_sequence: 3,
                    timestamps: QualifiedTimestamp {
                        exchange_unix_nanos: Some(3),
                        provider_unix_nanos: None,
                        received_unix_nanos: 3,
                    },
                    ..quote.metadata
                },
                bids: vec![DepthLevel {
                    price: 100,
                    quantity: 4,
                    order_count: Some(1),
                }],
                asks: vec![DepthLevel {
                    price: 101,
                    quantity: 5,
                    order_count: Some(1),
                }],
            })
            .expect("depth snapshot installs");
        order_book
    }

    fn ready_non_bar_study_fixture() -> (
        Coordinator<'static>,
        InstallProviderInstrument,
        StudyMarketInput,
    ) {
        let mut coordinator = coordinator();
        let selected = instrument();
        coordinator
            .install_provider_instrument(&selected)
            .expect("provider session installs");
        coordinator
            .engine
            .set_provider_health(
                "rithmic",
                ProviderGeneration(nonzero(1)),
                ProviderHealth::Online,
            )
            .expect("provider is online");
        let selected_series = series();
        let order_book = seeded_study_order_book(&selected, &selected_series);
        coordinator.order_books.insert(
            (selected.provider.clone(), selected.instrument_id.clone()),
            order_book,
        );
        let mut live_handoff = TradeModel::new(
            &selected_series,
            ProviderGeneration(nonzero(1)),
            &selected.venue_id,
            None,
        )
        .expect("live handoff");
        live_handoff.connected = true;
        coordinator
            .series
            .insert_trade(selected_series.clone(), live_handoff);
        set_trade_history_ready(&mut coordinator, &selected_series);
        let input = StudyMarketInput {
            series: selected_series,
            streams: StreamRequirements::BARS
                .with(MarketStream::Trades)
                .with(MarketStream::Quotes)
                .with(MarketStream::Depth),
        };
        (coordinator, selected, input)
    }

    fn trade_recovery_study_fixture() -> (
        Coordinator<'static>,
        InstallProviderInstrument,
        BarSeriesKey,
        StudyInstanceId,
    ) {
        let mut coordinator = coordinator();
        let owner = consumer(1);
        register(&mut coordinator, owner);
        coordinator.events.insert(owner, ConsumerEvents::default());
        let selected = instrument();
        coordinator
            .install_provider_instrument(&selected)
            .expect("provider session installs");
        let provider_generation = ProviderGeneration(nonzero(selected.session_generation));
        coordinator
            .engine
            .set_provider_health("rithmic", provider_generation, ProviderHealth::Online)
            .expect("provider is online");
        let selected_series = series();
        coordinator
            .engine
            .install_history(
                provider_generation,
                &selected_series,
                2,
                0,
                minute_bars(0, 3),
            )
            .expect("canonical history installs");
        let study_id = coordinator
            .handle_register_study(
                client(1),
                owner,
                study_registration_with_streams(
                    selected_series.clone(),
                    StreamRequirements::BARS.with(MarketStream::Trades),
                ),
            )
            .expect("trade study registers");
        (coordinator, selected, selected_series, study_id)
    }

    struct OutputAncestryRecoveryFixture {
        coordinator: Coordinator<'static>,
        selected: InstallProviderInstrument,
        primary: BarSeriesKey,
        secondary: BarSeriesKey,
        consumer_study: StudyInstanceId,
    }

    fn output_ancestry_recovery_fixture() -> OutputAncestryRecoveryFixture {
        let mut coordinator = coordinator();
        let owner = consumer(1);
        register(&mut coordinator, owner);
        coordinator.events.insert(owner, ConsumerEvents::default());
        let selected = instrument();
        coordinator
            .install_provider_instrument(&selected)
            .expect("provider session installs");
        let provider_generation = ProviderGeneration(nonzero(selected.session_generation));
        coordinator
            .engine
            .set_provider_health("rithmic", provider_generation, ProviderHealth::Online)
            .expect("provider is online");

        let secondary = series();
        let mut primary = secondary.clone();
        primary.period = BarPeriod::time(300).expect("five-minute period");
        coordinator
            .engine
            .install_history(provider_generation, &primary, 2, 0, minute_bars(0, 1))
            .expect("producer history installs");

        let producer = coordinator
            .handle_register_study(client(1), owner, study_registration(primary.clone()))
            .expect("producer study registers");
        coordinator
            .engine
            .install_history(provider_generation, &secondary, 2, 0, minute_bars(0, 3))
            .expect("quote history installs");
        let mut consumer_definition = study_definition(secondary.clone());
        consumer_definition.identifier = "test.output-ancestry.recovery-fence".to_string();
        consumer_definition.dependencies = vec![
            StudyDependency::Output(producer.output(0)),
            StudyDependency::Market(StudyMarketInput {
                series: secondary.clone(),
                streams: StreamRequirements::BARS.with(MarketStream::Quotes),
            }),
        ];
        let consumer_study = coordinator
            .handle_register_study(
                client(1),
                owner,
                native_study_registration(consumer_definition),
            )
            .expect("output consumer study registers");

        OutputAncestryRecoveryFixture {
            coordinator,
            selected,
            primary,
            secondary,
            consumer_study,
        }
    }

    fn quote_for_instrument(
        instrument: &InstallProviderInstrument,
        source_sequence: u64,
        timestamp: i64,
        price: i64,
    ) -> TopOfBookQuote {
        TopOfBookQuote {
            metadata: EventMetadata {
                provider_id: instrument.provider.clone(),
                instrument_id: instrument.instrument_id.clone(),
                entitlement_id: instrument.entitlement_id.clone(),
                source_sequence,
                session_generation: instrument.session_generation,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(timestamp),
                    provider_unix_nanos: None,
                    received_unix_nanos: timestamp,
                },
            },
            bid: Some(aeris_market_data::QuoteLevel {
                price,
                quantity: Some(1),
                order_count: None,
            }),
            ask: Some(aeris_market_data::QuoteLevel {
                price: price + 1,
                quantity: Some(1),
                order_count: None,
            }),
        }
    }

    fn set_trade_history_ready(coordinator: &mut Coordinator<'_>, series: &BarSeriesKey) {
        let live = coordinator
            .series
            .trade_lifecycle_mut(series)
            .expect("trade lifecycle");
        live.state = SeriesState::Live {
            last_progress: live.clock.monotonic(),
            observed_live: false,
        };
    }

    fn assert_rithmic_history_ready(coordinator: &Coordinator<'_>, series: &BarSeriesKey) {
        assert!(
            coordinator
                .series
                .trade_lifecycle(series)
                .is_some_and(SeriesLifecycle::history_ready)
        );
    }

    struct DetachedRithmicFixture {
        coordinator: Coordinator<'static>,
        series: BarSeriesKey,
        provider_generation: ProviderGeneration,
        consumer_id: ConsumerId,
        boundary: i64,
        live_timestamp: i64,
        detached_snapshot: Arc<aeris_market_engine::SeriesSnapshot>,
    }

    fn detached_rithmic_fixture() -> DetachedRithmicFixture {
        let mut coordinator = coordinator();
        let series = series();
        let provider_generation = ProviderGeneration(nonzero(1));
        let consumer_id = consumer(1);
        coordinator
            .engine
            .begin_provider_session("rithmic", provider_generation)
            .expect("provider session begins");
        let epoch = history_epoch(&mut coordinator, &series.provider_id, provider_generation);
        coordinator.history.begin(
            series.clone(),
            epoch,
            super::super::history::tracker::LaneKind::Current,
            None,
            coordinator.clock.as_ref(),
        );
        register(&mut coordinator, consumer_id);
        coordinator
            .engine
            .set_series_demand_with_streams(
                consumer_id,
                generation(1),
                &series,
                StreamRequirements::BARS,
            )
            .expect("series demand installs");
        let bars = minute_bars(0, HISTORY_SERIES_HIGH_WATERMARK);
        coordinator
            .engine
            .install_history(provider_generation, &series, 2, 0, bars.clone())
            .expect("history fills the per-series high watermark");
        let minute = 60_i64 * 1_000_000_000;
        coordinator
            .engine
            .set_viewport(
                consumer_id,
                generation(1),
                Viewport::try_new(1_000 * minute, 1_100 * minute).expect("historical viewport"),
            )
            .expect("historical viewport installs");
        let boundary = bars
            .last()
            .map(|bar| bar.exchange_timestamp_unix_nanos)
            .expect("history boundary");
        let mut live = TradeModel::new(&series, provider_generation, "CME", None)
            .expect("Rithmic live handoff");
        live.seed(2, 0, &bars, None, Some(boundary))
            .expect("live handoff seeds from current history");
        live.connected = true;
        coordinator.series.insert_trade(series.clone(), live);
        assert!(
            coordinator
                .compact_live_series_if_needed(&series, provider_generation)
                .expect("canonical history compacts")
        );
        assert_eq!(
            coordinator.engine.series_bar_count(&series),
            Some(HISTORY_SERIES_TARGET_BARS)
        );
        let detached_snapshot = coordinator
            .engine
            .series_snapshot(&series)
            .expect("detached canonical window");
        DetachedRithmicFixture {
            coordinator,
            series,
            provider_generation,
            consumer_id,
            boundary,
            live_timestamp: boundary + minute,
            detached_snapshot,
        }
    }

    fn current_rejoin_snapshot(live_timestamp: i64) -> HistorySnapshot {
        let current_start = HISTORY_SERIES_HIGH_WATERMARK - INITIAL_HISTORY_BARS;
        HistorySnapshot {
            price_scale: 2,
            quantity_scale: 0,
            bars: minute_bars(
                i64::try_from(current_start).expect("bounded current start"),
                INITIAL_HISTORY_BARS,
            ),
            forming: Some(FormingBar {
                bar: MarketBar {
                    source_sequence: u64::try_from(INITIAL_HISTORY_BARS + 1)
                        .expect("forming sequence"),
                    exchange_timestamp_seconds: live_timestamp.div_euclid(1_000_000_000),
                    exchange_timestamp_unix_nanos: live_timestamp,
                    open: 102,
                    high: 102,
                    low: 102,
                    close: 102,
                    volume: 1,
                },
                trades: None,
            }),
            handoff_boundary_unix_nanos: Some(live_timestamp),
            backwards_exhausted: false,
        }
    }

    #[test]
    fn stale_history_completion_cannot_rejoin_a_detached_newer_session() {
        let mut coordinator = coordinator();
        let selected = series();
        let first_generation = ProviderGeneration(nonzero(1));
        coordinator
            .engine
            .begin_provider_session("rithmic", first_generation)
            .expect("first provider session begins");
        let current = minute_bars(0, 2);
        coordinator
            .engine
            .install_history(first_generation, &selected, 2, 0, current.clone())
            .expect("first generation history installs");
        let first_epoch = history_epoch(&mut coordinator, &selected.provider_id, first_generation);
        coordinator.history.begin(
            selected.clone(),
            first_epoch,
            super::super::history::tracker::LaneKind::Current,
            None,
            coordinator.clock.as_ref(),
        );
        coordinator
            .history
            .set_detached(&selected, first_epoch, true, coordinator.clock.as_ref());
        let before = coordinator
            .engine
            .series_snapshot(&selected)
            .expect("cached first-generation history");

        coordinator
            .engine
            .end_provider_session("rithmic", first_generation)
            .expect("first provider session ends");
        let second_generation = ProviderGeneration(nonzero(2));
        coordinator
            .engine
            .begin_provider_session("rithmic", second_generation)
            .expect("second provider session begins");
        finish_history(
            &mut coordinator,
            &selected,
            first_generation,
            None,
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 0,
                bars: minute_bars(10, 2),
                forming: None,
                handoff_boundary_unix_nanos: Some(11 * 60 * 1_000_000_000),
                backwards_exhausted: false,
            }),
        );

        let after = coordinator
            .engine
            .series_snapshot(&selected)
            .expect("stale completion cannot replace cached history");
        assert_eq!(after.bars, before.bars);
        let second_epoch =
            history_epoch(&mut coordinator, &selected.provider_id, second_generation);
        assert!(!coordinator.history.is_detached(&selected, second_epoch));
        coordinator.prune_history_tracking();
        assert!(!coordinator.history.is_detached(&selected, first_epoch));
    }

    #[test]
    fn failed_covering_recovery_retries_and_rejoins_live_publication() {
        let DetachedRithmicFixture {
            mut coordinator,
            series,
            provider_generation,
            live_timestamp,
            ..
        } = detached_rithmic_fixture();
        let installed = instrument();
        coordinator.catalog.insert(
            (series.provider_id.clone(), series.instrument_id.clone()),
            installed,
        );
        let (history_tx, history_rx) = std::sync::mpsc::sync_channel(1);
        let history_tx = Box::leak(Box::new(history_tx));
        coordinator.providers.records.insert(
            "rithmic",
            ProviderDispatchRecord {
                descriptor: super::super::RITHMIC_DESCRIPTOR,
                history: history_tx,
                lifecycle: None,
                ctrader: None,
                hyperliquid: None,
                tastytrade: None,
                rithmic: None,
                catalog: None,
            },
        );
        finish_history(
            &mut coordinator,
            &series,
            provider_generation,
            None,
            Err("history channel did not finish".into()),
        );
        let epoch = history_epoch(&mut coordinator, &series.provider_id, provider_generation);
        assert!(
            coordinator
                .history
                .pending(
                    &series,
                    epoch,
                    super::super::history::tracker::LaneKind::Current
                )
                .is_some()
        );
        if let Some((deadline, _, _)) = coordinator.history.pending(
            &series,
            epoch,
            super::super::history::tracker::LaneKind::Current,
        ) {
            let clock = Arc::new(super::super::clock::ScriptedClock::new(1_000_000_000));
            // The next retry is driven by the injected monotonic clock.
            clock.advance(
                deadline.saturating_duration_since(coordinator.clock.monotonic())
                    + Duration::from_secs(1),
            );
            coordinator.clock = clock;
        }
        coordinator.retry_history();
        let retried = history_rx
            .try_recv()
            .expect("retry reached the history worker");
        coordinator.history_completed(
            &retried.ticket,
            provider_generation,
            None,
            Ok(current_rejoin_snapshot(live_timestamp)),
        );
        coordinator.publish_lifecycle_live();
        assert!(
            coordinator
                .history
                .pending(
                    &series,
                    epoch,
                    super::super::history::tracker::LaneKind::Current
                )
                .is_none()
        );
        assert!(
            coordinator
                .engine
                .series_snapshot(&series)
                .is_some_and(|snapshot| snapshot.forming),
            "a completed retry returns the canonical series to live publication"
        );
    }

    #[test]
    fn stale_history_completion_is_discarded_without_restarting_retired_generation() {
        let mut coordinator = coordinator();
        let selected = series();
        let first_generation = ProviderGeneration(nonzero(1));
        coordinator
            .engine
            .begin_provider_session("rithmic", first_generation)
            .expect("first provider session begins");
        coordinator
            .engine
            .end_provider_session("rithmic", first_generation)
            .expect("first provider session ends");
        let second_generation = ProviderGeneration(nonzero(2));
        coordinator
            .engine
            .begin_provider_session("rithmic", second_generation)
            .expect("second provider session begins");
        let ticket = history_ticket(&mut coordinator, &selected, first_generation, None);
        coordinator.history.defer_next(
            &selected,
            ticket.provider,
            super::super::history::tracker::LaneKind::Backfill,
            Some(HistoryRange {
                start_unix_nanos: 0,
                end_unix_nanos: 60_000_000_000,
            }),
        );

        assert!(
            coordinator
                .accept_history_completion(
                    &ticket,
                    first_generation,
                    None,
                    Ok(HistorySnapshot {
                        price_scale: 2,
                        quantity_scale: 0,
                        bars: minute_bars(0, 2),
                        forming: None,
                        handoff_boundary_unix_nanos: None,
                        backwards_exhausted: false,
                    }),
                )
                .is_none()
        );
        coordinator.prune_history_tracking();
        assert_eq!(coordinator.history.count(), 0);
    }

    #[test]
    fn history_tracking_cancels_and_prunes_stale_provider_generation() {
        let mut coordinator = coordinator();
        let consumer_id = consumer(1);
        register(&mut coordinator, consumer_id);
        let selected = series();
        let first_generation = ProviderGeneration(nonzero(1));
        coordinator
            .engine
            .begin_provider_session("rithmic", first_generation)
            .expect("first provider session begins");
        coordinator
            .engine
            .set_series_demand(consumer_id, generation(1), &selected)
            .expect("foreground demand subscribes");
        let ticket = history_ticket(&mut coordinator, &selected, first_generation, None);
        let stop = coordinator.history.cancel_token(&ticket);
        coordinator.history.defer_next(
            &selected,
            ticket.provider,
            super::super::history::tracker::LaneKind::Backfill,
            Some(HistoryRange {
                start_unix_nanos: 0,
                end_unix_nanos: 60_000_000_000,
            }),
        );

        coordinator
            .engine
            .end_provider_session("rithmic", first_generation)
            .expect("first provider session ends");
        let second_generation = ProviderGeneration(nonzero(2));
        coordinator
            .engine
            .begin_provider_session("rithmic", second_generation)
            .expect("second provider session begins");
        assert!(coordinator.engine.has_subscription(&selected));

        coordinator.prune_history_tracking();

        assert!(stop.load(Ordering::Acquire));
        assert_eq!(coordinator.history.count(), 0);
    }

    #[test]
    fn historical_detach_suppresses_live_tail_and_current_rejoin_restores_it() {
        let DetachedRithmicFixture {
            mut coordinator,
            series,
            provider_generation,
            consumer_id,
            boundary,
            live_timestamp,
            detached_snapshot,
        } = detached_rithmic_fixture();
        let minute = 60_i64 * 1_000_000_000;
        let epoch = history_epoch(&mut coordinator, &series.provider_id, provider_generation);
        assert!(coordinator.history.is_detached(&series, epoch));
        assert!(
            !coordinator.series_live_if_ready(&series),
            "a historical canonical window cannot advertise live readiness"
        );
        let coverage_before = coordinator
            .series
            .trade(&series)
            .and_then(TradeModel::coverage);
        coordinator
            .series
            .trade_lifecycle_mut(&series)
            .expect("live handoff remains allocated")
            .accept_trade(&rithmic_trade_for_series(&series, 1, live_timestamp))
            .expect("live handoff keeps ingesting while detached");
        assert_eq!(
            coordinator
                .series
                .trade(&series)
                .and_then(TradeModel::coverage)
                .map(|(_, end)| end),
            coverage_before.map(|(_, end)| end),
            "a detached seam buffers trades rather than mutating the stale live model"
        );
        assert_eq!(
            coordinator
                .series
                .trade(&series)
                .expect("retained trade model")
                .buffered
                .len(),
            1
        );
        coordinator.publish_lifecycle_live();
        let after_suppressed_live = coordinator
            .engine
            .series_snapshot(&series)
            .expect("canonical window remains detached");
        assert_eq!(after_suppressed_live.bars, detached_snapshot.bars);
        coordinator
            .engine
            .set_viewport(
                consumer_id,
                generation(1),
                Viewport::try_new(boundary - 100 * minute, live_timestamp + minute)
                    .expect("recent viewport"),
            )
            .expect("recent viewport installs");
        finish_history(
            &mut coordinator,
            &series,
            provider_generation,
            None,
            Ok(current_rejoin_snapshot(live_timestamp)),
        );
        assert!(!coordinator.history.is_detached(&series, epoch));
        coordinator.publish_lifecycle_live();
        let rejoined = coordinator
            .engine
            .series_snapshot(&series)
            .expect("current canonical window rejoins live");
        assert!(rejoined.forming);
        assert_eq!(
            rejoined
                .bars
                .last()
                .map(|bar| bar.exchange_timestamp_unix_nanos),
            Some(live_timestamp)
        );
        assert!(rejoined.bars.len() <= HISTORY_SERIES_HIGH_WATERMARK);
    }

    #[test]
    fn provider_state_outbox_is_canonical_and_replaced_by_newer_session_demand() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());

        let mut selected = instrument();
        selected.session_generation = 2;
        coordinator
            .install_provider_instrument(&selected)
            .expect("provider session installs");
        coordinator
            .accept_series_demand(
                client(1),
                &series(),
                StreamRequirements::BARS,
                &DemandWaiter {
                    consumer_id: consumer,
                    generation: generation(7),
                    started_at: Instant::now(),
                },
            )
            .expect("first demand installs");

        assert!(matches!(
            coordinator
                .events
                .get(&consumer)
                .expect("consumer outbox")
                .provider,
            Some(MarketRuntimeEvent::ProviderState(ref state)) if state.generation == 2
        ));
        assert!(
            coordinator
                .engine
                .set_provider_health(
                    "rithmic",
                    ProviderGeneration(nonzero(1)),
                    ProviderHealth::Online,
                )
                .is_err(),
            "a retired provider generation cannot mutate canonical provider status"
        );

        let mut replacement = selected;
        replacement.session_generation = 3;
        coordinator
            .install_provider_instrument(&replacement)
            .expect("newer provider session installs");
        coordinator
            .accept_series_demand(
                client(1),
                &series(),
                StreamRequirements::BARS,
                &DemandWaiter {
                    consumer_id: consumer,
                    generation: generation(8),
                    started_at: Instant::now(),
                },
            )
            .expect("replacement demand installs");

        assert!(matches!(
            coordinator
                .events
                .get(&consumer)
                .expect("consumer outbox")
                .provider,
            Some(MarketRuntimeEvent::ProviderState(ref state)) if state.generation == 3
        ));
    }

    #[test]
    fn study_registration_uses_shared_engine_lease_and_removal_releases_runtime_work() {
        let mut coordinator = coordinator();
        let owner = consumer(1);
        register(&mut coordinator, owner);
        coordinator.events.insert(owner, ConsumerEvents::default());
        let selected = instrument();
        coordinator
            .install_provider_instrument(&selected)
            .expect("instrument installs");
        let selected_series = series();
        coordinator
            .engine
            .install_history(
                ProviderGeneration(nonzero(selected.session_generation)),
                &selected_series,
                2,
                0,
                minute_bars(0, 3),
            )
            .expect("canonical history installs");

        let study_id = coordinator
            .handle_register_study(
                client(1),
                owner,
                study_registration(selected_series.clone()),
            )
            .expect("study registers");
        assert_eq!(coordinator.studies.owner(study_id), Some(owner));
        assert_eq!(coordinator.engine.data_lease_count(), 1);
        assert_eq!(
            coordinator
                .engine
                .subscription_status(&selected_series)
                .map(|status| (status.consumer_count, status.streams)),
            Some((0, StreamRequirements::BARS))
        );
        assert!(coordinator.series.trade_contains_key(&selected_series));
        assert!(matches!(
            coordinator
                .events
                .get_mut(&owner)
                .expect("consumer outbox")
                .pop(),
            Some(MarketRuntimeEvent::StudyOutputSnapshot(snapshot))
                if snapshot.consumer_id == owner
                    && snapshot.study_id == study_id
                    && snapshot.output_id == study_id.output(0)
                    && snapshot.study_identifier == "test.study"
                    && snapshot.output.title == "Test Study"
                    && snapshot.series.generation() == 1
        ));

        assert_eq!(
            coordinator
                .handle_remove_study(client(1), study_id)
                .expect("study removes"),
            vec![study_id]
        );
        assert!(coordinator.studies.is_empty());
        assert_eq!(coordinator.engine.data_lease_count(), 0);
        assert!(!coordinator.engine.has_subscription(&selected_series));
        assert!(!coordinator.series.trade_contains_key(&selected_series));
        assert!(matches!(
            coordinator
                .events
                .get_mut(&owner)
                .expect("consumer outbox")
                .pop(),
            Some(MarketRuntimeEvent::StudyRemoved(removed))
                if removed.consumer_id == owner && removed.study_ids == [study_id]
        ));
    }

    #[test]
    fn study_reinitialization_preserves_runtime_identity_and_republishes_output() {
        let mut coordinator = coordinator();
        let owner = consumer(1);
        register(&mut coordinator, owner);
        coordinator.events.insert(owner, ConsumerEvents::default());

        let selected = instrument();
        coordinator
            .install_provider_instrument(&selected)
            .expect("instrument installs");
        let selected_series = series();
        coordinator
            .engine
            .install_history(
                ProviderGeneration(nonzero(selected.session_generation)),
                &selected_series,
                2,
                0,
                minute_bars(0, 3),
            )
            .expect("canonical history installs");

        let study_id = coordinator
            .handle_register_study(
                client(1),
                owner,
                study_registration(selected_series.clone()),
            )
            .expect("study registers");
        assert!(matches!(
            coordinator
                .events
                .get_mut(&owner)
                .expect("consumer outbox")
                .pop(),
            Some(MarketRuntimeEvent::StudyOutputSnapshot(_))
        ));

        assert_eq!(
            coordinator
                .handle_reinitialize_study(
                    client(1),
                    study_id,
                    study_registration(selected_series.clone()),
                )
                .expect("study reinitializes"),
            vec![study_id]
        );
        assert_eq!(coordinator.studies.owner(study_id), Some(owner));
        assert_eq!(coordinator.engine.data_lease_count(), 1);
        assert!(coordinator.engine.has_subscription(&selected_series));
        assert!(matches!(
            coordinator
                .events
                .get_mut(&owner)
                .expect("consumer outbox")
                .pop(),
            Some(MarketRuntimeEvent::StudyOutputsInvalidated(invalidated))
                if invalidated.consumer_id == owner && invalidated.study_ids == [study_id]
        ));
        assert!(matches!(
            coordinator
                .events
                .get_mut(&owner)
                .expect("consumer outbox")
                .pop(),
            Some(MarketRuntimeEvent::StudyOutputSnapshot(snapshot))
                if snapshot.study_id == study_id
                    && snapshot.output_id == study_id.output(0)
                    && snapshot.series.generation() == 2
        ));
    }

    #[test]
    fn study_output_from_previous_selection_never_reaches_consumer() {
        let mut coordinator = coordinator();
        let owner = consumer(1);
        let client_id = client(1);
        coordinator.attached.insert(client_id);
        coordinator
            .handle_register(ConsumerIdentity {
                client_id,
                workspace_id: WorkspaceId(nonzero(1)),
                consumer_id: owner,
            })
            .expect("consumer registers");
        let selected = instrument();
        coordinator
            .install_provider_instrument(&selected)
            .expect("instrument installs");
        let key = series();
        coordinator
            .engine
            .set_series_demand(owner, generation(1), &key)
            .expect("initial selection");
        coordinator
            .engine
            .install_history(
                ProviderGeneration(nonzero(selected.session_generation)),
                &key,
                2,
                0,
                minute_bars(0, 3),
            )
            .expect("canonical bars");
        coordinator
            .handle_register_study(client_id, owner, study_registration(key.clone()))
            .expect("study publishes");
        assert!(
            !coordinator
                .events
                .get(&owner)
                .expect("outbox")
                .study_outputs
                .is_empty()
        );
        coordinator
            .engine
            .set_series_demand(owner, generation(2), &key)
            .expect("new selection");
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_drain_client(client_id, vec![(owner, 32)], &reply);
        let delivered = result.recv().expect("reply").expect("authorized");
        assert!(
            delivered.iter().all(|(_, event)| !matches!(
                &event.value,
                MarketRuntimeEvent::StudyOutputSnapshot(_)
            )),
            "retired study output must not be delivered after selection changes"
        );
    }

    #[test]
    fn failed_study_reinitialization_restores_runtime_state_without_public_invalidation() {
        let mut coordinator = coordinator();
        let owner = consumer(1);
        register(&mut coordinator, owner);
        coordinator.events.insert(owner, ConsumerEvents::default());

        let selected = instrument();
        coordinator
            .install_provider_instrument(&selected)
            .expect("instrument installs");
        let selected_series = series();
        coordinator
            .engine
            .install_history(
                ProviderGeneration(nonzero(selected.session_generation)),
                &selected_series,
                2,
                0,
                minute_bars(0, 3),
            )
            .expect("canonical history installs");
        let study_id = coordinator
            .handle_register_study(
                client(1),
                owner,
                study_registration(selected_series.clone()),
            )
            .expect("study registers");
        assert!(matches!(
            coordinator
                .events
                .get_mut(&owner)
                .expect("consumer outbox")
                .pop(),
            Some(MarketRuntimeEvent::StudyOutputSnapshot(_))
        ));
        let committed = coordinator
            .studies
            .output_series(study_id.output(0))
            .expect("committed output")
            .clone();

        assert!(
            coordinator
                .handle_reinitialize_study(
                    client(1),
                    study_id,
                    failing_study_registration(selected_series.clone()),
                )
                .is_err()
        );
        assert_eq!(
            coordinator.studies.output_series(study_id.output(0)),
            Some(&committed)
        );
        assert_eq!(coordinator.studies.owner(study_id), Some(owner));
        assert_eq!(coordinator.engine.data_lease_count(), 1);
        assert!(coordinator.engine.has_subscription(&selected_series));
        assert!(
            coordinator
                .events
                .get_mut(&owner)
                .expect("consumer outbox")
                .pop()
                .is_none()
        );
    }

    #[test]
    fn removing_consumer_also_releases_every_owned_study_lease() {
        let mut coordinator = coordinator();
        let owner = consumer(1);
        register(&mut coordinator, owner);
        let selected = instrument();
        coordinator
            .install_provider_instrument(&selected)
            .expect("instrument installs");
        let selected_series = series();
        coordinator
            .engine
            .install_history(
                ProviderGeneration(nonzero(selected.session_generation)),
                &selected_series,
                2,
                0,
                minute_bars(0, 3),
            )
            .expect("canonical history installs");
        coordinator
            .handle_register_study(
                client(1),
                owner,
                study_registration(selected_series.clone()),
            )
            .expect("study registers");

        coordinator
            .handle_remove(client(1), owner)
            .expect("consumer removes");
        assert!(coordinator.studies.is_empty());
        assert_eq!(coordinator.engine.data_lease_count(), 0);
        assert!(!coordinator.engine.has_subscription(&selected_series));
        assert!(!coordinator.series.trade_contains_key(&selected_series));
    }

    #[test]
    fn registered_native_study_survives_newer_provider_session_without_duplicate_demand() {
        let mut coordinator = coordinator();
        let owner = consumer(1);
        register(&mut coordinator, owner);
        coordinator.events.insert(owner, ConsumerEvents::default());

        let mut selected = instrument();
        selected.session_generation = 2;
        coordinator
            .install_provider_instrument(&selected)
            .expect("provider session installs");
        let selected_series = series();
        coordinator
            .engine
            .install_history(
                ProviderGeneration(nonzero(2)),
                &selected_series,
                2,
                0,
                minute_bars(0, 3),
            )
            .expect("initial session history installs");
        let study_id = coordinator
            .handle_register_study(
                client(1),
                owner,
                study_registration(selected_series.clone()),
            )
            .expect("study registers");
        assert_eq!(coordinator.engine.data_lease_count(), 1);

        let mut replacement = selected;
        replacement.session_generation = 3;
        coordinator
            .install_provider_instrument(&replacement)
            .expect("newer provider session installs");
        assert_eq!(
            coordinator
                .engine
                .provider_status("rithmic")
                .and_then(|status| status.generation),
            Some(ProviderGeneration(nonzero(3)))
        );
        assert_eq!(coordinator.studies.owner(study_id), Some(owner));
        assert_eq!(coordinator.engine.data_lease_count(), 1);
        assert!(coordinator.engine.has_subscription(&selected_series));
        assert_eq!(
            coordinator
                .engine
                .subscription_status(&selected_series)
                .map(|status| (status.consumer_count, status.streams)),
            Some((0, StreamRequirements::BARS))
        );
    }

    #[test]
    fn non_bar_native_study_preserves_stream_demand_across_newer_provider_session() {
        let mut coordinator = coordinator();
        let owner = consumer(1);
        register(&mut coordinator, owner);
        coordinator.events.insert(owner, ConsumerEvents::default());

        let mut selected = instrument();
        selected.session_generation = 2;
        coordinator
            .install_provider_instrument(&selected)
            .expect("provider session installs");
        let selected_series = series();
        coordinator
            .engine
            .install_history(
                ProviderGeneration(nonzero(2)),
                &selected_series,
                2,
                0,
                minute_bars(0, 3),
            )
            .expect("initial session history installs");
        let streams = StreamRequirements::BARS
            .with(MarketStream::Trades)
            .with(MarketStream::Quotes)
            .with(MarketStream::Depth);
        let study_id = coordinator
            .handle_register_study(
                client(1),
                owner,
                study_registration_with_streams(selected_series.clone(), streams),
            )
            .expect("non-bar study registers");
        assert_eq!(coordinator.engine.data_lease_count(), 1);
        assert_eq!(
            coordinator
                .engine
                .subscription_status(&selected_series)
                .map(|status| (status.consumer_count, status.streams)),
            Some((0, streams))
        );

        let mut replacement = selected;
        replacement.session_generation = 3;
        coordinator
            .install_provider_instrument(&replacement)
            .expect("newer provider session installs");
        assert_eq!(
            coordinator
                .engine
                .provider_status("rithmic")
                .and_then(|status| status.generation),
            Some(ProviderGeneration(nonzero(3)))
        );
        assert_eq!(coordinator.studies.owner(study_id), Some(owner));
        assert_eq!(coordinator.engine.data_lease_count(), 1);
        assert!(coordinator.engine.has_subscription(&selected_series));
        assert_eq!(
            coordinator
                .engine
                .subscription_status(&selected_series)
                .map(|status| (status.consumer_count, status.streams)),
            Some((0, streams))
        );
    }

    #[test]
    fn provider_recovery_fences_and_clears_non_bar_study_views() {
        let (mut coordinator, selected, input) = ready_non_bar_study_fixture();
        let live = study_live_market_data(
            &coordinator.engine,
            &coordinator.order_books,
            &coordinator.series,
            &input,
        )
        .expect("online ready series exposes current live market state");
        assert!(live.quote().is_some());
        assert!(live.trades().is_some_and(|trades| !trades.is_empty()));
        assert!(live.depth().is_some());

        for recovery_reason in [
            RecoveryReason::DetachedWindow,
            RecoveryReason::ProviderReconnected,
        ] {
            coordinator
                .series
                .trade_lifecycle_mut(&input.series)
                .expect("series handoff")
                .recover_trade(recovery_reason);
            assert_eq!(
                coordinator
                    .engine
                    .provider_status("rithmic")
                    .map(|status| status.health),
                Some(ProviderHealth::Online),
                "series recovery must not require taking the provider session offline"
            );
            assert!(
                study_live_market_data(
                    &coordinator.engine,
                    &coordinator.order_books,
                    &coordinator.series,
                    &input,
                )
                .is_none(),
                "bar-aligned non-bar views must be fenced until this series is history-ready"
            );
        }
        set_trade_history_ready(&mut coordinator, &input.series);
        assert!(
            study_live_market_data(
                &coordinator.engine,
                &coordinator.order_books,
                &coordinator.series,
                &input,
            )
            .is_some(),
            "series live views resume after the history seam is ready"
        );

        coordinator.mark_lifecycle_recovering(
            "rithmic",
            1,
            super::super::LiveModel::TradeBuilt,
            "test recovery",
        );
        assert!(
            study_live_market_data(
                &coordinator.engine,
                &coordinator.order_books,
                &coordinator.series,
                &input,
            )
            .is_none(),
            "recovering provider must not expose point-in-time live state to studies"
        );
        let order_book = coordinator
            .order_books
            .get(&(selected.provider, selected.instrument_id))
            .expect("canonical order book remains retained");
        assert!(order_book.top_of_book.is_none());
        assert!(order_book.recent_trades.is_empty());
        assert_eq!(order_book.book.state(), OrderBookState::Stale);
    }

    #[test]
    fn rithmic_trade_recovery_is_fenced_before_non_bar_study_execution() {
        let (mut coordinator, selected, selected_series, study_id) = trade_recovery_study_fixture();
        let live = coordinator
            .series
            .trade(&selected_series)
            .expect("series live handoff exists");
        assert!(live.connected);
        assert!(
            coordinator
                .series
                .trade_lifecycle(&selected_series)
                .is_some_and(SeriesLifecycle::history_ready)
        );

        // Start a new forming minute with the maximum representable volume, then
        // publish it so the study's canonical primary timeline includes that row.
        let mut first =
            rithmic_trade_for_series(&selected_series, 1, 180_i64.saturating_mul(1_000_000_000));
        first.quantity = i64::MAX;
        coordinator.accept_lifecycle_trade("rithmic", 1, &first);
        assert!(matches!(
            coordinator
                .series
                .trade_lifecycle_mut(&selected_series)
                .map(|live| live.state()),
            Some(SeriesState::Live {
                observed_live: true,
                ..
            })
        ));
        coordinator.publish_lifecycle_live();
        let generation_before_failure = coordinator
            .studies
            .output_series(study_id.output(0))
            .expect("study output after first live bar")
            .generation();
        assert_eq!(
            coordinator
                .engine
                .series_snapshot(&selected_series)
                .and_then(|snapshot| snapshot.bars.last().copied())
                .map(|bar| bar.exchange_timestamp_unix_nanos),
            Some(180_i64.saturating_mul(1_000_000_000))
        );

        // This trade overflows the live bar volume. It also lands inside the
        // canonical 180-second row, so publishing its non-bar change before
        // classifying aggregation recovery would incorrectly execute the study
        // once against a bar the handoff has just declared stale.
        let second =
            rithmic_trade_for_series(&selected_series, 2, 181_i64.saturating_mul(1_000_000_000));
        coordinator.accept_lifecycle_trade("rithmic", 1, &second);
        assert!(matches!(
            coordinator
                .series
                .trade_lifecycle_mut(&selected_series)
                .map(|live| live.state()),
            Some(SeriesState::Recovering { .. })
        ));

        assert_eq!(
            coordinator
                .engine
                .provider_status("rithmic")
                .map(|status| status.health),
            Some(ProviderHealth::Online),
            "one-series aggregation recovery must not replace provider-session ownership"
        );
        assert!(
            !coordinator
                .series
                .trade_lifecycle(&selected_series)
                .expect("series handoff remains retained")
                .history_ready()
        );
        assert_eq!(
            coordinator
                .studies
                .output_series(study_id.output(0))
                .expect("last committed study output remains available")
                .generation(),
            generation_before_failure,
            "the recovery-triggering trade must be fenced before non-bar study execution"
        );
        assert!(
            coordinator
                .order_books
                .get(&(selected.provider, selected.instrument_id))
                .is_some_and(|book| book.recent_trades.len() >= 2),
            "canonical point-in-time trade state still accepts the provider event"
        );
    }

    #[test]
    fn mtf_non_bar_wave_waits_for_every_market_dependency_to_be_history_ready() {
        let mut coordinator = coordinator();
        let owner = consumer(1);
        register(&mut coordinator, owner);
        coordinator.events.insert(owner, ConsumerEvents::default());

        let selected = instrument();
        coordinator
            .install_provider_instrument(&selected)
            .expect("provider session installs");
        let provider_generation = ProviderGeneration(nonzero(selected.session_generation));
        coordinator
            .engine
            .set_provider_health("rithmic", provider_generation, ProviderHealth::Online)
            .expect("provider is online");

        let secondary = series();
        let mut primary = secondary.clone();
        primary.period = BarPeriod::time(300).expect("five-minute period");
        coordinator
            .engine
            .install_history(provider_generation, &primary, 2, 0, minute_bars(0, 1))
            .expect("primary history installs");
        coordinator
            .engine
            .install_history(provider_generation, &secondary, 2, 0, minute_bars(0, 3))
            .expect("secondary history installs");

        let mut definition = study_definition(primary.clone());
        definition.identifier = "test.mtf.recovery-fence".to_string();
        definition
            .dependencies
            .push(StudyDependency::Market(StudyMarketInput {
                series: secondary.clone(),
                streams: StreamRequirements::BARS.with(MarketStream::Quotes),
            }));
        let study_id = coordinator
            .handle_register_study(
                client(1),
                owner,
                NativeStudyRegistration {
                    settings: StudySettings::defaults(&definition.settings)
                        .expect("valid defaults"),
                    definition,
                    program: NativeStudyProgram {
                        calculate: calculate_test_study,
                        state_factory: None,
                    },
                },
            )
            .expect("MTF study registers");
        assert_rithmic_history_ready(&coordinator, &primary);
        assert_rithmic_history_ready(&coordinator, &secondary);
        let generation_before_recovery = coordinator
            .studies
            .output_series(study_id.output(0))
            .expect("initial MTF output")
            .generation();

        coordinator
            .series
            .trade_lifecycle_mut(&primary)
            .expect("primary handoff remains installed")
            .recover_trade(RecoveryReason::ProviderReconnected);
        let quote_timestamp = 30_i64.saturating_mul(1_000_000_000);
        coordinator.provider_quote(
            "rithmic",
            selected.session_generation,
            &quote_for_instrument(&selected, 1, quote_timestamp, 100),
        );
        assert_eq!(
            coordinator
                .studies
                .output_series(study_id.output(0))
                .expect("last committed MTF output remains available")
                .generation(),
            generation_before_recovery,
            "a quote-driven MTF wave must not execute against a recovering BARS-only primary"
        );

        set_trade_history_ready(&mut coordinator, &primary);
        coordinator.provider_quote(
            "rithmic",
            selected.session_generation,
            &quote_for_instrument(&selected, 2, quote_timestamp, 101),
        );
        assert!(
            coordinator
                .studies
                .output_series(study_id.output(0))
                .expect("MTF output resumes")
                .generation()
                > generation_before_recovery,
            "the same quote-driven wave resumes once every market dependency is ready"
        );
    }

    #[test]
    fn non_bar_wave_waits_for_transitive_output_market_ancestry_to_be_ready() {
        let OutputAncestryRecoveryFixture {
            mut coordinator,
            selected,
            primary,
            secondary,
            consumer_study,
        } = output_ancestry_recovery_fixture();
        assert_rithmic_history_ready(&coordinator, &primary);
        assert_rithmic_history_ready(&coordinator, &secondary);
        let generation_before_recovery = coordinator
            .studies
            .output_series(consumer_study.output(0))
            .expect("initial output-backed MTF output")
            .generation();

        coordinator
            .series
            .trade_lifecycle_mut(&primary)
            .expect("producer handoff remains installed")
            .recover_trade(RecoveryReason::ProviderReconnected);
        let quote_timestamp = 30_i64.saturating_mul(1_000_000_000);
        coordinator.provider_quote(
            "rithmic",
            selected.session_generation,
            &quote_for_instrument(&selected, 1, quote_timestamp, 100),
        );
        assert_eq!(
            coordinator
                .studies
                .output_series(consumer_study.output(0))
                .expect("last committed output-backed MTF output remains")
                .generation(),
            generation_before_recovery,
            "a ready quote dependency must not bypass recovering producer market ancestry"
        );

        set_trade_history_ready(&mut coordinator, &primary);
        coordinator.provider_quote(
            "rithmic",
            selected.session_generation,
            &quote_for_instrument(&selected, 2, quote_timestamp, 101),
        );
        assert!(
            coordinator
                .studies
                .output_series(consumer_study.output(0))
                .expect("output-backed MTF output resumes")
                .generation()
                > generation_before_recovery,
            "the next non-bar event executes after transitive market ancestry is ready"
        );
    }

    #[test]
    fn newer_provider_session_demand_publishes_retained_older_snapshot_as_partial_baseline() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());

        let mut selected = instrument();
        selected.session_generation = 2;
        coordinator
            .install_provider_instrument(&selected)
            .expect("new provider session installs");
        let selected_series = series();
        let waiter = DemandWaiter {
            consumer_id: consumer,
            generation: generation(7),
            started_at: Instant::now(),
        };
        let (current_provider_generation, _) = coordinator
            .accept_series_demand(
                client(1),
                &selected_series,
                StreamRequirements::BARS,
                &waiter,
            )
            .expect("demand installs");
        assert_eq!(current_provider_generation.0.get(), 2);

        let retained = aeris_market_engine::ConsumerPublication {
            consumer_id: consumer,
            generation: generation(7),
            publication_generation: 1,
            snapshot: Arc::new(aeris_market_engine::SeriesSnapshot {
                series: selected_series.clone(),
                provider_generation: ProviderGeneration(nonzero(1)),
                publication_generation: 1,
                price_scale: 2,
                quantity_scale: 0,
                forming: false,
                bars: Arc::from([aeris_market_data::MarketBar {
                    source_sequence: 1,
                    exchange_timestamp_seconds: 60,
                    exchange_timestamp_unix_nanos: 60_000_000_000,
                    open: 100,
                    high: 110,
                    low: 90,
                    close: 105,
                    volume: 7,
                }]),
            }),
        };
        coordinator
            .publish_cached_demand(
                &selected_series,
                current_provider_generation,
                &waiter,
                &retained,
            )
            .expect("retained baseline publishes while current coverage refreshes");

        let events = coordinator.events.get(&consumer).expect("consumer outbox");
        assert!(matches!(
            events.series.front(),
            Some(MarketRuntimeEvent::SeriesSnapshot(snapshot))
                if snapshot.snapshot.provider_generation.0.get() == 1
        ));
        assert!(matches!(
            events.series_state,
            Some(MarketRuntimeEvent::SeriesState(ref state))
                if state.state == super::super::SeriesLoadState::Partial
        ));
    }

    #[test]
    fn provider_connected_after_history_promotes_rithmic_series_live() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        let selected = instrument();
        let selected_series = series();
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        coordinator
            .install_provider_instrument(&selected)
            .expect("instrument installs");
        coordinator
            .engine
            .set_series_demand_with_streams(
                consumer,
                generation(7),
                &selected_series,
                StreamRequirements::BARS,
            )
            .expect("demand installs");
        let provider_generation = ProviderGeneration(nonzero(1));
        let live = TradeModel::new(&selected_series, provider_generation, "CME", None)
            .expect("live handoff");
        coordinator
            .series
            .insert_trade(selected_series.clone(), live);
        set_trade_history_ready(&mut coordinator, &selected_series);

        coordinator.mark_lifecycle_online("rithmic", 1, super::super::LiveModel::TradeBuilt);
        assert!(matches!(
            coordinator
                .series
                .trade_lifecycle_mut(&selected_series)
                .map(|live| live.state()),
            Some(SeriesState::Live { .. })
        ));

        assert!(matches!(
            coordinator
                .events
                .get(&consumer)
                .and_then(|events| events.series_state.as_ref()),
            Some(MarketRuntimeEvent::SeriesState(state))
                if state.state == super::super::SeriesLoadState::Live
        ));
    }

    #[test]
    fn provider_connected_after_history_promotes_hyperliquid_series_live() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        let selected = hyperliquid_instrument();
        let selected_series = hyperliquid_series();
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        coordinator
            .install_provider_instrument(&selected)
            .expect("instrument installs");
        coordinator
            .engine
            .set_series_demand_with_streams(
                consumer,
                generation(7),
                &selected_series,
                StreamRequirements::BARS,
            )
            .expect("demand installs");
        let provider_generation = ProviderGeneration(nonzero(1));
        let mut live = SeriesLifecycle::new(
            selected_series.clone(),
            provider_generation,
            "BTC",
            "1m",
            super::super::CandleGapPolicy::Contiguous,
        );
        live.state = SeriesState::Live {
            last_progress: live.clock.monotonic(),
            observed_live: false,
        };
        coordinator.series.insert_candle(selected_series, live);

        coordinator.mark_lifecycle_online(
            "hyperliquid",
            1,
            super::super::LiveModel::ProviderCandles,
        );

        assert!(matches!(
            coordinator
                .events
                .get(&consumer)
                .and_then(|events| events.series_state.as_ref()),
            Some(MarketRuntimeEvent::SeriesState(state))
                if state.state == super::super::SeriesLoadState::Live
        ));
    }

    #[test]
    fn scripted_candle_publication_reports_seed_live_and_reconnect_in_order() {
        use super::super::clock::{Clock, ScriptedClock};
        use super::super::lifecycle::SeriesInput;
        use crate::SeriesPresentationStatus as Status;
        let clock = Arc::new(ScriptedClock::new(1_000_000_000));
        let mut coordinator = coordinator();
        coordinator.clock = Arc::clone(&clock) as Arc<dyn Clock>;
        let consumer = consumer(1);
        let selected = hyperliquid_instrument();
        let series = hyperliquid_series();
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        coordinator
            .install_provider_instrument(&selected)
            .expect("instrument installs");
        coordinator
            .engine
            .set_series_demand_with_streams(
                consumer,
                generation(7),
                &series,
                StreamRequirements::BARS,
            )
            .expect("demand installs");
        let provider_generation = ProviderGeneration(nonzero(1));
        let epoch = history_epoch(&mut coordinator, "hyperliquid", provider_generation);
        coordinator.series.insert_candle(
            series.clone(),
            SeriesLifecycle::with_clock(
                series.clone(),
                provider_generation,
                "BTC",
                "1m",
                super::super::CandleGapPolicy::Contiguous,
                clock.clone(),
            ),
        );
        let published = |coordinator: &Coordinator<'_>| {
            coordinator
                .events
                .get(&consumer)
                .and_then(|events| events.series_state.as_ref())
                .and_then(|event| match event {
                    MarketRuntimeEvent::SeriesState(state) => state.presentation,
                    _ => None,
                })
        };
        coordinator.broadcast_series_resolution_for(
            &series,
            super::super::SeriesLoadState::Resolving,
            None,
        );
        assert_eq!(published(&coordinator), Some(Status::Loading));
        coordinator.transition_candle_series(&series, SeriesInput::Online(epoch));
        assert_eq!(published(&coordinator), Some(Status::ConnectingLive));
        coordinator.transition_candle_series(&series, SeriesInput::Seeded(None));
        assert_eq!(published(&coordinator), Some(Status::ConnectingLive));
        clock.advance(Duration::from_secs(1));
        coordinator.transition_candle_series(&series, SeriesInput::Candle);
        assert_eq!(published(&coordinator), Some(Status::Live));
        coordinator.transition_candle_series(&series, SeriesInput::ProviderOffline(epoch));
        assert_eq!(published(&coordinator), Some(Status::Recovering));
        let mut epochs = aeris_market_engine::fence::EpochSequence::<
            aeris_market_engine::fence::ProviderTag,
        >::default();
        epochs.next();
        let replacement = epochs.next();
        coordinator.transition_candle_series(&series, SeriesInput::Online(replacement));
        assert_eq!(published(&coordinator), Some(Status::Recovering));
        coordinator.transition_candle_series(&series, SeriesInput::HistoryEmpty(None));
        assert_eq!(published(&coordinator), Some(Status::AwaitingData));
    }

    #[test]
    fn watchdog_ctrader_candles_recover_only_while_contract_hours_are_open() {
        use super::super::clock::{Clock, ScriptedClock};
        use chrono::{DateTime, Utc};
        for (at, expect_recovery) in [
            ("2026-08-17T14:00:00Z", true),
            ("2026-08-22T14:00:00Z", false),
        ] {
            let at_nanos = at
                .parse::<DateTime<Utc>>()
                .expect("UTC instant")
                .timestamp_nanos_opt()
                .expect("timestamp");
            let clock = Arc::new(ScriptedClock::new(at_nanos));
            let mut coordinator = coordinator();
            coordinator.clock = Arc::clone(&clock) as Arc<dyn Clock>;
            let installed = ctrader_watchdog_fixture();
            let selected = BarSeriesKey {
                provider_id: installed.provider.clone(),
                instrument_id: installed.instrument_id.clone(),
                entitlement_id: installed.entitlement_id.clone(),
                period: BarPeriod::time(60).expect("minute"),
                definition_version: 1,
            };
            let provider_generation = ProviderGeneration(nonzero(1));
            coordinator
                .engine
                .begin_provider_session("ctrader", provider_generation)
                .expect("provider begins");
            coordinator
                .engine
                .set_provider_health("ctrader", provider_generation, ProviderHealth::Online)
                .expect("provider online");
            let consumer = consumer(1);
            register(&mut coordinator, consumer);
            coordinator
                .engine
                .set_series_demand_with_streams(
                    consumer,
                    generation(1),
                    &selected,
                    StreamRequirements::BARS,
                )
                .expect("series demanded");
            let (history_tx, history_rx) = std::sync::mpsc::sync_channel(2);
            coordinator.providers.records.insert(
                "ctrader",
                super::super::ProviderDispatchRecord {
                    descriptor: super::super::ctrader::DESCRIPTOR,
                    history: Box::leak(Box::new(history_tx)),
                    lifecycle: None,
                    ctrader: None,
                    hyperliquid: None,
                    tastytrade: None,
                    rithmic: None,
                    catalog: None,
                },
            );
            coordinator.catalog.insert(
                (installed.provider.clone(), installed.instrument_id.clone()),
                installed.clone(),
            );
            coordinator.calendar.resolve(&installed, None);
            let mut live = SeriesLifecycle::with_clock(
                selected.clone(),
                provider_generation,
                "EURUSD",
                "1m",
                super::super::CandleGapPolicy::SessionGapsAllowed,
                Arc::clone(&clock) as Arc<dyn Clock>,
            );
            live.connected = true;
            live.state = SeriesState::Live {
                last_progress: live.clock.monotonic(),
                observed_live: false,
            };
            coordinator.series.insert_candle(selected.clone(), live);
            clock.advance(super::super::watchdog::OPEN_STALL_THRESHOLD + Duration::from_secs(1));
            coordinator.recover_stalled_live_series();
            assert_eq!(history_rx.try_recv().is_ok(), expect_recovery, "{at}");
            coordinator.recover_stalled_live_series();
            assert!(
                history_rx.try_recv().is_err(),
                "no duplicate recovery: {at}"
            );
            assert_eq!(
                coordinator
                    .series
                    .candle(&selected)
                    .expect("candle series")
                    .awaiting_history(),
                expect_recovery,
                "{at}"
            );
        }
    }

    fn ctrader_watchdog_fixture() -> InstallProviderInstrument {
        let mut installed = hyperliquid_instrument();
        installed.provider = "ctrader".into();
        installed.instrument_id = "instrument:ctrader:FX:EURUSD".into();
        installed.provider_symbol = "EURUSD".into();
        installed.venue_id = String::new();
        installed.contract_metadata = Some(Box::new(aeris_contracts::ProviderContractMetadata {
            session_hours: (1..=5)
                .map(|weekday| aeris_contracts::ProviderSessionHours {
                    weekday,
                    open_seconds: 0,
                    close_seconds: 86_400,
                    timezone: "UTC".into(),
                })
                .collect(),
            ..Default::default()
        }));
        installed
    }

    #[test]
    fn unreferenced_series_evicts_but_background_consumer_history_is_retained() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        let first = hyperliquid_series();
        let mut second = first.clone();
        second.instrument_id = "instrument:hyperliquid:ETH".to_string();

        coordinator
            .engine
            .begin_provider_session("hyperliquid", ProviderGeneration(nonzero(1)))
            .expect("provider starts");
        coordinator
            .engine
            .set_provider_health(
                "hyperliquid",
                ProviderGeneration(nonzero(1)),
                ProviderHealth::Online,
            )
            .expect("provider online");
        coordinator
            .engine
            .set_series_demand_with_streams(
                consumer,
                generation(1),
                &first,
                StreamRequirements::BARS,
            )
            .expect("first demand installs");
        coordinator
            .engine
            .install_history(
                ProviderGeneration(nonzero(1)),
                &first,
                2,
                0,
                vec![MarketBar {
                    source_sequence: 1,
                    exchange_timestamp_seconds: 60,
                    exchange_timestamp_unix_nanos: 60_000_000_000,
                    open: 100,
                    high: 101,
                    low: 99,
                    close: 100,
                    volume: 1,
                }],
            )
            .expect("history installs");

        coordinator
            .engine
            .set_resource_class(consumer, ConsumerResourceClass::Background)
            .expect("consumer backgrounds");
        assert!(!coordinator.engine.has_subscription(&first));
        let epoch = history_epoch(
            &mut coordinator,
            &first.provider_id,
            ProviderGeneration(nonzero(1)),
        );
        coordinator.history.begin(
            first.clone(),
            epoch,
            super::super::history::tracker::LaneKind::Backfill,
            None,
            coordinator.clock.as_ref(),
        );
        coordinator
            .history
            .coverage_mut(&first, epoch)
            .unwrap()
            .confirmed_empty = Some(HistoryRange {
            start_unix_nanos: 0,
            end_unix_nanos: 60_000_000_000,
        });
        coordinator.prune_history_tracking();
        assert!(
            coordinator
                .history
                .coverage(&first, epoch)
                .and_then(|coverage| coverage.confirmed_empty)
                .is_some()
        );
        assert_eq!(
            coordinator.evict_unreferenced_series(),
            [] as [BarSeriesKey; 0]
        );
        assert!(coordinator.engine.series_snapshot(&first).is_some());

        coordinator
            .engine
            .set_series_demand_with_streams(
                consumer,
                generation(2),
                &second,
                StreamRequirements::BARS,
            )
            .expect("replacement demand installs");
        coordinator.prune_history_tracking();
        assert!(coordinator.history.coverage(&first, epoch).is_none());
        assert_eq!(coordinator.evict_unreferenced_series(), vec![first.clone()]);
        assert!(coordinator.engine.series_snapshot(&first).is_none());
    }

    #[test]
    fn exhausted_ranged_history_retry_cools_down_exact_range_but_not_new_range() {
        let mut coordinator = coordinator();
        let selected = series();
        let generation = ProviderGeneration(nonzero(1));
        coordinator
            .engine
            .begin_provider_session("rithmic", generation)
            .expect("provider session begins");
        let failed = HistoryRange {
            start_unix_nanos: 0,
            end_unix_nanos: 60_000_000_000,
        };
        let epoch = history_epoch(&mut coordinator, &selected.provider_id, generation);
        for _ in 0..=MAXIMUM_HISTORY_RETRIES {
            coordinator.history.schedule(
                selected.clone(),
                epoch,
                super::super::history::tracker::LaneKind::Backfill,
                Some(failed),
                coordinator.clock.as_ref(),
                false,
            );
        }

        assert!(
            coordinator
                .enqueue_history_request(&selected, generation, Some(failed))
                .is_ok(),
            "identical failed range is suppressed during cooldown"
        );
        assert!(
            coordinator
                .history
                .pending(
                    &selected,
                    epoch,
                    super::super::history::tracker::LaneKind::Backfill
                )
                .is_some()
        );

        let older = HistoryRange {
            start_unix_nanos: -60_000_000_000,
            end_unix_nanos: 60_000_000_000,
        };
        assert!(
            coordinator
                .enqueue_history_request(&selected, generation, Some(older))
                .is_err(),
            "a genuinely different viewport range proceeds to provider dispatch"
        );
        assert!(
            coordinator
                .history
                .pending(
                    &selected,
                    epoch,
                    super::super::history::tracker::LaneKind::Backfill
                )
                .is_none()
        );
    }

    #[test]
    fn ranged_history_retry_backoff_blocks_duplicate_dispatch_before_deadline() {
        let mut coordinator = coordinator();
        let selected = series();
        let generation = ProviderGeneration(nonzero(1));
        coordinator
            .engine
            .begin_provider_session("rithmic", generation)
            .expect("provider session begins");
        let failed = HistoryRange {
            start_unix_nanos: 0,
            end_unix_nanos: 60_000_000_000,
        };
        let epoch = history_epoch(&mut coordinator, &selected.provider_id, generation);
        coordinator.history.schedule(
            selected.clone(),
            epoch,
            super::super::history::tracker::LaneKind::Backfill,
            Some(failed),
            coordinator.clock.as_ref(),
            false,
        );

        assert!(
            coordinator
                .enqueue_history_request(&selected, generation, Some(failed))
                .is_ok(),
            "duplicate range remains parked until its retry deadline"
        );
        assert_eq!(
            coordinator
                .history
                .pending(
                    &selected,
                    epoch,
                    super::super::history::tracker::LaneKind::Backfill
                )
                .map(|(_, range, attempts)| (attempts, range)),
            Some((1, Some(failed)))
        );
        assert!(coordinator.history.inflight(&selected, epoch).is_none());
    }

    #[test]
    fn ranged_history_queue_pressure_retains_one_retry() {
        let mut coordinator = coordinator();
        let selected = series();
        let generation = ProviderGeneration(nonzero(1));
        coordinator
            .engine
            .begin_provider_session("rithmic", generation)
            .expect("provider session begins");
        let range = HistoryRange {
            start_unix_nanos: 0,
            end_unix_nanos: 60_000_000_000,
        };
        let (history_tx, _history_rx) = std::sync::mpsc::sync_channel(0);
        let history_tx = Box::leak(Box::new(history_tx));
        coordinator.providers.records.insert(
            "rithmic",
            ProviderDispatchRecord {
                descriptor: super::super::RITHMIC_DESCRIPTOR,
                history: history_tx,
                lifecycle: None,
                ctrader: None,
                hyperliquid: None,
                tastytrade: None,
                rithmic: None,
                catalog: None,
            },
        );

        assert!(
            coordinator
                .enqueue_history_request_with_capacity_retry(&selected, generation, Some(range))
                .is_ok(),
            "bounded queue pressure is converted into coordinator-owned retry state"
        );
        let epoch = history_epoch(&mut coordinator, &selected.provider_id, generation);
        assert_eq!(
            coordinator
                .history
                .pending(
                    &selected,
                    epoch,
                    super::super::history::tracker::LaneKind::Backfill
                )
                .map(|(_, range, attempts)| (attempts, range)),
            Some((0, Some(range)))
        );
    }

    #[test]
    fn duplicate_covering_request_during_study_registration_does_not_queue_a_second_page() {
        let mut coordinator = coordinator();
        let selected = series();
        let generation = ProviderGeneration(nonzero(1));
        coordinator
            .engine
            .begin_provider_session("rithmic", generation)
            .expect("provider session begins");
        let ticket = history_ticket(&mut coordinator, &selected, generation, None);
        coordinator
            .enqueue_history_request(&selected, generation, None)
            .expect("the active page covers the duplicate demand");
        assert!(
            coordinator
                .history
                .inflight(&selected, ticket.provider)
                .is_some()
        );
        assert!(
            coordinator
                .history
                .take_deferred(&selected, ticket.provider)
                .is_none()
        );
    }

    #[test]
    fn empty_ranged_history_completion_records_coverage_without_retry() {
        let mut coordinator = coordinator();
        let selected = series();
        let generation = ProviderGeneration(nonzero(1));
        coordinator
            .engine
            .begin_provider_session("rithmic", generation)
            .expect("provider session begins");
        let range = HistoryRange {
            start_unix_nanos: 0,
            end_unix_nanos: 60_000_000_000,
        };
        let ticket = history_ticket(&mut coordinator, &selected, generation, Some(range));
        coordinator.history_completed(
            &ticket,
            generation,
            Some(range),
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 0,
                bars: Vec::new(),
                forming: None,
                handoff_boundary_unix_nanos: Some(range.end_unix_nanos),
                backwards_exhausted: false,
            }),
        );

        assert_eq!(
            coordinator
                .history
                .coverage(&selected, ticket.provider)
                .and_then(|coverage| coverage.confirmed_empty),
            Some(range)
        );
        assert!(
            coordinator
                .history
                .inflight(&selected, ticket.provider)
                .is_none()
        );
        assert!(
            coordinator
                .history
                .pending(
                    &selected,
                    ticket.provider,
                    super::super::history::tracker::LaneKind::Backfill
                )
                .is_none()
        );
    }

    fn empty_history_snapshot(boundary_unix_nanos: i64) -> HistorySnapshot {
        HistorySnapshot {
            price_scale: 2,
            quantity_scale: 0,
            bars: Vec::new(),
            forming: None,
            handoff_boundary_unix_nanos: Some(boundary_unix_nanos),
            backwards_exhausted: false,
        }
    }

    fn take_series_state(
        coordinator: &mut Coordinator<'_>,
        consumer_id: ConsumerId,
    ) -> Option<crate::MarketSeriesState> {
        match coordinator
            .events
            .get_mut(&consumer_id)?
            .series_state
            .take()?
        {
            MarketRuntimeEvent::SeriesState(state) => Some(state),
            _ => None,
        }
    }

    fn demand_series(
        coordinator: &mut Coordinator<'_>,
        consumer_id: ConsumerId,
        demand_generation: u64,
        selected: &BarSeriesKey,
    ) {
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_demand(
            client(1),
            selected,
            StreamRequirements::BARS,
            DemandWaiter {
                consumer_id,
                generation: generation(demand_generation),
                started_at: Instant::now(),
            },
            &reply,
        );
        result
            .recv()
            .expect("demand reply arrives")
            .expect("demand is accepted");
    }

    fn assert_no_history_request(
        history_receiver: &std::sync::mpsc::Receiver<super::super::HistoryRequest>,
        reason: &str,
    ) {
        assert!(
            matches!(
                history_receiver.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ),
            "{reason}"
        );
    }

    fn connected_rithmic_coordinator() -> (
        Coordinator<'static>,
        std::sync::mpsc::Receiver<super::super::HistoryRequest>,
    ) {
        let mut coordinator = coordinator();
        let (history_sender, history_receiver) = std::sync::mpsc::sync_channel(4);
        coordinator.providers.records.insert(
            "rithmic",
            super::super::ProviderDispatchRecord {
                descriptor: super::super::RITHMIC_DESCRIPTOR,
                history: Box::leak(Box::new(history_sender)),
                lifecycle: None,
                ctrader: None,
                hyperliquid: None,
                tastytrade: None,
                rithmic: None,
                catalog: None,
            },
        );
        coordinator.handle_provider_event(super::super::ProviderEvent::rithmic(
            RithmicRealtimeEvent::Connecting(1),
        ));
        coordinator
            .install_provider_instrument(&instrument())
            .expect("Rithmic instrument installs");
        (coordinator, history_receiver)
    }

    fn scripted_rithmic_coordinator(
        clock: Arc<dyn super::super::clock::Clock>,
    ) -> (
        Coordinator<'static>,
        std::sync::mpsc::Receiver<super::super::HistoryRequest>,
        std::sync::mpsc::SyncSender<RithmicRealtimeEvent>,
        std::sync::mpsc::Receiver<super::super::RithmicRealtimeControl>,
    ) {
        let (history_sender, history_receiver) = std::sync::mpsc::sync_channel(4);
        let (controls, controls_receiver) = std::sync::mpsc::sync_channel(4);
        let (events, event_receiver) =
            std::sync::mpsc::sync_channel(super::super::REALTIME_CAPACITY);
        let providers = ProviderDispatch {
            wake: None,
            records: BTreeMap::from([(
                "rithmic",
                ProviderDispatchRecord {
                    descriptor: super::super::RITHMIC_DESCRIPTOR,
                    history: Box::leak(Box::new(history_sender)),
                    lifecycle: None,
                    ctrader: None,
                    hyperliquid: None,
                    tastytrade: None,
                    rithmic: Some(Box::leak(Box::new(super::super::RithmicChannels {
                        controls,
                        events: event_receiver,
                        binding: Arc::new(std::sync::Mutex::new(
                            super::super::provider_session::RithmicBinding::default(),
                        )),
                    }))),
                    catalog: None,
                },
            )]),
            lanes: vec![super::super::ProviderEventLane::Realtime("rithmic")],
        };
        (
            Coordinator::with_clock(
                super::super::configured_engine().expect("configured engine"),
                providers,
                clock,
            ),
            history_receiver,
            events,
            controls_receiver,
        )
    }

    #[test]
    fn startup_history_uses_reserved_epoch_before_connecting_event_arrives() {
        let mut coordinator = coordinator();
        let (controls, pending_controls) = std::sync::mpsc::sync_channel(4);
        let (display_controls, _display_receiver) = std::sync::mpsc::sync_channel(4);
        let (history, history_receiver) = std::sync::mpsc::sync_channel(4);
        let history = Box::leak(Box::new(history));
        coordinator.providers.records.insert(
            "hyperliquid",
            ProviderDispatchRecord {
                descriptor: super::super::HYPERLIQUID_DESCRIPTOR,
                history,
                lifecycle: None,
                ctrader: None,
                hyperliquid: None,
                tastytrade: None,
                rithmic: None,
                catalog: None,
            },
        );
        coordinator.provider_registry.register(
            "hyperliquid",
            Box::new(super::super::provider_session::HyperliquidSession {
                controls,
                display_controls,
                history: history.clone(),
            }),
            super::super::HYPERLIQUID_DESCRIPTOR.capabilities.reconnect,
            coordinator.clock.as_ref(),
        );
        coordinator.provider_registry.reserve();
        coordinator
            .engine
            .begin_provider_session("hyperliquid", ProviderGeneration(nonzero(1)))
            .expect("catalog session");
        coordinator
            .install_provider_instrument(&hyperliquid_instrument())
            .expect("startup instrument installs");
        let selected = hyperliquid_series();
        let consumer_id = consumer(88);
        register(&mut coordinator, consumer_id);
        assert!(coordinator.ensure_provider_connection("hyperliquid"));
        let reserved = coordinator
            .provider_registry
            .epoch("hyperliquid")
            .expect("epoch reserved before worker handles connect");
        assert_eq!(reserved.get().get(), 2);
        assert!(pending_controls.try_recv().is_ok());
        assert_eq!(
            coordinator
                .engine
                .provider_status("hyperliquid")
                .and_then(|status| status.generation),
            Some(ProviderGeneration(nonzero(2))),
            "first demand and history must use the reserved epoch before the asynchronous Connecting event"
        );
        demand_series(&mut coordinator, consumer_id, 1, &selected);
        let request = history_receiver.try_recv().expect("first covering request");
        assert_eq!(request.ticket.provider, reserved);
        assert_eq!(
            request.provider_generation,
            ProviderGeneration(nonzero(2)),
            "a history completion must not be dropped for the preceding catalog generation"
        );
    }

    #[test]
    fn coordinator_drains_quote_guard_releases_before_waiting() {
        let mut coordinator = coordinator();
        coordinator
            .engine
            .register_provider(
                "quotes".into(),
                aeris_market_engine::ProviderConfig {
                    account_id: "quotes:public".into(),
                    capabilities: aeris_market_engine::ProviderCapabilities {
                        historical_bars: false,
                        realtime_bars: false,
                        streams: StreamRequirements::NONE.with(MarketStream::Quotes),
                    },
                    reconnect_delay: Duration::from_millis(250),
                },
            )
            .unwrap();
        let key = aeris_market_engine::InstrumentStreamKey {
            provider: "quotes".into(),
            instrument: aeris_instruments::InstrumentId::try_new("BTC").unwrap(),
            entitlement: None,
        };
        let lease_id = coordinator
            .engine
            .acquire_instrument_lease(&key, StreamRequirements::NONE.with(MarketStream::Quotes))
            .unwrap();
        let releases = Arc::new(Mutex::new(vec![lease_id]));
        coordinator.quote_releases = Some(Arc::clone(&releases));
        let (_commands, receiver) = std::sync::mpsc::sync_channel(1);
        assert!(coordinator_turn(
            &mut coordinator,
            &receiver,
            &[],
            &AtomicBool::new(false),
            Some(Duration::ZERO)
        ));
        assert_eq!(coordinator.engine.instrument_subscriptions().count(), 0);
        assert_eq!(coordinator.engine.data_lease_count(), 0);
        assert!(releases.lock().unwrap().is_empty());
    }

    #[test]
    fn rithmic_transport_replacement_rebinds_before_forwarding_new_wire_events() {
        let (mut coordinator, _, events, controls) =
            scripted_rithmic_coordinator(Arc::new(super::super::clock::SystemClock));
        assert!(coordinator.ensure_provider_connection("rithmic"));
        let first = coordinator
            .provider_registry
            .epoch("rithmic")
            .expect("reserved");
        assert!(
            controls.try_recv().is_err(),
            "binding does not open transport"
        );
        events.send(RithmicRealtimeEvent::Connected(1)).unwrap();
        let Some(ProviderRuntimeEvent::BoundRealtime(connected)) =
            coordinator.providers.take_event(0)
        else {
            panic!("initial event must be bound");
        };
        assert_eq!(connected.generation, first);
        coordinator.handle_bound_provider_event(connected);
        events.send(RithmicRealtimeEvent::Connecting(2)).unwrap();
        let Some(ProviderRuntimeEvent::BoundRealtime(replaced)) =
            coordinator.providers.take_event(0)
        else {
            panic!("replacement must precede new-wire events");
        };
        assert!(matches!(
            replaced.kind,
            super::super::ProviderEventKind::TransportReplaced
        ));
        assert_eq!(replaced.generation, first);
        coordinator.handle_bound_provider_event(replaced);
        let second = coordinator
            .provider_registry
            .epoch("rithmic")
            .expect("rebound");
        assert_ne!(first, second);
        assert_eq!(
            coordinator.provider_epochs.current("rithmic", 2),
            Some(second)
        );
        let Some(ProviderRuntimeEvent::BoundRealtime(connecting)) =
            coordinator.providers.take_event(0)
        else {
            panic!("queued new-wire event must resume after rebind");
        };
        assert_eq!(connecting.generation, second);
        assert!(controls.try_recv().is_err(), "rebind never opens a socket");
    }

    #[test]
    fn retired_provider_and_history_epochs_are_dropped_once_each() {
        use aeris_contracts::{MarketSessionPhase, MarketSessionSource, MarketSessionStatus};
        let (mut coordinator, _) = connected_rithmic_coordinator();
        let previous = coordinator
            .provider_epochs
            .stamp("rithmic", 1, &mut coordinator.provider_registry)
            .expect("first session minted by registry");
        let series = series();
        let before = coordinator.engine.series_snapshot(&series);
        coordinator
            .engine
            .begin_provider_session("rithmic", ProviderGeneration(nonzero(2)))
            .expect("new provider session");
        coordinator
            .provider_epochs
            .stamp("rithmic", 2, &mut coordinator.provider_registry)
            .expect("new session minted by registry");
        let phase = coordinator
            .calendar
            .phase(&instrument().instrument_id, 1_800_000_000_000);
        coordinator.handle_provider_event(super::super::WireProviderEvent {
            provider: "rithmic",
            generation: 1,
            kind: super::super::ProviderEventKind::Calendar {
                instrument_id: instrument().instrument_id.clone(),
                status: MarketSessionStatus {
                    instrument_id: instrument().instrument_id,
                    phase: MarketSessionPhase::Closed,
                    source: MarketSessionSource::ProviderCalendar,
                    session_start_unix_nanos: None,
                    session_end_unix_nanos: None,
                    next_open_unix_nanos: None,
                },
                valid_from_utc_ms: 1_799_999_999_000,
                valid_until_utc_ms: 1_800_000_001_000,
            },
        });
        assert_eq!(
            coordinator
                .calendar
                .phase(&instrument().instrument_id, 1_800_000_000_000),
            phase
        );
        assert_eq!(coordinator.provider_epochs.stale_counts(), (1, 0));
        let mut retired_ticket = super::super::fixture_history_ticket(&series);
        retired_ticket.provider = previous;
        coordinator.handle_command(Command::HistoryCompleted(
            series.clone(),
            ProviderGeneration(nonzero(1)),
            None,
            Err("retired history".into()),
            retired_ticket,
        ));
        assert_eq!(coordinator.provider_epochs.stale_counts(), (1, 1));
        assert_eq!(coordinator.engine.series_snapshot(&series), before);
    }

    #[test]
    fn provider_holiday_event_is_generation_fenced_and_closes_watchdog_calendar() {
        use aeris_contracts::{MarketSessionPhase, MarketSessionSource, MarketSessionStatus};
        use chrono::{DateTime, Utc};
        let midday = "2026-08-17T18:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("fixture")
            .timestamp_millis();
        let (mut coordinator, _) = connected_rithmic_coordinator();
        let instrument_id = instrument().instrument_id;
        assert_eq!(
            coordinator.calendar.phase(&instrument_id, midday),
            aeris_instruments::SessionPhase::Open
        );
        let event = |generation| super::super::WireProviderEvent {
            provider: "rithmic",
            generation,
            kind: super::super::ProviderEventKind::Calendar {
                instrument_id: instrument_id.clone(),
                status: MarketSessionStatus {
                    instrument_id: instrument_id.clone(),
                    phase: MarketSessionPhase::Closed,
                    source: MarketSessionSource::ProviderCalendar,
                    session_start_unix_nanos: None,
                    session_end_unix_nanos: None,
                    next_open_unix_nanos: None,
                },
                valid_from_utc_ms: midday - 60_000,
                valid_until_utc_ms: midday + 60_000,
            },
        };
        coordinator.handle_provider_event(event(0));
        assert_eq!(
            coordinator.calendar.phase(&instrument_id, midday),
            aeris_instruments::SessionPhase::Open
        );
        coordinator.handle_provider_event(event(1));
        assert_eq!(
            coordinator.calendar.phase(&instrument_id, midday),
            aeris_instruments::SessionPhase::Closed
        );
        assert_eq!(
            coordinator.calendar.phase(&instrument_id, midday + 120_000),
            aeris_instruments::SessionPhase::Open
        );
    }

    #[test]
    fn calendar_unknown_week_and_month_publish_explicit_market_demand_error() {
        let (mut coordinator, _) = connected_rithmic_coordinator();
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator.events.entry(consumer).or_default();
        let mut installed = instrument();
        installed.venue_id = "UNKNOWN".into();
        coordinator.calendar.resolve(&installed, None);
        coordinator.catalog.insert(
            (installed.provider.clone(), installed.instrument_id.clone()),
            installed,
        );
        for (index, period) in [
            BarPeriod::week(1).expect("week"),
            BarPeriod::month(1).expect("month"),
        ]
        .into_iter()
        .enumerate()
        {
            let selected = BarSeriesKey { period, ..series() };
            let (reply, receiver) = std::sync::mpsc::sync_channel(1);
            coordinator.handle_demand(
                client(1),
                &selected,
                StreamRequirements::BARS,
                DemandWaiter {
                    consumer_id: consumer,
                    generation: generation(index as u64 + 1),
                    started_at: Instant::now(),
                },
                &reply,
            );
            assert!(receiver.recv().expect("demand reply").is_err());
            let event = coordinator
                .events
                .get_mut(&consumer)
                .and_then(|events| events.demand_error.take())
                .expect("typed demand error");
            let MarketRuntimeEvent::DemandError(error) = event else {
                panic!("expected MarketDemandError");
            };
            assert_eq!(error.code, aeris_contracts::EngineFaultCode::Permanent);
            assert_eq!(error.series, Some(selected));
        }
    }

    #[test]
    fn calendar_tastytrade_equity_week_demand_returns_explicit_error() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator.events.entry(consumer).or_default();
        coordinator
            .engine
            .begin_provider_session("tastytrade", ProviderGeneration(nonzero(1)))
            .expect("provider session");
        let (history_sender, _) = std::sync::mpsc::sync_channel(1);
        coordinator.providers.records.insert(
            "tastytrade",
            super::super::ProviderDispatchRecord {
                descriptor: super::super::tastytrade::DESCRIPTOR,
                history: Box::leak(Box::new(history_sender)),
                lifecycle: None,
                ctrader: None,
                hyperliquid: None,
                tastytrade: None,
                rithmic: None,
                catalog: None,
            },
        );
        let instrument = InstallProviderInstrument {
            provider: "tastytrade".into(),
            instrument_id: "tastytrade:Equity:TEST".into(),
            provider_symbol: "TEST".into(),
            venue_id: "NYSE".into(),
            entitlement_id: super::super::tastytrade::ENTITLEMENT.into(),
            ..Default::default()
        };
        let selected = BarSeriesKey {
            provider_id: instrument.provider.clone(),
            instrument_id: instrument.instrument_id.clone(),
            entitlement_id: instrument.entitlement_id.clone(),
            period: BarPeriod::week(1).expect("week"),
            definition_version: 1,
        };
        coordinator.catalog.insert(
            (
                instrument.provider.clone(),
                instrument.instrument_id.clone(),
            ),
            instrument,
        );
        let (reply, receiver) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_demand(
            client(1),
            &selected,
            StreamRequirements::BARS,
            DemandWaiter {
                consumer_id: consumer,
                generation: generation(1),
                started_at: Instant::now(),
            },
            &reply,
        );
        assert!(receiver.recv().expect("demand reply").is_err());
        assert!(matches!(
            coordinator
                .events
                .get_mut(&consumer)
                .and_then(|events| events.demand_error.take()),
            Some(MarketRuntimeEvent::DemandError(crate::MarketDemandError {
                code: aeris_contracts::EngineFaultCode::Permanent,
                ..
            }))
        ));
    }

    #[test]
    fn watchdog_rithmic_cme_trade_series_recovers_without_recent_trades() {
        use super::super::clock::{Clock, ScriptedClock};
        use chrono::{DateTime, Utc};
        let at = "2026-08-17T14:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("UTC instant")
            .timestamp_nanos_opt()
            .expect("timestamp");
        let clock = Arc::new(ScriptedClock::new(at));
        let (mut coordinator, history_rx) = connected_rithmic_coordinator();
        coordinator.clock = Arc::clone(&clock) as Arc<dyn Clock>;
        let selected = series();
        let provider_generation = ProviderGeneration(nonzero(1));
        coordinator
            .engine
            .set_provider_health("rithmic", provider_generation, ProviderHealth::Online)
            .expect("provider online");
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator
            .engine
            .set_series_demand_with_streams(
                consumer,
                generation(1),
                &selected,
                StreamRequirements::BARS,
            )
            .expect("series demanded");
        let mut live = TradeModel::with_clock(
            &selected,
            provider_generation,
            "CME",
            None,
            Arc::clone(&clock) as Arc<dyn Clock>,
        )
        .expect("trade handoff");
        live.connected = true;
        coordinator.series.insert_trade(selected.clone(), live);
        set_trade_history_ready(&mut coordinator, &selected);
        clock.advance(super::super::watchdog::OPEN_STALL_THRESHOLD + Duration::from_secs(1));
        coordinator.recover_stalled_live_series();
        assert!(history_rx.try_recv().is_ok(), "current history requested");
        assert!(
            !coordinator
                .series
                .trade_lifecycle(&selected)
                .expect("trade handoff")
                .history_ready()
        );
        coordinator.recover_stalled_live_series();
        assert_no_history_request(&history_rx, "Rithmic recovery is requested only once");
    }

    fn scripted_watchdog_recovery(provider: &'static str, unknown: bool) {
        use super::super::clock::{Clock, ScriptedClock};
        use chrono::{DateTime, Utc};

        let at = "2026-08-17T14:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("UTC instant")
            .timestamp_nanos_opt()
            .expect("timestamp");
        let clock = Arc::new(ScriptedClock::new(at));
        let mut coordinator = coordinator();
        coordinator.clock = Arc::clone(&clock) as Arc<dyn Clock>;
        let installed = if provider == "hyperliquid" {
            hyperliquid_instrument()
        } else {
            let mut installed = instrument();
            installed.provider = provider.into();
            installed.instrument_id = format!("{provider}:ES");
            installed.provider_symbol = "ES".into();
            installed.venue_id = if unknown { String::new() } else { "CME".into() };
            installed
        };
        let selected = BarSeriesKey {
            provider_id: installed.provider.clone(),
            instrument_id: installed.instrument_id.clone(),
            entitlement_id: installed.entitlement_id.clone(),
            period: BarPeriod::time(60).expect("minute"),
            definition_version: 1,
        };
        let provider_generation = ProviderGeneration(nonzero(1));
        coordinator
            .engine
            .begin_provider_session(provider, provider_generation)
            .expect("session");
        coordinator
            .engine
            .set_provider_health(provider, provider_generation, ProviderHealth::Online)
            .expect("online");
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator
            .engine
            .set_series_demand_with_streams(
                consumer,
                generation(1),
                &selected,
                StreamRequirements::BARS,
            )
            .expect("demand");
        let (history_tx, history_rx) = std::sync::mpsc::sync_channel(2);
        let descriptor = super::super::BUILT_IN_PROVIDER_DESCRIPTORS
            .iter()
            .find(|descriptor| descriptor.id == provider)
            .expect("registered provider");
        coordinator.providers.records.insert(
            provider,
            super::super::ProviderDispatchRecord {
                descriptor: *descriptor,
                history: Box::leak(Box::new(history_tx)),
                lifecycle: None,
                ctrader: None,
                hyperliquid: None,
                tastytrade: None,
                rithmic: None,
                catalog: None,
            },
        );
        coordinator.catalog.insert(
            (installed.provider.clone(), installed.instrument_id.clone()),
            installed.clone(),
        );
        coordinator.calendar.resolve(&installed, None);
        install_scripted_watchdog_handoff(
            &mut coordinator,
            provider,
            &installed,
            &selected,
            provider_generation,
            &clock,
        );
        clock.advance(super::super::watchdog::OPEN_STALL_THRESHOLD + Duration::from_secs(1));
        coordinator.recover_stalled_live_series();
        assert_eq!(
            history_rx.try_iter().count(),
            usize::from(!unknown),
            "{provider}: normal open threshold"
        );
        if unknown {
            clock.advance(
                super::super::watchdog::UNKNOWN_STALL_THRESHOLD
                    .checked_sub(super::super::watchdog::OPEN_STALL_THRESHOLD)
                    .expect("long threshold exceeds open threshold"),
            );
            coordinator.recover_stalled_live_series();
            assert_eq!(history_rx.try_iter().count(), 1, "Unknown long threshold");
        }
        coordinator.recover_stalled_live_series();
        assert_no_history_request(&history_rx, "no duplicate current history request");
    }

    fn install_scripted_watchdog_handoff(
        coordinator: &mut Coordinator<'_>,
        provider: &str,
        installed: &InstallProviderInstrument,
        selected: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        clock: &Arc<super::super::clock::ScriptedClock>,
    ) {
        use super::super::clock::Clock;
        if provider == "hyperliquid" {
            let mut live = SeriesLifecycle::with_clock(
                selected.clone(),
                provider_generation,
                "BTC",
                "1m",
                super::super::CandleGapPolicy::Contiguous,
                Arc::clone(clock) as Arc<dyn Clock>,
            );
            live.connected = true;
            live.state = SeriesState::Live {
                last_progress: live.clock.monotonic(),
                observed_live: false,
            };
            coordinator.series.insert_candle(selected.clone(), live);
        } else {
            let mut live = TradeModel::with_clock(
                selected,
                provider_generation,
                &installed.venue_id,
                None,
                Arc::clone(clock) as Arc<dyn Clock>,
            )
            .expect("trade handoff");
            live.connected = true;
            coordinator.series.insert_trade(selected.clone(), live);
            set_trade_history_ready(coordinator, selected);
        }
    }

    #[test]
    fn watchdog_tastytrade_open_trade_series_requests_one_recovery() {
        scripted_watchdog_recovery("tastytrade", false);
    }

    #[test]
    fn watchdog_hyperliquid_always_open_candle_series_requests_one_recovery() {
        scripted_watchdog_recovery("hyperliquid", false);
    }

    #[test]
    fn watchdog_unknown_series_waits_for_long_threshold_before_one_recovery() {
        scripted_watchdog_recovery("tastytrade", true);
    }

    /// Rithmic Test answers a closed-market current replay with zero bars. That
    /// answer used to be reported as a retryable failure, and every heartbeat
    /// and every desktop recovery then logged in to the history plant again
    /// for the same empty window: polling the provider forbids.
    #[test]
    fn empty_current_history_is_final_until_live_trade_or_new_generation() {
        let (mut coordinator, history_receiver) = connected_rithmic_coordinator();
        let first_generation = ProviderGeneration(nonzero(1));
        let consumer = consumer(1);
        let selected = series();
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());

        demand_series(&mut coordinator, consumer, 1, &selected);
        let request = history_receiver
            .try_recv()
            .expect("the first demand requests covering history");
        assert_eq!(request.provider_generation, first_generation);
        assert!(request.range.is_none());

        coordinator.history_completed(
            &request.ticket,
            first_generation,
            None,
            Ok(empty_history_snapshot(60_000_000_000)),
        );

        let epoch = request.ticket.provider;
        assert!(coordinator.history.inflight(&selected, epoch).is_none());
        assert!(
            coordinator
                .history
                .pending(
                    &selected,
                    epoch,
                    super::super::history::tracker::LaneKind::Current
                )
                .is_none()
        );
        assert!(coordinator.engine.series_snapshot(&selected).is_none());
        let events = coordinator.events.get(&consumer).expect("consumer outbox");
        assert!(
            events.demand_error.is_none(),
            "an empty provider answer is not a retryable failure"
        );
        let state = take_series_state(&mut coordinator, consumer).expect("series state");
        assert_eq!(state.state, SeriesLoadState::Empty);
        assert_eq!(state.generation, generation(1));
        assert!(
            coordinator.engine.has_subscription(&selected),
            "the live subscription stays up while history is empty"
        );

        coordinator.handle_provider_event(super::super::ProviderEvent::rithmic(
            RithmicRealtimeEvent::Heartbeat(1, None),
        ));
        coordinator.retry_history();
        demand_series(&mut coordinator, consumer, 2, &selected);
        assert_no_history_request(
            &history_receiver,
            "heartbeats, retry ticks and an identical demand do not re-request",
        );
        let state = take_series_state(&mut coordinator, consumer).expect("re-stated state");
        assert_eq!(state.state, SeriesLoadState::Empty);
        assert_eq!(state.generation, generation(2));
        assert!(!coordinator.pending.contains_key(&selected));

        send_fixture_trade(&mut coordinator, &selected, 1, 30_000_000_000);
        assert_no_history_request(
            &history_receiver,
            "a trade the empty replay already covered is not new evidence",
        );

        send_fixture_trade(&mut coordinator, &selected, 2, 120_000_000_000);
        let request = history_receiver
            .try_recv()
            .expect("the first newer live trade allows one covering request");
        assert_eq!(request.provider_generation, first_generation);
        assert!(request.range.is_none());

        coordinator.history_completed(
            &request.ticket,
            first_generation,
            None,
            Ok(empty_history_snapshot(150_000_000_000)),
        );
        assert_eq!(
            coordinator
                .history
                .current_empty(&selected, request.ticket.provider),
            Some(super::super::EmptyCurrentHistory::Final)
        );
        send_fixture_trade(&mut coordinator, &selected, 3, 180_000_000_000);
        coordinator.handle_provider_event(super::super::ProviderEvent::rithmic(
            RithmicRealtimeEvent::Heartbeat(1, None),
        ));
        assert_no_history_request(
            &history_receiver,
            "a second empty answer is final for the provider generation",
        );

        coordinator.handle_provider_event(super::super::ProviderEvent::rithmic(
            RithmicRealtimeEvent::Connecting(2),
        ));
        let request = history_receiver
            .try_recv()
            .expect("a new provider generation requests covering history again");
        assert_eq!(request.provider_generation, ProviderGeneration(nonzero(2)));
        assert!(request.range.is_none());
    }

    fn current_request_with_deferred_viewport_backfill() -> (
        Coordinator<'static>,
        std::sync::mpsc::Receiver<super::super::HistoryRequest>,
        super::super::HistoryRequest,
        BarSeriesKey,
    ) {
        use super::super::clock::{Clock, ScriptedClock};

        let clock = Arc::new(ScriptedClock::new(1_000_000_000));
        let (mut coordinator, history_receiver) = connected_rithmic_coordinator();
        coordinator.clock = clock.clone() as Arc<dyn Clock>;
        let selected = series();
        let provider_generation = ProviderGeneration(nonzero(1));
        let consumer_id = consumer(1);
        register(&mut coordinator, consumer_id);
        coordinator
            .engine
            .set_series_demand_with_streams(
                consumer_id,
                generation(1),
                &selected,
                StreamRequirements::BARS,
            )
            .expect("series demand installs");
        coordinator
            .engine
            .install_history(provider_generation, &selected, 2, 0, minute_bars(10, 10))
            .expect("retained current history installs");
        coordinator
            .enqueue_history_request(&selected, provider_generation, None)
            .expect("current-edge refresh starts");
        let current = history_receiver.try_recv().expect("current request");
        assert!(current.range.is_none());

        let minute = 60_000_000_000;
        let viewport = Viewport::try_new(5 * minute, 9 * minute).expect("older viewport");
        coordinator
            .engine
            .set_viewport(consumer_id, generation(1), viewport)
            .expect("viewport installs");
        coordinator
            .request_viewport_history(consumer_id, generation(1), viewport)
            .expect("viewport backfill defers behind current");
        assert_no_history_request(&history_receiver, "backfill must wait for current");
        (coordinator, history_receiver, current, selected)
    }

    fn assert_deferred_backfill_dispatched(
        coordinator: &Coordinator<'_>,
        history_receiver: &std::sync::mpsc::Receiver<super::super::HistoryRequest>,
        current: &super::super::HistoryRequest,
        selected: &BarSeriesKey,
    ) {
        let minute = 60_000_000_000;
        let backfill = history_receiver
            .try_recv()
            .expect("completion alone dispatches the deferred backfill");
        assert_eq!(&backfill.ticket.series, selected);
        assert_eq!(backfill.ticket.provider, current.ticket.provider);
        assert_eq!(backfill.ticket.demand, current.ticket.demand);
        assert_eq!(
            backfill.ticket.lane,
            super::super::history::tracker::LaneKind::Backfill
        );
        assert!(backfill.range.is_some_and(|range| {
            range.start_unix_nanos < 5 * minute && range.end_unix_nanos == 10 * minute
        }));
        assert_eq!(
            coordinator
                .history
                .inflight(selected, current.ticket.provider)
                .map(|(ticket, _)| ticket),
            Some(&backfill.ticket)
        );
        assert_no_history_request(history_receiver, "exactly one backfill is dispatched");
    }

    #[test]
    fn empty_current_completion_dispatches_deferred_viewport_backfill_without_another_trigger() {
        let (mut coordinator, history_receiver, current, selected) =
            current_request_with_deferred_viewport_backfill();
        let minute = 60_000_000_000;

        coordinator.history_completed(
            &current.ticket,
            current.provider_generation,
            None,
            Ok(empty_history_snapshot(20 * minute)),
        );
        assert_deferred_backfill_dispatched(&coordinator, &history_receiver, &current, &selected);
    }

    #[test]
    fn empty_current_completion_with_live_handoff_drains_deferred_backfill() {
        let (mut coordinator, history_receiver, current, selected) =
            current_request_with_deferred_viewport_backfill();
        let live = TradeModel::new(&selected, current.provider_generation, "CME", None)
            .expect("trade handoff");
        coordinator.series.insert_trade(selected.clone(), live);
        set_trade_history_ready(&mut coordinator, &selected);

        coordinator.history_completed(
            &current.ticket,
            current.provider_generation,
            None,
            Ok(empty_history_snapshot(20 * 60_000_000_000)),
        );
        assert_deferred_backfill_dispatched(&coordinator, &history_receiver, &current, &selected);
    }

    #[test]
    fn failed_current_completion_drains_deferred_backfill_and_retains_retry() {
        let (mut coordinator, history_receiver, current, selected) =
            current_request_with_deferred_viewport_backfill();
        coordinator.history_completed(
            &current.ticket,
            current.provider_generation,
            None,
            Err("scripted current failure".to_string()),
        );
        assert_deferred_backfill_dispatched(&coordinator, &history_receiver, &current, &selected);
        assert!(
            coordinator
                .history
                .pending(
                    &selected,
                    current.ticket.provider,
                    super::super::history::tracker::LaneKind::Current
                )
                .is_some(),
            "the failed current edge remains scheduled for retry"
        );
    }

    #[test]
    fn nonempty_current_completion_drains_deferred_backfill() {
        let (mut coordinator, history_receiver, current, selected) =
            current_request_with_deferred_viewport_backfill();
        coordinator.history_completed(
            &current.ticket,
            current.provider_generation,
            None,
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 0,
                bars: minute_bars(10, 10),
                forming: None,
                handoff_boundary_unix_nanos: None,
                backwards_exhausted: false,
            }),
        );
        assert_deferred_backfill_dispatched(&coordinator, &history_receiver, &current, &selected);
    }

    #[test]
    fn stale_current_completion_does_not_dispatch_retired_backfill() {
        let (mut coordinator, history_receiver, current, selected) =
            current_request_with_deferred_viewport_backfill();
        coordinator
            .engine
            .begin_provider_session("rithmic", ProviderGeneration(nonzero(2)))
            .expect("new provider session begins");
        let next_epoch = history_epoch(&mut coordinator, "rithmic", ProviderGeneration(nonzero(2)));
        assert_ne!(next_epoch, current.ticket.provider);

        coordinator.history_completed(
            &current.ticket,
            current.provider_generation,
            None,
            Ok(empty_history_snapshot(20 * 60_000_000_000)),
        );
        assert_no_history_request(
            &history_receiver,
            "stale completion cannot dispatch retired viewport work",
        );
        assert!(
            coordinator
                .history
                .coverage(&selected, current.ticket.provider)
                .is_none(),
            "retired epoch and its deferred request are pruned together"
        );
    }

    fn send_fixture_trade(
        coordinator: &mut Coordinator<'_>,
        selected: &BarSeriesKey,
        sequence: u64,
        timestamp: i64,
    ) {
        coordinator.handle_provider_event(super::super::ProviderEvent::rithmic(
            RithmicRealtimeEvent::Trade(1, rithmic_trade_for_series(selected, sequence, timestamp)),
        ));
    }

    #[test]
    fn exhausted_backwards_history_suppresses_further_older_requests_without_error() {
        let mut coordinator = coordinator();
        let selected = series();
        let generation = ProviderGeneration(nonzero(1));
        coordinator
            .engine
            .begin_provider_session("rithmic", generation)
            .expect("provider session begins");
        let current = minute_bars(10, 2);
        let first_timestamp = current[0].exchange_timestamp_unix_nanos;
        coordinator
            .engine
            .install_history(generation, &selected, 2, 0, current)
            .expect("current history installs");
        let range = HistoryRange {
            start_unix_nanos: 0,
            end_unix_nanos: first_timestamp,
        };
        let ticket = history_ticket(&mut coordinator, &selected, generation, Some(range));
        coordinator.history_completed(
            &ticket,
            generation,
            Some(range),
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 0,
                bars: Vec::new(),
                forming: None,
                handoff_boundary_unix_nanos: None,
                backwards_exhausted: true,
            }),
        );

        assert!(
            coordinator
                .history
                .coverage(&selected, ticket.provider)
                .unwrap()
                .backwards_exhausted
        );
        assert!(
            coordinator
                .history
                .pending(
                    &selected,
                    ticket.provider,
                    super::super::history::tracker::LaneKind::Backfill
                )
                .is_none()
        );
        assert!(
            coordinator
                .history
                .inflight(&selected, ticket.provider)
                .is_none()
        );
        let farther_left = HistoryRange {
            start_unix_nanos: 0,
            end_unix_nanos: first_timestamp - 60_000_000_000,
        };
        assert!(
            coordinator
                .enqueue_history_request(&selected, generation, Some(farther_left))
                .is_ok()
        );
        assert!(
            coordinator
                .history
                .inflight(&selected, ticket.provider)
                .is_none()
        );
        assert!(
            coordinator
                .history
                .pending(
                    &selected,
                    ticket.provider,
                    super::super::history::tracker::LaneKind::Backfill
                )
                .is_none()
        );
        assert!(coordinator.events.is_empty());
    }

    /// A chart scrolled into history the provider cannot serve must keep its
    /// healthy live series. The failure is reported as older-history scoped so
    /// the presentation does not treat it as a current-series recovery.
    #[test]
    fn exhausted_older_history_backfill_keeps_the_current_series_live() {
        let mut coordinator = coordinator();
        let consumer_id = consumer(1);
        register(&mut coordinator, consumer_id);
        coordinator
            .events
            .insert(consumer_id, ConsumerEvents::default());
        let selected = series();
        let provider_generation = ProviderGeneration(nonzero(1));
        coordinator
            .engine
            .begin_provider_session("rithmic", provider_generation)
            .expect("provider session begins");
        let current = minute_bars(10, 2);
        let first_timestamp = current[0].exchange_timestamp_unix_nanos;
        coordinator
            .engine
            .install_history(provider_generation, &selected, 2, 0, current)
            .expect("current history installs");
        coordinator
            .engine
            .set_series_demand(consumer_id, generation(1), &selected)
            .expect("foreground demand subscribes");
        let range = HistoryRange {
            start_unix_nanos: 0,
            end_unix_nanos: first_timestamp,
        };

        for attempt in 0..=MAXIMUM_HISTORY_RETRIES {
            finish_history(
                &mut coordinator,
                &selected,
                provider_generation,
                Some(range),
                Err("candle history stopped before reaching the requested range".into()),
            );
            assert_eq!(
                take_series_state(&mut coordinator, consumer_id),
                None,
                "older-history attempt {attempt} must not move the current series into recovery"
            );
        }

        let events = coordinator
            .events
            .get_mut(&consumer_id)
            .expect("consumer outbox");
        let Some(MarketRuntimeEvent::DemandError(error)) = events.demand_error.take() else {
            panic!("exhausted older-history backfill reports one demand error");
        };
        assert_eq!(error.scope, crate::MarketDemandErrorScope::OlderHistory);
        assert_eq!(error.generation, generation(1));
        assert_eq!(error.series.as_ref(), Some(&selected));
        assert!(coordinator.engine.has_subscription(&selected));
        assert!(
            coordinator
                .engine
                .series_snapshot(&selected)
                .is_some_and(|snapshot| snapshot.bars.len() == 2),
            "the installed current history is retained"
        );
    }

    #[test]
    fn fresh_depth_demand_publishes_identified_awaiting_snapshot_frame() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        let selected = instrument();
        let selected_series = series();
        coordinator.catalog.insert(
            (selected.provider.clone(), selected.instrument_id.clone()),
            selected.clone(),
        );
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        coordinator
            .engine
            .set_series_demand_with_streams(
                consumer,
                generation(7),
                &selected_series,
                StreamRequirements::BARS.with(MarketStream::Depth),
            )
            .expect("depth demand installs");
        coordinator.reconcile_order_books();
        coordinator.publish_order_book_to_consumer(consumer);

        assert!(matches!(
            coordinator
                .events
                .get(&consumer)
                .and_then(|events| events.order_book.as_ref()),
            Some(MarketRuntimeEvent::OrderBookSnapshot(snapshot))
                if snapshot.publication.provider_id == selected.provider
                    && snapshot.publication.instrument_id == selected.instrument_id
                    && snapshot.publication.entitlement_id == selected.entitlement_id
                    && snapshot.publication.session_generation == selected.session_generation
                    && matches!(
                        snapshot.publication.state,
                        aeris_market_data::OrderBookState::Recovering(
                            aeris_market_data::OrderBookRecoveryReason::AwaitingSnapshot
                        )
                    )
        ));
    }

    #[test]
    fn quote_book_publishes_top_of_book_only_until_some_consumer_subscribes_depth() {
        let mut coordinator = coordinator();
        let selected = hyperliquid_instrument();
        let selected_series = hyperliquid_series();
        coordinator.catalog.insert(
            (selected.provider.clone(), selected.instrument_id.clone()),
            selected.clone(),
        );
        let chart = consumer(1);
        let order_book_panel = consumer(2);
        register(&mut coordinator, chart);
        register(&mut coordinator, order_book_panel);
        let quotes = StreamRequirements::BARS.with(MarketStream::Quotes);
        for consumer_id in [chart, order_book_panel] {
            coordinator
                .engine
                .set_series_demand_with_streams(
                    consumer_id,
                    generation(3),
                    &selected_series,
                    quotes,
                )
                .expect("quote demand installs");
        }
        coordinator.reconcile_order_books();
        let identity = (selected.provider.clone(), selected.instrument_id.clone());
        let top_of_book_only = |coordinator: &Coordinator<'_>| {
            coordinator
                .order_books
                .get(&identity)
                .expect("quote demand keeps a canonical book")
                .publishes_top_of_book_only()
        };
        assert!(top_of_book_only(&coordinator));

        assert!(
            coordinator
                .engine
                .set_stream_requirements(
                    order_book_panel,
                    generation(3),
                    quotes.with(MarketStream::Depth),
                )
                .expect("order book opens")
        );
        coordinator.reconcile_order_books();
        assert!(!top_of_book_only(&coordinator));

        assert!(
            coordinator
                .engine
                .set_stream_requirements(order_book_panel, generation(3), quotes)
                .expect("order book closes")
        );
        coordinator.reconcile_order_books();
        assert!(
            top_of_book_only(&coordinator),
            "the chart keeps receiving the live quote after the last depth consumer closes"
        );
    }

    fn display_book(
        display_generation: u64,
        source_sequence: u64,
        bid: i64,
    ) -> ProviderDisplayDepthEvent {
        let selected = hyperliquid_instrument();
        ProviderDisplayDepthEvent::Snapshot {
            provider_generation: 1,
            display_generation,
            snapshot: DepthSnapshot {
                metadata: EventMetadata {
                    provider_id: selected.provider,
                    instrument_id: selected.instrument_id,
                    entitlement_id: selected.entitlement_id,
                    source_sequence,
                    session_generation: 1,
                    timestamps: QualifiedTimestamp {
                        exchange_unix_nanos: Some(1),
                        provider_unix_nanos: None,
                        received_unix_nanos: 1,
                    },
                },
                bids: vec![DepthLevel {
                    price: bid,
                    quantity: 1,
                    order_count: Some(1),
                }],
                asks: vec![DepthLevel {
                    price: bid + 1_000,
                    quantity: 1,
                    order_count: Some(1),
                }],
            },
        }
    }

    #[test]
    fn reopened_order_book_shows_the_new_display_session_instead_of_the_frozen_one() {
        let mut coordinator = coordinator();
        let selected = hyperliquid_instrument();
        coordinator
            .install_provider_instrument(&selected)
            .expect("provider session installs");
        coordinator
            .engine
            .set_provider_health(
                "hyperliquid",
                ProviderGeneration(nonzero(1)),
                ProviderHealth::Online,
            )
            .expect("provider is online");
        let chart = consumer(1);
        let order_book_panel = consumer(2);
        register(&mut coordinator, chart);
        register(&mut coordinator, order_book_panel);
        let quotes = StreamRequirements::BARS.with(MarketStream::Quotes);
        coordinator
            .engine
            .set_series_demand_with_streams(chart, generation(3), &hyperliquid_series(), quotes)
            .expect("chart demand installs");
        coordinator
            .engine
            .set_series_demand_with_streams(
                order_book_panel,
                generation(3),
                &hyperliquid_series(),
                quotes.with(MarketStream::Depth),
            )
            .expect("order book demand installs");
        coordinator.reconcile_order_books();
        let identity = (selected.provider.clone(), selected.instrument_id.clone());
        let shown = |coordinator: &Coordinator<'_>| {
            coordinator
                .display_depth
                .get(&identity)
                .map(|display| (display.display_generation, display.source_sequence))
        };

        coordinator.handle_provider_display_depth(
            "hyperliquid",
            ProviderDisplayDepthEvent::Reset {
                display_generation: 1,
            },
        );
        coordinator.handle_provider_display_depth("hyperliquid", display_book(1, 500, 8_393_000));
        assert_eq!(shown(&coordinator), Some((1, 500)));

        assert!(
            coordinator
                .engine
                .set_stream_requirements(order_book_panel, generation(3), quotes)
                .expect("order book closes")
        );
        coordinator.reconcile_order_books();
        assert_eq!(
            shown(&coordinator),
            None,
            "the chart keeps the canonical book alive, but no one subscribes to the display image"
        );

        assert!(
            coordinator
                .engine
                .set_stream_requirements(
                    order_book_panel,
                    generation(3),
                    quotes.with(MarketStream::Depth),
                )
                .expect("order book reopens")
        );
        coordinator.reconcile_order_books();
        assert_eq!(
            shown(&coordinator),
            None,
            "no stale image while the new session warms up"
        );
        coordinator.handle_provider_display_depth(
            "hyperliquid",
            ProviderDisplayDepthEvent::Reset {
                display_generation: 2,
            },
        );
        coordinator.handle_provider_display_depth("hyperliquid", display_book(2, 1, 8_387_000));
        assert_eq!(
            shown(&coordinator),
            Some((2, 1)),
            "the new socket session's first book replaces nothing stale and is shown at once"
        );
    }

    #[test]
    fn depth_book_exists_only_while_at_least_one_foreground_consumer_requires_depth() {
        let mut coordinator = coordinator();
        let series = series();
        let instrument = instrument();
        coordinator.catalog.insert(
            (
                instrument.provider.clone(),
                instrument.instrument_id.clone(),
            ),
            instrument,
        );
        let first = consumer(1);
        let second = consumer(2);
        register(&mut coordinator, first);
        register(&mut coordinator, second);

        coordinator
            .engine
            .set_series_demand_with_streams(first, generation(7), &series, StreamRequirements::BARS)
            .expect("bars-only first demand installs");
        coordinator
            .engine
            .set_series_demand_with_streams(
                second,
                generation(7),
                &series,
                StreamRequirements::BARS,
            )
            .expect("bars-only second demand installs");
        coordinator.reconcile_order_books();
        assert!(coordinator.order_books.is_empty());

        let with_depth = StreamRequirements::BARS.with(MarketStream::Depth);
        assert!(
            coordinator
                .engine
                .set_stream_requirements(first, generation(7), with_depth)
                .expect("first order book opens")
        );
        coordinator.reconcile_order_books();
        assert_eq!(coordinator.order_books.len(), 1);

        assert!(
            coordinator
                .engine
                .set_stream_requirements(second, generation(7), with_depth)
                .expect("second order book opens")
        );
        assert!(
            coordinator
                .engine
                .set_stream_requirements(first, generation(7), StreamRequirements::BARS)
                .expect("first order book closes")
        );
        coordinator.reconcile_order_books();
        assert_eq!(
            coordinator.order_books.len(),
            1,
            "the shared canonical book survives while another consumer needs depth"
        );

        assert!(
            coordinator
                .engine
                .set_stream_requirements(second, generation(7), StreamRequirements::BARS)
                .expect("last order book closes")
        );
        coordinator.reconcile_order_books();
        assert!(
            coordinator.order_books.is_empty(),
            "the last depth consumer releases the canonical order book"
        );
    }

    #[test]
    fn inactive_consumer_cannot_restore_depth_after_panel_closes() {
        let mut coordinator = coordinator();
        let series = series();
        let instrument = instrument();
        coordinator.catalog.insert(
            (
                instrument.provider.clone(),
                instrument.instrument_id.clone(),
            ),
            instrument,
        );
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        let with_depth = StreamRequirements::BARS.with(MarketStream::Depth);
        coordinator
            .engine
            .set_series_demand_with_streams(consumer, generation(9), &series, with_depth)
            .expect("foreground depth demand installs");
        coordinator.reconcile_order_books();
        assert_eq!(coordinator.order_books.len(), 1);

        coordinator
            .engine
            .set_resource_class(consumer, ConsumerResourceClass::Background)
            .expect("consumer parks");
        coordinator.reconcile_order_books();
        assert!(coordinator.order_books.is_empty());

        assert!(
            coordinator
                .engine
                .set_stream_requirements(consumer, generation(9), StreamRequirements::BARS)
                .expect("panel closes while consumer is inactive")
        );
        coordinator.reconcile_order_books();
        assert!(coordinator.order_books.is_empty());

        coordinator
            .engine
            .set_resource_class(consumer, ConsumerResourceClass::Foreground)
            .expect("consumer returns to foreground");
        coordinator.reconcile_order_books();
        assert!(
            coordinator.order_books.is_empty(),
            "foreground restore must not resurrect depth after the panel closed"
        );
        assert_eq!(
            coordinator
                .engine
                .current_demand(consumer)
                .and_then(|demand| demand.generation),
            Some(generation(9))
        );
    }

    #[test]
    fn foreground_restore_recreates_rithmic_live_demand_across_idle_stop_overlap() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        let selected = series();
        let selected_instrument = instrument();
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        coordinator
            .install_provider_instrument(&selected_instrument)
            .expect("Rithmic instrument installs");
        coordinator
            .engine
            .set_series_demand_with_streams(
                consumer,
                generation(1),
                &selected,
                StreamRequirements::BARS,
            )
            .expect("foreground Rithmic demand installs");
        coordinator
            .engine
            .install_history(
                ProviderGeneration(nonzero(1)),
                &selected,
                2,
                0,
                minute_bars(0, 1),
            )
            .expect("Rithmic cached history installs");
        coordinator
            .ensure_realtime(&selected)
            .expect("initial Rithmic live handoff exists");
        assert!(coordinator.series.trade_contains_key(&selected));

        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_resource_class(
            client(1),
            consumer,
            ConsumerResourceClass::Background,
            &reply,
        );
        result
            .recv()
            .expect("background reply arrives")
            .expect("consumer backgrounds");
        assert!(!coordinator.engine.has_subscription(&selected));
        assert!(coordinator.series.trades_are_empty());

        let provider_generation = ProviderGeneration(nonzero(1));
        coordinator.session_mut("rithmic").stop_pending = Some(provider_generation);
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_resource_class(
            client(1),
            consumer,
            ConsumerResourceClass::Foreground,
            &reply,
        );
        result
            .recv()
            .expect("foreground reply arrives")
            .expect("consumer returns to foreground");
        assert!(coordinator.engine.has_subscription(&selected));
        let live = coordinator
            .series
            .trade(&selected)
            .expect("Rithmic live handoff is recreated");
        assert!(
            coordinator
                .series
                .trade_lifecycle(&selected)
                .is_some_and(SeriesLifecycle::history_ready)
        );
        assert_eq!(live.coverage(), Some((0, 0)));
        let demand = coordinator
            .rithmic_realtime_demand()
            .expect("Rithmic demand rebuilds");
        assert_eq!(demand.instruments.len(), 1);
        assert!(demand.instruments[0].trades);

        coordinator.handle_provider_event(super::super::ProviderEvent::rithmic(
            RithmicRealtimeEvent::Disconnected(1, None),
        ));
        assert!(coordinator.session("rithmic").stop_pending.is_none());
        let provider = coordinator
            .engine
            .provider_status("rithmic")
            .expect("Rithmic provider remains installed");
        assert_eq!(provider.generation, Some(provider_generation));
        assert_eq!(provider.health, ProviderHealth::Recovering);
        assert!(
            coordinator.series.trade_contains_key(&selected),
            "the idle-stop completion must restart rather than retire restored demand"
        );
    }

    #[test]
    fn idle_stop_overlap_only_counts_the_stopping_providers_order_book() {
        let mut coordinator = coordinator();
        let mut other = instrument();
        other.provider = "hyperliquid".into();
        other.instrument_id = "instrument:hyperliquid:BTC".into();
        coordinator.order_books.insert(
            (other.provider.clone(), other.instrument_id.clone()),
            ProviderOrderBook::new(other.clone(), super::super::TradeContinuity::Indexed, false),
        );
        assert!(coordinator.has_provider_live_demand("hyperliquid"));
        assert!(!coordinator.has_provider_live_demand("rithmic"));
        coordinator.order_books.insert(
            ("rithmic".into(), "instrument:rithmic:CME:MNQU6".into()),
            ProviderOrderBook::new(instrument(), super::super::TradeContinuity::Sequence, false),
        );
        assert!(coordinator.has_provider_live_demand("rithmic"));
    }

    #[test]
    fn foreground_restore_keeps_stale_cached_rithmic_baseline_partial_until_current_history() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        let selected = series();
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        coordinator
            .install_provider_instrument(&instrument())
            .expect("Rithmic instrument installs");
        coordinator
            .engine
            .set_series_demand_with_streams(
                consumer,
                generation(1),
                &selected,
                StreamRequirements::BARS,
            )
            .expect("foreground Rithmic demand installs");
        coordinator
            .engine
            .install_history(
                ProviderGeneration(nonzero(1)),
                &selected,
                2,
                0,
                minute_bars(0, 1),
            )
            .expect("generation-one cache installs");

        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_resource_class(
            client(1),
            consumer,
            ConsumerResourceClass::Background,
            &reply,
        );
        result
            .recv()
            .expect("background reply arrives")
            .expect("consumer backgrounds");
        coordinator
            .engine
            .begin_provider_session("rithmic", ProviderGeneration(nonzero(2)))
            .expect("provider advances while workspace is backgrounded");

        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_resource_class(
            client(1),
            consumer,
            ConsumerResourceClass::Foreground,
            &reply,
        );
        result
            .recv()
            .expect("foreground reply arrives")
            .expect("consumer resumes from retained stale cache");

        let live = coordinator
            .series
            .trade(&selected)
            .expect("current-generation live handoff is recreated");
        assert_eq!(live.generation, ProviderGeneration(nonzero(2)));
        assert!(
            !coordinator
                .series
                .trade_lifecycle(&selected)
                .expect("trade lifecycle")
                .history_ready()
        );
        let events = coordinator.events.get(&consumer).expect("consumer outbox");
        assert!(matches!(
            events.series.front(),
            Some(MarketRuntimeEvent::SeriesSnapshot(snapshot))
                if snapshot.snapshot.provider_generation == ProviderGeneration(nonzero(1))
        ));
        assert!(matches!(
            events.series_state,
            Some(MarketRuntimeEvent::SeriesState(ref state))
                if state.state == SeriesLoadState::Partial
                    && state.detail.as_deref() == Some("Refreshing provider coverage")
        ));
    }

    #[test]
    fn foreground_restore_recreates_candle_live_demand_across_idle_stop_overlap() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        let selected = hyperliquid_series();
        let selected_instrument = hyperliquid_instrument();
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        coordinator
            .install_provider_instrument(&selected_instrument)
            .expect("Hyperliquid instrument installs");
        coordinator
            .engine
            .set_series_demand_with_streams(
                consumer,
                generation(1),
                &selected,
                StreamRequirements::BARS,
            )
            .expect("foreground Hyperliquid demand installs");
        coordinator
            .engine
            .install_history(
                ProviderGeneration(nonzero(1)),
                &selected,
                2,
                8,
                minute_bars(0, 1),
            )
            .expect("Hyperliquid cached history installs");
        coordinator
            .ensure_realtime(&selected)
            .expect("initial Hyperliquid live handoff exists");
        assert!(coordinator.series.candle_contains_key(&selected));

        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_resource_class(
            client(1),
            consumer,
            ConsumerResourceClass::Background,
            &reply,
        );
        result
            .recv()
            .expect("background reply arrives")
            .expect("consumer backgrounds");
        assert!(!coordinator.engine.has_subscription(&selected));
        assert!(coordinator.series.candles_are_empty());

        let provider_generation = ProviderGeneration(nonzero(1));
        coordinator.session_mut("hyperliquid").stop_pending = Some(provider_generation);
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_resource_class(
            client(1),
            consumer,
            ConsumerResourceClass::Foreground,
            &reply,
        );
        result
            .recv()
            .expect("foreground reply arrives")
            .expect("consumer returns to foreground");
        assert!(coordinator.engine.has_subscription(&selected));
        let live = coordinator
            .series
            .candle(&selected)
            .expect("Hyperliquid live handoff is recreated");
        assert!(live.history_ready());
        assert_eq!(live.coverage(), Some((0, 0)));
        let demand = coordinator.hyperliquid_demand();
        assert_eq!(demand.candles.len(), 1);
        assert_eq!(demand.candles[0].instrument.wire_coin, "BTC");
        assert_eq!(demand.candles[0].interval, "1m");
        assert!(
            coordinator.session("hyperliquid").stop_pending.is_none(),
            "replacement demand must retire the cancellable idle-stop fence even without Disconnected"
        );
        assert!(coordinator.session("hyperliquid").demand_dirty);

        coordinator.handle_provider_event(super::super::ProviderEvent::hyperliquid(
            HyperliquidRealtimeEvent::Heartbeat(1, None),
        ));
        assert_eq!(
            coordinator
                .engine
                .provider_status("hyperliquid")
                .map(|status| status.health),
            Some(ProviderHealth::Online),
            "same-generation live events must be accepted when Subscribe cancels the idle Stop"
        );
        assert!(
            coordinator
                .series
                .candle(&selected)
                .is_some_and(|live| live.connected),
            "the accepted heartbeat marks the restored handoff connected"
        );
        assert!(
            coordinator.series.candle_contains_key(&selected),
            "restored Hyperliquid demand remains live without requiring Disconnected"
        );
    }

    #[test]
    fn foreground_restore_without_cache_waits_for_new_session_history_after_idle_stop() {
        let mut coordinator = coordinator();
        let (history_sender, history_receiver) = std::sync::mpsc::sync_channel(1);
        let history_sender = Box::leak(Box::new(history_sender));
        coordinator.providers.records.insert(
            "rithmic",
            super::super::ProviderDispatchRecord {
                descriptor: super::super::RITHMIC_DESCRIPTOR,
                history: history_sender,
                lifecycle: None,
                ctrader: None,
                hyperliquid: None,
                tastytrade: None,
                rithmic: None,
                catalog: None,
            },
        );
        let consumer = consumer(1);
        let selected = series();
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        coordinator
            .install_provider_instrument(&instrument())
            .expect("Rithmic instrument installs");
        coordinator
            .engine
            .set_series_demand_with_streams(
                consumer,
                generation(1),
                &selected,
                StreamRequirements::BARS,
            )
            .expect("foreground Rithmic demand installs");
        coordinator
            .ensure_realtime(&selected)
            .expect("initial Rithmic handoff exists");

        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_resource_class(
            client(1),
            consumer,
            ConsumerResourceClass::Background,
            &reply,
        );
        result
            .recv()
            .expect("background reply arrives")
            .expect("consumer backgrounds");
        let first_generation = ProviderGeneration(nonzero(1));
        coordinator
            .engine
            .end_provider_session("rithmic", first_generation)
            .expect("completed idle stop ends the first provider session");
        assert!(coordinator.engine.series_snapshot(&selected).is_none());

        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_resource_class(
            client(1),
            consumer,
            ConsumerResourceClass::Foreground,
            &reply,
        );
        result.recv().expect("foreground reply arrives").expect(
            "foreground resume waits for the worker instead of failing unavailable history",
        );
        assert!(coordinator.engine.has_subscription(&selected));
        let live = coordinator
            .series
            .trade_lifecycle(&selected)
            .expect("Rithmic handoff is recreated before reconnect");
        assert!(!live.history_ready());
        assert_eq!(live.generation, first_generation);
        assert!(matches!(
            history_receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));

        coordinator.handle_provider_event(super::super::ProviderEvent::rithmic(
            RithmicRealtimeEvent::Connecting(2),
        ));
        let second_generation = ProviderGeneration(nonzero(2));
        let provider = coordinator
            .engine
            .provider_status("rithmic")
            .expect("Rithmic provider remains installed");
        assert_eq!(provider.generation, Some(second_generation));
        assert_eq!(provider.health, ProviderHealth::Connecting);
        let live = coordinator
            .series
            .trade_lifecycle(&selected)
            .expect("Rithmic handoff survives reconnect");
        assert_eq!(live.generation, second_generation);
        assert!(!live.history_ready());
        let history = history_receiver
            .try_recv()
            .expect("new provider session enqueues covering history");
        assert_eq!(history.series, selected);
        assert_eq!(history.provider_generation, second_generation);
        assert!(history.range.is_none());
    }

    fn queued_state(_consumer_id: ConsumerId, value: u64) -> MarketRuntimeEvent {
        MarketRuntimeEvent::ProviderState(aeris_contracts::ProviderState {
            provider: "fixture".to_string(),
            state: ProviderConnectionState::Online,
            generation: value,
            detail: None,
            transport_rtt_nanos: None,
        })
    }

    #[test]
    fn client_poll_batch_is_bounded_fair_and_scoped_to_its_owner() {
        let mut coordinator = coordinator();
        let first_client = client(1);
        let second_client = client(2);
        coordinator.attached.insert(first_client);
        coordinator.attached.insert(second_client);

        let first = consumer(1);
        let second = consumer(2);
        let foreign = consumer(3);
        for (consumer_id, owner) in [
            (first, first_client),
            (second, first_client),
            (foreign, second_client),
        ] {
            coordinator.consumer_clients.insert(consumer_id, owner);
            coordinator
                .consumer_epochs
                .insert(consumer_id, coordinator.consumer_epoch_sequence.next());
            coordinator
                .events
                .insert(consumer_id, ConsumerEvents::default());
        }
        for value in 1..=300 {
            coordinator
                .events
                .get_mut(&first)
                .expect("first outbox")
                .series
                .push_back(queued_state(first, value));
        }
        coordinator
            .events
            .get_mut(&second)
            .expect("second outbox")
            .series
            .push_back(queued_state(second, 1));
        coordinator
            .events
            .get_mut(&foreign)
            .expect("foreign outbox")
            .series
            .push_back(queued_state(foreign, 1));

        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_drain_client(
            first_client,
            vec![
                (first, REALTIME_DRAIN_BUDGET),
                (second, REALTIME_DRAIN_BUDGET),
            ],
            &reply,
        );
        let batch = result
            .recv()
            .expect("poll reply arrives")
            .expect("attached client polls");

        assert_eq!(batch.len(), REALTIME_DRAIN_BUDGET);
        assert_eq!(batch[0].0, first.0.get());
        assert_eq!(batch[1].0, second.0.get());
        assert_eq!(batch[2].0, first.0.get());
        assert!(
            batch
                .iter()
                .all(|(consumer_id, _)| *consumer_id != foreign.0.get())
        );
        assert!(
            coordinator
                .events
                .get_mut(&foreign)
                .expect("foreign outbox remains")
                .pop()
                .is_some(),
            "polling one client must not steal another client's event"
        );
    }

    #[test]
    fn client_poll_respects_each_consumers_available_ui_capacity() {
        let mut coordinator = coordinator();
        let owner = client(1);
        coordinator.attached.insert(owner);
        let constrained = consumer(1);
        let available = consumer(2);
        for consumer_id in [constrained, available] {
            coordinator.consumer_clients.insert(consumer_id, owner);
            coordinator
                .consumer_epochs
                .insert(consumer_id, coordinator.consumer_epoch_sequence.next());
            coordinator
                .events
                .insert(consumer_id, ConsumerEvents::default());
            for value in 1..=8 {
                coordinator
                    .events
                    .get_mut(&consumer_id)
                    .expect("consumer outbox")
                    .series
                    .push_back(queued_state(consumer_id, value));
            }
        }

        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_drain_client(owner, vec![(constrained, 1), (available, 4)], &reply);
        let batch = result
            .recv()
            .expect("poll reply arrives")
            .expect("attached client polls");

        assert_eq!(batch.len(), 5);
        assert_eq!(
            batch
                .iter()
                .filter(|(consumer_id, _)| *consumer_id == constrained.0.get())
                .count(),
            1,
            "a nearly full pane may consume only its one available mailbox slot"
        );
        assert_eq!(
            batch
                .iter()
                .filter(|(consumer_id, _)| *consumer_id == available.0.get())
                .count(),
            4,
            "another pane must keep draining instead of being stalled by its neighbor"
        );
        assert_eq!(
            coordinator
                .events
                .get(&constrained)
                .expect("constrained outbox")
                .series
                .len(),
            7
        );
        assert_eq!(
            coordinator
                .events
                .get(&available)
                .expect("available outbox")
                .series
                .len(),
            4
        );
    }

    #[test]
    fn catalog_search_callbacks_are_fenced_before_the_consumer_outbox() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        coordinator.command_fences.record(
            consumer,
            "rithmic".into(),
            super::super::CommandKind::Search,
            8,
        );

        coordinator.handle_catalog_search(ProviderInstrumentSearchResult {
            consumer_id: consumer.0.get(),
            provider: "rithmic".to_string(),
            provider_generation: 1,
            search_generation: 7,
            instruments: Vec::new(),
        });
        assert!(
            coordinator
                .events
                .get(&consumer)
                .expect("consumer outbox")
                .catalog_search
                .is_none(),
            "a superseded provider callback never reaches presentation"
        );
        assert!(coordinator.command_fences.accepts(
            consumer,
            "rithmic",
            super::super::CommandKind::Search,
            8
        ));

        coordinator.handle_catalog_search(ProviderInstrumentSearchResult {
            consumer_id: consumer.0.get(),
            provider: "rithmic".to_string(),
            provider_generation: 1,
            search_generation: 8,
            instruments: Vec::new(),
        });
        assert!(matches!(
            coordinator
                .events
                .get(&consumer)
                .expect("consumer outbox")
                .catalog_search,
            Some(MarketRuntimeEvent::ProviderInstrumentSearchResult(ref result))
                if result.search_generation == 8
        ));
        assert_eq!(coordinator.command_fences.count_for(consumer), 1);
        assert!(coordinator.command_fences.accepts(
            consumer,
            "rithmic",
            super::super::CommandKind::Search,
            8
        ));
    }

    fn market_screen(consumer: ConsumerId, screen_generation: u64) -> ProviderMarketScreen {
        ProviderMarketScreen {
            consumer_id: consumer.0.get(),
            provider: "hyperliquid".to_string(),
            provider_generation: 1,
            screen_generation,
            captured_at_unix_millis: 1_700_000_000_000,
            markets: Vec::new(),
        }
    }

    #[test]
    fn market_screen_callbacks_are_fenced_to_the_latest_generation() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        coordinator.command_fences.record(
            consumer,
            "hyperliquid".into(),
            super::super::CommandKind::Screen,
            4,
        );

        coordinator.handle_catalog_screen(market_screen(consumer, 3));
        assert!(
            coordinator
                .events
                .get(&consumer)
                .expect("consumer outbox")
                .catalog_screen
                .is_none(),
            "a superseded screen never reaches presentation"
        );
        assert!(coordinator.command_fences.accepts(
            consumer,
            "hyperliquid",
            super::super::CommandKind::Screen,
            4
        ));

        coordinator.handle_catalog_screen(market_screen(consumer, 4));
        assert!(matches!(
            coordinator
                .events
                .get(&consumer)
                .expect("consumer outbox")
                .catalog_screen,
            Some(MarketRuntimeEvent::ProviderMarketScreen(ref screen))
                if screen.screen_generation == 4
        ));
        assert_eq!(coordinator.command_fences.count_for(consumer), 0);

        coordinator.handle_catalog_screen(market_screen(consumer, 4));
        assert!(
            coordinator
                .events
                .get_mut(&consumer)
                .expect("consumer outbox")
                .catalog_screen
                .take()
                .is_some_and(|event| matches!(
                    event,
                    MarketRuntimeEvent::ProviderMarketScreen(ref screen)
                        if screen.screen_generation == 4
                )),
            "a duplicate completion leaves the delivered screen untouched"
        );
    }

    #[test]
    fn overflowed_catalog_rejects_pending_market_screens() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        coordinator.command_fences.record(
            consumer,
            "hyperliquid".into(),
            super::super::CommandKind::Screen,
            9,
        );

        coordinator.reject_overflowed_catalog("hyperliquid");

        assert_eq!(coordinator.command_fences.count_for(consumer), 0);
        assert!(matches!(
            coordinator
                .events
                .get(&consumer)
                .expect("consumer outbox")
                .catalog_screen,
            Some(MarketRuntimeEvent::ProviderMarketScreenRejected(ref rejection))
                if rejection.command_generation == 9
        ));
    }

    #[test]
    fn preliminary_catalog_result_keeps_the_remote_completion_fenced() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        coordinator.command_fences.record(
            consumer,
            "tastytrade".into(),
            super::super::CommandKind::Search,
            8,
        );
        let result = ProviderInstrumentSearchResult {
            consumer_id: consumer.0.get(),
            provider: "tastytrade".into(),
            provider_generation: 1,
            search_generation: 8,
            instruments: Vec::new(),
        };
        coordinator.handle_catalog_search_preview(result.clone());
        assert!(coordinator.command_fences.accepts(
            consumer,
            "tastytrade",
            super::super::CommandKind::Search,
            8
        ));
        assert!(matches!(
            coordinator.events.get(&consumer).unwrap().catalog_search,
            Some(MarketRuntimeEvent::ProviderInstrumentSearchPreview(_))
        ));
        coordinator.handle_catalog_search(result);
        assert_eq!(coordinator.command_fences.count_for(consumer), 1);
    }

    #[test]
    fn startup_and_user_searches_have_independent_fences() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        coordinator.command_fences.record(
            consumer,
            "tastytrade".into(),
            super::super::CommandKind::StartupResolve,
            17,
        );
        coordinator.command_fences.record(
            consumer,
            "tastytrade".into(),
            super::super::CommandKind::Search,
            1,
        );

        coordinator.handle_catalog_search(ProviderInstrumentSearchResult {
            consumer_id: consumer.0.get(),
            provider: "tastytrade".into(),
            provider_generation: 1,
            search_generation: 17,
            instruments: Vec::new(),
        });
        assert!(!coordinator.command_fences.accepts(
            consumer,
            "tastytrade",
            super::super::CommandKind::StartupResolve,
            17
        ));
        assert!(coordinator.command_fences.accepts(
            consumer,
            "tastytrade",
            super::super::CommandKind::Search,
            1
        ));

        coordinator.handle_catalog_search(ProviderInstrumentSearchResult {
            consumer_id: consumer.0.get(),
            provider: "tastytrade".into(),
            provider_generation: 1,
            search_generation: 1,
            instruments: Vec::new(),
        });
        assert_eq!(coordinator.command_fences.count_for(consumer), 1);

        let events = coordinator
            .events
            .get_mut(&consumer)
            .expect("consumer outbox");
        let delivered = std::iter::from_fn(|| events.pop())
            .filter_map(|event| match event {
                MarketRuntimeEvent::ProviderInstrumentSearchResult(result) => {
                    Some(result.search_generation)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            delivered,
            [17, 1],
            "a picker result must not replace the pending startup re-resolution"
        );
    }

    #[test]
    fn catalog_search_result_selection_preserves_search_identity_for_broker_providers() {
        use super::super::descriptors::{InstrumentRequest, SelectionRequest};
        for descriptor in [
            super::super::ctrader::DESCRIPTOR,
            super::super::tastytrade::DESCRIPTOR,
        ] {
            let mut coordinator = coordinator();
            let owner = consumer(1);
            register(&mut coordinator, owner);
            coordinator.consumer_clients.insert(owner, client(1));
            coordinator.events.insert(owner, ConsumerEvents::default());
            let (commands, dispatched) = std::sync::mpsc::sync_channel(4);
            let (_events, incoming) =
                std::sync::mpsc::sync_channel::<super::super::ProviderCatalogEvent>(4);
            let catalog = super::super::ProviderCatalogChannelImpl::new(
                commands,
                incoming,
                |sender, _, command| sender.try_send(command).map_err(|error| error.to_string()),
            );
            let (history, _requests) = std::sync::mpsc::sync_channel(4);
            coordinator.providers.records.insert(
                descriptor.id,
                super::super::ProviderDispatchRecord {
                    descriptor,
                    history: &history,
                    lifecycle: None,
                    ctrader: None,
                    hyperliquid: None,
                    tastytrade: None,
                    rithmic: None,
                    catalog: Some(&catalog),
                },
            );
            let search = aeris_contracts::SearchProviderInstruments {
                consumer_id: owner.0.get(),
                search_generation: 7,
                startup_resolve: false,
                provider: descriptor.id.into(),
                query: "EURUSD".into(),
                maximum_results: 16,
                categories: aeris_contracts::InstrumentSearchCategories::ALL,
            };
            let (reply, accepted) = std::sync::mpsc::sync_channel(1);
            coordinator.handle_provider_search(client(1), search, &reply);
            accepted.recv().unwrap().unwrap();
            assert!(matches!(dispatched.try_recv().unwrap(),
                super::super::ProviderCatalogCommand::Search(search)
                    if search.search_generation == 7));
            coordinator.handle_catalog_search(aeris_contracts::ProviderInstrumentSearchResult {
                consumer_id: owner.0.get(),
                provider: descriptor.id.into(),
                provider_generation: 1,
                search_generation: 7,
                instruments: Vec::new(),
            });
            assert!(matches!(
                coordinator.events.get_mut(&owner).unwrap().pop(),
                Some(MarketRuntimeEvent::ProviderInstrumentSearchResult(_))
            ));
            let request = SelectionRequest {
                consumer_id: owner.0.get(),
                generation: 9,
                instrument: InstrumentRequest::CatalogSymbol {
                    provider_id: descriptor.id.into(),
                    symbol: "EURUSD".into(),
                    exchange: "demo".into(),
                    entitlement_id: descriptor.presentation.selection_entitlement_id.into(),
                    search_generation: 7,
                },
                interval: aeris_market_data::ChartInterval::Minute1,
                streams: StreamRequirements::BARS,
                viewport: None,
            };
            coordinator.begin_selection(client(1), request).unwrap();
            assert!(
                matches!(dispatched.try_recv().unwrap(),
                super::super::ProviderCatalogCommand::Select(selection)
                    if selection.search_generation == 7 && selection.selection_generation == 9),
                "{} must select against the search that produced the result",
                descriptor.id
            );
        }
    }

    #[test]
    fn retired_epoch_connected_does_not_publish_catalog_ready() {
        use aeris_market_engine::fence::{EpochSequence, ProviderTag};

        let mut coordinator = coordinator();
        let owner = consumer(1);
        register(&mut coordinator, owner);
        coordinator.events.insert(owner, ConsumerEvents::default());
        let (history, _requests) = std::sync::mpsc::sync_channel(1);
        coordinator.providers.records.insert(
            "ctrader",
            super::super::ProviderDispatchRecord {
                descriptor: super::super::ctrader::DESCRIPTOR,
                history: &history,
                lifecycle: None,
                ctrader: None,
                hyperliquid: None,
                tastytrade: None,
                rithmic: None,
                catalog: None,
            },
        );
        assert!(
            super::super::ctrader::DESCRIPTOR
                .presentation
                .catalog_refresh_on_startup
        );
        let mut epochs = EpochSequence::<ProviderTag>::default();
        let retired = epochs.next();
        let current = epochs.next();
        coordinator.provider_epochs.bind("ctrader", current);
        let connected = |epoch| super::super::ProviderEvent {
            provider: "ctrader",
            generation: epoch,
            wire_generation: epoch.get().get(),
            kind: super::super::ProviderEventKind::Connected,
        };
        coordinator.handle_bound_provider_event(connected(retired));
        assert!(
            coordinator.events.get_mut(&owner).unwrap().pop().is_none(),
            "retired Connected must not publish CatalogReady"
        );
        assert_eq!(coordinator.provider_epochs.stale_counts().0, 1);
        coordinator.handle_bound_provider_event(connected(current));
        assert!(matches!(
            coordinator.events.get_mut(&owner).unwrap().pop(),
            Some(MarketRuntimeEvent::CatalogReady { provider }) if provider == "ctrader"
        ));
    }

    #[test]
    fn superseded_startup_search_result_never_reaches_consumer() {
        let mut coordinator = coordinator();
        let owner = consumer(1);
        register(&mut coordinator, owner);
        coordinator.events.insert(owner, ConsumerEvents::default());
        for generation in [17, 18] {
            coordinator.command_fences.record(
                owner,
                "tastytrade".into(),
                super::super::CommandKind::StartupResolve,
                generation,
            );
        }
        for generation in [17, 18] {
            coordinator.handle_catalog_search(ProviderInstrumentSearchResult {
                consumer_id: owner.0.get(),
                provider: "tastytrade".into(),
                provider_generation: 1,
                search_generation: generation,
                instruments: Vec::new(),
            });
        }
        let events = coordinator.events.get_mut(&owner).expect("outbox");
        assert!(matches!(
            events.startup_catalog_search.take(),
            Some(MarketRuntimeEvent::ProviderInstrumentSearchResult(result))
                if result.search_generation == 18
        ));
        assert_eq!(coordinator.command_fences.count_for(owner), 0);
    }

    #[test]
    fn startup_and_user_selections_reach_the_consumer_independently() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        coordinator.command_fences.record_operation(
            consumer,
            "rithmic".into(),
            super::super::CommandKind::StartupResolve,
            17,
            true,
        );
        coordinator.command_fences.record_operation(
            consumer,
            "rithmic".into(),
            super::super::CommandKind::Select,
            3,
            true,
        );

        coordinator.handle_catalog_selection(consumer.0.get(), 17, instrument());
        coordinator.handle_catalog_rejection(
            aeris_contracts::ProviderCatalogRejected {
                consumer_id: consumer.0.get(),
                provider: "rithmic".into(),
                provider_generation: None,
                command_generation: 3,
                reason: aeris_contracts::ProviderCatalogRejectionReason::InstrumentUnavailable,
            },
            true,
        );

        let events = coordinator
            .events
            .get_mut(&consumer)
            .expect("consumer outbox");
        let delivered = std::iter::from_fn(|| events.pop()).collect::<Vec<_>>();
        assert!(delivered.iter().any(|event| matches!(
            event,
            MarketRuntimeEvent::ProviderInstrumentSelection(selection)
                if selection.command_generation
                    == 17
        )));
        assert!(delivered.iter().any(|event| matches!(
            event,
            MarketRuntimeEvent::ProviderCatalogRejected(rejection)
                if rejection.command_generation == 3
        )));
    }

    #[test]
    fn catalog_selection_callbacks_are_fenced_before_installation() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        coordinator.command_fences.record_operation(
            consumer,
            "rithmic".into(),
            super::super::CommandKind::Select,
            12,
            true,
        );

        let mut stale = instrument();
        stale.selection_generation = 11;
        coordinator.handle_catalog_selection(consumer.0.get(), 11, stale);
        assert!(coordinator.catalog.is_empty());
        assert!(
            coordinator
                .events
                .get(&consumer)
                .expect("consumer outbox")
                .catalog_selection
                .is_none()
        );
        assert!(coordinator.command_fences.accepts(
            consumer,
            "rithmic",
            super::super::CommandKind::Select,
            12
        ));

        let mut current = instrument();
        current.selection_generation = 12;
        coordinator.handle_catalog_selection(consumer.0.get(), 12, current.clone());
        assert_eq!(
            coordinator
                .catalog
                .get(&(current.provider.clone(), current.instrument_id.clone())),
            Some(&current)
        );
        assert!(matches!(
            coordinator
                .events
                .get(&consumer)
                .expect("consumer outbox")
                .catalog_selection,
            Some(MarketRuntimeEvent::ProviderInstrumentSelection(ref selection))
                if selection.command_generation == 12
        ));
        assert_eq!(coordinator.command_fences.count_for(consumer), 0);
    }

    #[test]
    fn consumer_registry_removes_every_per_consumer_resource_on_both_paths() {
        for detach in [false, true] {
            let mut coordinator = coordinator();
            let owner = consumer(1);
            let selected = series();
            coordinator.attached.insert(client(1));
            coordinator
                .handle_register(ConsumerIdentity {
                    client_id: client(1),
                    workspace_id: WorkspaceId(nonzero(1)),
                    consumer_id: owner,
                })
                .expect("consumer registers");
            coordinator
                .engine
                .begin_provider_session("rithmic", ProviderGeneration(nonzero(1)))
                .expect("provider session");
            coordinator
                .engine
                .set_series_demand(owner, generation(1), &selected)
                .expect("series demand");
            coordinator
                .engine
                .set_stream_requirements(
                    owner,
                    generation(1),
                    StreamRequirements::BARS.with(MarketStream::Depth),
                )
                .expect("depth demand");
            for kind in [
                super::super::CommandKind::Search,
                super::super::CommandKind::Select,
                super::super::CommandKind::Screen,
                super::super::CommandKind::StartupResolve,
            ] {
                coordinator
                    .command_fences
                    .record(owner, "rithmic".into(), kind, 10);
            }
            assert_eq!(coordinator.command_fences.count_for(owner), 4);
            assert!(coordinator.engine.has_subscription(&selected));
            if detach {
                coordinator.attached.remove(&client(1));
                coordinator.detach_client(client(1));
            } else {
                coordinator
                    .handle_remove(client(1), owner)
                    .expect("consumer removes");
            }
            assert_eq!(coordinator.command_fences.count_for(owner), 0);
            assert!(!coordinator.events.contains_key(&owner));
            assert!(!coordinator.consumer_clients.contains_key(&owner));
            assert!(!coordinator.engine.has_subscription(&selected));
            assert!(coordinator.engine.current_demand(owner).is_none());
            assert!(coordinator.order_books.is_empty());
        }
    }
}
