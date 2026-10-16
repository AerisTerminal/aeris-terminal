//! Bottom panel with open-position and trade-history tabs. Its header stays docked at the window
//! bottom while collapsed and rides the top edge of the panel while open; the open panel resizes
//! from that edge.
//!
//! The trading owner stays authoritative for fills, the round trips grouped from them and every
//! position. This module keeps one shared presentation copy of the open positions of every
//! account, practice and broker alike, and of the closed round trips, replaced only when the
//! owner's snapshot revision changes, plus the viewer's tab, open, height and account-filter
//! choices. A position leaves the positions tab when it closes and its round trip then appears
//! in the trade history.

use super::side_panel_dock::SIDE_PANEL_HEADER_HEIGHT;
use super::*;
use gpui::{Stateful, UniformListScrollHandle, uniform_list};
use std::{collections::BTreeSet, sync::Arc};

const TRADE_HISTORY_INITIAL_HEIGHT: f32 = 280.0;
/// Room for the header, the account statistics, the table header and a few rows.
const TRADE_HISTORY_MINIMUM_HEIGHT: f32 = 200.0;
const TRADE_HISTORY_MAXIMUM_HEIGHT: f32 = 640.0;
/// Space a resize always leaves for the title bar, chart header and workspace above.
const TRADE_HISTORY_MINIMUM_WORKSPACE_HEIGHT: f32 = 240.0;
const TRADE_HISTORY_RESIZE_HANDLE_HEIGHT: f32 = 6.0;
const ACCOUNT_STATS_GAP: f32 = 24.0;
const ACCOUNT_STATS_DIVIDER_HEIGHT: f32 = 28.0;
const TABLE_HEADER_HEIGHT: f32 = 30.0;
const TABLE_ROW_HEIGHT: f32 = 30.0;
const TABLE_CELL_PADDING_X: f32 = 6.0;
/// Content widths of the table columns; each cell adds its padding around them.
const TRADE_HISTORY_TIME_WIDTH: f32 = 100.0;
const TRADE_HISTORY_ACCOUNT_WIDTH: f32 = 85.0;
const TRADE_HISTORY_SYMBOL_WIDTH: f32 = 65.0;
const TRADE_HISTORY_SIDE_WIDTH: f32 = 40.0;
const TRADE_HISTORY_QUANTITY_WIDTH: f32 = 50.0;
const TRADE_HISTORY_PRICE_WIDTH: f32 = 72.0;
const TRADE_HISTORY_PNL_WIDTH: f32 = 95.0;
/// Entry, stop loss and take profit share one narrower width so the positions tab fits the
/// same compact window as the trade history.
const POSITION_PRICE_WIDTH: f32 = 64.0;
const POSITION_ACTION_WIDTH: f32 = 28.0;
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

impl TradeHistoryAccountFilter {
    fn includes(&self, account_id: &aeris_trading::TradingAccountId) -> bool {
        match self {
            Self::All => true,
            Self::Account(account_key) => account_id.as_str() == account_key,
        }
    }
}

/// Balance, all-time realized P&L and running open P&L of the shown accounts in one currency.
/// A total is `None` when any of its accounts does not know its value.
#[derive(Clone, Debug, Eq, PartialEq)]
struct AccountSummary {
    currency: String,
    balance: Option<aeris_trading::FixedPoint>,
    realized: Option<aeris_trading::FixedPoint>,
    unrealized: Option<aeris_trading::FixedPoint>,
}

/// Shared presentation copy of the trading owner's executed round trips.
#[derive(Clone, Default)]
pub(super) struct TradeHistory {
    revision: Option<u64>,
    /// Newest activity first, exactly as the owner's bounded snapshot orders them. Open trades
    /// are included here but never listed: they are the positions tab's rows.
    round_trips: Arc<[aeris_trading_runtime::TradeRoundTrip]>,
    accounts: Arc<[aeris_trading::TradingAccount]>,
    /// The owner's balance and P&L of every account.
    account_pnl: Arc<[aeris_trading::AccountPnl]>,
    symbols: Arc<BTreeMap<aeris_instruments::InstrumentId, String>>,
    /// Indices into `round_trips` of the closed trades that pass the current account filter.
    rows: Arc<[usize]>,
    /// Totals per currency of the accounts that pass the current account filter.
    summaries: Arc<[AccountSummary]>,
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
    Positions,
    TradeHistory,
}

/// How a position closes: a broker closes its own position by identity, while a practice
/// position closes at its instrument's current quote.
#[derive(Clone, Debug, Eq, PartialEq)]
enum PositionCloser {
    Broker {
        broker_position_id: String,
        /// Only a broker account the venue reaches now can close a position or report a
        /// current open P&L.
        connected: bool,
    },
    Simulated,
}

