//! Local-first Bybit V5 public market-data adapter (Linear Perpetuals and Spot).
//!
//! Market data only: no API keys, signing, or execution.
//! Supports 200-level L2 order books (`orderbook.200`), real-time public trades
//! (`publicTrade`), native klines (`kline`), and historical candles.

pub mod book;
pub mod candles;
pub mod endpoints;
pub mod history;
pub mod http;
pub mod identity;
pub mod meta;
pub mod open_interest;
pub mod trades;
pub mod ws;

pub use book::{BybitBookMessage, BybitBookState, MAXIMUM_BYBIT_BOOK_LEVELS, decode_orderbook};
pub use candles::{
    BybitCandlePage, BybitLiveCandle, MAXIMUM_BYBIT_CANDLES, bybit_interval_for_period,
    decode_candle_page, decode_live_candle, period_for_bybit_interval,
};
pub use endpoints::{BYBIT_LINEAR_WS_URL, BYBIT_REST_URL, BYBIT_SPOT_WS_URL};
pub use http::{BybitHttpClient, BybitHttpConfig, fetch_catalog};
pub use identity::{
    BybitInstrument, BybitMarketKind, BybitSymbolRef, instrument_id_for, parse_provider_symbol,
};
pub use meta::BybitCatalog;
pub use trades::decode_public_trades;
pub use ws::{BybitWsEvent, build_ping, build_subscribe, build_unsubscribe, parse_ws_frame};
