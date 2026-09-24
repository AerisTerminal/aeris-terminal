//! Immutable model generations and the single-writer client model.

use crate::errors::ReplayValidationError;
use crate::provenance::ProvenancedMarketBar;
use crate::replay_snapshot::{ReplaySession, ReplaySnapshot, ReplayStreamUpdate, ReplayTailUpdate};
use crate::stream::{SequenceDecision, StreamDelta, StreamProtocolError};
use aeris_instruments::InstrumentRevision;
use aeris_market_data::BarDefinition;
use std::{num::NonZeroU64, sync::Arc};

/// Immutable application generation published atomically by one model writer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketGeneration<T> {
    session_generation: NonZeroU64,
    publication_generation: NonZeroU64,
    first_sequence: NonZeroU64,
    last_sequence: NonZeroU64,
    items: Arc<[T]>,
}

impl<T> MarketGeneration<T> {
    /// Creates a non-empty, contiguous immutable generation.
    ///
    /// # Errors
    ///
    /// Returns an error for zero values, an empty generation, or a sequence/count mismatch.
    pub fn try_new(
        session_generation: u64,
        publication_generation: u64,
        first_sequence: u64,
        last_sequence: u64,
        items: Vec<T>,
    ) -> Result<Self, StreamProtocolError> {
        let session_generation =
            NonZeroU64::new(session_generation).ok_or(StreamProtocolError::ZeroSequence)?;
        let publication_generation =
            NonZeroU64::new(publication_generation).ok_or(StreamProtocolError::ZeroSequence)?;
        let first_sequence =
            NonZeroU64::new(first_sequence).ok_or(StreamProtocolError::ZeroSequence)?;
        let last_sequence =
            NonZeroU64::new(last_sequence).ok_or(StreamProtocolError::ZeroSequence)?;
        if items.is_empty() {
            return Err(StreamProtocolError::EmptySnapshot);
        }
        let expected = last_sequence
            .get()
            .checked_sub(first_sequence.get())
            .and_then(|difference| difference.checked_add(1))
            .ok_or(StreamProtocolError::InvalidSequenceRange {
                first: first_sequence.get(),
                last: last_sequence.get(),
            })?;
        if expected != u64::try_from(items.len()).unwrap_or(u64::MAX) {
            return Err(StreamProtocolError::ItemCountMismatch {
                expected,
                actual: items.len(),
            });
        }
        Ok(Self {
            session_generation,
            publication_generation,
            first_sequence,
            last_sequence,
            items: items.into(),
        })
    }

    #[must_use]
    pub const fn session_generation(&self) -> u64 {
        self.session_generation.get()
    }

    #[must_use]
    pub const fn publication_generation(&self) -> u64 {
        self.publication_generation.get()
    }

    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }

    #[must_use]
    pub const fn sequence_range(&self) -> (u64, u64) {
        (self.first_sequence.get(), self.last_sequence.get())
    }
}

/// Result of applying one validated market-bar update to the client model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketBarModelOutcome {
    /// A new immutable generation is ready for atomic publication to readers.
    Published(MarketGeneration<ProvenancedMarketBar>),
    /// The update was already represented by the current generation.
    Duplicate,
    /// Ordered mutation is blocked until a replacement snapshot is installed.
    ResnapshotRequired(ResnapshotReason),
}

/// Bounded single-writer client projection for one local market-bar series.
///
/// The owner feeds transport-neutral snapshots and deltas into this model from one
/// background worker. Readers receive cloned immutable generations and never observe
/// partially applied snapshots, deltas, session changes, or history eviction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketBarClientModel {
    maximum_items: std::num::NonZeroUsize,
    session: Option<ReplaySession>,
    current: Option<MarketGeneration<ProvenancedMarketBar>>,
    current_instrument: Option<InstrumentRevision>,
    current_bar_definition: Option<BarDefinition>,
    pending_resnapshot: Option<ResnapshotReason>,
}

impl MarketBarClientModel {
    /// Creates an empty model with an explicit retained-history bound.
    #[must_use]
    pub const fn new(maximum_items: std::num::NonZeroUsize) -> Self {
        Self {
            maximum_items,
            session: None,
            current: None,
            current_instrument: None,
            current_bar_definition: None,
            pending_resnapshot: Some(ResnapshotReason::InitialSubscription),
        }
    }

    /// Applies one validated update atomically and publishes at most one generation.
    ///
    /// Accepted deltas are validated in order before bounded oldest-item eviction.
    /// Gaps and session changes preserve the last immutable generation for readers
    /// but block all subsequent deltas until a replacement snapshot is installed.
    ///
    /// # Errors
    ///
    /// Returns an error when replay validation fails or when the immutable
    /// generation counter overflows.
    pub fn apply_update(
        &mut self,
        update: ReplayStreamUpdate,
    ) -> Result<MarketBarModelOutcome, ReplayValidationError> {
        match update {
            ReplayStreamUpdate::Snapshot(snapshot) => self.install_snapshot(&snapshot),
            ReplayStreamUpdate::Delta(delta) => self.apply_delta(&delta),
            ReplayStreamUpdate::Tail(update) => self.apply_tail(&update),
        }
    }

