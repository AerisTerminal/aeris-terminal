//! Accounts panel: market-data connections and the practice accounts used for trading.
//!
//! Presentation only. The market runtime owns broker connections and their credentials;
//! `trading_runtime` owns practice accounts. This panel dispatches their commands and shows
//! the last observed state.

use super::*;
use gpui::Stateful;

const ROW_HEIGHT: f32 = 44.0;
const CONTROL_HEIGHT: f32 = 32.0;

pub(super) fn accounts_panel_content(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    connection: &TastytradeConnectionView,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    div()
        .w(px(ACCOUNTS_PANEL_WIDTH))
        .max_h(px(ACCOUNTS_PANEL_MAX_HEIGHT))
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .child(section_label("Data connections", theme))
        .child(tastytrade_card(connection, theme))
        .child(tastytrade_attribution(theme))
        .child(public_feed_card(theme))
        .child(
            div()
                .h(px(theme.dimensions.border_width))
                .bg(gpui_color(colors.border_secondary)),
        )
        .child(section_label("Trading accounts", theme))
        .child(practice_accounts(app_state, app, theme))
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child(
                    "tastytrade provides market data only. Orders are placed on Aeris practice \
                     accounts with simulated funds.",
                ),
        )
}

fn tastytrade_attribution(theme: &AerisTheme) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .text_color(gpui_color(theme.colors.text_muted))
        .child("Market data provided by")
        .child(
            img(assets::ProviderLogo::for_theme(theme.mode).path())
                .w(px(112.0))
                .h(px(20.0))
                .object_fit(ObjectFit::Contain),
        )
}

fn section_label(label: &'static str, theme: &AerisTheme) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(platform_font_weight(TypographyRole::Strong))
        .text_color(gpui_color(theme.colors.text_secondary))
        .child(label.to_uppercase())
}

fn card(id: &'static str, theme: &AerisTheme) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap_2()
        .p_2()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border(px(theme.dimensions.border_width))
        .border_color(gpui_color(theme.colors.border_secondary))
        .bg(gpui_color(theme.colors.surface))
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
                        .text_color(gpui_color(theme.colors.text_primary))
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

fn tastytrade_card(connection: &TastytradeConnectionView, theme: &AerisTheme) -> impl IntoElement {
    let connected = connection.connected == Some(true);
    // A background check of a known state refreshes silently; only real changes show progress.
    let busy = match connection.operation {
        Some(
            TastytradeConnectionOperation::Connecting
            | TastytradeConnectionOperation::Disconnecting,
        ) => true,
        Some(TastytradeConnectionOperation::Checking) => connection.connected.is_none(),
        None => false,
    };
    let detail = match connection.operation {
        Some(TastytradeConnectionOperation::Connecting) => {
            "Complete the tastytrade login in your browser…"
        }
        Some(TastytradeConnectionOperation::Disconnecting) => "Disconnecting…",
        Some(TastytradeConnectionOperation::Checking) if connection.connected.is_none() => {
            "Checking connection…"
        }
        _ if connected => "Connected · Level 1 market data",
        _ => "Level 1 quotes, trades and candles",
    };
    let action = if connected {
        Button::new("accounts_tastytrade_disconnect")
            .variant(theme, ButtonVariant::Secondary)
            .label("Disconnect")
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(DisconnectTastytrade), cx);
            })
    } else {
        Button::new("accounts_tastytrade_connect")
            .variant(theme, ButtonVariant::Filled)
            .label("Connect")
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(ConnectTastytrade), cx);
            })
    }
    .with_size(px(CONTROL_HEIGHT))
    .loading(busy)
    .disabled(busy);
    card("accounts_tastytrade", theme)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(provider_heading(
                    "tastytrade",
                    detail.to_string(),
                    connected,
                    theme,
                ))
                .child(div().flex_none().child(action)),
        )
        .children(connection.message.as_ref().map(|message| {
            div()
                .text_xs()
                .text_color(gpui_color(if connection.failed {
                    theme.colors.danger
                } else {
                    theme.colors.text_muted
                }))
                .child(message.clone())
        }))
}

fn public_feed_card(theme: &AerisTheme) -> impl IntoElement {
    card("accounts_hyperliquid", theme).child(
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(provider_heading(
                "Hyperliquid",
                "Public market data · no login needed".to_string(),
                true,
                theme,
            ))
            .child(badge("Always on", theme)),
    )
}

