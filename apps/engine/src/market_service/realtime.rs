use super::{
    ActiveWorkerGuard, Arc, AtomicBool, BTreeMap, BTreeSet, BarPeriod, BarSeriesKey,
    COINBASE_PROVIDER_GENERATION, CanonicalOrderBookState, CanonicalTrade,
    CoinbaseHistoryReadiness, CoinbaseRealtimeWorkerState, ConsumerId, Coordinator, DepthSnapshot,
    DepthSnapshotWatch, Duration, FailureStage, FormingBar, HISTORY_BARS_PER_SERIES, HistoryRange,
    HistoryRequestKind, InstallProviderInstrument, Instant, LIVE_BUFFER_CAPACITY,
    LIVE_EDGE_REPAIR_RETRY_DELAY, LiveHandoff, LiveSeriesPublication,
    MAXIMUM_LIVE_EDGE_REPAIR_RETRIES, MAXIMUM_PUBLISHED_DEPTH_LEVELS, MAXIMUM_TRADED_VOLUME_LEVELS,
    MarketBar, MarketStream, MarketTrade, NonZeroU64, NonZeroUsize, OrderBook,
    OrderBookApplyOutcome, OrderBookRecoveryReason, Ordering, PendingLiveEdgeRepair,
    PersistenceState, ProviderConnectionState, ProviderGeneration, ProviderHealth,
    ProviderOrderBook, ProviderRequest, ProviderRuntimeLifecycle, PublishedTailState,
    RealtimeControl, RealtimeEvent, RealtimeProduct, RealtimeSource, Receiver, ResourceMode,
    RithmicCalendarPeriod, RithmicExchangeCalendar, RithmicLiveCadence, RithmicLiveHandoff,
    RithmicRealtimeControl, RithmicRealtimeEvent, RithmicSelection, SeriesLoadState, SyncSender,
    TrySendError, VecDeque, chart_stream_requirements, coinbase_aggregator, coinbase_instrument_id,
    coinbase_live_edge_repair_range, coinbase_series_profile, current_unix_nanos, id, ipc_series,
    order_flow_payload, publish_state, series_state_with_persistence, series_update_message,
    snapshot_message, thread,
};

