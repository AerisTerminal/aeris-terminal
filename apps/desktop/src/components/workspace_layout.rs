use super::toaster::{ToastLayer, ToastStack, toast_stack};
use super::*;
use aeris_desktop::command_registry::{self, WorkspaceShortcut};

pub(super) fn workspace_pane_grid(
    terminal: &Entity<TerminalApp>,
    workspace: &WorkspaceTab,
    toaster: &Toaster,
    theme: &AerisTheme,
    cx: &App,
) -> AnyElement {
    if let Some(pane_id) = workspace.maximized_pane {
        return workspace_pane_element(terminal, workspace, pane_id, toaster, theme, cx);
    }
    let layout = workspace.layout.layout();
    workspace_layout_element(terminal, workspace, &layout, toaster, theme, cx)
}

/// Toasts that belong to the workspace rather than one chart, such as fills, stack on the pane
/// in its top-right corner.
pub(super) fn toast_host(workspace: &WorkspaceTab) -> Option<ToastStack> {
    let pane_id = workspace
        .maximized_pane
        .unwrap_or_else(|| top_right_pane(&workspace.layout.layout()));
    workspace
        .panes
        .iter()
        .find(|pane| pane.id == pane_id)
        .map(|pane| ToastStack::of(&pane.surface))
}

fn top_right_pane(layout: &ChartWorkspaceLayout) -> u64 {
    match layout {
        ChartWorkspaceLayout::Cell { id } => *id,
        ChartWorkspaceLayout::Split {
            direction: ChartSplitDirection::Horizontal,
            b: right,
            ..
        } => top_right_pane(right),
        ChartWorkspaceLayout::Split {
            direction: ChartSplitDirection::Vertical,
            a: top,
            ..
        } => top_right_pane(top),
    }
}

/// The chart notice a pane shows now, from its chart state, connection and data.
pub(super) fn pane_chart_notice(
    surface: &WorkspaceSurface,
    cx: &App,
) -> Option<ChartSurfaceNotice> {
    let connection_state = surface
        .connection_state
        .unwrap_or(FeedConnectionState::Disconnected);
    let chart_has_market_data = surface
        .chart
        .as_ref()
        .is_some_and(|chart| chart.read(cx).has_market_data());
    let chart_state =
        connectivity_chart_state(surface.chart_state, connection_state, chart_has_market_data);
    chart_surface_notice(
        chart_state,
        chart_has_market_data,
        surface.showing_superseded_series(),
        chart_status_detail(
            chart_state,
            connection_state,
            surface.chart_failure,
            surface.chart_pane_controller.pending_interval(),
        )
        .as_str(),
    )
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
    toaster: &Toaster,
    theme: &AerisTheme,
    cx: &App,
) -> AnyElement {
    if let ChartWorkspaceLayout::Cell { id } = layout {
        return workspace_pane_element(terminal, workspace, *id, toaster, theme, cx);
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
    let first_element = workspace_layout_element(terminal, workspace, first, toaster, theme, cx);
    let second_element = workspace_layout_element(terminal, workspace, second, toaster, theme, cx);
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

/// The settings dialog open on a pane, drawn over that pane only.
fn pane_settings_dialog(
    entity: &Entity<WorkspaceSurface>,
    surface: &WorkspaceSurface,
    theme: &AerisTheme,
    cx: &App,
) -> Option<AnyElement> {
    match surface.indicator_settings_dialog.as_ref()? {
        IndicatorSettingsDialog::Study(dialog) => {
            Some(study_settings_dialog_layer(entity, dialog, theme, cx))
        }
        IndicatorSettingsDialog::BigTrades(dialog) => {
            Some(big_trades_dialog_layer(entity, dialog, theme))
        }
        IndicatorSettingsDialog::Volume(dialog) => Some(volume_dialog_layer(entity, dialog, theme)),
        IndicatorSettingsDialog::OrderFlow(dialog) => {
            Some(order_flow_dialog_layer(entity, dialog, theme))
        }
        IndicatorSettingsDialog::Drawing(dialog) => {
            drawing_settings_dialog_layer(entity, dialog, theme, cx)
        }
    }
}

pub(super) fn workspace_pane_element(
    terminal: &Entity<TerminalApp>,
    workspace: &WorkspaceTab,
    pane_id: u64,
    toaster: &Toaster,
    theme: &AerisTheme,
    cx: &App,
) -> AnyElement {
    let Some(pane) = workspace.panes.iter().find(|pane| pane.id == pane_id) else {
        return div().into_any_element();
    };
    let surface = pane.surface.read(cx);
    let toasts = toast_stack(&ToastLayer {
        host: terminal,
        toaster,
        stack: ToastStack::of(&pane.surface),
        price_axis_width: surface
            .chart
            .as_ref()
            .map_or(0.0, |chart| chart.read(cx).price_axis_width()),
        theme,
        now: Instant::now(),
    });
    let content = market_workspace(MarketWorkspaceState {
        pane_id,
        chart: surface.chart.as_ref(),
        notice: pane_chart_notice(surface, cx),
        toasts,
        theme,
    });
    let indicator_settings_dialog = pane_settings_dialog(&pane.surface, surface, theme, cx);
    let quantity_menu =
        super::order_quantity::order_quantity_menu_layer(&pane.surface, surface, theme);
    let workspace_id = workspace.id;
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
                    command_registry::workspace_click_shortcut(event.button, event.modifiers)
                        == Some(WorkspaceShortcut::MaximizePane),
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
                terminal.focus_workspace(window, terminal_cx);
            });
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
                        capture_flyout_open: false,
                        copy_price: None,
                        copy_feedback: None,
                    },
                    terminal_cx,
                );
            });
            cx.stop_propagation();
        })
        .child(content)
        .children(quantity_menu)
        .children(indicator_settings_dialog)
        .into_any_element()
}

