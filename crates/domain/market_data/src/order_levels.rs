//! Provider-neutral order-level (market-by-order) events and the canonical
//! bounded order-level book built from them.
//!
//! Order-level data is additive to aggregate price-level depth: a provider
//! publishes it only for explicit order-level demand, and it never replaces
//! the aggregate book.

use crate::{
    BookSide, EventMetadata, MarketDataValidationError, contracts::validate_identity_field,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
};

/// Most resting orders one order-level image may hold.
pub const MAXIMUM_ORDER_LEVEL_ORDERS: usize = 131_072;
/// Most changes one order-level update may carry, including merged updates.
pub const MAXIMUM_ORDER_LEVEL_CHANGES: usize = 16_384;

/// One resting order. `priority` is the provider's queue-priority value at
/// its price; it is carried as published and never derived from arrival order.
/// Prices may be zero or negative because spread books legitimately rest there.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestingOrder {
    pub order_id: String,
    pub side: BookSide,
    pub price: i64,
    pub quantity: i64,
    pub priority: u64,
}

impl RestingOrder {
    fn validate(&self) -> Result<(), MarketDataValidationError> {
        validate_identity_field("order_id", &self.order_id)?;
        if self.quantity <= 0 {
            return Err(MarketDataValidationError::InvalidQuantity);
        }
        Ok(())
    }
}

/// One order mutation with the provider's add/change/delete meaning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrderLevelChange {
    Add(RestingOrder),
    /// Replaces the order's price, quantity and priority.
    Change(RestingOrder),
    /// Removes the order resting on `side` at `price`.
    Delete {
        order_id: String,
        side: BookSide,
        price: i64,
    },
}

impl OrderLevelChange {
    /// Returns the exchange order identity the change applies to.
    #[must_use]
    pub fn order_id(&self) -> &str {
        match self {
            Self::Add(order) | Self::Change(order) => &order.order_id,
            Self::Delete { order_id, .. } => order_id,
        }
    }

    fn validate(&self) -> Result<(), MarketDataValidationError> {
        match self {
            Self::Add(order) | Self::Change(order) => order.validate(),
            Self::Delete { order_id, .. } => validate_identity_field("order_id", order_id),
        }
    }
}

/// A complete covering order-level image.
///
/// `image` names this covering image and `revision` counts the updates
/// already applied to it (zero when the provider adapter publishes it). They
/// are Aeris continuity counters, kept separate from the provider's own
/// optional `provider_sequence` evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderLevelSnapshot {
    pub metadata: EventMetadata,
    pub image: u64,
    pub revision: u64,
    pub provider_sequence: Option<u64>,
    pub orders: Vec<RestingOrder>,
}

impl OrderLevelSnapshot {
    /// Validates identity, bounded size and unique order ids.
    ///
    /// # Errors
    ///
    /// Returns an error when the image cannot safely establish order state.
    pub fn validate(&self) -> Result<(), MarketDataValidationError> {
        self.metadata.validate()?;
        if self.image == 0 {
            return Err(MarketDataValidationError::InvalidRevision);
        }
        if self.orders.len() > MAXIMUM_ORDER_LEVEL_ORDERS {
            return Err(MarketDataValidationError::OrderLevelLimitExceeded {
                maximum: MAXIMUM_ORDER_LEVEL_ORDERS,
            });
        }
        let mut ids = BTreeSet::new();
        for order in &self.orders {
            order.validate()?;
            if !ids.insert(order.order_id.as_str()) {
                return Err(MarketDataValidationError::DuplicateOrderId);
            }
        }
        Ok(())
    }
}

/// Ordered changes that move one image from `previous_revision` to
/// `revision`. Contiguous updates may be merged; the merged update spans both.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderLevelUpdate {
    pub metadata: EventMetadata,
    pub image: u64,
    pub previous_revision: u64,
    pub revision: u64,
    pub provider_sequence: Option<u64>,
    pub changes: Vec<OrderLevelChange>,
}

