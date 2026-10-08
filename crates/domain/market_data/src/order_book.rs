use crate::{
    BookSide, DepthDelta, DepthLevel, DepthSnapshot, MarketDataValidationError, QuoteLevel,
};
use std::{collections::BTreeMap, num::NonZeroUsize};

/// Recent real aggressor-side traded quantity accumulated at one price.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AggressorTradeVolumes {
    pub buy: i64,
    pub sell: i64,
}

impl AggressorTradeVolumes {
    #[must_use]
    pub const fn maximum_side(self) -> i64 {
        if self.buy > self.sell {
            self.buy
        } else {
            self.sell
        }
    }
}

/// Coarse reason that candidate order-book state was discarded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderBookRecoveryReason {
    AwaitingSnapshot,
    SequenceGap,
    CrossedBook,
    InvalidUpdate,
}

/// Current recoverability state of one order book.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderBookState {
    Recovering(OrderBookRecoveryReason),
    Ready,
    Stale,
}

/// Immutable bounded top-N book image for publication to models and UI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderBookPublication {
    pub provider_id: String,
    pub instrument_id: String,
    pub entitlement_id: String,
    pub session_generation: u64,
    pub revision: u64,
    pub source_watermark: u64,
    /// Latest independently observed provider BBO. These fields never replace
    /// aggregate/MBO depth; they are published alongside it so the UI can show
    /// top-of-book even while a covering depth image is still pending.
    pub best_bid: Option<QuoteLevel>,
    pub best_ask: Option<QuoteLevel>,
    pub bbo_source_watermark: u64,
    pub bids: Vec<DepthLevel>,
    pub asks: Vec<DepthLevel>,
    pub traded_volumes: BTreeMap<i64, AggressorTradeVolumes>,
    pub trade_source_watermark: u64,
    /// True when the provider publishes Level 1 only and has no depth image.
    pub top_of_book_only: bool,
    pub state: OrderBookState,
}

/// One display-ready depth level with authoritative fixed-point values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderBookColumnLevel {
    pub price: i64,
    /// `None` for a best bid or offer published without a size; its
    /// `quantity_text` is then empty. Depth levels always carry a quantity.
    pub quantity: Option<i64>,
    pub order_count: Option<u32>,
    pub price_text: String,
    pub quantity_text: String,
    pub traded_volume: i64,
    pub traded_volume_text: String,
    /// Relative bar width in basis points (`0..=10_000`) within this frame.
    pub relative_size_bps: u16,
}

/// One horizontally aligned Order Book row, best prices first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderBookRow {
    pub bid: Option<OrderBookColumnLevel>,
    pub ask: Option<OrderBookColumnLevel>,
}

/// Immutable bounded provider-neutral Order Book frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderBookFrame {
    pub provider_id: String,
    pub instrument_id: String,
    pub entitlement_id: String,
    pub session_generation: u64,
    pub selection_generation: u64,
    pub revision: u64,
    pub source_watermark: u64,
    pub bbo_source_watermark: u64,
    pub state: OrderBookState,
    /// Decimal scale used to format presentation-only price-grid rows.
    pub price_scale: u8,
    /// Decimal scale used to format recent trade quantities.
    pub quantity_scale: u8,
    /// Authoritative minimum fixed-point price increment. Missing means the UI
    /// must render only provider-published prices rather than infer a tick grid.
    pub price_increment: Option<i64>,
    pub best_bid: Option<OrderBookColumnLevel>,
    pub best_ask: Option<OrderBookColumnLevel>,
    /// Runtime-owned recent aggressor volume, independent of resting depth.
    pub traded_volumes: BTreeMap<i64, AggressorTradeVolumes>,
    pub trade_source_watermark: u64,
    /// True when this frame is an explicit Level 1 BBO publication.
    pub top_of_book_only: bool,
    pub rows: Vec<OrderBookRow>,
}

/// Result of applying a snapshot or ordered delta.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrderBookApplyOutcome {
    Published,
    RecoveryRequired(OrderBookRecoveryReason),
    IgnoredStale,
}

/// Bounded candidate book that fails closed on any ordering or spread violation.
#[derive(Clone)]
pub struct OrderBook {
    maximum_levels: NonZeroUsize,
    bids: BTreeMap<i64, DepthLevel>,
    asks: BTreeMap<i64, DepthLevel>,
    identity: Option<(String, String, String, u64)>,
    revision: u64,
    source_watermark: u64,
    state: OrderBookState,
    snapshot_ready: bool,
    required_snapshot_watermark: u64,
}

