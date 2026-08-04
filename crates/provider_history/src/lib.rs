//! Provider-neutral history capabilities, scheduling, and live cutover.
//!
//! The synchronous boundary is intended for bounded background workers. It
//! contains no provider SDK, networking, filesystem, or GPUI work.

mod error;
mod handoff;
mod model;
mod page_validation;
mod rate_gate;
mod scheduler;
mod scheduling_model;

pub use error::ProviderHistoryError;
pub use handoff::{
    HandoffBatch, HandoffCoordinator, HandoffState, LiveAcceptance, SequencedHistory,
    VerifiedHistorySnapshot,
};
pub use model::{
    Continuation, DataClass, DatasetCapability, HistoryCapabilities, HistoryItem, HistoryPage,
    HistoryPageRequest, HistoryRange, PaginationStyle, ProviderHistoryAdapter, RateLimit,
};
pub use scheduler::HistoryScheduler;
pub use scheduling_model::{
    CancelOutcome, Completion, Dispatch, DispatchOutcome, ExpiredRequest, FetchFailureOutcome,
    RequestInterest, RequestPriority, SchedulerConfig, Submission,
};
