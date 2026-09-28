use super::*;

pub(super) fn workspace_pane_grid(
    terminal: &Entity<TerminalApp>,
    workspace: &WorkspaceTab,
    theme: &AerisTheme,
    cx: &App,
) -> AnyElement {
    if let Some(pane_id) = workspace.maximized_pane {
        return workspace_pane_element(terminal, workspace, pane_id, theme, cx);
    }
    workspace_layout_element(terminal, workspace, &workspace.layout.layout(), theme, cx)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WorkspaceMaximizeTransition {
    Ignore,
    Set(Option<u64>),
}

pub(super) fn workspace_maximize_transition(
    current: Option<u64>,
    pane_id: u64,
    pane_count: usize,
) -> WorkspaceMaximizeTransition {
    if pane_count < 2 && current.is_none() {
        return WorkspaceMaximizeTransition::Ignore;
    }
    WorkspaceMaximizeTransition::Set(if current == Some(pane_id) {
        None
    } else {
        Some(pane_id)
    })
}

pub(super) fn workspace_layout_element(
    terminal: &Entity<TerminalApp>,
    workspace: &WorkspaceTab,
    layout: &ChartWorkspaceLayout,
    theme: &AerisTheme,
    cx: &App,
) -> AnyElement {
    if let ChartWorkspaceLayout::Cell { id } = layout {
        return workspace_pane_element(terminal, workspace, *id, theme, cx);
    }

    let ChartWorkspaceLayout::Split {
        direction,
        ratio,
        a: first,
        b: second,
    } = layout
    else {
        return div().into_any_element();
    };
    let first_ids = first.leaf_ids();
    let second_ids = second.leaf_ids();
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
    theme: &AerisTheme,
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
    let content = market_workspace(MarketWorkspaceState {
        pane_id,
        chart: surface.chart.as_ref(),
        chart_has_market_data,
        chart_is_superseded: surface.showing_superseded_series(),
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
    let study_settings_dialog = surface
        .study_settings_dialog
        .as_ref()
        .map(|dialog| study_settings_dialog_layer(&pane.surface, dialog, theme, cx));
    let workspace_id = workspace.id;
    let pane_focus = pane.focus.clone();
    let select_terminal = terminal.clone();
    let context_terminal = terminal.clone();
    let maximize_terminal = terminal.clone();
    let release_terminal = terminal.clone();
    div()
        .id(("workspace_pane", pane_id))
        .relative()
        .size_full()
        .min_w_0()
        .min_h_0()
        // Keep the chart canvas flush with the pane's right edge. Aeris draws structural
        // indicator separators across its complete viewport; host padding here would shorten
        // every separator and leave a visible break before the workspace boundary.
        .pb(px(WORKSPACE_PANE_BOTTOM_INSET))
        // Match the Aeris Charts grid contract: Alt+primary-click toggles one cell over the full
        // workspace. Capture and consume the press before the chart can pan, select, or place a
        // drawing, then consume the corresponding release after the layout has changed.
        .capture_any_mouse_down(move |event, _, app| {
            let toggled = maximize_terminal.update(app, |terminal, cx| {
                terminal.begin_workspace_pane_alt_click(
                    workspace_id,
                    pane_id,
                    event.button == MouseButton::Left && event.modifiers.alt,
                    cx,
                )
            });
            if toggled {
                app.stop_propagation();
            }
        })
        .capture_any_mouse_up(move |_, _, app| {
            let swallow = release_terminal.update(app, |terminal, _| {
                terminal.take_workspace_pane_mouse_up(workspace_id)
            });
            if swallow {
                app.stop_propagation();
            }
        })
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
                        copy_feedback_generation: None,
                    },
                    terminal_cx,
                );
            });
            cx.stop_propagation();
        })
        .child(content)
        .children(study_settings_dialog)
        .into_any_element()
}

