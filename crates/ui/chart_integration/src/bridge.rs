//! Bounded replay-to-Origin data bridge with correlated recovery commands.

use axiusflow_application::{
    ProvenancedMarketBar, ReplaySession, ReplaySnapshot, ReplayStreamUpdate, ReplayValidationError,
    ResnapshotReason, SequenceDecision,
};
use axiusflow_terminal_ui::BoundedUiQueue;
use std::{num::NonZeroUsize, time::Instant};

pub(crate) const MAX_RECOVERY_DISPATCH_ATTEMPTS: usize = 3;

pub(crate) enum RebaselineSnapshotQueueOutcome {
    Selected { publication_generation: u64 },
    Superseded,
}

#[derive(Debug)]
struct QueuedChartUpdate {
    update: ReplayStreamUpdate,
    publication_generation: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChartSeriesIdentity {
    instrument_id: String,
    instrument_revision: u64,
    bar_definition_id: String,
    bar_definition_version: u32,
    bar_interval_seconds: u32,
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
            schema_version: snapshot.evidence().schema_version,
        }
    }

    fn matches(&self, snapshot: &ReplaySnapshot) -> bool {
        self.instrument_id == snapshot.instrument().instrument_id.as_str()
            && self.instrument_revision == snapshot.instrument().revision
            && self.bar_definition_id == snapshot.bar_definition().definition_id
            && self.bar_definition_version == snapshot.bar_definition().version
            && self.bar_interval_seconds == snapshot.bar_definition().interval_seconds
            && self.schema_version == snapshot.evidence().schema_version
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChartStreamProvenance {
    partition_id: u32,
    ownership_epoch: u64,
    schema_version: u32,
}

impl ChartStreamProvenance {
    fn from_snapshot(snapshot: &ReplaySnapshot) -> Self {
        Self {
            partition_id: snapshot.evidence().partition_id,
            ownership_epoch: snapshot.evidence().ownership_epoch,
            schema_version: snapshot.evidence().schema_version,
        }
    }

    fn matches_delta(
        self,
        delta: &axiusflow_application::StreamDelta<ProvenancedMarketBar>,
    ) -> bool {
        let provenance = delta.item().provenance();
        provenance.partition_id == self.partition_id
            && provenance.ownership_epoch == self.ownership_epoch
            && provenance.schema_version == self.schema_version
    }
}

/// One queue drain collapsed into at most one authoritative Origin mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MergedChartData {
    snapshot: Option<ReplaySnapshot>,
    accepted_deltas: Vec<ProvenancedMarketBar>,
    last_sequence_decision: Option<SequenceDecision>,
}

impl MergedChartData {
    /// Returns the last snapshot in this drain, if one replaced prior state.
    #[must_use]
    pub const fn snapshot(&self) -> Option<&ReplaySnapshot> {
        self.snapshot.as_ref()
    }

    /// Returns contiguous deltas accepted after the retained snapshot or baseline.
    #[must_use]
    pub fn accepted_deltas(&self) -> &[ProvenancedMarketBar] {
        &self.accepted_deltas
    }

    /// Returns the last delta classification observed in this drain.
    #[must_use]
    pub const fn last_sequence_decision(&self) -> Option<SequenceDecision> {
        self.last_sequence_decision
    }

    #[must_use]
    pub(crate) const fn mutates_series(&self) -> bool {
        self.snapshot.is_some() || !self.accepted_deltas.is_empty()
    }
}

/// Correlated command for one bounded background snapshot recovery attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplayRecoveryCommand {
    pub request_id: u64,
    pub reason: ResnapshotReason,
}

