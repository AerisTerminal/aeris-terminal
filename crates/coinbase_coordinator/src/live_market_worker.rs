//! Explicit direct-device Coinbase desktop composition.

mod composition;
#[cfg(test)]
mod conformance;
mod diagnostics;
mod history;
mod lifecycle;
mod provenance;
mod publication;

use crate::market_worker::{
    ChartState, ChartViewportUpdate, CoinbaseSelectionRequest, MarketDataWorker,
    MarketWorkerMessage, MarketWorkerSender, MarketWorkerStartup, UiDiagnosticsReceiver,
    market_worker_channel, ui_diagnostics_channel,
};
use axiusflow_application::ReplayRecoveryCommand;
use axiusflow_application::{
    MarketBarClientModel, ProvenancedMarketBar, ReplayStreamUpdate, StreamDelta,
};
use axiusflow_coinbase_market_adapter::{
    CoinbaseAggregatedBar, CoinbaseDesktopEventError, CoinbaseDesktopMarketEvent,
    CoinbaseHttpsHistoryTransport, CoinbaseInterval, CoinbaseLevel2Book, CoinbaseLevel2Outcome,
    CoinbaseProductCatalog, CoinbaseProviderEvents, CoinbaseSpotProduct, aggregate_coinbase_bars,
    coinbase_depth_limit,
};
use axiusflow_desktop_provider_runtime::{
    DesktopMarketWorkerError, DesktopProviderError, DesktopProviderState, SessionGeneration,
};
use axiusflow_desktop_storage::{
    DataKind, HistoryScope, HistorySeriesIdentity, SegmentEncryptionKey,
};
use axiusflow_instruments::{InstrumentPrecision, InstrumentRevision};
use axiusflow_market_data::{BarDefinition, ChartInterval, MarketEvent};
use axiusflow_platform_runtime::{NetworkEvent, PowerEvent};
use axiusflow_provider_history::{CoverageClass, HistoryRange};
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

use composition::{
    CoinbaseDesktopWorker, OpenedWorker, ProductProfile, bar_definition_for_interval, client_model,
    instrument, loading_startup, nonzero, open_worker, product_profile, product_profile_from_spot,
    unix_nanos, worker_label,
};
use diagnostics::{diagnostics_wait_duration, flush_diagnostics};
use history::{
    ActiveHistoryTail, DirectHistorySource, FetchPhase, HistorySource, InitialHistoryContext,
    PreparedHistoryBatch, StreamingSeriesContext, history_request_range, needs_recent_phase,
    persist_live_tail, prepare_initial_history,
};
use lifecycle::{
    DrainSignal, EnvironmentalEvent, InboxDrainContext, ReconnectBackoff, WorkerInboxEvent,
    apply_initial_network, cancel_inflight_history, drain_worker_inbox, environment_events,
    forward_commands, wait_for_inbox,
};
use provenance::{cached_history_provenance, history_provenance, live_provenance};
use publication::{publish_cached_update, publish_ready_recovery, publish_update};

const HISTORY_BARS: usize = 350;
const VIEWPORT_PREFETCH_WINDOWS: i64 = 1;
const MODEL_ITEM_CAPACITY: usize = HISTORY_BARS;
const PROVIDER_EVENT_CAPACITY: usize = 16_384;
const PROVIDER_EVENT_BATCH: usize = 1_024;
const MESSAGE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 1;
const PARTITION_ID: u32 = 7;
const SCHEMA_VERSION: u32 = 1;
const INBOX_CAPACITY: usize = 64;
const INBOX_BATCH: usize = 1_024;
const UI_DIAGNOSTICS_CAPACITY: usize = 256;
const HISTORY_REQUEST_DEADLINE: Duration = Duration::from_secs(30);
const SUBSCRIPTION_ID: &str = "desktop_coinbase_one_minute_bars";
const VAULT_SERVICE: &str = "axiusflow-desktop-market-history";
const CATALOG_KEY_ID: &str = "history-catalog-key-v1";
const SEGMENT_KEY_ID: &str = "coinbase-public-bars-key-v1";

struct LiveLoopState {
    history: Option<InflightHistory>,
    streaming_generation: Option<SessionGeneration>,
    pending_viewport: Option<ChartViewportUpdate>,
    last_requested_range: Option<HistoryRange>,
    retained: VecDeque<ProvenancedMarketBar>,
    reconnect_backoff: ReconnectBackoff,
    recovery_announced: bool,
    pending_recovery: VecDeque<ReplayRecoveryCommand>,
    active_tail: ActiveHistoryTail,
}

pub(super) struct InflightHistory {
    pub(super) cancel: Arc<AtomicBool>,
}

enum HistoryCommand {
    Now(SyncSender<Result<i64, String>>),
    Fetch {
        generation: SessionGeneration,
        profile: ProductProfile,
        requested: HistoryRange,
        repairs: Vec<HistoryRange>,
        cancel: Arc<AtomicBool>,
        deadline: Instant,
    },
}

struct WorkerThreadInput {
    profile: ProductProfile,
    history_root: PathBuf,
    ui_thread: ThreadId,
    message_tx: MarketWorkerSender,
    inbox_tx: SyncSender<WorkerInboxEvent>,
    inbox_rx: Receiver<WorkerInboxEvent>,
    provider_wake_pending: Arc<AtomicBool>,
    ui_diagnostics_rx: UiDiagnosticsReceiver,
    detailed_diagnostics: bool,
    include_level2: bool,
    selection_sequence: Arc<AtomicU64>,
}

struct RunningWorker<V: axiusflow_platform_runtime::CredentialVault> {
    profile: ProductProfile,
    worker: CoinbaseDesktopWorker<V>,
    events: CoinbaseProviderEvents,
    segment_key: SegmentEncryptionKey,
    instrument: InstrumentRevision,
    bar_definition: BarDefinition,
    active_selection_generation: u64,
    worker_label: String,
    model: MarketBarClientModel,
    state: LiveLoopState,
    level2: Option<CoinbaseLevel2Book>,
    dom: axiusflow_terminal_ui::ReadOnlyDom,
}

