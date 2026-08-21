//! Axiusflow's native GPUI terminal entry point.

mod assets;
mod chart_chrome;
mod engine_market_worker;
mod engine_supervisor;
mod frame_poll_gate;
mod native_ui;
#[cfg(any(test, feature = "diagnostics"))]
mod readiness_conformance;
mod rithmic_engine_client;
mod rithmic_engine_history;
mod rithmic_history;
mod rithmic_shell;
#[cfg(feature = "diagnostics")]
mod windowed_benchmark;

use assets::UiIcon as HugeIcon;
#[cfg(feature = "diagnostics")]
use axiusflow_application::ReplayStreamUpdate;
use axiusflow_chart_integration::{
    ChartBridgeMetrics, ChartDrawingTool, ChartIndicator, ChartSplitDirection,
    ChartWorkspaceLayout, NucleusChartTheme, NucleusChartView, NucleusWorkspace,
};
use axiusflow_design_system::{AxiusflowTheme, RadiusToken, ThemeColor, ThemeMode};
use axiusflow_desktop::market_worker::{
    ChartState, EngineSeriesRequest, MarketDataWorker, MarketPublicationGeneration,
    MarketWorkerBootstrap, MarketWorkerMessage, MarketWorkerPublication, MarketWorkerRetirement,
    MarketWorkerStartup, PendingUiDiagnostics, ProviderCatalogCommand, ProviderCatalogEvent,
    UiDiagnosticsFeedback,
};
use axiusflow_engine_protocol::{
    ConsumerResourceClass, EngineLifetimeMode, InstallProviderInstrument,
    ProviderCatalogRejectionReason, ProviderInstrumentSummary, ResourceMode,
    SearchProviderInstruments, SelectProviderInstrument, SeriesCadence, SeriesKey,
    WorkspaceLayoutState, WorkspacePaneKind, WorkspacePaneState, WorkspaceSplitAxis,
    WorkspaceState, WorkspaceTabState,
};
use axiusflow_market_data::{ChartAggregation, ChartInterval};
use axiusflow_observability::FeedConnectionState;
use axiusflow_terminal_ui::{DomFrame, ReadOnlyDomView};
use gpui::{
    Animation, AnimationExt, AnyElement, App, Bounds, Context, Div, Entity, FocusHandle, Hsla,
    KeyBinding, KeyDownEvent, MouseButton, Orientation, Pixels, QuitMode, Render, Role,
    ScrollHandle, Stateful, Task, TitlebarOptions, WeakEntity, Window, WindowBounds,
    WindowControlArea, WindowOptions, actions, canvas, div, ease_out_quint, point, prelude::*, px,
    relative, rgb, size,
};
use gpui_platform::application;
use native_ui::{
    control::Button,
    icon::Icon,
    input::{Input, InputEvent, InputState},
    loader::Loader,
    scroll::{ThinScrollbar, tracked_overflow_y_scrollbar},
    tooltip::{TooltipSpec, with_tooltip},
};
use num_traits::ToPrimitive;
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    pin::Pin,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    task::{Context as TaskContext, Poll, Waker},
    time::Duration,
};

#[cfg(feature = "diagnostics")]
use std::time::Instant;

#[derive(Default)]
struct UiWakeState {
    pending: AtomicBool,
    waker: std::sync::Mutex<Option<Waker>>,
}

#[derive(Clone, Default)]
struct UiWake {
    state: Arc<UiWakeState>,
}

impl UiWake {
    fn callback(&self) -> Arc<dyn Fn() + Send + Sync> {
        let state = Arc::clone(&self.state);
        Arc::new(move || {
            state.pending.store(true, Ordering::Release);
            let waker = state
                .waker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(waker) = waker {
                waker.wake();
            }
        })
    }

    fn notified(&self) -> UiWakeNotified {
        UiWakeNotified {
            state: Arc::clone(&self.state),
        }
    }
}

struct UiWakeNotified {
    state: Arc<UiWakeState>,
}

impl std::future::Future for UiWakeNotified {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<()> {
        if self.state.pending.swap(false, Ordering::AcqRel) {
            return Poll::Ready(());
        }
        *self
            .state
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cx.waker().clone());
        if self.state.pending.swap(false, Ordering::AcqRel) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

const SIDE_PANEL_INITIAL_WIDTH: f32 = 320.0;
const SIDE_PANEL_MINIMUM_WIDTH: f32 = 240.0;
const SIDE_PANEL_MAXIMUM_WIDTH: f32 = 640.0;
const SIDE_PANEL_RESIZE_HANDLE_WIDTH: f32 = 5.0;
const MAXIMUM_STATUS_CHARACTERS: usize = 160;
const MAXIMUM_OPEN_WORKSPACES: usize = 8;
const MAXIMUM_PANES_PER_WORKSPACE: usize = 4;
const WORKSPACE_TITLE_BAR_HEIGHT: f32 = 42.0;
// Bound UI work when a provider delivers a burst of updates. Remaining mailbox
// messages stay queued and wake the next GPUI frame.
const MARKET_MESSAGES_PER_FRAME: usize = 8;
const WORKSPACE_TAB_WIDTH: f32 = 132.0;
const WORKSPACE_TAB_GAP: f32 = 2.0;
const WORKSPACE_TAB_STRIP_PADDING_LEFT: f32 = 8.0;
const TOOLTIP_OPEN_DELAY: Duration = Duration::from_millis(400);
const CHROME_OVERLAY_TRANSITION_DURATION: Duration = Duration::from_millis(140);
const CHROME_OVERLAY_TRANSITION_OFFSET: f32 = 5.0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum DesktopLifetimeMode {
    #[default]
    KeepEngineWarm,
    KeepMarketsLive,
    ExitWithDesktop,
}

impl DesktopLifetimeMode {
    const fn engine_resource_mode(self) -> ResourceMode {
        match self {
            Self::KeepMarketsLive => ResourceMode::MarketsLive,
            Self::KeepEngineWarm | Self::ExitWithDesktop => ResourceMode::Warm,
        }
    }

    const fn protocol_mode(self) -> EngineLifetimeMode {
        match self {
            Self::ExitWithDesktop => EngineLifetimeMode::ExitCompletely,
            Self::KeepEngineWarm => EngineLifetimeMode::KeepEngineWarm,
            Self::KeepMarketsLive => EngineLifetimeMode::KeepMarketsLive,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::ExitWithDesktop => "Exit fully",
            Self::KeepEngineWarm => "Engine warm",
            Self::KeepMarketsLive => "Markets live",
        }
    }

    const fn next(self, markets_live_permitted: bool) -> Self {
        match (self, markets_live_permitted) {
            (Self::ExitWithDesktop, _) => Self::KeepEngineWarm,
            (Self::KeepEngineWarm, true) => Self::KeepMarketsLive,
            (Self::KeepEngineWarm, false) | (Self::KeepMarketsLive, _) => Self::ExitWithDesktop,
        }
    }

    fn from_workspace(workspace: &WorkspaceState) -> Result<Self, String> {
        match EngineLifetimeMode::try_from(workspace.lifetime_mode)
            .map_err(|_| "resident engine returned an invalid lifetime mode".to_string())?
        {
            EngineLifetimeMode::ExitCompletely => Ok(Self::ExitWithDesktop),
            EngineLifetimeMode::KeepEngineWarm => Ok(Self::KeepEngineWarm),
            EngineLifetimeMode::KeepMarketsLive if workspace.markets_live_permitted => {
                Ok(Self::KeepMarketsLive)
            }
            EngineLifetimeMode::KeepMarketsLive => {
                Err("resident engine markets-live mode lacks explicit permission".to_string())
            }
        }
    }
}

struct LifecyclePreferenceRequest {
    mode: DesktopLifetimeMode,
    autostart_enabled: bool,
    markets_live_permitted: bool,
}

type LifecyclePreferenceResult = Result<WorkspaceState, String>;

#[derive(Clone, Copy)]
struct LifecyclePresentation {
    mode: DesktopLifetimeMode,
    autostart_enabled: bool,
    markets_live_permitted: bool,
    pending: bool,
}

fn finish_desktop_shutdown(
    mode: DesktopLifetimeMode,
    detach_failed: bool,
    shutdown_engine: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let shutdown_result = if mode == DesktopLifetimeMode::ExitWithDesktop {
        shutdown_engine()
    } else {
        Ok(())
    };
    match (detach_failed, shutdown_result) {
        (false, Ok(())) => Ok(()),
        (true, Ok(())) => {
            Err("desktop market worker did not detach before its deadline".to_string())
        }
        (false, Err(error)) => Err(error),
        (true, Err(error)) => Err(format!(
            "desktop market worker detach expired; engine shutdown failed: {error}"
        )),
    }
}

#[derive(Clone)]
struct DesktopLifecycle {
    mode: Rc<Cell<DesktopLifetimeMode>>,
    autostart_enabled: Rc<Cell<bool>>,
    markets_live_permitted: Rc<Cell<bool>>,
    preference_pending: Rc<Cell<bool>>,
    preference_error: Rc<RefCell<Option<String>>>,
    preference_requests: SyncSender<LifecyclePreferenceRequest>,
    preference_results: Rc<RefCell<Receiver<LifecyclePreferenceResult>>>,
    retirements: Rc<RefCell<Vec<Task<bool>>>>,
    terminals: Rc<RefCell<Vec<WeakEntity<WorkspaceSurface>>>>,
    shutdown_started: Rc<Cell<bool>>,
}

impl DesktopLifecycle {
    fn new(
        mode: DesktopLifetimeMode,
        autostart_enabled: bool,
        markets_live_permitted: bool,
    ) -> Result<Self, String> {
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("axiusflow-engine-lifecycle-client".to_string())
            .spawn(move || run_lifecycle_preferences(&request_rx, &result_tx))
            .map_err(|_| "desktop lifecycle client could not start".to_string())?;
        Ok(Self {
            mode: Rc::new(Cell::new(mode)),
            autostart_enabled: Rc::new(Cell::new(autostart_enabled)),
            markets_live_permitted: Rc::new(Cell::new(markets_live_permitted)),
            preference_pending: Rc::new(Cell::new(false)),
            preference_error: Rc::new(RefCell::new(None)),
            preference_requests: request_tx,
            preference_results: Rc::new(RefCell::new(result_rx)),
            retirements: Rc::new(RefCell::new(Vec::new())),
            terminals: Rc::new(RefCell::new(Vec::new())),
            shutdown_started: Rc::new(Cell::new(false)),
        })
    }

    fn presentation(&self) -> LifecyclePresentation {
        LifecyclePresentation {
            mode: self.mode.get(),
            autostart_enabled: self.autostart_enabled.get(),
            markets_live_permitted: self.markets_live_permitted.get(),
            pending: self.preference_pending.get(),
        }
    }

    fn preference_error(&self) -> Option<String> {
        self.preference_error.borrow().clone()
    }

    fn request_preferences(&self, request: LifecyclePreferenceRequest) -> Result<(), String> {
        if self.preference_pending.get() {
            return Ok(());
        }
        self.preference_requests
            .try_send(request)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    "engine lifecycle update is already pending".to_string()
                }
                mpsc::TrySendError::Disconnected(_) => {
                    "engine lifecycle client is unavailable".to_string()
                }
            })?;
        self.preference_pending.set(true);
        self.preference_error.borrow_mut().take();
        Ok(())
    }

    fn poll_preferences(&self) -> bool {
        match self.preference_results.borrow().try_recv() {
            Ok(Ok(workspace)) => {
                match DesktopLifetimeMode::from_workspace(&workspace) {
                    Ok(mode) => {
                        self.mode.set(mode);
                        self.autostart_enabled.set(workspace.autostart_enabled);
                        self.markets_live_permitted
                            .set(workspace.markets_live_permitted);
                        self.preference_error.borrow_mut().take();
                    }
                    Err(error) => *self.preference_error.borrow_mut() = Some(error),
                }
                self.preference_pending.set(false);
                true
            }
            Ok(Err(error)) => {
                *self.preference_error.borrow_mut() = Some(error);
                self.preference_pending.set(false);
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                if self.preference_pending.replace(false) {
                    *self.preference_error.borrow_mut() =
                        Some("engine lifecycle client stopped unexpectedly".to_string());
                    return true;
                }
                false
            }
        }
    }

    fn register_terminal(&self, terminal: &Entity<WorkspaceSurface>) {
        self.terminals.borrow_mut().push(terminal.downgrade());
    }

    fn retire_market_worker(&self, retirement: MarketWorkerRetirement, cx: &App) {
        self.retirements.borrow_mut().push(
            cx.background_executor()
                .spawn(async move { retirement.wait() }),
        );
    }

    fn begin_quit(&self, cx: &mut App) -> Option<Task<Result<(), String>>> {
        if self.shutdown_started.replace(true) {
            return None;
        }
        let terminals = self.terminals.borrow_mut().drain(..).collect::<Vec<_>>();
        for terminal in terminals {
            terminal
                .update(cx, |terminal, terminal_cx| {
                    terminal.retire_market_worker(terminal_cx);
                })
                .ok();
        }
        let retirements = self.retirements.borrow_mut().drain(..).collect::<Vec<_>>();
        let mode = self.mode.get();
        Some(cx.background_executor().spawn(async move {
            let mut detach_failed = false;
            for retirement in retirements {
                if !retirement.await {
                    detach_failed = true;
                }
            }
            finish_desktop_shutdown(mode, detach_failed, || {
                axiusflow_local_engine_client::shutdown_running_engine()
            })
        }))
    }

    fn quit_after_shutdown(&self, cx: &mut App) {
        let Some(shutdown) = self.begin_quit(cx) else {
            cx.quit();
            return;
        };
        cx.spawn(async move |cx| {
            if let Err(error) = shutdown.await {
                eprintln!("Axiusflow desktop shutdown failed: {error}");
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
}

fn run_lifecycle_preferences(
    requests: &Receiver<LifecyclePreferenceRequest>,
    results: &SyncSender<LifecyclePreferenceResult>,
) {
    while let Ok(request) = requests.recv() {
        let result = axiusflow_local_engine_client::sibling_engine_executable()
            .and_then(|executable| {
                axiusflow_local_engine_client::connect_or_start_engine(&executable)
            })
            .and_then(|mut client| {
                let workspace = client.restore_workspace()?;
                client.set_engine_lifecycle(
                    workspace.workspace_revision,
                    request.mode.protocol_mode(),
                    request.autostart_enabled,
                    request.markets_live_permitted,
                )
            });
        if results.send(result).is_err() {
            return;
        }
    }
}

#[derive(Clone)]
struct WorkspaceLayoutRequest {
    layout_generation: u64,
    active_workspace_id: u64,
    workspace_tabs: Vec<WorkspaceTabState>,
}

#[derive(Clone)]
struct WorkspaceLayoutPersistence {
    latest: Arc<Mutex<Option<WorkspaceLayoutRequest>>>,
    wake: SyncSender<()>,
    result: Arc<Mutex<Option<Result<WorkspaceState, String>>>>,
    workspace_revision: Rc<Cell<u64>>,
    layout_generation: Rc<Cell<u64>>,
    pending: Rc<Cell<bool>>,
    error: Rc<RefCell<Option<String>>>,
}

impl WorkspaceLayoutPersistence {
    fn new(workspace_revision: u64, layout_generation: u64) -> Result<Self, String> {
        let latest = Arc::new(Mutex::new(None));
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let result = Arc::new(Mutex::new(None));
        let worker_latest = Arc::clone(&latest);
        let worker_result = Arc::clone(&result);
        std::thread::Builder::new()
            .name("axiusflow-workspace-layout-client".to_string())
            .spawn(move || {
                run_workspace_layout_persistence(&worker_latest, &wake_rx, &worker_result);
            })
            .map_err(|_| "workspace layout client could not start".to_string())?;
        Ok(Self {
            latest,
            wake: wake_tx,
            result,
            workspace_revision: Rc::new(Cell::new(workspace_revision)),
            layout_generation: Rc::new(Cell::new(layout_generation)),
            pending: Rc::new(Cell::new(false)),
            error: Rc::new(RefCell::new(None)),
        })
    }

    fn request(
        &self,
        active_workspace_id: u64,
        workspace_tabs: Vec<WorkspaceTabState>,
    ) -> Result<(), String> {
        let generation = self.layout_generation.get().saturating_add(1);
        *self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(WorkspaceLayoutRequest {
            layout_generation: generation,
            active_workspace_id,
            workspace_tabs,
        });
        match self.wake.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => {
                self.pending.set(true);
                self.error.borrow_mut().take();
                Ok(())
            }
            Err(mpsc::TrySendError::Disconnected(())) => {
                Err("workspace layout client is unavailable".to_string())
            }
        }
    }

    fn poll(&self) -> bool {
        let Some(result) = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return false;
        };
        match result {
            Ok(workspace) => {
                self.workspace_revision.set(workspace.workspace_revision);
                self.layout_generation.set(workspace.layout_generation);
                self.error.borrow_mut().take();
            }
            Err(error) => {
                *self.error.borrow_mut() = Some(error);
            }
        }
        self.pending.set(false);
        true
    }

    fn error(&self) -> Option<String> {
        self.error.borrow().clone()
    }
}

fn run_workspace_layout_persistence(
    latest: &Mutex<Option<WorkspaceLayoutRequest>>,
    wake: &Receiver<()>,
    result: &Mutex<Option<Result<WorkspaceState, String>>>,
) {
    while wake.recv().is_ok() {
        while wake.recv_timeout(Duration::from_millis(100)).is_ok() {}
        let Some(request) = latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            continue;
        };
        let completed = axiusflow_local_engine_client::sibling_engine_executable()
            .and_then(|executable| {
                axiusflow_local_engine_client::connect_or_start_engine(&executable)
            })
            .and_then(|mut client| {
                let current = client.restore_workspace()?;
                client.set_workspace_layout(
                    current.workspace_revision,
                    request
                        .layout_generation
                        .max(current.layout_generation.saturating_add(1)),
                    request.active_workspace_id,
                    request.workspace_tabs,
                )
            });
        *result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(completed);
    }
}

actions!(
    axiusflow,
    [
        MinimizeWindow,
        ZoomWindow,
        ToggleFullscreen,
        CloseWindow,
        NewWorkspace,
        SelectNextWorkspace,
        SelectPreviousWorkspace,
        MoveWorkspaceLeft,
        MoveWorkspaceRight,
        CloseWorkspace,
        SplitPaneHorizontal,
        SplitPaneVertical,
        ClosePane,
    ]
);

static COINBASE_INTERVALS: &[ChartInterval] = &[
    ChartInterval::Minute1,
    ChartInterval::Minute3,
    ChartInterval::Minute5,
    ChartInterval::Minute15,
    ChartInterval::Minute30,
    ChartInterval::Hour1,
    ChartInterval::Hour2,
    ChartInterval::Hour4,
    ChartInterval::Hour8,
    ChartInterval::Hour12,
    ChartInterval::Day1,
    ChartInterval::Week1,
    ChartInterval::Month1,
];
const COINBASE_ENTITLEMENT_ID: &str = "crypto_public_realtime";
const COINBASE_CALENDAR_HISTORY_STATUS: &str =
    "Completed Coinbase history; current calendar bucket is not live";

const fn coinbase_interval_is_history_only(interval: ChartInterval) -> bool {
    matches!(interval, ChartInterval::Week1 | ChartInterval::Month1)
}

fn generation_status(
    worker_label: &str,
    subscription_id: &str,
    generation: MarketPublicationGeneration,
) -> String {
    let (first_sequence, last_sequence) = generation.sequence_range();
    format!(
        "{worker_label} · {subscription_id} · model g{} · {} retained · seq {first_sequence}–{last_sequence}",
        generation.publication_generation(),
        generation.retained_items(),
    )
}

fn bridge_status(metrics: ChartBridgeMetrics) -> String {
    format!(
        "bridge q{} · overflows {} · recoveries {}/{}{}",
        metrics.queued_updates,
        metrics.queue_overflows,
        metrics.completed_recoveries,
        metrics.failed_recoveries,
        if metrics.recovery_pending {
            " · snapshot pending"
        } else {
            ""
        }
    )
}

fn publication_chart_state(accepted: bool, recovery_pending: bool) -> ChartState {
    if accepted && !recovery_pending {
        ChartState::Ready
    } else {
        ChartState::Recovering
    }
}

fn reconciled_bridge_state(current: ChartState, recovery_pending: bool) -> ChartState {
    if current == ChartState::Ready && recovery_pending {
        ChartState::Recovering
    } else {
        current
    }
}

fn default_rithmic_contract_index(results: &[ProviderInstrumentSummary]) -> Option<usize> {
    results
        .iter()
        .enumerate()
        .filter(|(_, result)| {
            result.symbol.starts_with("MNQ")
                && result.symbol != "MNQ"
                && !result.symbol.contains('-')
                && result.expiration_date.is_some()
        })
        .min_by_key(|(_, result)| result.expiration_date.as_deref())
        .map(|(index, _)| index)
}

fn reconnect_contract_index(
    results: &[ProviderInstrumentSummary],
    target: &RithmicReconnectTarget,
) -> Option<usize> {
    results
        .iter()
        .position(|result| result.symbol == target.symbol && result.exchange == target.exchange)
}

#[cfg(feature = "diagnostics")]
const FOREGROUND_INTERACTION_SAMPLE_CAPACITY: usize = 128;

#[cfg(feature = "diagnostics")]
#[derive(Default)]
struct ForegroundInteractionDiagnostics {
    symbol_input_change: Vec<u64>,
    symbol_input_submit: Vec<u64>,
    instrument_selection: Vec<u64>,
    interval_selection: Vec<u64>,
}

#[cfg(feature = "diagnostics")]
impl ForegroundInteractionDiagnostics {
    fn record(samples: &mut Vec<u64>, elapsed_nanos: u64) {
        if samples.len() < FOREGROUND_INTERACTION_SAMPLE_CAPACITY {
            samples.push(elapsed_nanos);
        }
    }

    fn record_symbol_input(&mut self, submitted: bool, elapsed_nanos: u64) {
        let samples = if submitted {
            &mut self.symbol_input_submit
        } else {
            &mut self.symbol_input_change
        };
        Self::record(samples, elapsed_nanos);
    }

    fn record_instrument_selection(&mut self, elapsed_nanos: u64) {
        Self::record(&mut self.instrument_selection, elapsed_nanos);
    }

    fn record_interval_selection(&mut self, elapsed_nanos: u64) {
        Self::record(&mut self.interval_selection, elapsed_nanos);
    }
}

#[cfg(feature = "diagnostics")]
fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

struct WorkspaceSurface {
    chart: Option<Entity<NucleusChartView>>,
    dom: Entity<ReadOnlyDomView>,
    side_panel: Option<SidePanel>,
    side_panel_width: f32,
    side_panel_resize: Option<SidePanelResize>,
    scrolls: WorkspaceScrollHandles,
    chart_state: ChartState,
    chart_state_message: String,
    theme: AxiusflowTheme,
    replay_label: String,
    worker_label: String,
    subscription_id: String,
    bridge_label: String,
    market_worker: MarketDataWorker,
    lifecycle: DesktopLifecycle,
    pending_ui_diagnostics: Option<PendingUiDiagnostics>,
    connection_state: Option<FeedConnectionState>,
    connection_message: Option<String>,
    symbol_browser: rithmic_shell::RithmicSymbolBrowser,
    symbol_message: String,
    symbol_selection_pending: bool,
    series_browser: rithmic_history::RithmicSeriesBrowser,
    series_message: String,
    rithmic_autoload_started: bool,
    rithmic_reconnect: RithmicReconnectState,
    symbol_input: Option<Entity<InputState>>,
    indicator_input: Entity<InputState>,
    indicator_message: Option<String>,
    chrome_overlay: Option<ChromeOverlay>,
    chrome_overlay_phase: ChromeOverlayPhase,
    chrome_overlay_generation: u64,
    timeframe_trigger_bounds: Option<Bounds<Pixels>>,
    chrome_selection: usize,
    chrome_focus: FocusHandle,
    provider: TerminalProvider,
    coinbase_product: Option<InstallProviderInstrument>,
    coinbase_switch: CoinbaseSwitchState,
    coinbase_interval: ChartInterval,
    coinbase_pending_interval: Option<ChartInterval>,
    coinbase_pending_product: Option<InstallProviderInstrument>,
    coinbase_pending_sequence: Option<u64>,
    restored_viewport: Option<(i64, i64)>,
    last_persisted_viewport: Option<(i64, i64)>,
    resource_class: ConsumerResourceClass,
    #[cfg(feature = "diagnostics")]
    foreground_interactions: ForegroundInteractionDiagnostics,
    #[cfg(feature = "diagnostics")]
    live_evidence_enabled: bool,
    #[cfg(feature = "diagnostics")]
    live_evidence_publications: u16,
}

