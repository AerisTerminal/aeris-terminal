//! The symbol menu's searches across its scope's providers.
//!
//! Each provider in scope answers in its own lane. A new query is sent to every lane at once
//! and replaces whatever a lane was still waiting for, so a slow provider never delays typing
//! or another provider's results; replies are fenced by their search identity. The menu lists
//! every lane's answer as one flat list in the fixed provider order.

use super::{TerminalProvider, known_terminal_provider};
use aeris_contracts::{ProviderInstrumentKind, ProviderInstrumentSummary};
use aeris_desktop::chart_chrome::{ExchangeCode, SymbolSearchScope};
use std::num::NonZeroUsize;

pub(crate) const MAXIMUM_SYMBOL_QUERY_BYTES: usize = 64;
pub(crate) const MAXIMUM_SYMBOL_RESULTS: usize = 256;
/// Exchanges remembered per provider for the scope menu.
const MAXIMUM_LEARNED_EXCHANGES: usize = 32;

/// Which part of a lane a search fills.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SymbolSearchLane {
    /// The typed query, or the provider's listing for an empty one.
    Query,
    /// The contracts of the product being opened.
    ProductContracts,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SymbolSearchRequest {
    pub(crate) provider: TerminalProvider,
    pub(crate) request_id: NonZeroUsize,
    pub(crate) query: String,
    pub(crate) lane: SymbolSearchLane,
    pub(crate) exchange: Option<ExchangeCode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SymbolSelection {
    pub(crate) generation: NonZeroUsize,
    pub(crate) search_generation: NonZeroUsize,
    pub(crate) provider: TerminalProvider,
    pub(crate) instrument: ProviderInstrumentSummary,
}

/// A product on one provider exchange; searching its code lists its contracts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProductKey {
    pub(crate) provider: TerminalProvider,
    pub(crate) code: String,
    pub(crate) exchange: String,
}

impl ProductKey {
    fn of(provider: TerminalProvider, instrument: &ProviderInstrumentSummary) -> Option<Self> {
        product_code(instrument).map(|code| Self {
            provider,
            code: code.to_string(),
            exchange: instrument.exchange.clone(),
        })
    }

    fn names(&self, instrument: &ProviderInstrumentSummary) -> bool {
        product_code(instrument) == Some(self.code.as_str()) && instrument.exchange == self.exchange
    }
}

/// The product code a result belongs to. A listed product is its own code.
fn product_code(instrument: &ProviderInstrumentSummary) -> Option<&str> {
    let code = match instrument.kind {
        ProviderInstrumentKind::Product => instrument
            .product_code
            .as_deref()
            .unwrap_or(&instrument.symbol),
        ProviderInstrumentKind::Instrument | ProviderInstrumentKind::FrontMonthAlias => {
            instrument.product_code.as_deref()?
        }
    };
    (!code.is_empty()).then_some(code)
}

/// A selectable instrument shown by the instrument menu.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstrumentMenuSelection {
    /// An instrument of a lane's latest completed search.
    SearchResult {
        provider: TerminalProvider,
        index: usize,
    },
    /// A contract of the product being opened.
    ProductContract {
        provider: TerminalProvider,
        index: usize,
    },
}

