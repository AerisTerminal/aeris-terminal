//! Coordinator-owned per-series market lifecycle.
mod handlers;
mod trade_model;
pub(super) use trade_model::TradeModel;

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::Instant,
};

use aeris_instruments::SessionPhase;
use aeris_market_data::{BarSeriesKey, MarketBar};
use aeris_market_engine::ProviderGeneration;
use aeris_market_engine::fence::{Epoch, ProviderTag};

use super::{
    Coordinator, LIVE_BUFFER_CAPACITY, MarketTrade, SeriesLoadState, TradeHandoffSeed,
    clock::Clock, history::tracker::HistoryTicket,
};
pub(super) use crate::SeriesPresentationStatus as PresentationStatus;

/// The sole in-flight candle for the selected series. Completed bars belong to `MarketEngine`.
#[derive(Clone, Copy)]
pub(crate) struct FormingBar {
    pub(crate) bar: MarketBar,
    pub(crate) trades: Option<u32>,
}

enum LifecycleModel {
    TradeHandoff(Box<TradeModel>),
    CandleOverlay,
}

#[derive(Default)]
pub(super) struct SeriesLifecycles(BTreeMap<BarSeriesKey, SeriesLifecycle>);

impl SeriesLifecycles {
    pub(super) fn trade_contains_key(&self, series: &BarSeriesKey) -> bool {
        self.trade(series).is_some()
    }
    pub(super) fn candle_contains_key(&self, series: &BarSeriesKey) -> bool {
        self.candle(series).is_some()
    }
    #[cfg(test)]
    pub(super) fn trades_are_empty(&self) -> bool {
        self.trade_keys().next().is_none()
    }
    #[cfg(test)]
    pub(super) fn candles_are_empty(&self) -> bool {
        self.candle_keys().next().is_none()
    }
    pub(super) fn trade(&self, series: &BarSeriesKey) -> Option<&TradeModel> {
        self.0.get(series)?.trade_model()
    }
    pub(super) fn trade_mut(&mut self, series: &BarSeriesKey) -> Option<&mut TradeModel> {
        self.0.get_mut(series)?.trade_model_mut()
    }
    pub(super) fn trade_lifecycle(&self, series: &BarSeriesKey) -> Option<&SeriesLifecycle> {
        self.0
            .get(series)
            .filter(|live| live.trade_model().is_some())
    }
    pub(super) fn trade_lifecycle_mut(
        &mut self,
        series: &BarSeriesKey,
    ) -> Option<&mut SeriesLifecycle> {
        self.0
            .get_mut(series)
            .filter(|live| live.trade_model().is_some())
    }
    pub(super) fn candle(&self, series: &BarSeriesKey) -> Option<&SeriesLifecycle> {
        self.0.get(series).filter(|live| live.is_candle())
    }
    pub(super) fn candle_mut(&mut self, series: &BarSeriesKey) -> Option<&mut SeriesLifecycle> {
        self.0.get_mut(series).filter(|live| live.is_candle())
    }
    pub(super) fn insert_trade(&mut self, series: BarSeriesKey, live: TradeModel) {
        self.0.insert(series, SeriesLifecycle::from_trade(live));
    }
    pub(super) fn insert_candle(&mut self, series: BarSeriesKey, live: SeriesLifecycle) {
        self.0.insert(series, live);
    }
    pub(super) fn trade_keys(&self) -> impl Iterator<Item = &BarSeriesKey> {
        self.0
            .iter()
            .filter_map(|(series, live)| live.trade_model().map(|_| series))
    }
    pub(super) fn candle_keys(&self) -> impl Iterator<Item = &BarSeriesKey> {
        self.0
            .iter()
            .filter_map(|(series, live)| live.is_candle().then_some(series))
    }
    pub(super) fn keys(&self) -> impl Iterator<Item = &BarSeriesKey> {
        self.0.keys()
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = (&BarSeriesKey, &SeriesLifecycle)> {
        self.0.iter()
    }
    pub(super) fn get_mut(&mut self, series: &BarSeriesKey) -> Option<&mut SeriesLifecycle> {
        self.0.get_mut(series)
    }
    pub(super) fn iter_mut(
        &mut self,
    ) -> impl Iterator<Item = (&BarSeriesKey, &mut SeriesLifecycle)> {
        self.0.iter_mut()
    }
    pub(super) fn candle_iter(&self) -> impl Iterator<Item = (&BarSeriesKey, &SeriesLifecycle)> {
        self.0.iter().filter(|(_, live)| live.is_candle())
    }
    pub(super) fn candle_iter_mut(
        &mut self,
    ) -> impl Iterator<Item = (&BarSeriesKey, &mut SeriesLifecycle)> {
        self.0.iter_mut().filter(|(_, live)| live.is_candle())
    }
    pub(super) fn trade_lifecycle_iter_mut(
        &mut self,
    ) -> impl Iterator<Item = (&BarSeriesKey, &mut SeriesLifecycle)> {
        self.0
            .iter_mut()
            .filter(|(_, live)| live.trade_model().is_some())
    }
    pub(super) fn retain_trades(
        &mut self,
        mut keep: impl FnMut(&BarSeriesKey, &mut TradeModel) -> bool,
    ) {
        self.0.retain(|series, live| {
            live.trade_model_mut()
                .is_none_or(|model| keep(series, model))
        });
    }
    pub(super) fn retain_candles(
        &mut self,
        mut keep: impl FnMut(&BarSeriesKey, &mut SeriesLifecycle) -> bool,
    ) {
        self.0
            .retain(|series, live| !live.is_candle() || keep(series, live));
    }
}

#[derive(Clone, Copy)]
pub(super) struct CandleHandoffSeed<'a> {
    pub(super) price_scale: u8,
    pub(super) quantity_scale: u8,
    pub(super) bars: &'a [MarketBar],
    pub(super) forming: Option<FormingBar>,
}

