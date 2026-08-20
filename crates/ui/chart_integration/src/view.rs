//! GPUI entity hosting one authoritative Origin chart engine and renderer.

use crate::bridge::{ChartBridgeMetrics, ChartDataBridge};
use crate::origin_bridge::{
    apply_merged_chart_data, chart_data_queue_capacity, install_replay, install_volume_series,
    replay_price_divisor,
};
use crate::provenance::{DEFAULT_CHART_SERIES_MAX_POINTS, DisplayedProvenance};
use axiusflow_application::ReplayRecoveryCommand;
use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketEventProvenance, ReplaySnapshot,
    ReplayStreamUpdate, ReplayValidationError,
};
use axiusflow_design_system::AxiusflowTheme;
use gpui::{
    App, Bounds, Context, CursorStyle, Entity, FocusHandle, KeyDownEvent, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render, ScrollWheelEvent, Window, canvas, div,
    prelude::*, px,
};
use num_traits::ToPrimitive;
use origin_engine::{
    ChartEngine, ChartFrame, DrawingId, DrawingKind, DrawingModifiers, PriceScaleTarget,
};
use origin_render::draw_list::Prim;
use origin_render_gpui::backend::measure_text;
use origin_render_gpui::{GpuiChartRenderer, OriginViewport, PreparedOriginFrame};
use std::collections::HashSet;
use std::fmt;
#[cfg(feature = "diagnostics")]
use std::time::Instant;

const SCALE_FACTOR_EPSILON: f32 = 1.0e-4;
const WHEEL_LINE_HEIGHT: f32 = 32.0;
const KEYBOARD_PAGE_FRACTION: f64 = 0.8;
const PANE_SEPARATOR_HIT: f64 = 4.0;

/// A native indicator supported by the chart's current OHLCV data bridge.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChartIndicator {
    Volume,
    Vwap,
    Sma,
    Ema,
    Wma,
    Bollinger,
    Rsi,
    Macd,
    Stochastic,
    Atr,
}

impl ChartIndicator {
    /// All indicators that can be calculated truthfully from the installed OHLC columns.
    pub const ALL: [Self; 10] = [
        Self::Volume,
        Self::Vwap,
        Self::Sma,
        Self::Ema,
        Self::Wma,
        Self::Bollinger,
        Self::Rsi,
        Self::Macd,
        Self::Stochastic,
        Self::Atr,
    ];

    /// Returns the user-facing legacy catalog label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Volume => "Volume",
            Self::Vwap => "Volume Weighted Average Price",
            Self::Sma => "Moving Average",
            Self::Ema => "Moving Average Exponential",
            Self::Wma => "Weighted Moving Average",
            Self::Bollinger => "Bollinger Bands",
            Self::Rsi => "Relative Strength Index",
            Self::Macd => "MACD",
            Self::Stochastic => "Stochastic",
            Self::Atr => "Average True Range",
        }
    }

    /// Returns the fixed parameters shown by the legacy indicator catalog.
    #[must_use]
    pub const fn parameters(self) -> &'static str {
        match self {
            Self::Volume => "Up/down volume",
            Self::Vwap => "Session anchored",
            Self::Sma | Self::Ema | Self::Wma => "Period 20",
            Self::Bollinger => "Period 20 · Deviation 2",
            Self::Rsi | Self::Atr => "Period 14",
            Self::Macd => "Fast 12 · Slow 26 · Signal 9",
            Self::Stochastic => "%K 14 · %D 3",
        }
    }
}

/// Failure to create a native indicator on the chart's primary series.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChartIndicatorError {
    MarketDataUnavailable,
    CreationRejected(ChartIndicator),
}

impl fmt::Display for ChartIndicatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MarketDataUnavailable => {
                formatter.write_str("an indicator requires installed chart market data")
            }
            Self::CreationRejected(indicator) => {
                write!(
                    formatter,
                    "Origin rejected the {} indicator",
                    indicator.label()
                )
            }
        }
    }
}

impl std::error::Error for ChartIndicatorError {}

/// A drawing tool exposed by the native chart surface.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ChartDrawingTool {
    /// Selects, moves, and pans without creating a drawing.
    #[default]
    Cursor,
    TrendLine,
    HorizontalLine,
    VerticalLine,
    Ray,
    Rectangle,
    Brush,
    Text,
}

impl ChartDrawingTool {
    const fn drawing_kind(self) -> Option<DrawingKind> {
        match self {
            Self::Cursor => None,
            Self::TrendLine => Some(DrawingKind::TrendLine),
            Self::HorizontalLine => Some(DrawingKind::HorizontalLine),
            Self::VerticalLine => Some(DrawingKind::VerticalLine),
            Self::Ray => Some(DrawingKind::HorizontalRay),
            Self::Rectangle => Some(DrawingKind::Rectangle),
            Self::Brush => Some(DrawingKind::Brush),
            Self::Text => Some(DrawingKind::Text),
        }
    }
}

/// Aggregate state used by drawing-toolbar lock controls.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrawingsLockSummary {
    pub total: usize,
    pub locked_count: usize,
    pub all_locked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ChartDrag {
    Pane,
    TimeAxis,
    PriceAxis {
        pane: usize,
        target: PriceScaleTarget,
    },
    PaneSeparator {
        index: usize,
        last_y: f64,
    },
}

/// A GPUI entity hosting one authoritative Origin chart engine and renderer.
pub struct OriginChartView {
    engine: ChartEngine,
    renderer: GpuiChartRenderer,
    data_bridge: Option<ChartDataBridge>,
    displayed_provenance: DisplayedProvenance,
    price_divisor: f64,
    volume_series: usize,
    frame: ChartFrame,
    axis_prims: Vec<Prim>,
    built_for: (f32, f32, f32),
    fitted: bool,
    viewport_origin: (f32, f32),
    drag: Option<ChartDrag>,
    drawing_tool: ChartDrawingTool,
    locked_drawings: HashSet<DrawingId>,
    focus_handle: Option<FocusHandle>,
    cursor_style: CursorStyle,
    #[cfg(feature = "diagnostics")]
    last_snapshot_installation_nanos: Option<u64>,
}

fn apply_platform_theme(
    engine: &mut ChartEngine,
    theme: &AxiusflowTheme,
) -> Result<(), serde_json::Error> {
    let colors = theme.colors;
    let surface = colors.background.css_value();
    let border = colors.border.css_value();
    let axis_text = colors.foreground.css_value();
    let crosshair = colors.muted_foreground.css_value();
    let separator_hover = colors.accent.css_value();
    let positive = colors.positive.css_value();
    let negative = colors.negative.css_value();
    for series in &mut engine.series {
        series.up_color = Some(positive.clone());
        series.down_color = Some(negative.clone());
        series.wick_up_color = Some(positive.clone());
        series.wick_down_color = Some(negative.clone());
        series.border_up_color = Some(positive.clone());
        series.border_down_color = Some(negative.clone());
    }
    let patch = serde_json::json!({
        "layout": {
            "fontFamily": "Inter",
            "background": {
                "type": "solid",
                "color": surface,
                "topColor": surface,
                "bottomColor": surface
            },
            "textColor": axis_text,
            "panes": {
                "separatorColor": border,
                "separatorHoverColor": separator_hover
            }
        },
        "grid": {
            "vertLines": { "color": border },
            "horzLines": { "color": border }
        },
        "crosshair": {
            "vertLine": { "color": crosshair, "labelBackgroundColor": crosshair },
            "horzLine": { "color": crosshair, "labelBackgroundColor": crosshair }
        },
        "leftPriceScale": { "borderColor": border, "textColor": axis_text },
        "rightPriceScale": { "borderColor": border, "textColor": axis_text },
        "timeScale": { "borderColor": border }
    });
    engine.apply_options(&patch.to_string())
}

impl OriginChartView {
    /// Creates an empty Origin-owned surface without inventing market data.
    #[must_use]
    pub fn empty() -> Self {
        Self::empty_with_theme(&AxiusflowTheme::dark())
    }

    /// Creates an empty chart with the supplied platform theme applied atomically.
    #[must_use]
    pub fn empty_with_theme(theme: &AxiusflowTheme) -> Self {
        let mut engine = ChartEngine::new(1024.0, 640.0, 1.0);
        let volume_series = install_volume_series(&mut engine);
        let retention_applied =
            engine.set_series_max_points(0, Some(DEFAULT_CHART_SERIES_MAX_POINTS));
        debug_assert!(retention_applied);
        let volume_retention_applied =
            engine.set_series_max_points(volume_series, Some(DEFAULT_CHART_SERIES_MAX_POINTS));
        debug_assert!(volume_retention_applied);
        let theme_applied = apply_platform_theme(&mut engine, theme).is_ok();
        debug_assert!(theme_applied);
        Self {
            engine,
            renderer: GpuiChartRenderer::new(),
            data_bridge: None,
            displayed_provenance: DisplayedProvenance::empty(),
            price_divisor: 1.0,
            volume_series,
            frame: ChartFrame::default(),
            axis_prims: Vec::new(),
            built_for: (0.0, 0.0, 0.0),
            fitted: false,
            viewport_origin: (0.0, 0.0),
            drag: None,
            drawing_tool: ChartDrawingTool::Cursor,
            locked_drawings: HashSet::new(),
            focus_handle: None,
            cursor_style: CursorStyle::Crosshair,
            #[cfg(feature = "diagnostics")]
            last_snapshot_installation_nanos: None,
        }
    }

