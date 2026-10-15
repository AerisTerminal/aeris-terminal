// Deterministic coordinator replay through commands, the real registry, and consumer output.
use super::{boundary::Boundary, *};
use crate::market_service::{
    HistoryRequest, ProviderDemand,
    provider_session::{ProviderEventSink, ProviderSession, ProviderSessionError},
};
use aeris_market_engine::fence::{Epoch, ProviderTag};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

const MAX_REPLAY_REQUESTS: usize = 128;

#[derive(Default)]
struct Transcript {
    connects: Vec<u64>,
    history: VecDeque<HistoryRequest>,
    demands: Vec<ProviderDemand>,
}

struct ScriptedRegistry(Arc<Mutex<Transcript>>);

impl ProviderSession for ScriptedRegistry {
    fn adapter_retries_transport(&self) -> bool {
        true
    }
    fn connect(
        &self,
        epoch: Epoch<ProviderTag>,
        _: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError> {
        self.0.lock().expect("transcript").connects.push(epoch.get().get());
        Ok(true)
    }

    fn rebind(
        &self,
        epoch: Epoch<ProviderTag>,
        sink: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError> {
        self.connect(epoch, sink)
    }

    fn apply_demand(&self, demand: &ProviderDemand) -> Result<bool, ProviderSessionError> {
        self.0.lock().expect("transcript").demands.push(demand.clone());
        Ok(true)
    }

    fn request_history(&self, request: HistoryRequest) -> Result<bool, ProviderSessionError> {
        let mut transcript = self.0.lock().expect("transcript");
        if transcript.history.len() == MAX_REPLAY_REQUESTS {
            return Ok(false);
        }
        transcript.history.push_back(request);
        Ok(true)
    }

    fn release_idle(&self) -> Result<bool, ProviderSessionError> {
        Ok(true)
    }

    fn shutdown(&self) {}
}

enum ReplayStep {
    Demand(BarSeriesKey, u64),
    Seed(Vec<MarketBar>, MarketBar),
    Trade(u64, i64),
    Advance(Duration),
}

struct ReplayHarness {
    boundary: Boundary,
    transcript: Arc<Mutex<Transcript>>,
}

impl ReplayHarness {
    fn new() -> Self {
        let mut boundary = Boundary::new();
        let transcript = Arc::new(Mutex::new(Transcript::default()));
        boundary.register_replay_session(Box::new(ScriptedRegistry(Arc::clone(&transcript))));
        Self { boundary, transcript }
    }

    fn run(&mut self, steps: impl IntoIterator<Item = ReplayStep>) {
        for step in steps {
            match step {
                ReplayStep::Demand(series, epoch) => self.boundary.demand(series, epoch),
                ReplayStep::Seed(bars, forming) => {
                    let request = self.transcript.lock().expect("transcript").history
                        .pop_front().expect("seed was requested");
                    self.boundary.complete(request, bars, Some(forming));
                }
                ReplayStep::Trade(sequence, minute) => self.boundary.trade(sequence, minute),
                ReplayStep::Advance(duration) => self.boundary.advance(duration),
            }
        }
    }

    fn requests(&self) -> Vec<HistoryRequest> {
        self.transcript.lock().expect("transcript").history.iter().cloned().collect()
    }
}

#[test]
fn replay_switch_storm() {
    let mut harness = ReplayHarness::new();
    let first = harness.boundary.series();
    let mut second_instrument = instrument();
    second_instrument.instrument_id = "instrument:rithmic:CME:ESU6".into();
    second_instrument.provider_symbol = "ESU6".into();
    second_instrument.display_symbol = "ES Sep 2026".into();
    second_instrument.entitlement_id = "rithmic-test:CME:ESU6".into();
    harness.boundary.install(second_instrument.clone());
    let mut second = first.clone();
    second.instrument_id = second_instrument.instrument_id;
    second.entitlement_id = second_instrument.entitlement_id;
    for epoch in 1..=50 {
        let selected = if epoch % 2 == 0 { &second } else { &first };
        harness.run([ReplayStep::Demand(selected.clone(), epoch)]);
    }
    let old_requests = harness.requests();
    assert!(!old_requests.is_empty());
    for request in old_requests.into_iter().take(49) {
        harness.boundary.complete(request, minute_bars(1_000, 2), None);
    }
    let current = harness.transcript.lock().expect("transcript").history
        .pop_back().expect("final request");
    harness.boundary.complete(current, minute_bars(0, 3), None);
    let snapshots = harness.boundary.events().into_iter().filter_map(|event| match event {
        MarketRuntimeEvent::SeriesSnapshot(snapshot) => Some(snapshot),
        _ => None,
    }).collect::<Vec<_>>();
    assert!(snapshots.iter().all(|snapshot| snapshot.snapshot.bars.iter().all(
        |bar| bar.close < 1_000
    )), "retired completions must not publish");
    assert!(snapshots.iter().any(|snapshot| snapshot.generation == generation(50)));
    assert_eq!(harness.transcript.lock().unwrap().connects.len(), 1);
}

#[test]
fn replay_reseed_during_live_flow() {
    let mut harness = ReplayHarness::new();
    harness.run([
        ReplayStep::Demand(harness.boundary.series(), 1),
        ReplayStep::Seed(minute_bars(0, 3), minute_bars(3, 1)[0]),
        ReplayStep::Trade(1, 3),
        ReplayStep::Advance(Duration::from_secs(1)),
    ]);
    harness.boundary.events();
    let current = harness.boundary.registry_epoch();
    harness.boundary.coordinator.handle_bound_provider_event(
        crate::market_service::provider_event::ProviderEvent {
            provider: "rithmic",
            generation: harness.boundary.coordinator.provider_registry.epoch("rithmic").unwrap(),
            wire_generation: current,
            kind: crate::market_service::provider_event::ProviderEventKind::TransportReplaced,
        },
    );
    assert_eq!(harness.boundary.registry_epoch(), current + 1);
    let epoch = harness.boundary.coordinator.provider_registry.epoch("rithmic").unwrap();
    harness.boundary.coordinator.handle_bound_provider_event(
        crate::market_service::provider_event::ProviderEvent {
            provider: "rithmic",
            generation: epoch,
            wire_generation: current + 1,
            kind: crate::market_service::provider_event::ProviderEventKind::Connecting,
        },
    );
    harness.boundary.coordinator.handle_bound_provider_event(
        crate::market_service::provider_event::ProviderEvent {
            provider: "rithmic",
            generation: epoch,
            wire_generation: current + 1,
            kind: crate::market_service::provider_event::ProviderEventKind::Connected,
        },
    );
    harness.boundary.tick();
    harness.run([ReplayStep::Seed(minute_bars(0, 4), minute_bars(4, 1)[0])]);
    let snapshots = harness.boundary.events().into_iter().filter_map(|event| match event {
        MarketRuntimeEvent::SeriesSnapshot(snapshot) => Some(snapshot),
        _ => None,
    }).collect::<Vec<_>>();
    assert!(snapshots.iter().any(|snapshot| snapshot.snapshot.bars.windows(2).all(|pair|
        pair[1].exchange_timestamp_unix_nanos - pair[0].exchange_timestamp_unix_nanos
            == 60_000_000_000
    )));
    let series = harness.boundary.series();
    let live = harness.boundary.coordinator.series.trade_lifecycle(&series).unwrap();
    assert!(live.history_ready());
    assert_eq!(live.trade_model().unwrap().forming_tail_sequence, Some(5));
    assert_eq!(harness.transcript.lock().unwrap().connects.len(), 2);
}

#[test]
fn replay_stale_epoch_completion() {
    let mut harness = ReplayHarness::new();
    harness.run([ReplayStep::Demand(harness.boundary.series(), 1)]);
    let stale = harness.requests().pop().expect("first epoch request");
    let epoch = harness.boundary.coordinator.provider_registry.epoch("rithmic").unwrap();
    harness.boundary.coordinator.handle_bound_provider_event(
        crate::market_service::provider_event::ProviderEvent {
            provider: "rithmic",
            generation: epoch,
            wire_generation: epoch.get().get(),
            kind: crate::market_service::provider_event::ProviderEventKind::TransportReplaced,
        },
    );
    harness.boundary.complete(stale, minute_bars(1_000, 3), None);
    assert!(!harness.boundary.events().iter().any(|event|
        matches!(event, MarketRuntimeEvent::SeriesSnapshot(_))));
}

#[test]
fn replay_gap_recovery() {
    use aeris_rithmic_protocol_adapter::ProviderInvalidationReason;
    let mut harness = ReplayHarness::new();
    harness.run([
        ReplayStep::Demand(harness.boundary.series(), 1),
        ReplayStep::Seed(minute_bars(0, 3), minute_bars(3, 1)[0]),
    ]);
    harness.boundary.events();
    let epoch = harness.boundary.coordinator.provider_registry.epoch("rithmic").unwrap();
    let wire = crate::market_service::provider_event::ProviderEvent::rithmic(
        RithmicRealtimeEvent::Recovering(1, Some(ProviderInvalidationReason::SequenceGap)),
    );
    harness.boundary.coordinator.handle_bound_provider_event(
        crate::market_service::provider_event::ProviderEvent {
            provider: "rithmic",
            generation: epoch,
            wire_generation: wire.generation,
            kind: wire.kind,
        },
    );
    assert!(matches!(
        harness.boundary.coordinator.series
            .trade_lifecycle(&harness.boundary.series()).unwrap().state(),
        crate::market_service::lifecycle::SeriesState::Recovering {
            reason: crate::market_service::lifecycle::RecoveryReason::SequenceGap, ..
        }
    ));
    harness.boundary.tick();
    harness.run([ReplayStep::Seed(minute_bars(0, 4), minute_bars(4, 1)[0])]);
    let events = harness.boundary.events();
    assert!(events.iter().any(|event| matches!(event,
        MarketRuntimeEvent::SeriesSnapshot(snapshot)
            if snapshot.snapshot.bars.windows(2).all(|pair|
                pair[1].exchange_timestamp_unix_nanos - pair[0].exchange_timestamp_unix_nanos
                    == 60_000_000_000)
    )));
    let series = harness.boundary.series();
    let live = harness.boundary.coordinator.series.trade_lifecycle(&series).unwrap();
    assert!(live.history_ready());
    assert!(live.trade_model().unwrap().forming_tail_sequence.is_some());
}

#[test]
fn replay_sanitized_public_fixture() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/replay/hyperliquid_candles.json"
    )).expect("public fixture JSON");
    let bars = fixture["candles"].as_array().expect("candle array");
    assert!(bars.windows(2).all(|pair| pair[1]["t"].as_i64().unwrap()
        - pair[0]["t"].as_i64().unwrap() == 60_000));
}