pub(super) fn workspace_market_area(
    terminal: &Entity<TerminalApp>,
    workspace: &WorkspaceTab,
    active_surface: &Entity<WorkspaceSurface>,
    drawing_toolbar_collapsed: bool,
    watchlist: WatchlistPanelState,
    theme: &AerisTheme,
    cx: &mut Context<TerminalApp>,
) -> impl IntoElement + use<> {
    refresh_trading_pnl(active_surface.clone(), cx);
    let grid = workspace_pane_grid(terminal, workspace, theme, cx);
    let price_alert_dialog = workspace.panes.iter().find_map(|pane| {
        let surface = pane.surface.read(cx);
        surface.price_alert_dialog.as_ref().map(|dialog| {
            price_alert_dialog_layer(
                pane.surface.clone(),
                dialog,
                &surface.price_alerts,
                surface.price_alert_message.as_deref(),
                theme,
            )
        })
    });
    let surface = active_surface.read(cx);
    let drawing_state = surface.drawing_toolbar_state(cx);
    let drawing_scroll = surface.scrolls.drawing.clone();
    let chrome_focus = surface.chrome_focus.clone();
    let order_book_frame = surface.order_book.read(cx).frame().cloned();
    let side_panel = surface.side_panels.any().then(|| {
        workspace_side_panel(WorkspaceSidePanelState {
            app: active_surface.clone(),
            terminal: terminal.clone(),
            workspace_id: workspace.id,
            visible: surface.side_panels,
            width: surface.side_panel_width,
            split_basis_points: surface.side_panel_split_basis_points,
            order_book: OrderBookPanelState {
                order_book: &surface.order_book,
                column_menu_open: surface.menu_state.order_book_column_open,
                columns: surface.order_book.read(cx).columns(),
                ticket: TradingOrderControlsState {
                    app: active_surface,
                    frame: order_book_frame.as_ref(),
                    trading_pnl: surface.trading_pnl.current.as_ref(),
                    accounts: &surface.trading_pnl.accounts,
                    orders: &surface.trading_pnl.orders,
                    positions: &surface.trading_pnl.positions,
                    risk_meters: &surface.trading_pnl.risk_meters,
                    risk_locks: &surface.trading_pnl.risk_locks,
                    session_plans: &surface.trading_pnl.session_plans,
                    session_reviews: &surface.trading_pnl.session_reviews,
                    trade_copiers: &surface.trading_pnl.trade_copiers,
                    copy_dispatches: &surface.trading_pnl.copy_dispatches,
                    strategy_templates: &surface.trading_pnl.strategy_templates,
                    managed_brackets: &surface.trading_pnl.managed_brackets,
                    order_entry: &surface.trading_pnl.order_entry,
                    theme,
                },
            },
            time_sales: TimeSalesPanelState {
                app: active_surface.clone(),
                tape: surface.trade_tape.as_ref(),
                sweeps: &surface.trade_sweeps,
                product: surface.product.as_ref(),
                book: order_book_frame.as_ref(),
                filter: surface.time_sales_filter,
                scroll: &surface.scrolls.time_sales,
            },
            watchlist,
            theme,
        })
    });
    let context = workspace_context_panel(surface, active_surface, theme);
    let center = workspace_center_column(grid, context, drawing_toolbar_collapsed, active_surface);
    div()
        .relative()
        .flex()
        .size_full()
        .overflow_hidden()
        .track_focus(&chrome_focus)
        .child(center)
        .children(side_panel)
        .when(drawing_toolbar_collapsed, |market| {
            market.child(drawing_toolbar_expander(
                terminal.clone(),
                drawing_state.time_axis_height,
                theme,
            ))
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
        .children(price_alert_dialog)
}

/// Chart grid plus the bottom context panel, inset past the drawing toolbar.
fn workspace_center_column(
    grid: impl IntoElement,
    context: Option<AnyElement>,
    drawing_toolbar_collapsed: bool,
    active_surface: &Entity<WorkspaceSurface>,
) -> Div {
    let grid = div().flex_1().min_w_0().min_h_0().child(grid);
    let context_drag_app = active_surface.clone();
    // The drawing toolbar overlays the left edge of the whole center column, so
    // the chart grid and the context panel beneath it share one inset.
    div()
        .h_full()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .when(!drawing_toolbar_collapsed, |center| {
            center.ml(px(chart_chrome::CHART_CHROME_HEIGHT))
        })
        .child(grid)
        .children(context)
        .on_drag_move::<ContextPanelHeightDrag>(move |event, _, cx| {
            let bottom = f32::from(event.bounds.bottom());
            let available =
                f32::from(event.bounds.size.height) - CONTEXT_PANEL_MINIMUM_CHART_HEIGHT;
            let height = (bottom - f32::from(event.event.position.y)).min(available);
            context_drag_app.update(cx, |surface, surface_cx| {
                surface.set_context_panel_height(height, surface_cx);
            });
        })
}

fn workspace_context_panel(
    surface: &WorkspaceSurface,
    entity: &Entity<WorkspaceSurface>,
    theme: &AerisTheme,
) -> Option<AnyElement> {
    surface.context_panel_visible.then(|| {
        context_panel(ContextPanelState {
            app: entity.clone(),
            height: surface.context_panel_height,
            snapshot: &surface.context_snapshot,
            tab: surface.context_panel_tab,
            scroll: surface.scrolls.context.clone(),
            credential_dialog: surface.context_credential_dialog.as_ref(),
            credential_message: surface.context_credential_message.as_deref(),
            risk_message: surface.economic_event_risk_message.as_deref(),
            theme,
        })
        .into_any_element()
    })
}

fn project_working_order_markers(
    state: &WorkspaceSurface,
    cx: &mut Context<WorkspaceSurface>,
) -> (
    Vec<aeris_terminal_ui::OrderBookWorkingOrder>,
    Option<aeris_terminal_ui::OrderBookPositionMarker>,
) {
    let selected_account = state.trading_pnl.order_entry.selected_account_id.as_ref();
    let Some(instrument_id) = state
        .order_book
        .read(cx)
        .frame()
        .map(|frame| frame.instrument_id.as_str())
    else {
        return (Vec::new(), None);
    };
    let working_orders = state
        .trading_pnl
        .orders
        .iter()
        .filter(|order| order.status.is_open())
        .filter(|order| selected_account.is_some_and(|account_id| &order.account_id == account_id))
        .filter(|order| order.instrument_id.as_str() == instrument_id)
        .filter_map(|order| {
            let price = order.limit_price?;
            Some(aeris_terminal_ui::OrderBookWorkingOrder {
                price: price.units(),
                side: match order.side {
                    aeris_trading::OrderSide::Buy => aeris_terminal_ui::OrderBookLevelSide::Ask,
                    aeris_trading::OrderSide::Sell => aeris_terminal_ui::OrderBookLevelSide::Bid,
                },
                quantity: order.quantity.units(),
                quantity_scale: order.quantity.scale(),
            })
        })
        .take(8)
        .collect();
    let position_marker = selected_account.and_then(|account_id| {
        let position = state.trading_pnl.positions.iter().find(|position| {
            &position.position.account_id == account_id
                && position.position.instrument_id.as_str() == instrument_id
        })?;
        let price = position.position.average_entry_price?;
        let net_quantity = position.position.net_quantity.units();
        if net_quantity == 0 {
            return None;
        }
        let quantity = i64::try_from(net_quantity.unsigned_abs()).ok()?;
        Some(aeris_terminal_ui::OrderBookPositionMarker {
            price: price.units(),
            side: if net_quantity > 0 {
                aeris_terminal_ui::OrderBookLevelSide::Ask
            } else {
                aeris_terminal_ui::OrderBookLevelSide::Bid
            },
            quantity,
            quantity_scale: position.position.net_quantity.scale(),
            point_value: position.point_value,
            currency_scale: position.currency_scale,
        })
    });
    (working_orders, position_marker)
}

fn trading_pnl_refresh_due(
    surface: &Entity<WorkspaceSurface>,
    cx: &App,
    now: std::time::Instant,
) -> bool {
    let state = surface.read(cx);
    !state.trading_pnl.refresh_pending && now >= state.trading_pnl.next_refresh
}

fn refresh_trading_pnl(surface: Entity<WorkspaceSurface>, cx: &mut Context<TerminalApp>) {
    let now = std::time::Instant::now();
    if !trading_pnl_refresh_due(&surface, cx, now) {
        return;
    }
    let Some(service) = aeris_desktop::trading::handle() else {
        return;
    };
    surface.update(cx, |state, _| {
        state.trading_pnl.refresh_pending = true;
        state.trading_pnl.next_refresh = now + std::time::Duration::from_millis(250);
    });
    let snapshot = cx
        .background_executor()
        .spawn(async move { service.snapshot() });
    cx.spawn(async move |_, cx| {
        let result = snapshot.await;
        surface.update(cx, |state, state_cx| {
            state.trading_pnl.refresh_pending = false;
            if let Ok(snapshot) = result {
                let selected_account_id = state.trading_pnl.order_entry.selected_account_id.clone();
                let chart_snapshot = crate::desktop::chart_trading_snapshot(
                    &snapshot,
                    state.product.as_ref(),
                    selected_account_id.as_ref(),
                );
                let host_overlay = crate::desktop::chart_session_plan_overlay(
                    &snapshot,
                    selected_account_id.as_ref(),
                );
                let session_plan_levels = crate::desktop::chart_session_plan_levels(
                    &snapshot,
                    state.product.as_ref(),
                    selected_account_id.as_ref(),
                );
                let accounts = snapshot.accounts;
                let account_pnl = snapshot.account_pnl;
                state.trading_pnl.orders = snapshot.orders;
                state.trading_pnl.fills = snapshot.fills;
                state.trading_pnl.positions = snapshot.position_pnl;
                state.trading_pnl.risk_meters = snapshot.risk_meters;
                state.trading_pnl.risk_profiles = snapshot.risk_profiles;
                state.trading_pnl.risk_locks = snapshot.risk_locks;
                state.trading_pnl.session_plans = snapshot.session_plans;
                state.trading_pnl.session_reviews = snapshot.session_adherence_reviews;
                state.trading_pnl.trade_copiers = snapshot.trade_copiers;
                state.trading_pnl.copy_dispatches = snapshot.copy_dispatches;
                state.trading_pnl.strategy_templates = snapshot.strategy_templates;
                state.trading_pnl.managed_brackets = snapshot.managed_brackets;
                state.trading_pnl.accounts = accounts;
                if state
                    .trading_pnl
                    .order_entry
                    .selected_account_id
                    .as_ref()
                    .is_none_or(|account_id| {
                        !state
                            .trading_pnl
                            .accounts
                            .iter()
                            .any(|account| &account.id == account_id)
                    })
                {
                    state.trading_pnl.order_entry.selected_account_id = state
                        .trading_pnl
                        .accounts
                        .first()
                        .map(|account| account.id.clone());
                }
                state.trading_pnl.current = account_pnl.into_iter().find(|pnl| {
                    state
                        .trading_pnl
                        .order_entry
                        .selected_account_id
                        .as_ref()
                        .is_none_or(|account_id| account_id == &pnl.account_id)
                });
                let (working_orders, position_marker) =
                    project_working_order_markers(state, state_cx);
                state
                    .order_book
                    .update(state_cx, |order_book, order_book_cx| {
                        order_book.set_working_orders(working_orders, order_book_cx);
                        order_book.set_position_marker(position_marker, order_book_cx);
                    });
                if let Some(chart) = state.chart.as_ref() {
                    refresh_chart_trading_projection(
                        chart,
                        chart_snapshot,
                        host_overlay,
                        session_plan_levels,
                        state_cx,
                    );
                }
                state_cx.notify();
            }
        });
    })
    .detach();
}

fn refresh_chart_trading_projection(
    chart: &Entity<AerisChartView>,
    trading: Option<ChartTradingSnapshot>,
    host_overlay: ChartHostOverlaySnapshot,
    session_plan_levels: Vec<(f64, String)>,
    cx: &mut Context<WorkspaceSurface>,
) {
    if let Some(trading) = trading
        && chart.read(cx).trading_snapshot() != trading
    {
        let _ = chart.update(cx, |chart, _| chart.set_trading_snapshot(trading));
    }
    if chart.read(cx).host_overlay() != host_overlay {
        let _ = chart.update(cx, |chart, _| chart.set_host_overlay(host_overlay));
    }
    if chart.read(cx).session_plan_levels() != session_plan_levels {
        let _ = chart.update(cx, |chart, _| {
            chart.replace_session_plan_levels(session_plan_levels)
        });
    }
}
