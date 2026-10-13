// Coordinator command/event characterization. These tests observe replies and
// consumer publications, not the coordinator's internal ledgers.

use super::*;

struct Boundary {
    coordinator: Coordinator<'static>,
    history: std::sync::mpsc::Receiver<super::super::super::HistoryRequest>,
    series: BarSeriesKey,
    consumer: ConsumerId,
}

impl Boundary {
    fn new() -> Self {
        let (mut coordinator, history) = connected_rithmic_coordinator();
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
        Self {
            coordinator,
            history,
            series: series(),
            consumer,
        }
    }

    fn demand(&mut self, series: BarSeriesKey, generation_number: u64) {
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        self.coordinator.handle_command(Command::Demand(
            client(1),
            self.consumer,
            generation(generation_number),
            series,
            StreamRequirements::BARS,
            reply,
        ));
        received.recv().expect("demand reply").expect("demand succeeds");
    }

    fn complete(&mut self, provider_number: u64, bars: Vec<MarketBar>, forming: Option<MarketBar>) {
        self.coordinator.handle_command(Command::HistoryCompleted(
            self.series.clone(),
            ProviderGeneration(nonzero(provider_number)),
            None,
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 0,
                bars,
                forming: forming.map(|bar| FormingBar { bar, trades: None }),
                handoff_boundary_unix_nanos: Some(180_000_000_000),
                backwards_exhausted: false,
            }),
        ));
    }

    fn events(&mut self) -> Vec<MarketRuntimeEvent> {
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        self.coordinator
            .handle_command(Command::PollClient(client(1), vec![(self.consumer, 256)], reply));
        received
            .recv()
            .expect("poll reply")
            .expect("poll succeeds")
            .into_iter()
            .map(|(_, event)| event)
            .collect()
    }

    fn status(&mut self) -> MarketServiceStatus {
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        self.coordinator.handle_command(Command::Status(reply));
        received.recv().expect("status reply").expect("status succeeds")
    }

    fn trade(&mut self, sequence: u64, minute: i64) {
        self.coordinator.handle_provider_event(
            super::super::super::ProviderEvent::rithmic(RithmicRealtimeEvent::Trade(
                1,
                rithmic_trade_for_series(&self.series, sequence, minute * 60_000_000_000),
            )),
        );
        self.coordinator.publish_rithmic_live();
    }
}

#[test]
fn characterization_reseed_during_live_flow_keeps_one_forming_bar_and_contiguous_history() {
    let mut boundary = Boundary::new();
    boundary.demand(boundary.series.clone(), 1);
    boundary.history.try_recv().expect("initial history request");
    boundary.complete(1, minute_bars(0, 3), Some(minute_bars(3, 1)[0]));
    boundary.events();

    boundary.trade(1, 3);
    boundary.coordinator.handle_provider_event(
        super::super::super::ProviderEvent::rithmic(RithmicRealtimeEvent::Recovering(
            1,
            None,
        )),
    );
    boundary.trade(2, 4);
    boundary.complete(1, minute_bars(0, 4), Some(minute_bars(4, 1)[0]));
    boundary.trade(3, 4);

    let snapshots: Vec<_> = boundary
        .events()
        .into_iter()
        .filter_map(|event| match event {
            MarketRuntimeEvent::SeriesSnapshot(publication) => Some(publication.snapshot),
            _ => None,
        })
        .collect();
    let latest = snapshots.last().expect("a covering snapshot after reseed");
    assert!(latest.forming, "only the last bar is forming");
    assert_eq!(
        latest
            .bars
            .windows(2)
            .map(|bars| bars[1].exchange_timestamp_unix_nanos - bars[0].exchange_timestamp_unix_nanos)
            .collect::<Vec<_>>(),
        vec![60_000_000_000; latest.bars.len() - 1]
    );
}

#[test]
fn characterization_outbox_overflow_emits_covering_snapshot() {
    let mut boundary = Boundary::new();
    boundary.demand(boundary.series.clone(), 1);
    boundary.history.try_recv().expect("initial history request");
    boundary.complete(1, minute_bars(0, 3), Some(minute_bars(3, 1)[0]));
    boundary.events();
    for minute in 4..(4
        + i64::try_from(super::super::super::CONSUMER_SERIES_QUEUE_CAPACITY)
            .expect("bounded consumer queue")
        + 2)
    {
        boundary.trade(u64::try_from(minute).expect("positive fixture minute"), minute);
    }
    boundary.coordinator.recover_overflowed_series_queues();
    assert!(boundary.events().iter().any(|event| match event {
        MarketRuntimeEvent::SeriesSnapshot(publication) => publication.snapshot.bars.len() > 1_024,
        _ => false,
    }));
}

#[test]
fn characterization_stale_generation_history_completion_is_ignored() {
    let mut boundary = Boundary::new();
    boundary.demand(boundary.series.clone(), 1);
    boundary.history.try_recv().expect("initial history request");
    boundary.coordinator.handle_provider_event(
        super::super::super::ProviderEvent::rithmic(RithmicRealtimeEvent::Connecting(2)),
    );
    boundary.events();
    boundary.complete(1, minute_bars(0, 3), None);
    assert!(
        !boundary.events().iter().any(|event| {
            matches!(event, MarketRuntimeEvent::SeriesSnapshot(_))
        }),
        "the retired history response cannot publish bars"
    );
}

#[test]
fn characterization_symbol_or_timeframe_change_keeps_provider_session() {
    let mut boundary = Boundary::new();
    boundary.demand(boundary.series.clone(), 1);
    let initial = boundary
        .status()
        .providers
        .into_iter()
        .find(|state| state.provider == "rithmic")
        .expect("provider status");
    let mut other = boundary.series.clone();
    other.period = BarPeriod::time(300).expect("five minute interval");
    boundary.demand(other, 2);
    let mut symbol = instrument();
    symbol.instrument_id = "instrument:rithmic:CME:MESU6".into();
    symbol.provider_symbol = "MESU6".into();
    symbol.display_symbol = "MES Sep 2026".into();
    symbol.entitlement_id = "rithmic-test:CME:MESU6".into();
    let (reply, received) = std::sync::mpsc::sync_channel(1);
    boundary
        .coordinator
        .handle_command(Command::InstallProviderInstrument(symbol.clone(), reply));
    received
        .recv()
        .expect("install reply")
        .expect("second symbol installs");
    let mut second_series = boundary.series.clone();
    second_series.instrument_id = symbol.instrument_id;
    second_series.entitlement_id = symbol.entitlement_id;
    boundary.demand(second_series, 3);
    let after = boundary
        .status()
        .providers
        .into_iter()
        .find(|state| state.provider == "rithmic")
        .expect("provider status");
    assert_eq!(initial.generation, after.generation);
}
