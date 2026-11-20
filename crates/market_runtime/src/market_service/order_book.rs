use super::{
    AggressorSide, AggressorTradeVolumes, Arc, BTreeMap, CanonicalOrderBookState,
    InstallProviderInstrument, MAXIMUM_RECENT_LADDER_TRADES, MarketStream, MarketTrade,
    NonZeroUsize, OrderBook, OrderLevelApplyOutcome, OrderLevelBook, OrderLevelEvent,
    OrderLevelInterruption, OrderLevelState, RECENT_LADDER_TRADE_RETENTION_NANOS, TopOfBookQuote,
    VecDeque, calendar,
};
use crate::study::{
    StudyDepthView, StudyLiveMarketData, StudyMarketInput, StudyQuoteView, StudyTradeWindow,
};
use crate::{
    MAXIMUM_ORDER_LEVEL_VIEW_PRICES, MAXIMUM_ORDER_LEVEL_VIEW_QUEUE, MarketOrderLevelStatus,
    MarketOrderLevelView, MarketOrderQueue,
};
use aeris_market_data::{BookSide, MAXIMUM_ORDER_LEVEL_ORDERS};
use aeris_observability::diagnostic;

const MAXIMUM_CANONICAL_DEPTH_LEVELS: usize = 4_096;

/// Canonical per-instrument live bookkeeper owning L2 depth, market-by-order
/// queues, top-of-book quote, bounded recent trade retention, and sweep windows.
pub(super) struct ProviderOrderBook {
    pub(super) instrument: InstallProviderInstrument,
    pub(super) calendar: calendar::MarketCalendarService,
    top_of_book_only: bool,
    /// Set while no current demand subscribes depth. Providers stop maintaining the
    /// canonical image then, so consumers receive only the live top of book.
    depth_unsubscribed: bool,
    pub(super) book: OrderBook,
    pub(super) top_of_book: Option<TopOfBookQuote>,
    pub(super) recent_trades: VecDeque<crate::RetainedMarketTrade>,
    /// Sweeps classified once per ingested trade over the trailing window of
    /// `recent_trades`; shared by every tape publication until reclassified.
    pub(super) trade_sweeps: Arc<[crate::MarketTradeSweep]>,
    pub(super) traded_volumes: BTreeMap<i64, AggressorTradeVolumes>,
    pub(super) trade_session_generation: u64,
    pub(super) last_trade_source_sequence: u64,
    pub(super) next_trade_ingestion_ordinal: u64,
    pub(super) trade_tape_revision: u64,
    pub(super) trade_tape_rewrite_generation: u64,
    pub(super) trade_tape_dirty: bool,
    pub(super) trade_tape_published_at: Option<std::time::Instant>,
    pub(super) retention_clock_unix_nanos: i64,
    /// When the current session last delivered a newer top of book. Trade-built
    /// series on the same instrument treat it as feed liveness between trades.
    pub(super) quote_observed_at: Option<std::time::Instant>,
    /// Canonical market-by-order image. It mirrors the provider's order-level
    /// stream, which flows only while the instrument holds a subscription.
    pub(super) order_levels: OrderLevelBook,
    order_level_subscription: OrderLevelSubscription,
    /// Images discarded since this book opened, for consumer diagnostics.
    pub(super) order_level_recoveries: u64,
}

/// Whether current demand holds one of a provider's bounded order-level
/// subscriptions for an instrument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OrderLevelSubscription {
    Off,
    Admitted,
    /// Demanded while other instruments hold the provider's whole budget.
    BudgetExhausted,
}

impl ProviderOrderBook {
    pub(super) fn new(instrument: InstallProviderInstrument, top_of_book_only: bool) -> Self {
        let trade_session_generation = instrument.session_generation;
        let mut calendar = calendar::MarketCalendarService::default();
        calendar.resolve(&instrument);
        Self {
            instrument,
            calendar,
            top_of_book_only,
            depth_unsubscribed: false,
            book: OrderBook::new(
                NonZeroUsize::new(MAXIMUM_CANONICAL_DEPTH_LEVELS).unwrap_or(NonZeroUsize::MIN),
            ),
            top_of_book: None,
            recent_trades: VecDeque::new(),
            trade_sweeps: std::sync::Arc::from([]),
            traded_volumes: BTreeMap::new(),
            trade_session_generation,
            last_trade_source_sequence: 0,
            next_trade_ingestion_ordinal: 1,
            trade_tape_revision: 0,
            trade_tape_rewrite_generation: 0,
            trade_tape_dirty: true,
            trade_tape_published_at: None,
            retention_clock_unix_nanos: 0,
            quote_observed_at: None,
            order_levels: OrderLevelBook::new(
                NonZeroUsize::new(MAXIMUM_ORDER_LEVEL_ORDERS).unwrap_or(NonZeroUsize::MIN),
            ),
            order_level_subscription: OrderLevelSubscription::Off,
            order_level_recoveries: 0,
        }
    }

