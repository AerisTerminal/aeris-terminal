//! Keyboard trading: the per-session arm switch, the held-key guard, and the order confirmation
//! that one-click trading skips. Clicks on the order ticket are not keyboard trading and never
//! pass through here.

use super::chart_chrome::TradingShortcutMode;
use super::*;
use aeris_desktop::command_registry::{self, CommandId};

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

/// Keyboard trading state for this run of the app. Nothing here is saved, so every launch
/// starts disarmed.
#[derive(Default)]
pub(super) struct KeyboardTrading {
    armed: bool,
    /// Set when a shortcut runs and cleared by the next key release, so holding a chord sends
    /// once instead of once per key repeat.
    key_latched: bool,
    confirmation: Option<TradingConfirmation>,
}

impl KeyboardTrading {
    pub(super) const fn armed(&self) -> bool {
        self.armed
    }

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

struct TradingConfirmation {
    hotkey: TradingHotkey,
    message: String,
    skip_next_time: bool,
}

/// The title-bar state of keyboard trading.
#[derive(Clone, Copy)]
pub(super) struct KeyboardTradingIndicator {
    pub(super) armed: bool,
    pub(super) one_click: bool,
}

fn arm_shortcut_label() -> String {
    command_registry::command(CommandId::ToggleTradingArmed)
        .shortcut_label()
        .unwrap_or_else(|| "the command palette".to_string())
}

impl TerminalApp {
    pub(super) const fn keyboard_trading_indicator(&self) -> KeyboardTradingIndicator {
        KeyboardTradingIndicator {
            armed: self.keyboard_trading.armed(),
            one_click: self.chart_chrome.trading_shortcuts.one_click(),
        }
    }

    pub(super) fn toggle_trading_armed(
        &mut self,
        _: &ToggleTradingArmed,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.keyboard_trading.claim_key() {
            self.set_trading_armed(!self.keyboard_trading.armed, cx);
        }
    }

    pub(super) fn set_trading_armed(&mut self, armed: bool, cx: &mut Context<Self>) {
        self.keyboard_trading.armed = armed;
        if !armed {
            self.keyboard_trading.confirmation = None;
        }
        aeris_desktop::trading::record_notice(Ok(if armed {
            "Keyboard trading armed for this session".to_string()
        } else {
            "Keyboard trading off".to_string()
        }));
        cx.notify();
    }

    pub(super) fn toggle_one_click_trading(
        &mut self,
        _: &ToggleOneClickTrading,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mode = if self.chart_chrome.trading_shortcuts.one_click() {
            TradingShortcutMode::Confirm
        } else {
            TradingShortcutMode::OneClick
        };
        self.set_trading_shortcut_mode(mode, cx);
    }

