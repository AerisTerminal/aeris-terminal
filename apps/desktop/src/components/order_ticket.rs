//! Compact practice-trading controls owned by the order-book panel.

use super::{
    AerisTheme, App, Button, ButtonSize, ButtonVariant, ChromeOverlay, Div, Entity, FluentBuilder,
    HugeIcon, IntoElement, ParentElement, Styled, WorkspaceSurface, div, gpui_color, header_icon,
    px,
};
use gpui::{ClickEvent, Window};

#[derive(Clone, Copy)]
pub(super) struct TradingOrderControlsState<'a> {
    pub(super) app: &'a Entity<WorkspaceSurface>,
    pub(super) frame: Option<&'a aeris_market_data::OrderBookFrame>,
    pub(super) trading_pnl: Option<&'a aeris_trading::AccountPnl>,
    pub(super) has_open_position: bool,
    pub(super) accounts: &'a [aeris_trading::TradingAccount],
    pub(super) risk_locks: &'a [aeris_trading_runtime::RiskLock],
    pub(super) feedback: Option<&'a aeris_desktop::trading::TradingCommandFeedback>,
    pub(super) market_error: Option<&'a str>,
    pub(super) order_entry: &'a super::TradingOrderEntryState,
    pub(super) theme: &'a AerisTheme,
}

const CONTROL_HEIGHT: f32 = 32.0;
const GAP: f32 = 4.0;

pub(super) fn trading_order_controls(
    state: &TradingOrderControlsState<'_>,
) -> impl IntoElement + use<> {
    let selected_account = state.order_entry.selected_account_id.as_ref();
    let locked = selected_account.is_none()
        || state
            .risk_locks
            .iter()
            .any(|lock| Some(&lock.account_id) == selected_account);
    let ready = !locked && state.frame.is_some();
    div()
        .relative()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(GAP))
        .p_2()
        .border_t(px(state.theme.dimensions.border_width))
        .border_color(gpui_color(state.theme.colors.border))
        .bg(gpui_color(state.theme.colors.surface))
        .child(account_selector(state))
        .children(account_summary(state.trading_pnl, state.theme))
        .child(quantity_selector(
            state.app,
            state.order_entry.quantity,
            state.theme,
        ))
        .child(action_row(
            trade_order_button(
                "trading_buy_market",
                "Buy",
                state,
                ready,
                aeris_trading::OrderSide::Buy,
                move |frame, account, quantity, cx| {
                    dispatch_market(frame, account, quantity, aeris_trading::OrderSide::Buy, cx);
                },
            ),
            trade_order_button(
                "trading_sell_market",
                "Sell",
                state,
                ready,
                aeris_trading::OrderSide::Sell,
                move |frame, account, quantity, cx| {
                    dispatch_market(frame, account, quantity, aeris_trading::OrderSide::Sell, cx);
                },
            ),
        ))
        .child(action_row(
            order_button(
                "trading_join_bid",
                "Join Bid",
                state,
                ready && state.frame.is_some_and(|frame| frame.best_bid.is_some()),
                move |frame, account, quantity, cx| {
                    dispatch_join(frame, account, quantity, aeris_trading::OrderSide::Buy, cx);
                },
            ),
            order_button(
                "trading_join_ask",
                "Join Ask",
                state,
                ready && state.frame.is_some_and(|frame| frame.best_ask.is_some()),
                move |frame, account, quantity, cx| {
                    dispatch_join(frame, account, quantity, aeris_trading::OrderSide::Sell, cx);
                },
            ),
        ))
        .child(position_actions(state, ready && state.has_open_position))
        .child(global_actions(state))
        .children(status_rows(state.feedback, state.market_error, state.theme))
}

