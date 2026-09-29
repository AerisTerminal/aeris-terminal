//! Compact practice-trading controls owned by the order-book panel.

use super::*;
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
    pub(super) account_creator: Option<&'a super::PracticeAccountDialogState>,
    pub(super) theme: &'a AerisTheme,
}

const CONTROL_HEIGHT: f32 = 36.0;
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
        .border_t_1()
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
            order_button(
                "trading_buy_market",
                "Buy",
                state,
                ready,
                state.theme.colors.bullish,
                move |frame, account, quantity, cx| {
                    dispatch_market(frame, account, quantity, aeris_trading::OrderSide::Buy, cx);
                },
            ),
            order_button(
                "trading_sell_market",
                "Sell",
                state,
                ready,
                state.theme.colors.bearish,
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
                state.theme.colors.text_secondary,
                move |frame, account, quantity, cx| {
                    dispatch_join(frame, account, quantity, aeris_trading::OrderSide::Buy, cx);
                },
            ),
            order_button(
                "trading_join_ask",
                "Join Ask",
                state,
                ready && state.frame.is_some_and(|frame| frame.best_ask.is_some()),
                state.theme.colors.text_secondary,
                move |frame, account, quantity, cx| {
                    dispatch_join(frame, account, quantity, aeris_trading::OrderSide::Sell, cx);
                },
            ),
        ]))
        .child(position_actions(state, ready && state.has_open_position))
        .child(global_actions(state))
        .children(status_rows(state.feedback, state.market_error, state.theme))
        .children(
            state
                .account_creator
                .map(|creator| practice_account_dialog(state.app, creator, state.theme)),
        )
}

fn account_selector(state: &TradingOrderControlsState<'_>) -> impl IntoElement + use<> {
    let colors = state.theme.colors;
    let selected = state
        .order_entry
        .selected_account_id
        .as_ref()
        .and_then(|id| state.accounts.iter().find(|account| &account.id == id));
    let label = selected.map_or("Add practice account", |account| {
        account.display_name.as_str()
    });
    let toggle = state.app.clone();
    let create = state.app.clone();
    let mut selector = div().relative().child(
        div()
            .id("trading_account_selector")
            .h(px(CONTROL_HEIGHT))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .rounded(px(5.0))
            .border_1()
            .border_color(gpui_color(colors.border_secondary))
            .bg(gpui_color(colors.surface_secondary))
            .text_color(gpui_color(colors.text_primary))
            .cursor_pointer()
            .role(Role::Button)
            .aria_label("Select or add a practice account")
            .on_click(move |_, window, cx| {
                toggle.update(cx, |surface, surface_cx| {
                    if surface.trading_pnl.accounts.is_empty() {
                        open_practice_account_dialog(surface, window, surface_cx);
                    } else {
                        surface.trading_pnl.order_entry.account_menu_open =
                            !surface.trading_pnl.order_entry.account_menu_open;
                        surface_cx.notify();
                    }
                });
            })
            .child(div().min_w_0().truncate().child(label.to_string()))
            .child(header_icon(HugeIcon::ChevronDown)),
    );
    if state.order_entry.account_menu_open {
        let mut menu = div()
            .id("trading_account_menu")
            .absolute()
            .top(px(CONTROL_HEIGHT + 2.0))
            .left_0()
            .right_0()
            .max_h(px(240.0))
            .overflow_y_scroll()
            .occlude()
            .rounded(px(5.0))
            .border_1()
            .border_color(gpui_color(colors.border_secondary))
            .bg(gpui_color(colors.surface))
            .shadow_md()
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation());
        for (index, account) in state.accounts.iter().enumerate() {
            let select = state.app.clone();
            let account_id = account.id.clone();
            menu = menu.child(
                MenuRow::compact(
                    ("trading_account_option", index),
                    account.display_name.clone(),
                    state.theme,
                )
                .highlighted(state.order_entry.selected_account_id.as_ref() == Some(&account.id))
                .on_click(move |_, _, cx| {
                    select.update(cx, |surface, surface_cx| {
                        surface.trading_pnl.order_entry.selected_account_id =
                            Some(account_id.clone());
                        surface.trading_pnl.order_entry.account_menu_open = false;
                        surface.trading_pnl.current = None;
                        surface_cx.notify();
                    });
                }),
            );
        }
        menu = menu.child(
            MenuRow::compact("trading_add_account", "Add practice account…", state.theme)
                .leading(header_icon(HugeIcon::Add).with_size(px(16.0)))
                .on_click(move |_, window, cx| {
                    create.update(cx, |surface, surface_cx| {
                        surface.trading_pnl.order_entry.account_menu_open = false;
                        open_practice_account_dialog(surface, window, surface_cx);
                    });
                }),
        );
        selector = selector.child(gpui::deferred(menu));
    }
    selector
}

