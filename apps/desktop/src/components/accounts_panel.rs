//! Accounts panel: a sidebar panel with the market-data connections and the trading accounts.
//! Connecting a provider, creating a practice account and confirming a deletion all happen
//! inline in the panel; nothing opens a second dialog over it.
//!
//! Presentation only. The market runtime owns the saved Rithmic login and its credentials; `trading_runtime` owns practice accounts. This panel dispatches their
//! commands and shows the last observed state.

use super::*;
use gpui::Stateful;

const ROW_HEIGHT: f32 = 44.0;
const HEADER_HEIGHT: f32 = 44.0;
const SIDEBAR_WIDTH: f32 = 168.0;

/// The Accounts panel sections.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum AccountsTab {
    #[default]
    Connections,
    TradingAccounts,
}

impl AccountsTab {
    const ALL: [Self; 2] = [Self::Connections, Self::TradingAccounts];

    const fn label(self) -> &'static str {
        match self {
            Self::Connections => "Connections",
            Self::TradingAccounts => "Trading accounts",
        }
    }

    const fn icon(self) -> HugeIcon {
        match self {
            Self::Connections => HugeIcon::Chart,
            Self::TradingAccounts => HugeIcon::User,
        }
    }
}

/// What the Connections tab shows and drives: the runtime-mirrored connection state and the
/// window-owned login form.
#[derive(Clone, Copy)]
pub(super) struct ConnectionsView<'a> {
    pub(super) terminal: &'a Entity<TerminalApp>,
    pub(super) data: &'a DataConnections,
    pub(super) rithmic_login: &'a rithmic_login::RithmicLoginFlow,
}

impl WorkspaceSurface {
    /// Opens the Accounts panel on `tab`, or switches an open panel to it.
    pub(super) fn open_accounts_panel(
        &mut self,
        tab: AccountsTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.chrome_overlay == Some(ChromeOverlay::Accounts)
            && self.chrome_overlay_phase != ChromeOverlayPhase::Closing
        {
            self.select_accounts_tab(tab, window, cx);
            return;
        }
        self.menu_state.accounts_tab = tab;
        self.open_chrome_overlay(ChromeOverlay::Accounts, window, cx);
    }

    /// Switches the panel section. An unfinished form belongs to the section it was opened in,
    /// so leaving the section drops it.
    fn select_accounts_tab(
        &mut self,
        tab: AccountsTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu_state.accounts_tab == tab {
            return;
        }
        self.menu_state.accounts_tab = tab;
        self.trading_pnl.account_creator = None;
        self.trading_pnl.account_delete_confirmation = None;
        window.dispatch_action(Box::new(CancelRithmicLogin), cx);
        self.chrome_focus.focus(window, cx);
        cx.notify();
    }
}

/// The panel's size: the design size, shrunk to the room under the header.
pub(super) fn accounts_panel_size(
    extent: super::chrome_menu::ChromeMenuExtent,
) -> gpui::Size<Pixels> {
    size(
        px(ACCOUNTS_PANEL_WIDTH.min(extent.width)),
        px(ACCOUNTS_PANEL_HEIGHT.min(extent.height())),
    )
}

pub(super) fn accounts_panel_content(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    connections: ConnectionsView<'_>,
    panel_size: gpui::Size<Pixels>,
    theme: &AerisTheme,
) -> impl IntoElement {
    let tab = app_state.menu_state.accounts_tab;
    let content = div()
        .id("accounts_panel_content")
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_y_scroll()
        .px_5()
        .py_4()
        .flex()
        .flex_col()
        .gap_3();
    let content = match tab {
        AccountsTab::Connections => connections_tab(content, connections, theme),
        AccountsTab::TradingAccounts => trading_accounts_tab(content, app_state, app, theme),
    };
    div()
        .id("accounts_panel")
        .w(panel_size.width)
        .h(panel_size.height)
        .flex()
        .flex_col()
        .child(panel_header(app, theme))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(panel_sidebar(app, tab, theme))
                .child(content),
        )
}

