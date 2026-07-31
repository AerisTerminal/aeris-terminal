//! Axiusflow host integration for Origin Charts' existing GPUI backend.
//!
//! Origin owns chart state, layout, scales, interactions, frames, and rendering.
//! This crate only negotiates GPUI window geometry and submits the resulting
//! immutable Origin frame to `origin_render_gpui`.

use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarReplayPort, MarketEventProvenance,
    MarketStreamCommand, MarketStreamCommandOffer, MarketStreamControlSignal, MarketStreamEvent,
    MarketStreamPublication, MarketStreamRuntimePort, ProvenancedMarketBar, ReplaySession,
    ReplaySnapshot, ReplayStreamUpdate, ReplayValidationError, ResnapshotReason, SequenceDecision,
    UseCase,
};
use axiusflow_design_system::{AxiusflowTheme, ThemeColor};
use axiusflow_protocols::StreamProtocolError;
use axiusflow_terminal_ui::BoundedUiQueue;
use core::fmt;
use gpui::{App, Bounds, Context, Entity, Render, Window, canvas, div, prelude::*, rgb};
use num_traits::ToPrimitive;
use origin_engine::{ChartEngine, ChartFrame, SeriesKind};
use origin_render::draw_list::Prim;
use origin_render_gpui::{
    GpuiChartRenderer, GpuiRenderError, OriginViewport, PreparedOriginFrame, backend::measure_text,
};
use std::{collections::BTreeMap, error::Error, num::NonZeroUsize, time::Instant};

const SCALE_FACTOR_EPSILON: f32 = 1.0e-4;
const DEFAULT_CHART_DATA_QUEUE_CAPACITY: usize = 64;
const DEFAULT_CHART_SERIES_MAX_POINTS: usize = 4_096;
const MAX_RECOVERY_DISPATCH_ATTEMPTS: usize = 3;

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
    const fn mutates_series(&self) -> bool {
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
    queue: BoundedUiQueue<ReplayStreamUpdate>,
    session: Option<ReplaySession>,
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
            session: Some(ReplaySession::try_new(snapshot)?),
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

        let mut candidate_session = self.session;
        let mut candidate_generation = self.accepted_generation;
        let mut candidate_last_sequence = self.accepted_last_sequence;
        let mut merged = MergedChartData {
            snapshot: None,
            accepted_deltas: Vec::with_capacity(updates.len()),
            last_sequence_decision: None,
        };
        for update in updates {
            match update {
                ReplayStreamUpdate::Snapshot(snapshot) => {
                    if self.pending_resnapshot.is_some() {
                        self.rejected_uncorrelated_snapshots =
                            self.rejected_uncorrelated_snapshots.saturating_add(1);
                        merged.last_sequence_decision = Some(SequenceDecision::SnapshotRequired);
                        continue;
                    }
                    if !snapshot_advances(&snapshot, candidate_generation, candidate_last_sequence)
                    {
                        self.rejected_stale_snapshots =
                            self.rejected_stale_snapshots.saturating_add(1);
                        continue;
                    }
                    candidate_session = Some(ReplaySession::try_new(&snapshot)?);
                    candidate_generation = snapshot.evidence().generation;
                    candidate_last_sequence = snapshot.evidence().last_sequence;
                    merged.snapshot = Some(snapshot);
                    merged.accepted_deltas.clear();
                    merged.last_sequence_decision = None;
                }
                ReplayStreamUpdate::Delta(delta) => {
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
                        candidate_last_sequence = delta.sequence();
                        merged.accepted_deltas.push(delta.item().clone());
                    }
                }
            }
        }
        self.session = candidate_session;
        self.accepted_generation = candidate_generation;
        self.accepted_last_sequence = candidate_last_sequence;
        Ok(Some(merged))
    }

    /// Returns the number of producer updates waiting for the next drain.
    #[must_use]
    pub fn queued_update_count(&self) -> usize {
        self.queue.len()
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
        if !snapshot_advances(
            snapshot,
            self.accepted_generation,
            self.accepted_last_sequence,
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
        self.session = None;
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
        self.accepted_generation = snapshot.evidence().generation;
        self.accepted_last_sequence = snapshot.evidence().last_sequence;
        Ok(())
    }

    fn ensure_snapshot_advances(
        &mut self,
        snapshot: &ReplaySnapshot,
    ) -> Result<(), ReplayValidationError> {
        if snapshot_advances(
            snapshot,
            self.accepted_generation,
            self.accepted_last_sequence,
        ) {
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
        self.pending_resnapshot = None;
        self.recovery_dispatched = false;
        self.recovery_dispatch_attempts = 0;
        self.recovery_started = None;
        self.canceled_recoveries = self.canceled_recoveries.saturating_add(1);
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

/// Result of offering the chart bridge's current correlated recovery command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChartStreamRecoveryDispatch {
    NoPendingRecovery,
    Accepted,
    Full,
}

/// One bounded coordinator transition after polling a runtime port once.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChartStreamPollOutcome {
    Idle,
    Connected {
        connection_epoch: u64,
        attempt: usize,
    },
    InitialSnapshot {
        generation: u64,
        first_sequence: u64,
        last_sequence: u64,
    },
    PublicationQueued {
        generation: u64,
        first_sequence: u64,
        last_sequence: u64,
    },
    PublicationRejectedBeforeSnapshot {
        generation: u64,
        first_sequence: u64,
        last_sequence: u64,
    },
    PublicationGenerationDiscontinuity {
        expected_predecessor: Option<u64>,
        actual_predecessor: Option<u64>,
        generation: u64,
        last_sequence: u64,
    },
    PublicationBackpressured {
        generation: u64,
        first_sequence: u64,
        last_sequence: u64,
    },
    RecoverySnapshot {
        request_id: u64,
        installed: bool,
        generation: u64,
        last_sequence: u64,
    },
    Control(MarketStreamControlSignal),
    StreamInvalid {
        reason: ResnapshotReason,
        dropped_events: usize,
        latched: bool,
    },
    RecoveryAttemptFailed {
        request_id: Option<u64>,
        attempt: usize,
        remaining: usize,
    },
    RecoveryExhausted {
        request_id: Option<u64>,
        reason: ResnapshotReason,
        dropped_events: usize,
        canceled: bool,
    },
    RecoveryRejected {
        request_id: u64,
        active_request_id: u64,
        made_retryable: bool,
    },
    Stopped {
        graceful: bool,
        dropped_events: usize,
    },
}

/// Observable state for the transport-neutral runtime-to-chart coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChartStreamCoordinatorMetrics {
    pub bridge_installed: bool,
    pub publication_generation: Option<u64>,
    pub connected_events: u64,
    pub control_events: u64,
    pub publications_accepted: u64,
    pub publications_rejected: u64,
    pub stream_invalidations: u64,
    pub recovery_snapshots_installed: u64,
    pub recovery_snapshots_rejected: u64,
    pub recovery_exhaustions: u64,
    pub reported_dropped_events: u64,
    pub stopped: bool,
}

