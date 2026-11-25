//! Local-first OKX V5 public market-data adapter (Linear Perpetual Swaps and Spot).
//!
//! Market data only: no API keys, signing, or execution.
//! Supports 400-level L2 order books (`books` with strict `seqId`/`prevSeqId`
//! continuity checks), real-time trades (`trades`), native klines (`candle*`),
//! and historical candles. All SWAP contract sizes are normalized to base-asset
//! units via `ctVal`.

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

pub use book::{
    MAXIMUM_OKX_BOOK_LEVELS, OkxBookApplyOutcome, OkxBookMessage, OkxBookState, decode_books,
};
pub use candles::{
    MAXIMUM_OKX_CANDLES, OkxCandlePage, OkxLiveCandle, decode_candle_page, decode_live_candle,
    okx_interval_for_period, period_for_okx_interval,
};
pub use endpoints::{OKX_BUSINESS_WS_URL, OKX_PUBLIC_WS_URL, OKX_REST_URL};
pub use http::{OkxHttpClient, OkxHttpConfig, fetch_catalog};
pub use identity::{
    OkxInstrument, OkxMarketKind, OkxSymbolRef, instrument_id_for, parse_provider_symbol,
};
pub use meta::OkxCatalog;
pub use trades::decode_trades;
pub use ws::{OkxWsEvent, build_ping, build_subscribe, build_unsubscribe, parse_ws_frame};
