use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct DrawingToolbarState {
    pub(super) availability: DrawingToolbarAvailability,
    pub(super) active_tool: ChartDrawingTool,
    pub(super) drawing_count: usize,
    pub(super) selection: DrawingToolbarSelection,
    pub(super) selected_locked: bool,
    pub(super) time_axis_height: f32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum DrawingToolbarAvailability {
    #[default]
    Unavailable,
    Available,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum DrawingToolbarSelection {
    #[default]
    None,
    Drawing,
    Series,
}

impl DrawingToolbarState {
    pub(super) fn from_chart(chart: &NucleusChartView) -> Self {
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
            time_axis_height: chart.time_axis_height(),
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

const DRAWING_TOOLS: [DrawingToolSpec; 9] = [
    DrawingToolSpec {
        id: "drawing_cursor",
        label: "Cursor",
        tool: ChartDrawingTool::Cursor,
        icon: DrawingToolIcon::Asset(assets::DrawingIcon::Cursor),
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
        id: "drawing_path",
        label: "Path",
        tool: ChartDrawingTool::Path,
        icon: DrawingToolIcon::Asset(assets::DrawingIcon::Path),
        icon_size: 28.0,
    },
    DrawingToolSpec {
        id: "drawing_brush",
        label: "Brush",
        tool: ChartDrawingTool::Brush,
        icon: DrawingToolIcon::Asset(assets::DrawingIcon::Brush),
        icon_size: 24.0,
    },
    DrawingToolSpec {
        id: "drawing_text",
        label: "Text",
        tool: ChartDrawingTool::Text,
        icon: DrawingToolIcon::Asset(assets::DrawingIcon::Text),
        icon_size: 24.0,
    },
];

pub(super) fn drawing_toolbar(
    terminal: Entity<TerminalApp>,
    app: &Entity<WorkspaceSurface>,
    state: DrawingToolbarState,
    scroll: &ScrollHandle,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let tool_terminal = terminal.clone();
    let tools = DRAWING_TOOLS.into_iter().map(move |spec| {
        let terminal = tool_terminal.clone();
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
                terminal.update(cx, |terminal, terminal_cx| {
                    terminal.select_drawing_tool_on_active_workspace(spec.tool, terminal_cx);
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
                .flex_1()
                .w_full()
                .min_h(px(0.0))
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
                        .child(drawing_toolbar_actions(app, state, theme)),
                )
                .child(ThinScrollbar::new(
                    scroll,
                    gpui_color(colors.text_secondary),
                )),
        )
        .child(drawing_toolbar_collapse(
            terminal,
            state.time_axis_height,
            theme,
        ))
}

fn drawing_toolbar_actions(
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
                HugeIcon::LockKeyholeIcon,
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
                "drawing_clear_all",
                "Clear all drawings",
                HugeIcon::EraserIcon,
                24.0,
                false,
                state.drawing_count > 0,
                WorkspaceSurface::clear_drawings,
            ),
            app.clone(),
            theme,
        ))
}

const DRAWING_TOOLBAR_TOGGLE_ICON: f32 = 14.0;

fn drawing_toolbar_collapse(
    terminal: Entity<TerminalApp>,
    time_axis_height: f32,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    drawing_toolbar_toggle_hit(
        "drawing_toolbar_collapse",
        HugeIcon::LayoutAlignLeftIcon,
        "Collapse drawing toolbar",
        theme,
        move |_, cx| terminal.update(cx, TerminalApp::toggle_drawing_toolbar),
    )
    .flex_none()
    .w_full()
    .h(px(time_axis_height))
    .border_t_1()
    .border_color(gpui_color(colors.border))
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

pub(super) fn drawing_toolbar_expander(
    terminal: Entity<TerminalApp>,
    time_axis_height: f32,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    drawing_toolbar_toggle_hit(
        "drawing_toolbar_expand",
        HugeIcon::LayoutAlignLeftIcon,
        "Expand drawing toolbar",
        theme,
        move |_, cx| terminal.update(cx, TerminalApp::toggle_drawing_toolbar),
    )
    .absolute()
    .left_0()
    .bottom_0()
    .w(px(chart_chrome::CHART_CHROME_HEIGHT))
    .h(px(time_axis_height))
    .border_t_1()
    .border_r_1()
    .border_color(gpui_color(colors.border))
    .bg(gpui_color(colors.surface))
}

fn drawing_toolbar_toggle_hit(
    id: &'static str,
    icon: HugeIcon,
    tooltip: &'static str,
    theme: &AxiusflowTheme,
    on_activate: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let colors = theme.colors;
    let spec = TooltipSpec::new(tooltip, theme).show_delay(TOOLTIP_OPEN_DELAY);
    div()
        .id(id)
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(0.0))
        .text_color(gpui_color(colors.icon))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(tooltip)
        .hover(move |hit| hit.bg(gpui_color(colors.hover_bg.over(colors.surface))))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            on_activate(window, cx);
            cx.stop_propagation();
        })
        .tooltip(spec.builder())
        .tooltip_show_delay(spec.delay())
        .child(header_icon(icon).with_size(px(DRAWING_TOOLBAR_TOGGLE_ICON)))
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
