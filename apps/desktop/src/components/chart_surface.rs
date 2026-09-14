use super::{
    AxiusflowTheme, ChartNoticePlacement, ChartNoticeTone, ChartState, ChartSurfaceNotice, Context,
    Div, Entity, FluentBuilder, HugeIcon, InstallProviderInstrument, InteractiveElement,
    IntoElement, Loader, MenuRow, MouseButton, NucleusChartView, OrderBookColumn,
    OrderBookColumnVisibility, ParentElement, RadiusToken, ReadOnlyOrderBookView, Render, Role,
    SIDE_PANEL_RESIZE_HANDLE_WIDTH, SidePanel, SidePanelVisibility, StatefulInteractiveElement,
    Styled, TerminalApp, ToPrimitive, WORKSPACE_TAB_ICON_GLYPH, WORKSPACE_TAB_ICON_HIT,
    WatchlistRow, Window, WorkspaceSurface, chart_chrome, chart_surface_notice,
    chrome_close_button, chrome_tooltip, div, exchange_mark, gpui_color, header_icon,
    market_quote_change, market_quote_price, market_quote_values, platform_tabular_numerals, px,
};
use gpui::{AppContext, Stateful};

const SIDE_PANEL_HEADER_HEIGHT: f32 = 30.0;
const SIDE_PANEL_SPLIT_HANDLE_HEIGHT: f32 = 8.0;
const WATCHLIST_COLUMNS_HEIGHT: f32 = 26.0;
const WATCHLIST_ROW_HEIGHT: f32 = 30.0;
const WATCHLIST_LAST_WIDTH: f32 = 62.0;
const WATCHLIST_CHANGE_WIDTH: f32 = 54.0;
const WATCHLIST_CHANGE_PERCENT_WIDTH: f32 = 58.0;
const WATCHLIST_VOLUME_WIDTH: f32 = 52.0;

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
    pub(super) terminal: Entity<TerminalApp>,
    pub(super) workspace_id: u64,
    pub(super) visible: SidePanelVisibility,
    pub(super) width: f32,
    pub(super) split_basis_points: u32,
    pub(super) order_book: &'a Entity<ReadOnlyOrderBookView>,
    pub(super) watchlist: Vec<WatchlistRow>,
    pub(super) order_book_column_menu_open: bool,
    pub(super) order_book_columns: OrderBookColumnVisibility,
    pub(super) theme: &'a AxiusflowTheme,
}

fn order_book_side_panel(
    app: Entity<WorkspaceSurface>,
    order_book: &Entity<ReadOnlyOrderBookView>,
    column_menu_open: bool,
    columns: OrderBookColumnVisibility,
    theme: &AxiusflowTheme,
) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(gpui_color(theme.colors.surface))
        .size_full()
        .child(side_panel_header(
            SidePanel::OrderBook,
            app.clone(),
            column_menu_open,
            theme,
        ))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(order_book.clone()),
        )
        .children(
            column_menu_open.then(|| order_book_column_menu_layer(app, order_book, columns, theme)),
        )
}

fn watchlist_side_panel(
    app: Entity<WorkspaceSurface>,
    terminal: &Entity<TerminalApp>,
    watchlist: Vec<WatchlistRow>,
    theme: &AxiusflowTheme,
) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(gpui_color(theme.colors.surface))
        .size_full()
        .child(side_panel_header(SidePanel::Watchlist, app, false, theme))
        .child(watchlist_table(terminal, watchlist, theme))
}

fn side_panel_region(content: Div, panel: SidePanel, both_visible: bool, ratio: f32) -> Div {
    if !both_visible {
        return content;
    }
    div()
        .w_full()
        .min_h_0()
        .flex_basis(px(0.0))
        .flex_grow(match panel {
            SidePanel::OrderBook => ratio,
            SidePanel::Watchlist => 1.0 - ratio,
        })
        .overflow_hidden()
        .child(content)
}

