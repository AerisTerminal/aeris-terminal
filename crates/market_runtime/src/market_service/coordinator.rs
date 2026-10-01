use super::{
    Arc, AtomicBool, BTreeMap, BTreeSet, BarSeriesKey, COORDINATOR_TICK, CandleLiveHandoff,
    ClientId, Command, ConsumerEvents, ConsumerId, ConsumerIdentity, ConsumerResourceClass,
    DeferredHistoryRequest, DemandWaiter, Duration, EngineError, GenerationId, HistoryRange,
    InstallProviderInstrument, Instant, LiveHistoryState, MAXIMUM_STUDIES,
    MAXIMUM_STUDY_DEPENDENCIES, MAXIMUM_STUDY_OUTPUTS, MAXIMUM_STUDY_POINTS_PER_OUTPUT,
    MAXIMUM_STUDY_STATE_BYTES_PER_INSTANCE, MAXIMUM_STUDY_TOTAL_OUTPUT_POINTS,
    MAXIMUM_STUDY_TOTAL_STATE_BYTES, MarketEngine, MarketServiceStatus, MarketStream, NonZeroUsize,
    Ordering, PriceAlertRegistry, ProviderConnectionState, ProviderDispatch, ProviderGeneration,
    ProviderHealth, ProviderOrderBook, ProviderRuntimeEvent, ProviderRuntimeRegistry,
    ProviderState, REALTIME_CAPACITY, REALTIME_DRAIN_BUDGET, Receiver, RecvTimeoutError, Reply,
    RithmicLiveHandoff, RithmicRealtimeDemand, SeriesLoadState, StreamRequirements,
    StudyMarketLeaseChangeKind, StudyRuntime, StudyRuntimeConfig, authorize_consumer,
    publish_state, thread,
};
use crate::{
    MarketRuntimeEvent, MarketStudyOutputSnapshot,
    study::{
        StudyExecutionBatch, StudyLiveMarketData, StudyMarketInput, StudyNonBarChange,
        StudyRuntimeError,
    },
};

pub(super) struct OwnedCoordinatorChannels {
    pub(super) commands: Receiver<Command>,
    pub(super) providers: ProviderRuntimeRegistry,
}

