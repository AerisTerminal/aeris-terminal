//! GPUI entity hosting one authoritative Origin chart engine and renderer.

use crate::bridge::{ChartBridgeMetrics, ChartDataBridge, ReplayRecoveryCommand};
use crate::origin_bridge::{
    apply_merged_chart_data, apply_series_theme, apply_theme, chart_data_queue_capacity,
    install_replay, replay_price_divisor,
};
use crate::provenance::{DEFAULT_CHART_SERIES_MAX_POINTS, DisplayedProvenance};
use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketEventProvenance, ReplaySnapshot,
    ReplayStreamUpdate, ReplayValidationError, UseCase,
};
use axiusflow_design_system::AxiusflowTheme;
use gpui::{
    App, Bounds, Context, Entity, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Render, ScrollWheelEvent, Window, canvas, div, prelude::*, px, rgb,
};
use num_traits::ToPrimitive;
use origin_engine::{ChartEngine, ChartFrame, PriceScaleTarget};
use origin_render::draw_list::Prim;
use origin_render_gpui::backend::measure_text;
use origin_render_gpui::{GpuiChartRenderer, OriginViewport, PreparedOriginFrame};

const SCALE_FACTOR_EPSILON: f32 = 1.0e-4;
const WHEEL_LINE_HEIGHT: f32 = 32.0;

/// A GPUI entity hosting one authoritative Origin chart engine and renderer.
pub struct OriginChartView {
    engine: ChartEngine,
    renderer: GpuiChartRenderer,
    data_bridge: Option<ChartDataBridge>,
    displayed_provenance: DisplayedProvenance,
    price_divisor: f64,
    frame: ChartFrame,
    axis_prims: Vec<Prim>,
    theme: AxiusflowTheme,
    built_for: (f32, f32, f32),
    fitted: bool,
    viewport_origin: (f32, f32),
    panning: bool,
}

impl OriginChartView {
    /// Creates an empty themed Origin surface without inventing market data.
    #[must_use]
    pub fn empty(theme: AxiusflowTheme) -> Self {
        let mut engine = ChartEngine::new(1024.0, 640.0, 1.0);
        apply_theme(&mut engine, &theme);
        let retention_applied =
            engine.set_series_max_points(0, Some(DEFAULT_CHART_SERIES_MAX_POINTS));
        debug_assert!(retention_applied);
        apply_series_theme(&mut engine, &theme);
        Self {
            engine,
            renderer: GpuiChartRenderer::new(),
            data_bridge: None,
            displayed_provenance: DisplayedProvenance::empty(),
            price_divisor: 1.0,
            frame: ChartFrame::default(),
            axis_prims: Vec::new(),
            theme,
            built_for: (0.0, 0.0, 0.0),
            fitted: false,
            viewport_origin: (0.0, 0.0),
            panning: false,
        }
    }

    /// Creates a chart from the bounded embedded replay and default theme.
    #[must_use]
    pub fn new() -> Self {
        Self::with_theme(AxiusflowTheme::default())
    }

    /// Creates a chart from the bounded embedded replay and a resolved theme.
    ///
    /// # Panics
    ///
    /// Panics only if the application-owned embedded fixture violates its own
    /// validation contract.
    #[must_use]
    pub fn with_theme(theme: AxiusflowTheme) -> Self {
        let replay = EmbeddedReplaySource
            .execute(LoadEmbeddedReplay { bar_count: 600 })
            .expect("the embedded replay is validated application data");
        Self::with_theme_and_replay(theme, &replay)
    }

