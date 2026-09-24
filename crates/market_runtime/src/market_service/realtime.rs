use super::{
    AggressorSide, BTreeMap, BTreeSet, BarPeriod, BarSeriesKey, CanonicalOrderBookState,
    ConsumerId, Coordinator, DepthSnapshot, FailureStage, FormingBar, HyperliquidCandleDemand,
    HyperliquidDemand, HyperliquidInstrumentDemand, HyperliquidLiveCandle, HyperliquidLiveHandoff,
    HyperliquidRealtimeControl, HyperliquidRealtimeEvent, InstallProviderInstrument,
    LIVE_BUFFER_CAPACITY, LIVE_HANDOFF_HISTORY_BARS, LiveHistoryState, LiveSeriesPublication,
    MarketBar, MarketStream, MarketTrade, NonZeroUsize, OrderBook, OrderBookApplyOutcome, Ordering,
    ProviderGeneration, ProviderHealth, ProviderOrderBook, RithmicCalendarPeriod,
    RithmicExchangeCalendar, RithmicInstrumentDemand, RithmicLiveCadence, RithmicLiveHandoff,
    RithmicRealtimeControl, RithmicRealtimeDemand, RithmicRealtimeEvent, SeriesLoadState,
    StudyTradeSample, TopOfBookQuote, VecDeque, hyperliquid_interval_for_period, id,
    merge_live_candle, series_state_payload, series_update_message,
};
use crate::hyperliquid_display_depth::{
    HyperliquidDisplayBookDemand, HyperliquidDisplayDepthControl, HyperliquidDisplayDepthDemand,
    HyperliquidDisplayDepthEvent,
};
use crate::study::{
    StudyDepthView, StudyLiveMarketData, StudyMarketInput, StudyQuoteView, StudyTradeWindow,
};
use aeris_rithmic_protocol_adapter::ProviderInvalidationReason;

const MAXIMUM_CANONICAL_DEPTH_LEVELS: usize = 4_096;
const RECENT_TRADE_RETENTION_NANOS: i64 = 8 * 60 * 1_000_000_000;
const MAXIMUM_RECENT_LADDER_TRADES: usize = 65_536;

const fn rithmic_invalidation_detail(reason: Option<ProviderInvalidationReason>) -> &'static str {
    match reason {
        Some(ProviderInvalidationReason::Transport) => "Rithmic transport connection failed",
        Some(ProviderInvalidationReason::Authentication) => "Rithmic authentication was rejected",
        Some(ProviderInvalidationReason::AgreementRequired) => {
            "Rithmic account requires an agreement before market data can stream"
        }
        Some(ProviderInvalidationReason::UnsupportedSystem) => {
            "Rithmic Test system is unavailable for this account"
        }
        Some(ProviderInvalidationReason::SchemaMismatch) => {
            "Rithmic protocol schema does not match the installed provider kit"
        }
        Some(ProviderInvalidationReason::HeartbeatSilence) => {
            "Rithmic heartbeat response timed out"
        }
        Some(ProviderInvalidationReason::MessageSilence) => {
            "Rithmic market-data stream stopped producing messages"
        }
        Some(ProviderInvalidationReason::SequenceGap) => {
            "Rithmic market-data sequence continuity was lost"
        }
        Some(ProviderInvalidationReason::QueueOverflow) => {
            "Rithmic callback queue overflowed before market data could be consumed"
        }
        Some(ProviderInvalidationReason::MalformedMessage) => {
            "Rithmic sent market data that failed protocol validation"
        }
        None => "Rithmic live session is recovering",
    }
}

const fn rithmic_auto_recovers(reason: Option<ProviderInvalidationReason>) -> bool {
    matches!(
        reason,
        None | Some(
            ProviderInvalidationReason::Transport
                | ProviderInvalidationReason::HeartbeatSilence
                | ProviderInvalidationReason::MessageSilence
                | ProviderInvalidationReason::SequenceGap
                | ProviderInvalidationReason::QueueOverflow
        )
    )
}

