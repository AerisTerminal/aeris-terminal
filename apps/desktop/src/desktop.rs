//! Desktop composition and shared presentation types. Behavior lives in owned child modules.

#[path = "components/accounts_panel.rs"]
mod accounts_panel;
#[path = "desktop/alerts_controller.rs"]
mod alerts_controller;
use alerts_controller::{PriceAlertAnnouncement, PriceAlertDialogState, PriceAlertDropdown};
#[path = "components/app_navigation.rs"]
mod app_navigation;
#[path = "assets.rs"]
mod assets;
#[path = "components/big_trades_dialog.rs"]
mod big_trades_dialog;
#[path = "components/bottom_panel.rs"]
mod bottom_panel;
#[path = "components/chart_context_menus.rs"]
mod chart_context_menus;
#[path = "desktop/chart_pane_controller.rs"]
mod chart_pane_controller;
#[path = "desktop/context_panel_controller.rs"]
mod context_panel_controller;
use context_panel_controller::{ContextCredentialDialogState, ContextPanelTab};
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
#[path = "components/drawing_context_toolbar.rs"]
mod drawing_context_toolbar;
#[path = "components/drawing_settings_dialog.rs"]
mod drawing_settings_dialog;
#[path = "components/drawing_toolbar.rs"]
mod drawing_toolbar;
#[path = "engine_market_worker.rs"]
mod engine_market_worker;
#[path = "frame_poll_gate.rs"]
mod frame_poll_gate;
#[path = "components/frameless_title_bar.rs"]
mod frameless_title_bar;
#[path = "components/fullscreen_hint.rs"]
mod fullscreen_hint;
#[path = "components/indicator_menu.rs"]
mod indicator_menu;
#[path = "components/keyboard_shortcuts.rs"]
mod keyboard_shortcuts;
#[path = "components/layouts_view.rs"]
mod layouts_view;
#[path = "desktop/local_state.rs"]
mod local_state;
#[path = "desktop/studies_controller.rs"]
mod studies_controller;
#[cfg(test)]
pub(super) fn default_workspace_for_test() -> WorkspaceState {
    local_state::default_workspace()
}
#[path = "desktop/market_screener.rs"]
mod market_screener;
#[path = "components/market_screener_view.rs"]
mod market_screener_view;
#[path = "native_ui/mod.rs"]
mod native_ui;
#[path = "desktop/notification_center.rs"]
mod notification_center;
#[path = "components/order_book_panel.rs"]
mod order_book_panel;
#[path = "components/order_entry_panel.rs"]
mod order_entry_panel;
#[path = "components/order_flow_dialog.rs"]
mod order_flow_dialog;
#[path = "desktop/order_flow_presenter.rs"]
mod order_flow_presenter;
#[path = "components/order_quantity.rs"]
mod order_quantity;
#[path = "components/order_ticket.rs"]
mod order_ticket;
#[path = "desktop/pane_chrome_state.rs"]
mod pane_chrome_state;
#[path = "desktop/pane_persistence.rs"]
mod pane_persistence;
use pane_chrome_state::{ChromeOverlay, ChromeOverlayPhase, ChromeTrigger};
#[path = "components/platform_menu.rs"]
mod platform_menu;
#[path = "components/price_alert_dialog.rs"]
mod price_alert_dialog;
#[path = "components/provider_login.rs"]
mod provider_login;
#[cfg(any(test, feature = "diagnostics"))]
#[path = "readiness_conformance.rs"]
mod readiness_conformance;
#[path = "components/side_panel_dock.rs"]
mod side_panel_dock;
#[path = "components/sign_in_gate.rs"]
mod sign_in_gate;
#[path = "study_packages.rs"]
mod study_packages;
#[path = "components/study_settings_dialog.rs"]
mod study_settings_dialog;
#[path = "symbol_browser.rs"]
mod symbol_browser;
#[path = "desktop/symbol_entry.rs"]
mod symbol_entry;
#[path = "components/symbol_menu.rs"]
mod symbol_menu;
#[path = "desktop/tab_strip.rs"]
mod tab_strip;
use tab_strip::{TabClose, TabDragMove, TabStrip, WorkspaceDragState};
#[path = "components/terminal_chrome.rs"]
mod terminal_chrome;
#[path = "components/terminal_view.rs"]
mod terminal_view;
#[path = "components/time_sales_panel.rs"]
mod time_sales_panel;
#[path = "components/toaster.rs"]
mod toaster;
#[path = "components/trading_hotkeys.rs"]
mod trading_hotkeys;
#[path = "update.rs"]
mod update;
#[path = "components/volume_dialog.rs"]
mod volume_dialog;
#[path = "components/watchlist_panel.rs"]
mod watchlist_panel;
#[path = "components/workspace_layout.rs"]
mod workspace_layout;

use aeris_application::ReplayStreamUpdate;
use aeris_chart_integration::{
    AerisChartTheme, AerisChartView, AerisChartWorkspace, BigTradesFilter, BigTradesIntensity,
    BigTradesSettings, BigTradesSize, BuiltinIndicator, ChartAggressorSide, ChartAlertCondition,
    ChartAlertCreateRequest, ChartAlertFrequency, ChartAlertId, ChartAlertLine,
    ChartAlertLineStatus, ChartAlertPriceScale, ChartAlertSnapshot, ChartAppearanceColor,
    ChartAppearanceSettings, ChartAxisAppearance, ChartBridgeMetrics, ChartColor, ChartContextKind,
    ChartContextRequest, ChartDepthLevel, ChartDrawingId, ChartDrawingKind, ChartDrawingStamp,
    ChartExecutionId, ChartExecutionKind, ChartExecutionMarkerShape, ChartHostEvent,
    ChartHostEventMarker, ChartHostOverlaySnapshot, ChartHostTimeWindow, ChartIndicator,
    ChartIndicatorState, ChartInstrumentMetadata, ChartLegendOptions, ChartOrderId, ChartOrderKind,
    ChartOrderRole, ChartOrderSide, ChartOrderStatus, ChartPaneContent, ChartPaneHeight,
    ChartPositionId, ChartPositionSide, ChartSeriesColors, ChartSettingsRequest,
    ChartSplitDirection, ChartStudyInputRequirements, ChartStudyInputStream,
    ChartStudyOutputDescriptor, ChartStudyPaneTarget, ChartStudyPlotKind, ChartStudyPointStyle,
    ChartStudyScaleTarget, ChartStudyThresholdRegion, ChartThemeColors, ChartThemeDefaults,
    ChartTradingAnnotation, ChartTradingAnnotationPlacement, ChartTradingAnnotationTone,
    ChartTradingExecution, ChartTradingGroupId, ChartTradingIntent, ChartTradingIntentAction,
    ChartTradingPosition, ChartTradingPriceScale, ChartTradingSnapshot, ChartType,
    ChartWorkingOrder, ChartWorkspaceLayout, CumulativeDeltaReset, CumulativeDeltaSettings,
    CumulativeDeltaStyle, DEFAULT_STUDY_LINE_WIDTH, DeltaHistogramSettings, DepthHeatmapSettings,
    FootprintDisplayMode, MAXIMUM_STUDY_LINE_WIDTH, OrderBookDepthSettings, OrderFlowAggregation,
    OrderFlowDefaultColors, OrderFlowSettings, OrderFlowSweep, OrderFlowTrade, PriceAxisMenuAction,
    PriceAxisMenuState, VolumeAverageKind, VolumeAverageSettings, VolumePlacement, VolumeSettings,
    chart_color_hex, classify_order_flow_sweeps,
};
use aeris_context_runtime::{ContextSnapshot, ContextSource};
use aeris_contracts::{
    InstallProviderInstrument, PriceAlertCondition, PriceAlertFrequency, PriceAlertStatus,
    ProviderCatalogRejected, ProviderCatalogRejectionReason, ProviderInstrumentSearchResult,
    SearchProviderInstruments, SelectProviderInstrument, SeriesKey, WorkspaceBigTradesState,
    WorkspaceChartAppearanceState, WorkspaceChartIndicatorState, WorkspaceChartLegendState,
    WorkspaceChartPaneHeightState, WorkspaceChartSettingsTemplateState, WorkspaceChartState,
    WorkspaceChartStudyState, WorkspaceCumulativeDeltaState, WorkspaceDeltaHistogramState,
    WorkspaceDrawingTemplateState, WorkspaceLayoutState, WorkspaceOrderBookDepthState,
    WorkspaceOrderFlowSettingsState, WorkspacePaneKind, WorkspacePaneState,
    WorkspacePriceAlertState, WorkspacePriceAxisState, WorkspaceSavedLayoutState,
    WorkspaceSplitAxis, WorkspaceState, WorkspaceStudyDecimalState, WorkspaceStudyDependencyKind,
    WorkspaceStudyDependencyState, WorkspaceStudyMarketStream, WorkspaceStudySettingState,
    WorkspaceTabState, WorkspaceVolumeSettingsState, WorkspaceWatchlistEntryState,
    workspace_study_setting_state,
};
use aeris_design_system::{
    AerisTheme, BRAND_FONT_BYTES, PLATFORM_FONT_BYTES, RadiusToken, ThemeColor, TypographyRole,
};
use aeris_desktop::chart_chrome;
use aeris_desktop::market_worker::{
    MarketDataWorker, MarketPublicationGeneration, MarketRecoveryFailure, MarketWorkerBootstrap,
    MarketWorkerPublication, MarketWorkerRetirement, MarketWorkerStartup, OrderBookDetail,
    PaneProjectionEvent, PendingUiDiagnostics, UiDiagnosticsFeedback, verified_terms_supersede,
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
use aeris_observability::diagnostic;
use aeris_terminal_ui::{
    OrderBookColumn, OrderBookColumnVisibility, OrderBookLevelClick, OrderBookLevelDrop,
    OrderBookLevelSide, ReadOnlyOrderBookView,
};
use assets::UiIcon as HugeIcon;
use big_trades_dialog::big_trades_dialog_layer;
use catalog_projection::{
    ProviderCatalogCommand, ProviderCatalogEvent, project_provider_catalog_event,
};
use chart_chrome::{ExchangeCode, SymbolSearchCategory, SymbolSearchScope};
use chart_context_menus::{
    ChartSettingsTemplateView, ChartSettingsView, SETTINGS_CONTROL_WIDTH, chart_context_menu_layer,
    chart_settings_menu_layer, clamp_panel_origin_within, color_dot_button, color_swatch_button,
    named_color_button, overlay_height, price_axis_menu_layer,
};
#[cfg(test)]
use chart_context_menus::{
    PriceAxisMenuRow, chart_capture_menu_items, chart_context_menu_items,
    clamp_chart_capture_flyout_origin, clamp_chart_context_menu_origin,
    clamp_price_axis_menu_origin, price_axis_flyout_rows, price_axis_root_rows,
};
use chart_pane_controller::ChartState;
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
use chrome_menu::chrome_menu_extent;
#[cfg(test)]
use chrome_menu::{
    CHROME_MENU_LIST_HEIGHT, CHROME_MENU_MAX_HEIGHT, CHROME_MENU_SEARCH_HEIGHT, CHROME_MENU_WIDTH,
};
use command_palette::command_palette_layer;
use context_panel::{ContextPanelHeightDrag, ContextPanelState, context_panel};
use drawing_context_toolbar::DrawingContextToolbar;
use drawing_settings_dialog::{DrawingSettingsDialogState, drawing_settings_dialog_layer};
use drawing_toolbar::{
    DrawingFavoritesToolbar, DrawingSidebar, DrawingToolChoice, DrawingToolMenu,
    DrawingToolbarState, drawing_favorites_default_origin, drawing_favorites_toolbar_layer,
    drawing_tool_menu_layer, drawing_toolbar,
};
use frameless_title_bar::{
    FramelessTitleBar, TitleBarPlacement, frameless_reveal_zone, frameless_title_bar_row,
    title_bar_placement,
};
use fullscreen_hint::{FullscreenHint, fullscreen_hint_layer};
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
    button::{Button, ButtonSize, ButtonVariant, close_button},
    checkbox::CheckboxRow,
    color_picker::{ColorPicker, normalize_hex_color, parse_hex_hsla, transparency_backdrop},
    dialog::{
        ConfirmationDialog, ConfirmationTone, Dialog, DialogAlign, DialogMove, DialogSize,
        FLOATING_PANEL_MOVE_CURSOR,
    },
    icon::Icon,
    input::{Input, InputEvent, InputState, TextArea, TextareaState},
    loader::Loader,
    menu::{
        ANCHOR_GAP, MENU_PANEL_INSET, MENU_SEPARATOR_HEIGHT, MenuAlign, MenuAnchor, MenuPanel,
        MenuPlacement, MenuRow, MenuScale, MenuSide, PopupAnimationOrigin,
        animate_popup_from_origin, menu_panel_chrome_height, menu_panel_height,
    },
    platform_font_weight, platform_tabular_numerals,
    rem_scale::{ROOT_REM_PX, design_rems, rem_scaled},
    scroll::{ThinScrollbar, tracked_overflow_y_scrollbar},
    switch::{Switch, SwitchRow},
    tab::{Tab, TabList},
    theme::{base_theme, field_border, gpui_color, platform_border_width, popover_surface},
    tooltip::{TooltipSpec, with_tooltip},
};
use num_traits::ToPrimitive;
use order_book_panel::OrderBookPanelState;
use order_flow_dialog::{OrderFlowDialogState, OrderFlowIndicator, order_flow_dialog_layer};
use order_ticket::TradingOrderControlsState;
use platform_menu::platform_menu_layer;
use price_alert_dialog::{
    price_alert_dialog_layer, price_alert_trigger_text, replace_chart_price_alert_lines,
    runtime_price_alerts,
};
use reqwest_client::ReqwestClient;
use side_panel_dock::{WorkspaceSidePanelState, workspace_side_panel};
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap, VecDeque},
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
use symbol_browser::{
    ExactSymbolMatch, InstrumentMenuAction, InstrumentMenuEntry, InstrumentMenuSelection,
    OpenedProduct, SymbolBrowser, SymbolBrowserStatus, SymbolListingSummary, SymbolSearchFailure,
    SymbolSearchRequest,
};
use symbol_menu::{
    InstrumentSelectorAvailability, InstrumentSelectorMenu, InstrumentSelectorState,
    instrument_dialog_content, instrument_selector,
};
#[cfg(test)]
use terminal_chrome::{
    CaptionPlatform, CaptionPointerOwner, DrawingHistoryControl, WindowMoveGestureTransition,
    caption_keyboard_activates, caption_pointer_owner, connection_presentation,
};
use terminal_chrome::{
    WindowCommand, WindowMoveGestureEvent, WorkspaceTabBarState, chrome_tooltip, exchange_mark,
    fullscreen_escape_command, header_icon, mark_tile, series_glyph, terminal_header,
    watchlist_step, window_move_gesture_transition, workspace_title_bar,
};
use terminal_view::{
    TerminalShellInit, WorkspaceSplitDrag, WorkspaceTabsRestore, restored_workspace_tab,
    terminal_root, workspace_tab_strip, workspace_tabs_root,
};
use time_sales_panel::TimeSalesPanelState;
use toaster::Toaster;
use update::{DesktopUpdater, UpdatePresentation, UpdateState};
use volume_dialog::volume_dialog_layer;
use watchlist_panel::{WATCHLIST_ROW_HEIGHT, WatchlistFlagsView, WatchlistPanelState};
#[cfg(test)]
use workspace_layout::workspace_split_ratio;
use workspace_layout::{
    WorkspaceMarketArea, WorkspaceMaximizeTransition, pane_chart_notice, toast_host,
    workspace_market_area, workspace_maximize_transition, workspace_pane_grid,
};

fn install_platform_http_client(cx: &mut App) {
    match ReqwestClient::user_agent(concat!("Aeris/", env!("CARGO_PKG_VERSION"))) {
        Ok(client) => cx.set_http_client(Arc::new(client)),
        Err(error) => diagnostic!("Aeris image networking degraded: {error}"),
    }
}

trait AccountRefreshExitClaim {
    fn wait_for_exit(&self) -> Result<(), String>;
    fn retain_for_exit(self);
}

impl AccountRefreshExitClaim for aeris_account_runtime::AccountRefreshQuiesce {
    fn wait_for_exit(&self) -> Result<(), String> {
        self.wait()
    }

    fn retain_for_exit(self) {
        self.retain_until_process_exit();
    }
}

fn retain_account_refresh_quiesce_for_exit<Q: AccountRefreshExitClaim>(
    account_refresh: Result<Q, String>,
    context: &str,
) -> Result<(), String> {
    let quiesce = account_refresh.map_err(|error| {
        format!("Aeris refused {context} because account refresh could not be quiesced: {error}")
    })?;
    quiesce.wait_for_exit().map_err(|error| {
        format!("Aeris refused {context} because durable account refresh did not settle: {error}")
    })?;
    quiesce.retain_for_exit();
    Ok(())
}

fn exit_after_account_refresh_quiesce(
    account_service: &aeris_account_runtime::AccountService,
    exit_code: i32,
) -> ! {
    let exit_code = match retain_account_refresh_quiesce_for_exit(
        account_service.begin_refresh_quiesce(),
        "process exit",
    ) {
        Ok(()) => exit_code,
        Err(error) => {
            diagnostic!("{error}");
            1
        }
    };
    flush_diagnostics_before_exit();
    std::process::exit(exit_code);
}

