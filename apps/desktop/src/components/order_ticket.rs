//! Compact practice-trading controls owned by the order-book panel.

use super::{
    AerisTheme, App, ChromeOverlay, Div, Entity, FluentBuilder, HugeIcon, InteractiveElement,
    IntoElement, ParentElement, RadiusToken, Role, SharedString, StatefulInteractiveElement,
    Styled, TypographyRole, WorkspaceSurface, div, gpui_color, header_icon, platform_font_weight,
    px,
};
use gpui::Stateful;

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
        .child(action_row([
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
        ]))
        .child(action_row([
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
        ]))
        .child(position_actions(state, ready && state.has_open_position))
        .child(global_actions(state))
        .children(status_rows(state.feedback, state.market_error, state.theme))
}

fn account_selector(state: &TradingOrderControlsState<'_>) -> impl IntoElement + use<> {
    let colors = state.theme.colors;
    let selected = state
        .order_entry
        .selected_account_id
        .as_ref()
        .and_then(|id| state.accounts.iter().find(|account| &account.id == id));
    let label = selected.map_or("Choose a practice account", |account| {
        account.display_name.as_str()
    });
    let open = state.app.clone();
    // Accounts are created, chosen and deleted in the header Accounts panel; the ticket
    // only shows which one it trades on.
    div()
        .id("trading_account_selector")
        .h(px(CONTROL_HEIGHT))
        .px_3()
        .flex()
        .items_center()
        .justify_between()
        .rounded(px(f32::from(RadiusToken::Button.logical_pixels())))
        .border(px(state.theme.dimensions.border_width))
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface))
        .text_color(gpui_color(colors.text_primary))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label("Open accounts")
        .hover(move |button| button.bg(gpui_color(colors.hover_bg)))
        .active(move |button| button.bg(gpui_color(colors.active_bg)))
        .on_click(move |_, window, cx| {
            open.update(cx, |surface, surface_cx| {
                surface.open_chrome_overlay(ChromeOverlay::Accounts, window, surface_cx);
            });
        })
        .child(div().min_w_0().truncate().child(label.to_string()))
        .child(header_icon(HugeIcon::ChevronDown))
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
            compact_button(("trading_quantity", preset), preset.to_string(), theme)
                .when(quantity == preset, |button| {
                    button
                        .bg(gpui_color(theme.colors.active_bg))
                        .text_color(gpui_color(theme.colors.text_primary))
                })
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

fn quantity_step_button(
    id: impl Into<gpui::ElementId>,
    plus: bool,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let stroke = colors.text_secondary;
    div()
        .id(id)
        .min_w(px(28.0))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(RadiusToken::Button.logical_pixels())))
        .border(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface))
        .cursor_pointer()
        .role(Role::Button)
        .hover(move |button| button.bg(gpui_color(colors.hover_bg)))
        .active(move |button| button.bg(gpui_color(colors.active_bg)))
        .child(
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
                }),
        )
}

fn compact_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    div()
        .id(id)
        .min_w(px(28.0))
        .h_full()
        .px_2()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(RadiusToken::Button.logical_pixels())))
        .border(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface))
        .text_color(gpui_color(colors.text_secondary))
        .cursor_pointer()
        .role(Role::Button)
        .hover(move |button| button.bg(gpui_color(colors.hover_bg)))
        .active(move |button| button.bg(gpui_color(colors.active_bg)))
        .child(label.into())
}

fn action_row(buttons: [Stateful<Div>; 2]) -> Div {
    div()
        .h(px(CONTROL_HEIGHT))
        .flex()
        .gap(px(GAP))
        .children(buttons)
}

fn order_button(
    id: &'static str,
    label: &'static str,
    state: &TradingOrderControlsState<'_>,
    enabled: bool,
    action: impl Fn(&aeris_market_data::OrderBookFrame, Option<String>, u64, &mut App) + 'static,
) -> Stateful<Div> {
    let frame = state.frame.cloned();
    let account = state
        .order_entry
        .selected_account_id
        .as_ref()
        .map(|id| id.as_str().to_string());
    let quantity = state.order_entry.quantity;
    let colors = state.theme.colors;
    div()
        .id(id)
        .h_full()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(RadiusToken::Button.logical_pixels())))
        .border(px(state.theme.dimensions.border_width))
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface))
        .text_color(gpui_color(if enabled {
            colors.text_primary
        } else {
            colors.text_muted
        }))
        .font_weight(platform_font_weight(TypographyRole::Strong))
        .role(Role::Button)
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(move |button| button.bg(gpui_color(colors.hover_bg)))
                .active(move |button| button.bg(gpui_color(colors.active_bg)))
                .on_click(move |_, _, cx| {
                    if let Some(frame) = frame.as_ref() {
                        action(frame, account.clone(), quantity, cx);
                    }
                })
        })
        .when(!enabled, gpui::Styled::cursor_not_allowed)
        .child(label)
}

#[derive(Clone, Copy)]
struct TradeButtonPalette {
    fill: aeris_design_system::ThemeColor,
    hover: aeris_design_system::ThemeColor,
    active: aeris_design_system::ThemeColor,
    disabled: aeris_design_system::ThemeColor,
    foreground: aeris_design_system::ThemeColor,
    disabled_foreground: aeris_design_system::ThemeColor,
}

fn trade_button_palette(theme: &AerisTheme, side: aeris_trading::OrderSide) -> TradeButtonPalette {
    let colors = theme.colors;
    match side {
        aeris_trading::OrderSide::Buy => TradeButtonPalette {
            fill: colors.buy,
            hover: colors.buy_hover,
            active: colors.buy_active,
            disabled: colors.buy_disabled,
            foreground: colors.buy_foreground,
            disabled_foreground: colors.buy_disabled_foreground,
        },
        aeris_trading::OrderSide::Sell => TradeButtonPalette {
            fill: colors.sell,
            hover: colors.sell_hover,
            active: colors.sell_active,
            disabled: colors.sell_disabled,
            foreground: colors.sell_foreground,
            disabled_foreground: colors.sell_disabled_foreground,
        },
    }
}

