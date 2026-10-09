//! Bottom panel with trade-history and open-position tabs. Its header stays docked at the window
//! bottom while collapsed and rides the top edge of the panel while open; the open panel resizes
//! from that edge.
//!
//! The trading owner stays authoritative for fills, the round trips grouped from them and broker
//! positions. This module keeps one shared presentation copy of its bounded, newest-first
//! round-trip projection and open broker positions, replaced only when the owner's snapshot
//! revision changes, plus the viewer's tab, open, height and account-filter choices.

use super::side_panel_dock::SIDE_PANEL_HEADER_HEIGHT;
use super::*;
use gpui::{Stateful, UniformListScrollHandle, uniform_list};
use std::{collections::BTreeSet, sync::Arc};

const TRADE_HISTORY_INITIAL_HEIGHT: f32 = 260.0;
const TRADE_HISTORY_MINIMUM_HEIGHT: f32 = 140.0;
const TRADE_HISTORY_MAXIMUM_HEIGHT: f32 = 640.0;
/// Space a resize always leaves for the title bar, chart header and workspace above.
const TRADE_HISTORY_MINIMUM_WORKSPACE_HEIGHT: f32 = 240.0;
const TRADE_HISTORY_RESIZE_HANDLE_HEIGHT: f32 = 6.0;
/// Header controls stay inside the panel header with room around their hover fill.
const TRADE_HISTORY_HEADER_CONTROL_HEIGHT: f32 = 22.0;
const TRADE_HISTORY_ROW_HEIGHT: f32 = 28.0;
const TRADE_HISTORY_TIME_WIDTH: f32 = 112.0;
const TRADE_HISTORY_ACCOUNT_WIDTH: f32 = 85.0;
const TRADE_HISTORY_SYMBOL_WIDTH: f32 = 65.0;
const TRADE_HISTORY_SIDE_WIDTH: f32 = 40.0;
const TRADE_HISTORY_QUANTITY_WIDTH: f32 = 50.0;
const TRADE_HISTORY_PRICE_WIDTH: f32 = 72.0;
const TRADE_HISTORY_PNL_WIDTH: f32 = 95.0;
/// Entry, stop loss and take profit share one narrower width so the positions tab fits the
/// same compact window as the trade history.
const POSITION_PRICE_WIDTH: f32 = 64.0;
const POSITION_ACTION_WIDTH: f32 = 56.0;
const TRADE_HISTORY_CELL_GAP: f32 = 8.0;
const TRADE_HISTORY_ACCOUNT_MENU_WIDTH: f32 = 220.0;
const TRADE_HISTORY_ACCOUNT_MENU_MAX_HEIGHT: f32 = 180.0;

/// Drag payload for resizing the trade-history panel from its top edge.
#[derive(Clone)]
struct TradeHistoryHeightDrag;

impl Render for TradeHistoryHeightDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

/// Which account's executed trades the history shows. A single account is named by its
/// trading-owner account key, the same key the order ticket passes to trading commands.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) enum TradeHistoryAccountFilter {
    #[default]
    All,
    Account(String),
}

/// Shared presentation copy of the trading owner's executed round trips.
#[derive(Clone, Default)]
pub(super) struct TradeHistory {
    revision: Option<u64>,
    /// Newest activity first, exactly as the owner's bounded snapshot orders them.
    round_trips: Arc<[aeris_trading_runtime::TradeRoundTrip]>,
    accounts: Arc<[aeris_trading::TradingAccount]>,
    symbols: Arc<BTreeMap<aeris_instruments::InstrumentId, String>>,
    /// Indices into `round_trips` that pass the current account filter.
    rows: Arc<[usize]>,
}

impl TradeHistory {
    fn account_currency(&self, account_key: &str) -> &str {
        self.accounts
            .iter()
            .find(|account| account.id.as_str() == account_key)
            .map_or("", |account| account.currency.as_str())
    }

    fn account_name(&self, account_key: &str) -> String {
        self.accounts
            .iter()
            .find(|account| account.id.as_str() == account_key)
            .map_or_else(
                || account_key.to_string(),
                |account| account.display_name.clone(),
            )
    }

    fn symbol(&self, instrument_id: &aeris_instruments::InstrumentId) -> String {
        self.symbols
            .get(instrument_id)
            .cloned()
            .unwrap_or_else(|| instrument_id.as_str().to_string())
    }
}

/// Which list the open bottom panel shows.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum BottomPanelTab {
    #[default]
    TradeHistory,
    Positions,
}

/// Shared presentation copy of the trading owner's open broker positions.
#[derive(Clone, Default)]
struct OpenPositions {
    /// Newest opened first.
    positions: Arc<[aeris_trading::BrokerPosition]>,
    /// Broker accounts the venue reaches now; only their open P&L is current.
    connected: Arc<BTreeSet<aeris_trading::TradingAccountId>>,
    /// Indices into `positions` that pass the current account filter.
    rows: Arc<[usize]>,
}

