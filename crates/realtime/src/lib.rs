//! Canonical market events, fenced partition ownership, and bounded fanout contracts.

use core::fmt;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::error::Error;
use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_PARTITION_AUTHORITY_ID: AtomicU64 = AtomicU64::new(1);

pub const MAX_CANONICAL_PAYLOAD_BYTES: usize = 4_096;

/// Delivery and recovery semantics for one event stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticClass {
    StateReplace,
    OrderedDelta,
    ReliableEvent,
    AuthoritativeEvent,
    Snapshot,
}

/// Clock source retained for an optional NIC timestamp.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NicTimestampSource {
    SocketSoftware,
    KernelSoftware,
    NicHardware,
}

/// Complete Stage 1 canonical market timestamp vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanonicalTimestamps {
    pub exchange_unix_nanos: i64,
    pub provider_receive_unix_nanos: i64,
    pub nic_receive_unix_nanos: Option<i64>,
    pub axiusflow_receive_unix_nanos: i64,
    pub normalized_unix_nanos: i64,
    pub fanout_enqueue_unix_nanos: Option<i64>,
}

impl CanonicalTimestamps {
    /// Validates only boundaries known to share Axiusflow's measured clock.
    /// External provider/exchange clocks remain provenance, not subtraction input.
    ///
    /// # Errors
    ///
    /// Returns an error when normalize or fanout precedes local receipt.
    pub fn validate(self) -> Result<(), RealtimeError> {
        if self.normalized_unix_nanos < self.axiusflow_receive_unix_nanos {
            return Err(RealtimeError::TimestampRegression {
                earlier: self.axiusflow_receive_unix_nanos,
                later: self.normalized_unix_nanos,
            });
        }
        if self
            .fanout_enqueue_unix_nanos
            .is_some_and(|fanout| fanout < self.normalized_unix_nanos)
        {
            return Err(RealtimeError::TimestampRegression {
                earlier: self.normalized_unix_nanos,
                later: self.fanout_enqueue_unix_nanos.unwrap_or_default(),
            });
        }
        Ok(())
    }
}

/// Versioned identity required when canonical payload semantics depend on a series definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalSeriesIdentity {
    pub instrument_revision: u64,
    pub definition_id: String,
    pub definition_version: u32,
    pub interval_seconds: u32,
}

/// Provider-neutral canonical market event header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalEventHeader {
    pub event_id: String,
    pub event_time_unix_nanos: i64,
    pub publication_time_unix_nanos: i64,
    pub producer: String,
    pub correlation_id: String,
    pub causation_id: String,
    pub entitlement_revision: String,
    pub instrument_id: String,
    pub venue_id: String,
    pub source_id: String,
    pub series_identity: Option<CanonicalSeriesIdentity>,
    pub source_sequence: u64,
    pub partition_id: u32,
    pub ownership_epoch: u64,
    pub timestamps: CanonicalTimestamps,
    pub nic_timestamp_source: Option<NicTimestampSource>,
    pub correction_flags: u64,
    pub quality_flags: u64,
    pub schema_version: u32,
    pub semantic_class: SemanticClass,
}

/// Materialized canonical event. No driver-owned pointer crosses this boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalMarketEvent {
    header: CanonicalEventHeader,
    payload: Vec<u8>,
}

impl CanonicalMarketEvent {
    /// Creates a bounded, fully owned canonical event.
    ///
    /// # Errors
    ///
    /// Returns an error for missing identity, ordering, schema, timestamp, or payload bounds.
    pub fn try_new(header: CanonicalEventHeader, payload: &[u8]) -> Result<Self, RealtimeError> {
        for (field, value) in [
            ("event_id", header.event_id.as_str()),
            ("producer", header.producer.as_str()),
            ("entitlement_revision", header.entitlement_revision.as_str()),
            ("instrument_id", header.instrument_id.as_str()),
            ("venue_id", header.venue_id.as_str()),
            ("source_id", header.source_id.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(RealtimeError::EmptyIdentity(field));
            }
        }
        if let Some(series) = &header.series_identity {
            if series.instrument_revision == 0 {
                return Err(RealtimeError::ZeroInstrumentRevision);
            }
            if series.definition_id.trim().is_empty() {
                return Err(RealtimeError::EmptyIdentity("series.definition_id"));
            }
            if series.definition_version == 0 {
                return Err(RealtimeError::ZeroSeriesDefinitionVersion);
            }
            if series.interval_seconds == 0 {
                return Err(RealtimeError::ZeroSeriesInterval);
            }
        }
        if header.source_sequence == 0 {
            return Err(RealtimeError::ZeroSourceSequence);
        }
        if header.ownership_epoch == 0 {
            return Err(RealtimeError::ZeroOwnershipEpoch);
        }
        if header.schema_version == 0 {
            return Err(RealtimeError::ZeroSchemaVersion);
        }
        header.timestamps.validate()?;
        if payload.len() > MAX_CANONICAL_PAYLOAD_BYTES {
            return Err(RealtimeError::PayloadLimitExceeded {
                requested: payload.len(),
                maximum: MAX_CANONICAL_PAYLOAD_BYTES,
            });
        }
        Ok(Self {
            header,
            payload: payload.to_vec(),
        })
    }

