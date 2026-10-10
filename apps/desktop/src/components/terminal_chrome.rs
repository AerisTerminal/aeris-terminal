use super::app_navigation::app_navigation_button;
use super::platform_menu::account_avatar_button;
use super::*;
use aeris_contracts::{MarketSessionPhase, MarketSessionStatus};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WindowCommand {
    MaximizeOrRestore,
    ToggleFullscreen,
}

impl WindowCommand {
    pub(super) fn execute(self, window: &mut Window) {
        match self {
            Self::MaximizeOrRestore if window.is_fullscreen() => window.toggle_fullscreen(),
            Self::MaximizeOrRestore => window.zoom_window(),
            Self::ToggleFullscreen => window.toggle_fullscreen(),
        }
    }
}

/// Trading View style keys on a focused chart. The chart engine binds neither key, so they
/// fall through to the workspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChartShortcut {
    NextWatchlistSymbol,
    PreviousWatchlistSymbol,
    /// The chart alone fills the screen.
    ToggleChartFullscreen,
}

/// Space steps forward through the watchlist, Shift+Space back, and Shift+F toggles the
/// chart-only fullscreen. Any other modifier leaves the key to other shortcuts.
pub(super) fn chart_shortcut(key: &str, modifiers: gpui::Modifiers) -> Option<ChartShortcut> {
    if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
        return None;
    }
    match key {
        "space" if modifiers.shift => Some(ChartShortcut::PreviousWatchlistSymbol),
        "space" => Some(ChartShortcut::NextWatchlistSymbol),
        "f" if modifiers.shift => Some(ChartShortcut::ToggleChartFullscreen),
        _ => None,
    }
}

/// The watchlist row a step lands on, wrapping at both ends. A chart showing a symbol that is
/// not in the watchlist starts from the first row going forward and the last going back.
pub(super) fn watchlist_step(current: Option<usize>, len: usize, forward: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match (current, forward) {
        (Some(index), true) => (index + 1) % len,
        (Some(index), false) => (index + len - 1) % len,
        (None, true) => 0,
        (None, false) => len - 1,
    })
}