/// Viewer state of the bottom panel. It owns presentation only, never trading state.
pub(super) struct BottomPanelState {
    pub(super) open: bool,
    pub(super) tab: BottomPanelTab,
    pub(super) account_menu_open: bool,
    /// Height of the open panel, including its header.
    height: f32,
    filter: TradeHistoryAccountFilter,
    history: TradeHistory,
    positions: OpenPositions,
    scroll: UniformListScrollHandle,
    positions_scroll: UniformListScrollHandle,
}

impl Default for BottomPanelState {
    fn default() -> Self {
        Self {
            open: false,
            tab: BottomPanelTab::TradeHistory,
            account_menu_open: false,
            height: TRADE_HISTORY_INITIAL_HEIGHT,
            filter: TradeHistoryAccountFilter::All,
            history: TradeHistory::default(),
            positions: OpenPositions::default(),
            scroll: UniformListScrollHandle::new(),
            positions_scroll: UniformListScrollHandle::new(),
        }
    }
}

impl BottomPanelState {
    /// Adopts a newer owner snapshot. Returns whether the visible history changed.
    pub(super) fn apply_snapshot(
        &mut self,
        snapshot: &aeris_trading_runtime::TradingSnapshot,
    ) -> bool {
        if self.history.revision == Some(snapshot.revision) {
            return false;
        }
        self.history.revision = Some(snapshot.revision);
        self.history.round_trips = snapshot.round_trips.clone().into();
        self.history.accounts = snapshot.accounts.clone().into();
        self.history.symbols = Arc::new(
            snapshot
                .instruments
                .iter()
                .map(|instrument| {
                    (
                        instrument.instrument_id.clone(),
                        instrument.contract.provenance.display_symbol.clone(),
                    )
                })
                .collect(),
        );
        self.adopt_positions(
            &snapshot.broker_positions,
            &snapshot.connected_broker_accounts,
        );
        true
    }

    /// Replaces the open-position copy, newest opened first, and re-filters both lists.
    fn adopt_positions(
        &mut self,
        positions: &[aeris_trading::BrokerPosition],
        connected: &BTreeSet<aeris_trading::TradingAccountId>,
    ) {
        let mut positions = positions.to_vec();
        positions.sort_by_key(|position| std::cmp::Reverse(position.opened_unix_nanos));
        self.positions.positions = positions.into();
        self.positions.connected = Arc::new(connected.clone());
        self.refresh_rows();
    }

    /// Applies a bounded panel height. Returns whether it changed.
    fn set_height(&mut self, height: f32) -> bool {
        let height = height.clamp(TRADE_HISTORY_MINIMUM_HEIGHT, TRADE_HISTORY_MAXIMUM_HEIGHT);
        if (height - self.height).abs() < f32::EPSILON {
            return false;
        }
        self.height = height;
        true
    }

    /// Opens the panel on `tab`; selecting the tab already shown collapses the panel.
    fn select_tab(&mut self, tab: BottomPanelTab) {
        self.open = !(self.open && self.tab == tab);
        self.tab = tab;
        self.account_menu_open = false;
    }

    pub(super) fn set_filter(&mut self, filter: TradeHistoryAccountFilter) {
        self.filter = filter;
        self.account_menu_open = false;
        self.refresh_rows();
    }

    /// A filtered account that no longer exists falls back to every account instead of
    /// leaving an empty history for an account the viewer can no longer select.
    fn refresh_rows(&mut self) {
        if let TradeHistoryAccountFilter::Account(account_key) = &self.filter
            && !self
                .history
                .accounts
                .iter()
                .any(|account| account.id.as_str() == account_key)
        {
            self.filter = TradeHistoryAccountFilter::All;
        }
        self.history.rows = trade_history_rows(&self.history.round_trips, &self.filter).into();
        self.positions.rows = position_rows(&self.positions.positions, &self.filter).into();
    }

    fn shows_account_column(&self) -> bool {
        self.filter == TradeHistoryAccountFilter::All && self.history.accounts.len() > 1
    }

    fn filter_label(&self) -> String {
        match &self.filter {
            TradeHistoryAccountFilter::All => "All accounts".to_string(),
            TradeHistoryAccountFilter::Account(account_key) => {
                self.history.account_name(account_key)
            }
        }
    }
}

fn trade_history_rows(
    round_trips: &[aeris_trading_runtime::TradeRoundTrip],
    filter: &TradeHistoryAccountFilter,
) -> Vec<usize> {
    round_trips
        .iter()
        .enumerate()
        .filter(|(_, trip)| match filter {
            TradeHistoryAccountFilter::All => true,
            TradeHistoryAccountFilter::Account(account_key) => {
                trip.account_id.as_str() == account_key
            }
        })
        .map(|(index, _)| index)
        .collect()
}

fn position_rows(
    positions: &[aeris_trading::BrokerPosition],
    filter: &TradeHistoryAccountFilter,
) -> Vec<usize> {
    positions
        .iter()
        .enumerate()
        .filter(|(_, position)| match filter {
            TradeHistoryAccountFilter::All => true,
            TradeHistoryAccountFilter::Account(account_key) => {
                position.account_id.as_str() == account_key
            }
        })
        .map(|(index, _)| index)
        .collect()
}