    /// Creates a chart from the bounded embedded replay using Origin's own styling.
    ///
    /// # Panics
    ///
    /// Panics only if the application-owned embedded fixture violates its own
    /// validation contract.
    #[must_use]
    pub fn new() -> Self {
        let replay = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 600 })
            .expect("the embedded replay is validated application data");
        Self::with_replay(&replay)
    }

    /// Creates a chart from one validated application replay snapshot.
    ///
    /// A snapshot that cannot establish resumable sequence state installs
    /// without a live bridge instead of panicking the UI thread.
    #[must_use]
    pub fn with_replay(replay: &ReplaySnapshot) -> Self {
        Self::with_replay_and_theme(replay, &AxiusflowTheme::dark())
    }

    /// Creates a replay-backed chart with the supplied platform theme applied atomically.
    #[must_use]
    pub fn with_replay_and_theme(replay: &ReplaySnapshot, theme: &AxiusflowTheme) -> Self {
        let mut engine = ChartEngine::new(1024.0, 640.0, 1.0);
        let volume_series = install_volume_series(&mut engine);
        install_replay(&mut engine, volume_series, replay);
        let retention_applied =
            engine.set_series_max_points(0, Some(DEFAULT_CHART_SERIES_MAX_POINTS));
        debug_assert!(retention_applied);
        let volume_retention_applied =
            engine.set_series_max_points(volume_series, Some(DEFAULT_CHART_SERIES_MAX_POINTS));
        debug_assert!(volume_retention_applied);
        let data_bridge = ChartDataBridge::try_new(chart_data_queue_capacity(), replay).ok();
        debug_assert!(data_bridge.is_some());
        let theme_applied = apply_platform_theme(&mut engine, theme).is_ok();
        debug_assert!(theme_applied);

        Self {
            engine,
            renderer: GpuiChartRenderer::new(),
            data_bridge,
            displayed_provenance: DisplayedProvenance::from_snapshot(replay),
            price_divisor: replay_price_divisor(replay),
            volume_series,
            frame: ChartFrame::default(),
            axis_prims: Vec::new(),
            built_for: (0.0, 0.0, 0.0),
            fitted: false,
            viewport_origin: (0.0, 0.0),
            drag: None,
            drawing_tool: ChartDrawingTool::Cursor,
            locked_drawings: HashSet::new(),
            focus_handle: None,
            cursor_style: CursorStyle::Crosshair,
            #[cfg(feature = "diagnostics")]
            last_snapshot_installation_nanos: None,
        }
    }

    /// Replaces Origin's authoritative series data with one validated snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot establish resumable sequence state.
    pub fn load_replay(&mut self, replay: &ReplaySnapshot) -> Result<(), ReplayValidationError> {
        #[cfg(feature = "diagnostics")]
        let snapshot_install_started = Instant::now();
        if let Some(bridge) = &mut self.data_bridge {
            bridge.install_snapshot(replay)?;
        } else {
            self.data_bridge = Some(ChartDataBridge::try_new(
                chart_data_queue_capacity(),
                replay,
            )?);
        }
        install_replay(&mut self.engine, self.volume_series, replay);
        self.displayed_provenance.replace_snapshot(replay);
        self.price_divisor = replay_price_divisor(replay);
        self.invalidate_series_frame();
        self.fitted = false;
        #[cfg(feature = "diagnostics")]
        {
            self.last_snapshot_installation_nanos = Some(
                u64::try_from(snapshot_install_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            );
        }
        Ok(())
    }

    /// Restores the complete visible time range and automatic price scales.
    pub fn reset_view(&mut self) {
        self.engine.reset_time_scale();
        self.engine.fit_content();
        for pane in 0..self.engine.panes.len() {
            self.engine
                .set_price_scale_auto_scale_for(pane, PriceScaleTarget::Left, true);
            self.engine
                .set_price_scale_auto_scale_for(pane, PriceScaleTarget::Right, true);
        }
        self.invalidate_series_frame();
        self.fitted = true;
    }

    /// Applies the platform's resolved neutral palette without changing chart data or viewport.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if Origin rejects the generated options patch.
    pub fn set_platform_theme(&mut self, theme: &AxiusflowTheme) -> Result<(), serde_json::Error> {
        apply_platform_theme(&mut self.engine, theme)?;
        self.invalidate_series_frame();
        Ok(())
    }

    /// Returns the time scale to the newest bar without changing its zoom.
    pub fn scroll_to_latest(&mut self) {
        self.engine.scroll_to_real_time();
        self.invalidate_series_frame();
    }

    /// Returns the settled visible time range in Unix nanoseconds.
    #[must_use]
    pub fn visible_time_range_unix_nanos(&self) -> Option<(i64, i64)> {
        let (start, end) = self.engine.visible_time_range()?;
        let start = (start * 1_000_000_000.0).round().to_i64()?;
        let end = (end * 1_000_000_000.0).round().to_i64()?;
        (start < end).then_some((start, end))
    }

    /// Restores a persisted visible time range without replacing chart data.
    pub fn set_visible_time_range_unix_nanos(&mut self, start: i64, end: i64) -> bool {
        if start >= end {
            return false;
        }
        self.engine.set_visible_time_range(
            start.to_f64().unwrap_or(0.0) / 1_000_000_000.0,
            end.to_f64().unwrap_or(0.0) / 1_000_000_000.0,
        );
        self.invalidate_series_frame();
        self.fitted = true;
        true
    }

    /// Adds an indicator with the defaults shown by the legacy native catalog.
    ///
    /// # Errors
    ///
    /// Returns an error when no market snapshot has populated the primary series or Origin
    /// cannot create every output required by the selected indicator.
    pub fn add_indicator(
        &mut self,
        indicator: ChartIndicator,
    ) -> Result<Vec<usize>, ChartIndicatorError> {
        if !self.has_market_data() {
            return Err(ChartIndicatorError::MarketDataUnavailable);
        }
        let ids = match indicator {
            ChartIndicator::Volume => {
                self.engine.series[self.volume_series].visible = true;
                vec![self.volume_series]
            }
            ChartIndicator::Vwap => self
                .engine
                .add_vwap(0, Some(self.volume_series))
                .into_iter()
                .collect(),
            ChartIndicator::Sma => self.engine.add_sma(0, 20).into_iter().collect(),
            ChartIndicator::Ema => self.engine.add_ema(0, 20).into_iter().collect(),
            ChartIndicator::Wma => self.engine.add_wma(0, 20).into_iter().collect(),
            ChartIndicator::Bollinger => self.engine.add_bollinger(0, 20, 2.0),
            ChartIndicator::Rsi => self.engine.add_rsi(0, 14).into_iter().collect(),
            ChartIndicator::Macd => self.engine.add_macd(0, 12, 26, 9),
            ChartIndicator::Stochastic => self.engine.add_stochastic(0, 14, 3),
            ChartIndicator::Atr => self.engine.add_atr(0, 14).into_iter().collect(),
        };
        let expected_outputs = match indicator {
            ChartIndicator::Bollinger | ChartIndicator::Macd => 3,
            ChartIndicator::Stochastic => 2,
            ChartIndicator::Volume
            | ChartIndicator::Vwap
            | ChartIndicator::Sma
            | ChartIndicator::Ema
            | ChartIndicator::Wma
            | ChartIndicator::Rsi
            | ChartIndicator::Atr => 1,
        };
        if ids.len() != expected_outputs {
            for &id in &ids {
                self.engine.remove_series(id);
            }
            return Err(ChartIndicatorError::CreationRejected(indicator));
        }
        self.invalidate_series_frame();
        Ok(ids)
    }

    /// Returns the drawing tool currently armed on the chart surface.
    #[must_use]
    pub const fn drawing_tool(&self) -> ChartDrawingTool {
        self.drawing_tool
    }

    /// Arms a drawing tool, replacing any unfinished drawing gesture.
    pub fn set_drawing_tool(&mut self, tool: ChartDrawingTool) {
        self.end_drag(-1.0, -1.0);
        self.cancel_drawing_gesture();
        self.drawing_tool = tool;
        if let Some(kind) = tool.drawing_kind()
            && kind != DrawingKind::Brush
        {
            let armed = self.engine.drawing_create_begin(kind, None);
            debug_assert!(armed, "an empty drawing-options template is valid");
        }
        self.cursor_style = CursorStyle::Crosshair;
        self.invalidate_series_frame();
    }

    /// Cancels creation or movement and returns to the cursor tool.
    pub fn cancel_drawing(&mut self) {
        self.end_drag(-1.0, -1.0);
        self.cancel_drawing_gesture();
        self.drawing_tool = ChartDrawingTool::Cursor;
        self.cursor_style = CursorStyle::Crosshair;
        self.invalidate_series_frame();
    }

    /// Returns the number of committed drawings.
    #[must_use]
    pub fn drawing_count(&self) -> usize {
        self.engine.drawings().len()
    }

    /// Returns the selected drawing identifier, if any.
    #[must_use]
    pub fn selected_drawing_id(&self) -> Option<DrawingId> {
        self.engine.selected_drawing()
    }

    /// Returns whether the selected drawing is locked against pointer movement.
    #[must_use]
    pub fn selected_drawing_locked(&self) -> bool {
        self.engine
            .selected_drawing()
            .is_some_and(|id| self.locked_drawings.contains(&id))
    }

    /// Locks or unlocks the selected drawing against pointer movement.
    pub fn set_selected_drawing_locked(&mut self, locked: bool) -> bool {
        let Some(id) = self.engine.selected_drawing() else {
            return false;
        };
        let changed = if locked {
            self.locked_drawings.insert(id)
        } else {
            self.locked_drawings.remove(&id)
        };
        if changed {
            self.engine.drawing_drag_end();
            self.invalidate_series_frame();
        }
        changed
    }

    /// Returns aggregate lock state for the drawing toolbar.
    #[must_use]
    pub fn drawings_lock_summary(&self) -> DrawingsLockSummary {
        let total = self.engine.drawings().len();
        let locked_count = self
            .engine
            .drawings()
            .iter()
            .filter(|drawing| self.locked_drawings.contains(&drawing.id))
            .count();
        DrawingsLockSummary {
            total,
            locked_count,
            all_locked: total > 0 && total == locked_count,
        }
    }

    /// Locks or unlocks every committed drawing against pointer movement.
    pub fn set_all_drawings_locked(&mut self, locked: bool) -> bool {
        let previous = self.drawings_lock_summary();
        if locked {
            self.locked_drawings
                .extend(self.engine.drawings().iter().map(|drawing| drawing.id));
        } else {
            self.locked_drawings.clear();
        }
        let changed = previous.locked_count != self.drawings_lock_summary().locked_count;
        if changed {
            self.engine.drawing_drag_end();
            self.invalidate_series_frame();
        }
        changed
    }

    /// Removes the selected drawing, if one exists.
    pub fn remove_selected_drawing(&mut self) -> bool {
        let selected = self.engine.selected_drawing();
        let removed = self.engine.remove_selected_drawing();
        if removed {
            if let Some(id) = selected {
                self.locked_drawings.remove(&id);
            }
            self.invalidate_series_frame();
        }
        removed
    }

    /// Removes every committed drawing.
    pub fn clear_drawings(&mut self) {
        self.cancel_drawing_gesture();
        self.engine.clear_drawings();
        self.locked_drawings.clear();
        self.invalidate_series_frame();
    }

    /// Returns whether the newest bar is aligned to the real-time edge.
    #[must_use]
    pub fn is_at_latest(&self) -> bool {
        self.engine.scroll_position().abs() < f64::EPSILON
    }

    /// Returns whether a provider snapshot has populated the chart surface.
    #[must_use]
    pub const fn has_market_data(&self) -> bool {
        self.data_bridge.is_some()
    }

    /// Enqueues one replay update for the next chart frame.
    ///
    /// # Errors
    ///
    /// Returns the unchanged update if the bounded bridge is full.
    pub fn try_queue_replay_update(
        &mut self,
        update: ReplayStreamUpdate,
    ) -> Result<(), Box<ReplayStreamUpdate>> {
        let Some(bridge) = &mut self.data_bridge else {
            return Err(Box::new(update));
        };
        bridge.try_push(update)
    }

    /// Returns the number of replay commands waiting for the next frame.
    #[must_use]
    pub fn queued_replay_update_count(&self) -> usize {
        self.data_bridge
            .as_ref()
            .map_or(0, ChartDataBridge::queued_update_count)
    }

    /// Returns the next sequence expected by the chart bridge.
    #[must_use]
    pub fn expected_replay_sequence(&self) -> Option<u64> {
        self.data_bridge
            .as_ref()
            .and_then(ChartDataBridge::expected_sequence)
    }

    /// Returns bounded queue and resnapshot telemetry for this chart subscription.
    #[must_use]
    pub fn replay_bridge_metrics(&self) -> ChartBridgeMetrics {
        self.data_bridge
            .as_ref()
            .map_or_else(ChartBridgeMetrics::default, ChartDataBridge::metrics)
    }

    /// Takes the most recent foreground duration for installing one queued snapshot.
    #[cfg(feature = "diagnostics")]
    pub fn take_snapshot_installation_nanos(&mut self) -> Option<u64> {
        self.last_snapshot_installation_nanos.take()
    }

    /// Offers recovery to a bounded worker queue and marks dispatch only after acceptance.
    ///
    /// # Errors
    ///
    /// Returns the worker queue's rejection without changing bridge dispatch state.
    pub fn try_dispatch_replay_recovery<DispatchError>(
        &mut self,
        dispatch: impl FnOnce(ReplayRecoveryCommand) -> Result<(), DispatchError>,
    ) -> Result<bool, DispatchError> {
        let Some(bridge) = &mut self.data_bridge else {
            return Ok(false);
        };
        bridge.try_dispatch_recovery(dispatch)
    }

    /// Installs a response only when its request ID matches the active dispatched recovery.
    ///
    /// # Errors
    ///
    /// Returns an error if the correlated snapshot cannot establish resumable chart state.
    pub fn install_replay_recovery(
        &mut self,
        request_id: u64,
        replay: &ReplaySnapshot,
    ) -> Result<bool, ReplayValidationError> {
        let Some(bridge) = &mut self.data_bridge else {
            return Ok(false);
        };
        if !bridge.install_recovery_snapshot(request_id, replay)? {
            return Ok(false);
        }
        install_replay(&mut self.engine, self.volume_series, replay);
        self.displayed_provenance.replace_snapshot(replay);
        self.price_divisor = replay_price_divisor(replay);
        self.invalidate_series_frame();
        Ok(true)
    }

    /// Records failure only for the active correlated recovery request.
    pub fn mark_replay_recovery_failed(&mut self, request_id: u64) -> bool {
        self.data_bridge
            .as_mut()
            .is_some_and(|bridge| bridge.mark_recovery_failed(request_id))
    }

    /// Blocks ordered chart updates and requests a correlated replacement snapshot.
    pub fn mark_replay_stream_invalid(&mut self) {
        if let Some(bridge) = &mut self.data_bridge {
            bridge.mark_stream_invalid();
        }
    }

    /// Returns canonical evidence for the latest value installed into Origin.
    #[must_use]
    pub fn latest_market_provenance(&self) -> Option<&MarketEventProvenance> {
        self.displayed_provenance.latest()
    }

    fn apply_pending_data(&mut self) {
        let Some(bridge) = &mut self.data_bridge else {
            return;
        };
        match bridge.drain_merged() {
            Ok(Some(update)) if update.mutates_series() => {
                #[cfg(feature = "diagnostics")]
                let snapshot_install_started = update.snapshot().map(|_| Instant::now());
                if let Some(snapshot) = update.snapshot() {
                    self.displayed_provenance.replace_snapshot(snapshot);
                }
                self.displayed_provenance.extend(update.accepted_deltas());
                apply_merged_chart_data(
                    &mut self.engine,
                    self.volume_series,
                    &mut self.price_divisor,
                    &update,
                );
                self.invalidate_series_frame();
                #[cfg(feature = "diagnostics")]
                if let Some(started) = snapshot_install_started {
                    self.last_snapshot_installation_nanos =
                        Some(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
                }
            }
            Ok(_) => {}
            Err(error) => {
                bridge.mark_stream_invalid();
                eprintln!("replay update rejected; snapshot required: {error}");
            }
        }
    }

    fn invalidate_series_frame(&mut self) {
        self.frame = ChartFrame::default();
        self.axis_prims.clear();
        self.built_for = (0.0, 0.0, 0.0);
    }

    fn local_position(&self, position: gpui::Point<gpui::Pixels>) -> (f64, f64) {
        let window_x: f32 = position.x.into();
        let window_y: f32 = position.y.into();
        let chart_x = f64::from(window_x - self.viewport_origin.0);
        let y = f64::from(window_y - self.viewport_origin.1);
        (chart_x - self.engine.pane_left, y)
    }

    fn drawing_modifiers(modifiers: Modifiers) -> DrawingModifiers {
        DrawingModifiers {
            magnet: modifiers.control || modifiers.platform,
            straighten: modifiers.shift,
        }
    }

    fn cancel_drawing_gesture(&mut self) {
        self.engine.drawing_drag_end();
        self.engine.drawing_create_cancel();
        self.engine.brush_create_cancel();
    }

    fn drawing_pointer_down(&mut self, pane_x: f64, y: f64, modifiers: DrawingModifiers) -> bool {
        match self.drawing_tool {
            ChartDrawingTool::Cursor => {
                let hit = self.engine.hit_test_drawing(pane_x, y);
                if let Some(hit) = hit
                    && self.locked_drawings.contains(&hit.id)
                {
                    self.engine.set_selected_drawing(Some(hit.id));
                    self.engine.drawing_drag_end();
                    return true;
                }
                if self.engine.drawing_drag_start_at(pane_x, y) {
                    return true;
                }
                self.engine.set_selected_drawing(None);
                false
            }
            ChartDrawingTool::Brush => self.engine.brush_create_start(None, pane_x, y),
            _ => {
                let result = self.engine.drawing_create_click(pane_x, y, modifiers);
                if result > 0 {
                    self.drawing_tool = ChartDrawingTool::Cursor;
                    self.cursor_style = CursorStyle::Crosshair;
                }
                result != 0
            }
        }
    }

    fn drawing_pointer_move(
        &mut self,
        pane_x: f64,
        y: f64,
        dragging: bool,
        modifiers: DrawingModifiers,
    ) -> bool {
        if self.engine.brush_create_active() {
            if dragging {
                self.engine.brush_create_add(pane_x, y);
            } else {
                self.engine.brush_create_cancel();
            }
            return true;
        }
        if self.engine.drawing_drag_active() {
            if dragging {
                self.engine.drawing_drag_to(pane_x, y, modifiers);
            } else {
                self.engine.drawing_drag_end();
            }
            return true;
        }
        if self.engine.drawing_create_active() {
            self.engine.drawing_create_move(pane_x, y, modifiers);
            return true;
        }
        false
    }

    fn drawing_pointer_up(&mut self, pane_x: f64, y: f64, modifiers: DrawingModifiers) -> bool {
        if self.engine.brush_create_active() {
            self.engine.brush_create_add(pane_x, y);
            self.engine.brush_create_end();
            self.drawing_tool = ChartDrawingTool::Cursor;
            self.cursor_style = CursorStyle::Crosshair;
            return true;
        }
        if self.engine.drawing_drag_active() {
            self.engine.drawing_drag_to(pane_x, y, modifiers);
            self.engine.drawing_drag_end();
            return true;
        }
        false
    }

    fn update_crosshair(&mut self, pane_x: f64, y: f64) {
        self.engine.crosshair = (self.separator_at(y).is_none()
            && pane_x >= 0.0
            && pane_x <= self.engine.pane_w
            && y >= 0.0
            && y <= self.engine.pane_h)
            .then_some((pane_x, y));
        self.invalidate_series_frame();
    }

    fn separator_at(&self, y: f64) -> Option<usize> {
        self.engine
            .panes
            .iter()
            .skip(1)
            .position(|pane| (y - pane.top).abs() <= PANE_SEPARATOR_HIT)
    }

    fn update_cursor(&mut self, pane_x: f64, y: f64) {
        let active_separator = matches!(self.drag, Some(ChartDrag::PaneSeparator { .. }));
        let separator = self.separator_at(y);
        let separator_hover = (!active_separator).then_some(separator).flatten();
        if self.engine.separator_hover != separator_hover {
            self.engine.set_separator_hover(separator_hover);
            self.invalidate_series_frame();
        }
        self.cursor_style = if active_separator || separator.is_some() {
            CursorStyle::ResizeRow
        } else if self.engine.drawing_drag_active() {
            CursorStyle::ClosedHand
        } else if self.drawing_tool != ChartDrawingTool::Cursor {
            CursorStyle::Crosshair
        } else if let Some(hit) = self.engine.hit_test_drawing(pane_x, y) {
            match hit.cursor {
                "pointer" => CursorStyle::PointingHand,
                "move" => CursorStyle::OpenHand,
                "ns-resize" => CursorStyle::ResizeUpDown,
                "ew-resize" => CursorStyle::ResizeLeftRight,
                "nwse-resize" => CursorStyle::ResizeUpRightDownLeft,
                "nesw-resize" => CursorStyle::ResizeUpLeftDownRight,
                _ => CursorStyle::Crosshair,
            }
        } else {
            match self.drag {
                Some(ChartDrag::Pane) => CursorStyle::ClosedHand,
                Some(ChartDrag::TimeAxis) => CursorStyle::ResizeLeftRight,
                Some(ChartDrag::PriceAxis { .. }) => CursorStyle::ResizeUpDown,
                Some(ChartDrag::PaneSeparator { .. }) => CursorStyle::ResizeRow,
                None if y > self.engine.pane_h => CursorStyle::ResizeLeftRight,
                None if pane_x < 0.0 || pane_x > self.engine.pane_w => CursorStyle::ResizeUpDown,
                None => CursorStyle::Crosshair,
            }
        };
    }

    fn begin_drag(&mut self, pane_x: f64, y: f64, click_count: usize) {
        self.end_drag(pane_x, y);
        let pane = self.engine.pane_index_at_y(y);
        if click_count >= 2 {
            if y > self.engine.pane_h {
                self.engine.reset_time_scale();
            } else if pane_x < 0.0 {
                self.engine
                    .set_price_scale_auto_scale_for(pane, PriceScaleTarget::Left, true);
            } else if pane_x > self.engine.pane_w {
                self.engine
                    .set_price_scale_auto_scale_for(pane, PriceScaleTarget::Right, true);
            }
            self.update_crosshair(pane_x, y);
            return;
        }
        self.drag = if let Some(index) = self.separator_at(y) {
            self.engine.set_separator_hover(None);
            Some(ChartDrag::PaneSeparator { index, last_y: y })
        } else if y > self.engine.pane_h {
            self.engine.time_axis_start_scale(pane_x);
            Some(ChartDrag::TimeAxis)
        } else if pane_x < 0.0
            && self
                .engine
                .price_axis_scalable(pane, PriceScaleTarget::Left)
        {
            self.engine
                .price_axis_start_scale(pane, PriceScaleTarget::Left, y);
            Some(ChartDrag::PriceAxis {
                pane,
                target: PriceScaleTarget::Left,
            })
        } else if pane_x > self.engine.pane_w
            && self
                .engine
                .price_axis_scalable(pane, PriceScaleTarget::Right)
        {
            self.engine
                .price_axis_start_scale(pane, PriceScaleTarget::Right, y);
            Some(ChartDrag::PriceAxis {
                pane,
                target: PriceScaleTarget::Right,
            })
        } else if pane_x >= 0.0 && y >= 0.0 && y <= self.engine.pane_h {
            self.engine.time_scale.start_scroll(pane_x);
            Some(ChartDrag::Pane)
        } else {
            None
        };
        self.update_cursor(pane_x, y);
        self.update_crosshair(pane_x, y);
    }

    fn drag_to(&mut self, pane_x: f64, y: f64) {
        match self.drag {
            Some(ChartDrag::Pane) => self.engine.time_scale.scroll_to(pane_x),
            Some(ChartDrag::TimeAxis) => self.engine.time_axis_scale_to(pane_x),
            Some(ChartDrag::PriceAxis { pane, target }) => {
                self.engine.price_axis_scale_to(pane, target, y);
            }
            Some(ChartDrag::PaneSeparator { index, last_y }) => {
                self.engine.drag_pane_separator(index, y - last_y);
                self.drag = Some(ChartDrag::PaneSeparator { index, last_y: y });
                self.invalidate_series_frame();
            }
            None => {}
        }
        self.update_cursor(pane_x, y);
        if matches!(self.drag, Some(ChartDrag::PaneSeparator { .. })) {
            self.engine.crosshair = None;
            self.invalidate_series_frame();
        } else {
            self.update_crosshair(pane_x, y);
        }
    }

    fn end_drag(&mut self, pane_x: f64, y: f64) {
        match self.drag.take() {
            Some(ChartDrag::Pane) => self.engine.time_scale.end_scroll(),
            Some(ChartDrag::TimeAxis) => self.engine.time_axis_end_scale(),
            Some(ChartDrag::PriceAxis { pane, target }) => {
                self.engine.price_axis_end_scale(pane, target);
            }
            Some(ChartDrag::PaneSeparator { .. }) | None => {}
        }
        self.update_cursor(pane_x, y);
        self.update_crosshair(pane_x, y);
    }

    fn apply_wheel(&mut self, pane_x: f64, y: f64, normalized_x: f64, normalized_y: f64) {
        if normalized_y != 0.0 {
            let zoom = origin_engine::wheel_zoom_scale(normalized_y);
            let pane = self.engine.pane_index_at_y(y);
            if pane_x < 0.0 {
                self.engine
                    .price_axis_wheel_zoom(pane, PriceScaleTarget::Left, y, zoom);
            } else if pane_x > self.engine.pane_w {
                self.engine
                    .price_axis_wheel_zoom(pane, PriceScaleTarget::Right, y, zoom);
            } else {
                self.engine.time_scale.zoom(pane_x, zoom);
            }
        }
        if normalized_x != 0.0 {
            self.engine.time_scale.start_scroll(0.0);
            self.engine
                .time_scale
                .scroll_to(origin_engine::WHEEL_SCROLL_PX_PER_DELTA * normalized_x);
            self.engine.time_scale.end_scroll();
        }
        self.update_cursor(pane_x, y);
        self.update_crosshair(pane_x, y);
    }

    fn clear_pointer(&mut self, cx: &mut Context<Self>) {
        self.cancel_gesture();
        cx.notify();
    }

    fn cancel_gesture(&mut self) {
        self.end_drag(-1.0, -1.0);
        self.engine.drawing_drag_end();
        self.engine.brush_create_cancel();
        self.engine.crosshair = None;
        self.engine.set_separator_hover(None);
        self.cursor_style = CursorStyle::Crosshair;
        self.invalidate_series_frame();
    }

    fn move_pointer(&mut self, pane_x: f64, y: f64, dragging: bool, modifiers: DrawingModifiers) {
        if self.drawing_pointer_move(pane_x, y, dragging, modifiers) {
            self.update_crosshair(pane_x, y);
            return;
        }
        if self.drag.is_some() {
            if dragging {
                self.drag_to(pane_x, y);
            } else {
                self.end_drag(pane_x, y);
            }
        } else {
            self.update_cursor(pane_x, y);
            self.update_crosshair(pane_x, y);
        }
    }

    fn apply_key(&mut self, key: &str, accelerated: bool) -> bool {
        let step = if accelerated { 10.0 } else { 1.0 };
        let page =
            (self.engine.pane_w / self.engine.bar_spacing() * KEYBOARD_PAGE_FRACTION).max(1.0);
        let center = self.engine.pane_w / 2.0;
        match key {
            "left" => self
                .engine
                .scroll_to_position(self.engine.scroll_position() - step),
            "right" => self
                .engine
                .scroll_to_position(self.engine.scroll_position() + step),
            "pageup" => self
                .engine
                .scroll_to_position(self.engine.scroll_position() - page),
            "pagedown" => self
                .engine
                .scroll_to_position(self.engine.scroll_position() + page),
            "+" | "=" => self.engine.time_scale.zoom(center, 0.5),
            "-" | "_" => self.engine.time_scale.zoom(center, -0.5),
            "home" => self.reset_view(),
            "end" => self.scroll_to_latest(),
            "delete" | "backspace" => {
                if !self.remove_selected_drawing() {
                    return false;
                }
            }
            "escape" => self.cancel_drawing(),
            _ => return false,
        }
        self.invalidate_series_frame();
        true
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(focus_handle) = &self.focus_handle {
            window.focus(focus_handle, cx);
        }
        let (pane_x, y) = self.local_position(event.position);
        if self.separator_at(y).is_some() {
            self.begin_drag(pane_x, y, event.click_count);
        } else if self.drawing_pointer_down(pane_x, y, Self::drawing_modifiers(event.modifiers)) {
            self.update_cursor(pane_x, y);
            self.update_crosshair(pane_x, y);
        } else {
            self.begin_drag(pane_x, y, event.click_count);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        if self.apply_key(
            event.keystroke.key.as_str(),
            modifiers.control || modifiers.shift,
        ) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (pane_x, y) = self.local_position(event.position);
        self.move_pointer(
            pane_x,
            y,
            event.dragging(),
            Self::drawing_modifiers(event.modifiers),
        );
        cx.notify();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let (pane_x, y) = self.local_position(event.position);
        if !self.drawing_pointer_up(pane_x, y, Self::drawing_modifiers(event.modifiers)) {
            self.end_drag(pane_x, y);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (pane_x, y) = self.local_position(event.position);
        let delta = event.delta.pixel_delta(px(WHEEL_LINE_HEIGHT));
        let dx: f32 = delta.x.into();
        let dy: f32 = delta.y.into();
        let normalized_x = f64::from(dx) / 100.0;
        let normalized_y = f64::from(dy) / 100.0;
        self.apply_wheel(pane_x, y, normalized_x, normalized_y);
        cx.stop_propagation();
        cx.notify();
    }

    fn rebuild(&mut self, width: f32, height: f32, scale_factor: f32, window: &Window) {
        self.apply_pending_data();
        let dimensions = (width, height, scale_factor);
        if self.built_for == dimensions && !self.frame.panes.is_empty() {
            return;
        }

        if self.built_for.2 > 0.0 && (self.built_for.2 - scale_factor).abs() > SCALE_FACTOR_EPSILON
        {
            self.renderer.invalidate_caches();
        }
        self.built_for = dimensions;
        self.engine.css_width = f64::from(width);
        self.engine.css_height = f64::from(height);
        self.engine.dpr = f64::from(scale_factor);

        let layout = self.engine.options.get().layout;
        let font_size = layout.font_size.to_f32().unwrap_or(12.0);
        let measure = |text: &str| {
            f64::from(measure_text(window, text, &layout.font_family, font_size, 400, false).width)
        };

        self.engine.recompute_layout_with_measure(true, measure);
        if !self.fitted {
            self.engine.fit_content();
            self.fitted = true;
            self.engine.recompute_layout_with_measure(true, measure);
        }

        let max_label_width = (layout.font_size + 4.0) * 5.0 / 8.0
            * f64::from(self.engine.tick_mark_max_character_length.max(1));
        let axis_frame = self.engine.build_axis_frame(max_label_width, measure);
        self.engine.build_frame_into(&mut self.frame);
        self.engine
            .build_axis_primitives_into(&axis_frame, &mut self.axis_prims, |_| 0.0);
    }

    fn paint(&mut self, bounds: Bounds<gpui::Pixels>, window: &mut Window, cx: &mut App) {
        let viewport = OriginViewport::from_bounds(
            bounds.origin.x.into(),
            bounds.origin.y.into(),
            bounds.size.width.into(),
            bounds.size.height.into(),
        );
        let prepared = PreparedOriginFrame::from_engine(&self.frame, &self.engine)
            .with_axis(&self.axis_prims, &[]);
        if let Err(error) =
            self.renderer
                .paint_frame(&prepared, viewport, window.scale_factor(), window, cx)
        {
            eprintln!("origin frame skipped: {error}");
        }
    }
}

impl Default for OriginChartView {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for OriginChartView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity: Entity<Self> = cx.entity();
        let prepaint_entity = entity.clone();
        let hover_entity = entity.clone();
        let focus_handle = self
            .focus_handle
            .get_or_insert_with(|| cx.focus_handle())
            .clone();

        div()
            .id("origin_chart_surface")
            .size_full()
            .cursor(self.cursor_style)
            .track_focus(&focus_handle)
            .key_context("OriginChart")
            .on_hover(move |hovered, _, cx| {
                if !*hovered {
                    hover_entity.update(cx, OriginChartView::clear_pointer);
                }
            })
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                canvas(
                    move |bounds: Bounds<gpui::Pixels>, window, cx| {
                        let width = bounds.size.width.into();
                        let height = bounds.size.height.into();
                        let scale_factor = window.scale_factor();
                        prepaint_entity.update(cx, |chart, _| {
                            chart.viewport_origin =
                                (bounds.origin.x.into(), bounds.origin.y.into());
                            chart.rebuild(width, height, scale_factor, window);
                        });
                        bounds
                    },
                    move |_bounds: Bounds<gpui::Pixels>, prepainted, window, cx| {
                        entity.update(cx, |chart, cx| {
                            chart.paint(prepainted, window, cx);
                        });
                    },
                )
                .size_full(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_application::{Provenanced, ReplayTailUpdate};

    fn interactive_chart() -> OriginChartView {
        let mut chart = OriginChartView::new();
        chart.engine.recompute_layout_with_measure(true, |_| 48.0);
        chart.engine.fit_content();
        chart.engine.recompute_layout_with_measure(true, |_| 48.0);
        chart.fitted = true;
        chart
    }

    #[test]
    fn empty_chart_surface_accepts_its_first_real_snapshot() {
        let mut chart = OriginChartView::empty();
        assert!(!chart.has_market_data());
        assert_eq!(chart.queued_replay_update_count(), 0);
        assert_eq!(chart.expected_replay_sequence(), None);
        assert_eq!(chart.replay_bridge_metrics(), ChartBridgeMetrics::default());
        assert!(chart.latest_market_provenance().is_none());

        let replay = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
            .expect("embedded replay validates");
        chart.load_replay(&replay).expect("first snapshot installs");
        assert!(chart.has_market_data());
        assert_eq!(
            chart.expected_replay_sequence(),
            replay.stream().last_sequence().checked_add(1)
        );
        assert!(chart.latest_market_provenance().is_some());
    }

    #[test]
    fn chart_applies_live_tail_replace_and_append_in_one_frame_boundary() {
        let replay = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
            .expect("embedded replay validates");
        let mut chart = OriginChartView::with_replay(&replay);
        let initial_expected = chart
            .expected_replay_sequence()
            .expect("snapshot establishes sequence");
        let source = replay.bars().last().cloned().expect("tail exists");
        let mut replacement_bar = *source.value();
        replacement_bar.close = replacement_bar.close.saturating_add(1);
        let mut replacement_provenance = source.provenance().clone();
        replacement_provenance.event_id = "tail-replacement".to_string();
        let replacement = ReplayTailUpdate::try_new(
            Provenanced::new(replacement_bar, replacement_provenance),
            replay.evidence().publication_generation + 1,
            true,
        )
        .expect("replacement validates");
        chart
            .try_queue_replay_update(ReplayStreamUpdate::Tail(replacement))
            .expect("replacement queues");
        chart.apply_pending_data();
        assert_eq!(chart.expected_replay_sequence(), Some(initial_expected));
        assert_eq!(
            chart
                .latest_market_provenance()
                .map(|provenance| provenance.event_id.as_str()),
            Some("tail-replacement")
        );

        let appended = EmbeddedReplaySource
            .load_delta(initial_expected.saturating_sub(1))
            .expect("fixture delta loads")
            .expect("fixture delta exists");
        let appended = ReplayTailUpdate::try_new(
            appended.item().clone(),
            replay.evidence().publication_generation + 2,
            true,
        )
        .expect("append validates");
        chart
            .try_queue_replay_update(ReplayStreamUpdate::Tail(appended))
            .expect("append queues");
        chart.apply_pending_data();
        assert_eq!(
            chart.expected_replay_sequence(),
            initial_expected.checked_add(1)
        );
    }

    #[cfg(feature = "diagnostics")]
    #[test]
    fn snapshot_installation_records_queued_and_direct_foreground_durations() {
        let replay = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 64 })
            .expect("fixture validates");
        let replacement = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 65 })
            .expect("replacement fixture validates")
            .try_with_publication_generation(
                replay.evidence().publication_generation.saturating_add(1),
            )
            .expect("replacement generation validates");
        let mut chart = OriginChartView::with_replay(&replay);

        assert!(
            chart
                .try_queue_replay_update(ReplayStreamUpdate::Snapshot(replacement))
                .is_ok()
        );
        chart.apply_pending_data();

        assert!(chart.take_snapshot_installation_nanos().is_some());
        assert_eq!(chart.take_snapshot_installation_nanos(), None);

        let direct_replacement = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 66 })
            .expect("direct replacement fixture validates")
            .try_with_publication_generation(
                replay.evidence().publication_generation.saturating_add(2),
            )
            .expect("direct replacement generation validates");
        chart
            .load_replay(&direct_replacement)
            .expect("direct replacement installs");
        assert!(chart.take_snapshot_installation_nanos().is_some());
        assert_eq!(chart.take_snapshot_installation_nanos(), None);
    }

    #[test]
    fn visible_time_range_roundtrips_through_persistent_unix_nanos() {
        let mut chart = interactive_chart();
        let (start, end) = chart
            .visible_time_range_unix_nanos()
            .expect("interactive chart has a viewport");
        let restored = (start + 60_000_000_000, end - 60_000_000_000);
        assert!(chart.set_visible_time_range_unix_nanos(restored.0, restored.1));
        assert_eq!(chart.visible_time_range_unix_nanos(), Some(restored));
    }

    #[test]
    fn platform_theme_owns_neutrals_and_market_series_semantics() {
        let chart = OriginChartView::empty();
        let series = &chart.engine.series[0];

        assert_eq!(
            chart.engine.options.get().layout.background.color,
            "#070a0f"
        );
        assert_eq!(chart.engine.options.get().layout.text_color, "#fafafa");
        assert_eq!(chart.engine.options.get().layout.font_family, "Inter");
        assert_eq!(chart.engine.options.get().grid.vert_lines.color, "#16191f");
        assert_eq!(
            chart.engine.options.get().crosshair.vert_line.color,
            "#9da3aa"
        );
        assert!(series.line_color.is_none());
        assert_eq!(series.up_color.as_deref(), Some("#089981"));
        assert_eq!(series.down_color.as_deref(), Some("#f7525f"));
        assert_eq!(series.wick_up_color.as_deref(), Some("#089981"));
        assert_eq!(series.wick_down_color.as_deref(), Some("#f7525f"));
        assert_eq!(series.border_up_color.as_deref(), Some("#089981"));
        assert_eq!(series.border_down_color.as_deref(), Some("#f7525f"));
    }

    #[test]
    fn platform_theme_switch_is_atomic_for_data_viewport_drawings_and_indicators() {
        let mut chart = interactive_chart();
        let volume = chart
            .add_indicator(ChartIndicator::Volume)
            .expect("volume is available");
        let vwap = chart
            .add_indicator(ChartIndicator::Vwap)
            .expect("vwap is available");
        assert!(
            chart
                .engine
                .drawing_create_begin(DrawingKind::HorizontalLine, None)
        );
        assert_eq!(
            chart
                .engine
                .drawing_create_click(300.0, 200.0, DrawingModifiers::default()),
            1
        );
        chart.engine.time_scale.zoom(300.0, 0.5);
        chart.engine.scroll_to_position(-12.0);

        let price_data = chart
            .engine
            .data
            .series_data(0)
            .map(|(times, columns)| (times.to_vec(), columns.map(<[f64]>::to_vec)))
            .expect("price data");
        let volume_data = chart
            .engine
            .data
            .series_data(chart.volume_series)
            .map(|(times, columns)| (times.to_vec(), columns.map(<[f64]>::to_vec)))
            .expect("volume data");
        let spacing = chart.engine.bar_spacing();
        let offset = chart.engine.right_offset();
        let drawings = chart.engine.drawings_json();
        let pane_count = chart.engine.panes.len();
        let series_count = chart.engine.series.len();

        chart
            .set_platform_theme(&AxiusflowTheme::light())
            .expect("platform theme patch is valid");
        assert_eq!(
            chart.engine.options.get().layout.background.color,
            "#ffffff"
        );
        assert_eq!(chart.engine.options.get().layout.text_color, "#333333");
        assert_eq!(chart.engine.options.get().layout.font_family, "Inter");
        assert_eq!(chart.engine.options.get().grid.vert_lines.color, "#f3f3f3");
        assert_eq!(
            chart.engine.options.get().crosshair.vert_line.color,
            "#737373"
        );
        chart
            .set_platform_theme(&AxiusflowTheme::dark())
            .expect("platform theme patch is valid");
        assert_eq!(
            chart.engine.options.get().layout.background.color,
            "#070a0f"
        );
        assert_eq!(
            chart.engine.options.get().crosshair.vert_line.color,
            "#9da3aa"
        );
        assert_eq!(
            chart
                .engine
                .data
                .series_data(0)
                .map(|(times, columns)| { (times.to_vec(), columns.map(<[f64]>::to_vec)) }),
            Some(price_data)
        );
        assert_eq!(
            chart
                .engine
                .data
                .series_data(chart.volume_series)
                .map(|(times, columns)| { (times.to_vec(), columns.map(<[f64]>::to_vec)) }),
            Some(volume_data)
        );
        assert_eq!(chart.engine.bar_spacing().to_bits(), spacing.to_bits());
        assert_eq!(chart.engine.right_offset().to_bits(), offset.to_bits());
        assert_eq!(chart.engine.drawings_json(), drawings);
        assert_eq!(chart.engine.panes.len(), pane_count);
        assert_eq!(chart.engine.series.len(), series_count);
        assert_eq!(volume, vec![chart.volume_series]);
        assert!(chart.engine.series[chart.volume_series].visible);
        assert_eq!(
            chart.engine.indicator_info(vwap[0]).map(|info| info.kind),
            Some("vwap")
        );
    }

    #[test]
    fn wheel_zoom_and_horizontal_scroll_mutate_origin_without_refitting() {
        let mut chart = interactive_chart();
        let spacing = chart.engine.bar_spacing();
        chart.apply_wheel(400.0, 200.0, 0.0, 1.0);
        assert!((chart.engine.bar_spacing() - spacing).abs() > f64::EPSILON);
        let offset = chart.engine.right_offset();
        chart.apply_wheel(400.0, 200.0, 1.0, 0.0);
        assert!((chart.engine.right_offset() - offset).abs() > f64::EPSILON);
        assert!(chart.fitted);
    }

    #[test]
    fn mouse_pan_and_crosshair_have_bounded_lifecycle() {
        let mut chart = interactive_chart();
        chart.begin_drag(300.0, 200.0, 1);
        assert_eq!(chart.drag, Some(ChartDrag::Pane));
        assert_eq!(chart.engine.crosshair, Some((300.0, 200.0)));
        let offset = chart.engine.right_offset();
        chart.drag_to(340.0, 200.0);
        assert!((chart.engine.right_offset() - offset).abs() > f64::EPSILON);
        chart.end_drag(340.0, 200.0);
        assert!(chart.drag.is_none());
        chart.update_crosshair(-1.0, 200.0);
        assert!(chart.engine.crosshair.is_none());
    }

    #[test]
    fn axes_drag_and_double_click_reset_through_origin() {
        let mut chart = interactive_chart();
        chart.begin_drag(300.0, chart.engine.pane_h + 10.0, 1);
        assert_eq!(chart.drag, Some(ChartDrag::TimeAxis));
        chart.drag_to(340.0, chart.engine.pane_h + 10.0);
        chart.end_drag(340.0, chart.engine.pane_h + 10.0);
        assert!(chart.drag.is_none());

        let right_axis_x = chart.engine.pane_w + 1.0;
        chart.begin_drag(right_axis_x, 200.0, 1);
        assert!(matches!(
            chart.drag,
            Some(ChartDrag::PriceAxis {
                target: PriceScaleTarget::Right,
                ..
            })
        ));
        chart.drag_to(right_axis_x, 240.0);
        chart.end_drag(right_axis_x, 240.0);
        assert!(chart.drag.is_none());

        chart.engine.time_scale.start_scroll(0.0);
        chart.engine.time_scale.scroll_to(80.0);
        chart.engine.time_scale.end_scroll();
        let offset = chart.engine.right_offset();
        assert!(offset.abs() > f64::EPSILON);
        chart.begin_drag(300.0, chart.engine.pane_h + 10.0, 2);
        assert!(chart.engine.right_offset().abs() < offset.abs());
        assert!(chart.drag.is_none());
    }

    #[test]
    fn reset_view_fits_time_and_restores_automatic_price_scaling() {
        let mut chart = interactive_chart();
        chart.engine.time_scale.start_scroll(0.0);
        chart.engine.time_scale.scroll_to(80.0);
        chart.engine.time_scale.end_scroll();
        chart
            .engine
            .set_price_scale_auto_scale_for(0, PriceScaleTarget::Right, false);
        assert!(chart.engine.right_offset().abs() > f64::EPSILON);
        assert_eq!(
            chart
                .engine
                .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
            Some(false)
        );

        chart.reset_view();

        assert!(chart.engine.right_offset().abs() < f64::EPSILON);
        assert_eq!(
            chart
                .engine
                .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
            Some(true)
        );
        assert!(chart.fitted);
    }

    #[test]
    fn scroll_to_latest_preserves_zoom_and_returns_to_real_time_edge() {
        let mut chart = interactive_chart();
        chart.engine.time_scale.zoom(300.0, 0.5);
        chart.engine.scroll_to_position(-12.0);
        let spacing = chart.engine.bar_spacing();

        chart.scroll_to_latest();

        assert!(chart.is_at_latest());
        assert!((chart.engine.bar_spacing() - spacing).abs() < f64::EPSILON);
    }

    #[test]
    fn indicator_catalog_maps_to_origin_with_legacy_defaults() {
        let cases = [
            (ChartIndicator::Sma, 1, "sma", "SMA 20"),
            (ChartIndicator::Ema, 1, "ema", "EMA 20"),
            (ChartIndicator::Wma, 1, "wma", "WMA 20"),
            (ChartIndicator::Bollinger, 3, "bollinger", "Bollinger 20 2"),
            (ChartIndicator::Rsi, 1, "rsi", "RSI 14"),
            (ChartIndicator::Macd, 3, "macd", "MACD 12 26 9"),
            (
                ChartIndicator::Stochastic,
                2,
                "stochastic",
                "Stochastic 14 3",
            ),
            (ChartIndicator::Atr, 1, "atr", "ATR 14"),
        ];

        for (indicator, output_count, kind, title) in cases {
            let mut chart = interactive_chart();
            let ids = chart
                .add_indicator(indicator)
                .expect("supported indicator is created");

            assert_eq!(ids.len(), output_count);
            assert_eq!(chart.engine.series[ids[0]].title, title);
            for id in ids {
                assert_eq!(
                    chart
                        .engine
                        .indicator_info(id)
                        .expect("indicator lineage")
                        .kind,
                    kind
                );
            }
        }
    }

    #[test]
    fn replay_volume_drives_histogram_and_vwap_with_real_weights() {
        let replay = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
            .expect("embedded replay validates");
        let mut chart = OriginChartView::with_replay(&replay);
        let (_, volume_columns) = chart
            .engine
            .data
            .series_data(chart.volume_series)
            .expect("parallel volume series");
        let expected_volume = replay
            .bars()
            .iter()
            .map(|item| {
                item.value()
                    .volume
                    .to_f64()
                    .expect("fixture volume fits f64")
            })
            .collect::<Vec<_>>();
        assert_eq!(volume_columns[3], expected_volume);
        assert!(!chart.engine.series[chart.volume_series].visible);
        assert!(chart.engine.series[chart.volume_series].histogram_updown);
        assert!(chart.engine.series[chart.volume_series].overlay);

        assert_eq!(
            chart
                .add_indicator(ChartIndicator::Volume)
                .expect("volume histogram"),
            vec![chart.volume_series]
        );
        assert!(chart.engine.series[chart.volume_series].visible);

        let vwap = chart
            .add_indicator(ChartIndicator::Vwap)
            .expect("volume-weighted average");
        assert_eq!(vwap.len(), 1);
        assert_eq!(
            chart.engine.indicator_info(vwap[0]).map(|info| info.kind),
            Some("vwap")
        );
        let (_, vwap_columns) = chart
            .engine
            .data
            .series_data(vwap[0])
            .expect("vwap output data");
        assert_eq!(vwap_columns[3].len(), replay.bars().len());
        assert!(vwap_columns[3].iter().all(|value| value.is_finite()));
    }

    #[test]
    fn indicator_api_rejects_an_empty_chart_without_inventing_series() {
        let mut chart = OriginChartView::empty();
        let initial_series = chart.engine.series.len();

        for indicator in ChartIndicator::ALL {
            assert_eq!(
                chart.add_indicator(indicator),
                Err(ChartIndicatorError::MarketDataUnavailable)
            );
        }

        assert_eq!(chart.engine.series.len(), initial_series);
    }

    #[test]
    fn indicator_metadata_matches_the_legacy_picker_copy() {
        assert_eq!(ChartIndicator::ALL.len(), 10);
        assert_eq!(ChartIndicator::Volume.label(), "Volume");
        assert_eq!(ChartIndicator::Vwap.parameters(), "Session anchored");
        assert_eq!(ChartIndicator::Sma.label(), "Moving Average");
        assert_eq!(ChartIndicator::Sma.parameters(), "Period 20");
        assert_eq!(
            ChartIndicator::Bollinger.parameters(),
            "Period 20 · Deviation 2"
        );
        assert_eq!(
            ChartIndicator::Macd.parameters(),
            "Fast 12 · Slow 26 · Signal 9"
        );
        assert_eq!(ChartIndicator::Stochastic.parameters(), "%K 14 · %D 3");
        assert_eq!(ChartIndicator::Atr.parameters(), "Period 14");
    }

    #[test]
    fn anchored_drawing_tools_commit_real_origin_drawings_and_return_to_cursor() {
        let mut chart = interactive_chart();
        let tools = [
            (ChartDrawingTool::TrendLine, 2, 160.0),
            (ChartDrawingTool::HorizontalLine, 1, 180.0),
            (ChartDrawingTool::VerticalLine, 1, 200.0),
            (ChartDrawingTool::Ray, 1, 220.0),
            (ChartDrawingTool::Rectangle, 2, 240.0),
            (ChartDrawingTool::Text, 1, 260.0),
        ];
        let anchor_x = [260.0, 340.0];

        for (index, (tool, anchors, y)) in tools.into_iter().enumerate() {
            chart.set_drawing_tool(tool);
            assert_eq!(chart.drawing_tool(), tool);
            for &x in anchor_x.iter().take(anchors) {
                let handled = chart.drawing_pointer_down(x, y, DrawingModifiers::default());
                assert!(handled);
            }
            assert_eq!(chart.drawing_count(), index + 1);
            assert_eq!(chart.drawing_tool(), ChartDrawingTool::Cursor);
            assert!(!chart.engine.drawing_create_active());
        }
    }

    #[test]
    fn brush_capture_commits_on_release_and_returns_to_cursor() {
        let mut chart = interactive_chart();
        chart.set_drawing_tool(ChartDrawingTool::Brush);

        assert!(chart.drawing_pointer_down(240.0, 180.0, DrawingModifiers::default()));
        assert!(chart.drawing_pointer_move(280.0, 210.0, true, DrawingModifiers::default()));
        assert!(chart.drawing_pointer_up(320.0, 240.0, DrawingModifiers::default()));

        assert_eq!(chart.drawing_count(), 1);
        assert_eq!(chart.drawing_tool(), ChartDrawingTool::Cursor);
        assert!(!chart.engine.brush_create_active());
    }

    #[test]
    fn cursor_selects_and_moves_unlocked_drawings_but_locked_drawings_do_not_drag() {
        let mut chart = interactive_chart();
        chart.set_drawing_tool(ChartDrawingTool::HorizontalLine);
        assert!(chart.drawing_pointer_down(300.0, 200.0, DrawingModifiers::default()));
        let id = chart.selected_drawing_id().expect("drawing selected");
        let (_, drawing_y) = chart
            .engine
            .drawing_point_to_coordinate(id, 0)
            .expect("drawing coordinate");
        chart.set_drawing_tool(ChartDrawingTool::Cursor);

        assert!(chart.set_selected_drawing_locked(true));
        assert!(chart.drawing_pointer_down(500.0, drawing_y, DrawingModifiers::default()));
        assert!(!chart.engine.drawing_drag_active());
        assert_eq!(chart.selected_drawing_id(), Some(id));

        assert!(chart.set_selected_drawing_locked(false));
        assert!(chart.drawing_pointer_down(500.0, drawing_y, DrawingModifiers::default()));
        assert!(chart.engine.drawing_drag_active());
        assert!(chart.drawing_pointer_up(500.0, drawing_y + 30.0, DrawingModifiers::default()));
        assert!(!chart.engine.drawing_drag_active());
    }

    #[test]
    fn lock_summary_delete_clear_and_escape_follow_toolbar_contract() {
        let mut chart = interactive_chart();
        chart.set_drawing_tool(ChartDrawingTool::HorizontalLine);
        assert!(chart.drawing_pointer_down(300.0, 180.0, DrawingModifiers::default()));
        chart.set_drawing_tool(ChartDrawingTool::HorizontalLine);
        assert!(chart.drawing_pointer_down(300.0, 240.0, DrawingModifiers::default()));
        assert_eq!(chart.drawing_count(), 2);

        assert!(chart.set_all_drawings_locked(true));
        assert_eq!(
            chart.drawings_lock_summary(),
            DrawingsLockSummary {
                total: 2,
                locked_count: 2,
                all_locked: true,
            }
        );
        assert!(chart.apply_key("delete", false));
        assert_eq!(chart.drawing_count(), 1);
        assert_eq!(chart.drawings_lock_summary().locked_count, 1);

        assert!(chart.apply_key("escape", false));
        assert_eq!(chart.drawing_tool(), ChartDrawingTool::Cursor);
        assert!(!chart.engine.drawing_create_active());
        chart.clear_drawings();
        assert_eq!(
            chart.drawings_lock_summary(),
            DrawingsLockSummary::default()
        );
        assert!(!chart.apply_key("backspace", false));
    }

    #[test]
    fn cursor_mode_still_falls_through_to_chart_pan_on_a_drawing_miss() {
        let mut chart = interactive_chart();
        assert!(!chart.drawing_pointer_down(300.0, 200.0, DrawingModifiers::default()));

        chart.begin_drag(300.0, 200.0, 1);

        assert_eq!(chart.drag, Some(ChartDrag::Pane));
    }

    #[test]
    fn keyboard_navigation_scrolls_zooms_resets_and_ignores_unknown_keys() {
        let mut chart = interactive_chart();
        let offset = chart.engine.scroll_position();
        assert!(chart.apply_key("right", false));
        assert!((chart.engine.scroll_position() - offset - 1.0).abs() < f64::EPSILON);
        assert!(chart.apply_key("left", true));
        assert!((chart.engine.scroll_position() - offset + 9.0).abs() < f64::EPSILON);

        let page = chart.engine.pane_w / chart.engine.bar_spacing() * KEYBOARD_PAGE_FRACTION;
        let before_page = chart.engine.scroll_position();
        assert!(chart.apply_key("pageup", false));
        assert!((chart.engine.scroll_position() - before_page + page).abs() < f64::EPSILON);
        assert!(chart.apply_key("pagedown", false));
        assert!((chart.engine.scroll_position() - before_page).abs() < f64::EPSILON);

        let spacing = chart.engine.bar_spacing();
        assert!(chart.apply_key("+", false));
        assert!((chart.engine.bar_spacing() - spacing).abs() > f64::EPSILON);

        chart.engine.crosshair = Some((100.0, 100.0));
        assert!(chart.apply_key("escape", false));
        assert!(chart.engine.crosshair.is_none());
        assert!(chart.apply_key("home", false));
        assert!(chart.engine.scroll_position().abs() < f64::EPSILON);
        chart.engine.scroll_to_position(-4.0);
        assert!(!chart.is_at_latest());
        assert!(chart.apply_key("end", false));
        assert!(chart.is_at_latest());
        assert!(!chart.apply_key("a", false));
    }

    #[test]
    fn native_pointer_state_ends_a_drag_when_mouse_up_was_lost() {
        let mut chart = interactive_chart();
        chart.begin_drag(300.0, 200.0, 1);
        assert_eq!(chart.drag, Some(ChartDrag::Pane));

        chart.move_pointer(340.0, 200.0, false, DrawingModifiers::default());

        assert!(chart.drag.is_none());
        assert_eq!(chart.cursor_style, CursorStyle::Crosshair);
        assert_eq!(chart.engine.crosshair, Some((340.0, 200.0)));
    }

    #[test]
    fn indicator_separator_resize_has_bounded_native_pointer_state() {
        let mut chart = interactive_chart();
        chart
            .add_indicator(ChartIndicator::Rsi)
            .expect("RSI creates its indicator pane");
        chart.engine.recompute_layout_with_measure(true, |_| 48.0);
        assert_eq!(chart.engine.panes.len(), 2);
        let separator_y = chart.engine.panes[1].top;
        let first_stretch = chart.engine.panes[0].stretch_factor;

        chart.update_cursor(300.0, separator_y);
        assert_eq!(chart.cursor_style, CursorStyle::ResizeRow);
        assert_eq!(chart.engine.separator_hover, Some(0));

        chart.begin_drag(300.0, separator_y, 1);
        assert!(matches!(
            chart.drag,
            Some(ChartDrag::PaneSeparator { index: 0, .. })
        ));
        assert!(chart.engine.crosshair.is_none());
        chart.drag_to(300.0, separator_y + 20.0);
        assert!(chart.engine.panes[0].stretch_factor > first_stretch);

        chart.move_pointer(
            300.0,
            separator_y + 40.0,
            false,
            DrawingModifiers::default(),
        );
        assert!(chart.drag.is_none());
        assert_eq!(chart.cursor_style, CursorStyle::Crosshair);
    }

    #[test]
    fn escape_cancels_every_active_gesture_and_clears_pointer_state() {
        let mut chart = interactive_chart();
        chart.begin_drag(300.0, chart.engine.pane_h + 10.0, 1);
        assert_eq!(chart.drag, Some(ChartDrag::TimeAxis));

        assert!(chart.apply_key("escape", false));

        assert!(chart.drag.is_none());
        assert!(chart.engine.crosshair.is_none());
        assert_eq!(chart.cursor_style, CursorStyle::Crosshair);
    }

    #[test]
    fn pointer_cursor_truthfully_tracks_chart_and_axis_gestures() {
        let mut chart = interactive_chart();
        chart.update_cursor(300.0, 200.0);
        assert_eq!(chart.cursor_style, CursorStyle::Crosshair);
        chart.update_cursor(300.0, chart.engine.pane_h + 1.0);
        assert_eq!(chart.cursor_style, CursorStyle::ResizeLeftRight);
        chart.update_cursor(chart.engine.pane_w + 1.0, 200.0);
        assert_eq!(chart.cursor_style, CursorStyle::ResizeUpDown);

        chart.begin_drag(300.0, 200.0, 1);
        assert_eq!(chart.cursor_style, CursorStyle::ClosedHand);
        chart.end_drag(300.0, 200.0);
        assert_eq!(chart.cursor_style, CursorStyle::Crosshair);
    }
}
