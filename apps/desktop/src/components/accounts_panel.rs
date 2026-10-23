//! Accounts panel: a floating sidebar panel with the market-data connections and the trading
//! accounts. It opens centered in the window and moves by its title bar, like the chart
//! settings panel. Connecting a provider, creating a practice account and confirming a deletion
//! all happen inline in the panel; nothing opens a second dialog over it.
//!
//! Presentation only. The market runtime owns the saved Rithmic login and its credentials;
//! `trading_runtime` owns practice accounts. This panel dispatches their commands and shows the
//! last observed state.

use super::big_trades_dialog::{
    control_strip, described_setting_row, settings_group, settings_groups,
};
use super::*;

const HEADER_HEIGHT: f32 = 44.0;
const SIDEBAR_WIDTH: f32 = 184.0;
/// Login and account fields hold system names and user IDs, wider than a setting value.
pub(super) const ACCOUNTS_FIELD_WIDTH: f32 = 240.0;

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

/// Drag payload for moving the Accounts panel by its title bar.
#[derive(Clone)]
pub(super) struct AccountsPanelMoveDrag;

impl Render for AccountsPanelMoveDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
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

    /// Follows a title-bar drag; the layer clamps the stored origin inside the window.
    pub(super) fn move_accounts_panel(
        &mut self,
        pointer: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.menu_state.accounts_placement.follow(pointer) {
            cx.notify();
        }
    }
}

/// The panel's size: the design size, shrunk to the room the window leaves.
pub(super) fn accounts_panel_size(
    extent: super::chrome_menu::ChromeMenuExtent,
) -> gpui::Size<Pixels> {
    size(
        px(ACCOUNTS_PANEL_WIDTH.min(extent.width)),
        px(ACCOUNTS_PANEL_HEIGHT.min(extent.height())),
    )
}

/// Where the panel sits: centered until the viewer drags it, and always inside the window.
pub(super) fn accounts_panel_bounds(
    app_state: &WorkspaceSurface,
    extent: super::chrome_menu::ChromeMenuExtent,
    viewport: gpui::Size<Pixels>,
) -> Bounds<Pixels> {
    let panel_size = accounts_panel_size(extent);
    let origin = chart_context_menus::clamp_floating_panel_origin(
        app_state
            .menu_state
            .accounts_placement
            .origin
            .unwrap_or_else(|| {
                chart_context_menus::centered_floating_panel_origin(viewport, panel_size)
            }),
        viewport,
        panel_size,
    );
    Bounds::new(origin, panel_size)
}

pub(super) fn accounts_panel_content(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    connections: ConnectionsView<'_>,
    panel_size: gpui::Size<Pixels>,
    theme: &AerisTheme,
) -> impl IntoElement {
    let tab = app_state.menu_state.accounts_tab;
    let groups = match tab {
        AccountsTab::Connections => connections_groups(connections, theme),
        AccountsTab::TradingAccounts => trading_account_groups(app_state, app, theme),
    };
    div()
        .id("accounts_panel")
        .w(panel_size.width)
        .h(panel_size.height)
        .flex()
        .flex_col()
        .child(panel_title_bar(app, theme))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(panel_sidebar(app_state, app, tab, theme))
                .child(
                    div()
                        .id("accounts_panel_content")
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_6()
                        .py_4()
                        .child(settings_groups(groups, theme)),
                ),
        )
}

