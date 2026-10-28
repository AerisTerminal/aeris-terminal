//! The pane's runtime studies: registered, pending and deferred studies, their reinitializations,
//! removals and the retry pacing of work the market worker could not accept.

use super::{
    BarSeriesKey, PendingRuntimeStudyState, PendingStudyReinitialization, RuntimeStudyState,
    STUDY_RETRY_DELAY, StudyInstanceId, WorkspaceChartStudyState, WorkspaceStudyDependencyKind,
    WorkspaceStudySettingState,
};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::TrySendError;
use std::time::Instant;

/// What a settings request needs from the market worker.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum StudySettingsRequest {
    /// The pane holds no active study with that identity.
    Inactive,
    /// A reinitialization is in flight; the settings wait for it, or are dropped when they
    /// match its target.
    Waiting,
    /// The study already computes these settings.
    Unchanged,
    /// The study must be reinitialized as this replacement.
    Reinitialize(WorkspaceChartStudyState),
}

pub(super) struct StudiesController {
    active: Vec<RuntimeStudyState>,
    pending: HashMap<u64, PendingRuntimeStudyState>,
    reinitializing: HashMap<StudyInstanceId, PendingStudyReinitialization>,
    /// The newest user settings for a study whose reinitialization is still in flight. Only the
    /// latest replaces an earlier one, and it is sent once the runtime answers, so editing a
    /// setting keeps at most one request per study in flight.
    queued_settings: HashMap<StudyInstanceId, Vec<WorkspaceStudySettingState>>,
    /// Rejected automatic reinitializations: the chart series each targeted and
    /// when it may be retried.
    reinitialization_retries: HashMap<StudyInstanceId, (BarSeriesKey, Instant)>,
    suppressing_outputs: HashSet<StudyInstanceId>,
    automatic_removals: HashSet<StudyInstanceId>,
    removing: HashSet<StudyInstanceId>,
    deferred: Vec<PendingRuntimeStudyState>,
    /// The chart series (none when the selection cannot form one) whose deferred
    /// registration last failed to queue, and when it may be tried again.
    registration_retry: Option<(Option<BarSeriesKey>, Instant)>,
    next_local_id: u64,
}

impl Default for StudiesController {
    fn default() -> Self {
        Self {
            active: Vec::new(),
            pending: HashMap::new(),
            reinitializing: HashMap::new(),
            queued_settings: HashMap::new(),
            reinitialization_retries: HashMap::new(),
            suppressing_outputs: HashSet::new(),
            automatic_removals: HashSet::new(),
            removing: HashSet::new(),
            deferred: Vec::new(),
            registration_retry: None,
            next_local_id: 1,
        }
    }
}

