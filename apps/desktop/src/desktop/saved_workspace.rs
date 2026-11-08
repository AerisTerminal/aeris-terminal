//! The durable workspace state the terminal keeps beside its open tabs: the saved-layout
//! library with the working drawing set of every layout an open tab shows, the user's chart
//! settings, indicator and drawing style templates, and the workspace writer with the snapshot it last
//! accepted. The terminal composes the open tabs and watchlist into a snapshot; this owner
//! decides whether that snapshot, the library or the templates changed since the last save.

use std::collections::BTreeMap;

use super::chart_drawings::LayoutDrawings;
use super::layout_library::LayoutLibrary;
use super::local_state::{
    MAXIMUM_CHART_SETTINGS_TEMPLATES, MAXIMUM_DRAWING_TEMPLATES, MAXIMUM_INDICATOR_TEMPLATES,
};
use super::workspace_persistence::{WorkspaceLayoutPersistence, WorkspaceSaveContent};
use super::{
    WorkspaceChartSettingsTemplateState, WorkspaceChromeState, WorkspaceDrawingTemplateState,
    WorkspaceTabState, WorkspaceWatchlistEntryState,
};
use aeris_contracts::{WorkspaceIndicatorTemplateState, WorkspaceScreenerScanState};

/// A change the compared snapshot does not show, so it is saved even when the snapshot is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum UnsavedEdit {
    ChartSettingsTemplates,
    DrawingTemplates,
    Watchlist,
    IndicatorTemplates,
}

impl UnsavedEdit {
    const fn bit(self) -> u8 {
        match self {
            Self::ChartSettingsTemplates => 1,
            Self::DrawingTemplates => 2,
            Self::Watchlist => 4,
            Self::IndicatorTemplates => 8,
        }
    }
}

/// The open tabs, watchlist, shell chrome and saved scans as the workspace file stores them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct WorkspaceSnapshot {
    pub(super) layout: Vec<WorkspaceTabState>,
    pub(super) active_workspace_id: u64,
    pub(super) watchlist: Vec<WorkspaceWatchlistEntryState>,
    /// Chart chrome preferences persist with the document through the same writer.
    pub(super) chart_chrome: WorkspaceChromeState,
    pub(super) screener_scans: Vec<WorkspaceScreenerScanState>,
}

/// What a save request did.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum WorkspaceSave {
    /// This window has no workspace writer.
    Unavailable,
    /// Nothing changed since the last accepted save.
    Unchanged,
    /// The writer accepted the changed workspace.
    Requested,
    /// The writer refused the request; the change stays unsaved.
    Failed(String),
}

pub(super) struct SavedWorkspace {
    writer: Option<WorkspaceLayoutPersistence>,
    /// The snapshot and library revision of the last save the writer accepted.
    saved: WorkspaceSnapshot,
    saved_library_revision: u64,
    unsaved: u8,
    layouts: LayoutLibrary,
    /// The working drawing set of every saved layout an open tab shows, shared by those tabs.
    layout_drawings: BTreeMap<u64, LayoutDrawings>,
    chart_settings_templates: Vec<WorkspaceChartSettingsTemplateState>,
    /// User-named drawing style templates of every kind, shared by every chart.
    drawing_templates: Vec<WorkspaceDrawingTemplateState>,
    /// User-named indicator templates, shared by every chart.
    indicator_templates: Vec<WorkspaceIndicatorTemplateState>,
}

impl SavedWorkspace {
    /// `restored` is the workspace as it opened. The library counts as unsaved until its
    /// first save, so a library the restore changed is written back.
    pub(super) const fn new(
        writer: Option<WorkspaceLayoutPersistence>,
        restored: WorkspaceSnapshot,
        layouts: LayoutLibrary,
        layout_drawings: BTreeMap<u64, LayoutDrawings>,
        chart_settings_templates: Vec<WorkspaceChartSettingsTemplateState>,
        drawing_templates: Vec<WorkspaceDrawingTemplateState>,
        indicator_templates: Vec<WorkspaceIndicatorTemplateState>,
    ) -> Self {
        Self {
            writer,
            saved: restored,
            saved_library_revision: 0,
            unsaved: 0,
            layouts,
            layout_drawings,
            chart_settings_templates,
            drawing_templates,
            indicator_templates,
        }
    }

    pub(super) const fn writer(&self) -> Option<&WorkspaceLayoutPersistence> {
        self.writer.as_ref()
    }

