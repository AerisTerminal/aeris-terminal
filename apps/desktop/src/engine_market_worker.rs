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
    ProviderConnectionState, ProviderState, SeriesCadence, SeriesKey, SeriesLoadState,
    SeriesSnapshot, SeriesUpdate, WorkspacePaneKind, WorkspaceState, envelope,
};
#[cfg(test)]
use axiusflow_engine_protocol::{WorkspacePaneState, WorkspaceTabState};
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_market_data::{BarDefinition, ChartInterval, MarketBar};
use axiusflow_observability::FeedConnectionState;

use crate::engine_supervisor::EngineSupervisor;
use crate::resident_market_worker::{
    ChartState, DesktopMarketGeneration, MarketDataWorker, MarketPublicationGeneration,
    MarketWorkerBootstrap, MarketWorkerCommand, MarketWorkerMessage, MarketWorkerPublication,
    MarketWorkerSender, MarketWorkerStartup, market_worker_channel,
};

const DEFAULT_WORKSPACE_ID: u64 = 1;
const INITIAL_GENERATION: u64 = 1;
const MESSAGE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 32;
const MODEL_CAPACITY: usize = 350;
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

pub(super) fn start() -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    let products = coinbase_products();
    let product = products
        .first()
        .cloned()
        .ok_or_else(|| "Coinbase engine product catalog is empty".to_string())?;
    let mut workers = start_group(vec![(DEFAULT_WORKSPACE_ID, product)])?;
    workers
        .pop()
        .ok_or_else(|| "Coinbase engine worker group is empty".to_string())
}

