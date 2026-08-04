//! Coinbase Advanced Trade public market-data adapter.
//!
//! Subscribes to the public `heartbeats` and `market_trades` WebSocket channels,
//! validates `sequence_num` continuity across the connection, decodes trades to
//! exact fixed-point values, and emits canonical events with the full timestamp
//! vocabulary. The feed is legally authorized public data; the entitlement class
//! is `crypto_public_realtime` and every event carries `coinbase` provenance.

mod config;
mod decoder;
mod errors;
mod fixed_point;
mod history;
mod messages;
mod review;
mod session;

pub use config::{CoinbaseConfig, MAXIMUM_PRODUCTS};
pub use decoder::{CanonicalTrade, CoinbaseDecoder, DecoderMetrics};
pub use errors::CoinbaseError;
pub use fixed_point::FixedPointValue;
pub use history::{
    COINBASE_PUBLIC_ACCOUNT_ID, CoinbaseHistoryCapabilityAdapter, CoinbaseHistoryTransport,
    CoinbaseHttpsHistoryTransport, decode_history_bar,
};
pub use review::{ENTITLEMENT_CLASS, PROVIDER, PROVIDER_REVIEW, TERMS_REVIEW, WEBSOCKET_ENDPOINT};
pub use session::{CoinbaseConnection, CoinbaseSession, SessionHealth, SessionOutcome};