/// Error from polling one neutral runtime event into the bounded chart bridge.
#[derive(Debug)]
pub enum ChartStreamCoordinatorError<RuntimeError> {
    Runtime(RuntimeError),
    Replay(ReplayValidationError),
}

impl<RuntimeError: fmt::Display> fmt::Display for ChartStreamCoordinatorError<RuntimeError> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => write!(formatter, "market stream runtime failed: {error}"),
            Self::Replay(error) => write!(formatter, "market stream replay failed: {error}"),
        }
    }
}

impl<RuntimeError: Error + 'static> Error for ChartStreamCoordinatorError<RuntimeError> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Runtime(error) => Some(error),
            Self::Replay(error) => Some(error),
        }
    }
}

/// Bounded provider-neutral composition of one stream runtime and one chart bridge.
///
/// The coordinator owns no transport and introduces no queue beyond [`ChartDataBridge`].
/// It polls at most one ready runtime event per call, preserving consumer scheduling.
#[derive(Debug)]
pub struct ChartStreamCoordinator {
    bridge_capacity: NonZeroUsize,
    bridge: Option<ChartDataBridge>,
    publication_generation: Option<u64>,
    connected_events: u64,
    control_events: u64,
    publications_accepted: u64,
    publications_rejected: u64,
    stream_invalidations: u64,
    recovery_snapshots_installed: u64,
    recovery_snapshots_rejected: u64,
    recovery_exhaustions: u64,
    reported_dropped_events: u64,
    stopped: bool,
}

impl ChartStreamCoordinator {
    /// Creates an empty coordinator that will install its bridge from the first snapshot.
    #[must_use]
    pub const fn new(bridge_capacity: NonZeroUsize) -> Self {
        Self {
            bridge_capacity,
            bridge: None,
            publication_generation: None,
            connected_events: 0,
            control_events: 0,
            publications_accepted: 0,
            publications_rejected: 0,
            stream_invalidations: 0,
            recovery_snapshots_installed: 0,
            recovery_snapshots_rejected: 0,
            recovery_exhaustions: 0,
            reported_dropped_events: 0,
            stopped: false,
        }
    }

    /// Offers an initial connect without blocking.
    ///
    /// # Errors
    ///
    /// Returns the runtime port's disconnection or adapter failure.
    pub fn try_connect<Runtime: MarketStreamRuntimePort>(
        &self,
        runtime: &Runtime,
    ) -> Result<MarketStreamCommandOffer, Runtime::Error> {
        runtime.try_send_stream_command(MarketStreamCommand::Connect)
    }

    /// Offers shutdown without blocking.
    ///
    /// # Errors
    ///
    /// Returns the runtime port's disconnection or adapter failure.
    pub fn try_shutdown<Runtime: MarketStreamRuntimePort>(
        &self,
        runtime: &Runtime,
    ) -> Result<MarketStreamCommandOffer, Runtime::Error> {
        runtime.try_send_stream_command(MarketStreamCommand::Shutdown)
    }

    /// Offers the current correlated recovery command exactly once per accepted dispatch.
    ///
    /// # Errors
    ///
    /// Returns the runtime port's disconnection or adapter failure. A full command
    /// boundary remains retryable and is returned as [`ChartStreamRecoveryDispatch::Full`].
    pub fn try_dispatch_recovery<Runtime: MarketStreamRuntimePort>(
        &mut self,
        runtime: &Runtime,
    ) -> Result<ChartStreamRecoveryDispatch, Runtime::Error> {
        let Some(bridge) = self.bridge.as_mut() else {
            return Ok(ChartStreamRecoveryDispatch::NoPendingRecovery);
        };
        match bridge.try_dispatch_recovery(|ReplayRecoveryCommand { request_id, reason }| {
            match runtime
                .try_send_stream_command(MarketStreamCommand::Recover { request_id, reason })
            {
                Ok(MarketStreamCommandOffer::Accepted) => Ok(()),
                Ok(MarketStreamCommandOffer::Full) => Err(None),
                Err(error) => Err(Some(error)),
            }
        }) {
            Ok(true) => Ok(ChartStreamRecoveryDispatch::Accepted),
            Ok(false) => Ok(ChartStreamRecoveryDispatch::NoPendingRecovery),
            Err(None) => Ok(ChartStreamRecoveryDispatch::Full),
            Err(Some(error)) => Err(error),
        }
    }

    /// Polls and applies at most one ready neutral runtime event.
    ///
    /// # Errors
    ///
    /// Returns a runtime port failure or replay validation failure.
    pub fn poll_once<Runtime: MarketStreamRuntimePort>(
        &mut self,
        runtime: &Runtime,
    ) -> Result<ChartStreamPollOutcome, ChartStreamCoordinatorError<Runtime::Error>> {
        let Some(event) = runtime
            .try_recv_stream_event()
            .map_err(ChartStreamCoordinatorError::Runtime)?
        else {
            return Ok(ChartStreamPollOutcome::Idle);
        };
        self.apply_event(event)
            .map_err(ChartStreamCoordinatorError::Replay)
    }

