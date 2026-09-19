//! Validated bounded replay snapshots and gap-safe consumer sessions.

use crate::errors::ReplayValidationError;
use crate::provenance::{
    ProvenancedMarketBar, ReplayProvenance, embedded_event_provenance, snapshot_checksum,
    try_provenanced_market_bar, validate_provenanced_market_bar,
};
use crate::stream::{
    SequenceDecision, SequenceTracker, SnapshotEvidence, StreamDelta, StreamProtocolError,
    StreamSnapshot,
};
use tradingplot_instruments::InstrumentRevision;
use tradingplot_market_data::{BarDefinition, MarketBar};

/// A validated, bounded replay snapshot tied to one instrument revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplaySnapshot {
    instrument: InstrumentRevision,
    provenance: ReplayProvenance,
    bar_definition: BarDefinition,
    stream: StreamSnapshot<ProvenancedMarketBar>,
    evidence: SnapshotEvidence,
}

impl ReplaySnapshot {
    /// Validates an immutable embedded replay snapshot before it crosses into a UI adapter.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid reference or bar-definition data, invalid
    /// OHLCV values, sequence gaps, non-increasing timestamps, or stream bounds.
    pub fn try_new(
        instrument: InstrumentRevision,
        provenance: ReplayProvenance,
        bar_definition: BarDefinition,
        bars: Vec<MarketBar>,
    ) -> Result<Self, ReplayValidationError> {
        let bars = bars
            .into_iter()
            .map(|bar| {
                let evidence = embedded_event_provenance(&bar);
                try_provenanced_market_bar(bar, evidence)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let first = bars.first().ok_or(StreamProtocolError::EmptySnapshot)?;
        let last = bars.last().ok_or(StreamProtocolError::EmptySnapshot)?;
        let mut evidence = SnapshotEvidence {
            session_generation: first.provenance().session_generation,
            publication_generation: 1,
            first_sequence: first.value().source_sequence,
            last_sequence: last.value().source_sequence,
            schema_version: first.provenance().schema_version,
            checksum: [0; 32],
        };
        evidence.checksum = snapshot_checksum(&evidence, &instrument, &bar_definition, &bars);
        Self::try_new_provenanced(instrument, provenance, bar_definition, evidence, bars)
    }

    /// Validates a provenance-bearing immutable snapshot from replay or live transport.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid reference, evidence, sequence, timestamp, checksum, or bounds.
    pub fn try_new_provenanced(
        instrument: InstrumentRevision,
        provenance: ReplayProvenance,
        bar_definition: BarDefinition,
        evidence: SnapshotEvidence,
        bars: Vec<ProvenancedMarketBar>,
    ) -> Result<Self, ReplayValidationError> {
        instrument.validate()?;
        bar_definition.validate()?;
        if evidence.session_generation == 0 {
            return Err(ReplayValidationError::InvalidSnapshotEvidence(
                "session_generation",
            ));
        }
        if evidence.publication_generation == 0 {
            return Err(ReplayValidationError::InvalidSnapshotEvidence(
                "publication_generation",
            ));
        }
        if evidence.schema_version == 0 {
            return Err(ReplayValidationError::InvalidSnapshotEvidence(
                "schema_version",
            ));
        }
        let Some(first_item) = bars.first() else {
            return Err(StreamProtocolError::EmptySnapshot.into());
        };

        validate_provenanced_market_bar(first_item)?;
        let first_bar = *first_item.value();
        let mut previous = first_bar;
        let mut previous_timestamp = first_item.provenance().exchange_timestamp_unix_nanos;
        for item in &bars {
            validate_provenanced_market_bar(item)?;
            let item_provenance = item.provenance();
            if item_provenance.session_generation != evidence.session_generation {
                return Err(ReplayValidationError::SnapshotEvidenceMismatch(
                    "session_generation",
                ));
            }
            if item_provenance.schema_version != evidence.schema_version {
                return Err(ReplayValidationError::SnapshotEvidenceMismatch(
                    "schema_version",
                ));
            }
        }
        for item in bars.iter().skip(1) {
            let bar = *item.value();
            StreamDelta::try_new(previous.source_sequence, bar.source_sequence, ())?;
            let timestamp = item.provenance().exchange_timestamp_unix_nanos;
            if timestamp <= previous_timestamp {
                return Err(ReplayValidationError::NonIncreasingTimestamp {
                    source_sequence: bar.source_sequence,
                });
            }
            previous = bar;
            previous_timestamp = timestamp;
        }
        if evidence.first_sequence != first_bar.source_sequence {
            return Err(ReplayValidationError::SnapshotEvidenceMismatch(
                "first_sequence",
            ));
        }
        if evidence.last_sequence != previous.source_sequence {
            return Err(ReplayValidationError::SnapshotEvidenceMismatch(
                "last_sequence",
            ));
        }
        if snapshot_checksum(&evidence, &instrument, &bar_definition, &bars) != evidence.checksum {
            return Err(ReplayValidationError::SnapshotChecksumMismatch);
        }

        let stream =
            StreamSnapshot::try_new(first_bar.source_sequence, previous.source_sequence, bars)?;
        Ok(Self {
            instrument,
            provenance,
            bar_definition,
            stream,
            evidence,
        })
    }

    /// Builds integrity evidence for an already-provenanced bounded snapshot.
    ///
    /// This is the application boundary for direct provider adapters whose
    /// event provenance is known before the snapshot checksum is constructed.
    ///
    /// # Errors
    ///
    /// Returns an error for empty values, invalid provenance, identity,
    /// sequence, timestamp, generation, or stream bounds.
    pub fn try_from_provenanced_values(
        instrument: InstrumentRevision,
        provenance: ReplayProvenance,
        bar_definition: BarDefinition,
        publication_generation: u64,
        bars: Vec<ProvenancedMarketBar>,
    ) -> Result<Self, ReplayValidationError> {
        let first = bars.first().ok_or(StreamProtocolError::EmptySnapshot)?;
        let last = bars.last().ok_or(StreamProtocolError::EmptySnapshot)?;
        let first_provenance = first.provenance();
        let mut evidence = SnapshotEvidence {
            session_generation: first_provenance.session_generation,
            publication_generation,
            first_sequence: first.value().source_sequence,
            last_sequence: last.value().source_sequence,
            schema_version: first_provenance.schema_version,
            checksum: [0; 32],
        };
        evidence.checksum = snapshot_checksum(&evidence, &instrument, &bar_definition, &bars);
        Self::try_new_provenanced(instrument, provenance, bar_definition, evidence, bars)
    }

    /// Returns the immutable instrument revision associated with every bar.
    #[must_use]
    pub const fn instrument(&self) -> &InstrumentRevision {
        &self.instrument
    }

    /// Returns truthful transport provenance for display and diagnostics.
    #[must_use]
    pub const fn provenance(&self) -> ReplayProvenance {
        self.provenance
    }

    /// Returns the versioned definition used to build this bar series.
    #[must_use]
    pub const fn bar_definition(&self) -> &BarDefinition {
        &self.bar_definition
    }

    /// Returns the validated bounded stream snapshot.
    #[must_use]
    pub const fn stream(&self) -> &StreamSnapshot<ProvenancedMarketBar> {
        &self.stream
    }

    /// Returns the retained snapshot identity and computed integrity evidence.
    #[must_use]
    pub const fn evidence(&self) -> &SnapshotEvidence {
        &self.evidence
    }

    /// Reissues the same validated values under a nonzero publication generation.
    ///
    /// # Errors
    ///
    /// Returns an error when the publication generation is zero or rebuilt evidence is invalid.
    pub fn try_with_publication_generation(
        &self,
        publication_generation: u64,
    ) -> Result<Self, ReplayValidationError> {
        let mut evidence = self.evidence.clone();
        evidence.publication_generation = publication_generation;
        evidence.checksum = snapshot_checksum(
            &evidence,
            &self.instrument,
            &self.bar_definition,
            self.bars(),
        );
        Self::try_new_provenanced(
            self.instrument.clone(),
            self.provenance,
            self.bar_definition.clone(),
            evidence,
            self.bars().to_vec(),
        )
    }

    /// Returns contiguous fixed-point bars with inseparable canonical evidence.
    #[must_use]
    pub fn bars(&self) -> &[ProvenancedMarketBar] {
        self.stream.items()
    }

    /// Returns the inclusive source-sequence range.
    #[must_use]
    pub const fn sequence_range(&self) -> (u64, u64) {
        (self.stream.first_sequence(), self.stream.last_sequence())
    }
}

/// One transport-neutral update delivered to replay consumers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplayStreamUpdate {
    Snapshot(ReplaySnapshot),
    Delta(StreamDelta<ProvenancedMarketBar>),
    Tail(ReplayTailUpdate),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayTailOperation {
    Revise,
    Append,
}

/// One incremental live bar that either replaces the forming tail or appends its successor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayTailUpdate {
    item: ProvenancedMarketBar,
    publication_generation: u64,
    forming: bool,
    operation: ReplayTailOperation,
}

impl ReplayTailUpdate {
    /// Creates one validated live-tail publication.
    ///
    /// # Errors
    /// Returns an error for invalid canonical data, provenance, or a zero publication generation.
    pub fn try_new(
        item: ProvenancedMarketBar,
        publication_generation: u64,
        forming: bool,
        operation: ReplayTailOperation,
    ) -> Result<Self, ReplayValidationError> {
        validate_provenanced_market_bar(&item)?;
        if publication_generation == 0 {
            return Err(ReplayValidationError::InvalidSnapshotEvidence(
                "publication_generation",
            ));
        }
        Ok(Self {
            item,
            publication_generation,
            forming,
            operation,
        })
    }

