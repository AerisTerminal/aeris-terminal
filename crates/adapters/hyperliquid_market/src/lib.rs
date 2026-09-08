//! Local-first Hyperliquid public market-data adapter.
//!
//! This crate owns every Hyperliquid wire concern: direct mainnet
//! HTTPS/WebSocket requests, JSON decoding, metadata mapping, decimal to
//! fixed-point conversion, and connection mechanics. Provider wire types stay
//! private; the public surface returns normalized domain events
//! ([`axiusflow_market_data`]) plus adapter metadata.
//!
//! Market data only: no orders, wallet signing, execution, or paper trading.
//! Supported markets are core perpetuals, spot pairs, and builder-deployed
//! (HIP-3) perpetual namespaces. Depth is the documented public snapshot
//! (five levels per side on the fast WebSocket feed); the adapter exposes
//! available depth honestly and never advertises full-depth or order-level
//! data.

mod book;
mod candles;
mod context;
mod decimal;
mod endpoints;
mod http;
mod identity;
mod meta;
mod socket;
mod trades;
mod ws;

pub use book::{
    DecodedBook, MAXIMUM_HYPERLIQUID_BOOK_LEVELS, decode_bbo_quote, decode_book_snapshot,
};
pub use candles::{
    HyperliquidCandlePage, HyperliquidLiveCandle, MAXIMUM_HYPERLIQUID_CANDLES, decode_candle_page,
    decode_live_candle, hyperliquid_interval_for_period, merge_live_candle,
    period_for_hyperliquid_interval,
};
pub use context::{HyperliquidMarketContext, decode_asset_context};
pub use decimal::{
    FUNDING_RATE_SCALE, MAXIMUM_HYPERLIQUID_DECIMALS, NORMALIZED_PRICE_SCALE,
    NORMALIZED_QUANTITY_SCALE, NOTIONAL_SCALE, RawDecimal, funding_for_rate,
    parse_decimal_to_fixed, quantity_for_size, scale_for_market as decimal_scale_for_market,
};
pub use endpoints::{HYPERLIQUID_INFO_URL, HYPERLIQUID_WS_URL};
pub use http::{
    CandleSnapshotRequest, HyperliquidHttpConfig, fetch_candle_snapshot, fetch_meta_bundle,
    post_info,
};
pub use identity::{
    HyperliquidInstrument, HyperliquidMarketKind, instrument_id_for, wire_coin_for,
};
pub use meta::{HyperliquidCatalog, RawMetaBundle, decode_catalog};
pub use socket::{HyperliquidSocket, HyperliquidSocketShutdown, SocketEvent, is_read_timeout};
pub use trades::{HyperliquidTradeBatch, TradeDedup, decode_trades_batch};
pub use ws::{
    WsClientEvent, build_bbo_subscription, build_candle_subscription, build_l2_subscription,
    build_ping, build_trades_subscription, build_unsubscribe, parse_ws_frame,
};
