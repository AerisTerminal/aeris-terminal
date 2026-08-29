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
    MarketBarClientModel, MarketBarModelOutcome, MarketEventProvenance, Provenanced,
    ReplayProvenance, ReplayRecoveryCommand, ReplaySnapshot, ReplayStreamUpdate, ReplayTailUpdate,
};
use axiusflow_engine_protocol::{
    ConsumerResourceClass, DemandError, EngineFaultCode, FailureStage, InstallProviderInstrument,
    ProviderConnectionState, ProviderState, SearchProviderInstruments, SeriesCadence, SeriesKey,
    SeriesLoadState, SeriesSnapshot, SeriesUpdate, WorkspacePaneKind, WorkspaceState, envelope,
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
const MESSAGE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 32;
const MODEL_CAPACITY: usize = 32_768;
const SUBSCRIPTION_ID: &str = "desktop_engine_coinbase_bars";
const WORKER_LABEL: &str = "Coinbase engine - history and realtime IPC";
const POLL_INTERVAL: Duration = Duration::from_millis(16);
const BACKGROUND_POLL_INTERVAL: Duration = Duration::from_millis(250);
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
    model: MarketBarClientModel,
    publication: Option<MarketPublicationGeneration>,
    active_generation: u64,
    resource_class: ConsumerResourceClass,
    last_market_poll: std::time::Instant,
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
            model: empty_model(),
            publication: None,
            active_generation: initial_generation,
            resource_class: ConsumerResourceClass::Foreground,
            last_market_poll: std::time::Instant::now()
                .checked_sub(BACKGROUND_POLL_INTERVAL)
                .unwrap_or_else(std::time::Instant::now),
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
                    continue;
                }
            }
            poll_endpoint(client, record)?;
        }
        endpoints.retain(|record| record.endpoint.active);
        thread::sleep(POLL_INTERVAL);
    }
    Ok(())
}

fn poll_endpoint(client: &mut EngineSupervisor, record: &mut EndpointRecord) -> Result<(), String> {
    let endpoint = &mut record.endpoint;
    let now = std::time::Instant::now();
    if !endpoint.active || !endpoint.market_poll_due(now) {
        return Ok(());
    }
    let poll = client.poll_market_event(endpoint.consumer_id)?;
    endpoint.last_market_poll = now;
    if poll.reconnected {
        reset_application_model(&mut endpoint.model, &mut endpoint.publication);
        let _ = endpoint.messages.send(MarketWorkerMessage::Connection {
            state: FeedConnectionState::Recovering,
            message: "Resident engine restarted; restoring chart demand".to_string(),
        });
    }
    let Some(event) = poll.event else {
        return Ok(());
    };
    let (catalog, event) = classify_provider_catalog_event(event, "coinbase", endpoint.consumer_id);
    let event = match catalog {
        Some(event) => {
            let _ = endpoint
                .messages
                .send(MarketWorkerMessage::ProviderCatalog(event));
            return Ok(());
        }
        None => event,
    };
    let Some(event) = event else { return Ok(()) };
    let outcome = apply_polled_event(
        event,
        &PolledEventContext {
            consumer_id: endpoint.consumer_id,
            active_generation: endpoint.active_generation,
            realtime: interval_supports_realtime(record.interval),
            instrument: &record.product,
        },
        &mut endpoint.model,
        &mut endpoint.publication,
        &endpoint.messages,
    );
    let result = match outcome {
        Ok(PolledEventOutcome::Applied) => return Ok(()),
        Ok(PolledEventOutcome::ResnapshotRequired) => {
            recover_sequence_gap(client, &record.product, record.interval, endpoint)
        }
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        let _ = endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Error,
            message: error,
        });
    }
    Ok(())
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
        &mut endpoint.model,
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
            endpoint.model = empty_model();
            endpoint.publication = None;
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

impl WorkerEndpoint {
    fn market_poll_due(&self, now: std::time::Instant) -> bool {
        consumer_market_poll_due(
            self.resource_class,
            now.duration_since(self.last_market_poll),
        )
    }
}

