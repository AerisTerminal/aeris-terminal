//! Axiusflow's native GPUI terminal entry point.

mod assets;
mod chart_chrome;
#[path = "components/chart_context_menus.rs"]
mod chart_context_menus;
#[path = "components/chart_surface.rs"]
mod chart_surface;
#[path = "components/chart_toolbar_menus.rs"]
mod chart_toolbar_menus;
#[path = "components/chrome_menu.rs"]
mod chrome_menu;
#[path = "components/drawing_toolbar.rs"]
mod drawing_toolbar;
mod engine_market_worker;
mod engine_supervisor;
mod frame_poll_gate;
#[path = "components/indicator_menu.rs"]
mod indicator_menu;
mod native_ui;
mod onboarding;
#[cfg(any(test, feature = "diagnostics"))]
mod readiness_conformance;
mod rithmic_engine_client;
mod rithmic_engine_history;
mod rithmic_history;
mod rithmic_shell;
#[path = "components/symbol_menu.rs"]
mod symbol_menu;
#[path = "components/terminal_chrome.rs"]
mod terminal_chrome;
#[path = "components/terminal_view.rs"]
mod terminal_view;
#[cfg(any(test, feature = "diagnostics"))]
mod transition_capture;
#[path = "components/workspace_layout.rs"]
mod workspace_layout;

use assets::UiIcon as HugeIcon;
#[cfg(feature = "diagnostics")]
use axiusflow_application::ReplayStreamUpdate;
use axiusflow_chart_integration::{
    ChartBridgeMetrics, ChartContextKind, ChartContextRequest, ChartDrawingTool, ChartIndicator,
    ChartIndicatorState, ChartSplitDirection, ChartType, ChartWorkspaceLayout, NucleusChartTheme,
    NucleusChartView, NucleusWorkspace, PriceAxisMenuAction, PriceAxisMenuState,
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
use axiusflow_terminal_ui::{DomColumn, DomColumnVisibility, DomFrame, ReadOnlyDomView};
#[cfg(test)]
use chart_context_menus::{
    PriceAxisMenuRow, chart_context_menu_items, clamp_chart_context_menu_origin,
    clamp_price_axis_menu_origin, price_axis_flyout_rows, price_axis_root_rows,
};
use chart_context_menus::{
    account_menu_layer, chart_context_menu_layer, chart_settings_menu_layer, overlay_height,
    price_axis_menu_layer,
};
use chart_surface::{MarketWorkspaceState, market_workspace};
use chart_toolbar_menus::{
    chrome_overlay_layer, chrome_typeahead_blocked, chrome_typeahead_char,
    timeframe_group_intervals, timeframe_interval_group, timeframe_menu_groups,
};
#[cfg(test)]
use chart_toolbar_menus::{
    chrome_typeahead_char_from, clamp_anchored_menu_left, timeframe_flyout_height,
    timeframe_flyout_offset, timeframe_flyout_row_is_active, timeframe_menu_row_label,
    timeframe_overlay_extent, timeframe_overlay_left,
};
#[cfg(test)]
use chrome_menu::{
    CHROME_MENU_FOOTER_HEIGHT, CHROME_MENU_LIST_HEIGHT, CHROME_MENU_MAX_HEIGHT, CHROME_MENU_WIDTH,
};
use chrome_menu::{
    CHROME_MENU_INDICATOR_SEARCH_HEIGHT, CHROME_MENU_SEARCH_HEIGHT, chrome_close_button,
    chrome_menu_extent,
};
use drawing_toolbar::{DrawingToolbarState, drawing_toolbar, drawing_toolbar_expander};
use gpui::{
    Animation, AnimationExt, AnyElement, App, AssetSource, Bounds, ClipboardItem, Context, Div,
    Entity, FocusHandle, Hsla, ImageSource, KeyBinding, KeyDownEvent, MouseButton, ObjectFit,
    Orientation, Pixels, QuitMode, Render, RenderOnce, Role, ScrollHandle, SharedString, Stateful,
    Task, TitlebarOptions, WeakEntity, Window, WindowBounds, WindowControlArea, WindowOptions,
    actions, canvas, div, ease_out_quint, img, point, prelude::*, px, relative, size,
};
use gpui_platform::application;
use indicator_menu::{
    IndicatorDialogState, indicator_dialog_content, indicator_selector, native_indicator,
};
use native_ui::{
    control::Button,
    icon::Icon,
    input::{Input, InputEvent, InputState},
    loader::Loader,
    menu::{MenuRow, compact_menu_panel, menu_separator},
    scroll::{ThinScrollbar, tracked_overflow_y_scrollbar},
    toggle::Toggle,
    tooltip::{TooltipSpec, with_tooltip},
};
use num_traits::ToPrimitive;
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::HashMap,
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
use symbol_menu::{
    InstrumentSelectorAvailability, InstrumentSelectorMenu, InstrumentSelectorState,
    instrument_dialog_content, instrument_selector,
};
#[cfg(test)]
use terminal_chrome::{
    CaptionPlatform, CaptionPointerOwner, DrawingHistoryControl, WindowMoveGestureTransition,
    caption_keyboard_activates, caption_pointer_owner, chrome_control_foreground,
    connection_presentation,
};
use terminal_chrome::{
    WindowCommand, WindowMoveGestureEvent, WorkspaceTabBarState, button_activation,
    chrome_button_style, chrome_tooltip, exchange_mark, fullscreen_escape_command, gpui_color,
    header_icon, nucleus_chart_theme, series_glyph, terminal_header,
    window_move_gesture_transition, workspace_title_bar, workspace_title_bar_visible,
};
use terminal_view::{
    TerminalShellInit, WorkspaceSplitDrag, terminal_root, workspace_tab_strip, workspace_tabs_root,
};
use workspace_layout::workspace_market_area;
#[cfg(test)]
use workspace_layout::workspace_split_ratio;

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

const SIDE_PANEL_INITIAL_WIDTH: f32 = 340.0;
const SIDE_PANEL_MINIMUM_WIDTH: f32 = 300.0;
const SIDE_PANEL_MAXIMUM_WIDTH: f32 = 480.0;
const SIDE_PANEL_RESIZE_HANDLE_WIDTH: f32 = 8.0;
const MAXIMUM_STATUS_CHARACTERS: usize = 160;
const MAXIMUM_OPEN_WORKSPACES: usize = 8;
const MAXIMUM_PANES_PER_WORKSPACE: usize = 4;

#[derive(Clone, Copy)]
struct PlanLimits {
    workspaces: usize,
    panes_per_workspace: usize,
    indicators_per_chart: usize,
    extended_timeframes: bool,
}

fn current_plan_limits() -> PlanLimits {
    // Authentication is mandatory, but billing is intentionally not a
    // product-access boundary during early access. Keep one capability shape
    // until paid-plan enforcement is deliberately enabled.
    PlanLimits {
        workspaces: MAXIMUM_OPEN_WORKSPACES,
        panes_per_workspace: MAXIMUM_PANES_PER_WORKSPACE,
        indicators_per_chart: usize::MAX,
        extended_timeframes: true,
    }
}
const CHART_CONTEXT_MENU_WIDTH: f32 = 228.0;
const CHART_CONTEXT_MENU_ROW_HEIGHT: f32 = 32.0;
const CHART_CONTEXT_MENU_SEPARATOR_HEIGHT: f32 = 1.0;
const PRICE_AXIS_FLYOUT_WIDTH: f32 = 296.0;
const PRICE_AXIS_FLYOUT_GAP: f32 = 4.0;
const PRICE_AXIS_MENU_GAP: f32 = 4.0;
const ACCOUNT_MENU_GAP: f32 = 4.0;
const OVERLAY_EDGE_MARGIN: f32 = 8.0;
const TIMEFRAME_MENU_WIDTH: f32 = 168.0;
const TIMEFRAME_FLYOUT_WIDTH: f32 = 136.0;
const TIMEFRAME_FLYOUT_GAP: f32 = 5.0;
const QUICK_TIMEFRAME_POPUP_WIDTH: f32 = 300.0;
const QUICK_TIMEFRAME_POPUP_TOP: f32 = 64.0;
const TIMEFRAME_TYPEAHEAD_LIMIT: usize = 8;
const CHART_SETTINGS_MENU_WIDTH: f32 = 260.0;
const WORKSPACE_TITLE_BAR_HEIGHT: f32 = 42.0;
const WORKSPACE_TAB_ICON_HIT: f32 = 24.0;
const WORKSPACE_TAB_ICON_GLYPH: f32 = 13.0;
// Bound UI work when a provider delivers a burst of updates. Remaining mailbox
// messages stay queued and wake the next GPUI frame.
const MARKET_MESSAGES_PER_FRAME: usize = 64;
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

    const fn description(self) -> &'static str {
        match self {
            Self::ExitWithDesktop => {
                "When you close Axiusflow, everything stops. Prices will not keep updating until you open Axiusflow again."
            }
            Self::KeepEngineWarm => {
                "When you close Axiusflow, a small part of the app stays open so Axiusflow can start faster next time. Live prices do not keep updating."
            }
            Self::KeepMarketsLive => {
                "When you close Axiusflow, your selected markets keep receiving live prices in the background. This uses internet data and some computer resources. Turn on Live retention to use this option."
            }
        }
    }

    #[cfg(test)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleToggle {
    AutoStart,
    LiveRetention,
}

