//! Hyperliquid market-provider runtime registration, session binding, history
//! fetcher, display-depth binding, and catalog/scan dispatcher.

use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::Duration,
};

use aeris_hyperliquid_market_adapter::{
    HyperliquidHttpClient, HyperliquidHttpConfig, hyperliquid_interval_for_period,
};
use aeris_market_data::BarPeriod;
use aeris_market_engine::{
    MarketStream, ProviderCapabilities as EngineProviderCapabilities, StreamRequirements,
    fence::{Epoch, Fenced, ProviderTag},
};
use aeris_market_runtime::plugin::{ProviderEventSink, ProviderSession, ProviderSessionError};
use aeris_market_runtime::{
    ProviderCatalogSymbol, ProviderConnectionKind, ProviderDailyBarCadence, ProviderFeedLevel,
    ProviderPresentationDescriptor,
    plugin::{
        ActiveWorkerGuard, AlertDemandUpdate, COMMAND_CAPACITY, CalendarBarStamp,
        CandleDemandPolicy, CatalogPublisher, CatalogSessionBinding, ConcurrencyLimits,
        DemandFlushPolicy, DepthCapabilities, DisplayDepthRequest, HistoryRequest, HistorySnapshot,
        HistorySource, LiveModel, ProviderCapabilities, ProviderCatalogChannelImpl,
        ProviderCatalogChannels, ProviderCatalogCommand, ProviderCatalogEvent,
        ProviderCoordinatorWake, ProviderDemand, ProviderDescriptor, ProviderDisplayDepthChannels,
        ProviderDisplayDepthEvent, ProviderEvent, ProviderHistoryCapabilities,
        ProviderRealtimeChannels, ProviderRecoveryPolicy, ProviderRuntimeRecord,
        ProviderRuntimeSpec, ProviderStartHandle, QuoteCapability, REALTIME_CAPACITY,
        ReconnectPolicy, StartedProviderRuntime, SystemClock, TradeDemandPolicy,
        history_instrument_from_install, try_control, try_send_catalog_control,
    },
};

use crate::{
    hyperliquid_display_depth::{
        HyperliquidDisplayBookDemand, HyperliquidDisplayDepthControl,
        HyperliquidDisplayDepthDemand, HyperliquidDisplayDepthEvent,
    },
    hyperliquid_market_scan::HyperliquidScanJob,
    hyperliquid_realtime::{
        HYPERLIQUID_PUBLIC_ACCOUNT_ID, HyperliquidCandleDemand, HyperliquidCatalogControl,
        HyperliquidCatalogEvent, HyperliquidDemand, HyperliquidInstrumentDemand,
        HyperliquidRealtimeControl, HyperliquidRealtimeEvent,
    },
};

const REALTIME_CONTROL_CAPACITY: usize = 2;

pub(crate) const HYPERLIQUID_PRESENTATION: ProviderPresentationDescriptor =
    ProviderPresentationDescriptor {
        id: "hyperliquid",
        display_name: "Hyperliquid",
        chart_interval_labels: &[
            "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "8h", "12h", "1D", "3D", "1W", "1M",
        ],
        daily_bars: ProviderDailyBarCadence::FixedDay,
        search_hint: "Search Hyperliquid markets",
        logo_key: "hyperliquid",
        broker_venue_id: None,
        attribution: None,
        catalog_symbol: ProviderCatalogSymbol::ProviderSymbol,
        selection_entitlement_id: "hyperliquid-public",
        catalog_refresh_on_startup: true,
        ready_label_suffix: "",
        feed_level: ProviderFeedLevel::PriceLevels,
        search_categories_available: false,
        market_screen_available: true,
        market_scan_available: true,
        trades_available: true,
        connection_kind: ProviderConnectionKind::Public,
    };

pub(crate) const HYPERLIQUID_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: "hyperliquid",
    presentation: &HYPERLIQUID_PRESENTATION,
    login_schema: None,
    capabilities: HYPERLIQUID_CAPABILITIES,
    demand_flush: DemandFlushPolicy::Dirty,
};

