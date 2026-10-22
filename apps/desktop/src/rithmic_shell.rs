use aeris_contracts::{ProviderInstrumentKind, ProviderInstrumentSummary};
use aeris_observability::{FeedConnectionState, FeedIdentity};
use std::num::NonZeroUsize;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RithmicShellState {
    pub(crate) identity: FeedIdentity,
    connection: FeedConnectionState,
}

impl RithmicShellState {
    pub(crate) fn local() -> Result<Self, String> {
        let identity = FeedIdentity::try_new("rithmic", "RITHMIC_TEST", "Test")
            .map_err(|error| error.to_string())?;
        Ok(Self {
            identity,
            connection: FeedConnectionState::Discovering,
        })
    }

    pub(crate) const fn connection(&self) -> FeedConnectionState {
        self.connection
    }

    /// The server and system come from the saved login, which the runtime alone reads, so the
    /// pre-login shell names only the provider.
    pub(crate) fn profile_label(&self) -> String {
        match self.identity.provider() {
            "rithmic" => "Rithmic".to_string(),
            provider => provider.to_string(),
        }
    }
}

pub(crate) const MAXIMUM_SYMBOL_QUERY_BYTES: usize = 64;
pub(crate) const MAXIMUM_RITHMIC_SYMBOL_RESULTS: usize = 256;

/// Which part of the browser a search fills.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SymbolSearchLane {
    /// The typed query, or the provider's listing for an empty one.
    Query,
    /// The contracts of the expanded product.
    ProductContracts,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RithmicSymbolSearchRequest {
    pub(crate) request_id: NonZeroUsize,
    pub(crate) query: String,
    pub(crate) lane: SymbolSearchLane,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RithmicSymbolSelection {
    pub(crate) generation: NonZeroUsize,
    pub(crate) search_generation: NonZeroUsize,
    pub(crate) instrument: ProviderInstrumentSummary,
}

/// A product on one exchange; searching its code lists its contracts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProductKey {
    pub(crate) code: String,
    pub(crate) exchange: String,
}