    fn set_trading_shortcut_mode(&mut self, mode: TradingShortcutMode, cx: &mut Context<Self>) {
        self.chart_chrome.trading_shortcuts = mode;
        // Surfaces save their own copy of the chrome preferences, so each one must carry the
        // new value or its next save would restore the old one.
        for workspace in &self.workspaces {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, _| {
                    surface.chart_chrome.trading_shortcuts = mode;
                });
            }
        }
        self.save_chart_chrome_preferences(cx);
        aeris_desktop::trading::record_notice(Ok(if mode.one_click() {
            "One-click trading on: shortcuts send without confirmation".to_string()
        } else {
            "One-click trading off: shortcuts ask for confirmation".to_string()
        }));
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
        if !self.keyboard_trading.armed {
            aeris_desktop::trading::record_notice(Err(format!(
                "Keyboard trading is off. Press {} to arm it for this session.",
                arm_shortcut_label()
            )));
            cx.notify();
            return true;
        }
        let message = match self.trading_hotkey_summary(hotkey, cx) {
            Ok(message) => message,
            Err(reason) => {
                aeris_desktop::trading::record_notice(Err(reason));
                cx.notify();
                return true;
            }
        };
        if self.chart_chrome.trading_shortcuts.one_click() {
            self.send_trading_hotkey(hotkey, cx);
        } else {
            self.keyboard_trading.confirmation = Some(TradingConfirmation {
                hotkey,
                message,
                skip_next_time: false,
            });
            // Enter and Escape answer the dialog from the shell, ahead of a focused chart.
            self.chrome_focus.focus(window, cx);
        }
        cx.notify();
        true
    }

    /// What the command will do, or why it cannot run now.
    fn trading_hotkey_summary(&self, hotkey: TradingHotkey, cx: &App) -> Result<String, String> {
        let surface = self.active_surface();
        let trading = &surface.read(cx).trading_pnl;
        let account = trading
            .order_entry
            .selected_account_id
            .as_ref()
            .and_then(|id| trading.accounts.iter().find(|account| &account.id == id))
            .map(|account| account.display_name.clone());
        match hotkey {
            TradingHotkey::BuyMarket | TradingHotkey::SellMarket => {
                let account = account.ok_or("Choose a trading account first")?;
                // Risk-reducing commands stay available on a locked account; new orders do not.
                if let Some(reason) = super::selected_account_lock_reason(trading) {
                    return Err(format!("{account} is locked: {reason}"));
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
                self.trading_order_frame(cx)
                    .ok_or("No order book for this instrument yet")?;
                Ok(format!(
                    "Cancel working orders and close every position on {account} at the current bid and ask."
                ))
            }
            TradingHotkey::KillSwitch => Ok(
                "Lock every practice account and cancel its working orders. Accounts stay locked until you unlock them."
                    .to_string(),
            ),
        }
    }

    fn send_trading_hotkey(&self, hotkey: TradingHotkey, cx: &mut Context<Self>) {
        match hotkey {
            TradingHotkey::BuyMarket | TradingHotkey::SellMarket => {
                if self.trading_order_entry_locked(cx) {
                    return;
                }
                let (Some(frame), Some(side)) = (self.trading_order_frame(cx), hotkey.side())
                else {
                    return;
                };
                let (account_id, quantity, order_type, time_in_force) =
                    self.trading_order_entry(cx);
                aeris_desktop::trading::dispatch_simulated_order(
                    &frame,
                    side,
                    account_id,
                    quantity,
                    order_type,
                    time_in_force,
                    cx,
                );
            }
            TradingHotkey::CancelAll => aeris_desktop::trading::cancel_simulated_accounts(cx),
            TradingHotkey::FlattenAccount => {
                if let Some(frame) = self.trading_order_frame(cx) {
                    aeris_desktop::trading::flatten_simulated_account_for(
                        &frame,
                        self.trading_order_entry(cx).0,
                        cx,
                    );
                }
            }
            TradingHotkey::KillSwitch => aeris_desktop::trading::kill_simulated_accounts(cx),
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
        if self.keyboard_trading.armed {
            self.send_trading_hotkey(confirmation.hotkey, cx);
        }
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
        let hotkey = confirmation.hotkey;
        let cancel = terminal.clone();
        let confirm = terminal.clone();
        let skip = terminal.clone();
        Some(
            ConfirmationDialog::new(
                "trading_hotkey_confirmation",
                hotkey.title(),
                hotkey.tone(),
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
            .confirm_label(hotkey.confirm_label())
            .child(
                SwitchRow::new(
                    "trading_hotkey_skip_confirmation",
                    "Don't ask me again",
                    confirmation.skip_next_time,
                    &self.theme,
                )
                .description(
                    "Turns on one-click trading: shortcuts send at once. Turn it off from the command palette.",
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

/// The title-bar switch for keyboard trading. It shows whether shortcuts can trade and arms or
/// disarms them for this session.
pub(super) fn keyboard_trading_toggle(
    terminal: &Entity<TerminalApp>,
    indicator: KeyboardTradingIndicator,
    theme: &AerisTheme,
) -> Div {
    let colors = theme.colors;
    let label = match indicator {
        KeyboardTradingIndicator { armed: false, .. } => "Hotkeys off",
        KeyboardTradingIndicator {
            armed: true,
            one_click: false,
        } => "Hotkeys armed",
        KeyboardTradingIndicator {
            armed: true,
            one_click: true,
        } => "Hotkeys armed · one-click",
    };
    let toggle = terminal.clone();
    let armed = indicator.armed;
    div()
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .px_2()
        .child(
            Button::new("keyboard_trading_toggle", theme)
                .variant(ButtonVariant::Ghost)
                .button_size(ButtonSize::Sm)
                .selected(armed)
                .leading(div().size(px(6.0)).rounded_full().bg(gpui_color(if armed {
                    colors.danger
                } else {
                    colors.text_muted
                })))
                .label(label)
                .aria_label(if armed {
                    "Disarm keyboard trading"
                } else {
                    "Arm keyboard trading"
                })
                .tooltip(
                    TooltipSpec::new(
                        format!(
                            "Keyboard trading for this session ({})",
                            arm_shortcut_label()
                        ),
                        theme,
                    )
                    .show_delay(TOOLTIP_OPEN_DELAY),
                )
                .on_click(move |_, window, cx| {
                    toggle.update(cx, |terminal, terminal_cx| {
                        terminal.set_trading_armed(!armed, terminal_cx);
                        // The button took focus; shortcuts trade only from the workspace.
                        terminal.focus_workspace(window, terminal_cx);
                    });
                }),
        )
}
