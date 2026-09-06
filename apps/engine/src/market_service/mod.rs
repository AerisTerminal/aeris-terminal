//! Single-owner resident market coordinator and provider history/realtime workers.

use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axiusflow_coinbase_market_adapter::{
    COINBASE_PUBLIC_ACCOUNT_ID, CanonicalTrade, CoinbaseBarAggregator, CoinbaseBarAggregatorConfig,
    CoinbaseConfig, CoinbaseHistoryCapabilityAdapter, CoinbaseInterval, CoinbaseLevel2Book,
    CoinbaseLevel2Outcome, CoinbaseSession, CoinbaseSpotProduct, ENTITLEMENT_CLASS,
    aggregate_coinbase_bars, coinbase_instrument_id, decode_history_bar,
};
use axiusflow_engine_protocol::{
    DemandError, EngineFaultCode, FailureStage, HotSeries, InstallProviderInstrument,
    MarketBar as IpcMarketBar, OrderBookLevel as IpcOrderBookLevel,
    OrderBookSnapshot as IpcOrderBookSnapshot, OrderBookState as IpcOrderBookState,
    OrderFlowAggressor, OrderFlowLevel as IpcOrderFlowLevel,
    OrderFlowSnapshot as IpcOrderFlowSnapshot, OrderFlowTrade as IpcOrderFlowTrade,
    OrderFlowUpdate as IpcOrderFlowUpdate, PersistenceState, ProviderCatalogRejected,
    ProviderCatalogRejectionReason, ProviderConnectionState, ProviderInstrumentSelection,
    ProviderState, ResourceMode, SearchProviderInstruments, SelectProviderInstrument,
    SeriesCadence, SeriesKey, SeriesLoadState, SeriesSnapshot as IpcSeriesSnapshot, SeriesState,
    SeriesUpdateOperation, WorkspaceState, envelope,
};
use axiusflow_local_history::{
    HistoryScope, LocalHistoryError, LocalHistoryStore, RetainedRange, StoredHistory,
};
use axiusflow_market_data::{
    BarPeriod, BarSeriesKey, DepthLevel, DepthSnapshot, MarketBar, MarketTrade, OrderBook,
    OrderBookApplyOutcome, OrderBookRecoveryReason, OrderBookState as CanonicalOrderBookState,
};
use axiusflow_market_engine::{
    ClientId, ConsumerId, ConsumerIdentity, ConsumerResourceClass, EngineError, EngineResourceMode,
    GenerationId, HotSetManager, HotSetTier, MarketEngine, MarketEngineConfig, MarketStream,
    OrderFlowPublicationKind, ProviderCapabilities, ProviderConfig, ProviderGeneration,
    ProviderHealth, ProviderRequest, ResourcePolicyDecision, ResourcePolicyInput, SeriesSnapshot,
    SeriesTailOperation, StreamRequirements, Viewport, WorkspaceId, decide_resource_policy,
};
use axiusflow_provider_history::{CoverageSnapshot, DataClass, HistoryPageRequest, HistoryRange};
use axiusflow_rithmic_protocol_adapter::{
    RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID, RithmicCalendarPeriod, RithmicExchangeCalendar,
};
use sysinfo::System;

#[cfg(test)]
use std::sync::mpsc::TryRecvError;

use crate::coinbase_catalog::{
    CoinbaseCatalogControl, CoinbaseCatalogDispatchError, CoinbaseCatalogEvent,
};
use crate::rithmic_realtime::{
    RithmicCatalogControl, RithmicCatalogEvent, RithmicRealtimeControl, RithmicRealtimeEvent,
};

