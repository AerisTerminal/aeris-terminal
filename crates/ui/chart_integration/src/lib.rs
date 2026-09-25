//! `Aeris` host integration for Aeris Charts' existing GPUI backend.
//!
//! Nucleus owns chart state, layout, scales, interactions, frames, and rendering.
//! This crate only negotiates GPUI window geometry and submits the resulting
//! immutable chart frame to `aeris_charts_render_gpui`.

mod bridge;
mod nucleus_bridge;
mod provenance;
mod view;
mod workspace;

pub use aeris_application::ReplayRecoveryCommand;
pub use aeris_charts_engine::ChartTheme as NucleusChartTheme;
pub use aeris_charts_engine::{
    AlertCondition as ChartAlertCondition, AlertCreateRequest as ChartAlertCreateRequest,
    AlertFrequency as ChartAlertFrequency, AlertId as ChartAlertId, AlertLine as ChartAlertLine,
    AlertLineStatus as ChartAlertLineStatus, AlertPriceScale as ChartAlertPriceScale,
    AlertSnapshot as ChartAlertSnapshot,
};
pub use bridge::ChartBridgeMetrics;
pub use view::{
    ChartAppearanceSettings, ChartContextKind, ChartContextRequest, ChartDrawingTool,
    ChartIndicator, ChartIndicatorError, ChartIndicatorState, ChartStudyOutputDescriptor,
    ChartStudyOutputError, ChartStudyPaneTarget, ChartStudyPlotKind, ChartStudyPointStyle,
    ChartStudyScaleTarget, ChartStudyThresholdRegion, ChartType, DrawingsLockSummary,
    NucleusChartView, PriceAxisMenuAction, PriceAxisMenuState,
};
pub use workspace::{ChartSplitDirection, ChartWorkspaceLayout, NucleusWorkspace};