#[derive(Default)]
struct WorkspaceScrollHandles {
    drawing: ScrollHandle,
    indicator: ScrollHandle,
    instrument: ScrollHandle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalProvider {
    Coinbase,
    Rithmic,
}

const fn terminal_provider_id(provider: TerminalProvider) -> &'static str {
    match provider {
        TerminalProvider::Coinbase => "coinbase",
        TerminalProvider::Rithmic => "rithmic",
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CoinbaseSwitchState {
    #[default]
    Idle,
    Pending,
}

impl CoinbaseSwitchState {
    const fn is_pending(self) -> bool {
        matches!(self, Self::Pending)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChromeOverlay {
    Instrument,
    Indicator,
    Timeframe,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ChromeOverlayPhase {
    #[default]
    Opening,
    Closing,
}

fn chrome_overlay_progress(phase: ChromeOverlayPhase, delta: f32) -> f32 {
    let delta = delta.clamp(0.0, 1.0);
    match phase {
        ChromeOverlayPhase::Opening => delta,
        ChromeOverlayPhase::Closing => 1.0 - delta,
    }
}

const fn should_finish_chrome_overlay_close(
    phase: ChromeOverlayPhase,
    current_generation: u64,
    closing_generation: u64,
) -> bool {
    matches!(phase, ChromeOverlayPhase::Closing) && current_generation == closing_generation
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RithmicReconnectTarget {
    symbol: String,
    exchange: String,
    series: rithmic_history::RithmicSeries,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum RithmicReconnectState {
    #[default]
    Idle,
    AwaitingSearch(RithmicReconnectTarget),
    SearchInFlight(RithmicReconnectTarget),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RithmicSessionRetirement {
    None,
    Offline,
    Recovering,
    Stopped,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum DrawingToolbarVisibility {
    #[default]
    Expanded,
    Collapsed,
}

impl DrawingToolbarVisibility {
    const fn is_collapsed(self) -> bool {
        matches!(self, Self::Collapsed)
    }

    fn toggle(&mut self) {
        *self = if self.is_collapsed() {
            Self::Expanded
        } else {
            Self::Collapsed
        };
    }
}

impl RithmicSessionRetirement {
    const fn from_connection(state: FeedConnectionState) -> Self {
        match state {
            FeedConnectionState::Disconnected => Self::Offline,
            FeedConnectionState::Recovering => Self::Recovering,
            FeedConnectionState::Stopped => Self::Stopped,
            FeedConnectionState::Discovering
            | FeedConnectionState::Authenticating
            | FeedConnectionState::Streaming => Self::None,
        }
    }

    const fn chart_state(self, has_market_data: bool) -> Option<ChartState> {
        match self {
            Self::Offline if has_market_data => Some(ChartState::Stale),
            Self::Recovering if has_market_data => Some(ChartState::Recovering),
            Self::Stopped => Some(ChartState::Error),
            Self::None | Self::Offline | Self::Recovering => None,
        }
    }
}

impl RithmicReconnectState {
    fn target(&self) -> Option<&RithmicReconnectTarget> {
        match self {
            Self::AwaitingSearch(target) | Self::SearchInFlight(target) => Some(target),
            Self::Idle => None,
        }
    }
}

fn observe_chart(chart: Option<&Entity<NucleusChartView>>, cx: &mut Context<WorkspaceSurface>) {
    if let Some(chart) = chart {
        cx.observe(chart, |app, chart, cx| {
            if app.provider == TerminalProvider::Coinbase
                && let Some(viewport) = chart.read(cx).visible_time_range_unix_nanos()
                && app.last_persisted_viewport != Some(viewport)
                && app
                    .market_worker
                    .try_set_chart_viewport(viewport.0, viewport.1)
                    .is_ok()
            {
                app.last_persisted_viewport = Some(viewport);
            }
        })
        .detach();
    }
}

#[derive(Clone, Copy)]
enum InstrumentMenuSelection {
    Rithmic(usize),
    Coinbase(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SymbolSubmitDecision {
    Select(usize),
    Search,
    None,
}

const fn symbol_submit_decision(
    provider: TerminalProvider,
    result_count: usize,
    highlighted: usize,
) -> SymbolSubmitDecision {
    if highlighted < result_count {
        return SymbolSubmitDecision::Select(highlighted);
    }
    match provider {
        TerminalProvider::Rithmic => SymbolSubmitDecision::Search,
        TerminalProvider::Coinbase => SymbolSubmitDecision::None,
    }
}

#[derive(Clone)]
struct InstrumentMenuEntry {
    symbol: String,
    checked: bool,
    selection: InstrumentMenuSelection,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RithmicReadyAction {
    None,
    Reconnect(String),
    Autoload,
}

fn rithmic_ready_action(
    state: FeedConnectionState,
    message: &str,
    reconnect: &RithmicReconnectState,
    autoload_started: bool,
) -> RithmicReadyAction {
    if state != FeedConnectionState::Authenticating
        || !message.contains("ready for instrument search")
    {
        return RithmicReadyAction::None;
    }
    match reconnect {
        RithmicReconnectState::AwaitingSearch(target) => {
            RithmicReadyAction::Reconnect(target.symbol.clone())
        }
        RithmicReconnectState::Idle if !autoload_started => RithmicReadyAction::Autoload,
        RithmicReconnectState::Idle | RithmicReconnectState::SearchInFlight(_) => {
            RithmicReadyAction::None
        }
    }
}

struct HeaderState {
    theme: AxiusflowTheme,
    pane_count: usize,
    provider: TerminalProvider,
    instrument_label: String,
    series_label: String,
    instruments: Vec<InstrumentMenuEntry>,
    selected_series: Option<rithmic_history::RithmicSeries>,
    symbol_input: Option<Entity<InputState>>,
    indicator_input: Entity<InputState>,
    indicator_message: Option<String>,
    series_message: String,
    pending: HeaderPendingState,
    controls: HeaderControls,
    dom_visible: bool,
    connection_state: FeedConnectionState,
    chart_state: ChartState,
    delayed: bool,
    history_only: bool,
    instrument_scroll: ScrollHandle,
}

#[derive(Clone, Copy, Default)]
struct HeaderPendingState {
    symbol_selection: bool,
    series: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SidePanel {
    Dom,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SidePanelResize {
    pointer_x: f32,
    width: f32,
}

fn resized_side_panel_width(resize: SidePanelResize, pointer_x: f32) -> f32 {
    (resize.width + resize.pointer_x - pointer_x)
        .clamp(SIDE_PANEL_MINIMUM_WIDTH, SIDE_PANEL_MAXIMUM_WIDTH)
}

fn claim_once(claimed: &mut bool) -> bool {
    if *claimed {
        return false;
    }
    *claimed = true;
    true
}

impl SidePanel {
    const fn title(self) -> &'static str {
        match self {
            Self::Dom => "Order book",
        }
    }

    const fn toggle_label(self) -> &'static str {
        match self {
            Self::Dom => "DOM",
        }
    }

    const fn toggle_tooltip(self) -> &'static str {
        match self {
            Self::Dom => "Toggle read-only depth panel",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChartNoticePlacement {
    Center,
    TopLeft,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChartNoticeTone {
    Muted,
    Warning,
    Loss,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChartSurfaceNotice {
    label: &'static str,
    detail: Option<String>,
    placement: ChartNoticePlacement,
    tone: ChartNoticeTone,
}

fn chart_surface_notice(
    state: ChartState,
    has_market_data: bool,
    detail: &str,
) -> Option<ChartSurfaceNotice> {
    let placement = if has_market_data {
        ChartNoticePlacement::TopLeft
    } else {
        ChartNoticePlacement::Center
    };
    let detail = bounded_status_detail(detail, state.label());
    match state {
        ChartState::Loading => Some(ChartSurfaceNotice {
            label: state.label(),
            detail,
            placement,
            tone: ChartNoticeTone::Muted,
        }),
        ChartState::Ready => None,
        ChartState::Stale | ChartState::Recovering => Some(ChartSurfaceNotice {
            label: state.label(),
            detail,
            placement,
            tone: ChartNoticeTone::Warning,
        }),
        ChartState::Error => Some(ChartSurfaceNotice {
            label: state.label(),
            detail,
            placement,
            tone: ChartNoticeTone::Loss,
        }),
    }
}

fn bounded_status_detail(detail: &str, generic_label: &str) -> Option<String> {
    let detail = detail.trim();
    if detail.is_empty() || detail.eq_ignore_ascii_case(generic_label) {
        return None;
    }
    let mut bounded: String = detail.chars().take(MAXIMUM_STATUS_CHARACTERS).collect();
    if detail.chars().count() > MAXIMUM_STATUS_CHARACTERS {
        bounded.push('…');
    }
    Some(bounded)
}

fn chart_status_detail<'a>(
    chart_state: ChartState,
    connection_state: FeedConnectionState,
    chart_message: &'a str,
    connection_message: Option<&'a str>,
) -> &'a str {
    if chart_state != ChartState::Ready && connection_state != FeedConnectionState::Streaming {
        connection_message.unwrap_or(chart_message)
    } else {
        chart_message
    }
}

fn instrument_selector_label(selected: Option<(&str, &str)>, selection_pending: bool) -> String {
    let _ = selection_pending;
    selected.map_or_else(
        || "Contract".to_string(),
        |(symbol, exchange)| format!("{symbol} / {exchange}"),
    )
}

fn terminal_instrument_label(app: &WorkspaceSurface) -> String {
    if app.provider == TerminalProvider::Coinbase {
        return app.coinbase_product.as_ref().map_or_else(
            || "Select market".to_string(),
            |product| product.display_symbol.clone(),
        );
    }
    instrument_selector_label(
        app.symbol_browser.selected().map(|selection| {
            (
                selection.instrument.symbol.as_str(),
                selection.instrument.exchange.as_str(),
            )
        }),
        app.symbol_selection_pending,
    )
}

fn series_selector_label(
    selected: Option<rithmic_history::RithmicSeries>,
    pending: Option<rithmic_history::RithmicSeries>,
) -> String {
    let _ = pending;
    selected
        .map_or("Series", rithmic_history::RithmicSeries::label)
        .to_string()
}

#[derive(Clone, Copy)]
struct HeaderControls(u8);

impl HeaderControls {
    const INSTRUMENT: u8 = 1;
    const SERIES: u8 = 2;
    const DOM: u8 = 4;
    const FIT: u8 = 8;
    const LATEST: u8 = 16;

    const fn enabled(self, control: u8) -> bool {
        self.0 & control != 0
    }

    fn from_state(instrument: bool, selection: bool) -> Self {
        let mut controls = 0;
        if instrument {
            controls |= Self::INSTRUMENT;
        }
        if selection {
            controls |= Self::SERIES | Self::DOM;
        }
        Self(controls)
    }

    const fn with_chart_controls(mut self, chart_ready: bool) -> Self {
        if chart_ready {
            self.0 |= Self::FIT | Self::LATEST;
        }
        self
    }
}

struct TerminalStartupState {
    chart: Option<Entity<NucleusChartView>>,
    chart_state: ChartState,
    chart_state_message: String,
    replay_label: String,
    worker_label: String,
    subscription_id: String,
    connection_state: Option<FeedConnectionState>,
    connection_message: Option<String>,
    provider: TerminalProvider,
    coinbase_product: Option<InstallProviderInstrument>,
}

fn terminal_startup_state(
    startup: MarketWorkerStartup,
    cx: &mut Context<WorkspaceSurface>,
) -> TerminalStartupState {
    match startup {
        MarketWorkerStartup::Rithmic => {
            let shell = rithmic_shell::RithmicShellState::local()
                .expect("fixed Rithmic Test profile validates");
            let profile = shell.profile_label();
            let connection = shell.connection();
            let message = shell.message().to_string();
            TerminalStartupState {
                chart: Some(cx.new(move |_| NucleusChartView::empty())),
                chart_state: ChartState::Loading,
                chart_state_message: message.clone(),
                replay_label: profile.clone(),
                worker_label: "Rithmic market worker".to_string(),
                subscription_id: "Loading chart".to_string(),
                connection_state: Some(connection),
                connection_message: Some(message),
                provider: TerminalProvider::Rithmic,
                coinbase_product: None,
            }
        }
        MarketWorkerStartup::Loading(startup) => {
            let history_only = coinbase_interval_is_history_only(startup.coinbase_interval);
            TerminalStartupState {
                chart: None,
                chart_state: ChartState::Loading,
                chart_state_message: "waiting for a covering market snapshot".to_string(),
                replay_label: "waiting for a covering market snapshot".to_string(),
                worker_label: startup.worker_label,
                subscription_id: startup.subscription_id,
                connection_state: Some(if history_only {
                    FeedConnectionState::Authenticating
                } else {
                    FeedConnectionState::Discovering
                }),
                connection_message: Some(if history_only {
                    "Loading Coinbase history-only interval".to_string()
                } else {
                    "Connecting to Coinbase public markets".to_string()
                }),
                provider: TerminalProvider::Coinbase,
                coinbase_product: Some(startup.coinbase_product),
            }
        }
    }
}

fn initial_symbol_message(provider: TerminalProvider) -> String {
    match provider {
        TerminalProvider::Coinbase => "Loading Coinbase public spot catalog",
        TerminalProvider::Rithmic => "Search for an entitled Rithmic Test symbol",
    }
    .to_string()
}

impl WorkspaceSurface {
    fn new(
        cx: &mut Context<Self>,
        startup: MarketWorkerStartup,
        market_worker: MarketDataWorker,
        lifecycle: DesktopLifecycle,
        symbol_input: Option<Entity<InputState>>,
        indicator_input: Entity<InputState>,
    ) -> Self {
        let theme = AxiusflowTheme::dark();
        let restored_coinbase = match &startup {
            MarketWorkerStartup::Loading(startup) => {
                Some((startup.coinbase_interval, startup.restored_viewport))
            }
            MarketWorkerStartup::Rithmic => None,
        };
        let TerminalStartupState {
            chart,
            chart_state,
            chart_state_message,
            replay_label,
            worker_label,
            subscription_id,
            connection_state,
            connection_message,
            provider,
            coinbase_product,
        } = terminal_startup_state(startup, cx);
        let bridge_label = chart.as_ref().map_or_else(
            || "bridge awaiting snapshot".to_string(),
            |chart| bridge_status(chart.read(cx).replay_bridge_metrics()),
        );
        observe_chart(chart.as_ref(), cx);
        let dom = cx.new(move |_| ReadOnlyDomView::new(theme));
        Self {
            chart,
            dom,
            side_panel: None,
            side_panel_width: SIDE_PANEL_INITIAL_WIDTH,
            side_panel_resize: None,
            scrolls: WorkspaceScrollHandles::default(),
            chart_state,
            chart_state_message,
            theme,
            replay_label,
            worker_label,
            subscription_id,
            bridge_label,
            market_worker,
            lifecycle,
            pending_ui_diagnostics: None,
            connection_state,
            connection_message,
            symbol_browser: if provider == TerminalProvider::Coinbase {
                rithmic_shell::RithmicSymbolBrowser::coinbase_catalog_awaiting_search(
                    std::num::NonZeroUsize::MIN,
                    "",
                )
            } else {
                rithmic_shell::RithmicSymbolBrowser::default()
            },
            symbol_message: initial_symbol_message(provider),
            symbol_selection_pending: false,
            series_browser: rithmic_history::RithmicSeriesBrowser::default(),
            series_message: "Select a symbol before choosing a series".to_string(),
            rithmic_autoload_started: false,
            rithmic_reconnect: RithmicReconnectState::Idle,
            symbol_input,
            indicator_input,
            indicator_message: None,
            chrome_overlay: None,
            chrome_overlay_phase: ChromeOverlayPhase::Opening,
            chrome_overlay_generation: 0,
            timeframe_trigger_bounds: None,
            chrome_selection: 0,
            chrome_focus: cx.focus_handle().tab_stop(true),
            provider,
            coinbase_product,
            coinbase_switch: CoinbaseSwitchState::Idle,
            coinbase_interval: restored_coinbase
                .map_or(ChartInterval::Minute1, |restored| restored.0),
            coinbase_pending_interval: None,
            coinbase_pending_product: None,
            coinbase_pending_sequence: None,
            restored_viewport: restored_coinbase.and_then(|restored| restored.1),
            last_persisted_viewport: None,
            resource_class: ConsumerResourceClass::Foreground,
            #[cfg(feature = "diagnostics")]
            foreground_interactions: ForegroundInteractionDiagnostics::default(),
            #[cfg(feature = "diagnostics")]
            live_evidence_enabled: std::env::var_os("AXIUSFLOW_LIVE_EVIDENCE").is_some(),
            #[cfg(feature = "diagnostics")]
            live_evidence_publications: 0,
        }
    }

    fn retire_market_worker(&mut self, cx: &App) {
        if let Some(retirement) = self.market_worker.begin_retirement() {
            self.lifecycle.retire_market_worker(retirement, cx);
        }
    }

    fn set_market_resource_class(&mut self, resource_class: ConsumerResourceClass) {
        self.resource_class = resource_class;
        let _ = self
            .market_worker
            .try_set_market_resource_class(resource_class);
    }

    fn set_market_message_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.market_worker.set_message_wake(wake);
    }

    fn should_poll_market(&self) -> bool {
        self.resource_class == ConsumerResourceClass::Foreground
    }

    fn available_intervals(&self) -> &'static [ChartInterval] {
        if self.provider == TerminalProvider::Coinbase {
            COINBASE_INTERVALS
        } else {
            &ChartInterval::ALL
        }
    }

    fn selected_interval(&self) -> ChartInterval {
        if self.provider == TerminalProvider::Coinbase {
            self.coinbase_interval
        } else {
            self.series_browser
                .selected()
                .map_or(ChartInterval::Minute1, |request| request.series.interval())
        }
    }

    fn select_interval(&mut self, interval: ChartInterval, cx: &mut Context<Self>) -> bool {
        #[cfg(feature = "diagnostics")]
        let started = Instant::now();
        let selected = (|| {
            if self.provider != TerminalProvider::Coinbase {
                self.select_rithmic_series(interval.into(), cx);
                return true;
            }
            if self.coinbase_interval == interval && self.coinbase_pending_interval.is_none() {
                return true;
            }
            if self.coinbase_pending_interval == Some(interval) {
                return true;
            }
            let Some(product) = self.coinbase_product.clone() else {
                self.series_message = "Coinbase market selection is unavailable".to_string();
                cx.notify();
                return false;
            };
            let Ok(sequence) = self.market_worker.try_select_coinbase(product, interval) else {
                self.series_message = format!("{} history could not start", interval.label());
                cx.notify();
                return false;
            };
            self.coinbase_pending_interval = Some(interval);
            self.coinbase_pending_sequence = Some(sequence);
            self.coinbase_switch = CoinbaseSwitchState::Pending;
            self.chart_state = ChartState::Loading;
            self.chart_state_message = format!("Loading {} history", interval.label());
            self.series_message = format!("Switching to {}", interval.label());
            cx.notify();
            true
        })();
        #[cfg(feature = "diagnostics")]
        self.foreground_interactions
            .record_interval_selection(elapsed_nanos(started));
        selected
    }

    fn instrument_entries(&self, cx: &App) -> Vec<InstrumentMenuEntry> {
        if self.provider == TerminalProvider::Coinbase {
            let query = self
                .symbol_input
                .as_ref()
                .map(|input| input.read(cx).value().trim().to_ascii_uppercase())
                .unwrap_or_default();
            return self
                .symbol_browser
                .results()
                .iter()
                .enumerate()
                .filter(|(_, result)| {
                    query.is_empty()
                        || result.symbol.contains(&query)
                        || result
                            .name
                            .as_deref()
                            .is_some_and(|name| name.contains(&query))
                })
                .map(|(index, result)| InstrumentMenuEntry {
                    symbol: result.name.clone().unwrap_or_else(|| result.symbol.clone()),
                    checked: self
                        .coinbase_product
                        .as_ref()
                        .is_some_and(|selected| selected.provider_symbol == result.symbol),
                    selection: InstrumentMenuSelection::Coinbase(index),
                })
                .collect();
        }
        self.symbol_browser
            .results()
            .iter()
            .enumerate()
            .map(|(index, instrument)| InstrumentMenuEntry {
                symbol: instrument.symbol.clone(),
                checked: self.symbol_browser.selected().is_some_and(|selected| {
                    selected.instrument.symbol == instrument.symbol
                        && selected.instrument.exchange == instrument.exchange
                }),
                selection: InstrumentMenuSelection::Rithmic(index),
            })
            .collect()
    }

    fn select_instrument(
        &mut self,
        selection: InstrumentMenuSelection,
        cx: &mut Context<Self>,
    ) -> bool {
        #[cfg(feature = "diagnostics")]
        let started = Instant::now();
        let selected = (|| match selection {
            InstrumentMenuSelection::Rithmic(index) => self.select_rithmic_symbol(index, cx),
            InstrumentMenuSelection::Coinbase(index) => {
                if self.symbol_selection_pending {
                    self.symbol_message =
                        "A Coinbase market selection is already in progress".to_string();
                    cx.notify();
                    return false;
                }
                let Some(selection) = self.symbol_browser.select(index) else {
                    return false;
                };
                let request = SelectProviderInstrument {
                    consumer_id: 0,
                    selection_generation: selection.generation.get() as u64,
                    search_generation: selection.search_generation.get() as u64,
                    provider: "coinbase".to_string(),
                    symbol: selection.instrument.symbol.clone(),
                    exchange: selection.instrument.exchange.clone(),
                    entitlement_id: COINBASE_ENTITLEMENT_ID.to_string(),
                };
                if self.market_worker.try_select_provider(request).is_err() {
                    self.symbol_browser.reject_selection(selection.generation);
                    self.symbol_message =
                        "Coinbase market selection is busy; try again".to_string();
                    cx.notify();
                    return false;
                }
                self.symbol_selection_pending = true;
                self.symbol_message = format!("Selecting {}", selection.instrument.symbol);
                cx.notify();
                true
            }
        })();
        #[cfg(feature = "diagnostics")]
        self.foreground_interactions
            .record_instrument_selection(elapsed_nanos(started));
        selected
    }

    fn open_chrome_overlay(
        &mut self,
        overlay: ChromeOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chrome_overlay_generation = self.chrome_overlay_generation.saturating_add(1);
        self.chrome_overlay_phase = ChromeOverlayPhase::Opening;
        self.chrome_overlay = Some(overlay);
        self.chrome_selection = match overlay {
            ChromeOverlay::Timeframe => self
                .available_intervals()
                .iter()
                .position(|interval| *interval == self.selected_interval())
                .unwrap_or(0),
            ChromeOverlay::Instrument | ChromeOverlay::Indicator => 0,
        };
        match overlay {
            ChromeOverlay::Instrument => {
                if let Some(input) = &self.symbol_input {
                    input.update(cx, |input, input_cx| input.focus(window, input_cx));
                }
            }
            ChromeOverlay::Indicator => {
                self.indicator_input
                    .update(cx, |input, input_cx| input.focus(window, input_cx));
            }
            ChromeOverlay::Timeframe => self.chrome_focus.focus(window, cx),
        }
        cx.notify();
    }

    fn close_chrome_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chrome_overlay.is_none() || self.chrome_overlay_phase == ChromeOverlayPhase::Closing
        {
            return;
        }
        if self.chrome_overlay == Some(ChromeOverlay::Indicator) {
            self.indicator_input.update(cx, |input, input_cx| {
                input.set_value("", window, input_cx);
            });
        }
        self.chrome_focus.focus(window, cx);
        if cx.reduce_motion() {
            self.chrome_overlay = None;
            cx.notify();
            return;
        }

        self.chrome_overlay_generation = self.chrome_overlay_generation.saturating_add(1);
        self.chrome_overlay_phase = ChromeOverlayPhase::Closing;
        let generation = self.chrome_overlay_generation;
        cx.spawn_in(window, async move |app, cx| {
            cx.background_executor()
                .timer(CHROME_OVERLAY_TRANSITION_DURATION)
                .await;
            let _ = app.update_in(cx, |app, _, app_cx| {
                if should_finish_chrome_overlay_close(
                    app.chrome_overlay_phase,
                    app.chrome_overlay_generation,
                    generation,
                ) {
                    app.chrome_overlay = None;
                    app.chrome_overlay_phase = ChromeOverlayPhase::Opening;
                    app_cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn on_terminal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(command) =
            fullscreen_escape_command(event.keystroke.key.as_str(), window.is_fullscreen())
        {
            command.execute(window);
            cx.stop_propagation();
            return;
        }
        if self.chrome_overlay.is_none() || self.chrome_overlay_phase == ChromeOverlayPhase::Closing
        {
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => self.close_chrome_overlay(window, cx),
            "up" => {
                self.chrome_selection = self.chrome_selection.saturating_sub(1);
                cx.notify();
            }
            "down" => {
                let count = match self.chrome_overlay {
                    Some(ChromeOverlay::Instrument) => self.instrument_entries(cx).len(),
                    Some(ChromeOverlay::Indicator) => chart_chrome::filter_indicator_specs(
                        self.indicator_input.read(cx).value().as_ref(),
                    )
                    .len(),
                    Some(ChromeOverlay::Timeframe) => self.available_intervals().len(),
                    None => 0,
                };
                self.chrome_selection = (self.chrome_selection + 1).min(count.saturating_sub(1));
                cx.notify();
            }
            "enter" if self.chrome_overlay == Some(ChromeOverlay::Timeframe) => {
                if let Some(interval) = self
                    .available_intervals()
                    .get(self.chrome_selection)
                    .copied()
                    && self.select_interval(interval, cx)
                {
                    self.close_chrome_overlay(window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    #[cfg(feature = "diagnostics")]
    fn record_live_evidence_publication(&mut self, update: &ReplayStreamUpdate) {
        if !self.live_evidence_enabled || self.live_evidence_publications >= 256 {
            return;
        }
        self.live_evidence_publications = self.live_evidence_publications.saturating_add(1);
        match update {
            ReplayStreamUpdate::Snapshot(snapshot) => {
                let interval_nanos = (snapshot.bar_definition().interval_seconds > 0).then(|| {
                    i64::from(snapshot.bar_definition().interval_seconds)
                        .saturating_mul(1_000_000_000)
                });
                let interior_gaps = interval_nanos.map(|interval_nanos| {
                    snapshot
                        .bars()
                        .windows(2)
                        .filter(|pair| {
                            pair[1]
                                .provenance()
                                .exchange_timestamp_unix_nanos
                                .saturating_sub(pair[0].provenance().exchange_timestamp_unix_nanos)
                                != interval_nanos
                        })
                        .count()
                });
                let first_timestamp = snapshot
                    .bars()
                    .first()
                    .map_or(0, |bar| bar.provenance().exchange_timestamp_unix_nanos);
                let last_timestamp = snapshot
                    .bars()
                    .last()
                    .map_or(0, |bar| bar.provenance().exchange_timestamp_unix_nanos);
                let interior_gaps =
                    interior_gaps.map_or_else(|| "null".to_string(), |value| value.to_string());
                let interval_nanos =
                    interval_nanos.map_or_else(|| "null".to_string(), |value| value.to_string());
                eprintln!(
                    "AXIUSFLOW_LIVE_SNAPSHOT {{\"bar_count\":{},\"first_timestamp\":{first_timestamp},\"last_timestamp\":{last_timestamp},\"interior_gaps\":{interior_gaps},\"interval_nanos\":{interval_nanos}}}",
                    snapshot.bars().len()
                );
            }
            ReplayStreamUpdate::Delta(delta) => eprintln!(
                "AXIUSFLOW_LIVE_UPDATE {{\"kind\":\"delta\",\"timestamp\":{}}}",
                delta.item().provenance().exchange_timestamp_unix_nanos
            ),
            ReplayStreamUpdate::Tail(tail) => eprintln!(
                "AXIUSFLOW_LIVE_UPDATE {{\"kind\":\"tail\",\"timestamp\":{},\"forming\":{}}}",
                tail.item().provenance().exchange_timestamp_unix_nanos,
                tail.forming()
            ),
        }
    }

    fn apply_publication(&mut self, publication: MarketWorkerPublication, cx: &mut Context<Self>) {
        #[cfg(feature = "diagnostics")]
        self.record_live_evidence_publication(&publication.update);
        let MarketWorkerPublication {
            update,
            generation,
            subscription_id,
            worker_label,
            ui_diagnostics,
        } = publication;
        self.worker_label = worker_label;
        self.subscription_id = subscription_id;
        self.replay_label =
            generation_status(&self.worker_label, &self.subscription_id, generation);
        let next_state = match (&self.chart, update) {
            (None, axiusflow_application::ReplayStreamUpdate::Snapshot(snapshot)) => {
                let chart_theme = nucleus_chart_theme(self.theme.mode);
                let chart = cx
                    .new(move |_| NucleusChartView::with_replay_and_theme(&snapshot, chart_theme));
                if let Some((start, end)) = self.restored_viewport {
                    chart.update(cx, |chart, _| {
                        chart.set_visible_time_range_unix_nanos(start, end);
                    });
                }
                observe_chart(Some(&chart), cx);
                self.chart = Some(chart);
                ChartState::Ready
            }
            (Some(chart), update) => {
                let (accepted, recovery_pending) = chart.update(cx, |chart, _| {
                    let accepted = chart.try_queue_replay_update(update).is_ok();
                    if !accepted {
                        eprintln!("bounded chart queue overflowed; fixture resnapshot required");
                    }
                    (accepted, chart.replay_bridge_metrics().recovery_pending)
                });
                publication_chart_state(accepted, recovery_pending)
            }
            (
                None,
                axiusflow_application::ReplayStreamUpdate::Delta(_)
                | axiusflow_application::ReplayStreamUpdate::Tail(_),
            ) => {
                self.reject_incremental_publication(ui_diagnostics, cx);
                return;
            }
        };
        if let Some(diagnostics) = ui_diagnostics
            && let Some(replaced) = self.pending_ui_diagnostics.replace(diagnostics)
        {
            self.market_worker
                .send_ui_diagnostics(UiDiagnosticsFeedback::Coalesced {
                    generation: replaced.generation(),
                });
        }
        if next_state == ChartState::Ready {
            self.chart_state = ChartState::Ready;
            self.chart_state_message = if self.provider == TerminalProvider::Coinbase
                && coinbase_interval_is_history_only(self.coinbase_interval)
            {
                COINBASE_CALENDAR_HISTORY_STATUS.to_string()
            } else {
                "market snapshot is current".to_string()
            };
            if self.provider == TerminalProvider::Coinbase {
                self.symbol_selection_pending = false;
                self.symbol_message = self.coinbase_product.as_ref().map_or_else(
                    || "Coinbase market ready".to_string(),
                    |product| format!("{} · Coinbase spot", product.provider_symbol),
                );
                if coinbase_interval_is_history_only(self.coinbase_interval) {
                    self.connection_state = Some(FeedConnectionState::Disconnected);
                    self.connection_message = Some(COINBASE_CALENDAR_HISTORY_STATUS.to_string());
                } else {
                    self.connection_state = Some(FeedConnectionState::Streaming);
                    self.connection_message = Some("Coinbase market data is current".to_string());
                }
            }
        } else {
            self.set_chart_state(
                ChartState::Recovering,
                "chart update requires a correlated covering snapshot".to_string(),
                cx,
            );
        }
    }

    fn reject_incremental_publication(
        &mut self,
        diagnostics: Option<PendingUiDiagnostics>,
        cx: &mut Context<Self>,
    ) {
        if let Some(diagnostics) = diagnostics {
            self.market_worker
                .send_ui_diagnostics(UiDiagnosticsFeedback::Coalesced {
                    generation: diagnostics.generation(),
                });
        }
        self.set_chart_state(
            ChartState::Error,
            "market update arrived before the initial covering snapshot".to_string(),
            cx,
        );
    }

    fn apply_recovery(
        &mut self,
        request_id: u64,
        result: Result<MarketWorkerBootstrap, String>,
        cx: &mut Context<Self>,
    ) {
        let bootstrap = match result {
            Ok(bootstrap) => bootstrap,
            Err(error) => {
                if let Some(chart) = &self.chart {
                    chart.update(cx, |chart, chart_cx| {
                        chart.mark_replay_recovery_failed(request_id);
                        chart_cx.notify();
                    });
                }
                self.set_chart_state(ChartState::Error, error.clone(), cx);
                eprintln!("fixture recovery {request_id} failed: {error}");
                return;
            }
        };
        let Some(chart) = &self.chart else {
            self.set_chart_state(
                ChartState::Error,
                "recovery response arrived before the initial snapshot".to_string(),
                cx,
            );
            return;
        };
        let install = chart.update(cx, |chart, chart_cx| {
            let installed = chart.install_replay_recovery(request_id, &bootstrap.snapshot);
            chart_cx.notify();
            installed
        });
        match install {
            Ok(true) => {
                self.replay_label = generation_status(
                    &self.worker_label,
                    &bootstrap.subscription_id,
                    MarketPublicationGeneration::from_generation(&bootstrap.generation),
                );
                self.chart_state = ChartState::Ready;
                self.chart_state_message = if self.provider == TerminalProvider::Coinbase
                    && coinbase_interval_is_history_only(self.coinbase_interval)
                {
                    COINBASE_CALENDAR_HISTORY_STATUS.to_string()
                } else {
                    "market snapshot is current".to_string()
                };
                cx.notify();
            }
            Ok(false) => eprintln!("ignored stale fixture recovery response {request_id}"),
            Err(error) => {
                chart.update(cx, |chart, chart_cx| {
                    chart.mark_replay_recovery_failed(request_id);
                    chart_cx.notify();
                });
                eprintln!("fixture recovery {request_id} was rejected: {error}");
            }
        }
    }

    fn mark_market_stream_invalid(&mut self, message: &str, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.mark_replay_stream_invalid();
                chart_cx.notify();
            });
        }
        eprintln!("market worker invalidated the stream: {message}");
    }

    fn set_chart_state(&mut self, state: ChartState, message: String, cx: &mut Context<Self>) {
        if matches!(state, ChartState::Stale | ChartState::Recovering) {
            self.mark_market_stream_invalid(&message, cx);
        }
        self.chart_state = state;
        self.chart_state_message = message;
        cx.notify();
    }

    fn reset_chart_surface(&mut self, cx: &mut Context<Self>) {
        let chart_theme = nucleus_chart_theme(self.theme.mode);
        self.chart = Some(cx.new(move |_| NucleusChartView::empty_with_theme(chart_theme)));
    }

    fn dispatch_recovery(&mut self, cx: &mut Context<Self>) {
        if !self.market_worker.is_connected() {
            return;
        }
        let Some(chart) = &self.chart else {
            return;
        };
        let worker = &self.market_worker;
        let dispatch = chart.update(cx, |chart, chart_cx| {
            let result =
                chart.try_dispatch_replay_recovery(|command| worker.try_send_recovery(command));
            if result.as_ref().is_ok_and(|dispatched| *dispatched) {
                chart_cx.notify();
            }
            result
        });
        match dispatch {
            Ok(_) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(command)) => {
                self.market_worker.mark_disconnected();
                chart.update(cx, |chart, chart_cx| {
                    chart.mark_replay_recovery_failed(command.request_id);
                    chart_cx.notify();
                });
            }
        }
    }

    fn apply_market_worker_message(
        &mut self,
        message: MarketWorkerMessage,
        cx: &mut Context<Self>,
    ) {
        match message {
            MarketWorkerMessage::Update(publication) => {
                self.apply_publication(publication, cx);
            }
            MarketWorkerMessage::Diagnostics(snapshot) => {
                #[cfg(feature = "diagnostics")]
                eprintln!("desktop market diagnostics: {snapshot:?}");
                #[cfg(not(feature = "diagnostics"))]
                drop(snapshot);
            }
            MarketWorkerMessage::Recovery { request_id, result } => {
                self.apply_recovery(request_id, result, cx);
            }
            MarketWorkerMessage::State { state, message } => {
                if state == ChartState::Error && self.provider == TerminalProvider::Coinbase {
                    self.coinbase_switch = CoinbaseSwitchState::Idle;
                    self.coinbase_pending_interval = None;
                    self.coinbase_pending_product = None;
                    self.coinbase_pending_sequence = None;
                    self.symbol_selection_pending = false;
                    self.connection_state = Some(FeedConnectionState::Disconnected);
                    self.connection_message = Some(message.clone());
                } else if self.provider == TerminalProvider::Coinbase
                    && matches!(state, ChartState::Loading | ChartState::Recovering)
                {
                    self.connection_state = Some(FeedConnectionState::Recovering);
                    self.connection_message = Some(message.clone());
                }
                self.set_chart_state(state, message, cx);
            }
            MarketWorkerMessage::CoinbaseSwitchMarker { sequence } => {
                self.apply_coinbase_switch_marker(sequence, cx);
            }
            MarketWorkerMessage::Connection { state, message } => {
                self.apply_connection_state(state, message, cx);
            }
            MarketWorkerMessage::ProviderCatalog(event) => {
                self.apply_provider_catalog_event(event, cx);
            }
            MarketWorkerMessage::RithmicHistory {
                selection_generation,
                series_generation,
                result,
            } => {
                self.apply_rithmic_history(selection_generation, series_generation, result, cx);
            }
            MarketWorkerMessage::RithmicLive {
                selection_generation,
                series_generation,
                update,
            } => {
                self.apply_rithmic_live(selection_generation, series_generation, update, cx);
            }
            MarketWorkerMessage::RithmicDom(frame) => {
                self.apply_rithmic_dom(frame, cx);
            }
            MarketWorkerMessage::CoinbaseDom(frame) => {
                self.dom
                    .update(cx, |dom, dom_cx| dom.replace_frame(frame, dom_cx));
            }
            MarketWorkerMessage::ChartViewport {
                start_unix_nanos,
                end_unix_nanos,
            } => {
                let viewport = (start_unix_nanos, end_unix_nanos);
                self.restored_viewport = Some(viewport);
                self.last_persisted_viewport = Some(viewport);
                if let Some(chart) = &self.chart {
                    chart.update(cx, |chart, chart_cx| {
                        if chart.set_visible_time_range_unix_nanos(start_unix_nanos, end_unix_nanos)
                        {
                            chart_cx.notify();
                        }
                    });
                }
            }
        }
    }

    fn apply_provider_catalog_event(
        &mut self,
        event: ProviderCatalogEvent,
        cx: &mut Context<Self>,
    ) {
        self.apply_catalog_event(event, cx);
    }

    fn apply_catalog_results(
        &mut self,
        generation: u64,
        instruments: Vec<ProviderInstrumentSummary>,
    ) -> Option<usize> {
        let generation = usize_generation(generation)?;
        let count = instruments.len();
        self.symbol_browser
            .apply_results(generation, instruments)
            .then_some(count)
    }

    fn confirm_catalog_selection(&mut self, generation: u64) -> bool {
        usize_generation(generation)
            .is_some_and(|generation| self.symbol_browser.confirm_selection(generation))
    }

    fn consume_catalog_search_authorization(&mut self) {
        if let Some(search_generation) = self
            .symbol_browser
            .selected()
            .map(|selection| selection.search_generation)
        {
            self.symbol_browser
                .consume_completed_search(search_generation);
        }
    }

    fn apply_coinbase_switch_marker(&mut self, sequence: u64, cx: &mut Context<Self>) {
        if self.provider != TerminalProvider::Coinbase
            || !self.coinbase_switch.is_pending()
            || self.coinbase_pending_sequence != Some(sequence)
        {
            return;
        }
        if let Some(interval) = self.coinbase_pending_interval.take() {
            self.coinbase_interval = interval;
        }
        if let Some(product) = self.coinbase_pending_product.take() {
            self.coinbase_product = Some(product);
        }
        self.coinbase_pending_sequence = None;
        self.coinbase_switch = CoinbaseSwitchState::Idle;
        self.chart = None;
        self.restored_viewport = None;
        self.last_persisted_viewport = None;
        self.chart_state = ChartState::Loading;
        cx.notify();
    }

    fn poll_market_worker(&mut self, cx: &mut Context<Self>) -> usize {
        let chart_was_missing = self.chart.is_none();
        let (messages, disconnected) = self
            .market_worker
            .drain_messages_up_to(MARKET_MESSAGES_PER_FRAME);
        let applied = messages.len();
        let chart_update_received = messages
            .iter()
            .any(|message| matches!(message, MarketWorkerMessage::Update(_)));
        for message in messages {
            self.apply_market_worker_message(message, cx);
        }
        if self.provider == TerminalProvider::Rithmic
            && disconnected
            && !matches!(self.connection_state, Some(FeedConnectionState::Stopped))
        {
            self.apply_connection_state(
                FeedConnectionState::Stopped,
                "Rithmic market worker stopped".to_string(),
                cx,
            );
        } else if disconnected && self.chart_state != ChartState::Error {
            let message = match self.provider {
                TerminalProvider::Coinbase => "Coinbase market worker stopped",
                TerminalProvider::Rithmic => "Rithmic market worker stopped",
            }
            .to_string();
            self.connection_state = Some(FeedConnectionState::Stopped);
            self.connection_message = Some(message.clone());
            self.set_chart_state(ChartState::Error, message, cx);
        }
        self.dispatch_recovery(cx);
        self.dispatch_retained_symbol_search(cx);

        let status = self.chart.as_ref().map_or_else(
            || "bridge awaiting snapshot".to_string(),
            |chart| {
                let metrics = chart.read(cx).replay_bridge_metrics();
                let reconciled =
                    reconciled_bridge_state(self.chart_state, metrics.recovery_pending);
                if reconciled != self.chart_state {
                    self.chart_state = reconciled;
                    self.chart_state_message =
                        "chart validation requires a correlated covering snapshot".to_string();
                    cx.notify();
                }
                bridge_status(metrics)
            },
        );
        if self.bridge_label != status {
            self.bridge_label = status;
            cx.notify();
        }
        if applied > 0 {
            if chart_was_missing && self.chart.is_some() {
                cx.notify();
            } else if chart_update_received && let Some(chart) = &self.chart {
                chart.update(cx, |_, chart_cx| chart_cx.notify());
            }
        }
        applied + usize::from(disconnected)
    }

    fn apply_connection_state(
        &mut self,
        state: FeedConnectionState,
        message: String,
        cx: &mut Context<Self>,
    ) {
        let retirement = RithmicSessionRetirement::from_connection(state);
        let retained_market_data = self
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).has_market_data());
        match retirement {
            RithmicSessionRetirement::Offline | RithmicSessionRetirement::Recovering => {
                self.begin_rithmic_reconnect(cx);
            }
            RithmicSessionRetirement::Stopped => {
                self.rithmic_reconnect = RithmicReconnectState::Idle;
                self.retire_rithmic_session(cx);
            }
            RithmicSessionRetirement::None => {}
        }
        if let Some(chart_state) = retirement.chart_state(retained_market_data) {
            self.chart_state = chart_state;
            self.chart_state_message.clone_from(&message);
        }
        self.connection_state = Some(state);
        let ready_action = rithmic_ready_action(
            state,
            &message,
            &self.rithmic_reconnect,
            self.rithmic_autoload_started,
        );
        self.connection_message = Some(message);
        match ready_action {
            RithmicReadyAction::Reconnect(symbol) => {
                if self.search_symbol_query(&symbol, cx)
                    && let RithmicReconnectState::AwaitingSearch(target) = &self.rithmic_reconnect
                {
                    self.rithmic_reconnect = RithmicReconnectState::SearchInFlight(target.clone());
                }
            }
            RithmicReadyAction::Autoload => {
                self.rithmic_autoload_started = true;
                let _ = self.search_symbol_query("MNQ", cx);
            }
            RithmicReadyAction::None => {}
        }
        cx.notify();
    }

    fn apply_theme(&mut self, theme: &AxiusflowTheme, cx: &mut Context<Self>) {
        self.dom.update(cx, |dom, dom_cx| {
            dom.set_theme(*theme, dom_cx);
        });
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_theme(nucleus_chart_theme(theme.mode));
                chart_cx.notify();
            });
        }
        self.theme = *theme;
        cx.notify();
    }

    fn search_symbol_query(&mut self, query: &str, cx: &mut Context<Self>) -> bool {
        if self.symbol_browser.search_pending() || self.symbol_selection_pending {
            match self.symbol_browser.retain_latest_search(query) {
                Ok(already_dispatched) => {
                    if !already_dispatched {
                        self.symbol_message = format!(
                            "Waiting to search the latest {} query",
                            if self.provider == TerminalProvider::Coinbase {
                                "Coinbase"
                            } else {
                                "Rithmic"
                            }
                        );
                    }
                    cx.notify();
                    return already_dispatched;
                }
                Err(message) => {
                    self.symbol_message = message.to_string();
                    cx.notify();
                }
            }
            return false;
        }
        let request = match self.symbol_browser.begin_search(query) {
            Ok(request) => request,
            Err(message) => {
                self.symbol_message = message.to_string();
                cx.notify();
                return false;
            }
        };
        self.dispatch_symbol_search(request, cx)
    }

    fn dispatch_symbol_search(
        &mut self,
        request: rithmic_shell::RithmicSymbolSearchRequest,
        cx: &mut Context<Self>,
    ) -> bool {
        let provider = terminal_provider_id(self.provider);
        let retained_query = request.query.clone();
        let request_id = request.request_id;
        let search = SearchProviderInstruments {
            consumer_id: 0,
            search_generation: u64::try_from(request_id.get()).unwrap_or(u64::MAX),
            provider: provider.to_string(),
            query: request.query,
            maximum_results: u32::try_from(self.symbol_browser.maximum_results())
                .unwrap_or(u32::MAX),
        };
        let dispatched = if self.market_worker.try_search_provider(search).is_ok() {
            self.symbol_message = if provider == "coinbase" {
                "Searching Coinbase spot markets".to_string()
            } else {
                "Searching Rithmic Test symbols".to_string()
            };
            true
        } else {
            self.symbol_browser.reject_search(request_id);
            let _ = self.symbol_browser.retain_latest_search(&retained_query);
            self.symbol_message = "Symbol search is busy; try again".to_string();
            false
        };
        cx.notify();
        dispatched
    }

    fn dispatch_retained_symbol_search(&mut self, cx: &mut Context<Self>) -> bool {
        if self.symbol_selection_pending {
            return false;
        }
        if let Some(request) = self.symbol_browser.begin_retained_search() {
            return self.dispatch_symbol_search(request, cx);
        }
        false
    }

    fn search_symbol_input(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(input) = &self.symbol_input else {
            return false;
        };
        let query = input.read(cx).value().to_string();
        self.search_symbol_query(&query, cx)
    }

    fn submit_symbol_input(&mut self, cx: &mut Context<Self>) -> bool {
        let entries = self.instrument_entries(cx);
        match symbol_submit_decision(self.provider, entries.len(), self.chrome_selection) {
            SymbolSubmitDecision::Select(index) => entries
                .get(index)
                .is_some_and(|entry| self.select_instrument(entry.selection, cx)),
            SymbolSubmitDecision::Search => {
                self.search_symbol_input(cx);
                false
            }
            SymbolSubmitDecision::None => {
                self.symbol_message = "No Coinbase spot markets match this search".to_string();
                cx.notify();
                false
            }
        }
    }

    fn begin_rithmic_reconnect(&mut self, cx: &mut Context<Self>) {
        let series = self
            .series_browser
            .selected()
            .map_or(rithmic_history::RithmicSeries::Minute1, |request| {
                request.series
            });
        let no_retired_selection = if self.rithmic_reconnect == RithmicReconnectState::Idle {
            if let Some(selection) = self.symbol_browser.selected().cloned() {
                self.rithmic_reconnect =
                    RithmicReconnectState::AwaitingSearch(RithmicReconnectTarget {
                        symbol: selection.instrument.symbol,
                        exchange: selection.instrument.exchange,
                        series,
                    });
                false
            } else {
                true
            }
        } else {
            false
        };
        if no_retired_selection {
            self.rithmic_autoload_started = false;
        }
        self.retire_rithmic_session(cx);
    }

    fn retire_rithmic_session(&mut self, cx: &mut Context<Self>) {
        self.symbol_selection_pending = false;
        self.symbol_browser.invalidate_session();
        self.series_browser.reset();
        self.dom
            .update(cx, axiusflow_terminal_ui::ReadOnlyDomView::clear);
    }

    fn select_rithmic_symbol(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        if self.symbol_selection_pending {
            self.symbol_message = "A contract selection is already in progress".to_string();
            cx.notify();
            return false;
        }
        let Some(selection) = self.symbol_browser.select(index) else {
            return false;
        };
        let entitlement_id = format!(
            "rithmic-test:{}:{}",
            selection.instrument.exchange, selection.instrument.symbol
        );
        let request = SelectProviderInstrument {
            consumer_id: 0,
            selection_generation: u64::try_from(selection.generation.get()).unwrap_or(u64::MAX),
            search_generation: u64::try_from(selection.search_generation.get()).unwrap_or(u64::MAX),
            provider: "rithmic".to_string(),
            symbol: selection.instrument.symbol.clone(),
            exchange: selection.instrument.exchange.clone(),
            entitlement_id,
        };
        let dispatched = if self.market_worker.try_select_provider(request).is_ok() {
            self.symbol_selection_pending = true;
            self.dom
                .update(cx, axiusflow_terminal_ui::ReadOnlyDomView::clear);
            self.symbol_message = format!(
                "Selecting {} · {}",
                selection.instrument.symbol, selection.instrument.exchange
            );
            true
        } else {
            self.symbol_browser.reject_selection(selection.generation);
            self.symbol_message = "Symbol selection is busy; try again".to_string();
            false
        };
        cx.notify();
        dispatched
    }

    fn apply_catalog_event(&mut self, event: ProviderCatalogEvent, cx: &mut Context<Self>) {
        if provider_catalog_event_provider(&event) != terminal_provider_id(self.provider) {
            return;
        }
        let coinbase = self.provider == TerminalProvider::Coinbase;
        match event {
            ProviderCatalogEvent::SearchCompleted(result) => {
                let Some(count) =
                    self.apply_catalog_results(result.search_generation, result.instruments)
                else {
                    return;
                };
                self.symbol_message = if coinbase {
                    format!("{count} active Coinbase spot markets")
                } else {
                    format!("{count} matching symbols")
                };
                if self.symbol_browser.has_retained_search() {
                    self.dispatch_retained_symbol_search(cx);
                    cx.notify();
                    return;
                }
                if !coinbase && self.rithmic_reconnect != RithmicReconnectState::Idle {
                    if let Some(index) = self.rithmic_reconnect.target().and_then(|target| {
                        reconnect_contract_index(self.symbol_browser.results(), target)
                    }) {
                        self.select_rithmic_symbol(index, cx);
                    } else {
                        self.rithmic_reconnect = RithmicReconnectState::Idle;
                        self.symbol_message =
                            "The previous Rithmic contract is unavailable after reconnect"
                                .to_string();
                    }
                } else if !coinbase
                    && self.rithmic_autoload_started
                    && self.symbol_browser.selected().is_none()
                    && let Some(index) =
                        default_rithmic_contract_index(self.symbol_browser.results())
                {
                    self.select_rithmic_symbol(index, cx);
                }
                self.dispatch_retained_symbol_search(cx);
            }
            ProviderCatalogEvent::SelectionInstalled(instrument) if coinbase => {
                if !self.confirm_catalog_selection(instrument.selection_generation) {
                    return;
                }
                self.consume_catalog_search_authorization();
                let interval = self
                    .coinbase_pending_interval
                    .unwrap_or(self.coinbase_interval);
                let Ok(sequence) = self
                    .market_worker
                    .try_select_coinbase(instrument.clone(), interval)
                else {
                    self.symbol_selection_pending = false;
                    self.symbol_message = "Coinbase market history could not start".to_string();
                    return;
                };
                self.coinbase_pending_product = Some(instrument);
                self.coinbase_pending_interval = Some(interval);
                self.coinbase_pending_sequence = Some(sequence);
                self.coinbase_switch = CoinbaseSwitchState::Pending;
                self.chart_state = ChartState::Loading;
                self.chart_state_message = format!("Loading {} market history", interval.label());
                self.symbol_message = "Loading the selected Coinbase market".to_string();
            }
            ProviderCatalogEvent::SelectionInstalled(instrument) => {
                self.apply_rithmic_selection(&instrument, cx);
            }
            ProviderCatalogEvent::CommandRejected { rejection, command } => {
                let Some(generation) = usize_generation(rejection.command_generation) else {
                    return;
                };
                let selection = command == ProviderCatalogCommand::Selection;
                let rejected = if selection {
                    self.symbol_browser.reject_selection(generation)
                } else {
                    self.symbol_browser.reject_search(generation)
                };
                if !rejected {
                    return;
                }
                if coinbase || selection {
                    self.symbol_selection_pending = false;
                }
                let reason = ProviderCatalogRejectionReason::try_from(rejection.reason)
                    .unwrap_or(ProviderCatalogRejectionReason::Unspecified);
                self.symbol_message =
                    catalog_rejection_message(reason, command, coinbase).to_string();
                if !coinbase && let Some(target) = self.rithmic_reconnect.target().cloned() {
                    self.rithmic_reconnect = RithmicReconnectState::AwaitingSearch(target);
                    self.retire_rithmic_session(cx);
                }
                self.dispatch_retained_symbol_search(cx);
            }
        }
        cx.notify();
    }

    fn apply_rithmic_selection(
        &mut self,
        instrument: &InstallProviderInstrument,
        cx: &mut Context<Self>,
    ) {
        if !self.confirm_catalog_selection(instrument.selection_generation) {
            return;
        }
        self.symbol_selection_pending = false;
        let recovered_series = self
            .rithmic_reconnect
            .target()
            .map_or(rithmic_history::RithmicSeries::Minute1, |target| {
                target.series
            });
        self.rithmic_reconnect = RithmicReconnectState::Idle;
        self.series_browser.reset();
        self.reset_chart_surface(cx);
        self.bridge_label = "bridge awaiting series selection".to_string();
        self.replay_label = "Selected instrument · choose a series".to_string();
        self.subscription_id = format!("{} · {}", instrument.display_symbol, instrument.venue_id);
        self.symbol_message = format!("Selected {}", instrument.display_symbol);
        self.series_message = "Choose a chart series".to_string();
        self.connection_state = Some(FeedConnectionState::Streaming);
        self.connection_message = Some("Rithmic Test market subscription active".to_string());
        self.select_rithmic_series(recovered_series, cx);
    }

    fn select_rithmic_series(
        &mut self,
        series: rithmic_history::RithmicSeries,
        cx: &mut Context<Self>,
    ) {
        if self.series_browser.pending().is_some() {
            self.series_message = "A chart series is already loading".to_string();
            cx.notify();
            return;
        }
        let Some(selection) = self.symbol_browser.selected() else {
            self.series_message = "Select a symbol before choosing a series".to_string();
            cx.notify();
            return;
        };
        let request = self.series_browser.select(selection.generation, series);
        if self
            .market_worker
            .try_request_engine_series(EngineSeriesRequest {
                selection_generation: request.selection_generation,
                series_generation: request.series_generation,
                interval: request.series.interval(),
            })
            .is_ok()
        {
            self.reset_chart_surface(cx);
            self.bridge_label = "bridge awaiting visible history".to_string();
            self.series_message = format!("Loading {} visible history", series.label());
            self.set_chart_state(
                ChartState::Loading,
                format!("loading {} visible history", series.label()),
                cx,
            );
        } else {
            self.series_browser.reject(request.series_generation);
            self.series_message = "Rithmic history worker is busy; try again".to_string();
        }
        cx.notify();
    }

    fn apply_rithmic_history(
        &mut self,
        selection_generation: std::num::NonZeroUsize,
        series_generation: std::num::NonZeroUsize,
        result: Result<Box<MarketWorkerBootstrap>, String>,
        cx: &mut Context<Self>,
    ) {
        let Ok(bootstrap) = result else {
            if self.series_browser.reject(series_generation) {
                self.series_message = "Rithmic visible history is unavailable".to_string();
                self.set_chart_state(
                    ChartState::Error,
                    "Rithmic visible history could not be loaded".to_string(),
                    cx,
                );
            }
            return;
        };
        if !self
            .series_browser
            .accept(selection_generation, series_generation)
        {
            return;
        }
        let replay_label = generation_status(
            &bootstrap.worker_label,
            &bootstrap.subscription_id,
            MarketPublicationGeneration::from_generation(&bootstrap.generation),
        );
        let snapshot = bootstrap.snapshot;
        let visible_bar_count = snapshot.bars().len();
        let chart_theme = nucleus_chart_theme(self.theme.mode);
        self.chart =
            Some(cx.new(move |_| NucleusChartView::with_replay_and_theme(&snapshot, chart_theme)));
        self.worker_label = bootstrap.worker_label;
        self.subscription_id = bootstrap.subscription_id;
        self.replay_label = replay_label;
        self.bridge_label = self.chart.as_ref().map_or_else(
            || "bridge awaiting snapshot".to_string(),
            |chart| bridge_status(chart.read(cx).replay_bridge_metrics()),
        );
        self.series_message = format!("{visible_bar_count} visible bars are current");
        self.set_chart_state(
            ChartState::Ready,
            "Rithmic visible history is current".to_string(),
            cx,
        );
    }

    fn apply_rithmic_live(
        &mut self,
        selection_generation: std::num::NonZeroUsize,
        series_generation: std::num::NonZeroUsize,
        update: axiusflow_application::ReplayStreamUpdate,
        cx: &mut Context<Self>,
    ) {
        let Some(selected) = self.series_browser.selected() else {
            return;
        };
        if selected.selection_generation != selection_generation
            || selected.series_generation != series_generation
        {
            return;
        }
        let Some(chart) = &self.chart else {
            return;
        };
        if chart
            .update(cx, |chart, _| {
                chart.try_queue_replay_update(update).map_err(|_| ())
            })
            .is_err()
        {
            self.set_chart_state(
                ChartState::Recovering,
                "Rithmic live chart requires a covering snapshot".to_string(),
                cx,
            );
            return;
        }
        self.series_message = "Live candle is current".to_string();
        self.chart_state = ChartState::Ready;
        self.chart_state_message = "Rithmic live candle is current".to_string();
    }

    fn apply_rithmic_dom(&mut self, frame: DomFrame, cx: &mut Context<Self>) {
        let selected_generation = self
            .symbol_browser
            .selected()
            .and_then(|selection| u64::try_from(selection.generation.get()).ok());
        if selected_generation != Some(frame.selection_generation) {
            return;
        }
        self.dom.update(cx, |dom, dom_cx| {
            dom.replace_frame(frame, dom_cx);
        });
    }

    fn toggle_dom(&mut self, cx: &mut Context<Self>) {
        if self.symbol_browser.selected().is_some() {
            self.side_panel = (self.side_panel != Some(SidePanel::Dom)).then_some(SidePanel::Dom);
            cx.notify();
        }
    }

    fn close_side_panel(&mut self, cx: &mut Context<Self>) {
        self.side_panel_resize = None;
        if self.side_panel.take().is_some() {
            cx.notify();
        }
    }

    fn begin_side_panel_resize(&mut self, pointer_x: f32) {
        self.side_panel_resize = Some(SidePanelResize {
            pointer_x,
            width: self.side_panel_width,
        });
    }

    fn update_side_panel_resize(
        &mut self,
        pointer_x: f32,
        left_pressed: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(resize) = self.side_panel_resize else {
            return;
        };
        if !left_pressed {
            self.side_panel_resize = None;
            return;
        }
        let width = resized_side_panel_width(resize, pointer_x);
        if (width - self.side_panel_width).abs() > f32::EPSILON {
            self.side_panel_width = width;
            cx.notify();
        }
    }

    fn end_side_panel_resize(&mut self) {
        self.side_panel_resize = None;
    }

    fn reset_chart_view(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.reset_view();
                chart_cx.notify();
            });
        }
    }

    fn scroll_chart_to_latest(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.scroll_to_latest();
                chart_cx.notify();
            });
        }
    }

    fn select_drawing_tool(&mut self, tool: ChartDrawingTool, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_drawing_tool(tool);
                chart_cx.notify();
            });
            cx.notify();
        }
    }

    fn remove_selected_chart_object(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.remove_selected_chart_object() {
                    chart_cx.notify();
                }
            });
            cx.notify();
        }
    }

