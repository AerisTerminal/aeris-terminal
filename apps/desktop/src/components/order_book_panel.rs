//! Docked order book panel: depth ladder above the order ticket, plus its column menu.

use super::order_ticket::{TradingOrderControlsState, trading_order_controls};
use super::side_panel_dock::{side_panel_header, side_panel_header_button};
use super::{
    AerisTheme, Div, Entity, HugeIcon, InteractiveElement, IntoElement, MenuRow, MouseButton,
    OrderBookColumn, OrderBookColumnVisibility, ParentElement, PopupAnimationOrigin, RadiusToken,
    ReadOnlyOrderBookView, SidePanel, Styled, WorkspaceSurface, animate_popup_from_origin, div,
    gpui_color, header_icon, px,
};

#[derive(Clone, Copy)]
pub(super) struct OrderBookPanelState<'a> {
    pub(super) order_book: &'a Entity<ReadOnlyOrderBookView>,
    pub(super) column_menu_open: bool,
    pub(super) columns: OrderBookColumnVisibility,
    /// Order ticket docked under the ladder; it carries the panel's app, book frame and theme.
    pub(super) ticket: TradingOrderControlsState<'a>,
}

pub(super) fn order_book_side_panel(state: &OrderBookPanelState<'_>) -> Div {
    let OrderBookPanelState {
        order_book,
        column_menu_open,
        columns,
        ticket,
    } = *state;
    let app = ticket.app.clone();
    let theme = ticket.theme;
    let settings_app = app.clone();
    let recenter_order_book = order_book.clone();
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
            [
                side_panel_header_button(
                    "order_book_recenter",
                    "Center order book on the current spread",
                    HugeIcon::Refresh,
                    false,
                    move |cx| {
                        recenter_order_book.update(cx, ReadOnlyOrderBookView::recenter_ladder);
                    },
                    theme,
                ),
                side_panel_header_button(
                    "order_book_column_settings",
                    "Choose order-book columns",
                    HugeIcon::Settings,
                    column_menu_open,
                    move |cx| {
                        settings_app.update(cx, WorkspaceSurface::toggle_order_book_column_menu);
                    },
                    theme,
                ),
            ],
            theme,
        ))
        .child(
            div()
                .id("order_book_rows")
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(order_book.clone()),
        )
        .child(trading_order_controls(&ticket))
        .children(
            column_menu_open.then(|| order_book_column_menu_layer(app, order_book, columns, theme)),
        )
}

fn order_book_column_menu_layer(
    app: Entity<WorkspaceSurface>,
    order_book: &Entity<ReadOnlyOrderBookView>,
    columns: OrderBookColumnVisibility,
    theme: &AerisTheme,
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
        .border(px(theme.dimensions.border_width))
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
        .child(div().absolute().inset_0().occlude().on_mouse_down(
            MouseButton::Left,
            move |_, _, cx| {
                dismiss_app.update(cx, WorkspaceSurface::close_order_book_column_menu);
                cx.stop_propagation();
            },
        ))
        .child(animate_popup_from_origin(
            panel,
            "order_book_column_menu_enter",
            PopupAnimationOrigin::TOP_RIGHT,
        ))
}

fn order_book_column_menu_item(
    order_book: Entity<ReadOnlyOrderBookView>,
    column: OrderBookColumn,
    checked: bool,
    first: bool,
    last: bool,
    theme: &AerisTheme,
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