impl<V: axiusflow_platform_runtime::CredentialVault> RunningWorker<V> {
    fn reconcile_recovery(&mut self, message_tx: &MarketWorkerSender) -> Result<(), String> {
        request_recovery_if_required(&mut self.worker, &self.events, &mut self.state, message_tx)
    }
}

struct CoinbaseCallbackContext<'a> {
    streaming_generation: Option<SessionGeneration>,
    retained: &'a mut VecDeque<ProvenancedMarketBar>,
    model: &'a mut MarketBarClientModel,
    worker_label: &'a str,
    profile: &'a ProductProfile,
    instrument: &'a InstrumentRevision,
    bar_definition: &'a BarDefinition,
    level2: &'a mut Option<CoinbaseLevel2Book>,
    dom: &'a mut axiusflow_terminal_ui::ReadOnlyDom,
    message_tx: &'a MarketWorkerSender,
    active_tail: &'a mut ActiveHistoryTail,
    segment_key: &'a SegmentEncryptionKey,
}

/// Starts the live Coinbase coordinator and its bounded worker set.
///
/// # Errors
/// Returns an error if product configuration, storage, or worker startup fails.
pub fn start(
    product_id: String,
    history_root: PathBuf,
    ui_thread: ThreadId,
    detailed_diagnostics: bool,
    fetch_catalog: bool,
) -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    start_with_depth(
        product_id,
        history_root,
        ui_thread,
        detailed_diagnostics,
        fetch_catalog,
        true,
    )
}

/// Starts Coinbase with explicit control over the high-rate Level 2 channel.
///
/// # Errors
/// Returns an error if product configuration, storage, or worker startup fails.
pub fn start_with_depth(
    product_id: String,
    history_root: PathBuf,
    ui_thread: ThreadId,
    detailed_diagnostics: bool,
    fetch_catalog: bool,
    include_level2: bool,
) -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    let profile = product_profile(product_id)?;
    start_with_profile(
        profile,
        history_root,
        ui_thread,
        detailed_diagnostics,
        fetch_catalog,
        include_level2,
    )
}

/// Starts Coinbase from a validated catalog product with explicit Level 2 control.
///
/// # Errors
/// Returns an error if product metadata, storage, or worker startup fails.
pub fn start_product(
    product: CoinbaseSpotProduct,
    interval: ChartInterval,
    history_root: PathBuf,
    ui_thread: ThreadId,
    detailed_diagnostics: bool,
    fetch_catalog: bool,
    include_level2: bool,
) -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    start_with_profile(
        product_profile_from_spot(product, interval),
        history_root,
        ui_thread,
        detailed_diagnostics,
        fetch_catalog,
        include_level2,
    )
}

fn start_with_profile(
    profile: ProductProfile,
    history_root: PathBuf,
    ui_thread: ThreadId,
    detailed_diagnostics: bool,
    fetch_catalog: bool,
    include_level2: bool,
) -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    let startup = loading_startup(&profile);
    let (message_tx, message_rx) = market_worker_channel(nonzero(MESSAGE_CAPACITY));
    if fetch_catalog {
        let catalog_message_tx = message_tx.clone();
        thread::Builder::new()
            .name("axiusflow-coinbase-product-catalog".to_string())
            .spawn(move || {
                let mut catalog =
                    CoinbaseProductCatalog::with_transport(CoinbaseHttpsHistoryTransport::new());
                let result = catalog
                    .fetch_active_spot_products()
                    .map_err(|error| error.to_string());
                let _ = catalog_message_tx.send(MarketWorkerMessage::CoinbaseCatalog(result));
            })
            .map_err(|error| error.to_string())?;
    }
    let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (inbox_tx, inbox_rx) = mpsc::sync_channel(INBOX_CAPACITY);
    let diagnostics_wake_tx = inbox_tx.clone();
    let diagnostics_wake = Arc::new(move || {
        let _ = diagnostics_wake_tx.try_send(WorkerInboxEvent::UiDiagnosticsReady);
    });
    let (ui_diagnostics_tx, ui_diagnostics_rx) =
        ui_diagnostics_channel(nonzero(UI_DIAGNOSTICS_CAPACITY), diagnostics_wake);
    let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
    let provider_wake_pending = Arc::new(AtomicBool::new(false));
    let selection_sequence = Arc::new(AtomicU64::new(0));
    let command_inbox_tx = inbox_tx.clone();
    thread::Builder::new()
        .name("axiusflow-coinbase-command-inbox".to_string())
        .spawn(move || forward_commands(&command_rx, &command_inbox_tx))
        .map_err(|error| error.to_string())?;
    let worker_selection_sequence = Arc::clone(&selection_sequence);
    thread::Builder::new()
        .name("axiusflow-coinbase-market-worker".to_string())
        .spawn(move || {
            let error_tx = message_tx.clone();
            if let Err(error) = run_worker(WorkerThreadInput {
                profile,
                history_root,
                ui_thread,
                message_tx,
                inbox_tx,
                inbox_rx,
                provider_wake_pending,
                ui_diagnostics_rx,
                detailed_diagnostics,
                include_level2,
                selection_sequence: worker_selection_sequence,
            }) {
                let _ = error_tx.send(MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message: error,
                });
            }
            let _ = shutdown_tx.send(());
        })
        .map_err(|error| error.to_string())?;
    Ok((
        startup,
        MarketDataWorker::from_channels(
            command_tx,
            message_rx,
            shutdown_rx,
            Some(ui_diagnostics_tx),
            Some(selection_sequence),
        ),
    ))
}
fn ensure_history_parent(history_root: &Path) -> Result<(), String> {
    let Some(parent) = history_root.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    fs::create_dir_all(parent)
        .map_err(|error| format!("Coinbase history parent could not be created: {error}"))
}

enum SessionEnd {
    Shutdown,
    Exit,
    Reselect(Box<CoinbaseSelectionRequest>),
}

