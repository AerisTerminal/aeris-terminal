//! `Aeris` host integration for Aeris Charts' existing GPUI backend.
//!
//! Aeris Charts owns chart state, layout, scales, interactions, frames, and rendering.
//! This crate only negotiates GPUI window geometry and submits the resulting
//! immutable chart frame to `aeris_charts_render_gpui`.

mod bridge;
mod engine_bridge;
mod provenance;
mod saved_drawings;
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
    IndicatorChromeOptions as ChartIndicatorChrome,
};
pub use aeris_charts_engine::{
    DrawingAnchorDescriptor as ChartDrawingAnchorDescriptor, DrawingId as ChartDrawingId,
    DrawingInterval as ChartDrawingInterval, DrawingIntervalUnit as ChartDrawingIntervalUnit,
    DrawingIntervalVisibility as ChartDrawingIntervalVisibility,
    DrawingLabelMetric as ChartDrawingLabelMetric, DrawingLabelOptions as ChartDrawingLabelOptions,
    DrawingLevel as ChartDrawingLevel, DrawingPropertyDescriptor as ChartDrawingProperty,
    DrawingPropertyGroup as ChartDrawingPropertyGroup,
    DrawingPropertySchema as ChartDrawingPropertySchema,
    DrawingPropertyType as ChartDrawingPropertyType, MAX_DRAWING_LEVELS, MAX_DRAWING_NAME_BYTES,
    MAX_DRAWING_TEMPLATE_BYTES, MAX_DRAWING_TEMPLATES,
};
/// The keys a focused chart consumes; desktop shortcuts must stay off them.
pub use aeris_charts_render_gpui::input::chart_keystrokes;
pub use bridge::ChartBridgeMetrics;
pub use saved_drawings::saved_drawings_document;
pub use view::{
    AerisChartView, BigTradesFilter, BigTradesIntensity, BigTradesSettings, BigTradesSize,
    ChartAppearanceSettings, ChartContextKind, ChartContextRequest, ChartDrawingAnchorError,
    ChartDrawingAnchorText, ChartDrawingKind, ChartDrawingSettings, ChartDrawingStamp,
    ChartIndicator, ChartIndicatorError, ChartIndicatorState, ChartLegendOptions,
    ChartSettingsRequest, ChartStudyInputRequirements, ChartStudyInputStream,
    ChartStudyOutputDescriptor, ChartStudyOutputError, ChartStudyPaneTarget, ChartStudyPlotKind,
    ChartStudyPointStyle, ChartStudyScaleTarget, ChartStudyThresholdRegion, ChartType,
    DrawingsLockSummary, FootprintDisplayMode, OrderFlowAggregation, OrderFlowSettings,
    OrderFlowSweep, OrderFlowTrade, PriceAxisMenuAction, PriceAxisMenuState, VolumeAverageKind,
    VolumeAverageSettings, VolumePlacement, VolumeSettings, classify_order_flow_sweeps,
    drawing_color_hex,
};
pub use view::{DEFAULT_STUDY_LINE_WIDTH, MAXIMUM_STUDY_LINE_WIDTH};
pub use workspace::{AerisChartWorkspace, ChartSplitDirection, ChartWorkspaceLayout};
