//! Desktop-side client for engine-owned Coinbase historical and realtime bars.

use std::{
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axiusflow_application::{
    MarketEventProvenance, Provenanced, ReplayProvenance, ReplayRecoveryCommand, ReplaySnapshot,
    ReplayStreamUpdate, ReplayTailOperation, ReplayTailUpdate,
};
use axiusflow_engine_protocol::{
    ConsumerResourceClass, DemandError, EngineFaultCode, FailureStage, InstallProviderInstrument,
    ProviderConnectionState, ProviderState, SearchProviderInstruments, SeriesCadence, SeriesKey,
    SeriesLoadState, SeriesSnapshot, SeriesState, SeriesUpdate, SeriesUpdateOperation,
    WorkspacePaneKind, WorkspaceState, envelope,
};
#[cfg(test)]
use axiusflow_engine_protocol::{WorkspacePaneState, WorkspaceTabState};
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_market_data::{BarDefinition, ChartInterval, MarketBar};
use axiusflow_observability::FeedConnectionState;

use crate::engine_supervisor::EngineSupervisor;
use crate::rithmic_engine_history::{DomIdentity, dom_from_snapshot};
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
const MESSAGE_CAPACITY: usize = 256;
const COMMAND_CAPACITY: usize = 32;
const RETAINED_BAR_CAPACITY: usize = 32_768;
const SUBSCRIPTION_ID: &str = "desktop_engine_coinbase_bars";
const WORKER_LABEL: &str = "Coinbase engine - history and realtime IPC";
const EVENT_WAIT: Duration = Duration::from_millis(8);
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
    let product = default_coinbase_product("BTC-USD");
    let (mut workers, factory) = start_group(vec![(DEFAULT_WORKSPACE_ID, product)])?;
    let (startup, worker) = workers
        .pop()
        .ok_or_else(|| "Coinbase engine worker group is empty".to_string())?;
    Ok((startup, worker, factory))
}

pub(super) fn start_multi_chart() -> Result<Vec<(MarketWorkerStartup, MarketDataWorker)>, String> {
    let btc = default_coinbase_product("BTC-USD");
    let eth = default_coinbase_product("ETH-USD");
    let (workers, _factory) = start_group(vec![(1, btc), (2, eth)])?;
    Ok(workers)
}

pub(super) fn start_workspace_tabs(
    workspace: &WorkspaceState,
) -> Result<WorkspaceMarketGroup, String> {
    let client_id = random_identity()?;
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
            _ => Err("workspace chart cadence is unsupported by Coinbase".to_string()),
        },
        Ok(SeriesCadence::CalendarWeeks) if series.cadence_value == 1 => Ok(ChartInterval::Week1),
        Ok(SeriesCadence::CalendarMonths) if series.cadence_value == 1 => Ok(ChartInterval::Month1),
        _ => Err("workspace chart cadence is unsupported by Coinbase".to_string()),
    }
}

