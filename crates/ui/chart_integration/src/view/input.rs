//! Input.

use super::{
    ActivationRequest, ChartDrag, ChartDrawingTool, ChartType, Context, CursorStyle,
    DrawingModifiers, KEYBOARD_PAGE_FRACTION, KeyDownEvent, ModifiersChangedEvent, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, NucleusChartView, PANE_SEPARATOR_HIT, PointerInteractionState,
    PriceScaleTarget, ScrollWheelEvent, WHEEL_LINE_HEIGHT, Window, px,
    should_stop_mouse_up_propagation,
};

impl NucleusChartView {
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
    pub(super) fn update_crosshair_magnet(&mut self, magnet: bool) {
        let enabled = magnet && self.drawing_tool.drawing_kind().is_some();
        if self.engine.crosshair_ohlc_magnet != enabled {
            self.engine.crosshair_ohlc_magnet = enabled;
            self.invalidate_series_frame();
        }
    }
    pub(super) fn update_crosshair(&mut self, pane_x: f64, y: f64) {
        self.engine.crosshair = (self.separator_at(y).is_none()
            && pane_x >= 0.0
            && pane_x <= self.engine.pane_w
            && y >= 0.0
            && y <= self.engine.pane_h)
            .then_some((pane_x, y));
        self.invalidate_series_frame();
    }
    pub(super) fn separator_at(&self, y: f64) -> Option<usize> {
        self.engine
            .panes
            .iter()
            .skip(1)
            .position(|pane| (y - pane.top).abs() <= PANE_SEPARATOR_HIT)
    }
    pub(super) fn update_cursor(&mut self, pane_x: f64, y: f64) {
        let active_separator = matches!(self.drag, Some(ChartDrag::PaneSeparator { .. }));
        let separator = self.separator_at(y);
        let separator_hover = (!active_separator).then_some(separator).flatten();
        if self.engine.separator_hover != separator_hover {
            self.engine.set_separator_hover(separator_hover);
            self.invalidate_series_frame();
        }
        let drawing_cursor = (self.drawing_tool == ChartDrawingTool::Cursor
            && self.drag.is_none()
            && separator.is_none())
        .then(|| self.engine.hit_test_drawing(pane_x, y))
        .flatten()
        .map(|hit| hit.cursor);
        let hovered_series = (self.drawing_tool == ChartDrawingTool::Cursor
            && self.drag.is_none()
            && separator.is_none()
            && drawing_cursor.is_none())
        .then(|| self.engine.hit_test_series(pane_x, y))
        .flatten();
        if self.engine.hovered_series() != hovered_series {
            self.engine.set_hovered_series(hovered_series);
            self.invalidate_series_frame();
        }
        self.cursor_style = if self.engine.alert_create_hit_at(pane_x, y) {
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
            match self.drag {
                Some(ChartDrag::Pane { .. }) => CursorStyle::ClosedHand,
                Some(ChartDrag::TimeAxis) => CursorStyle::ResizeLeftRight,
                Some(ChartDrag::PriceAxis { .. }) => CursorStyle::ResizeUpDown,
                Some(ChartDrag::PaneSeparator { .. }) => CursorStyle::ResizeRow,
                None if y > self.engine.pane_h => CursorStyle::ResizeLeftRight,
                None if self.price_axis_at(pane_x, y).is_some() => CursorStyle::ResizeUpDown,
                Some(ChartDrag::BrushableRange) | None => CursorStyle::Crosshair,
            }
        };
    }
    pub(super) fn select_series_at(&mut self, pane_x: f64, y: f64) -> bool {
        let selected = self.engine.hit_test_series(pane_x, y);
        let previous_series = self.engine.selected_series();
        let previous_members = self.engine.selected_series_members().collect::<Vec<_>>();
        let previous_drawing = self.engine.selected_drawing();
        if let Some(selected) = selected {
            let study = self.study_series.iter().find_map(|((study_id, _), state)| {
                (state.series_id == selected).then_some(*study_id)
            });
            let members = study.map(|study_id| {
                self.study_series
                    .iter()
                    .filter_map(|((candidate, _), state)| {
                        (*candidate == study_id).then_some(state.series_id)
                    })
                    .collect::<Vec<_>>()
            });
            if !members
                .as_deref()
                .is_some_and(|members| self.engine.set_selected_series_group(selected, members))
            {
                self.engine.set_selected_series(Some(selected));
            }
        } else {
            self.engine.set_selected_series(None);
        }
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
    pub(super) fn unlocked_price_pan_target(&self, pane: usize) -> Option<PriceScaleTarget> {
        [PriceScaleTarget::Right, PriceScaleTarget::Left]
            .into_iter()
            .find(|&target| {
                self.engine.price_scale_auto_scale_for(pane, target) == Some(false)
                    && self.engine.price_axis_scalable(pane, target)
            })
    }
    pub(super) fn begin_price_axis_scale(&mut self, pane: usize, target: PriceScaleTarget, y: f64) {
        self.engine
            .set_price_scale_auto_scale_for(pane, target, false);
        self.engine.price_axis_start_scale(pane, target, y);
        self.drag = Some(ChartDrag::PriceAxis { pane, target });
    }
    pub(super) fn begin_drag(&mut self, pane_x: f64, y: f64, click_count: usize, shift: bool) {
        self.end_drag(pane_x, y);
        let pane = self.engine.pane_index_at_y(y);
        if click_count >= 2 {
            if y > self.engine.pane_h {
                self.engine.reset_time_scale();
            } else if self.price_axis_at(pane_x, y).is_some() {
                self.engine.reset_price_scales();
            } else if self.chart_type == ChartType::BrushableArea {
                self.clear_brushable_range();
            }
            self.update_crosshair(pane_x, y);
            return;
        }
        if let Some(index) = self.separator_at(y) {
            self.engine.set_separator_hover(None);
            self.drag = Some(ChartDrag::PaneSeparator { index, last_y: y });
        } else if y > self.engine.pane_h {
            self.engine.time_axis_start_scale(pane_x);
            self.drag = Some(ChartDrag::TimeAxis);
        } else if let Some((pane, target)) = self.price_axis_at(pane_x, y) {
            if self.engine.price_axis_scalable(pane, target) {
                self.begin_price_axis_scale(pane, target, y);
            } else {
                self.drag = None;
            }
        } else if pane_x >= 0.0 && y >= 0.0 && y <= self.engine.pane_h {
            if self.chart_type == ChartType::BrushableArea
                && self.drawing_tool == ChartDrawingTool::Cursor
                && shift
            {
                self.begin_brushable_range(pane_x, y);
                return;
            }
            self.engine.time_scale.start_scroll(pane_x);
            let price_pan = self
                .engine
                .begin_price_pan_at(pane, pane_x, y)
                .or_else(|| {
                    let target = self.unlocked_price_pan_target(pane)?;
                    self.engine.price_axis_start_scroll(pane, target, y);
                    Some(target)
                })
                .map(|target| (pane, target));
            self.drag = Some(ChartDrag::Pane { price_pan });
        } else {
            self.drag = None;
        }
        self.update_cursor(pane_x, y);
        self.update_crosshair(pane_x, y);
    }
    pub(super) fn drag_to(&mut self, pane_x: f64, y: f64) {
        match self.drag {
            Some(ChartDrag::Pane { price_pan }) => {
                self.engine.time_scale.scroll_to(pane_x);
                if let Some((pane, target)) = price_pan {
                    self.engine.price_axis_scroll_to(pane, target, y);
                }
            }
            Some(ChartDrag::BrushableRange) => {
                self.engine.delta_tooltip_mouse_move(pane_x);
                self.sync_brushable_range();
            }
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
    pub(super) fn end_drag(&mut self, pane_x: f64, y: f64) {
        match self.drag.take() {
            Some(ChartDrag::Pane { price_pan }) => {
                self.engine.time_scale.end_scroll();
                if let Some((pane, target)) = price_pan {
                    self.engine.price_axis_end_scroll(pane, target);
                }
            }
            Some(ChartDrag::BrushableRange) => {
                self.engine.delta_tooltip_mouse_up();
                self.sync_brushable_range();
            }
            Some(ChartDrag::TimeAxis) => self.engine.time_axis_end_scale(),
            Some(ChartDrag::PriceAxis { pane, target }) => {
                self.engine.price_axis_end_scale(pane, target);
            }
            Some(ChartDrag::PaneSeparator { .. }) | None => {}
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
        if normalized_y != 0.0 {
            let zoom = nucleuscharts_engine::wheel_zoom_scale(normalized_y);
            if let Some((pane, target)) = self.price_axis_at(pane_x, y) {
                self.engine.price_axis_wheel_zoom(pane, target, y, zoom);
            } else {
                self.engine.time_scale.zoom(pane_x, zoom);
            }
        }
        if normalized_x != 0.0 {
            self.engine.time_scale.start_scroll(0.0);
            self.engine
                .time_scale
                .scroll_to(nucleuscharts_engine::WHEEL_SCROLL_PX_PER_DELTA * normalized_x);
            self.engine.time_scale.end_scroll();
        }
        self.update_cursor(pane_x, y);
        self.update_crosshair(pane_x, y);
    }
    pub(super) fn clear_pointer(&mut self, cx: &mut Context<Self>) {
        self.cancel_pointer_gesture();
        cx.notify();
    }
    pub(super) fn cancel_pointer_gesture(&mut self) {
        self.end_drag(-1.0, -1.0);
        self.engine.drawing_drag_end();
        self.engine.brush_create_cancel();
        self.pending_brush_point = None;
        self.engine.crosshair_ohlc_magnet = false;
        self.engine.crosshair = None;
        self.engine.set_separator_hover(None);
        self.engine.set_hovered_series(None);
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
        if self.drag.is_some() {
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
            eprintln!("AXIUSFLOW_CHART_MOUSE_DOWN {{\"x\":{x},\"y\":{y}}}");
        }
        if let Some(focus_handle) = &self.focus_handle {
            window.focus(focus_handle, cx);
        }
        self.pending_activate = ActivationRequest::Pending;
        self.update_crosshair_magnet(event.modifiers.control || event.modifiers.platform);
        let (pane_x, y) = self.local_position(event.position);
        if self.engine.activate_alert_create_at(pane_x, y) {
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
        cx.notify();
    }
    pub(super) fn finish_mouse_up(&mut self, event: &MouseUpEvent) {
        let (pane_x, y) = self.local_position(event.position);
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