/// One open position of any account, as the positions tab lists it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct OpenPosition {
    account_id: aeris_trading::TradingAccountId,
    instrument_id: aeris_instruments::InstrumentId,
    side: aeris_trading::OrderSide,
    quantity: aeris_trading::FixedPoint,
    entry_price: Option<aeris_trading::FixedPoint>,
    stop_loss: Option<aeris_trading::FixedPoint>,
    take_profit: Option<aeris_trading::FixedPoint>,
    /// `None` while a broker account is unreachable, so its last reported value is stale.
    open_pnl: Option<aeris_trading::FixedPoint>,
    /// `None` when a practice position opened before the retained fill history.
    opened_unix_nanos: Option<i64>,
    closer: PositionCloser,
}

/// Shared presentation copy of every account's open positions.
#[derive(Clone, Default)]
struct OpenPositions {
    /// Newest opened first.
    positions: Arc<[OpenPosition]>,
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
            tab: BottomPanelTab::Positions,
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
    /// Adopts committed facts on revision changes and ephemeral marks on quote-only updates.
    pub(super) fn apply_snapshot(
        &mut self,
        snapshot: &aeris_trading_runtime::TradingSnapshot,
    ) -> bool {
        let facts_changed = self.history.revision != Some(snapshot.revision);
        if !facts_changed && self.history.account_pnl.as_ref() == snapshot.account_pnl.as_slice() {
            return false;
        }
        if facts_changed {
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
        }
        self.history.account_pnl = snapshot.account_pnl.clone().into();
        self.adopt_positions(open_positions(
            &snapshot.positions,
            &snapshot.broker_positions,
            &snapshot.round_trips,
            &snapshot.connected_broker_accounts,
            |account_id, instrument_id| snapshot.position_protection(account_id, instrument_id),
        ));
        true
    }

