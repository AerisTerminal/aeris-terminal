//! GPUI listeners. Every interaction decision is made by the Aeris Charts input controller; this
//! module binds the shared GPUI adapter and turns the engine's host requests into product state.

use super::{
    ActivationRequest, AerisChartView, ChartContextKind, ChartContextRequest, Context,
    KeyDownEvent, ModifiersChangedEvent, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PointerInteractionState, ScrollWheelEvent, SharedString, Window, px,
};
use aeris_charts_engine::{ChartContextMenu, ChartInputEvent, ChartRegion, PriceScaleTarget};
use aeris_charts_render_gpui::input::cursor_style;
use gpui::{CursorStyle, KeyUpEvent, PinchEvent, Point};
use num_traits::ToPrimitive;

impl AerisChartView {
    /// Cancels transient pointer state before a host-owned modal occludes the chart.
    pub fn suspend_pointer_interaction(&mut self) {
        self.pointer_interaction = PointerInteractionState::Suspended;
        self.engine.input_cancel();
    }

    /// Restores pointer handling after the host-owned modal has closed.
    pub fn resume_pointer_interaction(&mut self) {
        self.pointer_interaction = PointerInteractionState::Active;
        self.invalidate_series_frame();
    }

    fn pointer_suspended(&self) -> bool {
        self.pointer_interaction == PointerInteractionState::Suspended
    }

    /// The platform cursor for the engine's pointer feedback; inert while a host modal occludes
    /// the chart.
    pub(super) fn cursor_style(&self) -> CursorStyle {
        if self.pointer_suspended() {
            CursorStyle::Arrow
        } else {
            cursor_style(self.engine.input_cursor())
        }
    }

    /// Turns the engine's queued host requests into product requests the shell takes.
    pub(super) fn process_input_events(&mut self) {
        for event in self.engine.take_input_events() {
            match event {
                ChartInputEvent::ContextMenu(menu) => {
                    self.pending_context_menu = Some(self.context_menu_request(menu));
                }
                // Terminal has no pane click, crosshair, or drawing-created subscribers, and the
                // engine runs drawing text editing through the GPUI input adapter.
                ChartInputEvent::Click { .. }
                | ChartInputEvent::DoubleClick { .. }
                | ChartInputEvent::CrosshairLeft
                | ChartInputEvent::TextEditorOpened(_)
                | ChartInputEvent::DrawingCreated(_) => {}
                ChartInputEvent::RemoveSeries(series) => {
                    if self.remove_series_selection(series) {
                        self.engine.set_selected_series(None);
                    }
                }
            }
        }
    }

    fn context_menu_request(&self, menu: ChartContextMenu) -> ChartContextRequest {
        let kind = match menu.region {
            ChartRegion::PriceAxis {
                pane,
                target: PriceScaleTarget::Left,
            } => ChartContextKind::PriceAxis { pane, left: true },
            ChartRegion::PriceAxis {
                pane,
                target: PriceScaleTarget::Right,
            } => ChartContextKind::PriceAxis { pane, left: false },
            _ => ChartContextKind::Pane,
        };
        let copy_price = menu.context.and_then(|context| {
            self.engine
                .series_format_price(context.series.unwrap_or(0), context.price)
                .map(SharedString::from)
        });
        let origin = self.viewport_bounds.origin;
        let x = (menu.x + self.engine.pane_left)
            .to_f32()
            .unwrap_or_default();
        let y = menu.y.to_f32().unwrap_or_default();
        ChartContextRequest {
            position: Point::new(origin.x + px(x), origin.y + px(y)),
            kind,
            copy_price,
        }
    }

    /// Common tail of every listener: process engine requests, schedule deferred engine work, and
    /// repaint.
    fn after_input(&mut self, cx: &mut Context<Self>) {
        self.process_input_events();
        self.input_wake = self.input.wake_delay(&self.engine).map(|delay| {
            cx.spawn(async move |chart, cx| {
                cx.background_executor().timer(delay).await;
                let _ = chart.update(cx, |chart, chart_cx| {
                    chart.input_wake = None;
                    chart_cx.notify();
                });
            })
        });
        cx.notify();
    }

    pub(super) fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        if self.pointer_suspended() {
            return;
        }
        #[cfg(feature = "diagnostics")]
        if self.live_evidence_enabled && self.live_evidence_mouse_downs < 8 {
            self.live_evidence_mouse_downs = self.live_evidence_mouse_downs.saturating_add(1);
            let x: f32 = event.position.x.into();
            let y: f32 = event.position.y.into();
            eprintln!("AERIS_CHART_MOUSE_DOWN {{\"x\":{x},\"y\":{y}}}");
        }
        if let Some(focus_handle) = &self.focus_handle {
            window.focus(focus_handle, cx);
        }
        self.pending_activate = ActivationRequest::Pending;
        self.input.mouse_down(&mut self.engine, event);
        self.after_input(cx);
    }

    pub(super) fn on_context_menu(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        if self.pointer_suspended() {
            return;
        }
        if let Some(focus_handle) = &self.focus_handle {
            window.focus(focus_handle, cx);
        }
        self.pending_activate = ActivationRequest::Pending;
        self.input.context_menu(&mut self.engine, event);
        self.after_input(cx);
    }

    pub(super) fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_suspended() {
            return;
        }
        self.input.mouse_move(&mut self.engine, event);
        self.after_input(cx);
    }

    pub(super) fn on_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_suspended() {
            return;
        }
        self.input.mouse_up(&mut self.engine, event);
        cx.stop_propagation();
        self.after_input(cx);
    }

    /// GPUI dispatches `on_mouse_up_out` whenever this hitbox is not hovered, which includes a
    /// release inside the chart while an Aeris menu occludes it. Only a release geometrically
    /// outside the chart is a lost release; the window keeps receiving it.
    pub(super) fn on_mouse_up_out(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_suspended() || !self.release_outside_chart(event.position) {
            return;
        }
        self.input.mouse_up(&mut self.engine, event);
        self.after_input(cx);
    }

    /// Whether a release lies outside the chart canvas, with the same edge rule GPUI hitboxes use.
    pub(super) fn release_outside_chart(&self, position: Point<gpui::Pixels>) -> bool {
        !self.viewport_bounds.contains(&position)
    }

    pub(super) fn on_pointer_left(&mut self, cx: &mut Context<Self>) {
        self.engine.input_pointer_leave();
        self.after_input(cx);
    }

    pub(super) fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_suspended() {
            return;
        }
        if self.input.scroll_wheel(&mut self.engine, event) {
            cx.stop_propagation();
            self.after_input(cx);
        }
    }

    /// Trackpad pinch zooms around the pinch point; the engine owns the anchor rule.
    pub(super) fn on_pinch(
        &mut self,
        event: &PinchEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_suspended() {
            return;
        }
        if self.input.pinch(&mut self.engine, event) {
            cx.stop_propagation();
            self.after_input(cx);
        }
    }

    pub(super) fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_suspended() {
            return;
        }
        let editing = self.engine.drawing_text_edit().is_some();
        if self.input.key_down(&mut self.engine, event, cx) {
            if editing {
                window.prevent_default();
            }
            cx.stop_propagation();
            self.after_input(cx);
        }
    }

    pub(super) fn on_key_up(
        &mut self,
        event: &KeyUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.input.key_up(&mut self.engine, event) {
            cx.stop_propagation();
            self.after_input(cx);
        }
    }

    pub(super) fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_suspended() {
            return;
        }
        self.input.modifiers_changed(&mut self.engine, event);
        self.after_input(cx);
    }
}
