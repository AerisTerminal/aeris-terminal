//! Desktop-side bridge from the in-process market runtime to GPUI chart workers.

#[cfg(test)]
use std::time::{Duration, Instant};

use aeris_trading_runtime::clock::Clock;
use std::{
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

#[cfg(test)]
use aeris_application::ReplayStreamUpdate;
use aeris_application::{
    MarketEventProvenance, Provenanced, ReplayProvenance, ReplayRecoveryCommand, ReplaySnapshot,
    ReplayTailOperation, ReplayTailUpdate,
};
use aeris_contracts::{
    EngineFaultCode, FailureStage, InstallProviderInstrument, WorkspacePaneKind, WorkspaceState,
};
#[cfg(test)]
use aeris_contracts::{
    ProviderConnectionState, ProviderState, SeriesKey, SeriesLoadState, WorkspacePaneState,
    WorkspaceTabState,
};
use aeris_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use aeris_market_data::{BarDefinition, BarPeriod, BarSeriesKey, ChartInterval, MarketBar};
#[cfg(test)]
use aeris_market_runtime::MarketSeriesState;
pub(super) use aeris_market_runtime::{
    MarketConsumerResourceClass as ConsumerResourceClass, MarketDemandError,
    MarketDemandErrorScope, MarketOrderBookSnapshot, MarketPriceAlert, MarketPriceAlertTrigger,
    MarketRuntimeEvent, MarketSeriesSnapshot, MarketSeriesUpdate, MarketService, MarketStream,
    SeriesTailOperation, StreamRequirements,
};
use aeris_observability::FeedConnectionState;

use aeris_desktop::market_worker::{
    LocalCommandOutcome, MarketDataWorker, MarketRecoveryFailure, MarketWorkerCommand,
    MarketWorkerMessage, MarketWorkerSender, MarketWorkerStartup, market_worker_channel,
};

const DEFAULT_WORKSPACE_ID: u64 = 1;
pub(super) const INITIAL_GENERATION: u64 = 1;
const MESSAGE_CAPACITY: usize = 256;
const COMMAND_CAPACITY: usize = 32;
const SUBSCRIPTION_ID: &str = "desktop_runtime_rithmic_bars";
const WORKER_LABEL: &str = "Rithmic market runtime";
const HYPERLIQUID_SUBSCRIPTION_ID: &str = "desktop_runtime_hyperliquid_bars";
const HYPERLIQUID_WORKER_LABEL: &str = "Hyperliquid market runtime";
const WORKSPACE_ADDITION_CAPACITY: usize = super::local_state::MAXIMUM_WATCHLIST_ENTRIES
    + super::MAXIMUM_OPEN_WORKSPACES * (super::CHART_PANE_CAPACITY + 1);

/// The streams a chart demands from `provider`, limited to the ones that
/// provider publishes; the runtime rejects demand for any other stream.
pub(super) fn chart_demand_streams(
    market: &MarketService,
    provider: &str,
    depth_visible: bool,
) -> Result<StreamRequirements, String> {
    market.available_streams(provider, chart_streams(depth_visible))
}

fn chart_streams(depth_visible: bool) -> StreamRequirements {
    let streams = StreamRequirements::BARS
        .with(MarketStream::Trades)
        .with(MarketStream::Quotes);
    if depth_visible {
        streams.with(MarketStream::Depth)
    } else {
        streams
    }
}

/// What a runtime consumer endpoint is for, which decides whether it ever demands a series.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EndpointRole {
    /// Chart or market-summary demand for one instrument.
    Market,
    /// Rithmic instrument search before any chart demand exists.
    RithmicCatalog,
    /// Listed-market statistics screens and their selections; never demands a series.
    MarketScreen,
}

impl EndpointRole {
    const fn demands_series(self) -> bool {
        matches!(self, Self::Market)
    }
}

struct EndpointRecord {
    workspace_id: u64,
    product: InstallProviderInstrument,
    initial_instrument: aeris_market_runtime::InstrumentRequest,
    interval: ChartInterval,
    role: EndpointRole,
    pending_selection: Option<(u64, InstallProviderInstrument, ChartInterval)>,
    pending_catalog_selection: Option<(u64, u64)>,
    /// The initial runtime selection has one reply, even when a replacement
    /// request arrives before that reply or the runtime repeats an outcome.
    initial_selection_pending: bool,
    /// Instrument of the last session status sent to the chart.
    session_status_sent: Option<String>,
    /// The runtime publishes provider state for its current demand, which can
    /// name a pending selection's provider before its outcome is adopted.
    pending_provider_state: Option<aeris_market_runtime::MarketConsumerEvent>,
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
    pub restore_notices: Vec<String>,
}

#[derive(Clone)]
pub(super) struct WorkspaceMarketFactory {
    market: MarketService,
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
        self.create_endpoint(
            workspace_id,
            product,
            interval,
            ConsumerResourceClass::Foreground,
        )
    }

    pub fn create_summary_worker(
        &self,
        workspace_id: u64,
        product: InstallProviderInstrument,
        interval: ChartInterval,
        resource_class: ConsumerResourceClass,
    ) -> Result<MarketDataWorker, String> {
        self.create_endpoint(workspace_id, product, interval, resource_class)
            .map(|endpoint| endpoint.worker)
    }

    /// Starts a consumer that only screens `provider`'s listed markets and resolves the
    /// screener's selections. It owns no series demand, so it costs no provider stream.
    pub fn create_market_screen_worker(
        &self,
        workspace_id: u64,
        provider: &str,
    ) -> Result<MarketDataWorker, String> {
        let placeholder = InstallProviderInstrument {
            provider: provider.to_string(),
            ..Default::default()
        };
        self.add_endpoint(
            workspace_id,
            placeholder,
            ChartInterval::Day1,
            ConsumerResourceClass::Foreground,
            EndpointRole::MarketScreen,
        )
        .map(|endpoint| endpoint.worker)
    }

    fn create_endpoint(
        &self,
        workspace_id: u64,
        product: InstallProviderInstrument,
        interval: ChartInterval,
        resource_class: ConsumerResourceClass,
    ) -> Result<WorkspaceMarketPane, String> {
        self.add_endpoint(
            workspace_id,
            product,
            interval,
            resource_class,
            EndpointRole::Market,
        )
    }

    fn add_endpoint(
        &self,
        workspace_id: u64,
        product: InstallProviderInstrument,
        interval: ChartInterval,
        resource_class: ConsumerResourceClass,
        role: EndpointRole,
    ) -> Result<WorkspaceMarketPane, String> {
        let pane_id = allocate_pane_id(&self.next_pane_id)?;
        let consumer_id = allocate_consumer_id(&self.next_consumer_id)?;
        let (worker, mut endpoint) = worker_endpoint_for_market(
            &self.market,
            workspace_id,
            pane_id,
            product,
            consumer_id,
            interval,
            (None, INITIAL_GENERATION),
        );
        endpoint.endpoint.resource_class = resource_class;
        endpoint.role = role;
        if role == EndpointRole::Market {
            endpoint.initial_instrument =
                aeris_market_runtime::InstrumentRequest::restored(&endpoint.product);
        }
        self.additions
            .try_send(endpoint)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    "workspace creation is busy; try again after the current workspace opens"
                        .to_string()
                }
                mpsc::TrySendError::Disconnected(_) => {
                    "the market workspace runtime is unavailable".to_string()
                }
            })?;
        self.market.wake_bridges();
        Ok(worker)
    }
}

fn allocate_workspace_id(next_workspace_id: &AtomicU64) -> Result<u64, String> {
    next_workspace_id
        .try_update(Ordering::Relaxed, Ordering::Relaxed, checked_add_one)
        .map_err(|_| "workspace identity space is exhausted".to_string())
}

fn allocate_consumer_id(next_consumer_id: &AtomicU64) -> Result<u64, String> {
    next_consumer_id
        .try_update(Ordering::Relaxed, Ordering::Relaxed, checked_add_one)
        .map_err(|_| "market consumer identity space is exhausted".to_string())
}

fn allocate_pane_id(next_pane_id: &AtomicU64) -> Result<u64, String> {
    next_pane_id
        .try_update(Ordering::Relaxed, Ordering::Relaxed, checked_add_one)
        .map_err(|_| "workspace pane identity space is exhausted".to_string())
}

const fn checked_add_one(value: u64) -> Option<u64> {
    value.checked_add(1)
}

#[cfg(test)]
fn start() -> Result<
    (
        MarketWorkerStartup,
        MarketDataWorker,
        WorkspaceMarketFactory,
    ),
    String,
> {
    let product = default_hyperliquid_product();
    let (mut workers, factory) = start_group(
        MarketService::start()?,
        vec![(DEFAULT_WORKSPACE_ID, product)],
        crate::desktop::tests::test_trading_service(),
        Arc::new(aeris_trading_runtime::clock::SystemClock::default()),
    )?;
    let (startup, worker) = workers
        .pop()
        .ok_or_else(|| "Hyperliquid engine worker group is empty".to_string())?;
    Ok((startup, worker, factory))
}

pub(super) fn start_multi_chart(
    market: &MarketService,
    trading: aeris_trading_runtime::TradingService,
    clock: Arc<dyn Clock>,
) -> Result<Vec<(MarketWorkerStartup, MarketDataWorker)>, String> {
    let mnq = default_product("MNQ");
    let es = default_product("ES");
    let (workers, _factory) = start_group(market.clone(), vec![(1, mnq), (2, es)], trading, clock)?;
    Ok(workers)
}

pub(super) fn start_rithmic_catalog(
    market: &MarketService,
    trading: aeris_trading_runtime::TradingService,
    clock: Arc<dyn Clock>,
) -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    let client_id = random_order_book_identity()?;
    let consumer_id = random_order_book_identity()?;
    let pane_id = random_order_book_identity()?;
    let (mut pane, mut endpoint) = worker_endpoint_for_market(
        market,
        DEFAULT_WORKSPACE_ID,
        pane_id,
        default_product("MNQ"),
        consumer_id,
        ChartInterval::Minute1,
        (None, 0),
    );
    pane.startup = MarketWorkerStartup::Rithmic;
    endpoint.role = EndpointRole::RithmicCatalog;
    spawn_group(
        market.clone(),
        client_id,
        vec![endpoint],
        None,
        trading,
        clock,
    )?;
    Ok((pane.startup, pane.worker))
}

pub(super) fn start_workspace_tabs(
    market: &MarketService,
    workspace: &WorkspaceState,
    restored: bool,
    trading: aeris_trading_runtime::TradingService,
    clock: Arc<dyn Clock>,
) -> Result<WorkspaceMarketGroup, String> {
    let client_id = random_order_book_identity()?;
    let mut initial = Vec::new();
    let mut endpoints = Vec::new();
    let plan = restored_workspace_boot_plan(workspace)?;
    for notice in &plan.notices {
        aeris_observability::diagnostic!("Aeris workspace pane repair: {notice}");
    }
    let restore_notices = plan.notices.clone();
    for pane in plan.panes {
        let (worker, mut endpoint) = worker_endpoint_for_market(
            market,
            pane.workspace_id,
            pane.pane_id,
            pane.instrument,
            pane.consumer_id,
            pane.interval,
            (pane.restored_viewport, pane.generation),
        );
        endpoint.initial_instrument = workspace_initial_instrument(&endpoint.product, restored);
        initial.push(worker);
        endpoints.push(endpoint);
    }
    let (addition_tx, addition_rx) = mpsc::sync_channel(WORKSPACE_ADDITION_CAPACITY);
    spawn_group(
        market.clone(),
        client_id,
        endpoints,
        Some(addition_rx),
        trading,
        clock,
    )?;
    Ok(WorkspaceMarketGroup {
        initial,
        factory: WorkspaceMarketFactory {
            market: market.clone(),
            additions: addition_tx,
            next_workspace_id: Arc::new(AtomicU64::new(
                plan.maximum_workspace_id.saturating_add(1),
            )),
            next_pane_id: Arc::new(AtomicU64::new(plan.maximum_pane_id.saturating_add(1))),
            next_consumer_id: Arc::new(AtomicU64::new(plan.maximum_consumer_id.saturating_add(1))),
        },
        restore_notices,
    })
}

