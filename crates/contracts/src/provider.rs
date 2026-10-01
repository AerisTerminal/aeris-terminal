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

/// Stable provider metadata for catalog and chart presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderPresentationDescriptor {
    pub id: &'static str,
    pub display_name: &'static str,
    pub chart_interval_labels: &'static [&'static str],
    pub default_listing: &'static str,
    pub search_hint: &'static str,
    pub logo_key: &'static str,
    pub depth_available: bool,
    pub connection_kind: ProviderConnectionKind,
}
