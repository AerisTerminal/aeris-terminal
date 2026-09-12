use super::{
    AxiusflowTheme, ChartNoticePlacement, ChartNoticeTone, ChartState, ChartSurfaceNotice, Div,
    Entity, FluentBuilder, HugeIcon, InteractiveElement, IntoElement, Loader, MenuRow, MouseButton,
    NucleusChartView, OrderBookColumn, OrderBookColumnVisibility, ParentElement, RadiusToken,
    ReadOnlyOrderBookView, Role, SIDE_PANEL_RESIZE_HANDLE_WIDTH, SidePanel,
    StatefulInteractiveElement, Styled, WORKSPACE_TAB_ICON_GLYPH, WORKSPACE_TAB_ICON_HIT,
    WorkspaceSurface, chart_chrome, chart_surface_notice, chrome_close_button, chrome_tooltip, div,
    gpui_color, header_icon, px,
};

pub(super) struct MarketWorkspaceState<'a> {
    pub(super) pane_id: u64,
    pub(super) chart: Option<&'a Entity<NucleusChartView>>,
    pub(super) chart_has_market_data: bool,
    pub(super) chart_is_superseded: bool,
    pub(super) chart_state: ChartState,
    pub(super) chart_status_detail: String,
    pub(super) theme: &'a AxiusflowTheme,
}

#[allow(clippy::too_many_lines)]
pub(super) fn market_workspace(state: MarketWorkspaceState<'_>) -> impl IntoElement + use<> {
    let MarketWorkspaceState {
        pane_id,
        chart,
        chart_has_market_data,
        chart_is_superseded,
        chart_state,
        chart_status_detail,
        theme,
    } = state;
    let colors = theme.colors;
    let notice = chart_surface_notice(
        chart_state,
        chart_has_market_data,
        chart_is_superseded,
        &chart_status_detail,
    );
    let chart_surface = chart_pane_host(chart)
        .id(("primary_chart", pane_id))
        .bg(gpui_color(colors.surface))
        .children(notice.map(|notice| chart_notice(notice, theme)));
    div().size_full().overflow_hidden().child(chart_surface)
}

pub(super) struct WorkspaceSidePanelState<'a> {
    pub(super) app: Entity<WorkspaceSurface>,
    pub(super) workspace_id: u64,
    pub(super) panel: SidePanel,
    pub(super) width: f32,
    pub(super) order_book: &'a Entity<ReadOnlyOrderBookView>,
    pub(super) order_book_column_menu_open: bool,
    pub(super) order_book_columns: OrderBookColumnVisibility,
    pub(super) theme: &'a AxiusflowTheme,
}

pub(super) fn workspace_side_panel(state: WorkspaceSidePanelState<'_>) -> impl IntoElement + use<> {
    let WorkspaceSidePanelState {
        app,
        workspace_id,
        panel,
        width,
        order_book,
        order_book_column_menu_open,
        order_book_columns,
        theme,
    } = state;
    let colors = theme.colors;
    let resize_app = app.clone();
    let move_app = app.clone();
    let release_app = app.clone();
    div()
        .id(("workspace_side_panel", workspace_id))
        .w(px(width))
        .h_full()
        .flex_none()
        .relative()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(gpui_color(colors.surface))
        .border_l_1()
        .border_color(gpui_color(colors.border))
        .child(side_panel_header(
            panel,
            app.clone(),
            order_book_column_menu_open,
            theme,
        ))
        .child(
            div()
                .flex_1()
                .overflow_hidden()
                .children((panel == SidePanel::OrderBook).then_some(order_book.clone())),
        )
        .children(
            (panel == SidePanel::OrderBook && order_book_column_menu_open)
                .then(|| order_book_column_menu_layer(app, order_book, order_book_columns, theme)),
        )
        .child(
            div()
                .id(("side_panel_resize", workspace_id))
                .absolute()
                .occlude()
                .top_0()
                .left(px(-SIDE_PANEL_RESIZE_HANDLE_WIDTH / 2.0))
                .h_full()
                .w(px(SIDE_PANEL_RESIZE_HANDLE_WIDTH))
                .cursor_col_resize()
                .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                    resize_app.update(cx, |surface, _| {
                        surface.begin_side_panel_resize(f32::from(event.position.x));
                    });
                    cx.stop_propagation();
                }),
        )
        .on_mouse_move(move |event, _, cx| {
            move_app.update(cx, |surface, surface_cx| {
                surface.update_side_panel_resize(
                    f32::from(event.position.x),
                    event.pressed_button == Some(MouseButton::Left),
                    surface_cx,
                );
            });
        })
        .on_mouse_up(MouseButton::Left, move |_, _, cx| {
            release_app.update(cx, |surface, _| surface.end_side_panel_resize());
        })
}

pub(super) fn chart_pane_host(chart: Option<&Entity<NucleusChartView>>) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .size_full()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .children(chart.cloned())
}

