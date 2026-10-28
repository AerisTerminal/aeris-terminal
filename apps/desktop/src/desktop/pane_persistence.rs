//! What the pane owes the workspace document: the saved chart document and viewport a restore
//! applies to its first chart, the chart's last observed user-state revision, the viewport last
//! reported to the market worker and whether unsaved pane changes are pending.

use super::WorkspaceChartState;

/// A visible chart time range in Unix nanoseconds, start then end.
type Viewport = (i64, i64);

#[derive(Default)]
pub(super) struct PanePersistence {
    /// The restored document, consumed by the first chart that has a covering snapshot.
    saved_chart: Option<WorkspaceChartState>,
    /// The restored time range, kept for saves until the pane switches selection.
    restored_viewport: Option<Viewport>,
    reported_viewport: Option<Viewport>,
    /// The viewport a save stores for this pane, as last observed.
    durable_viewport: Option<Viewport>,
    chart_user_state_revision: u64,
    dirty: bool,
}

impl PanePersistence {
    pub(super) const fn restore(
        saved_chart: Option<WorkspaceChartState>,
        restored_viewport: Option<Viewport>,
    ) -> Self {
        Self {
            saved_chart,
            restored_viewport,
            reported_viewport: None,
            durable_viewport: restored_viewport,
            chart_user_state_revision: 0,
            dirty: false,
        }
    }

    pub(super) const fn saved_chart(&self) -> Option<&WorkspaceChartState> {
        self.saved_chart.as_ref()
    }

    pub(super) const fn take_saved_chart(&mut self) -> Option<WorkspaceChartState> {
        self.saved_chart.take()
    }

    pub(super) const fn restored_viewport(&self) -> Option<Viewport> {
        self.restored_viewport
    }

    pub(super) const fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Whether a save is owed, clearing the flag.
    pub(super) const fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    /// Forgets pending changes the document already holds, such as restored layout applied
    /// back to the pane.
    pub(super) const fn discard_dirty(&mut self) {
        self.dirty = false;
    }

    #[cfg(test)]
    pub(super) const fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Takes a newly installed chart's revision as the baseline, so the install itself is
    /// not a user change.
    pub(super) const fn adopt_chart_user_state(&mut self, revision: u64) {
        self.chart_user_state_revision = revision;
    }

    /// Records the chart's user-state revision; a change marks the pane dirty and is returned.
    pub(super) const fn observe_chart_user_state(&mut self, revision: u64) -> bool {
        if revision == self.chart_user_state_revision {
            return false;
        }
        self.chart_user_state_revision = revision;
        self.dirty = true;
        true
    }

    /// Records the viewport a save would store; a change marks the pane dirty and is returned.
    pub(super) fn observe_durable_viewport(&mut self, viewport: Option<Viewport>) -> bool {
        if self.durable_viewport == viewport {
            return false;
        }
        self.durable_viewport = viewport;
        self.dirty = true;
        true
    }

    /// Reports a viewport not yet reported through `report`, which returns whether the
    /// worker accepted it; only an accepted viewport is remembered. Returns whether it was.
    pub(super) fn report_viewport(
        &mut self,
        viewport: Viewport,
        report: impl FnOnce(Viewport) -> bool,
    ) -> bool {
        if self.reported_viewport == Some(viewport) || !report(viewport) {
            return false;
        }
        self.reported_viewport = Some(viewport);
        true
    }

    /// Drops both viewports when the pane switches selection: they describe the old series.
    pub(super) const fn forget_viewports(&mut self) {
        self.restored_viewport = None;
        self.reported_viewport = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_saved_chart_is_consumed_once() {
        let mut persistence = PanePersistence::restore(Some(WorkspaceChartState::default()), None);
        assert!(persistence.saved_chart().is_some());
        assert!(persistence.take_saved_chart().is_some());
        assert!(persistence.saved_chart().is_none());
        assert!(persistence.take_saved_chart().is_none());
    }

    #[test]
    fn dirty_is_taken_once_and_can_be_discarded() {
        let mut persistence = PanePersistence::default();
        assert!(!persistence.take_dirty());
        persistence.mark_dirty();
        assert!(persistence.is_dirty());
        assert!(persistence.take_dirty());
        assert!(!persistence.take_dirty());
        persistence.mark_dirty();
        persistence.discard_dirty();
        assert!(!persistence.is_dirty());
    }

    #[test]
    fn only_a_changed_chart_user_state_marks_the_pane_dirty() {
        let mut persistence = PanePersistence::default();
        persistence.adopt_chart_user_state(4);
        assert!(!persistence.is_dirty());
        assert!(!persistence.observe_chart_user_state(4));
        assert!(!persistence.is_dirty());
        assert!(persistence.observe_chart_user_state(5));
        assert!(persistence.take_dirty());
        assert!(!persistence.observe_chart_user_state(5));
        assert!(!persistence.is_dirty());
    }

    #[test]
    fn a_viewport_is_reported_until_the_worker_accepts_it() {
        let mut persistence = PanePersistence::default();
        let mut reports = 0;
        assert!(!persistence.report_viewport((1, 2), |_| {
            reports += 1;
            false
        }));
        assert!(persistence.report_viewport((1, 2), |_| {
            reports += 1;
            true
        }));
        assert!(!persistence.report_viewport((1, 2), |_| {
            reports += 1;
            true
        }));
        assert_eq!(reports, 2);
        assert!(persistence.report_viewport((1, 3), |viewport| viewport == (1, 3)));
    }

    #[test]
    fn only_a_changed_durable_viewport_marks_the_pane_dirty() {
        let mut persistence = PanePersistence::restore(None, Some((10, 20)));
        assert!(!persistence.observe_durable_viewport(Some((10, 20))));
        assert!(!persistence.is_dirty());
        assert!(persistence.observe_durable_viewport(None));
        assert!(persistence.take_dirty());
        assert!(!persistence.observe_durable_viewport(None));
        assert!(persistence.observe_durable_viewport(Some((30, 40))));
        assert!(persistence.take_dirty());
    }

    #[test]
    fn a_selection_switch_forgets_both_viewports() {
        let mut persistence = PanePersistence::restore(None, Some((10, 20)));
        assert!(persistence.report_viewport((10, 20), |_| true));
        persistence.forget_viewports();
        assert_eq!(persistence.restored_viewport(), None);
        assert!(persistence.report_viewport((10, 20), |_| true));
    }
}
