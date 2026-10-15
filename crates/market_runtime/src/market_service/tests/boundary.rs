// Scripted coordinator driver. Fixtures stay here; scenarios observe commands,
// published consumer events, and provider history requests only.
use super::*;

pub(super) struct Boundary {
    coordinator: Coordinator<'static>,
    commands: std::sync::mpsc::SyncSender<Command>,
    command_receiver: std::sync::mpsc::Receiver<Command>,
    provider_events: std::sync::mpsc::SyncSender<RithmicRealtimeEvent>,
    provider_requests: std::sync::mpsc::Receiver<super::super::super::RithmicRealtimeControl>,
    history: std::sync::mpsc::Receiver<super::super::super::HistoryRequest>,
    series: BarSeriesKey,
    consumer: ConsumerId,
    provider_generation: u64,
}

impl Boundary {
    pub(super) fn new() -> Self {
        let (coordinator, history, provider_events, provider_requests) = scripted_rithmic_coordinator(
            std::sync::Arc::new(super::super::super::clock::ScriptedClock::new(
                1_800_000_000_000_000_000,
            )),
        );
        let (commands, command_receiver) = std::sync::mpsc::sync_channel(32);
        let mut boundary = Self {
            coordinator,
            commands,
            command_receiver,
            provider_events,
            provider_requests,
            history,
            series: series(),
            consumer: consumer(1),
            provider_generation: 1,
        };
        boundary.provider_event(RithmicRealtimeEvent::Connecting(1));
        boundary.provider_event(RithmicRealtimeEvent::Connected(1));
        boundary.install(instrument());
        let consumer = consumer(1);
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        boundary.command(Command::Attach(client(1), reply));
        received.recv().expect("attach reply").expect("attach succeeds");
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        boundary.command(Command::Register(
            ConsumerIdentity {
                client_id: client(1),
                workspace_id: WorkspaceId(nonzero(1)),
                consumer_id: consumer,
            },
            reply,
        ));
        received.recv().expect("register reply").expect("register succeeds");
        boundary
    }

    fn command(&mut self, command: Command) {
        self.commands.send(command).expect("bounded coordinator command");
        self.tick();
    }

    pub(super) fn series(&self) -> BarSeriesKey { self.series.clone() }

    pub(super) fn demand(&mut self, series: BarSeriesKey, generation_number: u64) {
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        self.command(Command::Demand(
            client(1), self.consumer, generation(generation_number),
            series, StreamRequirements::BARS, reply,
        ));
        received.recv().expect("demand reply").expect("demand succeeds");
    }

    pub(super) fn select(
        &mut self,
        generation_number: u64,
        instrument: super::super::super::descriptors::InstrumentRequest,
        interval: aeris_market_data::ChartInterval,
    ) -> super::super::super::descriptors::SelectionOutcome {
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        self.command(Command::SelectAndDemand(
            client(1),
            super::super::super::descriptors::SelectionRequest {
                consumer_id: self.consumer.0.get(),
                generation: generation_number,
                instrument,
                interval,
                streams: StreamRequirements::BARS,
                viewport: None,
            },
            reply,
        ));
        received.recv().expect("exactly one selection reply").expect("transport")
    }

    pub(super) fn history_request(&self) -> super::super::super::HistoryRequest {
        self.history.try_recv().expect("provider history request")
    }

    pub(super) fn provider_requests(&self) -> Vec<super::super::super::RithmicRealtimeControl> {
        self.provider_requests.try_iter().collect()
    }

