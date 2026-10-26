//! Selection outcome fence for one chart pane.

use super::{ChartInterval, InstallProviderInstrument};

/// The pane's presentation state, projected only after its selection fence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChartState {
    Loading,
    Ready,
    Stale,
    Recovering,
    /// The provider answered the demand with no market history; the live
    /// subscription stays up and nothing is wrong or retrying.
    AwaitingData,
    /// Covering history exhausted its retry budget; the chart shows bars
    /// built from the live trade stream while one slow history probe stays
    /// armed in the runtime.
    HistoryUnavailable,
    Error,
}

impl ChartState {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Loading => "Loading chart",
            Self::Ready => "Chart ready",
            Self::Stale => "Chart stale",
            Self::Recovering => "Reconnecting chart",
            Self::AwaitingData => "Waiting for market data",
            Self::HistoryUnavailable => "History unavailable",
            Self::Error => "Chart unavailable",
        }
    }

    /// States that only an engine readiness report may leave; a bar
    /// publication alone does not make the chart current.
    pub(super) const fn awaits_engine_readiness(self) -> bool {
        matches!(
            self,
            Self::Loading | Self::AwaitingData | Self::HistoryUnavailable
        )
    }
}

/// The requested target belongs to the same exact selection as its outcome.
pub(super) struct PendingChartTarget {
    pub(super) interval: ChartInterval,
    pub(super) product: Option<InstallProviderInstrument>,
}

pub(super) struct ChartPaneController {
    pending_selection: Option<u64>,
    pending_target: Option<PendingChartTarget>,
    restore_target: Option<PendingChartTarget>,
    latest_selection: u64,
    current_generation: u64,
    displayed_generation: Option<u64>,
    initial_instrument_applied: bool,
}

impl Default for ChartPaneController {
    fn default() -> Self {
        Self {
            pending_selection: None,
            pending_target: None,
            restore_target: None,
            latest_selection: 0,
            current_generation: 1,
            displayed_generation: None,
            initial_instrument_applied: false,
        }
    }
}

impl ChartPaneController {
    pub(super) fn request(&mut self, generation: u64) {
        self.latest_selection = self.latest_selection.max(generation);
        self.pending_selection = Some(generation);
        self.pending_target = None;
    }

    pub(super) fn set_pending_target(
        &mut self,
        generation: u64,
        interval: ChartInterval,
        product: Option<InstallProviderInstrument>,
    ) {
        if self.is_pending(generation) {
            self.pending_target = Some(PendingChartTarget { interval, product });
        }
    }

    pub(super) fn pending_interval(&self) -> Option<ChartInterval> {
        self.pending_target.as_ref().map(|target| target.interval)
    }

    pub(super) fn pending_product(&self) -> Option<&InstallProviderInstrument> {
        self.pending_target.as_ref()?.product.as_ref()
    }

    pub(super) fn take_pending_target(&mut self) -> Option<PendingChartTarget> {
        self.pending_target.take()
    }

    pub(super) fn clear(&mut self) {
        self.pending_selection = None;
        self.pending_target = None;
    }

    /// The displayed binding is retained while the committed replacement loads.
    pub(super) fn remember_previous_selection(
        &mut self,
        product: Option<InstallProviderInstrument>,
        interval: ChartInterval,
    ) {
        self.restore_target = Some(PendingChartTarget { interval, product });
    }

    pub(super) fn restore_product(&self) -> Option<&InstallProviderInstrument> {
        self.restore_target.as_ref()?.product.as_ref()
    }

    pub(super) fn has_restore_target(&self) -> bool {
        self.restore_target.is_some()
    }

    pub(super) fn take_restore_target(&mut self) -> Option<PendingChartTarget> {
        let target = self.restore_target.take()?;
        if let Some(displayed) = self.displayed_generation {
            // The retained chart is the authoritative binding again. Its
            // publications may continue while the restored demand is sent.
            self.current_generation = displayed;
        }
        Some(target)
    }

    pub(super) fn finish_replacement(&mut self) {
        self.restore_target = None;
    }

    #[cfg(test)]
    pub(super) fn pending_selection(&self) -> Option<u64> {
        self.pending_selection
    }

    pub(super) fn is_pending(&self, generation: u64) -> bool {
        self.pending_selection == Some(generation)
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
        self.pending_target = None;
        true
    }

    /// Accept pane-scoped data only for the committed selection. While a
    /// request is pending, the previous chart remains the displayed binding.
    pub(super) fn accept_publication(&self, generation: u64) -> bool {
        generation == self.current_generation
    }

    /// The installed chart is a projection of a specific selection, not necessarily
    /// the most recently committed one. Keep it intact until a covering snapshot
    /// for the committed selection replaces it.
    pub(super) fn chart_installed(&mut self) {
        self.displayed_generation = Some(self.current_generation);
    }

