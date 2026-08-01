//! Bounded event branches with declared overflow semantics.

use crate::canonical_event::{CanonicalMarketEvent, SemanticClass};
use crate::errors::RealtimeError;
use crate::partition::{PublicationFence, verify_publication_identity};
use crate::snapshot::CanonicalSnapshot;
use std::collections::VecDeque;
use std::num::{NonZeroU64, NonZeroUsize};

/// Semantic action taken when a queue reaches its declared bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverflowAction {
    ConflateNewest,
    RequestSnapshot,
    RejectBeforeAcceptance,
    ShedOptionalWork,
    DisconnectSlowConsumer,
}

/// Complete declaration required for every bounded queue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuePolicy {
    pub name: String,
    pub producer: String,
    pub consumer: String,
    pub item_capacity: NonZeroUsize,
    pub byte_capacity: NonZeroUsize,
    pub semantic_class: SemanticClass,
    pub overflow_action: OverflowAction,
    pub maximum_residence_nanos: NonZeroU64,
    pub recovery: String,
    pub alert_threshold_items: NonZeroUsize,
}

impl QueuePolicy {
    /// Validates queue identity and semantic overflow compatibility.
    ///
    /// # Errors
    ///
    /// Returns an error for empty metadata, threshold overflow, or an unsafe action.
    pub fn validate(&self) -> Result<(), RealtimeError> {
        for (field, value) in [
            ("queue.name", self.name.as_str()),
            ("queue.producer", self.producer.as_str()),
            ("queue.consumer", self.consumer.as_str()),
            ("queue.recovery", self.recovery.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(RealtimeError::EmptyIdentity(field));
            }
        }
        if self.alert_threshold_items > self.item_capacity {
            return Err(RealtimeError::AlertThresholdExceedsCapacity);
        }
        let valid_action = matches!(
            (self.semantic_class, self.overflow_action),
            (SemanticClass::StateReplace, OverflowAction::ConflateNewest)
                | (SemanticClass::OrderedDelta, OverflowAction::RequestSnapshot)
                | (
                    SemanticClass::ReliableEvent
                        | SemanticClass::AuthoritativeEvent
                        | SemanticClass::Snapshot,
                    OverflowAction::RejectBeforeAcceptance | OverflowAction::DisconnectSlowConsumer
                )
        );
        if !valid_action {
            return Err(RealtimeError::IncompatibleOverflowAction);
        }
        Ok(())
    }
}

/// Result of one bounded branch enqueue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueOutcome {
    Enqueued,
    Conflated { removed: usize },
    SnapshotRequired,
    Rejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueueGap {
    pub expected: u64,
    pub actual: u64,
}

#[derive(Clone, Debug)]
struct QueuedEvent {
    enqueued_unix_nanos: i64,
    event: CanonicalMarketEvent,
}

/// Bounded queue carrying one declared semantic class.
#[derive(Clone, Debug)]
pub struct BoundedEventBranch {
    policy: QueuePolicy,
    items: VecDeque<QueuedEvent>,
    bytes: usize,
    overflows: u64,
    snapshot_required: bool,
    last_gap: Option<QueueGap>,
}

impl BoundedEventBranch {
    /// Creates a queue from a complete validated policy.
    ///
    /// # Errors
    ///
    /// Returns an error when policy metadata or semantics are invalid.
    pub fn try_new(policy: QueuePolicy) -> Result<Self, RealtimeError> {
        policy.validate()?;
        Ok(Self {
            items: VecDeque::with_capacity(policy.item_capacity.get()),
            policy,
            bytes: 0,
            overflows: 0,
            snapshot_required: false,
            last_gap: None,
        })
    }

    /// Enqueues without ever growing beyond item or byte bounds.
    ///
    /// # Errors
    ///
    /// Returns an error when event semantics do not match this queue.
    pub fn push(
        &mut self,
        event: CanonicalMarketEvent,
        enqueued_unix_nanos: i64,
    ) -> Result<QueueOutcome, RealtimeError> {
        if event.header().semantic_class != self.policy.semantic_class {
            return Err(RealtimeError::SemanticClassMismatch);
        }
        if self.snapshot_required {
            return Ok(QueueOutcome::SnapshotRequired);
        }
        let event_bytes = event.encoded_size_bytes();
        let would_overflow = self.items.len() == self.policy.item_capacity.get()
            || self.bytes.saturating_add(event_bytes) > self.policy.byte_capacity.get();
        if !would_overflow {
            self.bytes = self.bytes.saturating_add(event_bytes);
            self.items.push_back(QueuedEvent {
                enqueued_unix_nanos,
                event,
            });
            return Ok(QueueOutcome::Enqueued);
        }

        self.overflows = self.overflows.saturating_add(1);
        match self.policy.overflow_action {
            OverflowAction::ConflateNewest => {
                let removed = self.items.len();
                self.items.clear();
                self.bytes = 0;
                if event_bytes > self.policy.byte_capacity.get() {
                    return Ok(QueueOutcome::Rejected);
                }
                self.bytes = event_bytes;
                self.items.push_back(QueuedEvent {
                    enqueued_unix_nanos,
                    event,
                });
                Ok(QueueOutcome::Conflated { removed })
            }
            OverflowAction::RequestSnapshot => {
                let expected = self
                    .items
                    .back()
                    .and_then(|queued| queued.event.header().source_sequence.checked_add(1))
                    .unwrap_or(event.header().source_sequence);
                self.last_gap = Some(QueueGap {
                    expected,
                    actual: event.header().source_sequence,
                });
                self.snapshot_required = true;
                Ok(QueueOutcome::SnapshotRequired)
            }
            OverflowAction::RejectBeforeAcceptance
            | OverflowAction::ShedOptionalWork
            | OverflowAction::DisconnectSlowConsumer => Ok(QueueOutcome::Rejected),
        }
    }

