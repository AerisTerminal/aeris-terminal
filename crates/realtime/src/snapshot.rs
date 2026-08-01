//! Immutable canonical snapshots with content-derived checksums.

use crate::canonical_event::{CanonicalMarketEvent, NicTimestampSource, SemanticClass};
use crate::errors::RealtimeError;
use sha2::{Digest, Sha256};
use std::num::NonZeroU64;

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