pub(super) fn fullscreen_escape_command(key: &str, is_fullscreen: bool) -> Option<WindowCommand> {
    if key.eq_ignore_ascii_case("escape") && is_fullscreen {
        return Some(WindowCommand::ToggleFullscreen);
    }
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CaptionPlatform {
    Windows,
    Linux,
    MacOs,
    Other,
}

pub(super) const fn current_caption_platform() -> CaptionPlatform {
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
pub(super) enum CaptionPointerOwner {
    Native,
    Application,
    System,
}

pub(super) const fn caption_pointer_owner(platform: CaptionPlatform) -> CaptionPointerOwner {
    match platform {
        CaptionPlatform::Windows => CaptionPointerOwner::Native,
        CaptionPlatform::Linux | CaptionPlatform::Other => CaptionPointerOwner::Application,
        CaptionPlatform::MacOs => CaptionPointerOwner::System,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CaptionCommand {
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

    fn execute(self, terminal: Option<&Entity<TerminalApp>>, window: &mut Window, cx: &mut App) {
        match self {
            Self::Minimize => window.minimize_window(),
            Self::MaximizeOrRestore => window.zoom_window(),
            Self::Close if terminal.is_some() => {
                let Some(terminal) = terminal else {
                    return;
                };
                terminal.update(cx, |terminal, terminal_cx| {
                    terminal.close_window(&CloseWindow, window, terminal_cx);
                });
            }
            Self::Close => window.remove_window(),
        }
    }
}

pub(super) fn caption_keyboard_activates(key: &str) -> bool {
    matches!(key, "enter" | "space")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WindowMoveGestureEvent {
    Press,
    Move { left_pressed: bool },
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct WindowMoveGestureTransition {
    pub(super) pending: bool,
    pub(super) start_move: bool,
}

pub(super) const fn window_move_gesture_transition(
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

pub(super) fn terminal_header(
    app: &Entity<WorkspaceSurface>,
    state: HeaderState,
) -> impl IntoElement + use<> {
    let theme = state.theme;
    let controls = header_controls(app, state);
    div()
        .w_full()
        .h(px(theme.dimensions.app_header_height))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(gpui_color(theme.colors.border))
        .bg(gpui_color(theme.colors.surface))
        .child(header_brand(&theme))
        .child(div().h_full().min_w_0().flex_1().pr_3().child(controls))
}

fn header_brand(theme: &AerisTheme) -> Stateful<Div> {
    div()
        .id("aeris_header_brand")
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .pl_2()
        .pr_3()
        .child(brand_logo_sized(px(32.0)))
        .child(
            div()
                .text_sm()
                .font_family(aeris_design_system::brand_font_family())
                .font_weight(gpui::FontWeight(400.0))
                .text_color(gpui_color(theme.colors.text_primary))
                .child("Aeris"),
        )
}

#[derive(Clone, Copy)]
pub(super) struct WorkspaceTabBarState<'a> {
    pub(super) workspaces: &'a [WorkspaceTab],
    pub(super) market_summaries: &'a BTreeMap<MarketSummaryKey, MarketSummaryEntry>,
    pub(super) active: usize,
    pub(super) enabled: bool,
    pub(super) error: Option<&'a str>,
    pub(super) workspace_drag: Option<WorkspaceDragState>,
    pub(super) app_view: super::market_screener::AppView,
    pub(super) keyboard_trading: super::trading_hotkeys::KeyboardTradingIndicator,
    pub(super) theme: AerisTheme,
}

pub(super) fn workspace_window_drag_region(
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

pub(super) fn workspace_title_bar(
    terminal: &Entity<TerminalApp>,
    state: &WorkspaceTabBarState<'_>,
    window: &Window,
    cx: &App,
) -> impl IntoElement + use<> {
    let tabs = workspace_tab_strip(terminal, state, window, cx);
    let theme = state.theme;
    let drag_region = workspace_window_drag_region(
        div()
            .id("workspace_window_drag_region")
            .h_full()
            .min_w(px(12.0))
            .flex_1(),
        terminal,
    );
    let account = aeris_desktop::account::DesktopAccount::shared()
        .map_or_else(aeris_desktop::account::unavailable_menu_state, |account| {
            account.menu_state()
        });
    let profile_region = div()
        .id("workspace_profile_region")
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .pl_4()
        .pr_2()
        .gap_2()
        .child(account_avatar_button(terminal, &account, &theme))
        .child(app_navigation_button(terminal, state.app_view, &theme));
    div()
        .w_full()
        .h(px(WORKSPACE_TITLE_BAR_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(gpui_color(theme.colors.border_secondary))
        .bg(gpui_color(theme.colors.surface_secondary))
        .when(cfg!(target_os = "macos"), |bar| bar.pl(px(80.0)))
        .child(
            div()
                .h_full()
                .min_w_0()
                .flex_1()
                .flex()
                .items_center()
                .overflow_x_hidden()
                .child(profile_region)
                .child(tabs)
                .child(drag_region),
        )
        .child(super::trading_hotkeys::keyboard_trading_toggle(
            terminal,
            state.keyboard_trading,
            &theme,
        ))
        .child(workspace_window_controls(terminal, window, &theme))
}

#[derive(Clone, Copy)]
struct CaptionControlSpec {
    id: &'static str,
    icon: HugeIcon,
    label: &'static str,
    command: CaptionCommand,
    close: bool,
}

fn workspace_caption_control(
    terminal: Option<&Entity<TerminalApp>>,
    spec: CaptionControlSpec,
    pointer_owner: CaptionPointerOwner,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let CaptionControlSpec {
        id,
        icon,
        label,
        command,
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
                control.bg(gpui_color(colors.hover_bg.over(colors.surface_secondary)))
            }
        })
        .child(header_icon(icon).small());

    match pointer_owner {
        CaptionPointerOwner::Native => control
            .occlude()
            .window_control_area(command.window_control_area()),
        CaptionPointerOwner::Application => {
            let pointer_terminal = terminal.cloned();
            let key_terminal = terminal.cloned();
            control
                .role(Role::Button)
                .aria_label(label)
                .tab_index(0)
                .focus_visible(move |control| {
                    control
                        .border_2()
                        .border_color(gpui_color(colors.ring_primary))
                })
                .on_key_down(move |event, window, cx| {
                    if caption_keyboard_activates(event.keystroke.key.as_str()) {
                        command.execute(key_terminal.as_ref(), window, cx);
                        cx.stop_propagation();
                    }
                })
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(move |_, window, cx| {
                    command.execute(pointer_terminal.as_ref(), window, cx);
                    cx.stop_propagation();
                })
        }
        CaptionPointerOwner::System => control,
    }
}

pub(super) fn workspace_window_controls(
    terminal: &Entity<TerminalApp>,
    window: &Window,
    theme: &AerisTheme,
) -> Div {
    let pointer_owner = caption_pointer_owner(current_caption_platform());
    if pointer_owner == CaptionPointerOwner::System {
        return div().h_full();
    }
    let supported = window.window_controls();
    let minimize = workspace_caption_control(
        Some(terminal),
        CaptionControlSpec {
            id: "workspace_window_minimize",
            icon: HugeIcon::WindowMinimize,
            label: "Minimize window",
            command: CaptionCommand::Minimize,
            close: false,
        },
        pointer_owner,
        theme,
    );
    let maximize = workspace_caption_control(
        Some(terminal),
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
            close: false,
        },
        pointer_owner,
        theme,
    );
    let close = workspace_caption_control(
        Some(terminal),
        CaptionControlSpec {
            id: "workspace_window_close",
            icon: HugeIcon::WindowClose,
            label: "Close window",
            command: CaptionCommand::Close,
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

/// Feed health plus, once live, the selected product's market session for the header dot.
fn header_connection_presentation(state: &HeaderState) -> ConnectionPresentation {
    connection_presentation(
        state.provider,
        state.connection_state,
        state.transport_rtt_nanos,
    )
    .with_market(
        state.connection_state == FeedConnectionState::Streaming,
        state.market_session.as_ref(),
        &state.time_zone_id,
        current_unix_nanos(),
    )
}

pub(super) fn header_controls(
    app: &Entity<WorkspaceSurface>,
    state: HeaderState,
) -> impl IntoElement {
    // Give the global controls an explicit, non-shrinking track. GPUI cannot
    // infer a stable intrinsic width for this mixed Button/avatar group, which
    // previously let the flex item collapse to zero even on wide windows.
    let (side_panel_toggles, context_toggle, link_toggle) = header_panel_toggles(app, &state);
    let connection = header_connection_presentation(&state);
    let market_controls = div()
        .h_full()
        .min_w_0()
        .flex_1()
        .flex()
        .items_center()
        .gap_2()
        .overflow_x_hidden()
        .child(connection_status_indicator(connection, &state.theme))
        .child(instrument_selector(
            app.clone(),
            &InstrumentSelectorState {
                label: state.instrument_label,
                message: String::new(),
                instruments: state.instruments,
                input: state.symbol_input,
                availability: InstrumentSelectorAvailability {
                    selection_pending: state.pending.symbol_selection,
                    enabled: state.controls.enabled(HeaderControls::INSTRUMENT),
                },
                provider: state.provider,
                menu_provider: state.symbol_provider,
                menu: InstrumentSelectorMenu {
                    keyboard_selection: 0,
                    keyboard_active: false,
                },
                scroll: state.instrument_scroll,
                target: SymbolSelectionTarget::Chart,
                // The header trigger never renders the menu body that uses these.
                hosted_broker_disconnected: false,
                provider_menu_open: false,
                markets_flyout_open: false,
                search_categories: aeris_contracts::InstrumentSearchCategories::ALL,
            },
            &state.theme,
        ))
        .child(series_selector(
            app.clone(),
            state.series_label,
            state.series_message,
            state.pending.series,
            &state.theme,
            state.controls.enabled(HeaderControls::SERIES),
        ))
        .child(chart_type_selector(
            app.clone(),
            state.chart_type,
            state.controls.enabled(HeaderControls::CHART_TYPE),
            &state.theme,
        ))
        .child(indicator_selector(
            app.clone(),
            state.indicator_input,
            state.indicator_message,
            state.controls.enabled(HeaderControls::INDICATOR),
            &state.theme,
        ))
        .child(drawing_history_control(
            app.clone(),
            DrawingHistoryControl::Undo,
            state.drawing_history,
            &state.theme,
        ))
        .child(drawing_history_control(
            app.clone(),
            DrawingHistoryControl::Redo,
            state.drawing_history,
            &state.theme,
        ))
        .children(side_panel_toggles)
        .child(context_toggle)
        .child(link_toggle);
    let accounts = accounts_selector(app.clone(), state.account_label, &state.theme);
    let time_zone = time_zone_selector(
        app.clone(),
        state.time_zone_clock,
        &state.time_zone_id,
        &state.theme,
    );
    div()
        .w_full()
        .h_full()
        .min_w_0()
        .flex_1()
        .flex()
        .items_center()
        .child(market_controls)
        .child(accounts)
        .child(time_zone)
}

/// Rithmic requires its attribution marks to stay visible for as long as the
/// terminal holds a Rithmic session, including while that session recovers.
pub(super) fn shows_rithmic_attribution(
    provider: TerminalProvider,
    state: FeedConnectionState,
) -> bool {
    provider == TerminalProvider::Rithmic
        && matches!(
            state,
            FeedConnectionState::Authenticating
                | FeedConnectionState::Streaming
                | FeedConnectionState::Recovering
        )
}

/// Draws an attribution mark from the artwork pre-rendered for the window's density, at
/// that artwork's own pixel size, so the GPU never resamples it.
#[derive(IntoElement)]
pub(super) struct AttributionMarkImage {
    pub(super) mark: assets::AttributionMark,
    pub(super) mode: aeris_design_system::ThemeMode,
}

impl RenderOnce for AttributionMarkImage {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let density = assets::MarkDensity::for_scale(window.scale_factor());
        img(self.mark.path(self.mode, density))
            .flex_none()
            .w(px(self.mark.width(self.mode)))
            .h(px(assets::AttributionMark::HEIGHT))
            .object_fit(ObjectFit::Fill)
    }
}

pub(super) fn rithmic_attribution(theme: &AerisTheme) -> impl IntoElement + use<> {
    let mark = |mark| AttributionMarkImage {
        mark,
        mode: theme.mode,
    };
    div()
        .id("rithmic_attribution")
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_muted))
                .child("Connected to Rithmic"),
        )
        .child(mark(assets::AttributionMark::MarketDataByRithmic))
        .child(
            div()
                .w(px(theme.dimensions.border_width))
                .h(px(assets::AttributionMark::HEIGHT))
                .bg(gpui_color(theme.colors.border)),
        )
        .child(mark(assets::AttributionMark::PoweredByOmne))
}

fn header_panel_toggles(
    app: &Entity<WorkspaceSurface>,
    state: &HeaderState,
) -> (Vec<AnyElement>, AnyElement, AnyElement) {
    let side_panels = SidePanel::ALL
        .into_iter()
        .map(|panel| {
            side_panel_toggle(
                app.clone(),
                &state.theme,
                panel,
                !panel.requires_market() || state.controls.enabled(HeaderControls::MARKET_PANELS),
                state.side_panels.contains(panel),
            )
        })
        .collect();
    let context_command = aeris_desktop::command_registry::command(
        aeris_desktop::command_registry::CommandId::ToggleContext,
    );
    let context = panel_toggle(
        PanelToggleState {
            id: "context_panel_toggle",
            label: context_command.title,
            icon: HugeIcon::Info,
            enabled: true,
            selected: state.context_visible,
            tooltip: context_command.title,
            toggle: WorkspaceSurface::toggle_context_panel,
        },
        &state.theme,
        app.clone(),
    )
    .into_any_element();
    let link_label = match state.chart_link_group {
        1 => "Link A",
        2 => "Link B",
        3 => "Link C",
        4 => "Link D",
        _ => "Link",
    };
    let link = panel_toggle(
        PanelToggleState {
            id: "chart_link_group_toggle",
            label: link_label,
            icon: HugeIcon::SplitSideBySide,
            enabled: true,
            selected: state.chart_link_group != 0,
            tooltip: "Cycle linked chart group",
            toggle: WorkspaceSurface::cycle_chart_link_group,
        },
        &state.theme,
        app.clone(),
    )
    .into_any_element();
    (side_panels, context, link)
}

pub(super) fn side_panel_toggle(
    app: Entity<WorkspaceSurface>,
    theme: &AerisTheme,
    panel: SidePanel,
    enabled: bool,
    selected: bool,
) -> AnyElement {
    let (id, icon, toggle, command_id) = match panel {
        SidePanel::OrderBook => (
            "order_book_toggle",
            HugeIcon::DataPanel,
            WorkspaceSurface::toggle_order_book
                as fn(&mut WorkspaceSurface, &mut Context<WorkspaceSurface>),
            aeris_desktop::command_registry::CommandId::ToggleOrderBook,
        ),
        SidePanel::TimeSales => (
            "time_sales_toggle",
            HugeIcon::DataPanel,
            WorkspaceSurface::toggle_time_sales
                as fn(&mut WorkspaceSurface, &mut Context<WorkspaceSurface>),
            aeris_desktop::command_registry::CommandId::ToggleTimeSales,
        ),
        SidePanel::Watchlist => (
            "watchlist_toggle",
            HugeIcon::DataPanel,
            WorkspaceSurface::toggle_watchlist
                as fn(&mut WorkspaceSurface, &mut Context<WorkspaceSurface>),
            aeris_desktop::command_registry::CommandId::ToggleWatchlist,
        ),
    };
    let command = aeris_desktop::command_registry::command(command_id);
    panel_toggle(
        PanelToggleState {
            id,
            label: command.title,
            icon,
            enabled,
            selected,
            tooltip: command.title,
            toggle,
        },
        theme,
        app,
    )
    .into_any_element()
}

pub(super) fn connection_status_indicator(
    presentation: ConnectionPresentation,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let color = (presentation.color)(theme);
    let provider = SharedString::from(presentation.provider);
    let status = SharedString::from(presentation.status);
    let latency = SharedString::from(presentation.latency);
    let market_rows: Vec<(&'static str, SharedString)> = presentation
        .market_rows
        .into_iter()
        .map(|(label, value)| (label, SharedString::from(value)))
        .collect();
    let tooltip_theme = *theme;
    div()
        .id(("connection_status", usize::MAX))
        .flex()
        .child(
            div()
                .id("connection_status_dot")
                .size(px(7.0))
                .flex_none()
                .rounded_full()
                .bg(gpui_color(color)),
        )
        .tooltip(move |_, cx| {
            cx.new(|_| ConnectionStatusTooltip {
                provider: provider.clone(),
                status: status.clone(),
                latency: latency.clone(),
                market_rows: market_rows.clone(),
                theme: tooltip_theme,
            })
            .into()
        })
        .tooltip_show_delay(TOOLTIP_OPEN_DELAY)
}

struct ConnectionStatusTooltip {
    provider: SharedString,
    status: SharedString,
    latency: SharedString,
    market_rows: Vec<(&'static str, SharedString)>,
    theme: AerisTheme,
}

impl Render for ConnectionStatusTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        div().pl_2().pt_2().child(
            div()
                .min_w(px(190.0))
                .px_3()
                .py_2()
                .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                .border_1()
                .border_color(gpui_color(colors.border))
                .bg(gpui_color(colors.surface))
                .font_family(aeris_design_system::platform_font_family())
                .font_weight(platform_font_weight(TypographyRole::Normal))
                .flex()
                .flex_col()
                .gap_1()
                .child(connection_tooltip_row(
                    "Provider",
                    self.provider.clone(),
                    false,
                    &self.theme,
                ))
                .child(connection_tooltip_row(
                    "Status",
                    self.status.clone(),
                    false,
                    &self.theme,
                ))
                .child(connection_tooltip_row(
                    "Latency",
                    self.latency.clone(),
                    true,
                    &self.theme,
                ))
                .children(self.market_rows.iter().map(|(label, value)| {
                    connection_tooltip_row(label, value.clone(), false, &self.theme)
                })),
        )
    }
}

fn connection_tooltip_row(
    label: &'static str,
    value: SharedString,
    numeric: bool,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_muted))
                .child(label),
        )
        .child(if numeric {
            div()
                .font_family(aeris_design_system::platform_font_family())
                .font_features(platform_tabular_numerals())
                .text_xs()
                .text_color(gpui_color(theme.colors.text_primary))
                .child(value)
        } else {
            div()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_primary))
                .child(value)
        })
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
    theme: &AerisTheme,
    app: Entity<WorkspaceSurface>,
) -> impl IntoElement {
    Button::new(state.id, theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Lg)
        .icon(header_icon(state.icon))
        .label(state.label)
        .aria_label(state.tooltip)
        .tooltip(TooltipSpec::new(state.tooltip, theme).show_delay(TOOLTIP_OPEN_DELAY))
        .text_toggle()
        .selected(state.selected)
        .disabled(!state.enabled)
        .on_click(move |_, _, cx| {
            app.update(cx, state.toggle);
        })
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum DrawingHistoryControl {
    Undo,
    Redo,
}

impl DrawingHistoryControl {
    pub(super) const fn id(self) -> &'static str {
        match self {
            Self::Undo => "drawing_undo",
            Self::Redo => "drawing_redo",
        }
    }

    const fn tooltip(self) -> &'static str {
        match self {
            Self::Undo => "Undo drawing edit",
            Self::Redo => "Redo drawing edit",
        }
    }

    pub(super) const fn icon(self) -> HugeIcon {
        match self {
            Self::Undo => HugeIcon::Undo,
            Self::Redo => HugeIcon::Redo,
        }
    }

    pub(super) const fn enabled(self, history: DrawingHistoryState) -> bool {
        match self {
            Self::Undo => history.can_undo,
            Self::Redo => history.can_redo,
        }
    }

    const fn step(self) -> fn(&mut WorkspaceSurface, &mut Context<WorkspaceSurface>) {
        match self {
            Self::Undo => WorkspaceSurface::undo_drawing,
            Self::Redo => WorkspaceSurface::redo_drawing,
        }
    }
}

