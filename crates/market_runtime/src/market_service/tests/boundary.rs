// Scripted coordinator driver. Fixtures stay here; scenarios observe commands,
// published consumer events, and provider history requests only.
use super::*;

pub(super) struct Boundary {
    coordinator: Coordinator<'static>,
    history: std::sync::mpsc::Receiver<super::super::super::HistoryRequest>,
    series: BarSeriesKey,
    consumer: ConsumerId,
    provider_generation: u64,
}

impl Boundary {
    pub(super) fn new() -> Self {
        let (mut coordinator, history) = connected_rithmic_coordinator();
        coordinator.clock = std::sync::Arc::new(super::super::super::clock::ScriptedClock::new(
            1_800_000_000_000_000_000,
        ));
        coordinator.handle_provider_event(
            super::super::super::ProviderEvent::rithmic(RithmicRealtimeEvent::Connected(1)),
        );
        let consumer = consumer(1);
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_command(Command::Attach(client(1), reply));
        received.recv().expect("attach reply").expect("attach succeeds");
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        coordinator.handle_command(Command::Register(
            ConsumerIdentity {
                client_id: client(1),
                workspace_id: WorkspaceId(nonzero(1)),
                consumer_id: consumer,
            },
            reply,
        ));
        received.recv().expect("register reply").expect("register succeeds");
        Self { coordinator, history, series: series(), consumer, provider_generation: 1 }
    }

    pub(super) fn series(&self) -> BarSeriesKey { self.series.clone() }

    pub(super) fn demand(&mut self, series: BarSeriesKey, generation_number: u64) {
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        self.coordinator.handle_command(Command::Demand(
            client(1), self.consumer, generation(generation_number),
            series, StreamRequirements::BARS, reply,
        ));
        received.recv().expect("demand reply").expect("demand succeeds");
    }

    pub(super) fn history_request(&self) -> super::super::super::HistoryRequest {
        self.history.try_recv().expect("provider history request")
    }

    pub(super) fn complete(
        &mut self,
        request: super::super::super::HistoryRequest,
        bars: Vec<MarketBar>,
        forming: Option<MarketBar>,
    ) {
        self.coordinator.handle_command(Command::HistoryCompleted(
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
        self.coordinator.handle_command(Command::PollClient(client(1), vec![(self.consumer, 256)], reply));
        received.recv().expect("poll reply").expect("poll succeeds")
            .into_iter().map(|(_, event)| event).collect()
    }

    pub(super) fn status(&mut self) -> MarketServiceStatus {
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        self.coordinator.handle_command(Command::Status(reply));
        received.recv().expect("status reply").expect("status succeeds")
    }

    pub(super) fn provider_event(&mut self, event: RithmicRealtimeEvent) {
        if let RithmicRealtimeEvent::Connecting(next) = &event {
            self.provider_generation = *next;
        }
        self.coordinator.handle_provider_event(super::super::super::ProviderEvent::rithmic(event));
    }

    pub(super) fn trade(&mut self, sequence: u64, minute: i64) {
        let mut trade = rithmic_trade_for_series(&self.series, sequence, minute * 60_000_000_000);
        trade.metadata.session_generation = self.provider_generation;
        self.provider_event(RithmicRealtimeEvent::Trade(
            self.provider_generation, trade,
        ));
        self.tick();
    }

    pub(super) fn tick(&mut self) { benchmark_turn(&mut self.coordinator); }

    pub(super) fn install(&mut self, instrument: InstallProviderInstrument) {
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        self.coordinator.handle_command(Command::InstallProviderInstrument(instrument, reply));
        received.recv().expect("install reply").expect("instrument installs");
    }
}
