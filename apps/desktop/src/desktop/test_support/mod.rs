//! Deterministic desktop fixtures, excluded from the default executable.

mod fixture_market;
mod market_events;
mod provider_instruments;

pub use fixture_market::FixtureMarketWorker;
pub use market_events::{FixtureMarketEvents, FixtureSeries};
pub use provider_instruments::{
    fixture_futures_contract, fixture_futures_contracts, fixture_public_crypto,
};