impl OrderLevelUpdate {
    /// Validates identity, continuity counters and bounded changes.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid metadata, counters or changes.
    pub fn validate(&self) -> Result<(), MarketDataValidationError> {
        self.metadata.validate()?;
        if self.image == 0 || self.revision <= self.previous_revision {
            return Err(MarketDataValidationError::InvalidRevision);
        }
        if self.changes.is_empty() {
            return Err(MarketDataValidationError::EmptyOrderLevelUpdate);
        }
        if self.changes.len() > MAXIMUM_ORDER_LEVEL_CHANGES {
            return Err(MarketDataValidationError::OrderLevelLimitExceeded {
                maximum: MAXIMUM_ORDER_LEVEL_CHANGES,
            });
        }
        self.changes.iter().try_for_each(OrderLevelChange::validate)
    }

    /// Appends the exactly-next update of the same image, keeping the newest
    /// metadata and provider sequence. Returns `next` unchanged when it does
    /// not continue this update or the merged changes would exceed the bound.
    #[must_use]
    pub fn merge(&mut self, next: Self) -> Option<Self> {
        let continues = self.metadata.provider_id == next.metadata.provider_id
            && self.metadata.instrument_id == next.metadata.instrument_id
            && self.metadata.entitlement_id == next.metadata.entitlement_id
            && self.metadata.session_generation == next.metadata.session_generation
            && self.image == next.image
            && next.previous_revision == self.revision
            && self.changes.len().saturating_add(next.changes.len()) <= MAXIMUM_ORDER_LEVEL_CHANGES;
        if !continues {
            return Some(next);
        }
        self.metadata = next.metadata;
        self.revision = next.revision;
        self.provider_sequence = next.provider_sequence.or(self.provider_sequence);
        self.changes.extend(next.changes);
        None
    }
}

/// Why an order-level image stopped being authoritative.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderLevelInterruption {
    /// The provider stream or an Aeris counter skipped or contradicted state.
    SequenceGap,
    /// An order, change or delivery bound was exceeded.
    Overflow,
    /// The provider refused the order-level subscription.
    SubscriptionRefused,
    /// The provider refused the covering snapshot request.
    SnapshotRefused,
    /// A change could not apply to the image (unknown or duplicate order).
    InvalidUpdate,
    /// Nothing demands order levels for the instrument any more and the
    /// provider dropped its subscription.
    Released,
}

/// The provider discarded `image`. With `resnapshot` a covering image is
/// being requested; without it order-level data stays off for the session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderLevelReset {
    pub metadata: EventMetadata,
    pub image: u64,
    pub reason: OrderLevelInterruption,
    pub resnapshot: bool,
}

/// Provider-neutral order-level event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrderLevelEvent {
    Snapshot(OrderLevelSnapshot),
    Update(OrderLevelUpdate),
    Reset(OrderLevelReset),
}

impl OrderLevelEvent {
    /// Returns common ordering and provenance metadata.
    #[must_use]
    pub const fn metadata(&self) -> &EventMetadata {
        match self {
            Self::Snapshot(value) => &value.metadata,
            Self::Update(value) => &value.metadata,
            Self::Reset(value) => &value.metadata,
        }
    }

    /// Validates the event.
    ///
    /// # Errors
    ///
    /// Returns the event-specific canonical validation failure.
    pub fn validate(&self) -> Result<(), MarketDataValidationError> {
        match self {
            Self::Snapshot(value) => value.validate(),
            Self::Update(value) => value.validate(),
            Self::Reset(value) => value.metadata.validate(),
        }
    }
}

/// Authority of one order-level book. Only `Ready` holds a usable image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderLevelState {
    AwaitingSnapshot,
    Ready,
    /// The image was discarded and a covering image is expected.
    Recovering(OrderLevelInterruption),
    /// The provider turned order-level data off for its session.
    Unavailable(OrderLevelInterruption),
    /// The provider session ended; a new session must publish a fresh image.
    Stale,
}

