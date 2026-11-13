//! Docked order book panel: the depth ladder and its column menu.

use super::side_panel_dock::{side_panel_header, side_panel_header_button};
use super::{
    AerisTheme, Div, Entity, HugeIcon, InteractiveElement, IntoElement, MenuPanel, MenuPlacement,
    MenuRow, OrderBookColumn, OrderBookColumnVisibility, OrderBookHighlights, ParentElement,
    PopupAnimationOrigin, ReadOnlyOrderBookView, SidePanel, Styled, WorkspaceOrderBookState,
    WorkspaceSurface, div, gpui_color, px,
};
use aeris_localization::{panels, tr};
use aeris_market_runtime::RECENT_LADDER_TRADE_RETENTION_NANOS;
use gpui::StatefulInteractiveElement;

#[derive(Clone, Copy)]
pub(super) struct OrderBookPanelState<'a> {
    pub(super) app: &'a Entity<WorkspaceSurface>,
    pub(super) order_book: &'a Entity<ReadOnlyOrderBookView>,
    pub(super) column_menu_open: bool,
    pub(super) columns: OrderBookColumnVisibility,
    pub(super) highlights: OrderBookHighlights,
    pub(super) theme: &'a AerisTheme,
}

/// The docked book's choices as the pane persists them. The defaults persist as absent, so
/// a document that never changed them keeps its bytes.
pub(super) fn persisted_order_book_state(
    columns: OrderBookColumnVisibility,
    highlights: OrderBookHighlights,
) -> Option<WorkspaceOrderBookState> {
    if columns.chosen_bits() == OrderBookColumnVisibility::default().chosen_bits()
        && highlights == OrderBookHighlights::default()
    {
        return None;
    }
    Some(WorkspaceOrderBookState {
        chosen_columns: u32::from(columns.chosen_bits()),
        imbalance_ratio: highlights.imbalance_ratio().unwrap_or(0),
        large_lot_quantity: highlights.large_lot_quantity().unwrap_or(0),
    })
}

/// Saved column choices and highlights. Bits of no known column are dropped, and a
/// highlight value that is not offered restores the default highlights.
pub(super) fn restored_order_book_state(
    state: &WorkspaceOrderBookState,
) -> (u16, OrderBookHighlights) {
    let chosen_columns =
        u16::try_from(state.chosen_columns & u32::from(u16::MAX)).unwrap_or_default();
    let choice = |value: u32| (value != 0).then_some(value);
    let highlights = OrderBookHighlights::from_choices(
        choice(state.imbalance_ratio),
        choice(state.large_lot_quantity),
    )
    .unwrap_or_default();
    (chosen_columns, highlights)
}

pub(super) fn order_book_side_panel(state: &OrderBookPanelState<'_>) -> Div {
    let OrderBookPanelState {
        app,
        order_book,
        column_menu_open,
        columns,
        highlights,
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
                    tr!(panels::ORDER_BOOK_RECENTER),
                    HugeIcon::Refresh,
                    false,
                    move |cx| {
                        recenter_order_book.update(cx, ReadOnlyOrderBookView::recenter_ladder);
                    },
                    theme,
                ),
                side_panel_header_button(
                    "order_book_column_settings",
                    tr!(panels::ORDER_BOOK_CHOOSE_COLUMNS),
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
        .children(
            column_menu_open.then(|| order_book_column_menu_layer(app, columns, highlights, theme)),
        )
}

fn order_book_column_menu_layer(
    app: Entity<WorkspaceSurface>,
    columns: OrderBookColumnVisibility,
    highlights: OrderBookHighlights,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let off = || tr!(panels::ORDER_BOOK_HIGHLIGHT_OFF).to_string();
    let imbalance = highlights.imbalance_ratio().map_or_else(off, |ratio| {
        tr!(panels::ORDER_BOOK_IMBALANCE_RATIO, ratio = ratio)
    });
    let large_lots = highlights.large_lot_quantity().map_or_else(off, |size| {
        tr!(panels::ORDER_BOOK_LARGE_LOT_MINIMUM, size = size)
    });
    let panel = MenuPanel::new("order_book_column_menu", MenuPlacement::InFlow, theme)
        .width(px(196.0))
        .animate_from(PopupAnimationOrigin::TOP_RIGHT)
        .children(
            OrderBookColumn::ALL
                .into_iter()
                .map(|column| order_book_column_menu_item(app.clone(), column, columns, theme)),
        )
        .separator()
        .child(order_book_highlight_menu_item(
            "order_book_imbalance_ratio",
            tr!(panels::ORDER_BOOK_IMBALANCE),
            imbalance,
            app.clone(),
            WorkspaceSurface::cycle_order_book_imbalance_ratio,
            theme,
        ))
        .child(order_book_highlight_menu_item(
            "order_book_large_lots",
            tr!(panels::ORDER_BOOK_LARGE_LOTS),
            large_lots,
            app.clone(),
            WorkspaceSurface::cycle_order_book_large_lots,
            theme,
        ));
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
        tr!(
            panels::ORDER_BOOK_COLUMN_NOT_OFFERED,
            column = column.label()
        )
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
        OrderBookColumn::Delta => "order_book_column_delta",
        OrderBookColumn::Volume => "order_book_column_volume",
    };
    let row = MenuRow::compact(id, label, theme)
        .disabled(!offered)
        .checked(columns.is_visible(column))
        .on_click(move |_, _, cx| {
            app.update(cx, |surface, surface_cx| {
                surface.toggle_order_book_column(column, surface_cx);
            });
        });
    if reads_retained_trades(column) {
        row.detail(tr!(
            panels::ORDER_BOOK_TRADE_WINDOW,
            minutes = RECENT_LADDER_TRADE_RETENTION_NANOS / 60_000_000_000
        ))
    } else {
        row
    }
}

