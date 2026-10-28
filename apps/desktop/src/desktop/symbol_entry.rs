//! A pane's symbol entry: the search field, the catalog browser behind its results, which
//! menu a selection serves, and the selections still waiting on the market runtime (a typed
//! symbol, an opening product and an installed watchlist instrument).

use gpui::{App, Entity};

use super::symbol_browser::{ExactSymbolMatch, SymbolBrowser};
use super::{
    InputState, InstallProviderInstrument, SymbolSearchScope, SymbolSelectionTarget,
    instrument_target_after_close,
};

pub(super) struct SymbolEntry {
    browser: SymbolBrowser,
    /// Absent for a pane that was given no search field.
    input: Option<Entity<InputState>>,
    /// The menu the open instrument list serves.
    target: SymbolSelectionTarget,
    /// The menu a scheduled selection serves until the runtime installs or rejects it.
    pending_target: Option<SymbolSelectionTarget>,
    /// A watchlist selection the runtime installed, waiting for the terminal to add it.
    installed_watchlist_instrument: Option<InstallProviderInstrument>,
    /// A typed symbol that selects its exact market once every provider ahead of it answered.
    pending_mnemonic: Option<String>,
    /// The menu target of the product whose front month is opening.
    product_opening_target: Option<SymbolSelectionTarget>,
}

impl SymbolEntry {
    pub(super) const fn new(browser: SymbolBrowser, input: Option<Entity<InputState>>) -> Self {
        Self {
            browser,
            input,
            target: SymbolSelectionTarget::Chart,
            pending_target: None,
            installed_watchlist_instrument: None,
            pending_mnemonic: None,
            product_opening_target: None,
        }
    }

    pub(super) const fn browser(&self) -> &SymbolBrowser {
        &self.browser
    }

    pub(super) const fn browser_mut(&mut self) -> &mut SymbolBrowser {
        &mut self.browser
    }

    pub(super) const fn input(&self) -> Option<&Entity<InputState>> {
        self.input.as_ref()
    }