/// Steps the active chart's drawing history from the header toolbar. The control greys out when
/// its side of the history is empty, so the header never offers an edit the chart cannot make.
fn drawing_history_control(
    app: Entity<WorkspaceSurface>,
    control: DrawingHistoryControl,
    history: DrawingHistoryState,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let step = control.step();
    Button::new(control.id(), theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Lg)
        .icon(header_icon(control.icon()))
        .aria_label(control.tooltip())
        .tooltip(TooltipSpec::new(control.tooltip(), theme).show_delay(TOOLTIP_OPEN_DELAY))
        .disabled(!control.enabled(history))
        .on_click(move |_, _, cx| {
            app.update(cx, step);
        })
}

pub(super) fn header_icon(name: HugeIcon) -> Icon {
    Icon::default().path(name.path())
}

pub(super) fn series_icon_kind(chart_type: ChartType) -> assets::SeriesIcon {
    match chart_type {
        ChartType::Candles | ChartType::Footprint => assets::SeriesIcon::Candlestick,
        ChartType::HollowCandles
        | ChartType::HollowCandlesBullish
        | ChartType::HollowCandlesBearish => assets::SeriesIcon::HollowCandles,
        ChartType::Bars => assets::SeriesIcon::OhlcBar,
        ChartType::Line | ChartType::Baseline => assets::SeriesIcon::Line,
        ChartType::LineWithMarkers => assets::SeriesIcon::LineWithMarkers,
        ChartType::Area => assets::SeriesIcon::Area,
        ChartType::BrushableArea => assets::SeriesIcon::BrushableArea,
    }
}