struct WorkerEndpoint {
    consumer_id: u64,
    messages: MarketWorkerSender,
    commands: mpsc::Receiver<MarketWorkerCommand>,
    pending_resource_class: Arc<Mutex<Option<ConsumerResourceClass>>>,
    shutdown: mpsc::SyncSender<()>,
    publication: Option<MarketPublicationGeneration>,
    /// Set once the engine has reported this demand generation live. After that
    /// a `Partial` state is a background history repair, not a loading chart.
    live: bool,
    active_generation: u64,
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
    let client_id = random_identity()?;
    let mut workers = Vec::with_capacity(configurations.len());
    let mut endpoints = Vec::with_capacity(configurations.len());
    let mut maximum_workspace_id = 0;
    let mut maximum_pane_id = 1;
    let mut maximum_consumer_id = 0;
    for (workspace_id, product) in configurations {
        let consumer_id = random_identity()?;
        let pane_id = random_identity()?;
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
    let startup = MarketWorkerStartup::Loading(Box::new(
        axiusflow_desktop::market_worker::CoinbaseWorkerStartup {
            coinbase_product: product.clone(),
            coinbase_interval: interval,
            restored_viewport,
            subscription_id: SUBSCRIPTION_ID.to_string(),
            worker_label: WORKER_LABEL.to_string(),
        },
    ));
    let (message_tx, message_rx) =
        market_worker_channel(NonZeroUsize::new(MESSAGE_CAPACITY).unwrap_or(NonZeroUsize::MIN));
    let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
    let selection_sequence = Arc::new(AtomicU64::new(initial_generation));
    let pending_resource_class = Arc::new(Mutex::new(None));
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
            Some(selection_sequence),
        )
        .with_resource_class_slot(Arc::clone(&pending_resource_class)),
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
            shutdown: shutdown_tx,
            publication: None,
            live: false,
            active_generation: initial_generation,
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

fn run_workers(
    client_id: u64,
    endpoints: &mut Vec<EndpointRecord>,
    additions: Option<mpsc::Receiver<EndpointRecord>>,
) -> Result<(), String> {
    for record in endpoints.iter() {
        let _ = record
            .endpoint
            .messages
            .send(MarketWorkerMessage::Connection {
                state: FeedConnectionState::Discovering,
                message: "Connecting to the resident market engine".to_string(),
            });
    }
    let mut supervisor = EngineSupervisor::connect(client_id)?;
    let result = run_attached_workers(&mut supervisor, endpoints, additions);
    for record in endpoints.iter_mut().filter(|record| record.endpoint.active) {
        let _ = supervisor.remove_market_consumer(record.endpoint.consumer_id);
        record.endpoint.active = false;
        let _ = record.endpoint.shutdown.try_send(());
    }
    let detach_result = supervisor.detach();
    result.and(detach_result)
}

fn run_attached_workers(
    client: &mut EngineSupervisor,
    endpoints: &mut Vec<EndpointRecord>,
    additions: Option<mpsc::Receiver<EndpointRecord>>,
) -> Result<(), String> {
    for record in endpoints.iter_mut() {
        initialize_endpoint(
            client,
            record.workspace_id,
            &record.product,
            record.interval,
            &mut record.endpoint,
        )?;
    }

    let mut additions = additions;
    while additions.is_some() || endpoints.iter().any(|record| record.endpoint.active) {
        if let Some(receiver) = additions.as_ref() {
            loop {
                match receiver.try_recv() {
                    Ok(mut record) => {
                        match initialize_endpoint(
                            client,
                            record.workspace_id,
                            &record.product,
                            record.interval,
                            &mut record.endpoint,
                        ) {
                            Ok(()) => endpoints.push(record),
                            Err(error) => {
                                let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
                                    state: ChartState::Error,
                                    message: error,
                                });
                                retire_endpoint(client, &mut record.endpoint);
                            }
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        additions = None;
                        break;
                    }
                }
            }
        }
        for record in endpoints.iter_mut().filter(|record| record.endpoint.active) {
            process_pending_resource_class(client, &mut record.endpoint);
            match record.endpoint.commands.try_recv() {
                Ok(command) => {
                    if let Err(error) = process_command(client, record, command) {
                        let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
                            state: ChartState::Error,
                            message: error,
                        });
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    retire_endpoint(client, &mut record.endpoint);
                }
            }
        }
        endpoints.retain(|record| record.endpoint.active);
        if receive_and_apply_event(client, endpoints, EVENT_WAIT)? {
            for _ in 1..MARKET_EVENTS_PER_POLL {
                if !receive_and_apply_event(client, endpoints, Duration::ZERO)? {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// Receives and applies one pushed engine event. Returns whether one arrived.
fn receive_and_apply_event(
    client: &mut EngineSupervisor,
    endpoints: &mut [EndpointRecord],
    timeout: Duration,
) -> Result<bool, String> {
    let received = client.receive_market_event(timeout)?;
    if received.reconnected {
        for endpoint in endpoints
            .iter_mut()
            .filter(|record| record.endpoint.active)
            .map(|record| &mut record.endpoint)
        {
            endpoint.publication = None;
            let _ = endpoint.messages.send(MarketWorkerMessage::Connection {
                state: FeedConnectionState::Recovering,
                message: "Resident engine restarted; restoring chart demand".to_string(),
            });
        }
        return Ok(true);
    }
    let (Some(consumer_id), Some(event)) = (received.consumer_id, received.event) else {
        return Ok(false);
    };
    if consumer_id == 0 {
        let envelope::Payload::Fault(fault) = event else {
            return Err("engine pushed an unrouted market message".to_string());
        };
        for endpoint in endpoints
            .iter()
            .filter(|record| record.endpoint.active)
            .map(|record| &record.endpoint)
        {
            let _ = endpoint.messages.send(MarketWorkerMessage::State {
                state: ChartState::Error,
                message: fault.redacted_detail.clone(),
            });
        }
        return Ok(true);
    }
    let Some(record) = endpoints
        .iter_mut()
        .find(|record| record.endpoint.active && record.endpoint.consumer_id == consumer_id)
    else {
        return Ok(true);
    };
    let endpoint = &mut record.endpoint;
    let (catalog, event) = classify_provider_catalog_event(event, "coinbase", endpoint.consumer_id);
    let event = match catalog {
        Some(event) => {
            let _ = endpoint
                .messages
                .send(MarketWorkerMessage::ProviderCatalog(event));
            return Ok(true);
        }
        None => event,
    };
    let Some(event) = event else { return Ok(true) };
    let result = apply_pushed_event(
        event,
        &PushedEventContext {
            consumer_id: endpoint.consumer_id,
            active_generation: endpoint.active_generation,
            realtime: true,
            instrument: &record.product,
        },
        &mut endpoint.publication,
        &mut endpoint.live,
        &endpoint.messages,
    );
    if let Err(error) = result {
        let _ = endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Error,
            message: error,
        });
    }
    Ok(true)
}

fn process_pending_resource_class(client: &mut EngineSupervisor, endpoint: &mut WorkerEndpoint) {
    let pending = endpoint
        .pending_resource_class
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(resource_class) = pending
        && let Err(error) = set_resource_class(client, endpoint, resource_class)
    {
        let _ = endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Error,
            message: error,
        });
    }
}

fn initialize_endpoint(
    client: &mut EngineSupervisor,
    workspace_id: u64,
    product: &InstallProviderInstrument,
    interval: ChartInterval,
    endpoint: &mut WorkerEndpoint,
) -> Result<(), String> {
    let _ = endpoint.messages.send(MarketWorkerMessage::Connection {
        state: FeedConnectionState::Discovering,
        message: "Connecting to the resident market engine".to_string(),
    });
    client.register_consumer(workspace_id, endpoint.consumer_id)?;
    client.install_provider_instrument(product.clone())?;
    match request_snapshot(
        client,
        endpoint.consumer_id,
        endpoint.active_generation,
        series_key(product, interval)?,
        &endpoint.messages,
    ) {
        Ok((snapshot, generation)) => {
            let publication = MarketPublicationGeneration::from_generation(&generation);
            send_publication(
                &endpoint.messages,
                ReplayStreamUpdate::Snapshot(snapshot),
                publication,
            )?;
            endpoint.publication = Some(publication);
            let (state, message) = snapshot_connection_state(interval);
            let _ = endpoint.messages.send(MarketWorkerMessage::Connection {
                state,
                message: message.to_string(),
            });
        }
        Err(error) => {
            let _ = endpoint.messages.send(MarketWorkerMessage::State {
                state: ChartState::Error,
                message: error,
            });
        }
    }
    client.search_provider_instruments(SearchProviderInstruments {
        consumer_id: endpoint.consumer_id,
        search_generation: 1,
        provider: "coinbase".to_string(),
        query: String::new(),
        maximum_results: u32::try_from(crate::rithmic_shell::MAXIMUM_COINBASE_SYMBOL_RESULTS)
            .unwrap_or(u32::MAX),
    })?;
    Ok(())
}

