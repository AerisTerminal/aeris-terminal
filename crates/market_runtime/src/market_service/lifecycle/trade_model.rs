//! Trade aggregation for the coordinator-owned series lifecycle.
#[cfg(test)]
use super::super::InstallProviderInstrument;
use super::super::realtime::enqueue_bar_transition;
use super::super::{
    BarPeriod, BarSeriesKey, FormingBar, LIVE_BUFFER_CAPACITY, LIVE_HANDOFF_HISTORY_BARS,
    MarketBar, MarketTrade, ProviderGeneration, VecDeque, calendar, clock,
};
use std::{sync::Arc, time::Instant};

pub(in crate::market_service) struct TradeModel {
    pub(in crate::market_service) clock: Arc<dyn clock::Clock>,
    pub(in crate::market_service) calendar: calendar::MarketCalendarService,
    pub(in crate::market_service) series: BarSeriesKey,
    pub(in crate::market_service) generation: ProviderGeneration,
    cadence: TradeLiveCadence,
    calendar_stamp: calendar::CalendarBarStamp,
    pub(in crate::market_service) price_scale: u8,
    pub(in crate::market_service) quantity_scale: u8,
    pub(in crate::market_service) bars: Vec<MarketBar>,
    pub(in crate::market_service) buffered: VecDeque<MarketTrade>,
    pub(in crate::market_service) pending_publications: VecDeque<MarketBar>,
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
    pub(in crate::market_service) last_publication_at: Instant,
    pub(in crate::market_service) last_trade_at: Option<Instant>,
    /// Stall recoveries since the last trade; the watchdog waits longer after each.
    pub(in crate::market_service) quiet_stall_recoveries: u32,
    /// Covering history exhausted its retry budget, so the trade stream alone
    /// builds bars until a covering snapshot rejoins and clears this flag.
    pub(in crate::market_service) live_only: bool,
}

enum TradeLiveCadence {
    Fixed { seconds: i64 },
    Tick { trades: u32, forming: u32 },
    Calendar { period: calendar::CalendarBarPeriod },
}

impl TradeModel {
    #[cfg(test)]
    pub(in crate::market_service) fn new(
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        venue_id: &str,
    ) -> Option<Self> {
        Self::with_clock(
            series,
            generation,
            venue_id,
            std::sync::Arc::new(super::super::clock::SystemClock),
        )
    }

    #[cfg(test)]
    pub(in crate::market_service) fn with_clock(
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        venue_id: &str,
        clock: std::sync::Arc<dyn super::super::clock::Clock>,
    ) -> Option<Self> {
        let mut calendar = super::super::calendar::MarketCalendarService::default();
        calendar.resolve(&InstallProviderInstrument {
            instrument_id: series.instrument_id.clone(),
            venue_id: venue_id.into(),
            ..Default::default()
        });
        Self::with_calendar(
            series,
            generation,
            clock,
            calendar,
            calendar::CalendarBarStamp::SessionOpen,
        )
    }

    pub(in crate::market_service) fn with_calendar(
        series: &BarSeriesKey,
        generation: ProviderGeneration,
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
        Some(Self {
            last_publication_at: clock.monotonic(),
            clock,
            calendar,
            series: series.clone(),
            generation,
            cadence,
            calendar_stamp,
            price_scale: 0,
            quantity_scale: 0,
            bars: Vec::new(),
            buffered: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            pending_publications: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            connected: false,
            dirty: false,
            forming_tail_sequence: None,
            live_session_generation: None,
            last_trade_sequence: None,
            history_boundary_unix_nanos: i64::MIN,
            last_trade_at: None,
            quiet_stall_recoveries: 0,
            live_only: false,
        })
    }

    pub(in crate::market_service) fn reset(&mut self, generation: ProviderGeneration) {
        self.generation = generation;
        self.bars.clear();
        self.buffered.clear();
        self.pending_publications.clear();
        self.connected = false;
        self.dirty = false;
        self.forming_tail_sequence = None;
        self.live_session_generation = None;
        self.last_trade_sequence = None;
        self.history_boundary_unix_nanos = i64::MIN;
        self.last_publication_at = self.clock.monotonic();
        self.last_trade_at = None;
        self.quiet_stall_recoveries = 0;
        self.live_only = false;
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
        self.dirty = false;
    }

    pub(in crate::market_service) fn cancel_history_reseed(&mut self) -> Result<(), String> {
        self.replay_buffered()
    }

    pub(in crate::market_service) fn replay_buffered(&mut self) -> Result<(), String> {
        let buffered = std::mem::take(&mut self.buffered);
        for trade in &buffered {
            if self.apply_trade(trade)? {
                self.dirty = true;
            }
        }
        Ok(())
    }

