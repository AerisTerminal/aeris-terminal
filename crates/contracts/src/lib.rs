//! Shared in-process and persisted `Aeris` contracts.
//!
//! These prost DTOs carry bounded market/account metadata between ownership
//! boundaries in the single desktop process and encode the local workspace file.
//! No transport framing, peer protocol, socket, or process boundary lives here.

pub mod account;
mod messages;
pub mod provider;

pub use account::{
    AccountSessionState, AccountView, BeginLogin, CancelLogin, GetAccountStatus,
    LoginAuthorization, RefreshAccountProfile, SignOut,
};
pub use messages::{
    EngineFaultCode, FailureStage, Fault, InstallProviderInstrument, InstrumentSearchCategories,
    MarketSessionPhase, MarketSessionSource, MarketSessionStatus, PriceAlertCondition,
    PriceAlertFrequency, PriceAlertStatus, ProviderCatalogRejected, ProviderCatalogRejectionReason,
    ProviderConnectionState, ProviderContractMetadata, ProviderInstrumentClass,
    ProviderInstrumentKind, ProviderInstrumentSearchResult, ProviderInstrumentSummary,
    ProviderMarketScreen, ProviderMarketStatistics, ProviderSessionHours, ProviderState,
    ScreenProviderMarkets, SearchProviderInstruments, SelectProviderInstrument, SeriesCadence,
    SeriesKey, SeriesLoadState, WorkspaceBigTradesState, WorkspaceChartAppearanceState,
    WorkspaceChartIndicatorState, WorkspaceChartLegendState, WorkspaceChartSettingsTemplateState,
    WorkspaceChartState, WorkspaceChartStudyState, WorkspaceCumulativeDeltaState,
    WorkspaceDeltaHistogramState, WorkspaceDrawingTemplateState, WorkspaceInstrumentDrawingsState,
    WorkspaceLayoutState, WorkspaceOrderBookDepthState, WorkspaceOrderFlowSettingsState,
    WorkspacePaneKind, WorkspacePaneState, WorkspacePriceAlertState, WorkspacePriceAxisState,
    WorkspaceSplitAxis, WorkspaceState, WorkspaceStudyDecimalState, WorkspaceStudyDependencyKind,
    WorkspaceStudyDependencyState, WorkspaceStudyMarketStream, WorkspaceStudySettingState,
    WorkspaceTabState, WorkspaceVolumeSettingsState, WorkspaceWatchlistEntryState,
    workspace_study_setting_state,
};
pub use provider::{
    ProviderCatalogSymbol, ProviderConnectionKind, ProviderDailyBarCadence,
    ProviderPresentationDescriptor,
};