fn panel_header(app: &Entity<WorkspaceSurface>, theme: &AerisTheme) -> impl IntoElement {
    div()
        .h(px(HEADER_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .pl_4()
        .pr_2()
        .border_b(px(theme.dimensions.border_width))
        .border_color(gpui_color(theme.colors.border_secondary))
        .child(
            div()
                .text_sm()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .text_color(gpui_color(theme.colors.text_default))
                .child("Accounts and connections"),
        )
        .child(super::chrome_menu::chrome_menu_close_button(app, theme))
}

fn panel_sidebar(
    app: &Entity<WorkspaceSurface>,
    selected: AccountsTab,
    theme: &AerisTheme,
) -> impl IntoElement {
    let tabs = TabList::sidebar("accounts_panel_tabs", "Accounts sections").children(
        AccountsTab::ALL.into_iter().map(|tab| {
            let select = app.clone();
            Tab::new(("accounts_panel_tab", tab as usize), theme)
                .sidebar()
                .selected(selected == tab)
                .gap_2()
                .on_click(move |_, window, cx| {
                    select.update(cx, |surface, surface_cx| {
                        surface.select_accounts_tab(tab, window, surface_cx);
                    });
                    cx.stop_propagation();
                })
                .child(header_icon(tab.icon()).with_size(px(16.0)))
                .child(tab.label())
        }),
    );
    div()
        .w(px(SIDEBAR_WIDTH))
        .h_full()
        .flex_none()
        .p_2()
        .border_r(px(theme.dimensions.border_width))
        .border_color(gpui_color(theme.colors.border_secondary))
        .child(tabs)
}

fn connections_tab(
    content: Stateful<Div>,
    connections: ConnectionsView<'_>,
    theme: &AerisTheme,
) -> Stateful<Div> {
    content
        .child(section_heading(
            "Market data connections",
            "Sign in once; Aeris keeps the login on this computer and reconnects on its own.",
            theme,
        ))
        .child(rithmic_card(connections, theme))
        .child(rithmic_login::rithmic_marks(theme))
}

fn trading_accounts_tab(
    content: Stateful<Div>,
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    theme: &AerisTheme,
) -> Stateful<Div> {
    content
        .child(section_heading(
            "Practice accounts",
            "Choose the account this workspace trades on.",
            theme,
        ))
        .child(practice_accounts(app_state, app, theme))
        .children(broker_accounts(app_state, app, theme))
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_muted))
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
        .mt_2()
        .child(section_heading(
            "Broker accounts",
            "Accounts your broker reports for the connected login.",
            theme,
        ));
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

fn section_heading(
    title: &'static str,
    detail: &'static str,
    theme: &AerisTheme,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .text_sm()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .text_color(gpui_color(theme.colors.text_default))
                .child(title),
        )
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_muted))
                .child(detail),
        )
}

fn field_label(label: &'static str, theme: &AerisTheme) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(gpui_color(theme.colors.text_muted))
        .child(label)
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

/// The Rithmic connection card. Connect opens the login form inside the card.
fn rithmic_card(connections: ConnectionsView<'_>, theme: &AerisTheme) -> impl IntoElement {
    let view = &connections.data.rithmic;
    let connected = view.connected();
    let busy = view.busy();
    // A different login is a disconnect followed by a new connection, so a connected card
    // offers only Disconnect. The action steps aside while the login form is open.
    let action = (!rithmic_login::rithmic_login_form_open(connections.rithmic_login)).then(|| {
        if connected {
            Button::new("accounts_rithmic_disconnect", theme)
                .variant(ButtonVariant::Outline)
                .label("Disconnect")
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(DisconnectRithmic), cx);
                })
        } else {
            Button::new("accounts_rithmic_connect", theme)
                .variant(ButtonVariant::Default)
                .label("Connect")
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(ConnectRithmic), cx);
                })
        }
        .button_size(ButtonSize::Lg)
        .loading(busy)
        .disabled(busy)
    });
    card("accounts_rithmic")
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(provider_heading("Rithmic", view.detail(), connected, theme))
                .children(action.map(|action| div().flex_none().child(action))),
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
        .children(rithmic_login::rithmic_login_form(
            connections.terminal,
            connections.rithmic_login,
            view,
            theme,
        ))
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
    if practice.is_empty() && trading.account_creator.is_none() {
        list = list.child(
            div()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_muted))
                .child("No practice accounts yet. Create one to trade with simulated funds."),
        );
    }
    for (index, account) in practice {
        if trading.account_delete_confirmation.as_ref() == Some(&account.id) {
            list = list.child(delete_confirmation(app, account, theme));
            continue;
        }
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
    if let Some(creator) = trading.account_creator.as_ref() {
        return list.child(practice_account_form(app, creator, theme).mt_1());
    }
    let open = app.clone();
    list.child(
        div().mt_1().child(
            Button::new("accounts_new_practice_account", theme)
                .variant(ButtonVariant::Default)
                .icon(header_icon(HugeIcon::Add))
                .label("New practice account")
                .button_size(ButtonSize::Lg)
                .on_click(move |_, window, cx| {
                    open.update(cx, |surface, surface_cx| {
                        open_practice_account_form(surface, window, surface_cx);
                    });
                }),
        ),
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
            cx.stop_propagation();
        })
}