const COMMAND_CAPACITY: usize = 64;
const HISTORY_CAPACITY: usize = 8;
const STORAGE_CAPACITY: usize = 16;
const REALTIME_CAPACITY: usize = 2_048;
const RITHMIC_REALTIME_CONTROL_CAPACITY: usize = 2;
const REALTIME_DRAIN_BUDGET: usize = 256;
const LIVE_BUFFER_CAPACITY: usize = 4_096;
/// Bar updates a consumer may fall behind by before the oldest is dropped.
const CONSUMER_SERIES_QUEUE_CAPACITY: usize = 1_024;
const COORDINATOR_TICK: Duration = Duration::from_millis(16);
const LOCAL_HISTORY_READ_TIMEOUT: Duration = Duration::from_secs(2);
const EMPTY_REPAIR_RETRY_DELAY: Duration = Duration::from_secs(1);
const HISTORY_RETRY_DELAY: Duration = Duration::from_secs(1);
const HISTORY_CAPACITY_EXHAUSTED: &str = "provider history capacity is temporarily exhausted";
const MAXIMUM_HISTORY_RETRIES: u8 = 3;
const VIEWPORT_HISTORY_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAXIMUM_VIEWPORT_HISTORY_RETRIES: u8 = 3;
const LIVE_EDGE_REPAIR_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAXIMUM_LIVE_EDGE_REPAIR_RETRIES: u8 = 3;
const PROVIDER_RECONNECT_DELAY: Duration = Duration::from_millis(250);
const MAXIMUM_CONSUMERS: usize = 256;
const MAXIMUM_SERIES: usize = 128;
const HISTORY_BARS_PER_SERIES: usize = 32_768;
const VIEWPORT_LIVE_TAIL_RESERVE: usize = 512;
const VIEWPORT_BACKFILL_BARS: usize = HISTORY_BARS_PER_SERIES - VIEWPORT_LIVE_TAIL_RESERVE;
const MAXIMUM_STORED_BARS: usize = MAXIMUM_SERIES * (HISTORY_BARS_PER_SERIES + 1);
const MAXIMUM_CATALOG_INSTRUMENTS: usize = 4_096;
const MAXIMUM_CATALOG_FIELD_BYTES: usize = 256;
const MAXIMUM_PUBLISHED_DEPTH_LEVELS: usize = 50;
const MAXIMUM_TRADED_VOLUME_LEVELS: usize = 4_096;
const COINBASE_PROVIDER_GENERATION: u64 = 1;

type Reply<T> = SyncSender<Result<T, String>>;

/// Cloneable command boundary for the process-owned market coordinator.
#[derive(Clone)]
pub struct MarketService {
    commands: SyncSender<Command>,
    runtime: Arc<MarketRuntime>,
}

/// Bounded coordinator-owned lifecycle and memory snapshot for authenticated diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub struct MarketServiceStatus {
    pub resource_mode: ResourceMode,
    pub connected_desktop_clients: usize,
    pub providers: Vec<ProviderState>,
    pub retained_series: usize,
    pub retained_bars: usize,
    pub approximate_series_bytes: usize,
}

struct MarketRuntime {
    shutdown: Arc<AtomicBool>,
    active_provider_workers: Arc<Mutex<BTreeSet<String>>>,
    workers: Mutex<Option<Vec<thread::JoinHandle<()>>>>,
}

