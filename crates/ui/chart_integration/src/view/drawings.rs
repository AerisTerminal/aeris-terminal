//! Drawings. Tool arming, placement, editing, locks, and history are Aeris Charts state; this
//! module exposes them to the shell's toolbar and menus.

use super::{
    AerisChartView, ChartDrawingKind, ChartDrawingStamp, DrawingId, DrawingsLockSummary, css_hex,
    price_format_min_move,
};
use aeris_charts_engine::{
    ChartTimeZone, DrawingIntervalVisibility, DrawingLabelOptions, DrawingLevel,
    DrawingPropertySchema, ProfileDrawingOptions, ProfileSource, drawing_property_schema,
};
use aeris_charts_render::color::Color;
use num_traits::ToPrimitive;

/// One drawing as its toolbar and settings dialog edit it: the Aeris Charts property schema of
/// its kind and its current options.
#[derive(Clone, Debug, PartialEq)]
pub struct ChartDrawingSettings {
    pub id: DrawingId,
    pub schema: DrawingPropertySchema,
    options: serde_json::Value,
}

impl ChartDrawingSettings {
    #[must_use]
    pub const fn kind(&self) -> ChartDrawingKind {
        self.schema.kind
    }

    /// The option's current value; `Null` for a name the kind does not store.
    #[must_use]
    pub fn value(&self, name: &str) -> &serde_json::Value {
        self.options.get(name).unwrap_or(&serde_json::Value::Null)
    }

    #[must_use]
    pub fn flag(&self, name: &str) -> bool {
        self.value(name).as_bool().unwrap_or(false)
    }

    #[must_use]
    pub fn number(&self, name: &str) -> Option<f64> {
        self.value(name).as_f64()
    }

    #[must_use]
    pub fn text(&self, name: &str) -> &str {
        self.value(name).as_str().unwrap_or_default()
    }

    /// The color as `#RRGGBB` or `#RRGGBBAA`; `None` while the option is unset and the engine
    /// paints its fallback.
    #[must_use]
    pub fn color(&self, name: &str) -> Option<String> {
        drawing_color_hex(self.text(name))
    }

    #[must_use]
    pub fn levels(&self, name: &str) -> Vec<DrawingLevel> {
        serde_json::from_value(self.value(name).clone()).unwrap_or_default()
    }

    #[must_use]
    pub fn labels(&self) -> Vec<DrawingLabelOptions> {
        serde_json::from_value(self.value("labels").clone()).unwrap_or_default()
    }

    #[must_use]
    pub fn interval_visibility(&self) -> DrawingIntervalVisibility {
        serde_json::from_value(self.value("interval_visibility").clone()).unwrap_or_default()
    }
}

/// An engine CSS color as `#RRGGBB` or `#RRGGBBAA`, the form the host color picker edits.
#[must_use]
pub fn drawing_color_hex(css: &str) -> Option<String> {
    Color::parse_css(css).map(css_hex)
}

/// One anchor's coordinates as editable text: the price at the chart's precision and, when the
/// kind places the anchor in time, `YYYY-MM-DD HH:MM` in the chart's time zone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChartDrawingAnchorText {
    pub price: String,
    pub time: Option<String>,
}

/// Why typed anchor coordinates were not applied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChartDrawingAnchorError {
    Price,
    Time,
    Rejected,
}

impl std::fmt::Display for ChartDrawingAnchorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Price => "Enter a price as a number",
            Self::Time => "Enter a time as YYYY-MM-DD HH:MM",
            Self::Rejected => "The chart cannot place the point there",
        })
    }
}

const SECONDS_PER_DAY: i64 = 86_400;

