//! Versioned IPC protocol between the resident data-engine process and UI clients.
//!
//! Wire format: prost-encoded [`Envelope`] carried inside the bounded binary
//! framing from `axiusflow_transport`. Pure synchronous Rust: no async, no
//! GPUI, no network, no filesystem.

pub mod account;
mod codec;
mod error;
mod messages;

pub use account::{
    AccountSessionState, AccountView, BeginLogin, CancelLogin, GetAccountStatus,
    LoginAuthorization, SignOut,
};
pub use codec::{EnvelopeDecoder, encode_envelope};
pub use error::ProtocolError;
pub use messages::{
    AttachClient, ClientHello, ClientKind, ConsumerResourceClass, DemandError, DetachClient,
    EngineFaultCode, EngineLifetimeMode, EngineReady, EngineShutdownState, EngineStatus, Envelope,
    FailureStage, Fault, GetEngineStatus, Goodbye, HotSeries, InstallProviderInstrument, MarketBar,
    OrderBookLevel, OrderBookSnapshot, OrderBookState, OrderFlowAggressor, OrderFlowLevel,
    OrderFlowSnapshot, OrderFlowTrade, OrderFlowUpdate, PersistenceState, ProviderCatalogRejected,
    ProviderCatalogRejectionReason, ProviderConnectionState, ProviderInstrumentInstalled,
    ProviderInstrumentSearchResult, ProviderInstrumentSelection, ProviderInstrumentSummary,
    ProviderState, RegisterConsumer, RemoveConsumer, ResourceMode, RestoreWorkspace,
    SearchProviderInstruments, SelectProviderInstrument, SeriesCadence, SeriesDemand, SeriesKey,
    SeriesLoadState, SeriesSnapshot, SeriesState, SeriesUpdate, SeriesUpdateOperation,
    SetEngineLifecycle, SetEngineResourceMode, SetSelection, SetViewport, SetWatchlist,
    SetWorkspaceLayout, ShutdownEngine, StreamRole, ViewportDemand, VisibilityDemand,
    WorkspaceLayoutState, WorkspacePaneKind, WorkspacePaneState, WorkspaceSplitAxis,
    WorkspaceState, WorkspaceTabState, envelope,
};

/// Current compatible lifecycle contract revision advertised during readiness.
pub const LIFECYCLE_CONTRACT_REVISION: u32 = 1;

/// Protocol version carried by every envelope; mismatches are rejected at decode time.
///
/// Revision 16 adds the paired-session `session_nonce` and `stream_role` to
/// `ClientHello`: every session owns a write-only command stream and a
/// read-only event stream so blocking reads never share a transport handle
/// with writes.
///
/// Revision 17 removes the dormant single-instance `ActivateExistingUi`
/// command (tag 16, retired permanently): the audit found no producer and no
/// consumer anywhere in the workspace. Second-desktop behavior stays exactly
/// as implemented: each desktop attaches as an isolated authenticated client,
/// and lifecycle transactions block new processes through the update lock.
///
/// Revision 18 adds the phase 5 account boundary (tags 55-60): `BeginLogin`,
/// `CancelLogin`, `GetAccountStatus`, `LoginAuthorization`, the sanitized
/// `AccountView`, and `SignOut`. The engine owns the PKCE transaction and
/// vault material; the desktop receives only the browser URL and the
/// sanitized view.
///
/// Revision 19 adds the selection-command generation to
/// `ProviderInstrumentSelection`. Provider-wide instrument generations remain
/// independent fencing identities, while the echoed command generation lets a
/// desktop correlate a completion to the exact symbol-picker request.
pub const PROTOCOL_VERSION: u32 = 19;

/// Maximum prost payload accepted in one frame (3 MiB).
pub const MAX_FRAME_BYTES: usize = 3 * 1_048_576;

/// Maximum partial-frame storage retained by the decoder between pushes.
pub const MAX_BUFFERED_BYTES: usize = 2 * MAX_FRAME_BYTES;