// GPUI's `img()` rasterizes an SVG once at intrinsic size and stretches that bitmap,
// which pixelates large artwork; its `svg()` element only paints a monochrome mask.
// Rasterize bundled colored marks at the exact device-pixel size they are drawn instead.
struct ColoredMarkCache {
    images: HashMap<(SharedString, u64, u64), Arc<gpui::RenderImage>>,
}

impl ColoredMarkCache {
    fn new() -> Self {
        Self {
            images: HashMap::new(),
        }
    }
}

static MARK_CACHE: std::sync::LazyLock<Mutex<ColoredMarkCache>> =
    std::sync::LazyLock::new(|| Mutex::new(ColoredMarkCache::new()));

pub(super) fn ordered_f32_key(value: f32) -> u64 {
    value.to_bits().into()
}

pub(super) fn rasterize_colored_svg(
    path: &SharedString,
    logical_size: Pixels,
    window_scale: f32,
    cx: &App,
) -> Result<Arc<gpui::RenderImage>, gpui::ImageCacheError> {
    let window_scale = window_scale.max(1.0);
    let size_key = ordered_f32_key(f32::from(logical_size));
    let scale_key = ordered_f32_key(window_scale);
    let key = (path.clone(), size_key, scale_key);

    if let Ok(cache) = MARK_CACHE.lock()
        && let Some(image) = cache.images.get(&key)
    {
        return Ok(Arc::clone(image));
    }

    let bytes = assets::AerisAssets
        .load(path.as_ref())
        .map_err(|error| gpui::ImageCacheError::Other(Arc::new(error)))?
        .ok_or_else(|| {
            gpui::ImageCacheError::Asset(format!("Embedded resource not found: {path}").into())
        })?;
    let intrinsic = native_ui::icon::svg_intrinsic_width(&bytes).ok_or_else(|| {
        gpui::ImageCacheError::Asset(format!("SVG intrinsic width missing: {path}").into())
    })?;
    let target_logical = f32::from(logical_size) * window_scale;
    let scale_factor = (target_logical / intrinsic).max(1.0 / intrinsic);

    let image = cx
        .svg_renderer()
        .render_single_frame(&bytes, scale_factor)
        .map_err(|error| gpui::ImageCacheError::Usvg(Arc::new(error)))?;

    if let Ok(mut cache) = MARK_CACHE.lock() {
        cache.images.insert(key, Arc::clone(&image));
    }
    Ok(image)
}

