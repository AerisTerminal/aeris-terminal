use axiusflow_contracts::ProviderInstrumentSummary;
use axiusflow_observability::{FeedConnectionState, FeedIdentity};
use std::num::NonZeroUsize;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RithmicShellState {
    pub(crate) identity: FeedIdentity,
    connection: FeedConnectionState,
    message: String,
}

impl RithmicShellState {
    pub(crate) fn local() -> Result<Self, String> {
        let identity = FeedIdentity::try_new("rithmic", "RITHMIC_TEST", "Test")
            .map_err(|error| error.to_string())?;
        Ok(Self {
            identity,
            connection: FeedConnectionState::Disconnected,
            message: "Local shell ready; provider login has not started".to_string(),
        })
    }

    pub(crate) const fn connection(&self) -> FeedConnectionState {
        self.connection
    }

    pub(crate) fn profile_label(&self) -> String {
        let provider = match self.identity.provider() {
            "rithmic" => "Rithmic",
            provider => provider,
        };
        let system = match self.identity.system() {
            "RITHMIC_TEST" => "Rithmic Test",
            system => system,
        };
        format!("{provider} / {system} / {}", self.identity.environment())
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

pub(crate) const MAXIMUM_SYMBOL_QUERY_BYTES: usize = 64;
pub(crate) const MAXIMUM_RITHMIC_SYMBOL_RESULTS: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RithmicSymbolSearchRequest {
    pub(crate) request_id: NonZeroUsize,
    pub(crate) query: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RithmicSymbolSelection {
    pub(crate) generation: NonZeroUsize,
    pub(crate) search_generation: NonZeroUsize,
    pub(crate) instrument: ProviderInstrumentSummary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RithmicSymbolBrowser {
    maximum_results: usize,
    allow_empty_query: bool,
    next_search_id: usize,
    pending_search_id: Option<NonZeroUsize>,
    pending_search_query: Option<String>,
    retained_search_query: Option<String>,
    completed_search_id: Option<NonZeroUsize>,
    results: Vec<ProviderInstrumentSummary>,
    selection_generation: usize,
    pending_selection: Option<RithmicSymbolSelection>,
    selected: Option<RithmicSymbolSelection>,
}

impl Default for RithmicSymbolBrowser {
    fn default() -> Self {
        Self {
            maximum_results: MAXIMUM_RITHMIC_SYMBOL_RESULTS,
            allow_empty_query: false,
            next_search_id: 0,
            pending_search_id: None,
            pending_search_query: None,
            retained_search_query: None,
            completed_search_id: None,
            results: Vec::new(),
            selection_generation: 0,
            pending_selection: None,
            selected: None,
        }
    }
}

impl RithmicSymbolBrowser {
    pub(crate) fn rithmic_catalog_awaiting_search(request_id: NonZeroUsize, query: &str) -> Self {
        Self {
            maximum_results: MAXIMUM_RITHMIC_SYMBOL_RESULTS,
            allow_empty_query: true,
            next_search_id: request_id.get(),
            pending_search_id: Some(request_id),
            pending_search_query: Some(query.to_string()),
            ..Self::default()
        }
    }

    pub(crate) fn begin_search(
        &mut self,
        query: &str,
    ) -> Result<RithmicSymbolSearchRequest, &'static str> {
        let query = query.trim();
        self.validate_query(query)?;
        self.next_search_id = self.next_search_id.saturating_add(1).max(1);
        let request_id = NonZeroUsize::new(self.next_search_id).unwrap_or(NonZeroUsize::MIN);
        self.pending_search_id = Some(request_id);
        self.pending_search_query = Some(query.to_string());
        self.results.clear();
        Ok(RithmicSymbolSearchRequest {
            request_id,
            query: query.to_string(),
        })
    }

    pub(crate) fn apply_results(
        &mut self,
        request_id: NonZeroUsize,
        results: Vec<ProviderInstrumentSummary>,
    ) -> bool {
        let initial = self.next_search_id == 0 && self.pending_search_id.is_none();
        if self
            .pending_search_id
            .is_none_or(|pending| pending != request_id)
            && !initial
            || results.len() > self.maximum_results
        {
            return false;
        }
        self.next_search_id = self.next_search_id.max(request_id.get());
        self.pending_search_id = None;
        self.pending_search_query = None;
        self.completed_search_id = Some(request_id);
        self.results = results;
        true
    }

    pub(crate) fn retain_latest_search(&mut self, query: &str) -> Result<bool, &'static str> {
        let query = query.trim();
        if let Err(error) = self.validate_query(query) {
            self.retained_search_query = None;
            return Err(error);
        }
        let already_dispatched = self.pending_search_query.as_deref() == Some(query);
        self.retained_search_query = (!already_dispatched).then(|| query.to_string());
        Ok(already_dispatched)
    }

    pub(crate) fn begin_retained_search(&mut self) -> Option<RithmicSymbolSearchRequest> {
        if self.pending_search_id.is_some() {
            return None;
        }
        let query = self.retained_search_query.take()?;
        self.begin_search(&query).ok()
    }

    pub(crate) fn select(&mut self, index: usize) -> Option<RithmicSymbolSelection> {
        let instrument = self.results.get(index)?.clone();
        let search_generation = self.completed_search_id?;
        self.selection_generation = self.selection_generation.saturating_add(1).max(1);
        let selection = RithmicSymbolSelection {
            generation: NonZeroUsize::new(self.selection_generation).unwrap_or(NonZeroUsize::MIN),
            search_generation,
            instrument,
        };
        self.pending_selection = Some(selection.clone());
        Some(selection)
    }

    pub(crate) fn consume_completed_search(&mut self, search_generation: NonZeroUsize) -> bool {
        if self.completed_search_id != Some(search_generation) {
            return false;
        }
        self.completed_search_id = None;
        self.results.clear();
        true
    }

    pub(crate) fn confirm_selection(&mut self, generation: NonZeroUsize) -> bool {
        let Some(selection) = self
            .pending_selection
            .take_if(|selection| selection.generation == generation)
        else {
            return false;
        };
        self.selected = Some(selection);
        true
    }

    pub(crate) fn reject_search(&mut self, generation: NonZeroUsize) -> bool {
        if self.pending_search_id == Some(generation) {
            self.pending_search_id = None;
            self.pending_search_query = None;
            return true;
        }
        false
    }

    pub(crate) fn reject_selection(&mut self, generation: NonZeroUsize) -> bool {
        self.pending_selection
            .take_if(|selection| selection.generation == generation)
            .is_some()
    }

    #[cfg(test)]
    pub(crate) fn pending_search_id(&self) -> Option<NonZeroUsize> {
        self.pending_search_id
    }

    pub(crate) fn results(&self) -> &[ProviderInstrumentSummary] {
        &self.results
    }

    pub(crate) const fn maximum_results(&self) -> usize {
        self.maximum_results
    }

    pub(crate) const fn has_retained_search(&self) -> bool {
        self.retained_search_query.is_some()
    }

    pub(crate) const fn search_pending(&self) -> bool {
        self.pending_search_id.is_some()
    }

    pub(crate) fn selected(&self) -> Option<&RithmicSymbolSelection> {
        self.selected.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn invalidate_session(&mut self) {
        self.pending_search_id = None;
        self.pending_search_query = None;
        self.retained_search_query = None;
        self.completed_search_id = None;
        self.results.clear();
        self.pending_selection = None;
        self.selected = None;
    }

    fn validate_query(&self, query: &str) -> Result<(), &'static str> {
        if (!self.allow_empty_query && query.is_empty())
            || query.len() > MAXIMUM_SYMBOL_QUERY_BYTES
            || query.chars().any(char::is_control)
        {
            return Err(if self.allow_empty_query {
                "symbol search must be at most 64 printable bytes"
            } else {
                "symbol search must be 1-64 printable bytes"
            });
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) const fn connection_label(connection: FeedConnectionState) -> &'static str {
    match connection {
        FeedConnectionState::Disconnected => "Not connected",
        FeedConnectionState::Discovering => "Discovering Test systems",
        FeedConnectionState::Authenticating => "Authenticating",
        FeedConnectionState::Streaming => "Live market data",
        FeedConnectionState::Recovering => "Reconnecting",
        FeedConnectionState::Stopped => "Stopped",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(symbol: &str) -> ProviderInstrumentSummary {
        ProviderInstrumentSummary {
            symbol: symbol.to_string(),
            exchange: "CME".to_string(),
            name: Some(format!("{symbol} future")),
            product_code: Some("ES".to_string()),
            instrument_type: Some("FUTURE".to_string()),
            expiration_date: None,
        }
    }

    #[test]
    fn local_shell_is_available_before_login_or_history() {
        let shell = RithmicShellState::local().expect("fixed Rithmic Test profile validates");
        assert_eq!(shell.identity.provider(), "rithmic");
        assert_eq!(shell.identity.system(), "RITHMIC_TEST");
        assert_eq!(shell.identity.environment(), "Test");
        assert_eq!(shell.connection(), FeedConnectionState::Disconnected);
        assert_eq!(connection_label(shell.connection()), "Not connected");
        assert_eq!(shell.profile_label(), "Rithmic / Rithmic Test / Test");
        assert!(shell.message().contains("login has not started"));
    }

    #[test]
    fn symbol_browser_fences_stale_searches_and_selections() {
        let mut browser = RithmicSymbolBrowser::default();
        let first = browser.begin_search(" ES ").expect("query validates");
        assert_eq!(first.query, "ES");
        let second = browser.begin_search("NQ").expect("query validates");
        assert!(!browser.apply_results(first.request_id, vec![result("ESM7")]));
        assert!(browser.results().is_empty());
        assert!(browser.apply_results(second.request_id, vec![result("NQM7")]));

        let first_selection = browser.select(0).expect("result can be selected");
        let second_selection = browser.select(0).expect("replacement can be selected");
        assert!(second_selection.generation > first_selection.generation);
        assert!(browser.selected().is_none());
        assert!(!browser.confirm_selection(first_selection.generation));
        assert!(browser.confirm_selection(second_selection.generation));
        assert_eq!(browser.selected(), Some(&second_selection));
    }

    #[test]
    fn symbol_browser_bounds_queries_results_and_indexes() {
        let mut browser = RithmicSymbolBrowser::default();
        assert_eq!(browser.maximum_results(), MAXIMUM_RITHMIC_SYMBOL_RESULTS);
        assert!(browser.begin_search("").is_err());
        assert!(
            browser
                .begin_search(&"x".repeat(MAXIMUM_SYMBOL_QUERY_BYTES + 1))
                .is_err()
        );
        assert!(browser.begin_search("E\nS").is_err());

        let request = browser.begin_search("ES").expect("query validates");
        assert!(browser.search_pending());
        assert!(!browser.apply_results(
            request.request_id,
            vec![result("ES"); MAXIMUM_RITHMIC_SYMBOL_RESULTS + 1],
        ));
        assert!(browser.select(0).is_none());
        assert_eq!(browser.pending_search_id(), Some(request.request_id));
    }

    #[test]
    fn rithmic_catalog_keeps_empty_query_and_dynamic_result_capacity() {
        let mut browser =
            RithmicSymbolBrowser::rithmic_catalog_awaiting_search(NonZeroUsize::MIN, "");
        assert_eq!(browser.maximum_results(), MAXIMUM_RITHMIC_SYMBOL_RESULTS);
        assert!(browser.apply_results(
            NonZeroUsize::MIN,
            vec![result("BTC-USD"); MAXIMUM_RITHMIC_SYMBOL_RESULTS],
        ));
        assert_eq!(browser.results().len(), MAXIMUM_RITHMIC_SYMBOL_RESULTS);
        assert!(browser.begin_search("").is_ok());
    }

    #[test]
    fn session_invalidation_fences_catalog_and_selection_state() {
        let mut browser = RithmicSymbolBrowser::default();
        let search = browser.begin_search("ES").expect("query validates");
        assert!(browser.apply_results(search.request_id, vec![result("ESM7")]));
        let selection = browser.select(0).expect("result can be selected");
        assert!(browser.confirm_selection(selection.generation));

        browser.invalidate_session();

        assert!(browser.results().is_empty());
        assert!(browser.selected().is_none());
        assert!(!browser.apply_results(search.request_id, vec![result("ESM7")]));
        let next = browser.begin_search("ES").expect("new session can search");
        assert!(next.request_id > search.request_id);
    }

    #[test]
    fn search_and_selection_rejections_have_distinct_generation_order_bookains() {
        let mut browser = RithmicSymbolBrowser::default();
        let first_search = browser.begin_search("ES").expect("search validates");
        assert!(browser.apply_results(first_search.request_id, vec![result("ESM7")]));
        let first_selection = browser.select(0).expect("first selection");
        let second_selection = browser.select(0).expect("second selection");
        let second_search = browser.begin_search("NQ").expect("second search validates");
        assert_eq!(second_search.request_id, second_selection.generation);

        assert!(browser.reject_search(second_search.request_id));
        assert!(browser.confirm_selection(second_selection.generation));

        let third_search = browser.begin_search("YM").expect("third search validates");
        assert!(browser.apply_results(third_search.request_id, vec![result("YMM7")]));
        let third_selection = browser.select(0).unwrap_or(first_selection);
        let fourth_search = browser
            .begin_search("RTY")
            .expect("fourth search validates");
        assert!(browser.reject_selection(third_selection.generation));
        assert!(browser.apply_results(fourth_search.request_id, vec![result("RTYM7")]));
    }

    #[test]
    fn rejected_replacement_preserves_the_confirmed_selection() {
        let mut browser = RithmicSymbolBrowser::default();
        let search = browser.begin_search("ES").expect("search validates");
        assert!(browser.apply_results(search.request_id, vec![result("ESM7"), result("ESU7")]));
        let current = browser.select(0).expect("current selection");
        assert!(browser.confirm_selection(current.generation));
        let replacement = browser.select(1).expect("replacement selection");

        assert!(browser.reject_selection(replacement.generation));
        assert_eq!(browser.selected(), Some(&current));
    }

    #[test]
    fn rithmic_startup_search_and_rapid_typing_dispatch_only_the_latest_query() {
        let startup = NonZeroUsize::MIN;
        let mut browser = RithmicSymbolBrowser::rithmic_catalog_awaiting_search(startup, "");
        browser
            .retain_latest_search("B")
            .expect("first typed query validates");
        browser
            .retain_latest_search("BT")
            .expect("replacement typed query validates");
        browser
            .retain_latest_search("BTC")
            .expect("latest typed query validates");

        assert!(browser.apply_results(startup, vec![result("BTC-USD")]));
        let latest = browser
            .begin_retained_search()
            .expect("latest query eventually dispatches");
        assert_eq!(latest.query, "BTC");
        assert!(latest.request_id > startup);
        assert!(browser.begin_retained_search().is_none());
    }

    #[test]
    fn rithmic_rapid_typing_dispatches_only_the_latest_nonempty_query() {
        let mut browser = RithmicSymbolBrowser::default();
        let active = browser.begin_search("E").expect("prefix validates");
        assert!(!browser.retain_latest_search("ES").expect("query validates"));
        assert!(
            !browser
                .retain_latest_search("ESM")
                .expect("query validates")
        );

        assert!(browser.apply_results(active.request_id, vec![result("ESU6")]));
        let latest = browser
            .begin_retained_search()
            .expect("latest Rithmic query eventually dispatches");
        assert_eq!(latest.query, "ESM");
        assert!(latest.request_id > active.request_id);
    }

    #[test]
    fn enter_on_the_matching_pending_query_neither_duplicates_nor_retain_search() {
        let mut browser = RithmicSymbolBrowser::default();
        let active = browser.begin_search("ES").expect("query validates");

        assert!(
            browser
                .retain_latest_search(" ES ")
                .expect("query validates")
        );
        assert!(browser.search_pending());
        assert!(!browser.has_retained_search());
        assert!(browser.results().is_empty());

        assert!(browser.apply_results(active.request_id, vec![result("ESU6")]));
        assert!(browser.begin_retained_search().is_none());
        assert_eq!(browser.results()[0].symbol, "ESU6");
    }

    #[test]
    fn invalid_latest_rithmic_query_cancels_an_older_retained_prefix() {
        let mut browser = RithmicSymbolBrowser::default();
        let active = browser.begin_search("E").expect("prefix validates");
        assert!(!browser.retain_latest_search("ES").expect("query validates"));
        assert!(browser.retain_latest_search("").is_err());

        assert!(browser.apply_results(active.request_id, vec![result("ESU6")]));
        assert!(browser.begin_retained_search().is_none());
    }

    #[test]
    fn retyping_active_query_cancels_an_obsolete_retained_query() {
        let mut browser = RithmicSymbolBrowser::default();
        let active = browser.begin_search("BTC").expect("query validates");
        browser
            .retain_latest_search("ETH")
            .expect("replacement validates");
        browser
            .retain_latest_search("BTC")
            .expect("active query validates");

        assert!(browser.apply_results(active.request_id, vec![result("BTC-USD")]));
        assert!(browser.begin_retained_search().is_none());
    }

    #[test]
    fn consuming_completed_search_prevents_reusing_selection_authorization() {
        let mut browser =
            RithmicSymbolBrowser::rithmic_catalog_awaiting_search(NonZeroUsize::MIN, "");
        assert!(browser.apply_results(NonZeroUsize::MIN, vec![result("BTC-USD")]));
        let selection = browser.select(0).expect("catalog result is selectable");
        assert!(browser.confirm_selection(selection.generation));

        assert!(browser.consume_completed_search(selection.search_generation));
        assert!(browser.results().is_empty());
        assert!(browser.select(0).is_none());
        assert!(!browser.consume_completed_search(selection.search_generation));
        assert_eq!(browser.selected(), Some(&selection));
    }
}
