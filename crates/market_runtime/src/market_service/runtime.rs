use super::{
    ActiveWorkerGuard, Arc, AtomicBool, AtomicU64, BTreeMap, BTreeSet, BarSeriesKey,
    COMMAND_CAPACITY, ClientId, Command, ConsumerId, ConsumerIdentity, ConsumerResourceClass,
    Duration, GenerationId, HISTORY_CAPACITY, HistoryRequest, HistorySnapshot, HistorySource,
    InstallProviderInstrument, Instant, LiveHyperliquidHistory, LiveRithmicHistory, MarketEngine,
    MarketRuntime, MarketService, MarketServiceStatus, Mutex, NativeStudyRegistration, Ordering,
    OwnedCoordinatorChannels, ProviderCatalogChannelSet, ProviderCatalogChannels,
    ProviderCatalogCommand, ProviderCatalogDispatch, ProviderCoordinatorWake, ProviderDispatch,
    ProviderDispatchRecord, ProviderRealtimeChannelSet, ProviderRealtimeChannels,
    ProviderRealtimeDispatch, ProviderRuntimeEvent, ProviderRuntimeLifecycle,
    ProviderRuntimeRecord, ProviderRuntimeRegistry, ProviderRuntimeSpec, REALTIME_CAPACITY,
    RITHMIC_REALTIME_CONTROL_CAPACITY, Reply, RithmicCatalogControl, RithmicRealtimeControl,
    RithmicRealtimeEvent, SearchProviderInstruments, SelectProviderInstrument,
    StartedProviderRuntime, StreamRequirements, StudyInstanceId, SyncSender, TrySendError,
    Viewport, WorkspaceId, configured_engine, configured_reconnect_delay, id, mpsc,
    spawn_coordinator, spawn_history_worker, thread, try_send_hyperliquid_catalog,
    try_send_rithmic_catalog, validate_provider_instrument, validate_provider_search,
    validate_provider_selection,
};
use crate::MarketRuntimeEvent;
use crate::hyperliquid_display_depth::{
    HyperliquidDisplayDepthControl, HyperliquidDisplayDepthEvent,
};
use crate::hyperliquid_realtime::{
    HyperliquidCatalogControl, HyperliquidRealtimeControl, HyperliquidRealtimeEvent,
};

impl HistorySource for LiveRithmicHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        if request.series.provider_id != "rithmic" {
            return Err("Rithmic history received another provider".to_string());
        }
        fetch_rithmic_history(request)
    }
}

fn fetch_rithmic_history(request: &HistoryRequest) -> Result<HistorySnapshot, String> {
    let installed = request
        .instrument
        .as_ref()
        .ok_or_else(|| "Rithmic instrument is not installed".to_string())?;
    let snapshot = crate::rithmic_history::fetch(
        &request.series,
        request.provider_generation.0.get(),
        installed,
        request.req_window(),
        &request.stop,
    )?;
    Ok(HistorySnapshot {
        price_scale: snapshot.price_scale,
        quantity_scale: snapshot.quantity_scale,
        bars: snapshot.bars,
        forming: snapshot.forming,
        handoff_boundary_unix_nanos: Some(snapshot.handoff_boundary_unix_nanos),
    })
}

impl HistorySource for LiveHyperliquidHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        if request.series.provider_id != "hyperliquid" {
            return Err("Hyperliquid history received another provider".to_string());
        }
        let installed = request
            .instrument
            .as_ref()
            .ok_or_else(|| "Hyperliquid instrument is not installed".to_string())?;
        let snapshot = crate::hyperliquid_history::fetch(
            &mut self.0,
            &request.series,
            request.provider_generation.0.get(),
            installed,
            request.req_window(),
            &request.stop,
        )?;
        Ok(HistorySnapshot {
            price_scale: snapshot.price_scale,
            quantity_scale: snapshot.quantity_scale,
            bars: snapshot.bars,
            forming: snapshot.forming,
            handoff_boundary_unix_nanos: Some(snapshot.handoff_boundary_unix_nanos),
        })
    }
}

impl ProviderRuntimeRegistry {
    pub(super) fn start(
        specs: Vec<ProviderRuntimeSpec>,
        completions: &SyncSender<Command>,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
    ) -> Result<Self, String> {
        let wake = ProviderCoordinatorWake::new(completions.clone());
        let mut registry = Self {
            wake: wake.clone(),
            records: BTreeMap::new(),
        };
        for spec in specs {
            if registry.records.contains_key(spec.provider_id) {
                registry.cancel_and_join();
                return Err(format!(
                    "provider runtime {} is configured more than once",
                    spec.provider_id
                ));
            }
            let provider_id = spec.provider_id;
            let record =
                match Self::start_record(spec, completions, wake.clone(), engine, active_workers) {
                    Ok(record) => record,
                    Err(error) => {
                        registry.cancel_and_join();
                        return Err(error);
                    }
                };
            registry.records.insert(provider_id, record);
        }
        Ok(registry)
    }

    fn start_record(
        spec: ProviderRuntimeSpec,
        completions: &SyncSender<Command>,
        wake: ProviderCoordinatorWake,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
    ) -> Result<ProviderRuntimeRecord, String> {
        let cancellation = Arc::new(AtomicBool::new(false));
        let lifecycle = Arc::new(ProviderRuntimeLifecycle::default());
        let (history_tx, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
        let history_name: &'static str = if spec.provider_id == "hyperliquid" {
            "asceify-hyperliquid-history"
        } else {
            "asceify-rithmic-history"
        };
        let history_worker = spawn_history_worker(
            history_name,
            spec.history,
            history_rx,
            completions.clone(),
            Arc::clone(&cancellation),
            Arc::clone(active_workers),
        )?;
        let started = StartedProviderRuntime {
            history: history_tx,
            cancellation,
            lifecycle,
            workers: vec![history_worker],
        };
        match spec.provider_id {
            "hyperliquid" => Self::start_hyperliquid_record(
                started,
                spec.realtime.enabled,
                wake,
                engine,
                active_workers,
            ),
            "rithmic" => Self::start_rithmic_record(
                started,
                spec.realtime.enabled,
                wake,
                engine,
                active_workers,
            ),
            provider => {
                started.cancel_and_join();
                Err(format!("market provider is unsupported: {provider}"))
            }
        }
    }

