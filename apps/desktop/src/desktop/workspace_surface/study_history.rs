//! Runtime studies in the chart's undo history. Adding or removing studies records them as they
//! stood; undo and redo remove them or register them again through the study runtime.

use super::{
    App, Context, HashMap, PendingRuntimeStudyState, StudyInstanceId, WorkspaceChartStudyState,
    WorkspaceStudyDependencyKind, WorkspaceSurface,
};
use aeris_chart_integration::ChartHostEditId;
use aeris_localization::{tr, workspace};
use std::collections::HashSet;

/// Runtime studies one edit added or removed, as they stood.
struct RuntimeStudyEdit {
    added: bool,
    studies: Vec<WorkspaceChartStudyState>,
}

impl WorkspaceSurface {
    /// Records runtime studies the trader added or removed as one step of the chart's history.
    pub(super) fn record_runtime_study_edit(
        &self,
        added: bool,
        studies: Vec<WorkspaceChartStudyState>,
        cx: &mut Context<Self>,
    ) {
        if studies.is_empty() {
            return;
        }
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, _| {
                chart.record_host_edit(RuntimeStudyEdit { added, studies });
            });
        }
    }

    /// The durable states of the studies the trader sees, with the chart's visibility: all of
    /// them, or only `root` and the studies reading its outputs.
    pub(super) fn shown_study_states(
        &self,
        root: Option<StudyInstanceId>,
        cx: &App,
    ) -> Vec<WorkspaceChartStudyState> {
        let chart = self.chart.as_ref().map(|chart| chart.read(cx));
        let visible =
            |study_id: StudyInstanceId| chart.and_then(|chart| chart.study_visible(study_id.get()));
        match root {
            Some(study_id) => self.studies.persisted_subtree(study_id, visible),
            None => self.studies.shown_persisted_states(visible),
        }
    }

    /// Follows the chart's undo or redo of a runtime-study edit.
    pub(in crate::desktop) fn follow_runtime_study_edit(
        &mut self,
        edit: ChartHostEditId,
        undo: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(chart) = self.chart.clone() else {
            return;
        };
        let Some((added, studies)) = chart
            .read(cx)
            .host_edit::<RuntimeStudyEdit>(edit)
            .map(|edit| (edit.added, edit.studies.clone()))
        else {
            return;
        };
        if added == undo {
            self.withdraw_runtime_studies(&studies, cx);
            return;
        }
        let restored = self.reinstate_runtime_studies(studies, cx);
        chart.update(cx, |chart, _| {
            if let Some(edit) = chart.host_edit_mut::<RuntimeStudyEdit>(edit) {
                edit.studies = restored;
            }
        });
    }

    fn withdraw_runtime_studies(
        &mut self,
        studies: &[WorkspaceChartStudyState],
        cx: &mut Context<Self>,
    ) {
        let local_ids = studies
            .iter()
            .map(|study| study.local_id)
            .collect::<HashSet<_>>();
        let mut changed = self.studies.cancel_unregistered(&local_ids);
        for study_id in self.studies.removable_ids_for(&local_ids) {
            changed |= self.request_study_removal(study_id, cx);
        }
        if changed {
            self.persistence.mark_dirty();
        }
    }

    /// Registers the studies again under fresh local identities, so a removal of the originals
    /// still in flight cannot take them, and returns them as they are now known.
    fn reinstate_runtime_studies(
        &mut self,
        studies: Vec<WorkspaceChartStudyState>,
        cx: &mut Context<Self>,
    ) -> Vec<WorkspaceChartStudyState> {
        let mut local_ids = HashMap::with_capacity(studies.len());
        for study in &studies {
            let Some(local_id) = self.studies.allocate_local_id() else {
                Self::post_study_failure(tr!(workspace::STUDY_IDENTITY_EXHAUSTED), cx);
                return studies;
            };
            local_ids.insert(study.local_id, local_id);
        }
        let restored = studies
            .into_iter()
            .map(|mut study| {
                if let Some(&local_id) = local_ids.get(&study.local_id) {
                    study.local_id = local_id;
                }
                for dependency in &mut study.dependencies {
                    let reads_study = WorkspaceStudyDependencyKind::try_from(dependency.kind)
                        .is_ok_and(|kind| kind == WorkspaceStudyDependencyKind::StudyOutput);
                    if reads_study
                        && let Some(&local_id) = local_ids.get(&dependency.study_local_id)
                    {
                        dependency.study_local_id = local_id;
                    }
                }
                study
            })
            .collect::<Vec<_>>();
        self.studies
            .defer_restored(restored.iter().map(|persisted| PendingRuntimeStudyState {
                persisted: persisted.clone(),
                resolved_chart_series: None,
                remove_on_registration: false,
                persist_on_registration: true,
                blocked: false,
            }));
        self.persistence.mark_dirty();
        self.synchronize_runtime_studies(cx);
        restored
    }
}