pub(crate) fn flush_diagnostics_before_exit() {
    if !aeris_observability::flush_diagnostics() {
        // The diagnostic writer safely reports to stderr even if its bounded
        // file sink failed; do not attempt another flush on this exit path.
        diagnostic!("Aeris diagnostics could not be flushed before process exit");
    }
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
                diagnostic!("Aeris session shutdown was blocked: {error}");
                return None;
            }
        };
        match quiesce.wait() {
            Ok(()) => Some(aeris_platform_runtime::NativeSessionShutdownPermit::new(
                quiesce,
            )),
            Err(error) => {
                diagnostic!("Aeris session shutdown was blocked: {error}");
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
            self.state
                .waker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            return Poll::Ready(());
        }
        *self
            .state
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cx.waker().clone());
        if self.state.pending.swap(false, Ordering::AcqRel) {
            self.state
                .waker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl Drop for UiWakeNotified {
    fn drop(&mut self) {
        self.state
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

#[cfg(test)]
mod ui_wake_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::task::{Wake, Waker};

    struct CountingWake(AtomicUsize);

    impl Wake for CountingWake {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn retired_ui_listener_cannot_be_woken_by_a_late_trading_publication() {
        let wake = UiWake::default();
        let callback = wake.callback();
        let count = Arc::new(CountingWake(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&count));
        let mut task = TaskContext::from_waker(&waker);
        let mut listener = wake.notified();
        assert!(Pin::new(&mut listener).poll(&mut task).is_pending());
        drop(listener);
        std::thread::spawn(move || callback())
            .join()
            .expect("late bridge publication");
        assert_eq!(count.0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn completed_ui_listener_does_not_keep_a_stale_waker() {
        let wake = UiWake::default();
        let callback = wake.callback();
        let count = Arc::new(CountingWake(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&count));
        let mut task = TaskContext::from_waker(&waker);
        let mut listener = wake.notified();
        assert!(Pin::new(&mut listener).poll(&mut task).is_pending());
        wake.state.pending.store(true, Ordering::Release);
        assert!(Pin::new(&mut listener).poll(&mut task).is_ready());
        std::thread::spawn(move || callback())
            .join()
            .expect("publication after completion");
        assert_eq!(count.0.load(Ordering::SeqCst), 0);
    }
}

const SIDE_PANEL_INITIAL_WIDTH: f32 = 400.0;
const SIDE_PANEL_MINIMUM_WIDTH: f32 = 360.0;
const SIDE_PANEL_MAXIMUM_WIDTH: f32 = 480.0;
const SIDE_PANEL_RESIZE_HANDLE_WIDTH: f32 = 8.0;
/// Chart area always left above the context panel while resizing.
const CONTEXT_PANEL_MINIMUM_CHART_HEIGHT: f32 = 160.0;
const MAXIMUM_STATUS_CHARACTERS: usize = 160;
const MAXIMUM_OPEN_WORKSPACES: usize = 8;
/// Hard ceiling that keeps pane-owned charts, market demand, persistence and
/// worker queues bounded. Plan limits come from `account::plan_policy`.
const CHART_PANE_CAPACITY: usize = 64;
const CHART_CONTEXT_MENU_WIDTH: f32 = 228.0;
const CHART_CONTEXT_MENU_ROW_HEIGHT: f32 = 32.0;
const CHART_CONTEXT_MENU_SEPARATOR_HEIGHT: f32 = MENU_SEPARATOR_HEIGHT;
const PRICE_AXIS_FLYOUT_WIDTH: f32 = 296.0;
const PRICE_AXIS_FLYOUT_GAP: f32 = 4.0;
const CHART_CAPTURE_FLYOUT_WIDTH: f32 = 180.0;
const PRICE_AXIS_MENU_GAP: f32 = 4.0;
const OVERLAY_EDGE_MARGIN: f32 = 8.0;
const TIMEFRAME_MENU_WIDTH: f32 = 168.0;
const TIME_ZONE_MENU_WIDTH: f32 = 320.0;
const TIME_ZONE_MENU_MAX_HEIGHT: f32 = 520.0;
const ACCOUNTS_PANEL_WIDTH: f32 = 760.0;
const ACCOUNTS_PANEL_HEIGHT: f32 = 560.0;
const TIMEFRAME_FLYOUT_WIDTH: f32 = 136.0;
const TIMEFRAME_FLYOUT_GAP: f32 = 5.0;
const QUICK_TIMEFRAME_POPUP_WIDTH: f32 = 300.0;
const QUICK_TIMEFRAME_POPUP_TOP: f32 = 64.0;
const TIMEFRAME_TYPEAHEAD_LIMIT: usize = 8;
const CHART_SETTINGS_PANEL_WIDTH: f32 = 840.0;
const CHART_SETTINGS_PANEL_HEIGHT: f32 = 600.0;
/// Share of the shared screen-aware menu growth the chart settings panel takes, so it stays
/// compact on large screens like the chart context menus.
const CHART_SETTINGS_GROWTH_SHARE: f32 = 0.5;
/// Design height of the settings title bar that doubles as the panel's drag handle.
const CHART_SETTINGS_TITLE_BAR_HEIGHT: f32 = 44.0;
const CHART_SETTINGS_SIDEBAR_WIDTH: f32 = 168.0;
const WORKSPACE_TITLE_BAR_HEIGHT: f32 = 42.0;
const WORKSPACE_PANE_BOTTOM_INSET: f32 = 2.0;
// Bound UI work when a provider delivers a burst of updates. Remaining mailbox
// messages stay queued and wake the next GPUI frame.
const MARKET_MESSAGES_PER_FRAME: usize = 64;
const CHART_SYNC_EVENTS_PER_SURFACE: usize = 32;
const WORKSPACE_TAB_MAX_WIDTH: f32 = 360.0;
const WORKSPACE_TAB_HORIZONTAL_PADDING: f32 = 12.0;
const WORKSPACE_TAB_CONTENT_GAP: f32 = 4.0;
const WORKSPACE_TAB_CLOSE_GAP: f32 = 8.0;
const WORKSPACE_TAB_EXCHANGE_GLYPH: f32 = 16.0;
const TOOLTIP_OPEN_DELAY: Duration = Duration::from_millis(400);
const CHROME_OVERLAY_TRANSITION_DURATION: Duration = Duration::from_millis(140);
const CHROME_OVERLAY_EXIT_DURATION: Duration = Duration::from_millis(100);
const CHART_COPY_FEEDBACK_DURATION: Duration = Duration::from_millis(600);
const CHART_COPY_SUCCESS_ANIMATION_DURATION: Duration = Duration::from_millis(180);
/// Minimum wait before a study registration or automatic reinitialization that
/// failed is attempted again for the same chart series.
const STUDY_RETRY_DELAY: Duration = Duration::from_secs(5);

mod lifecycle;
use lifecycle::DesktopLifecycle;

mod workspace_persistence;
use workspace_persistence::{WorkspaceLayoutPersistence, WorkspaceLayoutShutdownWait};

mod chart_drawings;
use chart_drawings::{LayoutDrawings, PaneDrawingSync};

mod layout_library;
use layout_library::LayoutLibrary;

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
        SaveActiveLayout,
        SplitPaneHorizontal,
        SplitPaneVertical,
        ClosePane,
        ToggleOneClickTrading,
        TradingBuyMarket,
        TradingSellMarket,
        TradingCancelAll,
        TradingFlattenAccount,
        TradingKillSwitch,
        OpenCommandPalette,
        CancelProviderLogin,
        RefreshBrokerConnections,
    ]
);

#[derive(Clone, PartialEq)]
struct ConnectProvider(ProviderId);

#[derive(Clone, PartialEq)]
struct DisconnectProvider(ProviderId);

macro_rules! provider_action {
    ($name:ident) => {
        impl gpui::Action for $name {
            fn boxed_clone(&self) -> Box<dyn gpui::Action> {
                Box::new(self.clone())
            }
            fn partial_eq(&self, other: &dyn gpui::Action) -> bool {
                other.as_any().downcast_ref::<Self>() == Some(self)
            }
            fn name(&self) -> &'static str {
                Self::name_for_type()
            }
            fn name_for_type() -> &'static str {
                concat!("aeris::", stringify!($name))
            }
            fn build(value: serde_json::Value) -> gpui::Result<Box<dyn gpui::Action>> {
                let id = value
                    .as_str()
                    .ok_or_else(|| gpui::private::anyhow::anyhow!("Expected a provider ID"))?;
                let descriptor = aeris_market_runtime::published_provider_descriptors()
                    .iter()
                    .find(|descriptor| descriptor.id == id)
                    .ok_or_else(|| gpui::private::anyhow::anyhow!("Unknown provider ID"))?;
                Ok(Box::new(Self(ProviderId(descriptor.id))))
            }
        }
        gpui::register_action!($name);
    };
}
provider_action!(ConnectProvider);
provider_action!(DisconnectProvider);

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

fn provider_presentation(
    provider: ProviderId,
) -> Option<&'static aeris_contracts::ProviderPresentationDescriptor> {
    aeris_market_runtime::built_in_provider_presentations()
        .iter()
        .find(|descriptor| descriptor.id == provider.id())
}

/// Whether the provider streams every resting order at each price, which the
/// order book's queue column shows.
fn provider_order_queues_available(provider: ProviderId) -> bool {
    provider_presentation(provider).is_some_and(presentation_order_queues_available)
}

fn presentation_order_queues_available(
    descriptor: &aeris_contracts::ProviderPresentationDescriptor,
) -> bool {
    descriptor.feed_level == aeris_contracts::ProviderFeedLevel::OrderLevels
}

/// Whether the provider streams trade prints. Footprint charts, tape studies and
/// time and sales are drawn from them, so a provider without a trade tape offers none.
fn provider_trades_available(provider: ProviderId) -> bool {
    provider_presentation(provider).is_some_and(|descriptor| descriptor.trades_available)
}

/// The chart types a provider can draw; a footprint needs trade prints.
fn provider_chart_types(provider: ProviderId) -> Vec<ChartType> {
    chart_types_for_trades(provider_trades_available(provider))
}

fn chart_types_for_trades(trades: bool) -> Vec<ChartType> {
    ChartType::ALL
        .into_iter()
        .filter(|chart_type| trades || *chart_type != ChartType::Footprint)
        .collect()
}

fn provider_intervals(provider: ProviderId) -> &'static [ChartInterval] {
    static INTERVALS: OnceLock<Vec<(&'static str, Vec<ChartInterval>)>> = OnceLock::new();
    INTERVALS
        .get_or_init(|| {
            aeris_market_runtime::built_in_provider_presentations()
                .iter()
                .map(|descriptor| {
                    (
                        descriptor.id,
                        intervals_from_labels(descriptor.chart_interval_labels),
                    )
                })
                .collect()
        })
        .iter()
        .find(|(id, _)| *id == provider.id())
        .map_or(&[], |(_, intervals)| intervals.as_slice())
}

fn intervals_from_labels(labels: &[&str]) -> Vec<ChartInterval> {
    labels
        .iter()
        .filter_map(|label| {
            ChartInterval::ALL
                .iter()
                .copied()
                .find(|interval| interval.label() == *label)
        })
        .collect()
}

/// The interval a chart on `interval` opens `instrument` at: the same interval when the
/// instrument's provider offers it, otherwise the nearest one it does, so a seconds chart switched
/// to a minute-bar provider opens at one minute instead of failing.
fn instrument_interval(
    instrument: &InstallProviderInstrument,
    interval: ChartInterval,
) -> ChartInterval {
    ProviderId::find(&instrument.provider)
        .and_then(|provider| interval.nearest_offered(provider_intervals(provider)))
        .unwrap_or(interval)
}

/// The instrument menu opens on the results for its current query, an empty one
/// listing the provider's markets or products. A completed chart selection consumes
/// the search it came from (one-shot selection authorization), and closing the menu
/// clears the query, so an idle browser that no longer lists `query` searches again.
fn instrument_listing_refresh_needed(
    browser: &SymbolBrowser,
    selection_pending: bool,
    query: &str,
) -> bool {
    !selection_pending && !browser.lists_query(query) && !browser.search_pending()
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

/// The chart state a surface last observed, so only a change redraws the surface.
#[derive(Clone, Copy, Debug, Default)]
struct ObservedChart {
    clock_revision: u64,
    /// A selection change redraws the terminal's drawing toolbar.
    selected_drawing: Option<ChartDrawingId>,
}

struct WorkspaceSurface {
    chart: Option<Entity<AerisChartView>>,
    order_book: Entity<ReadOnlyOrderBookView>,
    order_flow: order_flow_presenter::OrderFlowPresenter,
    context_panel: context_panel_controller::ContextPanelController,
    chart_link_group: u8,
    chart_link_flags: u8,
    /// The trader's selected IANA chart time zone. The surface owns it so every chart this
    /// pane creates (restore, symbol or timeframe swap) and every save use the same value.
    chart_time_zone: String,
    pending_chart_sync_events: VecDeque<aeris_chart_integration::ChartSyncEvent>,
    pending_linked_instrument: Option<InstallProviderInstrument>,
    chrome: pane_chrome_state::PaneChromeState,
    scrolls: WorkspaceScrollHandles,
    chart_state: ChartState,
    chart_failure: Option<ChartFailure>,
    theme: AerisTheme,
    replay_label: String,
    worker_label: String,
    subscription_id: String,
    bridge_label: String,
    market_worker: MarketDataWorker,
    lifecycle: DesktopLifecycle,
    pending_ui_diagnostics: Option<PendingUiDiagnostics>,
    connection_state: Option<FeedConnectionState>,
    market_session_status: Option<aeris_contracts::MarketSessionStatus>,
    provider_transport_rtt_nanos: Option<u64>,
    trading_pnl: TradingPaneState,
    trading_pane_token: aeris_desktop::trading::TradingPaneToken,
    order_quantity: order_quantity::OrderQuantityControls,
    order_ticket: order_ticket::OrderTicketControls,
    chart_trading_visibility: ChartTradingVisibilitySettings,
    symbol_entry: symbol_entry::SymbolEntry,
    market_state: WorkspaceMarketState,
    series_message: String,
    indicator_input: Entity<InputState>,
    timeframe_input: Entity<InputState>,
    time_zone_input: Entity<InputState>,
    studies: studies_controller::StudiesController,
    indicator_settings_dialog: Option<IndicatorSettingsDialog>,
    chrome_focus: FocusHandle,
    provider: ProviderId,
    product: Option<InstallProviderInstrument>,
    descriptor: Option<aeris_market_runtime::InstrumentDescriptor>,
    pending_descriptor: Option<(u64, aeris_market_runtime::InstrumentDescriptor)>,
    restore_descriptor: Option<aeris_market_runtime::InstrumentDescriptor>,
    provider_display_name: Option<String>,
    pending_provider_display_name: Option<(u64, String)>,
    restore_provider_display_name: Option<String>,
    interval: ChartInterval,
    chart_pane_controller: chart_pane_controller::ChartPaneController,
    persistence: pane_persistence::PanePersistence,
    observed_chart: ObservedChart,
    pending_chart_context_menu: Option<ChartContextRequest>,
    pending_pane_activate: PaneActivationRequest,
    pending_settings_request: Option<LegendSettingsRequest>,
    pending_study_remove_request: Option<StudyInstanceId>,
    resource_class: ConsumerResourceClass,
    chart_chrome: chart_chrome::ChartChromePreferences,
    retained_chart_presentation: RetainedChartPresentation,
    alerts: alerts_controller::AlertsController,
    #[cfg(feature = "diagnostics")]
    foreground_interactions: ForegroundInteractionDiagnostics,
    #[cfg(feature = "diagnostics")]
    live_evidence_enabled: bool,
    #[cfg(feature = "diagnostics")]
    live_evidence_publications: u16,
}

/// Pane-originated notice changes are delivered to the window's single toast owner.
enum PaneNoticeEvent {
    Post(notification_center::UserNotice),
    Clear(notification_center::NoticeKey),
}

impl gpui::EventEmitter<PaneNoticeEvent> for WorkspaceSurface {}

/// Pane-only choices and feedback. Runtime-derived facts stay in the shared store.
#[derive(Default)]
struct TradingPaneState {
    feedback: Option<aeris_desktop::trading::TradingCommandFeedback>,
    order_entry: TradingOrderEntryState,
    applied_selection: Option<(
        Option<String>,
        Option<aeris_trading::TradingAccountId>,
        ChartTradingVisibilitySettings,
    )>,
    account_creator: Option<PracticeAccountFormState>,
    account_delete_confirmation: Option<aeris_trading::TradingAccountId>,
}

#[cfg(test)]
fn instruments_with_activity<'a>(
    orders: &'a [aeris_trading::Order],
    positions: &'a [aeris_trading_runtime::PositionPnl],
    selected: Option<&aeris_trading::TradingAccountId>,
) -> std::collections::HashSet<&'a str> {
    let Some(account_id) = selected else {
        return std::collections::HashSet::new();
    };
    let orders = orders
        .iter()
        .filter(|order| order.status.is_open() && &order.account_id == account_id)
        .map(|order| order.instrument_id.as_str());
    let positions = positions
        .iter()
        .map(|position| &position.position)
        .filter(|position| &position.account_id == account_id && position.net_quantity.units() != 0)
        .map(|position| position.instrument_id.as_str());
    orders.chain(positions).collect()
}

struct PaneLockView<'a> {
    selected_account: Option<&'a aeris_trading::TradingAccountId>,
    locks: &'a [aeris_trading_runtime::RiskLock],
}

fn selected_account_lock_reason<'a>(state: impl Into<PaneLockView<'a>>) -> Option<&'a str> {
    let view = state.into();
    let account_id = view.selected_account?;
    view.locks
        .iter()
        .find(|lock| &lock.account_id == account_id)
        .map(|lock| lock.reason.as_str())
}

impl WorkspaceSurface {
    fn selected_account_lock_reason(&self, cx: &App) -> Option<String> {
        let store = self.lifecycle.trading_store()?;
        let store = store.read(cx);
        selected_account_lock_reason(PaneLockView {
            selected_account: self.trading_pnl.order_entry.selected_account_id.as_ref(),
            locks: store.current()?.risk_locks.as_slice(),
        })
        .map(str::to_string)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TradingOrderEntryState {
    quantity: u64,
    selected_account_id: Option<aeris_trading::TradingAccountId>,
    ticket: order_ticket::TicketSettings,
}

impl Default for TradingOrderEntryState {
    fn default() -> Self {
        Self {
            quantity: 1,
            selected_account_id: None,
            ticket: order_ticket::TicketSettings::default(),
        }
    }
}

/// The inline new-practice-account form in the Accounts panel's Trading accounts tab.
struct PracticeAccountFormState {
    name: Entity<InputState>,
    equity: Entity<InputState>,
}

/// Every market-data connection the Accounts panel presents.
#[derive(Clone, Default)]
struct DataConnections {
    providers: std::collections::BTreeMap<&'static str, provider_login::ProviderLoginView>,
}

impl DataConnections {
    fn view(&self, provider: ProviderId) -> Option<&provider_login::ProviderLoginView> {
        self.providers.get(provider.id())
    }

    fn view_mut(&mut self, provider: ProviderId) -> &mut provider_login::ProviderLoginView {
        self.providers.entry(provider.id()).or_default()
    }

    /// Whether the runtime reported no stored connection for a provider that needs one.
    fn disconnected(&self, provider: ProviderId) -> bool {
        match provider.descriptor().map(|descriptor| descriptor.connect) {
            Some(aeris_market_runtime::ConnectMethod::Public) | None => false,
            Some(aeris_market_runtime::ConnectMethod::VaultCredentials { .. }) => self
                .view(provider)
                .is_some_and(provider_login::ProviderLoginView::missing),
        }
    }
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
    /// The user settings the study adopts once the runtime accepts them; `None` rebinds an
    /// automatic reinitialization to `series` with the settings it already has.
    replacement_settings: Option<Vec<WorkspaceStudySettingState>>,
}

/// A chart settings request waiting for the workspace to select its pane and open the dialog:
/// a legend control, or a drawing the trader double-clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LegendSettingsRequest {
    Study(StudyInstanceId),
    BigTrades,
    Volume,
    OrderFlow(OrderFlowIndicator),
    Drawing(ChartDrawingId),
}

/// The one settings dialog a surface can have open.
enum IndicatorSettingsDialog {
    Study(StudySettingsDialogState),
    BigTrades(BigTradesDialogState),
    Volume(VolumeDialogState),
    OrderFlow(OrderFlowDialogState),
    Drawing(Box<DrawingSettingsDialogState>),
}

/// Volume settings being edited. Every valid edit previews on the chart at once; Apply keeps
/// the chart as shown and Cancel restores `original`.
struct VolumeDialogState {
    original: VolumeSettings,
    draft: VolumeSettings,
    average_length: Entity<InputState>,
    color_picker: Option<DialogColorPickerState<VolumeColorTarget>>,
    /// The setting whose dropdown is open.
    menu: Option<String>,
    message: Option<String>,
    _subscriptions: Vec<gpui::Subscription>,
}

/// The volume color a dialog swatch edits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VolumeColorTarget {
    Up,
    Down,
    Average,
}

