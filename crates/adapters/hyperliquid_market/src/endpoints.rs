//! Direct Hyperliquid mainnet endpoints for public market testing.
//!
//! No credentials are used; every request is an unauthenticated public
//! market-data call.

/// Public info endpoint used for metadata, candles, books, and context.
pub const HYPERLIQUID_INFO_URL: &str = "https://api.hyperliquid.xyz/info";
/// Public multiplexed WebSocket endpoint for market-data subscriptions.
pub const HYPERLIQUID_WS_URL: &str = "wss://api.hyperliquid.xyz/ws";
