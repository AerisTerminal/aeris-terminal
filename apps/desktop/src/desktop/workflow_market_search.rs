//! The Workflows page's market search: one runtime catalog consumer that searches every
//! symbol-menu provider and installs the market the user picks. Installing it hands the
//! trading owner the market's verified terms, which a saved workflow needs. The consumer
//! holds no series demand, so it stays registered while the page is hidden.

use super::symbol_browser::{
    InstrumentMenuAction, InstrumentMenuEntry, InstrumentMenuSelection, OpenedProduct,
    SymbolBrowser, SymbolBrowserStatus, SymbolSearchFailure, SymbolSearchRequest,
};
use super::*;
use aeris_localization::{tr, workflows, workspace};

const MESSAGES_PER_POLL: usize = 16;

/// Why the search shows a notice in place of its status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum MarketSearchNotice {
    QueryTooLong,
    SearchFailed,
    Unavailable,
    /// A product without a listed contract, by its code.
    NoContracts(String),
    /// The runtime refused the selection, worded for the user.
    SelectionRefused(String),
    /// The installed market lacks the terms a workflow needs, by its symbol.
    NotTradable(String),
}

/// What one poll of the search consumer changed.
#[derive(Default)]
pub(super) struct MarketSearchPoll {
    pub(super) changed: bool,
    /// The picked market, installed by the runtime.
    pub(super) installed: Option<InstallProviderInstrument>,
}

pub(super) struct WorkflowMarketSearch {
    worker: Option<MarketDataWorker>,
    browser: SymbolBrowser,
    input: Option<Entity<InputState>>,
    /// The trimmed query; an empty one lists nothing.
    query: String,
    notice: Option<MarketSearchNotice>,
}

impl Default for WorkflowMarketSearch {
    fn default() -> Self {
        Self {
            worker: None,
            browser: SymbolBrowser::new(SymbolSearchScope::All),
            input: None,
            query: String::new(),
            notice: None,
        }
    }
}

impl WorkflowMarketSearch {
    /// Starts the search consumer the first time a workflow opens.
    pub(super) fn ensure_worker(
        &mut self,
        factory: Option<&engine_market_worker::WorkspaceMarketFactory>,
        workspace_id: u64,
        dirty: &Arc<DirtySet>,
    ) {
        if self.worker.is_some() {
            return;
        }
        let Some(factory) = factory else {
            self.notice = Some(MarketSearchNotice::Unavailable);
            return;
        };
        match factory.create_catalog_worker(workspace_id) {
            Ok(worker) => {
                worker.set_message_wake(dirty.source_wake(AppSource::WorkflowMarkets));
                self.worker = Some(worker);
                self.notice = None;
            }
            Err(error) => {
                diagnostic!("Aeris workflow market search could not start: {error}");
                self.notice = Some(MarketSearchNotice::Unavailable);
            }
        }
    }

    pub(super) const fn input(&self) -> Option<&Entity<InputState>> {
        self.input.as_ref()
    }

    pub(super) fn set_input(&mut self, input: Entity<InputState>) {
        self.input = Some(input);
    }

    /// Sends `query` to every provider at once; an empty query clears the results instead.
    /// An unchanged query, such as the field being emptied after a pick, keeps the notice.
    pub(super) fn search(&mut self, query: &str) {
        let query = query.trim();
        if query == self.query {
            return;
        }
        self.notice = None;
        query.clone_into(&mut self.query);
        if self.query.is_empty() {
            return;
        }
        match self.browser.begin_search(&self.query) {
            Ok(requests) => {
                for request in requests {
                    self.dispatch(request);
                }
            }
            Err(_) => self.notice = Some(MarketSearchNotice::QueryTooLong),
        }
    }

    /// The picked market was taken into the draft or refused; the field starts over.
    pub(super) fn finish(&mut self, notice: Option<MarketSearchNotice>) {
        self.query.clear();
        self.notice = notice;
    }

    pub(super) fn entries(&self) -> Vec<InstrumentMenuEntry> {
        if self.query.is_empty() {
            return Vec::new();
        }
        self.browser.menu_entries()
    }

    /// The line under the field: a notice, or how the search stands while it is busy or
    /// found nothing.
    pub(super) fn status(&self) -> Option<String> {
        if let Some(notice) = &self.notice {
            return Some(notice_text(notice));
        }
        if self.browser.pending_selection_provider().is_some() {
            return Some(tr!(workspace::SELECTING_MARKET).to_string());
        }
        if self.query.is_empty() {
            return None;
        }
        match self.browser.status() {
            SymbolBrowserStatus::Searching => {
                Some(tr!(workspace::SEARCHING_PROVIDER_MARKETS).to_string())
            }
            SymbolBrowserStatus::NoMatches => Some(tr!(workspace::NO_MARKETS_FOUND).to_string()),
            SymbolBrowserStatus::Listed(_) | SymbolBrowserStatus::Idle => None,
        }
    }

