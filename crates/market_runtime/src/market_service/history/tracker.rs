//! Coordinator-owned history lanes and provider-epoch retirement.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use aeris_market_data::BarSeriesKey;
use aeris_market_engine::fence::{DemandTag, Epoch, EpochSequence, ProviderTag};
use aeris_provider_history::HistoryRange;

use crate::market_service::{
    clock::Clock,
    reconnect::{Backoff, ReconnectPolicy},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LaneKind {
    Current,
    Backfill,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HistoryTicket {
    pub series: BarSeriesKey,
    pub provider: Epoch<ProviderTag>,
    pub demand: Epoch<DemandTag>,
    pub lane: LaneKind,
    pub seq: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LaneState {
    Idle,
    Deferred(HistoryTicket, Option<HistoryRange>),
    InFlight(HistoryTicket),
    RetryAt(Instant, Option<HistoryRange>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeferredRequest {
    Full,
    Range(HistoryRange),
}

struct Lane {
    state: LaneState,
    cancel: Arc<AtomicBool>,
    backoff: Backoff,
    seq: u32,
    range: Option<HistoryRange>,
    deferred: Option<DeferredRequest>,
    attempts: u8,
}

impl Lane {
    fn new(clock: &dyn Clock, seed: u64) -> Self {
        Self {
            state: LaneState::Idle,
            cancel: Arc::new(AtomicBool::new(false)),
            backoff: Backoff::new(ReconnectPolicy::rithmic_recovery(), clock, seed),
            seq: 0,
            range: None,
            deferred: None,
            attempts: 0,
        }
    }

    fn start(&mut self, ticket: HistoryTicket, range: Option<HistoryRange>) {
        self.cancel = Arc::new(AtomicBool::new(false));
        self.seq = ticket.seq;
        self.range = range;
        self.state = LaneState::InFlight(ticket);
    }

    fn fail(&mut self, now: Instant, range: Option<HistoryRange>, capacity: bool) -> bool {
        if !capacity {
            self.attempts = self.attempts.saturating_add(1);
        }
        let delay = if self.attempts > super::super::MAXIMUM_HISTORY_RETRIES {
            super::super::HISTORY_FAILED_RETRY_COOLDOWN
        } else if capacity {
            super::super::HISTORY_RETRY_DELAY
        } else {
            self.backoff.transient_delay()
        };
        self.state = LaneState::RetryAt(now + delay, range);
        self.attempts <= super::super::MAXIMUM_HISTORY_RETRIES
    }

    fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

#[derive(Default)]
pub(crate) struct Coverage {
    pub confirmed_empty: Option<HistoryRange>,
    pub backwards_exhausted: bool,
    pub current_empty: Option<super::super::EmptyCurrentHistory>,
}

struct SeriesHistory {
    demand: Epoch<DemandTag>,
    current: Lane,
    backfill: Lane,
    coverage: Coverage,
    detached: bool,
}

impl SeriesHistory {
    fn new(clock: &dyn Clock, seed: u64, demand: Epoch<DemandTag>) -> Self {
        Self {
            demand,
            current: Lane::new(clock, seed),
            backfill: Lane::new(clock, seed ^ 1),
            coverage: Coverage::default(),
            detached: false,
        }
    }

    fn lane(&self, kind: LaneKind) -> &Lane {
        match kind {
            LaneKind::Current => &self.current,
            LaneKind::Backfill => &self.backfill,
        }
    }

    fn lane_mut(&mut self, kind: LaneKind) -> &mut Lane {
        match kind {
            LaneKind::Current => &mut self.current,
            LaneKind::Backfill => &mut self.backfill,
        }
    }

    fn cancel(&mut self) {
        self.current.cancel();
        self.backfill.cancel();
    }
}

#[derive(Default)]
pub(crate) struct HistoryTracker {
    entries: BTreeMap<(BarSeriesKey, Epoch<ProviderTag>), SeriesHistory>,
    demands: EpochSequence<DemandTag>,
}

impl HistoryTracker {
    #[cfg(test)]
    pub(crate) fn count(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn coverage(
        &self,
        series: &BarSeriesKey,
        provider: Epoch<ProviderTag>,
    ) -> Option<&Coverage> {
        self.entries
            .get(&(series.clone(), provider))
            .map(|entry| &entry.coverage)
    }

    pub(crate) fn coverage_mut(
        &mut self,
        series: &BarSeriesKey,
        provider: Epoch<ProviderTag>,
    ) -> Option<&mut Coverage> {
        self.entries
            .get_mut(&(series.clone(), provider))
            .map(|entry| &mut entry.coverage)
    }

    pub(crate) fn current_empty(
        &self,
        series: &BarSeriesKey,
        provider: Epoch<ProviderTag>,
    ) -> Option<super::super::EmptyCurrentHistory> {
        self.entries
            .get(&(series.clone(), provider))
            .and_then(|entry| entry.coverage.current_empty)
    }

    pub(crate) fn current_empty_due(
        &self,
        provider_id: &str,
        provider: Epoch<ProviderTag>,
        instrument: &str,
        entitlement: &str,
        exchange_time: i64,
    ) -> Vec<BarSeriesKey> {
        self.entries.iter().filter(|((series, epoch), entry)| {
            *epoch == provider && series.provider_id == provider_id
                && series.instrument_id == instrument && series.entitlement_id == entitlement
                && matches!(entry.coverage.current_empty,
                    Some(super::super::EmptyCurrentHistory::AwaitingLiveTrade { boundary_unix_nanos })
                        if exchange_time > boundary_unix_nanos)
        }).map(|((series, _), _)| series.clone()).collect()
    }

    pub(crate) fn set_detached(
        &mut self,
        series: &BarSeriesKey,
        provider: Epoch<ProviderTag>,
        detached: bool,
        clock: &dyn Clock,
    ) {
        self.entries
            .entry((series.clone(), provider))
            .or_insert_with(|| SeriesHistory::new(clock, provider.get().get(), self.demands.next()))
            .detached = detached;
    }

    pub(crate) fn is_detached(&self, series: &BarSeriesKey, provider: Epoch<ProviderTag>) -> bool {
        self.entries
            .get(&(series.clone(), provider))
            .is_some_and(|entry| entry.detached)
    }

    pub(crate) fn cancel_token(&self, ticket: &HistoryTicket) -> Arc<AtomicBool> {
        Arc::clone(
            &self.entries[&(ticket.series.clone(), ticket.provider)]
                .lane(ticket.lane)
                .cancel,
        )
    }

    pub(crate) fn begin(
        &mut self,
        series: BarSeriesKey,
        provider: Epoch<ProviderTag>,
        kind: LaneKind,
        range: Option<HistoryRange>,
        clock: &dyn Clock,
    ) -> HistoryTicket {
        let entry = self
            .entries
            .entry((series.clone(), provider))
            .or_insert_with(|| {
                SeriesHistory::new(clock, provider.get().get(), self.demands.next())
            });
        let demand = entry.demand;
        let lane = entry.lane_mut(kind);
        let seq = lane
            .seq
            .checked_add(1)
            .expect("history ticket sequence exhausted");
        let ticket = HistoryTicket {
            series,
            provider,
            demand,
            lane: kind,
            seq,
        };
        lane.start(ticket.clone(), range);
        ticket
    }

    pub(crate) fn defer(&mut self, ticket: &HistoryTicket, range: Option<HistoryRange>) {
        if let Some(entry) = self
            .entries
            .get_mut(&(ticket.series.clone(), ticket.provider))
        {
            let lane = entry.lane_mut(ticket.lane);
            if lane.state == LaneState::InFlight(ticket.clone()) {
                lane.state = LaneState::Deferred(ticket.clone(), range);
            }
        }
    }

    pub(crate) fn defer_next(
        &mut self,
        series: &BarSeriesKey,
        provider: Epoch<ProviderTag>,
        kind: LaneKind,
        range: Option<HistoryRange>,
    ) {
        if let Some(entry) = self.entries.get_mut(&(series.clone(), provider)) {
            let demand = entry.demand;
            let lane = entry.lane_mut(kind);
            match &mut lane.deferred {
                Some(pending) => super::merge_deferred_request(pending, range),
                None => {
                    lane.deferred =
                        Some(range.map_or(DeferredRequest::Full, DeferredRequest::Range));
                }
            }
            if matches!(lane.state, LaneState::Idle) {
                let seq = lane
                    .seq
                    .checked_add(1)
                    .expect("history ticket sequence exhausted");
                lane.seq = seq;
                lane.state = LaneState::Deferred(
                    HistoryTicket {
                        series: series.clone(),
                        provider,
                        demand,
                        lane: kind,
                        seq,
                    },
                    range,
                );
            }
        }
    }

    pub(crate) fn inflight(
        &self,
        series: &BarSeriesKey,
        provider: Epoch<ProviderTag>,
    ) -> Option<(&HistoryTicket, Option<HistoryRange>)> {
        let entry = self.entries.get(&(series.clone(), provider))?;
        [&entry.current, &entry.backfill]
            .into_iter()
            .find_map(|lane| match &lane.state {
                LaneState::InFlight(ticket) => Some((ticket, lane.range)),
                _ => None,
            })
    }

    pub(crate) fn pending(
        &self,
        series: &BarSeriesKey,
        provider: Epoch<ProviderTag>,
        kind: LaneKind,
    ) -> Option<(Instant, Option<HistoryRange>, u8)> {
        let lane = self.entries.get(&(series.clone(), provider))?.lane(kind);
        match lane.state {
            LaneState::RetryAt(at, range) => Some((at, range, lane.attempts)),
            _ => None,
        }
    }

    pub(crate) fn retry_due(
        &self,
        now: Instant,
    ) -> Option<(
        BarSeriesKey,
        Epoch<ProviderTag>,
        LaneKind,
        Option<HistoryRange>,
    )> {
        [LaneKind::Current, LaneKind::Backfill]
            .into_iter()
            .find_map(|kind| {
                self.entries.iter().find_map(|((series, epoch), entry)| {
                    let lane = entry.lane(kind);
                    match lane.state {
                        LaneState::RetryAt(at, range)
                            if now >= at
                                && lane.attempts <= super::super::MAXIMUM_HISTORY_RETRIES
                                && self.inflight(series, *epoch).is_none() =>
                        {
                            Some((series.clone(), *epoch, kind, range))
                        }
                        _ => None,
                    }
                })
            })
    }

    /// Earliest actionable retry. A lane blocked by an in-flight request is
    /// reconsidered when that request completes, rather than spinning at its
    /// expired deadline.
    pub(crate) fn next_retry_deadline(&self) -> Option<Instant> {
        self.entries
            .iter()
            .flat_map(|((series, epoch), entry)| {
                [LaneKind::Current, LaneKind::Backfill]
                    .into_iter()
                    .filter_map(move |kind| {
                        let lane = entry.lane(kind);
                        match lane.state {
                            LaneState::RetryAt(at, _)
                                if lane.attempts <= super::super::MAXIMUM_HISTORY_RETRIES
                                    && self.inflight(series, *epoch).is_none() =>
                            {
                                Some(at)
                            }
                            _ => None,
                        }
                    })
            })
            .min()
    }

    pub(crate) fn schedule(
        &mut self,
        series: BarSeriesKey,
        provider: Epoch<ProviderTag>,
        kind: LaneKind,
        range: Option<HistoryRange>,
        clock: &dyn Clock,
        capacity: bool,
    ) -> bool {
        let entry = self.entries.entry((series, provider)).or_insert_with(|| {
            SeriesHistory::new(clock, provider.get().get(), self.demands.next())
        });
        let lane = entry.lane_mut(kind);
        let retry = lane.fail(clock.monotonic(), range, capacity);
        if !retry && kind == LaneKind::Current {
            lane.attempts = 0;
        }
        retry
    }

    pub(crate) fn clear_retry(
        &mut self,
        series: &BarSeriesKey,
        provider: Epoch<ProviderTag>,
        kind: LaneKind,
    ) {
        if let Some(entry) = self.entries.get_mut(&(series.clone(), provider)) {
            let lane = entry.lane_mut(kind);
            if matches!(lane.state, LaneState::RetryAt(..)) {
                lane.state = LaneState::Idle;
                lane.attempts = 0;
                lane.backoff.reset();
            }
        }
    }

    pub(crate) fn take_deferred(
        &mut self,
        series: &BarSeriesKey,
        provider: Epoch<ProviderTag>,
    ) -> Option<DeferredRequest> {
        let entry = self.entries.get_mut(&(series.clone(), provider))?;
        let kind = if entry.current.deferred.is_some() {
            LaneKind::Current
        } else {
            LaneKind::Backfill
        };
        let lane = entry.lane_mut(kind);
        let deferred = lane.deferred.take();
        if matches!(lane.state, LaneState::Deferred(..)) {
            lane.state = LaneState::Idle;
        }
        deferred
    }

    pub(crate) fn finish(
        &mut self,
        ticket: &HistoryTicket,
        range: Option<HistoryRange>,
        success: bool,
    ) -> Option<bool> {
        let entry = self
            .entries
            .get_mut(&(ticket.series.clone(), ticket.provider))?;
        let lane = entry.lane_mut(ticket.lane);
        if lane.state != LaneState::InFlight(ticket.clone()) || lane.range != range {
            return None;
        }
        let cancelled = lane.cancel.load(Ordering::Acquire);
        lane.state = LaneState::Idle;
        if success {
            lane.attempts = 0;
            lane.backoff.reset();
        }
        Some(cancelled)
    }

    pub(crate) fn cancel_provider(&mut self, provider: &str) {
        for ((series, _), entry) in &mut self.entries {
            if series.provider_id == provider {
                entry.cancel();
            }
        }
    }

    pub(crate) fn cancel_series(&mut self, series: &BarSeriesKey) {
        for ((key, _), entry) in &mut self.entries {
            if key == series {
                entry.cancel();
            }
        }
    }

    pub(crate) fn cancel_all(&mut self) {
        for entry in self.entries.values_mut() {
            entry.cancel();
        }
    }

    #[cfg(test)]
    pub(crate) fn fail(
        &mut self,
        ticket: &HistoryTicket,
        now: Instant,
        range: Option<HistoryRange>,
        capacity: bool,
    ) -> bool {
        let Some(entry) = self
            .entries
            .get_mut(&(ticket.series.clone(), ticket.provider))
        else {
            return false;
        };
        let lane = entry.lane_mut(ticket.lane);
        if lane.state != LaneState::InFlight(ticket.clone())
            && lane.state != LaneState::Deferred(ticket.clone(), range)
        {
            return false;
        }
        lane.fail(now, range, capacity)
    }

    #[cfg(test)]
    pub(crate) fn state(&self, ticket: &HistoryTicket) -> Option<&LaneState> {
        self.entries
            .get(&(ticket.series.clone(), ticket.provider))
            .map(|entry| &entry.lane(ticket.lane).state)
    }

    pub(crate) fn retain_demanded(
        &mut self,
        demanded: &BTreeSet<(BarSeriesKey, Epoch<ProviderTag>)>,
    ) {
        self.entries.retain(|key, entry| {
            if demanded.contains(key) {
                true
            } else {
                entry.cancel();
                false
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market_service::clock::ScriptedClock;
    use aeris_market_data::BarPeriod;
    use aeris_market_engine::fence::EpochSequence;

    fn series() -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "fixture".into(),
            instrument_id: "fixture:ABC".into(),
            entitlement_id: "fixture".into(),
            period: BarPeriod::time(60).expect("minute"),
            definition_version: 1,
        }
    }

    #[test]
    fn history_tracker_backfill_failure_does_not_delay_current_recovery() {
        let clock = ScriptedClock::new(1_000_000_000);
        let provider = EpochSequence::<ProviderTag>::default().next();
        let mut tracker = HistoryTracker::default();
        let old = tracker.begin(series(), provider, LaneKind::Backfill, None, &clock);
        assert!(tracker.fail(
            &old,
            clock.monotonic(),
            Some(HistoryRange {
                start_unix_nanos: 10,
                end_unix_nanos: 20
            }),
            false,
        ));
        let current = tracker.begin(series(), provider, LaneKind::Current, None, &clock);
        assert!(matches!(tracker.state(&old), Some(LaneState::RetryAt(..))));
        assert_eq!(
            tracker.state(&current),
            Some(&LaneState::InFlight(current.clone()))
        );
        assert_eq!(tracker.count(), 1);
    }

    #[test]
    fn history_tracker_retain_demanded_cancels_old_epoch_and_deferred_work() {
        let clock = ScriptedClock::new(1_000_000_000);
        let mut epochs = EpochSequence::<ProviderTag>::default();
        let old = epochs.next();
        let current = epochs.next();
        let mut tracker = HistoryTracker::default();
        let old_ticket = tracker.begin(series(), old, LaneKind::Current, None, &clock);
        let cancel = tracker.cancel_token(&old_ticket);
        tracker.defer(&old_ticket, None);
        assert_eq!(
            tracker.state(&old_ticket),
            Some(&LaneState::Deferred(old_ticket.clone(), None))
        );
        assert!(!tracker.is_detached(&series(), old));
        tracker.set_detached(&series(), old, true, &clock);
        assert!(tracker.is_detached(&series(), old));
        let coverage = tracker.coverage(&series(), old).expect("coverage");
        assert!(coverage.confirmed_empty.is_none());
        assert!(!coverage.backwards_exhausted);
        assert!(coverage.current_empty.is_none());
        tracker.begin(series(), current, LaneKind::Current, None, &clock);
        tracker.retain_demanded(&BTreeSet::from([(series(), current)]));
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(tracker.count(), 1);
        tracker.retain_demanded(&BTreeSet::new());
        assert_eq!(tracker.count(), 0);
    }

    #[test]
    fn history_tracker_retains_active_ticket_when_other_lane_defers_and_rejects_stale_sequence() {
        let clock = ScriptedClock::new(1_000_000_000);
        let provider = EpochSequence::<ProviderTag>::default().next();
        let mut tracker = HistoryTracker::default();
        let backfill = tracker.begin(series(), provider, LaneKind::Backfill, None, &clock);
        tracker.defer_next(&series(), provider, LaneKind::Current, None);
        assert_eq!(
            tracker.state(&backfill),
            Some(&LaneState::InFlight(backfill.clone()))
        );
        assert!(matches!(
            tracker.take_deferred(&series(), provider),
            Some(DeferredRequest::Full)
        ));
        assert_eq!(tracker.finish(&backfill, None, true), Some(false));
        let next = tracker.begin(series(), provider, LaneKind::Backfill, None, &clock);
        assert_ne!(backfill.seq, next.seq);
        assert_eq!(tracker.finish(&backfill, None, true), None);
        assert_eq!(
            tracker.state(&next),
            Some(&LaneState::InFlight(next.clone()))
        );
    }
}
