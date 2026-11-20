//! Concrete built-in market provider registrations isolated from neutral
//! `market_service` core orchestration.
//!
//! Each submodule packages one provider's descriptor, capabilities, history
//! source, session binding, catalog dispatcher, and worker launcher so the
//! core coordinator, runtime registry, and session manager stay provider-free.

pub(super) mod hyperliquid;
pub(super) mod rithmic;
