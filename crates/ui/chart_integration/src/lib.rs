//! Axiusflow host integration for Origin Charts' existing GPUI backend.
//!
//! Origin owns chart state, layout, scales, interactions, frames, and rendering.
//! This crate only negotiates GPUI window geometry and submits the resulting
//! immutable Origin frame to `origin_render_gpui`.

mod bridge;
mod coordinator;
mod host_benchmark;
mod origin_bridge;
mod provenance;
mod recovery_conformance;
mod view;

pub use bridge::{ChartBridgeMetrics, ChartDataBridge, MergedChartData, ReplayRecoveryCommand};
pub use coordinator::{
    ChartStreamCoordinator, ChartStreamCoordinatorError, ChartStreamCoordinatorMetrics,
    ChartStreamPollOutcome, ChartStreamRecoveryDispatch,
};
pub use host_benchmark::{
    OriginGpuiBenchmarkError, OriginGpuiHostSample, run_origin_gpui_host_sample,
};
pub use recovery_conformance::{ChartRecoveryConformance, run_chart_bridge_recovery_conformance};
pub use view::{
    ChartDrawingTool, ChartIndicator, ChartIndicatorError, DrawingsLockSummary, OriginChartView,
};
