//! Coordinator-owned per-series candle lifecycle.
mod handlers;

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
    CandleGapPolicy, Coordinator, LIVE_BUFFER_CAPACITY, SeriesLoadState, TradeLiveHandoff,
    clock::Clock, history::tracker::HistoryTicket,
};
pub(super) use crate::SeriesPresentationStatus as PresentationStatus;

/// The sole in-flight candle for the selected series. Completed bars belong to `MarketEngine`.
#[derive(Clone, Copy)]
pub(crate) struct FormingBar {
    pub(crate) bar: MarketBar,
    pub(crate) trades: Option<u32>,
}

/// M3.1 moves candle ownership here; trade-built state enters this same map in M3.2.
pub(super) enum SeriesLive {
    TradeBuilt(TradeLiveHandoff),
    ProviderCandles(SeriesLifecycle),
}

#[derive(Default)]
pub(super) struct SeriesLifecycles(pub(super) BTreeMap<BarSeriesKey, SeriesLive>);

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
    pub(super) fn trade(&self, series: &BarSeriesKey) -> Option<&TradeLiveHandoff> {
        match self.0.get(series)? {
            SeriesLive::TradeBuilt(live) => Some(live),
            SeriesLive::ProviderCandles(_) => None,
        }
    }
    pub(super) fn trade_mut(&mut self, series: &BarSeriesKey) -> Option<&mut TradeLiveHandoff> {
        match self.0.get_mut(series)? {
            SeriesLive::TradeBuilt(live) => Some(live),
            SeriesLive::ProviderCandles(_) => None,
        }
    }
    pub(super) fn candle(&self, series: &BarSeriesKey) -> Option<&SeriesLifecycle> {
        match self.0.get(series)? {
            SeriesLive::ProviderCandles(live) => Some(live),
            SeriesLive::TradeBuilt(_) => None,
        }
    }
    pub(super) fn candle_mut(&mut self, series: &BarSeriesKey) -> Option<&mut SeriesLifecycle> {
        match self.0.get_mut(series)? {
            SeriesLive::ProviderCandles(live) => Some(live),
            SeriesLive::TradeBuilt(_) => None,
        }
    }
    pub(super) fn insert_trade(&mut self, series: BarSeriesKey, live: TradeLiveHandoff) {
        self.0.insert(series, SeriesLive::TradeBuilt(live));
    }
    pub(super) fn insert_candle(&mut self, series: BarSeriesKey, live: SeriesLifecycle) {
        self.0.insert(series, SeriesLive::ProviderCandles(live));
    }
    pub(super) fn trade_keys(&self) -> impl Iterator<Item = &BarSeriesKey> {
        self.0.iter().filter_map(|(series, live)| {
            matches!(live, SeriesLive::TradeBuilt(_)).then_some(series)
        })
    }
    pub(super) fn candle_keys(&self) -> impl Iterator<Item = &BarSeriesKey> {
        self.0.iter().filter_map(|(series, live)| {
            matches!(live, SeriesLive::ProviderCandles(_)).then_some(series)
        })
    }
    pub(super) fn candle_iter(&self) -> impl Iterator<Item = (&BarSeriesKey, &SeriesLifecycle)> {
        self.0.iter().filter_map(|(series, live)| match live {
            SeriesLive::ProviderCandles(live) => Some((series, live)),
            SeriesLive::TradeBuilt(_) => None,
        })
    }
    pub(super) fn candle_iter_mut(
        &mut self,
    ) -> impl Iterator<Item = (&BarSeriesKey, &mut SeriesLifecycle)> {
        self.0.iter_mut().filter_map(|(series, live)| match live {
            SeriesLive::ProviderCandles(live) => Some((series, live)),
            SeriesLive::TradeBuilt(_) => None,
        })
    }
    pub(super) fn trade_values_mut(&mut self) -> impl Iterator<Item = &mut TradeLiveHandoff> {
        self.0.values_mut().filter_map(|live| match live {
            SeriesLive::TradeBuilt(live) => Some(live),
            SeriesLive::ProviderCandles(_) => None,
        })
    }
    pub(super) fn candle_values_mut(&mut self) -> impl Iterator<Item = &mut SeriesLifecycle> {
        self.0.values_mut().filter_map(|live| match live {
            SeriesLive::ProviderCandles(live) => Some(live),
            SeriesLive::TradeBuilt(_) => None,
        })
    }
    pub(super) fn trade_iter_mut(
        &mut self,
    ) -> impl Iterator<Item = (&BarSeriesKey, &mut TradeLiveHandoff)> {
        self.0.iter_mut().filter_map(|(series, live)| match live {
            SeriesLive::TradeBuilt(live) => Some((series, live)),
            SeriesLive::ProviderCandles(_) => None,
        })
    }
    pub(super) fn retain_trades(
        &mut self,
        mut keep: impl FnMut(&BarSeriesKey, &mut TradeLiveHandoff) -> bool,
    ) {
        self.0.retain(|series, live| match live {
            SeriesLive::TradeBuilt(live) => keep(series, live),
            SeriesLive::ProviderCandles(_) => true,
        });
    }
    pub(super) fn retain_candles(
        &mut self,
        mut keep: impl FnMut(&BarSeriesKey, &mut SeriesLifecycle) -> bool,
    ) {
        self.0.retain(|series, live| match live {
            SeriesLive::ProviderCandles(live) => keep(series, live),
            SeriesLive::TradeBuilt(_) => true,
        });
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
    pub(super) clock: Arc<dyn Clock>,
    pub(super) series: BarSeriesKey,
    pub(super) generation: ProviderGeneration,
    pub(super) gap_policy: CandleGapPolicy,
    pub(super) wire_coin: String,
    pub(super) interval: String,
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
    pub(super) fn with_clock(
        series: BarSeriesKey,
        generation: ProviderGeneration,
        wire_coin: String,
        interval: String,
        gap_policy: CandleGapPolicy,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            clock,
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

    fn step_live(&mut self, input: SeriesInput<'_>, ctx: &StepContext<'_>, out: &mut EffectBuf) {
        match input {
            SeriesInput::Online(_)
            | SeriesInput::HistoryRequested(_)
            | SeriesInput::Seeded(_)
            | SeriesInput::HistoryEmpty(_) => unreachable!("dispatched before live transitions"),
            SeriesInput::HistoryFailed => {
                out.push(SeriesEffect::ReportStatus(PresentationStatus::Recovering));
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
                    self.state = SeriesState::Recovering {
                        reason: RecoveryReason::QueueOverflow,
                        ticket: None,
                    };
                    out.push(SeriesEffect::RequestHistory);
                    out.push(SeriesEffect::ReportStatus(PresentationStatus::Recovering));
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
            SeriesInput::PhaseChanged(_) if matches!(self.state, SeriesState::Dormant { until: Some(until), .. } if ctx.clock.wall_nanos().is_some_and(|now| now.div_euclid(1_000_000) >= until)) =>
            {
                self.state = SeriesState::Recovering {
                    reason: RecoveryReason::SessionRolled,
                    ticket: None,
                };
                out.push(SeriesEffect::RequestHistory);
                out.push(SeriesEffect::ReportStatus(PresentationStatus::Recovering));
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
                self.state = SeriesState::Recovering {
                    reason,
                    ticket: None,
                };
                out.push(SeriesEffect::RequestHistory);
                out.push(SeriesEffect::ReportStatus(PresentationStatus::Recovering));
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
                            PresentationStatus::Recovering => SeriesLoadState::Partial,
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
            "BTC".into(),
            "1m".into(),
            CandleGapPolicy::Contiguous,
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
    fn lifecycle_online_then_seed_reports_live() {
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
        effects.clear();
        lifecycle.step(SeriesInput::Seeded(Some(&ticket)), &ctx, &mut effects);
        assert!(matches!(lifecycle.state(), SeriesState::Live { .. }));
        assert_eq!(effects.status(), Some(PresentationStatus::ConnectingLive));
        effects.clear();
        lifecycle.step(SeriesInput::Candle, &ctx, &mut effects);
        assert_eq!(effects.status(), Some(PresentationStatus::Live));
    }

    #[test]
    fn lifecycle_full_seed_buffer_discards_and_requests_reseed() {
        let (mut lifecycle, clock, epoch, _) = fixture();
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Online(epoch),
        );
        lifecycle
            .buffered
            .extend((0..LIVE_BUFFER_CAPACITY).map(|_| MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 1,
                exchange_timestamp_unix_nanos: 1_000_000_000,
                open: 100,
                high: 100,
                low: 100,
                close: 100,
                volume: 1,
            }));
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
        assert_eq!(effects.status(), None);
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
        assert_eq!(effects.status(), None);
        assert!(matches!(lifecycle.state(), SeriesState::Seeding { .. }));
    }

    #[test]
    fn lifecycle_seed_dormant_when_calendar_closed() {
        let (mut lifecycle, clock, epoch, ticket) = fixture();
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Closed,
            SeriesInput::Online(epoch),
        );
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Closed,
            SeriesInput::HistoryRequested(&ticket),
        );
        let effects = step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Closed,
            SeriesInput::Seeded(Some(&ticket)),
        );
        assert!(matches!(
            lifecycle.state(),
            SeriesState::Dormant {
                phase: SessionPhase::Closed,
                ..
            }
        ));
        assert_eq!(effects.status(), Some(PresentationStatus::AwaitingData));
    }

    #[test]
    fn lifecycle_empty_history_reports_awaiting_data() {
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
    fn lifecycle_retry_preserves_seed_then_exhaustion_fails() {
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
    fn lifecycle_gap_and_open_stall_recover_but_closed_stall_does_not() {
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
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Seeded(Some(&ticket)),
        );
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
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Open,
            SeriesInput::Seeded(None),
        );
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Open,
            SeriesInput::Recover(RecoveryReason::SequenceGap),
        );
        assert!(matches!(
            lifecycle.state(),
            SeriesState::Recovering {
                reason: RecoveryReason::SequenceGap,
                ..
            }
        ));
    }

    #[test]
    fn lifecycle_phase_close_then_session_roll_requests_history() {
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
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Seeded(Some(&ticket)),
        );
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::Closed,
            SeriesInput::PhaseChanged(SessionPhase::Closed),
        );
        assert!(matches!(lifecycle.state(), SeriesState::Dormant { .. }));
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
    }

    #[test]
    fn lifecycle_offline_then_new_epoch_reseeds_and_provider_failure_is_terminal() {
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
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Seeded(Some(&ticket)),
        );
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
        let mut epochs = EpochSequence::<ProviderTag>::default();
        epochs.next();
        let newer = epochs.next();
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Online(newer),
        );
        assert!(matches!(lifecycle.state(), SeriesState::Seeding { .. }));
        step(
            &mut lifecycle,
            clock.as_ref(),
            SessionPhase::AlwaysOpen,
            SeriesInput::Failed(SeriesFailure::Provider("unavailable".into())),
        );
        assert!(matches!(lifecycle.state(), SeriesState::Failed { .. }));
    }
}
