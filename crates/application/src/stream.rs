//! Transport-neutral replay provenance, checksum, and sequence mechanics.

use core::fmt;
use sha2::{Digest, Sha256};
use std::error::Error;

/// Maximum items accepted in one client snapshot.
pub const MAX_STREAM_SNAPSHOT_ITEMS: usize = 2_048;

/// Canonical evidence retained with each displayed or replayed market value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketEventProvenance {
    pub event_id: String,
    pub event_time_unix_nanos: i64,
    pub publication_time_unix_nanos: i64,
    pub producer: String,
    pub schema_version: u32,
    pub correlation_id: String,
    pub causation_id: String,
    pub entitlement_revision: String,
    pub session_generation: u64,
    pub source_id: String,
    pub source_sequence: u64,
    pub exchange_timestamp_unix_nanos: i64,
    pub provider_receive_timestamp_unix_nanos: i64,
    pub nic_receive_timestamp_unix_nanos: Option<i64>,
    pub axiusflow_receive_timestamp_unix_nanos: i64,
    pub normalized_timestamp_unix_nanos: i64,
    pub fanout_enqueue_timestamp_unix_nanos: Option<i64>,
    pub correction_flags: u64,
    pub quality_flags: u64,
    pub nic_timestamp_source: i32,
    pub semantic_class: i32,
}

/// A value that cannot cross replay/display boundaries without canonical evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Provenanced<Value> {
    value: Value,
    provenance: MarketEventProvenance,
}

impl<Value> Provenanced<Value> {
    #[must_use]
    pub const fn new(value: Value, provenance: MarketEventProvenance) -> Self {
        Self { value, provenance }
    }

    #[must_use]
    pub const fn value(&self) -> &Value {
        &self.value
    }

    #[must_use]
    pub const fn provenance(&self) -> &MarketEventProvenance {
        &self.provenance
    }

    #[must_use]
    pub fn into_parts(self) -> (Value, MarketEventProvenance) {
        (self.value, self.provenance)
    }
}

/// Snapshot identity and integrity evidence retained through recovery boundaries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotEvidence {
    pub session_generation: u64,
    pub publication_generation: u64,
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub schema_version: u32,
    pub checksum: [u8; 32],
}

/// Borrowed immutable instrument and series identity bound into snapshot integrity evidence.
#[derive(Clone, Copy, Debug)]
pub struct MarketSnapshotIdentityRef<'identity> {
    pub instrument_id: &'identity str,
    pub instrument_revision: u64,
    pub bar_definition_id: &'identity str,
    pub bar_definition_version: u32,
    pub bar_interval_seconds: u32,
    pub bar_trades_per_bar: Option<u32>,
}

/// Borrowed canonical fields used by the shared snapshot checksum algorithm.
#[derive(Clone, Copy, Debug)]
pub struct MarketValueChecksumRef<'value> {
    pub source_sequence: u64,
    pub exchange_timestamp_seconds: i64,
    pub exchange_timestamp_unix_nanos: i64,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub volume: i64,
    pub provenance: &'value MarketEventProvenance,
}

/// Computes SHA-256 over snapshot identity and every canonical value/evidence field.
#[must_use]
pub fn compute_market_snapshot_checksum<'value>(
    evidence: &SnapshotEvidence,
    identity: MarketSnapshotIdentityRef<'_>,
    values: impl IntoIterator<Item = MarketValueChecksumRef<'value>>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(evidence.session_generation.to_be_bytes());
    digest.update(evidence.publication_generation.to_be_bytes());
    digest.update(evidence.first_sequence.to_be_bytes());
    digest.update(evidence.last_sequence.to_be_bytes());
    digest.update(evidence.schema_version.to_be_bytes());
    update_string_digest(&mut digest, identity.instrument_id);
    digest.update(identity.instrument_revision.to_be_bytes());
    update_string_digest(&mut digest, identity.bar_definition_id);
    digest.update(identity.bar_definition_version.to_be_bytes());
    digest.update(identity.bar_interval_seconds.to_be_bytes());
    digest.update(identity.bar_trades_per_bar.unwrap_or(0).to_be_bytes());
    for value in values {
        digest.update(value.source_sequence.to_be_bytes());
        digest.update(value.exchange_timestamp_seconds.to_be_bytes());
        digest.update(value.exchange_timestamp_unix_nanos.to_be_bytes());
        digest.update(value.open.to_be_bytes());
        digest.update(value.high.to_be_bytes());
        digest.update(value.low.to_be_bytes());
        digest.update(value.close.to_be_bytes());
        digest.update(value.volume.to_be_bytes());
        update_provenance_digest(&mut digest, value.provenance);
    }
    digest.finalize().into()
}

