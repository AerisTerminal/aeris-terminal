//! Independently bounded direct fanout and durable tap.

use crate::bounded_queue::{BoundedEventBranch, QueueMetrics, QueueOutcome};
use crate::canonical_event::CanonicalMarketEvent;
use crate::errors::RealtimeError;
use crate::partition::{
    AcceptedCanonicalEvent, AcceptedDirectEvent, FencedPartition, PublicationFence,
    verify_publication_identity,
};
use crate::snapshot::CanonicalSnapshot;

/// Independent direct and durable outcomes for one canonical event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FanoutOutcome {
    pub direct: QueueOutcome,
    pub durable: QueueOutcome,
}

/// Direct fanout and durable tap are bounded independently; neither waits for the other.
#[derive(Clone, Debug)]
pub struct DirectDurableFanout {
    active_fence: PublicationFence,
    direct: BoundedEventBranch,
    durable: BoundedEventBranch,
}

impl DirectDurableFanout {
    /// Creates a pair only when both branches accept the same semantic class.
    ///
    /// # Errors
    ///
    /// Returns an error for incompatible branch policies.
    pub fn try_new(
        active_fence: PublicationFence,
        direct: BoundedEventBranch,
        durable: BoundedEventBranch,
    ) -> Result<Self, RealtimeError> {
        if direct.policy().semantic_class != durable.policy().semantic_class {
            return Err(RealtimeError::FanoutPolicyMismatch);
        }
        Ok(Self {
            active_fence,
            direct,
            durable,
        })
    }

    /// Fences both branches before a new owner can publish.
    ///
    /// # Errors
    ///
    /// Returns an error unless partition identity is stable and epoch increases.
    pub fn activate_fence(&mut self, next: PublicationFence) -> Result<(), RealtimeError> {
        if next.authority_id != self.active_fence.authority_id {
            return Err(RealtimeError::StalePublicationFence);
        }
        if next.partition_id != self.active_fence.partition_id {
            return Err(RealtimeError::PartitionMismatch {
                expected: self.active_fence.partition_id,
                actual: next.partition_id,
            });
        }
        if next.ownership_epoch <= self.active_fence.ownership_epoch {
            return Err(RealtimeError::StaleOwnershipEpoch {
                active: self.active_fence.ownership_epoch.get(),
                attempted: next.ownership_epoch.get(),
            });
        }
        self.active_fence = next;
        self.direct.fence_handoff();
        self.durable.fence_handoff();
        Ok(())
    }

    /// Publishes under the active fence and always returns both branch outcomes.
    ///
    /// Branch capacity/recovery outcomes remain independent. The partition's current
    /// ownership, fanout fence, event identity, enqueue timestamp, and semantic
    /// compatibility are validated before either queue mutates.
    ///
    /// # Errors
    ///
    /// Returns an error for stale ownership or invalid accepted-event metadata.
    /// Failures discovered after active authority verification latch partition and
    /// branch recovery so a consumed ordered capability cannot create silent loss.
    pub fn publish(
        &mut self,
        partition: &mut FencedPartition,
        fence: &PublicationFence,
        accepted: AcceptedCanonicalEvent,
        enqueued_unix_nanos: i64,
    ) -> Result<FanoutOutcome, RealtimeError> {
        if accepted.authority_id != fence.authority_id {
            return Err(RealtimeError::StalePublicationFence);
        }
        let event = accepted.event;
        partition.verify_fence(
            fence,
            event.header().partition_id,
            event.header().ownership_epoch,
        )?;
        self.verify_active_fence(fence, &event)?;
        let sequence = event.header().source_sequence;
        let declared_enqueue = event.header().timestamps.fanout_enqueue_unix_nanos;
        if declared_enqueue != Some(enqueued_unix_nanos) {
            self.latch_post_acceptance_failure(partition, sequence);
            return Err(RealtimeError::FanoutEnqueueTimestampMismatch {
                declared: declared_enqueue,
                actual: enqueued_unix_nanos,
            });
        }
        if event.header().semantic_class != self.direct.policy().semantic_class {
            self.latch_post_acceptance_failure(partition, sequence);
            return Err(RealtimeError::SemanticClassMismatch);
        }
        let direct = match self.direct.push(event.clone(), enqueued_unix_nanos) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.latch_post_acceptance_failure(partition, sequence);
                return Err(error);
            }
        };
        let durable = match self.durable.push(event, enqueued_unix_nanos) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.latch_post_acceptance_failure(partition, sequence);
                return Err(error);
            }
        };
        Ok(FanoutOutcome { direct, durable })
    }

    fn latch_post_acceptance_failure(&mut self, partition: &mut FencedPartition, sequence: u64) {
        partition.require_snapshot();
        self.direct.require_snapshot(sequence);
        self.durable.require_snapshot(sequence);
    }

    /// Clears ordered-loss latches on both branches after the same verified snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error for stale ownership, invalid range, schema, or checksum.
    pub fn recover_from_snapshot(
        &mut self,
        fence: &PublicationFence,
        snapshot: &CanonicalSnapshot,
    ) -> Result<(), RealtimeError> {
        self.verify_fence_value(fence)?;
        self.direct.recover_from_snapshot(fence, snapshot)?;
        self.durable.recover_from_snapshot(fence, snapshot)?;
        Ok(())
    }

    #[must_use]
    pub fn pop_direct(&mut self, now_unix_nanos: i64) -> Option<(AcceptedDirectEvent, u64)> {
        let (event, residence) = self.direct.pop(now_unix_nanos)?;
        Some((
            AcceptedDirectEvent {
                authority_id: self.active_fence.authority_id,
                event,
            },
            residence,
        ))
    }

    #[must_use]
    pub fn pop_durable(&mut self, now_unix_nanos: i64) -> Option<(CanonicalMarketEvent, u64)> {
        self.durable.pop(now_unix_nanos)
    }

    #[must_use]
    pub fn direct_metrics(&self, now_unix_nanos: i64) -> QueueMetrics {
        self.direct.metrics(now_unix_nanos)
    }

    #[must_use]
    pub fn durable_metrics(&self, now_unix_nanos: i64) -> QueueMetrics {
        self.durable.metrics(now_unix_nanos)
    }

    fn verify_active_fence(
        &self,
        fence: &PublicationFence,
        event: &CanonicalMarketEvent,
    ) -> Result<(), RealtimeError> {
        self.verify_fence_value(fence)?;
        verify_publication_identity(
            fence,
            event.header().partition_id,
            event.header().ownership_epoch,
        )
    }

    fn verify_fence_value(&self, fence: &PublicationFence) -> Result<(), RealtimeError> {
        if fence != &self.active_fence {
            return Err(RealtimeError::StalePublicationFence);
        }
        Ok(())
    }
}
