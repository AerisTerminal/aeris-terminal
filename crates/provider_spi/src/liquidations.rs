//! Provider-neutral bar-aligned liquidation volume contracts.

use std::sync::{Arc, atomic::AtomicBool};

use aeris_market_data::BarPeriod;

use crate::HistoryInstrument;

/// Upper bound on liquidation points requested or retained per bar series.
pub const MAXIMUM_LIQUIDATION_POINTS: usize = 500;

/// One bar-aligned liquidation volume sample returned by a provider adapter.
///
/// Volumes are aggregated per chart bar bucket in both USD notional (`long_usd`, `short_usd`) and
/// base-asset quantity (`long_contracts`, `short_contracts`). All volumes are finite and
/// non-negative.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiquidationSample {
    pub timestamp_unix_nanos: i64,
    pub long_usd: f64,
    pub short_usd: f64,
    pub long_contracts: f64,
    pub short_contracts: f64,
}

/// Request parameters for fetching bar-aligned liquidation volumes for an instrument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiquidationRequest {
    pub instrument: HistoryInstrument,
    pub period: BarPeriod,
    pub maximum_points: usize,
}

/// Provider-neutral liquidation fetcher registered on a provider runtime spec.
pub type LiquidationSource = dyn Fn(&LiquidationRequest, &Arc<AtomicBool>) -> Result<Vec<LiquidationSample>, String>
    + Send
    + Sync
    + 'static;