pub(crate) fn provider_spec(
    history: Box<dyn HistorySource>,
    realtime_enabled: bool,
) -> ProviderRuntimeSpec {
    ProviderRuntimeSpec::new(
        HYPERLIQUID_DESCRIPTOR,
        Some(history),
        realtime_enabled,
        Box::new(start_hyperliquid_runtime),
    )
    .with_open_interest(Arc::new(
        aeris_hyperliquid_market_adapter::open_interest::fetch_open_interest,
    ))
}

const HYPERLIQUID_CAPABILITIES: ProviderCapabilities = ProviderCapabilities {
    account_id: HYPERLIQUID_PUBLIC_ACCOUNT_ID,
    engine: EngineProviderCapabilities {
        historical_bars: true,
        realtime_bars: true,
        streams: StreamRequirements::BARS
            .with(MarketStream::Trades)
            .with(MarketStream::Quotes)
            .with(MarketStream::Depth),
    },
    always_open: true,
    concurrency: ConcurrencyLimits {
        maximum_sessions: 1,
    },
    depth: Some(DepthCapabilities),
    quotes: Some(QuoteCapability::DedicatedTopOfBook),
    reconnect: ReconnectPolicy::hyperliquid(),
    history: ProviderHistoryCapabilities {
        worker: "aeris-hyperliquid-history",
        supported_period: hyperliquid_supported_period,
    },
    recovery_policy: ProviderRecoveryPolicy::WorkerReconcilesDemand,
    alert_demand_update: AlertDemandUpdate::MarkDirty,
    live_model: LiveModel::ProviderCandles,
    calendar_bar_stamp: CalendarBarStamp::TradingDateMidnightUtc,
    overflow_recovery_detail: "Local market event queue overflow; repairing continuity",
    candle_requires_connected: true,
    candle_correction_detail: "Hyperliquid candle replacement requires covering history",
    candle_wire_interval: Some(hyperliquid_candle_interval),
    candle_demand_policy: CandleDemandPolicy::ReconcileAfterSelection,
    trade_demand_policy: TradeDemandPolicy::SessionManaged,
    instrument_missing_detail: "Hyperliquid instrument is not installed",
    catalog_session_binding: CatalogSessionBinding::CurrentSession,
    maximum_order_level_instruments: 0,
    recovers_silence: true,
};

#[derive(Default)]
pub(crate) struct LiveHyperliquidHistory(HyperliquidHttpClient);

impl HistorySource for LiveHyperliquidHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        let installed = request
            .instrument
            .as_ref()
            .ok_or_else(|| "Provider instrument is not installed".to_string())?;
        let instrument = history_instrument_from_install(installed);
        aeris_hyperliquid_market_adapter::history::fetch(
            &mut self.0,
            &request.series,
            request.provider_generation.0.get(),
            &instrument,
            request.req_window(),
            &request.stop,
        )
    }
}