impl TerminalApp {
    fn toggle_bottom_panel(&mut self, cx: &mut Context<Self>) {
        self.bottom_panel.open = !self.bottom_panel.open;
        self.bottom_panel.account_menu_open = false;
        cx.notify();
    }

    fn select_bottom_panel_tab(&mut self, tab: BottomPanelTab, cx: &mut Context<Self>) {
        self.bottom_panel.select_tab(tab);
        cx.notify();
    }

    fn set_trade_history_height(&mut self, height: f32, cx: &mut Context<Self>) {
        if self.bottom_panel.set_height(height) {
            cx.notify();
        }
    }

    fn toggle_trade_history_account_menu(&mut self, cx: &mut Context<Self>) {
        self.bottom_panel.account_menu_open = !self.bottom_panel.account_menu_open;
        cx.notify();
    }

    fn close_trade_history_account_menu(&mut self, cx: &mut Context<Self>) {
        if self.bottom_panel.account_menu_open {
            self.bottom_panel.account_menu_open = false;
            cx.notify();
        }
    }

    fn set_trade_history_filter(
        &mut self,
        filter: TradeHistoryAccountFilter,
        cx: &mut Context<Self>,
    ) {
        self.bottom_panel.set_filter(filter);
        cx.notify();
    }
}

/// Inputs of the bottom panel for one frame.
pub(super) struct BottomPanelView<'a> {
    pub(super) terminal: &'a Entity<TerminalApp>,
    pub(super) state: &'a BottomPanelState,
    /// The active chart formats trade times in its selected zone.
    pub(super) chart: Option<Entity<AerisChartView>>,
    /// The active pane holds a Rithmic session, so its attribution marks show.
    pub(super) rithmic_attribution: bool,
}

pub(super) fn bottom_panel(
    view: BottomPanelView<'_>,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let BottomPanelView {
        terminal,
        state,
        chart,
        rithmic_attribution,
    } = view;
    let open = state.open;
    let resize_terminal = terminal.clone();
    div()
        .id("bottom_panel")
        .relative()
        .flex_none()
        .w_full()
        .when(open, |panel| panel.h(px(state.height)))
        .flex()
        .flex_col()
        .border_t(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface))
        .child(trade_history_header(
            terminal,
            state,
            rithmic_attribution,
            theme,
        ))
        .when(open, |panel| match state.tab {
            BottomPanelTab::TradeHistory => panel
                .child(trade_history_column_header(
                    state.shows_account_column(),
                    theme,
                ))
                .child(trade_history_list(state, chart, theme))
                .child(trade_history_resize_handle()),
            BottomPanelTab::Positions => panel
                .child(position_column_header(state.shows_account_column(), theme))
                .child(position_list(state, chart, theme))
                .child(trade_history_resize_handle()),
        })
        .on_drag_move::<TradeHistoryHeightDrag>(move |event, window, cx| {
            // The panel's bottom edge is the window's bottom edge, so the pointer's distance
            // from it is the requested height.
            let bottom = f32::from(event.bounds.bottom());
            let available =
                f32::from(window.viewport_size().height) - TRADE_HISTORY_MINIMUM_WORKSPACE_HEIGHT;
            let height = (bottom - f32::from(event.event.position.y)).min(available);
            resize_terminal.update(cx, |terminal, terminal_cx| {
                terminal.set_trade_history_height(height, terminal_cx);
            });
        })
}

/// Top-edge handle; the panel owns the drag and converts it to a height.
fn trade_history_resize_handle() -> impl IntoElement {
    div()
        .id("trade_history_resize")
        .absolute()
        .occlude()
        .top_0()
        .left_0()
        .w_full()
        .h(px(TRADE_HISTORY_RESIZE_HANDLE_HEIGHT))
        .cursor_row_resize()
        .on_drag(TradeHistoryHeightDrag, |drag, _, _, cx| {
            cx.new(|_| drag.clone())
        })
}

fn trade_history_header(
    terminal: &Entity<TerminalApp>,
    state: &BottomPanelState,
    rithmic_attribution: bool,
    theme: &AerisTheme,
) -> Div {
    let colors = theme.colors;
    let open = state.open;
    let count_label = match state.tab {
        BottomPanelTab::TradeHistory => {
            let count = state.history.rows.len();
            format!("{count} {}", if count == 1 { "trade" } else { "trades" })
        }
        BottomPanelTab::Positions => {
            let count = state.positions.rows.len();
            format!(
                "{count} {}",
                if count == 1 { "position" } else { "positions" }
            )
        }
    };
    div()
        .h(px(SIDE_PANEL_HEADER_HEIGHT))
        .flex_none()
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .when(open, |header| {
            header
                .border_b(px(theme.dimensions.border_width))
                .border_color(gpui_color(colors.border))
        })
        .text_xs()
        .text_color(gpui_color(colors.text_secondary))
        .child(bottom_panel_tab(
            terminal,
            state,
            BottomPanelTab::TradeHistory,
            theme,
        ))
        .child(bottom_panel_tab(
            terminal,
            state,
            BottomPanelTab::Positions,
            theme,
        ))
        .when(open && state.history.accounts.len() > 1, |header| {
            header.child(trade_history_account_filter(terminal, state, theme))
        })
        .when(open, |header| header.child(count_label))
        .child(div().flex_1())
        .when(rithmic_attribution, |header| {
            header.child(super::terminal_chrome::rithmic_attribution(theme))
        })
        .child(trade_history_expand_button(terminal, open, theme))
}

