//! Rithmic market-provider runtime registration, session binding, history
//! fetcher, and catalog dispatcher.

use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        atomic::Ordering,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
};

use aeris_market_data::BarPeriod;
use aeris_market_engine::{
    MarketEngine, MarketStream, ProviderCapabilities as EngineProviderCapabilities,
    StreamRequirements,
    fence::{Epoch, ProviderTag},
};
pub(in super::super) use aeris_rithmic_protocol_adapter::{
    RITHMIC_LOGIN_SCHEMA, RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID, RithmicLogin,
};
pub use aeris_rithmic_protocol_adapter::{RithmicLoginSummary, RithmicServer};

use crate::{
    ProviderCatalogSymbol, ProviderPresentationDescriptor,
    rithmic_realtime::{
        RithmicCatalogControl, RithmicCatalogEvent, RithmicInstrumentDemand, RithmicLoginRevision,
        RithmicRealtimeControl, RithmicRealtimeDemand, RithmicRealtimeEvent,
    },
};

use super::super::{
    ActiveWorkerGuard, AlertDemandUpdate, COMMAND_CAPACITY, CandleDemandPolicy, CatalogPublisher,
    CatalogSessionBinding, Command, ConcurrencyLimits, Coordinator, DepthCapabilities,
    HistoryRequest, HistorySnapshot, HistorySource, LiveModel, ProviderCapabilities,
    ProviderCatalogChannelImpl, ProviderCatalogChannels, ProviderCatalogCommand,
    ProviderCatalogEvent, ProviderCoordinatorWake, ProviderDemand, ProviderDescriptor,
    ProviderEvent, ProviderEventKind, ProviderHistoryCapabilities, ProviderRealtimeChannels,
    ProviderRecoveryPolicy, ProviderRuntimeRecord, ProviderRuntimeRegistry, ProviderRuntimeSpec,
    ProviderStartContext, QuoteCapability, REALTIME_CAPACITY, RITHMIC_REALTIME_CONTROL_CAPACITY,
    StartedProviderRuntime, TradeDemandPolicy, calendar, configured_reconnect_delay,
    provider_session::{ProviderEventSink, ProviderSession, ProviderSessionError, try_control},
    reconnect, try_send_catalog_control,
};

pub(in super::super) const RITHMIC_PRESENTATION: ProviderPresentationDescriptor =
    ProviderPresentationDescriptor {
        id: "rithmic",
        display_name: "Rithmic",
        chart_interval_labels: &[
            "1s", "5s", "10s", "15s", "30s", "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h",
            "8h", "12h", "1D", "1W", "1M",
        ],
        daily_bars: crate::ProviderDailyBarCadence::TradingSession,
        search_hint: "Search Rithmic symbols",
        logo_key: "rithmic",
        broker_venue_id: Some(aeris_trading::venue::BROKER_VENUE_ID),
        attribution: Some(crate::ProviderAttribution {
            market_data_mark_key: "market_data_by_rithmic",
            powered_by_mark_key: "powered_by_omne",
            notices: &[
                "The R | Protocol API™ software is Copyright © {year} by Rithmic, LLC. All rights reserved.",
                "Trading Platform by Rithmic™ is a trademark of Rithmic, LLC. All rights reserved.",
                "The OMNE™ software is Copyright © {year} by Omnesys, LLC and Omnesys Technologies, Inc. All rights reserved.",
            ],
            powered_by_notice: "is a trademark of Omnesys, LLC and Omnesys Technologies, Inc. All rights reserved.",
        }),
        catalog_symbol: ProviderCatalogSymbol::ProviderSymbol,
        selection_entitlement_id: RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID,
        catalog_refresh_on_startup: false,
        ready_label_suffix: "",
        // Rithmic streams depth by order, not only aggregated price levels.
        feed_level: crate::ProviderFeedLevel::OrderLevels,
        search_categories_available: false,
        market_screen_available: false,
        market_scan_available: false,
        trades_available: true,
        connection_kind: crate::ProviderConnectionKind::Credentials,
    };