    fn start_rithmic_record(
        mut started: StartedProviderRuntime,
        enabled: bool,
        wake: ProviderCoordinatorWake,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
    ) -> Result<ProviderRuntimeRecord, String> {
        let reconnect_delay = if enabled {
            match configured_reconnect_delay(engine, "rithmic") {
                Ok(delay) => Some(delay),
                Err(error) => {
                    started.cancel_and_join();
                    return Err(error);
                }
            }
        } else {
            None
        };
        let (catalog_events_tx, catalog_events) = mpsc::sync_channel(COMMAND_CAPACITY);
        let catalog_events_tx = super::CatalogPublisher::new(catalog_events_tx, 0, wake.clone());
        let (catalog_controls, catalog_controls_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (realtime_events_tx, realtime_events) = mpsc::sync_channel(REALTIME_CAPACITY);
        let (realtime_controls, realtime_controls_rx) =
            mpsc::sync_channel(RITHMIC_REALTIME_CONTROL_CAPACITY);
        if enabled {
            let worker_cancellation = Arc::clone(&started.cancellation);
            let worker_lifecycle = Arc::clone(&started.lifecycle);
            let Some(reconnect_delay) = reconnect_delay else {
                started.cancel_and_join();
                return Err("Rithmic reconnect policy is unavailable".to_string());
            };
            let worker_activity = Arc::clone(active_workers);
            let provider = thread::Builder::new()
                .name("asceify-rithmic-provider".to_string())
                .spawn(move || {
                    let _activity =
                        ActiveWorkerGuard::register("asceify-rithmic-provider", worker_activity);
                    crate::rithmic_realtime::run(
                        &catalog_controls_rx,
                        &catalog_events_tx,
                        &realtime_controls_rx,
                        &realtime_events_tx,
                        &wake,
                        reconnect_delay,
                    );
                    if !worker_cancellation.load(Ordering::Acquire) {
                        worker_lifecycle
                            .mark_terminal_failure("Rithmic provider runtime stopped unexpectedly");
                    }
                })
                .map_err(|error| error.to_string());
            match provider {
                Ok(provider) => started.workers.push(provider),
                Err(error) => {
                    started.cancel_and_join();
                    return Err(error);
                }
            }
        }
        Ok(ProviderRuntimeRecord {
            history: started.history,
            cancellation: started.cancellation,
            lifecycle: started.lifecycle,
            realtime: ProviderRealtimeChannels {
                enabled,
                channels: ProviderRealtimeChannelSet::Rithmic {
                    controls: realtime_controls,
                    events: realtime_events,
                },
            },
            catalog: ProviderCatalogChannels {
                enabled,
                channels: ProviderCatalogChannelSet::Rithmic {
                    controls: catalog_controls,
                    events: catalog_events,
                },
            },
            workers: started.workers,
        })
    }

    fn start_hyperliquid_record(
        mut started: StartedProviderRuntime,
        enabled: bool,
        wake: ProviderCoordinatorWake,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
    ) -> Result<ProviderRuntimeRecord, String> {
        let reconnect_delay = if enabled {
            match configured_reconnect_delay(engine, "hyperliquid") {
                Ok(delay) => Some(delay),
                Err(error) => {
                    started.cancel_and_join();
                    return Err(error);
                }
            }
        } else {
            None
        };
        let (catalog_events_tx, catalog_events) = mpsc::sync_channel(COMMAND_CAPACITY);
        let catalog_events_tx = super::CatalogPublisher::new(catalog_events_tx, 1, wake.clone());
        let (catalog_controls, catalog_controls_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (realtime_events_tx, realtime_events) = mpsc::sync_channel(REALTIME_CAPACITY);
        let (realtime_controls, realtime_controls_rx) =
            mpsc::sync_channel(RITHMIC_REALTIME_CONTROL_CAPACITY);
        let (display_events_tx, display_events) = mpsc::sync_channel(REALTIME_CAPACITY);
        let (display_controls, display_controls_rx) =
            mpsc::sync_channel(RITHMIC_REALTIME_CONTROL_CAPACITY);
        // One generation counter shared by the catalog thread (install
        // stamping) and the realtime thread (socket sessions). The realtime
        // thread owns it: every connection publishes its generation here.
        let ws_generation = Arc::new(AtomicU64::new(1));
        if enabled {
            let Some(reconnect_delay) = reconnect_delay else {
                started.cancel_and_join();
                return Err("Hyperliquid reconnect policy is unavailable".to_string());
            };
            if let Err(error) = Self::spawn_hyperliquid_catalog(
                &mut started,
                active_workers,
                catalog_controls_rx,
                catalog_events_tx,
                Arc::clone(&ws_generation),
            ) {
                started.cancel_and_join();
                return Err(error);
            }
            if let Err(error) = Self::spawn_hyperliquid_provider(
                &mut started,
                active_workers,
                realtime_controls_rx,
                realtime_events_tx,
                ws_generation,
                wake.clone(),
                reconnect_delay,
            ) {
                started.cancel_and_join();
                return Err(error);
            }
            if let Err(error) = Self::spawn_hyperliquid_display_depth(
                &mut started,
                active_workers,
                display_controls_rx,
                display_events_tx,
                wake,
                reconnect_delay,
            ) {
                started.cancel_and_join();
                return Err(error);
            }
        }
        Ok(ProviderRuntimeRecord {
            history: started.history,
            cancellation: started.cancellation,
            lifecycle: started.lifecycle,
            realtime: ProviderRealtimeChannels {
                enabled,
                channels: ProviderRealtimeChannelSet::Hyperliquid {
                    controls: realtime_controls,
                    events: realtime_events,
                    display_controls,
                    display_events,
                },
            },
            catalog: ProviderCatalogChannels {
                enabled,
                channels: ProviderCatalogChannelSet::Hyperliquid {
                    controls: catalog_controls,
                    events: catalog_events,
                },
            },
            workers: started.workers,
        })
    }

    fn spawn_hyperliquid_catalog(
        started: &mut StartedProviderRuntime,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
        controls: mpsc::Receiver<crate::hyperliquid_realtime::HyperliquidCatalogControl>,
        events: super::CatalogPublisher<crate::hyperliquid_realtime::HyperliquidCatalogEvent>,
        ws_generation: Arc<AtomicU64>,
    ) -> Result<(), String> {
        let cancellation = Arc::clone(&started.cancellation);
        let lifecycle = Arc::clone(&started.lifecycle);
        let activity = Arc::clone(active_workers);
        let worker = thread::Builder::new()
            .name("asceify-hyperliquid-catalog".to_string())
            .spawn(move || {
                let _activity =
                    ActiveWorkerGuard::register("asceify-hyperliquid-catalog", activity);
                crate::hyperliquid_realtime::run_catalog(
                    &controls,
                    &events,
                    &ws_generation,
                    &cancellation,
                    asceify_hyperliquid_market_adapter::HyperliquidHttpConfig::default(),
                );
                if !cancellation.load(Ordering::Acquire) {
                    lifecycle
                        .mark_terminal_failure("Hyperliquid catalog runtime stopped unexpectedly");
                }
            })
            .map_err(|error| error.to_string())?;
        started.workers.push(worker);
        Ok(())
    }

    fn spawn_hyperliquid_provider(
        started: &mut StartedProviderRuntime,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
        controls: mpsc::Receiver<crate::hyperliquid_realtime::HyperliquidRealtimeControl>,
        events: mpsc::SyncSender<crate::hyperliquid_realtime::HyperliquidRealtimeEvent>,
        ws_generation: Arc<AtomicU64>,
        wake: ProviderCoordinatorWake,
        reconnect_delay: Duration,
    ) -> Result<(), String> {
        let cancellation = Arc::clone(&started.cancellation);
        let lifecycle = Arc::clone(&started.lifecycle);
        let activity = Arc::clone(active_workers);
        let worker = thread::Builder::new()
            .name("asceify-hyperliquid-provider".to_string())
            .spawn(move || {
                let _activity =
                    ActiveWorkerGuard::register("asceify-hyperliquid-provider", activity);
                crate::hyperliquid_realtime::run(
                    &controls,
                    &events,
                    &cancellation,
                    &ws_generation,
                    &wake,
                    reconnect_delay,
                );
                if !cancellation.load(Ordering::Acquire) {
                    lifecycle
                        .mark_terminal_failure("Hyperliquid provider runtime stopped unexpectedly");
                }
            })
            .map_err(|error| error.to_string())?;
        started.workers.push(worker);
        Ok(())
    }

    fn spawn_hyperliquid_display_depth(
        started: &mut StartedProviderRuntime,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
        controls: mpsc::Receiver<HyperliquidDisplayDepthControl>,
        events: mpsc::SyncSender<HyperliquidDisplayDepthEvent>,
        wake: ProviderCoordinatorWake,
        reconnect_delay: Duration,
    ) -> Result<(), String> {
        let cancellation = Arc::clone(&started.cancellation);
        let activity = Arc::clone(active_workers);
        let worker = thread::Builder::new()
            .name("asceify-hyperliquid-display-depth".to_string())
            .spawn(move || {
                let _activity =
                    ActiveWorkerGuard::register("asceify-hyperliquid-display-depth", activity);
                crate::hyperliquid_display_depth::run(
                    &controls,
                    &events,
                    &cancellation,
                    &wake,
                    reconnect_delay,
                );
            })
            .map_err(|error| error.to_string())?;
        started.workers.push(worker);
        Ok(())
    }

    pub(super) fn dispatch(&self) -> ProviderDispatch<'_> {
        let records = self
            .records
            .iter()
            .map(|(provider_id, record)| {
                let realtime = match &record.realtime.channels {
                    ProviderRealtimeChannelSet::Rithmic { controls, events }
                        if record.realtime.enabled =>
                    {
                        ProviderRealtimeDispatch::Rithmic { controls, events }
                    }
                    ProviderRealtimeChannelSet::Hyperliquid {
                        controls,
                        events,
                        display_controls,
                        display_events,
                    } if record.realtime.enabled => ProviderRealtimeDispatch::Hyperliquid {
                        controls,
                        events,
                        display_controls,
                        display_events,
                    },
                    _ => ProviderRealtimeDispatch::Disabled,
                };
                let catalog = match &record.catalog.channels {
                    ProviderCatalogChannelSet::Rithmic { controls, events }
                        if record.catalog.enabled =>
                    {
                        ProviderCatalogDispatch::Rithmic { controls, events }
                    }
                    ProviderCatalogChannelSet::Hyperliquid { controls, events }
                        if record.catalog.enabled =>
                    {
                        ProviderCatalogDispatch::Hyperliquid { controls, events }
                    }
                    _ => ProviderCatalogDispatch::Disabled,
                };
                (
                    *provider_id,
                    ProviderDispatchRecord {
                        history: &record.history,
                        lifecycle: Some(&record.lifecycle),
                        realtime,
                        catalog,
                    },
                )
            })
            .collect();
        ProviderDispatch {
            records,
            wake: Some(&self.wake),
        }
    }

    pub(super) fn cancel_and_join(&mut self) -> Vec<String> {
        for record in self.records.values() {
            record.cancellation.store(true, Ordering::Release);
        }
        let records = std::mem::take(&mut self.records);
        let mut panicked = Vec::new();
        for (_, record) in records {
            let ProviderRuntimeRecord {
                history,
                realtime,
                catalog,
                workers,
                ..
            } = record;
            drop(history);
            drop(realtime);
            drop(catalog);
            for worker in workers {
                let name = worker.thread().name().unwrap_or("unnamed").to_string();
                if worker.join().is_err() {
                    panicked.push(name);
                }
            }
        }
        panicked
    }
}

impl Drop for ProviderRuntimeRegistry {
    fn drop(&mut self) {
        let _ = self.cancel_and_join();
    }
}

pub(super) fn join_runtime_workers(workers: Vec<thread::JoinHandle<()>>) {
    for worker in workers {
        let _ = worker.join();
    }
}

impl ProviderDispatch<'_> {
    pub(super) fn history(
        &self,
        provider_id: &str,
    ) -> Result<&SyncSender<HistoryRequest>, &'static str> {
        self.records
            .get(provider_id)
            .map(|record| record.history)
            .ok_or("provider history runtime is unavailable")
    }

    pub(super) fn detail(&self, provider_id: &str) -> Option<String> {
        self.records
            .get(provider_id)
            .and_then(|record| record.lifecycle)
            .and_then(ProviderRuntimeLifecycle::detail)
    }

    pub(super) fn rithmic_realtime_enabled(&self) -> bool {
        self.records
            .get("rithmic")
            .is_some_and(|record| record.realtime.rithmic_controls().is_some())
    }

    pub(super) fn observe_generation(
        &self,
        provider_id: &str,
        generation: u64,
        reconnecting: bool,
    ) {
        if let Some(lifecycle) = self
            .records
            .get(provider_id)
            .and_then(|record| record.lifecycle)
        {
            lifecycle.observe_generation(generation, reconnecting);
        }
    }

    pub(super) fn transport_rtt_nanos(&self, provider_id: &str) -> Option<u64> {
        self.records
            .get(provider_id)
            .and_then(|record| record.lifecycle)
            .and_then(ProviderRuntimeLifecycle::transport_rtt_nanos)
    }

    fn observe_transport_rtt(&self, provider_id: &str, generation: u64, transport_rtt_nanos: u64) {
        if let Some(lifecycle) = self
            .records
            .get(provider_id)
            .and_then(|record| record.lifecycle)
        {
            lifecycle.observe_transport_rtt(generation, transport_rtt_nanos);
        }
    }

    pub(super) fn take_event(&self, lane: usize) -> Option<ProviderRuntimeEvent> {
        let event = match lane {
            0 => match &self.records.get("rithmic")?.realtime {
                ProviderRealtimeDispatch::Rithmic { events, .. } => events
                    .try_recv()
                    .ok()
                    .map(ProviderRuntimeEvent::RithmicRealtime),
                ProviderRealtimeDispatch::Hyperliquid { .. }
                | ProviderRealtimeDispatch::Disabled => None,
            },
            1 => match &self.records.get("rithmic")?.catalog {
                ProviderCatalogDispatch::Rithmic { events, .. } => events
                    .try_recv()
                    .ok()
                    .map(ProviderRuntimeEvent::RithmicCatalog),
                ProviderCatalogDispatch::Hyperliquid { .. } | ProviderCatalogDispatch::Disabled => {
                    None
                }
            },
            2 => match &self.records.get("hyperliquid")?.realtime {
                ProviderRealtimeDispatch::Hyperliquid { events, .. } => events
                    .try_recv()
                    .ok()
                    .map(ProviderRuntimeEvent::HyperliquidRealtime),
                ProviderRealtimeDispatch::Rithmic { .. } | ProviderRealtimeDispatch::Disabled => {
                    None
                }
            },
            3 => match &self.records.get("hyperliquid")?.catalog {
                ProviderCatalogDispatch::Hyperliquid { events, .. } => events
                    .try_recv()
                    .ok()
                    .map(ProviderRuntimeEvent::HyperliquidCatalog),
                ProviderCatalogDispatch::Rithmic { .. } | ProviderCatalogDispatch::Disabled => None,
            },
            4 => match &self.records.get("hyperliquid")?.realtime {
                ProviderRealtimeDispatch::Hyperliquid { display_events, .. } => display_events
                    .try_recv()
                    .ok()
                    .map(ProviderRuntimeEvent::HyperliquidDisplayDepth),
                ProviderRealtimeDispatch::Rithmic { .. } | ProviderRealtimeDispatch::Disabled => {
                    None
                }
            },
            _ => None,
        }?;
        self.observe_event(&event);
        Some(event)
    }

    fn observe_event(&self, event: &ProviderRuntimeEvent) {
        // Do not let retired queued events overwrite out-of-band health.
        let retired = match event {
            ProviderRuntimeEvent::RithmicRealtime(event) => self
                .wake
                .is_some_and(|wake| wake.overflowed(0, event.generation())),
            ProviderRuntimeEvent::HyperliquidRealtime(event) => self
                .wake
                .is_some_and(|wake| wake.overflowed(1, event.generation())),
            _ => false,
        };
        if retired {
            return;
        }
        match event {
            ProviderRuntimeEvent::RithmicRealtime(event) => {
                let (generation, reconnecting, transport_rtt_nanos) = match event {
                    RithmicRealtimeEvent::Failed(generation, _)
                    | RithmicRealtimeEvent::Connecting(generation)
                    | RithmicRealtimeEvent::Recovering(generation, _)
                    | RithmicRealtimeEvent::Disconnected(generation, _) => {
                        (*generation, true, None)
                    }
                    RithmicRealtimeEvent::Heartbeat(generation, transport_rtt_nanos) => {
                        (*generation, false, *transport_rtt_nanos)
                    }
                    RithmicRealtimeEvent::Connected(generation)
                    | RithmicRealtimeEvent::Trade(generation, _)
                    | RithmicRealtimeEvent::Quote(generation, _)
                    | RithmicRealtimeEvent::Depth(generation, _) => (*generation, false, None),
                };
                self.observe_generation("rithmic", generation, reconnecting);
                if let Some(transport_rtt_nanos) = transport_rtt_nanos {
                    self.observe_transport_rtt("rithmic", generation, transport_rtt_nanos);
                }
            }
            ProviderRuntimeEvent::HyperliquidRealtime(event) => {
                let (generation, reconnecting, transport_rtt_nanos) = match event {
                    HyperliquidRealtimeEvent::Connecting(generation)
                    | HyperliquidRealtimeEvent::Recovering(generation)
                    | HyperliquidRealtimeEvent::Disconnected(generation) => {
                        (*generation, true, None)
                    }
                    HyperliquidRealtimeEvent::Heartbeat(generation, transport_rtt_nanos) => {
                        (*generation, false, *transport_rtt_nanos)
                    }
                    HyperliquidRealtimeEvent::Connected(generation)
                    | HyperliquidRealtimeEvent::Candle(generation, ..)
                    | HyperliquidRealtimeEvent::Trades(generation, _)
                    | HyperliquidRealtimeEvent::Quote(generation, _)
                    | HyperliquidRealtimeEvent::Depth(generation, _) => (*generation, false, None),
                };
                self.observe_generation("hyperliquid", generation, reconnecting);
                if let Some(transport_rtt_nanos) = transport_rtt_nanos {
                    self.observe_transport_rtt("hyperliquid", generation, transport_rtt_nanos);
                }
            }
            ProviderRuntimeEvent::RithmicCatalog(_)
            | ProviderRuntimeEvent::HyperliquidCatalog(_)
            | ProviderRuntimeEvent::HyperliquidDisplayDepth(_) => {}
        }
    }

    pub(super) fn dispatch_catalog(
        &self,
        provider_id: &str,
        command: ProviderCatalogCommand,
    ) -> Result<(), String> {
        let record = self
            .records
            .get(provider_id)
            .ok_or_else(|| format!("{provider_id} catalog worker is unavailable"))?;
        match command {
            ProviderCatalogCommand::Search(search) => match &record.catalog {
                ProviderCatalogDispatch::Rithmic { controls, .. } => try_send_rithmic_catalog(
                    controls,
                    RithmicCatalogControl::Search(search),
                    provider_id,
                ),
                ProviderCatalogDispatch::Hyperliquid { controls, .. } => {
                    try_send_hyperliquid_catalog(
                        controls,
                        HyperliquidCatalogControl::Search(search),
                        provider_id,
                    )
                }
                ProviderCatalogDispatch::Disabled => {
                    Err(format!("{provider_id} catalog worker is unavailable"))
                }
            },
            ProviderCatalogCommand::Select(selection) => match &record.catalog {
                ProviderCatalogDispatch::Rithmic { controls, .. } => try_send_rithmic_catalog(
                    controls,
                    RithmicCatalogControl::Select(selection),
                    provider_id,
                ),
                ProviderCatalogDispatch::Hyperliquid { controls, .. } => {
                    try_send_hyperliquid_catalog(
                        controls,
                        HyperliquidCatalogControl::Select(selection),
                        provider_id,
                    )
                }
                ProviderCatalogDispatch::Disabled => {
                    Err(format!("{provider_id} catalog worker is unavailable"))
                }
            },
        }
    }

    pub(super) fn send_rithmic_realtime(
        &self,
        control: RithmicRealtimeControl,
    ) -> Result<bool, String> {
        let Some(controls) = self
            .records
            .get("rithmic")
            .ok_or_else(|| "Rithmic live worker is unavailable".to_string())?
            .realtime
            .rithmic_controls()
        else {
            return Ok(false);
        };
        match controls.try_send(control) {
            Ok(()) => Ok(true),
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => {
                Err("Rithmic live worker is unavailable".to_string())
            }
        }
    }

    pub(super) fn send_hyperliquid_realtime(
        &self,
        control: HyperliquidRealtimeControl,
    ) -> Result<bool, String> {
        let Some(controls) = self
            .records
            .get("hyperliquid")
            .ok_or_else(|| "Hyperliquid live worker is unavailable".to_string())?
            .realtime
            .hyperliquid_controls()
        else {
            return Ok(false);
        };
        match controls.try_send(control) {
            Ok(()) => Ok(true),
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => {
                Err("Hyperliquid live worker is unavailable".to_string())
            }
        }
    }

    pub(super) fn send_hyperliquid_display_depth(
        &self,
        control: HyperliquidDisplayDepthControl,
    ) -> Result<bool, String> {
        let Some(controls) = self
            .records
            .get("hyperliquid")
            .ok_or_else(|| "Hyperliquid display-depth worker is unavailable".to_string())?
            .realtime
            .hyperliquid_display_controls()
        else {
            return Ok(false);
        };
        match controls.try_send(control) {
            Ok(()) => Ok(true),
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => {
                Err("Hyperliquid display-depth worker is unavailable".to_string())
            }
        }
    }

    pub(super) fn stop(&self, provider_id: &str) -> Result<bool, String> {
        let Some(record) = self.records.get(provider_id) else {
            return Ok(true);
        };
        match &record.realtime {
            ProviderRealtimeDispatch::Rithmic { controls, .. } => {
                match controls.try_send(RithmicRealtimeControl::Stop) {
                    Ok(()) => Ok(true),
                    Err(TrySendError::Full(_)) => Ok(false),
                    Err(TrySendError::Disconnected(_)) => {
                        Err("Rithmic live worker is unavailable".to_string())
                    }
                }
            }
            ProviderRealtimeDispatch::Hyperliquid {
                controls,
                display_controls,
                ..
            } => {
                let raw_stopped = match controls.try_send(HyperliquidRealtimeControl::Stop) {
                    Err(TrySendError::Full(_)) => false,
                    Ok(()) | Err(TrySendError::Disconnected(_)) => true,
                };
                let display_stopped =
                    match display_controls.try_send(HyperliquidDisplayDepthControl::Stop) {
                        Err(TrySendError::Full(_)) => false,
                        Ok(()) | Err(TrySendError::Disconnected(_)) => true,
                    };
                Ok(raw_stopped && display_stopped)
            }
            ProviderRealtimeDispatch::Disabled => Ok(true),
        }
    }
}

