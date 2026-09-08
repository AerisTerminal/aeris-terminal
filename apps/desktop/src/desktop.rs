//! Desktop composition and shared presentation types. Behavior lives in owned child modules.

#[path = "components/about_dialog.rs"]
mod about_dialog;
#[path = "assets.rs"]
mod assets;
#[path = "chart_chrome.rs"]
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
#[path = "engine_market_worker.rs"]
mod engine_market_worker;
#[path = "frame_poll_gate.rs"]
mod frame_poll_gate;
#[path = "components/indicator_menu.rs"]
mod indicator_menu;
#[path = "desktop/local_state.rs"]
mod local_state;
#[path = "native_ui/mod.rs"]
mod native_ui;
#[path = "onboarding.rs"]
mod onboarding;
#[cfg(any(test, feature = "diagnostics"))]
#[path = "readiness_conformance.rs"]
mod readiness_conformance;
#[path = "rithmic_shell.rs"]
mod rithmic_shell;
#[path = "components/symbol_menu.rs"]
mod symbol_menu;
#[path = "components/terminal_chrome.rs"]
mod terminal_chrome;
#[path = "components/terminal_view.rs"]
mod terminal_view;
#[path = "update.rs"]
mod update;
#[path = "components/workspace_layout.rs"]
mod workspace_layout;

use about_dialog::about_dialog_layer;
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
    ChartState, MarketDataWorker, MarketPublicationGeneration, MarketWorkerBootstrap,
    MarketWorkerMessage, MarketWorkerPublication, MarketWorkerRetirement, MarketWorkerStartup,
    PendingUiDiagnostics, ProviderCatalogCommand, ProviderCatalogEvent, UiDiagnosticsFeedback,
};
use axiusflow_engine_protocol::{
    InstallProviderInstrument, ProviderCatalogRejected, ProviderCatalogRejectionReason,
    ProviderInstrumentSearchResult, ProviderInstrumentSummary, SearchProviderInstruments,
    SelectProviderInstrument, SeriesCadence, SeriesKey, WorkspaceLayoutState, WorkspacePaneKind,
    WorkspacePaneState, WorkspaceSplitAxis, WorkspaceState, WorkspaceTabState,
};
use axiusflow_market_data::{ChartAggregation, ChartInterval};
use axiusflow_market_runtime::MarketConsumerResourceClass as ConsumerResourceClass;
use axiusflow_observability::FeedConnectionState;
use axiusflow_terminal_ui::{OrderBookColumn, OrderBookColumnVisibility, ReadOnlyOrderBookView};
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
    tooltip::{TooltipSpec, with_tooltip},
};
use num_traits::ToPrimitive;
use reqwest_client::ReqwestClient;
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
use update::{DesktopUpdater, UpdatePresentation, UpdateState};
use workspace_layout::workspace_market_area;
#[cfg(test)]
use workspace_layout::workspace_split_ratio;

#[cfg(feature = "diagnostics")]
use std::time::Instant;

fn install_platform_http_client(cx: &mut App) {
    match ReqwestClient::user_agent(concat!("Axiusflow/", env!("CARGO_PKG_VERSION"))) {
        Ok(client) => cx.set_http_client(Arc::new(client)),
        Err(error) => eprintln!("Axiusflow image networking degraded: {error}"),
    }
}

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

mod lifecycle;
use lifecycle::DesktopLifecycle;

mod workspace_persistence;
use workspace_persistence::WorkspaceLayoutPersistence;

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

