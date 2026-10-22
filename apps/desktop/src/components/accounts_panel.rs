//! Accounts panel: market-data connections and the practice accounts used for trading.
//!
//! Presentation only. The market runtime owns the saved Rithmic login and its credentials; `trading_runtime` owns practice accounts. This panel dispatches their
//! commands and shows the last observed state.

use super::*;
use gpui::Stateful;

const ROW_HEIGHT: f32 = 44.0;

pub(super) fn accounts_panel_content(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    connections: &DataConnections,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    div()
        .id("accounts_panel")
        .w(px(ACCOUNTS_PANEL_WIDTH))
        .max_h(px(ACCOUNTS_PANEL_MAX_HEIGHT))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .child(section_label("Data connections", theme))
        .child(rithmic_card(&connections.rithmic, theme))
        .child(rithmic_login::rithmic_marks(theme))
        .child(
            div()
                .flex_none()
                .h(platform_border_width(theme))
                .bg(gpui_color(colors.border)),
        )
        .child(section_label("Trading accounts", theme))
        .child(practice_accounts(app_state, app, theme))
        .children(broker_accounts(app_state, app, theme))
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child(
                    "Practice accounts trade simulated funds; broker demo accounts trade on the \
                     broker's demo server.",
                ),
        )
}

/// Every trading account carries its environment, so simulated, demo and live accounts
/// are never confused where an order is placed.
pub(super) fn environment_badge(
    environment: aeris_trading::AccountEnvironment,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    // The neutral badge is the Theme System's `surface-secondary` pill with a `border-subtle`
    // edge, so it stays visible on the `surface-secondary` panels it sits in.
    let (label, background, text, edge) = match environment {
        aeris_trading::AccountEnvironment::Simulated => (
            "Simulated",
            colors.surface_secondary,
            colors.text_secondary,
            Some(colors.border_subtle),
        ),
        aeris_trading::AccountEnvironment::Demo => {
            ("Demo", colors.indigo_subtle, colors.indigo, None)
        }
        aeris_trading::AccountEnvironment::Live => (
            "Live · data only",
            colors.warning_subtle,
            colors.text_warning,
            None,
        ),
    };
    div()
        .flex_none()
        .when_some(edge, |badge, edge| {
            badge
                .border(platform_border_width(theme))
                .border_color(gpui_color(edge))
        })
        .h(px(20.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
        .bg(gpui_color(background))
        .text_xs()
        .text_color(gpui_color(text))
        .child(label)
        .into_any_element()
}

/// Broker accounts registered by the trading owner. They are never deleted here, and a
/// live account cannot be chosen for orders. Accounts recorded by a venue this build no
/// longer routes stay in the store for history but are not offered here.
fn broker_accounts(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    theme: &AerisTheme,
) -> Option<impl IntoElement> {
    let trading = &app_state.trading_pnl;
    let selected = trading.order_entry.selected_account_id.as_ref();
    let mut rows = trading
        .accounts
        .iter()
        .enumerate()
        .filter(|(_, account)| {
            account.environment != aeris_trading::AccountEnvironment::Simulated
                && account.venue_id == aeris_trading::venue::BROKER_VENUE_ID
        })
        .peekable();
    rows.peek()?;
    let mut list = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(section_label("Broker accounts", theme));
    for (index, account) in rows {
        let detail = match account.environment {
            aeris_trading::AccountEnvironment::Live => {
                "Broker live · trading is disabled".to_string()
            }
            _ if trading.connected_broker_accounts.contains(&account.id) => {
                format!("Broker demo · {}", account.currency)
            }
            _ => "Broker demo · not connected".to_string(),
        };
        let selectable = account.environment == aeris_trading::AccountEnvironment::Demo;
        list = list.child(account_row(
            app,
            AccountRow {
                index,
                account,
                detail,
                selected: selected == Some(&account.id),
                selectable,
            },
            theme,
        ));
    }
    Some(list)
}

fn section_label(label: &'static str, theme: &AerisTheme) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(platform_font_weight(TypographyRole::Strong))
        .text_color(gpui_color(theme.colors.text_secondary))
        .child(label.to_uppercase())
}

/// A connection block on the panel, inset to line up with the account rows below it.
fn card(id: &'static str) -> Stateful<Div> {
    div().id(id).flex().flex_col().gap_2().px_2()
}

fn status_dot(active: bool, theme: &AerisTheme) -> impl IntoElement {
    div()
        .flex_none()
        .size(px(8.0))
        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
        .bg(gpui_color(if active {
            theme.colors.text_positive
        } else {
            theme.colors.text_muted
        }))
}

fn provider_heading(
    name: &'static str,
    detail: String,
    active: bool,
    theme: &AerisTheme,
) -> impl IntoElement {
    div()
        .min_w_0()
        .flex_1()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(status_dot(active, theme))
                .child(
                    div()
                        .text_sm()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .text_color(gpui_color(theme.colors.text_default))
                        .child(name),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_secondary))
                .truncate()
                .child(detail),
        )
}