pub(super) struct WorkspaceMarketArea<'a> {
    pub(super) terminal: &'a Entity<TerminalApp>,
    pub(super) workspace: &'a WorkspaceTab,
    pub(super) active_surface: &'a Entity<WorkspaceSurface>,
    pub(super) expanded_drawing_toolbar: Option<DrawingSidebar<'a>>,
    pub(super) watchlist: WatchlistPanelState,
    pub(super) toaster: &'a Toaster,
    pub(super) theme: &'a AerisTheme,
}

pub(super) fn workspace_market_area(
    area: WorkspaceMarketArea<'_>,
    cx: &mut Context<TerminalApp>,
) -> impl IntoElement + use<> {
    let WorkspaceMarketArea {
        terminal,
        workspace,
        active_surface,
        expanded_drawing_toolbar,
        watchlist,
        toaster,
        theme,
    } = area;
    let drawing_toolbar_collapsed = expanded_drawing_toolbar.is_none();
    let grid = workspace_pane_grid(terminal, workspace, toaster, theme, cx);
    let price_alert_dialog = workspace.panes.iter().find_map(|pane| {
        let surface = pane.surface.read(cx);
        surface.price_alert_dialog.as_ref().map(|dialog| {
            price_alert_dialog_layer(&pane.surface, dialog, &surface.price_alerts, theme)
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
                app: active_surface,
                order_book: &surface.order_book,
                column_menu_open: surface.menu_state.order_book_column_open,
                columns: surface.order_book.read(cx).columns(),
                theme,
            },
            order_entry: TradingOrderControlsState {
                app: active_surface,
                frame: order_book_frame.as_ref(),
                product: surface.product.as_ref(),
                trading: &surface.trading_pnl,
                quantity: &surface.order_quantity,
                controls: &surface.order_ticket,
                theme,
            },
            time_sales: TimeSalesPanelState {
                app: active_surface.clone(),
                tape: surface.trade_tape.as_ref(),
                rows_cache: &surface.time_sales.rows,
                sweeps: &surface.trade_sweeps,
                product: surface.product.as_ref(),
                book: order_book_frame.as_ref(),
                filter: surface.time_sales.filter,
                scroll: &surface.scrolls.time_sales,
                trades_unavailable_from: (!super::provider_trades_available(surface.provider))
                    .then(|| surface.provider.display()),
            },
            watchlist,
            theme,
        })
    });
    let panel_sidebar = super::terminal_chrome::panel_sidebar(
        active_surface,
        super::terminal_chrome::PanelSidebarState {
            side_panels: surface.side_panels,
            context_visible: surface.context_panel_visible,
            market_selected: surface.has_market_selection(),
        },
        theme,
    );
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
        .child(panel_sidebar)
        .when_some(expanded_drawing_toolbar, |market, sidebar| {
            market.child(drawing_toolbar(
                terminal,
                active_surface,
                drawing_state,
                sidebar,
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
            now_unix_seconds: surface
                .lifecycle
                .clock()
                .wall_unix_nanos()
                .div_euclid(1_000_000_000),
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
    let Some((instrument_id, price_scale)) = state
        .order_book
        .read(cx)
        .frame()
        .map(|frame| (frame.instrument_id.as_str(), frame.price_scale))
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
            let (side, price) = crate::desktop::order_book_row_price(order)?;
            Some(aeris_terminal_ui::OrderBookWorkingOrder {
                price: price.units(),
                side,
                order_side: order.side,
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
        // An average entry finer than the book's price grid has no ladder row to mark.
        let price = position
            .position
            .average_entry_price?
            .exact_rescale(price_scale)
            .ok()?;
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

/// Apply only a new subscription publication, never a timer or render-path read.
impl TerminalApp {
    pub(super) fn refresh_trading_pane_for_selection(
        &self,
        pane: &Entity<WorkspaceSurface>,
        cx: &mut Context<Self>,
    ) {
        let needs_refresh = {
            let state = pane.read(cx);
            state.trading_pnl.applied_selection.as_ref()
                != Some(&(
                    state
                        .product
                        .as_ref()
                        .map(|product| product.instrument_id.clone()),
                    state.trading_pnl.order_entry.selected_account_id.clone(),
                    state.chart_trading_visibility,
                ))
        };
        if needs_refresh && let Some(current) = self.trading_store.read(cx).current() {
            // Selection is rare; the event path is the only regular WorkingSet drain.
            let current = current.clone();
            pane.update(cx, |state, pane_cx| {
                apply_trading_snapshot(state, &current, pane_cx);
            });
        }
    }

    pub(super) fn apply_trading_publication(&mut self, cx: &mut Context<Self>) {
        let view = self.trading_store.update(cx, |store, _| {
            store
                .drain()
                .cloned()
                .map(|snapshot| (snapshot, store.take_resync()))
        });
        let Some((snapshot, resync)) = view else {
            return;
        };
        let feedback = self.lifecycle.trading_feedback();
        feedback.record_venue_notices(&snapshot.venue_notices);
        let latest = feedback.latest();
        if resync {
            self.bottom_panel.invalidate_trading_revision();
        }
        if self.bottom_panel.apply_snapshot(&snapshot) {
            cx.notify();
        }
        self.announce_trading(&snapshot, cx);
        for workspace in &self.workspaces {
            for pane in &workspace.panes {
                let state = pane.surface.read(cx);
                let selected = state.trading_pnl.order_entry.selected_account_id.as_ref();
                let instrument = state.product.as_ref().and_then(|product| {
                    aeris_instruments::InstrumentId::try_new(product.instrument_id.clone()).ok()
                });
                let changed = self
                    .trading_store
                    .read(cx)
                    .pane_changed(selected, instrument.as_ref());
                let feedback_changed = latest.as_ref().map(|feedback| feedback.revision)
                    > state
                        .trading_pnl
                        .feedback
                        .as_ref()
                        .map(|feedback| feedback.revision);
                if !changed && !feedback_changed {
                    continue;
                }
                #[cfg(test)]
                self.trading_store.read(cx).note_pane_notify();
                pane.surface.update(cx, |state, state_cx| {
                    if latest.as_ref().map(|feedback| feedback.revision)
                        > state
                            .trading_pnl
                            .feedback
                            .as_ref()
                            .map(|feedback| feedback.revision)
                    {
                        state.trading_pnl.feedback.clone_from(&latest);
                    }
                    apply_trading_snapshot(state, &snapshot, state_cx);
                });
            }
        }
    }
}

fn apply_trading_snapshot(
    state: &mut WorkspaceSurface,
    snapshot: &aeris_trading_runtime::WorkingSet,
    state_cx: &mut Context<WorkspaceSurface>,
) {
    if state
        .trading_pnl
        .order_entry
        .selected_account_id
        .as_ref()
        .is_none_or(|account_id| {
            !snapshot
                .accounts
                .iter()
                .any(|account| &account.id == account_id)
        })
    {
        state.trading_pnl.order_entry.selected_account_id =
            snapshot.accounts.first().map(|account| account.id.clone());
    }
    let selected_account_id = state.trading_pnl.order_entry.selected_account_id.clone();
    let chart_snapshot = crate::desktop::chart_trading_snapshot(
        snapshot,
        state.product.as_ref(),
        selected_account_id.as_ref(),
    )
    .map(|snapshot| apply_chart_trading_visibility(snapshot, state.chart_trading_visibility));
    let host_overlay =
        crate::desktop::chart_session_plan_overlay(snapshot, selected_account_id.as_ref());
    let session_plan_levels = crate::desktop::chart_session_plan_levels(
        snapshot,
        state.product.as_ref(),
        selected_account_id.as_ref(),
    );
    state.trading_pnl.applied_selection = Some((
        state
            .product
            .as_ref()
            .map(|product| product.instrument_id.clone()),
        selected_account_id,
        state.chart_trading_visibility,
    ));
    state.trading_pnl.projection = TradingProjection::from_working_set(
        snapshot,
        state.trading_pnl.order_entry.selected_account_id.as_ref(),
    );
    let (working_orders, position_marker) = project_working_order_markers(state, state_cx);
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

pub(super) fn refresh_chart_trading_projection(
    chart: &Entity<AerisChartView>,
    trading: Option<ChartTradingSnapshot>,
    host_overlay: ChartHostOverlaySnapshot,
    session_plan_levels: Vec<(f64, String)>,
    cx: &mut Context<WorkspaceSurface>,
) {
    // `None` is authoritative too: it means the selected account/product no longer projects any
    // trading state. Leaving the previous snapshot installed is how cancelled/deleted practice
    // orders can appear to remain stuck on the chart after the runtime has already removed them.
    let trading = authoritative_chart_trading_projection(trading);
    let mut failed = false;
    if chart.read(cx).trading_snapshot() != trading {
        failed |= chart
            .update(cx, |chart, chart_cx| {
                let result = chart.set_trading_snapshot(trading);
                if result.is_ok() {
                    chart_cx.notify();
                }
                result
            })
            .is_err();
    }
    if chart.read(cx).host_overlay() != host_overlay {
        failed |= chart
            .update(cx, |chart, chart_cx| {
                let result = chart.set_host_overlay(host_overlay);
                if result.is_ok() {
                    chart_cx.notify();
                }
                result
            })
            .is_err();
    }
    if chart.read(cx).session_plan_levels() != session_plan_levels {
        failed |= chart
            .update(cx, |chart, chart_cx| {
                let result = chart.replace_session_plan_levels(session_plan_levels);
                if result.is_ok() {
                    chart_cx.notify();
                }
                result
            })
            .is_err();
    }
    if failed {
        aeris_observability::diagnostic!("Aeris chart trading projection could not be applied");
        WorkspaceSurface::post_indicator_failure(
            "trading_projection",
            "Chart trading unavailable",
            "Trading overlays could not be shown. Try again.",
            cx,
        );
    } else {
        WorkspaceSurface::clear_indicator_failure("trading_projection", cx);
    }
}

fn authoritative_chart_trading_projection(
    trading: Option<ChartTradingSnapshot>,
) -> ChartTradingSnapshot {
    trading.unwrap_or_default()
}

pub(crate) fn apply_chart_trading_visibility(
    mut trading: ChartTradingSnapshot,
    visibility: ChartTradingVisibilitySettings,
) -> ChartTradingSnapshot {
    if !visibility.show_order_management_lines {
        trading.positions.clear();
        trading.orders.clear();
    }
    if !visibility.show_execution_marks {
        trading.executions.clear();
    }
    trading
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_authoritative_trading_projection_clears_stale_chart_state() {
        let stale = ChartTradingSnapshot {
            orders: vec![aeris_chart_integration::ChartWorkingOrder {
                id: aeris_chart_integration::ChartOrderId::new("stale-order").expect("order id"),
                account_id: None,
                pane_index: 0,
                price_scale: aeris_chart_integration::ChartTradingPriceScale::Right,
                side: aeris_chart_integration::ChartOrderSide::Buy,
                kind: aeris_chart_integration::ChartOrderKind::Limit,
                role: aeris_chart_integration::ChartOrderRole::Working,
                status: aeris_chart_integration::ChartOrderStatus::Working,
                price: 100.0,
                stop_price: None,
                trailing_trigger_price: None,
                break_even_trigger_price: None,
                quantity: 1.0,
                filled_quantity: 0.0,
                position_id: None,
                parent_order_id: None,
                bracket_id: None,
                oco_group_id: None,
                revision: 1,
                annotations: Vec::new(),
            }],
            ..ChartTradingSnapshot::default()
        };
        assert_eq!(stale.orders.len(), 1);

        let cleared = authoritative_chart_trading_projection(None);
        assert_eq!(
            cleared.orders,
            [] as [aeris_chart_integration::ChartWorkingOrder; 0]
        );
        assert_eq!(
            cleared.positions,
            [] as [aeris_chart_integration::ChartTradingPosition; 0]
        );
    }

    #[test]
    fn chart_trading_visibility_filters_presentation_without_mutating_runtime_truth() {
        let snapshot = ChartTradingSnapshot {
            positions: vec![aeris_chart_integration::ChartTradingPosition {
                id: aeris_chart_integration::ChartPositionId::new("position:test")
                    .expect("position id"),
                account_id: None,
                pane_index: 0,
                price_scale: aeris_chart_integration::ChartTradingPriceScale::Right,
                side: aeris_chart_integration::ChartPositionSide::Long,
                average_price: 100.0,
                quantity: 1.0,
                display_pnl: Some(0.0),
                currency: Some("USD".to_string()),
                annotations: Vec::new(),
            }],
            executions: vec![aeris_chart_integration::ChartTradingExecution {
                id: aeris_chart_integration::ChartExecutionId::new("execution:test")
                    .expect("execution id"),
                account_id: None,
                pane_index: 0,
                price_scale: aeris_chart_integration::ChartTradingPriceScale::Right,
                side: aeris_chart_integration::ChartOrderSide::Buy,
                kind: aeris_chart_integration::ChartExecutionKind::PartialFill,
                time: 1,
                price: 100.0,
                quantity: 1.0,
                order_id: None,
                position_id: None,
                marker_shape: aeris_chart_integration::ChartExecutionMarkerShape::default(),
                size_by_quantity: false,
            }],
            ..ChartTradingSnapshot::default()
        };

        let filtered = apply_chart_trading_visibility(
            snapshot.clone(),
            ChartTradingVisibilitySettings {
                show_order_management_lines: false,
                show_execution_marks: true,
                ..ChartTradingVisibilitySettings::default()
            },
        );
        assert_eq!(
            filtered.positions,
            [] as [aeris_chart_integration::ChartTradingPosition; 0]
        );
        assert_eq!(filtered.executions.len(), 1);
        assert_eq!(snapshot.positions.len(), 1);

        let filtered = apply_chart_trading_visibility(
            snapshot,
            ChartTradingVisibilitySettings {
                show_order_management_lines: true,
                show_execution_marks: false,
                ..ChartTradingVisibilitySettings::default()
            },
        );
        assert_eq!(filtered.positions.len(), 1);
        assert_eq!(
            filtered.executions,
            [] as [aeris_chart_integration::ChartTradingExecution; 0]
        );
    }
}
