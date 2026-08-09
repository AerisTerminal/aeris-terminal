use crate::{
    rithmic_history::{
        InstalledRithmicInstrument, RithmicHistoryResult, RithmicHistoryTask, RithmicSeriesRequest,
        history_message,
    },
    rithmic_live_chart::{RithmicChartGeneration, RithmicLiveChart},
    rithmic_shell::RithmicShellState,
    rithmic_transition_capture::{
        AppliedEnvironmentEvidence, EvidenceFlag, NativeTransitionCapture,
        RithmicRuntimeStateEvidence,
    },
};
use axiusflow_application::ProvenancedMarketBar;
use axiusflow_coinbase_coordinator::market_worker::{
    MarketDataWorker, MarketWorkerCommand, MarketWorkerMessage, MarketWorkerStartup,
    market_worker_channel,
};
use axiusflow_desktop_history::HistoryWorkerConfig;
use axiusflow_desktop_provider_runtime::{
    AuthenticationState, DesktopMarketWorker, DesktopMarketWorkerConfig, DesktopProviderConfig,
    ProviderInvalidationReason, ProviderSessionEvent,
};
use axiusflow_desktop_storage::CatalogKey;
use axiusflow_instruments::InstrumentPrecision;
use axiusflow_market_data::MarketEvent;
use axiusflow_observability::FeedConnectionState;
use axiusflow_platform_runtime::{
    CredentialVault, NativeCredentialVault, NativeNetworkMonitor, NativePowerMonitor, NetworkEvent,
    PowerEvent,
};
use axiusflow_rithmic_protocol_adapter::{
    AppliedRithmicEvent, MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES, RITHMIC_TEST_VAULT_KEY,
    RITHMIC_TEST_VAULT_SERVICE, RithmicCallbackLimits, RithmicCatalogEvent,
    RithmicEnvironmentEvent, RithmicProviderConfig, RithmicProviderDriver, RithmicRetryScheduler,
    RithmicSessionLimits, apply_rithmic_environment_event, try_recv_rithmic_event,
};
use axiusflow_terminal_ui::{DomSelection, DomUpdateOutcome, ReadOnlyDom};
use std::{
    collections::VecDeque,
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, TryRecvError},
    },
    thread::{self, ThreadId},
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

const MESSAGE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 8;
const ENVIRONMENT_CAPACITY: usize = 8;
const ENVIRONMENT_BATCH: usize = 8;
const CALLBACK_CAPACITY: usize = 256;
const CALLBACK_BYTES: usize = 8 * 1024 * 1024;
const MAXIMUM_DEPTH: usize = 256;
const IDLE_WAIT: Duration = Duration::from_millis(50);
const MESSAGE_SILENCE: Duration = Duration::from_mins(2);
const CATALOG_KEY_ID: &str = "rithmic-test-history-catalog-key-v1";
const MAXIMUM_BUFFERED_HISTORY_TRADES: usize = 4_096;
const NETWORK_MONITOR_FAILED: u8 = 1;
const POWER_MONITOR_FAILED: u8 = 2;

type RithmicWorker =
    DesktopMarketWorker<ProvenancedMarketBar, NativeCredentialVault, RithmicProviderDriver>;
type RithmicEvents = axiusflow_rithmic_protocol_adapter::RithmicProviderEvents;

#[derive(Clone, Copy)]
struct ObservedEnvironmentEvent {
    source_ordinal: u64,
    event: RithmicEnvironmentEvent,
}

struct TransitionCaptureContext<'a> {
    events: &'a Receiver<ObservedEnvironmentEvent>,
    overflow: &'a AtomicU64,
    monitor_failures: &'a AtomicU8,
    capture: &'a mut Option<NativeTransitionCapture>,
}

#[derive(Clone, Copy)]
enum CatalogDispatchDomain {
    Search,
    Selection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EnvironmentMonitorFailure {
    Network,
    Power,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerExitReason {
    Shutdown,
    EnvironmentMonitorFailed,
}

struct EnvironmentMonitorHandles {
    initial_network: Option<NetworkEvent>,
    overflow: Arc<AtomicU64>,
    failures: Arc<AtomicU8>,
}

struct EnvironmentForwarding<'a> {
    sender: &'a mpsc::SyncSender<ObservedEnvironmentEvent>,
    source_ordinal: &'a AtomicU64,
    overflow: &'a AtomicU64,
    fail_capture_on_overflow: bool,
    monitor_failures: &'a AtomicU8,
    failure_flag: u8,
}

struct RithmicRuntimeState {
    selection_installed: bool,
    installed_instrument: Option<InstalledRithmicInstrument>,
    history: Option<RithmicHistoryTask>,
    live_chart: Option<RithmicLiveChart>,
    pending_live_request: Option<RithmicSeriesRequest>,
    buffered_history_trades: VecDeque<axiusflow_market_data::MarketTrade>,
    history_trade_overflow: bool,
    dom: ReadOnlyDom,
}

impl RithmicRuntimeState {
    fn new() -> Self {
        Self {
            selection_installed: false,
            installed_instrument: None,
            history: RithmicHistoryTask::start().ok(),
            live_chart: None,
            pending_live_request: None,
            buffered_history_trades: VecDeque::with_capacity(MAXIMUM_BUFFERED_HISTORY_TRADES),
            history_trade_overflow: false,
            dom: ReadOnlyDom::new(nonzero(20)),
        }
    }

    fn evidence(&self) -> RithmicRuntimeStateEvidence {
        RithmicRuntimeStateEvidence {
            selection_installed: EvidenceFlag::from(self.selection_installed),
            instrument_installed: EvidenceFlag::from(self.installed_instrument.is_some()),
            history_request_active: EvidenceFlag::from(
                self.history
                    .as_ref()
                    .is_some_and(RithmicHistoryTask::has_active_request),
            ),
            live_chart_installed: EvidenceFlag::from(self.live_chart.is_some()),
            pending_live_request: EvidenceFlag::from(self.pending_live_request.is_some()),
            buffered_history_trades: self.buffered_history_trades.len(),
            history_trade_overflow: EvidenceFlag::from(self.history_trade_overflow),
            depth_selection_installed: EvidenceFlag::from(self.dom.selection().is_some()),
        }
    }
}

pub(crate) fn start(
    history_root: PathBuf,
    ui_thread: ThreadId,
    detailed_diagnostics: bool,
    native_transition_report: Option<PathBuf>,
) -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    let shell = RithmicShellState::local()?;
    spawn_worker(shell, move |message_tx, command_rx| {
        run(
            &message_tx,
            &command_rx,
            history_root,
            ui_thread,
            detailed_diagnostics,
            native_transition_report,
        );
    })
}