    /// Replaces the open-position copy and re-filters both lists.
    fn adopt_positions(&mut self, positions: Vec<OpenPosition>) {
        self.positions.positions = positions.into();
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
        self.history.summaries = account_summaries(&self.history.account_pnl, &self.filter).into();
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

/// Every open position, newest opened first: practice positions from the owner's net
/// positions, dated by their open trade and protected by the owner's working stop loss and
/// take profit orders, and broker positions exactly as the broker reports them.
fn open_positions(
    simulated: &[aeris_trading::Position],
    broker: &[aeris_trading::BrokerPosition],
    round_trips: &[aeris_trading_runtime::TradeRoundTrip],
    connected: &BTreeSet<aeris_trading::TradingAccountId>,
    protection: impl Fn(
        &aeris_trading::TradingAccountId,
        &aeris_instruments::InstrumentId,
    ) -> aeris_trading_runtime::PositionProtection,
) -> Vec<OpenPosition> {
    let simulated = simulated.iter().filter_map(|position| {
        let net = position.net_quantity;
        let side = match net.units().signum() {
            1 => aeris_trading::OrderSide::Buy,
            -1 => aeris_trading::OrderSide::Sell,
            _ => return None,
        };
        let opened_unix_nanos = round_trips
            .iter()
            .find(|trip| {
                !trip.closed
                    && trip.account_id == position.account_id
                    && trip.instrument_id == position.instrument_id
            })
            .and_then(|trip| trip.entry)
            .map(|entry| entry.first_unix_nanos);
        let protection = protection(&position.account_id, &position.instrument_id);
        Some(OpenPosition {
            account_id: position.account_id.clone(),
            instrument_id: position.instrument_id.clone(),
            side,
            quantity: aeris_trading::FixedPoint::try_new(net.units().checked_abs()?, net.scale())
                .ok()?,
            entry_price: position.average_entry_price,
            stop_loss: protection.stop_loss,
            take_profit: protection.take_profit,
            open_pnl: Some(position.unrealized_pnl),
            opened_unix_nanos,
            closer: PositionCloser::Simulated,
        })
    });
    let broker = broker.iter().map(|position| {
        let connected = connected.contains(&position.account_id);
        OpenPosition {
            account_id: position.account_id.clone(),
            instrument_id: position.instrument_id.clone(),
            side: position.side,
            quantity: position.quantity,
            entry_price: Some(position.entry_price),
            stop_loss: position.stop_loss,
            take_profit: position.take_profit,
            open_pnl: connected.then_some(position.net_unrealized),
            opened_unix_nanos: Some(position.opened_unix_nanos),
            closer: PositionCloser::Broker {
                broker_position_id: position.broker_position_id.clone(),
                connected,
            },
        }
    });
    let mut positions = simulated.chain(broker).collect::<Vec<_>>();
    positions.sort_by_key(|position| std::cmp::Reverse(position.opened_unix_nanos));
    positions
}

/// Closed trades only; an open trade is listed as a position until it closes.
fn trade_history_rows(
    round_trips: &[aeris_trading_runtime::TradeRoundTrip],
    filter: &TradeHistoryAccountFilter,
) -> Vec<usize> {
    round_trips
        .iter()
        .enumerate()
        .filter(|(_, trip)| trip.closed && filter.includes(&trip.account_id))
        .map(|(index, _)| index)
        .collect()
}

fn position_rows(positions: &[OpenPosition], filter: &TradeHistoryAccountFilter) -> Vec<usize> {
    positions
        .iter()
        .enumerate()
        .filter(|(_, position)| filter.includes(&position.account_id))
        .map(|(index, _)| index)
        .collect()
}

/// Totals of the filtered accounts per currency, in the owner's account order.
fn account_summaries(
    account_pnl: &[aeris_trading::AccountPnl],
    filter: &TradeHistoryAccountFilter,
) -> Vec<AccountSummary> {
    let total = |sum: Option<aeris_trading::FixedPoint>, value| {
        sum.zip(value)
            .and_then(|(sum, value)| sum.checked_add(value).ok())
    };
    let mut summaries = Vec::<AccountSummary>::new();
    for pnl in account_pnl
        .iter()
        .filter(|pnl| filter.includes(&pnl.account_id))
    {
        match summaries
            .iter_mut()
            .find(|summary| summary.currency == pnl.currency)
        {
            Some(summary) => {
                summary.balance = total(summary.balance, pnl.balance);
                summary.realized = total(summary.realized, Some(pnl.realized));
                summary.unrealized = total(summary.unrealized, pnl.unrealized);
            }
            None => summaries.push(AccountSummary {
                currency: pnl.currency.clone(),
                balance: pnl.balance,
                realized: Some(pnl.realized),
                unrealized: pnl.unrealized,
            }),
        }
    }
    summaries
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

    /// Closes a practice position at the current quote of any open chart of its instrument;
    /// the practice venue fills only against a live bid and ask.
    fn close_simulated_position(
        &mut self,
        account_key: String,
        instrument_id: &aeris_instruments::InstrumentId,
        cx: &mut Context<Self>,
    ) {
        let frame = self
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.panes)
            .find_map(|pane| {
                pane.surface
                    .read(cx)
                    .order_book
                    .read(cx)
                    .frame()
                    .filter(|frame| {
                        frame.instrument_id == instrument_id.as_str()
                            && frame.best_bid.is_some()
                            && frame.best_ask.is_some()
                    })
                    .cloned()
            });
        if let Some(frame) = frame {
            aeris_desktop::trading::flatten_simulated_account_for(&frame, Some(account_key), cx);
        } else {
            aeris_desktop::trading::record_notice(Err(format!(
                "Open a {} chart with a live quote to close this practice position",
                self.bottom_panel.history.symbol(instrument_id)
            )));
        }
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
        .when(open, |panel| {
            let panel = panel.children(account_stats(state, theme));
            match state.tab {
                BottomPanelTab::TradeHistory => panel
                    .child(trade_history_column_header(
                        state.shows_account_column(),
                        theme,
                    ))
                    .child(trade_history_list(state, chart, theme)),
                BottomPanelTab::Positions => panel
                    .child(position_column_header(state.shows_account_column(), theme))
                    .child(position_list(terminal, state, chart, theme)),
            }
            .child(trade_history_resize_handle())
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
        .child(bottom_panel_tabs(terminal, state, theme))
        .when(open, |header| header.child(count_label))
        .child(div().flex_1())
        .when(rithmic_attribution, |header| {
            header.child(super::terminal_chrome::rithmic_attribution(theme))
        })
        // The account filter sits at the trailing edge, beside the collapse control.
        .when(open && state.history.accounts.len() > 1, |header| {
            header.child(trade_history_account_filter(terminal, state, theme))
        })
        .child(trade_history_expand_button(terminal, open, theme))
}

/// The statistics section above the table: balance, realized P&L and open P&L of the shown
/// accounts, one group per currency.
fn account_stats(state: &BottomPanelState, theme: &AerisTheme) -> Option<Div> {
    let colors = theme.colors;
    let summaries = &state.history.summaries;
    if summaries.is_empty() {
        return None;
    }
    let divider = || {
        div()
            .flex_none()
            .w(px(theme.dimensions.border_width))
            .h(px(ACCOUNT_STATS_DIVIDER_HEIGHT))
            .bg(gpui_color(colors.border))
    };
    let mut section = div()
        .flex_none()
        .w_full()
        .flex()
        .items_center()
        .gap(px(ACCOUNT_STATS_GAP))
        .px_3()
        .py_2()
        .overflow_hidden()
        .border_b(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border));
    for (index, summary) in summaries.iter().enumerate() {
        if index > 0 {
            section = section.child(divider());
        }
        section = section.children(account_summary_stats(summary, theme));
    }
    Some(section)
}

fn account_summary_stats(summary: &AccountSummary, theme: &AerisTheme) -> [Div; 3] {
    let colors = theme.colors;
    let stat = |label: &'static str, value: Div| {
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .text_xs()
                    .text_color(gpui_color(colors.text_secondary))
                    .child(label),
            )
            .child(
                value
                    .text_sm()
                    .font_weight(platform_font_weight(TypographyRole::Emphasis))
                    .font_features(platform_tabular_numerals()),
            )
    };
    [
        stat(
            "Balance",
            div()
                .text_color(gpui_color(colors.text_primary))
                .child(trade_history_money_text(summary.balance, &summary.currency)),
        ),
        stat(
            "Realized P&L",
            trade_history_pnl_value(summary.realized, &summary.currency, theme),
        ),
        stat(
            "Unrealized P&L",
            trade_history_pnl_value(summary.unrealized, &summary.currency, theme),
        ),
    ]
}

/// The panel's tabs. While the panel is collapsed no tab is selected; choosing the open tab
/// again collapses the panel.
fn bottom_panel_tabs(
    terminal: &Entity<TerminalApp>,
    state: &BottomPanelState,
    theme: &AerisTheme,
) -> TabList {
    TabList::new("bottom_panel_tabs", "Bottom panel", theme)
        .compact()
        .children(
            [BottomPanelTab::Positions, BottomPanelTab::TradeHistory].map(|tab| {
                let select_terminal = terminal.clone();
                let active = state.open && state.tab == tab;
                let (id, label, show, hide) = match tab {
                    BottomPanelTab::TradeHistory => (
                        "trade_history_tab",
                        "Trade history",
                        "Show trade history",
                        "Hide trade history",
                    ),
                    BottomPanelTab::Positions => (
                        "positions_tab",
                        "Positions",
                        "Show open positions",
                        "Hide open positions",
                    ),
                };
                Tab::new(id, theme)
                    .compact()
                    .selected(active)
                    .aria_label(if active { hide } else { show })
                    .child(label)
                    .on_click(move |_, _, cx| {
                        select_terminal.update(cx, |terminal, terminal_cx| {
                            terminal.select_bottom_panel_tab(tab, terminal_cx);
                        });
                    })
            }),
        )
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
            .variant(ButtonVariant::Ghost)
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
        .variant(ButtonVariant::Outline)
        .button_size(ButtonSize::Sm)
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
        // The trigger sits at the header's right edge, so the menu lines up with it from there.
        MenuPlacement::Anchored {
            side: MenuSide::Above,
            align: MenuAlign::End,
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
    table_header_frame(theme)
        .child(text_cell(TRADE_HISTORY_TIME_WIDTH, "Opened", theme))
        .child(text_cell(TRADE_HISTORY_TIME_WIDTH, "Closed", theme))
        .when(show_account, |row| {
            row.child(text_cell(TRADE_HISTORY_ACCOUNT_WIDTH, "Account", theme))
        })
        .child(text_cell(TRADE_HISTORY_SYMBOL_WIDTH, "Symbol", theme))
        .child(text_cell(TRADE_HISTORY_SIDE_WIDTH, "Side", theme))
        .child(numeric_cell(TRADE_HISTORY_QUANTITY_WIDTH, "Qty", theme))
        .child(numeric_cell(TRADE_HISTORY_PRICE_WIDTH, "Entry", theme))
        .child(numeric_cell(TRADE_HISTORY_PRICE_WIDTH, "Exit", theme))
        .child(last_cell(numeric_cell(
            TRADE_HISTORY_PNL_WIDTH,
            "P&L",
            theme,
        )))
}

fn trade_history_list(
    state: &BottomPanelState,
    chart: Option<Entity<AerisChartView>>,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    if state.history.rows.is_empty() {
        let message = if state.history.round_trips.iter().any(|trip| trip.closed) {
            "No closed trades for this account"
        } else {
            "No closed trades yet"
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
    let list = uniform_list(
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
    .track_scroll(&state.scroll);
    table_body(list, &state.scroll, theme)
}

/// The scrolling rows below a table header, with the screener's thin scrollbar.
fn table_body(
    list: gpui::UniformList,
    scroll: &UniformListScrollHandle,
    theme: &AerisTheme,
) -> AnyElement {
    let scrollbar = scroll.0.borrow().base_handle.clone();
    div()
        .relative()
        .flex_1()
        .min_h_0()
        .w_full()
        .child(list.size_full())
        .child(ThinScrollbar::new(
            &scrollbar,
            gpui_color(theme.colors.text_secondary),
        ))
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
    let closed = trip
        .exit
        .map(|exit| trade_history_time(chart, exit.last_unix_nanos));
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
    table_row_frame(theme)
        .id(("trade_history_row", index))
        .child(text_cell(
            TRADE_HISTORY_TIME_WIDTH,
            trade_history_text(opened),
            theme,
        ))
        .child(text_cell(
            TRADE_HISTORY_TIME_WIDTH,
            trade_history_text(closed),
            theme,
        ))
        .when(show_account, |row| {
            row.child(text_cell(
                TRADE_HISTORY_ACCOUNT_WIDTH,
                history.account_name(trip.account_id.as_str()),
                theme,
            ))
        })
        .child(text_cell(
            TRADE_HISTORY_SYMBOL_WIDTH,
            history.symbol(&trip.instrument_id),
            theme,
        ))
        .child(text_cell(TRADE_HISTORY_SIDE_WIDTH, side, theme).text_color(gpui_color(side_color)))
        .child(numeric_cell(
            TRADE_HISTORY_QUANTITY_WIDTH,
            trade_history_text(quantity),
            theme,
        ))
        .child(numeric_cell(
            TRADE_HISTORY_PRICE_WIDTH,
            trade_history_text(entry_price),
            theme,
        ))
        .child(numeric_cell(
            TRADE_HISTORY_PRICE_WIDTH,
            trade_history_text(exit_price),
            theme,
        ))
        .child(last_cell(trade_history_pnl_cell(
            trip.final_pnl,
            currency,
            theme,
        )))
}

fn position_column_header(show_account: bool, theme: &AerisTheme) -> impl IntoElement {
    table_header_frame(theme)
        .child(text_cell(TRADE_HISTORY_TIME_WIDTH, "Opened", theme))
        .when(show_account, |row| {
            row.child(text_cell(TRADE_HISTORY_ACCOUNT_WIDTH, "Account", theme))
        })
        .child(text_cell(TRADE_HISTORY_SYMBOL_WIDTH, "Symbol", theme))
        .child(text_cell(TRADE_HISTORY_SIDE_WIDTH, "Side", theme))
        .child(numeric_cell(TRADE_HISTORY_QUANTITY_WIDTH, "Qty", theme))
        .child(numeric_cell(POSITION_PRICE_WIDTH, "Entry", theme))
        .child(numeric_cell(POSITION_PRICE_WIDTH, "SL", theme))
        .child(numeric_cell(POSITION_PRICE_WIDTH, "TP", theme))
        .child(numeric_cell(TRADE_HISTORY_PNL_WIDTH, "Open P&L", theme))
        .child(position_action_cell())
}

fn position_list(
    terminal: &Entity<TerminalApp>,
    state: &BottomPanelState,
    chart: Option<Entity<AerisChartView>>,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    if state.positions.rows.is_empty() {
        let message = if state.positions.positions.is_empty() {
            "No open positions"
        } else {
            "No open positions for this account"
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
    let terminal = terminal.clone();
    let positions = state.positions.clone();
    let history = state.history.clone();
    let show_account = state.shows_account_column();
    let list_theme = *theme;
    let list = uniform_list(
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
                        &terminal,
                        &history,
                        chart,
                        show_account,
                        &list_theme,
                    ))
                })
                .collect::<Vec<_>>()
        },
    )
    .track_scroll(&state.positions_scroll);
    table_body(list, &state.positions_scroll, theme)
}

/// One open position. A broker position's open P&L is the broker's own value and shows only
/// while the venue reaches the account; closing it is asynchronous, so the row stays until the
/// broker reports the position closed.
fn position_row(
    index: usize,
    position: &OpenPosition,
    terminal: &Entity<TerminalApp>,
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
    let opened = position
        .opened_unix_nanos
        .map(|opened| trade_history_time(chart, opened));
    table_row_frame(theme)
        .id(("position_row", index))
        .child(text_cell(
            TRADE_HISTORY_TIME_WIDTH,
            trade_history_text(opened),
            theme,
        ))
        .when(show_account, |row| {
            row.child(text_cell(
                TRADE_HISTORY_ACCOUNT_WIDTH,
                history.account_name(position.account_id.as_str()),
                theme,
            ))
        })
        .child(text_cell(
            TRADE_HISTORY_SYMBOL_WIDTH,
            history.symbol(&position.instrument_id),
            theme,
        ))
        .child(text_cell(TRADE_HISTORY_SIDE_WIDTH, side, theme).text_color(gpui_color(side_color)))
        .child(numeric_cell(
            TRADE_HISTORY_QUANTITY_WIDTH,
            trade_history_decimal(position.quantity),
            theme,
        ))
        .child(numeric_cell(
            POSITION_PRICE_WIDTH,
            price(position.entry_price),
            theme,
        ))
        .child(numeric_cell(
            POSITION_PRICE_WIDTH,
            price(position.stop_loss),
            theme,
        ))
        .child(numeric_cell(
            POSITION_PRICE_WIDTH,
            price(position.take_profit),
            theme,
        ))
        .child(trade_history_pnl_cell(
            position.open_pnl,
            history.account_currency(position.account_id.as_str()),
            theme,
        ))
        .child(
            position_action_cell().child(position_close_button(index, position, terminal, theme)),
        )
}

/// The undivided trailing column that holds a position's close action.
fn position_action_cell() -> Div {
    div()
        .h_full()
        .w(px(POSITION_ACTION_WIDTH + 2.0 * TABLE_CELL_PADDING_X))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
}

fn position_close_button(
    index: usize,
    position: &OpenPosition,
    terminal: &Entity<TerminalApp>,
    theme: &AerisTheme,
) -> Button {
    let account_key = position.account_id.as_str().to_string();
    let button = Button::new(("position_close", index), theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Sm)
        .round()
        .danger_on_hover()
        .icon(header_icon(HugeIcon::Close))
        .aria_label("Close position")
        .tooltip(TooltipSpec::new("Close position", theme).show_delay(TOOLTIP_OPEN_DELAY));
    match &position.closer {
        PositionCloser::Broker {
            broker_position_id,
            connected,
        } => {
            let broker_position_id = broker_position_id.clone();
            button.disabled(!connected).on_click(move |_, _, cx| {
                aeris_desktop::trading::close_broker_position(
                    account_key.clone(),
                    broker_position_id.clone(),
                    cx,
                );
            })
        }
        PositionCloser::Simulated => {
            let terminal = terminal.clone();
            let instrument_id = position.instrument_id.clone();
            button.on_click(move |_, _, cx| {
                terminal.update(cx, |terminal, terminal_cx| {
                    terminal.close_simulated_position(
                        account_key.clone(),
                        &instrument_id,
                        terminal_cx,
                    );
                });
            })
        }
    }
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

/// A signed P&L amount colored by its sign; an unavailable result shows a dash.
fn trade_history_pnl_cell(
    pnl: Option<aeris_trading::FixedPoint>,
    currency: &str,
    theme: &AerisTheme,
) -> Div {
    numeric_cell(
        TRADE_HISTORY_PNL_WIDTH,
        trade_history_pnl_value(pnl, currency, theme),
        theme,
    )
}

fn trade_history_pnl_value(
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
    div()
        .text_color(gpui_color(color))
        .child(trade_history_money_text(pnl, currency))
}

fn trade_history_money_text(value: Option<aeris_trading::FixedPoint>, currency: &str) -> String {
    value.map_or_else(
        || "—".to_string(),
        |value| {
            format!(
                "{currency} {}",
                market_price_text(value.units(), u32::from(value.scale()))
            )
        },
    )
}

/// The column header band, styled like the screener table's.
fn table_header_frame(theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    table_frame(TABLE_HEADER_HEIGHT)
        .bg(gpui_color(colors.surface_secondary))
        .border_b(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border_secondary))
        .text_color(gpui_color(colors.text_secondary))
}

fn table_row_frame(theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    table_frame(TABLE_ROW_HEIGHT)
        .border_b(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border_subtle))
        .text_color(gpui_color(colors.text_default))
        .hover(move |row| row.bg(gpui_color(colors.hover_bg.over(colors.surface))))
}

/// Rows span the full panel width; a virtualized list otherwise sizes rows to their content.
fn table_frame(height: f32) -> Div {
    div()
        .w_full()
        .h(px(height))
        .flex_none()
        .flex()
        .items_center()
        .text_xs()
        .font_features(platform_tabular_numerals())
}

/// A padded cell at least `width` wide for its content, followed by the column divider.
fn table_cell(width: f32, theme: &AerisTheme) -> Div {
    div()
        .h_full()
        .min_w(px(width + 2.0 * TABLE_CELL_PADDING_X))
        .flex_1()
        .flex()
        .items_center()
        .overflow_hidden()
        .px(px(TABLE_CELL_PADDING_X))
        .border_r(px(theme.dimensions.border_width))
        .border_color(gpui_color(theme.colors.border_subtle))
}

fn text_cell(width: f32, content: impl IntoElement, theme: &AerisTheme) -> Div {
    table_cell(width, theme).child(div().min_w_0().truncate().child(content))
}

/// Numbers align to the cell's trailing edge.
fn numeric_cell(width: f32, content: impl IntoElement, theme: &AerisTheme) -> Div {
    table_cell(width, theme)
        .justify_end()
        .child(div().min_w_0().truncate().child(content))
}

/// A row's last cell carries no divider.
fn last_cell(cell: Div) -> Div {
    cell.border_r(px(0.0))
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
            open_pnl: None,
        }
    }

    fn open_round_trip(account: &str) -> aeris_trading_runtime::TradeRoundTrip {
        aeris_trading_runtime::TradeRoundTrip {
            exit: None,
            closed: false,
            final_pnl: None,
            ..round_trip(account)
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
    fn trade_history_lists_closed_trades_only() {
        let trips = [open_round_trip("a"), round_trip("a"), open_round_trip("b")];
        assert_eq!(
            trade_history_rows(&trips, &TradeHistoryAccountFilter::All),
            vec![1]
        );
        assert_eq!(
            trade_history_rows(&trips, &TradeHistoryAccountFilter::Account("b".to_string())),
            Vec::<usize>::new()
        );
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
        assert_eq!(trade_history_money_text(Some(loss), "USD"), "USD -2525.00");
        assert_eq!(trade_history_money_text(Some(gain), "USD"), "USD 7350.00");
        assert_eq!(trade_history_money_text(None, "USD"), "—");
    }

    fn account_pnl(
        account: &str,
        currency: &str,
        balance: Option<i64>,
        realized: i64,
        unrealized: Option<i64>,
    ) -> aeris_trading::AccountPnl {
        let money = |units| aeris_trading::FixedPoint::try_new(units, 2).expect("money");
        aeris_trading::AccountPnl {
            account_id: aeris_trading::TradingAccountId::try_new(account).expect("account"),
            currency: currency.to_string(),
            realized: money(realized),
            unrealized: unrealized.map(money),
            balance: balance.map(money),
            equity: None,
        }
    }

    #[test]
    fn account_summary_totals_the_shown_accounts_per_currency() {
        let money = |units| Some(aeris_trading::FixedPoint::try_new(units, 2).expect("money"));
        let accounts = [
            account_pnl("a", "USD", Some(100_000), 50_00, Some(-5_00)),
            account_pnl("b", "EUR", Some(200_000), -10_00, Some(3_00)),
            account_pnl("c", "USD", Some(50_000), 20_00, Some(1_00)),
            // A disconnected broker account knows only its realized P&L.
            account_pnl("d", "EUR", None, 7_00, None),
        ];
        assert_eq!(
            account_summaries(&accounts, &TradeHistoryAccountFilter::All),
            [
                AccountSummary {
                    currency: "USD".to_string(),
                    balance: money(150_000),
                    realized: money(70_00),
                    unrealized: money(-4_00),
                },
                AccountSummary {
                    currency: "EUR".to_string(),
                    balance: None,
                    realized: money(-3_00),
                    unrealized: None,
                },
            ]
        );
        assert_eq!(
            account_summaries(
                &accounts,
                &TradeHistoryAccountFilter::Account("c".to_string())
            ),
            [AccountSummary {
                currency: "USD".to_string(),
                balance: money(50_000),
                realized: money(20_00),
                unrealized: money(1_00),
            }]
        );
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

    fn simulated_position(account: &str, net_units: i64) -> aeris_trading::Position {
        let money = |units| aeris_trading::FixedPoint::try_new(units, 2).expect("money");
        aeris_trading::Position {
            account_id: aeris_trading::TradingAccountId::try_new(account).expect("account"),
            instrument_id: aeris_instruments::InstrumentId::try_new("hyperliquid:perp:BTC")
                .expect("instrument"),
            net_quantity: aeris_trading::FixedPoint::try_new(net_units, 8).expect("quantity"),
            average_entry_price: Some(
                aeris_trading::FixedPoint::try_new(8_309_000_000_000, 8).expect("price"),
            ),
            realized_pnl: money(0),
            unrealized_pnl: money(-1_250),
            last_fill_unix_nanos: 40,
        }
    }

    fn marked_snapshot(mark: i64) -> aeris_trading_runtime::TradingSnapshot {
        let mut position = simulated_position("practice", 100_000_000);
        position.unrealized_pnl = aeris_trading::FixedPoint::try_new(mark, 2).expect("mark");
        aeris_trading_runtime::TradingSnapshot {
            revision: 4,
            market_observation_error: None,
            accounts: Vec::new(),
            instruments: Vec::new(),
            orders: Vec::new(),
            order_events: Vec::new(),
            fills: Vec::new(),
            completed_trade_pnl: BTreeMap::new(),
            fill_realized_pnl: BTreeMap::new(),
            round_trips: vec![open_round_trip("practice")],
            positions: vec![position],
            broker_positions: Vec::new(),
            broker_balances: BTreeMap::new(),
            connected_broker_accounts: BTreeSet::new(),
            position_pnl: Vec::new(),
            account_pnl: vec![account_pnl("practice", "USD", Some(50_000), 0, Some(mark))],
            risk_profiles: Vec::new(),
            risk_meters: Vec::new(),
            risk_locks: Vec::new(),
            risk_rule_states: Vec::new(),
            discipline_states: Vec::new(),
            session_plans: Vec::new(),
            session_adherence_reviews: Vec::new(),
            trade_copiers: Vec::new(),
            copy_dispatches: Vec::new(),
            strategy_templates: Vec::new(),
            managed_brackets: Vec::new(),
            planned_bracket_legs: Vec::new(),
            protective_orders: Vec::new(),
        }
    }

    #[test]
    fn mark_only_snapshot_refreshes_open_position_and_summary_without_revision_bump() {
        let mut panel = BottomPanelState::default();
        let first = marked_snapshot(100);
        assert!(panel.apply_snapshot(&first));
        let frozen_history = panel.history.round_trips.clone();
        assert!(
            !panel.apply_snapshot(&first),
            "unchanged mark does not redraw"
        );
        let second = marked_snapshot(250);
        assert_eq!(first.revision, second.revision);
        assert!(panel.apply_snapshot(&second), "new mark redraws the panel");
        assert_eq!(
            panel.positions.positions[0].open_pnl,
            Some(aeris_trading::FixedPoint::try_new(250, 2).expect("mark"))
        );
        assert_eq!(
            panel.history.summaries[0].unrealized,
            panel.positions.positions[0].open_pnl
        );
        assert!(Arc::ptr_eq(&frozen_history, &panel.history.round_trips));
        assert!(!panel.apply_snapshot(&second));
    }

    fn broker_ids(state: &BottomPanelState) -> Vec<String> {
        state
            .positions
            .rows
            .iter()
            .filter_map(|row| match &state.positions.positions[*row].closer {
                PositionCloser::Broker {
                    broker_position_id, ..
                } => Some(broker_position_id.clone()),
                PositionCloser::Simulated => None,
            })
            .collect()
    }

    #[test]
    fn positions_follow_the_account_filter_newest_opened_first() {
        let mut state = BottomPanelState::default();
        state.adopt_positions(open_positions(
            &[],
            &[
                broker_position("a", "1", 10),
                broker_position("b", "2", 30),
                broker_position("a", "3", 20),
            ],
            &[],
            &BTreeSet::new(),
            |_, _| aeris_trading_runtime::PositionProtection::default(),
        ));
        assert_eq!(broker_ids(&state), ["2", "3", "1"]);
        assert_eq!(
            position_rows(
                &state.positions.positions,
                &TradeHistoryAccountFilter::Account("a".to_string())
            ),
            vec![1, 2]
        );
        assert!(
            state
                .positions
                .positions
                .iter()
                .all(|position| position.open_pnl.is_none()),
            "an unreachable broker account has no current open P&L"
        );
    }

    #[test]
    fn positions_list_practice_and_broker_accounts_together() {
        let connected =
            BTreeSet::from([aeris_trading::TradingAccountId::try_new("broker").expect("account")]);
        let positions = open_positions(
            &[
                simulated_position("practice", -200_000_000),
                simulated_position("flat", 0),
            ],
            &[broker_position("broker", "7", 10)],
            &[open_round_trip("practice"), round_trip("flat")],
            &connected,
            |account_id, _| aeris_trading_runtime::PositionProtection {
                stop_loss: (account_id.as_str() == "practice")
                    .then(|| aeris_trading::FixedPoint::try_new(1_920, 0).expect("stop")),
                take_profit: None,
            },
        );
        assert_eq!(positions.len(), 2, "a flat practice position is not open");

        let practice = &positions[0];
        assert_eq!(practice.closer, PositionCloser::Simulated);
        assert_eq!(
            practice.stop_loss.map(aeris_trading::FixedPoint::units),
            Some(1_920),
            "a practice position shows the owner's working stop loss"
        );
        assert_eq!(practice.take_profit, None);
        assert_eq!(practice.side, aeris_trading::OrderSide::Sell);
        assert_eq!(practice.quantity.units(), 200_000_000);
        assert_eq!(
            practice.opened_unix_nanos,
            Some(1_790_704_166_986_188_900),
            "a practice position is dated by its open trade"
        );
        assert_eq!(
            practice.open_pnl.map(aeris_trading::FixedPoint::units),
            Some(-1_250)
        );

        let broker = &positions[1];
        assert_eq!(
            broker.closer,
            PositionCloser::Broker {
                broker_position_id: "7".to_string(),
                connected: true,
            }
        );
        assert_eq!(
            broker.open_pnl.map(aeris_trading::FixedPoint::units),
            Some(-6)
        );
    }

    #[test]
    fn positions_is_the_first_and_default_tab() {
        let state = BottomPanelState::default();
        assert_eq!(state.tab, BottomPanelTab::Positions);
        assert_eq!(BottomPanelTab::default(), BottomPanelTab::Positions);
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
        assert!(
            table_width(&columns) <= 800.0 - 12.0,
            "the Close action must remain visible"
        );
    }

    /// Every cell pads its content width on both sides.
    fn table_width(columns: &[f32]) -> f32 {
        columns
            .iter()
            .map(|width| width + 2.0 * TABLE_CELL_PADDING_X)
            .sum()
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
        assert!(
            table_width(&columns) <= 800.0 - 12.0,
            "the P&L column must remain visible"
        );
    }
}
