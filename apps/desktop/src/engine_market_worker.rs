//! Desktop-side client for engine-owned Rithmic historical and realtime bars.

use std::{
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axiusflow_application::{
    MarketEventProvenance, Provenanced, ReplayProvenance, ReplayRecoveryCommand, ReplaySnapshot,
    ReplayStreamUpdate, ReplayTailOperation, ReplayTailUpdate,
};
use axiusflow_engine_protocol::{
    ConsumerResourceClass, DemandError, EngineFaultCode, FailureStage, InstallProviderInstrument,
    ProviderConnectionState, ProviderInstrumentSummary, ProviderState, SearchProviderInstruments,
    SelectProviderInstrument, SeriesCadence, SeriesKey, SeriesLoadState, SeriesSnapshot,
    SeriesState, SeriesUpdate, SeriesUpdateOperation, WorkspacePaneKind, WorkspaceState, envelope,
};
#[cfg(test)]
use axiusflow_engine_protocol::{WorkspacePaneState, WorkspaceTabState};
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_market_data::{BarDefinition, ChartInterval, MarketBar};
use axiusflow_observability::FeedConnectionState;

use crate::desktop::engine_supervisor::EngineSupervisor;
use crate::desktop::rithmic_engine_history::{OrderBookIdentity, order_book_from_snapshot};
#[cfg(test)]
use axiusflow_desktop::market_worker::ProviderCatalogEvent;
use axiusflow_desktop::market_worker::{
    ChartState, DesktopMarketGeneration, MarketDataWorker, MarketPublicationGeneration,
    MarketWorkerBootstrap, MarketWorkerCommand, MarketWorkerMessage, MarketWorkerPublication,
    MarketWorkerSender, MarketWorkerStartup, classify_provider_catalog_event,
    market_worker_channel,
};

const DEFAULT_WORKSPACE_ID: u64 = 1;
const INITIAL_GENERATION: u64 = 1;
const STARTUP_CATALOG_RESOLUTION_TIMEOUT: Duration = Duration::from_secs(12);
const STARTUP_CATALOG_COMMAND_GENERATION: u64 = u32::MAX as u64;
const MESSAGE_CAPACITY: usize = 256;
const COMMAND_CAPACITY: usize = 32;
const RETAINED_BAR_CAPACITY: usize = 32_768;
const SUBSCRIPTION_ID: &str = "desktop_engine_rithmic_bars";
const WORKER_LABEL: &str = "Rithmic engine - history and realtime IPC";
const HYPERLIQUID_SUBSCRIPTION_ID: &str = "desktop_engine_hyperliquid_bars";
const HYPERLIQUID_WORKER_LABEL: &str = "Hyperliquid engine - history and realtime IPC";
/// Recovery notice sent when the worker observes an engine replacement.
/// The UI matches on this to reset generation-fenced views back to loading
/// instead of holding books from a dead engine incarnation.
pub(crate) const ENGINE_RESTARTED_MESSAGE: &str =
    "Resident engine restarted; restoring chart demand";
/// Foreground selections live in latest-value slots outside the IPC reader.
/// Keep the reader wait short so a symbol click cannot sit behind a half-frame
/// polling quantum before the engine receives it.
const EVENT_WAIT: Duration = Duration::from_millis(2);
/// Backoff between engine-restore retries in the event loop. Each failed
/// receive already spent up to the supervisor's restore deadline retrying,
/// so this only spaces the recovering notices, never hot-loops reconnects.
const RESTORE_BACKOFF: Duration = Duration::from_millis(250);
/// Engine events one chart drains per tick before yielding to the other charts.
const MARKET_EVENTS_PER_POLL: usize = 512;
const WORKSPACE_ADDITION_CAPACITY: usize = 8;

struct EndpointRecord {
    workspace_id: u64,
    product: InstallProviderInstrument,
    interval: ChartInterval,
    endpoint: WorkerEndpoint,
}

pub(super) struct WorkspaceMarketPane {
    pub workspace_id: u64,
    pub pane_id: u64,
    pub consumer_id: u64,
    pub startup: MarketWorkerStartup,
    pub worker: MarketDataWorker,
}

pub(super) struct WorkspaceMarketGroup {
    pub initial: Vec<WorkspaceMarketPane>,
    pub factory: WorkspaceMarketFactory,
}

#[derive(Clone)]
pub(super) struct WorkspaceMarketFactory {
    additions: mpsc::SyncSender<EndpointRecord>,
    next_workspace_id: Arc<AtomicU64>,
    next_pane_id: Arc<AtomicU64>,
    next_consumer_id: Arc<AtomicU64>,
}

impl WorkspaceMarketFactory {
    pub fn create_workspace(
        &self,
        product: InstallProviderInstrument,
        interval: ChartInterval,
    ) -> Result<WorkspaceMarketPane, String> {
        let workspace_id = allocate_workspace_id(&self.next_workspace_id)?;
        self.create_pane(workspace_id, product, interval)
    }

    pub fn create_pane(
        &self,
        workspace_id: u64,
        product: InstallProviderInstrument,
        interval: ChartInterval,
    ) -> Result<WorkspaceMarketPane, String> {
        let pane_id = allocate_pane_id(&self.next_pane_id)?;
        let consumer_id = allocate_consumer_id(&self.next_consumer_id)?;
        let (worker, endpoint) = worker_endpoint(
            workspace_id,
            pane_id,
            product,
            consumer_id,
            interval,
            None,
            INITIAL_GENERATION,
        );
        self.additions
            .try_send(endpoint)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    "workspace creation is busy; try again after the current workspace opens"
                        .to_string()
                }
                mpsc::TrySendError::Disconnected(_) => {
                    "the resident engine workspace coordinator is unavailable".to_string()
                }
            })?;
        Ok(worker)
    }
}

fn allocate_workspace_id(next_workspace_id: &AtomicU64) -> Result<u64, String> {
    next_workspace_id
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, checked_add_one)
        .map_err(|_| "workspace identity space is exhausted".to_string())
}

fn allocate_consumer_id(next_consumer_id: &AtomicU64) -> Result<u64, String> {
    next_consumer_id
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, checked_add_one)
        .map_err(|_| "market consumer identity space is exhausted".to_string())
}

fn allocate_pane_id(next_pane_id: &AtomicU64) -> Result<u64, String> {
    next_pane_id
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, checked_add_one)
        .map_err(|_| "workspace pane identity space is exhausted".to_string())
}

const fn checked_add_one(value: u64) -> Option<u64> {
    value.checked_add(1)
}

pub(super) fn start() -> Result<
    (
        MarketWorkerStartup,
        MarketDataWorker,
        WorkspaceMarketFactory,
    ),
    String,
> {
    let product = default_hyperliquid_product();
    let (mut workers, factory) = start_group(vec![(DEFAULT_WORKSPACE_ID, product)])?;
    let (startup, worker) = workers
        .pop()
        .ok_or_else(|| "Hyperliquid engine worker group is empty".to_string())?;
    Ok((startup, worker, factory))
}

pub(super) fn start_multi_chart() -> Result<Vec<(MarketWorkerStartup, MarketDataWorker)>, String> {
    let mnq = default_product("MNQ");
    let es = default_product("ES");
    let (workers, _factory) = start_group(vec![(1, mnq), (2, es)])?;
    Ok(workers)
}

pub(super) fn start_workspace_tabs(
    workspace: &WorkspaceState,
) -> Result<WorkspaceMarketGroup, String> {
    let client_id = random_order_book_identity()?;
    let mut initial = Vec::new();
    let mut endpoints = Vec::new();
    let (maximum_workspace_id, maximum_pane_id, maximum_consumer_id) =
        workspace_identity_high_watermarks(workspace);
    for tab in &workspace.workspace_tabs {
        for pane in &tab.panes {
            if WorkspacePaneKind::try_from(pane.kind).ok() != Some(WorkspacePaneKind::Chart) {
                continue;
            }
            let instrument = pane
                .instrument
                .clone()
                .ok_or_else(|| "workspace chart instrument is missing".to_string())?;
            let interval = pane_interval(pane.series.as_ref())?;
            let (worker, endpoint) = worker_endpoint(
                tab.workspace_id,
                pane.pane_id,
                instrument,
                pane.consumer_id,
                interval,
                pane.viewport_start_unix_nanos
                    .zip(pane.viewport_end_unix_nanos),
                pane.generation,
            );
            initial.push(worker);
            endpoints.push(endpoint);
        }
    }
    if initial.is_empty() {
        return Err("persisted workspace contains no chart panes".to_string());
    }
    let (addition_tx, addition_rx) = mpsc::sync_channel(WORKSPACE_ADDITION_CAPACITY);
    spawn_group(client_id, endpoints, Some(addition_rx))?;
    Ok(WorkspaceMarketGroup {
        initial,
        factory: WorkspaceMarketFactory {
            additions: addition_tx,
            next_workspace_id: Arc::new(AtomicU64::new(maximum_workspace_id.saturating_add(1))),
            next_pane_id: Arc::new(AtomicU64::new(maximum_pane_id.saturating_add(1))),
            next_consumer_id: Arc::new(AtomicU64::new(maximum_consumer_id.saturating_add(1))),
        },
    })
}