/// Title bar of the panel. Dragging it moves the panel anywhere in the window.
fn panel_title_bar(app: &Entity<WorkspaceSurface>, theme: &AerisTheme) -> impl IntoElement {
    let colors = theme.colors;
    let grab = app.clone();
    div()
        .id("accounts_panel_title_bar")
        .relative()
        .h(px(HEADER_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .pl_4()
        .pr_2()
        .border_b(platform_border_width(theme))
        .border_color(gpui_color(colors.border_secondary))
        .cursor(FLOATING_PANEL_MOVE_CURSOR)
        .on_drag(AccountsPanelMoveDrag, move |drag, offset, _, cx| {
            grab.update(cx, |surface, _| {
                surface.menu_state.accounts_placement.grab(offset);
            });
            cx.new(|_| drag.clone())
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    header_icon(HugeIcon::User)
                        .with_size(px(16.0))
                        .color(gpui_color(colors.icon)),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .text_color(gpui_color(colors.text_default))
                        .child("Accounts and connections"),
                ),
        )
        .child(chart_context_menus::floating_panel_drag_grip(theme))
        .child(super::chrome_menu::chrome_menu_close_button(app, theme))
}

/// Section tabs, with the account this workspace trades on pinned under them.
fn panel_sidebar(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    selected: AccountsTab,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let tabs = TabList::sidebar("accounts_panel_tabs", "Accounts sections")
        .flex_1()
        .min_h_0()
        .children(AccountsTab::ALL.into_iter().map(|tab| {
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
        }));
    let trading = &app_state.trading_pnl;
    let active = trading
        .order_entry
        .selected_account_id
        .as_ref()
        .and_then(|id| trading.accounts.iter().find(|account| &account.id == id));
    div()
        .w(px(SIDEBAR_WIDTH))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .p_2()
        .border_r(platform_border_width(theme))
        .border_color(gpui_color(colors.border_secondary))
        .child(tabs)
        .child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap_1()
                .px_2()
                .pt_3()
                .pb_1()
                .border_t(platform_border_width(theme))
                .border_color(gpui_color(colors.border))
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.text_muted))
                        .child("Trading on"),
                )
                .child(
                    div()
                        .text_sm()
                        .truncate()
                        .text_color(gpui_color(if active.is_some() {
                            colors.text_default
                        } else {
                            colors.text_muted
                        }))
                        .child(active.map_or_else(
                            || "No account chosen".to_string(),
                            |account| account.display_name.clone(),
                        )),
                ),
        )
}

fn connections_groups(connections: ConnectionsView<'_>, theme: &AerisTheme) -> Vec<AnyElement> {
    vec![
        rithmic_group(connections, theme),
        settings_group(
            "Hyperliquid",
            [described_setting_row(
                "Hyperliquid",
                [SharedString::from(
                    "Crypto markets from Hyperliquid's public feed",
                )],
                control_strip(
                    feed_level_pill(TerminalProvider::Hyperliquid, theme)
                        .into_iter()
                        .chain(std::iter::once(pill(
                            "No login needed",
                            theme.colors.surface_secondary,
                            theme.colors.text_secondary,
                            Some(theme.colors.border_subtle),
                            theme,
                        ))),
                ),
                theme,
            )],
            theme,
        ),
    ]
}

/// The depth the provider's registration declares for its live feed.
fn feed_level_pill(provider: TerminalProvider, theme: &AerisTheme) -> Option<AnyElement> {
    let label = provider_presentation(provider)?.feed_level.label()?;
    Some(pill(
        label,
        theme.colors.surface_secondary,
        theme.colors.text_secondary,
        Some(theme.colors.border_subtle),
        theme,
    ))
}