#[derive(Clone, IntoElement)]
struct ColoredSvgMark {
    path: SharedString,
    size: Pixels,
}

impl RenderOnce for ColoredSvgMark {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let scale = window.scale_factor();
        colored_svg_element(
            &self.path,
            native_ui::icon::device_pixel_size(self.size, scale),
            scale,
            cx,
        )
    }
}

pub(super) fn colored_svg_element(
    path: &SharedString,
    size: Pixels,
    window_scale: f32,
    cx: &App,
) -> AnyElement {
    match rasterize_colored_svg(path, size, window_scale, cx) {
        Ok(image) => img(ImageSource::Render(image))
            .size(size)
            .flex_none()
            .object_fit(ObjectFit::Fill)
            .into_any_element(),
        Err(_) => img(path.clone())
            .size(size)
            .flex_none()
            .object_fit(ObjectFit::Fill)
            .into_any_element(),
    }
}

pub(super) fn series_glyph(chart_type: ChartType, size: Pixels) -> impl IntoElement {
    ColoredSvgMark {
        path: series_icon_kind(chart_type).path(),
        size,
    }
}

pub(super) fn exchange_mark(
    logo: assets::ExchangeLogo,
    size: Pixels,
    bordered: bool,
    colors: &aeris_design_system::ThemeColors,
) -> Div {
    let glyph_size = if bordered { size - px(4.0) } else { size };
    mark_tile(size, bordered, colors).child(ColoredSvgMark {
        path: logo.path(),
        size: glyph_size,
    })
}

