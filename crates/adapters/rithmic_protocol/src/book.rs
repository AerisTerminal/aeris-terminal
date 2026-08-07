use crate::{
    MarketIdentity, OrderBookLevel, OrderBookUpdate, OrderBookUpdateKind, ProviderTimestamp,
};
use std::{error::Error, fmt, num::NonZeroUsize};

const MAX_LEVELS_PER_SIDE: usize = 4_096;
const MAX_CHUNKS_PER_IMAGE: usize = 64;

/// Invalid aggregate-book assembler configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AggregateBookConfigError;

impl fmt::Display for AggregateBookConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid Rithmic aggregate-book assembly limits")
    }
}

impl Error for AggregateBookConfigError {}

/// Fixed memory and chunk bounds for one aggregate-book image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AggregateBookLimits {
    maximum_levels_per_side: NonZeroUsize,
    maximum_chunks: NonZeroUsize,
}

impl AggregateBookLimits {
    /// Creates validated image assembly limits.
    ///
    /// # Errors
    ///
    /// Returns an error above the protocol hard limits.
    pub fn try_new(
        maximum_levels_per_side: NonZeroUsize,
        maximum_chunks: NonZeroUsize,
    ) -> Result<Self, AggregateBookConfigError> {
        if maximum_levels_per_side.get() > MAX_LEVELS_PER_SIDE
            || maximum_chunks.get() > MAX_CHUNKS_PER_IMAGE
        {
            return Err(AggregateBookConfigError);
        }
        Ok(Self {
            maximum_levels_per_side,
            maximum_chunks,
        })
    }
}

/// Complete provider aggregate-book image ready for canonical conversion.
#[derive(Clone, Debug, PartialEq)]
pub struct AggregateBookImage {
    pub identity: MarketIdentity,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub timestamp: Option<ProviderTimestamp>,
    /// Connection-local reception order, not a provider sequence number.
    pub source_ordinal: u64,
}

/// Why a partial or complete provider image was discarded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AggregateBookRecoveryReason {
    ProviderReset,
    MissingBegin,
    NestedBegin,
    IdentityMismatch,
    TimestampMismatch,
    NonContiguousChunk,
    StaleOrdinal,
    ChunkLimitExceeded,
    LevelLimitExceeded,
    InvalidLevel,
    DuplicatePrice,
    CrossedBook,
}

/// One state transition from aggregate-book assembly.
#[derive(Clone, Debug, PartialEq)]
pub enum AggregateBookOutcome {
    Pending,
    Snapshot(AggregateBookImage),
    RecoveryRequired {
        observed_ordinal: u64,
        reason: AggregateBookRecoveryReason,
    },
    Unavailable {
        observed_ordinal: u64,
    },
}

struct CandidateImage {
    timestamp: Option<ProviderTimestamp>,
    bids: Vec<OrderBookLevel>,
    asks: Vec<OrderBookLevel>,
    chunks: usize,
    last_ordinal: u64,
}

/// Assembles provider chunks without inventing depth-delta semantics.
pub struct AggregateBookAssembler {
    identity: MarketIdentity,
    limits: AggregateBookLimits,
    candidate: Option<CandidateImage>,
    last_observed_ordinal: u64,
}

impl AggregateBookAssembler {
    #[must_use]
    pub const fn new(identity: MarketIdentity, limits: AggregateBookLimits) -> Self {
        Self {
            identity,
            limits,
            candidate: None,
            last_observed_ordinal: 0,
        }
    }

    /// Applies one aggregate-book frame at its checked connection-local ordinal.
    #[must_use]
    pub fn accept(
        &mut self,
        update: OrderBookUpdate,
        observed_ordinal: u64,
    ) -> AggregateBookOutcome {
        if observed_ordinal == 0 || observed_ordinal <= self.last_observed_ordinal {
            return self.recover(observed_ordinal, AggregateBookRecoveryReason::StaleOrdinal);
        }
        self.last_observed_ordinal = observed_ordinal;
        if update.identity != self.identity {
            return self.recover(
                observed_ordinal,
                AggregateBookRecoveryReason::IdentityMismatch,
            );
        }

        match update.kind {
            OrderBookUpdateKind::Clear => {
                self.recover(observed_ordinal, AggregateBookRecoveryReason::ProviderReset)
            }
            OrderBookUpdateKind::Unavailable => {
                self.candidate = None;
                AggregateBookOutcome::Unavailable { observed_ordinal }
            }
            OrderBookUpdateKind::Snapshot | OrderBookUpdateKind::Solo => {
                self.candidate = None;
                self.complete_standalone(update, observed_ordinal)
            }
            OrderBookUpdateKind::Begin => self.begin(update, observed_ordinal),
            OrderBookUpdateKind::Middle => self.append(update, observed_ordinal, false),
            OrderBookUpdateKind::End => self.append(update, observed_ordinal, true),
        }
    }

