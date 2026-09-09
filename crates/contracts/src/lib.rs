//! Shared in-process and persisted Axiusflow contracts.
//!
//! These prost DTOs carry bounded market/account metadata between ownership
//! boundaries in the single desktop process and encode the local workspace file.
//! No transport framing, peer protocol, socket, or process boundary lives here.

pub mod account;
mod messages;

pub use account::{
    AccountSessionState, AccountView, BeginLogin, CancelLogin, GetAccountStatus,
    LoginAuthorization, RefreshAccountProfile, SignOut,
};
pub use messages::{
    EngineFaultCode, FailureStage, Fault, InstallProviderInstrument, ProviderCatalogRejected,
    ProviderCatalogRejectionReason, ProviderConnectionState, ProviderInstrumentSearchResult,
    ProviderInstrumentSummary, ProviderState, SearchProviderInstruments, SelectProviderInstrument,
    SeriesCadence, SeriesKey, SeriesLoadState, WorkspaceLayoutState, WorkspacePaneKind,
    WorkspacePaneState, WorkspaceSplitAxis, WorkspaceState, WorkspaceTabState,
};