pub(super) fn side_panel_header(
    panel: SidePanel,
    app: Entity<WorkspaceSurface>,
    order_book_column_menu_open: bool,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let settings_app = app.clone();
    div()
        .h(px(30.0))
        .flex_none()
        .flex()
        .items_center()
        .px_2()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface))
        .text_xs()
        .text_color(gpui_color(colors.text_secondary))
        .child(div().flex_1().child(panel.title().to_uppercase()))
        .child(chrome_tooltip(
            "order_book_column_settings",
            "Choose order-book columns",
            div()
                .id("order_book_column_settings")
                .occlude()
                .size(px(WORKSPACE_TAB_ICON_HIT))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
                .text_color(gpui_color(if order_book_column_menu_open {
                    colors.icon_active
                } else {
                    colors.icon
                }))
                .cursor_pointer()
                .role(Role::Button)
                .aria_label("Choose order-book columns")
                .when(order_book_column_menu_open, |button| {
                    button.bg(gpui_color(colors.active_bg.over(colors.surface)))
                })
                .hover(move |button| button.bg(gpui_color(colors.hover_bg.over(colors.surface))))
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    settings_app.update(cx, WorkspaceSurface::toggle_order_book_column_menu);
                    cx.stop_propagation();
                })
                .child(header_icon(HugeIcon::Settings01).with_size(px(WORKSPACE_TAB_ICON_GLYPH))),
            theme,
        ))
        .child(chrome_tooltip(
            "close_side_panel",
            "Close side panel",
            chrome_close_button("close_side_panel", theme, move |_, cx| {
                app.update(cx, WorkspaceSurface::close_side_panel);
            }),
            theme,
        ))
}

pub(super) fn order_book_column_menu_layer(
    app: Entity<WorkspaceSurface>,
    order_book: &Entity<ReadOnlyOrderBookView>,
    columns: OrderBookColumnVisibility,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let dismiss_app = app;
    let mut panel = div()
        .id("order_book_column_menu")
        .absolute()
        .top(px(28.0))
        .right(px(30.0))
        .w(px(196.0))
        .occlude()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border_1()
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface_secondary))
        .text_color(gpui_color(colors.text_primary))
        .on_any_mouse_down(|_, _, cx| cx.stop_propagation());
    let last = OrderBookColumn::ALL.len().saturating_sub(1);
    for (index, column) in OrderBookColumn::ALL.into_iter().enumerate() {
        panel = panel.child(order_book_column_menu_item(
            order_book.clone(),
            column,
            columns.is_visible(column),
            index == 0,
            index == last,
            theme,
        ));
    }
    div()
        .id("order_book_column_menu_layer")
        .absolute()
        .inset_0()
        .child(
            div()
                .absolute()
                .inset_0()
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    dismiss_app.update(cx, WorkspaceSurface::close_order_book_column_menu);
                    cx.stop_propagation();
                }),
        )
        .child(panel)
}

pub(super) fn order_book_column_menu_item(
    order_book: Entity<ReadOnlyOrderBookView>,
    column: OrderBookColumn,
    checked: bool,
    first: bool,
    last: bool,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let available = column.available();
    let label = if available {
        column.label().to_string()
    } else {
        "P/L · routing unavailable".to_string()
    };
    let id = match column {
        OrderBookColumn::ProfitLoss => "order_book_column_profit_loss",
        OrderBookColumn::Bid => "order_book_column_bid",
        OrderBookColumn::SellTrades => "order_book_column_sell_trades",
        OrderBookColumn::Price => "order_book_column_price",
        OrderBookColumn::BuyTrades => "order_book_column_buy_trades",
        OrderBookColumn::Ask => "order_book_column_ask",
        OrderBookColumn::Orders => "order_book_column_orders",
    };
    let item_order_book = order_book;
    let mut item = MenuRow::compact(id, label, theme)
        .disabled(!available)
        .flush_in_panel(first, last)
        .on_click(move |_, _, cx| {
            item_order_book.update(cx, |order_book, order_book_cx| {
                order_book.toggle_column(column, order_book_cx);
            });
        });
    if checked {
        item = item.trailing(
            header_icon(HugeIcon::CheckIcon)
                .with_size(px(16.0))
                .color(gpui_color(theme.colors.icon)),
        );
    }
    item
}
pub(super) fn chart_notice(
    notice: ChartSurfaceNotice,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    if notice.label == ChartState::Loading.label() {
        if notice.placement != ChartNoticePlacement::Center {
            // A repair behind the chart the trader is still reading is announced by
            // the symbol legend's own spinner, beside the symbol it belongs to. A
            // second one in the corner lands on top of that legend.
            return div().into_any_element();
        }
        let spinner = Loader::from_path("chart_notice_loader", HugeIcon::Loader.path())
            .with_size(px(40.0))
            .color(gpui_color(colors.icon));
        let overlay = div()
            .id("chart_loading_status")
            .absolute()
            .occlude()
            .role(Role::Status)
            .aria_label(notice.label)
            // A loading surface is deliberately opaque. On first launch there
            // is no chart to read, and during a switch the retained chart belongs
            // to the previous selection.
            .bg(gpui_color(colors.surface))
            .gap_2()
            .child(spinner)
            .child(
                div()
                    .text_sm()
                    .text_color(gpui_color(colors.text_primary))
                    .child(notice.label),
            )
            .children(notice.detail.clone().map(|detail| {
                div()
                    .text_xs()
                    .text_color(gpui_color(colors.text_secondary))
                    .child(detail)
            }));
        return overlay
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .into_any_element();
    }
    let tone = match notice.tone {
        ChartNoticeTone::Muted => colors.text_secondary,
        ChartNoticeTone::Warning | ChartNoticeTone::Loss => colors.danger,
    };
    let label = div()
        .flex()
        .flex_col()
        .gap_1()
        .px_2()
        .py_1()
        .border_1()
        .rounded(px(f32::from(
            chart_chrome::CHART_SURFACE_RADIUS.logical_pixels(),
        )))
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface.with_alpha(0.94)))
        .text_xs()
        .text_color(gpui_color(tone))
        .child(notice.label)
        .children(notice.detail.map(|detail| {
            div()
                .text_color(gpui_color(colors.text_secondary))
                .child(detail)
        }));
    match notice.placement {
        ChartNoticePlacement::Center => div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(label)
            .into_any_element(),
        ChartNoticePlacement::BottomRight => div()
            .absolute()
            .right_2()
            .bottom_2()
            .child(label)
            .into_any_element(),
    }
}
