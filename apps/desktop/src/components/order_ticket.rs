//! Compact practice-trading controls owned by the order-book panel.

use super::Input;
use super::InputState;
use super::{
    AerisTheme, App, Button, ButtonSize, ButtonVariant, ChromeOverlay, Div, Entity, FluentBuilder,
    HugeIcon, IntoElement, ParentElement, StatefulInteractiveElement, Styled, Tab, TabList,
    WorkspaceSurface, div, gpui_color, header_icon, px,
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
    pub(super) quantity: &'a super::order_quantity::OrderQuantityControls,
    pub(super) price: &'a Entity<InputState>,
    pub(super) price_scale: Option<u32>,
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
        .child(quantity_selector(state))
        .child(
            div()
                .h(px(CONTROL_HEIGHT))
                .flex()
                .child(Input::new(state.price).platform(state.theme).flex_1()),
        )
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
        .child(action_row(
            priced_order_button("trading_buy_limit", "Buy Limit", state, ready, true, false),
            priced_order_button(
                "trading_sell_limit",
                "Sell Limit",
                state,
                ready,
                false,
                false,
            ),
        ))
        .child(action_row(
            priced_order_button("trading_buy_stop", "Buy Stop", state, ready, true, true),
            priced_order_button("trading_sell_stop", "Sell Stop", state, ready, false, true),
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
    let realized_and_open = pnl
        .unrealized
        .and_then(|unrealized| pnl.realized.checked_add(unrealized).ok());
    let realized_and_open_text = realized_and_open.map_or_else(
        || "unavailable".to_string(),
        |value| format_money(&pnl.currency, value),
    );
    let equity = pnl.equity.map_or_else(
        || "Equity unavailable".to_string(),
        |value| format!("Equity {}", format_money(&pnl.currency, value)),
    );
    let pnl_color = match realized_and_open {
        None => theme.colors.text_secondary,
        Some(value) if value.units() >= 0 => theme.colors.text_positive,
        Some(_) => theme.colors.text_negative,
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
                    .child(format!("P/L {realized_and_open_text}")),
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

fn quantity_selector(state: &TradingOrderControlsState<'_>) -> impl IntoElement + use<> {
    let theme = state.theme;
    let quantity = state.order_entry.quantity;
    let step = |id: &'static str, plus: bool| {
        let app = state.app.clone();
        quantity_step_button(id, plus, theme).on_click(move |_, window, cx| {
            app.update(cx, |surface, surface_cx| {
                let next = if plus {
                    quantity.saturating_add(1)
                } else {
                    quantity.saturating_sub(1)
                };
                surface.set_order_quantity(next, window, surface_cx);
            });
        })
    };
    div()
        .h(px(CONTROL_HEIGHT))
        .flex()
        .items_center()
        .gap(px(GAP))
        .child(step("trading_quantity_decrement", false))
        .child(super::order_quantity::ticket_quantity_field(
            state.quantity,
            theme,
        ))
        .child(step("trading_quantity_increment", true))
        .child(
            TabList::new("trading_quantity_presets", "Quantity presets", theme).children(
                [1_u64, 3, 5, 10, 15].map(|preset| {
                    let select = state.app.clone();
                    Tab::new(("trading_quantity", preset), theme)
                        .selected(quantity == preset)
                        .child(preset.to_string())
                        .on_click(move |_, window, cx| {
                            select.update(cx, |surface, surface_cx| {
                                surface.set_order_quantity(preset, window, surface_cx);
                            });
                        })
                }),
            ),
        )
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
        .icon(header_icon(if plus {
            HugeIcon::Add
        } else {
            HugeIcon::Minus
        }))
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
        .label(label)
        .disabled(!enabled)
        .on_click(frame_action(state, action))
}

pub(super) const fn trade_variant(side: aeris_trading::OrderSide) -> ButtonVariant {
    match side {
        aeris_trading::OrderSide::Buy => ButtonVariant::Buy,
        aeris_trading::OrderSide::Sell => ButtonVariant::Sell,
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
        .label(label)
        .disabled(!enabled)
        .on_click(frame_action(state, action))
}

pub(super) fn dispatch_market(
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

/// Scale a typed decimal without binary floating point or silent rounding.
fn parse_ticket_price(text: &str, scale: u32) -> Option<i64> {
    let text = text.trim();
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > usize::try_from(scale).ok()?
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let base = 10_i64.checked_pow(scale)?;
    let whole = whole.parse::<i64>().ok()?;
    let fraction_units = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<i64>()
            .ok()?
            .checked_mul(10_i64.checked_pow(scale - u32::try_from(fraction.len()).ok()?)?)?
    };
    whole.checked_mul(base)?.checked_add(fraction_units)
}

fn priced_action(buy: bool, stop: bool) -> (aeris_trading::OrderSide, aeris_trading::OrderType) {
    (
        if buy {
            aeris_trading::OrderSide::Buy
        } else {
            aeris_trading::OrderSide::Sell
        },
        if stop {
            aeris_trading::OrderType::Stop
        } else {
            aeris_trading::OrderType::Limit
        },
    )
}

fn priced_order_button(
    id: &'static str,
    label: &'static str,
    state: &TradingOrderControlsState<'_>,
    enabled: bool,
    buy: bool,
    stop: bool,
) -> Button {
    let price = state.price.clone();
    let scale = state.price_scale;
    let (side, order_type) = priced_action(buy, stop);
    order_button(
        id,
        label,
        state,
        enabled,
        move |frame, account, quantity, cx| {
            let text = price.read(cx).value();
            let Some(price_units) = scale
                .filter(|scale| *scale == u32::from(frame.price_scale))
                .and_then(|scale| parse_ticket_price(text.as_ref(), scale))
            else {
                aeris_desktop::trading::record_notice(Err(
                    "Enter a valid order price at the instrument's price precision".to_string(),
                ));
                return;
            };
            aeris_desktop::trading::dispatch_simulated_order_at_price(
                frame,
                aeris_desktop::trading::SimulatedPricedOrder {
                    side,
                    order_type,
                    account_key: account,
                    quantity,
                    time_in_force: aeris_trading::TimeInForce::Day,
                    price_units,
                },
                cx,
            );
        },
    )
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
            .label("Cancel All")
            .disabled(!cancel_enabled)
            .on_click(move |_, _, cx| {
                aeris_desktop::trading::cancel_simulated_account(account.clone(), cx);
            }),
        order_button(
            "trading_flatten_all",
            "Flatten All",
            state,
            !state.accounts.is_empty(),
            |_, _, _, cx| {
                aeris_desktop::trading::flatten_simulated_accounts(cx);
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
            ButtonVariant::Buy
        );
        assert_eq!(
            trade_variant(aeris_trading::OrderSide::Sell),
            ButtonVariant::Sell
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

    #[test]
    fn entered_price_scales_exactly_without_rounding() {
        assert_eq!(parse_ticket_price(" 82195.25 ", 2), Some(8_219_525));
        assert_eq!(parse_ticket_price("0.00000001", 8), Some(1));
        assert_eq!(parse_ticket_price("1.234", 2), None);
        assert_eq!(parse_ticket_price("", 2), None);
        assert_eq!(parse_ticket_price("NaN", 2), None);
        assert_eq!(parse_ticket_price("-1", 2), None);
        assert_eq!(parse_ticket_price("9223372036854775808", 0), None);
    }

    #[test]
    fn entered_price_actions_map_side_and_order_type() {
        for (buy, stop, side, order_type) in [
            (
                true,
                false,
                aeris_trading::OrderSide::Buy,
                aeris_trading::OrderType::Limit,
            ),
            (
                false,
                false,
                aeris_trading::OrderSide::Sell,
                aeris_trading::OrderType::Limit,
            ),
            (
                true,
                true,
                aeris_trading::OrderSide::Buy,
                aeris_trading::OrderType::Stop,
            ),
            (
                false,
                true,
                aeris_trading::OrderSide::Sell,
                aeris_trading::OrderType::Stop,
            ),
        ] {
            assert_eq!(priced_action(buy, stop), (side, order_type));
        }
    }
}