impl VolumeColorTarget {
    const fn label(self) -> &'static str {
        match self {
            Self::Up => "Up volume",
            Self::Down => "Down volume",
            Self::Average => "Average line",
        }
    }

    /// The color as drawn for `settings`.
    fn color(self, settings: &VolumeSettings) -> String {
        match self {
            Self::Up => settings.effective_up_color(),
            Self::Down => settings.effective_down_color(),
            Self::Average => settings.effective_average_color(),
        }
    }
}

/// The open color picker of an indicator dialog and the swatch it edits.
struct DialogColorPickerState<Target> {
    target: Target,
    input: Entity<InputState>,
    error: Option<String>,
}

/// One study line whose color the settings dialog edits.
struct StudyOutputColorDraft {
    output_index: usize,
    label: String,
    /// `None` keeps the chart's default color.
    color: Option<String>,
    default_color: String,
}

impl StudyOutputColorDraft {
    /// The color as drawn once the draft is applied.
    fn effective_color(&self) -> &str {
        self.color.as_deref().unwrap_or(&self.default_color)
    }
}

/// Big-trades settings being edited. Every valid edit previews on the chart at once; Apply keeps
/// the chart as shown and Cancel restores `original`.
struct BigTradesDialogState {
    original: BigTradesSettings,
    /// `None` selects the fixed minimum typed into `minimum_volume`.
    intensity: Option<BigTradesIntensity>,
    size: BigTradesSize,
    show_volume: bool,
    minimum_volume: Entity<InputState>,
    /// The setting whose dropdown is open.
    menu: Option<String>,
    message: Option<String>,
    _subscriptions: Vec<gpui::Subscription>,
}

/// Study settings being edited. Style edits preview on the chart at once and calculation edits
/// reinitialize the study as they are made; Apply keeps them and Cancel restores the style and
/// settings of `original`.
struct StudySettingsDialogState {
    study_id: StudyInstanceId,
    title: String,
    /// The study as it was when the dialog opened.
    original: WorkspaceChartStudyState,
    specs: Vec<StudySettingSpec>,
    draft_values: BTreeMap<String, StudySettingValue>,
    /// Host-owned output stroke width; presentation only, so it never reinitializes the study.
    line_width: u8,
    /// Host-owned line colors, presentation only like `line_width`.
    output_colors: Vec<StudyOutputColorDraft>,
    /// The picker's target is the output index of the edited line.
    color_picker: Option<DialogColorPickerState<usize>>,
    inputs: HashMap<String, Entity<InputState>>,
    _subscriptions: Vec<gpui::Subscription>,
    /// The setting whose dropdown is open.
    menu: Option<String>,
    message: Option<String>,
}

#[derive(Default)]
struct WorkspaceMarketState {
    symbol_selection_pending: bool,
}

#[derive(Default)]
struct RetainedChartPresentation {
    indicators: Vec<ChartIndicatorState>,
    price_precision: Option<u8>,
    chart_state: Option<WorkspaceChartState>,
    drawings: PaneDrawingSync,
    rejected_saved: RejectedSavedPresentation,
}

/// Saved presentation fields the restore could not parse. Each chart rebuild re-applies the
/// sanitized retained state, so the rejection is kept here and keeps the restore notice posted
/// until the trader edits that setting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RejectedSavedPresentation {
    appearance: bool,
    order_flow: bool,
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

/// A registered runtime identity. Unknown persisted ids have no desktop provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProviderId(&'static str);

impl ProviderId {
    const fn id(self) -> &'static str {
        self.0
    }

    fn descriptor(self) -> Option<&'static aeris_market_runtime::ProviderDescriptor> {
        aeris_market_runtime::published_provider_descriptors()
            .iter()
            .find(|descriptor| descriptor.id == self.0)
    }

    fn find(id: &str) -> Option<Self> {
        aeris_market_runtime::published_provider_descriptors()
            .iter()
            .find(|descriptor| descriptor.id == id)
            .map(|descriptor| Self(descriptor.id))
    }

    /// Only a terminal error state may hold this unregistered identity.
    const fn unsupported() -> Self {
        Self("")
    }

    fn display(self) -> &'static str {
        self.descriptor()
            .map_or(self.id(), |descriptor| descriptor.display_name)
    }
}

fn provider_ready_message(provider: ProviderId, product: &InstallProviderInstrument) -> String {
    let descriptor = provider_presentation(provider);
    let symbol = match descriptor.map(|descriptor| descriptor.catalog_symbol) {
        Some(aeris_contracts::ProviderCatalogSymbol::DisplaySymbol) => {
            product.display_symbol.as_str()
        }
        Some(aeris_contracts::ProviderCatalogSymbol::ProviderSymbol) | None => {
            product.provider_symbol.as_str()
        }
    };
    let display_name =
        descriptor.map_or_else(|| provider.id(), |descriptor| descriptor.display_name);
    let suffix = descriptor.map_or("", |descriptor| descriptor.ready_label_suffix);
    format!("{symbol} · {display_name}{suffix}")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TimeframeMenuGroup {
    Ticks,
    Seconds,
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
            Self::Seconds => "Seconds",
            Self::Minutes => "Minutes",
            Self::Hours => "Hours",
            Self::Days => "Days",
            Self::Weeks => "Weeks",
            Self::Months => "Months",
        }
    }
}

fn chrome_overlay_progress(phase: ChromeOverlayPhase, delta: f32) -> f32 {
    let delta = delta.clamp(0.0, 1.0);
    match phase {
        ChromeOverlayPhase::Opening => delta,
        ChromeOverlayPhase::Closing => 1.0 - delta,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderConnectionPresentation {
    None,
    Offline,
    Recovering,
    Stopped,
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

const fn chart_order_side(side: aeris_trading::OrderSide) -> ChartOrderSide {
    match side {
        aeris_trading::OrderSide::Buy => ChartOrderSide::Buy,
        aeris_trading::OrderSide::Sell => ChartOrderSide::Sell,
    }
}

const fn chart_order_kind(order_type: aeris_trading::OrderType) -> ChartOrderKind {
    match order_type {
        aeris_trading::OrderType::Market => ChartOrderKind::Market,
        aeris_trading::OrderType::Limit | aeris_trading::OrderType::RestingAt => {
            ChartOrderKind::Limit
        }
        aeris_trading::OrderType::Stop => ChartOrderKind::Stop,
        aeris_trading::OrderType::StopLimit => ChartOrderKind::StopLimit,
    }
}

/// The legacy variant exists only for the unchanged pre-migration chart fixture.
enum ChartOrderSource<'a> {
    WorkingSet(&'a aeris_trading_runtime::WorkingSet),
    Pane(&'a trading_store::PaneTradingView<'a>),
    #[cfg(test)]
    Snapshot(&'a aeris_trading_runtime::TradingSnapshot),
}

impl<'a> From<&'a aeris_trading_runtime::WorkingSet> for ChartOrderSource<'a> {
    fn from(view: &'a aeris_trading_runtime::WorkingSet) -> Self {
        Self::WorkingSet(view)
    }
}

impl<'a> From<&'a trading_store::PaneTradingView<'a>> for ChartOrderSource<'a> {
    fn from(view: &'a trading_store::PaneTradingView<'a>) -> Self {
        Self::Pane(view)
    }
}

#[cfg(test)]
impl<'a> From<&'a aeris_trading_runtime::TradingSnapshot> for ChartOrderSource<'a> {
    fn from(view: &'a aeris_trading_runtime::TradingSnapshot) -> Self {
        Self::Snapshot(view)
    }
}

impl ChartOrderSource<'_> {
    fn orders(&self) -> Box<dyn Iterator<Item = &aeris_trading::Order> + '_> {
        match self {
            Self::WorkingSet(view) => Box::new(view.orders.iter()),
            Self::Pane(view) => Box::new(view.orders()),
            #[cfg(test)]
            Self::Snapshot(view) => Box::new(view.orders.iter()),
        }
    }
    fn planned_legs(&self) -> Box<dyn Iterator<Item = &aeris_trading_runtime::PlannedLeg> + '_> {
        match self {
            Self::WorkingSet(view) => Box::new(view.planned_legs.iter()),
            Self::Pane(view) => Box::new(view.planned_legs()),
            #[cfg(test)]
            Self::Snapshot(view) => Box::new(view.planned_legs.iter()),
        }
    }
    fn managed_brackets(&self) -> &[aeris_trading_runtime::BracketView] {
        match self {
            Self::WorkingSet(view) => &view.managed_brackets,
            Self::Pane(view) => &view.working_set().managed_brackets,
            #[cfg(test)]
            Self::Snapshot(view) => &view.managed_brackets,
        }
    }
    fn order_events(&self) -> &[aeris_trading::OrderEvent] {
        match self {
            Self::WorkingSet(view) => &view.order_events,
            Self::Pane(view) => &view.working_set().order_events,
            #[cfg(test)]
            Self::Snapshot(view) => &view.order_events,
        }
    }
    fn protective_role(
        &self,
        id: &aeris_trading::ClientOrderId,
    ) -> Option<aeris_trading_runtime::ProtectionRole> {
        match self {
            Self::WorkingSet(view) => view.protective_role(id),
            Self::Pane(view) => view.working_set().protective_role(id),
            #[cfg(test)]
            Self::Snapshot(view) => view.protective_role(id),
        }
    }
}

