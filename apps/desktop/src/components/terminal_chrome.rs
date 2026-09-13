use super::chart_context_menus::account_avatar_button;
use super::*;

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

pub(super) const fn workspace_title_bar_visible(is_fullscreen: bool) -> bool {
    !is_fullscreen
}

pub(super) fn terminal_header(
    terminal: &Entity<TerminalApp>,
    app: &Entity<WorkspaceSurface>,
    state: HeaderState,
) -> impl IntoElement + use<> {
    let theme = state.theme;
    let controls = header_controls(terminal, app, state);
    div()
        .w_full()
        .h(px(theme.dimensions.app_header_height))
        .flex()
        .items_center()
        .px_3()
        .border_b_1()
        .border_color(gpui_color(theme.colors.border))
        .bg(gpui_color(theme.colors.surface))
        .child(controls)
}

#[derive(Clone, Copy)]
pub(super) struct WorkspaceTabBarState<'a> {
    pub(super) workspaces: &'a [WorkspaceTab],
    pub(super) active: usize,
    pub(super) enabled: bool,
    pub(super) error: Option<&'a str>,
    pub(super) workspace_drag: Option<WorkspaceDragState>,
    pub(super) theme: AxiusflowTheme,
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
    let tabs = workspace_tab_strip(terminal, state, cx);
    let theme = state.theme;
    let drag_region = workspace_window_drag_region(
        div()
            .id("workspace_window_drag_region")
            .h_full()
            .min_w(px(12.0))
            .flex_1(),
        terminal,
    );
    let account = axiusflow_desktop::account::DesktopAccount::shared().map_or_else(
        axiusflow_desktop::account::unavailable_menu_state,
        |account| account.menu_state(),
    );
    let profile_region = div()
        .id("workspace_profile_region")
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .pl_4()
        .pr_2()
        .gap_2()
        .child(account_avatar_button(terminal, &account, &theme));
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
        .child(workspace_window_controls(terminal, window, &theme))
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
    terminal: Option<&Entity<TerminalApp>>,
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
                .tab_index(tab_index)
                .focus_visible(move |control| {
                    control.border_2().border_color(gpui_color(colors.ring))
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
    theme: &AxiusflowTheme,
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
            tab_index: 0,
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
            tab_index: 1,
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

pub(super) fn onboarding_title_bar(window: &Window, theme: &AxiusflowTheme) -> Div {
    let pointer_owner = caption_pointer_owner(current_caption_platform());
    let drag_region = div().id("onboarding_window_drag_region").h_full().flex_1();
    let drag_region = if current_caption_platform() == CaptionPlatform::Windows {
        drag_region.window_control_area(WindowControlArea::Drag)
    } else {
        drag_region
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.start_window_move();
                cx.stop_propagation();
            })
            .on_click(|event, window, _| {
                if event.click_count() > 1 {
                    window.titlebar_double_click();
                }
            })
    };
    let controls = if pointer_owner == CaptionPointerOwner::System {
        div().h_full()
    } else {
        let supported = window.window_controls();
        let minimize = workspace_caption_control(
            None,
            CaptionControlSpec {
                id: "onboarding_window_minimize",
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
            None,
            CaptionControlSpec {
                id: "onboarding_window_maximize",
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
            None,
            CaptionControlSpec {
                id: "onboarding_window_close",
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
    };

    div()
        .absolute()
        .top_0()
        .left_0()
        .w_full()
        .h(px(WORKSPACE_TITLE_BAR_HEIGHT))
        .flex()
        .items_center()
        .bg(gpui_color(theme.colors.surface))
        .when(cfg!(target_os = "macos"), |bar| bar.pl(px(80.0)))
        .child(drag_region)
        .child(controls)
}

const HEADER_GLOBAL_CONTROLS_WIDTH: f32 = chart_chrome::CHART_CONTROL_SIZE + 8.0;

pub(super) fn header_controls(
    terminal: &Entity<TerminalApp>,
    app: &Entity<WorkspaceSurface>,
    state: HeaderState,
) -> impl IntoElement {
    // Give the global controls an explicit, non-shrinking track. GPUI cannot
    // infer a stable intrinsic width for this mixed Button/avatar group, which
    // previously let the flex item collapse to zero even on wide windows.
    let order_book_toggle = side_panel_toggle(
        app.clone(),
        &state.theme,
        SidePanel::OrderBook,
        state.controls.enabled(HeaderControls::ORDER_BOOK),
        state.order_book_visible,
    );
    let watchlist_toggle = side_panel_toggle(
        app.clone(),
        &state.theme,
        SidePanel::Watchlist,
        true,
        state.watchlist_visible,
    );
    let connection = connection_presentation(
        state.provider,
        state.connection_state,
        state.transport_rtt_nanos,
    );
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
                instruments: state.instruments,
                input: state.symbol_input,
                availability: InstrumentSelectorAvailability {
                    selection_pending: state.pending.symbol_selection,
                    enabled: state.controls.enabled(HeaderControls::INSTRUMENT),
                },
                provider: state.provider,
                catalog_exchange: assets::ExchangeLogo::Rithmic,
                menu: InstrumentSelectorMenu {
                    exchange_open: false,
                    keyboard_selection: 0,
                    keyboard_active: false,
                },
                scroll: state.instrument_scroll,
                target: SymbolSelectionTarget::Chart,
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
            state.chart_type_label,
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
        .child(order_book_toggle)
        .child(watchlist_toggle);
    let global_controls = header_global_controls(terminal, &state.theme);

    div()
        .w_full()
        .h_full()
        .min_w_0()
        .flex_1()
        .flex()
        .items_center()
        .child(market_controls)
        .child(global_controls)
}

fn header_global_controls(
    terminal: &Entity<TerminalApp>,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    div()
        .w(px(HEADER_GLOBAL_CONTROLS_WIDTH))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_end()
        .gap_2()
        .pl_2()
        .child(theme_toggle(terminal.clone(), theme))
}

pub(super) fn side_panel_toggle(
    app: Entity<WorkspaceSurface>,
    theme: &AxiusflowTheme,
    panel: SidePanel,
    enabled: bool,
    selected: bool,
) -> AnyElement {
    let (id, icon, toggle) = match panel {
        SidePanel::OrderBook => (
            "order_book_toggle",
            HugeIcon::SidebarRightIcon01,
            WorkspaceSurface::toggle_order_book
                as fn(&mut WorkspaceSurface, &mut Context<WorkspaceSurface>),
        ),
        SidePanel::Watchlist => (
            "watchlist_toggle",
            HugeIcon::AnalyticsUpIcon,
            WorkspaceSurface::toggle_watchlist
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

pub(super) fn connection_status_indicator(
    presentation: ConnectionPresentation,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let color = (presentation.color)(theme);
    let provider = SharedString::from(presentation.provider);
    let status = SharedString::from(presentation.status);
    let latency = SharedString::from(presentation.latency);
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
    theme: AxiusflowTheme,
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
                .font_family(axiusflow_design_system::platform_font_family())
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
                )),
        )
    }
}

fn connection_tooltip_row(
    label: &'static str,
    value: SharedString,
    numeric: bool,
    theme: &AxiusflowTheme,
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
                .font_family(axiusflow_design_system::platform_font_family())
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
    theme: &AxiusflowTheme,
    app: Entity<WorkspaceSurface>,
) -> impl IntoElement {
    let button = Button::new(state.id)
        .icon(header_icon(state.icon))
        .label(state.label)
        .aria_label(state.tooltip)
        .tooltip(TooltipSpec::new(state.tooltip, theme).show_delay(TOOLTIP_OPEN_DELAY))
        .with_size(px(chart_chrome::HEADER_CONTROL_CONTENT_SIZE))
        .disabled(!state.enabled)
        .when(state.enabled, Button::cursor_pointer)
        .when(!state.enabled, Button::cursor_not_allowed);
    let button = button_activation(button, state.enabled, move |_, cx| {
        app.update(cx, state.toggle);
    });
    chrome_button_style(button, theme, state.selected, state.enabled)
        .when(state.selected, |button| {
            button.bg(gpui_color(theme.colors.surface))
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
            Self::Undo => HugeIcon::Undo03,
            Self::Redo => HugeIcon::Redo01,
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
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let enabled = control.enabled(history);
    let button = Button::new(control.id())
        .icon(header_icon(control.icon()))
        .aria_label(control.tooltip())
        .tooltip(TooltipSpec::new(control.tooltip(), theme).show_delay(TOOLTIP_OPEN_DELAY))
        .with_size(px(chart_chrome::HEADER_CONTROL_CONTENT_SIZE))
        .w(px(chart_chrome::CHART_CONTROL_SIZE))
        .disabled(!enabled)
        .when(enabled, Button::cursor_pointer)
        .when(!enabled, Button::cursor_not_allowed);
    let step = control.step();
    let button = button_activation(button, enabled, move |_, cx| {
        app.update(cx, step);
    });
    chrome_button_style(button, theme, false, enabled)
}

pub(super) fn theme_toggle(
    terminal: Entity<TerminalApp>,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let next = theme.mode.toggled();
    let icon = match next {
        axiusflow_design_system::ThemeMode::Light => HugeIcon::SunIcon03,
        axiusflow_design_system::ThemeMode::Dark => HugeIcon::MoonIcon02,
    };
    let tooltip = format!("Switch to {} theme", next.label());
    let button = Button::new("theme_toggle")
        .tab_index(0)
        .icon(header_icon(icon))
        .aria_label(tooltip.clone())
        .tooltip(TooltipSpec::new(tooltip, theme).show_delay(TOOLTIP_OPEN_DELAY))
        .with_size(px(chart_chrome::HEADER_CONTROL_CONTENT_SIZE))
        .w(px(chart_chrome::CHART_CONTROL_SIZE))
        .cursor_pointer();
    let button = button_activation(button, true, move |window, cx| {
        terminal.update(cx, |terminal, cx| terminal.toggle_theme(window, cx));
    });
    chrome_button_style(button, theme, false, true)
}

pub(super) fn header_icon(name: HugeIcon) -> Icon {
    Icon::default().path(name.path())
}

pub(super) fn series_icon_kind(chart_type: ChartType) -> assets::SeriesIcon {
    match chart_type {
        ChartType::Candles => assets::SeriesIcon::Candlestick,
        ChartType::Bars => assets::SeriesIcon::OhlcBar,
        ChartType::Line | ChartType::Baseline => assets::SeriesIcon::Line,
        ChartType::Area => assets::SeriesIcon::Area,
        ChartType::BrushableArea => assets::SeriesIcon::BrushableArea,
    }
}

pub(super) fn series_glyph(chart_type: ChartType, size: Pixels) -> impl IntoElement {
    VectorImage::square(series_icon_kind(chart_type), size)
}

pub(super) fn exchange_mark(
    logo: assets::ExchangeLogo,
    size: Pixels,
    bordered: bool,
    colors: &axiusflow_design_system::ThemeColors,
) -> Div {
    let glyph_size = if bordered { size - px(4.0) } else { size };
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
        .child(VectorImage::square(logo, glyph_size))
}

pub(super) fn brand_mark_sized(size: Pixels) -> impl IntoElement {
    VectorImage::square(assets::BrandIcon::Mark, size)
}

pub(super) fn chrome_tooltip(
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

pub(super) fn series_selector(
    app: Entity<WorkspaceSurface>,
    label: String,
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
    );
    chrome_tooltip("series_selector", "Select chart timeframe", trigger, theme)
}

pub(super) fn chart_type_selector(
    app: Entity<WorkspaceSurface>,
    chart_type: ChartType,
    label: String,
    enabled: bool,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let button = Button::new("chart_type_selector")
        .leading(series_glyph(chart_type, px(chart_chrome::HEADER_ICON_SIZE)))
        .label(label)
        .caret(header_icon(HugeIcon::ChevronDown))
        .disabled(!enabled)
        .when(enabled, Button::cursor_pointer)
        .when(!enabled, Button::cursor_not_allowed);
    let open_app = app.clone();
    let bounds_app = app;
    let button = button_activation(
        chrome_button_style(button, theme, false, enabled),
        enabled,
        move |window, cx| {
            open_app.update(cx, |app, app_cx| {
                app.open_chrome_overlay(ChromeOverlay::ChartType, window, app_cx);
            });
        },
    );
    let trigger = div().relative().flex_none().child(button).child(
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
    );
    chrome_tooltip("chart_type_selector", "Select chart type", trigger, theme)
}

pub(super) fn chrome_button_style(
    button: Button,
    theme: &AxiusflowTheme,
    selected: bool,
    enabled: bool,
) -> Button {
    let colors = theme.colors;
    button
        .theme(theme)
        .resting_fill(colors.surface)
        .selected(selected)
        .h(px(chart_chrome::CHART_CONTROL_SIZE))
        .border_0()
        .text_color(gpui_color(chrome_control_foreground(
            &colors, selected, enabled,
        )))
        .when(selected, |button| {
            button.bg(gpui_color(colors.active_bg.over(colors.surface)))
        })
}

pub(super) fn chrome_control_foreground(
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

pub(super) fn button_activation(
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

pub(super) struct ConnectionPresentation {
    pub(super) provider: &'static str,
    pub(super) status: &'static str,
    pub(super) latency: String,
    color: ConnectionColor,
}

pub(super) fn connection_presentation(
    provider: TerminalProvider,
    state: FeedConnectionState,
    transport_rtt_nanos: Option<u64>,
) -> ConnectionPresentation {
    let (status, color): (&'static str, ConnectionColor) = match state {
        FeedConnectionState::Disconnected => ("Offline", |theme| theme.colors.danger),
        FeedConnectionState::Discovering => ("Connecting", |theme| theme.colors.primary),
        FeedConnectionState::Authenticating => ("Authenticating", |theme| theme.colors.primary),
        FeedConnectionState::Streaming => ("Live", |theme| theme.colors.primary),
        FeedConnectionState::Recovering => ("Reconnecting", |theme| theme.colors.danger),
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

pub(super) const fn nucleus_chart_theme(mode: ThemeMode) -> NucleusChartTheme {
    match mode {
        ThemeMode::Light => NucleusChartTheme::Light,
        ThemeMode::Dark => NucleusChartTheme::Dark,
    }
}

pub(super) fn gpui_color(color: ThemeColor) -> Hsla {
    let (h, s, l, a) = color.hsla_components();
    Hsla { h, s, l, a }
}
