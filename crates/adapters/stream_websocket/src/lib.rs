//! Bounded binary WebSocket message adapter for market-bar client sessions.
//!
//! This crate validates WebSocket message semantics and feeds the existing bounded
//! binary decoder and single-writer client model. It does not establish TLS,
//! authenticate, enforce entitlements, connect to a provider, or claim production
//! transport readiness.

mod background_runtime;
mod endpoint;
mod plain_loopback_owner;
mod session;

pub use background_runtime::{
    PlainLoopbackBackgroundRuntime, PlainLoopbackRuntimeCommand, PlainLoopbackRuntimeConfig,
    PlainLoopbackRuntimeConfigError, PlainLoopbackRuntimeEvent, PlainLoopbackRuntimeJoinError,
    PlainLoopbackRuntimePortError, PlainLoopbackRuntimeStartError,
};
pub use endpoint::{PlainLoopbackEndpointError, PlainLoopbackWebSocketEndpoint};
pub use plain_loopback_owner::{
    PlainLoopbackCommand, PlainLoopbackLifecycleConfig, PlainLoopbackLifecycleConfigError,
    PlainLoopbackLifecycleError, PlainLoopbackLifecycleEvent, PlainLoopbackLifecycleState,
    PlainLoopbackLifecycleStep, PlainLoopbackMarketWebSocketOwner, WebSocketControlSignal,
};
pub use session::{
    MarketWebSocketConfig, MarketWebSocketConfigError, MarketWebSocketError,
    MarketWebSocketPublication, MarketWebSocketSession, MarketWebSocketState,
    WebSocketMessageOutcome, WebSocketRecoveryReason,
};