    /// Creates a chart from one validated application replay snapshot.
    ///
    /// # Panics
    ///
    /// Panics only if the validated snapshot cannot establish resumable sequence state.
    #[must_use]
    pub fn with_theme_and_replay(theme: AxiusflowTheme, replay: &ReplaySnapshot) -> Self {
        let mut engine = ChartEngine::new(1024.0, 640.0, 1.0);
        apply_theme(&mut engine, &theme);
        install_replay(&mut engine, replay);
        let retention_applied =
            engine.set_series_max_points(0, Some(DEFAULT_CHART_SERIES_MAX_POINTS));
        debug_assert!(retention_applied);
        apply_series_theme(&mut engine, &theme);
        let data_bridge = ChartDataBridge::try_new(chart_data_queue_capacity(), replay)
            .expect("a validated replay snapshot establishes chart sequence state");

        Self {
            engine,
            renderer: GpuiChartRenderer::new(),
            data_bridge: Some(data_bridge),
            displayed_provenance: DisplayedProvenance::from_snapshot(replay),
            price_divisor: replay_price_divisor(replay),
            frame: ChartFrame::default(),
            axis_prims: Vec::new(),
            theme,
            built_for: (0.0, 0.0, 0.0),
            fitted: false,
            viewport_origin: (0.0, 0.0),
            panning: false,
        }
    }