#[test]
fn replay_day3_round_trip() {
    use aeris_contracts::{SeriesCadence, SeriesKey};
    use aeris_market_data::{BarPeriod, ChartInterval};
    for (cadence, value, period) in [
        (SeriesCadence::FixedSeconds, 259_200, BarPeriod::time(259_200).unwrap()),
        (SeriesCadence::SessionDays, 3, BarPeriod::session(3).unwrap()),
    ] {
        let series = SeriesKey {
            cadence: cadence.into(),
            cadence_value: value,
            ..Default::default()
        };
        assert_eq!(crate::cadence::legacy_interval(&series), Some(ChartInterval::Day3));
        assert_eq!(crate::cadence::legacy_series_period(&series), Some(period));
        assert_eq!(crate::cadence::persisted_interval(ChartInterval::Day3.label()),
            Some(ChartInterval::Day3));
    }
}

fn utc_ms(text: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(text).unwrap().timestamp_millis()
}

#[test]
fn replay_cme_roll_across_dst() {
    use crate::market_service::calendar::{CalendarSource, ExchangeRule, MarketCalendarService};
    let mut calendar = MarketCalendarService::default();
    calendar.insert("future".into(), CalendarSource::ExchangeRule(ExchangeRule::CmeGlobex));
    for (before, roll, after) in [
        ("2026-02-08T22:59:00Z", "2026-02-08T23:00:00Z", "2026-02-08T23:01:00Z"),
        ("2026-03-08T21:59:00Z", "2026-03-08T22:00:00Z", "2026-03-08T22:01:00Z"),
        ("2026-11-01T22:59:00Z", "2026-11-01T23:00:00Z", "2026-11-01T23:01:00Z"),
    ] {
        let prior = calendar.session_window("future", utc_ms(before)).unwrap();
        let next = calendar.session_window("future", utc_ms(after)).unwrap();
        assert_eq!(prior.open_utc_ms, utc_ms(roll));
        assert_eq!(next.open_utc_ms, utc_ms(roll));
        assert_eq!(calendar.phase("future", utc_ms(before)),
            aeris_instruments::SessionPhase::Closed);
        assert_eq!(calendar.phase("future", utc_ms(after)),
            aeris_instruments::SessionPhase::Open);
    }
}