impl LifecycleToggle {
    const fn id(self) -> &'static str {
        match self {
            Self::AutoStart => "settings_start_automatically",
            Self::LiveRetention => "settings_live_retention",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::AutoStart => "Start automatically",
            Self::LiveRetention => "Live retention",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::AutoStart => {
                "Start Axiusflow's background service when you sign in to your computer, so Axiusflow is ready faster when you open it."
            }
            Self::LiveRetention => {
                "Allow selected markets to keep receiving live prices after you close Axiusflow. This uses internet data and some computer resources in the background. Turning it off also turns off Markets live."
            }
        }
    }

    const fn toggle(self) -> fn(&mut TerminalApp, &mut Context<TerminalApp>) {
        match self {
            Self::AutoStart => TerminalApp::toggle_engine_autostart,
            Self::LiveRetention => TerminalApp::toggle_markets_live_permission,
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

/// Once recovery begins, transient retry states stay in one stable recovering
/// presentation until the provider is either streaming or terminally stopped.
const fn stabilized_connection_state(
    previous: Option<FeedConnectionState>,
    incoming: FeedConnectionState,
) -> FeedConnectionState {
    match (previous, incoming) {
        (
            Some(FeedConnectionState::Disconnected),
            FeedConnectionState::Discovering
            | FeedConnectionState::Authenticating
            | FeedConnectionState::Recovering,
        )
        | (
            Some(FeedConnectionState::Recovering),
            FeedConnectionState::Disconnected
            | FeedConnectionState::Discovering
            | FeedConnectionState::Authenticating
            | FeedConnectionState::Recovering,
        ) => FeedConnectionState::Recovering,
        _ => incoming,
    }
}

fn stable_connection_message(state: FeedConnectionState, incoming: String) -> String {
    match state {
        FeedConnectionState::Disconnected => "Market data offline".to_string(),
        FeedConnectionState::Recovering => "Reconnecting market data".to_string(),
        _ => incoming,
    }
}

const DEFAULT_RITHMIC_LISTING_QUERY: &str = "MNQ";

/// The instrument menu should open with a default provider listing instead of
/// a blank list. A completed Coinbase selection consumes the previous search
/// results (one-shot selection authorization), so an empty idle browser means
/// a fresh default search must be dispatched.
fn instrument_listing_refresh_needed(
    browser: &rithmic_shell::RithmicSymbolBrowser,
    selection_pending: bool,
) -> bool {
    !selection_pending
        && browser.results().is_empty()
        && !browser.search_pending()
        && !browser.has_retained_search()
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
    menu_state: WorkspaceMenuState,
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
    market_state: WorkspaceMarketState,
    series_browser: rithmic_history::RithmicSeriesBrowser,
    series_message: String,
    rithmic_reconnect: RithmicReconnectState,
    symbol_input: Option<Entity<InputState>>,
    indicator_input: Entity<InputState>,
    timeframe_input: Entity<InputState>,
    indicator_message: Option<String>,
    chrome_overlay: Option<ChromeOverlay>,
    chrome_overlay_phase: ChromeOverlayPhase,
    chrome_overlay_generation: u64,
    timeframe_menu_flyout: Option<TimeframeMenuGroup>,
    timeframe_flyout_close_token: u64,
    timeframe_hover_regions: u32,
    timeframe_trigger_bounds: Option<Bounds<Pixels>>,
    chart_type_trigger_bounds: Option<Bounds<Pixels>>,
    chrome_selection: usize,
    chrome_focus: FocusHandle,
    instrument_exchange: InstrumentExchangeUi,
    provider: TerminalProvider,
    coinbase_product: Option<InstallProviderInstrument>,
    coinbase_switch: CoinbaseSwitchState,
    coinbase_interval: ChartInterval,
    coinbase_pending_interval: Option<ChartInterval>,
    coinbase_pending_product: Option<InstallProviderInstrument>,
    coinbase_pending_sequence: Option<u64>,
    /// The selection to fall back to if the switch in flight never loads.
    ///
    /// A failed switch must leave the trader on the chart they had, not on an
    /// empty surface, so the previous demand is restored rather than abandoned.
    coinbase_previous_selection: Option<(Option<InstallProviderInstrument>, ChartInterval)>,
    restored_viewport: Option<(i64, i64)>,
    last_persisted_viewport: Option<(i64, i64)>,
    pending_chart_context_menu: Option<ChartContextRequest>,
    pending_pane_activate: PaneActivationRequest,
    resource_class: ConsumerResourceClass,
    chart_chrome: chart_chrome::ChartChromePreferences,
    retained_chart_presentation: RetainedChartPresentation,
    #[cfg(feature = "diagnostics")]
    foreground_interactions: ForegroundInteractionDiagnostics,
    #[cfg(feature = "diagnostics")]
    live_evidence_enabled: bool,
    #[cfg(feature = "diagnostics")]
    live_evidence_publications: u16,
}

#[derive(Default)]
struct WorkspaceMenuState {
    dom_column_open: bool,
    timeframe_flyout_keyboard: bool,
    chrome_list_keyboard: bool,
}

#[derive(Default)]
struct WorkspaceMarketState {
    symbol_selection_pending: bool,
    rithmic_autoload_started: bool,
}

#[derive(Default)]
struct RetainedChartPresentation {
    indicators: Vec<ChartIndicatorState>,
    price_precision: Option<u8>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PaneActivationRequest {
    #[default]
    None,
    Pending,
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

/// Where a Coinbase symbol or timeframe change is in its handover.
///
/// A switch is a presentation change, so it never blanks the chart. The chart on
/// screen keeps streaming its own series until the replacement's covering
/// snapshot arrives, and only then is it swapped. `Pending` is the window
/// between the request and the mailbox marker that orders it; `Swapping` is the
/// window between that marker and the snapshot that replaces the chart;
/// `Initializing` keeps that replacement covered until its live handoff lands.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CoinbaseSwitchState {
    #[default]
    Idle,
    Pending,
    Swapping,
    Initializing,
}

impl CoinbaseSwitchState {
    const fn is_pending(self) -> bool {
        matches!(self, Self::Pending)
    }

    /// Whether a chart is on screen that no longer matches the committed
    /// selection, and must not be fed the replacement's incremental updates.
    const fn is_swapping(self) -> bool {
        matches!(self, Self::Swapping)
    }

    const fn in_progress(self) -> bool {
        matches!(self, Self::Pending | Self::Swapping | Self::Initializing)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChromeOverlay {
    Instrument,
    Indicator,
    Timeframe,
    QuickTimeframe,
    ChartType,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TimeframeMenuGroup {
    Ticks,
    Minutes,
    Hours,
    Days,
    Weeks,
    Months,
}

impl TimeframeMenuGroup {
    const fn label(self) -> &'static str {
        match self {
            Self::Ticks => "Ticks",
            Self::Minutes => "Minutes",
            Self::Hours => "Hours",
            Self::Days => "Days",
            Self::Weeks => "Weeks",
            Self::Months => "Months",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstrumentExchangeUi {
    Idle(assets::ExchangeLogo),
    Menu(assets::ExchangeLogo),
}

impl InstrumentExchangeUi {
    const fn exchange(self) -> assets::ExchangeLogo {
        match self {
            Self::Idle(exchange) | Self::Menu(exchange) => exchange,
        }
    }

    const fn is_open(self) -> bool {
        matches!(self, Self::Menu(_))
    }
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
            let (activate, request) = chart.update(cx, |chart, _| {
                (
                    chart.take_activate_request(),
                    chart.take_context_menu_request(),
                )
            });
            if activate {
                app.pending_pane_activate = PaneActivationRequest::Pending;
            }
            let had_menu = request.is_some();
            if let Some(request) = request {
                app.pending_chart_context_menu = Some(request);
            }
            if activate || had_menu {
                cx.notify();
            }
            if chart.read(cx).has_market_data() {
                app.retained_chart_presentation.indicators = chart.read(cx).indicator_states();
                app.retained_chart_presentation.price_precision =
                    chart.read(cx).selected_price_precision();
            }
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

const fn instrument_row_highlighted(
    checked: bool,
    row_index: usize,
    keyboard_selection: usize,
    keyboard_active: bool,
) -> bool {
    if keyboard_active {
        keyboard_selection == row_index
    } else {
        checked
    }
}

fn current_instrument_menu_index(entries: &[InstrumentMenuEntry]) -> Option<usize> {
    entries.iter().position(|entry| entry.checked)
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
        || !message.contains(crate::rithmic_engine_client::RITHMIC_CATALOG_READY_MESSAGE)
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
    provider: TerminalProvider,
    instrument_label: String,
    series_label: String,
    chart_type: ChartType,
    chart_type_label: String,
    instruments: Vec<InstrumentMenuEntry>,
    selected_series: Option<rithmic_history::RithmicSeries>,
    symbol_input: Option<Entity<InputState>>,
    indicator_input: Entity<InputState>,
    indicator_message: Option<String>,
    series_message: String,
    pending: HeaderPendingState,
    drawing_history: DrawingHistoryState,
    controls: HeaderControls,
    dom_visible: bool,
    connection_state: FeedConnectionState,
    chart_state: ChartState,
    delayed: bool,
    instrument_scroll: ScrollHandle,
    account: axiusflow_desktop::account::AccountMenuState,
}

/// Whether the active chart's drawing history has an edit to step back to or forward to.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DrawingHistoryState {
    can_undo: bool,
    can_redo: bool,
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
    BottomRight,
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
    superseded: bool,
    detail: &str,
) -> Option<ChartSurfaceNotice> {
    let placement = if has_market_data && !superseded {
        ChartNoticePlacement::BottomRight
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

const fn connectivity_chart_state(
    chart_state: ChartState,
    connection_state: FeedConnectionState,
    has_market_data: bool,
) -> ChartState {
    match connection_state {
        FeedConnectionState::Disconnected if has_market_data => ChartState::Stale,
        FeedConnectionState::Recovering if has_market_data => ChartState::Recovering,
        FeedConnectionState::Stopped => ChartState::Error,
        _ => chart_state,
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
        app.market_state.symbol_selection_pending,
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
    const INDICATOR: u8 = 8;
    const CHART_TYPE: u8 = 16;

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
            self.0 |= Self::INDICATOR | Self::CHART_TYPE;
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
        MarketWorkerStartup::Loading(startup) => TerminalStartupState {
            chart: None,
            chart_state: ChartState::Loading,
            chart_state_message: "waiting for a covering market snapshot".to_string(),
            replay_label: "waiting for a covering market snapshot".to_string(),
            worker_label: startup.worker_label,
            subscription_id: startup.subscription_id,
            connection_state: Some(FeedConnectionState::Discovering),
            connection_message: Some("Connecting to Coinbase public markets".to_string()),
            provider: TerminalProvider::Coinbase,
            coinbase_product: Some(startup.coinbase_product),
        },
    }
}

fn initial_symbol_message(provider: TerminalProvider) -> String {
    match provider {
        TerminalProvider::Coinbase => "Loading Coinbase public spot catalog",
        TerminalProvider::Rithmic => "Search for an entitled Rithmic Test symbol",
    }
    .to_string()
}

fn initialize_chart_chrome(
    chart: Option<&Entity<NucleusChartView>>,
    preferences: chart_chrome::ChartChromePreferences,
    cx: &mut Context<WorkspaceSurface>,
) {
    if let Some(chart) = chart {
        chart.update(cx, |chart, _| {
            chart.apply_indicator_chrome_preferences(
                preferences.indicator_name_labels_visible,
                preferences.indicator_value_labels_visible,
                preferences.indicator_price_lines_visible,
            );
            chart.set_chart_type(preferences.chart_type);
        });
    }
}

impl WorkspaceSurface {
    #[allow(clippy::too_many_arguments)]
    fn new(
        cx: &mut Context<Self>,
        startup: MarketWorkerStartup,
        market_worker: MarketDataWorker,
        lifecycle: DesktopLifecycle,
        symbol_input: Option<Entity<InputState>>,
        indicator_input: Entity<InputState>,
        timeframe_input: Entity<InputState>,
        chart_chrome: chart_chrome::ChartChromePreferences,
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
        initialize_chart_chrome(chart.as_ref(), chart_chrome, cx);
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
            menu_state: WorkspaceMenuState::default(),
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
            market_state: WorkspaceMarketState::default(),
            series_browser: rithmic_history::RithmicSeriesBrowser::default(),
            series_message: "Select a symbol before choosing a series".to_string(),
            rithmic_reconnect: RithmicReconnectState::Idle,
            symbol_input,
            indicator_input,
            timeframe_input,
            indicator_message: None,
            chrome_overlay: None,
            chrome_overlay_phase: ChromeOverlayPhase::Opening,
            chrome_overlay_generation: 0,
            timeframe_menu_flyout: None,
            timeframe_flyout_close_token: 0,
            timeframe_hover_regions: 0,
            timeframe_trigger_bounds: None,
            chart_type_trigger_bounds: None,
            chrome_selection: 0,
            chrome_focus: cx.focus_handle().tab_stop(true),
            instrument_exchange: InstrumentExchangeUi::Idle(assets::ExchangeLogo::Coinbase),
            provider,
            coinbase_product,
            coinbase_switch: CoinbaseSwitchState::Idle,
            coinbase_interval: restored_coinbase
                .map_or(ChartInterval::Minute1, |restored| restored.0),
            coinbase_pending_interval: None,
            coinbase_pending_product: None,
            coinbase_pending_sequence: None,
            coinbase_previous_selection: None,
            restored_viewport: restored_coinbase.and_then(|restored| restored.1),
            last_persisted_viewport: None,
            pending_chart_context_menu: None,
            pending_pane_activate: PaneActivationRequest::None,
            resource_class: ConsumerResourceClass::Foreground,
            chart_chrome,
            retained_chart_presentation: RetainedChartPresentation::default(),
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

    fn quick_timeframe_matches(&self, cx: &App) -> Vec<ChartInterval> {
        let query = self.timeframe_input.read(cx).value();
        self.available_intervals()
            .iter()
            .copied()
            .filter(|interval| interval.matches_typeahead(query.as_ref()))
            .collect()
    }

    fn sync_timeframe_menu_selection(&mut self) {
        self.timeframe_menu_flyout = None;
        self.menu_state.timeframe_flyout_keyboard = false;
        self.timeframe_hover_regions = 0;
        let selected_group = timeframe_interval_group(self.selected_interval());
        self.chrome_selection = timeframe_menu_groups(self.available_intervals())
            .iter()
            .position(|group| *group == selected_group)
            .unwrap_or(0);
    }

    fn open_timeframe_group(
        &mut self,
        group: TimeframeMenuGroup,
        from_keyboard: bool,
        cx: &mut Context<Self>,
    ) {
        self.retain_timeframe_flyout();
        let already_open = self.timeframe_menu_flyout == Some(group);
        if !already_open {
            self.timeframe_menu_flyout = Some(group);
            self.chrome_selection = timeframe_group_intervals(group, self.available_intervals())
                .iter()
                .position(|interval| *interval == self.selected_interval())
                .unwrap_or(0);
        }
        if self.menu_state.timeframe_flyout_keyboard != from_keyboard || !already_open {
            self.menu_state.timeframe_flyout_keyboard = from_keyboard;
            cx.notify();
        }
    }

    fn retain_timeframe_flyout(&mut self) {
        self.timeframe_flyout_close_token = self.timeframe_flyout_close_token.saturating_add(1);
    }

    fn arm_timeframe_flyout_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.retain_timeframe_flyout();
        let token = self.timeframe_flyout_close_token;
        cx.spawn_in(window, async move |app, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let _ = app.update_in(cx, |app, _, app_cx| {
                if app.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && app.timeframe_flyout_close_token == token
                {
                    app.close_timeframe_flyout(app_cx);
                }
            });
        })
        .detach();
    }

    fn hover_timeframe_menu_region(
        &mut self,
        hovered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            self.timeframe_hover_regions = self.timeframe_hover_regions.saturating_add(1);
            self.retain_timeframe_flyout();
            return;
        }
        if self.timeframe_hover_regions == 0 {
            return;
        }
        self.timeframe_hover_regions -= 1;
        if self.timeframe_hover_regions == 0 {
            self.arm_timeframe_flyout_close(window, cx);
        }
    }

    fn close_timeframe_flyout(&mut self, cx: &mut Context<Self>) {
        let Some(group) = self.timeframe_menu_flyout.take() else {
            return;
        };
        self.menu_state.timeframe_flyout_keyboard = false;
        self.timeframe_hover_regions = 0;
        self.chrome_selection = timeframe_menu_groups(self.available_intervals())
            .iter()
            .position(|item| *item == group)
            .unwrap_or(0);
        cx.notify();
    }

    fn timeframe_menu_keyboard_count(&self) -> usize {
        if let Some(group) = self.timeframe_menu_flyout {
            timeframe_group_intervals(group, self.available_intervals()).len()
        } else {
            timeframe_menu_groups(self.available_intervals()).len()
        }
    }

    fn sync_chart_type_menu_selection(&mut self, cx: &App) {
        let selected = self.chart_type(cx);
        self.chrome_selection = ChartType::ALL
            .iter()
            .position(|chart_type| *chart_type == selected)
            .unwrap_or(0);
    }

    fn apply_highlighted_chart_type(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chart_type) = ChartType::ALL.get(self.chrome_selection).copied() {
            self.set_chart_type(chart_type, cx);
            self.close_chrome_overlay(window, cx);
        }
    }

    fn sync_quick_timeframe_selection(&mut self, cx: &App) {
        let query = self.timeframe_input.read(cx).value();
        let intervals = self.quick_timeframe_matches(cx);
        self.chrome_selection = intervals
            .iter()
            .position(|interval| interval.label() == query.as_ref())
            .or_else(|| {
                intervals
                    .iter()
                    .position(|interval| *interval == self.selected_interval())
            })
            .unwrap_or(0);
    }

    fn apply_highlighted_interval(
        &mut self,
        intervals: &[ChartInterval],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(interval) = intervals.get(self.chrome_selection).copied()
            && self.select_interval(interval, cx)
        {
            self.close_chrome_overlay(window, cx);
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
        let starter_interval = matches!(
            interval,
            ChartInterval::Minute1
                | ChartInterval::Minute3
                | ChartInterval::Minute5
                | ChartInterval::Minute15
                | ChartInterval::Minute30
                | ChartInterval::Hour1
                | ChartInterval::Hour2
                | ChartInterval::Hour4
                | ChartInterval::Hour8
                | ChartInterval::Hour12
                | ChartInterval::Day1
        );
        if !current_plan_limits().extended_timeframes && !starter_interval {
            self.series_message = "This timeframe requires a paid plan".to_string();
            cx.notify();
            return false;
        }
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
            if self.instrument_exchange.exchange() != assets::ExchangeLogo::Coinbase {
                return Vec::new();
            }
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
                .map(|(index, result)| {
                    let named = result
                        .name
                        .as_deref()
                        .map(str::trim)
                        .is_some_and(|name| !name.is_empty());
                    InstrumentMenuEntry {
                        symbol: if named {
                            result.name.clone().unwrap_or_else(|| result.symbol.clone())
                        } else {
                            result.symbol.replace('-', "/")
                        },
                        checked: self
                            .coinbase_product
                            .as_ref()
                            .is_some_and(|selected| selected.provider_symbol == result.symbol),
                        selection: InstrumentMenuSelection::Coinbase(index),
                    }
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

    fn activate_chrome_list_keyboard(&mut self) {
        if matches!(
            self.chrome_overlay,
            Some(ChromeOverlay::Instrument | ChromeOverlay::Indicator)
        ) {
            self.menu_state.chrome_list_keyboard = true;
        }
    }

    fn sync_instrument_menu_keyboard(&mut self, cx: &App) {
        if self.menu_state.chrome_list_keyboard {
            return;
        }
        self.chrome_selection =
            current_instrument_menu_index(&self.instrument_entries(cx)).unwrap_or(0);
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
                if self.market_state.symbol_selection_pending {
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
                self.market_state.symbol_selection_pending = true;
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
        if overlay != ChromeOverlay::QuickTimeframe {
            self.timeframe_input.update(cx, |input, input_cx| {
                input.set_value("", window, input_cx);
            });
        }
        if overlay != ChromeOverlay::Timeframe {
            self.timeframe_menu_flyout = None;
            self.menu_state.timeframe_flyout_keyboard = false;
            self.timeframe_hover_regions = 0;
        }
        self.menu_state.chrome_list_keyboard = false;
        self.chrome_selection = match overlay {
            ChromeOverlay::Timeframe => {
                self.sync_timeframe_menu_selection();
                self.chrome_selection
            }
            ChromeOverlay::ChartType => {
                self.sync_chart_type_menu_selection(cx);
                self.chrome_selection
            }
            ChromeOverlay::QuickTimeframe => {
                self.sync_quick_timeframe_selection(cx);
                self.chrome_selection
            }
            ChromeOverlay::Instrument => {
                current_instrument_menu_index(&self.instrument_entries(cx)).unwrap_or(0)
            }
            ChromeOverlay::Indicator => 0,
        };
        match overlay {
            ChromeOverlay::Instrument => {
                self.instrument_exchange =
                    InstrumentExchangeUi::Idle(assets::ExchangeLogo::Coinbase);
                if let Some(input) = &self.symbol_input {
                    input.update(cx, |input, input_cx| input.focus(window, input_cx));
                }
                self.refresh_default_instrument_listing(cx);
                self.sync_instrument_menu_keyboard(cx);
            }
            ChromeOverlay::Indicator => {
                self.indicator_input
                    .update(cx, |input, input_cx| input.focus(window, input_cx));
            }
            ChromeOverlay::QuickTimeframe => {
                self.timeframe_input
                    .update(cx, |input, input_cx| input.focus(window, input_cx));
            }
            ChromeOverlay::Timeframe | ChromeOverlay::ChartType => {
                self.chrome_focus.focus(window, cx);
            }
        }
        cx.notify();
    }

    fn close_chrome_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chrome_overlay.is_none() || self.chrome_overlay_phase == ChromeOverlayPhase::Closing
        {
            return;
        }
        match self.chrome_overlay {
            Some(ChromeOverlay::Indicator) => {
                self.indicator_input.update(cx, |input, input_cx| {
                    input.set_value("", window, input_cx);
                });
            }
            Some(ChromeOverlay::QuickTimeframe) => {
                self.timeframe_input.update(cx, |input, input_cx| {
                    input.set_value("", window, input_cx);
                });
            }
            Some(
                ChromeOverlay::Instrument | ChromeOverlay::Timeframe | ChromeOverlay::ChartType,
            )
            | None => {}
        }
        self.chrome_focus.focus(window, cx);
        if cx.reduce_motion() {
            self.chrome_overlay = None;
            self.timeframe_menu_flyout = None;
            self.menu_state.timeframe_flyout_keyboard = false;
            self.menu_state.chrome_list_keyboard = false;
            self.timeframe_hover_regions = 0;
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
                    app.timeframe_menu_flyout = None;
                    app.menu_state.timeframe_flyout_keyboard = false;
                    app.menu_state.chrome_list_keyboard = false;
                    app.timeframe_hover_regions = 0;
                    app.chrome_overlay_phase = ChromeOverlayPhase::Opening;
                    app_cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn toggle_instrument_exchange_menu(&mut self, cx: &mut Context<Self>) {
        self.instrument_exchange = match self.instrument_exchange {
            InstrumentExchangeUi::Idle(exchange) => InstrumentExchangeUi::Menu(exchange),
            InstrumentExchangeUi::Menu(exchange) => InstrumentExchangeUi::Idle(exchange),
        };
        cx.notify();
    }

    fn set_instrument_catalog_exchange(
        &mut self,
        exchange: assets::ExchangeLogo,
        cx: &mut Context<Self>,
    ) {
        self.instrument_exchange = InstrumentExchangeUi::Idle(exchange);
        self.chrome_selection = 0;
        cx.notify();
    }

    fn on_terminal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(command) =
            fullscreen_escape_command(event.keystroke.key.as_str(), window.is_fullscreen())
        {
            command.execute(window);
            return true;
        }
        if self.consume_chrome_typeahead(event, window, cx) {
            return true;
        }
        if self.chrome_overlay.is_none() || self.chrome_overlay_phase == ChromeOverlayPhase::Closing
        {
            return false;
        }
        match event.keystroke.key.as_str() {
            "escape" => {
                if let InstrumentExchangeUi::Menu(exchange) = self.instrument_exchange {
                    self.instrument_exchange = InstrumentExchangeUi::Idle(exchange);
                    cx.notify();
                } else if self.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && self.timeframe_menu_flyout.is_some()
                {
                    self.close_timeframe_flyout(cx);
                } else {
                    self.close_chrome_overlay(window, cx);
                }
            }
            "up" => {
                if self.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && self.timeframe_menu_flyout.is_some()
                    && !self.menu_state.timeframe_flyout_keyboard
                {
                    self.menu_state.timeframe_flyout_keyboard = true;
                    cx.notify();
                    return true;
                }
                self.activate_chrome_list_keyboard();
                self.chrome_selection = self.chrome_selection.saturating_sub(1);
                cx.notify();
            }
            "down" => {
                if self.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && self.timeframe_menu_flyout.is_some()
                    && !self.menu_state.timeframe_flyout_keyboard
                {
                    self.menu_state.timeframe_flyout_keyboard = true;
                    cx.notify();
                    return true;
                }
                let count = match self.chrome_overlay {
                    Some(ChromeOverlay::Instrument) => self.instrument_entries(cx).len(),
                    Some(ChromeOverlay::Indicator) => chart_chrome::filter_indicator_specs(
                        self.indicator_input.read(cx).value().as_ref(),
                    )
                    .len(),
                    Some(ChromeOverlay::Timeframe) => self.timeframe_menu_keyboard_count(),
                    Some(ChromeOverlay::ChartType) => ChartType::ALL.len(),
                    Some(ChromeOverlay::QuickTimeframe) => self.quick_timeframe_matches(cx).len(),
                    None => 0,
                };
                self.activate_chrome_list_keyboard();
                self.chrome_selection = (self.chrome_selection + 1).min(count.saturating_sub(1));
                cx.notify();
            }
            "left" if self.chrome_overlay == Some(ChromeOverlay::Timeframe) => {
                self.close_timeframe_flyout(cx);
            }
            "right" if self.chrome_overlay == Some(ChromeOverlay::Timeframe) => {
                if self.timeframe_menu_flyout.is_none()
                    && let Some(group) = timeframe_menu_groups(self.available_intervals())
                        .get(self.chrome_selection)
                        .copied()
                {
                    self.open_timeframe_group(group, true, cx);
                }
            }
            "enter" => return self.handle_chrome_enter(window, cx),
            _ => return false,
        }
        true
    }

    fn handle_chrome_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.chrome_overlay {
            Some(ChromeOverlay::Timeframe) => {
                if let Some(group) = self.timeframe_menu_flyout {
                    if self.menu_state.timeframe_flyout_keyboard {
                        let intervals =
                            timeframe_group_intervals(group, self.available_intervals());
                        self.apply_highlighted_interval(&intervals, window, cx);
                    } else {
                        self.menu_state.timeframe_flyout_keyboard = true;
                        cx.notify();
                    }
                } else if let Some(group) = timeframe_menu_groups(self.available_intervals())
                    .get(self.chrome_selection)
                    .copied()
                {
                    self.open_timeframe_group(group, true, cx);
                }
            }
            Some(ChromeOverlay::ChartType) => self.apply_highlighted_chart_type(window, cx),
            Some(ChromeOverlay::QuickTimeframe) => {
                let intervals = self.quick_timeframe_matches(cx);
                self.apply_highlighted_interval(&intervals, window, cx);
            }
            Some(ChromeOverlay::Instrument | ChromeOverlay::Indicator) | None => return false,
        }
        true
    }

    fn consume_chrome_typeahead(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if chrome_typeahead_blocked(event) {
            return false;
        }
        if self.chrome_overlay_phase == ChromeOverlayPhase::Closing {
            return false;
        }
        if self.drawing_toolbar_state(cx).active_tool == ChartDrawingTool::Text {
            return false;
        }
        if self
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).is_editing_text())
        {
            return false;
        }
        let Some(typed) = chrome_typeahead_char(event) else {
            return false;
        };
        if event.is_held && self.chrome_overlay.is_none() {
            return false;
        }
        match self.chrome_overlay {
            None if typed.is_ascii_digit() => {
                self.begin_quick_timeframe(typed, window, cx);
                true
            }
            None if typed.is_ascii_alphabetic() => self.begin_symbol_typeahead(typed, window, cx),
            Some(
                ChromeOverlay::Instrument
                | ChromeOverlay::Indicator
                | ChromeOverlay::Timeframe
                | ChromeOverlay::QuickTimeframe
                | ChromeOverlay::ChartType,
            )
            | None => false,
        }
    }

    fn begin_quick_timeframe(&mut self, typed: char, window: &mut Window, cx: &mut Context<Self>) {
        self.timeframe_input.update(cx, |input, input_cx| {
            input.set_value(typed.to_string(), window, input_cx);
            input.focus(window, input_cx);
        });
        self.open_chrome_overlay(ChromeOverlay::QuickTimeframe, window, cx);
    }

    fn begin_symbol_typeahead(
        &mut self,
        typed: char,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(input) = self.symbol_input.clone() else {
            return false;
        };
        let query = typed.to_string();
        input.update(cx, |input, input_cx| {
            input.set_value(&query, window, input_cx);
            input.focus(window, input_cx);
        });
        self.search_symbol_query(&query, cx);
        self.open_chrome_overlay(ChromeOverlay::Instrument, window, cx);
        true
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
        // A switch that has been committed but not yet drawn keeps the previous
        // chart on screen. That chart belongs to the previous series, so the
        // replacement's incremental updates must not reach it; only its covering
        // snapshot may, and that snapshot is what swaps the chart.
        let swapping = self.coinbase_switch.is_swapping();
        let next_state = match (&self.chart, update) {
            (existing, axiusflow_application::ReplayStreamUpdate::Snapshot(snapshot))
                if existing.is_none() || swapping =>
            {
                let chart_theme = nucleus_chart_theme(self.theme.mode);
                let chart = cx
                    .new(move |_| NucleusChartView::with_replay_and_theme(&snapshot, chart_theme));
                self.apply_chart_chrome_to_chart(&chart, cx);
                self.apply_retained_indicators_to_chart(&chart, cx);
                if let Some((start, end)) = self.restored_viewport {
                    chart.update(cx, |chart, _| {
                        chart.set_visible_time_range_unix_nanos(start, end);
                    });
                }
                observe_chart(Some(&chart), cx);
                self.chart = Some(chart);
                self.coinbase_switch = CoinbaseSwitchState::Initializing;
                ChartState::Ready
            }
            (Some(_), _) if swapping => {
                // The replacement has not arrived yet; the previous chart stays
                // as it is rather than being fed another series' bars.
                return;
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
            (None, _) => {
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
        // A publication says bars arrived, not that they are current. The engine
        // reports readiness separately, and while it is still loading current
        // coverage the chart is showing retained history — promoting it here is
        // what presented a stale chart as ready for the seconds before the
        // provider page and the live handoff landed.
        if next_state == ChartState::Ready && self.chart_state == ChartState::Loading {
            cx.notify();
        } else if next_state == ChartState::Ready {
            self.chart_state = ChartState::Ready;
            self.chart_state_message = "market snapshot is current".to_string();
            if self.provider == TerminalProvider::Coinbase {
                self.market_state.symbol_selection_pending = false;
                self.symbol_message = self.coinbase_product.as_ref().map_or_else(
                    || "Coinbase market ready".to_string(),
                    |product| format!("{} · Coinbase spot", product.provider_symbol),
                );
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
                self.chart_state_message = "market snapshot is current".to_string();
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

    /// Reports whether the chart on screen belongs to the selection the trader
    /// just left.
    ///
    /// A switch keeps the previous chart up rather than blanking the surface, so
    /// for as long as the replacement has not arrived the pixels are real market
    /// data from the wrong series. The surface has to say so.
    fn showing_superseded_series(&self) -> bool {
        self.chart.is_some()
            && match self.provider {
                TerminalProvider::Coinbase => self.coinbase_switch.in_progress(),
                TerminalProvider::Rithmic => self.series_browser.pending().is_some(),
            }
    }

    fn set_chart_state(&mut self, state: ChartState, message: String, cx: &mut Context<Self>) {
        if matches!(state, ChartState::Stale | ChartState::Recovering) {
            self.mark_market_stream_invalid(&message, cx);
        }
        self.chart_state = state;
        self.chart_state_message = message;
        cx.notify();
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
                    let swapping = self.coinbase_switch.is_swapping();
                    self.coinbase_switch = CoinbaseSwitchState::Idle;
                    self.coinbase_pending_interval = None;
                    self.coinbase_pending_product = None;
                    self.coinbase_pending_sequence = None;
                    self.market_state.symbol_selection_pending = false;
                    if swapping {
                        self.restore_coinbase_selection_after_failure(&message, cx);
                    } else {
                        self.coinbase_previous_selection = None;
                    }
                } else if self.provider == TerminalProvider::Coinbase && state == ChartState::Ready
                {
                    self.coinbase_switch = CoinbaseSwitchState::Idle;
                    self.coinbase_previous_selection = None;
                    self.market_state.symbol_selection_pending = false;
                    self.symbol_message = self.coinbase_product.as_ref().map_or_else(
                        || "Coinbase market ready".to_string(),
                        |product| format!("{} · Coinbase spot", product.provider_symbol),
                    );
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

    /// Commits a Coinbase switch's identity without touching the chart.
    ///
    /// The marker only says "everything after this belongs to the new
    /// selection". The chart the trader is looking at is left on screen — still
    /// its own series, still correct — under a loading notice, and is replaced
    /// in `apply_publication` when the replacement's covering snapshot arrives.
    /// Dropping it here is what produced the blank surface on every switch.
    fn apply_coinbase_switch_marker(&mut self, sequence: u64, cx: &mut Context<Self>) {
        if self.provider != TerminalProvider::Coinbase
            || !self.coinbase_switch.is_pending()
            || self.coinbase_pending_sequence != Some(sequence)
        {
            return;
        }
        self.coinbase_previous_selection =
            Some((self.coinbase_product.clone(), self.coinbase_interval));
        if let Some(interval) = self.coinbase_pending_interval.take() {
            self.coinbase_interval = interval;
        }
        if let Some(product) = self.coinbase_pending_product.take() {
            self.coinbase_product = Some(product);
            // Price levels belong to one instrument: a product switch drops
            // the old book back to loading instead of showing BTC levels
            // under an ETH selection. Interval-only switches keep the book.
            self.dom.update(cx, |dom, dom_cx| {
                dom.clear(dom_cx);
            });
        }
        self.coinbase_pending_sequence = None;
        self.coinbase_switch = if self.chart.is_some() {
            CoinbaseSwitchState::Swapping
        } else {
            CoinbaseSwitchState::Idle
        };
        self.retain_chart_presentation(cx);
        self.restored_viewport = None;
        self.last_persisted_viewport = None;
        self.chart_state = ChartState::Loading;
        cx.notify();
    }

    /// Restores the selection a failed switch was replacing.
    ///
    /// The chart on screen is still the previous series, so restoring means
    /// re-stating its demand and reporting an actionable error over it — never
    /// leaving the trader on a surface with no data and no way back.
    fn restore_coinbase_selection_after_failure(&mut self, detail: &str, cx: &mut Context<Self>) {
        let Some((product, interval)) = self.coinbase_previous_selection.take() else {
            return;
        };
        self.coinbase_product.clone_from(&product);
        self.coinbase_interval = interval;
        self.coinbase_pending_interval = None;
        self.coinbase_pending_product = None;
        self.coinbase_pending_sequence = None;
        self.coinbase_switch = CoinbaseSwitchState::Idle;
        self.dom.update(cx, |dom, dom_cx| {
            dom.clear(dom_cx);
        });
        let restored = product.and_then(|product| {
            self.market_worker
                .try_select_coinbase(product, interval)
                .ok()
        });
        if let Some(sequence) = restored {
            self.coinbase_pending_sequence = Some(sequence);
            self.coinbase_pending_interval = Some(interval);
            self.coinbase_switch = CoinbaseSwitchState::Pending;
        }
        self.series_message = format!("{detail} — showing {}", interval.label());
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
        let loading = self.chart_state == ChartState::Loading;
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.set_asset_loading(loading) {
                    chart_cx.notify();
                }
            });
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
        let state = stabilized_connection_state(self.connection_state, state);
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
        // Depth follows the same honesty rule as the empty panel: a fresh
        // demand restarts from loading, and only a concrete stop marks the
        // book unavailable. An engine replacement additionally clears books
        // from the dead incarnation, whose reset generations would fence
        // every new frame out forever.
        match state {
            FeedConnectionState::Disconnected => {
                self.dom.update(cx, |dom, dom_cx| {
                    dom.set_connection_state(
                        axiusflow_terminal_ui::DomConnectionState::Offline,
                        dom_cx,
                    );
                });
            }
            FeedConnectionState::Discovering
            | FeedConnectionState::Authenticating
            | FeedConnectionState::Recovering => {
                self.dom.update(cx, |dom, dom_cx| {
                    if message == engine_market_worker::ENGINE_RESTARTED_MESSAGE {
                        dom.clear(dom_cx);
                    }
                    dom.set_connection_state(
                        axiusflow_terminal_ui::DomConnectionState::Recovering,
                        dom_cx,
                    );
                });
            }
            FeedConnectionState::Streaming => {
                self.dom.update(cx, |dom, dom_cx| {
                    dom.set_connection_state(
                        axiusflow_terminal_ui::DomConnectionState::Online,
                        dom_cx,
                    );
                });
            }
            FeedConnectionState::Stopped => {
                self.dom.update(cx, |dom, dom_cx| {
                    dom.mark_unavailable(dom_cx);
                });
            }
        }
        let ready_action = rithmic_ready_action(
            state,
            &message,
            &self.rithmic_reconnect,
            self.market_state.rithmic_autoload_started,
        );
        self.connection_message = Some(stable_connection_message(state, message));
        match ready_action {
            RithmicReadyAction::Reconnect(symbol) => {
                if self.search_symbol_query(&symbol, cx)
                    && let RithmicReconnectState::AwaitingSearch(target) = &self.rithmic_reconnect
                {
                    self.rithmic_reconnect = RithmicReconnectState::SearchInFlight(target.clone());
                }
            }
            RithmicReadyAction::Autoload => {
                self.market_state.rithmic_autoload_started = true;
                let _ = self.search_symbol_query(DEFAULT_RITHMIC_LISTING_QUERY, cx);
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

    fn apply_indicator_chrome_preferences(
        &mut self,
        names: bool,
        values: bool,
        price_lines: bool,
        cx: &mut Context<Self>,
    ) {
        self.chart_chrome.indicator_name_labels_visible = names;
        self.chart_chrome.indicator_value_labels_visible = values;
        self.chart_chrome.indicator_price_lines_visible = price_lines;
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.apply_indicator_chrome_preferences(names, values, price_lines);
                chart_cx.notify();
            });
        }
    }

    fn apply_chart_chrome_to_chart(
        &self,
        chart: &Entity<NucleusChartView>,
        cx: &mut Context<Self>,
    ) {
        chart.update(cx, |chart, _| {
            chart.apply_indicator_chrome_preferences(
                self.chart_chrome.indicator_name_labels_visible,
                self.chart_chrome.indicator_value_labels_visible,
                self.chart_chrome.indicator_price_lines_visible,
            );
            chart.set_chart_type(self.chart_chrome.chart_type);
            let _ = chart.apply_price_axis_menu_action(
                0,
                false,
                PriceAxisMenuAction::SetPrecision(self.retained_chart_presentation.price_precision),
            );
        });
    }

    fn retain_chart_presentation(&mut self, cx: &App) {
        if let Some(chart) = &self.chart
            && chart.read(cx).has_market_data()
        {
            self.retained_chart_presentation.indicators = chart.read(cx).indicator_states();
            self.retained_chart_presentation.price_precision =
                chart.read(cx).selected_price_precision();
        }
    }

    fn apply_retained_indicators_to_chart(
        &self,
        chart: &Entity<NucleusChartView>,
        cx: &mut Context<Self>,
    ) {
        if self.retained_chart_presentation.indicators.is_empty() {
            return;
        }
        let states = self.retained_chart_presentation.indicators.clone();
        let result = chart.update(cx, |chart, _| chart.restore_indicator_states(&states));
        if let Err(error) = result {
            eprintln!("Axiusflow chart indicators could not be restored: {error}");
        }
    }

    fn chart_type(&self, cx: &App) -> ChartType {
        self.chart
            .as_ref()
            .map_or(self.chart_chrome.chart_type, |chart| {
                chart.read(cx).chart_type()
            })
    }

    fn set_chart_type(&mut self, chart_type: ChartType, cx: &mut Context<Self>) {
        self.chart_chrome.chart_type = chart_type;
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_chart_type(chart_type);
                chart_cx.notify();
            });
        }
        let preferences = self.chart_chrome;
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = chart_chrome::save_chart_chrome_preferences(preferences) {
                    eprintln!("Axiusflow chart chrome could not be saved: {error}");
                }
            })
            .detach();
        cx.notify();
    }

    fn refresh_default_instrument_listing(&mut self, cx: &mut Context<Self>) {
        if !instrument_listing_refresh_needed(
            &self.symbol_browser,
            self.market_state.symbol_selection_pending,
        ) {
            return;
        }
        let query = if self.provider == TerminalProvider::Coinbase {
            ""
        } else {
            DEFAULT_RITHMIC_LISTING_QUERY
        };
        let _ = self.search_symbol_query(query, cx);
    }

    fn search_symbol_query(&mut self, query: &str, cx: &mut Context<Self>) -> bool {
        if self.symbol_browser.search_pending() || self.market_state.symbol_selection_pending {
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
        if self.market_state.symbol_selection_pending {
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
            self.market_state.rithmic_autoload_started = false;
        }
        self.retire_rithmic_session(cx);
    }

    fn retire_rithmic_session(&mut self, cx: &mut Context<Self>) {
        self.market_state.symbol_selection_pending = false;
        self.symbol_browser.invalidate_session();
        self.series_browser.reset();
        self.dom
            .update(cx, axiusflow_terminal_ui::ReadOnlyDomView::clear);
    }

    fn select_rithmic_symbol(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        if self.market_state.symbol_selection_pending {
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
            self.market_state.symbol_selection_pending = true;
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
                    && self.market_state.rithmic_autoload_started
                    && self.symbol_browser.selected().is_none()
                    && let Some(index) =
                        default_rithmic_contract_index(self.symbol_browser.results())
                {
                    self.select_rithmic_symbol(index, cx);
                }
                self.dispatch_retained_symbol_search(cx);
                if self.chrome_overlay == Some(ChromeOverlay::Instrument) {
                    self.sync_instrument_menu_keyboard(cx);
                }
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
                    self.market_state.symbol_selection_pending = false;
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
                    self.market_state.symbol_selection_pending = false;
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
        self.market_state.symbol_selection_pending = false;
        let recovered_series = self
            .rithmic_reconnect
            .target()
            .map_or(rithmic_history::RithmicSeries::Minute1, |target| {
                target.series
            });
        self.rithmic_reconnect = RithmicReconnectState::Idle;
        self.series_browser.reset();
        // The chart for the previous contract stays on screen under a loading
        // notice until the new one's covering history arrives. Emptying it here
        // is what made every instrument switch blank the surface first.
        self.retain_chart_presentation(cx);
        self.bridge_label = "bridge awaiting series selection".to_string();
        self.replay_label = "Selected instrument · choose a series".to_string();
        self.subscription_id = format!("{} · {}", instrument.display_symbol, instrument.venue_id);
        self.symbol_message = format!("Selected {}", instrument.display_symbol);
        self.series_message = "Choose a chart series".to_string();
        self.select_rithmic_series(recovered_series, cx);
    }

    fn select_rithmic_series(
        &mut self,
        series: rithmic_history::RithmicSeries,
        cx: &mut Context<Self>,
    ) {
        let Some(selection) = self.symbol_browser.selected() else {
            self.series_message = "Select a symbol before choosing a series".to_string();
            cx.notify();
            return;
        };
        // A newer request supersedes one still loading rather than being
        // refused: the history task cancels the older fetch and the browser
        // fences everything but the newest generation, so rapid switching lands
        // on the last thing the trader asked for.
        let superseded = self.series_browser.pending();
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
            // The chart on screen stays: it is still its own series and still
            // correct, and it is replaced only when the replacement's covering
            // history arrives. Emptying it here is what produced the blank
            // surface on every switch.
            self.retain_chart_presentation(cx);
            self.bridge_label = "bridge awaiting visible history".to_string();
            self.series_message = format!("Loading {} visible history", series.label());
            self.set_chart_state(
                ChartState::Loading,
                format!("loading {} visible history", series.label()),
                cx,
            );
        } else {
            self.series_browser.reject(request.series_generation);
            if let Some(superseded) = superseded {
                self.series_browser.restore_pending(superseded);
            }
            self.series_message = "Rithmic history worker is busy; try again".to_string();
        }
        cx.notify();
    }

    /// Re-states the demand for the series still on screen after a failed switch.
    fn restore_rithmic_series_after_failure(&mut self) {
        let Some(selected) = self.series_browser.selected() else {
            return;
        };
        let _ = self
            .market_worker
            .try_request_engine_series(EngineSeriesRequest {
                selection_generation: selected.selection_generation,
                series_generation: selected.series_generation,
                interval: selected.series.interval(),
            });
    }

    fn apply_rithmic_history(
        &mut self,
        selection_generation: std::num::NonZeroUsize,
        series_generation: std::num::NonZeroUsize,
        result: Result<Box<MarketWorkerBootstrap>, String>,
        cx: &mut Context<Self>,
    ) {
        let bootstrap = match result {
            Ok(bootstrap) => bootstrap,
            Err(error) => {
                if self.series_browser.reject(series_generation) {
                    let (series_message, chart_message) =
                        rithmic_engine_history::history_failure_messages(&error);
                    self.series_message = series_message;
                    self.set_chart_state(ChartState::Error, chart_message, cx);
                    // The chart on screen is still the previous series, so its
                    // demand is restated rather than abandoned: the trader keeps
                    // a live chart and an actionable error, not an empty surface.
                    self.restore_rithmic_series_after_failure();
                }
                return;
            }
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
        let chart =
            cx.new(move |_| NucleusChartView::with_replay_and_theme(&snapshot, chart_theme));
        self.apply_chart_chrome_to_chart(&chart, cx);
        self.apply_retained_indicators_to_chart(&chart, cx);
        self.chart = Some(chart);
        observe_chart(self.chart.as_ref(), cx);
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

    /// Whether a market is selected. The header enables the DOM toggle on this
    /// and `toggle_dom` opens on it, so the two cannot drift apart again.
    fn has_market_selection(&self) -> bool {
        self.symbol_browser.selected().is_some() || self.coinbase_product.is_some()
    }

    fn toggle_dom(&mut self, cx: &mut Context<Self>) {
        if self.has_market_selection() {
            self.side_panel = (self.side_panel != Some(SidePanel::Dom)).then_some(SidePanel::Dom);
            if self.side_panel.is_none() {
                self.menu_state.dom_column_open = false;
            }
            cx.notify();
        }
    }

    fn toggle_dom_column_menu(&mut self, cx: &mut Context<Self>) {
        self.menu_state.dom_column_open = !self.menu_state.dom_column_open;
        cx.notify();
    }

    fn close_dom_column_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu_state.dom_column_open {
            self.menu_state.dom_column_open = false;
            cx.notify();
        }
    }

    fn close_side_panel(&mut self, cx: &mut Context<Self>) {
        self.side_panel_resize = None;
        self.menu_state.dom_column_open = false;
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
            self.retain_chart_presentation(cx);
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

    fn undo_drawing(&mut self, cx: &mut Context<Self>) {
        self.step_drawing_history(true, cx);
    }

    fn redo_drawing(&mut self, cx: &mut Context<Self>) {
        self.step_drawing_history(false, cx);
    }

    fn step_drawing_history(&mut self, undo: bool, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            let stepped = chart.update(cx, |chart, chart_cx| {
                let stepped = if undo {
                    chart.undo_drawing()
                } else {
                    chart.redo_drawing()
                };
                if stepped {
                    chart_cx.notify();
                }
                stepped
            });
            if stepped {
                cx.notify();
            }
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

    fn clear_indicators(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.clear_indicators() {
                    chart_cx.notify();
                }
            });
            self.retain_chart_presentation(cx);
            cx.notify();
        }
    }

    fn add_indicator(&mut self, indicator: ChartIndicator, cx: &mut Context<Self>) -> bool {
        let Some(chart) = self.chart.clone() else {
            self.indicator_message = Some("Chart data is not available yet".to_string());
            cx.notify();
            return false;
        };
        let maximum = current_plan_limits().indicators_per_chart;
        if chart.read(cx).indicator_states().len() >= maximum {
            self.indicator_message = Some(format!(
                "Your plan supports at most {maximum} indicators per chart"
            ));
            cx.notify();
            return false;
        }
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
                self.retain_chart_presentation(cx);
                true
            }
            Err(error) => {
                self.indicator_message = Some(error.to_string());
                cx.notify();
                false
            }
        }
    }

    /// Reports what the header's undo and redo controls may offer for the active chart.
    fn drawing_history_state(&self, cx: &App) -> DrawingHistoryState {
        self.chart
            .as_ref()
            .map_or_else(DrawingHistoryState::default, |chart| {
                let chart = chart.read(cx);
                DrawingHistoryState {
                    can_undo: chart.can_undo_drawing(),
                    can_redo: chart.can_redo_drawing(),
                }
            })
    }

    fn drawing_toolbar_state(&self, cx: &App) -> DrawingToolbarState {
        self.chart
            .as_ref()
            .map_or_else(DrawingToolbarState::default, |chart| {
                DrawingToolbarState::from_chart(chart.read(cx))
            })
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
        ProviderCatalogRejectionReason::SearchTimedOut if coinbase => {
            "The Coinbase market search timed out; try again"
        }
        ProviderCatalogRejectionReason::SearchTimedOut => {
            "The Rithmic symbol search timed out; try again"
        }
        ProviderCatalogRejectionReason::SelectionTimedOut if coinbase => {
            "The Coinbase market selection timed out; try again"
        }
        ProviderCatalogRejectionReason::SelectionTimedOut => {
            "The Rithmic symbol selection timed out; try again"
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

fn run_desktop_readiness_command(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let usage = "usage: axiusflow_desktop --desktop-readiness <report-path>";
    let report_path = arguments.next().ok_or_else(|| usage.to_string())?;
    if arguments.next().is_some() {
        return Err(usage.to_string());
    }
    let engine = axiusflow_local_engine_client::sibling_engine_executable()?;
    let mut client = axiusflow_local_engine_client::connect_or_start_engine(&engine)?;
    let ready = client.ready().clone();
    let workspace = client.restore_workspace()?;
    client.attach_client(u64::from(std::process::id()))?;
    let status = client.engine_status()?;
    if status.process_id == 0
        || status.connected_desktop_clients == 0
        || status.providers.is_empty()
        || axiusflow_engine_protocol::EngineShutdownState::try_from(status.shutdown_state)
            != Ok(axiusflow_engine_protocol::EngineShutdownState::Running)
    {
        return Err("candidate market service did not reach readiness".to_string());
    }
    let account = axiusflow_desktop::account::fetch_account_status(&mut client)?;
    axiusflow_desktop::account::verify_account_readiness(&account)?;
    // The probe shuts the engine down at the end, so it must never run
    // against a live session: this attach is counted, so more than one
    // client means another desktop is using the resident engine.
    if status.connected_desktop_clients > 1 {
        return Err("candidate readiness probe refused: another desktop session is attached to the resident engine".to_string());
    }
    let release = axiusflow_platform_runtime::current_release_identity();
    if ready.release_identity != release.release_identity
        || ready.install_generation != release.install_generation
    {
        return Err("candidate desktop and engine release identities do not match".to_string());
    }
    let report = LifecycleReadinessReport {
        schema_version: 1,
        release_identity: release.release_identity,
        install_generation: release.install_generation,
        engine_process_id: status.process_id,
        workspace_revision: workspace.workspace_revision,
        provider_count: status.providers.len(),
        authenticated_ipc_ready: true,
        workspace_restored: true,
        market_service_ready: true,
        account_ipc_ready: true,
    };
    client.shutdown_engine()?;
    let mut encoded = serde_json::to_vec(&report)
        .map_err(|_| "candidate readiness report could not be encoded".to_string())?;
    encoded.push(b'\n');
    std::fs::write(std::path::Path::new(&report_path), encoded)
        .map_err(|_| "candidate readiness report could not be written".to_string())
}

#[derive(serde::Serialize)]
// Wire mirror of the launcher readiness JSON: the flat boolean shape is the
// cross-binary contract, not internal state.
#[allow(clippy::struct_excessive_bools)]
struct LifecycleReadinessReport {
    schema_version: u32,
    release_identity: String,
    install_generation: u64,
    engine_process_id: u32,
    workspace_revision: u64,
    provider_count: usize,
    authenticated_ipc_ready: bool,
    workspace_restored: bool,
    market_service_ready: bool,
    account_ipc_ready: bool,
}

#[cfg(feature = "diagnostics")]
fn run_desktop_conformance_command(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let usage = "usage: axiusflow_desktop --desktop-conformance <report-path>";
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
                        let typed = app
                            .symbol_input
                            .as_ref()
                            .is_some_and(|input| !input.read(cx).value().trim().is_empty());
                        app.menu_state.chrome_list_keyboard = typed;
                        app.chrome_selection = if typed {
                            0
                        } else {
                            current_instrument_menu_index(&app.instrument_entries(cx)).unwrap_or(0)
                        };
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

fn subscribe_timeframe_input(
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
            move |_, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    terminal.update(cx, |app, app_cx| {
                        if app.chrome_overlay != Some(ChromeOverlay::QuickTimeframe) {
                            return;
                        }
                        let intervals = app.quick_timeframe_matches(app_cx);
                        app.apply_highlighted_interval(&intervals, window, app_cx);
                    });
                }
                InputEvent::Change => {
                    terminal.update(cx, |app, app_cx| {
                        if app.chrome_overlay != Some(ChromeOverlay::QuickTimeframe) {
                            return;
                        }
                        let query = input.read(app_cx).value();
                        if query.is_empty() {
                            app.close_chrome_overlay(window, app_cx);
                            return;
                        }
                        if query.len() > TIMEFRAME_TYPEAHEAD_LIMIT {
                            let truncated: String =
                                query.chars().take(TIMEFRAME_TYPEAHEAD_LIMIT).collect();
                            input.update(app_cx, |input, input_cx| {
                                input.set_value(truncated, window, input_cx);
                            });
                        }
                        app.sync_quick_timeframe_selection(app_cx);
                        app_cx.notify();
                    });
                }
                InputEvent::Focus | InputEvent::Blur => {}
            },
        )
        .detach();
}

fn workspace_surface_entity(
    bootstrap: MarketWorkerStartup,
    market_worker: MarketDataWorker,
    lifecycle: &DesktopLifecycle,
    chart_chrome: chart_chrome::ChartChromePreferences,
    window: &mut Window,
    cx: &mut App,
) -> Entity<WorkspaceSurface> {
    let symbol_input = Some(symbol_input_for_startup(&bootstrap, window, cx));
    let search_input = symbol_input.clone();
    let indicator_input =
        cx.new(|cx| InputState::new(window, cx).placeholder("Search native indicators"));
    let indicator_search_input = indicator_input.clone();
    let timeframe_input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("1m, 5, 1H, 1D")
            .centered()
    });
    let timeframe_search_input = timeframe_input.clone();
    let workspace_lifecycle = lifecycle.clone();
    let workspace = cx.new(move |cx| {
        WorkspaceSurface::new(
            cx,
            bootstrap,
            market_worker,
            workspace_lifecycle,
            symbol_input,
            indicator_input,
            timeframe_input,
            chart_chrome,
        )
    });
    lifecycle.register_terminal(&workspace);
    subscribe_symbol_input(search_input, &workspace, window, cx);
    subscribe_indicator_input(&indicator_search_input, &workspace, window, cx);
    subscribe_timeframe_input(&timeframe_search_input, &workspace, window, cx);
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

#[derive(Clone, Copy)]
struct ChartContextMenuState {
    pane_count: usize,
    flags: u8,
}

impl ChartContextMenuState {
    const READY: u8 = 1;
    const SPLIT: u8 = 2;
    const DRAWINGS: u8 = 4;
    const INDICATORS: u8 = 8;
    const COPY_PRICE: u8 = 16;

    const fn enabled(self, flag: u8) -> bool {
        self.flags & flag != 0
    }
}

#[derive(Clone, Copy)]
struct ChartContextMenuItem {
    id: &'static str,
    icon: HugeIcon,
    label: &'static str,
    enabled: bool,
    action: ChartContextAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChartContextAction {
    CopyPrice,
    Reset,
    ClearDrawings,
    ClearIndicators,
    Split(ChartSplitDirection),
    Close,
    Settings,
}

impl ChartContextAction {
    const fn is_destructive(self) -> bool {
        matches!(self, Self::ClearDrawings | Self::ClearIndicators)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PriceAxisMenuFlyout {
    #[default]
    None,
    Labels,
    Lines,
    ScaleMode,
    YAxis,
    Precision,
}

impl PriceAxisMenuFlyout {
    const fn geometry(self) -> (f32, f32, f32, f32) {
        match self {
            Self::None => (0.0, 0.0, 0.0, 0.0),
            Self::Labels => (10.0, 1.0, 0.0, 0.0),
            Self::Lines => (5.0, 0.0, 1.0, 0.0),
            Self::ScaleMode => (4.0, 0.0, 4.0, 2.0),
            Self::YAxis => (2.0, 0.0, 5.0, 2.0),
            Self::Precision => (9.0, 0.0, 6.0, 2.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ChartContextMenu {
    workspace_id: u64,
    pane_id: u64,
    position: gpui::Point<Pixels>,
    kind: ChartContextKind,
    flyout: PriceAxisMenuFlyout,
    copy_price: Option<SharedString>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspaceShellKind {
    Window,
    Tabs,
}

/// Shell menus are small booleans by design; the account dropdown joins the
/// two chart-menu options rather than growing a separate menu stack.
#[allow(clippy::struct_excessive_bools)]
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
    chart_context_menu: Option<ChartContextMenu>,
    chart_settings_menu: Option<ChartContextMenu>,
    account_menu_open: bool,
    account_menu_anchor: Option<gpui::Point<Pixels>>,
    chart_chrome: chart_chrome::ChartChromePreferences,
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
    fn new(init: TerminalShellInit, lifecycle: DesktopLifecycle, cx: &mut Context<Self>) -> Self {
        let mut workspaces = init.workspaces;
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
        let active = init
            .active_workspace_id
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
        let workspace_persistence = (init.workspace_shell == WorkspaceShellKind::Tabs)
            .then(|| {
                WorkspaceLayoutPersistence::new(init.workspace_revision, init.layout_generation)
            })
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
            workspace_factory: init.workspace_factory,
            workspace_persistence,
            persisted_layout,
            persisted_active_workspace_id,
            workspace_error: None,
            workspace_drag: None,
            chart_context_menu: None,
            chart_settings_menu: None,
            account_menu_open: false,
            account_menu_anchor: None,
            chart_chrome: init.chart_chrome,
            window_move_pending: false,
            closing: false,
        }
    }

    fn active_surface(&self) -> Entity<WorkspaceSurface> {
        let workspace = &self.workspaces[self.active];
        workspace.panes[workspace.active_pane].surface.clone()
    }

    fn chart_chrome_for_new_surface(&self, cx: &App) -> chart_chrome::ChartChromePreferences {
        let mut preferences = self.chart_chrome;
        preferences.chart_type = self.active_surface().read(cx).chart_type(cx);
        preferences
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

    fn absorb_pane_activate_requests(&mut self, cx: &mut Context<Self>) {
        let mut requested = None;
        for workspace in &self.workspaces {
            for pane in &workspace.panes {
                let activate = pane.surface.update(cx, |surface, _| {
                    let pending = surface.pending_pane_activate == PaneActivationRequest::Pending;
                    surface.pending_pane_activate = PaneActivationRequest::None;
                    pending
                });
                if activate {
                    requested = Some((workspace.id, pane.id));
                }
            }
        }
        if let Some((workspace_id, pane_id)) = requested {
            self.select_pane(workspace_id, pane_id, cx);
        }
    }

    fn select_drawing_tool_on_active_workspace(
        &mut self,
        tool: ChartDrawingTool,
        cx: &mut Context<Self>,
    ) {
        let panes: Vec<_> = self.workspaces[self.active]
            .panes
            .iter()
            .map(|pane| pane.surface.clone())
            .collect();
        for surface in panes {
            surface.update(cx, |surface, surface_cx| {
                surface.select_drawing_tool(tool, surface_cx);
            });
        }
    }

    fn absorb_chart_context_menu_requests(&mut self, cx: &mut Context<Self>) {
        let mut requested = None;
        for workspace in &self.workspaces {
            for pane in &workspace.panes {
                let request = pane
                    .surface
                    .update(cx, |surface, _| surface.pending_chart_context_menu.take());
                if let Some(request) = request {
                    requested = Some(ChartContextMenu {
                        workspace_id: workspace.id,
                        pane_id: pane.id,
                        position: request.position,
                        kind: request.kind,
                        flyout: PriceAxisMenuFlyout::None,
                        copy_price: request.copy_price,
                    });
                }
            }
        }
        if let Some(menu) = requested {
            self.open_chart_context_menu(menu, cx);
        }
    }

    fn open_chart_context_menu(&mut self, menu: ChartContextMenu, cx: &mut Context<Self>) {
        self.select_pane(menu.workspace_id, menu.pane_id, cx);
        self.chart_settings_menu = None;
        self.chart_context_menu = Some(menu);
        cx.notify();
    }

    fn close_chart_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.chart_context_menu.take().is_some() {
            cx.notify();
        }
    }

    fn close_chart_settings_menu(&mut self, cx: &mut Context<Self>) {
        if self.chart_settings_menu.take().is_some() {
            cx.notify();
        }
    }

    /// Opens the account dropdown under the avatar click point, or closes it
    /// when already open. The stored anchor keeps the panel glued to the
    /// avatar's rendered position instead of a fixed screen corner.
    fn toggle_account_menu_at(&mut self, anchor: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        if self.account_menu_open {
            self.account_menu_open = false;
            self.account_menu_anchor = None;
        } else {
            self.account_menu_open = true;
            self.account_menu_anchor = Some(anchor);
        }
        cx.notify();
    }

    fn close_account_menu(&mut self, cx: &mut Context<Self>) {
        if self.account_menu_open {
            self.account_menu_open = false;
            cx.notify();
        }
    }

    fn account_menu_overlay(
        &self,
        terminal: &Entity<Self>,
        viewport: gpui::Size<Pixels>,
    ) -> Option<AnyElement> {
        if !self.account_menu_open {
            return None;
        }
        let account = axiusflow_desktop::account::DesktopAccount::shared().map_or_else(
            axiusflow_desktop::account::unavailable_menu_state,
            |account| account.menu_state(),
        );
        Some(account_menu_layer(
            terminal,
            &account,
            self.account_menu_anchor,
            viewport,
            &self.theme,
        ))
    }

    fn finish_chart_context_menu(
        &mut self,
        menu: ChartContextMenu,
        action: ChartContextAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chart_context_menu = None;
        self.select_pane(menu.workspace_id, menu.pane_id, cx);
        match action {
            ChartContextAction::CopyPrice => {
                if let Some(price) = menu.copy_price.as_deref() {
                    cx.write_to_clipboard(ClipboardItem::new_string(price.to_string()));
                }
            }
            ChartContextAction::Reset => {
                self.update_context_menu_pane(&menu, WorkspaceSurface::reset_chart_view, cx);
            }
            ChartContextAction::ClearDrawings => {
                self.update_context_menu_pane(&menu, WorkspaceSurface::clear_drawings, cx);
            }
            ChartContextAction::ClearIndicators => {
                self.update_context_menu_pane(&menu, WorkspaceSurface::clear_indicators, cx);
            }
            ChartContextAction::Split(direction) => {
                self.split_active_pane(direction, window, cx);
            }
            ChartContextAction::Close => {
                self.close_active_pane(&ClosePane, window, cx);
            }
            ChartContextAction::Settings => {
                self.chart_settings_menu = Some(menu);
            }
        }
        cx.notify();
    }

    fn update_context_menu_pane(
        &self,
        menu: &ChartContextMenu,
        update: impl FnOnce(&mut WorkspaceSurface, &mut Context<WorkspaceSurface>),
        cx: &mut Context<Self>,
    ) {
        if let Some(workspace) = self
            .workspaces
            .iter()
            .find(|workspace| workspace.id == menu.workspace_id)
            && let Some(pane) = workspace.panes.iter().find(|pane| pane.id == menu.pane_id)
        {
            pane.surface.update(cx, update);
        }
    }

    fn context_menu_chart_objects(&self, menu: &ChartContextMenu, cx: &App) -> (bool, bool) {
        self.workspaces
            .iter()
            .find(|workspace| workspace.id == menu.workspace_id)
            .and_then(|workspace| workspace.panes.iter().find(|pane| pane.id == menu.pane_id))
            .and_then(|pane| pane.surface.read(cx).chart.as_ref())
            .map_or((false, false), |chart| {
                let chart = chart.read(cx);
                (chart.drawing_count() > 0, chart.has_indicators())
            })
    }

    fn context_menu_price_axis_state(
        &self,
        menu: &ChartContextMenu,
        pane: usize,
        left: bool,
        cx: &App,
    ) -> Option<PriceAxisMenuState> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.id == menu.workspace_id)
            .and_then(|workspace| workspace.panes.iter().find(|pane| pane.id == menu.pane_id))
            .and_then(|pane| pane.surface.read(cx).chart.as_ref())
            .and_then(|chart| chart.read(cx).price_axis_menu_state(pane, left))
    }

    fn apply_price_axis_menu(
        &mut self,
        menu: &ChartContextMenu,
        action: PriceAxisMenuAction,
        cx: &mut Context<Self>,
    ) {
        let ChartContextKind::PriceAxis { pane, left } = menu.kind else {
            return;
        };
        self.select_pane(menu.workspace_id, menu.pane_id, cx);
        self.update_context_menu_pane(
            menu,
            |surface, surface_cx| {
                if let Some(chart) = &surface.chart {
                    chart.update(surface_cx, |chart, chart_cx| {
                        chart.apply_price_axis_menu_action(pane, left, action);
                        chart_cx.notify();
                    });
                }
            },
            cx,
        );
        if matches!(
            action,
            PriceAxisMenuAction::ToggleIndicatorNameLabels
                | PriceAxisMenuAction::ToggleIndicatorValueLabels
                | PriceAxisMenuAction::ToggleIndicatorPriceLines
        ) {
            self.broadcast_indicator_chrome(menu, cx);
        }
        if let PriceAxisMenuAction::SetLeft(next_left) = action
            && let Some(open) = &mut self.chart_context_menu
            && let ChartContextKind::PriceAxis {
                left: open_left, ..
            } = &mut open.kind
        {
            *open_left = next_left;
        }
        cx.notify();
    }

    fn toggle_price_axis_flyout(&mut self, flyout: PriceAxisMenuFlyout, cx: &mut Context<Self>) {
        if let Some(menu) = &mut self.chart_context_menu {
            menu.flyout = if menu.flyout == flyout {
                PriceAxisMenuFlyout::None
            } else {
                flyout
            };
            cx.notify();
        }
    }

    fn broadcast_indicator_chrome(&mut self, menu: &ChartContextMenu, cx: &mut Context<Self>) {
        let (names, values, price_lines) = self
            .workspaces
            .iter()
            .find(|workspace| workspace.id == menu.workspace_id)
            .and_then(|workspace| workspace.panes.iter().find(|pane| pane.id == menu.pane_id))
            .and_then(|pane| pane.surface.read(cx).chart.clone())
            .map_or(
                (
                    self.chart_chrome.indicator_name_labels_visible,
                    self.chart_chrome.indicator_value_labels_visible,
                    self.chart_chrome.indicator_price_lines_visible,
                ),
                |chart| {
                    let chart = chart.read(cx);
                    (
                        chart.indicator_name_labels_visible(),
                        chart.indicator_value_labels_visible(),
                        chart.indicator_price_lines_visible(),
                    )
                },
            );
        self.chart_chrome.indicator_name_labels_visible = names;
        self.chart_chrome.indicator_value_labels_visible = values;
        self.chart_chrome.indicator_price_lines_visible = price_lines;
        self.chart_chrome.chart_type = self.active_surface().read(cx).chart_type(cx);
        for workspace in &self.workspaces {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, surface_cx| {
                    surface.apply_indicator_chrome_preferences(
                        names,
                        values,
                        price_lines,
                        surface_cx,
                    );
                });
            }
        }
        let preferences = self.chart_chrome;
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = chart_chrome::save_chart_chrome_preferences(preferences) {
                    eprintln!("Axiusflow chart chrome could not be saved: {error}");
                }
            })
            .detach();
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
        let maximum = current_plan_limits().workspaces;
        if self.workspaces.len() >= maximum {
            self.workspace_error = Some(format!(
                "Your plan supports at most {maximum} open workspaces"
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
        let surface = workspace_surface_entity(
            pane.startup,
            pane.worker,
            &self.lifecycle,
            self.chart_chrome_for_new_surface(cx),
            window,
            cx,
        );
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
            self.workspace_error = Some("This window cannot open another chart pane".to_string());
            cx.notify();
            return;
        };
        let workspace = &self.workspaces[self.active];
        let maximum = current_plan_limits().panes_per_workspace;
        if workspace.panes.len() >= maximum {
            self.workspace_error = Some(format!(
                "Your plan supports at most {maximum} charts per workspace"
            ));
            cx.notify();
            return;
        }
        let (product, interval, drawing_tool) = {
            let source = workspace.panes[workspace.active_pane].surface.read(cx);
            let Some(product) = source.coinbase_product.clone() else {
                self.workspace_error = Some("The active pane has no market to copy".to_string());
                cx.notify();
                return;
            };
            (
                product,
                source.coinbase_interval,
                source.drawing_toolbar_state(cx).active_tool,
            )
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
        let surface = workspace_surface_entity(
            pane.startup,
            pane.worker,
            &self.lifecycle,
            self.chart_chrome_for_new_surface(cx),
            window,
            cx,
        );
        surface.update(cx, |surface, surface_cx| {
            surface.apply_theme(&self.theme, surface_cx);
            surface.set_market_resource_class(ConsumerResourceClass::Foreground);
            surface.set_market_message_wake(self.market_frame_wake.callback());
            surface.select_drawing_tool(drawing_tool, surface_cx);
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

    fn set_lifetime_mode(&mut self, mode: DesktopLifetimeMode, cx: &mut Context<Self>) {
        let current = self.lifecycle.presentation();
        if current.pending || current.mode == mode {
            return;
        }
        if mode == DesktopLifetimeMode::KeepMarketsLive && !current.markets_live_permitted {
            return;
        }
        self.request_lifecycle_preferences(
            LifecyclePreferenceRequest {
                mode,
                autostart_enabled: current.autostart_enabled,
                markets_live_permitted: current.markets_live_permitted,
            },
            cx,
        );
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

    fn request_sign_in(cx: &mut Context<Self>) {
        let result = axiusflow_desktop::account::DesktopAccount::shared().map_or_else(
            || Err("sign-in is unavailable".to_string()),
            |account| account.request_sign_in(),
        );
        if let Err(error) = result {
            eprintln!("Axiusflow sign-in degraded: {error}");
        }
        cx.notify();
    }

    fn sign_out(cx: &mut Context<Self>) {
        let result = axiusflow_desktop::account::DesktopAccount::shared().map_or_else(
            || Err("sign-in is unavailable".to_string()),
            |account| account.request_sign_out(),
        );
        if let Err(error) = result {
            eprintln!("Axiusflow sign-out degraded: {error}");
        }
        cx.notify();
    }

    fn reopen_browser_page(cx: &mut Context<Self>) {
        let result = axiusflow_desktop::account::DesktopAccount::shared().map_or_else(
            || Err("sign-in is unavailable".to_string()),
            |account| account.reopen_browser(),
        );
        if let Err(error) = result {
            eprintln!("Axiusflow browser reopen degraded: {error}");
        }
        cx.notify();
    }

    fn cancel_sign_in(cx: &mut Context<Self>) {
        let result = axiusflow_desktop::account::DesktopAccount::shared().map_or_else(
            || Err("sign-in is unavailable".to_string()),
            |account| account.request_cancel(),
        );
        if let Err(error) = result {
            eprintln!("Axiusflow sign-in cancellation degraded: {error}");
        }
        cx.notify();
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
        if event.keystroke.key.as_str() == "escape"
            && (self.chart_context_menu.is_some() || self.chart_settings_menu.is_some())
        {
            self.close_chart_context_menu(cx);
            self.close_chart_settings_menu(cx);
            cx.stop_propagation();
            return;
        }
        if self.workspace_drag.is_some() && event.keystroke.key.as_str() == "escape" {
            cx.stop_active_drag(window);
            self.end_workspace_drag(cx);
            cx.stop_propagation();
            return;
        }
        if self.chart_context_menu.is_some() || self.chart_settings_menu.is_some() {
            return;
        }
        let handled = self.active_surface().update(cx, |workspace, workspace_cx| {
            workspace.on_terminal_key_down(event, window, workspace_cx)
        });
        if handled {
            window.prevent_default();
            cx.stop_propagation();
        }
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
        if !self.frame_poll_gate.try_schedule() {
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
                if let Some(account) = axiusflow_desktop::account::DesktopAccount::shared()
                    && account.poll()
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
                let authenticated = axiusflow_desktop::account::DesktopAccount::shared()
                    .is_some_and(|account| account.authenticated());
                let mut diagnostics = Vec::new();
                for workspace in &terminal.workspaces {
                    for pane in &workspace.panes {
                        let surface = pane.surface.clone();
                        let pending = surface.update(cx, |workspace, workspace_cx| {
                            if authenticated && workspace.should_poll_market() {
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

impl TerminalApp {
    fn start_market_wake_listener(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.market_wake_listener_started.is_some() {
            return;
        }
        self.market_wake_listener_started = Some(());
        // Authentication must progress even when a suspended provider has
        // no events to wake this window. Poll only account presentation;
        // a changed view schedules a frame that also drains retained data.
        cx.spawn_in(window, async move |terminal, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if terminal
                    .update_in(cx, |terminal, window, terminal_cx| {
                        if let Some(account) = axiusflow_desktop::account::DesktopAccount::shared()
                            && account.poll()
                        {
                            terminal.schedule_market_frame(window, terminal_cx);
                            terminal_cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
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

    fn chart_surface_menus(
        &self,
        terminal: &Entity<Self>,
        pane_count: usize,
        chart_has_market_data: bool,
        viewport: gpui::Size<Pixels>,
        cx: &App,
    ) -> (Option<AnyElement>, Option<AnyElement>) {
        let context_menu = self.chart_context_menu.clone().map(|menu| {
            if let ChartContextKind::PriceAxis { pane, left } = menu.kind {
                let state = self
                    .context_menu_price_axis_state(&menu, pane, left, cx)
                    .unwrap_or(PriceAxisMenuState {
                        flags: PriceAxisMenuState::PRICE_LINE
                            | PriceAxisMenuState::LAST_VALUE
                            | PriceAxisMenuState::TITLE
                            | PriceAxisMenuState::COUNTDOWN
                            | PriceAxisMenuState::INDICATOR_NAMES
                            | PriceAxisMenuState::INDICATOR_VALUES
                            | PriceAxisMenuState::INDICATOR_PRICE_LINES
                            | PriceAxisMenuState::AUTO_SCALE
                            | PriceAxisMenuState::ALIGN_LABELS,
                        mode: 0,
                        left,
                        precision: None,
                    });
                return price_axis_menu_layer(terminal, &menu, state, viewport, &self.theme);
            }
            let (has_drawings, has_indicators) = self.context_menu_chart_objects(&menu, cx);
            let mut flags = 0;
            if menu.copy_price.is_some() {
                flags |= ChartContextMenuState::COPY_PRICE;
            }
            if chart_has_market_data {
                flags |= ChartContextMenuState::READY;
            }
            if self.workspace_factory.is_some() && pane_count < MAXIMUM_PANES_PER_WORKSPACE {
                flags |= ChartContextMenuState::SPLIT;
            }
            if has_drawings {
                flags |= ChartContextMenuState::DRAWINGS;
            }
            if has_indicators {
                flags |= ChartContextMenuState::INDICATORS;
            }
            chart_context_menu_layer(
                terminal,
                &menu,
                ChartContextMenuState { pane_count, flags },
                viewport,
                &self.theme,
            )
        });
        let preference_error = self.lifecycle.preference_error();
        let settings_menu = self.chart_settings_menu.clone().map(|menu| {
            chart_settings_menu_layer(
                terminal,
                &menu,
                self.lifecycle.presentation(),
                preference_error.as_deref(),
                viewport,
                &self.theme,
            )
        });
        (context_menu, settings_menu)
    }
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
    chart_chrome: chart_chrome::ChartChromePreferences,
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
        if argument == "--desktop-readiness" {
            run_desktop_readiness_command(arguments)?;
            return Ok(None);
        }
        #[cfg(feature = "diagnostics")]
        if argument == "--desktop-conformance" {
            run_desktop_conformance_command(arguments).expect("desktop burst conformance passes");
            return Ok(None);
        }
        #[cfg(feature = "diagnostics")]
        if argument == "--desktop-endurance" {
            run_desktop_endurance_command(arguments).expect("desktop endurance conformance passes");
            return Ok(None);
        }
        #[cfg(feature = "diagnostics")]
        if argument == "--capture-native-transitions" {
            transition_capture::run_transition_capture_command(arguments)
                .expect("transition capture completes");
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
        let (startup, worker, factory) = engine_market_worker::start()?;
        workspace_factory = Some(factory);
        (vec![(startup, worker)], Vec::new(), lifecycle)
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
        chart_chrome: chart_chrome::load_chart_chrome_preferences(),
    }))
}

fn main() {
    let account =
        match axiusflow_desktop::account::DesktopAccount::install(u64::from(std::process::id())) {
            Ok(account) => account,
            Err(error) => {
                eprintln!("Axiusflow account client could not start: {error}");
                run_onboarding();
                return;
            }
        };
    if !wait_for_authenticated_account(&account, Duration::from_secs(2)) {
        run_onboarding();
        return;
    }
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

fn wait_for_authenticated_account(
    account: &axiusflow_desktop::account::DesktopAccount,
    timeout: Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let _ = account.poll();
        if account.authenticated() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn run_onboarding() {
    application()
        .with_assets(assets::AxiusflowAssets)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            cx.text_system()
                .add_fonts(vec![
                    Cow::Borrowed(include_bytes!(
                        "../../../crates/ui/design_system/HKGrotesk-Regular.ttf"
                    )),
                    Cow::Borrowed(include_bytes!(
                        "../../../crates/ui/design_system/HKGrotesk-Bold.ttf"
                    )),
                ])
                .expect("the bundled HK Grotesk fonts are valid");
            let options = desktop_window_options(0, cx);
            cx.open_window(options, |window, cx| {
                let screen = cx.new(|_| onboarding::OnboardingApp::new());
                let closing = screen.clone();
                // Terminal mounting replaces this window's content in place.
                // Only quit here while still onboarding; after the terminal
                // mounts its own should-close owns the close and the
                // app-level shutdown owns quit.
                window.on_window_should_close(cx, move |_, cx| {
                    if closing.read(cx).has_terminal() {
                        return true;
                    }
                    cx.quit();
                    true
                });
                screen
            })
            .expect("the Axiusflow onboarding window opens");
            cx.activate(true);
        });
}

fn run_desktop(configured: ConfiguredDesktop, lifecycle: DesktopLifecycle) {
    application()
        .with_assets(assets::AxiusflowAssets)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            cx.text_system()
                .add_fonts(vec![
                    Cow::Borrowed(include_bytes!(
                        "../../../crates/ui/design_system/HKGrotesk-Regular.ttf"
                    )),
                    Cow::Borrowed(include_bytes!(
                        "../../../crates/ui/design_system/HKGrotesk-Bold.ttf"
                    )),
                ])
                .expect("the bundled HK Grotesk fonts are valid");
            mount_desktop(configured, lifecycle, None, cx);
        });
}

/// Mounts the authenticated workspace in the existing onboarding window.
/// Startup I/O has completed on a background worker before entering GPUI.
fn mount_desktop(
    configured: ConfiguredDesktop,
    lifecycle: DesktopLifecycle,
    mut existing_window: Option<&mut Window>,
    cx: &mut App,
) -> Option<Entity<TerminalApp>> {
    let market_workers = configured.market_workers;
    let workspace_panes = configured.workspace_panes;
    let restored_workspace = configured.restored_workspace;
    let workspace_factory = configured.workspace_factory;
    let layout = configured.layout;
    let chart_chrome = configured.chart_chrome;
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
    let mut existing_root = None;
    match layout {
        DesktopLayout::Windows => {
            for (window_index, (bootstrap, market_worker)) in market_workers.into_iter().enumerate()
            {
                let window_lifecycle = lifecycle.clone();
                let window_factory = workspace_factory.clone();
                let build = move |window: &mut Window, cx: &mut App| {
                    terminal_root(
                        bootstrap,
                        market_worker,
                        window_factory,
                        &window_lifecycle,
                        chart_chrome,
                        window,
                        cx,
                    )
                };
                if let Some(window) = existing_window.take() {
                    existing_root = Some(build(window, cx));
                } else {
                    let options = desktop_window_options(window_index, cx);
                    cx.open_window(options, build)
                        .expect("the Axiusflow terminal window opens");
                }
            }
        }
        DesktopLayout::WorkspaceTabs => {
            let workspace_factory =
                workspace_factory.expect("workspace layout has a market workspace factory");
            let build = move |window: &mut Window, cx: &mut App| {
                workspace_tabs_root(
                    workspace_panes,
                    &restored_workspace,
                    workspace_factory,
                    &lifecycle,
                    chart_chrome,
                    window,
                    cx,
                )
            };
            if let Some(window) = existing_window {
                existing_root = Some(build(window, cx));
            } else {
                let options = desktop_window_options(0, cx);
                cx.open_window(options, build)
                    .expect("the Axiusflow workspace window opens");
            }
        }
    }
    cx.activate(true);
    existing_root
}
#[cfg(test)]
mod tests {
    use super::{
        CHART_CONTEXT_MENU_ROW_HEIGHT, CHART_CONTEXT_MENU_WIDTH, CHROME_MENU_FOOTER_HEIGHT,
        CHROME_MENU_LIST_HEIGHT, CHROME_MENU_MAX_HEIGHT, CHROME_MENU_SEARCH_HEIGHT,
        CHROME_MENU_WIDTH, COINBASE_ENTITLEMENT_ID, COINBASE_INTERVALS, CaptionPlatform,
        CaptionPointerOwner, ChartNoticePlacement, ChartNoticeTone, ChartState, ChromeOverlayPhase,
        DesktopLifetimeMode, HeaderControls, InputEvent, InstrumentMenuEntry,
        InstrumentMenuSelection, LifecycleToggle, OVERLAY_EDGE_MARGIN, PRICE_AXIS_MENU_GAP,
        PriceAxisMenuFlyout, PriceAxisMenuRow, ProviderCatalogCommand, RithmicReadyAction,
        RithmicReconnectState, RithmicReconnectTarget, RithmicSessionRetirement, SidePanel,
        SidePanelResize, SymbolInputAction, SymbolSubmitDecision, TIMEFRAME_FLYOUT_GAP,
        TIMEFRAME_FLYOUT_WIDTH, TIMEFRAME_MENU_WIDTH, TerminalProvider, TimeframeMenuGroup,
        WORKSPACE_TAB_GAP, WORKSPACE_TAB_STRIP_PADDING_LEFT, WORKSPACE_TAB_WIDTH, WindowCommand,
        WindowMoveGestureEvent, WindowMoveGestureTransition, WorkspaceDragState,
        active_workspace_after_close, bounded_status_detail, caption_keyboard_activates,
        caption_pointer_owner, catalog_rejection_message, chart_status_detail,
        chart_surface_notice, chrome_control_foreground, chrome_menu_extent,
        chrome_overlay_progress, chrome_typeahead_char_from, claim_once, clamp_anchored_menu_left,
        clamp_chart_context_menu_origin, clamp_price_axis_menu_origin, connection_presentation,
        connectivity_chart_state, current_instrument_menu_index, default_rithmic_contract_index,
        durable_workspace_viewport, finish_desktop_shutdown, fullscreen_escape_command, gpui_color,
        instrument_listing_refresh_needed, instrument_row_highlighted, instrument_selector_label,
        nucleus_chart_theme, price_axis_flyout_rows, price_axis_root_rows, publication_chart_state,
        reconciled_bridge_state, reconnect_contract_index, reorder_workspace_ids,
        resized_side_panel_width, rithmic_ready_action, series_selector_label,
        should_finish_chrome_overlay_close, split_lifetime_mode, stabilized_connection_state,
        stable_connection_message, symbol_input_action, symbol_submit_decision,
        timeframe_flyout_height, timeframe_flyout_offset, timeframe_flyout_row_is_active,
        timeframe_group_intervals, timeframe_interval_group, timeframe_menu_groups,
        timeframe_menu_row_label, timeframe_overlay_extent, timeframe_overlay_left,
        window_move_gesture_transition, workspace_drag_destination, workspace_drag_translation,
        workspace_label, workspace_series, workspace_split_ratio, workspace_switch,
        workspace_title_bar_visible, wrapped_workspace_index,
    };
    #[cfg(feature = "diagnostics")]
    use super::{FOREGROUND_INTERACTION_SAMPLE_CAPACITY, ForegroundInteractionDiagnostics};
    use axiusflow_chart_integration::{ChartSplitDirection, NucleusChartTheme, PriceAxisMenuState};
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

    #[test]
    fn instrument_menu_requests_a_default_listing_only_when_idle_and_empty() {
        use crate::rithmic_shell::RithmicSymbolBrowser;
        use std::num::NonZeroUsize;

        let startup = NonZeroUsize::MIN;
        let mut browser = RithmicSymbolBrowser::coinbase_catalog_awaiting_search(startup, "");
        assert!(
            !instrument_listing_refresh_needed(&browser, false),
            "startup search is already pending"
        );

        let result = ProviderInstrumentSummary {
            symbol: "BTC-USD".to_string(),
            exchange: "coinbase".to_string(),
            name: Some("BTC/USD".to_string()),
            product_code: Some("BTC-USD".to_string()),
            instrument_type: Some("spot".to_string()),
            expiration_date: None,
        };
        assert!(browser.apply_results(startup, vec![result]));
        assert!(
            !instrument_listing_refresh_needed(&browser, false),
            "populated listing needs no refresh"
        );

        let selection = browser.select(0).expect("catalog result is selectable");
        assert!(
            !instrument_listing_refresh_needed(&browser, true),
            "in-flight selection defers the refresh"
        );
        assert!(browser.confirm_selection(selection.generation));
        assert!(browser.consume_completed_search(selection.search_generation));
        assert!(
            instrument_listing_refresh_needed(&browser, false),
            "consumed selection authorization reopens as a fresh default listing"
        );

        browser
            .retain_latest_search("ETH")
            .expect("typed query validates");
        assert!(
            !instrument_listing_refresh_needed(&browser, false),
            "a retained typed query outranks the default listing"
        );
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
    fn timeframe_menu_groups_every_catalog_interval() {
        let groups: Vec<TimeframeMenuGroup> = ChartInterval::ALL
            .into_iter()
            .map(timeframe_interval_group)
            .collect();
        assert_eq!(
            groups,
            [
                TimeframeMenuGroup::Ticks,
                TimeframeMenuGroup::Minutes,
                TimeframeMenuGroup::Minutes,
                TimeframeMenuGroup::Minutes,
                TimeframeMenuGroup::Minutes,
                TimeframeMenuGroup::Minutes,
                TimeframeMenuGroup::Hours,
                TimeframeMenuGroup::Hours,
                TimeframeMenuGroup::Hours,
                TimeframeMenuGroup::Hours,
                TimeframeMenuGroup::Hours,
                TimeframeMenuGroup::Days,
                TimeframeMenuGroup::Days,
                TimeframeMenuGroup::Weeks,
                TimeframeMenuGroup::Months,
            ]
        );
        let coinbase_groups: Vec<TimeframeMenuGroup> = COINBASE_INTERVALS
            .iter()
            .copied()
            .map(timeframe_interval_group)
            .collect();
        assert_eq!(
            coinbase_groups,
            [
                TimeframeMenuGroup::Minutes,
                TimeframeMenuGroup::Minutes,
                TimeframeMenuGroup::Minutes,
                TimeframeMenuGroup::Minutes,
                TimeframeMenuGroup::Minutes,
                TimeframeMenuGroup::Hours,
                TimeframeMenuGroup::Hours,
                TimeframeMenuGroup::Hours,
                TimeframeMenuGroup::Hours,
                TimeframeMenuGroup::Hours,
                TimeframeMenuGroup::Days,
                TimeframeMenuGroup::Weeks,
                TimeframeMenuGroup::Months,
            ]
        );
    }

    #[test]
    fn timeframe_menu_uses_dual_group_and_interval_containers() {
        assert_eq!(
            timeframe_menu_groups(&ChartInterval::ALL),
            [
                TimeframeMenuGroup::Ticks,
                TimeframeMenuGroup::Minutes,
                TimeframeMenuGroup::Hours,
                TimeframeMenuGroup::Days,
                TimeframeMenuGroup::Weeks,
                TimeframeMenuGroup::Months,
            ]
        );
        assert_eq!(
            timeframe_menu_groups(COINBASE_INTERVALS),
            [
                TimeframeMenuGroup::Minutes,
                TimeframeMenuGroup::Hours,
                TimeframeMenuGroup::Days,
                TimeframeMenuGroup::Weeks,
                TimeframeMenuGroup::Months,
            ]
        );
        assert_eq!(
            timeframe_group_intervals(TimeframeMenuGroup::Minutes, &ChartInterval::ALL),
            [
                ChartInterval::Minute1,
                ChartInterval::Minute3,
                ChartInterval::Minute5,
                ChartInterval::Minute15,
                ChartInterval::Minute30,
            ]
        );
        assert_eq!(timeframe_menu_row_label(ChartInterval::Minute1), "1 Minute");
        assert_eq!(timeframe_menu_row_label(ChartInterval::Hour4), "4 Hours");
        assert_eq!(timeframe_menu_row_label(ChartInterval::Day1), "1 Day");
        assert_eq!(TimeframeMenuGroup::Minutes.label(), "Minutes");
        assert!(
            timeframe_menu_groups(COINBASE_INTERVALS)
                .into_iter()
                .all(|group| !timeframe_group_intervals(group, COINBASE_INTERVALS).is_empty()),
            "a hovered group owns its submenu; the root list does not keep a flyout open"
        );
        assert!((timeframe_flyout_offset(1) - CHART_CONTEXT_MENU_ROW_HEIGHT).abs() < f32::EPSILON);
        assert!(
            (timeframe_flyout_height(1) - (CHART_CONTEXT_MENU_ROW_HEIGHT + 2.0)).abs()
                < f32::EPSILON,
            "submenu height is the rows plus the 1px border"
        );
        assert!(
            (timeframe_overlay_extent(5, None).0 - TIMEFRAME_MENU_WIDTH).abs() < f32::EPSILON,
            "closed menu must not reserve a dead gap beside the root list"
        );
        let hours = timeframe_overlay_extent(5, Some((1, 5)));
        assert!(
            (hours.0 - (TIMEFRAME_MENU_WIDTH + TIMEFRAME_FLYOUT_GAP + TIMEFRAME_FLYOUT_WIDTH))
                .abs()
                < f32::EPSILON
        );
        assert!(hours.1 >= timeframe_flyout_offset(1) + timeframe_flyout_height(5));
        assert!(
            !timeframe_flyout_row_is_active(ChartInterval::Hour1, ChartInterval::Minute1, None, 0),
            "hovering a group must not mark its first row selected"
        );
        assert!(timeframe_flyout_row_is_active(
            ChartInterval::Minute1,
            ChartInterval::Minute1,
            None,
            0
        ));
        assert!(timeframe_flyout_row_is_active(
            ChartInterval::Hour1,
            ChartInterval::Minute1,
            Some(0),
            0
        ));
    }

    #[test]
    fn chrome_typeahead_opens_digits_as_intervals_and_letters_as_symbols() {
        assert_eq!(chrome_typeahead_char_from("1", Some("1"), false), Some('1'));
        assert_eq!(chrome_typeahead_char_from("m", Some("m"), false), Some('m'));
        assert_eq!(chrome_typeahead_char_from("m", Some("M"), true), Some('M'));
        assert_eq!(chrome_typeahead_char_from("m", None, true), Some('M'));
        assert_eq!(chrome_typeahead_char_from("a", Some("a"), false), Some('a'));
        assert_eq!(chrome_typeahead_char_from("enter", None, false), None);
        assert_eq!(
            COINBASE_INTERVALS
                .iter()
                .copied()
                .filter(|interval| interval.matches_typeahead("1m"))
                .collect::<Vec<_>>(),
            [ChartInterval::Minute1]
        );
        assert_eq!(
            COINBASE_INTERVALS
                .iter()
                .copied()
                .filter(|interval| interval.matches_typeahead("1M"))
                .collect::<Vec<_>>(),
            [ChartInterval::Month1]
        );
        assert!(ChartInterval::Minute1.matches_typeahead("1"));
        assert!(ChartInterval::Hour1.matches_typeahead("1"));
        assert!(!ChartInterval::Minute1.matches_typeahead("1M"));
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
        assert!(
            DesktopLifetimeMode::ExitWithDesktop
                .description()
                .contains("Prices will not keep updating")
        );
        assert!(
            DesktopLifetimeMode::KeepEngineWarm
                .description()
                .contains("start faster")
        );
        assert!(
            DesktopLifetimeMode::KeepMarketsLive
                .description()
                .contains("internet data")
        );
        assert_eq!(LifecycleToggle::AutoStart.label(), "Start automatically");
        assert!(
            LifecycleToggle::AutoStart
                .description()
                .contains("when you sign in")
        );
        assert!(
            LifecycleToggle::LiveRetention
                .description()
                .contains("after you close Axiusflow")
        );

        for description in [
            DesktopLifetimeMode::ExitWithDesktop.description(),
            DesktopLifetimeMode::KeepEngineWarm.description(),
            DesktopLifetimeMode::KeepMarketsLive.description(),
            LifecycleToggle::AutoStart.description(),
            LifecycleToggle::LiveRetention.description(),
        ] {
            assert!(!description.contains("resident engine"));
            assert!(!description.contains("resource mode"));
        }

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
        assert!((resized_side_panel_width(resize, -500.0) - 480.0).abs() < f32::EPSILON);
        assert!((resized_side_panel_width(resize, 1_000.0) - 300.0).abs() < f32::EPSILON);
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
    fn instrument_menu_highlights_only_the_live_market_until_keyboard_moves() {
        assert!(instrument_row_highlighted(true, 4, 0, false));
        assert!(
            !instrument_row_highlighted(false, 0, 0, false),
            "opening the menu must not paint catalog index 0 as the live stream"
        );
        assert!(instrument_row_highlighted(false, 0, 0, true));
        assert!(!instrument_row_highlighted(true, 4, 0, true));
        assert!(instrument_row_highlighted(true, 4, 4, true));
    }

    #[test]
    fn instrument_menu_index_follows_the_checked_live_market() {
        let entries = [
            InstrumentMenuEntry {
                symbol: "BTC/USD".into(),
                checked: false,
                selection: InstrumentMenuSelection::Coinbase(0),
            },
            InstrumentMenuEntry {
                symbol: "ETH/USD".into(),
                checked: true,
                selection: InstrumentMenuSelection::Coinbase(1),
            },
        ];
        assert_eq!(current_instrument_menu_index(&entries), Some(1));
        assert_eq!(
            current_instrument_menu_index(&[InstrumentMenuEntry {
                symbol: "AAVE/USD".into(),
                checked: false,
                selection: InstrumentMenuSelection::Coinbase(0),
            }]),
            None
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
            ProviderCatalogRejectionReason::SearchTimedOut,
            ProviderCatalogRejectionReason::SelectionTimedOut,
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
        assert_eq!(
            catalog_rejection_message(
                ProviderCatalogRejectionReason::SearchTimedOut,
                ProviderCatalogCommand::Search,
                false,
            ),
            "The Rithmic symbol search timed out; try again"
        );
        assert_eq!(
            catalog_rejection_message(
                ProviderCatalogRejectionReason::SelectionTimedOut,
                ProviderCatalogCommand::Selection,
                false,
            ),
            "The Rithmic symbol selection timed out; try again"
        );
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
                false,
                "Rithmic market worker stopped",
            )
            .expect("retained chart error notice")
            .placement,
            ChartNoticePlacement::BottomRight
        );
        assert_eq!(
            chart_surface_notice(
                stopped.chart_state(false).expect("stopped chart state"),
                false,
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

    /// A switch leaves the previous chart on screen so the surface never goes
    /// blank. That chart is real market data from the market the trader just
    /// left, so it has to be covered and named — a corner spinner over live
    /// candles reads as the new selection already streaming.
    #[test]
    fn a_superseded_chart_is_covered_and_named_not_left_looking_current() {
        let switching =
            chart_surface_notice(ChartState::Loading, true, true, "Loading 5m market history")
                .expect("a switch in flight is announced");
        assert_eq!(switching.placement, ChartNoticePlacement::Center);
        assert_eq!(
            switching.detail.as_deref(),
            Some("Loading 5m market history")
        );

        // A repair behind the chart the trader is actually looking at is
        // different: it stays out of the way.
        let repairing =
            chart_surface_notice(ChartState::Loading, true, false, "repairing coverage")
                .expect("a repair is announced");
        assert_eq!(repairing.placement, ChartNoticePlacement::BottomRight);
    }

    /// A load in flight is not an outage, and labelling it as one is what made
    /// an ordinary switch look like the feed had dropped.
    #[test]
    fn a_load_in_flight_reads_as_loading_unless_the_feed_is_actually_down() {
        assert_eq!(
            connection_presentation(
                TerminalProvider::Coinbase,
                FeedConnectionState::Streaming,
                ChartState::Loading,
                false,
            )
            .0,
            "Coinbase · Loading"
        );
        // A recovery in flight stays explicitly reconnecting and uses the
        // positive progress treatment instead of looking like a red failure.
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Recovering,
                ChartState::Loading,
                false,
            )
            .0,
            "Test · Reconnecting"
        );
        // It never outranks a feed that is down, because then the load is not
        // going to finish.
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Disconnected,
                ChartState::Recovering,
                false,
            )
            .0,
            "Offline"
        );
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Recovering,
                ChartState::Recovering,
                false,
            )
            .0,
            "Test · Reconnecting"
        );
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Recovering,
                ChartState::Stale,
                false,
            )
            .0,
            "Test · Reconnecting"
        );
    }

    #[test]
    fn reconnect_retry_states_do_not_flicker_back_to_offline() {
        assert_eq!(
            stabilized_connection_state(
                Some(FeedConnectionState::Streaming),
                FeedConnectionState::Disconnected,
            ),
            FeedConnectionState::Disconnected
        );
        assert_eq!(
            stabilized_connection_state(
                Some(FeedConnectionState::Disconnected),
                FeedConnectionState::Discovering,
            ),
            FeedConnectionState::Recovering
        );
        assert_eq!(
            stabilized_connection_state(
                Some(FeedConnectionState::Recovering),
                FeedConnectionState::Disconnected,
            ),
            FeedConnectionState::Recovering
        );
        assert_eq!(
            stabilized_connection_state(
                Some(FeedConnectionState::Recovering),
                FeedConnectionState::Streaming,
            ),
            FeedConnectionState::Streaming
        );
        assert_eq!(
            stable_connection_message(
                FeedConnectionState::Recovering,
                "Coinbase realtime disconnected".to_string(),
            ),
            "Reconnecting market data"
        );
        assert_eq!(
            connectivity_chart_state(ChartState::Ready, FeedConnectionState::Recovering, true,),
            ChartState::Recovering
        );
        assert_eq!(
            connectivity_chart_state(ChartState::Error, FeedConnectionState::Disconnected, true,),
            ChartState::Stale
        );
    }

    #[test]
    fn header_lifecycle_values_are_truthfully_labeled() {
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Disconnected,
                ChartState::Loading,
                false,
            )
            .0,
            "Offline"
        );
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Streaming,
                ChartState::Ready,
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
            )
            .0,
            "Coinbase · Live"
        );
        let controls = HeaderControls::from_state(true, true).with_chart_controls(true);
        assert!(controls.enabled(HeaderControls::INSTRUMENT));
        assert!(controls.enabled(HeaderControls::SERIES));
        assert!(controls.enabled(HeaderControls::DOM));
        assert!(controls.enabled(HeaderControls::INDICATOR));
        assert!(controls.enabled(HeaderControls::CHART_TYPE));
    }

    #[test]
    fn chart_controls_follow_retained_data_instead_of_transient_chart_state() {
        let retained_chart_controls =
            HeaderControls::from_state(true, false).with_chart_controls(true);
        assert!(retained_chart_controls.enabled(HeaderControls::INDICATOR));
        assert!(retained_chart_controls.enabled(HeaderControls::CHART_TYPE));

        let empty_chart_controls =
            HeaderControls::from_state(true, false).with_chart_controls(false);
        assert!(!empty_chart_controls.enabled(HeaderControls::INDICATOR));
        assert!(!empty_chart_controls.enabled(HeaderControls::CHART_TYPE));
    }

    #[test]
    fn chart_context_menu_stays_inside_the_window() {
        let overflow = clamp_chart_context_menu_origin(
            point(px(2000.0), px(2000.0)),
            size(px(800.0), px(600.0)),
        );
        assert!(overflow.x + px(CHART_CONTEXT_MENU_WIDTH) <= px(800.0) - px(OVERLAY_EDGE_MARGIN));
        assert!(overflow.y <= px(600.0) - px(OVERLAY_EDGE_MARGIN));
        assert_eq!(
            clamp_chart_context_menu_origin(
                point(px(-20.0), px(-20.0)),
                size(px(800.0), px(600.0))
            ),
            point(px(OVERLAY_EDGE_MARGIN), px(OVERLAY_EDGE_MARGIN))
        );
    }

    #[test]
    fn chart_context_remove_actions_use_destructive_color() {
        assert!(super::ChartContextAction::ClearDrawings.is_destructive());
        assert!(super::ChartContextAction::ClearIndicators.is_destructive());
        assert!(!super::ChartContextAction::CopyPrice.is_destructive());
        assert!(!super::ChartContextAction::Reset.is_destructive());
        assert!(!super::ChartContextAction::Close.is_destructive());
        assert!(!super::ChartContextAction::Settings.is_destructive());
        let items = super::chart_context_menu_items(super::ChartContextMenuState {
            pane_count: 1,
            flags: 0,
        });
        assert_eq!(items[0].icon, super::HugeIcon::Refresh01Icon);
        assert_eq!(items[1].icon, super::HugeIcon::Copy01Icon);
        assert_eq!(items[1].label, "Copy price");
        assert!(!items[1].enabled);
        assert_eq!(items[1].action, super::ChartContextAction::CopyPrice);
        let copy_ready = super::chart_context_menu_items(super::ChartContextMenuState {
            pane_count: 1,
            flags: super::ChartContextMenuState::COPY_PRICE,
        });
        assert!(copy_ready[1].enabled);
        let trash = items
            .into_iter()
            .filter(|item| item.action.is_destructive())
            .map(|item| item.icon)
            .collect::<Vec<_>>();
        assert_eq!(
            trash,
            [super::HugeIcon::DeleteIcon02, super::HugeIcon::DeleteIcon02]
        );
    }

    #[test]
    fn chrome_menus_shrink_to_fit_a_small_viewport() {
        let chrome_height = 44.0;
        let roomy = chrome_menu_extent(
            size(px(1920.0), px(1200.0)),
            chrome_height,
            CHROME_MENU_SEARCH_HEIGHT,
        );
        assert!((roomy.width - CHROME_MENU_WIDTH).abs() < f32::EPSILON);
        assert!((roomy.list_height - CHROME_MENU_LIST_HEIGHT).abs() < f32::EPSILON);

        let cramped = chrome_menu_extent(
            size(px(800.0), px(600.0)),
            chrome_height,
            CHROME_MENU_SEARCH_HEIGHT,
        );
        assert!(cramped.width < CHROME_MENU_WIDTH);
        assert!(cramped.width + OVERLAY_EDGE_MARGIN * 2.0 <= 800.0);
        assert!(cramped.list_height < CHROME_MENU_LIST_HEIGHT);
        let drawn = CHROME_MENU_SEARCH_HEIGHT + cramped.list_height + CHROME_MENU_FOOTER_HEIGHT;
        assert!(drawn + chrome_height + OVERLAY_EDGE_MARGIN * 2.0 <= 600.0);
        assert!(drawn <= CHROME_MENU_MAX_HEIGHT);
    }

    #[test]
    fn chrome_menus_never_exceed_a_tiny_viewport() {
        let tiny = chrome_menu_extent(size(px(240.0), px(180.0)), 44.0, CHROME_MENU_SEARCH_HEIGHT);
        assert!(tiny.width <= 240.0);
        assert!(tiny.list_height >= 0.0);
        assert!(
            CHROME_MENU_SEARCH_HEIGHT + tiny.list_height + CHROME_MENU_FOOTER_HEIGHT
                <= 180.0 - 44.0
        );
    }

    #[test]
    fn header_history_controls_gate_on_their_own_half_of_the_stack() {
        use super::{DrawingHistoryControl, DrawingHistoryState};

        let empty = DrawingHistoryState::default();
        assert!(!DrawingHistoryControl::Undo.enabled(empty));
        assert!(!DrawingHistoryControl::Redo.enabled(empty));

        let undo_only = DrawingHistoryState {
            can_undo: true,
            can_redo: false,
        };
        assert!(DrawingHistoryControl::Undo.enabled(undo_only));
        assert!(
            !DrawingHistoryControl::Redo.enabled(undo_only),
            "redo must stay disabled while nothing has been reversed"
        );

        assert_ne!(
            DrawingHistoryControl::Undo.id(),
            DrawingHistoryControl::Redo.id()
        );
        assert_eq!(DrawingHistoryControl::Undo.icon(), super::HugeIcon::Undo03);
        assert_eq!(DrawingHistoryControl::Redo.icon(), super::HugeIcon::Redo01);
    }

    #[test]
    fn anchored_menus_slide_back_inside_the_window() {
        let viewport = size(px(800.0), px(600.0));
        let flush_right = clamp_anchored_menu_left(px(760.0), viewport, TIMEFRAME_MENU_WIDTH);
        assert!(
            flush_right + px(TIMEFRAME_MENU_WIDTH) <= px(800.0) - px(OVERLAY_EDGE_MARGIN),
            "a trigger near the right edge must not push the panel off-screen"
        );
        assert_eq!(
            clamp_anchored_menu_left(px(120.0), viewport, TIMEFRAME_MENU_WIDTH),
            px(120.0),
            "a panel that already fits keeps its anchored position"
        );
        assert_eq!(
            clamp_anchored_menu_left(px(40.0), size(px(100.0), px(600.0)), TIMEFRAME_MENU_WIDTH),
            px(0.0),
            "a panel wider than the window pins to the left edge rather than going negative"
        );
    }

    #[test]
    fn price_axis_menu_stays_inside_the_window() {
        let overflow = clamp_price_axis_menu_origin(
            point(px(2000.0), px(2000.0)),
            size(px(800.0), px(600.0)),
            false,
        );
        assert!(overflow.x >= px(OVERLAY_EDGE_MARGIN));
        assert!(overflow.y >= px(OVERLAY_EDGE_MARGIN));
        assert!(overflow.x + px(CHART_CONTEXT_MENU_WIDTH) <= px(800.0) - px(OVERLAY_EDGE_MARGIN));
        assert!(overflow.y <= px(600.0) - px(OVERLAY_EDGE_MARGIN));
    }

    #[test]
    fn price_axis_menu_opens_into_the_chart() {
        let right_axis = clamp_price_axis_menu_origin(
            point(px(780.0), px(200.0)),
            size(px(800.0), px(600.0)),
            false,
        );
        assert!(right_axis.x + px(CHART_CONTEXT_MENU_WIDTH) + px(PRICE_AXIS_MENU_GAP) <= px(780.0));
        assert!(right_axis.x >= px(OVERLAY_EDGE_MARGIN));

        let left_axis = clamp_price_axis_menu_origin(
            point(px(24.0), px(200.0)),
            size(px(800.0), px(600.0)),
            true,
        );
        assert!(left_axis.x >= px(24.0) + px(PRICE_AXIS_MENU_GAP));
        assert!(left_axis.x + px(CHART_CONTEXT_MENU_WIDTH) <= px(800.0) - px(OVERLAY_EDGE_MARGIN));
    }

    #[test]
    fn price_axis_menu_compacts_labels_and_lines_into_flyouts() {
        let state = PriceAxisMenuState {
            flags: PriceAxisMenuState::PRICE_LINE
                | PriceAxisMenuState::LAST_VALUE
                | PriceAxisMenuState::TITLE
                | PriceAxisMenuState::COUNTDOWN
                | PriceAxisMenuState::INDICATOR_NAMES
                | PriceAxisMenuState::INDICATOR_VALUES
                | PriceAxisMenuState::INDICATOR_PRICE_LINES
                | PriceAxisMenuState::AUTO_SCALE
                | PriceAxisMenuState::ALIGN_LABELS,
            mode: 0,
            left: false,
            precision: None,
        };
        assert_eq!(
            price_axis_root_rows(PriceAxisMenuFlyout::None, state).map(PriceAxisMenuRow::label),
            [
                "Labels",
                "Lines",
                "Auto scale",
                "Invert scale",
                "Scale mode",
                "Y-axis",
                "Precision",
            ]
        );
        let labels = price_axis_flyout_rows(PriceAxisMenuFlyout::Labels, state);
        assert_eq!(
            labels
                .iter()
                .copied()
                .map(PriceAxisMenuRow::label)
                .collect::<Vec<_>>(),
            [
                "Symbol name label",
                "Symbol last price label",
                "Symbol previous day close price label",
                "Pre/post/night market price label",
                "High and low price labels",
                "Bid and ask labels",
                "Indicators and financials name labels",
                "Indicators and financials value labels",
                "Countdown to bar close",
                "No overlapping labels",
            ]
        );
        assert!(labels.iter().all(|row| {
            ![
                "Symbol previous day close price label",
                "Pre/post/night market price label",
                "High and low price labels",
            ]
            .contains(&row.label())
                || !row.enabled()
        }));
        let lines = price_axis_flyout_rows(PriceAxisMenuFlyout::Lines, state);
        assert_eq!(
            lines
                .iter()
                .copied()
                .map(PriceAxisMenuRow::label)
                .collect::<Vec<_>>(),
            [
                "Price line",
                "Previous day close price line",
                "Pre/post/night market price line",
                "High and low price lines",
                "Bid and ask lines",
                "Indicators and financials price lines",
            ]
        );
        assert!(lines.iter().all(|row| {
            ![
                "Previous day close price line",
                "Pre/post/night market price line",
                "High and low price lines",
            ]
            .contains(&row.label())
                || !row.enabled()
        }));
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
            false,
            "Rithmic Test session will retry",
        )
        .expect("recovery notice");
        assert_eq!(recovery.label, "Reconnecting chart");
        assert_eq!(recovery.placement, ChartNoticePlacement::BottomRight);
        assert_eq!(recovery.tone, ChartNoticeTone::Warning);
        assert!(chart_surface_notice(ChartState::Ready, true, false, "current").is_none());
    }

    #[test]
    fn chart_error_and_stale_notices_use_truthful_severity() {
        let stale = chart_surface_notice(ChartState::Stale, true, false, "trade stream is silent")
            .expect("stale notice");
        assert_eq!(stale.label, "Chart stale");
        assert_eq!(stale.tone, ChartNoticeTone::Warning);

        let error = chart_surface_notice(
            ChartState::Error,
            false,
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