    pub(super) fn complete(
        &mut self,
        request: super::super::super::HistoryRequest,
        bars: Vec<MarketBar>,
        forming: Option<MarketBar>,
    ) {
        self.command(Command::HistoryCompleted(
            request.series,
            request.provider_generation,
            request.range,
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 0,
                bars,
                forming: forming.map(|bar| FormingBar { bar, trades: None }),
                handoff_boundary_unix_nanos: Some(180_000_000_000),
                backwards_exhausted: false,
            }),
            request.ticket,
        ));
    }

    pub(super) fn events(&mut self) -> Vec<MarketRuntimeEvent> {
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        self.command(Command::DrainClient(client(1), vec![(self.consumer, 256)], reply));
        received.recv().expect("poll reply").expect("poll succeeds")
            .into_iter().map(|(_, event)| event.value).collect()
    }

    pub(super) fn status(&mut self) -> MarketServiceStatus {
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        self.command(Command::Status(reply));
        received.recv().expect("status reply").expect("status succeeds")
    }

    pub(super) fn provider_event(&mut self, event: RithmicRealtimeEvent) {
        if let RithmicRealtimeEvent::Connecting(next) = &event {
            self.provider_generation = *next;
        }
        self.provider_events.send(event).expect("bounded provider event");
        self.tick();
    }

    pub(super) fn trade(&mut self, sequence: u64, minute: i64) {
        let mut trade = rithmic_trade_for_series(&self.series, sequence, minute * 60_000_000_000);
        trade.metadata.session_generation = self.provider_generation;
        self.provider_event(RithmicRealtimeEvent::Trade(
            self.provider_generation, trade,
        ));
        self.tick();
    }

    pub(super) fn tick(&mut self) {
        assert!(coordinator_turn(
            &mut self.coordinator,
            &self.command_receiver,
            &[super::super::super::RITHMIC_DESCRIPTOR.flush_demand],
            &std::sync::atomic::AtomicBool::new(false),
            Some(std::time::Duration::ZERO),
        ));
    }

    pub(super) fn install(&mut self, instrument: InstallProviderInstrument) {
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        self.command(Command::InstallProviderInstrument(instrument, reply));
        received.recv().expect("install reply").expect("instrument installs");
    }
}

#[test]
fn select_and_demand_rejects_invalid_interval_without_retiring_previous_series() {
    use super::super::super::descriptors::{InstrumentRef, InstrumentRequest, SelectionOutcome};

    let mut boundary = Boundary::new();
    boundary.demand(boundary.series(), 1);
    let initial = boundary.history_request();
    boundary.complete(initial, minute_bars(0, 3), Some(minute_bars(3, 1)[0]));
    boundary.events();
    let reference = InstrumentRequest::Ref(InstrumentRef {
        provider_id: "rithmic".into(),
        instrument_id: boundary.series().instrument_id,
        entitlement_id: boundary.series().entitlement_id,
    });
    let failed = boundary.select(2, reference, aeris_market_data::ChartInterval::Day3);
    assert!(matches!(failed, SelectionOutcome::Failed { .. }));
    boundary.trade(1, 4);
    assert!(boundary.events().iter().any(|event| matches!(
        event,
        MarketRuntimeEvent::SeriesUpdate(update) if update.generation == generation(1)
    )));
}

#[test]
fn select_and_demand_resolves_once_and_rejects_retired_generation() {
    use super::super::super::descriptors::{InstrumentRef, InstrumentRequest, SelectionOutcome};

    let mut boundary = Boundary::new();
    let reference = InstrumentRequest::Ref(InstrumentRef {
        provider_id: "rithmic".into(),
        instrument_id: boundary.series().instrument_id,
        entitlement_id: boundary.series().entitlement_id,
    });
    let selected = boundary.select(2, reference.clone(), aeris_market_data::ChartInterval::Minute1);
    assert!(matches!(selected, SelectionOutcome::Selected { generation: selected, .. } if selected == generation(2)));
    let stale = boundary.select(1, reference, aeris_market_data::ChartInterval::Minute1);
    assert!(matches!(stale, SelectionOutcome::Failed { generation: retired, .. } if retired == generation(1)));
    assert_eq!(
        boundary.coordinator.engine.current_demand(boundary.consumer)
            .and_then(|demand| demand.generation),
        Some(generation(2)),
    );
}
