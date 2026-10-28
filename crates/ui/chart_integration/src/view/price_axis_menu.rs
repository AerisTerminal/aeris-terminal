//! The Y-axis menu: Aeris Charts-owned price-scale chrome read and changed through engine APIs.

use super::AerisChartView;
use aeris_charts_engine::{PriceScaleMode, PriceScaleTarget, SeriesChromeFlag};

/// Aeris Charts-owned price-axis chrome the Y-axis menu presents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PriceAxisMenuState {
    pub flags: u16,
    pub mode: u8,
    pub left: bool,
    pub precision: Option<u8>,
}

impl PriceAxisMenuState {
    pub const PRICE_LINE: u16 = 1;
    pub const LAST_VALUE: u16 = 2;
    pub const TITLE: u16 = 4;
    pub const COUNTDOWN: u16 = 8;
    pub const INDICATOR_NAMES: u16 = 16;
    pub const INDICATOR_VALUES: u16 = 32;
    pub const AUTO_SCALE: u16 = 64;
    pub const INVERT_SCALE: u16 = 128;
    pub const BID_ASK: u16 = 256;
    pub const ALIGN_LABELS: u16 = 512;
    pub const INDICATOR_PRICE_LINES: u16 = 1024;

    #[must_use]
    pub const fn enabled(self, flag: u16) -> bool {
        self.flags & flag != 0
    }
}

/// A user command from the Y-axis menu.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PriceAxisMenuAction {
    TogglePriceLine,
    ToggleLastValue,
    ToggleTitle,
    ToggleCountdown,
    ToggleIndicatorNameLabels,
    ToggleIndicatorValueLabels,
    ToggleIndicatorPriceLines,
    ToggleBidAsk,
    ToggleAlignLabels,
    ToggleAutoScale,
    ToggleInvertScale,
    SetMode(u8),
    SetLeft(bool),
    SetPrecision(Option<u8>),
}

const PRICE_AXIS_PRECISION_CHOICES: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 8];

fn price_axis_target(left: bool) -> PriceScaleTarget {
    if left {
        PriceScaleTarget::Left
    } else {
        PriceScaleTarget::Right
    }
}

pub(super) fn price_format_min_move(precision: u8) -> f64 {
    10_f64.powi(-i32::from(precision.min(18)))
}

const fn price_scale_mode_code(mode: PriceScaleMode) -> u8 {
    match mode {
        PriceScaleMode::Normal => 0,
        PriceScaleMode::Logarithmic => 1,
        PriceScaleMode::Percentage => 2,
        PriceScaleMode::IndexedTo100 => 3,
    }
}

impl AerisChartView {
    /// Reads Aeris Charts-owned Y-axis chrome for the hit-tested price scale.
    #[must_use]
    pub fn price_axis_menu_state(&self, pane: usize, left: bool) -> Option<PriceAxisMenuState> {
        let target = price_axis_target(left);
        let primary = self.engine.primary_series_on_price_scale(pane, target)?;
        let mut flags = 0;
        if self.product_price_line_visible() {
            flags |= PriceAxisMenuState::PRICE_LINE;
        }
        if primary.last_value_visible {
            flags |= PriceAxisMenuState::LAST_VALUE;
        }
        if primary.title_visible {
            flags |= PriceAxisMenuState::TITLE;
        }
        if primary.countdown_visible {
            flags |= PriceAxisMenuState::COUNTDOWN;
        }
        let chrome = self.engine.indicator_chrome_options();
        if chrome.name_labels_visible {
            flags |= PriceAxisMenuState::INDICATOR_NAMES;
        }
        if chrome.value_labels_visible {
            flags |= PriceAxisMenuState::INDICATOR_VALUES;
        }
        if chrome.price_lines_visible {
            flags |= PriceAxisMenuState::INDICATOR_PRICE_LINES;
        }
        if self.engine.price_scale_auto_scale_for(pane, target)? {
            flags |= PriceAxisMenuState::AUTO_SCALE;
        }
        if self.engine.price_scale_inverted_for(pane, target)? {
            flags |= PriceAxisMenuState::INVERT_SCALE;
        }
        if primary.bid_ask_visible {
            flags |= PriceAxisMenuState::BID_ASK;
        }
        if self.engine.price_scale_align_labels_for(pane, target)? {
            flags |= PriceAxisMenuState::ALIGN_LABELS;
        }
        Some(PriceAxisMenuState {
            flags,
            mode: price_scale_mode_code(self.engine.price_scale_mode_for(pane, target)?),
            left,
            precision: self.price_precision_override,
        })
    }

    /// Returns the explicit main price-axis precision selected by the host.
    ///
    /// `None` means the chart is intentionally following the instrument's automatic precision.
    #[must_use]
    pub const fn selected_price_precision(&self) -> Option<u8> {
        self.price_precision_override
    }

