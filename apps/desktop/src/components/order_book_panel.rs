//! Docked order book panel: depth ladder above the order ticket, plus its column menu.

use super::order_ticket::{TradingOrderControlsState, trading_order_controls};
use super::side_panel_dock::{side_panel_header, side_panel_header_button};
use super::{
    AerisTheme, Div, Entity, HugeIcon, InteractiveElement, IntoElement, MenuPanel, MenuPlacement,
    MenuRow, OrderBookColumn, OrderBookColumnVisibility, ParentElement, PopupAnimationOrigin,
    ReadOnlyOrderBookView, SidePanel, Styled, WorkspaceSurface, div, gpui_color, px,
};
use gpui::StatefulInteractiveElement;

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
    let dismiss_app = app;
    let panel = MenuPanel::new("order_book_column_menu", MenuPlacement::InFlow, theme)
        .width(px(196.0))
        .animate_from(PopupAnimationOrigin::TOP_RIGHT)
        .children(OrderBookColumn::ALL.into_iter().map(|column| {
            order_book_column_menu_item(
                order_book.clone(),
                column,
                columns.is_visible(column),
                theme,
            )
        }));
    div()
        .id("order_book_column_menu_layer")
        .absolute()
        .inset_0()
        .child(
            div()
                .id("order_book_column_menu_scrim")
                .absolute()
                .inset_0()
                .occlude()
                .on_click(move |_, _, cx| {
                    dismiss_app.update(cx, WorkspaceSurface::close_order_book_column_menu);
                    cx.stop_propagation();
                }),
        )
        // The menu opens under the header's column control at the panel's right edge.
        .child(div().absolute().top(px(28.0)).right(px(30.0)).child(panel))
}

fn order_book_column_menu_item(
    order_book: Entity<ReadOnlyOrderBookView>,
    column: OrderBookColumn,
    checked: bool,
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
    MenuRow::compact(id, label, theme)
        .disabled(!available)
        .checked(checked)
        .on_click(move |_, _, cx| {
            order_book.update(cx, |order_book, order_book_cx| {
                order_book.toggle_column(column, order_book_cx);
            });
        })
}