static RITHMIC_INTERVALS: &[ChartInterval] = &[
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
/// Hyperliquid serves every Rithmic interval plus a native 3-day candle.
/// Tick candles exist on neither public path, so 100t stays unoffered.
static HYPERLIQUID_INTERVALS: &[ChartInterval] = &[
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
    ChartInterval::Day3,
    ChartInterval::Week1,
    ChartInterval::Month1,
];
const RITHMIC_ENTITLEMENT_ID: &str = "crypto_public_realtime";
const HYPERLIQUID_ENTITLEMENT_ID: &str = "hyperliquid-public";

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

fn stopped_worker_chart_detail(chart_state: ChartState, existing: &str, fallback: &str) -> String {
    if chart_state == ChartState::Error && !existing.trim().is_empty() {
        existing.to_string()
    } else {
        fallback.to_string()
    }
}

const DEFAULT_RITHMIC_LISTING_QUERY: &str = "MNQ";

/// The instrument menu should open with a default provider listing instead of
/// a blank list. A completed Rithmic selection consumes the previous search
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
    order_book: Entity<ReadOnlyOrderBookView>,
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
    provider_transport_rtt_nanos: Option<u64>,
    symbol_browser: rithmic_shell::RithmicSymbolBrowser,
    symbol_message: String,
    market_state: WorkspaceMarketState,
    series_message: String,
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
    product: Option<InstallProviderInstrument>,
    rithmic_switch: RithmicSwitchState,
    interval: ChartInterval,
    rithmic_pending_interval: Option<ChartInterval>,
    rithmic_pending_product: Option<InstallProviderInstrument>,
    rithmic_pending_sequence: Option<u64>,
    /// The selection to fall back to if the switch in flight never loads.
    ///
    /// A failed switch must leave the trader on the chart they had, not on an
    /// empty surface, so the previous demand is restored rather than abandoned.
    rithmic_previous_selection: Option<(Option<InstallProviderInstrument>, ChartInterval)>,
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
    order_book_column_open: bool,
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
    Rithmic,
    Hyperliquid,
}

const fn terminal_provider_id(provider: TerminalProvider) -> &'static str {
    match provider {
        TerminalProvider::Rithmic => "rithmic",
        TerminalProvider::Hyperliquid => "hyperliquid",
    }
}

const fn terminal_provider_display(provider: TerminalProvider) -> &'static str {
    match provider {
        TerminalProvider::Rithmic => "Rithmic",
        TerminalProvider::Hyperliquid => "Hyperliquid",
    }
}

fn terminal_provider_from_id(provider: &str) -> TerminalProvider {
    if provider == terminal_provider_id(TerminalProvider::Hyperliquid) {
        TerminalProvider::Hyperliquid
    } else {
        TerminalProvider::Rithmic
    }
}

/// Where a Rithmic symbol or timeframe change is in its handover.
///
/// A switch is a presentation change, so it never blanks the chart. The chart on
/// screen keeps streaming its own series until the replacement's covering
/// snapshot arrives, and only then is it swapped. `Pending` is the window
/// between the request and the mailbox marker that orders it; `Swapping` is the
/// window between that marker and the snapshot that replaces the chart;
/// `Initializing` keeps that replacement covered until its live handoff lands.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum RithmicSwitchState {
    #[default]
    Idle,
    Pending,
    Swapping,
    Initializing,
}

