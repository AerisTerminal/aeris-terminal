//! Drawings. Tool arming, placement, editing, locks, and history are Aeris Charts state; this
//! module exposes them to the shell's toolbar and menus.

use super::{
    AerisChartView, ChartDrawingKind, ChartDrawingStamp, DrawingId, DrawingsLockSummary,
    price_format_min_move,
};
use aeris_charts_engine::{ProfileDrawingOptions, ProfileSource};
use num_traits::ToPrimitive;

impl AerisChartView {
    /// Returns the drawing tool currently armed on the chart surface, `None` for the cursor.
    #[must_use]
    pub fn drawing_tool(&self) -> Option<ChartDrawingKind> {
        self.engine.active_drawing_tool()
    }

    /// Arms a drawing tool (or the cursor with `None`), replacing any unfinished gesture. The
    /// icon-stamp tool arms with the first built-in stamp.
    pub fn set_drawing_tool(&mut self, tool: Option<ChartDrawingKind>) {
        let template =
            tool.and_then(|kind| self.drawing_tool_template(kind, ChartDrawingStamp::ALL[0]));
        self.arm_drawing_tool(tool, template.as_deref());
    }

    /// Arms the icon-stamp tool with one built-in stamp.
    pub fn set_drawing_stamp(&mut self, stamp: ChartDrawingStamp) {
        let kind = ChartDrawingKind::IconStamp;
        let template = self.drawing_tool_template(kind, stamp);
        self.arm_drawing_tool(Some(kind), template.as_deref());
    }

    fn arm_drawing_tool(&mut self, tool: Option<ChartDrawingKind>, template: Option<&str>) {
        self.engine.input_cancel();
        let _ = self.finish_text_edit();
        let armed = self.engine.set_drawing_tool(tool, template, None);
        debug_assert!(armed, "every built-in drawing tool has a valid template");
        self.invalidate_series_frame();
    }

    /// The host data a tool's template needs beyond the engine defaults: the series a
    /// data-bound profile reads and its price bin, or the stamp an icon stamp places.
    fn drawing_tool_template(
        &self,
        kind: ChartDrawingKind,
        stamp: ChartDrawingStamp,
    ) -> Option<String> {
        let patch = match kind {
            ChartDrawingKind::FixedRangeVolumeProfile
            | ChartDrawingKind::AnchoredVolumeProfile
            | ChartDrawingKind::AnchoredVwap => {
                let profile = ProfileDrawingOptions {
                    source: ProfileSource::Candles {
                        price_series: 0,
                        volume_series: self.volume_series,
                    },
                    tick_size: self.price_tick_size(),
                    ..ProfileDrawingOptions::default()
                };
                serde_json::json!({ "profile": profile })
            }
            ChartDrawingKind::IconStamp => serde_json::json!({ "icon_name": stamp.icon_name() }),
            _ => return None,
        };
        Some(patch.to_string())
    }

    /// The instrument's minimum price increment in chart price units, or the smallest displayed
    /// step when the provider publishes none.
    fn price_tick_size(&self) -> f64 {
        self.instrument_price_increment
            .filter(|&increment| increment > 0)
            .and_then(|increment| increment.to_f64())
            .map(|increment| increment / self.price_divisor)
            .filter(|tick| tick.is_finite() && *tick > 0.0)
            .unwrap_or_else(|| price_format_min_move(self.instrument_price_precision))
    }

    /// Cancels creation or movement and returns to the cursor tool.
    pub fn cancel_drawing(&mut self) {
        self.engine.input_cancel();
        let _ = self.finish_text_edit();
        self.engine.cancel_drawing_tool();
        self.invalidate_series_frame();
    }

    /// Whether Aeris Charts is editing text on a drawing.
    #[must_use]
    pub fn is_editing_text(&self) -> bool {
        self.engine.drawing_text_edit().is_some()
    }

    /// Commit the active text edit. The engine removes empty standalone text, but keeps an
    /// unlabeled trend line.
    pub fn finish_text_edit(&mut self) -> bool {
        let committed = self.engine.commit_drawing_text_edit();
        if committed {
            self.invalidate_series_frame();
        }
        committed
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
            .and_then(|id| self.engine.drawing(id))
            .is_some_and(|drawing| drawing.locked)
    }

    /// Locks or unlocks the selected drawing against pointer movement.
    pub fn set_selected_drawing_locked(&mut self, locked: bool) -> bool {
        let Some(id) = self.engine.selected_drawing() else {
            return false;
        };
        if self.selected_drawing_locked() == locked {
            return false;
        }
        let changed = self.engine.set_drawing_locked(id, locked);
        if changed {
            self.invalidate_series_frame();
        }
        changed
    }

    /// Returns aggregate lock state for the drawing toolbar.
    #[must_use]
    pub fn drawings_lock_summary(&self) -> DrawingsLockSummary {
        let drawings = self.engine.drawings();
        let total = drawings.len();
        let locked_count = drawings.iter().filter(|drawing| drawing.locked).count();
        DrawingsLockSummary {
            total,
            locked_count,
            all_locked: total > 0 && total == locked_count,
        }
    }

    /// Locks or unlocks every committed drawing against pointer movement.
    pub fn set_all_drawings_locked(&mut self, locked: bool) -> bool {
        let ids = self
            .engine
            .drawings()
            .iter()
            .filter(|drawing| drawing.locked != locked)
            .map(|drawing| drawing.id)
            .collect::<Vec<_>>();
        for &id in &ids {
            self.engine.set_drawing_locked(id, locked);
        }
        if ids.is_empty() {
            return false;
        }
        self.invalidate_series_frame();
        true
    }

    /// Removes the selected drawing, if one exists.
    pub fn remove_selected_drawing(&mut self) -> bool {
        let removed = self.engine.remove_selected_drawing();
        if removed {
            self.invalidate_series_frame();
        }
        removed
    }

    /// Removes every committed drawing.
    pub fn clear_drawings(&mut self) {
        let _ = self.finish_text_edit();
        self.engine.input_cancel();
        self.engine.cancel_drawing_creation();
        self.engine.clear_drawings();
        self.invalidate_series_frame();
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

    /// Steps the engine's chart-local drawing history. An in-flight gesture is settled first; the
    /// armed toolbar tool stays armed.
    fn step_drawing_history(&mut self, undo: bool) -> bool {
        let _ = self.finish_text_edit();
        self.engine.input_cancel();
        let stepped = if undo {
            self.engine.undo_drawing()
        } else {
            self.engine.redo_drawing()
        };
        if stepped {
            self.invalidate_series_frame();
        }
        stepped
    }
}