fn process_command(
    client: &mut EngineSupervisor,
    record: &mut EndpointRecord,
    command: MarketWorkerCommand,
) -> Result<(), String> {
    let EndpointRecord {
        product,
        interval,
        endpoint,
        ..
    } = record;
    match command {
        MarketWorkerCommand::ProviderSearch(mut request) => {
            if request.provider != "coinbase" {
                return Err("unsupported provider catalog command".to_string());
            }
            request.consumer_id = endpoint.consumer_id;
            client.search_provider_instruments(request)
        }
        MarketWorkerCommand::ProviderSelect(mut request) => {
            if request.provider != "coinbase" {
                return Err("unsupported provider catalog command".to_string());
            }
            request.consumer_id = endpoint.consumer_id;
            client.select_provider_instrument(request)
        }
        MarketWorkerCommand::CoinbaseSelect(request) => {
            let series = match series_key(&request.product, request.interval) {
                Ok(series) => series,
                Err(error) => {
                    let _ = endpoint.messages.send(MarketWorkerMessage::State {
                        state: ChartState::Error,
                        message: error,
                    });
                    return Ok(());
                }
            };
            let _ = endpoint
                .messages
                .send(MarketWorkerMessage::CoinbaseSwitchMarker {
                    sequence: request.sequence,
                });
            let _ = endpoint.messages.send(MarketWorkerMessage::State {
                state: ChartState::Loading,
                message: "Loading Coinbase history through the resident engine".to_string(),
            });
            endpoint.publication = None;
            endpoint.live = false;
            endpoint.active_generation = request.sequence;
            product.clone_from(&request.product);
            *interval = request.interval;
            client.set_series_demand(endpoint.consumer_id, request.sequence, series)
        }
        MarketWorkerCommand::Recovery(command) => {
            send_recovery(client, product, *interval, endpoint, command)
        }
        MarketWorkerCommand::ChartViewport(viewport) => {
            if viewport.selection_generation > 0 {
                client.set_market_viewport(
                    endpoint.consumer_id,
                    viewport.selection_generation,
                    viewport.start_unix_nanos,
                    viewport.end_unix_nanos,
                )?;
            }
            Ok(())
        }
        MarketWorkerCommand::ResourceClass(resource_class) => {
            set_resource_class(client, endpoint, resource_class)
        }
        MarketWorkerCommand::Shutdown => {
            retire_endpoint(client, endpoint);
            Ok(())
        }
        MarketWorkerCommand::EngineSeries(_) => {
            Err("Rithmic commands cannot enter the Coinbase engine client".to_string())
        }
    }
}

fn set_resource_class(
    client: &mut EngineSupervisor,
    endpoint: &mut WorkerEndpoint,
    resource_class: ConsumerResourceClass,
) -> Result<(), String> {
    client.set_market_resource_class(endpoint.consumer_id, resource_class)?;
    endpoint.resource_class = resource_class;
    Ok(())
}

fn retire_endpoint(client: &mut EngineSupervisor, endpoint: &mut WorkerEndpoint) {
    let _ = client.remove_market_consumer(endpoint.consumer_id);
    endpoint.active = false;
    let _ = endpoint.shutdown.try_send(());
}

/// Identity every pushed engine event is checked against.
struct PushedEventContext<'a> {
    consumer_id: u64,
    active_generation: u64,
    realtime: bool,
    instrument: &'a InstallProviderInstrument,
}

/// Applies one series-readiness transition, reporting a live handoff to the UI.
/// Turns the engine's load state into the state the chart presents.
///
/// The engine is explicit that a series serving retained local history is
/// `Partial`, not ready. Dropping that on the floor is what showed a stale chart
/// as current for the seconds before provider coverage landed, and then jumped.
/// Once the series has gone live the same `Partial` means something else — a
/// backfill repairing history behind a chart that is streaming — so it stops
/// being a loading state at that point.
fn apply_series_state(
    state: SeriesState,
    realtime: bool,
    published: bool,
    live: &mut bool,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    let load_state = SeriesLoadState::try_from(state.state)
        .map_err(|_| "engine returned an invalid realtime state".to_string())?;
    let announce = |chart_state: ChartState, message: String| {
        messages
            .send(MarketWorkerMessage::State {
                state: chart_state,
                message,
            })
            .map_err(|error| error.to_string())
    };
    match load_state {
        SeriesLoadState::Live => {
            if !realtime {
                return Err("engine marked a Coinbase calendar-history series live".to_string());
            }
            if !published {
                return Err("engine marked history live without a covering snapshot".to_string());
            }
            *live = true;
            announce(
                ChartState::Ready,
                "Coinbase history/live handoff is current".to_string(),
            )?;
            Ok(())
        }
        SeriesLoadState::Failed => Err(state
            .detail
            .unwrap_or_else(|| "Coinbase realtime failed".to_string())),
        SeriesLoadState::Ready if !published => {
            Err("engine marked history ready without a covering snapshot".to_string())
        }
        // Provider history is installed; a realtime series is still loading
        // until its trade handoff promotes it to Live. Revealing it at Ready
        // exposes the history/live seam as a stalled or disconnected chart.
        SeriesLoadState::Ready if realtime => {
            announce(
                ChartState::Loading,
                "Coinbase history is loaded; connecting the live edge".to_string(),
            )?;
            Ok(())
        }
        SeriesLoadState::Ready => {
            announce(
                ChartState::Ready,
                "Coinbase provider history is current".to_string(),
            )?;
            Ok(())
        }
        SeriesLoadState::Resolving | SeriesLoadState::Partial if !*live => {
            announce(
                ChartState::Loading,
                state.detail.unwrap_or_else(|| {
                    "Resident engine is loading current Coinbase coverage".to_string()
                }),
            )?;
            Ok(())
        }
        SeriesLoadState::Empty
        | SeriesLoadState::Resolving
        | SeriesLoadState::Partial
        | SeriesLoadState::Superseded => Ok(()),
    }
}