    #[must_use]
    pub const fn item(&self) -> &ProvenancedMarketBar {
        &self.item
    }

    #[must_use]
    pub const fn publication_generation(&self) -> u64 {
        self.publication_generation
    }

    #[must_use]
    pub const fn forming(&self) -> bool {
        self.forming
    }

    #[must_use]
    pub const fn operation(&self) -> ReplayTailOperation {
        self.operation
    }
}

/// Consumer-side replay state that never speculates across sequence gaps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplaySession {
    sequence_tracker: SequenceTracker,
    last_exchange_timestamp_unix_nanos: i64,
}

impl ReplaySession {
    /// Installs an initial snapshot as the accepted replay baseline.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot establish resumable sequence state.
    pub fn try_new(snapshot: &ReplaySnapshot) -> Result<Self, ReplayValidationError> {
        let mut sequence_tracker = SequenceTracker::default();
        sequence_tracker.install_snapshot(snapshot.stream())?;
        let last_exchange_timestamp_unix_nanos = snapshot
            .bars()
            .last()
            .ok_or(StreamProtocolError::EmptySnapshot)?
            .provenance()
            .exchange_timestamp_unix_nanos;
        Ok(Self {
            sequence_tracker,
            last_exchange_timestamp_unix_nanos,
        })
    }