impl OrderBook {
    /// Creates an empty book awaiting its first complete snapshot.
    #[must_use]
    pub fn new(maximum_levels: NonZeroUsize) -> Self {
        Self {
            maximum_levels,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            identity: None,
            revision: 0,
            source_watermark: 0,
            state: OrderBookState::Recovering(OrderBookRecoveryReason::AwaitingSnapshot),
            snapshot_ready: false,
            required_snapshot_watermark: 0,
        }
    }

    /// Returns the current recovery/readiness state.
    #[must_use]
    pub const fn state(&self) -> OrderBookState {
        self.state
    }

    /// Returns the current canonical book revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the latest accepted provider source watermark.
    #[must_use]
    pub const fn source_watermark(&self) -> u64 {
        self.source_watermark
    }

    /// Returns the provider session generation currently owning this book.
    #[must_use]
    pub fn session_generation(&self) -> Option<u64> {
        self.identity.as_ref().map(|identity| identity.3)
    }

    /// Iterates canonical bid levels from best to worst without allocating.
    pub fn bid_levels(&self) -> impl ExactSizeIterator<Item = DepthLevel> + '_ {
        self.bids.values().rev().copied()
    }

    /// Iterates canonical ask levels from best to worst without allocating.
    pub fn ask_levels(&self) -> impl ExactSizeIterator<Item = DepthLevel> + '_ {
        self.asks.values().copied()
    }

    /// Installs a complete bounded snapshot, replacing all candidate state.
    ///
    /// # Errors
    ///
    /// Returns a validation error without retaining any invalid candidate book.
    pub fn install_snapshot(
        &mut self,
        snapshot: &DepthSnapshot,
    ) -> Result<OrderBookApplyOutcome, MarketDataValidationError> {
        let identity = (
            snapshot.metadata.provider_id.clone(),
            snapshot.metadata.instrument_id.clone(),
            snapshot.metadata.entitlement_id.clone(),
            snapshot.metadata.session_generation,
        );
        if self.identity.is_none() {
            snapshot.metadata.validate()?;
        }
        let mut advances_generation = false;
        if let Some(current) = &self.identity {
            if identity.0 != current.0 || identity.1 != current.1 || identity.2 != current.2 {
                return Ok(OrderBookApplyOutcome::IgnoredStale);
            }
            if snapshot.metadata.session_generation < current.3 {
                return Ok(OrderBookApplyOutcome::IgnoredStale);
            }
            if snapshot.metadata.session_generation > current.3 {
                advances_generation = true;
            }
        }
        if !advances_generation {
            if self.identity.as_ref().is_some_and(|current| {
                current == &identity
                    && snapshot.metadata.source_sequence < self.required_snapshot_watermark
            }) {
                return Ok(OrderBookApplyOutcome::IgnoredStale);
            }
            if self.identity.as_ref().is_some_and(|current| {
                current == &identity && snapshot.metadata.source_sequence <= self.source_watermark
            }) {
                return Ok(OrderBookApplyOutcome::IgnoredStale);
            }
        }
        if let Err(error) = snapshot.validate(self.maximum_levels.get()) {
            if !advances_generation {
                if self.identity.is_none() {
                    self.identity = Some(identity);
                }
                self.require_recovery(
                    OrderBookRecoveryReason::InvalidUpdate,
                    snapshot.metadata.source_sequence,
                );
            }
            return Err(error);
        }
        if advances_generation {
            self.bids.clear();
            self.asks.clear();
            self.source_watermark = 0;
            self.required_snapshot_watermark = 0;
        }
        self.bids = snapshot
            .bids
            .iter()
            .map(|level| (level.price, *level))
            .collect();
        self.asks = snapshot
            .asks
            .iter()
            .map(|level| (level.price, *level))
            .collect();
        self.identity = Some(identity);
        self.source_watermark = snapshot.metadata.source_sequence;
        self.revision = self.revision.saturating_add(1).max(1);
        self.state = OrderBookState::Ready;
        self.snapshot_ready = true;
        self.required_snapshot_watermark = 0;
        Ok(OrderBookApplyOutcome::Published)
    }

    /// Applies one exactly-next delta or discards candidate state on a gap.
    ///
    /// # Errors
    ///
    /// Returns a validation or sequence error after entering recovery.
    pub fn apply_delta(
        &mut self,
        delta: &DepthDelta,
    ) -> Result<OrderBookApplyOutcome, MarketDataValidationError> {
        let Some(identity) = &self.identity else {
            delta.validate()?;
            return Ok(self.await_snapshot_after(delta));
        };
        if delta.metadata.provider_id != identity.0
            || delta.metadata.instrument_id != identity.1
            || delta.metadata.entitlement_id != identity.2
        {
            return Ok(OrderBookApplyOutcome::IgnoredStale);
        }
        if delta.metadata.session_generation < identity.3 {
            return Ok(OrderBookApplyOutcome::IgnoredStale);
        }
        if delta.metadata.session_generation > identity.3 {
            delta.validate()?;
            return Ok(self.await_snapshot_after(delta));
        }
        if !self.snapshot_ready {
            delta.validate()?;
            self.required_snapshot_watermark = self
                .required_snapshot_watermark
                .max(delta.metadata.source_sequence);
            return Ok(OrderBookApplyOutcome::RecoveryRequired(
                OrderBookRecoveryReason::AwaitingSnapshot,
            ));
        }
        if delta.metadata.source_sequence <= self.source_watermark {
            return Ok(OrderBookApplyOutcome::IgnoredStale);
        }
        if let Err(error) = delta.validate() {
            self.require_recovery(
                OrderBookRecoveryReason::InvalidUpdate,
                delta.metadata.source_sequence,
            );
            return Err(error);
        }
        let expected = self.source_watermark.saturating_add(1);
        if delta.metadata.source_sequence != expected {
            let actual = delta.metadata.source_sequence;
            self.require_recovery(OrderBookRecoveryReason::SequenceGap, actual);
            return Err(MarketDataValidationError::DepthGap { expected, actual });
        }
        let levels = match delta.side {
            BookSide::Bid => &mut self.bids,
            BookSide::Ask => &mut self.asks,
        };
        if delta.level.quantity == 0
            && levels.contains_key(&delta.level.price)
            && levels.len() >= self.maximum_levels.get()
        {
            self.require_recovery(
                OrderBookRecoveryReason::InvalidUpdate,
                delta.metadata.source_sequence,
            );
            return Ok(OrderBookApplyOutcome::RecoveryRequired(
                OrderBookRecoveryReason::InvalidUpdate,
            ));
        } else if delta.level.quantity == 0 {
            levels.remove(&delta.level.price);
        } else if !levels.contains_key(&delta.level.price)
            && levels.len() >= self.maximum_levels.get()
        {
            insert_if_improves(levels, delta.side, delta.level);
        } else {
            levels.insert(delta.level.price, delta.level);
        }
        if self
            .bids
            .last_key_value()
            .zip(self.asks.first_key_value())
            .is_some_and(|((bid, _), (ask, _))| bid >= ask)
        {
            self.require_recovery(
                OrderBookRecoveryReason::CrossedBook,
                delta.metadata.source_sequence,
            );
            return Ok(OrderBookApplyOutcome::RecoveryRequired(
                OrderBookRecoveryReason::CrossedBook,
            ));
        }
        self.source_watermark = delta.metadata.source_sequence;
        self.revision = self.revision.saturating_add(1).max(1);
        self.state = OrderBookState::Ready;
        self.snapshot_ready = true;
        Ok(OrderBookApplyOutcome::Published)
    }

    /// Marks a valid book stale and fails closed until a fresh covering snapshot arrives.
    pub fn mark_stale(&mut self) {
        if self.snapshot_ready && self.state == OrderBookState::Ready {
            self.bids.clear();
            self.asks.clear();
            self.required_snapshot_watermark =
                self.required_snapshot_watermark.max(self.source_watermark);
            self.state = OrderBookState::Stale;
            self.snapshot_ready = false;
            self.revision = self.revision.saturating_add(1).max(1);
        }
    }

    /// Returns the current immutable top-N image.
    #[must_use]
    pub fn publication(&self) -> OrderBookPublication {
        let (provider_id, instrument_id, entitlement_id, session_generation) =
            self.identity.as_ref().map_or_else(
                || (String::new(), String::new(), String::new(), 0),
                |identity| {
                    (
                        identity.0.clone(),
                        identity.1.clone(),
                        identity.2.clone(),
                        identity.3,
                    )
                },
            );
        OrderBookPublication {
            provider_id,
            instrument_id,
            entitlement_id,
            session_generation,
            revision: self.revision,
            source_watermark: self.source_watermark,
            best_bid: None,
            best_ask: None,
            bbo_source_watermark: 0,
            bids: self.bids.values().rev().copied().collect(),
            asks: self.asks.values().copied().collect(),
            traded_volumes: BTreeMap::new(),
            trade_source_watermark: 0,
            top_of_book_only: false,
            state: self.state,
        }
    }

    fn require_recovery(&mut self, reason: OrderBookRecoveryReason, observed_sequence: u64) {
        self.bids.clear();
        self.asks.clear();
        self.required_snapshot_watermark = self
            .required_snapshot_watermark
            .max(self.source_watermark)
            .max(observed_sequence);
        self.state = OrderBookState::Recovering(reason);
        self.snapshot_ready = false;
        self.revision = self.revision.saturating_add(1).max(1);
    }

    fn await_snapshot_after(&mut self, delta: &DepthDelta) -> OrderBookApplyOutcome {
        self.identity = Some((
            delta.metadata.provider_id.clone(),
            delta.metadata.instrument_id.clone(),
            delta.metadata.entitlement_id.clone(),
            delta.metadata.session_generation,
        ));
        self.bids.clear();
        self.asks.clear();
        self.source_watermark = 0;
        self.required_snapshot_watermark = delta.metadata.source_sequence;
        self.state = OrderBookState::Recovering(OrderBookRecoveryReason::AwaitingSnapshot);
        self.snapshot_ready = false;
        self.revision = self.revision.saturating_add(1).max(1);
        OrderBookApplyOutcome::RecoveryRequired(OrderBookRecoveryReason::AwaitingSnapshot)
    }
}