    fn toggle_selected_drawing_lock(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                let locked = chart.selected_drawing_locked();
                if chart.set_selected_drawing_locked(!locked) {
                    chart_cx.notify();
                }
            });
            cx.notify();
        }
    }

    fn toggle_all_drawings_lock(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                let all_locked = chart.drawings_lock_summary().all_locked;
                if chart.set_all_drawings_locked(!all_locked) {
                    chart_cx.notify();
                }
            });
            cx.notify();
        }
    }

    fn clear_drawings(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.clear_drawings();
                chart.cancel_drawing();
                chart_cx.notify();
            });
            cx.notify();
        }
    }

    fn add_indicator(&mut self, indicator: ChartIndicator, cx: &mut Context<Self>) -> bool {
        let Some(chart) = self.chart.clone() else {
            self.indicator_message = Some("Chart data is not available yet".to_string());
            cx.notify();
            return false;
        };
        let result = chart.update(cx, |chart, chart_cx| {
            let result = chart.add_indicator(indicator);
            if result.is_ok() {
                chart_cx.notify();
            }
            result
        });
        match result {
            Ok(_) => {
                self.indicator_message = None;
                true
            }
            Err(error) => {
                self.indicator_message = Some(error.to_string());
                cx.notify();
                false
            }
        }
    }

    fn drawing_toolbar_state(&self, cx: &App) -> DrawingToolbarState {
        self.chart
            .as_ref()
            .map_or_else(DrawingToolbarState::default, |chart| {
                DrawingToolbarState::from_chart(chart.read(cx))
            })
    }
}

fn chrome_overlay_layer(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    theme: &AxiusflowTheme,
    chrome_height: f32,
    cx: &App,
) -> Option<AnyElement> {
    let overlay = app_state.chrome_overlay?;
    let timeframe = overlay == ChromeOverlay::Timeframe;
    let timeframe_left = timeframe_overlay_left(app_state.timeframe_trigger_bounds);
    let phase = app_state.chrome_overlay_phase;
    let generation = app_state.chrome_overlay_generation;
    let closing = phase == ChromeOverlayPhase::Closing;
    let panel = match overlay {
        ChromeOverlay::Instrument => instrument_dialog_content(
            app,
            &InstrumentSelectorState {
                label: terminal_instrument_label(app_state),
                instruments: app_state.instrument_entries(cx),
                input: app_state.symbol_input.clone(),
                selection_pending: app_state.symbol_selection_pending,
                enabled: true,
                provider: app_state.provider,
                keyboard_selection: app_state.chrome_selection,
                scroll: app_state.scrolls.instrument.clone(),
            },
            theme,
        )
        .into_any_element(),
        ChromeOverlay::Indicator => indicator_dialog_content(
            app,
            &app_state.indicator_input,
            app_state.indicator_message.as_deref(),
            app_state.chrome_selection,
            &app_state.scrolls.indicator,
            theme,
            cx,
        )
        .into_any_element(),
        ChromeOverlay::Timeframe => timeframe_overlay_content(
            app,
            app_state.available_intervals(),
            app_state.selected_interval(),
            app_state.chrome_selection,
            app_state.series_browser.pending().is_some() || app_state.coinbase_switch.is_pending(),
            theme,
        )
        .into_any_element(),
    };
    let close_app = app.clone();
    Some(
        div()
            .id("chrome_overlay_scrim")
            .absolute()
            .top(px(chrome_height))
            .left_0()
            .right_0()
            .bottom_0()
            .occlude()
            .flex()
            .items_start()
            .when(timeframe, |scrim| scrim.justify_start().pl(timeframe_left))
            .when(!timeframe, |scrim| scrim.justify_center().pt_2())
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                close_app.update(cx, |app, app_cx| {
                    app.close_chrome_overlay(window, app_cx);
                });
                cx.stop_propagation();
            })
            .child(
                div()
                    .id("chrome_overlay_panel")
                    .relative()
                    .flex_none()
                    .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                    .border_1()
                    .border_color(gpui_color(theme.colors.border))
                    .bg(gpui_color(theme.colors.surface))
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(panel)
                    .children(closing.then(|| {
                        div()
                            .id("chrome_overlay_closing_blocker")
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .left_0()
                            .occlude()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    }))
                    .with_animation(
                        ("chrome_overlay_transition", generation),
                        Animation::new(CHROME_OVERLAY_TRANSITION_DURATION)
                            .with_easing(ease_out_quint()),
                        move |panel, delta| {
                            let progress = chrome_overlay_progress(phase, delta);
                            panel
                                .opacity(progress)
                                .mt(px(-CHROME_OVERLAY_TRANSITION_OFFSET * (1.0 - progress)))
                        },
                    ),
            )
            .into_any_element(),
    )
}

fn timeframe_overlay_left(trigger_bounds: Option<Bounds<Pixels>>) -> Pixels {
    trigger_bounds.map_or(px(0.0), |bounds| bounds.origin.x.max(px(0.0)))
}

fn timeframe_overlay_content(
    app: &Entity<WorkspaceSurface>,
    intervals: &'static [ChartInterval],
    selected: ChartInterval,
    keyboard_selection: usize,
    pending: bool,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    div()
        .w(px(192.0))
        .flex()
        .flex_col()
        .p_2()
        .gap_1()
        .text_color(gpui_color(colors.text_secondary))
        .children(
            intervals
                .iter()
                .copied()
                .enumerate()
                .map(move |(index, interval)| {
                    let row_app = app.clone();
                    div()
                        .id(("timeframe_overlay_row", index))
                        .h(px(40.0))
                        .flex()
                        .items_center()
                        .justify_between()
                        .px_3()
                        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                        .text_sm()
                        .when(selected == interval, |row| {
                            row.bg(gpui_color(colors.active_bg))
                                .text_color(gpui_color(colors.text_primary))
                        })
                        .when(keyboard_selection == index, |row| {
                            row.bg(gpui_color(colors.active_bg))
                                .text_color(gpui_color(colors.text_primary))
                        })
                        .when(!pending, |row| {
                            row.cursor_pointer()
                                .hover(|row| {
                                    row.bg(gpui_color(colors.hover_bg))
                                        .text_color(gpui_color(colors.text_primary))
                                })
                                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                    row_app.update(cx, |app, app_cx| {
                                        if app.select_interval(interval, app_cx) {
                                            app.close_chrome_overlay(window, app_cx);
                                        }
                                    });
                                    cx.stop_propagation();
                                })
                        })
                        .child(interval.label())
                        .children(
                            (selected == interval)
                                .then(|| header_icon(HugeIcon::CheckmarkCircleIcon01)),
                        )
                }),
        )
}

struct MarketWorkspaceState<'a> {
    app: Entity<WorkspaceSurface>,
    pane_id: u64,
    chart: Option<&'a Entity<NucleusChartView>>,
    chart_has_market_data: bool,
    dom: Entity<ReadOnlyDomView>,
    side_panel: Option<SidePanel>,
    side_panel_width: f32,
    chart_state: ChartState,
    chart_status_detail: String,
    theme: &'a AxiusflowTheme,
}

fn market_workspace(state: MarketWorkspaceState<'_>) -> impl IntoElement + use<> {
    let MarketWorkspaceState {
        app,
        pane_id,
        chart,
        chart_has_market_data,
        dom,
        side_panel,
        side_panel_width,
        chart_state,
        chart_status_detail,
        theme,
    } = state;
    let colors = theme.colors;
    let notice = chart_surface_notice(chart_state, chart_has_market_data, &chart_status_detail);
    let chart_surface = chart_pane_host(chart)
        .id(("primary_chart", pane_id))
        .bg(gpui_color(colors.surface))
        .children(notice.map(|notice| chart_notice(notice, theme)));
    let content = if let Some(side_panel) = side_panel {
        let resize_app = app.clone();
        let move_app = app.clone();
        let release_app = app.clone();
        let side_panel_content = div().w(px(side_panel_width)).flex_none().child(
            div()
                .size_full()
                .flex()
                .flex_col()
                .overflow_hidden()
                .bg(gpui_color(colors.surface))
                .child(side_panel_header(side_panel, app, theme))
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .children((side_panel == SidePanel::Dom).then_some(dom)),
                ),
        );
        div()
            .id(("market_workspace", pane_id))
            .size_full()
            .flex()
            .child(chart_surface)
            .child(
                div()
                    .id(("side_panel_resize", pane_id))
                    .h_full()
                    .w(px(SIDE_PANEL_RESIZE_HANDLE_WIDTH))
                    .flex_none()
                    .cursor_col_resize()
                    .bg(gpui_color(colors.border_secondary))
                    .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                        resize_app.update(cx, |surface, _| {
                            surface.begin_side_panel_resize(f32::from(event.position.x));
                        });
                        cx.stop_propagation();
                    }),
            )
            .child(side_panel_content)
            .on_mouse_move(move |event, _, cx| {
                move_app.update(cx, |surface, surface_cx| {
                    surface.update_side_panel_resize(
                        f32::from(event.position.x),
                        event.pressed_button == Some(MouseButton::Left),
                        surface_cx,
                    );
                });
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                release_app.update(cx, |surface, _| surface.end_side_panel_resize());
            })
            .into_any_element()
    } else {
        chart_surface.into_any_element()
    };
    div().size_full().overflow_hidden().child(content)
}