impl ProductKey {
    fn of(instrument: &ProviderInstrumentSummary) -> Option<Self> {
        product_code(instrument).map(|code| Self {
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
    /// An instrument of the latest completed search.
    SearchResult(usize),
    /// A contract of the expanded product.
    ExpandedContract(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InstrumentMenuAction {
    Select(InstrumentMenuSelection),
    /// Shows or hides a product's contracts; a product itself is never selected.
    ToggleProduct(ProductKey),
    /// A loading or empty notice inside an expanded product.
    Inert,
}

/// One instrument-menu row, derived from accepted browser state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstrumentMenuEntry {
    pub(crate) label: String,
    /// Supporting text after the label, such as a product name and contract month.
    pub(crate) description: Option<String>,
    /// Muted trailing text: the provider's asset class and, for a product, its exchange.
    pub(crate) detail: Option<String>,
    pub(crate) checked: bool,
    /// The row sits under a product header.
    pub(crate) nested: bool,
    /// For a product header, whether its contracts are shown.
    pub(crate) expanded: Option<bool>,
    pub(crate) action: InstrumentMenuAction,
}

/// What the latest completed search lists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SymbolListingSummary {
    Products(usize),
    Contracts { contracts: usize, products: usize },
    Markets(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingSymbolSearch {
    id: NonZeroUsize,
    query: String,
    lane: SymbolSearchLane,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ProductContracts {
    /// Waits for the one search lane to become free.
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RithmicSymbolBrowser {
    maximum_results: usize,
    next_search_id: usize,
    pending_search: Option<PendingSymbolSearch>,
    retained_search_query: Option<String>,
    completed_search_id: Option<NonZeroUsize>,
    completed_query: Option<String>,
    results: Vec<ProviderInstrumentSummary>,
    /// The latest product listing. A typed search replaces `results`, and its contract groups
    /// are still named after these products.
    products: Vec<ProviderInstrumentSummary>,
    /// The one listed product whose contracts are shown.
    expansion: Option<ProductExpansion>,
    /// Products of a typed search whose contracts the user hid.
    collapsed_products: Vec<ProductKey>,
    selection_generation: usize,
    pending_selection: Option<RithmicSymbolSelection>,
    selected: Option<RithmicSymbolSelection>,
}

impl Default for RithmicSymbolBrowser {
    fn default() -> Self {
        Self {
            maximum_results: MAXIMUM_RITHMIC_SYMBOL_RESULTS,
            next_search_id: 0,
            pending_search: None,
            retained_search_query: None,
            completed_search_id: None,
            completed_query: None,
            results: Vec::new(),
            products: Vec::new(),
            expansion: None,
            collapsed_products: Vec::new(),
            selection_generation: 0,
            pending_selection: None,
            selected: None,
        }
    }
}

impl RithmicSymbolBrowser {
    #[cfg(test)]
    pub(crate) fn rithmic_catalog_awaiting_search(request_id: NonZeroUsize, query: &str) -> Self {
        Self {
            next_search_id: request_id.get(),
            pending_search: Some(PendingSymbolSearch {
                id: request_id,
                query: query.to_string(),
                lane: SymbolSearchLane::Query,
            }),
            ..Self::default()
        }
    }

    /// Forgets every search and selection; request generations keep increasing.
    pub(crate) fn change_catalog(&mut self) {
        *self = Self {
            next_search_id: self.next_search_id,
            selection_generation: self.selection_generation,
            ..Self::default()
        };
    }

    pub(crate) fn begin_search(
        &mut self,
        query: &str,
    ) -> Result<RithmicSymbolSearchRequest, &'static str> {
        let query = query.trim();
        Self::validate_query(query)?;
        self.results.clear();
        self.expansion = None;
        self.collapsed_products.clear();
        Ok(self.dispatch(query.to_string(), SymbolSearchLane::Query))
    }

    fn dispatch(&mut self, query: String, lane: SymbolSearchLane) -> RithmicSymbolSearchRequest {
        self.next_search_id = self.next_search_id.saturating_add(1).max(1);
        let request_id = NonZeroUsize::new(self.next_search_id).unwrap_or(NonZeroUsize::MIN);
        self.pending_search = Some(PendingSymbolSearch {
            id: request_id,
            query: query.clone(),
            lane,
        });
        RithmicSymbolSearchRequest {
            request_id,
            query,
            lane,
        }
    }

    pub(crate) fn apply_results(
        &mut self,
        request_id: NonZeroUsize,
        results: Vec<ProviderInstrumentSummary>,
    ) -> bool {
        let initial = self.next_search_id == 0 && self.pending_search.is_none();
        let pending = self
            .pending_search
            .as_ref()
            .is_some_and(|search| search.id == request_id);
        if (!pending && !initial) || results.len() > self.maximum_results {
            return false;
        }
        let (lane, query) = self
            .pending_search
            .take()
            .map_or((SymbolSearchLane::Query, String::new()), |search| {
                (search.lane, search.query)
            });
        self.next_search_id = self.next_search_id.max(request_id.get());
        match lane {
            SymbolSearchLane::Query => {
                if results
                    .iter()
                    .any(|result| result.kind == ProviderInstrumentKind::Product)
                {
                    self.products = results
                        .iter()
                        .filter(|result| result.kind == ProviderInstrumentKind::Product)
                        .cloned()
                        .collect();
                }
                self.completed_search_id = Some(request_id);
                self.completed_query = Some(query);
                self.collapsed_products.clear();
                self.results = results;
            }
            SymbolSearchLane::ProductContracts => {
                // A product collapsed or replaced while its search ran drops the reply.
                if let Some(expansion) = self.expansion.as_mut().filter(|expansion| {
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

    pub(crate) fn retain_latest_search(&mut self, query: &str) -> Result<bool, &'static str> {
        let query = query.trim();
        if let Err(error) = Self::validate_query(query) {
            self.retained_search_query = None;
            return Err(error);
        }
        let already_dispatched = self
            .pending_search
            .as_ref()
            .is_some_and(|search| search.lane == SymbolSearchLane::Query && search.query == query);
        self.retained_search_query = (!already_dispatched).then(|| query.to_string());
        Ok(already_dispatched)
    }

    /// The next search once the lane is free: the latest typed query first, then the contracts
    /// of a product expanded while another search ran.
    pub(crate) fn begin_retained_search(&mut self) -> Option<RithmicSymbolSearchRequest> {
        if self.pending_search.is_some() {
            return None;
        }
        if let Some(query) = self.retained_search_query.take() {
            return self.begin_search(&query).ok();
        }
        self.begin_contract_search()
    }

    fn begin_contract_search(&mut self) -> Option<RithmicSymbolSearchRequest> {
        let code = self
            .expansion
            .as_ref()
            .filter(|expansion| expansion.contracts == ProductContracts::Unrequested)?
            .product
            .code
            .clone();
        let request = self.dispatch(code, SymbolSearchLane::ProductContracts);
        if let Some(expansion) = self.expansion.as_mut() {
            expansion.contracts = ProductContracts::Pending(request.request_id);
        }
        Some(request)
    }

    /// Shows or hides a product's contracts. Expanding a listed product collapses any other and
    /// returns its contract search when the search lane is free; a busy lane starts it later
    /// through `begin_retained_search`. The groups of a typed search toggle in place.
    pub(crate) fn toggle_product(
        &mut self,
        product: &ProductKey,
    ) -> Option<RithmicSymbolSearchRequest> {
        let listed = self
            .results
            .iter()
            .any(|result| result.kind == ProviderInstrumentKind::Product && product.names(result));
        if !listed {
            if let Some(index) = self
                .collapsed_products
                .iter()
                .position(|collapsed| collapsed == product)
            {
                self.collapsed_products.remove(index);
            } else if self.results.iter().any(|result| product.names(result)) {
                self.collapsed_products.push(product.clone());
            }
            return None;
        }
        if self
            .expansion
            .as_ref()
            .is_some_and(|expansion| &expansion.product == product)
        {
            self.expansion = None;
            return None;
        }
        self.expansion = Some(ProductExpansion {
            product: product.clone(),
            contracts: ProductContracts::Unrequested,
        });
        if self.pending_search.is_some() || self.retained_search_query.is_some() {
            return None;
        }
        self.begin_contract_search()
    }

    pub(crate) fn select(
        &mut self,
        selection: InstrumentMenuSelection,
    ) -> Option<RithmicSymbolSelection> {
        let (instrument, search_generation) = self.selectable(selection)?;
        let instrument = instrument.clone();
        self.selection_generation = self.selection_generation.saturating_add(1).max(1);
        let selection = RithmicSymbolSelection {
            generation: NonZeroUsize::new(self.selection_generation).unwrap_or(NonZeroUsize::MIN),
            search_generation,
            instrument,
        };
        self.pending_selection = Some(selection.clone());
        Some(selection)
    }

    fn selectable(
        &self,
        selection: InstrumentMenuSelection,
    ) -> Option<(&ProviderInstrumentSummary, NonZeroUsize)> {
        let (instrument, search_generation) = match selection {
            InstrumentMenuSelection::SearchResult(index) => {
                (self.results.get(index)?, self.completed_search_id?)
            }
            InstrumentMenuSelection::ExpandedContract(index) => {
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

    pub(crate) fn consume_completed_search(&mut self, search_generation: NonZeroUsize) -> bool {
        if self.completed_search_id == Some(search_generation) {
            self.completed_search_id = None;
            self.completed_query = None;
            self.results.clear();
            self.expansion = None;
            self.collapsed_products.clear();
            return true;
        }
        let expanded = self.expansion.as_ref().is_some_and(|expansion| {
            matches!(
                expansion.contracts,
                ProductContracts::Listed { search_id, .. } if search_id == search_generation
            )
        });
        if expanded {
            self.expansion = None;
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
    ) -> Option<RithmicSymbolSelection> {
        self.pending_selection
            .take_if(|selection| selection.generation == generation)
    }

    /// Ends a pending search; a rejected contract search collapses its product so expanding it
    /// again retries.
    pub(crate) fn reject_search(&mut self, generation: NonZeroUsize) -> bool {
        if self
            .pending_search
            .take_if(|search| search.id == generation)
            .is_none()
        {
            return false;
        }
        if self
            .expansion
            .as_ref()
            .is_some_and(|expansion| expansion.contracts == ProductContracts::Pending(generation))
        {
            self.expansion = None;
        }
        true
    }

    pub(crate) fn reject_selection(&mut self, generation: NonZeroUsize) -> bool {
        self.pending_selection
            .take_if(|selection| selection.generation == generation)
            .is_some()
    }

    #[cfg(test)]
    pub(crate) fn pending_search_id(&self) -> Option<NonZeroUsize> {
        self.pending_search.as_ref().map(|search| search.id)
    }

    pub(crate) fn results(&self) -> &[ProviderInstrumentSummary] {
        &self.results
    }

    /// Whether the shown results answer `query`, so reopening the menu needs no new search.
    pub(crate) fn lists_query(&self, query: &str) -> bool {
        !self.results.is_empty() && self.completed_query.as_deref() == Some(query.trim())
    }

    pub(crate) const fn maximum_results(&self) -> usize {
        self.maximum_results
    }

    pub(crate) const fn has_retained_search(&self) -> bool {
        self.retained_search_query.is_some()
    }

    pub(crate) const fn search_pending(&self) -> bool {
        self.pending_search.is_some()
    }

    pub(crate) const fn has_completed_search(&self) -> bool {
        self.completed_search_id.is_some()
    }

    pub(crate) fn selected(&self) -> Option<&RithmicSymbolSelection> {
        self.selected.as_ref()
    }

    pub(crate) fn listing_summary(&self) -> Option<SymbolListingSummary> {
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

    /// The menu rows: listed products with the expanded one's contracts beneath it, a typed
    /// search's contracts grouped under their product, and instruments without a product flat.
    pub(crate) fn menu_entries(&self) -> Vec<InstrumentMenuEntry> {
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
        let mut entries = Vec::with_capacity(self.results.len());
        for slot in slots {
            match slot {
                MenuSlot::Row(index) => {
                    let result = &self.results[index];
                    if result.kind == ProviderInstrumentKind::Product {
                        self.push_listed_product(&mut entries, result);
                    } else {
                        entries.push(self.contract_entry(
                            result,
                            InstrumentMenuSelection::SearchResult(index),
                            None,
                            false,
                        ));
                    }
                }
                MenuSlot::Group((code, exchange), members) => {
                    let product = ProductKey {
                        code: code.to_string(),
                        exchange: exchange.to_string(),
                    };
                    self.push_contract_group(&mut entries, product, &members);
                }
            }
        }
        entries
    }

    fn push_listed_product(
        &self,
        entries: &mut Vec<InstrumentMenuEntry>,
        listed: &ProviderInstrumentSummary,
    ) {
        let product = ProductKey::of(listed);
        let expansion = self
            .expansion
            .as_ref()
            .filter(|expansion| product.as_ref() == Some(&expansion.product));
        entries.push(InstrumentMenuEntry {
            label: listed.display_symbol.clone(),
            description: listed.name.clone(),
            detail: product_detail(listed.asset_class.as_deref(), &listed.exchange),
            checked: false,
            nested: false,
            expanded: product.as_ref().map(|_| expansion.is_some()),
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
                entries.push(notice_entry("Loading contracts"));
            }
            ProductContracts::Listed { contracts, .. } if contracts.is_empty() => {
                entries.push(notice_entry("No contracts listed"));
            }
            ProductContracts::Listed { contracts, .. } => {
                entries.extend(contracts.iter().enumerate().map(|(index, contract)| {
                    self.contract_entry(
                        contract,
                        InstrumentMenuSelection::ExpandedContract(index),
                        listed.name.as_deref(),
                        true,
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
    ) {
        let listed = self.products.iter().find(|listed| product.names(listed));
        let name = listed.and_then(|listed| listed.name.as_deref());
        let collapsed = self.collapsed_products.contains(&product);
        entries.push(InstrumentMenuEntry {
            label: name.map_or_else(|| product.code.clone(), str::to_string),
            description: None,
            detail: product_detail(
                listed.and_then(|listed| listed.asset_class.as_deref()),
                &product.exchange,
            ),
            checked: false,
            nested: false,
            expanded: Some(!collapsed),
            action: InstrumentMenuAction::ToggleProduct(product),
        });
        if collapsed {
            return;
        }
        entries.extend(members.iter().map(|&index| {
            self.contract_entry(
                &self.results[index],
                InstrumentMenuSelection::SearchResult(index),
                name,
                true,
            )
        }));
    }

    fn contract_entry(
        &self,
        instrument: &ProviderInstrumentSummary,
        selection: InstrumentMenuSelection,
        product_name: Option<&str>,
        nested: bool,
    ) -> InstrumentMenuEntry {
        InstrumentMenuEntry {
            label: instrument.display_symbol.clone(),
            description: contract_description(product_name, instrument),
            detail: instrument.asset_class.clone(),
            checked: self.selected.as_ref().is_some_and(|selected| {
                selected.instrument.symbol == instrument.symbol
                    && selected.instrument.exchange == instrument.exchange
            }),
            nested,
            expanded: None,
            action: InstrumentMenuAction::Select(selection),
        }
    }

    #[cfg(test)]
    pub(crate) fn invalidate_session(&mut self) {
        self.change_catalog();
    }

    /// An empty query asks the provider catalog for its default listing; the
    /// runtime catalog owns whether a provider accepts it.
    fn validate_query(query: &str) -> Result<(), &'static str> {
        if query.len() > MAXIMUM_SYMBOL_QUERY_BYTES || query.chars().any(char::is_control) {
            return Err("symbol search must be at most 64 printable bytes");
        }
        Ok(())
    }
}

fn notice_entry(label: &str) -> InstrumentMenuEntry {
    InstrumentMenuEntry {
        label: label.to_string(),
        description: None,
        detail: None,
        checked: false,
        nested: true,
        expanded: None,
        action: InstrumentMenuAction::Inert,
    }
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
pub(crate) const fn connection_label(connection: FeedConnectionState) -> &'static str {
    match connection {
        FeedConnectionState::Disconnected => "Not connected",
        FeedConnectionState::Discovering => "Discovering Rithmic systems",
        FeedConnectionState::Authenticating => "Authenticating",
        FeedConnectionState::Streaming => "Live market data",
        FeedConnectionState::Recovering => "Reconnecting",
        FeedConnectionState::Stopped => "Stopped",
    }
}

#[cfg(test)]
mod tests {
    use super::InstrumentMenuSelection::{ExpandedContract, SearchResult};
    use super::*;

    fn result(symbol: &str) -> ProviderInstrumentSummary {
        ProviderInstrumentSummary {
            kind: ProviderInstrumentKind::Instrument,
            symbol: symbol.to_string(),
            display_symbol: symbol.to_string(),
            exchange: "CME".to_string(),
            name: Some(format!("{symbol} future")),
            product_code: Some("ES".to_string()),
            instrument_type: Some("FUTURE".to_string()),
            expiration_date: None,
            asset_class: None,
        }
    }

    #[test]
    fn local_shell_is_available_before_login_or_history() {
        let shell = RithmicShellState::local().expect("fixed Rithmic profile validates");
        assert_eq!(shell.identity.provider(), "rithmic");
        assert_eq!(shell.identity.system(), "RITHMIC_TEST");
        assert_eq!(shell.identity.environment(), "Test");
        assert_eq!(shell.connection(), FeedConnectionState::Discovering);
        assert_eq!(
            connection_label(shell.connection()),
            "Discovering Rithmic systems"
        );
        assert_eq!(shell.profile_label(), "Rithmic");
    }

    #[test]
    fn symbol_browser_fences_stale_searches_and_selections() {
        let mut browser = RithmicSymbolBrowser::default();
        let first = browser.begin_search(" ES ").expect("query validates");
        assert_eq!(first.query, "ES");
        let second = browser.begin_search("NQ").expect("query validates");
        assert!(!browser.apply_results(first.request_id, vec![result("ESM7")]));
        assert_eq!(browser.results(), [] as [ProviderInstrumentSummary; 0]);
        assert!(browser.apply_results(second.request_id, vec![result("NQM7")]));

        let first_selection = browser
            .select(SearchResult(0))
            .expect("result can be selected");
        let second_selection = browser
            .select(SearchResult(0))
            .expect("replacement can be selected");
        assert!(second_selection.generation > first_selection.generation);
        assert!(browser.selected().is_none());
        assert!(!browser.confirm_selection(first_selection.generation));
        assert!(browser.confirm_selection(second_selection.generation));
        assert_eq!(browser.selected(), Some(&second_selection));
    }

    #[test]
    fn resolving_a_secondary_selection_does_not_replace_the_chart_selection() {
        let mut browser = RithmicSymbolBrowser::default();
        let search = browser.begin_search("ES").expect("query validates");
        assert!(browser.apply_results(search.request_id, vec![result("ESM7")]));
        let chart = browser.select(SearchResult(0)).expect("chart selection");
        assert!(browser.confirm_selection(chart.generation));

        let watchlist = browser
            .select(SearchResult(0))
            .expect("watchlist selection");
        assert_eq!(
            browser.resolve_selection(watchlist.generation),
            Some(watchlist)
        );
        assert_eq!(browser.selected(), Some(&chart));
    }

    #[test]
    fn completed_search_can_resolve_multiple_watchlist_symbols() {
        let mut browser = RithmicSymbolBrowser::default();
        let search = browser.begin_search("E").expect("query validates");
        assert!(browser.apply_results(search.request_id, vec![result("ESM7"), result("EPM7")]));

        let first = browser.select(SearchResult(0)).expect("first result");
        assert_eq!(browser.resolve_selection(first.generation), Some(first));
        let second = browser
            .select(SearchResult(1))
            .expect("second result remains available");
        assert_eq!(browser.resolve_selection(second.generation), Some(second));
        assert_eq!(browser.results().len(), 2);
    }

    #[test]
    fn symbol_browser_bounds_queries_results_and_indexes() {
        let mut browser = RithmicSymbolBrowser::default();
        assert_eq!(browser.maximum_results(), MAXIMUM_RITHMIC_SYMBOL_RESULTS);
        assert!(
            browser.begin_search("").is_ok(),
            "a fresh browser requests the empty-query default listing"
        );
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
        assert!(browser.select(SearchResult(0)).is_none());
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
        let selection = browser
            .select(SearchResult(0))
            .expect("result can be selected");
        assert!(browser.confirm_selection(selection.generation));

        browser.invalidate_session();

        assert_eq!(browser.results(), [] as [ProviderInstrumentSummary; 0]);
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
        let first_selection = browser.select(SearchResult(0)).expect("first selection");
        let second_selection = browser.select(SearchResult(0)).expect("second selection");
        let second_search = browser.begin_search("NQ").expect("second search validates");
        assert_eq!(second_search.request_id, second_selection.generation);

        assert!(browser.reject_search(second_search.request_id));
        assert!(browser.confirm_selection(second_selection.generation));

        let third_search = browser.begin_search("YM").expect("third search validates");
        assert!(browser.apply_results(third_search.request_id, vec![result("YMM7")]));
        let third_selection = browser.select(SearchResult(0)).unwrap_or(first_selection);
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
        let current = browser.select(SearchResult(0)).expect("current selection");
        assert!(browser.confirm_selection(current.generation));
        let replacement = browser
            .select(SearchResult(1))
            .expect("replacement selection");

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
        assert_eq!(browser.results(), [] as [ProviderInstrumentSummary; 0]);

        assert!(browser.apply_results(active.request_id, vec![result("ESU6")]));
        assert!(browser.begin_retained_search().is_none());
        assert_eq!(browser.results()[0].symbol, "ESU6");
    }

    #[test]
    fn invalid_latest_rithmic_query_cancels_an_older_retained_prefix() {
        let mut browser = RithmicSymbolBrowser::default();
        let active = browser.begin_search("E").expect("prefix validates");
        assert!(!browser.retain_latest_search("ES").expect("query validates"));
        assert!(browser.retain_latest_search("ES\u{7}").is_err());

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
        let selection = browser
            .select(SearchResult(0))
            .expect("catalog result is selectable");
        assert!(browser.confirm_selection(selection.generation));

        assert!(browser.consume_completed_search(selection.search_generation));
        assert_eq!(browser.results(), [] as [ProviderInstrumentSummary; 0]);
        assert!(browser.select(SearchResult(0)).is_none());
        assert!(!browser.consume_completed_search(selection.search_generation));
        assert_eq!(browser.selected(), Some(&selection));
    }

    #[test]
    fn a_completed_search_rejects_duplicate_and_stale_results() {
        let mut browser = RithmicSymbolBrowser::default();
        let search = browser.begin_search("ES").unwrap();
        assert!(browser.apply_results(search.request_id, vec![result("ESZ6"), result("ESH7")]));
        assert_eq!(browser.results().len(), 2);
        assert!(!browser.apply_results(search.request_id, vec![result("DUPLICATE")]));
        let next = browser.begin_search("NQ").unwrap();
        assert!(!browser.apply_results(search.request_id, vec![result("STALE")]));
        assert!(browser.apply_results(next.request_id, vec![result("NQZ6")]));
    }

    fn product(code: &str, name: Option<&str>) -> ProviderInstrumentSummary {
        ProviderInstrumentSummary {
            kind: ProviderInstrumentKind::Product,
            symbol: code.to_string(),
            display_symbol: code.to_string(),
            exchange: "CME".to_string(),
            name: name.map(str::to_string),
            product_code: Some(code.to_string()),
            asset_class: Some("futures".to_string()),
            ..ProviderInstrumentSummary::default()
        }
    }

    fn contract(symbol: &str, code: &str, expiration: &str) -> ProviderInstrumentSummary {
        ProviderInstrumentSummary {
            kind: ProviderInstrumentKind::Instrument,
            symbol: symbol.to_string(),
            display_symbol: symbol.to_string(),
            exchange: "CME".to_string(),
            name: Some(format!("{code} contract")),
            product_code: Some(code.to_string()),
            instrument_type: Some("Future".to_string()),
            expiration_date: Some(expiration.to_string()),
            asset_class: None,
        }
    }

    fn key(code: &str) -> ProductKey {
        ProductKey {
            code: code.to_string(),
            exchange: "CME".to_string(),
        }
    }

    fn labels(browser: &RithmicSymbolBrowser) -> Vec<(String, bool)> {
        browser
            .menu_entries()
            .into_iter()
            .map(|entry| (entry.label, entry.nested))
            .collect()
    }

    fn listed_browser() -> RithmicSymbolBrowser {
        let mut browser = RithmicSymbolBrowser::default();
        let listing = browser
            .begin_search("")
            .expect("empty query lists products");
        assert_eq!(listing.lane, SymbolSearchLane::Query);
        assert!(browser.apply_results(
            listing.request_id,
            vec![
                product("ES", Some("E-mini S&P 500")),
                product("NQ", Some("E-mini Nasdaq-100")),
            ],
        ));
        browser
    }

    #[test]
    fn product_listing_shows_collapsed_unselectable_products() {
        let mut browser = listed_browser();
        let entries = browser.menu_entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].label, "NQ");
        assert_eq!(entries[1].description.as_deref(), Some("E-mini Nasdaq-100"));
        assert_eq!(entries[1].detail.as_deref(), Some("futures · CME"));
        assert_eq!(entries[1].expanded, Some(false));
        assert_eq!(
            entries[1].action,
            InstrumentMenuAction::ToggleProduct(key("NQ"))
        );
        assert!(
            browser.select(SearchResult(1)).is_none(),
            "a product is never sent as a selection"
        );
        assert_eq!(
            browser.listing_summary(),
            Some(SymbolListingSummary::Products(2))
        );
        assert!(browser.lists_query(""));
        assert!(!browser.lists_query("NQ"));
    }

    #[test]
    fn expanding_a_product_searches_and_nests_its_contracts() {
        let mut browser = listed_browser();
        let search = browser
            .toggle_product(&key("NQ"))
            .expect("a free lane searches the product's contracts");
        assert_eq!(search.query, "NQ");
        assert_eq!(search.lane, SymbolSearchLane::ProductContracts);
        assert_eq!(
            labels(&browser),
            [
                ("ES".to_string(), false),
                ("NQ".to_string(), false),
                ("Loading contracts".to_string(), true),
            ]
        );
        assert_eq!(
            browser.menu_entries()[2].action,
            InstrumentMenuAction::Inert
        );

        assert!(browser.apply_results(
            search.request_id,
            vec![
                contract("NQZ6", "NQ", "20261218"),
                contract("NQH7", "NQ", "20270319"),
            ],
        ));
        let entries = browser.menu_entries();
        assert_eq!(
            entries.len(),
            4,
            "the products stay listed around the contracts"
        );
        assert_eq!(entries[1].expanded, Some(true));
        assert_eq!(entries[2].label, "NQZ6");
        assert!(entries[2].nested);
        assert_eq!(
            entries[2].description.as_deref(),
            Some("E-mini Nasdaq-100 (Dec 2026)")
        );
        assert_eq!(
            entries[3].action,
            InstrumentMenuAction::Select(ExpandedContract(1))
        );
        assert_eq!(
            browser.listing_summary(),
            Some(SymbolListingSummary::Products(2))
        );

        let selection = browser
            .select(ExpandedContract(0))
            .expect("an expanded contract is selectable");
        assert_eq!(selection.instrument.symbol, "NQZ6");
        assert_eq!(selection.search_generation, search.request_id);

        assert!(browser.toggle_product(&key("NQ")).is_none());
        assert_eq!(labels(&browser).len(), 2, "collapsing hides the contracts");
        assert!(browser.select(ExpandedContract(0)).is_none());
    }

    #[test]
    fn expanding_during_a_search_starts_after_it_and_drops_a_replaced_reply() {
        let mut browser = listed_browser();
        let nq = browser.toggle_product(&key("NQ")).expect("NQ contracts");
        assert!(
            browser.toggle_product(&key("ES")).is_none(),
            "the lane is busy with NQ"
        );
        assert!(browser.apply_results(nq.request_id, vec![contract("NQZ6", "NQ", "202612")]));
        assert_eq!(
            labels(&browser),
            [
                ("ES".to_string(), false),
                ("Loading contracts".to_string(), true),
                ("NQ".to_string(), false),
            ]
        );
        let es = browser
            .begin_retained_search()
            .expect("the replaced expansion searches once the lane frees");
        assert_eq!(es.query, "ES");
        assert!(browser.begin_retained_search().is_none());
    }

    #[test]
    fn a_rejected_contract_search_collapses_its_product() {
        let mut browser = listed_browser();
        let search = browser.toggle_product(&key("NQ")).expect("NQ contracts");
        assert!(browser.reject_search(search.request_id));
        assert_eq!(labels(&browser).len(), 2);
        assert!(
            browser.toggle_product(&key("NQ")).is_some(),
            "expanding retries"
        );
    }

    #[test]
    fn consuming_an_expanded_contract_selection_keeps_the_product_listing() {
        let mut browser = listed_browser();
        let search = browser.toggle_product(&key("NQ")).expect("NQ contracts");
        assert!(browser.apply_results(search.request_id, vec![contract("NQZ6", "NQ", "202612")]));
        let selection = browser.select(ExpandedContract(0)).expect("contract");
        assert!(browser.confirm_selection(selection.generation));
        assert!(browser.consume_completed_search(selection.search_generation));
        assert_eq!(labels(&browser).len(), 2);
        assert!(browser.lists_query(""));
        assert_eq!(browser.menu_entries()[1].expanded, Some(false));
    }

    #[test]
    fn typed_contracts_group_under_their_product_name() {
        let mut browser = listed_browser();
        let typed = browser.begin_search("NQZ6").expect("query validates");
        assert!(browser.apply_results(
            typed.request_id,
            vec![
                contract("NQZ6", "NQ", "20261218"),
                contract("MNQZ6", "MNQ", "20261218"),
                contract("NQH7", "NQ", "20270319"),
            ],
        ));
        assert_eq!(
            labels(&browser),
            [
                ("E-mini Nasdaq-100".to_string(), false),
                ("NQZ6".to_string(), true),
                ("NQH7".to_string(), true),
                ("MNQ".to_string(), false),
                ("MNQZ6".to_string(), true),
            ]
        );
        let entries = browser.menu_entries();
        assert_eq!(entries[0].expanded, Some(true));
        assert_eq!(entries[0].detail.as_deref(), Some("futures · CME"));
        assert_eq!(entries[4].description.as_deref(), Some("MNQ contract"));
        assert_eq!(
            entries[2].action,
            InstrumentMenuAction::Select(SearchResult(2))
        );
        assert_eq!(
            browser.listing_summary(),
            Some(SymbolListingSummary::Contracts {
                contracts: 3,
                products: 2
            })
        );

        assert!(
            browser.toggle_product(&key("NQ")).is_none(),
            "typed groups toggle without a search"
        );
        assert_eq!(labels(&browser).len(), 3);
        assert!(browser.toggle_product(&key("NQ")).is_none());
        assert_eq!(labels(&browser).len(), 5);
        assert!(!browser.search_pending());
    }

    #[test]
    fn instruments_without_a_product_stay_flat() {
        let mut browser = RithmicSymbolBrowser::default();
        let search = browser.begin_search("").expect("listing");
        let flat = ProviderInstrumentSummary {
            symbol: "BTC".to_string(),
            display_symbol: "BTC-USDC".to_string(),
            exchange: "Hyperliquid".to_string(),
            asset_class: Some("perpetual".to_string()),
            ..ProviderInstrumentSummary::default()
        };
        assert!(browser.apply_results(search.request_id, vec![flat]));
        let entries = browser.menu_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "BTC-USDC");
        assert_eq!(entries[0].description, None);
        assert_eq!(entries[0].detail.as_deref(), Some("perpetual"));
        assert!(!entries[0].nested);
        assert_eq!(entries[0].expanded, None);
        assert_eq!(
            browser.listing_summary(),
            Some(SymbolListingSummary::Markets(1))
        );
    }

    #[test]
    fn contract_months_parse_only_dated_expirations() {
        assert_eq!(contract_month("20261218").as_deref(), Some("Dec 2026"));
        assert_eq!(contract_month("2027-06-18").as_deref(), Some("Jun 2027"));
        assert_eq!(contract_month("202703").as_deref(), Some("Mar 2027"));
        for invalid in ["", "2027", "202713", "202700", "Dec26", "2026121"] {
            assert_eq!(contract_month(invalid), None, "{invalid}");
        }
    }
}