    #[must_use]
    pub fn pop(&mut self, now_unix_nanos: i64) -> Option<(CanonicalMarketEvent, u64)> {
        if self.snapshot_required {
            return None;
        }
        let queued = self.items.pop_front()?;
        self.bytes = self.bytes.saturating_sub(queued.event.encoded_size_bytes());
        let residence = now_unix_nanos.saturating_sub(queued.enqueued_unix_nanos);
        Some((queued.event, u64::try_from(residence).unwrap_or(0)))
    }

    #[must_use]
    pub const fn policy(&self) -> &QueuePolicy {
        &self.policy
    }

    pub(crate) fn require_snapshot(&mut self, actual_sequence: u64) {
        let expected = self
            .items
            .back()
            .and_then(|queued| queued.event.header().source_sequence.checked_add(1))
            .unwrap_or(actual_sequence);
        self.last_gap = Some(QueueGap {
            expected,
            actual: actual_sequence,
        });
        self.snapshot_required = true;
    }

    /// Atomically clears obsolete queued deltas and removes the gap latch after
    /// a verified replacement snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid checksum or mismatched fence identity.
    pub fn recover_from_snapshot(
        &mut self,
        fence: &PublicationFence,
        snapshot: &CanonicalSnapshot,
    ) -> Result<(), RealtimeError> {
        let descriptor = snapshot.descriptor();
        verify_publication_identity(
            fence,
            descriptor.partition_id,
            descriptor.ownership_epoch.get(),
        )?;
        if self.snapshot_required {
            self.items.clear();
            self.bytes = 0;
            self.snapshot_required = false;
            self.last_gap = None;
        }
        Ok(())
    }

    pub(crate) fn fence_handoff(&mut self) {
        self.items.clear();
        self.bytes = 0;
        self.last_gap = None;
        self.snapshot_required = self.policy.semantic_class == SemanticClass::OrderedDelta;
    }

    #[must_use]
    pub fn metrics(&self, now_unix_nanos: i64) -> QueueMetrics {
        let oldest_item_age_nanos = self.items.front().map_or(0, |queued| {
            u64::try_from(now_unix_nanos.saturating_sub(queued.enqueued_unix_nanos)).unwrap_or(0)
        });
        QueueMetrics {
            items: self.items.len(),
            bytes: self.bytes,
            overflows: self.overflows,
            snapshot_required: self.snapshot_required,
            oldest_item_age_nanos,
            residence_slo_exceeded: oldest_item_age_nanos
                > self.policy.maximum_residence_nanos.get(),
            last_gap: self.last_gap,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueueMetrics {
    pub items: usize,
    pub bytes: usize,
    pub overflows: u64,
    pub snapshot_required: bool,
    pub oldest_item_age_nanos: u64,
    pub residence_slo_exceeded: bool,
    pub last_gap: Option<QueueGap>,
}

#[cfg(test)]
mod tests {
    use super::{BoundedEventBranch, OverflowAction, QueueOutcome};
    use crate::canonical_event::SemanticClass;
    use crate::test_fixture::{event, policy};

    #[test]
    fn bounded_queue_applies_declared_overflow_semantics_without_growth() {
        let mut deltas = BoundedEventBranch::try_new(policy(
            SemanticClass::OrderedDelta,
            OverflowAction::RequestSnapshot,
            1,
        ))
        .expect("ordered-delta policy is valid");
        assert_eq!(
            deltas
                .push(event(1, 1, SemanticClass::OrderedDelta), 1_000)
                .expect("first delta enqueues"),
            QueueOutcome::Enqueued
        );
        assert_eq!(
            deltas
                .push(event(2, 1, SemanticClass::OrderedDelta), 2_000)
                .expect("overflow is classified"),
            QueueOutcome::SnapshotRequired
        );
        let metrics = deltas.metrics(2_100);
        assert_eq!(metrics.items, 1);
        assert_eq!(metrics.overflows, 1);
        assert!(metrics.snapshot_required);
        assert!(deltas.pop(2_100).is_none());

        let mut latest = BoundedEventBranch::try_new(policy(
            SemanticClass::StateReplace,
            OverflowAction::ConflateNewest,
            1,
        ))
        .expect("state-replace policy is valid");
        latest
            .push(event(1, 1, SemanticClass::StateReplace), 1_000)
            .expect("first state enqueues");
        assert_eq!(
            latest
                .push(event(2, 1, SemanticClass::StateReplace), 2_000)
                .expect("new state conflates"),
            QueueOutcome::Conflated { removed: 1 }
        );
        let (retained, _) = latest.pop(2_050).expect("latest state is retained");
        assert_eq!(retained.header().source_sequence, 2);
    }
}