    pub(super) const fn layouts(&self) -> &LayoutLibrary {
        &self.layouts
    }

    pub(super) const fn layouts_mut(&mut self) -> &mut LayoutLibrary {
        &mut self.layouts
    }

    pub(super) const fn layout_drawings(&self) -> &BTreeMap<u64, LayoutDrawings> {
        &self.layout_drawings
    }

    pub(super) const fn layout_drawings_mut(&mut self) -> &mut BTreeMap<u64, LayoutDrawings> {
        &mut self.layout_drawings
    }

    /// The library with the working drawing set of `layout_id`, borrowed together so a tab
    /// can save its drawings into its layout.
    pub(super) fn layout_with_drawings_mut(
        &mut self,
        layout_id: u64,
    ) -> (&mut LayoutLibrary, Option<&mut LayoutDrawings>) {
        (&mut self.layouts, self.layout_drawings.get_mut(&layout_id))
    }

    pub(super) fn chart_settings_templates(&self) -> &[WorkspaceChartSettingsTemplateState] {
        &self.chart_settings_templates
    }

    /// Stores `template`, replacing the template with its name in any letter case. Refused
    /// when the name is new and the templates are full.
    pub(super) fn store_chart_settings_template(
        &mut self,
        template: WorkspaceChartSettingsTemplateState,
    ) -> bool {
        if let Some(existing) = self
            .chart_settings_templates
            .iter_mut()
            .find(|existing| existing.name.eq_ignore_ascii_case(&template.name))
        {
            *existing = template;
        } else if self.chart_settings_templates.len() < MAXIMUM_CHART_SETTINGS_TEMPLATES {
            self.chart_settings_templates.push(template);
        } else {
            return false;
        }
        self.mark_unsaved(UnsavedEdit::ChartSettingsTemplates);
        true
    }

    /// Removes the template at `index`; reports whether there was one.
    pub(super) fn remove_chart_settings_template(&mut self, index: usize) -> bool {
        if index >= self.chart_settings_templates.len() {
            return false;
        }
        self.chart_settings_templates.remove(index);
        self.mark_unsaved(UnsavedEdit::ChartSettingsTemplates);
        true
    }

    pub(super) fn indicator_templates(&self) -> &[WorkspaceIndicatorTemplateState] {
        &self.indicator_templates
    }

    /// Stores `template`, replacing the template with its name in any letter case and keeping
    /// that template's star. Refused when the name is new and the templates are full.
    pub(super) fn store_indicator_template(
        &mut self,
        mut template: WorkspaceIndicatorTemplateState,
    ) -> bool {
        if let Some(existing) = self
            .indicator_templates
            .iter_mut()
            .find(|existing| existing.name.eq_ignore_ascii_case(&template.name))
        {
            template.favorite = existing.favorite;
            *existing = template;
        } else if self.indicator_templates.len() < MAXIMUM_INDICATOR_TEMPLATES {
            self.indicator_templates.push(template);
        } else {
            return false;
        }
        self.mark_unsaved(UnsavedEdit::IndicatorTemplates);
        true
    }

    /// Removes the template at `index`; reports whether there was one.
    pub(super) fn remove_indicator_template(&mut self, index: usize) -> bool {
        if index >= self.indicator_templates.len() {
            return false;
        }
        self.indicator_templates.remove(index);
        self.mark_unsaved(UnsavedEdit::IndicatorTemplates);
        true
    }

    /// Stars or unstars the template at `index`; reports whether there was one.
    pub(super) fn toggle_indicator_template_favorite(&mut self, index: usize) -> bool {
        let Some(template) = self.indicator_templates.get_mut(index) else {
            return false;
        };
        template.favorite = !template.favorite;
        self.mark_unsaved(UnsavedEdit::IndicatorTemplates);
        true
    }

    pub(super) fn drawing_templates(&self) -> &[WorkspaceDrawingTemplateState] {
        &self.drawing_templates
    }

    /// Stores `template`, replacing the template of its drawing kind with its name in any
    /// letter case. Refused when the name is new and the templates are full.
    pub(super) fn store_drawing_template(
        &mut self,
        template: WorkspaceDrawingTemplateState,
    ) -> bool {
        if let Some(existing) = self.drawing_templates.iter_mut().find(|existing| {
            existing.kind == template.kind && existing.name.eq_ignore_ascii_case(&template.name)
        }) {
            *existing = template;
        } else if self.drawing_templates.len() < MAXIMUM_DRAWING_TEMPLATES {
            self.drawing_templates.push(template);
        } else {
            return false;
        }
        self.mark_unsaved(UnsavedEdit::DrawingTemplates);
        true
    }