/// Result of applying one order-level event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderLevelApplyOutcome {
    Applied,
    RecoveryRequired(OrderLevelInterruption),
    IgnoredStale,
}

type BookIdentity = (String, String, String, u64);

/// Canonical bounded order-level book. It fails closed: any gap, unknown
/// order or bound violation discards the image until a covering snapshot.
#[derive(Clone)]
pub struct OrderLevelBook {
    maximum_orders: NonZeroUsize,
    identity: Option<BookIdentity>,
    metadata: Option<EventMetadata>,
    image: u64,
    revision: u64,
    provider_sequence: Option<u64>,
    orders: BTreeMap<String, RestingOrder>,
    bids: BTreeMap<i64, BTreeSet<(u64, String)>>,
    asks: BTreeMap<i64, BTreeSet<(u64, String)>>,
    state: OrderLevelState,
}

impl OrderLevelBook {
    /// Creates an empty book awaiting its first covering image.
    #[must_use]
    pub const fn new(maximum_orders: NonZeroUsize) -> Self {
        Self {
            maximum_orders,
            identity: None,
            metadata: None,
            image: 0,
            revision: 0,
            provider_sequence: None,
            orders: BTreeMap::new(),
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            state: OrderLevelState::AwaitingSnapshot,
        }
    }

    #[must_use]
    pub const fn state(&self) -> OrderLevelState {
        self.state
    }

    #[must_use]
    pub const fn image(&self) -> u64 {
        self.image
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub const fn provider_sequence(&self) -> Option<u64> {
        self.provider_sequence
    }

    #[must_use]
    pub fn session_generation(&self) -> Option<u64> {
        self.identity.as_ref().map(|identity| identity.3)
    }

    /// Number of resting orders in the current image.
    #[must_use]
    pub fn len(&self) -> usize {
        self.orders.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }

    /// Returns one resting order by exchange order id.
    #[must_use]
    pub fn order(&self, order_id: &str) -> Option<&RestingOrder> {
        self.orders.get(order_id)
    }

    /// Prices with resting orders on `side`, best first.
    pub fn prices(&self, side: BookSide) -> Box<dyn Iterator<Item = i64> + '_> {
        match side {
            BookSide::Bid => Box::new(self.bids.keys().rev().copied()),
            BookSide::Ask => Box::new(self.asks.keys().copied()),
        }
    }