/// One coordinator owns the history thread, environment monitors, and the
/// encrypted store session for the application's lifetime. Selection changes
/// rebuild the provider session in place on this thread, so the store lock is
/// always released before it is reacquired.
fn run_worker(input: WorkerThreadInput) -> Result<(), String> {
    let WorkerThreadInput {
        profile,
        history_root,
        ui_thread,
        message_tx,
        inbox_tx,
        inbox_rx,
        provider_wake_pending,
        ui_diagnostics_rx,
        detailed_diagnostics,
        include_level2,
        selection_sequence,
    } = input;
    ensure_history_parent(&history_root)?;
    let (history_command_tx, history_command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let history_inbox_tx = inbox_tx.clone();
    let history_handle = thread::Builder::new()
        .name("axiusflow-coinbase-history".to_string())
        .spawn(move || {
            let mut history_source = DirectHistorySource;
            history_command_loop(&mut history_source, &history_command_rx, &history_inbox_tx);
        })
        .map_err(|error| error.to_string())?;
    let (initial_network, monitors_active) = environment_events(inbox_tx.clone());
    let mut profile = profile;
    let mut active_selection_generation = 0;
    let result = loop {
        let OpenedWorker {
            worker,
            events,
            segment_key,
        } = match open_worker(
            &profile,
            &history_root,
            ui_thread,
            &inbox_tx,
            &provider_wake_pending,
            detailed_diagnostics,
            include_level2,
        ) {
            Ok(opened) => opened,
            Err(error) => break Err(error),
        };
        let history_source = DirectHistorySource;
        let running = match prepare_running_worker(
            profile.clone(),
            OpenedWorker {
                worker,
                events,
                segment_key,
            },
            initial_network,
            monitors_active,
            &history_source,
            &message_tx,
            active_selection_generation,
        ) {
            Ok(running) => running,
            Err(error) => break Err(error),
        };
        match run_session(
            running,
            &history_command_tx,
            &message_tx,
            &inbox_rx,
            &provider_wake_pending,
            &ui_diagnostics_rx,
            &selection_sequence,
        ) {
            Ok(SessionEnd::Reselect(request)) => {
                let sequence = request.sequence;
                profile = product_profile_from_spot(request.product, request.interval);
                active_selection_generation = sequence;
                let _ = message_tx.send(MarketWorkerMessage::CoinbaseSwitchMarker { sequence });
                let _ = message_tx.send(MarketWorkerMessage::State {
                    state: ChartState::Loading,
                    message: format!("Loading {} history", profile.interval.label()),
                });
            }
            other => break other.map(|_| ()),
        }
    };
    drop(history_command_tx);
    let _ = history_handle.join();
    result
}

fn prepare_running_worker<V: axiusflow_platform_runtime::CredentialVault, H: HistorySource>(
    profile: ProductProfile,
    opened: OpenedWorker<V>,
    initial_network: Option<NetworkEvent>,
    monitors_active: bool,
    history_source: &H,
    message_tx: &MarketWorkerSender,
    active_selection_generation: u64,
) -> Result<RunningWorker<V>, String> {
    let OpenedWorker {
        mut worker,
        events,
        segment_key,
    } = opened;
    apply_initial_network(&mut worker, initial_network)?;
    let instrument = instrument(&profile)?;
    let bar_definition = bar_definition_for_interval(profile.interval);
    let worker_label = worker_label(monitors_active);
    let mut model = client_model();
    let retained = prepare_initial_history(
        &mut worker,
        history_source,
        &InitialHistoryContext {
            profile: &profile,
            segment_key: &segment_key,
            instrument: &instrument,
            bar_definition: &bar_definition,
            worker_label: &worker_label,
            initial_network,
        },
        &mut model,
        message_tx,
    )?;
    Ok(RunningWorker {
        instrument,
        bar_definition,
        active_selection_generation,
        worker_label,
        model,
        state: LiveLoopState {
            history: None,
            streaming_generation: None,
            pending_viewport: None,
            last_requested_range: None,
            retained,
            reconnect_backoff: ReconnectBackoff::new(),
            recovery_announced: false,
            pending_recovery: VecDeque::with_capacity(COMMAND_CAPACITY),
            active_tail: ActiveHistoryTail::default(),
        },
        level2: None,
        dom: axiusflow_terminal_ui::ReadOnlyDom::new(coinbase_depth_limit()),
        profile,
        worker,
        events,
        segment_key,
    })
}

fn run_session<V: axiusflow_platform_runtime::CredentialVault>(
    mut running: RunningWorker<V>,
    history_command_tx: &SyncSender<HistoryCommand>,
    message_tx: &MarketWorkerSender,
    inbox_rx: &Receiver<WorkerInboxEvent>,
    provider_wake_pending: &AtomicBool,
    ui_diagnostics_rx: &UiDiagnosticsReceiver,
    selection_sequence: &AtomicU64,
) -> Result<SessionEnd, String> {
    let end = run_market_event_loop(
        &mut running,
        history_command_tx,
        message_tx,
        inbox_rx,
        &SessionWiring {
            provider_wake_pending,
            ui_diagnostics_rx,
            selection_sequence,
        },
    )?;
    cancel_inflight_history(&mut running.state.history);
    if !matches!(end, SessionEnd::Exit) {
        running.worker.stop().map_err(|error| error.to_string())?;
    }
    Ok(end)
}

fn history_command_loop<H: HistorySource>(
    source: &mut H,
    commands: &Receiver<HistoryCommand>,
    inbox_tx: &SyncSender<WorkerInboxEvent>,
) {
    'commands: while let Ok(command) = commands.recv() {
        let HistoryCommand::Fetch {
            generation,
            profile,
            requested,
            repairs,
            cancel,
            deadline,
        } = command
        else {
            let HistoryCommand::Now(reply) = command else {
                unreachable!();
            };
            let _ = reply.send(source.now_unix_nanos());
            continue;
        };
        if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
            continue;
        }
        let Ok(now) = source.now_unix_nanos() else {
            continue;
        };
        let mut remaining = repairs;
        if needs_recent_phase(profile.interval) {
            let recent = match history_request_range(&profile, now, FetchPhase::Recent) {
                Ok(range) => range,
                Err(error) => {
                    let _ = inbox_tx.send(WorkerInboxEvent::HistoryCompleted {
                        generation,
                        phase: FetchPhase::Recent,
                        result: Err(error),
                    });
                    continue;
                }
            };
            let visible_repairs = remaining
                .iter()
                .filter_map(|range| intersect_history_range(*range, recent))
                .collect::<Vec<_>>();
            remaining = remaining
                .into_iter()
                .flat_map(|range| subtract_history_range(range, recent))
                .collect();
            if !visible_repairs.is_empty() {
                let result = fetch_history_repairs(
                    source,
                    &profile,
                    now,
                    &visible_repairs,
                    &cancel,
                    deadline,
                )
                .map(|repairs| {
                    Box::new(PreparedHistoryBatch {
                        requested: recent,
                        repairs,
                        received_unix_nanos: now,
                    })
                });
                let failed = result.is_err();
                if inbox_tx
                    .send(WorkerInboxEvent::HistoryCompleted {
                        generation,
                        phase: FetchPhase::Recent,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
                if failed {
                    continue 'commands;
                }
            }
        }
        if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
            continue;
        }
        let result = fetch_history_repairs(source, &profile, now, &remaining, &cancel, deadline)
            .map(|repairs| {
                Box::new(PreparedHistoryBatch {
                    requested,
                    repairs,
                    received_unix_nanos: now,
                })
            });
        if !cancel.load(Ordering::Acquire)
            && inbox_tx
                .send(WorkerInboxEvent::HistoryCompleted {
                    generation,
                    phase: FetchPhase::Full,
                    result,
                })
                .is_err()
        {
            return;
        }
    }
}

