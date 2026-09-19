//! Bounded replay-to-Nucleus data bridge with correlated recovery commands.

use std::{num::NonZeroUsize, time::Instant};
use tradingplot_application::{
    ProvenancedMarketBar, ReplayRecoveryCommand, ReplaySession, ReplaySnapshot, ReplayStreamUpdate,
    ReplayValidationError, ResnapshotReason, SequenceDecision,
};
use tradingplot_terminal_ui::BoundedUiQueue;

pub(crate) const MAX_RECOVERY_DISPATCH_ATTEMPTS: usize = 3;

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChartSeriesIdentity {
    instrument_id: String,
    instrument_revision: u64,
    bar_definition_id: String,
    bar_definition_version: u32,
    bar_interval_seconds: u32,
    bar_trades_per_bar: Option<u32>,
    bar_calendar_months: Option<u32>,
    schema_version: u32,
}

impl ChartSeriesIdentity {
    fn from_snapshot(snapshot: &ReplaySnapshot) -> Self {
        Self {
            instrument_id: snapshot.instrument().instrument_id.as_str().to_string(),
            instrument_revision: snapshot.instrument().revision,
            bar_definition_id: snapshot.bar_definition().definition_id.clone(),
            bar_definition_version: snapshot.bar_definition().version,
            bar_interval_seconds: snapshot.bar_definition().interval_seconds,
            bar_trades_per_bar: snapshot.bar_definition().trades_per_bar,
            bar_calendar_months: snapshot.bar_definition().calendar_months,
            schema_version: snapshot.evidence().schema_version,
        }
    }

    fn matches(&self, snapshot: &ReplaySnapshot) -> bool {
        self.instrument_id == snapshot.instrument().instrument_id.as_str()
            && self.instrument_revision == snapshot.instrument().revision
            && self.bar_definition_id == snapshot.bar_definition().definition_id
            && self.bar_definition_version == snapshot.bar_definition().version
            && self.bar_interval_seconds == snapshot.bar_definition().interval_seconds
            && self.bar_trades_per_bar == snapshot.bar_definition().trades_per_bar
            && self.bar_calendar_months == snapshot.bar_definition().calendar_months
            && self.schema_version == snapshot.evidence().schema_version
    }
}

/// One queue drain collapsed into at most one authoritative Nucleus mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MergedChartData {
    snapshot: Option<ReplaySnapshot>,
    accepted_deltas: Vec<ProvenancedMarketBar>,
}

impl MergedChartData {
    /// Returns the last snapshot in this drain, if one replaced prior state.
    #[must_use]
    pub(crate) const fn snapshot(&self) -> Option<&ReplaySnapshot> {
        self.snapshot.as_ref()
    }

    /// Returns contiguous deltas accepted after the retained snapshot or baseline.
    #[must_use]
    pub(crate) fn accepted_deltas(&self) -> &[ProvenancedMarketBar] {
        &self.accepted_deltas
    }

    #[must_use]
    pub(crate) const fn mutates_series(&self) -> bool {
        self.snapshot.is_some() || !self.accepted_deltas.is_empty()
    }
}

/// Observable bounded-bridge state used for resnapshot and overload telemetry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ChartBridgeMetrics {
    pub queued_updates: usize,
    pub queue_overflows: u64,
    pub resnapshot_requests: u64,
    pub snapshot_required: bool,
    pub recovery_pending: bool,
    pub recovery_dispatched: bool,
    pub recovery_request_id: Option<u64>,
    pub completed_recoveries: u64,
    pub failed_recoveries: u64,
    pub canceled_recoveries: u64,
    pub recovery_dispatch_attempts: usize,
    pub rejected_uncorrelated_snapshots: u64,
    pub rejected_stale_snapshots: u64,
    pub last_recovery_duration_nanos: Option<u64>,
}

