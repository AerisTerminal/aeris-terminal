use super::{
    Arc, AtomicBool, BTreeMap, BTreeSet, BarSeriesKey, COORDINATOR_TICK, ClientId, Command,
    ConsumerEvents, ConsumerId, ConsumerIdentity, DeferredHistoryRequest, DemandWaiter, Duration,
    EMPTY_REPAIR_RETRY_DELAY, EngineError, EngineResourceMode, FailureStage, HistoryRange,
    HyperliquidLiveHandoff, InstallProviderInstrument, Instant, MAXIMUM_SERIES, MarketEngine,
    MarketServiceStatus, MarketStream, Ordering, PendingLocalHistoryRead, ProviderConnectionState,
    ProviderDispatch, ProviderGeneration, ProviderHealth, ProviderOrderBook, ProviderRuntimeEvent,
    ProviderRuntimeRegistry, ProviderState, REALTIME_CAPACITY, REALTIME_DRAIN_BUDGET, Receiver,
    RecvTimeoutError, Reply, ResourceMode, ResourcePolicyDecision, ResourcePolicyInput,
    RithmicLiveHandoff, RithmicRealtimeDemand, StorageRequest, StoredHistory, SyncSender, VecDeque,
    WarmSeries, authorize_consumer, chart_stream_requirements, decide_resource_policy, envelope,
    resource_policy_mode, thread,
};

pub(super) struct OwnedCoordinatorChannels {
    pub(super) commands: Receiver<Command>,
    pub(super) storage: SyncSender<StorageRequest>,
    pub(super) providers: ProviderRuntimeRegistry,
}

