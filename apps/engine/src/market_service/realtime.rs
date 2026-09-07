use super::{
    BTreeMap, BTreeSet, BarPeriod, BarSeriesKey, CanonicalOrderBookState, ConsumerId, Coordinator,
    DepthSnapshot, FailureStage, FormingBar, HyperliquidCandleDemand, HyperliquidDemand,
    HyperliquidInstrumentDemand, HyperliquidLiveCandle, HyperliquidLiveHandoff,
    HyperliquidRealtimeControl, HyperliquidRealtimeEvent, InstallProviderInstrument,
    LIVE_BUFFER_CAPACITY, LIVE_HANDOFF_HISTORY_BARS, LiveSeriesPublication,
    MAXIMUM_PUBLISHED_DEPTH_LEVELS, MAXIMUM_TRADED_VOLUME_LEVELS, MarketBar, MarketStream,
    MarketTrade, NonZeroUsize, OrderBook, OrderBookApplyOutcome, Ordering, PersistenceState,
    ProviderConnectionState, ProviderGeneration, ProviderHealth, ProviderOrderBook, ResourceMode,
    RithmicCalendarPeriod, RithmicExchangeCalendar, RithmicLiveCadence, RithmicLiveHandoff,
    RithmicRealtimeControl, RithmicRealtimeEvent, RithmicSelection, SeriesLoadState,
    TopOfBookQuote, VecDeque, chart_stream_requirements, hyperliquid_interval_for_period, id,
    ipc_series, merge_live_candle, order_flow_payload, series_state_with_persistence,
    series_update_message,
};
use axiusflow_rithmic_protocol_adapter::ProviderInvalidationReason;

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

impl ProviderOrderBook {
    pub(super) fn new(instrument: InstallProviderInstrument) -> Self {
        Self {
            instrument,
            book: OrderBook::new(
                NonZeroUsize::new(MAXIMUM_PUBLISHED_DEPTH_LEVELS).unwrap_or(NonZeroUsize::MIN),
            ),
            top_of_book: None,
            traded_volumes: BTreeMap::new(),
        }
    }

    pub(super) fn record_trade(&mut self, price: i64, quantity: i64) -> bool {
        if price <= 0
            || quantity <= 0
            || (!self.traded_volumes.contains_key(&price)
                && self.traded_volumes.len() == MAXIMUM_TRADED_VOLUME_LEVELS)
        {
            return false;
        }
        let Some(volume) = self
            .traded_volumes
            .get(&price)
            .copied()
            .unwrap_or(0)
            .checked_add(quantity)
        else {
            return false;
        };
        self.traded_volumes.insert(price, volume);
        true
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
            connected: false,
            history_ready: false,
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
        self.connected = false;
        self.history_ready = false;
        self.dirty = false;
        self.forming_tail_sequence = None;
        self.live_session_generation = None;
        self.last_trade_sequence = None;
        self.history_boundary_unix_nanos = i64::MIN;
        if let RithmicLiveCadence::Tick { trades, forming } = &mut self.cadence {
            *forming = *trades;
        }
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
        self.dirty = forming.is_some();
        let buffered = std::mem::take(&mut self.buffered);
        self.history_ready = true;
        for trade in &buffered {
            if self.apply_trade(trade)? {
                self.dirty = true;
            }
        }
        Ok(())
    }

    pub(super) fn take_publication(&mut self) -> Option<LiveSeriesPublication> {
        if !self.connected || !self.history_ready || !self.dirty {
            return None;
        }
        let active = self.bars.last().copied()?;
        self.dirty = false;
        Some(LiveSeriesPublication::Tails(vec![active]))
    }

    pub(super) fn accept_trade(&mut self, trade: &MarketTrade) -> Result<(), String> {
        if self.history_ready {
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
        Ok(true)
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
            connected: false,
            history_ready: false,
            dirty: false,
        }
    }

    pub(super) fn reset(&mut self, generation: ProviderGeneration) {
        self.generation = generation;
        self.bars.clear();
        self.forming = None;
        self.buffered.clear();
        self.connected = false;
        self.history_ready = false;
        self.dirty = false;
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
        self.dirty = self.forming.is_some();
        let buffered = std::mem::take(&mut self.buffered);
        self.history_ready = true;
        for candle in &buffered {
            if self.ingest(candle)? {
                self.dirty = true;
            }
        }
        Ok(())
    }