    #[must_use]
    pub const fn header(&self) -> &CanonicalEventHeader {
        &self.header
    }

    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    #[must_use]
    pub fn encoded_size_bytes(&self) -> usize {
        self.payload
            .len()
            .saturating_add(self.header.event_id.len())
            .saturating_add(self.header.producer.len())
            .saturating_add(self.header.correlation_id.len())
            .saturating_add(self.header.causation_id.len())
            .saturating_add(self.header.entitlement_revision.len())
            .saturating_add(self.header.instrument_id.len())
            .saturating_add(self.header.venue_id.len())
            .saturating_add(self.header.source_id.len())
            .saturating_add(
                self.header
                    .series_identity
                    .as_ref()
                    .map_or(0, |series| series.definition_id.len().saturating_add(24)),
            )
            .saturating_add(128)
    }
}

/// Identity and monotonic epoch of the sole active partition writer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartitionOwner {
    pub partition_id: u32,
    pub owner_id: String,
    pub ownership_epoch: NonZeroU64,
}

impl PartitionOwner {
    /// Creates validated ownership evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when the owner ID is empty or epoch is zero.
    pub fn try_new(
        partition_id: u32,
        owner_id: impl Into<String>,
        ownership_epoch: u64,
    ) -> Result<Self, RealtimeError> {
        let owner_id = owner_id.into();
        if owner_id.trim().is_empty() {
            return Err(RealtimeError::EmptyIdentity("owner_id"));
        }
        let ownership_epoch =
            NonZeroU64::new(ownership_epoch).ok_or(RealtimeError::ZeroOwnershipEpoch)?;
        Ok(Self {
            partition_id,
            owner_id,
            ownership_epoch,
        })
    }
}

/// Acceptance result for an ordered canonical partition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PartitionDecision {
    Accepted,
    Duplicate,
    SnapshotRequired,
    Gap { expected: u64, actual: u64 },
}

/// Result of presenting an owned event for fenced publication.
#[derive(Debug, Eq, PartialEq)]
pub enum PartitionAcceptance {
    Accepted(Box<AcceptedCanonicalEvent>),
    Duplicate,
    SnapshotRequired,
    Gap { expected: u64, actual: u64 },
}

/// Opaque proof that one exact canonical event was accepted by the current partition.
#[derive(Debug, Eq, PartialEq)]
pub struct AcceptedCanonicalEvent {
    authority_id: u64,
    event: CanonicalMarketEvent,
}

impl AcceptedCanonicalEvent {
    #[must_use]
    pub const fn event(&self) -> &CanonicalMarketEvent {
        &self.event
    }
}

/// Opaque direct-branch delivery accepted for latest-state projection.
#[derive(Debug, Eq, PartialEq)]
pub struct AcceptedDirectEvent {
    authority_id: u64,
    event: CanonicalMarketEvent,
}

impl AcceptedDirectEvent {
    #[must_use]
    pub const fn event(&self) -> &CanonicalMarketEvent {
        &self.event
    }
}

/// Opaque publication authority bound to one owner and epoch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationFence {
    authority_id: u64,
    partition_id: u32,
    owner_id: String,
    ownership_epoch: NonZeroU64,
}

impl PublicationFence {
    #[must_use]
    pub const fn partition_id(&self) -> u32 {
        self.partition_id
    }

    #[must_use]
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    #[must_use]
    pub const fn ownership_epoch(&self) -> u64 {
        self.ownership_epoch.get()
    }
}

/// Single-writer partition state with stale-owner fencing and gap latching.
#[derive(Debug, Eq, PartialEq)]
pub struct FencedPartition {
    authority_id: u64,
    owner: PartitionOwner,
    last_sequence: Option<u64>,
    generation: Option<NonZeroU64>,
    snapshot_required: bool,
}

