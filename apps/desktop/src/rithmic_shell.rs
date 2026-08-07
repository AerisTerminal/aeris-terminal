use axiusflow_observability::{FeedConnectionState, FeedIdentity};
use axiusflow_rithmic_protocol_adapter::{RithmicProviderConfig, SymbolSearchResult};
use std::num::NonZeroUsize;

pub(crate) const MAXIMUM_SYMBOL_QUERY_BYTES: usize = 64;
pub(crate) const MAXIMUM_SYMBOL_RESULTS: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RithmicSymbolSearchRequest {
    pub(crate) request_id: NonZeroUsize,
    pub(crate) query: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RithmicSymbolSelection {
    pub(crate) generation: NonZeroUsize,
    pub(crate) search_generation: NonZeroUsize,
    pub(crate) instrument: SymbolSearchResult,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RithmicSymbolBrowser {
    next_search_id: usize,
    pending_search_id: Option<NonZeroUsize>,
    completed_search_id: Option<NonZeroUsize>,
    results: Vec<SymbolSearchResult>,
    selection_generation: usize,
    pending_selection: Option<RithmicSymbolSelection>,
    selected: Option<RithmicSymbolSelection>,
}

impl RithmicSymbolBrowser {
    pub(crate) fn begin_search(
        &mut self,
        query: &str,
    ) -> Result<RithmicSymbolSearchRequest, &'static str> {
        let query = query.trim();
        if query.is_empty()
            || query.len() > MAXIMUM_SYMBOL_QUERY_BYTES
            || query.chars().any(char::is_control)
        {
            return Err("symbol search must be 1-64 printable bytes");
        }
        self.next_search_id = self.next_search_id.saturating_add(1).max(1);
        let request_id = NonZeroUsize::new(self.next_search_id).unwrap_or(NonZeroUsize::MIN);
        self.pending_search_id = Some(request_id);
        self.results.clear();
        Ok(RithmicSymbolSearchRequest {
            request_id,
            query: query.to_string(),
        })
    }

    pub(crate) fn apply_results(
        &mut self,
        request_id: NonZeroUsize,
        results: Vec<SymbolSearchResult>,
    ) -> bool {
        if self.pending_search_id != Some(request_id) || results.len() > MAXIMUM_SYMBOL_RESULTS {
            return false;
        }
        self.pending_search_id = None;
        self.completed_search_id = Some(request_id);
        self.results = results;
        true
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

    pub(crate) fn reject_command(&mut self, generation: NonZeroUsize) -> bool {
        if self.pending_search_id == Some(generation) {
            self.pending_search_id = None;
            return true;
        }
        self.pending_selection
            .take_if(|selection| selection.generation == generation)
            .is_some()
    }

    #[cfg(test)]
    pub(crate) fn pending_search_id(&self) -> Option<NonZeroUsize> {
        self.pending_search_id
    }

    pub(crate) fn results(&self) -> &[SymbolSearchResult] {
        &self.results
    }

    pub(crate) fn selected(&self) -> Option<&RithmicSymbolSelection> {
        self.selected.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RithmicShellState {
    identity: FeedIdentity,
    connection: FeedConnectionState,
    message: String,
}

impl RithmicShellState {
    pub(crate) fn local() -> Result<Self, String> {
        let environment = RithmicProviderConfig::environment();
        let identity = FeedIdentity::try_new(
            environment.provider_id,
            environment.system_id,
            environment.environment,
        )
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
        format!("{provider} · {system} · {}", self.identity.environment())
    }
}

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

impl RithmicShellState {
    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(symbol: &str) -> SymbolSearchResult {
        SymbolSearchResult {
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
        assert_eq!(shell.profile_label(), "Rithmic · Rithmic Test · Test");
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
        assert!(browser.begin_search("").is_err());
        assert!(
            browser
                .begin_search(&"x".repeat(MAXIMUM_SYMBOL_QUERY_BYTES + 1))
                .is_err()
        );
        assert!(browser.begin_search("E\nS").is_err());

        let request = browser.begin_search("ES").expect("query validates");
        assert!(!browser.apply_results(
            request.request_id,
            vec![result("ES"); MAXIMUM_SYMBOL_RESULTS + 1],
        ));
        assert!(browser.select(0).is_none());
        assert_eq!(browser.pending_search_id(), Some(request.request_id));
    }
}