/// Days from 1970-01-01 to a proleptic Gregorian date.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Wall-clock `YYYY-MM-DD HH:MM[:SS]` as seconds of a pseudo-epoch whose civil fields are the
/// local time, or `None` for text that names no real date and time.
fn parse_local_date_time(text: &str) -> Option<i64> {
    let (date, time) = text.trim().split_once([' ', 'T'])?;
    let mut date = date.splitn(3, '-');
    let year = date.next()?.parse::<i64>().ok()?;
    let month = date.next()?.parse::<u32>().ok()?;
    let day = date.next()?.parse::<u32>().ok()?;
    let mut time = time.trim().splitn(3, ':');
    let hour = time.next()?.parse::<i64>().ok()?;
    let minute = time.next()?.parse::<i64>().ok()?;
    let second = time
        .next()
        .map_or(Some(0), |second| second.parse::<i64>().ok())?;
    if !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..60).contains(&second)
    {
        return None;
    }
    let first = days_from_civil(year, month, 1);
    let next = if month == 12 {
        days_from_civil(year + 1, 1, 1)
    } else {
        days_from_civil(year, month + 1, 1)
    };
    if day == 0 || i64::from(day) > next - first {
        return None;
    }
    let days = first + i64::from(day) - 1;
    Some(days * SECONDS_PER_DAY + hour * 3_600 + minute * 60 + second)
}

