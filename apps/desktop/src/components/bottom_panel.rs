//! Bottom trade-history panel. Its header stays docked at the window bottom while collapsed
//! and rides the top edge of the panel while open; the open panel resizes from that edge.
//!
//! The trading owner stays authoritative for fills. This module keeps one shared presentation
//! copy of its bounded, newest-first fill projection, replaced only when the owner's snapshot
//! revision changes, plus the viewer's open, height and account-filter choices.

use super::side_panel_dock::SIDE_PANEL_HEADER_HEIGHT;
use super::*;
use gpui::{Stateful, UniformListScrollHandle, uniform_list};
use std::sync::Arc;

const TRADE_HISTORY_INITIAL_HEIGHT: f32 = 260.0;
const TRADE_HISTORY_MINIMUM_HEIGHT: f32 = 140.0;
const TRADE_HISTORY_MAXIMUM_HEIGHT: f32 = 640.0;
/// Space a resize always leaves for the title bar, chart header and workspace above.
const TRADE_HISTORY_MINIMUM_WORKSPACE_HEIGHT: f32 = 240.0;
const TRADE_HISTORY_RESIZE_HANDLE_HEIGHT: f32 = 6.0;
/// Header controls stay inside the panel header with room around their hover fill.
const TRADE_HISTORY_HEADER_CONTROL_HEIGHT: f32 = 22.0;
const TRADE_HISTORY_ROW_HEIGHT: f32 = 28.0;
const TRADE_HISTORY_TIME_WIDTH: f32 = 115.0;
const TRADE_HISTORY_ACCOUNT_WIDTH: f32 = 100.0;
const TRADE_HISTORY_SYMBOL_WIDTH: f32 = 80.0;
const TRADE_HISTORY_SIDE_WIDTH: f32 = 40.0;
const TRADE_HISTORY_QUANTITY_WIDTH: f32 = 70.0;
const TRADE_HISTORY_PRICE_WIDTH: f32 = 95.0;
const TRADE_HISTORY_PNL_WIDTH: f32 = 105.0;
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

