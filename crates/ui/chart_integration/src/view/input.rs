//! Input.

use super::{
    ActivationRequest, AerisChartView, ChartDrag, ChartDrawingTool, ChartType, Context,
    CursorStyle, DrawingModifiers, FinancialDrag, FinancialNavigation, KeyDownEvent,
    ModifiersChangedEvent, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PANE_SEPARATOR_HIT,
    PointerInteractionState, PriceScaleTarget, ScrollWheelEvent, TradingTooltipDwell,
    WHEEL_LINE_HEIGHT, Window, px, should_stop_mouse_up_propagation,
};
use aeris_charts_engine::{TradingCursor, TradingHitKind};

/// Matches the browser host's dwell before a close control reveals its action tooltip.
const TRADING_TOOLTIP_DWELL: std::time::Duration = std::time::Duration::from_millis(450);

/// GPUI's Windows backend currently falls back to the arrow for both hand cursors. Price lines
/// move only on the Y axis, so use the native vertical-resize affordance there instead of
/// presenting a draggable trading line as inert.
#[cfg(target_os = "windows")]
pub(super) const fn trading_line_cursor() -> CursorStyle {
    CursorStyle::ResizeUpDown
}

#[cfg(not(target_os = "windows"))]
pub(super) const fn trading_line_cursor() -> CursorStyle {
    CursorStyle::OpenHand
}

#[cfg(target_os = "windows")]
pub(super) const fn trading_line_drag_cursor() -> CursorStyle {
    CursorStyle::ResizeUpDown
}

#[cfg(not(target_os = "windows"))]
pub(super) const fn trading_line_drag_cursor() -> CursorStyle {
    CursorStyle::ClosedHand
}

impl AerisChartView {
    /// Cancels transient pointer state before a host-owned modal occludes the chart.
    pub fn suspend_pointer_interaction(&mut self) {
        self.pointer_interaction = PointerInteractionState::Suspended;
        self.cancel_pointer_gesture();
        self.cursor_style = CursorStyle::Arrow;
    }

    /// Restores pointer handling after the host-owned modal has closed.
    pub fn resume_pointer_interaction(&mut self) {
        self.pointer_interaction = PointerInteractionState::Active;
        self.cursor_style = CursorStyle::Crosshair;
        self.invalidate_series_frame();
    }

    pub(super) fn local_position(&self, position: gpui::Point<gpui::Pixels>) -> (f64, f64) {
        let window_x: f32 = position.x.into();
        let window_y: f32 = position.y.into();
        let chart_x = f64::from(window_x - self.viewport_origin.0);
        let y = f64::from(window_y - self.viewport_origin.1);
        (chart_x - self.engine.pane_left, y)
    }