pub(super) fn spawn_coordinator(
    engine: MarketEngine,
    mut channels: OwnedCoordinatorChannels,
    shutdown: Arc<AtomicBool>,
    available_memory_bytes: u64,
    hot_set_priority_count: usize,
) -> Result<thread::JoinHandle<()>, String> {
    thread::Builder::new()
        .name("axiusflow-market-engine".to_string())
        .spawn(move || {
            {
                let providers = channels.providers.dispatch();
                run_coordinator(
                    engine,
                    &channels.commands,
                    &channels.storage,
                    providers,
                    &shutdown,
                    available_memory_bytes,
                    hot_set_priority_count,
                );
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
    storage: &SyncSender<StorageRequest>,
    providers: ProviderDispatch<'_>,
    shutdown: &AtomicBool,
    available_memory_bytes: u64,
    hot_set_priority_count: usize,
) {
    let resource_policy = decide_resource_policy(ResourcePolicyInput {
        mode: EngineResourceMode::Warm,
        available_memory_bytes,
        consumer_count: 0,
        visible_consumer_count: 0,
        stored_series_count: 0,
        provider_series_limit: MAXIMUM_SERIES,
        hot_set_priority_count,
    });
    let mut coordinator = Coordinator {
        engine,
        providers,
        storage,
        resource_mode: ResourceMode::Warm,
        resource_policy,
        available_memory_bytes,
        hot_set_priority_count,
        last_consumer_activity: Instant::now(),
        attached: BTreeSet::new(),
        attached_sinks: BTreeMap::new(),
        consumer_clients: BTreeMap::new(),
        pending: BTreeMap::new(),
        history_inflight: BTreeMap::new(),
        history_deferred: BTreeMap::new(),
        history_cancellations: BTreeMap::new(),
        suspended_history: BTreeSet::new(),
        pending_empty_repairs: BTreeMap::new(),
        empty_repair_retry_at: Instant::now(),
        history_retries: BTreeMap::new(),
        local_history_deadlines: BTreeMap::new(),
        persistence_pending: BTreeMap::new(),
        persistence_degraded: BTreeSet::new(),
        persistence_backlog: VecDeque::new(),
        local_loaded: BTreeSet::new(),
        warm_reads: BTreeSet::new(),
        warm_series: BTreeMap::new(),
        warm_priority: Vec::new(),
        retained_history: BTreeMap::new(),
        prewarmed: BTreeSet::new(),
        retained_live: BTreeSet::new(),
        warm_search_generations: BTreeMap::new(),
        warm_restore_pending: BTreeMap::new(),
        warm_restore_skipped: BTreeSet::new(),
        events: BTreeMap::new(),
        rithmic_live: BTreeMap::new(),
        hyperliquid_live: BTreeMap::new(),
        order_books: BTreeMap::new(),
        catalog: BTreeMap::new(),
        catalog_sessions: BTreeMap::new(),
        rithmic_demand: None,
        rithmic_pending_demand: None,
        rithmic_stop_pending: None,
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
        let _ = drain_coordinator_events(&mut coordinator);
        coordinator.publish_rithmic_live();
        coordinator.publish_hyperliquid_live();
        coordinator.recover_overflowed_series_queues();
        coordinator.flush_attached_events();
        coordinator.flush_rithmic_demand();
        coordinator.flush_hyperliquid_demand();
        coordinator.stop_realtime_if_idle();
        coordinator.expire_local_history_reads();
        coordinator.retry_pending_empty_repairs();
        coordinator.retry_history();
        coordinator.enforce_resource_policy();
        match commands.recv_timeout(COORDINATOR_TICK) {
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
    let mut drained = 0;
    for lane in 0..4 {
        for _ in 0..REALTIME_DRAIN_BUDGET {
            let Some(event) = coordinator.providers.take_event(lane) else {
                break;
            };
            drained += 1;
            match event {
                ProviderRuntimeEvent::RithmicRealtime(event) => {
                    coordinator.handle_rithmic_realtime(event);
                }
                ProviderRuntimeEvent::RithmicCatalog(event) => {
                    coordinator.handle_rithmic_catalog(event);
                }
                ProviderRuntimeEvent::HyperliquidRealtime(event) => {
                    coordinator.handle_hyperliquid_realtime(event);
                }
                ProviderRuntimeEvent::HyperliquidCatalog(event) => {
                    coordinator.handle_hyperliquid_catalog(event);
                }
            }
        }
    }
    drained
}

/// Drains only work already bounded by provider event lanes before stopping
/// their workers. This preserves a final trade/candle rollover and its
/// persistence without turning shutdown into an unbounded wait for a live feed.
pub(super) fn drain_shutdown_provider_events(coordinator: &mut Coordinator<'_>) {
    let maximum_rounds = REALTIME_CAPACITY.div_ceil(REALTIME_DRAIN_BUDGET).max(1);
    for _ in 0..maximum_rounds {
        if drain_coordinator_events(coordinator) == 0 {
            break;
        }
    }
}

pub(super) struct Coordinator<'a> {
    pub(super) engine: MarketEngine,
    pub(super) providers: ProviderDispatch<'a>,
    pub(super) storage: &'a SyncSender<StorageRequest>,
    pub(super) resource_mode: ResourceMode,
    pub(super) resource_policy: ResourcePolicyDecision,
    pub(super) available_memory_bytes: u64,
    pub(super) hot_set_priority_count: usize,
    pub(super) last_consumer_activity: Instant,
    pub(super) attached: BTreeSet<ClientId>,
    pub(super) attached_sinks: BTreeMap<ClientId, SyncSender<(u64, envelope::Payload)>>,
    pub(super) consumer_clients: BTreeMap<ConsumerId, ClientId>,
    pub(super) pending: BTreeMap<BarSeriesKey, Vec<DemandWaiter>>,
    pub(super) history_inflight: BTreeMap<(BarSeriesKey, ProviderGeneration), Option<HistoryRange>>,
    pub(super) history_deferred:
        BTreeMap<(BarSeriesKey, ProviderGeneration), DeferredHistoryRequest>,
    pub(super) history_cancellations: BTreeMap<(BarSeriesKey, ProviderGeneration), Arc<AtomicBool>>,
    /// Requests canceled by account/lifecycle suspension. Their response may be
    /// internally valid but belongs to retired market access and cannot install.
    pub(super) suspended_history: BTreeSet<(BarSeriesKey, ProviderGeneration)>,
    pub(super) pending_empty_repairs: BTreeMap<BarSeriesKey, HistoryRange>,
    pub(super) empty_repair_retry_at: Instant,
    pub(super) history_retries:
        BTreeMap<(BarSeriesKey, ProviderGeneration), (Instant, u8, Option<HistoryRange>)>,
    pub(super) local_history_deadlines:
        BTreeMap<(BarSeriesKey, ProviderGeneration), PendingLocalHistoryRead>,
    pub(super) persistence_pending: BTreeMap<(BarSeriesKey, ProviderGeneration), usize>,
    pub(super) persistence_degraded: BTreeSet<(BarSeriesKey, ProviderGeneration)>,
    pub(super) persistence_backlog: VecDeque<StorageRequest>,
    pub(super) local_loaded: BTreeSet<(BarSeriesKey, ProviderGeneration)>,
    pub(super) warm_reads: BTreeSet<(BarSeriesKey, ProviderGeneration)>,
    pub(super) warm_series: BTreeMap<BarSeriesKey, WarmSeries>,
    pub(super) warm_priority: Vec<BarSeriesKey>,
    pub(super) retained_history: BTreeMap<BarSeriesKey, StoredHistory>,
    pub(super) prewarmed: BTreeSet<BarSeriesKey>,
    pub(super) retained_live: BTreeSet<BarSeriesKey>,
    pub(super) warm_search_generations: BTreeMap<String, u64>,
    pub(super) warm_restore_pending: BTreeMap<String, BarSeriesKey>,
    pub(super) warm_restore_skipped: BTreeSet<BarSeriesKey>,
    pub(super) events: BTreeMap<ConsumerId, ConsumerEvents>,
    pub(super) rithmic_live: BTreeMap<BarSeriesKey, RithmicLiveHandoff>,
    pub(super) hyperliquid_live: BTreeMap<BarSeriesKey, HyperliquidLiveHandoff>,
    pub(super) order_books: BTreeMap<(String, String), ProviderOrderBook>,
    pub(super) catalog: BTreeMap<(String, String), InstallProviderInstrument>,
    pub(super) catalog_sessions: BTreeMap<String, u64>,
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
        for consumer_id in self.engine.detach_client(client_id) {
            self.events.remove(&consumer_id);
            self.consumer_clients.remove(&consumer_id);
            self.remove_waiter(consumer_id);
        }
    }

    pub(super) fn begin_shutdown(&mut self) {
        self.publish_rithmic_live();
        self.publish_hyperliquid_live();
        self.flush_persistence_backlog_for_shutdown();
        for stop in self.history_cancellations.values() {
            stop.store(true, Ordering::Release);
        }
        let _ = self.providers.stop("rithmic");
        let _ = self.providers.stop("hyperliquid");
    }

    pub(super) fn handle_command(&mut self, command: Command) {
        match command {
            Command::ProviderWake => return,
            Command::HistoryCompleted(series, generation, range, result) => {
                self.history_completed(&series, generation, range, result);
            }
            Command::LocalHistoryCompleted(series, generation, result) => {
                self.retry_persistence_backlog();
                self.local_history_completed(&series, generation, result);
                self.schedule_warm_local_history();
            }
            Command::LocalHistoryRangeCompleted(series, generation, range, result) => {
                // Persistence accepted into the coordinator backlog gets first
                // use of capacity freed by a storage completion. A chained
                // viewport cache read may fall back to provider history rather
                // than indefinitely starving durable writes.
                self.retry_persistence_backlog();
                self.local_history_range_completed(&series, generation, range, result);
                self.schedule_warm_local_history();
            }
            Command::PersistenceCompleted(series, generation, result, elapsed_millis) => {
                self.persistence_completed(&series, generation, result, elapsed_millis);
                self.schedule_warm_local_history();
            }
            Command::ConfirmedEmptyResolved(series, range, result) => {
                self.retry_persistence_backlog();
                if result.is_err() {
                    self.remember_pending_empty_repair(&series, range);
                    self.empty_repair_retry_at = Instant::now() + EMPTY_REPAIR_RETRY_DELAY;
                }
                self.schedule_warm_local_history();
            }
            command @ (Command::RestoreHotSet(..)
            | Command::SetResourceMode(..)
            | Command::Status(..)
            | Command::Attach(..)
            | Command::Detach(..)) => self.handle_service_command(command),
            command => self.handle_consumer_command(command),
        }
        self.refresh_resource_policy();
    }

    pub(super) fn handle_service_command(&mut self, command: Command) {
        match command {
            Command::RestoreHotSet(series, reply) => {
                self.restore_hot_series(series);
                let _ = reply.send(Ok(()));
            }
            Command::SetResourceMode(mode, reply) => {
                let result = self.apply_resource_mode(mode);
                let _ = reply.send(result);
            }
            Command::Status(reply) => {
                let _ = reply.send(Ok(self.status()));
            }
            Command::Attach(client_id, events, reply) => {
                self.handle_attach(client_id, events, &reply);
            }
            Command::Detach(client_id, reply) => {
                self.attached.remove(&client_id);
                self.attached_sinks.remove(&client_id);
                self.detach_client(client_id);
                self.release_unused_live_market_data();
                let _ = reply.send(Ok(()));
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
                let result =
                    authorize_consumer(&self.engine, client_id, consumer_id).and_then(|()| {
                        let publication = self
                            .engine
                            .set_resource_class(consumer_id, resource_class)
                            .map_err(|error| error.to_string())?;
                        if let Some(publication) = publication {
                            self.publish_current_snapshot(&publication);
                        }
                        if let Some(events) = self.events.get_mut(&consumer_id)
                            && !resource_class.publishes_ui()
                        {
                            // A full snapshot queued just before a pane hid
                            // must not leak through after the class change.
                            // Canonical depth remains engine-owned and will
                            // be republished from its latest revision on
                            // Foreground restore below.
                            events.order_book = None;
                        }
                        Ok(())
                    });
                self.reconcile_order_books();
                if resource_class.publishes_ui() {
                    self.publish_order_book_to_consumer(consumer_id);
                }
                self.release_unused_live_market_data();
                let _ = reply.send(result);
            }
            Command::Demand(client_id, consumer_id, generation, series, reply) => {
                self.handle_demand(
                    client_id,
                    &series,
                    DemandWaiter {
                        consumer_id,
                        generation,
                        started_at: Instant::now(),
                    },
                    &reply,
                );
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
            Command::Poll(client_id, consumer_id, reply) => {
                self.handle_poll(client_id, consumer_id, &reply);
            }
            Command::HistoryCompleted(..)
            | Command::LocalHistoryCompleted(..)
            | Command::LocalHistoryRangeCompleted(..)
            | Command::PersistenceCompleted(..)
            | Command::ConfirmedEmptyResolved(..)
            | Command::ProviderWake
            | Command::RestoreHotSet(..)
            | Command::SetResourceMode(..)
            | Command::Status(..)
            | Command::Attach(..)
            | Command::Detach(..) => unreachable!("command was routed to the wrong dispatcher"),
        }
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
        self.events.remove(&consumer_id);
        self.consumer_clients.remove(&consumer_id);
        self.remove_waiter(consumer_id);
        self.engine.remove_consumer(consumer_id);
        self.release_unused_live_market_data();
        Ok(())
    }

    pub(super) fn apply_resource_mode(&mut self, mode: ResourceMode) -> Result<(), String> {
        let previous_mode = self.resource_mode;
        self.resource_mode = mode;
        self.refresh_resource_policy();
        self.schedule_warm_local_history();
        if mode == ResourceMode::OfflineSuspended {
            self.suspend_provider_work();
        } else if mode == ResourceMode::MarketsLive {
            if previous_mode != ResourceMode::MarketsLive {
                self.warm_restore_pending.clear();
                self.warm_restore_skipped.clear();
            }
            self.activate_markets_live_hot_set();
        } else {
            self.warm_restore_pending.clear();
            self.warm_restore_skipped.clear();
        }
        if mode != ResourceMode::OfflineSuspended {
            self.reconcile_authorized_demands()?;
        }
        self.release_unused_live_market_data();
        Ok(())
    }

    /// Rebuilds provider-owned work from the demand registry after policy has
    /// allowed market access again. Suspension intentionally destroys live
    /// handoffs and provider selections, while consumer demand remains the
    /// authoritative record of the charts the user still has open.
    fn reconcile_authorized_demands(&mut self) -> Result<(), String> {
        let demanded = self
            .engine
            .subscriptions()
            .into_iter()
            .map(|(series, _)| series)
            .collect::<BTreeSet<_>>();
        let mut failures = Vec::new();
        for series in demanded {
            let result = self
                .provider_generation_for_series(&series)
                .and_then(|generation| {
                    self.ensure_realtime(&series)?;
                    if let Some(snapshot) = self.engine.series_snapshot(&series) {
                        self.prepare_cached_demand(&series, generation, &snapshot)?;
                    } else {
                        self.enqueue_history(&series, generation)
                            .map_err(str::to_string)?;
                    }
                    Ok(())
                });
            if let Err(error) = result {
                self.broadcast_demand_error_for(
                    &series,
                    FailureStage::ProviderRealtime,
                    &error,
                    None,
                );
                failures.push(format!("{}: {error}", series.instrument_id));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "market demand reconciliation failed: {}",
                failures.join("; ")
            ))
        }
    }

    pub(super) fn refresh_resource_policy(&mut self) {
        let metrics = self.engine.metrics();
        if metrics.active_consumers > 0 {
            self.last_consumer_activity = Instant::now();
        }
        self.resource_policy = decide_resource_policy(ResourcePolicyInput {
            mode: resource_policy_mode(self.resource_mode),
            available_memory_bytes: self.available_memory_bytes,
            consumer_count: metrics.active_consumers,
            visible_consumer_count: self.engine.visible_consumer_count(),
            stored_series_count: metrics.stored_series,
            provider_series_limit: MAXIMUM_SERIES,
            hot_set_priority_count: self.hot_set_priority_count,
        });
        self.enforce_resource_policy();
    }

    pub(super) fn enforce_resource_policy(&mut self) {
        let metrics = self.engine.metrics();
        let retention_expired = metrics.active_consumers == 0
            && self.last_consumer_activity.elapsed()
                >= Duration::from_secs(self.resource_policy.warm_retention_seconds);
        let retained_market_series = if self.resource_mode == ResourceMode::MarketsLive {
            self.rithmic_live.len() + self.hyperliquid_live.len()
        } else {
            0
        };
        let maximum_cached_series = if retention_expired {
            0
        } else {
            self.resource_policy
                .maximum_cached_series
                .max(retained_market_series)
        };
        let maximum_decoded_bars = if retention_expired {
            0
        } else {
            self.resource_policy.maximum_decoded_bars
        };
        let warm_limit = if retention_expired {
            0
        } else {
            self.resource_policy.maximum_derived_series
        };
        let protected = self
            .rithmic_live
            .keys()
            .chain(self.hyperliquid_live.keys())
            .chain(self.warm_priority.iter().take(warm_limit))
            .cloned()
            .collect::<Vec<_>>();
        self.engine.evict_unsubscribed_series(
            maximum_cached_series,
            maximum_decoded_bars,
            &protected,
        );
        self.reconcile_order_books();
    }

    pub(super) fn reconcile_order_books(&mut self) {
        let mut required_identities = BTreeSet::new();
        if self.resource_mode != ResourceMode::OfflineSuspended {
            for (series, subscription) in self.engine.subscriptions() {
                if subscription.streams.contains(MarketStream::Depth) {
                    required_identities.insert((
                        series.provider_id.clone(),
                        series.instrument_id.clone(),
                        series.entitlement_id.clone(),
                    ));
                }
            }
            if self.resource_mode == ResourceMode::MarketsLive {
                required_identities.extend(
                    self.retained_live
                        .iter()
                        .filter(|series| {
                            chart_stream_requirements(series).contains(MarketStream::Depth)
                        })
                        .map(|series| {
                            (
                                series.provider_id.clone(),
                                series.instrument_id.clone(),
                                series.entitlement_id.clone(),
                            )
                        }),
                );
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
                book.instrument = instrument;
            } else {
                self.order_books
                    .insert(identity, ProviderOrderBook::new(instrument));
            }
        }
    }

    pub(super) fn suspend_provider_work(&mut self) {
        for events in self.events.values_mut() {
            events.clear_series();
        }
        let demanded = self
            .engine
            .subscriptions()
            .into_iter()
            .map(|(series, _)| series)
            .collect::<BTreeSet<_>>();
        for series in demanded {
            self.broadcast_series_recovery_for(
                &series,
                "Market access is suspended until the account is ready",
            );
        }
        self.suspended_history
            .extend(self.history_cancellations.keys().cloned());
        for stop in self.history_cancellations.values() {
            stop.store(true, Ordering::Release);
        }
        self.rithmic_pending_demand = None;
        self.rithmic_live.clear();
        self.hyperliquid_demand_dirty = false;
        self.hyperliquid_catalog_degraded = None;
        self.hyperliquid_live.clear();
        self.order_books.clear();
        self.stop_realtime_if_idle();
    }

    pub(super) fn status(&self) -> MarketServiceStatus {
        let metrics = self.engine.metrics();
        MarketServiceStatus {
            resource_mode: self.resource_mode,
            connected_desktop_clients: self.attached.len(),
            providers: ["rithmic", "hyperliquid"]
                .into_iter()
                .filter_map(|provider| self.provider_state(provider))
                .collect(),
            retained_series: metrics.stored_series,
            retained_bars: metrics.stored_bars,
            approximate_series_bytes: metrics.approximate_series_bytes,
        }
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
            state: state as i32,
            generation: status.generation.map_or(0, |generation| generation.0.get()),
            detail: self.providers.detail(provider),
            transport_rtt_nanos: self.providers.transport_rtt_nanos(provider),
        })
    }

    pub(super) fn accept_series_demand(
        &mut self,
        client_id: ClientId,
        series: &BarSeriesKey,
        waiter: &DemandWaiter,
    ) -> Result<
        (
            ProviderGeneration,
            Option<axiusflow_market_engine::ConsumerPublication>,
        ),
        String,
    > {
        authorize_consumer(&self.engine, client_id, waiter.consumer_id)?;
        let provider_generation = self.provider_generation_for_series(series)?;
        let streams = chart_stream_requirements(series);
        let publication = self
            .engine
            .set_series_demand_with_streams(waiter.consumer_id, waiter.generation, series, streams)
            .map_err(|error| error.to_string())?;
        self.reconcile_order_books();
        self.remove_waiter(waiter.consumer_id);
        let provider_state = self
            .provider_state(&series.provider_id)
            .map(envelope::Payload::ProviderState);
        if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
            events.clear_series();
            events.series_state = None;
            events.demand_error = None;
            // Demand can attach to an already-online resident session. Replay
            // its current state so a reconnecting desktop does not wait for a
            // provider transition that may never occur.
            events.provider = provider_state;
        }
        Ok((provider_generation, publication))
    }

    pub(super) fn handle_demand(
        &mut self,
        client_id: ClientId,
        series: &BarSeriesKey,
        waiter: DemandWaiter,
        reply: &Reply<()>,
    ) {
        if self.resource_mode == ResourceMode::OfflineSuspended {
            let _ = reply.send(Err(
                "engine market work is offline and suspended".to_string()
            ));
            return;
        }
        let (provider_generation, publication) =
            match self.accept_series_demand(client_id, series, &waiter) {
                Ok(accepted) => accepted,
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            };
        self.prune_unused_live_series();
        self.prune_history_tracking();
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
