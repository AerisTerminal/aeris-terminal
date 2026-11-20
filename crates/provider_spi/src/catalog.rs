//! Provider catalog commands and their bounded search, selection and screen results.

/// Instrument categories a provider search may return. Providers without a category simply
/// ignore it; a provider that serves a category from a slow remote lookup skips that lookup
/// entirely when the category is excluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstrumentSearchCategories {
    pub futures: bool,
    pub equities: bool,
}

impl InstrumentSearchCategories {
    pub const ALL: Self = Self {
        futures: true,
        equities: true,
    };
}

impl Default for InstrumentSearchCategories {
    fn default() -> Self {
        Self::ALL
    }
}

/// Requests one bounded exact provider-instrument search for a market consumer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchProviderInstruments {
    pub consumer_id: u64,
    pub search_generation: u64,
    /// Startup metadata resolution has its own command lane beside picker searches.
    pub startup_resolve: bool,
    pub provider: String,
    pub query: String,
    pub maximum_results: u32,
    pub categories: InstrumentSearchCategories,
    /// Restricts the search to one of the provider's exchanges, as named by its results.
    pub exchange: Option<String>,
}

/// Selects one exact instrument from the latest completed provider search.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectProviderInstrument {
    pub consumer_id: u64,
    pub selection_generation: u64,
    pub search_generation: u64,
    pub startup_resolve: bool,
    pub provider: String,
    pub symbol: String,
    pub exchange: String,
    pub entitlement_id: String,
}

/// What one provider search result names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProviderInstrumentKind {
    /// A selectable instrument.
    #[default]
    Instrument,
    /// A product that groups dated contracts. It is never selectable; a search
    /// for its `product_code` lists its contracts.
    Product,
    /// A provider alias that follows a product's front-month contract. It stays
    /// resolvable for routes saved on it, but symbol menus list the dated
    /// contract it points at instead.
    FrontMonthAlias,
}

/// Bounded provider-neutral presentation metadata for one search result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderInstrumentSummary {
    pub kind: ProviderInstrumentKind,
    /// Exact provider routing symbol used for selection requests.
    pub symbol: String,
    /// Provider-owned human presentation symbol. Consumers must render this
    /// rather than deriving a label from `symbol`.
    pub display_symbol: String,
    pub exchange: String,
    pub name: Option<String>,
    pub product_code: Option<String>,
    pub instrument_type: Option<String>,
    pub expiration_date: Option<String>,
    /// Provider-owned asset-class name, such as "Forex" or "US Shares". Display text
    /// only; consumers must not branch on it.
    pub asset_class: Option<String>,
}

/// Completed bounded provider-instrument search publication.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderInstrumentSearchResult {
    pub consumer_id: u64,
    pub provider: String,
    pub provider_generation: u64,
    pub search_generation: u64,
    pub instruments: Vec<ProviderInstrumentSummary>,
}

/// Requests one bounded market-statistics screen across a provider's listed instruments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScreenProviderMarkets {
    pub consumer_id: u64,
    pub screen_generation: u64,
    pub provider: String,
    pub maximum_results: u32,
}

/// Provider-reported statistics for one listed instrument. Each value carries an explicit
/// fixed-point scale; an absent value was not reported by the provider and is never
/// fabricated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderMarketStatistics {
    /// Selection identity and presentation metadata, identical to a search result.
    pub instrument: ProviderInstrumentSummary,
    /// Decimal scale for `mark_price` and `previous_day_price`.
    pub price_scale: u32,
    /// Provider mark price.
    pub mark_price: Option<i64>,
    /// Provider reference price from one day earlier, the base for day change.
    pub previous_day_price: Option<i64>,
    /// Decimal scale for `day_notional_volume` and `open_interest_notional`.
    pub notional_scale: u32,
    /// Quote-currency volume traded over the provider's trailing day.
    pub day_notional_volume: Option<i64>,
    /// Open interest valued at the mark price, in quote currency.
    pub open_interest_notional: Option<i64>,
    /// Decimal scale for `funding_rate`.
    pub funding_rate_scale: u32,
    /// Current funding rate per `funding_interval_seconds`, as a fraction (not percent).
    pub funding_rate: Option<i64>,
    /// Interval the funding rate applies to; absent when `funding_rate` is absent.
    pub funding_interval_seconds: Option<u32>,
}

/// Completed bounded provider market screen, ordered by day notional volume descending.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderMarketScreen {
    pub consumer_id: u64,
    pub provider: String,
    pub provider_generation: u64,
    pub screen_generation: u64,
    /// Wall-clock time the provider statistics were fetched.
    pub captured_at_unix_millis: i64,
    pub markets: Vec<ProviderMarketStatistics>,
}

/// Coarse catalog-command failure safe for presentation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum ProviderCatalogRejectionReason {
    #[default]
    Unspecified = 0,
    SearchRejected = 1,
    SupersededSearch = 2,
    InstrumentUnavailable = 3,
    SubscriptionRejected = 4,
    DispatchUnavailable = 5,
    SearchTimedOut = 6,
    SelectionTimedOut = 7,
}

/// Generation-fenced provider catalog command rejection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderCatalogRejected {
    pub consumer_id: u64,
    pub provider: String,
    pub provider_generation: Option<u64>,
    pub command_generation: u64,
    pub reason: ProviderCatalogRejectionReason,
}
