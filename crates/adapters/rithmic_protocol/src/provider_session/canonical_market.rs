//! Canonical market.

use aeris_observability::diagnostic;
use std::time::{Duration, Instant};

use super::{
    AggressorSide, BTreeMap, BookSide, DecodedDepthByOrderEndEvent, DecodedMarketMessage,
    DepthByOrderMutation, DepthByOrderMutationKind, DepthByOrderSide, DepthByOrderSnapshotLevel,
    DepthByOrderSnapshotMessage, DepthLevel, DepthSnapshot, EventMetadata, MAXIMUM_INSTRUMENTS,
    MarketEvent, MarketIdentity, MarketTrade, PROVIDER_ID, ProviderInvalidationReason,
    ProviderTimestamp, QualifiedTimestamp, QuoteLevel, QuoteSideUpdate, RetryDisposition,
    RithmicProviderConfig, RithmicProviderInstrument, SessionGeneration, SystemTime,
    TopOfBookQuote, TradeAggressor, UNIX_EPOCH, VecDeque,
};

pub(super) struct CanonicalSessionState {
    instruments: Vec<RithmicProviderInstrument>,
    generation: SessionGeneration,
    quotes: BTreeMap<String, QuoteState>,
    mbo_books: BTreeMap<String, MboBookAssembler>,
    depth_resnapshots: VecDeque<String>,
}

#[derive(Clone, Copy, Default)]
struct QuoteState {
    bid: Option<QuoteLevel>,
    ask: Option<QuoteLevel>,
}