    fn apply_event(
        &mut self,
        event: MarketStreamEvent,
    ) -> Result<ChartStreamPollOutcome, ReplayValidationError> {
        match event {
            MarketStreamEvent::Connected {
                connection_epoch,
                attempt,
            } => {
                self.connected_events = self.connected_events.saturating_add(1);
                Ok(ChartStreamPollOutcome::Connected {
                    connection_epoch,
                    attempt,
                })
            }
            MarketStreamEvent::Publication(publication) => self.apply_publication(*publication),
            MarketStreamEvent::RecoverySnapshot {
                request_id,
                publication,
            } => self.apply_recovery_snapshot(request_id, *publication),
            MarketStreamEvent::Control(signal) => {
                self.control_events = self.control_events.saturating_add(1);
                Ok(ChartStreamPollOutcome::Control(signal))
            }
            MarketStreamEvent::StreamInvalid {
                reason,
                dropped_events,
                ..
            } => {
                self.stream_invalidations = self.stream_invalidations.saturating_add(1);
                self.record_dropped_events(dropped_events);
                let latched = self.bridge.as_mut().is_some_and(|bridge| {
                    bridge.mark_stream_invalid_for(reason);
                    true
                });
                Ok(ChartStreamPollOutcome::StreamInvalid {
                    reason,
                    dropped_events,
                    latched,
                })
            }
            MarketStreamEvent::RecoveryAttemptFailed {
                request_id,
                attempt,
                remaining,
            } => Ok(ChartStreamPollOutcome::RecoveryAttemptFailed {
                request_id,
                attempt,
                remaining,
            }),
            MarketStreamEvent::RecoveryExhausted {
                request_id,
                reason,
                dropped_events,
                ..
            } => {
                self.recovery_exhaustions = self.recovery_exhaustions.saturating_add(1);
                self.record_dropped_events(dropped_events);
                let canceled = request_id.is_some_and(|request_id| {
                    self.bridge
                        .as_mut()
                        .is_some_and(|bridge| bridge.cancel_recovery(request_id))
                });
                Ok(ChartStreamPollOutcome::RecoveryExhausted {
                    request_id,
                    reason,
                    dropped_events,
                    canceled,
                })
            }
            MarketStreamEvent::RecoveryRejected {
                request_id,
                active_request_id,
            } => {
                let made_retryable = self
                    .bridge
                    .as_mut()
                    .is_some_and(|bridge| bridge.mark_recovery_failed(request_id));
                Ok(ChartStreamPollOutcome::RecoveryRejected {
                    request_id,
                    active_request_id,
                    made_retryable,
                })
            }
            MarketStreamEvent::Stopped {
                graceful,
                dropped_events,
            } => {
                self.record_dropped_events(dropped_events);
                self.stopped = true;
                Ok(ChartStreamPollOutcome::Stopped {
                    graceful,
                    dropped_events,
                })
            }
        }
    }

    fn apply_publication(
        &mut self,
        publication: MarketStreamPublication,
    ) -> Result<ChartStreamPollOutcome, ReplayValidationError> {
        let (_, update, publication_generation, predecessor_generation) = publication.into_parts();
        let generation = publication_generation.generation();
        let (first_sequence, last_sequence) = publication_generation.sequence_range();
        let Some(bridge) = self.bridge.as_mut() else {
            return match update {
                ReplayStreamUpdate::Snapshot(snapshot) => {
                    self.bridge = Some(ChartDataBridge::try_new(self.bridge_capacity, &snapshot)?);
                    self.publication_generation = Some(generation);
                    self.publications_accepted = self.publications_accepted.saturating_add(1);
                    Ok(ChartStreamPollOutcome::InitialSnapshot {
                        generation,
                        first_sequence,
                        last_sequence,
                    })
                }
                ReplayStreamUpdate::Delta(_) => {
                    self.publications_rejected = self.publications_rejected.saturating_add(1);
                    Ok(ChartStreamPollOutcome::PublicationRejectedBeforeSnapshot {
                        generation,
                        first_sequence,
                        last_sequence,
                    })
                }
            };
        };
        if matches!(&update, ReplayStreamUpdate::Delta(_))
            && predecessor_generation != self.publication_generation
        {
            self.publications_rejected = self.publications_rejected.saturating_add(1);
            return Ok(ChartStreamPollOutcome::PublicationGenerationDiscontinuity {
                expected_predecessor: self.publication_generation,
                actual_predecessor: predecessor_generation,
                generation,
                last_sequence,
            });
        }
        if bridge.try_push(update).is_ok() {
            self.publication_generation = Some(generation);
            self.publications_accepted = self.publications_accepted.saturating_add(1);
            Ok(ChartStreamPollOutcome::PublicationQueued {
                generation,
                first_sequence,
                last_sequence,
            })
        } else {
            self.publications_rejected = self.publications_rejected.saturating_add(1);
            Ok(ChartStreamPollOutcome::PublicationBackpressured {
                generation,
                first_sequence,
                last_sequence,
            })
        }
    }

    fn apply_recovery_snapshot(
        &mut self,
        request_id: u64,
        publication: MarketStreamPublication,
    ) -> Result<ChartStreamPollOutcome, ReplayValidationError> {
        let (_, update, publication_generation, _) = publication.into_parts();
        let generation = publication_generation.generation();
        let last_sequence = publication_generation.sequence_range().1;
        let installed = match (self.bridge.as_mut(), update) {
            (Some(bridge), ReplayStreamUpdate::Snapshot(snapshot)) => {
                bridge.install_recovery_snapshot(request_id, &snapshot)?
            }
            _ => false,
        };
        if installed {
            self.publication_generation = Some(generation);
            self.recovery_snapshots_installed = self.recovery_snapshots_installed.saturating_add(1);
        } else {
            self.recovery_snapshots_rejected = self.recovery_snapshots_rejected.saturating_add(1);
        }
        Ok(ChartStreamPollOutcome::RecoverySnapshot {
            request_id,
            installed,
            generation,
            last_sequence,
        })
    }

    fn record_dropped_events(&mut self, dropped_events: usize) {
        self.reported_dropped_events = self
            .reported_dropped_events
            .saturating_add(u64::try_from(dropped_events).unwrap_or(u64::MAX));
    }

    /// Latches a consumer-detected invalidation while preserving the last chart state.
    pub fn mark_stream_invalid(&mut self, reason: ResnapshotReason) -> bool {
        self.bridge.as_mut().is_some_and(|bridge| {
            bridge.mark_stream_invalid_for(reason);
            true
        })
    }

    /// Drains the chart bridge once, if an initial snapshot has installed it.
    ///
    /// # Errors
    ///
    /// Returns replay validation failures from the bounded chart bridge.
    pub fn drain_merged(&mut self) -> Result<Option<MergedChartData>, ReplayValidationError> {
        self.bridge
            .as_mut()
            .map_or(Ok(None), ChartDataBridge::drain_merged)
    }

    /// Returns the installed chart bridge, if the runtime supplied an initial snapshot.
    #[must_use]
    pub const fn bridge(&self) -> Option<&ChartDataBridge> {
        self.bridge.as_ref()
    }

    /// Returns bounded coordinator telemetry.
    #[must_use]
    pub fn metrics(&self) -> ChartStreamCoordinatorMetrics {
        ChartStreamCoordinatorMetrics {
            bridge_installed: self.bridge.is_some(),
            publication_generation: self.publication_generation,
            connected_events: self.connected_events,
            control_events: self.control_events,
            publications_accepted: self.publications_accepted,
            publications_rejected: self.publications_rejected,
            stream_invalidations: self.stream_invalidations,
            recovery_snapshots_installed: self.recovery_snapshots_installed,
            recovery_snapshots_rejected: self.recovery_snapshots_rejected,
            recovery_exhaustions: self.recovery_exhaustions,
            reported_dropped_events: self.reported_dropped_events,
            stopped: self.stopped,
        }
    }
}