fn bottom_panel_tab(
    terminal: &Entity<TerminalApp>,
    state: &BottomPanelState,
    tab: BottomPanelTab,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let select_terminal = terminal.clone();
    let active = state.open && state.tab == tab;
    let (id, label, show, hide) = match tab {
        BottomPanelTab::TradeHistory => (
            "trade_history_tab",
            "TRADE HISTORY",
            "Show trade history",
            "Hide trade history",
        ),
        BottomPanelTab::Positions => (
            "positions_tab",
            "POSITIONS",
            "Show open positions",
            "Hide open positions",
        ),
    };
    div()
        .id(id)
        .h(px(TRADE_HISTORY_HEADER_CONTROL_HEIGHT))
        .flex_none()
        .px_2()
        .flex()
        .items_center()
        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
        .role(Role::Button)
        .aria_label(if active { hide } else { show })
        .cursor_pointer()
        .text_color(gpui_color(if active {
            colors.text_active
        } else {
            colors.text_interactive
        }))
        .when(active, |tab| {
            tab.bg(gpui_color(colors.active_bg.over(colors.surface)))
        })
        .hover(move |tab| {
            tab.bg(gpui_color(colors.hover_bg.over(colors.surface)))
                .text_color(gpui_color(colors.text_hover))
        })
        .child(label)
        .on_click(move |_, _, cx| {
            select_terminal.update(cx, |terminal, terminal_cx| {
                terminal.select_bottom_panel_tab(tab, terminal_cx);
            });
        })
}

fn trade_history_expand_button(
    terminal: &Entity<TerminalApp>,
    open: bool,
    theme: &AerisTheme,
) -> impl IntoElement {
    let toggle_terminal = terminal.clone();
    let label = if open {
        "Collapse bottom panel"
    } else {
        "Expand bottom panel"
    };
    let chevron = header_icon(HugeIcon::ChevronDown);
    let chevron = if open { chevron } else { chevron.rotate(0.5) };
    chrome_tooltip(
        "trade_history_expand",
        label,
        Button::new("trade_history_expand", theme)
            .button_size(ButtonSize::Sm)
            .round()
            .icon(chevron)
            .aria_label(label)
            .on_click(move |_, _, cx| {
                toggle_terminal.update(cx, TerminalApp::toggle_bottom_panel);
            }),
        theme,
    )
}

fn trade_history_account_filter(
    terminal: &Entity<TerminalApp>,
    state: &BottomPanelState,
    theme: &AerisTheme,
) -> impl IntoElement {
    let toggle_terminal = terminal.clone();
    let dismiss_terminal = terminal.clone();
    let trigger = Button::new("trade_history_account_filter", theme)
        .variant(ButtonVariant::Secondary)
        .button_size(ButtonSize::Xs)
        .open(state.account_menu_open)
        .aria_label("Filter trades by account")
        .label(state.filter_label())
        .caret(header_icon(HugeIcon::ChevronDown))
        .on_click(move |_, _, cx| {
            toggle_terminal.update(cx, TerminalApp::toggle_trade_history_account_menu);
        });
    MenuAnchor::new("trade_history_account_filter_anchor", trigger)
        .menu(
            state
                .account_menu_open
                .then(|| trade_history_account_menu(terminal, state, theme)),
        )
        .on_dismiss(move |_, cx| {
            dismiss_terminal.update(cx, TerminalApp::close_trade_history_account_menu);
        })
}

/// Opens upward over the workspace so a short panel never clips it at the window bottom.
fn trade_history_account_menu(
    terminal: &Entity<TerminalApp>,
    state: &BottomPanelState,
    theme: &AerisTheme,
) -> MenuPanel {
    let all_terminal = terminal.clone();
    let mut menu = MenuPanel::new(
        "trade_history_account_menu",
        MenuPlacement::Anchored {
            side: MenuSide::Above,
            align: MenuAlign::Start,
        },
        theme,
    )
    .width(px(TRADE_HISTORY_ACCOUNT_MENU_WIDTH))
    .max_height(px(TRADE_HISTORY_ACCOUNT_MENU_MAX_HEIGHT))
    .child(
        MenuRow::compact("trade_history_account_all", "All accounts", theme)
            .checked(state.filter == TradeHistoryAccountFilter::All)
            .on_click(move |_, _, cx| {
                all_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.set_trade_history_filter(TradeHistoryAccountFilter::All, terminal_cx);
                });
            }),
    );
    for (index, account) in state.history.accounts.iter().enumerate() {
        let select_terminal = terminal.clone();
        let filter = TradeHistoryAccountFilter::Account(account.id.as_str().to_string());
        let selected = state.filter == filter;
        menu = menu.child(
            MenuRow::compact(
                ("trade_history_account", index),
                account.display_name.clone(),
                theme,
            )
            .checked(selected)
            .on_click(move |_, _, cx| {
                let filter = filter.clone();
                select_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.set_trade_history_filter(filter, terminal_cx);
                });
            }),
        );
    }
    menu
}

