use super::{
    AggressorSide, BTreeMap, BTreeSet, BarPeriod, BarSeriesKey, CandleGapPolicy, CandleLiveHandoff,
    CanonicalOrderBookState, ConsumerId, Coordinator, DepthSnapshot, Duration, FailureStage,
    FormingBar, HyperliquidInstrumentDemand, IndexedTradeMutation, InstallProviderInstrument,
    Instant, LIVE_BUFFER_CAPACITY, LIVE_HANDOFF_HISTORY_BARS, LiveHistoryState,
    LiveSeriesPublication, MAXIMUM_RECENT_LADDER_TRADES, MarketBar, MarketStream, MarketTrade,
    NonZeroUsize, OrderBook, OrderBookApplyOutcome, Ordering, ProviderDemand, ProviderDisconnect,
    ProviderDisplayDepthEvent, ProviderEvent, ProviderEventKind, ProviderGeneration,
    ProviderHealth, ProviderInstrumentDemand, ProviderOrderBook, ProviderTradeBatch,
    RithmicCalendarPeriod, RithmicExchangeCalendar, SeriesLoadState, StreamRequirements,
    TopOfBookQuote, TradeLiveCadence, TradeLiveHandoff, TradeLiveUpdate, VecDeque, id,
    merge_live_candle, series_state_payload, series_update_message,
};
#[cfg(test)]
use super::{HyperliquidDemand, RithmicRealtimeDemand};
use crate::hyperliquid_display_depth::{
    HyperliquidDisplayBookDemand, HyperliquidDisplayDepthControl, HyperliquidDisplayDepthDemand,
};
use crate::study::{
    StudyDepthView, StudyLiveMarketData, StudyMarketInput, StudyQuoteView, StudyTradeWindow,
};
use aeris_rithmic_protocol_adapter::ProviderInvalidationReason;

const MAXIMUM_CANONICAL_DEPTH_LEVELS: usize = 4_096;
const RECENT_TRADE_RETENTION_NANOS: i64 = 8 * 60 * 1_000_000_000;

pub(super) const fn rithmic_invalidation_detail(
    reason: Option<ProviderInvalidationReason>,
) -> &'static str {
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

