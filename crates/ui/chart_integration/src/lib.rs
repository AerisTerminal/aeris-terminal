//! Axiusflow host integration for Origin Charts' existing GPUI backend.
//!
//! Origin owns chart state, layout, scales, interactions, frames, and rendering.
//! This crate only negotiates GPUI window geometry and submits the resulting
//! immutable Origin frame to `origin_render_gpui`.

mod bridge;
mod origin_bridge;
mod provenance;
mod view;

pub use axiusflow_application::ReplayRecoveryCommand;
pub use bridge::{ChartBridgeMetrics, ChartDataBridge, MergedChartData};
pub use view::{
    ChartDrawingTool, ChartIndicator, ChartIndicatorError, DrawingsLockSummary, OriginChartView,
};