fn apply_pushed_event(
    event: envelope::Payload,
    context: &PushedEventContext<'_>,
    publication: &mut Option<MarketPublicationGeneration>,
    live: &mut bool,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    let &PushedEventContext {
        consumer_id,
        active_generation,
        realtime,
        instrument,
    } = context;
    match event {
        envelope::Payload::SeriesSnapshot(snapshot) => {
            if snapshot.consumer_id != consumer_id || snapshot.generation != active_generation {
                return Err("engine realtime snapshot identity mismatched".to_string());
            }
            let replay = replay_snapshot(&snapshot)?;
            let generation = generation_from_snapshot(&snapshot, &replay)?;
            let status = MarketPublicationGeneration::from_generation(&generation);
            *publication = Some(status);
            send_publication(messages, ReplayStreamUpdate::Snapshot(replay), status)?;
            Ok(())
        }
        envelope::Payload::SeriesUpdate(update) => {
            if update.consumer_id != consumer_id || update.generation != active_generation {
                return Err("engine realtime update identity mismatched".to_string());
            }
            let tail = replay_tail_update(&update)?;
            let status = tail_publication(
                publication.ok_or_else(|| {
                    "engine sent a Coinbase update before a covering snapshot".to_string()
                })?,
                &tail,
            );
            *publication = Some(status);
            send_publication(messages, ReplayStreamUpdate::Tail(tail), status)
        }
        envelope::Payload::ProviderState(state) => {
            apply_provider_state(&state, realtime, messages)?;
            Ok(())
        }
        envelope::Payload::SeriesState(state) => {
            if state.consumer_id != consumer_id || state.generation != active_generation {
                return Err("engine realtime state identity mismatched".to_string());
            }
            apply_series_state(state, realtime, publication.is_some(), live, messages)
        }
        envelope::Payload::DemandError(error) => Err(demand_error(&error)),
        envelope::Payload::OrderBookSnapshot(snapshot) => {
            if snapshot.consumer_id != consumer_id {
                return Err("engine order-book consumer mismatched".to_string());
            }
            if snapshot.provider_generation < instrument.session_generation {
                return Ok(());
            }
            let Ok(frame) = dom_from_snapshot(
                &DomIdentity {
                    instrument,
                    series_generation: active_generation,
                },
                &snapshot,
            ) else {
                // Depth is an ancillary stream. A stale or malformed book
                // image must never transition the price chart into a fatal
                // state; retain the last valid DOM frame and wait for the next
                // canonical snapshot.
                return Ok(());
            };
            messages
                .send(MarketWorkerMessage::CoinbaseDom(frame))
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        envelope::Payload::OrderFlowSnapshot(_) | envelope::Payload::OrderFlowUpdate(_) => Ok(()),
        envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
        _ => Err("engine returned an unexpected pushed market event".to_string()),
    }
}

fn apply_provider_state(
    state: &ProviderState,
    realtime: bool,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    if state.provider != "coinbase" {
        return Err("engine provider state identity mismatched".to_string());
    }
    if !realtime {
        return Ok(());
    }
    let provider_state = ProviderConnectionState::try_from(state.state)
        .map_err(|_| "engine returned an invalid provider state".to_string())?;
    let (connection, detail) = match provider_state {
        ProviderConnectionState::Disconnected => (
            FeedConnectionState::Disconnected,
            "Coinbase realtime is disconnected",
        ),
        ProviderConnectionState::Connecting => (
            FeedConnectionState::Discovering,
            "Coinbase realtime is connecting",
        ),
        ProviderConnectionState::Online => (
            FeedConnectionState::Streaming,
            "Coinbase history and realtime are current",
        ),
        ProviderConnectionState::Recovering => (
            FeedConnectionState::Recovering,
            "Coinbase realtime is recovering; retained history remains visible",
        ),
        ProviderConnectionState::Failed => {
            (FeedConnectionState::Stopped, "Coinbase realtime stopped")
        }
    };
    messages
        .send(MarketWorkerMessage::Connection {
            state: connection,
            message: state.detail.clone().unwrap_or_else(|| detail.to_string()),
        })
        .map_err(|error| error.to_string())
}

fn request_snapshot(
    client: &mut EngineSupervisor,
    consumer_id: u64,
    generation: u64,
    series: SeriesKey,
    messages: &MarketWorkerSender,
) -> Result<(ReplaySnapshot, DesktopMarketGeneration), String> {
    let realtime = series_supports_realtime(&series);
    client.set_series_demand(consumer_id, generation, series)?;
    loop {
        let poll = client.receive_market_event_for(consumer_id, Duration::from_millis(250))?;
        if poll.reconnected {
            let _ = messages.send(MarketWorkerMessage::Connection {
                state: FeedConnectionState::Recovering,
                message: "Resident engine restarted; restoring chart demand".to_string(),
            });
        }
        let Some(event) = poll.event else {
            continue;
        };
        // Catalog results are delivered once, so they are forwarded rather than
        // dropped while a snapshot is outstanding.
        let (catalog, event) = classify_provider_catalog_event(event, "coinbase", consumer_id);
        if let Some(catalog) = catalog {
            let _ = messages.send(MarketWorkerMessage::ProviderCatalog(catalog));
            continue;
        }
        let Some(event) = event else { continue };
        match event {
            envelope::Payload::SeriesState(state) => {
                let load_state = SeriesLoadState::try_from(state.state)
                    .map_err(|_| "engine returned an invalid series state".to_string())?;
                match load_state {
                    SeriesLoadState::Resolving => {
                        let _ = messages.send(MarketWorkerMessage::State {
                            state: ChartState::Loading,
                            message: "Resident engine is resolving Coinbase history".to_string(),
                        });
                    }
                    SeriesLoadState::Ready | SeriesLoadState::Live => {
                        return Err(
                            "engine marked history ready without a covering snapshot".to_string()
                        );
                    }
                    SeriesLoadState::Failed => {
                        return Err(state.detail.unwrap_or_else(|| {
                            "resident engine could not resolve Coinbase history".to_string()
                        }));
                    }
                    SeriesLoadState::Superseded => {
                        return Err("Coinbase history demand was superseded".to_string());
                    }
                    SeriesLoadState::Empty | SeriesLoadState::Partial => {}
                }
            }
            envelope::Payload::ProviderState(state) => {
                apply_provider_state(&state, realtime, messages)?;
            }
            envelope::Payload::SeriesSnapshot(snapshot) => {
                if snapshot.consumer_id != consumer_id || snapshot.generation != generation {
                    continue;
                }
                let replay = replay_snapshot(&snapshot)?;
                let publication = generation_from_snapshot(&snapshot, &replay)?;
                return Ok((replay, publication));
            }
            envelope::Payload::DemandError(error) => return Err(demand_error(&error)),
            envelope::Payload::Fault(fault) => return Err(fault.redacted_detail),
            // Order book and order flow are latest-value on the engine side and
            // are republished on the next depth or trade event, so skipping one
            // here costs nothing. Failing on them instead is what made every
            // resnapshot during live streaming kill the chart: the book is
            // refilled on every level-2 update, so one was almost always
            // waiting when a snapshot was requested.
            envelope::Payload::OrderBookSnapshot(_)
            | envelope::Payload::OrderFlowSnapshot(_)
            | envelope::Payload::OrderFlowUpdate(_)
            | envelope::Payload::SeriesUpdate(_) => {}
            _ => return Err("engine returned an unexpected market response".to_string()),
        }
    }
}

fn send_recovery(
    client: &mut EngineSupervisor,
    product: &InstallProviderInstrument,
    interval: ChartInterval,
    endpoint: &mut WorkerEndpoint,
    command: ReplayRecoveryCommand,
) -> Result<(), String> {
    let result = request_snapshot(
        client,
        endpoint.consumer_id,
        endpoint.active_generation,
        series_key(product, interval)?,
        &endpoint.messages,
    )
    .map(|(snapshot, generation)| {
        endpoint.publication = Some(MarketPublicationGeneration::from_generation(&generation));
        MarketWorkerBootstrap {
            snapshot,
            subscription_id: SUBSCRIPTION_ID.to_string(),
            generation,
            worker_label: WORKER_LABEL.to_string(),
        }
    });
    endpoint
        .messages
        .send(MarketWorkerMessage::Recovery {
            request_id: command.request_id,
            result,
        })
        .map_err(|error| error.to_string())
}

fn send_publication(
    messages: &MarketWorkerSender,
    update: ReplayStreamUpdate,
    generation: MarketPublicationGeneration,
) -> Result<(), String> {
    messages
        .send(MarketWorkerMessage::Update(MarketWorkerPublication {
            update,
            generation,
            subscription_id: SUBSCRIPTION_ID.to_string(),
            worker_label: WORKER_LABEL.to_string(),
            ui_diagnostics: None,
        }))
        .map_err(|error| error.to_string())
}

fn replay_snapshot(snapshot: &SeriesSnapshot) -> Result<ReplaySnapshot, String> {
    let series = snapshot
        .series
        .clone()
        .ok_or_else(|| "engine snapshot has no series identity".to_string())?;
    if series.provider != "coinbase"
        || !matches!(
            SeriesCadence::try_from(series.cadence),
            Ok(SeriesCadence::FixedSeconds
                | SeriesCadence::CalendarWeeks
                | SeriesCadence::CalendarMonths)
        )
        || series.entitlement_id != "crypto_public_realtime"
        || !series.instrument_id.starts_with("instrument:coinbase:")
    {
        return Err("engine Coinbase snapshot identity is invalid".to_string());
    }
    let price_scale = u8::try_from(snapshot.price_scale)
        .map_err(|_| "engine price scale is invalid".to_string())?;
    let quantity_scale = u8::try_from(snapshot.quantity_scale)
        .map_err(|_| "engine quantity scale is invalid".to_string())?;
    let (base, quote) = series
        .instrument_id
        .strip_prefix("instrument:coinbase:")
        .and_then(|value| value.split_once(':'))
        .filter(|(base, quote)| !base.is_empty() && !quote.is_empty() && !quote.contains(':'))
        .ok_or_else(|| "engine snapshot instrument identity is invalid".to_string())?;
    let instrument = InstrumentRevision {
        instrument_id: InstrumentId::try_new(series.instrument_id.clone())
            .map_err(|error| error.to_string())?,
        revision: u64::from(series.definition_revision),
        asset_class: AssetClass::CryptoAsset,
        symbol: format!(
            "{}/{}",
            base.to_ascii_uppercase(),
            quote.to_ascii_uppercase()
        ),
        venue_id: "COINBASE".to_string(),
        trading_currency: quote.to_ascii_uppercase(),
        precision: InstrumentPrecision::try_new(price_scale, quantity_scale)
            .map_err(|error| error.to_string())?,
        lifecycle: InstrumentLifecycle::Active,
    };
    let definition = replay_bar_definition(&series)?;
    let received = now_unix_nanos();
    let bars = snapshot
        .bars
        .iter()
        .map(|bar| {
            provenanced_engine_bar(
                &series,
                snapshot.provider_generation,
                snapshot.consumer_id,
                snapshot.generation,
                bar,
                received,
            )
        })
        .collect();
    ReplaySnapshot::try_from_provenanced_values(
        instrument,
        ReplayProvenance::LiveProvider,
        definition,
        snapshot.publication_generation,
        bars,
    )
    .map_err(|error| error.to_string())
}

fn generation_from_snapshot(
    snapshot: &SeriesSnapshot,
    replay: &ReplaySnapshot,
) -> Result<DesktopMarketGeneration, String> {
    let first_sequence = replay
        .bars()
        .first()
        .map(|bar| bar.value().source_sequence)
        .ok_or_else(|| "engine snapshot is empty".to_string())?;
    let last_sequence = replay
        .bars()
        .last()
        .map(|bar| bar.value().source_sequence)
        .ok_or_else(|| "engine snapshot is empty".to_string())?;
    DesktopMarketGeneration::try_new(
        snapshot.provider_generation,
        snapshot.publication_generation,
        first_sequence,
        last_sequence,
        replay.bars().to_vec(),
    )
    .map_err(|error| error.to_string())
}

fn tail_publication(
    current: MarketPublicationGeneration,
    tail: &ReplayTailUpdate,
) -> MarketPublicationGeneration {
    let sequence = tail.item().value().source_sequence;
    let (mut first, _) = current.sequence_range();
    let retained = match tail.operation() {
        ReplayTailOperation::Revise => current.retained_items(),
        ReplayTailOperation::Append => {
            let retained = current
                .retained_items()
                .saturating_add(1)
                .min(RETAINED_BAR_CAPACITY);
            if retained == RETAINED_BAR_CAPACITY && current.retained_items() == retained {
                first = first.saturating_add(1);
            }
            retained
        }
    };
    MarketPublicationGeneration::from_tail(tail.publication_generation(), retained, first, sequence)
}

fn replay_tail_update(update: &SeriesUpdate) -> Result<ReplayTailUpdate, String> {
    let series = update
        .series
        .as_ref()
        .ok_or_else(|| "engine update has no series identity".to_string())?;
    if series.provider != "coinbase"
        || series.cadence_value == 0
        || !matches!(
            SeriesCadence::try_from(series.cadence),
            Ok(SeriesCadence::FixedSeconds
                | SeriesCadence::CalendarWeeks
                | SeriesCadence::CalendarMonths)
        )
    {
        return Err("engine Coinbase update identity is invalid".to_string());
    }
    let bar = update
        .bar
        .as_ref()
        .ok_or_else(|| "engine Coinbase update has no bar".to_string())?;
    let item = provenanced_engine_bar(
        series,
        update.provider_generation,
        update.consumer_id,
        update.generation,
        bar,
        now_unix_nanos(),
    );
    let operation = match SeriesUpdateOperation::try_from(update.operation) {
        Ok(SeriesUpdateOperation::ReviseTail) => ReplayTailOperation::Revise,
        Ok(SeriesUpdateOperation::AppendTail) => ReplayTailOperation::Append,
        Ok(SeriesUpdateOperation::Unspecified) | Err(_) => {
            return Err("engine update operation is invalid".to_string());
        }
    };
    ReplayTailUpdate::try_new(
        item,
        update.publication_generation,
        update.forming,
        operation,
    )
    .map_err(|error| error.to_string())
}

fn provenanced_engine_bar(
    series: &SeriesKey,
    provider_generation: u64,
    consumer_id: u64,
    generation: u64,
    bar: &axiusflow_engine_protocol::MarketBar,
    received: i64,
) -> Provenanced<MarketBar> {
    let bar = MarketBar {
        source_sequence: bar.source_sequence,
        exchange_timestamp_seconds: bar.exchange_timestamp_seconds,
        exchange_timestamp_unix_nanos: bar.exchange_timestamp_unix_nanos,
        open: bar.open,
        high: bar.high,
        low: bar.low,
        close: bar.close,
        volume: bar.volume,
    };
    let exchange = bar.exchange_timestamp_unix_nanos;
    Provenanced::new(
        bar,
        MarketEventProvenance {
            event_id: format!(
                "engine-{provider_generation}-{generation}-{}",
                bar.source_sequence
            ),
            event_time_unix_nanos: exchange,
            publication_time_unix_nanos: received,
            producer: "axiusflow_engine".to_string(),
            schema_version: 1,
            correlation_id: format!("engine-series-{consumer_id}-{generation}"),
            causation_id: String::new(),
            entitlement_revision: series.entitlement_id.clone(),
            session_generation: provider_generation,
            source_id: series.provider.clone(),
            source_sequence: bar.source_sequence,
            exchange_timestamp_unix_nanos: exchange,
            provider_receive_timestamp_unix_nanos: received,
            nic_receive_timestamp_unix_nanos: None,
            axiusflow_receive_timestamp_unix_nanos: received,
            normalized_timestamp_unix_nanos: received,
            fanout_enqueue_timestamp_unix_nanos: Some(received),
            correction_flags: 0,
            quality_flags: 0,
            nic_timestamp_source: 0,
            semantic_class: 2,
        },
    )
}

fn series_key(
    product: &InstallProviderInstrument,
    interval: ChartInterval,
) -> Result<SeriesKey, String> {
    if product.provider != "coinbase"
        || product.venue_id != "coinbase"
        || product.entitlement_id != "crypto_public_realtime"
        || product.price_scale > 18
        || product.quantity_scale > 18
    {
        return Err("Coinbase installed instrument identity is invalid".to_string());
    }
    let (cadence, cadence_value) = match interval {
        ChartInterval::Minute1 => (SeriesCadence::FixedSeconds, 60),
        ChartInterval::Minute3 => (SeriesCadence::FixedSeconds, 180),
        ChartInterval::Minute5 => (SeriesCadence::FixedSeconds, 300),
        ChartInterval::Minute15 => (SeriesCadence::FixedSeconds, 900),
        ChartInterval::Minute30 => (SeriesCadence::FixedSeconds, 1_800),
        ChartInterval::Hour1 => (SeriesCadence::FixedSeconds, 3_600),
        ChartInterval::Hour2 => (SeriesCadence::FixedSeconds, 7_200),
        ChartInterval::Hour4 => (SeriesCadence::FixedSeconds, 14_400),
        ChartInterval::Hour8 => (SeriesCadence::FixedSeconds, 28_800),
        ChartInterval::Hour12 => (SeriesCadence::FixedSeconds, 43_200),
        ChartInterval::Day1 => (SeriesCadence::FixedSeconds, 86_400),
        ChartInterval::Week1 => (SeriesCadence::CalendarWeeks, 1),
        ChartInterval::Month1 => (SeriesCadence::CalendarMonths, 1),
        ChartInterval::Tick100 | ChartInterval::Day3 => {
            return Err("Coinbase chart interval is unsupported".to_string());
        }
    };
    Ok(SeriesKey {
        provider: "coinbase".to_string(),
        instrument_id: product.instrument_id.clone(),
        cadence_value,
        definition_revision: 1,
        entitlement_id: product.entitlement_id.clone(),
        cadence: cadence as i32,
    })
}

fn replay_bar_definition(series: &SeriesKey) -> Result<BarDefinition, String> {
    let (cadence_id, interval_seconds, calendar_months) =
        match SeriesCadence::try_from(series.cadence) {
            Ok(SeriesCadence::FixedSeconds) if series.cadence_value > 0 => (
                format!("{}s", series.cadence_value),
                series.cadence_value,
                None,
            ),
            Ok(SeriesCadence::CalendarWeeks) if series.cadence_value > 0 => (
                format!("calendar-weeks:{}", series.cadence_value),
                series
                    .cadence_value
                    .checked_mul(7 * 24 * 60 * 60)
                    .ok_or_else(|| "engine calendar-week cadence overflowed".to_string())?,
                None,
            ),
            Ok(SeriesCadence::CalendarMonths) if series.cadence_value > 0 => (
                format!("calendar-months:{}", series.cadence_value),
                0,
                Some(series.cadence_value),
            ),
            _ => return Err("engine Coinbase bar definition is invalid".to_string()),
        };
    Ok(BarDefinition {
        definition_id: format!("{}:{}:{cadence_id}", series.provider, series.instrument_id),
        version: series.definition_revision,
        interval_seconds,
        trades_per_bar: None,
        calendar_months,
    })
}

fn series_supports_realtime(series: &SeriesKey) -> bool {
    series.cadence_value > 0
        && matches!(
            SeriesCadence::try_from(series.cadence),
            Ok(SeriesCadence::FixedSeconds
                | SeriesCadence::CalendarWeeks
                | SeriesCadence::CalendarMonths)
        )
}

const fn snapshot_connection_state(
    _interval: ChartInterval,
) -> (FeedConnectionState, &'static str) {
    (
        FeedConnectionState::Discovering,
        "Historical bars are visible; Coinbase realtime is connecting",
    )
}

fn default_coinbase_product(product_id: &str) -> InstallProviderInstrument {
    let (base, quote) = product_id.split_once('-').unwrap_or(("BTC", "USD"));
    InstallProviderInstrument {
        provider: "coinbase".to_string(),
        session_generation: 1,
        selection_generation: 1,
        instrument_id: format!(
            "instrument:coinbase:{}:{}",
            base.to_ascii_lowercase(),
            quote.to_ascii_lowercase()
        ),
        provider_symbol: product_id.to_string(),
        display_symbol: format!("{base}/{quote}"),
        venue_id: "coinbase".to_string(),
        price_scale: 2,
        quantity_scale: 8,
        entitlement_id: "crypto_public_realtime".to_string(),
    }
}

#[cfg(test)]
fn coinbase_products() -> Vec<InstallProviderInstrument> {
    ["BTC-USD", "ETH-USD"]
        .into_iter()
        .map(default_coinbase_product)
        .collect()
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

fn random_identity() -> Result<u64, String> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_err(|_| "system CSPRNG is unavailable".to_string())?;
    Ok(NonZeroU64::new(u64::from_le_bytes(bytes))
        .unwrap_or(NonZeroU64::MIN)
        .get())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_chart_integration::{NucleusChartTheme, NucleusChartView};
    use axiusflow_engine_protocol::{
        MarketBar as IpcMarketBar, OrderBookLevel as IpcOrderBookLevel,
        OrderBookSnapshot as IpcOrderBookSnapshot, OrderBookState as IpcOrderBookState,
        ProviderInstrumentSearchResult, ProviderInstrumentSelection, ProviderInstrumentSummary,
    };

    fn handle_coinbase_catalog_event(
        endpoint: &mut WorkerEndpoint,
        event: envelope::Payload,
    ) -> Option<envelope::Payload> {
        let (catalog, event) =
            classify_provider_catalog_event(event, "coinbase", endpoint.consumer_id);
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

        let product = coinbase_products().remove(0);
        let (_worker, record) =
            worker_endpoint(2, 9, product.clone(), 41, ChartInterval::Minute5, None, 7);
        assert_eq!(record.workspace_id, 2);
        assert_eq!(record.endpoint.consumer_id, 41);
        assert_eq!(record.product.instrument_id, product.instrument_id);
        assert_eq!(record.interval, ChartInterval::Minute5);
    }

    #[test]
    fn coinbase_worker_startup_routes_catalog_events() {
        let product = coinbase_products().remove(0);
        let (mut pane, mut record) =
            worker_endpoint(2, 9, product.clone(), 41, ChartInterval::Minute5, None, 7);
        let MarketWorkerStartup::Loading(startup) = &pane.startup else {
            panic!("Coinbase endpoint must start in loading state");
        };
        assert_eq!(startup.coinbase_product, product);
        assert_eq!(startup.coinbase_interval, ChartInterval::Minute5);

        assert!(
            handle_coinbase_catalog_event(
                &mut record.endpoint,
                envelope::Payload::ProviderInstrumentSearchResult(ProviderInstrumentSearchResult {
                    consumer_id: 41,
                    provider: "coinbase".to_string(),
                    provider_generation: 1,
                    search_generation: 3,
                    instruments: vec![ProviderInstrumentSummary {
                        symbol: "BTC-USD".to_string(),
                        exchange: "coinbase".to_string(),
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
            handle_coinbase_catalog_event(
                &mut record.endpoint,
                envelope::Payload::ProviderInstrumentSelection(ProviderInstrumentSelection {
                    consumer_id: 41,
                    instrument: Some(selected),
                }),
            )
            .is_none()
        );
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::ProviderCatalog(
                ProviderCatalogEvent::SelectionInstalled(instrument)
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
                    coinbase_products().first().expect("BTC product"),
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
                    coinbase_products().first().expect("BTC product"),
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
                "Coinbase history is loaded; connecting the live edge".to_string()
            )],
            "the replacement stays covered until the trade handoff is live"
        );

        apply_series_state(
            state(SeriesLoadState::Live, None),
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
            coinbase_products().first().expect("BTC product"),
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
            instrument: &default_coinbase_product("BTC-USD"),
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
    fn non_contiguous_engine_tail_is_left_for_the_chart_validator() {
        let series = series_key(
            coinbase_products().first().expect("BTC product"),
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
                    instrument: &default_coinbase_product("BTC-USD"),
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
                    instrument: &default_coinbase_product("BTC-USD"),
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
        let product = default_coinbase_product("BTC-USD");
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
                instrument: &default_coinbase_product("BTC-USD"),
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
                provider: "coinbase".to_string(),
                state: ProviderConnectionState::Recovering as i32,
                generation: 2,
                detail: None,
            },
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
            }] if message.contains("retained history")
        ));
    }

    #[test]
    fn phase_four_series_keys_cover_required_symbols_and_intervals() {
        let products = vec![
            default_coinbase_product("BTC-USD"),
            default_coinbase_product("SOL-USD"),
        ];
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
        let product = default_coinbase_product("BTC-USD");
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
        for interval in [ChartInterval::Week1, ChartInterval::Month1] {
            let (state, message) = snapshot_connection_state(interval);
            assert_eq!(state, FeedConnectionState::Discovering);
            assert!(message.contains("realtime is connecting"));
        }

        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        apply_provider_state(
            &ProviderState {
                provider: "coinbase".to_string(),
                state: ProviderConnectionState::Connecting as i32,
                generation: 1,
                detail: None,
            },
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
        let product = default_coinbase_product("BTC-USD");
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
                instrument: &default_coinbase_product("BTC-USD"),
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
                instrument: &default_coinbase_product("BTC-USD"),
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
        let product = default_coinbase_product("BTC-USD");
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
                    instrument: &default_coinbase_product("BTC-USD"),
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
                    instrument: &default_coinbase_product("BTC-USD"),
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
    fn coinbase_series_keys_reject_conflicting_provider_metadata() {
        let mut product = coinbase_products().remove(0);
        product.provider = "rithmic".to_string();
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

    /// The Coinbase DOM panel stayed empty because the engine's order-book
    /// snapshot had no arm here: `CoinbaseDom` was declared, coalesced, and
    /// rendered, but never constructed. Levels are real BTC-USD top-of-book.
    #[test]
    fn coinbase_order_book_snapshot_uses_the_consumers_selection_identity() {
        let product = default_coinbase_product("BTC-USD");
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
        let mut publication = None;
        let mut live = false;

        let outcome = apply_pushed_event(
            envelope::Payload::OrderBookSnapshot(IpcOrderBookSnapshot {
                consumer_id: 1,
                generation: 1,
                provider: "coinbase".to_string(),
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
                        traded_volume: 125_000_000,
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
                        traded_volume: 75_000_000,
                    },
                    IpcOrderBookLevel {
                        price: 7_798_727,
                        quantity: 4_582_685,
                        order_count: None,
                        traded_volume: 0,
                    },
                ],
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
                MarketWorkerMessage::CoinbaseDom(frame) => Some(frame),
                _ => None,
            })
            .expect("Coinbase depth must reach the DOM panel");
        assert!(
            !frame.rows.is_empty(),
            "a projected Coinbase DOM frame must carry price rows"
        );
        assert_eq!(frame.selection_generation, product.selection_generation);
        assert_eq!(
            frame.rows[0]
                .bid
                .as_ref()
                .map(|level| level.traded_volume_text.as_str()),
            Some("1.25")
        );
        assert_eq!(
            frame.rows[0]
                .ask
                .as_ref()
                .map(|level| level.traded_volume_text.as_str()),
            Some("0.75")
        );
    }
}