fn insert_if_improves(levels: &mut BTreeMap<i64, DepthLevel>, side: BookSide, level: DepthLevel) {
    let improves_retained_depth = match side {
        BookSide::Bid => levels
            .first_key_value()
            .is_some_and(|(worst, _)| level.price > *worst),
        BookSide::Ask => levels
            .last_key_value()
            .is_some_and(|(worst, _)| level.price < *worst),
    };
    if improves_retained_depth {
        levels.insert(level.price, level);
        match side {
            BookSide::Bid => {
                levels.pop_first();
            }
            BookSide::Ask => {
                levels.pop_last();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventMetadata, QualifiedTimestamp};

    fn metadata(sequence: u64, generation: u64) -> EventMetadata {
        EventMetadata {
            provider_id: "fixture".to_string(),
            instrument_id: "instrument:fixture:es".to_string(),
            entitlement_id: "test".to_string(),
            source_sequence: sequence,
            session_generation: generation,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(10),
                provider_unix_nanos: Some(11),
                received_unix_nanos: 12,
            },
        }
    }

    fn snapshot(sequence: u64) -> DepthSnapshot {
        DepthSnapshot {
            metadata: metadata(sequence, 1),
            bids: vec![
                DepthLevel {
                    price: 100,
                    quantity: 5,
                    order_count: Some(1),
                },
                DepthLevel {
                    price: 99,
                    quantity: 6,
                    order_count: None,
                },
            ],
            asks: vec![
                DepthLevel {
                    price: 101,
                    quantity: 4,
                    order_count: Some(1),
                },
                DepthLevel {
                    price: 102,
                    quantity: 7,
                    order_count: None,
                },
            ],
        }
    }

    #[test]
    fn snapshot_and_ordered_deltas_publish_bounded_top_levels() {
        let mut book = OrderBook::new(NonZeroUsize::new(3).unwrap_or(NonZeroUsize::MIN));
        assert!(matches!(
            book.install_snapshot(&snapshot(10)),
            Ok(OrderBookApplyOutcome::Published)
        ));
        let outcome = book
            .apply_delta(&DepthDelta {
                metadata: metadata(11, 1),
                side: BookSide::Bid,
                level: DepthLevel {
                    price: 101,
                    quantity: 2,
                    order_count: Some(1),
                },
            })
            .expect("ordered delta applies");
        let OrderBookApplyOutcome::RecoveryRequired(OrderBookRecoveryReason::CrossedBook) = outcome
        else {
            panic!("crossed book must require recovery");
        };
        assert_eq!(
            book.state(),
            OrderBookState::Recovering(OrderBookRecoveryReason::CrossedBook)
        );
        assert_eq!(book.publication().bids, [] as [DepthLevel; 0]);
    }

    #[test]
    fn ordered_replacements_and_removals_preserve_revision_and_watermark() {
        let mut book = OrderBook::new(NonZeroUsize::new(3).unwrap_or(NonZeroUsize::MIN));
        let OrderBookApplyOutcome::Published = book
            .install_snapshot(&snapshot(10))
            .expect("snapshot installs")
        else {
            panic!("snapshot must publish");
        };
        let initial = book.publication();
        let OrderBookApplyOutcome::Published = book
            .apply_delta(&DepthDelta {
                metadata: metadata(11, 1),
                side: BookSide::Bid,
                level: DepthLevel {
                    price: 100,
                    quantity: 9,
                    order_count: Some(2),
                },
            })
            .expect("replacement applies")
        else {
            panic!("replacement must publish");
        };
        let replaced = book.publication();
        assert_eq!(replaced.revision, initial.revision + 1);
        assert_eq!(replaced.source_watermark, 11);
        assert_eq!(replaced.bids[0].quantity, 9);
        let OrderBookApplyOutcome::Published = book
            .apply_delta(&DepthDelta {
                metadata: metadata(12, 1),
                side: BookSide::Ask,
                level: DepthLevel {
                    price: 101,
                    quantity: 0,
                    order_count: None,
                },
            })
            .expect("removal applies")
        else {
            panic!("removal must publish");
        };
        let removed = book.publication();
        assert_eq!(removed.asks[0].price, 102);
        assert_eq!(removed.source_watermark, 12);
    }

    #[test]
    fn removing_a_retained_level_from_a_full_book_requires_recovery() {
        let mut book = OrderBook::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        book.install_snapshot(&snapshot(10))
            .expect("snapshot installs");
        assert_eq!(
            book.apply_delta(&DepthDelta {
                metadata: metadata(11, 1),
                side: BookSide::Ask,
                level: DepthLevel {
                    price: 101,
                    quantity: 0,
                    order_count: None,
                },
            }),
            Ok(OrderBookApplyOutcome::RecoveryRequired(
                OrderBookRecoveryReason::InvalidUpdate
            ))
        );
        assert_eq!(book.publication().asks, [] as [DepthLevel; 0]);
    }

    #[test]
    fn depth_gap_discards_candidate_and_requires_covering_snapshot() {
        let mut book = OrderBook::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        book.install_snapshot(&snapshot(20))
            .expect("snapshot installs");
        let ready_revision = book.publication().revision;
        assert_eq!(
            book.apply_delta(&DepthDelta {
                metadata: metadata(22, 1),
                side: BookSide::Ask,
                level: DepthLevel {
                    price: 101,
                    quantity: 0,
                    order_count: None
                },
            }),
            Err(MarketDataValidationError::DepthGap {
                expected: 21,
                actual: 22
            })
        );
        assert_eq!(
            book.state(),
            OrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap)
        );
        assert_eq!(book.publication().revision, ready_revision + 1);
        book.mark_stale();
        assert_eq!(
            book.state(),
            OrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap)
        );
        assert!(matches!(
            book.install_snapshot(&snapshot(30)),
            Ok(OrderBookApplyOutcome::Published)
        ));
    }

    #[test]
    fn stale_publications_advance_revision_only_from_ready() {
        let mut book = OrderBook::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        book.install_snapshot(&snapshot(10))
            .expect("snapshot installs");
        let ready_revision = book.publication().revision;
        book.mark_stale();
        assert_eq!(book.state(), OrderBookState::Stale);
        assert_eq!(book.publication().revision, ready_revision + 1);
        book.mark_stale();
        assert_eq!(book.publication().revision, ready_revision + 1);
    }

    #[test]
    fn delta_before_initial_snapshot_requires_a_covering_snapshot() {
        let mut book = OrderBook::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        assert_eq!(
            book.apply_delta(&DepthDelta {
                metadata: metadata(20, 1),
                side: BookSide::Bid,
                level: DepthLevel {
                    price: 100,
                    quantity: 9,
                    order_count: None,
                },
            }),
            Ok(OrderBookApplyOutcome::RecoveryRequired(
                OrderBookRecoveryReason::AwaitingSnapshot
            ))
        );
        assert_eq!(
            book.install_snapshot(&snapshot(19)),
            Ok(OrderBookApplyOutcome::IgnoredStale)
        );
        assert_eq!(
            book.state(),
            OrderBookState::Recovering(OrderBookRecoveryReason::AwaitingSnapshot)
        );
        assert!(matches!(
            book.install_snapshot(&snapshot(20)),
            Ok(OrderBookApplyOutcome::Published)
        ));
    }

    #[test]
    fn stale_generations_and_sequences_cannot_mutate_current_book() {
        let mut book = OrderBook::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        book.install_snapshot(&snapshot(10))
            .expect("snapshot installs");
        let before = book.publication();
        let mut stale_metadata = metadata(11, 1);
        stale_metadata.instrument_id = "instrument:fixture:nq".to_string();
        assert_eq!(
            book.apply_delta(&DepthDelta {
                metadata: stale_metadata,
                side: BookSide::Bid,
                level: DepthLevel {
                    price: 100,
                    quantity: 99,
                    order_count: None
                },
            }),
            Ok(OrderBookApplyOutcome::IgnoredStale)
        );
        assert_eq!(book.publication(), before);

        let mut wrong_snapshot = snapshot(11);
        wrong_snapshot.metadata.instrument_id = "instrument:fixture:nq".to_string();
        assert_eq!(
            book.install_snapshot(&wrong_snapshot),
            Ok(OrderBookApplyOutcome::IgnoredStale)
        );
        assert_eq!(book.publication(), before);

        let mut malformed_stale = metadata(10, 1);
        malformed_stale.timestamps.received_unix_nanos = 0;
        assert_eq!(
            book.apply_delta(&DepthDelta {
                metadata: malformed_stale,
                side: BookSide::Bid,
                level: DepthLevel {
                    price: 100,
                    quantity: -1,
                    order_count: None,
                },
            }),
            Ok(OrderBookApplyOutcome::IgnoredStale)
        );
        assert_eq!(book.publication(), before);

        let mut newer = snapshot(20);
        newer.metadata.session_generation = 3;
        book.install_snapshot(&newer)
            .expect("newer generation snapshot installs");
        let current = book.publication();
        let mut delayed = snapshot(30);
        delayed.metadata.session_generation = 2;
        delayed.bids[0].quantity = 0;
        assert_eq!(
            book.install_snapshot(&delayed),
            Ok(OrderBookApplyOutcome::IgnoredStale)
        );
        assert_eq!(book.publication(), current);

        assert!(matches!(
            book.apply_delta(&DepthDelta {
                metadata: metadata(22, 3),
                side: BookSide::Ask,
                level: DepthLevel {
                    price: 101,
                    quantity: 0,
                    order_count: None,
                },
            }),
            Err(MarketDataValidationError::DepthGap { .. })
        ));
        let recovering = book.publication();
        let mut old_snapshot = snapshot(40);
        old_snapshot.metadata.session_generation = 2;
        assert_eq!(
            book.install_snapshot(&old_snapshot),
            Ok(OrderBookApplyOutcome::IgnoredStale)
        );
        assert_eq!(book.publication(), recovering);

        assert_eq!(
            book.apply_delta(&DepthDelta {
                metadata: metadata(1, 4),
                side: BookSide::Bid,
                level: DepthLevel {
                    price: 100,
                    quantity: 1,
                    order_count: None,
                },
            }),
            Ok(OrderBookApplyOutcome::RecoveryRequired(
                OrderBookRecoveryReason::AwaitingSnapshot
            ))
        );
        assert_eq!(book.publication().session_generation, 4);

        let mut covering = snapshot(1);
        covering.metadata.session_generation = 4;
        assert!(matches!(
            book.install_snapshot(&covering),
            Ok(OrderBookApplyOutcome::Published)
        ));
    }

    #[test]
    fn malformed_higher_generations_cannot_poison_the_current_book() {
        let mut book = OrderBook::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        let mut current_snapshot = snapshot(20);
        current_snapshot.metadata.session_generation = 3;
        book.install_snapshot(&current_snapshot)
            .expect("current generation installs");
        let current = book.publication();

        let mut malformed_newer = snapshot(21);
        malformed_newer.metadata.session_generation = 4;
        malformed_newer.bids[0].quantity = 0;
        assert!(book.install_snapshot(&malformed_newer).is_err());
        assert_eq!(book.publication(), current);

        let mut malformed_newer_delta = metadata(1, 4);
        malformed_newer_delta.timestamps.received_unix_nanos = 0;
        assert!(
            book.apply_delta(&DepthDelta {
                metadata: malformed_newer_delta,
                side: BookSide::Bid,
                level: DepthLevel {
                    price: 100,
                    quantity: 1,
                    order_count: None,
                },
            })
            .is_err()
        );
        assert_eq!(book.publication(), current);
    }

    #[test]
    fn recovery_rejects_snapshots_below_the_observed_sequence_floor() {
        let mut book = OrderBook::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        book.install_snapshot(&snapshot(20))
            .expect("snapshot installs");
        assert!(matches!(
            book.apply_delta(&DepthDelta {
                metadata: metadata(22, 1),
                side: BookSide::Ask,
                level: DepthLevel {
                    price: 101,
                    quantity: 0,
                    order_count: None,
                },
            }),
            Err(MarketDataValidationError::DepthGap { .. })
        ));

        let mut stale = snapshot(21);
        stale.bids[0].quantity = 0;
        assert_eq!(
            book.install_snapshot(&stale),
            Ok(OrderBookApplyOutcome::IgnoredStale)
        );
        assert_eq!(
            book.state(),
            OrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap)
        );
        assert_eq!(
            book.apply_delta(&DepthDelta {
                metadata: metadata(30, 1),
                side: BookSide::Bid,
                level: DepthLevel {
                    price: 100,
                    quantity: 9,
                    order_count: None,
                },
            }),
            Ok(OrderBookApplyOutcome::RecoveryRequired(
                OrderBookRecoveryReason::AwaitingSnapshot
            ))
        );
        assert!(matches!(
            book.install_snapshot(&snapshot(22)),
            Ok(OrderBookApplyOutcome::IgnoredStale)
        ));
        assert!(matches!(
            book.install_snapshot(&snapshot(30)),
            Ok(OrderBookApplyOutcome::Published)
        ));
    }

    #[test]
    fn invalid_initial_snapshot_preserves_its_recovery_watermark() {
        let mut book = OrderBook::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        let mut invalid = snapshot(100);
        invalid.bids[0].quantity = 0;
        assert_eq!(
            book.install_snapshot(&invalid),
            Err(MarketDataValidationError::InvalidQuantity)
        );
        assert_eq!(
            book.state(),
            OrderBookState::Recovering(OrderBookRecoveryReason::InvalidUpdate)
        );
        assert_eq!(
            book.install_snapshot(&snapshot(90)),
            Ok(OrderBookApplyOutcome::IgnoredStale)
        );
        assert!(matches!(
            book.install_snapshot(&snapshot(100)),
            Ok(OrderBookApplyOutcome::Published)
        ));
    }

    #[test]
    fn updates_outside_retained_depth_advance_without_changing_levels() {
        let mut book = OrderBook::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        book.install_snapshot(&snapshot(10))
            .expect("snapshot installs");
        let OrderBookApplyOutcome::Published = book
            .apply_delta(&DepthDelta {
                metadata: metadata(11, 1),
                side: BookSide::Bid,
                level: DepthLevel {
                    price: 98,
                    quantity: 3,
                    order_count: None,
                },
            })
            .expect("out-of-range update is ordered")
        else {
            panic!("out-of-range update must publish its watermark");
        };
        let publication = book.publication();
        assert_eq!(publication.source_watermark, 11);
        assert_eq!(
            publication
                .bids
                .iter()
                .map(|level| level.price)
                .collect::<Vec<_>>(),
            vec![100, 99]
        );
    }

    #[test]
    fn better_levels_evict_the_worst_retained_level() {
        let mut book = OrderBook::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        let mut initial = snapshot(10);
        initial.bids[1].price = 90;
        initial.asks[0].price = 110;
        initial.asks[1].price = 120;
        book.install_snapshot(&initial).expect("snapshot installs");
        let OrderBookApplyOutcome::Published = book
            .apply_delta(&DepthDelta {
                metadata: metadata(11, 1),
                side: BookSide::Bid,
                level: DepthLevel {
                    price: 105,
                    quantity: 3,
                    order_count: None,
                },
            })
            .expect("better level applies")
        else {
            panic!("better level must publish");
        };
        let publication = book.publication();
        assert_eq!(
            publication
                .bids
                .iter()
                .map(|level| level.price)
                .collect::<Vec<_>>(),
            vec![105, 100]
        );
    }
}