fn workspace_identity_high_watermarks(workspace: &WorkspaceState) -> (u64, u64, u64) {
    workspace.workspace_tabs.iter().fold(
        (0, 0, 0),
        |(maximum_workspace, maximum_pane, maximum_consumer), tab| {
            tab.panes.iter().fold(
                (
                    maximum_workspace.max(tab.workspace_id),
                    maximum_pane,
                    maximum_consumer,
                ),
                |(maximum_workspace, maximum_pane, maximum_consumer), pane| {
                    (
                        maximum_workspace,
                        maximum_pane.max(pane.pane_id),
                        maximum_consumer.max(pane.consumer_id),
                    )
                },
            )
        },
    )
}

fn pane_interval(series: Option<&SeriesKey>) -> Result<ChartInterval, String> {
    let series = series.ok_or_else(|| "workspace chart series is missing".to_string())?;
    match SeriesCadence::try_from(series.cadence) {
        Ok(SeriesCadence::FixedSeconds) => match series.cadence_value {
            60 => Ok(ChartInterval::Minute1),
            180 => Ok(ChartInterval::Minute3),
            300 => Ok(ChartInterval::Minute5),
            1_800 => Ok(ChartInterval::Minute30),
            900 => Ok(ChartInterval::Minute15),
            3_600 => Ok(ChartInterval::Hour1),
            7_200 => Ok(ChartInterval::Hour2),
            14_400 => Ok(ChartInterval::Hour4),
            28_800 => Ok(ChartInterval::Hour8),
            43_200 => Ok(ChartInterval::Hour12),
            86_400 => Ok(ChartInterval::Day1),
            _ => Err("workspace chart cadence is unsupported by Rithmic".to_string()),
        },
        Ok(SeriesCadence::CalendarWeeks) if series.cadence_value == 1 => Ok(ChartInterval::Week1),
        Ok(SeriesCadence::CalendarMonths) if series.cadence_value == 1 => Ok(ChartInterval::Month1),
        _ => Err("workspace chart cadence is unsupported by Rithmic".to_string()),
    }
}

struct WorkerEndpoint {
    consumer_id: u64,
    messages: MarketWorkerSender,
    commands: mpsc::Receiver<MarketWorkerCommand>,
    pending_resource_class: Arc<Mutex<Option<ConsumerResourceClass>>>,
    pending_provider_selection:
        Arc<Mutex<Option<axiusflow_engine_protocol::SelectProviderInstrument>>>,
    pending_engine_selection:
        Arc<Mutex<Option<Box<axiusflow_desktop::market_worker::EngineSelectionRequest>>>>,
    shutdown: mpsc::SyncSender<()>,
    publication: Option<MarketPublicationGeneration>,
    pending_recovery: Option<ReplayRecoveryCommand>,
    /// Set once the engine has reported this demand generation live. After that
    /// a `Partial` state is a background history repair, not a loading chart.
    live: bool,
    active_generation: u64,
    provider_state_generation: Option<u64>,
    resource_class: ConsumerResourceClass,
    active: bool,
}

fn start_group(
    configurations: Vec<(u64, InstallProviderInstrument)>,
) -> Result<
    (
        Vec<(MarketWorkerStartup, MarketDataWorker)>,
        WorkspaceMarketFactory,
    ),
    String,
> {
    let client_id = random_order_book_identity()?;
    let mut workers = Vec::with_capacity(configurations.len());
    let mut endpoints = Vec::with_capacity(configurations.len());
    let mut maximum_workspace_id = 0;
    let mut maximum_pane_id = 1;
    let mut maximum_consumer_id = 0;
    for (workspace_id, product) in configurations {
        let consumer_id = random_order_book_identity()?;
        let pane_id = random_order_book_identity()?;
        maximum_workspace_id = maximum_workspace_id.max(workspace_id);
        maximum_pane_id = maximum_pane_id.max(pane_id);
        maximum_consumer_id = maximum_consumer_id.max(consumer_id);
        let (worker, endpoint) = worker_endpoint(
            workspace_id,
            pane_id,
            product,
            consumer_id,
            ChartInterval::Minute1,
            None,
            INITIAL_GENERATION,
        );
        workers.push((worker.startup, worker.worker));
        endpoints.push(endpoint);
    }
    let (addition_tx, addition_rx) = mpsc::sync_channel(WORKSPACE_ADDITION_CAPACITY);
    spawn_group(client_id, endpoints, Some(addition_rx))?;
    Ok((
        workers,
        WorkspaceMarketFactory {
            additions: addition_tx,
            next_workspace_id: Arc::new(AtomicU64::new(maximum_workspace_id.saturating_add(1))),
            next_pane_id: Arc::new(AtomicU64::new(maximum_pane_id.saturating_add(1))),
            next_consumer_id: Arc::new(AtomicU64::new(maximum_consumer_id.saturating_add(1))),
        },
    ))
}

fn worker_endpoint(
    workspace_id: u64,
    pane_id: u64,
    product: InstallProviderInstrument,
    consumer_id: u64,
    interval: ChartInterval,
    restored_viewport: Option<(i64, i64)>,
    initial_generation: u64,
) -> (WorkspaceMarketPane, EndpointRecord) {
    let (subscription_id, worker_label) = worker_identity(product.provider.as_str());
    let startup = MarketWorkerStartup::Loading(Box::new(
        axiusflow_desktop::market_worker::EngineWorkerStartup {
            product: product.clone(),
            interval,
            restored_viewport,
            subscription_id: subscription_id.to_string(),
            worker_label: worker_label.to_string(),
        },
    ));
    let (message_tx, message_rx) =
        market_worker_channel(NonZeroUsize::new(MESSAGE_CAPACITY).unwrap_or(NonZeroUsize::MIN));
    let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
    let selection_sequence = Arc::new(AtomicU64::new(initial_generation));
    let pending_resource_class = Arc::new(Mutex::new(None));
    let pending_provider_selection = Arc::new(Mutex::new(None));
    let pending_engine_selection = Arc::new(Mutex::new(None));
    let worker = WorkspaceMarketPane {
        workspace_id,
        pane_id,
        consumer_id,
        startup,
        worker: MarketDataWorker::from_channels(
            command_tx,
            message_rx,
            shutdown_rx,
            None,
            Some(Arc::clone(&selection_sequence)),
        )
        .with_resource_class_slot(Arc::clone(&pending_resource_class))
        .with_foreground_selection_slots(
            Arc::clone(&pending_provider_selection),
            Arc::clone(&pending_engine_selection),
        ),
    };
    let endpoint = EndpointRecord {
        workspace_id,
        product,
        interval,
        endpoint: WorkerEndpoint {
            consumer_id,
            messages: message_tx,
            commands: command_rx,
            pending_resource_class,
            pending_provider_selection,
            pending_engine_selection,
            shutdown: shutdown_tx,
            publication: None,
            pending_recovery: None,
            live: false,
            active_generation: initial_generation,
            provider_state_generation: None,
            resource_class: ConsumerResourceClass::Foreground,
            active: true,
        },
    };
    (worker, endpoint)
}

