use super::{
    ActiveWorkerGuard, Arc, AtomicBool, AtomicU64, BTreeMap, BTreeSet, BarSeriesKey,
    COMMAND_CAPACITY, ClientId, Command, ConsumerId, ConsumerIdentity, ConsumerResourceClass,
    Duration, GenerationId, HISTORY_CAPACITY, HistoryRequest, HistorySnapshot, HistorySource,
    HyperliquidChannels, InstallProviderInstrument, Instant, LiveHyperliquidHistory,
    LiveRithmicHistory, MarketEngine, MarketRuntime, MarketService, MarketServiceStatus, Mutex,
    NativeStudyRegistration, Ordering, OwnedCoordinatorChannels, ProviderCatalogChannelImpl,
    ProviderCatalogChannels, ProviderCatalogCommand, ProviderCoordinatorWake, ProviderDispatch,
    ProviderDispatchRecord, ProviderEventLane, ProviderRuntimeEvent, ProviderRuntimeLifecycle,
    ProviderRuntimeRecord, ProviderRuntimeRegistry, ProviderRuntimeSpec, REALTIME_CAPACITY,
    RITHMIC_REALTIME_CONTROL_CAPACITY, Reply, RithmicCatalogControl, ScreenProviderMarkets,
    SearchProviderInstruments, SelectProviderInstrument, StartedProviderRuntime,
    StreamRequirements, StudyInstanceId, SyncSender, TrySendError, Viewport, WorkspaceId,
    configured_reconnect_delay, id, mpsc, spawn_coordinator, spawn_history_worker, thread,
    try_send_hyperliquid_catalog, try_send_rithmic_catalog, validate_provider_instrument,
    validate_provider_screen, validate_provider_search, validate_provider_selection,
};
use crate::MarketRuntimeEvent;
use crate::hyperliquid_display_depth::{
    HyperliquidDisplayDepthControl, HyperliquidDisplayDepthEvent,
};
use crate::hyperliquid_realtime::HyperliquidCatalogControl;
use crate::rithmic_realtime::RithmicLoginRevision;
use aeris_contracts::ProviderPresentationDescriptor;

fn dispatch_rithmic_catalog(
    controls: &SyncSender<RithmicCatalogControl>,
    provider: &str,
    command: ProviderCatalogCommand,
) -> Result<(), String> {
    let control = match command {
        ProviderCatalogCommand::Search(value) => RithmicCatalogControl::Search(value),
        ProviderCatalogCommand::Select(value) => RithmicCatalogControl::Select(value),
        ProviderCatalogCommand::Screen(_) => {
            return Err(format!("{provider} does not provide a market screen"));
        }
    };
    try_send_rithmic_catalog(controls, control, provider)
}

fn dispatch_hyperliquid_catalog(
    controls: &SyncSender<HyperliquidCatalogControl>,
    provider: &str,
    command: ProviderCatalogCommand,
) -> Result<(), String> {
    let control = match command {
        ProviderCatalogCommand::Search(value) => HyperliquidCatalogControl::Search(value),
        ProviderCatalogCommand::Select(value) => HyperliquidCatalogControl::Select(value),
        ProviderCatalogCommand::Screen(value) => HyperliquidCatalogControl::Screen(value),
    };
    try_send_hyperliquid_catalog(controls, control, provider)
}

pub(super) fn dispatch_tastytrade_catalog(
    controls: &SyncSender<super::tastytrade::CatalogControl>,
    provider: &str,
    command: ProviderCatalogCommand,
) -> Result<(), String> {
    let control = match command {
        ProviderCatalogCommand::Search(value) => super::tastytrade::CatalogControl::Search(value),
        ProviderCatalogCommand::Select(value) => super::tastytrade::CatalogControl::Select(value),
        ProviderCatalogCommand::Screen(_) => {
            return Err(format!("{provider} does not provide a market screen"));
        }
    };
    controls
        .try_send(control)
        .map_err(|_| "Provider catalog capacity is exhausted or unavailable".to_string())
}

pub(super) fn dispatch_ctrader_catalog(
    controls: &SyncSender<super::ctrader::CatalogControl>,
    provider: &str,
    command: ProviderCatalogCommand,
) -> Result<(), String> {
    let control = match command {
        ProviderCatalogCommand::Search(value) => super::ctrader::CatalogControl::Search(value),
        ProviderCatalogCommand::Select(value) => super::ctrader::CatalogControl::Select(value),
        ProviderCatalogCommand::Screen(_) => {
            return Err(format!("{provider} does not provide a market screen"));
        }
    };
    controls
        .try_send(control)
        .map_err(|_| "Provider catalog capacity is exhausted or unavailable".to_string())
}

impl HistorySource for LiveRithmicHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        fetch_rithmic_history(request)
    }
}