impl Drop for MarketRuntime {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

enum Command {
    RestoreHotSet(Vec<WarmSeries>, Reply<()>),
    SetResourceMode(ResourceMode, Reply<()>),
    Status(Reply<MarketServiceStatus>),
    Attach(
        ClientId,
        Option<SyncSender<(u64, envelope::Payload)>>,
        Reply<()>,
    ),
    Detach(ClientId, Reply<()>),
    Register(ConsumerIdentity, Reply<()>),
    Remove(ClientId, ConsumerId, Reply<()>),
    Viewport(ClientId, ConsumerId, GenerationId, Viewport, Reply<()>),
    ResourceClass(ClientId, ConsumerId, ConsumerResourceClass, Reply<()>),
    Demand(ClientId, ConsumerId, GenerationId, BarSeriesKey, Reply<()>),
    SearchProviderInstruments(ClientId, SearchProviderInstruments, Reply<()>),
    SelectProviderInstrument(ClientId, SelectProviderInstrument, Reply<()>),
    InstallProviderInstrument(InstallProviderInstrument, Reply<()>),
    Poll(ClientId, ConsumerId, Reply<Option<envelope::Payload>>),
    HistoryCompleted(
        BarSeriesKey,
        ProviderGeneration,
        Option<HistoryRange>,
        HistoryRequestKind,
        Result<HistorySnapshot, String>,
    ),
    LocalHistoryCompleted(
        BarSeriesKey,
        ProviderGeneration,
        Result<Option<StoredHistory>, String>,
    ),
    ViewportHistoryLocalCompleted(
        BarSeriesKey,
        ProviderGeneration,
        HistoryRange,
        Result<LocalRangeHistory, String>,
    ),
    ConfirmedEmptyRecorded(BarSeriesKey, Result<(), LocalHistoryError>),
    ConfirmedEmptyResolved(BarSeriesKey, HistoryRange, Result<(), LocalHistoryError>),
    PersistenceCompleted(
        BarSeriesKey,
        ProviderGeneration,
        Result<(), LocalHistoryError>,
        u64,
    ),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistoryRequestKind {
    Initial,
    LiveEdgeRepair(u8),
    ViewportBackfill,
}

#[derive(Clone, Copy)]
struct PendingLiveEdgeRepair {
    range: HistoryRange,
    attempt: u8,
    ready_at: Instant,
}

struct HistoryRequest {
    series: BarSeriesKey,
    provider_generation: ProviderGeneration,
    instrument: Option<InstallProviderInstrument>,
    maximum_bars: usize,
    range: Option<HistoryRange>,
    kind: HistoryRequestKind,
    stop: Arc<AtomicBool>,
}

#[derive(Clone)]
struct WarmSeries {
    series: BarSeriesKey,
    instrument: InstallProviderInstrument,
    provider_watermark: u64,
}

struct ActiveViewport {
    consumer_generation: GenerationId,
    provider_generation: ProviderGeneration,
    series: BarSeriesKey,
    range: HistoryRange,
}

enum StorageRequest {
    Read(BarSeriesKey, ProviderGeneration),
    ReadRange(BarSeriesKey, ProviderGeneration, HistoryRange),
    Persist(
        BarSeriesKey,
        ProviderGeneration,
        Vec<MarketBar>,
        bool,
        Vec<(BarSeriesKey, HistoryRange)>,
        Instant,
    ),
    RecordConfirmedEmpty(BarSeriesKey, HistoryRange),
    ResolveConfirmedEmpty(BarSeriesKey, HistoryRange),
}

struct HistorySnapshot {
    price_scale: u8,
    quantity_scale: u8,
    /// Buckets the provider has closed. Only these are persisted and installed
    /// as canonical history.
    bars: Vec<MarketBar>,
    /// The period that was still open when the provider served this page.
    ///
    /// It is deliberately kept out of `bars`: history is a record of closed
    /// periods, and a half-built candle written into it stays wrong until the
    /// period ends. The live handoff seeds it as the in-flight bar instead,
    /// which is what gives a fresh chart the OHLCV that accrued before the
    /// trader selected it.
    forming: Option<FormingBar>,
    handoff_boundary_unix_nanos: Option<i64>,
    confirmed_empty: bool,
}

/// Everything one Rithmic series needs to close its history/live seam.
struct RithmicHandoffSeed<'a> {
    price_scale: u8,
    quantity_scale: u8,
    /// Periods the provider closed, already installed as canonical history.
    bars: &'a [MarketBar],
    /// The period the provider caught open, held live rather than in history.
    forming: Option<FormingBar>,
    /// Every trade at or before this instant is already inside what the provider
    /// returned, so replaying it would count it twice.
    handoff_boundary_unix_nanos: Option<i64>,
}

/// The period a provider page caught mid-flight, and how far into it the market
/// already is.
pub(crate) struct FormingBar {
    pub(crate) bar: MarketBar,
    /// Trades already inside the open bundle. `None` for a clock-driven period,
    /// where elapsed time rather than a count decides when the bar closes.
    pub(crate) trades: Option<u32>,
}

struct LocalRangeHistory {
    stored: Option<StoredHistory>,
    confirmed_empty: Vec<HistoryRange>,
}

struct DemandWaiter {
    consumer_id: ConsumerId,
    generation: GenerationId,
    started_at: Instant,
}

enum RealtimeControl {
    Start(Vec<RealtimeProduct>),
}

/// One realtime product plus the precision Level 2 decoding needs.
#[derive(Clone, Eq, PartialEq)]
struct RealtimeProduct {
    symbol: String,
    price_scale: u8,
    quantity_scale: u8,
}

enum RealtimeEvent {
    Connecting(ProviderGeneration),
    Connected(ProviderGeneration),
    Trade(ProviderGeneration, CanonicalTrade),
    Depth(ProviderGeneration, Box<DepthSnapshot>),
    Heartbeat(ProviderGeneration),
    Disconnected(ProviderGeneration),
}

/// Bounded per-consumer outbox.
///
/// The series channel is a queue, not a slot, because bar identity is a strict
/// sequence: silently replacing a pending update loses a bar, and the consumer
/// reads the resulting gap as corruption it can only repair with a full
/// resnapshot. Every other channel stays latest-value, because a newer order
/// book, provider state, or catalog result fully supersedes the one before it.
#[derive(Default)]
struct ConsumerEvents {
    provider: Option<envelope::Payload>,
    series: VecDeque<envelope::Payload>,
    /// Set when the series queue could not hold one more distinct bar. The
    /// coordinator resolves it by replacing the whole queue with a covering
    /// snapshot, because dropping the oldest update opens a sequence gap the
    /// consumer can only read as corruption.
    series_overflowed: bool,
    series_state: Option<envelope::Payload>,
    demand_error: Option<envelope::Payload>,
    order_book: Option<envelope::Payload>,
    order_flow: Option<envelope::Payload>,
    catalog_search: Option<envelope::Payload>,
    catalog_selection: Option<envelope::Payload>,
}

struct ProviderOrderBook {
    instrument: InstallProviderInstrument,
    book: OrderBook,
    traded_volumes: BTreeMap<i64, i64>,
    watch: DepthSnapshotWatch,
}

/// Bounded recovery state for a book still awaiting its first snapshot.
///
/// A missed initial venue snapshot otherwise stalls the book forever: later
/// deltas cannot build it, and nothing resubscribes while the product set is
/// unchanged. The watchdog resubscribes a few times, then goes quiet; a new
/// demand, product change, or session reconnect re-arms it.
#[derive(Default)]
struct DepthSnapshotWatch {
    awaited_since: Option<Instant>,
    last_attempt: Option<Instant>,
    resubscribes: u32,
}

mod realtime;
#[cfg(test)]
use realtime::run_realtime_worker;
use realtime::{spawn_realtime_worker, try_emit_realtime};

struct LiveHandoff {
    generation: ProviderGeneration,
    aggregator: CoinbaseBarAggregator,
    buffered: VecDeque<CanonicalTrade>,
    connected: bool,
    history: CoinbaseHistoryReadiness,
    dirty: bool,
    /// Highest sequence the canonical series already holds as a completed bar.
    /// Everything above it in the aggregator still has to be appended.
    published_completed: Option<u64>,
}

#[derive(Clone, Copy)]
struct PendingViewportHistoryRetry {
    ready_at: Instant,
    attempts: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoinbaseHistoryReadiness {
    Pending,
    /// Coinbase candles expose no trade watermark. The first open candle stays
    /// provisional until it closes and a provider repair replaces it.
    Provisional,
    Authoritative,
}

impl CoinbaseHistoryReadiness {
    const fn is_ready(self) -> bool {
        !matches!(self, Self::Pending)
    }