pub(crate) fn hyperliquid_wire(demand: &ProviderDemand) -> HyperliquidDemand {
    let mut candles = BTreeSet::new();
    let mut trades = BTreeSet::new();
    let mut quotes = BTreeSet::new();
    let mut books = BTreeSet::new();
    for (series, streams, instrument) in &demand.candle_series {
        let (Ok(price_scale), Ok(quantity_scale), Ok(interval)) = (
            u8::try_from(instrument.price_scale),
            u8::try_from(instrument.quantity_scale),
            hyperliquid_interval_for_period(series.period),
        ) else {
            continue;
        };
        let mapping = HyperliquidInstrumentDemand {
            wire_coin: instrument.provider_symbol.clone(),
            instrument_id: series.instrument_id.clone(),
            entitlement_id: series.entitlement_id.clone(),
            price_scale,
            quantity_scale,
        };
        if streams.contains(MarketStream::Bars) {
            candles.insert(HyperliquidCandleDemand {
                instrument: mapping.clone(),
                interval: interval.to_string(),
            });
        }
        if streams.contains(MarketStream::Trades) {
            trades.insert(mapping.clone());
        }
        if streams.contains(MarketStream::Quotes) || streams.contains(MarketStream::Depth) {
            quotes.insert(mapping.clone());
        }
        if streams.contains(MarketStream::Depth) {
            books.insert(mapping);
        }
    }
    for requested in &demand.instruments {
        let instrument = &requested.instrument;
        let (Ok(price_scale), Ok(quantity_scale)) = (
            u8::try_from(instrument.price_scale),
            u8::try_from(instrument.quantity_scale),
        ) else {
            continue;
        };
        let mapping = HyperliquidInstrumentDemand {
            wire_coin: instrument.provider_symbol.clone(),
            instrument_id: instrument.instrument_id.clone(),
            entitlement_id: instrument.entitlement_id.clone(),
            price_scale,
            quantity_scale,
        };
        if requested.streams.contains(MarketStream::Quotes)
            || requested.streams.contains(MarketStream::Depth)
        {
            quotes.insert(mapping.clone());
        }
        if requested.streams.contains(MarketStream::Depth) {
            books.insert(mapping);
        }
    }
    for requested in &demand.instruments {
        if requested.streams.contains(MarketStream::Trades) {
            let instrument = &requested.instrument;
            if let (Ok(price_scale), Ok(quantity_scale)) = (
                u8::try_from(instrument.price_scale),
                u8::try_from(instrument.quantity_scale),
            ) {
                trades.insert(HyperliquidInstrumentDemand {
                    wire_coin: instrument.provider_symbol.clone(),
                    instrument_id: instrument.instrument_id.clone(),
                    entitlement_id: instrument.entitlement_id.clone(),
                    price_scale,
                    quantity_scale,
                });
            }
        }
    }
    HyperliquidDemand {
        candles: candles.into_iter().collect(),
        trades: trades.into_iter().collect(),
        quotes: quotes.into_iter().collect(),
        books: books.into_iter().collect(),
    }
}

pub(crate) struct HyperliquidSession {
    pub(crate) controls: SyncSender<HyperliquidRealtimeControl>,
    pub(crate) display_controls: SyncSender<HyperliquidDisplayDepthControl>,
    pub(crate) history: SyncSender<HistoryRequest>,
}

impl ProviderSession for HyperliquidSession {
    #[cfg(feature = "probe")]
    fn probe_fail_websocket(
        &self,
        subsequent_connect_failures: u8,
    ) -> Result<bool, ProviderSessionError> {
        try_control(
            &self.controls,
            HyperliquidRealtimeControl::FailWebSocket {
                subsequent_connect_failures,
            },
        )
    }