    pub(super) fn position_is_inside_viewport(&self, position: gpui::Point<gpui::Pixels>) -> bool {
        let x: f32 = position.x.into();
        let y: f32 = position.y.into();
        let (width, height, _) = self.built_for;
        width > 0.0
            && height > 0.0
            && x >= self.viewport_origin.0
            && x <= self.viewport_origin.0 + width
            && y >= self.viewport_origin.1
            && y <= self.viewport_origin.1 + height
    }
    pub(super) fn update_crosshair_magnet(&mut self, magnet: bool) {
        if self.engine.set_crosshair_ohlc_magnet(magnet) {
            self.invalidate_series_frame();
        }
    }
    pub(super) fn update_crosshair(&mut self, pane_x: f64, y: f64) {
        self.engine
            .update_financial_crosshair(pane_x, y, PANE_SEPARATOR_HIT);
        self.invalidate_series_frame();
    }
    pub(super) fn separator_at(&self, y: f64) -> Option<usize> {
        self.engine.pane_separator_at(y, PANE_SEPARATOR_HIT)
    }
    pub(super) fn update_cursor(&mut self, pane_x: f64, y: f64) {
        let native_drag = self.engine.financial_drag();
        let active_separator = matches!(native_drag, Some(FinancialDrag::PaneSeparator { .. }));
        let separator = self.separator_at(y);
        let separator_hover = (!active_separator).then_some(separator).flatten();
        if self.engine.separator_hover() != separator_hover {
            self.engine.set_separator_hover(separator_hover);
            self.invalidate_series_frame();
        }
        let drawing_cursor = (self.drawing_tool == ChartDrawingTool::Cursor
            && self.drag.is_none()
            && native_drag.is_none()
            && separator.is_none())
        .then(|| self.engine.hit_test_drawing(pane_x, y))
        .flatten()
        .map(|hit| hit.cursor);
        let trading_idle = self.drawing_tool == ChartDrawingTool::Cursor
            && self.drag.is_none()
            && native_drag.is_none()
            && separator.is_none()
            && !self.engine.drawing_drag_active()
            && self.engine.trading_preview().is_none();
        let trading_cursor = if trading_idle {
            self.update_trading_hover(pane_x, y);
            self.engine.trading_cursor_at(pane_x, y)
        } else {
            None
        };
        let hovered_series = (self.drawing_tool == ChartDrawingTool::Cursor
            && self.drag.is_none()
            && native_drag.is_none()
            && separator.is_none()
            && drawing_cursor.is_none())
        .then(|| self.engine.hit_test_series(pane_x, y))
        .flatten();
        if self.engine.hovered_series() != hovered_series {
            self.engine.set_hovered_series(hovered_series);
            self.invalidate_series_frame();
        }
        self.cursor_style = if self.engine.trading_preview().is_some() {
            trading_line_drag_cursor()
        } else if let Some(cursor) = trading_cursor {
            match cursor {
                TradingCursor::Grab => trading_line_cursor(),
                TradingCursor::Pointer => CursorStyle::PointingHand,
            }
        } else if self.engine.alert_create_hit_at(pane_x, y) {
            CursorStyle::PointingHand
        } else if active_separator || separator.is_some() {
            CursorStyle::ResizeRow
        } else if self.engine.drawing_drag_active() {
            CursorStyle::ClosedHand
        } else if self.drawing_tool != ChartDrawingTool::Cursor {
            CursorStyle::Crosshair
        } else if let Some(cursor) = drawing_cursor {
            match cursor {
                "pointer" => CursorStyle::PointingHand,
                "move" => CursorStyle::OpenHand,
                "ns-resize" => CursorStyle::ResizeUpDown,
                "ew-resize" => CursorStyle::ResizeLeftRight,
                "nwse-resize" => CursorStyle::ResizeUpRightDownLeft,
                "nesw-resize" => CursorStyle::ResizeUpLeftDownRight,
                _ => CursorStyle::Crosshair,
            }
        } else if hovered_series.is_some() {
            CursorStyle::PointingHand
        } else {
            match native_drag {
                Some(FinancialDrag::Pane { .. }) => CursorStyle::ClosedHand,
                Some(FinancialDrag::TimeAxis) => CursorStyle::ResizeLeftRight,
                Some(FinancialDrag::PriceAxis { .. }) => CursorStyle::ResizeUpDown,
                Some(FinancialDrag::PaneSeparator { .. }) => CursorStyle::ResizeRow,
                None if y > self.engine.pane_h => CursorStyle::ResizeLeftRight,
                None if self.price_axis_at(pane_x, y).is_some() => CursorStyle::ResizeUpDown,
                None => CursorStyle::Crosshair,
            }
        };
    }
    /// Mirrors the browser host: a changed hover repaints, and landing on a close control
    /// starts the tooltip dwell once the caller has a context to time it.
    fn update_trading_hover(&mut self, pane_x: f64, y: f64) {
        if !self.engine.set_trading_hover(pane_x, y) {
            return;
        }
        self.invalidate_series_frame();
        self.trading_tooltip = if self
            .engine
            .trading_hit_at(pane_x, y)
            .is_some_and(|hit| hit.kind == TradingHitKind::CancelButton)
        {
            TradingTooltipDwell::Pending
        } else {
            TradingTooltipDwell::Idle
        };
    }
    fn schedule_trading_tooltip(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.trading_tooltip, TradingTooltipDwell::Pending) {
            return;
        }
        self.trading_tooltip = TradingTooltipDwell::Waiting {
            _task: cx.spawn(async move |chart, cx| {
                cx.background_executor().timer(TRADING_TOOLTIP_DWELL).await;
                let _ = chart.update(cx, |chart, chart_cx| {
                    chart.trading_tooltip = TradingTooltipDwell::Idle;
                    if chart.engine.arm_trading_tooltip() {
                        chart.invalidate_series_frame();
                        chart_cx.notify();
                    }
                });
            }),
        };
    }
    pub(super) fn select_series_at(&mut self, pane_x: f64, y: f64) -> bool {
        let selected = self.engine.hit_test_series(pane_x, y);
        let previous_series = self.engine.selected_series();
        let previous_members = self.engine.selected_series_members().collect::<Vec<_>>();
        let previous_drawing = self.engine.selected_drawing();
        self.engine.set_selected_series(selected);
        if selected.is_some() {
            self.engine.set_selected_drawing(None);
        }
        let selected_members = self.engine.selected_series_members().collect::<Vec<_>>();
        if previous_series != selected
            || previous_members != selected_members
            || (selected.is_some() && previous_drawing.is_some())
        {
            self.invalidate_series_frame();
        }
        selected.is_some()
    }
    pub(super) fn pointer_on_axis(&self, pane_x: f64, y: f64) -> bool {
        y > self.engine.pane_h || self.price_axis_at(pane_x, y).is_some()
    }
    pub(super) fn price_axis_at(&self, pane_x: f64, y: f64) -> Option<(usize, PriceScaleTarget)> {
        if y > self.engine.pane_h {
            return None;
        }
        let pane = self.engine.pane_index_at_y(y);
        self.engine
            .price_axis_target_at(pane, pane_x)
            .map(|target| (pane, target))
    }
    pub(super) fn begin_drag(&mut self, pane_x: f64, y: f64, click_count: usize, shift: bool) {
        self.end_drag(pane_x, y);
        if click_count >= 2
            && y <= self.engine.pane_h
            && self.price_axis_at(pane_x, y).is_none()
            && self.chart_type == ChartType::BrushableArea
        {
            self.clear_brushable_range();
            self.update_crosshair(pane_x, y);
            return;
        }
        if self.chart_type == ChartType::BrushableArea
            && self.drawing_tool == ChartDrawingTool::Cursor
            && shift
            && pane_x >= 0.0
            && y >= 0.0
            && y <= self.engine.pane_h
        {
            self.begin_brushable_range(pane_x, y);
            return;
        }
        let _ = self
            .engine
            .begin_financial_drag(pane_x, y, click_count, PANE_SEPARATOR_HIT);
        self.update_cursor(pane_x, y);
        self.update_crosshair(pane_x, y);
    }
    pub(super) fn drag_to(&mut self, pane_x: f64, y: f64) {
        match self.drag {
            Some(ChartDrag::BrushableRange) => {
                self.engine.delta_tooltip_mouse_move(pane_x);
                self.sync_brushable_range();
            }
            None if self.engine.update_financial_drag(pane_x, y) => {
                self.invalidate_series_layout();
            }
            None => {}
        }
        self.update_cursor(pane_x, y);
        if matches!(
            self.engine.financial_drag(),
            Some(FinancialDrag::PaneSeparator { .. })
        ) {
            self.engine.clear_crosshair_at();
            self.invalidate_series_frame();
        } else {
            self.update_crosshair(pane_x, y);
        }
    }
    pub(super) fn end_drag(&mut self, pane_x: f64, y: f64) {
        match self.drag.take() {
            Some(ChartDrag::BrushableRange) => {
                self.engine.delta_tooltip_mouse_up();
                self.sync_brushable_range();
            }
            None => self.engine.end_financial_drag(),
        }
        self.update_cursor(pane_x, y);
        self.update_crosshair(pane_x, y);
    }
    pub(super) fn apply_wheel(
        &mut self,
        pane_x: f64,
        y: f64,
        normalized_x: f64,
        normalized_y: f64,
    ) {
        self.engine
            .apply_financial_wheel(pane_x, y, normalized_x, normalized_y);
        self.update_cursor(pane_x, y);
        self.update_crosshair(pane_x, y);
    }
    pub(super) fn clear_pointer(&mut self, cx: &mut Context<Self>) {
        self.cancel_pointer_gesture();
        cx.notify();
    }
    pub(super) fn cancel_pointer_gesture(&mut self) {
        self.end_drag(-1.0, -1.0);
        self.engine.end_financial_drag();
        self.engine.drawing_drag_end();
        self.engine.brush_create_cancel();
        self.pending_brush_point = None;
        let _ = self.engine.set_crosshair_ohlc_magnet(false);
        self.engine.clear_crosshair_at();
        self.engine.set_separator_hover(None);
        self.engine.set_hovered_series(None);
        self.engine.clear_trading_hover();
        self.engine.clear_trading_pressed();
        self.trading_tooltip = TradingTooltipDwell::Idle;
        self.cursor_style = CursorStyle::Crosshair;
        self.invalidate_series_frame();
    }
    pub(super) fn move_pointer(
        &mut self,
        pane_x: f64,
        y: f64,
        dragging: bool,
        modifiers: DrawingModifiers,
    ) {
        if self.engine.trading_preview().is_some() {
            if dragging {
                self.engine.trading_drag_to(y);
            } else {
                self.engine.cancel_trading_drag();
            }
            self.update_cursor(pane_x, y);
            self.update_crosshair(pane_x, y);
            return;
        }
        if self.drag.is_some() || self.engine.financial_drag().is_some() {
            if dragging {
                self.drag_to(pane_x, y);
            } else {
                self.end_drag(pane_x, y);
            }
            return;
        }
        if self.drawing_pointer_move(pane_x, y, dragging, modifiers) {
            self.update_crosshair(pane_x, y);
            return;
        }
        self.update_cursor(pane_x, y);
        self.update_crosshair(pane_x, y);
    }
    pub(super) fn apply_key(&mut self, key: &str, accelerated: bool) -> bool {
        match key {
            "left" => self
                .engine
                .apply_financial_navigation(FinancialNavigation::PreviousBar, accelerated),
            "right" => self
                .engine
                .apply_financial_navigation(FinancialNavigation::NextBar, accelerated),
            "pageup" => self
                .engine
                .apply_financial_navigation(FinancialNavigation::PreviousPage, accelerated),
            "pagedown" => self
                .engine
                .apply_financial_navigation(FinancialNavigation::NextPage, accelerated),
            "+" | "=" => self
                .engine
                .apply_financial_navigation(FinancialNavigation::ZoomIn, accelerated),
            "-" | "_" => self
                .engine
                .apply_financial_navigation(FinancialNavigation::ZoomOut, accelerated),
            "home" => self.reset_view(),
            "end" => self.scroll_to_latest(),
            "enter" => {
                if !self.finish_path_creation() {
                    return false;
                }
            }
            "backspace"
                if self.drawing_tool == ChartDrawingTool::Path
                    && self.engine.drawing_create_active() =>
            {
                if !self.pop_path_anchor() {
                    return false;
                }
            }
            "delete" | "backspace" => {
                if !self.remove_selected_chart_object() {
                    return false;
                }
            }
            "escape" => self.cancel_drawing(),
            _ => return false,
        }
        self.invalidate_series_frame();
        true
    }
    pub(super) fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_interaction == PointerInteractionState::Suspended {
            cx.stop_propagation();
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
        self.update_crosshair_magnet(event.modifiers.control || event.modifiers.platform);
        let (pane_x, y) = self.local_position(event.position);
        self.end_drag(pane_x, y);
        self.trading_tooltip = TradingTooltipDwell::Idle;
        if !self.pointer_on_axis(pane_x, y) && self.engine.set_trading_pressed(pane_x, y) {
            self.invalidate_series_frame();
        }
        if self.engine.trading_activate_at(pane_x, y) {
            self.drag = None;
            self.cursor_style = CursorStyle::PointingHand;
            self.invalidate_series_frame();
        } else if !self.pointer_on_axis(pane_x, y) && self.engine.trading_drag_start_at(pane_x, y) {
            self.drag = None;
            self.cursor_style = trading_line_drag_cursor();
            self.invalidate_series_frame();
        } else if self.engine.activate_alert_create_at(pane_x, y) {
            self.drag = None;
            self.cursor_style = CursorStyle::PointingHand;
            self.invalidate_series_frame();
        } else if self.separator_at(y).is_some() {
            self.begin_drag(pane_x, y, event.click_count, event.modifiers.shift);
        } else if !self.pointer_on_axis(pane_x, y)
            && self.drawing_pointer_down(
                pane_x,
                y,
                Self::drawing_modifiers(event.modifiers),
                event.click_count,
            )
        {
            self.engine.set_selected_series(None);
            self.update_cursor(pane_x, y);
            self.update_crosshair(pane_x, y);
        } else if !self.pointer_on_axis(pane_x, y)
            && self.drawing_tool == ChartDrawingTool::Cursor
            && self.chart_type != ChartType::BrushableArea
            && self.select_series_at(pane_x, y)
        {
            self.update_cursor(pane_x, y);
            self.update_crosshair(pane_x, y);
        } else {
            self.begin_drag(pane_x, y, event.click_count, event.modifiers.shift);
        }
        cx.stop_propagation();
        cx.notify();
    }
    pub(super) fn on_context_menu(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_interaction == PointerInteractionState::Suspended {
            cx.stop_propagation();
            return;
        }
        if let Some(focus_handle) = &self.focus_handle {
            window.focus(focus_handle, cx);
        }
        self.pending_activate = ActivationRequest::Pending;
        let _ = self.finish_text_edit();
        self.cancel_pointer_gesture();
        let (pane_x, y) = self.local_position(event.position);
        self.pending_context_menu = Some(self.context_menu_request(event.position, pane_x, y));
        cx.stop_propagation();
        cx.notify();
    }
    pub(super) fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_interaction == PointerInteractionState::Suspended {
            return;
        }
        if self.apply_text_edit_key(event) {
            window.prevent_default();
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let modifiers = event.keystroke.modifiers;
        if self.apply_key(
            event.keystroke.key.as_str(),
            modifiers.control || modifiers.shift,
        ) {
            cx.stop_propagation();
            cx.notify();
        }
    }
    pub(super) fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_interaction == PointerInteractionState::Suspended {
            return;
        }
        self.update_crosshair_magnet(event.modifiers.control || event.modifiers.platform);
        cx.notify();
    }
    pub(super) fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_interaction == PointerInteractionState::Suspended {
            return;
        }
        self.update_crosshair_magnet(event.modifiers.control || event.modifiers.platform);
        let (pane_x, y) = self.local_position(event.position);
        self.move_pointer(
            pane_x,
            y,
            event.dragging(),
            Self::drawing_modifiers(event.modifiers),
        );
        self.schedule_trading_tooltip(cx);
        cx.notify();
    }
    pub(super) fn finish_mouse_up(&mut self, event: &MouseUpEvent) {
        let (pane_x, y) = self.local_position(event.position);
        if self.engine.clear_trading_pressed() {
            self.invalidate_series_frame();
        }
        if self.engine.trading_preview().is_some() {
            self.engine.trading_drag_end();
            self.invalidate_series_frame();
            return;
        }
        if !self.drawing_pointer_up(pane_x, y, Self::drawing_modifiers(event.modifiers)) {
            self.end_drag(pane_x, y);
        }
    }
    pub(super) fn on_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_interaction == PointerInteractionState::Suspended {
            return;
        }
        self.finish_mouse_up(event);
        if should_stop_mouse_up_propagation(false) {
            cx.stop_propagation();
        }
        cx.notify();
    }
    pub(super) fn on_mouse_up_out(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_interaction == PointerInteractionState::Suspended {
            return;
        }
        // GPUI dispatches `on_mouse_up_out` in capture phase when this hitbox is
        // not hovered. An occluding menu also makes the chart hitbox non-hovered,
        // even though the release is still geometrically inside the chart. Treat
        // only a real release outside the chart rectangle as a lost mouse-up.
        if self.position_is_inside_viewport(event.position) {
            return;
        }
        self.finish_mouse_up(event);
        if should_stop_mouse_up_propagation(true) {
            cx.stop_propagation();
        }
        cx.notify();
    }
    pub(super) fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_interaction == PointerInteractionState::Suspended {
            return;
        }
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
}
