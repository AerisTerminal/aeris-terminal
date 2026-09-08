//! Shared prost DTOs plus the bounded versioned workspace-persistence codec.
//!
//! Market and account runtimes use these DTOs directly in process. [`Envelope`]
//! remains only for backwards-compatible local workspace files; it is not a
//! runtime transport boundary.

pub mod account;
mod codec;
mod error;
mod messages;

pub use account::{
    AccountSessionState, AccountView, BeginLogin, CancelLogin, GetAccountStatus,
    LoginAuthorization, RefreshAccountProfile, SignOut,
};
pub use codec::{EnvelopeDecoder, encode_envelope};
pub use error::ProtocolError;
pub use messages::{
    EngineFaultCode, Envelope, FailureStage, Fault, InstallProviderInstrument, ProviderCatalogRejected,
    ProviderCatalogRejectionReason, ProviderConnectionState, ProviderInstrumentSearchResult,
    ProviderInstrumentSummary, ProviderState, SearchProviderInstruments, SelectProviderInstrument, SeriesCadence, SeriesKey,
    SeriesLoadState,
    WorkspaceLayoutState, WorkspacePaneKind, WorkspacePaneState, WorkspaceSplitAxis,
    WorkspaceState, WorkspaceTabState, envelope,
};

/// Existing workspace-file version. Kept at 21 so installed files written by
/// the former transport envelope remain readable; retired protobuf tags decode
/// as unknown fields.
pub const PROTOCOL_VERSION: u32 = 21;

/// Maximum prost payload accepted in one frame (3 MiB).
pub const MAX_FRAME_BYTES: usize = 3 * 1_048_576;

/// Maximum partial-frame storage retained by the decoder between pushes.
pub const MAX_BUFFERED_BYTES: usize = 2 * MAX_FRAME_BYTES;
