//! Deterministic desktop fixtures, excluded from the default executable.

use std::time::Duration;

mod fixture_market;
mod market_events;
mod providers;

pub use fixture_market::FixtureMarketWorker;
pub use market_events::{FixtureMarketEvents, FixtureSeries};
pub use providers::{fixture_futures_contract, fixture_futures_contracts, fixture_public_crypto};

/// Test trading owners checkpoint SQLite while parallel tests compete for the disk, so their
/// shutdown gets more headroom than the production exit deadline.
pub const TRADING_OWNER_TEST_SHUTDOWN: Duration = Duration::from_secs(10);