    /// Replaces current stream state with a fresh validated snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot establish resumable sequence state.
    pub fn install_snapshot(
        &mut self,
        snapshot: &ReplaySnapshot,
    ) -> Result<(), ReplayValidationError> {
        *self = Self::try_new(snapshot)?;
        Ok(())
    }

    /// Validates and classifies one delta against the accepted replay state.
    ///
    /// Accepted deltas advance state. Duplicates do not mutate state. A gap
    /// permanently requires a fresh snapshot before later deltas can be accepted.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid market data, a non-increasing timestamp on
    /// the next contiguous delta, or sequence overflow.
    pub fn accept_delta(
        &mut self,
        delta: &StreamDelta<ProvenancedMarketBar>,
    ) -> Result<SequenceDecision, ReplayValidationError> {
        validate_provenanced_market_bar(delta.item())?;
        let mut candidate = self.sequence_tracker;
        let decision = candidate.accept_delta(delta)?;
        match decision {
            SequenceDecision::Accepted => {
                if delta.item().provenance().exchange_timestamp_unix_nanos
                    <= self.last_exchange_timestamp_unix_nanos
                {
                    return Err(ReplayValidationError::NonIncreasingTimestamp {
                        source_sequence: delta.sequence(),
                    });
                }
                self.sequence_tracker = candidate;
                self.last_exchange_timestamp_unix_nanos =
                    delta.item().provenance().exchange_timestamp_unix_nanos;
            }
            SequenceDecision::Gap { .. } | SequenceDecision::SnapshotRequired => {
                self.sequence_tracker = candidate;
            }
            SequenceDecision::Duplicate => {}
        }
        Ok(decision)
    }

