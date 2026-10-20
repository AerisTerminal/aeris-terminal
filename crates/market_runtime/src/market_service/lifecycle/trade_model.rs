//! Trade aggregation for the coordinator-owned series lifecycle.
#[cfg(test)]
use super::super::InstallProviderInstrument;
use super::super::realtime::enqueue_bar_transition;
use super::super::{
    BTreeMap, BarPeriod, BarSeriesKey, CandleGapPolicy, FormingBar, IndexedTradeMutation,
    LIVE_BUFFER_CAPACITY, LIVE_HANDOFF_HISTORY_BARS, LiveSeriesPublication, MarketBar, MarketTrade,
    ProviderCandle, ProviderGeneration, VecDeque, calendar, clock,
};
use std::{sync::Arc, time::Instant};

pub(in crate::market_service) struct TradeModel {
    pub(in crate::market_service) clock: Arc<dyn clock::Clock>,
    pub(in crate::market_service) calendar: calendar::MarketCalendarService,
    pub(in crate::market_service) series: BarSeriesKey,
    pub(in crate::market_service) generation: ProviderGeneration,
    cadence: TradeLiveCadence,
    calendar_stamp: calendar::CalendarBarStamp,
    pub(in crate::market_service) gap_policy: CandleGapPolicy,
    pub(in crate::market_service) candle_symbol: Option<String>,
    pub(in crate::market_service) price_scale: u8,
    pub(in crate::market_service) quantity_scale: u8,
    pub(in crate::market_service) bars: Vec<MarketBar>,
    pub(in crate::market_service) buffered: VecDeque<TradeLiveUpdate>,
    pub(in crate::market_service) pending_publications: VecDeque<MarketBar>,
    pub(in crate::market_service) pending_corrections: VecDeque<MarketBar>,
    pub(in crate::market_service) connected: bool,
    pub(in crate::market_service) dirty: bool,
    /// The sequence of the period the provider caught open, when it caught one.
    ///
    /// Live trades revise an open period in place; they may never touch a period
    /// the provider has closed. Without this the two are indistinguishable, and
    /// the first trade of a freshly seeded chart was dropped rather than folded
    /// into the candle it belongs to.
    pub(in crate::market_service) forming_tail_sequence: Option<u64>,
    pub(in crate::market_service) live_session_generation: Option<u64>,
    pub(in crate::market_service) last_trade_sequence: Option<u64>,
    pub(in crate::market_service) history_boundary_unix_nanos: i64,
    pub(in crate::market_service) overlay_base: Option<MarketBar>,
    pub(in crate::market_service) indexed_overlay: BTreeMap<String, MarketTrade>,
    pub(in crate::market_service) provider_candle_count: Option<u64>,
    pub(in crate::market_service) provider_candle_timestamp: Option<i64>,
    pub(in crate::market_service) last_publication_at: Instant,
    pub(in crate::market_service) last_trade_at: Option<Instant>,
}

enum TradeLiveCadence {
    Fixed { seconds: i64 },
    Tick { trades: u32, forming: u32 },
    Calendar { period: calendar::CalendarBarPeriod },
}

#[derive(Clone)]
pub(in crate::market_service) enum TradeLiveUpdate {
    Append(MarketTrade),
    Indexed(IndexedTradeMutation),
}

impl TradeModel {
    #[cfg(test)]
    pub(in crate::market_service) fn new(
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        venue_id: &str,
        candle: Option<(CandleGapPolicy, String)>,
    ) -> Option<Self> {
        Self::with_clock(
            series,
            generation,
            venue_id,
            candle,
            std::sync::Arc::new(super::super::clock::SystemClock),
        )
    }

    #[cfg(test)]
    pub(in crate::market_service) fn with_clock(
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        venue_id: &str,
        candle: Option<(CandleGapPolicy, String)>,
        clock: std::sync::Arc<dyn super::super::clock::Clock>,
    ) -> Option<Self> {
        let mut calendar = super::super::calendar::MarketCalendarService::default();
        calendar.resolve(
            &InstallProviderInstrument {
                instrument_id: series.instrument_id.clone(),
                venue_id: venue_id.into(),
                ..Default::default()
            },
            None,
        );
        Self::with_calendar(
            series,
            generation,
            candle,
            clock,
            calendar,
            calendar::CalendarBarStamp::SessionOpen,
        )
    }