fn snapshot_advances(
    snapshot: &ReplaySnapshot,
    current_generation: u64,
    current_last_sequence: u64,
) -> bool {
    snapshot.evidence().generation >= current_generation
        && snapshot.evidence().last_sequence > current_last_sequence
}

/// Headless result for the bounded UI recovery lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChartRecoveryConformance {
    passed_checks: u8,
}

impl ChartRecoveryConformance {
    const REQUIRED_CHECKS: u8 = u8::MAX;

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }
}

/// Exercises overflow recovery, correlation, integrity, and displayed provenance retention.
///
/// # Errors
///
/// Returns an error only if the embedded application fixture is invalid.
pub fn run_chart_bridge_recovery_conformance()
-> Result<ChartRecoveryConformance, ReplayValidationError> {
    let source = EmbeddedReplaySource;
    let baseline = source.load_snapshot(LoadEmbeddedReplay { bar_count: 1 })?;
    let recovered = source.load_snapshot(LoadEmbeddedReplay { bar_count: 3 })?;
    let mut bridge = ChartDataBridge::try_new(NonZeroUsize::MIN, &baseline)?;
    let delta_2 = source
        .load_delta(1)?
        .ok_or(StreamProtocolError::SequenceOverflow)?;
    let delta_3 = source
        .load_delta(2)?
        .ok_or(StreamProtocolError::SequenceOverflow)?;
    let first_push_ok = bridge.try_push(ReplayStreamUpdate::Delta(delta_2)).is_ok();
    let overflowed = bridge.try_push(ReplayStreamUpdate::Delta(delta_3)).is_err();
    let overflow_metrics = bridge.metrics();
    let command = bridge
        .pending_resnapshot_request()
        .ok_or(StreamProtocolError::EmptySnapshot)?;
    let rejected_dispatch = bridge.try_dispatch_recovery(|_| Err::<(), ()>(())).is_err();
    let remained_retryable = bridge.pending_resnapshot_request() == Some(command)
        && !bridge.metrics().recovery_dispatched;
    let accepted_dispatch = bridge
        .try_dispatch_recovery(|offered| if offered == command { Ok(()) } else { Err(()) })
        .is_ok_and(|dispatched| dispatched);
    let ordinary_recovery_snapshot_rejected = bridge
        .try_push(ReplayStreamUpdate::Snapshot(recovered.clone()))
        .is_ok()
        && bridge.drain_merged()?.is_some()
        && bridge.requires_snapshot();
    let stale_response_rejected =
        !bridge.install_recovery_snapshot(command.request_id.saturating_add(1), &recovered)?;
    let correlated_response_installed =
        bridge.install_recovery_snapshot(command.request_id, &recovered)?;
    let duplicate_snapshot_rejected = matches!(
        bridge.install_snapshot(&recovered),
        Err(ReplayValidationError::StaleSnapshot { .. })
    );
    let cross_generation_rollback_rejected =
        cross_generation_rollback_check(&baseline, &recovered)?;
    let recovered_metrics = bridge.metrics();

    let bounded_retry_cancellation = bounded_retry_cancellation_check(&baseline)?;

    let (corrupted_snapshot_rejected, mismatched_pair_rejected) =
        snapshot_integrity_checks(&recovered);

    let displayed = DisplayedProvenance::from_snapshot(&recovered);
    let older_and_latest_provenance_retained = displayed.get(1).is_some_and(|provenance| {
        provenance.source_sequence == 1
            && provenance.entitlement_revision == "embedded_fixture_entitlement_v1"
    }) && displayed
        .latest()
        .is_some_and(|provenance| provenance.source_sequence == 3);
    let evidence_retained = recovered.evidence().first_sequence == 1
        && recovered.evidence().last_sequence == 3
        && recovered.evidence().generation == 1
        && recovered.evidence().checksum != [0; 32];
    let bounded_retention = bounded_retention_check()?;

    let mut passed_checks = 0_u8;
    if first_push_ok
        && overflowed
        && overflow_metrics.snapshot_required
        && overflow_metrics.recovery_pending
    {
        passed_checks |= 0b0000_0001;
    }
    if rejected_dispatch && remained_retryable && bounded_retry_cancellation {
        passed_checks |= 0b0000_0010;
    }
    if accepted_dispatch && ordinary_recovery_snapshot_rejected && stale_response_rejected {
        passed_checks |= 0b0000_0100;
    }
    if correlated_response_installed
        && duplicate_snapshot_rejected
        && cross_generation_rollback_rejected
        && !recovered_metrics.snapshot_required
        && !recovered_metrics.recovery_pending
        && recovered_metrics.completed_recoveries == 1
        && recovered_metrics.rejected_uncorrelated_snapshots == 2
        && recovered_metrics.rejected_stale_snapshots == 1
    {
        passed_checks |= 0b0000_1000;
    }
    if corrupted_snapshot_rejected {
        passed_checks |= 0b0001_0000;
    }
    if mismatched_pair_rejected {
        passed_checks |= 0b0010_0000;
    }
    if older_and_latest_provenance_retained {
        passed_checks |= 0b0100_0000;
    }
    if evidence_retained && bounded_retention {
        passed_checks |= 0b1000_0000;
    }
    Ok(ChartRecoveryConformance { passed_checks })
}

fn cross_generation_rollback_check(
    baseline: &ReplaySnapshot,
    recovered: &ReplaySnapshot,
) -> Result<bool, ReplayValidationError> {
    let rollback =
        baseline.try_with_generation(recovered.evidence().generation.saturating_add(1))?;
    let mut ordinary = ChartDataBridge::try_new(NonZeroUsize::MIN, recovered)?;
    let ordinary_rejected = matches!(
        ordinary.install_snapshot(&rollback),
        Err(ReplayValidationError::StaleSnapshot { .. })
    );

    let mut correlated = ChartDataBridge::try_new(NonZeroUsize::MIN, recovered)?;
    correlated.mark_stream_invalid();
    let command = correlated
        .pending_resnapshot_request()
        .ok_or(StreamProtocolError::EmptySnapshot)?;
    let dispatched = correlated
        .try_dispatch_recovery(|offered| if offered == command { Ok(()) } else { Err(()) })
        .is_ok_and(|accepted| accepted);
    let correlated_rejected =
        !correlated.install_recovery_snapshot(command.request_id, &rollback)?;
    Ok(ordinary_rejected
        && dispatched
        && correlated_rejected
        && correlated.metrics().rejected_stale_snapshots == 1)
}