fn spawn_worker(
    shell: RithmicShellState,
    task: impl FnOnce(
        axiusflow_coinbase_coordinator::market_worker::MarketWorkerSender,
        Receiver<MarketWorkerCommand>,
    ) + Send
    + 'static,
) -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    let (message_tx, message_rx) = market_worker_channel(nonzero(MESSAGE_CAPACITY));
    let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("axiusflow-rithmic-market-worker".to_string())
        .spawn(move || {
            task(message_tx, command_rx);
            let _ = shutdown_tx.send(());
        })
        .map_err(|_| "Rithmic market worker thread is unavailable".to_string())?;

    Ok((
        MarketWorkerStartup::Shell(shell),
        MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, None),
    ))
}

fn run(
    messages: &axiusflow_coinbase_coordinator::market_worker::MarketWorkerSender,
    commands: &Receiver<MarketWorkerCommand>,
    history_root: PathBuf,
    ui_thread: ThreadId,
    detailed_diagnostics: bool,
    native_transition_report: Option<PathBuf>,
) {
    let mut transition_capture = match native_transition_report {
        Some(report_path) => {
            let Ok(capture) = NativeTransitionCapture::start(report_path) else {
                send_connection(
                    messages,
                    FeedConnectionState::Recovering,
                    "native transition evidence could not start",
                );
                wait_for_shutdown(commands);
                return;
            };
            Some(capture)
        }
        None => None,
    };
    let capture_enabled = transition_capture.is_some();
    let (environment_tx, environment_rx) = mpsc::sync_channel(ENVIRONMENT_CAPACITY);
    let Ok(environment) = start_environment_monitors(environment_tx, capture_enabled) else {
        apply_capture(&mut transition_capture, |capture| {
            capture.observe_monitor_failure()
        });
        apply_capture(&mut transition_capture, |capture| capture.finalize(false));
        send_connection(
            messages,
            FeedConnectionState::Stopped,
            "Rithmic Test requires native lifecycle monitoring",
        );
        wait_for_shutdown(commands);
        return;
    };
    let EnvironmentMonitorHandles {
        initial_network,
        overflow: environment_overflow,
        failures: monitor_failures,
    } = environment;
    apply_capture(&mut transition_capture, |capture| {
        capture.observe_initial_network(initial_network)
    });
    let (wake_tx, wake_rx) = mpsc::sync_channel(1);
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = wake_tx.try_send(());
    });
    let opened = open_worker(history_root, ui_thread, detailed_diagnostics, wake);
    let Ok((worker, events)) = opened else {
        send_connection(
            messages,
            FeedConnectionState::Recovering,
            "Rithmic Test is waiting for credentials or local runtime access",
        );
        wait_for_shutdown(commands);
        return;
    };
    let mut transitions = TransitionCaptureContext {
        events: &environment_rx,
        overflow: &environment_overflow,
        monitor_failures: &monitor_failures,
        capture: &mut transition_capture,
    };
    run_connected(
        messages,
        commands,
        worker,
        &events,
        &wake_rx,
        initial_network,
        &mut transitions,
    );
}

fn run_connected(
    messages: &axiusflow_coinbase_coordinator::market_worker::MarketWorkerSender,
    commands: &Receiver<MarketWorkerCommand>,
    mut worker: RithmicWorker,
    events: &RithmicEvents,
    wake_rx: &Receiver<()>,
    initial_network: Option<NetworkEvent>,
    transitions: &mut TransitionCaptureContext<'_>,
) {
    send_connection(
        messages,
        FeedConnectionState::Discovering,
        "discovering Rithmic Test systems",
    );
    let mut retries = RithmicRetryScheduler::default();
    let mut state = RithmicRuntimeState::new();
    apply_initial_network_state(&mut worker, events, &mut retries, messages, initial_network);
    if worker.request_connection().is_err() {
        send_connection(
            messages,
            FeedConnectionState::Recovering,
            "Rithmic Test credentials are unavailable or require attention",
        );
    }

    let exit_reason = loop {
        if transitions.monitor_failures.swap(0, Ordering::AcqRel) != 0 {
            apply_capture(transitions.capture, |capture| {
                capture.observe_monitor_failure()
            });
            reset_live_state(&mut state);
            break WorkerExitReason::EnvironmentMonitorFailed;
        }
        let overflow_count = transitions.overflow.swap(0, Ordering::AcqRel);
        if overflow_count > 0 {
            apply_capture(transitions.capture, |capture| {
                capture.observe_overflow(overflow_count)
            });
        }
        if process_command(commands, &worker, events, messages, &mut state) {
            break WorkerExitReason::Shutdown;
        }
        drain_environment_events(
            transitions.events,
            &mut worker,
            events,
            &mut retries,
            messages,
            &mut state,
            transitions.capture,
        );
        drain_events(
            &mut worker,
            events,
            &mut retries,
            messages,
            &mut state,
            transitions.capture,
        );
        if let Some(result) = state
            .history
            .as_mut()
            .and_then(RithmicHistoryTask::try_recv)
            && apply_history_result(messages, &mut state, result)
        {
            continue;
        }
        if retries
            .retry_due(&mut worker, Instant::now())
            .is_ok_and(|generation| generation.is_some())
        {
            reset_live_state(&mut state);
            send_connection(
                messages,
                FeedConnectionState::Discovering,
                "reconnecting to Rithmic Test",
            );
        }
        observe_capture_runtime(transitions.capture, &worker, &state);
        if let Ok(Some(snapshot)) = worker.try_diagnostics_snapshot() {
            let _ = messages.send(MarketWorkerMessage::Diagnostics(Box::new(snapshot)));
        }
        let wait = retries.ticket().map_or(IDLE_WAIT, |ticket| {
            ticket
                .due_at
                .saturating_duration_since(Instant::now())
                .min(IDLE_WAIT)
        });
        match wake_rx.recv_timeout(wait) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break WorkerExitReason::Shutdown,
        }
    };
    let stopped = worker.stop().is_ok();
    apply_capture(transitions.capture, |capture| capture.finalize(stopped));
    send_connection(
        messages,
        if stopped {
            FeedConnectionState::Stopped
        } else {
            FeedConnectionState::Recovering
        },
        if stopped && exit_reason == WorkerExitReason::EnvironmentMonitorFailed {
            "Rithmic Test stopped because native lifecycle monitoring became unavailable"
        } else if stopped {
            "Rithmic Test session stopped"
        } else {
            "Rithmic Test session stop was not confirmed"
        },
    );
}

fn apply_initial_network_state(
    worker: &mut RithmicWorker,
    events: &RithmicEvents,
    retries: &mut RithmicRetryScheduler,
    messages: &axiusflow_coinbase_coordinator::market_worker::MarketWorkerSender,
    initial_network: Option<NetworkEvent>,
) {
    if initial_network != Some(NetworkEvent::Unavailable) {
        return;
    }
    if apply_rithmic_environment_event(
        worker,
        events,
        retries,
        RithmicEnvironmentEvent::Network(NetworkEvent::Unavailable),
    )
    .is_err()
    {
        send_connection(
            messages,
            FeedConnectionState::Recovering,
            "Rithmic Test could not apply native network state",
        );
    } else {
        let (connection, message) = initial_offline_status();
        send_connection(messages, connection, message);
    }
}

