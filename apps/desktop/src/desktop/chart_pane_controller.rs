//! Selection outcome fence for one chart pane.

#[derive(Default)]
pub(super) struct ChartPaneController {
    pending_selection: Option<u64>,
}

impl ChartPaneController {
    pub(super) fn request(&mut self, generation: u64) {
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
}
