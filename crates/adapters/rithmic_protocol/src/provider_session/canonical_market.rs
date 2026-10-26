//! Canonical market.

use crate::book::{
    PriceLevel, PriceLevelBook, PriceLevelFault, PriceLevelFrame, PriceLevelOutcome,
};
use aeris_market_data::{
    MAXIMUM_ORDER_LEVEL_CHANGES, MAXIMUM_ORDER_LEVEL_ORDERS, MarketDataValidationError,
    OrderLevelChange, OrderLevelEvent, OrderLevelInterruption, OrderLevelReset, OrderLevelSnapshot,
    OrderLevelUpdate, RestingOrder,
};
use aeris_observability::diagnostic;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use super::{
    AggressorSide, BTreeMap, BookSide, DecodedDepthByOrderEndEvent, DecodedMarketMessage,
    DepthByOrderMutation, DepthByOrderMutationKind, DepthByOrderSide, DepthByOrderSnapshotLevel,
    DepthByOrderSnapshotMessage, DepthByOrderSnapshotOutcome, DepthByOrderSnapshotRejection,
    DepthLevel, DepthSnapshot, EventMetadata, MAXIMUM_INSTRUMENTS, MarketEvent, MarketIdentity,
    MarketTrade, PROVIDER_ID, ProviderInvalidationReason, ProviderTimestamp, QualifiedTimestamp,
    QuoteLevel, QuoteSideUpdate, RetryDisposition, RithmicProviderConfig,
    RithmicProviderInstrument, SessionGeneration, SourceTimestamp, SystemTime, TopOfBookQuote,
    TradeAggressor, UNIX_EPOCH, VecDeque,
};

pub(super) struct CanonicalSessionState {
    instruments: Vec<RithmicProviderInstrument>,
    generation: SessionGeneration,
    quotes: BTreeMap<String, QuoteState>,
    /// Aggregate books, kept only for instruments subscribed to price levels.
    price_books: BTreeMap<String, PriceLevelBook>,
    /// Order-level books, kept only for instruments with order-level demand.
    mbo_books: BTreeMap<String, MboBookAssembler>,
    depth_resnapshots: VecDeque<String>,
    /// Order-level resets raised outside one frame's conversion (a refused
    /// subscription or an ambiguous completion), at most one per instrument.
    order_level_resets: VecDeque<(String, OrderLevelInterruption)>,
    /// Order-level books removed with their instrument and the image each
    /// last published, owed a released reset so no consumer keeps presenting
    /// an image the provider no longer maintains.
    released_order_levels: VecDeque<(RithmicProviderInstrument, u64)>,
    /// Highest image any removed order-level book published. A book installed
    /// later in the session numbers its images above it, so a consumer never
    /// mistakes a re-added instrument's first image for a retired one.
    retired_order_level_image: u64,
    /// Instrument and condition pairs already reported as withheld, so each
    /// is logged once per session.
    withheld: BTreeSet<(String, WithheldFrame)>,
    /// Set once a snapshot completion without an identity arrived while more
    /// than one book was owed a response. Which response it ended is unknown,
    /// so no later anonymous completion is attributed in this session.
    anonymous_completions_ambiguous: bool,
}

/// Clocks carried by one provider frame, published together so an event never
/// pairs one frame's gateway time with another frame's exchange time.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct FrameClocks {
    gateway: Option<ProviderTimestamp>,
    source: Option<SourceTimestamp>,
}

impl FrameClocks {
    const fn of_update(update: &crate::DepthByOrderUpdate) -> Self {
        Self {
            gateway: update.timestamp,
            source: update.source_timestamp,
        }
    }

    const fn is_empty(self) -> bool {
        self.gateway.is_none() && self.source.is_none()
    }
}

/// Provider frames the canonical market model cannot represent.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum WithheldFrame {
    CrossedBook,
    NonPositivePrice,
    ZeroQuantity,
}

#[derive(Clone, Copy, Default)]
struct QuoteState {
    bid: Option<QuoteLevel>,
    ask: Option<QuoteLevel>,
}

pub(super) const MAXIMUM_MBO_ORDERS: usize = MAXIMUM_ORDER_LEVEL_ORDERS;
const MAXIMUM_CANONICAL_DEPTH_LEVELS: usize = crate::book::MAXIMUM_PRICE_LEVELS_PER_SIDE;
const MAXIMUM_PENDING_MBO_MUTATIONS: usize = 131_072;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MboOrder {
    side: BookSide,
    price: i64,
    quantity: i64,
    priority: u64,
}

struct MboBookAssembler {
    identity: MarketIdentity,
    /// The last image published this session. It survives recovery so a
    /// newer image is always distinguishable from a discarded one.
    image: u64,
    /// Updates published against `image`.
    revision: u64,
    orders: BTreeMap<String, MboOrder>,
    last_provider_sequence: Option<u64>,
    snapshot_orders: BTreeMap<String, MboOrder>,
    snapshot_sequence: Option<u64>,
    pending_updates: VecDeque<crate::DepthByOrderUpdate>,
    /// Buffered updates that arrived before the first snapshot level. They
    /// precede the snapshot response on the ordered connection, so an image
    /// without any sequence cannot be ordered against them and drops them.
    updates_before_snapshot: Option<usize>,
    pending_mutations: usize,
    ready: bool,
    /// Set once snapshot recovery is exhausted; order-level depth stays off
    /// for the rest of this session and a new session retries it.
    disabled: bool,
    resnapshot_times: VecDeque<Instant>,
    snapshot_request: SnapshotRequest,
}

/// The covering snapshot response a book is owed. The session requests one
/// snapshot per book at a time and responses arrive in request order, so a
/// book owes at most one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SnapshotRequest {
    Idle,
    /// The owed response builds the next image.
    Owed,
    /// The provider refused a level of the owed response; it completes as a
    /// rejection whatever its terminal code says.
    Rejected(DepthByOrderSnapshotRejection),
    /// The owed response predates a recovery. It drains without touching the
    /// image, and the fresh snapshot is requested once it completes, so a
    /// response cut short by a reset can never complete a new image.
    Abandoned,
}

/// Why an order-level image was discarded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrderLevelFault {
    /// The provider stream contradicted the image (gap, duplicate, overflow).
    Inconsistent(ProviderInvalidationReason),
    /// The provider refused the covering snapshot request.
    SnapshotRejected(DepthByOrderSnapshotRejection),
}

impl From<(ProviderInvalidationReason, RetryDisposition)> for OrderLevelFault {
    fn from((reason, _): (ProviderInvalidationReason, RetryDisposition)) -> Self {
        Self::Inconsistent(reason)
    }
}

impl OrderLevelFault {
    const fn interruption(self) -> OrderLevelInterruption {
        match self {
            Self::Inconsistent(ProviderInvalidationReason::SequenceGap) => {
                OrderLevelInterruption::SequenceGap
            }
            Self::Inconsistent(ProviderInvalidationReason::QueueOverflow) => {
                OrderLevelInterruption::Overflow
            }
            Self::Inconsistent(_) => OrderLevelInterruption::InvalidUpdate,
            Self::SnapshotRejected(_) => OrderLevelInterruption::SnapshotRefused,
        }
    }
}

enum Recovery {
    RequestSnapshot,
    AfterAbandonedResponse,
    Disabled,
}

const MAXIMUM_DEPTH_RESNAPSHOTS: usize = 3;
const DEPTH_RESNAPSHOT_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug)]
enum MboBookOutcome {
    Pending,
    IgnoredStale,
    /// An abandoned response completed; the deferred snapshot is now due.
    AbandonedResponseDrained,
    Publish(OrderLevelPublication),
}

/// Order-level state to publish, before event metadata is attached.
#[derive(Debug)]
enum OrderLevelPublication {
    Snapshot {
        clocks: FrameClocks,
        image: u64,
        provider_sequence: Option<u64>,
        orders: Vec<RestingOrder>,
    },
    Update {
        clocks: FrameClocks,
        image: u64,
        previous_revision: u64,
        revision: u64,
        provider_sequence: Option<u64>,
        changes: Vec<OrderLevelChange>,
    },
    Reset {
        image: u64,
        reason: OrderLevelInterruption,
        resnapshot: bool,
    },
}

impl MboBookAssembler {
    fn new(identity: MarketIdentity) -> Self {
        Self {
            identity,
            image: 0,
            revision: 0,
            orders: BTreeMap::new(),
            last_provider_sequence: None,
            snapshot_orders: BTreeMap::new(),
            snapshot_sequence: None,
            pending_updates: VecDeque::new(),
            updates_before_snapshot: None,
            pending_mutations: 0,
            ready: false,
            disabled: false,
            resnapshot_times: VecDeque::new(),
            // The session requests a covering snapshot for every book it installs.
            snapshot_request: SnapshotRequest::Owed,
        }
    }

    #[cfg(test)]
    const fn ready(&self) -> bool {
        self.ready && !self.disabled
    }

    #[cfg(test)]
    const fn awaiting_snapshot(&self) -> bool {
        !self.ready && !self.disabled
    }

    fn owes_snapshot_response(&self) -> bool {
        self.snapshot_request != SnapshotRequest::Idle
    }