    pub(super) fn set_depth_subscribed(&mut self, subscribed: bool) {
        self.depth_unsubscribed = !subscribed;
    }

    pub(super) const fn order_level_subscription(&self) -> OrderLevelSubscription {
        self.order_level_subscription
    }

    pub(super) fn set_order_level_subscription(&mut self, subscription: OrderLevelSubscription) {
        self.order_level_subscription = subscription;
    }

    /// Applies one provider order-level event and reports whether the
    /// published view changed. The book mirrors the provider stream whatever
    /// the current demand, so a subscription kept across a demand flicker
    /// never misses an update; consumers see the view only while demanding it.
    pub(super) fn apply_order_levels(&mut self, event: &OrderLevelEvent) -> bool {
        if event.metadata().entitlement_id != self.instrument.entitlement_id {
            return false;
        }
        match self.order_levels.apply(event) {
            Ok(OrderLevelApplyOutcome::Applied) => true,
            Ok(OrderLevelApplyOutcome::IgnoredStale) => false,
            Ok(OrderLevelApplyOutcome::RecoveryRequired(reason)) => {
                if reason != OrderLevelInterruption::Released {
                    self.order_level_recoveries = self.order_level_recoveries.saturating_add(1);
                }
                true
            }
            Err(error) => {
                // The adapter validates the same continuity before publishing,
                // so this fails closed until the provider's next covering image.
                diagnostic!("Aeris order-level image discarded by the runtime: {error}");
                self.order_level_recoveries = self.order_level_recoveries.saturating_add(1);
                true
            }
        }
    }

    /// Bounded market-by-order view for a consumer demanding order levels.
    pub(super) fn order_level_view(&self) -> MarketOrderLevelView {
        let book = &self.order_levels;
        let status = match (self.order_level_subscription, book.state()) {
            (OrderLevelSubscription::BudgetExhausted, _) => MarketOrderLevelStatus::BudgetExhausted,
            (_, OrderLevelState::Ready) => MarketOrderLevelStatus::Ready,
            (_, OrderLevelState::Recovering(reason)) => MarketOrderLevelStatus::Recovering(reason),
            // A released image means the provider was asked to subscribe again.
            (
                _,
                OrderLevelState::AwaitingSnapshot
                | OrderLevelState::Unavailable(OrderLevelInterruption::Released),
            ) => MarketOrderLevelStatus::AwaitingSnapshot,
            (_, OrderLevelState::Unavailable(reason)) => {
                MarketOrderLevelStatus::Unavailable(reason)
            }
            (_, OrderLevelState::Stale) => MarketOrderLevelStatus::Stale,
        };
        let ready = status == MarketOrderLevelStatus::Ready;
        let queues = |side| {
            if !ready {
                return Vec::new();
            }
            book.prices(side)
                .take(MAXIMUM_ORDER_LEVEL_VIEW_PRICES)
                .map(|price| {
                    let mut queue = MarketOrderQueue {
                        price,
                        orders: Vec::new(),
                        order_count: 0,
                        quantity: 0,
                    };
                    for order in book.queue(side, price) {
                        queue.order_count += 1;
                        queue.quantity = queue.quantity.saturating_add(order.quantity);
                        if queue.orders.len() < MAXIMUM_ORDER_LEVEL_VIEW_QUEUE {
                            queue.orders.push(order.clone());
                        }
                    }
                    queue
                })
                .collect()
        };
        MarketOrderLevelView {
            status,
            provider_generation: book
                .session_generation()
                .unwrap_or(self.instrument.session_generation),
            image: book.image(),
            revision: book.revision(),
            provider_sequence: book.provider_sequence(),
            recoveries: self.order_level_recoveries,
            bids: queues(BookSide::Bid),
            asks: queues(BookSide::Ask),
        }
    }

    /// Consumers receive only the top of book when the provider has no depth or
    /// no current demand keeps the canonical image maintained.
    pub(super) const fn publishes_top_of_book_only(&self) -> bool {
        self.top_of_book_only || self.depth_unsubscribed
    }