fn trade_history_column_header(show_account: bool, theme: &AerisTheme) -> impl IntoElement {
    let colors = theme.colors;
    trade_history_row_frame()
        .border_b(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border_subtle))
        .text_color(gpui_color(colors.text_secondary))
        .child(trade_history_cell(TRADE_HISTORY_TIME_WIDTH).child("Opened"))
        .child(trade_history_cell(TRADE_HISTORY_TIME_WIDTH).child("Closed"))
        .when(show_account, |row| {
            row.child(trade_history_cell(TRADE_HISTORY_ACCOUNT_WIDTH).child("Account"))
        })
        .child(trade_history_cell(TRADE_HISTORY_SYMBOL_WIDTH).child("Symbol"))
        .child(trade_history_cell(TRADE_HISTORY_SIDE_WIDTH).child("Side"))
        .child(
            trade_history_cell(TRADE_HISTORY_QUANTITY_WIDTH)
                .text_right()
                .child("Qty"),
        )
        .child(
            trade_history_cell(TRADE_HISTORY_PRICE_WIDTH)
                .text_right()
                .child("Entry"),
        )
        .child(
            trade_history_cell(TRADE_HISTORY_PRICE_WIDTH)
                .text_right()
                .child("Exit"),
        )
        .child(
            trade_history_cell(TRADE_HISTORY_PNL_WIDTH)
                .text_right()
                .child("P&L"),
        )
}

fn trade_history_list(
    state: &BottomPanelState,
    chart: Option<Entity<AerisChartView>>,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    if state.history.rows.is_empty() {
        let message = if state.history.round_trips.is_empty() {
            "No executed trades yet"
        } else {
            "No executed trades for this account"
        };
        return div()
            .flex_1()
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(gpui_color(colors.text_secondary))
            .child(message)
            .into_any_element();
    }
    let history = state.history.clone();
    let show_account = state.shows_account_column();
    let list_theme = *theme;
    uniform_list(
        "trade_history_rows",
        history.rows.len(),
        move |range, _, cx| {
            let chart = chart.as_ref().map(|chart| chart.read(cx));
            range
                .filter_map(|index| {
                    let trip = history.round_trips.get(*history.rows.get(index)?)?;
                    Some(trade_history_row(
                        index,
                        trip,
                        &history,
                        chart,
                        show_account,
                        &list_theme,
                    ))
                })
                .collect::<Vec<_>>()
        },
    )
    .track_scroll(&state.scroll)
    .w_full()
    .flex_1()
    .min_h_0()
    .into_any_element()
}

fn trade_history_row(
    index: usize,
    trip: &aeris_trading_runtime::TradeRoundTrip,
    history: &TradeHistory,
    chart: Option<&AerisChartView>,
    show_account: bool,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let opened = trip
        .entry
        .map(|entry| trade_history_time(chart, entry.first_unix_nanos));
    let closed = if trip.closed {
        trip.exit
            .map(|exit| trade_history_time(chart, exit.last_unix_nanos))
    } else {
        Some("Open".to_string())
    };
    let (side, side_color) = match trip.side {
        aeris_trading::OrderSide::Buy => ("Long", colors.text_positive),
        aeris_trading::OrderSide::Sell => ("Short", colors.text_negative),
    };
    // A trade that opened before the retained history still reports the size it closed.
    let quantity = trip
        .entry
        .or(trip.exit)
        .map(|leg| trade_history_decimal(leg.quantity));
    let entry_price = trip
        .entry
        .map(|leg| trade_history_decimal(leg.average_price));
    let exit_price = trip
        .exit
        .map(|leg| trade_history_decimal(leg.average_price));
    let currency = history.account_currency(trip.account_id.as_str());
    trade_history_row_frame()
        .id(("trade_history_row", index))
        .hover(move |row| row.bg(gpui_color(colors.hover_bg)))
        .text_color(gpui_color(colors.text_default))
        .child(trade_history_cell(TRADE_HISTORY_TIME_WIDTH).child(trade_history_text(opened)))
        .child(trade_history_cell(TRADE_HISTORY_TIME_WIDTH).child(trade_history_text(closed)))
        .when(show_account, |row| {
            row.child(
                trade_history_cell(TRADE_HISTORY_ACCOUNT_WIDTH)
                    .child(history.account_name(trip.account_id.as_str())),
            )
        })
        .child(
            trade_history_cell(TRADE_HISTORY_SYMBOL_WIDTH)
                .child(history.symbol(&trip.instrument_id)),
        )
        .child(
            trade_history_cell(TRADE_HISTORY_SIDE_WIDTH)
                .text_color(gpui_color(side_color))
                .child(side),
        )
        .child(
            trade_history_cell(TRADE_HISTORY_QUANTITY_WIDTH)
                .text_right()
                .child(trade_history_text(quantity)),
        )
        .child(
            trade_history_cell(TRADE_HISTORY_PRICE_WIDTH)
                .text_right()
                .child(trade_history_text(entry_price)),
        )
        .child(
            trade_history_cell(TRADE_HISTORY_PRICE_WIDTH)
                .text_right()
                .child(trade_history_text(exit_price)),
        )
        .child(trade_history_pnl_cell(trip.final_pnl, currency, theme))
}