/// The current phase of a single demanded series; only the coordinator executes effects.
#[derive(Clone, Debug)]
pub(super) enum SeriesState {
    Requested {
        provider: Option<Epoch<ProviderTag>>,
    },
    Seeding {
        ticket: Option<HistoryTicket>,
    },
    Live {
        last_progress: Instant,
        observed_live: bool,
    },
    Dormant {
        phase: SessionPhase,
        until: Option<i64>,
    },
    Recovering {
        reason: RecoveryReason,
        ticket: Option<HistoryTicket>,
    },
    Failed {
        error: SeriesFailure,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecoveryReason {
    ProviderReconnected,
    SequenceGap,
    QueueOverflow,
    StalledWhileOpen,
    DetachedWindow,
    SessionRolled,
}

#[derive(Clone, Debug)]
pub(super) enum SeriesFailure {
    Provider(String),
    History(String),
}

pub(super) enum SeriesInput<'a> {
    Online(Epoch<ProviderTag>),
    HistoryRequested(&'a HistoryTicket),
    Seeded(Option<&'a HistoryTicket>),
    HistoryEmpty(Option<&'a HistoryTicket>),
    HistoryFailed,
    Candle,
    HistoryRetryExhausted,
    PhaseChanged(SessionPhase),
    ProviderOffline(Epoch<ProviderTag>),
    Recover(RecoveryReason),
    Failed(SeriesFailure),
}

pub(super) struct StepContext<'a> {
    pub(super) clock: &'a dyn Clock,
    pub(super) phase: SessionPhase,
    pub(super) next_transition: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SeriesEffect {
    SubscribeLive,
    RequestHistory,
    CancelHistory,
    PublishDelta,
    PublishCoveringSnapshot,
    ReportStatus(PresentationStatus),
}

/// Fixed-size effects, reused by the caller. Exceeding capacity is a programming error,
/// not a silently dropped transition.
#[derive(Default)]
pub(super) struct EffectBuf {
    items: [Option<SeriesEffect>; 8],
    len: usize,
}

impl EffectBuf {
    fn push(&mut self, effect: SeriesEffect) {
        assert!(
            self.len < self.items.len(),
            "series lifecycle effect capacity exceeded"
        );
        self.items[self.len] = Some(effect);
        self.len += 1;
    }

    pub(super) fn clear(&mut self) {
        self.items[..self.len].fill(None);
        self.len = 0;
    }

    fn iter(&self) -> impl Iterator<Item = SeriesEffect> + '_ {
        self.items[..self.len].iter().flatten().copied()
    }

    #[cfg(test)]
    fn status(&self) -> Option<PresentationStatus> {
        self.items[..self.len]
            .iter()
            .rev()
            .find_map(|item| match item {
                Some(SeriesEffect::ReportStatus(status)) => Some(*status),
                _ => None,
            })
    }
}

pub(super) struct SeriesLifecycle {
    model: LifecycleModel,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) series: BarSeriesKey,
    pub(super) generation: ProviderGeneration,
    wire_coin: String,
    wire_interval: String,
    pub(super) price_scale: u8,
    pub(super) quantity_scale: u8,
    pub(super) bars: Vec<MarketBar>,
    pub(super) forming: Option<FormingBar>,
    pub(super) buffered: VecDeque<MarketBar>,
    pub(super) pending_publications: VecDeque<MarketBar>,
    pub(super) connected: bool,
    pub(super) dirty: bool,
    pub(super) state: SeriesState,
    pub(super) epoch: Option<Epoch<ProviderTag>>,
    pub(super) stale_inputs: u64,
}

impl SeriesLifecycle {
    pub(super) fn from_trade(trade: TradeModel) -> Self {
        let mut lifecycle = Self::with_clock(
            trade.series.clone(),
            trade.generation,
            "",
            "",
            Arc::clone(&trade.clock),
        );
        lifecycle.connected = trade.connected;
        lifecycle.model = LifecycleModel::TradeHandoff(Box::new(trade));
        lifecycle
    }

    fn is_candle(&self) -> bool {
        matches!(self.model, LifecycleModel::CandleOverlay)
    }

    pub(super) fn matches_candle_symbol(
        &self,
        symbol: &super::provider_event::ProviderCandleSymbol,
    ) -> bool {
        self.is_candle() && symbol.matches(&self.wire_coin, &self.wire_interval)
    }

    pub(super) fn trade_model(&self) -> Option<&TradeModel> {
        match &self.model {
            LifecycleModel::TradeHandoff(model) => Some(model.as_ref()),
            LifecycleModel::CandleOverlay => None,
        }
    }

    pub(super) fn trade_model_mut(&mut self) -> Option<&mut TradeModel> {
        match &mut self.model {
            LifecycleModel::TradeHandoff(model) => Some(model.as_mut()),
            LifecycleModel::CandleOverlay => None,
        }
    }

    pub(super) fn begin_trade_history_reseed(&mut self) {
        if !self.history_ready() {
            return;
        }
        let Some(trade) = self.trade_model_mut() else {
            return;
        };
        trade.begin_history_reseed();
        self.state = SeriesState::Recovering {
            reason: RecoveryReason::DetachedWindow,
            ticket: None,
        };
    }

    pub(super) fn cancel_trade_history_reseed(&mut self) -> Result<(), String> {
        if !matches!(
            self.state,
            SeriesState::Recovering {
                reason: RecoveryReason::DetachedWindow,
                ..
            }
        ) {
            return Ok(());
        }
        let Some(trade) = self.trade_model_mut() else {
            return Ok(());
        };
        trade.cancel_history_reseed()?;
        self.state = SeriesState::Live {
            last_progress: self.clock.monotonic(),
            observed_live: false,
        };
        Ok(())
    }

    pub(super) fn seed_trade(&mut self, seed: TradeHandoffSeed<'_>) -> Result<(), String> {
        let Some(trade) = self.trade_model_mut() else {
            return Err("Trade-built lifecycle is unavailable".into());
        };
        trade.seed(
            seed.price_scale,
            seed.quantity_scale,
            seed.bars,
            seed.forming,
            seed.handoff_boundary_unix_nanos,
        )?;
        self.state = SeriesState::Live {
            last_progress: self.clock.monotonic(),
            observed_live: false,
        };
        Ok(())
    }

    /// Opens the live edge without covering history after its retry budget
    /// exhausted. Bars the stream already accumulated stay in place; a later
    /// covering snapshot reseeds and the coordinator clears the flag.
    pub(super) fn mark_trade_live_only(
        &mut self,
        price_scale: u8,
        quantity_scale: u8,
    ) -> Result<(), String> {
        let Some(trade) = self.trade_model_mut() else {
            return Err("Trade-built lifecycle is unavailable".into());
        };
        if trade.live_only {
            // Already announced; re-arm the live edge after a recovery attempt
            // and fold in the trades it buffered.
            trade.replay_buffered()?;
        } else {
            trade.seed_live_only(price_scale, quantity_scale)?;
        }
        self.state = SeriesState::Live {
            last_progress: self.clock.monotonic(),
            observed_live: false,
        };
        Ok(())
    }

    pub(super) fn recover_trade(&mut self, reason: RecoveryReason) {
        let reason = if self.dormant_session_rolled(self.clock.as_ref()) {
            RecoveryReason::SessionRolled
        } else {
            reason
        };
        let Some(trade) = self.trade_model_mut() else {
            return;
        };
        trade.dirty = false;
        trade.buffered.clear();
        trade.pending_publications.clear();
        self.state = SeriesState::Recovering {
            reason,
            ticket: None,
        };
    }

    pub(super) fn reset_trade(&mut self, generation: ProviderGeneration) {
        if let Some(trade) = self.trade_model_mut() {
            trade.reset(generation);
            self.generation = generation;
            self.connected = false;
            self.epoch = None;
            self.state = SeriesState::Requested { provider: None };
        }
    }

    pub(super) fn mark_trade_online(&mut self) -> bool {
        let ready = self.history_ready();
        let Some(trade) = self.trade_model_mut() else {
            return false;
        };
        trade.connected = true;
        self.connected = true;
        if ready {
            self.state = SeriesState::Live {
                last_progress: self.clock.monotonic(),
                observed_live: false,
            };
            true
        } else {
            false
        }
    }

    pub(super) fn accept_trade(&mut self, trade: &MarketTrade) -> Result<(), String> {
        if self.dormant_session_rolled(self.clock.as_ref()) {
            return Err("Trade arrived after dormant session boundary".into());
        }
        let ready = self.history_ready();
        let Some(model) = self.trade_model_mut() else {
            return Err("Trade-built lifecycle is unavailable".into());
        };
        if ready {
            model.accept_trade(trade)?;
        } else {
            model.buffer_trade(trade)?;
        }
        self.mark_trade_progress();
        Ok(())
    }

    pub(super) fn take_trade_publication(&mut self) -> Option<Vec<MarketBar>> {
        if !self.history_ready() {
            return None;
        }
        self.trade_model_mut()?.take_publication()
    }

    fn mark_trade_progress(&mut self) {
        if self.history_ready() && self.trade_model().is_some_and(|model| model.dirty) {
            self.state = SeriesState::Live {
                last_progress: self.clock.monotonic(),
                observed_live: true,
            };
        }
    }

    pub(super) fn with_clock(
        series: BarSeriesKey,
        generation: ProviderGeneration,
        wire_coin: &str,
        interval: &str,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            model: LifecycleModel::CandleOverlay,
            clock,
            series,
            generation,
            wire_coin: wire_coin.to_owned(),
            wire_interval: interval.to_owned(),
            price_scale: 0,
            quantity_scale: 0,
            bars: Vec::new(),
            forming: None,
            buffered: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            pending_publications: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            connected: false,
            dirty: false,
            state: SeriesState::Requested { provider: None },
            epoch: None,
            stale_inputs: 0,
        }
    }

    pub(super) fn state(&self) -> &SeriesState {
        &self.state
    }

    pub(super) fn last_progress(&self) -> Option<Instant> {
        match self.state {
            SeriesState::Live { last_progress, .. } => Some(last_progress),
            _ => None,
        }
    }

    pub(super) fn history_ready(&self) -> bool {
        matches!(
            self.state,
            SeriesState::Live { .. } | SeriesState::Dormant { .. }
        )
    }

    fn dormant_session_rolled(&self, clock: &dyn Clock) -> bool {
        matches!(
            self.state,
            SeriesState::Dormant { until: Some(until), .. }
                if clock.wall_nanos().is_some_and(|now| now.div_euclid(1_000_000) >= until)
        )
    }

    pub(super) fn presentation(&self) -> PresentationStatus {
        match self.state() {
            SeriesState::Requested { provider: None } => PresentationStatus::Loading,
            SeriesState::Seeding { .. }
            | SeriesState::Live {
                observed_live: false,
                ..
            } => PresentationStatus::ConnectingLive,
            SeriesState::Requested { provider: Some(_) } | SeriesState::Recovering { .. } => {
                PresentationStatus::Recovering
            }
            SeriesState::Live {
                observed_live: true,
                ..
            } => PresentationStatus::Live,
            SeriesState::Dormant { .. } | SeriesState::Failed { .. } => {
                PresentationStatus::AwaitingData
            }
        }
    }

    pub(super) fn awaiting_history(&self) -> bool {
        matches!(
            self.state,
            SeriesState::Requested { .. } | SeriesState::Recovering { .. }
        )
    }

    /// Computes control effects without touching the engine or performing I/O.
    pub(super) fn step(
        &mut self,
        input: SeriesInput<'_>,
        ctx: &StepContext<'_>,
        out: &mut EffectBuf,
    ) {
        match input {
            SeriesInput::Online(epoch) => self.step_online(epoch, out),
            SeriesInput::HistoryRequested(ticket) => self.step_history_requested(ticket),
            SeriesInput::Seeded(ticket) => self.step_seeded(ticket, ctx, out),
            SeriesInput::HistoryEmpty(ticket) => self.step_history_empty(ticket, ctx, out),
            other => self.step_live(other, ctx, out),
        }
    }

    fn step_online(&mut self, epoch: Epoch<ProviderTag>, out: &mut EffectBuf) {
        match epoch {
            epoch if self.epoch.is_some_and(|current| current > epoch) => {
                self.stale_inputs = self.stale_inputs.saturating_add(1);
            }
            epoch if self.epoch.is_none() && self.history_ready() => {
                self.epoch = Some(epoch);
            }
            epoch
                if self.epoch == Some(epoch)
                    && matches!(
                        self.state,
                        SeriesState::Seeding { ticket: Some(_) }
                            | SeriesState::Recovering {
                                ticket: Some(_),
                                ..
                            }
                    ) => {}
            epoch if self.epoch == Some(epoch) && self.history_ready() => {}
            epoch => {
                let reconnecting = matches!(
                    self.state,
                    SeriesState::Requested { provider: Some(_) } | SeriesState::Recovering { .. }
                );
                self.epoch = Some(epoch);
                self.state = SeriesState::Seeding { ticket: None };
                out.push(SeriesEffect::SubscribeLive);
                out.push(SeriesEffect::RequestHistory);
                out.push(SeriesEffect::ReportStatus(if reconnecting {
                    PresentationStatus::Recovering
                } else {
                    PresentationStatus::ConnectingLive
                }));
            }
        }
    }

    fn step_history_requested(&mut self, ticket: &HistoryTicket) {
        if self.epoch.is_some_and(|epoch| epoch != ticket.provider) {
            self.stale_inputs = self.stale_inputs.saturating_add(1);
            return;
        }
        self.epoch = Some(ticket.provider);
        match &mut self.state {
            SeriesState::Seeding { ticket: current }
            | SeriesState::Recovering {
                ticket: current, ..
            } => *current = Some(ticket.clone()),
            SeriesState::Requested { .. } => {
                self.state = SeriesState::Seeding {
                    ticket: Some(ticket.clone()),
                }
            }
            _ => self.stale_inputs = self.stale_inputs.saturating_add(1),
        }
    }

    fn current_ticket(&self, ticket: Option<&HistoryTicket>) -> bool {
        ticket.is_none_or(|ticket| {
            self.epoch == Some(ticket.provider)
                && matches!(&self.state,
                    SeriesState::Seeding { ticket: Some(current) }
                    | SeriesState::Recovering { ticket: Some(current), .. }
                    if current == ticket)
        })
    }

    fn step_seeded(
        &mut self,
        ticket: Option<&HistoryTicket>,
        ctx: &StepContext<'_>,
        out: &mut EffectBuf,
    ) {
        if !self.current_ticket(ticket) {
            self.stale_inputs = self.stale_inputs.saturating_add(1);
            return;
        }
        if matches!(ctx.phase, SessionPhase::Closed | SessionPhase::Break) {
            self.state = SeriesState::Dormant {
                phase: ctx.phase,
                until: ctx.next_transition,
            };
        } else {
            self.state = SeriesState::Live {
                last_progress: ctx.clock.monotonic(),
                observed_live: false,
            };
        }
        out.push(SeriesEffect::PublishCoveringSnapshot);
        out.push(SeriesEffect::ReportStatus(self.presentation()));
    }

    fn step_history_empty(
        &mut self,
        ticket: Option<&HistoryTicket>,
        ctx: &StepContext<'_>,
        out: &mut EffectBuf,
    ) {
        if !self.current_ticket(ticket) {
            self.stale_inputs = self.stale_inputs.saturating_add(1);
            return;
        }
        self.state = SeriesState::Dormant {
            phase: ctx.phase,
            until: ctx.next_transition,
        };
        out.push(SeriesEffect::ReportStatus(PresentationStatus::AwaitingData));
    }

    fn request_recovery(&mut self, reason: RecoveryReason, out: &mut EffectBuf) {
        self.state = SeriesState::Recovering {
            reason,
            ticket: None,
        };
        out.push(SeriesEffect::RequestHistory);
        out.push(SeriesEffect::ReportStatus(PresentationStatus::Recovering));
    }

    fn step_live(&mut self, input: SeriesInput<'_>, ctx: &StepContext<'_>, out: &mut EffectBuf) {
        match input {
            SeriesInput::Online(_)
            | SeriesInput::HistoryRequested(_)
            | SeriesInput::Seeded(_)
            | SeriesInput::HistoryEmpty(_) => unreachable!("dispatched before live transitions"),
            SeriesInput::HistoryFailed => {
                out.push(SeriesEffect::ReportStatus(PresentationStatus::Recovering));
            }
            SeriesInput::Candle if self.dormant_session_rolled(ctx.clock) => {
                self.request_recovery(RecoveryReason::SessionRolled, out);
            }
            SeriesInput::Candle if self.history_ready() => {
                self.state = SeriesState::Live {
                    last_progress: ctx.clock.monotonic(),
                    observed_live: true,
                };
                out.push(SeriesEffect::PublishDelta);
                out.push(SeriesEffect::ReportStatus(PresentationStatus::Live));
            }
            SeriesInput::Candle => {
                if self.buffered.len() == LIVE_BUFFER_CAPACITY {
                    self.buffered.clear();
                    self.request_recovery(RecoveryReason::QueueOverflow, out);
                }
            }
            SeriesInput::HistoryRetryExhausted => {
                self.state = SeriesState::Failed {
                    error: SeriesFailure::History("history retry budget exhausted".into()),
                };
                out.push(SeriesEffect::ReportStatus(PresentationStatus::AwaitingData));
            }
            SeriesInput::PhaseChanged(phase) if matches!(self.state, SeriesState::Dormant { phase: current, .. } if current == phase) =>
                {}
            SeriesInput::PhaseChanged(phase)
                if matches!(phase, SessionPhase::Closed | SessionPhase::Break) =>
            {
                self.state = SeriesState::Dormant {
                    phase,
                    until: ctx.next_transition,
                };
                out.push(SeriesEffect::ReportStatus(PresentationStatus::AwaitingData));
            }
            SeriesInput::PhaseChanged(_) if self.dormant_session_rolled(ctx.clock) => {
                self.request_recovery(RecoveryReason::SessionRolled, out);
            }
            SeriesInput::PhaseChanged(_) => {
                self.state = SeriesState::Live {
                    last_progress: ctx.clock.monotonic(),
                    observed_live: false,
                };
                out.push(SeriesEffect::ReportStatus(
                    PresentationStatus::ConnectingLive,
                ));
            }
            SeriesInput::ProviderOffline(epoch) if self.epoch != Some(epoch) => {
                self.stale_inputs = self.stale_inputs.saturating_add(1);
            }
            SeriesInput::ProviderOffline(_) => {
                self.state = SeriesState::Requested {
                    provider: self.epoch,
                };
                out.push(SeriesEffect::CancelHistory);
                out.push(SeriesEffect::ReportStatus(PresentationStatus::Recovering));
            }
            SeriesInput::Recover(RecoveryReason::StalledWhileOpen)
                if !matches!(
                    ctx.phase,
                    SessionPhase::Open | SessionPhase::Extended | SessionPhase::AlwaysOpen
                ) => {}
            SeriesInput::Recover(reason) => {
                let reason = if self.dormant_session_rolled(ctx.clock) {
                    RecoveryReason::SessionRolled
                } else {
                    reason
                };
                self.request_recovery(reason, out);
            }
            SeriesInput::Failed(error) => {
                self.state = SeriesState::Failed { error };
                out.push(SeriesEffect::ReportStatus(PresentationStatus::AwaitingData));
            }
        }
    }
}

impl Coordinator<'_> {
    /// The coordinator applies I/O effects after the per-series pure transition.
    pub(super) fn transition_candle_series(
        &mut self,
        series: &BarSeriesKey,
        input: SeriesInput<'_>,
    ) {
        let mut effects = std::mem::take(&mut self.series_effects);
        effects.clear();
        let Some(live) = self.series.candle_mut(series) else {
            self.series_effects = effects;
            return;
        };
        let at_utc_ms = self
            .clock
            .wall_nanos()
            .map(|nanos| nanos.div_euclid(1_000_000));
        let phase = at_utc_ms.map_or(SessionPhase::Unknown, |at| {
            self.calendar.phase(&series.instrument_id, at)
        });
        let next_transition = at_utc_ms.and_then(|at| {
            self.calendar
                .next_transition(&series.instrument_id, at)
                .map(|(at, _)| at)
        });
        live.step(
            input,
            &StepContext {
                clock: self.clock.as_ref(),
                phase,
                next_transition,
            },
            &mut effects,
        );
        let generation = live.generation;
        for effect in effects.iter() {
            match effect {
                SeriesEffect::RequestHistory => {
                    if self
                        .provider_epochs
                        .current(&series.provider_id, generation.0.get())
                        .is_none_or(|epoch| self.history.inflight(series, epoch).is_none())
                    {
                        self.request_series_history_recovery(series, generation);
                    }
                }
                SeriesEffect::CancelHistory => self.history.cancel_series(series),
                SeriesEffect::ReportStatus(presentation) => {
                    let state = if matches!(
                        self.series.candle(series).map(SeriesLifecycle::state),
                        Some(SeriesState::Failed { .. })
                    ) {
                        SeriesLoadState::Failed
                    } else {
                        match presentation {
                            PresentationStatus::Loading | PresentationStatus::ConnectingLive => {
                                SeriesLoadState::Resolving
                            }
                            PresentationStatus::Live => SeriesLoadState::Live,
                            PresentationStatus::Recovering
                            | PresentationStatus::HistoryUnavailable => SeriesLoadState::Partial,
                            PresentationStatus::AwaitingData => SeriesLoadState::Empty,
                        }
                    };
                    self.broadcast_candle_presentation(series, state, presentation);
                }
                SeriesEffect::SubscribeLive
                | SeriesEffect::PublishDelta
                | SeriesEffect::PublishCoveringSnapshot => {
                    // Demand has already been sent by ensure_realtime; the main
                    // publication turn drains the bounded per-series tail.
                }
            }
        }
        self.series_effects = effects;
    }