#[test]
fn replay_non_cme_calendar_resolution() {
    use crate::market_service::calendar::{CalendarSource, MarketCalendarService};
    use aeris_instruments::{CalendarSourceKind, SessionPhase};
    let mut calendar = MarketCalendarService::default();
    calendar.insert("public".into(), CalendarSource::AlwaysOpen);
    assert_eq!(calendar.phase("public", utc_ms("2026-10-18T12:00:00Z")),
        SessionPhase::AlwaysOpen);
    assert_eq!(calendar.source_kind("public"), CalendarSourceKind::AlwaysOpen);
    assert_eq!(calendar.phase("unresolved", utc_ms("2026-10-18T12:00:00Z")),
        SessionPhase::Unknown);
    let fx = crate::market_service::InstallProviderInstrument {
        instrument_id: "fx".into(),
        contract_metadata: Some(Box::new(aeris_contracts::ProviderContractMetadata {
            session_hours: vec![aeris_contracts::ProviderSessionHours {
                weekday: 1,
                open_seconds: 0,
                close_seconds: 3_600,
                timezone: "UTC".into(),
            }],
            ..Default::default()
        })),
        ..Default::default()
    };
    calendar.resolve(&fx, None);
    assert_eq!(calendar.source_kind("fx"), CalendarSourceKind::ContractHours);
    assert_eq!(calendar.phase("fx", utc_ms("2026-10-19T00:30:00Z")), SessionPhase::Open);
}