    pub(super) fn update_instrument(&mut self, instrument: InstallProviderInstrument) {
        if instrument.session_generation != self.instrument.session_generation
            || instrument.entitlement_id != self.instrument.entitlement_id
        {
            self.invalidate_live_market(instrument.session_generation);
        }
        self.instrument = instrument;
        self.calendar.resolve(&self.instrument);
    }

    pub(super) fn invalidate_live_market(&mut self, session_generation: u64) -> bool {
        let quote_changed = self.top_of_book.take().is_some();
        self.quote_observed_at = None;
        let trades_changed = !self.recent_trades.is_empty() || !self.traded_volumes.is_empty();
        let previous_revision = self.book.revision();
        self.reset_recent_trades_for_session(session_generation);
        self.book.mark_stale();
        let order_levels_changed = self.order_levels.state() == OrderLevelState::Ready;
        self.order_levels.mark_stale();
        quote_changed
            || trades_changed
            || order_levels_changed
            || self.book.revision() != previous_revision
    }

    fn reset_recent_trades_for_session(&mut self, session_generation: u64) {
        self.recent_trades.clear();
        self.trade_sweeps = std::sync::Arc::from([]);
        self.trade_tape_rewrite_generation = self.trade_tape_rewrite_generation.saturating_add(1);
        self.traded_volumes.clear();
        self.trade_session_generation = session_generation;
        self.last_trade_source_sequence = 0;
        self.next_trade_ingestion_ordinal = 1;
        self.trade_tape_revision = self.trade_tape_revision.saturating_add(1);
        self.trade_tape_dirty = true;
        self.retention_clock_unix_nanos = 0;
    }

    fn remove_recent_trade(&mut self, retained: crate::RetainedMarketTrade) {
        let trade = retained.trade;
        let Some(volumes) = self.traded_volumes.get_mut(&trade.price) else {
            return;
        };
        match trade.aggressor {
            AggressorSide::Buy => volumes.buy = volumes.buy.saturating_sub(trade.quantity),
            AggressorSide::Sell => volumes.sell = volumes.sell.saturating_sub(trade.quantity),
            AggressorSide::Unknown => {}
        }
        if volumes.buy <= 0 && volumes.sell <= 0 {
            self.traded_volumes.remove(&trade.price);
        }
    }

    pub(super) fn prune_recent_trades(&mut self, observed_unix_nanos: i64) -> bool {
        self.retention_clock_unix_nanos = self
            .retention_clock_unix_nanos
            .max(observed_unix_nanos.max(0));
        let cutoff = self
            .retention_clock_unix_nanos
            .saturating_sub(RECENT_LADDER_TRADE_RETENTION_NANOS);
        let mut changed = false;
        while self
            .recent_trades
            .front()
            .is_some_and(|trade| trade.observed_unix_nanos < cutoff)
            || self.recent_trades.len() > MAXIMUM_RECENT_LADDER_TRADES
        {
            let Some(expired) = self.recent_trades.pop_front() else {
                break;
            };
            self.remove_recent_trade(expired);
            changed = true;
        }
        if changed {
            self.trade_tape_revision = self.trade_tape_revision.saturating_add(1);
            self.trade_tape_dirty = true;
            self.refresh_trade_sweeps();
        }
        changed
    }

    /// Reclassifies sweeps over the trailing window after any retention or
    /// ingestion change, once per change, so publications carry the sweeps of
    /// exactly the retained tape.
    fn refresh_trade_sweeps(&mut self) {
        let (Ok(price_scale), Ok(quantity_scale)) = (
            u8::try_from(self.instrument.price_scale),
            u8::try_from(self.instrument.quantity_scale),
        ) else {
            self.trade_sweeps = std::sync::Arc::from([]);
            return;
        };
        self.trade_sweeps = super::trade_sweeps::classify_recent_trade_sweeps(
            &self.recent_trades,
            price_scale,
            quantity_scale,
        );
    }