fn chart_pane_host(chart: Option<&Entity<NucleusChartView>>) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .size_full()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .children(chart.cloned())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DrawingToolbarState {
    availability: DrawingToolbarAvailability,
    active_tool: ChartDrawingTool,
    drawing_count: usize,
    selection: DrawingToolbarSelection,
    selected_locked: bool,
    all_locked: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum DrawingToolbarAvailability {
    #[default]
    Unavailable,
    Available,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum DrawingToolbarSelection {
    #[default]
    None,
    Drawing,
    Series,
}

impl DrawingToolbarState {
    fn from_chart(chart: &NucleusChartView) -> Self {
        let selection = if chart.selected_drawing_id().is_some() {
            DrawingToolbarSelection::Drawing
        } else if chart.has_deletable_selection() {
            DrawingToolbarSelection::Series
        } else {
            DrawingToolbarSelection::None
        };
        Self {
            availability: DrawingToolbarAvailability::Available,
            active_tool: chart.drawing_tool(),
            drawing_count: chart.drawing_count(),
            selection,
            selected_locked: chart.selected_drawing_locked(),
            all_locked: chart.drawings_lock_summary().all_locked,
        }
    }
}

#[derive(Clone, Copy)]
enum DrawingToolIcon {
    Huge(HugeIcon),
    Asset(assets::DrawingIcon),
}

#[derive(Clone, Copy)]
struct DrawingToolSpec {
    id: &'static str,
    label: &'static str,
    tool: ChartDrawingTool,
    icon: DrawingToolIcon,
    icon_size: f32,
}

const DRAWING_TOOLS: [DrawingToolSpec; 8] = [
    DrawingToolSpec {
        id: "drawing_cursor",
        label: "Cursor",
        tool: ChartDrawingTool::Cursor,
        icon: DrawingToolIcon::Huge(HugeIcon::CursorIcon01),
        icon_size: 24.0,
    },
    DrawingToolSpec {
        id: "drawing_trend_line",
        label: "Trend line",
        tool: ChartDrawingTool::TrendLine,
        icon: DrawingToolIcon::Asset(assets::DrawingIcon::TrendLine),
        icon_size: 28.0,
    },
    DrawingToolSpec {
        id: "drawing_horizontal_line",
        label: "Horizontal line",
        tool: ChartDrawingTool::HorizontalLine,
        icon: DrawingToolIcon::Asset(assets::DrawingIcon::HorizontalLine),
        icon_size: 28.0,
    },
    DrawingToolSpec {
        id: "drawing_vertical_line",
        label: "Vertical line",
        tool: ChartDrawingTool::VerticalLine,
        icon: DrawingToolIcon::Asset(assets::DrawingIcon::VerticalLine),
        icon_size: 28.0,
    },
    DrawingToolSpec {
        id: "drawing_ray",
        label: "Ray",
        tool: ChartDrawingTool::Ray,
        icon: DrawingToolIcon::Asset(assets::DrawingIcon::Ray),
        icon_size: 28.0,
    },
    DrawingToolSpec {
        id: "drawing_rectangle",
        label: "Rectangle",
        tool: ChartDrawingTool::Rectangle,
        icon: DrawingToolIcon::Asset(assets::DrawingIcon::Rectangle),
        icon_size: 28.0,
    },
    DrawingToolSpec {
        id: "drawing_brush",
        label: "Brush",
        tool: ChartDrawingTool::Brush,
        icon: DrawingToolIcon::Huge(HugeIcon::Brush),
        icon_size: 24.0,
    },
    DrawingToolSpec {
        id: "drawing_text",
        label: "Text",
        tool: ChartDrawingTool::Text,
        icon: DrawingToolIcon::Huge(HugeIcon::Text),
        icon_size: 24.0,
    },
];

fn drawing_toolbar(
    terminal: Entity<TerminalApp>,
    app: &Entity<WorkspaceSurface>,
    state: DrawingToolbarState,
    scroll: &ScrollHandle,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let tool_app = app.clone();
    let tools = DRAWING_TOOLS.into_iter().map(move |spec| {
        let app = tool_app.clone();
        let enabled = state.availability == DrawingToolbarAvailability::Available;
        let button = drawing_toolbar_action(
            drawing_toolbar_button(
                spec.id,
                spec.icon,
                spec.label,
                spec.icon_size,
                theme,
                state.active_tool == spec.tool,
            ),
            enabled,
        );
        chrome_tooltip(
            spec.id,
            spec.label,
            button_activation(button, enabled, move |_, cx| {
                app.update(cx, |app, app_cx| {
                    app.select_drawing_tool(spec.tool, app_cx);
                });
            }),
            theme,
        )
    });
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left_0()
        .w(px(chart_chrome::CHART_CHROME_HEIGHT))
        .flex()
        .flex_col()
        .items_center()
        .overflow_hidden()
        .border_r_1()
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface))
        .child(
            div()
                .relative()
                .size_full()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .py_2()
                        .size_full()
                        .min_h(px(0.0))
                        .map(|body| tracked_overflow_y_scrollbar(body, scroll))
                        .children(tools)
                        .child(drawing_toolbar_actions(terminal, app, state, theme)),
                )
                .child(ThinScrollbar::new(
                    scroll,
                    gpui_color(colors.text_secondary),
                )),
        )
}

fn drawing_toolbar_actions(
    terminal: Entity<TerminalApp>,
    app: &Entity<WorkspaceSurface>,
    state: DrawingToolbarState,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .py_2()
        .w_full()
        .border_t_1()
        .border_color(gpui_color(theme.colors.border))
        .child(drawing_action_control(
            DrawingActionSpec::new(
                "drawing_delete_selected",
                "Delete selected chart object",
                HugeIcon::DeleteIcon02,
                24.0,
                false,
                state.selection != DrawingToolbarSelection::None,
                WorkspaceSurface::remove_selected_chart_object,
            ),
            app.clone(),
            theme,
        ))
        .child(drawing_action_control(
            DrawingActionSpec::new(
                "drawing_lock_selected",
                "Lock or unlock selected drawing",
                HugeIcon::Lock,
                20.0,
                state.selected_locked,
                state.selection == DrawingToolbarSelection::Drawing,
                WorkspaceSurface::toggle_selected_drawing_lock,
            ),
            app.clone(),
            theme,
        ))
        .child(drawing_action_control(
            DrawingActionSpec::new(
                "drawing_lock_all",
                "Lock or unlock all drawings",
                HugeIcon::AiLock,
                20.0,
                state.all_locked,
                state.drawing_count > 0,
                WorkspaceSurface::toggle_all_drawings_lock,
            ),
            app.clone(),
            theme,
        ))
        .child(drawing_action_control(
            DrawingActionSpec::new(
                "drawing_clear_all",
                "Clear all drawings",
                HugeIcon::AiEraser,
                24.0,
                false,
                state.drawing_count > 0,
                WorkspaceSurface::clear_drawings,
            ),
            app.clone(),
            theme,
        ))
        .child(chrome_tooltip(
            "drawing_toolbar_collapse",
            "Collapse drawing toolbar",
            button_activation(
                drawing_toolbar_button(
                    "drawing_toolbar_collapse",
                    DrawingToolIcon::Huge(HugeIcon::ArrowLeftIcon01),
                    "Collapse drawing toolbar",
                    24.0,
                    theme,
                    false,
                ),
                true,
                move |_, cx| terminal.update(cx, TerminalApp::toggle_drawing_toolbar),
            ),
            theme,
        ))
}

#[derive(Clone, Copy)]
struct DrawingActionSpec {
    id: &'static str,
    tooltip: &'static str,
    icon: HugeIcon,
    icon_size: f32,
    selected: bool,
    enabled: bool,
    action: fn(&mut WorkspaceSurface, &mut Context<WorkspaceSurface>),
}

impl DrawingActionSpec {
    const fn new(
        id: &'static str,
        tooltip: &'static str,
        icon: HugeIcon,
        icon_size: f32,
        selected: bool,
        enabled: bool,
        action: fn(&mut WorkspaceSurface, &mut Context<WorkspaceSurface>),
    ) -> Self {
        Self {
            id,
            tooltip,
            icon,
            icon_size,
            selected,
            enabled,
            action,
        }
    }
}

fn drawing_action_control(
    spec: DrawingActionSpec,
    app: Entity<WorkspaceSurface>,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let button = drawing_toolbar_button(
        spec.id,
        DrawingToolIcon::Huge(spec.icon),
        spec.tooltip,
        spec.icon_size,
        theme,
        spec.selected,
    );
    let button = drawing_toolbar_action(button, spec.enabled);
    let button = button_activation(button, spec.enabled, move |_, cx| {
        app.update(cx, spec.action);
    });
    chrome_tooltip(spec.id, spec.tooltip, button, theme)
}

fn drawing_toolbar_expander(
    terminal: Entity<TerminalApp>,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    div()
        .absolute()
        .left_0()
        .bottom_0()
        .border_1()
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface))
        .child(chrome_tooltip(
            "drawing_toolbar_expand",
            "Expand drawing toolbar",
            button_activation(
                drawing_toolbar_button(
                    "drawing_toolbar_expand",
                    DrawingToolIcon::Huge(HugeIcon::ArrowRightIcon01),
                    "Expand drawing toolbar",
                    14.0,
                    theme,
                    false,
                )
                .w(px(24.0))
                .h(px(28.0))
                .cursor_pointer(),
                true,
                move |_, cx| {
                    terminal.update(cx, TerminalApp::toggle_drawing_toolbar);
                },
            ),
            theme,
        ))
}

fn drawing_toolbar_button(
    id: &'static str,
    icon: DrawingToolIcon,
    _tooltip: &'static str,
    icon_size: f32,
    theme: &AxiusflowTheme,
    selected: bool,
) -> Button {
    let icon = match icon {
        DrawingToolIcon::Huge(icon) => header_icon(icon),
        DrawingToolIcon::Asset(icon) => Icon::default().path(icon.path()),
    };
    let button = Button::new(id)
        .icon(icon)
        .compact()
        .with_size(px(icon_size / 0.75))
        .w(px(32.0))
        .h(px(32.0))
        .rounded(px(f32::from(
            chart_chrome::CHART_CONTROL_RADIUS.logical_pixels(),
        )));
    chrome_button_style(button, theme, selected, true)
}

fn drawing_toolbar_action(button: Button, enabled: bool) -> Button {
    button
        .disabled(!enabled)
        .when(enabled, Button::cursor_pointer)
        .when(!enabled, Button::cursor_not_allowed)
}

fn side_panel_header(
    panel: SidePanel,
    app: Entity<WorkspaceSurface>,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    div()
        .h(px(30.0))
        .flex_none()
        .flex()
        .items_center()
        .px_2()
        .border_l_1()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface))
        .text_xs()
        .text_color(gpui_color(colors.text_secondary))
        .child(div().flex_1().child(panel.title().to_uppercase()))
        .child(chrome_tooltip(
            "close_side_panel",
            "Close side panel",
            button_activation(
                chrome_button_style(
                    Button::new("close_side_panel")
                        .icon(header_icon(HugeIcon::CancelIcon01))
                        .compact()
                        .with_size(px(chart_chrome::HEADER_CONTROL_CONTENT_SIZE))
                        .size(px(chart_chrome::CHART_CONTROL_SIZE))
                        .border_0()
                        .cursor_pointer(),
                    theme,
                    false,
                    true,
                ),
                true,
                move |_, cx| {
                    app.update(cx, WorkspaceSurface::close_side_panel);
                },
            ),
            theme,
        ))
}

fn chart_notice(notice: ChartSurfaceNotice, theme: &AxiusflowTheme) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let tone = match notice.tone {
        ChartNoticeTone::Muted => colors.text_secondary,
        ChartNoticeTone::Warning => colors.warning,
        ChartNoticeTone::Loss => colors.danger,
    };
    let loading = notice.label == ChartState::Loading.label();
    let label = div()
        .flex()
        .flex_col()
        .gap_1()
        .px_2()
        .py_1()
        .border_1()
        .rounded(px(f32::from(
            chart_chrome::CHART_SURFACE_RADIUS.logical_pixels(),
        )))
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface.with_alpha(0.94)))
        .text_xs()
        .text_color(gpui_color(tone))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .children(loading.then(|| {
                    Loader::from_path("chart_notice_loader", HugeIcon::Loader.path())
                        .xsmall()
                        .color(gpui_color(tone))
                }))
                .child(notice.label),
        )
        .children((!loading).then_some(notice.detail).flatten().map(|detail| {
            div()
                .text_color(gpui_color(colors.text_secondary))
                .child(detail)
        }));
    match notice.placement {
        ChartNoticePlacement::Center => div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(label),
        ChartNoticePlacement::TopLeft => div().absolute().top_2().left_2().child(label),
    }
}

fn catalog_rejection_message(
    reason: ProviderCatalogRejectionReason,
    command: ProviderCatalogCommand,
    coinbase: bool,
) -> &'static str {
    match reason {
        ProviderCatalogRejectionReason::SearchRejected if coinbase => {
            "Coinbase rejected the market search"
        }
        ProviderCatalogRejectionReason::SearchRejected => "Rithmic Test rejected the symbol search",
        ProviderCatalogRejectionReason::SupersededSearch => {
            "A newer symbol search replaced this one"
        }
        ProviderCatalogRejectionReason::InstrumentUnavailable => {
            "The selected symbol is no longer available"
        }
        ProviderCatalogRejectionReason::SubscriptionRejected if coinbase => {
            "Coinbase rejected the market subscription"
        }
        ProviderCatalogRejectionReason::SubscriptionRejected => {
            "Rithmic Test rejected the market subscription"
        }
        ProviderCatalogRejectionReason::DispatchUnavailable
            if coinbase && command == ProviderCatalogCommand::Search =>
        {
            "The Coinbase search could not be scheduled"
        }
        ProviderCatalogRejectionReason::DispatchUnavailable if coinbase => {
            "The Coinbase selection could not be scheduled"
        }
        ProviderCatalogRejectionReason::DispatchUnavailable
            if command == ProviderCatalogCommand::Search =>
        {
            "The Rithmic search could not be scheduled"
        }
        ProviderCatalogRejectionReason::DispatchUnavailable => {
            "The Rithmic selection could not be scheduled"
        }
        ProviderCatalogRejectionReason::Unspecified if coinbase => {
            "The Coinbase catalog request failed"
        }
        ProviderCatalogRejectionReason::Unspecified => "The Rithmic catalog request failed",
    }
}

fn provider_catalog_event_provider(event: &ProviderCatalogEvent) -> &str {
    match event {
        ProviderCatalogEvent::SearchCompleted(result) => &result.provider,
        ProviderCatalogEvent::SelectionInstalled(instrument) => &instrument.provider,
        ProviderCatalogEvent::CommandRejected { rejection, .. } => &rejection.provider,
    }
}

fn usize_generation(generation: u64) -> Option<std::num::NonZeroUsize> {
    usize::try_from(generation)
        .ok()
        .and_then(std::num::NonZeroUsize::new)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowCommand {
    MaximizeOrRestore,
    ToggleFullscreen,
}

impl WindowCommand {
    fn execute(self, window: &mut Window) {
        match self {
            Self::MaximizeOrRestore if window.is_fullscreen() => window.toggle_fullscreen(),
            Self::MaximizeOrRestore => window.zoom_window(),
            Self::ToggleFullscreen => window.toggle_fullscreen(),
        }
    }
}

fn fullscreen_escape_command(key: &str, is_fullscreen: bool) -> Option<WindowCommand> {
    if key.eq_ignore_ascii_case("escape") && is_fullscreen {
        return Some(WindowCommand::ToggleFullscreen);
    }
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptionPlatform {
    Windows,
    Linux,
    MacOs,
    Other,
}

const fn current_caption_platform() -> CaptionPlatform {
    if cfg!(target_os = "windows") {
        CaptionPlatform::Windows
    } else if cfg!(target_os = "linux") {
        CaptionPlatform::Linux
    } else if cfg!(target_os = "macos") {
        CaptionPlatform::MacOs
    } else {
        CaptionPlatform::Other
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptionPointerOwner {
    Native,
    Application,
    System,
}

const fn caption_pointer_owner(platform: CaptionPlatform) -> CaptionPointerOwner {
    match platform {
        CaptionPlatform::Windows => CaptionPointerOwner::Native,
        CaptionPlatform::Linux | CaptionPlatform::Other => CaptionPointerOwner::Application,
        CaptionPlatform::MacOs => CaptionPointerOwner::System,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptionCommand {
    Minimize,
    MaximizeOrRestore,
    Close,
}

impl CaptionCommand {
    const fn window_control_area(self) -> WindowControlArea {
        match self {
            Self::Minimize => WindowControlArea::Min,
            Self::MaximizeOrRestore => WindowControlArea::Max,
            Self::Close => WindowControlArea::Close,
        }
    }

    fn execute(self, terminal: &Entity<TerminalApp>, window: &mut Window, cx: &mut App) {
        match self {
            Self::Minimize => window.minimize_window(),
            Self::MaximizeOrRestore => window.zoom_window(),
            Self::Close => {
                terminal.update(cx, |terminal, terminal_cx| {
                    terminal.close_window(&CloseWindow, window, terminal_cx);
                });
            }
        }
    }
}

fn caption_keyboard_activates(key: &str) -> bool {
    matches!(key, "enter" | "space")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowMoveGestureEvent {
    Press,
    Move { left_pressed: bool },
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WindowMoveGestureTransition {
    pending: bool,
    start_move: bool,
}

const fn window_move_gesture_transition(
    pending: bool,
    event: WindowMoveGestureEvent,
) -> WindowMoveGestureTransition {
    match event {
        WindowMoveGestureEvent::Press => WindowMoveGestureTransition {
            pending: true,
            start_move: false,
        },
        WindowMoveGestureEvent::Move { left_pressed: true } if pending => {
            WindowMoveGestureTransition {
                pending: false,
                start_move: true,
            }
        }
        WindowMoveGestureEvent::Move { .. } | WindowMoveGestureEvent::Cancel => {
            WindowMoveGestureTransition {
                pending: false,
                start_move: false,
            }
        }
    }
}

const fn workspace_title_bar_visible(is_fullscreen: bool) -> bool {
    !is_fullscreen
}

fn terminal_header(
    terminal: &Entity<TerminalApp>,
    app: &Entity<WorkspaceSurface>,
    state: HeaderState,
) -> impl IntoElement + use<> {
    let theme = state.theme;
    let controls = header_controls(terminal, app, state);
    div()
        .w_full()
        .h(px(theme.dimensions.app_header_height.logical_pixels))
        .flex()
        .items_center()
        .px_3()
        .border_b_1()
        .border_color(gpui_color(theme.colors.border))
        .bg(gpui_color(theme.colors.surface))
        .child(controls)
}

#[derive(Clone, Copy)]
struct WorkspaceTabBarState<'a> {
    workspaces: &'a [WorkspaceTab],
    active: usize,
    enabled: bool,
    error: Option<&'a str>,
    workspace_drag: Option<WorkspaceDragState>,
    lifecycle: LifecyclePresentation,
    lifecycle_error: Option<&'a str>,
    theme: AxiusflowTheme,
}

fn workspace_window_drag_region(
    region: Stateful<Div>,
    terminal: &Entity<TerminalApp>,
) -> Stateful<Div> {
    if current_caption_platform() == CaptionPlatform::Windows {
        return region.window_control_area(WindowControlArea::Drag);
    }

    let press_terminal = terminal.clone();
    let move_terminal = terminal.clone();
    let release_terminal = terminal.clone();
    region
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            press_terminal.update(cx, |terminal, _| {
                terminal.handle_window_move_gesture(WindowMoveGestureEvent::Press, window);
            });
        })
        .on_mouse_move(move |event, window, cx| {
            move_terminal.update(cx, |terminal, _| {
                terminal.handle_window_move_gesture(
                    WindowMoveGestureEvent::Move {
                        left_pressed: event.pressed_button == Some(MouseButton::Left),
                    },
                    window,
                );
            });
        })
        .on_mouse_up(MouseButton::Left, move |_, window, cx| {
            release_terminal.update(cx, |terminal, _| {
                terminal.handle_window_move_gesture(WindowMoveGestureEvent::Cancel, window);
            });
        })
        .on_click(|event, window, _| {
            if event.click_count() > 1 {
                if current_caption_platform() == CaptionPlatform::MacOs {
                    window.titlebar_double_click();
                } else {
                    window.zoom_window();
                }
            }
        })
        .when(
            current_caption_platform() == CaptionPlatform::Linux,
            |region| {
                region.on_mouse_down(MouseButton::Right, |event, window, _| {
                    if window.window_controls().window_menu {
                        window.show_window_menu(event.position);
                    }
                })
            },
        )
}

fn workspace_title_bar(
    terminal: &Entity<TerminalApp>,
    state: &WorkspaceTabBarState<'_>,
    window: &Window,
) -> impl IntoElement + use<> {
    let tabs = workspace_tab_strip(terminal, state);
    let theme = state.theme;
    let drag_region = workspace_window_drag_region(
        div()
            .id("workspace_window_drag_region")
            .h_full()
            .min_w(px(12.0))
            .flex_1(),
        terminal,
    );
    let brand_region = workspace_window_drag_region(
        div()
            .id("workspace_window_brand_region")
            .h_full()
            .flex_none()
            .flex()
            .items_center()
            .pl_4()
            .pr_2()
            .text_sm()
            .child("Axiusflow"),
        terminal,
    );
    div()
        .w_full()
        .h(px(WORKSPACE_TITLE_BAR_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(gpui_color(theme.colors.border_secondary))
        .bg(gpui_color(theme.colors.surface))
        .when(cfg!(target_os = "macos"), |bar| bar.pl(px(80.0)))
        .child(
            div()
                .h_full()
                .min_w_0()
                .flex_1()
                .flex()
                .items_center()
                .overflow_x_hidden()
                .child(brand_region)
                .child(tabs)
                .child(drag_region),
        )
        .child(engine_lifecycle_controls(
            terminal,
            state.lifecycle,
            state.lifecycle_error,
            &theme,
        ))
        .child(workspace_window_controls(terminal, window, &theme))
}

fn workspace_pane_controls(
    terminal: &Entity<TerminalApp>,
    pane_count: usize,
    theme: &AxiusflowTheme,
) -> Div {
    let button = |id: &'static str, icon: HugeIcon, tooltip: &'static str, enabled: bool| {
        let button = Button::new(id)
            .icon(header_icon(icon))
            .tooltip(TooltipSpec::new(tooltip, theme).show_delay(TOOLTIP_OPEN_DELAY))
            .aria_label(tooltip)
            .with_size(px(chart_chrome::HEADER_CONTROL_CONTENT_SIZE))
            .w(px(chart_chrome::CHART_CONTROL_SIZE))
            .disabled(!enabled);
        chrome_button_style(button, theme, false, enabled)
    };
    let can_split = pane_count < MAXIMUM_PANES_PER_WORKSPACE;
    let can_close = pane_count > 1;
    let horizontal_terminal = terminal.clone();
    let vertical_terminal = terminal.clone();
    let close_terminal = terminal.clone();
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(button_activation(
            button(
                "split_pane_horizontal",
                HugeIcon::SplitSideBySide,
                "Split chart side by side",
                can_split,
            ),
            can_split,
            move |window, cx| {
                horizontal_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.split_active_pane(
                        ChartSplitDirection::Horizontal,
                        window,
                        terminal_cx,
                    );
                });
            },
        ))
        .child(button_activation(
            button(
                "split_pane_vertical",
                HugeIcon::SplitStacked,
                "Split chart top and bottom",
                can_split,
            ),
            can_split,
            move |window, cx| {
                vertical_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.split_active_pane(ChartSplitDirection::Vertical, window, terminal_cx);
                });
            },
        ))
        .child(button_activation(
            button(
                "close_pane",
                HugeIcon::CancelIcon01,
                "Close chart pane",
                can_close,
            ),
            can_close,
            move |window, cx| {
                close_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.close_active_pane(&ClosePane, window, terminal_cx);
                });
            },
        ))
}

fn engine_lifecycle_controls(
    terminal: &Entity<TerminalApp>,
    state: LifecyclePresentation,
    error: Option<&str>,
    theme: &AxiusflowTheme,
) -> Div {
    let colors = theme.colors;
    let button = |id: &'static str, label: String| {
        Button::new(id)
            .theme(theme)
            .label(label)
            .with_size(px(chart_chrome::HEADER_CONTROL_CONTENT_SIZE))
            .h(px(chart_chrome::CHART_CONTROL_SIZE))
            .px_2()
            .border_1()
            .border_color(gpui_color(colors.input_border))
            .bg(gpui_color(colors.input_fill))
            .text_color(gpui_color(colors.text_primary))
            .rounded(px(f32::from(
                chart_chrome::SYMBOL_TRIGGER_RADIUS.logical_pixels(),
            )))
            .disabled(state.pending)
    };
    let mode_terminal = terminal.clone();
    let mode_tooltip = error.map_or_else(
        || "Choose what happens to the resident engine when Axiusflow closes".to_string(),
        ToString::to_string,
    );
    let mode = button_activation(
        button("engine_lifetime_mode", state.mode.label().to_string()),
        !state.pending,
        move |_, cx| {
            mode_terminal.update(cx, TerminalApp::cycle_lifetime_mode);
        },
    );
    let mode = chrome_tooltip("engine_lifetime_mode", mode_tooltip, mode, theme);
    let autostart_terminal = terminal.clone();
    let autostart = button(
        "engine_autostart",
        if state.autostart_enabled {
            "Login start on"
        } else {
            "Login start off"
        }
        .to_string(),
    );
    let autostart = button_activation(autostart, !state.pending, move |_, cx| {
        autostart_terminal.update(cx, TerminalApp::toggle_engine_autostart);
    });
    let autostart = chrome_tooltip(
        "engine_autostart",
        "Start the resident engine with this operating-system user session",
        autostart,
        theme,
    );
    let permission_terminal = terminal.clone();
    let permission = button(
        "markets_live_permission",
        if state.markets_live_permitted {
            "Live retention on"
        } else {
            "Live retention off"
        }
        .to_string(),
    );
    let permission = button_activation(permission, !state.pending, move |_, cx| {
        permission_terminal.update(cx, TerminalApp::toggle_markets_live_permission);
    });
    let permission = chrome_tooltip(
        "markets_live_permission",
        "Explicitly permit selected provider sessions to remain live without a desktop",
        permission,
        theme,
    );
    div()
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .children([
            mode.into_any_element(),
            autostart.into_any_element(),
            permission.into_any_element(),
        ])
}

#[derive(Clone, Copy)]
struct CaptionControlSpec {
    id: &'static str,
    icon: HugeIcon,
    label: &'static str,
    command: CaptionCommand,
    tab_index: isize,
    close: bool,
}

fn workspace_caption_control(
    terminal: &Entity<TerminalApp>,
    spec: CaptionControlSpec,
    pointer_owner: CaptionPointerOwner,
    theme: &AxiusflowTheme,
) -> Stateful<Div> {
    let CaptionControlSpec {
        id,
        icon,
        label,
        command,
        tab_index,
        close,
    } = spec;
    let colors = theme.colors;
    let control = div()
        .id(id)
        .w(px(46.0))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .text_color(gpui_color(colors.icon))
        .hover(move |control| {
            if close {
                control
                    .bg(gpui_color(colors.danger))
                    .text_color(gpui_color(colors.danger_foreground))
            } else {
                control.bg(gpui_color(colors.hover_bg))
            }
        })
        .child(header_icon(icon).small());

    match pointer_owner {
        CaptionPointerOwner::Native => control
            .occlude()
            .window_control_area(command.window_control_area()),
        CaptionPointerOwner::Application => {
            let pointer_terminal = terminal.clone();
            let key_terminal = terminal.clone();
            control
                .role(Role::Button)
                .aria_label(label)
                .tab_index(tab_index)
                .focus_visible(move |control| {
                    control.border_2().border_color(gpui_color(colors.primary))
                })
                .on_key_down(move |event, window, cx| {
                    if caption_keyboard_activates(event.keystroke.key.as_str()) {
                        command.execute(&key_terminal, window, cx);
                        cx.stop_propagation();
                    }
                })
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(move |_, window, cx| {
                    command.execute(&pointer_terminal, window, cx);
                    cx.stop_propagation();
                })
        }
        CaptionPointerOwner::System => control,
    }
}

fn workspace_window_controls(
    terminal: &Entity<TerminalApp>,
    window: &Window,
    theme: &AxiusflowTheme,
) -> Div {
    let pointer_owner = caption_pointer_owner(current_caption_platform());
    if pointer_owner == CaptionPointerOwner::System {
        return div().h_full();
    }
    let supported = window.window_controls();
    let minimize = workspace_caption_control(
        terminal,
        CaptionControlSpec {
            id: "workspace_window_minimize",
            icon: HugeIcon::WindowMinimize,
            label: "Minimize window",
            command: CaptionCommand::Minimize,
            tab_index: 0,
            close: false,
        },
        pointer_owner,
        theme,
    );
    let maximize = workspace_caption_control(
        terminal,
        CaptionControlSpec {
            id: "workspace_window_maximize",
            icon: if window.is_maximized() {
                HugeIcon::WindowRestore
            } else {
                HugeIcon::WindowMaximize
            },
            label: if window.is_maximized() {
                "Restore window"
            } else {
                "Maximize window"
            },
            command: CaptionCommand::MaximizeOrRestore,
            tab_index: 1,
            close: false,
        },
        pointer_owner,
        theme,
    );
    let close = workspace_caption_control(
        terminal,
        CaptionControlSpec {
            id: "workspace_window_close",
            icon: HugeIcon::WindowClose,
            label: "Close window",
            command: CaptionCommand::Close,
            tab_index: 2,
            close: true,
        },
        pointer_owner,
        theme,
    );
    div()
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .tab_group()
        .children(supported.minimize.then_some(minimize))
        .children(supported.maximize.then_some(maximize))
        .child(close)
}

fn header_controls(
    terminal: &Entity<TerminalApp>,
    app: &Entity<WorkspaceSurface>,
    state: HeaderState,
) -> impl IntoElement {
    let dom_toggle = side_panel_toggle(
        app.clone(),
        &state.theme,
        SidePanel::Dom,
        state.controls.enabled(HeaderControls::DOM),
        state.dom_visible,
    );
    let (connection_label, connection_color) = connection_presentation(
        state.provider,
        state.connection_state,
        state.chart_state,
        state.delayed,
        state.history_only,
    );
    div()
        .h_full()
        .flex()
        .items_center()
        .gap_2()
        .child(connection_status_indicator(
            connection_label,
            connection_color(&state.theme),
            &state.theme,
        ))
        .child(instrument_selector(
            app.clone(),
            &InstrumentSelectorState {
                label: state.instrument_label,
                instruments: state.instruments,
                input: state.symbol_input,
                selection_pending: state.pending.symbol_selection,
                enabled: state.controls.enabled(HeaderControls::INSTRUMENT),
                provider: state.provider,
                keyboard_selection: 0,
                scroll: state.instrument_scroll,
            },
            &state.theme,
        ))
        .child(series_selector(
            app.clone(),
            state.series_label,
            state.selected_series,
            state.series_message,
            state.pending.series,
            &state.theme,
            state.controls.enabled(HeaderControls::SERIES),
        ))
        .child(workspace_pane_controls(
            terminal,
            state.pane_count,
            &state.theme,
        ))
        .child(panel_toggle(
            PanelToggleState {
                id: "latest_chart",
                label: "Latest",
                icon: HugeIcon::ArrowRightDouble,
                enabled: state.controls.enabled(HeaderControls::LATEST),
                selected: false,
                tooltip: "Return to the latest bar (End)",
                toggle: WorkspaceSurface::scroll_chart_to_latest,
            },
            &state.theme,
            app.clone(),
        ))
        .child(panel_toggle(
            PanelToggleState {
                id: "fit_chart",
                label: "Fit",
                icon: HugeIcon::FitToScreen,
                enabled: state.controls.enabled(HeaderControls::FIT),
                selected: false,
                tooltip: "Fit chart and reset price scales (Home)",
                toggle: WorkspaceSurface::reset_chart_view,
            },
            &state.theme,
            app.clone(),
        ))
        .child(indicator_selector(
            app.clone(),
            state.indicator_input,
            state.indicator_message,
            state.controls.enabled(HeaderControls::FIT),
            &state.theme,
        ))
        .child(dom_toggle)
        .child(theme_toggle(terminal.clone(), &state.theme))
}

fn side_panel_toggle(
    app: Entity<WorkspaceSurface>,
    theme: &AxiusflowTheme,
    panel: SidePanel,
    enabled: bool,
    selected: bool,
) -> AnyElement {
    let (id, icon, toggle) = match panel {
        SidePanel::Dom => (
            "dom_toggle",
            HugeIcon::SidebarRightIcon01,
            WorkspaceSurface::toggle_dom
                as fn(&mut WorkspaceSurface, &mut Context<WorkspaceSurface>),
        ),
    };
    panel_toggle(
        PanelToggleState {
            id,
            label: panel.toggle_label(),
            icon,
            enabled,
            selected,
            tooltip: panel.toggle_tooltip(),
            toggle,
        },
        theme,
        app,
    )
    .into_any_element()
}

fn instrument_selector(
    app: Entity<WorkspaceSurface>,
    state: &InstrumentSelectorState,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let trigger = Button::new("instrument_selector")
        .icon(header_icon(HugeIcon::ExchangeIcon01))
        .loading_icon(header_icon(HugeIcon::Loader))
        .label(state.label.clone())
        .caret(header_icon(HugeIcon::ChevronDown))
        .with_size(px(chart_chrome::HEADER_CONTROL_CONTENT_SIZE))
        .border_1()
        .border_color(gpui_color(theme.colors.input_border))
        .bg(gpui_color(theme.colors.input_fill))
        .text_color(gpui_color(theme.colors.text_primary))
        .h(px(chart_chrome::CHART_CONTROL_SIZE))
        .px_3()
        .rounded(px(f32::from(
            chart_chrome::SYMBOL_TRIGGER_RADIUS.logical_pixels(),
        )))
        .disabled(!state.enabled)
        .when(state.enabled, Button::cursor_pointer)
        .when(!state.enabled, Button::cursor_not_allowed);
    let trigger = trigger.when(!state.enabled, |trigger| {
        trigger.text_color(gpui_color(theme.colors.text_muted))
    });
    chrome_tooltip(
        "instrument_selector",
        format!(
            "Search or select a {} market",
            match state.provider {
                TerminalProvider::Coinbase => "Coinbase spot",
                TerminalProvider::Rithmic => "Rithmic",
            }
        ),
        button_activation(
            trigger.loading(state.selection_pending),
            state.enabled,
            move |window, cx| {
                app.update(cx, |app, app_cx| {
                    app.open_chrome_overlay(ChromeOverlay::Instrument, window, app_cx);
                });
            },
        ),
        theme,
    )
}

