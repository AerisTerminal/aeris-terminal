//! Partition-owned bounded latest-state projection for recovery snapshots.

use crate::canonical_event::{CanonicalEventHeader, CanonicalMarketEvent, SemanticClass};
use crate::errors::RealtimeError;
use crate::partition::{AcceptedDirectEvent, FencedPartition, PartitionDecision, PublicationFence};
use crate::snapshot::CanonicalSnapshot;
use std::collections::VecDeque;
use std::num::{NonZeroU64, NonZeroUsize};

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