/// The columns drawn from the runtime's retained tape, whose window their menu rows name.
const fn reads_retained_trades(column: OrderBookColumn) -> bool {
    matches!(
        column,
        OrderBookColumn::SellTrades
            | OrderBookColumn::BuyTrades
            | OrderBookColumn::Delta
            | OrderBookColumn::Volume
    )
}

/// A row that steps one highlight through its offered values; the current value is its detail.
fn order_book_highlight_menu_item(
    id: &'static str,
    label: &'static str,
    value: String,
    app: Entity<WorkspaceSurface>,
    cycle: fn(&mut WorkspaceSurface, &mut gpui::Context<WorkspaceSurface>),
    theme: &AerisTheme,
) -> impl IntoElement {
    MenuRow::compact(id, label, theme)
        .detail(value)
        .on_click(move |_, _, cx| app.update(cx, cycle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_contracts::WorkspacePaneState;
    use prost::Message as _;

    #[test]
    fn order_book_choices_round_trip_and_defaults_persist_as_absent() {
        assert_eq!(
            persisted_order_book_state(
                OrderBookColumnVisibility::default(),
                OrderBookHighlights::default()
            ),
            None
        );
        let columns = OrderBookColumnVisibility::default()
            .with_chosen_bits(OrderBookColumnVisibility::default().chosen_bits() | 1 << 8 | 1 << 7);
        let highlights = OrderBookHighlights::from_choices(None, Some(100)).expect("offered");
        let saved = persisted_order_book_state(columns, highlights).expect("changed choices");
        assert_eq!(
            saved,
            WorkspaceOrderBookState {
                chosen_columns: 0b1_1011_1110,
                imbalance_ratio: 0,
                large_lot_quantity: 100,
            }
        );
        let decoded = WorkspaceOrderBookState::decode(saved.encode_to_vec().as_slice())
            .expect("saved choices decode");
        assert_eq!(
            restored_order_book_state(&decoded),
            (columns.chosen_bits(), highlights)
        );
    }

    #[test]
    fn damaged_order_book_choices_restore_known_columns_and_default_highlights() {
        let (chosen_columns, highlights) = restored_order_book_state(&WorkspaceOrderBookState {
            chosen_columns: 0xffff_0102,
            imbalance_ratio: 7,
            large_lot_quantity: 50,
        });
        assert_eq!(
            chosen_columns, 0x0102,
            "bits past the persisted width are dropped"
        );
        assert_eq!(highlights, OrderBookHighlights::default());
        assert_eq!(
            OrderBookColumnVisibility::default()
                .with_chosen_bits(0xfe02)
                .chosen_bits(),
            0x0202,
            "bits of no known column are dropped"
        );
    }

    #[test]
    fn documents_written_before_order_book_choices_keep_their_bytes() {
        let legacy = WorkspacePaneState {
            pane_id: 3,
            consumer_id: 4,
            side_panel_visibility: 1,
            side_panel_width: 400,
            side_panel_split_basis_points: 5_000,
            interval: "1m".into(),
            ..WorkspacePaneState::default()
        };
        let bytes = legacy.encode_to_vec();
        let decoded = WorkspacePaneState::decode(bytes.as_slice()).expect("legacy pane decodes");
        assert_eq!(decoded.order_book, None);
        // A pane restored without saved choices shows the defaults, which save as absent.
        let resaved = WorkspacePaneState {
            order_book: persisted_order_book_state(
                OrderBookColumnVisibility::default(),
                OrderBookHighlights::default(),
            ),
            ..decoded
        };
        assert_eq!(resaved.encode_to_vec(), bytes);
    }
}
