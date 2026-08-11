//! Single-owner resident market coordinator and Coinbase history/realtime workers.

use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axiusflow_coinbase_market_adapter::{
    COINBASE_PUBLIC_ACCOUNT_ID, CanonicalTrade, CoinbaseBarAggregator, CoinbaseBarAggregatorConfig,
    CoinbaseConfig, CoinbaseHistoryCapabilityAdapter, CoinbaseSession, ENTITLEMENT_CLASS,
    decode_history_bar,
};
use axiusflow_local_engine_protocol::{
    DemandError, EngineFaultCode, MarketBar as IpcMarketBar, PersistenceState,
    ProviderConnectionState, ProviderState, SeriesKey, SeriesLoadState,
    SeriesSnapshot as IpcSeriesSnapshot, SeriesState, envelope,
};
use axiusflow_market_data::{BarPeriod, BarSeriesKey, MarketBar};
use axiusflow_market_engine::{
    ClientId, ConsumerId, ConsumerIdentity, EngineError, GenerationId, MarketEngine,
    MarketEngineConfig, ProviderCapabilities, ProviderGeneration, ProviderHealth, Viewport,
    WorkspaceId,
};
use axiusflow_provider_history::{DataClass, HistoryPageRequest, HistoryRange};

const COMMAND_CAPACITY: usize = 64;
const HISTORY_CAPACITY: usize = 8;
const REALTIME_CAPACITY: usize = 2_048;
const REALTIME_DRAIN_BUDGET: usize = 256;
const LIVE_BUFFER_CAPACITY: usize = 4_096;
const COORDINATOR_TICK: Duration = Duration::from_millis(16);
const RECONNECT_DELAY: Duration = Duration::from_millis(250);
const MAXIMUM_CONSUMERS: usize = 256;
const MAXIMUM_SERIES: usize = 128;
const HISTORY_BARS_PER_SERIES: usize = 350;
const MAXIMUM_STORED_BARS: usize = MAXIMUM_SERIES * (HISTORY_BARS_PER_SERIES + 1);
const COINBASE_PROVIDER_GENERATION: u64 = 1;

type Reply<T> = SyncSender<Result<T, String>>;

/// Cloneable command boundary for the process-owned market coordinator.
#[derive(Clone)]
pub struct MarketService {
    commands: SyncSender<Command>,
}

enum Command {
    Attach(ClientId, Reply<()>),
    Detach(ClientId, Reply<()>),
    Register(ConsumerIdentity, Reply<()>),
    Remove(ClientId, ConsumerId, Reply<()>),
    Viewport(ClientId, ConsumerId, GenerationId, Viewport, Reply<()>),
    Visibility(ClientId, ConsumerId, bool, Reply<()>),
    Demand(
        ClientId,
        ConsumerId,
        GenerationId,
        BarSeriesKey,
        Reply<Vec<envelope::Payload>>,
    ),
    Poll(ClientId, ConsumerId, Reply<Option<envelope::Payload>>),
    HistoryCompleted(
        BarSeriesKey,
        ProviderGeneration,
        Result<HistorySnapshot, String>,
    ),
}

struct HistoryRequest {
    series: BarSeriesKey,
    provider_generation: ProviderGeneration,
}

struct HistorySnapshot {
    price_scale: u8,
    quantity_scale: u8,
    bars: Vec<MarketBar>,
}

struct DemandWaiter {
    consumer_id: ConsumerId,
    generation: GenerationId,
    reply: Reply<Vec<envelope::Payload>>,
}

enum RealtimeControl {
    Start,
}

enum RealtimeEvent {
    Connecting(ProviderGeneration),
    Connected(ProviderGeneration),
    Trade(ProviderGeneration, CanonicalTrade),
    Heartbeat(ProviderGeneration),
    Disconnected(ProviderGeneration),
}

#[derive(Default)]
struct ConsumerEvents {
    provider: Option<envelope::Payload>,
    snapshot: Option<envelope::Payload>,
    series_state: Option<envelope::Payload>,
}

impl ConsumerEvents {
    fn pop(&mut self) -> Option<envelope::Payload> {
        self.provider
            .take()
            .or_else(|| self.snapshot.take())
            .or_else(|| self.series_state.take())
    }
}

struct LiveHandoff {
    series: BarSeriesKey,
    generation: ProviderGeneration,
    aggregator: CoinbaseBarAggregator,
    buffered: VecDeque<CanonicalTrade>,
    connected: bool,
    history_ready: bool,
    dirty: bool,
}

impl LiveHandoff {
    fn try_new(series: BarSeriesKey, generation: ProviderGeneration) -> Result<Self, String> {
        Ok(Self {
            series,
            generation,
            aggregator: coinbase_aggregator()?,
            buffered: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            connected: false,
            history_ready: false,
            dirty: false,
        })
    }

    fn reset(&mut self, generation: ProviderGeneration) -> Result<(), String> {
        self.generation = generation;
        self.aggregator = coinbase_aggregator()?;
        self.buffered.clear();
        self.connected = false;
        self.history_ready = false;
        self.dirty = false;
        Ok(())
    }
}

trait HistorySource: Send + 'static {
    fn fetch(&mut self, series: &BarSeriesKey) -> Result<HistorySnapshot, String>;
}

trait RealtimeSource: Send + 'static {
    fn run_generation(
        &mut self,
        generation: ProviderGeneration,
        events: &SyncSender<RealtimeEvent>,
        overflow: &AtomicBool,
        stop: &Arc<AtomicBool>,
    );
}

struct LiveCoinbaseHistory {
    adapter: CoinbaseHistoryCapabilityAdapter,
}

struct LiveCoinbaseRealtime {
    config: CoinbaseConfig,
}

#[cfg(test)]
struct FixtureHistory {
    bars: Vec<MarketBar>,
}

#[cfg(test)]
enum FixtureRealtimeAction {
    Connected,
    Trade(CanonicalTrade),
    Heartbeat,
    Disconnect,
}

#[cfg(test)]
struct FixtureRealtime {
    actions: Receiver<FixtureRealtimeAction>,
    generations: SyncSender<ProviderGeneration>,
    stops: SyncSender<ProviderGeneration>,
}

#[cfg(test)]
struct FixtureRealtimeHarness {
    service: MarketService,
    actions: SyncSender<FixtureRealtimeAction>,
    generations: Receiver<ProviderGeneration>,
    stops: Receiver<ProviderGeneration>,
}

#[cfg(test)]
impl HistorySource for FixtureHistory {
    fn fetch(&mut self, _series: &BarSeriesKey) -> Result<HistorySnapshot, String> {
        Ok(HistorySnapshot {
            price_scale: 2,
            quantity_scale: 8,
            bars: self.bars.clone(),
        })
    }
}

