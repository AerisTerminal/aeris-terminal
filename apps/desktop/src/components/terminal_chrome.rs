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
            .gap_2()
            .text_sm()
            .child(brand_mark())
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
                .child(brand_region)
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
                control.bg(gpui_color(colors.hover_bg.over(colors.surface_secondary)))
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
                    control.border_2().border_color(gpui_color(colors.ring))
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

pub(super) fn header_controls(
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
    );
    div()
        .h_full()
        .flex()
        .items_center()
        .gap_2()
        .child(account_avatar_button(
            terminal,
            &state.account,
            &state.theme,
        ))
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
                availability: InstrumentSelectorAvailability {
                    selection_pending: state.pending.symbol_selection,
                    enabled: state.controls.enabled(HeaderControls::INSTRUMENT),
                },
                provider: state.provider,
                catalog_exchange: assets::ExchangeLogo::Coinbase,
                menu: InstrumentSelectorMenu {
                    exchange_open: false,
                    keyboard_selection: 0,
                    keyboard_active: false,
                },
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
        .child(dom_toggle)
        .child(theme_toggle(terminal.clone(), &state.theme))
}

pub(super) fn side_panel_toggle(
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

pub(super) fn connection_status_indicator(
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

pub(super) fn svg_intrinsic_width(bytes: &[u8]) -> Option<f32> {
    let header = std::str::from_utf8(bytes.get(..768)?).ok()?;
    let svg = header.find("<svg")?;
    let width = header[svg..].find("width=\"")? + svg + 7;
    let rest = &header[width..];
    let end = rest.find('"')?;
    rest[..end].parse().ok()
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

    let bytes = assets::AxiusflowAssets
        .load(path.as_ref())
        .map_err(|error| gpui::ImageCacheError::Other(Arc::new(error)))?
        .ok_or_else(|| {
            gpui::ImageCacheError::Asset(format!("Embedded resource not found: {path}").into())
        })?;
    let intrinsic = svg_intrinsic_width(&bytes).ok_or_else(|| {
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
        colored_svg_element(&self.path, self.size, window.scale_factor(), cx)
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
        .child(ColoredSvgMark {
            path: logo.path(),
            size: glyph_size,
        })
}

pub(super) fn brand_mark() -> impl IntoElement {
    ColoredSvgMark {
        path: assets::BrandIcon::Mark.path(),
        size: px(28.0),
    }
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

pub(super) fn connection_presentation(
    provider: TerminalProvider,
    state: FeedConnectionState,
    chart_state: ChartState,
    delayed: bool,
) -> (String, ConnectionColor) {
    let provider = match provider {
        TerminalProvider::Coinbase => "Coinbase",
        TerminalProvider::Rithmic => "Test",
    };
    // Provider connectivity outranks chart readiness. Buffered publications and
    // history transitions can continue while the transport is offline; letting
    // those states choose the label makes the status oscillate and can paint an
    // offline feed green.
    match state {
        FeedConnectionState::Disconnected => {
            return ("Offline".to_string(), |theme| theme.colors.danger);
        }
        FeedConnectionState::Recovering => {
            return (format!("{provider} · Reconnecting"), |theme| {
                theme.colors.bearish
            });
        }
        FeedConnectionState::Stopped => {
            return ("Stopped".to_string(), |theme| theme.colors.danger);
        }
        FeedConnectionState::Discovering
        | FeedConnectionState::Authenticating
        | FeedConnectionState::Streaming => {}
    }
    if chart_state == ChartState::Stale {
        return (format!("{provider} · Stale"), |theme| theme.colors.bearish);
    }
    if chart_state == ChartState::Recovering {
        return (format!("{provider} · Reconnecting"), |theme| {
            theme.colors.bearish
        });
    }
    // A switch or a first load is in flight. The trader is waiting on this
    // chart, not watching a feed fail, and calling that "Reconnecting" is what
    // made an ordinary switch look like an outage. A real outage still outranks
    // it, because then the load is not going to finish.
    if chart_state == ChartState::Loading
        && !matches!(
            state,
            FeedConnectionState::Disconnected | FeedConnectionState::Stopped
        )
    {
        if state == FeedConnectionState::Recovering {
            return (format!("{provider} · Reconnecting"), |theme| {
                theme.colors.bearish
            });
        }
        return (format!("{provider} · Loading"), |theme| {
            theme.colors.primary
        });
    }
    if chart_state == ChartState::Error && state == FeedConnectionState::Streaming {
        return (format!("{provider} · Data error"), |theme| {
            theme.colors.danger
        });
    }
    if state == FeedConnectionState::Streaming && delayed {
        return (format!("{provider} · Delayed"), |theme| {
            theme.colors.bearish
        });
    }
    match state {
        FeedConnectionState::Disconnected
        | FeedConnectionState::Recovering
        | FeedConnectionState::Stopped => unreachable!("terminal connectivity handled above"),
        FeedConnectionState::Discovering => (format!("{provider} · Discovering"), |theme| {
            theme.colors.primary
        }),
        FeedConnectionState::Authenticating => (format!("{provider} · Authenticating"), |theme| {
            theme.colors.primary
        }),
        FeedConnectionState::Streaming => {
            (format!("{provider} · Live"), |theme| theme.colors.bullish)
        }
    }
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