/// Bounded consumer bridge between replay producers and one chart frame.
#[derive(Debug)]
pub(crate) struct ChartDataBridge {
    queue: BoundedUiQueue<ReplayStreamUpdate>,
    session: Option<ReplaySession>,
    accepted_series: ChartSeriesIdentity,
    accepted_session_generation: u64,
    accepted_publication_generation: u64,
    accepted_last_sequence: u64,
    queue_overflows: u64,
    resnapshot_requests: u64,
    pending_resnapshot: Option<ReplayRecoveryCommand>,
    recovery_dispatched: bool,
    recovery_dispatch_attempts: usize,
    next_recovery_request_id: u64,
    recovery_started: Option<Instant>,
    completed_recoveries: u64,
    failed_recoveries: u64,
    canceled_recoveries: u64,
    rejected_uncorrelated_snapshots: u64,
    rejected_stale_snapshots: u64,
    last_recovery_duration_nanos: Option<u64>,
}

impl ChartDataBridge {
    /// Creates a bounded bridge with an installed replay baseline.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot establish resumable session state.
    pub(crate) fn try_new(
        capacity: NonZeroUsize,
        snapshot: &ReplaySnapshot,
    ) -> Result<Self, ReplayValidationError> {
        Ok(Self {
            queue: BoundedUiQueue::new(capacity),
            session: Some(ReplaySession::try_new(snapshot)?),
            accepted_series: ChartSeriesIdentity::from_snapshot(snapshot),
            accepted_session_generation: snapshot.evidence().session_generation,
            accepted_publication_generation: snapshot.evidence().publication_generation,
            accepted_last_sequence: snapshot.evidence().last_sequence,
            queue_overflows: 0,
            resnapshot_requests: 0,
            pending_resnapshot: None,
            recovery_dispatched: false,
            recovery_dispatch_attempts: 0,
            next_recovery_request_id: 1,
            recovery_started: None,
            completed_recoveries: 0,
            failed_recoveries: 0,
            canceled_recoveries: 0,
            rejected_uncorrelated_snapshots: 0,
            rejected_stale_snapshots: 0,
            last_recovery_duration_nanos: None,
        })
    }

    /// Enqueues one update without allowing producer work to grow unbounded.
    ///
    /// # Errors
    ///
    /// Returns the unchanged update when the bridge is at capacity.
    pub(crate) fn try_push(
        &mut self,
        update: ReplayStreamUpdate,
    ) -> Result<(), Box<ReplayStreamUpdate>> {
        match self.queue.try_push(update) {
            Ok(()) => Ok(()),
            Err(rejected) => {
                self.queue_overflows = self.queue_overflows.saturating_add(1);
                self.queue.drain().for_each(drop);
                self.session = None;
                self.request_resnapshot(ResnapshotReason::QueueOverflow);
                Err(Box::new(rejected))
            }
        }
    }

    /// Replaces session state immediately and discards obsolete queued updates.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot establish resumable session state.
    pub(crate) fn install_snapshot(
        &mut self,
        snapshot: &ReplaySnapshot,
    ) -> Result<(), ReplayValidationError> {
        if let Some(command) = self.pending_resnapshot {
            self.rejected_uncorrelated_snapshots =
                self.rejected_uncorrelated_snapshots.saturating_add(1);
            return Err(ReplayValidationError::UncorrelatedRecoverySnapshot {
                request_id: command.request_id,
            });
        }
        self.ensure_snapshot_advances(snapshot)?;
        self.install_snapshot_state(snapshot)?;
        Ok(())
    }