/// Observable bounded-bridge state used for resnapshot and overload telemetry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
pub struct ChartDataBridge {
    queue: BoundedUiQueue<QueuedChartUpdate>,
    rebaseline_snapshot_queued: bool,
    queued_rebaseline_provenance: Option<ChartStreamProvenance>,
    fallback_rebaseline_reason: Option<ResnapshotReason>,
    session: Option<ReplaySession>,
    accepted_series: ChartSeriesIdentity,
    accepted_partition_id: u32,
    accepted_ownership_epoch: u64,
    accepted_generation: u64,
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
    pub fn try_new(
        capacity: NonZeroUsize,
        snapshot: &ReplaySnapshot,
    ) -> Result<Self, ReplayValidationError> {
        Ok(Self {
            queue: BoundedUiQueue::new(capacity),
            rebaseline_snapshot_queued: false,
            queued_rebaseline_provenance: None,
            fallback_rebaseline_reason: None,
            session: Some(ReplaySession::try_new(snapshot)?),
            accepted_series: ChartSeriesIdentity::from_snapshot(snapshot),
            accepted_partition_id: snapshot.evidence().partition_id,
            accepted_ownership_epoch: snapshot.evidence().ownership_epoch,
            accepted_generation: snapshot.evidence().generation,
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
    pub fn try_push(&mut self, update: ReplayStreamUpdate) -> Result<(), Box<ReplayStreamUpdate>> {
        self.try_push_queued(update, None)
    }

    pub(crate) fn try_push_publication(
        &mut self,
        update: ReplayStreamUpdate,
        publication_generation: u64,
    ) -> Result<(), Box<ReplayStreamUpdate>> {
        self.try_push_queued(update, Some(publication_generation))
    }

    fn try_push_queued(
        &mut self,
        update: ReplayStreamUpdate,
        publication_generation: Option<u64>,
    ) -> Result<(), Box<ReplayStreamUpdate>> {
        match self.queue.try_push(QueuedChartUpdate {
            update,
            publication_generation,
        }) {
            Ok(()) => Ok(()),
            Err(rejected) => {
                self.queue_overflows = self.queue_overflows.saturating_add(1);
                self.queue.drain().for_each(drop);
                self.rebaseline_snapshot_queued = false;
                self.queued_rebaseline_provenance = None;
                self.session = None;
                self.request_resnapshot(ResnapshotReason::QueueOverflow);
                Err(Box::new(rejected.update))
            }
        }
    }

    pub(crate) fn queue_rebaseline_snapshot(
        &mut self,
        snapshot: ReplaySnapshot,
        publication_generation: u64,
    ) -> RebaselineSnapshotQueueOutcome {
        if !self.snapshot_may_replace_accepted(&snapshot, self.fallback_rebaseline_reason) {
            self.rejected_stale_snapshots = self.rejected_stale_snapshots.saturating_add(1);
            return RebaselineSnapshotQueueOutcome::Superseded;
        }
        let mut selected: Option<(ReplaySnapshot, u64)> = None;
        let mut retained_deltas = Vec::new();
        for queued_update in self.queue.drain() {
            match queued_update.update {
                ReplayStreamUpdate::Snapshot(queued) => {
                    let queued_generation = queued_update
                        .publication_generation
                        .unwrap_or(queued.evidence().generation);
                    let advances_accepted = snapshot_may_replace(
                        &queued,
                        self.accepted_partition_id,
                        self.accepted_ownership_epoch,
                        self.accepted_generation,
                        self.accepted_last_sequence,
                        self.accepted_series.matches(&queued),
                        self.fallback_rebaseline_reason,
                    );
                    let advances_selected = selected.as_ref().is_none_or(|(current, _)| {
                        snapshot_may_replace(
                            &queued,
                            current.evidence().partition_id,
                            current.evidence().ownership_epoch,
                            current.evidence().generation,
                            current.evidence().last_sequence,
                            snapshots_share_series(current, &queued),
                            self.fallback_rebaseline_reason,
                        )
                    });
                    if advances_accepted && advances_selected {
                        let matches_retained_series = selected.as_ref().map_or_else(
                            || self.accepted_series.matches(&queued),
                            |(current, _)| snapshots_share_series(current, &queued),
                        );
                        retain_snapshot_tail(
                            &mut retained_deltas,
                            &queued,
                            queued_generation,
                            matches_retained_series,
                        );
                        selected = Some((queued, queued_generation));
                    }
                }
                ReplayStreamUpdate::Delta(delta) => {
                    if selected.as_ref().is_none_or(|(current, _)| {
                        delta.sequence() > current.evidence().last_sequence
                            && delta_matches_snapshot(&delta, current)
                    }) {
                        retained_deltas.push(QueuedChartUpdate {
                            update: ReplayStreamUpdate::Delta(delta),
                            publication_generation: queued_update.publication_generation,
                        });
                    }
                }
            }
        }
        let incoming_selected = selected.as_ref().is_none_or(|(current, _)| {
            snapshot_may_replace(
                &snapshot,
                current.evidence().partition_id,
                current.evidence().ownership_epoch,
                current.evidence().generation,
                current.evidence().last_sequence,
                snapshots_share_series(current, &snapshot),
                self.fallback_rebaseline_reason,
            )
        });
        if incoming_selected {
            let matches_retained_series = selected.as_ref().map_or_else(
                || self.accepted_series.matches(&snapshot),
                |(current, _)| snapshots_share_series(current, &snapshot),
            );
            retain_snapshot_tail(
                &mut retained_deltas,
                &snapshot,
                publication_generation,
                matches_retained_series,
            );
            selected = Some((snapshot, publication_generation));
        } else {
            self.rejected_stale_snapshots = self.rejected_stale_snapshots.saturating_add(1);
        }
        let Some((selected, selected_generation)) = selected else {
            return RebaselineSnapshotQueueOutcome::Superseded;
        };
        self.replace_rebaseline_queue(
            selected,
            selected_generation,
            retained_deltas,
            incoming_selected,
        )
    }

    fn replace_rebaseline_queue(
        &mut self,
        selected: ReplaySnapshot,
        selected_generation: u64,
        retained_deltas: Vec<QueuedChartUpdate>,
        incoming_selected: bool,
    ) -> RebaselineSnapshotQueueOutcome {
        let selected_provenance = ChartStreamProvenance::from_snapshot(&selected);
        let queued_generation = retained_deltas
            .last()
            .and_then(|update| update.publication_generation)
            .unwrap_or(selected_generation);
        let updates = std::iter::once(QueuedChartUpdate {
            update: ReplayStreamUpdate::Snapshot(selected),
            publication_generation: Some(selected_generation),
        })
        .chain(retained_deltas);
        for update in updates {
            if self.queue.try_push(update).is_err() {
                self.queue_overflows = self.queue_overflows.saturating_add(1);
                self.queue.drain().for_each(drop);
                self.rebaseline_snapshot_queued = false;
                self.queued_rebaseline_provenance = None;
                self.session = None;
                self.request_resnapshot(ResnapshotReason::QueueOverflow);
                return RebaselineSnapshotQueueOutcome::Superseded;
            }
        }
        self.rebaseline_snapshot_queued = true;
        self.queued_rebaseline_provenance = Some(selected_provenance);
        if incoming_selected {
            RebaselineSnapshotQueueOutcome::Selected {
                publication_generation: queued_generation,
            }
        } else {
            RebaselineSnapshotQueueOutcome::Superseded
        }
    }

    /// Replaces session state immediately and discards obsolete queued updates.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot establish resumable session state.
    pub fn install_snapshot(
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
    pub fn drain_merged(&mut self) -> Result<Option<MergedChartData>, ReplayValidationError> {
        let updates: Vec<_> = self.queue.drain().collect();
        if updates.is_empty() {
            return Ok(None);
        }
        let transition_reason = if self.rebaseline_snapshot_queued {
            self.fallback_rebaseline_reason
        } else {
            None
        };
        self.rebaseline_snapshot_queued = false;
        self.queued_rebaseline_provenance = None;

        let mut candidate_session = self.session;
        let mut candidate_series = self.accepted_series.clone();
        let mut candidate_partition_id = self.accepted_partition_id;
        let mut candidate_ownership_epoch = self.accepted_ownership_epoch;
        let mut candidate_generation = self.accepted_generation;
        let mut candidate_last_sequence = self.accepted_last_sequence;
        let mut merged = MergedChartData {
            snapshot: None,
            accepted_deltas: Vec::with_capacity(updates.len()),
            last_sequence_decision: None,
        };
        for queued_update in updates {
            match queued_update.update {
                ReplayStreamUpdate::Snapshot(snapshot) => {
                    if self.pending_resnapshot.is_some() {
                        self.rejected_uncorrelated_snapshots =
                            self.rejected_uncorrelated_snapshots.saturating_add(1);
                        merged.last_sequence_decision = Some(SequenceDecision::SnapshotRequired);
                        continue;
                    }
                    if !snapshot_may_replace(
                        &snapshot,
                        candidate_partition_id,
                        candidate_ownership_epoch,
                        candidate_generation,
                        candidate_last_sequence,
                        candidate_series.matches(&snapshot),
                        transition_reason,
                    ) {
                        self.rejected_stale_snapshots =
                            self.rejected_stale_snapshots.saturating_add(1);
                        continue;
                    }
                    candidate_session = Some(ReplaySession::try_new(&snapshot)?);
                    candidate_series = ChartSeriesIdentity::from_snapshot(&snapshot);
                    candidate_partition_id = snapshot.evidence().partition_id;
                    candidate_ownership_epoch = snapshot.evidence().ownership_epoch;
                    candidate_generation = snapshot.evidence().generation;
                    candidate_last_sequence = snapshot.evidence().last_sequence;
                    merged.snapshot = Some(snapshot);
                    merged.accepted_deltas.clear();
                    merged.last_sequence_decision = None;
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
                        merged.last_sequence_decision = Some(SequenceDecision::SnapshotRequired);
                        continue;
                    };
                    let decision = session.accept_delta(&delta)?;
                    merged.last_sequence_decision = Some(decision);
                    if matches!(decision, SequenceDecision::Gap { .. }) {
                        self.request_resnapshot(ResnapshotReason::SequenceGap);
                    }
                    if decision == SequenceDecision::Accepted {
                        if let Some(publication_generation) = queued_update.publication_generation {
                            candidate_generation = publication_generation;
                        }
                        candidate_last_sequence = delta.sequence();
                        merged.accepted_deltas.push(delta.item().clone());
                    }
                }
            }
        }
        self.session = candidate_session;
        self.accepted_series = candidate_series;
        self.accepted_partition_id = candidate_partition_id;
        self.accepted_ownership_epoch = candidate_ownership_epoch;
        self.accepted_generation = candidate_generation;
        self.accepted_last_sequence = candidate_last_sequence;
        if merged.snapshot.is_some() {
            self.fallback_rebaseline_reason = None;
        }
        Ok(Some(merged))
    }

    /// Returns the number of producer updates waiting for the next drain.
    #[must_use]
    pub fn queued_update_count(&self) -> usize {
        self.queue.len()
    }

    pub(crate) const fn has_queued_rebaseline_snapshot(&self) -> bool {
        self.rebaseline_snapshot_queued
    }

    pub(crate) fn delta_matches_queued_rebaseline(
        &self,
        delta: &axiusflow_application::StreamDelta<ProvenancedMarketBar>,
    ) -> bool {
        self.queued_rebaseline_provenance
            .is_none_or(|provenance| provenance.matches_delta(delta))
    }

    pub(crate) const fn accepted_publication_generation(&self) -> u64 {
        self.accepted_generation
    }

    /// Returns whether sequence state requires a replacement snapshot.
    #[must_use]
    pub fn requires_snapshot(&self) -> bool {
        self.session.is_none_or(ReplaySession::requires_snapshot)
    }

    /// Returns the next expected sequence when a snapshot is installed.
    #[must_use]
    pub fn expected_sequence(&self) -> Option<u64> {
        self.session.and_then(ReplaySession::expected_sequence)
    }

    /// Peeks the retryable command without claiming that a worker queue accepted it.
    #[must_use]
    pub fn pending_resnapshot_request(&self) -> Option<ReplayRecoveryCommand> {
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
    pub fn try_dispatch_recovery<DispatchError>(
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
    pub fn install_recovery_snapshot(
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
            return Ok(false);
        }
        self.install_snapshot_state(snapshot)?;
        self.complete_recovery();
        Ok(true)
    }

    /// Latches recovery after a decoder/model invariant failure.
    pub fn mark_stream_invalid(&mut self) {
        self.mark_stream_invalid_for(ResnapshotReason::SequenceGap);
    }

    /// Latches recovery with the transport-neutral reason reported by a runtime port.
    pub fn mark_stream_invalid_for(&mut self, reason: ResnapshotReason) {
        self.fallback_rebaseline_reason = None;
        if self.rebaseline_snapshot_queued {
            self.queue.drain().for_each(drop);
            self.rebaseline_snapshot_queued = false;
            self.queued_rebaseline_provenance = None;
        }
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
    pub fn mark_recovery_failed(&mut self, request_id: u64) -> bool {
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

    /// Terminally cancels the matching recovery after the worker exhausts its retry cycle.
    pub fn cancel_recovery(&mut self, request_id: u64) -> bool {
        if self.pending_resnapshot.map(|command| command.request_id) != Some(request_id) {
            return false;
        }
        self.failed_recoveries = self.failed_recoveries.saturating_add(1);
        self.cancel_recovery_state();
        true
    }

    pub(crate) fn cancel_pending_recovery_for(&mut self, reason: ResnapshotReason) -> bool {
        if self.pending_resnapshot.map(|command| command.reason) != Some(reason) {
            return false;
        }
        self.failed_recoveries = self.failed_recoveries.saturating_add(1);
        self.cancel_recovery_state();
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
        self.rebaseline_snapshot_queued = false;
        self.queued_rebaseline_provenance = None;
        self.session = Some(session);
        self.accepted_series = ChartSeriesIdentity::from_snapshot(snapshot);
        self.accepted_partition_id = snapshot.evidence().partition_id;
        self.accepted_ownership_epoch = snapshot.evidence().ownership_epoch;
        self.accepted_generation = snapshot.evidence().generation;
        self.accepted_last_sequence = snapshot.evidence().last_sequence;
        self.fallback_rebaseline_reason = None;
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
            current_generation: self.accepted_generation,
            current_last_sequence: self.accepted_last_sequence,
            actual_generation: snapshot.evidence().generation,
            actual_last_sequence: snapshot.evidence().last_sequence,
        })
    }

    fn cancel_recovery_state(&mut self) {
        self.fallback_rebaseline_reason = self.pending_resnapshot.map(|command| command.reason);
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
            self.accepted_partition_id,
            self.accepted_ownership_epoch,
            self.accepted_generation,
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
        self.fallback_rebaseline_reason = None;
        self.recovery_dispatched = false;
        self.recovery_dispatch_attempts = 0;
    }

    /// Returns queue depth, overload, and recovery telemetry.
    #[must_use]
    pub fn metrics(&self) -> ChartBridgeMetrics {
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
    current_ownership_epoch: u64,
    current_generation: u64,
    current_last_sequence: u64,
) -> bool {
    snapshot.evidence().ownership_epoch > current_ownership_epoch
        || (snapshot.evidence().ownership_epoch == current_ownership_epoch
            && snapshot.evidence().generation >= current_generation
            && snapshot.evidence().last_sequence > current_last_sequence)
}

fn snapshot_may_replace(
    snapshot: &ReplaySnapshot,
    current_partition_id: u32,
    current_ownership_epoch: u64,
    current_generation: u64,
    current_last_sequence: u64,
    same_series: bool,
    transition_reason: Option<ResnapshotReason>,
) -> bool {
    if snapshot.evidence().partition_id != current_partition_id {
        return same_series
            && matches!(
                transition_reason,
                Some(ResnapshotReason::OwnershipHandoff | ResnapshotReason::TransportReset)
            );
    }
    if snapshot.evidence().ownership_epoch < current_ownership_epoch {
        return false;
    }
    if !same_series && transition_reason != Some(ResnapshotReason::SchemaChanged) {
        return false;
    }
    if snapshot.evidence().ownership_epoch > current_ownership_epoch {
        return true;
    }
    if !same_series {
        return true;
    }
    if transition_reason == Some(ResnapshotReason::TransportReset)
        && snapshot.evidence().generation == current_generation
        && snapshot.evidence().last_sequence == current_last_sequence
    {
        return true;
    }
    snapshot_advances(
        snapshot,
        current_ownership_epoch,
        current_generation,
        current_last_sequence,
    )
}

fn snapshots_share_series(left: &ReplaySnapshot, right: &ReplaySnapshot) -> bool {
    left.instrument() == right.instrument()
        && left.bar_definition() == right.bar_definition()
        && left.evidence().schema_version == right.evidence().schema_version
}

fn retain_snapshot_tail(
    retained_deltas: &mut Vec<QueuedChartUpdate>,
    snapshot: &ReplaySnapshot,
    snapshot_generation: u64,
    matches_retained_series: bool,
) {
    if !matches_retained_series {
        retained_deltas.clear();
        return;
    }
    let mut expected_sequence = snapshot.evidence().last_sequence.saturating_add(1);
    let mut expected_generation = snapshot_generation.saturating_add(1);
    let mut coherent_tail = Vec::with_capacity(retained_deltas.len());
    for mut queued_update in retained_deltas.drain(..) {
        let ReplayStreamUpdate::Delta(delta) = &queued_update.update else {
            continue;
        };
        if delta.sequence() < expected_sequence {
            continue;
        }
        let generation = queued_update
            .publication_generation
            .unwrap_or(expected_generation);
        if delta.sequence() != expected_sequence
            || generation != expected_generation
            || !delta_matches_snapshot(delta, snapshot)
        {
            break;
        }
        queued_update.publication_generation = Some(generation);
        coherent_tail.push(queued_update);
        expected_sequence = expected_sequence.saturating_add(1);
        expected_generation = expected_generation.saturating_add(1);
    }
    *retained_deltas = coherent_tail;
}

fn delta_matches_snapshot(
    delta: &axiusflow_application::StreamDelta<ProvenancedMarketBar>,
    snapshot: &ReplaySnapshot,
) -> bool {
    let provenance = delta.item().provenance();
    let evidence = snapshot.evidence();
    provenance.partition_id == evidence.partition_id
        && provenance.ownership_epoch == evidence.ownership_epoch
        && provenance.schema_version == evidence.schema_version
}
