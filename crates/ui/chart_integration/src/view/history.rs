//! The chart's one undo history. Aeris Charts records drawings and its own indicators itself;
//! this module records the order-flow indicators the product owns, follows every undo and redo
//! step into product state, and forwards edits the desktop recorded back to it.

use super::{
    AerisChartView, ChartHostEvent, Context, InvalidOrderFlowSettings, OrderFlowSettings,
    VolumeIndicator,
};
use aeris_charts_engine::{ChartHistoryChange, HostEditId};
use std::any::Any;

/// Volume settings kept for removed volume indicators the history can bring back, newest last.
/// Matches the engine's bound on recorded edits, so every reachable removal keeps its settings.
const RETIRED_VOLUME_LIMIT: usize = 100;

/// Order-flow indicators one edit added or removed. Undo and redo set only those indicators, so
/// settings changed in between stay as they are.
#[derive(Clone, Copy)]
struct OrderFlowIndicatorEdit {
    before: OrderFlowSettings,
    after: OrderFlowSettings,
}

impl OrderFlowIndicatorEdit {
    fn new(before: OrderFlowSettings, after: OrderFlowSettings) -> Option<Self> {
        let edit = Self { before, after };
        (edit.apply(before, after) != before).then_some(edit)
    }

    /// `current` with every indicator this edit added or removed taken from `target`.
    const fn apply(
        self,
        mut current: OrderFlowSettings,
        target: OrderFlowSettings,
    ) -> OrderFlowSettings {
        let (before, after) = (self.before, self.after);
        if before.show_cumulative_delta != after.show_cumulative_delta {
            current.show_cumulative_delta = target.show_cumulative_delta;
        }
        if before.show_delta_histogram != after.show_delta_histogram {
            current.show_delta_histogram = target.show_delta_histogram;
        }
        if before.big_trades.is_some() != after.big_trades.is_some() {
            current.big_trades = target.big_trades;
        }
        if before.order_book_depth.is_some() != after.order_book_depth.is_some() {
            current.order_book_depth = target.order_book_depth;
        }
        if before.depth_heatmap.is_some() != after.depth_heatmap.is_some() {
            current.depth_heatmap = target.depth_heatmap;
        }
        current
    }
}