fn position_column_header(show_account: bool, theme: &AerisTheme) -> impl IntoElement {
    let colors = theme.colors;
    let right =
        |width: f32, label: &'static str| trade_history_cell(width).text_right().child(label);
    trade_history_row_frame()
        .border_b(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border_subtle))
        .text_color(gpui_color(colors.text_secondary))
        .child(trade_history_cell(TRADE_HISTORY_TIME_WIDTH).child("Opened"))
        .when(show_account, |row| {
            row.child(trade_history_cell(TRADE_HISTORY_ACCOUNT_WIDTH).child("Account"))
        })
        .child(trade_history_cell(TRADE_HISTORY_SYMBOL_WIDTH).child("Symbol"))
        .child(trade_history_cell(TRADE_HISTORY_SIDE_WIDTH).child("Side"))
        .child(right(TRADE_HISTORY_QUANTITY_WIDTH, "Qty"))
        .child(right(POSITION_PRICE_WIDTH, "Entry"))
        .child(right(POSITION_PRICE_WIDTH, "SL"))
        .child(right(POSITION_PRICE_WIDTH, "TP"))
        .child(right(TRADE_HISTORY_PNL_WIDTH, "Open P&L"))
        .child(div().w(px(POSITION_ACTION_WIDTH)).flex_none())
}

fn position_list(
    state: &BottomPanelState,
    chart: Option<Entity<AerisChartView>>,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    if state.positions.rows.is_empty() {
        let message = if state.positions.positions.is_empty() {
            "No open broker positions"
        } else {
            "No open broker positions for this account"
        };
        return div()
            .flex_1()
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(gpui_color(colors.text_secondary))
            .child(message)
            .into_any_element();
    }
    let positions = state.positions.clone();
    let history = state.history.clone();
    let show_account = state.shows_account_column();
    let list_theme = *theme;
    uniform_list(
        "position_rows",
        positions.rows.len(),
        move |range, _, cx| {
            let chart = chart.as_ref().map(|chart| chart.read(cx));
            range
                .filter_map(|index| {
                    let position = positions.positions.get(*positions.rows.get(index)?)?;
                    Some(position_row(
                        index,
                        position,
                        positions.connected.contains(&position.account_id),
                        &history,
                        chart,
                        show_account,
                        &list_theme,
                    ))
                })
                .collect::<Vec<_>>()
        },
    )
    .track_scroll(&state.positions_scroll)
    .w_full()
    .flex_1()
    .min_h_0()
    .into_any_element()
}

/// One open broker position. Its open P&L is the broker's own value and shows only while the
/// venue reaches the account; closing it is asynchronous, so the row stays until the broker
/// reports the position closed.
fn position_row(
    index: usize,
    position: &aeris_trading::BrokerPosition,
    connected: bool,
    history: &TradeHistory,
    chart: Option<&AerisChartView>,
    show_account: bool,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let (side, side_color) = match position.side {
        aeris_trading::OrderSide::Buy => ("Long", colors.text_positive),
        aeris_trading::OrderSide::Sell => ("Short", colors.text_negative),
    };
    let price = |value: Option<aeris_trading::FixedPoint>| {
        trade_history_text(value.map(trade_history_decimal))
    };
    let right = |width: f32, text: String| trade_history_cell(width).text_right().child(text);
    let account_key = position.account_id.as_str().to_string();
    let broker_position_id = position.broker_position_id.clone();
    trade_history_row_frame()
        .id(("position_row", index))
        .hover(move |row| row.bg(gpui_color(colors.hover_bg)))
        .text_color(gpui_color(colors.text_default))
        .child(
            trade_history_cell(TRADE_HISTORY_TIME_WIDTH)
                .child(trade_history_time(chart, position.opened_unix_nanos)),
        )
        .when(show_account, |row| {
            row.child(
                trade_history_cell(TRADE_HISTORY_ACCOUNT_WIDTH)
                    .child(history.account_name(position.account_id.as_str())),
            )
        })
        .child(
            trade_history_cell(TRADE_HISTORY_SYMBOL_WIDTH)
                .child(history.symbol(&position.instrument_id)),
        )
        .child(
            trade_history_cell(TRADE_HISTORY_SIDE_WIDTH)
                .text_color(gpui_color(side_color))
                .child(side),
        )
        .child(right(
            TRADE_HISTORY_QUANTITY_WIDTH,
            trade_history_decimal(position.quantity),
        ))
        .child(right(
            POSITION_PRICE_WIDTH,
            trade_history_decimal(position.entry_price),
        ))
        .child(right(POSITION_PRICE_WIDTH, price(position.stop_loss)))
        .child(right(POSITION_PRICE_WIDTH, price(position.take_profit)))
        .child(trade_history_pnl_cell(
            connected.then_some(position.net_unrealized),
            history.account_currency(position.account_id.as_str()),
            theme,
        ))
        .child(
            div().w(px(POSITION_ACTION_WIDTH)).flex_none().child(
                Button::new(("position_close", index), theme)
                    .variant(ButtonVariant::Secondary)
                    .button_size(ButtonSize::Sm)
                    .label("Close")
                    .disabled(!connected)
                    .on_click(move |_, _, cx| {
                        aeris_desktop::trading::close_broker_position(
                            account_key.clone(),
                            broker_position_id.clone(),
                            cx,
                        );
                    }),
            ),
        )
}