fn intersect_history_range(left: HistoryRange, right: HistoryRange) -> Option<HistoryRange> {
    let start_unix_nanos = left.start_unix_nanos.max(right.start_unix_nanos);
    let end_unix_nanos = left.end_unix_nanos.min(right.end_unix_nanos);
    (start_unix_nanos < end_unix_nanos).then_some(HistoryRange {
        start_unix_nanos,
        end_unix_nanos,
    })
}

fn subtract_history_range(range: HistoryRange, removed: HistoryRange) -> Vec<HistoryRange> {
    let Some(overlap) = intersect_history_range(range, removed) else {
        return vec![range];
    };
    let mut retained = Vec::with_capacity(2);
    if range.start_unix_nanos < overlap.start_unix_nanos {
        retained.push(HistoryRange {
            start_unix_nanos: range.start_unix_nanos,
            end_unix_nanos: overlap.start_unix_nanos,
        });
    }
    if overlap.end_unix_nanos < range.end_unix_nanos {
        retained.push(HistoryRange {
            start_unix_nanos: overlap.end_unix_nanos,
            end_unix_nanos: range.end_unix_nanos,
        });
    }
    retained
}

fn fetch_history_repairs<H: HistorySource>(
    source: &mut H,
    profile: &ProductProfile,
    now: i64,
    repairs: &[HistoryRange],
    cancel: &Arc<AtomicBool>,
    deadline: Instant,
) -> Result<Vec<history::PreparedHistory>, String> {
    let mut fetched = Vec::with_capacity(repairs.len());
    for &range in repairs {
        if cancel.load(Ordering::Acquire) {
            return Err("Coinbase history repair was cancelled".to_string());
        }
        if Instant::now() >= deadline {
            cancel.store(true, Ordering::Release);
            return Err("Coinbase history repair deadline exceeded".to_string());
        }
        let repair = source.fetch(profile, now, Arc::clone(cancel), range)?;
        if cancel.load(Ordering::Acquire) {
            return Err("Coinbase history repair was cancelled before decode".to_string());
        }
        if Instant::now() >= deadline {
            cancel.store(true, Ordering::Release);
            return Err("Coinbase history repair deadline exceeded".to_string());
        }
        fetched.push(repair);
    }
    Ok(fetched)
}

enum LoopAction {
    Continue,
    Shutdown,
    Exit,
    Reselect(Box<CoinbaseSelectionRequest>),
}

struct SessionWiring<'a> {
    provider_wake_pending: &'a AtomicBool,
    ui_diagnostics_rx: &'a UiDiagnosticsReceiver,
    selection_sequence: &'a AtomicU64,
}

fn run_market_event_loop<V: axiusflow_platform_runtime::CredentialVault>(
    running: &mut RunningWorker<V>,
    history_command_tx: &SyncSender<HistoryCommand>,
    message_tx: &MarketWorkerSender,
    inbox_rx: &Receiver<WorkerInboxEvent>,
    wiring: &SessionWiring<'_>,
) -> Result<SessionEnd, String> {
    let mut ready_event = None;
    loop {
        match market_loop_iteration(
            running,
            history_command_tx,
            message_tx,
            inbox_rx,
            &mut ready_event,
            wiring,
        )? {
            LoopAction::Continue => {}
            LoopAction::Shutdown => return Ok(SessionEnd::Shutdown),
            LoopAction::Exit => return Ok(SessionEnd::Exit),
            LoopAction::Reselect(request) => return Ok(SessionEnd::Reselect(request)),
        }
    }
}