    /// Orders resting on `side` at `price`, in ascending provider priority.
    pub fn queue(&self, side: BookSide, price: i64) -> impl Iterator<Item = &RestingOrder> + '_ {
        let levels = match side {
            BookSide::Bid => &self.bids,
            BookSide::Ask => &self.asks,
        };
        levels
            .get(&price)
            .into_iter()
            .flatten()
            .filter_map(|(_, order_id)| self.orders.get(order_id))
    }

    /// Returns the current image as a covering snapshot, or `None` while the
    /// book holds no authoritative image.
    #[must_use]
    pub fn covering_snapshot(&self) -> Option<OrderLevelSnapshot> {
        if self.state != OrderLevelState::Ready {
            return None;
        }
        Some(OrderLevelSnapshot {
            metadata: self.metadata.clone()?,
            image: self.image,
            revision: self.revision,
            provider_sequence: self.provider_sequence,
            orders: self.orders.values().cloned().collect(),
        })
    }

    /// Applies one order-level event.
    ///
    /// # Errors
    ///
    /// Returns the validation or continuity failure after discarding the image.
    pub fn apply(
        &mut self,
        event: &OrderLevelEvent,
    ) -> Result<OrderLevelApplyOutcome, MarketDataValidationError> {
        match event {
            OrderLevelEvent::Snapshot(snapshot) => self.install_snapshot(snapshot),
            OrderLevelEvent::Update(update) => self.apply_update(update),
            OrderLevelEvent::Reset(reset) => Ok(self.reset(reset)),
        }
    }

    fn install_snapshot(
        &mut self,
        snapshot: &OrderLevelSnapshot,
    ) -> Result<OrderLevelApplyOutcome, MarketDataValidationError> {
        match self.admit(&snapshot.metadata) {
            Admission::Stale => return Ok(OrderLevelApplyOutcome::IgnoredStale),
            Admission::Current if snapshot.image < self.image => {
                return Ok(OrderLevelApplyOutcome::IgnoredStale);
            }
            Admission::Current | Admission::NewSession => {}
        }
        let validated = snapshot.validate().and_then(|()| {
            if snapshot.orders.len() > self.maximum_orders.get() {
                Err(MarketDataValidationError::OrderLevelLimitExceeded {
                    maximum: self.maximum_orders.get(),
                })
            } else {
                Ok(())
            }
        });
        self.identity = Some(identity(&snapshot.metadata));
        self.image = snapshot.image;
        if let Err(error) = validated {
            let reason = match error {
                MarketDataValidationError::OrderLevelLimitExceeded { .. } => {
                    OrderLevelInterruption::Overflow
                }
                _ => OrderLevelInterruption::InvalidUpdate,
            };
            self.discard(OrderLevelState::Recovering(reason));
            return Err(error);
        }
        self.clear();
        for order in &snapshot.orders {
            self.insert(order.clone());
        }
        self.metadata = Some(snapshot.metadata.clone());
        self.revision = snapshot.revision;
        self.provider_sequence = snapshot.provider_sequence;
        self.state = OrderLevelState::Ready;
        Ok(OrderLevelApplyOutcome::Applied)
    }

    fn apply_update(
        &mut self,
        update: &OrderLevelUpdate,
    ) -> Result<OrderLevelApplyOutcome, MarketDataValidationError> {
        match self.admit(&update.metadata) {
            Admission::Stale => return Ok(OrderLevelApplyOutcome::IgnoredStale),
            Admission::NewSession => {
                self.identity = Some(identity(&update.metadata));
                self.image = 0;
                self.discard(OrderLevelState::AwaitingSnapshot);
                return Ok(OrderLevelApplyOutcome::IgnoredStale);
            }
            Admission::Current => {}
        }
        if self.state != OrderLevelState::Ready
            || update.image < self.image
            || (update.image == self.image && update.revision <= self.revision)
        {
            return Ok(OrderLevelApplyOutcome::IgnoredStale);
        }
        if update.image != self.image || update.previous_revision != self.revision {
            let (expected, actual) = if update.image == self.image {
                (self.revision, update.previous_revision)
            } else {
                (self.image, update.image)
            };
            self.discard(OrderLevelState::Recovering(
                OrderLevelInterruption::SequenceGap,
            ));
            return Err(MarketDataValidationError::OrderLevelGap { expected, actual });
        }
        let applied = update.validate().and_then(|()| {
            update
                .changes
                .iter()
                .try_for_each(|change| self.apply_change(change))
        });
        if let Err(error) = applied {
            let reason = match error {
                MarketDataValidationError::OrderLevelLimitExceeded { .. } => {
                    OrderLevelInterruption::Overflow
                }
                _ => OrderLevelInterruption::InvalidUpdate,
            };
            self.discard(OrderLevelState::Recovering(reason));
            return Err(error);
        }
        self.metadata = Some(update.metadata.clone());
        self.revision = update.revision;
        if update.provider_sequence.is_some() {
            self.provider_sequence = update.provider_sequence;
        }
        Ok(OrderLevelApplyOutcome::Applied)
    }

    fn reset(&mut self, reset: &OrderLevelReset) -> OrderLevelApplyOutcome {
        match self.admit(&reset.metadata) {
            Admission::Stale => return OrderLevelApplyOutcome::IgnoredStale,
            Admission::Current if reset.image < self.image => {
                return OrderLevelApplyOutcome::IgnoredStale;
            }
            Admission::Current | Admission::NewSession => {}
        }
        self.identity = Some(identity(&reset.metadata));
        self.image = reset.image;
        self.discard(if reset.resnapshot {
            OrderLevelState::Recovering(reset.reason)
        } else {
            OrderLevelState::Unavailable(reset.reason)
        });
        OrderLevelApplyOutcome::RecoveryRequired(reset.reason)
    }

    /// Fails closed after the provider session that owned the image ended.
    pub fn mark_stale(&mut self) {
        if self.state == OrderLevelState::Ready {
            self.discard(OrderLevelState::Stale);
        }
    }

    fn admit(&self, metadata: &EventMetadata) -> Admission {
        let Some(current) = &self.identity else {
            return Admission::NewSession;
        };
        if metadata.provider_id != current.0
            || metadata.instrument_id != current.1
            || metadata.entitlement_id != current.2
            || metadata.session_generation < current.3
        {
            Admission::Stale
        } else if metadata.session_generation > current.3 {
            Admission::NewSession
        } else {
            Admission::Current
        }
    }

    fn apply_change(&mut self, change: &OrderLevelChange) -> Result<(), MarketDataValidationError> {
        match change {
            OrderLevelChange::Add(order) => {
                if self.orders.contains_key(&order.order_id) {
                    return Err(MarketDataValidationError::DuplicateOrderId);
                }
                if self.orders.len() >= self.maximum_orders.get() {
                    return Err(MarketDataValidationError::OrderLevelLimitExceeded {
                        maximum: self.maximum_orders.get(),
                    });
                }
                self.insert(order.clone());
            }
            OrderLevelChange::Change(order) => {
                let previous = self
                    .remove(&order.order_id)
                    .ok_or(MarketDataValidationError::UnknownOrderId)?;
                if previous.side != order.side {
                    return Err(MarketDataValidationError::InvalidDepth);
                }
                self.insert(order.clone());
            }
            OrderLevelChange::Delete {
                order_id,
                side,
                price,
            } => {
                let previous = self
                    .remove(order_id)
                    .ok_or(MarketDataValidationError::UnknownOrderId)?;
                if previous.side != *side || previous.price != *price {
                    return Err(MarketDataValidationError::InvalidDepth);
                }
            }
        }
        Ok(())
    }

    fn insert(&mut self, order: RestingOrder) {
        let levels = match order.side {
            BookSide::Bid => &mut self.bids,
            BookSide::Ask => &mut self.asks,
        };
        levels
            .entry(order.price)
            .or_default()
            .insert((order.priority, order.order_id.clone()));
        self.orders.insert(order.order_id.clone(), order);
    }

    fn remove(&mut self, order_id: &str) -> Option<RestingOrder> {
        let order = self.orders.remove(order_id)?;
        let levels = match order.side {
            BookSide::Bid => &mut self.bids,
            BookSide::Ask => &mut self.asks,
        };
        if let Some(queue) = levels.get_mut(&order.price) {
            queue.remove(&(order.priority, order.order_id.clone()));
            if queue.is_empty() {
                levels.remove(&order.price);
            }
        }
        Some(order)
    }

    fn clear(&mut self) {
        self.orders.clear();
        self.bids.clear();
        self.asks.clear();
    }

    fn discard(&mut self, state: OrderLevelState) {
        self.clear();
        self.metadata = None;
        self.revision = 0;
        self.provider_sequence = None;
        self.state = state;
    }
}