pub(super) const fn rithmic_auto_recovers(reason: Option<ProviderInvalidationReason>) -> bool {
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

const LIVE_SERIES_STALL_THRESHOLD: Duration = Duration::from_secs(30);
const RECENT_INSTRUMENT_TRADE_WINDOW: Duration = Duration::from_secs(10);

fn live_series_stalled(
    now: Instant,
    last_publication: Instant,
    last_trade: Option<Instant>,
    provider_health: ProviderHealth,
    session_phase: aeris_contracts::MarketSessionPhase,
    connected: bool,
    history_state: LiveHistoryState,
) -> bool {
    provider_health == ProviderHealth::Online
        && matches!(
            session_phase,
            aeris_contracts::MarketSessionPhase::Regular
                | aeris_contracts::MarketSessionPhase::PreMarket
                | aeris_contracts::MarketSessionPhase::PostMarket
                | aeris_contracts::MarketSessionPhase::Overnight
                | aeris_contracts::MarketSessionPhase::AlwaysOpen
        )
        && connected
        && history_state == LiveHistoryState::Ready
        && now.duration_since(last_publication) >= LIVE_SERIES_STALL_THRESHOLD
        && last_trade
            .is_some_and(|trade| now.duration_since(trade) <= RECENT_INSTRUMENT_TRADE_WINDOW)
}

impl ProviderOrderBook {
    pub(super) fn new(
        instrument: InstallProviderInstrument,
        trade_continuity: super::TradeContinuity,
        top_of_book_only: bool,
    ) -> Self {
        let trade_session_generation = instrument.session_generation;
        Self {
            instrument,
            trade_continuity,
            top_of_book_only,
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
            indexed_trade_ordinals: BTreeMap::new(),
            trade_tape_dirty: true,
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
        self.indexed_trade_ordinals.clear();
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
        self.indexed_trade_ordinals.remove(&trade.trade_id);
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
        while self.recent_trades.front().is_some_and(|trade| {
            trade.observed_unix_nanos < cutoff
                && self.trade_continuity == super::TradeContinuity::Sequence
        }) || self.recent_trades.len() > MAXIMUM_RECENT_LADDER_TRADES
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
        if self.trade_continuity == super::TradeContinuity::Sequence
            && trade.metadata.source_sequence <= self.last_trade_source_sequence
        {
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
        let retained = crate::RetainedMarketTrade {
            ingestion_ordinal,
            observed_unix_nanos,
            trade: std::sync::Arc::new(trade.clone()),
        };
        if self.trade_continuity == super::TradeContinuity::Indexed {
            self.indexed_trade_ordinals
                .insert(trade.trade_id.clone(), ingestion_ordinal);
        }
        self.recent_trades.push_back(retained.clone());
        self.trade_tape_revision = self.trade_tape_revision.saturating_add(1);
        self.trade_tape_dirty = true;
        let mut changed = true;
        if self.recent_trades.len() > MAXIMUM_RECENT_LADDER_TRADES {
            changed |= self.prune_recent_trades(observed_unix_nanos);
        }
        changed
    }

    pub(super) fn replace_indexed_trade(
        &mut self,
        index: &str,
        trade: Option<&MarketTrade>,
        historical: bool,
    ) -> Result<(), String> {
        if let Some(trade) = trade
            && (trade.validate().is_err()
                || trade.metadata.provider_id != self.instrument.provider
                || trade.metadata.instrument_id != self.instrument.instrument_id
                || trade.metadata.entitlement_id != self.instrument.entitlement_id
                || trade.metadata.session_generation != self.trade_session_generation)
        {
            return Err("Indexed trade identity is stale or invalid".into());
        }
        let previous = self.indexed_trade_ordinals.get(index).copied();
        // A history reply cannot overwrite a newer live correction or resurrect a cancellation.
        if historical && previous.is_some() {
            return Ok(());
        }
        if let Some(ordinal) = previous
            && ordinal > 0
            && let Ok(position) = self
                .recent_trades
                .binary_search_by_key(&ordinal, |retained| retained.ingestion_ordinal)
        {
            if let Some(trade) = trade
                && self.recent_trades.get(position).is_some_and(|retained| {
                    retained.trade.price == trade.price
                        && retained.trade.quantity == trade.quantity
                        && retained.trade.aggressor == trade.aggressor
                        && retained.trade.metadata.timestamps.exchange_unix_nanos
                            == trade.metadata.timestamps.exchange_unix_nanos
                })
            {
                return Ok(());
            }
            if let Some(retained) = self.recent_trades.remove(position) {
                self.remove_recent_trade(retained);
            }
            self.trade_tape_rewrite_generation =
                self.trade_tape_rewrite_generation.saturating_add(1);
            self.trade_tape_revision = self.trade_tape_revision.saturating_add(1);
            self.trade_tape_dirty = true;
        }
        if self.indexed_trade_ordinals.len() >= MAXIMUM_RECENT_LADDER_TRADES * 2
            && !self.indexed_trade_ordinals.contains_key(index)
        {
            return Err(
                "Indexed trade identity window exhausted; history recovery is required".into(),
            );
        }
        if let Some(trade) = trade {
            if historical {
                self.trade_tape_rewrite_generation =
                    self.trade_tape_rewrite_generation.saturating_add(1);
            }
            self.accept_recent_trade(trade);
        } else {
            self.indexed_trade_ordinals.insert(index.into(), 0);
        }
        Ok(())
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

impl TradeLiveHandoff {
    pub(super) fn new(
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        venue_id: &str,
        candle: Option<(CandleGapPolicy, String)>,
    ) -> Option<Self> {
        let cadence = match series.period {
            BarPeriod::Tick { trades } => TradeLiveCadence::Tick {
                trades,
                forming: trades,
            },
            BarPeriod::Time { seconds } => TradeLiveCadence::Fixed {
                seconds: i64::from(seconds),
            },
            BarPeriod::Session { days } => TradeLiveCadence::Fixed {
                seconds: i64::from(days) * 86_400,
            },
            BarPeriod::Week { weeks: 1 } => TradeLiveCadence::Calendar {
                calendar: RithmicExchangeCalendar::for_venue(venue_id)?,
                period: RithmicCalendarPeriod::Week,
            },
            BarPeriod::Month { months: 1 } => TradeLiveCadence::Calendar {
                calendar: RithmicExchangeCalendar::for_venue(venue_id)?,
                period: RithmicCalendarPeriod::Month,
            },
            BarPeriod::Week { .. } | BarPeriod::Month { .. } => return None,
        };
        let (gap_policy, candle_symbol) = candle
            .map_or((CandleGapPolicy::Contiguous, None), |(policy, symbol)| {
                (policy, Some(symbol))
            });
        Some(Self {
            series: series.clone(),
            generation,
            cadence,
            gap_policy,
            candle_symbol,
            price_scale: 0,
            quantity_scale: 0,
            bars: Vec::new(),
            buffered: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            pending_publications: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            pending_corrections: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            connected: false,
            history_state: LiveHistoryState::AwaitingHistory,
            dirty: false,
            forming_tail_sequence: None,
            live_session_generation: None,
            last_trade_sequence: None,
            history_boundary_unix_nanos: i64::MIN,
            overlay_base: None,
            indexed_overlay: BTreeMap::new(),
            provider_candle_count: None,
            provider_candle_timestamp: None,
            last_publication_at: Instant::now(),
            last_trade_at: None,
        })
    }

    pub(super) fn reset(&mut self, generation: ProviderGeneration) {
        self.generation = generation;
        self.bars.clear();
        self.buffered.clear();
        self.pending_publications.clear();
        self.pending_corrections.clear();
        self.connected = false;
        self.history_state = LiveHistoryState::AwaitingHistory;
        self.dirty = false;
        self.forming_tail_sequence = None;
        self.live_session_generation = None;
        self.last_trade_sequence = None;
        self.history_boundary_unix_nanos = i64::MIN;
        self.overlay_base = None;
        self.indexed_overlay.clear();
        self.provider_candle_count = None;
        self.provider_candle_timestamp = None;
        self.last_publication_at = Instant::now();
        self.last_trade_at = None;
        if let TradeLiveCadence::Tick { trades, forming } = &mut self.cadence {
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
        self.pending_corrections.clear();
        self.dirty = false;
    }

    pub(super) fn cancel_history_reseed(&mut self) -> Result<(), String> {
        if self.history_state != LiveHistoryState::Reseeding {
            return Ok(());
        }
        let buffered = std::mem::take(&mut self.buffered);
        self.history_state = LiveHistoryState::Ready;
        for update in &buffered {
            if self.apply_update(update)? {
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
        self.pending_corrections.clear();
        // A tick bundle whose trade count is unknown cannot be resumed: the
        // cadence would not know when it closes, so it is treated as complete.
        let forming = forming.filter(|forming| {
            let countable =
                forming.trades.is_some() || !matches!(self.cadence, TradeLiveCadence::Tick { .. });
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
        self.last_publication_at = Instant::now();
        self.last_trade_at = None;
        self.overlay_base = None;
        self.indexed_overlay.clear();
        self.provider_candle_count = forming
            .as_ref()
            .and_then(|forming| forming.trades)
            .map(u64::from);
        self.provider_candle_timestamp = forming
            .as_ref()
            .map(|forming| forming.bar.exchange_timestamp_unix_nanos);
        if let TradeLiveCadence::Tick {
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
        for update in &buffered {
            if self.apply_update(update)? {
                self.dirty = true;
            }
        }
        Ok(())
    }

    pub(super) fn take_publication(&mut self) -> Option<LiveSeriesPublication> {
        if !self.connected || self.history_state != LiveHistoryState::Ready || !self.dirty {
            return None;
        }
        // The engine cannot correct a bar before it has accepted the queued
        // append that created it. DXLink candle and trade channels can revise
        // the same bucket before this coordinator tick drains either queue.
        if !self.pending_publications.is_empty() {
            let tails = self.pending_publications.drain(..).collect::<Vec<_>>();
            self.dirty = !self.pending_corrections.is_empty();
            return Some(LiveSeriesPublication::Tails(tails));
        }
        if let Some(bar) = self.pending_corrections.pop_front() {
            self.dirty = !self.pending_corrections.is_empty();
            return Some(LiveSeriesPublication::CompletedCorrection(bar));
        }
        self.dirty = false;
        None
    }

    pub(super) fn accept_trade(&mut self, trade: &MarketTrade) -> Result<(), String> {
        if self.history_state == LiveHistoryState::Ready {
            self.last_trade_at = Some(Instant::now());
            if self.apply_trade(trade)? {
                self.dirty = true;
            }
        } else if self.buffered.len() == LIVE_BUFFER_CAPACITY {
            return Err("Rithmic history/live buffer overflowed".to_string());
        } else {
            self.buffered
                .push_back(TradeLiveUpdate::Append(trade.clone()));
        }
        Ok(())
    }

    pub(super) fn accept_indexed_trade(
        &mut self,
        mutation: &IndexedTradeMutation,
    ) -> Result<(), String> {
        if self.history_state == LiveHistoryState::Ready {
            if mutation.kind == super::IndexedTradeKind::New
                && mutation.trade.as_ref().is_some_and(|trade| {
                    trade
                        .metadata
                        .timestamps
                        .exchange_unix_nanos
                        .is_some_and(|time| time > self.history_boundary_unix_nanos)
                })
            {
                self.last_trade_at = Some(Instant::now());
            }
            if self.apply_indexed_trade(mutation)? {
                self.dirty = true;
            }
        } else if self.buffered.len() == LIVE_BUFFER_CAPACITY {
            return Err("Trade-built history/live buffer overflowed".to_string());
        } else {
            self.buffered
                .push_back(TradeLiveUpdate::Indexed(mutation.clone()));
        }
        Ok(())
    }

    fn apply_update(&mut self, update: &TradeLiveUpdate) -> Result<bool, String> {
        match update {
            TradeLiveUpdate::Append(trade) => self.apply_trade(trade),
            TradeLiveUpdate::Indexed(mutation) => self.apply_indexed_trade(mutation),
        }
    }

    fn apply_indexed_trade(&mut self, mutation: &IndexedTradeMutation) -> Result<bool, String> {
        if self
            .last_trade_sequence
            .is_some_and(|sequence| mutation.source_sequence <= sequence)
        {
            return Ok(false);
        }
        let previous = self.indexed_overlay.get(&mutation.index).cloned();
        match mutation.kind {
            super::IndexedTradeKind::New => {
                if previous.is_some() {
                    return Err("Indexed trade identity was reused".to_string());
                }
            }
            super::IndexedTradeKind::Correction | super::IndexedTradeKind::Cancel => {
                if previous.is_none() {
                    return Err(
                        "Indexed trade correction fell outside the retained overlay".to_string()
                    );
                }
            }
        }
        if let Some(trade) = mutation.trade.as_ref() {
            trade.validate().map_err(|error| error.to_string())?;
            if trade.metadata.provider_id != self.series.provider_id
                || trade.metadata.instrument_id != self.series.instrument_id
                || trade.metadata.entitlement_id != self.series.entitlement_id
            {
                return Ok(false);
            }
            let exchange_nanos = trade
                .metadata
                .timestamps
                .exchange_unix_nanos
                .ok_or_else(|| "Trade-built live trade has no exchange timestamp".to_string())?;
            if mutation.kind == super::IndexedTradeKind::New
                && exchange_nanos <= self.history_boundary_unix_nanos
            {
                self.last_trade_sequence = Some(mutation.source_sequence);
                return Ok(false);
            }
            if let Some(session_generation) = self.live_session_generation
                && session_generation != trade.metadata.session_generation
            {
                return Err("Trade-built live session generation changed".to_string());
            }
        }
        if self.overlay_base.is_none() {
            self.overlay_base = self.bars.last().copied();
        }
        match mutation.kind {
            super::IndexedTradeKind::New | super::IndexedTradeKind::Correction => {
                let trade = mutation
                    .trade
                    .clone()
                    .ok_or_else(|| "Indexed trade mutation has no trade".to_string())?;
                if previous.is_none()
                    && self.indexed_overlay.len() == super::MAXIMUM_TRADE_BAR_OVERLAY
                {
                    return Err("Indexed trade overlay exceeded its bound".to_string());
                }
                self.live_session_generation = Some(trade.metadata.session_generation);
                self.indexed_overlay.insert(mutation.index.clone(), trade);
            }
            super::IndexedTradeKind::Cancel => {
                self.indexed_overlay.remove(&mutation.index);
            }
        }
        self.last_trade_sequence = Some(mutation.source_sequence);
        let changed = self.rebuild_indexed_overlay()?;
        if self.indexed_overlay.is_empty() {
            self.overlay_base = None;
        }
        Ok(changed)
    }

    fn rebuild_indexed_overlay(&mut self) -> Result<bool, String> {
        let base = self
            .overlay_base
            .ok_or_else(|| "Indexed trade overlay has no authoritative base".to_string())?;
        if matches!(self.cadence, TradeLiveCadence::Tick { .. }) {
            return Err("Indexed corrections are unsupported for tick bars".to_string());
        }
        let previous = self.bars.clone();
        let base_position = previous
            .iter()
            .position(|bar| {
                bar.source_sequence == base.source_sequence
                    && bar.exchange_timestamp_unix_nanos == base.exchange_timestamp_unix_nanos
            })
            .ok_or_else(|| "Indexed trade overlay base is no longer retained".to_string())?;
        let previous_overlay = previous[base_position..].to_vec();
        let mut trades = self.indexed_overlay.values().cloned().collect::<Vec<_>>();
        trades.sort_unstable_by(|left, right| {
            (
                left.metadata.timestamps.exchange_unix_nanos,
                left.metadata.source_sequence,
                &left.trade_id,
            )
                .cmp(&(
                    right.metadata.timestamps.exchange_unix_nanos,
                    right.metadata.source_sequence,
                    &right.trade_id,
                ))
        });
        let rebuilt = self.aggregate_indexed_trades(base, &trades)?;
        if previous_overlay.iter().any(|old| {
            !rebuilt
                .iter()
                .any(|new| new.exchange_timestamp_unix_nanos == old.exchange_timestamp_unix_nanos)
        }) {
            return Err("Indexed correction removed a published candle".to_string());
        }
        let newest_timestamp = rebuilt
            .last()
            .map_or(base.exchange_timestamp_unix_nanos, |bar| {
                bar.exchange_timestamp_unix_nanos
            });
        let mut changed = false;
        for bar in rebuilt.iter().copied() {
            let prior = previous_overlay.iter().find(|candidate| {
                candidate.exchange_timestamp_unix_nanos == bar.exchange_timestamp_unix_nanos
            });
            if prior == Some(&bar) {
                continue;
            }
            changed = true;
            if prior.is_some()
                && (bar.exchange_timestamp_unix_nanos < newest_timestamp
                    || self.forming_tail_sequence != Some(bar.source_sequence))
            {
                enqueue_bar_transition(
                    &mut self.pending_corrections,
                    bar,
                    "Trade-built correction buffer overflowed",
                )?;
            } else {
                enqueue_bar_transition(
                    &mut self.pending_publications,
                    bar,
                    "Trade-built live publication buffer overflowed",
                )?;
            }
        }
        let mut retained = previous[..base_position].to_vec();
        retained.extend(rebuilt);
        if retained.len() > LIVE_HANDOFF_HISTORY_BARS {
            // Never evict the authoritative overlay base. The overlay itself
            // is separately bounded, so retaining its generated bars remains
            // bounded even during a long provider-candle outage.
            let excess = (retained.len() - LIVE_HANDOFF_HISTORY_BARS).min(base_position);
            retained.drain(..excess);
        }
        self.bars = retained;
        Ok(changed)
    }

    fn aggregate_indexed_trades(
        &self,
        base: MarketBar,
        trades: &[MarketTrade],
    ) -> Result<Vec<MarketBar>, String> {
        let mut rebuilt = vec![base];
        for trade in trades {
            let exchange_nanos = trade
                .metadata
                .timestamps
                .exchange_unix_nanos
                .ok_or_else(|| "Trade-built live trade has no exchange timestamp".to_string())?;
            let last = *rebuilt
                .last()
                .ok_or_else(|| "Trade-built live handoff has no history".to_string())?;
            let next = match self.cadence {
                TradeLiveCadence::Fixed { seconds } => {
                    let trade_seconds = exchange_nanos.div_euclid(1_000_000_000);
                    if trade_seconds < last.exchange_timestamp_seconds {
                        return Err("Indexed trade precedes the retained overlay".to_string());
                    }
                    let elapsed = trade_seconds - last.exchange_timestamp_seconds;
                    if elapsed < seconds {
                        updated_rithmic_bar(last, trade, last.exchange_timestamp_unix_nanos)?
                    } else {
                        let intervals = elapsed.div_euclid(seconds);
                        let start_seconds = last
                            .exchange_timestamp_seconds
                            .checked_add(intervals.saturating_mul(seconds))
                            .ok_or_else(|| "Trade-built live timestamp overflowed".to_string())?;
                        started_rithmic_bar(
                            last,
                            trade,
                            start_seconds.checked_mul(1_000_000_000).ok_or_else(|| {
                                "Trade-built live timestamp overflowed".to_string()
                            })?,
                        )?
                    }
                }
                TradeLiveCadence::Calendar { calendar, period } => {
                    let last_bucket = calendar.bucket(last.exchange_timestamp_seconds, period);
                    let trade_bucket =
                        calendar.bucket(exchange_nanos.div_euclid(1_000_000_000), period);
                    if last_bucket == trade_bucket {
                        updated_rithmic_bar(last, trade, exchange_nanos)?
                    } else {
                        started_rithmic_bar(last, trade, exchange_nanos)?
                    }
                }
                TradeLiveCadence::Tick { .. } => unreachable!(),
            };
            if next.source_sequence == last.source_sequence {
                if let Some(forming) = rebuilt.last_mut() {
                    *forming = next;
                }
            } else {
                rebuilt.push(next);
            }
        }
        Ok(rebuilt)
    }

    pub(super) fn accept_provider_candle(
        &mut self,
        symbol: &str,
        candle: super::ProviderCandle,
    ) -> Result<(), String> {
        if self.candle_symbol.as_deref() != Some(symbol) {
            return Ok(());
        }
        // A covering history request owns the seam while this handoff waits.
        // Its snapshot includes provider candles up to the new boundary; an
        // independent live-channel replay cannot safely revise the old seed.
        if self.history_state != LiveHistoryState::Ready {
            return Ok(());
        }
        let count = candle
            .trade_count
            .ok_or_else(|| "Trade-built provider candle has no trade count".to_string())?;
        let watermark = candle
            .trade_watermark
            .ok_or_else(|| "Trade-built provider candle has no ingestion watermark".to_string())?;
        candle.bar.validate().map_err(|error| error.to_string())?;
        if self.provider_candle_timestamp == Some(candle.bar.exchange_timestamp_unix_nanos)
            && self
                .provider_candle_count
                .is_some_and(|previous| count < previous)
        {
            return Ok(());
        }

        let matching = self.bars.iter().position(|bar| {
            bar.exchange_timestamp_unix_nanos == candle.bar.exchange_timestamp_unix_nanos
        });
        let mut authoritative = candle.bar;
        if let Some(position) = matching {
            authoritative.source_sequence = self.bars[position].source_sequence;
        } else {
            return self.append_provider_candle(authoritative, count);
        }

        let eligible = self
            .indexed_overlay
            .iter()
            .filter(|(_, trade)| {
                trade.metadata.source_sequence <= watermark
                    && self
                        .trade_belongs_to_candle(authoritative.exchange_timestamp_unix_nanos, trade)
            })
            .map(|(index, trade)| (index.clone(), trade.clone()))
            .collect::<Vec<_>>();
        let overlay_uses_candle = self.overlay_base.is_some_and(|base| {
            base.exchange_timestamp_unix_nanos <= authoritative.exchange_timestamp_unix_nanos
        });
        if overlay_uses_candle && self.has_older_unreconciled_trade(&authoritative) {
            // Candle and TimeAndSale arrive on independent DXLink channels.
            // A newer candle may overtake the one that will close this older
            // overlay bucket. Keep the trade-built bar and wait for that
            // covering candle; the bounded overlay still recovers explicitly
            // if the expected correction never arrives.
            return Ok(());
        }
        let baseline_count = if self.provider_candle_timestamp
            == Some(authoritative.exchange_timestamp_unix_nanos)
        {
            self.provider_candle_count
        } else {
            overlay_uses_candle.then_some(0)
        };
        if overlay_uses_candle
            && !provider_candle_covers_trades(&authoritative, count, baseline_count, &eligible)?
        {
            // Candle and TimeAndSale are independent DXLink channels. A candle
            // that has not incorporated every locally observed print through
            // its ingestion watermark is stale and must not move the chart
            // behind the trade-built forming bar.
            return Ok(());
        }

        let position = matching.ok_or_else(|| "Provider candle match disappeared".to_string())?;
        if overlay_uses_candle {
            self.overlay_base = Some(authoritative);
            for (index, _) in &eligible {
                self.indexed_overlay.remove(index);
            }
            if self.rebuild_indexed_overlay()? {
                self.dirty = true;
            }
            if self.indexed_overlay.is_empty() {
                self.overlay_base = None;
            }
        } else if self.bars[position] != authoritative {
            self.bars[position] = authoritative;
            if position + 1 < self.bars.len()
                || self.forming_tail_sequence != Some(authoritative.source_sequence)
            {
                enqueue_bar_transition(
                    &mut self.pending_corrections,
                    authoritative,
                    "Trade-built correction buffer overflowed",
                )?;
            } else {
                enqueue_bar_transition(
                    &mut self.pending_publications,
                    authoritative,
                    "Trade-built live publication buffer overflowed",
                )?;
            }
            self.dirty = true;
        }
        self.provider_candle_count = Some(count);
        self.provider_candle_timestamp = Some(authoritative.exchange_timestamp_unix_nanos);
        Ok(())
    }

    fn has_older_unreconciled_trade(&self, candle: &MarketBar) -> bool {
        self.indexed_overlay.values().any(|trade| {
            trade
                .metadata
                .timestamps
                .exchange_unix_nanos
                .is_some_and(|timestamp| timestamp < candle.exchange_timestamp_unix_nanos)
        })
    }

    fn append_provider_candle(
        &mut self,
        mut authoritative: MarketBar,
        count: u64,
    ) -> Result<(), String> {
        let last = self
            .bars
            .last()
            .copied()
            .ok_or_else(|| "Trade-built live handoff has no history".to_string())?;
        if authoritative.exchange_timestamp_unix_nanos <= last.exchange_timestamp_unix_nanos {
            return Err("Provider candle no longer overlaps retained live state".to_string());
        }
        if self.gap_policy == CandleGapPolicy::Contiguous
            && let TradeLiveCadence::Fixed { seconds } = self.cadence
            && authoritative.exchange_timestamp_seconds
                != last.exchange_timestamp_seconds.saturating_add(seconds)
        {
            return Err("Provider candle is discontinuous".to_string());
        }
        if !self.indexed_overlay.is_empty() {
            return Err("Provider candle skipped the retained trade overlay".to_string());
        }
        authoritative.source_sequence = last
            .source_sequence
            .checked_add(1)
            .ok_or_else(|| "Trade-built live sequence overflowed".to_string())?;
        self.forming_tail_sequence = Some(authoritative.source_sequence);
        self.bars.push(authoritative);
        enqueue_bar_transition(
            &mut self.pending_publications,
            authoritative,
            "Trade-built live publication buffer overflowed",
        )?;
        self.provider_candle_count = Some(count);
        self.provider_candle_timestamp = Some(authoritative.exchange_timestamp_unix_nanos);
        self.dirty = true;
        Ok(())
    }

    fn trade_belongs_to_candle(&self, open_nanos: i64, trade: &MarketTrade) -> bool {
        let Some(exchange_nanos) = trade.metadata.timestamps.exchange_unix_nanos else {
            return false;
        };
        match self.cadence {
            TradeLiveCadence::Fixed { seconds } => {
                let open_seconds = open_nanos.div_euclid(1_000_000_000);
                let trade_seconds = exchange_nanos.div_euclid(1_000_000_000);
                trade_seconds >= open_seconds
                    && trade_seconds < open_seconds.saturating_add(seconds)
            }
            TradeLiveCadence::Calendar { calendar, period } => {
                calendar.bucket(open_nanos.div_euclid(1_000_000_000), period)
                    == calendar.bucket(exchange_nanos.div_euclid(1_000_000_000), period)
            }
            TradeLiveCadence::Tick { .. } => false,
        }
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
            TradeLiveCadence::Fixed { seconds } => {
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
            TradeLiveCadence::Tick {
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
            TradeLiveCadence::Calendar { calendar, period } => {
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

fn provider_candle_covers_trades(
    candle: &MarketBar,
    count: u64,
    baseline_count: Option<u64>,
    trades: &[(String, MarketTrade)],
) -> Result<bool, String> {
    let Some(expected_count) = baseline_count.and_then(|baseline| {
        u64::try_from(trades.len())
            .ok()
            .and_then(|trades| baseline.checked_add(trades))
    }) else {
        return Ok(false);
    };
    if count != expected_count {
        return Ok(false);
    }
    let Some((_, latest)) = trades
        .iter()
        .max_by(|(left_index, left), (right_index, right)| {
            (
                left.metadata.timestamps.exchange_unix_nanos,
                left.metadata.source_sequence,
                left_index,
            )
                .cmp(&(
                    right.metadata.timestamps.exchange_unix_nanos,
                    right.metadata.source_sequence,
                    right_index,
                ))
        })
    else {
        return Ok(true);
    };
    let mut quantity = 0_i64;
    for (_, trade) in trades {
        if trade.price > candle.high || trade.price < candle.low {
            return Ok(false);
        }
        quantity = quantity
            .checked_add(trade.quantity)
            .ok_or_else(|| "Trade-built reconciliation volume overflowed".to_string())?;
    }
    Ok(candle.close == latest.price && candle.volume >= quantity)
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

impl CandleLiveHandoff {
    pub(super) fn new(
        series: BarSeriesKey,
        generation: ProviderGeneration,
        wire_coin: String,
        interval: String,
        gap_policy: CandleGapPolicy,
    ) -> Self {
        Self {
            series,
            generation,
            gap_policy,
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
            if self
                .forming
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
        let forming = forming.map(|forming| forming.bar).filter(|forming| {
            bars.last()
                .is_none_or(|last| last.source_sequence < forming.source_sequence)
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
                forming,
                "Provider candle publication buffer overflowed",
            )?;
        }
        self.dirty = !self.pending_publications.is_empty();
        let buffered = std::mem::take(&mut self.buffered);
        self.history_state = LiveHistoryState::Ready;
        for candle in &buffered {
            if self
                .forming
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

    pub(super) fn take_publication(&mut self) -> Option<LiveSeriesPublication> {
        if !self.connected || self.history_state != LiveHistoryState::Ready || !self.dirty {
            return None;
        }
        let tails = self.pending_publications.drain(..).collect::<Vec<_>>();
        self.dirty = false;
        (!tails.is_empty()).then_some(LiveSeriesPublication::Tails(tails))
    }

    pub(super) fn accept_bar(&mut self, bar: MarketBar) -> Result<(), String> {
        if self.history_state == LiveHistoryState::Ready {
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
        let active = self.forming.or_else(|| self.bars.last().copied());
        if self.gap_policy == CandleGapPolicy::Contiguous
            && let (Some(active), Some(duration)) = (active, self.series.period.duration_nanos())
            && candle.exchange_timestamp_unix_nanos > active.exchange_timestamp_unix_nanos
            && active.exchange_timestamp_unix_nanos.checked_add(duration)
                != Some(candle.exchange_timestamp_unix_nanos)
        {
            return Err("Provider candle has a time gap".to_string());
        }
        let sequence = match &self.forming {
            Some(forming)
                if forming.exchange_timestamp_unix_nanos
                    == candle.exchange_timestamp_unix_nanos =>
            {
                forming.source_sequence
            }
            Some(forming) => forming
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
        merge_live_candle(&mut self.bars, &mut self.forming, bar)?;
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

    pub(super) fn publish_non_bar_study_change(
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

    fn clear_provider_display_depth(&mut self, provider: &str) {
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
                        .series_snapshot(series)
                        .and_then(|snapshot| snapshot.bars.last().copied());
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

    fn install_completed_live_correction(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        price_scale: u8,
        quantity_scale: u8,
        bar: MarketBar,
    ) -> Result<(), String> {
        if self
            .detached_history
            .contains(&(series.clone(), generation))
        {
            return Ok(());
        }
        if self.engine.series_snapshot(series).is_some_and(|snapshot| {
            snapshot.forming
                && snapshot.bars.last().is_some_and(|tail| {
                    tail.source_sequence == bar.source_sequence
                        && tail.exchange_timestamp_unix_nanos == bar.exchange_timestamp_unix_nanos
                })
        }) {
            return self.install_live_tails(
                series,
                generation,
                price_scale,
                quantity_scale,
                vec![bar],
            );
        }
        let exchange_timestamp_unix_nanos = bar.exchange_timestamp_unix_nanos;
        let publications = self
            .engine
            .replace_realtime_completed_bar(generation, series, price_scale, quantity_scale, bar)
            .map_err(|error| {
                let canonical = self
                    .engine
                    .series_snapshot(series)
                    .and_then(|snapshot| snapshot.bars.last().copied());
                format!(
                    "{error}; completed correction sequence={} time={} canonical_tail={canonical:?}",
                    bar.source_sequence, bar.exchange_timestamp_unix_nanos
                )
            })?;
        self.publish_installed_history(&publications);
        match self.execute_study_bar_change(series, exchange_timestamp_unix_nanos) {
            Ok(batch) => {
                self.publish_study_outputs(&batch.executed);
                for error in batch.errors {
                    eprintln!("Aeris live study execution failed: {error}");
                }
            }
            Err(error) => eprintln!("Aeris live study execution failed: {error}"),
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn rithmic_realtime_demand(&self) -> Result<RithmicRealtimeDemand, String> {
        self.provider_demand("rithmic").rithmic_wire()
    }

    pub(super) fn provider_demand(&self, provider_id: &str) -> ProviderDemand {
        let mut instruments = BTreeMap::<String, ProviderInstrumentDemand>::new();
        let mut candle_series = Vec::new();
        let mut missing_catalog = false;
        for series in self
            .series_live
            .trade_keys()
            .chain(self.series_live.candle_keys())
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
                .is_some_and(|descriptor| descriptor.candle_wire_interval.is_some())
            {
                candle_series.push((series.clone(), streams, instrument.clone()));
            }
            instruments
                .entry(instrument.instrument_id.clone())
                .and_modify(|entry| entry.streams = entry.streams.union(streams))
                .or_insert(ProviderInstrumentDemand {
                    instrument,
                    streams,
                    alert_trades: false,
                    alert_instrument: None,
                    display_depth: false,
                });
        }
        let alert_overrides_instrument = self
            .providers
            .descriptor(provider_id)
            .is_some_and(|descriptor| descriptor.alert_overrides_instrument);
        for instrument in self.price_alerts.active_instruments(provider_id) {
            instruments
                .entry(instrument.instrument_id.clone())
                .and_modify(|entry| {
                    entry.alert_trades = true;
                    entry.alert_instrument = Some(instrument.clone());
                    if alert_overrides_instrument {
                        entry.instrument = instrument.clone();
                    }
                })
                .or_insert(ProviderInstrumentDemand {
                    alert_instrument: Some(instrument.clone()),
                    instrument,
                    streams: StreamRequirements::NONE,
                    alert_trades: true,
                    display_depth: false,
                });
        }
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
        let neutral = self.provider_demand("rithmic");
        let demand = neutral.rithmic_wire()?;
        if demand.instruments.is_empty() {
            return Ok(());
        }
        if self.session("rithmic").accepted.as_ref() == Some(&neutral)
            && self.session("rithmic").pending.is_none()
        {
            return Ok(());
        }
        if self
            .providers
            .send_control("rithmic", super::ProviderControl::Demand(neutral.clone()))?
        {
            let session = self.session_mut("rithmic");
            session.pending = None;
            session.accepted = Some(neutral);
        } else {
            self.session_mut("rithmic").pending = Some(neutral);
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
        match descriptor.live_model {
            super::LiveModel::TradeBuilt => {
                if !self.series_live.trade_contains_key(series) {
                    let generation = self.provider_generation_for_series(series)?;
                    let instrument = self
                        .catalog
                        .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                        .ok_or_else(|| descriptor.instrument_missing_detail.to_string())?
                        .clone();
                    let candle = descriptor
                        .candle_wire_interval
                        .map(|wire_interval| {
                            wire_interval(series.period).map(|interval| {
                                (
                                    descriptor.gap_policy,
                                    format!("{}{{={interval}}}", instrument.provider_symbol),
                                )
                            })
                        })
                        .transpose()?;
                    if let Some(mut handoff) =
                        TradeLiveHandoff::new(series, generation, &instrument.venue_id, candle)
                    {
                        handoff.connected = connected;
                        self.series_live.insert_trade(series.clone(), handoff);
                    }
                }
                match descriptor.trade_demand_policy {
                    super::TradeDemandPolicy::Immediate => self.send_rithmic_demand()?,
                    super::TradeDemandPolicy::SessionManaged => {
                        self.flush_session_managed_demand(descriptor.id);
                    }
                }
            }
            super::LiveModel::ProviderCandles => {
                if descriptor.candle_demand_policy
                    == super::CandleDemandPolicy::ReconcileAfterSelection
                {
                    // Newest-wins controls may replace a queued idle Stop without a
                    // Disconnected event. Retire that fence when demand resumes.
                    self.session_mut(&series.provider_id).stop_pending = None;
                }
                if !self.series_live.candle_contains_key(series) {
                    let generation = self.provider_generation_for_series(series)?;
                    let instrument = self.candle_instrument(series)?.clone();
                    let interval =
                        descriptor.candle_wire_interval.ok_or_else(|| {
                            "provider candle interval is unavailable".to_string()
                        })?(series.period)?;
                    let mut handoff = CandleLiveHandoff::new(
                        series.clone(),
                        generation,
                        instrument.provider_symbol,
                        interval,
                        descriptor.gap_policy,
                    );
                    handoff.connected = connected;
                    self.series_live.insert_candle(series.clone(), handoff);
                }
                if descriptor.candle_demand_policy
                    == super::CandleDemandPolicy::ReconcileAfterSelection
                {
                    self.session_mut(&series.provider_id).demand_dirty = true;
                }
            }
        }
        Ok(())
    }

    pub(super) fn handle_provider_event(&mut self, event: ProviderEvent) {
        let ProviderEvent {
            provider,
            generation,
            kind,
        } = event;
        let Some(session) = self.sessions.get(provider) else {
            return;
        };
        if session.suspended
            || generation < session.generation_floor
            || (session
                .stop_pending
                .is_some_and(|pending| pending.0.get() == generation)
                && !matches!(kind, ProviderEventKind::Disconnected(_)))
        {
            return;
        }
        let Some(descriptor) = self.providers.descriptor(provider) else {
            return;
        };
        match kind {
            ProviderEventKind::Connecting => match descriptor.live_model {
                super::LiveModel::TradeBuilt => self.rithmic_connecting(provider, generation),
                super::LiveModel::ProviderCandles => {
                    self.candle_provider_connecting(provider, generation);
                }
            },
            ProviderEventKind::Connected | ProviderEventKind::Heartbeat(_) => {
                match descriptor.live_model {
                    super::LiveModel::TradeBuilt => self.rithmic_online(provider, generation),
                    super::LiveModel::ProviderCandles => {
                        self.candle_provider_online(provider, generation);
                    }
                }
            }
            ProviderEventKind::Recovering {
                detail,
                provider_detail,
            } => {
                match descriptor.live_model {
                    super::LiveModel::TradeBuilt => {
                        self.rithmic_recovering(provider, generation, detail);
                    }
                    super::LiveModel::ProviderCandles => {
                        self.candle_provider_recovering(provider, generation, detail);
                    }
                }
                if let Some(provider_detail) = provider_detail {
                    self.broadcast_provider_for(provider, Some(&provider_detail));
                }
            }
            ProviderEventKind::Failed(detail) => self.rithmic_failed(provider, generation, &detail),
            ProviderEventKind::Disconnected(reason) => {
                self.handle_provider_disconnected(provider, generation, descriptor, reason);
            }
            ProviderEventKind::Trades(batch) => {
                self.accept_provider_trades(provider, generation, descriptor.live_model, &batch);
            }
            ProviderEventKind::IndexedTrades {
                instrument,
                changes,
            } => self.accept_indexed_trades(provider, generation, &instrument, &changes),
            ProviderEventKind::TapeBackfill {
                instrument,
                trades,
                truncated,
            } => self.accept_tape_backfill(provider, generation, &instrument, &trades, truncated),
            ProviderEventKind::Quote(quote) => self.provider_quote(provider, generation, &quote),
            ProviderEventKind::Depth(depth) => self.provider_depth(provider, generation, &depth),
            ProviderEventKind::Candle { symbol, candle } => {
                self.accept_provider_candle(provider, generation, &symbol, candle, descriptor);
            }
            ProviderEventKind::CandleRecovery(symbol) => {
                self.recover_provider_candle(provider, generation, &symbol);
            }
            ProviderEventKind::TradeRecovery(instrument) => {
                self.request_indexed_trade_recovery(
                    provider,
                    generation,
                    &instrument.instrument_id,
                );
            }
        }
    }

    pub(super) fn flush_session_managed_demand(&mut self, provider: &'static str) {
        if !self.sessions.contains_key(provider) {
            return;
        }
        if let Some(ready) = self.session(provider).authorization {
            if self
                .providers
                .send_control(
                    provider,
                    super::ProviderControl::AuthorizationChanged(ready),
                )
                .is_ok_and(|sent| sent)
            {
                self.session_mut(provider).authorization = None;
            }
            return;
        }
        if self.session(provider).suspended {
            return;
        }
        let demand = self.provider_demand(provider);
        if self.session(provider).accepted.as_ref() == Some(&demand) {
            return;
        }
        if demand.candle_series.is_empty() && demand.instruments.is_empty() {
            if self.session(provider).accepted.is_some()
                && self
                    .providers
                    .send_control(provider, super::ProviderControl::Stop)
                    .is_ok_and(|sent| sent)
            {
                self.session_mut(provider).accepted = Some(demand);
            }
            return;
        }
        if self
            .providers
            .send_control(provider, super::ProviderControl::Demand(demand.clone()))
            .is_ok_and(|sent| sent)
        {
            self.session_mut(provider).accepted = Some(demand);
        }
    }

    fn accept_tape_backfill(
        &mut self,
        provider: &str,
        generation: u64,
        instrument: &InstallProviderInstrument,
        trades: &[MarketTrade],
        truncated: bool,
    ) {
        if self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation)
            .map(|current| current.0.get())
            != Some(generation)
        {
            return;
        }
        if let Some(book) = self
            .order_books
            .get_mut(&(provider.to_string(), instrument.instrument_id.clone()))
        {
            for trade in trades {
                if book
                    .replace_indexed_trade(&trade.trade_id, Some(trade), true)
                    .is_err()
                {
                    self.request_indexed_trade_recovery(
                        provider,
                        generation,
                        &instrument.instrument_id,
                    );
                    return;
                }
            }
        }
        if let Some(observed) = trades
            .iter()
            .filter_map(|trade| trade.metadata.timestamps.exchange_unix_nanos)
            .max()
        {
            self.publish_non_bar_study_change(
                provider,
                &instrument.instrument_id,
                &instrument.entitlement_id,
                MarketStream::Trades,
                observed,
            );
        }
        self.broadcast_order_book(provider, &instrument.instrument_id);
        if truncated {
            self.broadcast_provider_for(provider, Some("Available tick history is partial"));
        }
    }

    fn recover_provider_candle(&mut self, provider: &str, generation: u64, symbol: &str) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        let series = self
            .series_live
            .candle_iter()
            .filter(|(series, live)| {
                series.provider_id == provider
                    && live.generation == generation
                    && format!("{}{{={}}}", live.wire_coin, live.interval) == symbol
            })
            .map(|(series, _)| series.clone())
            .chain(
                self.series_live
                    .trade_keys()
                    .filter(|series| series.provider_id == provider)
                    .filter_map(|series| {
                        let live = self.series_live.trade(series)?;
                        (live.generation == generation
                            && live.candle_symbol.as_deref() == Some(symbol))
                        .then(|| series.clone())
                    }),
            )
            .collect::<Vec<_>>();
        for series in series {
            if self.series_live.trade_contains_key(&series) {
                self.rithmic_series_recovering(
                    &series,
                    generation,
                    FailureStage::Aggregation,
                    "Candle correction is reloading the available history",
                );
            } else {
                self.candle_series_recovering(
                    &series,
                    generation,
                    FailureStage::Aggregation,
                    "Candle correction is reloading the available history",
                );
            }
        }
    }

    fn accept_indexed_trades(
        &mut self,
        provider: &str,
        generation: u64,
        instrument: &InstallProviderInstrument,
        changes: &[IndexedTradeMutation],
    ) {
        if self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation)
            .map(|current| current.0.get())
            != Some(generation)
        {
            return;
        }
        let mut failed = false;
        if let Some(book) = self
            .order_books
            .get_mut(&(provider.to_string(), instrument.instrument_id.clone()))
        {
            for change in changes {
                if book
                    .replace_indexed_trade(&change.index, change.trade.as_ref(), false)
                    .is_err()
                {
                    failed = true;
                    break;
                }
            }
        }
        if failed {
            self.request_indexed_trade_recovery(provider, generation, &instrument.instrument_id);
            return;
        }
        let Ok(provider_generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        let failed_series = self
            .series_live
            .trade_iter_mut()
            .filter(|(series, live)| {
                series.provider_id == provider
                    && live.generation == provider_generation
                    && live.connected
                    && series.instrument_id == instrument.instrument_id
                    && series.entitlement_id == instrument.entitlement_id
            })
            .filter_map(|(series, live)| {
                match changes
                    .iter()
                    .try_for_each(|change| live.accept_indexed_trade(change))
                {
                    Ok(()) => None,
                    Err(error) => {
                        if live.history_state != LiveHistoryState::AwaitingHistory {
                            eprintln!(
                                "Aeris market indexed trade rejected for {}: {error}",
                                series.instrument_id
                            );
                        }
                        Some(series.clone())
                    }
                }
            })
            .collect::<Vec<_>>();
        if !failed_series.is_empty() {
            for series in failed_series {
                self.rithmic_series_recovering(
                    &series,
                    provider_generation,
                    FailureStage::Aggregation,
                    "Indexed trade aggregation requires covering history",
                );
            }
            self.request_indexed_trade_recovery(provider, generation, &instrument.instrument_id);
            return;
        }
        for change in changes {
            if let Some(trade) = &change.trade {
                self.evaluate_price_alert_trade(trade);
            }
        }
        self.request_history_after_indexed_trades(
            provider,
            provider_generation,
            instrument,
            changes,
        );
        if let Some(observed) = changes
            .iter()
            .filter_map(|change| change.trade.as_ref())
            .filter_map(|trade| trade.metadata.timestamps.exchange_unix_nanos)
            .max()
        {
            self.publish_non_bar_study_change(
                provider,
                &instrument.instrument_id,
                &instrument.entitlement_id,
                MarketStream::Trades,
                observed,
            );
        }
        self.broadcast_order_book(provider, &instrument.instrument_id);
    }

    fn request_history_after_indexed_trades(
        &mut self,
        provider: &str,
        generation: ProviderGeneration,
        instrument: &InstallProviderInstrument,
        changes: &[IndexedTradeMutation],
    ) {
        if let Some(newest) = changes
            .iter()
            .filter(|change| change.kind == super::IndexedTradeKind::New)
            .filter_map(|change| change.trade.as_ref())
            .filter_map(|trade| trade.metadata.timestamps.exchange_unix_nanos)
            .max()
        {
            self.request_current_history_after_live_trade(
                provider,
                generation,
                &instrument.instrument_id,
                &instrument.entitlement_id,
                newest,
            );
        }
    }

    fn request_indexed_trade_recovery(
        &mut self,
        provider: &str,
        generation: u64,
        instrument_id: &str,
    ) {
        if let Some(book) = self
            .order_books
            .get_mut(&(provider.to_string(), instrument_id.to_string()))
        {
            book.invalidate_live_market(generation);
            self.broadcast_order_book(provider, instrument_id);
        }
        let Ok(provider_generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        let candidates = self.series_live.trade_keys().cloned().collect::<Vec<_>>();
        let affected = candidates
            .into_iter()
            .filter(|series| {
                series.provider_id == provider
                    && series.instrument_id == instrument_id
                    && self
                        .series_live
                        .trade(series)
                        .is_some_and(|live| live.generation == provider_generation)
            })
            .collect::<Vec<_>>();
        for series in affected {
            self.rithmic_series_recovering(
                &series,
                provider_generation,
                FailureStage::Aggregation,
                "Indexed trade state requires covering history",
            );
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
                self.rithmic_trade(provider, generation, trade);
            }
        } else {
            let mut dirty_books = BTreeSet::new();
            for trade in trades {
                if let Some(instrument_id) = self.candle_provider_trade(provider, generation, trade)
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
        if self.history_current_empty.is_empty() {
            return;
        }
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
        symbol: &str,
        candle: super::ProviderCandle,
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
        let failed = match descriptor.live_model {
            super::LiveModel::TradeBuilt => self
                .series_live
                .trade_iter_mut()
                .filter(|(series, live)| {
                    series.provider_id == provider
                        && live.generation == generation
                        && (!descriptor.candle_requires_connected || live.connected)
                        && live.candle_symbol.as_deref() == Some(symbol)
                })
                .filter_map(
                    |(series, live)| match live.accept_provider_candle(symbol, candle) {
                        Ok(()) => None,
                        Err(error) => {
                            eprintln!(
                                "Aeris market provider candle rejected for {}: {error}",
                                series.instrument_id
                            );
                            Some(series.clone())
                        }
                    },
                )
                .collect::<Vec<_>>(),
            super::LiveModel::ProviderCandles => self
                .series_live
                .candle_iter_mut()
                .filter(|(series, live)| {
                    series.provider_id == provider
                        && live.generation == generation
                        && (!descriptor.candle_requires_connected || live.connected)
                        && format!("{}{{={}}}", live.wire_coin, live.interval) == symbol
                })
                .filter_map(|(series, live)| {
                    live.accept_bar(candle.bar).is_err().then(|| series.clone())
                })
                .collect::<Vec<_>>(),
        };
        for series in failed {
            match descriptor.live_model {
                super::LiveModel::TradeBuilt => self.rithmic_series_recovering(
                    &series,
                    generation,
                    FailureStage::Aggregation,
                    descriptor.candle_correction_detail,
                ),
                super::LiveModel::ProviderCandles => self.candle_series_recovering(
                    &series,
                    generation,
                    FailureStage::Aggregation,
                    descriptor.candle_correction_detail,
                ),
            }
        }
    }

    fn handle_provider_disconnected(
        &mut self,
        provider: &str,
        generation: u64,
        descriptor: super::ProviderDescriptor,
        reason: ProviderDisconnect,
    ) {
        if matches!(reason, ProviderDisconnect::End) {
            self.end_provider_live_session(provider, generation);
            return;
        }
        let stop_pending = self
            .session(provider)
            .stop_pending
            .is_some_and(|pending| pending.0.get() == generation);
        if stop_pending {
            self.handle_idle_stop_overlap(provider, generation, descriptor);
            return;
        }
        match descriptor.live_model {
            super::LiveModel::TradeBuilt => {
                self.session_mut(provider).accepted = None;
                self.session_mut(provider).pending = None;
                let (detail, auto_recover, failed) = match reason {
                    ProviderDisconnect::Recover {
                        detail,
                        auto_recover,
                    } => (detail, auto_recover, false),
                    ProviderDisconnect::Fail {
                        detail,
                        auto_recover,
                    } => (detail, auto_recover, true),
                    ProviderDisconnect::End => return,
                };
                if failed {
                    self.rithmic_failed(provider, generation, detail);
                } else {
                    self.rithmic_recovering(provider, generation, detail);
                }
                if auto_recover
                    && descriptor.recovery_policy
                        == super::ProviderRecoveryPolicy::CoordinatorReissuesDemand
                    && (self
                        .series_live
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
                    | ProviderDisconnect::Fail { detail, .. } => detail,
                    ProviderDisconnect::End => return,
                };
                self.candle_provider_recovering(provider, generation, detail);
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
        let demanded = match descriptor.live_model {
            super::LiveModel::TradeBuilt => {
                self.series_live
                    .trade_keys()
                    .any(|series| series.provider_id == provider)
                    || self.price_alerts.has_active_provider(provider)
            }
            super::LiveModel::ProviderCandles => {
                self.series_live
                    .candle_keys()
                    .any(|series| series.provider_id == provider)
                    || !self.order_books.is_empty()
                    || self.price_alerts.has_active_provider(provider)
            }
        };
        if demanded {
            match descriptor.live_model {
                super::LiveModel::TradeBuilt => {
                    self.session_mut(provider).accepted = None;
                    self.session_mut(provider).pending = None;
                    if descriptor.trade_demand_policy == super::TradeDemandPolicy::Immediate {
                        let _ = self.send_rithmic_demand();
                    }
                    self.rithmic_recovering(
                        provider,
                        generation,
                        "Trade-built realtime restarted after idle-stop overlap",
                    );
                }
                super::LiveModel::ProviderCandles => {
                    self.session_mut(provider).engaged = false;
                    self.session_mut(provider).demand_dirty = true;
                    self.candle_provider_recovering(
                        provider,
                        generation,
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

    pub(super) fn candle_provider_connecting(&mut self, provider: &str, generation: u64) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        let current = self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation);
        if current.is_some_and(|current| generation < current) {
            return;
        }
        if current.is_none_or(|current| generation > current)
            && self
                .engine
                .begin_provider_session(provider, generation)
                .is_err()
        {
            return;
        }
        if current.is_some_and(|current| generation > current) {
            self.clear_provider_display_depth(provider);
            self.invalidate_provider_live_market(provider, generation);
            if self
                .providers
                .descriptor(provider)
                .is_some_and(|descriptor| {
                    descriptor.candle_demand_policy
                        == super::CandleDemandPolicy::ReconcileAfterSelection
                })
            {
                self.session_mut(provider).demand_dirty = true;
            }
            for ((series, _), stop) in &self.history_cancellations {
                if series.provider_id == provider {
                    stop.store(true, Ordering::Release);
                }
            }
            let series = self
                .series_live
                .candle_keys()
                .filter(|series| series.provider_id == provider)
                .cloned()
                .collect::<Vec<_>>();
            for selected in &series {
                if let Some(live) = self.series_live.candle_mut(selected) {
                    live.reset(generation);
                }
                self.broadcast_series_recovery_for(
                    selected,
                    "Provider live session changed; covering history is reloading",
                );
            }
            for selected in series {
                self.request_series_history_recovery(&selected, generation);
            }
        }
        if self
            .engine
            .set_provider_health(provider, generation, ProviderHealth::Connecting)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for(provider, None);
    }

    pub(super) fn candle_provider_online(&mut self, provider: &str, generation: u64) {
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
        if self
            .engine
            .set_provider_health(provider, generation, ProviderHealth::Online)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for(provider, None);
        let mut ready = Vec::new();
        let missing = self
            .series_live
            .candle_iter_mut()
            .filter(|(series, _)| series.provider_id == provider)
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
                self.request_series_history_recovery(&series, generation);
            }
        }
    }

    pub(super) fn candle_provider_trade(
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
            self.candle_provider_recovering(
                provider,
                generation.0.get(),
                "Provider live session identity requires recovery",
            );
            for (series, live) in self.series_live.candle_iter_mut() {
                if series.provider_id != provider {
                    continue;
                }
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

    pub(super) fn candle_series_recovering(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        _stage: FailureStage,
        detail: &str,
    ) {
        if self
            .series_live
            .candle(series)
            .is_some_and(|live| live.history_state == LiveHistoryState::AwaitingHistory)
        {
            return;
        }
        eprintln!(
            "Aeris market series recovery started for {}: {detail}",
            series.instrument_id
        );
        self.cancel_inflight_history_for_live_recovery(series, generation);
        if let Some(live) = self.series_live.candle_mut(series) {
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
            self.request_series_history_recovery(series, generation);
        }
    }

    pub(super) fn candle_provider_recovering(
        &mut self,
        provider: &str,
        generation: u64,
        detail: &'static str,
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
        self.price_alerts.reset_provider_baselines(provider);
        self.clear_provider_display_depth(provider);
        self.session_mut(provider).demand_dirty = true;
        if self.price_alerts.has_active_provider(provider) {
            self.session_mut(provider).demand_dirty = true;
        }
        if self
            .engine
            .set_provider_health(provider, generation, ProviderHealth::Recovering)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for(provider, Some(detail));
        for (series, live) in self.series_live.candle_iter_mut() {
            if series.provider_id != provider {
                continue;
            }
            live.connected = false;
        }
        self.invalidate_provider_live_market(provider, generation);
    }

    pub(super) fn rithmic_connecting(&mut self, provider: &str, generation: u64) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        let current = self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation);
        if current.is_some_and(|current| generation < current) {
            return;
        }
        if current.is_none_or(|current| generation > current)
            && self
                .engine
                .begin_provider_session(provider, generation)
                .is_err()
        {
            return;
        }
        if current.is_some_and(|current| generation > current) {
            self.invalidate_provider_live_market(provider, generation);
            for ((series, _), stop) in &self.history_cancellations {
                if series.provider_id == provider {
                    stop.store(true, Ordering::Release);
                }
            }
            let series = self
                .series_live
                .trade_keys()
                .filter(|series| series.provider_id == provider)
                .cloned()
                .collect::<Vec<_>>();
            for selected in &series {
                if let Some(live) = self.series_live.trade_mut(selected) {
                    live.reset(generation);
                }
                self.broadcast_series_recovery_for(
                    selected,
                    "Rithmic live session changed; covering history is reloading",
                );
            }
            for selected in series {
                self.request_series_history_recovery(&selected, generation);
            }
        }
        if self
            .engine
            .set_provider_health(provider, generation, ProviderHealth::Connecting)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for(provider, None);
    }

    pub(super) fn rithmic_online(&mut self, provider: &str, generation: u64) {
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
        if self
            .engine
            .set_provider_health(provider, generation, ProviderHealth::Online)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for(provider, None);
        let mut ready = Vec::new();
        let missing = self
            .series_live
            .trade_iter_mut()
            .filter(|(series, _)| series.provider_id == provider)
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
                self.request_series_history_recovery(&series, generation);
            }
        }
    }

    pub(super) fn rithmic_trade(&mut self, provider: &str, generation: u64, trade: &MarketTrade) {
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
        if trade.metadata.provider_id != provider
            || trade.metadata.session_generation != generation.0.get()
        {
            self.rithmic_recovering(
                provider,
                generation.0.get(),
                "Rithmic live session identity requires recovery",
            );
            for (series, live) in self.series_live.trade_iter_mut() {
                if series.provider_id != provider {
                    continue;
                }
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
            .series_live
            .trade_iter_mut()
            .filter(|(series, live)| {
                series.provider_id == provider
                    && live.generation == generation
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
            .get_mut(&(provider.to_string(), instrument_id.clone()))
            .is_some_and(|order_book| order_book.accept_recent_trade(trade));
        if trade_changed {
            self.broadcast_order_book(provider, &instrument_id);
            self.publish_non_bar_study_change(
                provider,
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
        if self
            .series_live
            .trade(series)
            .is_some_and(|live| live.history_state == LiveHistoryState::AwaitingHistory)
        {
            return;
        }
        eprintln!(
            "Aeris market series recovery started for {}: {detail}",
            series.instrument_id
        );
        if let Some(live) = self.series_live.trade_mut(series) {
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
            self.request_series_history_recovery(series, generation);
        }
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
            self.mark_provider_alert_demand_changed(&trade.metadata.provider_id);
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

    pub(super) fn rithmic_recovering(
        &mut self,
        provider: &str,
        generation: u64,
        detail: &'static str,
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
        self.price_alerts.reset_provider_baselines(provider);
        if self
            .engine
            .set_provider_health(provider, generation, ProviderHealth::Recovering)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for(provider, Some(detail));
        for (series, live) in self.series_live.trade_iter_mut() {
            if series.provider_id != provider {
                continue;
            }
            live.connected = false;
        }
        for (series, live) in self.series_live.candle_iter_mut() {
            if series.provider_id == provider {
                live.connected = false;
            }
        }
        self.invalidate_provider_live_market(provider, generation);
    }

    pub(super) fn rithmic_failed(&mut self, provider: &str, generation: u64, detail: &str) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        let current_generation = self
            .engine
            .provider_status(provider)
            .and_then(|status| status.generation);
        if current_generation != Some(generation)
            && (current_generation.is_some()
                || self
                    .engine
                    .begin_provider_session(provider, generation)
                    .is_err())
        {
            return;
        }
        self.price_alerts.reset_provider_baselines(provider);
        if self
            .engine
            .set_provider_health(provider, generation, ProviderHealth::Failed)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for(provider, Some(detail));
        for (series, live) in self.series_live.trade_iter_mut() {
            if series.provider_id != provider {
                continue;
            }
            live.connected = false;
        }
        for (series, live) in self.series_live.candle_iter_mut() {
            if series.provider_id == provider {
                live.connected = false;
            }
        }
        self.invalidate_provider_live_market(provider, generation);
    }

    pub(super) fn publish_rithmic_live(&mut self) {
        let ready = self
            .series_live
            .trade_values_mut()
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
                LiveSeriesPublication::CompletedCorrection(bar) => self
                    .install_completed_live_correction(
                        &series,
                        generation,
                        price_scale,
                        quantity_scale,
                        bar,
                    ),
            };
            if let Err(error) = published {
                eprintln!("Aeris engine live publication failed: {error}");
                self.rithmic_series_recovering(
                    &series,
                    generation,
                    FailureStage::Publication,
                    "Live publication requires covering history",
                );
            } else if let Some(live) = self.series_live.trade_mut(&series) {
                live.last_publication_at = Instant::now();
            }
        }
    }

    pub(super) fn recover_stalled_live_series(&mut self) {
        let now = Instant::now();
        let candidates = self
            .series_live
            .trade_iter_mut()
            .filter(|(_, live)| {
                live.connected
                    && live.history_state == LiveHistoryState::Ready
                    && now.duration_since(live.last_publication_at) >= LIVE_SERIES_STALL_THRESHOLD
                    && live.last_trade_at.is_some_and(|trade| {
                        now.duration_since(trade) <= RECENT_INSTRUMENT_TRADE_WINDOW
                    })
            })
            .map(|(series, live)| (series.clone(), live.generation))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return;
        }
        let Ok(now_unix_nanos) = super::tastytrade::now_nanos() else {
            return;
        };
        for (series, generation) in candidates {
            let provider_health = self
                .engine
                .provider_status(&series.provider_id)
                .filter(|status| status.generation == Some(generation))
                .map_or(ProviderHealth::Disconnected, |status| status.health);
            let session_phase = self
                .catalog
                .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                .map_or(aeris_contracts::MarketSessionPhase::Unknown, |instrument| {
                    self.broker_api
                        .market_session_status(
                            &instrument.instrument_id,
                            &instrument.provider_symbol,
                            now_unix_nanos,
                        )
                        .phase
                });
            let stalled = self.series_live.trade(&series).is_some_and(|live| {
                live_series_stalled(
                    now,
                    live.last_publication_at,
                    live.last_trade_at,
                    provider_health,
                    session_phase,
                    live.connected,
                    live.history_state,
                )
            });
            if stalled {
                self.rithmic_series_recovering(
                    &series,
                    generation,
                    FailureStage::Publication,
                    "Live series stalled while instrument trades continued",
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
            self.series_live.trade(series).is_some_and(|live| {
                live.connected && live.history_state == LiveHistoryState::Ready
            }) || self.series_live.candle(series).is_some_and(|live| {
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
    #[cfg(test)]
    pub(super) fn hyperliquid_demand(&self) -> HyperliquidDemand {
        self.provider_demand("hyperliquid").hyperliquid_wire()
    }

    fn hyperliquid_display_depth_demand(
        &self,
        demand: &ProviderDemand,
    ) -> HyperliquidDisplayDepthDemand {
        let Some(provider_generation) = self
            .engine
            .provider_status("hyperliquid")
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
                "hyperliquid".to_string(),
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
        if !self.sessions.contains_key("hyperliquid") {
            return;
        }
        if !self.session("hyperliquid").demand_dirty {
            return;
        }
        let neutral = self.provider_demand("hyperliquid");
        let demand = neutral.hyperliquid_wire();
        let display_demand = self.hyperliquid_display_depth_demand(&neutral);
        let empty = demand.candles.is_empty()
            && demand.trades.is_empty()
            && demand.quotes.is_empty()
            && demand.books.is_empty();
        if empty && !self.session("hyperliquid").engaged {
            self.session_mut("hyperliquid").demand_dirty = false;
            return;
        }
        let raw = self
            .providers
            .send_control("hyperliquid", super::ProviderControl::Demand(neutral));
        let display = self.providers.send_hyperliquid_display_depth(
            HyperliquidDisplayDepthControl::Subscribe(display_demand),
        );
        match (raw, display) {
            (Ok(true), Ok(true)) => {
                self.session_mut("hyperliquid").demand_dirty = false;
                self.session_mut("hyperliquid").engaged = !empty;
            }
            // A full channel retries on the next coordinator tick; the
            // worker coalesces to the newest set.
            (Ok(false), _) | (_, Ok(false)) => {}
            (Err(_), _) | (_, Err(_)) => {
                self.session_mut("hyperliquid").demand_dirty = false;
            }
        }
    }

    pub(super) fn publish_candle_live(&mut self) {
        let ready = self
            .series_live
            .candle_values_mut()
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
                LiveSeriesPublication::CompletedCorrection(bar) => self
                    .install_completed_live_correction(
                        &series,
                        generation,
                        price_scale,
                        quantity_scale,
                        bar,
                    ),
            };
            if let Err(error) = published {
                eprintln!("Aeris engine live publication failed: {error}");
                self.candle_series_recovering(
                    &series,
                    generation,
                    FailureStage::Publication,
                    "Live publication requires covering history",
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
        let retired_trade_providers = self
            .series_live
            .trade_keys()
            .filter(|series| !self.engine.has_subscription(series))
            .map(|series| series.provider_id.clone())
            .collect::<BTreeSet<_>>();
        for series in self
            .series_live
            .trade_keys()
            .filter(|series| !self.engine.has_subscription(series))
        {
            for ((active, _), stop) in &self.history_cancellations {
                if active == series {
                    stop.store(true, Ordering::Release);
                }
            }
        }
        self.series_live
            .retain_trades(|series, _| self.engine.has_subscription(series));
        for provider in retired_trade_providers {
            if self
                .providers
                .descriptor(&provider)
                .is_some_and(|descriptor| {
                    descriptor.trade_demand_policy == super::TradeDemandPolicy::Immediate
                })
            {
                let _ = self.send_rithmic_demand();
            }
        }
        let retired_candle_providers = self
            .series_live
            .candle_keys()
            .filter(|series| !self.engine.has_subscription(series))
            .map(|series| series.provider_id.clone())
            .collect::<BTreeSet<_>>();
        for series in self
            .series_live
            .candle_keys()
            .filter(|series| !self.engine.has_subscription(series))
        {
            for ((active, _), stop) in &self.history_cancellations {
                if active == series {
                    stop.store(true, Ordering::Release);
                }
            }
        }
        self.series_live
            .retain_candles(|series, _| self.engine.has_subscription(series));
        for provider in retired_candle_providers {
            self.session_mut(&provider).demand_dirty = true;
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
        if !self.sessions.contains_key("rithmic") {
            return;
        }
        let Some(neutral) = self.session_mut("rithmic").pending.take() else {
            return;
        };
        match self
            .providers
            .send_control("rithmic", super::ProviderControl::Demand(neutral.clone()))
        {
            Ok(true) => {
                self.session_mut("rithmic").accepted = Some(neutral);
            }
            Ok(false) | Err(_) => {
                self.session_mut("rithmic").pending = Some(neutral);
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
            match descriptor.alert_demand_update {
                super::AlertDemandUpdate::Immediate => {
                    let _ = self.send_rithmic_demand();
                }
                super::AlertDemandUpdate::MarkDirty => {
                    self.session_mut(provider).demand_dirty = true;
                }
                super::AlertDemandUpdate::WorkerManaged => {}
            }
        }
    }

    pub(super) fn stop_realtime_if_idle(&mut self) {
        let descriptors = self
            .providers
            .records
            .values()
            .map(|record| record.descriptor)
            .collect::<Vec<_>>();
        for descriptor in descriptors {
            if descriptor.idle_stop_policy != super::IdleStopPolicy::Coordinator {
                continue;
            }
            let provider = descriptor.id;
            let demanded = match descriptor.live_model {
                super::LiveModel::TradeBuilt => self
                    .series_live
                    .trade_keys()
                    .any(|series| series.provider_id == provider),
                super::LiveModel::ProviderCandles => self
                    .series_live
                    .candle_keys()
                    .any(|series| series.provider_id == provider),
            } || self.price_alerts.has_active_provider(provider);
            let session = self.session(provider);
            let engaged = match descriptor.live_model {
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
            match self
                .providers
                .send_control(provider, super::ProviderControl::Stop)
            {
                Ok(true) => self.session_mut(provider).stop_pending = Some(generation),
                Ok(false) => {}
                Err(_) => {
                    let session = self.session_mut(provider);
                    match descriptor.live_model {
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

    fn tastytrade_minute_series() -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "tastytrade".to_string(),
            instrument_id: "tastytrade:Future:/ESZ26".to_string(),
            entitlement_id: "tastytrade-authorized".to_string(),
            period: BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        }
    }

    fn tastytrade_trade(
        identity: &str,
        sequence: u64,
        timestamp: i64,
        price: i64,
        quantity: i64,
    ) -> MarketTrade {
        MarketTrade {
            metadata: EventMetadata {
                provider_id: "tastytrade".to_string(),
                instrument_id: "tastytrade:Future:/ESZ26".to_string(),
                entitlement_id: "tastytrade-authorized".to_string(),
                source_sequence: sequence,
                session_generation: 1,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(timestamp),
                    provider_unix_nanos: None,
                    received_unix_nanos: timestamp,
                },
            },
            trade_id: identity.to_string(),
            price,
            quantity,
            aggressor: AggressorSide::Unknown,
        }
    }

    fn indexed_trade(
        identity: &str,
        kind: super::super::IndexedTradeKind,
        sequence: u64,
        timestamp: i64,
        price: i64,
        quantity: i64,
    ) -> IndexedTradeMutation {
        IndexedTradeMutation {
            index: identity.to_string(),
            kind,
            source_sequence: sequence,
            trade: (kind != super::super::IndexedTradeKind::Cancel)
                .then(|| tastytrade_trade(identity, sequence, timestamp, price, quantity)),
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
        let mut book = ProviderOrderBook::new(
            ladder_instrument(1),
            super::super::TradeContinuity::Sequence,
            false,
        );
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
    fn indexed_trade_correction_cancellation_and_delayed_history_are_fenced() {
        let mut instrument = ladder_instrument(1);
        instrument.provider = "tastytrade".into();
        let mut book =
            ProviderOrderBook::new(instrument, super::super::TradeContinuity::Indexed, true);
        let mut trade = ladder_trade(1, 20, 1_000_000_000, 20_000, 4, AggressorSide::Buy);
        trade.metadata.provider_id = "tastytrade".into();
        trade.trade_id = "9007199254740993".into();
        book.replace_indexed_trade(&trade.trade_id, Some(&trade), false)
            .unwrap();
        let revision = book.trade_tape_revision;
        book.replace_indexed_trade(&trade.trade_id, Some(&trade), true)
            .unwrap();
        assert_eq!(book.trade_tape_revision, revision);
        let mut corrected = trade.clone();
        corrected.quantity = 7;
        corrected.aggressor = AggressorSide::Sell;
        corrected.metadata.source_sequence = 21;
        book.replace_indexed_trade(&trade.trade_id, Some(&corrected), false)
            .unwrap();
        assert_eq!(book.recent_trades.len(), 1);
        assert_eq!(
            book.traded_volumes[&20_000],
            AggressorTradeVolumes { buy: 0, sell: 7 }
        );
        assert!(book.trade_tape_rewrite_generation > 0);
        book.replace_indexed_trade(&trade.trade_id, Some(&trade), true)
            .unwrap();
        assert_eq!(book.recent_trades[0].trade.quantity, 7);
        book.replace_indexed_trade(&trade.trade_id, None, false)
            .unwrap();
        book.replace_indexed_trade(&trade.trade_id, Some(&trade), true)
            .unwrap();
        assert!(book.recent_trades.is_empty());
        assert!(book.traded_volumes.is_empty());
        let mut older = trade.clone();
        older.trade_id = "10".into();
        older.metadata.source_sequence = 1;
        book.replace_indexed_trade(&older.trade_id, Some(&older), true)
            .unwrap();
        assert_eq!(book.recent_trades[0].trade.metadata.source_sequence, 1);
        assert_eq!(book.last_trade_source_sequence, 21);
        let mut retired = older.clone();
        retired.metadata.session_generation = 2;
        assert!(
            book.replace_indexed_trade(&retired.trade_id, Some(&retired), true)
                .is_err()
        );
    }

    #[test]
    fn recent_ladder_trades_are_side_aware_deduped_retained_and_session_fenced() {
        let mut book = ProviderOrderBook::new(
            ladder_instrument(1),
            super::super::TradeContinuity::Sequence,
            false,
        );
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
    fn retained_trade_tape_enforces_its_hard_item_bound() {
        let mut book = ProviderOrderBook::new(
            ladder_instrument(1),
            super::super::TradeContinuity::Sequence,
            false,
        );
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
        let mut live = TradeLiveHandoff::new(&series, generation(), "CME", None).expect("handoff");
        live.seed(2, 0, &[bar(10, 1_000_000_000, 10_000)], None, None)
            .expect("seed");
        live.connected = true;

        live.accept_trade(&rithmic_trade(1, 2_000_000_000, 10_100))
            .expect("first");
        live.accept_trade(&rithmic_trade(2, 3_000_000_000, 10_200))
            .expect("second");
        live.accept_trade(&rithmic_trade(3, 4_000_000_000, 10_300))
            .expect("third");

        let LiveSeriesPublication::Tails(bars) = live.take_publication().expect("burst") else {
            panic!("tails")
        };
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
        let mut live = TradeLiveHandoff::new(&series, generation(), "CME", None).expect("handoff");
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
    fn tastytrade_indexed_trades_drive_forming_bar_and_apply_correction_and_cancel() {
        let minute = 60_000_000_000;
        let series = tastytrade_minute_series();
        let mut live = TradeLiveHandoff::new(
            &series,
            generation(),
            "CME",
            Some((
                CandleGapPolicy::SessionGapsAllowed,
                "/ESZ26:XCME{=m}".to_string(),
            )),
        )
        .expect("handoff");
        let forming = MarketBar {
            source_sequence: 2,
            exchange_timestamp_seconds: 60,
            exchange_timestamp_unix_nanos: minute,
            open: 10_000,
            high: 10_000,
            low: 10_000,
            close: 10_000,
            volume: 10,
        };
        live.seed(
            2,
            0,
            &[bar(1, 0, 10_000)],
            Some(FormingBar {
                bar: forming,
                trades: Some(10),
            }),
            Some(minute),
        )
        .expect("seed");
        live.connected = true;
        let _ = live.take_publication().expect("seed tail");

        live.accept_indexed_trade(&indexed_trade(
            "trade-1",
            super::super::IndexedTradeKind::New,
            1,
            minute + 1,
            10_100,
            2,
        ))
        .expect("new trade");
        let LiveSeriesPublication::Tails(updated) =
            live.take_publication().expect("trade publication")
        else {
            panic!("tails")
        };
        assert_eq!((updated[0].close, updated[0].volume), (10_100, 12));

        live.accept_indexed_trade(&indexed_trade(
            "trade-1",
            super::super::IndexedTradeKind::Correction,
            2,
            minute + 2,
            10_200,
            3,
        ))
        .expect("correction");
        let LiveSeriesPublication::Tails(corrected) =
            live.take_publication().expect("correction publication")
        else {
            panic!("tails")
        };
        assert_eq!((corrected[0].close, corrected[0].volume), (10_200, 13));

        live.accept_indexed_trade(&indexed_trade(
            "trade-1",
            super::super::IndexedTradeKind::Cancel,
            3,
            minute + 2,
            0,
            0,
        ))
        .expect("cancel");
        let LiveSeriesPublication::Tails(cancelled) =
            live.take_publication().expect("cancel publication")
        else {
            panic!("tails")
        };
        assert_eq!(cancelled, vec![forming]);
    }

    #[test]
    fn tastytrade_candle_reconciliation_ignores_lag_then_replaces_completed_bar() {
        let minute = 60_000_000_000;
        let symbol = "/ESZ26:XCME{=m}";
        let series = tastytrade_minute_series();
        let mut live = TradeLiveHandoff::new(
            &series,
            generation(),
            "CME",
            Some((CandleGapPolicy::SessionGapsAllowed, symbol.to_string())),
        )
        .expect("handoff");
        let forming = MarketBar {
            source_sequence: 2,
            exchange_timestamp_seconds: 60,
            exchange_timestamp_unix_nanos: minute,
            open: 10_000,
            high: 10_000,
            low: 10_000,
            close: 10_000,
            volume: 10,
        };
        live.seed(
            2,
            0,
            &[bar(1, 0, 10_000)],
            Some(FormingBar {
                bar: forming,
                trades: Some(10),
            }),
            Some(minute),
        )
        .expect("seed");
        live.connected = true;
        let _ = live.take_publication().expect("seed tail");
        for mutation in [
            indexed_trade(
                "trade-1",
                super::super::IndexedTradeKind::New,
                1,
                minute + 1,
                10_100,
                2,
            ),
            indexed_trade(
                "trade-2",
                super::super::IndexedTradeKind::New,
                2,
                minute + 2,
                10_200,
                3,
            ),
        ] {
            live.accept_indexed_trade(&mutation).expect("trade");
        }
        let _ = live.take_publication().expect("trade publication");

        live.accept_provider_candle(
            symbol,
            super::super::ProviderCandle {
                bar: candle(minute, 10_000, 10_100, 10_000, 10_100, 12),
                trade_count: Some(11),
                trade_watermark: Some(2),
            },
        )
        .expect("lagging candle is ignored");
        assert!(live.take_publication().is_none());
        assert_eq!(live.bars.last().map(|bar| bar.close), Some(10_200));

        live.accept_indexed_trade(&indexed_trade(
            "trade-3",
            super::super::IndexedTradeKind::New,
            4,
            2 * minute + 1,
            10_300,
            1,
        ))
        .expect("rollover trade");
        let _ = live.take_publication().expect("rollover publication");
        live.accept_provider_candle(
            symbol,
            super::super::ProviderCandle {
                bar: candle(minute, 10_000, 10_250, 9_950, 10_200, 20),
                trade_count: Some(12),
                trade_watermark: Some(2),
            },
        )
        .expect("authoritative candle");
        let LiveSeriesPublication::CompletedCorrection(reconciled) =
            live.take_publication().expect("completed correction")
        else {
            panic!("completed correction")
        };
        assert_eq!(reconciled.source_sequence, 2);
        assert_eq!(
            (reconciled.high, reconciled.low, reconciled.volume),
            (10_250, 9_950, 20)
        );
        assert_eq!(live.bars.last().map(|bar| bar.close), Some(10_300));
    }

    #[test]
    fn newer_provider_candle_waits_for_older_trade_bucket_without_recovery() {
        let minute = 60_000_000_000;
        let symbol = "/ESZ26:XCME{=m}";
        let series = tastytrade_minute_series();
        let mut live = TradeLiveHandoff::new(
            &series,
            generation(),
            "CME",
            Some((CandleGapPolicy::SessionGapsAllowed, symbol.into())),
        )
        .expect("handoff");
        live.seed(
            2,
            0,
            &[bar(1, 0, 10_000)],
            Some(FormingBar {
                bar: candle(minute, 10_000, 10_000, 10_000, 10_000, 10),
                trades: Some(10),
            }),
            Some(minute),
        )
        .expect("seed");
        live.connected = true;
        let _ = live.take_publication();
        for (id, sequence, time) in [("older", 1, minute + 1), ("newer", 2, 2 * minute + 1)] {
            live.accept_indexed_trade(&indexed_trade(
                id,
                super::super::IndexedTradeKind::New,
                sequence,
                time,
                10_100,
                1,
            ))
            .expect("trade");
        }
        let _ = live.take_publication();
        live.accept_provider_candle(
            symbol,
            super::super::ProviderCandle {
                bar: candle(2 * minute, 10_100, 10_100, 10_100, 10_100, 1),
                trade_count: Some(1),
                trade_watermark: Some(2),
            },
        )
        .expect("out-of-order candle waits for older bucket");
        assert_eq!(live.history_state, LiveHistoryState::Ready);
        assert!(live.take_publication().is_none());
    }

    #[test]
    fn pending_append_precedes_correction_for_the_same_bucket() {
        let mut live = TradeLiveHandoff::new(
            &tastytrade_minute_series(),
            generation(),
            "CME",
            Some((
                CandleGapPolicy::SessionGapsAllowed,
                "/ESZ26:XCME{=m}".into(),
            )),
        )
        .expect("handoff");
        live.connected = true;
        live.history_state = LiveHistoryState::Ready;
        live.dirty = true;
        let appended = bar(2, 60, 10_000);
        let corrected = MarketBar {
            close: 10_100,
            ..appended
        };
        live.pending_publications.push_back(appended);
        live.pending_corrections.push_back(corrected);
        let LiveSeriesPublication::Tails(tails) = live.take_publication().expect("append") else {
            panic!("append must precede its correction");
        };
        assert_eq!(tails, vec![appended]);
        let LiveSeriesPublication::CompletedCorrection(correction) =
            live.take_publication().expect("correction")
        else {
            panic!("correction follows accepted append");
        };
        assert_eq!(correction, corrected);
    }

    #[test]
    fn provider_candle_replay_does_not_restart_an_awaiting_history_repair() {
        let symbol = "/ESZ26:XCME{=m}";
        let series = tastytrade_minute_series();
        let mut live = TradeLiveHandoff::new(
            &series,
            generation(),
            "CME",
            Some((CandleGapPolicy::SessionGapsAllowed, symbol.to_string())),
        )
        .expect("handoff");
        live.seed(2, 0, &[bar(1, 0, 10_000)], None, Some(0))
            .expect("seed");
        live.history_state = LiveHistoryState::AwaitingHistory;
        let previous = live.bars.clone();
        live.accept_provider_candle(
            symbol,
            super::super::ProviderCandle {
                bar: candle(-60_000_000_000, 10_000, 10_000, 10_000, 10_000, 1),
                trade_count: Some(1),
                trade_watermark: Some(1),
            },
        )
        .expect("the covering request owns this replay");
        assert_eq!(live.bars, previous);
        assert_eq!(live.history_state, LiveHistoryState::AwaitingHistory);
    }

    #[test]
    fn stalled_series_requires_online_open_market_and_recent_trades() {
        let now = Instant::now();
        let old_publication = now
            .checked_sub(LIVE_SERIES_STALL_THRESHOLD + Duration::from_secs(1))
            .expect("test clock can subtract 31 seconds");
        let recent_trade = now.checked_sub(Duration::from_secs(1));
        assert!(live_series_stalled(
            now,
            old_publication,
            recent_trade,
            ProviderHealth::Online,
            aeris_contracts::MarketSessionPhase::Regular,
            true,
            LiveHistoryState::Ready,
        ));
        for (health, phase, connected, history_state) in [
            (
                ProviderHealth::Recovering,
                aeris_contracts::MarketSessionPhase::Regular,
                true,
                LiveHistoryState::Ready,
            ),
            (
                ProviderHealth::Online,
                aeris_contracts::MarketSessionPhase::Closed,
                true,
                LiveHistoryState::Ready,
            ),
            (
                ProviderHealth::Online,
                aeris_contracts::MarketSessionPhase::Regular,
                false,
                LiveHistoryState::Ready,
            ),
            (
                ProviderHealth::Online,
                aeris_contracts::MarketSessionPhase::Regular,
                true,
                LiveHistoryState::AwaitingHistory,
            ),
        ] {
            assert!(!live_series_stalled(
                now,
                old_publication,
                recent_trade,
                health,
                phase,
                connected,
                history_state,
            ));
        }
        assert!(!live_series_stalled(
            now,
            now.checked_sub(Duration::from_secs(5)).expect("test clock"),
            recent_trade,
            ProviderHealth::Online,
            aeris_contracts::MarketSessionPhase::Regular,
            true,
            LiveHistoryState::Ready,
        ));
        assert!(!live_series_stalled(
            now,
            old_publication,
            now.checked_sub(RECENT_INSTRUMENT_TRADE_WINDOW + Duration::from_secs(1)),
            ProviderHealth::Online,
            aeris_contracts::MarketSessionPhase::Regular,
            true,
            LiveHistoryState::Ready,
        ));
    }

    #[test]
    fn tastytrade_late_new_trade_after_history_boundary_is_ignored() {
        let minute = 60_000_000_000;
        let mut live = TradeLiveHandoff::new(
            &tastytrade_minute_series(),
            generation(),
            "CME",
            Some((
                CandleGapPolicy::SessionGapsAllowed,
                "/ESZ26:XCME{=m}".into(),
            )),
        )
        .expect("handoff");
        live.seed(
            2,
            0,
            &[bar(1, 0, 10_000)],
            Some(FormingBar {
                bar: candle(minute, 10_000, 10_100, 10_000, 10_100, 10),
                trades: Some(10),
            }),
            Some(minute),
        )
        .expect("seed");
        live.connected = true;
        let _ = live.take_publication();
        live.accept_indexed_trade(&indexed_trade(
            "late-trade",
            super::super::IndexedTradeKind::New,
            1,
            minute - 1,
            10_050,
            1,
        ))
        .expect("late print is explicitly rejected by the history boundary");
        assert!(live.take_publication().is_none());
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
        let mut live = CandleLiveHandoff::new(
            series,
            generation(),
            "BTC".to_string(),
            "1m".to_string(),
            CandleGapPolicy::Contiguous,
        );
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

        let LiveSeriesPublication::Tails(bars) = live.take_publication().expect("rollover tails")
        else {
            panic!("tails")
        };
        assert_eq!(
            bars.iter()
                .map(|bar| bar.source_sequence)
                .collect::<Vec<_>>(),
            vec![11, 12]
        );
        assert_eq!(bars[0].close, 10_250);
    }

    #[test]
    fn candle_gap_policy_allows_session_gaps_but_rejects_missing_contiguous_bars() {
        let series = BarSeriesKey {
            provider_id: "provider".to_string(),
            instrument_id: "instrument:provider:TEST".to_string(),
            entitlement_id: "provider-test".to_string(),
            period: BarPeriod::time(60).expect("time period"),
            definition_version: 1,
        };
        let previous = bar(10, 60_000_000_000, 10_000);
        let next = bar(11, 180_000_000_000, 10_100);
        for (policy, accepted) in [
            (CandleGapPolicy::Contiguous, false),
            (CandleGapPolicy::SessionGapsAllowed, true),
        ] {
            let mut live = CandleLiveHandoff::new(
                series.clone(),
                generation(),
                "TEST".to_string(),
                "1m".to_string(),
                policy,
            );
            live.seed(2, 2, &[previous], None).expect("history seeds");
            assert_eq!(live.ingest(next).is_ok(), accepted);
        }
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
        let mut live = CandleLiveHandoff::new(
            series,
            generation(),
            "BTC".to_string(),
            "1m".to_string(),
            CandleGapPolicy::Contiguous,
        );
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