    /// Opens the live edge without covering history. The first trade anchors
    /// the first bar; a later covering snapshot reseeds and clears the flag.
    pub(in crate::market_service) fn seed_live_only(
        &mut self,
        price_scale: u8,
        quantity_scale: u8,
    ) -> Result<(), String> {
        self.pending_publications.clear();
        self.price_scale = price_scale;
        self.quantity_scale = quantity_scale;
        self.forming_tail_sequence = None;
        self.bars.clear();
        self.history_boundary_unix_nanos = i64::MIN;
        self.live_session_generation = None;
        self.last_trade_sequence = None;
        self.last_publication_at = self.clock.monotonic();
        self.last_trade_at = None;
        self.live_only = true;
        self.replay_buffered()
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
        self.replay_buffered()
    }

    pub(in crate::market_service) fn take_publication(&mut self) -> Option<Vec<MarketBar>> {
        if !self.connected {
            return None;
        }
        self.drain_publication()
    }

    pub(in crate::market_service) fn drain_publication(&mut self) -> Option<Vec<MarketBar>> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        let tails = self.pending_publications.drain(..).collect::<Vec<_>>();
        (!tails.is_empty()).then_some(tails)
    }

    pub(in crate::market_service) fn accept_trade(
        &mut self,
        trade: &MarketTrade,
    ) -> Result<(), String> {
        self.last_trade_at = Some(self.clock.monotonic());
        self.quiet_stall_recoveries = 0;
        if self.apply_trade(trade)? {
            self.dirty = true;
        }
        Ok(())
    }

    pub(in crate::market_service) fn buffer_trade(
        &mut self,
        trade: &MarketTrade,
    ) -> Result<(), String> {
        if self.buffered.len() == LIVE_BUFFER_CAPACITY {
            return Err("Trade-built history/live buffer overflowed".to_string());
        }
        self.quiet_stall_recoveries = 0;
        self.buffered.push_back(trade.clone());
        Ok(())
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
        // Bars are stamped on the history clock, so live trades bucket on it too.
        let exchange_nanos = trade
            .metadata
            .timestamps
            .history_clock_unix_nanos()
            .ok_or_else(|| "Trade-built live trade has no history-clock timestamp".to_string())?;
        if self.last_trade_sequence.is_none() && exchange_nanos <= self.history_boundary_unix_nanos
        {
            return Ok(false);
        }
        let Some(last) = self.bars.last().copied() else {
            return self.start_live_only_bar(trade, exchange_nanos);
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

    /// Anchors the first bar of a live-only series; every later bar follows
    /// the normal cadence transition from it.
    fn start_live_only_bar(
        &mut self,
        trade: &MarketTrade,
        exchange_nanos: i64,
    ) -> Result<bool, String> {
        if !self.live_only {
            return Err("Trade-built live handoff has no history".to_string());
        }
        let open_nanos = match self.cadence {
            TradeLiveCadence::Fixed { seconds } => {
                let trade_seconds = exchange_nanos.div_euclid(1_000_000_000);
                trade_seconds
                    .div_euclid(seconds)
                    .checked_mul(seconds)
                    .and_then(|open| open.checked_mul(1_000_000_000))
                    .ok_or_else(|| "Trade-built live timestamp overflowed".to_string())?
            }
            TradeLiveCadence::Tick {
                ref mut forming, ..
            } => {
                *forming = 1;
                exchange_nanos
            }
            TradeLiveCadence::Calendar { period } => {
                if self
                    .calendar
                    .bar_bucket(
                        &self.series.instrument_id,
                        exchange_nanos.div_euclid(1_000_000),
                        period,
                    )
                    .is_none()
                {
                    return Ok(false);
                }
                self.calendar_bar_open(exchange_nanos, period)
            }
        };
        let bar = MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: open_nanos.div_euclid(1_000_000_000),
            exchange_timestamp_unix_nanos: open_nanos,
            open: trade.price,
            high: trade.price,
            low: trade.price,
            close: trade.price,
            volume: trade.quantity,
        };
        bar.validate().map_err(|error| error.to_string())?;
        self.bars.push(bar);
        self.forming_tail_sequence = Some(bar.source_sequence);
        self.live_session_generation = Some(trade.metadata.session_generation);
        self.last_trade_sequence = Some(trade.metadata.source_sequence);
        enqueue_bar_transition(
            &mut self.pending_publications,
            bar,
            "Trade-built live publication buffer overflowed",
        )?;
        self.dirty = true;
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