    /// Replaces Origin's authoritative series data with one validated snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot establish resumable sequence state.
    pub fn load_replay(&mut self, replay: &ReplaySnapshot) -> Result<(), ReplayValidationError> {
        if let Some(bridge) = &mut self.data_bridge {
            bridge.install_snapshot(replay)?;
        } else {
            self.data_bridge = Some(ChartDataBridge::try_new(
                chart_data_queue_capacity(),
                replay,
            )?);
        }
        install_replay(&mut self.engine, replay);
        self.displayed_provenance.replace_snapshot(replay);
        self.price_divisor = replay_price_divisor(replay);
        self.invalidate_series_frame();
        self.fitted = false;
        Ok(())
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

    /// Returns whether a detected stream gap requires a replacement snapshot.
    #[must_use]
    pub fn replay_requires_snapshot(&self) -> bool {
        self.data_bridge
            .as_ref()
            .is_some_and(ChartDataBridge::requires_snapshot)
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

    /// Peeks one retryable correlated recovery command without marking it dispatched.
    #[must_use]
    pub fn pending_replay_resnapshot_request(&self) -> Option<ReplayRecoveryCommand> {
        self.data_bridge
            .as_ref()
            .and_then(ChartDataBridge::pending_resnapshot_request)
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
        install_replay(&mut self.engine, replay);
        self.displayed_provenance.replace_snapshot(replay);
        self.price_divisor = replay_price_divisor(replay);
        self.invalidate_series_frame();
        self.fitted = false;
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

    /// Returns canonical evidence for a displayed value by its source sequence.
    #[must_use]
    pub fn market_provenance(&self, source_sequence: u64) -> Option<&MarketEventProvenance> {
        self.displayed_provenance.get(source_sequence)
    }

    /// Returns canonical evidence for the latest value installed into Origin.
    #[must_use]
    pub fn latest_market_provenance(&self) -> Option<&MarketEventProvenance> {
        self.displayed_provenance.latest()
    }

    /// Applies a complete theme revision to Origin before the next frame.
    pub fn set_theme(&mut self, theme: AxiusflowTheme) {
        if self.theme == theme {
            return;
        }
        apply_theme(&mut self.engine, &theme);
        apply_series_theme(&mut self.engine, &theme);
        self.theme = theme;
        self.renderer.invalidate_caches();
        self.built_for = (0.0, 0.0, 0.0);
    }

    fn apply_pending_data(&mut self) {
        let Some(bridge) = &mut self.data_bridge else {
            return;
        };
        match bridge.drain_merged() {
            Ok(Some(update)) if update.mutates_series() => {
                let replaces_snapshot = update.snapshot().is_some();
                if let Some(snapshot) = update.snapshot() {
                    self.displayed_provenance.replace_snapshot(snapshot);
                }
                self.displayed_provenance.extend(update.accepted_deltas());
                apply_merged_chart_data(&mut self.engine, &mut self.price_divisor, &update);
                self.invalidate_series_frame();
                if replaces_snapshot {
                    self.fitted = false;
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

    fn update_crosshair(&mut self, pane_x: f64, y: f64) {
        self.engine.crosshair =
            (pane_x >= 0.0 && pane_x <= self.engine.pane_w && y >= 0.0 && y <= self.engine.pane_h)
                .then_some((pane_x, y));
        self.invalidate_series_frame();
    }

    fn begin_pan(&mut self, pane_x: f64, y: f64) {
        if pane_x < 0.0 || pane_x > self.engine.pane_w || y < 0.0 || y > self.engine.pane_h {
            self.panning = false;
            self.update_crosshair(pane_x, y);
            return;
        }
        self.engine.time_scale.end_scroll();
        self.engine.time_scale.start_scroll(pane_x);
        self.panning = true;
        self.update_crosshair(pane_x, y);
    }

    fn pan_to(&mut self, pane_x: f64, y: f64) {
        if self.panning {
            self.engine.time_scale.scroll_to(pane_x);
        }
        self.update_crosshair(pane_x, y);
    }

    fn end_pan(&mut self, pane_x: f64, y: f64) {
        if self.panning {
            self.engine.time_scale.end_scroll();
            self.panning = false;
        }
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
        self.update_crosshair(pane_x, y);
    }

    fn clear_pointer(&mut self, cx: &mut Context<Self>) {
        if self.panning {
            self.engine.time_scale.end_scroll();
            self.panning = false;
        }
        self.engine.crosshair = None;
        self.invalidate_series_frame();
        cx.notify();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (pane_x, y) = self.local_position(event.position);
        self.begin_pan(pane_x, y);
        cx.stop_propagation();
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (pane_x, y) = self.local_position(event.position);
        if self.panning && event.dragging() {
            self.pan_to(pane_x, y);
        } else {
            self.update_crosshair(pane_x, y);
        }
        cx.notify();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let (pane_x, y) = self.local_position(event.position);
        self.end_pan(pane_x, y);
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
        let prepared = PreparedOriginFrame::new(&self.frame).with_axis(&self.axis_prims, &[]);
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

        div()
            .id("origin_chart_surface")
            .size_full()
            .bg(rgb(self.theme.colors.background.rgb_u32()))
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

    fn interactive_chart() -> OriginChartView {
        let mut chart = OriginChartView::with_theme(AxiusflowTheme::dark());
        chart.engine.recompute_layout_with_measure(true, |_| 48.0);
        chart.engine.fit_content();
        chart.engine.recompute_layout_with_measure(true, |_| 48.0);
        chart.fitted = true;
        chart
    }

    #[test]
    fn empty_chart_surface_accepts_its_first_real_snapshot() {
        let mut chart = OriginChartView::empty(AxiusflowTheme::dark());
        assert_eq!(chart.queued_replay_update_count(), 0);
        assert_eq!(chart.expected_replay_sequence(), None);
        assert_eq!(chart.replay_bridge_metrics(), ChartBridgeMetrics::default());
        assert!(chart.latest_market_provenance().is_none());

        let replay = EmbeddedReplaySource
            .execute(LoadEmbeddedReplay { bar_count: 16 })
            .expect("embedded replay validates");
        chart.load_replay(&replay).expect("first snapshot installs");
        assert_eq!(
            chart.expected_replay_sequence(),
            replay.stream().last_sequence().checked_add(1)
        );
        assert!(chart.latest_market_provenance().is_some());
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
        chart.begin_pan(-1.0, 200.0);
        assert!(!chart.panning);
        chart.begin_pan(300.0, 200.0);
        assert!(chart.panning);
        assert_eq!(chart.engine.crosshair, Some((300.0, 200.0)));
        let offset = chart.engine.right_offset();
        chart.pan_to(340.0, 200.0);
        assert!((chart.engine.right_offset() - offset).abs() > f64::EPSILON);
        chart.end_pan(340.0, 200.0);
        assert!(!chart.panning);
        chart.update_crosshair(-1.0, 200.0);
        assert!(chart.engine.crosshair.is_none());
    }
}
