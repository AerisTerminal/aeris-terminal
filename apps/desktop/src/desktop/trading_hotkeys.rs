//! Keyboard trading: the held-key guard and the order confirmation that one-click trading skips.
//! Orders from a position drawing's toolbar share the confirmation. Clicks on the order ticket
//! never pass through here.

use super::chart_chrome::TradingShortcutMode;
use super::*;

/// A trading command sent by a shortcut or the command palette.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TradingHotkey {
    BuyMarket,
    SellMarket,
    CancelAll,
    FlattenAccount,
    KillSwitch,
}

impl TradingHotkey {
    const fn confirm_label(self) -> &'static str {
        match self {
            Self::BuyMarket => "Buy",
            Self::SellMarket => "Sell",
            Self::CancelAll => "Cancel orders",
            Self::FlattenAccount => "Flatten",
            Self::KillSwitch => "Lock accounts",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::BuyMarket => "Buy at market?",
            Self::SellMarket => "Sell at market?",
            Self::CancelAll => "Cancel all orders?",
            Self::FlattenAccount => "Flatten account?",
            Self::KillSwitch => "Lock every account?",
        }
    }

    /// Orders open risk; the other commands only remove it.
    const fn tone(self) -> ConfirmationTone {
        match self {
            Self::BuyMarket | Self::SellMarket => ConfirmationTone::Positive,
            Self::CancelAll | Self::FlattenAccount | Self::KillSwitch => {
                ConfirmationTone::Destructive
            }
        }
    }

    const fn side(self) -> Option<aeris_trading::OrderSide> {
        match self {
            Self::BuyMarket => Some(aeris_trading::OrderSide::Buy),
            Self::SellMarket => Some(aeris_trading::OrderSide::Sell),
            Self::CancelAll | Self::FlattenAccount | Self::KillSwitch => None,
        }
    }
}

/// Keyboard trading state for this run of the app.
#[derive(Default)]
pub(super) struct KeyboardTrading {
    /// Set when a shortcut runs and cleared by the next key release, so holding a chord sends
    /// once instead of once per key repeat.
    key_latched: bool,
    confirmation: Option<TradingConfirmation>,
}

impl KeyboardTrading {
    pub(super) const fn confirming(&self) -> bool {
        self.confirmation.is_some()
    }

    pub(super) fn release_key(&mut self) {
        self.key_latched = false;
    }

    /// Claims the shortcut for this key press. A repeat of a held key, or any trading key while
    /// the confirmation is open, gets `false`.
    fn claim_key(&mut self) -> bool {
        if self.key_latched || self.confirmation.is_some() {
            return false;
        }
        self.key_latched = true;
        true
    }
}

/// What an accepted confirmation sends.
enum ConfirmedTrade {
    Hotkey(TradingHotkey),
    /// The bracket a long or short position drawing describes, at the ticket quantity.
    DrawingBracket {
        chart: Entity<AerisChartView>,
        drawing: ChartDrawingId,
    },
}

impl ConfirmedTrade {
    const fn title(&self) -> &'static str {
        match self {
            Self::Hotkey(hotkey) => hotkey.title(),
            Self::DrawingBracket { .. } => "Place bracket order?",
        }
    }

    const fn confirm_label(&self) -> &'static str {
        match self {
            Self::Hotkey(hotkey) => hotkey.confirm_label(),
            Self::DrawingBracket { .. } => "Place order",
        }
    }

    const fn tone(&self) -> ConfirmationTone {
        match self {
            Self::Hotkey(hotkey) => hotkey.tone(),
            Self::DrawingBracket { .. } => ConfirmationTone::Positive,
        }
    }
}

pub(super) fn risk_increasing_trade_refusal(account: &str, reason: Option<&str>) -> Option<String> {
    reason.map(|reason| format!("{account} is locked: {reason}"))
}

struct TradingConfirmation {
    trade: ConfirmedTrade,
    message: String,
    skip_next_time: bool,
}