/// The account row turned into its delete confirmation, in place.
fn delete_confirmation(
    app: &Entity<WorkspaceSurface>,
    account: &aeris_trading::TradingAccount,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let cancel = app.clone();
    let confirm = app.clone();
    let account_key = account.id.as_str().to_string();
    card("accounts_delete_confirmation")
        .child(
            div()
                .text_sm()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .text_color(gpui_color(colors.text_danger))
                .child(format!("Delete {}?", account.display_name)),
        )
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_secondary))
                .child(
                    "This permanently removes its open positions, working orders, fills, P/L \
                     history, risk state, and local copier references. It cannot be undone.",
                ),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("accounts_delete_cancel", theme)
                        .variant(ButtonVariant::Outline)
                        .label("Cancel")
                        .on_click(move |_, _, cx| {
                            cancel.update(cx, |surface, surface_cx| {
                                surface.trading_pnl.account_delete_confirmation = None;
                                surface_cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new("accounts_delete_confirm", theme)
                        .variant(ButtonVariant::Destructive)
                        .label("Delete account")
                        .on_click(move |_, _, cx| {
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
                        }),
                ),
        )
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
    surface.trading_pnl.account_creator = Some(PracticeAccountFormState { name, equity });
    cx.notify();
}

fn practice_account_form(
    app: &Entity<WorkspaceSurface>,
    creator: &PracticeAccountFormState,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let cancel = app.clone();
    let create = app.clone();
    let name = creator.name.clone();
    let equity = creator.equity.clone();
    card("accounts_practice_form")
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .text_color(gpui_color(theme.colors.text_default))
                .child("New practice account"),
        )
        .child(field_label("Account name", theme))
        .child(Input::new(&creator.name).platform(theme))
        .child(field_label("Starting equity in USD", theme))
        .child(Input::new(&creator.equity).platform(theme))
        .child(
            div()
                .mt_1()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("accounts_create_cancel", theme)
                        .variant(ButtonVariant::Outline)
                        .label("Cancel")
                        .on_click(move |_, _, cx| {
                            cancel.update(cx, |surface, surface_cx| {
                                surface.trading_pnl.account_creator = None;
                                surface_cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new("accounts_create_confirm", theme)
                        .variant(ButtonVariant::Primary)
                        .label("Create account")
                        .on_click(move |_, _, cx| {
                            let name = name.read(cx).value().to_string();
                            let equity = equity.read(cx).value().to_string();
                            let service = create.read(cx).lifecycle.trading().clone();
                            let feedback = create.read(cx).lifecycle.trading_feedback().clone();
                            create.update(cx, |surface, surface_cx| {
                                surface.trading_pnl.account_creator = None;
                                surface_cx.notify();
                            });
                            aeris_desktop::trading::create_practice_account(
                                &service, &feedback, name, &equity, cx,
                            );
                        }),
                ),
        )
}
