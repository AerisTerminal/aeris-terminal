//! The pane's chrome: which header menu is open and how its rows are being navigated, the
//! trigger anchors menus open from, the Accounts panel section and placement, and the docked
//! side-panel layout.

use super::accounts_panel::AccountsTab;
use super::chart_context_menus::FloatingPanelPlacement;
use super::{
    Bounds, Pixels, ProviderId, SIDE_PANEL_INITIAL_WIDTH, SidePanel, SidePanelVisibility,
    TimeframeMenuGroup, clamped_side_panel_width,
};
use num_traits::ToPrimitive;

const DEFAULT_SIDE_PANEL_SPLIT_BASIS_POINTS: u32 = 5_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChromeOverlay {
    Instrument,
    Indicator,
    Timeframe,
    QuickTimeframe,
    ChartType,
    TimeZone,
    Accounts,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ChromeOverlayPhase {
    #[default]
    Opening,
    Closing,
}

/// A header control whose measured bounds anchor the menu it opens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChromeTrigger {
    /// Shared by the timeframe menu and the quick-timeframe popup.
    Timeframe,
    ChartType,
    TimeZone,
    Accounts,
}

/// How the highlighted row of an open list was last chosen.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum MenuNavigation {
    #[default]
    Pointer,
    Keyboard,
}