pub(super) fn spawn_coordinator(
    engine: MarketEngine,
    mut channels: OwnedCoordinatorChannels,
    shutdown: Arc<AtomicBool>,
) -> Result<thread::JoinHandle<()>, String> {
    thread::Builder::new()
        .name("aeris-market-engine".to_string())
        .spawn(move || {
            {
                let providers = channels.providers.dispatch();
                run_coordinator(engine, &channels.commands, providers, &shutdown);
            }
            let panicked = channels.providers.cancel_and_join();
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
) {
    let mut coordinator = Coordinator {
        engine,
        studies: StudyRuntime::new(StudyRuntimeConfig {
            maximum_studies: NonZeroUsize::new(MAXIMUM_STUDIES).unwrap_or(NonZeroUsize::MIN),
            maximum_dependencies_per_study: NonZeroUsize::new(MAXIMUM_STUDY_DEPENDENCIES)
                .unwrap_or(NonZeroUsize::MIN),
            maximum_outputs_per_study: NonZeroUsize::new(MAXIMUM_STUDY_OUTPUTS)
                .unwrap_or(NonZeroUsize::MIN),
            maximum_points_per_output: NonZeroUsize::new(MAXIMUM_STUDY_POINTS_PER_OUTPUT)
                .unwrap_or(NonZeroUsize::MIN),
            maximum_total_output_points: NonZeroUsize::new(MAXIMUM_STUDY_TOTAL_OUTPUT_POINTS)
                .unwrap_or(NonZeroUsize::MIN),
            maximum_state_bytes_per_study: NonZeroUsize::new(
                MAXIMUM_STUDY_STATE_BYTES_PER_INSTANCE,
            )
            .unwrap_or(NonZeroUsize::MIN),
            maximum_total_state_bytes: NonZeroUsize::new(MAXIMUM_STUDY_TOTAL_STATE_BYTES)
                .unwrap_or(NonZeroUsize::MIN),
        }),
        providers,
        attached: BTreeSet::new(),
        consumer_clients: BTreeMap::new(),
        pending: BTreeMap::new(),
        history_inflight: BTreeMap::new(),
        history_deferred: BTreeMap::new(),
        history_confirmed_empty: BTreeMap::new(),
        detached_history: BTreeSet::new(),
        history_cancellations: BTreeMap::new(),
        history_retries: BTreeMap::new(),
        events: BTreeMap::new(),
        rithmic_live: BTreeMap::new(),
        candle_live: BTreeMap::new(),
        order_books: BTreeMap::new(),
        hyperliquid_display_depth: BTreeMap::new(),
        hyperliquid_display_generation: 0,
        price_alerts: PriceAlertRegistry::default(),
        delta_divergence_watermarks: BTreeMap::new(),
        catalog: BTreeMap::new(),
        catalog_sessions: BTreeMap::new(),
        catalog_searches: BTreeMap::new(),
        catalog_selections: BTreeMap::new(),
        rithmic_demand: None,
        rithmic_pending_demand: None,
        rithmic_stop_pending: None,
        tastytrade_demand: None,
        tastytrade_authorization: None,
        tastytrade_suspended: false,
        tastytrade_generation_floor: 0,
        tastytrade_recovery: None,
        hyperliquid_engaged: false,
        hyperliquid_demand_dirty: false,
        hyperliquid_stop_pending: None,
        hyperliquid_catalog_degraded: None,
    };
    loop {
        if shutdown.load(Ordering::Acquire) {
            drain_shutdown_provider_events(&mut coordinator);
            coordinator.begin_shutdown();
            return;
        }
        coordinator.studies.begin_turn();
        let drained = drain_coordinator_events(&mut coordinator);
        coordinator.broadcast_dirty_trade_tapes();
        coordinator.publish_rithmic_live();
        coordinator.publish_candle_live();
        coordinator.recover_overflowed_series_queues();
        coordinator.flush_rithmic_demand();
        coordinator.flush_hyperliquid_demand();
        coordinator.flush_tastytrade_demand();
        coordinator.stop_realtime_if_idle();
        coordinator.retry_history();
        match commands.recv_timeout(if drained >= REALTIME_DRAIN_BUDGET {
            Duration::ZERO
        } else {
            COORDINATOR_TICK
        }) {
            Ok(command) if !shutdown.load(Ordering::Acquire) => {
                coordinator.handle_command(command);
            }
            Ok(_) => {
                drain_shutdown_provider_events(&mut coordinator);
                coordinator.begin_shutdown();
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn drain_coordinator_events(coordinator: &mut Coordinator<'_>) -> usize {
    if let Some(wake) = coordinator.providers.wake {
        for (index, provider) in [(0, "rithmic"), (1, "hyperliquid"), (2, "tastytrade")] {
            if wake.catalog_overflow[index].swap(false, Ordering::AcqRel) {
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
    'drain: for _ in 0..REALTIME_DRAIN_BUDGET {
        for offset in 0..7 {
            let lane = (first_lane + offset) % 7;
            if let Some(wake) = coordinator.providers.wake {
                wake.drain_cursor.store((lane + 1) % 7, Ordering::Relaxed);
                if started.elapsed() >= Duration::from_millis(4) {
                    wake.notify();
                    break 'drain;
                }
            }
            let Some(event) = coordinator.providers.take_event(lane) else {
                continue;
            };
            drained += 1;
            match event {
                ProviderRuntimeEvent::TastytradeRealtime(event) => {
                    coordinator.handle_tastytrade_realtime(event);
                }
                ProviderRuntimeEvent::TastytradeCatalog(event) => {
                    coordinator.handle_tastytrade_catalog(event);
                }
                ProviderRuntimeEvent::RithmicRealtime(event) => {
                    if coordinator
                        .providers
                        .wake
                        .is_some_and(|wake| wake.overflowed(0, event.generation()))
                    {
                        continue;
                    }
                    coordinator.handle_rithmic_realtime(event);
                }
                ProviderRuntimeEvent::RithmicCatalog(event) => {
                    coordinator.handle_rithmic_catalog(event);
                }
                ProviderRuntimeEvent::HyperliquidRealtime(event) => {
                    if coordinator
                        .providers
                        .wake
                        .is_some_and(|wake| wake.overflowed(1, event.generation()))
                    {
                        continue;
                    }
                    coordinator.handle_hyperliquid_realtime(event);
                }
                ProviderRuntimeEvent::HyperliquidDisplayDepth(event) => {
                    coordinator.handle_hyperliquid_display_depth(event);
                }
                ProviderRuntimeEvent::HyperliquidCatalog(event) => {
                    coordinator.handle_hyperliquid_catalog(event);
                }
            }
        }
    }
    if let Some(wake) = coordinator.providers.wake {
        let rithmic = wake.pending_overflow[0].swap(0, Ordering::AcqRel);
        let hyperliquid = wake.pending_overflow[1].swap(0, Ordering::AcqRel);
        let tastytrade = wake.pending_overflow[2].swap(0, Ordering::AcqRel);
        if tastytrade != 0 {
            coordinator.candle_provider_recovering(
                "tastytrade",
                tastytrade,
                "Tastytrade queue overflow requires recovery",
            );
        }
        if rithmic != 0 {
            coordinator.rithmic_recovering(
                rithmic,
                "Local market event queue overflow; repairing continuity",
            );
        }
        if hyperliquid != 0 {
            coordinator.candle_provider_recovering(
                "hyperliquid",
                hyperliquid,
                "Local market event queue overflow; repairing continuity",
            );
        }
    }
    drained
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
    rithmic_live: &BTreeMap<BarSeriesKey, RithmicLiveHandoff>,
    candle_live: &BTreeMap<BarSeriesKey, CandleLiveHandoff>,
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
    if input.series.provider_id == "rithmic" {
        return rithmic_live.get(&input.series).is_some_and(|live| {
            live.generation == provider_generation
                && live.connected
                && live.history_state == LiveHistoryState::Ready
        });
    }
    if matches!(
        input.series.provider_id.as_str(),
        "hyperliquid" | "tastytrade"
    ) {
        return candle_live.get(&input.series).is_some_and(|live| {
            live.generation == provider_generation
                && live.connected
                && live.history_state == LiveHistoryState::Ready
        });
    }
    false
}

fn study_live_market_data<'a>(
    engine: &MarketEngine,
    order_books: &'a BTreeMap<(String, String), ProviderOrderBook>,
    rithmic_live: &BTreeMap<BarSeriesKey, RithmicLiveHandoff>,
    candle_live: &BTreeMap<BarSeriesKey, CandleLiveHandoff>,
    input: &StudyMarketInput,
) -> Option<StudyLiveMarketData<'a>> {
    if !study_market_series_live_ready(engine, rithmic_live, candle_live, input) {
        return None;
    }
    let provider = engine.provider_status(&input.series.provider_id)?;
    let provider_generation = provider.generation?;
    let generation = provider_generation.0.get();
    order_books
        .values()
        .find_map(|book| book.study_live_market_data(input, generation))
}

pub(super) struct Coordinator<'a> {
    pub(super) engine: MarketEngine,
    pub(super) studies: StudyRuntime,
    pub(super) providers: ProviderDispatch<'a>,
    pub(super) attached: BTreeSet<ClientId>,
    pub(super) consumer_clients: BTreeMap<ConsumerId, ClientId>,
    pub(super) pending: BTreeMap<BarSeriesKey, Vec<DemandWaiter>>,
    pub(super) history_inflight: BTreeMap<(BarSeriesKey, ProviderGeneration), Option<HistoryRange>>,
    pub(super) history_deferred:
        BTreeMap<(BarSeriesKey, ProviderGeneration), DeferredHistoryRequest>,
    /// Provider-confirmed empty suffix for the current backwards history edge.
    /// This is request coverage metadata only; canonical bars remain owned by
    /// `MarketEngine::SeriesStore`.
    pub(super) history_confirmed_empty: BTreeMap<(BarSeriesKey, ProviderGeneration), HistoryRange>,
    /// Series whose bounded canonical working window is intentionally away from
    /// the live tail. The provider handoff remains active; live publication is
    /// gated until current history reseeds this canonical window.
    pub(super) detached_history: BTreeSet<(BarSeriesKey, ProviderGeneration)>,
    pub(super) history_cancellations: BTreeMap<(BarSeriesKey, ProviderGeneration), Arc<AtomicBool>>,
    pub(super) history_retries:
        BTreeMap<(BarSeriesKey, ProviderGeneration), (Instant, u8, Option<HistoryRange>)>,
    pub(super) events: BTreeMap<ConsumerId, ConsumerEvents>,
    pub(super) rithmic_live: BTreeMap<BarSeriesKey, RithmicLiveHandoff>,
    pub(super) candle_live: BTreeMap<BarSeriesKey, CandleLiveHandoff>,
    pub(super) order_books: BTreeMap<(String, String), ProviderOrderBook>,
    /// Provider-aggregated Hyperliquid depth used only for DOM presentation.
    /// Canonical `order_books` remain sourced from the standard full-precision
    /// L2 subscription.
    pub(super) hyperliquid_display_depth: BTreeMap<String, crate::MarketDisplayDepth>,
    pub(super) hyperliquid_display_generation: u64,
    /// Bounded alert definitions, baselines, and trigger evaluation. Provider
    /// workers remain shared with ordinary market demand.
    pub(super) price_alerts: PriceAlertRegistry,
    /// Latest completed bar evaluated for each consumer's deterministic delta
    /// divergence rule. A generation change makes the prior watermark stale.
    pub(super) delta_divergence_watermarks: BTreeMap<ConsumerId, (GenerationId, u64)>,
    pub(super) catalog: BTreeMap<(String, String), InstallProviderInstrument>,
    pub(super) catalog_sessions: BTreeMap<String, u64>,
    /// Latest accepted provider-catalog command generation per consumer/provider.
    /// Provider callbacks are fenced here before entering the canonical consumer
    /// outbox, so desktop presentation code never needs a second pending-command map.
    pub(super) catalog_searches: BTreeMap<(ConsumerId, String), u64>,
    pub(super) catalog_selections: BTreeMap<(ConsumerId, String), u64>,
    /// Complete Rithmic provider demand last accepted by the live worker.
    pub(super) rithmic_demand: Option<RithmicRealtimeDemand>,
    /// A replacement subscription set the control channel could not take yet.
    /// One slot, newest wins: the worker applies complete replacement intent,
    /// so an intermediate set never has to survive a full control queue.
    pub(super) rithmic_pending_demand: Option<RithmicRealtimeDemand>,
    pub(super) rithmic_stop_pending: Option<ProviderGeneration>,
    /// Whether the Hyperliquid worker currently holds a subscription set.
    ///
    /// The coordinator rebuilds the whole desired set and the worker diffs it;
    /// this flag only decides idle shutdown.
    pub(super) tastytrade_authorization: Option<bool>,
    pub(super) tastytrade_suspended: bool,
    pub(super) tastytrade_generation_floor: u64,
    pub(super) tastytrade_recovery: Option<u64>,
    pub(super) tastytrade_demand: Option<super::tastytrade::Demand>,
    pub(super) hyperliquid_engaged: bool,
    /// Set whenever live handoffs or depth demand change the desired
    /// Hyperliquid subscriptions; cleared once the worker accepts the set.
    pub(super) hyperliquid_demand_dirty: bool,
    pub(super) hyperliquid_stop_pending: Option<ProviderGeneration>,
    /// Engine generation at which a catalog refresh failure downgraded
    /// provider state. A later catalog success restores Online only when the
    /// session has not moved on meanwhile, so worker-driven states win.
    pub(super) hyperliquid_catalog_degraded: Option<ProviderGeneration>,
}

impl Coordinator<'_> {
    pub(super) fn detach_client(&mut self, client_id: ClientId) {
        let mut removed_consumer = false;
        let mut removed_study = false;
        for consumer_id in self.engine.detach_client(client_id) {
            removed_consumer = true;
            removed_study |= !self.studies.remove_consumer(consumer_id).is_empty();
            self.price_alerts.remove_consumer(consumer_id);
            self.delta_divergence_watermarks.remove(&consumer_id);
            self.events.remove(&consumer_id);
            self.consumer_clients.remove(&consumer_id);
            self.catalog_searches
                .retain(|(candidate, _), _| *candidate != consumer_id);
            self.catalog_selections
                .retain(|(candidate, _), _| *candidate != consumer_id);
            self.remove_waiter(consumer_id);
        }
        if removed_study && let Err(error) = self.reconcile_study_market_data() {
            eprintln!("Aeris study cleanup failed during client detach: {error}");
        }
        if removed_consumer {
            let _ = self.send_rithmic_demand();
            self.hyperliquid_demand_dirty = true;
        }
    }

    pub(super) fn begin_shutdown(&mut self) {
        self.publish_rithmic_live();
        self.publish_candle_live();
        for stop in self.history_cancellations.values() {
            stop.store(true, Ordering::Release);
        }
        let _ = self.providers.stop("rithmic");
        let _ = self.providers.stop("hyperliquid");
        let _ = self.providers.stop("tastytrade");
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
            Command::HistoryCompleted(series, generation, range, result) => {
                self.history_completed(&series, generation, range, result);
            }
            command @ (Command::BrokerAuthorizationChanged(..)
            | Command::AvailableStreams(..)
            | Command::Status(..)
            | Command::Attach(..)
            | Command::Detach(..)
            | Command::PollClient(..)) => self.handle_service_command(command),
            command => self.handle_consumer_command(command),
        }
    }

    pub(super) fn handle_service_command(&mut self, command: Command) {
        match command {
            Command::BrokerAuthorizationChanged(ready, reply) => {
                self.tastytrade_suspended = !ready;
                self.tastytrade_authorization = Some(ready);
                if !ready
                    && let Some(generation) = self
                        .engine
                        .provider_status("tastytrade")
                        .and_then(|s| s.generation)
                {
                    self.tastytrade_generation_floor = generation.0.get().saturating_add(1);
                    self.candle_provider_recovering(
                        "tastytrade",
                        generation.0.get(),
                        "Tastytrade disconnected",
                    );
                    let _ = self.engine.end_provider_session("tastytrade", generation);
                }
                self.flush_tastytrade_demand();
                self.broadcast_provider_for(
                    "tastytrade",
                    if ready {
                        None
                    } else {
                        Some("Tastytrade disconnected")
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
            Command::Attach(client_id, reply) => {
                self.handle_attach(client_id, &reply);
            }
            Command::Detach(client_id, reply) => {
                self.attached.remove(&client_id);
                self.detach_client(client_id);
                self.release_unused_live_market_data();
                let _ = reply.send(Ok(()));
            }
            Command::PollClient(client_id, consumer_budgets, reply) => {
                self.handle_poll_client(client_id, consumer_budgets, &reply);
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
            Command::InstallProviderInstrument(instrument, reply) => {
                let _ = reply.send(self.install_provider_instrument(&instrument));
            }
            Command::ReplacePriceAlerts(client_id, consumer_id, alerts, reply) => {
                let result =
                    authorize_consumer(&self.engine, client_id, consumer_id).and_then(|()| {
                        self.price_alerts
                            .replace_consumer_alerts(consumer_id, alerts)
                    });
                if result.is_ok() {
                    let _ = self.send_rithmic_demand();
                    self.hyperliquid_demand_dirty = true;
                    self.stop_realtime_if_idle();
                }
                let _ = reply.send(result);
            }
            Command::Poll(client_id, consumer_id, reply) => {
                self.handle_poll(client_id, consumer_id, &reply);
            }
            Command::Request(..)
            | Command::HistoryCompleted(..)
            | Command::ProviderWake
            | Command::Status(..)
            | Command::Attach(..)
            | Command::Detach(..)
            | Command::PollClient(..)
            | Command::AvailableStreams(..)
            | Command::BrokerAuthorizationChanged(..) => {
                unreachable!("command was routed to the wrong dispatcher")
            }
        }
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
                    if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                        publish_state(
                            events,
                            &publication,
                            SeriesLoadState::Partial,
                            Some("Refreshing provider coverage"),
                        );
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
        Ok(())
    }

    pub(super) fn handle_remove(
        &mut self,
        client_id: ClientId,
        consumer_id: ConsumerId,
    ) -> Result<(), String> {
        authorize_consumer(&self.engine, client_id, consumer_id)?;
        let removed_studies = self.studies.remove_consumer(consumer_id);
        self.events.remove(&consumer_id);
        self.consumer_clients.remove(&consumer_id);
        self.catalog_searches
            .retain(|(candidate, _), _| *candidate != consumer_id);
        self.catalog_selections
            .retain(|(candidate, _), _| *candidate != consumer_id);
        self.price_alerts.remove_consumer(consumer_id);
        self.remove_waiter(consumer_id);
        self.engine.remove_consumer(consumer_id);
        if !removed_studies.is_empty() {
            self.reconcile_study_market_data()?;
        }
        let _ = self.send_rithmic_demand();
        self.hyperliquid_demand_dirty = true;
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
            self.execute_study_ready(study_id)
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
            events.publish_study_removed(owner, &removed);
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
            self.execute_study_ready_subtree(study_id)
                .map_err(|error| error.to_string())
        });
        match prepared {
            Ok(executed) => {
                if let Some(events) = self.events.get_mut(&owner) {
                    events.publish_study_invalidated(owner, &affected);
                }
                self.publish_study_outputs(&executed);
                Ok(affected)
            }
            Err(error) => {
                self.studies.restore_subtree_checkpoint(checkpoint);
                if let Err(rollback_error) = self.reconcile_study_market_data() {
                    eprintln!("Aeris study reinitialization rollback failed: {rollback_error}");
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

    fn execute_study_ready(
        &mut self,
        study_id: super::StudyInstanceId,
    ) -> Result<bool, StudyRuntimeError> {
        let engine = &self.engine;
        let order_books = &self.order_books;
        let rithmic_live = &self.rithmic_live;
        let candle_live = &self.candle_live;
        let studies = &mut self.studies;
        let mut live_market = |input: &StudyMarketInput| {
            study_live_market_data(engine, order_books, rithmic_live, candle_live, input)
        };
        studies.execute_ready_with_live(engine, study_id, &mut live_market)
    }

    fn execute_study_ready_subtree(
        &mut self,
        study_id: super::StudyInstanceId,
    ) -> Result<Vec<super::StudyInstanceId>, StudyRuntimeError> {
        let engine = &self.engine;
        let order_books = &self.order_books;
        let rithmic_live = &self.rithmic_live;
        let candle_live = &self.candle_live;
        let studies = &mut self.studies;
        let mut live_market = |input: &StudyMarketInput| {
            study_live_market_data(engine, order_books, rithmic_live, candle_live, input)
        };
        studies.execute_ready_subtree_with_live(engine, study_id, &mut live_market)
    }

    pub(super) fn execute_studies_ready_for_market(
        &mut self,
        series: &BarSeriesKey,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError> {
        let engine = &self.engine;
        let order_books = &self.order_books;
        let rithmic_live = &self.rithmic_live;
        let candle_live = &self.candle_live;
        let studies = &mut self.studies;
        let mut live_market = |input: &StudyMarketInput| {
            study_live_market_data(engine, order_books, rithmic_live, candle_live, input)
        };
        studies.execute_ready_for_market_with_live(engine, series, &mut live_market)
    }

    pub(super) fn execute_study_bar_change(
        &mut self,
        series: &BarSeriesKey,
        exchange_timestamp_unix_nanos: i64,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError> {
        let engine = &self.engine;
        let order_books = &self.order_books;
        let rithmic_live = &self.rithmic_live;
        let candle_live = &self.candle_live;
        let studies = &mut self.studies;
        let mut live_market = |input: &StudyMarketInput| {
            study_live_market_data(engine, order_books, rithmic_live, candle_live, input)
        };
        studies.execute_live_market_change_with_live(
            engine,
            series,
            exchange_timestamp_unix_nanos,
            &mut live_market,
        )
    }

    pub(super) fn execute_study_history_range_change(
        &mut self,
        series: &BarSeriesKey,
        first_changed_unix_nanos: i64,
        last_changed_unix_nanos: i64,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError> {
        let engine = &self.engine;
        let order_books = &self.order_books;
        let rithmic_live = &self.rithmic_live;
        let candle_live = &self.candle_live;
        let studies = &mut self.studies;
        let mut live_market = |input: &StudyMarketInput| {
            study_live_market_data(engine, order_books, rithmic_live, candle_live, input)
        };
        studies.execute_history_range_change_with_live(
            engine,
            series,
            first_changed_unix_nanos,
            last_changed_unix_nanos,
            &mut live_market,
        )
    }

    pub(super) fn execute_study_non_bar_change(
        &mut self,
        provider_id: &str,
        instrument_id: &str,
        entitlement_id: &str,
        stream: MarketStream,
        observed_unix_nanos: i64,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError> {
        let engine = &self.engine;
        let order_books = &self.order_books;
        let rithmic_live = &self.rithmic_live;
        let candle_live = &self.candle_live;
        let studies = &mut self.studies;
        let mut live_market = |input: &StudyMarketInput| {
            study_live_market_data(engine, order_books, rithmic_live, candle_live, input)
        };
        let mut market_ready = |input: &StudyMarketInput| {
            study_market_series_live_ready(engine, rithmic_live, candle_live, input)
        };
        studies.execute_live_non_bar_change_with_live(
            engine,
            StudyNonBarChange {
                provider_id,
                instrument_id,
                entitlement_id,
                stream,
                observed_unix_nanos,
            },
            &mut live_market,
            &mut market_ready,
        )
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
        let mut required_identities = BTreeSet::new();
        for (series, subscription) in self.engine.subscriptions() {
            if subscription.streams.contains(MarketStream::Depth)
                || subscription.streams.contains(MarketStream::Trades)
                || subscription.streams.contains(MarketStream::Quotes)
            {
                required_identities.insert((
                    series.provider_id.clone(),
                    series.instrument_id.clone(),
                    series.entitlement_id.clone(),
                ));
            }
        }
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
            if let Some(book) = self.order_books.get_mut(&identity) {
                // Catalog selection counters describe consumers, not a new book.
                // Keep canonical revisions monotonic across metadata refreshes;
                // transport generations are fenced by provider_depth itself.
                book.update_instrument(instrument);
            } else {
                self.order_books
                    .insert(identity, ProviderOrderBook::new(instrument));
            }
        }
        let current_hyperliquid_generation = self
            .engine
            .provider_status("hyperliquid")
            .and_then(|status| status.generation)
            .map(|generation| generation.0.get());
        self.hyperliquid_display_depth
            .retain(|instrument_id, display| {
                current_hyperliquid_generation == Some(display.provider_generation)
                    && self
                        .order_books
                        .contains_key(&("hyperliquid".to_string(), instrument_id.clone()))
            });
    }

    pub(super) fn status(&self) -> MarketServiceStatus {
        let metrics = self.engine.metrics();
        MarketServiceStatus {
            connected_desktop_clients: self.attached.len(),
            study_execution_failed: self.studies.execution_failed(),
            providers: ["rithmic", "hyperliquid", "tastytrade"]
                .into_iter()
                .filter_map(|provider| self.provider_state(provider))
                .collect(),
            retained_series: metrics.stored_series,
            retained_bars: metrics.stored_bars,
            approximate_series_bytes: metrics.approximate_series_bytes,
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
        let provider_generation = self.provider_generation_for_series(series)?;
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
            self.stop_realtime_if_idle();
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
        self.stop_realtime_if_idle();
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market_service::{
        FormingBar, HISTORY_FAILED_RETRY_COOLDOWN, HISTORY_RETRY_DELAY,
        HISTORY_SERIES_HIGH_WATERMARK, HISTORY_SERIES_TARGET_BARS, HistorySnapshot,
        INITIAL_HISTORY_BARS, LiveHistoryState, MAXIMUM_HISTORY_RETRIES, MAXIMUM_STORED_BARS,
        ProviderCatalogDispatch, ProviderDispatchRecord, ProviderRealtimeDispatch,
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
    use aeris_contracts::ProviderInstrumentSearchResult;
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
            },
            attached: BTreeSet::new(),
            consumer_clients: BTreeMap::new(),
            pending: BTreeMap::new(),
            history_inflight: BTreeMap::new(),
            history_deferred: BTreeMap::new(),
            history_confirmed_empty: BTreeMap::new(),
            detached_history: BTreeSet::new(),
            history_cancellations: BTreeMap::new(),
            history_retries: BTreeMap::new(),
            events: BTreeMap::new(),
            rithmic_live: BTreeMap::new(),
            candle_live: BTreeMap::new(),
            order_books: BTreeMap::new(),
            hyperliquid_display_depth: BTreeMap::new(),
            hyperliquid_display_generation: 0,
            price_alerts: PriceAlertRegistry::default(),
            delta_divergence_watermarks: BTreeMap::new(),
            catalog: BTreeMap::new(),
            catalog_sessions: BTreeMap::new(),
            catalog_searches: BTreeMap::new(),
            catalog_selections: BTreeMap::new(),
            rithmic_demand: None,
            rithmic_pending_demand: None,
            rithmic_stop_pending: None,
            tastytrade_demand: None,
            tastytrade_authorization: None,
            tastytrade_suspended: false,
            tastytrade_generation_floor: 0,
            tastytrade_recovery: None,
            hyperliquid_engaged: false,
            hyperliquid_demand_dirty: false,
            hyperliquid_stop_pending: None,
            hyperliquid_catalog_degraded: None,
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
    fn exhausted_current_history_keeps_one_cooldown_recovery_ticket() {
        let mut coordinator = coordinator();
        let selected = series();
        let generation = ProviderGeneration(nonzero(1));
        for _ in 0..super::super::MAXIMUM_HISTORY_RETRIES {
            assert!(coordinator.schedule_history_retry(&selected, generation, None, "transient"));
        }
        assert!(!coordinator.schedule_history_retry(&selected, generation, None, "transient"));
        let ticket = coordinator
            .history_retries
            .get(&(selected, generation))
            .unwrap();
        assert_eq!(ticket.1, 0);
        assert!(ticket.2.is_none());
        assert!(ticket.0 > Instant::now());
        assert_eq!(coordinator.history_retries.len(), 1);
    }

    #[test]
    fn tastytrade_logout_rejects_late_events_after_reauthorization() {
        let mut coordinator = coordinator();
        coordinator
            .handle_tastytrade_realtime(super::super::tastytrade::RealtimeEvent::Connecting(1));
        coordinator
            .handle_tastytrade_realtime(super::super::tastytrade::RealtimeEvent::Connected(1));
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_command(Command::BrokerAuthorizationChanged(false, reply));
        result.recv().unwrap().unwrap();
        assert!(coordinator.tastytrade_suspended);
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_command(Command::BrokerAuthorizationChanged(true, reply));
        result.recv().unwrap().unwrap();
        coordinator
            .handle_tastytrade_realtime(super::super::tastytrade::RealtimeEvent::Connected(1));
        assert_ne!(
            coordinator
                .engine
                .provider_status("tastytrade")
                .unwrap()
                .health,
            ProviderHealth::Online
        );
        coordinator
            .handle_tastytrade_realtime(super::super::tastytrade::RealtimeEvent::Connecting(2));
        coordinator
            .handle_tastytrade_realtime(super::super::tastytrade::RealtimeEvent::Connected(2));
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
        assert!(!coordinator.rithmic_live.contains_key(&selected));
        assert!(!coordinator.pending.contains_key(&selected));
        assert!(
            !coordinator
                .history_inflight
                .keys()
                .any(|(series, _)| series == &selected)
        );
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
        let mut order_book = ProviderOrderBook::new(selected.clone());
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
            bid: Some(DepthLevel {
                price: 100,
                quantity: 2,
                order_count: None,
            }),
            ask: Some(DepthLevel {
                price: 101,
                quantity: 2,
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
        let mut live_handoff = RithmicLiveHandoff::new(
            &selected_series,
            ProviderGeneration(nonzero(1)),
            &selected.venue_id,
        )
        .expect("live handoff");
        live_handoff.connected = true;
        live_handoff.history_state = LiveHistoryState::Ready;
        coordinator
            .rithmic_live
            .insert(selected_series.clone(), live_handoff);
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
            bid: Some(DepthLevel {
                price,
                quantity: 1,
                order_count: None,
            }),
            ask: Some(DepthLevel {
                price: price + 1,
                quantity: 1,
                order_count: None,
            }),
        }
    }

    fn assert_rithmic_history_state(
        coordinator: &Coordinator<'_>,
        series: &BarSeriesKey,
        expected: LiveHistoryState,
    ) {
        assert_eq!(
            coordinator
                .rithmic_live
                .get(series)
                .map(|live| live.history_state),
            Some(expected)
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
        let mut live = RithmicLiveHandoff::new(&series, provider_generation, "CME")
            .expect("Rithmic live handoff");
        live.seed(2, 0, &bars, None, Some(boundary))
            .expect("live handoff seeds from current history");
        live.connected = true;
        coordinator.rithmic_live.insert(series.clone(), live);
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
        coordinator
            .detached_history
            .insert((selected.clone(), first_generation));
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
        coordinator.history_completed(
            &selected,
            first_generation,
            None,
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 0,
                bars: minute_bars(10, 2),
                forming: None,
                handoff_boundary_unix_nanos: Some(11 * 60 * 1_000_000_000),
            }),
        );

        let after = coordinator
            .engine
            .series_snapshot(&selected)
            .expect("stale completion cannot replace cached history");
        assert_eq!(after.bars, before.bars);
        assert!(
            !coordinator
                .detached_history
                .contains(&(selected.clone(), second_generation))
        );
        coordinator.prune_history_tracking();
        assert!(
            !coordinator
                .detached_history
                .contains(&(selected, first_generation))
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
        let stale_key = (selected.clone(), first_generation);
        coordinator.history_inflight.insert(stale_key.clone(), None);
        coordinator.history_deferred.insert(
            stale_key.clone(),
            DeferredHistoryRequest::Range(HistoryRange {
                start_unix_nanos: 0,
                end_unix_nanos: 60_000_000_000,
            }),
        );

        assert!(
            coordinator
                .accept_history_completion(
                    &selected,
                    first_generation,
                    None,
                    Ok(HistorySnapshot {
                        price_scale: 2,
                        quantity_scale: 0,
                        bars: minute_bars(0, 2),
                        forming: None,
                        handoff_boundary_unix_nanos: None,
                    }),
                )
                .is_none()
        );
        assert!(!coordinator.history_inflight.contains_key(&stale_key));
        assert!(!coordinator.history_deferred.contains_key(&stale_key));
        assert!(
            !coordinator
                .history_inflight
                .contains_key(&(selected.clone(), second_generation))
        );
        assert!(coordinator.history_retries.is_empty());
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
        let stale_key = (selected.clone(), first_generation);
        let stop = Arc::new(AtomicBool::new(false));
        coordinator
            .history_cancellations
            .insert(stale_key.clone(), Arc::clone(&stop));
        coordinator.history_deferred.insert(
            stale_key.clone(),
            DeferredHistoryRequest::Range(HistoryRange {
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
        assert!(!coordinator.history_deferred.contains_key(&stale_key));
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
        let detached_key = (series.clone(), provider_generation);
        assert!(coordinator.detached_history.contains(&detached_key));
        assert!(
            !coordinator.series_live_if_ready(&series),
            "a historical canonical window cannot advertise live readiness"
        );
        coordinator
            .rithmic_live
            .get_mut(&series)
            .expect("live handoff remains allocated")
            .accept_trade(&rithmic_trade_for_series(&series, 1, live_timestamp))
            .expect("live handoff keeps ingesting while detached");
        assert_eq!(
            coordinator
                .rithmic_live
                .get(&series)
                .and_then(RithmicLiveHandoff::coverage)
                .map(|(_, end)| end),
            Some(live_timestamp)
        );
        coordinator.publish_rithmic_live();
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
        coordinator.history_completed(
            &series,
            provider_generation,
            None,
            Ok(current_rejoin_snapshot(live_timestamp)),
        );
        assert!(!coordinator.detached_history.contains(&detached_key));
        coordinator.publish_rithmic_live();
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
        assert!(coordinator.rithmic_live.contains_key(&selected_series));
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
        assert!(!coordinator.rithmic_live.contains_key(&selected_series));
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
        assert!(!coordinator.rithmic_live.contains_key(&selected_series));
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
            &coordinator.rithmic_live,
            &coordinator.candle_live,
            &input,
        )
        .expect("online ready series exposes current live market state");
        assert!(live.quote().is_some());
        assert!(live.trades().is_some_and(|trades| !trades.is_empty()));
        assert!(live.depth().is_some());

        for recovery_state in [
            LiveHistoryState::Reseeding,
            LiveHistoryState::AwaitingHistory,
        ] {
            coordinator
                .rithmic_live
                .get_mut(&input.series)
                .expect("series handoff")
                .history_state = recovery_state;
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
                    &coordinator.rithmic_live,
                    &coordinator.candle_live,
                    &input,
                )
                .is_none(),
                "bar-aligned non-bar views must be fenced until this series is history-ready"
            );
        }
        coordinator
            .rithmic_live
            .get_mut(&input.series)
            .expect("series handoff")
            .history_state = LiveHistoryState::Ready;
        assert!(
            study_live_market_data(
                &coordinator.engine,
                &coordinator.order_books,
                &coordinator.rithmic_live,
                &coordinator.candle_live,
                &input,
            )
            .is_some(),
            "series live views resume after the history seam is ready"
        );

        coordinator.rithmic_recovering(1, "test recovery");
        assert!(
            study_live_market_data(
                &coordinator.engine,
                &coordinator.order_books,
                &coordinator.rithmic_live,
                &coordinator.candle_live,
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
            .rithmic_live
            .get(&selected_series)
            .expect("series live handoff exists");
        assert!(live.connected);
        assert_eq!(live.history_state, LiveHistoryState::Ready);

        // Start a new forming minute with the maximum representable volume, then
        // publish it so the study's canonical primary timeline includes that row.
        let mut first =
            rithmic_trade_for_series(&selected_series, 1, 180_i64.saturating_mul(1_000_000_000));
        first.quantity = i64::MAX;
        coordinator.rithmic_trade(1, &first);
        coordinator.publish_rithmic_live();
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
        coordinator.rithmic_trade(1, &second);

        assert_eq!(
            coordinator
                .engine
                .provider_status("rithmic")
                .map(|status| status.health),
            Some(ProviderHealth::Online),
            "one-series aggregation recovery must not replace provider-session ownership"
        );
        assert_eq!(
            coordinator
                .rithmic_live
                .get(&selected_series)
                .expect("series handoff remains retained")
                .history_state,
            LiveHistoryState::AwaitingHistory
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
        assert_rithmic_history_state(&coordinator, &primary, LiveHistoryState::Ready);
        assert_rithmic_history_state(&coordinator, &secondary, LiveHistoryState::Ready);
        let generation_before_recovery = coordinator
            .studies
            .output_series(study_id.output(0))
            .expect("initial MTF output")
            .generation();

        coordinator
            .rithmic_live
            .get_mut(&primary)
            .expect("primary handoff remains installed")
            .history_state = LiveHistoryState::AwaitingHistory;
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

        coordinator
            .rithmic_live
            .get_mut(&primary)
            .expect("primary handoff remains installed")
            .history_state = LiveHistoryState::Ready;
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
        assert_rithmic_history_state(&coordinator, &primary, LiveHistoryState::Ready);
        assert_rithmic_history_state(&coordinator, &secondary, LiveHistoryState::Ready);
        let generation_before_recovery = coordinator
            .studies
            .output_series(consumer_study.output(0))
            .expect("initial output-backed MTF output")
            .generation();

        coordinator
            .rithmic_live
            .get_mut(&primary)
            .expect("producer handoff remains installed")
            .history_state = LiveHistoryState::AwaitingHistory;
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

        coordinator
            .rithmic_live
            .get_mut(&primary)
            .expect("producer handoff remains installed")
            .history_state = LiveHistoryState::Ready;
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
        let mut live = RithmicLiveHandoff::new(&selected_series, provider_generation, "CME")
            .expect("live handoff");
        live.history_state = LiveHistoryState::Ready;
        coordinator.rithmic_live.insert(selected_series, live);

        coordinator.rithmic_online(1);

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
        let mut live = CandleLiveHandoff::new(
            selected_series.clone(),
            provider_generation,
            "BTC".to_string(),
            "1m".to_string(),
        );
        live.history_state = LiveHistoryState::Ready;
        coordinator.candle_live.insert(selected_series, live);

        coordinator.candle_provider_online("hyperliquid", 1);

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
        let empty_key = (first.clone(), ProviderGeneration(nonzero(1)));
        coordinator.history_confirmed_empty.insert(
            empty_key.clone(),
            HistoryRange {
                start_unix_nanos: 0,
                end_unix_nanos: 60_000_000_000,
            },
        );
        coordinator.prune_history_tracking();
        assert!(coordinator.history_confirmed_empty.contains_key(&empty_key));
        assert!(coordinator.evict_unreferenced_series().is_empty());
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
        assert!(!coordinator.history_confirmed_empty.contains_key(&empty_key));
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
        let key = (selected.clone(), generation);
        coordinator.history_retries.insert(
            key.clone(),
            (
                Instant::now() + HISTORY_FAILED_RETRY_COOLDOWN,
                MAXIMUM_HISTORY_RETRIES.saturating_add(1),
                Some(failed),
            ),
        );

        assert!(
            coordinator
                .enqueue_history_request(&selected, generation, Some(failed))
                .is_ok(),
            "identical failed range is suppressed during cooldown"
        );
        assert!(coordinator.history_retries.contains_key(&key));

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
        assert!(!coordinator.history_retries.contains_key(&key));
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
        let key = (selected.clone(), generation);
        coordinator.history_retries.insert(
            key.clone(),
            (Instant::now() + HISTORY_RETRY_DELAY, 1, Some(failed)),
        );

        assert!(
            coordinator
                .enqueue_history_request(&selected, generation, Some(failed))
                .is_ok(),
            "duplicate range remains parked until its retry deadline"
        );
        assert_eq!(
            coordinator
                .history_retries
                .get(&key)
                .map(|(_, attempts, range)| (*attempts, *range)),
            Some((1, Some(failed)))
        );
        assert!(coordinator.history_inflight.is_empty());
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
                history: history_tx,
                lifecycle: None,
                realtime: ProviderRealtimeDispatch::Disabled,
                catalog: ProviderCatalogDispatch::Disabled,
            },
        );

        assert!(
            coordinator
                .enqueue_history_request_with_capacity_retry(&selected, generation, Some(range))
                .is_ok(),
            "bounded queue pressure is converted into coordinator-owned retry state"
        );
        assert_eq!(
            coordinator
                .history_retries
                .get(&(selected, generation))
                .map(|(_, attempts, queued_range)| (*attempts, *queued_range)),
            Some((0, Some(range)))
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
        let key = (selected.clone(), generation);
        coordinator
            .history_inflight
            .insert(key.clone(), Some(range));
        coordinator
            .history_retries
            .insert(key.clone(), (Instant::now(), 1, Some(range)));

        coordinator.history_completed(
            &selected,
            generation,
            Some(range),
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 0,
                bars: Vec::new(),
                forming: None,
                handoff_boundary_unix_nanos: Some(range.end_unix_nanos),
            }),
        );

        assert_eq!(coordinator.history_confirmed_empty.get(&key), Some(&range));
        assert!(!coordinator.history_inflight.contains_key(&key));
        assert!(!coordinator.history_retries.contains_key(&key));
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
        assert!(coordinator.rithmic_live.contains_key(&selected));

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
        assert!(coordinator.rithmic_live.is_empty());

        let provider_generation = ProviderGeneration(nonzero(1));
        coordinator.rithmic_stop_pending = Some(provider_generation);
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
            .rithmic_live
            .get(&selected)
            .expect("Rithmic live handoff is recreated");
        assert_eq!(live.history_state, LiveHistoryState::Ready);
        assert_eq!(live.coverage(), Some((0, 0)));
        let demand = coordinator
            .rithmic_realtime_demand()
            .expect("Rithmic demand rebuilds");
        assert_eq!(demand.instruments.len(), 1);
        assert!(demand.instruments[0].trades);

        coordinator.handle_rithmic_realtime(RithmicRealtimeEvent::Disconnected(1, None));
        assert!(coordinator.rithmic_stop_pending.is_none());
        let provider = coordinator
            .engine
            .provider_status("rithmic")
            .expect("Rithmic provider remains installed");
        assert_eq!(provider.generation, Some(provider_generation));
        assert_eq!(provider.health, ProviderHealth::Recovering);
        assert!(
            coordinator.rithmic_live.contains_key(&selected),
            "the idle-stop completion must restart rather than retire restored demand"
        );
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
            .rithmic_live
            .get(&selected)
            .expect("current-generation live handoff is recreated");
        assert_eq!(live.generation, ProviderGeneration(nonzero(2)));
        assert_eq!(live.history_state, LiveHistoryState::AwaitingHistory);
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
        assert!(coordinator.candle_live.contains_key(&selected));

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
        assert!(coordinator.candle_live.is_empty());

        let provider_generation = ProviderGeneration(nonzero(1));
        coordinator.hyperliquid_stop_pending = Some(provider_generation);
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
            .candle_live
            .get(&selected)
            .expect("Hyperliquid live handoff is recreated");
        assert_eq!(live.history_state, LiveHistoryState::Ready);
        assert_eq!(live.coverage(), Some((0, 0)));
        let demand = coordinator.hyperliquid_demand();
        assert_eq!(demand.candles.len(), 1);
        assert_eq!(demand.candles[0].instrument.wire_coin, "BTC");
        assert_eq!(demand.candles[0].interval, "1m");
        assert!(
            coordinator.hyperliquid_stop_pending.is_none(),
            "replacement demand must retire the cancellable idle-stop fence even without Disconnected"
        );
        assert!(coordinator.hyperliquid_demand_dirty);

        coordinator.handle_hyperliquid_realtime(HyperliquidRealtimeEvent::Heartbeat(1, None));
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
                .candle_live
                .get(&selected)
                .is_some_and(|live| live.connected),
            "the accepted heartbeat marks the restored handoff connected"
        );
        assert!(
            coordinator.candle_live.contains_key(&selected),
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
                history: history_sender,
                lifecycle: None,
                realtime: super::super::ProviderRealtimeDispatch::Disabled,
                catalog: super::super::ProviderCatalogDispatch::Disabled,
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
            .rithmic_live
            .get(&selected)
            .expect("Rithmic handoff is recreated before reconnect");
        assert_eq!(live.history_state, LiveHistoryState::AwaitingHistory);
        assert_eq!(live.generation, first_generation);
        assert!(matches!(
            history_receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));

        coordinator.handle_rithmic_realtime(RithmicRealtimeEvent::Connecting(2));
        let second_generation = ProviderGeneration(nonzero(2));
        let provider = coordinator
            .engine
            .provider_status("rithmic")
            .expect("Rithmic provider remains installed");
        assert_eq!(provider.generation, Some(second_generation));
        assert_eq!(provider.health, ProviderHealth::Connecting);
        let live = coordinator
            .rithmic_live
            .get(&selected)
            .expect("Rithmic handoff survives reconnect");
        assert_eq!(live.generation, second_generation);
        assert_eq!(live.history_state, LiveHistoryState::AwaitingHistory);
        let history = history_receiver
            .try_recv()
            .expect("new provider session enqueues covering history");
        assert_eq!(history.series, selected);
        assert_eq!(history.provider_generation, second_generation);
        assert!(history.range.is_none());
    }

    fn queued_state(consumer_id: ConsumerId, value: u64) -> MarketRuntimeEvent {
        MarketRuntimeEvent::SeriesState(crate::MarketSeriesState {
            consumer_id,
            generation: generation(value),
            series: None,
            state: super::super::SeriesLoadState::Resolving,
            detail: None,
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
        coordinator.handle_poll_client(
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
        coordinator.handle_poll_client(owner, vec![(constrained, 1), (available, 4)], &reply);
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
        let key = (consumer, "rithmic".to_string());
        coordinator.catalog_searches.insert(key.clone(), 8);

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
        assert_eq!(coordinator.catalog_searches.get(&key), Some(&8));

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
        assert!(!coordinator.catalog_searches.contains_key(&key));
    }

    #[test]
    fn preliminary_catalog_result_keeps_the_remote_completion_fenced() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        let key = (consumer, "tastytrade".to_string());
        coordinator.catalog_searches.insert(key.clone(), 8);
        let result = ProviderInstrumentSearchResult {
            consumer_id: consumer.0.get(),
            provider: "tastytrade".into(),
            provider_generation: 1,
            search_generation: 8,
            instruments: Vec::new(),
        };
        coordinator.handle_catalog_search_preview(result.clone());
        assert_eq!(coordinator.catalog_searches.get(&key), Some(&8));
        assert!(matches!(
            coordinator.events.get(&consumer).unwrap().catalog_search,
            Some(MarketRuntimeEvent::ProviderInstrumentSearchPreview(_))
        ));
        coordinator.handle_catalog_search(result);
        assert!(!coordinator.catalog_searches.contains_key(&key));
    }

    #[test]
    fn catalog_selection_callbacks_are_fenced_before_installation() {
        let mut coordinator = coordinator();
        let consumer = consumer(1);
        register(&mut coordinator, consumer);
        coordinator
            .events
            .insert(consumer, ConsumerEvents::default());
        let key = (consumer, "rithmic".to_string());
        coordinator.catalog_selections.insert(key.clone(), 12);

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
        assert_eq!(coordinator.catalog_selections.get(&key), Some(&12));

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
        assert!(!coordinator.catalog_selections.contains_key(&key));
    }
}