impl FencedPartition {
    #[must_use]
    pub fn new(owner: PartitionOwner) -> Self {
        Self {
            authority_id: NEXT_PARTITION_AUTHORITY_ID.fetch_add(1, Ordering::Relaxed),
            owner,
            last_sequence: None,
            generation: None,
            snapshot_required: false,
        }
    }

    #[must_use]
    pub const fn owner(&self) -> &PartitionOwner {
        &self.owner
    }

    /// Issues the only publication token accepted for the current owner epoch.
    #[must_use]
    pub fn publication_fence(&self) -> PublicationFence {
        PublicationFence {
            authority_id: self.authority_id,
            partition_id: self.owner.partition_id,
            owner_id: self.owner.owner_id.clone(),
            ownership_epoch: self.owner.ownership_epoch,
        }
    }

    /// Fences the current writer and clears sequence state for explicit recovery.
    ///
    /// # Errors
    ///
    /// Returns an error unless partition identity is stable and epoch increases.
    pub fn handoff(&mut self, next: PartitionOwner) -> Result<(), RealtimeError> {
        if next.partition_id != self.owner.partition_id {
            return Err(RealtimeError::PartitionMismatch {
                expected: self.owner.partition_id,
                actual: next.partition_id,
            });
        }
        if next.ownership_epoch <= self.owner.ownership_epoch {
            return Err(RealtimeError::StaleOwnershipEpoch {
                active: self.owner.ownership_epoch.get(),
                attempted: next.ownership_epoch.get(),
            });
        }
        self.owner = next;
        self.last_sequence = None;
        self.generation = None;
        self.snapshot_required = true;
        Ok(())
    }

    /// Installs a checksum-verified snapshot under the active publication fence.
    ///
    /// # Errors
    ///
    /// Returns an error for stale ownership, invalid descriptor continuity,
    /// checksum mismatch, or a non-increasing generation.
    pub fn install_snapshot(
        &mut self,
        fence: &PublicationFence,
        snapshot: &CanonicalSnapshot,
    ) -> Result<(), RealtimeError> {
        let descriptor = snapshot.descriptor();
        self.verify_fence(
            fence,
            descriptor.partition_id,
            descriptor.ownership_epoch.get(),
        )?;
        if self
            .generation
            .is_some_and(|generation| descriptor.generation <= generation)
        {
            return Err(RealtimeError::StaleSnapshotGeneration {
                active: self.generation.map_or(0, NonZeroU64::get),
                attempted: descriptor.generation.get(),
            });
        }
        if let Some(active) = self.last_sequence
            && descriptor.last_sequence.get() < active
        {
            return Err(RealtimeError::SnapshotSequenceRegression {
                active,
                attempted: descriptor.last_sequence.get(),
            });
        }
        self.last_sequence = Some(descriptor.last_sequence.get());
        self.generation = Some(descriptor.generation);
        self.snapshot_required = false;
        Ok(())
    }

    fn require_snapshot(&mut self) {
        self.snapshot_required = true;
    }

    /// Accepts one event only with the active owner token and never crosses a gap.
    ///
    /// # Errors
    ///
    /// Returns an error for mismatched, stale, or foreign ownership.
    pub fn accept(
        &mut self,
        fence: &PublicationFence,
        event: &CanonicalMarketEvent,
    ) -> Result<PartitionDecision, RealtimeError> {
        let header = event.header();
        self.verify_fence(fence, header.partition_id, header.ownership_epoch)?;
        if self.snapshot_required {
            return Ok(PartitionDecision::SnapshotRequired);
        }
        let Some(last) = self.last_sequence else {
            self.last_sequence = Some(header.source_sequence);
            return Ok(PartitionDecision::Accepted);
        };
        if header.source_sequence <= last {
            return Ok(PartitionDecision::Duplicate);
        }
        let expected = last.checked_add(1).ok_or(RealtimeError::SequenceOverflow)?;
        if header.source_sequence != expected {
            self.snapshot_required = true;
            return Ok(PartitionDecision::Gap {
                expected,
                actual: header.source_sequence,
            });
        }
        self.last_sequence = Some(header.source_sequence);
        Ok(PartitionDecision::Accepted)
    }

    /// Accepts one owned event and mints publication proof only for `Accepted`.
    ///
    /// # Errors
    ///
    /// Returns an error for mismatched, stale, or foreign ownership.
    pub fn accept_for_publication(
        &mut self,
        fence: &PublicationFence,
        event: CanonicalMarketEvent,
    ) -> Result<PartitionAcceptance, RealtimeError> {
        match self.accept(fence, &event)? {
            PartitionDecision::Accepted => Ok(PartitionAcceptance::Accepted(Box::new(
                AcceptedCanonicalEvent {
                    authority_id: self.authority_id,
                    event,
                },
            ))),
            PartitionDecision::Duplicate => Ok(PartitionAcceptance::Duplicate),
            PartitionDecision::SnapshotRequired => Ok(PartitionAcceptance::SnapshotRequired),
            PartitionDecision::Gap { expected, actual } => {
                Ok(PartitionAcceptance::Gap { expected, actual })
            }
        }
    }