pub(super) fn workspace_side_panel(state: WorkspaceSidePanelState<'_>) -> impl IntoElement + use<> {
    let WorkspaceSidePanelState {
        app,
        terminal,
        workspace_id,
        visible,
        width,
        split_basis_points,
        order_book,
        watchlist,
        order_book_column_menu_open,
        order_book_columns,
        theme,
    } = state;
    let colors = theme.colors;
    let order_book_visible = visible.contains(SidePanel::OrderBook);
    let watchlist_visible = visible.contains(SidePanel::Watchlist);
    let both_visible = order_book_visible && watchlist_visible;
    let ratio = if both_visible {
        (split_basis_points.to_f32().unwrap_or(5_000.0) / 10_000.0).clamp(0.05, 0.95)
    } else {
        1.0
    };
    let order_book_panel = order_book_visible.then(|| {
        side_panel_region(
            order_book_side_panel(
                app.clone(),
                order_book,
                order_book_column_menu_open,
                order_book_columns,
                theme,
            ),
            SidePanel::OrderBook,
            both_visible,
            ratio,
        )
    });
    let watchlist_panel = watchlist_visible.then(|| {
        side_panel_region(
            watchlist_side_panel(app.clone(), &terminal, watchlist, theme),
            SidePanel::Watchlist,
            both_visible,
            ratio,
        )
    });
    let split_drag_app = app.clone();
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
        .children(order_book_panel)
        .children(
            both_visible.then(|| side_panel_split_handle(workspace_id, gpui_color(colors.border))),
        )
        .children(watchlist_panel)
        .on_drag_move::<SidePanelSplitDrag>(move |event, _, cx| {
            let height = f32::from(event.bounds.size.height) - SIDE_PANEL_SPLIT_HANDLE_HEIGHT;
            if height <= 0.0 {
                return;
            }
            let ratio = (f32::from(event.event.position.y)
                - f32::from(event.bounds.top())
                - SIDE_PANEL_SPLIT_HANDLE_HEIGHT / 2.0)
                / height;
            split_drag_app.update(cx, |surface, surface_cx| {
                surface.set_side_panel_split_ratio(ratio, surface_cx);
            });
        })
        .on_drag_move::<SidePanelWidthDrag>(move |event, _, cx| {
            let width = super::clamped_side_panel_width(
                f32::from(event.bounds.right()) - f32::from(event.event.position.x),
            );
            app.update(cx, |surface, surface_cx| {
                surface.set_side_panel_width(width, surface_cx);
            });
        })
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
                .on_drag(SidePanelWidthDrag, |drag, _, _, cx| {
                    cx.new(|_| drag.clone())
                }),
        )
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

#[derive(Clone)]
struct SidePanelSplitDrag;

impl Render for SidePanelSplitDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

#[derive(Clone)]
struct SidePanelWidthDrag;

impl Render for SidePanelWidthDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

#[derive(Clone)]
struct WatchlistRowDrag {
    provider: String,
    instrument_id: String,
}

impl Render for WatchlistRowDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

fn side_panel_split_handle(workspace_id: u64, border: gpui::Hsla) -> impl IntoElement {
    div()
        .id(("side_panel_split", workspace_id))
        .relative()
        .occlude()
        .flex_none()
        .w_full()
        .h(px(SIDE_PANEL_SPLIT_HANDLE_HEIGHT))
        .cursor_row_resize()
        .on_drag(SidePanelSplitDrag, |drag, _, _, cx| {
            cx.new(|_| drag.clone())
        })
        .child(
            div()
                .absolute()
                .left_0()
                .top(px(3.0))
                .w_full()
                .h(px(1.0))
                .bg(border),
        )
}

fn watchlist_table(
    terminal: &Entity<TerminalApp>,
    rows: Vec<WatchlistRow>,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let mut body = div()
        .flex_1()
        .min_h_0()
        .font_features(platform_tabular_numerals());
    if rows.is_empty() {
        body = body.child(
            div()
                .px_3()
                .py_4()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child("Add symbols with +"),
        );
    } else {
        for (index, row) in rows.into_iter().enumerate() {
            body = body.child(watchlist_row(terminal, row, index, theme));
        }
    }
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(gpui_color(colors.surface))
        .child(watchlist_columns(theme))
        .child(body.id("watchlist_body").overflow_y_scroll())
}