fn connection_status_indicator(
    label: String,
    color: ThemeColor,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    chrome_tooltip(
        "connection_status",
        label,
        div()
            .id("connection_status_dot")
            .size(px(7.0))
            .flex_none()
            .rounded_full()
            .bg(gpui_color(color)),
        theme,
    )
}

fn indicator_selector(
    app: Entity<WorkspaceSurface>,
    _input: Entity<InputState>,
    _message: Option<String>,
    enabled: bool,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let trigger = Button::new("indicator_selector")
        .icon(header_icon(HugeIcon::ChartLineDataIcon02))
        .with_size(px(chart_chrome::HEADER_CONTROL_CONTENT_SIZE))
        .w(px(chart_chrome::CHART_CONTROL_SIZE))
        .h(px(chart_chrome::CHART_CONTROL_SIZE))
        .rounded(px(f32::from(
            chart_chrome::CHART_CONTROL_RADIUS.logical_pixels(),
        )))
        .disabled(!enabled)
        .when(enabled, Button::cursor_pointer)
        .when(!enabled, Button::cursor_not_allowed);
    chrome_tooltip(
        "indicator_selector",
        "Indicators",
        button_activation(
            chrome_button_style(trigger, theme, false, enabled),
            enabled,
            move |window, cx| {
                app.update(cx, |app, app_cx| {
                    app.open_chrome_overlay(ChromeOverlay::Indicator, window, app_cx);
                });
            },
        ),
        theme,
    )
}

fn indicator_dialog_content(
    app: &Entity<WorkspaceSurface>,
    input: &Entity<InputState>,
    message: Option<&str>,
    keyboard_selection: usize,
    scroll: &ScrollHandle,
    theme: &AxiusflowTheme,
    cx: &App,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let indicator_specs = chart_chrome::filter_indicator_specs(input.read(cx).value().as_ref());
    let result_count = indicator_specs.len();
    let (status, status_color) = indicator_status(message, &colors);
    let rows = indicator_specs
        .into_iter()
        .enumerate()
        .map(|(index, spec)| {
            let row_app = app.clone();
            let add_app = app.clone();
            let row_input = input.clone();
            let add_input = input.clone();
            let indicator = native_indicator(spec.kind);
            div()
                .id(("indicator_dialog_row", index))
                .min_h(px(52.0))
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .rounded(px(f32::from(
                    chart_chrome::CHART_CONTROL_RADIUS.logical_pixels(),
                )))
                .cursor_pointer()
                .when(keyboard_selection == index, |row| {
                    row.bg(gpui_color(colors.active_bg))
                        .text_color(gpui_color(colors.text_primary))
                })
                .hover(|row| {
                    row.bg(gpui_color(colors.hover_bg))
                        .text_color(gpui_color(colors.text_primary))
                })
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    let added = row_app.update(cx, |app, cx| app.add_indicator(indicator, cx));
                    if added {
                        let _ = &row_input;
                        row_app.update(cx, |app, app_cx| {
                            app.close_chrome_overlay(window, app_cx);
                        });
                    }
                    cx.stop_propagation();
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .flex_1()
                        .child(div().text_sm().child(spec.label))
                        .child(
                            div()
                                .text_xs()
                                .text_color(gpui_color(colors.text_secondary))
                                .child(format!(
                                    "{}  ·  {}  ·  {}",
                                    spec.kind.identifier().to_ascii_uppercase(),
                                    spec.parameter_description,
                                    spec.location_description()
                                )),
                        ),
                )
                .child(button_activation(
                    chrome_button_style(
                        Button::new(("add_indicator", index))
                            .icon(header_icon(HugeIcon::AddIcon01))
                            .border_0()
                            .compact()
                            .cursor_pointer()
                            .tab_stop(false),
                        theme,
                        false,
                        true,
                    ),
                    true,
                    move |window, cx| {
                        let added = add_app.update(cx, |app, cx| app.add_indicator(indicator, cx));
                        if added {
                            let _ = &add_input;
                            add_app.update(cx, |app, app_cx| {
                                app.close_chrome_overlay(window, app_cx);
                            });
                        }
                    },
                ))
        });
    chrome_menu_surface(&colors)
        .child(chrome_menu_search_header(input, &colors))
        .child(indicator_status_bar(
            result_count,
            status,
            status_color,
            &colors,
        ))
        .child(scrollable_menu_body(
            chrome_menu_scroll_body().children(rows),
            scroll,
            colors.text_secondary,
        ))
        .child(indicator_dialog_footer(&colors))
}

fn indicator_status(
    message: Option<&str>,
    colors: &axiusflow_design_system::ThemeColors,
) -> (String, ThemeColor) {
    message.map_or_else(
        || ("OHLC-compatible".to_string(), colors.text_secondary),
        |message| (message.to_string(), colors.danger),
    )
}

fn indicator_status_bar(
    result_count: usize,
    status: String,
    status_color: ThemeColor,
    colors: &axiusflow_design_system::ThemeColors,
) -> impl IntoElement + use<> {
    div()
        .h(px(34.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .text_xs()
        .text_color(gpui_color(colors.text_secondary))
        .child(format!("{result_count} native indicators"))
        .child(div().text_color(gpui_color(status_color)).child(status))
}

fn indicator_dialog_footer(
    colors: &axiusflow_design_system::ThemeColors,
) -> impl IntoElement + use<> {
    chrome_menu_footer(colors)
        .child("Enter Add  ·  Esc Close")
        .child("Publisher: Native")
}

fn chrome_menu_search_header(
    input: &Entity<InputState>,
    colors: &axiusflow_design_system::ThemeColors,
) -> Div {
    div()
        .h(px(chart_chrome::CHART_CHROME_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .child(header_icon(HugeIcon::SearchIcon01))
        .child(
            Input::new(input)
                .appearance(false)
                .bordered(false)
                .focus_bordered(false)
                .flex_1(),
        )
}

const fn native_indicator(kind: chart_chrome::IndicatorKind) -> ChartIndicator {
    match kind {
        chart_chrome::IndicatorKind::Sma => ChartIndicator::Sma,
        chart_chrome::IndicatorKind::Ema => ChartIndicator::Ema,
        chart_chrome::IndicatorKind::Wma => ChartIndicator::Wma,
        chart_chrome::IndicatorKind::BollingerBands => ChartIndicator::Bollinger,
        chart_chrome::IndicatorKind::Vwap => ChartIndicator::Vwap,
        chart_chrome::IndicatorKind::Volume => ChartIndicator::Volume,
        chart_chrome::IndicatorKind::Rsi => ChartIndicator::Rsi,
        chart_chrome::IndicatorKind::Macd => ChartIndicator::Macd,
        chart_chrome::IndicatorKind::Stochastic => ChartIndicator::Stochastic,
        chart_chrome::IndicatorKind::Atr => ChartIndicator::Atr,
    }
}

struct InstrumentSelectorState {
    label: String,
    instruments: Vec<InstrumentMenuEntry>,
    input: Option<Entity<InputState>>,
    selection_pending: bool,
    enabled: bool,
    provider: TerminalProvider,
    keyboard_selection: usize,
    scroll: ScrollHandle,
}

fn instrument_dialog_content(
    app: &Entity<WorkspaceSurface>,
    state: &InstrumentSelectorState,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let header = instrument_dialog_header(state, theme);
    let rows = state
        .instruments
        .iter()
        .enumerate()
        .map(|(index, instrument)| {
            let checked = instrument.checked;
            let app = app.clone();
            let symbol = instrument.symbol.clone();
            let selection = instrument.selection;
            div()
                .id(("instrument_dialog_row", index))
                .min_h(px(48.0))
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                .text_sm()
                .when(state.keyboard_selection == index || checked, |row| {
                    row.bg(gpui_color(colors.active_bg))
                        .text_color(gpui_color(colors.text_primary))
                })
                .when(!state.selection_pending, |row| {
                    row.cursor_pointer()
                        .hover(|row| {
                            row.bg(gpui_color(colors.hover_bg))
                                .text_color(gpui_color(colors.text_primary))
                        })
                        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                            let dispatched =
                                app.update(cx, |app, cx| app.select_instrument(selection, cx));
                            if dispatched {
                                app.update(cx, |app, app_cx| {
                                    app.close_chrome_overlay(window, app_cx);
                                });
                            }
                            cx.stop_propagation();
                        })
                })
                .when(state.selection_pending, gpui::Styled::cursor_not_allowed)
                .child(
                    div()
                        .size(px(24.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(gpui_color(colors.surface_secondary))
                        .child(header_icon(HugeIcon::ExchangeIcon01)),
                )
                .child(div().flex_1().text_sm().child(symbol))
                .children(
                    checked
                        .then(|| header_icon(HugeIcon::CheckmarkCircleIcon01).into_any_element()),
                )
        });
    chrome_menu_surface(&colors)
        .child(header)
        .child(scrollable_menu_body(
            chrome_menu_scroll_body().children(rows),
            &state.scroll,
            colors.text_secondary,
        ))
        .child(instrument_dialog_footer(&colors, state.provider))
}

fn instrument_dialog_footer(
    colors: &axiusflow_design_system::ThemeColors,
    provider: TerminalProvider,
) -> impl IntoElement + use<> {
    chrome_menu_footer(colors)
        .child("Enter Search  ·  Esc Close")
        .child(match provider {
            TerminalProvider::Coinbase => "Coinbase public spot catalog",
            TerminalProvider::Rithmic => "Rithmic Test catalog",
        })
}

fn instrument_dialog_header(state: &InstrumentSelectorState, theme: &AxiusflowTheme) -> AnyElement {
    let Some(input) = state.input.as_ref() else {
        return div().into_any_element();
    };
    chrome_menu_search_header(input, &theme.colors).into_any_element()
}

const CHROME_MENU_WIDTH: f32 = 720.0;
const CHROME_MENU_HEIGHT: f32 = chart_chrome::CHART_CHROME_HEIGHT + 480.0 + 40.0;

fn chrome_menu_surface(colors: &axiusflow_design_system::ThemeColors) -> Div {
    div()
        .flex()
        .flex_col()
        .w(px(CHROME_MENU_WIDTH))
        .h(px(CHROME_MENU_HEIGHT))
        .overflow_hidden()
        .bg(gpui_color(colors.surface))
        .text_color(gpui_color(colors.text_secondary))
}

fn chrome_menu_scroll_body() -> Div {
    div().flex().flex_col().flex_1().min_h_0().gap_1().p_2()
}

fn scrollable_menu_body(
    body: Div,
    scroll: &ScrollHandle,
    color: ThemeColor,
) -> impl IntoElement + use<> {
    div()
        .relative()
        .flex_1()
        .min_h_0()
        .child(tracked_overflow_y_scrollbar(body, scroll))
        .child(ThinScrollbar::new(scroll, gpui_color(color)))
}

fn chrome_menu_footer(colors: &axiusflow_design_system::ThemeColors) -> Div {
    div()
        .h(px(40.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .border_t_1()
        .border_color(gpui_color(colors.border))
        .text_xs()
        .text_color(gpui_color(colors.text_secondary))
}

#[derive(Clone, Copy)]
struct PanelToggleState {
    id: &'static str,
    label: &'static str,
    icon: HugeIcon,
    enabled: bool,
    selected: bool,
    tooltip: &'static str,
    toggle: fn(&mut WorkspaceSurface, &mut Context<WorkspaceSurface>),
}

fn panel_toggle(
    state: PanelToggleState,
    theme: &AxiusflowTheme,
    app: Entity<WorkspaceSurface>,
) -> impl IntoElement {
    let button = Button::new(state.id)
        .icon(header_icon(state.icon))
        .label(state.label)
        .with_size(px(chart_chrome::HEADER_CONTROL_CONTENT_SIZE))
        .disabled(!state.enabled)
        .when(state.enabled, Button::cursor_pointer)
        .when(!state.enabled, Button::cursor_not_allowed);
    let button = button_activation(button, state.enabled, move |_, cx| {
        app.update(cx, state.toggle);
    });
    chrome_tooltip(
        state.id,
        state.tooltip,
        chrome_button_style(button, theme, state.selected, state.enabled),
        theme,
    )
}

fn theme_toggle(terminal: Entity<TerminalApp>, theme: &AxiusflowTheme) -> impl IntoElement + use<> {
    let next = theme.mode.toggled();
    let icon = match next {
        axiusflow_design_system::ThemeMode::Light => HugeIcon::SunIcon03,
        axiusflow_design_system::ThemeMode::Dark => HugeIcon::MoonIcon02,
    };
    let button = Button::new("theme_toggle")
        .tab_index(0)
        .icon(header_icon(icon))
        .with_size(px(chart_chrome::HEADER_CONTROL_CONTENT_SIZE))
        .w(px(chart_chrome::CHART_CONTROL_SIZE))
        .cursor_pointer();
    let button = button_activation(button, true, move |window, cx| {
        terminal.update(cx, |terminal, cx| terminal.toggle_theme(window, cx));
    });
    chrome_tooltip(
        "theme_toggle",
        format!("Switch to {} theme", next.label()),
        chrome_button_style(button, theme, false, true),
        theme,
    )
}

fn header_icon(name: HugeIcon) -> Icon {
    Icon::default().path(name.path())
}

fn chrome_tooltip(
    id: &'static str,
    label: impl Into<gpui::SharedString>,
    trigger: impl IntoElement + 'static,
    theme: &AxiusflowTheme,
) -> AnyElement {
    with_tooltip(
        (id, usize::MAX),
        trigger,
        &TooltipSpec::new(label, theme).show_delay(TOOLTIP_OPEN_DELAY),
    )
    .into_any_element()
}

fn series_selector(
    app: Entity<WorkspaceSurface>,
    label: String,
    _selected: Option<rithmic_history::RithmicSeries>,
    _message: String,
    pending: bool,
    theme: &AxiusflowTheme,
    enabled: bool,
) -> impl IntoElement {
    let button = Button::new("series_selector")
        .label(label)
        .loading_icon(header_icon(HugeIcon::Loader))
        .caret(header_icon(HugeIcon::ChevronDown))
        .disabled(!enabled)
        .loading(pending)
        .when(enabled, Button::cursor_pointer)
        .when(!enabled, Button::cursor_not_allowed);
    let open_app = app.clone();
    let bounds_app = app;
    let button = button_activation(
        chrome_button_style(button, theme, false, enabled),
        enabled && !pending,
        move |window, cx| {
            open_app.update(cx, |app, app_cx| {
                app.open_chrome_overlay(ChromeOverlay::Timeframe, window, app_cx);
            });
        },
    );
    let trigger = div().relative().flex_none().child(button).child(
        canvas(
            move |bounds, _, cx| {
                bounds_app.update(cx, |app, app_cx| {
                    if app.timeframe_trigger_bounds == Some(bounds) {
                        return;
                    }
                    app.timeframe_trigger_bounds = Some(bounds);
                    if app.chrome_overlay == Some(ChromeOverlay::Timeframe) {
                        app_cx.notify();
                    }
                });
            },
            |_, (), _, _| {},
        )
        .absolute()
        .inset_0(),
    );
    chrome_tooltip("series_selector", "Select chart timeframe", trigger, theme)
}

fn chrome_button_style(
    button: Button,
    theme: &AxiusflowTheme,
    selected: bool,
    enabled: bool,
) -> Button {
    let colors = theme.colors;
    button
        .theme(theme)
        .selected(selected)
        .h(px(chart_chrome::CHART_CONTROL_SIZE))
        .border_0()
        .text_color(gpui_color(chrome_control_foreground(
            &colors, selected, enabled,
        )))
        .when(selected, |button| button.bg(gpui_color(colors.active_bg)))
}

fn chrome_control_foreground(
    colors: &axiusflow_design_system::ThemeColors,
    selected: bool,
    enabled: bool,
) -> ThemeColor {
    if !enabled {
        colors.text_muted
    } else if selected {
        colors.icon_active
    } else {
        colors.icon
    }
}

fn button_activation(
    button: Button,
    enabled: bool,
    handler: impl Fn(&mut Window, &mut App) + 'static,
) -> Button {
    button.when(enabled, |button| {
        button.on_click(move |_, window, cx| {
            handler(window, cx);
            cx.stop_propagation();
        })
    })
}

type ConnectionColor = fn(&AxiusflowTheme) -> ThemeColor;

fn connection_presentation(
    provider: TerminalProvider,
    state: FeedConnectionState,
    chart_state: ChartState,
    delayed: bool,
    history_only: bool,
) -> (String, ConnectionColor) {
    let provider = match provider {
        TerminalProvider::Coinbase => "Coinbase",
        TerminalProvider::Rithmic => "Test",
    };
    if history_only {
        return match chart_state {
            ChartState::Ready => (format!("{provider} · Completed history"), |theme| {
                theme.colors.positive
            }),
            ChartState::Error => (format!("{provider} · Data error"), |theme| {
                theme.colors.danger
            }),
            ChartState::Loading | ChartState::Stale | ChartState::Recovering => {
                (format!("{provider} · Loading history"), |theme| {
                    theme.colors.warning
                })
            }
        };
    }
    if chart_state == ChartState::Stale {
        return (format!("{provider} · Stale"), |theme| theme.colors.warning);
    }
    if chart_state == ChartState::Recovering {
        return (format!("{provider} · Reconnecting"), |theme| {
            theme.colors.warning
        });
    }
    if chart_state == ChartState::Error && state == FeedConnectionState::Streaming {
        return (format!("{provider} · Data error"), |theme| {
            theme.colors.danger
        });
    }
    if state == FeedConnectionState::Streaming && delayed {
        return (format!("{provider} · Delayed"), |theme| {
            theme.colors.warning
        });
    }
    match state {
        FeedConnectionState::Disconnected => ("Offline".to_string(), |theme| theme.colors.danger),
        FeedConnectionState::Discovering => (format!("{provider} · Discovering"), |theme| {
            theme.colors.primary
        }),
        FeedConnectionState::Authenticating => (format!("{provider} · Authenticating"), |theme| {
            theme.colors.primary
        }),
        FeedConnectionState::Streaming => {
            (format!("{provider} · Live"), |theme| theme.colors.positive)
        }
        FeedConnectionState::Recovering => (format!("{provider} · Reconnecting"), |theme| {
            theme.colors.warning
        }),
        FeedConnectionState::Stopped => ("Stopped".to_string(), |theme| theme.colors.danger),
    }
}

const fn nucleus_chart_theme(mode: ThemeMode) -> NucleusChartTheme {
    match mode {
        ThemeMode::Light => NucleusChartTheme::Light,
        ThemeMode::Dark => NucleusChartTheme::Dark,
    }
}

fn gpui_color(color: ThemeColor) -> Hsla {
    let mut resolved: Hsla = rgb(color.rgb_u32()).into();
    resolved.a = color.alpha();
    resolved
}

#[cfg(feature = "diagnostics")]
fn run_desktop_readiness_command(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let usage = "usage: axiusflow_desktop --desktop-readiness <report-path>";
    let report_path = arguments.next().ok_or_else(|| usage.to_string())?;
    if arguments.next().is_some() {
        return Err(usage.to_string());
    }
    readiness_conformance::run(std::path::Path::new(&report_path))
        .map_err(|error| error.to_string())
}

#[cfg(feature = "diagnostics")]
fn run_desktop_endurance_command(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let usage = "usage: axiusflow_desktop --desktop-endurance <report-path> <duration-seconds>";
    let report_path = arguments.next().ok_or_else(|| usage.to_string())?;
    let duration_seconds = arguments
        .next()
        .ok_or_else(|| usage.to_string())?
        .to_string_lossy()
        .parse::<u64>()
        .map_err(|_| usage.to_string())?;
    if arguments.next().is_some() {
        return Err(usage.to_string());
    }
    readiness_conformance::run_endurance(
        std::path::Path::new(&report_path),
        std::time::Duration::from_secs(duration_seconds),
    )
    .map_err(|error| error.to_string())
}

fn symbol_input_for_startup(
    startup: &MarketWorkerStartup,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    match startup {
        MarketWorkerStartup::Rithmic => {
            cx.new(|cx| InputState::new(window, cx).placeholder("Search Rithmic symbols"))
        }
        MarketWorkerStartup::Loading(_) => {
            cx.new(|cx| InputState::new(window, cx).placeholder("Search Coinbase spot markets"))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SymbolInputAction {
    Search,
    Submit,
    Ignore,
}

const fn symbol_input_action(event: &InputEvent) -> SymbolInputAction {
    match event {
        InputEvent::Change => SymbolInputAction::Search,
        InputEvent::PressEnter { .. } => SymbolInputAction::Submit,
        InputEvent::Focus | InputEvent::Blur => SymbolInputAction::Ignore,
    }
}

fn desktop_window_options(window_index: usize, cx: &mut App) -> WindowOptions {
    let mut bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);
    let offset = if window_index == 0 { px(0.0) } else { px(48.0) };
    bounds.origin.x += offset;
    bounds.origin.y += offset;
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(point(px(9.0), px(9.0))),
        }),
        app_owns_titlebar_drag: true,
        ..Default::default()
    }
}

fn subscribe_symbol_input(
    input: Option<Entity<InputState>>,
    terminal: &Entity<WorkspaceSurface>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(input) = input else {
        return;
    };
    let terminal = terminal.clone();
    window
        .subscribe(&input, cx, move |_, event: &InputEvent, window, cx| {
            #[cfg(feature = "diagnostics")]
            let started = Instant::now();
            #[cfg(feature = "diagnostics")]
            let measured_submission = match event {
                InputEvent::Change => Some(false),
                InputEvent::PressEnter { .. } => Some(true),
                InputEvent::Focus | InputEvent::Blur => None,
            };
            match symbol_input_action(event) {
                SymbolInputAction::Submit => {
                    let selected = terminal.update(cx, WorkspaceSurface::submit_symbol_input);
                    if selected {
                        terminal.update(cx, |app, app_cx| {
                            app.close_chrome_overlay(window, app_cx);
                        });
                    }
                }
                SymbolInputAction::Search => {
                    terminal.update(cx, |app, cx| {
                        app.chrome_selection = 0;
                        app.search_symbol_input(cx);
                        cx.notify();
                    });
                }
                SymbolInputAction::Ignore => {}
            }
            #[cfg(feature = "diagnostics")]
            if let Some(submitted) = measured_submission {
                terminal.update(cx, |app, _| {
                    app.foreground_interactions
                        .record_symbol_input(submitted, elapsed_nanos(started));
                });
            }
        })
        .detach();
}

fn subscribe_indicator_input(
    input: &Entity<InputState>,
    terminal: &Entity<WorkspaceSurface>,
    window: &mut Window,
    cx: &mut App,
) {
    let terminal = terminal.clone();
    let input = input.clone();
    window
        .subscribe(
            &input.clone(),
            cx,
            move |_, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let query = input.read(cx).value().to_string();
                    let indicator = terminal.update(cx, |app, _| {
                        chart_chrome::filter_indicator_specs(&query)
                            .get(app.chrome_selection)
                            .map(|spec| native_indicator(spec.kind))
                    });
                    if let Some(indicator) = indicator {
                        let added = terminal.update(cx, |app, cx| app.add_indicator(indicator, cx));
                        if added {
                            terminal.update(cx, |app, app_cx| {
                                app.close_chrome_overlay(window, app_cx);
                            });
                        }
                    }
                } else {
                    terminal.update(cx, |app, cx| {
                        app.chrome_selection = 0;
                        cx.notify();
                    });
                }
            },
        )
        .detach();
}

fn workspace_surface_entity(
    bootstrap: MarketWorkerStartup,
    market_worker: MarketDataWorker,
    lifecycle: &DesktopLifecycle,
    window: &mut Window,
    cx: &mut App,
) -> Entity<WorkspaceSurface> {
    let symbol_input = Some(symbol_input_for_startup(&bootstrap, window, cx));
    let search_input = symbol_input.clone();
    let indicator_input =
        cx.new(|cx| InputState::new(window, cx).placeholder("Search native indicators"));
    let indicator_search_input = indicator_input.clone();
    let workspace_lifecycle = lifecycle.clone();
    let workspace = cx.new(move |cx| {
        WorkspaceSurface::new(
            cx,
            bootstrap,
            market_worker,
            workspace_lifecycle,
            symbol_input,
            indicator_input,
        )
    });
    lifecycle.register_terminal(&workspace);
    subscribe_symbol_input(search_input, &workspace, window, cx);
    subscribe_indicator_input(&indicator_search_input, &workspace, window, cx);
    workspace
}

struct WorkspacePane {
    id: u64,
    consumer_id: u64,
    surface: Entity<WorkspaceSurface>,
    focus: FocusHandle,
}

struct WorkspaceTab {
    id: u64,
    label: String,
    panes: Vec<WorkspacePane>,
    active_pane: usize,
    layout: NucleusWorkspace,
    generation: u64,
    focus: FocusHandle,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WorkspaceDragState {
    tab_id: u64,
    cursor_offset_x: f32,
    pointer_x: Option<f32>,
    strip_left: f32,
}

struct TerminalApp {
    workspaces: Vec<WorkspaceTab>,
    active: usize,
    theme: AxiusflowTheme,
    drawing_toolbar: DrawingToolbarVisibility,
    window_active: bool,
    frame_poll_gate: frame_poll_gate::FramePollGate,
    market_frame_wake: UiWake,
    market_wake_listener_started: Option<()>,
    chrome_focus: FocusHandle,
    lifecycle: DesktopLifecycle,
    workspace_factory: Option<engine_market_worker::WorkspaceMarketFactory>,
    workspace_persistence: Option<WorkspaceLayoutPersistence>,
    persisted_layout: Vec<WorkspaceTabState>,
    persisted_active_workspace_id: u64,
    workspace_error: Option<String>,
    workspace_drag: Option<WorkspaceDragState>,
    window_move_pending: bool,
    closing: bool,
}

fn workspace_switch(active: usize, next: usize, workspace_count: usize) -> Option<(usize, usize)> {
    (next != active && next < workspace_count).then_some((active, next))
}

fn wrapped_workspace_index(
    current: usize,
    workspace_count: usize,
    direction: isize,
) -> Option<usize> {
    if workspace_count == 0 || current >= workspace_count {
        return None;
    }
    match direction.cmp(&0) {
        std::cmp::Ordering::Less => Some(if current == 0 {
            workspace_count - 1
        } else {
            current - 1
        }),
        std::cmp::Ordering::Equal => Some(current),
        std::cmp::Ordering::Greater => Some((current + 1) % workspace_count),
    }
}

fn workspace_label(index: usize) -> String {
    format!("Workspace {}", index + 1)
}

fn workspace_series(interval: ChartInterval, instrument: &InstallProviderInstrument) -> SeriesKey {
    let (cadence, cadence_value) = match interval {
        ChartInterval::Week1 => (SeriesCadence::CalendarWeeks, 1),
        _ => match interval.aggregation() {
            ChartAggregation::Trades(value) => (SeriesCadence::Trades, value.get()),
            ChartAggregation::FixedSeconds(value) => (SeriesCadence::FixedSeconds, value.get()),
            ChartAggregation::CalendarMonth => (SeriesCadence::CalendarMonths, 1),
        },
    };
    SeriesKey {
        provider: instrument.provider.clone(),
        instrument_id: instrument.instrument_id.clone(),
        cadence_value,
        definition_revision: 1,
        entitlement_id: instrument.entitlement_id.clone(),
        cadence: cadence as i32,
    }
}

fn workspace_layout_tabs(workspaces: &[WorkspaceTab], cx: &App) -> Vec<WorkspaceTabState> {
    workspaces
        .iter()
        .map(|workspace| {
            let layout = workspace.layout.layout();
            let pane_weights = layout.pane_basis_points();
            WorkspaceTabState {
                workspace_id: workspace.id,
                label: workspace.label.clone(),
                split_axis: layout_root_axis(&layout) as i32,
                panes: workspace
                    .panes
                    .iter()
                    .filter_map(|pane| {
                        let surface = pane.surface.read(cx);
                        let instrument = surface.coinbase_product.clone()?;
                        let viewport =
                            surface
                                .chart
                                .as_ref()
                                .map_or(surface.restored_viewport, |chart| {
                                    let chart = chart.read(cx);
                                    durable_workspace_viewport(
                                        surface.restored_viewport,
                                        chart.has_market_data(),
                                        chart.is_at_latest(),
                                        chart.visible_time_range_unix_nanos(),
                                    )
                                });
                        Some(WorkspacePaneState {
                            pane_id: pane.id,
                            consumer_id: pane.consumer_id,
                            kind: WorkspacePaneKind::Chart as i32,
                            instrument: Some(instrument.clone()),
                            series: Some(workspace_series(surface.coinbase_interval, &instrument)),
                            viewport_start_unix_nanos: viewport.map(|range| range.0),
                            viewport_end_unix_nanos: viewport.map(|range| range.1),
                            size_basis_points: pane_weights
                                .iter()
                                .find_map(|(pane_id, basis)| {
                                    (*pane_id == pane.id).then_some(*basis)
                                })
                                .unwrap_or(1),
                            generation: workspace.generation.max(1),
                        })
                    })
                    .collect(),
                active_pane_id: workspace.panes[workspace.active_pane].id,
                generation: workspace.generation.max(1),
                layout: Some(workspace_layout_state(&layout)),
            }
        })
        .collect()
}

fn layout_root_axis(layout: &ChartWorkspaceLayout) -> WorkspaceSplitAxis {
    match layout {
        ChartWorkspaceLayout::Split { direction, .. } => split_axis(*direction),
        ChartWorkspaceLayout::Pane { .. } => WorkspaceSplitAxis::Horizontal,
    }
}

fn workspace_layout_state(layout: &ChartWorkspaceLayout) -> WorkspaceLayoutState {
    match layout {
        ChartWorkspaceLayout::Pane { pane_id } => WorkspaceLayoutState {
            pane_id: *pane_id,
            split_axis: WorkspaceSplitAxis::Horizontal as i32,
            ratio_basis_points: 0,
            first: None,
            second: None,
        },
        ChartWorkspaceLayout::Split {
            direction,
            ratio,
            first,
            second,
        } => WorkspaceLayoutState {
            pane_id: 0,
            split_axis: split_axis(*direction) as i32,
            ratio_basis_points: (*ratio * 10_000.0)
                .round()
                .clamp(500.0, 9_500.0)
                .to_u32()
                .unwrap_or(5_000),
            first: Some(Box::new(workspace_layout_state(first))),
            second: Some(Box::new(workspace_layout_state(second))),
        },
    }
}

fn chart_workspace_layout(layout: &WorkspaceLayoutState) -> Option<ChartWorkspaceLayout> {
    match (&layout.first, &layout.second) {
        (None, None) if layout.pane_id != 0 => Some(ChartWorkspaceLayout::Pane {
            pane_id: layout.pane_id,
        }),
        (Some(first), Some(second)) if layout.pane_id == 0 => Some(ChartWorkspaceLayout::Split {
            direction: chart_split_direction(WorkspaceSplitAxis::try_from(layout.split_axis).ok()?),
            ratio: f64::from(layout.ratio_basis_points) / 10_000.0,
            first: Box::new(chart_workspace_layout(first)?),
            second: Box::new(chart_workspace_layout(second)?),
        }),
        _ => None,
    }
}

const fn chart_split_direction(axis: WorkspaceSplitAxis) -> ChartSplitDirection {
    match axis {
        WorkspaceSplitAxis::Horizontal => ChartSplitDirection::Horizontal,
        WorkspaceSplitAxis::Vertical => ChartSplitDirection::Vertical,
    }
}

const fn split_axis(direction: ChartSplitDirection) -> WorkspaceSplitAxis {
    match direction {
        ChartSplitDirection::Horizontal => WorkspaceSplitAxis::Horizontal,
        ChartSplitDirection::Vertical => WorkspaceSplitAxis::Vertical,
    }
}

const fn durable_workspace_viewport(
    restored: Option<(i64, i64)>,
    chart_has_market_data: bool,
    chart_is_at_latest: bool,
    current: Option<(i64, i64)>,
) -> Option<(i64, i64)> {
    if !chart_has_market_data {
        restored
    } else if chart_is_at_latest {
        None
    } else {
        current
    }
}

fn reorder_workspace_ids(ids: &mut Vec<u64>, dragged_id: u64, destination_index: usize) -> bool {
    let Some(from) = ids.iter().position(|id| *id == dragged_id) else {
        return false;
    };
    if from == destination_index {
        return false;
    }
    let dragged = ids.remove(from);
    let insertion_index = destination_index.min(ids.len());
    ids.insert(insertion_index, dragged);
    insertion_index != from
}

fn workspace_drag_destination(
    pointer_x: f32,
    strip_left: f32,
    cursor_offset_x: f32,
    workspace_count: usize,
) -> Option<usize> {
    let last = workspace_count.checked_sub(1)?;
    let stride = WORKSPACE_TAB_WIDTH + WORKSPACE_TAB_GAP;
    let dragged_left = pointer_x - cursor_offset_x - strip_left - WORKSPACE_TAB_STRIP_PADDING_LEFT;
    if !dragged_left.is_finite() {
        return None;
    }
    let mut destination = 0;
    let mut boundary = stride / 2.0;
    while destination < last && dragged_left >= boundary {
        destination += 1;
        boundary += stride;
    }
    Some(destination)
}

fn workspace_drag_translation(
    drag: Option<WorkspaceDragState>,
    tab_id: u64,
    index: usize,
) -> Option<f32> {
    let drag = drag.filter(|drag| drag.tab_id == tab_id)?;
    let pointer_x = drag.pointer_x?;
    let index_offset = (0..index).fold(0.0, |offset, _| {
        offset + WORKSPACE_TAB_WIDTH + WORKSPACE_TAB_GAP
    });
    let slot_left = drag.strip_left + WORKSPACE_TAB_STRIP_PADDING_LEFT + index_offset;
    Some(pointer_x - drag.cursor_offset_x - slot_left)
}

fn active_workspace_after_close(ids: &[u64], active_id: u64, closing_id: u64) -> Option<u64> {
    if ids.len() <= 1 || !ids.contains(&closing_id) {
        return None;
    }
    if active_id != closing_id {
        return Some(active_id);
    }
    let closing = ids.iter().position(|id| *id == closing_id)?;
    ids.get(closing + 1)
        .or_else(|| closing.checked_sub(1).and_then(|index| ids.get(index)))
        .copied()
}

impl TerminalApp {
    fn new(
        mut workspaces: Vec<WorkspaceTab>,
        active_workspace_id: Option<u64>,
        workspace_revision: u64,
        layout_generation: u64,
        lifecycle: DesktopLifecycle,
        workspace_factory: Option<engine_market_worker::WorkspaceMarketFactory>,
        cx: &mut Context<Self>,
    ) -> Self {
        let market_frame_wake = UiWake::default();
        for (index, workspace) in workspaces.iter_mut().enumerate() {
            workspace.focus = workspace
                .focus
                .clone()
                .tab_index(isize::try_from(index.saturating_mul(2)).unwrap_or(isize::MAX))
                .tab_stop(true);
        }
        for workspace in &workspaces {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, _| {
                    surface.set_market_message_wake(market_frame_wake.callback());
                });
                cx.observe(&pane.surface, |_, _, cx| cx.notify()).detach();
            }
        }
        let active = active_workspace_id
            .and_then(|id| workspaces.iter().position(|workspace| workspace.id == id))
            .unwrap_or(0);
        for (index, workspace) in workspaces.iter().enumerate() {
            let resource_class = if index == active {
                ConsumerResourceClass::Foreground
            } else {
                ConsumerResourceClass::Background
            };
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, _| {
                    surface.set_market_resource_class(resource_class);
                });
            }
        }
        let workspace_persistence = workspace_factory
            .as_ref()
            .map(|_| WorkspaceLayoutPersistence::new(workspace_revision, layout_generation))
            .transpose()
            .unwrap_or_else(|error| {
                eprintln!("Axiusflow workspace persistence could not start: {error}");
                None
            });
        let persisted_layout = workspace_layout_tabs(&workspaces, cx);
        let persisted_active_workspace_id = workspaces[active].id;
        Self {
            workspaces,
            active,
            theme: AxiusflowTheme::dark(),
            drawing_toolbar: DrawingToolbarVisibility::Expanded,
            window_active: true,
            frame_poll_gate: frame_poll_gate::FramePollGate::default(),
            market_frame_wake,
            market_wake_listener_started: None,
            chrome_focus: cx.focus_handle().tab_stop(true),
            lifecycle,
            workspace_factory,
            workspace_persistence,
            persisted_layout,
            persisted_active_workspace_id,
            workspace_error: None,
            workspace_drag: None,
            window_move_pending: false,
            closing: false,
        }
    }

    fn active_surface(&self) -> Entity<WorkspaceSurface> {
        let workspace = &self.workspaces[self.active];
        workspace.panes[workspace.active_pane].surface.clone()
    }

    fn set_workspace_resource_class(
        &self,
        workspace_index: usize,
        resource_class: ConsumerResourceClass,
        cx: &mut Context<Self>,
    ) {
        if let Some(workspace) = self.workspaces.get(workspace_index) {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, _| {
                    surface.set_market_resource_class(resource_class);
                });
            }
        }
    }

    fn refresh_workspace_focus_order(&mut self) {
        for (index, workspace) in self.workspaces.iter_mut().enumerate() {
            workspace.focus = workspace
                .focus
                .clone()
                .tab_index(isize::try_from(index.saturating_mul(2)).unwrap_or(isize::MAX))
                .tab_stop(true);
        }
    }

    fn select_pane(&mut self, workspace_id: u64, pane_id: u64, cx: &mut Context<Self>) {
        let Some(workspace) = self
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return;
        };
        let Some(index) = workspace.panes.iter().position(|pane| pane.id == pane_id) else {
            return;
        };
        if workspace.active_pane != index {
            workspace.active_pane = index;
            workspace.generation = workspace.generation.saturating_add(1);
            cx.notify();
        }
    }

    fn resize_workspace_split(
        &mut self,
        workspace_id: u64,
        left_pane_id: u64,
        right_pane_id: u64,
        ratio: f64,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return;
        };
        let current = workspace.layout.layout();
        if current
            .boundary_ratio_for_panes(left_pane_id, right_pane_id)
            .is_some_and(|current| (current - ratio).abs() < 0.0001)
        {
            return;
        }
        if workspace
            .layout
            .resize_between(left_pane_id, right_pane_id, ratio)
            .is_err()
        {
            return;
        }
        workspace.generation = workspace.generation.saturating_add(1);
        cx.notify();
    }

    fn persist_workspace_layout_if_changed(&mut self, cx: &App) {
        let Some(persistence) = self.workspace_persistence.as_ref() else {
            return;
        };
        let layout = workspace_layout_tabs(&self.workspaces, cx);
        let active_workspace_id = self.workspaces[self.active].id;
        if layout == self.persisted_layout
            && active_workspace_id == self.persisted_active_workspace_id
        {
            return;
        }
        if let Err(error) = persistence.request(active_workspace_id, layout.clone()) {
            self.workspace_error = Some(error);
            return;
        }
        self.persisted_layout = layout;
        self.persisted_active_workspace_id = active_workspace_id;
    }

    fn select_workspace(&mut self, next: usize, cx: &mut Context<Self>) {
        let Some((previous, next)) = workspace_switch(self.active, next, self.workspaces.len())
        else {
            return;
        };
        self.set_workspace_resource_class(previous, ConsumerResourceClass::Background, cx);
        self.set_workspace_resource_class(next, ConsumerResourceClass::Foreground, cx);
        self.active = next;
        self.workspace_error = None;
        cx.notify();
    }

    fn select_workspace_id(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        if let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == tab_id)
        {
            self.select_workspace(index, cx);
        }
    }

    fn select_and_focus_workspace(
        &mut self,
        next: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if next >= self.workspaces.len() {
            return;
        }
        self.select_workspace(next, cx);
        self.workspaces[next].focus.focus(window, cx);
    }

    fn select_relative_workspace(
        &mut self,
        current: usize,
        direction: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(next) = wrapped_workspace_index(current, self.workspaces.len(), direction) {
            self.select_and_focus_workspace(next, window, cx);
        }
    }

    fn select_next_workspace(
        &mut self,
        _: &SelectNextWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_relative_workspace(self.active, 1, window, cx);
    }

    fn select_previous_workspace(
        &mut self,
        _: &SelectPreviousWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_relative_workspace(self.active, -1, window, cx);
    }

    fn move_active_workspace(
        &mut self,
        direction: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(destination) = self.active.checked_add_signed(direction) else {
            return;
        };
        if destination >= self.workspaces.len() {
            return;
        }
        let active_id = self.workspaces[self.active].id;
        if self.reorder_workspace(active_id, destination, cx) {
            self.workspaces[self.active].focus.focus(window, cx);
        }
    }

    fn move_workspace_left(
        &mut self,
        _: &MoveWorkspaceLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_active_workspace(-1, window, cx);
    }

    fn move_workspace_right(
        &mut self,
        _: &MoveWorkspaceRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_active_workspace(1, window, cx);
    }

    fn close_active_workspace(
        &mut self,
        _: &CloseWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab_id = self.workspaces[self.active].id;
        self.close_workspace(tab_id, window, cx);
    }

    fn reorder_workspace(
        &mut self,
        dragged_id: u64,
        destination_index: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let active_id = self.workspaces[self.active].id;
        let mut ids = self
            .workspaces
            .iter()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>();
        if !reorder_workspace_ids(&mut ids, dragged_id, destination_index) {
            return false;
        }
        self.workspaces.sort_by_key(|workspace| {
            ids.iter()
                .position(|id| *id == workspace.id)
                .unwrap_or(usize::MAX)
        });
        self.active = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == active_id)
            .unwrap_or(0);
        self.refresh_workspace_focus_order();
        cx.notify();
        true
    }

    fn begin_workspace_drag(&mut self, tab_id: u64, cursor_offset_x: f32, cx: &mut Context<Self>) {
        if self
            .workspaces
            .iter()
            .any(|workspace| workspace.id == tab_id)
        {
            self.workspace_drag = Some(WorkspaceDragState {
                tab_id,
                cursor_offset_x: if cursor_offset_x.is_finite() {
                    cursor_offset_x.clamp(0.0, WORKSPACE_TAB_WIDTH)
                } else {
                    WORKSPACE_TAB_WIDTH / 2.0
                },
                pointer_x: None,
                strip_left: 0.0,
            });
            cx.notify();
        }
    }

    fn move_workspace_drag(
        &mut self,
        tab_id: u64,
        pointer_x: f32,
        strip_left: f32,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self
            .workspace_drag
            .as_mut()
            .filter(|drag| drag.tab_id == tab_id)
        else {
            return;
        };
        let cursor_offset_x = drag.cursor_offset_x;
        drag.pointer_x = Some(pointer_x);
        drag.strip_left = strip_left;
        let Some(destination_index) = workspace_drag_destination(
            pointer_x,
            strip_left,
            cursor_offset_x,
            self.workspaces.len(),
        ) else {
            return;
        };
        if !self.reorder_workspace(tab_id, destination_index, cx) {
            cx.notify();
        }
    }

    fn end_workspace_drag(&mut self, cx: &mut Context<Self>) {
        if self.workspace_drag.take().is_some() {
            cx.notify();
        }
    }

    fn close_workspace(&mut self, tab_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self
            .workspaces
            .iter()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>();
        let active_id = self.workspaces[self.active].id;
        if ids.len() == 1 && ids[0] == tab_id {
            self.retire_workspaces(cx);
            window.remove_window();
            return;
        }
        let Some(next_active_id) = active_workspace_after_close(&ids, active_id, tab_id) else {
            return;
        };
        let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == tab_id)
        else {
            return;
        };
        let removed = self.workspaces.remove(index);
        let focus_next_tab = active_id == tab_id || removed.focus.is_focused(window);
        if self
            .workspace_drag
            .is_some_and(|drag| drag.tab_id == tab_id)
        {
            self.workspace_drag = None;
            cx.stop_active_drag(window);
        }
        for pane in removed.panes {
            pane.surface.update(cx, |workspace, workspace_cx| {
                workspace.set_market_resource_class(ConsumerResourceClass::Detached);
                workspace.retire_market_worker(workspace_cx);
            });
        }
        self.active = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == next_active_id)
            .unwrap_or(0);
        self.refresh_workspace_focus_order();
        self.set_workspace_resource_class(self.active, ConsumerResourceClass::Foreground, cx);
        if focus_next_tab {
            self.workspaces[self.active].focus.focus(window, cx);
        }
        self.workspace_error = None;
        cx.notify();
    }

    fn add_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspaces.len() >= MAXIMUM_OPEN_WORKSPACES {
            self.workspace_error = Some(format!(
                "Axiusflow supports at most {MAXIMUM_OPEN_WORKSPACES} open workspaces"
            ));
            cx.notify();
            return;
        }
        let Some(factory) = self.workspace_factory.clone() else {
            return;
        };
        let active = self.active_surface();
        let (product, interval) = {
            let active = active.read(cx);
            let Some(product) = active.coinbase_product.clone() else {
                self.workspace_error =
                    Some("The active workspace has no market to copy".to_string());
                cx.notify();
                return;
            };
            (product, active.coinbase_interval)
        };
        let pane = match factory.create_workspace(product, interval) {
            Ok(worker) => worker,
            Err(error) => {
                self.workspace_error = Some(error);
                cx.notify();
                return;
            }
        };
        let workspace_id = pane.workspace_id;
        let pane_id = pane.pane_id;
        let consumer_id = pane.consumer_id;
        let surface =
            workspace_surface_entity(pane.startup, pane.worker, &self.lifecycle, window, cx);
        surface.update(cx, |workspace, workspace_cx| {
            workspace.apply_theme(&self.theme, workspace_cx);
            workspace.set_market_message_wake(self.market_frame_wake.callback());
            let _ = workspace_cx;
        });
        cx.observe(&surface, |_, _, cx| cx.notify()).detach();
        self.set_workspace_resource_class(self.active, ConsumerResourceClass::Background, cx);
        self.workspaces.push(WorkspaceTab {
            id: workspace_id,
            label: format!("Workspace {workspace_id}"),
            panes: vec![WorkspacePane {
                id: pane_id,
                consumer_id,
                surface,
                focus: cx.focus_handle(),
            }],
            active_pane: 0,
            layout: NucleusWorkspace::new(pane_id, MAXIMUM_PANES_PER_WORKSPACE),
            generation: 1,
            focus: cx.focus_handle(),
        });
        self.refresh_workspace_focus_order();
        self.active = self.workspaces.len() - 1;
        self.workspace_error = None;
        cx.notify();
    }

    fn new_workspace(&mut self, _: &NewWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        self.add_workspace(window, cx);
    }

    fn split_active_pane(
        &mut self,
        split_direction: ChartSplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(factory) = self.workspace_factory.clone() else {
            return;
        };
        let workspace = &self.workspaces[self.active];
        if workspace.panes.len() >= MAXIMUM_PANES_PER_WORKSPACE {
            self.workspace_error = Some(format!(
                "A workspace supports at most {MAXIMUM_PANES_PER_WORKSPACE} panes"
            ));
            cx.notify();
            return;
        }
        let (product, interval) = {
            let source = workspace.panes[workspace.active_pane].surface.read(cx);
            let Some(product) = source.coinbase_product.clone() else {
                self.workspace_error = Some("The active pane has no market to copy".to_string());
                cx.notify();
                return;
            };
            (product, source.coinbase_interval)
        };
        let workspace_id = workspace.id;
        let insertion_index = workspace.active_pane.saturating_add(1);
        let pane = match factory.create_pane(workspace_id, product, interval) {
            Ok(pane) => pane,
            Err(error) => {
                self.workspace_error = Some(error);
                cx.notify();
                return;
            }
        };
        let surface =
            workspace_surface_entity(pane.startup, pane.worker, &self.lifecycle, window, cx);
        surface.update(cx, |surface, surface_cx| {
            surface.apply_theme(&self.theme, surface_cx);
            surface.set_market_resource_class(ConsumerResourceClass::Foreground);
            surface.set_market_message_wake(self.market_frame_wake.callback());
        });
        cx.observe(&surface, |_, _, cx| cx.notify()).detach();
        let workspace = &mut self.workspaces[self.active];
        let source_pane_id = workspace.panes[workspace.active_pane].id;
        if let Err(error) = workspace
            .layout
            .split(source_pane_id, split_direction, pane.pane_id)
        {
            surface.update(cx, |surface, surface_cx| {
                surface.set_market_resource_class(ConsumerResourceClass::Detached);
                surface.retire_market_worker(surface_cx);
            });
            self.workspace_error = Some(error.to_string());
            cx.notify();
            return;
        }
        workspace.panes.insert(
            insertion_index,
            WorkspacePane {
                id: pane.pane_id,
                consumer_id: pane.consumer_id,
                surface,
                focus: cx.focus_handle(),
            },
        );
        workspace.active_pane = insertion_index;
        workspace.generation = workspace.generation.saturating_add(1);
        self.workspace_error = None;
        cx.notify();
    }

    fn split_pane_horizontal(
        &mut self,
        _: &SplitPaneHorizontal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_active_pane(ChartSplitDirection::Horizontal, window, cx);
    }

    fn split_pane_vertical(
        &mut self,
        _: &SplitPaneVertical,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_active_pane(ChartSplitDirection::Vertical, window, cx);
    }

    fn close_active_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = &mut self.workspaces[self.active];
        if workspace.panes.len() == 1 {
            return;
        }
        let removed_pane_id = workspace.panes[workspace.active_pane].id;
        if let Err(error) = workspace.layout.remove(removed_pane_id) {
            self.workspace_error = Some(error.to_string());
            cx.notify();
            return;
        }
        let removed = workspace.panes.remove(workspace.active_pane);
        let pane_order = workspace.layout.layout().pane_ids();
        let recipient_id = pane_order
            .get(
                workspace
                    .active_pane
                    .min(pane_order.len().saturating_sub(1)),
            )
            .copied()
            .unwrap_or(workspace.panes[0].id);
        let recipient = workspace
            .panes
            .iter()
            .position(|pane| pane.id == recipient_id)
            .unwrap_or(0);
        workspace.active_pane = recipient;
        workspace.generation = workspace.generation.saturating_add(1);
        removed.surface.update(cx, |surface, surface_cx| {
            surface.set_market_resource_class(ConsumerResourceClass::Detached);
            surface.retire_market_worker(surface_cx);
        });
        workspace.panes[recipient].focus.focus(window, cx);
        self.workspace_error = None;
        cx.notify();
    }

    fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.theme = self.theme.toggled();
        window.refresh();
        for workspace in &self.workspaces {
            for pane in &workspace.panes {
                pane.surface.update(cx, |workspace, workspace_cx| {
                    workspace.apply_theme(&self.theme, workspace_cx);
                });
            }
        }
        cx.notify();
    }

    fn cycle_lifetime_mode(&mut self, cx: &mut Context<Self>) {
        let current = self.lifecycle.presentation();
        let request = LifecyclePreferenceRequest {
            mode: current.mode.next(current.markets_live_permitted),
            autostart_enabled: current.autostart_enabled,
            markets_live_permitted: current.markets_live_permitted,
        };
        self.request_lifecycle_preferences(request, cx);
    }

    fn toggle_engine_autostart(&mut self, cx: &mut Context<Self>) {
        let current = self.lifecycle.presentation();
        let request = LifecyclePreferenceRequest {
            mode: current.mode,
            autostart_enabled: !current.autostart_enabled,
            markets_live_permitted: current.markets_live_permitted,
        };
        self.request_lifecycle_preferences(request, cx);
    }

    fn toggle_markets_live_permission(&mut self, cx: &mut Context<Self>) {
        let current = self.lifecycle.presentation();
        let permitted = !current.markets_live_permitted;
        let request = LifecyclePreferenceRequest {
            mode: if !permitted && current.mode == DesktopLifetimeMode::KeepMarketsLive {
                DesktopLifetimeMode::KeepEngineWarm
            } else {
                current.mode
            },
            autostart_enabled: current.autostart_enabled,
            markets_live_permitted: permitted,
        };
        self.request_lifecycle_preferences(request, cx);
    }

    fn request_lifecycle_preferences(
        &mut self,
        request: LifecyclePreferenceRequest,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.lifecycle.request_preferences(request) {
            *self.lifecycle.preference_error.borrow_mut() = Some(error);
        }
        cx.notify();
    }

    fn toggle_drawing_toolbar(&mut self, cx: &mut Context<Self>) {
        self.drawing_toolbar.toggle();
        cx.notify();
    }

    fn retire_workspaces<C: gpui::AppContext>(&mut self, cx: &mut C) {
        for workspace in &self.workspaces {
            for pane in &workspace.panes {
                pane.surface.update(cx, |workspace, workspace_cx| {
                    workspace.retire_market_worker(workspace_cx);
                });
            }
        }
    }

    fn claim_close(&mut self, cx: &mut impl gpui::AppContext) -> bool {
        if !claim_once(&mut self.closing) {
            return false;
        }
        self.retire_workspaces(cx);
        true
    }

    fn close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        if self.claim_close(cx) {
            window.remove_window();
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key.as_str() == "tab" {
            if event.keystroke.modifiers.shift {
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
            cx.stop_propagation();
            return;
        }
        if self.workspace_drag.is_some() && event.keystroke.key.as_str() == "escape" {
            cx.stop_active_drag(window);
            self.end_workspace_drag(cx);
            cx.stop_propagation();
            return;
        }
        self.active_surface().update(cx, |workspace, workspace_cx| {
            workspace.on_terminal_key_down(event, window, workspace_cx);
        });
    }

    fn track_window_activation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let window_active = window.is_window_active();
        let became_active = window_active && !self.window_active;
        self.window_active = window_active;
        if !window_active {
            self.handle_window_move_gesture(WindowMoveGestureEvent::Cancel, window);
            for workspace in &self.workspaces {
                for pane in &workspace.panes {
                    pane.surface
                        .update(cx, |surface, _| surface.end_side_panel_resize());
                }
            }
            if self.workspace_drag.is_some() {
                cx.stop_active_drag(window);
                self.end_workspace_drag(cx);
            }
        }
        if became_active {
            self.schedule_market_frame(window, cx);
        }
    }

    fn handle_window_move_gesture(&mut self, event: WindowMoveGestureEvent, window: &mut Window) {
        let transition = window_move_gesture_transition(self.window_move_pending, event);
        self.window_move_pending = transition.pending;
        if transition.start_move {
            window.start_window_move();
        }
    }

    fn schedule_market_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.frame_poll_gate.try_schedule(self.window_active) {
            return;
        }
        let terminal = cx.entity();
        window.on_next_frame(move |window, cx| {
            let diagnostics = terminal.update(cx, |terminal, cx| {
                terminal.frame_poll_gate.complete();
                if terminal.lifecycle.poll_preferences()
                    || terminal.lifecycle.presentation().pending
                {
                    cx.notify();
                }
                if terminal
                    .workspace_persistence
                    .as_ref()
                    .is_some_and(WorkspaceLayoutPersistence::poll)
                {
                    terminal.workspace_error = terminal
                        .workspace_persistence
                        .as_ref()
                        .and_then(WorkspaceLayoutPersistence::error);
                    cx.notify();
                }
                let mut diagnostics = Vec::new();
                for workspace in &terminal.workspaces {
                    for pane in &workspace.panes {
                        let surface = pane.surface.clone();
                        let pending = surface.update(cx, |workspace, workspace_cx| {
                            if workspace.should_poll_market() {
                                workspace.poll_market_worker(workspace_cx);
                            }
                            workspace.pending_ui_diagnostics.take()
                        });
                        if let Some(pending) = pending {
                            diagnostics.push((surface, pending));
                        }
                    }
                }
                terminal.persist_workspace_layout_if_changed(cx);
                diagnostics
            });
            if !diagnostics.is_empty() {
                window.on_next_frame(move |_, cx| {
                    for (workspace, mut diagnostics) in diagnostics {
                        diagnostics.mark_frame_submit();
                        workspace.update(cx, |workspace, _| {
                            workspace
                                .market_worker
                                .send_ui_diagnostics(diagnostics.into_presented());
                        });
                    }
                });
            }
        });
    }
}

