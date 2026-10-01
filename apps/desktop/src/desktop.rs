//! Desktop composition and shared presentation types. Behavior lives in owned child modules.

#[path = "components/about_dialog.rs"]
mod about_dialog;
#[path = "components/accounts_panel.rs"]
mod accounts_panel;
#[path = "assets.rs"]
mod assets;
#[path = "components/bottom_panel.rs"]
mod bottom_panel;
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
#[path = "components/command_palette.rs"]
mod command_palette;
#[path = "components/context_panel.rs"]
mod context_panel;
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
#[path = "components/order_book_panel.rs"]
mod order_book_panel;
#[path = "components/order_ticket.rs"]
mod order_ticket;
#[path = "components/price_alert_dialog.rs"]
mod price_alert_dialog;
#[cfg(any(test, feature = "diagnostics"))]
#[path = "readiness_conformance.rs"]
mod readiness_conformance;
#[path = "rithmic_shell.rs"]
mod rithmic_shell;
#[path = "components/side_panel_dock.rs"]
mod side_panel_dock;
#[path = "study_packages.rs"]
mod study_packages;
#[path = "components/study_settings_dialog.rs"]
mod study_settings_dialog;
#[path = "components/symbol_menu.rs"]
mod symbol_menu;
#[path = "components/terminal_chrome.rs"]
mod terminal_chrome;
#[path = "components/terminal_view.rs"]
mod terminal_view;
#[path = "components/time_sales_panel.rs"]
mod time_sales_panel;
#[path = "update.rs"]
mod update;
#[path = "components/watchlist_panel.rs"]
mod watchlist_panel;
#[path = "components/workspace_layout.rs"]
mod workspace_layout;

use about_dialog::about_dialog_layer;
use aeris_application::ReplayStreamUpdate;
use aeris_chart_integration::{
    AerisChartTheme, AerisChartView, AerisChartWorkspace, ChartAggressorSide, ChartAlertCondition,
    ChartAlertCreateRequest, ChartAlertFrequency, ChartAlertId, ChartAlertLine,
    ChartAlertLineStatus, ChartAlertPriceScale, ChartAlertSnapshot, ChartAppearanceColor,
    ChartAppearanceSettings, ChartBridgeMetrics, ChartContextKind, ChartContextRequest,
    ChartDrawingKind, ChartExecutionId, ChartExecutionKind, ChartExecutionMarkerShape,
    ChartHostEventMarker, ChartHostOverlaySnapshot, ChartHostTimeWindow, ChartIndicator,
    ChartIndicatorState, ChartInstrumentMetadata, ChartOrderId, ChartOrderKind, ChartOrderRole,
    ChartOrderSide, ChartOrderStatus, ChartPositionId, ChartPositionSide, ChartSplitDirection,
    ChartStudyInputRequirements, ChartStudyInputStream, ChartStudyOutputDescriptor,
    ChartStudyPaneTarget, ChartStudyPlotKind, ChartStudyPointStyle, ChartStudyScaleTarget,
    ChartStudyThresholdRegion, ChartThemeColors, ChartTradingAnnotation,
    ChartTradingAnnotationPlacement, ChartTradingAnnotationTone, ChartTradingExecution,
    ChartTradingGroupId, ChartTradingIntent, ChartTradingIntentAction, ChartTradingPosition,
    ChartTradingPriceScale, ChartTradingSnapshot, ChartType, ChartWorkingOrder,
    ChartWorkspaceLayout, DEFAULT_STUDY_LINE_WIDTH, FootprintDisplayMode, MAXIMUM_STUDY_LINE_WIDTH,
    OrderFlowAggregation, OrderFlowSettings, OrderFlowSweep, OrderFlowTrade, PriceAxisMenuAction,
    PriceAxisMenuState, classify_order_flow_sweeps,
};
use aeris_context_runtime::{ContextSnapshot, ContextSource, ContextView};
use aeris_contracts::{
    InstallProviderInstrument, PriceAlertCondition, PriceAlertFrequency, PriceAlertStatus,
    ProviderCatalogRejected, ProviderCatalogRejectionReason, ProviderConnectionKind,
    ProviderInstrumentSearchResult, ProviderInstrumentSummary, SearchProviderInstruments,
    SelectProviderInstrument, SeriesCadence, SeriesKey, WorkspaceChartAppearanceState,
    WorkspaceChartIndicatorState, WorkspaceChartSettingsTemplateState, WorkspaceChartState,
    WorkspaceChartStudyState, WorkspaceLayoutState, WorkspaceOrderFlowSettingsState,
    WorkspacePaneKind, WorkspacePaneState, WorkspacePriceAlertState, WorkspacePriceAxisState,
    WorkspaceSplitAxis, WorkspaceState, WorkspaceStudyDecimalState, WorkspaceStudyDependencyKind,
    WorkspaceStudyDependencyState, WorkspaceStudyMarketStream, WorkspaceStudySettingState,
    WorkspaceTabState, WorkspaceWatchlistEntryState, workspace_study_setting_state,
};
use aeris_design_system::{
    AerisTheme, BRAND_FONT_BYTES, PLATFORM_FONT_BYTES, RadiusToken, ThemeColor, ThemeMode,
    TypographyRole,
};
use aeris_desktop::market_worker::{
    ChartState, MarketDataWorker, MarketPublicationGeneration, MarketWorkerBootstrap,
    MarketWorkerMessage, MarketWorkerPublication, MarketWorkerRetirement, MarketWorkerStartup,
    PendingUiDiagnostics, ProviderCatalogCommand, ProviderCatalogEvent, UiDiagnosticsFeedback,
};
use aeris_market_data::{BarSeriesKey, ChartAggregation, ChartInterval, MarketBar};
use aeris_market_runtime::MarketConsumerResourceClass as ConsumerResourceClass;
use aeris_market_runtime::study::{
    NativeStudyRegistration, StudyDecimal, StudyDependency, StudyInstanceId, StudyMarketInput,
    StudyPaneTarget, StudyPlotKind, StudyPointStyle, StudyScaleTarget, StudySettingCondition,
    StudySettingControl, StudySettingSpec, StudySettingValue, StudyThresholdRegion,
};
use aeris_market_runtime::{
    MAXIMUM_PRICE_ALERTS_PER_CONSUMER, MarketPriceAlert, MarketPriceAlertTrigger, MarketStream,
    StreamRequirements,
};
use aeris_observability::FeedConnectionState;
use aeris_terminal_ui::{
    OrderBookColumn, OrderBookColumnVisibility, OrderBookLevelClick, OrderBookLevelDrop,
    OrderBookLevelSide, ReadOnlyOrderBookView,
};
use assets::UiIcon as HugeIcon;
#[cfg(test)]
use chart_context_menus::chart_context_menu_scale;
use chart_context_menus::{
    ChartSettingsTemplateView, ChartSettingsView, account_menu_layer, chart_context_menu_layer,
    chart_settings_menu_layer, overlay_height, price_axis_menu_layer,
};
#[cfg(test)]
use chart_context_menus::{
    PriceAxisMenuRow, chart_context_menu_items, clamp_chart_context_menu_origin,
    clamp_price_axis_menu_origin, price_axis_flyout_rows, price_axis_root_rows,
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
    CHROME_MENU_LIST_HEIGHT, CHROME_MENU_MAX_HEIGHT, CHROME_MENU_SEARCH_HEIGHT, CHROME_MENU_WIDTH,
};
use chrome_menu::{
    ChromeIconButtonTone, chrome_close_button, chrome_icon_button, chrome_menu_extent,
};
use command_palette::command_palette_layer;
use context_panel::{ContextPanelHeightDrag, ContextPanelState, context_panel};
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
    color_picker::{ColorPicker, normalize_hex_color},
    control::Button,
    icon::Icon,
    input::{Input, InputEvent, InputState},
    loader::Loader,
    menu::{
        MenuRow, MenuScale, PopupAnimationOrigin, animate_popup_from_origin,
        flat_compact_menu_panel, menu_separator,
    },
    platform_font_weight, platform_tabular_numerals,
    rem_scale::{design_rems, rem_scaled},
    scroll::{ThinScrollbar, tracked_overflow_y_scrollbar},
    tab::Tab,
    theme::{ButtonVariant, base_theme, gpui_color},
    tooltip::{TooltipSpec, with_tooltip},
};
use num_traits::ToPrimitive;
use order_book_panel::OrderBookPanelState;
use order_ticket::TradingOrderControlsState;
use price_alert_dialog::{
    price_alert_dialog_layer, replace_chart_price_alert_lines, runtime_price_alerts,
};
use reqwest_client::ReqwestClient;
use side_panel_dock::{WorkspaceSidePanelState, workspace_side_panel};
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    pin::Pin,
    rc::Rc,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    task::{Context as TaskContext, Poll, Waker},
    time::{Duration, Instant},
};
use study_settings_dialog::study_settings_dialog_layer;
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
    WindowCommand, WindowMoveGestureEvent, WorkspaceTabBarState, aeris_chart_theme,
    button_activation, button_activation_at, chrome_button_style, chrome_tooltip, exchange_mark,
    fullscreen_escape_command, header_icon, round_icon_button, series_glyph, terminal_header,
    window_move_gesture_transition, workspace_title_bar, workspace_title_bar_visible,
};
use terminal_view::{
    TerminalShellInit, WorkspaceSplitDrag, terminal_root, workspace_tab_strip, workspace_tabs_root,
};
use time_sales_panel::TimeSalesPanelState;
use update::{DesktopUpdater, UpdatePresentation, UpdateState};
use watchlist_panel::{WATCHLIST_ROW_HEIGHT, WatchlistPanelState};
#[cfg(test)]
use workspace_layout::workspace_split_ratio;
use workspace_layout::{
    WorkspaceMaximizeTransition, workspace_market_area, workspace_maximize_transition,
};

fn install_platform_http_client(cx: &mut App) {
    match ReqwestClient::user_agent(concat!("Aeris/", env!("CARGO_PKG_VERSION"))) {
        Ok(client) => cx.set_http_client(Arc::new(client)),
        Err(error) => eprintln!("Aeris image networking degraded: {error}"),
    }
}

fn retain_account_refresh_quiesce_for_exit(
    account_refresh: Result<aeris_account_runtime::AccountRefreshQuiesce, String>,
    context: &str,
) {
    let quiesce = match account_refresh {
        Ok(quiesce) => quiesce,
        Err(error) => {
            eprintln!(
                "Aeris refused {context} because account refresh could not be quiesced: {error}"
            );
            loop {
                std::thread::park();
            }
        }
    };

    let first_error = match quiesce.wait() {
        Ok(()) => {
            quiesce.retain_until_process_exit();
            return;
        }
        Err(error) => error,
    };
    eprintln!("Aeris account shutdown retrying after: {first_error}");
    let second_error = match quiesce.wait() {
        Ok(()) => {
            quiesce.retain_until_process_exit();
            return;
        }
        Err(error) => error,
    };
    eprintln!(
        "Aeris refused {context} because durable account refresh did not settle: {second_error}"
    );
    // This startup/background path has no GPUI lifecycle to return to. Keep the
    // existing quiesce claim alive after the bounded retries so a forced exit
    // cannot be followed by another rotating grant in this process.
    loop {
        std::thread::park();
    }
}

fn exit_after_account_refresh_quiesce(exit_code: i32) -> ! {
    retain_account_refresh_quiesce_for_exit(
        aeris_desktop::account::begin_refresh_quiesce(),
        "process exit",
    );
    std::process::exit(exit_code);
}