fn start_environment_monitors(
    sender: mpsc::SyncSender<ObservedEnvironmentEvent>,
    fail_capture_on_overflow: bool,
) -> Result<EnvironmentMonitorHandles, EnvironmentMonitorFailure> {
    let network = NativeNetworkMonitor::connect()
        .map(|monitor| {
            let current = monitor.current();
            (monitor, current)
        })
        .map_err(|_| EnvironmentMonitorFailure::Network);
    let power = NativePowerMonitor::connect().map_err(|_| EnvironmentMonitorFailure::Power);
    let (network, power, initial_network) = require_environment_monitors(network, power)?;
    let source_ordinal = Arc::new(AtomicU64::new(0));
    let overflow = Arc::new(AtomicU64::new(0));
    let monitor_failures = Arc::new(AtomicU8::new(0));
    {
        let network_sender = sender.clone();
        let network_ordinal = Arc::clone(&source_ordinal);
        let network_overflow = Arc::clone(&overflow);
        let network_failures = Arc::clone(&monitor_failures);
        thread::Builder::new()
            .name("axiusflow-rithmic-network-monitor".to_string())
            .spawn(move || {
                let mut monitor = network;
                forward_environment_events(
                    || monitor.next_event().map_err(|_| ()),
                    RithmicEnvironmentEvent::Network,
                    &EnvironmentForwarding {
                        sender: &network_sender,
                        source_ordinal: &network_ordinal,
                        overflow: &network_overflow,
                        fail_capture_on_overflow,
                        monitor_failures: &network_failures,
                        failure_flag: NETWORK_MONITOR_FAILED,
                    },
                );
            })
            .map_err(|_| EnvironmentMonitorFailure::Network)?;
    }
    {
        let power_ordinal = Arc::clone(&source_ordinal);
        let power_overflow = Arc::clone(&overflow);
        let power_failures = Arc::clone(&monitor_failures);
        thread::Builder::new()
            .name("axiusflow-rithmic-power-monitor".to_string())
            .spawn(move || {
                let mut monitor = power;
                forward_environment_events(
                    || monitor.next_event().map_err(|_| ()),
                    RithmicEnvironmentEvent::Power,
                    &EnvironmentForwarding {
                        sender: &sender,
                        source_ordinal: &power_ordinal,
                        overflow: &power_overflow,
                        fail_capture_on_overflow,
                        monitor_failures: &power_failures,
                        failure_flag: POWER_MONITOR_FAILED,
                    },
                );
            })
            .map_err(|_| EnvironmentMonitorFailure::Power)?;
    }
    Ok(EnvironmentMonitorHandles {
        initial_network: Some(initial_network),
        overflow,
        failures: monitor_failures,
    })
}

fn require_environment_monitors<N, P>(
    network: Result<(N, NetworkEvent), EnvironmentMonitorFailure>,
    power: Result<P, EnvironmentMonitorFailure>,
) -> Result<(N, P, NetworkEvent), EnvironmentMonitorFailure> {
    let (network, initial_network) = network?;
    let power = power?;
    Ok((network, power, initial_network))
}

fn forward_environment_events<E>(
    mut next_event: impl FnMut() -> Result<E, ()>,
    wrap: impl Fn(E) -> RithmicEnvironmentEvent,
    forwarding: &EnvironmentForwarding<'_>,
) {
    loop {
        let Ok(event) = next_event() else {
            forwarding
                .monitor_failures
                .fetch_or(forwarding.failure_flag, Ordering::AcqRel);
            break;
        };
        let observed = ObservedEnvironmentEvent {
            source_ordinal: forwarding
                .source_ordinal
                .fetch_add(1, Ordering::AcqRel)
                .saturating_add(1),
            event: wrap(event),
        };
        if forwarding.fail_capture_on_overflow {
            match forwarding.sender.try_send(observed) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(_)) => {
                    forwarding.overflow.fetch_add(1, Ordering::AcqRel);
                }
                Err(mpsc::TrySendError::Disconnected(_)) => break,
            }
        } else if forwarding.sender.send(observed).is_err() {
            break;
        }
    }
}

fn drain_environment_events(
    receiver: &Receiver<ObservedEnvironmentEvent>,
    worker: &mut RithmicWorker,
    events: &RithmicEvents,
    retries: &mut RithmicRetryScheduler,
    messages: &axiusflow_coinbase_coordinator::market_worker::MarketWorkerSender,
    state: &mut RithmicRuntimeState,
    transition_capture: &mut Option<NativeTransitionCapture>,
) {
    for _ in 0..ENVIRONMENT_BATCH {
        let Ok(observed) = receiver.try_recv() else {
            break;
        };
        let event = observed.event;
        let retired_generation = active_generation(worker)
            .ok()
            .flatten()
            .map(axiusflow_desktop_provider_runtime::SessionGeneration::get);
        let before = state.evidence();
        let Ok(generation) = apply_rithmic_environment_event(worker, events, retries, event) else {
            apply_capture(transition_capture, |capture| {
                capture.observe_environment_failure()
            });
            send_connection(
                messages,
                FeedConnectionState::Recovering,
                "Rithmic Test native environment transition failed closed",
            );
            continue;
        };
        reset_live_state(state);
        let fresh_generation =
            generation.map(axiusflow_desktop_provider_runtime::SessionGeneration::get);
        let session_stop_confirmed = retired_generation.is_some()
            && matches!(
                event,
                RithmicEnvironmentEvent::Network(NetworkEvent::Unavailable)
                    | RithmicEnvironmentEvent::Power(PowerEvent::Suspending)
            );
        apply_capture(transition_capture, |capture| {
            capture.observe_environment_applied(AppliedEnvironmentEvidence {
                event,
                source_ordinal: observed.source_ordinal,
                retired_generation,
                fresh_generation,
                session_stop_confirmed,
                before,
                after: state.evidence(),
            })
        });
        let (connection, message) = match (event, generation) {
            (_, Some(_)) => (
                FeedConnectionState::Discovering,
                "native environment restored; reconnecting to Rithmic Test",
            ),
            (RithmicEnvironmentEvent::Network(NetworkEvent::Unavailable), None) => (
                FeedConnectionState::Disconnected,
                "Rithmic Test paused while the network is unavailable",
            ),
            (RithmicEnvironmentEvent::Power(PowerEvent::Suspending), None) => (
                FeedConnectionState::Disconnected,
                "Rithmic Test paused while the system is suspended",
            ),
            _ => (
                FeedConnectionState::Recovering,
                "Rithmic Test is waiting for native environment recovery",
            ),
        };
        send_connection(messages, connection, message);
    }
}

fn reset_live_state(state: &mut RithmicRuntimeState) {
    if let Some(history) = state.history.as_mut() {
        history.cancel();
    }
    state.selection_installed = false;
    state.installed_instrument = None;
    state.live_chart = None;
    state.pending_live_request = None;
    state.buffered_history_trades.clear();
    state.history_trade_overflow = false;
    state.dom.clear();
}

