//! Public Coinbase Advanced Trade (Spot) market-data adapter for Aeris Terminal.

pub mod book;
pub mod candles;
pub mod endpoints;
pub mod history;
pub mod http;
pub mod identity;
pub mod meta;
pub mod trades;
pub mod ws;

pub use book::{
    CoinbaseBookApplyOutcome, CoinbaseBookMessage, CoinbaseBookState, MAXIMUM_COINBASE_BOOK_LEVELS,
    decode_l2_events,
};
pub use candles::{
    CoinbaseCandlePage, MAXIMUM_COINBASE_CANDLES, coinbase_interval_for_period, decode_candle_page,
    period_for_coinbase_interval,
};
pub use endpoints::{COINBASE_REST_URL, COINBASE_WS_URL};
pub use http::{CoinbaseHttpClient, CoinbaseHttpConfig, fetch_catalog};
pub use identity::{
    CoinbaseInstrument, CoinbaseMarketKind, CoinbaseSymbolRef, instrument_id_for,
    parse_provider_symbol,
};
pub use meta::{CoinbaseCatalog, decode_products_page, summary_for_instrument};
pub use trades::{
    aggressor_from_maker_side, decode_ticker_trades, decode_ws_trades, parse_rfc3339_nanos,
};
pub use ws::{CoinbaseWsEvent, build_subscribe, build_unsubscribe, parse_ws_frame};