    /// Selects a listed market, or lists a product's contracts to select its nearest one.
    pub(super) fn activate(&mut self, action: &InstrumentMenuAction) {
        self.notice = None;
        match action {
            InstrumentMenuAction::Select(selection) => self.select(*selection),
            InstrumentMenuAction::OpenProduct(product) => {
                if let Some(request) = self.browser.open_product(product) {
                    self.dispatch(request);
                }
            }
            InstrumentMenuAction::Inert => {}
        }
    }

    /// Hands the consumer to shutdown; the page starts a new one if a workflow opens again.
    pub(super) fn begin_retirement(&mut self) -> Option<MarketWorkerRetirement> {
        self.worker.take()?.begin_retirement()
    }

    pub(super) fn poll(&mut self) -> MarketSearchPoll {
        let mut outcome = MarketSearchPoll::default();
        let Some(worker) = &mut self.worker else {
            return outcome;
        };
        let (messages, disconnected) = worker.drain_messages_up_to(MESSAGES_PER_POLL);
        for message in messages {
            match message {
                PaneProjectionEvent::RuntimeProjection(
                    aeris_desktop::market_worker::RuntimeProjection { event, .. },
                ) => {
                    if let Some(event) = project_provider_catalog_event(event.value) {
                        outcome.changed |= self.apply(event, &mut outcome.installed);
                    }
                }
                PaneProjectionEvent::LocalFailure {
                    scope: aeris_desktop::market_worker::LocalFailureScope::Catalog,
                    message,
                } => {
                    diagnostic!("Aeris workflow market search failed: {message}");
                    self.notice = Some(MarketSearchNotice::SearchFailed);
                    outcome.changed = true;
                }
                _ => {}
            }
        }
        if disconnected {
            self.worker = None;
            self.notice = Some(MarketSearchNotice::Unavailable);
            outcome.changed = true;
        }
        outcome
    }