pub(super) fn start_multi_chart() -> Result<Vec<(MarketWorkerStartup, MarketDataWorker)>, String> {
    let products = coinbase_products();
    let btc = products
        .first()
        .cloned()
        .ok_or_else(|| "Coinbase engine product catalog is empty".to_string())?;
    let eth = products
        .get(1)
        .cloned()
        .ok_or_else(|| "Coinbase engine ETH product is unavailable".to_string())?;
    start_group(vec![(1, btc), (2, eth)])
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
            300 => Ok(ChartInterval::Minute5),
            900 => Ok(ChartInterval::Minute15),
            3_600 => Ok(ChartInterval::Hour1),
            _ => Err("workspace chart cadence is unsupported by Coinbase".to_string()),
        },
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
) -> Result<Vec<(MarketWorkerStartup, MarketDataWorker)>, String> {
    let client_id = random_identity()?;
    let mut workers = Vec::with_capacity(configurations.len());
    let mut endpoints = Vec::with_capacity(configurations.len());
    for (workspace_id, product) in configurations {
        let consumer_id = random_identity()?;
        let pane_id = random_identity()?;
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
    spawn_group(client_id, endpoints, None)?;
    Ok(workers)
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
            let endpoint = &mut record.endpoint;
            process_pending_resource_class(client, endpoint);
            match endpoint.commands.try_recv() {
                Ok(command) => {
                    if let Err(error) =
                        process_command(client, &record.product, record.interval, endpoint, command)
                    {
                        let _ = endpoint.messages.send(MarketWorkerMessage::State {
                            state: ChartState::Error,
                            message: error,
                        });
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    retire_endpoint(client, endpoint);
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
        let _ = endpoint.messages.send(MarketWorkerMessage::Connection {
            state: FeedConnectionState::Recovering,
            message: "Resident engine restarted; restoring chart demand".to_string(),
        });
    }
    let Some(event) = poll.event else {
        return Ok(());
    };
    let outcome = apply_polled_event(
        event,
        endpoint.consumer_id,
        endpoint.active_generation,
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
    let _ = endpoint.messages.send(MarketWorkerMessage::CoinbaseCatalog(
        Ok(coinbase_products()),
    ));
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
            let _ = endpoint.messages.send(MarketWorkerMessage::Connection {
                state: FeedConnectionState::Discovering,
                message: "Historical bars are visible; Coinbase realtime is connecting".to_string(),
            });
        }
        Err(error) => {
            let _ = endpoint.messages.send(MarketWorkerMessage::State {
                state: ChartState::Error,
                message: error,
            });
        }
    }
    Ok(())
}

fn process_command(
    client: &mut EngineSupervisor,
    product: &InstallProviderInstrument,
    interval: ChartInterval,
    endpoint: &mut WorkerEndpoint,
    command: MarketWorkerCommand,
) -> Result<(), String> {
    match command {
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
            client.set_series_demand(endpoint.consumer_id, request.sequence, series)
        }
        MarketWorkerCommand::Recovery(command) => {
            send_recovery(client, product, interval, endpoint, command)
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
        MarketWorkerCommand::ProviderSearch(_)
        | MarketWorkerCommand::ProviderSelect(_)
        | MarketWorkerCommand::EngineSeries(_) => {
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

fn apply_polled_event(
    event: envelope::Payload,
    consumer_id: u64,
    active_generation: u64,
    model: &mut MarketBarClientModel,
    publication: &mut Option<MarketPublicationGeneration>,
    messages: &MarketWorkerSender,
) -> Result<PolledEventOutcome, String> {
    match event {
        envelope::Payload::SeriesSnapshot(snapshot) => {
            if snapshot.consumer_id != consumer_id || snapshot.generation != active_generation {
                return Err("engine realtime snapshot identity mismatched".to_string());
            }
            let replay = replay_snapshot(&snapshot)?;
            let outcome = model
                .apply_update(ReplayStreamUpdate::Snapshot(replay.clone()))
                .map_err(|error| error.to_string())?;
            let MarketBarModelOutcome::Published(generation) = outcome else {
                return Err("engine realtime snapshot was not publishable".to_string());
            };
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
            apply_provider_state(&state, messages)?;
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
                SeriesLoadState::Empty
                | SeriesLoadState::Resolving
                | SeriesLoadState::Partial
                | SeriesLoadState::Ready
                | SeriesLoadState::Superseded => Ok(PolledEventOutcome::Applied),
            }
        }
        envelope::Payload::DemandError(error) => Err(demand_error(&error)),
        envelope::Payload::OrderFlowSnapshot(_) | envelope::Payload::OrderFlowUpdate(_) => {
            Ok(PolledEventOutcome::Applied)
        }
        envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
        _ => Err("engine returned an unexpected polled market event".to_string()),
    }
}

fn apply_provider_state(
    state: &ProviderState,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    if state.provider != "coinbase" {
        return Err("engine provider state identity mismatched".to_string());
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
    client.set_series_demand(consumer_id, generation, series)?;
    loop {
        let poll = client.poll_market_event(consumer_id)?;
        if poll.reconnected {
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
                apply_provider_state(&state, messages)?;
            }
            envelope::Payload::SeriesSnapshot(snapshot) => {
                let replay = replay_snapshot(&snapshot)?;
                let outcome = model
                    .apply_update(ReplayStreamUpdate::Snapshot(replay.clone()))
                    .map_err(|error| error.to_string())?;
                let MarketBarModelOutcome::Published(generation) = outcome else {
                    return Err("engine snapshot did not publish a client generation".to_string());
                };
                return Ok((replay, generation));
            }
            envelope::Payload::DemandError(error) => return Err(demand_error(&error)),
            envelope::Payload::Fault(fault) => return Err(fault.redacted_detail),
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
        || SeriesCadence::try_from(series.cadence) != Ok(SeriesCadence::FixedSeconds)
        || !coinbase_products().into_iter().any(|product| {
            product.instrument_id == series.instrument_id
                && product.entitlement_id == series.entitlement_id
        })
    {
        return Err("engine Coinbase snapshot identity is invalid".to_string());
    }
    let price_scale = u8::try_from(snapshot.price_scale)
        .map_err(|_| "engine price scale is invalid".to_string())?;
    let quantity_scale = u8::try_from(snapshot.quantity_scale)
        .map_err(|_| "engine quantity scale is invalid".to_string())?;
    let product = coinbase_products()
        .into_iter()
        .find(|product| product.instrument_id == series.instrument_id)
        .ok_or_else(|| "engine snapshot instrument is unsupported".to_string())?;
    if series.entitlement_id != product.entitlement_id {
        return Err("engine Coinbase snapshot entitlement is invalid".to_string());
    }
    let instrument = InstrumentRevision {
        instrument_id: InstrumentId::try_new(series.instrument_id.clone())
            .map_err(|error| error.to_string())?,
        revision: u64::from(series.definition_revision),
        asset_class: AssetClass::CryptoAsset,
        symbol: product.display_symbol,
        venue_id: "COINBASE".to_string(),
        trading_currency: "USD".to_string(),
        precision: InstrumentPrecision::try_new(price_scale, quantity_scale)
            .map_err(|error| error.to_string())?,
        lifecycle: InstrumentLifecycle::Active,
    };
    let definition = BarDefinition {
        definition_id: format!(
            "{}:{}:{}s",
            series.provider, series.instrument_id, series.cadence_value
        ),
        version: series.definition_revision,
        interval_seconds: series.cadence_value,
        trades_per_bar: None,
    };
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
    let supported_product = coinbase_products().into_iter().any(|supported| {
        product.provider == supported.provider
            && product.provider_symbol == supported.provider_symbol
            && product.instrument_id == supported.instrument_id
            && product.venue_id == supported.venue_id
            && product.price_scale == supported.price_scale
            && product.quantity_scale == supported.quantity_scale
            && product.entitlement_id == supported.entitlement_id
    });
    if !supported_product
        || !matches!(
            interval,
            ChartInterval::Minute1
                | ChartInterval::Minute5
                | ChartInterval::Minute15
                | ChartInterval::Hour1
        )
    {
        return Err(
            "this migration slice supports BTC-USD/ETH-USD at 1m, 5m, 15m, and 1h".to_string(),
        );
    }
    Ok(SeriesKey {
        provider: "coinbase".to_string(),
        instrument_id: product.instrument_id.clone(),
        cadence_value: match interval {
            ChartInterval::Minute1 => 60,
            ChartInterval::Minute5 => 300,
            ChartInterval::Minute15 => 900,
            ChartInterval::Hour1 => 3_600,
            _ => unreachable!("supported intervals were validated above"),
        },
        definition_revision: 1,
        entitlement_id: product.entitlement_id.clone(),
        cadence: SeriesCadence::FixedSeconds as i32,
    })
}

fn coinbase_products() -> Vec<InstallProviderInstrument> {
    [("BTC", "btc"), ("ETH", "eth")]
        .into_iter()
        .map(|(base, canonical)| InstallProviderInstrument {
            provider: "coinbase".to_string(),
            session_generation: 1,
            selection_generation: 1,
            instrument_id: format!("instrument:coinbase:{canonical}:usd"),
            provider_symbol: format!("{base}-USD"),
            display_symbol: format!("{base}/USD"),
            venue_id: "coinbase".to_string(),
            price_scale: 2,
            quantity_scale: 8,
            entitlement_id: "crypto_public_realtime".to_string(),
        })
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
    use axiusflow_engine_protocol::MarketBar as IpcMarketBar;

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
                1,
                1,
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
                1,
                1,
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
    fn engine_recovery_state_remains_explicit_while_history_is_retained() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        apply_provider_state(
            &ProviderState {
                provider: "coinbase".to_string(),
                state: ProviderConnectionState::Recovering as i32,
                generation: 2,
                detail: None,
            },
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
        let products = coinbase_products();
        assert_eq!(products.len(), 2);
        for product in &products {
            for (interval, seconds) in [
                (ChartInterval::Minute1, 60),
                (ChartInterval::Minute5, 300),
                (ChartInterval::Minute15, 900),
                (ChartInterval::Hour1, 3_600),
            ] {
                let series = series_key(product, interval).expect("phase-four series validates");
                assert_eq!(series.cadence_value, seconds);
                assert_eq!(series.instrument_id, product.instrument_id);
            }
        }
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
}