    fn validate_snapshot_transition(
        &self,
        snapshot: &ReplaySnapshot,
    ) -> Result<(), ReplayValidationError> {
        let Some(current) = self.current.as_ref() else {
            return Ok(());
        };
        let evidence = snapshot.evidence();
        if self.current_instrument.as_ref() != Some(snapshot.instrument())
            || self.current_bar_definition.as_ref() != Some(snapshot.bar_definition())
        {
            return Err(ReplayValidationError::SnapshotEvidenceMismatch(
                "series_identity",
            ));
        }
        if evidence.session_generation < current.session_generation() {
            return Err(ReplayValidationError::SnapshotSessionGenerationRegression {
                current_generation: current.session_generation(),
                actual_generation: evidence.session_generation,
            });
        }
        if evidence.session_generation == current.session_generation()
            && (evidence.publication_generation <= current.publication_generation()
                || evidence.last_sequence < current.sequence_range().1)
        {
            return Err(ReplayValidationError::StaleSnapshot {
                current_generation: current.publication_generation(),
                current_last_sequence: current.sequence_range().1,
                actual_generation: evidence.publication_generation,
                actual_last_sequence: evidence.last_sequence,
            });
        }
        Ok(())
    }

    fn install_snapshot(
        &mut self,
        snapshot: &ReplaySnapshot,
    ) -> Result<MarketBarModelOutcome, ReplayValidationError> {
        self.validate_snapshot_transition(snapshot)?;
        let session = ReplaySession::try_new(snapshot)?;
        let evidence = snapshot.evidence();
        let retained_start = snapshot
            .bars()
            .len()
            .saturating_sub(self.maximum_items.get());
        let retained_items = snapshot.bars()[retained_start..].to_vec();
        let first_sequence = retained_items
            .first()
            .ok_or(StreamProtocolError::EmptySnapshot)?
            .value()
            .source_sequence;
        let generation = MarketGeneration::try_new(
            evidence.session_generation,
            evidence.publication_generation,
            first_sequence,
            evidence.last_sequence,
            retained_items,
        )?;
        self.session = Some(session);
        self.current = Some(generation.clone());
        self.current_instrument = Some(snapshot.instrument().clone());
        self.current_bar_definition = Some(snapshot.bar_definition().clone());
        self.pending_resnapshot = None;
        Ok(MarketBarModelOutcome::Published(generation))
    }

    fn apply_delta(
        &mut self,
        delta: &StreamDelta<ProvenancedMarketBar>,
    ) -> Result<MarketBarModelOutcome, ReplayValidationError> {
        let Some(current) = self.current.as_ref() else {
            return Ok(MarketBarModelOutcome::ResnapshotRequired(
                self.pending_resnapshot
                    .unwrap_or(ResnapshotReason::InitialSubscription),
            ));
        };
        let provenance = delta.item().provenance();
        if provenance.session_generation != current.session_generation() {
            self.session = None;
            self.pending_resnapshot = Some(ResnapshotReason::SessionChanged);
            return Ok(MarketBarModelOutcome::ResnapshotRequired(
                ResnapshotReason::SessionChanged,
            ));
        }
        let current_schema_version = current
            .items()
            .first()
            .ok_or(StreamProtocolError::EmptySnapshot)?
            .provenance()
            .schema_version;
        if provenance.schema_version != current_schema_version {
            self.session = None;
            self.pending_resnapshot = Some(ResnapshotReason::SchemaChanged);
            return Ok(MarketBarModelOutcome::ResnapshotRequired(
                ResnapshotReason::SchemaChanged,
            ));
        }
        let Some(mut candidate_session) = self.session else {
            return Ok(MarketBarModelOutcome::ResnapshotRequired(
                self.pending_resnapshot
                    .unwrap_or(ResnapshotReason::SequenceGap),
            ));
        };
        match candidate_session.accept_delta(delta)? {
            SequenceDecision::Duplicate => Ok(MarketBarModelOutcome::Duplicate),
            SequenceDecision::Gap { .. } | SequenceDecision::SnapshotRequired => {
                self.session = Some(candidate_session);
                let reason = self
                    .pending_resnapshot
                    .unwrap_or(ResnapshotReason::SequenceGap);
                self.pending_resnapshot = Some(reason);
                Ok(MarketBarModelOutcome::ResnapshotRequired(reason))
            }
            SequenceDecision::Accepted => {
                let next_generation = current
                    .publication_generation()
                    .checked_add(1)
                    .ok_or(StreamProtocolError::SequenceOverflow)?;
                let mut items = current.items().to_vec();
                items.push(delta.item().clone());
                let evicted = items.len().saturating_sub(self.maximum_items.get());
                items.drain(..evicted);
                let first_sequence = items
                    .first()
                    .ok_or(StreamProtocolError::EmptySnapshot)?
                    .value()
                    .source_sequence;
                let last_sequence = items
                    .last()
                    .ok_or(StreamProtocolError::EmptySnapshot)?
                    .value()
                    .source_sequence;
                let generation = MarketGeneration::try_new(
                    current.session_generation(),
                    next_generation,
                    first_sequence,
                    last_sequence,
                    items,
                )?;
                self.session = Some(candidate_session);
                self.current = Some(generation.clone());
                self.pending_resnapshot = None;
                Ok(MarketBarModelOutcome::Published(generation))
            }
        }
    }