fn spawn_group(
    client_id: u64,
    mut endpoints: Vec<EndpointRecord>,
    additions: Option<mpsc::Receiver<EndpointRecord>>,
) -> Result<(), String> {
    // One supervisor owns every pane in this workspace group. Tab selection and
    // layout edits only change endpoint resource classes or membership; provider
    // sessions remain resident-engine owned and are never created per pane.
    thread::Builder::new()
        .name("axiusflow-engine-market-client".to_string())
        .spawn(move || {
            if let Err(error) = run_workers(client_id, &mut endpoints, additions) {
                for record in &endpoints {
                    let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
                        state: ChartState::Error,
                        message: error.clone(),
                    });
                }
            }
            for record in endpoints {
                let _ = record.endpoint.shutdown.try_send(());
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// User-visible provider name for worker status and error messages.
fn provider_display_name(provider: &str) -> &str {
    match provider {
        "rithmic" => "Rithmic",
        "hyperliquid" => "Hyperliquid",
        _ => provider,
    }
}

/// Identity every pushed engine event is checked against.
struct PushedEventContext<'a> {
    consumer_id: u64,
    active_generation: u64,
    realtime: bool,
    instrument: &'a InstallProviderInstrument,
}

fn worker_identity(provider: &str) -> (&'static str, &'static str) {
    if provider == "hyperliquid" {
        (HYPERLIQUID_SUBSCRIPTION_ID, HYPERLIQUID_WORKER_LABEL)
    } else {
        (SUBSCRIPTION_ID, WORKER_LABEL)
    }
}

#[path = "engine_market_worker/replay_conversion.rs"]
mod replay_conversion;
use replay_conversion::{
    generation_from_snapshot, replay_snapshot, replay_tail_update, series_key, tail_publication,
};
#[cfg(test)]
use replay_conversion::{replay_bar_definition, snapshot_instrument};

fn default_product(product_id: &str) -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: "rithmic".to_string(),
        session_generation: 1,
        selection_generation: 1,
        instrument_id: format!("instrument:rithmic:CME:{product_id}"),
        provider_symbol: product_id.to_string(),
        display_symbol: product_id.to_string(),
        venue_id: "CME".to_string(),
        price_scale: 2,
        quantity_scale: 0,
        entitlement_id: format!("rithmic-test:CME:{product_id}"),
    }
}

/// Fresh-install default: Hyperliquid BTC perpetual, one-minute candles.
fn default_hyperliquid_product() -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: "hyperliquid".to_string(),
        session_generation: 1,
        selection_generation: 1,
        instrument_id: "hyperliquid:perp:BTC".to_string(),
        provider_symbol: "BTC".to_string(),
        display_symbol: "BTC-PERP".to_string(),
        venue_id: "Hyperliquid".to_string(),
        price_scale: 8,
        quantity_scale: 8,
        entitlement_id: "hyperliquid-public".to_string(),
    }
}

#[cfg(test)]
fn products() -> Vec<InstallProviderInstrument> {
    ["MNQ", "ES"].into_iter().map(default_product).collect()
}

fn demand_error(error: &DemandError) -> String {
    let class = EngineFaultCode::try_from(error.code).map_or("unknown", |code| match code {
        EngineFaultCode::Retryable => "retryable",
        EngineFaultCode::Offline => "offline",
        EngineFaultCode::Cancelled => "cancelled",
        EngineFaultCode::Permanent => "permanent",
        EngineFaultCode::CorruptLocalState => "corrupt local state",
        EngineFaultCode::Unauthenticated => "unauthenticated",
        EngineFaultCode::VersionMismatch => "version mismatch",
        EngineFaultCode::Backpressure => "backpressure",
        EngineFaultCode::MalformedMessage => "malformed message",
        EngineFaultCode::OversizedFrame => "oversized frame",
    });
    let stage = match FailureStage::try_from(error.stage_code) {
        Ok(stage) => failure_stage_label(stage),
        Err(_) => error.stage.as_str(),
    };
    let elapsed = error
        .elapsed_millis
        .map_or(String::new(), |elapsed| format!(" after {elapsed} ms"));
    format!("{stage} failed ({class}){elapsed}: {}", error.detail)
}

const fn failure_stage_label(stage: FailureStage) -> &'static str {
    match stage {
        FailureStage::Unspecified => "market demand",
        FailureStage::ProviderHistory => "provider history",
        FailureStage::CanonicalValidation => "canonical validation",
        FailureStage::MemoryInstall => "memory install",
        FailureStage::Aggregation => "aggregation",
        FailureStage::SegmentEncode => "segment encode",
        FailureStage::Encryption => "encryption",
        FailureStage::FilesystemWrite => "filesystem write",
        FailureStage::CatalogCommit => "catalog commit",
        FailureStage::Handoff => "history/live handoff",
        FailureStage::Publication => "publication",
        FailureStage::IpcSend => "local IPC send",
        FailureStage::ChartInstall => "chart install",
        FailureStage::ProviderRealtime => "provider realtime",
    }
}

fn now_unix_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(i64::MAX)
}

fn random_order_book_identity() -> Result<u64, String> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_err(|_| "system CSPRNG is unavailable".to_string())?;
    Ok(NonZeroU64::new(u64::from_le_bytes(bytes))
        .unwrap_or(NonZeroU64::MIN)
        .get())
}

#[path = "engine_market_worker/runtime.rs"]
mod runtime;
#[cfg(test)]
use runtime::note_engine_restore_failure;
use runtime::{retire_endpoint, run_workers};

#[path = "engine_market_worker/selection_commands.rs"]
mod selection_commands;
use selection_commands::{initialize_endpoint, process_command, set_resource_class};