fn consumer_market_poll_due(resource_class: ConsumerResourceClass, elapsed: Duration) -> bool {
    match resource_class {
        ConsumerResourceClass::Foreground => true,
        ConsumerResourceClass::Background => elapsed >= BACKGROUND_POLL_INTERVAL,
        ConsumerResourceClass::Warm | ConsumerResourceClass::Detached => false,
    }
}

fn retire_endpoint(client: &mut EngineSupervisor, endpoint: &mut WorkerEndpoint) {
    let _ = client.remove_market_consumer(endpoint.consumer_id);
    endpoint.active = false;
    let _ = endpoint.shutdown.try_send(());
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PolledEventOutcome {
    Applied,
    ResnapshotRequired,
}

/// Identity every polled engine event is checked against.
struct PolledEventContext<'a> {
    consumer_id: u64,
    active_generation: u64,
    realtime: bool,
    instrument: &'a InstallProviderInstrument,
}

fn apply_polled_event(
    event: envelope::Payload,
    context: &PolledEventContext<'_>,
    model: &mut MarketBarClientModel,
    publication: &mut Option<MarketPublicationGeneration>,
    messages: &MarketWorkerSender,
) -> Result<PolledEventOutcome, String> {
    let &PolledEventContext {
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
            let (replay, generation) = apply_snapshot(model, &snapshot)?;
            let status = MarketPublicationGeneration::from_generation(&generation);
            *publication = Some(status);
            send_publication(messages, ReplayStreamUpdate::Snapshot(replay), status)?;
            Ok(PolledEventOutcome::Applied)
        }
        envelope::Payload::SeriesUpdate(update) => {
            if update.consumer_id != consumer_id || update.generation != active_generation {
                return Err("engine realtime update identity mismatched".to_string());
            }
            let tail = replay_tail_update(&update)?;
            match model
                .apply_update(ReplayStreamUpdate::Tail(tail.clone()))
                .map_err(|error| error.to_string())?
            {
                MarketBarModelOutcome::Published(generation) => {
                    let status = MarketPublicationGeneration::from_generation(&generation);
                    *publication = Some(status);
                    send_publication(messages, ReplayStreamUpdate::Tail(tail), status)?;
                    Ok(PolledEventOutcome::Applied)
                }
                MarketBarModelOutcome::Duplicate => Ok(PolledEventOutcome::Applied),
                MarketBarModelOutcome::ResnapshotRequired(_) => {
                    Ok(PolledEventOutcome::ResnapshotRequired)
                }
            }
        }
        envelope::Payload::ProviderState(state) => {
            apply_provider_state(&state, realtime, messages)?;
            Ok(PolledEventOutcome::Applied)
        }
        envelope::Payload::SeriesState(state) => {
            if state.consumer_id != consumer_id || state.generation != active_generation {
                return Err("engine realtime state identity mismatched".to_string());
            }
            match SeriesLoadState::try_from(state.state)
                .map_err(|_| "engine returned an invalid realtime state".to_string())?
            {
                SeriesLoadState::Live => {
                    if !realtime {
                        return Err(
                            "engine marked a Coinbase calendar-history series live".to_string()
                        );
                    }
                    if publication.is_none() {
                        return Err(
                            "engine marked history live without a covering snapshot".to_string()
                        );
                    }
                    messages
                        .send(MarketWorkerMessage::State {
                            state: ChartState::Ready,
                            message: "Coinbase history/live handoff is current".to_string(),
                        })
                        .map_err(|error| error.to_string())?;
                    Ok(PolledEventOutcome::Applied)
                }
                SeriesLoadState::Failed => Err(state
                    .detail
                    .unwrap_or_else(|| "Coinbase realtime failed".to_string())),
                SeriesLoadState::Ready if publication.is_none() => {
                    Err("engine marked history ready without a covering snapshot".to_string())
                }
                SeriesLoadState::Ready
                | SeriesLoadState::Empty
                | SeriesLoadState::Resolving
                | SeriesLoadState::Partial
                | SeriesLoadState::Superseded => Ok(PolledEventOutcome::Applied),
            }
        }
        envelope::Payload::DemandError(error) => Err(demand_error(&error)),
        envelope::Payload::OrderBookSnapshot(snapshot) => {
            if snapshot.consumer_id != consumer_id {
                return Err("engine order-book consumer mismatched".to_string());
            }
            if snapshot.provider_generation < instrument.session_generation {
                return Ok(PolledEventOutcome::Applied);
            }
            let frame = dom_from_snapshot(
                &DomIdentity {
                    instrument,
                    series_generation: active_generation,
                    selection_generation: instrument.selection_generation,
                },
                &snapshot,
            )?;
            messages
                .send(MarketWorkerMessage::CoinbaseDom(frame))
                .map_err(|error| error.to_string())?;
            Ok(PolledEventOutcome::Applied)
        }
        envelope::Payload::OrderFlowSnapshot(_) | envelope::Payload::OrderFlowUpdate(_) => {
            Ok(PolledEventOutcome::Applied)
        }
        envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
        _ => Err("engine returned an unexpected polled market event".to_string()),
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
    model: &mut MarketBarClientModel,
    messages: &MarketWorkerSender,
) -> Result<(ReplaySnapshot, DesktopMarketGeneration), String> {
    let realtime = series_supports_realtime(&series);
    client.set_series_demand(consumer_id, generation, series)?;
    loop {
        let poll = client.poll_market_event(consumer_id)?;
        if poll.reconnected {
            *model = empty_model();
            let _ = messages.send(MarketWorkerMessage::Connection {
                state: FeedConnectionState::Recovering,
                message: "Resident engine restarted; restoring chart demand".to_string(),
            });
        }
        let Some(event) = poll.event else {
            thread::sleep(POLL_INTERVAL);
            continue;
        };
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
                let (replay, generation) = apply_snapshot(model, &snapshot)?;
                return Ok((replay, generation));
            }
            envelope::Payload::DemandError(error) => return Err(demand_error(&error)),
            envelope::Payload::Fault(fault) => return Err(fault.redacted_detail),
            _ => return Err("engine returned an unexpected market response".to_string()),
        }
    }
}