impl MarketService {
    /// Starts the desktop-owned in-process market runtime.
    ///
    /// Market state is intentionally ephemeral. History is requested from the
    /// provider when demanded; no local market-history store or persisted hot set
    /// participates in startup.
    ///
    /// # Errors
    ///
    /// Returns an error when the market engine or one of the bounded provider
    /// runtime workers cannot be initialized.
    pub fn start() -> Result<Self, String> {
        Self::start_composed(vec![
            ProviderRuntimeSpec::rithmic(Box::new(LiveRithmicHistory), true),
            ProviderRuntimeSpec::hyperliquid(Box::<LiveHyperliquidHistory>::default(), true),
        ])
    }

    pub(super) fn start_composed(providers: Vec<ProviderRuntimeSpec>) -> Result<Self, String> {
        let engine = configured_engine()?;
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let shutdown = Arc::new(AtomicBool::new(false));
        let active_provider_workers = Arc::new(Mutex::new(BTreeSet::new()));
        let provider_registry = ProviderRuntimeRegistry::start(
            providers,
            &command_tx,
            &engine,
            &active_provider_workers,
        )?;
        let workers = vec![spawn_coordinator(
            engine,
            OwnedCoordinatorChannels {
                commands: command_rx,
                providers: provider_registry,
            },
            Arc::clone(&shutdown),
        )?];
        let service =
            Self::build_market_service(command_tx, shutdown, active_provider_workers, workers);
        Ok(service)
    }