fn update_provenance_digest(digest: &mut Sha256, provenance: &MarketEventProvenance) {
    update_string_digest(digest, &provenance.event_id);
    digest.update(provenance.event_time_unix_nanos.to_be_bytes());
    digest.update(provenance.publication_time_unix_nanos.to_be_bytes());
    update_string_digest(digest, &provenance.producer);
    digest.update(provenance.schema_version.to_be_bytes());
    update_string_digest(digest, &provenance.correlation_id);
    update_string_digest(digest, &provenance.causation_id);
    update_string_digest(digest, &provenance.entitlement_revision);
    digest.update(provenance.session_generation.to_be_bytes());
    update_string_digest(digest, &provenance.source_id);
    digest.update(provenance.source_sequence.to_be_bytes());
    digest.update(provenance.exchange_timestamp_unix_nanos.to_be_bytes());
    digest.update(
        provenance
            .provider_receive_timestamp_unix_nanos
            .to_be_bytes(),
    );
    match provenance.nic_receive_timestamp_unix_nanos {
        Some(timestamp) => {
            digest.update([1]);
            digest.update(timestamp.to_be_bytes());
        }
        None => digest.update([0]),
    }
    digest.update(
        provenance
            .axiusflow_receive_timestamp_unix_nanos
            .to_be_bytes(),
    );
    digest.update(provenance.normalized_timestamp_unix_nanos.to_be_bytes());
    match provenance.fanout_enqueue_timestamp_unix_nanos {
        Some(timestamp) => {
            digest.update([1]);
            digest.update(timestamp.to_be_bytes());
        }
        None => digest.update([0]),
    }
    digest.update(provenance.correction_flags.to_be_bytes());
    digest.update(provenance.quality_flags.to_be_bytes());
    digest.update(provenance.nic_timestamp_source.to_be_bytes());
    digest.update(provenance.semantic_class.to_be_bytes());
}

fn update_string_digest(digest: &mut Sha256, value: &str) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(value.as_bytes());
}

/// One validated bounded snapshot from a sequence-numbered stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamSnapshot<T> {
    first_sequence: u64,
    last_sequence: u64,
    items: Vec<T>,
}

impl<T> StreamSnapshot<T> {
    /// Creates a non-empty snapshot whose item count matches its sequence range.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or reversed sequences, empty or oversized
    /// input, or a count that does not match the inclusive sequence range.
    pub fn try_new(
        first_sequence: u64,
        last_sequence: u64,
        items: Vec<T>,
    ) -> Result<Self, StreamProtocolError> {
        if first_sequence == 0 || last_sequence == 0 {
            return Err(StreamProtocolError::ZeroSequence);
        }
        if last_sequence < first_sequence {
            return Err(StreamProtocolError::InvalidSequenceRange {
                first: first_sequence,
                last: last_sequence,
            });
        }
        if items.is_empty() {
            return Err(StreamProtocolError::EmptySnapshot);
        }
        if items.len() > MAX_STREAM_SNAPSHOT_ITEMS {
            return Err(StreamProtocolError::ItemLimitExceeded {
                requested: items.len(),
                maximum: MAX_STREAM_SNAPSHOT_ITEMS,
            });
        }

        let expected_count = last_sequence - first_sequence + 1;
        let actual_count = u64::try_from(items.len())
            .map_err(|_| StreamProtocolError::ItemCountOverflow(items.len()))?;
        if actual_count != expected_count {
            return Err(StreamProtocolError::ItemCountMismatch {
                expected: expected_count,
                actual: items.len(),
            });
        }

        Ok(Self {
            first_sequence,
            last_sequence,
            items,
        })
    }