    /// Discards the order-level image after a fault. A response still owed is
    /// abandoned and the fresh snapshot waits for it to drain; once the
    /// recovery budget for the rolling window is spent, order-level depth is
    /// disabled instead.
    fn recover(&mut self, now: Instant) -> Recovery {
        let owed = self.owes_snapshot_response();
        let mut resnapshot_times = std::mem::take(&mut self.resnapshot_times);
        while resnapshot_times
            .front()
            .is_some_and(|time| now.saturating_duration_since(*time) >= DEPTH_RESNAPSHOT_WINDOW)
        {
            resnapshot_times.pop_front();
        }
        let image = self.image;
        *self = Self::new(self.identity.clone());
        self.image = image;
        self.snapshot_request = if owed {
            SnapshotRequest::Abandoned
        } else {
            SnapshotRequest::Owed
        };
        if resnapshot_times.len() >= MAXIMUM_DEPTH_RESNAPSHOTS {
            self.disabled = true;
            if !owed {
                self.snapshot_request = SnapshotRequest::Idle;
            }
            return Recovery::Disabled;
        }
        resnapshot_times.push_back(now);
        self.resnapshot_times = resnapshot_times;
        if owed {
            Recovery::AfterAbandonedResponse
        } else {
            Recovery::RequestSnapshot
        }
    }

    /// Requests the snapshot deferred behind a drained response. Returns
    /// false when the book was disabled meanwhile.
    fn resume_after_drained_response(&mut self) -> bool {
        if self.disabled {
            return false;
        }
        self.snapshot_request = SnapshotRequest::Owed;
        true
    }

    /// Turns order-level depth off for the rest of the session.
    fn disable(&mut self) {
        let image = self.image;
        *self = Self::new(self.identity.clone());
        self.image = image;
        self.disabled = true;
        self.snapshot_request = SnapshotRequest::Idle;
    }

    fn reject_snapshot_level(&mut self, rejection: DepthByOrderSnapshotRejection) {
        if self.snapshot_request == SnapshotRequest::Owed {
            self.snapshot_request = SnapshotRequest::Rejected(rejection);
        }
    }

    /// Settles the covering snapshot response this book is owed.
    fn complete_snapshot(
        &mut self,
        outcome: DepthByOrderSnapshotOutcome,
        completion_sequence: Option<u64>,
        price_scale: u8,
        quantity_scale: u8,
    ) -> Result<MboBookOutcome, OrderLevelFault> {
        match std::mem::replace(&mut self.snapshot_request, SnapshotRequest::Idle) {
            SnapshotRequest::Idle => Ok(MboBookOutcome::IgnoredStale),
            SnapshotRequest::Abandoned => Ok(MboBookOutcome::AbandonedResponseDrained),
            SnapshotRequest::Rejected(rejection) => {
                Err(OrderLevelFault::SnapshotRejected(rejection))
            }
            SnapshotRequest::Owed => match outcome {
                DepthByOrderSnapshotOutcome::Rejected(rejection) => {
                    Err(OrderLevelFault::SnapshotRejected(rejection))
                }
                DepthByOrderSnapshotOutcome::Accepted => self
                    .finish_snapshot(completion_sequence, price_scale, quantity_scale)
                    .map_err(OrderLevelFault::from),
            },
        }
    }

    fn accept_update(
        &mut self,
        update: crate::DepthByOrderUpdate,
        price_scale: u8,
        quantity_scale: u8,
    ) -> Result<MboBookOutcome, (ProviderInvalidationReason, RetryDisposition)> {
        if update.identity != self.identity {
            return Err(malformed());
        }
        if self.disabled {
            return Ok(MboBookOutcome::IgnoredStale);
        }
        if !self.ready {
            if let (Some(sequence), Some(snapshot)) =
                (update.sequence_number, self.snapshot_sequence)
                && sequence <= snapshot
            {
                return Ok(MboBookOutcome::IgnoredStale);
            }
            self.pending_mutations = self
                .pending_mutations
                .checked_add(update.mutations.len())
                .filter(|count| *count <= MAXIMUM_PENDING_MBO_MUTATIONS)
                .ok_or((
                    ProviderInvalidationReason::QueueOverflow,
                    RetryDisposition::Transient,
                ))?;
            self.pending_updates.push_back(update);
            return Ok(MboBookOutcome::Pending);
        }
        let clocks = FrameClocks::of_update(&update);
        let provider_sequence = update.sequence_number;
        match self.apply_live_update(update, price_scale, quantity_scale)? {
            Some(changes) if !changes.is_empty() => {
                let previous_revision = self.revision;
                self.revision = self.revision.checked_add(1).ok_or_else(malformed)?;
                Ok(MboBookOutcome::Publish(OrderLevelPublication::Update {
                    clocks,
                    image: self.image,
                    previous_revision,
                    revision: self.revision,
                    provider_sequence,
                    changes,
                }))
            }
            Some(_) | None => Ok(MboBookOutcome::IgnoredStale),
        }
    }

    /// Applies one update to the ready image in arrival order and returns
    /// its changes, or `None` for a stale update that was not applied.
    fn apply_live_update(
        &mut self,
        update: crate::DepthByOrderUpdate,
        price_scale: u8,
        quantity_scale: u8,
    ) -> Result<Option<Vec<OrderLevelChange>>, (ProviderInvalidationReason, RetryDisposition)> {
        if update.mutations.len() > MAXIMUM_ORDER_LEVEL_CHANGES {
            return Err((
                ProviderInvalidationReason::QueueOverflow,
                RetryDisposition::Transient,
            ));
        }
        // An unsequenced frame applies in arrival order without consuming a
        // sequence. If it did consume one, the next sequenced frame is a gap.
        // Continuity starts at the first sequence observed after an image
        // that carried none.
        if let (Some(sequence), Some(last)) = (update.sequence_number, self.last_provider_sequence)
        {
            if sequence <= last {
                return Ok(None);
            }
            if sequence != last.saturating_add(1) {
                return Err((
                    ProviderInvalidationReason::SequenceGap,
                    RetryDisposition::Transient,
                ));
            }
        }
        let mut changes = Vec::with_capacity(update.mutations.len());
        for mutation in update.mutations {
            changes.push(self.apply_mutation(mutation, price_scale, quantity_scale)?);
        }
        if let Some(sequence) = update.sequence_number {
            self.last_provider_sequence = Some(sequence);
        }
        Ok(Some(changes))
    }

    fn accept_snapshot_level(
        &mut self,
        level: DepthByOrderSnapshotLevel,
        price_scale: u8,
        quantity_scale: u8,
    ) -> Result<MboBookOutcome, (ProviderInvalidationReason, RetryDisposition)> {
        if level.identity != self.identity {
            return Err(malformed());
        }
        if self.disabled
            || matches!(
                self.snapshot_request,
                SnapshotRequest::Abandoned | SnapshotRequest::Rejected(_)
            )
        {
            return Ok(MboBookOutcome::IgnoredStale);
        }
        if self.ready {
            return if level
                .sequence_number
                .zip(self.last_provider_sequence)
                .is_some_and(|(sequence, last)| sequence <= last)
            {
                Ok(MboBookOutcome::IgnoredStale)
            } else {
                Err((
                    ProviderInvalidationReason::SequenceGap,
                    RetryDisposition::Transient,
                ))
            };
        }
        // Levels without a sequence contribute orders but no baseline evidence;
        // every level that carries one must agree.
        if let Some(sequence) = level.sequence_number {
            if self
                .snapshot_sequence
                .is_some_and(|snapshot| snapshot != sequence)
            {
                return Err((
                    ProviderInvalidationReason::SequenceGap,
                    RetryDisposition::Transient,
                ));
            }
            self.snapshot_sequence = Some(sequence);
        }
        if self.updates_before_snapshot.is_none() {
            self.updates_before_snapshot = Some(self.pending_updates.len());
        }
        let side = match level.side {
            DepthByOrderSide::Bid => BookSide::Bid,
            DepthByOrderSide::Ask => BookSide::Ask,
        };
        let price = fixed_price(level.price, price_scale)?;
        for snapshot_order in level.orders {
            // A repeated order id means the response contradicts itself.
            if self
                .snapshot_orders
                .contains_key(&snapshot_order.exchange_order_id)
            {
                return Err((
                    ProviderInvalidationReason::SequenceGap,
                    RetryDisposition::Transient,
                ));
            }
            if self.snapshot_orders.len() >= MAXIMUM_MBO_ORDERS {
                return Err((
                    ProviderInvalidationReason::QueueOverflow,
                    RetryDisposition::Transient,
                ));
            }
            let order = MboOrder {
                side,
                price,
                quantity: fixed_quantity(snapshot_order.size, quantity_scale)?,
                priority: snapshot_order.priority,
            };
            self.snapshot_orders
                .insert(snapshot_order.exchange_order_id, order);
        }
        Ok(MboBookOutcome::Pending)
    }