impl ProviderOrderBook {
    pub(super) fn new(instrument: InstallProviderInstrument) -> Self {
        Self {
            instrument,
            book: OrderBook::new(
                NonZeroUsize::new(MAXIMUM_PUBLISHED_DEPTH_LEVELS).unwrap_or(NonZeroUsize::MIN),
            ),
            traded_volumes: BTreeMap::new(),
            watch: DepthSnapshotWatch::default(),
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
}

pub(super) fn requires_covering_publication(
    published: Option<PublishedTailState>,
    active_sequence: u64,
) -> bool {
    match published {
        Some(PublishedTailState::Covering(sequence)) => {
            sequence.checked_add(1) != Some(active_sequence)
        }
        Some(PublishedTailState::Forming(sequence)) => sequence != active_sequence,
        None => true,
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
            published: None,
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
        self.published = None;
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
        let mut working = bars.to_vec();
        if let Some(forming) = &forming {
            working.push(forming.bar);
        }
        let last_bar_boundary = working
            .last()
            .ok_or_else(|| "Rithmic live handoff requires history".to_string())?
            .exchange_timestamp_unix_nanos;
        self.price_scale = price_scale;
        self.quantity_scale = quantity_scale;
        self.published = if forming.is_some() {
            bars.last()
                .map(|bar| PublishedTailState::Covering(bar.source_sequence))
        } else {
            working
                .last()
                .map(|bar| PublishedTailState::Covering(bar.source_sequence))
        };
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
        let covering = requires_covering_publication(self.published, active.source_sequence);
        self.published = Some(PublishedTailState::Forming(active.source_sequence));
        if covering {
            Some(LiveSeriesPublication::Covering(self.bars.clone()))
        } else {
            Some(LiveSeriesPublication::Tails(vec![active]))
        }
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
            if self.bars.len() > HISTORY_BARS_PER_SERIES + 1 {
                self.bars.remove(0);
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

impl LiveHandoff {
    pub(super) fn try_new(
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        installed: &InstallProviderInstrument,
    ) -> Result<Self, String> {
        let profile = coinbase_series_profile(series, installed)?;
        Ok(Self {
            generation,
            aggregator: coinbase_aggregator(profile)?,
            buffered: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            connected: false,
            history: CoinbaseHistoryReadiness::Pending,
            dirty: false,
            published_completed: None,
        })
    }

    pub(super) fn reset(&mut self, generation: ProviderGeneration) {
        self.generation = generation;
        self.aggregator.reset();
        self.buffered.clear();
        self.connected = false;
        self.history = CoinbaseHistoryReadiness::Pending;
        self.dirty = false;
        self.published_completed = None;
    }

    /// Every bar the canonical series has not seen yet, oldest first, ending in
    /// the still-forming bucket.
    ///
    /// Sequences come from the bucket, so this run always continues the series
    /// the engine holds — including after a repair reseeded this aggregator from
    /// a longer window.
    pub(super) fn take_publication(&mut self) -> Option<LiveSeriesPublication> {
        if !self.connected || !self.history.is_ready() || !self.dirty {
            return None;
        }
        let mut bars = self
            .aggregator
            .completed_after(self.published_completed.unwrap_or(0));
        if let Some(newest) = bars.last() {
            self.published_completed = Some(newest.source_sequence);
        }
        bars.extend(self.aggregator.in_flight());
        self.dirty = false;
        (!bars.is_empty()).then_some(LiveSeriesPublication::Tails(bars))
    }
}

pub(super) fn spawn_realtime_worker(
    realtime: Box<dyn RealtimeSource>,
    controls: Receiver<RealtimeControl>,
    events: SyncSender<RealtimeEvent>,
    reconnect_delay: Duration,
    state: &CoinbaseRealtimeWorkerState,
) -> Result<thread::JoinHandle<()>, String> {
    let overflow = Arc::clone(&state.overflow);
    let stop = Arc::clone(&state.stop);
    let lifecycle = Arc::clone(&state.lifecycle);
    let active_workers = Arc::clone(&state.active_workers);
    thread::Builder::new()
        .name("axiusflow-coinbase-realtime".to_string())
        .spawn(move || {
            let _activity =
                ActiveWorkerGuard::register("axiusflow-coinbase-realtime", active_workers);
            run_realtime_worker(
                realtime,
                &controls,
                &events,
                &overflow,
                &stop,
                reconnect_delay,
                &lifecycle,
            );
        })
        .map_err(|error| error.to_string())
}

pub(super) fn run_realtime_worker(
    mut source: Box<dyn RealtimeSource>,
    control: &Receiver<RealtimeControl>,
    events: &SyncSender<RealtimeEvent>,
    overflow: &AtomicBool,
    stop: &Arc<AtomicBool>,
    reconnect_delay: Duration,
    lifecycle: &ProviderRuntimeLifecycle,
) {
    let mut generation = ProviderGeneration(
        NonZeroU64::new(COINBASE_PROVIDER_GENERATION).unwrap_or(NonZeroU64::MIN),
    );
    loop {
        let Ok(RealtimeControl::Start(products)) = control.recv() else {
            return;
        };
        stop.store(false, Ordering::Release);
        if source.configure(products).is_err() {
            lifecycle.mark_terminal_failure("Coinbase realtime configuration failed");
            return;
        }
        loop {
            if stop.load(Ordering::Acquire) {
                break;
            }
            if events.send(RealtimeEvent::Connecting(generation)).is_err() {
                return;
            }
            lifecycle.observe_generation(generation.0.get(), true);
            let connected = source.run_generation(generation, control, events, overflow, stop);
            if connected {
                lifecycle.observe_generation(generation.0.get(), false);
            }
            let stopped = stop.load(Ordering::Acquire);
            if events
                .send(RealtimeEvent::Disconnected(generation))
                .is_err()
            {
                return;
            }
            if connected {
                let Some(next) = generation.0.get().checked_add(1).and_then(NonZeroU64::new) else {
                    return;
                };
                generation = ProviderGeneration(next);
            }
            if stopped {
                break;
            }
            thread::park_timeout(reconnect_delay);
        }
    }
}

pub(super) fn try_emit_realtime(
    events: &SyncSender<RealtimeEvent>,
    overflow: &AtomicBool,
    event: RealtimeEvent,
) -> bool {
    match events.try_send(event) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => {
            overflow.store(true, Ordering::Release);
            false
        }
        Err(TrySendError::Disconnected(_)) => false,
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
        if streams.contains(MarketStream::Trades) {
            self.engine
                .verify_provider_request(&series.provider_id, ProviderRequest::Trades)
                .map_err(|error| error.to_string())?;
        }
        if streams.contains(MarketStream::Quotes) {
            self.engine
                .verify_provider_request(&series.provider_id, ProviderRequest::Quotes)
                .map_err(|error| error.to_string())?;
        }
        if streams.contains(MarketStream::Depth) {
            self.engine
                .verify_provider_request(&series.provider_id, ProviderRequest::Depth)
                .map_err(|error| error.to_string())?;
        }
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
        if series.provider_id != "coinbase" {
            return Err("resident engine realtime provider is unsupported".to_string());
        }
        if !self.live.contains_key(series) {
            let instrument = self.coinbase_instrument(series)?.clone();
            let mut handoff =
                LiveHandoff::try_new(series, self.coinbase_provider_generation(), &instrument)?;
            handoff.connected = self.realtime_connected;
            self.live.insert(series.clone(), handoff);
        }
        self.sync_coinbase_realtime()?;
        Ok(())
    }

    pub(super) fn validate_coinbase_realtime_capacity(
        &self,
        series: &BarSeriesKey,
        consumer_id: ConsumerId,
    ) -> Result<(), String> {
        if series.provider_id != "coinbase"
            || !chart_stream_requirements(series).contains(MarketStream::Trades)
        {
            return Ok(());
        }
        let replaced = self
            .engine
            .current_demand(consumer_id)
            .and_then(|demand| demand.series.as_ref());
        let mut products = BTreeSet::new();
        for active in self.live.keys() {
            let replaced_last_reference = replaced == Some(active)
                && self
                    .engine
                    .subscription_status(active)
                    .is_some_and(|status| status.consumer_count == 1);
            if !replaced_last_reference {
                products.insert(self.coinbase_instrument(active)?.provider_symbol.clone());
            }
        }
        products.insert(self.coinbase_instrument(series)?.provider_symbol.clone());
        if products.len() > axiusflow_coinbase_market_adapter::MAXIMUM_PRODUCTS {
            return Err("Coinbase realtime product capacity is exhausted".to_string());
        }
        Ok(())
    }

    pub(super) fn handle_realtime(&mut self, event: RealtimeEvent) {
        match event {
            RealtimeEvent::Connecting(generation) => self.realtime_connecting(generation),
            RealtimeEvent::Connected(generation) => self.realtime_connected(generation),
            RealtimeEvent::Trade(generation, trade) => self.realtime_trade(generation, &trade),
            RealtimeEvent::Depth(generation, snapshot) => {
                self.provider_depth("coinbase", generation.0.get(), &snapshot);
            }
            RealtimeEvent::Heartbeat(generation) => self.realtime_heartbeat(generation),
            RealtimeEvent::Disconnected(generation) => {
                if generation == self.coinbase_provider_generation() {
                    self.realtime_disconnected("Coinbase realtime disconnected");
                }
            }
        }
    }

    pub(super) fn handle_rithmic_realtime(&mut self, event: RithmicRealtimeEvent) {
        match event {
            RithmicRealtimeEvent::Connecting(generation) => self.rithmic_connecting(generation),
            RithmicRealtimeEvent::Connected(generation)
            | RithmicRealtimeEvent::Heartbeat(generation) => self.rithmic_online(generation),
            RithmicRealtimeEvent::Trade(generation, trade) => {
                self.rithmic_trade(generation, &trade);
            }
            RithmicRealtimeEvent::Depth(generation, snapshot) => {
                self.provider_depth("rithmic", generation, &snapshot);
            }
            RithmicRealtimeEvent::Recovering(generation) => {
                self.rithmic_recovering(generation, "Rithmic live session is recovering");
            }
            RithmicRealtimeEvent::Disconnected(generation) => {
                self.rithmic_selection = None;
                self.rithmic_pending_selection = None;
                self.rithmic_recovering(generation, "Rithmic live session is recovering");
            }
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
        let _ = self
            .engine
            .set_provider_health("rithmic", generation, ProviderHealth::Connecting);
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
        let _ = self
            .engine
            .set_provider_health("rithmic", generation, ProviderHealth::Online);
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
                let _ = self.enqueue_history(&series, generation);
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
            let _ = self.enqueue_history(series, generation);
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

    /// A demanded Coinbase book waiting this long for its first snapshot is
    /// stalled: the venue snapshot was missed and later deltas cannot build
    /// the book. Triggers one resubscribe dance; see
    /// `MAXIMUM_DEPTH_SNAPSHOT_RESUBSCRIBES`.
    const DEPTH_SNAPSHOT_STALL_TIMEOUT: Duration = Duration::from_secs(30);
    /// Bounded resubscribe dances per stalled book before going quiet.
    /// Recovery then waits for a fresh demand, product change, or session
    /// reconnect, each of which advances or replaces the book and re-arms
    /// the watch.
    pub(super) const MAXIMUM_DEPTH_SNAPSHOT_RESUBSCRIBES: u32 = 5;
    /// Minimum spacing between resubscribe dances for one book, so a full
    /// control channel degrades to one attempt per second instead of one per
    /// tick.
    const DEPTH_RESUBSCRIBE_SPACING: Duration = Duration::from_secs(1);

    /// Resubscribes Coinbase books stalled awaiting their first snapshot.
    ///
    /// Rithmic books are skipped: their history and backfill legitimately
    /// take tens of seconds, so a wall-clock bound cannot tell a stall from
    /// a slow load.
    pub(super) fn recover_stalled_depth_snapshots(&mut self) {
        if self.resource_mode == ResourceMode::OfflineSuspended {
            return;
        }
        let now = Instant::now();
        let mut stalled = Vec::new();
        for (identity, order_book) in &mut self.order_books {
            if order_book.instrument.provider != "coinbase" {
                continue;
            }
            if order_book.book.state()
                != CanonicalOrderBookState::Recovering(OrderBookRecoveryReason::AwaitingSnapshot)
            {
                order_book.watch = DepthSnapshotWatch::default();
                continue;
            }
            let awaited = *order_book.watch.awaited_since.get_or_insert(now);
            if now.duration_since(awaited) < Self::DEPTH_SNAPSHOT_STALL_TIMEOUT
                || order_book.watch.resubscribes >= Self::MAXIMUM_DEPTH_SNAPSHOT_RESUBSCRIBES
                || order_book.watch.last_attempt.is_some_and(|attempt| {
                    now.duration_since(attempt) < Self::DEPTH_RESUBSCRIBE_SPACING
                })
            {
                continue;
            }
            stalled.push((
                identity.clone(),
                order_book.instrument.provider_symbol.clone(),
            ));
        }
        if stalled.is_empty() {
            return;
        }
        let Ok(products) = self.coinbase_live_products() else {
            return;
        };
        let stalled_symbols: BTreeSet<String> = stalled
            .iter()
            .map(|(_, symbol)| symbol.clone())
            .filter(|symbol| products.contains_key(symbol))
            .collect();
        if stalled_symbols.is_empty() {
            return;
        }
        // Exclude-then-restore: the venue re-sends snapshots only on
        // subscribe, so the stalled products leave and rejoin while every
        // other product keeps its Ready book untouched.
        let reduced = products
            .iter()
            .filter(|(symbol, _)| !stalled_symbols.contains(*symbol))
            .map(|(_, product)| product.clone())
            .collect::<Vec<_>>();
        let full = products.into_values().collect::<Vec<_>>();
        let completed = match self.providers.start_coinbase_realtime(reduced) {
            Err(_) => return,
            Ok(sent) => sent && matches!(self.providers.start_coinbase_realtime(full), Ok(true)),
        };
        for (identity, _) in &stalled {
            let Some(order_book) = self.order_books.get_mut(identity) else {
                continue;
            };
            order_book.watch.last_attempt = Some(now);
            if !completed {
                continue;
            }
            order_book.watch.resubscribes = order_book.watch.resubscribes.saturating_add(1);
            order_book.watch.awaited_since = Some(now);
            if order_book.watch.resubscribes >= Self::MAXIMUM_DEPTH_SNAPSHOT_RESUBSCRIBES {
                eprintln!(
                    "Axiusflow engine depth snapshot unavailable for {} after bounded resubscribes; waiting for fresh demand",
                    order_book.instrument.instrument_id,
                );
            }
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
        let _ = self
            .engine
            .set_provider_health("rithmic", generation, ProviderHealth::Recovering);
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

    pub(super) fn realtime_connecting(&mut self, generation: ProviderGeneration) {
        let current = self.coinbase_provider_generation();
        if generation < current {
            return;
        }
        if generation > current {
            if self
                .engine
                .begin_provider_session("coinbase", generation)
                .is_err()
            {
                return;
            }
            for ((series, request_generation), stop) in &self.history_cancellations {
                if series.provider_id == "coinbase" && *request_generation < generation {
                    stop.store(true, Ordering::Release);
                }
            }
            for live in self.live.values_mut() {
                live.reset(generation);
            }
        } else {
            let _ =
                self.engine
                    .set_provider_health("coinbase", generation, ProviderHealth::Connecting);
        }
        let state = if generation.0.get() == COINBASE_PROVIDER_GENERATION {
            ProviderConnectionState::Connecting
        } else {
            ProviderConnectionState::Recovering
        };
        self.realtime_connected = false;
        self.broadcast_provider(state, generation, None);
    }

    pub(super) fn realtime_connected(&mut self, generation: ProviderGeneration) {
        if generation != self.coinbase_provider_generation() {
            return;
        }
        self.realtime_connected = true;
        let mut missing = Vec::new();
        for (series, live) in &mut self.live {
            live.connected = true;
            if !live.history.is_ready() {
                missing.push(series.clone());
            }
        }
        for series in missing {
            let _ = self.enqueue_history(&series, generation);
        }
        self.provider_online_if_all_series_ready();
    }

    /// Applies one live trade to every series seeded against this generation.
    ///
    /// A quiet stretch is not a seam. While the socket is up the trade feed is
    /// authoritative for "nothing traded", so a bucket the feed skipped is empty
    /// rather than missing and the aggregator carries the close across it. Real
    /// holes come from losing the socket, and that path reconnects on a fresh
    /// generation and reseeds from history instead of repairing a seam here.
    pub(super) fn realtime_trade(
        &mut self,
        generation: ProviderGeneration,
        trade: &CanonicalTrade,
    ) {
        if generation != self.coinbase_provider_generation() {
            return;
        }
        if let Ok(instrument_id) = coinbase_instrument_id(&trade.product_id)
            && let Some(instrument) = self
                .order_books
                .get(&("coinbase".to_string(), instrument_id.clone()))
                .map(|order_book| order_book.instrument.clone())
            && let (Ok(price_scale), Ok(quantity_scale)) = (
                u8::try_from(instrument.price_scale),
                u8::try_from(instrument.quantity_scale),
            )
            && let Ok(projected) = trade.to_market_trade(
                price_scale,
                quantity_scale,
                generation.0.get(),
                current_unix_nanos().unwrap_or(trade.provider_timestamp_unix_nanos),
            )
        {
            self.record_order_book_trade(
                "coinbase",
                &instrument_id,
                projected.price,
                projected.quantity,
            );
        }
        let mut interrupted = None;
        let mut rolled = Vec::new();
        for (series, live) in self.live.iter_mut().filter(|(_, live)| {
            live.generation == generation
                && live.connected
                && live.aggregator.product_id() == trade.product_id
        }) {
            if live.history.is_ready() {
                match live.aggregator.apply_trade(trade) {
                    Ok(Some(completed)) => {
                        if let Some(current) = live.aggregator.in_flight() {
                            rolled.push((series.clone(), completed, current));
                        }
                    }
                    Ok(None) => {}
                    Err(_) => {
                        interrupted = Some("Coinbase realtime aggregation failed");
                        break;
                    }
                }
                live.dirty = true;
            } else if live.buffered.len() == LIVE_BUFFER_CAPACITY {
                interrupted = Some("Coinbase history/live buffer overflowed");
                break;
            } else {
                live.buffered.push_back(trade.clone());
            }
        }
        if let Some(detail) = interrupted {
            self.realtime_interrupted(FailureStage::Aggregation, detail);
            return;
        }
        for (series, completed, current) in rolled {
            self.schedule_coinbase_live_edge_repair(&series, generation, &completed, &current);
        }
    }

    /// Reconciles every newly closed live candle with Coinbase's own OHLCV.
    ///
    /// Trades keep the forming candle responsive, but the public stream cannot
    /// prove that it observed every trade in a bucket. A short provider page at
    /// each roll replaces the closed edge before drift can accumulate into flat
    /// candles followed by a discontinuous open.
    pub(super) fn schedule_coinbase_live_edge_repair(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        completed: &MarketBar,
        current: &MarketBar,
    ) {
        let key = (series.clone(), generation);
        let Ok(range) = coinbase_live_edge_repair_range(series, completed, current) else {
            return;
        };
        self.pending_live_edge_repairs
            .entry(key)
            .and_modify(|pending| {
                pending.range.start_unix_nanos =
                    pending.range.start_unix_nanos.min(range.start_unix_nanos);
                pending.range.end_unix_nanos =
                    pending.range.end_unix_nanos.max(range.end_unix_nanos);
                pending.attempt = 0;
                pending.ready_at = Instant::now();
            })
            .or_insert(PendingLiveEdgeRepair {
                range,
                attempt: 0,
                ready_at: Instant::now(),
            });
        self.flush_coinbase_live_edge_repair(series, generation);
    }

    pub(super) fn retry_coinbase_live_edge_repair(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
        attempt: u8,
    ) {
        let Some(range) = range else {
            return;
        };
        let Some(next_attempt) = attempt.checked_add(1) else {
            return;
        };
        if next_attempt > MAXIMUM_LIVE_EDGE_REPAIR_RETRIES {
            return;
        }
        let delay =
            LIVE_EDGE_REPAIR_RETRY_DELAY.saturating_mul(2_u32.saturating_pow(u32::from(attempt)));
        let retry = PendingLiveEdgeRepair {
            range,
            attempt: next_attempt,
            ready_at: Instant::now() + delay,
        };
        self.pending_live_edge_repairs
            .entry((series.clone(), generation))
            .and_modify(|pending| {
                pending.range.start_unix_nanos =
                    pending.range.start_unix_nanos.min(range.start_unix_nanos);
                pending.range.end_unix_nanos =
                    pending.range.end_unix_nanos.max(range.end_unix_nanos);
                pending.attempt = pending.attempt.min(next_attempt);
                pending.ready_at = pending.ready_at.min(retry.ready_at);
            })
            .or_insert(retry);
    }

    pub(super) fn flush_coinbase_live_edge_repair(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) {
        let key = (series.clone(), generation);
        if self.history_inflight.contains_key(&key) {
            return;
        }
        let Some(pending) = self.pending_live_edge_repairs.get(&key).copied() else {
            return;
        };
        if pending.ready_at > Instant::now() {
            return;
        }
        self.pending_live_edge_repairs.remove(&key);
        if self
            .enqueue_history_request(
                series,
                generation,
                Some(pending.range),
                HistoryRequestKind::LiveEdgeRepair(pending.attempt),
            )
            .is_err()
        {
            self.pending_live_edge_repairs.insert(key, pending);
        }
    }

    pub(super) fn flush_coinbase_live_edge_repairs(&mut self) {
        let repairs = self
            .pending_live_edge_repairs
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for (series, generation) in repairs {
            self.flush_coinbase_live_edge_repair(&series, generation);
        }
    }

    pub(super) fn realtime_heartbeat(&mut self, generation: ProviderGeneration) {
        let missing = self
            .live
            .iter()
            .filter(|(_, live)| {
                live.generation == generation && live.connected && !live.history.is_ready()
            })
            .map(|(series, _)| series.clone())
            .collect::<Vec<_>>();
        for series in missing {
            let _ = self.enqueue_history(&series, generation);
        }
    }

    /// Rebuilds every Coinbase history/live seam after an interruption that
    /// affects the whole provider — a realtime queue overflow, or aggregation
    /// refusing a trade.
    ///
    /// `connected` stays owned by the socket lifecycle. Clearing it here left
    /// the feed permanently down whenever the socket had not actually dropped:
    /// nothing but a reconnect set it back, and no reconnect was coming. What an
    /// interruption really invalidates is the seam, so this clears
    /// `history_ready` and refetches, which the heartbeat also retries.
    pub(super) fn realtime_interrupted(&mut self, _stage: FailureStage, detail: &str) {
        let generation = self.invalidate_coinbase_seams(detail);
        // The socket is still up, so nobody else is going to reseed these: the
        // refetch has to start here. Clearing `connected` instead — which is what
        // this used to do — left the feed down for good, because only a reconnect
        // set it back and no reconnect was coming.
        for series in self.live.keys().cloned().collect::<Vec<_>>() {
            self.broadcast_series_resolution_for(
                &series,
                SeriesLoadState::Partial,
                PersistenceState::Durable,
                Some(detail),
            );
            if let Err(error) = self.enqueue_history(&series, generation) {
                self.broadcast_demand_error_for(&series, FailureStage::Handoff, error, None);
            }
        }
    }

    /// The socket dropped. The worker reconnects on a fresh generation and
    /// `realtime_connected` reseeds every series then, so this only marks the
    /// seams stale — refetching now would fetch against a generation that is
    /// already being retired.
    pub(super) fn realtime_disconnected(&mut self, detail: &'static str) {
        self.invalidate_coinbase_seams(detail);
        self.realtime_connected = false;
        for live in self.live.values_mut() {
            live.connected = false;
        }
    }

    /// Marks every Coinbase history/live seam as needing a reseed and reports the
    /// provider as recovering. Returns the current provider generation.
    pub(super) fn invalidate_coinbase_seams(&mut self, detail: &str) -> ProviderGeneration {
        let generation = self.coinbase_provider_generation();
        let _ = self
            .engine
            .set_provider_health("coinbase", generation, ProviderHealth::Recovering);
        for live in self.live.values_mut() {
            live.history = CoinbaseHistoryReadiness::Pending;
            live.dirty = false;
            live.published_completed = None;
            live.buffered.clear();
            live.aggregator.reset();
        }
        self.broadcast_provider(
            ProviderConnectionState::Recovering,
            generation,
            Some(detail),
        );
        generation
    }

    pub(super) fn provider_online_if_all_series_ready(&mut self) {
        if !self.realtime_connected
            || self.live.is_empty()
            || self.live.values().any(|live| !live.history.is_ready())
        {
            return;
        }
        let generation = self.coinbase_provider_generation();
        let _ = self
            .engine
            .set_provider_health("coinbase", generation, ProviderHealth::Online);
        self.broadcast_provider(ProviderConnectionState::Online, generation, None);
        let series = self.live.keys().cloned().collect::<Vec<_>>();
        for series in series {
            if self
                .live
                .get(&series)
                .is_some_and(|live| !live.history.is_authoritative())
            {
                self.broadcast_series_resolution_for(
                    &series,
                    SeriesLoadState::Partial,
                    PersistenceState::Durable,
                    Some(
                        "The current Coinbase candle is provisional until its authoritative close",
                    ),
                );
            } else {
                self.broadcast_series_state_for(&series, SeriesLoadState::Live);
            }
        }
    }

    pub(super) fn series_live_if_ready(&mut self, series: &BarSeriesKey) -> bool {
        if !self
            .live
            .get(series)
            .is_some_and(|live| live.connected && live.history.is_authoritative())
        {
            return false;
        }
        if self
            .engine
            .provider_status("coinbase")
            .is_some_and(|status| status.health == ProviderHealth::Online)
        {
            self.broadcast_series_state_for(series, SeriesLoadState::Live);
        } else {
            self.provider_online_if_all_series_ready();
        }
        true
    }

    /// Replaces an overflowed consumer's queued bar stream with one covering
    /// snapshot.
    ///
    /// The queue only overflows when a consumer falls further behind than
    /// [`CONSUMER_SERIES_QUEUE_CAPACITY`] distinct bars, which a healthy chart
    /// never does. Recovering with the current series — rather than dropping
    /// the oldest update — keeps the strict `+1` sequence contract intact, and
    /// costs exactly one snapshot.
    pub(super) fn publish_live(&mut self) {
        let ready = self
            .live
            .iter_mut()
            .filter_map(|(series, live)| {
                Some((
                    series.clone(),
                    live.generation,
                    live.aggregator.price_scale(),
                    live.aggregator.quantity_scale(),
                    live.take_publication()?,
                ))
            })
            .collect::<Vec<_>>();
        for (series, generation, price_scale, quantity_scale, update) in ready {
            if let Err(error) = self.install_live_publication(
                &series,
                generation,
                price_scale,
                quantity_scale,
                update,
            ) {
                eprintln!("Axiusflow engine Coinbase live publication failed: {error}");
                self.reseed_coinbase_series(&series, "Coinbase live publication needs a reseed");
            }
        }
    }

    /// Installs one live publication into the canonical series.
    pub(super) fn install_live_publication(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        price_scale: u8,
        quantity_scale: u8,
        update: LiveSeriesPublication,
    ) -> Result<(), String> {
        match update {
            LiveSeriesPublication::Tails(bars) => {
                let last = bars.len().saturating_sub(1);
                for (index, bar) in bars.into_iter().enumerate() {
                    let publications = self
                        .engine
                        .install_realtime_tail(
                            generation,
                            series,
                            price_scale,
                            quantity_scale,
                            bar,
                            index == last,
                        )
                        .map_err(|error| error.to_string())?;
                    for publication in publications {
                        if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                            events.publish_series_update(series_update_message(&publication));
                        }
                    }
                }
                Ok(())
            }
            LiveSeriesPublication::Covering(bars) => {
                let publications = self
                    .engine
                    .install_realtime(generation, series, price_scale, quantity_scale, bars, true)
                    .map_err(|error| error.to_string())?;
                for publication in publications {
                    if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                        events.publish_snapshot(snapshot_message(&publication));
                    }
                }
                Ok(())
            }
        }
    }

    /// Rearms one Coinbase series after its live publication stopped lining up
    /// with the canonical series.
    ///
    /// This is deliberately per-series and recoverable. Escalating to
    /// `realtime_interrupted` used to take every other chart down with it and
    /// left them all down, because only a socket reconnect cleared the flags and
    /// the socket had not dropped.
    pub(super) fn reseed_coinbase_series(&mut self, series: &BarSeriesKey, detail: &'static str) {
        let generation = self.coinbase_provider_generation();
        let Some(live) = self.live.get_mut(series) else {
            return;
        };
        live.history = CoinbaseHistoryReadiness::Pending;
        live.dirty = false;
        live.published_completed = None;
        live.buffered.clear();
        live.aggregator.reset();
        self.broadcast_series_resolution_for(
            series,
            SeriesLoadState::Partial,
            PersistenceState::Durable,
            Some(detail),
        );
        if let Err(error) = self.enqueue_history(series, generation) {
            self.broadcast_demand_error_for(series, FailureStage::Handoff, error, None);
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
            let published = match update {
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
                LiveSeriesPublication::Covering(bars) => self
                    .engine
                    .install_realtime(generation, &series, price_scale, quantity_scale, bars, true)
                    .map(|publications| {
                        for publication in publications {
                            if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                                publish_state(
                                    events,
                                    &publication,
                                    SeriesLoadState::Live,
                                    PersistenceState::Durable,
                                    None,
                                );
                            }
                        }
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

    pub(super) fn coinbase_provider_generation(&self) -> ProviderGeneration {
        self.engine
            .provider_status("coinbase")
            .and_then(|status| status.generation)
            .unwrap_or(ProviderGeneration(NonZeroU64::MIN))
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
        self.live.retain(|series, _| {
            self.engine.has_subscription(series)
                || self.resource_mode == ResourceMode::MarketsLive
                    && self.retained_live.contains(series)
        });
        self.rithmic_live.retain(|series, _| {
            self.engine.has_subscription(series)
                || self.resource_mode == ResourceMode::MarketsLive
                    && self.retained_live.contains(series)
        });
    }

    pub(super) fn release_unused_live_market_data(&mut self) {
        self.prune_history_tracking();
        if self.resource_mode == ResourceMode::MarketsLive {
            return;
        }
        self.prune_unused_live_series();
        if !self.live.is_empty() {
            let _ = self.sync_coinbase_realtime();
        }
        self.stop_realtime_if_idle();
    }

    pub(super) fn sync_coinbase_realtime(&mut self) -> Result<(), String> {
        let subscriptions = self.coinbase_live_products()?;
        let products = subscriptions.keys().cloned().collect::<BTreeSet<_>>();
        if products.is_empty()
            || products.len() > axiusflow_coinbase_market_adapter::MAXIMUM_PRODUCTS
        {
            return Err("Coinbase realtime subscription set is invalid".to_string());
        }
        if products != self.realtime_products
            && self
                .providers
                .start_coinbase_realtime(subscriptions.into_values().collect())?
        {
            self.realtime_products = products;
            self.realtime_started = true;
        }
        Ok(())
    }

    /// Products the live series currently require, shared by subscription
    /// sync and the stalled-depth resubscribe dance so both agree on the set.
    fn coinbase_live_products(&self) -> Result<BTreeMap<String, RealtimeProduct>, String> {
        let mut subscriptions = BTreeMap::new();
        for series in self.live.keys() {
            let instrument = self.coinbase_instrument(series)?;
            let (Ok(price_scale), Ok(quantity_scale)) = (
                u8::try_from(instrument.price_scale),
                u8::try_from(instrument.quantity_scale),
            ) else {
                return Err("Coinbase instrument precision is invalid".to_string());
            };
            subscriptions.insert(
                instrument.provider_symbol.clone(),
                RealtimeProduct {
                    symbol: instrument.provider_symbol.clone(),
                    price_scale,
                    quantity_scale,
                },
            );
        }
        Ok(subscriptions)
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
        if self.live.is_empty() && self.realtime_started {
            self.providers.stop("coinbase");
            self.realtime_started = false;
            self.realtime_connected = false;
            self.realtime_products.clear();
            self.live.clear();
            let generation = self.coinbase_provider_generation();
            let _ = self.engine.end_provider_session("coinbase", generation);
        }
        if self.rithmic_live.is_empty() && self.rithmic_selection.is_some() {
            self.providers.stop("rithmic");
            self.rithmic_selection = None;
            self.rithmic_pending_selection = None;
            if let Some(generation) = self
                .engine
                .provider_status("rithmic")
                .and_then(|status| status.generation)
            {
                let _ = self.engine.end_provider_session("rithmic", generation);
            }
        }
    }
}
