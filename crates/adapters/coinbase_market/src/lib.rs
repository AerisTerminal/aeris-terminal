//! Coinbase Advanced Trade public market-data adapter.
//!
//! Subscribes to the public `heartbeats` and `market_trades` WebSocket channels,
//! validates `sequence_num` continuity across the connection, decodes trades to
//! exact fixed-point values, and emits canonical events with the full timestamp
//! vocabulary. The feed is legally authorized public data; the entitlement class
//! is `crypto_public_realtime` and every event carries `coinbase` provenance.

mod aggregation;
mod config;
mod decoder;
mod desktop_driver;
mod errors;
mod fixed_point;
mod fixture;
mod history;
mod messages;
mod review;
mod session;

pub use aggregation::{
    CoinbaseAggregatedBar, CoinbaseBarAggregationError, CoinbaseBarAggregator,
    CoinbaseBarAggregatorConfig, MAXIMUM_AGGREGATED_HISTORY_BARS, mantissa_at_scale,
};
pub use config::{CoinbaseConfig, MAXIMUM_PRODUCTS};
pub use decoder::{CanonicalTrade, CoinbaseDecoder, DecoderMetrics};
pub use desktop_driver::{
    CoinbaseDesktopEventError, CoinbaseProviderDriver, CoinbaseProviderDriverError,
    CoinbaseProviderEvent, CoinbaseProviderEvents, CoinbaseProviderInvalidReason,
    seed_coinbase_bar_history, try_recv_coinbase_aggregated_bar, try_recv_coinbase_bar,
    try_recv_coinbase_trade,
};
pub use errors::CoinbaseError;
pub use fixed_point::FixedPointValue;
pub use fixture::{CoinbaseFixtureSession, deterministic_fixture_session};
pub use history::{
    COINBASE_PUBLIC_ACCOUNT_ID, CoinbaseHistoryCapabilityAdapter, CoinbaseHistoryTransport,
    CoinbaseHttpsHistoryTransport, decode_history_bar, decode_history_segment,
    encode_history_segment,
};
pub use review::{ENTITLEMENT_CLASS, PROVIDER, PROVIDER_REVIEW, TERMS_REVIEW, WEBSOCKET_ENDPOINT};
pub use session::{CoinbaseConnection, CoinbaseSession, SessionHealth, SessionOutcome};
