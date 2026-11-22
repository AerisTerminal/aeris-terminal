//! Public Binance USDT-M Futures and Spot HTTPS/WebSocket endpoints.

/// Public USDT-M Futures REST API base URL.
pub const BINANCE_FUTURES_REST_URL: &str = "https://fapi.binance.com";
/// Public USDT-M Futures market-data WebSocket stream URL (`aggTrade`, `kline`).
pub const BINANCE_FUTURES_MARKET_WS_URL: &str = "wss://fstream.binance.com/market/stream";
/// Public USDT-M Futures high-frequency public WebSocket stream URL (`depth`, `bookTicker`).
pub const BINANCE_FUTURES_PUBLIC_WS_URL: &str = "wss://fstream.binance.com/public/stream";

/// Public Spot REST API base URL.
pub const BINANCE_SPOT_REST_URL: &str = "https://api.binance.com";
/// Public Spot market-data mirror used if the primary Spot REST endpoint is geo-restricted.
pub const BINANCE_SPOT_FALLBACK_REST_URL: &str = "https://data-api.binance.vision";
/// Public Spot combined WebSocket stream URL.
pub const BINANCE_SPOT_WS_URL: &str = "wss://stream.binance.com:9443/stream";
