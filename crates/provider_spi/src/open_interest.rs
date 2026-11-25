//! Provider-neutral historical and current open-interest contracts shared between
//! exchange adapters and `market_runtime`.

use std::sync::{Arc, atomic::AtomicBool};

use aeris_market_data::BarPeriod;

use crate::HistoryInstrument;

/// Maximum open-interest points requested or retained for one chart series fetch.
pub const MAXIMUM_OPEN_INTEREST_POINTS: usize = 500;

/// One canonical open-interest observation aligned to a bar timestamp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OpenInterestSample {
    /// Bar-aligned UTC timestamp in Unix nanoseconds.
    pub timestamp_unix_nanos: i64,
    /// Open interest in base-asset contracts / coins (e.g. BTC).
    pub contracts: f64,
    /// Open interest valued in quote USD when supplied by the exchange.
    pub notional_usd: Option<f64>,
}

/// Bounded request for historical and current open interest of one installed instrument.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenInterestRequest {
    pub instrument: HistoryInstrument,
    pub period: BarPeriod,
    pub maximum_points: usize,
}

/// Provider-neutral open-interest fetcher registered on a provider runtime specification.
pub type OpenInterestSource = dyn Fn(&OpenInterestRequest, &Arc<AtomicBool>) -> Result<Vec<OpenInterestSample>, String>
    + Send
    + Sync;