    /// Discards any partial image when its owning session ends.
    pub fn reset(&mut self) {
        self.candidate = None;
        self.last_observed_ordinal = 0;
    }

    fn begin(&mut self, update: OrderBookUpdate, observed_ordinal: u64) -> AggregateBookOutcome {
        if self.candidate.is_some() {
            return self.recover(observed_ordinal, AggregateBookRecoveryReason::NestedBegin);
        }
        if let Some(reason) = self.validate_chunk(&update, 1, 0, 0) {
            return self.recover(observed_ordinal, reason);
        }
        self.candidate = Some(CandidateImage {
            timestamp: update.timestamp,
            bids: update.bids,
            asks: update.asks,
            chunks: 1,
            last_ordinal: observed_ordinal,
        });
        AggregateBookOutcome::Pending
    }

    fn append(
        &mut self,
        update: OrderBookUpdate,
        observed_ordinal: u64,
        complete: bool,
    ) -> AggregateBookOutcome {
        let Some(candidate) = self.candidate.as_ref() else {
            return self.recover(observed_ordinal, AggregateBookRecoveryReason::MissingBegin);
        };
        if candidate.timestamp != update.timestamp {
            return self.recover(
                observed_ordinal,
                AggregateBookRecoveryReason::TimestampMismatch,
            );
        }
        if candidate.last_ordinal.checked_add(1) != Some(observed_ordinal) {
            return self.recover(
                observed_ordinal,
                AggregateBookRecoveryReason::NonContiguousChunk,
            );
        }
        if let Some(reason) = self.validate_chunk(
            &update,
            candidate.chunks.saturating_add(1),
            candidate.bids.len(),
            candidate.asks.len(),
        ) {
            return self.recover(observed_ordinal, reason);
        }

        let Some(candidate) = self.candidate.as_mut() else {
            return self.recover(observed_ordinal, AggregateBookRecoveryReason::MissingBegin);
        };
        candidate.bids.extend(update.bids);
        candidate.asks.extend(update.asks);
        candidate.chunks += 1;
        candidate.last_ordinal = observed_ordinal;
        if !complete {
            return AggregateBookOutcome::Pending;
        }
        let Some(candidate) = self.candidate.take() else {
            return self.recover(observed_ordinal, AggregateBookRecoveryReason::MissingBegin);
        };
        self.finish(
            candidate.bids,
            candidate.asks,
            candidate.timestamp,
            observed_ordinal,
        )
    }

    fn complete_standalone(
        &mut self,
        update: OrderBookUpdate,
        observed_ordinal: u64,
    ) -> AggregateBookOutcome {
        if let Some(reason) = self.validate_chunk(&update, 1, 0, 0) {
            return self.recover(observed_ordinal, reason);
        }
        self.finish(update.bids, update.asks, update.timestamp, observed_ordinal)
    }

    fn validate_chunk(
        &self,
        update: &OrderBookUpdate,
        chunks: usize,
        existing_bids: usize,
        existing_asks: usize,
    ) -> Option<AggregateBookRecoveryReason> {
        if chunks > self.limits.maximum_chunks.get() {
            return Some(AggregateBookRecoveryReason::ChunkLimitExceeded);
        }
        if existing_bids.saturating_add(update.bids.len())
            > self.limits.maximum_levels_per_side.get()
            || existing_asks.saturating_add(update.asks.len())
                > self.limits.maximum_levels_per_side.get()
        {
            return Some(AggregateBookRecoveryReason::LevelLimitExceeded);
        }
        if update
            .bids
            .iter()
            .chain(&update.asks)
            .any(|level| !level.price.is_finite() || level.price <= 0.0 || level.size == 0)
        {
            return Some(AggregateBookRecoveryReason::InvalidLevel);
        }
        None
    }