    fn finish_snapshot(
        &mut self,
        completion_sequence: Option<u64>,
        price_scale: u8,
        quantity_scale: u8,
    ) -> Result<MboBookOutcome, (ProviderInvalidationReason, RetryDisposition)> {
        if self.ready || self.disabled {
            return Ok(MboBookOutcome::IgnoredStale);
        }
        let baseline = match (self.snapshot_sequence, completion_sequence) {
            (Some(snapshot), Some(completion)) if snapshot != completion => {
                return Err((
                    ProviderInvalidationReason::SequenceGap,
                    RetryDisposition::Transient,
                ));
            }
            (Some(snapshot), _) => Some(snapshot),
            (None, completion) => completion,
        };
        let updates_before_snapshot = self
            .updates_before_snapshot
            .take()
            .unwrap_or(self.pending_updates.len());
        self.orders = std::mem::take(&mut self.snapshot_orders);
        self.last_provider_sequence = baseline;
        self.snapshot_sequence = None;
        self.ready = true;
        self.pending_mutations = 0;
        let pending_updates = std::mem::take(&mut self.pending_updates);
        if baseline.is_none() {
            return self.replay_after_unsequenced_image(
                pending_updates,
                updates_before_snapshot,
                price_scale,
                quantity_scale,
            );
        }
        let mut clocks = FrameClocks::default();
        // An unsequenced buffered update is placeable only after a sequenced
        // update beyond the baseline; before that it may predate the image.
        let mut past_baseline = false;
        for update in pending_updates {
            if update.identity != self.identity {
                return Err(malformed());
            }
            match update.sequence_number {
                Some(sequence) => {
                    if sequence <= self.last_provider_sequence.unwrap_or(0) {
                        continue;
                    }
                    let expected = self.last_provider_sequence.unwrap_or(0).saturating_add(1);
                    if sequence != expected {
                        return Err((
                            ProviderInvalidationReason::SequenceGap,
                            RetryDisposition::Transient,
                        ));
                    }
                    self.last_provider_sequence = Some(sequence);
                    past_baseline = true;
                }
                None if !past_baseline => {
                    return Err((
                        ProviderInvalidationReason::SequenceGap,
                        RetryDisposition::Transient,
                    ));
                }
                None => {}
            }
            let frame = FrameClocks::of_update(&update);
            for mutation in update.mutations {
                self.apply_mutation(mutation, price_scale, quantity_scale)?;
            }
            if !frame.is_empty() {
                clocks = frame;
            }
        }
        self.snapshot(clocks)
    }

    /// Rithmic Test sends order-level snapshots with no sequence on any level
    /// or on the completion. Updates interleaved with the snapshot levels
    /// replay in arrival order. One the image already reflects either reapplies
    /// the same order state or contradicts the image through the order-id
    /// checks, which requests a resnapshot.
    fn replay_after_unsequenced_image(
        &mut self,
        pending_updates: VecDeque<crate::DepthByOrderUpdate>,
        updates_before_snapshot: usize,
        price_scale: u8,
        quantity_scale: u8,
    ) -> Result<MboBookOutcome, (ProviderInvalidationReason, RetryDisposition)> {
        let mut clocks = FrameClocks::default();
        for update in pending_updates.into_iter().skip(updates_before_snapshot) {
            if update.identity != self.identity {
                return Err(malformed());
            }
            let frame = FrameClocks::of_update(&update);
            if self
                .apply_live_update(update, price_scale, quantity_scale)?
                .is_some()
                && !frame.is_empty()
            {
                clocks = frame;
            }
        }
        self.snapshot(clocks)
    }

    fn finish_initial_image(
        &mut self,
        event: &DecodedDepthByOrderEndEvent,
    ) -> Result<MboBookOutcome, (ProviderInvalidationReason, RetryDisposition)> {
        if !event
            .identities
            .iter()
            .any(|identity| identity == &self.identity)
        {
            return Ok(MboBookOutcome::Pending);
        }
        if self.disabled {
            return Ok(MboBookOutcome::IgnoredStale);
        }
        if self.ready {
            // Without a sequenced baseline the marker carries no evidence of a gap.
            return if event.sequence_number.is_none_or(|sequence| {
                self.last_provider_sequence
                    .is_none_or(|last| sequence <= last)
            }) {
                Ok(MboBookOutcome::IgnoredStale)
            } else {
                Err((
                    ProviderInvalidationReason::SequenceGap,
                    RetryDisposition::Transient,
                ))
            };
        }
        // The explicit 115/116 snapshot owns readiness. Template 161 may
        // interleave while that snapshot is in flight, but it cannot make a
        // partially observed live stream authoritative.
        Ok(MboBookOutcome::Pending)
    }

    fn apply_mutation(
        &mut self,
        mutation: DepthByOrderMutation,
        price_scale: u8,
        quantity_scale: u8,
    ) -> Result<OrderLevelChange, (ProviderInvalidationReason, RetryDisposition)> {
        let gap = || {
            (
                ProviderInvalidationReason::SequenceGap,
                RetryDisposition::Transient,
            )
        };
        let side = match mutation.side {
            DepthByOrderSide::Bid => BookSide::Bid,
            DepthByOrderSide::Ask => BookSide::Ask,
        };
        let price = fixed_price(mutation.price, price_scale)?;
        if mutation.kind == DepthByOrderMutationKind::New {
            // Adding an order already resting means its removal was missed.
            if self.orders.contains_key(&mutation.exchange_order_id) {
                return Err(gap());
            }
            if self.orders.len() >= MAXIMUM_MBO_ORDERS {
                return Err((
                    ProviderInvalidationReason::QueueOverflow,
                    RetryDisposition::Transient,
                ));
            }
            let order = MboOrder {
                side,
                price,
                quantity: fixed_quantity(mutation.size, quantity_scale)?,
                priority: mutation.priority,
            };
            let change = OrderLevelChange::Add(resting_order(&mutation.exchange_order_id, order));
            self.orders.insert(mutation.exchange_order_id, order);
            return Ok(change);
        }
        // A change or delete must name a resting order on the same side, at the
        // previous price when the provider states one.
        let Some(previous) = self.orders.remove(&mutation.exchange_order_id) else {
            return Err(gap());
        };
        if previous.side != side {
            return Err(gap());
        }
        if let Some(previous_price) = mutation.previous_price
            && fixed_price(previous_price, price_scale)? != previous.price
        {
            return Err(gap());
        }
        if mutation.kind == DepthByOrderMutationKind::Delete {
            return Ok(OrderLevelChange::Delete {
                order_id: mutation.exchange_order_id,
                side: previous.side,
                price: previous.price,
            });
        }
        let updated = MboOrder {
            side,
            price,
            quantity: fixed_quantity(mutation.size, quantity_scale)?,
            priority: mutation.priority,
        };
        let change = OrderLevelChange::Change(resting_order(&mutation.exchange_order_id, updated));
        self.orders.insert(mutation.exchange_order_id, updated);
        Ok(change)
    }

    /// Publishes the assembled image as the next covering image.
    fn snapshot(
        &mut self,
        clocks: FrameClocks,
    ) -> Result<MboBookOutcome, (ProviderInvalidationReason, RetryDisposition)> {
        self.image = self.image.checked_add(1).ok_or_else(malformed)?;
        self.revision = 0;
        Ok(MboBookOutcome::Publish(OrderLevelPublication::Snapshot {
            clocks,
            image: self.image,
            provider_sequence: self.last_provider_sequence,
            orders: self
                .orders
                .iter()
                .map(|(order_id, order)| resting_order(order_id, *order))
                .collect(),
        }))
    }
}

fn resting_order(order_id: &str, order: MboOrder) -> RestingOrder {
    RestingOrder {
        order_id: order_id.to_string(),
        side: order.side,
        price: order.price,
        quantity: order.quantity,
        priority: order.priority,
    }
}

/// Order-level inconsistencies and refused snapshots recover the book with a
/// fresh snapshot on the live connection instead of invalidating the session
/// and its chart streams. No image becomes authoritative until a covering
/// snapshot completes, and every discarded image publishes a reset.
fn order_level_outcome<E: Into<OrderLevelFault>>(
    assembler: &mut MboBookAssembler,
    outcome: Result<MboBookOutcome, E>,
    instrument_id: &str,
    depth_resnapshots: &mut VecDeque<String>,
) -> Option<OrderLevelPublication> {
    let fault = match outcome {
        Ok(MboBookOutcome::AbandonedResponseDrained) => {
            if assembler.resume_after_drained_response() {
                queue_depth_resnapshot(depth_resnapshots, instrument_id);
            }
            return None;
        }
        Ok(MboBookOutcome::Publish(publication)) => return Some(publication),
        Ok(MboBookOutcome::Pending | MboBookOutcome::IgnoredStale) => return None,
        Err(fault) => fault.into(),
    };
    Some(recover_order_levels(
        assembler,
        fault,
        instrument_id,
        depth_resnapshots,
    ))
}

fn recover_order_levels(
    assembler: &mut MboBookAssembler,
    fault: OrderLevelFault,
    instrument_id: &str,
    depth_resnapshots: &mut VecDeque<String>,
) -> OrderLevelPublication {
    let resnapshot = match assembler.recover(Instant::now()) {
        Recovery::RequestSnapshot => {
            diagnostic!("Aeris Rithmic order-level depth resnapshot requested: {fault:?}");
            queue_depth_resnapshot(depth_resnapshots, instrument_id);
            true
        }
        Recovery::AfterAbandonedResponse => {
            diagnostic!(
                "Aeris Rithmic order-level depth resnapshot requested after the in-flight response drains: {fault:?}"
            );
            true
        }
        Recovery::Disabled => {
            diagnostic!(
                "Aeris Rithmic order-level depth disabled for this session after repeated {fault:?}"
            );
            false
        }
    };
    OrderLevelPublication::Reset {
        image: assembler.image,
        reason: fault.interruption(),
        resnapshot,
    }
}
fn queue_depth_resnapshot(depth_resnapshots: &mut VecDeque<String>, instrument_id: &str) {
    if !depth_resnapshots
        .iter()
        .any(|pending| pending == instrument_id)
    {
        depth_resnapshots.push_back(instrument_id.to_string());
    }
}

impl CanonicalSessionState {
    pub(super) fn instruments(&self) -> &[RithmicProviderInstrument] {
        &self.instruments
    }

    pub(super) fn new(config: &RithmicProviderConfig, generation: SessionGeneration) -> Self {
        let mut state = Self {
            instruments: Vec::new(),
            generation,
            quotes: BTreeMap::new(),
            price_books: BTreeMap::new(),
            mbo_books: BTreeMap::new(),
            depth_resnapshots: VecDeque::new(),
            order_level_resets: VecDeque::new(),
            released_order_levels: VecDeque::new(),
            retired_order_level_image: 0,
            withheld: BTreeSet::new(),
            anonymous_completions_ambiguous: false,
        };
        for instrument in &config.instruments {
            state.install_books(instrument);
        }
        state.instruments.clone_from(&config.instruments);
        state
    }