fn apply_snapshot(
    model: &mut MarketBarClientModel,
    snapshot: &SeriesSnapshot,
) -> Result<(ReplaySnapshot, DesktopMarketGeneration), String> {
    let replay = replay_snapshot(snapshot)?;
    let outcome = model
        .apply_update(ReplayStreamUpdate::Snapshot(replay.clone()))
        .map_err(|error| error.to_string())?;
    let MarketBarModelOutcome::Published(generation) = outcome else {
        return Err("engine snapshot did not publish a client generation".to_string());
    };
    Ok((replay, generation))
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
        &mut endpoint.model,
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

fn recover_sequence_gap(
    client: &mut EngineSupervisor,
    product: &InstallProviderInstrument,
    interval: ChartInterval,
    endpoint: &mut WorkerEndpoint,
) -> Result<(), String> {
    let _ = endpoint.messages.send(MarketWorkerMessage::Connection {
        state: FeedConnectionState::Recovering,
        message: "Realtime updates were coalesced; requesting a covering snapshot".to_string(),
    });
    let (snapshot, generation) = request_snapshot(
        client,
        endpoint.consumer_id,
        endpoint.active_generation,
        series_key(product, interval)?,
        &mut endpoint.model,
        &endpoint.messages,
    )?;
    let publication = MarketPublicationGeneration::from_generation(&generation);
    send_publication(
        &endpoint.messages,
        ReplayStreamUpdate::Snapshot(snapshot),
        publication,
    )?;
    endpoint.publication = Some(publication);
    Ok(())
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

fn replay_tail_update(update: &SeriesUpdate) -> Result<ReplayTailUpdate, String> {
    let series = update
        .series
        .as_ref()
        .ok_or_else(|| "engine update has no series identity".to_string())?;
    if series.provider != "coinbase"
        || SeriesCadence::try_from(series.cadence) != Ok(SeriesCadence::FixedSeconds)
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
    ReplayTailUpdate::try_new(item, update.publication_generation, update.forming)
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
    SeriesCadence::try_from(series.cadence) == Ok(SeriesCadence::FixedSeconds)
}

const fn interval_supports_realtime(interval: ChartInterval) -> bool {
    !matches!(interval, ChartInterval::Week1 | ChartInterval::Month1)
}

const fn snapshot_connection_state(interval: ChartInterval) -> (FeedConnectionState, &'static str) {
    if interval_supports_realtime(interval) {
        (
            FeedConnectionState::Discovering,
            "Historical bars are visible; Coinbase realtime is connecting",
        )
    } else {
        (
            FeedConnectionState::Disconnected,
            "Completed Coinbase history; current calendar bucket is not live",
        )
    }
}

fn reset_application_model(
    model: &mut MarketBarClientModel,
    publication: &mut Option<MarketPublicationGeneration>,
) {
    *model = empty_model();
    *publication = None;
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

fn empty_model() -> MarketBarClientModel {
    MarketBarClientModel::new(NonZeroUsize::new(MODEL_CAPACITY).unwrap_or(NonZeroUsize::MIN))
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
    use axiusflow_engine_protocol::{
        MarketBar as IpcMarketBar, OrderBookLevel as IpcOrderBookLevel,
        OrderBookSnapshot as IpcOrderBookSnapshot, OrderBookState as IpcOrderBookState,
        ProviderInstrumentSearchResult, ProviderInstrumentSelection, ProviderInstrumentSummary,
        SeriesState,
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
        })
        .expect("tail converts");
        assert_eq!(update.item().value().source_sequence, 3);
        assert_eq!(update.item().value().close, 115);
        assert_eq!(update.publication_generation(), 8);
        assert!(update.forming());
    }

    #[test]
    fn non_contiguous_engine_tail_requests_a_covering_snapshot_without_erroring_chart() {
        let series = series_key(
            coinbase_products().first().expect("BTC product"),
            ChartInterval::Minute1,
        )
        .expect("series");
        let (sender, _receiver) =
            market_worker_channel(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
        let mut model = empty_model();
        let mut publication = None;
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
            apply_polled_event(
                envelope::Payload::SeriesSnapshot(snapshot),
                &PolledEventContext {
                    consumer_id: 1,
                    active_generation: 1,
                    realtime: true,
                    instrument: &default_coinbase_product("BTC-USD"),
                },
                &mut model,
                &mut publication,
                &sender,
            ),
            Ok(PolledEventOutcome::Applied)
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
        };
        assert_eq!(
            apply_polled_event(
                envelope::Payload::SeriesUpdate(skipped_tail),
                &PolledEventContext {
                    consumer_id: 1,
                    active_generation: 1,
                    realtime: true,
                    instrument: &default_coinbase_product("BTC-USD"),
                },
                &mut model,
                &mut publication,
                &sender,
            ),
            Ok(PolledEventOutcome::ResnapshotRequired)
        );
        assert_eq!(
            publication.map(MarketPublicationGeneration::sequence_range),
            Some((1, 1))
        );
    }

    #[test]
    fn live_state_without_covering_snapshot_is_rejected() {
        let (sender, _receiver) = market_worker_channel(NonZeroUsize::MIN);
        let mut model = empty_model();
        let mut publication = None;

        let result = apply_polled_event(
            envelope::Payload::SeriesState(SeriesState {
                consumer_id: 1,
                generation: 7,
                state: SeriesLoadState::Live as i32,
                ..SeriesState::default()
            }),
            &PolledEventContext {
                consumer_id: 1,
                active_generation: 7,
                realtime: true,
                instrument: &default_coinbase_product("BTC-USD"),
            },
            &mut model,
            &mut publication,
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
    fn calendar_history_state_never_claims_realtime_connection() {
        for interval in [ChartInterval::Week1, ChartInterval::Month1] {
            let (state, message) = snapshot_connection_state(interval);
            assert_eq!(state, FeedConnectionState::Disconnected);
            assert!(message.contains("Completed Coinbase history"));
            assert!(message.contains("current calendar bucket is not live"));
            assert!(!message.contains("is current"));
        }

        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        apply_provider_state(
            &ProviderState {
                provider: "coinbase".to_string(),
                state: ProviderConnectionState::Connecting as i32,
                generation: 1,
                detail: None,
            },
            false,
            &sender,
        )
        .expect("history-only provider state is ignored");
        assert!(receiver.drain().0.is_empty());
    }

    #[test]
    fn calendar_history_rejects_an_engine_live_claim() {
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
        let mut model = empty_model();
        let mut publication = None;
        apply_polled_event(
            envelope::Payload::SeriesSnapshot(snapshot),
            &PolledEventContext {
                consumer_id: 1,
                active_generation: 7,
                realtime: false,
                instrument: &default_coinbase_product("BTC-USD"),
            },
            &mut model,
            &mut publication,
            &sender,
        )
        .expect("completed calendar snapshot applies");

        assert_eq!(
            apply_polled_event(
                envelope::Payload::SeriesState(SeriesState {
                    consumer_id: 1,
                    generation: 7,
                    state: SeriesLoadState::Live as i32,
                    ..SeriesState::default()
                }),
                &PolledEventContext {
                    consumer_id: 1,
                    active_generation: 7,
                    realtime: false,
                    instrument: &default_coinbase_product("BTC-USD"),
                },
                &mut model,
                &mut publication,
                &sender,
            ),
            Err("engine marked a Coinbase calendar-history series live".to_string())
        );
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
        let mut model = empty_model();
        let mut publication = None;
        assert_eq!(
            apply_polled_event(
                snapshot(9, 12, 12),
                &PolledEventContext {
                    consumer_id: 1,
                    active_generation: 1,
                    realtime: true,
                    instrument: &default_coinbase_product("BTC-USD"),
                },
                &mut model,
                &mut publication,
                &sender,
            ),
            Ok(PolledEventOutcome::Applied)
        );

        reset_application_model(&mut model, &mut publication);

        assert_eq!(
            apply_polled_event(
                snapshot(1, 1, 1),
                &PolledEventContext {
                    consumer_id: 1,
                    active_generation: 1,
                    realtime: true,
                    instrument: &default_coinbase_product("BTC-USD"),
                },
                &mut model,
                &mut publication,
                &sender,
            ),
            Ok(PolledEventOutcome::Applied)
        );
        assert_eq!(
            model
                .current_generation()
                .map(axiusflow_application::MarketGeneration::session_generation),
            Some(1)
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
    fn coinbase_order_book_snapshot_reaches_the_dom_panel() {
        let product = default_coinbase_product("BTC-USD");
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
        let mut model = empty_model();
        let mut publication = None;

        let outcome = apply_polled_event(
            envelope::Payload::OrderBookSnapshot(IpcOrderBookSnapshot {
                consumer_id: 1,
                generation: 1,
                provider: "coinbase".to_string(),
                instrument_id: product.instrument_id.clone(),
                entitlement_id: product.entitlement_id.clone(),
                provider_generation: 1,
                selection_generation: 1,
                revision: 1,
                source_watermark: 1,
                state: IpcOrderBookState::Ready as i32,
                bids: vec![
                    IpcOrderBookLevel {
                        price: 7_798_670,
                        quantity: 653_408,
                        order_count: None,
                    },
                    IpcOrderBookLevel {
                        price: 7_798_514,
                        quantity: 2_564_592,
                        order_count: None,
                    },
                ],
                asks: vec![
                    IpcOrderBookLevel {
                        price: 7_798_671,
                        quantity: 22_517_771,
                        order_count: None,
                    },
                    IpcOrderBookLevel {
                        price: 7_798_727,
                        quantity: 4_582_685,
                        order_count: None,
                    },
                ],
            }),
            &PolledEventContext {
                consumer_id: 1,
                active_generation: 1,
                realtime: true,
                instrument: &product,
            },
            &mut model,
            &mut publication,
            &sender,
        );

        assert_eq!(outcome, Ok(PolledEventOutcome::Applied));
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
    }
}