impl InstrumentMenuSelection {
    pub(crate) const fn provider(self) -> TerminalProvider {
        match self {
            Self::SearchResult { provider, .. } | Self::ProductContract { provider, .. } => {
                provider
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InstrumentMenuAction {
    Select(InstrumentMenuSelection),
    /// Opens the product's nearest-expiry contract; a product itself is never selected.
    OpenProduct(ProductKey),
    /// A product without a code to search.
    Inert,
}

/// One instrument-menu row, derived from accepted browser state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstrumentMenuEntry {
    pub(crate) provider: TerminalProvider,
    pub(crate) label: String,
    /// Supporting text after the label, such as a product name and contract month.
    pub(crate) description: Option<String>,
    /// Muted trailing text: the provider's asset class and, for a product or a contract, its
    /// exchange.
    pub(crate) detail: Option<String>,
    pub(crate) checked: bool,
    pub(crate) action: InstrumentMenuAction,
}

/// What the lanes' latest completed searches list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SymbolListingSummary {
    Products(usize),
    Contracts(usize),
    Markets(usize),
}

impl SymbolListingSummary {
    const fn count(self) -> usize {
        match self {
            Self::Products(count) | Self::Contracts(count) | Self::Markets(count) => count,
        }
    }

    /// Two lanes' summaries together; a mix of kinds counts as markets.
    const fn combine(self, other: Self) -> Self {
        let count = self.count() + other.count();
        match (self, other) {
            (Self::Products(_), Self::Products(_)) => Self::Products(count),
            (Self::Contracts(_), Self::Contracts(_)) => Self::Contracts(count),
            _ => Self::Markets(count),
        }
    }
}

/// How the lanes stand, for the menu footer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SymbolBrowserStatus {
    Searching,
    Listed(SymbolListingSummary),
    /// Every answering provider completed or failed without a match.
    NoMatches,
    Idle,
}

/// Whether an exact symbol is listed, for a typed symbol that selects its market directly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExactSymbolMatch {
    Found(InstrumentMenuSelection),
    /// An earlier provider in the scope has not answered yet.
    Pending,
    Missing,
}

/// Why a lane's search ended without results. Neither is shown as an error: the lane lists
/// nothing, and an unavailable lane answers again once its provider's catalog is ready.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SymbolSearchFailure {
    /// The provider has no active catalog session.
    Unavailable,
    Failed,
}