    /// Returns the first included source sequence.
    #[must_use]
    pub const fn first_sequence(&self) -> u64 {
        self.first_sequence
    }

    /// Returns the last included source sequence.
    #[must_use]
    pub const fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    /// Returns all snapshot items in sequence order.
    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// Returns the number of items in the snapshot.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns whether the snapshot has no items. Valid snapshots are never empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// One sequence-numbered update following an accepted predecessor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamDelta<T> {
    previous_sequence: u64,
    sequence: u64,
    item: T,
}

impl<T> StreamDelta<T> {
    /// Creates a delta that immediately follows its declared predecessor.
    ///
    /// # Errors
    ///
    /// Returns an error for zero sequences, overflow, or a non-contiguous pair.
    pub fn try_new(
        previous_sequence: u64,
        sequence: u64,
        item: T,
    ) -> Result<Self, StreamProtocolError> {
        if previous_sequence == 0 || sequence == 0 {
            return Err(StreamProtocolError::ZeroSequence);
        }
        let expected = previous_sequence
            .checked_add(1)
            .ok_or(StreamProtocolError::SequenceOverflow)?;
        if sequence != expected {
            return Err(StreamProtocolError::NonContiguousDelta {
                expected,
                actual: sequence,
            });
        }
        Ok(Self {
            previous_sequence,
            sequence,
            item,
        })
    }

    /// Returns the predecessor sequence declared by the producer.
    #[must_use]
    pub const fn previous_sequence(&self) -> u64 {
        self.previous_sequence
    }

    /// Returns this update's source sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the immutable update value.
    #[must_use]
    pub const fn item(&self) -> &T {
        &self.item
    }
}

/// Result of sequence-checking one delta against client state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SequenceDecision {
    Accepted,
    Duplicate,
    SnapshotRequired,
    Gap { expected: u64, actual: u64 },
}

/// Client-side sequence state. A gap permanently requires a new snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SequenceTracker {
    expected_sequence: Option<u64>,
    snapshot_required: bool,
}

impl SequenceTracker {
    /// Installs a snapshot and resets any prior gap state.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot ends at the maximum sequence and
    /// cannot be resumed safely.
    pub fn install_snapshot<T>(
        &mut self,
        snapshot: &StreamSnapshot<T>,
    ) -> Result<(), StreamProtocolError> {
        self.expected_sequence = Some(
            snapshot
                .last_sequence()
                .checked_add(1)
                .ok_or(StreamProtocolError::SequenceOverflow)?,
        );
        self.snapshot_required = false;
        Ok(())
    }

    /// Checks and advances one delta without speculating across gaps.
    ///
    /// # Errors
    ///
    /// Returns an error only if the accepted sequence cannot advance.
    pub fn accept_delta<T>(
        &mut self,
        delta: &StreamDelta<T>,
    ) -> Result<SequenceDecision, StreamProtocolError> {
        if self.snapshot_required {
            return Ok(SequenceDecision::SnapshotRequired);
        }
        let Some(expected) = self.expected_sequence else {
            return Ok(SequenceDecision::SnapshotRequired);
        };
        if delta.sequence() < expected {
            return Ok(SequenceDecision::Duplicate);
        }
        if delta.sequence() > expected {
            self.snapshot_required = true;
            return Ok(SequenceDecision::Gap {
                expected,
                actual: delta.sequence(),
            });
        }

        self.expected_sequence = Some(
            expected
                .checked_add(1)
                .ok_or(StreamProtocolError::SequenceOverflow)?,
        );
        Ok(SequenceDecision::Accepted)
    }

    /// Returns the next sequence expected after the accepted state.
    #[must_use]
    pub const fn expected_sequence(self) -> Option<u64> {
        self.expected_sequence
    }