    fn dispatch(&mut self, request: SymbolSearchRequest) {
        let search = SearchProviderInstruments {
            startup_resolve: false,
            consumer_id: 0,
            search_generation: u64::try_from(request.request_id.get()).unwrap_or(u64::MAX),
            provider: request.provider.id().to_string(),
            query: request.query,
            maximum_results: u32::try_from(self.browser.maximum_results()).unwrap_or(u32::MAX),
            categories: aeris_market_runtime::InstrumentSearchCategories::ALL,
            exchange: request
                .exchange
                .map(|exchange| exchange.as_str().to_string()),
        };
        let sent = self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.try_search_provider(search).is_ok());
        if !sent {
            self.browser.reject_search(
                request.provider,
                request.request_id,
                SymbolSearchFailure::Failed,
            );
            self.notice = Some(MarketSearchNotice::SearchFailed);
        }
    }

    fn select(&mut self, selection: InstrumentMenuSelection) {
        if self.browser.pending_selection_provider().is_some() {
            return;
        }
        let Some(descriptor) = super::provider_presentation(selection.provider()) else {
            return;
        };
        let Some(selection) = self.browser.select(selection) else {
            return;
        };
        let request = SelectProviderInstrument {
            startup_resolve: false,
            consumer_id: 0,
            selection_generation: u64::try_from(selection.generation.get()).unwrap_or(u64::MAX),
            search_generation: u64::try_from(selection.search_generation.get()).unwrap_or(u64::MAX),
            provider: selection.provider.id().to_string(),
            symbol: selection.instrument.symbol.clone(),
            exchange: selection.instrument.exchange.clone(),
            entitlement_id: descriptor.selection_entitlement_id.to_string(),
        };
        let sent = self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.try_select_provider(request).is_ok());
        if !sent {
            self.browser.reject_selection(selection.generation);
            self.notice = Some(MarketSearchNotice::SelectionRefused(
                tr!(workspace::MARKET_SELECTION_UNSCHEDULED).to_string(),
            ));
        }
    }

    fn apply(
        &mut self,
        event: ProviderCatalogEvent,
        installed: &mut Option<InstallProviderInstrument>,
    ) -> bool {
        let Some(provider) = ProviderId::find(provider_catalog_event_provider(&event))
            .filter(|provider| self.browser.covers(*provider))
        else {
            return false;
        };
        match event {
            ProviderCatalogEvent::SearchCompleted(result) => {
                let accepted = usize_generation(result.search_generation).is_some_and(|search| {
                    self.browser
                        .apply_results(provider, search, result.instruments)
                });
                if accepted {
                    self.select_opened_product();
                }
                accepted
            }
            ProviderCatalogEvent::SelectionInstalled {
                command_generation,
                instrument,
            } => {
                let resolved = usize_generation(command_generation)
                    .and_then(|generation| self.browser.resolve_selection(generation));
                if resolved.is_some() {
                    *installed = Some(instrument);
                }
                resolved.is_some()
            }
            ProviderCatalogEvent::CatalogReady { .. } => {
                if self.query.is_empty() {
                    return false;
                }
                if let Some(request) = self.browser.retry_lane(provider) {
                    self.dispatch(request);
                }
                true
            }
            ProviderCatalogEvent::CommandRejected { rejection, command } => {
                self.apply_rejection(provider, &rejection, command)
            }
            ProviderCatalogEvent::ScreenCompleted(_)
            | ProviderCatalogEvent::ScreenRejected(_)
            | ProviderCatalogEvent::ScanUpdated(_)
            | ProviderCatalogEvent::ScanRejected(_) => false,
        }
    }

    fn select_opened_product(&mut self) {
        match self.browser.take_opened_product() {
            Some(OpenedProduct::Contract(selection)) => self.select(selection),
            Some(OpenedProduct::NoContracts(product)) => {
                self.notice = Some(MarketSearchNotice::NoContracts(product.code));
            }
            None => {}
        }
    }

    fn apply_rejection(
        &mut self,
        provider: ProviderId,
        rejection: &ProviderCatalogRejected,
        command: ProviderCatalogCommand,
    ) -> bool {
        let Some(generation) = usize_generation(rejection.command_generation) else {
            return false;
        };
        if command == ProviderCatalogCommand::Selection {
            if !self.browser.reject_selection(generation) {
                return false;
            }
            self.notice = Some(MarketSearchNotice::SelectionRefused(
                catalog_rejection_message(rejection.reason, command, provider),
            ));
            return true;
        }
        let failure = match rejection.reason {
            // The lane already sent the search that replaced this one.
            ProviderCatalogRejectionReason::SupersededSearch => return false,
            ProviderCatalogRejectionReason::DispatchUnavailable => SymbolSearchFailure::Unavailable,
            _ => SymbolSearchFailure::Failed,
        };
        self.browser.reject_search(provider, generation, failure)
    }
}

fn notice_text(notice: &MarketSearchNotice) -> String {
    match notice {
        MarketSearchNotice::QueryTooLong => tr!(workspace::MARKET_QUERY_TOO_LONG).to_string(),
        MarketSearchNotice::SearchFailed => tr!(workspace::MARKET_SEARCH_FAILED).to_string(),
        MarketSearchNotice::Unavailable => tr!(workspace::MARKET_SEARCH_UNAVAILABLE).to_string(),
        MarketSearchNotice::NoContracts(product) => {
            tr!(workspace::NO_PRODUCT_CONTRACTS, product = product)
        }
        MarketSearchNotice::SelectionRefused(detail) => detail.clone(),
        MarketSearchNotice::NotTradable(symbol) => {
            tr!(workflows::MARKET_NOT_TRADABLE, symbol = symbol)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_query_lists_nothing_and_sends_no_search() {
        let mut search = WorkflowMarketSearch::default();
        search.search("   ");
        assert_eq!(search.entries().len(), 0);
        assert_eq!(search.status(), None);
        assert!(!search.browser.searched());
    }

    #[test]
    fn a_search_without_a_consumer_fails_visibly() {
        let mut search = WorkflowMarketSearch::default();
        search.search("ES");
        assert_eq!(search.notice, Some(MarketSearchNotice::SearchFailed));
        assert!(!search.browser.search_pending());
    }

    #[test]
    fn an_overlong_query_is_refused_before_any_search() {
        let mut search = WorkflowMarketSearch::default();
        search.search(&"E".repeat(65));
        assert_eq!(search.notice, Some(MarketSearchNotice::QueryTooLong));
        assert!(!search.browser.searched());
    }

    #[test]
    fn finishing_clears_the_query_and_keeps_the_notice() {
        let mut search = WorkflowMarketSearch::default();
        search.search("ES");
        search.finish(Some(MarketSearchNotice::NotTradable("ESZ6".into())));
        assert_eq!(search.entries().len(), 0);
        assert_eq!(
            search.notice,
            Some(MarketSearchNotice::NotTradable("ESZ6".into()))
        );
    }
}