    fn apply_tail(
        &mut self,
        update: &ReplayTailUpdate,
    ) -> Result<MarketBarModelOutcome, ReplayValidationError> {
        let Some(current) = self.current.as_ref() else {
            return Ok(MarketBarModelOutcome::ResnapshotRequired(
                self.pending_resnapshot
                    .unwrap_or(ResnapshotReason::InitialSubscription),
            ));
        };
        if update.item().provenance().session_generation != current.session_generation() {
            self.session = None;
            self.pending_resnapshot = Some(ResnapshotReason::SessionChanged);
            return Ok(MarketBarModelOutcome::ResnapshotRequired(
                ResnapshotReason::SessionChanged,
            ));
        }
        let Some(mut candidate_session) = self.session else {
            return Ok(MarketBarModelOutcome::ResnapshotRequired(
                self.pending_resnapshot
                    .unwrap_or(ResnapshotReason::SequenceGap),
            ));
        };
        let decision = candidate_session.accept_tail(update)?;
        match decision {
            SequenceDecision::Duplicate => Ok(MarketBarModelOutcome::Duplicate),
            SequenceDecision::Gap { .. } | SequenceDecision::SnapshotRequired => {
                self.session = Some(candidate_session);
                self.pending_resnapshot = Some(ResnapshotReason::SequenceGap);
                Ok(MarketBarModelOutcome::ResnapshotRequired(
                    ResnapshotReason::SequenceGap,
                ))
            }
            SequenceDecision::Accepted => {
                let mut items = current.items().to_vec();
                let sequence = update.item().value().source_sequence;
                if current.sequence_range().1 == sequence {
                    if let Some(last) = items.last_mut() {
                        *last = update.item().clone();
                    }
                } else {
                    items.push(update.item().clone());
                }
                let evicted = items.len().saturating_sub(self.maximum_items.get());
                items.drain(..evicted);
                let first_sequence = items
                    .first()
                    .ok_or(StreamProtocolError::EmptySnapshot)?
                    .value()
                    .source_sequence;
                let last_sequence = items
                    .last()
                    .ok_or(StreamProtocolError::EmptySnapshot)?
                    .value()
                    .source_sequence;
                let generation = MarketGeneration::try_new(
                    current.session_generation(),
                    update.publication_generation(),
                    first_sequence,
                    last_sequence,
                    items,
                )?;
                self.session = Some(candidate_session);
                self.current = Some(generation.clone());
                self.pending_resnapshot = None;
                Ok(MarketBarModelOutcome::Published(generation))
            }
        }
    }

    /// Explicitly blocks ordered deltas while preserving the last immutable generation.
    ///
    /// Connected adapters call this after transport loss, protocol failure, or bounded
    /// queue overflow. A subsequent validated snapshot atomically clears the latch.
    pub fn require_resnapshot(&mut self, reason: ResnapshotReason) {
        self.session = None;
        self.pending_resnapshot = Some(reason);
    }

    /// Returns the latest immutable generation, if an initial snapshot was installed.
    #[must_use]
    pub const fn current_generation(&self) -> Option<&MarketGeneration<ProvenancedMarketBar>> {
        self.current.as_ref()
    }

    /// Returns whether ordered updates are blocked pending a replacement snapshot.
    #[must_use]
    pub const fn requires_snapshot(&self) -> bool {
        self.pending_resnapshot.is_some()
    }

    /// Returns the active recovery reason, if mutation is currently blocked.
    #[must_use]
    pub const fn pending_resnapshot_reason(&self) -> Option<ResnapshotReason> {
        self.pending_resnapshot
    }
}

/// Reason a consumer requests a bounded atomic replacement snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResnapshotReason {
    InitialSubscription,
    SequenceGap,
    SchemaChanged,
    QueueOverflow,
    SessionChanged,
    TransportReset,
}

/// Correlated command for one bounded background snapshot recovery attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplayRecoveryCommand {
    pub request_id: u64,
    pub reason: ResnapshotReason,
}