fn apply_history_result(
    messages: &axiusflow_coinbase_coordinator::market_worker::MarketWorkerSender,
    state: &mut RithmicRuntimeState,
    result: RithmicHistoryResult,
) -> bool {
    let generation = RithmicChartGeneration {
        selection: result.selection_generation,
        series: result.series_generation,
    };
    let matching_request = state.pending_live_request.is_some_and(|request| {
        request.selection_generation == generation.selection
            && request.series_generation == generation.series
    });
    if !matching_request {
        return false;
    }
    let mut chart =
        result.result.as_ref().ok().and_then(|bootstrap| {
            RithmicLiveChart::from_history(generation, &bootstrap.snapshot).ok()
        });
    let _ = messages.send(history_message(result));
    if matching_request && !state.history_trade_overflow {
        let mut valid = true;
        if let Some(active_chart) = chart.as_mut() {
            while let Some(trade) = state.buffered_history_trades.pop_front() {
                if !publish_trade(messages, active_chart, &trade) {
                    valid = false;
                    break;
                }
            }
        }
        if !valid {
            chart = None;
        }
    }
    if matching_request && state.history_trade_overflow && reschedule_history(state) {
        let _ = messages.send(MarketWorkerMessage::State {
            state: axiusflow_coinbase_coordinator::market_worker::ChartState::Recovering,
            message: "Rithmic history is covering buffered trade overflow".to_string(),
        });
        return true;
    }
    state.pending_live_request = None;
    state.buffered_history_trades.clear();
    state.history_trade_overflow = false;
    state.live_chart = chart;
    false
}

const fn initial_offline_status() -> (FeedConnectionState, &'static str) {
    (
        FeedConnectionState::Disconnected,
        "Rithmic Test is offline; connection will start when the network returns",
    )
}

fn reschedule_history(state: &mut RithmicRuntimeState) -> bool {
    let rescheduled = state
        .pending_live_request
        .zip(state.installed_instrument.clone())
        .is_some_and(|(request, instrument)| {
            state
                .history
                .as_mut()
                .is_some_and(|history| history.request(request, instrument).is_ok())
        });
    state.buffered_history_trades.clear();
    state.history_trade_overflow = false;
    rescheduled
}

fn process_command(
    commands: &Receiver<MarketWorkerCommand>,
    worker: &RithmicWorker,
    events: &RithmicEvents,
    messages: &axiusflow_coinbase_coordinator::market_worker::MarketWorkerSender,
    state: &mut RithmicRuntimeState,
) -> bool {
    let (dispatch, rejection, failure_message) = match commands.try_recv() {
        Ok(MarketWorkerCommand::Shutdown) | Err(TryRecvError::Disconnected) => return true,
        Ok(MarketWorkerCommand::RithmicSearch(search)) => {
            let command_generation = search.generation();
            (
                dispatch_catalog_command(worker, events, |events, generation| {
                    events.search_symbols(generation, search)
                }),
                Some(catalog_dispatch_rejection(
                    command_generation,
                    CatalogDispatchDomain::Search,
                )),
                "Rithmic symbol search could not be scheduled",
            )
        }
        Ok(MarketWorkerCommand::RithmicSelect(selection)) => {
            let command_generation = selection.generation();
            state.live_chart = None;
            (
                dispatch_catalog_command(worker, events, |events, generation| {
                    events.select_instrument(generation, selection)
                }),
                Some(catalog_dispatch_rejection(
                    command_generation,
                    CatalogDispatchDomain::Selection,
                )),
                "Rithmic symbol selection could not be scheduled",
            )
        }
        Ok(MarketWorkerCommand::RithmicHistory(request)) => {
            state.live_chart = None;
            state.pending_live_request = Some(request);
            state.buffered_history_trades.clear();
            state.history_trade_overflow = false;
            (
                state.history.as_mut().ok_or(()).and_then(|history| {
                    state
                        .installed_instrument
                        .clone()
                        .ok_or(())
                        .and_then(|instrument| history.request(request, instrument).map_err(|_| ()))
                }),
                None,
                "Rithmic visible history could not be scheduled",
            )
        }
        Ok(
            MarketWorkerCommand::Recovery(_)
            | MarketWorkerCommand::CoinbaseSelect(_)
            | MarketWorkerCommand::ChartViewport(_),
        )
        | Err(TryRecvError::Empty) => return false,
    };
    if dispatch.is_err() {
        if let Some(rejection) = rejection {
            let _ = messages.send(MarketWorkerMessage::RithmicCatalog(rejection));
        }
        send_connection(
            messages,
            catalog_connection_state(state.selection_installed),
            failure_message,
        );
    }
    false
}

fn catalog_dispatch_rejection(
    command_generation: NonZeroUsize,
    domain: CatalogDispatchDomain,
) -> RithmicCatalogEvent {
    let reason = match domain {
        CatalogDispatchDomain::Search => {
            axiusflow_rithmic_protocol_adapter::RithmicCatalogRejection::SearchDispatchUnavailable
        }
        CatalogDispatchDomain::Selection => {
            axiusflow_rithmic_protocol_adapter::RithmicCatalogRejection::SelectionDispatchUnavailable
        }
    };
    RithmicCatalogEvent::CommandRejected {
        session_generation: None,
        command_generation,
        reason,
    }
}

fn drain_events(
    worker: &mut RithmicWorker,
    events: &RithmicEvents,
    retries: &mut RithmicRetryScheduler,
    messages: &axiusflow_coinbase_coordinator::market_worker::MarketWorkerSender,
    state: &mut RithmicRuntimeState,
    transition_capture: &mut Option<NativeTransitionCapture>,
) {
    while events.has_ready() {
        while let Some(callback) = events.try_recv_catalog() {
            if active_generation(worker).ok().flatten() != Some(callback.generation) {
                continue;
            }
            if let axiusflow_rithmic_protocol_adapter::RithmicCatalogEvent::SelectionInstalled {
                selection_generation,
                instrument,
                entitlement_id,
                ..
            } = &callback.event
            {
                install_catalog_selection(
                    state,
                    callback.generation,
                    *selection_generation,
                    instrument,
                    entitlement_id,
                );
                if let Some(frame) = state.dom.frame() {
                    let _ = messages.send(MarketWorkerMessage::RithmicDom(frame));
                }
            }
            let _ = messages.send(MarketWorkerMessage::RithmicCatalog(callback.event));
        }
        match try_recv_rithmic_event(worker, events, retries, Instant::now()) {
            Ok(Some(event)) => {
                apply_capture(transition_capture, |capture| {
                    capture.observe_provider_event(&event)
                });
                if matches!(
                    event,
                    AppliedRithmicEvent::RetryScheduled(_)
                        | AppliedRithmicEvent::TerminalFailure { .. }
                        | AppliedRithmicEvent::Semantic(ProviderSessionEvent::Invalidated { .. })
                ) {
                    if let Some(history) = state.history.as_mut() {
                        history.cancel();
                    }
                    state.selection_installed = false;
                    state.installed_instrument = None;
                    state.live_chart = None;
                    state.pending_live_request = None;
                    state.buffered_history_trades.clear();
                    state.history_trade_overflow = false;
                    if let Some(frame) = state.dom.mark_stale() {
                        let _ = messages.send(MarketWorkerMessage::RithmicDom(frame));
                    }
                }
                publish_live_chart(messages, &event, state);
                publish_dom(messages, &event, &mut state.dom);
                publish_event(messages, &event, &mut state.selection_installed);
            }
            Ok(None) => break,
            Err(_) => {
                send_connection(
                    messages,
                    FeedConnectionState::Recovering,
                    "Rithmic Test session recovery is required",
                );
                break;
            }
        }
    }
}