fn market_loop_iteration<V: axiusflow_platform_runtime::CredentialVault>(
    running: &mut RunningWorker<V>,
    history_command_tx: &SyncSender<HistoryCommand>,
    message_tx: &MarketWorkerSender,
    inbox_rx: &Receiver<WorkerInboxEvent>,
    ready_event: &mut Option<WorkerInboxEvent>,
    wiring: &SessionWiring<'_>,
) -> Result<LoopAction, String> {
    match drain_worker_inbox(
        inbox_rx,
        ready_event,
        &mut InboxDrainContext {
            worker: &mut running.worker,
            events: &running.events,
            state: &mut running.state,
            message_tx,
            provider_wake_pending: wiring.provider_wake_pending,
            selection_sequence: wiring.selection_sequence,
            active_selection_generation: running.active_selection_generation,
            series: StreamingSeriesContext {
                profile: &running.profile,
                segment_key: &running.segment_key,
                instrument: &running.instrument,
                bar_definition: &running.bar_definition,
                worker_label: &running.worker_label,
            },
            model: &mut running.model,
        },
    )? {
        DrainSignal::None => {}
        DrainSignal::Shutdown => return Ok(LoopAction::Shutdown),
        DrainSignal::Reselect(request) => return Ok(LoopAction::Reselect(request)),
    }
    flush_diagnostics(&mut running.worker, wiring.ui_diagnostics_rx, message_tx)?;

    running.reconcile_recovery(message_tx)?;

    maybe_start_history(running, history_command_tx, message_tx)?;

    drain_coinbase_callbacks(
        &mut running.worker,
        &running.events,
        CoinbaseCallbackContext {
            streaming_generation: running.state.streaming_generation,
            retained: &mut running.state.retained,
            model: &mut running.model,
            worker_label: &running.worker_label,
            profile: &running.profile,
            instrument: &running.instrument,
            bar_definition: &running.bar_definition,
            level2: &mut running.level2,
            dom: &mut running.dom,
            message_tx,
            active_tail: &mut running.state.active_tail,
            segment_key: &running.segment_key,
        },
    )?;

    running.reconcile_recovery(message_tx)?;

    if !publish_ready_recovery(
        running.state.streaming_generation,
        &mut running.state.pending_recovery,
        message_tx,
        (&running.instrument, &running.bar_definition),
        &running.state.retained,
        &mut running.model,
        &running.worker_label,
    ) {
        return Ok(LoopAction::Exit);
    }
    discard_provider_events(&mut running.worker)?;
    flush_diagnostics(&mut running.worker, wiring.ui_diagnostics_rx, message_tx)?;
    if running.events.has_ready() {
        return Ok(LoopAction::Continue);
    }
    let recovery_required = matches!(
        running
            .worker
            .provider_state()
            .map_err(|error| error.to_string())?,
        DesktopProviderState::RecoveryRequired { .. }
    );
    *ready_event = wait_for_inbox(
        inbox_rx,
        Some(diagnostics_wait_duration(
            running
                .state
                .reconnect_backoff
                .wait_duration(recovery_required, Instant::now()),
        )),
    )?;
    Ok(LoopAction::Continue)
}

fn maybe_start_history<V: axiusflow_platform_runtime::CredentialVault>(
    running: &mut RunningWorker<V>,
    history_command_tx: &SyncSender<HistoryCommand>,
    message_tx: &MarketWorkerSender,
) -> Result<(), String> {
    if running.state.history.is_some() {
        return Ok(());
    }
    let DesktopProviderState::Streaming { generation } = running
        .worker
        .provider_state()
        .map_err(|error| error.to_string())?
    else {
        return Ok(());
    };
    let viewport = running.state.pending_viewport.take();
    if viewport.is_none() && running.state.streaming_generation.is_some() {
        return Ok(());
    }
    let (clock_tx, clock_rx) = mpsc::sync_channel(1);
    history_command_tx
        .send(HistoryCommand::Now(clock_tx))
        .map_err(|_| "Coinbase history worker stopped".to_string())?;
    let now = clock_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .map_err(|_| "Coinbase history clock did not respond".to_string())??;
    let requested = if let Some(viewport) = viewport {
        viewport_history_range(&running.profile, viewport)?
    } else {
        history_request_range(&running.profile, now, FetchPhase::Full)?
    };
    if running.state.last_requested_range == Some(requested) {
        return Ok(());
    }
    let coverage = history_coverage_plan(running, requested, now)?;
    if running.state.streaming_generation.is_none()
        && activate_complete_cached_history(
            running,
            generation,
            message_tx,
            coverage.classification(),
            now,
        )?
    {
        running.state.last_requested_range = Some(requested);
        return Ok(());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    running.state.history = Some(InflightHistory {
        cancel: Arc::clone(&cancel),
    });
    running.state.last_requested_range = Some(requested);
    history_command_tx
        .send(HistoryCommand::Fetch {
            generation,
            profile: running.profile.clone(),
            requested,
            repairs: coverage.repair_ranges().to_vec(),
            cancel,
            deadline: Instant::now() + HISTORY_REQUEST_DEADLINE,
        })
        .map_err(|_| "Coinbase history worker stopped".to_string())
}

fn viewport_history_range(
    profile: &ProductProfile,
    viewport: ChartViewportUpdate,
) -> Result<HistoryRange, String> {
    let interval_nanos = chart_interval_nanos(profile.interval)?;
    let visible_span = viewport
        .end_unix_nanos
        .checked_sub(viewport.start_unix_nanos)
        .ok_or_else(|| "Coinbase viewport range underflow".to_string())?;
    let prefetch = visible_span
        .checked_mul(VIEWPORT_PREFETCH_WINDOWS)
        .ok_or_else(|| "Coinbase viewport prefetch overflow".to_string())?;
    let start = viewport
        .start_unix_nanos
        .saturating_sub(prefetch)
        .div_euclid(interval_nanos)
        * interval_nanos;
    let end = viewport
        .end_unix_nanos
        .saturating_add(interval_nanos - 1)
        .div_euclid(interval_nanos)
        * interval_nanos;
    Ok(HistoryRange {
        start_unix_nanos: start,
        end_unix_nanos: end,
    })
}

fn chart_interval_nanos(interval: ChartInterval) -> Result<i64, String> {
    let seconds = match interval.aggregation() {
        axiusflow_market_data::ChartAggregation::FixedSeconds(seconds) => i64::from(seconds.get()),
        axiusflow_market_data::ChartAggregation::CalendarMonth => 30 * 24 * 60 * 60,
        axiusflow_market_data::ChartAggregation::Trades(_) => {
            return Err(
                "Coinbase viewport history does not support trade-count intervals".to_string(),
            );
        }
    };
    seconds
        .checked_mul(1_000_000_000)
        .ok_or_else(|| "Coinbase viewport interval overflow".to_string())
}

fn activate_complete_cached_history<V: axiusflow_platform_runtime::CredentialVault>(
    running: &mut RunningWorker<V>,
    generation: SessionGeneration,
    message_tx: &MarketWorkerSender,
    coverage: CoverageClass,
    now: i64,
) -> Result<bool, String> {
    if running.state.retained.is_empty() {
        return Ok(false);
    }
    if !cache_can_resume_live(coverage, true) {
        return Ok(false);
    }
    let scope = HistoryScope {
        provider_id: "coinbase".to_string(),
        account_id: axiusflow_coinbase_market_adapter::COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
        entitlement_revision: axiusflow_coinbase_market_adapter::ENTITLEMENT_CLASS.to_string(),
    };
    let series = HistorySeriesIdentity {
        scope: &scope,
        instrument_id: &running.profile.instrument_id,
        data_kind: DataKind::Bars,
        resolution: running.profile.interval.label(),
        source_revision: 1,
        schema_revision: 1,
        calendar_revision: 1,
        adjustment_revision: 1,
        correction_revision: 1,
    };
    if running.profile.interval == ChartInterval::Minute1 {
        let latest = running
            .worker
            .latest_history_identity(series, now / 1_000_000_000)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "complete Coinbase coverage has no retained tail".to_string())?;
        running.worker.seed_coinbase_bar_history(
            generation,
            &running.profile.product_id,
            &latest,
            &running.segment_key,
            now / 1_000_000_000,
        )?;
    }
    running.state.streaming_generation = Some(generation);
    running.state.reconnect_backoff.reset();
    running.state.recovery_announced = false;
    let _ = message_tx.send(MarketWorkerMessage::State {
        state: ChartState::Ready,
        message: "Authenticated local Coinbase coverage is complete; live tail resumed".to_string(),
    });
    Ok(true)
}