fn enqueue_bar_transition(
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
    pub(super) fn new(instrument: InstallProviderInstrument) -> Self {
        let trade_session_generation = instrument.session_generation;
        Self {
            instrument,
            book: OrderBook::new(
                NonZeroUsize::new(MAXIMUM_CANONICAL_DEPTH_LEVELS).unwrap_or(NonZeroUsize::MIN),
            ),
            top_of_book: None,
            recent_trades: VecDeque::new(),
            traded_volumes: BTreeMap::new(),
            trade_session_generation,
            last_trade_source_sequence: 0,
            retention_clock_unix_nanos: 0,
        }
    }

    pub(super) fn update_instrument(&mut self, instrument: InstallProviderInstrument) {
        if instrument.session_generation != self.instrument.session_generation
            || instrument.entitlement_id != self.instrument.entitlement_id
        {
            self.invalidate_live_market(instrument.session_generation);
        }
        self.instrument = instrument;
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
        self.traded_volumes.clear();
        self.trade_session_generation = session_generation;
        self.last_trade_source_sequence = 0;
        self.retention_clock_unix_nanos = 0;
    }

    fn remove_recent_trade(&mut self, trade: StudyTradeSample) {
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
        self.last_trade_source_sequence = trade.metadata.source_sequence;
        let observed_unix_nanos = self
            .retention_clock_unix_nanos
            .max(trade.metadata.timestamps.received_unix_nanos);
        let mut changed = self.prune_recent_trades(observed_unix_nanos);
        let aggressor = trade.aggressor;
        if aggressor == AggressorSide::Unknown {
            return changed;
        }
        let volumes = self.traded_volumes.entry(trade.price).or_default();
        match aggressor {
            AggressorSide::Buy => volumes.buy = volumes.buy.saturating_add(trade.quantity),
            AggressorSide::Sell => volumes.sell = volumes.sell.saturating_add(trade.quantity),
            AggressorSide::Unknown => unreachable!(),
        }
        self.recent_trades.push_back(StudyTradeSample {
            observed_unix_nanos,
            price: trade.price,
            quantity: trade.quantity,
            aggressor,
        });
        changed = true;
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

impl RithmicLiveHandoff {
    pub(super) fn new(
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        venue_id: &str,
    ) -> Option<Self> {
        let cadence = match series.period {
            BarPeriod::Tick { trades } => RithmicLiveCadence::Tick {
                trades,
                forming: trades,
            },
            BarPeriod::Time { seconds } => RithmicLiveCadence::Fixed {
                seconds: i64::from(seconds),
            },
            BarPeriod::Session { days } => RithmicLiveCadence::Fixed {
                seconds: i64::from(days) * 86_400,
            },
            BarPeriod::Week { weeks: 1 } => RithmicLiveCadence::Calendar {
                calendar: RithmicExchangeCalendar::for_venue(venue_id)?,
                period: RithmicCalendarPeriod::Week,
            },
            BarPeriod::Month { months: 1 } => RithmicLiveCadence::Calendar {
                calendar: RithmicExchangeCalendar::for_venue(venue_id)?,
                period: RithmicCalendarPeriod::Month,
            },
            BarPeriod::Week { .. } | BarPeriod::Month { .. } => return None,
        };
        Some(Self {
            series: series.clone(),
            generation,
            cadence,
            price_scale: 0,
            quantity_scale: 0,
            bars: Vec::new(),
            buffered: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            pending_publications: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            connected: false,
            history_state: LiveHistoryState::AwaitingHistory,
            dirty: false,
            forming_tail_sequence: None,
            live_session_generation: None,
            last_trade_sequence: None,
            history_boundary_unix_nanos: i64::MIN,
        })
    }

    pub(super) fn reset(&mut self, generation: ProviderGeneration) {
        self.generation = generation;
        self.bars.clear();
        self.buffered.clear();
        self.pending_publications.clear();
        self.connected = false;
        self.history_state = LiveHistoryState::AwaitingHistory;
        self.dirty = false;
        self.forming_tail_sequence = None;
        self.live_session_generation = None;
        self.last_trade_sequence = None;
        self.history_boundary_unix_nanos = i64::MIN;
        if let RithmicLiveCadence::Tick { trades, forming } = &mut self.cadence {
            *forming = *trades;
        }
    }

    pub(super) fn coverage(&self) -> Option<(i64, i64)> {
        Some((
            self.bars.first()?.exchange_timestamp_unix_nanos,
            self.bars.last()?.exchange_timestamp_unix_nanos,
        ))
    }

    pub(super) fn begin_history_reseed(&mut self) {
        if self.history_state != LiveHistoryState::Ready {
            return;
        }
        self.history_state = LiveHistoryState::Reseeding;
        self.pending_publications.clear();
        self.dirty = false;
    }

    pub(super) fn cancel_history_reseed(&mut self) -> Result<(), String> {
        if self.history_state != LiveHistoryState::Reseeding {
            return Ok(());
        }
        let buffered = std::mem::take(&mut self.buffered);
        self.history_state = LiveHistoryState::Ready;
        for trade in &buffered {
            if self.apply_trade(trade)? {
                self.dirty = true;
            }
        }
        Ok(())
    }

    /// Closes the history/live seam.
    ///
    /// `bars` are the periods the provider has closed and the engine has
    /// installed as canonical history. `forming` is the period it caught open,
    /// which is held here rather than in history and published as a tail. Every
    /// trade at or before `handoff_boundary_unix_nanos` is already inside what
    /// the provider returned, so replaying it would count it twice.
    pub(super) fn seed(
        &mut self,
        price_scale: u8,
        quantity_scale: u8,
        bars: &[MarketBar],
        forming: Option<FormingBar>,
        handoff_boundary_unix_nanos: Option<i64>,
    ) -> Result<(), String> {
        self.pending_publications.clear();
        // A tick bundle whose trade count is unknown cannot be resumed: the
        // cadence would not know when it closes, so it is treated as complete.
        let forming = forming.filter(|forming| {
            let countable = forming.trades.is_some()
                || !matches!(self.cadence, RithmicLiveCadence::Tick { .. });
            countable
                && bars
                    .last()
                    .is_none_or(|last| last.source_sequence < forming.bar.source_sequence)
        });
        let start = bars.len().saturating_sub(LIVE_HANDOFF_HISTORY_BARS);
        let mut working = bars[start..].to_vec();
        if let Some(forming) = &forming {
            working.push(forming.bar);
        }
        let last_bar_boundary = working
            .last()
            .ok_or_else(|| "Rithmic live handoff requires history".to_string())?
            .exchange_timestamp_unix_nanos;
        self.price_scale = price_scale;
        self.quantity_scale = quantity_scale;
        self.forming_tail_sequence = forming.as_ref().map(|forming| forming.bar.source_sequence);
        self.bars = working;
        self.history_boundary_unix_nanos = handoff_boundary_unix_nanos
            .unwrap_or(last_bar_boundary)
            .max(last_bar_boundary);
        self.live_session_generation = None;
        self.last_trade_sequence = None;
        if let RithmicLiveCadence::Tick {
            trades,
            forming: open,
        } = &mut self.cadence
        {
            *open = forming
                .as_ref()
                .and_then(|forming| forming.trades)
                .unwrap_or(*trades);
        }
        // The open period has to reach the consumer even if no trade arrives
        // next: it is the candle the chart opens on.
        if let Some(forming) = forming.as_ref() {
            enqueue_bar_transition(
                &mut self.pending_publications,
                forming.bar,
                "Rithmic live publication buffer overflowed",
            )?;
        }
        self.dirty = !self.pending_publications.is_empty();
        let buffered = std::mem::take(&mut self.buffered);
        self.history_state = LiveHistoryState::Ready;
        for trade in &buffered {
            if self.apply_trade(trade)? {
                self.dirty = true;
            }
        }
        Ok(())
    }

    pub(super) fn take_publication(&mut self) -> Option<LiveSeriesPublication> {
        if !self.connected || self.history_state != LiveHistoryState::Ready || !self.dirty {
            return None;
        }
        let tails = self.pending_publications.drain(..).collect::<Vec<_>>();
        self.dirty = false;
        (!tails.is_empty()).then_some(LiveSeriesPublication::Tails(tails))
    }

    pub(super) fn accept_trade(&mut self, trade: &MarketTrade) -> Result<(), String> {
        if self.history_state == LiveHistoryState::Ready {
            if self.apply_trade(trade)? {
                self.dirty = true;
            }
        } else if self.buffered.len() == LIVE_BUFFER_CAPACITY {
            return Err("Rithmic history/live buffer overflowed".to_string());
        } else {
            self.buffered.push_back(trade.clone());
        }
        Ok(())
    }

    pub(super) fn apply_trade(&mut self, trade: &MarketTrade) -> Result<bool, String> {
        trade.validate().map_err(|error| error.to_string())?;
        if trade.metadata.provider_id != self.series.provider_id
            || trade.metadata.instrument_id != self.series.instrument_id
            || trade.metadata.entitlement_id != self.series.entitlement_id
        {
            return Ok(false);
        }
        if let Some(session_generation) = self.live_session_generation
            && session_generation != trade.metadata.session_generation
        {
            return Err("Rithmic live session generation changed".to_string());
        }
        if self
            .last_trade_sequence
            .is_some_and(|sequence| trade.metadata.source_sequence <= sequence)
        {
            return Ok(false);
        }
        let exchange_nanos = trade
            .metadata
            .timestamps
            .exchange_unix_nanos
            .ok_or_else(|| "Rithmic live trade has no exchange timestamp".to_string())?;
        if self.last_trade_sequence.is_none() && exchange_nanos <= self.history_boundary_unix_nanos
        {
            return Ok(false);
        }
        let Some(last) = self.bars.last().copied() else {
            return Err("Rithmic live handoff has no history".to_string());
        };
        let next = match self.cadence {
            RithmicLiveCadence::Fixed { seconds } => {
                let trade_seconds = exchange_nanos.div_euclid(1_000_000_000);
                if trade_seconds < last.exchange_timestamp_seconds {
                    return Ok(false);
                }
                let elapsed = trade_seconds - last.exchange_timestamp_seconds;
                if elapsed < seconds {
                    // A period the provider closed is never revised by a live
                    // trade; the period it caught open always is.
                    if self.last_trade_sequence.is_none()
                        && self.forming_tail_sequence != Some(last.source_sequence)
                    {
                        return Ok(false);
                    }
                    updated_rithmic_bar(last, trade, last.exchange_timestamp_unix_nanos)?
                } else {
                    let intervals = elapsed.div_euclid(seconds);
                    let start_seconds = last
                        .exchange_timestamp_seconds
                        .checked_add(intervals.saturating_mul(seconds))
                        .ok_or_else(|| "Rithmic live timestamp overflowed".to_string())?;
                    started_rithmic_bar(
                        last,
                        trade,
                        start_seconds
                            .checked_mul(1_000_000_000)
                            .ok_or_else(|| "Rithmic live timestamp overflowed".to_string())?,
                    )?
                }
            }
            RithmicLiveCadence::Tick {
                trades,
                ref mut forming,
            } => {
                if *forming >= trades {
                    *forming = 1;
                    started_rithmic_bar(last, trade, exchange_nanos)?
                } else {
                    *forming = forming.saturating_add(1);
                    updated_rithmic_bar(last, trade, exchange_nanos)?
                }
            }
            RithmicLiveCadence::Calendar { calendar, period } => {
                let last_bucket = calendar.bucket(last.exchange_timestamp_seconds, period);
                let trade_bucket =
                    calendar.bucket(exchange_nanos.div_euclid(1_000_000_000), period);
                if last_bucket == trade_bucket {
                    updated_rithmic_bar(last, trade, exchange_nanos)?
                } else {
                    started_rithmic_bar(last, trade, exchange_nanos)?
                }
            }
        };
        self.commit_trade_bar(last, next, trade)?;
        Ok(true)
    }

    fn commit_trade_bar(
        &mut self,
        last: MarketBar,
        next: MarketBar,
        trade: &MarketTrade,
    ) -> Result<(), String> {
        if next.source_sequence == last.source_sequence {
            if let Some(forming) = self.bars.last_mut() {
                *forming = next;
            }
        } else {
            self.bars.push(next);
            if self.bars.len() > LIVE_HANDOFF_HISTORY_BARS {
                let excess = self.bars.len() - LIVE_HANDOFF_HISTORY_BARS;
                self.bars.drain(..excess);
            }
        }
        self.live_session_generation = Some(trade.metadata.session_generation);
        self.last_trade_sequence = Some(trade.metadata.source_sequence);
        enqueue_bar_transition(
            &mut self.pending_publications,
            next,
            "Rithmic live publication buffer overflowed",
        )?;
        self.dirty = true;
        Ok(())
    }
}

pub(super) fn updated_rithmic_bar(
    mut bar: MarketBar,
    trade: &MarketTrade,
    _exchange_timestamp_unix_nanos: i64,
) -> Result<MarketBar, String> {
    bar.high = bar.high.max(trade.price);
    bar.low = bar.low.min(trade.price);
    bar.close = trade.price;
    bar.volume = bar
        .volume
        .checked_add(trade.quantity)
        .ok_or_else(|| "Rithmic live volume overflowed".to_string())?;
    Ok(bar)
}

pub(super) fn started_rithmic_bar(
    completed: MarketBar,
    trade: &MarketTrade,
    exchange_timestamp_unix_nanos: i64,
) -> Result<MarketBar, String> {
    Ok(MarketBar {
        source_sequence: completed
            .source_sequence
            .checked_add(1)
            .ok_or_else(|| "Rithmic live sequence overflowed".to_string())?,
        exchange_timestamp_seconds: exchange_timestamp_unix_nanos.div_euclid(1_000_000_000),
        exchange_timestamp_unix_nanos,
        open: trade.price,
        high: trade.price,
        low: trade.price,
        close: trade.price,
        volume: trade.quantity,
    })
}

impl HyperliquidLiveHandoff {
    pub(super) fn new(
        series: BarSeriesKey,
        generation: ProviderGeneration,
        wire_coin: String,
        interval: String,
    ) -> Self {
        Self {
            series,
            generation,
            wire_coin,
            interval,
            price_scale: 0,
            quantity_scale: 0,
            bars: Vec::new(),
            forming: None,
            buffered: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            pending_publications: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            connected: false,
            history_state: LiveHistoryState::AwaitingHistory,
            dirty: false,
        }
    }

    pub(super) fn reset(&mut self, generation: ProviderGeneration) {
        self.generation = generation;
        self.bars.clear();
        self.forming = None;
        self.buffered.clear();
        self.pending_publications.clear();
        self.connected = false;
        self.history_state = LiveHistoryState::AwaitingHistory;
        self.dirty = false;
    }

    pub(super) fn coverage(&self) -> Option<(i64, i64)> {
        let first = self
            .bars
            .first()
            .copied()
            .or(self.forming)?
            .exchange_timestamp_unix_nanos;
        let last = self
            .forming
            .or_else(|| self.bars.last().copied())?
            .exchange_timestamp_unix_nanos;
        Some((first, last))
    }

    pub(super) fn begin_history_reseed(&mut self) {
        if self.history_state != LiveHistoryState::Ready {
            return;
        }
        self.history_state = LiveHistoryState::Reseeding;
        self.pending_publications.clear();
        self.dirty = false;
    }

    pub(super) fn cancel_history_reseed(&mut self) -> Result<(), String> {
        if self.history_state != LiveHistoryState::Reseeding {
            return Ok(());
        }
        let buffered = std::mem::take(&mut self.buffered);
        self.history_state = LiveHistoryState::Ready;
        for candle in &buffered {
            if self.ingest(candle)? {
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
        let forming = forming.map(|forming| forming.bar).filter(|forming| {
            bars.last()
                .is_none_or(|last| last.source_sequence < forming.source_sequence)
        });
        let start = bars.len().saturating_sub(LIVE_HANDOFF_HISTORY_BARS);
        let working = bars[start..].to_vec();
        if working.is_empty() && forming.is_none() {
            return Err("Hyperliquid live handoff requires history".to_string());
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
                forming,
                "Hyperliquid live publication buffer overflowed",
            )?;
        }
        self.dirty = !self.pending_publications.is_empty();
        let buffered = std::mem::take(&mut self.buffered);
        self.history_state = LiveHistoryState::Ready;
        for candle in &buffered {
            if self.ingest(candle)? {
                self.dirty = true;
            }
        }
        Ok(())
    }

    pub(super) fn take_publication(&mut self) -> Option<LiveSeriesPublication> {
        if !self.connected || self.history_state != LiveHistoryState::Ready || !self.dirty {
            return None;
        }
        let tails = self.pending_publications.drain(..).collect::<Vec<_>>();
        self.dirty = false;
        (!tails.is_empty()).then_some(LiveSeriesPublication::Tails(tails))
    }

    pub(super) fn accept_candle(&mut self, candle: &HyperliquidLiveCandle) -> Result<(), String> {
        if self.history_state == LiveHistoryState::Ready {
            if self.ingest(candle)? {
                self.dirty = true;
            }
        } else if self.buffered.len() == LIVE_BUFFER_CAPACITY {
            return Err("Hyperliquid history/live buffer overflowed".to_string());
        } else {
            self.buffered.push_back(*candle);
        }
        Ok(())
    }

    /// Merges one live replacement, owning the sequence deterministically.
    ///
    /// The same open timestamp keeps the forming sequence; a newer period
    /// extends it. A redelivered update therefore resolves to the identical
    /// bar instead of looking like a new one.
    fn ingest(&mut self, candle: &HyperliquidLiveCandle) -> Result<bool, String> {
        let active = self.forming.or_else(|| self.bars.last().copied());
        if let (Some(active), Some(duration)) = (active, self.series.period.duration_nanos())
            && candle.open_nanos > active.exchange_timestamp_unix_nanos
            && active.exchange_timestamp_unix_nanos.checked_add(duration) != Some(candle.open_nanos)
        {
            return Err("Hyperliquid live candle has a time gap".to_string());
        }
        let sequence = match &self.forming {
            Some(forming) if forming.exchange_timestamp_unix_nanos == candle.open_nanos => {
                forming.source_sequence
            }
            Some(forming) => forming
                .source_sequence
                .checked_add(1)
                .ok_or_else(|| "Hyperliquid live sequence overflowed".to_string())?,
            None => self
                .bars
                .last()
                .map_or(0, |bar| bar.source_sequence)
                .checked_add(1)
                .ok_or_else(|| "Hyperliquid live sequence overflowed".to_string())?,
        };
        let bar = MarketBar {
            source_sequence: sequence,
            exchange_timestamp_seconds: candle.open_nanos.div_euclid(1_000_000_000),
            exchange_timestamp_unix_nanos: candle.open_nanos,
            open: candle.open,
            high: candle.high,
            low: candle.low,
            close: candle.close,
            volume: candle.volume,
        };
        bar.validate().map_err(|error| error.to_string())?;
        merge_live_candle(&mut self.bars, &mut self.forming, bar)?;
        if self.bars.len() > LIVE_HANDOFF_HISTORY_BARS {
            let excess = self.bars.len() - LIVE_HANDOFF_HISTORY_BARS;
            self.bars.drain(..excess);
        }
        enqueue_bar_transition(
            &mut self.pending_publications,
            bar,
            "Hyperliquid live publication buffer overflowed",
        )?;
        self.dirty = true;
        Ok(true)
    }
}

impl Coordinator<'_> {
    fn cancel_inflight_history_for_live_recovery(
        &self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) {
        let key = (series.clone(), generation);
        if self.history_inflight.contains_key(&key)
            && let Some(stop) = self.history_cancellations.get(&key)
        {
            // The buffered live seam is about to be discarded. Any covering
            // history already in flight may end before those discarded events,
            // so its completion can no longer safely seed continuity. The
            // normal cancelled-completion path schedules a fresh covering read.
            stop.store(true, Ordering::Release);
        }
    }

    fn publish_non_bar_study_change(
        &mut self,
        provider_id: &str,
        instrument_id: &str,
        entitlement_id: &str,
        stream: MarketStream,
        observed_unix_nanos: i64,
    ) {
        match self.execute_study_non_bar_change(
            provider_id,
            instrument_id,
            entitlement_id,
            stream,
            observed_unix_nanos,
        ) {
            Ok(batch) => {
                self.publish_study_outputs(&batch.executed);
                for error in batch.errors {
                    eprintln!("Aeris live non-bar study execution failed: {error}");
                }
            }
            Err(error) => {
                // Native study failure must not change acceptance/recovery of
                // authoritative provider state.
                eprintln!("Aeris live non-bar study execution failed: {error}");
            }
        }
    }

    fn clear_hyperliquid_display_depth(&mut self) {
        let affected = self
            .hyperliquid_display_depth
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        self.hyperliquid_display_depth.clear();
        for instrument_id in affected {
            self.broadcast_order_book("hyperliquid", &instrument_id);
        }
    }

    fn invalidate_provider_live_market(&mut self, provider: &str, generation: ProviderGeneration) {
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

    pub(super) fn handle_hyperliquid_display_depth(&mut self, event: HyperliquidDisplayDepthEvent) {
        match event {
            HyperliquidDisplayDepthEvent::Reset { display_generation } => {
                if display_generation <= self.hyperliquid_display_generation {
                    return;
                }
                self.hyperliquid_display_generation = display_generation;
                self.clear_hyperliquid_display_depth();
            }
            HyperliquidDisplayDepthEvent::Snapshot(display) => {
                if display.display_generation < self.hyperliquid_display_generation {
                    return;
                }
                let Some(provider_generation) = self
                    .engine
                    .provider_status("hyperliquid")
                    .and_then(|status| status.generation)
                else {
                    return;
                };
                if provider_generation.0.get() != display.provider_generation
                    || display.snapshot.metadata.provider_id != "hyperliquid"
                    || display.snapshot.metadata.session_generation != display.provider_generation
                {
                    return;
                }
                let instrument_id = display.snapshot.metadata.instrument_id.clone();
                let Some(order_book) = self
                    .order_books
                    .get(&("hyperliquid".to_string(), instrument_id.clone()))
                else {
                    return;
                };
                if order_book.instrument.entitlement_id != display.snapshot.metadata.entitlement_id
                {
                    return;
                }
                // A full display queue may coalesce its reset. A validated
                // snapshot carries the same generation boundary itself.
                if display.display_generation > self.hyperliquid_display_generation {
                    self.hyperliquid_display_generation = display.display_generation;
                    self.clear_hyperliquid_display_depth();
                }
                let source_sequence = display.snapshot.metadata.source_sequence;
                if self
                    .hyperliquid_display_depth
                    .get(&instrument_id)
                    .is_some_and(|current| {
                        current.provider_generation > display.provider_generation
                            || (current.provider_generation == display.provider_generation
                                && (current.display_generation > display.display_generation
                                    || (current.display_generation == display.display_generation
                                        && current.source_sequence >= source_sequence)))
                    })
                {
                    return;
                }
                self.hyperliquid_display_depth.insert(
                    instrument_id.clone(),
                    crate::MarketDisplayDepth {
                        provider_generation: display.provider_generation,
                        display_generation: display.display_generation,
                        source_sequence,
                        bids: display.snapshot.bids,
                        asks: display.snapshot.asks,
                    },
                );
                self.broadcast_order_book("hyperliquid", &instrument_id);
            }
        }
    }

    fn install_live_tails(
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
            let publications = self
                .engine
                .install_realtime_tail(generation, series, price_scale, quantity_scale, bar, true)
                .map_err(|error| error.to_string())?;
            for publication in publications {
                if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                    events.publish_series_update(series_update_message(&publication));
                }
            }
            match self.execute_study_bar_change(series, exchange_timestamp_unix_nanos) {
                Ok(batch) => {
                    self.publish_study_outputs(&batch.executed);
                    for error in batch.errors {
                        eprintln!("Aeris live study execution failed: {error}");
                    }
                }
                Err(error) => {
                    // Study failure is isolated from canonical market publication.
                    // A user calculation must never force provider recovery or make
                    // an accepted market tail look discontinuous.
                    eprintln!("Aeris live study execution failed: {error}");
                }
            }
        }
        Ok(())
    }

    pub(super) fn rithmic_realtime_demand(&self) -> Result<RithmicRealtimeDemand, String> {
        let mut instruments = BTreeMap::<String, RithmicInstrumentDemand>::new();
        for series in self.rithmic_live.keys() {
            let Some(streams) = self
                .engine
                .subscription_status(series)
                .map(|status| status.streams)
            else {
                continue;
            };
            let instrument = self
                .catalog
                .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                .cloned()
                .ok_or_else(|| "Rithmic instrument is not installed".to_string())?;
            let demand = instruments
                .entry(instrument.instrument_id.clone())
                .or_insert_with(|| RithmicInstrumentDemand {
                    instrument,
                    trades: false,
                    quotes: false,
                    order_book: false,
                });
            // Rithmic bars are constructed from trades. Provider transport
            // therefore needs the trade feed for bar continuity even when no
            // consumer requested downstream order-flow publications.
            demand.trades |= streams.contains(MarketStream::Bars)
                || streams.contains(MarketStream::Trades)
                || streams.contains(MarketStream::Depth);
            demand.quotes |= streams.contains(MarketStream::Quotes);
            demand.order_book |= streams.contains(MarketStream::Depth);
        }
        for instrument in self.price_alerts.active_instruments("rithmic") {
            instruments
                .entry(instrument.instrument_id.clone())
                .and_modify(|demand| demand.trades = true)
                .or_insert(RithmicInstrumentDemand {
                    instrument,
                    trades: true,
                    quotes: false,
                    order_book: false,
                });
        }
        Ok(RithmicRealtimeDemand {
            instruments: instruments
                .into_values()
                .filter(|demand| demand.trades || demand.quotes || demand.order_book)
                .collect(),
        })
    }

    pub(super) fn send_rithmic_demand(&mut self) -> Result<(), String> {
        if !self.providers.rithmic_realtime_enabled() {
            return Ok(());
        }
        let demand = self.rithmic_realtime_demand()?;
        if demand.instruments.is_empty() {
            return Ok(());
        }
        if self.rithmic_demand.as_ref() == Some(&demand) && self.rithmic_pending_demand.is_none() {
            return Ok(());
        }
        if self
            .providers
            .send_rithmic_realtime(RithmicRealtimeControl::Subscribe(demand.clone()))?
        {
            self.rithmic_pending_demand = None;
            self.rithmic_demand = Some(demand);
        } else {
            self.rithmic_pending_demand = Some(demand);
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
        if series.provider_id == "rithmic" {
            if !self.rithmic_live.contains_key(series) {
                let venue_id = self
                    .catalog
                    .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                    .ok_or_else(|| "Rithmic instrument is not installed".to_string())?
                    .venue_id
                    .clone();
                if let Some(mut handoff) = RithmicLiveHandoff::new(
                    series,
                    self.provider_generation_for_series(series)?,
                    &venue_id,
                ) {
                    handoff.connected = self
                        .engine
                        .provider_status("rithmic")
                        .is_some_and(|status| status.health == ProviderHealth::Online);
                    self.rithmic_live.insert(series.clone(), handoff);
                }
            }
            self.send_rithmic_demand()?;
            return Ok(());
        }
        if series.provider_id == "hyperliquid" {
            // Hyperliquid drains controls newest-wins, so a replacement
            // Subscribe can cancel an already queued idle Stop without the
            // worker ever emitting Disconnected. Resuming accepted demand must
            // therefore retire the old stop fence here; otherwise valid events
            // from the still-current generation remain suppressed forever.
            self.hyperliquid_stop_pending = None;
            // Hyperliquid multiplexes every demand over one connection, so a
            // timeframe change only changes this series' candle feed and a
            // symbol change only adds its feeds: neither reconnects. The full
            // desired set is rebuilt below and flushed on the next tick.
            let instrument = self.hyperliquid_instrument(series)?.clone();
            let interval = hyperliquid_interval_for_period(series.period)
                .map_err(|_| "Hyperliquid history is unavailable for this interval".to_string())?
                .to_string();
            let generation = self.provider_generation_for_series(series)?;
            if !self.hyperliquid_live.contains_key(series) {
                let mut handoff = HyperliquidLiveHandoff::new(
                    series.clone(),
                    generation,
                    instrument.provider_symbol.clone(),
                    interval,
                );
                handoff.connected = self
                    .engine
                    .provider_status("hyperliquid")
                    .is_some_and(|status| status.health == ProviderHealth::Online);
                self.hyperliquid_live.insert(series.clone(), handoff);
            }
            self.hyperliquid_demand_dirty = true;
            return Ok(());
        }
        Err("realtime provider is unsupported".to_string())
    }

    pub(super) fn handle_rithmic_realtime(&mut self, event: RithmicRealtimeEvent) {
        let event_generation = match &event {
            RithmicRealtimeEvent::Failed(generation, _)
            | RithmicRealtimeEvent::Connecting(generation)
            | RithmicRealtimeEvent::Connected(generation)
            | RithmicRealtimeEvent::Trade(generation, _)
            | RithmicRealtimeEvent::Quote(generation, _)
            | RithmicRealtimeEvent::Depth(generation, _)
            | RithmicRealtimeEvent::Recovering(generation, _)
            | RithmicRealtimeEvent::Disconnected(generation, _)
            | RithmicRealtimeEvent::Heartbeat(generation, _) => *generation,
        };
        if self
            .rithmic_stop_pending
            .is_some_and(|pending| pending.0.get() == event_generation)
            && !matches!(event, RithmicRealtimeEvent::Disconnected(..))
        {
            return;
        }
        match event {
            RithmicRealtimeEvent::Failed(generation, error) => {
                eprintln!("Aeris Rithmic reconnect requires intervention: {error}");
                self.rithmic_failed(
                    generation,
                    "Rithmic reconnect could not start; check provider configuration",
                );
            }
            RithmicRealtimeEvent::Connecting(generation) => self.rithmic_connecting(generation),
            RithmicRealtimeEvent::Connected(generation)
            | RithmicRealtimeEvent::Heartbeat(generation, _) => self.rithmic_online(generation),
            RithmicRealtimeEvent::Trade(generation, trade) => {
                self.rithmic_trade(generation, &trade);
            }
            RithmicRealtimeEvent::Quote(generation, quote) => {
                self.provider_quote("rithmic", generation, &quote);
            }
            RithmicRealtimeEvent::Depth(generation, snapshot) => {
                self.provider_depth("rithmic", generation, &snapshot);
            }
            RithmicRealtimeEvent::Recovering(generation, reason) => {
                self.rithmic_recovering(generation, rithmic_invalidation_detail(reason));
            }
            RithmicRealtimeEvent::Disconnected(generation, reason) => {
                let auto_recover = rithmic_auto_recovers(reason);
                if self
                    .rithmic_stop_pending
                    .is_some_and(|pending| pending.0.get() == generation)
                {
                    self.rithmic_stop_pending = None;
                    if !self.rithmic_live.is_empty()
                        || self.price_alerts.has_active_provider("rithmic")
                    {
                        self.rithmic_demand = None;
                        self.rithmic_pending_demand = None;
                        let _ = self.send_rithmic_demand();
                        self.rithmic_recovering(
                            generation,
                            "Rithmic realtime restarted after idle-stop overlap",
                        );
                        return;
                    }
                    self.rithmic_demand = None;
                    self.rithmic_pending_demand = None;
                    if let Ok(generation) = id(generation).map(ProviderGeneration) {
                        let _ = self.engine.end_provider_session("rithmic", generation);
                        self.broadcast_provider_for("rithmic", None);
                    }
                    return;
                }
                self.rithmic_demand = None;
                self.rithmic_pending_demand = None;
                if let Some(reason) = reason {
                    self.rithmic_failed(generation, rithmic_invalidation_detail(Some(reason)));
                } else {
                    self.rithmic_recovering(generation, "Rithmic live session is recovering");
                }
                if auto_recover
                    && (!self.rithmic_live.is_empty()
                        || self.price_alerts.has_active_provider("rithmic"))
                {
                    let _ = self.send_rithmic_demand();
                }
            }
        }
    }

    pub(super) fn handle_hyperliquid_realtime(&mut self, event: HyperliquidRealtimeEvent) {
        let event_generation = match &event {
            HyperliquidRealtimeEvent::Connecting(generation)
            | HyperliquidRealtimeEvent::Connected(generation)
            | HyperliquidRealtimeEvent::Candle(generation, ..)
            | HyperliquidRealtimeEvent::Trades(generation, _)
            | HyperliquidRealtimeEvent::Quote(generation, _)
            | HyperliquidRealtimeEvent::Depth(generation, _)
            | HyperliquidRealtimeEvent::Recovering(generation)
            | HyperliquidRealtimeEvent::Disconnected(generation)
            | HyperliquidRealtimeEvent::Heartbeat(generation, _) => *generation,
        };
        if self
            .hyperliquid_stop_pending
            .is_some_and(|pending| pending.0.get() == event_generation)
            && !matches!(event, HyperliquidRealtimeEvent::Disconnected(_))
        {
            return;
        }
        match event {
            HyperliquidRealtimeEvent::Connecting(generation) => {
                self.hyperliquid_connecting(generation);
            }
            HyperliquidRealtimeEvent::Connected(generation) => self.hyperliquid_online(generation),
            HyperliquidRealtimeEvent::Heartbeat(generation, _) => {
                self.hyperliquid_online(generation);
            }
            HyperliquidRealtimeEvent::Candle(generation, wire_coin, interval, candle) => {
                self.hyperliquid_candle(generation, &wire_coin, &interval, &candle);
            }
            HyperliquidRealtimeEvent::Trades(generation, trades) => {
                let mut dirty_books = BTreeSet::new();
                for trade in &trades {
                    if let Some(instrument_id) = self.hyperliquid_trade(generation, trade) {
                        dirty_books.insert(instrument_id);
                    }
                }
                for instrument_id in dirty_books {
                    self.broadcast_order_book("hyperliquid", &instrument_id);
                }
            }
            HyperliquidRealtimeEvent::Quote(generation, quote) => {
                self.provider_quote("hyperliquid", generation, &quote);
            }
            HyperliquidRealtimeEvent::Depth(generation, snapshot) => {
                self.provider_depth("hyperliquid", generation, &snapshot);
            }
            HyperliquidRealtimeEvent::Recovering(generation) => {
                self.hyperliquid_recovering(generation, "Hyperliquid live session is recovering");
            }
            HyperliquidRealtimeEvent::Disconnected(generation) => {
                if self
                    .hyperliquid_stop_pending
                    .is_some_and(|pending| pending.0.get() == generation)
                {
                    self.hyperliquid_stop_pending = None;
                    if !self.hyperliquid_live.is_empty()
                        || !self.order_books.is_empty()
                        || self.price_alerts.has_active_provider("hyperliquid")
                    {
                        self.hyperliquid_engaged = false;
                        self.hyperliquid_demand_dirty = true;
                        self.hyperliquid_recovering(
                            generation,
                            "Hyperliquid realtime restarted after idle-stop overlap",
                        );
                        return;
                    }
                    self.hyperliquid_engaged = false;
                    self.hyperliquid_demand_dirty = false;
                    if let Ok(generation) = id(generation).map(ProviderGeneration) {
                        let _ = self.engine.end_provider_session("hyperliquid", generation);
                        self.broadcast_provider_for("hyperliquid", None);
                    }
                    return;
                }
                self.hyperliquid_engaged = false;
                self.hyperliquid_recovering(generation, "Hyperliquid live session is recovering");
            }
        }
    }

    pub(super) fn hyperliquid_connecting(&mut self, generation: u64) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        let current = self
            .engine
            .provider_status("hyperliquid")
            .and_then(|status| status.generation);
        if current.is_some_and(|current| generation < current) {
            return;
        }
        if current.is_none_or(|current| generation > current)
            && self
                .engine
                .begin_provider_session("hyperliquid", generation)
                .is_err()
        {
            return;
        }
        if current.is_some_and(|current| generation > current) {
            self.clear_hyperliquid_display_depth();
            self.invalidate_provider_live_market("hyperliquid", generation);
            self.hyperliquid_demand_dirty = true;
            for ((series, _), stop) in &self.history_cancellations {
                if series.provider_id == "hyperliquid" {
                    stop.store(true, Ordering::Release);
                }
            }
            let series = self.hyperliquid_live.keys().cloned().collect::<Vec<_>>();
            for selected in &series {
                if let Some(live) = self.hyperliquid_live.get_mut(selected) {
                    live.reset(generation);
                }
                self.broadcast_series_recovery_for(
                    selected,
                    "Hyperliquid live session changed; covering history is reloading",
                );
            }
            for selected in series {
                let _ = self.enqueue_history_recovery(&selected, generation);
            }
        }
        if self
            .engine
            .set_provider_health("hyperliquid", generation, ProviderHealth::Connecting)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for("hyperliquid", None);
    }

    pub(super) fn hyperliquid_online(&mut self, generation: u64) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status("hyperliquid")
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        if self
            .engine
            .set_provider_health("hyperliquid", generation, ProviderHealth::Online)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for("hyperliquid", None);
        let mut ready = Vec::new();
        let missing = self
            .hyperliquid_live
            .iter_mut()
            .filter_map(|(series, live)| {
                if live.generation != generation {
                    return None;
                }
                live.connected = true;
                if live.history_state == LiveHistoryState::Ready {
                    ready.push(series.clone());
                    None
                } else {
                    Some(series.clone())
                }
            })
            .collect::<Vec<_>>();
        for series in ready {
            self.series_live_if_ready(&series);
        }
        for series in missing {
            if !self
                .history_inflight
                .contains_key(&(series.clone(), generation))
            {
                let _ = self.enqueue_history_recovery(&series, generation);
            }
        }
    }

    pub(super) fn hyperliquid_candle(
        &mut self,
        generation: u64,
        wire_coin: &str,
        interval: &str,
        candle: &HyperliquidLiveCandle,
    ) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status("hyperliquid")
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        let failed = self
            .hyperliquid_live
            .iter_mut()
            .filter(|(_, live)| {
                live.generation == generation
                    && live.connected
                    && live.wire_coin == wire_coin
                    && live.interval == interval
            })
            .filter_map(|(series, live)| {
                live.accept_candle(candle).is_err().then(|| series.clone())
            })
            .collect::<Vec<_>>();
        for series in failed {
            self.hyperliquid_series_recovering(
                &series,
                generation,
                FailureStage::Aggregation,
                "Hyperliquid candle replacement requires covering history",
            );
        }
    }

    pub(super) fn hyperliquid_trade(
        &mut self,
        generation: u64,
        trade: &MarketTrade,
    ) -> Option<String> {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return None;
        };
        if self
            .engine
            .provider_status("hyperliquid")
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return None;
        }
        if trade.metadata.provider_id != "hyperliquid"
            || trade.metadata.session_generation != generation.0.get()
        {
            self.hyperliquid_recovering(
                generation.0.get(),
                "Hyperliquid live session identity requires recovery",
            );
            for live in self.hyperliquid_live.values_mut() {
                live.history_state = LiveHistoryState::AwaitingHistory;
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
            .get_mut(&("hyperliquid".to_string(), instrument_id.clone()))
            .is_some_and(|order_book| order_book.accept_recent_trade(trade));
        if trade_changed {
            self.publish_non_bar_study_change(
                "hyperliquid",
                &instrument_id,
                &entitlement_id,
                MarketStream::Trades,
                observed_unix_nanos,
            );
        }
        trade_changed.then_some(instrument_id)
    }

    pub(super) fn hyperliquid_series_recovering(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        _stage: FailureStage,
        detail: &str,
    ) {
        self.cancel_inflight_history_for_live_recovery(series, generation);
        if let Some(live) = self.hyperliquid_live.get_mut(series) {
            live.history_state = LiveHistoryState::AwaitingHistory;
            live.dirty = false;
            live.buffered.clear();
            live.pending_publications.clear();
        }
        self.broadcast_series_recovery_for(series, detail);
        if !self
            .history_inflight
            .contains_key(&(series.clone(), generation))
        {
            let _ = self.enqueue_history_recovery(series, generation);
        }
    }

    pub(super) fn hyperliquid_recovering(&mut self, generation: u64, detail: &'static str) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status("hyperliquid")
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        self.price_alerts.reset_provider_baselines("hyperliquid");
        self.clear_hyperliquid_display_depth();
        self.hyperliquid_demand_dirty = true;
        if self.price_alerts.has_active_provider("hyperliquid") {
            self.hyperliquid_demand_dirty = true;
        }
        if self
            .engine
            .set_provider_health("hyperliquid", generation, ProviderHealth::Recovering)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for("hyperliquid", Some(detail));
        for live in self.hyperliquid_live.values_mut() {
            live.connected = false;
        }
        self.invalidate_provider_live_market("hyperliquid", generation);
    }

    pub(super) fn rithmic_connecting(&mut self, generation: u64) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        let current = self
            .engine
            .provider_status("rithmic")
            .and_then(|status| status.generation);
        if current.is_some_and(|current| generation < current) {
            return;
        }
        if current.is_none_or(|current| generation > current)
            && self
                .engine
                .begin_provider_session("rithmic", generation)
                .is_err()
        {
            return;
        }
        if current.is_some_and(|current| generation > current) {
            self.invalidate_provider_live_market("rithmic", generation);
            for ((series, _), stop) in &self.history_cancellations {
                if series.provider_id == "rithmic" {
                    stop.store(true, Ordering::Release);
                }
            }
            let series = self.rithmic_live.keys().cloned().collect::<Vec<_>>();
            for selected in &series {
                if let Some(live) = self.rithmic_live.get_mut(selected) {
                    live.reset(generation);
                }
                self.broadcast_series_recovery_for(
                    selected,
                    "Rithmic live session changed; covering history is reloading",
                );
            }
            for selected in series {
                let _ = self.enqueue_history_recovery(&selected, generation);
            }
        }
        if self
            .engine
            .set_provider_health("rithmic", generation, ProviderHealth::Connecting)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for("rithmic", None);
    }

    pub(super) fn rithmic_online(&mut self, generation: u64) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status("rithmic")
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        if self
            .engine
            .set_provider_health("rithmic", generation, ProviderHealth::Online)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for("rithmic", None);
        let mut ready = Vec::new();
        let missing = self
            .rithmic_live
            .iter_mut()
            .filter_map(|(series, live)| {
                if live.generation != generation {
                    return None;
                }
                live.connected = true;
                if live.history_state == LiveHistoryState::Ready {
                    ready.push(series.clone());
                    None
                } else {
                    Some(series.clone())
                }
            })
            .collect::<Vec<_>>();
        for series in ready {
            self.series_live_if_ready(&series);
        }
        for series in missing {
            if !self
                .history_inflight
                .contains_key(&(series.clone(), generation))
            {
                let _ = self.enqueue_history_recovery(&series, generation);
            }
        }
    }

    pub(super) fn rithmic_trade(&mut self, generation: u64, trade: &MarketTrade) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status("rithmic")
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        if trade.metadata.provider_id != "rithmic"
            || trade.metadata.session_generation != generation.0.get()
        {
            self.rithmic_recovering(
                generation.0.get(),
                "Rithmic live session identity requires recovery",
            );
            for live in self.rithmic_live.values_mut() {
                live.history_state = LiveHistoryState::AwaitingHistory;
                live.dirty = false;
                live.buffered.clear();
                live.pending_publications.clear();
            }
            return;
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
        // Classify this trade against every bar-aligned series before exposing
        // its point-in-time trade state to studies. A trade can itself reveal
        // that one timeframe's live aggregator needs covering recovery; that
        // series must enter AwaitingHistory before the non-bar study wave runs,
        // while unaffected timeframes remain eligible.
        let failed = self
            .rithmic_live
            .iter_mut()
            .filter(|(_, live)| {
                live.generation == generation
                    && live.connected
                    && live.series.instrument_id == trade.metadata.instrument_id
                    && live.series.entitlement_id == trade.metadata.entitlement_id
            })
            .filter_map(|(series, live)| live.accept_trade(trade).is_err().then(|| series.clone()))
            .collect::<Vec<_>>();
        for series in failed {
            self.rithmic_series_recovering(
                &series,
                generation,
                FailureStage::Aggregation,
                "Rithmic instrument aggregation requires covering history",
            );
        }
        let trade_changed = self
            .order_books
            .get_mut(&("rithmic".to_string(), instrument_id.clone()))
            .is_some_and(|order_book| order_book.accept_recent_trade(trade));
        if trade_changed {
            self.broadcast_order_book("rithmic", &instrument_id);
            self.publish_non_bar_study_change(
                "rithmic",
                &instrument_id,
                &entitlement_id,
                MarketStream::Trades,
                observed_unix_nanos,
            );
        }
    }

    pub(super) fn rithmic_series_recovering(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        _stage: FailureStage,
        detail: &str,
    ) {
        if let Some(live) = self.rithmic_live.get_mut(series) {
            live.history_state = LiveHistoryState::AwaitingHistory;
            live.dirty = false;
            live.buffered.clear();
            live.pending_publications.clear();
        }
        self.broadcast_series_recovery_for(series, detail);
        if !self
            .history_inflight
            .contains_key(&(series.clone(), generation))
        {
            let _ = self.enqueue_history_recovery(series, generation);
        }
    }

    fn evaluate_price_alert_trade(&mut self, trade: &MarketTrade) {
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
            if trade.metadata.provider_id == "rithmic" {
                let _ = self.send_rithmic_demand();
            } else if trade.metadata.provider_id == "hyperliquid" {
                self.hyperliquid_demand_dirty = true;
            }
            self.stop_realtime_if_idle();
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

    pub(super) fn rithmic_recovering(&mut self, generation: u64, detail: &'static str) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status("rithmic")
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        self.price_alerts.reset_provider_baselines("rithmic");
        if self
            .engine
            .set_provider_health("rithmic", generation, ProviderHealth::Recovering)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for("rithmic", Some(detail));
        for live in self.rithmic_live.values_mut() {
            live.connected = false;
        }
        self.invalidate_provider_live_market("rithmic", generation);
    }

    fn rithmic_failed(&mut self, generation: u64, detail: &'static str) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status("rithmic")
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        self.price_alerts.reset_provider_baselines("rithmic");
        if self
            .engine
            .set_provider_health("rithmic", generation, ProviderHealth::Failed)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for("rithmic", Some(detail));
        for live in self.rithmic_live.values_mut() {
            live.connected = false;
        }
        self.invalidate_provider_live_market("rithmic", generation);
    }

    pub(super) fn publish_rithmic_live(&mut self) {
        let ready = self
            .rithmic_live
            .values_mut()
            .filter_map(|live| {
                let update = live.take_publication()?;
                Some((
                    live.series.clone(),
                    live.generation,
                    live.price_scale,
                    live.quantity_scale,
                    update,
                ))
            })
            .collect::<Vec<_>>();
        for (series, generation, price_scale, quantity_scale, update) in ready {
            let published = match update {
                LiveSeriesPublication::Tails(bars) => {
                    self.install_live_tails(&series, generation, price_scale, quantity_scale, bars)
                }
            };
            if let Err(error) = published {
                eprintln!("Aeris engine Rithmic live publication failed: {error}");
                self.rithmic_series_recovering(
                    &series,
                    generation,
                    FailureStage::Publication,
                    "Rithmic live publication requires covering history",
                );
            }
        }
    }

    pub(super) fn series_live_if_ready(&mut self, series: &BarSeriesKey) -> bool {
        let detached = self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation)
            .is_some_and(|generation| {
                self.detached_history
                    .contains(&(series.clone(), generation))
            });
        if detached {
            return false;
        }
        let ready =
            self.rithmic_live.get(series).is_some_and(|live| {
                live.connected && live.history_state == LiveHistoryState::Ready
            }) || self.hyperliquid_live.get(series).is_some_and(|live| {
                live.connected && live.history_state == LiveHistoryState::Ready
            });
        if ready {
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
            }
        }
        ready
    }

    /// Rebuilds the complete desired Hyperliquid subscription set from the
    /// authoritative engine stream union. Each provider feed follows only the
    /// stream classes that require it; retaining canonical live state never
    /// implies an unrelated upstream subscription.
    pub(super) fn hyperliquid_demand(&self) -> HyperliquidDemand {
        let mut candles = BTreeSet::new();
        let mut trades = BTreeSet::new();
        let mut quotes = BTreeSet::new();
        let mut books = BTreeSet::new();
        for (series, live) in &self.hyperliquid_live {
            let Some(streams) = self
                .engine
                .subscription_status(series)
                .map(|status| status.streams)
            else {
                continue;
            };
            let Ok(instrument) = self.hyperliquid_instrument(series) else {
                continue;
            };
            let (Ok(price_scale), Ok(quantity_scale)) = (
                u8::try_from(instrument.price_scale),
                u8::try_from(instrument.quantity_scale),
            ) else {
                continue;
            };
            let mapping = crate::hyperliquid_realtime::HyperliquidInstrumentDemand {
                wire_coin: live.wire_coin.clone(),
                instrument_id: series.instrument_id.clone(),
                entitlement_id: series.entitlement_id.clone(),
                price_scale,
                quantity_scale,
            };
            if streams.contains(MarketStream::Bars) {
                candles.insert(HyperliquidCandleDemand {
                    instrument: mapping.clone(),
                    interval: live.interval.clone(),
                });
            }
            if streams.contains(MarketStream::Trades) {
                trades.insert(mapping.clone());
            }
            if streams.contains(MarketStream::Quotes) || streams.contains(MarketStream::Depth) {
                quotes.insert(mapping.clone());
            }
            if streams.contains(MarketStream::Depth) {
                books.insert(mapping);
            }
        }
        for instrument in self.price_alerts.active_instruments("hyperliquid") {
            let (Ok(price_scale), Ok(quantity_scale)) = (
                u8::try_from(instrument.price_scale),
                u8::try_from(instrument.quantity_scale),
            ) else {
                continue;
            };
            trades.insert(HyperliquidInstrumentDemand {
                wire_coin: instrument.provider_symbol,
                instrument_id: instrument.instrument_id,
                entitlement_id: instrument.entitlement_id,
                price_scale,
                quantity_scale,
            });
        }
        HyperliquidDemand {
            candles: candles.into_iter().collect(),
            trades: trades.into_iter().collect(),
            quotes: quotes.into_iter().collect(),
            books: books.into_iter().collect(),
        }
    }

    fn hyperliquid_display_depth_demand(&self) -> HyperliquidDisplayDepthDemand {
        let Some(provider_generation) = self
            .engine
            .provider_status("hyperliquid")
            .and_then(|status| status.generation)
        else {
            return HyperliquidDisplayDepthDemand::default();
        };
        let mut books = BTreeSet::new();
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
                .filter(|series| series.provider_id == "hyperliquid")
            else {
                continue;
            };
            let Some(book) = self
                .order_books
                .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                .filter(|book| book.instrument.entitlement_id == series.entitlement_id)
            else {
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
        if !self.hyperliquid_demand_dirty {
            return;
        }
        let demand = self.hyperliquid_demand();
        let display_demand = self.hyperliquid_display_depth_demand();
        let empty = demand.candles.is_empty()
            && demand.trades.is_empty()
            && demand.quotes.is_empty()
            && demand.books.is_empty();
        if empty && !self.hyperliquid_engaged {
            self.hyperliquid_demand_dirty = false;
            return;
        }
        let raw = self
            .providers
            .send_hyperliquid_realtime(HyperliquidRealtimeControl::Subscribe(demand));
        let display = self.providers.send_hyperliquid_display_depth(
            HyperliquidDisplayDepthControl::Subscribe(display_demand),
        );
        match (raw, display) {
            (Ok(true), Ok(true)) => {
                self.hyperliquid_demand_dirty = false;
                self.hyperliquid_engaged = !empty;
            }
            // A full channel retries on the next coordinator tick; the
            // worker coalesces to the newest set.
            (Ok(false), _) | (_, Ok(false)) => {}
            (Err(_), _) | (_, Err(_)) => {
                self.hyperliquid_demand_dirty = false;
            }
        }
    }

    pub(super) fn publish_hyperliquid_live(&mut self) {
        let ready = self
            .hyperliquid_live
            .values_mut()
            .filter_map(|live| {
                let update = live.take_publication()?;
                Some((
                    live.series.clone(),
                    live.generation,
                    live.price_scale,
                    live.quantity_scale,
                    update,
                ))
            })
            .collect::<Vec<_>>();
        for (series, generation, price_scale, quantity_scale, update) in ready {
            let published = match update {
                LiveSeriesPublication::Tails(bars) => {
                    self.install_live_tails(&series, generation, price_scale, quantity_scale, bars)
                }
            };
            if let Err(error) = published {
                eprintln!("Aeris engine Hyperliquid live publication failed: {error}");
                self.hyperliquid_series_recovering(
                    &series,
                    generation,
                    FailureStage::Publication,
                    "Hyperliquid live publication requires covering history",
                );
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
            for ((active, _), stop) in &self.history_cancellations {
                if active == &series {
                    stop.store(true, Ordering::Release);
                }
            }
        }
    }

    pub(super) fn prune_unused_live_series(&mut self) {
        for series in self
            .rithmic_live
            .keys()
            .filter(|series| !self.engine.has_subscription(series))
        {
            for ((active, _), stop) in &self.history_cancellations {
                if active == series {
                    stop.store(true, Ordering::Release);
                }
            }
        }
        let retained_rithmic = self.rithmic_live.len();
        self.rithmic_live
            .retain(|series, _| self.engine.has_subscription(series));
        if self.rithmic_live.len() != retained_rithmic && !self.rithmic_live.is_empty() {
            let _ = self.send_rithmic_demand();
        }
        for series in self
            .hyperliquid_live
            .keys()
            .filter(|series| !self.engine.has_subscription(series))
        {
            for ((active, _), stop) in &self.history_cancellations {
                if active == series {
                    stop.store(true, Ordering::Release);
                }
            }
        }
        let retained_hyperliquid = self.hyperliquid_live.len();
        self.hyperliquid_live
            .retain(|series, _| self.engine.has_subscription(series));
        if self.hyperliquid_live.len() != retained_hyperliquid {
            self.hyperliquid_demand_dirty = true;
        }
    }

    pub(super) fn release_unused_live_market_data(&mut self) {
        self.prune_history_tracking();
        self.prune_unused_live_series();
        self.evict_unreferenced_series();
        self.stop_realtime_if_idle();
    }
    /// Hands the worker a replacement subscription set the control channel refused.
    pub(super) fn flush_rithmic_demand(&mut self) {
        let Some(demand) = self.rithmic_pending_demand.take() else {
            return;
        };
        match self
            .providers
            .send_rithmic_realtime(RithmicRealtimeControl::Subscribe(demand.clone()))
        {
            Ok(true) => self.rithmic_demand = Some(demand),
            Ok(false) | Err(_) => self.rithmic_pending_demand = Some(demand),
        }
    }
    pub(super) fn stop_realtime_if_idle(&mut self) {
        if self.rithmic_live.is_empty()
            && !self.price_alerts.has_active_provider("rithmic")
            && self.rithmic_demand.is_some()
            && self.rithmic_stop_pending.is_none()
            && let Some(generation) = self
                .engine
                .provider_status("rithmic")
                .and_then(|status| status.generation)
        {
            match self.providers.stop("rithmic") {
                Ok(true) => self.rithmic_stop_pending = Some(generation),
                Ok(false) => {}
                Err(_) => {
                    self.rithmic_demand = None;
                    self.rithmic_pending_demand = None;
                    let _ = self.engine.end_provider_session("rithmic", generation);
                }
            }
        }
        if self.hyperliquid_live.is_empty()
            && !self.price_alerts.has_active_provider("hyperliquid")
            && self.hyperliquid_engaged
            && self.hyperliquid_stop_pending.is_none()
            && let Some(generation) = self
                .engine
                .provider_status("hyperliquid")
                .and_then(|status| status.generation)
        {
            match self.providers.stop("hyperliquid") {
                Ok(true) => self.hyperliquid_stop_pending = Some(generation),
                Ok(false) => {}
                Err(_) => {
                    self.hyperliquid_engaged = false;
                    self.hyperliquid_demand_dirty = false;
                    let _ = self.engine.end_provider_session("hyperliquid", generation);
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

    fn rithmic_tick_series() -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
            entitlement_id: "rithmic-test:CME:MNQ".to_string(),
            period: BarPeriod::tick(2).expect("tick period"),
            definition_version: 1,
        }
    }

    fn rithmic_trade(sequence: u64, timestamp: i64, price: i64) -> MarketTrade {
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
        }
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
            bid: Some(DepthLevel {
                price: 20_000,
                quantity: 2,
                order_count: None,
            }),
            ask: Some(DepthLevel {
                price: 20_025,
                quantity: 3,
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
        let mut book = ProviderOrderBook::new(ladder_instrument(1));
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
        let mut book = ProviderOrderBook::new(ladder_instrument(1));
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
        assert!(!book.accept_recent_trade(&ladder_trade(
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
    fn rithmic_tick_burst_keeps_completed_revision_before_next_forming_bar() {
        let series = rithmic_tick_series();
        let mut live = RithmicLiveHandoff::new(&series, generation(), "CME").expect("handoff");
        live.seed(2, 0, &[bar(10, 1_000_000_000, 10_000)], None, None)
            .expect("seed");
        live.connected = true;

        live.accept_trade(&rithmic_trade(1, 2_000_000_000, 10_100))
            .expect("first");
        live.accept_trade(&rithmic_trade(2, 3_000_000_000, 10_200))
            .expect("second");
        live.accept_trade(&rithmic_trade(3, 4_000_000_000, 10_300))
            .expect("third");

        let LiveSeriesPublication::Tails(bars) = live.take_publication().expect("burst");
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
        let mut live = RithmicLiveHandoff::new(&series, generation(), "CME").expect("handoff");
        let seed = bar(10, 1_000_000_000, 10_000);
        live.seed(
            2,
            0,
            &[seed],
            None,
            Some(seed.exchange_timestamp_unix_nanos),
        )
        .expect("initial seed");
        live.connected = true;
        live.begin_history_reseed();

        live.accept_trade(&rithmic_trade(1, 2_000_000_000, 10_100))
            .expect("trade buffers while reseeding");
        live.accept_trade(&rithmic_trade(2, 3_000_000_000, 10_200))
            .expect("second trade buffers while reseeding");
        assert_eq!(live.buffered.len(), 2);
        assert_eq!(live.bars.last().map(|bar| bar.close), Some(10_000));
        assert_eq!(live.history_state, LiveHistoryState::Reseeding);

        live.seed(
            2,
            0,
            &[seed],
            None,
            Some(seed.exchange_timestamp_unix_nanos),
        )
        .expect("current history reseed replays buffered trades");
        assert!(live.buffered.is_empty());
        assert_eq!(live.history_state, LiveHistoryState::Ready);
        assert_eq!(live.bars.last().map(|bar| bar.source_sequence), Some(11));
        assert_eq!(live.bars.last().map(|bar| bar.close), Some(10_200));
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
        let mut live =
            HyperliquidLiveHandoff::new(series, generation(), "BTC".to_string(), "1m".to_string());
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

        live.accept_candle(&HyperliquidLiveCandle {
            open_nanos: 120_000_000_000,
            open: 10_000,
            high: 10_400,
            low: 9_900,
            close: 10_250,
            volume: 4,
        })
        .expect("final revision");
        live.accept_candle(&HyperliquidLiveCandle {
            open_nanos: 180_000_000_000,
            open: 10_250,
            high: 10_500,
            low: 10_200,
            close: 10_300,
            volume: 1,
        })
        .expect("rollover");

        let LiveSeriesPublication::Tails(bars) = live.take_publication().expect("rollover tails");
        assert_eq!(
            bars.iter()
                .map(|bar| bar.source_sequence)
                .collect::<Vec<_>>(),
            vec![11, 12]
        );
        assert_eq!(bars[0].close, 10_250);
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
        let mut live =
            HyperliquidLiveHandoff::new(series, generation(), "BTC".to_string(), "1m".to_string());
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

        live.accept_candle(&HyperliquidLiveCandle {
            open_nanos: 120_000_000_000,
            open: 10_000,
            high: 10_400,
            low: 9_900,
            close: 10_250,
            volume: 4,
        })
        .expect("forming revision buffers");
        live.accept_candle(&HyperliquidLiveCandle {
            open_nanos: 180_000_000_000,
            open: 10_250,
            high: 10_500,
            low: 10_200,
            close: 10_300,
            volume: 1,
        })
        .expect("new candle buffers");
        assert_eq!(live.buffered.len(), 2);
        assert_eq!(live.forming.map(|bar| bar.close), Some(10_100));

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
        assert_eq!(live.history_state, LiveHistoryState::Ready);
        assert_eq!(live.bars.last().map(|bar| bar.close), Some(10_250));
        assert_eq!(live.forming.map(|bar| bar.close), Some(10_300));
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
            assert!(!rithmic_auto_recovers(Some(reason)), "{reason:?}");
        }
        for reason in [
            ProviderInvalidationReason::Transport,
            ProviderInvalidationReason::HeartbeatSilence,
            ProviderInvalidationReason::MessageSilence,
            ProviderInvalidationReason::SequenceGap,
            ProviderInvalidationReason::QueueOverflow,
        ] {
            assert!(rithmic_auto_recovers(Some(reason)), "{reason:?}");
        }
        assert!(rithmic_auto_recovers(None));
    }
}