    /// The search field's text, empty without a field.
    pub(super) fn query(&self, cx: &App) -> String {
        self.input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// Changes the providers searched, reporting whether the scope changed. A typed symbol
    /// waiting on the former scope's listing is dropped with it.
    pub(super) fn set_scope(&mut self, scope: SymbolSearchScope) -> bool {
        if !self.browser.set_scope(scope) {
            return false;
        }
        self.pending_mnemonic = None;
        true
    }

    pub(super) const fn target(&self) -> SymbolSelectionTarget {
        self.target
    }

    /// The instrument menu opens to choose a market for `target`.
    pub(super) const fn open_for(&mut self, target: SymbolSelectionTarget) {
        self.target = target;
    }

    /// Closing the instrument menu returns it to the chart unless a selection it scheduled
    /// is still pending, which keeps serving the menu that made it.
    pub(super) const fn menu_closed(&mut self, selection_pending: bool) {
        self.target = instrument_target_after_close(self.target, selection_pending);
    }

    /// A selection for `target` was handed to the market runtime.
    pub(super) const fn selection_scheduled(&mut self, target: SymbolSelectionTarget) {
        self.pending_target = Some(target);
    }

    /// Whether the pending selection adds to the watchlist instead of switching the chart.
    pub(super) fn awaits_watchlist_install(&self) -> bool {
        self.pending_target == Some(SymbolSelectionTarget::Watchlist)
    }

    /// The chart switch the pending selection asked for completed or failed.
    pub(super) const fn forget_pending_selection(&mut self) {
        self.pending_target = None;
    }

    /// The runtime answered the pending selection; a menu that is no longer open returns
    /// to choosing the chart's market.
    pub(super) const fn selection_settled(&mut self, menu_open: bool) {
        self.pending_target = None;
        if !menu_open {
            self.target = SymbolSelectionTarget::Chart;
        }
    }

    /// The runtime installed the pending watchlist selection as `instrument`.
    pub(super) fn watchlist_instrument_installed(
        &mut self,
        instrument: InstallProviderInstrument,
        menu_open: bool,
    ) {
        self.installed_watchlist_instrument = Some(instrument);
        self.selection_settled(menu_open);
    }

    pub(super) const fn take_installed_watchlist_instrument(
        &mut self,
    ) -> Option<InstallProviderInstrument> {
        self.installed_watchlist_instrument.take()
    }

    /// Selects `symbol`'s exact market once the search listing it answers.
    pub(super) fn await_mnemonic(&mut self, symbol: String) {
        self.pending_mnemonic = Some(symbol);
    }

    pub(super) fn clear_mnemonic(&mut self) {
        self.pending_mnemonic = None;
    }

    /// How the waiting typed symbol matches the current results, if one is waiting.
    pub(super) fn pending_mnemonic_match(&self) -> Option<ExactSymbolMatch> {
        self.pending_mnemonic
            .as_deref()
            .map(|symbol| self.browser.exact_symbol(symbol))
    }

    /// A product row opened for `target` is listing its contracts.
    pub(super) const fn begin_product_opening(&mut self, target: SymbolSelectionTarget) {
        self.product_opening_target = Some(target);
    }

    pub(super) const fn cancel_product_opening(&mut self) {
        self.product_opening_target = None;
    }

    /// The menu the opened product serves; the chart when none was recorded.
    pub(super) const fn take_product_opening_target(&mut self) -> SymbolSelectionTarget {
        match self.product_opening_target.take() {
            Some(target) => target,
            None => SymbolSelectionTarget::Chart,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> SymbolEntry {
        SymbolEntry::new(SymbolBrowser::new(SymbolSearchScope::default()), None)
    }

    fn instrument(id: &str) -> InstallProviderInstrument {
        InstallProviderInstrument {
            instrument_id: id.to_string(),
            ..InstallProviderInstrument::default()
        }
    }

    #[test]
    fn a_closed_menu_returns_to_the_chart_unless_its_selection_is_pending() {
        let mut entry = entry();
        assert_eq!(entry.target(), SymbolSelectionTarget::Chart);
        entry.open_for(SymbolSelectionTarget::Watchlist);
        entry.menu_closed(true);
        assert_eq!(entry.target(), SymbolSelectionTarget::Watchlist);
        entry.menu_closed(false);
        assert_eq!(entry.target(), SymbolSelectionTarget::Chart);
    }

    #[test]
    fn an_installed_watchlist_selection_is_handed_over_once() {
        let mut entry = entry();
        entry.open_for(SymbolSelectionTarget::Watchlist);
        entry.selection_scheduled(SymbolSelectionTarget::Watchlist);
        assert!(entry.awaits_watchlist_install());
        entry.watchlist_instrument_installed(instrument("BTC"), true);
        assert!(!entry.awaits_watchlist_install());
        assert_eq!(
            entry.target(),
            SymbolSelectionTarget::Watchlist,
            "the open menu keeps serving the watchlist"
        );
        assert_eq!(
            entry.take_installed_watchlist_instrument(),
            Some(instrument("BTC"))
        );
        assert_eq!(entry.take_installed_watchlist_instrument(), None);
    }

    #[test]
    fn a_settled_selection_returns_a_closed_menu_to_the_chart() {
        let mut entry = entry();
        entry.open_for(SymbolSelectionTarget::Watchlist);
        entry.selection_scheduled(SymbolSelectionTarget::Watchlist);
        entry.selection_settled(false);
        assert!(!entry.awaits_watchlist_install());
        assert_eq!(entry.target(), SymbolSelectionTarget::Chart);

        entry.open_for(SymbolSelectionTarget::Watchlist);
        entry.selection_scheduled(SymbolSelectionTarget::Watchlist);
        entry.forget_pending_selection();
        assert!(!entry.awaits_watchlist_install());
        assert_eq!(
            entry.target(),
            SymbolSelectionTarget::Watchlist,
            "a chart switch's answer leaves the menu target alone"
        );
    }

    #[test]
    fn an_opened_product_serves_the_menu_that_opened_it() {
        let mut entry = entry();
        entry.begin_product_opening(SymbolSelectionTarget::Watchlist);
        assert_eq!(
            entry.take_product_opening_target(),
            SymbolSelectionTarget::Watchlist
        );
        assert_eq!(
            entry.take_product_opening_target(),
            SymbolSelectionTarget::Chart
        );
        entry.begin_product_opening(SymbolSelectionTarget::Watchlist);
        entry.cancel_product_opening();
        assert_eq!(
            entry.take_product_opening_target(),
            SymbolSelectionTarget::Chart
        );
    }

    #[test]
    fn a_typed_symbol_waits_until_cleared_or_the_scope_changes() {
        let mut entry = entry();
        assert_eq!(entry.pending_mnemonic_match(), None);
        entry.await_mnemonic("BTC".to_string());
        assert!(entry.pending_mnemonic_match().is_some());
        entry.clear_mnemonic();
        assert_eq!(entry.pending_mnemonic_match(), None);

        entry.await_mnemonic("BTC".to_string());
        let scope = entry.browser().scope();
        assert!(!entry.set_scope(scope), "the same scope is no change");
        assert!(entry.pending_mnemonic_match().is_some());
        assert!(entry.set_scope(SymbolSearchScope::Provider("fixture")));
        assert_eq!(entry.pending_mnemonic_match(), None);
    }
}