#[cfg(target_os = "windows")]
fn native_account_session_shutdown_guard(
    begin_quiesce: impl Fn() -> Result<aeris_account_runtime::AccountRefreshQuiesce, String>
    + Send
    + Sync
    + 'static,
) -> Result<aeris_platform_runtime::NativeSessionShutdownGuard, String> {
    aeris_platform_runtime::NativeSessionShutdownGuard::connect(move || {
        let quiesce = match begin_quiesce() {
            Ok(quiesce) => quiesce,
            Err(error) => {
                eprintln!("Aeris session shutdown was blocked: {error}");
                return None;
            }
        };
        match quiesce.wait() {
            Ok(()) => Some(aeris_platform_runtime::NativeSessionShutdownPermit::new(
                quiesce,
            )),
            Err(error) => {
                eprintln!("Aeris session shutdown was blocked: {error}");
                None
            }
        }
    })
    .map_err(|error| format!("native account session shutdown guard is unavailable: {error}"))
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

const SIDE_PANEL_INITIAL_WIDTH: f32 = 400.0;
const SIDE_PANEL_MINIMUM_WIDTH: f32 = 360.0;
const SIDE_PANEL_MAXIMUM_WIDTH: f32 = 480.0;
const SIDE_PANEL_RESIZE_HANDLE_WIDTH: f32 = 8.0;
const CONTEXT_PANEL_INITIAL_HEIGHT: f32 = 248.0;
const CONTEXT_PANEL_MINIMUM_HEIGHT: f32 = 140.0;
const CONTEXT_PANEL_MAXIMUM_HEIGHT: f32 = 640.0;
/// Chart area always left above the context panel while resizing.
const CONTEXT_PANEL_MINIMUM_CHART_HEIGHT: f32 = 160.0;
const MAXIMUM_STATUS_CHARACTERS: usize = 160;
const MAXIMUM_OPEN_WORKSPACES: usize = 8;
// PRE-PRODUCTION: re-enable this product restriction and qualify the production
// value before publishing Aeris. Development keeps product gating disabled while
// retaining a hard ceiling so pane-owned charts, market demand, persistence, and
// worker queues remain bounded.
const CHART_PANE_PRODUCT_RESTRICTIONS_ENABLED: bool = false;
const PRODUCTION_CHART_PANE_LIMIT: usize = 4;
const DEVELOPMENT_CHART_PANE_SAFETY_CEILING: usize = 64;
const CHART_PANE_CAPACITY: usize = if CHART_PANE_PRODUCT_RESTRICTIONS_ENABLED {
    PRODUCTION_CHART_PANE_LIMIT
} else {
    DEVELOPMENT_CHART_PANE_SAFETY_CEILING
};

#[derive(Clone, Copy)]
struct PlanLimits {
    workspaces: usize,
    indicators_per_chart: usize,
    extended_timeframes: bool,
}

fn current_plan_limits() -> PlanLimits {
    // Authentication is mandatory, but billing is intentionally not a
    // product-access boundary during early access. Keep one capability shape
    // until paid-plan enforcement is deliberately enabled.
    PlanLimits {
        workspaces: MAXIMUM_OPEN_WORKSPACES,
        indicators_per_chart: usize::MAX,
        extended_timeframes: true,
    }
}
const CHART_CONTEXT_MENU_WIDTH: f32 = 228.0;
const CHART_CONTEXT_MENU_ROW_HEIGHT: f32 = 32.0;
const CHART_CONTEXT_MENU_SEPARATOR_HEIGHT: f32 = 1.0;
/// Share of the shared screen-aware menu growth the chart context menus take.
const CHART_CONTEXT_MENU_GROWTH_SHARE: f32 = 0.5;
const PRICE_AXIS_FLYOUT_WIDTH: f32 = 296.0;
const PRICE_AXIS_FLYOUT_GAP: f32 = 4.0;
const PRICE_AXIS_MENU_GAP: f32 = 4.0;
const ACCOUNT_MENU_GAP: f32 = 4.0;
const OVERLAY_EDGE_MARGIN: f32 = 8.0;
const TIMEFRAME_MENU_WIDTH: f32 = 168.0;
const TIME_ZONE_MENU_WIDTH: f32 = 320.0;
const TIME_ZONE_MENU_MAX_HEIGHT: f32 = 520.0;
const ACCOUNTS_PANEL_WIDTH: f32 = 380.0;
const ACCOUNTS_PANEL_MAX_HEIGHT: f32 = 560.0;
const TIMEFRAME_FLYOUT_WIDTH: f32 = 136.0;
const TIMEFRAME_FLYOUT_GAP: f32 = 5.0;
const QUICK_TIMEFRAME_POPUP_WIDTH: f32 = 300.0;
const QUICK_TIMEFRAME_POPUP_TOP: f32 = 64.0;
const TIMEFRAME_TYPEAHEAD_LIMIT: usize = 8;
const CHART_SETTINGS_MENU_WIDTH: f32 = 260.0;
const CHART_SETTINGS_PANEL_WIDTH: f32 = 840.0;
const CHART_SETTINGS_PANEL_HEIGHT: f32 = 600.0;
/// Share of the shared screen-aware menu growth the chart settings panel takes, so it stays
/// compact on large screens like the chart context menus.
const CHART_SETTINGS_GROWTH_SHARE: f32 = 0.5;
/// Design height of the settings title bar that doubles as the panel's drag handle.
const CHART_SETTINGS_TITLE_BAR_HEIGHT: f32 = 36.0;
const CHART_SETTINGS_SIDEBAR_WIDTH: f32 = 176.0;
const WORKSPACE_TITLE_BAR_HEIGHT: f32 = 42.0;
// GPUI snaps the hover fill and the glyph to device pixels independently, so the
// hit/glyph inset must be a whole, equal number of device pixels on both sides at
// every supported scale. 24/16 satisfies that at 100/125/150/175/200%; an odd or
// fractional inset (the former 13px glyph) leaves the glyph off-centre in its hover.
const WORKSPACE_TAB_ICON_HIT: f32 = 24.0;
const WORKSPACE_TAB_ICON_GLYPH: f32 = 16.0;
const WORKSPACE_PANE_BOTTOM_INSET: f32 = 2.0;
// Bound UI work when a provider delivers a burst of updates. Remaining mailbox
// messages stay queued and wake the next GPUI frame.
const MARKET_MESSAGES_PER_FRAME: usize = 64;
const CHART_SYNC_EVENTS_PER_SURFACE: usize = 32;
const WORKSPACE_TAB_MAX_WIDTH: f32 = 360.0;
const WORKSPACE_TAB_GAP: f32 = 2.0;
const WORKSPACE_TAB_STRIP_PADDING_LEFT: f32 = 8.0;
const WORKSPACE_TAB_HORIZONTAL_PADDING: f32 = 12.0;
const WORKSPACE_TAB_CONTENT_GAP: f32 = 4.0;
const WORKSPACE_TAB_CLOSE_GAP: f32 = 8.0;
const WORKSPACE_TAB_EXCHANGE_GLYPH: f32 = 16.0;
const TOOLTIP_OPEN_DELAY: Duration = Duration::from_millis(400);
const CHROME_OVERLAY_TRANSITION_DURATION: Duration = Duration::from_millis(140);
const CHROME_OVERLAY_EXIT_DURATION: Duration = Duration::from_millis(100);
const COPY_PRICE_FEEDBACK_DURATION: Duration = Duration::from_millis(600);
const COPY_PRICE_SUCCESS_ANIMATION_DURATION: Duration = Duration::from_millis(180);

mod lifecycle;
use lifecycle::DesktopLifecycle;

mod workspace_persistence;
use workspace_persistence::{WorkspaceLayoutPersistence, WorkspaceLayoutShutdownWait};

actions!(
    aeris,
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
        TradingBuyMarket,
        TradingSellMarket,
        TradingCancelAll,
        TradingFlattenAccount,
        TradingKillSwitch,
        OpenCommandPalette,
        ConnectTastytrade,
        DisconnectTastytrade,
        RefreshTastytradeConnection,
    ]
);

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

fn provider_presentation(
    provider: TerminalProvider,
) -> Option<&'static aeris_contracts::ProviderPresentationDescriptor> {
    aeris_market_runtime::built_in_provider_presentations()
        .iter()
        .find(|descriptor| descriptor.id == terminal_provider_id(provider))
}

fn provider_intervals(provider: TerminalProvider) -> &'static [ChartInterval] {
    static INTERVALS: OnceLock<Vec<(&'static str, Vec<ChartInterval>)>> = OnceLock::new();
    INTERVALS
        .get_or_init(|| {
            aeris_market_runtime::built_in_provider_presentations()
                .iter()
                .map(|descriptor| {
                    (
                        descriptor.id,
                        descriptor
                            .chart_interval_labels
                            .iter()
                            .filter_map(|label| {
                                ChartInterval::ALL
                                    .iter()
                                    .copied()
                                    .find(|interval| interval.label() == *label)
                            })
                            .collect(),
                    )
                })
                .collect()
        })
        .iter()
        .find(|(id, _)| *id == terminal_provider_id(provider))
        .map_or(&[], |(_, intervals)| intervals.as_slice())
}

/// Rithmic searches require text, so its default listing is a symbol query;
/// Hyperliquid lists its whole catalog for an empty query.
fn default_listing_query(provider: TerminalProvider) -> &'static str {
    provider_presentation(provider).map_or(DEFAULT_RITHMIC_LISTING_QUERY, |descriptor| {
        descriptor.default_listing
    })
}

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
    chart: Option<Entity<AerisChartView>>,
    order_book: Entity<ReadOnlyOrderBookView>,
    trade_tape: Option<aeris_market_runtime::MarketTradeTapeSnapshot>,
    trade_sweeps: Arc<[OrderFlowSweep]>,
    time_sales_filter: TimeSalesFilter,
    context_view: Option<ContextView>,
    context_snapshot: Arc<ContextSnapshot>,
    context_panel_visible: bool,
    context_panel_tab: ContextPanelTab,
    context_panel_height: f32,
    context_credential_dialog: Option<ContextCredentialDialogState>,
    context_credential_message: Option<String>,
    economic_event_risk_message: Option<String>,
    chart_link_group: u8,
    chart_link_flags: u8,
    pending_chart_sync_events: VecDeque<aeris_chart_integration::ChartSyncEvent>,
    pending_linked_instrument: Option<InstallProviderInstrument>,
    side_panels: SidePanelVisibility,
    side_panel_width: f32,
    side_panel_split_basis_points: u32,
    menu_state: WorkspaceMenuState,
    scrolls: WorkspaceScrollHandles,
    chart_state: ChartState,
    chart_state_message: String,
    theme: AerisTheme,
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
    trading_pnl: TradingPnlState,
    chart_trading_visibility: ChartTradingVisibilitySettings,
    symbol_browser: rithmic_shell::RithmicSymbolBrowser,
    symbol_message: String,
    market_state: WorkspaceMarketState,
    symbol_selection_target: SymbolSelectionTarget,
    pending_symbol_selection_target: Option<SymbolSelectionTarget>,
    pending_watchlist_instrument: Option<InstallProviderInstrument>,
    pending_mnemonic_symbol: Option<String>,
    series_message: String,
    symbol_input: Option<Entity<InputState>>,
    indicator_input: Entity<InputState>,
    timeframe_input: Entity<InputState>,
    time_zone_input: Entity<InputState>,
    indicator_message: Option<String>,
    studies: RuntimeStudiesState,
    study_settings_dialog: Option<StudySettingsDialogState>,
    chrome_overlay: Option<ChromeOverlay>,
    chrome_overlay_phase: ChromeOverlayPhase,
    chrome_overlay_generation: u64,
    chrome_overlay_trigger_position: Option<gpui::Point<Pixels>>,
    timeframe_menu_flyout: Option<TimeframeMenuGroup>,
    timeframe_flyout_close_token: u64,
    timeframe_hover_regions: u32,
    timeframe_trigger_bounds: Option<Bounds<Pixels>>,
    chart_type_trigger_bounds: Option<Bounds<Pixels>>,
    time_zone_trigger_bounds: Option<Bounds<Pixels>>,
    chrome_selection: usize,
    chrome_focus: FocusHandle,
    provider: TerminalProvider,
    symbol_provider: TerminalProvider,
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
    last_chart_clock_revision: u64,
    pending_chart_context_menu: Option<ChartContextRequest>,
    pending_pane_activate: PaneActivationRequest,
    pending_study_settings_request: Option<StudyInstanceId>,
    pending_study_remove_request: Option<StudyInstanceId>,
    resource_class: ConsumerResourceClass,
    chart_chrome: chart_chrome::ChartChromePreferences,
    retained_chart_presentation: RetainedChartPresentation,
    restored_chart_state: Option<WorkspaceChartState>,
    chart_persistence_dirty: bool,
    last_chart_user_state_revision: u64,
    price_alerts: Vec<WorkspacePriceAlertState>,
    price_alert_dialog: Option<PriceAlertDialogState>,
    price_alert_message: Option<String>,
    #[cfg(feature = "diagnostics")]
    foreground_interactions: ForegroundInteractionDiagnostics,
    #[cfg(feature = "diagnostics")]
    live_evidence_enabled: bool,
    #[cfg(feature = "diagnostics")]
    live_evidence_publications: u16,
}

struct ContextCredentialDialogState {
    inputs: Vec<(ContextSource, Entity<InputState>)>,
}

struct TradingPnlState {
    current: Option<aeris_trading::AccountPnl>,
    accounts: Vec<aeris_trading::TradingAccount>,
    orders: Vec<aeris_trading::Order>,
    positions: Vec<aeris_trading_runtime::PositionPnl>,
    risk_profiles: Vec<aeris_trading_runtime::RiskProfile>,
    risk_locks: Vec<aeris_trading_runtime::RiskLock>,
    feedback: Option<aeris_desktop::trading::TradingCommandFeedback>,
    market_error: Option<String>,
    order_entry: TradingOrderEntryState,
    account_creator: Option<PracticeAccountDialogState>,
    account_delete_confirmation: Option<aeris_trading::TradingAccountId>,
    refresh_pending: bool,
    next_refresh: Instant,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ContextPanelTab {
    #[default]
    Calendar,
    Energy,
    Commitments,
    Agriculture,
    Macro,
}

impl ContextPanelTab {
    const ALL: [Self; 5] = [
        Self::Calendar,
        Self::Energy,
        Self::Commitments,
        Self::Agriculture,
        Self::Macro,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Calendar => "Calendar",
            Self::Energy => "Energy",
            Self::Commitments => "COT",
            Self::Agriculture => "Agriculture",
            Self::Macro => "Macro",
        }
    }

    const fn persisted(self) -> u32 {
        match self {
            Self::Calendar => 0,
            Self::Energy => 1,
            Self::Commitments => 2,
            Self::Agriculture => 3,
            Self::Macro => 4,
        }
    }