/// Shared presentation copy of the trading owner's executed fills.
#[derive(Clone, Default)]
pub(super) struct TradeHistory {
    revision: Option<u64>,
    /// Newest first, exactly as the owner's bounded snapshot orders them.
    fills: Arc<[aeris_trading::Fill]>,
    completed_trade_pnl: Arc<BTreeMap<aeris_trading::FillId, aeris_trading::FixedPoint>>,
    fill_realized_pnl: Arc<BTreeMap<aeris_trading::FillId, aeris_trading::FixedPoint>>,
    accounts: Arc<[aeris_trading::TradingAccount]>,
    symbols: Arc<BTreeMap<aeris_instruments::InstrumentId, String>>,
    /// Indices into `fills` that pass the current account filter.
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

/// Viewer state of the bottom panel. It owns presentation only, never trading state.
pub(super) struct BottomPanelState {
    pub(super) trade_history_open: bool,
    pub(super) account_menu_open: bool,
    /// Height of the open panel, including its header.
    height: f32,
    filter: TradeHistoryAccountFilter,
    history: TradeHistory,
    scroll: UniformListScrollHandle,
}

impl Default for BottomPanelState {
    fn default() -> Self {
        Self {
            trade_history_open: false,
            account_menu_open: false,
            height: TRADE_HISTORY_INITIAL_HEIGHT,
            filter: TradeHistoryAccountFilter::All,
            history: TradeHistory::default(),
            scroll: UniformListScrollHandle::new(),
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
        self.history.fills = snapshot.fills.clone().into();
        self.history.completed_trade_pnl = Arc::new(snapshot.completed_trade_pnl.clone());
        self.history.fill_realized_pnl = Arc::new(snapshot.fill_realized_pnl.clone());
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
        self.refresh_rows();
        true
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
        self.history.rows = trade_history_rows(&self.history.fills, &self.filter).into();
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
    fills: &[aeris_trading::Fill],
    filter: &TradeHistoryAccountFilter,
) -> Vec<usize> {
    fills
        .iter()
        .enumerate()
        .filter(|(_, fill)| match filter {
            TradeHistoryAccountFilter::All => true,
            TradeHistoryAccountFilter::Account(account_key) => {
                fill.account_id.as_str() == account_key
            }
        })
        .map(|(index, _)| index)
        .collect()
}

impl TerminalApp {
    fn toggle_trade_history(&mut self, cx: &mut Context<Self>) {
        self.bottom_panel.trade_history_open = !self.bottom_panel.trade_history_open;
        self.bottom_panel.account_menu_open = false;
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
    let open = state.trade_history_open;
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
            panel
                .child(trade_history_column_header(
                    state.shows_account_column(),
                    theme,
                ))
                .child(trade_history_list(state, chart, theme))
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
    let open = state.trade_history_open;
    let count = state.history.rows.len();
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
        .child(trade_history_tab(terminal, open, theme))
        .when(open && state.history.accounts.len() > 1, |header| {
            header.child(trade_history_account_filter(terminal, state, theme))
        })
        .when(open, |header| {
            header.child(format!(
                "{count} {}",
                if count == 1 { "trade" } else { "trades" }
            ))
        })
        .child(div().flex_1())
        .when(rithmic_attribution, |header| {
            header.child(super::terminal_chrome::rithmic_attribution(theme))
        })
        .child(trade_history_expand_button(terminal, open, theme))
}

fn trade_history_tab(
    terminal: &Entity<TerminalApp>,
    open: bool,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let toggle_terminal = terminal.clone();
    div()
        .id("trade_history_tab")
        .h(px(TRADE_HISTORY_HEADER_CONTROL_HEIGHT))
        .flex_none()
        .px_2()
        .flex()
        .items_center()
        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
        .role(Role::Button)
        .aria_label(if open {
            "Hide trade history"
        } else {
            "Show trade history"
        })
        .cursor_pointer()
        .text_color(gpui_color(if open {
            colors.text_active
        } else {
            colors.text_interactive
        }))
        .when(open, |tab| {
            tab.bg(gpui_color(colors.active_bg.over(colors.surface)))
        })
        .hover(move |tab| {
            tab.bg(gpui_color(colors.hover_bg.over(colors.surface)))
                .text_color(gpui_color(colors.text_hover))
        })
        .child("TRADE HISTORY")
        .on_click(move |_, _, cx| {
            toggle_terminal.update(cx, TerminalApp::toggle_trade_history);
        })
}

fn trade_history_expand_button(
    terminal: &Entity<TerminalApp>,
    open: bool,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let toggle_terminal = terminal.clone();
    let label = if open {
        "Collapse trade history"
    } else {
        "Expand trade history"
    };
    let chevron = header_icon(HugeIcon::ChevronDown).with_size(px(WORKSPACE_TAB_ICON_GLYPH));
    let chevron = if open { chevron } else { chevron.rotate(0.5) };
    chrome_tooltip(
        "trade_history_expand",
        label,
        div()
            .id("trade_history_expand")
            .size(px(WORKSPACE_TAB_ICON_HIT))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
            .role(Role::Button)
            .aria_label(label)
            .cursor_pointer()
            .text_color(gpui_color(colors.icon))
            .hover(move |button| {
                button
                    .bg(gpui_color(colors.hover_bg.over(colors.surface)))
                    .text_color(gpui_color(colors.icon_active))
            })
            .child(chevron)
            .on_click(move |_, _, cx| {
                toggle_terminal.update(cx, TerminalApp::toggle_trade_history);
            }),
        theme,
    )
}

fn trade_history_account_filter(
    terminal: &Entity<TerminalApp>,
    state: &BottomPanelState,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let toggle_terminal = terminal.clone();
    let dismiss_terminal = terminal.clone();
    let trigger = div()
        .id("trade_history_account_filter")
        .h(px(TRADE_HISTORY_HEADER_CONTROL_HEIGHT))
        .flex_none()
        .px_2()
        .flex()
        .items_center()
        .gap_1()
        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
        .border(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(if state.account_menu_open {
            colors.active_bg.over(colors.surface_secondary)
        } else {
            colors.surface_secondary
        }))
        .text_color(gpui_color(colors.text_default))
        .role(Role::Button)
        .aria_label("Filter trades by account")
        .cursor_pointer()
        .hover(move |trigger| {
            trigger.bg(gpui_color(colors.hover_bg.over(colors.surface_secondary)))
        })
        .child(state.filter_label())
        .child(
            header_icon(HugeIcon::ChevronDown)
                .with_size(px(12.0))
                .color(gpui_color(colors.icon)),
        )
        .on_click(move |_, _, cx| {
            toggle_terminal.update(cx, TerminalApp::toggle_trade_history_account_menu);
        });
    div()
        .id("trade_history_account_filter_anchor")
        .relative()
        .flex_none()
        .child(trigger)
        .when(state.account_menu_open, |anchor| {
            anchor
                .on_mouse_down_out(move |_, _, cx| {
                    dismiss_terminal.update(cx, TerminalApp::close_trade_history_account_menu);
                })
                .child(gpui::deferred(trade_history_account_menu(
                    terminal, state, theme,
                )))
        })
}

/// Opens upward over the workspace so a short panel never clips it at the window bottom.
fn trade_history_account_menu(
    terminal: &Entity<TerminalApp>,
    state: &BottomPanelState,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let all_terminal = terminal.clone();
    let mut menu = div()
        .id("trade_history_account_menu")
        .absolute()
        .bottom(px(TRADE_HISTORY_HEADER_CONTROL_HEIGHT + 4.0))
        .left_0()
        .w(px(TRADE_HISTORY_ACCOUNT_MENU_WIDTH))
        .max_h(px(TRADE_HISTORY_ACCOUNT_MENU_MAX_HEIGHT))
        .overflow_y_scroll()
        .occlude()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface_secondary))
        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
        .child(
            MenuRow::compact("trade_history_account_all", "All accounts", theme)
                .highlighted(state.filter == TradeHistoryAccountFilter::All)
                .on_click(move |_, _, cx| {
                    all_terminal.update(cx, |terminal, terminal_cx| {
                        terminal
                            .set_trade_history_filter(TradeHistoryAccountFilter::All, terminal_cx);
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
            .highlighted(selected)
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
        .child(trade_history_cell(TRADE_HISTORY_TIME_WIDTH).child("Time"))
        .when(show_account, |row| {
            row.child(trade_history_cell(TRADE_HISTORY_ACCOUNT_WIDTH).child("Account"))
        })
        .child(trade_history_cell(TRADE_HISTORY_SYMBOL_WIDTH).child("Symbol"))
        .child(trade_history_cell(TRADE_HISTORY_SIDE_WIDTH).child("Side"))
        .child(
            trade_history_cell(TRADE_HISTORY_QUANTITY_WIDTH)
                .text_right()
                .child("Quantity"),
        )
        .child(
            trade_history_cell(TRADE_HISTORY_PRICE_WIDTH)
                .text_right()
                .child("Price"),
        )
        .child(
            trade_history_cell(TRADE_HISTORY_PNL_WIDTH)
                .text_right()
                .child("Realized P&L"),
        )
        .child(
            trade_history_cell(TRADE_HISTORY_PNL_WIDTH)
                .text_right()
                .child("Trade P&L"),
        )
}

fn trade_history_list(
    state: &BottomPanelState,
    chart: Option<Entity<AerisChartView>>,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    if state.history.rows.is_empty() {
        let message = if state.history.fills.is_empty() {
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
                    let fill = history.fills.get(*history.rows.get(index)?)?;
                    Some(trade_history_row(
                        index,
                        fill,
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
    fill: &aeris_trading::Fill,
    history: &TradeHistory,
    chart: Option<&AerisChartView>,
    show_account: bool,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let utc_seconds = fill.execution_unix_nanos.div_euclid(1_000_000_000);
    let time = chart.map_or_else(
        || "—".to_string(),
        |chart| chart.time_zone_date_time_label_at(utc_seconds),
    );
    let (side, side_color) = match fill.side {
        aeris_trading::OrderSide::Buy => ("Buy", colors.text_positive),
        aeris_trading::OrderSide::Sell => ("Sell", colors.text_negative),
    };
    let currency = history.account_currency(fill.account_id.as_str());
    let realized_pnl = history.fill_realized_pnl.get(&fill.id).copied();
    let trade_pnl = history.completed_trade_pnl.get(&fill.id).copied();
    trade_history_row_frame()
        .id(("trade_history_row", index))
        .hover(move |row| row.bg(gpui_color(colors.hover_bg)))
        .text_color(gpui_color(colors.text_default))
        .child(trade_history_cell(TRADE_HISTORY_TIME_WIDTH).child(time))
        .when(show_account, |row| {
            row.child(
                trade_history_cell(TRADE_HISTORY_ACCOUNT_WIDTH)
                    .child(history.account_name(fill.account_id.as_str())),
            )
        })
        .child(
            trade_history_cell(TRADE_HISTORY_SYMBOL_WIDTH)
                .child(history.symbol(&fill.instrument_id)),
        )
        .child(
            trade_history_cell(TRADE_HISTORY_SIDE_WIDTH)
                .text_color(gpui_color(side_color))
                .child(side),
        )
        .child(
            trade_history_cell(TRADE_HISTORY_QUANTITY_WIDTH)
                .text_right()
                .child(market_price_text(
                    fill.quantity.units(),
                    u32::from(fill.quantity.scale()),
                )),
        )
        .child(
            trade_history_cell(TRADE_HISTORY_PRICE_WIDTH)
                .text_right()
                .child(market_price_text(
                    fill.price.units(),
                    u32::from(fill.price.scale()),
                )),
        )
        .child(trade_history_pnl_cell(realized_pnl, currency, theme))
        .child(trade_history_pnl_cell(trade_pnl, currency, theme))
}

/// A signed P&L amount colored by its sign; fills that realized nothing show a dash.
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

    fn fill(id: &str, account: &str) -> aeris_trading::Fill {
        aeris_trading::Fill {
            id: aeris_trading::FillId::try_new(id).expect("fill id"),
            order_id: aeris_trading::OrderId::try_new(format!("order-{id}")).expect("order id"),
            account_id: aeris_trading::TradingAccountId::try_new(account).expect("account"),
            instrument_id: aeris_instruments::InstrumentId::try_new("hyperliquid:perp:BTC")
                .expect("instrument"),
            side: aeris_trading::OrderSide::Buy,
            price: aeris_trading::FixedPoint::try_new(8_309_000_000_000, 8).expect("price"),
            quantity: aeris_trading::FixedPoint::try_new(100_000_000, 8).expect("quantity"),
            execution_unix_nanos: 1_790_704_166_986_188_900,
            provenance: aeris_trading::TradingProvenance {
                venue_id: "aeris-sim".to_string(),
                provider_id: "hyperliquid".to_string(),
                session_generation: 1,
                source_sequence: 1,
                observed_unix_nanos: 1_790_704_166_986_188_900,
            },
        }
    }

    #[test]
    fn account_filter_keeps_newest_first_order_and_selects_one_account() {
        let fills = [fill("3", "b"), fill("2", "a"), fill("1", "b")];
        assert_eq!(
            trade_history_rows(&fills, &TradeHistoryAccountFilter::All),
            vec![0, 1, 2]
        );
        let only_b = TradeHistoryAccountFilter::Account("b".to_string());
        assert_eq!(trade_history_rows(&fills, &only_b), vec![0, 2]);
    }

    #[test]
    fn filter_for_a_deleted_account_falls_back_to_all_accounts() {
        let mut state = BottomPanelState::default();
        state.history.fills = vec![fill("1", "a"), fill("2", "b")].into();
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
    fn closing_fills_show_signed_pnl_and_other_fills_a_dash() {
        let loss = aeris_trading::FixedPoint::try_new(-252_500, 2).expect("loss");
        let gain = aeris_trading::FixedPoint::try_new(735_000, 2).expect("gain");
        assert_eq!(trade_history_pnl_text(Some(loss), "USD"), "USD -2525.00");
        assert_eq!(trade_history_pnl_text(Some(gain), "USD"), "USD 7350.00");
        assert_eq!(trade_history_pnl_text(None, "USD"), "—");
    }

    #[test]
    fn all_trade_history_columns_fit_a_compact_window() {
        let columns = [
            TRADE_HISTORY_TIME_WIDTH,
            TRADE_HISTORY_ACCOUNT_WIDTH,
            TRADE_HISTORY_SYMBOL_WIDTH,
            TRADE_HISTORY_SIDE_WIDTH,
            TRADE_HISTORY_QUANTITY_WIDTH,
            TRADE_HISTORY_PRICE_WIDTH,
            TRADE_HISTORY_PNL_WIDTH,
            TRADE_HISTORY_PNL_WIDTH,
        ];
        let width = columns.iter().sum::<f32>()
            + TRADE_HISTORY_CELL_GAP
                * f32::from(u8::try_from(columns.len() - 1).expect("column count fits"))
            + 16.0;
        assert!(
            width <= 800.0 - 12.0,
            "both P&L columns must remain visible"
        );
    }
}