impl TerminalApp {
    pub(super) fn toggle_one_click_trading(
        &mut self,
        _: &ToggleOneClickTrading,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mode = if self.chrome.preferences().trading_shortcuts.one_click() {
            TradingShortcutMode::Confirm
        } else {
            TradingShortcutMode::OneClick
        };
        self.set_trading_shortcut_mode(mode, cx);
    }

    fn set_trading_shortcut_mode(&mut self, mode: TradingShortcutMode, cx: &mut Context<Self>) {
        self.chrome.preferences_mut().trading_shortcuts = mode;
        // Surfaces save their own copy of the chrome preferences, so each one must carry the
        // new value or its next save would restore the old one.
        for workspace in self.tabs.iter() {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, _| {
                    surface.chart_chrome.trading_shortcuts = mode;
                });
            }
        }
        self.save_chart_chrome_preferences(cx);
        aeris_desktop::trading::record_notice(
            self.lifecycle.trading_feedback(),
            Ok(if mode.one_click() {
                "One-click trading on: shortcuts send without confirmation".to_string()
            } else {
                "One-click trading off: shortcuts ask for confirmation".to_string()
            }),
        );
        cx.notify();
    }

    /// Runs a trading shortcut. Returns `false` when the key is not for trading here (another
    /// page or a field outside the workspace has focus), so the key can reach that target.
    pub(super) fn run_trading_hotkey(
        &mut self,
        hotkey: TradingHotkey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.trading_hotkeys_enabled(window, cx) {
            return false;
        }
        if !self.keyboard_trading.claim_key() {
            return true;
        }
        let message = match self.trading_hotkey_summary(hotkey, cx) {
            Ok(message) => message,
            Err(reason) => {
                let pane = self.active_surface().read(cx).trading_pane_token.clone();
                self.lifecycle
                    .trading_feedback()
                    .record_for_pane(&pane, Err(reason));
                cx.notify();
                return true;
            }
        };
        self.confirm_or_send_trade(ConfirmedTrade::Hotkey(hotkey), message, window, cx);
        true
    }

    /// Places the bracket of a long or short position drawing on the active chart at the
    /// ticket quantity, after the same confirmation as keyboard orders.
    pub(super) fn request_drawing_bracket_order(
        &mut self,
        chart: Entity<AerisChartView>,
        drawing: ChartDrawingId,
        long: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.keyboard_trading.confirming() {
            return;
        }
        match self.drawing_bracket_summary(long, cx) {
            Ok(message) => self.confirm_or_send_trade(
                ConfirmedTrade::DrawingBracket { chart, drawing },
                message,
                window,
                cx,
            ),
            Err(reason) => {
                let pane = self.active_surface().read(cx).trading_pane_token.clone();
                self.lifecycle
                    .trading_feedback()
                    .record_for_pane(&pane, Err(reason));
                cx.notify();
            }
        }
    }

    /// One-click trading sends at once; otherwise the confirmation opens.
    fn confirm_or_send_trade(
        &mut self,
        trade: ConfirmedTrade,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.chrome.preferences().trading_shortcuts.one_click() {
            self.send_confirmed_trade(&trade, cx);
        } else {
            self.keyboard_trading.confirmation = Some(TradingConfirmation {
                trade,
                message,
                skip_next_time: false,
            });
            // Enter and Escape answer the dialog from the shell, ahead of a focused chart.
            self.window.chrome_focus().focus(window, cx);
        }
        cx.notify();
    }

    fn selected_trading_account_name(&self, cx: &App) -> Option<String> {
        let surface = self.active_surface();
        let surface = surface.read(cx);
        let trading = &surface.trading_pnl;
        let store = self.services.trading_store().read(cx);
        trading
            .order_entry
            .selected_account_id
            .as_ref()
            .and_then(|id| {
                store
                    .current()?
                    .accounts
                    .iter()
                    .find(|account| &account.id == id)
            })
            .map(|account| account.display_name.clone())
    }

    fn drawing_bracket_summary(&self, long: bool, cx: &App) -> Result<String, String> {
        let account = self
            .selected_trading_account_name(cx)
            .ok_or("Choose a trading account first")?;
        let surface = self.active_surface();
        let surface = surface.read(cx);
        let trading = &surface.trading_pnl;
        if let Some(message) = risk_increasing_trade_refusal(
            &account,
            surface.selected_account_lock_reason(cx).as_deref(),
        ) {
            return Err(message);
        }
        let frame = self
            .trading_order_frame(cx)
            .ok_or("No order book for this instrument yet")?;
        let side = if long { "Buy" } else { "Sell" };
        Ok(format!(
            "{side} {} {} when price reaches the drawing's entry, with its stop and target, on {account}.",
            trading.order_entry.quantity, frame.instrument_id
        ))
    }

    /// What the command will do, or why it cannot run now.
    fn trading_hotkey_summary(&self, hotkey: TradingHotkey, cx: &App) -> Result<String, String> {
        let surface = self.active_surface();
        let surface = surface.read(cx);
        let trading = &surface.trading_pnl;
        let account = self.selected_trading_account_name(cx);
        match hotkey {
            TradingHotkey::BuyMarket | TradingHotkey::SellMarket => {
                let account = account.ok_or("Choose a trading account first")?;
                // Risk-reducing commands stay available on a locked account; new orders do not.
                if let Some(message) = risk_increasing_trade_refusal(
                    &account,
                    surface.selected_account_lock_reason(cx).as_deref(),
                ) {
                    return Err(message);
                }
                let frame = self
                    .trading_order_frame(cx)
                    .ok_or("No order book for this instrument yet")?;
                let side = hotkey.confirm_label();
                let quantity = trading.order_entry.quantity;
                Ok(format!(
                    "{side} {quantity} {} at market on {account}.",
                    frame.instrument_id
                ))
            }
            TradingHotkey::CancelAll => {
                Ok("Cancel every working order on all practice accounts.".to_string())
            }
            TradingHotkey::FlattenAccount => {
                let account = account.ok_or("Choose a trading account first")?;
                Ok(format!(
                    "Cancel working orders and close every position on {account} at the trading owner's current quotes."
                ))
            }
            TradingHotkey::KillSwitch => Ok(
                "Lock every practice account and cancel its working orders. Accounts stay locked until you unlock them."
                    .to_string(),
            ),
        }
    }

    fn send_confirmed_trade(&self, trade: &ConfirmedTrade, cx: &mut Context<Self>) {
        match trade {
            ConfirmedTrade::Hotkey(hotkey) => self.send_trading_hotkey(*hotkey, cx),
            ConfirmedTrade::DrawingBracket { chart, drawing } => {
                if self.trading_order_entry_locked(cx) {
                    return;
                }
                let quantity = self.trading_order_entry(cx).1;
                // Aeris Charts turns the drawing into a bracket intent, which the chart's pane
                // dispatches like any other chart order.
                let placed = match quantity.chart_size() {
                    Some(quantity) => chart.update(cx, |chart, chart_cx| {
                        let placed = chart.place_bracket_order_from_drawing(*drawing, quantity);
                        chart_cx.notify();
                        placed
                    }),
                    None => Err(ChartIntentRejection::InvalidQuantity.message().to_string()),
                };
                if let Err(reason) = placed {
                    let pane = self.active_surface().read(cx).trading_pane_token.clone();
                    self.lifecycle
                        .trading_feedback()
                        .record_for_pane(&pane, Err(reason));
                }
            }
        }
    }

    fn send_trading_hotkey(&self, hotkey: TradingHotkey, cx: &mut Context<Self>) {
        let pane = self.active_surface().read(cx).trading_pane_token.clone();
        match hotkey {
            TradingHotkey::BuyMarket | TradingHotkey::SellMarket => {
                if self.trading_order_entry_locked(cx) {
                    return;
                }
                let (Some(product), Some(side)) = (
                    self.active_surface().read(cx).product.clone(),
                    hotkey.side(),
                ) else {
                    return;
                };
                let (account_id, quantity, order_type, time_in_force) =
                    self.trading_order_entry(cx);
                aeris_desktop::trading::dispatch_simulated_order(
                    self.lifecycle.trading(),
                    self.lifecycle.trading_feedback(),
                    &pane,
                    &product,
                    side,
                    aeris_desktop::trading::SimulatedOrderSelection {
                        account_key: account_id,
                        quantity,
                        order_type,
                        time_in_force,
                        template_id: None,
                    },
                    cx,
                );
            }
            TradingHotkey::CancelAll => {
                aeris_desktop::trading::cancel_simulated_accounts(
                    self.lifecycle.trading(),
                    self.lifecycle.trading_feedback(),
                    &pane,
                    cx,
                );
            }
            TradingHotkey::FlattenAccount => {
                aeris_desktop::trading::flatten_simulated_account(
                    self.lifecycle.trading(),
                    self.lifecycle.trading_feedback(),
                    &pane,
                    self.trading_order_entry(cx).0,
                    cx,
                );
            }
            TradingHotkey::KillSwitch => {
                aeris_desktop::trading::kill_simulated_accounts(
                    self.lifecycle.trading(),
                    self.lifecycle.trading_feedback(),
                    &pane,
                    cx,
                );
            }
        }
    }

    /// Sends the confirmed command against the workspace as it is now. Choosing "Don't ask
    /// again" turns on one-click trading.
    pub(super) fn confirm_trading_hotkey(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(confirmation) = self.keyboard_trading.confirmation.take() else {
            return;
        };
        if confirmation.skip_next_time {
            self.set_trading_shortcut_mode(TradingShortcutMode::OneClick, cx);
        }
        self.send_confirmed_trade(&confirmation.trade, cx);
        self.focus_workspace(window, cx);
        cx.notify();
    }

    pub(super) fn cancel_trading_hotkey(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.keyboard_trading.confirmation.take().is_some() {
            self.focus_workspace(window, cx);
            cx.notify();
        }
    }

    fn set_trading_confirmation_skip(&mut self, skip: bool, cx: &mut Context<Self>) {
        if let Some(confirmation) = &mut self.keyboard_trading.confirmation {
            confirmation.skip_next_time = skip;
            cx.notify();
        }
    }

    /// Enter confirms and Escape cancels while the dialog is open. A held Enter does not
    /// confirm, so the key that opened the dialog cannot also answer it.
    pub(super) fn trading_confirmation_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.keyboard_trading.confirming() {
            return false;
        }
        match event.keystroke.key.as_str() {
            "enter" if !event.is_held => self.confirm_trading_hotkey(window, cx),
            "escape" => self.cancel_trading_hotkey(window, cx),
            _ => return false,
        }
        true
    }

    pub(super) fn trading_confirmation_layer(&self, terminal: &Entity<Self>) -> Option<AnyElement> {
        let confirmation = self.keyboard_trading.confirmation.as_ref()?;
        let trade = &confirmation.trade;
        let cancel = terminal.clone();
        let confirm = terminal.clone();
        let skip = terminal.clone();
        Some(
            ConfirmationDialog::new(
                "trading_hotkey_confirmation",
                trade.title(),
                trade.tone(),
                &self.theme,
                move |window, cx| {
                    cancel.update(cx, |terminal, terminal_cx| {
                        terminal.cancel_trading_hotkey(window, terminal_cx);
                    });
                },
                move |window, cx| {
                    confirm.update(cx, |terminal, terminal_cx| {
                        terminal.confirm_trading_hotkey(window, terminal_cx);
                    });
                },
            )
            .message(confirmation.message.clone())
            .confirm_label(trade.confirm_label())
            .child(
                SwitchRow::new(
                    "trading_hotkey_skip_confirmation",
                    "Don't ask me again",
                    confirmation.skip_next_time,
                    &self.theme,
                )
                .description(
                    "Turns on one-click trading: shortcuts and drawing orders send at once. Turn it off from the command palette.",
                )
                .on_change(move |checked, _, _, cx| {
                    skip.update(cx, |terminal, terminal_cx| {
                        terminal.set_trading_confirmation_skip(checked, terminal_cx);
                    });
                }),
            )
            .into_any_element(),
        )
    }
}