#[test]
fn replay_backoff_schedules() {
    use crate::market_service::{
        clock::ScriptedClock,
        reconnect::{Backoff, ReconnectPolicy, RetryClass, RetryDecision},
    };
    let clock = ScriptedClock::new(42);
    let mut hyperliquid = Backoff::new(ReconnectPolicy::hyperliquid(), &clock, 19);
    let mut repeated = Backoff::new(ReconnectPolicy::hyperliquid(), &clock, 19);
    let mut rithmic = Backoff::new(ReconnectPolicy::rithmic_recovery(), &clock, 19);
    for attempt in 0..10 {
        let upper = Duration::from_millis(500 * (1 << attempt)).min(Duration::from_secs(30));
        let wait = hyperliquid.failure(RetryClass::Transient);
        assert_eq!(wait, repeated.failure(RetryClass::Transient));
        assert!(matches!(wait, RetryDecision::Wait(delay) if delay <= upper));
        let rithmic_wait = rithmic.failure(RetryClass::Transient);
        let rithmic_upper = Duration::from_secs(1 << attempt).min(Duration::from_secs(30));
        assert!(matches!(rithmic_wait, RetryDecision::Wait(delay)
            if delay >= rithmic_upper / 2 && delay <= rithmic_upper));
    }
}

#[test]
fn replay_superseded_generations() {
    use crate::market_service::descriptors::{InstrumentRef, InstrumentRequest, SelectionOutcome};
    let mut harness = ReplayHarness::new();
    let selected = harness.boundary.series();
    let reference = InstrumentRequest::Ref(InstrumentRef {
        provider_id: selected.provider_id,
        instrument_id: selected.instrument_id,
        entitlement_id: selected.entitlement_id,
        query: None,
    });
    assert!(matches!(harness.boundary.select(2, reference.clone(),
        aeris_market_data::ChartInterval::Minute1), SelectionOutcome::Selected { .. }));
    assert!(matches!(harness.boundary.select(1, reference,
        aeris_market_data::ChartInterval::Minute1), SelectionOutcome::Failed { .. }));
    assert_eq!(harness.transcript.lock().unwrap().connects.len(), 1);
}

#[test]
fn replay_overflow_covering_snapshot() {
    let mut boundary = Boundary::new();
    boundary.demand(boundary.series(), 1);
    let request = boundary.history_request();
    boundary.complete(request, minute_bars(0, 3), Some(minute_bars(3, 1)[0]));
    boundary.events();
    for minute in 4..=4 + 1_025 {
        boundary.trade(u64::try_from(minute).expect("positive fixture minute"), minute);
    }
    assert!(boundary.events().iter().any(|event| matches!(event,
        MarketRuntimeEvent::SeriesSnapshot(snapshot) if snapshot.snapshot.bars.len() > 1_024)));
}

#[test]
fn replay_quote_lease_share_release() {
    use aeris_market_engine::{MarketStream, StreamRequirements};
    let mut harness = ReplayHarness::new();
    let series = harness.boundary.series();
    let (chart, _) = harness.boundary.coordinator.engine.acquire_data_lease(
        &series, StreamRequirements::BARS.with(MarketStream::Quotes)).unwrap();
    let key = crate::market_service::InstrumentStreamKey {
        provider: series.provider_id.clone(),
        instrument: aeris_instruments::InstrumentId::try_new(series.instrument_id.clone()).unwrap(),
        entitlement: Some(series.entitlement_id.clone()),
    };
    let trading = harness.boundary.coordinator.engine.acquire_instrument_lease(
        &key, StreamRequirements::NONE.with(MarketStream::Quotes)).unwrap();
    let alert = harness.boundary.coordinator.engine.acquire_instrument_lease(
        &key, StreamRequirements::NONE.with(MarketStream::Quotes)).unwrap();
    assert_eq!(harness.boundary.coordinator.engine.instrument_subscriptions().count(), 1);
    for lease in [chart, trading] {
        assert!(harness.boundary.coordinator.engine.release_data_lease(lease));
        assert_eq!(harness.boundary.coordinator.engine.instrument_subscriptions().count(), 1);
    }
    assert!(harness.boundary.coordinator.engine.release_data_lease(alert));
    assert_eq!(harness.boundary.coordinator.engine.instrument_subscriptions().count(), 0);
}