fn bounded_retry_cancellation_check(
    baseline: &ReplaySnapshot,
) -> Result<bool, ReplayValidationError> {
    let mut bridge = ChartDataBridge::try_new(NonZeroUsize::MIN, baseline)?;
    bridge.mark_stream_invalid();
    let command = bridge
        .pending_resnapshot_request()
        .ok_or(StreamProtocolError::EmptySnapshot)?;
    let mut bounded = true;
    for _ in 0..MAX_RECOVERY_DISPATCH_ATTEMPTS {
        bounded &= bridge
            .try_dispatch_recovery(|offered| if offered == command { Ok(()) } else { Err(()) })
            .is_ok_and(|dispatched| dispatched);
        bounded &= bridge.mark_recovery_failed(command.request_id);
    }
    let metrics = bridge.metrics();
    Ok(bounded
        && bridge.pending_resnapshot_request().is_none()
        && metrics.canceled_recoveries == 1
        && metrics.recovery_dispatch_attempts == 0)
}

fn bounded_retention_check() -> Result<bool, ReplayValidationError> {
    let replay = EmbeddedReplaySource.load_snapshot(LoadEmbeddedReplay { bar_count: 16 })?;
    let limit = NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN);
    let provenance = DisplayedProvenance::from_snapshot_with_limit(&replay, limit);
    let mut engine = ChartEngine::new(320.0, 200.0, 1.0);
    install_replay(&mut engine, &replay);
    Ok(engine.set_series_max_points(0, Some(limit.get()))
        && engine.series_max_points(0) == Some(limit.get())
        && provenance.len() == limit.get()
        && provenance.get(12).is_none()
        && provenance.get(13).is_some()
        && provenance
            .latest()
            .is_some_and(|item| item.source_sequence == 16))
}

fn snapshot_integrity_checks(recovered: &ReplaySnapshot) -> (bool, bool) {
    let mut corrupted_items = recovered.bars().to_vec();
    let (mut corrupted_bar, corrupted_provenance) = corrupted_items[0].clone().into_parts();
    corrupted_bar.volume = corrupted_bar.volume.saturating_add(1);
    corrupted_items[0] =
        axiusflow_application::Provenanced::new(corrupted_bar, corrupted_provenance);
    let corrupted_snapshot_rejected = matches!(
        ReplaySnapshot::try_new_provenanced(
            recovered.instrument().clone(),
            recovered.provenance(),
            recovered.bar_definition().clone(),
            recovered.evidence().clone(),
            corrupted_items,
        ),
        Err(ReplayValidationError::SnapshotChecksumMismatch)
    );
    let mut relabeled_definition = recovered.bar_definition().clone();
    relabeled_definition.definition_id.push_str(":relabeled");
    let relabeled_series_rejected = matches!(
        ReplaySnapshot::try_new_provenanced(
            recovered.instrument().clone(),
            recovered.provenance(),
            relabeled_definition,
            recovered.evidence().clone(),
            recovered.bars().to_vec(),
        ),
        Err(ReplayValidationError::SnapshotChecksumMismatch)
    );

    let mut mismatched_items = recovered.bars().to_vec();
    let (bar, mut mismatched_provenance) = mismatched_items[0].clone().into_parts();
    mismatched_provenance.source_sequence = mismatched_provenance.source_sequence.saturating_add(1);
    mismatched_items[0] = axiusflow_application::Provenanced::new(bar, mismatched_provenance);
    let sequence_mismatch_rejected = matches!(
        ReplaySnapshot::try_new_provenanced(
            recovered.instrument().clone(),
            recovered.provenance(),
            recovered.bar_definition().clone(),
            recovered.evidence().clone(),
            mismatched_items,
        ),
        Err(ReplayValidationError::ProvenanceSequenceMismatch { .. })
    );

    let mut timestamp_mismatched_items = recovered.bars().to_vec();
    let (bar, mut timestamp_mismatched_provenance) =
        timestamp_mismatched_items[0].clone().into_parts();
    timestamp_mismatched_provenance.exchange_timestamp_unix_nanos = timestamp_mismatched_provenance
        .exchange_timestamp_unix_nanos
        .saturating_add(1);
    timestamp_mismatched_items[0] =
        axiusflow_application::Provenanced::new(bar, timestamp_mismatched_provenance);
    let timestamp_mismatch_rejected = matches!(
        ReplaySnapshot::try_new_provenanced(
            recovered.instrument().clone(),
            recovered.provenance(),
            recovered.bar_definition().clone(),
            recovered.evidence().clone(),
            timestamp_mismatched_items,
        ),
        Err(ReplayValidationError::ProvenanceExchangeTimestampMismatch { .. })
    );
    (
        corrupted_snapshot_rejected && relabeled_series_rejected,
        sequence_mismatch_rejected && timestamp_mismatch_rejected,
    )
}

#[derive(Debug)]
struct DisplayedProvenance {
    by_source_sequence: BTreeMap<u64, MarketEventProvenance>,
    max_items: NonZeroUsize,
}

impl DisplayedProvenance {
    fn from_snapshot(snapshot: &ReplaySnapshot) -> Self {
        Self::from_snapshot_with_limit(
            snapshot,
            NonZeroUsize::new(DEFAULT_CHART_SERIES_MAX_POINTS).unwrap_or(NonZeroUsize::MIN),
        )
    }

    fn from_snapshot_with_limit(snapshot: &ReplaySnapshot, max_items: NonZeroUsize) -> Self {
        let mut history = Self {
            by_source_sequence: BTreeMap::new(),
            max_items,
        };
        history.replace_snapshot(snapshot);
        history
    }

    fn replace_snapshot(&mut self, snapshot: &ReplaySnapshot) {
        self.by_source_sequence.clear();
        self.extend(snapshot.bars());
    }

    fn extend(&mut self, items: &[ProvenancedMarketBar]) {
        self.by_source_sequence.extend(
            items
                .iter()
                .map(|item| (item.value().source_sequence, item.provenance().clone())),
        );
        while self.by_source_sequence.len() > self.max_items.get() {
            self.by_source_sequence.pop_first();
        }
    }

    fn len(&self) -> usize {
        self.by_source_sequence.len()
    }

    fn get(&self, source_sequence: u64) -> Option<&MarketEventProvenance> {
        self.by_source_sequence.get(&source_sequence)
    }

    fn latest(&self) -> Option<&MarketEventProvenance> {
        self.by_source_sequence
            .last_key_value()
            .map(|(_, provenance)| provenance)
    }
}

/// One deterministic replay-to-Origin-to-GPUI-host timing sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OriginGpuiHostSample {
    pub origin_frame_construction_nanos: u64,
    pub gpui_host_preparation_nanos: u64,
    pub origin_primitive_count: usize,
    pub origin_last_source_sequence: u64,
    pub gpui_plan_operations: u32,
    pub gpui_mesh_vertices: u32,
    pub submission_boundary_ready: bool,
    pub renderer_submission_performed: bool,
    pub physical_presentation_measured: bool,
}