fn watchlist_columns(theme: &AxiusflowTheme) -> impl IntoElement + use<> {
    let colors = theme.colors;
    div()
        .h(px(WATCHLIST_COLUMNS_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .px_2()
        .text_xs()
        .text_color(gpui_color(colors.text_muted))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .whitespace_nowrap()
                .text_ellipsis()
                .child("ASSET"),
        )
        .child(watchlist_cell("LAST", WATCHLIST_LAST_WIDTH, theme))
        .child(watchlist_cell("CHANGE", WATCHLIST_CHANGE_WIDTH, theme))
        .child(watchlist_cell(
            "CHANGE %",
            WATCHLIST_CHANGE_PERCENT_WIDTH,
            theme,
        ))
        .child(watchlist_cell("VOLUME", WATCHLIST_VOLUME_WIDTH, theme))
}

fn watchlist_cell(value: impl Into<gpui::SharedString>, width: f32, theme: &AxiusflowTheme) -> Div {
    div()
        .w(px(width))
        .flex_none()
        .pr_1()
        .border_l_1()
        .border_color(gpui_color(theme.colors.border))
        .text_right()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(value.into())
}

fn watchlist_row(
    terminal: &Entity<TerminalApp>,
    row: WatchlistRow,
    index: usize,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let scale = row.instrument.price_scale;
    let values = market_quote_values(row.last, row.previous_close);
    let tone = values
        .change
        .map_or(colors.text_muted, |value| match value.cmp(&0) {
            std::cmp::Ordering::Less => colors.danger,
            std::cmp::Ordering::Greater => colors.primary,
            std::cmp::Ordering::Equal => colors.text_secondary,
        });
    let asset_tone = if row.message.is_some() {
        colors.text_muted
    } else {
        colors.text_primary
    };
    let logo = match row.instrument.provider.as_str() {
        "hyperliquid" => Some(super::assets::ExchangeLogo::Hyperliquid),
        "rithmic" => Some(super::assets::ExchangeLogo::Rithmic),
        _ => None,
    };
    let instrument = row.instrument.clone();
    let active = row.active;
    let content = div()
        .id(gpui::SharedString::from(format!(
            "watchlist_row_{}_{}",
            row.instrument.provider, row.instrument.instrument_id
        )))
        .group("watchlist_asset_row")
        .h(px(WATCHLIST_ROW_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .px_2()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .text_xs()
        .bg(gpui_color(if active {
            colors.active_bg.over(colors.surface)
        } else {
            colors.surface
        }))
        .when(!active, |item| {
            item.hover(move |item| item.bg(gpui_color(colors.hover_bg.over(colors.surface))))
        })
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .items_center()
                .gap_1()
                .whitespace_nowrap()
                .text_color(gpui_color(asset_tone))
                .children(logo.map(|logo| exchange_mark(logo, px(16.0), false, &colors)))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_ellipsis()
                        .child(row.instrument.display_symbol),
                ),
        )
        .child(watchlist_cell(
            values
                .last
                .map_or_else(|| "—".to_string(), |value| market_quote_price(value, scale)),
            WATCHLIST_LAST_WIDTH,
            theme,
        ))
        .child(
            watchlist_cell(
                values.change.map_or_else(
                    || "—".to_string(),
                    |value| market_quote_change(value, scale),
                ),
                WATCHLIST_CHANGE_WIDTH,
                theme,
            )
            .text_color(gpui_color(tone)),
        )
        .child(
            watchlist_cell(
                values
                    .change_percent
                    .map_or_else(|| "—".to_string(), |value| format!("{value:+.2}%")),
                WATCHLIST_CHANGE_PERCENT_WIDTH,
                theme,
            )
            .text_color(gpui_color(tone)),
        )
        .child(watchlist_cell(
            row.last.map_or_else(
                || "—".to_string(),
                |bar| compact_watchlist_volume(bar.volume, row.instrument.quantity_scale),
            ),
            WATCHLIST_VOLUME_WIDTH,
            theme,
        ));
    interactive_watchlist_row(content, terminal, instrument, index, active)
}

fn interactive_watchlist_row(
    row: Stateful<Div>,
    terminal: &Entity<TerminalApp>,
    instrument: InstallProviderInstrument,
    index: usize,
    active: bool,
) -> Stateful<Div> {
    let provider = instrument.provider.clone();
    let instrument_id = instrument.instrument_id.clone();
    let remove_terminal = terminal.clone();
    let select_terminal = terminal.clone();
    let move_terminal = terminal.clone();
    let drag = WatchlistRowDrag {
        provider: provider.clone(),
        instrument_id: instrument_id.clone(),
    };
    row.cursor_pointer()
        .role(Role::Button)
        .aria_selected(active)
        .aria_label(format!("Select {}", instrument.display_symbol))
        .on_click(move |_, _, cx| {
            select_terminal.update(cx, |terminal, terminal_cx| {
                terminal.select_watchlist_instrument(&instrument, terminal_cx);
            });
        })
        .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
        .on_drag_move::<WatchlistRowDrag>(move |event, _, cx| {
            let drag = event.drag(cx).clone();
            move_terminal.update(cx, |terminal, terminal_cx| {
                terminal.move_watchlist_instrument(
                    &drag.provider,
                    &drag.instrument_id,
                    index,
                    terminal_cx,
                );
            });
        })
        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
            remove_terminal.update(cx, |terminal, terminal_cx| {
                terminal.remove_watchlist_instrument(&provider, &instrument_id, terminal_cx);
            });
            cx.stop_propagation();
        })
}