fn trade_history_time(chart: Option<&AerisChartView>, unix_nanos: i64) -> String {
    chart.map_or_else(
        || "—".to_string(),
        |chart| chart.time_zone_date_time_label_at(unix_nanos.div_euclid(1_000_000_000)),
    )
}

fn trade_history_decimal(value: aeris_trading::FixedPoint) -> String {
    market_price_text(value.units(), u32::from(value.scale()))
}

fn trade_history_text(value: Option<String>) -> String {
    value.unwrap_or_else(|| "—".to_string())
}

/// A signed P&L amount colored by its sign; a trade without a final result shows a dash.
fn trade_history_pnl_cell(
    pnl: Option<aeris_trading::FixedPoint>,
    currency: &str,
    theme: &AerisTheme,
) -> Div {
    let colors = theme.colors;
    let color = pnl.map_or(colors.text_secondary, |pnl| match pnl.units().cmp(&0) {
        std::cmp::Ordering::Less => colors.text_negative,
        std::cmp::Ordering::Equal => colors.text_secondary,
        std::cmp::Ordering::Greater => colors.text_positive,
    });
    trade_history_cell(TRADE_HISTORY_PNL_WIDTH)
        .text_right()
        .text_color(gpui_color(color))
        .child(trade_history_pnl_text(pnl, currency))
}

fn trade_history_pnl_text(pnl: Option<aeris_trading::FixedPoint>, currency: &str) -> String {
    pnl.map_or_else(
        || "—".to_string(),
        |pnl| {
            format!(
                "{currency} {}",
                market_price_text(pnl.units(), u32::from(pnl.scale()))
            )
        },
    )
}

/// Rows span the full panel width; a virtualized list otherwise sizes rows to their content.
fn trade_history_row_frame() -> Div {
    div()
        .w_full()
        .h(px(TRADE_HISTORY_ROW_HEIGHT))
        .flex_none()
        .px_2()
        .flex()
        .items_center()
        .gap(px(TRADE_HISTORY_CELL_GAP))
        .text_xs()
        .font_features(platform_tabular_numerals())
}