/// Failures from the deterministic headless Origin/GPUI host boundary.
#[derive(Debug)]
pub enum OriginGpuiBenchmarkError {
    Replay(ReplayValidationError),
    QueueRejected,
    MissingMutation,
    Gpui(GpuiRenderError),
}

impl fmt::Display for OriginGpuiBenchmarkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Origin GPUI host benchmark failed: {self:?}")
    }
}

impl Error for OriginGpuiBenchmarkError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Replay(error) => Some(error),
            Self::Gpui(error) => Some(error),
            Self::QueueRejected | Self::MissingMutation => None,
        }
    }
}

/// Applies one decoded incremental update through Origin and prepares the real GPUI scene plan.
///
/// The client snapshot and update remain canonical fixed-point values until
/// `apply_merged_chart_data` converts them at the Origin boundary. Origin owns all chart state.
/// This headless path reaches the renderer-submission preparation boundary but deliberately does
/// not claim a GPUI window submission or a physically presented pixel.
///
/// # Errors
///
/// Returns replay validation, bounded queue, missing-mutation, or GPUI planning failures.
pub fn run_origin_gpui_host_sample(
    baseline: &ReplaySnapshot,
    update: ReplayStreamUpdate,
) -> Result<OriginGpuiHostSample, OriginGpuiBenchmarkError> {
    const WIDTH: f64 = 1_280.0;
    const HEIGHT: f64 = 720.0;
    const SCALE_FACTOR: f32 = 1.0;

    let mut engine = ChartEngine::new(WIDTH, HEIGHT, f64::from(SCALE_FACTOR));
    install_replay(&mut engine, baseline);
    engine.series[0].kind = SeriesKind::Candlestick;
    let mut price_divisor = replay_price_divisor(baseline);
    let mut bridge = ChartDataBridge::try_new(NonZeroUsize::MIN, baseline)
        .map_err(OriginGpuiBenchmarkError::Replay)?;
    bridge
        .try_push(update)
        .map_err(|_| OriginGpuiBenchmarkError::QueueRejected)?;

    let origin_started = Instant::now();
    let merged = bridge
        .drain_merged()
        .map_err(OriginGpuiBenchmarkError::Replay)?
        .filter(MergedChartData::mutates_series)
        .ok_or(OriginGpuiBenchmarkError::MissingMutation)?;
    let origin_last_source_sequence = merged.accepted_deltas().last().map_or_else(
        || {
            merged
                .snapshot()
                .map_or(baseline.evidence().last_sequence, |snapshot| {
                    snapshot.evidence().last_sequence
                })
        },
        |item| item.value().source_sequence,
    );
    apply_merged_chart_data(&mut engine, &mut price_divisor, &merged);
    engine.css_width = WIDTH;
    engine.css_height = HEIGHT;
    engine.dpr = f64::from(SCALE_FACTOR);
    let measure =
        |text: &str| u32::try_from(text.len()).map_or(f64::from(u32::MAX), f64::from) * 7.0;
    engine.recompute_layout_with_measure(true, measure);
    engine.fit_content();
    engine.recompute_layout_with_measure(true, measure);
    let layout = engine.options.get().layout;
    let max_label_width = (layout.font_size + 4.0) * 5.0 / 8.0
        * f64::from(engine.tick_mark_max_character_length.max(1));
    let axis_frame = engine.build_axis_frame(max_label_width, measure);
    let mut frame = ChartFrame::default();
    let mut axis_prims = Vec::new();
    engine.build_frame_into(&mut frame);
    engine.build_axis_primitives_into(&axis_frame, &mut axis_prims, |_| 0.0);
    let origin_frame_construction_nanos =
        u64::try_from(origin_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let origin_primitive_count = frame
        .panes
        .iter()
        .map(|pane| {
            pane.under
                .len()
                .saturating_add(pane.main.len())
                .saturating_add(pane.top_prims.len())
        })
        .sum::<usize>()
        .saturating_add(axis_prims.len());

    let host_started = Instant::now();
    let prepared = PreparedOriginFrame::new(&frame).with_axis(&axis_prims, &[]);
    let mut renderer = GpuiChartRenderer::new();
    let metrics = renderer
        .plan_frame(&prepared, SCALE_FACTOR)
        .map_err(OriginGpuiBenchmarkError::Gpui)?;
    let gpui_host_preparation_nanos =
        u64::try_from(host_started.elapsed().as_nanos()).unwrap_or(u64::MAX);

    Ok(OriginGpuiHostSample {
        origin_frame_construction_nanos,
        gpui_host_preparation_nanos,
        origin_primitive_count,
        origin_last_source_sequence,
        gpui_plan_operations: metrics.ops,
        gpui_mesh_vertices: metrics.mesh_vertices,
        submission_boundary_ready: metrics.prims > 0 && metrics.paint_nanos == 0,
        renderer_submission_performed: false,
        physical_presentation_measured: false,
    })
}

/// A GPUI entity hosting one authoritative Origin chart engine and renderer.
pub struct OriginChartView {
    engine: ChartEngine,
    renderer: GpuiChartRenderer,
    data_bridge: ChartDataBridge,
    displayed_provenance: DisplayedProvenance,
    price_divisor: f64,
    frame: ChartFrame,
    axis_prims: Vec<Prim>,
    theme: AxiusflowTheme,
    built_for: (f32, f32, f32),
    fitted: bool,
}

impl OriginChartView {
    /// Creates a chart from the bounded embedded replay and default theme.
    #[must_use]
    pub fn new() -> Self {
        Self::with_theme(AxiusflowTheme::default())
    }

    /// Creates a chart from the bounded embedded replay and a resolved theme.
    ///
    /// # Panics
    ///
    /// Panics only if the application-owned embedded fixture violates its own
    /// validation contract.
    #[must_use]
    pub fn with_theme(theme: AxiusflowTheme) -> Self {
        let replay = EmbeddedReplaySource
            .execute(LoadEmbeddedReplay { bar_count: 600 })
            .expect("the embedded replay is validated application data");
        Self::with_theme_and_replay(theme, &replay)
    }

    /// Creates a chart from one validated application replay snapshot.
    ///
    /// # Panics
    ///
    /// Panics only if the validated snapshot cannot establish resumable sequence state.
    #[must_use]
    pub fn with_theme_and_replay(theme: AxiusflowTheme, replay: &ReplaySnapshot) -> Self {
        let mut engine = ChartEngine::new(1024.0, 640.0, 1.0);
        apply_theme(&mut engine, &theme);
        install_replay(&mut engine, replay);
        let retention_applied =
            engine.set_series_max_points(0, Some(DEFAULT_CHART_SERIES_MAX_POINTS));
        debug_assert!(retention_applied);
        apply_series_theme(&mut engine, &theme);
        let data_bridge = ChartDataBridge::try_new(chart_data_queue_capacity(), replay)
            .expect("a validated replay snapshot establishes chart sequence state");

        Self {
            engine,
            renderer: GpuiChartRenderer::new(),
            data_bridge,
            displayed_provenance: DisplayedProvenance::from_snapshot(replay),
            price_divisor: replay_price_divisor(replay),
            frame: ChartFrame::default(),
            axis_prims: Vec::new(),
            theme,
            built_for: (0.0, 0.0, 0.0),
            fitted: false,
        }
    }

    /// Replaces Origin's authoritative series data with one validated snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot establish resumable sequence state.
    pub fn load_replay(&mut self, replay: &ReplaySnapshot) -> Result<(), ReplayValidationError> {
        self.data_bridge.install_snapshot(replay)?;
        install_replay(&mut self.engine, replay);
        self.displayed_provenance.replace_snapshot(replay);
        self.price_divisor = replay_price_divisor(replay);
        self.invalidate_series_frame();
        self.fitted = false;
        Ok(())
    }

    /// Enqueues one replay update for the next chart frame.
    ///
    /// # Errors
    ///
    /// Returns the unchanged update if the bounded bridge is full.
    pub fn try_queue_replay_update(
        &mut self,
        update: ReplayStreamUpdate,
    ) -> Result<(), Box<ReplayStreamUpdate>> {
        self.data_bridge.try_push(update)
    }

    /// Returns the number of replay commands waiting for the next frame.
    #[must_use]
    pub fn queued_replay_update_count(&self) -> usize {
        self.data_bridge.queued_update_count()
    }

    /// Returns whether a detected stream gap requires a replacement snapshot.
    #[must_use]
    pub fn replay_requires_snapshot(&self) -> bool {
        self.data_bridge.requires_snapshot()
    }

    /// Returns the next sequence expected by the chart bridge.
    #[must_use]
    pub fn expected_replay_sequence(&self) -> Option<u64> {
        self.data_bridge.expected_sequence()
    }

    /// Returns bounded queue and resnapshot telemetry for this chart subscription.
    #[must_use]
    pub fn replay_bridge_metrics(&self) -> ChartBridgeMetrics {
        self.data_bridge.metrics()
    }

    /// Peeks one retryable correlated recovery command without marking it dispatched.
    #[must_use]
    pub fn pending_replay_resnapshot_request(&self) -> Option<ReplayRecoveryCommand> {
        self.data_bridge.pending_resnapshot_request()
    }

    /// Offers recovery to a bounded worker queue and marks dispatch only after acceptance.
    ///
    /// # Errors
    ///
    /// Returns the worker queue's rejection without changing bridge dispatch state.
    pub fn try_dispatch_replay_recovery<DispatchError>(
        &mut self,
        dispatch: impl FnOnce(ReplayRecoveryCommand) -> Result<(), DispatchError>,
    ) -> Result<bool, DispatchError> {
        self.data_bridge.try_dispatch_recovery(dispatch)
    }

    /// Installs a response only when its request ID matches the active dispatched recovery.
    ///
    /// # Errors
    ///
    /// Returns an error if the correlated snapshot cannot establish resumable chart state.
    pub fn install_replay_recovery(
        &mut self,
        request_id: u64,
        replay: &ReplaySnapshot,
    ) -> Result<bool, ReplayValidationError> {
        if !self
            .data_bridge
            .install_recovery_snapshot(request_id, replay)?
        {
            return Ok(false);
        }
        install_replay(&mut self.engine, replay);
        self.displayed_provenance.replace_snapshot(replay);
        self.price_divisor = replay_price_divisor(replay);
        self.invalidate_series_frame();
        self.fitted = false;
        Ok(true)
    }

    /// Records failure only for the active correlated recovery request.
    pub fn mark_replay_recovery_failed(&mut self, request_id: u64) -> bool {
        self.data_bridge.mark_recovery_failed(request_id)
    }

    /// Blocks ordered chart updates and requests a correlated replacement snapshot.
    pub fn mark_replay_stream_invalid(&mut self) {
        self.data_bridge.mark_stream_invalid();
    }

    /// Returns canonical evidence for a displayed value by its source sequence.
    #[must_use]
    pub fn market_provenance(&self, source_sequence: u64) -> Option<&MarketEventProvenance> {
        self.displayed_provenance.get(source_sequence)
    }

    /// Returns canonical evidence for the latest value installed into Origin.
    #[must_use]
    pub fn latest_market_provenance(&self) -> Option<&MarketEventProvenance> {
        self.displayed_provenance.latest()
    }

    /// Applies a complete theme revision to Origin before the next frame.
    pub fn set_theme(&mut self, theme: AxiusflowTheme) {
        if self.theme == theme {
            return;
        }
        apply_theme(&mut self.engine, &theme);
        apply_series_theme(&mut self.engine, &theme);
        self.theme = theme;
        self.renderer.invalidate_caches();
        self.built_for = (0.0, 0.0, 0.0);
    }

    fn apply_pending_data(&mut self) {
        match self.data_bridge.drain_merged() {
            Ok(Some(update)) if update.mutates_series() => {
                let replaces_snapshot = update.snapshot().is_some();
                if let Some(snapshot) = update.snapshot() {
                    self.displayed_provenance.replace_snapshot(snapshot);
                }
                self.displayed_provenance.extend(update.accepted_deltas());
                apply_merged_chart_data(&mut self.engine, &mut self.price_divisor, &update);
                self.invalidate_series_frame();
                if replaces_snapshot {
                    self.fitted = false;
                }
            }
            Ok(_) => {}
            Err(error) => {
                self.data_bridge.mark_stream_invalid();
                eprintln!("replay update rejected; snapshot required: {error}");
            }
        }
    }

    fn invalidate_series_frame(&mut self) {
        self.frame = ChartFrame::default();
        self.axis_prims.clear();
        self.built_for = (0.0, 0.0, 0.0);
    }

    fn rebuild(&mut self, width: f32, height: f32, scale_factor: f32, window: &Window) {
        self.apply_pending_data();
        let dimensions = (width, height, scale_factor);
        if self.built_for == dimensions && !self.frame.panes.is_empty() {
            return;
        }

        if self.built_for.2 > 0.0 && (self.built_for.2 - scale_factor).abs() > SCALE_FACTOR_EPSILON
        {
            self.renderer.invalidate_caches();
        }
        self.built_for = dimensions;
        self.engine.css_width = f64::from(width);
        self.engine.css_height = f64::from(height);
        self.engine.dpr = f64::from(scale_factor);

        let layout = self.engine.options.get().layout;
        let font_size = layout.font_size.to_f32().unwrap_or(12.0);
        let measure = |text: &str| {
            f64::from(measure_text(window, text, &layout.font_family, font_size, 400, false).width)
        };

        self.engine.recompute_layout_with_measure(true, measure);
        if !self.fitted {
            self.engine.fit_content();
            self.fitted = true;
            self.engine.recompute_layout_with_measure(true, measure);
        }

        let max_label_width = (layout.font_size + 4.0) * 5.0 / 8.0
            * f64::from(self.engine.tick_mark_max_character_length.max(1));
        let axis_frame = self.engine.build_axis_frame(max_label_width, measure);
        self.engine.build_frame_into(&mut self.frame);
        self.engine
            .build_axis_primitives_into(&axis_frame, &mut self.axis_prims, |_| 0.0);
    }

    fn paint(&mut self, bounds: Bounds<gpui::Pixels>, window: &mut Window, cx: &mut App) {
        let viewport = OriginViewport::from_bounds(
            bounds.origin.x.into(),
            bounds.origin.y.into(),
            bounds.size.width.into(),
            bounds.size.height.into(),
        );
        let prepared = PreparedOriginFrame::new(&self.frame).with_axis(&self.axis_prims, &[]);
        if let Err(error) =
            self.renderer
                .paint_frame(&prepared, viewport, window.scale_factor(), window, cx)
        {
            eprintln!("origin frame skipped: {error}");
        }
    }
}

impl Default for OriginChartView {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for OriginChartView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity: Entity<Self> = cx.entity();
        let prepaint_entity = entity.clone();

        div()
            .size_full()
            .bg(rgb(self.theme.colors.background.rgb_u32()))
            .child(
                canvas(
                    move |bounds: Bounds<gpui::Pixels>, window, cx| {
                        let width = bounds.size.width.into();
                        let height = bounds.size.height.into();
                        let scale_factor = window.scale_factor();
                        prepaint_entity.update(cx, |chart, _| {
                            chart.rebuild(width, height, scale_factor, window);
                        });
                        bounds
                    },
                    move |_bounds: Bounds<gpui::Pixels>, prepainted, window, cx| {
                        entity.update(cx, |chart, cx| {
                            chart.paint(prepainted, window, cx);
                        });
                    },
                )
                .size_full(),
            )
    }
}

