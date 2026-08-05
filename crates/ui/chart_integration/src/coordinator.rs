//! Bounded coordinator translating runtime-port events into chart transitions.

use crate::bridge::{
    ChartDataBridge, MergedChartData, RebaselineSnapshotQueueOutcome, ReplayRecoveryCommand,
};
use axiusflow_application::{
    MarketStreamCommand, MarketStreamCommandOffer, MarketStreamControlSignal, MarketStreamEvent,
    MarketStreamPublication, MarketStreamRuntimePort, ReplayStreamUpdate, ReplayValidationError,
    ResnapshotReason,
};
use core::fmt;
use std::{error::Error, num::NonZeroUsize};

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
    PublicationSubscriptionMismatch {
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
    PublicationRejectedAwaitingRecovery {
        generation: u64,
        first_sequence: u64,
        last_sequence: u64,
    },
    PublicationSnapshotSuperseded {
        generation: u64,
        first_sequence: u64,
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
    subscription_id: Option<String>,
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
            subscription_id: None,
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
                let canceled = self.bridge.as_mut().is_some_and(|bridge| match request_id {
                    Some(request_id) => bridge.cancel_recovery(request_id),
                    None => bridge.cancel_pending_recovery_for(reason),
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
        let (subscription_id, update, publication_generation, predecessor_generation) =
            publication.into_parts();
        let generation = publication_generation.generation();
        let (first_sequence, last_sequence) = publication_generation.sequence_range();
        if let Some(outcome) = self.reject_subscription_mismatch(
            &subscription_id,
            generation,
            first_sequence,
            last_sequence,
        ) {
            return Ok(outcome);
        }
        let Some(bridge) = self.bridge.as_mut() else {
            return self.apply_initial_publication(
                subscription_id,
                update,
                generation,
                first_sequence,
                last_sequence,
            );
        };
        let update_is_delta = matches!(&update, ReplayStreamUpdate::Delta(_));
        let snapshot_required = bridge.requires_snapshot();
        let recovery_pending = bridge.metrics().recovery_pending;
        if snapshot_required
            && (recovery_pending || (update_is_delta && !bridge.has_queued_rebaseline_snapshot()))
        {
            self.publications_rejected = self.publications_rejected.saturating_add(1);
            return Ok(
                ChartStreamPollOutcome::PublicationRejectedAwaitingRecovery {
                    generation,
                    first_sequence,
                    last_sequence,
                },
            );
        }
        if snapshot_required && !update_is_delta {
            let (outcome, selected_generation) =
                queue_required_snapshot(bridge, update, generation, first_sequence, last_sequence);
            if let Some(selected_generation) = selected_generation {
                self.publication_generation = Some(selected_generation);
                self.publications_accepted = self.publications_accepted.saturating_add(1);
            } else {
                self.publications_rejected = self.publications_rejected.saturating_add(1);
            }
            return Ok(outcome);
        }
        if let Some(outcome) = reject_mismatched_rebaseline_delta(
            bridge,
            &update,
            generation,
            first_sequence,
            last_sequence,
        ) {
            self.publications_rejected = self.publications_rejected.saturating_add(1);
            return Ok(outcome);
        }
        if let Some(outcome) = publication_generation_discontinuity(
            update_is_delta,
            predecessor_generation,
            self.publication_generation,
            generation,
            last_sequence,
        ) {
            self.publications_rejected = self.publications_rejected.saturating_add(1);
            self.stream_invalidations = self.stream_invalidations.saturating_add(1);
            bridge.mark_stream_invalid_for(ResnapshotReason::SequenceGap);
            return Ok(outcome);
        }
        if bridge.try_push_publication(update, generation).is_ok() {
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
        let (subscription_id, update, publication_generation, _) = publication.into_parts();
        let generation = publication_generation.generation();
        let last_sequence = publication_generation.sequence_range().1;
        let installed = match (
            self.subscription_id.as_deref() == Some(subscription_id.as_str()),
            self.bridge.as_mut(),
            update,
        ) {
            (true, Some(bridge), ReplayStreamUpdate::Snapshot(snapshot)) => {
                bridge.install_recovery_snapshot(request_id, &snapshot)?
            }
            _ => false,
        };
        if !installed {
            let _ = self
                .bridge
                .as_mut()
                .is_some_and(|bridge| bridge.mark_recovery_failed(request_id));
        }
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

    fn reject_subscription_mismatch(
        &mut self,
        subscription_id: &str,
        generation: u64,
        first_sequence: u64,
        last_sequence: u64,
    ) -> Option<ChartStreamPollOutcome> {
        if self.bridge.is_none() || self.subscription_id.as_deref() == Some(subscription_id) {
            return None;
        }
        self.publications_rejected = self.publications_rejected.saturating_add(1);
        Some(ChartStreamPollOutcome::PublicationSubscriptionMismatch {
            generation,
            first_sequence,
            last_sequence,
        })
    }

    fn apply_initial_publication(
        &mut self,
        subscription_id: String,
        update: ReplayStreamUpdate,
        generation: u64,
        first_sequence: u64,
        last_sequence: u64,
    ) -> Result<ChartStreamPollOutcome, ReplayValidationError> {
        match update {
            ReplayStreamUpdate::Snapshot(snapshot) => {
                self.bridge = Some(ChartDataBridge::try_new(self.bridge_capacity, &snapshot)?);
                self.subscription_id = Some(subscription_id);
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
        }
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
        let Some(bridge) = self.bridge.as_mut() else {
            return Ok(None);
        };
        let merged = bridge.drain_merged()?;
        if merged.is_some() {
            self.publication_generation = Some(bridge.accepted_publication_generation());
        }
        Ok(merged)
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

fn reject_mismatched_rebaseline_delta(
    bridge: &ChartDataBridge,
    update: &ReplayStreamUpdate,
    generation: u64,
    first_sequence: u64,
    last_sequence: u64,
) -> Option<ChartStreamPollOutcome> {
    let ReplayStreamUpdate::Delta(delta) = update else {
        return None;
    };
    (!bridge.delta_matches_queued_rebaseline(delta)).then_some(
        ChartStreamPollOutcome::PublicationRejectedAwaitingRecovery {
            generation,
            first_sequence,
            last_sequence,
        },
    )
}

fn queue_required_snapshot(
    bridge: &mut ChartDataBridge,
    update: ReplayStreamUpdate,
    generation: u64,
    first_sequence: u64,
    last_sequence: u64,
) -> (ChartStreamPollOutcome, Option<u64>) {
    let ReplayStreamUpdate::Snapshot(snapshot) = update else {
        return (
            ChartStreamPollOutcome::PublicationRejectedAwaitingRecovery {
                generation,
                first_sequence,
                last_sequence,
            },
            None,
        );
    };
    match bridge.queue_rebaseline_snapshot(snapshot, generation) {
        RebaselineSnapshotQueueOutcome::Selected {
            publication_generation,
        } => (
            ChartStreamPollOutcome::PublicationQueued {
                generation,
                first_sequence,
                last_sequence,
            },
            Some(publication_generation),
        ),
        RebaselineSnapshotQueueOutcome::Superseded => (
            ChartStreamPollOutcome::PublicationSnapshotSuperseded {
                generation,
                first_sequence,
                last_sequence,
            },
            None,
        ),
    }
}

fn publication_generation_discontinuity(
    update_is_delta: bool,
    actual_predecessor: Option<u64>,
    expected_predecessor: Option<u64>,
    generation: u64,
    last_sequence: u64,
) -> Option<ChartStreamPollOutcome> {
    (update_is_delta && actual_predecessor != expected_predecessor).then_some(
        ChartStreamPollOutcome::PublicationGenerationDiscontinuity {
            expected_predecessor,
            actual_predecessor,
            generation,
            last_sequence,
        },
    )
}