/// What opening a product came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OpenedProduct {
    /// The product's nearest-expiry contract, ready to select.
    Contract(InstrumentMenuSelection),
    NoContracts(ProductKey),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingSymbolSearch {
    id: NonZeroUsize,
    query: String,
    lane: SymbolSearchLane,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ProductContracts {
    Pending(NonZeroUsize),
    Listed {
        search_id: NonZeroUsize,
        contracts: Vec<ProviderInstrumentSummary>,
        /// Whether its contract was handed out for selection.
        opened: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProductOpening {
    product: ProductKey,
    contracts: ProductContracts,
}

/// One provider's searches.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ProviderLane {
    provider: TerminalProvider,
    pending: Option<PendingSymbolSearch>,
    completed_search_id: Option<NonZeroUsize>,
    completed_query: Option<String>,
    failure: Option<SymbolSearchFailure>,
    results: Vec<ProviderInstrumentSummary>,
    /// The latest product listing. A typed search replaces `results`, and its contracts are
    /// still described by these products' names.
    products: Vec<ProviderInstrumentSummary>,
    opening: Option<ProductOpening>,
}

impl ProviderLane {
    const fn new(provider: TerminalProvider) -> Self {
        Self {
            provider,
            pending: None,
            completed_search_id: None,
            completed_query: None,
            failure: None,
            results: Vec::new(),
            products: Vec::new(),
            opening: None,
        }
    }

    fn query_pending(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|search| search.lane == SymbolSearchLane::Query)
    }

    fn lists(&self, query: &str) -> bool {
        !self.results.is_empty() && self.completed_query.as_deref() == Some(query)
    }

    fn summary(&self) -> Option<SymbolListingSummary> {
        if self.results.is_empty() {
            return None;
        }
        let products = self
            .results
            .iter()
            .filter(|result| result.kind == ProviderInstrumentKind::Product)
            .count();
        Some(if products > 0 {
            SymbolListingSummary::Products(products)
        } else if self
            .results
            .iter()
            .any(|result| product_code(result).is_some())
        {
            SymbolListingSummary::Contracts(self.results.len())
        } else {
            SymbolListingSummary::Markets(self.results.len())
        })
    }

    fn selectable(
        &self,
        selection: InstrumentMenuSelection,
    ) -> Option<(&ProviderInstrumentSummary, NonZeroUsize)> {
        let (instrument, search_generation) = match selection {
            InstrumentMenuSelection::SearchResult { index, .. } => {
                (self.results.get(index)?, self.completed_search_id?)
            }
            InstrumentMenuSelection::ProductContract { index, .. } => {
                match &self.opening.as_ref()?.contracts {
                    ProductContracts::Listed {
                        search_id,
                        contracts,
                        ..
                    } => (contracts.get(index)?, *search_id),
                    ProductContracts::Pending(_) => return None,
                }
            }
        };
        (instrument.kind == ProviderInstrumentKind::Instrument)
            .then_some((instrument, search_generation))
    }

    /// The lane's rows in its provider's order: listed products open their nearest contract,
    /// and every contract or market is its own row.
    fn push_entries(
        &self,
        entries: &mut Vec<InstrumentMenuEntry>,
        selected: Option<&SymbolSelection>,
    ) {
        for (index, result) in self.results.iter().enumerate() {
            entries.push(if result.kind == ProviderInstrumentKind::Product {
                self.product_entry(result)
            } else {
                self.contract_entry(result, index, selected)
            });
        }
    }

    fn product_entry(&self, listed: &ProviderInstrumentSummary) -> InstrumentMenuEntry {
        let product = ProductKey::of(self.provider, listed);
        let opening = self.opening.as_ref().is_some_and(|opening| {
            product.as_ref() == Some(&opening.product)
                && matches!(opening.contracts, ProductContracts::Pending(_))
        });
        InstrumentMenuEntry {
            provider: self.provider,
            label: listed.display_symbol.clone(),
            description: listed.name.clone(),
            detail: if opening {
                Some("Opening".to_string())
            } else {
                product_detail(listed.asset_class.as_deref(), &listed.exchange)
            },
            checked: false,
            action: product.map_or(
                InstrumentMenuAction::Inert,
                InstrumentMenuAction::OpenProduct,
            ),
        }
    }

    fn contract_entry(
        &self,
        instrument: &ProviderInstrumentSummary,
        index: usize,
        selected: Option<&SymbolSelection>,
    ) -> InstrumentMenuEntry {
        let product = ProductKey::of(self.provider, instrument);
        let product_name = product
            .as_ref()
            .and_then(|product| self.products.iter().find(|listed| product.names(listed)))
            .and_then(|listed| listed.name.as_deref());
        InstrumentMenuEntry {
            provider: self.provider,
            label: instrument.display_symbol.clone(),
            description: contract_description(product_name, instrument),
            // A product's contracts name their exchange so a delayed copy reads apart from
            // its real-time one.
            detail: if product.is_some() {
                product_detail(instrument.asset_class.as_deref(), &instrument.exchange)
            } else {
                instrument.asset_class.clone()
            },
            checked: selected.is_some_and(|selected| {
                selected.provider == self.provider
                    && selected.instrument.symbol == instrument.symbol
                    && selected.instrument.exchange == instrument.exchange
            }),
            action: InstrumentMenuAction::Select(InstrumentMenuSelection::SearchResult {
                provider: self.provider,
                index,
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SymbolBrowser {
    scope: SymbolSearchScope,
    /// One lane per provider in scope, in the fixed provider order.
    lanes: Vec<ProviderLane>,
    maximum_results: usize,
    next_search_id: usize,
    /// The latest query sent to the lanes.
    query: Option<String>,
    exchanges: Vec<(TerminalProvider, Vec<ExchangeCode>)>,
    selection_generation: usize,
    pending_selection: Option<SymbolSelection>,
    selected: Option<SymbolSelection>,
}

impl SymbolBrowser {
    pub(crate) fn new(scope: SymbolSearchScope) -> Self {
        Self {
            scope,
            lanes: scope_lanes(scope),
            maximum_results: MAXIMUM_SYMBOL_RESULTS,
            next_search_id: 0,
            query: None,
            exchanges: Vec::new(),
            selection_generation: 0,
            pending_selection: None,
            selected: None,
        }
    }

    pub(crate) const fn scope(&self) -> SymbolSearchScope {
        self.scope
    }

    /// Changes which providers answer. Every search is forgotten; request generations keep
    /// increasing, and the chart's confirmed selection and the learned exchanges stay.
    pub(crate) fn set_scope(&mut self, scope: SymbolSearchScope) -> bool {
        if self.scope == scope {
            return false;
        }
        self.scope = scope;
        self.lanes = scope_lanes(scope);
        self.query = None;
        self.pending_selection = None;
        true
    }

    /// Whether a provider answers in the current scope.
    pub(crate) fn covers(&self, provider: TerminalProvider) -> bool {
        self.lanes.iter().any(|lane| lane.provider == provider)
    }

    /// The single provider in scope, when the scope names one.
    pub(crate) fn scoped_provider(&self) -> Option<TerminalProvider> {
        self.scope.provider().and_then(known_terminal_provider)
    }

    fn next_request_id(&mut self) -> NonZeroUsize {
        self.next_search_id = self.next_search_id.saturating_add(1).max(1);
        NonZeroUsize::new(self.next_search_id).unwrap_or(NonZeroUsize::MIN)
    }

    /// Sends `query` to every lane at once. A lane still waiting for an older search drops
    /// that reply when it arrives.
    pub(crate) fn begin_search(
        &mut self,
        query: &str,
    ) -> Result<Vec<SymbolSearchRequest>, &'static str> {
        let query = query.trim();
        validate_query(query)?;
        let request_id = self.next_request_id();
        let exchange = self.scope.exchange();
        self.query = Some(query.to_string());
        Ok(self
            .lanes
            .iter_mut()
            .map(|lane| {
                lane.results.clear();
                lane.opening = None;
                lane.failure = None;
                lane.pending = Some(PendingSymbolSearch {
                    id: request_id,
                    query: query.to_string(),
                    lane: SymbolSearchLane::Query,
                });
                SymbolSearchRequest {
                    provider: lane.provider,
                    request_id,
                    query: query.to_string(),
                    lane: SymbolSearchLane::Query,
                    exchange,
                }
            })
            .collect())
    }

    /// Re-sends the latest query to a lane whose provider's catalog just became ready, when
    /// that lane has not answered it.
    pub(crate) fn retry_lane(&mut self, provider: TerminalProvider) -> Option<SymbolSearchRequest> {
        let query = self.query.clone().unwrap_or_default();
        let exchange = self.scope.exchange();
        let lane_index = self.lanes.iter().position(|lane| {
            lane.provider == provider
                && lane.pending.is_none()
                && (lane.failure.is_some() || lane.completed_search_id.is_none())
        })?;
        let request_id = self.next_request_id();
        let lane = &mut self.lanes[lane_index];
        lane.failure = None;
        lane.pending = Some(PendingSymbolSearch {
            id: request_id,
            query: query.clone(),
            lane: SymbolSearchLane::Query,
        });
        Some(SymbolSearchRequest {
            provider,
            request_id,
            query,
            lane: SymbolSearchLane::Query,
            exchange,
        })
    }

    pub(crate) fn apply_results(
        &mut self,
        provider: TerminalProvider,
        request_id: NonZeroUsize,
        mut results: Vec<ProviderInstrumentSummary>,
    ) -> bool {
        if results.len() > self.maximum_results {
            return false;
        }
        let Some(lane) = self.lanes.iter_mut().find(|lane| lane.provider == provider) else {
            return false;
        };
        let Some(search) = lane.pending.take_if(|search| search.id == request_id) else {
            return false;
        };
        // The dated contract an alias follows is listed beside it.
        results.retain(|result| result.kind != ProviderInstrumentKind::FrontMonthAlias);
        learn_exchanges(&mut self.exchanges, provider, &results);
        match search.lane {
            SymbolSearchLane::Query => {
                if results
                    .iter()
                    .any(|result| result.kind == ProviderInstrumentKind::Product)
                {
                    lane.products = results
                        .iter()
                        .filter(|result| result.kind == ProviderInstrumentKind::Product)
                        .cloned()
                        .collect();
                }
                lane.completed_search_id = Some(request_id);
                lane.completed_query = Some(search.query);
                lane.failure = None;
                lane.results = results;
            }
            SymbolSearchLane::ProductContracts => {
                // A product replaced while its search ran drops the reply.
                if let Some(opening) = lane
                    .opening
                    .as_mut()
                    .filter(|opening| opening.contracts == ProductContracts::Pending(request_id))
                {
                    let contracts = results
                        .into_iter()
                        .filter(|result| {
                            result.kind == ProviderInstrumentKind::Instrument
                                && opening.product.names(result)
                        })
                        .collect();
                    opening.contracts = ProductContracts::Listed {
                        search_id: request_id,
                        contracts,
                        opened: false,
                    };
                }
            }
        }
        true
    }

    /// Starts listing a product's contracts so its nearest-expiry one can open. Opening
    /// another product replaces it; a lane refreshing its listing opens nothing.
    pub(crate) fn open_product(&mut self, product: &ProductKey) -> Option<SymbolSearchRequest> {
        let exchange = self.scope.exchange();
        let lane_index = self.lanes.iter().position(|lane| {
            lane.provider == product.provider
                && !lane.query_pending()
                && lane.results.iter().any(|result| {
                    result.kind == ProviderInstrumentKind::Product && product.names(result)
                })
        })?;
        let request_id = self.next_request_id();
        for lane in &mut self.lanes {
            lane.opening = None;
        }
        let lane = &mut self.lanes[lane_index];
        lane.opening = Some(ProductOpening {
            product: product.clone(),
            contracts: ProductContracts::Pending(request_id),
        });
        lane.pending = Some(PendingSymbolSearch {
            id: request_id,
            query: product.code.clone(),
            lane: SymbolSearchLane::ProductContracts,
        });
        Some(SymbolSearchRequest {
            provider: product.provider,
            request_id,
            query: product.code.clone(),
            lane: SymbolSearchLane::ProductContracts,
            exchange,
        })
    }

    /// The opened product's nearest-expiry contract once its contracts are listed, handed out
    /// once. A product without contracts is forgotten.
    pub(crate) fn take_opened_product(&mut self) -> Option<OpenedProduct> {
        for lane in &mut self.lanes {
            let Some(opening) = lane.opening.as_mut() else {
                continue;
            };
            let ProductContracts::Listed {
                contracts, opened, ..
            } = &mut opening.contracts
            else {
                continue;
            };
            if *opened {
                continue;
            }
            let Some(index) = nearest_expiry(contracts.iter().enumerate()) else {
                let product = opening.product.clone();
                lane.opening = None;
                return Some(OpenedProduct::NoContracts(product));
            };
            *opened = true;
            return Some(OpenedProduct::Contract(
                InstrumentMenuSelection::ProductContract {
                    provider: lane.provider,
                    index,
                },
            ));
        }
        None
    }

    pub(crate) fn select(&mut self, selection: InstrumentMenuSelection) -> Option<SymbolSelection> {
        let provider = selection.provider();
        let (instrument, search_generation) = self
            .lanes
            .iter()
            .find(|lane| lane.provider == provider)?
            .selectable(selection)?;
        let instrument = instrument.clone();
        self.selection_generation = self.selection_generation.saturating_add(1).max(1);
        let selection = SymbolSelection {
            generation: NonZeroUsize::new(self.selection_generation).unwrap_or(NonZeroUsize::MIN),
            search_generation,
            provider,
            instrument,
        };
        self.pending_selection = Some(selection.clone());
        Some(selection)
    }

    /// A chart selection is authorized by its search once; afterwards that search's results
    /// are cleared so they cannot select again.
    pub(crate) fn consume_completed_search(
        &mut self,
        provider: TerminalProvider,
        search_generation: NonZeroUsize,
    ) -> bool {
        let Some(lane) = self.lanes.iter_mut().find(|lane| lane.provider == provider) else {
            return false;
        };
        if lane.completed_search_id == Some(search_generation) {
            lane.completed_search_id = None;
            lane.completed_query = None;
            lane.results.clear();
            lane.opening = None;
            return true;
        }
        let opened = lane.opening.as_ref().is_some_and(|opening| {
            matches!(
                opening.contracts,
                ProductContracts::Listed { search_id, .. } if search_id == search_generation
            )
        });
        if opened {
            lane.opening = None;
        }
        opened
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

    pub(crate) fn resolve_selection(
        &mut self,
        generation: NonZeroUsize,
    ) -> Option<SymbolSelection> {
        self.pending_selection
            .take_if(|selection| selection.generation == generation)
    }

    pub(crate) fn pending_selection_provider(&self) -> Option<TerminalProvider> {
        self.pending_selection
            .as_ref()
            .map(|selection| selection.provider)
    }

    /// Ends a lane's pending search. A rejected contract search forgets its product so
    /// opening it again retries; a rejected query search records why.
    pub(crate) fn reject_search(
        &mut self,
        provider: TerminalProvider,
        generation: NonZeroUsize,
        failure: SymbolSearchFailure,
    ) -> bool {
        let Some(lane) = self.lanes.iter_mut().find(|lane| lane.provider == provider) else {
            return false;
        };
        let Some(search) = lane.pending.take_if(|search| search.id == generation) else {
            return false;
        };
        match search.lane {
            SymbolSearchLane::Query => lane.failure = Some(failure),
            SymbolSearchLane::ProductContracts => {
                if lane.opening.as_ref().is_some_and(|opening| {
                    opening.contracts == ProductContracts::Pending(generation)
                }) {
                    lane.opening = None;
                }
            }
        }
        true
    }

    pub(crate) fn reject_selection(&mut self, generation: NonZeroUsize) -> bool {
        self.pending_selection
            .take_if(|selection| selection.generation == generation)
            .is_some()
    }

    /// Finds an exact symbol in provider order, so an earlier provider's market wins over a
    /// later one's even when the later one answers first. A product code finds that
    /// product's nearest-expiry contract.
    pub(crate) fn exact_symbol(&self, symbol: &str) -> ExactSymbolMatch {
        for lane in &self.lanes {
            if lane.query_pending() {
                return ExactSymbolMatch::Pending;
            }
            let instruments = || {
                lane.results
                    .iter()
                    .enumerate()
                    .filter(|(_, instrument)| instrument.kind == ProviderInstrumentKind::Instrument)
            };
            let index = instruments()
                .find(|(_, instrument)| {
                    instrument.symbol.eq_ignore_ascii_case(symbol)
                        || instrument.display_symbol.eq_ignore_ascii_case(symbol)
                })
                .map(|(index, _)| index)
                .or_else(|| {
                    nearest_expiry(instruments().filter(|(_, instrument)| {
                        instrument
                            .product_code
                            .as_deref()
                            .is_some_and(|code| code.eq_ignore_ascii_case(symbol))
                    }))
                });
            if let Some(index) = index {
                return ExactSymbolMatch::Found(InstrumentMenuSelection::SearchResult {
                    provider: lane.provider,
                    index,
                });
            }
        }
        ExactSymbolMatch::Missing
    }

    /// Whether any query has been sent since the scope was last chosen.
    pub(crate) const fn searched(&self) -> bool {
        self.query.is_some()
    }

    pub(crate) fn has_results(&self) -> bool {
        self.lanes.iter().any(|lane| !lane.results.is_empty())
    }

    /// Whether the shown results answer `query`, so reopening the menu needs no new search.
    pub(crate) fn lists_query(&self, query: &str) -> bool {
        let query = query.trim();
        self.lanes.iter().any(|lane| lane.lists(query))
    }

    pub(crate) fn search_pending(&self) -> bool {
        self.lanes.iter().any(ProviderLane::query_pending)
    }

    pub(crate) const fn maximum_results(&self) -> usize {
        self.maximum_results
    }

    pub(crate) fn selected(&self) -> Option<&SymbolSelection> {
        self.selected.as_ref()
    }

    /// Exchanges a provider's results have named, in the order they were first seen.
    #[cfg(test)]
    pub(crate) fn exchanges(&self, provider: TerminalProvider) -> &[ExchangeCode] {
        self.exchanges
            .iter()
            .find(|(candidate, _)| *candidate == provider)
            .map_or(&[], |(_, exchanges)| exchanges.as_slice())
    }

    /// Every provider's learned exchanges, for the scope menu.
    pub(crate) fn learned_exchanges(&self) -> &[(TerminalProvider, Vec<ExchangeCode>)] {
        &self.exchanges
    }

    pub(crate) fn status(&self) -> SymbolBrowserStatus {
        if self.search_pending() {
            return SymbolBrowserStatus::Searching;
        }
        let listed = self
            .lanes
            .iter()
            .filter_map(ProviderLane::summary)
            .reduce(SymbolListingSummary::combine);
        if let Some(summary) = listed {
            return SymbolBrowserStatus::Listed(summary);
        }
        let answered = self.lanes.iter().any(|lane| {
            lane.completed_search_id.is_some() || lane.failure == Some(SymbolSearchFailure::Failed)
        });
        if answered {
            SymbolBrowserStatus::NoMatches
        } else {
            SymbolBrowserStatus::Idle
        }
    }

    /// The menu rows: every lane's rows in the fixed provider order.
    pub(crate) fn menu_entries(&self) -> Vec<InstrumentMenuEntry> {
        let mut entries = Vec::new();
        for lane in &self.lanes {
            lane.push_entries(&mut entries, self.selected.as_ref());
        }
        entries
    }
}

/// The lanes of a scope. A scope naming a provider this build does not expose has none.
fn scope_lanes(scope: SymbolSearchScope) -> Vec<ProviderLane> {
    match scope.provider() {
        None => TerminalProvider::ALL
            .into_iter()
            .filter(|provider| super::provider_presentation(*provider).is_some())
            .map(ProviderLane::new)
            .collect(),
        Some(provider) => known_terminal_provider(provider)
            .map(ProviderLane::new)
            .into_iter()
            .collect(),
    }
}

/// The index of the earliest-expiring contract; the first one wins a tie, so a provider's
/// real-time listing is preferred when it orders it first. Without any expiration the first
/// contract is the nearest.
fn nearest_expiry<'a>(
    contracts: impl Iterator<Item = (usize, &'a ProviderInstrumentSummary)>,
) -> Option<usize> {
    contracts
        .min_by_key(|(_, contract)| {
            let expiration = contract.expiration_date.as_deref().map(expiration_key);
            (expiration.is_none(), expiration)
        })
        .map(|(index, _)| index)
}

/// An expiration's digits, so `YYYYMMDD` and `YYYY-MM-DD` order by date.
fn expiration_key(expiration: &str) -> Vec<u8> {
    expiration.bytes().filter(u8::is_ascii_digit).collect()
}

fn learn_exchanges(
    learned: &mut Vec<(TerminalProvider, Vec<ExchangeCode>)>,
    provider: TerminalProvider,
    results: &[ProviderInstrumentSummary],
) {
    let index = learned
        .iter()
        .position(|(candidate, _)| *candidate == provider)
        .unwrap_or_else(|| {
            learned.push((provider, Vec::new()));
            learned.len() - 1
        });
    let exchanges = &mut learned[index].1;
    for result in results {
        if exchanges.len() >= MAXIMUM_LEARNED_EXCHANGES {
            return;
        }
        if let Some(code) = ExchangeCode::new(&result.exchange)
            && !exchanges.contains(&code)
        {
            exchanges.push(code);
        }
    }
}

/// An empty query asks each provider catalog for its default listing; the runtime catalog
/// owns whether a provider accepts it.
fn validate_query(query: &str) -> Result<(), &'static str> {
    if query.len() > MAXIMUM_SYMBOL_QUERY_BYTES || query.chars().any(char::is_control) {
        return Err("symbol search must be at most 64 printable bytes");
    }
    Ok(())
}

fn product_detail(asset_class: Option<&str>, exchange: &str) -> Option<String> {
    let parts = asset_class
        .into_iter()
        .chain([exchange])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// A dated contract reads as its product's name and its month, the listed product's name
/// when the listing has one and the provider's own contract name otherwise.
fn contract_description(
    product_name: Option<&str>,
    contract: &ProviderInstrumentSummary,
) -> Option<String> {
    let month = contract.expiration_date.as_deref().and_then(contract_month);
    match (product_name.or(contract.name.as_deref()), month) {
        (Some(name), Some(month)) => Some(format!("{name} ({month})")),
        (Some(name), None) => Some(name.to_string()),
        (None, month) => month,
    }
}

/// The month of a `YYYYMM`, `YYYYMMDD` or `YYYY-MM-DD` expiration, such as "Dec 2026".
fn contract_month(expiration: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    if !expiration
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'-')
    {
        return None;
    }
    let digits = expiration
        .bytes()
        .filter(u8::is_ascii_digit)
        .collect::<Vec<_>>();
    if !matches!(digits.len(), 6 | 8) {
        return None;
    }
    let year = std::str::from_utf8(&digits[..4]).ok()?;
    let month = usize::from(digits[4] - b'0') * 10 + usize::from(digits[5] - b'0');
    let name = MONTHS.get(month.checked_sub(1)?)?;
    Some(format!("{name} {year}"))
}

#[cfg(test)]
#[path = "symbol_browser_tests.rs"]
mod tests;
