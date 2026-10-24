//! The symbol menu's searches across its scope's providers.
//!
//! Each provider in scope answers in its own lane. A new query is sent to every lane at once
//! and replaces whatever a lane was still waiting for, so a slow provider never delays typing
//! or another provider's results; replies are fenced by their search identity.

use super::{TerminalProvider, known_terminal_provider, terminal_provider_display};
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
    /// The contracts of the expanded product.
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
        ProviderInstrumentKind::Instrument => instrument.product_code.as_deref()?,
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
    /// A contract of the expanded product.
    ExpandedContract {
        provider: TerminalProvider,
        index: usize,
    },
}

impl InstrumentMenuSelection {
    pub(crate) const fn provider(self) -> TerminalProvider {
        match self {
            Self::SearchResult { provider, .. } | Self::ExpandedContract { provider, .. } => {
                provider
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InstrumentMenuAction {
    Select(InstrumentMenuSelection),
    /// Shows or hides a product's contracts; a product itself is never selected.
    ToggleProduct(ProductKey),
    /// A section header, or a loading or empty notice inside an expanded product.
    Inert,
}

/// One instrument-menu row, derived from accepted browser state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstrumentMenuEntry {
    pub(crate) provider: TerminalProvider,
    pub(crate) label: String,
    /// Supporting text after the label, such as a product name and contract month.
    pub(crate) description: Option<String>,
    /// Muted trailing text: the provider's asset class and, for a product, its exchange. A
    /// section header shows its provider's search status here.
    pub(crate) detail: Option<String>,
    pub(crate) checked: bool,
    /// The row sits under a product header.
    pub(crate) nested: bool,
    /// For a product header, whether its contracts are shown.
    pub(crate) expanded: Option<bool>,
    /// The header that starts one provider's section when several providers are searched.
    pub(crate) section: bool,
    pub(crate) action: InstrumentMenuAction,
}

/// What a lane's latest completed search lists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SymbolListingSummary {
    Products(usize),
    Contracts { contracts: usize, products: usize },
    Markets(usize),
}

/// How the lanes stand, for the menu footer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SymbolBrowserStatus {
    Searching,
    Failed,
    Listed(SymbolListingSummary),
    /// Every answering provider completed without a match.
    NoMatches,
    /// Several providers answered; each section carries its own summary.
    Sections,
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

/// Why a lane's search ended without results.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SymbolSearchFailure {
    /// The provider has no active catalog session; in a multi-provider scope its section is
    /// hidden until its catalog is ready.
    Unavailable,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingSymbolSearch {
    id: NonZeroUsize,
    query: String,
    lane: SymbolSearchLane,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ProductContracts {
    /// Waits for the lane's query search, which a contract search would supersede.
    Unrequested,
    Pending(NonZeroUsize),
    Listed {
        search_id: NonZeroUsize,
        contracts: Vec<ProviderInstrumentSummary>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProductExpansion {
    product: ProductKey,
    contracts: ProductContracts,
}

enum MenuSlot<'a> {
    Row(usize),
    Group((&'a str, &'a str), Vec<usize>),
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
    /// The latest product listing. A typed search replaces `results`, and its contract groups
    /// are still named after these products.
    products: Vec<ProviderInstrumentSummary>,
    expansion: Option<ProductExpansion>,
    /// Products of a typed search whose contracts the user hid.
    collapsed_products: Vec<ProductKey>,
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
            expansion: None,
            collapsed_products: Vec::new(),
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
        if products > 0 {
            return Some(SymbolListingSummary::Products(products));
        }
        let mut groups: Vec<(&str, &str)> = Vec::new();
        for result in &self.results {
            if let Some(code) = product_code(result) {
                let group = (code, result.exchange.as_str());
                if !groups.contains(&group) {
                    groups.push(group);
                }
            }
        }
        Some(if groups.is_empty() {
            SymbolListingSummary::Markets(self.results.len())
        } else {
            SymbolListingSummary::Contracts {
                contracts: self.results.len(),
                products: groups.len(),
            }
        })
    }

    /// A section that has nothing to say is not shown: an unavailable provider, or one never
    /// searched.
    fn visible_section(&self) -> bool {
        self.failure != Some(SymbolSearchFailure::Unavailable)
            && (self.pending.is_some()
                || self.completed_search_id.is_some()
                || self.failure.is_some())
    }

    fn section_status(&self) -> String {
        if self.query_pending() {
            return "Searching".to_string();
        }
        if self.failure == Some(SymbolSearchFailure::Failed) {
            return "Search failed · try again".to_string();
        }
        match self.summary() {
            Some(SymbolListingSummary::Products(count)) => {
                super::counted(count, "product", "products")
            }
            Some(SymbolListingSummary::Contracts { contracts, .. }) => {
                super::counted(contracts, "contract", "contracts")
            }
            Some(SymbolListingSummary::Markets(count)) => {
                super::counted(count, "market", "markets")
            }
            None => "No matches".to_string(),
        }
    }

    fn begin_contract_search(
        &mut self,
        request_id: NonZeroUsize,
        exchange: Option<ExchangeCode>,
    ) -> Option<SymbolSearchRequest> {
        if self.query_pending() {
            return None;
        }
        let code = self
            .expansion
            .as_ref()
            .filter(|expansion| expansion.contracts == ProductContracts::Unrequested)?
            .product
            .code
            .clone();
        self.pending = Some(PendingSymbolSearch {
            id: request_id,
            query: code.clone(),
            lane: SymbolSearchLane::ProductContracts,
        });
        if let Some(expansion) = self.expansion.as_mut() {
            expansion.contracts = ProductContracts::Pending(request_id);
        }
        Some(SymbolSearchRequest {
            provider: self.provider,
            request_id,
            query: code,
            lane: SymbolSearchLane::ProductContracts,
            exchange,
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
            InstrumentMenuSelection::ExpandedContract { index, .. } => {
                match &self.expansion.as_ref()?.contracts {
                    ProductContracts::Listed {
                        search_id,
                        contracts,
                    } => (contracts.get(index)?, *search_id),
                    ProductContracts::Unrequested | ProductContracts::Pending(_) => return None,
                }
            }
        };
        (instrument.kind == ProviderInstrumentKind::Instrument)
            .then_some((instrument, search_generation))
    }

    /// The lane's rows: listed products with the expanded one's contracts beneath it, a typed
    /// search's contracts grouped under their product, and instruments without a product flat.
    fn push_entries(
        &self,
        entries: &mut Vec<InstrumentMenuEntry>,
        selected: Option<&SymbolSelection>,
    ) {
        let mut slots: Vec<MenuSlot<'_>> = Vec::with_capacity(self.results.len());
        for (index, result) in self.results.iter().enumerate() {
            let group = (result.kind == ProviderInstrumentKind::Instrument)
                .then(|| product_code(result))
                .flatten()
                .map(|code| (code, result.exchange.as_str()));
            let Some(group) = group else {
                slots.push(MenuSlot::Row(index));
                continue;
            };
            match slots.iter_mut().find_map(|slot| match slot {
                MenuSlot::Group(key, members) if *key == group => Some(members),
                MenuSlot::Group(..) | MenuSlot::Row(_) => None,
            }) {
                Some(members) => members.push(index),
                None => slots.push(MenuSlot::Group(group, vec![index])),
            }
        }
        for slot in slots {
            match slot {
                MenuSlot::Row(index) => {
                    let result = &self.results[index];
                    if result.kind == ProviderInstrumentKind::Product {
                        self.push_listed_product(entries, result, selected);
                    } else {
                        entries.push(self.contract_entry(
                            result,
                            InstrumentMenuSelection::SearchResult {
                                provider: self.provider,
                                index,
                            },
                            None,
                            false,
                            selected,
                        ));
                    }
                }
                MenuSlot::Group((code, exchange), members) => {
                    let product = ProductKey {
                        provider: self.provider,
                        code: code.to_string(),
                        exchange: exchange.to_string(),
                    };
                    self.push_contract_group(entries, product, &members, selected);
                }
            }
        }
    }

    fn push_listed_product(
        &self,
        entries: &mut Vec<InstrumentMenuEntry>,
        listed: &ProviderInstrumentSummary,
        selected: Option<&SymbolSelection>,
    ) {
        let product = ProductKey::of(self.provider, listed);
        let expansion = self
            .expansion
            .as_ref()
            .filter(|expansion| product.as_ref() == Some(&expansion.product));
        entries.push(InstrumentMenuEntry {
            provider: self.provider,
            label: listed.display_symbol.clone(),
            description: listed.name.clone(),
            detail: product_detail(listed.asset_class.as_deref(), &listed.exchange),
            checked: false,
            nested: false,
            expanded: product.as_ref().map(|_| expansion.is_some()),
            section: false,
            action: product.map_or(
                InstrumentMenuAction::Inert,
                InstrumentMenuAction::ToggleProduct,
            ),
        });
        let Some(expansion) = expansion else {
            return;
        };
        match &expansion.contracts {
            ProductContracts::Unrequested | ProductContracts::Pending(_) => {
                entries.push(self.notice_entry("Loading contracts"));
            }
            ProductContracts::Listed { contracts, .. } if contracts.is_empty() => {
                entries.push(self.notice_entry("No contracts listed"));
            }
            ProductContracts::Listed { contracts, .. } => {
                entries.extend(contracts.iter().enumerate().map(|(index, contract)| {
                    self.contract_entry(
                        contract,
                        InstrumentMenuSelection::ExpandedContract {
                            provider: self.provider,
                            index,
                        },
                        listed.name.as_deref(),
                        true,
                        selected,
                    )
                }));
            }
        }
    }

    fn push_contract_group(
        &self,
        entries: &mut Vec<InstrumentMenuEntry>,
        product: ProductKey,
        members: &[usize],
        selected: Option<&SymbolSelection>,
    ) {
        let listed = self.products.iter().find(|listed| product.names(listed));
        let name = listed.and_then(|listed| listed.name.as_deref());
        let collapsed = self.collapsed_products.contains(&product);
        entries.push(InstrumentMenuEntry {
            provider: self.provider,
            label: name.map_or_else(|| product.code.clone(), str::to_string),
            description: None,
            detail: product_detail(
                listed.and_then(|listed| listed.asset_class.as_deref()),
                &product.exchange,
            ),
            checked: false,
            nested: false,
            expanded: Some(!collapsed),
            section: false,
            action: InstrumentMenuAction::ToggleProduct(product),
        });
        if collapsed {
            return;
        }
        entries.extend(members.iter().map(|&index| {
            self.contract_entry(
                &self.results[index],
                InstrumentMenuSelection::SearchResult {
                    provider: self.provider,
                    index,
                },
                name,
                true,
                selected,
            )
        }));
    }

    fn contract_entry(
        &self,
        instrument: &ProviderInstrumentSummary,
        selection: InstrumentMenuSelection,
        product_name: Option<&str>,
        nested: bool,
        selected: Option<&SymbolSelection>,
    ) -> InstrumentMenuEntry {
        InstrumentMenuEntry {
            provider: self.provider,
            label: instrument.display_symbol.clone(),
            description: contract_description(product_name, instrument),
            detail: instrument.asset_class.clone(),
            checked: selected.is_some_and(|selected| {
                selected.provider == self.provider
                    && selected.instrument.symbol == instrument.symbol
                    && selected.instrument.exchange == instrument.exchange
            }),
            nested,
            expanded: None,
            section: false,
            action: InstrumentMenuAction::Select(selection),
        }
    }

    fn notice_entry(&self, label: &str) -> InstrumentMenuEntry {
        InstrumentMenuEntry {
            provider: self.provider,
            label: label.to_string(),
            description: None,
            detail: None,
            checked: false,
            nested: true,
            expanded: None,
            section: false,
            action: InstrumentMenuAction::Inert,
        }
    }

    fn section_entry(&self) -> InstrumentMenuEntry {
        InstrumentMenuEntry {
            provider: self.provider,
            label: terminal_provider_display(self.provider).to_string(),
            description: None,
            detail: Some(self.section_status()),
            checked: false,
            nested: false,
            expanded: None,
            section: true,
            action: InstrumentMenuAction::Inert,
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
                lane.expansion = None;
                lane.collapsed_products.clear();
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
        results: Vec<ProviderInstrumentSummary>,
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
                lane.collapsed_products.clear();
                lane.results = results;
            }
            SymbolSearchLane::ProductContracts => {
                // A product collapsed or replaced while its search ran drops the reply.
                if let Some(expansion) = lane.expansion.as_mut().filter(|expansion| {
                    expansion.contracts == ProductContracts::Pending(request_id)
                }) {
                    let exchange = expansion.product.exchange.as_str();
                    let contracts = results
                        .into_iter()
                        .filter(|result| {
                            result.kind == ProviderInstrumentKind::Instrument
                                && result.exchange == exchange
                        })
                        .collect();
                    expansion.contracts = ProductContracts::Listed {
                        search_id: request_id,
                        contracts,
                    };
                }
            }
        }
        true
    }

    /// Contract searches of products expanded while their lane's query search ran.
    pub(crate) fn begin_contract_searches(&mut self) -> Vec<SymbolSearchRequest> {
        let exchange = self.scope.exchange();
        let mut requests = Vec::new();
        for index in 0..self.lanes.len() {
            let waiting = !self.lanes[index].query_pending()
                && self.lanes[index]
                    .expansion
                    .as_ref()
                    .is_some_and(|expansion| expansion.contracts == ProductContracts::Unrequested);
            if waiting {
                let request_id = self.next_request_id();
                requests.extend(self.lanes[index].begin_contract_search(request_id, exchange));
            }
        }
        requests
    }

    /// Shows or hides a product's contracts. Expanding a listed product collapses any other and
    /// returns its contract search unless its lane's query search is still running; that
    /// search starts later through `begin_contract_searches`. A typed search's groups toggle
    /// in place.
    pub(crate) fn toggle_product(&mut self, product: &ProductKey) -> Option<SymbolSearchRequest> {
        let exchange = self.scope.exchange();
        let lane_index = self
            .lanes
            .iter()
            .position(|lane| lane.provider == product.provider)?;
        let listed = self.lanes[lane_index]
            .results
            .iter()
            .any(|result| result.kind == ProviderInstrumentKind::Product && product.names(result));
        if !listed {
            let lane = &mut self.lanes[lane_index];
            if let Some(index) = lane
                .collapsed_products
                .iter()
                .position(|collapsed| collapsed == product)
            {
                lane.collapsed_products.remove(index);
            } else if lane.results.iter().any(|result| product.names(result)) {
                lane.collapsed_products.push(product.clone());
            }
            return None;
        }
        if self.lanes[lane_index]
            .expansion
            .as_ref()
            .is_some_and(|expansion| &expansion.product == product)
        {
            self.lanes[lane_index].expansion = None;
            return None;
        }
        for lane in &mut self.lanes {
            lane.expansion = None;
        }
        self.lanes[lane_index].expansion = Some(ProductExpansion {
            product: product.clone(),
            contracts: ProductContracts::Unrequested,
        });
        if self.lanes[lane_index].query_pending() {
            return None;
        }
        let request_id = self.next_request_id();
        self.lanes[lane_index].begin_contract_search(request_id, exchange)
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
            lane.expansion = None;
            lane.collapsed_products.clear();
            return true;
        }
        let expanded = lane.expansion.as_ref().is_some_and(|expansion| {
            matches!(
                expansion.contracts,
                ProductContracts::Listed { search_id, .. } if search_id == search_generation
            )
        });
        if expanded {
            lane.expansion = None;
        }
        expanded
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

    /// Ends a lane's pending search. A rejected contract search collapses its product so
    /// expanding it again retries; a rejected query search records why.
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
                if lane.expansion.as_ref().is_some_and(|expansion| {
                    expansion.contracts == ProductContracts::Pending(generation)
                }) {
                    lane.expansion = None;
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
    /// later one's even when the later one answers first.
    pub(crate) fn exact_symbol(&self, symbol: &str) -> ExactSymbolMatch {
        for lane in &self.lanes {
            if lane.query_pending() {
                return ExactSymbolMatch::Pending;
            }
            if let Some(index) = lane.results.iter().position(|instrument| {
                instrument.kind == ProviderInstrumentKind::Instrument
                    && (instrument.symbol.eq_ignore_ascii_case(symbol)
                        || instrument.display_symbol.eq_ignore_ascii_case(symbol))
            }) {
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
        if let [lane] = self.lanes.as_slice() {
            if let Some(summary) = lane.summary() {
                return SymbolBrowserStatus::Listed(summary);
            }
            return match lane.failure {
                Some(_) => SymbolBrowserStatus::Failed,
                None if lane.completed_search_id.is_some() => SymbolBrowserStatus::NoMatches,
                None => SymbolBrowserStatus::Idle,
            };
        }
        if self.has_results() {
            SymbolBrowserStatus::Sections
        } else if self
            .lanes
            .iter()
            .any(|lane| lane.completed_search_id.is_some())
        {
            SymbolBrowserStatus::NoMatches
        } else if self
            .lanes
            .iter()
            .any(|lane| lane.failure == Some(SymbolSearchFailure::Failed))
        {
            SymbolBrowserStatus::Failed
        } else {
            SymbolBrowserStatus::Idle
        }
    }

    /// The menu rows. With several providers in scope each answering provider's rows follow a
    /// section header carrying its status; an unavailable provider has no section.
    pub(crate) fn menu_entries(&self) -> Vec<InstrumentMenuEntry> {
        let sections = self.lanes.len() > 1;
        let mut entries = Vec::new();
        for lane in &self.lanes {
            if sections {
                if !lane.visible_section() {
                    continue;
                }
                entries.push(lane.section_entry());
            }
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

/// A contract under a listed product reads as that product's name and its month. Without a
/// listed name the provider's own contract name is shown as is, since it may already carry
/// the month.
fn contract_description(
    product_name: Option<&str>,
    contract: &ProviderInstrumentSummary,
) -> Option<String> {
    let month = contract.expiration_date.as_deref().and_then(contract_month);
    match (product_name, month) {
        (Some(name), Some(month)) => Some(format!("{name} ({month})")),
        (Some(name), None) => Some(name.to_string()),
        (None, month) => contract.name.clone().or(month),
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