    fn install_books(&mut self, instrument: &RithmicProviderInstrument) {
        let instrument_id = &instrument.descriptor.instrument_id;
        if instrument.order_book {
            self.price_books
                .insert(instrument_id.clone(), PriceLevelBook::default());
        }
        if instrument.order_levels {
            let mut assembler = MboBookAssembler::new(MarketIdentity {
                symbol: instrument.descriptor.provider_symbol.clone(),
                exchange: instrument.descriptor.venue_id.clone(),
            });
            assembler.image = self
                .mbo_books
                .values()
                .map(|book| book.image)
                .fold(self.retired_order_level_image, u64::max);
            self.mbo_books.insert(instrument_id.clone(), assembler);
        }
    }

    pub(super) fn convert(
        &mut self,
        message: DecodedMarketMessage,
        source_ordinal: u64,
        received_unix_nanos: i64,
    ) -> Result<Option<MarketEvent>, (ProviderInvalidationReason, RetryDisposition)> {
        match message {
            DecodedMarketMessage::Trade(trade) => {
                self.convert_trade(&trade, source_ordinal, received_unix_nanos)
            }
            DecodedMarketMessage::Quote(quote) => {
                self.convert_quote(&quote, source_ordinal, received_unix_nanos)
            }
            DecodedMarketMessage::OrderBook(update) => {
                self.convert_price_levels(&update, source_ordinal, received_unix_nanos)
            }
            DecodedMarketMessage::DepthByOrderSnapshot(DepthByOrderSnapshotMessage::Level(
                level,
            )) => {
                let instrument = self.instrument(&level.identity)?.clone();
                self.advance_order_levels(
                    &instrument,
                    source_ordinal,
                    received_unix_nanos,
                    |book, price_scale, quantity_scale| {
                        book.accept_snapshot_level(level, price_scale, quantity_scale)
                    },
                )
            }
            DecodedMarketMessage::DepthByOrderSnapshot(
                DepthByOrderSnapshotMessage::LevelRejected {
                    identity,
                    rejection,
                },
            ) => self.reject_order_level_snapshot(identity.as_ref(), rejection),
            DecodedMarketMessage::DepthByOrderSnapshot(DepthByOrderSnapshotMessage::Complete {
                outcome,
                identity,
                sequence_number,
            }) => {
                let instrument = if let Some(identity) = identity.as_ref() {
                    self.instrument(identity)?.clone()
                } else if let Some(instrument) = self.anonymous_completion_owner() {
                    instrument
                } else {
                    return Ok(None);
                };
                self.advance_order_levels(
                    &instrument,
                    source_ordinal,
                    received_unix_nanos,
                    |book, price_scale, quantity_scale| {
                        book.complete_snapshot(
                            outcome,
                            sequence_number,
                            price_scale,
                            quantity_scale,
                        )
                    },
                )
            }
            DecodedMarketMessage::DepthByOrder(update) => {
                let instrument = self.instrument(&update.identity)?.clone();
                self.advance_order_levels(
                    &instrument,
                    source_ordinal,
                    received_unix_nanos,
                    |book, price_scale, quantity_scale| {
                        book.accept_update(update, price_scale, quantity_scale)
                    },
                )
            }
            DecodedMarketMessage::DepthByOrderEnd(event) => {
                let matching = self
                    .instruments
                    .iter()
                    .filter(|instrument| {
                        event
                            .identities
                            .iter()
                            .any(|identity| instrument.identity_matches(identity))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let [instrument] = matching.as_slice() else {
                    return Err(malformed());
                };
                self.advance_order_levels(
                    instrument,
                    source_ordinal,
                    received_unix_nanos,
                    |book, _, _| book.finish_initial_image(&event),
                )
            }
        }
    }

    fn convert_trade(
        &mut self,
        trade: &crate::TradeUpdate,
        source_ordinal: u64,
        received_unix_nanos: i64,
    ) -> Result<Option<MarketEvent>, (ProviderInvalidationReason, RetryDisposition)> {
        let instrument = self.instrument(&trade.identity)?;
        if trade.size == 0 {
            let instrument_id = instrument.descriptor.instrument_id.clone();
            self.withhold(&instrument_id, WithheldFrame::ZeroQuantity);
            return Ok(None);
        }
        let event = MarketEvent::Trade(MarketTrade {
            metadata: metadata(
                instrument,
                self.generation,
                source_ordinal,
                FrameClocks {
                    gateway: Some(trade.timestamp),
                    source: trade.source_timestamp,
                },
                received_unix_nanos,
            )?,
            trade_id: format!("rithmic-local-{}-{source_ordinal}", self.generation.get()),
            price: fixed_price(trade.price, instrument.descriptor.price_scale)?,
            quantity: fixed_quantity(trade.size, instrument.descriptor.quantity_scale)?,
            aggressor: match trade.aggressor {
                Some(TradeAggressor::Buy) => AggressorSide::Buy,
                Some(TradeAggressor::Sell) => AggressorSide::Sell,
                None => AggressorSide::Unknown,
            },
        });
        self.publishable(event)
    }

    fn convert_quote(
        &mut self,
        quote: &crate::QuoteUpdate,
        source_ordinal: u64,
        received_unix_nanos: i64,
    ) -> Result<Option<MarketEvent>, (ProviderInvalidationReason, RetryDisposition)> {
        let instrument = self.instrument(&quote.identity)?.clone();
        let state = self
            .quotes
            .entry(instrument.descriptor.instrument_id.clone())
            .or_default();
        if quote.is_snapshot {
            *state = QuoteState::default();
        }
        apply_quote_side(&mut state.bid, quote.bid);
        apply_quote_side(&mut state.ask, quote.ask);
        // Best bid/offer frames carry only the gateway clock.
        let event = MarketEvent::Quote(TopOfBookQuote {
            metadata: metadata(
                &instrument,
                self.generation,
                source_ordinal,
                FrameClocks {
                    gateway: Some(quote.timestamp),
                    source: None,
                },
                received_unix_nanos,
            )?,
            bid: state
                .bid
                .map(|level| canonical_quote_level(level, &instrument))
                .transpose()?
                .flatten(),
            ask: state
                .ask
                .map(|level| canonical_quote_level(level, &instrument))
                .transpose()?
                .flatten(),
        });
        self.publishable(event)
    }

    fn reject_order_level_snapshot(
        &mut self,
        identity: Option<&MarketIdentity>,
        rejection: DepthByOrderSnapshotRejection,
    ) -> Result<Option<MarketEvent>, (ProviderInvalidationReason, RetryDisposition)> {
        if let Some(identity) = identity {
            let instrument_id = self.instrument(identity)?.descriptor.instrument_id.clone();
            self.mbo_books
                .get_mut(&instrument_id)
                .ok_or_else(malformed)?
                .reject_snapshot_level(rejection);
        } else {
            // Unattributed: every owed response is treated as refused.
            for assembler in self.mbo_books.values_mut() {
                assembler.reject_snapshot_level(rejection);
            }
        }
        Ok(None)
    }

    /// Runs one order-level step on the instrument's book and publishes what
    /// it produced, recovering the book on a fault.
    fn advance_order_levels<E: Into<OrderLevelFault>>(
        &mut self,
        instrument: &RithmicProviderInstrument,
        source_ordinal: u64,
        received_unix_nanos: i64,
        step: impl FnOnce(&mut MboBookAssembler, u8, u8) -> Result<MboBookOutcome, E>,
    ) -> Result<Option<MarketEvent>, (ProviderInvalidationReason, RetryDisposition)> {
        let instrument_id = &instrument.descriptor.instrument_id;
        let assembler = self
            .mbo_books
            .get_mut(instrument_id)
            .ok_or_else(malformed)?;
        let outcome = step(
            assembler,
            instrument.descriptor.price_scale,
            instrument.descriptor.quantity_scale,
        );
        let publication = order_level_outcome(
            assembler,
            outcome,
            instrument_id,
            &mut self.depth_resnapshots,
        );
        self.publish_order_levels(instrument, publication, source_ordinal, received_unix_nanos)
    }

    /// Aggregate depth comes only from the provider's price-level book, never
    /// from order-level data. A frame sequence the book cannot represent
    /// replaces the session: only a new subscription image can rebuild it.
    fn convert_price_levels(
        &mut self,
        update: &crate::OrderBookUpdate,
        source_ordinal: u64,
        received_unix_nanos: i64,
    ) -> Result<Option<MarketEvent>, (ProviderInvalidationReason, RetryDisposition)> {
        let instrument = self.instrument(&update.identity)?.clone();
        let Some(book) = self
            .price_books
            .get_mut(&instrument.descriptor.instrument_id)
        else {
            return Ok(None);
        };
        let frame = PriceLevelFrame {
            kind: update.kind,
            bids: price_levels(&update.bids, &instrument)?,
            asks: price_levels(&update.asks, &instrument)?,
        };
        match book.apply(frame) {
            Ok(PriceLevelOutcome::Pending) => Ok(None),
            Ok(PriceLevelOutcome::Changed) => {
                let event = MarketEvent::DepthSnapshot(DepthSnapshot {
                    metadata: metadata(
                        &instrument,
                        self.generation,
                        source_ordinal,
                        FrameClocks {
                            gateway: update.timestamp,
                            source: None,
                        },
                        received_unix_nanos,
                    )?,
                    bids: book.bids().map(depth_level).collect(),
                    asks: book.asks().map(depth_level).collect(),
                });
                self.publishable(event)
            }
            Err(fault) => {
                diagnostic!("Aeris Rithmic aggregate book discarded: {fault:?}");
                Err((
                    match fault {
                        PriceLevelFault::BrokenGroup => ProviderInvalidationReason::SequenceGap,
                        PriceLevelFault::LevelLimitExceeded => {
                            ProviderInvalidationReason::QueueOverflow
                        }
                    },
                    RetryDisposition::Transient,
                ))
            }
        }
    }

    /// Order-level events are never withheld: a skipped update would break
    /// continuity. An event the canonical contract rejects discards the image
    /// and publishes its reset instead.
    fn publish_order_levels(
        &mut self,
        instrument: &RithmicProviderInstrument,
        publication: Option<OrderLevelPublication>,
        source_ordinal: u64,
        received_unix_nanos: i64,
    ) -> Result<Option<MarketEvent>, (ProviderInvalidationReason, RetryDisposition)> {
        let Some(publication) = publication else {
            return Ok(None);
        };
        let event = order_level_event(
            instrument,
            self.generation,
            publication,
            source_ordinal,
            received_unix_nanos,
        )?;
        if event.validate().is_ok() {
            return Ok(Some(MarketEvent::OrderLevels(event)));
        }
        let instrument_id = &instrument.descriptor.instrument_id;
        let assembler = self
            .mbo_books
            .get_mut(instrument_id)
            .ok_or_else(malformed)?;
        let reset = recover_order_levels(
            assembler,
            OrderLevelFault::Inconsistent(ProviderInvalidationReason::MalformedMessage),
            instrument_id,
            &mut self.depth_resnapshots,
        );
        let event = order_level_event(
            instrument,
            self.generation,
            reset,
            source_ordinal,
            received_unix_nanos,
        )?;
        event.validate().map_err(|_| malformed())?;
        Ok(Some(MarketEvent::OrderLevels(event)))
    }

    /// Turns order-level data off for an instrument whose order-level
    /// subscription the provider refused; its other streams continue.
    pub(super) fn refuse_order_levels(&mut self, instrument_id: &str) {
        if let Some(assembler) = self.mbo_books.get_mut(instrument_id) {
            assembler.disable();
            self.depth_resnapshots
                .retain(|pending| pending != instrument_id);
            self.queue_order_level_reset(
                instrument_id,
                OrderLevelInterruption::SubscriptionRefused,
            );
            diagnostic!("Aeris Rithmic order-level subscription refused; order levels stay off");
        }
    }

    fn queue_order_level_reset(&mut self, instrument_id: &str, reason: OrderLevelInterruption) {
        if !self
            .order_level_resets
            .iter()
            .any(|(pending, _)| pending == instrument_id)
        {
            self.order_level_resets
                .push_back((instrument_id.to_string(), reason));
        }
    }

    /// Resets raised outside a frame conversion, ready to publish.
    pub(super) fn take_order_level_resets(
        &mut self,
        source_ordinal: u64,
        received_unix_nanos: i64,
    ) -> Result<Vec<MarketEvent>, (ProviderInvalidationReason, RetryDisposition)> {
        let mut events =
            Vec::with_capacity(self.order_level_resets.len() + self.released_order_levels.len());
        while let Some((instrument_id, reason)) = self.order_level_resets.pop_front() {
            let (Some(instrument), Some(assembler)) = (
                self.instruments
                    .iter()
                    .find(|instrument| instrument.descriptor.instrument_id == instrument_id),
                self.mbo_books.get(&instrument_id),
            ) else {
                continue;
            };
            let event = order_level_event(
                instrument,
                self.generation,
                OrderLevelPublication::Reset {
                    image: assembler.image,
                    reason,
                    resnapshot: false,
                },
                source_ordinal,
                received_unix_nanos,
            )?;
            event.validate().map_err(|_| malformed())?;
            events.push(MarketEvent::OrderLevels(event));
        }
        while let Some((instrument, image)) = self.released_order_levels.pop_front() {
            let event = order_level_event(
                &instrument,
                self.generation,
                OrderLevelPublication::Reset {
                    image,
                    reason: OrderLevelInterruption::Released,
                    resnapshot: false,
                },
                source_ordinal,
                received_unix_nanos,
            )?;
            event.validate().map_err(|_| malformed())?;
            events.push(MarketEvent::OrderLevels(event));
        }
        Ok(events)
    }

    /// Attributes a snapshot completion without an identity to the one book
    /// owed a response. When several are owed, which response ended is
    /// unknown: every owed book turns order-level depth off for the session
    /// rather than risk completing an image from another book's response.
    fn anonymous_completion_owner(&mut self) -> Option<RithmicProviderInstrument> {
        let owed = self
            .instruments
            .iter()
            .filter(|instrument| {
                self.mbo_books
                    .get(&instrument.descriptor.instrument_id)
                    .is_some_and(MboBookAssembler::owes_snapshot_response)
            })
            .cloned()
            .collect::<Vec<_>>();
        match owed.as_slice() {
            [] => None,
            [instrument] if !self.anonymous_completions_ambiguous => Some(instrument.clone()),
            _ => {
                self.anonymous_completions_ambiguous = true;
                for instrument in &owed {
                    let instrument_id = &instrument.descriptor.instrument_id;
                    if let Some(assembler) = self.mbo_books.get_mut(instrument_id) {
                        assembler.disable();
                        self.queue_order_level_reset(
                            instrument_id,
                            OrderLevelInterruption::InvalidUpdate,
                        );
                    }
                }
                diagnostic!(
                    "Aeris Rithmic order-level depth disabled for this session: an unattributed snapshot completion arrived while {} books awaited one",
                    owed.len()
                );
                None
            }
        }
    }

    fn instrument(
        &self,
        identity: &MarketIdentity,
    ) -> Result<&RithmicProviderInstrument, (ProviderInvalidationReason, RetryDisposition)> {
        self.instruments
            .iter()
            .find(|instrument| instrument.identity_matches(identity))
            .ok_or_else(malformed)
    }

    /// Canonical events reject crossed or locked books, non-positive prices
    /// and zero quantities. A futures book in pre-open is legitimately crossed
    /// and spreads trade at or below zero, so such a frame is withheld instead
    /// of failing every stream on the session. The aggregate image stays
    /// intact and the next representable frame publishes.
    fn publishable(
        &mut self,
        event: MarketEvent,
    ) -> Result<Option<MarketEvent>, (ProviderInvalidationReason, RetryDisposition)> {
        let condition = match event.validate(MAXIMUM_CANONICAL_DEPTH_LEVELS) {
            Ok(()) => return Ok(Some(event)),
            Err(
                MarketDataValidationError::InvalidQuote | MarketDataValidationError::InvalidDepth,
            ) => WithheldFrame::CrossedBook,
            Err(MarketDataValidationError::InvalidPrice) => WithheldFrame::NonPositivePrice,
            Err(MarketDataValidationError::InvalidQuantity) => WithheldFrame::ZeroQuantity,
            Err(_) => return Err(malformed()),
        };
        self.withhold(&event.metadata().instrument_id, condition);
        Ok(None)
    }

    fn withhold(&mut self, instrument_id: &str, condition: WithheldFrame) {
        if self.withheld.insert((instrument_id.to_string(), condition)) {
            diagnostic!("Aeris Rithmic market frames withheld from publication: {condition:?}");
        }
    }

    pub(super) fn add_instrument(
        &mut self,
        instrument: RithmicProviderInstrument,
    ) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
        if self.instruments.len() >= MAXIMUM_INSTRUMENTS
            || self.instruments.iter().any(|current| {
                current.descriptor.instrument_id == instrument.descriptor.instrument_id
                    || (current.descriptor.provider_symbol == instrument.descriptor.provider_symbol
                        && current.descriptor.venue_id == instrument.descriptor.venue_id)
            })
        {
            return Err(malformed());
        }
        self.install_books(&instrument);
        self.instruments.push(instrument);
        Ok(())
    }

    pub(super) fn remove_instrument(&mut self, instrument_id: &str) {
        if let Some(book) = self.mbo_books.remove(instrument_id) {
            self.retired_order_level_image = self.retired_order_level_image.max(book.image);
            if let Some(instrument) = self
                .instruments
                .iter()
                .find(|instrument| instrument.descriptor.instrument_id == instrument_id)
            {
                self.released_order_levels
                    .push_back((instrument.clone(), book.image));
            }
        }
        self.instruments
            .retain(|instrument| instrument.descriptor.instrument_id != instrument_id);
        self.quotes.remove(instrument_id);
        self.price_books.remove(instrument_id);
        self.depth_resnapshots
            .retain(|pending| pending != instrument_id);
        self.order_level_resets
            .retain(|(pending, _)| pending != instrument_id);
        self.withheld
            .retain(|(withheld, _)| withheld != instrument_id);
    }

    /// Order-level books that need a fresh snapshot on the live connection.
    pub(super) fn take_depth_resnapshots(&mut self) -> Vec<RithmicProviderInstrument> {
        let mut instruments = Vec::new();
        while let Some(instrument_id) = self.depth_resnapshots.pop_front() {
            if let Some(instrument) = self
                .instruments
                .iter()
                .find(|instrument| instrument.descriptor.instrument_id == instrument_id)
            {
                instruments.push(instrument.clone());
            }
        }
        instruments
    }
}

fn price_levels(
    levels: &[crate::OrderBookLevel],
    instrument: &RithmicProviderInstrument,
) -> Result<Vec<PriceLevel>, (ProviderInvalidationReason, RetryDisposition)> {
    levels
        .iter()
        .map(|level| {
            Ok(PriceLevel {
                price: fixed_price(level.price, instrument.descriptor.price_scale)?,
                // Zero size removes the level.
                quantity: if level.size == 0 {
                    0
                } else {
                    fixed_quantity(level.size, instrument.descriptor.quantity_scale)?
                },
                order_count: level.orders,
            })
        })
        .collect()
}

const fn depth_level(level: PriceLevel) -> DepthLevel {
    DepthLevel {
        price: level.price,
        quantity: level.quantity,
        order_count: level.order_count,
    }
}

fn order_level_event(
    instrument: &RithmicProviderInstrument,
    generation: SessionGeneration,
    publication: OrderLevelPublication,
    source_ordinal: u64,
    received_unix_nanos: i64,
) -> Result<OrderLevelEvent, (ProviderInvalidationReason, RetryDisposition)> {
    let event_metadata = |clocks| {
        metadata(
            instrument,
            generation,
            source_ordinal,
            clocks,
            received_unix_nanos,
        )
    };
    Ok(match publication {
        OrderLevelPublication::Snapshot {
            clocks,
            image,
            provider_sequence,
            orders,
        } => OrderLevelEvent::Snapshot(OrderLevelSnapshot {
            metadata: event_metadata(clocks)?,
            image,
            revision: 0,
            provider_sequence,
            orders,
        }),
        OrderLevelPublication::Update {
            clocks,
            image,
            previous_revision,
            revision,
            provider_sequence,
            changes,
        } => OrderLevelEvent::Update(OrderLevelUpdate {
            metadata: event_metadata(clocks)?,
            image,
            previous_revision,
            revision,
            provider_sequence,
            changes,
        }),
        OrderLevelPublication::Reset {
            image,
            reason,
            resnapshot,
        } => OrderLevelEvent::Reset(OrderLevelReset {
            metadata: event_metadata(FrameClocks::default())?,
            image,
            reason,
            resnapshot,
        }),
    })
}
fn apply_quote_side(state: &mut Option<QuoteLevel>, update: QuoteSideUpdate) {
    match update {
        QuoteSideUpdate::Unchanged => {}
        QuoteSideUpdate::Cleared => *state = None,
        QuoteSideUpdate::Value(level) => *state = Some(level),
    }
}

/// The Rithmic gateway clock publishes as the provider time. The exchange time
/// is published only from the exchange clock, and only when its parts resolve
/// to one instant; it is never inferred from the gateway clock.
fn metadata(
    instrument: &RithmicProviderInstrument,
    generation: SessionGeneration,
    source_sequence: u64,
    clocks: FrameClocks,
    received_unix_nanos: i64,
) -> Result<EventMetadata, (ProviderInvalidationReason, RetryDisposition)> {
    Ok(EventMetadata {
        provider_id: PROVIDER_ID.to_string(),
        instrument_id: instrument.descriptor.instrument_id.clone(),
        entitlement_id: instrument.entitlement_id.clone(),
        source_sequence,
        session_generation: generation.get(),
        timestamps: QualifiedTimestamp {
            exchange_unix_nanos: clocks.source.and_then(SourceTimestamp::unix_nanos),
            provider_unix_nanos: clocks.gateway.map(provider_timestamp_nanos).transpose()?,
            received_unix_nanos,
        },
    })
}

/// A side quoted with zero size carries no resting liquidity, so it publishes
/// as absent rather than as a price the trading owner could fill against.
fn canonical_quote_level(
    level: QuoteLevel,
    instrument: &RithmicProviderInstrument,
) -> Result<Option<aeris_market_data::QuoteLevel>, (ProviderInvalidationReason, RetryDisposition)> {
    if level.size == 0 {
        return Ok(None);
    }
    Ok(Some(aeris_market_data::QuoteLevel {
        price: fixed_price(level.price, instrument.descriptor.price_scale)?,
        quantity: Some(fixed_quantity(
            level.size,
            instrument.descriptor.quantity_scale,
        )?),
        order_count: level.orders,
    }))
}

/// Non-positive prices convert exactly; canonical validation decides whether
/// the event carrying them can publish.
fn fixed_price(
    value: f64,
    scale: u8,
) -> Result<i64, (ProviderInvalidationReason, RetryDisposition)> {
    if !value.is_finite() || scale > 18 {
        return Err(malformed());
    }
    let rendered = format!("{value:.precision$}", precision = usize::from(scale));
    let round_trip = rendered.parse::<f64>().map_err(|_| malformed())?;
    let tolerance = f64::EPSILON * value.abs().max(1.0) * 4.0;
    if (round_trip - value).abs() > tolerance {
        return Err(malformed());
    }
    let digits = rendered.replace('.', "");
    digits.parse::<i64>().map_err(|_| malformed())
}

fn fixed_quantity(
    value: u32,
    scale: u8,
) -> Result<i64, (ProviderInvalidationReason, RetryDisposition)> {
    if value == 0 || scale > 18 {
        return Err(malformed());
    }
    i64::from(value)
        .checked_mul(10_i64.checked_pow(u32::from(scale)).ok_or_else(malformed)?)
        .ok_or_else(malformed)
}

fn provider_timestamp_nanos(
    timestamp: ProviderTimestamp,
) -> Result<i64, (ProviderInvalidationReason, RetryDisposition)> {
    i64::from(timestamp.seconds)
        .checked_mul(1_000_000_000)
        .and_then(|value| {
            i64::from(timestamp.microseconds)
                .checked_mul(1_000)
                .and_then(|microseconds| value.checked_add(microseconds))
        })
        .ok_or_else(malformed)
}

pub(super) fn unix_nanos_now() -> Result<i64, (ProviderInvalidationReason, RetryDisposition)> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| malformed())?
        .as_nanos();
    i64::try_from(nanos).map_err(|_| malformed())
}

