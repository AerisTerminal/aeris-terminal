//! Worker-owned desktop history hydration and immutable chart publication.
//!
//! Blocking storage, provider response handling, and payload decoding are kept
//! behind [`HistoryWorker`]. UI consumers receive only shared immutable
//! [`HistoryPublication`] values from the bounded cache.

mod cache;
mod error;
mod model;
mod worker;

pub use error::DesktopHistoryError;
pub use model::{
    CacheSource, ChartId, ControlPlaneState, HistoryPublication, HydrationOutcome,
    HydrationRequest, ProviderConnectionState, StartupCacheState, WorkerMetrics,
};
pub use worker::{HistoryDecoder, HistoryWorker, HistoryWorkerConfig};