fn rithmic_card(view: &rithmic_login::RithmicLoginView, theme: &AerisTheme) -> impl IntoElement {
    let connected = view.connected();
    let busy = view.busy();
    let actions = if connected {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .child(
                Button::new("accounts_rithmic_change", theme)
                    .variant(ButtonVariant::Ghost)
                    .label("Change")
                    .button_size(ButtonSize::Lg)
                    .disabled(busy)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ConnectRithmic), cx);
                    }),
            )
            .child(
                Button::new("accounts_rithmic_disconnect", theme)
                    .variant(ButtonVariant::Outline)
                    .label("Disconnect")
                    .button_size(ButtonSize::Lg)
                    .loading(busy)
                    .disabled(busy)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(DisconnectRithmic), cx);
                    }),
            )
    } else {
        div().flex_none().child(
            Button::new("accounts_rithmic_connect", theme)
                .variant(ButtonVariant::Default)
                .label("Connect")
                .button_size(ButtonSize::Lg)
                .loading(busy)
                .disabled(busy)
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(ConnectRithmic), cx);
                }),
        )
    };
    card("accounts_rithmic")
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(provider_heading("Rithmic", view.detail(), connected, theme))
                .child(actions),
        )
        .children(view.message.as_ref().map(|message| {
            div()
                .text_xs()
                .text_color(gpui_color(if view.failed {
                    theme.colors.danger
                } else {
                    theme.colors.text_muted
                }))
                .child(message.clone())
        }))
}

fn practice_accounts(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    theme: &AerisTheme,
) -> impl IntoElement {
    let trading = &app_state.trading_pnl;
    let selected = trading.order_entry.selected_account_id.as_ref();
    let mut list = div().flex().flex_col().gap_1();
    let practice = trading
        .accounts
        .iter()
        .enumerate()
        .filter(|(_, account)| account.environment == aeris_trading::AccountEnvironment::Simulated)
        .collect::<Vec<_>>();
    if practice.is_empty() {
        list = list.child(
            div()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_muted))
                .child("No practice accounts yet. Create one to trade with simulated funds."),
        );
    }
    for (index, account) in practice {
        let detail = account.starting_equity.map_or_else(
            || "Practice account".to_string(),
            |equity| {
                format!(
                    "Practice · started with {}",
                    super::order_ticket::format_money(&account.currency, equity)
                )
            },
        );
        list = list.child(account_row(
            app,
            AccountRow {
                index,
                account,
                detail,
                selected: selected == Some(&account.id),
                selectable: true,
            },
            theme,
        ));
    }
    let open = app.clone();
    list.child(
        Button::new("accounts_new_practice_account", theme)
            .variant(ButtonVariant::Secondary)
            .icon(header_icon(HugeIcon::Add))
            .label("New practice account")
            .button_size(ButtonSize::Lg)
            .on_click(move |_, window, cx| {
                open.update(cx, |surface, surface_cx| {
                    open_practice_account_form(surface, window, surface_cx);
                });
            }),
    )
}

struct AccountRow<'a> {
    index: usize,
    account: &'a aeris_trading::TradingAccount,
    detail: String,
    selected: bool,
    /// Whether orders may be placed on it; live broker accounts are data-only.
    selectable: bool,
}

fn account_row(
    app: &Entity<WorkspaceSurface>,
    row: AccountRow<'_>,
    theme: &AerisTheme,
) -> impl IntoElement {
    let AccountRow {
        index,
        account,
        detail,
        selected,
        selectable,
    } = row;
    let colors = theme.colors;
    let select = app.clone();
    let account_id = account.id.clone();
    let simulated = account.environment == aeris_trading::AccountEnvironment::Simulated;
    div()
        .id(("accounts_trading_row", index))
        .h(px(ROW_HEIGHT))
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .bg(gpui_color(if selected {
            colors.active_bg.over(colors.surface_secondary)
        } else {
            colors.surface_secondary
        }))
        .when(selectable, |row| {
            row.cursor_pointer()
                .role(Role::Button)
                .aria_label(format!("Trade on {}", account.display_name))
                .hover(move |row| {
                    row.bg(gpui_color(colors.hover_bg.over(colors.surface_secondary)))
                })
                .on_click(move |_, _, cx| {
                    select.update(cx, |surface, surface_cx| {
                        surface.select_trading_account(account_id.clone(), surface_cx);
                    });
                })
        })
        .child(
            div()
                .flex_none()
                .size(px(16.0))
                .flex()
                .items_center()
                .justify_center()
                .when(selected, |slot| {
                    slot.child(
                        header_icon(HugeIcon::CheckIcon)
                            .with_size(px(16.0))
                            .color(gpui_color(colors.icon_active)),
                    )
                }),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .text_color(gpui_color(colors.text_default))
                        .truncate()
                        .child(account.display_name.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.text_secondary))
                        .truncate()
                        .child(detail),
                ),
        )
        .child(environment_badge(account.environment, theme))
        // Only practice accounts can be deleted; broker accounts belong to the broker.
        .when(simulated, |row| {
            row.child(delete_account_button(app, index, account, theme))
        })
}