pub(super) const fn malformed() -> (ProviderInvalidationReason, RetryDisposition) {
    (
        ProviderInvalidationReason::MalformedMessage,
        RetryDisposition::Terminal,
    )
}

#[cfg(test)]
mod tests {
    use super::super::tests::{generation, identity, instrument, timestamp};
    use super::*;
    use DepthByOrderSnapshotOutcome::Accepted;

    fn published_snapshot(outcome: MboBookOutcome) -> (u64, Option<u64>, Vec<RestingOrder>) {
        let MboBookOutcome::Publish(OrderLevelPublication::Snapshot {
            image,
            provider_sequence,
            orders,
            ..
        }) = outcome
        else {
            panic!("a covering snapshot must publish, got {outcome:?}");
        };
        (image, provider_sequence, orders)
    }

    fn published_update(outcome: MboBookOutcome) -> (u64, u64, Vec<OrderLevelChange>) {
        let MboBookOutcome::Publish(OrderLevelPublication::Update {
            previous_revision,
            revision,
            changes,
            ..
        }) = outcome
        else {
            panic!("a ready update must publish its changes, got {outcome:?}");
        };
        (previous_revision, revision, changes)
    }

    fn reset_of(publication: Option<OrderLevelPublication>) -> (OrderLevelInterruption, bool) {
        match publication {
            Some(OrderLevelPublication::Reset {
                reason, resnapshot, ..
            }) => (reason, resnapshot),
            other => panic!("a discarded image must publish a reset, got {other:?}"),
        }
    }

