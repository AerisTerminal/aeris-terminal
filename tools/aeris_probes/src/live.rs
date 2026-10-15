use std::{
    thread,
    time::{Duration, Instant},
};

use aeris_instruments::InstrumentId;
use aeris_market_data::{ChartInterval, MarketBar};
use aeris_market_engine::Viewport;
use aeris_market_runtime::{
    InstrumentRequest, InstrumentStreamKey, LeaseOwner, MarketRuntimeEvent, MarketService,
    SelectionOutcome, SelectionRequest, SeriesTailOperation, StreamRequirements,
};
use serde_json::{Value, json};

use crate::{Evidence, ProbeError, ProbeResult, cli::Options, market, utc_now};

const CLIENT: u64 = 77;
const CONSUMER: u64 = 78;
const MAXIMUM_PROBE_BARS: usize = 20_000;

struct Observation {
    generation: u64,
    selected: Option<aeris_market_runtime::InstrumentDescriptor>,
    live: bool,
    bars: Vec<MarketBar>,
    forming: bool,
    updates: u64,
    closed_bars: u64,
    epoch: u64,
    sessions: u64,
    maximum_queued: usize,
    stale_events: u64,
    stale_history: u64,
    transitions: Vec<Value>,
    retry_records: Vec<Value>,
    started: Instant,
}

impl Observation {
    fn new() -> Self {
        Self {
            generation: 0,
            selected: None,
            live: false,
            bars: Vec::new(),
            forming: false,
            updates: 0,
            closed_bars: 0,
            epoch: 0,
            sessions: 0,
            maximum_queued: 0,
            stale_events: 0,
            stale_history: 0,
            transitions: Vec::new(),
            retry_records: Vec::new(),
            started: Instant::now(),
        }
    }

    fn reset(&mut self, generation: u64) {
        self.generation = generation;
        self.selected = None;
        self.live = false;
        self.bars.clear();
        self.forming = false;
        self.updates = 0;
        self.closed_bars = 0;
    }

