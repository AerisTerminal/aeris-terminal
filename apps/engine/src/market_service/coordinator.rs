use super::{
    ActiveViewport, Arc, AtomicBool, BTreeMap, BTreeSet, BarSeriesKey, COORDINATOR_TICK, ClientId,
    Command, ConsumerEvents, ConsumerId, ConsumerIdentity, ConsumerResourceClass, DemandWaiter,
    Duration, EMPTY_REPAIR_RETRY_DELAY, EngineError, EngineResourceMode, FailureStage,
    HistoryRange, InstallProviderInstrument, Instant, LiveHandoff, MAXIMUM_SERIES, MarketEngine,
    MarketServiceStatus, MarketStream, Ordering, PendingLiveEdgeRepair, PersistenceState,
    ProviderConnectionState, ProviderDispatch, ProviderGeneration, ProviderHealth,
    ProviderOrderBook, ProviderRuntimeEvent, ProviderRuntimeRegistry, ProviderState,
    REALTIME_DRAIN_BUDGET, Receiver, RecvTimeoutError, Reply, ResourceMode, ResourcePolicyDecision,
    ResourcePolicyInput, RithmicLiveHandoff, StorageRequest, StoredHistory, SyncSender, WarmSeries,
    authorize_consumer, chart_stream_requirements, decide_resource_policy, envelope,
    local_history_failure_stage, publish_ready, resource_policy_mode, thread,
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
        pending_live_edge_repairs: BTreeMap::new(),
        history_cancellations: BTreeMap::new(),
        history_coverage: BTreeMap::new(),
        viewport_history_ranges: BTreeMap::new(),
        active_viewports: BTreeMap::new(),
        viewport_history_local_inflight: BTreeSet::new(),
        deferred_publications: BTreeSet::new(),
        pending_empty_repairs: BTreeMap::new(),
        empty_repair_retry_at: Instant::now(),
        history_retries: BTreeMap::new(),
        local_history_deadlines: BTreeMap::new(),
        local_loaded: BTreeSet::new(),
        warming: BTreeSet::new(),
        warm_series: BTreeMap::new(),
        warm_priority: Vec::new(),
        retained_history: BTreeMap::new(),
        prewarmed: BTreeSet::new(),
        retained_live: BTreeSet::new(),
        warm_rithmic_search_generation: 0,
        events: BTreeMap::new(),
        live: BTreeMap::new(),
        rithmic_live: BTreeMap::new(),
        order_books: BTreeMap::new(),
        catalog: BTreeMap::new(),
        catalog_sessions: BTreeMap::new(),
        catalog_selections: BTreeMap::new(),
        realtime_started: false,
        realtime_connected: false,
        rithmic_selection: None,
        rithmic_pending_selection: None,
        realtime_products: BTreeSet::new(),
    };
    loop {
        if shutdown.load(Ordering::Acquire) {
            coordinator.begin_shutdown();
            return;
        }
        drain_coordinator_events(&mut coordinator);
        if coordinator.providers.coinbase_overflowed() {
            coordinator
                .realtime_interrupted(FailureStage::Handoff, "Coinbase realtime queue overflowed");
        }
        coordinator.publish_live();
        coordinator.publish_rithmic_live();
        coordinator.recover_overflowed_series_queues();
        coordinator.flush_attached_events();
        coordinator.flush_rithmic_selection();
        if !coordinator.live.is_empty() {
            let _ = coordinator.sync_coinbase_realtime();
        }
        coordinator.expire_local_history_reads();
        coordinator.retry_pending_empty_repairs();
        coordinator.retry_history();
        coordinator.flush_coinbase_live_edge_repairs();
        coordinator.recover_stalled_depth_snapshots();
        coordinator.enforce_resource_policy();
        match commands.recv_timeout(COORDINATOR_TICK) {
            Ok(command) if !shutdown.load(Ordering::Acquire) => {
                coordinator.handle_command(command);
            }
            Ok(_) => {
                coordinator.begin_shutdown();
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn drain_coordinator_events(coordinator: &mut Coordinator<'_>) {
    for lane in 0..4 {
        for _ in 0..REALTIME_DRAIN_BUDGET {
            let Some(event) = coordinator.providers.take_event(lane) else {
                break;
            };
            match event {
                ProviderRuntimeEvent::CoinbaseRealtime(event) => {
                    coordinator.handle_realtime(event);
                }
                ProviderRuntimeEvent::RithmicRealtime(event) => {
                    coordinator.handle_rithmic_realtime(event);
                }
                ProviderRuntimeEvent::CoinbaseCatalog(event) => {
                    coordinator.handle_coinbase_catalog(event);
                }
                ProviderRuntimeEvent::RithmicCatalog(event) => {
                    coordinator.handle_rithmic_catalog(event);
                }
            }
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
    pub(super) pending_live_edge_repairs:
        BTreeMap<(BarSeriesKey, ProviderGeneration), PendingLiveEdgeRepair>,
    pub(super) history_cancellations: BTreeMap<(BarSeriesKey, ProviderGeneration), Arc<AtomicBool>>,
    pub(super) history_coverage: BTreeMap<BarSeriesKey, Vec<HistoryRange>>,
    pub(super) viewport_history_ranges: BTreeMap<(BarSeriesKey, ProviderGeneration), HistoryRange>,
    pub(super) active_viewports: BTreeMap<ConsumerId, ActiveViewport>,
    pub(super) viewport_history_local_inflight:
        BTreeSet<(BarSeriesKey, ProviderGeneration, HistoryRange)>,
    pub(super) deferred_publications: BTreeSet<BarSeriesKey>,
    pub(super) pending_empty_repairs: BTreeMap<BarSeriesKey, HistoryRange>,
    pub(super) empty_repair_retry_at: Instant,
    pub(super) history_retries: BTreeMap<(BarSeriesKey, ProviderGeneration), (Instant, u8)>,
    pub(super) local_history_deadlines: BTreeMap<(BarSeriesKey, ProviderGeneration), Instant>,
    pub(super) local_loaded: BTreeSet<(BarSeriesKey, ProviderGeneration)>,
    pub(super) warming: BTreeSet<(BarSeriesKey, ProviderGeneration)>,
    pub(super) warm_series: BTreeMap<BarSeriesKey, WarmSeries>,
    pub(super) warm_priority: Vec<BarSeriesKey>,
    pub(super) retained_history: BTreeMap<BarSeriesKey, StoredHistory>,
    pub(super) prewarmed: BTreeSet<BarSeriesKey>,
    pub(super) retained_live: BTreeSet<BarSeriesKey>,
    pub(super) warm_rithmic_search_generation: u64,
    pub(super) events: BTreeMap<ConsumerId, ConsumerEvents>,
    pub(super) live: BTreeMap<BarSeriesKey, LiveHandoff>,
    pub(super) rithmic_live: BTreeMap<BarSeriesKey, RithmicLiveHandoff>,
    pub(super) order_books: BTreeMap<(String, String), ProviderOrderBook>,
    pub(super) catalog: BTreeMap<(String, String), InstallProviderInstrument>,
    pub(super) catalog_sessions: BTreeMap<String, u64>,
    pub(super) catalog_selections: BTreeMap<String, u64>,
    pub(super) realtime_started: bool,
    pub(super) realtime_connected: bool,
    /// The instrument the Rithmic live worker is currently selected on.
    ///
    /// A boolean here could only say "a selection was sent once", so changing
    /// symbol never sent the replacement `Select` and the worker stayed on the
    /// previous instrument: the new chart received the old contract's trades, or
    /// nothing at all. Holding the identity makes "the selection is stale" a
    /// question with an answer, and holding the generation makes a selection
    /// from a retired session stale by construction.
    pub(super) rithmic_selection: Option<RithmicSelection>,
    /// A replacement selection the control channel could not take yet.
    ///
    /// One slot, newest wins: the worker itself already coalesces queued
    /// selections, so a switch that arrives while the channel is full only has
    /// to survive until the next tick rather than fail the demand behind it.
    pub(super) rithmic_pending_selection: Option<InstallProviderInstrument>,
    pub(super) realtime_products: BTreeSet<String>,
}

/// The Rithmic live worker's current instrument selection.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct RithmicSelection {
    pub(super) instrument_id: String,
    pub(super) generation: ProviderGeneration,
}

impl Coordinator<'_> {
    pub(super) fn detach_client(&mut self, client_id: ClientId) {
        for consumer_id in self.engine.detach_client(client_id) {
            self.providers.release_catalog_consumer(consumer_id.0.get());
            self.events.remove(&consumer_id);
            self.consumer_clients.remove(&consumer_id);
            self.remove_waiter(consumer_id);
            self.active_viewports.remove(&consumer_id);
        }
    }

    pub(super) fn begin_shutdown(&self) {
        for stop in self.history_cancellations.values() {
            stop.store(true, Ordering::Release);
        }
        self.providers.stop("coinbase");
        self.providers.stop("rithmic");
    }

    pub(super) fn handle_command(&mut self, command: Command) {
        match command {
            Command::HistoryCompleted(series, generation, range, kind, result) => {
                self.history_completed(&series, generation, range, kind, result);
            }
            Command::LocalHistoryCompleted(series, generation, result) => {
                self.local_history_completed(&series, generation, result);
            }
            Command::ViewportHistoryLocalCompleted(series, generation, range, result) => {
                self.viewport_history_local_completed(&series, generation, range, result);
            }
            Command::PersistenceCompleted(series, generation, result, elapsed_millis) => {
                self.persistence_completed(&series, generation, result, elapsed_millis);
            }
            Command::ConfirmedEmptyRecorded(series, result) => {
                if let Err(error) = result {
                    self.broadcast_persistence_for(
                        &series,
                        PersistenceState::Degraded,
                        Some("Confirmed-empty history persistence is degraded"),
                    );
                    self.broadcast_demand_error_for(
                        &series,
                        local_history_failure_stage(error),
                        &error.to_string(),
                        None,
                    );
                }
            }
            Command::ConfirmedEmptyResolved(series, range, result) => {
                if result.is_err() {
                    self.remember_pending_empty_repair(&series, range);
                    self.empty_repair_retry_at = Instant::now() + EMPTY_REPAIR_RETRY_DELAY;
                }
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
                self.apply_resource_mode(mode);
                let _ = reply.send(Ok(()));
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
                        if let Some(publication) = publication
                            && let Some(events) = self.events.get_mut(&consumer_id)
                        {
                            publish_ready(events, &publication);
                        }
                        Ok(())
                    });
                self.reconcile_order_books();
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
            | Command::ViewportHistoryLocalCompleted(..)
            | Command::PersistenceCompleted(..)
            | Command::ConfirmedEmptyRecorded(..)
            | Command::ConfirmedEmptyResolved(..)
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
        if let Err(error) = self
            .providers
            .authorize_catalog_consumer(identity.consumer_id.0.get())
        {
            self.engine.remove_consumer(identity.consumer_id);
            return Err(error);
        }
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
        self.active_viewports.remove(&consumer_id);
        self.engine.remove_consumer(consumer_id);
        self.providers.release_catalog_consumer(consumer_id.0.get());
        self.release_unused_live_market_data();
        Ok(())
    }

    pub(super) fn apply_resource_mode(&mut self, mode: ResourceMode) {
        self.resource_mode = mode;
        self.refresh_resource_policy();
        if mode == ResourceMode::OfflineSuspended {
            self.suspend_provider_work();
        } else if mode == ResourceMode::MarketsLive {
            self.activate_markets_live_hot_set();
        }
        self.release_unused_live_market_data();
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
            self.live.len().saturating_add(self.rithmic_live.len())
        } else {
            0
        };
        let maximum_cached_series = if retention_expired {
            0
        } else {
            self.resource_policy
                .maximum_derived_series
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
            .live
            .keys()
            .chain(self.rithmic_live.keys())
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
            for consumer_id in self.events.keys() {
                let Some(demand) = self.engine.current_demand(*consumer_id) else {
                    continue;
                };
                let Some(series) = demand.series.as_ref() else {
                    continue;
                };
                if demand.streams.is_some_and(|streams| {
                    streams.contains(MarketStream::Depth)
                        && (demand.resource_class == ConsumerResourceClass::Foreground
                            || self.resource_policy.retain_hidden_depth)
                }) {
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
                    && instrument == &book.instrument
            })
        });
        for instrument in required {
            let identity = (
                instrument.provider.clone(),
                instrument.instrument_id.clone(),
            );
            let replace = self
                .order_books
                .get(&identity)
                .is_none_or(|book| book.instrument != instrument);
            if replace {
                self.order_books
                    .insert(identity, ProviderOrderBook::new(instrument));
            }
        }
    }

    pub(super) fn suspend_provider_work(&mut self) {
        for stop in self.history_cancellations.values() {
            stop.store(true, Ordering::Release);
        }
        self.providers.stop("coinbase");
        self.providers.stop("rithmic");
        self.realtime_started = false;
        self.realtime_connected = false;
        self.rithmic_selection = None;
        self.rithmic_pending_selection = None;
        self.realtime_products.clear();
        self.live.clear();
        self.rithmic_live.clear();
        self.order_books.clear();
    }

    pub(super) fn status(&self) -> MarketServiceStatus {
        let metrics = self.engine.metrics();
        MarketServiceStatus {
            resource_mode: self.resource_mode,
            connected_desktop_clients: self.attached.len(),
            providers: ["coinbase", "rithmic"]
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
        if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
            events.clear_series();
            events.series_state = None;
            events.demand_error = None;
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
        if let Err(error) = self.validate_coinbase_realtime_capacity(series, waiter.consumer_id) {
            let _ = reply.send(Err(error));
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
        self.active_viewports.remove(&waiter.consumer_id);
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