    fn verify_fence(
        &self,
        fence: &PublicationFence,
        partition_id: u32,
        ownership_epoch: u64,
    ) -> Result<(), RealtimeError> {
        if fence.authority_id != self.authority_id {
            return Err(RealtimeError::StalePublicationFence);
        }
        if partition_id != self.owner.partition_id || fence.partition_id != self.owner.partition_id
        {
            return Err(RealtimeError::PartitionMismatch {
                expected: self.owner.partition_id,
                actual: partition_id,
            });
        }
        if fence.owner_id != self.owner.owner_id {
            return Err(RealtimeError::OwnerMismatch);
        }
        if ownership_epoch != self.owner.ownership_epoch.get()
            || fence.ownership_epoch != self.owner.ownership_epoch
        {
            return Err(RealtimeError::StaleOwnershipEpoch {
                active: self.owner.ownership_epoch.get(),
                attempted: ownership_epoch,
            });
        }
        Ok(())
    }
}

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

    fn require_snapshot(&mut self, actual_sequence: u64) {
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

    fn fence_handoff(&mut self) {
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

fn verify_publication_identity(
    fence: &PublicationFence,
    partition_id: u32,
    ownership_epoch: u64,
) -> Result<(), RealtimeError> {
    if fence.partition_id != partition_id {
        return Err(RealtimeError::PartitionMismatch {
            expected: fence.partition_id,
            actual: partition_id,
        });
    }
    if fence.ownership_epoch.get() != ownership_epoch {
        return Err(RealtimeError::StaleOwnershipEpoch {
            active: fence.ownership_epoch.get(),
            attempted: ownership_epoch,
        });
    }
    Ok(())
}

/// Atomic snapshot identity and checksum contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotDescriptor {
    pub partition_id: u32,
    pub ownership_epoch: NonZeroU64,
    pub generation: NonZeroU64,
    pub first_sequence: NonZeroU64,
    pub last_sequence: NonZeroU64,
    pub schema_version: u32,
    pub checksum: [u8; 32],
}

impl SnapshotDescriptor {
    fn validate_shape(&self) -> Result<(), RealtimeError> {
        if self.last_sequence < self.first_sequence {
            return Err(RealtimeError::InvalidSnapshotRange {
                first: self.first_sequence.get(),
                last: self.last_sequence.get(),
            });
        }
        if self.schema_version == 0 {
            return Err(RealtimeError::ZeroSchemaVersion);
        }
        Ok(())
    }
}

/// Immutable canonical snapshot whose checksum is derived from its complete content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalSnapshot {
    descriptor: SnapshotDescriptor,
    events: Vec<CanonicalMarketEvent>,
}

impl CanonicalSnapshot {
    /// Creates a snapshot from canonical content and computes its descriptor checksum internally.
    ///
    /// # Errors
    ///
    /// Returns an error when identity, sequence, schema, or item-count invariants diverge.
    pub fn try_from_events(
        partition_id: u32,
        ownership_epoch: u64,
        generation: u64,
        schema_version: u32,
        events: Vec<CanonicalMarketEvent>,
    ) -> Result<Self, RealtimeError> {
        let first_sequence = events
            .first()
            .ok_or(RealtimeError::EmptySnapshot)?
            .header()
            .source_sequence;
        let last_sequence = events
            .last()
            .ok_or(RealtimeError::EmptySnapshot)?
            .header()
            .source_sequence;
        let mut descriptor = SnapshotDescriptor {
            partition_id,
            ownership_epoch: NonZeroU64::new(ownership_epoch)
                .ok_or(RealtimeError::ZeroOwnershipEpoch)?,
            generation: NonZeroU64::new(generation).ok_or(RealtimeError::ZeroSnapshotGeneration)?,
            first_sequence: NonZeroU64::new(first_sequence)
                .ok_or(RealtimeError::ZeroSourceSequence)?,
            last_sequence: NonZeroU64::new(last_sequence)
                .ok_or(RealtimeError::ZeroSourceSequence)?,
            schema_version,
            checksum: [0; 32],
        };
        validate_snapshot_events(&descriptor, &events)?;
        descriptor.checksum = compute_canonical_snapshot_checksum(&descriptor, &events);
        Ok(Self { descriptor, events })
    }

