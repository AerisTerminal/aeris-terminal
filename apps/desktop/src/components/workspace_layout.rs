use super::*;

pub(super) fn workspace_pane_grid(
    terminal: &Entity<TerminalApp>,
    workspace: &WorkspaceTab,
    theme: &AxiusflowTheme,
    cx: &App,
) -> AnyElement {
    workspace_layout_element(terminal, workspace, &workspace.layout.layout(), theme, cx)
}

pub(super) fn workspace_layout_element(
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

pub(super) fn workspace_split_handle(
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

pub(super) fn workspace_split_ratio(
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

pub(super) fn workspace_pane_element(
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
    let chart_state =
        connectivity_chart_state(surface.chart_state, connection_state, chart_has_market_data);
    let order_book_columns = surface.order_book.read(cx).columns();
    let content = market_workspace(MarketWorkspaceState {
        app: pane.surface.clone(),
        pane_id,
        chart: surface.chart.as_ref(),
        chart_has_market_data,
        chart_is_superseded: surface.showing_superseded_series(),
        order_book: surface.order_book.clone(),
        side_panel: surface.side_panel,
        side_panel_width: surface.side_panel_width,
        order_book_column_menu_open: surface.menu_state.order_book_column_open,
        order_book_columns,
        chart_state,
        chart_status_detail: chart_status_detail(
            chart_state,
            connection_state,
            &surface.chart_state_message,
            surface.connection_message.as_deref(),
        )
        .to_string(),
        theme,
    });
    let price_alert_dialog = surface.price_alert_dialog.as_ref().map(|dialog| {
        price_alert_dialog_layer(
            pane.surface.clone(),
            dialog,
            &surface.price_alerts,
            surface.price_alert_message.as_deref(),
            theme,
        )
    });
    let study_settings_dialog = surface
        .study_settings_dialog
        .as_ref()
        .map(|dialog| study_settings_dialog_layer(&pane.surface, dialog, theme, cx));
    let workspace_id = workspace.id;
    let pane_focus = pane.focus.clone();
    let select_terminal = terminal.clone();
    let context_terminal = terminal.clone();
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
        .on_mouse_down(MouseButton::Right, move |event, _, cx| {
            context_terminal.update(cx, |terminal, terminal_cx| {
                terminal.open_chart_context_menu(
                    ChartContextMenu {
                        workspace_id,
                        pane_id,
                        position: event.position,
                        kind: ChartContextKind::Pane,
                        flyout: PriceAxisMenuFlyout::None,
                        copy_price: None,
                    },
                    terminal_cx,
                );
            });
            cx.stop_propagation();
        })
        .child(content)
        .children(price_alert_dialog)
        .children(study_settings_dialog)
        .into_any_element()
}

pub(super) fn workspace_market_area(
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
    let chrome_focus = surface.chrome_focus.clone();
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
        .track_focus(&chrome_focus)
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