    /// Removes the template at `index`; reports whether there was one.
    pub(super) fn remove_drawing_template(&mut self, index: usize) -> bool {
        if index >= self.drawing_templates.len() {
            return false;
        }
        self.drawing_templates.remove(index);
        self.mark_unsaved(UnsavedEdit::DrawingTemplates);
        true
    }

    pub(super) const fn mark_unsaved(&mut self, edit: UnsavedEdit) {
        self.unsaved |= edit.bit();
    }

    /// Hands `snapshot` with the library and templates to the writer when anything changed
    /// since the last accepted save.
    pub(super) fn save_if_changed(&mut self, snapshot: WorkspaceSnapshot) -> WorkspaceSave {
        let Some(writer) = self.writer.as_ref() else {
            return WorkspaceSave::Unavailable;
        };
        let library_revision = self.layouts.revision();
        if self.is_saved(&snapshot, library_revision) {
            return WorkspaceSave::Unchanged;
        }
        if let Err(error) = writer.request_with_chart_settings(WorkspaceSaveContent {
            active_workspace_id: snapshot.active_workspace_id,
            workspace_tabs: snapshot.layout.clone(),
            chart_settings_templates: self.chart_settings_templates.clone(),
            watchlist_entries: snapshot.watchlist.clone(),
            drawing_templates: self.drawing_templates.clone(),
            saved_layouts: self.layouts.states(),
            chart_chrome: snapshot.chart_chrome.clone(),
            screener_scans: snapshot.screener_scans.clone(),
            indicator_templates: self.indicator_templates.clone(),
        }) {
            return WorkspaceSave::Failed(error);
        }
        self.record_saved(snapshot, library_revision);
        WorkspaceSave::Requested
    }

    fn is_saved(&self, snapshot: &WorkspaceSnapshot, library_revision: u64) -> bool {
        self.unsaved == 0
            && library_revision == self.saved_library_revision
            && *snapshot == self.saved
    }