#[cfg(test)]
impl RealtimeSource for FixtureRealtime {
    fn run_generation(
        &mut self,
        generation: ProviderGeneration,
        events: &SyncSender<RealtimeEvent>,
        overflow: &AtomicBool,
        stop: &Arc<AtomicBool>,
    ) {
        if self.generations.send(generation).is_err() {
            return;
        }
        while !stop.load(Ordering::Acquire) {
            let action = match self.actions.recv_timeout(Duration::from_millis(10)) {
                Ok(action) => action,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            let event = match action {
                FixtureRealtimeAction::Connected => {
                    if events.send(RealtimeEvent::Connected(generation)).is_err() {
                        return;
                    }
                    continue;
                }
                FixtureRealtimeAction::Trade(trade) => RealtimeEvent::Trade(generation, trade),
                FixtureRealtimeAction::Heartbeat => RealtimeEvent::Heartbeat(generation),
                FixtureRealtimeAction::Disconnect => return,
            };
            if !try_emit_realtime(events, overflow, event) {
                return;
            }
        }
        let _ = self.stops.send(generation);
    }
}

impl LiveCoinbaseHistory {
    fn try_new() -> Result<Self, String> {
        CoinbaseHistoryCapabilityAdapter::try_new()
            .map(|adapter| Self { adapter })
            .map_err(|error| error.to_string())
    }
}

impl HistorySource for LiveCoinbaseHistory {
    fn fetch(&mut self, series: &BarSeriesKey) -> Result<HistorySnapshot, String> {
        if series.provider_id != "coinbase"
            || series.instrument_id != "instrument:coinbase:btc:usd"
            || series.period != BarPeriod::time(60).map_err(|error| error.to_string())?
            || series.definition_version != 1
        {
            return Err(
                "the first engine migration slice supports BTC-USD one-minute bars only"
                    .to_string(),
            );
        }
        let now_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock is unavailable".to_string())?
            .as_secs();
        let end_seconds = now_seconds - now_seconds % 60;
        let end_unix_nanos = i64::try_from(end_seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000_000_000))
            .ok_or_else(|| "Coinbase history end time overflowed".to_string())?;
        let span_nanos = i64::try_from(HISTORY_BARS_PER_SERIES)
            .ok()
            .and_then(|count| count.checked_mul(60_000_000_000))
            .ok_or_else(|| "Coinbase history span overflowed".to_string())?;
        let request = HistoryPageRequest {
            provider_id: "coinbase".to_string(),
            account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
            entitlement_revision: ENTITLEMENT_CLASS.to_string(),
            instrument_id: series.instrument_id.clone(),
            data_class: DataClass::Bars,
            resolution: "1m".to_string(),
            range: HistoryRange {
                start_unix_nanos: end_unix_nanos.saturating_sub(span_nanos),
                end_unix_nanos,
            },
            maximum_items: NonZeroUsize::new(HISTORY_BARS_PER_SERIES).unwrap_or(NonZeroUsize::MIN),
            continuation: None,
        };
        let batch = self.adapter.fetch_paginated(&request)?;
        let bars = batch
            .items
            .iter()
            .map(decode_history_bar)
            .collect::<Result<Vec<_>, _>>()?;
        if bars.is_empty() {
            return Err("Coinbase returned no completed historical bars".to_string());
        }
        Ok(HistorySnapshot {
            price_scale: 2,
            quantity_scale: 8,
            bars,
        })
    }
}

impl LiveCoinbaseRealtime {
    fn try_new() -> Result<Self, String> {
        CoinbaseConfig::try_new(vec!["BTC-USD".to_string()])
            .map(|config| Self {
                config: config.with_level2(false),
            })
            .map_err(|error| error.to_string())
    }
}

impl RealtimeSource for LiveCoinbaseRealtime {
    fn run_generation(
        &mut self,
        generation: ProviderGeneration,
        events: &SyncSender<RealtimeEvent>,
        overflow: &AtomicBool,
        stop: &Arc<AtomicBool>,
    ) {
        let Ok(connection) =
            CoinbaseSession::new(self.config.clone()).connect_cancellable(Arc::clone(stop))
        else {
            return;
        };
        if events.send(RealtimeEvent::Connected(generation)).is_err() {
            return;
        }
        let queue_failed = Cell::new(false);
        let _ = connection.collect_until_stopped_with_market_events(
            &mut || queue_failed.get() || stop.load(Ordering::Acquire),
            &mut |trade| {
                if !try_emit_realtime(
                    events,
                    overflow,
                    RealtimeEvent::Trade(generation, trade.clone()),
                ) {
                    queue_failed.set(true);
                }
            },
            &mut || {
                if !try_emit_realtime(events, overflow, RealtimeEvent::Heartbeat(generation)) {
                    queue_failed.set(true);
                }
            },
            &mut |_| {},
        );
    }
}

impl MarketService {
    /// Starts the process-owned market coordinator and its single Coinbase history worker.
    ///
    /// # Errors
    /// Returns an error when provider configuration or either bounded worker cannot start.
    pub fn start() -> Result<Self, String> {
        Self::start_with_sources(
            LiveCoinbaseHistory::try_new()?,
            Some(Box::new(LiveCoinbaseRealtime::try_new()?)),
        )
    }

    #[cfg(test)]
    /// Starts a deterministic in-memory history source for IPC integration tests.
    ///
    /// # Errors
    /// Returns an error when either bounded worker cannot start.
    pub(crate) fn start_fixture(bars: Vec<MarketBar>) -> Result<Self, String> {
        Self::start_with_sources(FixtureHistory { bars }, None)
    }

    #[cfg(test)]
    fn start_fixture_realtime(bars: Vec<MarketBar>) -> Result<FixtureRealtimeHarness, String> {
        let (action_tx, action_rx) = mpsc::sync_channel(16);
        let (generation_tx, generation_rx) = mpsc::sync_channel(4);
        let (stop_tx, stop_rx) = mpsc::sync_channel(4);
        let service = Self::start_with_sources(
            FixtureHistory { bars },
            Some(Box::new(FixtureRealtime {
                actions: action_rx,
                generations: generation_tx,
                stops: stop_tx,
            })),
        )?;
        Ok(FixtureRealtimeHarness {
            service,
            actions: action_tx,
            generations: generation_rx,
            stops: stop_rx,
        })
    }

    #[cfg(test)]
    fn start_with_source(source: impl HistorySource) -> Result<Self, String> {
        Self::start_with_sources(source, None)
    }