    const fn is_authoritative(self) -> bool {
        matches!(self, Self::Authoritative)
    }
}

struct RithmicLiveHandoff {
    series: BarSeriesKey,
    generation: ProviderGeneration,
    cadence: RithmicLiveCadence,
    price_scale: u8,
    quantity_scale: u8,
    bars: Vec<MarketBar>,
    buffered: VecDeque<MarketTrade>,
    connected: bool,
    history_ready: bool,
    dirty: bool,
    published: Option<PublishedTailState>,
    /// The sequence of the period the provider caught open, when it caught one.
    ///
    /// Live trades revise an open period in place; they may never touch a period
    /// the provider has closed. Without this the two are indistinguishable, and
    /// the first trade of a freshly seeded chart was dropped rather than folded
    /// into the candle it belongs to.
    forming_tail_sequence: Option<u64>,
    live_session_generation: Option<u64>,
    last_trade_sequence: Option<u64>,
    history_boundary_unix_nanos: i64,
}

enum RithmicLiveCadence {
    Fixed {
        seconds: i64,
    },
    Tick {
        trades: u32,
        forming: u32,
    },
    Calendar {
        calendar: RithmicExchangeCalendar,
        period: RithmicCalendarPeriod,
    },
}

/// One live publication for a series.
///
/// `Tails` is an append-only run: every bar extends the canonical series by one
/// sequence, or replaces the forming bar in place. A bucket roll therefore adds
/// the completed bar and opens the next one without ever replacing the covering
/// history — which is what used to discard a backfill the moment a bar rolled.
enum LiveSeriesPublication {
    Tails(Vec<MarketBar>),
    Covering(Vec<MarketBar>),
}

/// What the canonical series last accepted from one live handoff.
#[derive(Clone, Copy)]
enum PublishedTailState {
    Covering(u64),
    Forming(u64),
}

/// A live bar that neither continues the published tail nor revises it in place
/// cannot be appended, so the whole series is republished instead.
trait HistorySource: Send + 'static {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String>;
}

trait RealtimeSource: Send + 'static {
    fn configure(&mut self, _products: Vec<RealtimeProduct>) -> Result<(), String> {
        Ok(())
    }

