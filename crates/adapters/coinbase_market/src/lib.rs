//! Coinbase Advanced Trade public market-data adapter.
//!
//! Subscribes to the public `heartbeats` and `market_trades` WebSocket channels,
//! validates `sequence_num` continuity across the connection, decodes trades to
//! exact fixed-point values, and emits canonical events with the full timestamp
//! vocabulary. The feed is legally authorized public data; the entitlement class
//! is `crypto_public_realtime` and every event carries `coinbase` provenance.

mod aggregation;
mod catalog;
mod config;
mod decoder;
mod errors;
mod fixed_point;
mod history;
mod interval;
mod level2;
mod messages;
mod network_cache;
mod review;
mod session;

pub use aggregation::{
    CoinbaseAggregatedBar, CoinbaseBarAggregationError, CoinbaseBarAggregator,
    CoinbaseBarAggregatorConfig, MAXIMUM_AGGREGATED_HISTORY_BARS, mantissa_at_scale,
};
pub use catalog::{
    CoinbaseCatalogDiagnostics, CoinbaseProductCatalog, CoinbaseSpotProduct, coinbase_instrument_id,
};
pub use config::{CoinbaseConfig, MAXIMUM_PRODUCTS};
pub use decoder::{CanonicalTrade, CoinbaseDecoder, DecoderMetrics};
pub use errors::CoinbaseError;
pub use fixed_point::FixedPointValue;
pub(crate) use history::PublicRequestGate;
pub use history::{
    COINBASE_PUBLIC_ACCOUNT_ID, CoinbaseHistoryBatch, CoinbaseHistoryCapabilityAdapter,
    CoinbaseHistoryDiagnostics, CoinbaseHistoryTransport, CoinbaseHttpsHistoryTransport,
    decode_history_bar, decode_history_segment, encode_history_bar, encode_history_segment,
    history_segment_item_count,
};
pub use interval::{CoinbaseAggregationDiagnostics, CoinbaseInterval, aggregate_coinbase_bars};
pub use level2::{
    COINBASE_MAXIMUM_LEVELS_PER_SIDE, CoinbaseLevel2Book, CoinbaseLevel2Diagnostics,
    CoinbaseLevel2Outcome, coinbase_depth_limit,
};
pub use network_cache::{
    COINBASE_ACCELERATION_CACHE_BYTES, CoinbaseCacheDiagnostics, CoinbaseNetworkFirstCache,
};
pub use review::{ENTITLEMENT_CLASS, PROVIDER, PROVIDER_REVIEW, TERMS_REVIEW, WEBSOCKET_ENDPOINT};
pub use session::{CoinbaseConnection, CoinbaseSession, SessionHealth, SessionOutcome};