    fn broadcast_candle_presentation(
        &mut self,
        selected: &BarSeriesKey,
        state: SeriesLoadState,
        presentation: PresentationStatus,
    ) {
        for (consumer_id, events) in &mut self.events {
            let Some(demand) = self.engine.current_demand(*consumer_id) else {
                continue;
            };
            let (Some(generation), Some(series)) = (demand.generation, demand.series.as_ref())
            else {
                continue;
            };
            if series != selected {
                continue;
            }
            events.series_state = Some(super::series_state_payload(
                *consumer_id,
                generation,
                selected.clone(),
                state,
                None,
            ));
            if let Some(super::MarketRuntimeEvent::SeriesState(published)) =
                events.series_state.as_mut()
            {
                published.presentation = Some(presentation);
                if let Some(SeriesState::Failed { error }) =
                    self.series.candle(selected).map(SeriesLifecycle::state)
                {
                    published.detail = Some(match error {
                        SeriesFailure::Provider(detail) | SeriesFailure::History(detail) => {
                            detail.clone()
                        }
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market_service::clock::ScriptedClock;
    use crate::market_service::history::tracker::LaneKind;
    use aeris_market_data::BarPeriod;
    use aeris_market_engine::fence::{DemandTag, EpochSequence};
    use std::num::NonZeroU64;

    fn fixture() -> (
        SeriesLifecycle,
        Arc<ScriptedClock>,
        Epoch<ProviderTag>,
        HistoryTicket,
    ) {
        let clock = Arc::new(ScriptedClock::new(1_000_000_000));
        let series = BarSeriesKey {
            provider_id: "fixture".into(),
            instrument_id: "fixture:BTC".into(),
            entitlement_id: "fixture".into(),
            period: BarPeriod::time(60).expect("period"),
            definition_version: 1,
        };
        let lifecycle = SeriesLifecycle::with_clock(
            series.clone(),
            ProviderGeneration(NonZeroU64::new(1).expect("generation")),
            "BTC",
            "1m",
            clock.clone(),
        );
        let epoch = EpochSequence::<ProviderTag>::default().next();
        let ticket = HistoryTicket {
            series,
            provider: epoch,
            demand: EpochSequence::<DemandTag>::default().next(),
            lane: LaneKind::Current,
            seq: 1,
        };
        (lifecycle, clock, epoch, ticket)
    }

    fn step(
        lifecycle: &mut SeriesLifecycle,
        clock: &dyn Clock,
        phase: SessionPhase,
        input: SeriesInput<'_>,
    ) -> EffectBuf {
        let mut effects = EffectBuf::default();
        lifecycle.step(
            input,
            &StepContext {
                clock,
                phase,
                next_transition: Some(2_000),
            },
            &mut effects,
        );
        effects
    }

    fn seeded_live() -> (
        SeriesLifecycle,
        Arc<ScriptedClock>,
        Epoch<ProviderTag>,
        HistoryTicket,
    ) {
        let (mut lifecycle, clock, epoch, ticket) = fixture();
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Open,
            SeriesInput::Online(epoch),
        );
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Open,
            SeriesInput::HistoryRequested(&ticket),
        );
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Open,
            SeriesInput::Seeded(Some(&ticket)),
        );
        (lifecycle, clock, epoch, ticket)
    }

    #[test]
    fn cached_candle_demand_identity_matches_wire_fields_without_event_formatting() {
        use super::super::provider_event::ProviderCandleSymbol;

        let (live, _, _, _) = fixture();
        let event = |coin: &str, interval: &str| ProviderCandleSymbol {
            coin: coin.into(),
            interval: interval.into(),
        };
        assert!(live.matches_candle_symbol(&event("BTC", "1m")));
        assert!(!live.matches_candle_symbol(&event("ETH", "1m")));
        assert!(!live.matches_candle_symbol(&event("BTC", "5m")));
    }

    #[derive(Clone, Copy, Debug)]
    enum LifecycleCase {
        CandleOverlay,
        Sequence,
    }

    fn case_fixture(
        case: LifecycleCase,
    ) -> (
        SeriesLifecycle,
        Arc<ScriptedClock>,
        Epoch<ProviderTag>,
        HistoryTicket,
    ) {
        let (mut live, clock, epoch, mut ticket) = fixture();
        if matches!(case, LifecycleCase::Sequence) {
            live.series.provider_id = "rithmic".into();
            live.series.instrument_id = format!("{}:BTC", live.series.provider_id);
            ticket.series = live.series.clone();
            live = SeriesLifecycle::from_trade(
                TradeModel::with_clock(&live.series, live.generation, "CME", clock.clone())
                    .expect("time-series trade model"),
            );
        }
        (live, clock, epoch, ticket)
    }

    fn bar_at(minute: i64, sequence: u64) -> MarketBar {
        let nanos = minute * 60_000_000_000;
        MarketBar {
            source_sequence: sequence,
            exchange_timestamp_seconds: nanos.div_euclid(1_000_000_000),
            exchange_timestamp_unix_nanos: nanos,
            open: 100,
            high: 100,
            low: 100,
            close: 100,
            volume: 1,
        }
    }

    fn seed_case(
        case: LifecycleCase,
        live: &mut SeriesLifecycle,
        completed: &[MarketBar],
        forming: MarketBar,
    ) {
        let handoff_boundary = forming.exchange_timestamp_unix_nanos;
        let forming = Some(FormingBar {
            bar: forming,
            trades: Some(1),
        });
        match case {
            LifecycleCase::CandleOverlay => {
                live.seed(2, 0, completed, forming).expect("candle seam");
            }
            LifecycleCase::Sequence => {
                live.seed_trade(TradeHandoffSeed {
                    price_scale: 2,
                    quantity_scale: 0,
                    bars: completed,
                    forming,
                    handoff_boundary_unix_nanos: Some(handoff_boundary),
                })
                .expect("trade seam");
            }
        }
    }

    fn next_trade(live: &SeriesLifecycle) -> MarketTrade {
        use aeris_market_data::{AggressorSide, EventMetadata, QualifiedTimestamp};

        let timestamp = bar_at(3, 4).exchange_timestamp_unix_nanos + 1;
        MarketTrade {
            metadata: EventMetadata {
                provider_id: live.series.provider_id.clone(),
                instrument_id: live.series.instrument_id.clone(),
                entitlement_id: live.series.entitlement_id.clone(),
                source_sequence: 7,
                session_generation: 1,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(timestamp),
                    provider_unix_nanos: None,
                    received_unix_nanos: timestamp,
                },
            },
            trade_id: "next".into(),
            price: 101,
            quantity: 1,
            aggressor: AggressorSide::Unknown,
        }
    }

    fn ingest_case(case: LifecycleCase, live: &mut SeriesLifecycle) {
        if matches!(case, LifecycleCase::CandleOverlay) {
            live.accept_bar(bar_at(3, 4)).expect("next candle");
            return;
        }
        let trade = next_trade(live);
        live.accept_trade(&trade).expect("sequence trade");
    }

    fn assert_contiguous_one_forming(case: LifecycleCase, live: &SeriesLifecycle) {
        let (completed, forming) = match case {
            LifecycleCase::CandleOverlay => (
                live.bars.as_slice(),
                live.forming.expect("one forming candle").bar,
            ),
            LifecycleCase::Sequence => {
                let model = live.trade_model().expect("trade model");
                let (forming, completed) = model.bars.split_last().expect("forming bar");
                assert_eq!(model.forming_tail_sequence, Some(forming.source_sequence));
                (completed, *forming)
            }
        };
        assert_eq!(
            completed
                .iter()
                .map(|bar| bar.exchange_timestamp_unix_nanos)
                .collect::<Vec<_>>(),
            (0..3)
                .map(|minute| bar_at(minute, 1).exchange_timestamp_unix_nanos)
                .collect::<Vec<_>>(),
            "{case:?}: contiguous completed history"
        );
        assert_eq!(
            forming.exchange_timestamp_unix_nanos,
            bar_at(3, 4).exchange_timestamp_unix_nanos,
            "{case:?}: exactly one open period"
        );
    }

    /// A provider stamping history on its own clock also reports an exchange
    /// clock that can fall in an earlier period; live bars follow history's.
    #[test]
    fn trade_built_bars_bucket_on_the_history_clock() {
        let case = LifecycleCase::Sequence;
        let (mut live, clock, epoch, ticket) = case_fixture(case);
        for input in [
            SeriesInput::Online(epoch),
            SeriesInput::HistoryRequested(&ticket),
            SeriesInput::Seeded(Some(&ticket)),
        ] {
            step(&mut live, clock.as_ref(), SessionPhase::AlwaysOpen, input);
        }
        seed_case(case, &mut live, &[bar_at(0, 1), bar_at(1, 2)], bar_at(2, 3));
        live.connected = true;
        live.mark_trade_online();
        let next_period = bar_at(3, 4).exchange_timestamp_unix_nanos;
        let mut trade = next_trade(&live);
        trade.metadata.timestamps.exchange_unix_nanos = Some(next_period - 1);
        trade.metadata.timestamps.provider_unix_nanos = Some(next_period + 1);
        live.accept_trade(&trade).expect("live trade applies");
        let model = live.trade_model().expect("trade model");
        assert_eq!(
            model
                .bars
                .iter()
                .map(|bar| bar.exchange_timestamp_unix_nanos)
                .collect::<Vec<_>>(),
            (0..4)
                .map(|minute| bar_at(minute, 1).exchange_timestamp_unix_nanos)
                .collect::<Vec<_>>(),
            "the trade opens the history-clock period, not the exchange-clock one"
        );
    }

    #[test]
    fn contract_is_shared_by_candle_and_sequence_models() {
        for case in [LifecycleCase::CandleOverlay, LifecycleCase::Sequence] {
            let (mut live, clock, epoch, ticket) = case_fixture(case);
            let online = step(
                &mut live,
                clock.as_ref(),
                SessionPhase::AlwaysOpen,
                SeriesInput::Online(epoch),
            );
            assert!(
                matches!(live.state(), SeriesState::Seeding { .. }),
                "{case:?}"
            );
            assert!(
                online
                    .iter()
                    .any(|effect| effect == SeriesEffect::RequestHistory)
            );
            step(
                &mut live,
                clock.as_ref(),
                SessionPhase::AlwaysOpen,
                SeriesInput::HistoryRequested(&ticket),
            );
            let stale = HistoryTicket {
                seq: 2,
                ..ticket.clone()
            };
            step(
                &mut live,
                clock.as_ref(),
                SessionPhase::AlwaysOpen,
                SeriesInput::Seeded(Some(&stale)),
            );
            assert_eq!(live.stale_inputs, 1, "{case:?}: retired ticket rejected");
            assert!(!live.history_ready());
            step(
                &mut live,
                clock.as_ref(),
                SessionPhase::AlwaysOpen,
                SeriesInput::Seeded(Some(&ticket)),
            );
            seed_case(case, &mut live, &[bar_at(0, 1), bar_at(1, 2)], bar_at(2, 3));
            live.connected = true;
            if live.trade_model().is_some() {
                live.mark_trade_online();
            }
            assert!(live.history_ready(), "{case:?}: history-ready");
            ingest_case(case, &mut live);
            assert_contiguous_one_forming(case, &live);
            step(
                &mut live,
                clock.as_ref(),
                SessionPhase::Open,
                SeriesInput::Recover(RecoveryReason::SequenceGap),
            );
            if live.trade_model().is_some() {
                live.recover_trade(RecoveryReason::SequenceGap);
            }
            assert!(
                matches!(
                    live.state(),
                    SeriesState::Recovering {
                        reason: RecoveryReason::SequenceGap,
                        ..
                    }
                ),
                "{case:?}"
            );
            let rejoin_ticket = HistoryTicket {
                seq: 3,
                ..ticket.clone()
            };
            step(
                &mut live,
                clock.as_ref(),
                SessionPhase::Open,
                SeriesInput::HistoryRequested(&rejoin_ticket),
            );
            step(
                &mut live,
                clock.as_ref(),
                SessionPhase::Open,
                SeriesInput::Seeded(Some(&rejoin_ticket)),
            );
            seed_case(
                case,
                &mut live,
                &[bar_at(0, 1), bar_at(1, 2), bar_at(2, 3)],
                bar_at(3, 4),
            );
            assert_contiguous_one_forming(case, &live);
            assert!(live.history_ready(), "{case:?}: rejoined after recovery");
        }
    }

    #[test]
    fn lifecycle_requested_online_subscribes_and_requests_current_history() {
        let (mut lifecycle, clock, epoch, _) = fixture();
        assert_eq!(lifecycle.presentation(), PresentationStatus::Loading);
        let effects = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Online(epoch),
        );
        assert!(matches!(lifecycle.state(), SeriesState::Seeding { .. }));
        assert!(
            effects
                .iter()
                .any(|effect| effect == SeriesEffect::SubscribeLive)
        );
        assert!(
            effects
                .iter()
                .any(|effect| effect == SeriesEffect::RequestHistory)
        );
        assert_eq!(effects.status(), Some(PresentationStatus::ConnectingLive));
    }

    #[test]
    fn lifecycle_matching_seed_merges_and_publishes_live_or_dormant() {
        let (mut lifecycle, clock, epoch, ticket) = fixture();
        let ctx = StepContext {
            clock: clock.as_ref(),
            phase: SessionPhase::AlwaysOpen,
            next_transition: None,
        };
        let mut effects = EffectBuf::default();
        lifecycle.step(SeriesInput::Online(epoch), &ctx, &mut effects);
        lifecycle.step(SeriesInput::HistoryRequested(&ticket), &ctx, &mut effects);
        assert!(matches!(lifecycle.state(), SeriesState::Seeding { .. }));
        assert_eq!(effects.status(), Some(PresentationStatus::ConnectingLive));
        lifecycle
            .accept_bar(bar_at(2, 3))
            .expect("buffer live candle");
        let seeding = lifecycle.state.clone();
        lifecycle
            .seed(
                2,
                0,
                &[bar_at(0, 1)],
                Some(FormingBar {
                    bar: bar_at(1, 2),
                    trades: None,
                }),
            )
            .expect("merge buffered live candle into history seam");
        lifecycle.state = seeding;
        assert_eq!(
            lifecycle
                .bars
                .iter()
                .map(|bar| bar.source_sequence)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(
            lifecycle.forming.map(|forming| forming.bar.source_sequence),
            Some(3)
        );
        assert!(lifecycle.buffered.is_empty());
        effects.clear();
        lifecycle.step(SeriesInput::Seeded(Some(&ticket)), &ctx, &mut effects);
        assert!(matches!(lifecycle.state(), SeriesState::Live { .. }));
        assert_eq!(effects.status(), Some(PresentationStatus::ConnectingLive));
        assert!(
            effects
                .iter()
                .any(|effect| effect == SeriesEffect::PublishCoveringSnapshot)
        );

        let (mut closed, clock, epoch, ticket) = fixture();
        step(
            &mut closed,
            clock.as_ref(),
            SessionPhase::Closed,
            SeriesInput::Online(epoch),
        );
        step(
            &mut closed,
            clock.as_ref(),
            SessionPhase::Closed,
            SeriesInput::HistoryRequested(&ticket),
        );
        let effects = step(
            &mut closed,
            clock.as_ref(),
            SessionPhase::Closed,
            SeriesInput::Seeded(Some(&ticket)),
        );
        assert!(matches!(
            closed.state(),
            SeriesState::Dormant {
                phase: SessionPhase::Closed,
                ..
            }
        ));
        assert_eq!(effects.status(), Some(PresentationStatus::AwaitingData));
        assert!(
            effects
                .iter()
                .any(|effect| effect == SeriesEffect::PublishCoveringSnapshot)
        );
    }

    #[test]
    fn lifecycle_seeding_buffers_bounded_candles_then_recovers_on_overflow() {
        let (mut lifecycle, clock, epoch, _) = fixture();
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Online(epoch),
        );
        for sequence in 0..LIVE_BUFFER_CAPACITY - 1 {
            lifecycle
                .accept_bar(bar_at(
                    i64::try_from(sequence).expect("buffer index fits i64"),
                    sequence as u64 + 1,
                ))
                .expect("bounded buffer append");
        }
        assert_eq!(lifecycle.buffered.len(), LIVE_BUFFER_CAPACITY - 1);
        assert!(matches!(lifecycle.state(), SeriesState::Seeding { .. }));
        let before = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Candle,
        );
        assert_eq!(lifecycle.buffered.len(), LIVE_BUFFER_CAPACITY - 1);
        assert!(before.iter().next().is_none());
        let overflow_minute =
            i64::try_from(LIVE_BUFFER_CAPACITY).expect("buffer capacity fits i64");
        lifecycle
            .accept_bar(bar_at(overflow_minute, LIVE_BUFFER_CAPACITY as u64 + 1))
            .expect("last bounded append");
        assert_eq!(lifecycle.buffered.len(), LIVE_BUFFER_CAPACITY);
        assert!(
            lifecycle
                .accept_bar(bar_at(overflow_minute, LIVE_BUFFER_CAPACITY as u64 + 1))
                .is_err()
        );
        let effects = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Candle,
        );
        assert!(lifecycle.buffered.is_empty());
        assert!(matches!(
            lifecycle.state(),
            SeriesState::Recovering {
                reason: RecoveryReason::QueueOverflow,
                ..
            }
        ));
        assert!(
            effects
                .iter()
                .any(|effect| effect == SeriesEffect::RequestHistory)
        );
    }

    #[test]
    fn lifecycle_stale_epoch_and_ticket_are_counted_without_transition() {
        let (mut lifecycle, clock, epoch, ticket) = fixture();
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Online(epoch),
        );
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::HistoryRequested(&ticket),
        );
        let stale = HistoryTicket {
            seq: 2,
            ..ticket.clone()
        };
        let effects = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Seeded(Some(&stale)),
        );
        assert_eq!(lifecycle.stale_inputs, 1);
        assert!(effects.iter().next().is_none());
        assert!(matches!(lifecycle.state(), SeriesState::Seeding { .. }));
        let mut epochs = EpochSequence::<ProviderTag>::default();
        let _current = epochs.next();
        let newer = epochs.next();
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Online(newer),
        );
        let effects = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::ProviderOffline(epoch),
        );
        assert_eq!(lifecycle.stale_inputs, 2);
        assert!(effects.iter().next().is_none());
        let effects = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Online(epoch),
        );
        assert_eq!(lifecycle.stale_inputs, 3);
        assert!(effects.iter().next().is_none());
        assert_eq!(lifecycle.epoch, Some(newer));
        assert!(matches!(lifecycle.state(), SeriesState::Seeding { .. }));
    }

    #[test]
    fn empty_history_reports_awaiting_data() {
        let (mut lifecycle, clock, epoch, ticket) = fixture();
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Online(epoch),
        );
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::HistoryRequested(&ticket),
        );
        let effects = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::HistoryEmpty(Some(&ticket)),
        );
        assert!(matches!(lifecycle.state(), SeriesState::Dormant { .. }));
        assert_eq!(effects.status(), Some(PresentationStatus::AwaitingData));
    }

    #[test]
    fn lifecycle_history_error_preserves_seeding_until_retry_exhaustion() {
        let (mut lifecycle, clock, epoch, ticket) = fixture();
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Online(epoch),
        );
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::HistoryRequested(&ticket),
        );
        let retry = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::HistoryFailed,
        );
        assert!(matches!(lifecycle.state(), SeriesState::Seeding { .. }));
        assert_eq!(retry.status(), Some(PresentationStatus::Recovering));
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::HistoryRetryExhausted,
        );
        assert!(matches!(lifecycle.state(), SeriesState::Failed { .. }));
    }

    #[test]
    fn lifecycle_live_candle_closes_bar_and_publishes_delta() {
        let (mut lifecycle, clock, _, _) = seeded_live();
        lifecycle
            .seed(
                2,
                0,
                &[bar_at(0, 1)],
                Some(FormingBar {
                    bar: bar_at(1, 2),
                    trades: None,
                }),
            )
            .expect("initial live seam");
        lifecycle
            .accept_bar(bar_at(2, 3))
            .expect("contiguous candle");
        let effects = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Open,
            SeriesInput::Candle,
        );
        assert_eq!(
            lifecycle.bars.last().map(|bar| bar.source_sequence),
            Some(2)
        );
        assert_eq!(
            lifecycle.forming.map(|forming| forming.bar.source_sequence),
            Some(3)
        );
        assert!(matches!(
            lifecycle.state(),
            SeriesState::Live {
                observed_live: true,
                ..
            }
        ));
        assert!(
            effects
                .iter()
                .any(|effect| effect == SeriesEffect::PublishDelta)
        );
        assert_eq!(effects.status(), Some(PresentationStatus::Live));
    }

    #[test]
    fn lifecycle_live_gap_and_queue_overflow_request_recovery() {
        for reason in [RecoveryReason::SequenceGap, RecoveryReason::QueueOverflow] {
            let (mut lifecycle, clock, _, _) = seeded_live();
            let effects = step(
                &mut lifecycle,
                clock.as_ref(),
                SessionPhase::Open,
                SeriesInput::Recover(reason),
            );
            assert!(
                matches!(lifecycle.state(), SeriesState::Recovering { reason: actual, .. } if *actual == reason)
            );
            assert!(
                effects
                    .iter()
                    .any(|effect| effect == SeriesEffect::RequestHistory)
            );
            assert_eq!(effects.status(), Some(PresentationStatus::Recovering));
        }
    }

    #[test]
    fn lifecycle_open_stall_recovers_but_closed_stall_does_not() {
        let (mut lifecycle, clock, _, _) = seeded_live();
        let closed = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Closed,
            SeriesInput::Recover(RecoveryReason::StalledWhileOpen),
        );
        assert!(matches!(lifecycle.state(), SeriesState::Live { .. }));
        assert_eq!(closed.status(), None);
        let open = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Open,
            SeriesInput::Recover(RecoveryReason::StalledWhileOpen),
        );
        assert!(matches!(
            lifecycle.state(),
            SeriesState::Recovering {
                reason: RecoveryReason::StalledWhileOpen,
                ..
            }
        ));
        assert_eq!(open.status(), Some(PresentationStatus::Recovering));
        assert!(
            open.iter()
                .any(|effect| effect == SeriesEffect::RequestHistory)
        );
    }

    #[test]
    fn lifecycle_closed_phase_enters_dormant() {
        let (mut lifecycle, clock, _, _) = seeded_live();
        let effects = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Closed,
            SeriesInput::PhaseChanged(SessionPhase::Closed),
        );
        assert!(matches!(
            lifecycle.state(),
            SeriesState::Dormant {
                phase: SessionPhase::Closed,
                until: Some(2_000)
            }
        ));
        assert_eq!(effects.status(), Some(PresentationStatus::AwaitingData));
    }

    #[test]
    fn lifecycle_dormant_reopens_or_reseeds_after_session_roll() {
        let (mut lifecycle, clock, _, _) = seeded_live();
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Closed,
            SeriesInput::PhaseChanged(SessionPhase::Closed),
        );
        let reopened = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Open,
            SeriesInput::PhaseChanged(SessionPhase::Open),
        );
        assert!(matches!(lifecycle.state(), SeriesState::Live { .. }));
        assert_eq!(reopened.status(), Some(PresentationStatus::ConnectingLive));
        assert!(
            !reopened
                .iter()
                .any(|effect| effect == SeriesEffect::RequestHistory)
        );

        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Break,
            SeriesInput::PhaseChanged(SessionPhase::Break),
        );
        clock.advance(std::time::Duration::from_secs(3));
        let effects = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Open,
            SeriesInput::PhaseChanged(SessionPhase::Open),
        );
        assert!(matches!(
            lifecycle.state(),
            SeriesState::Recovering {
                reason: RecoveryReason::SessionRolled,
                ..
            }
        ));
        assert!(
            effects
                .iter()
                .any(|effect| effect == SeriesEffect::RequestHistory)
        );

        let (mut trade, _, _, _) = case_fixture(LifecycleCase::Sequence);
        seed_case(
            LifecycleCase::Sequence,
            &mut trade,
            &[bar_at(0, 1), bar_at(1, 2)],
            bar_at(2, 3),
        );
        trade.state = SeriesState::Dormant {
            phase: SessionPhase::Break,
            until: None,
        };
        ingest_case(LifecycleCase::Sequence, &mut trade);
        assert!(
            matches!(
                trade.state(),
                SeriesState::Live {
                    observed_live: true,
                    ..
                }
            ),
            "trade during Dormant resumes live series"
        );
    }

    #[test]
    fn lifecycle_rolled_dormant_candle_requests_session_reseed() {
        let (mut lifecycle, clock, _, _) = seeded_live();
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Closed,
            SeriesInput::PhaseChanged(SessionPhase::Closed),
        );
        clock.advance(std::time::Duration::from_secs(3));
        let effects = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Open,
            SeriesInput::Candle,
        );
        assert!(matches!(
            lifecycle.state(),
            SeriesState::Recovering {
                reason: RecoveryReason::SessionRolled,
                ..
            }
        ));
        assert!(
            effects
                .iter()
                .any(|effect| effect == SeriesEffect::RequestHistory)
        );
        assert!(
            !effects
                .iter()
                .any(|effect| effect == SeriesEffect::PublishDelta)
        );
    }

    #[test]
    fn lifecycle_rolled_dormant_trade_rejects_mutation_and_recovers_session() {
        let case = LifecycleCase::Sequence;
        let (mut lifecycle, clock, _, _) = case_fixture(case);
        seed_case(
            case,
            &mut lifecycle,
            &[bar_at(0, 1), bar_at(1, 2)],
            bar_at(2, 3),
        );
        lifecycle.state = SeriesState::Dormant {
            phase: SessionPhase::Closed,
            until: Some(2_000),
        };
        clock.advance(std::time::Duration::from_secs(3));
        let trade = next_trade(&lifecycle);
        let before = lifecycle.trade_model().expect("trade model").bars.clone();
        assert!(
            lifecycle.accept_trade(&trade).is_err(),
            "rolled trade requires recovery"
        );
        assert_eq!(lifecycle.trade_model().expect("trade model").bars, before);
        lifecycle.recover_trade(RecoveryReason::SequenceGap);
        assert!(matches!(
            lifecycle.state(),
            SeriesState::Recovering {
                reason: RecoveryReason::SessionRolled,
                ..
            }
        ));
    }

    #[test]
    fn lifecycle_current_provider_offline_cancels_history() {
        let (mut lifecycle, clock, epoch, _) = seeded_live();
        let offline = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::ProviderOffline(epoch),
        );
        assert!(matches!(
            lifecycle.state(),
            SeriesState::Requested { provider: Some(_) }
        ));
        assert!(
            offline
                .iter()
                .any(|effect| effect == SeriesEffect::CancelHistory)
        );
        assert_eq!(offline.status(), Some(PresentationStatus::Recovering));
    }

    #[test]
    fn lifecycle_new_provider_epoch_reseeds() {
        let (mut lifecycle, clock, epoch, _) = seeded_live();
        let mut epochs = EpochSequence::<ProviderTag>::default();
        epochs.next();
        let newer = epochs.next();
        assert!(newer > epoch);
        let effects = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Online(newer),
        );
        assert!(matches!(lifecycle.state(), SeriesState::Seeding { .. }));
        assert_eq!(lifecycle.epoch, Some(newer));
        assert!(
            effects
                .iter()
                .any(|effect| effect == SeriesEffect::SubscribeLive)
        );
        assert!(
            effects
                .iter()
                .any(|effect| effect == SeriesEffect::RequestHistory)
        );
    }

    #[test]
    fn lifecycle_provider_failure_reports_failed() {
        let (mut lifecycle, clock, _, _) = seeded_live();
        let effects = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Failed(SeriesFailure::Provider("unavailable".into())),
        );
        assert!(
            matches!(lifecycle.state(), SeriesState::Failed { error: SeriesFailure::Provider(reason) } if reason == "unavailable")
        );
        assert_eq!(effects.status(), Some(PresentationStatus::AwaitingData));
    }
}