pub(in super::super) const RITHMIC_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: "rithmic",
    presentation: &RITHMIC_PRESENTATION,
    login_schema: Some(&RITHMIC_LOGIN_SCHEMA),
    capabilities: ProviderCapabilities {
        account_id: RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID,
        engine: EngineProviderCapabilities {
            historical_bars: true,
            realtime_bars: true,
            streams: StreamRequirements::BARS
                .with(MarketStream::Trades)
                .with(MarketStream::Quotes)
                .with(MarketStream::Depth)
                .with(MarketStream::OrderLevels),
        },
        always_open: false,
        concurrency: ConcurrencyLimits {
            maximum_sessions: 1,
        },
        depth: Some(DepthCapabilities),
        quotes: Some(QuoteCapability::DedicatedTopOfBook),
        reconnect: reconnect::ReconnectPolicy::rithmic_recovery(),
        history: ProviderHistoryCapabilities {
            worker: "aeris-rithmic-history",
            supported_period: rithmic_supported_period,
        },
        recovery_policy: ProviderRecoveryPolicy::CoordinatorReissuesDemand,
        alert_demand_update: AlertDemandUpdate::Immediate,
        live_model: LiveModel::TradeBuilt,
        calendar_bar_stamp: calendar::CalendarBarStamp::SessionOpen,
        overflow_recovery_detail: "Local market event queue overflow; repairing continuity",
        candle_requires_connected: false,
        candle_correction_detail: "Provider candle correction requires covering history",
        candle_wire_interval: None,
        candle_demand_policy: CandleDemandPolicy::SessionManaged,
        trade_demand_policy: TradeDemandPolicy::Immediate,
        instrument_missing_detail: "Rithmic instrument is not installed",
        catalog_session_binding: CatalogSessionBinding::ResolvingSession,
        maximum_order_level_instruments: 8,
        recovers_silence: true,
    },
    start: start_rithmic_runtime,
    flush_demand: flush_rithmic_provider_demand,
};

pub(in super::super) struct LiveRithmicHistory;

impl HistorySource for LiveRithmicHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        let installed = request
            .instrument
            .as_ref()
            .ok_or_else(|| "Provider instrument is not installed".to_string())?;
        let instrument = super::super::history_instrument_from_install(installed);
        aeris_rithmic_protocol_adapter::history_fetch::fetch(
            &request.series,
            request.provider_generation.0.get(),
            &instrument,
            request.req_window(),
            &request.stop,
        )
    }
}

pub(in super::super) fn rithmic_wire(
    demand: &ProviderDemand,
) -> Result<RithmicRealtimeDemand, String> {
    if demand.missing_catalog {
        return Err("Rithmic instrument is not installed".to_string());
    }
    Ok(RithmicRealtimeDemand {
        instruments: demand
            .instruments
            .iter()
            .filter_map(|requested| {
                let trades = requested.streams.contains(MarketStream::Bars)
                    || requested.streams.contains(MarketStream::Trades)
                    || requested.streams.contains(MarketStream::Depth);
                let quotes = requested.streams.contains(MarketStream::Quotes);
                let order_book = requested.streams.contains(MarketStream::Depth);
                let order_levels = requested.order_levels;
                (trades || quotes || order_book || order_levels).then_some(
                    RithmicInstrumentDemand {
                        instrument: requested.instrument.clone(),
                        trades,
                        quotes,
                        order_book,
                        order_levels,
                    },
                )
            })
            .collect(),
    })
}

pub(in super::super) struct RithmicSession {
    pub(in super::super) controls: SyncSender<RithmicRealtimeControl>,
    pub(in super::super) history: SyncSender<HistoryRequest>,
    pub(in super::super) binding: Arc<Mutex<RithmicBinding>>,
}

#[derive(Default)]
pub(in super::super) struct RithmicBinding {
    pub(in super::super) sink: Option<ProviderEventSink>,
    pub(in super::super) wire_generation: Option<u64>,
    pub(in super::super) pending: Option<RithmicRealtimeEvent>,
}

impl ProviderSession for RithmicSession {
    fn adapter_retries_transport(&self) -> bool {
        true
    }

    fn adapter_labels_sessions(&self) -> bool {
        true
    }