    /// Drains all currently queued commands into one ordered chart update.
    ///
    /// A later snapshot supersedes all earlier updates in the same drain.
    /// Duplicates are ignored, while a gap blocks subsequent deltas until a
    /// fresh snapshot is observed.
    ///
    /// # Errors
    ///
    /// Returns an error when an otherwise contiguous delta violates replay
    /// market-data or timestamp invariants.
    pub(crate) fn drain_merged(
        &mut self,
    ) -> Result<Option<MergedChartData>, ReplayValidationError> {
        let updates: Vec<_> = self.queue.drain().collect();
        if updates.is_empty() {
            return Ok(None);
        }
        let mut candidate_session = self.session;
        let mut candidate_series = self.accepted_series.clone();
        let mut candidate_session_generation = self.accepted_session_generation;
        let mut candidate_publication_generation = self.accepted_publication_generation;
        let mut candidate_last_sequence = self.accepted_last_sequence;
        let mut merged = MergedChartData {
            snapshot: None,
            accepted_deltas: Vec::with_capacity(updates.len()),
        };
        for update in updates {
            match update {
                ReplayStreamUpdate::Snapshot(snapshot) => {
                    if self.pending_resnapshot.is_some() {
                        self.rejected_uncorrelated_snapshots =
                            self.rejected_uncorrelated_snapshots.saturating_add(1);
                        continue;
                    }
                    if !snapshot_may_replace(
                        &snapshot,
                        candidate_session_generation,
                        candidate_publication_generation,
                        candidate_last_sequence,
                        candidate_series.matches(&snapshot),
                        None,
                    ) {
                        self.rejected_stale_snapshots =
                            self.rejected_stale_snapshots.saturating_add(1);
                        continue;
                    }
                    candidate_session = Some(ReplaySession::try_new(&snapshot)?);
                    candidate_series = ChartSeriesIdentity::from_snapshot(&snapshot);
                    candidate_session_generation = snapshot.evidence().session_generation;
                    candidate_publication_generation = snapshot.evidence().publication_generation;
                    candidate_last_sequence = snapshot.evidence().last_sequence;
                    merged.snapshot = Some(snapshot);
                    merged.accepted_deltas.clear();
                }
                ReplayStreamUpdate::Delta(delta) => {
                    if merged
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| !delta_matches_snapshot(&delta, snapshot))
                    {
                        continue;
                    }
                    let Some(session) = candidate_session.as_mut() else {
                        continue;
                    };
                    let decision = session.accept_delta(&delta)?;
                    if matches!(decision, SequenceDecision::Gap { .. }) {
                        self.request_resnapshot(ResnapshotReason::SequenceGap);
                    }
                    if decision == SequenceDecision::Accepted {
                        candidate_last_sequence = delta.sequence();
                        merged.accepted_deltas.push(delta.item().clone());
                    }
                }
                ReplayStreamUpdate::Tail(update) => {
                    if update.item().provenance().session_generation != candidate_session_generation
                        || update.item().provenance().schema_version
                            != candidate_series.schema_version
                        || update.publication_generation() <= candidate_publication_generation
                    {
                        continue;
                    }
                    let Some(session) = candidate_session.as_mut() else {
                        continue;
                    };
                    let decision = session.accept_tail(&update)?;
                    if matches!(decision, SequenceDecision::Gap { .. }) {
                        self.request_resnapshot(ResnapshotReason::SequenceGap);
                    }
                    if decision == SequenceDecision::Accepted {
                        candidate_publication_generation = update.publication_generation();
                        candidate_last_sequence =
                            candidate_last_sequence.max(update.item().value().source_sequence);
                        merged.accepted_deltas.push(update.item().clone());
                    }
                }
            }
        }
        self.session = candidate_session;
        self.accepted_series = candidate_series;
        self.accepted_session_generation = candidate_session_generation;
        self.accepted_publication_generation = candidate_publication_generation;
        self.accepted_last_sequence = candidate_last_sequence;
        Ok(Some(merged))
    }

    /// Returns the number of producer updates waiting for the next drain.
    #[must_use]
    pub(crate) fn queued_update_count(&self) -> usize {
        self.queue.len()
    }

    /// Returns whether sequence state requires a replacement snapshot.
    #[must_use]
    pub(crate) fn requires_snapshot(&self) -> bool {
        self.session.is_none_or(ReplaySession::requires_snapshot)
    }

    /// Returns the next expected sequence when a snapshot is installed.
    #[must_use]
    pub(crate) fn expected_sequence(&self) -> Option<u64> {
        self.session.and_then(ReplaySession::expected_sequence)
    }

    /// Peeks the retryable command without claiming that a worker queue accepted it.
    #[must_use]
    pub(crate) fn pending_resnapshot_request(&self) -> Option<ReplayRecoveryCommand> {
        if self.recovery_dispatched {
            None
        } else {
            self.pending_resnapshot
        }
    }

    /// Offers the pending command to a bounded worker queue and marks it dispatched only on acceptance.
    ///
    /// # Errors
    ///
    /// Returns the dispatcher's rejection without changing command state.
    pub(crate) fn try_dispatch_recovery<DispatchError>(
        &mut self,
        dispatch: impl FnOnce(ReplayRecoveryCommand) -> Result<(), DispatchError>,
    ) -> Result<bool, DispatchError> {
        let Some(command) = self.pending_resnapshot_request() else {
            return Ok(false);
        };
        dispatch(command)?;
        self.recovery_dispatched = true;
        self.recovery_dispatch_attempts = self.recovery_dispatch_attempts.saturating_add(1);
        Ok(true)
    }

    /// Installs only a response correlated to the currently dispatched request.
    ///
    /// Returns `Ok(false)` for a stale or unsolicited response without mutating replay state.
    ///
    /// # Errors
    ///
    /// Returns an error if a correlated snapshot cannot establish resumable session state.
    pub(crate) fn install_recovery_snapshot(
        &mut self,
        request_id: u64,
        snapshot: &ReplaySnapshot,
    ) -> Result<bool, ReplayValidationError> {
        if !self.recovery_dispatched
            || self.pending_resnapshot.map(|command| command.request_id) != Some(request_id)
        {
            self.rejected_uncorrelated_snapshots =
                self.rejected_uncorrelated_snapshots.saturating_add(1);
            return Ok(false);
        }
        if !self.snapshot_may_replace_accepted(
            snapshot,
            self.pending_resnapshot.map(|command| command.reason),
        ) {
            self.rejected_stale_snapshots = self.rejected_stale_snapshots.saturating_add(1);
            // The worker has consumed this response. Re-arm the bounded
            // recovery instead of waiting forever for a second response to it.
            self.mark_recovery_failed(request_id);
            return Ok(false);
        }
        self.install_snapshot_state(snapshot)?;
        self.complete_recovery();
        Ok(true)
    }

    /// Latches recovery after a decoder/model invariant failure.
    pub(crate) fn mark_stream_invalid(&mut self) {
        self.mark_stream_invalid_for(ResnapshotReason::SequenceGap);
    }

    /// Latches recovery with the transport-neutral reason reported by a runtime port.
    pub(crate) fn mark_stream_invalid_for(&mut self, reason: ResnapshotReason) {
        self.queue.drain().for_each(drop);
        self.session = None;
        if self.pending_resnapshot.map(|command| command.reason) != Some(reason) {
            self.pending_resnapshot = None;
            self.recovery_dispatched = false;
            self.recovery_dispatch_attempts = 0;
            self.recovery_started = None;
        }
        self.request_resnapshot(reason);
    }

    /// Records a failed correlated background recovery and makes the command retryable
    /// only while its bounded dispatch budget remains.
    pub(crate) fn mark_recovery_failed(&mut self, request_id: u64) -> bool {
        if self.pending_resnapshot.map(|command| command.request_id) != Some(request_id) {
            return false;
        }
        self.failed_recoveries = self.failed_recoveries.saturating_add(1);
        if self.recovery_dispatch_attempts >= MAX_RECOVERY_DISPATCH_ATTEMPTS {
            self.cancel_recovery_state();
        } else {
            self.recovery_dispatched = false;
        }
        true
    }

    fn request_resnapshot(&mut self, reason: ResnapshotReason) {
        if self.pending_resnapshot.is_none() {
            let request_id = self.next_recovery_request_id;
            self.next_recovery_request_id = request_id.checked_add(1).unwrap_or(1);
            self.pending_resnapshot = Some(ReplayRecoveryCommand { request_id, reason });
            self.recovery_dispatch_attempts = 0;
            self.recovery_started = Some(Instant::now());
            self.resnapshot_requests = self.resnapshot_requests.saturating_add(1);
        }
    }

    fn install_snapshot_state(
        &mut self,
        snapshot: &ReplaySnapshot,
    ) -> Result<(), ReplayValidationError> {
        let session = ReplaySession::try_new(snapshot)?;
        self.queue.drain().for_each(drop);
        self.session = Some(session);
        self.accepted_series = ChartSeriesIdentity::from_snapshot(snapshot);
        self.accepted_session_generation = snapshot.evidence().session_generation;
        self.accepted_publication_generation = snapshot.evidence().publication_generation;
        self.accepted_last_sequence = snapshot.evidence().last_sequence;
        Ok(())
    }

    fn ensure_snapshot_advances(
        &mut self,
        snapshot: &ReplaySnapshot,
    ) -> Result<(), ReplayValidationError> {
        if self.snapshot_may_replace_accepted(snapshot, None) {
            return Ok(());
        }
        self.rejected_stale_snapshots = self.rejected_stale_snapshots.saturating_add(1);
        Err(ReplayValidationError::StaleSnapshot {
            current_generation: self.accepted_publication_generation,
            current_last_sequence: self.accepted_last_sequence,
            actual_generation: snapshot.evidence().publication_generation,
            actual_last_sequence: snapshot.evidence().last_sequence,
        })
    }

    fn cancel_recovery_state(&mut self) {
        self.pending_resnapshot = None;
        self.recovery_dispatched = false;
        self.recovery_dispatch_attempts = 0;
        self.recovery_started = None;
        self.canceled_recoveries = self.canceled_recoveries.saturating_add(1);
    }

    fn snapshot_may_replace_accepted(
        &self,
        snapshot: &ReplaySnapshot,
        transition_reason: Option<ResnapshotReason>,
    ) -> bool {
        snapshot_may_replace(
            snapshot,
            self.accepted_session_generation,
            self.accepted_publication_generation,
            self.accepted_last_sequence,
            self.accepted_series.matches(snapshot),
            transition_reason,
        )
    }

    fn complete_recovery(&mut self) {
        if let Some(started) = self.recovery_started.take() {
            self.last_recovery_duration_nanos =
                Some(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
            self.completed_recoveries = self.completed_recoveries.saturating_add(1);
        }
        self.pending_resnapshot = None;
        self.recovery_dispatched = false;
        self.recovery_dispatch_attempts = 0;
    }

    /// Returns queue depth, overload, and recovery telemetry.
    #[must_use]
    pub(crate) fn metrics(&self) -> ChartBridgeMetrics {
        ChartBridgeMetrics {
            queued_updates: self.queue.len(),
            queue_overflows: self.queue_overflows,
            resnapshot_requests: self.resnapshot_requests,
            snapshot_required: self.requires_snapshot(),
            recovery_pending: self.pending_resnapshot.is_some(),
            recovery_dispatched: self.recovery_dispatched,
            recovery_request_id: self.pending_resnapshot.map(|command| command.request_id),
            completed_recoveries: self.completed_recoveries,
            failed_recoveries: self.failed_recoveries,
            canceled_recoveries: self.canceled_recoveries,
            recovery_dispatch_attempts: self.recovery_dispatch_attempts,
            rejected_uncorrelated_snapshots: self.rejected_uncorrelated_snapshots,
            rejected_stale_snapshots: self.rejected_stale_snapshots,
            last_recovery_duration_nanos: self.last_recovery_duration_nanos,
        }
    }
}