    /// Verifies externally supplied descriptor evidence against immutable snapshot content.
    ///
    /// # Errors
    ///
    /// Returns an error when content invariants fail or the derived checksum does not match.
    pub fn try_from_parts(
        descriptor: SnapshotDescriptor,
        events: Vec<CanonicalMarketEvent>,
    ) -> Result<Self, RealtimeError> {
        validate_snapshot_events(&descriptor, &events)?;
        if compute_canonical_snapshot_checksum(&descriptor, &events) != descriptor.checksum {
            return Err(RealtimeError::SnapshotChecksumMismatch);
        }
        Ok(Self { descriptor, events })
    }

    #[must_use]
    pub const fn descriptor(&self) -> &SnapshotDescriptor {
        &self.descriptor
    }

    #[must_use]
    pub fn events(&self) -> &[CanonicalMarketEvent] {
        &self.events
    }
}

/// One bounded latest-state snapshot request for a single canonical partition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LatestStateSnapshotRequest {
    maximum_items: NonZeroUsize,
}

impl LatestStateSnapshotRequest {
    #[must_use]
    pub const fn new(maximum_items: NonZeroUsize) -> Self {
        Self { maximum_items }
    }

    #[must_use]
    pub const fn maximum_items(self) -> usize {
        self.maximum_items.get()
    }
}

/// Partition-owned bounded canonical projection used only to derive recovery snapshots.
///
/// This projection is not an authority: installation and every direct update require
/// the current [`FencedPartition`] and its opaque [`PublicationFence`].
#[derive(Debug)]
pub struct CanonicalLatestState {
    active_fence: PublicationFence,
    item_capacity: NonZeroUsize,
    generation: NonZeroU64,
    schema_version: u32,
    events: VecDeque<CanonicalMarketEvent>,
    snapshot_required: bool,
}

impl CanonicalLatestState {
    /// Installs the initial checksum-verified snapshot into the partition and projection.
    ///
    /// If the snapshot exceeds the retained bound, only its newest contiguous tail is
    /// retained. Served snapshots receive a checksum derived from that exact tail.
    ///
    /// # Errors
    ///
    /// Returns an error for stale ownership, invalid snapshot evidence, or mixed semantics.
    pub fn try_new(
        partition: &mut FencedPartition,
        fence: &PublicationFence,
        snapshot: &CanonicalSnapshot,
        item_capacity: NonZeroUsize,
    ) -> Result<Self, RealtimeError> {
        partition.verify_fence(
            fence,
            snapshot.descriptor().partition_id,
            snapshot.descriptor().ownership_epoch.get(),
        )?;
        let events = prepare_latest_state_events(snapshot, item_capacity, None)?;
        partition.install_snapshot(fence, snapshot)?;
        Ok(Self {
            active_fence: fence.clone(),
            item_capacity,
            generation: snapshot.descriptor().generation,
            schema_version: snapshot.descriptor().schema_version,
            events,
            snapshot_required: false,
        })
    }

    /// Atomically replaces a gap-latched or ownership-fenced projection from a snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error before mutation for stale authority, regressing same-epoch
    /// generation/sequence evidence, changed series identity, or invalid content.
    pub fn install_snapshot(
        &mut self,
        partition: &mut FencedPartition,
        fence: &PublicationFence,
        snapshot: &CanonicalSnapshot,
    ) -> Result<(), RealtimeError> {
        let descriptor = snapshot.descriptor();
        partition.verify_fence(
            fence,
            descriptor.partition_id,
            descriptor.ownership_epoch.get(),
        )?;
        let current = self
            .events
            .front()
            .ok_or(RealtimeError::EmptySnapshot)?
            .header();
        let events = prepare_latest_state_events(snapshot, self.item_capacity, Some(current))?;
        if descriptor.ownership_epoch == self.active_fence.ownership_epoch {
            if descriptor.generation <= self.generation {
                return Err(RealtimeError::StaleSnapshotGeneration {
                    active: self.generation.get(),
                    attempted: descriptor.generation.get(),
                });
            }
            let current_last = self
                .events
                .back()
                .ok_or(RealtimeError::EmptySnapshot)?
                .header()
                .source_sequence;
            if descriptor.last_sequence.get() < current_last {
                return Err(RealtimeError::LatestStateSequenceRegression {
                    active: current_last,
                    attempted: descriptor.last_sequence.get(),
                });
            }
        } else if descriptor.ownership_epoch < self.active_fence.ownership_epoch {
            return Err(RealtimeError::StaleOwnershipEpoch {
                active: self.active_fence.ownership_epoch.get(),
                attempted: descriptor.ownership_epoch.get(),
            });
        }
        partition.install_snapshot(fence, snapshot)?;
        self.active_fence = fence.clone();
        self.generation = descriptor.generation;
        self.schema_version = descriptor.schema_version;
        self.events = events;
        self.snapshot_required = false;
        Ok(())
    }

