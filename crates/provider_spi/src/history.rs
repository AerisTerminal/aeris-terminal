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
