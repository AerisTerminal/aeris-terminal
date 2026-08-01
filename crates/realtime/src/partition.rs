//! Fenced single-writer partition ownership and publication proof.

use crate::canonical_event::CanonicalMarketEvent;
use crate::errors::RealtimeError;
use crate::snapshot::CanonicalSnapshot;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_PARTITION_AUTHORITY_ID: AtomicU64 = AtomicU64::new(1);

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
    pub(crate) authority_id: u64,
    pub(crate) event: CanonicalMarketEvent,
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
    pub(crate) authority_id: u64,
    pub(crate) event: CanonicalMarketEvent,
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
    pub(crate) authority_id: u64,
    pub(crate) partition_id: u32,
    pub(crate) owner_id: String,
    pub(crate) ownership_epoch: NonZeroU64,
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

    pub(crate) fn require_snapshot(&mut self) {
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

    pub(crate) fn verify_fence(
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

pub(crate) fn verify_publication_identity(
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

#[cfg(test)]
mod tests {
    use super::{FencedPartition, PartitionDecision, PartitionOwner};
    use crate::canonical_event::SemanticClass;
    use crate::errors::RealtimeError;
    use crate::test_fixture::event;

    #[test]
    fn partition_accepts_contiguous_events_and_latches_on_gap() {
        let owner = PartitionOwner::try_new(7, "owner-1", 1).expect("owner is valid");
        let mut partition = FencedPartition::new(owner);
        let fence = partition.publication_fence();

        assert_eq!(
            partition
                .accept(&fence, &event(1, 1, SemanticClass::OrderedDelta))
                .expect("first event is accepted"),
            PartitionDecision::Accepted
        );
        assert_eq!(
            partition
                .accept(&fence, &event(1, 1, SemanticClass::OrderedDelta))
                .expect("duplicate is classified"),
            PartitionDecision::Duplicate
        );
        assert_eq!(
            partition
                .accept(&fence, &event(3, 1, SemanticClass::OrderedDelta))
                .expect("gap is classified"),
            PartitionDecision::Gap {
                expected: 2,
                actual: 3,
            }
        );
        assert_eq!(
            partition
                .accept(&fence, &event(2, 1, SemanticClass::OrderedDelta))
                .expect("latched partition requires recovery"),
            PartitionDecision::SnapshotRequired
        );
    }

    #[test]
    fn ownership_handoff_rejects_the_stale_publication_fence() {
        let owner = PartitionOwner::try_new(7, "owner-1", 1).expect("owner is valid");
        let mut partition = FencedPartition::new(owner);
        let stale = partition.publication_fence();
        partition
            .handoff(PartitionOwner::try_new(7, "owner-2", 2).expect("next owner is valid"))
            .expect("higher epoch handoff succeeds");

        assert!(matches!(
            partition.accept(&stale, &event(1, 1, SemanticClass::OrderedDelta)),
            Err(RealtimeError::OwnerMismatch | RealtimeError::StaleOwnershipEpoch { .. })
        ));
        assert!(matches!(
            partition
                .handoff(PartitionOwner::try_new(7, "owner-3", 2).expect("owner shape is valid")),
            Err(RealtimeError::StaleOwnershipEpoch {
                active: 2,
                attempted: 2,
            })
        ));
    }
}