    fn finish(
        &mut self,
        mut bids: Vec<OrderBookLevel>,
        mut asks: Vec<OrderBookLevel>,
        timestamp: Option<ProviderTimestamp>,
        observed_ordinal: u64,
    ) -> AggregateBookOutcome {
        bids.sort_by(|left, right| right.price.total_cmp(&left.price));
        asks.sort_by(|left, right| left.price.total_cmp(&right.price));
        if has_duplicate_prices(&bids) || has_duplicate_prices(&asks) {
            return self.recover(
                observed_ordinal,
                AggregateBookRecoveryReason::DuplicatePrice,
            );
        }
        if bids
            .first()
            .zip(asks.first())
            .is_some_and(|(bid, ask)| bid.price >= ask.price)
        {
            return self.recover(observed_ordinal, AggregateBookRecoveryReason::CrossedBook);
        }
        AggregateBookOutcome::Snapshot(AggregateBookImage {
            identity: self.identity.clone(),
            bids,
            asks,
            timestamp,
            source_ordinal: observed_ordinal,
        })
    }

    fn recover(
        &mut self,
        observed_ordinal: u64,
        reason: AggregateBookRecoveryReason,
    ) -> AggregateBookOutcome {
        self.candidate = None;
        AggregateBookOutcome::RecoveryRequired {
            observed_ordinal,
            reason,
        }
    }
}