    pub(super) fn chart_needs_replacement(&self) -> bool {
        self.displayed_generation != Some(self.current_generation)
    }

    pub(super) fn selection_in_progress(&self) -> bool {
        self.pending_selection.is_some() || self.restore_target.is_some()
    }

    pub(super) fn has_pending_selection(&self) -> bool {
        self.pending_selection.is_some()
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

    /// The initial runtime descriptor is a one-shot reply, not a continuing
    /// source of terms. A repeated reply cannot replace the accepted terms.
    pub(super) fn accept_initial_instrument(&self, generation: u64) -> bool {
        !self.initial_instrument_applied
            && self.pending_selection.is_none()
            && generation >= self.latest_selection
    }

    pub(super) fn adopt_initial_instrument(&mut self, generation: u64) -> bool {
        if !self.accept_initial_instrument(generation) {
            return false;
        }
        self.current_generation = generation;
        self.initial_instrument_applied = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_states_have_explicit_user_facing_labels_and_readiness_gate() {
        assert_eq!(ChartState::Loading.label(), "Loading chart");
        assert_eq!(ChartState::Ready.label(), "Chart ready");
        assert_eq!(ChartState::Stale.label(), "Chart stale");
        assert_eq!(ChartState::Recovering.label(), "Reconnecting chart");
        assert_eq!(ChartState::AwaitingData.label(), "Waiting for market data");
        assert_eq!(
            ChartState::HistoryUnavailable.label(),
            "History unavailable"
        );
        assert_eq!(ChartState::Error.label(), "Chart unavailable");
        assert!(ChartState::Loading.awaits_engine_readiness());
        assert!(ChartState::AwaitingData.awaits_engine_readiness());
        assert!(ChartState::HistoryUnavailable.awaits_engine_readiness());
        for state in [
            ChartState::Ready,
            ChartState::Stale,
            ChartState::Recovering,
            ChartState::Error,
        ] {
            assert!(!state.awaits_engine_readiness());
        }
    }

    #[test]
    fn initial_descriptor_is_consumed_once_even_for_the_same_generation() {
        let mut pane = ChartPaneController::default();
        assert!(pane.adopt_initial_instrument(7));
        assert!(pane.accept_publication(7));
        assert!(!pane.adopt_initial_instrument(7));
        assert!(!pane.adopt_initial_instrument(8));
    }

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
    fn pending_target_follows_only_the_current_request_and_is_consumed() {
        let mut pane = ChartPaneController::default();
        pane.request(7);
        pane.set_pending_target(7, ChartInterval::Minute5, None);
        pane.request(8);
        pane.set_pending_target(7, ChartInterval::Day1, None);
        assert_eq!(pane.pending_interval(), None);
        pane.set_pending_target(8, ChartInterval::Minute15, None);
        assert_eq!(pane.pending_interval(), Some(ChartInterval::Minute15));
        assert!(!pane.reject(7));
        assert_eq!(pane.pending_interval(), Some(ChartInterval::Minute15));
        assert!(pane.accept(8));
        assert_eq!(
            pane.take_pending_target().map(|target| target.interval),
            Some(ChartInterval::Minute15)
        );
        assert_eq!(pane.pending_interval(), None);
        pane.request(9);
        pane.set_pending_target(9, ChartInterval::Hour1, None);
        assert!(pane.reject(9));
        assert_eq!(pane.pending_interval(), None);
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
    fn failed_replacement_re_adopts_previous_instrument_and_interval_once() {
        let mut pane = ChartPaneController::default();
        pane.chart_installed();
        pane.request(2);
        assert!(pane.accept(2));
        pane.remember_previous_selection(None, ChartInterval::Hour1);
        assert!(pane.selection_in_progress());
        assert_eq!(pane.restore_product(), None);
        assert_eq!(
            pane.take_restore_target().map(|target| target.interval),
            Some(ChartInterval::Hour1)
        );
        assert!(
            pane.accept_publication(1),
            "the displayed binding is re-adopted"
        );
        assert!(!pane.accept_publication(2), "failed replacement is retired");
        assert!(!pane.chart_needs_replacement());
        assert!(!pane.selection_in_progress());
        assert!(pane.take_restore_target().is_none(), "one failure notice");
        pane.request(3);
        assert!(pane.accept(3));
        pane.remember_previous_selection(None, ChartInterval::Day1);
        pane.finish_replacement();
        assert!(pane.take_restore_target().is_none());
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

    #[test]
    fn displayed_chart_generation_changes_only_on_covering_install() {
        let mut pane = ChartPaneController::default();
        assert!(pane.chart_needs_replacement());
        pane.chart_installed();
        assert!(!pane.chart_needs_replacement());
        pane.request(2);
        assert!(!pane.chart_needs_replacement());
        assert!(pane.accept(2));
        assert!(pane.chart_needs_replacement());
        pane.request(3);
        assert!(pane.accept(3));
        assert!(pane.chart_needs_replacement());
        pane.chart_installed();
        assert!(!pane.chart_needs_replacement());
        pane.request(4);
        assert!(pane.reject(4));
        assert!(!pane.chart_needs_replacement());
    }

    #[test]
    fn fixed_switch_publication_permutations_obey_the_selection_fence() {
        // A replacement commit, old/new data and old/new lifecycle can arrive
        // in any order after dispatch. Only committed data can install a chart.
        for commit_at in 0..5 {
            for old_data_at in 0..5 {
                for new_data_at in 0..5 {
                    if commit_at == old_data_at
                        || commit_at == new_data_at
                        || old_data_at == new_data_at
                    {
                        continue;
                    }
                    for old_state_at in 0..5 {
                        if [commit_at, old_data_at, new_data_at].contains(&old_state_at) {
                            continue;
                        }
                        let new_state_at = (0..5)
                            .find(|at| {
                                ![commit_at, old_data_at, new_data_at, old_state_at].contains(at)
                            })
                            .expect("fifth event");
                        let order = [
                            commit_at,
                            old_data_at,
                            new_data_at,
                            old_state_at,
                            new_state_at,
                        ];
                        let mut pane = ChartPaneController::default();
                        pane.chart_installed();
                        pane.request(2);
                        let mut replacement_installed = false;
                        for step in 0..5 {
                            if step == commit_at {
                                assert!(pane.accept(2), "order {order:?}");
                            } else if step == old_data_at {
                                assert_eq!(
                                    pane.accept_publication(1),
                                    step < commit_at,
                                    "order {order:?}"
                                );
                            } else if step == new_data_at {
                                if pane.accept_publication(2) {
                                    pane.chart_installed();
                                    replacement_installed = true;
                                }
                            } else if step == old_state_at {
                                assert!(!pane.accept_series_state(1), "order {order:?}");
                            } else {
                                assert_eq!(
                                    pane.accept_series_state(2),
                                    step > commit_at,
                                    "order {order:?}"
                                );
                            }
                            assert_eq!(
                                pane.chart_needs_replacement(),
                                step >= commit_at && !replacement_installed,
                                "order {order:?}, step {step}"
                            );
                        }
                        assert!(!pane.accept_publication(1), "order {order:?}");
                        assert!(pane.accept_publication(2), "order {order:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn seeded_switch_and_publication_interleavings_keep_exact_binding() {
        for seed in 0..128_u64 {
            let mut random = seed.wrapping_add(1);
            let mut pane = ChartPaneController::default();
            pane.chart_installed();
            let (mut committed, mut displayed, mut pending, mut next) = (1, 1, None, 2);
            for step in 0..256 {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                match random % 8 {
                    0 => {
                        pane.request(next);
                        pending = Some(next);
                        next += 1;
                    }
                    1 => {
                        let generation = if random & 8 == 0 {
                            pending.unwrap_or(next)
                        } else {
                            next
                        };
                        assert_eq!(
                            pane.accept(generation),
                            pending == Some(generation),
                            "seed {seed}, step {step}"
                        );
                        if pending == Some(generation) {
                            committed = generation;
                            pending = None;
                        }
                    }
                    2 => {
                        let generation = if random & 8 == 0 {
                            pending.unwrap_or(next)
                        } else {
                            next
                        };
                        assert_eq!(
                            pane.reject(generation),
                            pending == Some(generation),
                            "seed {seed}, step {step}"
                        );
                        if pending == Some(generation) {
                            pending = None;
                        }
                    }
                    3..=5 => {
                        let generation = match random % 3 {
                            0 => committed,
                            1 => next,
                            _ => committed.saturating_sub(1),
                        };
                        let accepted = pane.accept_publication(generation);
                        assert_eq!(
                            accepted,
                            generation == committed,
                            "seed {seed}, step {step}"
                        );
                        if accepted && random & 16 != 0 {
                            pane.chart_installed();
                            displayed = committed;
                        }
                    }
                    6 => {
                        let generation = if random & 8 == 0 { committed } else { next };
                        assert_eq!(
                            pane.accept_series_state(generation),
                            pending.is_none() && generation == committed,
                            "seed {seed}, step {step}"
                        );
                    }
                    _ => {
                        let generation = if random & 8 == 0 { committed } else { next };
                        assert_eq!(
                            pane.accept_study_publication(Some(generation)),
                            generation == committed,
                            "seed {seed}, step {step}"
                        );
                    }
                }
                assert_eq!(
                    pane.pending_selection(),
                    pending,
                    "seed {seed}, step {step}"
                );
                assert_eq!(
                    pane.chart_needs_replacement(),
                    displayed != committed,
                    "seed {seed}, step {step}"
                );
            }
        }
    }
}