fn workspace_initial_instrument(
    product: &InstallProviderInstrument,
    restored: bool,
) -> aeris_market_runtime::InstrumentRequest {
    if restored {
        aeris_market_runtime::InstrumentRequest::restored(product)
    } else {
        aeris_market_runtime::InstrumentRequest::startup(product)
    }
}

pub(super) struct RestoredWorkspacePaneSpec {
    pub(super) workspace_id: u64,
    pub(super) pane_id: u64,
    pub(super) consumer_id: u64,
    pub(super) instrument: InstallProviderInstrument,
    pub(super) interval: ChartInterval,
    pub(super) restored_viewport: Option<(i64, i64)>,
    pub(super) generation: u64,
}

pub(super) struct RestoredWorkspaceBootPlan {
    pub(super) panes: Vec<RestoredWorkspacePaneSpec>,
    pub(super) notices: Vec<String>,
    maximum_workspace_id: u64,
    maximum_pane_id: u64,
    maximum_consumer_id: u64,
}

pub(super) fn restored_workspace_boot_plan(
    workspace: &WorkspaceState,
) -> Result<RestoredWorkspaceBootPlan, String> {
    let (panes, notices) = restored_workspace_pane_specs(workspace)?;
    let (maximum_workspace_id, maximum_pane_id, maximum_consumer_id) =
        workspace_identity_high_watermarks(workspace);
    Ok(RestoredWorkspaceBootPlan {
        panes,
        notices,
        maximum_workspace_id,
        maximum_pane_id,
        maximum_consumer_id,
    })
}

fn restored_workspace_pane_specs(
    workspace: &WorkspaceState,
) -> Result<(Vec<RestoredWorkspacePaneSpec>, Vec<String>), String> {
    let mut panes = Vec::new();
    let mut notices = Vec::new();
    for tab in &workspace.workspace_tabs {
        for pane in &tab.panes {
            if WorkspacePaneKind::try_from(pane.kind).ok() != Some(WorkspacePaneKind::Chart) {
                continue;
            }
            let Some(instrument) = pane.instrument.clone() else {
                notices.push(format!(
                    "Pane {} was skipped because its instrument is missing",
                    pane.pane_id
                ));
                continue;
            };
            let Some(presentation) = provider_presentation(&instrument.provider) else {
                notices.push(format!(
                    "Pane {} was skipped because its provider is unavailable",
                    pane.pane_id
                ));
                continue;
            };
            let requested =
                aeris_market_runtime::persisted_interval(&pane.interval).or_else(|| {
                    pane.series
                        .as_ref()
                        .and_then(aeris_market_runtime::legacy_interval)
                });
            let interval = requested
                .filter(|interval| {
                    aeris_market_runtime::resolve_series_key(&instrument, *interval, presentation)
                        .is_ok()
                })
                .or_else(|| {
                    let offered = presentation
                        .chart_interval_labels
                        .iter()
                        .filter_map(|label| aeris_market_runtime::persisted_interval(label))
                        .collect::<Vec<_>>();
                    requested
                        .unwrap_or(ChartInterval::Minute1)
                        .nearest_offered(&offered)
                });
            let Some(interval) = interval else {
                notices.push(format!(
                    "Pane {} was skipped because it has no supported chart interval",
                    pane.pane_id
                ));
                continue;
            };
            if requested != Some(interval) {
                notices.push(format!(
                    "Pane {} used an unsupported chart interval and was repaired to {}",
                    pane.pane_id,
                    interval.label()
                ));
            }
            panes.push(RestoredWorkspacePaneSpec {
                workspace_id: tab.workspace_id,
                pane_id: pane.pane_id,
                consumer_id: pane.consumer_id,
                interval,
                instrument,
                restored_viewport: pane
                    .viewport_start_unix_nanos
                    .zip(pane.viewport_end_unix_nanos),
                generation: pane.generation,
            });
        }
    }
    if panes.is_empty() {
        return Err("persisted workspace contains no chart panes".to_string());
    }
    Ok((panes, notices))
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

struct WorkerEndpoint {
    consumer_id: u64,
    messages: MarketWorkerSender,
    commands: mpsc::Receiver<MarketWorkerCommand>,
    shutdown: mpsc::SyncSender<()>,
    pending_recovery: Option<ReplayRecoveryCommand>,
    active_generation: u64,
    resource_class: ConsumerResourceClass,
    depth_visible: bool,
    active: bool,
}

fn start_group(
    market: MarketService,
    configurations: Vec<(u64, InstallProviderInstrument)>,
    trading: aeris_trading_runtime::TradingService,
    clock: Arc<dyn Clock>,
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
        let (worker, endpoint) = worker_endpoint_for_market(
            &market,
            workspace_id,
            pane_id,
            product,
            consumer_id,
            ChartInterval::Minute1,
            (None, INITIAL_GENERATION),
        );
        workers.push((worker.startup, worker.worker));
        endpoints.push(endpoint);
    }
    let (addition_tx, addition_rx) = mpsc::sync_channel(WORKSPACE_ADDITION_CAPACITY);
    spawn_group(
        market.clone(),
        client_id,
        endpoints,
        Some(addition_rx),
        trading,
        clock,
    )?;
    Ok((
        workers,
        WorkspaceMarketFactory {
            market,
            additions: addition_tx,
            next_workspace_id: Arc::new(AtomicU64::new(maximum_workspace_id.saturating_add(1))),
            next_pane_id: Arc::new(AtomicU64::new(maximum_pane_id.saturating_add(1))),
            next_consumer_id: Arc::new(AtomicU64::new(maximum_consumer_id.saturating_add(1))),
        },
    ))
}

fn worker_endpoint_for_market(
    market: &MarketService,
    workspace_id: u64,
    pane_id: u64,
    product: InstallProviderInstrument,
    consumer_id: u64,
    interval: ChartInterval,
    selection: (Option<(i64, i64)>, u64),
) -> (WorkspaceMarketPane, EndpointRecord) {
    let wake_market = market.clone();
    worker_endpoint_with_wake(
        Arc::new(move || wake_market.wake_bridges()),
        workspace_id,
        pane_id,
        product,
        consumer_id,
        interval,
        selection,
    )
}

