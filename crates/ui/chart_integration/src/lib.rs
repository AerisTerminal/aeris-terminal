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
    AlertSnapshot as ChartAlertSnapshot, ExecutionId as ChartExecutionId,
    ExecutionKind as ChartExecutionKind, ExecutionMarkerShape as ChartExecutionMarkerShape,
    HostEventMarker as ChartHostEventMarker, HostOverlaySnapshot as ChartHostOverlaySnapshot,
    HostTimeWindow as ChartHostTimeWindow, InstrumentMetadata as ChartInstrumentMetadata,
    OrderId as ChartOrderId, OrderKind as ChartOrderKind, OrderRole as ChartOrderRole,
    OrderSide as ChartOrderSide, OrderStatus as ChartOrderStatus, PositionId as ChartPositionId,
    PositionSide as ChartPositionSide, TradingAnnotation as ChartTradingAnnotation,
    TradingAnnotationTone as ChartTradingAnnotationTone, TradingExecution as ChartTradingExecution,
    TradingGroupId as ChartTradingGroupId, TradingIntent as ChartTradingIntent,
    TradingIntentAction as ChartTradingIntentAction, TradingPosition as ChartTradingPosition,
    TradingPriceScale as ChartTradingPriceScale, TradingSnapshot as ChartTradingSnapshot,
    WorkingOrder as ChartWorkingOrder,
};
pub use bridge::ChartBridgeMetrics;
pub use view::{
    ChartAppearanceSettings, ChartContextKind, ChartContextRequest, ChartDrawingTool,
    ChartIndicator, ChartIndicatorError, ChartIndicatorState, ChartStudyInputRequirements,
    ChartStudyInputStream, ChartStudyOutputDescriptor, ChartStudyOutputError, ChartStudyPaneTarget,
    ChartStudyPlotKind, ChartStudyPointStyle, ChartStudyScaleTarget, ChartStudyThresholdRegion,
    ChartType, DrawingsLockSummary, NucleusChartView, PriceAxisMenuAction, PriceAxisMenuState,
};
pub use workspace::{ChartSplitDirection, ChartWorkspaceLayout, NucleusWorkspace};