fn fetch_rithmic_history(request: &HistoryRequest) -> Result<HistorySnapshot, String> {
    let installed = request
        .instrument
        .as_ref()
        .ok_or_else(|| "Provider instrument is not installed".to_string())?;
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
        backwards_exhausted: false,
    })
}

impl HistorySource for LiveHyperliquidHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        let installed = request
            .instrument
            .as_ref()
            .ok_or_else(|| "Provider instrument is not installed".to_string())?;
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
            handoff_boundary_unix_nanos: snapshot.handoff_boundary_unix_nanos,
            backwards_exhausted: false,
        })
    }
}

impl ProviderRuntimeRegistry {
    pub(super) fn start(
        specs: Vec<ProviderRuntimeSpec>,
        completions: &SyncSender<Command>,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
        context: &super::ProviderStartContext,
    ) -> Result<Self, String> {
        let wake = ProviderCoordinatorWake::new(
            completions.clone(),
            specs.iter().map(|spec| spec.descriptor.id),
        );
        let mut registry = Self {
            wake: wake.clone(),
            records: BTreeMap::new(),
            order: Vec::new(),
        };
        for spec in specs {
            if registry.records.contains_key(spec.descriptor.id) {
                registry.cancel_and_join();
                return Err(format!(
                    "provider runtime {} is configured more than once",
                    spec.descriptor.id
                ));
            }
            let provider_id = spec.descriptor.id;
            let record = match Self::start_record(
                spec,
                completions,
                wake.clone(),
                engine,
                active_workers,
                context,
            ) {
                Ok(record) => record,
                Err(error) => {
                    registry.cancel_and_join();
                    return Err(error);
                }
            };
            registry.records.insert(provider_id, record);
            registry.order.push(provider_id);
        }
        Ok(registry)
    }

    fn start_record(
        spec: ProviderRuntimeSpec,
        completions: &SyncSender<Command>,
        wake: ProviderCoordinatorWake,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
        context: &super::ProviderStartContext,
    ) -> Result<ProviderRuntimeRecord, String> {
        (spec.descriptor.start)(spec, completions, wake, engine, active_workers, context)
    }