/// Round tile shared by every provider and exchange mark, so a provider drawn with a generic
/// icon occupies exactly the same footprint as one drawn with its logo.
pub(super) fn mark_tile(
    size: Pixels,
    bordered: bool,
    colors: &aeris_design_system::ThemeColors,
) -> Div {
    div()
        .size(size)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .overflow_hidden()
        .when(bordered, |mark| {
            mark.border_1().border_color(gpui_color(colors.border))
        })
}

pub(super) fn brand_logo_sized(size: Pixels) -> impl IntoElement {
    ColoredSvgMark {
        path: assets::BrandAsset::MainLogo.path(),
        size,
    }
}

pub(super) fn chrome_tooltip(
    id: &'static str,
    label: impl Into<gpui::SharedString>,
    trigger: impl IntoElement + 'static,
    theme: &AerisTheme,
) -> AnyElement {
    with_tooltip(
        (id, usize::MAX),
        trigger,
        &TooltipSpec::new(label, theme).show_delay(TOOLTIP_OPEN_DELAY),
    )
    .into_any_element()
}

pub(super) fn series_selector(
    app: Entity<WorkspaceSurface>,
    label: String,
    _message: String,
    pending: bool,
    theme: &AerisTheme,
    enabled: bool,
) -> impl IntoElement {
    let open_app = app.clone();
    let bounds_app = app;
    let button = Button::new("series_selector", theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Lg)
        .label(label)
        .loading_icon(header_icon(HugeIcon::Loader))
        .caret(header_icon(HugeIcon::ChevronDown))
        .disabled(!enabled)
        .loading(pending)
        .on_click(move |event, window, cx| {
            open_app.update(cx, |app, app_cx| {
                app.open_chrome_overlay_at(
                    ChromeOverlay::Timeframe,
                    event.position(),
                    window,
                    app_cx,
                );
            });
        });
    let trigger = div()
        .relative()
        .flex_none()
        .child(
            canvas(
                move |bounds, _, cx| {
                    bounds_app.update(cx, |app, app_cx| {
                        if app.timeframe_trigger_bounds == Some(bounds) {
                            return;
                        }
                        app.timeframe_trigger_bounds = Some(bounds);
                        if matches!(
                            app.chrome_overlay,
                            Some(ChromeOverlay::Timeframe | ChromeOverlay::QuickTimeframe)
                        ) {
                            app_cx.notify();
                        }
                    });
                },
                |_, (), _, _| {},
            )
            .absolute()
            .inset_0(),
        )
        .child(button);
    chrome_tooltip("series_selector", "Select chart timeframe", trigger, theme)
}

