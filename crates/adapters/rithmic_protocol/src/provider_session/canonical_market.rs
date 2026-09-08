//! Canonical market.

use super::{
    AggregateBookAssembler, AggregateBookLimits, AggregateBookOutcome, AggressorSide, BTreeMap,
    BookSide, DecodedDepthByOrderEndEvent, DecodedMarketMessage, DepthByOrderMutation,
    DepthByOrderMutationKind, DepthByOrderSide, DepthByOrderSnapshotLevel,
    DepthByOrderSnapshotMessage, DepthLevel, DepthSnapshot, EventMetadata, MAXIMUM_INSTRUMENTS,
    MarketEvent, MarketIdentity, MarketTrade, NonZeroUsize, OrderBookLevel, PROVIDER_ID,
    ProviderInvalidationReason, ProviderTimestamp, QualifiedTimestamp, QuoteLevel, QuoteSideUpdate,
    RetryDisposition, RithmicProviderConfig, RithmicProviderInstrument, SessionGeneration,
    SystemTime, TopOfBookQuote, TradeAggressor, UNIX_EPOCH, VecDeque,
};

pub(super) struct CanonicalSessionState {
    instruments: Vec<RithmicProviderInstrument>,
    generation: SessionGeneration,
    quotes: BTreeMap<String, QuoteState>,
    books: BTreeMap<String, AggregateBookAssembler>,
    mbo_books: BTreeMap<String, MboBookAssembler>,
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
}

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
        }
    }

    const fn ready(&self) -> bool {
        self.ready
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
        if !self.ready {
            if self
                .snapshot_sequence
                .is_some_and(|snapshot| update.sequence_number <= snapshot)
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
        if self
            .last_provider_sequence
            .is_some_and(|last| update.sequence_number <= last)
        {
            return Ok(MboBookOutcome::IgnoredStale);
        }
        if self.ready
            && self
                .last_provider_sequence
                .is_some_and(|last| update.sequence_number != last.saturating_add(1))
        {
            return Err((
                ProviderInvalidationReason::SequenceGap,
                RetryDisposition::Transient,
            ));
        }
        for mutation in update.mutations {
            self.apply_mutation(mutation, price_scale, quantity_scale)?;
        }
        self.last_provider_sequence = Some(update.sequence_number);
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
        if self.ready {
            return if self
                .last_provider_sequence
                .is_some_and(|last| level.sequence_number <= last)
            {
                Ok(MboBookOutcome::IgnoredStale)
            } else {
                Err((
                    ProviderInvalidationReason::SequenceGap,
                    RetryDisposition::Transient,
                ))
            };
        }
        if self
            .snapshot_sequence
            .is_some_and(|sequence| sequence != level.sequence_number)
        {
            return Err((
                ProviderInvalidationReason::SequenceGap,
                RetryDisposition::Transient,
            ));
        }
        self.snapshot_sequence = Some(level.sequence_number);
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
        if self.ready {
            return Ok(MboBookOutcome::IgnoredStale);
        }
        let baseline = match (self.snapshot_sequence, completion_sequence) {
            (Some(snapshot), Some(completion)) if snapshot != completion => {
                return Err((
                    ProviderInvalidationReason::SequenceGap,
                    RetryDisposition::Transient,
                ));
            }
            (Some(snapshot), _) => snapshot,
            (None, Some(completion)) => completion,
            (None, None) => return Err(malformed()),
        };
        self.orders = std::mem::take(&mut self.snapshot_orders);
        self.bids = std::mem::take(&mut self.snapshot_bids);
        self.asks = std::mem::take(&mut self.snapshot_asks);
        self.last_provider_sequence = Some(baseline);
        self.snapshot_sequence = None;
        self.ready = true;
        let mut timestamp = None;
        while let Some(update) = self.pending_updates.pop_front() {
            if update.identity != self.identity {
                return Err(malformed());
            }
            if update.sequence_number <= self.last_provider_sequence.unwrap_or(0) {
                continue;
            }
            let expected = self.last_provider_sequence.unwrap_or(0).saturating_add(1);
            if update.sequence_number != expected {
                return Err((
                    ProviderInvalidationReason::SequenceGap,
                    RetryDisposition::Transient,
                ));
            }
            for mutation in update.mutations {
                self.apply_mutation(mutation, price_scale, quantity_scale)?;
            }
            self.last_provider_sequence = Some(update.sequence_number);
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
        if self.ready {
            return if self
                .last_provider_sequence
                .is_some_and(|last| event.sequence_number <= last)
            {
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

impl CanonicalSessionState {
    pub(super) fn instruments(&self) -> &[RithmicProviderInstrument] {
        &self.instruments
    }

    pub(super) fn try_new(
        config: &RithmicProviderConfig,
        generation: SessionGeneration,
    ) -> Result<Self, (ProviderInvalidationReason, RetryDisposition)> {
        let mut books = BTreeMap::new();
        let mut mbo_books = BTreeMap::new();
        for instrument in &config.instruments {
            if instrument.order_book {
                let limits = AggregateBookLimits::try_new(
                    NonZeroUsize::new(4_096).unwrap_or(NonZeroUsize::MIN),
                    NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN),
                )
                .map_err(|_| malformed())?;
                books.insert(
                    instrument.descriptor.instrument_id.clone(),
                    AggregateBookAssembler::new(
                        MarketIdentity {
                            symbol: instrument.descriptor.provider_symbol.clone(),
                            exchange: instrument.descriptor.venue_id.clone(),
                        },
                        limits,
                    ),
                );
                mbo_books.insert(
                    instrument.descriptor.instrument_id.clone(),
                    MboBookAssembler::new(MarketIdentity {
                        symbol: instrument.descriptor.provider_symbol.clone(),
                        exchange: instrument.descriptor.venue_id.clone(),
                    }),
                );
            }
        }
        Ok(Self {
            instruments: config.instruments.clone(),
            generation,
            quotes: BTreeMap::new(),
            books,
            mbo_books,
        })
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
            DecodedMarketMessage::OrderBook(update) => {
                let instrument = self.instrument(&update.identity)?.clone();
                if self
                    .mbo_books
                    .get(&instrument.descriptor.instrument_id)
                    .is_some_and(MboBookAssembler::ready)
                {
                    return Ok(None);
                }
                let assembler = self
                    .books
                    .get_mut(&instrument.descriptor.instrument_id)
                    .ok_or_else(malformed)?;
                match assembler.accept(update, source_ordinal) {
                    AggregateBookOutcome::Pending => Ok(None),
                    AggregateBookOutcome::Snapshot(image) => {
                        let bids = canonical_levels(
                            &image.bids,
                            instrument.descriptor.price_scale,
                            instrument.descriptor.quantity_scale,
                        )?;
                        let asks = canonical_levels(
                            &image.asks,
                            instrument.descriptor.price_scale,
                            instrument.descriptor.quantity_scale,
                        )?;
                        let event = MarketEvent::DepthSnapshot(DepthSnapshot {
                            metadata: metadata(
                                &instrument,
                                self.generation,
                                image.source_ordinal,
                                image.timestamp,
                                received_unix_nanos,
                            )?,
                            bids,
                            asks,
                        });
                        validate_market(event).map(Some)
                    }
                    AggregateBookOutcome::RecoveryRequired { .. }
                    | AggregateBookOutcome::Unavailable { .. } => Err((
                        ProviderInvalidationReason::SequenceGap,
                        RetryDisposition::Transient,
                    )),
                }
            }
            DecodedMarketMessage::DepthByOrderSnapshot(DepthByOrderSnapshotMessage::Level(
                level,
            )) => {
                let instrument = self.instrument(&level.identity)?.clone();
                let assembler = self
                    .mbo_books
                    .get_mut(&instrument.descriptor.instrument_id)
                    .ok_or_else(malformed)?;
                match assembler.accept_snapshot_level(
                    level,
                    instrument.descriptor.price_scale,
                    instrument.descriptor.quantity_scale,
                )? {
                    MboBookOutcome::Pending | MboBookOutcome::IgnoredStale => Ok(None),
                    MboBookOutcome::Snapshot { .. } => Err(malformed()),
                }
            }
            DecodedMarketMessage::DepthByOrderSnapshot(DepthByOrderSnapshotMessage::Complete {
                accepted,
                identity,
                sequence_number,
            }) => {
                if !accepted {
                    return Err((
                        ProviderInvalidationReason::Transport,
                        RetryDisposition::Transient,
                    ));
                }
                let instrument = if let Some(identity) = identity.as_ref() {
                    self.instrument(identity)?.clone()
                } else {
                    let mut matching = self
                        .instruments
                        .iter()
                        .filter(|instrument| instrument.order_book);
                    let instrument = matching.next().cloned().ok_or_else(malformed)?;
                    if matching.next().is_some() {
                        return Err(malformed());
                    }
                    instrument
                };
                let assembler = self
                    .mbo_books
                    .get_mut(&instrument.descriptor.instrument_id)
                    .ok_or_else(malformed)?;
                match assembler.finish_snapshot(
                    sequence_number,
                    instrument.descriptor.price_scale,
                    instrument.descriptor.quantity_scale,
                )? {
                    MboBookOutcome::Pending | MboBookOutcome::IgnoredStale => Ok(None),
                    MboBookOutcome::Snapshot {
                        timestamp,
                        bids,
                        asks,
                    } => {
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
                match assembler.accept_update(
                    update,
                    instrument.descriptor.price_scale,
                    instrument.descriptor.quantity_scale,
                )? {
                    MboBookOutcome::Pending | MboBookOutcome::IgnoredStale => Ok(None),
                    MboBookOutcome::Snapshot {
                        timestamp,
                        bids,
                        asks,
                    } => {
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
                match assembler.finish_initial_image(&event)? {
                    MboBookOutcome::Pending | MboBookOutcome::IgnoredStale => Ok(None),
                    MboBookOutcome::Snapshot {
                        timestamp,
                        bids,
                        asks,
                    } => {
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
            let limits = AggregateBookLimits::try_new(
                NonZeroUsize::new(4_096).unwrap_or(NonZeroUsize::MIN),
                NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN),
            )
            .map_err(|_| malformed())?;
            self.books.insert(
                instrument.descriptor.instrument_id.clone(),
                AggregateBookAssembler::new(
                    MarketIdentity {
                        symbol: instrument.descriptor.provider_symbol.clone(),
                        exchange: instrument.descriptor.venue_id.clone(),
                    },
                    limits,
                ),
            );
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
        self.books.remove(instrument_id);
        self.mbo_books.remove(instrument_id);
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

fn canonical_levels(
    levels: &[OrderBookLevel],
    price_scale: u8,
    quantity_scale: u8,
) -> Result<Vec<DepthLevel>, (ProviderInvalidationReason, RetryDisposition)> {
    levels
        .iter()
        .map(|level| {
            Ok(DepthLevel {
                price: fixed_price(level.price, price_scale)?,
                quantity: fixed_quantity(level.size, quantity_scale)?,
                order_count: level.orders,
            })
        })
        .collect()
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
            sequence_number: 41,
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
                sequence_number: 40,
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
                sequence_number: 40,
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
            sequence_number: 42,
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
                    sequence_number: 41,
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
                    sequence_number: 41,
                    timestamp: Some(timestamp()),
                })
                .expect("end marker is non-authoritative while snapshot is pending"),
            MboBookOutcome::Pending
        ));
        assert!(!assembler.ready());
    }
}