fn active_header_state(
    workspace: &WorkspaceSurface,
    pane_count: usize,
    theme: &AxiusflowTheme,
    chart_has_market_data: bool,
    cx: &App,
) -> HeaderState {
    HeaderState {
        theme: *theme,
        pane_count,
        provider: workspace.provider,
        instrument_label: terminal_instrument_label(workspace),
        series_label: if workspace.provider == TerminalProvider::Coinbase {
            workspace.coinbase_interval.label().to_string()
        } else {
            series_selector_label(
                workspace
                    .series_browser
                    .selected()
                    .map(|request| request.series),
                workspace
                    .series_browser
                    .pending()
                    .map(|request| request.series),
            )
        },
        instruments: workspace.instrument_entries(cx),
        selected_series: workspace
            .series_browser
            .selected()
            .map(|request| request.series),
        symbol_input: workspace.symbol_input.clone(),
        indicator_input: workspace.indicator_input.clone(),
        indicator_message: workspace.indicator_message.clone(),
        series_message: workspace.series_message.clone(),
        pending: HeaderPendingState {
            symbol_selection: workspace.symbol_selection_pending,
            series: workspace.series_browser.pending().is_some()
                || workspace.coinbase_switch.is_pending(),
        },
        controls: HeaderControls::from_state(
            workspace.symbol_input.is_some()
                || !workspace.symbol_browser.results().is_empty()
                || !workspace.symbol_browser.results().is_empty(),
            workspace.symbol_browser.selected().is_some() || workspace.coinbase_product.is_some(),
        )
        .with_chart_controls(chart_has_market_data),
        dom_visible: workspace.side_panel == Some(SidePanel::Dom),
        connection_state: workspace
            .connection_state
            .unwrap_or(FeedConnectionState::Disconnected),
        chart_state: workspace.chart_state,
        delayed: false,
        history_only: workspace.provider == TerminalProvider::Coinbase
            && matches!(
                workspace.coinbase_interval,
                ChartInterval::Week1 | ChartInterval::Month1
            ),
        instrument_scroll: workspace.scrolls.instrument.clone(),
    }
}

impl TerminalApp {
    fn rendered_title_bar(
        &self,
        terminal: &Entity<Self>,
        window: &Window,
        fullscreen: bool,
    ) -> Option<impl IntoElement + use<>> {
        workspace_title_bar_visible(fullscreen).then(|| {
            let lifecycle_error = self.lifecycle.preference_error();
            workspace_title_bar(
                terminal,
                &WorkspaceTabBarState {
                    workspaces: &self.workspaces,
                    active: self.active,
                    enabled: self.workspace_factory.is_some(),
                    error: self.workspace_error.as_deref(),
                    workspace_drag: self.workspace_drag,
                    lifecycle: self.lifecycle.presentation(),
                    lifecycle_error: lifecycle_error.as_deref(),
                    theme: self.theme,
                },
                window,
            )
        })
    }
}

fn workspace_pane_grid(
    terminal: &Entity<TerminalApp>,
    workspace: &WorkspaceTab,
    theme: &AxiusflowTheme,
    cx: &App,
) -> AnyElement {
    workspace_layout_element(terminal, workspace, &workspace.layout.layout(), theme, cx)
}

fn workspace_layout_element(
    terminal: &Entity<TerminalApp>,
    workspace: &WorkspaceTab,
    layout: &ChartWorkspaceLayout,
    theme: &AxiusflowTheme,
    cx: &App,
) -> AnyElement {
    if let ChartWorkspaceLayout::Pane { pane_id } = layout {
        return workspace_pane_element(terminal, workspace, *pane_id, theme, cx);
    }

    let ChartWorkspaceLayout::Split {
        direction,
        ratio,
        first,
        second,
    } = layout
    else {
        return div().into_any_element();
    };
    let first_ids = first.pane_ids();
    let second_ids = second.pane_ids();
    let left_pane_id = *first_ids.last().unwrap_or(&0);
    let right_pane_id = *second_ids.first().unwrap_or(&0);
    let first_element = workspace_layout_element(terminal, workspace, first, theme, cx);
    let second_element = workspace_layout_element(terminal, workspace, second, theme, cx);
    let workspace_id = workspace.id;
    let split_id = format!("workspace_split_{workspace_id}_{left_pane_id}_{right_pane_id}");
    let direction = *direction;
    let drag = WorkspaceSplitDrag {
        workspace_id,
        left_pane_id,
        right_pane_id,
        direction,
    };
    let drag_terminal = terminal.clone();
    let handle_drag = drag.clone();
    let handle_id = format!("{split_id}_handle");
    let ratio = ratio.to_f32().unwrap_or(0.5).clamp(0.05, 0.95);
    let first = div()
        .flex_none()
        .min_w_0()
        .min_h_0()
        .when(direction == ChartSplitDirection::Horizontal, |panel| {
            panel.w(relative(ratio)).h_full()
        })
        .when(direction == ChartSplitDirection::Vertical, |panel| {
            panel.h(relative(ratio)).w_full()
        })
        .child(first_element);
    let second = div().flex_1().min_w_0().min_h_0().child(second_element);
    let handle = workspace_split_handle(
        handle_id,
        direction,
        ratio,
        handle_drag,
        gpui_color(theme.colors.border),
    );
    div()
        .id(split_id)
        .relative()
        .size_full()
        .min_w_0()
        .min_h_0()
        .flex()
        .when(direction == ChartSplitDirection::Vertical, |group| {
            group.flex_col()
        })
        .on_drag_move::<WorkspaceSplitDrag>(move |event, _, cx| {
            let drag = event.drag(cx);
            if drag.workspace_id != workspace_id
                || drag.left_pane_id != left_pane_id
                || drag.right_pane_id != right_pane_id
            {
                return;
            }
            let Some(ratio) = workspace_split_ratio(
                drag.direction,
                f32::from(event.event.position.x),
                f32::from(event.event.position.y),
                f32::from(event.bounds.left()),
                f32::from(event.bounds.top()),
                f32::from(event.bounds.size.width),
                f32::from(event.bounds.size.height),
            ) else {
                return;
            };
            drag_terminal.update(cx, |terminal, terminal_cx| {
                terminal.resize_workspace_split(
                    workspace_id,
                    left_pane_id,
                    right_pane_id,
                    ratio,
                    terminal_cx,
                );
            });
        })
        .child(first)
        .child(second)
        .child(handle)
        .into_any_element()
}

fn workspace_split_handle(
    id: String,
    direction: ChartSplitDirection,
    ratio: f32,
    drag: WorkspaceSplitDrag,
    border: Hsla,
) -> impl IntoElement {
    div()
        .id(id)
        .absolute()
        .occlude()
        .when(direction == ChartSplitDirection::Horizontal, |handle| {
            handle
                .top_0()
                .left(relative(ratio))
                .ml(px(-4.0))
                .h_full()
                .w(px(8.0))
                .cursor_col_resize()
        })
        .when(direction == ChartSplitDirection::Vertical, |handle| {
            handle
                .left_0()
                .top(relative(ratio))
                .mt(px(-4.0))
                .w_full()
                .h(px(8.0))
                .cursor_row_resize()
        })
        .on_drag(drag, move |drag, _, _, cx| cx.new(|_| drag.clone()))
        .child(
            div()
                .absolute()
                .bg(border)
                .when(direction == ChartSplitDirection::Horizontal, |line| {
                    line.left(px(3.0)).top_0().h_full().w(px(1.0))
                })
                .when(direction == ChartSplitDirection::Vertical, |line| {
                    line.left_0().top(px(3.0)).w_full().h(px(1.0))
                }),
        )
}

fn workspace_split_ratio(
    direction: ChartSplitDirection,
    pointer_x: f32,
    pointer_y: f32,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
) -> Option<f64> {
    let ratio = match direction {
        ChartSplitDirection::Horizontal if width.is_finite() && width > 0.0 => {
            (pointer_x - left) / width
        }
        ChartSplitDirection::Vertical if height.is_finite() && height > 0.0 => {
            (pointer_y - top) / height
        }
        _ => return None,
    };
    ratio
        .is_finite()
        .then(|| f64::from(ratio.clamp(0.05, 0.95)))
}