    fn start_with_sources(
        source: impl HistorySource,
        realtime: Option<Box<dyn RealtimeSource>>,
    ) -> Result<Self, String> {
        let engine = configured_engine()?;
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (history_tx, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
        let (realtime_tx, realtime_rx) = mpsc::sync_channel(REALTIME_CAPACITY);
        let (realtime_control_tx, realtime_control_rx) = mpsc::sync_channel(1);
        let realtime_overflow = Arc::new(AtomicBool::new(false));
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let completion_tx = command_tx.clone();
        thread::Builder::new()
            .name("axiusflow-coinbase-history".to_string())
            .spawn(move || run_history_worker(source, &history_rx, &completion_tx))
            .map_err(|error| error.to_string())?;
        if let Some(realtime) = realtime {
            let overflow = Arc::clone(&realtime_overflow);
            let stop = Arc::clone(&realtime_stop);
            thread::Builder::new()
                .name("axiusflow-coinbase-realtime".to_string())
                .spawn(move || {
                    run_realtime_worker(
                        realtime,
                        &realtime_control_rx,
                        &realtime_tx,
                        &overflow,
                        &stop,
                    );
                })
                .map_err(|error| error.to_string())?;
        }
        thread::Builder::new()
            .name("axiusflow-market-engine".to_string())
            .spawn(move || {
                run_coordinator(
                    engine,
                    &command_rx,
                    &history_tx,
                    &realtime_control_tx,
                    &realtime_rx,
                    &realtime_overflow,
                    &realtime_stop,
                );
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            commands: command_tx,
        })
    }

    /// Attaches a client identity to resident market state.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn attach(&self, client_id: u64) -> Result<(), String> {
        self.request(|reply| Ok(Command::Attach(id(client_id).map(ClientId)?, reply)))
    }

    /// Detaches a client and all of its consumers.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn detach(&self, client_id: u64) -> Result<(), String> {
        self.request(|reply| Ok(Command::Detach(id(client_id).map(ClientId)?, reply)))
    }

    /// Registers one market consumer owned by an attached client.
    ///
    /// # Errors
    /// Returns an error for invalid identity, ownership, bounds, or coordinator failure.
    pub fn register_consumer(
        &self,
        client_id: u64,
        workspace_id: u64,
        consumer_id: u64,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Register(
                ConsumerIdentity {
                    client_id: ClientId(id(client_id)?),
                    workspace_id: WorkspaceId(id(workspace_id)?),
                    consumer_id: ConsumerId(id(consumer_id)?),
                },
                reply,
            ))
        })
    }

    /// Removes one market consumer.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn remove_consumer(&self, client_id: u64, consumer_id: u64) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Remove(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                reply,
            ))
        })
    }

    /// Applies a generation-fenced viewport to one consumer.
    ///
    /// # Errors
    /// Returns an error for invalid identity, range, generation, or coordinator failure.
    pub fn set_viewport(
        &self,
        client_id: u64,
        consumer_id: u64,
        generation: u64,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Viewport(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                GenerationId(id(generation)?),
                Viewport::try_new(start_unix_nanos, end_unix_nanos)
                    .map_err(|error| error.to_string())?,
                reply,
            ))
        })
    }

    /// Updates one consumer's presentation priority.
    ///
    /// # Errors
    /// Returns an error for invalid identity, missing consumer, or coordinator failure.
    pub fn set_visibility(
        &self,
        client_id: u64,
        consumer_id: u64,
        visible: bool,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Visibility(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                visible,
                reply,
            ))
        })
    }

    /// Resolves a covering snapshot for one generation-fenced series demand.
    ///
    /// # Errors
    /// Returns an error for invalid demand, unavailable coordinator, or failed reply delivery.
    pub fn set_demand(
        &self,
        client_id: u64,
        consumer_id: u64,
        generation: u64,
        series: &SeriesKey,
    ) -> Result<Vec<envelope::Payload>, String> {
        self.request(|reply| {
            Ok(Command::Demand(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                GenerationId(id(generation)?),
                internal_series(series)?,
                reply,
            ))
        })
    }

    /// Drains at most one bounded market publication for an owned consumer.
    ///
    /// # Errors
    /// Returns an error for invalid ownership or coordinator failure.
    pub fn poll_event(
        &self,
        client_id: u64,
        consumer_id: u64,
    ) -> Result<Option<envelope::Payload>, String> {
        self.request(|reply| {
            Ok(Command::Poll(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                reply,
            ))
        })
    }

    fn request<T>(
        &self,
        build: impl FnOnce(Reply<T>) -> Result<Command, String>,
    ) -> Result<T, String> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let command = build(reply_tx)?;
        self.commands
            .send(command)
            .map_err(|_| "market engine coordinator is unavailable".to_string())?;
        reply_rx
            .recv()
            .map_err(|_| "market engine coordinator stopped before replying".to_string())?
    }
}

fn run_history_worker(
    mut source: impl HistorySource,
    requests: &Receiver<HistoryRequest>,
    completions: &SyncSender<Command>,
) {
    while let Ok(request) = requests.recv() {
        let result = source.fetch(&request.series);
        if completions
            .send(Command::HistoryCompleted(
                request.series,
                request.provider_generation,
                result,
            ))
            .is_err()
        {
            return;
        }
    }
}

fn run_realtime_worker(
    mut source: Box<dyn RealtimeSource>,
    control: &Receiver<RealtimeControl>,
    events: &SyncSender<RealtimeEvent>,
    overflow: &AtomicBool,
    stop: &Arc<AtomicBool>,
) {
    let mut generation = ProviderGeneration(
        NonZeroU64::new(COINBASE_PROVIDER_GENERATION).unwrap_or(NonZeroU64::MIN),
    );
    loop {
        if !matches!(control.recv(), Ok(RealtimeControl::Start)) {
            return;
        }
        loop {
            if stop.load(Ordering::Acquire) {
                break;
            }
            if events.send(RealtimeEvent::Connecting(generation)).is_err() {
                return;
            }
            source.run_generation(generation, events, overflow, stop);
            let stopped = stop.load(Ordering::Acquire);
            if events
                .send(RealtimeEvent::Disconnected(generation))
                .is_err()
            {
                return;
            }
            let Some(next) = generation.0.get().checked_add(1).and_then(NonZeroU64::new) else {
                return;
            };
            generation = ProviderGeneration(next);
            if stopped {
                break;
            }
            thread::park_timeout(RECONNECT_DELAY);
        }
    }
}

fn try_emit_realtime(
    events: &SyncSender<RealtimeEvent>,
    overflow: &AtomicBool,
    event: RealtimeEvent,
) -> bool {
    match events.try_send(event) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => {
            overflow.store(true, Ordering::Release);
            false
        }
        Err(TrySendError::Disconnected(_)) => false,
    }
}