    /// Returns whether a detected gap requires a fresh snapshot.
    #[must_use]
    pub const fn requires_snapshot(self) -> bool {
        self.snapshot_required || self.expected_sequence.is_none()
    }
}

/// Validation failures for bounded snapshot/delta mechanics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamProtocolError {
    EmptySnapshot,
    ItemLimitExceeded { requested: usize, maximum: usize },
    ZeroSequence,
    InvalidSequenceRange { first: u64, last: u64 },
    ItemCountOverflow(usize),
    ItemCountMismatch { expected: u64, actual: usize },
    NonContiguousDelta { expected: u64, actual: u64 },
    SequenceOverflow,
}

impl fmt::Display for StreamProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySnapshot => formatter.write_str("stream snapshot must not be empty"),
            Self::ItemLimitExceeded { requested, maximum } => write!(
                formatter,
                "stream snapshot contains {requested} items; maximum is {maximum}"
            ),
            Self::ZeroSequence => formatter.write_str("stream sequences must be non-zero"),
            Self::InvalidSequenceRange { first, last } => {
                write!(formatter, "invalid stream sequence range {first}..={last}")
            }
            Self::ItemCountOverflow(actual) => {
                write!(
                    formatter,
                    "stream item count {actual} cannot be represented"
                )
            }
            Self::ItemCountMismatch { expected, actual } => write!(
                formatter,
                "stream sequence range requires {expected} items; received {actual}"
            ),
            Self::NonContiguousDelta { expected, actual } => write!(
                formatter,
                "stream delta expected sequence {expected}; received {actual}"
            ),
            Self::SequenceOverflow => formatter.write_str("stream sequence overflowed"),
        }
    }
}

impl Error for StreamProtocolError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance(session_generation: u64) -> MarketEventProvenance {
        MarketEventProvenance {
            event_id: "event".into(),
            event_time_unix_nanos: 1,
            publication_time_unix_nanos: 2,
            producer: "fixture".into(),
            schema_version: 2,
            correlation_id: "correlation".into(),
            causation_id: "causation".into(),
            entitlement_revision: "entitlement".into(),
            session_generation,
            source_id: "source".into(),
            source_sequence: 1,
            exchange_timestamp_unix_nanos: 1_000_000_000,
            provider_receive_timestamp_unix_nanos: 1_000_000_001,
            nic_receive_timestamp_unix_nanos: None,
            axiusflow_receive_timestamp_unix_nanos: 1_000_000_002,
            normalized_timestamp_unix_nanos: 1_000_000_003,
            fanout_enqueue_timestamp_unix_nanos: None,
            correction_flags: 0,
            quality_flags: 0,
            nic_timestamp_source: 0,
            semantic_class: 1,
        }
    }

    fn checksum(session_generation: u64, publication_generation: u64) -> [u8; 32] {
        let provenance = provenance(session_generation);
        compute_market_snapshot_checksum(
            &SnapshotEvidence {
                session_generation,
                publication_generation,
                first_sequence: 1,
                last_sequence: 1,
                schema_version: 2,
                checksum: [0; 32],
            },
            MarketSnapshotIdentityRef {
                instrument_id: "BTC-USD",
                instrument_revision: 1,
                bar_definition_id: "one-minute",
                bar_definition_version: 1,
                bar_interval_seconds: 60,
                bar_trades_per_bar: None,
            },
            [MarketValueChecksumRef {
                source_sequence: 1,
                exchange_timestamp_seconds: 1,
                exchange_timestamp_unix_nanos: 1_000_000_000,
                open: 10,
                high: 12,
                low: 9,
                close: 11,
                volume: 5,
                provenance: &provenance,
            }],
        )
    }

    #[test]
    fn checksum_is_deterministic_and_fences_session_and_publication_generations() {
        assert_eq!(checksum(1, 1), checksum(1, 1));
        assert_ne!(checksum(1, 1), checksum(2, 1));
        assert_ne!(checksum(1, 1), checksum(1, 2));
    }
}