    fn run_generation(
        &mut self,
        generation: ProviderGeneration,
        controls: &Receiver<RealtimeControl>,
        events: &SyncSender<RealtimeEvent>,
        overflow: &AtomicBool,
        stop: &Arc<AtomicBool>,
    ) -> bool;
}

struct LiveCoinbaseHistory {
    adapter: CoinbaseHistoryCapabilityAdapter,
}

struct LiveRithmicHistory;

struct ProviderRuntimeSpec {
    provider_id: &'static str,
    history: Box<dyn HistorySource>,
    realtime: ProviderRealtimeSpec,
}

enum ProviderRealtimeSpec {
    Coinbase(Option<Box<dyn RealtimeSource>>),
    Rithmic { enabled: bool },
}

impl ProviderRuntimeSpec {
    fn coinbase(
        history: Box<dyn HistorySource>,
        realtime: Option<Box<dyn RealtimeSource>>,
    ) -> Self {
        Self {
            provider_id: "coinbase",
            history,
            realtime: ProviderRealtimeSpec::Coinbase(realtime),
        }
    }

    fn rithmic(history: Box<dyn HistorySource>, enabled: bool) -> Self {
        Self {
            provider_id: "rithmic",
            history,
            realtime: ProviderRealtimeSpec::Rithmic { enabled },
        }
    }
}

#[derive(Default)]
struct ProviderRuntimeLifecycle {
    generation: AtomicU64,
    reconnecting: AtomicBool,
    terminal_failure: Mutex<Option<String>>,
}