pub(super) fn chart_type_selector(
    app: Entity<WorkspaceSurface>,
    chart_type: ChartType,
    enabled: bool,
    theme: &AerisTheme,
) -> impl IntoElement {
    let open_app = app.clone();
    let bounds_app = app;
    let button = Button::new("chart_type_selector", theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Lg)
        .leading(series_glyph(chart_type, px(chart_chrome::HEADER_ICON_SIZE)))
        .aria_label(chart_type.label())
        .disabled(!enabled)
        .on_click(move |event, window, cx| {
            open_app.update(cx, |app, app_cx| {
                app.open_chrome_overlay_at(
                    ChromeOverlay::ChartType,
                    event.position(),
                    window,
                    app_cx,
                );
            });
        });
    let trigger = div()
        .relative()
        .flex_none()
        .child(
            canvas(
                move |bounds, _, cx| {
                    bounds_app.update(cx, |app, app_cx| {
                        if app.chart_type_trigger_bounds == Some(bounds) {
                            return;
                        }
                        app.chart_type_trigger_bounds = Some(bounds);
                        if app.chrome_overlay == Some(ChromeOverlay::ChartType) {
                            app_cx.notify();
                        }
                    });
                },
                |_, (), _, _| {},
            )
            .absolute()
            .inset_0(),
        )
        .child(button);
    chrome_tooltip("chart_type_selector", "Select chart type", trigger, theme)
}

pub(super) fn time_zone_selector(
    app: Entity<WorkspaceSurface>,
    clock: String,
    time_zone_id: &str,
    theme: &AerisTheme,
) -> impl IntoElement {
    // The zone belongs to the pane, not the chart, so it is selectable before data loads.
    let open_app = app.clone();
    let bounds_app = app;
    let button = Button::new("time_zone_selector", theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Lg)
        .label(clock)
        .caret(header_icon(HugeIcon::ChevronDown))
        .on_click(move |event, window, cx| {
            open_app.update(cx, |app, app_cx| {
                app.open_chrome_overlay_at(
                    ChromeOverlay::TimeZone,
                    event.position(),
                    window,
                    app_cx,
                );
            });
        });
    let trigger = div()
        .relative()
        .flex_none()
        .child(
            canvas(
                move |bounds, _, cx| {
                    bounds_app.update(cx, |app, app_cx| {
                        if app.time_zone_trigger_bounds == Some(bounds) {
                            return;
                        }
                        app.time_zone_trigger_bounds = Some(bounds);
                        if app.chrome_overlay == Some(ChromeOverlay::TimeZone) {
                            app_cx.notify();
                        }
                    });
                },
                |_, (), _, _| {},
            )
            .absolute()
            .inset_0(),
        )
        .child(button);
    chrome_tooltip(
        "time_zone_selector",
        format!("Chart time zone · {time_zone_id}"),
        trigger,
        theme,
    )
}

/// Opens the Accounts panel. The label names the practice account this workspace trades on,
/// so the active account is always visible in the header.
fn accounts_selector(
    app: Entity<WorkspaceSurface>,
    account_label: Option<String>,
    theme: &AerisTheme,
) -> impl IntoElement {
    let open_app = app.clone();
    let bounds_app = app;
    let button = Button::new("accounts_selector", theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Lg)
        .icon(header_icon(HugeIcon::User))
        .label(account_label.unwrap_or_else(|| "Accounts".to_string()))
        .caret(header_icon(HugeIcon::ChevronDown))
        .on_click(move |event, window, cx| {
            open_app.update(cx, |app, app_cx| {
                app.open_chrome_overlay_at(
                    ChromeOverlay::Accounts,
                    event.position(),
                    window,
                    app_cx,
                );
            });
        });
    let trigger = div()
        .relative()
        .flex_none()
        .child(
            canvas(
                move |bounds, _, cx| {
                    bounds_app.update(cx, |app, app_cx| {
                        if app.menu_state.accounts_trigger_bounds == Some(bounds) {
                            return;
                        }
                        app.menu_state.accounts_trigger_bounds = Some(bounds);
                        if app.chrome_overlay == Some(ChromeOverlay::Accounts) {
                            app_cx.notify();
                        }
                    });
                },
                |_, (), _, _| {},
            )
            .absolute()
            .inset_0(),
        )
        .child(button);
    chrome_tooltip(
        "accounts_selector",
        "Accounts and connections",
        trigger,
        theme,
    )
}

type ConnectionColor = fn(&AerisTheme) -> ThemeColor;

pub(super) struct ConnectionPresentation {
    pub(super) provider: &'static str,
    pub(super) status: &'static str,
    pub(super) latency: String,
    pub(super) color: ConnectionColor,
    /// Market phase, session hours and countdown rows appended to the status tooltip.
    pub(super) market_rows: Vec<(&'static str, String)>,
}

impl ConnectionPresentation {
    /// Folds the runtime market session into the header dot. Feed health keeps priority: only a
    /// live feed lets the dot show the market phase (open, extended hours, or closed).
    pub(super) fn with_market(
        mut self,
        feed_live: bool,
        status: Option<&MarketSessionStatus>,
        time_zone: &str,
        now_unix_nanos: i64,
    ) -> Self {
        let Some(status) = status else {
            return self;
        };
        if feed_live && let Some(color) = market_dot_color(status.phase) {
            self.color = color;
        }
        self.market_rows = market_tooltip_rows(status, time_zone, now_unix_nanos);
        self
    }
}

fn market_dot_color(phase: MarketSessionPhase) -> Option<ConnectionColor> {
    match phase {
        MarketSessionPhase::Regular | MarketSessionPhase::AlwaysOpen => {
            Some(|theme| theme.colors.positive)
        }
        MarketSessionPhase::PreMarket
        | MarketSessionPhase::PostMarket
        | MarketSessionPhase::Overnight => Some(|theme| theme.colors.warning),
        MarketSessionPhase::Closed => Some(|theme| theme.colors.text_muted),
        MarketSessionPhase::Unknown => None,
    }
}

const fn market_phase_label(phase: MarketSessionPhase) -> &'static str {
    match phase {
        MarketSessionPhase::Regular => "Open",
        MarketSessionPhase::PreMarket => "Pre-market",
        MarketSessionPhase::PostMarket => "Post-market",
        MarketSessionPhase::Overnight => "Overnight session",
        MarketSessionPhase::Closed => "Closed",
        MarketSessionPhase::AlwaysOpen => "Open 24/7",
        MarketSessionPhase::Unknown => "Hours unavailable",
    }
}