fn worker_endpoint_with_wake(
    wake: Arc<dyn Fn() + Send + Sync>,
    workspace_id: u64,
    pane_id: u64,
    product: InstallProviderInstrument,
    consumer_id: u64,
    interval: ChartInterval,
    (restored_viewport, initial_generation): (Option<(i64, i64)>, u64),
) -> (WorkspaceMarketPane, EndpointRecord) {
    let (subscription_id, worker_label) = worker_identity(product.provider.as_str());
    let startup = MarketWorkerStartup::Loading(Box::new(
        aeris_desktop::market_worker::EngineWorkerStartup {
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
        .with_command_wake(wake),
    };
    let endpoint = EndpointRecord {
        workspace_id,
        initial_instrument: aeris_market_runtime::InstrumentRequest::startup(&product),
        product,
        interval,
        role: EndpointRole::Market,
        pending_selection: None,
        pending_catalog_selection: None,
        initial_selection_pending: initial_generation != 0,
        session_status_sent: None,
        pending_provider_state: None,
        endpoint: WorkerEndpoint {
            consumer_id,
            messages: message_tx,
            commands: command_rx,
            shutdown: shutdown_tx,
            pending_recovery: None,
            active_generation: initial_generation,
            resource_class: ConsumerResourceClass::Foreground,
            depth_visible: false,
            active: true,
        },
    };
    (worker, endpoint)
}

#[cfg(test)]
fn worker_endpoint(
    workspace_id: u64,
    pane_id: u64,
    product: InstallProviderInstrument,
    consumer_id: u64,
    interval: ChartInterval,
    restored_viewport: Option<(i64, i64)>,
    initial_generation: u64,
) -> (WorkspaceMarketPane, EndpointRecord) {
    worker_endpoint_with_wake(
        Arc::new(|| {}),
        workspace_id,
        pane_id,
        product,
        consumer_id,
        interval,
        (restored_viewport, initial_generation),
    )
}

/// Captures the same initial worker dispatch used by a restored pane, without
/// starting a provider session in the GPUI pane fixture.
#[cfg(test)]
pub(super) fn restored_selection_for_pane_harness(
    product: InstallProviderInstrument,
    interval: ChartInterval,
    consumer_id: u64,
) -> aeris_market_runtime::SelectionRequest {
    let (_, mut record) = worker_endpoint(1, consumer_id, product, consumer_id, interval, None, 1);
    record.initial_instrument = workspace_initial_instrument(&record.product, true);
    let mut requests = Vec::new();
    selection_commands::dispatch_initial_selection(&record, StreamRequirements::BARS, |request| {
        requests.push(request);
        Ok(())
    })
    .expect("restored initial selection dispatch");
    let [request] = requests.as_slice() else {
        panic!("restored endpoint must dispatch exactly one selection");
    };
    request.clone()
}

fn spawn_group(
    market: MarketService,
    client_id: u64,
    mut endpoints: Vec<EndpointRecord>,
    additions: Option<mpsc::Receiver<EndpointRecord>>,
    trading: aeris_trading_runtime::TradingService,
    clock: Arc<dyn Clock>,
) -> Result<(), String> {
    // One presentation worker drains all runtime-owned consumer outboxes for this
    // workspace group. Tab selection and layout edits only change endpoint
    // resource classes or membership; provider sessions remain runtime-owned.
    thread::Builder::new()
        .name("aeris-engine-market-client".to_string())
        .spawn(move || {
            if let Err(error) = run_workers(
                &market,
                client_id,
                &mut endpoints,
                additions,
                &trading,
                clock.as_ref(),
            ) {
                aeris_observability::diagnostic!("Aeris market worker stopped: {error}");
                for record in &endpoints {
                    runtime::report_failure(
                        &record.endpoint.messages,
                        record
                            .role
                            .demands_series()
                            .then_some(record.endpoint.active_generation),
                        error.clone(),
                    );
                }
            }
            // run_workers already acknowledged endpoints it retired. Only an
            // early attach failure leaves active records for this final fallback.
            for record in endpoints
                .into_iter()
                .filter(|record| record.endpoint.active)
            {
                match record.endpoint.shutdown.try_send(()) {
                    Ok(()) | Err(mpsc::TrySendError::Disconnected(())) => {
                        // The waiter may have been released during pane teardown.
                    }
                    Err(mpsc::TrySendError::Full(())) => {
                        aeris_observability::diagnostic!(
                            "Aeris market worker retirement acknowledgement was pending"
                        );
                    }
                }
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// User-visible provider name for worker status and error messages.
fn provider_display_name(provider: &str) -> &str {
    provider_presentation(provider).map_or(provider, |descriptor| descriptor.display_name)
}

fn provider_presentation(
    provider: &str,
) -> Option<&'static aeris_contracts::ProviderPresentationDescriptor> {
    aeris_market_runtime::built_in_provider_presentations()
        .iter()
        .find(|descriptor| descriptor.id == provider)
}

pub(crate) fn worker_identity(provider: &str) -> (&'static str, &'static str) {
    if provider == "hyperliquid" {
        (HYPERLIQUID_SUBSCRIPTION_ID, HYPERLIQUID_WORKER_LABEL)
    } else {
        (SUBSCRIPTION_ID, WORKER_LABEL)
    }
}

#[path = "engine_market_worker/replay_conversion.rs"]
mod replay_conversion;
#[cfg(test)]
use replay_conversion::{replay_bar_definition, snapshot_instrument};
pub(crate) use replay_conversion::{replay_runtime_snapshot, replay_runtime_tail_update};
pub(crate) use replay_conversion::{runtime_order_book_frame, series_key};

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
        price_increment: None,
        ..Default::default()
    }
}

/// Fresh-install default: Hyperliquid BTC perpetual, one-minute candles.
#[cfg(test)]
fn default_hyperliquid_product() -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: "hyperliquid".to_string(),
        session_generation: 1,
        selection_generation: 1,
        instrument_id: "hyperliquid:perp:BTC".to_string(),
        provider_symbol: "BTC".to_string(),
        display_symbol: "BTC-USDC".to_string(),
        venue_id: "Hyperliquid".to_string(),
        price_scale: 8,
        quantity_scale: 8,
        entitlement_id: "hyperliquid-public".to_string(),
        price_increment: None,
        ..Default::default()
    }
}

#[cfg(test)]
fn products() -> Vec<InstallProviderInstrument> {
    ["MNQ", "ES"].into_iter().map(default_product).collect()
}

fn demand_error(error: &MarketDemandError) -> String {
    let class = match error.code {
        EngineFaultCode::Retryable => "retryable",
        EngineFaultCode::Offline => "offline",
        EngineFaultCode::Cancelled => "cancelled",
        EngineFaultCode::Permanent => "permanent",
        EngineFaultCode::CorruptLocalState => "corrupt local state",
        EngineFaultCode::Unauthenticated => "unauthenticated",
    };
    let stage = failure_stage_label(error.stage);
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
        FailureStage::Handoff => "history/live handoff",
        FailureStage::Publication => "publication",
        FailureStage::ChartInstall => "chart install",
        FailureStage::ProviderRealtime => "provider realtime",
    }
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
use runtime::{retire_endpoint, run_workers};

#[path = "engine_market_worker/selection_commands.rs"]
mod selection_commands;
#[cfg(test)]
use selection_commands::retain_depth_visibility;
use selection_commands::{
    initialize_catalog_endpoint, initialize_endpoint, initialize_market_screen_endpoint,
    process_command,
};

#[path = "engine_market_worker/publications.rs"]
mod publications;
use publications::{cancel_pending_recovery, complete_pending_recovery, send_recovery};

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_chart_integration::AerisChartView;
    use aeris_contracts::{
        ProviderInstrumentSearchResult, ProviderInstrumentSummary, SelectProviderInstrument,
    };
    use aeris_design_system::AerisTheme;
    use aeris_market_data::{DepthLevel, OrderBookPublication, OrderBookState};
    use aeris_market_runtime::fence::{ConsumerTag, EpochSequence, Fenced};
    use aeris_market_runtime::{
        CanonicalMarketSeriesSnapshot, MarketConsumerId, MarketGenerationId,
        MarketProviderGeneration, MarketProviderInstrumentSelection, MarketTradeTapeSnapshot,
    };
    use std::collections::BTreeMap;

    fn fenced_event(value: MarketRuntimeEvent) -> aeris_market_runtime::MarketConsumerEvent {
        Fenced {
            epoch: EpochSequence::<ConsumerTag>::default().next(),
            value,
        }
    }

    fn apply_provider_state(
        state: &ProviderState,
        expected_provider: &str,
        selection_generation: u64,
        realtime: bool,
        messages: &MarketWorkerSender,
    ) -> Result<(), String> {
        if !realtime {
            return Ok(());
        }
        runtime::deliver_runtime_provider_state(
            &series_endpoint(messages.clone(), 1, selection_generation),
            expected_provider,
            fenced_event(MarketRuntimeEvent::ProviderState(state.clone())),
        )
    }

    fn series_endpoint(
        messages: MarketWorkerSender,
        consumer_id: u64,
        generation: u64,
    ) -> WorkerEndpoint {
        let (_command_sender, commands) = mpsc::channel();
        let (shutdown, _shutdown_receiver) = mpsc::sync_channel(1);
        WorkerEndpoint {
            consumer_id,
            messages,
            commands,
            shutdown,
            pending_recovery: None,
            active_generation: generation,
            resource_class: ConsumerResourceClass::Foreground,
            depth_visible: false,
            active: true,
        }
    }

    #[test]
    fn depth_visible_chart_explicitly_demands_real_trades() {
        let streams = chart_streams(true);
        assert!(streams.contains(MarketStream::Bars));
        assert!(streams.contains(MarketStream::Depth));
        assert!(streams.contains(MarketStream::Trades));
        assert!(chart_streams(false).contains(MarketStream::Trades));
    }

    #[test]
    fn depth_visibility_during_startup_resolution_is_retained_for_the_resolved_demand() {
        let product = default_hyperliquid_product();
        let (_pane, mut record) =
            worker_endpoint(1, 2, product.clone(), 41, ChartInterval::Minute1, None, 5);

        // The order book opens while the restored instrument is still being
        // resolved: no demand exists yet, so the intent is retained for selection.
        assert_eq!(
            retain_depth_visibility(&mut record.endpoint, true, true),
            None
        );
        assert!(record.endpoint.depth_visible);
        assert!(chart_streams(record.endpoint.depth_visible).contains(MarketStream::Depth));

        let streams = retain_depth_visibility(&mut record.endpoint, false, false)
            .expect("installed demand receives the new streams");
        assert!(!streams.contains(MarketStream::Depth));
        assert!(!record.endpoint.depth_visible);
    }

    #[test]
    fn depth_opened_before_a_restored_selection_resolves_waits_for_the_selection() {
        let market = MarketService::start().expect("market runtime");
        let (_pane, mut record) = worker_endpoint(
            1,
            2,
            default_hyperliquid_product(),
            41,
            ChartInterval::Minute1,
            None,
            7,
        );
        assert!(record.initial_selection_pending);

        // The restored generation has no demand until `select_and_demand` replies, so
        // the intent must be retained for `apply_depth_visibility` instead of being
        // applied to the unresolved generation and lost.
        process_command(
            &market,
            1,
            &mut record,
            MarketWorkerCommand::DepthVisible(true),
        )
        .expect("visibility is retained without touching unresolved demand");
        assert!(record.endpoint.depth_visible);
    }

    #[allow(clippy::too_many_arguments)]
    fn runtime_snapshot(
        consumer_id: u64,
        generation: u64,
        series: BarSeriesKey,
        provider_generation: u64,
        price_scale: u32,
        quantity_scale: u32,
        bars: Vec<MarketBar>,
        publication_generation: u64,
        forming: bool,
    ) -> MarketSeriesSnapshot {
        MarketSeriesSnapshot {
            consumer_id: MarketConsumerId(NonZeroU64::new(consumer_id).expect("consumer id")),
            generation: MarketGenerationId(NonZeroU64::new(generation).expect("generation")),
            publication_generation,
            snapshot: Arc::new(CanonicalMarketSeriesSnapshot {
                series,
                provider_generation: MarketProviderGeneration(
                    NonZeroU64::new(provider_generation).expect("provider generation"),
                ),
                publication_generation,
                price_scale: u8::try_from(price_scale).expect("price scale"),
                quantity_scale: u8::try_from(quantity_scale).expect("quantity scale"),
                forming,
                bars: Arc::from(bars),
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn runtime_update(
        consumer_id: u64,
        generation: u64,
        series: BarSeriesKey,
        provider_generation: u64,
        bar: MarketBar,
        forming: bool,
        publication_generation: u64,
        operation: SeriesTailOperation,
    ) -> MarketSeriesUpdate {
        MarketSeriesUpdate {
            consumer_id: MarketConsumerId(NonZeroU64::new(consumer_id).expect("consumer id")),
            generation: MarketGenerationId(NonZeroU64::new(generation).expect("generation")),
            publication_generation,
            series,
            provider_generation: MarketProviderGeneration(
                NonZeroU64::new(provider_generation).expect("provider generation"),
            ),
            forming,
            operation,
            bar,
        }
    }

    #[test]
    fn runtime_bar_bridge_keeps_coordinator_envelopes_and_rejects_retired_selection() {
        use aeris_market_runtime::fence::{ConsumerTag, EpochSequence, Fenced};
        let product = default_hyperliquid_product();
        let (mut pane, record) =
            worker_endpoint(1, 2, product.clone(), 41, ChartInterval::Minute1, None, 3);
        let series = series_key(&product, ChartInterval::Minute1).expect("series");
        let bar = MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 60,
            exchange_timestamp_unix_nanos: 60_000_000_000,
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 7,
        };
        let mut epochs = EpochSequence::<ConsumerTag>::default();
        let epoch = epochs.next();
        let snapshot = |generation| Fenced {
            epoch,
            value: MarketRuntimeEvent::SeriesSnapshot(runtime_snapshot(
                41,
                generation,
                series.clone(),
                product.session_generation,
                product.price_scale,
                product.quantity_scale,
                vec![bar],
                1,
                false,
            )),
        };
        runtime::deliver_runtime_series_publication(&record.endpoint, snapshot(2), 10)
            .expect("retired snapshot is ignored");
        runtime::deliver_runtime_series_publication(&record.endpoint, snapshot(3), 11)
            .expect("current snapshot is queued");
        let tail = Fenced {
            epoch,
            value: MarketRuntimeEvent::SeriesUpdate(runtime_update(
                41,
                3,
                series,
                product.session_generation,
                bar,
                true,
                2,
                SeriesTailOperation::Append,
            )),
        };
        runtime::deliver_runtime_series_publication(&record.endpoint, tail, 12)
            .expect("current tail is queued after snapshot");
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::RuntimeProjection(aeris_desktop::market_worker::RuntimeProjection { event: Fenced {
                        epoch: snapshot_epoch,
                        value: MarketRuntimeEvent::SeriesSnapshot(_),
                    }, received_unix_nanos: Some(11), recovery_request_id: None, .. }),
                MarketWorkerMessage::RuntimeProjection(aeris_desktop::market_worker::RuntimeProjection { event: Fenced {
                        epoch: tail_epoch,
                        value: MarketRuntimeEvent::SeriesUpdate(_),
                    }, received_unix_nanos: Some(12), recovery_request_id: None, .. }),
            ] if *snapshot_epoch == epoch && *tail_epoch == epoch
        ));
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
    fn workspace_endpoint_keeps_click_before_queued_viewport_and_engine_selection() {
        let product = default_hyperliquid_product();
        let (pane, record) =
            worker_endpoint(2, 9, product.clone(), 41, ChartInterval::Minute1, None, 7);
        let provider_selection = SelectProviderInstrument {
            startup_resolve: false,
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
            .expect("provider selection queued");
        pane.worker
            .try_set_chart_viewport(100, 200)
            .expect("viewport queued after click");
        assert!(matches!(
            record.endpoint.commands.try_recv(),
            Ok(MarketWorkerCommand::ProviderSelect(selection)) if selection == provider_selection
        ));
        assert!(matches!(
            record.endpoint.commands.try_recv(),
            Ok(MarketWorkerCommand::ChartViewport(viewport))
                if viewport.start_unix_nanos == 100 && viewport.end_unix_nanos == 200
        ));

        assert_eq!(
            pane.worker
                .try_select_instrument(product, ChartInterval::Minute5)
                .expect("engine selection queued"),
            8
        );
        let MarketWorkerCommand::EngineSelect(pending_engine) = record
            .endpoint
            .commands
            .try_recv()
            .expect("engine selection")
        else {
            panic!("expected ordered engine selection");
        };
        assert_eq!(pending_engine.sequence, 8);
        assert_eq!(pending_engine.interval, ChartInterval::Minute5);
    }

    #[test]
    fn catalog_only_rithmic_endpoint_defers_demand_and_starts_selection_at_one() {
        let placeholder = default_product("MNQ");
        let (mut pane, mut record) = worker_endpoint(
            DEFAULT_WORKSPACE_ID,
            9,
            placeholder,
            41,
            ChartInterval::Minute1,
            None,
            0,
        );
        pane.startup = MarketWorkerStartup::Rithmic;
        record.role = EndpointRole::RithmicCatalog;

        assert!(matches!(pane.startup, MarketWorkerStartup::Rithmic));
        assert!(!record.role.demands_series());
        assert_eq!(record.endpoint.active_generation, 0);

        let selected = default_product("ES");
        assert_eq!(
            pane.worker
                .try_select_instrument(selected.clone(), ChartInterval::Minute5)
                .expect("first catalog selection is dispatchable"),
            1
        );
        let MarketWorkerCommand::EngineSelect(pending) = record
            .endpoint
            .commands
            .try_recv()
            .expect("selection queued")
        else {
            panic!("expected ordered catalog selection");
        };
        assert_eq!(pending.sequence, 1);
        assert_eq!(pending.product, selected);
        assert_eq!(pending.interval, ChartInterval::Minute5);
    }

    #[test]
    #[ignore = "requires the optimized market runtime and live Hyperliquid public access"]
    fn native_market_runtime_hyperliquid_startup_resolves_stale_default_generation() {
        let (_startup, mut worker, factory) = start().expect("start Hyperliquid market worker");
        drop(factory);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let (messages, disconnected) = worker.drain_messages();
            for message in messages {
                match message {
                    MarketWorkerMessage::RuntimeProjection(
                        aeris_desktop::market_worker::RuntimeProjection {
                            event:
                                Fenced {
                                    value: MarketRuntimeEvent::SeriesSnapshot(snapshot),
                                    ..
                                },
                            ..
                        },
                    ) => {
                        assert!(!snapshot.snapshot.bars.is_empty());
                        return;
                    }
                    MarketWorkerMessage::Runtime(Fenced {
                        value:
                            MarketRuntimeEvent::SeriesState(MarketSeriesState {
                                state: SeriesLoadState::Failed,
                                detail,
                                ..
                            }),
                        ..
                    }) => panic!("Hyperliquid market runtime startup failed: {detail:?}"),
                    MarketWorkerMessage::LocalFailure {
                        scope: aeris_desktop::market_worker::LocalFailureScope::Selection(_),
                        message,
                    } => {
                        panic!("Hyperliquid market endpoint failed: {message}")
                    }
                    _ => {}
                }
            }
            assert!(!disconnected, "Hyperliquid market worker disconnected");
            assert!(
                Instant::now() < deadline,
                "Hyperliquid market runtime did not publish a covering snapshot"
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

        let epoch = EpochSequence::<ConsumerTag>::default().next();
        assert!(!runtime::deliver_provider_catalog_event(
            &mut record,
            Fenced {
                epoch,
                value: MarketRuntimeEvent::ProviderInstrumentSearchResult(
                    ProviderInstrumentSearchResult {
                        consumer_id: 41,
                        provider: "rithmic".to_string(),
                        provider_generation: 1,
                        search_generation: 3,
                        instruments: vec![ProviderInstrumentSummary {
                            symbol: "MNQ".to_string(),
                            display_symbol: "MNQ".to_string(),
                            exchange: "CME".to_string(),
                            ..ProviderInstrumentSummary::default()
                        }],
                    },
                )
            },
        ));
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Runtime(Fenced {
                epoch: received_epoch,
                value: MarketRuntimeEvent::ProviderInstrumentSearchResult(result),
            })] if *received_epoch == epoch && result.search_generation == 3
        ));

        let mut selected = product.clone();
        selected.selection_generation = 5;
        assert!(runtime::deliver_provider_catalog_event(
            &mut record,
            Fenced {
                epoch,
                value: MarketRuntimeEvent::ProviderInstrumentSelection(
                    MarketProviderInstrumentSelection {
                        consumer_id: MarketConsumerId(
                            NonZeroU64::new(41).expect("nonzero consumer")
                        ),
                        instrument: selected,
                        command_generation: 4,
                    }
                )
            },
        ));
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Runtime(Fenced {
                epoch: received_epoch,
                value: MarketRuntimeEvent::ProviderInstrumentSelection(
                    MarketProviderInstrumentSelection { command_generation: 4, instrument, .. }
                ),
            })] if *received_epoch == epoch && instrument.selection_generation == 5
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
    fn restored_hyperliquid_day3_pane_keeps_its_native_interval() {
        // Use the historical protobuf wire values so this stays a reader fixture,
        // rather than another desktop cadence construction site.
        for (cadence, cadence_value) in [(1, 259_200), (3, 3)] {
            let series = SeriesKey {
                cadence,
                cadence_value,
                ..SeriesKey::default()
            };
            let pane = WorkspacePaneState {
                pane_id: 7,
                consumer_id: 9,
                kind: WorkspacePaneKind::Chart as i32,
                instrument: Some(default_hyperliquid_product()),
                series: Some(series),
                interval: String::new(),
                generation: 1,
                ..WorkspacePaneState::default()
            };
            let workspace = WorkspaceState {
                workspace_tabs: vec![WorkspaceTabState {
                    workspace_id: 1,
                    panes: vec![pane],
                    ..WorkspaceTabState::default()
                }],
                ..WorkspaceState::default()
            };
            let plan = restored_workspace_boot_plan(&workspace).expect("legacy Day3 restores");
            assert_eq!(plan.panes[0].interval, ChartInterval::Day3);
            assert_eq!(plan.notices, Vec::<String>::new());
        }
    }

    #[test]
    fn restored_panes_preserve_each_persisted_instrument_ref() {
        use aeris_market_runtime::{InstrumentRef, InstrumentRequest};

        let instruments = ["BTC", "ETH", "SOL", "HYPE"]
            .into_iter()
            .enumerate()
            .map(|(index, symbol)| {
                let mut product = default_hyperliquid_product();
                product.instrument_id = format!("hyperliquid:perp:{symbol}");
                product.provider_symbol = symbol.into();
                product.display_symbol = format!("{symbol}-USDC");
                product.entitlement_id = format!("saved-entitlement-{index}");
                product
            })
            .collect::<Vec<_>>();
        let workspace = WorkspaceState {
            workspace_tabs: vec![WorkspaceTabState {
                workspace_id: 1,
                panes: instruments
                    .iter()
                    .enumerate()
                    .map(|(index, product)| WorkspacePaneState {
                        pane_id: index as u64 + 1,
                        consumer_id: index as u64 + 1,
                        kind: WorkspacePaneKind::Chart as i32,
                        instrument: Some(product.clone()),
                        interval: ChartInterval::Minute1.label().into(),
                        generation: 1,
                        ..WorkspacePaneState::default()
                    })
                    .collect(),
                ..WorkspaceTabState::default()
            }],
            ..WorkspaceState::default()
        };
        let plan = restored_workspace_boot_plan(&workspace).expect("four panes");
        assert_eq!(plan.panes.len(), 4);
        let mut requests = Vec::new();
        for (pane, expected) in plan.panes.iter().zip(&instruments) {
            let (worker, mut record) = worker_endpoint(
                pane.workspace_id,
                pane.pane_id,
                pane.instrument.clone(),
                pane.consumer_id,
                pane.interval,
                pane.restored_viewport,
                pane.generation,
            );
            record.initial_instrument = workspace_initial_instrument(&record.product, true);
            assert_eq!(
                record.initial_instrument,
                InstrumentRequest::Ref(InstrumentRef {
                    provider_id: expected.provider.clone(),
                    instrument_id: expected.instrument_id.clone(),
                    entitlement_id: expected.entitlement_id.clone(),
                    query: Some(expected.provider_symbol.clone()),
                })
            );
            assert!(record.initial_selection_pending);
            requests.push(assert_restored_pane_dispatch_and_reply(
                pane, expected, worker, record,
            ));
        }
        assert_eq!(requests.len(), 4, "one initial command for each saved pane");
        assert!(matches!(
            workspace_initial_instrument(&instruments[0], false),
            InstrumentRequest::Default
        ));
    }

    fn assert_restored_pane_dispatch_and_reply(
        pane: &RestoredWorkspacePaneSpec,
        expected: &InstallProviderInstrument,
        mut worker: WorkspaceMarketPane,
        mut record: EndpointRecord,
    ) -> aeris_market_runtime::SelectionRequest {
        use aeris_market_runtime::{
            InstrumentDescriptor, MarketGenerationId, ResolvedSeries, SelectionOutcome,
        };

        let mut requests = Vec::new();
        selection_commands::dispatch_initial_selection(
            &record,
            StreamRequirements::BARS,
            |request| {
                requests.push(request);
                Ok(())
            },
        )
        .expect("one initial dispatch");
        let [request] = requests.as_slice() else {
            panic!("one request per restored pane");
        };
        assert_eq!(request.consumer_id, pane.consumer_id);
        assert_eq!(request.generation, pane.generation);
        assert_eq!(request.interval, pane.interval);
        assert_eq!(request.instrument, record.initial_instrument);
        let presentation = provider_presentation(&expected.provider).expect("restored provider");
        let outcome = || SelectionOutcome::Selected {
            generation: MarketGenerationId(
                NonZeroU64::new(pane.generation).expect("restored generation"),
            ),
            descriptor: Box::new(
                InstrumentDescriptor::from_install(expected, presentation, true)
                    .expect("restored descriptor"),
            ),
            series: ResolvedSeries::resolve(expected, pane.interval, presentation)
                .expect("restored series"),
        };
        runtime::apply_selection_outcome(&mut record, outcome()).expect("first reply");
        runtime::apply_selection_outcome(&mut record, outcome()).expect("duplicate reply");
        assert!(matches!(
            worker.worker.drain_messages().0.as_slice(),
            [MarketWorkerMessage::LocalCommandOutcome(
                aeris_desktop::market_worker::LocalCommandOutcome::SelectedInstrument {
                    generation,
                    instrument
                }
            )]
                if *generation == pane.generation && instrument == expected
        ));
        assert!(!record.initial_selection_pending);
        requests.pop().expect("one dispatched request")
    }

    #[test]
    fn one_unsupported_pane_repairs_without_discarding_other_panes() {
        let pane = |pane_id, interval: &str| WorkspacePaneState {
            pane_id,
            consumer_id: pane_id,
            kind: WorkspacePaneKind::Chart as i32,
            instrument: Some(default_hyperliquid_product()),
            interval: interval.to_string(),
            generation: 1,
            ..WorkspacePaneState::default()
        };
        let workspace = WorkspaceState {
            workspace_tabs: vec![WorkspaceTabState {
                workspace_id: 1,
                panes: vec![pane(1, "1m"), pane(2, "100t")],
                ..WorkspaceTabState::default()
            }],
            ..WorkspaceState::default()
        };

        let plan = restored_workspace_boot_plan(&workspace).expect("workspace repairs");
        assert_eq!(plan.panes.len(), 2);
        assert_eq!(plan.panes[0].interval, ChartInterval::Minute1);
        assert_eq!(plan.panes[1].interval, ChartInterval::Minute1);
        assert_eq!(plan.notices.len(), 1);
        assert!(plan.notices[0].contains("Pane 2"));
    }

    #[test]
    fn runtime_snapshot_preserves_precision_exact_time_generation_and_engine_provenance() {
        let publication = runtime_snapshot(
            1,
            1,
            series_key(
                products().first().expect("BTC product"),
                ChartInterval::Minute1,
            )
            .expect("series"),
            8,
            2,
            8,
            vec![MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 1_700_000_000,
                exchange_timestamp_unix_nanos: 1_700_000_000_123_456_789,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            }],
            4,
            false,
        );
        let snapshot =
            replay_runtime_snapshot(&publication, None, 1_000_000_000).expect("snapshot converts");
        assert_eq!(snapshot.instrument().precision.price_scale(), 2);
        assert_eq!(snapshot.instrument().precision.quantity_scale(), 8);
        assert_eq!(snapshot.evidence().session_generation, 8);
        assert_eq!(snapshot.evidence().publication_generation, 4);
        assert_eq!(
            snapshot.bars()[0]
                .provenance()
                .exchange_timestamp_unix_nanos,
            1_700_000_000_123_456_789
        );
        assert_eq!(snapshot.bars()[0].provenance().producer, "aeris_engine");
        assert_eq!(
            snapshot.bars()[0].provenance().publication_time_unix_nanos,
            1_000_000_000
        );
    }

    #[test]
    fn runtime_live_update_preserves_one_tail_without_rebuilding_history() {
        let update = replay_runtime_tail_update(
            &runtime_update(
                1,
                2,
                series_key(
                    products().first().expect("BTC product"),
                    ChartInterval::Minute1,
                )
                .expect("series"),
                7,
                MarketBar {
                    source_sequence: 3,
                    exchange_timestamp_seconds: 120,
                    exchange_timestamp_unix_nanos: 120_000_000_000,
                    open: 100,
                    high: 120,
                    low: 90,
                    close: 115,
                    volume: 9,
                },
                true,
                8,
                SeriesTailOperation::Append,
            ),
            1_000_000_000,
        )
        .expect("tail converts");
        assert_eq!(update.item().value().source_sequence, 3);
        assert_eq!(update.item().value().close, 115);
        assert_eq!(
            update.item().provenance().publication_time_unix_nanos,
            1_000_000_000
        );
        assert_eq!(update.publication_generation(), 8);
        assert!(update.forming());
    }

    fn drained_states(
        receiver: &aeris_desktop::market_worker::MarketWorkerReceiver,
    ) -> Vec<(SeriesLoadState, Option<String>)> {
        receiver
            .drain()
            .0
            .into_iter()
            .filter_map(|message| match message {
                MarketWorkerMessage::Runtime(Fenced {
                    value: MarketRuntimeEvent::SeriesState(state),
                    ..
                }) => {
                    assert!(state.generation.0.get() > 0);
                    Some((state.state, state.detail))
                }
                _ => None,
            })
            .collect()
    }

    /// The engine says outright that a series serving retained canonical history is
    /// not ready. Swallowing that is what showed a stale chart as current.
    #[test]
    fn retained_partial_history_and_live_repair_keep_fenced_readiness_order() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(16).unwrap_or(NonZeroUsize::MIN));
        let endpoint = series_endpoint(sender, 1, 1);
        let state = |load_state: SeriesLoadState, detail: Option<&str>| MarketSeriesState {
            consumer_id: MarketConsumerId(NonZeroU64::MIN),
            generation: MarketGenerationId(NonZeroU64::MIN),
            series: None,
            state: load_state,
            detail: detail.map(str::to_string),
            presentation: None,
        };

        runtime::deliver_runtime_series_event(
            &endpoint,
            fenced_event(MarketRuntimeEvent::SeriesState(state(
                SeriesLoadState::Partial,
                Some("Showing retained canonical history while provider coverage repairs"),
            ))),
        )
        .expect("a partial series is not a failure");
        assert_eq!(
            drained_states(&receiver),
            vec![(
                SeriesLoadState::Partial,
                Some(
                    "Showing retained canonical history while provider coverage repairs"
                        .to_string()
                )
            )],
            "retained history has to read as loading, with the engine's own reason"
        );

        runtime::deliver_runtime_series_event(
            &endpoint,
            fenced_event(MarketRuntimeEvent::SeriesState(state(
                SeriesLoadState::Ready,
                None,
            ))),
        )
        .expect("provider history can precede the live handoff");
        assert_eq!(
            drained_states(&receiver),
            vec![(SeriesLoadState::Ready, None)],
            "the replacement stays covered until the trade handoff is live"
        );

        runtime::deliver_runtime_series_event(
            &endpoint,
            fenced_event(MarketRuntimeEvent::SeriesState(state(
                SeriesLoadState::Live,
                None,
            ))),
        )
        .expect("the handoff completes");
        assert_eq!(
            drained_states(&receiver),
            vec![(SeriesLoadState::Live, None)]
        );

        // The same state now means a backfill repairing history behind a chart
        // that is streaming. Reporting that as loading would flap the surface on
        // every viewport repair.
        runtime::deliver_runtime_series_event(
            &endpoint,
            fenced_event(MarketRuntimeEvent::SeriesState(state(
                SeriesLoadState::Partial,
                Some("visible coverage repairs"),
            ))),
        )
        .expect("a backfill is not a failure");
        assert_eq!(
            drained_states(&receiver),
            vec![(
                SeriesLoadState::Partial,
                Some("visible coverage repairs".to_string())
            )],
            "the pane's readiness projection, not the bridge, suppresses live backfill loading"
        );
    }

    /// A provider that answers with no history (a closed market) is settled,
    /// not failing. Presenting it as recovering invalidated the replay stream
    /// and re-requested the identical empty history about once a minute.
    #[test]
    fn empty_provider_history_keeps_its_fenced_readiness_reason() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
        let endpoint = series_endpoint(sender, 1, 1);
        let empty = MarketSeriesState {
            consumer_id: MarketConsumerId(NonZeroU64::MIN),
            generation: MarketGenerationId(NonZeroU64::MIN),
            series: None,
            state: SeriesLoadState::Empty,
            detail: Some("waiting for live trades".to_string()),
            presentation: None,
        };

        runtime::deliver_runtime_series_event(
            &endpoint,
            fenced_event(MarketRuntimeEvent::SeriesState(empty.clone())),
        )
        .expect("an empty provider answer is not a failure");
        assert_eq!(
            drained_states(&receiver),
            vec![(
                SeriesLoadState::Empty,
                Some("waiting for live trades".to_string())
            )]
        );
        runtime::deliver_runtime_series_event(
            &endpoint,
            fenced_event(MarketRuntimeEvent::SeriesState(MarketSeriesState {
                detail: None,
                ..empty
            })),
        )
        .expect("an empty provider answer is not a failure");
        let states = drained_states(&receiver);
        assert_eq!(states, vec![(SeriesLoadState::Empty, None)]);
        assert!(
            states
                .iter()
                .all(|(state, _)| *state == SeriesLoadState::Empty)
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
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
        let endpoint = series_endpoint(sender, 1, 1);
        let snapshot = |publication_generation: u64, sequence: u64| {
            runtime_snapshot(
                1,
                1,
                series.clone(),
                7,
                2,
                8,
                vec![MarketBar {
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
                false,
            )
        };
        assert_eq!(
            runtime::deliver_runtime_series_publication(
                &endpoint,
                fenced_event(MarketRuntimeEvent::SeriesSnapshot(snapshot(4, 2))),
                1_000_000_000,
            ),
            Ok(())
        );
        // The thin client forwards covering images. The chart bridge owns stale
        // publication rejection rather than a second model in this worker.
        for superseded in [snapshot(1, 2), snapshot(4, 1)] {
            assert_eq!(
                runtime::deliver_runtime_series_publication(
                    &endpoint,
                    fenced_event(MarketRuntimeEvent::SeriesSnapshot(superseded)),
                    1_000_000_000,
                ),
                Ok(()),
                "a superseded covering snapshot must not take the chart down"
            );
        }
        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        let forwarded = messages
            .into_iter()
            .filter_map(|message| match message {
                MarketWorkerMessage::RuntimeProjection(
                    aeris_desktop::market_worker::RuntimeProjection {
                        event:
                            Fenced {
                                value: MarketRuntimeEvent::SeriesSnapshot(snapshot),
                                ..
                            },
                        ..
                    },
                ) => Some((
                    snapshot.publication_generation,
                    (
                        snapshot
                            .snapshot
                            .bars
                            .first()
                            .expect("first bar")
                            .source_sequence,
                        snapshot
                            .snapshot
                            .bars
                            .last()
                            .expect("last bar")
                            .source_sequence,
                    ),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(forwarded, vec![(4, (2, 2)), (1, (2, 2)), (4, (1, 1))]);
    }

    #[test]
    fn recovery_completes_from_pushed_snapshot_without_blocking_the_shared_reader() {
        let product = default_hyperliquid_product();
        let (mut pane, mut record) =
            worker_endpoint(1, 2, product.clone(), 41, ChartInterval::Minute1, None, 3);
        record.endpoint.pending_recovery = Some(ReplayRecoveryCommand {
            request_id: 9,
            reason: aeris_application::ResnapshotReason::QueueOverflow,
        });
        let snapshot = MarketRuntimeEvent::SeriesSnapshot(runtime_snapshot(
            41,
            3,
            series_key(&product, ChartInterval::Minute1).expect("series"),
            product.session_generation,
            product.price_scale,
            product.quantity_scale,
            vec![MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 60,
                exchange_timestamp_unix_nanos: 60_000_000_000,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            }],
            1,
            false,
        ));

        let fenced = fenced_event(snapshot.clone());
        let epoch = fenced.epoch;
        assert!(
            complete_pending_recovery(&fenced, &product, &mut record.endpoint, 1_000_000_000)
                .expect("pushed recovery completes")
        );
        assert!(record.endpoint.pending_recovery.is_none());
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::RuntimeProjection(aeris_desktop::market_worker::RuntimeProjection { event: aeris_market_runtime::fence::Fenced {
                    epoch: delivered_epoch,
                    value: MarketRuntimeEvent::SeriesSnapshot(snapshot)
                }, recovery_request_id: Some(9), received_unix_nanos: Some(1_000_000_000), .. })] if *delivered_epoch == epoch && snapshot.publication_generation == 1 && snapshot.generation.0.get() == 3
        ));
    }

    #[test]
    fn recovery_acknowledgements_keep_pending_request_under_mailbox_backpressure() {
        let product = default_hyperliquid_product();
        let (mut pane, mut record) =
            worker_endpoint(1, 2, product.clone(), 41, ChartInterval::Minute1, None, 3);
        let pending = ReplayRecoveryCommand {
            request_id: 9,
            reason: aeris_application::ResnapshotReason::QueueOverflow,
        };
        record.endpoint.pending_recovery = Some(pending);
        let sender = record.endpoint.messages.clone();
        let mut count = 0;
        loop {
            match sender.send(MarketWorkerMessage::LocalFailure {
                scope: aeris_desktop::market_worker::LocalFailureScope::Catalog,
                message: "fixture".to_string(),
            }) {
                Ok(()) => count += 1,
                Err(aeris_desktop::market_worker::MarketWorkerSendError::Full) => break,
                Err(error) => panic!("unexpected mailbox failure: {error}"),
            }
            assert!(count < 4096, "control mailbox must be bounded");
        }
        assert!(count > 0);
        assert!(cancel_pending_recovery(&mut record.endpoint).is_err());
        assert_eq!(record.endpoint.pending_recovery, Some(pending));
        assert_eq!(pane.worker.drain_messages().0.len(), count);
        cancel_pending_recovery(&mut record.endpoint).expect("retry delivers cancellation");
        assert!(record.endpoint.pending_recovery.is_none());
        assert!(matches!(
            pane.worker.drain_messages().0.as_slice(),
            [MarketWorkerMessage::LocalCommandOutcome(
                LocalCommandOutcome::Recovery {
                    generation: 3,
                    request_id: 9,
                    failure: MarketRecoveryFailure::Superseded,
                }
            )]
        ));

        record.endpoint.pending_recovery = Some(pending);
        for _ in 0..count {
            sender
                .send(MarketWorkerMessage::LocalFailure {
                    scope: aeris_desktop::market_worker::LocalFailureScope::Catalog,
                    message: "fixture".to_string(),
                })
                .expect("control reserve fills");
        }
        let event = MarketRuntimeEvent::DemandError(MarketDemandError {
            consumer_id: MarketConsumerId(NonZeroU64::new(41).expect("consumer")),
            generation: MarketGenerationId(NonZeroU64::new(3).expect("generation")),
            scope: MarketDemandErrorScope::Series,
            code: EngineFaultCode::Retryable,
            stage: FailureStage::ProviderHistory,
            detail: "Fixture recovery failed".to_string(),
            series: Some(series_key(&product, ChartInterval::Minute1).expect("series")),
            cause: "fixture failure".to_string(),
            elapsed_millis: None,
        });
        assert!(
            complete_pending_recovery(
                &fenced_event(event.clone()),
                &product,
                &mut record.endpoint,
                1_000_000_000
            )
            .is_err()
        );
        assert_eq!(record.endpoint.pending_recovery, Some(pending));
        assert_eq!(pane.worker.drain_messages().0.len(), count);
        assert!(
            complete_pending_recovery(
                &fenced_event(event.clone()),
                &product,
                &mut record.endpoint,
                1_000_000_000
            )
            .expect("retry delivers failure")
        );
        assert!(record.endpoint.pending_recovery.is_none());
        assert!(matches!(
            pane.worker.drain_messages().0.as_slice(),
            [MarketWorkerMessage::LocalCommandOutcome(
                LocalCommandOutcome::Recovery {
                    generation: 3,
                    request_id: 9,
                    failure: MarketRecoveryFailure::Failed(_),
                }
            )]
        ));
    }

    #[test]
    fn engine_selection_cancels_old_recovery_before_new_snapshot_can_complete_it() {
        let product = default_hyperliquid_product();
        let (mut pane, mut record) =
            worker_endpoint(1, 2, product.clone(), 41, ChartInterval::Minute1, None, 3);
        record.endpoint.pending_recovery = Some(ReplayRecoveryCommand {
            request_id: 9,
            reason: aeris_application::ResnapshotReason::QueueOverflow,
        });

        cancel_pending_recovery(&mut record.endpoint).expect("selection cancels old recovery");
        record.endpoint.active_generation = 4;
        let snapshot = MarketRuntimeEvent::SeriesSnapshot(runtime_snapshot(
            41,
            4,
            series_key(&product, ChartInterval::Minute1).expect("series"),
            product.session_generation,
            product.price_scale,
            product.quantity_scale,
            vec![MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 60,
                exchange_timestamp_unix_nanos: 60_000_000_000,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            }],
            1,
            false,
        ));

        assert!(
            !complete_pending_recovery(
                &fenced_event(snapshot),
                &product,
                &mut record.endpoint,
                1_000_000_000
            )
            .expect("new snapshot is not consumed by old recovery")
        );
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::LocalCommandOutcome(
                LocalCommandOutcome::Recovery {
                    generation: 3,
                    request_id: 9,
                    failure: MarketRecoveryFailure::Superseded,
                }
            )]
        ));
    }

    #[test]
    fn queued_recovery_from_previous_generation_is_rejected_if_selection_advances_first() {
        let product = default_hyperliquid_product();
        let (mut pane, mut record) =
            worker_endpoint(1, 2, product.clone(), 41, ChartInterval::Minute1, None, 7);
        pane.worker
            .try_send_recovery(ReplayRecoveryCommand {
                request_id: 10,
                reason: aeris_application::ResnapshotReason::QueueOverflow,
            })
            .expect("recovery queues under generation seven");
        assert_eq!(
            pane.worker
                .try_select_instrument(product, ChartInterval::Minute5)
                .expect("new selection queues after recovery"),
            8
        );
        let queued_recovery = record
            .endpoint
            .commands
            .try_recv()
            .expect("older recovery remains first in the ordered lane");
        let MarketWorkerCommand::EngineSelect(pending) = record
            .endpoint
            .commands
            .try_recv()
            .expect("selection follows recovery in the ordered lane")
        else {
            panic!("expected queued selection");
        };
        record.endpoint.active_generation = pending.sequence;
        let MarketWorkerCommand::Recovery {
            selection_generation,
            command,
        } = queued_recovery
        else {
            panic!("expected queued recovery command");
        };
        assert_eq!(selection_generation, Some(7));

        let fenced = selection_commands::fence_recovery_command(
            &record.endpoint,
            selection_generation,
            command,
        )
        .expect("stale recovery is handled without touching runtime demand");

        assert!(fenced.is_none());
        assert!(record.endpoint.pending_recovery.is_none());
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::LocalCommandOutcome(
                LocalCommandOutcome::Recovery {
                    generation: 7,
                    request_id: 10,
                    failure: MarketRecoveryFailure::Superseded,
                }
            )]
        ));

        let current = ReplayRecoveryCommand {
            request_id: 11,
            reason: aeris_application::ResnapshotReason::QueueOverflow,
        };
        assert_eq!(
            selection_commands::fence_recovery_command(&record.endpoint, Some(8), current)
                .expect("current recovery passes the selection fence"),
            Some(current)
        );
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(messages.is_empty());
    }

    #[test]
    fn non_contiguous_engine_tail_is_left_for_the_chart_validator() {
        let series = series_key(
            products().first().expect("BTC product"),
            ChartInterval::Minute1,
        )
        .expect("series");
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
        let endpoint = series_endpoint(sender, 1, 1);
        let snapshot = runtime_snapshot(
            1,
            1,
            series.clone(),
            7,
            2,
            8,
            vec![MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 60,
                exchange_timestamp_unix_nanos: 60_000_000_000,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            }],
            1,
            false,
        );
        assert_eq!(
            runtime::deliver_runtime_series_publication(
                &endpoint,
                fenced_event(MarketRuntimeEvent::SeriesSnapshot(snapshot)),
                1_000_000_000,
            ),
            Ok(())
        );
        let skipped_tail = runtime_update(
            1,
            1,
            series,
            7,
            MarketBar {
                source_sequence: 3,
                exchange_timestamp_seconds: 180,
                exchange_timestamp_unix_nanos: 180_000_000_000,
                open: 105,
                high: 120,
                low: 100,
                close: 115,
                volume: 9,
            },
            true,
            2,
            SeriesTailOperation::Append,
        );
        assert_eq!(
            runtime::deliver_runtime_series_publication(
                &endpoint,
                fenced_event(MarketRuntimeEvent::SeriesUpdate(skipped_tail)),
                1_000_000_000,
            ),
            Ok(())
        );
        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        let (chart, delivered) = replay_into_a_chart(&messages);
        assert_eq!(delivered, vec![1, 3]);
        assert!(
            chart
                .expect("the covering snapshot builds a chart")
                .replay_bridge_metrics()
                .recovery_pending,
            "the worker must forward the gap and leave recovery ownership to the chart validator"
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
        let endpoint = series_endpoint(sender, 1, 1);

        assert_eq!(
            runtime::deliver_runtime_series_publication(
                &endpoint,
                fenced_event(MarketRuntimeEvent::SeriesSnapshot(runtime_snapshot(
                    1,
                    1,
                    series.clone(),
                    7,
                    2,
                    8,
                    vec![burst_bar(1)],
                    1,
                    false,
                ))),
                1_000_000_000,
            ),
            Ok(())
        );
        for sequence in 2..=BURST {
            assert_eq!(
                runtime::deliver_runtime_series_publication(
                    &endpoint,
                    fenced_event(MarketRuntimeEvent::SeriesUpdate(runtime_update(
                        1,
                        1,
                        series.clone(),
                        7,
                        burst_bar(sequence),
                        sequence == BURST,
                        sequence,
                        SeriesTailOperation::Append,
                    ))),
                    1_000_000_000,
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
    fn replay_into_a_chart(messages: &[MarketWorkerMessage]) -> (Option<AerisChartView>, Vec<u64>) {
        const MARKET_MESSAGES_PER_FRAME: usize = 64;
        let mut chart = None;
        let mut delivered = Vec::new();
        for frame in messages.chunks(MARKET_MESSAGES_PER_FRAME) {
            for message in frame {
                let MarketWorkerMessage::RuntimeProjection(
                    aeris_desktop::market_worker::RuntimeProjection {
                        event,
                        received_unix_nanos: Some(received_unix_nanos),
                        ..
                    },
                ) = message
                else {
                    continue;
                };
                let update = match &event.value {
                    MarketRuntimeEvent::SeriesSnapshot(snapshot) => {
                        replay_runtime_snapshot(snapshot, None, *received_unix_nanos)
                            .map(ReplayStreamUpdate::Snapshot)
                    }
                    MarketRuntimeEvent::SeriesUpdate(tail) => {
                        replay_runtime_tail_update(tail, *received_unix_nanos)
                            .map(ReplayStreamUpdate::Tail)
                    }
                    _ => continue,
                }
                .expect("fenced chart data converts");
                match &update {
                    ReplayStreamUpdate::Snapshot(snapshot) => {
                        delivered.extend(
                            snapshot
                                .bars()
                                .iter()
                                .map(|bar| bar.value().source_sequence),
                        );
                        chart = Some(AerisChartView::with_replay_and_theme(
                            snapshot,
                            AerisTheme::dark(),
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

    fn burst_bar(sequence: u64) -> MarketBar {
        let seconds = i64::try_from(sequence).unwrap_or(i64::MAX) * 60;
        MarketBar {
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
    fn retained_snapshot_from_older_provider_session_reaches_the_chart_baseline() {
        let mut product = default_product("MNQ");
        product.session_generation = 2;
        let series = series_key(&product, ChartInterval::Minute1).expect("series");
        let snapshot = runtime_snapshot(
            1,
            7,
            series,
            1,
            product.price_scale,
            product.quantity_scale,
            vec![MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 60,
                exchange_timestamp_unix_nanos: 60_000_000_000,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            }],
            1,
            false,
        );
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let endpoint = series_endpoint(sender, 1, 7);

        runtime::deliver_runtime_series_publication(
            &endpoint,
            fenced_event(MarketRuntimeEvent::SeriesSnapshot(snapshot)),
            1_000_000_000,
        )
        .expect("runtime-owned retained baseline applies");

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::RuntimeProjection(aeris_desktop::market_worker::RuntimeProjection { event: Fenced {
                    value: MarketRuntimeEvent::SeriesSnapshot(snapshot),
                    ..
                }, .. })] if snapshot.snapshot.provider_generation.0.get() == 1
        ));
    }

    #[test]
    fn engine_recovery_state_remains_explicit_while_history_is_retained() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        apply_provider_state(
            &ProviderState {
                provider: "rithmic".to_string(),
                state: ProviderConnectionState::Recovering,
                generation: 2,
                detail: None,
                transport_rtt_nanos: None,
            },
            "rithmic",
            7,
            true,
            &sender,
        )
        .expect("provider state applies");
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::RuntimeProjection(
                aeris_desktop::market_worker::RuntimeProjection {
                    selection_generation: Some(7),
                    event: Fenced {
                        value: MarketRuntimeEvent::ProviderState(ProviderState {
                            state: ProviderConnectionState::Recovering,
                            transport_rtt_nanos: None,
                            ..
                        }),
                        ..
                    },
                    ..
                }
            )]
        ));
    }

    #[test]
    fn newer_provider_session_reaches_the_connection_presentation() {
        let product = default_product("MNQ");
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        apply_provider_state(
            &ProviderState {
                provider: product.provider.clone(),
                state: ProviderConnectionState::Online,
                generation: product.session_generation + 1,
                detail: None,
                transport_rtt_nanos: None,
            },
            &product.provider,
            7,
            true,
            &sender,
        )
        .expect("new engine-owned provider session");
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::RuntimeProjection(
                aeris_desktop::market_worker::RuntimeProjection {
                    selection_generation: Some(7),
                    event: Fenced {
                        value: MarketRuntimeEvent::ProviderState(ProviderState {
                            state: ProviderConnectionState::Online,
                            ..
                        }),
                        ..
                    },
                    ..
                }
            )]
        ));
    }

    #[test]
    fn engine_online_provider_state_forwards_measured_transport_rtt() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        apply_provider_state(
            &ProviderState {
                provider: "hyperliquid".to_string(),
                state: ProviderConnectionState::Online,
                generation: 4,
                detail: None,
                transport_rtt_nanos: Some(12_500_000),
            },
            "hyperliquid",
            8,
            true,
            &sender,
        )
        .expect("provider state applies");
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::RuntimeProjection(
                aeris_desktop::market_worker::RuntimeProjection {
                    event: Fenced {
                        value: MarketRuntimeEvent::ProviderState(ProviderState {
                            state: ProviderConnectionState::Online,
                            transport_rtt_nanos: Some(12_500_000),
                            ..
                        }),
                        ..
                    },
                    ..
                }
            )]
        ));
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
            ] {
                let series = series_key(product, interval).expect("phase-four series validates");
                assert_eq!(
                    series.period,
                    BarPeriod::time(seconds).expect("fixed period validates")
                );
                assert_eq!(series.instrument_id, product.instrument_id);
            }
            for (interval, period) in [
                (
                    ChartInterval::Day1,
                    BarPeriod::session(1).expect("session day validates"),
                ),
                (
                    ChartInterval::Week1,
                    BarPeriod::week(1).expect("week period validates"),
                ),
                (
                    ChartInterval::Month1,
                    BarPeriod::month(1).expect("month period validates"),
                ),
            ] {
                let series = series_key(product, interval).expect("calendar series validates");
                assert_eq!(series.period, period);
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
                state: ProviderConnectionState::Connecting,
                generation: 1,
                detail: None,
                transport_rtt_nanos: None,
            },
            "rithmic",
            7,
            true,
            &sender,
        )
        .expect("calendar provider state is forwarded");
        assert!(matches!(
            receiver.drain().0.as_slice(),
            [MarketWorkerMessage::RuntimeProjection(
                aeris_desktop::market_worker::RuntimeProjection {
                    event: Fenced {
                        value: MarketRuntimeEvent::ProviderState(ProviderState {
                            state: ProviderConnectionState::Connecting,
                            ..
                        }),
                        ..
                    },
                    ..
                }
            )]
        ));
    }

    #[test]
    fn calendar_series_accepts_the_engine_live_handoff() {
        let product = default_product("MNQ");
        let series = series_key(&product, ChartInterval::Week1).expect("calendar series");
        let snapshot = runtime_snapshot(
            1,
            7,
            series,
            1,
            2,
            8,
            vec![MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 0,
                exchange_timestamp_unix_nanos: 0,
                open: 100,
                high: 100,
                low: 100,
                close: 100,
                volume: 1,
            }],
            1,
            false,
        );
        let (sender, receiver) = market_worker_channel(NonZeroUsize::new(2).expect("capacity"));
        let endpoint = series_endpoint(sender, 1, 7);
        runtime::deliver_runtime_series_publication(
            &endpoint,
            fenced_event(MarketRuntimeEvent::SeriesSnapshot(snapshot)),
            1_000_000_000,
        )
        .expect("calendar snapshot applies");

        runtime::deliver_runtime_series_event(
            &endpoint,
            fenced_event(MarketRuntimeEvent::SeriesState(MarketSeriesState {
                consumer_id: MarketConsumerId(NonZeroU64::MIN),
                generation: MarketGenerationId(NonZeroU64::new(7).expect("generation validates")),
                series: None,
                state: SeriesLoadState::Live,
                detail: None,
                presentation: None,
            })),
        )
        .expect("calendar live state applies");
        assert!(matches!(
            receiver.drain().0.as_slice(),
            [
                MarketWorkerMessage::RuntimeProjection(
                    aeris_desktop::market_worker::RuntimeProjection { .. }
                ),
                MarketWorkerMessage::Runtime(Fenced {
                    value: MarketRuntimeEvent::SeriesState(MarketSeriesState {
                        state: SeriesLoadState::Live,
                        ..
                    }),
                    ..
                })
            ]
        ));
    }

    #[test]
    fn replacement_engine_snapshots_are_forwarded_without_a_worker_generation_fence() {
        let product = default_product("MNQ");
        let series = series_key(&product, ChartInterval::Minute1).expect("series");
        let snapshot = |provider_generation, publication_generation, source_sequence| {
            MarketRuntimeEvent::SeriesSnapshot(runtime_snapshot(
                1,
                1,
                series.clone(),
                provider_generation,
                2,
                8,
                vec![MarketBar {
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
                false,
            ))
        };
        let (sender, receiver) = market_worker_channel(NonZeroUsize::new(4).unwrap());
        let endpoint = series_endpoint(sender, 1, 1);
        assert_eq!(
            runtime::deliver_runtime_series_publication(
                &endpoint,
                fenced_event(snapshot(9, 12, 12)),
                1_000_000_000,
            ),
            Ok(())
        );

        assert_eq!(
            runtime::deliver_runtime_series_publication(
                &endpoint,
                fenced_event(snapshot(1, 1, 1)),
                1_000_000_000,
            ),
            Ok(())
        );
        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        let generations = messages
            .into_iter()
            .filter_map(|message| match message {
                MarketWorkerMessage::RuntimeProjection(
                    aeris_desktop::market_worker::RuntimeProjection {
                        event:
                            Fenced {
                                value: MarketRuntimeEvent::SeriesSnapshot(snapshot),
                                ..
                            },
                        ..
                    },
                ) => Some(snapshot.publication_generation),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(generations, vec![12, 1]);
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
    fn rithmic_series_keys_accept_the_selected_market_entitlement() {
        // Catalog selections install the provider's registered selection
        // entitlement; studies and recovery key their series on it.
        let entitlement = provider_presentation("rithmic")
            .expect("Rithmic presentation")
            .selection_entitlement_id;
        let mut product = default_product("MNQZ6");
        product.entitlement_id = entitlement.to_string();
        let series = series_key(&product, ChartInterval::Minute1).expect("selected series");
        assert_eq!(series.entitlement_id, entitlement);
        product.entitlement_id = " ".to_string();
        assert!(series_key(&product, ChartInterval::Minute1).is_err());
    }

    #[test]
    fn structured_demand_errors_render_stage_and_elapsed_context() {
        let error = MarketDemandError {
            consumer_id: MarketConsumerId(NonZeroU64::MIN),
            generation: MarketGenerationId(NonZeroU64::new(2).expect("generation validates")),
            scope: MarketDemandErrorScope::Series,
            code: EngineFaultCode::Retryable,
            stage: FailureStage::Handoff,
            detail: "history/live handoff failed".to_string(),
            series: None,
            cause: "history and realtime state could not be joined safely".to_string(),
            elapsed_millis: Some(17),
        };
        assert_eq!(
            demand_error(&error),
            "history/live handoff failed (retryable) after 17 ms: history/live handoff failed"
        );
    }

    #[test]
    fn retryable_and_permanent_demand_errors_retain_their_fenced_classification() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::new(2).expect("capacity"));
        let endpoint = series_endpoint(sender, 1, 2);
        let retryable = MarketDemandError {
            consumer_id: MarketConsumerId(NonZeroU64::MIN),
            generation: MarketGenerationId(NonZeroU64::new(2).expect("generation validates")),
            scope: MarketDemandErrorScope::Series,
            code: EngineFaultCode::Retryable,
            stage: FailureStage::ProviderHistory,
            detail: "covering history is retrying".to_string(),
            series: None,
            cause: "provider history was unavailable".to_string(),
            elapsed_millis: Some(4),
        };

        assert_eq!(
            runtime::deliver_runtime_series_event(
                &endpoint,
                fenced_event(MarketRuntimeEvent::DemandError(retryable.clone())),
            ),
            Ok(())
        );
        assert!(matches!(
            receiver.drain().0.as_slice(),
            [MarketWorkerMessage::Runtime(Fenced {
                value: MarketRuntimeEvent::DemandError(MarketDemandError {
                    code: EngineFaultCode::Retryable,
                    ..
                }),
                ..
            })]
        ));

        let permanent = MarketDemandError {
            code: EngineFaultCode::Permanent,
            detail: "permanent fixture failure".to_string(),
            elapsed_millis: None,
            ..retryable.clone()
        };
        assert_eq!(
            runtime::deliver_runtime_series_event(
                &endpoint,
                fenced_event(MarketRuntimeEvent::DemandError(permanent)),
            ),
            Ok(())
        );
        assert!(matches!(
            receiver.drain().0.as_slice(),
            [MarketWorkerMessage::Runtime(Fenced {
                value: MarketRuntimeEvent::DemandError(MarketDemandError {
                    code: EngineFaultCode::Permanent,
                    detail,
                    ..
                }),
                ..
            })] if detail == "permanent fixture failure"
        ));
        assert_eq!(
            demand_error(&retryable),
            "provider history failed (retryable) after 4 ms: covering history is retrying"
        );
    }

    /// A scroll-back page the provider could not serve froze the healthy live
    /// chart: the backfill error was presented as a current-series recovery,
    /// which invalidated the stream the live candle was still updating.
    #[test]
    fn older_history_demand_error_keeps_the_live_chart_and_its_pending_recovery() {
        let product = default_hyperliquid_product();
        let (mut pane, mut record) =
            worker_endpoint(1, 2, product.clone(), 41, ChartInterval::Minute1, None, 3);
        record.endpoint.pending_recovery = Some(ReplayRecoveryCommand {
            request_id: 9,
            reason: aeris_application::ResnapshotReason::QueueOverflow,
        });
        let error = MarketDemandError {
            consumer_id: MarketConsumerId(NonZeroU64::new(41).expect("consumer validates")),
            generation: MarketGenerationId(NonZeroU64::new(3).expect("generation validates")),
            scope: MarketDemandErrorScope::OlderHistory,
            code: EngineFaultCode::Retryable,
            stage: FailureStage::ProviderHistory,
            detail: "Visible history backfill is temporarily unavailable".to_string(),
            series: Some(series_key(&product, ChartInterval::Minute1).expect("series")),
            cause: "provider history did not produce usable canonical bars".to_string(),
            elapsed_millis: None,
        };
        let event = MarketRuntimeEvent::DemandError(error.clone());

        assert!(
            !complete_pending_recovery(
                &fenced_event(event),
                &product,
                &mut record.endpoint,
                1_000_000_000
            )
            .expect("older-history error is not a recovery outcome"),
            "an older-history failure cannot fail the current-series recovery"
        );
        assert!(record.endpoint.pending_recovery.is_some());
        assert_eq!(
            runtime::deliver_runtime_series_event(
                &record.endpoint,
                fenced_event(MarketRuntimeEvent::DemandError(error)),
            ),
            Ok(())
        );
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(
            matches!(
                messages.as_slice(),
                [MarketWorkerMessage::Runtime(Fenced {
                    value: MarketRuntimeEvent::DemandError(MarketDemandError {
                        scope: MarketDemandErrorScope::OlderHistory,
                        ..
                    }),
                    ..
                })]
            ),
            "the pane receives the scoped error and keeps its live chart"
        );
    }

    /// A real MNQ top-of-book fixture with accurate Rithmic integer-contract volumes.
    fn mnq_top_of_book_snapshot(product: &InstallProviderInstrument) -> MarketOrderBookSnapshot {
        MarketOrderBookSnapshot {
            consumer_id: MarketConsumerId(NonZeroU64::MIN),
            generation: MarketGenerationId(NonZeroU64::MIN),
            publication: OrderBookPublication {
                provider_id: "rithmic".to_string(),
                instrument_id: product.instrument_id.clone(),
                entitlement_id: product.entitlement_id.clone(),
                session_generation: 1,
                revision: 1,
                source_watermark: 1,
                state: OrderBookState::Ready,
                bids: vec![DepthLevel {
                    price: 7_798_670,
                    quantity: 653_408,
                    order_count: None,
                }],
                asks: vec![DepthLevel {
                    price: 7_798_671,
                    quantity: 22_517_771,
                    order_count: None,
                }],
                best_bid: Some(aeris_market_data::QuoteLevel {
                    price: 7_798_670,
                    quantity: Some(653_408),
                    order_count: Some(3),
                }),
                best_ask: Some(aeris_market_data::QuoteLevel {
                    price: 7_798_671,
                    quantity: Some(22_517_771),
                    order_count: Some(4),
                }),
                bbo_source_watermark: 2,
                traded_volumes: BTreeMap::new(),
                trade_source_watermark: 0,
                top_of_book_only: false,
            },
            display_depth: None,
        }
    }

    /// The Rithmic Order Book panel stayed empty because the engine's order-book
    /// snapshot had no arm here: `OrderBook` was declared, coalesced, and
    /// rendered, but never constructed.
    #[test]
    fn rithmic_order_book_snapshot_uses_the_consumers_selection_identity() {
        let mut product = default_product("MNQ");
        product.selection_generation = 9;
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
        let endpoint = series_endpoint(sender, 1, 1);
        let snapshot = mnq_top_of_book_snapshot(&product);
        let deliver = |snapshot| {
            runtime::deliver_runtime_order_book(
                &endpoint,
                fenced_event(MarketRuntimeEvent::OrderBookSnapshot(snapshot)),
            )
        };
        let mut future = snapshot.clone();
        future.generation = MarketGenerationId(NonZeroU64::new(2).expect("generation"));
        assert_eq!(
            deliver(future),
            Err("engine order-book generation advanced unexpectedly".into())
        );
        let mut wrong_consumer = snapshot.clone();
        wrong_consumer.consumer_id = MarketConsumerId(NonZeroU64::new(2).expect("consumer"));
        assert_eq!(
            deliver(wrong_consumer),
            Err("engine order-book consumer mismatched".into())
        );
        assert_eq!(deliver(snapshot), Ok(()));
        let (messages, _) = receiver.drain();
        let frame = messages
            .into_iter()
            .find_map(|message| match message {
                MarketWorkerMessage::Runtime(Fenced {
                    value: MarketRuntimeEvent::OrderBookSnapshot(snapshot),
                    ..
                }) => runtime_order_book_frame(&snapshot, &product, 1),
                _ => None,
            })
            .expect("Rithmic depth must reach the Order Book panel");
        assert!(
            !frame.rows.is_empty(),
            "a projected Rithmic Order Book frame must carry price rows"
        );
        assert_eq!(
            frame.selection_generation, 1,
            "the frame carries the demand generation the pane accepts, not the catalog's"
        );
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
            Some("")
        );
        assert_eq!(
            frame.rows[0]
                .ask
                .as_ref()
                .map(|level| level.traded_volume_text.as_str()),
            Some("")
        );
    }

    #[test]
    fn trade_tape_publication_is_consumer_and_generation_fenced() {
        let product = default_product("MNQ");
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        let snapshot = |generation, revision| MarketTradeTapeSnapshot {
            consumer_id: MarketConsumerId(NonZeroU64::MIN),
            generation: MarketGenerationId(NonZeroU64::new(generation).unwrap_or(NonZeroU64::MIN)),
            provider_id: product.provider.clone(),
            instrument_id: product.instrument_id.clone(),
            entitlement_id: product.entitlement_id.clone(),
            provider_generation: 3,
            revision,
            source_watermark: revision,
            rewrite_generation: 0,
            price_scale: 2,
            quantity_scale: 0,
            trades: Arc::from([]),
        };
        let endpoint = series_endpoint(sender, 1, 2);
        let deliver = |snapshot| {
            runtime::deliver_runtime_trade_tape(
                &endpoint,
                fenced_event(MarketRuntimeEvent::TradeTapeSnapshot(snapshot)),
            )
        };
        let mut future = snapshot(3, 10);
        assert_eq!(
            deliver(future.clone()),
            Err("engine trade-tape generation advanced unexpectedly".into())
        );
        future.consumer_id = MarketConsumerId(NonZeroU64::new(2).expect("consumer"));
        assert_eq!(
            deliver(future),
            Err("engine trade-tape consumer mismatched".into())
        );
        assert_eq!(deliver(snapshot(1, 8)), Ok(()));
        assert_eq!(deliver(snapshot(2, 9)), Ok(()));
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Runtime(Fenced {
                value: MarketRuntimeEvent::TradeTapeSnapshot(snapshot),
                ..
            })] if snapshot.revision == 9
        ));
    }

    #[test]
    fn study_publications_keep_the_consumer_envelope_and_selection_fence() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::new(4).expect("capacity"));
        let endpoint = series_endpoint(sender, 1, 2);
        let removed =
            |consumer, generation: Option<u64>| aeris_market_runtime::MarketStudyRemoved {
                consumer_id: MarketConsumerId(NonZeroU64::new(consumer).expect("consumer")),
                generation: generation.map(|generation| {
                    MarketGenerationId(NonZeroU64::new(generation).expect("generation"))
                }),
                study_ids: vec![],
            };
        let deliver = |removed| {
            runtime::deliver_runtime_study(
                &endpoint,
                fenced_event(MarketRuntimeEvent::StudyRemoved(removed)),
            )
        };
        assert_eq!(
            deliver(removed(2, Some(2))),
            Err("engine study consumer mismatched".into())
        );
        assert_eq!(
            deliver(removed(1, Some(3))),
            Err("engine study generation advanced unexpectedly".into())
        );
        assert_eq!(deliver(removed(1, Some(1))), Ok(()));
        assert_eq!(deliver(removed(1, None)), Ok(()));
        assert_eq!(deliver(removed(1, Some(2))), Ok(()));
        assert_eq!(
            runtime::deliver_runtime_study(
                &endpoint,
                fenced_event(MarketRuntimeEvent::StudyOutputsInvalidated(
                    aeris_market_runtime::MarketStudyOutputsInvalidated {
                        consumer_id: MarketConsumerId(NonZeroU64::MIN),
                        generation: Some(MarketGenerationId(
                            NonZeroU64::new(2).expect("generation"),
                        )),
                        study_ids: vec![],
                    },
                )),
            ),
            Ok(())
        );
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::Runtime(Fenced {
                    value: MarketRuntimeEvent::StudyRemoved(first),
                    ..
                }),
                MarketWorkerMessage::Runtime(Fenced {
                    value: MarketRuntimeEvent::StudyRemoved(second),
                    ..
                }),
                MarketWorkerMessage::Runtime(Fenced {
                    value: MarketRuntimeEvent::StudyOutputsInvalidated(_),
                    ..
                })
            ] if first.generation.is_none()
                && second.generation.is_some_and(|generation| generation.0.get() == 2)
        ));
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
            assert_eq!(series.provider_id, "hyperliquid");
            assert_eq!(
                series.period,
                BarPeriod::time(seconds).expect("fixed period validates")
            );
            assert_eq!(series.instrument_id, product.instrument_id);
            assert_eq!(series.entitlement_id, "hyperliquid-public");
        }
        // Native 3-day candles exist on Hyperliquid but not on Rithmic.
        let day3 = series_key(&product, ChartInterval::Day3).expect("HL day3 series");
        assert_eq!(
            day3.period,
            BarPeriod::time(259_200).expect("three-day fixed period validates")
        );
        // Tick candles exist on neither public path.
        assert!(series_key(&product, ChartInterval::Tick100).is_err());
        // Rithmic intervals stay Rithmic-only.
        assert!(series_key(&default_product("MNQ"), ChartInterval::Day3).is_err());
    }

    #[test]
    fn second_series_keys_exist_only_where_the_provider_serves_second_bars() {
        for interval in [ChartInterval::Second1, ChartInterval::Second30] {
            assert!(series_key(&default_hyperliquid_product(), interval).is_err());
        }
        let five = series_key(&default_product("MNQ"), ChartInterval::Second5)
            .expect("Rithmic second series");
        assert_eq!(
            five.period,
            BarPeriod::time(5).expect("five-second period validates")
        );
    }

    #[test]
    fn snapshot_instrument_parses_all_hyperliquid_identities() {
        let perp = BarSeriesKey {
            provider_id: "hyperliquid".to_string(),
            instrument_id: "hyperliquid:perp:BTC".to_string(),
            entitlement_id: "hyperliquid-public".to_string(),
            period: BarPeriod::time(60).expect("minute period validates"),
            definition_version: 1,
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
        let spot = BarSeriesKey {
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
        let builder = BarSeriesKey {
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
        let publication = runtime_snapshot(
            1,
            1,
            series,
            7,
            8,
            8,
            vec![MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 60,
                exchange_timestamp_unix_nanos: 60_000_000_000,
                open: 6_700_050_000_000,
                high: 6_700_100_000_000,
                low: 6_699_900_000_000,
                close: 6_700_075_000_000,
                volume: 98_639_000,
            }],
            1,
            false,
        );
        let snapshot = replay_runtime_snapshot(&publication, None, 1_000_000_000)
            .expect("HL snapshot converts");
        assert_eq!(snapshot.instrument().precision.price_scale(), 8);
        assert_eq!(snapshot.instrument().precision.quantity_scale(), 8);
        assert_eq!(snapshot.instrument().symbol, "BTC");
    }

    #[test]
    fn hyperliquid_publication_uses_hyperliquid_worker_identity() {
        let product = default_hyperliquid_product();
        let snapshot = runtime_snapshot(
            1,
            1,
            series_key(&product, ChartInterval::Minute1).expect("HL series"),
            7,
            8,
            8,
            vec![MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 60,
                exchange_timestamp_unix_nanos: 60_000_000_000,
                open: 6_700_050_000_000,
                high: 6_700_100_000_000,
                low: 6_699_900_000_000,
                close: 6_700_075_000_000,
                volume: 98_639_000,
            }],
            1,
            false,
        );
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let endpoint = series_endpoint(sender, 1, 8);
        // This fixture previously stamped a different selection onto converted
        // replay data. The canonical event now carries that selection itself.
        let snapshot = MarketSeriesSnapshot {
            generation: MarketGenerationId(NonZeroU64::new(8).expect("generation")),
            ..snapshot
        };
        runtime::deliver_runtime_series_publication(
            &endpoint,
            fenced_event(MarketRuntimeEvent::SeriesSnapshot(snapshot)),
            1_000_000_000,
        )
        .expect("publication queues");

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::RuntimeProjection(aeris_desktop::market_worker::RuntimeProjection { event: Fenced {
                    value: MarketRuntimeEvent::SeriesSnapshot(snapshot),
                    ..
                }, .. })] if snapshot.snapshot.series.provider_id == "hyperliquid"
                && snapshot.generation.0.get() == 8
        ));
        let MarketWorkerMessage::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection { event, .. },
        ) = &messages[0]
        else {
            panic!("canonical snapshot");
        };
        let MarketRuntimeEvent::SeriesSnapshot(snapshot) = &event.value else {
            panic!("canonical snapshot");
        };
        let (subscription_id, worker_label) =
            worker_identity(&snapshot.snapshot.series.provider_id);
        assert_eq!(subscription_id, HYPERLIQUID_SUBSCRIPTION_ID);
        assert_eq!(worker_label, HYPERLIQUID_WORKER_LABEL);
    }

    #[test]
    fn provider_state_keeps_coordinator_epoch_until_pane_projection() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let endpoint = series_endpoint(sender, 41, 8);
        let event = fenced_event(MarketRuntimeEvent::ProviderState(ProviderState {
            provider: "hyperliquid".into(),
            state: ProviderConnectionState::Online,
            generation: 3,
            detail: None,
            transport_rtt_nanos: Some(12_500_000),
        }));
        let epoch = event.epoch;
        runtime::deliver_runtime_provider_state(&endpoint, "hyperliquid", event)
            .expect("matching state");
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::RuntimeProjection(aeris_desktop::market_worker::RuntimeProjection { event: Fenced {
                    epoch: actual,
                    value: MarketRuntimeEvent::ProviderState(ProviderState {
                        transport_rtt_nanos: Some(12_500_000),
                        ..
                    }),
                }, selection_generation: Some(8), .. })] if *actual == epoch
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
                    state: ProviderConnectionState::Online,
                    generation: 1,
                    detail: None,
                    transport_rtt_nanos: Some(12_000_000),
                },
                "rithmic",
                7,
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
                    state: ProviderConnectionState::Online,
                    generation: 1,
                    detail: None,
                    transport_rtt_nanos: Some(12_000_000),
                },
                "hyperliquid",
                7,
                true,
                &sender,
            )
            .is_err()
        );
    }
}