fn format_local_date_time(zone: ChartTimeZone, utc_seconds: f64) -> Option<String> {
    let parts = zone.local_parts(utc_seconds.floor().to_i64()?)?;
    let seconds = if parts.second == 0 {
        String::new()
    } else {
        format!(":{:02}", parts.second)
    };
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}{seconds}",
        parts.year, parts.month, parts.day, parts.hour, parts.minute
    ))
}

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
    pub(super) fn price_tick_size(&self) -> f64 {
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

    /// Returns whether a committed drawing with this identifier exists.
    #[must_use]
    pub fn has_drawing(&self, id: DrawingId) -> bool {
        self.engine.drawing(id).is_some()
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

    /// The drawing's property schema and current options, or `None` for an unknown id.
    #[must_use]
    pub fn drawing_settings(&self, id: DrawingId) -> Option<ChartDrawingSettings> {
        let kind = self.engine.drawing(id)?.kind;
        let options = serde_json::from_str(&self.engine.drawing_options_json(id)?).ok()?;
        Some(ChartDrawingSettings {
            id,
            schema: drawing_property_schema(kind),
            options,
        })
    }

    #[must_use]
    pub fn selected_drawing_settings(&self) -> Option<ChartDrawingSettings> {
        self.drawing_settings(self.engine.selected_drawing()?)
    }

    /// Merges an options patch into one drawing. Outside a settings session each call is one
    /// undo step; inside one, the session commit records the whole edit.
    pub fn apply_drawing_options(&mut self, id: DrawingId, patch: &serde_json::Value) -> bool {
        let applied = self.engine.drawing_apply_options(id, &patch.to_string());
        if applied {
            self.invalidate_series_frame();
        }
        applied
    }

    /// Starts a settings session that previews edits live; any previous session is committed.
    pub fn begin_drawing_settings_edit(&mut self, id: DrawingId) -> bool {
        let _ = self.finish_text_edit();
        self.engine.begin_drawing_options_edit(id)
    }

    /// Ends the settings session, keeping its edits as one undo step.
    pub fn commit_drawing_settings_edit(&mut self) -> bool {
        let committed = self.engine.commit_drawing_options_edit();
        if committed {
            self.invalidate_series_frame();
        }
        committed
    }

    /// Ends the settings session, restoring the drawing to how it was when the session began.
    pub fn cancel_drawing_settings_edit(&mut self) -> bool {
        let cancelled = self.engine.cancel_drawing_options_edit();
        if cancelled {
            self.invalidate_series_frame();
        }
        cancelled
    }

    /// One anchor as editable text in the chart's price precision and time zone.
    #[must_use]
    pub fn drawing_anchor_text(
        &self,
        id: DrawingId,
        index: usize,
    ) -> Option<ChartDrawingAnchorText> {
        let (price, time) = self.engine.drawing_anchor(id, index)?;
        let precision = usize::from(
            self.price_precision_override
                .unwrap_or(self.instrument_price_precision)
                .min(18),
        );
        let zone = ChartTimeZone::parse(self.engine.time_zone_id()).unwrap_or_default();
        Some(ChartDrawingAnchorText {
            price: format!("{price:.precision$}"),
            time: time.and_then(|time| format_local_date_time(zone, time)),
        })
    }

    /// Moves one anchor to typed coordinates; `time` is wall time in the chart's time zone.
    ///
    /// # Errors
    ///
    /// Returns which field could not be read, or `Rejected` when Aeris Charts refuses the move.
    pub fn set_drawing_anchor_text(
        &mut self,
        id: DrawingId,
        index: usize,
        price: &str,
        time: Option<&str>,
    ) -> Result<(), ChartDrawingAnchorError> {
        let price = price
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|price| price.is_finite())
            .ok_or(ChartDrawingAnchorError::Price)?;
        let time = match time {
            Some(text) => {
                let zone = ChartTimeZone::parse(self.engine.time_zone_id()).unwrap_or_default();
                let utc_millis = parse_local_date_time(text)
                    .and_then(|local| local.checked_mul(1_000))
                    .and_then(|local| zone.utc_millis_from_local_epoch_millis(local))
                    .ok_or(ChartDrawingAnchorError::Time)?;
                utc_millis.to_f64().map(|millis| millis / 1_000.0)
            }
            None => None,
        };
        if !self.engine.set_drawing_anchor(id, index, price, time) {
            return Err(ChartDrawingAnchorError::Rejected);
        }
        self.invalidate_series_frame();
        Ok(())
    }

    /// The drawing's style saved as a named Aeris Charts template document.
    #[must_use]
    pub fn drawing_template_json(&self, id: DrawingId, name: &str) -> Option<String> {
        self.engine.drawing_template_json(id, name)
    }

    /// Restyles a drawing from a template of its own kind, as one undo step.
    pub fn apply_drawing_template_json(&mut self, id: DrawingId, json: &str) -> bool {
        let applied = self.engine.apply_drawing_template_json(id, json);
        if applied {
            self.invalidate_series_frame();
        }
        applied
    }

    pub fn set_drawing_locked(&mut self, id: DrawingId, locked: bool) -> bool {
        let changed = self.engine.set_drawing_locked(id, locked);
        if changed {
            self.invalidate_series_frame();
        }
        changed
    }

    pub fn set_drawing_visible(&mut self, id: DrawingId, visible: bool) -> bool {
        let changed = self.engine.set_drawing_visibility(id, visible);
        if changed {
            self.invalidate_series_frame();
        }
        changed
    }

    pub fn remove_drawing(&mut self, id: DrawingId) -> bool {
        let _ = self.finish_text_edit();
        let removed = self.engine.remove_drawing(id);
        if removed {
            self.invalidate_series_frame();
        }
        removed
    }

    /// Duplicates a drawing in place and selects the copy.
    pub fn clone_drawing(&mut self, id: DrawingId) -> Option<DrawingId> {
        let copy = self.engine.clone_drawing(id, 0.0, 0.0)?;
        self.engine.set_selected_drawing(Some(copy));
        self.invalidate_series_frame();
        Some(copy)
    }

    /// Asks the trading host to place the bracket a long or short position drawing describes.
    ///
    /// # Errors
    ///
    /// Returns Aeris Charts' reason when the drawing is not a position, the quantity is below
    /// the instrument minimum, or another trading request is still active.
    pub fn place_bracket_order_from_drawing(
        &mut self,
        id: DrawingId,
        quantity: f64,
    ) -> Result<(), String> {
        self.engine
            .place_bracket_order_from_drawing(id, quantity)
            .map_err(|error| error.to_string())?;
        self.invalidate_series_frame();
        Ok(())
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