fn has_duplicate_prices(levels: &[OrderBookLevel]) -> bool {
    levels
        .windows(2)
        .any(|pair| pair[0].price.total_cmp(&pair[1].price).is_eq())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OrderBookSides;

    #[test]
    fn standalone_images_are_sorted_and_stamped_with_local_ordinal() {
        let mut assembler = new_assembler(4, 4);
        let result = assembler.accept(
            update(
                OrderBookUpdateKind::Solo,
                &[99.0, 100.0],
                &[102.0, 101.0],
                timestamp(1),
            ),
            7,
        );
        assert!(matches!(
            result,
            AggregateBookOutcome::Snapshot(AggregateBookImage {
                bids,
                asks,
                source_ordinal: 7,
                ..
            }) if prices(&bids) == [100.0, 99.0] && prices(&asks) == [101.0, 102.0]
        ));
    }

    #[test]
    fn begin_middle_end_publishes_only_one_complete_image() {
        let mut assembler = new_assembler(4, 3);
        assert_eq!(
            assembler.accept(
                update(OrderBookUpdateKind::Begin, &[100.0], &[], timestamp(1)),
                10,
            ),
            AggregateBookOutcome::Pending
        );
        assert_eq!(
            assembler.accept(
                update(OrderBookUpdateKind::Middle, &[99.0], &[101.0], timestamp(1)),
                11,
            ),
            AggregateBookOutcome::Pending
        );
        assert!(matches!(
            assembler.accept(
                update(OrderBookUpdateKind::End, &[], &[102.0], timestamp(1)),
                12,
            ),
            AggregateBookOutcome::Snapshot(AggregateBookImage { bids, asks, .. })
                if prices(&bids) == [100.0, 99.0] && prices(&asks) == [101.0, 102.0]
        ));
    }

    #[test]
    fn clear_and_unavailable_never_publish_empty_snapshots() {
        let mut assembler = new_assembler(4, 3);
        let _ = assembler.accept(
            update(OrderBookUpdateKind::Begin, &[100.0], &[], timestamp(1)),
            1,
        );
        assert_eq!(
            assembler.accept(
                update(OrderBookUpdateKind::Clear, &[], &[], timestamp(2)),
                2,
            ),
            AggregateBookOutcome::RecoveryRequired {
                observed_ordinal: 2,
                reason: AggregateBookRecoveryReason::ProviderReset,
            }
        );
        assert_eq!(
            assembler.accept(
                update(OrderBookUpdateKind::Unavailable, &[], &[], timestamp(3)),
                3,
            ),
            AggregateBookOutcome::Unavailable {
                observed_ordinal: 3,
            }
        );
    }

    #[test]
    fn malformed_chunk_sequences_discard_partial_state() {
        let mut assembler = new_assembler(4, 3);
        assert!(matches!(
            assembler.accept(
                update(OrderBookUpdateKind::Middle, &[100.0], &[], timestamp(1)),
                1,
            ),
            AggregateBookOutcome::RecoveryRequired {
                reason: AggregateBookRecoveryReason::MissingBegin,
                ..
            }
        ));
        let _ = assembler.accept(
            update(OrderBookUpdateKind::Begin, &[100.0], &[], timestamp(1)),
            2,
        );
        assert!(matches!(
            assembler.accept(
                update(OrderBookUpdateKind::End, &[], &[101.0], timestamp(1)),
                4,
            ),
            AggregateBookOutcome::RecoveryRequired {
                reason: AggregateBookRecoveryReason::NonContiguousChunk,
                ..
            }
        ));
        assert!(matches!(
            assembler.accept(
                update(OrderBookUpdateKind::End, &[], &[101.0], timestamp(1)),
                5,
            ),
            AggregateBookOutcome::RecoveryRequired {
                reason: AggregateBookRecoveryReason::MissingBegin,
                ..
            }
        ));
    }

    #[test]
    fn limits_duplicates_crossing_and_identity_fail_closed() {
        let mut assembler = new_assembler(1, 1);
        assert!(matches!(
            assembler.accept(
                update(OrderBookUpdateKind::Solo, &[100.0, 99.0], &[], timestamp(1)),
                1,
            ),
            AggregateBookOutcome::RecoveryRequired {
                reason: AggregateBookRecoveryReason::LevelLimitExceeded,
                ..
            }
        ));
        assert!(matches!(
            assembler.accept(
                update(OrderBookUpdateKind::Solo, &[100.0], &[100.0], timestamp(1)),
                2,
            ),
            AggregateBookOutcome::RecoveryRequired {
                reason: AggregateBookRecoveryReason::CrossedBook,
                ..
            }
        ));
        let mut wrong = update(OrderBookUpdateKind::Solo, &[100.0], &[101.0], timestamp(1));
        wrong.identity.symbol = "NQM7".to_string();
        assert!(matches!(
            assembler.accept(wrong, 3),
            AggregateBookOutcome::RecoveryRequired {
                reason: AggregateBookRecoveryReason::IdentityMismatch,
                ..
            }
        ));

        let mut duplicate_assembler = new_assembler(2, 1);
        assert!(matches!(
            duplicate_assembler.accept(
                update(
                    OrderBookUpdateKind::Solo,
                    &[100.0, 100.0],
                    &[],
                    timestamp(1)
                ),
                1,
            ),
            AggregateBookOutcome::RecoveryRequired {
                reason: AggregateBookRecoveryReason::DuplicatePrice,
                ..
            }
        ));
    }

    fn new_assembler(maximum_levels: usize, maximum_chunks: usize) -> AggregateBookAssembler {
        AggregateBookAssembler::new(
            identity(),
            AggregateBookLimits::try_new(
                NonZeroUsize::new(maximum_levels).expect("nonzero levels"),
                NonZeroUsize::new(maximum_chunks).expect("nonzero chunks"),
            )
            .expect("valid limits"),
        )
    }

    fn update(
        kind: OrderBookUpdateKind,
        bids: &[f64],
        asks: &[f64],
        timestamp: ProviderTimestamp,
    ) -> OrderBookUpdate {
        OrderBookUpdate {
            identity: identity(),
            kind,
            present_sides: OrderBookSides {
                bids: !bids.is_empty(),
                asks: !asks.is_empty(),
            },
            bids: bids.iter().copied().map(level).collect(),
            asks: asks.iter().copied().map(level).collect(),
            timestamp: Some(timestamp),
        }
    }

    fn identity() -> MarketIdentity {
        MarketIdentity {
            symbol: "ESM7".to_string(),
            exchange: "CME".to_string(),
        }
    }

    const fn timestamp(microseconds: i32) -> ProviderTimestamp {
        ProviderTimestamp {
            seconds: 1_800_000_000,
            microseconds,
        }
    }

    const fn level(price: f64) -> OrderBookLevel {
        OrderBookLevel {
            price,
            size: 1,
            orders: Some(1),
            implied_size: None,
        }
    }

    fn prices(levels: &[OrderBookLevel]) -> Vec<f64> {
        levels.iter().map(|level| level.price).collect()
    }
}