pub(crate) fn snapshot_advances(
    snapshot: &ReplaySnapshot,
    current_session_generation: u64,
    current_publication_generation: u64,
    current_last_sequence: u64,
) -> bool {
    snapshot.evidence().session_generation > current_session_generation
        || (snapshot.evidence().session_generation == current_session_generation
            && snapshot.evidence().publication_generation >= current_publication_generation
            && snapshot.evidence().last_sequence >= current_last_sequence
            && (snapshot.evidence().publication_generation > current_publication_generation
                || snapshot.evidence().last_sequence > current_last_sequence))
}

fn snapshot_may_replace(
    snapshot: &ReplaySnapshot,
    current_session_generation: u64,
    current_publication_generation: u64,
    current_last_sequence: u64,
    same_series: bool,
    transition_reason: Option<ResnapshotReason>,
) -> bool {
    if snapshot.evidence().session_generation < current_session_generation {
        return false;
    }
    if !same_series && transition_reason != Some(ResnapshotReason::SchemaChanged) {
        return false;
    }
    if snapshot.evidence().session_generation > current_session_generation {
        return true;
    }
    if !same_series {
        return true;
    }
    // A correlated authoritative snapshot can repair local queue/model state
    // without waiting for another candle. Neither accepted watermark may rewind.
    if transition_reason.is_some()
        && snapshot.evidence().publication_generation == current_publication_generation
        && snapshot.evidence().last_sequence == current_last_sequence
    {
        return true;
    }
    snapshot_advances(
        snapshot,
        current_session_generation,
        current_publication_generation,
        current_last_sequence,
    )
}

