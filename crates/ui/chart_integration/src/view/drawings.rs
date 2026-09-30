//! Drawings.

use super::{
    AerisChartView, ChartDrawingTool, CursorStyle, DrawingId, DrawingKind, DrawingModifiers,
    DrawingTextEditKey, DrawingsLockSummary, KeyDownEvent, Modifiers, text_edit_text,
};

impl AerisChartView {
    /// Returns the drawing tool currently armed on the chart surface.
    #[must_use]
    pub const fn drawing_tool(&self) -> ChartDrawingTool {
        self.drawing_tool
    }
    /// Arms a drawing tool, replacing any unfinished drawing gesture.
    ///
    /// Creation stays idle until the first click. Starting it on arm would paint a
    /// pre-click handle that magnet-snaps instead of the OHLC crosshair.
    pub fn set_drawing_tool(&mut self, tool: ChartDrawingTool) {
        self.end_drag(-1.0, -1.0);
        let _ = self.finish_text_edit();
        self.cancel_drawing_gesture();
        self.drawing_tool = tool;
        self.cursor_style = CursorStyle::Crosshair;
        self.invalidate_series_frame();
    }
    /// Cancels creation or movement and returns to the cursor tool.
    pub fn cancel_drawing(&mut self) {
        self.end_drag(-1.0, -1.0);
        let _ = self.finish_text_edit();
        self.cancel_drawing_gesture();
        self.drawing_tool = ChartDrawingTool::Cursor;
        self.cursor_style = CursorStyle::Crosshair;
        self.invalidate_series_frame();
    }
    /// Whether Aeris Charts is editing text on a drawing.
    #[must_use]
    pub fn is_editing_text(&self) -> bool {
        self.engine.drawing_text_edit().is_some()
    }
    pub(super) fn begin_text_edit(&mut self, id: aeris_charts_engine::DrawingId) {
        if self.engine.begin_drawing_text_edit(id, true) {
            self.engine.set_selected_drawing(Some(id));
            self.invalidate_series_frame();
        }
    }
    /// Commit the active text edit. The engine removes empty standalone text, but keeps an
    /// unlabeled trend line.
    pub fn finish_text_edit(&mut self) -> bool {
        let Some((id, _, _)) = self.engine.drawing_text_edit() else {
            return false;
        };
        if self.engine.commit_drawing_text_edit() {
            if self.engine.drawing(id).is_none() {
                self.locked_drawings.remove(&id);
            }
        } else {
            return false;
        }
        self.invalidate_series_frame();
        self.mark_user_state_changed();
        true
    }
    pub(super) fn cancel_text_edit(&mut self) -> bool {
        let Some((id, _, _)) = self.engine.drawing_text_edit() else {
            return false;
        };
        if !self.engine.cancel_drawing_text_edit() {
            return false;
        }
        if self.engine.drawing(id).is_none() {
            self.locked_drawings.remove(&id);
        }
        self.invalidate_series_frame();
        self.mark_user_state_changed();
        true
    }
    pub(super) fn apply_text_edit_key(&mut self, event: &KeyDownEvent) -> bool {
        if !self.is_editing_text() {
            return false;
        }
        let modifiers = event.keystroke.modifiers;
        let word = if cfg!(target_os = "macos") {
            modifiers.alt
        } else {
            modifiers.control && !event.prefer_character_input
        };
        let line = cfg!(target_os = "macos") && modifiers.platform;
        let key = match event.keystroke.key.as_str() {
            "escape" => return self.cancel_text_edit(),
            "enter" => return self.finish_text_edit(),
            "a" if (modifiers.control || modifiers.platform)
                && !modifiers.alt
                && !event.prefer_character_input =>
            {
                return self.engine.drawing_text_edit_select_all() || self.is_editing_text();
            }
            "backspace" if word => Some(DrawingTextEditKey::DeleteWordBackward),
            "delete" if word => Some(DrawingTextEditKey::DeleteWordForward),
            "backspace" => Some(DrawingTextEditKey::Backspace),
            "delete" => Some(DrawingTextEditKey::Delete),
            "left" if line => Some(DrawingTextEditKey::Home),
            "right" if line => Some(DrawingTextEditKey::End),
            "left" if word => Some(DrawingTextEditKey::WordLeft),
            "right" if word => Some(DrawingTextEditKey::WordRight),
            "left" => Some(DrawingTextEditKey::Left),
            "right" => Some(DrawingTextEditKey::Right),
            "home" | "up" => Some(DrawingTextEditKey::Home),
            "end" | "down" => Some(DrawingTextEditKey::End),
            _ => None,
        };
        if let Some(key) = key {
            let _ = self.engine.drawing_text_edit_key(key, modifiers.shift);
            self.invalidate_series_frame();
        } else if !modifiers.function
            && (event.prefer_character_input
                || (!modifiers.control && !modifiers.platform && !modifiers.alt))
            && let Some(text) = text_edit_text(event)
        {
            let _ = self.engine.drawing_text_edit_insert(&text);
            self.invalidate_series_frame();
        }
        true
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
            self.mark_user_state_changed();
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
            self.mark_user_state_changed();
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
            self.mark_user_state_changed();
        }
        removed
    }
    /// Removes every committed drawing.
    pub fn clear_drawings(&mut self) {
        let changed = !self.engine.drawings().is_empty();
        let _ = self.finish_text_edit();
        self.cancel_drawing_gesture();
        self.engine.clear_drawings();
        self.locked_drawings.clear();
        self.invalidate_series_frame();
        if changed {
            self.mark_user_state_changed();
        }
    }
    /// Returns whether a committed drawing edit can be reversed.
    #[must_use]
    pub fn can_undo_drawing(&self) -> bool {
        self.engine.can_undo_drawing()
    }
    /// Returns whether a reversed drawing edit can be reapplied.
    #[must_use]
    pub fn can_redo_drawing(&self) -> bool {
        self.engine.can_redo_drawing()
    }
    /// Reverses the newest committed drawing edit.
    pub fn undo_drawing(&mut self) -> bool {
        self.step_drawing_history(true)
    }
    /// Reapplies the newest reversed drawing edit.
    pub fn redo_drawing(&mut self) -> bool {
        self.step_drawing_history(false)
    }
    /// Steps the engine's chart-local drawing history one command in either direction.
    ///
    /// An in-flight gesture is settled first: the engine drops its own pending creation and brush
    /// capture on a step. The toolbar selection stays armed; the next click starts a fresh
    /// placement so Ctrl magnet can still snap the crosshair instead of a leftover handle.
    /// Locks are keyed by drawing id and are host state, so a tombstoned id is dropped and a
    /// restored drawing comes back unlocked.
    pub(super) fn step_drawing_history(&mut self, undo: bool) -> bool {
        let _ = self.finish_text_edit();
        self.cancel_drawing_gesture();
        let stepped = if undo {
            self.engine.undo_drawing()
        } else {
            self.engine.redo_drawing()
        };
        if !stepped {
            return false;
        }
        self.locked_drawings.retain(|locked| {
            self.engine
                .drawings()
                .iter()
                .any(|drawing| drawing.id == *locked)
        });
        self.invalidate_series_frame();
        self.mark_user_state_changed();
        true
    }
    pub(super) fn drawing_modifiers(modifiers: Modifiers) -> DrawingModifiers {
        DrawingModifiers {
            magnet: modifiers.control || modifiers.platform,
            straighten: modifiers.shift,
        }
    }
    pub(super) fn place_drawing_anchor(
        &mut self,
        x: f64,
        y: f64,
        modifiers: DrawingModifiers,
    ) -> i64 {
        let Some(kind) = self
            .drawing_tool
            .drawing_kind()
            .filter(|kind| *kind != DrawingKind::Brush)
        else {
            return 0;
        };
        if !self.engine.drawing_create_active() && !self.engine.drawing_create_begin(kind, None) {
            return 0;
        }
        self.engine.drawing_create_click(x, y, modifiers)
    }
    pub(super) fn cancel_drawing_gesture(&mut self) {
        self.engine.drawing_drag_end();
        self.engine.drawing_create_cancel();
        self.engine.brush_create_cancel();
        self.pending_brush_point = None;
    }
    pub(super) fn finish_path_creation(&mut self) -> bool {
        if self.drawing_tool != ChartDrawingTool::Path {
            return false;
        }
        let id = self.engine.drawing_create_finish();
        if id == 0 {
            return false;
        }
        self.drawing_tool = ChartDrawingTool::Cursor;
        self.cursor_style = CursorStyle::Crosshair;
        let _ = self.engine.set_crosshair_ohlc_magnet(false);
        self.invalidate_series_frame();
        self.mark_user_state_changed();
        true
    }
    pub(super) fn pop_path_anchor(&mut self) -> bool {
        if self.drawing_tool != ChartDrawingTool::Path {
            return false;
        }
        let changed = self.engine.drawing_create_pop_anchor();
        if changed {
            self.invalidate_series_frame();
        }
        changed
    }
    /// Capture at most the newest brush sample per painted frame.
    ///
    /// Wayland delivers per-HID-report motion, often one axis per event. Adding
    /// every sample records that staircase as stroke knots. Browser hosts
    /// coalesce to display frames; this GPUI host does the same.
    pub(super) fn flush_pending_brush(&mut self) -> bool {
        let Some((x, y)) = self.pending_brush_point.take() else {
            return false;
        };
        if !self.engine.brush_create_active() {
            return false;
        }
        if self.engine.brush_create_add(x, y) {
            self.invalidate_series_frame();
            true
        } else {
            false
        }
    }
    pub(super) fn drawing_pointer_down(
        &mut self,
        pane_x: f64,
        y: f64,
        modifiers: DrawingModifiers,
        click_count: usize,
    ) -> bool {
        if self.is_editing_text() {
            let editing = self.engine.editing_drawing();
            let hit_id = self.engine.hit_test_drawing(pane_x, y).map(|hit| hit.id);
            let on_text = editing.is_some_and(|id| {
                let on_label = self.engine.drawing_text_hit_at(pane_x, y) == Some(id);
                let on_standalone_text = hit_id == Some(id)
                    && self
                        .engine
                        .drawing(id)
                        .is_some_and(|drawing| drawing.kind == DrawingKind::Text);
                on_label || on_standalone_text
            });
            if on_text {
                let _ = self.engine.drawing_text_edit_caret_at(pane_x, y);
                self.invalidate_series_frame();
                return true;
            }
            let _ = self.finish_text_edit();
        }

        match self.drawing_tool {
            ChartDrawingTool::Cursor => {
                if let Some(id) = self.engine.drawing_text_hit_at(pane_x, y) {
                    self.begin_text_edit(id);
                    return true;
                }
                let hit = self.engine.hit_test_drawing(pane_x, y);
                if let Some(hit) = hit
                    && self.engine.drawing(hit.id).is_some_and(|drawing| {
                        drawing.kind == DrawingKind::Text
                            && (drawing.text.trim().is_empty()
                                || self.engine.selected_drawing() == Some(hit.id)
                                || click_count >= 2)
                    })
                {
                    self.begin_text_edit(hit.id);
                    return true;
                }
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
            ChartDrawingTool::Path if click_count >= 2 => {
                let _ = self.place_drawing_anchor(pane_x, y, modifiers);
                let _ = self.finish_path_creation();
                true
            }
            tool => {
                let placing_text = tool == ChartDrawingTool::Text;
                let result = self.place_drawing_anchor(pane_x, y, modifiers);
                if result > 0 {
                    self.drawing_tool = ChartDrawingTool::Cursor;
                    self.cursor_style = CursorStyle::Crosshair;
                    let _ = self.engine.set_crosshair_ohlc_magnet(false);
                    if placing_text && let Ok(id) = DrawingId::try_from(result) {
                        self.begin_text_edit(id);
                    } else {
                        self.mark_user_state_changed();
                    }
                }
                result != 0
            }
        }
    }
    pub(super) fn drawing_pointer_move(
        &mut self,
        pane_x: f64,
        y: f64,
        dragging: bool,
        modifiers: DrawingModifiers,
    ) -> bool {
        if self.engine.brush_create_active() {
            if dragging {
                self.pending_brush_point = Some((pane_x, y));
            } else {
                self.pending_brush_point = None;
                self.engine.brush_create_cancel();
            }
            return true;
        }
        if self.engine.drawing_drag_active() {
            if dragging {
                self.engine.drawing_drag_to(pane_x, y, modifiers);
            } else {
                self.engine.drawing_drag_end();
                self.mark_user_state_changed();
            }
            return true;
        }
        if self.engine.drawing_create_active() {
            self.engine.drawing_create_move(pane_x, y, modifiers);
            return true;
        }
        false
    }
    pub(super) fn drawing_pointer_up(
        &mut self,
        pane_x: f64,
        y: f64,
        modifiers: DrawingModifiers,
    ) -> bool {
        if self.engine.brush_create_active() {
            self.pending_brush_point = Some((pane_x, y));
            self.flush_pending_brush();
            self.engine.brush_create_end();
            self.drawing_tool = ChartDrawingTool::Cursor;
            self.cursor_style = CursorStyle::Crosshair;
            let _ = self.engine.set_crosshair_ohlc_magnet(false);
            self.mark_user_state_changed();
            return true;
        }
        if self.engine.drawing_drag_active() {
            self.engine.drawing_drag_to(pane_x, y, modifiers);
            self.engine.drawing_drag_end();
            self.mark_user_state_changed();
            return true;
        }
        false
    }
}