fn trade_history_cell(width: f32) -> Div {
    div().min_w(px(width)).flex_1().truncate()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(account: &str) -> aeris_trading_runtime::TradeRoundTrip {
        let leg = aeris_trading_runtime::TradeLeg {
            first_unix_nanos: 1_790_704_166_986_188_900,
            last_unix_nanos: 1_790_704_166_986_188_900,
            average_price: aeris_trading::FixedPoint::try_new(8_309_000_000_000, 8).expect("price"),
            quantity: aeris_trading::FixedPoint::try_new(100_000_000, 8).expect("quantity"),
        };
        aeris_trading_runtime::TradeRoundTrip {
            account_id: aeris_trading::TradingAccountId::try_new(account).expect("account"),
            instrument_id: aeris_instruments::InstrumentId::try_new("hyperliquid:perp:BTC")
                .expect("instrument"),
            side: aeris_trading::OrderSide::Buy,
            entry: Some(leg),
            exit: Some(leg),
            closed: true,
            final_pnl: Some(aeris_trading::FixedPoint::try_new(0, 2).expect("pnl")),
        }
    }

    #[test]
    fn account_filter_keeps_newest_first_order_and_selects_one_account() {
        let trips = [round_trip("b"), round_trip("a"), round_trip("b")];
        assert_eq!(
            trade_history_rows(&trips, &TradeHistoryAccountFilter::All),
            vec![0, 1, 2]
        );
        let only_b = TradeHistoryAccountFilter::Account("b".to_string());
        assert_eq!(trade_history_rows(&trips, &only_b), vec![0, 2]);
    }

    #[test]
    fn filter_for_a_deleted_account_falls_back_to_all_accounts() {
        let mut state = BottomPanelState::default();
        state.history.round_trips = vec![round_trip("a"), round_trip("b")].into();
        state.history.accounts = Arc::from(Vec::new());
        state.set_filter(TradeHistoryAccountFilter::Account("a".to_string()));
        assert_eq!(state.filter, TradeHistoryAccountFilter::All);
        assert_eq!(&*state.history.rows, &[0, 1]);
    }

    #[test]
    fn panel_height_stays_within_its_bounds() {
        let mut state = BottomPanelState::default();
        assert!(state.set_height(10.0));
        assert!((state.height - TRADE_HISTORY_MINIMUM_HEIGHT).abs() < f32::EPSILON);
        assert!(state.set_height(10_000.0));
        assert!((state.height - TRADE_HISTORY_MAXIMUM_HEIGHT).abs() < f32::EPSILON);
        assert!(
            !state.set_height(10_000.0),
            "an unchanged height does not re-render"
        );
    }

    #[test]
    fn final_pnl_shows_its_sign_and_an_unknown_result_a_dash() {
        let loss = aeris_trading::FixedPoint::try_new(-252_500, 2).expect("loss");
        let gain = aeris_trading::FixedPoint::try_new(735_000, 2).expect("gain");
        assert_eq!(trade_history_pnl_text(Some(loss), "USD"), "USD -2525.00");
        assert_eq!(trade_history_pnl_text(Some(gain), "USD"), "USD 7350.00");
        assert_eq!(trade_history_pnl_text(None, "USD"), "—");
    }

    fn broker_position(account: &str, id: &str, opened: i64) -> aeris_trading::BrokerPosition {
        let money = |units| aeris_trading::FixedPoint::try_new(units, 2).expect("money");
        aeris_trading::BrokerPosition {
            account_id: aeris_trading::TradingAccountId::try_new(account).expect("account"),
            instrument_id: aeris_instruments::InstrumentId::try_new("ctrader:demo:1001:1")
                .expect("instrument"),
            broker_position_id: id.into(),
            side: aeris_trading::OrderSide::Buy,
            quantity: money(100_000),
            entry_price: aeris_trading::FixedPoint::try_new(112_142, 5).expect("entry"),
            stop_loss: None,
            take_profit: None,
            swap: money(0),
            commission: money(-5),
            gross_unrealized: money(-1),
            net_unrealized: money(-6),
            opened_unix_nanos: opened,
        }
    }

    #[test]
    fn positions_follow_the_account_filter_newest_opened_first() {
        let mut state = BottomPanelState::default();
        state.adopt_positions(
            &[
                broker_position("a", "1", 10),
                broker_position("b", "2", 30),
                broker_position("a", "3", 20),
            ],
            &BTreeSet::new(),
        );
        let ids = |state: &BottomPanelState| {
            state
                .positions
                .rows
                .iter()
                .map(|row| state.positions.positions[*row].broker_position_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&state), ["2", "3", "1"]);
        assert_eq!(
            position_rows(
                &state.positions.positions,
                &TradeHistoryAccountFilter::Account("a".to_string())
            ),
            vec![1, 2]
        );
    }

    #[test]
    fn selecting_the_shown_tab_collapses_and_another_tab_switches() {
        let mut state = BottomPanelState::default();
        state.select_tab(BottomPanelTab::Positions);
        assert!(state.open);
        assert_eq!(state.tab, BottomPanelTab::Positions);
        state.select_tab(BottomPanelTab::TradeHistory);
        assert!(state.open, "switching tabs keeps the panel open");
        assert_eq!(state.tab, BottomPanelTab::TradeHistory);
        state.select_tab(BottomPanelTab::TradeHistory);
        assert!(!state.open);
    }

    #[test]
    fn all_position_columns_fit_a_compact_window() {
        let columns = [
            TRADE_HISTORY_TIME_WIDTH,
            TRADE_HISTORY_ACCOUNT_WIDTH,
            TRADE_HISTORY_SYMBOL_WIDTH,
            TRADE_HISTORY_SIDE_WIDTH,
            TRADE_HISTORY_QUANTITY_WIDTH,
            POSITION_PRICE_WIDTH,
            POSITION_PRICE_WIDTH,
            POSITION_PRICE_WIDTH,
            TRADE_HISTORY_PNL_WIDTH,
            POSITION_ACTION_WIDTH,
        ];
        let width = columns.iter().sum::<f32>()
            + TRADE_HISTORY_CELL_GAP
                * f32::from(u8::try_from(columns.len() - 1).expect("column count fits"))
            + 16.0;
        assert!(
            width <= 800.0 - 12.0,
            "the Close action must remain visible"
        );
    }

    #[test]
    fn all_trade_history_columns_fit_a_compact_window() {
        let columns = [
            TRADE_HISTORY_TIME_WIDTH,
            TRADE_HISTORY_TIME_WIDTH,
            TRADE_HISTORY_ACCOUNT_WIDTH,
            TRADE_HISTORY_SYMBOL_WIDTH,
            TRADE_HISTORY_SIDE_WIDTH,
            TRADE_HISTORY_QUANTITY_WIDTH,
            TRADE_HISTORY_PRICE_WIDTH,
            TRADE_HISTORY_PRICE_WIDTH,
            TRADE_HISTORY_PNL_WIDTH,
        ];
        let width = columns.iter().sum::<f32>()
            + TRADE_HISTORY_CELL_GAP
                * f32::from(u8::try_from(columns.len() - 1).expect("column count fits"))
            + 16.0;
        assert!(width <= 800.0 - 12.0, "the P&L column must remain visible");
    }
}