impl RithmicSwitchState {
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

const fn switch_requires_chart_cover(has_chart: bool, state: RithmicSwitchState) -> bool {
    has_chart && state.in_progress()
}

/// A `Ready` control message can still be queued for the previously selected
/// series while a catalog selection is making its UI -> worker round trip.
/// Letting that old readiness retire `Pending` (or the pre-snapshot `Swapping`
/// phase) drops the switch marker that follows and leaves the chart on the old
/// symbol. Only an idle series or a replacement that has already installed its
/// covering snapshot (`Initializing`) may complete on `Ready`.
const fn ready_state_can_complete_switch(state: RithmicSwitchState) -> bool {
    matches!(
        state,
        RithmicSwitchState::Idle | RithmicSwitchState::Initializing
    )
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderConnectionPresentation {
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

impl ProviderConnectionPresentation {
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
            if matches!(
                app.provider,
                TerminalProvider::Rithmic | TerminalProvider::Hyperliquid
            ) && let Some(viewport) = chart.read(cx).visible_time_range_unix_nanos()
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
    Hyperliquid(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SymbolSubmitDecision {
    Select(usize),
    Search,
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
        TerminalProvider::Rithmic | TerminalProvider::Hyperliquid => SymbolSubmitDecision::Search,
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

fn should_autoload_rithmic_catalog(
    state: FeedConnectionState,
    message: &str,
    autoload_started: bool,
) -> bool {
    if state != FeedConnectionState::Authenticating
        || !message.contains(crate::desktop::engine_market_worker::RITHMIC_CATALOG_READY_MESSAGE)
    {
        return false;
    }
    !autoload_started
}

struct HeaderState {
    theme: AxiusflowTheme,
    provider: TerminalProvider,
    instrument_label: String,
    series_label: String,
    chart_type: ChartType,
    chart_type_label: String,
    instruments: Vec<InstrumentMenuEntry>,
    symbol_input: Option<Entity<InputState>>,
    indicator_input: Entity<InputState>,
    indicator_message: Option<String>,
    series_message: String,
    pending: HeaderPendingState,
    drawing_history: DrawingHistoryState,
    controls: HeaderControls,
    order_book_visible: bool,
    connection_state: FeedConnectionState,
    transport_rtt_nanos: Option<u64>,
    instrument_scroll: ScrollHandle,
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
    OrderBook,
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
            Self::OrderBook => "Order Book",
        }
    }

    const fn toggle_label(self) -> &'static str {
        match self {
            Self::OrderBook => "Order Book",
        }
    }

    const fn toggle_tooltip(self) -> &'static str {
        match self {
            Self::OrderBook => "Toggle read-only order book",
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
    if matches!(
        app.provider,
        TerminalProvider::Rithmic | TerminalProvider::Hyperliquid
    ) {
        return app.product.as_ref().map_or_else(
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

fn series_selector_label(selected: ChartInterval) -> String {
    selected.label().to_string()
}

#[derive(Clone, Copy)]
struct HeaderControls(u8);

impl HeaderControls {
    const INSTRUMENT: u8 = 1;
    const SERIES: u8 = 2;
    const ORDER_BOOK: u8 = 4;
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
            controls |= Self::SERIES | Self::ORDER_BOOK;
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
    product: Option<InstallProviderInstrument>,
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
                product: None,
            }
        }
        MarketWorkerStartup::Loading(startup) => {
            let provider = terminal_provider_from_id(startup.product.provider.as_str());
            TerminalStartupState {
                chart: None,
                chart_state: ChartState::Loading,
                chart_state_message: "waiting for a covering market snapshot".to_string(),
                replay_label: "waiting for a covering market snapshot".to_string(),
                worker_label: startup.worker_label,
                subscription_id: startup.subscription_id,
                connection_state: Some(FeedConnectionState::Discovering),
                connection_message: Some(match provider {
                    TerminalProvider::Rithmic => "Connecting to Rithmic public markets".to_string(),
                    TerminalProvider::Hyperliquid => {
                        "Connecting to Hyperliquid public markets".to_string()
                    }
                }),
                provider,
                product: Some(startup.product),
            }
        }
    }
}

fn initial_symbol_message(provider: TerminalProvider) -> String {
    match provider {
        TerminalProvider::Rithmic => "Search for an entitled Rithmic Test symbol",
        TerminalProvider::Hyperliquid => "Search Hyperliquid perps and spot markets",
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

mod workspace_surface;

fn catalog_rejection_message(
    reason: ProviderCatalogRejectionReason,
    command: ProviderCatalogCommand,
    provider: TerminalProvider,
) -> &'static str {
    let rithmic = provider == TerminalProvider::Rithmic;
    let hyperliquid = provider == TerminalProvider::Hyperliquid;
    match reason {
        ProviderCatalogRejectionReason::SearchRejected if rithmic => {
            "Rithmic rejected the market search"
        }
        ProviderCatalogRejectionReason::SearchRejected if hyperliquid => {
            "Hyperliquid rejected the market search"
        }
        ProviderCatalogRejectionReason::SearchRejected => "Rithmic Test rejected the symbol search",
        ProviderCatalogRejectionReason::SupersededSearch => {
            "A newer symbol search replaced this one"
        }
        ProviderCatalogRejectionReason::InstrumentUnavailable => {
            "The selected symbol is no longer available"
        }
        ProviderCatalogRejectionReason::SubscriptionRejected if rithmic => {
            "Rithmic rejected the market subscription"
        }
        ProviderCatalogRejectionReason::SubscriptionRejected if hyperliquid => {
            "Hyperliquid rejected the market subscription"
        }
        ProviderCatalogRejectionReason::SubscriptionRejected => {
            "Rithmic Test rejected the market subscription"
        }
        ProviderCatalogRejectionReason::DispatchUnavailable
            if rithmic && command == ProviderCatalogCommand::Search =>
        {
            "The Rithmic search could not be scheduled"
        }
        ProviderCatalogRejectionReason::DispatchUnavailable if rithmic => {
            "The Rithmic selection could not be scheduled"
        }
        ProviderCatalogRejectionReason::DispatchUnavailable
            if hyperliquid && command == ProviderCatalogCommand::Search =>
        {
            "The Hyperliquid search could not be scheduled"
        }
        ProviderCatalogRejectionReason::DispatchUnavailable if hyperliquid => {
            "The Hyperliquid selection could not be scheduled"
        }
        ProviderCatalogRejectionReason::DispatchUnavailable
            if command == ProviderCatalogCommand::Search =>
        {
            "The Rithmic search could not be scheduled"
        }
        ProviderCatalogRejectionReason::DispatchUnavailable => {
            "The Rithmic selection could not be scheduled"
        }
        ProviderCatalogRejectionReason::SearchTimedOut if rithmic => {
            "The Rithmic market search timed out; try again"
        }
        ProviderCatalogRejectionReason::SearchTimedOut if hyperliquid => {
            "The Hyperliquid market search timed out; try again"
        }
        ProviderCatalogRejectionReason::SearchTimedOut => {
            "The Rithmic symbol search timed out; try again"
        }
        ProviderCatalogRejectionReason::SelectionTimedOut if rithmic => {
            "The Rithmic market selection timed out; try again"
        }
        ProviderCatalogRejectionReason::SelectionTimedOut if hyperliquid => {
            "The Hyperliquid market selection timed out; try again"
        }
        ProviderCatalogRejectionReason::SelectionTimedOut => {
            "The Rithmic symbol selection timed out; try again"
        }
        ProviderCatalogRejectionReason::Unspecified if hyperliquid => {
            "The Hyperliquid catalog request failed"
        }
        ProviderCatalogRejectionReason::Unspecified if rithmic => {
            "The Rithmic catalog request failed"
        }
        ProviderCatalogRejectionReason::Unspecified => "The Rithmic catalog request failed",
    }
}

fn provider_catalog_event_provider(event: &ProviderCatalogEvent) -> &str {
    match event {
        ProviderCatalogEvent::SearchCompleted(result) => &result.provider,
        ProviderCatalogEvent::SelectionInstalled { instrument, .. } => &instrument.provider,
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
    let workspace = local_state::load_workspace();
    let market = axiusflow_market_runtime::MarketService::start()?;
    let status = market.status()?;
    if status.providers.is_empty() {
        return Err("candidate market service did not reach readiness".to_string());
    }
    let _account_service = axiusflow_account_runtime::AccountService::new(
        axiusflow_account_runtime::AccountServiceConfig::from_environment(),
    );
    let release = axiusflow_platform_runtime::current_release_identity();
    let report = LifecycleReadinessReport {
        schema_version: 2,
        release_identity: release.release_identity,
        install_generation: release.install_generation,
        desktop_process_id: std::process::id(),
        workspace_revision: workspace.workspace_revision,
        provider_count: status.providers.len(),
        workspace_restored: true,
        market_service_ready: true,
        account_runtime_ready: true,
    };
    market.shutdown(std::time::Duration::from_secs(2))?;
    let mut encoded = serde_json::to_vec(&report)
        .map_err(|_| "candidate readiness report could not be encoded".to_string())?;
    encoded.push(b'\n');
    std::fs::write(std::path::Path::new(&report_path), encoded)
        .map_err(|_| "candidate readiness report could not be written".to_string())
}

#[derive(serde::Serialize)]
// Wire mirror of the launcher readiness JSON: this validates the direct
// in-process desktop runtime before a release is activated.
struct LifecycleReadinessReport {
    schema_version: u32,
    release_identity: String,
    install_generation: u64,
    desktop_process_id: u32,
    workspace_revision: u64,
    provider_count: usize,
    workspace_restored: bool,
    market_service_ready: bool,
    account_runtime_ready: bool,
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
            cx.new(|cx| InputState::new(window, cx).placeholder("Search Rithmic spot markets"))
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
            title: Some("Axiusflow".into()),
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
    profile_refresh_on_activation: bool,
    about_dialog_open: bool,
    updater: Option<DesktopUpdater>,
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
                        let instrument = surface.product.clone()?;
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
                            series: Some(workspace_series(surface.interval, &instrument)),
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

mod workspace_tabs;

struct ConfiguredDesktop {
    market_workers: Vec<(MarketWorkerStartup, MarketDataWorker)>,
    workspace_panes: Vec<engine_market_worker::WorkspaceMarketPane>,
    restored_workspace: WorkspaceState,
    workspace_factory: Option<engine_market_worker::WorkspaceMarketFactory>,
    layout: DesktopLayout,
    chart_chrome: chart_chrome::ChartChromePreferences,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum DesktopLayout {
    #[default]
    Windows,
    WorkspaceTabs,
}

struct ConfiguredLifecycle {
    workspace: WorkspaceState,
}

fn configure_desktop_state() -> ConfiguredLifecycle {
    let workspace = local_state::load_workspace();
    ConfiguredLifecycle { workspace }
}

fn configured_market_workers() -> Result<Option<ConfiguredDesktop>, String> {
    let mut arguments = std::env::args_os().skip(1);
    let command = arguments.next();
    let mut layout = DesktopLayout::Windows;
    let mut workspace_factory = None;
    let (market_workers, workspace_panes, lifecycle) = if let Some(argument) = command {
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
        if argument == "--rithmic-test" {
            if arguments.next().is_some() {
                eprintln!("usage: axiusflow_desktop --rithmic-test");
                std::process::exit(2);
            }
            let lifecycle = configure_desktop_state();
            (
                vec![engine_market_worker::start_rithmic_catalog()?],
                Vec::new(),
                lifecycle,
            )
        } else if argument == "--multi-chart" {
            if arguments.next().is_some() {
                eprintln!("usage: axiusflow_desktop --multi-chart");
                std::process::exit(2);
            }
            let lifecycle = configure_desktop_state();
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
            let lifecycle = configure_desktop_state();
            layout = DesktopLayout::WorkspaceTabs;
            let group = engine_market_worker::start_workspace_tabs(&lifecycle.workspace)?;
            workspace_factory = Some(group.factory);
            (Vec::new(), group.initial, lifecycle)
        } else {
            eprintln!("unsupported argument: {}", argument.to_string_lossy());
            std::process::exit(2);
        }
    } else {
        // The installed desktop is a persisted workspace, not the legacy
        // single-chart/window bootstrap. Starting through the workspace group
        // restores pane identity, symbol, interval, viewport and layout and
        // mounts the persistence owner used for subsequent durable changes.
        let lifecycle = configure_desktop_state();
        layout = DesktopLayout::WorkspaceTabs;
        let group = engine_market_worker::start_workspace_tabs(&lifecycle.workspace)?;
        workspace_factory = Some(group.factory);
        (Vec::new(), group.initial, lifecycle)
    };
    Ok(Some(ConfiguredDesktop {
        market_workers,
        workspace_panes,
        restored_workspace: lifecycle.workspace.clone(),
        workspace_factory,
        layout,
        chart_chrome: chart_chrome::load_chart_chrome_preferences(),
    }))
}

pub(super) fn run() {
    let mut lifecycle_arguments = std::env::args_os().skip(1);
    if lifecycle_arguments.next().as_deref() == Some(std::ffi::OsStr::new("--desktop-readiness")) {
        if let Err(error) = run_desktop_readiness_command(lifecycle_arguments) {
            eprintln!("Axiusflow desktop readiness failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    if std::env::args_os().len() == 1
        && let Err(error) = schedule_versioned_launcher_promotion()
    {
        eprintln!("Axiusflow launcher promotion deferred: {error}");
    }
    let account = match axiusflow_desktop::account::DesktopAccount::install() {
        Ok(account) => account,
        Err(error) => {
            eprintln!("Axiusflow account client could not start: {error}");
            run_onboarding();
            return;
        }
    };
    if !account.authenticated() {
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
    let lifecycle = DesktopLifecycle::new();
    run_desktop(configured, lifecycle);
}

fn schedule_versioned_launcher_promotion() -> Result<(), String> {
    let executable = std::env::current_exe()
        .map_err(|_| "desktop executable path is unavailable".to_string())?;
    let release_root = executable
        .parent()
        .ok_or_else(|| "desktop release directory is unavailable".to_string())?;
    let launcher = release_root.join(format!(
        "axiusflow_launcher{}",
        std::env::consts::EXE_SUFFIX
    ));
    let metadata = match std::fs::symlink_metadata(&launcher) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("versioned launcher metadata is unavailable".to_string()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("versioned launcher is invalid".to_string());
    }
    std::process::Command::new(launcher)
        .arg("--promote-stable-launcher")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| "versioned launcher promotion could not be scheduled".to_string())
}

fn run_onboarding() {
    application()
        .with_assets(assets::AxiusflowAssets)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            install_platform_http_client(cx);
            cx.set_app_identity("com.axiusflow.desktop", "Axiusflow");
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
            install_platform_http_client(cx);
            cx.set_app_identity("com.axiusflow.desktop", "Axiusflow");
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
mod tests;

#[cfg(test)]
mod http_wiring_tests {
    use super::install_platform_http_client;
    use gpui::{Asset, ImageAssetLoader, Resource};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
        time::Duration,
    };

    #[test]
    fn installed_http_client_loads_remote_avatar_resource() {
        const GIF_1X1: &[u8] = &[
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00,
            0x00, 0x00, 0xff, 0xff, 0xff, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00,
            0x3b,
        ];

        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback avatar server binds");
        let address = listener.local_addr().expect("loopback address resolves");
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("avatar request connects");
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("loopback read timeout configures");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = socket.read(&mut chunk).expect("avatar request reads");
                assert!(count > 0, "avatar request closed before headers completed");
                request.extend_from_slice(&chunk[..count]);
                assert!(
                    request.len() < 16 * 1024,
                    "avatar request headers stay bounded"
                );
            }
            let request = String::from_utf8_lossy(&request);
            assert!(
                request.starts_with("GET /avatar.gif "),
                "GPUI image loader must issue a GET for the avatar resource: {request}"
            );
            let expected_user_agent =
                format!("user-agent: Axiusflow/{}", env!("CARGO_PKG_VERSION"));
            assert!(
                request
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case(&expected_user_agent)),
                "installed ReqwestClient must carry the Axiusflow user agent: {request}"
            );

            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: image/gif\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                GIF_1X1.len()
            )
            .expect("avatar response headers write");
            socket
                .write_all(GIF_1X1)
                .expect("avatar response body writes");
        });

        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let url = format!("http://{address}/avatar.gif");
        gpui_platform::headless().run(move |cx| {
            install_platform_http_client(cx);
            let load = <ImageAssetLoader as Asset>::load(Resource::Uri(url.into()), cx);
            let loaded = reqwest_client::runtime().block_on(load).is_ok();
            let _ = result_tx.send(loaded);
            cx.quit();
        });

        assert!(
            result_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("avatar loader reports its result"),
            "the GPUI avatar loader must decode a remote image through the installed ReqwestClient"
        );
        server.join().expect("loopback avatar server completes");
    }
}