impl StudiesController {
    /// Adopts restored studies, which wait for a chart before they are registered. New studies
    /// take local identities above every restored one.
    pub(super) fn restore(deferred: Vec<PendingRuntimeStudyState>) -> Self {
        let next_local_id = deferred
            .iter()
            .map(|state| state.persisted.local_id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .unwrap_or(0);
        Self {
            deferred,
            next_local_id,
            ..Self::default()
        }
    }

    pub(super) fn active(&self) -> &[RuntimeStudyState] {
        &self.active
    }

    pub(super) fn active_study(&self, study_id: StudyInstanceId) -> Option<&RuntimeStudyState> {
        self.active.iter().find(|state| state.study_id == study_id)
    }

    pub(super) fn is_active(&self, study_id: StudyInstanceId) -> bool {
        self.active_study(study_id).is_some()
    }

    pub(super) fn is_removing(&self, study_id: StudyInstanceId) -> bool {
        self.removing.contains(&study_id)
    }

    /// Whether a removal or reinitialization of the study is in flight.
    pub(super) fn is_busy(&self, study_id: StudyInstanceId) -> bool {
        self.removing.contains(&study_id) || self.reinitializing.contains_key(&study_id)
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Whether any study is registered, waiting for registration or deferred.
    pub(super) fn has_any(&self) -> bool {
        !self.active.is_empty() || !self.pending.is_empty() || !self.deferred.is_empty()
    }

    /// The studies the trader still sees: removals and cancelled registrations do not count.
    pub(super) fn count(&self) -> usize {
        self.active
            .iter()
            .filter(|state| {
                !self.removing.contains(&state.study_id)
                    && !self.automatic_removals.contains(&state.study_id)
            })
            .count()
            + self
                .pending
                .values()
                .filter(|state| !state.remove_on_registration)
                .count()
            + self.deferred.len()
    }

    /// The durable study list in local-identity order. `visible` reports the chart's current
    /// visibility of an active study, when the chart shows it.
    pub(super) fn persisted_states(
        &self,
        mut visible: impl FnMut(StudyInstanceId) -> Option<bool>,
    ) -> Vec<WorkspaceChartStudyState> {
        let mut persisted = self
            .active
            .iter()
            .filter(|state| !self.automatic_removals.contains(&state.study_id))
            .map(|state| {
                let mut persisted = state.persisted.clone();
                persisted.visible = visible(state.study_id).unwrap_or(persisted.visible);
                persisted
            })
            .chain(
                self.pending
                    .values()
                    .filter(|state| !state.remove_on_registration)
                    .map(|state| state.persisted.clone()),
            )
            .chain(
                self.deferred
                    .iter()
                    .filter(|state| !state.remove_on_registration)
                    .map(|state| state.persisted.clone()),
            )
            .collect::<Vec<_>>();
        persisted.sort_by_key(|state| state.local_id);
        persisted
    }

    /// Adopts the chart's visibility of each active study it reports.
    pub(super) fn retain_visibility(
        &mut self,
        mut visible: impl FnMut(StudyInstanceId) -> Option<bool>,
    ) {
        for state in &mut self.active {
            if let Some(visible) = visible(state.study_id) {
                state.persisted.visible = visible;
            }
        }
    }

    /// Active studies without a removal in flight.
    pub(super) fn removable_ids(&self) -> Vec<StudyInstanceId> {
        self.active
            .iter()
            .filter(|state| !self.removing.contains(&state.study_id))
            .map(|state| state.study_id)
            .collect()
    }

    pub(super) fn allocate_local_id(&mut self) -> Option<u64> {
        let local_id = self.next_local_id;
        if local_id == 0 {
            return None;
        }
        self.next_local_id = self.next_local_id.checked_add(1).unwrap_or(0);
        Some(local_id)
    }

    pub(super) fn insert_pending(
        &mut self,
        request_sequence: u64,
        state: PendingRuntimeStudyState,
    ) {
        self.pending.insert(request_sequence, state);
    }

    pub(super) fn take_pending(
        &mut self,
        request_sequence: u64,
    ) -> Option<PendingRuntimeStudyState> {
        self.pending.remove(&request_sequence)
    }

    pub(super) fn activate(&mut self, state: RuntimeStudyState) {
        self.active.push(state);
    }

    /// Adopts a study the runtime registered after the pane cancelled it; its removal is
    /// dispatched by `dispatch_automatic_removals` and its outputs stay hidden meanwhile.
    pub(super) fn activate_for_removal(&mut self, state: RuntimeStudyState) {
        self.automatic_removals.insert(state.study_id);
        self.active.push(state);
    }

    /// The first deferred study that is not blocked on a failed registration.
    pub(super) fn next_deferred_index(&self) -> Option<usize> {
        self.deferred.iter().position(|state| !state.blocked)
    }

    pub(super) fn take_deferred(&mut self, index: usize) -> Option<PendingRuntimeStudyState> {
        (index < self.deferred.len()).then(|| self.deferred.remove(index))
    }

    /// Puts a deferred study that could not be queued back where `take_deferred` found it.
    pub(super) fn return_deferred(&mut self, index: usize, state: PendingRuntimeStudyState) {
        self.deferred.insert(index.min(self.deferred.len()), state);
    }

    /// Defers a study whose registration the runtime rejected; it is not retried on its own.
    pub(super) fn push_front_blocked(&mut self, mut state: PendingRuntimeStudyState) {
        state.blocked = true;
        self.deferred.insert(0, state);
    }

    /// Holds back deferred registrations after one could not be queued. Returns
    /// whether the series had not already failed, so the failure is reported once
    /// instead of on every market poll.
    pub(super) fn defer_registration(
        &mut self,
        series: Option<BarSeriesKey>,
        now: Instant,
    ) -> bool {
        let first = self
            .registration_retry
            .as_ref()
            .is_none_or(|(failed, _)| *failed != series);
        self.registration_retry = Some((series, now + STUDY_RETRY_DELAY));
        first
    }

    pub(super) fn registration_due(&self, series: Option<&BarSeriesKey>, now: Instant) -> bool {
        self.registration_retry
            .as_ref()
            .is_none_or(|(failed, retry_at)| failed.as_ref() != series || now >= *retry_at)
    }

    /// A deferred registration was queued, so later ones need not wait.
    pub(super) fn registration_succeeded(&mut self) {
        self.registration_retry = None;
    }

    /// The earliest retry deadline still ahead of `now`: a deferred registration or a held-back
    /// automatic reinitialization. A passed deadline no longer holds anything back.
    pub(super) fn next_retry_at(&self, now: Instant) -> Option<Instant> {
        let registration = self
            .registration_retry
            .as_ref()
            .filter(|_| self.next_deferred_index().is_some())
            .map(|(_, retry_at)| *retry_at);
        registration
            .into_iter()
            .chain(
                self.reinitialization_retries
                    .values()
                    .map(|(_, retry_at)| *retry_at),
            )
            .filter(|retry_at| *retry_at > now)
            .min()
    }

    /// Queues the removal of every cancelled study not already being removed. Returns whether
    /// every one was queued.
    pub(super) fn dispatch_automatic_removals(
        &mut self,
        mut try_remove: impl FnMut(StudyInstanceId) -> Result<(), TrySendError<StudyInstanceId>>,
    ) -> bool {
        let mut retry = self
            .automatic_removals
            .iter()
            .filter(|study_id| !self.removing.contains(study_id))
            .copied()
            .collect::<Vec<_>>();
        retry.sort_unstable();
        for study_id in retry {
            match try_remove(study_id) {
                Ok(()) => {
                    self.removing.insert(study_id);
                }
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => return false,
            }
        }
        true
    }

    pub(super) fn mark_removing(&mut self, study_id: StudyInstanceId) {
        self.removing.insert(study_id);
    }

    /// Ends a failed removal. Returns whether it was an automatic removal, which is retried
    /// instead of reported.
    pub(super) fn removal_failed(&mut self, study_id: StudyInstanceId) -> bool {
        self.removing.remove(&study_id);
        self.automatic_removals.contains(&study_id)
    }

    /// Cancels every pending registration and drops every deferred study. Returns whether a
    /// study the trader still saw was discarded.
    pub(super) fn discard_unregistered(&mut self) -> bool {
        let changed = !self.deferred.is_empty()
            || self
                .pending
                .values()
                .any(|state| !state.remove_on_registration);
        for pending in self.pending.values_mut() {
            pending.remove_on_registration = true;
        }
        self.deferred.clear();
        self.registration_retry = None;
        changed
    }

    pub(super) fn begin_reinitialization(
        &mut self,
        study_id: StudyInstanceId,
        pending: PendingStudyReinitialization,
    ) {
        self.reinitializing.insert(study_id, pending);
        self.suppressing_outputs.insert(study_id);
    }

    pub(super) fn complete_reinitialization(&mut self, study_id: StudyInstanceId) -> Option<bool> {
        let pending = self.reinitializing.remove(&study_id)?;
        self.reinitialization_retries.remove(&study_id);
        let mut persisted_changed = false;
        if let Some(state) = self
            .active
            .iter_mut()
            .find(|state| state.study_id == study_id)
        {
            state.resolved_chart_series = Some(pending.series);
            if let Some(settings) = pending.replacement_settings {
                state.persisted.settings = settings;
                persisted_changed = true;
            }
        }
        Some(persisted_changed)
    }

    /// Cancels a rejected reinitialization. An automatic one (no settings
    /// replacement) waits before it is retried for the same series, so a runtime
    /// that keeps rejecting it is not asked again on every market update.
    pub(super) fn reject_reinitialization(&mut self, study_id: StudyInstanceId, now: Instant) {
        self.suppressing_outputs.remove(&study_id);
        if let Some(pending) = self.reinitializing.remove(&study_id)
            && pending.replacement_settings.is_none()
        {
            self.reinitialization_retries
                .insert(study_id, (pending.series, now + STUDY_RETRY_DELAY));
        }
    }

    /// Holds back an automatic reinitialization the pane could not queue. Returns
    /// whether the series had not already failed, so the failure is reported once
    /// instead of on every market poll.
    pub(super) fn defer_reinitialization(
        &mut self,
        study_id: StudyInstanceId,
        series: BarSeriesKey,
        now: Instant,
    ) -> bool {
        let first = self
            .reinitialization_retries
            .get(&study_id)
            .is_none_or(|(failed, _)| *failed != series);
        self.reinitialization_retries
            .insert(study_id, (series, now + STUDY_RETRY_DELAY));
        first
    }

    pub(super) fn reinitialization_due(
        &self,
        study_id: StudyInstanceId,
        series: &BarSeriesKey,
        now: Instant,
    ) -> bool {
        self.reinitialization_retries
            .get(&study_id)
            .is_none_or(|(rejected, retry_at)| rejected != series || now >= *retry_at)
    }

    /// Active studies that follow the chart series (`uses_current_chart`) but were resolved
    /// against another one, are not being removed or reinitialized, and are due for a retry.
    pub(super) fn stale_studies(
        &self,
        series: &BarSeriesKey,
        now: Instant,
        uses_current_chart: impl Fn(&WorkspaceChartStudyState) -> bool,
    ) -> Vec<StudyInstanceId> {
        self.active
            .iter()
            .filter(|state| {
                uses_current_chart(&state.persisted)
                    && state.resolved_chart_series.as_ref() != Some(series)
                    && !self.removing.contains(&state.study_id)
                    && !self.automatic_removals.contains(&state.study_id)
                    && !self.reinitializing.contains_key(&state.study_id)
                    && self.reinitialization_due(state.study_id, series, now)
            })
            .map(|state| state.study_id)
            .collect()
    }

    pub(super) fn invalidate_study_outputs(&mut self, study_ids: &[StudyInstanceId]) {
        for study_id in study_ids {
            if !self.reinitializing.contains_key(study_id) {
                self.suppressing_outputs.remove(study_id);
            }
        }
    }

    pub(super) fn suppresses_output(&self, study_id: StudyInstanceId) -> bool {
        self.suppressing_outputs.contains(&study_id) || self.automatic_removals.contains(&study_id)
    }

    pub(super) fn remove_runtime_subtree(&mut self, study_ids: &[StudyInstanceId]) -> bool {
        let removed_runtime_ids = study_ids.iter().copied().collect::<HashSet<_>>();
        let mut removed_local_ids = self
            .active
            .iter()
            .filter(|state| removed_runtime_ids.contains(&state.study_id))
            .map(|state| state.persisted.local_id)
            .collect::<HashSet<_>>();

        loop {
            let previous_len = removed_local_ids.len();
            for state in self.pending.values().chain(self.deferred.iter()) {
                if state.persisted.dependencies.iter().any(|dependency| {
                    WorkspaceStudyDependencyKind::try_from(dependency.kind).is_ok_and(|kind| {
                        kind == WorkspaceStudyDependencyKind::StudyOutput
                            && removed_local_ids.contains(&dependency.study_local_id)
                    })
                }) {
                    removed_local_ids.insert(state.persisted.local_id);
                }
            }
            if removed_local_ids.len() == previous_len {
                break;
            }
        }

        self.queued_settings
            .retain(|study_id, _| !removed_runtime_ids.contains(study_id));
        let previous_len = self.active.len();
        self.active
            .retain(|state| !removed_runtime_ids.contains(&state.study_id));
        let mut changed = self.active.len() != previous_len;
        for state in self.pending.values_mut() {
            if removed_local_ids.contains(&state.persisted.local_id)
                && !state.remove_on_registration
            {
                state.remove_on_registration = true;
                changed = true;
            }
        }
        let previous_deferred_len = self.deferred.len();
        self.deferred
            .retain(|state| !removed_local_ids.contains(&state.persisted.local_id));
        changed |= self.deferred.len() != previous_deferred_len;
        for study_id in study_ids {
            self.reinitializing.remove(study_id);
            self.reinitialization_retries.remove(study_id);
            self.suppressing_outputs.remove(study_id);
            self.automatic_removals.remove(study_id);
            self.removing.remove(study_id);
        }
        changed
    }

    /// Sets an active study's durable stroke width and line colors. Returns `None` when the
    /// study is not active, otherwise whether its persisted style changed and the study as it
    /// now stands.
    pub(super) fn set_style(
        &mut self,
        study_id: StudyInstanceId,
        line_width: u32,
        output_colors: &[String],
    ) -> Option<(bool, &WorkspaceChartStudyState)> {
        let active = self
            .active
            .iter_mut()
            .find(|state| state.study_id == study_id)?;
        let changed = active.persisted.line_width != line_width
            || active.persisted.output_colors != output_colors;
        if changed {
            active.persisted.line_width = line_width;
            active.persisted.output_colors = output_colors.to_vec();
        }
        Some((changed, &active.persisted))
    }

    /// Decides what `settings` need. While an earlier reinitialization is in flight the newest
    /// settings wait in `queued_settings` (or replace nothing when they match its target), so
    /// editing keeps at most one request per study in flight.
    pub(super) fn request_settings(
        &mut self,
        study_id: StudyInstanceId,
        settings: Vec<WorkspaceStudySettingState>,
    ) -> StudySettingsRequest {
        let Some(active) = self.active.iter().find(|state| state.study_id == study_id) else {
            return StudySettingsRequest::Inactive;
        };
        if let Some(pending) = self.reinitializing.get(&study_id) {
            let target = pending
                .replacement_settings
                .as_ref()
                .unwrap_or(&active.persisted.settings);
            if *target == settings {
                self.queued_settings.remove(&study_id);
            } else {
                self.queued_settings.insert(study_id, settings);
            }
            return StudySettingsRequest::Waiting;
        }
        if active.persisted.settings == settings {
            return StudySettingsRequest::Unchanged;
        }
        let mut replacement = active.persisted.clone();
        replacement.settings = settings;
        StudySettingsRequest::Reinitialize(replacement)
    }

    /// The settings that waited for the study's previous reinitialization to finish.
    pub(super) fn take_queued_settings(
        &mut self,
        study_id: StudyInstanceId,
    ) -> Option<Vec<WorkspaceStudySettingState>> {
        self.queued_settings.remove(&study_id)
    }

    #[cfg(test)]
    pub(super) fn push_deferred(&mut self, state: PendingRuntimeStudyState) {
        self.deferred.push(state);
    }

    #[cfg(test)]
    pub(super) const fn pending(&self) -> &HashMap<u64, PendingRuntimeStudyState> {
        &self.pending
    }

    #[cfg(test)]
    pub(super) fn deferred(&self) -> &[PendingRuntimeStudyState] {
        &self.deferred
    }

    #[cfg(test)]
    pub(super) fn is_reinitializing(&self, study_id: StudyInstanceId) -> bool {
        self.reinitializing.contains_key(&study_id)
    }

    #[cfg(test)]
    pub(super) fn has_reinitializations(&self) -> bool {
        !self.reinitializing.is_empty()
    }

    #[cfg(test)]
    pub(super) fn is_automatic_removal(&self, study_id: StudyInstanceId) -> bool {
        self.automatic_removals.contains(&study_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn study(local_id: u64) -> WorkspaceChartStudyState {
        WorkspaceChartStudyState {
            local_id,
            ..WorkspaceChartStudyState::default()
        }
    }

    fn pending(local_id: u64, remove_on_registration: bool) -> PendingRuntimeStudyState {
        PendingRuntimeStudyState {
            persisted: study(local_id),
            resolved_chart_series: None,
            remove_on_registration,
            persist_on_registration: false,
            blocked: false,
        }
    }

    fn runtime(id: u64, local_id: u64) -> RuntimeStudyState {
        RuntimeStudyState {
            study_id: study_id(id),
            persisted: study(local_id),
            resolved_chart_series: None,
        }
    }

    fn study_id(id: u64) -> StudyInstanceId {
        StudyInstanceId::try_from_u64(id).expect("study id")
    }

    fn series(instrument_id: &str) -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "fixture".to_string(),
            instrument_id: instrument_id.to_string(),
            entitlement_id: "fixture".to_string(),
            period: aeris_market_data::BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        }
    }

    #[test]
    fn local_ids_start_above_restored_studies_and_stop_at_exhaustion() {
        let mut fresh = StudiesController::default();
        assert_eq!(fresh.allocate_local_id(), Some(1));
        assert_eq!(fresh.allocate_local_id(), Some(2));

        let mut restored = StudiesController::restore(vec![pending(7, false), pending(3, false)]);
        assert_eq!(restored.allocate_local_id(), Some(8));

        let mut nearly_full = StudiesController::restore(vec![pending(u64::MAX - 1, false)]);
        assert_eq!(nearly_full.allocate_local_id(), Some(u64::MAX));
        assert_eq!(nearly_full.allocate_local_id(), None);
        assert_eq!(nearly_full.allocate_local_id(), None);

        let mut full = StudiesController::restore(vec![pending(u64::MAX, false)]);
        assert_eq!(full.allocate_local_id(), None);
    }

    #[test]
    fn count_excludes_removals_and_cancelled_registrations_but_persistence_keeps_manual_removals() {
        let mut studies = StudiesController::restore(vec![pending(5, false)]);
        studies.activate(runtime(1, 1));
        studies.activate(runtime(2, 2));
        studies.activate_for_removal(runtime(3, 3));
        studies.mark_removing(study_id(2));
        studies.insert_pending(10, pending(4, false));
        studies.insert_pending(11, pending(6, true));

        assert_eq!(studies.count(), 3);
        assert!(studies.has_any());
        assert!(studies.is_busy(study_id(2)));
        assert!(!studies.is_busy(study_id(1)));
        assert!(studies.suppresses_output(study_id(3)));
        assert_eq!(studies.removable_ids(), vec![study_id(1), study_id(3)]);
        assert_eq!(
            studies
                .persisted_states(|_| None)
                .iter()
                .map(|state| state.local_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 4, 5],
            "a manual removal persists until the runtime acknowledges it"
        );
    }

    #[test]
    fn registration_retry_reports_each_series_once_until_a_registration_succeeds() {
        let failed = series("failed");
        let now = Instant::now();
        let mut studies = StudiesController::default();

        assert!(studies.defer_registration(Some(failed.clone()), now));
        assert!(!studies.defer_registration(Some(failed.clone()), now));
        assert!(!studies.registration_due(Some(&failed), now));

        studies.registration_succeeded();
        assert!(studies.registration_due(Some(&failed), now));
        assert!(
            studies.defer_registration(Some(failed), now),
            "a success forgets the earlier failure"
        );
    }

    #[test]
    fn the_next_retry_is_the_earliest_deadline_still_holding_work_back() {
        let now = Instant::now();
        let later = now + STUDY_RETRY_DELAY;
        let mut studies = StudiesController::default();
        assert_eq!(studies.next_retry_at(now), None);

        studies.defer_registration(Some(series("failed")), now);
        assert_eq!(
            studies.next_retry_at(now),
            None,
            "a registration retry with nothing deferred holds nothing back"
        );

        studies.push_deferred(pending(1, false));
        assert_eq!(studies.next_retry_at(now), Some(later));
        let earlier = now
            .checked_sub(std::time::Duration::from_secs(1))
            .expect("an instant before now");
        studies.defer_reinitialization(study_id(2), series("current"), earlier);
        let reinitialization_at = earlier + STUDY_RETRY_DELAY;
        assert_eq!(studies.next_retry_at(now), Some(reinitialization_at));
        assert_eq!(
            studies.next_retry_at(later),
            None,
            "deadlines already reached need no timer"
        );
        studies.registration_succeeded();
        assert_eq!(studies.next_retry_at(now), Some(reinitialization_at));
    }

    #[test]
    fn discarding_unregistered_studies_reports_only_visible_work() {
        let mut studies = StudiesController::default();
        studies.insert_pending(1, pending(1, true));
        assert!(
            !studies.discard_unregistered(),
            "an already cancelled registration is not a change"
        );

        studies.insert_pending(2, pending(2, false));
        studies.push_deferred(pending(3, false));
        assert!(studies.discard_unregistered());
        assert_eq!(studies.deferred(), []);
        assert!(
            studies
                .pending()
                .values()
                .all(|state| state.remove_on_registration)
        );
        assert_eq!(studies.count(), 0);
    }

    #[test]
    fn deferred_studies_return_to_their_place_and_blocked_ones_are_skipped() {
        let mut studies = StudiesController::restore(vec![pending(1, false), pending(2, false)]);
        assert_eq!(studies.next_deferred_index(), Some(0));
        let first = studies.take_deferred(0).expect("first deferred study");
        studies.return_deferred(0, first);
        assert_eq!(studies.deferred()[0].persisted.local_id, 1);
        assert!(studies.take_deferred(9).is_none());

        let blocked = studies.take_deferred(0).expect("first deferred study");
        studies.push_front_blocked(blocked);
        assert!(studies.deferred()[0].blocked);
        assert_eq!(studies.next_deferred_index(), Some(1));
    }

    #[test]
    fn settings_requests_wait_behind_an_inflight_reinitialization() {
        let id = study_id(1);
        let mut studies = StudiesController::default();
        assert_eq!(
            studies.request_settings(id, Vec::new()),
            StudySettingsRequest::Inactive
        );
        studies.activate(runtime(1, 1));
        assert_eq!(
            studies.request_settings(id, Vec::new()),
            StudySettingsRequest::Unchanged
        );

        let edited = vec![WorkspaceStudySettingState {
            identifier: "length".to_string(),
            value: None,
        }];
        let StudySettingsRequest::Reinitialize(replacement) =
            studies.request_settings(id, edited.clone())
        else {
            panic!("changed settings reinitialize the study");
        };
        assert_eq!(replacement.settings, edited);

        studies.begin_reinitialization(
            id,
            PendingStudyReinitialization {
                series: series("current"),
                replacement_settings: Some(edited.clone()),
            },
        );
        assert_eq!(
            studies.request_settings(id, Vec::new()),
            StudySettingsRequest::Waiting
        );
        assert_eq!(studies.take_queued_settings(id), Some(Vec::new()));
        assert_eq!(
            studies.request_settings(id, edited),
            StudySettingsRequest::Waiting
        );
        assert_eq!(
            studies.take_queued_settings(id),
            None,
            "settings matching the in-flight target replace nothing"
        );
    }

    #[test]
    fn style_changes_report_only_real_changes() {
        let id = study_id(1);
        let mut studies = StudiesController::default();
        assert!(studies.set_style(id, 2, &[]).is_none());
        studies.activate(runtime(1, 1));
        let colors = vec!["--chart-blue".to_string()];
        assert_eq!(
            studies
                .set_style(id, 3, &colors)
                .map(|(changed, state)| (changed, state.line_width)),
            Some((true, 3))
        );
        assert_eq!(
            studies
                .set_style(id, 3, &colors)
                .map(|(changed, _)| changed),
            Some(false)
        );
    }

    #[test]
    fn stale_studies_skip_busy_current_and_independent_studies() {
        let current = series("current");
        let now = Instant::now();
        let mut studies = StudiesController::default();
        studies.activate(runtime(1, 1));
        studies.activate(RuntimeStudyState {
            resolved_chart_series: Some(current.clone()),
            ..runtime(2, 2)
        });
        studies.activate(runtime(3, 3));
        studies.mark_removing(study_id(3));
        studies.activate(runtime(4, 4));

        let uses_chart = |state: &WorkspaceChartStudyState| state.local_id != 4;
        assert_eq!(
            studies.stale_studies(&current, now, uses_chart),
            vec![study_id(1)]
        );
        assert!(studies.defer_reinitialization(study_id(1), current.clone(), now));
        assert_eq!(
            studies.stale_studies(&current, now, uses_chart),
            [] as [StudyInstanceId; 0]
        );
        assert_eq!(
            studies.stale_studies(&current, now + STUDY_RETRY_DELAY, uses_chart),
            vec![study_id(1)]
        );
    }
}