    fn record_saved(&mut self, snapshot: WorkspaceSnapshot, library_revision: u64) {
        self.saved = snapshot;
        self.saved_library_revision = library_revision;
        self.unsaved = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(restored: WorkspaceSnapshot) -> SavedWorkspace {
        SavedWorkspace::new(
            None,
            restored,
            LayoutLibrary::new(Vec::new()),
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    fn snapshot(active_workspace_id: u64) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            active_workspace_id,
            ..WorkspaceSnapshot::default()
        }
    }

    fn chart_template(name: &str) -> WorkspaceChartSettingsTemplateState {
        WorkspaceChartSettingsTemplateState {
            name: name.to_string(),
            ..WorkspaceChartSettingsTemplateState::default()
        }
    }

    fn drawing_template(kind: &str, name: &str) -> WorkspaceDrawingTemplateState {
        WorkspaceDrawingTemplateState {
            kind: kind.to_string(),
            name: name.to_string(),
            template_json: String::new(),
        }
    }

    #[test]
    fn a_save_is_needed_until_every_change_is_recorded() {
        let mut saved = workspace(snapshot(1));
        let revision = saved.layouts().revision();
        saved.record_saved(snapshot(1), revision);
        assert!(saved.is_saved(&snapshot(1), revision));
        assert!(
            !saved.is_saved(&snapshot(2), revision),
            "a changed snapshot"
        );
        let chrome_changed = WorkspaceSnapshot {
            chart_chrome: WorkspaceChromeState {
                chart_type: "line".to_string(),
                ..WorkspaceChromeState::default()
            },
            ..snapshot(1)
        };
        assert!(
            !saved.is_saved(&chrome_changed, revision),
            "a changed chrome preference"
        );
        assert!(
            !saved.is_saved(&snapshot(1), revision + 1),
            "a changed layout library"
        );
        for edit in [
            UnsavedEdit::ChartSettingsTemplates,
            UnsavedEdit::DrawingTemplates,
            UnsavedEdit::Watchlist,
            UnsavedEdit::IndicatorTemplates,
        ] {
            saved.mark_unsaved(edit);
            assert!(
                !saved.is_saved(&snapshot(1), revision),
                "{edit:?} is unsaved"
            );
            saved.record_saved(snapshot(1), revision);
            assert!(saved.is_saved(&snapshot(1), revision));
        }
    }

    #[test]
    fn without_a_writer_changes_stay_unsaved() {
        let mut saved = workspace(snapshot(1));
        saved.mark_unsaved(UnsavedEdit::Watchlist);
        assert_eq!(
            saved.save_if_changed(snapshot(2)),
            WorkspaceSave::Unavailable
        );
        assert!(!saved.is_saved(&snapshot(2), saved.layouts().revision()));
    }

    #[test]
    fn chart_settings_templates_replace_by_name_within_their_bound() {
        let mut saved = workspace(snapshot(1));
        saved.record_saved(snapshot(1), saved.layouts().revision());
        assert!(saved.store_chart_settings_template(chart_template("Day")));
        assert!(!saved.is_saved(&snapshot(1), saved.layouts().revision()));
        assert!(saved.store_chart_settings_template(chart_template("DAY")));
        assert_eq!(
            saved.chart_settings_templates().len(),
            1,
            "names ignore case"
        );
        for index in 1..MAXIMUM_CHART_SETTINGS_TEMPLATES {
            assert!(saved.store_chart_settings_template(chart_template(&format!("T{index}"))));
        }
        assert!(!saved.store_chart_settings_template(chart_template("Overflow")));
        assert!(saved.store_chart_settings_template(chart_template("day")));
        assert!(saved.remove_chart_settings_template(0));
        assert!(!saved.remove_chart_settings_template(MAXIMUM_CHART_SETTINGS_TEMPLATES));
    }

    #[test]
    fn indicator_templates_replace_by_name_keep_their_star_and_stay_bounded() {
        let template = |name: &str| WorkspaceIndicatorTemplateState {
            name: name.to_string(),
            ..WorkspaceIndicatorTemplateState::default()
        };
        let mut saved = workspace(snapshot(1));
        saved.record_saved(snapshot(1), saved.layouts().revision());
        assert!(saved.store_indicator_template(template("Scalping")));
        assert!(!saved.is_saved(&snapshot(1), saved.layouts().revision()));
        assert!(saved.toggle_indicator_template_favorite(0));
        let replaced = WorkspaceIndicatorTemplateState {
            studies: vec![aeris_contracts::WorkspaceChartStudyState::default()],
            ..template("SCALPING")
        };
        assert!(saved.store_indicator_template(replaced));
        assert_eq!(saved.indicator_templates().len(), 1, "names ignore case");
        let stored = &saved.indicator_templates()[0];
        assert_eq!(stored.name, "SCALPING");
        assert_eq!(stored.studies.len(), 1);
        assert!(stored.favorite, "saving over a template keeps its star");
        for index in 1..MAXIMUM_INDICATOR_TEMPLATES {
            assert!(saved.store_indicator_template(template(&format!("T{index}"))));
        }
        assert!(!saved.store_indicator_template(template("Overflow")));
        assert!(saved.remove_indicator_template(0));
        assert!(!saved.remove_indicator_template(MAXIMUM_INDICATOR_TEMPLATES));
        assert!(!saved.toggle_indicator_template_favorite(MAXIMUM_INDICATOR_TEMPLATES));
    }

    #[test]
    fn drawing_templates_replace_by_kind_and_name_within_their_bound() {
        let mut saved = workspace(snapshot(1));
        assert!(saved.store_drawing_template(drawing_template("ray", "Blue")));
        assert!(saved.store_drawing_template(drawing_template("ray", "BLUE")));
        assert!(saved.store_drawing_template(drawing_template("line", "Blue")));
        assert_eq!(saved.drawing_templates().len(), 2);
        for index in 2..MAXIMUM_DRAWING_TEMPLATES {
            assert!(saved.store_drawing_template(drawing_template("ray", &format!("R{index}"))));
        }
        assert!(!saved.store_drawing_template(drawing_template("ray", "Overflow")));
        saved.record_saved(snapshot(1), saved.layouts().revision());
        assert!(saved.remove_drawing_template(1));
        assert!(!saved.is_saved(&snapshot(1), saved.layouts().revision()));
        assert!(!saved.remove_drawing_template(MAXIMUM_DRAWING_TEMPLATES));
    }
}