fn history_coverage_plan<V: axiusflow_platform_runtime::CredentialVault>(
    running: &RunningWorker<V>,
    requested: HistoryRange,
    now: i64,
) -> Result<axiusflow_provider_history::CoveragePlan, String> {
    let scope = HistoryScope {
        provider_id: "coinbase".to_string(),
        account_id: axiusflow_coinbase_market_adapter::COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
        entitlement_revision: axiusflow_coinbase_market_adapter::ENTITLEMENT_CLASS.to_string(),
    };
    running
        .worker
        .history_coverage_snapshot(
            HistorySeriesIdentity {
                scope: &scope,
                instrument_id: &running.profile.instrument_id,
                data_kind: DataKind::Bars,
                resolution: running.profile.interval.label(),
                source_revision: 1,
                schema_revision: 1,
                calendar_revision: 1,
                adjustment_revision: 1,
                correction_revision: 1,
            },
            now / 1_000_000_000,
        )
        .map_err(|error| error.to_string())?
        .plan(requested)
        .map_err(|error| error.to_string())
}

const fn cache_can_resume_live(class: CoverageClass, has_predecessor: bool) -> bool {
    has_predecessor && matches!(class, CoverageClass::Complete)
}

fn discard_provider_events<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
) -> Result<(), String> {
    while worker
        .try_recv_provider_event()
        .map_err(|error| error.to_string())?
        .is_some()
    {}
    Ok(())
}

fn fence_failed_history<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    generation: SessionGeneration,
    retained: &mut VecDeque<ProvenancedMarketBar>,
    recovery_announced: &mut bool,
    message_tx: &MarketWorkerSender,
) -> Result<(), String> {
    worker.reset_aggregation();
    worker
        .session_invalid(generation)
        .map_err(|failure| failure.to_string())?;
    retained.clear();
    *recovery_announced = true;
    let _ = message_tx.send(MarketWorkerMessage::State {
        state: ChartState::Recovering,
        message: "Coinbase history recovery required; awaiting a fresh covering snapshot"
            .to_string(),
    });
    Ok(())
}

fn apply_environment_event<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    events: &CoinbaseProviderEvents,
    event: EnvironmentalEvent,
    history: &mut Option<InflightHistory>,
    streaming_generation: &mut Option<SessionGeneration>,
    retained: &mut VecDeque<ProvenancedMarketBar>,
    message_tx: &MarketWorkerSender,
) -> Result<bool, String> {
    let _ = match event {
        EnvironmentalEvent::Network(NetworkEvent::Unavailable) => {
            let next = worker.handle_network_event(NetworkEvent::Unavailable);
            discard_coinbase_callbacks(events);
            next
        }
        EnvironmentalEvent::Power(PowerEvent::Suspending) => {
            let next = worker.handle_power_event(PowerEvent::Suspending);
            discard_coinbase_callbacks(events);
            next
        }
        EnvironmentalEvent::Network(NetworkEvent::Available) => {
            discard_coinbase_callbacks(events);
            worker.handle_network_event(NetworkEvent::Available)
        }
        EnvironmentalEvent::Power(PowerEvent::Resumed) => {
            discard_coinbase_callbacks(events);
            worker.handle_power_event(PowerEvent::Resumed)
        }
    }
    .map_err(|error| error.to_string())?;
    worker.reset_aggregation();
    cancel_inflight_history(history);
    *streaming_generation = None;
    retained.clear();
    let _ = message_tx.send(MarketWorkerMessage::State {
        state: ChartState::Stale,
        message: "direct provider lifecycle changed; a fresh snapshot is required".to_string(),
    });
    Ok(true)
}

fn discard_coinbase_callbacks(events: &CoinbaseProviderEvents) {
    while events.try_recv().is_some() {}
}

