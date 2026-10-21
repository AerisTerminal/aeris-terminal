//! Deterministic desktop fixtures, excluded from the default executable.

mod fixture_market;
mod market_events;

pub use fixture_market::FixtureMarketWorker;
pub use market_events::{FixtureMarketEvents, FixtureSeries};
