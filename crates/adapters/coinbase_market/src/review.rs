//! Provider and terms evidence for the Coinbase public feed.

/// Provider identity carried in provenance.
pub const PROVIDER: &str = "coinbase";
/// Entitlement class for the public real-time feed.
pub const ENTITLEMENT_CLASS: &str = "crypto_public_realtime";
/// Public advanced-trade WebSocket endpoint.
pub const WEBSOCKET_ENDPOINT: &str = "wss://advanced-trade-ws.coinbase.com";
/// DNS host for the public advanced-trade WebSocket endpoint.
pub(crate) const WEBSOCKET_HOST: &str = "advanced-trade-ws.coinbase.com";