fn workspace_pane_element(
    terminal: &Entity<TerminalApp>,
    workspace: &WorkspaceTab,
    pane_id: u64,
    theme: &AxiusflowTheme,
    cx: &App,
) -> AnyElement {
    let Some(pane) = workspace.panes.iter().find(|pane| pane.id == pane_id) else {
        return div().into_any_element();
    };
    let surface = pane.surface.read(cx);
    let connection_state = surface
        .connection_state
        .unwrap_or(FeedConnectionState::Disconnected);
    let chart_has_market_data = surface
        .chart
        .as_ref()
        .is_some_and(|chart| chart.read(cx).has_market_data());
    let content = market_workspace(MarketWorkspaceState {
        app: pane.surface.clone(),
        pane_id,
        chart: surface.chart.as_ref(),
        chart_has_market_data,
        dom: surface.dom.clone(),
        side_panel: surface.side_panel,
        side_panel_width: surface.side_panel_width,
        chart_state: surface.chart_state,
        chart_status_detail: chart_status_detail(
            surface.chart_state,
            connection_state,
            &surface.chart_state_message,
            surface.connection_message.as_deref(),
        )
        .to_string(),
        theme,
    });
    let workspace_id = workspace.id;
    let pane_focus = pane.focus.clone();
    let select_terminal = terminal.clone();
    div()
        .id(("workspace_pane", pane_id))
        .relative()
        .size_full()
        .min_w_0()
        .min_h_0()
        .pr(px(3.0))
        .pb(px(2.0))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            select_terminal.update(cx, |terminal, terminal_cx| {
                terminal.select_pane(workspace_id, pane_id, terminal_cx);
            });
            pane_focus.focus(window, cx);
        })
        .child(content)
        .into_any_element()
}

fn workspace_market_area(
    terminal: &Entity<TerminalApp>,
    workspace: &WorkspaceTab,
    active_surface: &Entity<WorkspaceSurface>,
    drawing_toolbar_collapsed: bool,
    theme: &AxiusflowTheme,
    cx: &App,
) -> impl IntoElement + use<> {
    let grid = workspace_pane_grid(terminal, workspace, theme, cx);
    let surface = active_surface.read(cx);
    let drawing_state = surface.drawing_toolbar_state(cx);
    let drawing_scroll = surface.scrolls.drawing.clone();
    let grid = div()
        .h_full()
        .flex_1()
        .min_w_0()
        .when(!drawing_toolbar_collapsed, |grid| {
            grid.ml(px(chart_chrome::CHART_CHROME_HEIGHT))
        })
        .child(grid);
    div()
        .relative()
        .flex()
        .size_full()
        .overflow_hidden()
        .child(grid)
        .when(drawing_toolbar_collapsed, |market| {
            market.child(drawing_toolbar_expander(terminal.clone(), theme))
        })
        .when(!drawing_toolbar_collapsed, |market| {
            market.child(drawing_toolbar(
                terminal.clone(),
                active_surface,
                drawing_state,
                &drawing_scroll,
                theme,
            ))
        })
}

impl TerminalApp {
    fn start_market_wake_listener(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.market_wake_listener_started.is_some() {
            return;
        }
        self.market_wake_listener_started = Some(());
        let wake = self.market_frame_wake.clone();
        cx.spawn_in(window, async move |terminal, cx| {
            loop {
                wake.notified().await;
                if terminal
                    .update_in(cx, |terminal, window, terminal_cx| {
                        terminal.schedule_market_frame(window, terminal_cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
}

impl Render for TerminalApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.start_market_wake_listener(window, cx);
        if self.workspace_drag.is_some() && !cx.has_active_drag() {
            self.workspace_drag = None;
        }
        self.track_window_activation(window, cx);
        self.schedule_market_frame(window, cx);
        let terminal = cx.entity();
        let pane_count = self.workspaces[self.active].panes.len();
        let active = self.active_surface();
        let workspace = active.read(cx);
        let chart_has_market_data = workspace
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).has_market_data());
        let fullscreen = window.is_fullscreen();
        let overlay = chrome_overlay_layer(
            workspace,
            &active,
            &self.theme,
            chart_chrome::CHART_CHROME_HEIGHT
                + if fullscreen {
                    0.0
                } else {
                    WORKSPACE_TITLE_BAR_HEIGHT
                },
            cx,
        );
        let title_bar = self.rendered_title_bar(&terminal, window, fullscreen);
        let header = terminal_header(
            &terminal,
            &active,
            active_header_state(
                workspace,
                pane_count,
                &self.theme,
                chart_has_market_data,
                cx,
            ),
        );
        let market = workspace_market_area(
            &terminal,
            &self.workspaces[self.active],
            &active,
            self.drawing_toolbar.is_collapsed(),
            &self.theme,
            cx,
        );
        let fullscreen_focus = self.chrome_focus.clone();
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .track_focus(&self.chrome_focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|terminal, _, window, cx| {
                    terminal.handle_window_move_gesture(WindowMoveGestureEvent::Cancel, window);
                    terminal.end_workspace_drag(cx);
                }),
            )
            .on_action(|_: &MinimizeWindow, window, _| window.minimize_window())
            .on_action(|_: &ZoomWindow, window, _| {
                WindowCommand::MaximizeOrRestore.execute(window);
            })
            .map(|root| workspace_action_handlers(root, cx))
            .on_action(move |_: &ToggleFullscreen, window, cx| {
                window.toggle_fullscreen();
                fullscreen_focus.focus(window, cx);
            })
            .on_action(cx.listener(Self::close_window))
            .bg(gpui_color(self.theme.colors.surface))
            .text_color(gpui_color(self.theme.colors.text_primary))
            .children(title_bar)
            .child(header)
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .bg(gpui_color(self.theme.colors.surface))
                    .child(market),
            )
            .children(overlay)
    }
}

fn workspace_action_handlers(root: Div, cx: &mut Context<TerminalApp>) -> Div {
    root.on_action(cx.listener(TerminalApp::new_workspace))
        .on_action(cx.listener(TerminalApp::select_next_workspace))
        .on_action(cx.listener(TerminalApp::select_previous_workspace))
        .on_action(cx.listener(TerminalApp::move_workspace_left))
        .on_action(cx.listener(TerminalApp::move_workspace_right))
        .on_action(cx.listener(TerminalApp::close_active_workspace))
        .on_action(cx.listener(TerminalApp::split_pane_horizontal))
        .on_action(cx.listener(TerminalApp::split_pane_vertical))
        .on_action(cx.listener(TerminalApp::close_active_pane))
}

#[derive(Clone)]
struct WorkspaceTabDrag {
    tab_id: u64,
}

impl Render for WorkspaceTabDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

#[derive(Clone)]
struct WorkspaceSplitDrag {
    workspace_id: u64,
    left_pane_id: u64,
    right_pane_id: u64,
    direction: ChartSplitDirection,
}

impl Render for WorkspaceSplitDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

#[derive(Clone, Copy)]
struct WorkspaceTabRenderState {
    index: usize,
    active: usize,
    workspace_count: usize,
    drag_enabled: bool,
    drag_translation: Option<f32>,
    theme: AxiusflowTheme,
}

fn workspace_tab_close_button(
    terminal: Entity<TerminalApp>,
    tab_id: u64,
    index: usize,
    label: &str,
    theme: &AxiusflowTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let key_terminal = terminal.clone();
    div()
        .id(("close_workspace", tab_id))
        .occlude()
        .size(px(20.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
        .text_color(gpui_color(colors.icon))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(format!("Close {label}"))
        .tab_index(isize::try_from(index.saturating_mul(2).saturating_add(1)).unwrap_or(isize::MAX))
        .hover(move |close| {
            close
                .bg(gpui_color(colors.danger))
                .text_color(gpui_color(colors.danger_foreground))
        })
        .focus_visible(move |close| close.border_2().border_color(gpui_color(colors.primary)))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            terminal.update(cx, |terminal, cx| {
                terminal.close_workspace(tab_id, window, cx);
            });
            cx.stop_propagation();
        })
        .on_key_down(move |event, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                key_terminal.update(cx, |terminal, cx| {
                    terminal.close_workspace(tab_id, window, cx);
                });
                cx.stop_propagation();
            }
        })
        .child(header_icon(HugeIcon::CancelIcon01).size(px(12.0)))
}

fn handle_workspace_tab_key(
    terminal: &Entity<TerminalApp>,
    tab_id: u64,
    index: usize,
    event: &KeyDownEvent,
    window: &mut Window,
    cx: &mut App,
) {
    let key = event.keystroke.key.as_str();
    if !matches!(
        key,
        "left" | "right" | "home" | "end" | "enter" | "space" | "delete"
    ) {
        return;
    }
    terminal.update(cx, |terminal, cx| match key {
        "left" => terminal.select_relative_workspace(index, -1, window, cx),
        "right" => terminal.select_relative_workspace(index, 1, window, cx),
        "home" => terminal.select_and_focus_workspace(0, window, cx),
        "end" => {
            let last = terminal.workspaces.len().saturating_sub(1);
            terminal.select_and_focus_workspace(last, window, cx);
        }
        "enter" | "space" => terminal.select_workspace_id(tab_id, cx),
        "delete" => terminal.close_workspace(tab_id, window, cx),
        _ => {}
    });
    cx.stop_propagation();
}

fn workspace_add_button(
    terminal: Entity<TerminalApp>,
    enabled: bool,
    theme: &AxiusflowTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let key_terminal = terminal.clone();
    div()
        .id("add_workspace")
        .occlude()
        .size(px(24.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
        .text_color(gpui_color(if enabled {
            colors.icon
        } else {
            colors.text_muted
        }))
        .role(Role::Button)
        .aria_label("Create workspace")
        .tab_index(isize::MAX)
        .tab_stop(enabled)
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(move |button| button.bg(gpui_color(colors.hover_bg)))
                .focus_visible(move |button| {
                    button.border_2().border_color(gpui_color(colors.primary))
                })
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    terminal.update(cx, |terminal, cx| terminal.add_workspace(window, cx));
                    cx.stop_propagation();
                })
                .on_key_down(move |event, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        key_terminal.update(cx, |terminal, cx| terminal.add_workspace(window, cx));
                        cx.stop_propagation();
                    }
                })
        })
        .child(header_icon(HugeIcon::AddIcon01).size(px(13.0)))
}

fn workspace_tab(
    terminal: &Entity<TerminalApp>,
    workspace: &WorkspaceTab,
    state: &WorkspaceTabRenderState,
) -> AnyElement {
    let index = state.index;
    let drag_enabled = state.drag_enabled;
    let theme = state.theme;
    let colors = theme.colors;
    let tab_id = workspace.id;
    let selected = index == state.active;
    let select_terminal = terminal.clone();
    let key_terminal = terminal.clone();
    let middle_click_terminal = terminal.clone();
    let drag_terminal = terminal.clone();
    let drag = WorkspaceTabDrag { tab_id };
    let tab_focus = workspace.focus.clone();
    let mouse_focus = workspace.focus.clone();
    div()
        .id(("workspace_tab", tab_id))
        .occlude()
        .w(px(WORKSPACE_TAB_WIDTH))
        .h(px(chart_chrome::CHART_CONTROL_SIZE))
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .pl_3()
        .pr_1()
        .rounded(px(f32::from(
            chart_chrome::SYMBOL_TRIGGER_RADIUS.logical_pixels(),
        )))
        .border_1()
        .border_color(gpui_color(if selected {
            colors.border
        } else {
            colors.surface
        }))
        .bg(gpui_color(if selected {
            colors.active_bg
        } else {
            colors.surface
        }))
        .text_sm()
        .text_color(gpui_color(if selected {
            colors.text_primary
        } else {
            colors.text_secondary
        }))
        .track_focus(&tab_focus)
        .role(Role::Tab)
        .aria_label(workspace.label.clone())
        .aria_selected(selected)
        .aria_position_in_set(index + 1)
        .aria_size_of_set(state.workspace_count)
        .cursor_pointer()
        .when_some(state.drag_translation, |tab, translation| {
            tab.relative().left(px(translation)).shadow_md()
        })
        .hover(move |tab| {
            tab.bg(gpui_color(colors.hover_bg))
                .text_color(gpui_color(colors.text_primary))
        })
        .focus_visible(move |tab| tab.border_color(gpui_color(colors.primary)).border_2())
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            mouse_focus.focus(window, cx);
            select_terminal.update(cx, |terminal, cx| {
                terminal.select_workspace_id(tab_id, cx);
            });
        })
        .on_aux_click(move |event, window, cx| {
            if event.is_middle_click() {
                middle_click_terminal.update(cx, |terminal, cx| {
                    terminal.close_workspace(tab_id, window, cx);
                });
                cx.stop_propagation();
            }
        })
        .on_key_down(move |event, window, cx| {
            handle_workspace_tab_key(&key_terminal, tab_id, index, event, window, cx);
        })
        .when(drag_enabled, |tab| {
            tab.on_drag(drag, move |drag, cursor_offset, _, cx| {
                drag_terminal.update(cx, |terminal, cx| {
                    terminal.begin_workspace_drag(drag.tab_id, f32::from(cursor_offset.x), cx);
                });
                cx.new(|_| drag.clone())
            })
        })
        .child(workspace.label.clone())
        .child(workspace_tab_close_button(
            terminal.clone(),
            tab_id,
            index,
            &workspace.label,
            &theme,
        ))
        .into_any_element()
}

fn workspace_tab_strip(
    terminal: &Entity<TerminalApp>,
    state: &WorkspaceTabBarState<'_>,
) -> impl IntoElement + use<> {
    let workspaces = state.workspaces;
    let enabled = state.enabled;
    let theme = state.theme;
    let colors = theme.colors;
    let tabs = workspaces.iter().enumerate().map(|(index, workspace)| {
        workspace_tab(
            terminal,
            workspace,
            &WorkspaceTabRenderState {
                index,
                active: state.active,
                workspace_count: workspaces.len(),
                drag_enabled: enabled,
                drag_translation: workspace_drag_translation(
                    state.workspace_drag,
                    workspace.id,
                    index,
                ),
                theme,
            },
        )
    });
    let add_terminal = terminal.clone();
    let move_terminal = terminal.clone();
    let end_terminal = terminal.clone();
    let add_enabled = enabled && workspaces.len() < MAXIMUM_OPEN_WORKSPACES;
    div()
        .id("workspace_tab_list")
        .h_full()
        .min_w_0()
        .flex_none()
        .flex()
        .items_center()
        .gap_0p5()
        .pl_2()
        .overflow_x_hidden()
        .role(Role::TabList)
        .aria_label("Workspaces")
        .aria_orientation(Orientation::Horizontal)
        .tab_group()
        .on_drag_move::<WorkspaceTabDrag>(move |event, _, cx| {
            let tab_id = event.drag(cx).tab_id;
            move_terminal.update(cx, |terminal, cx| {
                terminal.move_workspace_drag(
                    tab_id,
                    f32::from(event.event.position.x),
                    f32::from(event.bounds.left()),
                    cx,
                );
            });
        })
        .on_drop(move |_: &WorkspaceTabDrag, _, cx| {
            end_terminal.update(cx, TerminalApp::end_workspace_drag);
        })
        .children(enabled.then_some(tabs).into_iter().flatten())
        .children(enabled.then(|| {
            chrome_tooltip(
                "add_workspace",
                "Create workspace",
                workspace_add_button(add_terminal, add_enabled, &theme),
                &theme,
            )
        }))
        .children(enabled.then(|| {
            div()
                .id("workspace_tab_drop_target")
                .h(px(chart_chrome::CHART_CONTROL_SIZE))
                .min_w(px(16.0))
        }))
        .children(state.error.map(|error| {
            chrome_tooltip(
                "workspace_creation_error",
                error.to_string(),
                div()
                    .size(px(7.0))
                    .rounded_full()
                    .bg(gpui_color(colors.danger)),
                &theme,
            )
        }))
}

