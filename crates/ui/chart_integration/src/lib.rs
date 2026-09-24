//! `Aeris` host integration for Nucleus Charts' existing GPUI backend.
//!
//! Nucleus owns chart state, layout, scales, interactions, frames, and rendering.
//! This crate only negotiates GPUI window geometry and submits the resulting
//! immutable Nucleus frame to `nucleuscharts_render_gpui`.

mod bridge;
mod nucleus_bridge;
mod provenance;
mod view;
mod workspace;

pub use aeris_application::ReplayRecoveryCommand;
pub use bridge::ChartBridgeMetrics;
pub use nucleuscharts_engine::ChartTheme as NucleusChartTheme;
pub use nucleuscharts_engine::{
    AlertCondition as ChartAlertCondition, AlertCreateRequest as ChartAlertCreateRequest,
    AlertFrequency as ChartAlertFrequency, AlertId as ChartAlertId, AlertLine as ChartAlertLine,
    AlertLineStatus as ChartAlertLineStatus, AlertPriceScale as ChartAlertPriceScale,
    AlertSnapshot as ChartAlertSnapshot,
};
pub use view::{
    ChartAppearanceSettings, ChartContextKind, ChartContextRequest, ChartDrawingTool,
    ChartIndicator, ChartIndicatorError, ChartIndicatorState, ChartStudyOutputDescriptor,
    ChartStudyOutputError, ChartStudyPaneTarget, ChartStudyPlotKind, ChartStudyPointStyle,
    ChartStudyScaleTarget, ChartStudyThresholdRegion, ChartType, DrawingsLockSummary,
    NucleusChartView, PriceAxisMenuAction, PriceAxisMenuState,
};
pub use workspace::{ChartSplitDirection, ChartWorkspaceLayout, NucleusWorkspace};