fn install_catalog_selection(
    state: &mut RithmicRuntimeState,
    session_generation: axiusflow_desktop_provider_runtime::SessionGeneration,
    selection_generation: NonZeroUsize,
    instrument: &axiusflow_desktop_provider_runtime::InstrumentDescriptor,
    entitlement_id: &str,
) {
    if let Some(history) = state.history.as_mut() {
        history.cancel();
    }
    state.live_chart = None;
    state.pending_live_request = None;
    state.buffered_history_trades.clear();
    state.history_trade_overflow = false;
    state.selection_installed = true;
    state.installed_instrument = Some(InstalledRithmicInstrument {
        selection_generation,
        descriptor: instrument.clone(),
        entitlement_id: entitlement_id.to_string(),
    });
    if let Ok(precision) =
        InstrumentPrecision::try_new(instrument.price_scale, instrument.quantity_scale)
    {
        state.dom.select(DomSelection {
            provider_id: "rithmic".to_string(),
            instrument_id: instrument.instrument_id.clone(),
            entitlement_id: entitlement_id.to_string(),
            session_generation: session_generation.get(),
            selection_generation: u64::try_from(selection_generation.get()).unwrap_or(u64::MAX),
            precision,
        });
    }
}

fn observe_capture_runtime(
    transition_capture: &mut Option<NativeTransitionCapture>,
    worker: &RithmicWorker,
    state: &RithmicRuntimeState,
) {
    let generation = active_generation(worker)
        .ok()
        .flatten()
        .map(axiusflow_desktop_provider_runtime::SessionGeneration::get);
    let evidence = state.evidence();
    apply_capture(transition_capture, |capture| {
        capture.observe_runtime(generation, &evidence)
    });
}