enum Admission {
    Stale,
    Current,
    NewSession,
}

fn identity(metadata: &EventMetadata) -> BookIdentity {
    (
        metadata.provider_id.clone(),
        metadata.instrument_id.clone(),
        metadata.entitlement_id.clone(),
        metadata.session_generation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QualifiedTimestamp;

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

    fn order(
        order_id: &str,
        side: BookSide,
        price: i64,
        quantity: i64,
        priority: u64,
    ) -> RestingOrder {
        RestingOrder {
            order_id: order_id.to_string(),
            side,
            price,
            quantity,
            priority,
        }
    }

    fn snapshot(image: u64) -> OrderLevelEvent {
        OrderLevelEvent::Snapshot(OrderLevelSnapshot {
            metadata: metadata(1, 1),
            image,
            revision: 0,
            provider_sequence: Some(40),
            orders: vec![
                order("b1", BookSide::Bid, 100, 2, 7),
                order("b2", BookSide::Bid, 100, 3, 5),
                order("a1", BookSide::Ask, 101, 4, 1),
            ],
        })
    }

    fn update(previous_revision: u64, changes: Vec<OrderLevelChange>) -> OrderLevelUpdate {
        OrderLevelUpdate {
            metadata: metadata(previous_revision + 2, 1),
            image: 1,
            previous_revision,
            revision: previous_revision + 1,
            provider_sequence: Some(41 + previous_revision),
            changes,
        }
    }

    fn book() -> OrderLevelBook {
        OrderLevelBook::new(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN))
    }

    #[test]
    fn snapshot_and_contiguous_updates_preserve_identity_priority_and_sequence() {
        let mut book = book();
        assert_eq!(
            book.apply(&snapshot(1)),
            Ok(OrderLevelApplyOutcome::Applied)
        );
        let queue = book
            .queue(BookSide::Bid, 100)
            .map(|order| order.order_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(queue, ["b2", "b1"]);
        let changes = vec![
            OrderLevelChange::Add(order("a2", BookSide::Ask, 102, 1, 2)),
            OrderLevelChange::Change(order("b1", BookSide::Bid, 99, 6, 9)),
            OrderLevelChange::Delete {
                order_id: "a1".to_string(),
                side: BookSide::Ask,
                price: 101,
            },
        ];
        assert_eq!(
            book.apply(&OrderLevelEvent::Update(update(0, changes))),
            Ok(OrderLevelApplyOutcome::Applied)
        );
        assert_eq!(book.revision(), 1);
        assert_eq!(book.provider_sequence(), Some(41));
        assert_eq!(book.prices(BookSide::Bid).collect::<Vec<_>>(), [100, 99]);
        assert_eq!(book.prices(BookSide::Ask).collect::<Vec<_>>(), [102]);
        assert_eq!(book.order("b1").map(|order| order.priority), Some(9));
        let covering = book.covering_snapshot().expect("ready book covers itself");
        assert_eq!((covering.image, covering.revision), (1, 1));
        let mut replica = super::tests::book();
        assert_eq!(
            replica.apply(&OrderLevelEvent::Snapshot(covering)),
            Ok(OrderLevelApplyOutcome::Applied)
        );
        assert_eq!(replica.order("a2"), book.order("a2"));
    }

    #[test]
    fn a_revision_gap_discards_the_image_until_a_covering_snapshot() {
        let mut book = book();
        book.apply(&snapshot(1)).expect("snapshot installs");
        let skipped = update(
            1,
            vec![OrderLevelChange::Add(order("x", BookSide::Bid, 98, 1, 1))],
        );
        assert_eq!(
            book.apply(&OrderLevelEvent::Update(skipped)),
            Err(MarketDataValidationError::OrderLevelGap {
                expected: 0,
                actual: 1
            })
        );
        assert_eq!(
            book.state(),
            OrderLevelState::Recovering(OrderLevelInterruption::SequenceGap)
        );
        assert!(book.is_empty());
        assert!(book.covering_snapshot().is_none());
        let late = update(
            0,
            vec![OrderLevelChange::Add(order("y", BookSide::Bid, 98, 1, 1))],
        );
        assert_eq!(
            book.apply(&OrderLevelEvent::Update(late)),
            Ok(OrderLevelApplyOutcome::IgnoredStale)
        );
        assert_eq!(
            book.apply(&snapshot(2)),
            Ok(OrderLevelApplyOutcome::Applied)
        );
        assert_eq!(book.state(), OrderLevelState::Ready);
    }

    #[test]
    fn unknown_duplicate_and_over_bound_changes_fail_closed() {
        for change in [
            OrderLevelChange::Add(order("b1", BookSide::Bid, 100, 1, 1)),
            OrderLevelChange::Change(order("missing", BookSide::Bid, 100, 1, 1)),
            OrderLevelChange::Delete {
                order_id: "a1".to_string(),
                side: BookSide::Ask,
                price: 999,
            },
        ] {
            let mut book = book();
            book.apply(&snapshot(1)).expect("snapshot installs");
            assert!(
                book.apply(&OrderLevelEvent::Update(update(0, vec![change])))
                    .is_err()
            );
            assert_eq!(
                book.state(),
                OrderLevelState::Recovering(OrderLevelInterruption::InvalidUpdate)
            );
        }
        let mut book = book();
        book.apply(&snapshot(1)).expect("snapshot installs");
        let over = update(
            0,
            vec![
                OrderLevelChange::Add(order("n1", BookSide::Bid, 97, 1, 1)),
                OrderLevelChange::Add(order("n2", BookSide::Bid, 96, 1, 1)),
            ],
        );
        assert_eq!(
            book.apply(&OrderLevelEvent::Update(over)),
            Err(MarketDataValidationError::OrderLevelLimitExceeded { maximum: 4 })
        );
        assert_eq!(
            book.state(),
            OrderLevelState::Recovering(OrderLevelInterruption::Overflow)
        );
    }

    #[test]
    fn resets_stale_sessions_and_newer_sessions_never_present_an_old_image() {
        let mut book = book();
        book.apply(&snapshot(1)).expect("snapshot installs");
        let reset = OrderLevelEvent::Reset(OrderLevelReset {
            metadata: metadata(9, 1),
            image: 1,
            reason: OrderLevelInterruption::SnapshotRefused,
            resnapshot: false,
        });
        assert_eq!(
            book.apply(&reset),
            Ok(OrderLevelApplyOutcome::RecoveryRequired(
                OrderLevelInterruption::SnapshotRefused
            ))
        );
        assert_eq!(
            book.state(),
            OrderLevelState::Unavailable(OrderLevelInterruption::SnapshotRefused)
        );

        let mut restarted = super::tests::book();
        restarted.apply(&snapshot(1)).expect("snapshot installs");
        restarted.mark_stale();
        assert_eq!(restarted.state(), OrderLevelState::Stale);
        restarted
            .apply(&snapshot(1))
            .expect("the same image reinstalls");
        let mut newer = update(
            0,
            vec![OrderLevelChange::Add(order("z", BookSide::Ask, 103, 1, 1))],
        );
        newer.metadata.session_generation = 2;
        assert_eq!(
            restarted.apply(&OrderLevelEvent::Update(newer)),
            Ok(OrderLevelApplyOutcome::IgnoredStale)
        );
        assert_eq!(restarted.state(), OrderLevelState::AwaitingSnapshot);
        assert!(restarted.is_empty());
    }

    #[test]
    fn only_exactly_next_updates_of_one_image_merge_within_the_change_bound() {
        let first = update(
            0,
            vec![OrderLevelChange::Add(order("m1", BookSide::Bid, 90, 1, 1))],
        );
        let mut merged = first.clone();
        let second = update(
            1,
            vec![OrderLevelChange::Add(order("m2", BookSide::Bid, 90, 1, 2))],
        );
        assert_eq!(merged.merge(second), None);
        assert_eq!((merged.previous_revision, merged.revision), (0, 2));
        assert_eq!(merged.changes.len(), 2);
        let skipped = update(
            3,
            vec![OrderLevelChange::Add(order("m3", BookSide::Bid, 90, 1, 3))],
        );
        assert!(merged.merge(skipped).is_some());
        let mut oversized = first;
        oversized.changes = vec![
            OrderLevelChange::Delete {
                order_id: "q".to_string(),
                side: BookSide::Bid,
                price: 1,
            };
            MAXIMUM_ORDER_LEVEL_CHANGES
        ];
        let next = update(
            1,
            vec![OrderLevelChange::Add(order("m4", BookSide::Bid, 90, 1, 4))],
        );
        assert!(oversized.merge(next).is_some());
    }
}