fn delete_account_button(
    app: &Entity<WorkspaceSurface>,
    index: usize,
    account: &aeris_trading::TradingAccount,
    theme: &AerisTheme,
) -> impl IntoElement {
    let delete = app.clone();
    let account_id = account.id.clone();
    Button::new(("accounts_practice_delete", index), theme)
        .variant(ButtonVariant::Ghost)
        .icon(header_icon(HugeIcon::Trash))
        .aria_label(format!("Delete {}", account.display_name))
        .danger_on_hover()
        .on_click(move |_, _, cx| {
            delete.update(cx, |surface, surface_cx| {
                surface.trading_pnl.account_creator = None;
                surface.trading_pnl.account_delete_confirmation = Some(account_id.clone());
                surface_cx.notify();
            });
        })
}

/// The practice-account create or delete dialog, centered over the whole window. It is rendered
/// above the accounts panel that opened it, which stays open underneath.
pub(super) fn account_dialog_layer(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    theme: &AerisTheme,
) -> Option<AnyElement> {
    let trading = &app_state.trading_pnl;
    if let Some(creator) = trading.account_creator.as_ref() {
        return Some(create_account_dialog(app, creator, theme).into_any_element());
    }
    trading
        .account_delete_confirmation
        .as_ref()
        .map(|account_id| {
            delete_account_dialog(app, account_id, &trading.accounts, theme).into_any_element()
        })
}

fn delete_account_dialog(
    app: &Entity<WorkspaceSurface>,
    account_id: &aeris_trading::TradingAccountId,
    accounts: &[aeris_trading::TradingAccount],
    theme: &AerisTheme,
) -> ConfirmationDialog {
    let display_name = accounts
        .iter()
        .find(|account| &account.id == account_id)
        .map_or(account_id.as_str(), |account| account.display_name.as_str())
        .to_string();
    let account_key = account_id.as_str().to_string();
    let cancel = app.clone();
    let confirm = app.clone();
    ConfirmationDialog::new(
        "accounts_delete_dialog",
        "Delete practice account?",
        ConfirmationTone::Destructive,
        theme,
        move |_, cx| {
            cancel.update(cx, |surface, surface_cx| {
                surface.trading_pnl.account_delete_confirmation = None;
                surface_cx.notify();
            });
        },
        move |_, cx| {
            let service = confirm.read(cx).lifecycle.trading().clone();
            let feedback = confirm.read(cx).lifecycle.trading_feedback().clone();
            confirm.update(cx, |surface, surface_cx| {
                surface.trading_pnl.account_delete_confirmation = None;
                surface_cx.notify();
            });
            aeris_desktop::trading::delete_practice_account(
                &service,
                &feedback,
                account_key.clone(),
                cx,
            );
        },
    )
    .message(format!(
        "Delete {display_name} permanently, including its open positions, working orders, \
         fills, P/L history, risk state, and local copier references. This cannot be undone."
    ))
}

fn open_practice_account_form(
    surface: &mut WorkspaceSurface,
    window: &mut Window,
    cx: &mut Context<WorkspaceSurface>,
) {
    if surface.trading_pnl.account_creator.is_some() {
        return;
    }
    surface.trading_pnl.account_delete_confirmation = None;
    let name = cx.new(|input_cx| InputState::new(window, input_cx).placeholder("Account name"));
    let equity =
        cx.new(|input_cx| InputState::new(window, input_cx).placeholder("Starting equity"));
    name.update(cx, |input, input_cx| {
        input.focus(window, input_cx);
    });
    equity.update(cx, |input, input_cx| {
        input.set_value("50000", window, input_cx);
    });
    surface.trading_pnl.account_creator = Some(PracticeAccountDialogState { name, equity });
    cx.notify();
}

fn create_account_dialog(
    app: &Entity<WorkspaceSurface>,
    creator: &PracticeAccountDialogState,
    theme: &AerisTheme,
) -> ConfirmationDialog {
    let cancel = app.clone();
    let create = app.clone();
    let name = creator.name.clone();
    let equity = creator.equity.clone();
    ConfirmationDialog::new(
        "accounts_create_dialog",
        "New practice account",
        ConfirmationTone::Positive,
        theme,
        move |_, cx| {
            cancel.update(cx, |surface, surface_cx| {
                surface.trading_pnl.account_creator = None;
                surface_cx.notify();
            });
        },
        move |_, cx| {
            let name = name.read(cx).value().to_string();
            let equity = equity.read(cx).value().to_string();
            let service = create.read(cx).lifecycle.trading().clone();
            let feedback = create.read(cx).lifecycle.trading_feedback().clone();
            create.update(cx, |surface, surface_cx| {
                surface.trading_pnl.account_creator = None;
                surface_cx.notify();
            });
            aeris_desktop::trading::create_practice_account(&service, &feedback, name, &equity, cx);
        },
    )
    .confirm_label("Create account")
    .child(Input::new(&creator.name).platform(theme))
    .child(Input::new(&creator.equity).platform(theme))
    .child(
        div()
            .text_xs()
            .text_color(gpui_color(theme.colors.text_muted))
            .child("Starting equity in USD"),
    )
}