fn apply_capture(
    transition_capture: &mut Option<NativeTransitionCapture>,
    operation: impl FnOnce(
        &mut NativeTransitionCapture,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>,
) {
    let failed = transition_capture
        .as_mut()
        .is_some_and(|capture| operation(capture).is_err());
    if failed {
        eprintln!("native transition evidence checkpoint failed");
        *transition_capture = None;
    }
}

fn publish_dom(
    messages: &axiusflow_coinbase_coordinator::market_worker::MarketWorkerSender,
    event: &AppliedRithmicEvent,
    dom: &mut ReadOnlyDom,
) {
    let AppliedRithmicEvent::Semantic(ProviderSessionEvent::Market { event, .. }) = event else {
        return;
    };
    match dom.apply_event(event) {
        Ok(DomUpdateOutcome::Published(frame) | DomUpdateOutcome::RecoveryRequired(frame, _)) => {
            let _ = messages.send(MarketWorkerMessage::RithmicDom(frame));
        }
        Ok(DomUpdateOutcome::Ignored) => {}
        Err(_) => {
            if let Some(frame) = dom.frame() {
                let _ = messages.send(MarketWorkerMessage::RithmicDom(frame));
            }
        }
    }
}

fn publish_live_chart(
    messages: &axiusflow_coinbase_coordinator::market_worker::MarketWorkerSender,
    event: &AppliedRithmicEvent,
    state: &mut RithmicRuntimeState,
) {
    let AppliedRithmicEvent::Semantic(ProviderSessionEvent::Market {
        event: MarketEvent::Trade(trade),
        ..
    }) = event
    else {
        return;
    };
    let Some(chart) = state.live_chart.as_mut() else {
        if state.pending_live_request.is_some() && !state.history_trade_overflow {
            buffer_history_trade(
                &mut state.buffered_history_trades,
                &mut state.history_trade_overflow,
                trade,
            );
        }
        return;
    };
    if !publish_trade(messages, chart, trade) {
        state.live_chart = None;
    }
}

fn buffer_history_trade(
    buffer: &mut VecDeque<axiusflow_market_data::MarketTrade>,
    overflowed: &mut bool,
    trade: &axiusflow_market_data::MarketTrade,
) {
    if buffer.len() >= MAXIMUM_BUFFERED_HISTORY_TRADES {
        buffer.clear();
        *overflowed = true;
    } else {
        buffer.push_back(trade.clone());
    }
}

fn publish_trade(
    messages: &axiusflow_coinbase_coordinator::market_worker::MarketWorkerSender,
    chart: &mut RithmicLiveChart,
    trade: &axiusflow_market_data::MarketTrade,
) -> bool {
    let generation = chart.generation();
    match chart.apply_trade(generation, trade) {
        Ok(publication) => {
            let _ = messages.send(MarketWorkerMessage::RithmicLive {
                selection_generation: publication.generation.selection,
                series_generation: publication.generation.series,
                snapshot: publication.snapshot,
            });
            true
        }
        Err(
            crate::rithmic_live_chart::RithmicLiveChartError::OutOfOrderTrade
            | crate::rithmic_live_chart::RithmicLiveChartError::StaleGeneration,
        ) => true,
        Err(_) => {
            let _ = messages.send(MarketWorkerMessage::State {
                state: axiusflow_coinbase_coordinator::market_worker::ChartState::Recovering,
                message: "Rithmic live candles require a covering history snapshot".to_string(),
            });
            false
        }
    }
}

fn open_worker(
    history_root: PathBuf,
    ui_thread: ThreadId,
    detailed_diagnostics: bool,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<(RithmicWorker, RithmicEvents), String> {
    std::fs::create_dir_all(&history_root)
        .map_err(|_| "Rithmic history directory is unavailable".to_string())?;
    let credential_vault = NativeCredentialVault::new(RITHMIC_TEST_VAULT_SERVICE)
        .map_err(|_| "native credential vault unavailable".to_string())?;
    let key_vault = NativeCredentialVault::new(RITHMIC_TEST_VAULT_SERVICE)
        .map_err(|_| "native key vault unavailable".to_string())?;
    let catalog_key = load_catalog_key(&key_vault)?;
    let provider = RithmicProviderConfig::try_new(
        "Axiusflow",
        env!("CARGO_PKG_VERSION"),
        RithmicSessionLimits::default(),
        MESSAGE_SILENCE,
        Vec::new(),
    )
    .map_err(|_| "Rithmic provider configuration is invalid".to_string())?;
    let callback_limits = RithmicCallbackLimits::try_new(
        nonzero(CALLBACK_CAPACITY),
        nonzero(CALLBACK_BYTES),
        nonzero(MAXIMUM_DEPTH),
    )
    .map_err(|_| "Rithmic callback limits are invalid".to_string())?;
    let (driver, events) = RithmicProviderDriver::new_with_wake(provider, callback_limits, wake);
    let provider_config =
        DesktopProviderConfig::new(nonzero(32), nonzero(MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES))
            .with_diagnostics(
                RithmicProviderConfig::environment(),
                detailed_diagnostics
                    .then_some(NonZeroU64::new(10_000_000_000).unwrap_or(NonZeroU64::MIN)),
            )
            .map_err(|_| "Rithmic diagnostics configuration is invalid".to_string())?;
    let worker = DesktopMarketWorker::try_open(
        credential_vault,
        driver,
        RITHMIC_TEST_VAULT_KEY,
        history_root,
        catalog_key,
        ui_thread,
        DesktopMarketWorkerConfig {
            provider: provider_config,
            history: HistoryWorkerConfig {
                maximum_cache_entries: nonzero(2),
                maximum_decoded_bytes: nonzero(2 * 1024 * 1024),
                maximum_charts: nonzero(1),
                maximum_segment_read_bytes: nonzero(1024 * 1024),
                maximum_buffered_live: nonzero(512),
                maximum_handoffs: nonzero(1),
            },
            maximum_catalog_entries: 64,
        },
    )
    .map_err(|_| "Rithmic desktop runtime is unavailable".to_string())?;
    Ok((worker, events))
}

fn publish_event(
    messages: &axiusflow_coinbase_coordinator::market_worker::MarketWorkerSender,
    event: &AppliedRithmicEvent,
    selection_installed: &mut bool,
) {
    if matches!(
        event,
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::InstrumentsDiscovered {
            instruments,
            ..
        }) if !instruments.is_empty()
    ) {
        *selection_installed = true;
    }
    let (state, message) = reduce_event(event, *selection_installed);
    send_connection(messages, state, message);
}

fn reduce_event(
    event: &AppliedRithmicEvent,
    selection_installed: bool,
) -> (FeedConnectionState, &'static str) {
    match event {
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::DiscoveryStarted) => (
            FeedConnectionState::Discovering,
            "discovering Rithmic Test systems",
        ),
        AppliedRithmicEvent::Semantic(
            ProviderSessionEvent::SystemsDiscovered { .. }
            | ProviderSessionEvent::AuthenticationChanged {
                state: AuthenticationState::Required | AuthenticationState::Accepted,
                ..
            },
        ) => (
            FeedConnectionState::Authenticating,
            "authenticating the Rithmic Test session",
        ),
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::AuthenticationChanged {
            state: AuthenticationState::Rejected,
            ..
        }) => (
            FeedConnectionState::Stopped,
            "Rithmic Test authentication was rejected",
        ),
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::AuthenticationChanged {
            state: AuthenticationState::AgreementRequired,
            ..
        }) => (
            FeedConnectionState::Stopped,
            "Rithmic Test agreements require attention",
        ),
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::InstrumentsDiscovered {
            instruments,
            ..
        }) if instruments.is_empty() => (
            FeedConnectionState::Authenticating,
            "Rithmic Test session is ready for instrument search",
        ),
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::InstrumentsDiscovered { .. }) => (
            FeedConnectionState::Streaming,
            "Rithmic Test instrument selection is installed",
        ),
        AppliedRithmicEvent::RetryScheduled(ticket) => retry_presentation(ticket.reason),
        AppliedRithmicEvent::TerminalFailure { reason, .. } => terminal_failure(*reason),
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::Stopped) => {
            (FeedConnectionState::Stopped, "Rithmic Test session stopped")
        }
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::Market { .. }) => (
            FeedConnectionState::Streaming,
            "Rithmic Test feed is streaming",
        ),
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::Heartbeat { .. })
            if selection_installed =>
        {
            (
                FeedConnectionState::Streaming,
                "Rithmic Test selected feed is active",
            )
        }
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::Heartbeat { .. }) => (
            FeedConnectionState::Authenticating,
            "Rithmic Test session is ready for instrument search",
        ),
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::Invalidated { .. }) => (
            FeedConnectionState::Recovering,
            "Rithmic Test session recovery is required",
        ),
    }
}

fn active_generation(
    worker: &RithmicWorker,
) -> Result<Option<axiusflow_desktop_provider_runtime::SessionGeneration>, ()> {
    match worker.provider_state().map_err(|_| ())? {
        axiusflow_desktop_provider_runtime::DesktopProviderState::Connecting {
            generation, ..
        }
        | axiusflow_desktop_provider_runtime::DesktopProviderState::Streaming { generation } => {
            Ok(Some(generation))
        }
        _ => Ok(None),
    }
}

const fn catalog_connection_state(selection_installed: bool) -> FeedConnectionState {
    if selection_installed {
        FeedConnectionState::Streaming
    } else {
        FeedConnectionState::Authenticating
    }
}

fn dispatch_catalog_command(
    worker: &RithmicWorker,
    events: &RithmicEvents,
    dispatch: impl FnOnce(
        &RithmicEvents,
        axiusflow_desktop_provider_runtime::SessionGeneration,
    )
        -> Result<(), axiusflow_rithmic_protocol_adapter::RithmicProviderCommandError>,
) -> Result<(), ()> {
    let generation = active_generation(worker)?.ok_or(())?;
    dispatch(events, generation).map_err(|_| ())
}

fn terminal_failure(reason: ProviderInvalidationReason) -> (FeedConnectionState, &'static str) {
    match reason {
        ProviderInvalidationReason::Authentication => (
            FeedConnectionState::Stopped,
            "Rithmic Test authentication was rejected",
        ),
        ProviderInvalidationReason::AgreementRequired => (
            FeedConnectionState::Stopped,
            "Rithmic Test agreements require attention",
        ),
        ProviderInvalidationReason::UnsupportedSystem => (
            FeedConnectionState::Stopped,
            "Rithmic Test was not offered by system discovery",
        ),
        ProviderInvalidationReason::SchemaMismatch => (
            FeedConnectionState::Stopped,
            "Rithmic rejected the protocol template version",
        ),
        ProviderInvalidationReason::MalformedMessage => (
            FeedConnectionState::Stopped,
            "Rithmic returned an unexpected protocol response",
        ),
        ProviderInvalidationReason::Transport => (
            FeedConnectionState::Stopped,
            "Rithmic Test transport stopped",
        ),
        ProviderInvalidationReason::HeartbeatSilence
        | ProviderInvalidationReason::MessageSilence
        | ProviderInvalidationReason::SequenceGap
        | ProviderInvalidationReason::QueueOverflow => (
            FeedConnectionState::Stopped,
            "Rithmic Test session cannot continue",
        ),
    }
}

