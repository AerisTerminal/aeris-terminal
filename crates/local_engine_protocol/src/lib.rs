//! Versioned IPC protocol between the resident data-engine process and UI clients.
//!
//! Wire format: prost-encoded [`Envelope`] carried inside the bounded binary
//! framing from `axiusflow_transport`. Pure synchronous Rust: no async, no
//! GPUI, no network, no filesystem.

mod catalog;
mod codec;
mod error;
mod fence;
mod messages;

pub use catalog::{CATALOG_CHUNK_BUDGET_BYTES, CatalogReassembler, split_catalog};
pub use codec::{EnvelopeDecoder, encode_envelope};
pub use error::ProtocolError;
pub use fence::Fence;
pub use messages::{
    ActivateExistingUi, CatalogEntry, CatalogSnapshot, ChartDelta, ChartProvenance, ChartSnapshot,
    ClientHello, ClientKind, DomBookState, DomLevel, DomRecoveryReason, DomRow, DomSnapshot,
    EngineFaultCode, EngineHeartbeat, EngineReady, Envelope, Fault, Goodbye, HotSeries,
    ProviderConnectionState, ProviderState, ResourceMode, RestoreWorkspace, RithmicCatalog,
    RithmicChart, RithmicHistory, RithmicSearch, RithmicSelect, RithmicSymbol, SetSelection,
    SetViewport, SetWatchlist, SubscribeView, UnsubscribeView, ViewKind, WorkspaceState, envelope,
};

/// Protocol version carried by every envelope; mismatches are rejected at decode time.
pub const PROTOCOL_VERSION: u32 = 1;

/// Maximum prost payload accepted in one frame (1 MiB).
pub const MAX_FRAME_BYTES: usize = 1_048_576;

/// Maximum partial-frame storage retained by the decoder between pushes.
pub const MAX_BUFFERED_BYTES: usize = 2 * MAX_FRAME_BYTES;