impl ProviderRuntimeLifecycle {
    fn observe_generation(&self, generation: u64, reconnecting: bool) {
        self.generation.store(generation, Ordering::Release);
        self.reconnecting.store(reconnecting, Ordering::Release);
    }

    fn mark_terminal_failure(&self, detail: impl Into<String>) {
        *self
            .terminal_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(detail.into());
    }

    fn detail(&self) -> Option<String> {
        self.terminal_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .or_else(|| {
                self.reconnecting.load(Ordering::Acquire).then(|| {
                    format!(
                        "provider runtime generation {} is reconnecting",
                        self.generation.load(Ordering::Acquire)
                    )
                })
            })
    }
}

enum ProviderRealtimeChannels {
    Coinbase {
        enabled: bool,
        controls: SyncSender<RealtimeControl>,
        events: Receiver<RealtimeEvent>,
        overflow: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
    },
    Rithmic {
        enabled: bool,
        controls: SyncSender<RithmicRealtimeControl>,
        events: Receiver<RithmicRealtimeEvent>,
    },
}

enum ProviderCatalogChannels {
    Coinbase {
        controls: CoinbaseCatalogControl,
        events: Receiver<CoinbaseCatalogEvent>,
    },
    Rithmic {
        enabled: bool,
        controls: SyncSender<RithmicCatalogControl>,
        events: Receiver<RithmicCatalogEvent>,
    },
}

struct ProviderRuntimeRecord {
    history: SyncSender<HistoryRequest>,
    cancellation: Arc<AtomicBool>,
    lifecycle: Arc<ProviderRuntimeLifecycle>,
    realtime: ProviderRealtimeChannels,
    catalog: ProviderCatalogChannels,
    workers: Vec<thread::JoinHandle<()>>,
}

struct StartedProviderRuntime {
    history: SyncSender<HistoryRequest>,
    cancellation: Arc<AtomicBool>,
    lifecycle: Arc<ProviderRuntimeLifecycle>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl StartedProviderRuntime {
    fn cancel_and_join(self) {
        self.cancellation.store(true, Ordering::Release);
        drop(self.history);
        join_runtime_workers(self.workers);
    }
}

struct CoinbaseRealtimeWorkerState {
    overflow: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    lifecycle: Arc<ProviderRuntimeLifecycle>,
    active_workers: Arc<Mutex<BTreeSet<String>>>,
}

/// Engine-owned bounded runtime records keyed by provider identity.
///
/// `MarketEngine` remains the capability and generation authority. This registry owns only the
/// concrete adapter workers and their bounded dispatch/lifecycle state.
struct ProviderRuntimeRegistry {
    records: BTreeMap<&'static str, ProviderRuntimeRecord>,
}

struct ActiveWorkerGuard {
    name: &'static str,
    active: Arc<Mutex<BTreeSet<String>>>,
}

impl ActiveWorkerGuard {
    fn register(name: &'static str, active: Arc<Mutex<BTreeSet<String>>>) -> Self {
        active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(name.to_string());
        Self { name, active }
    }
}

impl Drop for ActiveWorkerGuard {
    fn drop(&mut self) {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(self.name);
    }
}

struct ProviderDispatch<'a> {
    records: BTreeMap<&'static str, ProviderDispatchRecord<'a>>,
}

struct ProviderDispatchRecord<'a> {
    history: &'a SyncSender<HistoryRequest>,
    lifecycle: Option<&'a ProviderRuntimeLifecycle>,
    realtime: ProviderRealtimeDispatch<'a>,
    catalog: ProviderCatalogDispatch<'a>,
}

enum ProviderRealtimeDispatch<'a> {
    Coinbase {
        controls: &'a SyncSender<RealtimeControl>,
        events: &'a Receiver<RealtimeEvent>,
        overflow: &'a AtomicBool,
        stop: &'a Arc<AtomicBool>,
    },
    Rithmic {
        controls: &'a SyncSender<RithmicRealtimeControl>,
        events: &'a Receiver<RithmicRealtimeEvent>,
    },
    #[cfg(test)]
    TestCoinbase {
        controls: &'a SyncSender<RealtimeControl>,
        stop: &'a Arc<AtomicBool>,
    },
    #[cfg(test)]
    TestRithmic {
        controls: &'a SyncSender<RithmicRealtimeControl>,
    },
    Disabled,
}

