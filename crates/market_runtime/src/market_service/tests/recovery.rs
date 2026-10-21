// Coordinator command/registry characterization, observing provider demand rather
// than inspecting the lease registry's internal counters.
use super::{boundary::Boundary, *};
use crate::market_service::{
    HistoryRequest, ProviderDemand,
    provider_session::{ProviderEventSink, ProviderSession, ProviderSessionError},
};
use aeris_market_engine::{
    MarketStream, StreamRequirements,
    fence::{Epoch, ProviderTag},
};
use std::sync::{Arc, Mutex};
use std::{sync::mpsc, time::Duration};

#[derive(Default)]
struct DemandTranscript {
    demands: Vec<ProviderDemand>,
    history: Vec<HistoryRequest>,
    connects: Vec<u64>,
    idle_releases: usize,
}

struct RecordingSession {
    transcript: Arc<Mutex<DemandTranscript>>,
    adapter_retries_transport: bool,
}

impl ProviderSession for RecordingSession {
    fn adapter_retries_transport(&self) -> bool { self.adapter_retries_transport }

    fn connect(
        &self,
        epoch: Epoch<ProviderTag>,
        _: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError> {
        self.transcript.lock().expect("demand transcript").connects.push(epoch.get().get());
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
        self.transcript.lock().expect("demand transcript").demands.push(demand.clone());
        Ok(true)
    }

    fn request_history(&self, request: HistoryRequest) -> Result<bool, ProviderSessionError> {
        self.transcript.lock().expect("demand transcript").history.push(request);
        Ok(true)
    }

    fn release_idle(&self) -> Result<bool, ProviderSessionError> {
        self.transcript.lock().expect("demand transcript").idle_releases += 1;
        Ok(true)
    }
    fn shutdown(&self) {}
}

#[test]
fn chart_and_price_alert_share_one_provider_quote_until_last_release() {
    let mut boundary = Boundary::new();
    let transcript = Arc::new(Mutex::new(DemandTranscript::default()));
    boundary.register_replay_session(Box::new(RecordingSession {
        transcript: Arc::clone(&transcript),
        adapter_retries_transport: true,
    }));
    let series = boundary.series();
    boundary.demand_with_streams(
        series,
        1,
        StreamRequirements::BARS.with(MarketStream::Quotes),
    );
    let installed = instrument();
    let alert = crate::MarketPriceAlert {
        id: "shared-quote".into(),
        instrument: installed,
        price: 100,
        condition: aeris_contracts::PriceAlertCondition::CrossingUp,
        frequency: aeris_contracts::PriceAlertFrequency::EveryTime,
        active: true,
    };
    boundary.register_alert_consumer();
    boundary.replace_alerts(vec![alert]);
    boundary.tick();
    boundary.remove_consumer();
    boundary.tick();
    assert_eq!(
        transcript.lock().expect("demand transcript").idle_releases,
        0,
        "releasing the chart leaves the alert subscription in place"
    );
    boundary.replace_alerts(Vec::new());
    boundary.tick();

    let transcript = transcript.lock().expect("demand transcript");
    let subscriptions = transcript.demands.iter().map(|demand| {
        let wire = demand.rithmic_wire().expect("installed instrument");
        let quotes = wire.instruments.iter().filter(|item| item.quotes).count();
        assert!(quotes <= 1, "a shared quote never duplicates the wire subscription");
        quotes
    }).collect::<Vec<_>>();
    assert!(
        subscriptions.contains(&1),
        "chart demand reaches the provider"
    );
    assert_eq!(transcript.idle_releases, 1, "last release unsubscribes once");
    assert!(
        subscriptions.iter().all(|&count| count == 1),
        "removing the chart must not unsubscribe the alert's quote"
    );
}

fn utc_minute(timestamp: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .expect("UTC timestamp")
        .timestamp()
        .div_euclid(60)
}

#[test]
fn cme_day_bar_rolls_at_chicago_session_boundary_on_both_sides_of_dst() {
    for (open, last_trade, next_open) in [
        ("2026-03-02T23:00:00Z", "2026-03-03T21:59:00Z", "2026-03-03T23:00:00Z"),
        ("2026-03-09T22:00:00Z", "2026-03-10T20:59:00Z", "2026-03-10T22:00:00Z"),
        ("2026-11-02T23:00:00Z", "2026-11-03T21:59:00Z", "2026-11-03T23:00:00Z"),
    ] {
        let mut boundary = Boundary::new();
        let mut series = boundary.series();
        series.period = BarPeriod::session(1).expect("daily session");
        boundary.demand(series.clone(), 1);
        let request = boundary.history_request();
        let forming = minute_bars(utc_minute(open), 1)[0];
        boundary.complete(
            request,
            minute_bars(utc_minute(open) - 24 * 60, 1),
            Some(forming),
        );
        boundary.events();
        boundary.trade_for(&series, 1, utc_minute(last_trade));
        boundary.trade_for(&series, 2, utc_minute(next_open));
        let events = boundary.events();
        let updates = events.iter().filter_map(|event| match event {
            MarketRuntimeEvent::SeriesUpdate(update) => Some(update),
            _ => None,
        }).collect::<Vec<_>>();
        assert!(
            updates.iter().any(|update| update.bar.exchange_timestamp_unix_nanos
                == utc_minute(open) * 60_000_000_000
                && update.operation == aeris_market_engine::SeriesTailOperation::Revise)
                && updates.iter().any(|update| update.bar.exchange_timestamp_unix_nanos
                    == utc_minute(next_open) * 60_000_000_000
                    && update.operation == aeris_market_engine::SeriesTailOperation::Append),
            "{open}: published prior bar must give way to the next 17:00 CT session bar: {events:?}"
        );
    }
}

#[test]
fn installed_non_cme_instruments_resolve_contract_continuous_and_unknown_calendars() {
    use aeris_instruments::{CalendarSourceKind, SessionPhase};

    let mut boundary = Boundary::new();
    let mut contract = instrument();
    contract.instrument_id = "instrument:rithmic:FX:EURUSD".into();
    contract.provider_symbol = "EURUSD".into();
    contract.venue_id = "unmapped".into();
    contract.contract_metadata = Some(Box::new(aeris_contracts::ProviderContractMetadata {
        session_hours: vec![aeris_contracts::ProviderSessionHours {
            weekday: 1,
            open_seconds: 0,
            close_seconds: 3_600,
            timezone: "UTC".into(),
        }],
        ..Default::default()
    }));
    boundary.install(contract.clone());
    let at = utc_minute("2026-10-19T00:30:00Z") * 60_000;
    assert_eq!(
        boundary.coordinator.calendar.source_kind(&contract.instrument_id),
        CalendarSourceKind::ContractHours
    );
    assert_eq!(
        boundary.coordinator.calendar.phase(&contract.instrument_id, at),
        SessionPhase::Open
    );
    assert_eq!(
        boundary.coordinator.calendar.phase(&contract.instrument_id, at + 3_600_000),
        SessionPhase::Closed
    );

    let mut continuous = contract.clone();
    continuous.instrument_id = "instrument:rithmic:crypto:BTC".into();
    continuous.provider_symbol = "BTC".into();
    continuous.venue_id = "Hyperliquid".into();
    continuous.contract_metadata = None;
    boundary.install(continuous.clone());
    assert_eq!(
        boundary.coordinator.calendar.source_kind(&continuous.instrument_id),
        CalendarSourceKind::AlwaysOpen
    );
    assert_eq!(
        boundary.coordinator.calendar.phase(&continuous.instrument_id, at),
        SessionPhase::AlwaysOpen
    );

    let mut unknown = continuous.clone();
    unknown.instrument_id = "instrument:rithmic:unmapped:SYMBOL".into();
    unknown.provider_symbol = "SYMBOL".into();
    unknown.venue_id = "unmapped".into();
    boundary.install(unknown.clone());
    assert_eq!(
        boundary.coordinator.calendar.source_kind(&unknown.instrument_id),
        CalendarSourceKind::Unknown
    );
    assert_eq!(
        boundary.coordinator.calendar.phase(&unknown.instrument_id, at),
        SessionPhase::Unknown
    );
}

fn scripted_hyperliquid_coordinator(
    clock: &Arc<crate::market_service::clock::ScriptedClock>,
    transcript: &Arc<Mutex<DemandTranscript>>,
) -> (
    Coordinator<'static>,
    mpsc::SyncSender<aeris_market_engine::fence::Fenced<ProviderTag, crate::hyperliquid_realtime::HyperliquidRealtimeEvent>>,
) {
    use crate::market_service::reconnect::ReconnectPolicy;
    let (history, _history_receiver) = mpsc::sync_channel(8);
    let (controls, _control_receiver) = mpsc::sync_channel(8);
    let (display_controls, _display_control_receiver) = mpsc::sync_channel(8);
    let (_display_events, display_receiver) = mpsc::sync_channel(8);
    let (events, event_receiver) = mpsc::sync_channel(8);
    let channels = Box::leak(Box::new(crate::market_service::HyperliquidChannels {
        controls,
        events: event_receiver,
        display_controls,
        display_events: display_receiver,
    }));
    let providers = crate::market_service::ProviderDispatch {
        wake: None,
        records: std::collections::BTreeMap::from([(
            "hyperliquid",
            crate::market_service::ProviderDispatchRecord {
                descriptor: crate::market_service::HYPERLIQUID_DESCRIPTOR,
                history: Box::leak(Box::new(history)),
                lifecycle: None,
                hyperliquid: Some(channels),
                rithmic: None,
                catalog: None,
            },
        )]),
        lanes: vec![crate::market_service::ProviderEventLane::Realtime("hyperliquid")],
    };
    let mut coordinator = Coordinator::with_clock(
        crate::market_service::configured_engine().expect("configured engine"),
        providers,
        clock.clone(),
    );
    coordinator.provider_registry.register(
        "hyperliquid",
        Box::new(RecordingSession {
            transcript: Arc::clone(transcript),
            adapter_retries_transport: false,
        }),
        ReconnectPolicy::hyperliquid(),
        clock.as_ref(),
    );
    (coordinator, events)
}

#[test]
fn hyperliquid_coordinator_retries_at_bounded_jittered_deadlines() {
    use crate::hyperliquid_realtime::HyperliquidRealtimeEvent;
    use crate::market_service::reconnect::{Backoff, ReconnectPolicy, RetryClass, RetryDecision};
    let clock = Arc::new(crate::market_service::clock::ScriptedClock::new(
        utc_minute("2026-08-17T14:00:00Z") * 60_000_000_000,
    ));
    let transcript = Arc::new(Mutex::new(DemandTranscript::default()));
    let (mut coordinator, events) = scripted_hyperliquid_coordinator(&clock, &transcript);
    let (commands, command_receiver) = mpsc::sync_channel(8);
    let turn = |coordinator: &mut Coordinator<'static>| {
        assert!(coordinator_turn(
            coordinator,
            &command_receiver,
            &[crate::market_service::HYPERLIQUID_DESCRIPTOR.flush_demand],
            &std::sync::atomic::AtomicBool::new(false),
            Some(Duration::ZERO),
        ));
    };
    let (reply, received) = mpsc::sync_channel(1);
    commands.send(Command::Attach(client(1), reply)).expect("attach command");
    turn(&mut coordinator);
    received.recv().expect("attach reply").expect("attached");
    let (reply, received) = mpsc::sync_channel(1);
    commands.send(Command::Register(ConsumerIdentity {
        client_id: client(1),
        workspace_id: WorkspaceId(nonzero(1)),
        consumer_id: consumer(1),
    }, reply)).expect("register command");
    turn(&mut coordinator);
    received.recv().expect("register reply").expect("registered");
    assert!(coordinator.ensure_provider_connection("hyperliquid"));
    let mut installed = hyperliquid_instrument();
    installed.session_generation = coordinator.provider_registry.epoch("hyperliquid")
        .expect("provider epoch").get().get();
    let (reply, received) = mpsc::sync_channel(1);
    commands.send(Command::InstallProviderInstrument(installed, reply)).expect("install");
    turn(&mut coordinator);
    received.recv().expect("install reply").expect("installed");
    let (reply, received) = mpsc::sync_channel(1);
    commands.send(Command::Demand(
        client(1), consumer(1), generation(1), hyperliquid_series(),
        StreamRequirements::BARS, reply,
    )).expect("demand");
    turn(&mut coordinator);
    received.recv().expect("demand reply").expect("demanded");
    turn(&mut coordinator);
    let mut expected = Backoff::new(ReconnectPolicy::hyperliquid(), clock.as_ref(), 0x5052_4f56);
    for attempt in 0..6 {
        let epoch = coordinator.provider_registry.epoch("hyperliquid").expect("active epoch");
        let wire = epoch.get().get();
        for event in [
            HyperliquidRealtimeEvent::Connecting(wire),
            HyperliquidRealtimeEvent::Connected(wire),
            HyperliquidRealtimeEvent::Disconnected(wire),
        ] {
            events.send(aeris_market_engine::fence::Fenced { epoch, value: event })
                .expect("provider event");
        }
        turn(&mut coordinator);
        let RetryDecision::Wait(delay) = expected.failure(RetryClass::Transient) else {
            panic!("Hyperliquid retry is unbounded");
        };
        let deadline = coordinator.provider_registry.next_retry().expect("retry scheduled");
        assert_eq!(deadline.saturating_duration_since(
            crate::market_service::clock::Clock::monotonic(clock.as_ref())
        ), delay.max(Duration::from_millis(1)), "attempt {attempt}");
        if delay > Duration::from_millis(1) {
            clock.advance(delay.checked_sub(Duration::from_millis(1)).expect("positive delay"));
            turn(&mut coordinator);
            assert_eq!(transcript.lock().expect("connects").connects.len(), attempt + 1);
            clock.advance(Duration::from_millis(1));
        } else {
            clock.advance(Duration::from_millis(1));
        }
        turn(&mut coordinator);
        assert_eq!(transcript.lock().expect("connects").connects.len(), attempt + 2);
    }
}