    pub(super) fn take_publication(&mut self) -> Option<LiveSeriesPublication> {
        if !self.connected || !self.history_ready || !self.dirty {
            return None;
        }
        let active = self.forming.or_else(|| self.bars.last().copied())?;
        self.dirty = false;
        Some(LiveSeriesPublication::Tails(vec![active]))
    }

    pub(super) fn accept_candle(&mut self, candle: &HyperliquidLiveCandle) -> Result<(), String> {
        if self.history_ready {
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
        Ok(true)
    }
}

impl Coordinator<'_> {
    pub(super) fn ensure_realtime(&mut self, series: &BarSeriesKey) -> Result<(), String> {
        let streams = self
            .engine
            .subscription_status(series)
            .map(|status| status.streams)
            .or_else(|| {
                (self.resource_mode == ResourceMode::MarketsLive
                    && self.retained_live.contains(series))
                .then(|| chart_stream_requirements(series))
            })
            .ok_or_else(|| "series has no accepted upstream subscription".to_string())?;
        // This is deliberately capability-only. After an account/offline
        // suspension the provider session is inactive; the Select/Subscribe
        // control queued below is what wakes the worker and creates the next
        // active generation. Requiring an active session here deadlocks that
        // recovery before the wake-up control can be sent.
        self.engine
            .verify_provider_stream_requirements(&series.provider_id, streams)
            .map_err(|error| error.to_string())?;
        if !streams.contains(MarketStream::Trades)
            && !streams.contains(MarketStream::Quotes)
            && !streams.contains(MarketStream::Depth)
        {
            return Ok(());
        }
        if series.provider_id == "rithmic" {
            // A timeframe change keeps the same instrument and generation, so
            // it sends nothing; a symbol change, or a session that has been
            // retired and replaced, sends the worker its replacement selection.
            if self.providers.rithmic_realtime_enabled() {
                let selection = RithmicSelection {
                    instrument_id: series.instrument_id.clone(),
                    generation: self.provider_generation_for_series(series)?,
                };
                if self.rithmic_selection.as_ref() != Some(&selection) {
                    let instrument = self
                        .catalog
                        .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                        .cloned()
                        .ok_or_else(|| "Rithmic instrument is not installed".to_string())?;
                    if !self
                        .providers
                        .send_rithmic_realtime(RithmicRealtimeControl::Select(instrument.clone()))?
                    {
                        // The worker coalesces queued selections to the newest, so a
                        // full channel is retried on the next coordinator tick.
                        self.rithmic_pending_selection = Some(instrument);
                    }
                    self.rithmic_selection = Some(selection);
                }
            }
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
            return Ok(());
        }
        if series.provider_id == "hyperliquid" {
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
        Err("resident engine realtime provider is unsupported".to_string())
    }

    pub(super) fn handle_rithmic_realtime(&mut self, event: RithmicRealtimeEvent) {
        let event_generation = match &event {
            RithmicRealtimeEvent::Connecting(generation)
            | RithmicRealtimeEvent::Connected(generation)
            | RithmicRealtimeEvent::Heartbeat(generation)
            | RithmicRealtimeEvent::Trade(generation, _)
            | RithmicRealtimeEvent::Quote(generation, _)
            | RithmicRealtimeEvent::Depth(generation, _)
            | RithmicRealtimeEvent::Recovering(generation, _)
            | RithmicRealtimeEvent::Disconnected(generation, _) => *generation,
        };
        if self
            .rithmic_stop_pending
            .is_some_and(|pending| pending.0.get() == event_generation)
            && !matches!(event, RithmicRealtimeEvent::Disconnected(..))
        {
            return;
        }
        match event {
            RithmicRealtimeEvent::Connecting(generation) => self.rithmic_connecting(generation),
            RithmicRealtimeEvent::Connected(generation)
            | RithmicRealtimeEvent::Heartbeat(generation) => self.rithmic_online(generation),
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
                if self
                    .rithmic_stop_pending
                    .is_some_and(|pending| pending.0.get() == generation)
                {
                    self.rithmic_stop_pending = None;
                    if !self.rithmic_live.is_empty() {
                        self.rithmic_selection = None;
                        self.rithmic_pending_selection = None;
                        let demanded = self.rithmic_live.keys().cloned().collect::<Vec<_>>();
                        for series in demanded {
                            let _ = self.ensure_realtime(&series);
                        }
                        self.rithmic_recovering(
                            generation,
                            "Rithmic realtime restarted after idle-stop overlap",
                        );
                        return;
                    }
                    self.rithmic_selection = None;
                    self.rithmic_pending_selection = None;
                    if let Ok(generation) = id(generation).map(ProviderGeneration) {
                        let _ = self.engine.end_provider_session("rithmic", generation);
                        self.broadcast_provider_for(
                            "rithmic",
                            ProviderConnectionState::Disconnected,
                            generation,
                            None,
                        );
                    }
                    return;
                }
                self.rithmic_selection = None;
                self.rithmic_pending_selection = None;
                if let Some(reason) = reason {
                    self.rithmic_failed(generation, rithmic_invalidation_detail(Some(reason)));
                } else {
                    self.rithmic_recovering(generation, "Rithmic live session is recovering");
                }
            }
        }
    }

