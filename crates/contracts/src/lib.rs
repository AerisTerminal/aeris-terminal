//! Persisted `Aeris` workspace schema.
//!
//! These prost messages encode the local workspace file, including the
//! provider instrument and series identities a saved pane restores. Only
//! durable schema lives here; in-process market and account types belong to
//! their owning runtimes.

mod messages;

pub use messages::{
    InstallProviderInstrument, PriceAlertCondition, PriceAlertFrequency, PriceAlertStatus,
    ProviderContractMetadata, ProviderInstrumentClass, ProviderSessionHours, SeriesCadence,
    SeriesKey, WorkspaceBigTradesState, WorkspaceChartAppearanceState,
    WorkspaceChartIndicatorState, WorkspaceChartLegendState, WorkspaceChartPaneHeightState,
    WorkspaceChartSettingsTemplateState, WorkspaceChartState, WorkspaceChartStudyState,
    WorkspaceChromePoint, WorkspaceChromeState, WorkspaceCumulativeDeltaState,
    WorkspaceDeltaHistogramState, WorkspaceDrawingTemplateState, WorkspaceInstrumentDrawingsState,
    WorkspaceLayoutState, WorkspaceOrderBookDepthState, WorkspaceOrderFlowSettingsState,
    WorkspacePaneKind, WorkspacePaneState, WorkspacePriceAlertState, WorkspacePriceAxisState,
    WorkspaceSavedLayoutState, WorkspaceSplitAxis, WorkspaceState, WorkspaceStudyDecimalState,
    WorkspaceStudyDependencyKind, WorkspaceStudyDependencyState, WorkspaceStudyMarketStream,
    WorkspaceStudySettingState, WorkspaceTabState, WorkspaceVolumeSettingsState,
    WorkspaceWatchlistEntryState, workspace_study_setting_state,
};