fn badge(label: &'static str, theme: &AerisTheme) -> impl IntoElement {
    div()
        .flex_none()
        .h(px(20.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
        .border(px(theme.dimensions.border_width))
        .border_color(gpui_color(theme.colors.border_secondary))
        .bg(gpui_color(theme.colors.surface_secondary))
        .text_xs()
        .text_color(gpui_color(theme.colors.text_muted))
        .child(label)
}

fn practice_accounts(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    theme: &AerisTheme,
) -> impl IntoElement {
    let trading = &app_state.trading_pnl;
    let selected = trading.order_entry.selected_account_id.as_ref();
    let mut list = div().flex().flex_col().gap_1();
    if trading.accounts.is_empty() {
        list = list.child(
            div()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_muted))
                .child("No practice accounts yet. Create one to trade with simulated funds."),
        );
    }
    for (index, account) in trading.accounts.iter().enumerate() {
        list = list.child(practice_account_row(
            app,
            index,
            account,
            selected == Some(&account.id),
            theme,
        ));
    }
    let open = app.clone();
    list.child(
        Button::new("accounts_new_practice_account")
            .variant(theme, ButtonVariant::Secondary)
            .leading(header_icon(HugeIcon::Add).with_size(px(16.0)))
            .label("New practice account")
            .with_size(px(CONTROL_HEIGHT))
            .on_click(move |_, window, cx| {
                open.update(cx, |surface, surface_cx| {
                    open_practice_account_form(surface, window, surface_cx);
                });
            }),
    )
}

fn practice_account_row(
    app: &Entity<WorkspaceSurface>,
    index: usize,
    account: &aeris_trading::TradingAccount,
    selected: bool,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let select = app.clone();
    let account_id = account.id.clone();
    let detail = account.starting_equity.map_or_else(
        || "Practice account".to_string(),
        |equity| {
            format!(
                "Practice · started with {}",
                super::order_ticket::format_money(&account.currency, equity)
            )
        },
    );
    div()
        .id(("accounts_practice_row", index))
        .h(px(ROW_HEIGHT))
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border(px(theme.dimensions.border_width))
        .border_color(gpui_color(if selected {
            colors.border
        } else {
            colors.border_secondary
        }))
        .bg(gpui_color(if selected {
            colors.active_bg.over(colors.surface)
        } else {
            colors.surface
        }))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(format!("Trade on {}", account.display_name))
        .hover(move |row| row.bg(gpui_color(colors.hover_bg.over(colors.surface))))
        .on_click(move |_, _, cx| {
            select.update(cx, |surface, surface_cx| {
                surface.trading_pnl.order_entry.selected_account_id = Some(account_id.clone());
                surface.trading_pnl.current = None;
                surface_cx.notify();
            });
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
                        .text_color(gpui_color(colors.text_primary))
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
        .child(delete_account_button(app, index, account, theme))
}

fn delete_account_button(
    app: &Entity<WorkspaceSurface>,
    index: usize,
    account: &aeris_trading::TradingAccount,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let delete = app.clone();
    let account_id = account.id.clone();
    div()
        .id(("accounts_practice_delete", index))
        .flex_none()
        .size(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
        .text_color(gpui_color(colors.text_muted))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(format!("Delete {}", account.display_name))
        .hover(move |button| {
            button
                .bg(gpui_color(colors.danger))
                .text_color(gpui_color(colors.danger_foreground))
        })
        .active(move |button| {
            button
                .bg(gpui_color(colors.danger_active))
                .text_color(gpui_color(colors.danger_foreground))
        })
        .child(header_icon(HugeIcon::Trash).with_size(px(14.0)))
        .on_click(move |_, _, cx| {
            delete.update(cx, |surface, surface_cx| {
                surface.trading_pnl.account_creator = None;
                surface.trading_pnl.account_delete_confirmation = Some(account_id.clone());
                surface_cx.notify();
            });
            cx.stop_propagation();
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
            confirm.update(cx, |surface, surface_cx| {
                surface.trading_pnl.account_delete_confirmation = None;
                surface_cx.notify();
            });
            aeris_desktop::trading::delete_practice_account(account_key.clone(), cx);
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
            create.update(cx, |surface, surface_cx| {
                surface.trading_pnl.account_creator = None;
                surface_cx.notify();
            });
            aeris_desktop::trading::create_practice_account(name, &equity, cx);
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