    pub(super) fn handle_hyperliquid_realtime(&mut self, event: HyperliquidRealtimeEvent) {
        let event_generation = match &event {
            HyperliquidRealtimeEvent::Connecting(generation)
            | HyperliquidRealtimeEvent::Connected(generation)
            | HyperliquidRealtimeEvent::Heartbeat(generation)
            | HyperliquidRealtimeEvent::Candle(generation, ..)
            | HyperliquidRealtimeEvent::Trades(generation, _)
            | HyperliquidRealtimeEvent::Depth(generation, _)
            | HyperliquidRealtimeEvent::Recovering(generation)
            | HyperliquidRealtimeEvent::Disconnected(generation) => *generation,
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
            HyperliquidRealtimeEvent::Connected(generation)
            | HyperliquidRealtimeEvent::Heartbeat(generation) => {
                self.hyperliquid_online(generation);
            }
            HyperliquidRealtimeEvent::Candle(generation, wire_coin, interval, candle) => {
                self.hyperliquid_candle(generation, &wire_coin, &interval, &candle);
            }
            HyperliquidRealtimeEvent::Trades(generation, trades) => {
                for trade in &trades {
                    self.hyperliquid_trade(generation, trade);
                }
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
                    if !self.hyperliquid_live.is_empty() || !self.order_books.is_empty() {
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
                        self.broadcast_provider_for(
                            "hyperliquid",
                            ProviderConnectionState::Disconnected,
                            generation,
                            None,
                        );
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
                let _ = self.enqueue_local_history(&selected, generation);
            }
        }
        if self
            .engine
            .set_provider_health("hyperliquid", generation, ProviderHealth::Connecting)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for(
            "hyperliquid",
            ProviderConnectionState::Connecting,
            generation,
            None,
        );
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
        self.broadcast_provider_for(
            "hyperliquid",
            ProviderConnectionState::Online,
            generation,
            None,
        );
        let missing = self
            .hyperliquid_live
            .iter_mut()
            .filter_map(|(series, live)| {
                if live.generation != generation {
                    return None;
                }
                live.connected = true;
                (!live.history_ready).then(|| series.clone())
            })
            .collect::<Vec<_>>();
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

    pub(super) fn hyperliquid_trade(&mut self, generation: u64, trade: &MarketTrade) {
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
        if trade.metadata.provider_id != "hyperliquid"
            || trade.metadata.session_generation != generation.0.get()
        {
            self.hyperliquid_recovering(
                generation.0.get(),
                "Hyperliquid live session identity requires recovery",
            );
            for live in self.hyperliquid_live.values_mut() {
                live.history_ready = false;
                live.dirty = false;
                live.buffered.clear();
            }
            return;
        }
        self.record_order_book_trade(
            "hyperliquid",
            &trade.metadata.instrument_id,
            trade.price,
            trade.quantity,
        );
        let order_flow_series = self
            .hyperliquid_live
            .keys()
            .filter(|series| {
                series.instrument_id == trade.metadata.instrument_id
                    && series.entitlement_id == trade.metadata.entitlement_id
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut order_flow_failed = BTreeSet::new();
        for series in order_flow_series {
            match self
                .engine
                .install_order_flow_trade(generation, &series, trade)
            {
                Ok(publications) => {
                    for publication in publications {
                        if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                            events.order_flow = Some(order_flow_payload(&publication));
                        }
                    }
                }
                Err(_) => {
                    order_flow_failed.insert(series);
                }
            }
        }
        for series in &order_flow_failed {
            self.hyperliquid_series_recovering(
                series,
                generation,
                FailureStage::CanonicalValidation,
                "Hyperliquid order-flow reconstruction requires covering history",
            );
        }
    }

    pub(super) fn hyperliquid_series_recovering(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        stage: FailureStage,
        detail: &str,
    ) {
        if let Some(live) = self.hyperliquid_live.get_mut(series) {
            live.history_ready = false;
            live.dirty = false;
            live.buffered.clear();
        }
        self.broadcast_demand_error_for(series, stage, detail, None);
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
        if self
            .engine
            .set_provider_health("hyperliquid", generation, ProviderHealth::Recovering)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for(
            "hyperliquid",
            ProviderConnectionState::Recovering,
            generation,
            Some(detail),
        );
        for live in self.hyperliquid_live.values_mut() {
            live.connected = false;
        }
        let stale_books = self
            .order_books
            .iter_mut()
            .filter(|((provider, _), _)| provider == "hyperliquid")
            .filter_map(|(identity, order_book)| {
                order_book.book.mark_stale();
                matches!(order_book.book.state(), CanonicalOrderBookState::Stale)
                    .then(|| identity.clone())
            })
            .collect::<Vec<_>>();
        for (provider, instrument_id) in stale_books {
            self.broadcast_order_book(&provider, &instrument_id);
        }
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
                let _ = self.enqueue_local_history(&selected, generation);
            }
        }
        if self
            .engine
            .set_provider_health("rithmic", generation, ProviderHealth::Connecting)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for(
            "rithmic",
            ProviderConnectionState::Connecting,
            generation,
            None,
        );
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
        self.broadcast_provider_for("rithmic", ProviderConnectionState::Online, generation, None);
        let missing = self
            .rithmic_live
            .iter_mut()
            .filter_map(|(series, live)| {
                if live.generation != generation {
                    return None;
                }
                live.connected = true;
                (!live.history_ready).then(|| series.clone())
            })
            .collect::<Vec<_>>();
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
                live.history_ready = false;
                live.dirty = false;
                live.buffered.clear();
            }
            return;
        }
        self.record_order_book_trade(
            "rithmic",
            &trade.metadata.instrument_id,
            trade.price,
            trade.quantity,
        );
        let order_flow_series = self
            .rithmic_live
            .keys()
            .filter(|series| {
                series.instrument_id == trade.metadata.instrument_id
                    && series.entitlement_id == trade.metadata.entitlement_id
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut order_flow_failed = BTreeSet::new();
        for series in order_flow_series {
            match self
                .engine
                .install_order_flow_trade(generation, &series, trade)
            {
                Ok(publications) => {
                    for publication in publications {
                        if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                            events.order_flow = Some(order_flow_payload(&publication));
                        }
                    }
                }
                Err(_) => {
                    order_flow_failed.insert(series);
                }
            }
        }
        for series in &order_flow_failed {
            self.rithmic_series_recovering(
                series,
                generation,
                FailureStage::CanonicalValidation,
                "Rithmic order-flow reconstruction requires covering history",
            );
        }
        let failed = self
            .rithmic_live
            .iter_mut()
            .filter(|(_, live)| {
                live.generation == generation
                    && live.connected
                    && live.series.instrument_id == trade.metadata.instrument_id
                    && live.series.entitlement_id == trade.metadata.entitlement_id
                    && !order_flow_failed.contains(&live.series)
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
    }

    pub(super) fn rithmic_series_recovering(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        stage: FailureStage,
        detail: &str,
    ) {
        if let Some(live) = self.rithmic_live.get_mut(series) {
            live.history_ready = false;
            live.dirty = false;
            live.buffered.clear();
        }
        self.broadcast_demand_error_for(series, stage, detail, None);
        self.broadcast_series_recovery_for(series, detail);
        if !self
            .history_inflight
            .contains_key(&(series.clone(), generation))
        {
            let _ = self.enqueue_history_recovery(series, generation);
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
        let should_publish = self
            .order_books
            .get_mut(&(provider.to_string(), instrument_id.clone()))
            .filter(|order_book| {
                order_book.instrument.entitlement_id == snapshot.metadata.entitlement_id
            })
            .is_some_and(|order_book| {
                matches!(
                    order_book.book.install_snapshot(snapshot),
                    Ok(OrderBookApplyOutcome::Published(_)
                        | OrderBookApplyOutcome::RecoveryRequired(_))
                ) || matches!(
                    order_book.book.state(),
                    CanonicalOrderBookState::Recovering(_)
                )
            });
        if should_publish {
            self.broadcast_order_book(provider, &instrument_id);
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
        let changed = self
            .order_books
            .get_mut(&(provider.to_string(), instrument_id.clone()))
            .is_some_and(|order_book| order_book.install_top_of_book(quote));
        if changed {
            self.broadcast_order_book(provider, &instrument_id);
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
        if self
            .engine
            .set_provider_health("rithmic", generation, ProviderHealth::Recovering)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for(
            "rithmic",
            ProviderConnectionState::Recovering,
            generation,
            Some(detail),
        );
        for live in self.rithmic_live.values_mut() {
            live.connected = false;
        }
        let stale_books = self
            .order_books
            .iter_mut()
            .filter(|((provider, _), _)| provider == "rithmic")
            .filter_map(|(identity, order_book)| {
                order_book.book.mark_stale();
                matches!(order_book.book.state(), CanonicalOrderBookState::Stale)
                    .then(|| identity.clone())
            })
            .collect::<Vec<_>>();
        for (provider, instrument_id) in stale_books {
            self.broadcast_order_book(&provider, &instrument_id);
        }
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
        if self
            .engine
            .set_provider_health("rithmic", generation, ProviderHealth::Failed)
            .is_err()
        {
            return;
        }
        self.broadcast_provider_for(
            "rithmic",
            ProviderConnectionState::Failed,
            generation,
            Some(detail),
        );
        for live in self.rithmic_live.values_mut() {
            live.connected = false;
        }
        let stale_books = self
            .order_books
            .iter_mut()
            .filter(|((provider, _), _)| provider == "rithmic")
            .filter_map(|(identity, order_book)| {
                order_book.book.mark_stale();
                matches!(order_book.book.state(), CanonicalOrderBookState::Stale)
                    .then(|| identity.clone())
            })
            .collect::<Vec<_>>();
        for (provider, instrument_id) in stale_books {
            self.broadcast_order_book(&provider, &instrument_id);
        }
    }

    pub(super) fn record_order_book_trade(
        &mut self,
        provider: &str,
        instrument_id: &str,
        price: i64,
        quantity: i64,
    ) {
        let recorded = self
            .order_books
            .get_mut(&(provider.to_string(), instrument_id.to_string()))
            .is_some_and(|order_book| order_book.record_trade(price, quantity));
        if recorded {
            self.broadcast_order_book(provider, instrument_id);
        }
    }

    pub(super) fn publish_rithmic_live(&mut self) {
        let ready = self
            .rithmic_live
            .values_mut()
            .filter_map(|live| {
                Some((
                    live.series.clone(),
                    live.generation,
                    live.price_scale,
                    live.quantity_scale,
                    live.take_publication()?,
                ))
            })
            .collect::<Vec<_>>();
        for (series, generation, price_scale, quantity_scale, update) in ready {
            let published: Result<(), axiusflow_market_engine::EngineError> = match update {
                LiveSeriesPublication::Tails(bars) => bars.into_iter().try_for_each(|bar| {
                    let publications = self.engine.install_realtime_tail(
                        generation,
                        &series,
                        price_scale,
                        quantity_scale,
                        bar,
                        true,
                    )?;
                    for publication in publications {
                        if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                            events.publish_series_update(series_update_message(&publication));
                            events.series_state = Some(series_state_with_persistence(
                                publication.consumer_id,
                                publication.generation,
                                ipc_series(&publication.series),
                                SeriesLoadState::Live,
                                PersistenceState::Durable,
                                None,
                            ));
                        }
                    }
                    Ok(())
                }),
            };
            if let Err(error) = published {
                if let Some(live) = self.rithmic_live.get_mut(&series) {
                    live.history_ready = false;
                }
                eprintln!("Axiusflow engine Rithmic live publication failed: {error}");
                self.broadcast_provider_for(
                    "rithmic",
                    ProviderConnectionState::Recovering,
                    generation,
                    Some("Rithmic live publication requires covering history"),
                );
                self.broadcast_demand_error_for(
                    &series,
                    FailureStage::Publication,
                    "Rithmic live publication requires covering history",
                    None,
                );
            }
        }
    }

    pub(super) fn series_live_if_ready(&mut self, series: &BarSeriesKey) -> bool {
        let ready = self
            .rithmic_live
            .get(series)
            .is_some_and(|live| live.connected && live.history_ready)
            || self
                .hyperliquid_live
                .get(series)
                .is_some_and(|live| live.connected && live.history_ready);
        if ready {
            self.broadcast_series_resolution_for(
                series,
                SeriesLoadState::Live,
                PersistenceState::Durable,
                None,
            );
        }
        ready
    }

    /// Rebuilds the complete desired Hyperliquid subscription set from live
    /// handoffs and depth demand. Candles follow chart series, trades follow
    /// live instruments, and books follow Order Book demand; nothing else subscribes.
    pub(super) fn hyperliquid_demand(&self) -> HyperliquidDemand {
        let mut candles = BTreeSet::new();
        let mut trades = BTreeSet::new();
        for (series, live) in &self.hyperliquid_live {
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
            candles.insert(HyperliquidCandleDemand {
                instrument: mapping.clone(),
                interval: live.interval.clone(),
            });
            trades.insert(mapping);
        }
        let mut books = BTreeSet::new();
        for ((provider, _), book) in &self.order_books {
            if provider != "hyperliquid" {
                continue;
            }
            let (Ok(price_scale), Ok(quantity_scale)) = (
                u8::try_from(book.instrument.price_scale),
                u8::try_from(book.instrument.quantity_scale),
            ) else {
                continue;
            };
            books.insert(HyperliquidInstrumentDemand {
                wire_coin: book.instrument.provider_symbol.clone(),
                instrument_id: book.instrument.instrument_id.clone(),
                entitlement_id: book.instrument.entitlement_id.clone(),
                price_scale,
                quantity_scale,
            });
        }
        HyperliquidDemand {
            candles: candles.into_iter().collect(),
            trades: trades.into_iter().collect(),
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
        let empty =
            demand.candles.is_empty() && demand.trades.is_empty() && demand.books.is_empty();
        if empty && !self.hyperliquid_engaged {
            self.hyperliquid_demand_dirty = false;
            return;
        }
        match self
            .providers
            .send_hyperliquid_realtime(HyperliquidRealtimeControl::Subscribe(demand))
        {
            Ok(true) => {
                self.hyperliquid_demand_dirty = false;
                self.hyperliquid_engaged = !empty;
            }
            // A full channel retries on the next coordinator tick; the
            // worker coalesces to the newest set.
            Ok(false) => {}
            Err(_) => {
                self.hyperliquid_demand_dirty = false;
            }
        }
    }

    pub(super) fn publish_hyperliquid_live(&mut self) {
        let ready = self
            .hyperliquid_live
            .values_mut()
            .filter_map(|live| {
                Some((
                    live.series.clone(),
                    live.generation,
                    live.price_scale,
                    live.quantity_scale,
                    live.take_publication()?,
                ))
            })
            .collect::<Vec<_>>();
        for (series, generation, price_scale, quantity_scale, update) in ready {
            let published: Result<(), axiusflow_market_engine::EngineError> = match update {
                LiveSeriesPublication::Tails(bars) => bars.into_iter().try_for_each(|bar| {
                    let publications = self.engine.install_realtime_tail(
                        generation,
                        &series,
                        price_scale,
                        quantity_scale,
                        bar,
                        true,
                    )?;
                    for publication in publications {
                        if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                            events.publish_series_update(series_update_message(&publication));
                            events.series_state = Some(series_state_with_persistence(
                                publication.consumer_id,
                                publication.generation,
                                ipc_series(&publication.series),
                                SeriesLoadState::Live,
                                PersistenceState::Durable,
                                None,
                            ));
                        }
                    }
                    Ok(())
                }),
            };
            if let Err(error) = published {
                if let Some(live) = self.hyperliquid_live.get_mut(&series) {
                    live.history_ready = false;
                }
                eprintln!("Axiusflow engine Hyperliquid live publication failed: {error}");
                self.broadcast_provider_for(
                    "hyperliquid",
                    ProviderConnectionState::Recovering,
                    generation,
                    Some("Hyperliquid live publication requires covering history"),
                );
                self.broadcast_demand_error_for(
                    &series,
                    FailureStage::Publication,
                    "Hyperliquid live publication requires covering history",
                    None,
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
        if self.resource_mode == ResourceMode::MarketsLive {
            return;
        }
        self.local_history_deadlines
            .retain(|(series, _), _| !unobserved.contains(series));
        for series in unobserved {
            for ((active, _), stop) in &self.history_cancellations {
                if active == &series {
                    stop.store(true, Ordering::Release);
                }
            }
        }
    }

    pub(super) fn prune_unused_live_series(&mut self) {
        for series in self.rithmic_live.keys().filter(|series| {
            !(self.engine.has_subscription(series)
                || self.resource_mode == ResourceMode::MarketsLive
                    && self.retained_live.contains(series))
        }) {
            for ((active, _), stop) in &self.history_cancellations {
                if active == series {
                    stop.store(true, Ordering::Release);
                }
            }
        }
        self.rithmic_live.retain(|series, _| {
            self.engine.has_subscription(series)
                || self.resource_mode == ResourceMode::MarketsLive
                    && self.retained_live.contains(series)
        });
        for series in self.hyperliquid_live.keys().filter(|series| {
            !(self.engine.has_subscription(series)
                || self.resource_mode == ResourceMode::MarketsLive
                    && self.retained_live.contains(series))
        }) {
            for ((active, _), stop) in &self.history_cancellations {
                if active == series {
                    stop.store(true, Ordering::Release);
                }
            }
        }
        let retained_hyperliquid = self.hyperliquid_live.len();
        self.hyperliquid_live.retain(|series, _| {
            self.engine.has_subscription(series)
                || self.resource_mode == ResourceMode::MarketsLive
                    && self.retained_live.contains(series)
        });
        if self.hyperliquid_live.len() != retained_hyperliquid {
            self.hyperliquid_demand_dirty = true;
        }
    }

    pub(super) fn release_unused_live_market_data(&mut self) {
        self.prune_history_tracking();
        if self.resource_mode == ResourceMode::MarketsLive {
            return;
        }
        self.prune_unused_live_series();
        self.stop_realtime_if_idle();
    }
    /// Hands the worker a replacement selection the control channel refused.
    pub(super) fn flush_rithmic_selection(&mut self) {
        let Some(instrument) = self.rithmic_pending_selection.take() else {
            return;
        };
        if self
            .providers
            .send_rithmic_realtime(RithmicRealtimeControl::Select(instrument.clone()))
            .is_ok_and(|sent| !sent)
        {
            self.rithmic_pending_selection = Some(instrument);
        }
    }
    pub(super) fn stop_realtime_if_idle(&mut self) {
        if self.rithmic_live.is_empty()
            && self.rithmic_selection.is_some()
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
                    self.rithmic_selection = None;
                    self.rithmic_pending_selection = None;
                    let _ = self.engine.end_provider_session("rithmic", generation);
                }
            }
        }
        if self.hyperliquid_live.is_empty()
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
