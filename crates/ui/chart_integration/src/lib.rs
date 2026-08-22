//! Axiusflow host integration for Nucleus Charts' existing GPUI backend.
//!
//! Nucleus owns chart state, layout, scales, interactions, frames, and rendering.
//! This crate only negotiates GPUI window geometry and submits the resulting
//! immutable Nucleus frame to `nucleuscharts_render_gpui`.

mod bridge;
mod nucleus_bridge;
mod provenance;
mod view;
mod workspace;

pub use axiusflow_application::ReplayRecoveryCommand;
pub use bridge::ChartBridgeMetrics;
pub use nucleuscharts_engine::ChartTheme as NucleusChartTheme;
pub use view::{
    ChartContextKind, ChartContextRequest, ChartDrawingTool, ChartIndicator, ChartIndicatorError,
    DrawingsLockSummary, NucleusChartView, PriceAxisMenuAction, PriceAxisMenuState,
};
pub use workspace::{ChartSplitDirection, ChartWorkspaceLayout, NucleusWorkspace};
