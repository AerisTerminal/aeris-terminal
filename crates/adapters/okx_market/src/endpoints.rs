//! Public OKX V5 REST and WebSocket endpoints.

/// Public OKX V5 REST API base URL.
pub const OKX_REST_URL: &str = "https://www.okx.com";
/// Public OKX V5 WebSocket URL (`books`, `trades`, `tickers`).
pub const OKX_PUBLIC_WS_URL: &str = "wss://ws.okx.com/ws/v5/public";
/// Public OKX V5 Business WebSocket URL (`candle*` live klines).
pub const OKX_BUSINESS_WS_URL: &str = "wss://ws.okx.com/ws/v5/business";
