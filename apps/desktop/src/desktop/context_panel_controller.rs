//! The pane's market-context panel: its snapshot, layout and API-key dialog.

use super::{Entity, InputState, WorkspaceChartState};
use aeris_context_runtime::{ContextSnapshot, ContextSource, ContextView};
use num_traits::ToPrimitive;
use std::sync::Arc;

pub(super) const CONTEXT_PANEL_INITIAL_HEIGHT: f32 = 248.0;
pub(super) const CONTEXT_PANEL_MINIMUM_HEIGHT: f32 = 140.0;
pub(super) const CONTEXT_PANEL_MAXIMUM_HEIGHT: f32 = 640.0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ContextPanelTab {
    #[default]
    Calendar,
    Energy,
    Commitments,
    Agriculture,
    Macro,
}

impl ContextPanelTab {
    pub(super) const ALL: [Self; 5] = [
        Self::Calendar,
        Self::Energy,
        Self::Commitments,
        Self::Agriculture,
        Self::Macro,
    ];

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Calendar => "Calendar",
            Self::Energy => "Energy",
            Self::Commitments => "COT",
            Self::Agriculture => "Agriculture",
            Self::Macro => "Macro",
        }
    }

    pub(super) const fn persisted(self) -> u32 {
        match self {
            Self::Calendar => 0,
            Self::Energy => 1,
            Self::Commitments => 2,
            Self::Agriculture => 3,
            Self::Macro => 4,
        }
    }

    const fn from_persisted(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Calendar),
            1 => Some(Self::Energy),
            2 => Some(Self::Commitments),
            3 => Some(Self::Agriculture),
            4 => Some(Self::Macro),
            _ => None,
        }
    }
}

/// The open API-key dialog: one masked input per keyed context source.
pub(super) struct ContextCredentialDialogState {
    pub(super) inputs: Vec<(ContextSource, Entity<InputState>)>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PanelVisibility {
    #[default]
    Hidden,
    Shown,
}

/// Owns everything the context panel shows. Setters report whether anything changed so the
/// owning surface decides on notifying and on marking its layout for persistence.
pub(super) struct ContextPanelController {
    view: Option<ContextView>,
    snapshot: Arc<ContextSnapshot>,
    visibility: PanelVisibility,
    tab: ContextPanelTab,
    height: f32,
    credential_dialog: Option<ContextCredentialDialogState>,
    credential_message: Option<String>,
    risk_message: Option<String>,
}

impl ContextPanelController {
    pub(super) fn restore(
        view: Option<ContextView>,
        restored: Option<&WorkspaceChartState>,
    ) -> Self {
        let snapshot = view.as_ref().map_or_else(
            || Arc::new(ContextSnapshot::empty(1)),
            ContextView::snapshot,
        );
        let visibility = if restored.is_some_and(|state| state.context_panel_visible) {
            PanelVisibility::Shown
        } else {
            PanelVisibility::Hidden
        };
        let tab = restored
            .and_then(|state| ContextPanelTab::from_persisted(state.context_panel_tab))
            .unwrap_or_default();
        Self {
            view,
            snapshot,
            visibility,
            tab,
            height: restored_context_panel_height(restored),
            credential_dialog: None,
            credential_message: None,
            risk_message: None,
        }
    }

    /// Adopts the context runtime's newer snapshot, if it published one.
    pub(super) fn refresh_snapshot(&mut self) -> bool {
        let Some(snapshot) = self.view.as_ref().map(ContextView::snapshot) else {
            return false;
        };
        if snapshot.revision == self.snapshot.revision {
            return false;
        }
        self.snapshot = snapshot;
        true
    }

    pub(super) const fn snapshot(&self) -> &Arc<ContextSnapshot> {
        &self.snapshot
    }

    pub(super) fn is_visible(&self) -> bool {
        self.visibility == PanelVisibility::Shown
    }

    pub(super) const fn tab(&self) -> ContextPanelTab {
        self.tab
    }

    pub(super) const fn height(&self) -> f32 {
        self.height
    }

    pub(super) fn set_visible(&mut self, visible: bool) -> bool {
        let visibility = if visible {
            PanelVisibility::Shown
        } else {
            PanelVisibility::Hidden
        };
        let changed = self.visibility != visibility;
        self.visibility = visibility;
        changed
    }

    pub(super) fn set_height(&mut self, height: f32) -> bool {
        let height = clamped_context_panel_height(height);
        let changed = (height - self.height).abs() > f32::EPSILON;
        if changed {
            self.height = height;
        }
        changed
    }

