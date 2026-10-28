//! What the terminal tells the user outside a pane's own surface: the toast stacks of every
//! pane and the workspace error line under the tab strip, which names the last workspace action
//! that could not complete until another action succeeds.

use super::Toaster;

pub(super) struct AppNotices {
    toaster: Toaster,
    workspace_error: Option<String>,
}

impl AppNotices {
    pub(super) fn new(workspace_error: Option<String>) -> Self {
        Self {
            toaster: Toaster::default(),
            workspace_error,
        }
    }

    pub(super) const fn toaster(&self) -> &Toaster {
        &self.toaster
    }

    pub(super) const fn toaster_mut(&mut self) -> &mut Toaster {
        &mut self.toaster
    }

    pub(super) fn workspace_error(&self) -> Option<&str> {
        self.workspace_error.as_deref()
    }

    pub(super) fn show_workspace_error(&mut self, error: impl Into<String>) {
        self.workspace_error = Some(error.into());
    }

    pub(super) fn clear_workspace_error(&mut self) {
        self.workspace_error = None;
    }

    /// Shows `error`, or clears the line once the failing action recovered.
    pub(super) fn set_workspace_error(&mut self, error: Option<String>) {
        self.workspace_error = error;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_workspace_error_line_shows_the_latest_failure_until_cleared() {
        let mut notices = AppNotices::new(Some("restored with a notice".to_string()));
        assert_eq!(notices.workspace_error(), Some("restored with a notice"));
        notices.show_workspace_error("first");
        notices.show_workspace_error("second");
        assert_eq!(notices.workspace_error(), Some("second"));
        notices.clear_workspace_error();
        assert_eq!(notices.workspace_error(), None);
        notices.set_workspace_error(Some("writer failed".to_string()));
        assert_eq!(notices.workspace_error(), Some("writer failed"));
        notices.set_workspace_error(None);
        assert_eq!(notices.workspace_error(), None);
    }
}