fn compact_watchlist_volume(value: i64, scale: u32) -> String {
    let exponent = i32::try_from(scale).unwrap_or(i32::MAX);
    let divisor = 10_f64.powi(exponent);
    let value = value.to_f64().unwrap_or(0.0) / divisor;
    for (threshold, suffix) in [(1_000_000_000.0, "B"), (1_000_000.0, "M"), (1_000.0, "K")] {
        if value.abs() >= threshold {
            return format!("{:.2}{suffix}", value / threshold);
        }
    }
    format!("{value:.2}")
}

pub(super) fn side_panel_header(
    panel: SidePanel,
    app: Entity<WorkspaceSurface>,
    order_book_column_menu_open: bool,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let settings_app = app.clone();
    let add_app = app.clone();
    let close_id = match panel {
        SidePanel::OrderBook => "close_order_book_panel",
        SidePanel::Watchlist => "close_watchlist_panel",
    };
    div()
        .h(px(SIDE_PANEL_HEADER_HEIGHT))
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
        .children((panel == SidePanel::OrderBook).then(|| {
            chrome_tooltip(
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
                    .hover(move |button| {
                        button.bg(gpui_color(colors.hover_bg.over(colors.surface)))
                    })
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        settings_app.update(cx, WorkspaceSurface::toggle_order_book_column_menu);
                        cx.stop_propagation();
                    })
                    .child(
                        header_icon(HugeIcon::Settings01).with_size(px(WORKSPACE_TAB_ICON_GLYPH)),
                    ),
                theme,
            )
        }))
        .children((panel == SidePanel::Watchlist).then(|| {
            chrome_tooltip(
                "watchlist_add_symbol",
                "Add symbol to watchlist",
                div()
                    .id("watchlist_add_symbol")
                    .occlude()
                    .size(px(WORKSPACE_TAB_ICON_HIT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
                    .text_color(gpui_color(colors.icon))
                    .cursor_pointer()
                    .role(Role::Button)
                    .aria_label("Add symbol to watchlist")
                    .hover(move |button| {
                        button.bg(gpui_color(colors.hover_bg.over(colors.surface)))
                    })
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        add_app.update(cx, |surface, surface_cx| {
                            surface.open_watchlist_symbol_menu(window, surface_cx);
                        });
                        cx.stop_propagation();
                    })
                    .child(
                        header_icon(HugeIcon::AddIcon01).with_size(px(WORKSPACE_TAB_ICON_GLYPH)),
                    ),
                theme,
            )
        }))
        .child(chrome_tooltip(
            close_id,
            "Close side panel",
            chrome_close_button(close_id, theme, move |_, cx| {
                app.update(cx, |surface, surface_cx| {
                    surface.close_side_panel(panel, surface_cx);
                });
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