    /// Validates one append-or-replace live tail against the accepted covering state.
    ///
    /// # Errors
    /// Returns an error when canonical data is invalid or a replacement changes bar time.
    pub fn accept_tail(
        &mut self,
        update: &ReplayTailUpdate,
    ) -> Result<SequenceDecision, ReplayValidationError> {
        validate_provenanced_market_bar(update.item())?;
        let Some(expected) = self.sequence_tracker.expected_sequence() else {
            return Ok(SequenceDecision::SnapshotRequired);
        };
        let current = expected.saturating_sub(1);
        let sequence = update.item().value().source_sequence;
        if sequence < current {
            return Ok(SequenceDecision::Duplicate);
        }
        if sequence == current {
            if update.operation() != ReplayTailOperation::Revise {
                return Err(ReplayValidationError::TailOperationMismatch {
                    source_sequence: sequence,
                });
            }
            if update.item().provenance().exchange_timestamp_unix_nanos
                != self.last_exchange_timestamp_unix_nanos
            {
                return Err(ReplayValidationError::TailTimestampChanged {
                    source_sequence: sequence,
                });
            }
            return Ok(SequenceDecision::Accepted);
        }
        if update.operation() != ReplayTailOperation::Append {
            return Err(ReplayValidationError::TailOperationMismatch {
                source_sequence: sequence,
            });
        }
        let delta =
            StreamDelta::try_new(sequence.saturating_sub(1), sequence, update.item().clone())?;
        self.accept_delta(&delta)
    }

    /// Returns the next source sequence expected by this session.
    #[must_use]
    pub const fn expected_sequence(self) -> Option<u64> {
        self.sequence_tracker.expected_sequence()
    }

    /// Returns whether a fresh snapshot is required before accepting deltas.
    #[must_use]
    pub const fn requires_snapshot(self) -> bool {
        self.sequence_tracker.requires_snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EmbeddedReplaySource, LoadEmbeddedReplay, Provenanced};

    #[test]
    fn snapshot_orders_bars_by_exact_provenance_time_within_one_second() {
        let baseline = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
            .expect("fixture snapshot");
        let first = baseline.bars()[0].clone();
        let mut second_bar = *baseline.bars()[1].value();
        second_bar.exchange_timestamp_seconds = first.value().exchange_timestamp_seconds;
        second_bar.exchange_timestamp_unix_nanos =
            first.value().exchange_timestamp_unix_nanos + 500_000_000;
        let mut second_provenance = baseline.bars()[1].provenance().clone();
        second_provenance.exchange_timestamp_unix_nanos = second_bar.exchange_timestamp_unix_nanos;
        let second = Provenanced::new(second_bar, second_provenance);

        let snapshot = ReplaySnapshot::try_from_provenanced_values(
            baseline.instrument().clone(),
            baseline.provenance(),
            baseline.bar_definition().clone(),
            2,
            vec![first, second],
        )
        .expect("subsecond ordering is retained");
        assert_eq!(
            snapshot.bars()[0].value().exchange_timestamp_seconds,
            snapshot.bars()[1].value().exchange_timestamp_seconds
        );
    }

    #[test]
    fn live_tail_replaces_in_place_then_appends_contiguously() {
        let baseline = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
            .expect("fixture snapshot");
        let mut session = ReplaySession::try_new(&baseline).expect("session starts");
        let mut replacement_bar = *baseline.bars()[1].value();
        replacement_bar.close = replacement_bar.close.saturating_add(1);
        let replacement = ReplayTailUpdate::try_new(
            Provenanced::new(replacement_bar, baseline.bars()[1].provenance().clone()),
            baseline.evidence().publication_generation + 1,
            true,
            ReplayTailOperation::Revise,
        )
        .expect("replacement validates");
        assert_eq!(
            session.accept_tail(&replacement).expect("tail replaces"),
            SequenceDecision::Accepted
        );
        assert_eq!(
            session.expected_sequence(),
            baseline.evidence().last_sequence.checked_add(1)
        );

        let appended = EmbeddedReplaySource
            .load_delta(baseline.evidence().last_sequence)
            .expect("fixture delta loads")
            .expect("fixture delta exists");
        let appended = ReplayTailUpdate::try_new(
            appended.item().clone(),
            baseline.evidence().publication_generation + 2,
            true,
            ReplayTailOperation::Append,
        )
        .expect("append validates");
        assert_eq!(
            session.accept_tail(&appended).expect("tail appends"),
            SequenceDecision::Accepted
        );
        assert_eq!(
            session.expected_sequence(),
            appended.item().value().source_sequence.checked_add(1)
        );
    }
}