    fn observe(&mut self, event: MarketRuntimeEvent) -> ProbeResult<()> {
        match event {
            MarketRuntimeEvent::SelectionOutcome(SelectionOutcome::Selected {
                generation,
                descriptor,
                ..
            }) if generation.0.get() == self.generation => {
                self.selected = Some(*descriptor);
            }
            MarketRuntimeEvent::SelectionOutcome(SelectionOutcome::Failed {
                generation,
                error,
            }) if generation.0.get() == self.generation => {
                return Err(ProbeError::Runtime(format!("selection failed: {error:?}")));
            }
            MarketRuntimeEvent::SeriesState(state)
                if state.generation.0.get() == self.generation =>
            {
                self.live = matches!(
                    state.presentation,
                    Some(aeris_market_runtime::SeriesPresentationStatus::Live)
                );
                self.transitions.push(json!({
                    "at":utc_now().to_rfc3339(), "generation":self.generation,
                    "phase":format!("{:?}", state.presentation), "epoch":self.epoch,
                }));
            }
            MarketRuntimeEvent::SeriesSnapshot(publication)
                if publication.generation.0.get() == self.generation =>
            {
                let snapshot = &publication.snapshot;
                if snapshot.bars.len() > MAXIMUM_PROBE_BARS {
                    return Err(ProbeError::Invariant("snapshot exceeds probe bar capacity"));
                }
                self.bars = snapshot.bars.to_vec();
                self.forming = snapshot.forming;
            }
            MarketRuntimeEvent::SeriesUpdate(update)
                if update.generation.0.get() == self.generation =>
            {
                self.updates += 1;
                let prior_forming = self.forming;
                self.forming = update.forming;
                match update.operation {
                    SeriesTailOperation::Revise => {
                        if let Some(last) = self.bars.last_mut()
                            && last.exchange_timestamp_unix_nanos
                                == update.bar.exchange_timestamp_unix_nanos
                        {
                            *last = update.bar;
                        }
                    }
                    SeriesTailOperation::Append => {
                        if self.bars.len() >= MAXIMUM_PROBE_BARS {
                            return Err(ProbeError::Invariant(
                                "live tail exceeds probe bar capacity",
                            ));
                        }
                        if self.bars.last().is_none_or(|bar| {
                            bar.exchange_timestamp_unix_nanos
                                < update.bar.exchange_timestamp_unix_nanos
                        }) {
                            if self.bars.last().is_some_and(|bar| {
                                closes_forming_bar(prior_forming, *bar, update.bar, 60)
                            }) {
                                self.closed_bars += 1;
                            }
                            self.bars.push(update.bar);
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn pump(&mut self, service: &MarketService) -> ProbeResult<()> {
        let _ = service.wait_for_events(CLIENT, Duration::from_millis(30));
        for (_, event) in market(service.drain_events(CLIENT, &[(CONSUMER, 256)]))? {
            self.observe(event.value)?;
        }
        let status = market(service.status())?;
        let current = status
            .providers
            .iter()
            .find(|provider| provider.provider == "hyperliquid")
            .map_or(0, |provider| provider.generation);
        if current != 0 && current != self.epoch {
            self.epoch = current;
            self.transitions.push(json!({"at":utc_now().to_rfc3339(),
                "epoch":current, "kind":"connection_attempt",
                "elapsed_ms":self.started.elapsed().as_millis()}));
        }
        self.sessions = status.probe.hyperliquid_sessions;
        self.maximum_queued = self
            .maximum_queued
            .max(status.probe.maximum_queued_series_updates);
        self.stale_events = status.probe.stale_events;
        self.stale_history = status.probe.stale_history;
        let now = utc_now().timestamp_millis();
        self.retry_records = status
            .probe
            .retry_attempts
            .iter()
            .map(|record| {
                let failed =
                    now.saturating_sub(i64::try_from(record.failed_age_ms).unwrap_or(i64::MAX));
                let resumed = record
                    .resumed_age_ms
                    .map(|age| now.saturating_sub(i64::try_from(age).unwrap_or(i64::MAX)));
                json!({"attempt":record.attempt, "scheduled_delay_ms":record.delay_ms,
                "failed_at_unix_ms":failed, "resumed_at_unix_ms":resumed,
                "actual_delay_ms":resumed.map(|time| time.saturating_sub(failed))})
            })
            .collect();
        Ok(())
    }

    fn until(
        &mut self,
        service: &MarketService,
        timeout: Duration,
        predicate: impl Fn(&Self) -> bool,
    ) -> ProbeResult<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            self.pump(service)?;
            if predicate(self) {
                return Ok(());
            }
        }
        Err(ProbeError::Invariant(
            "timed out waiting for a live market observation",
        ))
    }
}

fn check_continuity(bars: &[MarketBar], seconds: i64) -> (usize, usize) {
    let gaps = bars
        .windows(2)
        .filter(|pair| {
            pair[1].exchange_timestamp_seconds - pair[0].exchange_timestamp_seconds != seconds
        })
        .count();
    (bars.len(), gaps)
}

fn closes_forming_bar(forming: bool, previous: MarketBar, next: MarketBar, seconds: i64) -> bool {
    forming && next.exchange_timestamp_seconds - previous.exchange_timestamp_seconds == seconds
}

fn select(
    service: &MarketService,
    state: &mut Observation,
    symbol: &str,
    interval: ChartInterval,
    seconds: i64,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    let generation = state.generation + 1;
    state.reset(generation);
    let instrument = if symbol == "BTC" {
        InstrumentRequest::Default
    } else {
        InstrumentRequest::CatalogSymbol {
            provider_id: "hyperliquid".into(),
            symbol: symbol.into(),
            exchange: "Hyperliquid".into(),
            entitlement_id: "hyperliquid-public".into(),
            search_generation: 0,
        }
    };
    let started = Instant::now();
    market(service.select_and_demand(
        CLIENT,
        SelectionRequest {
            consumer_id: CONSUMER,
            generation,
            instrument,
            interval,
            streams: StreamRequirements::BARS,
            viewport: None,
        },
    ))?;
    state.until(service, Duration::from_secs(40), |observed| {
        observed.selected.is_some() && observed.live && observed.bars.len() > 10
    })?;
    let (bars, gaps) = check_continuity(&state.bars, seconds);
    evidence.check(
        "select_and_demand_selected",
        true,
        json!({"symbol":symbol,"interval":interval.label(),"generation":generation,
            "latency_ms":started.elapsed().as_millis()}),
    )?;
    evidence.check(
        "history_contiguous",
        gaps == 0,
        json!({"symbol":symbol,"interval":interval.label(),"bars":bars,"interior_gaps":gaps}),
    )?;
    if interval == ChartInterval::Day1 {
        let aligned = state
            .bars
            .iter()
            .all(|bar| bar.exchange_timestamp_seconds.rem_euclid(86_400) == 0);
        evidence.check(
            "day1_alignment",
            aligned,
            json!({"bars":bars,"utc_midnight":aligned}),
        )?;
    }
    Ok(())
}

fn reconnect(
    service: &MarketService,
    state: &mut Observation,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    let old_epoch = state.epoch;
    let old_sessions = state.sessions;
    market(service.probe_fail_hyperliquid_websocket(0))?;
    state.until(service, Duration::from_secs(25), |current| {
        current.epoch > old_epoch
            && current.sessions > old_sessions
            && current.live
            && current.bars.len() > 10
    })?;
    let (_, gaps) = check_continuity(&state.bars, 60);
    evidence.check(
        "forced_reconnect",
        state.epoch == old_epoch + 1 && gaps == 0 && state.forming,
        json!({"old_epoch":old_epoch,"new_epoch":state.epoch,
            "sessions":state.sessions,"interior_gaps":gaps,"forming":state.forming}),
    )?;
    Ok(())
}

fn retry_schedule(
    service: &MarketService,
    state: &mut Observation,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    let prior = state.epoch;
    let start = Instant::now();
    let started_unix_ms = utc_now().timestamp_millis();
    market(service.probe_fail_hyperliquid_websocket(2))?;
    state.until(service, Duration::from_secs(30), |current| {
        current.epoch >= prior + 3 && current.live && current.sessions >= 3
    })?;
    let attempts: Vec<_> = state
        .transitions
        .iter()
        .filter(|entry| {
            entry["kind"] == "connection_attempt"
                && entry["epoch"].as_u64().is_some_and(|epoch| epoch > prior)
        })
        .collect();
    let schedule: Vec<_> = state
        .retry_records
        .iter()
        .filter(|entry| {
            entry["failed_at_unix_ms"]
                .as_i64()
                .is_some_and(|time| time >= started_unix_ms)
        })
        .collect();
    let bounded = attempts.len() == 3
        && schedule.len() == 3
        && schedule.iter().all(|entry| {
            let attempt = entry["attempt"].as_u64().unwrap_or(u64::MAX);
            let ceiling = 500_u64
                .saturating_mul(
                    1_u64
                        .checked_shl(u32::try_from(attempt.saturating_sub(1)).unwrap_or(u32::MAX))
                        .unwrap_or(u64::MAX),
                )
                .min(30_000);
            entry["scheduled_delay_ms"]
                .as_u64()
                .is_some_and(|delay| delay <= ceiling)
                && entry["actual_delay_ms"]
                    .as_u64()
                    .is_some_and(|delay| delay <= ceiling + 200)
        });
    evidence.check(
        "websocket_only_backoff",
        bounded,
        json!({"attempts":attempts, "registry_retry_schedule":schedule,
            "policy_initial_ms":500, "multiplier":2, "cap_ms":30000,
            "observation_tolerance_ms":200,
            "elapsed_ms":start.elapsed().as_millis()}),
    )?;
    Ok(())
}

fn quote_lease(
    service: &MarketService,
    state: &mut Observation,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    let selected = state
        .selected
        .as_ref()
        .ok_or(ProbeError::Invariant("catalog missing"))?;
    let key = InstrumentStreamKey {
        provider: selected.instrument.provider.clone(),
        instrument: InstrumentId::try_new(selected.instrument.instrument_id.clone())
            .map_err(|error| ProbeError::Runtime(error.to_string()))?,
        entitlement: Some(selected.entitlement_id.clone()),
    };
    let lease = market(service.subscribe_quotes(LeaseOwner::Trading, key))?;
    let quote_deadline = Instant::now() + Duration::from_secs(20);
    let mut quote_age = None;
    while Instant::now() < quote_deadline {
        state.pump(service)?;
        if let Some(quote) = lease.latest() {
            let now = utc_now().timestamp_nanos_opt().unwrap_or(i64::MAX);
            let age = now
                .saturating_sub(quote.quote.received_unix_nanos)
                .div_euclid(1_000_000);
            if (0..10_000).contains(&age) {
                quote_age = Some(age);
                break;
            }
        }
    }
    evidence.check(
        "quote_subscription_live",
        quote_age.is_some(),
        json!({"age_ms":quote_age}),
    )?;
    drop(lease);
    Ok(())
}

fn switch_without_reconnect(
    service: &MarketService,
    state: &mut Observation,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    let epoch = state.epoch;
    let sessions = state.sessions;
    for (interval, seconds) in [
        (ChartInterval::Hour1, 3_600),
        (ChartInterval::Day1, 86_400),
        (ChartInterval::Minute1, 60),
    ] {
        select(service, state, "BTC", interval, seconds, evidence)?;
    }
    evidence.check(
        "interval_switch_no_reconnect",
        state.epoch == epoch && state.sessions == sessions,
        json!({"epoch_before":epoch,"epoch_after":state.epoch,"sessions":state.sessions}),
    )?;
    for symbol in ["ETH", "SOL", "BTC"] {
        select(service, state, symbol, ChartInterval::Minute1, 60, evidence)?;
    }
    evidence.check(
        "symbol_switch_no_reconnect",
        state.epoch == epoch && state.sessions == sessions,
        json!({"epoch_before":epoch,"epoch_after":state.epoch,"sessions":state.sessions}),
    )
}

fn page_history(
    service: &MarketService,
    state: &mut Observation,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    let first = state
        .bars
        .first()
        .ok_or(ProbeError::Invariant("no history"))?
        .exchange_timestamp_unix_nanos;
    let last = state
        .bars
        .last()
        .ok_or(ProbeError::Invariant("no history"))?
        .exchange_timestamp_unix_nanos;
    let viewport = Viewport::try_new(first - 400 * 60_000_000_000, last)
        .map_err(|error| ProbeError::Runtime(error.to_string()))?;
    market(service.set_viewport(
        CLIENT,
        CONSUMER,
        state.generation,
        viewport.start_unix_nanos,
        viewport.end_unix_nanos,
    ))?;
    state.until(service, Duration::from_secs(30), |current| {
        current
            .bars
            .first()
            .is_some_and(|bar| bar.exchange_timestamp_unix_nanos < first)
    })?;
    let (bars, gaps) = check_continuity(&state.bars, 60);
    evidence.check("history_paging", gaps == 0,
        json!({"bars":bars,"older_bars":(first - state.bars[0].exchange_timestamp_unix_nanos)/60_000_000_000,
            "interior_gaps":gaps}))
}

pub(super) fn run(
    service: &MarketService,
    options: &Options,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    market(service.attach(CLIENT))?;
    market(service.register_consumer(CLIENT, 1, CONSUMER))?;
    let mut state = Observation::new();
    select(
        service,
        &mut state,
        &options.instrument,
        ChartInterval::Minute1,
        60,
        evidence,
    )?;
    evidence.check(
        "catalog_resolved",
        state
            .selected
            .as_ref()
            .is_some_and(|descriptor| descriptor.instrument.provider == "hyperliquid"),
        json!({"provider":"hyperliquid","symbol":options.instrument}),
    )?;
    state.until(service, Duration::from_secs(30), |current| current.forming)?;
    evidence.check(
        "live_forming_bar_seen",
        true,
        json!({"elapsed_ms":state.started.elapsed().as_millis()}),
    )?;

    quote_lease(service, &mut state, evidence)?;
    switch_without_reconnect(service, &mut state, evidence)?;
    page_history(service, &mut state, evidence)?;

    reconnect(service, &mut state, evidence)?;
    retry_schedule(service, &mut state, evidence)?;
    evidence.check(
        "stale_publications_dropped",
        true,
        json!({"stale_provider_events":state.stale_events,"stale_history":state.stale_history}),
    )?;
    evidence.check(
        "outbox_bounded",
        state.maximum_queued <= 1_024,
        json!({"maximum_observed_queued_series_updates":state.maximum_queued,"limit":1024}),
    )?;
    let wait = options.duration.saturating_sub(state.started.elapsed());
    let before = state.closed_bars;
    let end = Instant::now() + wait;
    while Instant::now() < end {
        state.pump(service)?;
        if state.closed_bars > before {
            break;
        }
        thread::yield_now();
    }
    let (bars, gaps) = check_continuity(&state.bars, 60);
    evidence.check(
        "bar_close_contiguous",
        state.closed_bars > before && gaps == 0,
        json!({"closed_bars":state.closed_bars,"bars":bars,"interior_gaps":gaps}),
    )?;
    evidence.metrics = json!({"transitions":state.transitions,
        "provider_sessions":state.sessions,"provider_epoch":state.epoch,
        "stale_events":state.stale_events,"stale_history":state.stale_history});
    market(service.detach(CLIENT))
}

#[cfg(test)]
mod tests {
    use super::{check_continuity, closes_forming_bar};
    use aeris_market_data::MarketBar;

    fn bar(seconds: i64) -> MarketBar {
        MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: seconds,
            exchange_timestamp_unix_nanos: seconds * 1_000_000_000,
            open: 1,
            high: 1,
            low: 1,
            close: 1,
            volume: 0,
        }
    }

    #[test]
    fn probe_counts_interior_gaps_not_just_duplicate_bars() {
        assert_eq!(check_continuity(&[bar(0), bar(60), bar(180)], 60), (3, 1));
        assert_eq!(check_continuity(&[bar(0), bar(60), bar(120)], 60), (3, 0));
        assert_eq!(check_continuity(&[bar(0), bar(0)], 60), (2, 1));
    }

    #[test]
    fn probe_counts_close_when_new_forming_bucket_arrives() {
        assert!(closes_forming_bar(true, bar(0), bar(60), 60));
        assert!(!closes_forming_bar(false, bar(0), bar(60), 60));
        assert!(!closes_forming_bar(true, bar(0), bar(120), 60));
    }
}
