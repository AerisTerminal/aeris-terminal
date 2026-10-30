//! Requests the chart sends to its host, and the selection removal that can raise one.

use super::{AerisChartView, ChartSettingsRequest};
use gpui::{Context, Pixels, Point, SharedString};

/// Distinguishes a pane-canvas right-click from a price-axis right-click.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChartContextKind {
    Pane,
    PriceAxis { pane: usize, left: bool },
}

/// A chart-surface right-click waiting for the shell to present a menu.
#[derive(Clone, Debug, PartialEq)]
pub struct ChartContextRequest {
    pub position: Point<Pixels>,
    pub kind: ChartContextKind,
    /// Aeris Charts-formatted price at the right-click, when the click was on the pane.
    pub copy_price: Option<SharedString>,
}

/// A request from the chart to its host, emitted as a GPUI event when it happens.
#[derive(Clone, Debug, PartialEq)]
pub enum ChartHostEvent {
    /// A press on the chart asks the host to make its pane active.
    Activate,
    /// A right-click asks the host to present the chart context menu.
    ContextMenu(ChartContextRequest),
    /// A legend control or drawing asks the host to open that item's settings.
    Settings(ChartSettingsRequest),
    /// The user removed a runtime-managed study, by its runtime study identity.
    RemoveStudy(u64),
    /// Bar replay's phase, clock, speed or step changed; read it through
    /// [`AerisChartView::bar_replay_state`].
    ReplayChanged,
}

/// The outcome of deleting the selected chart object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SelectionRemoval {
    /// Nothing removable was selected.
    Protected,
    /// The chart removed the object itself.
    Removed,
    /// A runtime-managed study, which only its host can remove.
    Study(u64),
}

impl SelectionRemoval {
    pub(super) const fn host_event(self) -> Option<ChartHostEvent> {
        match self {
            Self::Study(study_id) => Some(ChartHostEvent::RemoveStudy(study_id)),
            Self::Protected | Self::Removed => None,
        }
    }
}

impl AerisChartView {
    /// Returns whether the current chart-object selection can be deleted by product chrome.
    #[must_use]
    pub fn has_deletable_selection(&self) -> bool {
        self.engine.selected_drawing().is_some()
            || self
                .engine
                .selected_series()
                .is_some_and(|series| series != 0)
    }

    /// Removes the selected drawing or native Aeris Charts indicator; a selected runtime-managed
    /// study becomes a [`ChartHostEvent::RemoveStudy`].
    ///
    /// The price series is product-owned and cannot be deleted.
    pub fn remove_selected_chart_object(&mut self, cx: &mut Context<Self>) -> bool {
        let removal = self.remove_selection();
        if let Some(event) = removal.host_event() {
            cx.emit(event);
        }
        removal != SelectionRemoval::Protected
    }

    pub(super) fn remove_selection(&mut self) -> SelectionRemoval {
        if self.remove_selected_drawing() {
            self.cancel_drawing();
            return SelectionRemoval::Removed;
        }
        let Some(series) = self.engine.selected_series() else {
            return SelectionRemoval::Protected;
        };
        let removal = self.remove_series_selection(series);
        if removal != SelectionRemoval::Protected {
            self.engine.set_selected_series(None);
        }
        removal
    }

    /// Removes one selected product series. Studies are runtime-owned and only their host can
    /// remove them; the price series, the footprint presentation and the volume weights are
    /// protected.
    pub(super) fn remove_series_selection(&mut self, series: u32) -> SelectionRemoval {
        if series == 0 || series == self.volume_series || self.footprint_series_id() == Some(series)
        {
            return SelectionRemoval::Protected;
        }
        if let Some(study_id) = self.engine.external_study_for_series(series) {
            return SelectionRemoval::Study(study_id);
        }
        if let Some(study) = self.order_flow_study_for_series(series) {
            return if self.remove_order_flow_study(study) {
                SelectionRemoval::Removed
            } else {
                SelectionRemoval::Protected
            };
        }
        if self.is_volume_output(series) {
            self.remove_volume();
        } else if !self.engine.remove_indicator_for_series(series)
            && !self.engine.remove_series(series)
        {
            return SelectionRemoval::Protected;
        }
        self.invalidate_series_layout();
        self.mark_user_state_changed();
        SelectionRemoval::Removed
    }
}

impl gpui::EventEmitter<ChartHostEvent> for AerisChartView {}