    /// Applies one event received from the accepted direct branch.
    ///
    /// Duplicate events do not mutate state. A gap permanently blocks serving and
    /// further updates until [`Self::install_snapshot`] installs verified replacement state.
    ///
    /// # Errors
    ///
    /// Returns an error for stale authority, changed series identity, or generation overflow.
    pub fn apply_direct_event(
        &mut self,
        partition: &FencedPartition,
        fence: &PublicationFence,
        direct: AcceptedDirectEvent,
    ) -> Result<PartitionDecision, RealtimeError> {
        if direct.authority_id != fence.authority_id {
            return Err(RealtimeError::StalePublicationFence);
        }
        let event = direct.event;
        partition.verify_fence(
            fence,
            event.header().partition_id,
            event.header().ownership_epoch,
        )?;
        if fence != &self.active_fence {
            return Err(RealtimeError::StalePublicationFence);
        }
        if let Err(error) = validate_latest_state_event_identity(
            self.events
                .front()
                .ok_or(RealtimeError::EmptySnapshot)?
                .header(),
            event.header(),
        ) {
            self.snapshot_required = true;
            return Err(error);
        }
        if self.snapshot_required {
            return Ok(PartitionDecision::SnapshotRequired);
        }
        let last_sequence = self
            .events
            .back()
            .ok_or(RealtimeError::EmptySnapshot)?
            .header()
            .source_sequence;
        if event.header().source_sequence <= last_sequence {
            return Ok(PartitionDecision::Duplicate);
        }
        let expected = last_sequence
            .checked_add(1)
            .ok_or(RealtimeError::SequenceOverflow)?;
        if event.header().source_sequence != expected {
            self.snapshot_required = true;
            return Ok(PartitionDecision::Gap {
                expected,
                actual: event.header().source_sequence,
            });
        }
        let next_generation = self
            .generation
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or(RealtimeError::SequenceOverflow)?;
        self.events.push_back(event);
        while self.events.len() > self.item_capacity.get() {
            let _ = self.events.pop_front();
        }
        self.generation = next_generation;
        Ok(PartitionDecision::Accepted)
    }

    /// Derives a checksum-valid immutable tail snapshot under current partition authority.
    ///
    /// # Errors
    ///
    /// Returns an error while ownership is stale or recovery is required.
    pub fn serve_snapshot(
        &self,
        partition: &FencedPartition,
        fence: &PublicationFence,
        request: LatestStateSnapshotRequest,
    ) -> Result<CanonicalSnapshot, RealtimeError> {
        let first = self.events.front().ok_or(RealtimeError::EmptySnapshot)?;
        partition.verify_fence(
            fence,
            first.header().partition_id,
            first.header().ownership_epoch,
        )?;
        if fence != &self.active_fence {
            return Err(RealtimeError::StalePublicationFence);
        }
        if self.snapshot_required {
            return Err(RealtimeError::LatestStateSnapshotRequired);
        }
        let retained = request.maximum_items().min(self.events.len());
        let start = self.events.len().saturating_sub(retained);
        let events = self.events.iter().skip(start).cloned().collect();
        CanonicalSnapshot::try_from_events(
            fence.partition_id,
            fence.ownership_epoch.get(),
            self.generation.get(),
            self.schema_version,
            events,
        )
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation.get()
    }

    #[must_use]
    pub fn retained_items(&self) -> usize {
        self.events.len()
    }

    #[must_use]
    pub const fn requires_snapshot(&self) -> bool {
        self.snapshot_required
    }
}

fn prepare_latest_state_events(
    snapshot: &CanonicalSnapshot,
    item_capacity: NonZeroUsize,
    expected: Option<&CanonicalEventHeader>,
) -> Result<VecDeque<CanonicalMarketEvent>, RealtimeError> {
    let first = snapshot
        .events()
        .first()
        .ok_or(RealtimeError::EmptySnapshot)?
        .header();
    if first.semantic_class != SemanticClass::OrderedDelta {
        return Err(RealtimeError::SemanticClassMismatch);
    }
    if let Some(expected) = expected {
        validate_latest_state_series_identity(expected, first)?;
    }
    for event in snapshot.events() {
        if event.header().semantic_class != first.semantic_class {
            return Err(RealtimeError::SnapshotEventMismatch {
                sequence: event.header().source_sequence,
                field: "semantic_class",
            });
        }
    }
    let start = snapshot.events().len().saturating_sub(item_capacity.get());
    Ok(snapshot.events()[start..].iter().cloned().collect())
}