fn account_selector(state: &TradingOrderControlsState<'_>) -> impl IntoElement + use<> {
    let selected = state
        .order_entry
        .selected_account_id
        .as_ref()
        .and_then(|id| state.accounts.iter().find(|account| &account.id == id));
    let label = selected.map_or("Choose a trading account", |account| {
        account.display_name.as_str()
    });
    let badge = selected
        .map(|account| super::accounts_panel::environment_badge(account.environment, state.theme));
    let open = state.app.clone();
    // Accounts are created, chosen and deleted in the header Accounts panel; the ticket
    // only shows which one it trades on.
    Button::new("trading_account_selector", state.theme)
        .variant(ButtonVariant::Outline)
        .button_size(ButtonSize::Lg)
        .trigger()
        .full_width()
        .aria_label("Open accounts")
        .label(label.to_string())
        .when_some(badge, Button::trailing)
        .caret(header_icon(HugeIcon::ChevronDown))
        .on_click(move |_, window, cx| {
            open.update(cx, |surface, surface_cx| {
                surface.open_chrome_overlay(ChromeOverlay::Accounts, window, surface_cx);
            });
        })
}

fn account_summary(
    pnl: Option<&aeris_trading::AccountPnl>,
    theme: &AerisTheme,
) -> Option<impl IntoElement + use<>> {
    let pnl = pnl?;
    let realized_and_open = pnl.realized.checked_add(pnl.unrealized).ok().map_or_else(
        || "P/L unavailable".to_string(),
        |value| format_money(&pnl.currency, value),
    );
    let equity = pnl.equity.map_or_else(
        || "Equity unavailable".to_string(),
        |value| format!("Equity {}", format_money(&pnl.currency, value)),
    );
    let pnl_color = if pnl.realized.units().saturating_add(pnl.unrealized.units()) >= 0 {
        theme.colors.text_positive
    } else {
        theme.colors.text_negative
    };
    Some(
        div()
            .h(px(24.0))
            .px_1()
            .flex()
            .items_center()
            .justify_between()
            .text_xs()
            .child(
                div()
                    .text_color(gpui_color(theme.colors.text_secondary))
                    .child(equity),
            )
            .child(
                div()
                    .text_color(gpui_color(pnl_color))
                    .child(format!("P/L {realized_and_open}")),
            ),
    )
}

pub(super) fn format_money(currency: &str, value: aeris_trading::FixedPoint) -> String {
    let scale = usize::from(value.scale());
    let units = value.units();
    let sign = if units < 0 { "-" } else { "" };
    let magnitude = units.unsigned_abs();
    let base = 10_u64.saturating_pow(u32::try_from(scale).unwrap_or(18));
    format!(
        "{sign}{currency} {}.{:0scale$}",
        magnitude / base,
        magnitude % base,
        scale = scale
    )
}

fn quantity_selector(
    app: &Entity<WorkspaceSurface>,
    quantity: u64,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let mut row = div().h(px(CONTROL_HEIGHT)).flex().gap(px(GAP));
    let decrement = app.clone();
    let increment = app.clone();
    row = row
        .child(
            quantity_step_button("trading_quantity_decrement", false, theme).on_click(
                move |_, _, cx| {
                    decrement.update(cx, |surface, surface_cx| {
                        surface.trading_pnl.order_entry.quantity =
                            quantity.saturating_sub(1).max(1);
                        surface_cx.notify();
                    });
                },
            ),
        )
        .child(
            div()
                .h_full()
                .px_2()
                .flex()
                .items_center()
                .justify_center()
                .text_color(gpui_color(theme.colors.text_primary))
                .child(format!("{quantity} lots")),
        )
        .child(
            quantity_step_button("trading_quantity_increment", true, theme).on_click(
                move |_, _, cx| {
                    increment.update(cx, |surface, surface_cx| {
                        surface.trading_pnl.order_entry.quantity =
                            quantity.saturating_add(1).min(999);
                        surface_cx.notify();
                    });
                },
            ),
        );
    for preset in [1_u64, 3, 5, 10, 15] {
        let select = app.clone();
        row = row.child(
            ticket_button(("trading_quantity", preset), theme)
                .label(preset.to_string())
                .selected(quantity == preset)
                .on_click(move |_, _, cx| {
                    select.update(cx, |surface, surface_cx| {
                        surface.trading_pnl.order_entry.quantity = preset;
                        surface_cx.notify();
                    });
                }),
        );
    }
    row
}