    pub(in crate::market_service) fn with_calendar(
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        candle: Option<(CandleGapPolicy, String)>,
        clock: std::sync::Arc<dyn super::super::clock::Clock>,
        calendar: super::super::calendar::MarketCalendarService,
        calendar_stamp: calendar::CalendarBarStamp,
    ) -> Option<Self> {
        use super::super::calendar::CalendarBarPeriod;
        let cadence = match series.period {
            BarPeriod::Tick { trades } => TradeLiveCadence::Tick {
                trades,
                forming: trades,
            },
            BarPeriod::Time { seconds } => TradeLiveCadence::Fixed {
                seconds: i64::from(seconds),
            },
            BarPeriod::Session { days } => TradeLiveCadence::Calendar {
                period: CalendarBarPeriod::Session(days),
            },
            BarPeriod::Week { weeks: 1 } => TradeLiveCadence::Calendar {
                period: CalendarBarPeriod::Week,
            },
            BarPeriod::Month { months: 1 } => TradeLiveCadence::Calendar {
                period: CalendarBarPeriod::Month,
            },
            BarPeriod::Week { .. } | BarPeriod::Month { .. } => return None,
        };
        if matches!(cadence, TradeLiveCadence::Calendar { .. })
            && calendar.source_kind(&series.instrument_id)
                == aeris_instruments::CalendarSourceKind::Unknown
        {
            return None;
        }
        let (gap_policy, candle_symbol) = candle
            .map_or((CandleGapPolicy::Contiguous, None), |(policy, symbol)| {
                (policy, Some(symbol))
            });
        Some(Self {
            last_publication_at: clock.monotonic(),
            clock,
            calendar,
            series: series.clone(),
            generation,
            cadence,
            calendar_stamp,
            gap_policy,
            candle_symbol,
            price_scale: 0,
            quantity_scale: 0,
            bars: Vec::new(),
            buffered: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            pending_publications: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            pending_corrections: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            connected: false,
            dirty: false,
            forming_tail_sequence: None,
            live_session_generation: None,
            last_trade_sequence: None,
            history_boundary_unix_nanos: i64::MIN,
            overlay_base: None,
            indexed_overlay: BTreeMap::new(),
            provider_candle_count: None,
            provider_candle_timestamp: None,
            last_trade_at: None,
        })
    }

    pub(in crate::market_service) fn reset(&mut self, generation: ProviderGeneration) {
        self.generation = generation;
        self.bars.clear();
        self.buffered.clear();
        self.pending_publications.clear();
        self.pending_corrections.clear();
        self.connected = false;
        self.dirty = false;
        self.forming_tail_sequence = None;
        self.live_session_generation = None;
        self.last_trade_sequence = None;
        self.history_boundary_unix_nanos = i64::MIN;
        self.overlay_base = None;
        self.indexed_overlay.clear();
        self.provider_candle_count = None;
        self.provider_candle_timestamp = None;
        self.last_publication_at = self.clock.monotonic();
        self.last_trade_at = None;
        if let TradeLiveCadence::Tick { trades, forming } = &mut self.cadence {
            *forming = *trades;
        }
    }

    pub(in crate::market_service) fn coverage(&self) -> Option<(i64, i64)> {
        Some((
            self.bars.first()?.exchange_timestamp_unix_nanos,
            self.bars.last()?.exchange_timestamp_unix_nanos,
        ))
    }

    pub(in crate::market_service) fn begin_history_reseed(&mut self) {
        self.pending_publications.clear();
        self.pending_corrections.clear();
        self.dirty = false;
    }