fn retry_presentation(reason: ProviderInvalidationReason) -> (FeedConnectionState, &'static str) {
    let message = match reason {
        ProviderInvalidationReason::HeartbeatSilence => {
            "Rithmic heartbeat response timed out; reconnecting"
        }
        ProviderInvalidationReason::MessageSilence => {
            "Rithmic market data became silent; reconnecting"
        }
        ProviderInvalidationReason::SequenceGap => {
            "Rithmic market data sequence gap detected; reconnecting"
        }
        ProviderInvalidationReason::QueueOverflow => {
            "Rithmic market data queue overflowed; reconnecting"
        }
        ProviderInvalidationReason::Transport
        | ProviderInvalidationReason::Authentication
        | ProviderInvalidationReason::AgreementRequired
        | ProviderInvalidationReason::UnsupportedSystem
        | ProviderInvalidationReason::SchemaMismatch
        | ProviderInvalidationReason::MalformedMessage => "Rithmic Test session will retry",
    };
    (FeedConnectionState::Recovering, message)
}

fn send_connection(
    messages: &axiusflow_coinbase_coordinator::market_worker::MarketWorkerSender,
    state: FeedConnectionState,
    message: &str,
) {
    let _ = messages.send(MarketWorkerMessage::Connection {
        state,
        message: message.to_string(),
    });
}

fn wait_for_shutdown(commands: &Receiver<MarketWorkerCommand>) {
    while let Ok(command) = commands.recv() {
        if matches!(command, MarketWorkerCommand::Shutdown) {
            break;
        }
    }
}

fn load_catalog_key(vault: &NativeCredentialVault) -> Result<CatalogKey, String> {
    let bytes = load_or_create_key(vault, CATALOG_KEY_ID)?;
    CatalogKey::try_new(CATALOG_KEY_ID.to_string(), bytes)
        .map_err(|_| "Rithmic catalog key is invalid".to_string())
}

fn load_or_create_key(vault: &NativeCredentialVault, key_id: &str) -> Result<[u8; 32], String> {
    if let Some(mut stored) = vault
        .load(key_id)
        .map_err(|_| "native key vault is unavailable".to_string())?
    {
        let result = <[u8; 32]>::try_from(stored.as_slice())
            .map_err(|_| "native catalog key has an invalid length".to_string());
        stored.zeroize();
        return result;
    }
    let mut generated = Zeroizing::new([0_u8; 32]);
    getrandom::fill(generated.as_mut()).map_err(|_| "catalog key generation failed".to_string())?;
    vault
        .store(key_id, generated.as_ref())
        .map_err(|_| "native key vault is unavailable".to_string())?;
    Ok(*generated)
}