/// The outline button every ticket control shares.
fn ticket_button(id: impl Into<gpui::ElementId>, theme: &AerisTheme) -> Button {
    Button::new(id, theme)
        .variant(ButtonVariant::Outline)
        .button_size(ButtonSize::Lg)
}

fn quantity_step_button(id: impl Into<gpui::ElementId>, plus: bool, theme: &AerisTheme) -> Button {
    ticket_button(id, theme)
        .aria_label(if plus {
            "Increase quantity"
        } else {
            "Decrease quantity"
        })
        .leading(quantity_step_glyph(plus, theme))
}

/// The icon set has no minus glyph, so the stepper draws its own matching +/− strokes.
fn quantity_step_glyph(plus: bool, theme: &AerisTheme) -> Div {
    let stroke = theme.colors.text_secondary;
    div()
        .relative()
        .size(px(14.0))
        .child(
            div()
                .absolute()
                .left(px(2.0))
                .top(px(6.5))
                .w(px(10.0))
                .h(px(1.0))
                .bg(gpui_color(stroke)),
        )
        .when(plus, |icon| {
            icon.child(
                div()
                    .absolute()
                    .left(px(6.5))
                    .top(px(2.0))
                    .w(px(1.0))
                    .h(px(10.0))
                    .bg(gpui_color(stroke)),
            )
        })
}

fn action_row(left: Button, right: Button) -> Div {
    div()
        .h(px(CONTROL_HEIGHT))
        .flex()
        .gap(px(GAP))
        .child(left.flex_1())
        .child(right.flex_1())
}

/// An action on the ticket's selected account at the latest book frame.
fn frame_action(
    state: &TradingOrderControlsState<'_>,
    action: impl Fn(&aeris_market_data::OrderBookFrame, Option<String>, u64, &mut App) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let frame = state.frame.cloned();
    let account = state
        .order_entry
        .selected_account_id
        .as_ref()
        .map(|id| id.as_str().to_string());
    let quantity = state.order_entry.quantity;
    move |_, _, cx| {
        if let Some(frame) = frame.as_ref() {
            action(frame, account.clone(), quantity, cx);
        }
    }
}

fn order_button(
    id: &'static str,
    label: &'static str,
    state: &TradingOrderControlsState<'_>,
    enabled: bool,
    action: impl Fn(&aeris_market_data::OrderBookFrame, Option<String>, u64, &mut App) + 'static,
) -> Button {
    ticket_button(id, state.theme)
        .strong()
        .label(label)
        .disabled(!enabled)
        .on_click(frame_action(state, action))
}

const fn trade_variant(side: aeris_trading::OrderSide) -> ButtonVariant {
    match side {
        aeris_trading::OrderSide::Buy => ButtonVariant::Positive,
        aeris_trading::OrderSide::Sell => ButtonVariant::Negative,
    }
}

fn trade_order_button(
    id: &'static str,
    label: &'static str,
    state: &TradingOrderControlsState<'_>,
    enabled: bool,
    side: aeris_trading::OrderSide,
    action: impl Fn(&aeris_market_data::OrderBookFrame, Option<String>, u64, &mut App) + 'static,
) -> Button {
    Button::new(id, state.theme)
        .variant(trade_variant(side))
        .button_size(ButtonSize::Lg)
        .strong()
        .label(label)
        .disabled(!enabled)
        .on_click(frame_action(state, action))
}

