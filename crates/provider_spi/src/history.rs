//! Provider-neutral history fetch contracts shared between adapters and `market_runtime`.

use aeris_market_data::MarketBar;
use aeris_provider_history::HistoryRange;

/// The period that was still open when a provider served a history page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FormingBar {
    pub bar: MarketBar,
    pub trades: Option<u32>,
}

/// Bounded window passed to a provider history fetcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoryFetchWindow {
    pub maximum_bars: usize,
    pub range: Option<HistoryRange>,
}

/// Non-wire contract metadata needed by a provider history fetcher.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HistoryContractMetadata {
    pub point_value: Option<(i64, u8)>,
    pub currency: Option<String>,
    pub expiration_date: Option<String>,
    pub first_notice_date: Option<String>,
    pub last_trade_date: Option<String>,
}

/// Provider-neutral installed instrument projection passed to history fetchers so
/// adapters never depend on the workspace persistence schema (`aeris_contracts`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HistoryInstrument {
    pub provider: String,
    pub session_generation: u64,
    pub instrument_id: String,
    pub provider_symbol: String,
    pub display_symbol: String,
    pub venue_id: String,
    pub entitlement_id: String,
    pub price_scale: u32,
    pub quantity_scale: u32,
    pub price_increment: Option<i64>,
    pub contract: Option<HistoryContractMetadata>,
}

/// Canonical bar page returned by a provider history fetcher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderHistorySnapshot {
    pub price_scale: u8,
    pub quantity_scale: u8,
    pub bars: Vec<MarketBar>,
    pub forming: Option<FormingBar>,
    pub handoff_boundary_unix_nanos: Option<i64>,
    pub backwards_exhausted: bool,
}