    fn connect(
        &self,
        epoch: Epoch<ProviderTag>,
        sink: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError> {
        debug_assert_eq!(sink.epoch, epoch);
        try_control(&self.controls, HyperliquidRealtimeControl::Connect(sink))
    }

    fn apply_demand(&self, demand: &ProviderDemand) -> Result<bool, ProviderSessionError> {
        try_control(
            &self.controls,
            HyperliquidRealtimeControl::Subscribe(hyperliquid_wire(demand)),
        )
    }

    fn request_history(&self, request: HistoryRequest) -> Result<bool, ProviderSessionError> {
        try_control(&self.history, request)
    }

    fn release_idle(&self) -> Result<bool, ProviderSessionError> {
        let main = try_control(&self.controls, HyperliquidRealtimeControl::Stop)?;
        let display = try_control(&self.display_controls, HyperliquidDisplayDepthControl::Stop)?;
        Ok(main && display)
    }

    fn shutdown(&self) {
        let _ = self.release_idle();
    }
}

pub(crate) fn realtime_channels(
    controls: SyncSender<HyperliquidRealtimeControl>,
    events: Receiver<Fenced<ProviderTag, HyperliquidRealtimeEvent>>,
    display_controls: SyncSender<HyperliquidDisplayDepthControl>,
) -> ProviderRealtimeChannels {
    ProviderRealtimeChannels::new(
        move |history| {
            Box::new(HyperliquidSession {
                controls: controls.clone(),
                display_controls: display_controls.clone(),
                history: history.clone(),
            })
        },
        move || {
            let bound = events.try_recv().ok()?;
            let epoch = bound.epoch;
            let wire = crate::hyperliquid_realtime::into_wire_provider_event(bound.value);
            (wire.generation == epoch.get().get()).then_some(ProviderEvent::bound(
                wire.provider,
                epoch,
                wire.generation,
                wire.kind,
            ))
        },
    )
}

pub(crate) fn display_depth_channels(
    controls: SyncSender<HyperliquidDisplayDepthControl>,
    events: Receiver<HyperliquidDisplayDepthEvent>,
) -> ProviderDisplayDepthChannels {
    ProviderDisplayDepthChannels::new(
        move |requests: &[DisplayDepthRequest]| {
            let mut books = BTreeSet::new();
            for request in requests {
                let (Ok(price_scale), Ok(quantity_scale)) = (
                    u8::try_from(request.instrument.price_scale),
                    u8::try_from(request.instrument.quantity_scale),
                ) else {
                    continue;
                };
                books.insert(HyperliquidDisplayBookDemand {
                    instrument: HyperliquidInstrumentDemand {
                        wire_coin: request.instrument.provider_symbol.clone(),
                        instrument_id: request.instrument.instrument_id.clone(),
                        entitlement_id: request.instrument.entitlement_id.clone(),
                        price_scale,
                        quantity_scale,
                    },
                    provider_generation: request.provider_generation,
                });
            }
            let demand = HyperliquidDisplayDepthDemand {
                books: books.into_iter().collect(),
            };
            match controls.try_send(HyperliquidDisplayDepthControl::Subscribe(demand)) {
                Ok(()) => Ok(true),
                Err(TrySendError::Full(_)) => Ok(false),
                Err(TrySendError::Disconnected(_)) => {
                    Err("Provider display-depth worker is unavailable".to_string())
                }
            }
        },
        move || events.try_recv().ok().map(display_depth_event),
    )
}

pub(crate) fn dispatch_catalog(
    controls: &SyncSender<HyperliquidCatalogControl>,
    provider: &str,
    command: ProviderCatalogCommand,
) -> Result<(), String> {
    let control = match command {
        ProviderCatalogCommand::Search(value) => HyperliquidCatalogControl::Search(value),
        ProviderCatalogCommand::Select(value) => HyperliquidCatalogControl::Select(value),
        ProviderCatalogCommand::Screen(value) => HyperliquidCatalogControl::Screen(value),
        ProviderCatalogCommand::Scan(scan) => {
            let interval = scan.definition.timeframe;
            // Every Hyperliquid market trades around the clock.
            let period = aeris_market_runtime::resolve_cadence(
                interval,
                &HYPERLIQUID_PRESENTATION,
                aeris_instruments::CalendarSourceKind::AlwaysOpen,
            )
            .map_err(|_| format!("{provider} does not offer a {interval} market scan"))?;
            HyperliquidCatalogControl::Scan { scan, period }
        }
    };
    try_send_catalog_control(controls, control, provider)
}

fn display_depth_event(event: HyperliquidDisplayDepthEvent) -> ProviderDisplayDepthEvent {
    match event {
        HyperliquidDisplayDepthEvent::Reset { display_generation } => {
            ProviderDisplayDepthEvent::Reset { display_generation }
        }
        HyperliquidDisplayDepthEvent::Snapshot(display) => ProviderDisplayDepthEvent::Snapshot {
            provider_generation: display.provider_generation,
            display_generation: display.display_generation,
            snapshot: display.snapshot,
        },
    }
}

impl From<HyperliquidCatalogEvent> for ProviderCatalogEvent {
    fn from(event: HyperliquidCatalogEvent) -> Self {
        match event {
            HyperliquidCatalogEvent::SearchCompleted(result) => Self::SearchCompleted(result),
            HyperliquidCatalogEvent::SelectionResolved {
                consumer_id,
                command_generation,
                instrument,
            } => Self::SelectionResolved {
                consumer_id,
                command_generation,
                instrument,
            },
            HyperliquidCatalogEvent::Rejected {
                rejection,
                selection,
            } => Self::Rejected {
                rejection,
                selection,
            },
            HyperliquidCatalogEvent::ScreenCompleted(screen) => Self::ScreenCompleted(screen),
            HyperliquidCatalogEvent::ScreenRejected(rejection) => Self::ScreenRejected(rejection),
            HyperliquidCatalogEvent::ScanUpdated(scan) => Self::ScanUpdated(scan),
            HyperliquidCatalogEvent::ScanRejected(rejection) => Self::ScanRejected(rejection),
            HyperliquidCatalogEvent::RefreshFailed { detail } => Self::RefreshFailed(detail),
        }
    }
}

fn hyperliquid_candle_interval(period: BarPeriod) -> Result<String, String> {
    hyperliquid_interval_for_period(period)
        .map(str::to_owned)
        .map_err(|_| "Hyperliquid history is unavailable for this interval".to_string())
}

fn hyperliquid_supported_period(period: BarPeriod) -> Result<(), String> {
    hyperliquid_interval_for_period(period)
        .map(|_| ())
        .map_err(|_| "Provider history is unavailable for this interval".to_string())
}

fn start_hyperliquid_runtime(
    spec: ProviderRuntimeSpec,
    handle: &ProviderStartHandle<'_>,
) -> Result<ProviderRuntimeRecord, String> {
    let enabled = spec.realtime_enabled;
    let descriptor = spec.descriptor;
    let mut started = handle.start_history_runtime(spec)?;
    let wake = handle.wake();
    let active_workers = handle.active_workers();
    let reconnect_delay = if enabled {
        match handle.configured_reconnect_delay(descriptor.id) {
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
    // Scan publications share the catalog channel, so its overflow rejects pending scans too.
    let scan_events_tx =
        CatalogPublisher::new(catalog_events_tx.clone(), descriptor.id, wake.clone());
    let catalog_events_tx = CatalogPublisher::new(catalog_events_tx, descriptor.id, wake.clone());
    let (catalog_controls, catalog_controls_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (scan_jobs, scan_jobs_rx) =
        mpsc::sync_channel(crate::hyperliquid_market_scan::SCAN_JOB_CAPACITY);
    let (realtime_events_tx, realtime_events) = mpsc::sync_channel(REALTIME_CAPACITY);
    let (realtime_controls, realtime_controls_rx) = mpsc::sync_channel(REALTIME_CONTROL_CAPACITY);
    let (display_events_tx, display_events) = mpsc::sync_channel(REALTIME_CAPACITY);
    let (display_controls, display_controls_rx) = mpsc::sync_channel(REALTIME_CONTROL_CAPACITY);
    // One generation counter shared by the catalog thread (install
    // stamping) and the realtime thread (socket sessions). The realtime
    // thread owns it: every connection publishes its generation here.
    let ws_generation = Arc::new(AtomicU64::new(1));
    if enabled {
        let Some(reconnect_delay) = reconnect_delay else {
            started.cancel_and_join();
            return Err("Provider reconnect policy is unavailable".to_string());
        };
        if let Err(error) =
            spawn_hyperliquid_scan(&mut started, &active_workers, scan_jobs_rx, scan_events_tx)
        {
            started.cancel_and_join();
            return Err(error.to_string());
        }
        if let Err(error) = spawn_hyperliquid_catalog(
            &mut started,
            &active_workers,
            catalog_controls_rx,
            catalog_events_tx,
            scan_jobs,
            Arc::clone(&ws_generation),
        ) {
            started.cancel_and_join();
            return Err(error.to_string());
        }
        if let Err(error) = spawn_hyperliquid_provider(
            &mut started,
            &active_workers,
            realtime_controls_rx,
            realtime_events_tx,
            ws_generation,
            wake.clone(),
        ) {
            started.cancel_and_join();
            return Err(error.to_string());
        }
        if let Err(error) = spawn_hyperliquid_display_depth(
            &mut started,
            &active_workers,
            display_controls_rx,
            display_events_tx,
            wake,
            reconnect_delay,
        ) {
            started.cancel_and_join();
            return Err(error.to_string());
        }
    }
    Ok(ProviderRuntimeRecord {
        descriptor,
        history: started.history,
        cancellation: started.cancellation,
        lifecycle: started.lifecycle,
        realtime: enabled.then(|| {
            realtime_channels(realtime_controls, realtime_events, display_controls.clone())
        }),
        display_depth: enabled.then(|| display_depth_channels(display_controls, display_events)),
        catalog: ProviderCatalogChannels {
            enabled,
            channels: Box::new(ProviderCatalogChannelImpl::new(
                catalog_controls,
                catalog_events,
                dispatch_catalog,
            )),
        },
        workers: started.workers,
    })
}

fn spawn_hyperliquid_catalog(
    started: &mut StartedProviderRuntime,
    active_workers: &Arc<Mutex<BTreeSet<String>>>,
    controls: mpsc::Receiver<HyperliquidCatalogControl>,
    events: CatalogPublisher<HyperliquidCatalogEvent>,
    scan_jobs: mpsc::SyncSender<HyperliquidScanJob>,
    ws_generation: Arc<AtomicU64>,
) -> std::io::Result<()> {
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
                &scan_jobs,
                &ws_generation,
                &cancellation,
                HyperliquidHttpConfig::default(),
            );
            if !cancellation.load(Ordering::Acquire) {
                lifecycle.mark_terminal_failure("Provider catalog runtime stopped unexpectedly");
            }
        })?;
    started.workers.push(worker);
    Ok(())
}

fn spawn_hyperliquid_scan(
    started: &mut StartedProviderRuntime,
    active_workers: &Arc<Mutex<BTreeSet<String>>>,
    jobs: mpsc::Receiver<HyperliquidScanJob>,
    events: CatalogPublisher<HyperliquidCatalogEvent>,
) -> std::io::Result<()> {
    let cancellation = Arc::clone(&started.cancellation);
    let lifecycle = Arc::clone(&started.lifecycle);
    let activity = Arc::clone(active_workers);
    let worker = thread::Builder::new()
        .name(crate::hyperliquid_market_scan::SCAN_WORKER_NAME.to_string())
        .spawn(move || {
            let _activity = ActiveWorkerGuard::register(
                crate::hyperliquid_market_scan::SCAN_WORKER_NAME,
                activity,
            );
            let mut client = HyperliquidHttpClient::default();
            crate::hyperliquid_market_scan::run_scan(
                &jobs,
                &events,
                &cancellation,
                &SystemClock,
                |request, stop| {
                    crate::hyperliquid_market_scan::fetch_candles(&mut client, request, stop)
                },
            );
            if !cancellation.load(Ordering::Acquire) {
                lifecycle.mark_terminal_failure("Provider scan runtime stopped unexpectedly");
            }
        })?;
    started.workers.push(worker);
    Ok(())
}

fn spawn_hyperliquid_provider(
    started: &mut StartedProviderRuntime,
    active_workers: &Arc<Mutex<BTreeSet<String>>>,
    controls: mpsc::Receiver<HyperliquidRealtimeControl>,
    events: mpsc::SyncSender<Fenced<ProviderTag, HyperliquidRealtimeEvent>>,
    ws_generation: Arc<AtomicU64>,
    wake: ProviderCoordinatorWake,
) -> std::io::Result<()> {
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
        })?;
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
) -> std::io::Result<()> {
    let cancellation = Arc::clone(&started.cancellation);
    let activity = Arc::clone(active_workers);
    let worker = thread::Builder::new()
        .name(crate::hyperliquid_display_depth::WORKER_NAME.to_string())
        .spawn(move || {
            let _activity = ActiveWorkerGuard::register(
                crate::hyperliquid_display_depth::WORKER_NAME,
                activity,
            );
            let notify = move || wake.notify();
            crate::hyperliquid_display_depth::run(
                &controls,
                &events,
                &cancellation,
                &notify,
                reconnect_delay,
            );
        })?;
    started.workers.push(worker);
    Ok(())
}
