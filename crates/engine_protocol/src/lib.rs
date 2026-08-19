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
    ActivateExistingUi, AttachClient, ClientHello, ClientKind, ConsumerResourceClass, DemandError,
    DetachClient, EngineFaultCode, EngineLifetimeMode, EngineReady, EngineShutdownState,
    EngineStatus, Envelope, FailureStage, Fault, GetEngineStatus, Goodbye, HotSeries,
    InstallProviderInstrument, MarketBar, MarketEventIdle, OrderBookLevel, OrderBookSnapshot,
    OrderBookState, OrderFlowAggressor, OrderFlowLevel, OrderFlowSnapshot, OrderFlowTrade,
    OrderFlowUpdate, PersistenceState, PollMarketEvent, ProviderCatalogRejected,
    ProviderCatalogRejectionReason, ProviderConnectionState, ProviderInstrumentInstalled,
    ProviderInstrumentSearchResult, ProviderInstrumentSelection, ProviderInstrumentSummary,
    ProviderState, RegisterConsumer, RemoveConsumer, ResourceMode, RestoreWorkspace,
    SearchProviderInstruments, SelectProviderInstrument, SeriesCadence, SeriesDemand, SeriesKey,
    SeriesLoadState, SeriesSnapshot, SeriesState, SeriesUpdate, SetEngineLifecycle,
    SetEngineResourceMode, SetSelection, SetViewport, SetWatchlist, SetWorkspaceLayout,
    ShutdownEngine, ViewportDemand, VisibilityDemand, WorkspacePaneKind, WorkspacePaneState,
    WorkspaceSplitAxis, WorkspaceState, WorkspaceTabState, envelope,
};

/// Current compatible lifecycle contract revision advertised during readiness.
pub const LIFECYCLE_CONTRACT_REVISION: u32 = 1;

/// Protocol version carried by every envelope; mismatches are rejected at decode time.
pub const PROTOCOL_VERSION: u32 = 11;

/// Maximum prost payload accepted in one frame (1 MiB).
pub const MAX_FRAME_BYTES: usize = 1_048_576;

/// Maximum partial-frame storage retained by the decoder between pushes.
pub const MAX_BUFFERED_BYTES: usize = 2 * MAX_FRAME_BYTES;