#[test]
fn replay_history_exhaustion() {
    let mut harness = ReplayHarness::new();
    let series = harness.boundary.series();
    harness.run([ReplayStep::Demand(series.clone(), 1)]);
    let request = harness.transcript.lock().unwrap().history.pop_front().unwrap();
    let provider_epoch = request.ticket.provider;
    harness.boundary.complete(request, minute_bars(10, 3), Some(minute_bars(13, 1)[0]));
    harness.boundary.viewport(1, 0, minute_bars(10, 1)[0].exchange_timestamp_unix_nanos);
    let older = harness.transcript.lock().unwrap().history.pop_front().expect("older page");
    assert!(older.range.is_some());
    harness.boundary.complete_with_exhaustion(older, Vec::new(), None, true);
    assert!(harness.boundary.coordinator.history.coverage(&series, provider_epoch)
        .is_some_and(|coverage| coverage.backwards_exhausted));
    assert!(harness.boundary.coordinator.engine.has_subscription(&series));
    assert!(harness.boundary.coordinator.engine.series_snapshot(&series)
        .is_some_and(|snapshot| snapshot.bars.len() == 3));
    assert!(harness.requests().is_empty());
    harness.boundary.viewport(1, 0, minute_bars(10, 1)[0].exchange_timestamp_unix_nanos);
    assert!(harness.requests().is_empty(), "exhausted edge must not request again");
}

fn replay_ctrader_stall(phase: aeris_instruments::SessionPhase) ->
    crate::market_service::lifecycle::SeriesState
{
    use crate::market_service::{
        clock::{Clock, ScriptedClock},
        history::tracker::{HistoryTicket, LaneKind},
        lifecycle::{EffectBuf, RecoveryReason, SeriesInput, SeriesLifecycle, StepContext},
    };
    use aeris_market_engine::{ProviderGeneration, fence::{DemandTag, EpochSequence}};
    let clock = Arc::new(ScriptedClock::new(
        utc_ms("2026-10-19T00:30:00Z") * 1_000_000,
    ));
    let series = BarSeriesKey {
        provider_id: "ctrader".into(),
        instrument_id: "ctrader:EURUSD".into(),
        entitlement_id: "ctrader-demo".into(),
        period: aeris_market_data::BarPeriod::time(60).unwrap(),
        definition_version: 1,
    };
    let provider = EpochSequence::<ProviderTag>::default().next();
    let ticket = HistoryTicket {
        series: series.clone(), provider,
        demand: EpochSequence::<DemandTag>::default().next(),
        lane: LaneKind::Current, seq: 1,
    };
    let mut lifecycle = SeriesLifecycle::with_clock(
        series, ProviderGeneration(provider.get()), "EURUSD", "1m",
        crate::market_service::CandleGapPolicy::Contiguous, clock.clone(),
    );
    let mut effects = EffectBuf::default();
    for input in [SeriesInput::Online(provider), SeriesInput::HistoryRequested(&ticket),
        SeriesInput::Seeded(Some(&ticket))] {
        lifecycle.step(input, &StepContext {
            clock: clock.as_ref(), phase: aeris_instruments::SessionPhase::Open,
            next_transition: None,
        }, &mut effects);
    }
    assert!(matches!(lifecycle.state(), crate::market_service::lifecycle::SeriesState::Live { .. }));
    clock.advance(crate::market_service::watchdog::OPEN_STALL_THRESHOLD + Duration::from_secs(1));
    lifecycle.step(SeriesInput::Recover(RecoveryReason::StalledWhileOpen),
        &StepContext { clock: clock.as_ref(), phase, next_transition: None }, &mut effects);
    let _ = clock.monotonic();
    lifecycle.state().clone()
}

#[test]
fn replay_stall_while_open() {
    assert!(matches!(replay_ctrader_stall(aeris_instruments::SessionPhase::Open),
        crate::market_service::lifecycle::SeriesState::Recovering { .. }));
}

#[test]
fn replay_stall_while_closed() {
    assert!(matches!(replay_ctrader_stall(aeris_instruments::SessionPhase::Closed),
        crate::market_service::lifecycle::SeriesState::Live { .. }));
}