fn terminal_root(
    bootstrap: MarketWorkerStartup,
    market_worker: MarketDataWorker,
    lifecycle: &DesktopLifecycle,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TerminalApp> {
    let surface = workspace_surface_entity(bootstrap, market_worker, lifecycle, window, cx);
    terminal_shell_root(
        TerminalShellInit {
            workspaces: vec![WorkspaceTab {
                id: 1,
                label: workspace_label(0),
                panes: vec![WorkspacePane {
                    id: 1,
                    consumer_id: 1,
                    surface,
                    focus: cx.focus_handle(),
                }],
                active_pane: 0,
                layout: NucleusWorkspace::new(1, MAXIMUM_PANES_PER_WORKSPACE),
                generation: 1,
                focus: cx.focus_handle(),
            }],
            active_workspace_id: Some(1),
            workspace_revision: 0,
            layout_generation: 1,
            workspace_factory: None,
        },
        lifecycle,
        window,
        cx,
    )
}

struct TerminalShellInit {
    workspaces: Vec<WorkspaceTab>,
    active_workspace_id: Option<u64>,
    workspace_revision: u64,
    layout_generation: u64,
    workspace_factory: Option<engine_market_worker::WorkspaceMarketFactory>,
}

fn terminal_shell_root(
    init: TerminalShellInit,
    lifecycle: &DesktopLifecycle,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TerminalApp> {
    let terminal_lifecycle = lifecycle.clone();
    let terminal = cx.new(move |cx| {
        TerminalApp::new(
            init.workspaces,
            init.active_workspace_id,
            init.workspace_revision,
            init.layout_generation,
            terminal_lifecycle,
            init.workspace_factory,
            cx,
        )
    });
    let closing_terminal = terminal.clone();
    window.on_window_should_close(cx, move |_, cx| {
        closing_terminal.update(cx, |terminal, cx| {
            terminal.claim_close(cx);
        });
        true
    });
    let focus_terminal = terminal.clone();
    window.on_next_frame(move |window, cx| {
        focus_terminal.update(cx, |terminal, cx| {
            terminal.chrome_focus.focus(window, cx);
        });
    });
    terminal
}

fn workspace_tabs_root(
    mut market_panes: Vec<engine_market_worker::WorkspaceMarketPane>,
    restored: &WorkspaceState,
    workspace_factory: engine_market_worker::WorkspaceMarketFactory,
    lifecycle: &DesktopLifecycle,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TerminalApp> {
    let mut workspaces = Vec::with_capacity(restored.workspace_tabs.len());
    for tab in &restored.workspace_tabs {
        let mut panes = Vec::with_capacity(tab.panes.len());
        for persisted in &tab.panes {
            let Some(index) = market_panes.iter().position(|pane| {
                pane.workspace_id == tab.workspace_id && pane.pane_id == persisted.pane_id
            }) else {
                continue;
            };
            let pane = market_panes.remove(index);
            panes.push(WorkspacePane {
                id: pane.pane_id,
                consumer_id: pane.consumer_id,
                surface: workspace_surface_entity(pane.startup, pane.worker, lifecycle, window, cx),
                focus: cx.focus_handle(),
            });
        }
        if panes.is_empty() {
            continue;
        }
        let Some(layout) = tab.layout.as_ref().and_then(chart_workspace_layout) else {
            continue;
        };
        let pane_order = layout.pane_ids();
        panes.sort_by_key(|pane| {
            pane_order
                .iter()
                .position(|pane_id| *pane_id == pane.id)
                .unwrap_or(usize::MAX)
        });
        let active_pane = panes
            .iter()
            .position(|pane| pane.id == tab.active_pane_id)
            .unwrap_or(0);
        let Ok(layout) = NucleusWorkspace::restore(&layout, MAXIMUM_PANES_PER_WORKSPACE) else {
            continue;
        };
        workspaces.push(WorkspaceTab {
            id: tab.workspace_id,
            label: tab.label.clone(),
            panes,
            active_pane,
            layout,
            generation: tab.generation,
            focus: cx.focus_handle(),
        });
    }
    terminal_shell_root(
        TerminalShellInit {
            workspaces,
            active_workspace_id: Some(restored.active_workspace_id),
            workspace_revision: restored.workspace_revision,
            layout_generation: restored.layout_generation,
            workspace_factory: Some(workspace_factory),
        },
        lifecycle,
        window,
        cx,
    )
}

struct ConfiguredDesktop {
    market_workers: Vec<(MarketWorkerStartup, MarketDataWorker)>,
    workspace_panes: Vec<engine_market_worker::WorkspaceMarketPane>,
    restored_workspace: WorkspaceState,
    workspace_factory: Option<engine_market_worker::WorkspaceMarketFactory>,
    lifetime_mode: DesktopLifetimeMode,
    autostart_enabled: bool,
    markets_live_permitted: bool,
    layout: DesktopLayout,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum DesktopLayout {
    #[default]
    Windows,
    WorkspaceTabs,
}

fn split_lifetime_mode(
    first: Option<std::ffi::OsString>,
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
) -> (Option<DesktopLifetimeMode>, Option<std::ffi::OsString>) {
    if first.as_deref() == Some(std::ffi::OsStr::new("--exit-with-desktop")) {
        (Some(DesktopLifetimeMode::ExitWithDesktop), arguments.next())
    } else if first.as_deref() == Some(std::ffi::OsStr::new("--keep-markets-live")) {
        (Some(DesktopLifetimeMode::KeepMarketsLive), arguments.next())
    } else {
        (None, first)
    }
}

struct ConfiguredLifecycle {
    mode: DesktopLifetimeMode,
    autostart_enabled: bool,
    markets_live_permitted: bool,
    workspace: WorkspaceState,
}

fn configure_engine_lifecycle(
    launch_override: Option<DesktopLifetimeMode>,
) -> Result<ConfiguredLifecycle, String> {
    let executable = axiusflow_local_engine_client::sibling_engine_executable()?;
    let mut client = axiusflow_local_engine_client::connect_or_start_engine(&executable)?;
    let workspace = client.restore_workspace()?;
    let persisted = DesktopLifetimeMode::from_workspace(&workspace)?;
    let mode = launch_override.unwrap_or(persisted);
    client.set_engine_resource_mode(mode.engine_resource_mode())?;
    Ok(ConfiguredLifecycle {
        mode,
        autostart_enabled: workspace.autostart_enabled,
        markets_live_permitted: workspace.markets_live_permitted,
        workspace,
    })
}

fn configured_market_workers() -> Result<Option<ConfiguredDesktop>, String> {
    let mut arguments = std::env::args_os().skip(1);
    let first = arguments.next();
    let (lifetime_mode, command) = split_lifetime_mode(first, &mut arguments);
    let mut layout = DesktopLayout::Windows;
    let mut workspace_factory = None;
    let (market_workers, workspace_panes, lifecycle) = if let Some(argument) = command {
        #[cfg(feature = "diagnostics")]
        if argument == "--windowed-benchmark" {
            let report_path = arguments.next().ok_or_else(|| {
                "usage: axiusflow_desktop --windowed-benchmark <report-path>".to_string()
            })?;
            windowed_benchmark::run(std::path::Path::new(&report_path))
                .map_err(|error| format!("windowed benchmark failed: {error}"))?;
            return Ok(None);
        }
        #[cfg(feature = "diagnostics")]
        if argument == "--desktop-readiness" {
            run_desktop_readiness_command(arguments).expect("desktop readiness conformance passes");
            return Ok(None);
        }
        #[cfg(feature = "diagnostics")]
        if argument == "--desktop-endurance" {
            run_desktop_endurance_command(arguments).expect("desktop endurance conformance passes");
            return Ok(None);
        }
        if argument == "--rithmic-test" {
            if arguments.next().is_some() {
                eprintln!("usage: axiusflow_desktop --rithmic-test");
                std::process::exit(2);
            }
            let lifecycle = configure_engine_lifecycle(lifetime_mode)?;
            (vec![rithmic_engine_client::start()?], Vec::new(), lifecycle)
        } else if argument == "--multi-chart" {
            if arguments.next().is_some() {
                eprintln!("usage: axiusflow_desktop --multi-chart");
                std::process::exit(2);
            }
            let lifecycle = configure_engine_lifecycle(lifetime_mode)?;
            (
                engine_market_worker::start_multi_chart()?,
                Vec::new(),
                lifecycle,
            )
        } else if argument == "--workspace-tabs" {
            if arguments.next().is_some() {
                eprintln!("usage: axiusflow_desktop --workspace-tabs");
                std::process::exit(2);
            }
            let lifecycle = configure_engine_lifecycle(lifetime_mode)?;
            layout = DesktopLayout::WorkspaceTabs;
            let group = engine_market_worker::start_workspace_tabs(&lifecycle.workspace)?;
            workspace_factory = Some(group.factory);
            (Vec::new(), group.initial, lifecycle)
        } else {
            eprintln!("unsupported argument: {}", argument.to_string_lossy());
            std::process::exit(2);
        }
    } else {
        let lifecycle = configure_engine_lifecycle(lifetime_mode)?;
        (vec![engine_market_worker::start()?], Vec::new(), lifecycle)
    };
    Ok(Some(ConfiguredDesktop {
        market_workers,
        workspace_panes,
        restored_workspace: lifecycle.workspace.clone(),
        workspace_factory,
        lifetime_mode: lifecycle.mode,
        autostart_enabled: lifecycle.autostart_enabled,
        markets_live_permitted: lifecycle.markets_live_permitted,
        layout,
    }))
}

fn main() {
    let configured = match configured_market_workers() {
        Ok(Some(configured)) => configured,
        Ok(None) => return,
        Err(error) => {
            eprintln!("Axiusflow market worker could not start: {error}");
            std::process::exit(1);
        }
    };
    let lifecycle = match DesktopLifecycle::new(
        configured.lifetime_mode,
        configured.autostart_enabled,
        configured.markets_live_permitted,
    ) {
        Ok(lifecycle) => lifecycle,
        Err(error) => {
            eprintln!("Axiusflow lifecycle client could not start: {error}");
            std::process::exit(1);
        }
    };
    run_desktop(configured, lifecycle);
}

fn run_desktop(configured: ConfiguredDesktop, lifecycle: DesktopLifecycle) {
    let market_workers = configured.market_workers;
    let workspace_panes = configured.workspace_panes;
    let restored_workspace = configured.restored_workspace;
    let workspace_factory = configured.workspace_factory;
    let layout = configured.layout;
    application()
        .with_assets(assets::AxiusflowAssets)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            cx.text_system()
                .add_fonts(vec![Cow::Borrowed(include_bytes!(
                    "../../../crates/ui/design_system/inter_400.ttf"
                ))])
                .expect("the bundled Inter Regular font is valid");
            cx.bind_keys([
                KeyBinding::new("f11", ToggleFullscreen, None),
                KeyBinding::new("alt-enter", ToggleFullscreen, None),
                KeyBinding::new("alt-f9", MinimizeWindow, None),
                KeyBinding::new("alt-f10", ZoomWindow, None),
                KeyBinding::new("alt-f4", CloseWindow, None),
                KeyBinding::new("ctrl-t", NewWorkspace, None),
                KeyBinding::new("ctrl-tab", SelectNextWorkspace, None),
                KeyBinding::new("ctrl-shift-tab", SelectPreviousWorkspace, None),
                KeyBinding::new("ctrl-shift-pageup", MoveWorkspaceLeft, None),
                KeyBinding::new("ctrl-shift-pagedown", MoveWorkspaceRight, None),
                KeyBinding::new("ctrl-w", CloseWorkspace, None),
                KeyBinding::new("ctrl-alt-h", SplitPaneHorizontal, None),
                KeyBinding::new("ctrl-alt-v", SplitPaneVertical, None),
                KeyBinding::new("ctrl-shift-w", ClosePane, None),
            ]);
            let quit_lifecycle = lifecycle.clone();
            cx.on_app_quit(move |cx| {
                let quit = quit_lifecycle.begin_quit(cx);
                async move {
                    if let Some(quit) = quit
                        && let Err(error) = quit.await
                    {
                        eprintln!("Axiusflow desktop shutdown failed: {error}");
                    }
                }
            })
            .detach();
            let last_window_lifecycle = lifecycle.clone();
            cx.on_window_closed(move |cx, _| {
                if cx.windows().is_empty() {
                    last_window_lifecycle.quit_after_shutdown(cx);
                }
            })
            .detach();
            match layout {
                DesktopLayout::Windows => {
                    for (window_index, (bootstrap, market_worker)) in
                        market_workers.into_iter().enumerate()
                    {
                        let options = desktop_window_options(window_index, cx);
                        let window_lifecycle = lifecycle.clone();
                        cx.open_window(options, move |window, cx| {
                            terminal_root(bootstrap, market_worker, &window_lifecycle, window, cx)
                        })
                        .expect("the Axiusflow terminal window opens");
                    }
                }
                DesktopLayout::WorkspaceTabs => {
                    let window_lifecycle = lifecycle.clone();
                    let options = desktop_window_options(0, cx);
                    let workspace_factory =
                        workspace_factory.expect("workspace layout has a market workspace factory");
                    cx.open_window(options, move |window, cx| {
                        workspace_tabs_root(
                            workspace_panes,
                            &restored_workspace,
                            workspace_factory,
                            &window_lifecycle,
                            window,
                            cx,
                        )
                    })
                    .expect("the Axiusflow workspace window opens");
                }
            }
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::{
        COINBASE_CALENDAR_HISTORY_STATUS, COINBASE_ENTITLEMENT_ID, COINBASE_INTERVALS,
        CaptionPlatform, CaptionPointerOwner, ChartNoticePlacement, ChartNoticeTone, ChartState,
        ChromeOverlayPhase, DesktopLifetimeMode, HeaderControls, InputEvent,
        ProviderCatalogCommand, RithmicReadyAction, RithmicReconnectState, RithmicReconnectTarget,
        RithmicSessionRetirement, SidePanel, SidePanelResize, SymbolInputAction,
        SymbolSubmitDecision, TerminalProvider, WORKSPACE_TAB_GAP,
        WORKSPACE_TAB_STRIP_PADDING_LEFT, WORKSPACE_TAB_WIDTH, WindowCommand,
        WindowMoveGestureEvent, WindowMoveGestureTransition, WorkspaceDragState,
        active_workspace_after_close, bounded_status_detail, caption_keyboard_activates,
        caption_pointer_owner, catalog_rejection_message, chart_status_detail,
        chart_surface_notice, chrome_control_foreground, chrome_overlay_progress, claim_once,
        connection_presentation, default_rithmic_contract_index, durable_workspace_viewport,
        finish_desktop_shutdown, fullscreen_escape_command, gpui_color, instrument_selector_label,
        nucleus_chart_theme, publication_chart_state, reconciled_bridge_state,
        reconnect_contract_index, reorder_workspace_ids, resized_side_panel_width,
        rithmic_ready_action, series_selector_label, should_finish_chrome_overlay_close,
        split_lifetime_mode, symbol_input_action, symbol_submit_decision, timeframe_overlay_left,
        window_move_gesture_transition, workspace_drag_destination, workspace_drag_translation,
        workspace_label, workspace_series, workspace_split_ratio, workspace_switch,
        workspace_title_bar_visible, wrapped_workspace_index,
    };
    #[cfg(feature = "diagnostics")]
    use super::{FOREGROUND_INTERACTION_SAMPLE_CAPACITY, ForegroundInteractionDiagnostics};
    use axiusflow_chart_integration::{ChartSplitDirection, NucleusChartTheme};
    use axiusflow_design_system::{AxiusflowTheme, ThemeColor, ThemeMode};
    use axiusflow_engine_protocol::{
        EngineLifetimeMode, InstallProviderInstrument, ProviderCatalogRejectionReason,
        ProviderInstrumentSummary, ResourceMode, SeriesCadence, WorkspaceState,
    };
    use axiusflow_market_data::ChartInterval;
    use axiusflow_observability::FeedConnectionState;
    use gpui::{Bounds, point, px, size};
    use std::{cell::Cell, ffi::OsString};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum CatalogCommandDomain {
        Search,
        Selection,
    }

    const fn catalog_rejection_domain(command: ProviderCatalogCommand) -> CatalogCommandDomain {
        match command {
            ProviderCatalogCommand::Search => CatalogCommandDomain::Search,
            ProviderCatalogCommand::Selection => CatalogCommandDomain::Selection,
        }
    }

    fn should_apply_rithmic_worker_stop(
        disconnected: bool,
        connection_state: Option<FeedConnectionState>,
    ) -> bool {
        disconnected
            && matches!(
                connection_state,
                Some(state) if !matches!(state, FeedConnectionState::Stopped)
            )
    }

    impl RithmicReconnectState {
        fn capture_retired_selection(
            &mut self,
            selection: Option<super::rithmic_shell::RithmicSymbolSelection>,
            series: super::rithmic_history::RithmicSeries,
        ) -> bool {
            if *self == Self::Idle
                && let Some(selection) = selection
            {
                *self = Self::AwaitingSearch(super::RithmicReconnectTarget {
                    symbol: selection.instrument.symbol,
                    exchange: selection.instrument.exchange,
                    series,
                });
            }
            *self == Self::Idle
        }
    }

    #[test]
    fn chrome_overlay_motion_opens_and_closes_in_opposite_directions() {
        let assert_progress = |actual: f32, expected: f32| {
            assert!((actual - expected).abs() < f32::EPSILON);
        };
        assert_progress(
            chrome_overlay_progress(ChromeOverlayPhase::Opening, 0.0),
            0.0,
        );
        assert_progress(
            chrome_overlay_progress(ChromeOverlayPhase::Opening, 1.0),
            1.0,
        );
        assert_progress(
            chrome_overlay_progress(ChromeOverlayPhase::Closing, 0.0),
            1.0,
        );
        assert_progress(
            chrome_overlay_progress(ChromeOverlayPhase::Closing, 1.0),
            0.0,
        );
        assert_progress(
            chrome_overlay_progress(ChromeOverlayPhase::Opening, -1.0),
            0.0,
        );
        assert_progress(
            chrome_overlay_progress(ChromeOverlayPhase::Closing, 2.0),
            0.0,
        );
    }

    #[test]
    fn stale_close_completion_cannot_remove_a_reopened_overlay() {
        assert!(should_finish_chrome_overlay_close(
            ChromeOverlayPhase::Closing,
            7,
            7
        ));
        assert!(!should_finish_chrome_overlay_close(
            ChromeOverlayPhase::Opening,
            8,
            7
        ));
        assert!(!should_finish_chrome_overlay_close(
            ChromeOverlayPhase::Closing,
            8,
            7
        ));
    }

    #[test]
    fn timeframe_overlay_uses_the_trigger_left_edge() {
        let trigger = Bounds::new(point(px(214.0), px(52.0)), size(px(64.0), px(32.0)));
        assert_eq!(timeframe_overlay_left(Some(trigger)), px(214.0));
        assert_eq!(timeframe_overlay_left(None), px(0.0));
    }

    #[test]
    fn workspace_split_ratio_tracks_the_active_axis_and_clamps_safe_bounds() {
        assert_eq!(
            workspace_split_ratio(
                ChartSplitDirection::Horizontal,
                350.0,
                0.0,
                100.0,
                0.0,
                500.0,
                200.0,
            ),
            Some(0.5)
        );
        assert_eq!(
            workspace_split_ratio(
                ChartSplitDirection::Vertical,
                0.0,
                325.0,
                0.0,
                25.0,
                500.0,
                400.0,
            ),
            Some(0.75)
        );
        let minimum = workspace_split_ratio(
            ChartSplitDirection::Horizontal,
            -100.0,
            0.0,
            0.0,
            0.0,
            500.0,
            200.0,
        )
        .expect("finite horizontal bounds produce a ratio");
        assert!((minimum - 0.05).abs() < f64::from(f32::EPSILON));
        assert_eq!(
            workspace_split_ratio(
                ChartSplitDirection::Vertical,
                0.0,
                100.0,
                0.0,
                0.0,
                500.0,
                0.0,
            ),
            None
        );
    }

    #[test]
    fn desktop_lifetime_modes_are_explicit_per_launch_policies() {
        let mut remaining = vec![OsString::from("--multi-chart")].into_iter();
        let (mode, command) =
            split_lifetime_mode(Some(OsString::from("--exit-with-desktop")), &mut remaining);
        assert_eq!(mode, Some(DesktopLifetimeMode::ExitWithDesktop));
        assert_eq!(command, Some(OsString::from("--multi-chart")));

        let mut remaining = vec![OsString::from("--rithmic-test")].into_iter();
        let (mode, command) =
            split_lifetime_mode(Some(OsString::from("--keep-markets-live")), &mut remaining);
        assert_eq!(mode, Some(DesktopLifetimeMode::KeepMarketsLive));
        assert_eq!(
            mode.expect("launch override exists").engine_resource_mode(),
            ResourceMode::MarketsLive
        );
        assert_eq!(command, Some(OsString::from("--rithmic-test")));

        let mut empty = Vec::<OsString>::new().into_iter();
        let (mode, command) = split_lifetime_mode(None, &mut empty);
        assert_eq!(mode, None);
        assert_eq!(command, None);
    }

    #[test]
    fn lifecycle_controls_never_enter_markets_live_without_explicit_permission() {
        assert_eq!(
            DesktopLifetimeMode::KeepEngineWarm.next(false),
            DesktopLifetimeMode::ExitWithDesktop
        );
        assert_eq!(
            DesktopLifetimeMode::KeepEngineWarm.next(true),
            DesktopLifetimeMode::KeepMarketsLive
        );
        assert_eq!(
            DesktopLifetimeMode::KeepMarketsLive.next(true),
            DesktopLifetimeMode::ExitWithDesktop
        );
        assert_eq!(DesktopLifetimeMode::ExitWithDesktop.label(), "Exit fully");
        assert_eq!(DesktopLifetimeMode::KeepEngineWarm.label(), "Engine warm");
        assert_eq!(DesktopLifetimeMode::KeepMarketsLive.label(), "Markets live");

        let mut workspace = WorkspaceState {
            lifetime_mode: EngineLifetimeMode::KeepMarketsLive as i32,
            markets_live_permitted: false,
            ..WorkspaceState::default()
        };
        assert!(DesktopLifetimeMode::from_workspace(&workspace).is_err());
        workspace.markets_live_permitted = true;
        assert_eq!(
            DesktopLifetimeMode::from_workspace(&workspace),
            Ok(DesktopLifetimeMode::KeepMarketsLive)
        );
    }

    #[test]
    fn exit_with_desktop_still_requests_engine_shutdown_after_detach_expiry() {
        let shutdown_called = Cell::new(false);
        let result = finish_desktop_shutdown(DesktopLifetimeMode::ExitWithDesktop, true, || {
            shutdown_called.set(true);
            Ok(())
        });
        assert!(shutdown_called.get());
        assert!(result.is_err());
    }

    #[cfg(feature = "diagnostics")]
    #[test]
    fn foreground_interaction_samples_are_bounded_per_handler() {
        let mut diagnostics = ForegroundInteractionDiagnostics::default();
        for sample in 0..=FOREGROUND_INTERACTION_SAMPLE_CAPACITY {
            let elapsed = u64::try_from(sample).unwrap_or(u64::MAX);
            diagnostics.record_symbol_input(false, elapsed);
            diagnostics.record_symbol_input(true, elapsed);
            diagnostics.record_instrument_selection(elapsed);
            diagnostics.record_interval_selection(elapsed);
        }
        assert_eq!(
            diagnostics.symbol_input_change.len(),
            FOREGROUND_INTERACTION_SAMPLE_CAPACITY
        );
        assert_eq!(
            diagnostics.symbol_input_submit.len(),
            FOREGROUND_INTERACTION_SAMPLE_CAPACITY
        );
        assert_eq!(
            diagnostics.instrument_selection.len(),
            FOREGROUND_INTERACTION_SAMPLE_CAPACITY
        );
        assert_eq!(
            diagnostics.interval_selection.len(),
            FOREGROUND_INTERACTION_SAMPLE_CAPACITY
        );
    }

    #[test]
    fn escape_exits_fullscreen_without_stealing_regular_escape() {
        assert_eq!(
            fullscreen_escape_command("escape", true),
            Some(WindowCommand::ToggleFullscreen)
        );
        assert_eq!(fullscreen_escape_command("escape", false), None);
        assert_eq!(fullscreen_escape_command("enter", true), None);
    }

    #[test]
    fn fullscreen_hides_workspace_title_bar_and_native_controls() {
        assert!(!workspace_title_bar_visible(true));
        assert!(workspace_title_bar_visible(false));
    }

    #[test]
    fn caption_pointer_ownership_is_exclusive_per_platform() {
        assert_eq!(
            caption_pointer_owner(CaptionPlatform::Windows),
            CaptionPointerOwner::Native
        );
        assert_eq!(
            caption_pointer_owner(CaptionPlatform::Linux),
            CaptionPointerOwner::Application
        );
        assert_eq!(
            caption_pointer_owner(CaptionPlatform::MacOs),
            CaptionPointerOwner::System
        );
        assert_eq!(
            caption_pointer_owner(CaptionPlatform::Other),
            CaptionPointerOwner::Application
        );
    }

    #[test]
    fn caption_keyboard_activation_accepts_only_button_activation_keys() {
        assert!(caption_keyboard_activates("enter"));
        assert!(caption_keyboard_activates("space"));
        assert!(!caption_keyboard_activates("escape"));
        assert!(!caption_keyboard_activates("tab"));
    }

    #[test]
    fn window_move_waits_for_a_pressed_pointer_move_and_cancels_cleanly() {
        assert_eq!(
            window_move_gesture_transition(false, WindowMoveGestureEvent::Press),
            WindowMoveGestureTransition {
                pending: true,
                start_move: false,
            }
        );
        assert_eq!(
            window_move_gesture_transition(
                true,
                WindowMoveGestureEvent::Move { left_pressed: true },
            ),
            WindowMoveGestureTransition {
                pending: false,
                start_move: true,
            }
        );
        assert_eq!(
            window_move_gesture_transition(
                true,
                WindowMoveGestureEvent::Move {
                    left_pressed: false,
                },
            ),
            WindowMoveGestureTransition {
                pending: false,
                start_move: false,
            }
        );
        assert_eq!(
            window_move_gesture_transition(true, WindowMoveGestureEvent::Cancel),
            WindowMoveGestureTransition {
                pending: false,
                start_move: false,
            }
        );
    }

    #[test]
    fn side_panel_resize_clamps_and_reuses_no_stale_pointer_state() {
        let resize = SidePanelResize {
            pointer_x: 500.0,
            width: 320.0,
        };
        assert!((resized_side_panel_width(resize, 420.0) - 400.0).abs() < f32::EPSILON);
        assert!((resized_side_panel_width(resize, -500.0) - 640.0).abs() < f32::EPSILON);
        assert!((resized_side_panel_width(resize, 1_000.0) - 240.0).abs() < f32::EPSILON);
    }

    #[test]
    fn window_close_retirement_is_claimed_exactly_once() {
        let mut closing = false;
        assert!(claim_once(&mut closing));
        assert!(!claim_once(&mut closing));
    }

    #[test]
    fn workspace_tabs_switch_one_surface_and_preserve_stable_labels() {
        assert_eq!(workspace_switch(0, 1, 2), Some((0, 1)));
        assert_eq!(workspace_switch(1, 1, 2), None);
        assert_eq!(workspace_switch(0, 2, 2), None);
        assert_eq!(wrapped_workspace_index(0, 3, -1), Some(2));
        assert_eq!(wrapped_workspace_index(2, 3, 1), Some(0));
        assert_eq!(wrapped_workspace_index(1, 3, -1), Some(0));
        assert_eq!(wrapped_workspace_index(1, 3, 1), Some(2));
        assert_eq!(wrapped_workspace_index(0, 0, 1), None);
        assert_eq!(workspace_label(0), "Workspace 1");
        assert_eq!(workspace_label(7), "Workspace 8");
    }

    #[test]
    fn workspace_tabs_reorder_and_close_without_changing_active_identity() {
        let mut ids = vec![1, 2, 3];
        assert!(reorder_workspace_ids(&mut ids, 1, 2));
        assert_eq!(ids, vec![2, 3, 1]);
        assert!(reorder_workspace_ids(&mut ids, 1, 0));
        assert_eq!(ids, vec![1, 2, 3]);
        assert!(reorder_workspace_ids(&mut ids, 1, 1));
        assert_eq!(ids, vec![2, 1, 3]);
        assert!(reorder_workspace_ids(&mut ids, 1, 0));
        assert_eq!(ids, vec![1, 2, 3]);
        assert!(reorder_workspace_ids(&mut ids, 3, 0));
        assert_eq!(ids, vec![3, 1, 2]);
        let trailing_index = ids.len();
        assert!(reorder_workspace_ids(&mut ids, 3, trailing_index));
        assert_eq!(ids, vec![1, 2, 3]);
        let trailing_index = ids.len();
        assert!(!reorder_workspace_ids(&mut ids, 3, trailing_index));
        assert!(!reorder_workspace_ids(&mut ids, 2, 1));
        assert!(!reorder_workspace_ids(&mut ids, 99, 0));

        assert_eq!(active_workspace_after_close(&ids, 2, 1), Some(2));
        assert_eq!(active_workspace_after_close(&ids, 2, 2), Some(3));
        assert_eq!(active_workspace_after_close(&ids, 3, 3), Some(2));
        assert_eq!(active_workspace_after_close(&[1], 1, 1), None);
    }

    #[test]
    fn workspace_viewport_persistence_ignores_automatic_live_scrolling() {
        let restored = Some((100, 200));
        let current = Some((300, 400));
        assert_eq!(
            durable_workspace_viewport(restored, false, false, current),
            restored
        );
        assert_eq!(
            durable_workspace_viewport(restored, true, true, current),
            None
        );
        assert_eq!(
            durable_workspace_viewport(restored, true, false, current),
            current
        );
    }

    #[test]
    fn workspace_drag_reflows_at_neighbor_slot_boundaries() {
        let strip_left = 100.0;
        let cursor_offset = WORKSPACE_TAB_WIDTH / 2.0;
        let first_center = strip_left + WORKSPACE_TAB_STRIP_PADDING_LEFT + cursor_offset;
        let second_center = first_center + WORKSPACE_TAB_WIDTH + WORKSPACE_TAB_GAP;
        let third_center = second_center + WORKSPACE_TAB_WIDTH + WORKSPACE_TAB_GAP;

        assert_eq!(
            workspace_drag_destination(first_center, strip_left, cursor_offset, 3),
            Some(0)
        );
        assert_eq!(
            workspace_drag_destination(second_center, strip_left, cursor_offset, 3),
            Some(1)
        );
        assert_eq!(
            workspace_drag_destination(third_center, strip_left, cursor_offset, 3),
            Some(2)
        );
        assert_eq!(
            workspace_drag_destination(strip_left - 500.0, strip_left, cursor_offset, 3),
            Some(0)
        );
        assert_eq!(
            workspace_drag_destination(third_center + 500.0, strip_left, cursor_offset, 3),
            Some(2)
        );
        assert_eq!(
            workspace_drag_destination(first_center, strip_left, cursor_offset, 0),
            None
        );
        assert_eq!(
            workspace_drag_destination(f32::NAN, strip_left, cursor_offset, 3),
            None
        );

        let drag = WorkspaceDragState {
            tab_id: 7,
            cursor_offset_x: cursor_offset,
            pointer_x: Some(second_center + 10.0),
            strip_left,
        };
        assert_eq!(workspace_drag_translation(Some(drag), 7, 1), Some(10.0));
        assert_eq!(workspace_drag_translation(Some(drag), 8, 1), None);
    }

    #[test]
    fn chrome_controls_use_icon_and_disabled_hierarchy() {
        let colors = AxiusflowTheme::light().colors;
        assert_eq!(chrome_control_foreground(&colors, false, true), colors.icon);
        assert_eq!(
            chrome_control_foreground(&colors, true, true),
            colors.icon_active
        );
        assert_eq!(
            chrome_control_foreground(&colors, false, false),
            colors.text_muted
        );
    }

    #[test]
    fn shell_theme_maps_only_to_nucleus_theme_selection() {
        assert_eq!(
            nucleus_chart_theme(ThemeMode::Light),
            NucleusChartTheme::Light
        );
        assert_eq!(
            nucleus_chart_theme(ThemeMode::Dark),
            NucleusChartTheme::Dark
        );
    }

    #[test]
    fn publication_is_ready_only_after_bridge_acceptance_without_recovery() {
        assert_eq!(publication_chart_state(true, false), ChartState::Ready);
        assert_eq!(publication_chart_state(false, true), ChartState::Recovering);
        assert_eq!(publication_chart_state(true, true), ChartState::Recovering);
    }

    #[test]
    fn catalog_rejections_preserve_search_and_selection_generation_domains() {
        assert_eq!(
            catalog_rejection_domain(ProviderCatalogCommand::Search),
            CatalogCommandDomain::Search
        );
        assert_eq!(
            catalog_rejection_domain(ProviderCatalogCommand::Selection),
            CatalogCommandDomain::Selection
        );
    }

    #[test]
    fn symbol_input_searches_only_on_change_events() {
        assert_eq!(
            symbol_input_action(&InputEvent::Change),
            SymbolInputAction::Search
        );
        assert_eq!(
            symbol_input_action(&InputEvent::PressEnter {
                secondary: false,
                shift: false,
            }),
            SymbolInputAction::Submit
        );
        assert_eq!(
            symbol_input_action(&InputEvent::Focus),
            SymbolInputAction::Ignore
        );
        assert_eq!(
            symbol_input_action(&InputEvent::Blur),
            SymbolInputAction::Ignore
        );
    }

    #[test]
    fn enter_closes_only_for_an_available_provider_selection() {
        assert_eq!(
            symbol_submit_decision(TerminalProvider::Rithmic, 3, 1),
            SymbolSubmitDecision::Select(1)
        );
        assert_eq!(
            symbol_submit_decision(TerminalProvider::Rithmic, 0, 0),
            SymbolSubmitDecision::Search
        );
        assert_eq!(
            symbol_submit_decision(TerminalProvider::Rithmic, 1, 2),
            SymbolSubmitDecision::Search
        );
        assert_eq!(
            symbol_submit_decision(TerminalProvider::Coinbase, 2, 0),
            SymbolSubmitDecision::Select(0)
        );
        assert_eq!(
            symbol_submit_decision(TerminalProvider::Coinbase, 0, 0),
            SymbolSubmitDecision::None
        );
    }

    #[test]
    fn coinbase_catalog_rejections_never_name_rithmic() {
        for reason in [
            ProviderCatalogRejectionReason::SearchRejected,
            ProviderCatalogRejectionReason::SupersededSearch,
            ProviderCatalogRejectionReason::InstrumentUnavailable,
            ProviderCatalogRejectionReason::SubscriptionRejected,
            ProviderCatalogRejectionReason::DispatchUnavailable,
            ProviderCatalogRejectionReason::Unspecified,
        ] {
            for command in [
                ProviderCatalogCommand::Search,
                ProviderCatalogCommand::Selection,
            ] {
                let message = catalog_rejection_message(reason, command, true);
                assert!(!message.contains("Rithmic"), "{message}");
            }
        }
    }

    #[test]
    fn persisted_calendar_series_keep_week_and_month_identity() {
        let instrument = InstallProviderInstrument {
            provider: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            entitlement_id: COINBASE_ENTITLEMENT_ID.to_string(),
            ..InstallProviderInstrument::default()
        };
        let week = workspace_series(ChartInterval::Week1, &instrument);
        let month = workspace_series(ChartInterval::Month1, &instrument);
        assert_eq!(week.cadence, SeriesCadence::CalendarWeeks as i32);
        assert_eq!(month.cadence, SeriesCadence::CalendarMonths as i32);
        assert_eq!(week.cadence_value, 1);
        assert_eq!(month.cadence_value, 1);
        assert!(COINBASE_INTERVALS.contains(&ChartInterval::Month1));
    }

    #[test]
    fn bridge_recovery_replaces_ready_after_deferred_gap_validation() {
        assert_eq!(
            reconciled_bridge_state(ChartState::Ready, true),
            ChartState::Recovering
        );
        assert_eq!(
            reconciled_bridge_state(ChartState::Stale, true),
            ChartState::Stale
        );
    }

    #[test]
    fn default_rithmic_contract_skips_continuous_and_spread_symbols() {
        let result = |symbol: &str, expiration: &str| ProviderInstrumentSummary {
            symbol: symbol.to_string(),
            exchange: "CME-Delayed".to_string(),
            name: None,
            product_code: Some("MNQ".to_string()),
            instrument_type: Some("FUTURE".to_string()),
            expiration_date: Some(expiration.to_string()),
        };
        let results = vec![
            result("MNQ", "20260918"),
            result("MNQU6-MNQZ6", "20260918"),
            result("MNQZ6", "20261218"),
            result("MNQU6", "20260918"),
            ProviderInstrumentSummary {
                symbol: "NQ".to_string(),
                exchange: "CME-Delayed".to_string(),
                name: None,
                product_code: Some("NQ".to_string()),
                instrument_type: Some("FUTURE".to_string()),
                expiration_date: None,
            },
        ];
        assert_eq!(default_rithmic_contract_index(&results), Some(3));
    }

    #[test]
    fn reconnect_contract_requires_the_exact_symbol_and_exchange() {
        let result = |exchange: &str| ProviderInstrumentSummary {
            symbol: "MNQU6".to_string(),
            exchange: exchange.to_string(),
            name: None,
            product_code: Some("MNQ".to_string()),
            instrument_type: Some("FUTURE".to_string()),
            expiration_date: Some("20260918".to_string()),
        };
        let results = vec![result("CME-Delayed"), result("CME")];
        let target = RithmicReconnectTarget {
            symbol: "MNQU6".to_string(),
            exchange: "CME".to_string(),
            series: crate::rithmic_history::RithmicSeries::from(ChartInterval::Minute5),
        };
        assert_eq!(reconnect_contract_index(&results, &target), Some(1));
        let missing = RithmicReconnectTarget {
            exchange: "CBOT".to_string(),
            ..target
        };
        assert_eq!(reconnect_contract_index(&results, &missing), None);
    }

    #[test]
    fn disconnected_session_retires_surfaces_and_keeps_a_stale_chart_notice() {
        let retirement =
            RithmicSessionRetirement::from_connection(FeedConnectionState::Disconnected);
        assert_eq!(retirement, RithmicSessionRetirement::Offline);
        assert_eq!(retirement.chart_state(true), Some(ChartState::Stale));
        assert_eq!(retirement.chart_state(false), None);
        let mut retained_chart_state = retirement
            .chart_state(true)
            .expect("offline retained chart becomes stale");
        assert_eq!(retained_chart_state, ChartState::Stale);
        retained_chart_state =
            RithmicSessionRetirement::from_connection(FeedConnectionState::Recovering)
                .chart_state(true)
                .expect("the same retained chart advances to reconnecting");
        assert_eq!(retained_chart_state, ChartState::Recovering);
        assert_eq!(
            RithmicSessionRetirement::from_connection(FeedConnectionState::Recovering)
                .chart_state(true),
            Some(ChartState::Recovering)
        );
        assert_eq!(
            RithmicSessionRetirement::from_connection(FeedConnectionState::Streaming),
            RithmicSessionRetirement::None
        );
    }

    #[test]
    fn terminal_session_stop_is_a_truthful_chart_error_with_or_without_data() {
        let stopped = RithmicSessionRetirement::from_connection(FeedConnectionState::Stopped);
        assert_eq!(stopped.chart_state(true), Some(ChartState::Error));
        assert_eq!(stopped.chart_state(false), Some(ChartState::Error));
        assert_eq!(
            chart_surface_notice(
                stopped.chart_state(true).expect("stopped chart state"),
                true,
                "Rithmic market worker stopped",
            )
            .expect("retained chart error notice")
            .placement,
            ChartNoticePlacement::TopLeft
        );
        assert_eq!(
            chart_surface_notice(
                stopped.chart_state(false).expect("stopped chart state"),
                false,
                "Rithmic market worker stopped",
            )
            .expect("empty chart error notice")
            .placement,
            ChartNoticePlacement::Center
        );
    }

    #[test]
    fn dead_rithmic_worker_stop_transition_is_applied_once() {
        assert!(should_apply_rithmic_worker_stop(
            true,
            Some(FeedConnectionState::Streaming)
        ));
        assert!(!should_apply_rithmic_worker_stop(
            true,
            Some(FeedConnectionState::Stopped)
        ));
        assert!(!should_apply_rithmic_worker_stop(
            false,
            Some(FeedConnectionState::Streaming)
        ));
        assert!(!should_apply_rithmic_worker_stop(true, None));
    }

    #[test]
    fn authentication_ready_reselects_the_retired_contract() {
        let reconnect = RithmicReconnectState::AwaitingSearch(RithmicReconnectTarget {
            symbol: "MNQU6".to_string(),
            exchange: "CME".to_string(),
            series: crate::rithmic_history::RithmicSeries::from(ChartInterval::Minute5),
        });
        assert_eq!(
            rithmic_ready_action(
                FeedConnectionState::Authenticating,
                "Rithmic Test session is ready for instrument search",
                &reconnect,
                true,
            ),
            RithmicReadyAction::Reconnect("MNQU6".to_string())
        );
        assert_eq!(
            rithmic_ready_action(
                FeedConnectionState::Discovering,
                "discovering Rithmic Test systems",
                &reconnect,
                true,
            ),
            RithmicReadyAction::None
        );
    }

    #[test]
    fn interrupted_initial_autoload_restarts_after_authentication() {
        let mut reconnect = RithmicReconnectState::Idle;
        assert!(
            reconnect
                .capture_retired_selection(None, crate::rithmic_history::RithmicSeries::Minute1,)
        );
        assert_eq!(
            rithmic_ready_action(
                FeedConnectionState::Authenticating,
                "Rithmic Test session is ready for instrument search",
                &reconnect,
                false,
            ),
            RithmicReadyAction::Autoload
        );
        assert_eq!(
            rithmic_ready_action(
                FeedConnectionState::Authenticating,
                "Rithmic Test session is ready for instrument search",
                &RithmicReconnectState::Idle,
                true,
            ),
            RithmicReadyAction::None
        );
    }

    #[test]
    fn gpui_theme_attachment_preserves_alpha() {
        let attached = gpui_color(ThemeColor::from_rgb8(240, 240, 240).with_alpha(19.0 / 255.0));
        assert!((attached.a - 19.0 / 255.0).abs() < f32::EPSILON);
    }

    #[test]
    fn header_lifecycle_values_are_truthfully_labeled() {
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Disconnected,
                ChartState::Loading,
                false,
                false,
            )
            .0,
            "Offline"
        );
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Recovering,
                ChartState::Loading,
                false,
                false,
            )
            .0,
            "Test · Reconnecting"
        );
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Streaming,
                ChartState::Ready,
                false,
                false,
            )
            .0,
            "Test · Live"
        );
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Streaming,
                ChartState::Stale,
                false,
                false,
            )
            .0,
            "Test · Stale"
        );
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Streaming,
                ChartState::Ready,
                true,
                false,
            )
            .0,
            "Test · Delayed"
        );
        assert_eq!(
            connection_presentation(
                TerminalProvider::Coinbase,
                FeedConnectionState::Streaming,
                ChartState::Ready,
                false,
                false,
            )
            .0,
            "Coinbase · Live"
        );
        assert_eq!(
            connection_presentation(
                TerminalProvider::Coinbase,
                FeedConnectionState::Streaming,
                ChartState::Ready,
                false,
                true,
            )
            .0,
            "Coinbase · Completed history"
        );
        assert!(COINBASE_CALENDAR_HISTORY_STATUS.contains("current calendar bucket is not live"));
        assert!(!COINBASE_CALENDAR_HISTORY_STATUS.contains("is current"));
        let controls = HeaderControls::from_state(true, true).with_chart_controls(true);
        assert!(controls.enabled(HeaderControls::INSTRUMENT));
        assert!(controls.enabled(HeaderControls::SERIES));
        assert!(controls.enabled(HeaderControls::DOM));
        assert!(controls.enabled(HeaderControls::FIT));
        assert!(controls.enabled(HeaderControls::LATEST));
    }

    #[test]
    fn chart_controls_follow_retained_data_instead_of_transient_chart_state() {
        let retained_chart_controls =
            HeaderControls::from_state(true, false).with_chart_controls(true);
        assert!(retained_chart_controls.enabled(HeaderControls::FIT));
        assert!(retained_chart_controls.enabled(HeaderControls::LATEST));

        let empty_chart_controls =
            HeaderControls::from_state(true, false).with_chart_controls(false);
        assert!(!empty_chart_controls.enabled(HeaderControls::FIT));
        assert!(!empty_chart_controls.enabled(HeaderControls::LATEST));
    }

    #[test]
    fn side_panel_controls_keep_stable_labels_and_explicit_destinations() {
        assert_eq!(SidePanel::Dom.toggle_label(), "DOM");
        assert_eq!(SidePanel::Dom.title(), "Order book");
        assert_eq!(
            SidePanel::Dom.toggle_tooltip(),
            "Toggle read-only depth panel"
        );
    }

    #[test]
    fn chart_notice_distinguishes_empty_loading_from_retained_recovery() {
        let loading = chart_surface_notice(
            ChartState::Loading,
            false,
            "discovering Rithmic Test systems",
        )
        .expect("loading notice");
        assert_eq!(loading.label, "Loading chart");
        assert_eq!(
            loading.detail.as_deref(),
            Some("discovering Rithmic Test systems")
        );
        assert_eq!(loading.placement, ChartNoticePlacement::Center);
        assert_eq!(loading.tone, ChartNoticeTone::Muted);

        let recovery = chart_surface_notice(
            ChartState::Recovering,
            true,
            "Rithmic Test session will retry",
        )
        .expect("recovery notice");
        assert_eq!(recovery.label, "Reconnecting chart");
        assert_eq!(recovery.placement, ChartNoticePlacement::TopLeft);
        assert_eq!(recovery.tone, ChartNoticeTone::Warning);
        assert!(chart_surface_notice(ChartState::Ready, true, "current").is_none());
    }

    #[test]
    fn chart_error_and_stale_notices_use_truthful_severity() {
        let stale = chart_surface_notice(ChartState::Stale, true, "trade stream is silent")
            .expect("stale notice");
        assert_eq!(stale.label, "Chart stale");
        assert_eq!(stale.tone, ChartNoticeTone::Warning);

        let error = chart_surface_notice(
            ChartState::Error,
            false,
            "Rithmic Test authentication was rejected",
        )
        .expect("error notice");
        assert_eq!(error.label, "Chart unavailable");
        assert_eq!(
            error.detail.as_deref(),
            Some("Rithmic Test authentication was rejected")
        );
        assert_eq!(error.placement, ChartNoticePlacement::Center);
        assert_eq!(error.tone, ChartNoticeTone::Loss);
    }

    #[test]
    fn contextual_labels_keep_contract_and_pending_series_truthful() {
        assert_eq!(instrument_selector_label(None, false), "Contract");
        assert_eq!(
            instrument_selector_label(Some(("MNQU6", "CME")), false),
            "MNQU6 / CME"
        );
        assert_eq!(
            instrument_selector_label(Some(("MNQU6", "CME")), true),
            "MNQU6 / CME"
        );
        assert_eq!(series_selector_label(None, None), "Series");
        assert_eq!(
            series_selector_label(Some(crate::rithmic_history::RithmicSeries::Minute1), None),
            "1m"
        );
        assert_eq!(
            series_selector_label(
                Some(crate::rithmic_history::RithmicSeries::Minute1),
                Some(crate::rithmic_history::RithmicSeries::from(
                    ChartInterval::Minute5,
                )),
            ),
            "1m"
        );
    }

    #[test]
    fn chart_detail_prefers_connection_context_until_streaming() {
        assert_eq!(
            chart_status_detail(
                ChartState::Loading,
                FeedConnectionState::Authenticating,
                "waiting for chart",
                Some("Rithmic Test agreements require attention"),
            ),
            "Rithmic Test agreements require attention"
        );
        assert_eq!(
            chart_status_detail(
                ChartState::Recovering,
                FeedConnectionState::Streaming,
                "history is covering a gap",
                Some("feed is streaming"),
            ),
            "history is covering a gap"
        );
    }

    #[test]
    fn chart_detail_is_bounded_and_suppresses_generic_duplicates() {
        assert_eq!(
            bounded_status_detail(" Chart unavailable ", "Chart unavailable"),
            None
        );
        let detail = bounded_status_detail(&"x".repeat(200), "Chart unavailable")
            .expect("long detail remains visible");
        assert_eq!(detail.chars().count(), 161);
        assert!(detail.ends_with('…'));
    }
}