#[test]
fn rithmic_transport_recovery_does_not_stack_coordinator_retry() {
    use aeris_rithmic_protocol_adapter::ProviderInvalidationReason;
    use crate::market_service::provider_event::ProviderEvent;
    let mut boundary = Boundary::new();
    let transcript = Arc::new(Mutex::new(DemandTranscript::default()));
    boundary.register_replay_session(Box::new(RecordingSession {
        transcript: Arc::clone(&transcript),
        adapter_retries_transport: true,
    }));
    boundary.demand(boundary.series(), 1);
    let epoch = boundary.coordinator.provider_registry.epoch("rithmic").expect("epoch");
    let wire = crate::market_service::provider_event::ProviderEvent::rithmic(
        RithmicRealtimeEvent::Disconnected(1, Some(ProviderInvalidationReason::Transport)),
    );
    boundary.coordinator.handle_bound_provider_event(ProviderEvent {
        provider: "rithmic",
        generation: epoch,
        wire_generation: wire.generation,
        kind: wire.kind,
    });
    boundary.tick();
    assert!(boundary.coordinator.provider_registry.next_retry().is_none());
    boundary.advance(Duration::from_secs(60));
    assert_eq!(transcript.lock().expect("connects").connects.len(), 1);
    boundary.coordinator.handle_bound_provider_event(ProviderEvent {
        provider: "rithmic",
        generation: epoch,
        wire_generation: 2,
        kind: crate::market_service::provider_event::ProviderEventKind::TransportReplaced,
    });
    boundary.tick();
    assert_eq!(transcript.lock().expect("connects").connects.len(), 2,
        "adapter replacement rebinds once without a coordinator reconnect");
    assert!(boundary.coordinator.provider_registry.next_retry().is_none());
    assert_eq!(
        crate::market_service::reconnect::ReconnectPolicy::rithmic_recovery().initial_delay(),
        Duration::from_secs(1),
        "adapter non-transport policy begins with a one-second jitter ceiling"
    );
}
