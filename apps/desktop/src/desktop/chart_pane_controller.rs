//! Selection outcome fence for one chart pane.

#[derive(Default)]
pub(super) struct ChartPaneController {
    pending_selection: Option<u64>,
    latest_selection: u64,
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
        true
    }

    /// An initial instrument descriptor must not overwrite a later pane selection
    /// just because its product happens to have the same provider and symbol.
    pub(super) fn accept_initial_instrument(&self, generation: u64) -> bool {
        self.pending_selection.is_none() && generation >= self.latest_selection
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
}
