use super::coordinator::StudyChange;
use super::lifecycle::{RecoveryReason, SeriesInput, SeriesLifecycle, SeriesState};
use super::{
    AggressorSide, BTreeMap, BTreeSet, BarSeriesKey, CanonicalOrderBookState, ConsumerId,
    Coordinator, DepthSnapshot, FailureStage, FormingBar, HyperliquidInstrumentDemand,
    InstallProviderInstrument, LIVE_BUFFER_CAPACITY, LIVE_HANDOFF_HISTORY_BARS,
    MAXIMUM_RECENT_LADDER_TRADES, MarketBar, MarketStream, MarketTrade, NonZeroUsize, OrderBook,
    OrderBookApplyOutcome, Ordering, ProviderDemand, ProviderDisconnect, ProviderDisplayDepthEvent,
    ProviderEvent, ProviderEventKind, ProviderGeneration, ProviderHealth, ProviderInstrumentDemand,
    ProviderOrderBook, ProviderTradeBatch, SeriesLoadState, TopOfBookQuote, TradeModel, VecDeque,
    id, merge_live_candle, series_state_payload, series_update_message,
};
#[cfg(test)]
use super::{
    BarPeriod, Duration, HyperliquidDemand, Instant, RithmicRealtimeDemand, TradeHandoffSeed,
};
use crate::hyperliquid_display_depth::{
    HyperliquidDisplayBookDemand, HyperliquidDisplayDepthControl, HyperliquidDisplayDepthDemand,
};
use crate::study::{
    StudyDepthView, StudyLiveMarketData, StudyMarketInput, StudyNonBarChange, StudyQuoteView,
    StudyTradeWindow,
};
use aeris_observability::diagnostic;
#[cfg(test)]
use aeris_rithmic_protocol_adapter::ProviderInvalidationReason;

const MAXIMUM_CANONICAL_DEPTH_LEVELS: usize = 4_096;
const RECENT_TRADE_RETENTION_NANOS: i64 = 8 * 60 * 1_000_000_000;

pub(in crate::market_service) fn enqueue_bar_transition(
    queue: &mut VecDeque<MarketBar>,
    bar: MarketBar,
    overflow: &'static str,
) -> Result<(), String> {
    if let Some(last) = queue.back_mut() {
        if last.source_sequence == bar.source_sequence {
            *last = bar;
            return Ok(());
        }
        if last.source_sequence > bar.source_sequence {
            return Err("live bar transition sequence regressed".to_string());
        }
    }
    if queue.len() == LIVE_BUFFER_CAPACITY {
        return Err(overflow.to_string());
    }
    queue.push_back(bar);
    Ok(())
}