    pub(super) fn accept_recent_trade(&mut self, trade: &MarketTrade) -> bool {
        if trade.validate().is_err()
            || trade.metadata.provider_id != self.instrument.provider
            || trade.metadata.instrument_id != self.instrument.instrument_id
            || trade.metadata.entitlement_id != self.instrument.entitlement_id
        {
            return false;
        }
        if trade.metadata.session_generation < self.trade_session_generation {
            return false;
        }
        if trade.metadata.session_generation > self.trade_session_generation {
            self.reset_recent_trades_for_session(trade.metadata.session_generation);
        }
        if trade.metadata.source_sequence <= self.last_trade_source_sequence {
            return false;
        }
        self.last_trade_source_sequence = self
            .last_trade_source_sequence
            .max(trade.metadata.source_sequence);
        let observed_unix_nanos = self
            .retention_clock_unix_nanos
            .max(trade.metadata.timestamps.received_unix_nanos);
        self.prune_recent_trades(observed_unix_nanos);
        match trade.aggressor {
            AggressorSide::Buy => {
                let volumes = self.traded_volumes.entry(trade.price).or_default();
                volumes.buy = volumes.buy.saturating_add(trade.quantity);
            }
            AggressorSide::Sell => {
                let volumes = self.traded_volumes.entry(trade.price).or_default();
                volumes.sell = volumes.sell.saturating_add(trade.quantity);
            }
            AggressorSide::Unknown => {}
        }
        let ingestion_ordinal = self.next_trade_ingestion_ordinal;
        self.next_trade_ingestion_ordinal = self.next_trade_ingestion_ordinal.saturating_add(1);
        let timestamps = &trade.metadata.timestamps;
        let timestamp_nanos = timestamps
            .exchange_unix_nanos
            .or(timestamps.provider_unix_nanos)
            .unwrap_or(timestamps.received_unix_nanos);
        let trading_day = self.calendar.trading_day(
            &self.instrument.instrument_id,
            timestamp_nanos.div_euclid(1_000_000),
        );
        let retained = crate::RetainedMarketTrade {
            ingestion_ordinal,
            observed_unix_nanos,
            trading_day,
            trade: std::sync::Arc::new(trade.clone()),
        };
        self.recent_trades.push_back(retained.clone());
        self.trade_tape_revision = self.trade_tape_revision.saturating_add(1);
        self.trade_tape_dirty = true;
        let mut changed = true;
        if self.recent_trades.len() > MAXIMUM_RECENT_LADDER_TRADES {
            changed |= self.prune_recent_trades(observed_unix_nanos);
        }
        self.refresh_trade_sweeps();
        changed
    }

    pub(super) fn install_top_of_book(&mut self, quote: &TopOfBookQuote) -> bool {
        if quote.metadata.provider_id != self.instrument.provider
            || quote.metadata.instrument_id != self.instrument.instrument_id
            || quote.metadata.entitlement_id != self.instrument.entitlement_id
            || quote.metadata.session_generation < self.instrument.session_generation
            || quote.validate().is_err()
        {
            return false;
        }
        if self.top_of_book.as_ref().is_some_and(|current| {
            current.metadata.session_generation > quote.metadata.session_generation
                || (current.metadata.session_generation == quote.metadata.session_generation
                    && current.metadata.source_sequence >= quote.metadata.source_sequence)
        }) {
            return false;
        }
        self.top_of_book = Some(quote.clone());
        true
    }

    pub(super) fn study_live_market_data<'a>(
        &'a self,
        input: &StudyMarketInput,
        provider_generation: u64,
    ) -> Option<StudyLiveMarketData<'a>> {
        if self.instrument.provider != input.series.provider_id
            || self.instrument.instrument_id != input.series.instrument_id
            || self.instrument.entitlement_id != input.series.entitlement_id
        {
            return None;
        }
        let price_scale = u8::try_from(self.instrument.price_scale).ok()?;
        let quantity_scale = u8::try_from(self.instrument.quantity_scale).ok()?;
        let quote = input
            .streams
            .contains(MarketStream::Quotes)
            .then(|| {
                self.top_of_book
                    .as_ref()
                    .filter(|quote| quote.metadata.session_generation == provider_generation)
                    .map(|quote| StudyQuoteView::new(quote, price_scale, quantity_scale))
            })
            .flatten();
        let trades = (input.streams.contains(MarketStream::Trades)
            && self.trade_session_generation == provider_generation)
            .then(|| {
                StudyTradeWindow::new(
                    &self.recent_trades,
                    self.trade_session_generation,
                    self.last_trade_source_sequence,
                    price_scale,
                    quantity_scale,
                )
            });
        let depth = input
            .streams
            .contains(MarketStream::Depth)
            .then(|| {
                (self.book.state() == CanonicalOrderBookState::Ready
                    && self.book.session_generation() == Some(provider_generation))
                .then(|| StudyDepthView::new(&self.book, price_scale, quantity_scale))
            })
            .flatten();
        Some(StudyLiveMarketData::new(quote, trades, depth))
    }
}
