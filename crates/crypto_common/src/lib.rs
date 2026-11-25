//! Shared deterministic primitives for cryptocurrency exchange adapters.
//!
//! Pure domain-layer building blocks with zero network I/O:
//! - Checked decimal-to-fixed-point conversion without `f64` round trips
//! - Bounded local L2 order book for snapshot+delta synchronization and
//!   cross-venue tick-bucketed order book aggregation
//! - Bounded trade-ID deduplication ring
//! - Shared candle history window sizing
//! - Canonical cross-exchange symbol normalization (`BTC-USDT-PERP`, `BTC-USDT-SPOT`)

pub mod book;
pub mod canonical;
pub mod decimal;
pub mod error;
pub mod history;
pub mod liquidations;
pub mod trades;

pub use book::{
    BookError, BookLevelUpdate, DEFAULT_MAX_BOOK_LEVELS, LocalOrderBook, MAXIMUM_BOOK_LEVELS,
    merge_books,
};
pub use canonical::{CanonicalCryptoKey, CryptoMarketKind};
pub use decimal::{
    FUNDING_RATE_SCALE, MAXIMUM_CRYPTO_DECIMALS, NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE,
    NOTIONAL_SCALE, RawDecimal, decimal_places_for_step, multiply_fixed_decimal,
    parse_aggregate_decimal_to_fixed, parse_decimal_to_fixed,
};
pub use error::{CryptoError, CryptoResult};
pub use history::{bar_period_millis, history_window, history_window_for_range};
pub use liquidations::{
    LiquidationBucket, LiquidationEvent, LiquidationLedger, bucket_liquidation_events,
};
pub use trades::TradeDedupRing;