    fn connect(
        &self,
        epoch: Epoch<ProviderTag>,
        sink: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError> {
        debug_assert_eq!(epoch, sink.epoch);
        self.binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sink = Some(sink);
        Ok(true)
    }

    fn rebind(
        &self,
        epoch: Epoch<ProviderTag>,
        sink: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError> {
        debug_assert_eq!(epoch, sink.epoch);
        let mut binding = self
            .binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        binding.sink = Some(sink);
        binding.wire_generation = binding
            .pending
            .as_ref()
            .map(RithmicRealtimeEvent::generation);
        Ok(true)
    }

    fn apply_demand(&self, demand: &ProviderDemand) -> Result<bool, ProviderSessionError> {
        let wire = rithmic_wire(demand).map_err(|_| ProviderSessionError::InvalidDemand)?;
        try_control(&self.controls, RithmicRealtimeControl::Subscribe(wire))
    }

    fn request_history(&self, request: HistoryRequest) -> Result<bool, ProviderSessionError> {
        try_control(&self.history, request)
    }

    fn release_idle(&self) -> Result<bool, ProviderSessionError> {
        try_control(&self.controls, RithmicRealtimeControl::Stop)
    }

    fn shutdown(&self) {
        let _ = self.release_idle();
    }
}

impl RithmicBinding {
    pub(in super::super) fn take(
        &mut self,
        events: &Receiver<RithmicRealtimeEvent>,
    ) -> Option<ProviderEvent> {
        let sink = self.sink?;
        let event = match self.pending.take() {
            Some(event) => event,
            None => events.try_recv().ok()?,
        };
        let wire_generation = event.generation();
        match self.wire_generation {
            Some(current) if wire_generation < current => None,
            Some(current) if wire_generation > current => {
                self.pending = Some(event);
                Some(ProviderEvent {
                    provider: RITHMIC_DESCRIPTOR.id,
                    generation: sink.epoch,
                    wire_generation,
                    kind: ProviderEventKind::TransportReplaced,
                })
            }
            _ => {
                self.wire_generation = Some(wire_generation);
                let wire = crate::rithmic_realtime::into_wire_provider_event(event);
                Some(ProviderEvent {
                    provider: wire.provider,
                    generation: sink.epoch,
                    wire_generation,
                    kind: wire.kind,
                })
            }
        }
    }
}

pub(in super::super) fn realtime_channels(
    controls: SyncSender<RithmicRealtimeControl>,
    events: Receiver<RithmicRealtimeEvent>,
    binding: Arc<Mutex<RithmicBinding>>,
) -> ProviderRealtimeChannels {
    let session_binding = Arc::clone(&binding);
    ProviderRealtimeChannels::new(
        move |history| {
            Box::new(RithmicSession {
                controls: controls.clone(),
                history: history.clone(),
                binding: Arc::clone(&session_binding),
            })
        },
        move || {
            binding
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(&events)
        },
    )
}

pub(in super::super) fn dispatch_catalog(
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
        ProviderCatalogCommand::Scan(_) => {
            return Err(format!("{provider} does not provide a market scan"));
        }
    };
    try_send_catalog_control(controls, control, provider)
}

impl From<RithmicCatalogEvent> for ProviderCatalogEvent {
    fn from(event: RithmicCatalogEvent) -> Self {
        match event {
            RithmicCatalogEvent::SearchCompleted(result) => Self::SearchCompleted(result),
            RithmicCatalogEvent::SelectionResolved {
                consumer_id,
                command_generation,
                instrument,
            } => Self::SelectionResolved {
                consumer_id,
                command_generation,
                instrument,
            },
            RithmicCatalogEvent::Rejected {
                rejection,
                selection,
            } => Self::Rejected {
                rejection,
                selection,
            },
        }
    }
}

pub(in super::super) fn rithmic_supported_period(period: BarPeriod) -> Result<(), String> {
    aeris_rithmic_protocol_adapter::history_fetch::chart_interval(period).map(|_| ())
}

fn flush_rithmic_provider_demand(coordinator: &mut Coordinator<'_>) {
    coordinator.flush_pending_demand(RITHMIC_DESCRIPTOR.id);
}

pub(in super::super) fn start_rithmic_runtime(
    spec: ProviderRuntimeSpec,
    completions: &SyncSender<Command>,
    wake: ProviderCoordinatorWake,
    engine: &MarketEngine,
    active_workers: &Arc<Mutex<BTreeSet<String>>>,
    context: &ProviderStartContext,
) -> Result<ProviderRuntimeRecord, String> {
    let enabled = spec.realtime.enabled;
    let descriptor = spec.descriptor;
    let started =
        ProviderRuntimeRegistry::start_history_runtime(spec, completions, active_workers)?;
    start_rithmic_record(
        started,
        descriptor,
        enabled,
        context.rithmic_login.clone(),
        wake,
        engine,
        active_workers,
    )
}

fn start_rithmic_record(
    mut started: StartedProviderRuntime,
    descriptor: ProviderDescriptor,
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
    let catalog_events_tx = CatalogPublisher::new(catalog_events_tx, descriptor.id, wake.clone());
    let (catalog_controls, catalog_controls_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (realtime_events_tx, realtime_events) = mpsc::sync_channel(REALTIME_CAPACITY);
    let binding = Arc::new(Mutex::new(RithmicBinding::default()));
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
                    worker_lifecycle.mark_terminal_failure("Provider runtime stopped unexpectedly");
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
        // A history-only runtime has no realtime receiver. Do not
        // register its intentionally closed control channel as a session.
        realtime: enabled.then(|| realtime_channels(realtime_controls, realtime_events, binding)),
        display_depth: None,
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
