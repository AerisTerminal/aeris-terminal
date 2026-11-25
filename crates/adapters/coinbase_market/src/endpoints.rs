//! Official public Coinbase Advanced Trade market-data endpoints.

/// Coinbase Advanced Trade public market-data REST API base URL.
pub const COINBASE_REST_URL: &str = "https://api.coinbase.com/api/v3/brokerage/market";

/// Coinbase Advanced Trade public WebSocket feed (`market_trades`, `level2`, `heartbeats`).
pub const COINBASE_WS_URL: &str = "wss://advanced-trade-ws.coinbase.com";