fn account_summary(
    pnl: Option<&aeris_trading::AccountPnl>,
    theme: &AerisTheme,
) -> Option<impl IntoElement + use<>> {
    let pnl = pnl?;
    let realized_and_open = pnl.realized.checked_add(pnl.unrealized).ok().map_or_else(
        || "P/L unavailable".to_string(),
        |value| format_money(pnl, value),
    );
    let equity = pnl.equity.map_or_else(
        || "Equity unavailable".to_string(),
        |value| format!("Equity {}", format_money(pnl, value)),
    );
    let pnl_color = if pnl.realized.units().saturating_add(pnl.unrealized.units()) >= 0 {
        theme.colors.bullish
    } else {
        theme.colors.bearish
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

fn format_money(pnl: &aeris_trading::AccountPnl, value: aeris_trading::FixedPoint) -> String {
    let scale = usize::from(value.scale());
    let units = value.units();
    let sign = if units < 0 { "-" } else { "" };
    let magnitude = units.unsigned_abs();
    let base = 10_u64.saturating_pow(u32::try_from(scale).unwrap_or(18));
    format!(
        "{sign}{} {}.{:0scale$}",
        pnl.currency,
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
            compact_button("trading_quantity_decrement", "−", theme).on_click(move |_, _, cx| {
                decrement.update(cx, |surface, surface_cx| {
                    surface.trading_pnl.order_entry.quantity = quantity.saturating_sub(1).max(1);
                    surface_cx.notify();
                });
            }),
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
            compact_button("trading_quantity_increment", "+", theme).on_click(move |_, _, cx| {
                increment.update(cx, |surface, surface_cx| {
                    surface.trading_pnl.order_entry.quantity = quantity.saturating_add(1).min(999);
                    surface_cx.notify();
                });
            }),
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

fn compact_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let hover = theme.colors.hover_bg;
    div()
        .id(id)
        .min_w(px(28.0))
        .h_full()
        .px_2()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.0))
        .border_1()
        .border_color(gpui_color(theme.colors.border_secondary))
        .text_color(gpui_color(theme.colors.text_secondary))
        .cursor_pointer()
        .role(Role::Button)
        .hover(move |button| button.bg(gpui_color(hover)))
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
    tone: aeris_design_system::ThemeColor,
    action: impl Fn(&aeris_market_data::OrderBookFrame, Option<String>, u64, &mut App) + 'static,
) -> Stateful<Div> {
    let frame = state.frame.cloned();
    let account = state
        .order_entry
        .selected_account_id
        .as_ref()
        .map(|id| id.as_str().to_string());
    let quantity = state.order_entry.quantity;
    let background = if enabled {
        tone
    } else {
        state.theme.colors.hover_bg
    };
    div()
        .id(id)
        .h_full()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.0))
        .bg(gpui_color(background))
        .text_color(gpui_color(if enabled {
            state.theme.colors.surface
        } else {
            state.theme.colors.text_muted
        }))
        .font_weight(platform_font_weight(TypographyRole::Strong))
        .role(Role::Button)
        .when(enabled, |button| {
            button.cursor_pointer().on_click(move |_, _, cx| {
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
    let hover = theme.colors.button_fill_hover;
    div()
        .id(id)
        .h_full()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.0))
        .bg(gpui_color(theme.colors.button_fill))
        .text_color(gpui_color(if enabled {
            theme.colors.button_fill_foreground
        } else {
            theme.colors.text_muted
        }))
        .font_weight(platform_font_weight(TypographyRole::Strong))
        .role(Role::Button)
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(move |button| button.bg(gpui_color(hover)))
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

fn open_practice_account_dialog(
    surface: &mut WorkspaceSurface,
    window: &mut Window,
    cx: &mut Context<WorkspaceSurface>,
) {
    if surface.trading_pnl.account_creator.is_some() {
        return;
    }
    let name = cx.new(|input_cx| InputState::new(window, input_cx).placeholder("Account name"));
    let equity =
        cx.new(|input_cx| InputState::new(window, input_cx).placeholder("Starting equity"));
    equity.update(cx, |input, input_cx| {
        input.set_value("50000", window, input_cx);
    });
    surface.trading_pnl.account_creator = Some(super::PracticeAccountDialogState { name, equity });
    cx.notify();
}

fn practice_account_dialog(
    app: &Entity<WorkspaceSurface>,
    creator: &super::PracticeAccountDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let cancel = app.clone();
    let create = app.clone();
    let name = creator.name.clone();
    let equity = creator.equity.clone();
    div()
        .id("practice_account_dialog_scrim")
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui_color(theme.colors.surface.with_alpha(0.86)))
        .child(
            div()
                .w_full()
                .mx_2()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .rounded(px(6.0))
                .border_1()
                .border_color(gpui_color(theme.colors.border_secondary))
                .bg(gpui_color(theme.colors.surface))
                .child(
                    div()
                        .text_sm()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .child("Create practice account"),
                )
                .child(Input::new(&creator.name).platform(theme))
                .child(Input::new(&creator.equity).platform(theme))
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui_color(theme.colors.text_muted))
                        .child("Starting equity in USD"),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("practice_account_cancel")
                                .variant(theme, ButtonVariant::Secondary)
                                .label("Cancel")
                                .on_click(move |_, _, cx| {
                                    cancel.update(cx, |surface, surface_cx| {
                                        surface.trading_pnl.account_creator = None;
                                        surface_cx.notify();
                                    });
                                }),
                        )
                        .child(
                            Button::new("practice_account_create")
                                .variant(theme, ButtonVariant::Filled)
                                .label("Create")
                                .on_click(move |_, _, cx| {
                                    let name = name.read(cx).value().to_string();
                                    let equity = equity.read(cx).value().to_string();
                                    create.update(cx, |surface, surface_cx| {
                                        surface.trading_pnl.account_creator = None;
                                        surface_cx.notify();
                                    });
                                    aeris_desktop::trading::create_practice_account(
                                        name, &equity, cx,
                                    );
                                }),
                        ),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_format_keeps_exact_currency_scale() {
        let pnl = aeris_trading::AccountPnl {
            account_id: aeris_trading::TradingAccountId::try_new("test").expect("account"),
            currency: "USD".to_string(),
            realized: aeris_trading::FixedPoint::try_new(0, 2).expect("realized"),
            unrealized: aeris_trading::FixedPoint::try_new(0, 2).expect("unrealized"),
            equity: None,
        };
        assert_eq!(
            format_money(
                &pnl,
                aeris_trading::FixedPoint::try_new(-12_345_678, 2).expect("value")
            ),
            "-USD 123456.78"
        );
    }
}