    pub(in crate::market_service) fn cancel_history_reseed(&mut self) -> Result<(), String> {
        let buffered = std::mem::take(&mut self.buffered);
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
    pub(in crate::market_service) fn seed(
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
            .ok_or_else(|| "Trade-built live handoff requires history".to_string())?
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
        self.last_publication_at = self.clock.monotonic();
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
                "Trade-built live publication buffer overflowed",
            )?;
        }
        self.dirty = !self.pending_publications.is_empty();
        let buffered = std::mem::take(&mut self.buffered);
        for update in &buffered {
            if self.apply_update(update)? {
                self.dirty = true;
            }
        }
        Ok(())
    }

    pub(in crate::market_service) fn take_publication(&mut self) -> Option<LiveSeriesPublication> {
        if !self.connected || !self.dirty {
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

    pub(in crate::market_service) fn accept_trade(
        &mut self,
        trade: &MarketTrade,
    ) -> Result<(), String> {
        self.last_trade_at = Some(self.clock.monotonic());
        if self.apply_trade(trade)? {
            self.dirty = true;
        }
        Ok(())
    }

    pub(in crate::market_service) fn accept_indexed_trade(
        &mut self,
        mutation: &IndexedTradeMutation,
    ) -> Result<(), String> {
        if mutation.kind == super::super::IndexedTradeKind::New
            && mutation.trade.as_ref().is_some_and(|trade| {
                trade
                    .metadata
                    .timestamps
                    .exchange_unix_nanos
                    .is_some_and(|time| time > self.history_boundary_unix_nanos)
            })
        {
            self.last_trade_at = Some(self.clock.monotonic());
        }
        if self.apply_indexed_trade(mutation)? {
            self.dirty = true;
        }
        Ok(())
    }

    fn buffer_update(&mut self, update: TradeLiveUpdate) -> Result<(), String> {
        if self.buffered.len() == LIVE_BUFFER_CAPACITY {
            return Err("Trade-built history/live buffer overflowed".to_string());
        }
        self.buffered.push_back(update);
        Ok(())
    }

    pub(in crate::market_service) fn buffer_trade(
        &mut self,
        trade: &MarketTrade,
    ) -> Result<(), String> {
        self.buffer_update(TradeLiveUpdate::Append(trade.clone()))
    }

    pub(in crate::market_service) fn buffer_indexed_trade(
        &mut self,
        mutation: &IndexedTradeMutation,
    ) -> Result<(), String> {
        self.buffer_update(TradeLiveUpdate::Indexed(mutation.clone()))
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
            super::super::IndexedTradeKind::New => {
                if previous.is_some() {
                    return Err("Indexed trade identity was reused".to_string());
                }
            }
            super::super::IndexedTradeKind::Correction | super::super::IndexedTradeKind::Cancel => {
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
            if mutation.kind == super::super::IndexedTradeKind::New
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
            super::super::IndexedTradeKind::New | super::super::IndexedTradeKind::Correction => {
                let trade = mutation
                    .trade
                    .clone()
                    .ok_or_else(|| "Indexed trade mutation has no trade".to_string())?;
                if previous.is_none()
                    && self.indexed_overlay.len() == super::super::MAXIMUM_TRADE_BAR_OVERLAY
                {
                    return Err("Indexed trade overlay exceeded its bound".to_string());
                }
                self.live_session_generation = Some(trade.metadata.session_generation);
                self.indexed_overlay.insert(mutation.index.clone(), trade);
            }
            super::super::IndexedTradeKind::Cancel => {
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
        self.forming_tail_sequence = self.bars.last().map(|bar| bar.source_sequence);
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
                TradeLiveCadence::Calendar { period } => {
                    let last_bucket =
                        self.stamped_bar_bucket(last.exchange_timestamp_unix_nanos, period);
                    let trade_bucket = self.calendar.bar_bucket(
                        &self.series.instrument_id,
                        exchange_nanos.div_euclid(1_000_000),
                        period,
                    );
                    let Some(trade_bucket) = trade_bucket else {
                        continue;
                    };
                    if last_bucket == Some(trade_bucket) {
                        updated_rithmic_bar(last, trade, last.exchange_timestamp_unix_nanos)?
                    } else {
                        started_rithmic_bar(
                            last,
                            trade,
                            self.calendar_bar_open(exchange_nanos, period),
                        )?
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

    pub(in crate::market_service) fn accept_provider_candle(
        &mut self,
        symbol: &str,
        candle: ProviderCandle,
    ) -> Result<(), String> {
        if self.candle_symbol.as_deref() != Some(symbol) {
            return Ok(());
        }
        // A covering history request owns the seam while this handoff waits.
        // Its snapshot includes provider candles up to the new boundary; an
        // independent live-channel replay cannot safely revise the old seed.
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
            TradeLiveCadence::Calendar { period } => {
                self.stamped_bar_bucket(open_nanos, period)
                    == self.calendar.bar_bucket(
                        &self.series.instrument_id,
                        exchange_nanos.div_euclid(1_000_000),
                        period,
                    )
            }
            TradeLiveCadence::Tick { .. } => false,
        }
    }

    fn stamped_bar_bucket(
        &self,
        open_nanos: i64,
        period: calendar::CalendarBarPeriod,
    ) -> Option<i64> {
        self.calendar.stamped_bar_bucket(
            &self.series.instrument_id,
            open_nanos.div_euclid(1_000_000),
            period,
            self.calendar_stamp,
        )
    }

    fn calendar_bar_open(&self, exchange_nanos: i64, period: calendar::CalendarBarPeriod) -> i64 {
        self.calendar
            .bar_open_stamp(
                &self.series.instrument_id,
                exchange_nanos.div_euclid(1_000_000),
                period,
                self.calendar_stamp,
            )
            .saturating_mul(1_000_000)
    }

    pub(in crate::market_service) fn apply_trade(
        &mut self,
        trade: &MarketTrade,
    ) -> Result<bool, String> {
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
            return Err("Trade-built live session generation changed".to_string());
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
            .ok_or_else(|| "Trade-built live trade has no exchange timestamp".to_string())?;
        if self.last_trade_sequence.is_none() && exchange_nanos <= self.history_boundary_unix_nanos
        {
            return Ok(false);
        }
        let Some(last) = self.bars.last().copied() else {
            return Err("Trade-built live handoff has no history".to_string());
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
                        .ok_or_else(|| "Trade-built live timestamp overflowed".to_string())?;
                    started_rithmic_bar(
                        last,
                        trade,
                        start_seconds
                            .checked_mul(1_000_000_000)
                            .ok_or_else(|| "Trade-built live timestamp overflowed".to_string())?,
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
            TradeLiveCadence::Calendar { period } => {
                let last_bucket =
                    self.stamped_bar_bucket(last.exchange_timestamp_unix_nanos, period);
                let trade_bucket = self.calendar.bar_bucket(
                    &self.series.instrument_id,
                    exchange_nanos.div_euclid(1_000_000),
                    period,
                );
                let Some(trade_bucket) = trade_bucket else {
                    return Ok(false);
                };
                if last_bucket == Some(trade_bucket) {
                    updated_rithmic_bar(last, trade, last.exchange_timestamp_unix_nanos)?
                } else {
                    started_rithmic_bar(
                        last,
                        trade,
                        self.calendar_bar_open(exchange_nanos, period),
                    )?
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
        self.forming_tail_sequence = Some(next.source_sequence);
        self.live_session_generation = Some(trade.metadata.session_generation);
        self.last_trade_sequence = Some(trade.metadata.source_sequence);
        enqueue_bar_transition(
            &mut self.pending_publications,
            next,
            "Trade-built live publication buffer overflowed",
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

pub(in crate::market_service) fn updated_rithmic_bar(
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
        .ok_or_else(|| "Trade-built live volume overflowed".to_string())?;
    Ok(bar)
}

pub(in crate::market_service) fn started_rithmic_bar(
    completed: MarketBar,
    trade: &MarketTrade,
    exchange_timestamp_unix_nanos: i64,
) -> Result<MarketBar, String> {
    Ok(MarketBar {
        source_sequence: completed
            .source_sequence
            .checked_add(1)
            .ok_or_else(|| "Trade-built live sequence overflowed".to_string())?,
        exchange_timestamp_seconds: exchange_timestamp_unix_nanos.div_euclid(1_000_000_000),
        exchange_timestamp_unix_nanos,
        open: trade.price,
        high: trade.price,
        low: trade.price,
        close: trade.price,
        volume: trade.quantity,
    })
}