pub(super) const MAXIMUM_MBO_ORDERS: usize = 131_072;
const MAXIMUM_CANONICAL_DEPTH_LEVELS: usize = 4_096;
const MAXIMUM_PENDING_MBO_MUTATIONS: usize = 131_072;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MboOrder {
    side: BookSide,
    price: i64,
    quantity: i64,
    priority: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MboAggregateLevel {
    quantity: i64,
    order_count: u32,
}

struct MboBookAssembler {
    identity: MarketIdentity,
    orders: BTreeMap<String, MboOrder>,
    bids: BTreeMap<i64, MboAggregateLevel>,
    asks: BTreeMap<i64, MboAggregateLevel>,
    last_provider_sequence: Option<u64>,
    snapshot_orders: BTreeMap<String, MboOrder>,
    snapshot_bids: BTreeMap<i64, MboAggregateLevel>,
    snapshot_asks: BTreeMap<i64, MboAggregateLevel>,
    snapshot_sequence: Option<u64>,
    pending_updates: VecDeque<crate::DepthByOrderUpdate>,
    pending_mutations: usize,
    ready: bool,
    /// Set once snapshot recovery is exhausted; order-level depth stays off
    /// for the rest of this session and a new session retries it.
    disabled: bool,
    resnapshot_times: VecDeque<Instant>,
}

const MAXIMUM_DEPTH_RESNAPSHOTS: usize = 3;
const DEPTH_RESNAPSHOT_WINDOW: Duration = Duration::from_secs(60);

enum MboBookOutcome {
    Pending,
    IgnoredStale,
    Snapshot {
        timestamp: Option<ProviderTimestamp>,
        bids: Vec<DepthLevel>,
        asks: Vec<DepthLevel>,
    },
}

impl MboBookAssembler {
    fn new(identity: MarketIdentity) -> Self {
        Self {
            identity,
            orders: BTreeMap::new(),
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_provider_sequence: None,
            snapshot_orders: BTreeMap::new(),
            snapshot_bids: BTreeMap::new(),
            snapshot_asks: BTreeMap::new(),
            snapshot_sequence: None,
            pending_updates: VecDeque::new(),
            pending_mutations: 0,
            ready: false,
            disabled: false,
            resnapshot_times: VecDeque::new(),
        }
    }

    #[cfg(test)]
    const fn ready(&self) -> bool {
        self.ready && !self.disabled
    }

    const fn awaiting_snapshot(&self) -> bool {
        !self.ready && !self.disabled
    }

    /// Discards the order-level image after an inconsistency. Returns true when
    /// a fresh snapshot should be requested, false once the recovery budget for
    /// the rolling window is spent and order-level depth is disabled instead.
    fn recover(&mut self, now: Instant) -> bool {
        let mut resnapshot_times = std::mem::take(&mut self.resnapshot_times);
        while resnapshot_times
            .front()
            .is_some_and(|time| now.saturating_duration_since(*time) >= DEPTH_RESNAPSHOT_WINDOW)
        {
            resnapshot_times.pop_front();
        }
        *self = Self::new(self.identity.clone());
        if resnapshot_times.len() >= MAXIMUM_DEPTH_RESNAPSHOTS {
            self.disabled = true;
            return false;
        }
        resnapshot_times.push_back(now);
        self.resnapshot_times = resnapshot_times;
        true
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
        // An unsequenced frame applies in arrival order without consuming a
        // sequence. If it did consume one, the next sequenced frame is a gap.
        if let Some(sequence) = update.sequence_number {
            if self
                .last_provider_sequence
                .is_some_and(|last| sequence <= last)
            {
                return Ok(MboBookOutcome::IgnoredStale);
            }
            if self
                .last_provider_sequence
                .is_some_and(|last| sequence != last.saturating_add(1))
            {
                return Err((
                    ProviderInvalidationReason::SequenceGap,
                    RetryDisposition::Transient,
                ));
            }
        }
        for mutation in update.mutations {
            self.apply_mutation(mutation, price_scale, quantity_scale)?;
        }
        if let Some(sequence) = update.sequence_number {
            self.last_provider_sequence = Some(sequence);
        }
        if self.ready {
            Ok(self.snapshot(update.timestamp))
        } else {
            Ok(MboBookOutcome::Pending)
        }
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
        if self.disabled {
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
        let side = match level.side {
            DepthByOrderSide::Bid => BookSide::Bid,
            DepthByOrderSide::Ask => BookSide::Ask,
        };
        let price = fixed_price(level.price, price_scale)?;
        for snapshot_order in level.orders {
            if self
                .snapshot_orders
                .contains_key(&snapshot_order.exchange_order_id)
                || self.snapshot_orders.len() >= MAXIMUM_MBO_ORDERS
            {
                return Err((
                    ProviderInvalidationReason::QueueOverflow,
                    RetryDisposition::Transient,
                ));
            }
            let exchange_order_id = snapshot_order.exchange_order_id;
            let order = MboOrder {
                side,
                price,
                quantity: fixed_quantity(snapshot_order.size, quantity_scale)?,
                priority: snapshot_order.priority,
            };
            add_mbo_aggregate(&mut self.snapshot_bids, &mut self.snapshot_asks, order)?;
            self.snapshot_orders.insert(exchange_order_id, order);
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
        // Without any sequenced baseline, buffered updates cannot be ordered
        // against the image, so the order-level book cannot be trusted.
        let baseline = match (self.snapshot_sequence, completion_sequence) {
            (Some(snapshot), Some(completion)) if snapshot != completion => {
                return Err((
                    ProviderInvalidationReason::SequenceGap,
                    RetryDisposition::Transient,
                ));
            }
            (Some(snapshot), _) => snapshot,
            (None, Some(completion)) => completion,
            (None, None) => {
                return Err((
                    ProviderInvalidationReason::SequenceGap,
                    RetryDisposition::Transient,
                ));
            }
        };
        self.orders = std::mem::take(&mut self.snapshot_orders);
        self.bids = std::mem::take(&mut self.snapshot_bids);
        self.asks = std::mem::take(&mut self.snapshot_asks);
        self.last_provider_sequence = Some(baseline);
        self.snapshot_sequence = None;
        self.ready = true;
        let mut timestamp = None;
        // An unsequenced buffered update is placeable only after a sequenced
        // update beyond the baseline; before that it may predate the image.
        let mut past_baseline = false;
        while let Some(update) = self.pending_updates.pop_front() {
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
            for mutation in update.mutations {
                self.apply_mutation(mutation, price_scale, quantity_scale)?;
            }
            timestamp = update.timestamp.or(timestamp);
        }
        self.pending_mutations = 0;
        Ok(self.snapshot(timestamp))
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
            return if event.sequence_number.is_none_or(|sequence| {
                self.last_provider_sequence
                    .is_some_and(|last| sequence <= last)
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
    ) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
        let side = match mutation.side {
            DepthByOrderSide::Bid => BookSide::Bid,
            DepthByOrderSide::Ask => BookSide::Ask,
        };
        let price = fixed_price(mutation.price, price_scale)?;
        match mutation.kind {
            DepthByOrderMutationKind::New => {
                if self.orders.contains_key(&mutation.exchange_order_id)
                    || self.orders.len() >= MAXIMUM_MBO_ORDERS
                {
                    return Err((
                        ProviderInvalidationReason::QueueOverflow,
                        RetryDisposition::Transient,
                    ));
                }
                let quantity = fixed_quantity(mutation.size, quantity_scale)?;
                let order = MboOrder {
                    side,
                    price,
                    quantity,
                    priority: mutation.priority,
                };
                self.add_aggregate(order)?;
                self.orders.insert(mutation.exchange_order_id, order);
            }
            DepthByOrderMutationKind::Change => {
                let Some(previous) = self.orders.remove(&mutation.exchange_order_id) else {
                    return Err((
                        ProviderInvalidationReason::SequenceGap,
                        RetryDisposition::Transient,
                    ));
                };
                if let Some(previous_price) = mutation.previous_price {
                    let expected = fixed_price(previous_price, price_scale)?;
                    if expected != previous.price {
                        return Err((
                            ProviderInvalidationReason::SequenceGap,
                            RetryDisposition::Transient,
                        ));
                    }
                }
                self.remove_aggregate(previous)?;
                let quantity = fixed_quantity(mutation.size, quantity_scale)?;
                let updated = MboOrder {
                    side,
                    price,
                    quantity,
                    priority: mutation.priority,
                };
                self.add_aggregate(updated)?;
                self.orders.insert(mutation.exchange_order_id, updated);
            }
            DepthByOrderMutationKind::Delete => {
                let Some(previous) = self.orders.remove(&mutation.exchange_order_id) else {
                    return Err((
                        ProviderInvalidationReason::SequenceGap,
                        RetryDisposition::Transient,
                    ));
                };
                if let Some(previous_price) = mutation.previous_price {
                    let expected = fixed_price(previous_price, price_scale)?;
                    if expected != previous.price {
                        return Err((
                            ProviderInvalidationReason::SequenceGap,
                            RetryDisposition::Transient,
                        ));
                    }
                }
                self.remove_aggregate(previous)?;
            }
        }
        Ok(())
    }

    fn add_aggregate(
        &mut self,
        order: MboOrder,
    ) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
        add_mbo_aggregate(&mut self.bids, &mut self.asks, order)
    }

    fn remove_aggregate(
        &mut self,
        order: MboOrder,
    ) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
        remove_mbo_aggregate(&mut self.bids, &mut self.asks, order)
    }

    fn snapshot(&self, timestamp: Option<ProviderTimestamp>) -> MboBookOutcome {
        let bids = self
            .bids
            .iter()
            .rev()
            .take(MAXIMUM_CANONICAL_DEPTH_LEVELS)
            .map(|(price, level)| DepthLevel {
                price: *price,
                quantity: level.quantity,
                order_count: Some(level.order_count),
            })
            .collect();
        let asks = self
            .asks
            .iter()
            .take(MAXIMUM_CANONICAL_DEPTH_LEVELS)
            .map(|(price, level)| DepthLevel {
                price: *price,
                quantity: level.quantity,
                order_count: Some(level.order_count),
            })
            .collect();
        MboBookOutcome::Snapshot {
            timestamp,
            bids,
            asks,
        }
    }
}

fn add_mbo_aggregate(
    bids: &mut BTreeMap<i64, MboAggregateLevel>,
    asks: &mut BTreeMap<i64, MboAggregateLevel>,
    order: MboOrder,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    let levels = match order.side {
        BookSide::Bid => bids,
        BookSide::Ask => asks,
    };
    let level = levels.entry(order.price).or_insert(MboAggregateLevel {
        quantity: 0,
        order_count: 0,
    });
    level.quantity = level
        .quantity
        .checked_add(order.quantity)
        .ok_or_else(malformed)?;
    level.order_count = level.order_count.checked_add(1).ok_or_else(malformed)?;
    Ok(())
}

fn remove_mbo_aggregate(
    bids: &mut BTreeMap<i64, MboAggregateLevel>,
    asks: &mut BTreeMap<i64, MboAggregateLevel>,
    order: MboOrder,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    let levels = match order.side {
        BookSide::Bid => bids,
        BookSide::Ask => asks,
    };
    let Some(level) = levels.get_mut(&order.price) else {
        return Err(malformed());
    };
    level.quantity = level
        .quantity
        .checked_sub(order.quantity)
        .ok_or_else(malformed)?;
    level.order_count = level.order_count.checked_sub(1).ok_or_else(malformed)?;
    if level.quantity == 0 && level.order_count == 0 {
        levels.remove(&order.price);
    } else if level.quantity <= 0 || level.order_count == 0 {
        return Err(malformed());
    }
    Ok(())
}

/// Order-level inconsistencies recover the book with a fresh snapshot on the
/// live connection instead of invalidating the session and its chart streams.
fn order_level_outcome(
    assembler: &mut MboBookAssembler,
    outcome: Result<MboBookOutcome, (ProviderInvalidationReason, RetryDisposition)>,
    instrument_id: &str,
    depth_resnapshots: &mut VecDeque<String>,
) -> Option<MboBookOutcome> {
    match outcome {
        Ok(outcome) => Some(outcome),
        Err((reason, _)) => {
            if assembler.recover(Instant::now()) {
                diagnostic!("Aeris Rithmic order-level depth resnapshot requested: {reason:?}");
                if !depth_resnapshots
                    .iter()
                    .any(|pending| pending == instrument_id)
                {
                    depth_resnapshots.push_back(instrument_id.to_string());
                }
            } else {
                diagnostic!(
                    "Aeris Rithmic order-level depth disabled for this session after repeated {reason:?}"
                );
            }
            None
        }
    }
}

impl CanonicalSessionState {
    pub(super) fn instruments(&self) -> &[RithmicProviderInstrument] {
        &self.instruments
    }

    pub(super) fn new(config: &RithmicProviderConfig, generation: SessionGeneration) -> Self {
        let mut mbo_books = BTreeMap::new();
        for instrument in &config.instruments {
            if instrument.order_book {
                mbo_books.insert(
                    instrument.descriptor.instrument_id.clone(),
                    MboBookAssembler::new(MarketIdentity {
                        symbol: instrument.descriptor.provider_symbol.clone(),
                        exchange: instrument.descriptor.venue_id.clone(),
                    }),
                );
            }
        }
        Self {
            instruments: config.instruments.clone(),
            generation,
            quotes: BTreeMap::new(),
            mbo_books,
            depth_resnapshots: VecDeque::new(),
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn convert(
        &mut self,
        message: DecodedMarketMessage,
        source_ordinal: u64,
        received_unix_nanos: i64,
    ) -> Result<Option<MarketEvent>, (ProviderInvalidationReason, RetryDisposition)> {
        match message {
            DecodedMarketMessage::Trade(trade) => {
                let instrument = self.instrument(&trade.identity)?;
                let event = MarketEvent::Trade(MarketTrade {
                    metadata: metadata(
                        instrument,
                        self.generation,
                        source_ordinal,
                        Some(trade.timestamp),
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
                validate_market(event).map(Some)
            }
            DecodedMarketMessage::Quote(quote) => {
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
                let event = MarketEvent::Quote(TopOfBookQuote {
                    metadata: metadata(
                        &instrument,
                        self.generation,
                        source_ordinal,
                        Some(quote.timestamp),
                        received_unix_nanos,
                    )?,
                    bid: state
                        .bid
                        .map(|level| canonical_quote_level(level, &instrument))
                        .transpose()?,
                    ask: state
                        .ask
                        .map(|level| canonical_quote_level(level, &instrument))
                        .transpose()?,
                });
                validate_market(event).map(Some)
            }
            // Depth comes from the order-level book. Price-level frames carry
            // incremental changes (including zero-size removals) that the image
            // assembler would misread as whole books, so they are not a depth source.
            DecodedMarketMessage::OrderBook(update) => {
                self.instrument(&update.identity)?;
                Ok(None)
            }
            DecodedMarketMessage::DepthByOrderSnapshot(DepthByOrderSnapshotMessage::Level(
                level,
            )) => {
                let instrument = self.instrument(&level.identity)?.clone();
                let assembler = self
                    .mbo_books
                    .get_mut(&instrument.descriptor.instrument_id)
                    .ok_or_else(malformed)?;
                let outcome = assembler.accept_snapshot_level(
                    level,
                    instrument.descriptor.price_scale,
                    instrument.descriptor.quantity_scale,
                );
                match order_level_outcome(
                    assembler,
                    outcome,
                    &instrument.descriptor.instrument_id,
                    &mut self.depth_resnapshots,
                ) {
                    None | Some(MboBookOutcome::Pending | MboBookOutcome::IgnoredStale) => Ok(None),
                    Some(MboBookOutcome::Snapshot { .. }) => Err(malformed()),
                }
            }
            DecodedMarketMessage::DepthByOrderSnapshot(DepthByOrderSnapshotMessage::Complete {
                accepted,
                identity,
                sequence_number,
            }) => {
                let instrument = if let Some(identity) = identity.as_ref() {
                    self.instrument(identity)?.clone()
                } else {
                    // An anonymous completion belongs to the one book awaiting a
                    // snapshot; if that is ambiguous, every waiting book re-snapshots.
                    let awaiting = self
                        .instruments
                        .iter()
                        .filter(|instrument| {
                            instrument.order_book
                                && self
                                    .mbo_books
                                    .get(&instrument.descriptor.instrument_id)
                                    .is_some_and(MboBookAssembler::awaiting_snapshot)
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    match awaiting.as_slice() {
                        [] => return Ok(None),
                        [instrument] => instrument.clone(),
                        _ => {
                            for instrument in &awaiting {
                                if let Some(assembler) =
                                    self.mbo_books.get_mut(&instrument.descriptor.instrument_id)
                                {
                                    let _ = order_level_outcome(
                                        assembler,
                                        Err((
                                            ProviderInvalidationReason::SequenceGap,
                                            RetryDisposition::Transient,
                                        )),
                                        &instrument.descriptor.instrument_id,
                                        &mut self.depth_resnapshots,
                                    );
                                }
                            }
                            return Ok(None);
                        }
                    }
                };
                let assembler = self
                    .mbo_books
                    .get_mut(&instrument.descriptor.instrument_id)
                    .ok_or_else(malformed)?;
                // A rejected order-level snapshot leaves depth to the aggregate book.
                let outcome = if accepted {
                    assembler.finish_snapshot(
                        sequence_number,
                        instrument.descriptor.price_scale,
                        instrument.descriptor.quantity_scale,
                    )
                } else {
                    Err((
                        ProviderInvalidationReason::Transport,
                        RetryDisposition::Transient,
                    ))
                };
                match order_level_outcome(
                    assembler,
                    outcome,
                    &instrument.descriptor.instrument_id,
                    &mut self.depth_resnapshots,
                ) {
                    None | Some(MboBookOutcome::Pending | MboBookOutcome::IgnoredStale) => Ok(None),
                    Some(MboBookOutcome::Snapshot {
                        timestamp,
                        bids,
                        asks,
                    }) => {
                        let event = MarketEvent::DepthSnapshot(DepthSnapshot {
                            metadata: metadata(
                                &instrument,
                                self.generation,
                                source_ordinal,
                                timestamp,
                                received_unix_nanos,
                            )?,
                            bids,
                            asks,
                        });
                        validate_market(event).map(Some)
                    }
                }
            }
            DecodedMarketMessage::DepthByOrder(update) => {
                let instrument = self.instrument(&update.identity)?.clone();
                let assembler = self
                    .mbo_books
                    .get_mut(&instrument.descriptor.instrument_id)
                    .ok_or_else(malformed)?;
                let outcome = assembler.accept_update(
                    update,
                    instrument.descriptor.price_scale,
                    instrument.descriptor.quantity_scale,
                );
                match order_level_outcome(
                    assembler,
                    outcome,
                    &instrument.descriptor.instrument_id,
                    &mut self.depth_resnapshots,
                ) {
                    None | Some(MboBookOutcome::Pending | MboBookOutcome::IgnoredStale) => Ok(None),
                    Some(MboBookOutcome::Snapshot {
                        timestamp,
                        bids,
                        asks,
                    }) => {
                        let event = MarketEvent::DepthSnapshot(DepthSnapshot {
                            metadata: metadata(
                                &instrument,
                                self.generation,
                                source_ordinal,
                                timestamp,
                                received_unix_nanos,
                            )?,
                            bids,
                            asks,
                        });
                        validate_market(event).map(Some)
                    }
                }
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
                if matching.len() != 1 {
                    return Err(malformed());
                }
                let instrument = &matching[0];
                let assembler = self
                    .mbo_books
                    .get_mut(&instrument.descriptor.instrument_id)
                    .ok_or_else(malformed)?;
                let outcome = assembler.finish_initial_image(&event);
                match order_level_outcome(
                    assembler,
                    outcome,
                    &instrument.descriptor.instrument_id,
                    &mut self.depth_resnapshots,
                ) {
                    None | Some(MboBookOutcome::Pending | MboBookOutcome::IgnoredStale) => Ok(None),
                    Some(MboBookOutcome::Snapshot {
                        timestamp,
                        bids,
                        asks,
                    }) => {
                        let event = MarketEvent::DepthSnapshot(DepthSnapshot {
                            metadata: metadata(
                                instrument,
                                self.generation,
                                source_ordinal,
                                timestamp,
                                received_unix_nanos,
                            )?,
                            bids,
                            asks,
                        });
                        validate_market(event).map(Some)
                    }
                }
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
        if instrument.order_book {
            self.mbo_books.insert(
                instrument.descriptor.instrument_id.clone(),
                MboBookAssembler::new(MarketIdentity {
                    symbol: instrument.descriptor.provider_symbol.clone(),
                    exchange: instrument.descriptor.venue_id.clone(),
                }),
            );
        }
        self.instruments.push(instrument);
        Ok(())
    }

    pub(super) fn remove_instrument(&mut self, instrument_id: &str) {
        self.instruments
            .retain(|instrument| instrument.descriptor.instrument_id != instrument_id);
        self.quotes.remove(instrument_id);
        self.mbo_books.remove(instrument_id);
        self.depth_resnapshots
            .retain(|pending| pending != instrument_id);
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

fn apply_quote_side(state: &mut Option<QuoteLevel>, update: QuoteSideUpdate) {
    match update {
        QuoteSideUpdate::Unchanged => {}
        QuoteSideUpdate::Cleared => *state = None,
        QuoteSideUpdate::Value(level) => *state = Some(level),
    }
}

fn metadata(
    instrument: &RithmicProviderInstrument,
    generation: SessionGeneration,
    source_sequence: u64,
    timestamp: Option<ProviderTimestamp>,
    received_unix_nanos: i64,
) -> Result<EventMetadata, (ProviderInvalidationReason, RetryDisposition)> {
    Ok(EventMetadata {
        provider_id: PROVIDER_ID.to_string(),
        instrument_id: instrument.descriptor.instrument_id.clone(),
        entitlement_id: instrument.entitlement_id.clone(),
        source_sequence,
        session_generation: generation.get(),
        timestamps: QualifiedTimestamp {
            exchange_unix_nanos: timestamp.map(provider_timestamp_nanos).transpose()?,
            provider_unix_nanos: None,
            received_unix_nanos,
        },
    })
}

fn canonical_quote_level(
    level: QuoteLevel,
    instrument: &RithmicProviderInstrument,
) -> Result<DepthLevel, (ProviderInvalidationReason, RetryDisposition)> {
    Ok(DepthLevel {
        price: fixed_price(level.price, instrument.descriptor.price_scale)?,
        quantity: fixed_quantity(level.size, instrument.descriptor.quantity_scale)?,
        order_count: level.orders,
    })
}

fn fixed_price(
    value: f64,
    scale: u8,
) -> Result<i64, (ProviderInvalidationReason, RetryDisposition)> {
    if !value.is_finite() || value <= 0.0 || scale > 18 {
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

pub(super) fn validate_market(
    event: MarketEvent,
) -> Result<MarketEvent, (ProviderInvalidationReason, RetryDisposition)> {
    event.validate(4_096).map_err(|_| malformed())?;
    Ok(event)
}

pub(super) const fn malformed() -> (ProviderInvalidationReason, RetryDisposition) {
    (
        ProviderInvalidationReason::MalformedMessage,
        RetryDisposition::Terminal,
    )
}

#[cfg(test)]
mod tests {
    use super::super::tests::{identity, timestamp};
    use super::*;
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

        let MboBookOutcome::Snapshot { bids, asks, .. } = assembler
            .finish_snapshot(Some(40), 2, 2)
            .expect("covering snapshot completes and live delta replays")
        else {
            panic!("covering snapshot must publish");
        };
        assert!(assembler.ready());
        assert_eq!(bids.len(), 1);
        assert_eq!(bids[0].price, 510_000);
        assert_eq!(bids[0].quantity, 800);
        assert_eq!(bids[0].order_count, Some(2));
        assert_eq!(asks.len(), 1);
        assert_eq!(asks[0].price, 510_025);
        assert_eq!(asks[0].quantity, 400);
        assert_eq!(asks[0].order_count, Some(1));

        let delete = crate::DepthByOrderUpdate {
            identity: identity(),
            sequence_number: Some(42),
            mutations: vec![DepthByOrderMutation {
                kind: DepthByOrderMutationKind::Delete,
                side: DepthByOrderSide::Bid,
                price: 5_100.0,
                previous_price: Some(5_100.0),
                size: 0,
                priority: 12,
                exchange_order_id: "bid-2".to_string(),
            }],
            timestamp: Some(timestamp()),
        };
        let MboBookOutcome::Snapshot { bids, .. } = assembler
            .accept_update(delete, 2, 2)
            .expect("next live update applies")
        else {
            panic!("ready MBO update must publish");
        };
        assert_eq!(bids[0].quantity, 500);
        assert_eq!(bids[0].order_count, Some(1));
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
        }
    }

    #[test]
    fn unsequenced_live_delete_applies_without_consuming_a_sequence() {
        let mut assembler = MboBookAssembler::new(identity());
        assembler
            .accept_snapshot_level(bid_level(Some(40)), 2, 2)
            .expect("sequenced snapshot level installs");
        assembler
            .finish_snapshot(Some(40), 2, 2)
            .expect("sequenced snapshot completes");

        let MboBookOutcome::Snapshot { bids, .. } = assembler
            .accept_update(
                bid_update(None, DepthByOrderMutationKind::Delete, "bid-2"),
                2,
                2,
            )
            .expect("unsequenced live delete applies in arrival order")
        else {
            panic!("ready MBO update must publish");
        };
        assert_eq!(bids[0].order_count, Some(1));

        assert!(matches!(
            assembler
                .accept_update(
                    bid_update(Some(41), DepthByOrderMutationKind::Change, "bid-1"),
                    2,
                    2
                )
                .expect("next sequenced update follows the snapshot baseline"),
            MboBookOutcome::Snapshot { .. }
        ));
    }

    #[test]
    fn unsequenced_snapshot_requests_a_bounded_resnapshot_instead_of_failing_the_session() {
        let mut assembler = MboBookAssembler::new(identity());
        let mut resnapshots = VecDeque::new();
        for attempt in 0..MAXIMUM_DEPTH_RESNAPSHOTS {
            assembler
                .accept_snapshot_level(bid_level(None), 2, 2)
                .expect("unsequenced level contributes orders");
            let outcome = assembler.finish_snapshot(None, 2, 2);
            assert!(matches!(
                outcome,
                Err((ProviderInvalidationReason::SequenceGap, _))
            ));
            assert!(
                order_level_outcome(&mut assembler, outcome, "instrument", &mut resnapshots)
                    .is_none()
            );
            assert!(assembler.awaiting_snapshot(), "attempt {attempt} retries");
            assert_eq!(resnapshots.pop_front().as_deref(), Some("instrument"));
        }

        let exhausted = assembler.finish_snapshot(None, 2, 2);
        assert!(
            order_level_outcome(&mut assembler, exhausted, "instrument", &mut resnapshots)
                .is_none()
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
        assert!(matches!(
            assembler.finish_snapshot(Some(40), 2, 2),
            Err((ProviderInvalidationReason::SequenceGap, _))
        ));
    }
}