    fn build_market_service(
        commands: SyncSender<Command>,
        shutdown: Arc<AtomicBool>,
        active_provider_workers: Arc<Mutex<BTreeSet<String>>>,
        workers: Vec<thread::JoinHandle<()>>,
    ) -> Self {
        Self {
            commands,
            runtime: Arc::new(MarketRuntime {
                shutdown,
                active_provider_workers,
                workers: Mutex::new(Some(workers)),
            }),
        }
    }

    /// Cancels provider work, drains accepted provider events, and joins owned workers.
    ///
    /// # Errors
    /// Returns an error when a worker panics or the complete shutdown exceeds `timeout`.
    pub fn shutdown(&self, timeout: Duration) -> Result<(), String> {
        self.runtime.shutdown.store(true, Ordering::Release);
        let mut workers = self
            .runtime
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| "market engine shutdown is already in progress".to_string())?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| "market engine shutdown deadline overflowed".to_string())?;
        let mut panicked = Vec::new();
        loop {
            let mut index = 0;
            while index < workers.len() {
                if workers[index].is_finished() {
                    let worker = workers.swap_remove(index);
                    let name = worker.thread().name().unwrap_or("unnamed").to_string();
                    if worker.join().is_err() {
                        panicked.push(name);
                    }
                } else {
                    index += 1;
                }
            }
            if workers.is_empty() {
                return if panicked.is_empty() {
                    Ok(())
                } else {
                    Err(format!(
                        "market engine workers panicked during shutdown: {}",
                        panicked.join(", ")
                    ))
                };
            }
            let now = Instant::now();
            if now >= deadline {
                let mut pending = workers
                    .iter()
                    .map(|worker| worker.thread().name().unwrap_or("unnamed"))
                    .map(str::to_string)
                    .collect::<BTreeSet<_>>();
                pending.extend(
                    self.runtime
                        .active_provider_workers
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .iter()
                        .cloned(),
                );
                let pending = pending.into_iter().collect::<Vec<_>>().join(", ");
                return Err(format!(
                    "market engine shutdown deadline expired with active workers: {pending}"
                ));
            }
            thread::sleep(Duration::from_millis(5).min(deadline.duration_since(now)));
        }
    }

    /// Attaches a client identity to in-process market state.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn attach(&self, client_id: u64) -> Result<(), String> {
        self.request(|reply| Ok(Command::Attach(id(client_id).map(ClientId)?, reply)))
    }

    /// Detaches a client and all of its consumers.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn detach(&self, client_id: u64) -> Result<(), String> {
        self.request(|reply| Ok(Command::Detach(id(client_id).map(ClientId)?, reply)))
    }

    /// Returns one bounded coordinator-owned lifecycle and memory snapshot.
    ///
    /// # Errors
    /// Returns an error when the coordinator is unavailable.
    pub fn status(&self) -> Result<MarketServiceStatus, String> {
        self.request(|reply| Ok(Command::Status(reply)))
    }

    /// Registers one market consumer owned by an attached client.
    ///
    /// # Errors
    /// Returns an error for invalid identity, ownership, bounds, or coordinator failure.
    pub fn register_consumer(
        &self,
        client_id: u64,
        workspace_id: u64,
        consumer_id: u64,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Register(
                ConsumerIdentity {
                    client_id: ClientId(id(client_id)?),
                    workspace_id: WorkspaceId(id(workspace_id)?),
                    consumer_id: ConsumerId(id(consumer_id)?),
                },
                reply,
            ))
        })
    }

    /// Removes one market consumer.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn remove_consumer(&self, client_id: u64, consumer_id: u64) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Remove(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                reply,
            ))
        })
    }

    /// Applies a generation-fenced viewport to one consumer.
    ///
    /// # Errors
    /// Returns an error for invalid identity, range, generation, or coordinator failure.
    pub fn set_viewport(
        &self,
        client_id: u64,
        consumer_id: u64,
        generation: u64,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Viewport(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                GenerationId(id(generation)?),
                Viewport::try_new(start_unix_nanos, end_unix_nanos)
                    .map_err(|error| error.to_string())?,
                reply,
            ))
        })
    }

    /// Updates one consumer's presentation priority.
    ///
    /// # Errors
    /// Returns an error for invalid identity, missing consumer, or coordinator failure.
    pub fn set_visibility(
        &self,
        client_id: u64,
        consumer_id: u64,
        visible: bool,
    ) -> Result<(), String> {
        self.set_resource_class(
            client_id,
            consumer_id,
            if visible {
                ConsumerResourceClass::Foreground
            } else {
                ConsumerResourceClass::Background
            },
        )
    }

    /// Updates one consumer's exact resource class.
    ///
    /// # Errors
    /// Returns an error for invalid identity, missing consumer, or coordinator failure.
    pub fn set_resource_class(
        &self,
        client_id: u64,
        consumer_id: u64,
        resource_class: ConsumerResourceClass,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::ResourceClass(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                resource_class,
                reply,
            ))
        })
    }

    /// Replaces one consumer's exact upstream stream set without changing its
    /// selected market generation or restarting history.
    ///
    /// # Errors
    /// Returns an error for invalid identity, stale selection, unsupported
    /// streams, or coordinator failure.
    pub fn set_streams(
        &self,
        client_id: u64,
        consumer_id: u64,
        generation: u64,
        streams: StreamRequirements,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Streams(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                GenerationId(id(generation)?),
                streams,
                reply,
            ))
        })
    }

    /// Accepts one generation-fenced series demand without waiting for provider I/O.
    ///
    /// # Errors
    /// Returns an error for invalid demand or an unavailable coordinator.
    pub fn set_demand(
        &self,
        client_id: u64,
        consumer_id: u64,
        generation: u64,
        series: &BarSeriesKey,
        streams: StreamRequirements,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Demand(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                GenerationId(id(generation)?),
                series.clone(),
                streams,
                reply,
            ))
        })
    }

    /// Registers one native study instance anchored to an owned market consumer.
    ///
    /// Study dependencies may request several canonical series, but those data
    /// requirements are reconciled through runtime-owned engine leases rather
    /// than additional presentation consumers.
    ///
    /// # Errors
    /// Returns an error for invalid identity, ownership, study definition,
    /// provider capability, bounded capacity, or coordinator failure.
    pub fn register_study(
        &self,
        client_id: u64,
        consumer_id: u64,
        registration: NativeStudyRegistration,
    ) -> Result<StudyInstanceId, String> {
        self.request(|reply| {
            Ok(Command::RegisterStudy(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                registration,
                reply,
            ))
        })
    }

    /// Reinitializes one owned study in place after a declared input or setting
    /// change while preserving its stable instance/output identities.
    ///
    /// # Errors
    /// Returns an error for ownership, definition, provider-demand, execution,
    /// or coordinator failure. Runtime state rolls back when preparation fails.
    pub fn reinitialize_study(
        &self,
        client_id: u64,
        study_id: StudyInstanceId,
        registration: NativeStudyRegistration,
    ) -> Result<Vec<StudyInstanceId>, String> {
        self.request(|reply| {
            Ok(Command::ReinitializeStudy(
                ClientId(id(client_id)?),
                study_id,
                registration,
                reply,
            ))
        })
    }

    /// Removes one owned study and every downstream study that depends on it.
    ///
    /// Released market dependencies are removed from the same authoritative
    /// provider subscription set used by charts.
    ///
    /// # Errors
    /// Returns an error for invalid identity, ownership, unknown study, or
    /// coordinator failure.
    pub fn remove_study(
        &self,
        client_id: u64,
        study_id: StudyInstanceId,
    ) -> Result<Vec<StudyInstanceId>, String> {
        self.request(|reply| {
            Ok(Command::RemoveStudy(
                ClientId(id(client_id)?),
                study_id,
                reply,
            ))
        })
    }

    /// Schedules one bounded exact provider-instrument search for an owned consumer.
    ///
    /// # Errors
    /// Returns an error for invalid identity, unsupported input, ownership, or coordinator failure.
    pub fn search_provider_instruments(
        &self,
        client_id: u64,
        search: SearchProviderInstruments,
    ) -> Result<(), String> {
        validate_provider_search(&search)?;
        self.request(|reply| {
            Ok(Command::SearchProviderInstruments(
                ClientId(id(client_id)?),
                search,
                reply,
            ))
        })
    }

    /// Schedules one exact provider-instrument selection for an owned consumer.
    ///
    /// # Errors
    /// Returns an error for invalid identity, unsupported input, ownership, or coordinator failure.
    pub fn select_provider_instrument(
        &self,
        client_id: u64,
        selection: SelectProviderInstrument,
    ) -> Result<(), String> {
        validate_provider_selection(&selection)?;
        self.request(|reply| {
            Ok(Command::SelectProviderInstrument(
                ClientId(id(client_id)?),
                selection,
                reply,
            ))
        })
    }

    /// Installs one bounded adapter-resolved instrument in the runtime catalog.
    ///
    /// # Errors
    /// Returns an error for invalid identity, stale generations, capacity, or coordinator failure.
    pub fn install_provider_instrument(
        &self,
        instrument: &InstallProviderInstrument,
    ) -> Result<(), String> {
        validate_provider_instrument(instrument)?;
        self.request(|reply| {
            Ok(Command::InstallProviderInstrument(
                instrument.clone(),
                reply,
            ))
        })
    }

    /// Transactionally replaces one consumer's bounded price-alert set.
    ///
    /// Alert conditions are evaluated by the coordinator against accepted
    /// trade prices. Provider demand is shared with ordinary chart demand.
    ///
    /// # Errors
    /// Returns an error for invalid ownership, instrument metadata, capacity,
    /// or an unavailable coordinator.
    pub fn replace_price_alerts(
        &self,
        client_id: u64,
        consumer_id: u64,
        alerts: &[crate::MarketPriceAlert],
    ) -> Result<(), String> {
        for alert in alerts {
            validate_provider_instrument(&alert.instrument)?;
        }
        self.request(|reply| {
            Ok(Command::ReplacePriceAlerts(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                alerts.to_vec(),
                reply,
            ))
        })
    }

    /// Drains at most one bounded market publication for an owned consumer.
    ///
    /// # Errors
    /// Returns an error for invalid ownership or coordinator failure.
    pub fn poll_event(
        &self,
        client_id: u64,
        consumer_id: u64,
    ) -> Result<Option<MarketRuntimeEvent>, String> {
        self.request(|reply| {
            Ok(Command::Poll(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                reply,
            ))
        })
    }

    /// Drains one bounded fair batch directly from this client's canonical
    /// consumer outboxes. No intermediate desktop event queue is created.
    ///
    /// # Errors
    /// Returns an error for an unattached client or coordinator failure.
    pub fn poll_events(
        &self,
        client_id: u64,
        consumer_budgets: &[(u64, usize)],
    ) -> Result<Vec<(u64, MarketRuntimeEvent)>, String> {
        let consumer_budgets = consumer_budgets
            .iter()
            .map(|(consumer_id, maximum)| Ok((ConsumerId(id(*consumer_id)?), *maximum)))
            .collect::<Result<Vec<_>, String>>()?;
        self.request(|reply| {
            Ok(Command::PollClient(
                ClientId(id(client_id)?),
                consumer_budgets,
                reply,
            ))
        })
    }

    fn request<T>(
        &self,
        build: impl FnOnce(Reply<T>) -> Result<Command, String>,
    ) -> Result<T, String> {
        self.request_with_timeout(build, Duration::from_secs(5))
    }

    fn request_with_timeout<T>(
        &self,
        build: impl FnOnce(Reply<T>) -> Result<Command, String>,
        timeout: Duration,
    ) -> Result<T, String> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let state = Arc::new(super::RequestState(std::sync::atomic::AtomicU8::new(0)));
        let command = Command::Request(Box::new(build(reply_tx)?), Arc::clone(&state));
        let deadline = Instant::now() + timeout;
        let mut command = command;
        loop {
            match self.commands.try_send(command) {
                Ok(()) => break,
                Err(TrySendError::Disconnected(_)) => {
                    return Err("market engine coordinator is unavailable".to_string());
                }
                Err(TrySendError::Full(returned)) => {
                    if Instant::now() >= deadline {
                        return Err(
                            "market coordinator is busy; request was not submitted".to_string()
                        );
                    }
                    command = returned;
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
        match reply_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("market engine coordinator stopped before replying".to_string())
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if state.cancel() {
                    Err("market coordinator deadline expired; request was cancelled before execution".to_string())
                } else {
                    // Never automatically retry a mutation whose execution has started.
                    Err("market coordinator deadline expired after execution started; outcome is unknown".to_string())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::study::{
        NativeStudyProgram, NativeStudyRegistration, StudyDefinition, StudyDependency,
        StudyExecutionContext, StudyInvalidationPolicy, StudyMarketInput, StudyOutputSpec,
        StudyPaneTarget, StudyPlotKind, StudyPointStyle, StudyScaleTarget, StudySettings,
    };
    use asceify_market_data::{BarPeriod, MarketBar};

    struct RecordingStudyHistory {
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl HistorySource for RecordingStudyHistory {
        fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(request.series.instrument_id.clone());
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 0,
                bars: vec![
                    MarketBar {
                        source_sequence: 1,
                        exchange_timestamp_seconds: 1_700_000_000,
                        exchange_timestamp_unix_nanos: 1_700_000_000_000_000_000,
                        open: 10_000,
                        high: 10_200,
                        low: 9_900,
                        close: 10_100,
                        volume: 100,
                    },
                    MarketBar {
                        source_sequence: 2,
                        exchange_timestamp_seconds: 1_700_000_060,
                        exchange_timestamp_unix_nanos: 1_700_000_060_000_000_000,
                        open: 10_100,
                        high: 10_300,
                        low: 10_000,
                        close: 10_200,
                        volume: 120,
                    },
                ],
                forming: None,
                handoff_boundary_unix_nanos: Some(1_700_000_060_000_000_000),
            })
        }
    }

    fn study_instrument() -> InstallProviderInstrument {
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

    fn study_series() -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            period: BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        }
    }

    fn study_definition(series: BarSeriesKey) -> StudyDefinition {
        StudyDefinition {
            identifier: "runtime.history.study".to_string(),
            dependencies: vec![StudyDependency::Market(StudyMarketInput {
                series,
                streams: StreamRequirements::BARS,
            })],
            settings: Vec::new(),
            outputs: vec![StudyOutputSpec {
                identifier: "value".to_string(),
                title: "Runtime History Study".to_string(),
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

    fn calculate_test_study(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let output = context
            .output(0)
            .ok_or_else(|| "test study output is unavailable".to_string())?;
        if !output.is_empty() {
            output.set(0, None)?;
        }
        Ok(())
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

    #[test]
    fn queued_request_deadline_cancels_execution_without_waiting_for_coordinator() {
        let (commands, receiver) = mpsc::sync_channel(1);
        let service = MarketService {
            commands,
            runtime: Arc::new(MarketRuntime {
                shutdown: Arc::new(AtomicBool::new(false)),
                active_provider_workers: Arc::new(Mutex::new(BTreeSet::new())),
                workers: Mutex::new(None),
            }),
        };
        let result =
            service.request_with_timeout(|reply| Ok(Command::Status(reply)), Duration::ZERO);
        assert!(result.unwrap_err().contains("cancelled before execution"));
        let Command::Request(_, state) = receiver.try_recv().unwrap() else {
            panic!("request envelope");
        };
        assert!(!state.start());
    }

    #[test]
    fn uncached_study_dependency_uses_existing_bounded_history_worker() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service = MarketService::start_composed(vec![ProviderRuntimeSpec::rithmic(
            Box::new(RecordingStudyHistory {
                calls: Arc::clone(&calls),
            }),
            false,
        )])
        .expect("test runtime starts");
        service.attach(1).expect("client attaches");
        service
            .register_consumer(1, 1, 1)
            .expect("consumer registers");
        service
            .install_provider_instrument(&study_instrument())
            .expect("instrument installs");

        let study_id = service
            .register_study(1, 1, study_registration(study_series()))
            .expect("study registers without waiting for provider history");

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let called = !calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty();
            let status = service.status().expect("runtime status");
            if called && status.retained_bars >= 2 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "study dependency did not complete through provider history worker"
            );
            thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(
            *calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["instrument:rithmic:CME:MNQU6".to_string()]
        );
        assert_eq!(
            service.remove_study(1, study_id).expect("study removes"),
            vec![study_id]
        );
        service.remove_consumer(1, 1).expect("consumer removes");
        service.detach(1).expect("client detaches");
        service
            .shutdown(Duration::from_secs(2))
            .expect("test runtime shuts down");
    }
}