fn trade_order_button(
    id: &'static str,
    label: &'static str,
    state: &TradingOrderControlsState<'_>,
    enabled: bool,
    side: aeris_trading::OrderSide,
    action: impl Fn(&aeris_market_data::OrderBookFrame, Option<String>, u64, &mut App) + 'static,
) -> Stateful<Div> {
    let frame = state.frame.cloned();
    let account = state
        .order_entry
        .selected_account_id
        .as_ref()
        .map(|id| id.as_str().to_string());
    let quantity = state.order_entry.quantity;
    let palette = trade_button_palette(state.theme, side);
    div()
        .id(id)
        .h_full()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(RadiusToken::Button.logical_pixels())))
        .bg(gpui_color(if enabled {
            palette.fill
        } else {
            palette.disabled
        }))
        .text_color(gpui_color(if enabled {
            palette.foreground
        } else {
            palette.disabled_foreground
        }))
        .font_weight(platform_font_weight(TypographyRole::Strong))
        .role(Role::Button)
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(move |button| button.bg(gpui_color(palette.hover)))
                .active(move |button| button.bg(gpui_color(palette.active)))
                .on_click(move |_, _, cx| {
                    if let Some(frame) = frame.as_ref() {
                        action(frame, account.clone(), quantity, cx);
                    }
                })
        })
        .when(!enabled, gpui::Styled::cursor_not_allowed)
        .child(label)
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

fn neutral_action_button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    div()
        .id(id)
        .h_full()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(RadiusToken::Button.logical_pixels())))
        .border(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface))
        .text_color(gpui_color(if enabled {
            colors.text_primary
        } else {
            colors.text_muted
        }))
        .font_weight(platform_font_weight(TypographyRole::Strong))
        .role(Role::Button)
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(move |button| button.bg(gpui_color(colors.hover_bg)))
                .active(move |button| button.bg(gpui_color(colors.active_bg)))
        })
        .when(!enabled, gpui::Styled::cursor_not_allowed)
        .child(label)
}

fn position_actions(state: &TradingOrderControlsState<'_>, enabled: bool) -> Div {
    let close_frame = state.frame.cloned();
    let reverse_frame = state.frame.cloned();
    let account = state
        .order_entry
        .selected_account_id
        .as_ref()
        .map(|id| id.as_str().to_string());
    let close_account = account.clone();
    action_row([
        neutral_action_button(
            "trading_close_position",
            "Close Position",
            enabled,
            state.theme,
        )
        .when(enabled, |button| {
            button.on_click(move |_, _, cx| {
                if let Some(frame) = close_frame.as_ref() {
                    aeris_desktop::trading::flatten_simulated_account_for(
                        frame,
                        close_account.clone(),
                        cx,
                    );
                }
            })
        }),
        neutral_action_button("trading_reverse_position", "Reverse", enabled, state.theme).when(
            enabled,
            |button| {
                button.on_click(move |_, _, cx| {
                    if let Some(frame) = reverse_frame.as_ref() {
                        aeris_desktop::trading::reverse_simulated_position(
                            frame,
                            account.clone(),
                            cx,
                        );
                    }
                })
            },
        ),
    ])
}

fn global_actions(state: &TradingOrderControlsState<'_>) -> Div {
    let account = state
        .order_entry
        .selected_account_id
        .as_ref()
        .map(|id| id.as_str().to_string());
    let cancel_enabled = account.is_some();
    let flatten_frame = state.frame.cloned();
    action_row([
        neutral_action_button(
            "trading_cancel_all",
            "Cancel All",
            cancel_enabled,
            state.theme,
        )
        .when(cancel_enabled, |button| {
            button.on_click(move |_, _, cx| {
                aeris_desktop::trading::cancel_simulated_account(account.clone(), cx);
            })
        }),
        neutral_action_button(
            "trading_flatten_all",
            "Flatten All",
            flatten_frame.is_some() && !state.accounts.is_empty(),
            state.theme,
        )
        .when(!state.accounts.is_empty(), |button| {
            button.when_some(flatten_frame, |button, frame| {
                button.on_click(move |_, _, cx| {
                    aeris_desktop::trading::flatten_simulated_accounts(&frame, cx);
                })
            })
        }),
    ])
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
    fn market_buttons_use_the_dedicated_trade_tokens() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let buy = trade_button_palette(&theme, aeris_trading::OrderSide::Buy);
            assert_eq!(buy.fill, theme.colors.buy);
            assert_eq!(buy.hover, theme.colors.buy_hover);
            assert_eq!(buy.active, theme.colors.buy_active);
            assert_eq!(buy.disabled, theme.colors.buy_disabled);
            assert_eq!(buy.foreground, theme.colors.buy_foreground);
            assert_eq!(
                buy.disabled_foreground,
                theme.colors.buy_disabled_foreground
            );

            let sell = trade_button_palette(&theme, aeris_trading::OrderSide::Sell);
            assert_eq!(sell.fill, theme.colors.sell);
            assert_eq!(sell.hover, theme.colors.sell_hover);
            assert_eq!(sell.active, theme.colors.sell_active);
            assert_eq!(sell.disabled, theme.colors.sell_disabled);
            assert_eq!(sell.foreground, theme.colors.sell_foreground);
            assert_eq!(
                sell.disabled_foreground,
                theme.colors.sell_disabled_foreground
            );
        }
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
