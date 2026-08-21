//! Engine-owned, provider-neutral local history mechanics.
//!
//! This crate owns authenticated immutable segment reads, writes, decoding, and
//! validation. Provider repair, canonical in-memory caching, generations, and
//! history/live handoff remain owned by the resident market engine.

mod error;
mod store;

pub use axiusflow_local_storage::{HistoryScope, RetainedRange};
pub use error::LocalHistoryError;
pub use store::{LocalHistoryStore, StoredHistory};