fn apply_theme(engine: &mut ChartEngine, theme: &AxiusflowTheme) {
    let background = css_color(theme.colors.background);
    let axis_text = css_color(theme.colors.chart_axis_text);
    let border = css_color(theme.colors.border);
    let options = format!(
        r#"{{
  "layout":{{"background":{{"type":"solid","color":"{background}"}},"textColor":"{axis_text}","panes":{{"separatorColor":"{border}"}}}},
  "leftPriceScale":{{"borderColor":"{border}"}},"rightPriceScale":{{"borderColor":"{border}"}},
  "timeScale":{{"borderColor":"{border}"}},
  "grid":{{"vertLines":{{"color":"{border}"}},"horzLines":{{"color":"{border}"}}}}
}}"#
    );
    engine
        .options
        .apply_str(&options)
        .expect("generated Axiusflow Origin theme options are valid");
}

fn apply_series_theme(engine: &mut ChartEngine, theme: &AxiusflowTheme) {
    let series = &mut engine.series[0];
    let candle_up = css_color(theme.colors.chart_candle_up);
    let candle_down = css_color(theme.colors.chart_candle_down);
    series.line_color = Some(css_color(theme.colors.chart_palette[0]));
    series.up_color = Some(candle_up.clone());
    series.down_color = Some(candle_down.clone());
    series.wick_up_color = Some(candle_up.clone());
    series.wick_down_color = Some(candle_down.clone());
    series.border_up_color = Some(candle_up);
    series.border_down_color = Some(candle_down);
}

