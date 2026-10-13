//! Provider-neutral history capabilities and live cutover.
//!
//! The synchronous boundary is intended for bounded background workers. It
//! contains no provider SDK, networking, filesystem, or GPUI work.

mod error;
mod handoff;
mod model;

pub use error::ProviderHistoryError;
pub use handoff::{
    HandoffBatch, HandoffCoordinator, HandoffState, LiveAcceptance, SequencedHistory,
    VerifiedHistorySnapshot,
};
pub use model::{
    Continuation, DataClass, DatasetCapability, HistoryCapabilities, HistoryItem, HistoryPage,
    HistoryPageRequest, HistoryRange, PaginationStyle, ProviderHistoryAdapter, RateLimit,
};