fn market_tooltip_rows(
    status: &MarketSessionStatus,
    time_zone: &str,
    now_unix_nanos: i64,
) -> Vec<(&'static str, String)> {
    let mut rows = vec![("Market", market_phase_label(status.phase).to_string())];
    let wall = |nanos: i64| {
        AerisChartView::time_zone_wall_time_label(time_zone, nanos.div_euclid(1_000_000_000))
            .unwrap_or_else(|| "--:--".to_string())
    };
    if let Some((start, end)) = status
        .session_start_unix_nanos
        .zip(status.session_end_unix_nanos)
    {
        rows.push((
            "Session",
            format!("{}–{} {time_zone}", wall(start), wall(end)),
        ));
    }
    let countdown = match status.phase {
        MarketSessionPhase::Closed => status
            .next_open_unix_nanos
            .map(|open| ("Opens in", countdown_text(open, now_unix_nanos))),
        MarketSessionPhase::Regular
        | MarketSessionPhase::PreMarket
        | MarketSessionPhase::PostMarket
        | MarketSessionPhase::Overnight => status
            .session_end_unix_nanos
            .map(|close| ("Closes in", countdown_text(close, now_unix_nanos))),
        MarketSessionPhase::AlwaysOpen | MarketSessionPhase::Unknown => None,
    };
    rows.extend(countdown);
    rows
}

fn current_unix_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0)
}

/// Whole minutes until `target`, rounded up so "0m" only appears once the boundary passes.
pub(super) fn countdown_text(target_unix_nanos: i64, now_unix_nanos: i64) -> String {
    const MINUTE: i64 = 60_000_000_000;
    let remaining = target_unix_nanos.saturating_sub(now_unix_nanos).max(0);
    let minutes = remaining.saturating_add(MINUTE - 1) / MINUTE;
    let (hours, minutes) = (minutes / 60, minutes % 60);
    if hours == 0 {
        format!("{minutes}m")
    } else {
        format!("{hours}h {minutes}m")
    }
}

pub(super) fn connection_presentation(
    provider: TerminalProvider,
    state: FeedConnectionState,
    transport_rtt_nanos: Option<u64>,
) -> ConnectionPresentation {
    let (status, color): (&'static str, ConnectionColor) = match state {
        FeedConnectionState::Disconnected => ("Offline", |theme| theme.colors.danger),
        FeedConnectionState::Discovering => ("Connecting", |theme| theme.colors.warning),
        FeedConnectionState::Authenticating => ("Authenticating", |theme| theme.colors.warning),
        FeedConnectionState::Streaming => ("Live", |theme| theme.colors.positive),
        FeedConnectionState::Recovering => ("Reconnecting", |theme| theme.colors.warning),
        FeedConnectionState::Stopped => ("Stopped", |theme| theme.colors.danger),
    };
    ConnectionPresentation {
        provider: terminal_provider_display(provider),
        status,
        latency: if state == FeedConnectionState::Streaming {
            format_transport_rtt(transport_rtt_nanos)
        } else {
            "Measuring…".to_string()
        },
        color,
        market_rows: Vec::new(),
    }
}

fn format_transport_rtt(transport_rtt_nanos: Option<u64>) -> String {
    transport_rtt_nanos.map_or_else(
        || "Measuring…".to_string(),
        |nanos| {
            let tenths_of_a_millisecond = nanos / 100_000;
            format!(
                "{}.{:01} ms RTT",
                tenths_of_a_millisecond / 10,
                tenths_of_a_millisecond % 10
            )
        },
    )
}

pub(super) const fn aeris_chart_theme(mode: ThemeMode) -> AerisChartTheme {
    match mode {
        ThemeMode::Light => AerisChartTheme::Light,
        ThemeMode::Dark => AerisChartTheme::Dark,
    }
}

#[cfg(test)]
mod tests {
    use super::{FeedConnectionState, TerminalProvider, shows_rithmic_attribution};

    #[test]
    fn rithmic_attribution_follows_the_rithmic_session_only() {
        for state in [
            FeedConnectionState::Authenticating,
            FeedConnectionState::Streaming,
            FeedConnectionState::Recovering,
        ] {
            assert!(shows_rithmic_attribution(TerminalProvider::Rithmic, state));
            assert!(!shows_rithmic_attribution(
                TerminalProvider::Tastytrade,
                state
            ));
        }
        for state in [
            FeedConnectionState::Disconnected,
            FeedConnectionState::Discovering,
            FeedConnectionState::Stopped,
        ] {
            assert!(!shows_rithmic_attribution(TerminalProvider::Rithmic, state));
        }
    }
}