fn drain_coinbase_callbacks<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    events: &CoinbaseProviderEvents,
    context: CoinbaseCallbackContext<'_>,
) -> Result<(), String> {
    let CoinbaseCallbackContext {
        streaming_generation,
        retained,
        model,
        worker_label,
        profile,
        instrument,
        bar_definition,
        level2,
        dom,
        message_tx,
        active_tail,
        segment_key,
    } = context;
    for _ in 0..PROVIDER_EVENT_BATCH {
        if !events.has_ready() {
            break;
        }
        let received = match worker.try_recv_coinbase_market_event(events) {
            Ok(received) => received,
            Err(_error)
                if matches!(
                    worker.provider_state().map_err(|error| error.to_string())?,
                    DesktopProviderState::RecoveryRequired { .. }
                ) =>
            {
                return Ok(());
            }
            Err(error) if is_stale_coinbase_callback(&error) => continue,
            Err(error) => return Err(error.to_string()),
        };
        let Some(received) = received else {
            continue;
        };
        if let CoinbaseDesktopMarketEvent::Level2 {
            generation,
            payload,
        } = received
        {
            publish_coinbase_depth(profile, generation, &payload, level2, dom, message_tx)?;
            continue;
        }
        let CoinbaseDesktopMarketEvent::Bar {
            generation,
            completed,
        } = received
        else {
            continue;
        };
        if streaming_generation != Some(generation) {
            // Bars completed before the covering snapshot installs are
            // discarded; installation resets aggregation and seeds from the
            // authenticated covering history instead.
            continue;
        }
        if profile.interval != axiusflow_market_data::ChartInterval::Minute1 {
            if let Some(finalized) = publish_aggregated_coinbase_interval(
                worker,
                generation,
                completed,
                retained,
                model,
                worker_label,
                profile,
                instrument,
                bar_definition,
                message_tx,
            )? {
                persist_tail_bar(worker, profile, active_tail, finalized, segment_key)?;
            }
            continue;
        }
        let item = live_provenance(completed, generation)?;
        let durable_bar = *item.value();
        let previous = retained
            .back()
            .ok_or_else(|| "Coinbase live stream has no snapshot predecessor".to_string())?
            .value()
            .source_sequence;
        let delta = StreamDelta::try_new(previous, item.value().source_sequence, item)
            .map_err(|error| error.to_string())?;
        retained.push_back(delta.item().clone());
        if retained.len() > MODEL_ITEM_CAPACITY {
            retained.pop_front();
        }
        persist_tail_bar(worker, profile, active_tail, durable_bar, segment_key)?;
        publish_update(
            worker,
            generation,
            model,
            ReplayStreamUpdate::Delta(delta),
            worker_label,
            message_tx,
        )?;
    }
    Ok(())
}

fn persist_tail_bar<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    profile: &ProductProfile,
    active_tail: &mut ActiveHistoryTail,
    bar: axiusflow_market_data::MarketBar,
    segment_key: &SegmentEncryptionKey,
) -> Result<(), String> {
    persist_live_tail(
        worker,
        profile,
        active_tail,
        bar,
        segment_key,
        unix_nanos()?,
    )
}

#[allow(clippy::too_many_arguments)]
fn publish_aggregated_coinbase_interval<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    generation: SessionGeneration,
    completed: CoinbaseAggregatedBar,
    retained: &mut VecDeque<ProvenancedMarketBar>,
    model: &mut MarketBarClientModel,
    worker_label: &str,
    profile: &ProductProfile,
    instrument: &InstrumentRevision,
    bar_definition: &BarDefinition,
    message_tx: &MarketWorkerSender,
) -> Result<Option<axiusflow_market_data::MarketBar>, String> {
    let interval = CoinbaseInterval::try_from(profile.interval).map_err(str::to_string)?;
    let (bucket, _) = aggregate_coinbase_bars(&[completed.bar], interval)?;
    let mut bucket = *bucket
        .first()
        .ok_or_else(|| "Coinbase live interval aggregation produced no bar".to_string())?;
    let previous_sequence = retained
        .back()
        .ok_or_else(|| "Coinbase live stream has no snapshot predecessor".to_string())?
        .value()
        .source_sequence;
    let replace_last = retained.back().is_some_and(|previous| {
        previous.value().exchange_timestamp_seconds == bucket.exchange_timestamp_seconds
    });
    let finalized = if replace_last {
        None
    } else {
        retained.back().map(|previous| *previous.value())
    };
    if replace_last {
        let previous = *retained
            .back()
            .ok_or_else(|| "Coinbase aggregate predecessor disappeared".to_string())?
            .value();
        let (combined, _) = aggregate_coinbase_bars(&[previous, completed.bar], interval)?;
        bucket = *combined
            .first()
            .ok_or_else(|| "Coinbase live interval merge produced no bar".to_string())?;
        bucket.source_sequence = previous_sequence;
    } else {
        bucket.source_sequence = previous_sequence
            .checked_add(1)
            .ok_or_else(|| "Coinbase live interval sequence overflow".to_string())?;
    }
    let item = live_provenance(
        CoinbaseAggregatedBar {
            bar: bucket,
            provider_timestamp_unix_nanos: completed.provider_timestamp_unix_nanos,
            provider_sequence_num: completed.provider_sequence_num,
        },
        generation,
    )?;
    if replace_last {
        retained.pop_back();
    }
    retained.push_back(item);
    if retained.len() > MODEL_ITEM_CAPACITY {
        retained.pop_front();
    }
    let snapshot = axiusflow_application::ReplaySnapshot::try_from_provenanced_values(
        instrument.clone(),
        axiusflow_application::ReplayProvenance::LiveProvider,
        bar_definition.clone(),
        model
            .current_generation()
            .map_or(1, |current| current.generation().saturating_add(1)),
        retained.iter().cloned().collect(),
    )
    .map_err(|error| error.to_string())?;
    publish_update(
        worker,
        generation,
        model,
        ReplayStreamUpdate::Snapshot(snapshot),
        worker_label,
        message_tx,
    )?;
    Ok(finalized)
}