fn validate_latest_state_event_identity(
    expected: &CanonicalEventHeader,
    actual: &CanonicalEventHeader,
) -> Result<(), RealtimeError> {
    validate_latest_state_series_identity(expected, actual)?;
    for (field, matches) in [
        ("partition_id", actual.partition_id == expected.partition_id),
        (
            "ownership_epoch",
            actual.ownership_epoch == expected.ownership_epoch,
        ),
        (
            "schema_version",
            actual.schema_version == expected.schema_version,
        ),
    ] {
        if !matches {
            return Err(RealtimeError::SnapshotEventMismatch {
                sequence: actual.source_sequence,
                field,
            });
        }
    }
    Ok(())
}

fn validate_latest_state_series_identity(
    expected: &CanonicalEventHeader,
    actual: &CanonicalEventHeader,
) -> Result<(), RealtimeError> {
    for (field, matches) in [
        (
            "instrument_id",
            actual.instrument_id == expected.instrument_id,
        ),
        ("venue_id", actual.venue_id == expected.venue_id),
        ("source_id", actual.source_id == expected.source_id),
        (
            "series_identity",
            actual.series_identity == expected.series_identity,
        ),
        (
            "schema_version",
            actual.schema_version == expected.schema_version,
        ),
        (
            "semantic_class",
            actual.semantic_class == expected.semantic_class,
        ),
    ] {
        if !matches {
            return Err(RealtimeError::SnapshotEventMismatch {
                sequence: actual.source_sequence,
                field,
            });
        }
    }
    Ok(())
}

fn validate_snapshot_events(
    descriptor: &SnapshotDescriptor,
    events: &[CanonicalMarketEvent],
) -> Result<(), RealtimeError> {
    descriptor.validate_shape()?;
    let first = events.first().ok_or(RealtimeError::EmptySnapshot)?;
    let expected_count = descriptor
        .last_sequence
        .get()
        .checked_sub(descriptor.first_sequence.get())
        .and_then(|difference| difference.checked_add(1))
        .ok_or(RealtimeError::InvalidSnapshotRange {
            first: descriptor.first_sequence.get(),
            last: descriptor.last_sequence.get(),
        })?;
    if expected_count != u64::try_from(events.len()).unwrap_or(u64::MAX) {
        return Err(RealtimeError::SnapshotItemCountMismatch {
            expected: expected_count,
            actual: events.len(),
        });
    }
    let first_header = first.header();
    for (index, event) in events.iter().enumerate() {
        let header = event.header();
        let offset = u64::try_from(index).map_err(|_| RealtimeError::SequenceOverflow)?;
        let expected_sequence = descriptor
            .first_sequence
            .get()
            .checked_add(offset)
            .ok_or(RealtimeError::SequenceOverflow)?;
        if header.source_sequence != expected_sequence {
            return Err(RealtimeError::SnapshotEventMismatch {
                sequence: header.source_sequence,
                field: "source_sequence",
            });
        }
        for (field, matches) in [
            (
                "partition_id",
                header.partition_id == descriptor.partition_id,
            ),
            (
                "ownership_epoch",
                header.ownership_epoch == descriptor.ownership_epoch.get(),
            ),
            (
                "schema_version",
                header.schema_version == descriptor.schema_version,
            ),
            (
                "instrument_id",
                header.instrument_id == first_header.instrument_id,
            ),
            ("venue_id", header.venue_id == first_header.venue_id),
            ("source_id", header.source_id == first_header.source_id),
            (
                "series_identity",
                header.series_identity == first_header.series_identity,
            ),
        ] {
            if !matches {
                return Err(RealtimeError::SnapshotEventMismatch {
                    sequence: header.source_sequence,
                    field,
                });
            }
        }
    }
    Ok(())
}

fn compute_canonical_snapshot_checksum(
    descriptor: &SnapshotDescriptor,
    events: &[CanonicalMarketEvent],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(descriptor.partition_id.to_be_bytes());
    digest.update(descriptor.ownership_epoch.get().to_be_bytes());
    digest.update(descriptor.generation.get().to_be_bytes());
    digest.update(descriptor.first_sequence.get().to_be_bytes());
    digest.update(descriptor.last_sequence.get().to_be_bytes());
    digest.update(descriptor.schema_version.to_be_bytes());
    for event in events {
        update_canonical_event_digest(&mut digest, event);
    }
    digest.finalize().into()
}