    fn start_history_runtime(
        spec: ProviderRuntimeSpec,
        completions: &SyncSender<Command>,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
    ) -> Result<StartedProviderRuntime, String> {
        let cancellation = Arc::new(AtomicBool::new(false));
        let lifecycle = Arc::new(ProviderRuntimeLifecycle::default());
        let (history_tx, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
        let super::HistorySourceKind::DedicatedWorker(worker_name) =
            spec.descriptor.capabilities.history.source
        else {
            return Err("Provider history source is not a dedicated worker".to_string());
        };
        let history_worker = spawn_history_worker(
            worker_name,
            spec.history
                .ok_or_else(|| "Provider history source is unavailable".to_string())?,
            history_rx,
            completions.clone(),
            Arc::clone(&cancellation),
            Arc::clone(active_workers),
        )?;
        Ok(StartedProviderRuntime {
            history: history_tx,
            cancellation,
            lifecycle,
            workers: vec![history_worker],
        })
    }

    pub(super) fn start_rithmic_runtime(
        spec: ProviderRuntimeSpec,
        completions: &SyncSender<Command>,
        wake: ProviderCoordinatorWake,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
        context: &super::ProviderStartContext,
    ) -> Result<ProviderRuntimeRecord, String> {
        let enabled = spec.realtime.enabled;
        let descriptor = spec.descriptor;
        let started = Self::start_history_runtime(spec, completions, active_workers)?;
        Self::start_rithmic_record(
            started,
            descriptor,
            enabled,
            context.rithmic_login.clone(),
            wake,
            engine,
            active_workers,
        )
    }

    pub(super) fn start_hyperliquid_runtime(
        spec: ProviderRuntimeSpec,
        completions: &SyncSender<Command>,
        wake: ProviderCoordinatorWake,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
        _context: &super::ProviderStartContext,
    ) -> Result<ProviderRuntimeRecord, String> {
        let enabled = spec.realtime.enabled;
        let descriptor = spec.descriptor;
        let started = Self::start_history_runtime(spec, completions, active_workers)?;
        Self::start_hyperliquid_record(started, descriptor, enabled, wake, engine, active_workers)
    }

    pub(super) fn start_tastytrade_runtime(
        spec: ProviderRuntimeSpec,
        completions: &SyncSender<Command>,
        wake: ProviderCoordinatorWake,
        _engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
        context: &super::ProviderStartContext,
    ) -> Result<ProviderRuntimeRecord, String> {
        let ProviderRuntimeSpec {
            descriptor,
            history,
            ..
        } = spec;
        if descriptor.capabilities.history.source != super::HistorySourceKind::ProviderSession
            || history.is_some()
        {
            return Err("Provider history source must be runtime-owned".to_string());
        }
        super::tastytrade::start_record(
            descriptor,
            completions,
            wake,
            active_workers,
            &context.tastytrade,
        )
    }

    pub(super) fn start_ctrader_runtime(
        spec: ProviderRuntimeSpec,
        completions: &SyncSender<Command>,
        wake: ProviderCoordinatorWake,
        _engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
        context: &super::ProviderStartContext,
    ) -> Result<ProviderRuntimeRecord, String> {
        let ProviderRuntimeSpec {
            descriptor,
            history,
            ..
        } = spec;
        if descriptor.capabilities.history.source != super::HistorySourceKind::ProviderSession
            || history.is_some()
        {
            return Err("Provider history source must be runtime-owned".to_string());
        }
        super::ctrader::start_record(
            descriptor,
            completions,
            wake,
            active_workers,
            &context.ctrader,
        )
    }

    fn start_rithmic_record(
        mut started: StartedProviderRuntime,
        descriptor: super::ProviderDescriptor,
        enabled: bool,
        login: RithmicLoginRevision,
        wake: ProviderCoordinatorWake,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
    ) -> Result<ProviderRuntimeRecord, String> {
        let reconnect_delay = if enabled {
            match configured_reconnect_delay(engine, descriptor.id) {
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
        let catalog_events_tx =
            super::CatalogPublisher::new(catalog_events_tx, descriptor.id, wake.clone());
        let (catalog_controls, catalog_controls_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (realtime_events_tx, realtime_events) = mpsc::sync_channel(REALTIME_CAPACITY);
        let binding = Arc::new(Mutex::new(
            super::provider_session::RithmicBinding::default(),
        ));
        let (realtime_controls, realtime_controls_rx) =
            mpsc::sync_channel(RITHMIC_REALTIME_CONTROL_CAPACITY);
        if enabled {
            let worker_cancellation = Arc::clone(&started.cancellation);
            let worker_lifecycle = Arc::clone(&started.lifecycle);
            let Some(reconnect_delay) = reconnect_delay else {
                started.cancel_and_join();
                return Err("Provider reconnect policy is unavailable".to_string());
            };
            let worker_activity = Arc::clone(active_workers);
            let provider = thread::Builder::new()
                .name(crate::rithmic_realtime::WORKER_NAME.to_string())
                .spawn(move || {
                    let _activity = ActiveWorkerGuard::register(
                        crate::rithmic_realtime::WORKER_NAME,
                        worker_activity,
                    );
                    crate::rithmic_realtime::run(
                        &catalog_controls_rx,
                        &catalog_events_tx,
                        &realtime_controls_rx,
                        &realtime_events_tx,
                        &wake,
                        reconnect_delay,
                        &login,
                    );
                    if !worker_cancellation.load(Ordering::Acquire) {
                        worker_lifecycle
                            .mark_terminal_failure("Provider runtime stopped unexpectedly");
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
            descriptor,
            history: started.history,
            cancellation: started.cancellation,
            lifecycle: started.lifecycle,
            hyperliquid: None,
            ctrader: None,
            tastytrade: None,
            rithmic: Some(super::RithmicChannels {
                controls: realtime_controls,
                events: realtime_events,
                binding,
            }),
            catalog: ProviderCatalogChannels {
                enabled,
                channels: Box::new(ProviderCatalogChannelImpl::new(
                    catalog_controls,
                    catalog_events,
                    dispatch_rithmic_catalog,
                )),
            },
            workers: started.workers,
        })
    }

    fn start_hyperliquid_record(
        mut started: StartedProviderRuntime,
        descriptor: super::ProviderDescriptor,
        enabled: bool,
        wake: ProviderCoordinatorWake,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
    ) -> Result<ProviderRuntimeRecord, String> {
        let reconnect_delay = if enabled {
            match configured_reconnect_delay(engine, descriptor.id) {
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
        let catalog_events_tx =
            super::CatalogPublisher::new(catalog_events_tx, descriptor.id, wake.clone());
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
                return Err("Provider reconnect policy is unavailable".to_string());
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
            descriptor,
            history: started.history,
            cancellation: started.cancellation,
            lifecycle: started.lifecycle,
            hyperliquid: enabled.then_some(HyperliquidChannels {
                controls: realtime_controls,
                events: realtime_events,
                display_controls,
                display_events,
            }),
            ctrader: None,
            tastytrade: None,
            rithmic: None,
            catalog: ProviderCatalogChannels {
                enabled,
                channels: Box::new(ProviderCatalogChannelImpl::new(
                    catalog_controls,
                    catalog_events,
                    dispatch_hyperliquid_catalog,
                )),
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
            .name(crate::hyperliquid_realtime::CATALOG_WORKER_NAME.to_string())
            .spawn(move || {
                let _activity = ActiveWorkerGuard::register(
                    crate::hyperliquid_realtime::CATALOG_WORKER_NAME,
                    activity,
                );
                crate::hyperliquid_realtime::run_catalog(
                    &controls,
                    &events,
                    &ws_generation,
                    &cancellation,
                    aeris_hyperliquid_market_adapter::HyperliquidHttpConfig::default(),
                );
                if !cancellation.load(Ordering::Acquire) {
                    lifecycle
                        .mark_terminal_failure("Provider catalog runtime stopped unexpectedly");
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
        events: mpsc::SyncSender<
            aeris_market_engine::fence::Fenced<
                aeris_market_engine::fence::ProviderTag,
                crate::hyperliquid_realtime::HyperliquidRealtimeEvent,
            >,
        >,
        ws_generation: Arc<AtomicU64>,
        wake: ProviderCoordinatorWake,
    ) -> Result<(), String> {
        let cancellation = Arc::clone(&started.cancellation);
        let lifecycle = Arc::clone(&started.lifecycle);
        let activity = Arc::clone(active_workers);
        let worker = thread::Builder::new()
            .name(crate::hyperliquid_realtime::PROVIDER_WORKER_NAME.to_string())
            .spawn(move || {
                let _activity = ActiveWorkerGuard::register(
                    crate::hyperliquid_realtime::PROVIDER_WORKER_NAME,
                    activity,
                );
                crate::hyperliquid_realtime::run(
                    &controls,
                    &events,
                    &cancellation,
                    &ws_generation,
                    &wake,
                );
                if !cancellation.load(Ordering::Acquire) {
                    lifecycle.mark_terminal_failure("Provider runtime stopped unexpectedly");
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
            .name(crate::hyperliquid_display_depth::WORKER_NAME.to_string())
            .spawn(move || {
                let _activity = ActiveWorkerGuard::register(
                    crate::hyperliquid_display_depth::WORKER_NAME,
                    activity,
                );
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
        let records: BTreeMap<_, _> = self
            .records
            .iter()
            .map(|(provider_id, record)| {
                let catalog = record
                    .catalog
                    .enabled
                    .then_some(record.catalog.channels.as_ref());
                (
                    *provider_id,
                    ProviderDispatchRecord {
                        descriptor: record.descriptor,
                        history: &record.history,
                        lifecycle: Some(&record.lifecycle),
                        ctrader: record.ctrader.as_ref(),
                        hyperliquid: record.hyperliquid.as_ref(),
                        tastytrade: record.tastytrade.as_ref(),
                        rithmic: record.rithmic.as_ref(),
                        catalog,
                    },
                )
            })
            .collect();
        let mut lanes = Vec::new();
        for provider_id in &self.order {
            let Some(record) = records.get(provider_id) else {
                continue;
            };
            if record.ctrader.is_some()
                || record.hyperliquid.is_some()
                || record.tastytrade.is_some()
                || record.rithmic.is_some()
            {
                lanes.push(ProviderEventLane::Realtime(provider_id));
            }
            if record.catalog.is_some() {
                lanes.push(ProviderEventLane::Catalog(provider_id));
            }
            if record.hyperliquid.is_some() {
                lanes.push(ProviderEventLane::DisplayDepth(provider_id));
            }
        }
        ProviderDispatch {
            records,
            wake: Some(&self.wake),
            lanes,
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
                hyperliquid: _,
                ctrader: _,
                tastytrade: _,
                rithmic: _,
                catalog,
                workers,
                ..
            } = record;
            drop(history);
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
    pub(super) fn descriptor(&self, provider_id: &str) -> Option<super::ProviderDescriptor> {
        let registered = self
            .records
            .get(provider_id)
            .map(|record| record.descriptor);
        #[cfg(test)]
        {
            registered.or_else(|| {
                super::BUILT_IN_PROVIDER_DESCRIPTORS
                    .iter()
                    .copied()
                    .find(|descriptor| descriptor.id == provider_id)
            })
        }
        #[cfg(not(test))]
        {
            registered
        }
    }

    pub(super) fn live_model(&self, provider_id: &str) -> Option<super::LiveModel> {
        self.descriptor(provider_id)
            .map(|descriptor| descriptor.capabilities.live_model)
    }

    pub(super) fn validate_period(
        &self,
        provider_id: &str,
        period: super::BarPeriod,
    ) -> Result<(), String> {
        let descriptor = self
            .descriptor(provider_id)
            .ok_or_else(|| "market provider is unsupported".to_string())?;
        (descriptor.capabilities.history.supported_period)(period)
    }

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
            .get(super::RITHMIC_DESCRIPTOR.id)
            .is_some_and(|record| record.rithmic.is_some())
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
        let event = match *self.lanes.get(lane)? {
            ProviderEventLane::Realtime(provider_id) => self
                .records
                .get(provider_id)?
                .rithmic
                .and_then(|channels| {
                    channels
                        .binding
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take(&channels.events)
                })
                .map(ProviderRuntimeEvent::BoundRealtime)
                .or_else(|| {
                    self.records
                        .get(provider_id)?
                        .ctrader?
                        .events
                        .try_recv()
                        .ok()
                        .and_then(|bound| {
                            let wire = super::ProviderEvent::ctrader(bound.value);
                            (wire.generation == bound.epoch.get().get()).then_some(
                                ProviderRuntimeEvent::BoundRealtime(super::ProviderEvent {
                                    provider: wire.provider,
                                    generation: bound.epoch,
                                    wire_generation: wire.generation,
                                    kind: wire.kind,
                                }),
                            )
                        })
                })
                .or_else(|| {
                    let bound = self
                        .records
                        .get(provider_id)?
                        .tastytrade?
                        .events
                        .try_recv()
                        .ok()?;
                    let wire = super::ProviderEvent::tastytrade(bound.value);
                    (wire.generation == bound.epoch.get().get()).then_some(
                        ProviderRuntimeEvent::BoundRealtime(super::ProviderEvent {
                            provider: wire.provider,
                            generation: bound.epoch,
                            wire_generation: wire.generation,
                            kind: wire.kind,
                        }),
                    )
                })
                .or_else(|| {
                    let bound = self
                        .records
                        .get(provider_id)?
                        .hyperliquid?
                        .events
                        .try_recv()
                        .ok()?;
                    let epoch = bound.epoch;
                    let wire = super::ProviderEvent::hyperliquid(bound.value);
                    (wire.generation == epoch.get().get()).then_some(
                        ProviderRuntimeEvent::BoundRealtime(super::ProviderEvent {
                            provider: wire.provider,
                            generation: epoch,
                            wire_generation: wire.generation,
                            kind: wire.kind,
                        }),
                    )
                }),
            ProviderEventLane::Catalog(provider_id) => self
                .records
                .get(provider_id)?
                .catalog?
                .try_recv()
                .map(|event| ProviderRuntimeEvent::Catalog(provider_id, event)),
            ProviderEventLane::DisplayDepth(provider_id) => self
                .records
                .get(provider_id)?
                .hyperliquid?
                .display_events
                .try_recv()
                .ok()
                .map(|event| ProviderRuntimeEvent::DisplayDepth(provider_id, event.into())),
        }?;
        self.observe_event(&event);
        Some(event)
    }

    fn observe_event(&self, event: &ProviderRuntimeEvent) {
        let Some(metadata) = event.metadata() else {
            return;
        };
        // A retired queued event cannot overwrite out-of-band recovery health.
        if self
            .wake
            .is_some_and(|wake| wake.overflowed(metadata.provider, metadata.generation))
        {
            return;
        }
        self.observe_generation(
            metadata.provider,
            metadata.generation,
            metadata.reconnecting,
        );
        if let Some(rtt) = metadata.transport_rtt_nanos {
            self.observe_transport_rtt(metadata.provider, metadata.generation, rtt);
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
        record
            .catalog
            .ok_or_else(|| format!("{provider_id} catalog worker is unavailable"))?
            .send(provider_id, command)
    }

    pub(super) fn send_hyperliquid_display_depth(
        &self,
        control: HyperliquidDisplayDepthControl,
    ) -> Result<bool, String> {
        let Some(controls) = self
            .records
            .get(super::HYPERLIQUID_DESCRIPTOR.id)
            .ok_or_else(|| "Provider display-depth worker is unavailable".to_string())?
            .hyperliquid
            .map(|channel| &channel.display_controls)
        else {
            return Ok(false);
        };
        match controls.try_send(control) {
            Ok(()) => Ok(true),
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => {
                Err("Provider display-depth worker is unavailable".to_string())
            }
        }
    }
}

impl MarketService {
    /// Revision of the runtime-owned exchange calendars. It changes whenever a calendar is
    /// loaded, refreshed or cleared; a session status computed under an older revision is stale.
    #[must_use]
    pub fn market_session_revision(&self) -> u64 {
        self.runtime.broker_api.calendar_revision()
    }

    /// Locked or crossed cTrader books withheld from canonical state since startup.
    #[must_use]
    pub fn ctrader_stream_statistics(&self) -> super::CtraderStreamStatistics {
        self.runtime.ctrader.counters.snapshot()
    }

    /// Attaches the trading owner's venue generation to the cTrader supervisor, which
    /// relays it over the shared demo session (D8). A newer attachment retires an older one.
    pub fn attach_ctrader_venue(&self, link: super::CtraderVenueLink) {
        self.runtime.ctrader.venue.put(link);
    }

    /// Returns the runtime-owned dated session projection for one installed instrument.
    #[must_use]
    pub fn market_session_status(
        &self,
        instrument: &aeris_contracts::InstallProviderInstrument,
        now_unix_nanos: i64,
    ) -> aeris_contracts::MarketSessionStatus {
        self.calendar_for(instrument, now_unix_nanos).status(
            &instrument.instrument_id,
            now_unix_nanos.div_euclid(1_000_000),
        )
    }

    fn calendar_for(
        &self,
        instrument: &aeris_contracts::InstallProviderInstrument,
        now_unix_nanos: i64,
    ) -> super::calendar::MarketCalendarService {
        use super::calendar::{CalendarSource, MarketCalendarService};
        let capabilities = super::BUILT_IN_PROVIDER_DESCRIPTORS
            .iter()
            .find(|descriptor| descriptor.id == instrument.provider)
            .map(|descriptor| descriptor.capabilities);
        let provider_calendar = if capabilities.is_some_and(|capabilities| {
            capabilities.calendar == super::CalendarCapability::ProviderPublished
        }) {
            let status = self.runtime.broker_api.market_session_status(
                &instrument.instrument_id,
                &instrument.provider_symbol,
                now_unix_nanos,
            );
            (status.source == aeris_contracts::MarketSessionSource::ProviderCalendar)
                .then_some(status)
        } else {
            None
        };
        let mut calendar = MarketCalendarService::default();
        calendar.resolve(instrument, None);
        if capabilities.is_some_and(|capabilities| capabilities.always_open) {
            calendar.insert(instrument.instrument_id.clone(), CalendarSource::AlwaysOpen);
        }
        if let Some(status) = provider_calendar {
            let valid_from = if status.phase == aeris_contracts::MarketSessionPhase::Closed {
                now_unix_nanos
            } else {
                status.session_start_unix_nanos.unwrap_or(now_unix_nanos)
            };
            let valid_until = if status.phase == aeris_contracts::MarketSessionPhase::Closed {
                status.next_open_unix_nanos
            } else {
                status.session_end_unix_nanos
            };
            if let Some(valid_until) = valid_until {
                calendar.publish_provider_calendar(
                    &instrument.instrument_id,
                    status,
                    valid_from.div_euclid(1_000_000),
                    valid_until.div_euclid(1_000_000),
                );
            }
        }
        calendar
    }

    /// Checks the installed instrument's calendar before simulated execution.
    /// Quote freshness remains the trading owner's separate responsibility.
    /// # Errors
    /// Rejects a closed or unavailable calendar.
    pub fn require_practice_market_session(
        &self,
        provider_id: &str,
        instrument_id: &str,
        provider_symbol: &str,
        now_unix_nanos: i64,
    ) -> Result<bool, String> {
        let installed = self
            .request(|reply| {
                Ok(Command::InstalledInstrument(
                    provider_id.into(),
                    instrument_id.into(),
                    reply,
                ))
            })?
            .ok_or_else(|| "Practice market instrument is not installed".to_string())?;
        if installed.provider_symbol != provider_symbol {
            return Err("Practice market instrument identity changed".into());
        }
        self.calendar_for(&installed, now_unix_nanos)
            .require_practice_session(
                &installed.instrument_id,
                now_unix_nanos.div_euclid(1_000_000),
            )
            .map(|()| true)
            .map_err(|error| error.to_string())
    }

    /// Returns the requested streams supported by the authoritative provider configuration.
    /// # Errors
    /// Returns coordinator failure.
    pub fn available_streams(
        &self,
        provider: &str,
        requested: StreamRequirements,
    ) -> Result<StreamRequirements, String> {
        self.request(|reply| Ok(Command::AvailableStreams(provider.into(), requested, reply)))
    }

    /// Returns provider-neutral metadata for catalog and chart presentation.
    #[must_use]
    pub fn provider_presentations(&self) -> Vec<ProviderPresentationDescriptor> {
        self.runtime.provider_presentations.clone()
    }

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
            ProviderRuntimeSpec::tastytrade(),
            ProviderRuntimeSpec::ctrader(),
        ])
    }

    pub(super) fn start_composed(providers: Vec<ProviderRuntimeSpec>) -> Result<Self, String> {
        let provider_presentations = providers
            .iter()
            .map(|provider| *provider.descriptor.presentation)
            .collect();
        let descriptors = providers
            .iter()
            .map(|provider| provider.descriptor)
            .collect::<Vec<_>>();
        let provider_search_preparers = descriptors
            .iter()
            .filter_map(|descriptor| {
                descriptor
                    .prepare_search
                    .map(|prepare| (descriptor.id, prepare))
            })
            .collect();
        let engine = super::configured_engine_from_descriptors(&descriptors)?;
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let shutdown = Arc::new(AtomicBool::new(false));
        let active_provider_workers = Arc::new(Mutex::new(BTreeSet::new()));
        let broker_api = Arc::new(super::tastytrade::BrokerApi::default());
        let start_context = super::ProviderStartContext {
            tastytrade: Arc::clone(&broker_api),
            ctrader: super::ctrader::CtraderShared::default(),
            rithmic_login: RithmicLoginRevision::default(),
        };
        let provider_registry = ProviderRuntimeRegistry::start(
            providers,
            &command_tx,
            &engine,
            &active_provider_workers,
            &start_context,
        )?;
        let workers = vec![spawn_coordinator(
            engine,
            OwnedCoordinatorChannels {
                commands: command_rx,
                providers: provider_registry,
            },
            Arc::clone(&shutdown),
            Arc::new(super::clock::SystemClock),
        )?];
        Self::build_market_service(
            command_tx,
            shutdown,
            active_provider_workers,
            workers,
            start_context,
            provider_search_preparers,
            provider_presentations,
        )
    }

    fn build_market_service(
        commands: SyncSender<Command>,
        shutdown: Arc<AtomicBool>,
        active_provider_workers: Arc<Mutex<BTreeSet<String>>>,
        mut workers: Vec<thread::JoinHandle<()>>,
        context: super::ProviderStartContext,
        provider_search_preparers: BTreeMap<&'static str, super::ProviderSearchPreparer>,
        provider_presentations: Vec<ProviderPresentationDescriptor>,
    ) -> Result<Self, String> {
        let super::ProviderStartContext {
            tastytrade: broker_api,
            ctrader,
            rithmic_login,
        } = context;
        let (broker_authorization, worker) =
            match super::broker_authorization::BrokerAuthorization::start(
                &shutdown,
                Arc::clone(&broker_api),
            ) {
                Ok(started) => started,
                Err(error) => {
                    shutdown.store(true, Ordering::Release);
                    // A full queue already makes the coordinator runnable.
                    let _ = commands.try_send(Command::ProviderWake);
                    join_runtime_workers(workers);
                    return Err(error);
                }
            };
        workers.push(worker);
        let shutdown_wake = commands.clone();
        Ok(Self {
            commands,
            runtime: Arc::new(MarketRuntime {
                shutdown,
                shutdown_wake,
                broker_api,
                ctrader,
                provider_presentations,
                provider_search_preparers,
                broker_authorization,
                rithmic_login: super::rithmic_login::RithmicLogin::new(rithmic_login),
                active_provider_workers,
                workers: Mutex::new(Some(workers)),
            }),
        })
    }

    /// Cancels provider work, drains accepted provider events, and joins owned workers.
    ///
    /// # Errors
    /// Returns an error when a worker panics or the complete shutdown exceeds `timeout`.
    pub fn shutdown(&self, timeout: Duration) -> Result<(), String> {
        self.runtime.shutdown.store(true, Ordering::Release);
        // A full queue already makes the coordinator runnable.
        let _ = self.commands.try_send(Command::ProviderWake);
        self.runtime.broker_api.cancel_searches();
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
        let preparation = self
            .runtime
            .provider_search_preparers
            .get(search.provider.as_str())
            .copied()
            .map(|prepare| {
                (
                    prepare,
                    search.consumer_id,
                    search.search_generation,
                    search.startup_resolve,
                )
            });
        self.request(|reply| {
            Ok(Command::SearchProviderInstruments(
                ClientId(id(client_id)?),
                search,
                reply,
            ))
        })?;
        if let Some((prepare, consumer, generation, startup)) = preparation {
            prepare(&self.runtime.broker_api, consumer, generation, startup)?;
        }
        Ok(())
    }

    /// Schedules one bounded listed-market statistics screen for an owned consumer. The
    /// result arrives as `MarketRuntimeEvent::ProviderMarketScreen`, fenced to the latest
    /// screen generation of that consumer and provider.
    ///
    /// # Errors
    /// Returns an error for invalid identity, a provider without a market screen, ownership,
    /// or coordinator failure.
    pub fn screen_provider_markets(
        &self,
        client_id: u64,
        screen: ScreenProviderMarkets,
    ) -> Result<(), String> {
        validate_provider_screen(&screen)?;
        self.request(|reply| {
            Ok(Command::ScreenProviderMarkets(
                ClientId(id(client_id)?),
                screen,
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

    pub(super) fn request<T>(
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
    use super::super::ProviderCatalogChannel;
    use super::*;
    use crate::study::{
        NativeStudyProgram, NativeStudyRegistration, StudyDefinition, StudyDependency,
        StudyExecutionContext, StudyInvalidationPolicy, StudyMarketInput, StudyOutputSpec,
        StudyPaneTarget, StudyPlotKind, StudyPointStyle, StudyScaleTarget, StudySettings,
    };
    use aeris_market_data::{BarPeriod, MarketBar};

    #[test]
    fn catalog_channel_dispatches_and_converts_without_provider_enum() {
        let (control_tx, control_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (event_tx, event_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let channel = super::super::ProviderCatalogChannelImpl::new(
            control_tx,
            event_rx,
            dispatch_rithmic_catalog,
        );
        channel
            .send(
                "rithmic",
                ProviderCatalogCommand::Search(SearchProviderInstruments::default()),
            )
            .expect("bounded catalog request");
        assert!(matches!(
            control_rx.try_recv(),
            Ok(RithmicCatalogControl::Search(_))
        ));
        event_tx
            .send(
                crate::rithmic_realtime::RithmicCatalogEvent::SearchCompleted(
                    aeris_contracts::ProviderInstrumentSearchResult::default(),
                ),
            )
            .expect("bounded catalog response");
        assert!(matches!(
            channel.try_recv(),
            Some(super::super::ProviderCatalogEvent::SearchCompleted(_))
        ));
    }

    #[test]
    fn catalog_channel_preserves_provider_screen_and_capacity_rules() {
        let (rithmic_tx, rithmic_rx) = mpsc::sync_channel(1);
        let (_, rithmic_events) =
            mpsc::sync_channel::<crate::rithmic_realtime::RithmicCatalogEvent>(1);
        let rithmic =
            ProviderCatalogChannelImpl::new(rithmic_tx, rithmic_events, dispatch_rithmic_catalog);
        assert!(
            rithmic
                .send(
                    "rithmic",
                    ProviderCatalogCommand::Screen(ScreenProviderMarkets::default())
                )
                .is_err()
        );
        rithmic
            .send(
                "rithmic",
                ProviderCatalogCommand::Search(SearchProviderInstruments::default()),
            )
            .expect("first control fits");
        assert!(
            rithmic
                .send(
                    "rithmic",
                    ProviderCatalogCommand::Select(SelectProviderInstrument::default())
                )
                .is_err(),
            "a full bounded catalog channel refuses another control"
        );
        assert!(matches!(
            rithmic_rx.try_recv(),
            Ok(RithmicCatalogControl::Search(_))
        ));

        let (hyperliquid_tx, hyperliquid_rx) = mpsc::sync_channel(1);
        let (_, hyperliquid_events) =
            mpsc::sync_channel::<crate::hyperliquid_realtime::HyperliquidCatalogEvent>(1);
        let hyperliquid = ProviderCatalogChannelImpl::new(
            hyperliquid_tx,
            hyperliquid_events,
            dispatch_hyperliquid_catalog,
        );
        hyperliquid
            .send(
                "hyperliquid",
                ProviderCatalogCommand::Screen(ScreenProviderMarkets::default()),
            )
            .expect("public provider supports market screens");
        assert!(matches!(
            hyperliquid_rx.try_recv(),
            Ok(HyperliquidCatalogControl::Screen(_))
        ));
    }

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
                backwards_exhausted: false,
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
            ..Default::default()
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
        let service = MarketService::build_market_service(
            commands,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(BTreeSet::new())),
            Vec::new(),
            super::super::ProviderStartContext {
                tastytrade: Arc::new(super::super::tastytrade::BrokerApi::default()),
                ctrader: super::super::ctrader::CtraderShared::default(),
                rithmic_login: RithmicLoginRevision::default(),
            },
            BTreeMap::new(),
            vec![*super::super::RITHMIC_DESCRIPTOR.presentation],
        )
        .expect("test runtime starts");
        assert_eq!(service.provider_presentations()[0].id, "rithmic");
        let result =
            service.request_with_timeout(|reply| Ok(Command::Status(reply)), Duration::ZERO);
        assert!(result.unwrap_err().contains("cancelled before execution"));
        let Command::Request(_, state) = receiver.try_recv().unwrap() else {
            panic!("request envelope");
        };
        assert!(!state.start());
        service
            .shutdown(Duration::from_secs(1))
            .expect("test runtime stops");
    }

    #[test]
    fn fourth_descriptor_starts_without_coordinator_registration_edits() {
        const FOURTH_PRESENTATION: aeris_contracts::ProviderPresentationDescriptor =
            aeris_contracts::ProviderPresentationDescriptor {
                id: "fourth-test-provider",
                ..super::super::RITHMIC_PRESENTATION
            };
        let mut spec = ProviderRuntimeSpec::rithmic(Box::new(LiveRithmicHistory), false);
        spec.descriptor.id = "fourth-test-provider";
        spec.descriptor.presentation = &FOURTH_PRESENTATION;
        spec.descriptor.capabilities.account_id = "fourth-test-account";
        spec.descriptor.capabilities.history.source =
            super::super::HistorySourceKind::DedicatedWorker("aeris-fourth-history");
        spec.descriptor.flush_demand = |_| {};
        let service = MarketService::start_composed(vec![spec]).expect("fourth runtime starts");
        assert!(
            service
                .status()
                .expect("runtime status")
                .providers
                .iter()
                .any(|provider| provider.provider == "fourth-test-provider")
        );
        service
            .shutdown(Duration::from_secs(1))
            .expect("fourth runtime stops");
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