fn chart_working_orders<'a>(
    snapshot: impl Into<ChartOrderSource<'a>>,
    account_id: &aeris_trading::TradingAccountId,
    instrument_id: &str,
    price_increment: Option<f64>,
) -> Vec<ChartWorkingOrder> {
    let snapshot = snapshot.into();
    snapshot
        .orders()
        // The chart draws actionable lines. Cancelled, rejected and filled orders stay in the
        // runtime's history but must leave the chart, or a cancel appears to do nothing.
        .filter(|order| {
            order.status.is_open()
                && &order.account_id == account_id
                && order.instrument_id.as_str() == instrument_id
        })
        // A pending bracket's held legs draw through the planned-leg chain below with their
        // role and entry; drawing the bare orders here as well would double every leg line.
        .filter(|order| {
            !snapshot
                .planned_legs()
                .any(|leg| leg.client_order_id == order.client_order_id)
        })
        .filter_map(|order| {
            let price = chart_price(order.limit_price.or(order.stop_price))?;
            let id = ChartOrderId::new(order.client_order_id.as_str()).ok()?;
            let side = chart_order_side(order.side);
            let kind = chart_order_kind(order.order_type);
            let status = aeris_chart_integration::chart_order_status(order.status);
            let semantics = chart_order_semantics(&snapshot, order, price_increment);
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
        .chain(chart_planned_legs(&snapshot, account_id, instrument_id))
        .collect()
}

fn chart_working_orders_from_pane(
    pane: &trading_store::PaneTradingView<'_>,
    account_id: &aeris_trading::TradingAccountId,
    instrument_id: &str,
    price_increment: Option<f64>,
) -> Vec<ChartWorkingOrder> {
    chart_working_orders(
        ChartOrderSource::Pane(pane),
        account_id,
        instrument_id,
        price_increment,
    )
}

/// A pending bracket's stop and targets, drawn as its entry's legs so the entry shows the
/// protection it carries and each leg can be dragged before the entry fills. A leg keeps the
/// identity of the order it becomes, so arming leaves the line in place.
fn chart_planned_legs<'a>(
    snapshot: &'a ChartOrderSource<'_>,
    account_id: &'a aeris_trading::TradingAccountId,
    instrument_id: &'a str,
) -> impl Iterator<Item = ChartWorkingOrder> + 'a {
    snapshot
        .planned_legs()
        .filter(move |leg| {
            &leg.account_id == account_id && leg.instrument_id.as_str() == instrument_id
        })
        .filter_map(|leg| {
            let price = chart_price(Some(leg.price))?;
            let bracket_id = ChartTradingGroupId::new(leg.bracket_id.clone()).ok();
            Some(ChartWorkingOrder {
                id: ChartOrderId::new(leg.client_order_id.as_str()).ok()?,
                account_id: None,
                pane_index: 0,
                price_scale: ChartTradingPriceScale::Right,
                side: chart_order_side(leg.side),
                kind: chart_order_kind(leg.order_type),
                role: match leg.role {
                    aeris_trading_runtime::ProtectionRole::StopLoss => ChartOrderRole::StopLoss,
                    aeris_trading_runtime::ProtectionRole::TakeProfit => ChartOrderRole::TakeProfit,
                },
                status: ChartOrderStatus::Working,
                price,
                stop_price: (leg.order_type == aeris_trading::OrderType::Stop).then_some(price),
                trailing_trigger_price: None,
                break_even_trigger_price: None,
                quantity: chart_quantity(leg.quantity)?,
                filled_quantity: 0.0,
                position_id: None,
                parent_order_id: Some(ChartOrderId::new(leg.entry_client_order_id.as_str()).ok()?),
                bracket_id: bracket_id.clone(),
                oco_group_id: bracket_id,
                revision: 0,
                annotations: Vec::new(),
            })
        })
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
    snapshot: &ChartOrderSource<'_>,
    order: &aeris_trading::Order,
    price_increment: Option<f64>,
) -> ChartOrderSemantics {
    let managed = snapshot.managed_brackets().iter().find(|bracket| {
        bracket.stop_client_order_id.as_ref() == Some(&order.client_order_id)
            || bracket
                .target_client_order_ids
                .contains(&order.client_order_id)
    });
    let protective_role = snapshot.protective_role(&order.client_order_id);
    let role = match protective_role {
        Some(aeris_trading_runtime::ProtectionRole::StopLoss) => ChartOrderRole::StopLoss,
        Some(aeris_trading_runtime::ProtectionRole::TakeProfit) => ChartOrderRole::TakeProfit,
        None => ChartOrderRole::Working,
    };
    let bracket_id =
        managed.and_then(|bracket| ChartTradingGroupId::new(bracket.bracket_id.clone()).ok());
    let parent_order_id =
        managed.and_then(|bracket| ChartOrderId::new(bracket.entry_client_order_id.as_str()).ok());
    let is_protection = protective_role.is_some();
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
    if let Some(warning) = snapshot.order_events().iter().find_map(|event| {
        (event.order_id == order.id
            && event.kind == aeris_trading::OrderEventKind::Accepted
            && event.reason == Some(aeris_trading::OrderEventReason::RiskWarning))
        .then_some(event.detail.as_deref())
        .flatten()
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
    snapshot: &ChartOrderSource<'_>,
    bracket: &aeris_trading_runtime::BracketView,
    price_increment: Option<f64>,
) -> (Option<f64>, Option<f64>) {
    let entry_price = bracket
        .entry_price
        .and_then(|price| chart_price(Some(price)));
    let increment = price_increment.filter(|increment| increment.is_finite() && *increment > 0.0);
    let side = snapshot
        .orders()
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

fn chart_positions_for<'a>(
    positions: impl Iterator<Item = &'a aeris_trading_runtime::PositionPnl>,
    account_id: &aeris_trading::TradingAccountId,
    instrument_id: &str,
    currency: &str,
) -> Vec<ChartTradingPosition> {
    positions
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

fn chart_positions_from_pane(
    pane: &trading_store::PaneTradingView<'_>,
    account_id: &aeris_trading::TradingAccountId,
    instrument_id: &str,
    currency: &str,
) -> Vec<ChartTradingPosition> {
    chart_positions_for(pane.positions(), account_id, instrument_id, currency)
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
    snapshot: &aeris_trading_runtime::WorkingSet,
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
            let side = chart_order_side(fill.side);
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
    snapshot: &aeris_trading_runtime::WorkingSet,
    pane: &trading_store::PaneTradingView<'_>,
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
    // Fixed-point scales are storage precision (some feeds store every value at eight places),
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

    let orders = chart_working_orders_from_pane(pane, account_id, instrument_id, price_increment);

    let positions = chart_positions_from_pane(pane, account_id, instrument_id, &account.currency);

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
    snapshot: &aeris_trading_runtime::WorkingSet,
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
    snapshot: &aeris_trading_runtime::WorkingSet,
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
    // A decimal price such as 1224.37 has no exact `f64`, and scaling it adds one more rounding,
    // so the scaled value can sit a few ulps off a whole unit. Accept that representation error,
    // which grows with magnitude, but nothing coarser: a price between units is still rejected.
    let representation_error = (units.abs() * 4.0 * f64::EPSILON).max(1.0e-6);
    if !rounded.is_finite() || representation_error >= 0.5 {
        return None;
    }
    if (units - rounded).abs() > representation_error {
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

/// Why the desktop could not send a chart trading action, worded for the order-entry status line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChartIntentRejection {
    Incomplete,
    NoAccount,
    AccountLocked,
    NoInstrument,
    NotOnChart,
    InvalidPrice,
    InvalidQuantity,
    PlannedLegKept,
    Unsupported,
}

impl ChartIntentRejection {
    const fn message(self) -> &'static str {
        match self {
            Self::Incomplete => "The chart sent an incomplete trading action. Try again.",
            Self::NoAccount => "Select a practice account to trade from the chart.",
            Self::AccountLocked => "The selected account is locked by a risk rule.",
            Self::NoInstrument => "The chart has no instrument loaded yet.",
            Self::NotOnChart => "That order or position belongs to another account or instrument.",
            Self::InvalidPrice => "That price isn't valid for this instrument.",
            Self::InvalidQuantity => "That size isn't valid for this instrument.",
            Self::PlannedLegKept => {
                "A pending bracket keeps its stop and target. Drag them, or cancel the entry."
            }
            Self::Unsupported => "That chart action isn't available for this order.",
        }
    }
}

fn chart_order_from_intent(
    intent: &ChartTradingIntent,
    app: &WorkspaceSurface,
    order_type: aeris_trading::OrderType,
) -> Result<aeris_trading_runtime::PlaceOrder, ChartIntentRejection> {
    let product = app
        .product
        .as_ref()
        .ok_or(ChartIntentRejection::NoInstrument)?;
    let account_id = app
        .trading_pnl
        .order_entry
        .selected_account_id
        .clone()
        .ok_or(ChartIntentRejection::NoAccount)?;
    let side = match intent.side.ok_or(ChartIntentRejection::Incomplete)? {
        ChartOrderSide::Buy => aeris_trading::OrderSide::Buy,
        ChartOrderSide::Sell => aeris_trading::OrderSide::Sell,
    };
    let price = intent
        .price
        .and_then(|price| chart_price_fixed_point(price, product.price_scale))
        .ok_or(ChartIntentRejection::InvalidPrice)?;
    let quantity = intent
        .quantity
        .and_then(|quantity| chart_quantity_fixed_point(quantity, product.quantity_scale))
        .ok_or(ChartIntentRejection::InvalidQuantity)?;
    let submitted_unix_nanos = app.lifecycle.clock().wall_unix_nanos();
    let provenance = aeris_trading::TradingProvenance {
        venue_id: "aeris-sim".to_string(),
        provider_id: product.provider.clone(),
        session_generation: product.session_generation,
        source_sequence: 1, // Placeholder; the trading owner stamps the current quote.
        observed_unix_nanos: submitted_unix_nanos,
    };
    let client_order_id = aeris_trading::ClientOrderId::try_new(format!(
        "chart-{}-{submitted_unix_nanos}",
        intent.sequence
    ))
    .map_err(|_| ChartIntentRejection::Incomplete)?;
    let (limit_price, stop_price) = match order_type {
        aeris_trading::OrderType::Market => (None, None),
        aeris_trading::OrderType::Limit | aeris_trading::OrderType::RestingAt => {
            (Some(price), None)
        }
        aeris_trading::OrderType::Stop => (None, Some(price)),
        aeris_trading::OrderType::StopLimit => (Some(price), Some(price)),
    };
    let instrument_id = aeris_instruments::InstrumentId::try_new(product.instrument_id.clone())
        .map_err(|_| ChartIntentRejection::NoInstrument)?;
    let order = aeris_trading_runtime::PlaceOrder {
        client_order_id,
        account_id,
        instrument_id,
        side,
        order_type,
        time_in_force: aeris_trading::TimeInForce::Day,
        quantity,
        limit_price,
        stop_price,
        submitted_unix_nanos,
        provenance,
    };
    Ok(order)
}

/// Rolls back a chart trading intent the desktop could not dispatch, and says why in the
/// order-entry status line instead of leaving the chart silently unchanged.
fn reject_chart_intent(
    chart: &Entity<AerisChartView>,
    sequence: u32,
    reason: ChartIntentRejection,
    app: &WorkspaceSurface,
    cx: &mut Context<WorkspaceSurface>,
) {
    app.lifecycle
        .trading_feedback()
        .record_for_pane(&app.trading_pane_token, Err(reason.message().to_string()));
    chart.update(cx, |chart, _| {
        chart.resolve_trading_intent(sequence, false);
    });
}

fn dispatch_chart_cancel(
    chart: Entity<AerisChartView>,
    sequence: u32,
    client_order_id: aeris_trading::ClientOrderId,
    expected_revision: u32,
    app: &WorkspaceSurface,
    cx: &mut Context<WorkspaceSurface>,
) {
    let service = app.lifecycle.trading().clone();
    let feedback = app.lifecycle.trading_feedback().clone();
    let Some(request_id) = feedback.begin_request(&app.trading_pane_token) else {
        chart.update(cx, |chart, _| chart.resolve_trading_intent(sequence, false));
        return;
    };
    let lost_pane_feedback = feedback.clone();
    let task = cx.background_executor().spawn(async move {
        let result = aeris_desktop::trading::ChartOrderResult::from_checked(
            service.cancel_order_checked(aeris_trading_runtime::RevisionedCancelOrder {
                client_order_id,
                expected_revision: Some(expected_revision),
            }),
            aeris_desktop::trading::OrderOutcome::Cancelled,
        );
        finish_chart_order_result(&feedback, request_id, result)
    });
    cx.spawn(async move |pane, cx| {
        let result = task.await;
        chart.update(cx, |chart, _| {
            chart
                .resolve_trading_intent(sequence, matches!(result, ChartOrderResolution::Accepted));
        });
        if let ChartOrderResolution::Stale(current) = result
            && pane
                .update(cx, |_, cx| {
                    app_stale_order_notice_and_line(&chart, &current, cx);
                })
                .is_err()
        {
            lost_pane_feedback.record(Err(aeris_desktop::trading::STALE_ORDER_NOTICE.to_string()));
        }
    })
    .detach();
}

enum ChartOrderResolution {
    Accepted,
    Rejected,
    Stale(Box<aeris_trading::Order>),
}

fn finish_chart_order_result(
    feedback: &aeris_desktop::trading::TradingFeedback,
    request_id: aeris_desktop::trading::RequestId,
    result: aeris_desktop::trading::ChartOrderResult,
) -> ChartOrderResolution {
    match result {
        aeris_desktop::trading::ChartOrderResult::Accepted(message) => {
            feedback.finish_request(request_id, Ok(message));
            ChartOrderResolution::Accepted
        }
        aeris_desktop::trading::ChartOrderResult::Rejected(message) => {
            feedback.finish_request(request_id, Err(message));
            ChartOrderResolution::Rejected
        }
        aeris_desktop::trading::ChartOrderResult::Stale(current) => {
            feedback.retire_request(request_id);
            ChartOrderResolution::Stale(current)
        }
    }
}

fn app_stale_order_notice_and_line(
    chart: &Entity<AerisChartView>,
    current: &aeris_trading::Order,
    cx: &mut Context<WorkspaceSurface>,
) {
    let mut snapshot = chart.read(cx).trading_snapshot().clone();
    if let Ok(id) = ChartOrderId::new(current.client_order_id.as_str()) {
        snapshot.orders.retain(|order| order.id != id);
        if current.status.is_open()
            && let Some(price) = chart_price(current.limit_price.or(current.stop_price))
        {
            let mut line = chart
                .read(cx)
                .trading_snapshot()
                .orders
                .iter()
                .find(|order| order.id == id)
                .cloned();
            if let Some(line) = line.as_mut() {
                line.price = price;
                line.stop_price = chart_price(current.stop_price);
                line.quantity = chart_quantity(current.quantity).unwrap_or(line.quantity);
                snapshot.orders.push(line.clone());
            }
        }
        chart.update(cx, |chart, chart_cx| {
            if chart.set_trading_snapshot(snapshot).is_ok() {
                chart_cx.notify();
            }
        });
    }
    post_stale_order_notice(cx);
}

fn post_stale_order_notice(cx: &mut Context<WorkspaceSurface>) {
    cx.emit(PaneNoticeEvent::Post(notification_center::UserNotice {
        key: notification_center::NoticeKey {
            scope: cx.entity_id().as_u64(),
            kind: "stale_order_edit",
        },
        severity: notification_center::NoticeSeverity::Warning,
        tone: notification_center::NoticeTone::Warning,
        source: notification_center::NoticeSource::Pane(cx.entity_id().as_u64()),
        title: aeris_desktop::trading::STALE_ORDER_NOTICE,
        detail: "The current order is shown on the chart.".into(),
        action: None,
        lifetime: notification_center::NoticeLifetime::UntilDismissed,
    }));
}

fn dispatch_chart_place_bracket(
    chart: Entity<AerisChartView>,
    intent: &ChartTradingIntent,
    app: &WorkspaceSurface,
    cx: &mut Context<WorkspaceSurface>,
) {
    let sequence = intent.sequence;
    match chart_bracket_from_intent(intent, app, cx) {
        Ok(command) => {
            let service = app.lifecycle.trading().clone();
            let feedback = app.lifecycle.trading_feedback().clone();
            let Some(request_id) = feedback.begin_request(&app.trading_pane_token) else {
                chart.update(cx, |chart, _| chart.resolve_trading_intent(sequence, false));
                return;
            };
            let task = cx.background_executor().spawn(async move {
                let result = service.place_chart_bracket(command);
                feedback
                    .finish_pane_result(request_id, result.map(|_| "Bracket placed".to_string()))
            });
            cx.spawn(async move |_, cx| {
                let accepted = task.await;
                chart.update(cx, |chart, _| {
                    chart.resolve_trading_intent(sequence, accepted);
                });
            })
            .detach();
        }
        Err(reason) => reject_chart_intent(&chart, sequence, reason, app, cx),
    }
}

fn chart_bracket_from_intent(
    intent: &ChartTradingIntent,
    app: &WorkspaceSurface,
    _cx: &App,
) -> Result<aeris_trading_runtime::PlaceChartBracket, ChartIntentRejection> {
    let product = app
        .product
        .as_ref()
        .ok_or(ChartIntentRejection::NoInstrument)?;
    let entry = chart_order_from_intent(intent, app, aeris_trading::OrderType::RestingAt)?;
    let price = |price: Option<f64>| {
        price
            .and_then(|price| chart_price_fixed_point(price, product.price_scale))
            .ok_or(ChartIntentRejection::InvalidPrice)
    };
    let stop_price = price(intent.stop_loss_price)?;
    let target_price = price(intent.take_profit_price)?;
    Ok(aeris_trading_runtime::PlaceChartBracket {
        entry,
        stop_price,
        target_price,
    })
}

fn chart_protection_from_intent(
    intent: &ChartTradingIntent,
    app: &WorkspaceSurface,
    cx: &App,
) -> Result<aeris_trading_runtime::PlaceOrder, ChartIntentRejection> {
    let account_id = app
        .trading_pnl
        .order_entry
        .selected_account_id
        .as_ref()
        .ok_or(ChartIntentRejection::NoAccount)?;
    let product = app
        .product
        .as_ref()
        .ok_or(ChartIntentRejection::NoInstrument)?;
    let store = app
        .lifecycle
        .trading_store()
        .ok_or(ChartIntentRejection::NotOnChart)?;
    let store = store.read(cx);
    let instrument = aeris_instruments::InstrumentId::try_new(product.instrument_id.clone())
        .map_err(|_| ChartIntentRejection::NotOnChart)?;
    let identity_matches = intent.position_id.as_ref().is_some_and(|position_id| {
        chart_position_id(account_id, &product.instrument_id).as_ref() == Some(position_id)
    }) || intent.order_id.as_ref().is_some_and(|order_id| {
        store.current().is_some_and(|_| {
            store
                .pane_view(account_id, &instrument)
                .orders()
                .any(|order| order.client_order_id.as_str() == order_id.as_str())
        })
    });
    if !identity_matches {
        return Err(ChartIntentRejection::NotOnChart);
    }
    let order_type = if intent.action == ChartTradingIntentAction::CreateStopLoss {
        aeris_trading::OrderType::Stop
    } else {
        aeris_trading::OrderType::Limit
    };
    chart_order_from_intent(intent, app, order_type)
}

fn dispatch_chart_create_protection(
    chart: Entity<AerisChartView>,
    intent: &ChartTradingIntent,
    app: &WorkspaceSurface,
    cx: &mut Context<WorkspaceSurface>,
) {
    let sequence = intent.sequence;
    let prepared = chart_protection_from_intent(intent, app, cx)
        .map(|prepared| (prepared, app.lifecycle.trading().clone()));
    let (order, service) = match prepared {
        Ok(prepared) => prepared,
        Err(reason) => {
            reject_chart_intent(&chart, sequence, reason, app, cx);
            return;
        }
    };
    let role = if intent.action == ChartTradingIntentAction::CreateStopLoss {
        aeris_trading_runtime::ProtectionRole::StopLoss
    } else {
        aeris_trading_runtime::ProtectionRole::TakeProfit
    };
    let feedback = app.lifecycle.trading_feedback().clone();
    let Some(request_id) = feedback.begin_request(&app.trading_pane_token) else {
        chart.update(cx, |chart, _| chart.resolve_trading_intent(sequence, false));
        return;
    };
    let task = cx.background_executor().spawn(async move {
        let result =
            service.place_protective_order(aeris_trading_runtime::PlaceProtection { order, role });
        feedback.finish_pane_result(
            request_id,
            result.map(|order| {
                aeris_desktop::trading::order_outcome(
                    &order,
                    aeris_desktop::trading::OrderOutcome::ProtectionPlaced,
                )
            }),
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

fn dispatch_chart_modify(
    chart: Entity<AerisChartView>,
    intent: &ChartTradingIntent,
    order: &aeris_trading::Order,
    app: &WorkspaceSurface,
    cx: &mut Context<WorkspaceSurface>,
) {
    let sequence = intent.sequence;
    let order_revision = order.revision;
    let prepared = chart_modify_from_intent(intent, order, app, cx)
        .map(|command| (command, app.lifecycle.trading().clone()));
    let (command, service) = match prepared {
        Ok(prepared) => prepared,
        Err(reason) => {
            reject_chart_intent(&chart, sequence, reason, app, cx);
            return;
        }
    };
    let feedback = app.lifecycle.trading_feedback().clone();
    let Some(request_id) = feedback.begin_request(&app.trading_pane_token) else {
        chart.update(cx, |chart, _| chart.resolve_trading_intent(sequence, false));
        return;
    };
    let lost_pane_feedback = feedback.clone();
    let task = cx.background_executor().spawn(async move {
        let result = aeris_desktop::trading::ChartOrderResult::from_checked(
            service.modify_order_checked(aeris_trading_runtime::RevisionedModifyOrder {
                order: command,
                expected_revision: Some(order_revision),
            }),
            aeris_desktop::trading::OrderOutcome::Modified,
        );
        finish_chart_order_result(&feedback, request_id, result)
    });
    cx.spawn(async move |pane, cx| {
        let result = task.await;
        chart.update(cx, |chart, _| {
            chart
                .resolve_trading_intent(sequence, matches!(result, ChartOrderResolution::Accepted));
        });
        if let ChartOrderResolution::Stale(current) = result
            && pane
                .update(cx, |_, cx| {
                    app_stale_order_notice_and_line(&chart, &current, cx);
                })
                .is_err()
        {
            lost_pane_feedback.record(Err(aeris_desktop::trading::STALE_ORDER_NOTICE.to_string()));
        }
    })
    .detach();
}

fn chart_modify_from_intent(
    intent: &ChartTradingIntent,
    order: &aeris_trading::Order,
    app: &WorkspaceSurface,
    _cx: &App,
) -> Result<aeris_trading_runtime::ModifyOrder, ChartIntentRejection> {
    let product = app
        .product
        .as_ref()
        .ok_or(ChartIntentRejection::NoInstrument)?;
    let client_order_id = intent
        .order_id
        .as_ref()
        .and_then(|order_id| aeris_trading::ClientOrderId::try_new(order_id.as_str()).ok())
        .ok_or(ChartIntentRejection::Incomplete)?;
    let price = intent
        .price
        .and_then(|value| chart_price_fixed_point(value, product.price_scale))
        .ok_or(ChartIntentRejection::InvalidPrice)?;
    let (limit_price, stop_price) = match order.order_type {
        aeris_trading::OrderType::Limit => (Some(price), None),
        aeris_trading::OrderType::Stop => (None, Some(price)),
        aeris_trading::OrderType::Market
        | aeris_trading::OrderType::StopLimit
        | aeris_trading::OrderType::RestingAt => {
            return Err(ChartIntentRejection::Unsupported);
        }
    };
    let modified_unix_nanos = app.lifecycle.clock().wall_unix_nanos();
    Ok(aeris_trading_runtime::ModifyOrder {
        client_order_id,
        time_in_force: order.time_in_force,
        limit_price,
        stop_price,
        modified_unix_nanos,
        provenance: aeris_trading::TradingProvenance {
            venue_id: "aeris-sim".to_string(),
            provider_id: product.provider.clone(),
            session_generation: product.session_generation,
            source_sequence: 1, // Placeholder; the trading owner stamps the current quote.
            observed_unix_nanos: modified_unix_nanos,
        },
    })
}

fn chart_close_position_from_intent(
    intent: &ChartTradingIntent,
    app: &WorkspaceSurface,
    cx: &App,
) -> Result<
    (
        aeris_trading::TradingAccountId,
        aeris_instruments::InstrumentId,
    ),
    ChartIntentRejection,
> {
    let account_id = app
        .trading_pnl
        .order_entry
        .selected_account_id
        .as_ref()
        .ok_or(ChartIntentRejection::NoAccount)?;
    let product = app
        .product
        .as_ref()
        .ok_or(ChartIntentRejection::NoInstrument)?;
    let expected_position_id = chart_position_id(account_id, &product.instrument_id)
        .ok_or(ChartIntentRejection::NotOnChart)?;
    let has_position = app.lifecycle.trading_store().is_some_and(|store| {
        let store = store.read(cx);
        aeris_instruments::InstrumentId::try_new(product.instrument_id.clone())
            .ok()
            .filter(|_| store.current().is_some())
            .is_some_and(|instrument| {
                store
                    .pane_view(account_id, &instrument)
                    .positions()
                    .any(|position| position.position.net_quantity.units() != 0)
            })
    });
    if intent.position_id.as_ref() != Some(&expected_position_id) || !has_position {
        return Err(ChartIntentRejection::NotOnChart);
    }
    aeris_desktop::trading::prepare_flatten_for(
        &product.instrument_id,
        Some(account_id.as_str().to_string()),
    )
    .ok_or(ChartIntentRejection::NoInstrument)
}

fn dispatch_chart_close_position(
    chart: Entity<AerisChartView>,
    intent: &ChartTradingIntent,
    app: &WorkspaceSurface,
    cx: &mut Context<WorkspaceSurface>,
) {
    let sequence = intent.sequence;
    let prepared = chart_close_position_from_intent(intent, app, cx)
        .map(|prepared| (prepared, app.lifecycle.trading().clone()));
    let ((account_id, instrument), service) = match prepared {
        Ok(prepared) => prepared,
        Err(reason) => {
            reject_chart_intent(&chart, sequence, reason, app, cx);
            return;
        }
    };
    let feedback = app.lifecycle.trading_feedback().clone();
    let Some(request_id) = feedback.begin_request(&app.trading_pane_token) else {
        chart.update(cx, |chart, _| chart.resolve_trading_intent(sequence, false));
        return;
    };
    let task = cx.background_executor().spawn(async move {
        feedback.finish_pane_result(
            request_id,
            aeris_desktop::trading::flatten_feedback(
                "Position closed",
                service.flatten(aeris_trading_runtime::FlattenScope {
                    account: Some(account_id),
                    instrument: Some(instrument),
                }),
            ),
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
        reject_chart_intent(&chart, sequence, ChartIntentRejection::Incomplete, app, cx);
        return;
    }
    if app.selected_account_lock_reason(cx).is_some()
        && !matches!(
            action,
            ChartTradingIntentAction::CancelOrder | ChartTradingIntentAction::ClosePosition
        )
    {
        reject_chart_intent(
            &chart,
            sequence,
            ChartIntentRejection::AccountLocked,
            app,
            cx,
        );
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
    if let Some(client_order_id) = chart_intent_planned_leg(intent, app, cx) {
        if action == ChartTradingIntentAction::ModifyOrder {
            dispatch_chart_move_planned_leg(chart, intent, client_order_id, app, cx);
        } else {
            reject_chart_intent(
                &chart,
                sequence,
                ChartIntentRejection::PlannedLegKept,
                app,
                cx,
            );
        }
        return;
    }
    let order = match chart_intent_order(intent, app, cx) {
        Ok(order) => order,
        Err(reason) => {
            reject_chart_intent(&chart, sequence, reason, app, cx);
            return;
        }
    };
    if action == ChartTradingIntentAction::ModifyOrder {
        dispatch_chart_modify(chart, intent, &order, app, cx);
        return;
    }
    dispatch_chart_cancel(
        chart,
        sequence,
        order.client_order_id.clone(),
        order.revision,
        app,
        cx,
    );
}

/// The selected account's pending-bracket leg on the chart's instrument that `intent` acts on.
fn chart_intent_planned_leg(
    intent: &ChartTradingIntent,
    app: &WorkspaceSurface,
    cx: &App,
) -> Option<aeris_trading::ClientOrderId> {
    let order_id = intent.order_id.as_ref()?;
    let selected_account = app.trading_pnl.order_entry.selected_account_id.as_ref()?;
    let product = app.product.as_ref()?;
    let instrument =
        aeris_instruments::InstrumentId::try_new(product.instrument_id.clone()).ok()?;
    let store = app.lifecycle.trading_store()?;
    store
        .read(cx)
        .pane_view(selected_account, &instrument)
        .planned_legs()
        .find(|leg| leg.client_order_id.as_str() == order_id.as_str())
        .map(|leg| leg.client_order_id.clone())
}

fn dispatch_chart_move_planned_leg(
    chart: Entity<AerisChartView>,
    intent: &ChartTradingIntent,
    client_order_id: aeris_trading::ClientOrderId,
    app: &WorkspaceSurface,
    cx: &mut Context<WorkspaceSurface>,
) {
    let sequence = intent.sequence;
    let prepared = app
        .product
        .as_ref()
        .ok_or(ChartIntentRejection::NoInstrument)
        .and_then(|product| {
            intent
                .price
                .and_then(|price| chart_price_fixed_point(price, product.price_scale))
                .ok_or(ChartIntentRejection::InvalidPrice)
        })
        .map(|price| (price, app.lifecycle.trading().clone()));
    let (price, service) = match prepared {
        Ok(prepared) => prepared,
        Err(reason) => {
            reject_chart_intent(&chart, sequence, reason, app, cx);
            return;
        }
    };
    let feedback = app.lifecycle.trading_feedback().clone();
    let Some(request_id) = feedback.begin_request(&app.trading_pane_token) else {
        chart.update(cx, |chart, _| chart.resolve_trading_intent(sequence, false));
        return;
    };
    let task = cx.background_executor().spawn(async move {
        feedback.finish_pane_result(
            request_id,
            service
                .move_planned_leg(aeris_trading_runtime::MovePlannedLeg {
                    client_order_id,
                    price,
                })
                .map(|_| "Bracket leg moved".to_string()),
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

/// The selected account's working order on the chart's instrument that `intent` acts on.
fn chart_intent_order(
    intent: &ChartTradingIntent,
    app: &WorkspaceSurface,
    cx: &App,
) -> Result<aeris_trading::Order, ChartIntentRejection> {
    let order_id = intent
        .order_id
        .as_ref()
        .ok_or(ChartIntentRejection::Incomplete)?;
    let selected_account = app
        .trading_pnl
        .order_entry
        .selected_account_id
        .as_ref()
        .ok_or(ChartIntentRejection::NoAccount)?;
    let product = app
        .product
        .as_ref()
        .ok_or(ChartIntentRejection::NoInstrument)?;
    let store = app
        .lifecycle
        .trading_store()
        .ok_or(ChartIntentRejection::NotOnChart)?;
    let store = store.read(cx);
    let instrument = aeris_instruments::InstrumentId::try_new(product.instrument_id.clone())
        .map_err(|_| ChartIntentRejection::NotOnChart)?;
    if store.current().is_none() {
        return Err(ChartIntentRejection::NotOnChart);
    }
    store
        .pane_view(selected_account, &instrument)
        .orders()
        .find(|order| order.client_order_id.as_str() == order_id.as_str())
        .cloned()
        .ok_or(ChartIntentRejection::NotOnChart)
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

fn legend_settings_request(request: ChartSettingsRequest) -> Option<LegendSettingsRequest> {
    let order_flow = |indicator| Some(LegendSettingsRequest::OrderFlow(indicator));
    match request {
        ChartSettingsRequest::Study(study_id) => {
            StudyInstanceId::try_from_u64(study_id).map(LegendSettingsRequest::Study)
        }
        ChartSettingsRequest::BigTrades => Some(LegendSettingsRequest::BigTrades),
        ChartSettingsRequest::Volume => Some(LegendSettingsRequest::Volume),
        ChartSettingsRequest::CumulativeDelta => order_flow(OrderFlowIndicator::CumulativeDelta),
        ChartSettingsRequest::DeltaHistogram => order_flow(OrderFlowIndicator::DeltaHistogram),
        ChartSettingsRequest::OrderBookDepth => order_flow(OrderFlowIndicator::OrderBook),
        ChartSettingsRequest::Drawing(id) => Some(LegendSettingsRequest::Drawing(id)),
    }
}

fn observe_chart(chart: Option<&Entity<AerisChartView>>, cx: &mut Context<WorkspaceSurface>) {
    if let Some(chart) = chart {
        order_quantity::attach_chart_trade_bar(chart, cx);
        cx.observe(chart, |app, chart, cx| {
            if app
                .persistence
                .observe_chart_user_state(chart.read(cx).user_state_revision())
            {
                app.sync_order_book_demand(cx);
                cx.notify();
            }
            let clock_revision = chart.read(cx).clock_revision();
            if clock_revision != app.observed_chart.clock_revision {
                app.observed_chart.clock_revision = clock_revision;
                cx.notify();
            }
            let selected_drawing = chart.read(cx).selected_drawing_id();
            if selected_drawing != app.observed_chart.selected_drawing {
                app.observed_chart.selected_drawing = selected_drawing;
                cx.notify();
            }
            app.close_vanished_drawing_settings_dialog(cx);
            let (intents, sync_events) = chart.update(cx, |chart, _| {
                (chart.take_trading_intents(), chart.take_sync_events())
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
            if let Some(request) = alert_request {
                app.open_price_alert_dialog(request, cx);
            }
            if had_alert_request || had_sync_events {
                cx.notify();
            }
            if chart.read(cx).has_market_data() {
                app.retained_chart_presentation.indicators = chart.read(cx).indicator_states();
                app.retained_chart_presentation.price_precision =
                    chart.read(cx).selected_price_precision();
            }
            if provider_presentation(app.provider).is_some()
                && let Some(viewport) = chart.read(cx).visible_time_range_unix_nanos()
            {
                let market_worker = &app.market_worker;
                app.persistence.report_viewport(viewport, |(start, end)| {
                    market_worker.try_set_chart_viewport(start, end).is_ok()
                });
            }
        })
        .detach();
        cx.subscribe(chart, |app, _, event: &ChartHostEvent, cx| {
            match event {
                ChartHostEvent::Activate => {
                    app.pending_pane_activate = PaneActivationRequest::Pending;
                }
                ChartHostEvent::ContextMenu(request) => {
                    app.pending_chart_context_menu = Some(request.clone());
                }
                ChartHostEvent::Settings(request) => {
                    if let Some(request) = legend_settings_request(*request) {
                        app.pending_settings_request = Some(request);
                    }
                }
                ChartHostEvent::RemoveStudy(study_id) => {
                    if let Some(study_id) = StudyInstanceId::try_from_u64(*study_id) {
                        app.pending_study_remove_request = Some(study_id);
                    }
                }
            }
            cx.notify();
        })
        .detach();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SymbolSubmitDecision {
    Select(usize),
    Search,
}

const fn symbol_submit_decision(result_count: usize, highlighted: usize) -> SymbolSubmitDecision {
    if highlighted < result_count {
        return SymbolSubmitDecision::Select(highlighted);
    }
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

/// A typed search highlights its first instrument, not the product header above it, so
/// Enter selects the best match.
fn first_selectable_instrument_index(entries: &[InstrumentMenuEntry]) -> Option<usize> {
    entries
        .iter()
        .position(|entry| matches!(entry.action, InstrumentMenuAction::Select(_)))
}

fn counted(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

struct HeaderState {
    theme: AerisTheme,
    provider: ProviderId,
    instrument_label: String,
    series_label: String,
    chart_type: ChartType,
    instruments: Vec<InstrumentMenuEntry>,
    symbol_input: Option<Entity<InputState>>,
    indicator_input: Entity<InputState>,
    time_zone_id: String,
    time_zone_clock: String,
    now_unix_nanos: i64,
    account_label: Option<String>,
    pending: HeaderPendingState,
    drawing_history: DrawingHistoryState,
    controls: HeaderControls,
    chart_link_group: u8,
    connection_state: FeedConnectionState,
    transport_rtt_nanos: Option<u64>,
    /// Runtime session projection for the selected product, shown by the header market dot.
    market_session: Option<aeris_contracts::MarketSessionStatus>,
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
    OrderEntry,
    TimeSales,
    Watchlist,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SidePanelVisibility(u8);

impl SidePanelVisibility {
    const PERSISTED: u8 = 0b1111;

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

    fn needs_order_book(self) -> bool {
        SidePanel::ALL
            .into_iter()
            .any(|panel| panel.needs_order_book() && self.contains(panel))
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

#[cfg(test)]
fn claim_once(claimed: &mut bool) -> bool {
    if *claimed {
        return false;
    }
    *claimed = true;
    true
}

impl SidePanel {
    const ALL: [Self; 4] = [
        Self::OrderBook,
        Self::OrderEntry,
        Self::TimeSales,
        Self::Watchlist,
    ];

    const fn title(self) -> &'static str {
        match self {
            Self::OrderBook => "Order Book",
            Self::OrderEntry => "Order Entry",
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
            Self::OrderEntry => 8,
        }
    }

    /// Market panels need a selected instrument before they can open.
    const fn requires_market(self) -> bool {
        matches!(self, Self::OrderBook | Self::OrderEntry | Self::TimeSales)
    }

    /// Panels that read the live order-book frame and so keep its depth demand open.
    const fn needs_order_book(self) -> bool {
        matches!(self, Self::OrderBook | Self::OrderEntry)
    }
}

struct MarketSummaryEntry {
    instrument: InstallProviderInstrument,
    descriptor: Option<aeris_market_runtime::InstrumentDescriptor>,
    worker: Option<MarketDataWorker>,
    previous_close: Option<i64>,
    last: Option<MarketBar>,
    tick_reference: Option<i64>,
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

/// Fixed decimal places for an instrument's summary prices: the precision of its tick when the
/// provider supplies one, else its maximum price decimals. A fixed count keeps the column from
/// changing width as prints land on and off the grid.
fn market_summary_price_decimals(instrument: &InstallProviderInstrument) -> Option<u32> {
    let scale = instrument.price_scale.min(18);
    if let Some(mut increment) = instrument
        .price_increment
        .filter(|increment| *increment > 0)
    {
        let mut decimals = scale;
        while decimals > 0 && increment % 10 == 0 {
            increment /= 10;
            decimals -= 1;
        }
        return Some(decimals);
    }
    instrument
        .contract_metadata
        .as_ref()
        .and_then(|metadata| metadata.price_max_decimals)
        .map(|decimals| decimals.min(scale))
}

/// The value in units of the presented decimals, rounded half away from zero; the raw value when
/// the instrument has no presentation grid.
fn market_summary_presented_units(value: i64, instrument: &InstallProviderInstrument) -> i128 {
    let Some(decimals) = market_summary_price_decimals(instrument) else {
        return i128::from(value);
    };
    let step = 10_i128.pow(instrument.price_scale.min(18) - decimals);
    let magnitude = (i128::from(value).abs() + step / 2) / step;
    if value < 0 { -magnitude } else { magnitude }
}

fn market_summary_price(value: i64, instrument: &InstallProviderInstrument) -> String {
    let Some(decimals) = market_summary_price_decimals(instrument) else {
        return market_price_text(value, instrument.price_scale);
    };
    let units = market_summary_presented_units(value, instrument);
    let factor = 10_i128.pow(decimals);
    let sign = if units < 0 { "-" } else { "" };
    let whole = units.abs() / factor;
    if decimals == 0 {
        return format!("{sign}{whole}");
    }
    let fraction = units.abs() % factor;
    let width = usize::try_from(decimals).unwrap_or(18);
    format!("{sign}{whole}.{fraction:0width$}")
}

fn market_summary_change(value: i64, instrument: &InstallProviderInstrument) -> String {
    let text = market_summary_price(value.abs(), instrument);
    if text.bytes().all(|byte| byte == b'0' || byte == b'.') {
        return text;
    }
    match value.cmp(&0) {
        std::cmp::Ordering::Greater => format!("+{text}"),
        std::cmp::Ordering::Less => format!("-{text}"),
        std::cmp::Ordering::Equal => text,
    }
}

/// How a presented last price moved from its previous distinct presented value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MarketSummaryTick {
    direction: std::cmp::Ordering,
    /// Byte offset in the presented text where the changed digits begin.
    changed_from: usize,
}

fn market_summary_tick(
    last_text: &str,
    last: i64,
    reference: i64,
    instrument: &InstallProviderInstrument,
) -> Option<MarketSummaryTick> {
    let direction = market_summary_presented_units(last, instrument)
        .cmp(&market_summary_presented_units(reference, instrument));
    if direction == std::cmp::Ordering::Equal {
        return None;
    }
    let reference_text = market_summary_price(reference, instrument);
    let changed_from = if last_text.len() == reference_text.len() {
        last_text
            .bytes()
            .zip(reference_text.bytes())
            .position(|(current, previous)| current != previous)
            .unwrap_or(0)
    } else {
        0
    };
    Some(MarketSummaryTick {
        direction,
        changed_from,
    })
}

#[derive(Clone)]
struct WatchlistRow {
    instrument: InstallProviderInstrument,
    previous_close: Option<i64>,
    last: Option<MarketBar>,
    /// The last close before its most recent presented change; colors the tick direction.
    tick_reference: Option<i64>,
    message: Option<String>,
    active: bool,
    /// The selected account has a working order or an open position here.
    trading_activity: bool,
    /// The row's flag color, a normalized CSS hex value.
    flag: Option<String>,
}

impl MarketSummaryEntry {
    fn new(
        instrument: InstallProviderInstrument,
        worker: Option<MarketDataWorker>,
        message: Option<String>,
        resource_class: ConsumerResourceClass,
    ) -> Self {
        Self {
            descriptor: engine_market_worker::initial_descriptor(&instrument),
            instrument,
            worker,
            previous_close: None,
            last: None,
            tick_reference: None,
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
                self.replace_last(Some(bar));
            }
            Some(current) => {
                self.previous_close = Some(current.close);
                self.replace_last(Some(bar));
            }
            None => self.replace_last(Some(bar)),
        }
    }

    /// Keeps the previous close as the tick reference only when the presented price changes, so
    /// sub-grid prints do not flip the tick direction.
    fn replace_last(&mut self, bar: Option<MarketBar>) {
        match (self.last, bar) {
            (Some(current), Some(next))
                if market_summary_presented_units(current.close, &self.instrument)
                    != market_summary_presented_units(next.close, &self.instrument) =>
            {
                self.tick_reference = Some(current.close);
            }
            (_, None) => self.tick_reference = None,
            _ => {}
        }
        self.last = bar;
    }

    fn apply_update(&mut self, update: ReplayStreamUpdate) {
        match update {
            ReplayStreamUpdate::Snapshot(snapshot) => {
                let mut bars = snapshot.bars().iter().rev();
                self.replace_last(bars.next().map(|item| *item.value()));
                self.previous_close = bars.next().map(|item| item.value().close);
            }
            ReplayStreamUpdate::Delta(delta) => self.apply_bar(*delta.item().value()),
            ReplayStreamUpdate::Tail(tail) => self.apply_bar(*tail.item().value()),
        }
        self.message = None;
    }

    fn apply_runtime_publication(
        &mut self,
        event: aeris_market_runtime::MarketConsumerEvent,
        received_unix_nanos: i64,
    ) {
        use aeris_market_runtime::MarketRuntimeEvent;
        let update = match event.value {
            MarketRuntimeEvent::SeriesSnapshot(snapshot)
                if snapshot.generation.0.get() == engine_market_worker::INITIAL_GENERATION =>
            {
                self.descriptor
                    .as_ref()
                    .ok_or_else(|| "market summary has no bound descriptor".to_string())
                    .and_then(|descriptor| {
                        engine_market_worker::replay_runtime_snapshot(
                            &snapshot,
                            descriptor,
                            received_unix_nanos,
                        )
                    })
                    .map(ReplayStreamUpdate::Snapshot)
            }
            MarketRuntimeEvent::SeriesUpdate(tail)
                if tail.generation.0.get() == engine_market_worker::INITIAL_GENERATION =>
            {
                engine_market_worker::replay_runtime_tail_update(&tail, received_unix_nanos)
                    .map(ReplayStreamUpdate::Tail)
            }
            _ => return,
        };
        match update {
            Ok(update) => self.apply_update(update),
            Err(_) => self.message = Some("Market data unavailable".to_string()),
        }
    }

    fn poll(&mut self) -> bool {
        let Some(worker) = &mut self.worker else {
            return false;
        };
        let (messages, disconnected) = worker.drain_messages_up_to(32);
        let changed = !messages.is_empty() || disconnected;
        for message in messages {
            match message {
                PaneProjectionEvent::LocalCommandOutcome(
                    aeris_desktop::market_worker::LocalCommandOutcome::BoundDescriptor {
                        generation,
                        descriptor,
                        ..
                    },
                ) if generation == engine_market_worker::INITIAL_GENERATION
                    && descriptor.instrument.provider == self.instrument.provider
                    && descriptor.instrument.instrument_id == self.instrument.instrument_id
                    && descriptor.entitlement_id == self.instrument.entitlement_id =>
                {
                    self.instrument = descriptor.instrument.clone();
                    self.descriptor = Some(*descriptor);
                }
                PaneProjectionEvent::RuntimeProjection(
                    aeris_desktop::market_worker::RuntimeProjection {
                        event: aeris_market_runtime::MarketConsumerEvent {
                            value: aeris_market_runtime::MarketRuntimeEvent::ProviderInstrumentSelection(selection),
                            ..
                        },
                        ..
                    },
                ) if aeris_desktop::market_worker::verified_terms_supersede(
                    &self.instrument,
                    &selection.instrument,
                ) => {
                    self.instrument = selection.instrument;
                    self.descriptor = engine_market_worker::refreshed_descriptor(
                        &self.instrument,
                        self.descriptor.take(),
                    );
                }
                PaneProjectionEvent::RuntimeProjection(
                    aeris_desktop::market_worker::RuntimeProjection {
                        event,
                        received_unix_nanos: Some(received_unix_nanos),
                        recovery_request_id: None,
                        selection_generation: None,
                    },
                ) => self.apply_runtime_publication(event, received_unix_nanos),
                PaneProjectionEvent::LocalFailure {
                    scope: aeris_desktop::market_worker::LocalFailureScope::Catalog,
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
    /// Top-right of the chart, clear of its price axis, over a chart the trader is reading.
    Corner,
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
        ChartNoticePlacement::Corner
    } else {
        ChartNoticePlacement::Center
    };
    let detail = bounded_status_detail(detail, state.label());
    match state {
        ChartState::Loading | ChartState::AwaitingData => Some(ChartSurfaceNotice {
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
        ChartState::HistoryUnavailable => Some(ChartSurfaceNotice {
            label: state.label(),
            detail: Some(
                if has_market_data {
                    "Showing live bars; history will backfill when the provider answers."
                } else {
                    "Waiting for live trades; history will backfill when the provider answers."
                }
                .to_string(),
            ),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChartFailure {
    UnsupportedProvider,
    NoDefault,
    Market,
    WorkerStopped,
}

impl ChartFailure {
    const fn detail(self) -> &'static str {
        match self {
            Self::NoDefault => "Choose an instrument",
            Self::UnsupportedProvider => {
                "This restored market provider is unavailable. Choose another market."
            }
            Self::Market => "Market chart could not be loaded. Try again.",
            Self::WorkerStopped => "Market worker stopped.",
        }
    }
}

fn chart_status_detail(
    chart_state: ChartState,
    connection_state: FeedConnectionState,
    failure: Option<ChartFailure>,
    pending_interval: Option<ChartInterval>,
) -> String {
    if chart_state == ChartState::Error
        && let Some(failure) = failure
    {
        return failure.detail().to_string();
    }
    match connection_state {
        FeedConnectionState::Stopped => "Market worker stopped.".to_string(),
        FeedConnectionState::Disconnected => "Market data offline.".to_string(),
        FeedConnectionState::Discovering => "Connecting to market data.".to_string(),
        FeedConnectionState::Authenticating => "Authenticating market data.".to_string(),
        FeedConnectionState::Recovering => "Reconnecting market data.".to_string(),
        FeedConnectionState::Streaming => match chart_state {
            ChartState::Loading => pending_interval.map_or_else(
                || "Waiting for a covering market snapshot.".to_string(),
                |interval| format!("Loading {} history.", interval.label()),
            ),
            ChartState::Stale | ChartState::Recovering => {
                "Waiting for a covering market snapshot.".to_string()
            }
            ChartState::AwaitingData => "Waiting for market data.".to_string(),
            ChartState::HistoryUnavailable => {
                "Waiting for live trades; history will backfill when the provider answers."
                    .to_string()
            }
            ChartState::Error => "Market chart could not be loaded. Try again.".to_string(),
            ChartState::Ready => String::new(),
        },
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

fn instrument_selector_label(selected: Option<(&str, &str)>) -> String {
    selected.map_or_else(
        || "Contract".to_string(),
        |(symbol, exchange)| format!("{symbol} / {exchange}"),
    )
}

fn terminal_instrument_label(app: &WorkspaceSurface) -> String {
    if let Some(descriptor) = app.descriptor.as_ref()
        && app.product.as_ref().is_some_and(|product| {
            product.provider == descriptor.instrument.provider
                && product.instrument_id == descriptor.instrument.instrument_id
                && product.entitlement_id == descriptor.entitlement_id
        })
    {
        return descriptor.display_symbol.clone();
    }
    if provider_presentation(app.provider).is_some() {
        return app.product.as_ref().map_or_else(
            || "Select market".to_string(),
            |product| product.display_symbol.clone(),
        );
    }
    instrument_selector_label(app.symbol_entry.browser().selected().map(|selection| {
        (
            selection.instrument.symbol.as_str(),
            selection.instrument.exchange.as_str(),
        )
    }))
}

fn series_selector_label(selected: ChartInterval) -> String {
    selected.label().to_string()
}

#[derive(Clone, Copy)]
struct HeaderControls(u8);

impl HeaderControls {
    const INSTRUMENT: u8 = 1;
    const SERIES: u8 = 2;
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
            controls |= Self::SERIES;
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
    chart_failure: Option<ChartFailure>,
    replay_label: String,
    worker_label: String,
    subscription_id: String,
    connection_state: Option<FeedConnectionState>,
    provider: ProviderId,
    product: Option<InstallProviderInstrument>,
}

fn terminal_startup_state(startup: MarketWorkerStartup) -> TerminalStartupState {
    match startup {
        MarketWorkerStartup::Loading(startup) => {
            if startup.product.provider.is_empty() {
                return TerminalStartupState {
                    chart: None,
                    chart_state: ChartState::Loading,
                    chart_failure: None,
                    replay_label: "waiting for the runtime default".to_string(),
                    worker_label: startup.worker_label,
                    subscription_id: startup.subscription_id,
                    connection_state: Some(FeedConnectionState::Discovering),
                    provider: ProviderId::unsupported(),
                    product: None,
                };
            }
            let Some(provider) = ProviderId::find(startup.product.provider.as_str()) else {
                let message = ChartFailure::UnsupportedProvider.detail().to_string();
                return TerminalStartupState {
                    chart: None,
                    chart_state: ChartState::Error,
                    chart_failure: Some(ChartFailure::UnsupportedProvider),
                    replay_label: message.clone(),
                    worker_label: startup.worker_label,
                    subscription_id: startup.subscription_id,
                    connection_state: None,
                    provider: ProviderId::unsupported(),
                    product: None,
                };
            };
            TerminalStartupState {
                chart: None,
                chart_state: ChartState::Loading,
                chart_failure: None,
                replay_label: "waiting for a covering market snapshot".to_string(),
                worker_label: startup.worker_label,
                subscription_id: startup.subscription_id,
                connection_state: Some(FeedConnectionState::Discovering),
                provider,
                product: Some(startup.product),
            }
        }
    }
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

mod catalog_projection;
mod trading_store;
mod workspace_surface;

fn catalog_rejection_message(
    reason: ProviderCatalogRejectionReason,
    command: ProviderCatalogCommand,
    provider: ProviderId,
) -> String {
    let provider_name = provider.display();
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
        ProviderCatalogEvent::CatalogReady { provider } => provider,
        ProviderCatalogEvent::SearchCompleted(result) => &result.provider,
        ProviderCatalogEvent::SelectionInstalled { instrument, .. } => &instrument.provider,
        ProviderCatalogEvent::CommandRejected { rejection, .. }
        | ProviderCatalogEvent::ScreenRejected(rejection) => &rejection.provider,
        ProviderCatalogEvent::ScreenCompleted(screen) => &screen.provider,
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
    use super::{
        AccountRefreshExitClaim, retain_account_refresh_quiesce_for_exit,
        wait_for_account_restore_readiness,
    };
    use aeris_account_runtime::AccountRestoreReadiness;
    use std::{
        cell::Cell,
        sync::atomic::{AtomicUsize, Ordering},
        time::{Duration, Instant},
    };

    static EXIT_WAIT_CALLS: AtomicUsize = AtomicUsize::new(0);

    struct StalledRefreshQuiesce;

    impl AccountRefreshExitClaim for StalledRefreshQuiesce {
        fn wait_for_exit(&self) -> Result<(), String> {
            EXIT_WAIT_CALLS.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(10));
            Err("fixture refresh stayed active".to_string())
        }

        fn retain_for_exit(self) {
            panic!("a stalled refresh cannot be retained");
        }
    }

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

    #[test]
    fn stalled_account_quiesce_returns_after_one_bounded_wait() {
        EXIT_WAIT_CALLS.store(0, Ordering::Relaxed);
        let started = Instant::now();
        let error =
            retain_account_refresh_quiesce_for_exit(Ok(StalledRefreshQuiesce), "fixture exit")
                .expect_err("stalled quiesce fails");

        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(EXIT_WAIT_CALLS.load(Ordering::Relaxed), 1);
        assert!(error.contains("fixture exit"));
        assert!(error.contains("fixture refresh stayed active"));
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
        assert_ne!(
            parser_valid_but_unbootable.workspace_tabs,
            [] as [aeris_contracts::WorkspaceTabState; 0]
        );
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
    let persistence = aeris_desktop::chart_chrome::DesktopPersistence::new(
        aeris_platform_runtime::native_data_root().map_err(|error| error.to_string())?,
    );
    let workspace = local_state::load_workspace_for_readiness(&persistence.workspace_path())
        .map_err(|error| format!("candidate workspace restore failed: {error}"))?;
    validate_workspace_boot_for_readiness(&workspace)?;
    let account_service = aeris_account_runtime::AccountService::new(
        aeris_account_runtime::AccountServiceConfig::production(),
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
                )?;
                return Err(error);
            }
        }
    };
    let sign_in_required = aeris_desktop::account::sign_in_required();
    if sign_in_required {
        account_service.start_restore();
    }
    let result = (|| {
        if sign_in_required {
            wait_for_account_restore_readiness(ACCOUNT_RESTORE_READINESS_TIMEOUT, || {
                account_service.restore_readiness()
            })?;
        }
        let market = aeris_market_runtime::MarketService::start()?;
        let trading = start_trading_service().map_err(|error| error.to_string())?;
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
        let (validation_mode, log_path, vault_namespace) = readiness_data_root_fields()?;
        let report = LifecycleReadinessReport {
            schema_version: 6,
            release_identity: release.release_identity,
            install_generation: release.install_generation,
            desktop_process_id: std::process::id(),
            workspace_revision: workspace.workspace_revision,
            provider_count: status.providers.len(),
            workspace_restored: true,
            validation_mode,
            instance_lock: ReadinessFlag(true),
            log_path,
            vault_namespace,
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
        aeris_platform_runtime::fs::write_file_durably(std::path::Path::new(&report_path), &encoded)
            .map_err(|_| "candidate readiness report could not be written".to_string())
    })();
    let quiesce_result = retain_account_refresh_quiesce_for_exit(
        account_service.begin_refresh_quiesce(),
        "candidate readiness exit",
    );
    match (result, quiesce_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(operation_error), Err(quiesce_error)) => {
            Err(format!("{operation_error}; {quiesce_error}"))
        }
    }
}

fn readiness_data_root_fields() -> Result<(ReadinessFlag, String, &'static str), String> {
    let root = aeris_platform_runtime::active_data_root()
        .map_err(|_| "candidate data root policy is unavailable".to_string())?;
    let log_path = root
        .path()
        .join("logs")
        .join("aeris.log")
        .to_string_lossy()
        .into_owned();
    Ok((
        ReadinessFlag(root.is_validation()),
        log_path,
        if root.is_validation() {
            "validation"
        } else {
            "production"
        },
    ))
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
    validation_mode: ReadinessFlag,
    instance_lock: ReadinessFlag,
    log_path: String,
    vault_namespace: &'static str,
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

#[derive(serde::Serialize)]
#[serde(transparent)]
struct ReadinessFlag(bool);

fn start_trading_service()
-> Result<aeris_trading_runtime::TradingService, aeris_trading_runtime::StoreError> {
    let database_path = aeris_platform_runtime::native_data_root()
        .map_err(|error| {
            aeris_trading_runtime::StoreError::Other(format!(
                "trading data root is unavailable: {error}"
            ))
        })?
        .join("trading")
        .join("trading.sqlite3");
    aeris_trading_runtime::TradingService::start(aeris_trading_runtime::TradingServiceConfig {
        database_path,
        retention: aeris_trading_runtime::TradingRetention::default(),
        clock: std::sync::Arc::new(aeris_trading_runtime::clock::SystemClock::default()),
        execution: aeris_trading_runtime::ExecutionPolicy::default(),
    })
}

fn trading_corruption_message(
    database: &std::path::Path,
    newest_backup: Option<&std::path::Path>,
) -> String {
    let backup = newest_backup.map_or_else(
        || "No migration backup is available.".to_string(),
        |path| format!("Newest migration backup:\n{}", path.display()),
    );
    format!(
        "Aeris cannot open the trading database because it is corrupt:\n{}\n\n{backup}\n\n\
         Select Yes to quarantine the database and its WAL sidecars. Aeris will exit; \
         a new database is created only after you restart.\n\n\
         Select No to exit without changing any files.",
        database.display()
    )
}

fn quarantine_trading_database(database: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let directory = database
        .parent()
        .ok_or_else(|| "trading database has no parent directory".to_string())?;
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "system clock precedes Unix epoch".to_string())?
        .as_millis();
    let quarantine = directory.join(format!("trading.corrupt-{unix}.sqlite3"));
    for suffix in ["-wal", "-shm"] {
        let source = std::path::PathBuf::from(format!("{}{suffix}", database.display()));
        if source.exists() {
            let destination = std::path::PathBuf::from(format!("{}{suffix}", quarantine.display()));
            aeris_platform_runtime::fs::move_file(&source, &destination).map_err(|error| {
                format!(
                    "trading sidecar {} could not be quarantined: {error}",
                    source.display()
                )
            })?;
        }
    }
    aeris_platform_runtime::fs::move_file(database, &quarantine).map_err(|error| {
        format!(
            "trading database {} could not be quarantined: {error}",
            database.display()
        )
    })?;
    Ok(quarantine)
}

fn handle_trading_corruption(
    account_service: &aeris_account_runtime::AccountService,
    path: &std::path::Path,
    newest_backup: Option<&std::path::Path>,
    detail: &str,
) -> ! {
    diagnostic!(
        "Aeris trading database integrity check failed for {}: {detail}",
        path.display()
    );
    let message = trading_corruption_message(path, newest_backup);
    match aeris_platform_runtime::confirm_message_box("Aeris Terminal", &message) {
        Ok(true) => match quarantine_trading_database(path) {
            Ok(quarantine) => {
                diagnostic!(
                    "Aeris corrupt trading database quarantined at {}",
                    quarantine.display()
                );
                fatal_startup(
                    account_service,
                    "The corrupt trading database was quarantined. Restart Aeris to create a new database.",
                );
            }
            Err(error) => {
                diagnostic!("Aeris trading database quarantine failed: {error}");
                fatal_startup(
                    account_service,
                    "The corrupt trading database could not be quarantined and was not recreated.",
                );
            }
        },
        Ok(false) => {
            fatal_startup(
                account_service,
                "The corrupt trading database was left untouched. Aeris cannot start.",
            );
        }
        Err(error) => {
            diagnostic!("Aeris corruption confirmation dialog failed: {error}");
            fatal_startup(
                account_service,
                "Aeris could not obtain confirmation. The corrupt trading database was left untouched.",
            );
        }
    }
}

#[cfg(test)]
mod trading_corruption_tests {
    use super::{quarantine_trading_database, trading_corruption_message};

    #[test]
    fn corruption_message_names_database_backup_and_requires_confirmation() {
        let database = std::path::Path::new("trading/trading.sqlite3");
        let backup = std::path::Path::new("trading/backups/trading-v18-123.sqlite3");
        let message = trading_corruption_message(database, Some(backup));
        assert!(message.contains(&database.display().to_string()));
        assert!(message.contains(&backup.display().to_string()));
        assert!(message.contains("Select Yes"));
        assert!(message.contains("Select No"));
        assert!(message.contains("only after you restart"));
    }

    #[test]
    fn confirmed_quarantine_moves_database_and_sidecars_together() {
        let directory = std::env::temp_dir().join(format!(
            "aeris-corruption-quarantine-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("fixture directory");
        let database = directory.join("trading.sqlite3");
        std::fs::write(&database, b"database").expect("database fixture");
        std::fs::write(format!("{}-wal", database.display()), b"wal").expect("WAL fixture");
        std::fs::write(format!("{}-shm", database.display()), b"shm").expect("SHM fixture");

        let quarantine = quarantine_trading_database(&database).expect("quarantine succeeds");

        assert!(!database.exists());
        assert_eq!(
            std::fs::read(&quarantine).expect("database moved"),
            b"database"
        );
        assert_eq!(
            std::fs::read(format!("{}-wal", quarantine.display())).expect("WAL moved"),
            b"wal"
        );
        assert_eq!(
            std::fs::read(format!("{}-shm", quarantine.display())).expect("SHM moved"),
            b"shm"
        );
        std::fs::remove_dir_all(directory).expect("fixture cleanup");
    }
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

/// Every window opens maximized; the centered 1280x820 bounds are where it restores to.
fn desktop_window_options(window_index: usize, cx: &mut App) -> WindowOptions {
    let mut bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);
    let offset = if window_index == 0 { px(0.0) } else { px(48.0) };
    bounds.origin.x += offset;
    bounds.origin.y += offset;
    WindowOptions {
        window_bounds: Some(WindowBounds::Maximized(bounds)),
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
                        if !app.chrome.is_overlay_open(ChromeOverlay::Instrument) {
                            return;
                        }
                        if app.submit_symbol_input(app_cx)
                            && symbol_menu_closes_after_selection(app.symbol_entry.target())
                        {
                            app.close_chrome_overlay(window, app_cx);
                        }
                    });
                }
                SymbolInputAction::Search => {
                    terminal.update(cx, |app, cx| {
                        if !app.chrome.is_overlay_open(ChromeOverlay::Instrument) {
                            return;
                        }
                        let typed = app
                            .symbol_entry
                            .input()
                            .is_some_and(|input| !input.read(cx).value().trim().is_empty());
                        app.chrome.set_list_keyboard(typed);
                        let selection = if typed {
                            0
                        } else {
                            current_instrument_menu_index(&app.instrument_entries(cx)).unwrap_or(0)
                        };
                        app.chrome.set_selection(selection);
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
                    // Enter adds only the highlighted row; with nothing highlighted it is a no-op.
                    let entry = terminal.update(cx, |app, cx| {
                        app.chrome
                            .list_keyboard_active()
                            .then(|| {
                                app.indicator_menu_entries(cx)
                                    .get(app.chrome.selection())
                                    .copied()
                            })
                            .flatten()
                    });
                    if let Some(entry) = entry {
                        let added =
                            terminal.update(cx, |app, cx| app.add_indicator_menu_entry(entry, cx));
                        if added {
                            terminal.update(cx, |app, app_cx| {
                                app.close_chrome_overlay(window, app_cx);
                            });
                        }
                    }
                } else if matches!(event, InputEvent::Change) {
                    let typed = !input.read(cx).value().trim().is_empty();
                    terminal.update(cx, |app, cx| {
                        app.chrome.set_list_keyboard(typed);
                        app.chrome.set_selection(0);
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
                        if !app.chrome.is_overlay_open(ChromeOverlay::QuickTimeframe) {
                            return;
                        }
                        app.submit_quick_timeframe(window, app_cx);
                    });
                }
                InputEvent::Change => {
                    terminal.update(cx, |app, app_cx| {
                        if !app.chrome.is_overlay_open(ChromeOverlay::QuickTimeframe) {
                            return;
                        }
                        let query = input.read(app_cx).value();
                        app.chrome
                            .retract_quick_timeframe_error_after_edit(query.as_ref());
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
                        if !app.chrome.is_overlay_open(ChromeOverlay::TimeZone) {
                            return;
                        }
                        app.apply_highlighted_time_zone(window, app_cx);
                    });
                }
                InputEvent::Change => {
                    terminal.update(cx, |app, app_cx| {
                        if !app.chrome.is_overlay_open(ChromeOverlay::TimeZone) {
                            return;
                        }
                        app.chrome.set_selection(0);
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
        aeris_trading::OrderType::Market | aeris_trading::OrderType::RestingAt => return None,
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
                    if surface.selected_account_lock_reason(cx).is_some() {
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
                let service = click_workspace.read(cx).lifecycle.trading().clone();
                let feedback = click_workspace
                    .read(cx)
                    .lifecycle
                    .trading_feedback()
                    .clone();
                let pane = click_workspace.read(cx).trading_pane_token.clone();
                aeris_desktop::trading::dispatch_simulated_order_at_price(
                    &service,
                    &feedback,
                    &pane,
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
    subscribe_order_book_trading_drop(workspace, order_book, window, cx);
}

fn subscribe_order_book_trading_drop(
    workspace: &Entity<WorkspaceSurface>,
    order_book: &Entity<ReadOnlyOrderBookView>,
    window: &mut Window,
    cx: &mut App,
) {
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
                let (account_id, store) = {
                    let surface = drop_workspace.read(cx);
                    if surface.selected_account_lock_reason(cx).is_some() {
                        return;
                    }
                    let Some(account_id) =
                        surface.trading_pnl.order_entry.selected_account_id.clone()
                    else {
                        return;
                    };
                    let Some(store) = surface.lifecycle.trading_store() else {
                        return;
                    };
                    (account_id, store)
                };
                let Some(frame) = drop_order_book.read(cx).frame().cloned() else {
                    return;
                };
                let store = store.read(cx);
                let Ok(instrument) =
                    aeris_instruments::InstrumentId::try_new(frame.instrument_id.clone())
                else {
                    return;
                };
                if store.current().is_none() {
                    return;
                }
                let Some(order) = store
                    .pane_view(&account_id, &instrument)
                    .orders()
                    .find(|order| {
                        order.status.is_open()
                            && order_book_row_price(order).is_some_and(|(side, price)| {
                                side == event.source_side && price.units() == event.source_price
                            })
                    })
                    .cloned()
                else {
                    return;
                };
                let Ok(target_price) =
                    aeris_trading::FixedPoint::try_new(event.target_price, frame.price_scale)
                else {
                    return;
                };
                let service = drop_workspace.read(cx).lifecycle.trading().clone();
                let feedback = drop_workspace.read(cx).lifecycle.trading_feedback().clone();
                let pane = drop_workspace.read(cx).trading_pane_token.clone();
                aeris_desktop::trading::modify_simulated_order_at_price(
                    &service,
                    &feedback,
                    &pane,
                    aeris_desktop::trading::SimulatedOrderModification {
                        client_order_id: order.client_order_id,
                        expected_revision: order.revision,
                        order_type: order.order_type,
                        time_in_force: order.time_in_force,
                        price: target_price,
                    },
                    &frame,
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
    let order_quantity = order_quantity::OrderQuantityControls::new(
        TradingOrderEntryState::default().quantity,
        window,
        cx,
    );
    let order_ticket = order_ticket::OrderTicketControls::new(
        TradingOrderEntryState::default().ticket,
        window,
        cx,
    );
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
            order_quantity,
            order_ticket,
            chart_chrome,
            restored.chart,
        )
    });
    let order_book = workspace.read(cx).order_book.clone();
    subscribe_order_book_trading(&workspace, &order_book, window, cx);
    if let Some((visibility, width, split)) = restored.side_panel {
        workspace.update(cx, |surface, surface_cx| {
            surface.chrome.restore_side_panel_size(width, split);
            surface.apply_side_panels(SidePanelVisibility::from_persisted(visibility), surface_cx);
            surface.persistence.discard_dirty();
        });
    }
    lifecycle.register_terminal(&workspace);
    subscribe_symbol_input(search_input, &workspace, window, cx);
    subscribe_indicator_input(&indicator_search_input, &workspace, window, cx);
    subscribe_timeframe_input(&timeframe_search_input, &workspace, window, cx);
    subscribe_time_zone_input(&time_zone_search_input, &workspace, window, cx);
    order_quantity::subscribe_order_quantity_inputs(&workspace, window, cx);
    order_ticket::subscribe_order_ticket_inputs(&workspace, window, cx);
    workspace
}

struct WorkspacePane {
    id: u64,
    consumer_id: u64,
    surface: Entity<WorkspaceSurface>,
}

struct WorkspaceTab {
    id: u64,
    /// The name of the saved layout the tab shows.
    label: String,
    saved_layout_id: u64,
    /// The arrangement this tab last autosaved, so two tabs showing one layout with different
    /// arrangements do not overwrite each other on every save; the last change wins.
    autosaved_snapshot: Option<WorkspaceTabState>,
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

/// The color a newly set watchlist flag starts with, as the hex value the flag stores.
fn default_watchlist_flag_color(theme: &AerisTheme) -> String {
    theme.colors.swatch_default.css_hex()
}

/// One watchlist row as the desktop owns it: the instrument and the user's flag.
#[derive(Clone, Debug, PartialEq)]
struct WatchlistEntry {
    instrument: InstallProviderInstrument,
    /// A normalized CSS hex color; `None` while unflagged.
    flag: Option<String>,
}

impl WatchlistEntry {
    fn is(&self, provider: &str, instrument_id: &str) -> bool {
        self.instrument.provider == provider && self.instrument.instrument_id == instrument_id
    }
}

/// Session-only watchlist flag UI: the color filter, its menu and the open flag picker.
#[derive(Default)]
struct WatchlistFlagState {
    /// Shows only the watchlist rows flagged with this color.
    filter: Option<String>,
    filter_menu_open: bool,
    picker: Option<WatchlistFlagPicker>,
}

/// The open color picker of one watchlist row's flag.
#[derive(Clone)]
struct WatchlistFlagPicker {
    provider: String,
    instrument_id: String,
    input: Entity<InputState>,
    error: Option<String>,
}

impl WatchlistFlagPicker {
    fn is_for(&self, entry: &WatchlistEntry) -> bool {
        entry.is(&self.provider, &self.instrument_id)
    }
}

/// Indices of the entries the watchlist shows: all of them, or only those flagged with
/// `filter`. The row whose flag picker is open stays shown while its color changes.
fn visible_watchlist_indices(
    entries: &[WatchlistEntry],
    filter: Option<&str>,
    picker: Option<&WatchlistFlagPicker>,
) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            filter.is_none_or(|filter| entry.flag.as_deref() == Some(filter))
                || picker.is_some_and(|picker| picker.is_for(entry))
        })
        .map(|(index, _)| index)
        .collect()
}

/// The distinct flag colors in watchlist order, which the flag filter offers.
fn watchlist_flag_colors(entries: &[WatchlistEntry]) -> Vec<String> {
    let mut colors: Vec<String> = Vec::new();
    for flag in entries.iter().filter_map(|entry| entry.flag.as_ref()) {
        if !colors.contains(flag) {
            colors.push(flag.clone());
        }
    }
    colors
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
    /// Opens the capture chart submenu; the capture is taken by its Copy or Save rows.
    CaptureMenu,
    CopyCapture,
    SaveCapture,
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
    capture_flyout_open: bool,
    copy_price: Option<SharedString>,
    copy_feedback: Option<ChartCopyFeedback>,
}

/// The "Copied" confirmation on the row whose copy finished; the generation fences the timer
/// that closes the menu afterwards.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChartCopyFeedback {
    action: ChartContextAction,
    generation: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ChartSettingsSection {
    #[default]
    Symbol,
    Canvas,
    Legend,
    Trading,
}

impl ChartSettingsSection {
    const ALL: [Self; 4] = [Self::Symbol, Self::Canvas, Self::Legend, Self::Trading];

    const fn label(self) -> &'static str {
        match self {
            Self::Symbol => "Symbol",
            Self::Canvas => "Canvas",
            Self::Legend => "Legend",
            Self::Trading => "Trading",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Symbol => "Colors and style of the price series",
            Self::Canvas => "Background, grid and crosshair",
            Self::Legend => "What the legends show over the chart",
            Self::Trading => "Orders, positions and fills on the chart",
        }
    }

    const fn icon(self) -> HugeIcon {
        match self {
            Self::Symbol => HugeIcon::Chart,
            Self::Canvas => HugeIcon::View,
            Self::Legend => HugeIcon::DataPanel,
            Self::Trading => HugeIcon::OrderEntry,
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
    AreaBottom,
    BaselineTop,
    BaselineBottom,
    Background,
    Text,
    Grid,
    Crosshair,
    AxisBorder,
    AxisText,
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
            Self::AreaBottom => "Area bottom",
            Self::BaselineTop => "Above baseline",
            Self::BaselineBottom => "Below baseline",
            Self::Background => "Background",
            Self::Text => "Text",
            Self::Grid => "Grid",
            Self::Crosshair => "Crosshair",
            Self::AxisBorder => "Axis border",
            Self::AxisText => "Axis text",
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
    /// Instrument ticks per footprint row; zero is automatic.
    FootprintTicksPerRow(u32),
    ToggleOrderManagementLines,
    ToggleExecutionMarks,
    ToggleExtendOrderLinesLeft,
    ToggleSymbolLegend,
    ToggleSymbolLegendValues,
    ToggleIndicatorLegend,
    ToggleIndicatorLegendValues,
    /// Applies to every chart: the Buy / quantity / Sell bar under the symbol legend.
    ToggleChartTradeButtons,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChartTradingVisibilitySettings {
    show_order_management_lines: bool,
    show_execution_marks: bool,
    extend_order_lines_left: bool,
}

impl Default for ChartTradingVisibilitySettings {
    fn default() -> Self {
        Self {
            show_order_management_lines: true,
            show_execution_marks: true,
            extend_order_lines_left: true,
        }
    }
}

#[derive(Clone, Debug)]
struct ChartSettingsSnapshot {
    chart_type: ChartType,
    appearance: ChartAppearanceSettings,
    /// What theme-following appearance and axis colors paint under the chart's theme.
    theme_defaults: ChartThemeDefaults,
    axis: ChartAxisAppearance,
    series_colors: ChartSeriesColors,
    crosshair_mode: u8,
    order_flow: OrderFlowSettings,
    time_zone: String,
    trading_visibility: ChartTradingVisibilitySettings,
    chart_trade_buttons_visible: bool,
    legend: ChartLegendOptions,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ChartSettingsTemplateOverlay {
    #[default]
    Closed,
    Menu,
    /// The menu with its saved-templates flyout beside it.
    TemplatesFlyout,
    SaveDialog,
    /// Confirming deletion of the saved template at this index.
    DeleteDialog(usize),
}

impl ChartSettingsTemplateOverlay {
    const fn menu_open(self) -> bool {
        matches!(self, Self::Menu | Self::TemplatesFlyout)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspaceShellKind {
    Window,
    Tabs,
}

/// Chart-only fullscreen state; see [`TerminalApp::toggle_chart_fullscreen`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChartFullscreen {
    /// Entering put the window into fullscreen, so leaving takes it out again.
    restores_window: bool,
}

/// Shell menus are small booleans by design; the account dropdown joins the
/// two chart-menu options rather than growing a separate menu stack.
#[allow(clippy::struct_excessive_bools)]
struct TerminalApp {
    tabs: TabStrip,
    theme: AerisTheme,
    drawing_tool_menu: DrawingToolMenu,
    /// Pointer offset from the favorites toolbar origin while its grip is dragged.
    drawing_favorites_grab: Option<gpui::Point<Pixels>>,
    window_active: bool,
    frame_poll_gate: frame_poll_gate::FramePollGate,
    market_frame_wake: UiWake,
    market_wake_listener_started: Option<()>,
    chrome_focus: FocusHandle,
    lifecycle: DesktopLifecycle,
    trading_store: Entity<trading_store::TradingStore>,
    workspace_factory: Option<engine_market_worker::WorkspaceMarketFactory>,
    watchlist: Vec<WatchlistEntry>,
    watchlist_flags: WatchlistFlagState,
    market_summaries: BTreeMap<MarketSummaryKey, MarketSummaryEntry>,
    persisted_watchlist: Vec<WorkspaceWatchlistEntryState>,
    watchlist_persistence_dirty: bool,
    workspace_persistence: Option<WorkspaceLayoutPersistence>,
    persisted_layout: Vec<WorkspaceTabState>,
    persisted_active_workspace_id: u64,
    layout_library: LayoutLibrary,
    persisted_layout_library_revision: u64,
    /// The working drawing set of every saved layout an open tab shows, shared by those tabs.
    layout_drawings: BTreeMap<u64, LayoutDrawings>,
    workspace_error: Option<String>,
    watchlist_drag: Option<WatchlistDragState>,
    watchlist_scroll: ScrollHandle,
    chart_context_menu: Option<ChartContextMenu>,
    chart_context_copy_feedback_generation: u64,
    chart_settings_menu: Option<ChartContextMenu>,
    chart_settings_placement: chart_context_menus::FloatingPanelPlacement,
    chart_settings_section: ChartSettingsSection,
    chart_settings_color_picker: Option<ChartColorPickerState>,
    /// The settings dropdown that is open, by its row id.
    chart_settings_select: Option<&'static str>,
    chart_settings_template_overlay: ChartSettingsTemplateOverlay,
    chart_settings_template_name: Option<Entity<InputState>>,
    chart_settings_template_error: Option<String>,
    chart_settings_templates: Vec<WorkspaceChartSettingsTemplateState>,
    chart_settings_persistence_dirty: bool,
    /// User-named drawing style templates of every kind, shared by every chart.
    drawing_templates: Vec<WorkspaceDrawingTemplateState>,
    drawing_templates_persistence_dirty: bool,
    drawing_context_toolbar: DrawingContextToolbar,
    /// Avatar click point the open platform menu is anchored under.
    platform_menu_anchor: Option<gpui::Point<Pixels>>,
    pages: app_navigation::AppPages,
    frameless_title_bar: FramelessTitleBar,
    fullscreen_hint: FullscreenHint,
    toaster: Toaster,
    /// Shift+F chart-only fullscreen: only the workspace's charts show. Holds whether entering it
    /// also put the window into fullscreen, so leaving restores the window as it was.
    chart_fullscreen: Option<ChartFullscreen>,
    bottom_panel: bottom_panel::BottomPanelState,
    profile_refresh_on_activation: bool,
    command_palette_input: Entity<InputState>,
    command_palette_open: bool,
    command_palette_selection: usize,
    command_palette_message: Option<String>,
    keyboard_trading: trading_hotkeys::KeyboardTrading,
    keyboard_shortcuts_open: bool,
    data_connections: DataConnections,
    provider_login: provider_login::ProviderLoginFlow,
    linked_sync_revisions: BTreeMap<String, u64>,
    event_risk_dispatches: BTreeMap<String, i64>,
    next_event_risk_check: Instant,
    updater: Option<DesktopUpdater>,
    update_restart_persistence_pending: bool,
    chart_chrome: chart_chrome::ChartChromePreferences,
    window_move_pending: bool,
    closing: bool,
    /// The start page, layout menu and layout dialogs.
    layouts_ui: layouts_view::LayoutsUi,
}

fn workspace_layout_tabs(workspaces: &[WorkspaceTab], cx: &App) -> Vec<WorkspaceTabState> {
    workspaces
        .iter()
        .map(|workspace| workspace_tab_state(workspace, cx))
        .collect()
}

fn workspace_tab_state(workspace: &WorkspaceTab, cx: &App) -> WorkspaceTabState {
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
                let viewport = surface.chart.as_ref().map_or(
                    surface.persistence.restored_viewport(),
                    |chart| {
                        let chart = chart.read(cx);
                        durable_workspace_viewport(
                            surface.persistence.restored_viewport(),
                            chart.has_market_data(),
                            chart.is_at_latest(),
                            chart.visible_time_range_unix_nanos(),
                        )
                    },
                );
                Some(WorkspacePaneState {
                    pane_id: pane.id,
                    consumer_id: pane.consumer_id,
                    kind: WorkspacePaneKind::Chart as i32,
                    instrument: Some(instrument.clone()),
                    series: None,
                    viewport_start_unix_nanos: viewport.map(|range| range.0),
                    viewport_end_unix_nanos: viewport.map(|range| range.1),
                    size_basis_points: pane_weights
                        .iter()
                        .find_map(|(pane_id, basis)| (*pane_id == pane.id).then_some(*basis))
                        .unwrap_or(1),
                    generation: workspace.generation.max(1),
                    chart: surface.workspace_chart_state(cx),
                    side_panel_visibility: u32::from(surface.chrome.side_panels().0),
                    side_panel_width: surface
                        .chrome
                        .side_panel_width()
                        .round()
                        .to_u32()
                        .unwrap_or(400),
                    side_panel_split_basis_points: surface.chrome.side_panel_split_basis_points(),
                    interval: surface.interval.label().to_string(),
                })
            })
            .collect(),
        active_pane_id: workspace.panes[workspace.active_pane].id,
        generation: workspace.generation.max(1),
        layout: Some(workspace_layout_state(&layout)),
        saved_layout_id: workspace.saved_layout_id,
    }
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

mod workspace_tabs;

mod layout_tabs;

struct ConfiguredDesktop {
    market_workers: Vec<(MarketWorkerStartup, MarketDataWorker)>,
    workspace_panes: Vec<engine_market_worker::WorkspaceMarketPane>,
    restored_workspace: WorkspaceState,
    workspace_factory: Option<engine_market_worker::WorkspaceMarketFactory>,
    layout: DesktopLayout,
    chart_chrome: chart_chrome::ChartChromePreferences,
    workspace_notice: Option<String>,
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

fn configure_desktop_state(
    services: &aeris_desktop::services::DesktopServices,
) -> ConfiguredLifecycle {
    ConfiguredLifecycle {
        workspace: local_state::load_workspace(&services.persistence.workspace_path()),
    }
}

fn configured_market_workers(
    command_arguments: Vec<std::ffi::OsString>,
    services: &aeris_desktop::services::DesktopServices,
) -> Result<Option<ConfiguredDesktop>, String> {
    let mut arguments = command_arguments.into_iter();
    let command = arguments.next();
    let layout = DesktopLayout::WorkspaceTabs;
    let workspace_factory;
    let (market_workers, workspace_panes, lifecycle, workspace_notice) =
        if let Some(argument) = command {
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
            if argument == "--workspace-tabs" {
                if arguments.next().is_some() {
                    diagnostic!("usage: aeris_desktop --workspace-tabs");
                    exit_after_account_refresh_quiesce(&services.account_service, 2);
                }
                let lifecycle = configure_desktop_state(services);
                let group = engine_market_worker::start_workspace_tabs(
                    &services.market,
                    &lifecycle.workspace,
                    services.trading.clone(),
                    Arc::clone(&services.clock),
                )?;
                let workspace_notice = group.restore_notices.join("\n");
                workspace_factory = Some(group.factory);
                (Vec::new(), group.initial, lifecycle, workspace_notice)
            } else {
                diagnostic!("unsupported argument: {}", argument.to_string_lossy());
                exit_after_account_refresh_quiesce(&services.account_service, 2);
            }
        } else {
            // The installed desktop is a persisted workspace, not the legacy
            // single-chart/window bootstrap. Starting through the workspace group
            // restores pane identity, symbol, interval, viewport and layout and
            // mounts the persistence owner used for subsequent durable changes.
            let lifecycle = configure_desktop_state(services);
            let group = engine_market_worker::start_workspace_tabs(
                &services.market,
                &lifecycle.workspace,
                services.trading.clone(),
                Arc::clone(&services.clock),
            )?;
            let workspace_notice = group.restore_notices.join("\n");
            workspace_factory = Some(group.factory);
            (Vec::new(), group.initial, lifecycle, workspace_notice)
        };
    Ok(Some(ConfiguredDesktop {
        market_workers,
        workspace_panes,
        restored_workspace: lifecycle.workspace.clone(),
        workspace_factory,
        layout,
        chart_chrome: services.persistence.load_chart_chrome_preferences(),
        workspace_notice: (!workspace_notice.is_empty()).then_some(workspace_notice),
    }))
}

#[cfg(debug_assertions)]
fn take_forced_diagnostics_panic(arguments: &mut Vec<std::ffi::OsString>) -> bool {
    let requested = arguments
        .iter()
        .any(|argument| argument == "--force-diagnostics-panic");
    arguments.retain(|argument| argument != "--force-diagnostics-panic");
    requested
}

#[cfg(all(test, debug_assertions))]
#[test]
fn forced_diagnostics_panic_is_consumed_before_desktop_argument_dispatch() {
    let mut arguments = vec![
        "--force-diagnostics-panic".into(),
        "--workspace-tabs".into(),
    ];
    assert!(take_forced_diagnostics_panic(&mut arguments));
    assert_eq!(arguments, ["--workspace-tabs"]);
    assert!(!take_forced_diagnostics_panic(&mut arguments));
}

#[cfg(debug_assertions)]
fn panic_after_stores_open() -> ! {
    diagnostic!("Aeris desktop stores opened; entering forced diagnostics panic session");
    assert!(
        aeris_observability::flush_diagnostics(),
        "forced diagnostics panic could not flush the session log"
    );
    panic!("forced diagnostics panic requested by --force-diagnostics-panic");
}

fn dispatch_desktop_readiness(arguments: &mut Vec<std::ffi::OsString>) -> bool {
    if arguments.first().map(std::ffi::OsString::as_os_str)
        != Some(std::ffi::OsStr::new("--desktop-readiness"))
    {
        return false;
    }
    if let Err(error) = run_desktop_readiness_command(std::mem::take(arguments).into_iter().skip(1))
    {
        diagnostic!("Aeris desktop readiness failed: {error}");
        flush_diagnostics_before_exit();
        std::process::exit(1);
    }
    true
}

fn start_desktop_persistence(
    account_service: &aeris_account_runtime::AccountService,
) -> chart_chrome::DesktopPersistence {
    let root = aeris_platform_runtime::native_data_root().unwrap_or_else(|error| {
        diagnostic!("Aeris desktop persistence root could not resolve: {error}");
        fatal_startup(
            account_service,
            "The desktop data folder could not be opened.",
        );
    });
    chart_chrome::DesktopPersistence::new(root)
}

fn compose_desktop_services(
    market: aeris_market_runtime::MarketService,
    trading: aeris_trading_runtime::TradingService,
    account_service: aeris_account_runtime::AccountService,
    persistence: chart_chrome::DesktopPersistence,
) -> aeris_desktop::services::DesktopServices {
    aeris_desktop::services::DesktopServices::new(
        market,
        trading,
        account_service,
        persistence,
        Arc::new(aeris_trading_runtime::clock::SystemClock::default()),
    )
}

pub(super) fn run(
    mut command_arguments: Vec<std::ffi::OsString>,
    instance_lock: aeris_platform_runtime::InstanceLock,
) {
    #[cfg(debug_assertions)]
    let force_diagnostics_panic = take_forced_diagnostics_panic(&mut command_arguments);
    if dispatch_desktop_readiness(&mut command_arguments) {
        return;
    }
    if let Err(error) = aeris_platform_runtime::migrate_legacy_native_data_root() {
        diagnostic!("Aeris legacy local state migration deferred: {error}");
    }
    let account_service = aeris_account_runtime::AccountService::new(
        aeris_account_runtime::AccountServiceConfig::production(),
    );
    let launcher_promotion = command_arguments.is_empty().then(|| {
        aeris_platform_runtime::active_data_root()
            .ok()
            .and_then(|root| {
                root.run_installed_app_side_effect(
                    aeris_platform_runtime::InstalledAppSideEffect::LauncherPromotion,
                    schedule_versioned_launcher_promotion,
                )
            })
    });
    if let Some(Some(Err(error))) = launcher_promotion {
        diagnostic!("Aeris launcher promotion deferred: {error}");
    }
    let trading = match start_trading_service() {
        Ok(trading) => trading,
        Err(aeris_trading_runtime::StoreError::Corrupt {
            path,
            newest_backup,
            detail,
        }) => handle_trading_corruption(&account_service, &path, newest_backup.as_deref(), &detail),
        Err(error) => {
            diagnostic!("Aeris trading owner could not start: {error}");
            fatal_startup(&account_service, "The trading service could not start.");
        }
    };
    let persistence = start_desktop_persistence(&account_service);
    let account = match aeris_desktop::services::install_account(&account_service) {
        Ok(account) => account,
        Err(error) => {
            diagnostic!("Aeris account session could not start: {error}");
            stop_partial_startup(&trading, None);
            fatal_startup(&account_service, "The account session could not start.");
        }
    };
    #[cfg(debug_assertions)]
    if force_diagnostics_panic {
        panic_after_stores_open();
    }
    let instance_lock = Arc::new(instance_lock);
    let startup = SignedInStartup {
        command_arguments,
        trading: trading.clone(),
        account_service: account_service.clone(),
        account: account.clone(),
        persistence,
    };
    if let Some(account) = account {
        run_sign_in(sign_in_gate::SignInGate::new(
            account,
            startup,
            trading,
            account_service,
            instance_lock,
        ));
        return;
    }
    let SignedInRuntime {
        configured,
        services,
        context,
    } = match start_signed_in_runtime(startup) {
        Ok(runtime) => runtime,
        Err(message) => {
            stop_partial_startup(&trading, None);
            fatal_startup(&account_service, message);
        }
    };
    let lifecycle = DesktopLifecycle::new(services, context, instance_lock);
    run_desktop(configured, lifecycle);
}

/// Owners opened before sign-in: only the local trading store, the account
/// session and the persistence paths. Provider sessions, the broker venue,
/// public context sources and the workspace that demands market data start
/// only in [`start_signed_in_runtime`].
struct SignedInStartup {
    command_arguments: Vec<std::ffi::OsString>,
    trading: aeris_trading_runtime::TradingService,
    account_service: aeris_account_runtime::AccountService,
    account: Option<aeris_desktop::account::DesktopAccount>,
    persistence: chart_chrome::DesktopPersistence,
}

struct SignedInRuntime {
    configured: ConfiguredDesktop,
    services: aeris_desktop::services::DesktopServices,
    context: aeris_context_runtime::ContextService,
}

/// Starts everything that reaches a provider or public source. It runs once
/// per process: directly on a validation root, otherwise when the sign-in
/// gate releases it. Blocking startup work, so never on the UI thread.
/// A failure stops what this call started and returns the user-facing message;
/// the trading owner stays with the caller.
fn start_signed_in_runtime(startup: SignedInStartup) -> Result<SignedInRuntime, &'static str> {
    let SignedInStartup {
        command_arguments,
        trading,
        account_service,
        account,
        persistence,
    } = startup;
    let context = start_context_service().map_err(|error| {
        diagnostic!("Aeris context owner could not start: {error}");
        "The context service could not start."
    })?;
    let market = match aeris_market_runtime::MarketService::start() {
        Ok(market) => market,
        Err(error) => {
            diagnostic!("Aeris market owner could not start: {error}");
            stop_signed_in_owners(None, &context);
            return Err("The market service could not start.");
        }
    };
    let services = compose_desktop_services(market, trading.clone(), account_service, persistence)
        .with_account(account);
    let configured = match configured_market_workers(command_arguments, &services) {
        Ok(Some(configured)) => configured,
        Ok(None) => {
            stop_signed_in_owners(Some(&services.market), &context);
            stop_partial_startup(&trading, None);
            exit_after_account_refresh_quiesce(&services.account_service, 0);
        }
        Err(error) => {
            diagnostic!("Aeris market worker could not start: {error}");
            stop_signed_in_owners(Some(&services.market), &context);
            return Err("The market service could not start.");
        }
    };
    if let Err(error) = attach_trading_market_feed(&trading, &services.market) {
        diagnostic!("Aeris trading market feed could not attach: {error}");
        stop_signed_in_owners(Some(&services.market), &context);
        return Err("The trading market feed could not start.");
    }
    if let Err(error) = attach_broker_venue(&trading, &services.market) {
        diagnostic!("Aeris broker venue could not attach: {error}");
    }
    Ok(SignedInRuntime {
        configured,
        services,
        context,
    })
}

fn stop_signed_in_owners(
    market: Option<&aeris_market_runtime::MarketService>,
    context: &aeris_context_runtime::ContextService,
) {
    if let Some(market) = market
        && let Err(error) = market.shutdown(Duration::from_secs(2))
    {
        diagnostic!("Aeris market owner could not stop: {error}");
    }
    if let Err(error) = context.shutdown(Duration::from_secs(2)) {
        diagnostic!("Aeris context owner could not stop: {error}");
    }
}

fn attach_trading_market_feed(
    trading: &aeris_trading_runtime::TradingService,
    market: &aeris_market_runtime::MarketService,
) -> Result<(), String> {
    let quote_inbox = trading.attach_market_feed(market.trading_quote_interest_sink())?;
    market.attach_trading_feed(aeris_market_runtime::market_service::TradingFeedLink {
        events: Box::new(move |event| {
            if let Err(error) = quote_inbox.push(event) {
                diagnostic!("Aeris trading market feed rejected an event: {error}");
            }
        }),
    })
}

/// Broker orders route only while the market runtime's venue relay is attached; without
/// it the trading owner refuses them, so a failed attach is reported, not fatal.
fn attach_broker_venue(
    trading: &aeris_trading_runtime::TradingService,
    market: &aeris_market_runtime::MarketService,
) -> Result<(), String> {
    let owner = trading.clone();
    let inbox = trading.demo_venue_inbox();
    market.attach_broker_venue(aeris_market_runtime::BrokerVenueLink {
        attach: Box::new(move |capabilities| owner.attach_demo_venue(capabilities)),
        events: Box::new(move |event| inbox.push(event)),
    })
}

fn stop_partial_startup(
    trading: &aeris_trading_runtime::TradingService,
    context: Option<&aeris_context_runtime::ContextService>,
) {
    if let Some(context) = context
        && let Err(error) = context.shutdown(Duration::from_secs(2))
    {
        diagnostic!("Aeris context owner could not stop during startup cleanup: {error}");
    }
    if let Err(error) = trading.shutdown(Duration::from_secs(2)) {
        diagnostic!("Aeris trading owner could not stop during startup cleanup: {error}");
    }
}

fn fatal_startup(account_service: &aeris_account_runtime::AccountService, message: &str) -> ! {
    let diagnostics_log_path = aeris_platform_runtime::native_data_root().map_or_else(
        |_| std::path::PathBuf::from("aeris.log"),
        |root| root.join("logs").join("aeris.log"),
    );
    let detail = format!(
        "{message}\n\nDiagnostics were written to:\n{}",
        diagnostics_log_path.display()
    );
    if let Err(error) = aeris_platform_runtime::message_box("Aeris Terminal", &detail) {
        diagnostic!("Aeris startup error dialog could not open: {error}");
    }
    exit_after_account_refresh_quiesce(account_service, 1);
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
            prepare_application(cx);
            mount_desktop(configured, lifecycle, None, cx);
        });
}

/// Runs the sign-in window first; it mounts the workspace once the account
/// owner reports a usable session.
fn run_sign_in(gate: sign_in_gate::SignInGate) {
    application()
        .with_assets(assets::AerisAssets)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            prepare_application(cx);
            sign_in_gate::open_sign_in_window(gate, cx);
        });
}

fn prepare_application(cx: &mut App) {
    gpui_base::init(cx);
    cx.set_global(base_theme(&AerisTheme::dark()));
    install_platform_http_client(cx);
    if aeris_platform_runtime::active_data_root().is_ok_and(|root| {
        root.allows_installed_app_side_effect(
            aeris_platform_runtime::InstalledAppSideEffect::NotificationIdentityRegistration,
        )
    }) {
        cx.set_app_identity(
            aeris_platform_runtime::APPLICATION_IDENTIFIER,
            aeris_platform_runtime::APPLICATION_NAME,
        );
    }
    if let Err(error) = cx.text_system().add_fonts(
        PLATFORM_FONT_BYTES
            .iter()
            .map(|font| Cow::Borrowed(*font))
            .chain(std::iter::once(Cow::Borrowed(BRAND_FONT_BYTES)))
            .collect(),
    ) {
        diagnostic!("Aeris bundled fonts unavailable, using system fallback: {error}");
    }
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
    let workspace_notice = configured.workspace_notice;
    if let Err(error) = bind_desktop_keys(cx) {
        diagnostic!("Aeris keyboard shortcuts are invalid: {error}");
        return None;
    }
    let quit_lifecycle = lifecycle.clone();
    cx.on_app_quit(move |cx| {
        let quit = quit_lifecycle.begin_quit(cx);
        async move {
            if let Some(quit) = quit
                && let Err(error) = quit.await
            {
                let blocks_exit = error.blocks_exit();
                diagnostic!("Aeris desktop shutdown failed: {error}");
                if blocks_exit {
                    flush_diagnostics_before_exit();
                    std::process::exit(1);
                }
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
                    if let Err(error) = cx.open_window(options, build) {
                        diagnostic!("Aeris terminal window could not open: {error}");
                    }
                }
            }
        }
        DesktopLayout::WorkspaceTabs => {
            let Some(workspace_factory) = workspace_factory else {
                diagnostic!("Aeris workspace factory is unavailable");
                return None;
            };
            let build = move |window: &mut Window, cx: &mut App| {
                workspace_tabs_root(
                    workspace_panes,
                    WorkspaceTabsRestore {
                        state: &restored_workspace,
                        notice: workspace_notice,
                    },
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
                if let Err(error) = cx.open_window(options, build) {
                    diagnostic!("Aeris workspace window could not open: {error}");
                }
            }
        }
    }
    cx.activate(true);
    existing_root
}

/// The GPUI action a key binding dispatches for a command. Commands without one run only from
/// the command palette, so the registry must not give them a chord.
fn command_key_action(
    id: aeris_desktop::command_registry::CommandId,
) -> Option<Box<dyn gpui::Action>> {
    use aeris_desktop::command_registry::CommandId;
    Some(match id {
        CommandId::OpenPalette => Box::new(OpenCommandPalette),
        CommandId::ToggleFullscreen => Box::new(ToggleFullscreen),
        CommandId::MinimizeWindow => Box::new(MinimizeWindow),
        CommandId::ZoomWindow => Box::new(ZoomWindow),
        CommandId::CloseWindow => Box::new(CloseWindow),
        CommandId::NewWorkspace => Box::new(NewWorkspace),
        CommandId::SelectNextWorkspace => Box::new(SelectNextWorkspace),
        CommandId::SelectPreviousWorkspace => Box::new(SelectPreviousWorkspace),
        CommandId::MoveWorkspaceLeft => Box::new(MoveWorkspaceLeft),
        CommandId::MoveWorkspaceRight => Box::new(MoveWorkspaceRight),
        CommandId::CloseWorkspace => Box::new(CloseWorkspace),
        CommandId::SaveLayout => Box::new(SaveActiveLayout),
        CommandId::SplitHorizontal => Box::new(SplitPaneHorizontal),
        CommandId::SplitVertical => Box::new(SplitPaneVertical),
        CommandId::ClosePane => Box::new(ClosePane),
        CommandId::ToggleOneClickTrading => Box::new(ToggleOneClickTrading),
        CommandId::BuyMarket => Box::new(TradingBuyMarket),
        CommandId::SellMarket => Box::new(TradingSellMarket),
        CommandId::CancelAll => Box::new(TradingCancelAll),
        CommandId::FlattenAccount => Box::new(TradingFlattenAccount),
        CommandId::KillSwitch => Box::new(TradingKillSwitch),
        CommandId::ToggleContext
        | CommandId::ToggleOrderBook
        | CommandId::ToggleOrderEntry
        | CommandId::ToggleTimeSales
        | CommandId::ToggleWatchlist
        | CommandId::ChartCandles
        | CommandId::ChartBars
        | CommandId::ChartLine
        | CommandId::ChartArea
        | CommandId::ChartBaseline
        | CommandId::ChartFootprint
        | CommandId::Interval1Minute
        | CommandId::Interval5Minutes
        | CommandId::Interval15Minutes
        | CommandId::Interval1Hour
        | CommandId::Interval1Day
        | CommandId::ToggleLayoutAutosave
        | CommandId::CopyLayout
        | CommandId::OpenRecentLayout => return None,
    })
}

/// Builds every desktop key binding from the command registry after checking it, and the
/// workspace shortcuts, against the system shortcuts and the keys the chart consumes.
fn desktop_key_bindings() -> Result<Vec<KeyBinding>, String> {
    use aeris_desktop::command_registry::{COMMANDS, WORKSPACE_SHORTCUTS, validate_chords};
    validate_chords(
        COMMANDS,
        WORKSPACE_SHORTCUTS,
        &aeris_chart_integration::chart_keystrokes(),
    )?;
    let mut bindings = Vec::new();
    for spec in COMMANDS.iter().filter(|spec| !spec.chords.is_empty()) {
        let action = command_key_action(spec.id)
            .ok_or_else(|| format!("{} has a shortcut but no key action", spec.title))?;
        for chord in spec.chords {
            let binding = KeyBinding::load(
                chord,
                action.boxed_clone(),
                None,
                false,
                None,
                &gpui::DummyKeyboardMapper,
            )
            .map_err(|error| format!("{} chord {chord}: {error}", spec.title))?;
            bindings.push(binding);
        }
    }
    Ok(bindings)
}

fn bind_desktop_keys(cx: &mut App) -> Result<(), String> {
    cx.bind_keys(desktop_key_bindings()?);
    Ok(())
}
#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "desktop/test_support/pane_harness.rs"]
mod pane_harness;

#[cfg(test)]
#[path = "http_wiring_tests.rs"]
mod http_wiring_tests;