    fn resting(
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

    fn bid_delete(order_id: &str) -> OrderLevelChange {
        OrderLevelChange::Delete {
            order_id: order_id.to_string(),
            side: BookSide::Bid,
            price: 510_000,
        }
    }

    #[test]
    fn mbo_snapshot_atomically_replays_interleaved_live_updates() {
        let mut assembler = MboBookAssembler::new(identity());
        let live = crate::DepthByOrderUpdate {
            identity: identity(),
            sequence_number: Some(41),
            mutations: vec![DepthByOrderMutation {
                kind: DepthByOrderMutationKind::Change,
                side: DepthByOrderSide::Bid,
                price: 5_100.0,
                previous_price: Some(5_100.0),
                size: 5,
                priority: 13,
                exchange_order_id: "bid-1".to_string(),
            }],
            timestamp: Some(timestamp()),
            source_timestamp: None,
            jop_timestamp: None,
        };
        assert!(matches!(
            assembler
                .accept_update(live, 2, 2)
                .expect("pre-snapshot live update buffers"),
            MboBookOutcome::Pending
        ));
        assert!(!assembler.ready());

        for level in [
            DepthByOrderSnapshotLevel {
                identity: identity(),
                sequence_number: Some(40),
                side: DepthByOrderSide::Bid,
                price: 5_100.0,
                orders: vec![
                    crate::DepthByOrderSnapshotOrder {
                        size: 2,
                        priority: 11,
                        exchange_order_id: "bid-1".to_string(),
                    },
                    crate::DepthByOrderSnapshotOrder {
                        size: 3,
                        priority: 12,
                        exchange_order_id: "bid-2".to_string(),
                    },
                ],
            },
            DepthByOrderSnapshotLevel {
                identity: identity(),
                sequence_number: Some(40),
                side: DepthByOrderSide::Ask,
                price: 5_100.25,
                orders: vec![crate::DepthByOrderSnapshotOrder {
                    size: 4,
                    priority: 21,
                    exchange_order_id: "ask-1".to_string(),
                }],
            },
        ] {
            assert!(matches!(
                assembler
                    .accept_snapshot_level(level, 2, 2)
                    .expect("snapshot level installs into candidate state"),
                MboBookOutcome::Pending
            ));
        }

        let (image, provider_sequence, orders) = published_snapshot(
            assembler
                .complete_snapshot(Accepted, Some(40), 2, 2)
                .expect("covering snapshot completes and live delta replays"),
        );
        assert!(assembler.ready());
        assert_eq!((image, provider_sequence), (1, Some(41)));
        assert_eq!(
            orders,
            [
                resting("ask-1", BookSide::Ask, 510_025, 400, 21),
                resting("bid-1", BookSide::Bid, 510_000, 500, 13),
                resting("bid-2", BookSide::Bid, 510_000, 300, 12),
            ]
        );

        let mut delete = bid_update(Some(42), DepthByOrderMutationKind::Delete, "bid-2");
        delete.mutations[0].size = 0;
        let (previous_revision, revision, changes) = published_update(
            assembler
                .accept_update(delete, 2, 2)
                .expect("next live update applies"),
        );
        assert_eq!((previous_revision, revision), (0, 1));
        assert_eq!(changes, [bid_delete("bid-2")]);
    }

    #[test]
    fn dbo_end_marker_cannot_make_partial_live_state_authoritative() {
        let mut assembler = MboBookAssembler::new(identity());
        assembler
            .accept_update(
                crate::DepthByOrderUpdate {
                    identity: identity(),
                    sequence_number: Some(41),
                    mutations: vec![DepthByOrderMutation {
                        kind: DepthByOrderMutationKind::New,
                        side: DepthByOrderSide::Bid,
                        price: 5_100.0,
                        previous_price: None,
                        size: 2,
                        priority: 11,
                        exchange_order_id: "bid-1".to_string(),
                    }],
                    timestamp: Some(timestamp()),
                    source_timestamp: None,
                    jop_timestamp: None,
                },
                2,
                2,
            )
            .expect("live update buffers");
        assert!(matches!(
            assembler
                .finish_initial_image(&DecodedDepthByOrderEndEvent {
                    identities: vec![identity()],
                    sequence_number: Some(41),
                    timestamp: Some(timestamp()),
                })
                .expect("end marker is non-authoritative while snapshot is pending"),
            MboBookOutcome::Pending
        ));
        assert!(!assembler.ready());
    }

    fn bid_level(sequence_number: Option<u64>) -> DepthByOrderSnapshotLevel {
        DepthByOrderSnapshotLevel {
            identity: identity(),
            sequence_number,
            side: DepthByOrderSide::Bid,
            price: 5_100.0,
            orders: vec![
                crate::DepthByOrderSnapshotOrder {
                    size: 2,
                    priority: 11,
                    exchange_order_id: "bid-1".to_string(),
                },
                crate::DepthByOrderSnapshotOrder {
                    size: 3,
                    priority: 12,
                    exchange_order_id: "bid-2".to_string(),
                },
            ],
        }
    }

    fn bid_update(
        sequence_number: Option<u64>,
        kind: DepthByOrderMutationKind,
        exchange_order_id: &str,
    ) -> crate::DepthByOrderUpdate {
        crate::DepthByOrderUpdate {
            identity: identity(),
            sequence_number,
            mutations: vec![DepthByOrderMutation {
                kind,
                side: DepthByOrderSide::Bid,
                price: 5_100.0,
                previous_price: Some(5_100.0),
                size: 4,
                priority: 12,
                exchange_order_id: exchange_order_id.to_string(),
            }],
            timestamp: Some(timestamp()),
            source_timestamp: None,
            jop_timestamp: None,
        }
    }

    #[test]
    fn unsequenced_live_delete_applies_without_consuming_a_sequence() {
        let mut assembler = MboBookAssembler::new(identity());
        assembler
            .accept_snapshot_level(bid_level(Some(40)), 2, 2)
            .expect("sequenced snapshot level installs");
        assembler
            .complete_snapshot(Accepted, Some(40), 2, 2)
            .expect("sequenced snapshot completes");

        let (_, revision, changes) = published_update(
            assembler
                .accept_update(
                    bid_update(None, DepthByOrderMutationKind::Delete, "bid-2"),
                    2,
                    2,
                )
                .expect("unsequenced live delete applies in arrival order"),
        );
        assert_eq!(revision, 1);
        assert_eq!(changes, [bid_delete("bid-2")]);

        let (previous_revision, revision, changes) = published_update(
            assembler
                .accept_update(
                    bid_update(Some(41), DepthByOrderMutationKind::Change, "bid-1"),
                    2,
                    2,
                )
                .expect("next sequenced update follows the snapshot baseline"),
        );
        assert_eq!((previous_revision, revision), (1, 2));
        assert_eq!(
            changes,
            [OrderLevelChange::Change(resting(
                "bid-1",
                BookSide::Bid,
                510_000,
                400,
                12
            ))]
        );
    }

    #[test]
    fn inconsistent_snapshot_requests_a_bounded_resnapshot_instead_of_failing_the_session() {
        let mut assembler = MboBookAssembler::new(identity());
        let mut resnapshots = VecDeque::new();
        for attempt in 0..MAXIMUM_DEPTH_RESNAPSHOTS {
            assembler
                .accept_snapshot_level(bid_level(Some(40)), 2, 2)
                .expect("sequenced level contributes orders");
            let outcome = assembler.complete_snapshot(Accepted, Some(41), 2, 2);
            assert_eq!(
                outcome.as_ref().err(),
                Some(&OrderLevelFault::Inconsistent(
                    ProviderInvalidationReason::SequenceGap
                ))
            );
            assert_eq!(
                reset_of(order_level_outcome(
                    &mut assembler,
                    outcome,
                    "instrument",
                    &mut resnapshots
                )),
                (OrderLevelInterruption::SequenceGap, true)
            );
            assert!(assembler.awaiting_snapshot(), "attempt {attempt} retries");
            assert_eq!(resnapshots.pop_front().as_deref(), Some("instrument"));
        }

        assembler
            .accept_snapshot_level(bid_level(Some(40)), 2, 2)
            .expect("sequenced level contributes orders");
        let exhausted = assembler.complete_snapshot(Accepted, Some(41), 2, 2);
        assert_eq!(
            reset_of(order_level_outcome(
                &mut assembler,
                exhausted,
                "instrument",
                &mut resnapshots
            )),
            (OrderLevelInterruption::SequenceGap, false),
            "an exhausted budget turns order levels off"
        );
        assert!(
            resnapshots.is_empty(),
            "the budget is spent within the window"
        );
        assert!(!assembler.awaiting_snapshot());
        assert!(matches!(
            assembler
                .accept_update(
                    bid_update(Some(41), DepthByOrderMutationKind::New, "bid-3"),
                    2,
                    2
                )
                .expect("disabled book ignores order-level frames"),
            MboBookOutcome::IgnoredStale
        ));
    }

    #[test]
    fn unsequenced_update_buffered_before_the_baseline_is_not_replayed() {
        let mut assembler = MboBookAssembler::new(identity());
        assembler
            .accept_update(
                bid_update(None, DepthByOrderMutationKind::Delete, "bid-2"),
                2,
                2,
            )
            .expect("pre-snapshot update buffers");
        assembler
            .accept_snapshot_level(bid_level(Some(40)), 2, 2)
            .expect("snapshot level installs");
        assert_eq!(
            assembler.complete_snapshot(Accepted, Some(40), 2, 2).err(),
            Some(OrderLevelFault::Inconsistent(
                ProviderInvalidationReason::SequenceGap
            ))
        );
    }

    /// Rithmic Test answers a snapshot request with levels and a completion
    /// that carry no sequence, followed by sequenced live updates.
    #[test]
    fn unsequenced_snapshot_installs_and_checks_continuity_from_the_first_live_sequence() {
        let mut assembler = MboBookAssembler::new(identity());
        // Precedes the snapshot response, so the image already reflects it.
        assembler
            .accept_update(
                bid_update(Some(500), DepthByOrderMutationKind::New, "bid-2"),
                2,
                2,
            )
            .expect("pre-snapshot update buffers");
        assembler
            .accept_snapshot_level(bid_level(None), 2, 2)
            .expect("unsequenced level contributes orders");
        assembler
            .accept_update(
                bid_update(Some(700), DepthByOrderMutationKind::Change, "bid-1"),
                2,
                2,
            )
            .expect("interleaved update buffers");
        let outcome = assembler
            .complete_snapshot(Accepted, None, 2, 2)
            .expect("an unsequenced snapshot completes");
        let (image, provider_sequence, orders) = published_snapshot(outcome);
        assert!(assembler.ready());
        assert_eq!((image, provider_sequence), (1, Some(700)));
        assert_eq!(
            orders,
            [
                resting("bid-1", BookSide::Bid, 510_000, 400, 12),
                resting("bid-2", BookSide::Bid, 510_000, 300, 12),
            ]
        );

        assert!(matches!(
            assembler
                .finish_initial_image(&DecodedDepthByOrderEndEvent {
                    identities: vec![identity()],
                    sequence_number: Some(650),
                    timestamp: Some(timestamp()),
                })
                .expect("a marker at or below the live baseline is stale"),
            MboBookOutcome::IgnoredStale
        ));
        assert!(matches!(
            assembler
                .accept_update(
                    bid_update(Some(700), DepthByOrderMutationKind::Change, "bid-1"),
                    2,
                    2
                )
                .expect("a replayed sequence is stale"),
            MboBookOutcome::IgnoredStale
        ));
        let (_, _, changes) = published_update(
            assembler
                .accept_update(
                    bid_update(Some(701), DepthByOrderMutationKind::Delete, "bid-2"),
                    2,
                    2,
                )
                .expect("the next live sequence applies"),
        );
        assert_eq!(changes, [bid_delete("bid-2")]);

        assert!(matches!(
            assembler.accept_update(
                bid_update(Some(703), DepthByOrderMutationKind::Change, "bid-1"),
                2,
                2
            ),
            Err((ProviderInvalidationReason::SequenceGap, _))
        ));
    }

    #[test]
    fn sequenced_end_marker_before_any_live_sequence_is_not_a_gap() {
        let mut assembler = MboBookAssembler::new(identity());
        assembler
            .accept_snapshot_level(bid_level(None), 2, 2)
            .expect("unsequenced level contributes orders");
        assembler
            .complete_snapshot(Accepted, None, 2, 2)
            .expect("unsequenced snapshot completes");
        assert!(matches!(
            assembler
                .finish_initial_image(&DecodedDepthByOrderEndEvent {
                    identities: vec![identity()],
                    sequence_number: Some(9),
                    timestamp: Some(timestamp()),
                })
                .expect("no baseline means no gap evidence"),
            MboBookOutcome::IgnoredStale
        ));
        assert!(matches!(
            assembler
                .accept_update(
                    bid_update(Some(10), DepthByOrderMutationKind::Change, "bid-1"),
                    2,
                    2
                )
                .expect("the first live sequence starts continuity"),
            MboBookOutcome::Publish(OrderLevelPublication::Update { .. })
        ));
    }

    #[test]
    fn unsequenced_snapshot_contradicted_by_an_interleaved_update_resnapshots() {
        let mut assembler = MboBookAssembler::new(identity());
        let mut resnapshots = VecDeque::new();
        assembler
            .accept_snapshot_level(bid_level(None), 2, 2)
            .expect("unsequenced level contributes orders");
        assembler
            .accept_update(
                bid_update(Some(41), DepthByOrderMutationKind::Delete, "bid-9"),
                2,
                2,
            )
            .expect("interleaved update buffers");
        let outcome = assembler.complete_snapshot(Accepted, None, 2, 2);
        assert_eq!(
            outcome.as_ref().err(),
            Some(&OrderLevelFault::Inconsistent(
                ProviderInvalidationReason::SequenceGap
            ))
        );
        assert_eq!(
            reset_of(order_level_outcome(
                &mut assembler,
                outcome,
                "instrument",
                &mut resnapshots
            )),
            (OrderLevelInterruption::SequenceGap, true)
        );
        assert_eq!(resnapshots.pop_front().as_deref(), Some("instrument"));
        assert!(assembler.awaiting_snapshot());
    }

    fn ask_level(exchange_order_id: &str) -> DepthByOrderSnapshotLevel {
        DepthByOrderSnapshotLevel {
            identity: identity(),
            sequence_number: None,
            side: DepthByOrderSide::Ask,
            price: 5_100.25,
            orders: vec![crate::DepthByOrderSnapshotOrder {
                size: 1,
                priority: 30,
                exchange_order_id: exchange_order_id.to_string(),
            }],
        }
    }

    const REFUSED: DepthByOrderSnapshotRejection = DepthByOrderSnapshotRejection { code: 5 };

    #[test]
    fn refused_snapshot_retries_within_the_budget_then_disables_order_level_depth() {
        let mut assembler = MboBookAssembler::new(identity());
        let mut resnapshots = VecDeque::new();
        for attempt in 0..MAXIMUM_DEPTH_RESNAPSHOTS {
            let outcome = assembler.complete_snapshot(
                DepthByOrderSnapshotOutcome::Rejected(REFUSED),
                None,
                2,
                2,
            );
            assert_eq!(
                outcome.as_ref().err(),
                Some(&OrderLevelFault::SnapshotRejected(REFUSED))
            );
            assert_eq!(
                reset_of(order_level_outcome(
                    &mut assembler,
                    outcome,
                    "instrument",
                    &mut resnapshots
                )),
                (OrderLevelInterruption::SnapshotRefused, true)
            );
            assert!(assembler.awaiting_snapshot(), "attempt {attempt} retries");
            assert_eq!(resnapshots.pop_front().as_deref(), Some("instrument"));
        }
        let exhausted =
            assembler.complete_snapshot(DepthByOrderSnapshotOutcome::Rejected(REFUSED), None, 2, 2);
        assert_eq!(
            reset_of(order_level_outcome(
                &mut assembler,
                exhausted,
                "instrument",
                &mut resnapshots
            )),
            (OrderLevelInterruption::SnapshotRefused, false)
        );
        assert!(resnapshots.is_empty());
        assert!(!assembler.awaiting_snapshot());
        assert!(!assembler.owes_snapshot_response());
    }

    #[test]
    fn refused_snapshot_level_keeps_its_image_from_becoming_authoritative() {
        let mut assembler = MboBookAssembler::new(identity());
        let mut resnapshots = VecDeque::new();
        assembler
            .accept_snapshot_level(bid_level(None), 2, 2)
            .expect("first level contributes orders");
        assembler.reject_snapshot_level(DepthByOrderSnapshotRejection { code: 7 });
        assert!(matches!(
            assembler
                .accept_snapshot_level(ask_level("ask-1"), 2, 2)
                .expect("levels after a refusal drain"),
            MboBookOutcome::IgnoredStale
        ));
        let outcome = assembler.complete_snapshot(Accepted, None, 2, 2);
        assert_eq!(
            outcome.as_ref().err(),
            Some(&OrderLevelFault::SnapshotRejected(
                DepthByOrderSnapshotRejection { code: 7 }
            )),
            "an accepted terminal frame cannot complete a refused image"
        );
        assert_eq!(
            reset_of(order_level_outcome(
                &mut assembler,
                outcome,
                "instrument",
                &mut resnapshots
            )),
            (OrderLevelInterruption::SnapshotRefused, true)
        );
        assert!(!assembler.ready());
        assert_eq!(resnapshots.pop_front().as_deref(), Some("instrument"));
    }

    #[test]
    fn response_cut_short_by_a_recovery_drains_before_the_fresh_snapshot() {
        let mut assembler = MboBookAssembler::new(identity());
        let mut resnapshots = VecDeque::new();
        assembler
            .accept_snapshot_level(bid_level(None), 2, 2)
            .expect("first level contributes orders");
        // A duplicate order id inside the response discards the image mid-response.
        let duplicate = assembler.accept_snapshot_level(bid_level(None), 2, 2);
        assert_eq!(
            reset_of(order_level_outcome(
                &mut assembler,
                duplicate,
                "instrument",
                &mut resnapshots
            )),
            (OrderLevelInterruption::SequenceGap, true)
        );
        assert!(
            resnapshots.is_empty(),
            "the fresh request waits for the in-flight response"
        );
        assert!(matches!(
            assembler
                .accept_snapshot_level(ask_level("ask-old"), 2, 2)
                .expect("the rest of the abandoned response drains"),
            MboBookOutcome::IgnoredStale
        ));
        let drained = assembler.complete_snapshot(Accepted, None, 2, 2);
        assert!(
            order_level_outcome(&mut assembler, drained, "instrument", &mut resnapshots).is_none(),
            "the abandoned completion never publishes its partial image"
        );
        assert!(!assembler.ready());
        assert_eq!(resnapshots.pop_front().as_deref(), Some("instrument"));

        assembler
            .accept_snapshot_level(ask_level("ask-new"), 2, 2)
            .expect("the fresh response builds the image");
        let (image, _, orders) = published_snapshot(
            assembler
                .complete_snapshot(Accepted, None, 2, 2)
                .expect("the fresh response completes"),
        );
        assert_eq!(image, 1, "the abandoned response never published an image");
        assert_eq!(
            orders,
            [resting("ask-new", BookSide::Ask, 510_025, 100, 30)],
            "no order from the abandoned response survives"
        );
    }

    #[test]
    fn unowed_completion_is_ignored_after_the_image_is_ready() {
        let mut assembler = MboBookAssembler::new(identity());
        assembler
            .accept_snapshot_level(bid_level(None), 2, 2)
            .expect("level contributes orders");
        assembler
            .complete_snapshot(Accepted, None, 2, 2)
            .expect("snapshot completes");
        assert!(matches!(
            assembler
                .complete_snapshot(DepthByOrderSnapshotOutcome::Rejected(REFUSED), None, 2, 2)
                .expect("an unowed completion is stale"),
            MboBookOutcome::IgnoredStale
        ));
        assert!(assembler.ready());
    }

    #[test]
    fn exchange_clock_publishes_only_from_the_source_clock() {
        let gateway = ProviderTimestamp {
            seconds: 1_700_000_000,
            microseconds: 123_456,
        };
        let source = SourceTimestamp {
            seconds: 1_700_000_000,
            microseconds: Some(123_455),
            nanoseconds: Some(123_455_789),
        };
        let instrument = instrument();
        let generation = generation(1);
        let both = metadata(
            &instrument,
            generation,
            1,
            FrameClocks {
                gateway: Some(gateway),
                source: Some(source),
            },
            9,
        )
        .expect("metadata builds");
        assert_eq!(
            both.timestamps,
            QualifiedTimestamp {
                exchange_unix_nanos: Some(1_700_000_000_123_455_789),
                provider_unix_nanos: Some(1_700_000_000_123_456_000),
                received_unix_nanos: 9,
            }
        );
        let gateway_only = metadata(
            &instrument,
            generation,
            1,
            FrameClocks {
                gateway: Some(gateway),
                source: None,
            },
            9,
        )
        .expect("metadata builds");
        assert_eq!(gateway_only.timestamps.exchange_unix_nanos, None);
        assert_eq!(
            gateway_only.timestamps.provider_unix_nanos,
            Some(1_700_000_000_123_456_000)
        );
    }
}