    /// Applies one Y-axis menu command through Aeris Charts' scale and series APIs.
    pub fn apply_price_axis_menu_action(
        &mut self,
        pane: usize,
        left: bool,
        action: PriceAxisMenuAction,
    ) -> bool {
        let target = price_axis_target(left);
        let Some(primary_id) = self.primary_series_id_on_scale(pane, target) else {
            return false;
        };
        let applied = match action {
            PriceAxisMenuAction::TogglePriceLine => self
                .engine
                .toggle_series_chrome(0, SeriesChromeFlag::PriceLine),
            PriceAxisMenuAction::ToggleLastValue => self
                .engine
                .toggle_series_chrome(primary_id, SeriesChromeFlag::LastValue),
            PriceAxisMenuAction::ToggleTitle => self
                .engine
                .toggle_series_chrome(primary_id, SeriesChromeFlag::Title),
            PriceAxisMenuAction::ToggleCountdown => self
                .engine
                .toggle_series_chrome(primary_id, SeriesChromeFlag::Countdown),
            PriceAxisMenuAction::ToggleIndicatorNameLabels => self.toggle_indicator_name_labels(),
            PriceAxisMenuAction::ToggleIndicatorValueLabels => self.toggle_indicator_value_labels(),
            PriceAxisMenuAction::ToggleIndicatorPriceLines => self.toggle_indicator_price_lines(),
            PriceAxisMenuAction::ToggleBidAsk => self
                .engine
                .toggle_series_chrome(primary_id, SeriesChromeFlag::BidAsk),
            PriceAxisMenuAction::ToggleAlignLabels => {
                self.engine.toggle_price_scale_align_labels(pane, target)
            }
            PriceAxisMenuAction::ToggleAutoScale => {
                let enabled = self.engine.price_scale_auto_scale_for(pane, target) != Some(true);
                self.engine
                    .set_price_scale_auto_scale_for(pane, target, enabled);
                true
            }
            PriceAxisMenuAction::ToggleInvertScale => {
                let inverted = self.engine.price_scale_inverted_for(pane, target) != Some(true);
                self.engine
                    .set_price_scale_inverted_for(pane, target, inverted);
                true
            }
            PriceAxisMenuAction::SetMode(mode) if mode <= 3 => {
                let mode = match mode {
                    1 => PriceScaleMode::Logarithmic,
                    2 => PriceScaleMode::Percentage,
                    3 => PriceScaleMode::IndexedTo100,
                    _ => PriceScaleMode::Normal,
                };
                self.engine.set_price_scale_mode_for(pane, target, mode);
                true
            }
            PriceAxisMenuAction::SetMode(_) => false,
            PriceAxisMenuAction::SetLeft(next_left) => self.move_price_axis(pane, left, next_left),
            PriceAxisMenuAction::SetPrecision(precision) => self.set_price_precision(precision),
        };
        if applied {
            self.mark_user_state_changed();
            self.invalidate_series_layout();
        }
        applied
    }

    /// Restores a previously captured stable price-axis state by replaying the
    /// same public actions used by the menu, so Aeris Charts remains authoritative.
    pub fn restore_price_axis_menu_state(&mut self, desired: PriceAxisMenuState) -> bool {
        let mut current = self
            .price_axis_menu_state(0, false)
            .or_else(|| self.price_axis_menu_state(0, true));
        let Some(mut current_state) = current else {
            return false;
        };
        if current_state.left != desired.left {
            let _ = self.apply_price_axis_menu_action(
                0,
                current_state.left,
                PriceAxisMenuAction::SetLeft(desired.left),
            );
            current = self.price_axis_menu_state(0, desired.left);
            let Some(next) = current else {
                return false;
            };
            current_state = next;
        }
        let flags = [
            (
                PriceAxisMenuState::PRICE_LINE,
                PriceAxisMenuAction::TogglePriceLine,
            ),
            (
                PriceAxisMenuState::LAST_VALUE,
                PriceAxisMenuAction::ToggleLastValue,
            ),
            (PriceAxisMenuState::TITLE, PriceAxisMenuAction::ToggleTitle),
            (
                PriceAxisMenuState::COUNTDOWN,
                PriceAxisMenuAction::ToggleCountdown,
            ),
            (
                PriceAxisMenuState::INDICATOR_NAMES,
                PriceAxisMenuAction::ToggleIndicatorNameLabels,
            ),
            (
                PriceAxisMenuState::INDICATOR_VALUES,
                PriceAxisMenuAction::ToggleIndicatorValueLabels,
            ),
            (
                PriceAxisMenuState::INDICATOR_PRICE_LINES,
                PriceAxisMenuAction::ToggleIndicatorPriceLines,
            ),
            // AUTO_SCALE is deliberately not replayed: the manual price range it implies is
            // not part of this state, so disabling it on a new chart leaves no range to draw.
            (
                PriceAxisMenuState::INVERT_SCALE,
                PriceAxisMenuAction::ToggleInvertScale,
            ),
            (
                PriceAxisMenuState::BID_ASK,
                PriceAxisMenuAction::ToggleBidAsk,
            ),
            (
                PriceAxisMenuState::ALIGN_LABELS,
                PriceAxisMenuAction::ToggleAlignLabels,
            ),
        ];
        for (flag, action) in flags {
            if current_state.enabled(flag) != desired.enabled(flag) {
                let _ = self.apply_price_axis_menu_action(0, desired.left, action);
            }
        }
        if current_state.mode != desired.mode {
            let _ = self.apply_price_axis_menu_action(
                0,
                desired.left,
                PriceAxisMenuAction::SetMode(desired.mode),
            );
        }
        if current_state.precision != desired.precision {
            let _ = self.apply_price_axis_menu_action(
                0,
                desired.left,
                PriceAxisMenuAction::SetPrecision(desired.precision),
            );
        }
        true
    }

    fn move_price_axis(&mut self, pane: usize, from_left: bool, to_left: bool) -> bool {
        if from_left == to_left {
            return true;
        }
        let from = price_axis_target(from_left);
        let to = price_axis_target(to_left);
        self.engine.rebind_price_scale_series(pane, from, to)
    }

    fn set_price_precision(&mut self, precision: Option<u8>) -> bool {
        if let Some(digits) = precision
            && !PRICE_AXIS_PRECISION_CHOICES.contains(&digits)
        {
            return false;
        }
        self.price_precision_override = precision;
        self.apply_selected_price_format();
        true
    }
}
