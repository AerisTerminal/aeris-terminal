//! Provider and terms evidence for the Coinbase public feed.

/// Provider identity carried in provenance.
pub const PROVIDER: &str = "coinbase";
/// Entitlement class for the public real-time feed.
pub const ENTITLEMENT_CLASS: &str = "crypto_public_realtime";
/// Public advanced-trade WebSocket endpoint.
pub const WEBSOCKET_ENDPOINT: &str = "wss://advanced-trade-ws.coinbase.com";
/// DNS host for the public advanced-trade WebSocket endpoint.
pub(crate) const WEBSOCKET_HOST: &str = "advanced-trade-ws.coinbase.com";
/// Recorded provider review.
pub const PROVIDER_REVIEW: &str = "Coinbase Advanced Trade public market data requires no API key for heartbeats, market_trades, candles, ticker, status, and level2 channels; the adapter subscribes only to public channels, sends no authenticated request, and records the venue in every event's provenance";
/// Recorded terms review.
pub const TERMS_REVIEW: &str = "the feed is used for display inside this terminal under Coinbase's published API terms; raw-feed redistribution is not performed, the entitlement class crypto_public_realtime is recorded on every event, and delayed-vs-real-time equity tiers do not apply to this venue";