    pub(super) fn set_tab(&mut self, tab: ContextPanelTab) -> bool {
        let changed = self.tab != tab;
        self.tab = tab;
        changed
    }

    pub(super) fn persist_into(&self, state: &mut WorkspaceChartState) {
        state.context_panel_visible = self.is_visible();
        state.context_panel_tab = self.tab.persisted();
        state.context_panel_height = self.height.round().to_u32().unwrap_or(0);
    }

    pub(super) const fn credential_dialog(&self) -> Option<&ContextCredentialDialogState> {
        self.credential_dialog.as_ref()
    }

    pub(super) fn credential_message(&self) -> Option<&str> {
        self.credential_message.as_deref()
    }

    pub(super) fn risk_message(&self) -> Option<&str> {
        self.risk_message.as_deref()
    }

    /// Closes an open API-key dialog, or opens one with the inputs `open` builds.
    pub(super) fn toggle_credential_dialog(
        &mut self,
        open: impl FnOnce() -> Vec<(ContextSource, Entity<InputState>)>,
    ) {
        if self.credential_dialog.take().is_none() {
            self.credential_dialog = Some(ContextCredentialDialogState { inputs: open() });
            self.credential_message = None;
        }
    }

    pub(super) fn take_credential_dialog(&mut self) -> Option<ContextCredentialDialogState> {
        self.credential_dialog.take()
    }

    pub(super) fn set_credential_message(&mut self, message: impl Into<String>) {
        self.credential_message = Some(message.into());
    }

    pub(super) fn set_risk_message(&mut self, message: Option<String>) {
        self.risk_message = message;
    }
}

pub(super) fn restored_context_panel_height(restored: Option<&WorkspaceChartState>) -> f32 {
    restored
        .map(|state| state.context_panel_height)
        .filter(|height| *height != 0)
        .and_then(|height| height.to_f32())
        .map_or(CONTEXT_PANEL_INITIAL_HEIGHT, clamped_context_panel_height)
}

pub(super) fn clamped_context_panel_height(height: f32) -> f32 {
    height.clamp(CONTEXT_PANEL_MINIMUM_HEIGHT, CONTEXT_PANEL_MAXIMUM_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restored(height: u32) -> WorkspaceChartState {
        WorkspaceChartState {
            context_panel_height: height,
            ..WorkspaceChartState::default()
        }
    }

    #[test]
    fn restored_height_defaults_and_clamps_to_resizable_bounds() {
        let height = restored_context_panel_height;
        assert!((height(None) - CONTEXT_PANEL_INITIAL_HEIGHT).abs() < f32::EPSILON);
        // Workspaces saved before the panel was resizable persist zero.
        assert!((height(Some(&restored(0))) - CONTEXT_PANEL_INITIAL_HEIGHT).abs() < f32::EPSILON);
        assert!((height(Some(&restored(320))) - 320.0).abs() < f32::EPSILON);
        assert!(
            (height(Some(&restored(9_000))) - CONTEXT_PANEL_MAXIMUM_HEIGHT).abs() < f32::EPSILON
        );
        assert!(
            (clamped_context_panel_height(10.0) - CONTEXT_PANEL_MINIMUM_HEIGHT).abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn setters_report_only_real_changes_and_persist_the_layout() {
        let mut panel = ContextPanelController::restore(None, None);
        assert!(!panel.is_visible());
        assert!(!panel.set_visible(false));
        assert!(panel.set_visible(true));
        assert!(!panel.set_tab(ContextPanelTab::Calendar));
        assert!(panel.set_tab(ContextPanelTab::Energy));
        assert!(panel.set_height(320.0));
        assert!(!panel.set_height(320.0));
        assert!(
            !panel.refresh_snapshot(),
            "no context runtime publishes nothing"
        );

        let mut state = WorkspaceChartState::default();
        panel.persist_into(&mut state);
        assert!(state.context_panel_visible);
        assert_eq!(state.context_panel_tab, ContextPanelTab::Energy.persisted());
        assert_eq!(state.context_panel_height, 320);

        let reopened = ContextPanelController::restore(None, Some(&state));
        assert!(reopened.is_visible());
        assert_eq!(reopened.tab(), ContextPanelTab::Energy);
        assert!((reopened.height() - 320.0).abs() < f32::EPSILON);
    }

    #[test]
    fn unknown_persisted_tab_restores_the_calendar() {
        let state = WorkspaceChartState {
            context_panel_tab: 9,
            ..WorkspaceChartState::default()
        };
        let panel = ContextPanelController::restore(None, Some(&state));
        assert_eq!(panel.tab(), ContextPanelTab::Calendar);
    }
}