fn dispatch_market(
    frame: &aeris_market_data::OrderBookFrame,
    account: Option<String>,
    quantity: u64,
    side: aeris_trading::OrderSide,
    cx: &mut App,
) {
    aeris_desktop::trading::dispatch_simulated_selected_order(
        frame,
        side,
        aeris_desktop::trading::SimulatedOrderSelection {
            account_key: account,
            quantity,
            order_type: aeris_trading::OrderType::Market,
            time_in_force: aeris_trading::TimeInForce::Day,
            template_id: None,
        },
        cx,
    );
}

fn dispatch_join(
    frame: &aeris_market_data::OrderBookFrame,
    account: Option<String>,
    quantity: u64,
    side: aeris_trading::OrderSide,
    cx: &mut App,
) {
    let price_units = match side {
        aeris_trading::OrderSide::Buy => frame.best_bid.as_ref().map(|level| level.price),
        aeris_trading::OrderSide::Sell => frame.best_ask.as_ref().map(|level| level.price),
    };
    if let Some(price_units) = price_units {
        aeris_desktop::trading::dispatch_simulated_order_at_price(
            frame,
            aeris_desktop::trading::SimulatedPricedOrder {
                side,
                order_type: aeris_trading::OrderType::Limit,
                account_key: account,
                quantity,
                time_in_force: aeris_trading::TimeInForce::Day,
                price_units,
            },
            cx,
        );
    }
}

fn position_actions(state: &TradingOrderControlsState<'_>, enabled: bool) -> Div {
    action_row(
        order_button(
            "trading_close_position",
            "Close Position",
            state,
            enabled,
            |frame, account, _, cx| {
                aeris_desktop::trading::flatten_simulated_account_for(frame, account, cx);
            },
        ),
        order_button(
            "trading_reverse_position",
            "Reverse",
            state,
            enabled,
            |frame, account, _, cx| {
                aeris_desktop::trading::reverse_simulated_position(frame, account, cx);
            },
        ),
    )
}

fn global_actions(state: &TradingOrderControlsState<'_>) -> Div {
    let account = state
        .order_entry
        .selected_account_id
        .as_ref()
        .map(|id| id.as_str().to_string());
    let cancel_enabled = account.is_some();
    action_row(
        // Cancelling needs no book frame, only the selected account.
        ticket_button("trading_cancel_all", state.theme)
            .strong()
            .label("Cancel All")
            .disabled(!cancel_enabled)
            .on_click(move |_, _, cx| {
                aeris_desktop::trading::cancel_simulated_account(account.clone(), cx);
            }),
        order_button(
            "trading_flatten_all",
            "Flatten All",
            state,
            state.frame.is_some() && !state.accounts.is_empty(),
            |frame, _, _, cx| {
                aeris_desktop::trading::flatten_simulated_accounts(frame, cx);
            },
        ),
    )
}

fn status_rows(
    feedback: Option<&aeris_desktop::trading::TradingCommandFeedback>,
    market_error: Option<&str>,
    theme: &AerisTheme,
) -> Vec<Div> {
    feedback
        .map(|feedback| {
            div()
                .pt_1()
                .text_xs()
                .text_color(gpui_color(if feedback.is_error {
                    theme.colors.danger
                } else {
                    theme.colors.text_muted
                }))
                .child(feedback.message.clone())
        })
        .into_iter()
        .chain(market_error.map(|error| {
            div()
                .pt_1()
                .text_xs()
                .text_color(gpui_color(theme.colors.danger))
                .child(error.to_string())
        }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn market_buttons_use_the_dedicated_trade_variants() {
        // The variants' `buy-*` / `sell-*` token ramps are covered in `native_ui::button`.
        assert_eq!(
            trade_variant(aeris_trading::OrderSide::Buy),
            ButtonVariant::Positive
        );
        assert_eq!(
            trade_variant(aeris_trading::OrderSide::Sell),
            ButtonVariant::Negative
        );
    }

    #[test]
    fn money_format_keeps_exact_currency_scale() {
        assert_eq!(
            format_money(
                "USD",
                aeris_trading::FixedPoint::try_new(-12_345_678, 2).expect("value")
            ),
            "-USD 123456.78"
        );
    }
}
