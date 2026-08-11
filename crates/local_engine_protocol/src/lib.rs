//! Versioned IPC protocol between the resident data-engine process and UI clients.
//!
//! Wire format: prost-encoded [`Envelope`] carried inside the bounded binary
//! framing from `axiusflow_transport`. Pure synchronous Rust: no async, no
//! GPUI, no network, no filesystem.

mod codec;
mod error;
mod messages;

pub use codec::{EnvelopeDecoder, encode_envelope};
pub use error::ProtocolError;
pub use messages::{
    ActivateExistingUi, AttachClient, ClientHello, ClientKind, DemandError, DetachClient,
    EngineFaultCode, EngineReady, Envelope, Fault, Goodbye, HotSeries, InstallProviderInstrument,
    MarketBar, MarketEventIdle, OrderBookLevel, OrderBookSnapshot, OrderBookState,
    PersistenceState, PollMarketEvent, ProviderConnectionState, ProviderInstrumentInstalled,
    ProviderState, RegisterConsumer, RemoveConsumer, ResourceMode, RestoreWorkspace, SeriesCadence,
    SeriesDemand, SeriesKey, SeriesLoadState, SeriesSnapshot, SeriesState, SeriesUpdate,
    SetEngineResourceMode, SetSelection, SetViewport, SetWatchlist, ShutdownEngine, ViewportDemand,
    VisibilityDemand, WorkspaceState, envelope,
};

/// Protocol version carried by every envelope; mismatches are rejected at decode time.
pub const PROTOCOL_VERSION: u32 = 9;

/// Maximum prost payload accepted in one frame (1 MiB).
pub const MAX_FRAME_BYTES: usize = 1_048_576;

/// Maximum partial-frame storage retained by the decoder between pushes.
pub const MAX_BUFFERED_BYTES: usize = 2 * MAX_FRAME_BYTES;