fn update_canonical_event_digest(digest: &mut Sha256, event: &CanonicalMarketEvent) {
    let header = event.header();
    update_digest_string(digest, &header.event_id);
    digest.update(header.event_time_unix_nanos.to_be_bytes());
    digest.update(header.publication_time_unix_nanos.to_be_bytes());
    update_digest_string(digest, &header.producer);
    update_digest_string(digest, &header.correlation_id);
    update_digest_string(digest, &header.causation_id);
    update_digest_string(digest, &header.entitlement_revision);
    update_digest_string(digest, &header.instrument_id);
    update_digest_string(digest, &header.venue_id);
    update_digest_string(digest, &header.source_id);
    match &header.series_identity {
        Some(series) => {
            digest.update([1]);
            digest.update(series.instrument_revision.to_be_bytes());
            update_digest_string(digest, &series.definition_id);
            digest.update(series.definition_version.to_be_bytes());
            digest.update(series.interval_seconds.to_be_bytes());
        }
        None => digest.update([0]),
    }
    digest.update(header.source_sequence.to_be_bytes());
    digest.update(header.partition_id.to_be_bytes());
    digest.update(header.ownership_epoch.to_be_bytes());
    let timestamps = header.timestamps;
    digest.update(timestamps.exchange_unix_nanos.to_be_bytes());
    digest.update(timestamps.provider_receive_unix_nanos.to_be_bytes());
    match timestamps.nic_receive_unix_nanos {
        Some(timestamp) => {
            digest.update([1]);
            digest.update(timestamp.to_be_bytes());
        }
        None => digest.update([0]),
    }
    digest.update(timestamps.axiusflow_receive_unix_nanos.to_be_bytes());
    digest.update(timestamps.normalized_unix_nanos.to_be_bytes());
    match timestamps.fanout_enqueue_unix_nanos {
        Some(timestamp) => {
            digest.update([1]);
            digest.update(timestamp.to_be_bytes());
        }
        None => digest.update([0]),
    }
    digest.update([match header.nic_timestamp_source {
        None => 0,
        Some(NicTimestampSource::SocketSoftware) => 1,
        Some(NicTimestampSource::KernelSoftware) => 2,
        Some(NicTimestampSource::NicHardware) => 3,
    }]);
    digest.update(header.correction_flags.to_be_bytes());
    digest.update(header.quality_flags.to_be_bytes());
    digest.update(header.schema_version.to_be_bytes());
    digest.update([match header.semantic_class {
        SemanticClass::StateReplace => 1,
        SemanticClass::OrderedDelta => 2,
        SemanticClass::ReliableEvent => 3,
        SemanticClass::AuthoritativeEvent => 4,
        SemanticClass::Snapshot => 5,
    }]);
    digest.update(
        u64::try_from(event.payload().len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    digest.update(event.payload());
}

fn update_digest_string(digest: &mut Sha256, value: &str) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(value.as_bytes());
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RealtimeError {
    EmptyIdentity(&'static str),
    ZeroSourceSequence,
    ZeroOwnershipEpoch,
    ZeroSnapshotGeneration,
    ZeroSchemaVersion,
    ZeroInstrumentRevision,
    ZeroSeriesDefinitionVersion,
    ZeroSeriesInterval,
    EmptySnapshot,
    SnapshotItemCountMismatch { expected: u64, actual: usize },
    SnapshotEventMismatch { sequence: u64, field: &'static str },
    PayloadLimitExceeded { requested: usize, maximum: usize },
    TimestampRegression { earlier: i64, later: i64 },
    PartitionMismatch { expected: u32, actual: u32 },
    OwnerMismatch,
    StaleOwnershipEpoch { active: u64, attempted: u64 },
    StalePublicationFence,
    FanoutEnqueueTimestampMismatch { declared: Option<i64>, actual: i64 },
    StaleSnapshotGeneration { active: u64, attempted: u64 },
    SnapshotSequenceRegression { active: u64, attempted: u64 },
    LatestStateSequenceRegression { active: u64, attempted: u64 },
    LatestStateSnapshotRequired,
    InvalidSnapshotRange { first: u64, last: u64 },
    SnapshotChecksumMismatch,
    SequenceOverflow,
    AlertThresholdExceedsCapacity,
    IncompatibleOverflowAction,
    SemanticClassMismatch,
    FanoutPolicyMismatch,
}

impl fmt::Display for RealtimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "real-time contract rejected input: {self:?}")
    }
}

impl Error for RealtimeError {}