impl<'a> ProviderRealtimeDispatch<'a> {
    fn coinbase_controls(&self) -> Option<&'a SyncSender<RealtimeControl>> {
        match self {
            Self::Coinbase { controls, .. } => Some(controls),
            #[cfg(test)]
            Self::TestCoinbase { controls, .. } => Some(controls),
            _ => None,
        }
    }

    fn rithmic_controls(&self) -> Option<&'a SyncSender<RithmicRealtimeControl>> {
        match self {
            Self::Rithmic { controls, .. } => Some(controls),
            #[cfg(test)]
            Self::TestRithmic { controls } => Some(controls),
            _ => None,
        }
    }
}

enum ProviderCatalogDispatch<'a> {
    Coinbase {
        controls: &'a CoinbaseCatalogControl,
        events: &'a Receiver<CoinbaseCatalogEvent>,
    },
    Rithmic {
        controls: &'a SyncSender<RithmicCatalogControl>,
        events: &'a Receiver<RithmicCatalogEvent>,
    },
    #[cfg(test)]
    TestCoinbase {
        controls: &'a CoinbaseCatalogControl,
    },
    Disabled,
}

enum ProviderCatalogCommand {
    Search(SearchProviderInstruments),
    Select(SelectProviderInstrument),
}

enum ProviderRuntimeEvent {
    CoinbaseRealtime(RealtimeEvent),
    RithmicRealtime(RithmicRealtimeEvent),
    CoinbaseCatalog(CoinbaseCatalogEvent),
    RithmicCatalog(RithmicCatalogEvent),
}

struct LiveCoinbaseRealtime {
    config: Option<CoinbaseConfig>,
    products: Vec<RealtimeProduct>,
}

mod runtime;
use runtime::join_runtime_workers;
#[cfg(test)]
use runtime::{
    coinbase_handoff_replay_boundary, coinbase_history_edge_is_current, forming_coinbase_bucket,
};

mod storage;
use storage::spawn_storage_worker;

mod coordinator;
use coordinator::{Coordinator, OwnedCoordinatorChannels, RithmicSelection, spawn_coordinator};

fn try_enqueue_history(
    history: &SyncSender<HistoryRequest>,
    request: HistoryRequest,
) -> Result<(), &'static str> {
    match history.try_send(request) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => Err(HISTORY_CAPACITY_EXHAUSTED),
        Err(TrySendError::Disconnected(_)) => Err("provider history worker is unavailable"),
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

fn configured_reconnect_delay(engine: &MarketEngine, provider: &str) -> Result<Duration, String> {
    engine
        .provider_reconnect_delay(provider)
        .ok_or_else(|| format!("{provider} reconnect policy is unavailable"))
}

fn available_memory_bytes() -> u64 {
    let mut system = System::new();
    system.refresh_memory();
    system.available_memory()
}