fn publish_coinbase_depth(
    profile: &ProductProfile,
    generation: SessionGeneration,
    payload: &[u8],
    level2: &mut Option<CoinbaseLevel2Book>,
    dom: &mut axiusflow_terminal_ui::ReadOnlyDom,
    message_tx: &MarketWorkerSender,
) -> Result<(), String> {
    let generation_value = generation.get();
    if level2.is_none()
        || dom.selection().is_none_or(|selection| {
            selection.session_generation != generation_value
                || selection.instrument_id != profile.instrument_id
        })
    {
        *level2 = Some(
            CoinbaseLevel2Book::try_new(
                profile.product_id.clone(),
                profile.price_scale,
                profile.quantity_scale,
                generation_value,
            )
            .map_err(|error| error.to_string())?,
        );
        dom.select(axiusflow_terminal_ui::DomSelection {
            provider_id: "coinbase".to_string(),
            instrument_id: profile.instrument_id.clone(),
            entitlement_id: axiusflow_coinbase_market_adapter::ENTITLEMENT_CLASS.to_string(),
            session_generation: generation_value,
            selection_generation: generation_value,
            precision: InstrumentPrecision::try_new(profile.price_scale, profile.quantity_scale)
                .map_err(|error| error.to_string())?,
        });
    }
    let outcome = level2
        .as_mut()
        .ok_or_else(|| "Coinbase Level 2 state is unavailable".to_string())?
        .apply_message(payload, unix_nanos()?)
        .map_err(|error| error.to_string())?;
    let frame = match outcome {
        CoinbaseLevel2Outcome::Snapshot(snapshot) => match dom
            .apply_event(&MarketEvent::DepthSnapshot(snapshot))
            .map_err(|error| error.to_string())?
        {
            axiusflow_terminal_ui::DomUpdateOutcome::Published(frame)
            | axiusflow_terminal_ui::DomUpdateOutcome::RecoveryRequired(frame, _) => Some(frame),
            axiusflow_terminal_ui::DomUpdateOutcome::Ignored => None,
        },
        CoinbaseLevel2Outcome::Deltas { deltas, .. } => {
            let mut frame = None;
            for delta in deltas {
                match dom
                    .apply_event(&MarketEvent::DepthDelta(delta))
                    .map_err(|error| error.to_string())?
                {
                    axiusflow_terminal_ui::DomUpdateOutcome::Published(next)
                    | axiusflow_terminal_ui::DomUpdateOutcome::RecoveryRequired(next, _) => {
                        frame = Some(next);
                    }
                    axiusflow_terminal_ui::DomUpdateOutcome::Ignored => {}
                }
            }
            frame
        }
        CoinbaseLevel2Outcome::RecoveryRequired => dom.mark_stale(),
        CoinbaseLevel2Outcome::Ignored => None,
    };
    if let Some(frame) = frame {
        let _ = message_tx.send(MarketWorkerMessage::CoinbaseDom(frame));
    }
    Ok(())
}

fn is_stale_coinbase_callback(error: &CoinbaseDesktopEventError) -> bool {
    matches!(
        error,
        CoinbaseDesktopEventError::Runtime(DesktopMarketWorkerError::Provider(
            DesktopProviderError::StaleGeneration
        ))
    )
}

fn request_recovery_if_required<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    events: &CoinbaseProviderEvents,
    state: &mut LiveLoopState,
    message_tx: &MarketWorkerSender,
) -> Result<(), String> {
    let provider_state = worker.provider_state().map_err(|error| error.to_string())?;
    if let DesktopProviderState::RecoveryRequired { reason, .. } = provider_state {
        worker.reset_aggregation();
        state.streaming_generation = None;
        state.pending_viewport = None;
        state.last_requested_range = None;
        cancel_inflight_history(&mut state.history);
        state.retained.clear();
        if !state.recovery_announced {
            message_tx
                .send(MarketWorkerMessage::State {
                    state: ChartState::Recovering,
                    message: format!(
                        "Coinbase provider recovery required ({reason:?}, source={:?}); awaiting a fresh covering snapshot",
                        events.invalid_reason()
                    ),
                })
                .map_err(|_| "desktop market UI channel disconnected".to_string())?;
            state.recovery_announced = true;
        }
        if state.reconnect_backoff.retry_ready(Instant::now()) {
            worker
                .request_connection()
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod startup_path_tests {
    use super::{
        cache_can_resume_live, ensure_history_parent, intersect_history_range,
        subtract_history_range,
    };
    use axiusflow_provider_history::{CoverageClass, HistoryRange};
    use std::{fs, path::PathBuf, time::SystemTime};

    struct TestRoot(PathBuf);

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn default_history_parent_is_created_before_storage_opens() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system time follows the epoch")
            .as_nanos();
        let root = TestRoot(std::env::temp_dir().join(format!(
            "axiusflow-coinbase-parent-{}-{unique}",
            std::process::id()
        )));
        let history = root.0.join("Axiusflow/market-history/coinbase");
        ensure_history_parent(&history).expect("nested history parent is created");
        assert!(history.parent().is_some_and(std::path::Path::is_dir));
        assert!(
            !history.exists(),
            "storage retains ownership of the final root"
        );
    }

    #[test]
    fn only_complete_cached_coverage_with_a_predecessor_bypasses_provider_history() {
        assert!(cache_can_resume_live(CoverageClass::Complete, true));
        assert!(!cache_can_resume_live(CoverageClass::Complete, false));
        for class in [
            CoverageClass::Partial,
            CoverageClass::ConfirmedEmpty,
            CoverageClass::Missing,
            CoverageClass::Invalidated,
            CoverageClass::Quarantined,
        ] {
            assert!(!cache_can_resume_live(class, true));
        }
    }

    #[test]
    fn visible_history_split_fetches_only_intersections_and_retains_older_gaps() {
        let missing = HistoryRange {
            start_unix_nanos: 100,
            end_unix_nanos: 500,
        };
        let visible = HistoryRange {
            start_unix_nanos: 300,
            end_unix_nanos: 500,
        };
        assert_eq!(intersect_history_range(missing, visible), Some(visible));
        assert_eq!(
            subtract_history_range(missing, visible),
            vec![HistoryRange {
                start_unix_nanos: 100,
                end_unix_nanos: 300,
            }]
        );
        let disjoint = HistoryRange {
            start_unix_nanos: 700,
            end_unix_nanos: 900,
        };
        assert_eq!(subtract_history_range(missing, disjoint), vec![missing]);
    }
}
