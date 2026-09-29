//! `Aeris` host integration for Aeris Charts' existing GPUI backend.
//!
//! Aeris Charts owns chart state, layout, scales, interactions, frames, and rendering.
//! This crate only negotiates GPUI window geometry and submits the resulting
//! immutable chart frame to `aeris_charts_render_gpui`.

mod bridge;
mod engine_bridge;
mod provenance;
mod view;
mod workspace;

pub use aeris_application::ReplayRecoveryCommand;
pub use aeris_charts_engine::ChartTheme as AerisChartTheme;
pub use aeris_charts_engine::{
    AggressorSide as ChartAggressorSide, AlertCondition as ChartAlertCondition,
    AlertCreateRequest as ChartAlertCreateRequest, AlertFrequency as ChartAlertFrequency,
    AlertId as ChartAlertId, AlertLine as ChartAlertLine, AlertLineStatus as ChartAlertLineStatus,
    AlertPriceScale as ChartAlertPriceScale, AlertSnapshot as ChartAlertSnapshot, ChartSyncEvent,
    ChartSyncEventKind, CrosshairSyncPosition, DEFAULT_TIME_ZONE, ExecutionId as ChartExecutionId,
    ExecutionKind as ChartExecutionKind, ExecutionMarkerShape as ChartExecutionMarkerShape,
    HostEventMarker as ChartHostEventMarker, HostOverlaySnapshot as ChartHostOverlaySnapshot,
    HostTimeWindow as ChartHostTimeWindow, InstrumentMetadata as ChartInstrumentMetadata,
    OrderId as ChartOrderId, OrderKind as ChartOrderKind, OrderRole as ChartOrderRole,
    OrderSide as ChartOrderSide, OrderStatus as ChartOrderStatus, PositionId as ChartPositionId,
    PositionSide as ChartPositionSide, TRADINGVIEW_TIME_ZONES,
    TradingAnnotation as ChartTradingAnnotation,
    TradingAnnotationPlacement as ChartTradingAnnotationPlacement,
    TradingAnnotationTone as ChartTradingAnnotationTone, TradingExecution as ChartTradingExecution,
    TradingGroupId as ChartTradingGroupId, TradingIntent as ChartTradingIntent,
    TradingIntentAction as ChartTradingIntentAction, TradingPosition as ChartTradingPosition,
    TradingPriceScale as ChartTradingPriceScale, TradingSnapshot as ChartTradingSnapshot,
    VisibleTimeRangeSync, WorkingOrder as ChartWorkingOrder,
};
pub use aeris_charts_engine::{
    AppearanceColor as ChartAppearanceColor, FinancialThemeColors as ChartThemeColors,
};
pub use bridge::ChartBridgeMetrics;
pub use view::{
    AerisChartView, ChartAppearanceSettings, ChartContextKind, ChartContextRequest,
    ChartDrawingTool, ChartIndicator, ChartIndicatorError, ChartIndicatorState,
    ChartStudyInputRequirements, ChartStudyInputStream, ChartStudyOutputDescriptor,
    ChartStudyOutputError, ChartStudyPaneTarget, ChartStudyPlotKind, ChartStudyPointStyle,
    ChartStudyScaleTarget, ChartStudyThresholdRegion, ChartType, DrawingsLockSummary,
    FootprintDisplayMode, OrderFlowAggregation, OrderFlowSettings, OrderFlowSweep, OrderFlowTrade,
    PriceAxisMenuAction, PriceAxisMenuState, classify_order_flow_sweeps,
};
pub use view::{DEFAULT_STUDY_LINE_WIDTH, MAXIMUM_STUDY_LINE_WIDTH};
pub use workspace::{AerisChartWorkspace, ChartSplitDirection, ChartWorkspaceLayout};