/// The Rithmic connection. Connect opens the login form in place; the connection row keeps
/// its status while the form is open.
fn rithmic_group(connections: ConnectionsView<'_>, theme: &AerisTheme) -> AnyElement {
    let colors = theme.colors;
    let view = &connections.data.rithmic;
    let connected = view.connected();
    let busy = view.busy();
    let form_open = rithmic_login::rithmic_login_form_open(connections.rithmic_login);
    // A different login is a disconnect followed by a new connection, so a connected row
    // offers only Disconnect.
    let action = (!form_open).then(|| {
        if connected {
            Button::new("accounts_rithmic_disconnect", theme)
                .variant(ButtonVariant::Secondary)
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
        .loading(busy)
        .disabled(busy)
        .into_any_element()
    });
    let status = if connected {
        pill(
            "Connected",
            colors.positive_subtle,
            colors.text_positive,
            None,
            theme,
        )
    } else {
        pill(
            "Not connected",
            colors.surface_secondary,
            colors.text_secondary,
            Some(colors.border_subtle),
            theme,
        )
    };
    let mut rows = vec![described_setting_row(
        "Rithmic",
        [SharedString::from(view.detail())],
        control_strip(
            feed_level_pill(TerminalProvider::Rithmic, theme)
                .into_iter()
                .chain(std::iter::once(status))
                .chain(action),
        ),
        theme,
    )];
    rows.extend(view.message.as_ref().map(|message| {
        div()
            .pb_2()
            .text_xs()
            .text_color(gpui_color(if view.failed {
                colors.danger
            } else {
                colors.text_secondary
            }))
            .child(message.clone())
            .into_any_element()
    }));
    rows.extend(rithmic_login::rithmic_login_form(
        connections.terminal,
        connections.rithmic_login,
        view,
        theme,
    ));
    rows.push(
        div()
            .pt_2()
            .child(rithmic_login::rithmic_marks(theme))
            .into_any_element(),
    );
    settings_group("Rithmic · CME Group futures", rows, theme)
}

fn trading_account_groups(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    theme: &AerisTheme,
) -> Vec<AnyElement> {
    let mut groups = vec![practice_accounts(app_state, app, theme)];
    groups.extend(broker_accounts(app_state, app, theme));
    groups
}

/// A `radius-large` pill: a neutral badge passes its `border-subtle` edge, a tinted badge
/// pairs a `-subtle` fill with its text color and has none.
fn pill(
    label: &'static str,
    background: ThemeColor,
    text: ThemeColor,
    edge: Option<ThemeColor>,
    theme: &AerisTheme,
) -> AnyElement {
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

/// Every trading account carries its environment, so simulated, demo and live accounts
/// are never confused where an order is placed.
pub(super) fn environment_badge(
    environment: aeris_trading::AccountEnvironment,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    match environment {
        aeris_trading::AccountEnvironment::Simulated => pill(
            "Simulated",
            colors.surface_secondary,
            colors.text_secondary,
            Some(colors.border_subtle),
            theme,
        ),
        aeris_trading::AccountEnvironment::Demo => {
            pill("Demo", colors.indigo_subtle, colors.indigo, None, theme)
        }
        aeris_trading::AccountEnvironment::Live => pill(
            "Live · data only",
            colors.warning_subtle,
            colors.text_warning,
            None,
            theme,
        ),
    }
}

fn practice_accounts(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    theme: &AerisTheme,
) -> AnyElement {
    let trading = &app_state.trading_pnl;
    let selected = trading.order_entry.selected_account_id.as_ref();
    let practice = trading
        .accounts
        .iter()
        .enumerate()
        .filter(|(_, account)| account.environment == aeris_trading::AccountEnvironment::Simulated)
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    if practice.is_empty() && trading.account_creator.is_none() {
        rows.push(
            div()
                .py_2()
                .text_sm()
                .text_color(gpui_color(theme.colors.text_secondary))
                .child("No practice accounts yet.")
                .into_any_element(),
        );
    }
    for (index, account) in practice {
        if trading.account_delete_confirmation.as_ref() == Some(&account.id) {
            rows.push(delete_confirmation(app, account, theme));
            continue;
        }
        let detail = account.starting_equity.map_or_else(
            || "Simulated funds".to_string(),
            |equity| {
                format!(
                    "Started with {}",
                    super::order_ticket::format_money(&account.currency, equity)
                )
            },
        );
        rows.push(account_row(
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
        rows.extend(practice_account_form(app, creator, theme));
    } else {
        let open = app.clone();
        rows.push(described_setting_row(
            "New practice account",
            [SharedString::from(
                "Trade simulated funds on live prices, starting from any balance",
            )],
            Button::new("accounts_new_practice_account", theme)
                .variant(ButtonVariant::Default)
                .icon(header_icon(HugeIcon::Add))
                .label("New account")
                .on_click(move |_, window, cx| {
                    open.update(cx, |surface, surface_cx| {
                        open_practice_account_form(surface, window, surface_cx);
                    });
                })
                .into_any_element(),
            theme,
        ));
    }
    settings_group("Practice accounts", rows, theme)
}

/// Broker accounts registered by the trading owner. They are never deleted here, and a
/// live account cannot be chosen for orders. Accounts recorded by a venue this build no
/// longer routes stay in the store for history but are not offered here.
fn broker_accounts(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    theme: &AerisTheme,
) -> Option<AnyElement> {
    let trading = &app_state.trading_pnl;
    let selected = trading.order_entry.selected_account_id.as_ref();
    let rows = trading
        .accounts
        .iter()
        .enumerate()
        .filter(|(_, account)| {
            account.environment != aeris_trading::AccountEnvironment::Simulated
                && account.venue_id == aeris_trading::venue::BROKER_VENUE_ID
        })
        .map(|(index, account)| {
            let detail = match account.environment {
                aeris_trading::AccountEnvironment::Live => {
                    "Live account · trading is disabled".to_string()
                }
                _ if trading.connected_broker_accounts.contains(&account.id) => {
                    format!("Broker demo server · {}", account.currency)
                }
                _ => "Broker demo server · not connected".to_string(),
            };
            account_row(
                app,
                AccountRow {
                    index,
                    account,
                    detail,
                    selected: selected == Some(&account.id),
                    selectable: account.environment == aeris_trading::AccountEnvironment::Demo,
                },
                theme,
            )
        })
        .collect::<Vec<_>>();
    (!rows.is_empty()).then(|| settings_group("Broker accounts", rows, theme))
}

struct AccountRow<'a> {
    index: usize,
    account: &'a aeris_trading::TradingAccount,
    detail: String,
    selected: bool,
    /// Whether orders may be placed on it; live broker accounts are data-only.
    selectable: bool,
}

/// One account: its name and detail on the left; its environment, the Use control and, for
/// a practice account, Delete on the right.
fn account_row(
    app: &Entity<WorkspaceSurface>,
    row: AccountRow<'_>,
    theme: &AerisTheme,
) -> AnyElement {
    let AccountRow {
        index,
        account,
        detail,
        selected,
        selectable,
    } = row;
    let simulated = account.environment == aeris_trading::AccountEnvironment::Simulated;
    let select = app.clone();
    let account_id = account.id.clone();
    let use_button = selectable.then(|| {
        Button::new(("accounts_trading_use", index), theme)
            .variant(ButtonVariant::Secondary)
            .selected(selected)
            .icon(header_icon(if selected {
                HugeIcon::CheckIcon
            } else {
                HugeIcon::User
            }))
            .label(if selected { "Trading" } else { "Use" })
            .aria_label(format!("Trade on {}", account.display_name))
            .on_click(move |_, _, cx| {
                select.update(cx, |surface, surface_cx| {
                    surface.select_trading_account(account_id.clone(), surface_cx);
                });
            })
            .into_any_element()
    });
    // Only practice accounts can be deleted; broker accounts belong to the broker.
    let delete = simulated.then(|| delete_account_button(app, index, account, theme));
    described_setting_row(
        account.display_name.clone(),
        [SharedString::from(detail)],
        control_strip(
            std::iter::once(environment_badge(account.environment, theme))
                .chain(use_button)
                .chain(delete),
        ),
        theme,
    )
}

fn delete_account_button(
    app: &Entity<WorkspaceSurface>,
    index: usize,
    account: &aeris_trading::TradingAccount,
    theme: &AerisTheme,
) -> AnyElement {
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
        .into_any_element()
}

/// The account row turned into its delete confirmation, in place.
fn delete_confirmation(
    app: &Entity<WorkspaceSurface>,
    account: &aeris_trading::TradingAccount,
    theme: &AerisTheme,
) -> AnyElement {
    let cancel = app.clone();
    let confirm = app.clone();
    let account_key = account.id.as_str().to_string();
    described_setting_row(
        format!("Delete {}?", account.display_name),
        [SharedString::from(
            "Permanently removes its positions, working orders, fills, P/L history, risk state \
             and copier references. This cannot be undone.",
        )],
        control_strip([
            Button::new("accounts_delete_cancel", theme)
                .variant(ButtonVariant::Secondary)
                .label("Cancel")
                .on_click(move |_, _, cx| {
                    cancel.update(cx, |surface, surface_cx| {
                        surface.trading_pnl.account_delete_confirmation = None;
                        surface_cx.notify();
                    });
                })
                .into_any_element(),
            Button::new("accounts_delete_confirm", theme)
                .variant(ButtonVariant::Destructive)
                .label("Delete")
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
                })
                .into_any_element(),
        ]),
        theme,
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

/// A text field in an Accounts row's control slot.
pub(super) fn accounts_field(input: &Entity<InputState>, theme: &AerisTheme) -> AnyElement {
    div()
        .w(px(ACCOUNTS_FIELD_WIDTH))
        .h(px(32.0))
        .flex()
        .child(Input::new(input).platform(theme).flex_1())
        .into_any_element()
}

fn practice_account_form(
    app: &Entity<WorkspaceSurface>,
    creator: &PracticeAccountFormState,
    theme: &AerisTheme,
) -> [AnyElement; 3] {
    let cancel = app.clone();
    let create = app.clone();
    let name = creator.name.clone();
    let equity = creator.equity.clone();
    [
        described_setting_row(
            "Account name",
            [SharedString::from(
                "Shown in the header and the order ticket",
            )],
            accounts_field(&creator.name, theme),
            theme,
        ),
        described_setting_row(
            "Starting balance",
            [SharedString::from("Simulated US dollars")],
            accounts_field(&creator.equity, theme),
            theme,
        ),
        div()
            .pt_1()
            .pb_2()
            .flex()
            .justify_end()
            .gap_2()
            .child(
                Button::new("accounts_create_cancel", theme)
                    .variant(ButtonVariant::Secondary)
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
            )
            .into_any_element(),
    ]
}
