//! Docked order book panel: the depth ladder and its column menu.

use super::side_panel_dock::{side_panel_header, side_panel_header_button};
use super::{
    AerisTheme, Div, Entity, HugeIcon, InteractiveElement, IntoElement, MenuPanel, MenuPlacement,
    MenuRow, OrderBookColumn, OrderBookColumnVisibility, ParentElement, PopupAnimationOrigin,
    ReadOnlyOrderBookView, SidePanel, Styled, WorkspaceSurface, div, gpui_color, px,
};
use gpui::StatefulInteractiveElement;

#[derive(Clone, Copy)]
pub(super) struct OrderBookPanelState<'a> {
    pub(super) app: &'a Entity<WorkspaceSurface>,
    pub(super) order_book: &'a Entity<ReadOnlyOrderBookView>,
    pub(super) column_menu_open: bool,
    pub(super) columns: OrderBookColumnVisibility,
    pub(super) theme: &'a AerisTheme,
}

pub(super) fn order_book_side_panel(state: &OrderBookPanelState<'_>) -> Div {
    let OrderBookPanelState {
        app,
        order_book,
        column_menu_open,
        columns,
        theme,
    } = *state;
    let app = app.clone();
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
        .children(column_menu_open.then(|| order_book_column_menu_layer(app, columns, theme)))
}

fn order_book_column_menu_layer(
    app: Entity<WorkspaceSurface>,
    columns: OrderBookColumnVisibility,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let panel = MenuPanel::new("order_book_column_menu", MenuPlacement::InFlow, theme)
        .width(px(196.0))
        .animate_from(PopupAnimationOrigin::TOP_RIGHT)
        .children(
            OrderBookColumn::ALL
                .into_iter()
                .map(|column| order_book_column_menu_item(app.clone(), column, columns, theme)),
        );
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
                    app.update(cx, WorkspaceSurface::close_order_book_column_menu);
                    cx.stop_propagation();
                }),
        )
        // The menu opens under the header's column control at the panel's right edge.
        .child(div().absolute().top(px(28.0)).right(px(30.0)).child(panel))
}

fn order_book_column_menu_item(
    app: Entity<WorkspaceSurface>,
    column: OrderBookColumn,
    columns: OrderBookColumnVisibility,
    theme: &AerisTheme,
) -> impl IntoElement {
    let offered = columns.is_offered(column);
    let label = if offered {
        column.label().to_string()
    } else {
        format!("{} · not offered by this provider", column.label())
    };
    let id = match column {
        OrderBookColumn::ProfitLoss => "order_book_column_profit_loss",
        OrderBookColumn::Bid => "order_book_column_bid",
        OrderBookColumn::SellTrades => "order_book_column_sell_trades",
        OrderBookColumn::Price => "order_book_column_price",
        OrderBookColumn::BuyTrades => "order_book_column_buy_trades",
        OrderBookColumn::Ask => "order_book_column_ask",
        OrderBookColumn::Orders => "order_book_column_orders",
        OrderBookColumn::Queue => "order_book_column_queue",
    };
    MenuRow::compact(id, label, theme)
        .disabled(!offered)
        .checked(columns.is_visible(column))
        .on_click(move |_, _, cx| {
            app.update(cx, |surface, surface_cx| {
                surface.toggle_order_book_column(column, surface_cx);
            });
        })
}
