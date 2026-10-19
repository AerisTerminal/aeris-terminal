//! Selection outcome fence for one chart pane.

pub(super) struct ChartPaneController {
    pending_selection: Option<u64>,
    latest_selection: u64,
    current_generation: u64,
}

impl Default for ChartPaneController {
    fn default() -> Self {
        Self {
            pending_selection: None,
            latest_selection: 0,
            current_generation: 1,
        }
    }
}

impl ChartPaneController {
    pub(super) fn request(&mut self, generation: u64) {
        self.latest_selection = self.latest_selection.max(generation);
        self.pending_selection = Some(generation);
    }

    pub(super) fn clear(&mut self) {
        self.pending_selection = None;
    }

    #[cfg(test)]
    pub(super) fn pending_selection(&self) -> Option<u64> {
        self.pending_selection
    }

    /// Retired selection outcomes cannot change the current pane or its notice.
    pub(super) fn accept(&mut self, generation: u64) -> bool {
        if self.pending_selection != Some(generation) {
            return false;
        }
        self.pending_selection = None;
        self.current_generation = generation;
        true
    }

    /// A failed request retires its pending outcome, not the binding still
    /// displayed by the pane. The old generation remains valid for publications.
    pub(super) fn reject(&mut self, generation: u64) -> bool {
        if self.pending_selection != Some(generation) {
            return false;
        }
        self.pending_selection = None;
        true
    }

    /// Accept pane-scoped data only for the committed selection. While a
    /// request is pending, the previous chart remains the displayed binding.
    pub(super) fn accept_publication(&self, generation: u64) -> bool {
        generation == self.current_generation
    }

    /// A previous series may keep publishing data while its replacement is
    /// pending, but its lifecycle state cannot settle or fail that request.
    pub(super) fn accept_series_state(&self, generation: u64) -> bool {
        self.pending_selection.is_none() && self.accept_publication(generation)
    }

    /// A study publication without a demand belongs to the consumer's
    /// app-scoped study lifecycle; demand-bound output must match this chart.
    pub(super) fn accept_study_publication(&self, generation: Option<u64>) -> bool {
        generation.is_none_or(|generation| self.accept_publication(generation))
    }

    /// An initial instrument descriptor must not overwrite a later pane selection
    /// just because its product happens to have the same provider and symbol.
    pub(super) fn accept_initial_instrument(&self, generation: u64) -> bool {
        self.pending_selection.is_none() && generation >= self.latest_selection
    }

    pub(super) fn adopt_initial_instrument(&mut self, generation: u64) -> bool {
        if !self.accept_initial_instrument(generation) {
            return false;
        }
        self.current_generation = generation;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_outcomes_accept_only_the_exact_pending_generation() {
        let mut pane = ChartPaneController::default();
        pane.request(7);
        pane.request(8);
        assert!(!pane.accept(7));
        assert!(!pane.accept(9));
        assert_eq!(pane.pending_selection(), Some(8));
        assert!(pane.accept(8));
        assert!(!pane.accept(8));
        pane.request(10);
        assert!(pane.accept(10));
        assert_eq!(pane.pending_selection(), None);
    }

    #[test]
    fn retired_initial_instrument_cannot_overwrite_a_pending_or_committed_selection() {
        let mut pane = ChartPaneController::default();
        assert!(pane.accept_initial_instrument(1));
        pane.request(2);
        assert!(!pane.accept_initial_instrument(1));
        assert!(!pane.accept_initial_instrument(2));
        assert!(pane.accept(2));
        assert!(!pane.accept_initial_instrument(1));
        pane.request(3);
        pane.clear();
        assert!(!pane.accept_initial_instrument(2));
        assert!(pane.accept_initial_instrument(3));
    }

    #[test]
    fn session_publications_follow_only_the_committed_selection() {
        let mut pane = ChartPaneController::default();
        assert!(pane.adopt_initial_instrument(3));
        pane.request(4);
        assert!(pane.accept_publication(3), "old chart is still displayed");
        assert!(!pane.accept_publication(4));
        assert!(pane.accept(4));
        assert!(!pane.accept_publication(3));
        assert!(pane.accept_publication(4));
        pane.request(5);
        pane.clear();
        assert!(pane.accept_publication(4), "failed request kept its chart");
        assert!(!pane.accept_publication(5));
    }

    #[test]
    fn rejected_request_retains_the_displayed_binding_and_retired_outcomes_stay_rejected() {
        let mut pane = ChartPaneController::default();
        assert!(pane.adopt_initial_instrument(1));
        pane.request(2);
        assert!(pane.accept_publication(1));
        assert!(!pane.accept_publication(2));
        assert!(pane.reject(2));
        assert!(
            pane.accept_publication(1),
            "failure keeps the prior binding"
        );
        assert!(!pane.accept_publication(2));
        assert!(!pane.reject(2));
        assert!(!pane.accept(2));
        pane.request(3);
        assert!(!pane.reject(2));
        assert!(pane.accept(3));
        assert!(!pane.accept_publication(1));
        assert!(pane.accept_publication(3));
    }

    #[test]
    fn pending_selection_does_not_accept_old_series_lifecycle_states() {
        let mut pane = ChartPaneController::default();
        pane.request(2);
        assert!(pane.accept_publication(1));
        assert!(!pane.accept_series_state(1));
        assert!(!pane.accept_series_state(2));
        assert!(pane.reject(2));
        assert!(pane.accept_series_state(1));
        pane.request(3);
        assert!(pane.accept(3));
        assert!(!pane.accept_series_state(1));
        assert!(pane.accept_series_state(3));
    }

    #[test]
    fn study_publications_reject_retired_and_uncommitted_demand() {
        let mut pane = ChartPaneController::default();
        assert!(pane.accept_study_publication(None));
        pane.request(2);
        assert!(pane.accept_study_publication(Some(1)));
        assert!(!pane.accept_study_publication(Some(2)));
        assert!(pane.accept(2));
        assert!(!pane.accept_study_publication(Some(1)));
        assert!(pane.accept_study_publication(Some(2)));
        assert!(pane.accept_study_publication(None));
    }
}
