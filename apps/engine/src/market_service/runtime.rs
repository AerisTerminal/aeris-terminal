use super::{
    ActiveWorkerGuard, Arc, AtomicBool, AtomicU64, BTreeMap, BTreeSet, COMMAND_CAPACITY, ClientId,
    Command, ConsumerId, ConsumerIdentity, ConsumerResourceClass, Duration, GenerationId,
    HISTORY_CAPACITY, HistoryRequest, HistorySnapshot, HistorySource, HotSeries,
    InstallProviderInstrument, Instant, LiveHyperliquidHistory, LiveRithmicHistory,
    LocalHistoryStore, MarketEngine, MarketRuntime, MarketService, MarketServiceStatus, Mutex,
    Ordering, OwnedCoordinatorChannels, ProviderCatalogChannelSet, ProviderCatalogChannels,
    ProviderCatalogCommand, ProviderCatalogDispatch, ProviderDispatch, ProviderDispatchRecord,
    ProviderRealtimeChannelSet, ProviderRealtimeChannels, ProviderRealtimeDispatch,
    ProviderRuntimeEvent, ProviderRuntimeLifecycle, ProviderRuntimeRecord, ProviderRuntimeRegistry,
    ProviderRuntimeSpec, REALTIME_CAPACITY, RITHMIC_REALTIME_CONTROL_CAPACITY, Reply, ResourceMode,
    RithmicCatalogControl, RithmicRealtimeControl, RithmicRealtimeEvent, STORAGE_CAPACITY,
    SearchProviderInstruments, SelectProviderInstrument, SeriesKey, StartedProviderRuntime,
    SyncSender, TrySendError, Viewport, WorkspaceId, WorkspaceState, available_memory_bytes,
    configured_engine, configured_reconnect_delay, envelope, id, internal_series, mpsc,
    retained_hot_series, spawn_coordinator, spawn_history_worker, spawn_storage_worker, thread,
    try_send_hyperliquid_catalog, try_send_rithmic_catalog, validate_provider_instrument,
    validate_provider_search, validate_provider_selection, warm_series,
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
        request.maximum_bars,
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
            &request.series,
            request.provider_generation.0.get(),
            installed,
            request.maximum_bars,
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
        let mut registry = Self {
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
            let record = match Self::start_record(spec, completions, engine, active_workers) {
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
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
    ) -> Result<ProviderRuntimeRecord, String> {
        let cancellation = Arc::new(AtomicBool::new(false));
        let lifecycle = Arc::new(ProviderRuntimeLifecycle::default());
        let (history_tx, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
        let history_name: &'static str = if spec.provider_id == "hyperliquid" {
            "axiusflow-hyperliquid-history"
        } else {
            "axiusflow-rithmic-history"
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
                engine,
                active_workers,
            ),
            "rithmic" => {
                Self::start_rithmic_record(started, spec.realtime.enabled, engine, active_workers)
            }
            provider => {
                started.cancel_and_join();
                Err(format!(
                    "resident engine market provider is unsupported: {provider}"
                ))
            }
        }
    }

    fn start_rithmic_record(
        mut started: StartedProviderRuntime,
        enabled: bool,
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
                .name("axiusflow-rithmic-provider".to_string())
                .spawn(move || {
                    let _activity =
                        ActiveWorkerGuard::register("axiusflow-rithmic-provider", worker_activity);
                    crate::rithmic_realtime::run(
                        &catalog_controls_rx,
                        &catalog_events_tx,
                        &realtime_controls_rx,
                        &realtime_events_tx,
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
        let (catalog_controls, catalog_controls_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (realtime_events_tx, realtime_events) = mpsc::sync_channel(REALTIME_CAPACITY);
        let (realtime_controls, realtime_controls_rx) =
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
        events: mpsc::SyncSender<crate::hyperliquid_realtime::HyperliquidCatalogEvent>,
        ws_generation: Arc<AtomicU64>,
    ) -> Result<(), String> {
        let cancellation = Arc::clone(&started.cancellation);
        let lifecycle = Arc::clone(&started.lifecycle);
        let activity = Arc::clone(active_workers);
        let worker = thread::Builder::new()
            .name("axiusflow-hyperliquid-catalog".to_string())
            .spawn(move || {
                let _activity =
                    ActiveWorkerGuard::register("axiusflow-hyperliquid-catalog", activity);
                crate::hyperliquid_realtime::run_catalog(
                    &controls,
                    &events,
                    &ws_generation,
                    &cancellation,
                    axiusflow_hyperliquid_market_adapter::HyperliquidHttpConfig::default(),
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
        reconnect_delay: Duration,
    ) -> Result<(), String> {
        let cancellation = Arc::clone(&started.cancellation);
        let lifecycle = Arc::clone(&started.lifecycle);
        let activity = Arc::clone(active_workers);
        let worker = thread::Builder::new()
            .name("axiusflow-hyperliquid-provider".to_string())
            .spawn(move || {
                let _activity =
                    ActiveWorkerGuard::register("axiusflow-hyperliquid-provider", activity);
                crate::hyperliquid_realtime::run(
                    &controls,
                    &events,
                    &cancellation,
                    &ws_generation,
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
                    ProviderRealtimeChannelSet::Hyperliquid { controls, events }
                        if record.realtime.enabled =>
                    {
                        ProviderRealtimeDispatch::Hyperliquid { controls, events }
                    }
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
        ProviderDispatch { records }
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
            _ => None,
        }?;
        match &event {
            ProviderRuntimeEvent::RithmicRealtime(event) => {
                let (generation, reconnecting) = match event {
                    RithmicRealtimeEvent::Connecting(generation)
                    | RithmicRealtimeEvent::Recovering(generation)
                    | RithmicRealtimeEvent::Disconnected(generation) => (*generation, true),
                    RithmicRealtimeEvent::Connected(generation)
                    | RithmicRealtimeEvent::Heartbeat(generation)
                    | RithmicRealtimeEvent::Trade(generation, _)
                    | RithmicRealtimeEvent::Depth(generation, _) => (*generation, false),
                };
                self.observe_generation("rithmic", generation, reconnecting);
            }
            ProviderRuntimeEvent::HyperliquidRealtime(event) => {
                let (generation, reconnecting) = match event {
                    HyperliquidRealtimeEvent::Connecting(generation)
                    | HyperliquidRealtimeEvent::Recovering(generation)
                    | HyperliquidRealtimeEvent::Disconnected(generation) => (*generation, true),
                    HyperliquidRealtimeEvent::Connected(generation)
                    | HyperliquidRealtimeEvent::Heartbeat(generation)
                    | HyperliquidRealtimeEvent::Candle(generation, ..)
                    | HyperliquidRealtimeEvent::Trades(generation, _)
                    | HyperliquidRealtimeEvent::Depth(generation, _) => (*generation, false),
                };
                self.observe_generation("hyperliquid", generation, reconnecting);
            }
            ProviderRuntimeEvent::RithmicCatalog(_)
            | ProviderRuntimeEvent::HyperliquidCatalog(_) => {}
        }
        Some(event)
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

    pub(super) fn stop(&self, provider_id: &str) {
        let Some(record) = self.records.get(provider_id) else {
            return;
        };
        match &record.realtime {
            ProviderRealtimeDispatch::Rithmic { controls, .. } => {
                let _ = controls.try_send(RithmicRealtimeControl::Stop);
            }
            ProviderRealtimeDispatch::Hyperliquid { controls, .. } => {
                let _ = controls.try_send(HyperliquidRealtimeControl::Stop);
            }
            ProviderRealtimeDispatch::Disabled => {}
        }
    }
}

impl MarketService {
    /// Starts the process-owned market coordinator and its bounded provider-history worker.
    ///
    /// # Errors
    /// Returns an error when provider configuration or either bounded worker cannot start.
    pub fn start(workspace: &WorkspaceState) -> Result<Self, String> {
        let hot_series = retained_hot_series(workspace, available_memory_bytes())?;
        let storage = LocalHistoryStore::open(
            &crate::default_engine_state_root()?
                .join("market-history")
                .join("rithmic"),
        )
        .map_err(|error| error.to_string());
        let service = Self::start_composed(
            vec![
                ProviderRuntimeSpec::rithmic(Box::new(LiveRithmicHistory), true),
                ProviderRuntimeSpec::hyperliquid(Box::new(LiveHyperliquidHistory), true),
            ],
            Some(storage),
            hot_series.len(),
        )?;
        service.restore_hot_set(&hot_series)?;
        Ok(service)
    }

    pub(super) fn start_composed(
        providers: Vec<ProviderRuntimeSpec>,
        storage: Option<Result<LocalHistoryStore, String>>,
        hot_set_priority_count: usize,
    ) -> Result<Self, String> {
        let engine = configured_engine()?;
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (storage_tx, storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
        let shutdown = Arc::new(AtomicBool::new(false));
        let active_provider_workers = Arc::new(Mutex::new(BTreeSet::new()));
        let available_memory_bytes = available_memory_bytes();
        let provider_registry = ProviderRuntimeRegistry::start(
            providers,
            &command_tx,
            &engine,
            &active_provider_workers,
        )?;
        let workers = vec![
            spawn_storage_worker(
                storage,
                storage_rx,
                command_tx.clone(),
                Arc::clone(&shutdown),
            )?,
            spawn_coordinator(
                engine,
                OwnedCoordinatorChannels {
                    commands: command_rx,
                    storage: storage_tx,
                    providers: provider_registry,
                },
                Arc::clone(&shutdown),
                available_memory_bytes,
                hot_set_priority_count,
            )?,
        ];
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

    /// Cancels provider work, drains accepted persistence, and joins owned workers.
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

    /// Attaches a client identity to resident market state.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn attach(&self, client_id: u64) -> Result<(), String> {
        self.request(|reply| Ok(Command::Attach(id(client_id).map(ClientId)?, None, reply)))
    }

    pub(crate) fn attach_stream(
        &self,
        client_id: u64,
        events: SyncSender<(u64, envelope::Payload)>,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Attach(
                id(client_id).map(ClientId)?,
                Some(events),
                reply,
            ))
        })
    }

    /// Detaches a client and all of its consumers.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn detach(&self, client_id: u64) -> Result<(), String> {
        self.request(|reply| Ok(Command::Detach(id(client_id).map(ClientId)?, reply)))
    }

    /// Applies the engine-owned background market retention policy.
    ///
    /// # Errors
    /// Returns an error when the coordinator is unavailable.
    pub fn set_resource_mode(&self, mode: ResourceMode) -> Result<(), String> {
        self.request(|reply| Ok(Command::SetResourceMode(mode, reply)))
    }

    fn restore_hot_set(&self, hot_series: &[HotSeries]) -> Result<(), String> {
        let restored = hot_series
            .iter()
            .filter_map(|series| {
                if let Ok(series) = warm_series(series) {
                    Some(series)
                } else {
                    eprintln!("Axiusflow engine skipped unsupported hot-set metadata");
                    None
                }
            })
            .collect::<Vec<_>>();
        self.request(|reply| Ok(Command::RestoreHotSet(restored, reply)))
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

    /// Accepts one generation-fenced series demand without waiting for provider I/O.
    ///
    /// # Errors
    /// Returns an error for invalid demand or an unavailable coordinator.
    pub fn set_demand(
        &self,
        client_id: u64,
        consumer_id: u64,
        generation: u64,
        series: &SeriesKey,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Demand(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                GenerationId(id(generation)?),
                internal_series(series)?,
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

    /// Installs one bounded adapter-resolved instrument in the engine-owned catalog.
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

    /// Drains at most one bounded market publication for an owned consumer.
    ///
    /// # Errors
    /// Returns an error for invalid ownership or coordinator failure.
    pub fn poll_event(
        &self,
        client_id: u64,
        consumer_id: u64,
    ) -> Result<Option<envelope::Payload>, String> {
        self.request(|reply| {
            Ok(Command::Poll(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                reply,
            ))
        })
    }

    fn request<T>(
        &self,
        build: impl FnOnce(Reply<T>) -> Result<Command, String>,
    ) -> Result<T, String> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let command = build(reply_tx)?;
        self.commands
            .send(command)
            .map_err(|_| "market engine coordinator is unavailable".to_string())?;
        reply_rx
            .recv()
            .map_err(|_| "market engine coordinator stopped before replying".to_string())?
    }
}
