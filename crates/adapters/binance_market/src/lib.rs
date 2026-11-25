//! Local-first Binance public market-data adapter (USDT-M Perpetuals and Spot).
//!
//! Market data only: no API keys, signing, or execution.
//! Supports deep L2 order books (up to 1,000 levels per side with strict
//! snapshot+delta sequence synchronization), aggregate trades (`aggTrade`),
//! top-of-book (`bookTicker`), native klines, and historical candles.

pub mod book;
pub mod candles;
pub mod endpoints;
pub mod history;
pub mod http;
pub mod identity;
pub mod liquidations;
pub mod meta;
pub mod open_interest;
pub mod trades;
pub mod ws;

pub use book::{
    BinanceDepthUpdate, BinanceOrderBookSync, MAXIMUM_BINANCE_BOOK_LEVELS, decode_book_ticker,
    decode_depth_snapshot, decode_depth_update,
};
pub use candles::{
    BinanceCandlePage, BinanceLiveCandle, MAXIMUM_BINANCE_FUTURES_CANDLES,
    MAXIMUM_BINANCE_SPOT_CANDLES, binance_interval_for_period, decode_candle_page,
    decode_live_candle, period_for_binance_interval,
};
pub use endpoints::{
    BINANCE_FUTURES_MARKET_WS_URL, BINANCE_FUTURES_PUBLIC_WS_URL, BINANCE_FUTURES_REST_URL,
    BINANCE_SPOT_FALLBACK_REST_URL, BINANCE_SPOT_REST_URL, BINANCE_SPOT_WS_URL,
};
pub use http::{BinanceHttpClient, BinanceHttpConfig, fetch_catalog};
pub use identity::{
    BinanceInstrument, BinanceMarketKind, BinanceSymbolRef, instrument_id_for,
    parse_provider_symbol,
};
pub use liquidations::decode_force_order;
pub use meta::BinanceCatalog;
pub use trades::decode_agg_trade;
pub use ws::{BinanceWsEvent, build_subscribe, build_unsubscribe, parse_ws_frame};
