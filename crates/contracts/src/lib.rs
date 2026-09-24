//! Shared in-process and persisted `Aeris` contracts.
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
    EngineFaultCode, FailureStage, Fault, InstallProviderInstrument, PriceAlertCondition,
    PriceAlertFrequency, PriceAlertStatus, ProviderCatalogRejected, ProviderCatalogRejectionReason,
    ProviderConnectionState, ProviderInstrumentSearchResult, ProviderInstrumentSummary,
    ProviderState, SearchProviderInstruments, SelectProviderInstrument, SeriesCadence, SeriesKey,
    SeriesLoadState, WorkspaceChartAppearanceState, WorkspaceChartIndicatorState,
    WorkspaceChartSettingsTemplateState, WorkspaceChartState, WorkspaceChartStudyState,
    WorkspaceLayoutState, WorkspacePaneKind, WorkspacePaneState, WorkspacePriceAlertState,
    WorkspacePriceAxisState, WorkspaceSplitAxis, WorkspaceState, WorkspaceStudyDecimalState,
    WorkspaceStudyDependencyKind, WorkspaceStudyDependencyState, WorkspaceStudyMarketStream,
    WorkspaceStudySettingState, WorkspaceTabState, WorkspaceWatchlistEntryState,
    workspace_study_setting_state,
};
