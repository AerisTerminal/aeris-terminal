use super::{
    Arc, AtomicBool, BTreeMap, BTreeSet, BarSeriesKey, COORDINATOR_TICK, ClientId, Command,
    ConsumerEvents, ConsumerId, ConsumerIdentity, ConsumerResourceClass, DeferredHistoryRequest,
    DemandWaiter, EngineError, GenerationId, HistoryRange, HyperliquidLiveHandoff,
    InstallProviderInstrument, Instant, MarketEngine, MarketServiceStatus, MarketStream, Ordering,
    ProviderConnectionState, ProviderDispatch, ProviderGeneration, ProviderHealth,
    ProviderOrderBook, ProviderRuntimeEvent, ProviderRuntimeRegistry, ProviderState,
    REALTIME_CAPACITY, REALTIME_DRAIN_BUDGET, Receiver, RecvTimeoutError, Reply,
    RithmicLiveHandoff, RithmicRealtimeDemand, StreamRequirements, authorize_consumer, thread,
};
use crate::MarketRuntimeEvent;

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
        .name("axiusflow-market-engine".to_string())
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
        hyperliquid_live: BTreeMap::new(),
        order_books: BTreeMap::new(),
        catalog: BTreeMap::new(),
        catalog_sessions: BTreeMap::new(),
        catalog_searches: BTreeMap::new(),
        catalog_selections: BTreeMap::new(),
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
        coordinator.flush_rithmic_demand();
        coordinator.flush_hyperliquid_demand();
        coordinator.stop_realtime_if_idle();
        coordinator.retry_history();
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
/// accepted provider events without turning shutdown into an unbounded wait for a live feed.
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
    pub(super) hyperliquid_live: BTreeMap<BarSeriesKey, HyperliquidLiveHandoff>,
    pub(super) order_books: BTreeMap<(String, String), ProviderOrderBook>,
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
            self.catalog_searches
                .retain(|(candidate, _), _| *candidate != consumer_id);
            self.catalog_selections
                .retain(|(candidate, _), _| *candidate != consumer_id);
            self.remove_waiter(consumer_id);
        }
    }

    pub(super) fn begin_shutdown(&mut self) {
        self.publish_rithmic_live();
        self.publish_hyperliquid_live();
        for stop in self.history_cancellations.values() {
            stop.store(true, Ordering::Release);
        }
        let _ = self.providers.stop("rithmic");
        let _ = self.providers.stop("hyperliquid");
    }

    pub(super) fn handle_command(&mut self, command: Command) {
        match command {
            Command::ProviderWake => (),
            Command::HistoryCompleted(series, generation, range, result) => {
                self.history_completed(&series, generation, range, result);
            }
            command @ (Command::Status(..)
            | Command::Attach(..)
            | Command::Detach(..)
            | Command::PollClient(..)) => self.handle_service_command(command),
            command => self.handle_consumer_command(command),
        }
    }

    pub(super) fn handle_service_command(&mut self, command: Command) {
        match command {
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
            | Command::ProviderWake
            | Command::Status(..)
            | Command::Attach(..)
            | Command::Detach(..)
            | Command::PollClient(..) => unreachable!("command was routed to the wrong dispatcher"),
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
                // A full snapshot queued just before a pane hid must not leak
                // through after the class change. Canonical depth remains
                // runtime-owned and is republished on Foreground restore.
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
            self.ensure_realtime(&series)?;
            if streams.contains(MarketStream::Depth) {
                self.publish_order_book_to_consumer(consumer_id);
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
        self.events.remove(&consumer_id);
        self.consumer_clients.remove(&consumer_id);
        self.catalog_searches
            .retain(|(candidate, _), _| *candidate != consumer_id);
        self.catalog_selections
            .retain(|(candidate, _), _| *candidate != consumer_id);
        self.remove_waiter(consumer_id);
        self.engine.remove_consumer(consumer_id);
        self.release_unused_live_market_data();
        Ok(())
    }

    pub(super) fn reconcile_order_books(&mut self) {
        let mut required_identities = BTreeSet::new();
        for (series, subscription) in self.engine.subscriptions() {
            if subscription.streams.contains(MarketStream::Depth) {
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
    }

    pub(super) fn status(&self) -> MarketServiceStatus {
        let metrics = self.engine.metrics();
        MarketServiceStatus {
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
        streams: StreamRequirements,
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
        FormingBar, HISTORY_FAILED_RETRY_COOLDOWN, HISTORY_SERIES_HIGH_WATERMARK,
        HISTORY_SERIES_TARGET_BARS, HistorySnapshot, INITIAL_HISTORY_BARS, LiveHistoryState,
        MAXIMUM_HISTORY_RETRIES, MAXIMUM_STORED_BARS,
    };
    use axiusflow_engine_protocol::ProviderInstrumentSearchResult;
    use axiusflow_market_data::{
        AggressorSide, BarPeriod, EventMetadata, MarketBar, MarketTrade, QualifiedTimestamp,
    };
    use axiusflow_market_engine::{ConsumerResourceClass, GenerationId, Viewport, WorkspaceId};
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
            display_symbol: "BTC-PERP".to_string(),
            venue_id: "Hyperliquid".to_string(),
            price_scale: 2,
            quantity_scale: 8,
            entitlement_id: "hyperliquid-public".to_string(),
            price_increment: None,
        }
    }

    fn coordinator() -> Coordinator<'static> {
        Coordinator {
            engine: super::super::configured_engine().expect("configured engine"),
            providers: ProviderDispatch {
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
            hyperliquid_live: BTreeMap::new(),
            order_books: BTreeMap::new(),
            catalog: BTreeMap::new(),
            catalog_sessions: BTreeMap::new(),
            catalog_searches: BTreeMap::new(),
            catalog_selections: BTreeMap::new(),
            rithmic_demand: None,
            rithmic_pending_demand: None,
            rithmic_stop_pending: None,
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

    struct DetachedRithmicFixture {
        coordinator: Coordinator<'static>,
        series: BarSeriesKey,
        provider_generation: ProviderGeneration,
        consumer_id: ConsumerId,
        boundary: i64,
        live_timestamp: i64,
        detached_snapshot: Arc<axiusflow_market_engine::SeriesSnapshot>,
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

        let retained = axiusflow_market_engine::ConsumerPublication {
            consumer_id: consumer,
            generation: generation(7),
            publication_generation: 1,
            snapshot: Arc::new(axiusflow_market_engine::SeriesSnapshot {
                series: selected_series.clone(),
                provider_generation: ProviderGeneration(nonzero(1)),
                publication_generation: 1,
                price_scale: 2,
                quantity_scale: 0,
                forming: false,
                bars: Arc::from([axiusflow_market_data::MarketBar {
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
        let mut live = HyperliquidLiveHandoff::new(
            selected_series.clone(),
            provider_generation,
            "BTC".to_string(),
            "1m".to_string(),
        );
        live.history_state = LiveHistoryState::Ready;
        coordinator.hyperliquid_live.insert(selected_series, live);

        coordinator.hyperliquid_online(1);

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
                        axiusflow_market_data::OrderBookState::Recovering(
                            axiusflow_market_data::OrderBookRecoveryReason::AwaitingSnapshot
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