impl ProviderOrderBook {
    pub(super) fn new(instrument: InstallProviderInstrument, top_of_book_only: bool) -> Self {
        let trade_session_generation = instrument.session_generation;
        let mut calendar = super::calendar::MarketCalendarService::default();
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
            traded_volumes: BTreeMap::new(),
            trade_session_generation,
            last_trade_source_sequence: 0,
            next_trade_ingestion_ordinal: 1,
            trade_tape_revision: 0,
            trade_tape_rewrite_generation: 0,
            trade_tape_dirty: true,
            trade_tape_published_at: None,
            retention_clock_unix_nanos: 0,
        }
    }

    pub(super) fn set_depth_subscribed(&mut self, subscribed: bool) {
        self.depth_unsubscribed = !subscribed;
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

    fn invalidate_live_market(&mut self, session_generation: u64) -> bool {
        let quote_changed = self.top_of_book.take().is_some();
        let trades_changed = !self.recent_trades.is_empty() || !self.traded_volumes.is_empty();
        let previous_revision = self.book.revision();
        self.reset_recent_trades_for_session(session_generation);
        self.book.mark_stale();
        quote_changed || trades_changed || self.book.revision() != previous_revision
    }

    fn reset_recent_trades_for_session(&mut self, session_generation: u64) {
        self.recent_trades.clear();
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
            .saturating_sub(RECENT_TRADE_RETENTION_NANOS);
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
        }
        changed
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

impl SeriesLifecycle {
    #[cfg(test)]
    pub(super) fn new(
        series: BarSeriesKey,
        generation: ProviderGeneration,
        wire_coin: &str,
        interval: &str,
    ) -> Self {
        Self::with_clock(
            series,
            generation,
            wire_coin,
            interval,
            std::sync::Arc::new(super::clock::SystemClock),
        )
    }

    pub(super) fn reset(&mut self, generation: ProviderGeneration) {
        self.generation = generation;
        self.bars.clear();
        self.forming = None;
        self.buffered.clear();
        self.pending_publications.clear();
        self.connected = false;
        self.state = SeriesState::Requested { provider: None };
        self.epoch = None;
        self.dirty = false;
    }

    pub(super) fn coverage(&self) -> Option<(i64, i64)> {
        let first = self
            .bars
            .first()
            .copied()
            .or(self.forming.map(|forming| forming.bar))?
            .exchange_timestamp_unix_nanos;
        let last = self
            .forming
            .map(|forming| forming.bar)
            .or_else(|| self.bars.last().copied())?
            .exchange_timestamp_unix_nanos;
        Some((first, last))
    }

    pub(super) fn begin_history_reseed(&mut self) {
        if !self.history_ready() {
            return;
        }
        self.state = SeriesState::Recovering {
            reason: RecoveryReason::DetachedWindow,
            ticket: None,
        };
        self.pending_publications.clear();
        self.dirty = false;
    }

    pub(super) fn cancel_history_reseed(&mut self) -> Result<(), String> {
        if !matches!(
            self.state,
            SeriesState::Recovering {
                reason: RecoveryReason::DetachedWindow,
                ..
            }
        ) {
            return Ok(());
        }
        let buffered = std::mem::take(&mut self.buffered);
        self.state = SeriesState::Live {
            last_progress: self.clock.monotonic(),
            observed_live: true,
        };
        for candle in &buffered {
            if self
                .forming
                .map(|forming| forming.bar)
                .or_else(|| self.bars.last().copied())
                .is_some_and(|tail| {
                    candle.exchange_timestamp_unix_nanos < tail.exchange_timestamp_unix_nanos
                })
            {
                continue;
            }
            if self.ingest(*candle)? {
                self.dirty = true;
            }
        }
        Ok(())
    }

    /// Closes the history/live seam with provider candles.
    ///
    /// `bars` are the periods the provider has closed and the engine has
    /// installed as canonical history. `forming` is the period it caught
    /// open, held here rather than in history and published as the tail the
    /// chart opens on. Live replacements that arrived during the fetch replay
    /// in open-timestamp order; anything at or before the installed tail is
    /// already inside it, so replaying it would count it twice.
    pub(super) fn seed(
        &mut self,
        price_scale: u8,
        quantity_scale: u8,
        bars: &[MarketBar],
        forming: Option<FormingBar>,
    ) -> Result<(), String> {
        self.pending_publications.clear();
        let forming = forming.filter(|forming| {
            bars.last()
                .is_none_or(|last| last.source_sequence < forming.bar.source_sequence)
        });
        let start = bars.len().saturating_sub(LIVE_HANDOFF_HISTORY_BARS);
        let working = bars[start..].to_vec();
        if working.is_empty() && forming.is_none() {
            return Err("Provider candle handoff requires history".to_string());
        }
        self.price_scale = price_scale;
        self.quantity_scale = quantity_scale;
        self.bars = working;
        self.forming = forming;
        // The open period has to reach the consumer even if no candle arrives
        // next: it is the candle the chart opens on.
        if let Some(forming) = self.forming {
            enqueue_bar_transition(
                &mut self.pending_publications,
                forming.bar,
                "Provider candle publication buffer overflowed",
            )?;
        }
        self.dirty = !self.pending_publications.is_empty();
        let buffered = std::mem::take(&mut self.buffered);
        self.state = SeriesState::Live {
            last_progress: self.clock.monotonic(),
            observed_live: false,
        };
        for candle in &buffered {
            if self
                .forming
                .map(|forming| forming.bar)
                .or_else(|| self.bars.last().copied())
                .is_some_and(|tail| {
                    candle.exchange_timestamp_unix_nanos < tail.exchange_timestamp_unix_nanos
                })
            {
                continue;
            }
            if self.ingest(*candle)? {
                self.dirty = true;
            }
        }
        Ok(())
    }

    pub(super) fn take_publication(&mut self) -> Option<Vec<MarketBar>> {
        if !self.connected || !self.history_ready() || !self.dirty {
            return None;
        }
        let tails = self.pending_publications.drain(..).collect::<Vec<_>>();
        self.dirty = false;
        (!tails.is_empty()).then_some(tails)
    }

    pub(super) fn accept_bar(&mut self, bar: MarketBar) -> Result<(), String> {
        if self.history_ready() {
            if self.ingest(bar)? {
                self.dirty = true;
            }
        } else if self.buffered.len() == LIVE_BUFFER_CAPACITY {
            return Err("Provider history/live buffer overflowed".into());
        } else {
            self.buffered.push_back(bar);
        }
        Ok(())
    }

    /// Merges one live replacement, owning the sequence deterministically.
    ///
    /// The same open timestamp keeps the forming sequence; a newer period
    /// extends it. A redelivered update therefore resolves to the identical
    /// bar instead of looking like a new one.
    fn ingest(&mut self, candle: MarketBar) -> Result<bool, String> {
        let active = self
            .forming
            .map(|forming| forming.bar)
            .or_else(|| self.bars.last().copied());
        if let (Some(active), Some(duration)) = (active, self.series.period.duration_nanos())
            && candle.exchange_timestamp_unix_nanos > active.exchange_timestamp_unix_nanos
            && active.exchange_timestamp_unix_nanos.checked_add(duration)
                != Some(candle.exchange_timestamp_unix_nanos)
        {
            return Err("Provider candle has a time gap".to_string());
        }
        let sequence = match &self.forming {
            Some(forming)
                if forming.bar.exchange_timestamp_unix_nanos
                    == candle.exchange_timestamp_unix_nanos =>
            {
                forming.bar.source_sequence
            }
            Some(forming) => forming
                .bar
                .source_sequence
                .checked_add(1)
                .ok_or_else(|| "Provider candle sequence overflowed".to_string())?,
            None => self
                .bars
                .last()
                .map_or(0, |bar| bar.source_sequence)
                .checked_add(1)
                .ok_or_else(|| "Provider candle sequence overflowed".to_string())?,
        };
        let bar = MarketBar {
            source_sequence: sequence,
            exchange_timestamp_seconds: candle
                .exchange_timestamp_unix_nanos
                .div_euclid(1_000_000_000),
            exchange_timestamp_unix_nanos: candle.exchange_timestamp_unix_nanos,
            open: candle.open,
            high: candle.high,
            low: candle.low,
            close: candle.close,
            volume: candle.volume,
        };
        bar.validate().map_err(|error| error.to_string())?;
        let mut forming = self.forming.map(|forming| forming.bar);
        merge_live_candle(&mut self.bars, &mut forming, bar)?;
        self.forming = forming.map(|bar| FormingBar { bar, trades: None });
        if self.bars.len() > LIVE_HANDOFF_HISTORY_BARS {
            let excess = self.bars.len() - LIVE_HANDOFF_HISTORY_BARS;
            self.bars.drain(..excess);
        }
        enqueue_bar_transition(
            &mut self.pending_publications,
            bar,
            "Provider candle publication buffer overflowed",
        )?;
        self.dirty = true;
        self.state = SeriesState::Live {
            last_progress: self.clock.monotonic(),
            observed_live: true,
        };
        Ok(true)
    }
}

fn trading_feed_state(kind: &ProviderEventKind) -> Option<aeris_trading::market::FeedState> {
    use aeris_trading::market::{FeedReason, FeedState};
    match kind {
        ProviderEventKind::Connecting | ProviderEventKind::Recovering { .. } => {
            Some(FeedState::Stale {
                reason: FeedReason::Recovering,
            })
        }
        ProviderEventKind::Connected | ProviderEventKind::Heartbeat(_) => Some(FeedState::Live),
        ProviderEventKind::Disconnected(_) => Some(FeedState::Stale {
            reason: FeedReason::Disconnected,
        }),
        ProviderEventKind::Failed(_) => Some(FeedState::Unavailable {
            reason: FeedReason::Disconnected,
        }),
        _ => None,
    }
}

impl Coordinator<'_> {
    pub(super) fn cancel_inflight_history_for_live_recovery(
        &self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) {
        if let Some(epoch) = self
            .provider_epochs
            .current(&series.provider_id, generation.0.get())
            && let Some((ticket, _)) = self.history.inflight(series, epoch)
        {
            // The buffered live seam is about to be discarded. Any covering
            // history already in flight may end before those discarded events,
            // so its completion can no longer safely seed continuity. The
            // normal cancelled-completion path schedules a fresh covering read.
            self.history
                .cancel_token(ticket)
                .store(true, Ordering::Release);
        }
    }

    pub(super) fn publish_non_bar_study_change(
        &mut self,
        provider_id: &str,
        instrument_id: &str,
        entitlement_id: &str,
        stream: MarketStream,
        observed_unix_nanos: i64,
    ) {
        match self.execute_study_change(StudyChange::NonBar(StudyNonBarChange {
            provider_id,
            instrument_id,
            entitlement_id,
            stream,
            observed_unix_nanos,
        })) {
            Ok(batch) => {
                self.publish_study_outputs(&batch.executed);
                for error in batch.errors {
                    diagnostic!("Aeris live non-bar study execution failed: {error}");
                }
            }
            Err(error) => {
                // Native study failure must not change acceptance/recovery of
                // authoritative provider state.
                diagnostic!("Aeris live non-bar study execution failed: {error}");
            }
        }
    }

    pub(super) fn clear_provider_display_depth(&mut self, provider: &str) {
        let affected = self
            .display_depth
            .keys()
            .filter(|(candidate, _)| candidate == provider)
            .cloned()
            .collect::<Vec<_>>();
        for identity in affected {
            self.display_depth.remove(&identity);
            self.broadcast_order_book(&identity.0, &identity.1);
        }
    }

    pub(super) fn invalidate_provider_live_market(
        &mut self,
        provider: &str,
        generation: ProviderGeneration,
    ) {
        let affected = self
            .order_books
            .iter_mut()
            .filter(|((candidate, _), _)| candidate == provider)
            .filter_map(|(identity, order_book)| {
                order_book
                    .invalidate_live_market(generation.0.get())
                    .then(|| identity.clone())
            })
            .collect::<Vec<_>>();
        for (provider, instrument_id) in affected {
            self.broadcast_order_book(&provider, &instrument_id);
        }
    }

    pub(super) fn handle_provider_display_depth(
        &mut self,
        provider: &str,
        event: ProviderDisplayDepthEvent,
    ) {
        match event {
            ProviderDisplayDepthEvent::Reset { display_generation } => {
                let current = self.display_generations.entry(provider.into()).or_default();
                if display_generation <= *current {
                    return;
                }
                *current = display_generation;
                self.clear_provider_display_depth(provider);
            }
            ProviderDisplayDepthEvent::Snapshot {
                provider_generation,
                display_generation,
                snapshot,
            } => {
                let current = self.display_generations.get(provider).copied().unwrap_or(0);
                if display_generation < current {
                    return;
                }
                let Some(current_generation) = self
                    .engine
                    .provider_status(provider)
                    .and_then(|status| status.generation)
                else {
                    return;
                };
                if current_generation.0.get() != provider_generation
                    || snapshot.metadata.provider_id != provider
                    || snapshot.metadata.session_generation != provider_generation
                {
                    return;
                }
                let instrument_id = snapshot.metadata.instrument_id.clone();
                let Some(order_book) = self
                    .order_books
                    .get(&(provider.to_string(), instrument_id.clone()))
                else {
                    return;
                };
                if order_book.instrument.entitlement_id != snapshot.metadata.entitlement_id {
                    return;
                }
                // A full display queue may coalesce its reset. A validated
                // snapshot carries the same generation boundary itself.
                if display_generation > current {
                    self.display_generations
                        .insert(provider.into(), display_generation);
                    self.clear_provider_display_depth(provider);
                }
                let source_sequence = snapshot.metadata.source_sequence;
                if self
                    .display_depth
                    .get(&(provider.into(), instrument_id.clone()))
                    .is_some_and(|current| {
                        current.provider_generation > provider_generation
                            || (current.provider_generation == provider_generation
                                && (current.display_generation > display_generation
                                    || (current.display_generation == display_generation
                                        && current.source_sequence >= source_sequence)))
                    })
                {
                    return;
                }
                self.display_depth.insert(
                    (provider.into(), instrument_id.clone()),
                    crate::MarketDisplayDepth {
                        provider_generation,
                        display_generation,
                        source_sequence,
                        bids: snapshot.bids,
                        asks: snapshot.asks,
                    },
                );
                self.broadcast_order_book(provider, &instrument_id);
            }
        }
    }

    pub(super) fn install_live_tails(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        price_scale: u8,
        quantity_scale: u8,
        bars: Vec<MarketBar>,
    ) -> Result<(), String> {
        for bar in bars {
            if self.compact_live_series_if_needed(series, generation)? {
                // `take_publication` already drained the provider-owned handoff;
                // while detached, canonical history must not accept a tail that
                // is not contiguous with its bounded historical window.
                break;
            }
            let exchange_timestamp_unix_nanos = bar.exchange_timestamp_unix_nanos;
            let publications = match self.engine.install_realtime_tail(
                generation,
                series,
                price_scale,
                quantity_scale,
                bar,
                true,
            ) {
                Ok(publications) => publications,
                Err(error) => {
                    let canonical = self
                        .engine
                        .series_segments(series)
                        .and_then(aeris_market_engine::SeriesSegments::last);
                    return Err(format!(
                        "{error}; handoff bar sequence={} time={} canonical_tail={canonical:?}",
                        bar.source_sequence, bar.exchange_timestamp_unix_nanos
                    ));
                }
            };
            for publication in publications {
                if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                    events.publish_series_update(series_update_message(&publication));
                }
            }
            match self.execute_study_change(StudyChange::Bar(series, exchange_timestamp_unix_nanos))
            {
                Ok(batch) => {
                    self.publish_study_outputs(&batch.executed);
                    for error in batch.errors {
                        diagnostic!("Aeris live study execution failed: {error}");
                    }
                }
                Err(error) => {
                    // Study failure is isolated from canonical market publication.
                    // A user calculation must never force provider recovery or make
                    // an accepted market tail look discontinuous.
                    diagnostic!("Aeris live study execution failed: {error}");
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn rithmic_realtime_demand(&self) -> Result<RithmicRealtimeDemand, String> {
        self.provider_demand(super::RITHMIC_DESCRIPTOR.id)
            .rithmic_wire()
    }

    pub(super) fn provider_demand(&self, provider_id: &str) -> ProviderDemand {
        let mut instruments = BTreeMap::<String, ProviderInstrumentDemand>::new();
        let mut candle_series = Vec::new();
        let mut missing_catalog = false;
        for series in self
            .series
            .trade_keys()
            .chain(self.series.candle_keys())
            .filter(|series| series.provider_id == provider_id)
        {
            let Some(streams) = self
                .engine
                .subscription_status(series)
                .map(|status| status.streams)
            else {
                continue;
            };
            let Some(instrument) = self
                .catalog
                .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                .cloned()
            else {
                missing_catalog = true;
                continue;
            };
            if self
                .providers
                .descriptor(provider_id)
                .is_some_and(|descriptor| descriptor.capabilities.candle_wire_interval.is_some())
            {
                candle_series.push((series.clone(), streams, instrument.clone()));
            }
            instruments
                .entry(instrument.instrument_id.clone())
                .and_modify(|entry| entry.streams = entry.streams.union(streams))
                .or_insert(ProviderInstrumentDemand {
                    instrument,
                    streams,
                    display_depth: false,
                });
        }
        self.merge_instrument_subscriptions(provider_id, &mut instruments, &mut missing_catalog);
        self.mark_display_depth_demand(provider_id, &mut instruments);
        ProviderDemand {
            instruments: instruments.into_values().collect(),
            candle_series,
            explicit_trade_ids: self
                .engine
                .subscriptions()
                .into_iter()
                .filter(|(series, subscription)| {
                    series.provider_id == provider_id
                        && subscription.streams.contains(MarketStream::Trades)
                })
                .map(|(series, _)| series.instrument_id)
                .collect(),
            missing_catalog,
        }
    }

    fn merge_instrument_subscriptions(
        &self,
        provider_id: &str,
        instruments: &mut BTreeMap<String, ProviderInstrumentDemand>,
        missing_catalog: &mut bool,
    ) {
        for (key, streams) in self
            .engine
            .instrument_subscriptions()
            .filter(|(key, _)| key.provider == provider_id)
        {
            let Some(instrument) = self
                .catalog
                .get(&(key.provider.clone(), key.instrument.as_str().to_string()))
                .filter(|instrument| {
                    key.entitlement.as_deref() == Some(instrument.entitlement_id.as_str())
                })
                .cloned()
            else {
                *missing_catalog = true;
                continue;
            };
            instruments
                .entry(instrument.instrument_id.clone())
                .and_modify(|entry| entry.streams = entry.streams.union(streams))
                .or_insert(ProviderInstrumentDemand {
                    instrument,
                    streams,
                    display_depth: false,
                });
        }
    }

    fn mark_display_depth_demand(
        &self,
        provider_id: &str,
        instruments: &mut BTreeMap<String, ProviderInstrumentDemand>,
    ) {
        for consumer_id in self.events.keys() {
            let Some(demand) = self.engine.current_demand(*consumer_id) else {
                continue;
            };
            if !demand.resource_class.publishes_ui()
                || !demand
                    .streams
                    .is_some_and(|streams| streams.contains(MarketStream::Depth))
            {
                continue;
            }
            let Some(series) = demand
                .series
                .as_ref()
                .filter(|series| series.provider_id == provider_id)
            else {
                continue;
            };
            if self
                .order_books
                .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                .is_some_and(|book| book.instrument.entitlement_id == series.entitlement_id)
                && let Some(entry) = instruments.get_mut(&series.instrument_id)
            {
                entry.display_depth = true;
            }
        }
    }

    pub(super) fn send_rithmic_demand(&mut self) -> Result<(), String> {
        if !self.providers.rithmic_realtime_enabled() {
            return Ok(());
        }
        let neutral = self.provider_demand(super::RITHMIC_DESCRIPTOR.id);
        if neutral.rithmic_wire()?.instruments.is_empty() {
            return Ok(());
        }
        if self.session(super::RITHMIC_DESCRIPTOR.id).accepted.as_ref() == Some(&neutral)
            && self.session(super::RITHMIC_DESCRIPTOR.id).pending.is_none()
        {
            return Ok(());
        }
        if !self.ensure_provider_connection(super::RITHMIC_DESCRIPTOR.id) {
            return Ok(());
        }
        if self
            .provider_registry
            .apply_demand(super::RITHMIC_DESCRIPTOR.id, &neutral)
            .is_ok_and(|sent| sent)
        {
            let session = self.session_mut(super::RITHMIC_DESCRIPTOR.id);
            session.pending = None;
            session.accepted = Some(neutral);
        } else {
            self.session_mut(super::RITHMIC_DESCRIPTOR.id).pending = Some(neutral);
        }
        Ok(())
    }

    pub(super) fn ensure_realtime(&mut self, series: &BarSeriesKey) -> Result<(), String> {
        let streams = self
            .engine
            .subscription_status(series)
            .map(|status| status.streams)
            .ok_or_else(|| "series has no accepted upstream subscription".to_string())?;
        // This is deliberately capability-only. After an account/offline
        // suspension the provider session is inactive; the Select/Subscribe
        // control queued below is what wakes the worker and creates the next
        // active generation. Requiring an active session here deadlocks that
        // recovery before the wake-up control can be sent.
        self.engine
            .verify_provider_stream_requirements(&series.provider_id, streams)
            .map_err(|error| error.to_string())?;
        let descriptor = self
            .providers
            .descriptor(&series.provider_id)
            .ok_or_else(|| "realtime provider is unsupported".to_string())?;
        let connected = self
            .engine
            .provider_status(&series.provider_id)
            .is_some_and(|status| status.health == ProviderHealth::Online);
        match descriptor.capabilities.live_model {
            super::LiveModel::TradeBuilt => {
                if !self.series.trade_contains_key(series) {
                    let generation = self.provider_generation_for_series(series)?;
                    let instrument = self
                        .catalog
                        .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                        .ok_or_else(|| {
                            descriptor
                                .capabilities
                                .instrument_missing_detail
                                .to_string()
                        })?
                        .clone();
                    if let Some(mut handoff) = TradeModel::with_calendar(
                        series,
                        generation,
                        std::sync::Arc::clone(&self.clock),
                        self.calendar.for_instrument(&instrument.instrument_id),
                        descriptor.capabilities.calendar_bar_stamp,
                    ) {
                        handoff.connected = connected;
                        self.series.insert_trade(series.clone(), handoff);
                    }
                }
                match descriptor.capabilities.trade_demand_policy {
                    super::TradeDemandPolicy::Immediate => self.send_rithmic_demand()?,
                    super::TradeDemandPolicy::SessionManaged => {
                        self.flush_session_managed_demand(descriptor.id);
                    }
                }
            }
            super::LiveModel::ProviderCandles => {
                if descriptor.capabilities.candle_demand_policy
                    == super::CandleDemandPolicy::ReconcileAfterSelection
                {
                    // Newest-wins controls may replace a queued idle Stop without a
                    // Disconnected event. Retire that fence when demand resumes.
                    self.session_mut(&series.provider_id).stop_pending = None;
                }
                if !self.series.candle_contains_key(series) {
                    let generation = self.provider_generation_for_series(series)?;
                    let instrument = self.candle_instrument(series)?.clone();
                    let interval = descriptor
                        .capabilities
                        .candle_wire_interval
                        .ok_or_else(|| "provider candle interval is unavailable".to_string())?(
                        series.period,
                    )?;
                    let mut handoff = SeriesLifecycle::with_clock(
                        series.clone(),
                        generation,
                        &instrument.provider_symbol,
                        &interval,
                        std::sync::Arc::clone(&self.clock),
                    );
                    handoff.connected = connected;
                    self.series.insert_candle(series.clone(), handoff);
                }
                if descriptor.capabilities.candle_demand_policy
                    == super::CandleDemandPolicy::ReconcileAfterSelection
                {
                    self.session_mut(&series.provider_id).demand_dirty = true;
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn handle_provider_event(&mut self, wire: super::WireProviderEvent) {
        let engine_generation = self
            .engine
            .provider_status(wire.provider)
            .and_then(|status| status.generation)
            .map(|generation| generation.0.get());
        if wire.generation == 0
            || engine_generation.is_some_and(|current| wire.generation < current)
        {
            self.provider_epochs.drop_event();
            return;
        }
        let Some(epoch) =
            self.provider_epochs
                .stamp(wire.provider, wire.generation, &mut self.provider_registry)
        else {
            self.provider_epochs.drop_event();
            return;
        };
        self.dispatch_provider_event(ProviderEvent {
            provider: wire.provider,
            generation: epoch,
            wire_generation: wire.generation,
            kind: wire.kind,
        });
    }

    pub(super) fn handle_bound_provider_event(&mut self, event: ProviderEvent) {
        if !self
            .provider_epochs
            .accepts(event.provider, event.generation)
        {
            self.provider_epochs.drop_event();
            return;
        }
        if matches!(event.kind, ProviderEventKind::TransportReplaced) {
            match self.provider_registry.rebind(event.provider) {
                Ok(Some(epoch)) => {
                    self.provider_epochs
                        .bind_wire(event.provider, epoch, event.wire_generation);
                    self.session_mut(event.provider).accepted = None;
                    self.session_mut(event.provider).pending = None;
                    let demand = self.provider_demand(event.provider);
                    if !demand.instruments.is_empty() || !demand.candle_series.is_empty() {
                        match self.provider_registry.apply_demand(event.provider, &demand) {
                            Ok(true) => self.session_mut(event.provider).accepted = Some(demand),
                            Ok(false) => self.session_mut(event.provider).pending = Some(demand),
                            Err(_) => self.control_retry = true,
                        }
                    }
                }
                _ => self.control_retry = true,
            }
            return;
        }
        if self
            .provider_epochs
            .current(event.provider, event.wire_generation)
            .is_none()
        {
            self.provider_epochs
                .bind_wire(event.provider, event.generation, event.wire_generation);
        }
        if self
            .provider_registry
            .adapter_retries_transport(event.provider)
        {
            if matches!(event.kind, ProviderEventKind::Connected) {
                self.provider_registry.connected(
                    event.provider,
                    event.generation,
                    self.clock.monotonic(),
                );
            }
            self.dispatch_provider_event(event);
            return;
        }
        match &event.kind {
            ProviderEventKind::Connected => self.provider_registry.connected(
                event.provider,
                event.generation,
                self.clock.monotonic(),
            ),
            ProviderEventKind::Disconnected(ProviderDisconnect::Recover {
                retry_after: Some(retry_after),
                ..
            }) => {
                self.provider_registry.disconnected_with_class(
                    event.provider,
                    event.generation,
                    self.clock.monotonic(),
                    super::reconnect::RetryClass::RateLimited {
                        retry_after: *retry_after,
                    },
                );
            }
            ProviderEventKind::Failed(_) => self.provider_registry.disconnected_with_class(
                event.provider,
                event.generation,
                self.clock.monotonic(),
                super::reconnect::RetryClass::Fatal,
            ),
            ProviderEventKind::Disconnected(_) => self.provider_registry.disconnected(
                event.provider,
                event.generation,
                self.clock.monotonic(),
            ),
            _ => {}
        }
        if matches!(
            event.kind,
            ProviderEventKind::Disconnected(_) | ProviderEventKind::Failed(_)
        ) && self.provider_registry.has_control(event.provider)
        {
            self.session_mut(event.provider).accepted = None;
        }
        self.dispatch_provider_event(event);
    }

    fn dispatch_provider_event(&mut self, event: ProviderEvent) {
        if !self.accepts_provider_event(&event) {
            self.provider_epochs.drop_event();
            return;
        }
        let ProviderEvent {
            provider,
            wire_generation: generation,
            kind,
            ..
        } = event;
        let Some(descriptor) = self.providers.descriptor(provider) else {
            return;
        };
        let feed_state = trading_feed_state(&kind);
        match kind {
            ProviderEventKind::TransportReplaced => unreachable!("handled before dispatch"),
            ProviderEventKind::Connecting => {
                self.begin_lifecycle_session(
                    provider,
                    generation,
                    descriptor.capabilities.live_model,
                );
            }
            ProviderEventKind::Connected => {
                self.provider_connected(provider, generation, descriptor);
            }
            ProviderEventKind::Heartbeat(_) => {
                self.mark_lifecycle_online(
                    provider,
                    generation,
                    descriptor.capabilities.live_model,
                );
            }
            ProviderEventKind::Recovering {
                detail,
                provider_detail,
                recovery_reason,
            } => self.handle_provider_recovering_event(
                provider,
                generation,
                descriptor.capabilities.live_model,
                detail,
                provider_detail,
                recovery_reason,
            ),
            ProviderEventKind::Failed(detail) => {
                self.mark_lifecycle_failed(provider, generation, &detail);
            }
            ProviderEventKind::Disconnected(reason) => {
                self.handle_provider_disconnected(provider, generation, descriptor, &reason);
            }
            ProviderEventKind::Trades(batch) => {
                self.accept_provider_trades(
                    provider,
                    generation,
                    descriptor.capabilities.live_model,
                    &batch,
                );
            }
            ProviderEventKind::Quote(quote) => self.provider_quote(provider, generation, &quote),
            ProviderEventKind::Depth(depth) => self.provider_depth(provider, generation, &depth),
            ProviderEventKind::Candle { symbol, bar } => {
                self.accept_provider_candle(provider, generation, &symbol, bar, descriptor);
            }
        }
        if let Some(state) = feed_state {
            self.publish_provider_feed(provider, generation, state);
        }
    }

    fn provider_connected(
        &mut self,
        provider: &str,
        generation: u64,
        descriptor: super::ProviderDescriptor,
    ) {
        if descriptor.presentation.catalog_refresh_on_startup {
            self.publish_catalog_ready(provider);
        }
        self.mark_lifecycle_online(provider, generation, descriptor.capabilities.live_model);
        // The trading owner clears per-instrument state on a generation bump; restore its
        // session contract before the trailing Feed::Live resumes quote flow.
        self.republish_trading_sessions(provider);
    }

    fn handle_provider_recovering_event(
        &mut self,
        provider: &str,
        generation: u64,
        model: super::LiveModel,
        detail: &'static str,
        provider_detail: Option<String>,
        recovery_reason: Option<RecoveryReason>,
    ) {
        if model == super::LiveModel::TradeBuilt
            && recovery_reason == Some(RecoveryReason::SequenceGap)
        {
            self.recover_trade_sequence_gaps(provider, generation, detail);
        }
        self.mark_lifecycle_recovering(provider, generation, model, detail);
        if let Some(provider_detail) = provider_detail {
            self.broadcast_provider_for(provider, Some(&provider_detail));
        }
    }

    fn accepts_provider_event(&self, event: &ProviderEvent) -> bool {
        self.provider_registry.has_control(event.provider) && {
            let session = self.session(event.provider);
            self.provider_epochs
                .accepts(event.provider, event.generation)
                && (session
                    .stop_pending
                    .is_none_or(|pending| pending.0.get() != event.wire_generation)
                    || matches!(event.kind, ProviderEventKind::Disconnected(_)))
        }
    }

    pub(super) fn flush_session_managed_demand(&mut self, provider: &'static str) {
        if !self.provider_registry.has_control(provider) {
            return;
        }
        let demand = self.provider_demand(provider);
        if self.provider_registry.contains(provider)
            && (!demand.candle_series.is_empty() || !demand.instruments.is_empty())
            && !self.provider_registry.catalog_ready(provider)
            && (!self.ensure_provider_connection(provider)
                || !self.provider_registry.catalog_ready(provider))
        {
            // A retry deadline, not a new demand edge, reserves the next epoch.
            return;
        }
        if self.session(provider).accepted.as_ref() == Some(&demand) {
            return;
        }
        if demand.candle_series.is_empty() && demand.instruments.is_empty() {
            if self.session(provider).accepted.is_some() {
                let sent = self
                    .provider_registry
                    .release_idle(provider)
                    .is_ok_and(|sent| sent);
                if sent {
                    self.session_mut(provider).accepted = Some(demand);
                } else {
                    self.control_retry = true;
                }
            }
            return;
        }
        let sent = self.ensure_provider_connection(provider)
            && self
                .provider_registry
                .apply_demand(provider, &demand)
                .is_ok_and(|sent| sent);
        if sent {
            self.session_mut(provider).accepted = Some(demand);
        } else {
            self.control_retry = true;
        }
    }

    fn accept_provider_trades(
        &mut self,
        provider: &str,
        generation: u64,
        live_model: super::LiveModel,
        batch: &ProviderTradeBatch,
    ) {
        let trades = match &batch {
            ProviderTradeBatch::One(trade) => std::slice::from_ref(trade),
            ProviderTradeBatch::Many(trades) => trades.as_slice(),
        };
        if live_model == super::LiveModel::TradeBuilt {
            for trade in trades {
                self.accept_lifecycle_trade(provider, generation, trade);
            }
        } else {
            let mut dirty_books = BTreeSet::new();
            for trade in trades {
                if let Some(instrument_id) =
                    self.accept_provider_candle_trade(provider, generation, trade)
                {
                    dirty_books.insert(instrument_id);
                }
            }
            for instrument_id in dirty_books {
                self.broadcast_order_book(provider, &instrument_id);
            }
        }
        self.request_history_after_live_trades(provider, generation, trades);
    }

    fn request_history_after_live_trades(
        &mut self,
        provider: &str,
        generation: u64,
        trades: &[MarketTrade],
    ) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        for trade in trades {
            if trade.metadata.provider_id != provider
                || trade.metadata.session_generation != generation.0.get()
            {
                continue;
            }
            if let Some(exchange_unix_nanos) = trade.metadata.timestamps.exchange_unix_nanos {
                self.request_current_history_after_live_trade(
                    provider,
                    generation,
                    &trade.metadata.instrument_id,
                    &trade.metadata.entitlement_id,
                    exchange_unix_nanos,
                );
            }
        }
    }

    fn accept_provider_candle(
        &mut self,
        provider: &str,
        generation: u64,
        symbol: &super::provider_event::ProviderCandleSymbol,
        bar: MarketBar,
        descriptor: super::ProviderDescriptor,
    ) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        if descriptor.capabilities.live_model != super::LiveModel::ProviderCandles {
            return;
        }
        let matched = self
            .series
            .candle_iter()
            .filter(|(series, live)| {
                series.provider_id == provider
                    && live.generation == generation
                    && (!descriptor.capabilities.candle_requires_connected || live.connected)
                    && live.matches_candle_symbol(symbol)
            })
            .map(|(series, _)| series.clone())
            .collect::<Vec<_>>();
        let mut failed = Vec::new();
        for series in &matched {
            self.transition_candle_series(series, SeriesInput::Candle);
            if self
                .series
                .candle_mut(series)
                .is_some_and(|live| live.accept_bar(bar).is_err())
            {
                failed.push(series.clone());
            }
        }
        // Without a trade stream the first live candle is the only
        // evidence that a series answered empty now has data.
        if !descriptor
            .capabilities
            .engine
            .streams
            .contains(MarketStream::Trades)
        {
            for series in matched {
                self.request_current_history_after_live_trade(
                    provider,
                    generation,
                    &series.instrument_id,
                    &series.entitlement_id,
                    bar.exchange_timestamp_unix_nanos,
                );
            }
        }
        for series in failed {
            self.recover_lifecycle_series(
                &series,
                generation,
                FailureStage::Aggregation,
                descriptor.capabilities.candle_correction_detail,
            );
        }
    }

    fn handle_provider_disconnected(
        &mut self,
        provider: &str,
        generation: u64,
        descriptor: super::ProviderDescriptor,
        reason: &ProviderDisconnect,
    ) {
        if let ProviderDisconnect::Recover {
            provider_detail: Some(detail),
            ..
        } = reason
        {
            self.broadcast_provider_for(provider, Some(detail));
        }
        let stop_pending = self
            .session(provider)
            .stop_pending
            .is_some_and(|pending| pending.0.get() == generation);
        if stop_pending {
            self.handle_idle_stop_overlap(provider, generation, descriptor);
            return;
        }
        match descriptor.capabilities.live_model {
            super::LiveModel::TradeBuilt => {
                self.session_mut(provider).accepted = None;
                self.session_mut(provider).pending = None;
                let (detail, auto_recover, failed) = match reason {
                    ProviderDisconnect::SequenceGap {
                        detail,
                        auto_recover,
                    } => {
                        self.recover_trade_sequence_gaps(provider, generation, detail);
                        (*detail, *auto_recover, false)
                    }
                    ProviderDisconnect::Recover {
                        detail,
                        auto_recover,
                        ..
                    } => (*detail, *auto_recover, false),
                    ProviderDisconnect::Fail {
                        detail,
                        auto_recover,
                    } => (*detail, *auto_recover, true),
                };
                if failed {
                    self.mark_lifecycle_failed(provider, generation, detail);
                } else {
                    self.mark_lifecycle_recovering(
                        provider,
                        generation,
                        descriptor.capabilities.live_model,
                        detail,
                    );
                }
                if auto_recover
                    && descriptor.capabilities.recovery_policy
                        == super::ProviderRecoveryPolicy::CoordinatorReissuesDemand
                    && (self
                        .series
                        .trade_keys()
                        .any(|series| series.provider_id == provider)
                        || self.price_alerts.has_active_provider(provider))
                {
                    let _ = self.send_rithmic_demand();
                }
            }
            super::LiveModel::ProviderCandles => {
                self.session_mut(provider).engaged = false;
                let detail = match reason {
                    ProviderDisconnect::Recover { detail, .. }
                    | ProviderDisconnect::Fail { detail, .. }
                    | ProviderDisconnect::SequenceGap { detail, .. } => *detail,
                };
                self.mark_lifecycle_recovering(
                    provider,
                    generation,
                    descriptor.capabilities.live_model,
                    detail,
                );
            }
        }
    }

    fn handle_idle_stop_overlap(
        &mut self,
        provider: &str,
        generation: u64,
        descriptor: super::ProviderDescriptor,
    ) {
        self.session_mut(provider).stop_pending = None;
        let demanded = self.has_provider_live_demand(provider);
        if demanded {
            match descriptor.capabilities.live_model {
                super::LiveModel::TradeBuilt => {
                    self.session_mut(provider).accepted = None;
                    self.session_mut(provider).pending = None;
                    if descriptor.capabilities.trade_demand_policy
                        == super::TradeDemandPolicy::Immediate
                    {
                        let _ = self.send_rithmic_demand();
                    }
                    self.mark_lifecycle_recovering(
                        provider,
                        generation,
                        descriptor.capabilities.live_model,
                        "Trade-built realtime restarted after idle-stop overlap",
                    );
                }
                super::LiveModel::ProviderCandles => {
                    self.session_mut(provider).engaged = false;
                    self.session_mut(provider).demand_dirty = true;
                    self.mark_lifecycle_recovering(
                        provider,
                        generation,
                        descriptor.capabilities.live_model,
                        "Provider realtime restarted after idle-stop overlap",
                    );
                }
            }
            return;
        }
        let session = self.session_mut(provider);
        session.accepted = None;
        session.pending = None;
        session.engaged = false;
        session.demand_dirty = false;
        self.end_provider_live_session(provider, generation);
    }

    fn end_provider_live_session(&mut self, provider: &str, generation: u64) {
        if let Ok(generation) = id(generation).map(ProviderGeneration) {
            let _ = self.engine.end_provider_session(provider, generation);
        }
        self.broadcast_provider_for(provider, None);
    }

    pub(super) fn accept_provider_candle_trade(
        &mut self,
        provider: &str,
        generation: u64,
        trade: &MarketTrade,
    ) -> Option<String> {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return None;
        };
        if self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return None;
        }
        if trade.metadata.provider_id != provider
            || trade.metadata.session_generation != generation.0.get()
        {
            self.mark_lifecycle_recovering(
                provider,
                generation.0.get(),
                super::LiveModel::ProviderCandles,
                "Provider live session identity requires recovery",
            );
            for (series, live) in self.series.candle_iter_mut() {
                if series.provider_id != provider {
                    continue;
                }
                live.state = SeriesState::Recovering {
                    reason: RecoveryReason::ProviderReconnected,
                    ticket: None,
                };
                live.dirty = false;
                live.buffered.clear();
                live.pending_publications.clear();
            }
            return None;
        }
        self.evaluate_price_alert_trade(trade);
        let instrument_id = trade.metadata.instrument_id.clone();
        let entitlement_id = trade.metadata.entitlement_id.clone();
        let observed_unix_nanos = trade
            .metadata
            .timestamps
            .exchange_unix_nanos
            .or(trade.metadata.timestamps.provider_unix_nanos)
            .unwrap_or(trade.metadata.timestamps.received_unix_nanos);
        let trade_changed = self
            .order_books
            .get_mut(&(provider.to_string(), instrument_id.clone()))
            .is_some_and(|order_book| order_book.accept_recent_trade(trade));
        if trade_changed {
            self.publish_non_bar_study_change(
                provider,
                &instrument_id,
                &entitlement_id,
                MarketStream::Trades,
                observed_unix_nanos,
            );
        }
        trade_changed.then_some(instrument_id)
    }

    pub(super) fn evaluate_price_alert_trade(&mut self, trade: &MarketTrade) {
        let instrument = self
            .price_alerts
            .active_instruments(&trade.metadata.provider_id)
            .into_iter()
            .find(|instrument| {
                instrument.instrument_id == trade.metadata.instrument_id
                    && instrument.entitlement_id == trade.metadata.entitlement_id
            });
        let Some(instrument) = instrument else {
            return;
        };
        let observed_unix_nanos = trade
            .metadata
            .timestamps
            .exchange_unix_nanos
            .unwrap_or(trade.metadata.timestamps.received_unix_nanos);
        let triggers = self.price_alerts.evaluate(
            &instrument,
            trade.metadata.session_generation,
            trade.metadata.source_sequence,
            trade.price,
            observed_unix_nanos,
        );
        let demand_changed = triggers.iter().any(|trigger| !trigger.remains_active);
        for trigger in triggers {
            if let Some(events) = self.events.get_mut(&trigger.consumer_id) {
                events.publish_price_alert(crate::MarketRuntimeEvent::PriceAlertTriggered(trigger));
            }
        }
        if demand_changed {
            if let Err(error) = self.reconcile_price_alert_leases() {
                diagnostic!("Aeris price alert lease release failed: {error}");
            }
            self.release_idle();
        }
    }

    pub(super) fn provider_depth(
        &mut self,
        provider: &str,
        generation: u64,
        snapshot: &DepthSnapshot,
    ) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation)
            != Some(generation)
            || snapshot.metadata.provider_id != provider
            || snapshot.metadata.session_generation != generation.0.get()
        {
            return;
        }
        let instrument_id = snapshot.metadata.instrument_id.clone();
        let entitlement_id = snapshot.metadata.entitlement_id.clone();
        let observed_unix_nanos = snapshot
            .metadata
            .timestamps
            .exchange_unix_nanos
            .or(snapshot.metadata.timestamps.provider_unix_nanos)
            .unwrap_or(snapshot.metadata.timestamps.received_unix_nanos);
        let (book_changed, trades_changed) = self
            .order_books
            .get_mut(&(provider.to_string(), instrument_id.clone()))
            .filter(|order_book| {
                order_book.instrument.entitlement_id == snapshot.metadata.entitlement_id
            })
            .map_or((false, false), |order_book| {
                let trades_changed = order_book.prune_recent_trades(observed_unix_nanos);
                let book_changed = matches!(
                    order_book.book.install_snapshot(snapshot),
                    Ok(OrderBookApplyOutcome::Published
                        | OrderBookApplyOutcome::RecoveryRequired(_))
                ) || matches!(
                    order_book.book.state(),
                    CanonicalOrderBookState::Recovering(_)
                );
                (book_changed, trades_changed)
            });
        if book_changed || trades_changed {
            self.broadcast_order_book(provider, &instrument_id);
        }
        if book_changed {
            self.publish_non_bar_study_change(
                provider,
                &instrument_id,
                &entitlement_id,
                MarketStream::Depth,
                observed_unix_nanos,
            );
        }
        if trades_changed {
            self.publish_non_bar_study_change(
                provider,
                &instrument_id,
                &entitlement_id,
                MarketStream::Trades,
                observed_unix_nanos,
            );
        }
    }

    pub(super) fn provider_quote(
        &mut self,
        provider: &str,
        generation: u64,
        quote: &TopOfBookQuote,
    ) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation)
            != Some(generation)
            || quote.metadata.provider_id != provider
            || quote.metadata.session_generation != generation.0.get()
        {
            return;
        }
        let instrument_id = quote.metadata.instrument_id.clone();
        let entitlement_id = quote.metadata.entitlement_id.clone();
        let observed_unix_nanos = quote
            .metadata
            .timestamps
            .exchange_unix_nanos
            .or(quote.metadata.timestamps.provider_unix_nanos)
            .unwrap_or(quote.metadata.timestamps.received_unix_nanos);
        let (quote_changed, trades_changed) = self
            .order_books
            .get_mut(&(provider.to_string(), instrument_id.clone()))
            .map_or((false, false), |order_book| {
                let trades_changed = order_book.prune_recent_trades(observed_unix_nanos);
                (order_book.install_top_of_book(quote), trades_changed)
            });
        if quote_changed || trades_changed {
            self.broadcast_order_book(provider, &instrument_id);
        }
        if quote_changed {
            self.update_trading_quote(provider, &instrument_id, &entitlement_id, quote);
            self.publish_non_bar_study_change(
                provider,
                &instrument_id,
                &entitlement_id,
                MarketStream::Quotes,
                observed_unix_nanos,
            );
        }
        if trades_changed {
            self.publish_non_bar_study_change(
                provider,
                &instrument_id,
                &entitlement_id,
                MarketStream::Trades,
                observed_unix_nanos,
            );
        }
    }

    fn update_trading_quote(
        &mut self,
        provider: &str,
        instrument_id: &str,
        entitlement_id: &str,
        quote: &TopOfBookQuote,
    ) {
        if let Some(instrument) = self
            .catalog
            .get(&(provider.to_string(), instrument_id.to_string()))
            && let (Ok(instrument_id), Ok(price_scale), Ok(quantity_scale)) = (
                aeris_instruments::InstrumentId::try_new(instrument_id),
                u8::try_from(instrument.price_scale),
                u8::try_from(instrument.quantity_scale),
            )
        {
            let key = super::InstrumentStreamKey {
                provider: provider.to_string(),
                instrument: instrument_id.clone(),
                entitlement: Some(entitlement_id.to_string()),
            };
            if self
                .engine
                .instrument_subscriptions()
                .any(|(leased, _)| leased == key)
                || (self.trading_feed.attached()
                    && self
                        .engine
                        .subscriptions()
                        .into_iter()
                        .any(|(series, status)| {
                            status.streams.contains(MarketStream::Quotes)
                                && series.provider_id == provider
                                && series.instrument_id == instrument_id.as_str()
                                && series.entitlement_id == entitlement_id
                        }))
            {
                let route = aeris_trading::market::MarketRoute {
                    provider_id: provider.to_string(),
                    instrument_id,
                    provider_symbol: instrument.provider_symbol.clone(),
                    display_symbol: instrument.display_symbol.clone(),
                    venue_id: instrument.venue_id.clone(),
                    entitlement_id: Some(instrument.entitlement_id.clone()),
                    price_scale,
                    quantity_scale,
                };
                let received = self
                    .clock
                    .wall_nanos()
                    .unwrap_or(quote.metadata.timestamps.received_unix_nanos);
                let phase = self
                    .calendar
                    .phase(&instrument.instrument_id, received.div_euclid(1_000_000));
                let (changed_key, current) = {
                    let mut quotes = self
                        .quotes
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let changed_key = quotes
                        .latest(&key)
                        .is_none_or(|old| old.phase != phase)
                        .then(|| key.clone());
                    (
                        changed_key,
                        quotes.update(key, &route, quote, received, phase),
                    )
                };
                if let Some(state) = current
                    && self.trading_feed.attached()
                {
                    if let Some(key) = changed_key {
                        self.publish_trading_session(&key);
                    }
                    self.trading_feed
                        .publish(aeris_trading::market::MarketFeedEvent::Quote(state.quote));
                }
            }
        }
    }

    pub(super) fn recover_stalled_live_series(&mut self) {
        self.refresh_candle_session_phases();
        let now = self.clock.monotonic();
        let trade_candidates = self
            .series
            .trade_lifecycle_iter_mut()
            .filter_map(|(series, live)| {
                let model = live.trade_model()?;
                (live.connected
                    && live.history_ready()
                    && now.duration_since(model.last_publication_at)
                        >= super::watchdog::OPEN_STALL_THRESHOLD)
                    .then(|| {
                        (
                            series.clone(),
                            live.generation,
                            model.last_publication_at,
                            false,
                        )
                    })
            })
            .collect::<Vec<_>>();
        let candidates = trade_candidates
            .into_iter()
            .chain(
                self.series
                    .candle_iter()
                    .filter(|(_, live)| {
                        live.connected
                            && live.history_ready()
                            && live.last_progress().is_some_and(|last| {
                                now.duration_since(last) >= super::watchdog::OPEN_STALL_THRESHOLD
                            })
                    })
                    .map(|(series, live)| {
                        (
                            series.clone(),
                            live.generation,
                            live.last_progress().expect("filtered live state"),
                            true,
                        )
                    }),
            )
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return;
        }
        let at_utc_ms = self
            .clock
            .wall_nanos()
            .map(|nanos| nanos.div_euclid(1_000_000));
        for (series, generation, last_progress, candle) in candidates {
            let provider_health = self
                .engine
                .provider_status(&series.provider_id)
                .filter(|status| status.generation == Some(generation))
                .map_or(ProviderHealth::Disconnected, |status| status.health);
            let phase = at_utc_ms.map_or(aeris_instruments::SessionPhase::Unknown, |at| {
                self.calendar.phase(&series.instrument_id, at)
            });
            if super::watchdog::stalled(now, last_progress, provider_health, phase, true, true) {
                if candle {
                    self.recover_candle_series_for_reason(
                        &series,
                        generation,
                        RecoveryReason::StalledWhileOpen,
                        "Live candle series stalled while the market was open",
                    );
                } else {
                    self.recover_lifecycle_series(
                        &series,
                        generation,
                        FailureStage::Publication,
                        "Live trade series stalled while the market was open",
                    );
                }
            }
        }
    }

    fn refresh_candle_session_phases(&mut self) {
        if let Some(at_utc_ms) = self
            .clock
            .wall_nanos()
            .map(|nanos| nanos.div_euclid(1_000_000))
        {
            let changes = self
                .series
                .candle_iter()
                .filter(|(_, live)| live.connected && live.history_ready())
                .filter_map(|(series, live)| {
                    let phase = self.calendar.phase(&series.instrument_id, at_utc_ms);
                    match (live.state(), phase) {
                        (
                            SeriesState::Live { .. },
                            aeris_instruments::SessionPhase::Closed
                            | aeris_instruments::SessionPhase::Break,
                        )
                        | (
                            SeriesState::Dormant { .. },
                            aeris_instruments::SessionPhase::Open
                            | aeris_instruments::SessionPhase::Extended
                            | aeris_instruments::SessionPhase::AlwaysOpen,
                        ) => Some((series.clone(), phase)),
                        _ => None,
                    }
                })
                .collect::<Vec<_>>();
            for (series, phase) in changes {
                self.transition_candle_series(&series, SeriesInput::PhaseChanged(phase));
            }
        }
    }

    pub(super) fn series_live_if_ready(&mut self, series: &BarSeriesKey) -> bool {
        let detached = self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation)
            .is_some_and(|generation| {
                self.provider_epochs
                    .current(&series.provider_id, generation.0.get())
                    .is_some_and(|epoch| self.history.is_detached(series, epoch))
            });
        if detached {
            return false;
        }
        let ready = self
            .series
            .trade_lifecycle(series)
            .is_some_and(|live| live.connected && live.history_ready())
            || self
                .series
                .candle(series)
                .is_some_and(|live| live.connected && live.history_ready());
        if ready {
            let presentation = self
                .series
                .candle(series)
                .map(SeriesLifecycle::presentation);
            for (consumer_id, events) in &mut self.events {
                let Some(demand) = self.engine.current_demand(*consumer_id) else {
                    continue;
                };
                let (Some(generation), Some(selected)) =
                    (demand.generation, demand.series.as_ref())
                else {
                    continue;
                };
                if selected != series {
                    continue;
                }
                events.series_state = Some(series_state_payload(
                    *consumer_id,
                    generation,
                    series.clone(),
                    SeriesLoadState::Live,
                    None,
                ));
                if let (Some(presentation), Some(super::MarketRuntimeEvent::SeriesState(state))) =
                    (presentation, events.series_state.as_mut())
                {
                    state.presentation = Some(presentation);
                }
            }
        }
        ready
    }

    /// Rebuilds the complete desired Hyperliquid subscription set from the
    /// authoritative engine stream union. Each provider feed follows only the
    /// stream classes that require it; retaining canonical live state never
    /// implies an unrelated upstream subscription.
    #[cfg(test)]
    pub(super) fn hyperliquid_demand(&self) -> HyperliquidDemand {
        self.provider_demand(super::HYPERLIQUID_DESCRIPTOR.id)
            .hyperliquid_wire()
    }

    fn hyperliquid_display_depth_demand(
        &self,
        demand: &ProviderDemand,
    ) -> HyperliquidDisplayDepthDemand {
        let Some(provider_generation) = self
            .engine
            .provider_status(super::HYPERLIQUID_DESCRIPTOR.id)
            .and_then(|status| status.generation)
        else {
            return HyperliquidDisplayDepthDemand::default();
        };
        let mut books = BTreeSet::new();
        for requested in demand
            .instruments
            .iter()
            .filter(|requested| requested.display_depth)
        {
            let Some(book) = self.order_books.get(&(
                super::HYPERLIQUID_DESCRIPTOR.id.to_string(),
                requested.instrument.instrument_id.clone(),
            )) else {
                continue;
            };
            let (Ok(price_scale), Ok(quantity_scale)) = (
                u8::try_from(book.instrument.price_scale),
                u8::try_from(book.instrument.quantity_scale),
            ) else {
                continue;
            };
            books.insert(HyperliquidDisplayBookDemand {
                instrument: HyperliquidInstrumentDemand {
                    wire_coin: book.instrument.provider_symbol.clone(),
                    instrument_id: book.instrument.instrument_id.clone(),
                    entitlement_id: book.instrument.entitlement_id.clone(),
                    price_scale,
                    quantity_scale,
                },
                provider_generation: provider_generation.0.get(),
            });
        }
        HyperliquidDisplayDepthDemand {
            books: books.into_iter().collect(),
        }
    }

    /// Hands the worker the rebuilt subscription set. The worker diffs it
    /// against live subscriptions, so presentation changes never reconnect.
    pub(super) fn flush_hyperliquid_demand(&mut self) {
        if !self
            .provider_registry
            .has_control(super::HYPERLIQUID_DESCRIPTOR.id)
        {
            return;
        }
        if !self.session(super::HYPERLIQUID_DESCRIPTOR.id).demand_dirty {
            return;
        }
        let neutral = self.provider_demand(super::HYPERLIQUID_DESCRIPTOR.id);
        let demand = neutral.hyperliquid_wire();
        let display_demand = self.hyperliquid_display_depth_demand(&neutral);
        let empty = demand.candles.is_empty()
            && demand.trades.is_empty()
            && demand.quotes.is_empty()
            && demand.books.is_empty();
        if empty && !self.session(super::HYPERLIQUID_DESCRIPTOR.id).engaged {
            self.session_mut(super::HYPERLIQUID_DESCRIPTOR.id)
                .demand_dirty = false;
            return;
        }
        if !self.ensure_provider_connection(super::HYPERLIQUID_DESCRIPTOR.id) {
            if self
                .provider_registry
                .is_failed(super::HYPERLIQUID_DESCRIPTOR.id)
            {
                // A terminal provider session can never deliver this demand,
                // and every selection has already published its rejection.
                // Clearing the dirty flag stops the coordinator from
                // re-flushing the same undeliverable demand every turn.
                let session = self.session_mut(super::HYPERLIQUID_DESCRIPTOR.id);
                session.demand_dirty = false;
                session.engaged = false;
            }
            return;
        }
        let raw = self
            .provider_registry
            .apply_demand(super::HYPERLIQUID_DESCRIPTOR.id, &neutral)
            .map_err(|_| "Provider live worker is unavailable".to_string());
        let display = self.providers.send_hyperliquid_display_depth(
            HyperliquidDisplayDepthControl::Subscribe(display_demand),
        );
        match (raw, display) {
            (Ok(true), Ok(true)) => {
                self.session_mut(super::HYPERLIQUID_DESCRIPTOR.id)
                    .demand_dirty = false;
                self.session_mut(super::HYPERLIQUID_DESCRIPTOR.id).engaged = !empty;
            }
            // A full channel retries at the explicit control deadline; the
            // worker coalesces to the newest set.
            (Ok(false), _) | (_, Ok(false)) => {}
            (Err(_), _) | (_, Err(_)) => {
                self.session_mut(super::HYPERLIQUID_DESCRIPTOR.id)
                    .demand_dirty = false;
            }
        }
    }

    pub(super) fn remove_waiter(&mut self, consumer_id: ConsumerId) {
        for waiters in self.pending.values_mut() {
            waiters.retain(|waiter| waiter.consumer_id != consumer_id);
        }
        let unobserved = self
            .pending
            .iter()
            .filter(|(_, waiters)| waiters.is_empty())
            .map(|(series, _)| series.clone())
            .collect::<Vec<_>>();
        self.pending.retain(|_, waiters| !waiters.is_empty());
        for series in unobserved {
            self.history.cancel_series(&series);
        }
    }

    pub(super) fn prune_unused_live_series(&mut self) {
        let retired_trade_providers = self
            .series
            .trade_keys()
            .filter(|series| !self.engine.has_subscription(series))
            .map(|series| series.provider_id.clone())
            .collect::<BTreeSet<_>>();
        for series in self
            .series
            .trade_keys()
            .filter(|series| !self.engine.has_subscription(series))
        {
            self.history.cancel_series(series);
        }
        self.series
            .retain_trades(|series, _| self.engine.has_subscription(series));
        for provider in retired_trade_providers {
            if self
                .providers
                .descriptor(&provider)
                .is_some_and(|descriptor| {
                    descriptor.capabilities.trade_demand_policy
                        == super::TradeDemandPolicy::Immediate
                })
            {
                let _ = self.send_rithmic_demand();
            }
        }
        let retired_candle_providers = self
            .series
            .candle_keys()
            .filter(|series| !self.engine.has_subscription(series))
            .map(|series| series.provider_id.clone())
            .collect::<BTreeSet<_>>();
        for series in self
            .series
            .candle_keys()
            .filter(|series| !self.engine.has_subscription(series))
        {
            self.history.cancel_series(series);
        }
        self.series
            .retain_candles(|series, _| self.engine.has_subscription(series));
        for provider in retired_candle_providers {
            self.session_mut(&provider).demand_dirty = true;
        }
    }

    pub(super) fn release_unused_live_market_data(&mut self) {
        self.prune_history_tracking();
        self.prune_unused_live_series();
        self.evict_unreferenced_series();
        self.release_idle();
    }
    /// Hands the worker a replacement subscription set the control channel refused.
    pub(super) fn flush_rithmic_demand(&mut self) {
        if !self
            .provider_registry
            .has_control(super::RITHMIC_DESCRIPTOR.id)
        {
            return;
        }
        let Some(neutral) = self
            .session_mut(super::RITHMIC_DESCRIPTOR.id)
            .pending
            .take()
        else {
            return;
        };
        match self
            .provider_registry
            .apply_demand(super::RITHMIC_DESCRIPTOR.id, &neutral)
        {
            Ok(true) => {
                self.session_mut(super::RITHMIC_DESCRIPTOR.id).accepted = Some(neutral);
            }
            Ok(false) | Err(_) => {
                self.session_mut(super::RITHMIC_DESCRIPTOR.id).pending = Some(neutral);
            }
        }
    }
    pub(super) fn mark_alert_demand_changed(&mut self) {
        let providers = self
            .providers
            .records
            .values()
            .map(|record| record.descriptor.id)
            .collect::<Vec<_>>();
        for provider in providers {
            self.mark_provider_alert_demand_changed(provider);
        }
    }

    fn mark_provider_alert_demand_changed(&mut self, provider: &str) {
        if let Some(descriptor) = self.providers.descriptor(provider) {
            match descriptor.capabilities.alert_demand_update {
                super::AlertDemandUpdate::Immediate => {
                    let _ = self.send_rithmic_demand();
                }
                super::AlertDemandUpdate::MarkDirty => {
                    self.session_mut(provider).demand_dirty = true;
                }
            }
        }
    }

    pub(super) fn has_provider_live_demand(&self, provider: &str) -> bool {
        self.series
            .trade_keys()
            .chain(self.series.candle_keys())
            .any(|series| series.provider_id == provider)
            || self.order_books.keys().any(|(id, _)| id == provider)
            || !self.provider_demand(provider).instruments.is_empty()
    }

    pub(super) fn release_idle(&mut self) {
        let descriptors = self
            .providers
            .records
            .values()
            .map(|record| record.descriptor)
            .collect::<Vec<_>>();
        for descriptor in descriptors {
            let provider = descriptor.id;
            // Ask for this provider's full demand, including depth and quote-only
            // subscriptions. Another provider's active book must not keep it alive.
            let demanded = self.has_provider_live_demand(provider);
            let session = self.session(provider);
            let engaged = match descriptor.capabilities.live_model {
                super::LiveModel::TradeBuilt => session.accepted.is_some(),
                super::LiveModel::ProviderCandles => session.engaged,
            };
            if demanded || !engaged || session.stop_pending.is_some() {
                continue;
            }
            let Some(generation) = self
                .engine
                .provider_status(provider)
                .and_then(|status| status.generation)
            else {
                continue;
            };
            let stopped = self
                .provider_registry
                .release_idle(provider)
                .map_err(|_| "Provider live worker is unavailable".to_string());
            match stopped {
                Ok(true) => self.session_mut(provider).stop_pending = Some(generation),
                Ok(false) => self.control_retry = true,
                Err(_) => {
                    let session = self.session_mut(provider);
                    match descriptor.capabilities.live_model {
                        super::LiveModel::TradeBuilt => {
                            session.accepted = None;
                            session.pending = None;
                        }
                        super::LiveModel::ProviderCandles => {
                            session.engaged = false;
                            session.demand_dirty = false;
                        }
                    }
                    let _ = self.engine.end_provider_session(provider, generation);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_market_data::{AggressorTradeVolumes, DepthLevel, EventMetadata, QualifiedTimestamp};
    use aeris_market_engine::StreamRequirements;
    use std::num::NonZeroU64;

    fn generation() -> ProviderGeneration {
        ProviderGeneration(NonZeroU64::new(1).expect("generation"))
    }

    fn bar(sequence: u64, open_nanos: i64, close: i64) -> MarketBar {
        MarketBar {
            source_sequence: sequence,
            exchange_timestamp_seconds: open_nanos.div_euclid(1_000_000_000),
            exchange_timestamp_unix_nanos: open_nanos,
            open: 10_000,
            high: close.max(10_000),
            low: close.min(10_000),
            close,
            volume: 1,
        }
    }

    fn candle(
        open_nanos: i64,
        open: i64,
        high: i64,
        low: i64,
        close: i64,
        volume: i64,
    ) -> MarketBar {
        MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: open_nanos.div_euclid(1_000_000_000),
            exchange_timestamp_unix_nanos: open_nanos,
            open,
            high,
            low,
            close,
            volume,
        }
    }

    #[test]
    fn provider_live_candle_replacement_rolls_forming_once() {
        let minute = 60_000_000_000;
        let mut closed = vec![bar(1, minute, 10_100)];
        let first = bar(2, 2 * minute, 10_200);
        let mut forming = Some(first);
        assert!(!merge_live_candle(&mut closed, &mut forming, first).expect("duplicate"));
        let next = bar(3, 3 * minute, 10_300);
        assert!(merge_live_candle(&mut closed, &mut forming, next).expect("rollover"));
        assert_eq!(closed, vec![bar(1, minute, 10_100), first]);
        assert_eq!(forming, Some(next));
        assert!(merge_live_candle(&mut closed, &mut forming, first).is_err());
    }

    fn rithmic_tick_series() -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
            entitlement_id: "rithmic-test:CME:MNQ".to_string(),
            period: BarPeriod::tick(2).expect("tick period"),
            definition_version: 1,
        }
    }

    #[test]
    fn nonadjacent_rithmic_ingestion_ordinals_are_not_sequence_gaps() {
        let mut series = rithmic_tick_series();
        series.period = BarPeriod::time(60).expect("minute");
        let mut live = TradeModel::new(&series, generation(), "CME").expect("handoff");
        live.seed(
            2,
            0,
            &[bar(1, 60_000_000_000, 10_000)],
            None,
            Some(60_000_000_000),
        )
        .expect("seed");
        live.accept_trade(&tick_trade(2, 120_000_000_000, 10_100))
            .expect("first trade");
        live.accept_trade(&tick_trade(7, 180_000_000_000, 10_200))
            .expect("non-trade events can occupy intervening ordinals");
        assert_eq!(live.bars.len(), 3);
        assert_eq!(live.bars[2].close, 10_200);
    }

    #[test]
    fn session_live_bars_follow_cme_provider_opens_across_dst() {
        use chrono::{DateTime, Utc};
        let at = |text: &str| {
            text.parse::<DateTime<Utc>>()
                .expect("fixture instant")
                .timestamp_nanos_opt()
                .expect("fixture range")
        };
        for (old_open, next_open, before, after) in [
            (
                "2026-02-09T23:00:00Z",
                "2026-02-10T23:00:00Z",
                "2026-02-10T22:59:00Z",
                "2026-02-10T23:01:00Z",
            ),
            (
                "2026-03-09T22:00:00Z",
                "2026-03-10T22:00:00Z",
                "2026-03-10T21:59:00Z",
                "2026-03-10T22:01:00Z",
            ),
        ] {
            let mut series = rithmic_tick_series();
            series.period = BarPeriod::session(1).expect("daily");
            let mut live = TradeModel::new(&series, generation(), "CME").expect("daily handoff");
            live.seed(2, 0, &[bar(10, at(old_open), 10_000)], None, Some(0))
                .expect("provider daily bar");
            live.connected = true;
            live.accept_trade(&tick_trade(1, at(before), 10_100))
                .expect("old session");
            live.accept_trade(&tick_trade(2, at(after), 10_200))
                .expect("new session");
            assert_eq!(live.bars.len(), 2);
            assert_eq!(
                live.bars[0].close, 10_100,
                "the pre-roll trade updates the prior daily bar"
            );
            assert_eq!(live.bars[0].exchange_timestamp_unix_nanos, at(old_open));
            assert_eq!(live.bars[1].exchange_timestamp_unix_nanos, at(next_open));
        }
    }

    #[test]
    fn session_day3_closes_only_on_a_calendar_roll() {
        use chrono::{DateTime, Utc};
        let at = |text: &str| {
            text.parse::<DateTime<Utc>>()
                .expect("instant")
                .timestamp_nanos_opt()
                .expect("range")
        };
        let mut series = rithmic_tick_series();
        series.period = BarPeriod::session(3).expect("three sessions");
        let mut live = TradeModel::new(&series, generation(), "CME").expect("handoff");
        live.seed(
            2,
            0,
            &[bar(10, at("2026-03-09T22:00:00Z"), 10_000)],
            None,
            Some(0),
        )
        .expect("seed");
        live.connected = true;
        live.accept_trade(&tick_trade(1, at("2026-03-10T21:59:00Z"), 10_100))
            .expect("before roll");
        live.accept_trade(&tick_trade(2, at("2026-03-10T22:01:00Z"), 10_200))
            .expect("roll");
        assert_eq!(
            live.bars.len(),
            2,
            "three-session boundary follows trading dates"
        );
    }

    #[test]
    fn midnight_utc_equity_bars_take_their_trading_date_trades() {
        use super::super::calendar::{
            CalendarBarStamp, CalendarSource, ExchangeRule, MarketCalendarService,
        };
        use chrono::{DateTime, Utc};
        let at = |text: &str| {
            text.parse::<DateTime<Utc>>()
                .expect("instant")
                .timestamp_nanos_opt()
                .expect("range")
        };
        for (period, stamp, during, next, next_stamp) in [
            (
                BarPeriod::session(1).expect("daily"),
                "2026-05-14T00:00:00Z",
                "2026-05-14T15:00:00Z",
                "2026-05-15T14:00:00Z",
                "2026-05-15T00:00:00Z",
            ),
            (
                BarPeriod::week(1).expect("week"),
                "2026-05-11T00:00:00Z",
                "2026-05-14T15:00:00Z",
                "2026-05-18T14:00:00Z",
                "2026-05-18T00:00:00Z",
            ),
        ] {
            let mut series = rithmic_tick_series();
            series.period = period;
            let mut calendar = MarketCalendarService::default();
            calendar.insert(
                series.instrument_id.clone(),
                CalendarSource::ExchangeRule(ExchangeRule::UsEquity),
            );
            let mut live = TradeModel::with_calendar(
                &series,
                generation(),
                std::sync::Arc::new(super::super::clock::SystemClock),
                calendar,
                CalendarBarStamp::TradingDateMidnightUtc,
            )
            .expect("calendar handoff");
            live.seed(2, 0, &[bar(10, at(stamp), 10_000)], None, Some(0))
                .expect("seed");
            live.connected = true;
            live.accept_trade(&tick_trade(1, at(during), 10_100))
                .expect("same period");
            assert_eq!(
                live.bars.len(),
                1,
                "{period:?}: a trade updates its trading date's bar"
            );
            assert_eq!(live.bars[0].close, 10_100);
            assert_eq!(live.bars[0].exchange_timestamp_unix_nanos, at(stamp));
            live.accept_trade(&tick_trade(2, at(next), 10_200))
                .expect("next period");
            assert_eq!(live.bars.len(), 2, "{period:?}: next period");
            assert_eq!(live.bars[1].exchange_timestamp_unix_nanos, at(next_stamp));
        }
    }

    #[test]
    fn six_non_cme_week_and_month_live_series_follow_calendar() {
        use super::super::calendar::{CalendarSource, ExchangeRule};
        use aeris_contracts::ProviderSessionHours;
        for (label, source, weekly_before, weekly_after, monthly_before, monthly_after) in [
            (
                "contract-hours-fx",
                CalendarSource::ContractHours(
                    (1..=7)
                        .map(|weekday| ProviderSessionHours {
                            weekday,
                            open_seconds: 0,
                            close_seconds: 86_400,
                            timezone: "America/New_York".into(),
                        })
                        .collect(),
                ),
                "2026-08-28T18:00:00Z",
                "2026-08-31T18:00:00Z",
                "2026-08-31T18:00:00Z",
                "2026-09-01T18:00:00Z",
            ),
            (
                "us-equity",
                CalendarSource::ExchangeRule(ExchangeRule::UsEquity),
                "2026-08-28T15:00:00Z",
                "2026-08-31T15:00:00Z",
                "2026-08-31T15:00:00Z",
                "2026-09-01T15:00:00Z",
            ),
            (
                "cfe",
                CalendarSource::ExchangeRule(ExchangeRule::Cfe),
                "2026-08-28T19:00:00Z",
                "2026-08-30T23:00:00Z",
                "2026-08-31T19:00:00Z",
                "2026-09-01T19:00:00Z",
            ),
        ] {
            for (period, before, after) in [
                (
                    BarPeriod::week(1).expect("week"),
                    weekly_before,
                    weekly_after,
                ),
                (
                    BarPeriod::month(1).expect("month"),
                    monthly_before,
                    monthly_after,
                ),
            ] {
                assert_calendar_live_roll(label, source.clone(), period, before, after);
            }
        }
    }

    fn assert_calendar_live_roll(
        label: &str,
        source: super::super::calendar::CalendarSource,
        period: BarPeriod,
        before: &str,
        after: &str,
    ) {
        use super::super::calendar::MarketCalendarService;
        use chrono::{DateTime, Utc};
        let at = |text: &str| {
            text.parse::<DateTime<Utc>>()
                .expect("instant")
                .timestamp_nanos_opt()
                .expect("range")
        };
        let mut calendar = MarketCalendarService::default();
        calendar.insert(label.into(), source);
        let mut series = rithmic_tick_series();
        series.instrument_id = label.into();
        series.period = period;
        let mut live = TradeModel::with_calendar(
            &series,
            generation(),
            std::sync::Arc::new(super::super::clock::SystemClock),
            calendar,
            super::super::calendar::CalendarBarStamp::SessionOpen,
        )
        .expect("calendar handoff");
        live.seed(
            2,
            0,
            &[bar(10, at(before) - 3_600_000_000_000, 10_000)],
            None,
            Some(0),
        )
        .expect("seed");
        live.connected = true;
        let mut first = tick_trade(1, at(before), 10_100);
        first.metadata.instrument_id = label.into();
        let mut second = tick_trade(2, at(after), 10_200);
        second.metadata.instrument_id = label.into();
        live.accept_trade(&first).expect("forming update");
        assert_eq!(live.bars.len(), 1, "{label}: forming {period:?}");
        live.accept_trade(&second).expect("calendar roll");
        assert_eq!(live.bars.len(), 2, "{label}: boundary {period:?}");
        let mut book = ProviderOrderBook::new(
            InstallProviderInstrument {
                provider: series.provider_id.clone(),
                instrument_id: label.into(),
                entitlement_id: series.entitlement_id.clone(),
                session_generation: 1,
                ..Default::default()
            },
            true,
        );
        book.calendar = live.calendar.for_instrument(label);
        assert!(book.accept_recent_trade(&first));
        assert!(
            book.recent_trades
                .back()
                .and_then(|trade| trade.trading_day)
                .is_some(),
            "{label}: calendar supplies tape trading day"
        );
    }

    fn tick_trade(sequence: u64, timestamp: i64, price: i64) -> MarketTrade {
        MarketTrade {
            metadata: aeris_market_data::EventMetadata {
                provider_id: "rithmic".to_string(),
                instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
                entitlement_id: "rithmic-test:CME:MNQ".to_string(),
                source_sequence: sequence,
                session_generation: 1,
                timestamps: aeris_market_data::QualifiedTimestamp {
                    exchange_unix_nanos: Some(timestamp),
                    provider_unix_nanos: None,
                    received_unix_nanos: timestamp,
                },
            },
            trade_id: format!("trade-{sequence}"),
            price,
            quantity: 1,
            aggressor: aeris_market_data::AggressorSide::Unknown,
        }
    }

    fn ladder_instrument(session_generation: u64) -> InstallProviderInstrument {
        InstallProviderInstrument {
            provider: "rithmic".to_string(),
            session_generation,
            selection_generation: 1,
            instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
            provider_symbol: "MNQ".to_string(),
            display_symbol: "MNQ".to_string(),
            venue_id: "CME".to_string(),
            price_scale: 2,
            quantity_scale: 0,
            entitlement_id: "rithmic-test:CME:MNQ".to_string(),
            price_increment: Some(25),
            ..Default::default()
        }
    }

    #[test]
    fn recent_trades_expire_by_time_and_release_ladder_volume() {
        let mut book = ProviderOrderBook::new(ladder_instrument(1), true);
        let trade = ladder_trade(1, 1, 1_000_000_000_000, 20_000, 4, AggressorSide::Buy);
        assert!(book.accept_recent_trade(&trade));
        assert_eq!(book.recent_trades.len(), 1);
        assert!(book.prune_recent_trades(1_000_000_000_000 + RECENT_TRADE_RETENTION_NANOS + 1));
        assert!(book.recent_trades.is_empty());
        assert!(book.traded_volumes.is_empty());
    }

    fn ladder_trade(
        session_generation: u64,
        sequence: u64,
        received_unix_nanos: i64,
        price: i64,
        quantity: i64,
        aggressor: AggressorSide,
    ) -> MarketTrade {
        MarketTrade {
            metadata: aeris_market_data::EventMetadata {
                provider_id: "rithmic".to_string(),
                instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
                entitlement_id: "rithmic-test:CME:MNQ".to_string(),
                source_sequence: sequence,
                session_generation,
                timestamps: aeris_market_data::QualifiedTimestamp {
                    exchange_unix_nanos: Some(received_unix_nanos),
                    provider_unix_nanos: None,
                    received_unix_nanos,
                },
            },
            trade_id: format!("ladder-{session_generation}-{sequence}"),
            price,
            quantity,
            aggressor,
        }
    }

    fn assert_unknown_trade_is_exactly_retained(book: &ProviderOrderBook) {
        assert_eq!(book.recent_trades.len(), 3);
        let retained = book.recent_trades.back().expect("unknown trade retained");
        assert_eq!(retained.ingestion_ordinal, 3);
        assert_eq!(retained.trade.trade_id, "ladder-1-3");
        assert_eq!(retained.trade.metadata.source_sequence, 3);
        assert_eq!(retained.trade.aggressor, AggressorSide::Unknown);
    }

    fn ladder_metadata(session_generation: u64, source_sequence: u64) -> EventMetadata {
        EventMetadata {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
            entitlement_id: "rithmic-test:CME:MNQ".to_string(),
            source_sequence,
            session_generation,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(i64::try_from(source_sequence).expect("small sequence")),
                provider_unix_nanos: None,
                received_unix_nanos: i64::try_from(source_sequence).expect("small sequence"),
            },
        }
    }

    fn ladder_quote(session_generation: u64, source_sequence: u64) -> TopOfBookQuote {
        TopOfBookQuote {
            metadata: ladder_metadata(session_generation, source_sequence),
            bid: Some(aeris_market_data::QuoteLevel {
                price: 20_000,
                quantity: Some(2),
                order_count: None,
            }),
            ask: Some(aeris_market_data::QuoteLevel {
                price: 20_025,
                quantity: Some(3),
                order_count: None,
            }),
        }
    }

    fn ladder_depth(session_generation: u64, source_sequence: u64) -> DepthSnapshot {
        DepthSnapshot {
            metadata: ladder_metadata(session_generation, source_sequence),
            bids: vec![DepthLevel {
                price: 20_000,
                quantity: 4,
                order_count: Some(1),
            }],
            asks: vec![DepthLevel {
                price: 20_025,
                quantity: 5,
                order_count: Some(1),
            }],
        }
    }

    fn ladder_study_input() -> StudyMarketInput {
        StudyMarketInput {
            series: BarSeriesKey {
                provider_id: "rithmic".to_string(),
                instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
                entitlement_id: "rithmic-test:CME:MNQ".to_string(),
                period: BarPeriod::time(60).expect("minute period"),
                definition_version: 1,
            },
            streams: StreamRequirements::BARS
                .with(MarketStream::Trades)
                .with(MarketStream::Quotes)
                .with(MarketStream::Depth),
        }
    }

    #[test]
    fn study_live_market_views_are_generation_fenced_and_invalidated_for_recovery() {
        let mut book = ProviderOrderBook::new(ladder_instrument(1), false);
        assert!(book.install_top_of_book(&ladder_quote(1, 1)));
        assert!(book.accept_recent_trade(&ladder_trade(1, 2, 2, 20_000, 3, AggressorSide::Buy,)));
        assert!(matches!(
            book.book.install_snapshot(&ladder_depth(1, 3)),
            Ok(OrderBookApplyOutcome::Published)
        ));
        let input = ladder_study_input();
        let current = book
            .study_live_market_data(&input, 1)
            .expect("matching instrument exposes current live state");
        assert!(current.quote().is_some());
        assert!(current.trades().is_some_and(|trades| !trades.is_empty()));
        assert!(current.depth().is_some());

        let newer_generation = book
            .study_live_market_data(&input, 2)
            .expect("instrument identity still matches");
        assert!(newer_generation.quote().is_none());
        assert!(newer_generation.trades().is_none());
        assert!(newer_generation.depth().is_none());

        book.update_instrument(ladder_instrument(2));
        let replaced_session = book
            .study_live_market_data(&input, 2)
            .expect("replacement session keeps the canonical instrument identity");
        assert!(replaced_session.quote().is_none());
        assert!(
            replaced_session
                .trades()
                .is_some_and(StudyTradeWindow::is_empty)
        );
        assert!(replaced_session.depth().is_none());

        assert!(book.install_top_of_book(&ladder_quote(2, 1)));
        assert!(book.accept_recent_trade(&ladder_trade(2, 2, 4, 20_025, 7, AggressorSide::Sell,)));
        assert!(matches!(
            book.book.install_snapshot(&ladder_depth(2, 3)),
            Ok(OrderBookApplyOutcome::Published)
        ));
        let restored = book
            .study_live_market_data(&input, 2)
            .expect("new generation exposes fresh live state");
        assert!(restored.quote().is_some());
        assert!(restored.trades().is_some_and(|trades| !trades.is_empty()));
        assert!(restored.depth().is_some());

        assert!(book.invalidate_live_market(2));
        let recovering = book
            .study_live_market_data(&input, 2)
            .expect("recovery keeps the canonical instrument identity");
        assert!(recovering.quote().is_none());
        assert!(recovering.trades().is_some_and(StudyTradeWindow::is_empty));
        assert!(recovering.depth().is_none());
    }

    #[test]
    fn recent_ladder_trades_are_side_aware_deduped_retained_and_session_fenced() {
        let mut book = ProviderOrderBook::new(ladder_instrument(1), false);
        let start = 1_000_000_000_i64;
        assert!(book.accept_recent_trade(&ladder_trade(
            1,
            1,
            start,
            20_000,
            4,
            AggressorSide::Buy,
        )));
        assert!(book.accept_recent_trade(&ladder_trade(
            1,
            2,
            start + 1,
            20_000,
            3,
            AggressorSide::Sell,
        )));
        assert_eq!(
            book.traded_volumes.get(&20_000).copied(),
            Some(AggressorTradeVolumes { buy: 4, sell: 3 })
        );
        assert!(!book.accept_recent_trade(&ladder_trade(
            1,
            2,
            start + 2,
            20_000,
            99,
            AggressorSide::Buy,
        )));
        assert!(book.accept_recent_trade(&ladder_trade(
            1,
            3,
            start + 3,
            20_000,
            99,
            AggressorSide::Unknown,
        )));
        assert!(!book.accept_recent_trade(&ladder_trade(
            1,
            2,
            start + 4,
            20_000,
            99,
            AggressorSide::Sell,
        )));
        assert_eq!(
            book.traded_volumes.get(&20_000).copied(),
            Some(AggressorTradeVolumes { buy: 4, sell: 3 })
        );
        assert_unknown_trade_is_exactly_retained(&book);

        assert!(
            book.prune_recent_trades(
                start
                    .saturating_add(RECENT_TRADE_RETENTION_NANOS)
                    .saturating_add(1)
            )
        );
        assert_eq!(
            book.traded_volumes.get(&20_000).copied(),
            Some(AggressorTradeVolumes { buy: 0, sell: 3 })
        );

        assert!(
            book.accept_recent_trade(&ladder_trade(
                2,
                1,
                start
                    .saturating_add(RECENT_TRADE_RETENTION_NANOS)
                    .saturating_add(2),
                20_025,
                7,
                AggressorSide::Buy,
            ))
        );
        assert_eq!(book.trade_session_generation, 2);
        assert_eq!(book.last_trade_source_sequence, 1);
        assert_eq!(book.recent_trades.len(), 1);
        assert_eq!(book.recent_trades[0].ingestion_ordinal, 1);
        assert_eq!(book.recent_trades[0].trade.trade_id, "ladder-2-1");
        assert_eq!(book.traded_volumes.len(), 1);
        assert_eq!(
            book.traded_volumes.get(&20_025).copied(),
            Some(AggressorTradeVolumes { buy: 7, sell: 0 })
        );

        assert!(
            !book.accept_recent_trade(&ladder_trade(
                2,
                1,
                start
                    .saturating_add(RECENT_TRADE_RETENTION_NANOS)
                    .saturating_add(3),
                20_025,
                7,
                AggressorSide::Buy,
            ))
        );
    }

    #[test]
    fn retained_trades_carry_the_venue_trading_day() {
        // Sunday 2026-08-16 (Unix day 20,681) 17:00 CDT opens Monday's CME trading date.
        let session_open = (20_681 * 86_400 + 22 * 3_600) * 1_000_000_000;
        let mut book = ProviderOrderBook::new(ladder_instrument(1), false);
        for (sequence, received) in [(1, session_open - 1), (2, session_open)] {
            assert!(book.accept_recent_trade(&ladder_trade(
                1,
                sequence,
                received,
                20_000,
                1,
                AggressorSide::Buy,
            )));
        }
        let days = book
            .recent_trades
            .iter()
            .map(|trade| trade.trading_day)
            .collect::<Vec<_>>();
        assert_eq!(days, [Some(20_681), Some(20_682)]);

        let mut unknown_venue = ladder_instrument(1);
        unknown_venue.venue_id = "UNKNOWN".to_string();
        let mut book = ProviderOrderBook::new(unknown_venue, false);
        assert!(book.accept_recent_trade(&ladder_trade(
            1,
            1,
            session_open,
            20_000,
            1,
            AggressorSide::Buy,
        )));
        assert_eq!(book.recent_trades[0].trading_day, None);
    }

    #[test]
    fn retained_trade_tape_enforces_its_hard_item_bound() {
        let mut book = ProviderOrderBook::new(ladder_instrument(1), false);
        for sequence in
            1..=u64::try_from(MAXIMUM_RECENT_LADDER_TRADES + 2).expect("tape capacity fits u64")
        {
            assert!(book.accept_recent_trade(&ladder_trade(
                1,
                sequence,
                1_000_000_000,
                20_000,
                1,
                AggressorSide::Buy,
            )));
        }
        assert_eq!(book.recent_trades.len(), MAXIMUM_RECENT_LADDER_TRADES);
        assert_eq!(
            book.recent_trades
                .front()
                .map(|trade| trade.ingestion_ordinal),
            Some(3)
        );
        assert_eq!(
            book.recent_trades
                .back()
                .map(|trade| trade.ingestion_ordinal),
            Some(u64::try_from(MAXIMUM_RECENT_LADDER_TRADES + 2).expect("capacity fits u64"))
        );
        assert_eq!(
            book.traded_volumes.get(&20_000).map(|volumes| volumes.buy),
            Some(i64::try_from(MAXIMUM_RECENT_LADDER_TRADES).expect("capacity fits i64"))
        );
    }

    #[test]
    fn rithmic_tick_burst_keeps_completed_revision_before_next_forming_bar() {
        let series = rithmic_tick_series();
        let mut live = TradeModel::new(&series, generation(), "CME").expect("handoff");
        live.seed(2, 0, &[bar(10, 1_000_000_000, 10_000)], None, None)
            .expect("seed");
        live.connected = true;

        live.accept_trade(&tick_trade(1, 2_000_000_000, 10_100))
            .expect("first");
        live.accept_trade(&tick_trade(2, 3_000_000_000, 10_200))
            .expect("second");
        live.accept_trade(&tick_trade(3, 4_000_000_000, 10_300))
            .expect("third");

        let bars = live.take_publication().expect("burst");
        assert_eq!(
            bars.iter()
                .map(|bar| bar.source_sequence)
                .collect::<Vec<_>>(),
            vec![11, 12]
        );
        assert_eq!(bars[0].close, 10_200, "final prior tick bar is retained");
    }

    #[test]
    fn rithmic_reseed_buffers_and_replays_live_trades_without_losing_the_seam() {
        let series = rithmic_tick_series();
        let mut live = SeriesLifecycle::from_trade(
            TradeModel::new(&series, generation(), "CME").expect("handoff"),
        );
        let seed = bar(10, 1_000_000_000, 10_000);
        let seed_input = TradeHandoffSeed {
            price_scale: 2,
            quantity_scale: 0,
            bars: &[seed],
            forming: None,
            handoff_boundary_unix_nanos: Some(seed.exchange_timestamp_unix_nanos),
        };
        live.seed_trade(seed_input).expect("initial seed");
        live.mark_trade_online();
        live.begin_trade_history_reseed();

        live.accept_trade(&tick_trade(1, 2_000_000_000, 10_100))
            .expect("trade buffers while reseeding");
        live.accept_trade(&tick_trade(2, 3_000_000_000, 10_200))
            .expect("second trade buffers while reseeding");
        assert!(live.take_trade_publication().is_none());
        assert_eq!(live.trade_model().expect("model").buffered.len(), 2);
        assert_eq!(
            live.trade_model()
                .expect("model")
                .bars
                .last()
                .map(|bar| bar.close),
            Some(10_000)
        );
        assert!(matches!(
            live.state(),
            SeriesState::Recovering {
                reason: RecoveryReason::DetachedWindow,
                ..
            }
        ));

        live.seed_trade(seed_input)
            .expect("current history reseed replays buffered trades");
        let model = live.trade_model().expect("model");
        assert!(model.buffered.is_empty());
        assert!(live.history_ready());
        assert_eq!(model.bars.last().map(|bar| bar.source_sequence), Some(11));
        assert_eq!(model.bars.last().map(|bar| bar.close), Some(10_200));
    }

    #[test]
    fn watchdog_stalled_series_requires_online_open_market() {
        let now = Instant::now();
        let old_publication = now
            .checked_sub(super::super::watchdog::OPEN_STALL_THRESHOLD + Duration::from_secs(1))
            .expect("test clock can subtract 31 seconds");
        assert!(super::super::watchdog::stalled(
            now,
            old_publication,
            ProviderHealth::Online,
            aeris_instruments::SessionPhase::Open,
            true,
            true,
        ));
        for (health, phase, connected, history_ready) in [
            (
                ProviderHealth::Recovering,
                aeris_instruments::SessionPhase::Open,
                true,
                true,
            ),
            (
                ProviderHealth::Online,
                aeris_instruments::SessionPhase::Closed,
                true,
                true,
            ),
            (
                ProviderHealth::Online,
                aeris_instruments::SessionPhase::Open,
                false,
                true,
            ),
            (
                ProviderHealth::Online,
                aeris_instruments::SessionPhase::Open,
                true,
                false,
            ),
        ] {
            assert!(!super::super::watchdog::stalled(
                now,
                old_publication,
                health,
                phase,
                connected,
                history_ready,
            ));
        }
        assert!(!super::super::watchdog::stalled(
            now,
            now.checked_sub(Duration::from_secs(5)).expect("test clock"),
            ProviderHealth::Online,
            aeris_instruments::SessionPhase::Open,
            true,
            true,
        ));
    }

    #[test]
    fn watchdog_uses_scripted_clock_without_waiting() {
        use super::super::clock::{Clock, ScriptedClock};
        let clock = ScriptedClock::new(1_000_000_000);
        let published = clock.monotonic();
        clock.advance(super::super::watchdog::OPEN_STALL_THRESHOLD + Duration::from_secs(1));
        let now = clock.monotonic();
        assert!(super::super::watchdog::stalled(
            now,
            published,
            ProviderHealth::Online,
            aeris_instruments::SessionPhase::Open,
            true,
            true,
        ));
        assert_eq!(clock.wall_nanos().expect("wall clock"), 32_000_000_000);
    }

    #[test]
    fn hyperliquid_rollover_keeps_final_prior_revision_before_append() {
        let series = BarSeriesKey {
            provider_id: "hyperliquid".to_string(),
            instrument_id: "instrument:hyperliquid:BTC".to_string(),
            entitlement_id: "hyperliquid-public".to_string(),
            period: BarPeriod::time(60).expect("time period"),
            definition_version: 1,
        };
        let mut live = SeriesLifecycle::new(series, generation(), "BTC", "1m");
        live.seed(
            2,
            2,
            &[bar(10, 60_000_000_000, 10_000)],
            Some(FormingBar {
                bar: bar(11, 120_000_000_000, 10_100),
                trades: None,
            }),
        )
        .expect("seed");
        live.connected = true;
        let _ = live.take_publication().expect("seed tail");

        live.accept_bar(candle(120_000_000_000, 10_000, 10_400, 9_900, 10_250, 4))
            .expect("final revision");
        live.accept_bar(candle(180_000_000_000, 10_250, 10_500, 10_200, 10_300, 1))
            .expect("rollover");

        let bars = live.take_publication().expect("rollover tails");
        assert_eq!(
            bars.iter()
                .map(|bar| bar.source_sequence)
                .collect::<Vec<_>>(),
            vec![11, 12]
        );
        assert_eq!(bars[0].close, 10_250);
    }

    #[test]
    fn candle_series_rejects_missing_contiguous_bars() {
        let series = BarSeriesKey {
            provider_id: "provider".to_string(),
            instrument_id: "instrument:provider:TEST".to_string(),
            entitlement_id: "provider-test".to_string(),
            period: BarPeriod::time(60).expect("time period"),
            definition_version: 1,
        };
        let previous = bar(10, 60_000_000_000, 10_000);
        let next = bar(11, 180_000_000_000, 10_100);
        let mut live = SeriesLifecycle::new(series, generation(), "TEST", "1m");
        live.seed(2, 2, &[previous], None).expect("history seeds");
        assert!(live.ingest(next).is_err());
    }

    #[test]
    fn hyperliquid_reseed_buffers_and_replays_candles_without_losing_the_seam() {
        let series = BarSeriesKey {
            provider_id: "hyperliquid".to_string(),
            instrument_id: "instrument:hyperliquid:BTC".to_string(),
            entitlement_id: "hyperliquid-public".to_string(),
            period: BarPeriod::time(60).expect("time period"),
            definition_version: 1,
        };
        let mut live = SeriesLifecycle::new(series, generation(), "BTC", "1m");
        let closed = bar(10, 60_000_000_000, 10_000);
        let forming_bar = bar(11, 120_000_000_000, 10_100);
        live.seed(
            2,
            2,
            &[closed],
            Some(FormingBar {
                bar: forming_bar,
                trades: None,
            }),
        )
        .expect("initial seed");
        live.connected = true;
        live.begin_history_reseed();

        live.accept_bar(candle(120_000_000_000, 10_000, 10_400, 9_900, 10_250, 4))
            .expect("forming revision buffers");
        live.accept_bar(candle(180_000_000_000, 10_250, 10_500, 10_200, 10_300, 1))
            .expect("new candle buffers");
        assert_eq!(live.buffered.len(), 2);
        assert_eq!(live.forming.map(|forming| forming.bar.close), Some(10_100));

        live.seed(
            2,
            2,
            &[closed],
            Some(FormingBar {
                bar: forming_bar,
                trades: None,
            }),
        )
        .expect("current history reseed replays buffered candles");
        assert!(live.buffered.is_empty());
        assert!(live.history_ready());
        assert_eq!(live.bars.last().map(|bar| bar.close), Some(10_250));
        assert_eq!(live.forming.map(|forming| forming.bar.close), Some(10_300));
    }

    #[test]
    fn terminal_rithmic_failures_do_not_auto_resubscribe() {
        for reason in [
            ProviderInvalidationReason::Authentication,
            ProviderInvalidationReason::AgreementRequired,
            ProviderInvalidationReason::UnsupportedSystem,
            ProviderInvalidationReason::SchemaMismatch,
            ProviderInvalidationReason::MalformedMessage,
        ] {
            assert!(
                !super::super::provider_event::rithmic_auto_recovers(Some(reason)),
                "{reason:?}"
            );
        }
        for reason in [
            ProviderInvalidationReason::Transport,
            ProviderInvalidationReason::HeartbeatSilence,
            ProviderInvalidationReason::MessageSilence,
            ProviderInvalidationReason::SequenceGap,
            ProviderInvalidationReason::QueueOverflow,
        ] {
            assert!(
                super::super::provider_event::rithmic_auto_recovers(Some(reason)),
                "{reason:?}"
            );
        }
        assert!(super::super::provider_event::rithmic_auto_recovers(None));
    }
}