const fn resource_policy_mode(mode: ResourceMode) -> EngineResourceMode {
    match mode {
        ResourceMode::Interactive => EngineResourceMode::Interactive,
        ResourceMode::Warm => EngineResourceMode::Warm,
        ResourceMode::Constrained => EngineResourceMode::Constrained,
        ResourceMode::OfflineSuspended => EngineResourceMode::OfflineSuspended,
        ResourceMode::MarketsLive => EngineResourceMode::MarketsLive,
    }
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
            ProviderConfig {
                account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
                capabilities: ProviderCapabilities {
                    historical_bars: true,
                    realtime_bars: true,
                    streams: StreamRequirements::BARS
                        .with(MarketStream::Trades)
                        .with(MarketStream::Depth),
                },
                reconnect_delay: PROVIDER_RECONNECT_DELAY,
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
    engine
        .register_provider(
            "rithmic".to_string(),
            ProviderConfig {
                account_id: RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID.to_string(),
                capabilities: ProviderCapabilities {
                    historical_bars: true,
                    realtime_bars: true,
                    streams: StreamRequirements::BARS
                        .with(MarketStream::Trades)
                        .with(MarketStream::Depth),
                },
                reconnect_delay: PROVIDER_RECONNECT_DELAY,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(engine)
}

mod publication;
use publication::{
    engine_install_failure_stage, fail_waiters, local_history_failure_stage, order_flow_payload,
    publish_ready, publish_state, series_state, series_state_with_persistence,
    series_update_message, snapshot_message,
};

mod instrument_selection;
use instrument_selection::{
    chart_stream_requirements, coinbase_catalog_dispatch_error, id, try_send_rithmic_catalog,
    validate_provider_instrument, validate_provider_search, validate_provider_selection,
};

mod history;
#[cfg(test)]
use history::recent_coinbase_history_range;
use history::{
    HistoryPrecedence, canonical_local_range, canonicalize_coinbase_history, coinbase_aggregator,
    coinbase_bar_coverage_ranges, coinbase_interval, coinbase_live_edge_repair_range,
    coinbase_series_interval, coinbase_series_profile, current_unix_nanos, internal_series,
    reconcile_history_repair, record_covered_range, retained_hot_series, spawn_history_worker,
    warm_series,
};

fn ipc_series(series: &BarSeriesKey) -> SeriesKey {
    SeriesKey {
        provider: series.provider_id.clone(),
        instrument_id: series.instrument_id.clone(),
        cadence_value: match series.period {
            BarPeriod::Time { seconds } => seconds,
            BarPeriod::Tick { trades } => trades,
            BarPeriod::Session { days } => days,
            BarPeriod::Week { weeks } => weeks,
            BarPeriod::Month { months } => months,
        },
        definition_revision: series.definition_version,
        entitlement_id: series.entitlement_id.clone(),
        cadence: match series.period {
            BarPeriod::Time { .. } => SeriesCadence::FixedSeconds,
            BarPeriod::Tick { .. } => SeriesCadence::Trades,
            BarPeriod::Session { .. } => SeriesCadence::SessionDays,
            BarPeriod::Week { .. } => SeriesCadence::CalendarWeeks,
            BarPeriod::Month { .. } => SeriesCadence::CalendarMonths,
        } as i32,
    }
}

const fn ipc_bar(bar: MarketBar) -> IpcMarketBar {
    IpcMarketBar {
        source_sequence: bar.source_sequence,
        exchange_timestamp_seconds: bar.exchange_timestamp_seconds,
        exchange_timestamp_unix_nanos: bar.exchange_timestamp_unix_nanos,
        open: bar.open,
        high: bar.high,
        low: bar.low,
        close: bar.close,
        volume: bar.volume,
    }
}

#[cfg(test)]
fn test_coinbase_instrument() -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: "coinbase".to_string(),
        session_generation: 1,
        selection_generation: 1,
        instrument_id: "instrument:coinbase:btc:usd".to_string(),
        provider_symbol: "BTC-USD".to_string(),
        display_symbol: "BTC/USD".to_string(),
        venue_id: "coinbase".to_string(),
        price_scale: 2,
        quantity_scale: 8,
        entitlement_id: ENTITLEMENT_CLASS.to_string(),
    }
}

#[cfg(test)]
fn test_coinbase_eth_instrument() -> InstallProviderInstrument {
    let mut instrument = test_coinbase_instrument();
    instrument.instrument_id = "instrument:coinbase:eth:usd".to_string();
    instrument.provider_symbol = "ETH-USD".to_string();
    instrument.display_symbol = "ETH/USD".to_string();
    instrument
}

#[cfg(test)]
pub(crate) mod tests;