    const fn from_persisted(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Calendar),
            1 => Some(Self::Energy),
            2 => Some(Self::Commitments),
            3 => Some(Self::Agriculture),
            4 => Some(Self::Macro),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum TimeSalesSideFilter {
    #[default]
    All,
    Buy,
    Sell,
}

impl TimeSalesSideFilter {
    const fn label(self) -> &'static str {
        match self {
            Self::All => "All sides",
            Self::Buy => "Buys",
            Self::Sell => "Sells",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::All => Self::Buy,
            Self::Buy => Self::Sell,
            Self::Sell => Self::All,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TimeSalesFilter {
    side: TimeSalesSideFilter,
    minimum_quantity: f64,
    price_range_ticks: Option<u32>,
}

impl Default for TimeSalesFilter {
    fn default() -> Self {
        Self {
            side: TimeSalesSideFilter::All,
            minimum_quantity: 0.0,
            price_range_ticks: None,
        }
    }
}

impl TimeSalesFilter {
    fn reset(&mut self) -> bool {
        let default = Self::default();
        if *self == default {
            return false;
        }
        *self = default;
        true
    }
}

impl Default for TradingPnlState {
    fn default() -> Self {
        Self {
            current: None,
            accounts: Vec::new(),
            orders: Vec::new(),
            positions: Vec::new(),
            risk_profiles: Vec::new(),
            risk_locks: Vec::new(),
            feedback: None,
            market_error: None,
            order_entry: TradingOrderEntryState::default(),
            account_creator: None,
            account_delete_confirmation: None,
            refresh_pending: false,
            next_refresh: Instant::now(),
        }
    }
}

fn selected_account_lock_reason(state: &TradingPnlState) -> Option<&str> {
    let account_id = state.order_entry.selected_account_id.as_ref()?;
    state
        .risk_locks
        .iter()
        .find(|lock| &lock.account_id == account_id)
        .map(|lock| lock.reason.as_str())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TradingOrderEntryState {
    quantity: u64,
    selected_account_id: Option<aeris_trading::TradingAccountId>,
}

impl Default for TradingOrderEntryState {
    fn default() -> Self {
        Self {
            quantity: 1,
            selected_account_id: None,
        }
    }
}

struct PracticeAccountDialogState {
    name: Entity<InputState>,
    equity: Entity<InputState>,
}

/// Presentation mirror of the runtime-owned tastytrade connection for the Accounts panel.
/// The market runtime owns the connection and its credentials; this holds only the last
/// result the desktop observed.
#[derive(Clone, Default)]
struct TastytradeConnectionView {
    /// `None` until the runtime has been asked; refreshed whenever the Accounts panel opens.
    connected: Option<bool>,
    operation: Option<TastytradeConnectionOperation>,
    message: Option<String>,
    failed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TastytradeConnectionOperation {
    Checking,
    Connecting,
    Disconnecting,
}

#[derive(Clone)]
struct PriceAlertDialogState {
    request: ChartAlertCreateRequest,
    instrument: InstallProviderInstrument,
    condition: PriceAlertCondition,
    frequency: PriceAlertFrequency,
    open_dropdown: Option<PriceAlertDropdown>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PriceAlertDropdown {
    Condition,
    Frequency,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeStudyState {
    study_id: StudyInstanceId,
    persisted: WorkspaceChartStudyState,
    resolved_chart_series: Option<BarSeriesKey>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingRuntimeStudyState {
    persisted: WorkspaceChartStudyState,
    resolved_chart_series: Option<BarSeriesKey>,
    remove_on_registration: bool,
    persist_on_registration: bool,
    blocked: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingStudyReinitialization {
    series: BarSeriesKey,
    replacement_persisted: Option<WorkspaceChartStudyState>,
}

struct StudySettingsDialogState {
    study_id: StudyInstanceId,
    title: String,
    specs: Vec<StudySettingSpec>,
    draft_values: BTreeMap<String, StudySettingValue>,
    /// Host-owned output stroke width; presentation only, so it never reinitializes the study.
    line_width: u8,
    inputs: HashMap<String, Entity<InputState>>,
    _subscriptions: Vec<gpui::Subscription>,
    message: Option<String>,
}

struct RuntimeStudiesState {
    active: Vec<RuntimeStudyState>,
    pending: HashMap<u64, PendingRuntimeStudyState>,
    reinitializing: HashMap<StudyInstanceId, PendingStudyReinitialization>,
    suppressing_outputs: HashSet<StudyInstanceId>,
    automatic_removals: HashSet<StudyInstanceId>,
    removing: HashSet<StudyInstanceId>,
    deferred: Vec<PendingRuntimeStudyState>,
    next_local_id: u64,
}

impl Default for RuntimeStudiesState {
    fn default() -> Self {
        Self {
            active: Vec::new(),
            pending: HashMap::new(),
            reinitializing: HashMap::new(),
            suppressing_outputs: HashSet::new(),
            automatic_removals: HashSet::new(),
            removing: HashSet::new(),
            deferred: Vec::new(),
            next_local_id: 1,
        }
    }
}

impl RuntimeStudiesState {
    fn allocate_local_id(&mut self) -> Option<u64> {
        let local_id = self.next_local_id;
        if local_id == 0 {
            return None;
        }
        self.next_local_id = self.next_local_id.checked_add(1).unwrap_or(0);
        Some(local_id)
    }

    fn begin_reinitialization(
        &mut self,
        study_id: StudyInstanceId,
        pending: PendingStudyReinitialization,
    ) {
        self.reinitializing.insert(study_id, pending);
        self.suppressing_outputs.insert(study_id);
    }

    fn complete_reinitialization(&mut self, study_id: StudyInstanceId) -> Option<bool> {
        let pending = self.reinitializing.remove(&study_id)?;
        let mut persisted_changed = false;
        if let Some(state) = self
            .active
            .iter_mut()
            .find(|state| state.study_id == study_id)
        {
            state.resolved_chart_series = Some(pending.series);
            if let Some(replacement) = pending.replacement_persisted {
                state.persisted = replacement;
                persisted_changed = true;
            }
        }
        Some(persisted_changed)
    }

    fn cancel_reinitialization(&mut self, study_id: StudyInstanceId) {
        self.reinitializing.remove(&study_id);
        self.suppressing_outputs.remove(&study_id);
    }

    fn invalidate_study_outputs(&mut self, study_ids: &[StudyInstanceId]) {
        for study_id in study_ids {
            if !self.reinitializing.contains_key(study_id) {
                self.suppressing_outputs.remove(study_id);
            }
        }
    }

    fn suppresses_output(&self, study_id: StudyInstanceId) -> bool {
        self.suppressing_outputs.contains(&study_id) || self.automatic_removals.contains(&study_id)
    }

    fn remove_runtime_subtree(&mut self, study_ids: &[StudyInstanceId]) -> bool {
        let removed_runtime_ids = study_ids.iter().copied().collect::<HashSet<_>>();
        let mut removed_local_ids = self
            .active
            .iter()
            .filter(|state| removed_runtime_ids.contains(&state.study_id))
            .map(|state| state.persisted.local_id)
            .collect::<HashSet<_>>();

        loop {
            let previous_len = removed_local_ids.len();
            for state in self.pending.values().chain(self.deferred.iter()) {
                if state.persisted.dependencies.iter().any(|dependency| {
                    WorkspaceStudyDependencyKind::try_from(dependency.kind).is_ok_and(|kind| {
                        kind == WorkspaceStudyDependencyKind::StudyOutput
                            && removed_local_ids.contains(&dependency.study_local_id)
                    })
                }) {
                    removed_local_ids.insert(state.persisted.local_id);
                }
            }
            if removed_local_ids.len() == previous_len {
                break;
            }
        }

        let previous_len = self.active.len();
        self.active
            .retain(|state| !removed_runtime_ids.contains(&state.study_id));
        let mut changed = self.active.len() != previous_len;
        for state in self.pending.values_mut() {
            if removed_local_ids.contains(&state.persisted.local_id)
                && !state.remove_on_registration
            {
                state.remove_on_registration = true;
                changed = true;
            }
        }
        let previous_deferred_len = self.deferred.len();
        self.deferred
            .retain(|state| !removed_local_ids.contains(&state.persisted.local_id));
        changed |= self.deferred.len() != previous_deferred_len;
        for study_id in study_ids {
            self.reinitializing.remove(study_id);
            self.suppressing_outputs.remove(study_id);
            self.automatic_removals.remove(study_id);
            self.removing.remove(study_id);
        }
        changed
    }
}

#[derive(Default)]
struct WorkspaceMenuState {
    order_book_column_open: bool,
    timeframe_flyout_keyboard: bool,
    chrome_list_keyboard: bool,
    /// Header Accounts trigger, measured each frame so its panel opens beneath it.
    accounts_trigger_bounds: Option<Bounds<Pixels>>,
    /// Why the last quick-timeframe submission was rejected; cleared by an edit or close.
    quick_timeframe_error: Option<QuickTimeframeError>,
}

/// A rejected quick-timeframe submission. The rejected text is kept so only a real edit
/// clears the error: the input also reports a change when Enter submits unchanged text.
struct QuickTimeframeError {
    query: String,
    message: String,
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
    chart_state: Option<WorkspaceChartState>,
    instrument_id: Option<String>,
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
    time_zone: ScrollHandle,
    time_sales: ScrollHandle,
    context: ScrollHandle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalProvider {
    Tastytrade,
    Rithmic,
    Hyperliquid,
}

const fn terminal_provider_id(provider: TerminalProvider) -> &'static str {
    match provider {
        TerminalProvider::Rithmic => "rithmic",
        TerminalProvider::Hyperliquid => "hyperliquid",
        TerminalProvider::Tastytrade => "tastytrade",
    }
}

fn terminal_provider_display(provider: TerminalProvider) -> &'static str {
    provider_presentation(provider).map_or(terminal_provider_id(provider), |descriptor| {
        descriptor.display_name
    })
}

fn provider_ready_message(
    provider: TerminalProvider,
    product: &InstallProviderInstrument,
) -> String {
    let descriptor = provider_presentation(provider);
    let symbol = match descriptor.map(|descriptor| descriptor.catalog_symbol) {
        Some(aeris_contracts::ProviderCatalogSymbol::DisplaySymbol) => {
            product.display_symbol.as_str()
        }
        Some(aeris_contracts::ProviderCatalogSymbol::ProviderSymbol) | None => {
            product.provider_symbol.as_str()
        }
    };
    let display_name = descriptor.map_or_else(
        || terminal_provider_id(provider),
        |descriptor| descriptor.display_name,
    );
    let suffix = descriptor.map_or("", |descriptor| descriptor.ready_label_suffix);
    format!("{symbol} · {display_name}{suffix}")
}

fn terminal_provider_from_id(provider: &str) -> TerminalProvider {
    if provider == "tastytrade" {
        TerminalProvider::Tastytrade
    } else if provider == terminal_provider_id(TerminalProvider::Hyperliquid) {
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
    TimeZone,
    Accounts,
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

fn chart_decimal(units: i64, scale: u8) -> Option<f64> {
    let value = units.to_f64()? * 10_f64.powi(-(i32::from(scale)));
    value.is_finite().then_some(value)
}

fn chart_price(value: Option<aeris_trading::FixedPoint>) -> Option<f64> {
    value.and_then(|value| chart_decimal(value.units(), value.scale()))
}

fn chart_quantity(value: aeris_trading::FixedPoint) -> Option<f64> {
    chart_decimal(value.units().unsigned_abs().try_into().ok()?, value.scale())
}

fn chart_working_orders(
    snapshot: &aeris_trading_runtime::TradingSnapshot,
    account_id: &aeris_trading::TradingAccountId,
    instrument_id: &str,
    price_increment: Option<f64>,
) -> Vec<ChartWorkingOrder> {
    snapshot
        .orders
        .iter()
        // The chart draws actionable lines. Cancelled, rejected and filled orders stay in the
        // runtime's history but must leave the chart, or a cancel appears to do nothing.
        .filter(|order| {
            order.status.is_open()
                && &order.account_id == account_id
                && order.instrument_id.as_str() == instrument_id
        })
        .filter_map(|order| {
            let price = chart_price(order.limit_price.or(order.stop_price))?;
            let id = ChartOrderId::new(order.client_order_id.as_str()).ok()?;
            let side = match order.side {
                aeris_trading::OrderSide::Buy => ChartOrderSide::Buy,
                aeris_trading::OrderSide::Sell => ChartOrderSide::Sell,
            };
            let kind = match order.order_type {
                aeris_trading::OrderType::Market => ChartOrderKind::Market,
                aeris_trading::OrderType::Limit => ChartOrderKind::Limit,
                aeris_trading::OrderType::Stop => ChartOrderKind::Stop,
                aeris_trading::OrderType::StopLimit => ChartOrderKind::StopLimit,
            };
            let status = match order.status {
                aeris_trading::OrderStatus::Pending => ChartOrderStatus::PendingSubmit,
                aeris_trading::OrderStatus::Working => ChartOrderStatus::Working,
                aeris_trading::OrderStatus::PendingModify => ChartOrderStatus::PendingModify,
                aeris_trading::OrderStatus::PartiallyFilled => ChartOrderStatus::PartiallyFilled,
                aeris_trading::OrderStatus::PendingCancel => ChartOrderStatus::PendingCancel,
                aeris_trading::OrderStatus::Filled => ChartOrderStatus::Filled,
                aeris_trading::OrderStatus::Cancelled => ChartOrderStatus::Cancelled,
                aeris_trading::OrderStatus::Rejected => ChartOrderStatus::Rejected,
            };
            let semantics = chart_order_semantics(snapshot, order, price_increment);
            Some(ChartWorkingOrder {
                id,
                account_id: None,
                pane_index: 0,
                price_scale: ChartTradingPriceScale::Right,
                side,
                kind,
                role: semantics.role,
                status,
                price,
                stop_price: chart_price(order.stop_price),
                trailing_trigger_price: semantics.trailing_trigger_price,
                break_even_trigger_price: semantics.break_even_trigger_price,
                quantity: chart_quantity(order.quantity)?,
                filled_quantity: chart_quantity(order.filled_quantity)?,
                position_id: semantics.position_id,
                parent_order_id: semantics.parent_order_id,
                bracket_id: semantics.bracket_id.clone(),
                oco_group_id: semantics.bracket_id,
                revision: 0,
                annotations: semantics.annotations,
            })
        })
        .collect()
}

struct ChartOrderSemantics {
    role: ChartOrderRole,
    position_id: Option<ChartPositionId>,
    parent_order_id: Option<ChartOrderId>,
    bracket_id: Option<ChartTradingGroupId>,
    trailing_trigger_price: Option<f64>,
    break_even_trigger_price: Option<f64>,
    annotations: Vec<ChartTradingAnnotation>,
}

fn chart_order_semantics(
    snapshot: &aeris_trading_runtime::TradingSnapshot,
    order: &aeris_trading::Order,
    price_increment: Option<f64>,
) -> ChartOrderSemantics {
    let managed = snapshot.managed_brackets.iter().find(|bracket| {
        bracket.stop_client_order_id.as_ref() == Some(&order.client_order_id)
            || bracket
                .target_client_order_ids
                .contains(&order.client_order_id)
    });
    let protective = snapshot
        .protective_orders
        .iter()
        .find(|protective| protective.client_order_id == order.client_order_id);
    let role = managed.map_or_else(
        || {
            protective.map_or(ChartOrderRole::Working, |protective| {
                match protective.role {
                    aeris_trading_runtime::ProtectiveOrderRole::StopLoss => {
                        ChartOrderRole::StopLoss
                    }
                    aeris_trading_runtime::ProtectiveOrderRole::TakeProfit => {
                        ChartOrderRole::TakeProfit
                    }
                }
            })
        },
        |bracket| {
            if bracket.stop_client_order_id.as_ref() == Some(&order.client_order_id) {
                ChartOrderRole::StopLoss
            } else {
                ChartOrderRole::TakeProfit
            }
        },
    );
    let bracket_id =
        managed.and_then(|bracket| ChartTradingGroupId::new(bracket.bracket_id.clone()).ok());
    let parent_order_id =
        managed.and_then(|bracket| ChartOrderId::new(bracket.entry_client_order_id.as_str()).ok());
    let is_protection = managed.is_some() || protective.is_some();
    let position_id = is_protection
        .then(|| chart_position_id(&order.account_id, order.instrument_id.as_str()))
        .flatten();
    let (trailing_trigger_price, break_even_trigger_price) = managed
        .map_or((None, None), |bracket| {
            managed_trigger_prices(snapshot, bracket, price_increment)
        });
    // Every practice protection is locally managed, so a chip saying so carries no information
    // and an inline chip would cover the marker's quantity, drag surface and cursor.
    let mut annotations = Vec::new();
    if let Some(warning) = snapshot.order_events.iter().find_map(|event| {
        (event.order_id == order.id && event.kind == aeris_trading::OrderEventKind::Accepted)
            .then_some(event.detail.as_deref())
            .flatten()
            .filter(|detail| detail.starts_with("RISK WARNING"))
    }) {
        annotations.push(ChartTradingAnnotation {
            id: "risk-warning".to_string(),
            text: "RISK".to_string(),
            tooltip: Some(warning.to_string()),
            tone: ChartTradingAnnotationTone::Warning,
            // Above the line, so the warning never covers the marker's drag and close controls.
            placement: ChartTradingAnnotationPlacement::Above,
        });
    }
    ChartOrderSemantics {
        role,
        position_id,
        parent_order_id,
        bracket_id,
        trailing_trigger_price,
        break_even_trigger_price,
        annotations,
    }
}

fn managed_trigger_prices(
    snapshot: &aeris_trading_runtime::TradingSnapshot,
    bracket: &aeris_trading_runtime::ManagedBracket,
    price_increment: Option<f64>,
) -> (Option<f64>, Option<f64>) {
    let entry_price = bracket
        .entry_price
        .and_then(|price| chart_price(Some(price)));
    let increment = price_increment.filter(|increment| increment.is_finite() && *increment > 0.0);
    let side = snapshot
        .orders
        .iter()
        .find(|order| order.client_order_id == bracket.entry_client_order_id)
        .map(|order| order.side);
    let signed_increment = match side {
        Some(aeris_trading::OrderSide::Buy) => increment,
        Some(aeris_trading::OrderSide::Sell) => increment.map(|increment| -increment),
        None => None,
    };
    let trigger = |ticks: u32| {
        entry_price
            .zip(signed_increment)
            .and_then(|(entry, increment)| {
                let price = entry + increment * f64::from(ticks);
                price.is_finite().then_some(price)
            })
    };
    (
        bracket
            .template
            .trailing_stop
            .map(|rule| rule.activation_ticks)
            .and_then(trigger),
        bracket
            .template
            .break_even
            .map(|rule| rule.activation_ticks)
            .and_then(trigger),
    )
}

fn chart_positions(
    snapshot: &aeris_trading_runtime::TradingSnapshot,
    account_id: &aeris_trading::TradingAccountId,
    instrument_id: &str,
    currency: &str,
) -> Vec<ChartTradingPosition> {
    snapshot
        .position_pnl
        .iter()
        .filter(|position| {
            &position.position.account_id == account_id
                && position.position.instrument_id.as_str() == instrument_id
                && position.position.net_quantity.units() != 0
        })
        .filter_map(|position| {
            let position_id = chart_position_id(account_id, instrument_id)?;
            let average_price = chart_price(position.position.average_entry_price)?;
            let quantity = chart_quantity(position.position.net_quantity)?;
            let side = if position.position.net_quantity.units() > 0 {
                ChartPositionSide::Long
            } else {
                ChartPositionSide::Short
            };
            Some(ChartTradingPosition {
                id: position_id,
                account_id: None,
                pane_index: 0,
                price_scale: ChartTradingPriceScale::Right,
                side,
                average_price,
                quantity,
                display_pnl: chart_price(Some(position.position.unrealized_pnl)),
                currency: Some(currency.to_string()),
                annotations: Vec::new(),
            })
        })
        .collect()
}

fn chart_position_id(
    account_id: &aeris_trading::TradingAccountId,
    instrument_id: &str,
) -> Option<ChartPositionId> {
    ChartPositionId::new(format!("position:{}:{instrument_id}", account_id.as_str())).ok()
}

fn chart_time_seconds_from_unix_nanos(unix_nanos: i64) -> i64 {
    unix_nanos.div_euclid(1_000_000_000)
}

fn chart_executions(
    snapshot: &aeris_trading_runtime::TradingSnapshot,
    account_id: &aeris_trading::TradingAccountId,
    instrument_id: &str,
) -> Vec<ChartTradingExecution> {
    snapshot
        .fills
        .iter()
        .filter(|fill| {
            &fill.account_id == account_id && fill.instrument_id.as_str() == instrument_id
        })
        .filter_map(|fill| {
            let id = ChartExecutionId::new(fill.id.as_str()).ok()?;
            let order_id = snapshot
                .orders
                .iter()
                .find(|order| order.id == fill.order_id)
                .and_then(|order| ChartOrderId::new(order.client_order_id.as_str()).ok());
            let side = match fill.side {
                aeris_trading::OrderSide::Buy => ChartOrderSide::Buy,
                aeris_trading::OrderSide::Sell => ChartOrderSide::Sell,
            };
            Some(ChartTradingExecution {
                id,
                account_id: None,
                pane_index: 0,
                price_scale: ChartTradingPriceScale::Right,
                side,
                kind: ChartExecutionKind::PartialFill,
                time: chart_time_seconds_from_unix_nanos(fill.execution_unix_nanos),
                price: chart_price(Some(fill.price))?,
                quantity: chart_quantity(fill.quantity)?,
                order_id,
                position_id: None,
                marker_shape: ChartExecutionMarkerShape::default(),
                size_by_quantity: false,
            })
        })
        .collect()
}

fn chart_trading_snapshot(
    snapshot: &aeris_trading_runtime::TradingSnapshot,
    product: Option<&InstallProviderInstrument>,
    selected_account: Option<&aeris_trading::TradingAccountId>,
) -> Option<ChartTradingSnapshot> {
    let product = product?;
    let account_id = selected_account?;
    let price_scale = u8::try_from(product.price_scale).ok()?;
    let account = snapshot
        .accounts
        .iter()
        .find(|account| &account.id == account_id)?;
    let instrument_id = product.instrument_id.as_str();
    let price_increment = product
        .price_increment
        .and_then(|units| chart_decimal(units, price_scale));
    let contract = product.contract_metadata.as_deref();
    let point_value = contract.and_then(|metadata| {
        metadata
            .point_value
            .zip(metadata.point_value_scale)
            .and_then(|(units, scale)| {
                u8::try_from(scale)
                    .ok()
                    .and_then(|scale| chart_decimal(units, scale))
            })
    });
    // Fixed-point scales are storage precision (Hyperliquid stores every value at eight places),
    // not display precision. Leaving both unset lets the chart format trading prices with its
    // price-axis formatter and show sizes without trailing zeros.
    let instrument = ChartInstrumentMetadata {
        tick_size: price_increment,
        price_precision: None,
        quantity_precision: None,
        minimum_quantity: None,
        point_value,
        currency: Some(account.currency.clone()),
    };

    let orders = chart_working_orders(snapshot, account_id, instrument_id, price_increment);

    let positions = chart_positions(snapshot, account_id, instrument_id, &account.currency);

    let executions = chart_executions(snapshot, account_id, instrument_id);

    Some(ChartTradingSnapshot {
        instrument,
        positions,
        orders,
        executions,
        round_trips: Vec::new(),
    })
}

fn chart_session_plan_overlay(
    snapshot: &aeris_trading_runtime::TradingSnapshot,
    selected_account: Option<&aeris_trading::TradingAccountId>,
) -> ChartHostOverlaySnapshot {
    let Some(plan) = selected_account.and_then(|account_id| {
        snapshot
            .session_plans
            .iter()
            .find(|plan| &plan.account_id == account_id)
    }) else {
        return ChartHostOverlaySnapshot::default();
    };
    let bias = match plan.bias {
        aeris_trading_runtime::SessionBias::Long => "LONG",
        aeris_trading_runtime::SessionBias::Short => "SHORT",
        aeris_trading_runtime::SessionBias::Neutral => "NEUTRAL",
    };
    let setup = plan.active_setup.as_deref().unwrap_or("setup pending");
    let label = format!("PLAN {bias} · {setup} · {} levels", plan.levels.len())
        .chars()
        .take(96)
        .collect::<String>();
    ChartHostOverlaySnapshot {
        events: vec![
            ChartHostEventMarker {
                id: "session-plan-start".to_string(),
                time: chart_time_seconds_from_unix_nanos(plan.session_start_unix_nanos),
                importance: 1,
                label: label.clone(),
                icon: None,
            },
            ChartHostEventMarker {
                id: "session-plan-end".to_string(),
                time: chart_time_seconds_from_unix_nanos(plan.session_end_unix_nanos),
                importance: 1,
                label: "PLAN END".to_string(),
                icon: None,
            },
        ],
        windows: vec![ChartHostTimeWindow {
            id: "session-plan-window".to_string(),
            start_time: chart_time_seconds_from_unix_nanos(plan.session_start_unix_nanos),
            end_time: chart_time_seconds_from_unix_nanos(plan.session_end_unix_nanos),
            label,
        }],
    }
}

fn chart_session_plan_levels(
    snapshot: &aeris_trading_runtime::TradingSnapshot,
    product: Option<&InstallProviderInstrument>,
    selected_account: Option<&aeris_trading::TradingAccountId>,
) -> Vec<(f64, String)> {
    let (Some(product), Some(account_id)) = (product, selected_account) else {
        return Vec::new();
    };
    snapshot
        .session_plans
        .iter()
        .find(|plan| &plan.account_id == account_id)
        .into_iter()
        .flat_map(|plan| &plan.levels)
        .filter(|level| level.instrument_id.as_str() == product.instrument_id)
        .filter_map(|level| {
            chart_price(Some(level.price)).map(|price| (price, level.label.clone()))
        })
        .collect()
}

fn chart_price_fixed_point(value: f64, scale: u32) -> Option<aeris_trading::FixedPoint> {
    if !value.is_finite() || scale > u32::from(aeris_trading::MAXIMUM_DECIMAL_SCALE) {
        return None;
    }
    let multiplier = 10_f64.powi(i32::try_from(scale).ok()?);
    let units = value * multiplier;
    let rounded = units.round();
    if !rounded.is_finite() || (units - rounded).abs() > 1.0e-6 {
        return None;
    }
    let units = rounded.to_i64()?;
    aeris_trading::FixedPoint::try_new(units, u8::try_from(scale).ok()?).ok()
}

fn chart_quantity_fixed_point(value: f64, scale: u32) -> Option<aeris_trading::FixedPoint> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    chart_price_fixed_point(value, scale)
}

fn chart_order_from_intent(
    intent: &ChartTradingIntent,
    app: &WorkspaceSurface,
    frame: &aeris_market_data::OrderBookFrame,
    order_type: aeris_trading::OrderType,
) -> Option<(
    aeris_trading_runtime::PlaceOrder,
    aeris_trading_runtime::SimulatedMarketObservation,
)> {
    let product = app.product.as_ref()?;
    let account_id = app.trading_pnl.order_entry.selected_account_id.clone()?;
    let side = match intent.side? {
        ChartOrderSide::Buy => aeris_trading::OrderSide::Buy,
        ChartOrderSide::Sell => aeris_trading::OrderSide::Sell,
    };
    let price = intent
        .price
        .and_then(|price| chart_price_fixed_point(price, product.price_scale))?;
    let quantity = intent
        .quantity
        .and_then(|quantity| chart_quantity_fixed_point(quantity, product.quantity_scale))?;
    let submitted_unix_nanos = aeris_desktop::trading::now();
    let provenance = aeris_trading::TradingProvenance {
        venue_id: "aeris-sim".to_string(),
        provider_id: frame.provider_id.clone(),
        session_generation: frame.session_generation,
        source_sequence: frame
            .source_watermark
            .max(frame.bbo_source_watermark)
            .max(1),
        observed_unix_nanos: submitted_unix_nanos,
    };
    let client_order_id = aeris_trading::ClientOrderId::try_new(format!(
        "chart-{}-{submitted_unix_nanos}",
        intent.sequence
    ))
    .ok()?;
    let (limit_price, stop_price) = match order_type {
        aeris_trading::OrderType::Market => (None, None),
        aeris_trading::OrderType::Limit => (Some(price), None),
        aeris_trading::OrderType::Stop => (None, Some(price)),
        aeris_trading::OrderType::StopLimit => (Some(price), Some(price)),
    };
    let instrument_id =
        aeris_instruments::InstrumentId::try_new(product.instrument_id.clone()).ok()?;
    let order = aeris_trading_runtime::PlaceOrder {
        client_order_id,
        account_id,
        instrument_id: instrument_id.clone(),
        side,
        order_type,
        time_in_force: aeris_trading::TimeInForce::Day,
        quantity,
        limit_price,
        stop_price,
        submitted_unix_nanos,
        provenance: provenance.clone(),
    };
    let bid = frame.best_bid.as_ref()?;
    let ask = frame.best_ask.as_ref()?;
    Some((
        order,
        aeris_trading_runtime::SimulatedMarketObservation {
            instrument_id,
            bid: aeris_trading::FixedPoint::try_new(bid.price, frame.price_scale).ok()?,
            ask: aeris_trading::FixedPoint::try_new(ask.price, frame.price_scale).ok()?,
            provenance,
        },
    ))
}

/// Rolls back a chart trading intent the desktop could not dispatch, and says so in the
/// order-entry status line instead of leaving the chart silently unchanged.
fn reject_chart_intent(
    chart: &Entity<AerisChartView>,
    sequence: u32,
    cx: &mut Context<WorkspaceSurface>,
) {
    aeris_desktop::trading::record_outcome(
        Err::<(), _>(
            "Chart order action needs a selected, unlocked practice account, a current bid and              ask, and the chart's instrument"
                .to_string(),
        ),
        "",
    );
    chart.update(cx, |chart, _| {
        chart.resolve_trading_intent(sequence, false);
    });
}

fn dispatch_chart_cancel(
    chart: Entity<AerisChartView>,
    sequence: u32,
    client_order_id: aeris_trading::ClientOrderId,
    service: aeris_trading_runtime::TradingService,
    cx: &mut Context<WorkspaceSurface>,
) {
    let task = cx.background_executor().spawn(async move {
        aeris_desktop::trading::record_outcome(
            service.cancel_order(client_order_id),
            "Practice order cancelled",
        )
    });
    cx.spawn(async move |_, cx| {
        let accepted = task.await;
        chart.update(cx, |chart, _| {
            chart.resolve_trading_intent(sequence, accepted);
        });
    })
    .detach();
}

fn chart_tick_offset(
    first: aeris_trading::FixedPoint,
    second: aeris_trading::FixedPoint,
    tick: i64,
) -> Option<u32> {
    if first.scale() != second.scale() || tick <= 0 {
        return None;
    }
    let distance = first.units().abs_diff(second.units());
    let tick = u64::try_from(tick).ok()?;
    (distance > 0 && distance.is_multiple_of(tick))
        .then(|| u32::try_from(distance / tick).ok())
        .flatten()
}

fn dispatch_chart_place_bracket(
    chart: Entity<AerisChartView>,
    intent: &ChartTradingIntent,
    app: &WorkspaceSurface,
    cx: &mut Context<WorkspaceSurface>,
) {
    let sequence = intent.sequence;
    let Some(frame) = app.order_book.read(cx).frame().cloned() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some((entry, observation)) =
        chart_order_from_intent(intent, app, &frame, aeris_trading::OrderType::Limit)
    else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some(product) = app.product.as_ref() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some(stop_price) = intent
        .stop_loss_price
        .and_then(|price| chart_price_fixed_point(price, product.price_scale))
    else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some(target_price) = intent
        .take_profit_price
        .and_then(|price| chart_price_fixed_point(price, product.price_scale))
    else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some(entry_price) = entry.limit_price else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let tick = product.price_increment.filter(|tick| *tick > 0);
    let valid_relation = match entry.side {
        aeris_trading::OrderSide::Buy => {
            stop_price.units() < entry_price.units() && target_price.units() > entry_price.units()
        }
        aeris_trading::OrderSide::Sell => {
            stop_price.units() > entry_price.units() && target_price.units() < entry_price.units()
        }
    };
    let Some((stop_offset_ticks, target_offset_ticks)) = tick.and_then(|tick| {
        valid_relation.then(|| {
            Some((
                chart_tick_offset(entry_price, stop_price, tick)?,
                chart_tick_offset(entry_price, target_price, tick)?,
            ))
        })?
    }) else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let template = aeris_trading_runtime::BracketStrategyTemplate {
        template_id: format!("chart-bracket-{}", entry.client_order_id.as_str()),
        revision: 1,
        name: "Chart bracket".to_string(),
        stop_offset_ticks,
        targets: vec![aeris_trading_runtime::BracketTarget {
            offset_ticks: target_offset_ticks,
            quantity_percent: 100,
        }],
        trailing_stop: None,
        break_even: None,
        enabled: true,
    };
    let Some(service) = aeris_desktop::trading::handle() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let task = cx.background_executor().spawn(async move {
        let result = service
            .place_inline_bracket(aeris_trading_runtime::PlaceInlineBracket { entry, template })
            .and_then(|_| service.observe_market(observation));
        aeris_desktop::trading::record_outcome(result, "Practice bracket placed")
    });
    cx.spawn(async move |_, cx| {
        let accepted = task.await;
        chart.update(cx, |chart, _| {
            chart.resolve_trading_intent(sequence, accepted);
        });
    })
    .detach();
}

fn dispatch_chart_create_protection(
    chart: Entity<AerisChartView>,
    intent: &ChartTradingIntent,
    app: &WorkspaceSurface,
    cx: &mut Context<WorkspaceSurface>,
) {
    let sequence = intent.sequence;
    let Some(account_id) = app.trading_pnl.order_entry.selected_account_id.as_ref() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some(product) = app.product.as_ref() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let identity_matches = intent.position_id.as_ref().is_some_and(|position_id| {
        chart_position_id(account_id, &product.instrument_id).as_ref() == Some(position_id)
    }) || intent.order_id.as_ref().is_some_and(|order_id| {
        app.trading_pnl.orders.iter().any(|order| {
            order.client_order_id.as_str() == order_id.as_str()
                && &order.account_id == account_id
                && order.instrument_id.as_str() == product.instrument_id
        })
    });
    if !identity_matches {
        reject_chart_intent(&chart, sequence, cx);
        return;
    }
    let Some(frame) = app.order_book.read(cx).frame().cloned() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let order_type = if intent.action == ChartTradingIntentAction::CreateStopLoss {
        aeris_trading::OrderType::Stop
    } else {
        aeris_trading::OrderType::Limit
    };
    let Some((order, observation)) = chart_order_from_intent(intent, app, &frame, order_type)
    else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some(service) = aeris_desktop::trading::handle() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let role = if intent.action == ChartTradingIntentAction::CreateStopLoss {
        aeris_trading_runtime::ProtectiveOrderRole::StopLoss
    } else {
        aeris_trading_runtime::ProtectiveOrderRole::TakeProfit
    };
    let task = cx.background_executor().spawn(async move {
        let result = service
            .place_protective_order(aeris_trading_runtime::PlaceProtectiveOrder { order, role })
            .and_then(|_| service.observe_market(observation));
        aeris_desktop::trading::record_outcome(result, "Practice protective order placed")
    });
    cx.spawn(async move |_, cx| {
        let accepted = task.await;
        chart.update(cx, |chart, _| {
            chart.resolve_trading_intent(sequence, accepted);
        });
    })
    .detach();
}

fn dispatch_chart_modify(
    chart: Entity<AerisChartView>,
    intent: &ChartTradingIntent,
    order: &aeris_trading::Order,
    app: &WorkspaceSurface,
    cx: &mut Context<WorkspaceSurface>,
) {
    let sequence = intent.sequence;
    let Some(product) = app.product.as_ref() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some(frame) = app.order_book.read(cx).frame().cloned() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some(service) = aeris_desktop::trading::handle() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some(order_id) = intent.order_id.as_ref().map(ChartOrderId::as_str) else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Ok(client_order_id) = aeris_trading::ClientOrderId::try_new(order_id.to_string()) else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some(price) = intent
        .price
        .and_then(|value| chart_price_fixed_point(value, product.price_scale))
    else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let (limit_price, stop_price) = match order.order_type {
        aeris_trading::OrderType::Limit => (Some(price), None),
        aeris_trading::OrderType::Stop => (None, Some(price)),
        aeris_trading::OrderType::Market | aeris_trading::OrderType::StopLimit => {
            reject_chart_intent(&chart, sequence, cx);
            return;
        }
    };
    let modified_unix_nanos = aeris_desktop::trading::now();
    let command = aeris_trading_runtime::ModifyOrder {
        client_order_id,
        time_in_force: order.time_in_force,
        limit_price,
        stop_price,
        modified_unix_nanos,
        provenance: aeris_trading::TradingProvenance {
            venue_id: "aeris-sim".to_string(),
            provider_id: frame.provider_id.clone(),
            session_generation: frame.session_generation,
            source_sequence: frame
                .source_watermark
                .max(frame.bbo_source_watermark)
                .max(1),
            observed_unix_nanos: modified_unix_nanos,
        },
    };
    let task = cx.background_executor().spawn(async move {
        aeris_desktop::trading::record_outcome(
            service.modify_order(command),
            "Practice order modified",
        )
    });
    cx.spawn(async move |_, cx| {
        let accepted = task.await;
        chart.update(cx, |chart, _| {
            chart.resolve_trading_intent(sequence, accepted);
        });
    })
    .detach();
}

fn dispatch_chart_close_position(
    chart: Entity<AerisChartView>,
    intent: &ChartTradingIntent,
    app: &WorkspaceSurface,
    cx: &mut Context<WorkspaceSurface>,
) {
    let sequence = intent.sequence;
    let Some(account_id) = app.trading_pnl.order_entry.selected_account_id.as_ref() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some(product) = app.product.as_ref() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some(expected_position_id) = chart_position_id(account_id, &product.instrument_id) else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    if intent.position_id.as_ref() != Some(&expected_position_id)
        || !app.trading_pnl.positions.iter().any(|position| {
            &position.position.account_id == account_id
                && position.position.instrument_id.as_str() == product.instrument_id
                && position.position.net_quantity.units() != 0
        })
    {
        reject_chart_intent(&chart, sequence, cx);
        return;
    }
    let Some(frame) = app.order_book.read(cx).frame().cloned() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some((account_id, observation)) =
        aeris_desktop::trading::prepare_flatten_for(&frame, Some(account_id.as_str().to_string()))
    else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some(service) = aeris_desktop::trading::handle() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let task = cx.background_executor().spawn(async move {
        aeris_desktop::trading::record_outcome(
            service.flatten_account(account_id, observation),
            "Practice position closed",
        )
    });
    cx.spawn(async move |_, cx| {
        let accepted = task.await;
        chart.update(cx, |chart, _| {
            chart.resolve_trading_intent(sequence, accepted);
        });
    })
    .detach();
}

fn dispatch_chart_trading_intent(
    chart: Entity<AerisChartView>,
    intent: &ChartTradingIntent,
    app: &WorkspaceSurface,
    cx: &mut Context<WorkspaceSurface>,
) {
    let sequence = intent.sequence;
    let action = intent.action;
    if !chart_intent_has_confirmation_evidence(intent) {
        reject_chart_intent(&chart, sequence, cx);
        return;
    }
    if selected_account_lock_reason(&app.trading_pnl).is_some()
        && !matches!(
            action,
            ChartTradingIntentAction::CancelOrder | ChartTradingIntentAction::ClosePosition
        )
    {
        reject_chart_intent(&chart, sequence, cx);
        return;
    }
    let supported = matches!(
        action,
        ChartTradingIntentAction::PlaceBracketOrder
            | ChartTradingIntentAction::ModifyOrder
            | ChartTradingIntentAction::CancelOrder
            | ChartTradingIntentAction::CreateStopLoss
            | ChartTradingIntentAction::CreateTakeProfit
            | ChartTradingIntentAction::ClosePosition
    );
    if !supported {
        reject_chart_intent(&chart, sequence, cx);
        return;
    }
    if action == ChartTradingIntentAction::ClosePosition {
        dispatch_chart_close_position(chart, intent, app, cx);
        return;
    }
    if action == ChartTradingIntentAction::PlaceBracketOrder {
        dispatch_chart_place_bracket(chart, intent, app, cx);
        return;
    }
    if matches!(
        action,
        ChartTradingIntentAction::CreateStopLoss | ChartTradingIntentAction::CreateTakeProfit
    ) {
        dispatch_chart_create_protection(chart, intent, app, cx);
        return;
    }
    let Some(order_id) = intent.order_id.as_ref().map(ChartOrderId::as_str) else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some(order) = app
        .trading_pnl
        .orders
        .iter()
        .find(|order| order.client_order_id.as_str() == order_id)
    else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Some(selected_account) = app.trading_pnl.order_entry.selected_account_id.as_ref() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    if &order.account_id != selected_account
        || app
            .product
            .as_ref()
            .is_none_or(|product| product.instrument_id != order.instrument_id.as_str())
    {
        reject_chart_intent(&chart, sequence, cx);
        return;
    }
    let Some(service) = aeris_desktop::trading::handle() else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    let Ok(client_order_id) = aeris_trading::ClientOrderId::try_new(order_id.to_string()) else {
        reject_chart_intent(&chart, sequence, cx);
        return;
    };
    match action {
        ChartTradingIntentAction::CancelOrder => {
            dispatch_chart_cancel(chart, sequence, client_order_id, service, cx);
        }
        ChartTradingIntentAction::ModifyOrder => {
            dispatch_chart_modify(chart, intent, order, app, cx);
        }
        _ => reject_chart_intent(&chart, sequence, cx),
    }
}

fn chart_intent_has_confirmation_evidence(intent: &ChartTradingIntent) -> bool {
    match intent.action {
        ChartTradingIntentAction::PlaceBracketOrder => {
            intent.drawing_id.is_some()
                && intent.side.is_some()
                && intent.price.is_some()
                && intent.stop_loss_price.is_some()
                && intent.take_profit_price.is_some()
                && intent.quantity.is_some()
        }
        ChartTradingIntentAction::ModifyOrder => {
            intent.order_id.is_some() && intent.price.is_some()
        }
        ChartTradingIntentAction::CancelOrder => intent.order_id.is_some(),
        ChartTradingIntentAction::CreateStopLoss | ChartTradingIntentAction::CreateTakeProfit => {
            (intent.position_id.is_some() || intent.order_id.is_some())
                && intent.side.is_some()
                && intent.price.is_some()
                && intent.quantity.is_some()
        }
        ChartTradingIntentAction::ClosePosition => intent.position_id.is_some(),
    }
}

fn observe_chart(chart: Option<&Entity<AerisChartView>>, cx: &mut Context<WorkspaceSurface>) {
    if let Some(chart) = chart {
        cx.observe(chart, |app, chart, cx| {
            let user_state_revision = chart.read(cx).user_state_revision();
            if user_state_revision != app.last_chart_user_state_revision {
                app.last_chart_user_state_revision = user_state_revision;
                app.chart_persistence_dirty = true;
                cx.notify();
            }
            let clock_revision = chart.read(cx).clock_revision();
            if clock_revision != app.last_chart_clock_revision {
                app.last_chart_clock_revision = clock_revision;
                cx.notify();
            }
            let (
                activate,
                request,
                study_settings_request,
                study_remove_request,
                intents,
                sync_events,
            ) = chart.update(cx, |chart, _| {
                (
                    chart.take_activate_request(),
                    chart.take_context_menu_request(),
                    chart.take_study_settings_request(),
                    chart.take_study_remove_request(),
                    chart.take_trading_intents(),
                    chart.take_sync_events(),
                )
            });
            let had_sync_events = !sync_events.is_empty();
            for event in sync_events {
                if app.pending_chart_sync_events.len() == CHART_SYNC_EVENTS_PER_SURFACE {
                    app.pending_chart_sync_events.pop_front();
                }
                app.pending_chart_sync_events.push_back(event);
            }
            for intent in intents {
                dispatch_chart_trading_intent(chart.clone(), &intent, app, cx);
            }
            let alert_request = chart
                .update(cx, |chart, _| chart.take_alert_create_requests())
                .into_iter()
                .last();
            let had_alert_request = alert_request.is_some();
            if activate {
                app.pending_pane_activate = PaneActivationRequest::Pending;
            }
            let had_menu = request.is_some();
            if let Some(request) = request {
                app.pending_chart_context_menu = Some(request);
            }
            let had_study_settings_request = study_settings_request.is_some();
            if let Some(study_id) = study_settings_request.and_then(StudyInstanceId::try_from_u64) {
                app.pending_study_settings_request = Some(study_id);
            }
            let had_study_remove_request = study_remove_request.is_some();
            if let Some(study_id) = study_remove_request.and_then(StudyInstanceId::try_from_u64) {
                app.pending_study_remove_request = Some(study_id);
            }
            if let Some(request) = alert_request {
                app.open_price_alert_dialog(request);
            }
            if activate
                || had_menu
                || had_alert_request
                || had_study_settings_request
                || had_study_remove_request
                || had_sync_events
            {
                cx.notify();
            }
            if chart.read(cx).has_market_data() {
                app.retained_chart_presentation.indicators = chart.read(cx).indicator_states();
                app.retained_chart_presentation.price_precision =
                    chart.read(cx).selected_price_precision();
            }
            if provider_presentation(app.provider).is_some()
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
struct InstrumentMenuSelection(usize);

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
    let _ = provider;
    SymbolSubmitDecision::Search
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
    label: String,
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
    theme: AerisTheme,
    provider: TerminalProvider,
    symbol_provider: TerminalProvider,
    instrument_label: String,
    series_label: String,
    chart_type: ChartType,
    chart_type_label: String,
    instruments: Vec<InstrumentMenuEntry>,
    symbol_input: Option<Entity<InputState>>,
    indicator_input: Entity<InputState>,
    time_zone_id: String,
    time_zone_clock: String,
    account_label: Option<String>,
    indicator_message: Option<String>,
    series_message: String,
    pending: HeaderPendingState,
    drawing_history: DrawingHistoryState,
    controls: HeaderControls,
    side_panels: SidePanelVisibility,
    context_visible: bool,
    chart_link_group: u8,
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

/// Docked workspace side panels, listed in docking order from the chart outward.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SidePanel {
    OrderBook,
    TimeSales,
    Watchlist,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SidePanelVisibility(u8);

impl SidePanelVisibility {
    const PERSISTED: u8 = 0b111;

    fn from_persisted(bits: u32) -> Self {
        Self(u8::try_from(bits & u32::from(Self::PERSISTED)).unwrap_or_default())
    }

    const fn contains(self, panel: SidePanel) -> bool {
        self.0 & panel.persisted_bit() != 0
    }

    fn set(&mut self, panel: SidePanel, visible: bool) {
        if visible {
            self.0 |= panel.persisted_bit();
        } else {
            self.0 &= !panel.persisted_bit();
        }
    }

    const fn any(self) -> bool {
        self.0 != 0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SymbolSelectionTarget {
    #[default]
    Chart,
    Watchlist,
}

const fn instrument_target_after_close(
    target: SymbolSelectionTarget,
    selection_pending: bool,
) -> SymbolSelectionTarget {
    if selection_pending {
        target
    } else {
        SymbolSelectionTarget::Chart
    }
}

const fn symbol_menu_closes_after_selection(target: SymbolSelectionTarget) -> bool {
    matches!(target, SymbolSelectionTarget::Chart)
}

fn clamped_side_panel_width(width: f32) -> f32 {
    width.clamp(SIDE_PANEL_MINIMUM_WIDTH, SIDE_PANEL_MAXIMUM_WIDTH)
}

fn clamped_context_panel_height(height: f32) -> f32 {
    height.clamp(CONTEXT_PANEL_MINIMUM_HEIGHT, CONTEXT_PANEL_MAXIMUM_HEIGHT)
}

#[cfg(test)]
fn claim_once(claimed: &mut bool) -> bool {
    if *claimed {
        return false;
    }
    *claimed = true;
    true
}

impl SidePanel {
    const ALL: [Self; 3] = [Self::OrderBook, Self::TimeSales, Self::Watchlist];

    const fn title(self) -> &'static str {
        match self {
            Self::OrderBook => "Order Book",
            Self::TimeSales => "Time & Sales",
            Self::Watchlist => "Watchlist",
        }
    }

    /// Durable workspace bit. Values are stable across releases; Time & Sales keeps the
    /// bit it used while it was docked inside the order book.
    const fn persisted_bit(self) -> u8 {
        match self {
            Self::OrderBook => 1,
            Self::Watchlist => 2,
            Self::TimeSales => 4,
        }
    }

    /// Market panels need a selected instrument before they can open.
    const fn requires_market(self) -> bool {
        matches!(self, Self::OrderBook | Self::TimeSales)
    }
}

struct MarketSummaryEntry {
    instrument: InstallProviderInstrument,
    worker: Option<MarketDataWorker>,
    previous_close: Option<i64>,
    last: Option<MarketBar>,
    message: Option<String>,
    resource_class: ConsumerResourceClass,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct MarketSummaryKey {
    provider: String,
    instrument_id: String,
    entitlement_id: String,
}

impl MarketSummaryKey {
    fn from_instrument(instrument: &InstallProviderInstrument) -> Self {
        Self {
            provider: instrument.provider.clone(),
            instrument_id: instrument.instrument_id.clone(),
            entitlement_id: instrument.entitlement_id.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct MarketSummaryValues {
    last: Option<i64>,
    change: Option<i64>,
    change_percent: Option<f64>,
}

fn market_summary_values(
    last: Option<MarketBar>,
    previous_close: Option<i64>,
) -> MarketSummaryValues {
    let last = last.map(|bar| bar.close);
    let change = last
        .zip(previous_close)
        .map(|(last, previous)| last - previous);
    let change_percent = change.zip(previous_close).and_then(|(change, previous)| {
        (previous != 0)
            .then(|| change.to_f64().unwrap_or(0.0) / previous.to_f64().unwrap_or(1.0) * 100.0)
    });
    MarketSummaryValues {
        last,
        change,
        change_percent,
    }
}

pub(crate) fn market_price_text(value: i64, scale: u32) -> String {
    let scale = scale.min(18);
    if scale == 0 {
        return value.to_string();
    }

    let factor = 10_i128.pow(scale);
    let magnitude = i128::from(value).abs();
    let whole = magnitude / factor;
    let fraction = magnitude % factor;
    let minimum_precision = usize::try_from(scale.min(2)).unwrap_or(2);
    let mut fraction = format!(
        "{fraction:0width$}",
        width = usize::try_from(scale).unwrap_or(18)
    );
    while fraction.len() > minimum_precision && fraction.ends_with('0') {
        fraction.pop();
    }
    let sign = if value < 0 { "-" } else { "" };
    format!("{sign}{whole}.{fraction}")
}

fn market_summary_price(value: i64, scale: u32) -> String {
    market_price_text(value, scale)
}

fn market_summary_change(value: i64, scale: u32) -> String {
    let text = market_summary_price(value.abs(), scale);
    match value.cmp(&0) {
        std::cmp::Ordering::Greater => format!("+{text}"),
        std::cmp::Ordering::Less => format!("-{text}"),
        std::cmp::Ordering::Equal => text,
    }
}

#[derive(Clone)]
struct WatchlistRow {
    instrument: InstallProviderInstrument,
    previous_close: Option<i64>,
    last: Option<MarketBar>,
    message: Option<String>,
    active: bool,
}

impl MarketSummaryEntry {
    fn new(
        instrument: InstallProviderInstrument,
        worker: Option<MarketDataWorker>,
        message: Option<String>,
        resource_class: ConsumerResourceClass,
    ) -> Self {
        Self {
            instrument,
            worker,
            previous_close: None,
            last: None,
            message,
            resource_class,
        }
    }

    fn set_resource_class(&mut self, resource_class: ConsumerResourceClass) {
        if self.resource_class == resource_class {
            return;
        }
        let Some(worker) = &self.worker else {
            self.resource_class = resource_class;
            return;
        };
        if worker.try_set_market_resource_class(resource_class).is_ok() {
            self.resource_class = resource_class;
        }
    }

    fn apply_bar(&mut self, bar: MarketBar) {
        match self.last {
            Some(current)
                if bar.exchange_timestamp_unix_nanos < current.exchange_timestamp_unix_nanos => {}
            Some(current)
                if bar.exchange_timestamp_unix_nanos == current.exchange_timestamp_unix_nanos =>
            {
                self.last = Some(bar);
            }
            Some(current) => {
                self.previous_close = Some(current.close);
                self.last = Some(bar);
            }
            None => self.last = Some(bar),
        }
    }

    fn apply_update(&mut self, update: ReplayStreamUpdate) {
        match update {
            ReplayStreamUpdate::Snapshot(snapshot) => {
                let mut bars = snapshot.bars().iter().rev();
                self.last = bars.next().map(|item| *item.value());
                self.previous_close = bars.next().map(|item| item.value().close);
            }
            ReplayStreamUpdate::Delta(delta) => self.apply_bar(*delta.item().value()),
            ReplayStreamUpdate::Tail(tail) => self.apply_bar(*tail.item().value()),
        }
        self.message = None;
    }

    fn poll(&mut self) -> bool {
        let Some(worker) = &mut self.worker else {
            return false;
        };
        let (messages, disconnected) = worker.drain_messages_up_to(32);
        let changed = !messages.is_empty() || disconnected;
        for message in messages {
            match message {
                MarketWorkerMessage::Update(publication) => self.apply_update(publication.update),
                MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message,
                } => self.message = Some(message),
                _ => {}
            }
        }
        if disconnected {
            self.message = Some("Market data unavailable".to_string());
            self.worker = None;
        }
        changed
    }

    fn values(&self) -> MarketSummaryValues {
        market_summary_values(self.last, self.previous_close)
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
    if provider_presentation(app.provider).is_some() {
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
    /// Order book and Time & Sales, which need a selected market.
    const MARKET_PANELS: u8 = 4;
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
            controls |= Self::SERIES | Self::MARKET_PANELS;
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
    chart: Option<Entity<AerisChartView>>,
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
                chart: Some(cx.new(move |_| AerisChartView::empty())),
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
                connection_message: Some(provider_connection_message(provider)),
                provider,
                product: Some(startup.product),
            }
        }
    }
}

fn provider_connection_message(provider: TerminalProvider) -> String {
    let descriptor = provider_presentation(provider);
    let display_name = descriptor.map_or_else(
        || terminal_provider_id(provider),
        |value| value.display_name,
    );
    match descriptor.map(|value| value.connection_kind) {
        Some(ProviderConnectionKind::Credentials) => {
            format!("Connecting to authorized {display_name} markets")
        }
        Some(ProviderConnectionKind::HostedBroker) => {
            format!("Connecting to {display_name} hosted markets")
        }
        Some(ProviderConnectionKind::Public) | None => {
            format!("Connecting to {display_name} public markets")
        }
    }
}

fn initial_symbol_message(provider: TerminalProvider) -> String {
    provider_presentation(provider)
        .map_or("Search provider markets", |descriptor| {
            descriptor.search_hint
        })
        .to_string()
}

fn initialize_chart_chrome(
    chart: Option<&Entity<AerisChartView>>,
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
) -> String {
    let provider_name = terminal_provider_display(provider);
    match reason {
        ProviderCatalogRejectionReason::SearchRejected => {
            format!("{provider_name} rejected the market search")
        }
        ProviderCatalogRejectionReason::SupersededSearch => {
            "A newer symbol search replaced this one".to_string()
        }
        ProviderCatalogRejectionReason::InstrumentUnavailable => {
            format!("The selected {provider_name} asset is no longer available")
        }
        ProviderCatalogRejectionReason::SubscriptionRejected => {
            format!("{provider_name} rejected the market subscription")
        }
        ProviderCatalogRejectionReason::DispatchUnavailable => {
            let operation = match command {
                ProviderCatalogCommand::Search => "search",
                ProviderCatalogCommand::Selection => "selection",
            };
            format!("The {provider_name} {operation} could not be scheduled")
        }
        ProviderCatalogRejectionReason::SearchTimedOut => {
            format!("The {provider_name} market search timed out; try again")
        }
        ProviderCatalogRejectionReason::SelectionTimedOut => {
            format!("The {provider_name} market selection timed out; try again")
        }
        ProviderCatalogRejectionReason::Unspecified => {
            format!("The {provider_name} catalog request failed")
        }
    }
}

fn provider_catalog_event_provider(event: &ProviderCatalogEvent) -> &str {
    match event {
        ProviderCatalogEvent::SearchCompleted(result)
        | ProviderCatalogEvent::SearchPreview(result) => &result.provider,
        ProviderCatalogEvent::SelectionInstalled { instrument, .. }
        | ProviderCatalogEvent::StartupInstrumentResolved(instrument) => &instrument.provider,
        ProviderCatalogEvent::CommandRejected { rejection, .. } => &rejection.provider,
    }
}

fn usize_generation(generation: u64) -> Option<std::num::NonZeroUsize> {
    usize::try_from(generation)
        .ok()
        .and_then(std::num::NonZeroUsize::new)
}

const ACCOUNT_RESTORE_READINESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const ACCOUNT_RESTORE_READINESS_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(10);

fn wait_for_account_restore_readiness(
    timeout: std::time::Duration,
    mut readiness: impl FnMut() -> aeris_account_runtime::AccountRestoreReadiness,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match readiness() {
            aeris_account_runtime::AccountRestoreReadiness::Ready => return Ok(()),
            aeris_account_runtime::AccountRestoreReadiness::Failed => {
                return Err("candidate account restore failed local readiness".to_string());
            }
            aeris_account_runtime::AccountRestoreReadiness::Pending => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err("candidate account restore local readiness timed out".to_string());
        }
        std::thread::sleep(ACCOUNT_RESTORE_READINESS_POLL_INTERVAL);
    }
}

fn validate_workspace_boot_for_readiness(workspace: &WorkspaceState) -> Result<(), String> {
    engine_market_worker::restored_workspace_boot_plan(workspace)
        .map(|_| ())
        .map_err(|error| format!("candidate workspace bootstrap failed: {error}"))
}

#[cfg(test)]
mod desktop_readiness_account_tests {
    use super::wait_for_account_restore_readiness;
    use aeris_account_runtime::AccountRestoreReadiness;
    use std::{cell::Cell, time::Duration};

    #[test]
    fn account_restore_readiness_accepts_ready_without_network_or_ui() {
        assert!(
            wait_for_account_restore_readiness(Duration::ZERO, || {
                AccountRestoreReadiness::Ready
            })
            .is_ok()
        );
    }

    #[test]
    fn account_restore_readiness_fails_closed_on_failed_or_pending() {
        assert!(
            wait_for_account_restore_readiness(Duration::from_secs(1), || {
                AccountRestoreReadiness::Failed
            })
            .is_err()
        );
        assert!(
            wait_for_account_restore_readiness(Duration::ZERO, || {
                AccountRestoreReadiness::Pending
            })
            .is_err()
        );
    }

    #[test]
    fn account_restore_readiness_can_settle_after_bounded_pending_work() {
        let polls = Cell::new(0_u8);
        assert!(
            wait_for_account_restore_readiness(Duration::from_secs(1), || {
                let next = polls.get().saturating_add(1);
                polls.set(next);
                if next < 2 {
                    AccountRestoreReadiness::Pending
                } else {
                    AccountRestoreReadiness::Ready
                }
            })
            .is_ok()
        );
        assert_eq!(polls.get(), 2);
    }
}

#[cfg(test)]
mod desktop_readiness_workspace_tests {
    use super::validate_workspace_boot_for_readiness;
    use aeris_contracts::WorkspacePaneKind;

    #[test]
    fn readiness_reuses_the_production_workspace_boot_planner() {
        let valid = super::local_state::sanitize_workspace(super::local_state::default_workspace());
        assert!(validate_workspace_boot_for_readiness(&valid).is_ok());

        let mut parser_valid_but_unbootable = super::local_state::default_workspace();
        parser_valid_but_unbootable.workspace_tabs[0].panes[0].kind =
            WorkspacePaneKind::OrderBook as i32;
        let parser_valid_but_unbootable =
            super::local_state::sanitize_workspace(parser_valid_but_unbootable);
        assert!(!parser_valid_but_unbootable.workspace_tabs.is_empty());
        assert!(
            validate_workspace_boot_for_readiness(&parser_valid_but_unbootable).is_err(),
            "readiness must reject persistence that production startup cannot turn into chart workers"
        );
    }
}

fn run_desktop_readiness_command(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let usage = "usage: aeris_desktop --desktop-readiness <report-path>";
    let report_path = arguments.next().ok_or_else(|| usage.to_string())?;
    if arguments.next().is_some() {
        return Err(usage.to_string());
    }
    let workspace = local_state::load_workspace_for_readiness()
        .map_err(|error| format!("candidate workspace restore failed: {error}"))?;
    validate_workspace_boot_for_readiness(&workspace)?;
    let account_service = aeris_account_runtime::AccountService::new(
        aeris_account_runtime::AccountServiceConfig::from_environment(),
    );
    #[cfg(target_os = "windows")]
    let _session_shutdown_guard = {
        let session_account = account_service.clone();
        match native_account_session_shutdown_guard(move || session_account.begin_refresh_quiesce())
        {
            Ok(guard) => guard,
            Err(error) => {
                retain_account_refresh_quiesce_for_exit(
                    account_service.begin_refresh_quiesce(),
                    "candidate readiness exit",
                );
                return Err(error);
            }
        }
    };
    if aeris_desktop::account::AUTH_BACKEND_CONFIGURED {
        account_service.start_restore();
    }
    let result = (|| {
        if aeris_desktop::account::AUTH_BACKEND_CONFIGURED {
            wait_for_account_restore_readiness(ACCOUNT_RESTORE_READINESS_TIMEOUT, || {
                account_service.restore_readiness()
            })?;
        }
        let market = aeris_market_runtime::MarketService::start()?;
        let trading = start_trading_service()?;
        let context = start_context_service()?;
        let status = market.status()?;
        if status.providers.is_empty() {
            return Err("candidate market service did not reach readiness".to_string());
        }
        let release = aeris_platform_runtime::current_release_identity();
        let trading_status = trading.status()?;
        if trading_status.schema_version == 0 {
            return Err("candidate trading service did not reach readiness".to_string());
        }
        let report = LifecycleReadinessReport {
            schema_version: 4,
            release_identity: release.release_identity,
            install_generation: release.install_generation,
            desktop_process_id: std::process::id(),
            workspace_revision: workspace.workspace_revision,
            provider_count: status.providers.len(),
            workspace_restored: true,
            services: LifecycleServiceReadiness {
                market: ServiceReady(true),
                account: ServiceReady(true),
                trading: ServiceReady(true),
                context: ServiceReady(true),
            },
        };
        context.shutdown(std::time::Duration::from_secs(2))?;
        market.shutdown(std::time::Duration::from_secs(2))?;
        trading.shutdown(std::time::Duration::from_secs(2))?;
        let mut encoded = serde_json::to_vec(&report)
            .map_err(|_| "candidate readiness report could not be encoded".to_string())?;
        encoded.push(b'\n');
        std::fs::write(std::path::Path::new(&report_path), encoded)
            .map_err(|_| "candidate readiness report could not be written".to_string())
    })();
    retain_account_refresh_quiesce_for_exit(
        account_service.begin_refresh_quiesce(),
        "candidate readiness exit",
    );
    result
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
    services: LifecycleServiceReadiness,
}

#[derive(serde::Serialize)]
struct LifecycleServiceReadiness {
    market: ServiceReady,
    account: ServiceReady,
    trading: ServiceReady,
    context: ServiceReady,
}

#[derive(serde::Serialize)]
#[serde(transparent)]
struct ServiceReady(bool);

fn start_trading_service() -> Result<aeris_trading_runtime::TradingService, String> {
    let database_path = aeris_platform_runtime::native_data_root()
        .map_err(|error| format!("trading data root is unavailable: {error}"))?
        .join("trading")
        .join("trading.sqlite3");
    aeris_trading_runtime::TradingService::start(aeris_trading_runtime::TradingServiceConfig {
        database_path,
        retention: aeris_trading_runtime::TradingRetention::default(),
    })
}

fn start_context_service() -> Result<aeris_context_runtime::ContextService, String> {
    aeris_context_runtime::ContextService::start(
        aeris_context_runtime::ContextServiceConfig::default(),
    )
}

#[cfg(feature = "diagnostics")]
fn run_desktop_conformance_command(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let usage = "usage: aeris_desktop --desktop-conformance <report-path>";
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
    let usage = "usage: aeris_desktop --desktop-endurance <report-path> <duration-seconds>";
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
    _startup: &MarketWorkerStartup,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).placeholder("Search markets"))
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
            title: Some("Aeris Terminal".into()),
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
                    terminal.update(cx, |app, app_cx| {
                        if app.submit_symbol_input(app_cx)
                            && symbol_menu_closes_after_selection(app.symbol_selection_target)
                        {
                            app.close_chrome_overlay(window, app_cx);
                        }
                    });
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
                        app.scrolls.instrument.set_offset(point(px(0.0), px(0.0)));
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
                } else if matches!(event, InputEvent::Change) {
                    terminal.update(cx, |app, cx| {
                        app.chrome_selection = 0;
                        app.scrolls.indicator.set_offset(point(px(0.0), px(0.0)));
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
                        if app.chrome_overlay != Some(ChromeOverlay::QuickTimeframe)
                            || app.chrome_overlay_phase == ChromeOverlayPhase::Closing
                        {
                            return;
                        }
                        app.submit_quick_timeframe(window, app_cx);
                    });
                }
                InputEvent::Change => {
                    terminal.update(cx, |app, app_cx| {
                        if app.chrome_overlay != Some(ChromeOverlay::QuickTimeframe)
                            || app.chrome_overlay_phase == ChromeOverlayPhase::Closing
                        {
                            return;
                        }
                        let query = input.read(app_cx).value();
                        // Only a real edit retracts the rejection; Enter also reports a
                        // change while the rejected text is still in place.
                        if app
                            .menu_state
                            .quick_timeframe_error
                            .as_ref()
                            .is_some_and(|error| error.query != query.as_ref())
                        {
                            app.menu_state.quick_timeframe_error = None;
                        }
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

fn subscribe_time_zone_input(
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
                        if app.chrome_overlay != Some(ChromeOverlay::TimeZone)
                            || app.chrome_overlay_phase == ChromeOverlayPhase::Closing
                        {
                            return;
                        }
                        app.apply_highlighted_time_zone(window, app_cx);
                    });
                }
                InputEvent::Change => {
                    terminal.update(cx, |app, app_cx| {
                        if app.chrome_overlay != Some(ChromeOverlay::TimeZone)
                            || app.chrome_overlay_phase == ChromeOverlayPhase::Closing
                        {
                            return;
                        }
                        app.chrome_selection = 0;
                        app.scrolls.time_zone.set_offset(point(px(0.0), px(0.0)));
                        app_cx.notify();
                    });
                }
                InputEvent::Focus | InputEvent::Blur => {}
            },
        )
        .detach();
}

#[derive(Default)]
struct WorkspaceSurfaceRestore {
    chart: Option<WorkspaceChartState>,
    side_panel: Option<(u32, u32, u32)>,
}

/// Maps a ladder click to a resting order at that price.
///
/// Bid rows sit below the market and ask rows above it. Limit orders rest on the passive side
/// (buy below, sell above); stop orders trigger on a breakout (buy above, sell below). A market
/// selection places a resting limit, because a ladder click always names a price.
pub(crate) fn ladder_click_order(
    row_side: OrderBookLevelSide,
    selected_type: aeris_trading::OrderType,
) -> (aeris_trading::OrderSide, aeris_trading::OrderType) {
    let order_type = match selected_type {
        aeris_trading::OrderType::Market => aeris_trading::OrderType::Limit,
        order_type => order_type,
    };
    let breakout = matches!(
        order_type,
        aeris_trading::OrderType::Stop | aeris_trading::OrderType::StopLimit
    );
    let side = match (row_side, breakout) {
        (OrderBookLevelSide::Bid, false) | (OrderBookLevelSide::Ask, true) => {
            aeris_trading::OrderSide::Buy
        }
        (OrderBookLevelSide::Ask, false) | (OrderBookLevelSide::Bid, true) => {
            aeris_trading::OrderSide::Sell
        }
    };
    (side, order_type)
}

/// Returns the ladder row side and price where a working order is drawn and dragged.
pub(crate) fn order_book_row_price(
    order: &aeris_trading::Order,
) -> Option<(OrderBookLevelSide, aeris_trading::FixedPoint)> {
    let (price, breakout) = match order.order_type {
        aeris_trading::OrderType::Market => return None,
        aeris_trading::OrderType::Limit => (order.limit_price?, false),
        aeris_trading::OrderType::Stop | aeris_trading::OrderType::StopLimit => {
            (order.stop_price?, true)
        }
    };
    let side = match (order.side, breakout) {
        (aeris_trading::OrderSide::Buy, false) | (aeris_trading::OrderSide::Sell, true) => {
            OrderBookLevelSide::Bid
        }
        (aeris_trading::OrderSide::Sell, false) | (aeris_trading::OrderSide::Buy, true) => {
            OrderBookLevelSide::Ask
        }
    };
    Some((side, price))
}

fn subscribe_order_book_trading(
    workspace: &Entity<WorkspaceSurface>,
    order_book: &Entity<ReadOnlyOrderBookView>,
    window: &mut Window,
    cx: &mut App,
) {
    let click_workspace = workspace.clone();
    let click_order_book = order_book.clone();
    window
        .subscribe(
            order_book,
            cx,
            move |_, event: &OrderBookLevelClick, _, cx| {
                let order_entry = {
                    let surface = click_workspace.read(cx);
                    if selected_account_lock_reason(&surface.trading_pnl).is_some() {
                        return;
                    }
                    surface.trading_pnl.order_entry.clone()
                };
                let frame = click_order_book.read(cx).frame().cloned();
                let Some(frame) = frame else {
                    return;
                };
                let (side, order_type) =
                    ladder_click_order(event.side, aeris_trading::OrderType::Market);
                aeris_desktop::trading::dispatch_simulated_order_at_price(
                    &frame,
                    aeris_desktop::trading::SimulatedPricedOrder {
                        side,
                        order_type,
                        account_key: order_entry
                            .selected_account_id
                            .map(|id| id.as_str().to_string()),
                        quantity: order_entry.quantity,
                        time_in_force: aeris_trading::TimeInForce::Day,
                        price_units: event.price,
                    },
                    cx,
                );
            },
        )
        .detach();
    let drop_workspace = workspace.clone();
    let drop_order_book = order_book.clone();
    window
        .subscribe(
            order_book,
            cx,
            move |_, event: &OrderBookLevelDrop, _, cx| {
                if event.source_side != event.target_side {
                    return;
                }
                let (account_id, orders) = {
                    let surface = drop_workspace.read(cx);
                    if selected_account_lock_reason(&surface.trading_pnl).is_some() {
                        return;
                    }
                    let Some(account_id) =
                        surface.trading_pnl.order_entry.selected_account_id.clone()
                    else {
                        return;
                    };
                    (account_id, surface.trading_pnl.orders.clone())
                };
                let Some(frame) = drop_order_book.read(cx).frame().cloned() else {
                    return;
                };
                let Some(order) = orders.into_iter().find(|order| {
                    order.status.is_open()
                        && order.account_id.as_str() == account_id.as_str()
                        && order.instrument_id.as_str() == frame.instrument_id.as_str()
                        && order_book_row_price(order).is_some_and(|(side, price)| {
                            side == event.source_side && price.units() == event.source_price
                        })
                }) else {
                    return;
                };
                let Ok(target_price) =
                    aeris_trading::FixedPoint::try_new(event.target_price, frame.price_scale)
                else {
                    return;
                };
                aeris_desktop::trading::modify_simulated_order_at_price(
                    order.client_order_id,
                    order.order_type,
                    order.time_in_force,
                    &frame,
                    target_price,
                    cx,
                );
            },
        )
        .detach();
}

fn workspace_surface_entity(
    bootstrap: MarketWorkerStartup,
    market_worker: MarketDataWorker,
    lifecycle: &DesktopLifecycle,
    chart_chrome: chart_chrome::ChartChromePreferences,
    restored: WorkspaceSurfaceRestore,
    window: &mut Window,
    cx: &mut App,
) -> Entity<WorkspaceSurface> {
    let symbol_input = Some(symbol_input_for_startup(&bootstrap, window, cx));
    let search_input = symbol_input.clone();
    let indicator_input =
        cx.new(|cx| InputState::new(window, cx).placeholder("Search native indicators"));
    let indicator_search_input = indicator_input.clone();
    let timeframe_input = cx.new(|cx| InputState::new(window, cx).placeholder("1m, 5, 1H, 1D"));
    timeframe_input.update(cx, |input, cx| {
        input.set_text_align(gpui::TextAlign::Center, cx);
    });
    let timeframe_search_input = timeframe_input.clone();
    let time_zone_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search time zones"));
    let time_zone_search_input = time_zone_input.clone();
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
            time_zone_input,
            chart_chrome,
            restored.chart,
        )
    });
    let order_book = workspace.read(cx).order_book.clone();
    subscribe_order_book_trading(&workspace, &order_book, window, cx);
    if let Some((visibility, width, split)) = restored.side_panel {
        workspace.update(cx, |surface, surface_cx| {
            surface.side_panel_width = width
                .to_f32()
                .filter(|width| *width > 0.0)
                .unwrap_or(SIDE_PANEL_INITIAL_WIDTH)
                .clamp(SIDE_PANEL_MINIMUM_WIDTH, SIDE_PANEL_MAXIMUM_WIDTH);
            surface.side_panel_split_basis_points = if split == 0 {
                5_000
            } else {
                split.clamp(500, 9_500)
            };
            surface.apply_side_panels(SidePanelVisibility::from_persisted(visibility), surface_cx);
            surface.chart_persistence_dirty = false;
        });
    }
    lifecycle.register_terminal(&workspace);
    subscribe_symbol_input(search_input, &workspace, window, cx);
    subscribe_indicator_input(&indicator_search_input, &workspace, window, cx);
    subscribe_timeframe_input(&timeframe_search_input, &workspace, window, cx);
    subscribe_time_zone_input(&time_zone_search_input, &workspace, window, cx);
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
    layout: AerisChartWorkspace,
    /// Ephemeral host presentation state matching the Aeris Charts grid contract. This is not
    /// persisted because maximizing a pane must not replace or mutate the authoritative layout.
    maximized_pane: Option<u64>,
    /// An Alt+click changes the layout below the pointer. Consume its matching release so the
    /// newly mounted chart cannot receive a stray click or drawing anchor.
    swallow_pane_mouse_up: bool,
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

#[derive(Clone, Debug, PartialEq)]
struct WatchlistDragState {
    provider: String,
    instrument_id: String,
    cursor_offset_y: f32,
    pointer_y: Option<f32>,
    body_top: f32,
    scroll_offset_y: f32,
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
    copy_feedback_generation: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ChartSettingsSection {
    #[default]
    Series,
    Canvas,
    Trading,
}

impl ChartSettingsSection {
    const ALL: [Self; 3] = [Self::Series, Self::Canvas, Self::Trading];

    const fn label(self) -> &'static str {
        match self {
            Self::Series => "Series",
            Self::Canvas => "Canvas",
            Self::Trading => "Trading",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum ChartColorSetting {
    Up,
    Down,
    WickUp,
    WickDown,
    BorderUp,
    BorderDown,
    Line,
    AreaTop,
    BaselineTop,
    BaselineBottom,
    Grid,
    Crosshair,
}

struct ChartColorPickerState {
    setting: ChartColorSetting,
    input: Entity<InputState>,
    error: Option<String>,
}

impl ChartColorSetting {
    const fn label(self) -> &'static str {
        match self {
            Self::Up => "Up candles",
            Self::Down => "Down candles",
            Self::WickUp => "Up wicks",
            Self::WickDown => "Down wicks",
            Self::BorderUp => "Up borders",
            Self::BorderDown => "Down borders",
            Self::Line => "Line",
            Self::AreaTop => "Area top",
            Self::BaselineTop => "Above baseline",
            Self::BaselineBottom => "Below baseline",
            Self::Grid => "Grid",
            Self::Crosshair => "Crosshair",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChartSettingsAction {
    ToggleGrid,
    GridStyle(u8),
    CrosshairMode(u8),
    CrosshairWidth(u8),
    CrosshairStyle(u8),
    ToggleWicks,
    ToggleBorders,
    ToggleOpen,
    ToggleThinBars,
    LineWidth(u8),
    LineStyle(u8),
    FootprintMode(FootprintDisplayMode),
    ToggleCumulativeDelta,
    ToggleDeltaHistogram,
    ToggleTradeBubbles,
    TradeBubbleMinimumVolumeBits(u64),
    /// Instrument ticks per footprint row; zero is automatic.
    FootprintTicksPerRow(u32),
    ToggleOrderManagementLines,
    ToggleExecutionMarks,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChartTradingVisibilitySettings {
    show_order_management_lines: bool,
    show_execution_marks: bool,
}

impl Default for ChartTradingVisibilitySettings {
    fn default() -> Self {
        Self {
            show_order_management_lines: true,
            show_execution_marks: true,
        }
    }
}

#[derive(Clone, Debug)]
struct ChartSettingsSnapshot {
    chart_type: ChartType,
    appearance: ChartAppearanceSettings,
    crosshair_mode: u8,
    order_flow: OrderFlowSettings,
    time_zone: String,
    trading_visibility: ChartTradingVisibilitySettings,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ChartSettingsTemplateOverlay {
    #[default]
    Closed,
    Menu,
    SaveDialog,
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
    theme: AerisTheme,
    drawing_toolbar: DrawingToolbarVisibility,
    window_active: bool,
    frame_poll_gate: frame_poll_gate::FramePollGate,
    market_frame_wake: UiWake,
    market_wake_listener_started: Option<()>,
    chrome_focus: FocusHandle,
    lifecycle: DesktopLifecycle,
    workspace_factory: Option<engine_market_worker::WorkspaceMarketFactory>,
    watchlist: Vec<InstallProviderInstrument>,
    market_summaries: BTreeMap<MarketSummaryKey, MarketSummaryEntry>,
    persisted_watchlist: Vec<WorkspaceWatchlistEntryState>,
    watchlist_persistence_dirty: bool,
    workspace_persistence: Option<WorkspaceLayoutPersistence>,
    persisted_layout: Vec<WorkspaceTabState>,
    persisted_active_workspace_id: u64,
    workspace_error: Option<String>,
    workspace_drag: Option<WorkspaceDragState>,
    watchlist_drag: Option<WatchlistDragState>,
    watchlist_scroll: ScrollHandle,
    chart_context_menu: Option<ChartContextMenu>,
    chart_context_copy_feedback_generation: u64,
    chart_settings_menu: Option<ChartContextMenu>,
    chart_settings_placement: chart_context_menus::ChartSettingsPlacement,
    chart_settings_section: ChartSettingsSection,
    chart_settings_color_picker: Option<ChartColorPickerState>,
    chart_settings_template_overlay: ChartSettingsTemplateOverlay,
    chart_settings_template_name: Option<Entity<InputState>>,
    chart_settings_template_error: Option<String>,
    chart_settings_templates: Vec<WorkspaceChartSettingsTemplateState>,
    chart_settings_persistence_dirty: bool,
    account_menu_open: bool,
    account_menu_anchor: Option<gpui::Point<Pixels>>,
    bottom_panel: bottom_panel::BottomPanelState,
    profile_refresh_on_activation: bool,
    about_dialog_open: bool,
    command_palette_input: Entity<InputState>,
    command_palette_open: bool,
    command_palette_selection: usize,
    command_palette_message: Option<String>,
    broker_connection_task: Option<gpui::Task<()>>,
    tastytrade_connection: TastytradeConnectionView,
    linked_sync_revisions: BTreeMap<String, u64>,
    event_risk_dispatches: BTreeMap<String, i64>,
    next_event_risk_check: Instant,
    updater: Option<DesktopUpdater>,
    update_restart_persistence_pending: bool,
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
            let pane_weights = layout.basis_points();
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
                            chart: surface.workspace_chart_state(cx),
                            side_panel_visibility: u32::from(surface.side_panels.0),
                            side_panel_width: surface
                                .side_panel_width
                                .round()
                                .to_u32()
                                .unwrap_or(400),
                            side_panel_split_basis_points: surface.side_panel_split_basis_points,
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
        ChartWorkspaceLayout::Cell { .. } => WorkspaceSplitAxis::Horizontal,
    }
}

fn workspace_layout_state(layout: &ChartWorkspaceLayout) -> WorkspaceLayoutState {
    match layout {
        ChartWorkspaceLayout::Cell { id } => WorkspaceLayoutState {
            pane_id: *id,
            split_axis: WorkspaceSplitAxis::Horizontal as i32,
            ratio_basis_points: 0,
            first: None,
            second: None,
        },
        ChartWorkspaceLayout::Split {
            direction,
            ratio,
            a,
            b,
        } => WorkspaceLayoutState {
            pane_id: 0,
            split_axis: split_axis(*direction) as i32,
            ratio_basis_points: (*ratio * 10_000.0)
                .round()
                .clamp(500.0, 9_500.0)
                .to_u32()
                .unwrap_or(5_000),
            first: Some(Box::new(workspace_layout_state(a))),
            second: Some(Box::new(workspace_layout_state(b))),
        },
    }
}

fn chart_workspace_layout(layout: &WorkspaceLayoutState) -> Option<ChartWorkspaceLayout> {
    match (&layout.first, &layout.second) {
        (None, None) if layout.pane_id != 0 => {
            Some(ChartWorkspaceLayout::Cell { id: layout.pane_id })
        }
        (Some(first), Some(second)) if layout.pane_id == 0 => Some(ChartWorkspaceLayout::Split {
            direction: chart_split_direction(WorkspaceSplitAxis::try_from(layout.split_axis).ok()?),
            ratio: f64::from(layout.ratio_basis_points) / 10_000.0,
            a: Box::new(chart_workspace_layout(first)?),
            b: Box::new(chart_workspace_layout(second)?),
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
    tab_widths: &[f32],
) -> Option<usize> {
    let last = tab_widths.len().checked_sub(1)?;
    let dragged_left = pointer_x - cursor_offset_x - strip_left - WORKSPACE_TAB_STRIP_PADDING_LEFT;
    if !dragged_left.is_finite() {
        return None;
    }
    let mut destination = 0;
    let mut boundary = f32::midpoint(tab_widths[0], WORKSPACE_TAB_GAP);
    while destination < last && dragged_left >= boundary {
        destination += 1;
        boundary +=
            f32::midpoint(tab_widths[destination - 1], tab_widths[destination]) + WORKSPACE_TAB_GAP;
    }
    Some(destination)
}

fn workspace_drag_translation(
    drag: Option<WorkspaceDragState>,
    tab_id: u64,
    index: usize,
    tab_widths: &[f32],
) -> Option<f32> {
    let drag = drag.filter(|drag| drag.tab_id == tab_id)?;
    let pointer_x = drag.pointer_x?;
    let dragged_width = *tab_widths.get(index)?;
    let index_offset = tab_widths
        .get(..index)?
        .iter()
        .fold(0.0, |offset, width| offset + width + WORKSPACE_TAB_GAP);
    let occupied_width = tab_widths
        .iter()
        .copied()
        .reduce(|total, width| total + WORKSPACE_TAB_GAP + width)?;
    let requested_left =
        pointer_x - drag.cursor_offset_x - drag.strip_left - WORKSPACE_TAB_STRIP_PADDING_LEFT;
    if !requested_left.is_finite() || !occupied_width.is_finite() {
        return None;
    }
    let bounded_left = requested_left.clamp(0.0, (occupied_width - dragged_width).max(0.0));
    Some(bounded_left - index_offset)
}

fn watchlist_drag_destination(
    pointer_y: f32,
    body_top: f32,
    scroll_offset_y: f32,
    cursor_offset_y: f32,
    row_count: usize,
) -> Option<usize> {
    let last = row_count.checked_sub(1)?;
    let dragged_top = pointer_y - cursor_offset_y - body_top - scroll_offset_y;
    if !dragged_top.is_finite() {
        return None;
    }
    let mut destination = 0;
    let mut boundary = WATCHLIST_ROW_HEIGHT / 2.0;
    while destination < last && dragged_top >= boundary {
        destination += 1;
        boundary += WATCHLIST_ROW_HEIGHT;
    }
    Some(destination)
}

fn watchlist_drag_translation(
    drag: Option<&WatchlistDragState>,
    provider: &str,
    instrument_id: &str,
    index: usize,
) -> Option<f32> {
    let drag =
        drag.filter(|drag| drag.provider == provider && drag.instrument_id == instrument_id)?;
    let pointer_y = drag.pointer_y?;
    let index = index.to_f32()?;
    let slot_top = drag.body_top + drag.scroll_offset_y + index * WATCHLIST_ROW_HEIGHT;
    Some(pointer_y - drag.cursor_offset_y - slot_top)
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
            run_desktop_conformance_command(arguments)?;
            return Ok(None);
        }
        #[cfg(feature = "diagnostics")]
        if argument == "--desktop-endurance" {
            run_desktop_endurance_command(arguments)?;
            return Ok(None);
        }
        if argument == "--rithmic-test" {
            if arguments.next().is_some() {
                eprintln!("usage: aeris_desktop --rithmic-test");
                exit_after_account_refresh_quiesce(2);
            }
            let lifecycle = configure_desktop_state();
            (
                vec![engine_market_worker::start_rithmic_catalog()?],
                Vec::new(),
                lifecycle,
            )
        } else if argument == "--multi-chart" {
            if arguments.next().is_some() {
                eprintln!("usage: aeris_desktop --multi-chart");
                exit_after_account_refresh_quiesce(2);
            }
            let lifecycle = configure_desktop_state();
            (
                engine_market_worker::start_multi_chart()?,
                Vec::new(),
                lifecycle,
            )
        } else if argument == "--workspace-tabs" {
            if arguments.next().is_some() {
                eprintln!("usage: aeris_desktop --workspace-tabs");
                exit_after_account_refresh_quiesce(2);
            }
            let lifecycle = configure_desktop_state();
            layout = DesktopLayout::WorkspaceTabs;
            let group = engine_market_worker::start_workspace_tabs(&lifecycle.workspace)?;
            workspace_factory = Some(group.factory);
            (Vec::new(), group.initial, lifecycle)
        } else {
            eprintln!("unsupported argument: {}", argument.to_string_lossy());
            exit_after_account_refresh_quiesce(2);
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
            eprintln!("Aeris desktop readiness failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = aeris_platform_runtime::migrate_legacy_native_data_root() {
        eprintln!("Aeris legacy local state migration deferred: {error}");
    }
    if std::env::args_os().len() == 1
        && let Err(error) = schedule_versioned_launcher_promotion()
    {
        eprintln!("Aeris launcher promotion deferred: {error}");
    }
    let trading = match start_trading_service() {
        Ok(trading) => trading,
        Err(error) => {
            eprintln!("Aeris trading owner could not start: {error}");
            exit_after_account_refresh_quiesce(1);
        }
    };
    let context = match start_context_service() {
        Ok(context) => context,
        Err(error) => {
            eprintln!("Aeris context owner could not start: {error}");
            let _ = trading.shutdown(Duration::from_secs(2));
            exit_after_account_refresh_quiesce(1);
        }
    };
    if let Err(error) = aeris_desktop::trading::install(trading.clone()) {
        eprintln!("Aeris trading owner could not be installed: {error}");
        let _ = trading.shutdown(Duration::from_secs(2));
        exit_after_account_refresh_quiesce(1);
    }
    let configured = match configured_market_workers() {
        Ok(Some(configured)) => configured,
        Ok(None) => {
            let _ = context.shutdown(Duration::from_secs(2));
            let _ = trading.shutdown(Duration::from_secs(2));
            exit_after_account_refresh_quiesce(0);
        }
        Err(error) => {
            eprintln!("Aeris market worker could not start: {error}");
            let _ = context.shutdown(Duration::from_secs(2));
            let _ = trading.shutdown(Duration::from_secs(2));
            exit_after_account_refresh_quiesce(1);
        }
    };
    let lifecycle = DesktopLifecycle::new(trading, context);
    run_desktop(configured, lifecycle);
}

fn schedule_versioned_launcher_promotion() -> Result<(), String> {
    let executable = std::env::current_exe()
        .map_err(|_| "desktop executable path is unavailable".to_string())?;
    let release_root = executable
        .parent()
        .ok_or_else(|| "desktop release directory is unavailable".to_string())?;
    let launcher = release_root.join(format!("aeris_launcher{}", std::env::consts::EXE_SUFFIX));
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

fn run_desktop(configured: ConfiguredDesktop, lifecycle: DesktopLifecycle) {
    application()
        .with_assets(assets::AerisAssets)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            gpui_base::init(cx);
            cx.set_global(base_theme(&AerisTheme::dark()));
            install_platform_http_client(cx);
            cx.set_app_identity("com.aeris.desktop", "Aeris Terminal");
            cx.text_system()
                .add_fonts(
                    PLATFORM_FONT_BYTES
                        .iter()
                        .map(|font| Cow::Borrowed(*font))
                        .chain(std::iter::once(Cow::Borrowed(BRAND_FONT_BYTES)))
                        .collect(),
                )
                .expect("the bundled platform font is valid");
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
    if let Err(error) = validate_trading_keymap() {
        eprintln!("Aeris trading keymap is invalid: {error}");
        return None;
    }
    bind_desktop_keys(cx);
    let quit_lifecycle = lifecycle.clone();
    cx.on_app_quit(move |cx| {
        let quit = quit_lifecycle.begin_quit(cx);
        async move {
            if let Some(quit) = quit
                && let Err(error) = quit.await
            {
                eprintln!("Aeris desktop shutdown failed: {error}");
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
                        .expect("the Aeris terminal window opens");
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
                    .expect("the Aeris workspace window opens");
            }
        }
    }
    cx.activate(true);
    existing_root
}

fn validate_trading_keymap() -> Result<(), String> {
    let keymap = aeris_desktop::keymap::KeymapOwner::defaults()?;
    keymap.validate_against_reserved(&[
        "f11",
        "alt-enter",
        "alt-f9",
        "alt-f10",
        "alt-f4",
        "ctrl-t",
        "ctrl-tab",
        "ctrl-shift-tab",
        "ctrl-shift-pageup",
        "ctrl-shift-pagedown",
        "ctrl-w",
        "ctrl-alt-h",
        "ctrl-alt-v",
        "ctrl-shift-w",
    ])
}

fn bind_desktop_keys(cx: &mut App) {
    use aeris_desktop::command_registry::{CommandId, command};
    cx.bind_keys([
        KeyBinding::new("f11", ToggleFullscreen, None),
        KeyBinding::new("alt-enter", ToggleFullscreen, None),
        KeyBinding::new("alt-f9", MinimizeWindow, None),
        KeyBinding::new("alt-f10", ZoomWindow, None),
        KeyBinding::new("alt-f4", CloseWindow, None),
        KeyBinding::new(
            command(CommandId::NewWorkspace).chord.unwrap_or("ctrl-t"),
            NewWorkspace,
            None,
        ),
        KeyBinding::new("ctrl-tab", SelectNextWorkspace, None),
        KeyBinding::new("ctrl-shift-tab", SelectPreviousWorkspace, None),
        KeyBinding::new("ctrl-shift-pageup", MoveWorkspaceLeft, None),
        KeyBinding::new("ctrl-shift-pagedown", MoveWorkspaceRight, None),
        KeyBinding::new("ctrl-w", CloseWorkspace, None),
        KeyBinding::new(
            command(CommandId::SplitHorizontal)
                .chord
                .unwrap_or("ctrl-alt-h"),
            SplitPaneHorizontal,
            None,
        ),
        KeyBinding::new(
            command(CommandId::SplitVertical)
                .chord
                .unwrap_or("ctrl-alt-v"),
            SplitPaneVertical,
            None,
        ),
        KeyBinding::new("ctrl-shift-w", ClosePane, None),
        KeyBinding::new(
            command(CommandId::BuyMarket).chord.unwrap_or("ctrl-b"),
            TradingBuyMarket,
            None,
        ),
        KeyBinding::new(
            command(CommandId::SellMarket).chord.unwrap_or("ctrl-s"),
            TradingSellMarket,
            None,
        ),
        KeyBinding::new(
            command(CommandId::CancelAll)
                .chord
                .unwrap_or("ctrl-shift-x"),
            TradingCancelAll,
            None,
        ),
        KeyBinding::new(
            command(CommandId::FlattenAccount)
                .chord
                .unwrap_or("ctrl-shift-f"),
            TradingFlattenAccount,
            None,
        ),
        KeyBinding::new(
            command(CommandId::KillSwitch)
                .chord
                .unwrap_or("ctrl-shift-k"),
            TradingKillSwitch,
            None,
        ),
        KeyBinding::new(
            command(CommandId::OpenPalette).chord.unwrap_or("ctrl-k"),
            OpenCommandPalette,
            None,
        ),
    ]);
}
#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "http_wiring_tests.rs"]
mod http_wiring_tests;