fn delta_matches_snapshot(
    delta: &tradingplot_application::StreamDelta<ProvenancedMarketBar>,
    snapshot: &ReplaySnapshot,
) -> bool {
    let provenance = delta.item().provenance();
    let evidence = snapshot.evidence();
    provenance.session_generation == evidence.session_generation
        && provenance.schema_version == evidence.schema_version
}

#[cfg(test)]
mod tests {
    use super::*;
    use tradingplot_application::{EmbeddedReplaySource, LoadEmbeddedReplay};

    fn snapshot() -> ReplaySnapshot {
        EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
            .expect("replay fixture")
    }

    fn dispatch(bridge: &mut ChartDataBridge) -> u64 {
        let mut id = None;
        assert!(
            bridge
                .try_dispatch_recovery(|command| {
                    id = Some(command.request_id);
                    Ok::<(), ()>(())
                })
                .expect("dispatch")
        );
        id.expect("request")
    }

    #[test]
    fn covering_recovery_does_not_require_another_completed_candle() {
        let baseline = snapshot();
        for reason in [
            ResnapshotReason::QueueOverflow,
            ResnapshotReason::SequenceGap,
            ResnapshotReason::TransportReset,
        ] {
            for publication in [
                baseline.evidence().publication_generation,
                baseline.evidence().publication_generation + 1,
            ] {
                let mut bridge =
                    ChartDataBridge::try_new(NonZeroUsize::MIN, &baseline).expect("bridge");
                bridge.mark_stream_invalid_for(reason);
                let request = dispatch(&mut bridge);
                let covering = baseline
                    .clone()
                    .try_with_publication_generation(publication)
                    .expect("covering snapshot");
                assert!(
                    bridge
                        .install_recovery_snapshot(request, &covering)
                        .expect("recovery")
                );
                assert!(!bridge.requires_snapshot());
                assert!(!bridge.metrics().recovery_pending);
                assert_eq!(bridge.metrics().completed_recoveries, 1);
            }
        }
    }

