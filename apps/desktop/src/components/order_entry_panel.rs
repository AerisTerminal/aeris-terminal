//! Docked order entry panel: the practice-trading ticket for the pane's market.

use super::order_ticket::{TradingOrderControlsState, trading_order_controls};
use super::side_panel_dock::side_panel_header;
use super::{
    AnyElement, Div, InteractiveElement, ParentElement, SIDE_PANEL_MINIMUM_WIDTH, SidePanel,
    StatefulInteractiveElement, Styled, div, gpui_color,
};

/// Docked width, including its leading border: the narrowest width the ticket's quantity
/// presets and paired order buttons are laid out for.
pub(super) const ORDER_ENTRY_PANEL_WIDTH: f32 = SIDE_PANEL_MINIMUM_WIDTH;

pub(super) fn order_entry_side_panel(ticket: &TradingOrderControlsState<'_>) -> Div {
    let theme = ticket.theme;
    div()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(gpui_color(theme.colors.surface))
        .size_full()
        .child(side_panel_header(
            SidePanel::OrderEntry,
            ticket.app.clone(),
            std::iter::empty::<AnyElement>(),
            theme,
        ))
        .child(
            div()
                .id("order_entry_ticket")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(trading_order_controls(ticket)),
        )
}