impl AerisChartView {
    /// Whether the chart has an edit to undo: a drawing, an indicator or a host-recorded edit.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        self.engine.can_undo()
    }

    /// Whether the chart has an undone edit to redo.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        self.engine.can_redo()
    }

    /// Undoes the chart's newest edit, whichever owner made it. An in-flight gesture is settled
    /// first; the armed toolbar tool stays armed.
    pub fn undo(&mut self, cx: &mut Context<Self>) -> bool {
        self.step_history(true, cx)
    }

    /// Redoes the chart's newest undone edit.
    pub fn redo(&mut self, cx: &mut Context<Self>) -> bool {
        self.step_history(false, cx)
    }

    /// Makes every edit recorded until [`Self::end_history_group`] one undo step, such as a
    /// host action that removes runtime studies and chart indicators together.
    pub fn begin_history_group(&mut self) {
        self.engine.begin_history_group();
    }

    /// Closes the group [`Self::begin_history_group`] opened.
    pub fn end_history_group(&mut self) {
        self.engine.end_history_group();
    }

    /// Records an edit to a host-owned object as one step of this chart's history. Undo and redo
    /// of it arrive as [`ChartHostEvent::HostEdit`]; the host reverses or reapplies the edit from
    /// the recorded value without recording it again.
    pub fn record_host_edit<T: Any + Send>(&mut self, edit: T) {
        self.engine.record_host_edit(Box::new(edit));
    }

    /// A host edit still in the history, when it holds a `T`.
    #[must_use]
    pub fn host_edit<T: Any>(&self, edit: HostEditId) -> Option<&T> {
        self.engine.host_edit(edit)?.downcast_ref()
    }

    /// A host edit still in the history, for a host that must update it (for example with the
    /// identities recreated objects received).
    pub fn host_edit_mut<T: Any>(&mut self, edit: HostEditId) -> Option<&mut T> {
        self.engine.host_edit_mut(edit)?.downcast_mut()
    }

    /// Sets the durable order-flow presentation as a trader's edit: adding or removing an
    /// order-flow indicator becomes one undo step.
    ///
    /// # Errors
    /// Returns the error of [`Self::set_order_flow_settings`].
    pub fn edit_order_flow_settings(
        &mut self,
        settings: OrderFlowSettings,
    ) -> Result<bool, InvalidOrderFlowSettings> {
        let before = self.order_flow_settings;
        let changed = self.set_order_flow_settings(settings)?;
        if let Some(edit) = OrderFlowIndicatorEdit::new(before, self.order_flow_settings) {
            self.engine.record_host_edit(Box::new(edit));
        }
        Ok(changed)
    }

    fn step_history(&mut self, undo: bool, cx: &mut Context<Self>) -> bool {
        let stepped = self.apply_history_step(undo);
        if stepped {
            self.after_input(cx);
        }
        stepped
    }

    /// Steps the engine's history; the product follows it when the queued input events are
    /// processed.
    pub(super) fn apply_history_step(&mut self, undo: bool) -> bool {
        let _ = self.finish_text_edit();
        self.engine.input_cancel();
        let stepped = if undo {
            self.engine.undo()
        } else {
            self.engine.redo()
        };
        if stepped {
            self.invalidate_series_frame();
        }
        stepped
    }

    /// Brings product state to an undo or redo step and returns the host requests it raised.
    pub(super) fn follow_history_changes(&mut self) -> Vec<ChartHostEvent> {
        let mut host_events = Vec::new();
        for change in self.engine.take_history_changes() {
            match change {
                ChartHistoryChange::IndicatorRestored { previous, binding } => {
                    self.restore_retired_volume(previous, binding);
                }
                ChartHistoryChange::IndicatorRemoved(binding) => self.retire_volume(binding),
                ChartHistoryChange::VolumeProfileRestored { id, .. } => {
                    self.volume_profile = Some(id);
                }
                ChartHistoryChange::VolumeProfileRemoved(id) => self.volume_profile_removed(id),
                ChartHistoryChange::HostEditUndone(edit) => {
                    host_events.extend(self.follow_host_edit(edit, true));
                }
                ChartHistoryChange::HostEditRedone(edit) => {
                    host_events.extend(self.follow_host_edit(edit, false));
                }
            }
        }
        self.invalidate_series_layout();
        self.mark_user_state_changed();
        host_events
    }

    /// Applies an order-flow edit this chart recorded; any other edit belongs to the host.
    fn follow_host_edit(&mut self, edit: HostEditId, undo: bool) -> Option<ChartHostEvent> {
        let Some(order_flow) = self.host_edit::<OrderFlowIndicatorEdit>(edit).copied() else {
            return Some(ChartHostEvent::HostEdit { edit, undo });
        };
        let target = if undo {
            order_flow.before
        } else {
            order_flow.after
        };
        let settings = order_flow.apply(self.order_flow_settings, target);
        self.set_order_flow_settings(settings)
            .ok()
            .filter(|changed| *changed)
            .map(|_| ChartHostEvent::OrderFlowSettingsChanged)
    }

    /// Forgets the volume indicator drawn by `binding`, keeping its settings for the history.
    pub(super) fn retire_volume(&mut self, binding: u32) {
        if self
            .volume
            .as_ref()
            .is_none_or(|volume| volume.columns != binding)
        {
            return;
        }
        if let Some(volume) = self.volume.take() {
            if self.retired_volumes.len() == RETIRED_VOLUME_LIMIT {
                self.retired_volumes.pop_front();
            }
            self.retired_volumes
                .push_back((volume.columns, volume.settings));
        }
    }

    /// Adopts a binding the history brought back as the volume indicator when it was one.
    fn restore_retired_volume(&mut self, previous: u32, binding: u32) {
        let Some(index) = self
            .retired_volumes
            .iter()
            .position(|(retired, _)| *retired == previous)
        else {
            return;
        };
        let Some((_, settings)) = self.retired_volumes.remove(index) else {
            return;
        };
        let average = self
            .engine
            .indicator_bindings()
            .into_iter()
            .find(|info| info.binding_id == binding)
            .and_then(|info| info.outputs.get(1).copied());
        if let Some(average) = average {
            self.volume = Some(VolumeIndicator {
                columns: binding,
                average,
                settings,
            });
        }
    }

    /// Forgets the volume profile the chart or its history removed.
    pub(super) fn volume_profile_removed(&mut self, id: aeris_charts_engine::NativePrimitiveId) {
        if self.volume_profile == Some(id) {
            self.volume_profile = None;
        }
    }

    /// Forgets every recorded edit after the chart was recreated from saved state.
    pub(super) fn clear_history(&mut self) {
        self.engine.clear_history();
        self.retired_volumes.clear();
    }
}