impl MenuNavigation {
    const fn from_keyboard(keyboard: bool) -> Self {
        if keyboard {
            Self::Keyboard
        } else {
            Self::Pointer
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum MenuVisibility {
    #[default]
    Closed,
    Open,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SymbolProviderMenu {
    #[default]
    Closed,
    Open,
    /// Open with a provider's exchange and category flyout shown beside it.
    Flyout(ProviderId),
}

#[derive(Clone, Copy, Debug)]
struct OpenOverlay {
    kind: ChromeOverlay,
    phase: ChromeOverlayPhase,
    trigger_position: Option<gpui::Point<Pixels>>,
    list_navigation: MenuNavigation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TimeframeFlyout {
    group: TimeframeMenuGroup,
    navigation: MenuNavigation,
}

/// What showing a timeframe group's flyout changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TimeframeFlyoutChange {
    /// A different group's flyout opened, so its highlighted row needs choosing.
    Opened,
    /// The same flyout stayed open and only its navigation mode changed.
    Navigation,
    Unchanged,
}

/// A rejected quick-timeframe submission. The rejected text is kept so only a real edit
/// clears the error: the input also reports a change when Enter submits unchanged text.
struct QuickTimeframeError {
    query: String,
    message: String,
}

#[derive(Clone, Copy, Debug, Default)]
struct TriggerBounds {
    timeframe: Option<Bounds<Pixels>>,
    chart_type: Option<Bounds<Pixels>>,
    time_zone: Option<Bounds<Pixels>>,
    accounts: Option<Bounds<Pixels>>,
}

impl TriggerBounds {
    const fn slot(&mut self, trigger: ChromeTrigger) -> &mut Option<Bounds<Pixels>> {
        match trigger {
            ChromeTrigger::Timeframe => &mut self.timeframe,
            ChromeTrigger::ChartType => &mut self.chart_type,
            ChromeTrigger::TimeZone => &mut self.time_zone,
            ChromeTrigger::Accounts => &mut self.accounts,
        }
    }

    const fn get(&self, trigger: ChromeTrigger) -> Option<Bounds<Pixels>> {
        match trigger {
            ChromeTrigger::Timeframe => self.timeframe,
            ChromeTrigger::ChartType => self.chart_type,
            ChromeTrigger::TimeZone => self.time_zone,
            ChromeTrigger::Accounts => self.accounts,
        }
    }
}

/// The docked side-panel size one pane hands to another with its panels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SidePanelSize {
    width: f32,
    split_basis_points: u32,
}

impl SidePanelSize {
    /// A persisted size: zero means unset, and out-of-range values are clamped.
    fn from_persisted(width: u32, split_basis_points: u32) -> Self {
        Self {
            width: clamped_side_panel_width(
                width
                    .to_f32()
                    .filter(|width| *width > 0.0)
                    .unwrap_or(SIDE_PANEL_INITIAL_WIDTH),
            ),
            split_basis_points: if split_basis_points == 0 {
                DEFAULT_SIDE_PANEL_SPLIT_BASIS_POINTS
            } else {
                split_basis_points.clamp(500, 9_500)
            },
        }
    }
}

impl Default for SidePanelSize {
    fn default() -> Self {
        Self {
            width: SIDE_PANEL_INITIAL_WIDTH,
            split_basis_points: DEFAULT_SIDE_PANEL_SPLIT_BASIS_POINTS,
        }
    }
}

/// Owns the pane's chrome state. Mutators report whether anything changed so the owning
/// surface decides on notifying, on persistence and on any animation timer.
#[derive(Default)]
pub(super) struct PaneChromeState {
    overlay: Option<OpenOverlay>,
    /// Advances on every open and every animated close, so a stale close timer is ignored.
    overlay_generation: u64,
    selection: usize,
    timeframe_flyout: Option<TimeframeFlyout>,
    /// Advances whenever the pointer re-enters the timeframe menu, cancelling a pending close.
    timeframe_flyout_close_token: u64,
    /// The timeframe menu and flyout regions the pointer is inside.
    timeframe_hover_regions: u32,
    trigger_bounds: TriggerBounds,
    /// The Accounts panel section shown when it opens; kept across openings.
    accounts_tab: AccountsTab,
    /// Where the Accounts panel was dragged; every fresh open starts centered.
    accounts_placement: FloatingPanelPlacement,
    quick_timeframe_error: Option<QuickTimeframeError>,
    /// The symbol menu's search-scope dropdown, opened from the search-field logo.
    symbol_provider_menu: SymbolProviderMenu,
    order_book_column_menu: MenuVisibility,
    side_panels: SidePanelVisibility,
    side_panel_size: SidePanelSize,
}

impl PaneChromeState {
    /// The overlay on screen, including one playing its closing animation.
    pub(super) fn overlay(&self) -> Option<ChromeOverlay> {
        self.overlay.map(|overlay| overlay.kind)
    }

    /// The overlay that is open and not closing.
    pub(super) fn open_overlay(&self) -> Option<ChromeOverlay> {
        self.overlay
            .filter(|overlay| overlay.phase == ChromeOverlayPhase::Opening)
            .map(|overlay| overlay.kind)
    }

    pub(super) fn is_overlay_open(&self, kind: ChromeOverlay) -> bool {
        self.open_overlay() == Some(kind)
    }

    pub(super) fn is_overlay_closing(&self) -> bool {
        self.overlay_phase() == ChromeOverlayPhase::Closing
    }

    pub(super) fn overlay_phase(&self) -> ChromeOverlayPhase {
        self.overlay
            .map_or(ChromeOverlayPhase::Opening, |overlay| overlay.phase)
    }

    pub(super) const fn overlay_generation(&self) -> u64 {
        self.overlay_generation
    }

    pub(super) fn overlay_trigger_position(&self) -> Option<gpui::Point<Pixels>> {
        self.overlay.and_then(|overlay| overlay.trigger_position)
    }

    /// Shows `kind`, replacing any overlay, with pointer navigation. Only the timeframe menu
    /// keeps the timeframe flyout; the Accounts panel reopens centered.
    pub(super) fn open(
        &mut self,
        kind: ChromeOverlay,
        trigger_position: Option<gpui::Point<Pixels>>,
    ) {
        if kind == ChromeOverlay::Accounts {
            self.accounts_placement = FloatingPanelPlacement::default();
        }
        self.overlay_generation = self.overlay_generation.saturating_add(1);
        self.overlay = Some(OpenOverlay {
            kind,
            phase: ChromeOverlayPhase::Opening,
            trigger_position,
            list_navigation: MenuNavigation::Pointer,
        });
        if kind != ChromeOverlay::Timeframe {
            self.reset_timeframe_flyout();
        }
    }

    /// Starts the closing animation and returns the generation its timer must finish.
    pub(super) fn begin_close(&mut self) -> u64 {
        self.overlay_generation = self.overlay_generation.saturating_add(1);
        if let Some(overlay) = &mut self.overlay {
            overlay.phase = ChromeOverlayPhase::Closing;
        }
        self.overlay_generation
    }

    /// Removes the overlay when the close started at `closing_generation` is still current.
    pub(super) fn finish_close(&mut self, closing_generation: u64) -> bool {
        let current = self.overlay_generation == closing_generation;
        if !current || !self.is_overlay_closing() {
            return false;
        }
        self.dismiss();
        true
    }

    /// Removes the overlay at once, without a closing animation.
    pub(super) fn dismiss(&mut self) {
        self.overlay = None;
        self.reset_timeframe_flyout();
    }

    pub(super) const fn selection(&self) -> usize {
        self.selection
    }

    pub(super) const fn set_selection(&mut self, selection: usize) {
        self.selection = selection;
    }

    pub(super) fn list_keyboard_active(&self) -> bool {
        self.overlay
            .is_some_and(|overlay| overlay.list_navigation == MenuNavigation::Keyboard)
    }

    /// Typing into a list's search field hands its highlight to the keyboard; clearing the
    /// field hands it back to the pointer.
    pub(super) fn set_list_keyboard(&mut self, keyboard: bool) {
        if let Some(overlay) = &mut self.overlay {
            overlay.list_navigation = MenuNavigation::from_keyboard(keyboard);
        }
    }

    /// The indicator menu has no current item, so nothing is highlighted until the user types
    /// or presses an arrow; that first arrow lands on the top row.
    pub(super) fn indicator_menu_awaits_keyboard(&self) -> bool {
        self.overlay()
            .is_some_and(|kind| kind == ChromeOverlay::Indicator)
            && !self.list_keyboard_active()
    }

    pub(super) fn select_previous_row(&mut self) {
        let first_keyboard_step = self.activate_list_keyboard();
        self.selection = if first_keyboard_step {
            0
        } else {
            self.selection.saturating_sub(1)
        };
    }

    pub(super) fn select_next_row(&mut self, count: usize) {
        let first_keyboard_step = self.activate_list_keyboard();
        self.selection = if first_keyboard_step {
            0
        } else {
            (self.selection + 1).min(count.saturating_sub(1))
        };
    }

    /// Hands a searchable list's highlight to the keyboard and reports whether this is the
    /// indicator menu's first keyboard step.
    fn activate_list_keyboard(&mut self) -> bool {
        let first_keyboard_step = self.indicator_menu_awaits_keyboard();
        if let Some(overlay) = &mut self.overlay
            && matches!(
                overlay.kind,
                ChromeOverlay::Instrument | ChromeOverlay::Indicator | ChromeOverlay::TimeZone
            )
        {
            overlay.list_navigation = MenuNavigation::Keyboard;
        }
        first_keyboard_step
    }

    pub(super) fn timeframe_flyout(&self) -> Option<TimeframeMenuGroup> {
        self.timeframe_flyout.map(|flyout| flyout.group)
    }

    pub(super) fn timeframe_flyout_keyboard(&self) -> bool {
        self.timeframe_flyout
            .is_some_and(|flyout| flyout.navigation == MenuNavigation::Keyboard)
    }

    /// Shows `group`'s flyout and cancels any pending flyout close.
    pub(super) fn show_timeframe_flyout(
        &mut self,
        group: TimeframeMenuGroup,
        from_keyboard: bool,
    ) -> TimeframeFlyoutChange {
        self.retain_timeframe_flyout();
        let flyout = TimeframeFlyout {
            group,
            navigation: MenuNavigation::from_keyboard(from_keyboard),
        };
        let change = match self.timeframe_flyout {
            Some(current) if current == flyout => TimeframeFlyoutChange::Unchanged,
            Some(current) if current.group == group => TimeframeFlyoutChange::Navigation,
            _ => TimeframeFlyoutChange::Opened,
        };
        self.timeframe_flyout = Some(flyout);
        change
    }

    /// Hands an open flyout's highlight to the keyboard; reports whether it was the pointer's.
    pub(super) fn focus_timeframe_flyout_keyboard(&mut self) -> bool {
        match &mut self.timeframe_flyout {
            Some(flyout) if flyout.navigation == MenuNavigation::Pointer => {
                flyout.navigation = MenuNavigation::Keyboard;
                true
            }
            _ => false,
        }
    }

    /// Closes the flyout, returning the group it showed.
    pub(super) fn close_timeframe_flyout(&mut self) -> Option<TimeframeMenuGroup> {
        let flyout = self.timeframe_flyout.take()?;
        self.timeframe_hover_regions = 0;
        Some(flyout.group)
    }

    pub(super) const fn reset_timeframe_flyout(&mut self) {
        self.timeframe_flyout = None;
        self.timeframe_hover_regions = 0;
    }

    const fn retain_timeframe_flyout(&mut self) {
        self.timeframe_flyout_close_token = self.timeframe_flyout_close_token.saturating_add(1);
    }

    pub(super) const fn enter_timeframe_menu_region(&mut self) {
        self.timeframe_hover_regions = self.timeframe_hover_regions.saturating_add(1);
        self.retain_timeframe_flyout();
    }

    /// Reports whether the pointer has now left every timeframe menu region.
    pub(super) const fn leave_timeframe_menu_region(&mut self) -> bool {
        if self.timeframe_hover_regions == 0 {
            return false;
        }
        self.timeframe_hover_regions -= 1;
        self.timeframe_hover_regions == 0
    }

    /// Returns the token a delayed flyout close must still hold when its timer fires.
    pub(super) const fn arm_timeframe_flyout_close(&mut self) -> u64 {
        self.retain_timeframe_flyout();
        self.timeframe_flyout_close_token
    }

    pub(super) fn timeframe_flyout_close_due(&self, token: u64) -> bool {
        self.overlay() == Some(ChromeOverlay::Timeframe)
            && self.timeframe_flyout_close_token == token
    }

    pub(super) const fn trigger_bounds(&self, trigger: ChromeTrigger) -> Option<Bounds<Pixels>> {
        self.trigger_bounds.get(trigger)
    }

    /// Records a trigger's measured bounds; reports whether they moved.
    pub(super) fn record_trigger_bounds(
        &mut self,
        trigger: ChromeTrigger,
        bounds: Bounds<Pixels>,
    ) -> bool {
        let slot = self.trigger_bounds.slot(trigger);
        if *slot == Some(bounds) {
            return false;
        }
        *slot = Some(bounds);
        true
    }

    pub(super) const fn accounts_tab(&self) -> AccountsTab {
        self.accounts_tab
    }

    pub(super) fn set_accounts_tab(&mut self, tab: AccountsTab) -> bool {
        if self.accounts_tab == tab {
            return false;
        }
        self.accounts_tab = tab;
        true
    }

    pub(super) const fn accounts_placement(&self) -> &FloatingPanelPlacement {
        &self.accounts_placement
    }

    pub(super) const fn accounts_placement_mut(&mut self) -> &mut FloatingPanelPlacement {
        &mut self.accounts_placement
    }

    pub(super) fn quick_timeframe_error(&self) -> Option<&str> {
        self.quick_timeframe_error
            .as_ref()
            .map(|error| error.message.as_str())
    }

    pub(super) fn reject_quick_timeframe(&mut self, query: String, message: String) {
        self.quick_timeframe_error = Some(QuickTimeframeError { query, message });
    }

    pub(super) fn clear_quick_timeframe_error(&mut self) {
        self.quick_timeframe_error = None;
    }

    /// Only a real edit retracts the rejection; Enter also reports a change while the
    /// rejected text is still in place.
    pub(super) fn retract_quick_timeframe_error_after_edit(&mut self, query: &str) {
        if self
            .quick_timeframe_error
            .as_ref()
            .is_some_and(|error| error.query != query)
        {
            self.quick_timeframe_error = None;
        }
    }

    pub(super) fn symbol_provider_menu_open(&self) -> bool {
        self.symbol_provider_menu != SymbolProviderMenu::Closed
    }

    pub(super) const fn symbol_scope_flyout_provider(&self) -> Option<ProviderId> {
        match self.symbol_provider_menu {
            SymbolProviderMenu::Flyout(provider) => Some(provider),
            SymbolProviderMenu::Closed | SymbolProviderMenu::Open => None,
        }
    }

    pub(super) const fn close_symbol_provider_menu(&mut self) {
        self.symbol_provider_menu = SymbolProviderMenu::Closed;
    }

    pub(super) const fn toggle_symbol_provider_menu(&mut self) {
        self.symbol_provider_menu = match self.symbol_provider_menu {
            SymbolProviderMenu::Closed => SymbolProviderMenu::Open,
            SymbolProviderMenu::Open | SymbolProviderMenu::Flyout(_) => SymbolProviderMenu::Closed,
        };
    }

    /// Shows or hides a provider's flyout beside the open scope menu; reports a change.
    pub(super) fn set_symbol_scope_flyout(&mut self, provider: Option<ProviderId>) -> bool {
        let next = match (self.symbol_provider_menu, provider) {
            (SymbolProviderMenu::Closed, _) => return false,
            (_, Some(provider)) => SymbolProviderMenu::Flyout(provider),
            (_, None) => SymbolProviderMenu::Open,
        };
        if self.symbol_provider_menu == next {
            return false;
        }
        self.symbol_provider_menu = next;
        true
    }

    pub(super) fn order_book_column_menu_open(&self) -> bool {
        self.order_book_column_menu == MenuVisibility::Open
    }

    pub(super) const fn toggle_order_book_column_menu(&mut self) {
        self.order_book_column_menu = match self.order_book_column_menu {
            MenuVisibility::Closed => MenuVisibility::Open,
            MenuVisibility::Open => MenuVisibility::Closed,
        };
    }

    pub(super) fn close_order_book_column_menu(&mut self) -> bool {
        let was_open = self.order_book_column_menu_open();
        self.order_book_column_menu = MenuVisibility::Closed;
        was_open
    }

    pub(super) const fn side_panels(&self) -> SidePanelVisibility {
        self.side_panels
    }

    /// Shows or hides one docked panel; hiding the order book also closes its column menu.
    pub(super) fn set_side_panel_visible(&mut self, panel: SidePanel, visible: bool) -> bool {
        if self.side_panels.contains(panel) == visible {
            return false;
        }
        self.side_panels.set(panel, visible);
        if panel == SidePanel::OrderBook && !visible {
            self.order_book_column_menu = MenuVisibility::Closed;
        }
        true
    }

    pub(super) const fn side_panel_width(&self) -> f32 {
        self.side_panel_size.width
    }

    pub(super) const fn side_panel_split_basis_points(&self) -> u32 {
        self.side_panel_size.split_basis_points
    }

    pub(super) const fn side_panel_size(&self) -> SidePanelSize {
        self.side_panel_size
    }

    pub(super) const fn set_side_panel_size(&mut self, size: SidePanelSize) {
        self.side_panel_size = size;
    }

    pub(super) fn restore_side_panel_size(&mut self, width: u32, split_basis_points: u32) {
        self.side_panel_size = SidePanelSize::from_persisted(width, split_basis_points);
    }

    pub(super) fn set_side_panel_width(&mut self, width: f32) -> bool {
        let width = clamped_side_panel_width(width);
        if (width - self.side_panel_size.width).abs() <= f32::EPSILON {
            return false;
        }
        self.side_panel_size.width = width;
        true
    }

    pub(super) fn set_side_panel_split_ratio(&mut self, ratio: f32) -> bool {
        let basis_points = (ratio.clamp(0.05, 0.95) * 10_000.0)
            .round()
            .to_u32()
            .unwrap_or(DEFAULT_SIDE_PANEL_SPLIT_BASIS_POINTS);
        if basis_points == self.side_panel_size.split_basis_points {
            return false;
        }
        self.side_panel_size.split_basis_points = basis_points;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};

    fn opened(kind: ChromeOverlay) -> PaneChromeState {
        let mut chrome = PaneChromeState::default();
        chrome.open(kind, None);
        chrome
    }

    #[test]
    fn a_closing_overlay_stays_shown_but_is_not_open() {
        let mut chrome = opened(ChromeOverlay::ChartType);
        assert!(chrome.is_overlay_open(ChromeOverlay::ChartType));
        chrome.begin_close();
        assert_eq!(chrome.overlay(), Some(ChromeOverlay::ChartType));
        assert_eq!(chrome.open_overlay(), None);
        assert!(chrome.is_overlay_closing());
    }

    #[test]
    fn stale_close_completion_cannot_remove_a_reopened_overlay() {
        let mut chrome = opened(ChromeOverlay::ChartType);
        let closing = chrome.begin_close();
        chrome.open(ChromeOverlay::TimeZone, None);
        assert!(!chrome.finish_close(closing));
        assert!(chrome.is_overlay_open(ChromeOverlay::TimeZone));

        let closing = chrome.begin_close();
        assert!(chrome.finish_close(closing));
        assert_eq!(chrome.overlay(), None);
        assert_eq!(chrome.overlay_phase(), ChromeOverlayPhase::Opening);
        assert!(
            !chrome.finish_close(closing),
            "a finished close is not repeated"
        );
    }

    #[test]
    fn opening_resets_list_navigation_and_keeps_the_trigger_position() {
        let mut chrome = opened(ChromeOverlay::Instrument);
        chrome.set_list_keyboard(true);
        assert!(chrome.list_keyboard_active());
        let trigger = point(px(12.0), px(4.0));
        chrome.open(ChromeOverlay::Indicator, Some(trigger));
        assert!(!chrome.list_keyboard_active());
        assert_eq!(chrome.overlay_trigger_position(), Some(trigger));
        chrome.dismiss();
        assert!(!chrome.list_keyboard_active());
        assert_eq!(chrome.overlay_trigger_position(), None);
    }

    #[test]
    fn the_first_indicator_arrow_lands_on_the_top_row() {
        let mut chrome = opened(ChromeOverlay::Indicator);
        chrome.set_selection(3);
        assert!(chrome.indicator_menu_awaits_keyboard());
        chrome.select_next_row(10);
        assert_eq!(chrome.selection(), 0);
        assert!(chrome.list_keyboard_active());
        chrome.select_next_row(2);
        chrome.select_next_row(2);
        assert_eq!(chrome.selection(), 1, "the next row stops at the last row");
        chrome.select_previous_row();
        chrome.select_previous_row();
        assert_eq!(chrome.selection(), 0);
    }

    #[test]
    fn arrows_on_a_fixed_menu_never_hand_it_to_list_keyboard_navigation() {
        let mut chrome = opened(ChromeOverlay::ChartType);
        chrome.set_selection(2);
        chrome.select_previous_row();
        assert_eq!(chrome.selection(), 1);
        assert!(!chrome.list_keyboard_active());
    }

    #[test]
    fn showing_a_timeframe_flyout_reports_what_changed() {
        let mut chrome = opened(ChromeOverlay::Timeframe);
        assert_eq!(
            chrome.show_timeframe_flyout(TimeframeMenuGroup::Minutes, false),
            TimeframeFlyoutChange::Opened
        );
        assert_eq!(
            chrome.show_timeframe_flyout(TimeframeMenuGroup::Minutes, false),
            TimeframeFlyoutChange::Unchanged
        );
        assert_eq!(
            chrome.show_timeframe_flyout(TimeframeMenuGroup::Minutes, true),
            TimeframeFlyoutChange::Navigation
        );
        assert!(chrome.timeframe_flyout_keyboard());
        assert_eq!(
            chrome.show_timeframe_flyout(TimeframeMenuGroup::Hours, true),
            TimeframeFlyoutChange::Opened
        );
        assert_eq!(
            chrome.close_timeframe_flyout(),
            Some(TimeframeMenuGroup::Hours)
        );
        assert_eq!(chrome.close_timeframe_flyout(), None);
        assert!(!chrome.timeframe_flyout_keyboard());
    }

    #[test]
    fn only_a_pointer_flyout_is_handed_to_the_keyboard() {
        let mut chrome = opened(ChromeOverlay::Timeframe);
        assert!(
            !chrome.focus_timeframe_flyout_keyboard(),
            "no flyout is open"
        );
        chrome.show_timeframe_flyout(TimeframeMenuGroup::Days, false);
        assert!(chrome.focus_timeframe_flyout_keyboard());
        assert!(!chrome.focus_timeframe_flyout_keyboard());
        assert!(chrome.timeframe_flyout_keyboard());
    }

    #[test]
    fn opening_another_overlay_drops_the_timeframe_flyout() {
        let mut chrome = opened(ChromeOverlay::Timeframe);
        chrome.show_timeframe_flyout(TimeframeMenuGroup::Seconds, false);
        chrome.open(ChromeOverlay::Timeframe, None);
        assert_eq!(chrome.timeframe_flyout(), Some(TimeframeMenuGroup::Seconds));
        chrome.open(ChromeOverlay::ChartType, None);
        assert_eq!(chrome.timeframe_flyout(), None);
    }

    #[test]
    fn a_flyout_close_waits_for_the_pointer_to_leave_every_region() {
        let mut chrome = opened(ChromeOverlay::Timeframe);
        assert!(
            !chrome.leave_timeframe_menu_region(),
            "no region was entered"
        );
        chrome.enter_timeframe_menu_region();
        chrome.enter_timeframe_menu_region();
        assert!(!chrome.leave_timeframe_menu_region());
        assert!(chrome.leave_timeframe_menu_region());
        let token = chrome.arm_timeframe_flyout_close();
        assert!(chrome.timeframe_flyout_close_due(token));
        chrome.enter_timeframe_menu_region();
        assert!(
            !chrome.timeframe_flyout_close_due(token),
            "re-entering the menu cancels the pending close"
        );
        let token = chrome.arm_timeframe_flyout_close();
        chrome.dismiss();
        assert!(!chrome.timeframe_flyout_close_due(token));
    }

    #[test]
    fn trigger_bounds_report_only_a_move() {
        let mut chrome = PaneChromeState::default();
        let bounds = Bounds::new(point(px(1.0), px(2.0)), size(px(30.0), px(20.0)));
        assert!(chrome.record_trigger_bounds(ChromeTrigger::TimeZone, bounds));
        assert!(!chrome.record_trigger_bounds(ChromeTrigger::TimeZone, bounds));
        assert_eq!(chrome.trigger_bounds(ChromeTrigger::TimeZone), Some(bounds));
        assert_eq!(chrome.trigger_bounds(ChromeTrigger::Timeframe), None);
    }

    #[test]
    fn opening_accounts_recenters_the_panel_and_keeps_its_section() {
        let mut chrome = PaneChromeState::default();
        assert!(chrome.set_accounts_tab(AccountsTab::TradingAccounts));
        assert!(!chrome.set_accounts_tab(AccountsTab::TradingAccounts));
        chrome.open(ChromeOverlay::Accounts, None);
        assert!(
            chrome
                .accounts_placement_mut()
                .follow(point(px(50.0), px(60.0)))
        );
        assert!(chrome.accounts_placement().origin.is_some());
        chrome.dismiss();
        chrome.open(ChromeOverlay::Accounts, None);
        assert_eq!(chrome.accounts_placement().origin, None);
        assert_eq!(chrome.accounts_tab(), AccountsTab::TradingAccounts);
    }

    #[test]
    fn only_an_edit_retracts_a_quick_timeframe_rejection() {
        let mut chrome = PaneChromeState::default();
        chrome.reject_quick_timeframe("9x".into(), "not available".into());
        chrome.retract_quick_timeframe_error_after_edit("9x");
        assert_eq!(chrome.quick_timeframe_error(), Some("not available"));
        chrome.retract_quick_timeframe_error_after_edit("9");
        assert_eq!(chrome.quick_timeframe_error(), None);
    }

    #[test]
    fn the_scope_flyout_opens_only_inside_the_open_scope_menu() {
        let provider = ProviderId::unsupported();
        let mut chrome = PaneChromeState::default();
        assert!(!chrome.set_symbol_scope_flyout(Some(provider)));
        chrome.toggle_symbol_provider_menu();
        assert!(chrome.symbol_provider_menu_open());
        assert!(chrome.set_symbol_scope_flyout(Some(provider)));
        assert_eq!(chrome.symbol_scope_flyout_provider(), Some(provider));
        assert!(!chrome.set_symbol_scope_flyout(Some(provider)));
        assert!(chrome.set_symbol_scope_flyout(None));
        chrome.set_symbol_scope_flyout(Some(provider));
        chrome.toggle_symbol_provider_menu();
        assert!(!chrome.symbol_provider_menu_open());
        assert_eq!(chrome.symbol_scope_flyout_provider(), None);
    }

    #[test]
    fn hiding_the_order_book_closes_its_column_menu() {
        let mut chrome = PaneChromeState::default();
        assert!(chrome.set_side_panel_visible(SidePanel::OrderBook, true));
        assert!(!chrome.set_side_panel_visible(SidePanel::OrderBook, true));
        chrome.toggle_order_book_column_menu();
        assert!(chrome.order_book_column_menu_open());
        assert!(chrome.set_side_panel_visible(SidePanel::TimeSales, true));
        assert!(chrome.order_book_column_menu_open());
        assert!(chrome.set_side_panel_visible(SidePanel::OrderBook, false));
        assert!(!chrome.order_book_column_menu_open());
        assert!(!chrome.close_order_book_column_menu());
        assert!(chrome.side_panels().contains(SidePanel::TimeSales));
    }

    #[test]
    fn persisted_side_panel_sizes_are_defaulted_and_clamped() {
        let mut chrome = PaneChromeState::default();
        chrome.restore_side_panel_size(0, 0);
        assert!((chrome.side_panel_width() - SIDE_PANEL_INITIAL_WIDTH).abs() < f32::EPSILON);
        assert_eq!(chrome.side_panel_split_basis_points(), 5_000);
        chrome.restore_side_panel_size(9_000, 10_000);
        assert!((chrome.side_panel_width() - 480.0).abs() < f32::EPSILON);
        assert_eq!(chrome.side_panel_split_basis_points(), 9_500);
        chrome.restore_side_panel_size(100, 1);
        assert!((chrome.side_panel_width() - 360.0).abs() < f32::EPSILON);
        assert_eq!(chrome.side_panel_split_basis_points(), 500);
    }

    #[test]
    fn side_panel_resizes_report_only_real_changes() {
        let mut chrome = PaneChromeState::default();
        assert!(!chrome.set_side_panel_width(SIDE_PANEL_INITIAL_WIDTH));
        assert!(chrome.set_side_panel_width(900.0));
        assert!((chrome.side_panel_width() - 480.0).abs() < f32::EPSILON);
        assert!(!chrome.set_side_panel_split_ratio(0.5));
        assert!(chrome.set_side_panel_split_ratio(0.99));
        assert_eq!(chrome.side_panel_split_basis_points(), 9_500);

        let mut recipient = PaneChromeState::default();
        recipient.set_side_panel_size(chrome.side_panel_size());
        assert_eq!(recipient.side_panel_size(), chrome.side_panel_size());
    }
}