#[path = "engine_market_worker/publications.rs"]
mod publications;
#[cfg(test)]
use publications::{
    apply_provider_state, apply_realtime_demand_error, apply_series_state, send_publication,
};
use publications::{
    apply_pushed_event, cancel_pending_recovery, complete_pending_recovery, send_recovery,
};

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_chart_integration::{NucleusChartTheme, NucleusChartView};
    use axiusflow_engine_protocol::{
        MarketBar as IpcMarketBar, OrderBookLevel as IpcOrderBookLevel,
        OrderBookSnapshot as IpcOrderBookSnapshot, OrderBookState as IpcOrderBookState,
        ProviderInstrumentSearchResult, ProviderInstrumentSelection, ProviderInstrumentSummary,
        SelectProviderInstrument,
    };

    fn handle_rithmic_catalog_event(
        endpoint: &mut WorkerEndpoint,
        event: envelope::Payload,
    ) -> Option<envelope::Payload> {
        let (catalog, event) =
            classify_provider_catalog_event(event, "rithmic", endpoint.consumer_id);
        match catalog {
            Some(event) => {
                let _ = endpoint
                    .messages
                    .send(MarketWorkerMessage::ProviderCatalog(event));
                None
            }
            None => event,
        }
    }

    #[test]
    fn workspace_creation_allocates_one_consumer_per_tab_without_identity_wraparound() {
        let next_workspace_id = AtomicU64::new(2);
        let next_pane_id = AtomicU64::new(9);
        assert_eq!(allocate_workspace_id(&next_workspace_id), Ok(2));
        assert_eq!(allocate_workspace_id(&next_workspace_id), Ok(3));
        let exhausted = AtomicU64::new(u64::MAX);
        assert!(allocate_workspace_id(&exhausted).is_err());
        assert!(allocate_consumer_id(&exhausted).is_err());
        assert_eq!(allocate_pane_id(&next_pane_id), Ok(9));

        let product = products().remove(0);
        let (_worker, record) =
            worker_endpoint(2, 9, product.clone(), 41, ChartInterval::Minute5, None, 7);
        assert_eq!(record.workspace_id, 2);
        assert_eq!(record.endpoint.consumer_id, 41);
        assert_eq!(record.product.instrument_id, product.instrument_id);
        assert_eq!(record.interval, ChartInterval::Minute5);
    }

    #[test]
    fn workspace_endpoint_wires_foreground_selection_slots() {
        let product = default_hyperliquid_product();
        let (pane, record) =
            worker_endpoint(2, 9, product.clone(), 41, ChartInterval::Minute1, None, 7);
        let provider_selection = SelectProviderInstrument {
            consumer_id: 0,
            selection_generation: 3,
            search_generation: 2,
            provider: "hyperliquid".to_string(),
            symbol: "ETH".to_string(),
            exchange: "Hyperliquid".to_string(),
            entitlement_id: "hyperliquid-public".to_string(),
        };
        pane.worker
            .try_select_provider(provider_selection.clone())
            .expect("provider selection enters foreground slot");
        assert_eq!(
            record
                .endpoint
                .pending_provider_selection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref(),
            Some(&provider_selection)
        );

        assert_eq!(
            pane.worker
                .try_select_engine(product, ChartInterval::Minute5)
                .expect("engine selection enters foreground slot"),
            8
        );
        let pending_engine = record
            .endpoint
            .pending_engine_selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("engine selection is retained");
        assert_eq!(pending_engine.sequence, 8);
        assert_eq!(pending_engine.interval, ChartInterval::Minute5);
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires the optimized resident engine and live Hyperliquid public access"]
    fn native_resident_hyperliquid_startup_resolves_stale_default_generation() {
        let (_startup, mut worker, factory) = start().expect("start resident Hyperliquid worker");
        drop(factory);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let (messages, disconnected) = worker.drain_messages();
            for message in messages {
                match message {
                    MarketWorkerMessage::Update(MarketWorkerPublication {
                        update: ReplayStreamUpdate::Snapshot(snapshot),
                        ..
                    }) => {
                        assert!(!snapshot.bars().is_empty());
                        return;
                    }
                    MarketWorkerMessage::State {
                        state: ChartState::Error,
                        message,
                    } => panic!("resident Hyperliquid startup failed: {message}"),
                    _ => {}
                }
            }
            assert!(
                !disconnected,
                "resident Hyperliquid startup worker disconnected"
            );
            assert!(
                Instant::now() < deadline,
                "resident Hyperliquid startup did not publish a covering snapshot"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn rithmic_worker_startup_routes_catalog_events() {
        let product = products().remove(0);
        let (mut pane, mut record) =
            worker_endpoint(2, 9, product.clone(), 41, ChartInterval::Minute5, None, 7);
        let MarketWorkerStartup::Loading(startup) = &pane.startup else {
            panic!("Rithmic endpoint must start in loading state");
        };
        assert_eq!(startup.product, product);
        assert_eq!(startup.interval, ChartInterval::Minute5);

        assert!(
            handle_rithmic_catalog_event(
                &mut record.endpoint,
                envelope::Payload::ProviderInstrumentSearchResult(ProviderInstrumentSearchResult {
                    consumer_id: 41,
                    provider: "rithmic".to_string(),
                    provider_generation: 1,
                    search_generation: 3,
                    instruments: vec![ProviderInstrumentSummary {
                        symbol: "MNQ".to_string(),
                        exchange: "CME".to_string(),
                        ..ProviderInstrumentSummary::default()
                    }],
                },),
            )
            .is_none()
        );
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::ProviderCatalog(
                ProviderCatalogEvent::SearchCompleted(result)
            )] if result.search_generation == 3
        ));

        let mut selected = product.clone();
        selected.selection_generation = 5;
        assert!(
            handle_rithmic_catalog_event(
                &mut record.endpoint,
                envelope::Payload::ProviderInstrumentSelection(ProviderInstrumentSelection {
                    consumer_id: 41,
                    instrument: Some(selected),
                    command_generation: 4,
                }),
            )
            .is_none()
        );
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::ProviderCatalog(
                ProviderCatalogEvent::SelectionInstalled {
                    command_generation: 4,
                    instrument,
                }
            )] if instrument.selection_generation == 5
        ));

        drop(record);
    }

    #[test]
    fn restored_workspace_ids_remain_stable_and_new_ids_begin_above_every_high_watermark() {
        let workspace = WorkspaceState {
            workspace_tabs: vec![
                WorkspaceTabState {
                    workspace_id: 9,
                    panes: vec![WorkspacePaneState {
                        pane_id: 21,
                        consumer_id: 31,
                        ..WorkspacePaneState::default()
                    }],
                    ..WorkspaceTabState::default()
                },
                WorkspaceTabState {
                    workspace_id: 4,
                    panes: vec![WorkspacePaneState {
                        pane_id: 17,
                        consumer_id: 44,
                        ..WorkspacePaneState::default()
                    }],
                    ..WorkspaceTabState::default()
                },
            ],
            ..WorkspaceState::default()
        };
        assert_eq!(workspace_identity_high_watermarks(&workspace), (9, 21, 44));
        assert_eq!(allocate_workspace_id(&AtomicU64::new(10)), Ok(10));
        assert_eq!(allocate_pane_id(&AtomicU64::new(22)), Ok(22));
        assert_eq!(allocate_consumer_id(&AtomicU64::new(45)), Ok(45));
        assert_eq!(workspace.workspace_tabs[0].panes[0].consumer_id, 31);
        assert_eq!(workspace.workspace_tabs[1].panes[0].consumer_id, 44);
    }

    #[test]
    fn ipc_snapshot_preserves_fixed_point_precision_and_engine_provenance() {
        let snapshot = replay_snapshot(&SeriesSnapshot {
            consumer_id: 1,
            generation: 1,
            series: Some(
                series_key(
                    products().first().expect("BTC product"),
                    ChartInterval::Minute1,
                )
                .expect("series"),
            ),
            provider_generation: 7,
            price_scale: 2,
            quantity_scale: 8,
            bars: vec![IpcMarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 60,
                exchange_timestamp_unix_nanos: 60_123_456_000,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            }],
            publication_generation: 1,
            forming: false,
        })
        .expect("snapshot converts");
        assert_eq!(snapshot.instrument().precision.price_scale(), 2);
        assert_eq!(snapshot.instrument().precision.quantity_scale(), 8);
        assert_eq!(snapshot.evidence().session_generation, 7);
        assert_eq!(snapshot.bars()[0].provenance().producer, "axiusflow_engine");
    }

    #[test]
    fn ipc_live_update_preserves_one_tail_without_rebuilding_history() {
        let update = replay_tail_update(&SeriesUpdate {
            consumer_id: 1,
            generation: 2,
            series: Some(
                series_key(
                    products().first().expect("BTC product"),
                    ChartInterval::Minute1,
                )
                .expect("series"),
            ),
            provider_generation: 7,
            bar: Some(IpcMarketBar {
                source_sequence: 3,
                exchange_timestamp_seconds: 120,
                exchange_timestamp_unix_nanos: 120_000_000_000,
                open: 100,
                high: 120,
                low: 90,
                close: 115,
                volume: 9,
            }),
            forming: true,
            publication_generation: 8,
            operation: SeriesUpdateOperation::AppendTail as i32,
        })
        .expect("tail converts");
        assert_eq!(update.item().value().source_sequence, 3);
        assert_eq!(update.item().value().close, 115);
        assert_eq!(update.publication_generation(), 8);
        assert!(update.forming());
    }

    fn drained_states(
        receiver: &axiusflow_desktop::market_worker::MarketWorkerReceiver,
    ) -> Vec<(ChartState, String)> {
        receiver
            .drain()
            .0
            .into_iter()
            .filter_map(|message| match message {
                MarketWorkerMessage::State { state, message } => Some((state, message)),
                _ => None,
            })
            .collect()
    }

    /// The engine says outright that a series serving retained local history is
    /// not ready. Swallowing that is what showed a stale chart as current.
    #[test]
    fn retained_partial_history_presents_as_loading_until_the_series_goes_live() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(16).unwrap_or(NonZeroUsize::MIN));
        let mut live = false;
        let state = |load_state: SeriesLoadState, detail: Option<&str>| SeriesState {
            consumer_id: 1,
            generation: 1,
            series: None,
            state: load_state as i32,
            persistence: 0,
            detail: detail.map(str::to_string),
        };

        apply_series_state(
            state(
                SeriesLoadState::Partial,
                Some("Showing retained local history while provider coverage repairs"),
            ),
            "rithmic",
            true,
            true,
            &mut live,
            &sender,
        )
        .expect("a partial series is not a failure");
        assert_eq!(
            drained_states(&receiver),
            vec![(
                ChartState::Loading,
                "Showing retained local history while provider coverage repairs".to_string()
            )],
            "retained history has to read as loading, with the engine's own reason"
        );

        apply_series_state(
            state(SeriesLoadState::Ready, None),
            "rithmic",
            true,
            true,
            &mut live,
            &sender,
        )
        .expect("provider history can precede the live handoff");
        assert_eq!(
            drained_states(&receiver),
            vec![(
                ChartState::Loading,
                "Rithmic history is loaded; connecting the live edge".to_string()
            )],
            "the replacement stays covered until the trade handoff is live"
        );

        apply_series_state(
            state(SeriesLoadState::Live, None),
            "rithmic",
            true,
            true,
            &mut live,
            &sender,
        )
        .expect("the handoff completes");
        assert!(live, "the series is live from here on");
        assert_eq!(
            drained_states(&receiver)
                .into_iter()
                .map(|(state, _)| state)
                .collect::<Vec<_>>(),
            vec![ChartState::Ready]
        );

        // The same state now means a backfill repairing history behind a chart
        // that is streaming. Reporting that as loading would flap the surface on
        // every viewport repair.
        apply_series_state(
            state(SeriesLoadState::Partial, Some("visible coverage repairs")),
            "rithmic",
            true,
            true,
            &mut live,
            &sender,
        )
        .expect("a backfill is not a failure");
        assert!(
            drained_states(&receiver).is_empty(),
            "a repair behind a live chart must not put it back into loading"
        );
    }

    /// The engine republishes a covering snapshot whenever it repairs coverage,
    /// so the client is routinely handed one it is already past. That is not a
    /// failure, and failing on it put an error over a chart that was streaming.
    #[test]
    fn covering_snapshots_are_forwarded_to_the_chart_validator() {
        let series = series_key(
            products().first().expect("BTC product"),
            ChartInterval::Minute1,
        )
        .expect("series");
        let (sender, _receiver) =
            market_worker_channel(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
        let mut publication = None;
        let mut live = false;
        let snapshot = |publication_generation: u64, sequence: u64| SeriesSnapshot {
            consumer_id: 1,
            generation: 1,
            series: Some(series.clone()),
            provider_generation: 7,
            price_scale: 2,
            quantity_scale: 8,
            bars: vec![IpcMarketBar {
                source_sequence: sequence,
                exchange_timestamp_seconds: i64::try_from(sequence).expect("sequence") * 60,
                exchange_timestamp_unix_nanos: i64::try_from(sequence).expect("sequence")
                    * 60_000_000_000,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            }],
            publication_generation,
            forming: false,
        };
        let context = PushedEventContext {
            consumer_id: 1,
            active_generation: 1,
            realtime: true,
            instrument: &default_product("MNQ"),
        };

        assert_eq!(
            apply_pushed_event(
                envelope::Payload::SeriesSnapshot(snapshot(4, 2)),
                &context,
                &mut publication,
                &mut live,
                &sender,
            ),
            Ok(())
        );
        // The thin client forwards covering images. The chart bridge owns stale
        // publication rejection rather than a second model in this worker.
        for superseded in [snapshot(1, 2), snapshot(4, 1)] {
            assert_eq!(
                apply_pushed_event(
                    envelope::Payload::SeriesSnapshot(superseded),
                    &context,
                    &mut publication,
                    &mut live,
                    &sender,
                ),
                Ok(()),
                "a superseded covering snapshot must not take the chart down"
            );
        }
        assert_eq!(
            publication.map(MarketPublicationGeneration::sequence_range),
            Some((1, 1))
        );
    }

    #[test]
    fn recovery_completes_from_pushed_snapshot_without_blocking_the_shared_reader() {
        let product = default_hyperliquid_product();
        let (mut pane, mut record) =
            worker_endpoint(1, 2, product.clone(), 41, ChartInterval::Minute1, None, 3);
        record.endpoint.pending_recovery = Some(ReplayRecoveryCommand {
            request_id: 9,
            reason: axiusflow_application::ResnapshotReason::QueueOverflow,
        });
        let snapshot = envelope::Payload::SeriesSnapshot(SeriesSnapshot {
            consumer_id: 41,
            generation: 3,
            series: Some(series_key(&product, ChartInterval::Minute1).expect("series")),
            provider_generation: product.session_generation,
            price_scale: product.price_scale,
            quantity_scale: product.quantity_scale,
            bars: vec![IpcMarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 60,
                exchange_timestamp_unix_nanos: 60_000_000_000,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            }],
            publication_generation: 1,
            forming: false,
        });

        assert!(
            complete_pending_recovery(&snapshot, &product, &mut record.endpoint)
                .expect("pushed recovery completes")
        );
        assert!(record.endpoint.pending_recovery.is_none());
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Recovery {
                request_id: 9,
                result: Ok(bootstrap),
            }] if bootstrap.generation.publication_generation() == 1
        ));
    }

    #[test]
    fn engine_selection_cancels_old_recovery_before_new_snapshot_can_complete_it() {
        let product = default_hyperliquid_product();
        let (mut pane, mut record) =
            worker_endpoint(1, 2, product.clone(), 41, ChartInterval::Minute1, None, 3);
        record.endpoint.pending_recovery = Some(ReplayRecoveryCommand {
            request_id: 9,
            reason: axiusflow_application::ResnapshotReason::QueueOverflow,
        });

        cancel_pending_recovery(
            &mut record.endpoint,
            "market recovery was superseded by a new market selection",
        )
        .expect("selection cancels old recovery");
        record.endpoint.active_generation = 4;
        let snapshot = envelope::Payload::SeriesSnapshot(SeriesSnapshot {
            consumer_id: 41,
            generation: 4,
            series: Some(series_key(&product, ChartInterval::Minute1).expect("series")),
            provider_generation: product.session_generation,
            price_scale: product.price_scale,
            quantity_scale: product.quantity_scale,
            bars: vec![IpcMarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 60,
                exchange_timestamp_unix_nanos: 60_000_000_000,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            }],
            publication_generation: 1,
            forming: false,
        });

        assert!(
            !complete_pending_recovery(&snapshot, &product, &mut record.endpoint)
                .expect("new snapshot is not consumed by old recovery")
        );
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Recovery {
                request_id: 9,
                result: Err(error),
            }] if error == "market recovery was superseded by a new market selection"
        ));
    }

    #[test]
    fn non_contiguous_engine_tail_is_left_for_the_chart_validator() {
        let series = series_key(
            products().first().expect("BTC product"),
            ChartInterval::Minute1,
        )
        .expect("series");
        let (sender, _receiver) =
            market_worker_channel(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
        let mut publication = None;
        let mut live = false;
        let snapshot = SeriesSnapshot {
            consumer_id: 1,
            generation: 1,
            series: Some(series.clone()),
            provider_generation: 7,
            price_scale: 2,
            quantity_scale: 8,
            bars: vec![IpcMarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 60,
                exchange_timestamp_unix_nanos: 60_000_000_000,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            }],
            publication_generation: 1,
            forming: false,
        };
        assert_eq!(
            apply_pushed_event(
                envelope::Payload::SeriesSnapshot(snapshot),
                &PushedEventContext {
                    consumer_id: 1,
                    active_generation: 1,
                    realtime: true,
                    instrument: &default_product("MNQ"),
                },
                &mut publication,
                &mut live,
                &sender,
            ),
            Ok(())
        );
        let skipped_tail = SeriesUpdate {
            consumer_id: 1,
            generation: 1,
            series: Some(series),
            provider_generation: 7,
            bar: Some(IpcMarketBar {
                source_sequence: 3,
                exchange_timestamp_seconds: 180,
                exchange_timestamp_unix_nanos: 180_000_000_000,
                open: 105,
                high: 120,
                low: 100,
                close: 115,
                volume: 9,
            }),
            forming: true,
            publication_generation: 2,
            operation: SeriesUpdateOperation::AppendTail as i32,
        };
        assert_eq!(
            apply_pushed_event(
                envelope::Payload::SeriesUpdate(skipped_tail),
                &PushedEventContext {
                    consumer_id: 1,
                    active_generation: 1,
                    realtime: true,
                    instrument: &default_product("MNQ"),
                },
                &mut publication,
                &mut live,
                &sender,
            ),
            Ok(())
        );
        assert_eq!(
            publication.map(MarketPublicationGeneration::sequence_range),
            Some((1, 3))
        );
    }

    /// The whole desktop path, driven deterministically: the worker's pushed-event translation
    /// the real bounded mailbox, and a real chart bridge.
    ///
    /// The desktop used to poll one engine event per 16ms frame, so anything the
    /// engine produced faster than 62 events a second backed up and was coalesced
    /// away. The chart read the resulting sequence gaps as corruption and asked
    /// for a covering snapshot, over and over — the stop/start the maintainer
    /// reported. Every distinct bar must survive a burst larger than one frame,
    /// and the bridge must never ask to be resnapshotted.
    #[test]
    fn a_burst_larger_than_one_frame_reaches_the_chart_bridge_without_recovery() {
        /// Comfortably more than one frame's drain, and more than three seconds
        /// of the old one-event-per-16ms budget.
        const BURST: u64 = 200;
        let product = default_product("MNQ");
        let series = series_key(&product, ChartInterval::Minute1).expect("series");
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(MESSAGE_CAPACITY).unwrap_or(NonZeroUsize::MIN));
        let mut publication = None;
        let mut live = false;
        let context = PushedEventContext {
            consumer_id: 1,
            active_generation: 1,
            realtime: true,
            instrument: &product,
        };

        assert_eq!(
            apply_pushed_event(
                envelope::Payload::SeriesSnapshot(SeriesSnapshot {
                    consumer_id: 1,
                    generation: 1,
                    series: Some(series.clone()),
                    provider_generation: 7,
                    price_scale: 2,
                    quantity_scale: 8,
                    bars: vec![burst_bar(1)],
                    publication_generation: 1,
                    forming: false,
                }),
                &context,
                &mut publication,
                &mut live,
                &sender,
            ),
            Ok(())
        );
        for sequence in 2..=BURST {
            assert_eq!(
                apply_pushed_event(
                    envelope::Payload::SeriesUpdate(SeriesUpdate {
                        consumer_id: 1,
                        generation: 1,
                        series: Some(series.clone()),
                        provider_generation: 7,
                        bar: Some(burst_bar(sequence)),
                        forming: sequence == BURST,
                        publication_generation: sequence,
                        operation: SeriesUpdateOperation::AppendTail as i32,
                    }),
                    &context,
                    &mut publication,
                    &mut live,
                    &sender,
                ),
                Ok(()),
                "bar {sequence} must apply without a resnapshot"
            );
        }

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        let (chart, delivered) = replay_into_a_chart(&messages);

        assert_eq!(
            delivered,
            (1..=BURST).collect::<Vec<_>>(),
            "the burst lost or reordered a bar between the engine and the chart"
        );
        assert!(
            !chart
                .expect("a chart was built")
                .replay_bridge_metrics()
                .recovery_pending,
            "an unbroken burst must never leave the bridge asking for a resnapshot"
        );
    }

    /// Feeds drained mailbox messages into a real chart bridge, one frame at a
    /// time, and reports every bar sequence that reached it.
    ///
    /// The UI drains a bounded number of messages per frame and the chart applies
    /// what it was handed before painting, so the drain is replayed the same way
    /// here: the bridge queue is bounded, and a host that never applies would
    /// fill it.
    fn replay_into_a_chart(
        messages: &[MarketWorkerMessage],
    ) -> (Option<NucleusChartView>, Vec<u64>) {
        const MARKET_MESSAGES_PER_FRAME: usize = 64;
        let mut chart = None;
        let mut delivered = Vec::new();
        for frame in messages.chunks(MARKET_MESSAGES_PER_FRAME) {
            for message in frame {
                let MarketWorkerMessage::Update(publication) = message else {
                    continue;
                };
                match &publication.update {
                    ReplayStreamUpdate::Snapshot(snapshot) => {
                        delivered.extend(
                            snapshot
                                .bars()
                                .iter()
                                .map(|bar| bar.value().source_sequence),
                        );
                        chart = Some(NucleusChartView::with_replay_and_theme(
                            snapshot,
                            NucleusChartTheme::Dark,
                        ));
                    }
                    update => {
                        if let ReplayStreamUpdate::Tail(tail) = update {
                            delivered.push(tail.item().value().source_sequence);
                        }
                        chart
                            .as_mut()
                            .expect("the covering snapshot arrives first")
                            .try_queue_replay_update(update.clone())
                            .expect("the chart bridge accepts every queued bar");
                    }
                }
            }
            if let Some(chart) = chart.as_mut() {
                chart.apply_queued_replay_updates();
            }
        }
        (chart, delivered)
    }

    fn burst_bar(sequence: u64) -> IpcMarketBar {
        let seconds = i64::try_from(sequence).unwrap_or(i64::MAX) * 60;
        IpcMarketBar {
            source_sequence: sequence,
            exchange_timestamp_seconds: seconds,
            exchange_timestamp_unix_nanos: seconds * 1_000_000_000,
            open: 100,
            high: 120,
            low: 90,
            close: 100 + i64::try_from(sequence % 17).unwrap_or(0),
            volume: 9,
        }
    }

    #[test]
    fn live_state_without_covering_snapshot_is_rejected() {
        let (sender, _receiver) = market_worker_channel(NonZeroUsize::MIN);
        let mut publication = None;
        let mut live = false;

        let result = apply_pushed_event(
            envelope::Payload::SeriesState(SeriesState {
                consumer_id: 1,
                generation: 7,
                state: SeriesLoadState::Live as i32,
                ..SeriesState::default()
            }),
            &PushedEventContext {
                consumer_id: 1,
                active_generation: 7,
                realtime: true,
                instrument: &default_product("MNQ"),
            },
            &mut publication,
            &mut live,
            &sender,
        );

        assert_eq!(
            result,
            Err("engine marked history live without a covering snapshot".to_string())
        );
    }

    #[test]
    fn engine_recovery_state_remains_explicit_while_history_is_retained() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        apply_provider_state(
            &ProviderState {
                provider: "rithmic".to_string(),
                state: ProviderConnectionState::Recovering as i32,
                generation: 2,
                detail: None,
                transport_rtt_nanos: None,
            },
            "rithmic",
            true,
            &sender,
        )
        .expect("provider state applies");
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Connection {
                state: FeedConnectionState::Recovering,
                message,
                transport_rtt_nanos: None,
            }] if message.contains("retained history")
        ));
    }

    #[test]
    fn newer_provider_session_reaches_the_connection_presentation() {
        let product = default_product("MNQ");
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let mut publication = None;
        let mut live = false;
        apply_pushed_event(
            envelope::Payload::ProviderState(ProviderState {
                provider: product.provider.clone(),
                state: ProviderConnectionState::Online as i32,
                generation: product.session_generation + 1,
                detail: None,
                transport_rtt_nanos: None,
            }),
            &PushedEventContext {
                consumer_id: 1,
                active_generation: 7,
                realtime: true,
                instrument: &product,
            },
            &mut publication,
            &mut live,
            &sender,
        )
        .expect("new engine-owned provider session");
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Connection {
                state: FeedConnectionState::Streaming,
                ..
            }]
        ));
    }

    #[test]
    fn engine_online_provider_state_forwards_measured_transport_rtt() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        apply_provider_state(
            &ProviderState {
                provider: "hyperliquid".to_string(),
                state: ProviderConnectionState::Online as i32,
                generation: 4,
                detail: None,
                transport_rtt_nanos: Some(12_500_000),
            },
            "hyperliquid",
            true,
            &sender,
        )
        .expect("provider state applies");
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Connection {
                state: FeedConnectionState::Streaming,
                transport_rtt_nanos: Some(12_500_000),
                ..
            }]
        ));
    }

    #[test]
    fn engine_restore_failure_stays_recovering_without_killing_the_worker() {
        let product = products().remove(0);
        let (mut live_pane, live_record) =
            worker_endpoint(2, 9, product.clone(), 41, ChartInterval::Minute5, None, 7);
        let (mut dead_pane, mut dead_record) =
            worker_endpoint(2, 10, product.clone(), 42, ChartInterval::Minute5, None, 7);
        dead_record.endpoint.active = false;
        let records = vec![live_record, dead_record];

        note_engine_restore_failure(
            &records,
            "resident engine connection failed: ipc_receive failed: local engine connection closed; recovery failed: sign in before using the Axiusflow platform",
        );

        let (messages, disconnected) = live_pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Connection {
                state: FeedConnectionState::Recovering,
                message,
                transport_rtt_nanos: None,
            }] if message.contains("sign in before using the Axiusflow platform")
        ));
        // Retired endpoints never observe the failure.
        let (messages, _) = dead_pane.worker.drain_messages();
        assert!(messages.is_empty());
        drop(records);
    }

    #[test]
    fn phase_four_series_keys_cover_required_symbols_and_intervals() {
        let products = vec![default_product("MNQ"), default_product("ES")];
        for product in &products {
            for (interval, seconds) in [
                (ChartInterval::Minute1, 60),
                (ChartInterval::Minute3, 180),
                (ChartInterval::Minute5, 300),
                (ChartInterval::Minute15, 900),
                (ChartInterval::Minute30, 1_800),
                (ChartInterval::Hour1, 3_600),
                (ChartInterval::Hour2, 7_200),
                (ChartInterval::Hour4, 14_400),
                (ChartInterval::Hour8, 28_800),
                (ChartInterval::Hour12, 43_200),
                (ChartInterval::Day1, 86_400),
            ] {
                let series = series_key(product, interval).expect("phase-four series validates");
                assert_eq!(series.cadence_value, seconds);
                assert_eq!(series.instrument_id, product.instrument_id);
            }
            for (interval, cadence, value) in [
                (ChartInterval::Week1, SeriesCadence::CalendarWeeks, 1),
                (ChartInterval::Month1, SeriesCadence::CalendarMonths, 1),
            ] {
                let series = series_key(product, interval).expect("calendar series validates");
                assert_eq!(series.cadence, cadence as i32);
                assert_eq!(series.cadence_value, value);
            }
        }
    }

    #[test]
    fn calendar_month_replay_is_explicit_instead_of_approximated_as_thirty_days() {
        let product = default_product("MNQ");
        let week = replay_bar_definition(
            &series_key(&product, ChartInterval::Week1).expect("week series"),
        )
        .expect("week definition");
        let month = replay_bar_definition(
            &series_key(&product, ChartInterval::Month1).expect("month series"),
        )
        .expect("calendar month definition");

        assert!(week.definition_id.ends_with("calendar-weeks:1"));
        assert_eq!(week.interval_seconds, 7 * 24 * 60 * 60);
        assert!(month.definition_id.ends_with("calendar-months:1"));
        assert_eq!(month.interval_seconds, 0);
        assert_eq!(month.trades_per_bar, None);
        assert_eq!(month.calendar_months, Some(1));
        assert!(month.validate().is_ok());
    }

    #[test]
    fn calendar_snapshot_connects_to_the_shared_realtime_session() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        apply_provider_state(
            &ProviderState {
                provider: "rithmic".to_string(),
                state: ProviderConnectionState::Connecting as i32,
                generation: 1,
                detail: None,
                transport_rtt_nanos: None,
            },
            "rithmic",
            true,
            &sender,
        )
        .expect("calendar provider state is forwarded");
        assert!(matches!(
            receiver.drain().0.as_slice(),
            [MarketWorkerMessage::Connection {
                state: FeedConnectionState::Discovering,
                ..
            }]
        ));
    }

    #[test]
    fn calendar_series_accepts_the_engine_live_handoff() {
        let product = default_product("MNQ");
        let series = series_key(&product, ChartInterval::Week1).expect("calendar series");
        let snapshot = SeriesSnapshot {
            consumer_id: 1,
            generation: 7,
            series: Some(series),
            provider_generation: 1,
            price_scale: 2,
            quantity_scale: 8,
            bars: vec![IpcMarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 0,
                exchange_timestamp_unix_nanos: 0,
                open: 100,
                high: 100,
                low: 100,
                close: 100,
                volume: 1,
            }],
            publication_generation: 1,
            forming: false,
        };
        let (sender, _receiver) = market_worker_channel(NonZeroUsize::MIN);
        let mut publication = None;
        let mut live = false;
        apply_pushed_event(
            envelope::Payload::SeriesSnapshot(snapshot),
            &PushedEventContext {
                consumer_id: 1,
                active_generation: 7,
                realtime: true,
                instrument: &default_product("MNQ"),
            },
            &mut publication,
            &mut live,
            &sender,
        )
        .expect("calendar snapshot applies");

        apply_pushed_event(
            envelope::Payload::SeriesState(SeriesState {
                consumer_id: 1,
                generation: 7,
                state: SeriesLoadState::Live as i32,
                ..SeriesState::default()
            }),
            &PushedEventContext {
                consumer_id: 1,
                active_generation: 7,
                realtime: true,
                instrument: &default_product("MNQ"),
            },
            &mut publication,
            &mut live,
            &sender,
        )
        .expect("calendar live state applies");
        assert!(live);
    }

    #[test]
    fn replacement_engine_resets_application_generation_fence() {
        let product = default_product("MNQ");
        let series = series_key(&product, ChartInterval::Minute1).expect("series");
        let snapshot = |provider_generation, publication_generation, source_sequence| {
            envelope::Payload::SeriesSnapshot(SeriesSnapshot {
                consumer_id: 1,
                generation: 1,
                series: Some(series.clone()),
                provider_generation,
                price_scale: 2,
                quantity_scale: 8,
                bars: vec![IpcMarketBar {
                    source_sequence,
                    exchange_timestamp_seconds: i64::try_from(source_sequence).unwrap_or(i64::MAX),
                    exchange_timestamp_unix_nanos: i64::try_from(source_sequence)
                        .unwrap_or(i64::MAX)
                        .saturating_mul(1_000_000_000),
                    open: 100,
                    high: 110,
                    low: 90,
                    close: 105,
                    volume: 7,
                }],
                publication_generation,
                forming: false,
            })
        };
        let (sender, _receiver) = market_worker_channel(NonZeroUsize::new(4).unwrap());
        let mut publication = None;
        let mut live = false;
        assert_eq!(
            apply_pushed_event(
                snapshot(9, 12, 12),
                &PushedEventContext {
                    consumer_id: 1,
                    active_generation: 1,
                    realtime: true,
                    instrument: &default_product("MNQ"),
                },
                &mut publication,
                &mut live,
                &sender,
            ),
            Ok(())
        );

        publication = None;

        assert_eq!(
            apply_pushed_event(
                snapshot(1, 1, 1),
                &PushedEventContext {
                    consumer_id: 1,
                    active_generation: 1,
                    realtime: true,
                    instrument: &default_product("MNQ"),
                },
                &mut publication,
                &mut live,
                &sender,
            ),
            Ok(())
        );
        assert_eq!(
            publication.map(MarketPublicationGeneration::publication_generation),
            Some(1)
        );
    }

    #[test]
    fn rithmic_series_keys_reject_conflicting_provider_metadata() {
        // A Hyperliquid provider claiming a Rithmic instrument identity (and
        // vice versa) must fail here instead of misrouting demand.
        let mut product = products().remove(0);
        product.provider = "hyperliquid".to_string();
        assert!(series_key(&product, ChartInterval::Minute1).is_err());
        let mut product = products().remove(0);
        product.instrument_id = "hyperliquid:perp:BTC".to_string();
        assert!(series_key(&product, ChartInterval::Minute1).is_err());
    }

    #[test]
    fn structured_demand_errors_render_stage_and_elapsed_context() {
        let error = DemandError {
            consumer_id: 1,
            generation: 2,
            code: EngineFaultCode::Retryable as i32,
            stage: "handoff".to_string(),
            detail: "history/live handoff failed".to_string(),
            series: None,
            stage_code: FailureStage::Handoff as i32,
            cause: "history and realtime state could not be joined safely".to_string(),
            elapsed_millis: Some(17),
        };
        assert_eq!(
            demand_error(&error),
            "history/live handoff failed (retryable) after 17 ms: history/live handoff failed"
        );
    }

    #[test]
    fn retryable_demand_error_recovers_without_making_the_chart_unavailable() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let retryable = DemandError {
            consumer_id: 1,
            generation: 2,
            code: EngineFaultCode::Retryable as i32,
            stage: "provider history".to_string(),
            detail: "covering history is retrying".to_string(),
            series: None,
            stage_code: FailureStage::ProviderHistory as i32,
            cause: "provider history was unavailable".to_string(),
            elapsed_millis: Some(4),
        };

        assert_eq!(
            apply_realtime_demand_error(&retryable, 1, 2, &sender),
            Ok(())
        );
        assert_eq!(
            drained_states(&receiver),
            vec![(
                ChartState::Recovering,
                "provider history failed (retryable) after 4 ms: covering history is retrying"
                    .to_string(),
            )]
        );

        let permanent = DemandError {
            code: EngineFaultCode::Permanent as i32,
            detail: "permanent fixture failure".to_string(),
            elapsed_millis: None,
            ..retryable
        };
        assert!(
            apply_realtime_demand_error(&permanent, 1, 2, &sender)
                .is_err_and(|detail| detail.contains("permanent fixture failure"))
        );
    }

    /// The Rithmic Order Book panel stayed empty because the engine's order-book
    /// snapshot had no arm here: `OrderBook` was declared, coalesced, and
    /// rendered, but never constructed. Levels are a real MNQ top-of-book fixture
    /// with accurate Rithmic integer-contract volumes.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn rithmic_order_book_snapshot_uses_the_consumers_selection_identity() {
        let product = default_product("MNQ");
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
        let mut publication = None;
        let mut live = false;

        let outcome = apply_pushed_event(
            envelope::Payload::OrderBookSnapshot(IpcOrderBookSnapshot {
                consumer_id: 1,
                generation: 1,
                provider: "rithmic".to_string(),
                instrument_id: product.instrument_id.clone(),
                entitlement_id: product.entitlement_id.clone(),
                provider_generation: 1,
                // The engine book is shared by instrument, so this may carry
                // the generation of another chart that selected BTC/USD.
                selection_generation: 99,
                revision: 1,
                source_watermark: 1,
                state: IpcOrderBookState::Ready as i32,
                bids: vec![
                    IpcOrderBookLevel {
                        price: 7_798_670,
                        quantity: 653_408,
                        order_count: None,
                        traded_volume: 125,
                    },
                    IpcOrderBookLevel {
                        price: 7_798_514,
                        quantity: 2_564_592,
                        order_count: None,
                        traded_volume: 0,
                    },
                ],
                asks: vec![
                    IpcOrderBookLevel {
                        price: 7_798_671,
                        quantity: 22_517_771,
                        order_count: None,
                        traded_volume: 75,
                    },
                    IpcOrderBookLevel {
                        price: 7_798_727,
                        quantity: 4_582_685,
                        order_count: None,
                        traded_volume: 0,
                    },
                ],
                best_bid: Some(IpcOrderBookLevel {
                    price: 7_798_670,
                    quantity: 653_408,
                    order_count: Some(3),
                    traded_volume: 125,
                }),
                best_ask: Some(IpcOrderBookLevel {
                    price: 7_798_671,
                    quantity: 22_517_771,
                    order_count: Some(4),
                    traded_volume: 75,
                }),
                bbo_source_watermark: 2,
            }),
            &PushedEventContext {
                consumer_id: 1,
                active_generation: 1,
                realtime: true,
                instrument: &product,
            },
            &mut publication,
            &mut live,
            &sender,
        );

        assert_eq!(outcome, Ok(()));
        let (messages, _) = receiver.drain();
        let frame = messages
            .into_iter()
            .find_map(|message| match message {
                MarketWorkerMessage::OrderBook(frame) => Some(frame),
                _ => None,
            })
            .expect("Rithmic depth must reach the Order Book panel");
        assert!(
            !frame.rows.is_empty(),
            "a projected Rithmic Order Book frame must carry price rows"
        );
        assert_eq!(frame.selection_generation, product.selection_generation);
        assert_eq!(
            frame.best_bid.as_ref().map(|level| level.price),
            Some(7_798_670)
        );
        assert_eq!(
            frame.best_ask.as_ref().map(|level| level.price),
            Some(7_798_671)
        );
        assert_eq!(
            frame.rows[0]
                .bid
                .as_ref()
                .map(|level| level.traded_volume_text.as_str()),
            Some("125")
        );
        assert_eq!(
            frame.rows[0]
                .ask
                .as_ref()
                .map(|level| level.traded_volume_text.as_str()),
            Some("75")
        );
    }

    #[test]
    fn hyperliquid_series_keys_cover_supported_intervals() {
        let product = default_hyperliquid_product();
        for (interval, seconds) in [
            (ChartInterval::Minute1, 60),
            (ChartInterval::Minute5, 300),
            (ChartInterval::Hour1, 3_600),
            (ChartInterval::Day1, 86_400),
        ] {
            let series = series_key(&product, interval).expect("HL series validates");
            assert_eq!(series.provider, "hyperliquid");
            assert_eq!(series.cadence_value, seconds);
            assert_eq!(series.instrument_id, product.instrument_id);
            assert_eq!(series.entitlement_id, "hyperliquid-public");
        }
        // Native 3-day candles exist on Hyperliquid but not on Rithmic.
        let day3 = series_key(&product, ChartInterval::Day3).expect("HL day3 series");
        assert_eq!(day3.cadence, SeriesCadence::SessionDays as i32);
        // Tick candles exist on neither public path.
        assert!(series_key(&product, ChartInterval::Tick100).is_err());
        // Rithmic intervals stay Rithmic-only.
        assert!(series_key(&default_product("MNQ"), ChartInterval::Day3).is_err());
    }

    #[test]
    fn snapshot_instrument_parses_all_hyperliquid_identities() {
        let perp = SeriesKey {
            provider: "hyperliquid".to_string(),
            instrument_id: "hyperliquid:perp:BTC".to_string(),
            cadence_value: 60,
            definition_revision: 1,
            entitlement_id: "hyperliquid-public".to_string(),
            cadence: SeriesCadence::FixedSeconds as i32,
        };
        assert_eq!(
            snapshot_instrument(&perp).expect("perp parses"),
            (
                "Hyperliquid".to_string(),
                "BTC".to_string(),
                AssetClass::Future,
                "USDC".to_string()
            )
        );
        let spot = SeriesKey {
            instrument_id: "hyperliquid:spot:7:HFUN/USDC".to_string(),
            ..perp.clone()
        };
        assert_eq!(
            snapshot_instrument(&spot).expect("spot parses"),
            (
                "Hyperliquid Spot".to_string(),
                "HFUN/USDC".to_string(),
                AssetClass::CryptoAsset,
                "USDC".to_string()
            )
        );
        let builder = SeriesKey {
            instrument_id: "hyperliquid:builder:xyz:TSLA".to_string(),
            ..perp.clone()
        };
        assert_eq!(
            snapshot_instrument(&builder).expect("builder parses"),
            (
                "xyz".to_string(),
                "xyz:TSLA".to_string(),
                AssetClass::Future,
                "USDC".to_string()
            )
        );
        // Misrouted or truncated identities fail closed.
        for bad in [
            "hyperliquid:perp:BTC:EXTRA",
            "hyperliquid:spot:7:HFUNUSDC",
            "hyperliquid:spot:HFUN/USDC",
            "hyperliquid:builder:TSLA",
            "instrument:rithmic:CME:MNQ",
            "hyperliquid:",
            "",
        ] {
            let mut series = perp.clone();
            series.instrument_id = bad.to_string();
            assert!(
                snapshot_instrument(&series).is_err(),
                "{bad} must not parse"
            );
        }
    }

    #[test]
    fn hyperliquid_snapshot_converts_with_eight_place_precision() {
        let product = default_hyperliquid_product();
        let series = series_key(&product, ChartInterval::Minute1).expect("HL series");
        let snapshot = replay_snapshot(&SeriesSnapshot {
            consumer_id: 1,
            generation: 1,
            series: Some(series),
            provider_generation: 7,
            price_scale: 8,
            quantity_scale: 8,
            bars: vec![IpcMarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 60,
                exchange_timestamp_unix_nanos: 60_000_000_000,
                open: 6_700_050_000_000,
                high: 6_700_100_000_000,
                low: 6_699_900_000_000,
                close: 6_700_075_000_000,
                volume: 98_639_000,
            }],
            publication_generation: 1,
            forming: false,
        })
        .expect("HL snapshot converts");
        assert_eq!(snapshot.instrument().precision.price_scale(), 8);
        assert_eq!(snapshot.instrument().precision.quantity_scale(), 8);
        assert_eq!(snapshot.instrument().symbol, "BTC");
    }

    #[test]
    fn hyperliquid_publication_uses_hyperliquid_worker_identity() {
        let product = default_hyperliquid_product();
        let snapshot = SeriesSnapshot {
            consumer_id: 1,
            generation: 1,
            series: Some(series_key(&product, ChartInterval::Minute1).expect("HL series")),
            provider_generation: 7,
            price_scale: 8,
            quantity_scale: 8,
            bars: vec![IpcMarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 60,
                exchange_timestamp_unix_nanos: 60_000_000_000,
                open: 6_700_050_000_000,
                high: 6_700_100_000_000,
                low: 6_699_900_000_000,
                close: 6_700_075_000_000,
                volume: 98_639_000,
            }],
            publication_generation: 1,
            forming: false,
        };
        let replay = replay_snapshot(&snapshot).expect("HL snapshot converts");
        let generation = MarketPublicationGeneration::from_generation(
            &generation_from_snapshot(&snapshot, &replay).expect("generation validates"),
        );
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);

        send_publication(
            &sender,
            ReplayStreamUpdate::Snapshot(replay),
            generation,
            "hyperliquid",
        )
        .expect("publication queues");

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Update(publication)]
                if publication.subscription_id == HYPERLIQUID_SUBSCRIPTION_ID
                    && publication.worker_label == HYPERLIQUID_WORKER_LABEL
        ));
    }

    #[test]
    fn provider_state_mismatch_is_rejected_per_endpoint() {
        let (sender, _receiver) = market_worker_channel(NonZeroUsize::MIN);
        // A Hyperliquid state update on a Rithmic endpoint misroutes.
        assert!(
            apply_provider_state(
                &ProviderState {
                    provider: "hyperliquid".to_string(),
                    state: ProviderConnectionState::Online as i32,
                    generation: 1,
                    detail: None,
                    transport_rtt_nanos: Some(12_000_000),
                },
                "rithmic",
                true,
                &sender,
            )
            .is_err()
        );
        // And the reverse misroutes identically.
        assert!(
            apply_provider_state(
                &ProviderState {
                    provider: "rithmic".to_string(),
                    state: ProviderConnectionState::Online as i32,
                    generation: 1,
                    detail: None,
                    transport_rtt_nanos: Some(12_000_000),
                },
                "hyperliquid",
                true,
                &sender,
            )
            .is_err()
        );
    }
}