fn nonzero(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap_or(NonZeroUsize::MIN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_desktop_provider_runtime::{ProviderEnvironment, SessionGeneration};
    use axiusflow_market_data::{
        AggressorSide, DepthLevel, DepthSnapshot, EventMetadata, MarketTrade, QualifiedTimestamp,
    };

    fn generation() -> SessionGeneration {
        SessionGeneration::new(NonZeroU64::MIN)
    }

    #[test]
    fn local_catalog_dispatch_failures_are_unfenced_and_domain_specific() {
        let generation = nonzero(7);
        let search = catalog_dispatch_rejection(generation, CatalogDispatchDomain::Search);
        let selection = catalog_dispatch_rejection(generation, CatalogDispatchDomain::Selection);
        assert!(matches!(
            search,
            RithmicCatalogEvent::CommandRejected {
                session_generation: None,
                command_generation,
                reason: axiusflow_rithmic_protocol_adapter::RithmicCatalogRejection::SearchDispatchUnavailable,
            } if command_generation == generation
        ));
        assert!(matches!(
            selection,
            RithmicCatalogEvent::CommandRejected {
                session_generation: None,
                command_generation,
                reason: axiusflow_rithmic_protocol_adapter::RithmicCatalogRejection::SelectionDispatchUnavailable,
            } if command_generation == generation
        ));
    }

    #[test]
    fn initial_offline_state_is_explicit_and_not_discovering() {
        let (state, message) = initial_offline_status();
        assert_eq!(state, FeedConnectionState::Disconnected);
        assert!(message.contains("offline"));
        assert!(!message.contains("discover"));
    }

    #[test]
    fn environment_reset_fences_stale_history_results_before_publication() {
        let (messages, receiver) = market_worker_channel(nonzero(4));
        let mut state = RithmicRuntimeState::new();
        state.pending_live_request = Some(RithmicSeriesRequest {
            selection_generation: nonzero(2),
            series_generation: nonzero(3),
            series: crate::rithmic_history::RithmicSeries::Minute1,
        });
        reset_live_state(&mut state);
        assert!(!apply_history_result(
            &messages,
            &mut state,
            RithmicHistoryResult {
                selection_generation: nonzero(2),
                series_generation: nonzero(3),
                result: Err("stale environment result".to_string()),
            },
        ));
        assert!(receiver.drain().0.is_empty());
    }

    #[test]
    fn reducer_exposes_only_coarse_lifecycle_states() {
        let cases = [
            (
                AppliedRithmicEvent::Semantic(ProviderSessionEvent::DiscoveryStarted),
                FeedConnectionState::Discovering,
            ),
            (
                AppliedRithmicEvent::Semantic(ProviderSessionEvent::SystemsDiscovered {
                    environments: vec![ProviderEnvironment {
                        provider_id: "rithmic".to_string(),
                        system_id: "RITHMIC_TEST".to_string(),
                        environment: "Test".to_string(),
                    }],
                }),
                FeedConnectionState::Authenticating,
            ),
            (
                AppliedRithmicEvent::Semantic(ProviderSessionEvent::InstrumentsDiscovered {
                    generation: generation(),
                    instruments: Vec::new(),
                }),
                FeedConnectionState::Authenticating,
            ),
            (
                AppliedRithmicEvent::Semantic(ProviderSessionEvent::Stopped),
                FeedConnectionState::Stopped,
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(reduce_event(&event, false).0, expected);
        }
    }

    #[test]
    fn preselection_heartbeat_never_claims_live_market_data() {
        let heartbeat = AppliedRithmicEvent::Semantic(ProviderSessionEvent::Heartbeat {
            generation: generation(),
            received_unix_nanos: 1,
        });
        let (state, message) = reduce_event(&heartbeat, false);
        assert_eq!(state, FeedConnectionState::Authenticating);
        assert!(!message.contains("streaming"));
        assert!(!message.contains("live"));

        assert_eq!(
            reduce_event(&heartbeat, true).0,
            FeedConnectionState::Streaming
        );
    }

    #[test]
    fn delayed_selection_uses_one_entitlement_for_history_and_depth() {
        let entitlement_id = "rithmic-test:CME-Delayed:MNQU6";
        let instrument = axiusflow_desktop_provider_runtime::InstrumentDescriptor {
            instrument_id: "rithmic:CME:MNQU6".to_string(),
            provider_symbol: "MNQU6".to_string(),
            display_symbol: "MNQU6".to_string(),
            venue_id: "CME".to_string(),
            price_scale: 2,
            quantity_scale: 0,
        };
        let mut state = RithmicRuntimeState::new();
        install_catalog_selection(
            &mut state,
            generation(),
            nonzero(7),
            &instrument,
            entitlement_id,
        );

        let installed = state
            .installed_instrument
            .as_ref()
            .expect("history identity is installed");
        assert_eq!(installed.descriptor.venue_id, "CME");
        assert_eq!(installed.entitlement_id, entitlement_id);
        assert_eq!(
            state
                .dom
                .selection()
                .map(|selection| selection.entitlement_id.as_str()),
            Some(entitlement_id)
        );
        let outcome = state
            .dom
            .apply_event(&MarketEvent::DepthSnapshot(DepthSnapshot {
                metadata: EventMetadata {
                    provider_id: "rithmic".to_string(),
                    instrument_id: instrument.instrument_id,
                    entitlement_id: entitlement_id.to_string(),
                    source_sequence: 1,
                    session_generation: generation().get(),
                    timestamps: QualifiedTimestamp {
                        exchange_unix_nanos: None,
                        provider_unix_nanos: None,
                        received_unix_nanos: 1,
                    },
                },
                bids: vec![DepthLevel {
                    price: 2_000_000,
                    quantity: 2,
                    order_count: Some(1),
                }],
                asks: vec![DepthLevel {
                    price: 2_000_025,
                    quantity: 3,
                    order_count: Some(1),
                }],
            }))
            .expect("matching delayed-entitlement depth validates");
        assert!(matches!(outcome, DomUpdateOutcome::Published(_)));
    }

    #[test]
    fn terminal_provider_failures_never_render_provider_text() {
        for reason in [
            ProviderInvalidationReason::Authentication,
            ProviderInvalidationReason::AgreementRequired,
            ProviderInvalidationReason::UnsupportedSystem,
        ] {
            let (state, message) = reduce_event(
                &AppliedRithmicEvent::TerminalFailure {
                    generation: generation(),
                    reason,
                },
                false,
            );
            assert_eq!(state, FeedConnectionState::Stopped);
            assert!(!message.contains("account"));
            assert!(!message.contains("user"));
            assert!(!message.contains("password"));
        }
    }

    #[test]
    fn transient_silence_reasons_remain_visible_at_the_shipping_boundary() {
        let (heartbeat_state, heartbeat_message) =
            retry_presentation(ProviderInvalidationReason::HeartbeatSilence);
        assert_eq!(heartbeat_state, FeedConnectionState::Recovering);
        assert!(heartbeat_message.contains("heartbeat"));

        let (message_state, message) =
            retry_presentation(ProviderInvalidationReason::MessageSilence);
        assert_eq!(message_state, FeedConnectionState::Recovering);
        assert!(message.contains("silent"));
    }

    #[test]
    fn shell_returns_before_background_completion_and_drop_acknowledges_shutdown() {
        let shell = RithmicShellState::local().expect("fixed shell profile validates");
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (stopped_tx, stopped_rx) = mpsc::sync_channel(1);
        let (startup, worker) = spawn_worker(shell, move |_messages, commands| {
            let _ = started_tx.send(());
            wait_for_shutdown(&commands);
            let _ = stopped_tx.send(());
        })
        .expect("bounded worker thread starts");

        assert!(matches!(startup, MarketWorkerStartup::Shell(_)));
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("background task starts independently");
        drop(worker);
        stopped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("drop requests and acknowledges shutdown");
    }

    #[test]
    fn native_lifecycle_startup_requires_both_monitors() {
        assert_eq!(
            require_environment_monitors::<(), ()>(Err(EnvironmentMonitorFailure::Network), Ok(())),
            Err(EnvironmentMonitorFailure::Network)
        );
        assert_eq!(
            require_environment_monitors::<(), ()>(
                Ok(((), NetworkEvent::Available)),
                Err(EnvironmentMonitorFailure::Power)
            ),
            Err(EnvironmentMonitorFailure::Power)
        );
        assert_eq!(
            require_environment_monitors(Ok(((), NetworkEvent::Unavailable)), Ok(())),
            Ok(((), (), NetworkEvent::Unavailable))
        );
    }

    #[test]
    fn monitor_stream_failure_is_latched_even_when_capture_inbox_is_full() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let source_ordinal = AtomicU64::new(0);
        let overflow = AtomicU64::new(0);
        let failures = AtomicU8::new(0);
        let mut events = VecDeque::from([
            Ok(NetworkEvent::Unavailable),
            Ok(NetworkEvent::Available),
            Err(()),
        ]);
        forward_environment_events(
            || events.pop_front().unwrap_or(Err(())),
            RithmicEnvironmentEvent::Network,
            &EnvironmentForwarding {
                sender: &sender,
                source_ordinal: &source_ordinal,
                overflow: &overflow,
                fail_capture_on_overflow: true,
                monitor_failures: &failures,
                failure_flag: NETWORK_MONITOR_FAILED,
            },
        );

        let retained = receiver.try_recv().expect("first event remains bounded");
        assert_eq!(retained.source_ordinal, 1);
        assert_eq!(
            retained.event,
            RithmicEnvironmentEvent::Network(NetworkEvent::Unavailable)
        );
        assert_eq!(source_ordinal.load(Ordering::Acquire), 2);
        assert_eq!(overflow.load(Ordering::Acquire), 1);
        assert_eq!(failures.load(Ordering::Acquire), NETWORK_MONITOR_FAILED);
    }

    #[test]
    fn history_handoff_trade_buffer_fails_closed_at_its_exact_bound() {
        let trade = MarketTrade {
            metadata: EventMetadata {
                provider_id: "rithmic".to_string(),
                instrument_id: "mnq".to_string(),
                entitlement_id: "test".to_string(),
                source_sequence: 1,
                session_generation: 1,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(1),
                    provider_unix_nanos: Some(1),
                    received_unix_nanos: 1,
                },
            },
            trade_id: "trade".to_string(),
            price: 1,
            quantity: 1,
            aggressor: AggressorSide::Unknown,
        };
        let mut buffer = VecDeque::new();
        let mut overflowed = false;
        for _ in 0..MAXIMUM_BUFFERED_HISTORY_TRADES {
            buffer_history_trade(&mut buffer, &mut overflowed, &trade);
        }
        assert_eq!(buffer.len(), MAXIMUM_BUFFERED_HISTORY_TRADES);
        assert!(!overflowed);

        buffer_history_trade(&mut buffer, &mut overflowed, &trade);
        assert!(buffer.is_empty());
        assert!(overflowed);
    }
}
