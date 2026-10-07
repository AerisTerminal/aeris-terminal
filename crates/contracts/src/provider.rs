//! Provider-neutral presentation metadata published by `market_runtime`.

/// How a provider session is established from the desktop's perspective.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderConnectionKind {
    /// The provider uses credentials held by the native vault.
    Credentials,
    /// The provider is available without account credentials.
    Public,
    /// The provider is authorized through a hosted broker capability.
    HostedBroker,
}

/// Which persisted instrument identity is accepted by a provider catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderCatalogSymbol {
    ProviderSymbol,
    DisplaySymbol,
}

/// Stable provider metadata for catalog and chart presentation.
// Each flag is an independent, static provider capability, not encoded state.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderPresentationDescriptor {
    pub id: &'static str,
    pub display_name: &'static str,
    pub chart_interval_labels: &'static [&'static str],
    pub default_listing: &'static str,
    pub search_hint: &'static str,
    pub logo_key: &'static str,
    pub catalog_symbol: ProviderCatalogSymbol,
    pub selection_entitlement_id: &'static str,
    pub catalog_refresh_on_startup: bool,
    pub ready_label_suffix: &'static str,
    pub depth_available: bool,
    /// Whether search honours `InstrumentSearchCategories` (it serves more than one category).
    pub search_categories_available: bool,
    /// Whether the provider answers `ScreenProviderMarkets` with listed-market statistics.
    pub market_screen_available: bool,
    pub connection_kind: ProviderConnectionKind,
}