fn css_color(color: ThemeColor) -> String {
    color.css_value()
}

fn chart_data_queue_capacity() -> NonZeroUsize {
    NonZeroUsize::new(DEFAULT_CHART_DATA_QUEUE_CAPACITY).unwrap_or(NonZeroUsize::MIN)
}

fn replay_price_divisor(replay: &ReplaySnapshot) -> f64 {
    10_f64.powi(i32::from(replay.instrument().precision.price_scale()))
}

fn apply_merged_chart_data(
    engine: &mut ChartEngine,
    price_divisor: &mut f64,
    update: &MergedChartData,
) {
    if let Some(snapshot) = update.snapshot() {
        *price_divisor = replay_price_divisor(snapshot);
        install_replay_with_deltas(engine, snapshot, update.accepted_deltas());
        return;
    }

    let rows = update.accepted_deltas().iter().map(|item| {
        let bar = *item.value();
        (
            bar.exchange_timestamp_seconds
                .to_f64()
                .expect("validated replay timestamps fit f64"),
            [
                fixed_price(bar.open, *price_divisor),
                fixed_price(bar.high, *price_divisor),
                fixed_price(bar.low, *price_divisor),
                fixed_price(bar.close, *price_divisor),
            ],
        )
    });
    let accepted = engine.update_series_bars(0, rows);
    debug_assert_eq!(accepted, update.accepted_deltas().len());
}

fn install_replay(engine: &mut ChartEngine, replay: &ReplaySnapshot) {
    install_replay_with_deltas(engine, replay, &[]);
}

fn install_replay_with_deltas(
    engine: &mut ChartEngine,
    replay: &ReplaySnapshot,
    deltas: &[ProvenancedMarketBar],
) {
    let item_count = replay.bars().len().saturating_add(deltas.len());
    let mut times = Vec::with_capacity(item_count);
    let mut open = Vec::with_capacity(item_count);
    let mut high = Vec::with_capacity(item_count);
    let mut low = Vec::with_capacity(item_count);
    let mut close = Vec::with_capacity(item_count);
    let price_divisor = replay_price_divisor(replay);

    for item in replay.bars().iter().chain(deltas) {
        let bar = *item.value();
        times.push(
            bar.exchange_timestamp_seconds
                .to_f64()
                .expect("validated replay timestamps fit f64"),
        );
        open.push(fixed_price(bar.open, price_divisor));
        high.push(fixed_price(bar.high, price_divisor));
        low.push(fixed_price(bar.low, price_divisor));
        close.push(fixed_price(bar.close, price_divisor));
    }

    engine
        .set_series_data(0, &times, &open, &high, &low, &close)
        .expect("validated replay columns satisfy Origin's data contract");
    engine.series[0].kind = SeriesKind::Candlestick;
}

fn fixed_price(value: i64, divisor: f64) -> f64 {
    value
        .to_f64()
        .expect("validated fixed-point price fits f64")
        / divisor
}