fn run_coordinator(
    engine: MarketEngine,
    commands: &Receiver<Command>,
    history: &SyncSender<HistoryRequest>,
    realtime_control: &SyncSender<RealtimeControl>,
    realtime: &Receiver<RealtimeEvent>,
    realtime_overflow: &AtomicBool,
    realtime_stop: &Arc<AtomicBool>,
) {
    let mut coordinator = Coordinator {
        engine,
        history,
        realtime_control,
        realtime_stop,
        attached: BTreeSet::new(),
        pending: BTreeMap::new(),
        history_inflight: BTreeSet::new(),
        events: BTreeMap::new(),
        live: None,
        realtime_started: false,
    };
    loop {
        for _ in 0..REALTIME_DRAIN_BUDGET {
            match realtime.try_recv() {
                Ok(event) => coordinator.handle_realtime(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if realtime_overflow.swap(false, Ordering::AcqRel) {
            coordinator.realtime_interrupted("Coinbase realtime queue overflowed");
        }
        coordinator.publish_live();
        match commands.recv_timeout(COORDINATOR_TICK) {
            Ok(command) => coordinator.handle_command(command),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

struct Coordinator<'a> {
    engine: MarketEngine,
    history: &'a SyncSender<HistoryRequest>,
    realtime_control: &'a SyncSender<RealtimeControl>,
    realtime_stop: &'a Arc<AtomicBool>,
    attached: BTreeSet<ClientId>,
    pending: BTreeMap<BarSeriesKey, Vec<DemandWaiter>>,
    history_inflight: BTreeSet<(BarSeriesKey, ProviderGeneration)>,
    events: BTreeMap<ConsumerId, ConsumerEvents>,
    live: Option<LiveHandoff>,
    realtime_started: bool,
}

impl Coordinator<'_> {
    fn handle_command(&mut self, command: Command) {
        match command {
            Command::Attach(client_id, reply) => {
                let result = self
                    .attached
                    .insert(client_id)
                    .then_some(())
                    .ok_or_else(|| "client identity is already attached".to_string());
                let _ = reply.send(result);
            }
            Command::Detach(client_id, reply) => {
                self.attached.remove(&client_id);
                self.remove_client_events(client_id);
                self.engine.detach_client(client_id);
                self.stop_realtime_if_idle();
                let _ = reply.send(Ok(()));
            }
            Command::Register(identity, reply) => {
                let result = if self.attached.contains(&identity.client_id) {
                    self.engine
                        .register_consumer(identity, true)
                        .map_err(|error| error.to_string())
                } else {
                    Err("client must attach before registering consumers".to_string())
                };
                if result.is_ok() {
                    self.events
                        .insert(identity.consumer_id, ConsumerEvents::default());
                }
                let _ = reply.send(result);
            }
            Command::Remove(client_id, consumer_id, reply) => {
                let result = authorize_consumer(&self.engine, client_id, consumer_id).map(|()| {
                    self.events.remove(&consumer_id);
                    self.remove_waiter(consumer_id);
                    self.engine.remove_consumer(consumer_id);
                    self.stop_realtime_if_idle();
                });
                let _ = reply.send(result);
            }
            Command::Viewport(client_id, consumer_id, generation, viewport, reply) => {
                let result =
                    authorize_consumer(&self.engine, client_id, consumer_id).and_then(|()| {
                        match self.engine.set_viewport(consumer_id, generation, viewport) {
                            Ok(()) | Err(EngineError::StaleConsumerGeneration { .. }) => Ok(()),
                            Err(error) => Err(error.to_string()),
                        }
                    });
                let _ = reply.send(result);
            }
            Command::Visibility(client_id, consumer_id, visible, reply) => {
                let result =
                    authorize_consumer(&self.engine, client_id, consumer_id).and_then(|()| {
                        self.engine
                            .set_visibility(consumer_id, visible)
                            .map_err(|error| error.to_string())
                    });
                let _ = reply.send(result);
            }
            Command::Demand(client_id, consumer_id, generation, series, reply) => {
                self.handle_demand(
                    client_id,
                    &series,
                    DemandWaiter {
                        consumer_id,
                        generation,
                        reply,
                    },
                );
            }
            Command::Poll(client_id, consumer_id, reply) => {
                let result = authorize_consumer(&self.engine, client_id, consumer_id).map(|()| {
                    self.events
                        .get_mut(&consumer_id)
                        .and_then(ConsumerEvents::pop)
                });
                let _ = reply.send(result);
            }
            Command::HistoryCompleted(series, generation, result) => {
                self.history_completed(&series, generation, result);
            }
        }
    }

    fn handle_demand(&mut self, client_id: ClientId, series: &BarSeriesKey, waiter: DemandWaiter) {
        let demand =
            authorize_consumer(&self.engine, client_id, waiter.consumer_id).and_then(|()| {
                self.engine
                    .set_series_demand(waiter.consumer_id, waiter.generation, series)
                    .map_err(|error| error.to_string())
            });
        if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
            events.snapshot = None;
            events.series_state = None;
        }
        if let Err(error) = self.ensure_realtime(series) {
            let _ = waiter.reply.send(Err(error));
            return;
        }
        match demand {
            Ok(Some(publication)) => {
                let _ = waiter.reply.send(Ok(ready_messages(&publication)));
            }
            Ok(None) => {
                let first = !self.pending.contains_key(series);
                self.pending.entry(series.clone()).or_default().push(waiter);
                if first {
                    let generation = self.current_provider_generation();
                    if let Err(detail) = self.enqueue_history(series, generation)
                        && let Some(waiters) = self.pending.remove(series)
                    {
                        fail_waiters(waiters, detail);
                    }
                }
            }
            Err(error) => {
                let _ = waiter.reply.send(Err(error));
            }
        }
    }

    fn ensure_realtime(&mut self, series: &BarSeriesKey) -> Result<(), String> {
        match &self.live {
            Some(live) if live.series != *series => {
                return Err("the current engine realtime slice supports one series".to_string());
            }
            Some(_) => {}
            None => {
                self.live = Some(LiveHandoff::try_new(
                    series.clone(),
                    self.current_provider_generation(),
                )?);
            }
        }
        if !self.realtime_started {
            self.realtime_stop.store(false, Ordering::Release);
            match self.realtime_control.try_send(RealtimeControl::Start) {
                Ok(()) | Err(TrySendError::Full(_)) => self.realtime_started = true,
                Err(TrySendError::Disconnected(_)) => {
                    self.realtime_stop.store(true, Ordering::Release);
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn enqueue_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) -> Result<(), &'static str> {
        let key = (series.clone(), generation);
        if self.history_inflight.contains(&key) {
            return Ok(());
        }
        let request = HistoryRequest {
            series: series.clone(),
            provider_generation: generation,
        };
        match try_enqueue_history(self.history, request) {
            Ok(()) => {
                self.history_inflight.insert(key);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn history_completed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        result: Result<HistorySnapshot, String>,
    ) {
        self.history_inflight.remove(&(series.clone(), generation));
        if generation != self.current_provider_generation() {
            let current = self.current_provider_generation();
            let _ = self.enqueue_history(series, current);
            return;
        }
        let Ok(snapshot) = result else {
            if let Some(waiters) = self.pending.remove(series) {
                fail_waiters(waiters, "Coinbase historical bars are unavailable");
            }
            self.broadcast_provider(
                ProviderConnectionState::Recovering,
                generation,
                Some("Coinbase history repair is retrying"),
            );
            return;
        };
        let bars = snapshot.bars;
        let waiting = self
            .pending
            .get(series)
            .map(|waiters| {
                waiters
                    .iter()
                    .map(|waiter| waiter.consumer_id)
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let installed = self.engine.install_history(
            generation,
            series,
            snapshot.price_scale,
            snapshot.quantity_scale,
            bars.clone(),
        );
        let publications = match installed {
            Ok(publications) => publications,
            Err(error) => {
                if let Some(waiters) = self.pending.remove(series) {
                    fail_waiters(waiters, &error.to_string());
                }
                return;
            }
        };
        for publication in publications {
            if !waiting.contains(&publication.consumer_id)
                && let Some(events) = self.events.get_mut(&publication.consumer_id)
            {
                events.snapshot = Some(snapshot_message(&publication));
            }
        }
        if let Some(live) = self.live.as_mut().filter(|live| live.series == *series) {
            let connected = live.connected;
            let buffered = std::mem::take(&mut live.buffered);
            live.aggregator = match coinbase_aggregator() {
                Ok(aggregator) => aggregator,
                Err(error) => {
                    self.realtime_interrupted(&error);
                    return;
                }
            };
            if live.aggregator.seed_canonical_history(&bars).is_err()
                || buffered
                    .iter()
                    .any(|trade| live.aggregator.apply_trade(trade).is_err())
            {
                self.realtime_interrupted("Coinbase history/live handoff failed");
                return;
            }
            live.connected = connected;
            live.history_ready = true;
            live.dirty = live.aggregator.in_flight().is_some();
        }
        if let Some(waiters) = self.pending.remove(series) {
            complete_waiters(&self.engine, waiters);
        }
        if self.live.as_ref().is_some_and(|live| live.connected) {
            self.provider_online();
        }
    }

    fn handle_realtime(&mut self, event: RealtimeEvent) {
        match event {
            RealtimeEvent::Connecting(generation) => self.realtime_connecting(generation),
            RealtimeEvent::Connected(generation) => self.realtime_connected(generation),
            RealtimeEvent::Trade(generation, trade) => self.realtime_trade(generation, trade),
            RealtimeEvent::Heartbeat(generation) => self.realtime_heartbeat(generation),
            RealtimeEvent::Disconnected(generation) => {
                if generation == self.current_provider_generation() {
                    self.realtime_interrupted("Coinbase realtime disconnected");
                }
            }
        }
    }

    fn realtime_connecting(&mut self, generation: ProviderGeneration) {
        let current = self.current_provider_generation();
        if generation < current {
            return;
        }
        if generation > current {
            if self
                .engine
                .begin_provider_session("coinbase", generation)
                .is_err()
            {
                return;
            }
            if let Some(live) = self.live.as_mut()
                && live.reset(generation).is_err()
            {
                return;
            }
        } else {
            let _ =
                self.engine
                    .set_provider_health("coinbase", generation, ProviderHealth::Connecting);
        }
        let state = if generation.0.get() == COINBASE_PROVIDER_GENERATION {
            ProviderConnectionState::Connecting
        } else {
            ProviderConnectionState::Recovering
        };
        self.broadcast_provider(state, generation, None);
    }

    fn realtime_connected(&mut self, generation: ProviderGeneration) {
        if generation != self.current_provider_generation() {
            return;
        }
        let Some(live) = self.live.as_mut() else {
            return;
        };
        live.connected = true;
        let series = live.series.clone();
        if live.history_ready {
            self.provider_online();
        } else {
            let _ = self.enqueue_history(&series, generation);
        }
    }

    fn realtime_trade(&mut self, generation: ProviderGeneration, trade: CanonicalTrade) {
        let Some(live) = self
            .live
            .as_mut()
            .filter(|live| live.generation == generation && live.connected)
        else {
            return;
        };
        if live.history_ready {
            if live.aggregator.apply_trade(&trade).is_err() {
                self.realtime_interrupted("Coinbase realtime aggregation failed");
            } else {
                live.dirty = true;
            }
        } else if live.buffered.len() == LIVE_BUFFER_CAPACITY {
            self.realtime_interrupted("Coinbase history/live buffer overflowed");
        } else {
            live.buffered.push_back(trade);
        }
    }

    fn realtime_heartbeat(&mut self, generation: ProviderGeneration) {
        let Some(live) = self
            .live
            .as_ref()
            .filter(|live| live.generation == generation && live.connected && !live.history_ready)
        else {
            return;
        };
        let series = live.series.clone();
        let _ = self.enqueue_history(&series, generation);
    }

    fn realtime_interrupted(&mut self, detail: &str) {
        let generation = self.current_provider_generation();
        let _ = self
            .engine
            .set_provider_health("coinbase", generation, ProviderHealth::Recovering);
        if let Some(live) = self.live.as_mut() {
            live.connected = false;
            live.history_ready = false;
            live.dirty = false;
            live.buffered.clear();
        }
        self.broadcast_provider(
            ProviderConnectionState::Recovering,
            generation,
            Some(detail),
        );
    }

    fn provider_online(&mut self) {
        let generation = self.current_provider_generation();
        let _ = self
            .engine
            .set_provider_health("coinbase", generation, ProviderHealth::Online);
        self.broadcast_provider(ProviderConnectionState::Online, generation, None);
        self.broadcast_series_state(SeriesLoadState::Live);
    }

    fn publish_live(&mut self) {
        let Some(live) = self
            .live
            .as_mut()
            .filter(|live| live.connected && live.history_ready && live.dirty)
        else {
            return;
        };
        let Some(active) = live.aggregator.in_flight() else {
            live.dirty = false;
            return;
        };
        let mut bars = live.aggregator.history();
        bars.push(active);
        let series = live.series.clone();
        let generation = live.generation;
        live.dirty = false;
        match self
            .engine
            .install_realtime(generation, &series, 2, 8, bars, true)
        {
            Ok(publications) => {
                for publication in publications {
                    if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                        events.snapshot = Some(snapshot_message(&publication));
                    }
                }
            }
            Err(_) => self.realtime_interrupted("Coinbase live publication failed"),
        }
    }

    fn broadcast_provider(
        &mut self,
        state: ProviderConnectionState,
        generation: ProviderGeneration,
        detail: Option<&str>,
    ) {
        let payload = envelope::Payload::ProviderState(ProviderState {
            provider: "coinbase".to_string(),
            state: state as i32,
            generation: generation.0.get(),
            detail: detail.map(str::to_string),
        });
        for events in self.events.values_mut() {
            events.provider = Some(payload.clone());
        }
    }

    fn broadcast_series_state(&mut self, state: SeriesLoadState) {
        for (consumer_id, events) in &mut self.events {
            let Some(demand) = self.engine.current_demand(*consumer_id) else {
                continue;
            };
            let (Some(generation), Some(series)) = (demand.generation, demand.series.as_ref())
            else {
                continue;
            };
            events.series_state = Some(series_state(
                *consumer_id,
                generation,
                ipc_series(series),
                state,
                None,
            ));
        }
    }

    fn current_provider_generation(&self) -> ProviderGeneration {
        self.engine
            .provider_status("coinbase")
            .and_then(|status| status.generation)
            .unwrap_or(ProviderGeneration(NonZeroU64::MIN))
    }

    fn remove_client_events(&mut self, client_id: ClientId) {
        let removed = self
            .events
            .keys()
            .copied()
            .filter(|consumer_id| {
                self.engine
                    .current_demand(*consumer_id)
                    .is_some_and(|demand| demand.identity.client_id == client_id)
            })
            .collect::<Vec<_>>();
        for consumer_id in removed {
            self.events.remove(&consumer_id);
            self.remove_waiter(consumer_id);
        }
    }

    fn remove_waiter(&mut self, consumer_id: ConsumerId) {
        for waiters in self.pending.values_mut() {
            waiters.retain(|waiter| waiter.consumer_id != consumer_id);
        }
        self.pending.retain(|_, waiters| !waiters.is_empty());
    }

    fn stop_realtime_if_idle(&mut self) {
        if !self.events.is_empty() || !self.realtime_started {
            return;
        }
        self.realtime_stop.store(true, Ordering::Release);
        self.realtime_started = false;
        if let Some(live) = self.live.as_mut() {
            live.connected = false;
            live.history_ready = false;
            live.dirty = false;
            live.buffered.clear();
        }
    }
}

fn try_enqueue_history(
    history: &SyncSender<HistoryRequest>,
    request: HistoryRequest,
) -> Result<(), &'static str> {
    match history.try_send(request) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => Err("Coinbase history capacity is temporarily exhausted"),
        Err(TrySendError::Disconnected(_)) => Err("Coinbase history worker is unavailable"),
    }
}

fn authorize_consumer(
    engine: &MarketEngine,
    client_id: ClientId,
    consumer_id: ConsumerId,
) -> Result<(), String> {
    engine
        .current_demand(consumer_id)
        .filter(|demand| demand.identity.client_id == client_id)
        .map(|_| ())
        .ok_or_else(|| "consumer is not owned by the attached client".to_string())
}

fn configured_engine() -> Result<MarketEngine, String> {
    let mut engine = MarketEngine::new(MarketEngineConfig {
        maximum_consumers: NonZeroUsize::new(MAXIMUM_CONSUMERS).unwrap_or(NonZeroUsize::MIN),
        maximum_series: NonZeroUsize::new(MAXIMUM_SERIES).unwrap_or(NonZeroUsize::MIN),
        maximum_bars: NonZeroUsize::new(MAXIMUM_STORED_BARS).unwrap_or(NonZeroUsize::MIN),
    });
    engine
        .register_provider(
            "coinbase".to_string(),
            ProviderCapabilities {
                historical_bars: true,
                realtime_bars: true,
            },
        )
        .map_err(|error| error.to_string())?;
    engine
        .begin_provider_session(
            "coinbase",
            ProviderGeneration(
                NonZeroU64::new(COINBASE_PROVIDER_GENERATION).unwrap_or(NonZeroU64::MIN),
            ),
        )
        .map_err(|error| error.to_string())?;
    Ok(engine)
}

fn complete_waiters(engine: &MarketEngine, waiters: Vec<DemandWaiter>) {
    for waiter in waiters {
        let response = engine
            .latest_publication(waiter.consumer_id)
            .filter(|publication| publication.generation == waiter.generation)
            .map_or_else(
                || Ok(superseded_messages(waiter.consumer_id, waiter.generation)),
                |publication| Ok(ready_messages(publication)),
            );
        let _ = waiter.reply.send(response);
    }
}

fn fail_waiters(waiters: Vec<DemandWaiter>, detail: &str) {
    for waiter in waiters {
        let _ = waiter.reply.send(Ok(failed_messages(
            waiter.consumer_id,
            waiter.generation,
            detail,
        )));
    }
}

fn ready_messages(
    publication: &axiusflow_market_engine::ConsumerPublication,
) -> Vec<envelope::Payload> {
    let series = ipc_series(&publication.snapshot.series);
    vec![
        series_state(
            publication.consumer_id,
            publication.generation,
            series.clone(),
            SeriesLoadState::Resolving,
            None,
        ),
        snapshot_message(publication),
        series_state(
            publication.consumer_id,
            publication.generation,
            series,
            SeriesLoadState::Ready,
            None,
        ),
    ]
}

fn snapshot_message(
    publication: &axiusflow_market_engine::ConsumerPublication,
) -> envelope::Payload {
    envelope::Payload::SeriesSnapshot(IpcSeriesSnapshot {
        consumer_id: publication.consumer_id.0.get(),
        generation: publication.generation.0.get(),
        series: Some(ipc_series(&publication.snapshot.series)),
        provider_generation: publication.snapshot.provider_generation.0.get(),
        price_scale: u32::from(publication.snapshot.price_scale),
        quantity_scale: u32::from(publication.snapshot.quantity_scale),
        bars: publication
            .snapshot
            .bars
            .iter()
            .copied()
            .map(ipc_bar)
            .collect(),
        publication_generation: publication.publication_generation,
        forming: publication.snapshot.forming,
    })
}

fn superseded_messages(
    consumer_id: ConsumerId,
    generation: GenerationId,
) -> Vec<envelope::Payload> {
    vec![series_state(
        consumer_id,
        generation,
        SeriesKey::default(),
        SeriesLoadState::Superseded,
        Some("a newer consumer generation replaced this demand".to_string()),
    )]
}

fn failed_messages(
    consumer_id: ConsumerId,
    generation: GenerationId,
    detail: &str,
) -> Vec<envelope::Payload> {
    vec![
        series_state(
            consumer_id,
            generation,
            SeriesKey::default(),
            SeriesLoadState::Failed,
            Some(detail.to_string()),
        ),
        envelope::Payload::DemandError(DemandError {
            consumer_id: consumer_id.0.get(),
            generation: generation.0.get(),
            code: EngineFaultCode::Retryable as i32,
            stage: "provider_history".to_string(),
            detail: detail.to_string(),
        }),
    ]
}

fn series_state(
    consumer_id: ConsumerId,
    generation: GenerationId,
    series: SeriesKey,
    state: SeriesLoadState,
    detail: Option<String>,
) -> envelope::Payload {
    envelope::Payload::SeriesState(SeriesState {
        consumer_id: consumer_id.0.get(),
        generation: generation.0.get(),
        series: Some(series),
        state: state as i32,
        persistence: PersistenceState::NotRequested as i32,
        detail,
    })
}

fn id(value: u64) -> Result<NonZeroU64, String> {
    NonZeroU64::new(value).ok_or_else(|| "market identity must be non-zero".to_string())
}

fn coinbase_aggregator() -> Result<CoinbaseBarAggregator, String> {
    CoinbaseBarAggregatorConfig::try_new(
        "BTC-USD",
        2,
        8,
        NonZeroUsize::new(HISTORY_BARS_PER_SERIES).unwrap_or(NonZeroUsize::MIN),
    )
    .map(CoinbaseBarAggregator::new)
    .map_err(|error| error.to_string())
}

fn internal_series(series: &SeriesKey) -> Result<BarSeriesKey, String> {
    if series.provider.trim().is_empty()
        || series.instrument_id.trim().is_empty()
        || series.definition_revision == 0
    {
        return Err("market series identity is invalid".to_string());
    }
    Ok(BarSeriesKey {
        provider_id: series.provider.clone(),
        instrument_id: series.instrument_id.clone(),
        entitlement_id: ENTITLEMENT_CLASS.to_string(),
        period: BarPeriod::time(series.interval_seconds).map_err(|error| error.to_string())?,
        definition_version: series.definition_revision,
    })
}

fn ipc_series(series: &BarSeriesKey) -> SeriesKey {
    SeriesKey {
        provider: series.provider_id.clone(),
        instrument_id: series.instrument_id.clone(),
        interval_seconds: match series.period {
            BarPeriod::Time { seconds } => seconds,
            BarPeriod::Tick { .. } | BarPeriod::Daily => 0,
        },
        definition_revision: series.definition_version,
    }
}

const fn ipc_bar(bar: MarketBar) -> IpcMarketBar {
    IpcMarketBar {
        source_sequence: bar.source_sequence,
        exchange_timestamp_seconds: bar.exchange_timestamp_seconds,
        open: bar.open,
        high: bar.high,
        low: bar.low,
        close: bar.close,
        volume: bar.volume,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_coinbase_market_adapter::FixedPointValue;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::Instant;

    struct ControlledHistory {
        fetches: Arc<AtomicUsize>,
        release: Receiver<()>,
    }

    impl HistorySource for ControlledHistory {
        fn fetch(&mut self, _series: &BarSeriesKey) -> Result<HistorySnapshot, String> {
            self.fetches.fetch_add(1, Ordering::AcqRel);
            self.release
                .recv()
                .map_err(|_| "test history release disconnected".to_string())?;
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 8,
                bars: vec![MarketBar {
                    source_sequence: 1,
                    exchange_timestamp_seconds: 60,
                    open: 100,
                    high: 110,
                    low: 90,
                    close: 105,
                    volume: 7,
                }],
            })
        }
    }

    fn btc() -> SeriesKey {
        SeriesKey {
            provider: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            interval_seconds: 60,
            definition_revision: 1,
        }
    }

    fn history_bar() -> MarketBar {
        MarketBar {
            source_sequence: 2,
            exchange_timestamp_seconds: 60,
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 7,
        }
    }

    fn trade(minute: i64, price: &str, provider_sequence: u64) -> CanonicalTrade {
        CanonicalTrade {
            product_id: "BTC-USD".to_string(),
            trade_id: format!("fixture-{provider_sequence}"),
            price: FixedPointValue::parse(price).expect("price parses"),
            size: FixedPointValue::parse("0.00000001").expect("size parses"),
            maker_side_buy: true,
            trade_time_unix_nanos: minute * 60_000_000_000 + 1,
            provider_timestamp_unix_nanos: minute * 60_000_000_000 + 2,
            sequence_num: provider_sequence,
            canonical_sequence: provider_sequence,
        }
    }

    fn poll_until(
        service: &MarketService,
        client_id: u64,
        consumer_id: u64,
        mut accept: impl FnMut(&envelope::Payload) -> bool,
    ) -> envelope::Payload {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(event) = service
                .poll_event(client_id, consumer_id)
                .expect("market poll succeeds")
                && accept(&event)
            {
                return event;
            }
            assert!(Instant::now() < deadline, "market event timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn attach_fixture_consumers(harness: &FixtureRealtimeHarness) {
        for client in 1..=2 {
            harness.service.attach(client).expect("client attaches");
            harness
                .service
                .register_consumer(client, 1, client)
                .expect("consumer registers");
            let messages = harness
                .service
                .set_demand(client, client, 1, &btc())
                .expect("history demand resolves");
            assert!(messages.iter().any(|message| {
                matches!(message, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.provider_generation == 1 && !snapshot.forming)
            }));
        }
    }

    #[test]
    fn later_consumers_reuse_one_engine_history_fetch() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let service = MarketService::start_with_source(ControlledHistory {
            fetches: Arc::clone(&fetches),
            release: release_rx,
        })
        .expect("test service starts");
        for client in 1..=2 {
            service.attach(client).expect("client attaches");
            service
                .register_consumer(client, 1, client)
                .expect("consumer registers");
        }
        assert_eq!(
            service.attach(1).expect_err("duplicate client is rejected"),
            "client identity is already attached"
        );
        assert_eq!(
            service
                .set_visibility(1, 2, false)
                .expect_err("another client's consumer is rejected"),
            "consumer is not owned by the attached client"
        );
        let first = service.clone();
        let first_request = thread::spawn(move || first.set_demand(1, 1, 1, &btc()));
        while fetches.load(Ordering::Acquire) == 0 {
            thread::yield_now();
        }
        release_tx.send(()).expect("history released");
        let first_messages = first_request
            .join()
            .expect("request thread joins")
            .expect("first demand succeeds");
        let second_messages = service
            .set_demand(2, 2, 1, &btc())
            .expect("cache hit succeeds");
        for messages in [first_messages, second_messages] {
            assert!(messages.iter().any(|message| {
                matches!(message, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.bars.len() == 1)
            }));
        }
        service
            .set_demand(1, 1, 2, &btc())
            .expect("newer selection reuses cache");
        service
            .set_viewport(1, 1, 1, 60, 120)
            .expect("retired viewport is a fenced no-op");
        assert_eq!(fetches.load(Ordering::Acquire), 1);
    }

    #[test]
    fn full_history_queue_fails_waiters_without_blocking_the_coordinator() {
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        history_tx
            .try_send(HistoryRequest {
                series: internal_series(&btc()).expect("first series"),
                provider_generation: ProviderGeneration(id(1).expect("provider generation")),
            })
            .expect("fill history queue");
        let mut second = internal_series(&btc()).expect("second series");
        second.instrument_id = "instrument:coinbase:eth:usd".to_string();
        assert_eq!(
            try_enqueue_history(
                &history_tx,
                HistoryRequest {
                    series: second,
                    provider_generation: ProviderGeneration(id(1).expect("provider generation")),
                },
            ),
            Err("Coinbase history capacity is temporarily exhausted")
        );
    }

    #[test]
    fn realtime_handoff_recovers_without_reconstructing_consumers() {
        let harness = MarketService::start_fixture_realtime(vec![history_bar()])
            .expect("realtime fixture starts");
        attach_fixture_consumers(&harness);
        assert_eq!(
            harness
                .generations
                .recv_timeout(Duration::from_secs(1))
                .expect("first realtime generation")
                .0
                .get(),
            1
        );
        harness
            .actions
            .send(FixtureRealtimeAction::Connected)
            .expect("connect fixture");
        harness
            .actions
            .send(FixtureRealtimeAction::Trade(trade(2, "2.00", 1)))
            .expect("first live trade");
        let first_live = poll_until(
            &harness.service,
            1,
            1,
            |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.provider_generation == 1 && snapshot.forming),
        );
        assert!(matches!(
            first_live,
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.bars.last().is_some_and(|bar| bar.source_sequence == 3 && bar.close == 200)
        ));

        harness
            .actions
            .send(FixtureRealtimeAction::Disconnect)
            .expect("disconnect fixture");
        let recovering = poll_until(
            &harness.service,
            1,
            1,
            |event| matches!(event, envelope::Payload::ProviderState(state) if state.state == ProviderConnectionState::Recovering as i32),
        );
        assert!(matches!(
            recovering,
            envelope::Payload::ProviderState(state) if state.generation == 1
        ));
        assert_eq!(
            harness
                .generations
                .recv_timeout(Duration::from_secs(1))
                .expect("second realtime generation")
                .0
                .get(),
            2
        );
        harness
            .actions
            .send(FixtureRealtimeAction::Connected)
            .expect("reconnect fixture");
        harness
            .actions
            .send(FixtureRealtimeAction::Trade(trade(2, "2.10", 2)))
            .expect("resumed live trade");
        harness
            .actions
            .send(FixtureRealtimeAction::Heartbeat)
            .expect("recovery heartbeat");
        for (client, consumer) in [(1, 1), (2, 2)] {
            let resumed = poll_until(
                &harness.service,
                client,
                consumer,
                |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.provider_generation == 2 && snapshot.forming),
            );
            assert!(matches!(
                resumed,
                envelope::Payload::SeriesSnapshot(snapshot)
                    if snapshot.generation == 1
                        && snapshot.bars.last().is_some_and(|bar| bar.close == 210)
            ));
        }
        harness.service.detach(1).expect("first client detaches");
        assert!(
            harness
                .stops
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "shared realtime remains for the second consumer"
        );
        harness.service.detach(2).expect("second client detaches");
        assert_eq!(
            harness
                .stops
                .recv_timeout(Duration::from_secs(1))
                .expect("last consumer stops realtime")
                .0
                .get(),
            2
        );
    }

    #[test]
    fn realtime_queue_overflow_closes_and_restarts_the_provider_generation() {
        let (action_tx, action_rx) = mpsc::sync_channel(4);
        let (generation_tx, generation_rx) = mpsc::sync_channel(4);
        let (stop_tx, _stop_rx) = mpsc::sync_channel(4);
        let (control_tx, control_rx) = mpsc::sync_channel(1);
        let (event_tx, event_rx) = mpsc::sync_channel(1);
        let overflow = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_overflow = Arc::clone(&overflow);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            run_realtime_worker(
                Box::new(FixtureRealtime {
                    actions: action_rx,
                    generations: generation_tx,
                    stops: stop_tx,
                }),
                &control_rx,
                &event_tx,
                &worker_overflow,
                &worker_stop,
            );
        });

        control_tx
            .send(RealtimeControl::Start)
            .expect("start realtime worker");
        assert!(matches!(
            event_rx.recv_timeout(Duration::from_secs(1)),
            Ok(RealtimeEvent::Connecting(generation)) if generation.0.get() == 1
        ));
        assert_eq!(
            generation_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("first fixture generation")
                .0
                .get(),
            1
        );
        action_tx
            .send(FixtureRealtimeAction::Connected)
            .expect("queue connected state");
        action_tx
            .send(FixtureRealtimeAction::Trade(trade(2, "2.00", 1)))
            .expect("overflow realtime queue");
        let overflow_deadline = Instant::now() + Duration::from_secs(1);
        while !overflow.load(Ordering::Acquire) {
            assert!(
                Instant::now() < overflow_deadline,
                "realtime overflow timed out"
            );
            thread::yield_now();
        }
        assert!(matches!(
            event_rx.recv_timeout(Duration::from_secs(1)),
            Ok(RealtimeEvent::Connected(generation)) if generation.0.get() == 1
        ));
        assert!(matches!(
            event_rx.recv_timeout(Duration::from_secs(1)),
            Ok(RealtimeEvent::Disconnected(generation)) if generation.0.get() == 1
        ));
        assert!(matches!(
            event_rx.recv_timeout(Duration::from_secs(1)),
            Ok(RealtimeEvent::Connecting(generation)) if generation.0.get() == 2
        ));

        stop.store(true, Ordering::Release);
        drop(action_tx);
        drop(control_tx);
        while event_rx.recv_timeout(Duration::from_millis(50)).is_ok() {}
        worker.join().expect("realtime worker exits");
    }
}