    #[test]
    fn stale_correlated_response_rearms_bounded_recovery_but_unrelated_response_does_not() {
        let stale = snapshot();
        let current = stale
            .clone()
            .try_with_publication_generation(stale.evidence().publication_generation + 1)
            .expect("new publication");
        let mut bridge = ChartDataBridge::try_new(NonZeroUsize::MIN, &current).expect("bridge");
        bridge.mark_stream_invalid();
        for attempt in 1..=MAX_RECOVERY_DISPATCH_ATTEMPTS {
            let request = dispatch(&mut bridge);
            assert!(
                !bridge
                    .install_recovery_snapshot(request + 1, &current)
                    .expect("unrelated")
            );
            assert!(bridge.metrics().recovery_dispatched);
            assert!(
                !bridge
                    .install_recovery_snapshot(request, &stale)
                    .expect("stale")
            );
            assert!(!bridge.metrics().recovery_dispatched);
            assert_eq!(
                bridge.metrics().recovery_pending,
                attempt < MAX_RECOVERY_DISPATCH_ATTEMPTS
            );
            assert_eq!(
                bridge.accepted_publication_generation,
                current.evidence().publication_generation
            );
        }
        assert!(bridge.requires_snapshot());
        assert_eq!(bridge.metrics().failed_recoveries, 3);
    }

    #[test]
    fn ordinary_snapshot_accepts_new_forming_revision_but_not_duplicate_or_rewind() {
        let baseline = snapshot();
        let mut bridge = ChartDataBridge::try_new(NonZeroUsize::MIN, &baseline).expect("bridge");
        assert!(bridge.install_snapshot(&baseline).is_err());
        let revision = baseline
            .clone()
            .try_with_publication_generation(baseline.evidence().publication_generation + 1)
            .expect("new publication");
        bridge
            .install_snapshot(&revision)
            .expect("forming revision advances");
        assert!(bridge.install_snapshot(&baseline).is_err());
    }
}
