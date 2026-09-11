//! Single-owner in-process market runtime and provider workers.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    MarketRuntimeEvent,
    study::{
        MAXIMUM_STUDY_DEPENDENCIES_PER_INSTANCE, MAXIMUM_STUDY_OUTPUTS_PER_INSTANCE,
        NativeStudyRegistration, StudyInstanceId, StudyMarketLeaseChangeKind, StudyOutputId,
        StudyRuntime, StudyRuntimeConfig,
    },
};
use axiusflow_contracts::{
    EngineFaultCode, FailureStage, InstallProviderInstrument, ProviderCatalogRejected,
    ProviderCatalogRejectionReason, ProviderConnectionState, ProviderState,
    SearchProviderInstruments, SelectProviderInstrument, SeriesLoadState,
};
use axiusflow_hyperliquid_market_adapter::{
    HyperliquidLiveCandle, hyperliquid_interval_for_period, merge_live_candle,
};
use axiusflow_market_data::{
    AggressorSide, AggressorTradeVolumes, BarPeriod, BarSeriesKey, DepthSnapshot, MarketBar,
    MarketTrade, OrderBook, OrderBookApplyOutcome, OrderBookState as CanonicalOrderBookState,
    TopOfBookQuote,
};
use axiusflow_market_engine::{
    ClientId, ConsumerId, ConsumerIdentity, ConsumerResourceClass, EngineError, GenerationId,
    MarketEngine, MarketEngineConfig, MarketStream, ProviderCapabilities, ProviderConfig,
    ProviderGeneration, ProviderHealth, ProviderRequest, SeriesSnapshot, SeriesTailOperation,
    StreamRequirements, Viewport, WorkspaceId,
};
use axiusflow_provider_history::HistoryRange;
use axiusflow_rithmic_protocol_adapter::{
    RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID, RithmicCalendarPeriod, RithmicExchangeCalendar,
};

use crate::hyperliquid_realtime::{
    HYPERLIQUID_PUBLIC_ACCOUNT_ID, HyperliquidCandleDemand, HyperliquidCatalogControl,
    HyperliquidCatalogEvent, HyperliquidDemand, HyperliquidInstrumentDemand,
    HyperliquidRealtimeControl, HyperliquidRealtimeEvent,
};
use crate::rithmic_realtime::{
    RithmicCatalogControl, RithmicCatalogEvent, RithmicInstrumentDemand, RithmicRealtimeControl,
    RithmicRealtimeDemand, RithmicRealtimeEvent,
};

mod alerts;
use alerts::PriceAlertRegistry;

const COMMAND_CAPACITY: usize = 64;
const HISTORY_CAPACITY: usize = 8;
const REALTIME_CAPACITY: usize = 2_048;
const RITHMIC_REALTIME_CONTROL_CAPACITY: usize = 2;
const REALTIME_DRAIN_BUDGET: usize = 256;
/// Bar updates a consumer may fall behind by before the oldest is dropped.
const CONSUMER_SERIES_QUEUE_CAPACITY: usize = 1_024;
const COORDINATOR_TICK: Duration = Duration::from_millis(16);
const HISTORY_RETRY_DELAY: Duration = Duration::from_secs(1);
/// After the bounded automatic retry budget is exhausted, keep the identical
/// ranged request in a failed cooldown like Flowsurface's `RequestHandler`.
const HISTORY_FAILED_RETRY_COOLDOWN: Duration = Duration::from_secs(30);
const HISTORY_CAPACITY_EXHAUSTED: &str = "provider history capacity is temporarily exhausted";
const MAXIMUM_HISTORY_RETRIES: u8 = 3;
const PROVIDER_RECONNECT_DELAY: Duration = Duration::from_millis(250);
const MAXIMUM_CONSUMERS: usize = 256;
const MAXIMUM_SERIES: usize = 64;
const MAXIMUM_STUDIES: usize = MAXIMUM_CONSUMERS * 16;
const MAXIMUM_STUDY_DEPENDENCIES: usize = MAXIMUM_STUDY_DEPENDENCIES_PER_INSTANCE;
const MAXIMUM_STUDY_OUTPUTS: usize = MAXIMUM_STUDY_OUTPUTS_PER_INSTANCE;
/// Upper bound for one provider history request. Retained chart history itself
/// is managed separately by the canonical working-window watermarks below.
const MAXIMUM_HISTORY_BARS_PER_REQUEST: usize = 8_192;
/// Canonical history compacts to this many bars after crossing the high watermark.
const HISTORY_SERIES_TARGET_BARS: usize = 12_288;
/// Hard runtime working-set watermark for one canonical series.
const HISTORY_SERIES_HIGH_WATERMARK: usize = 16_384;
/// Global canonical bar ceiling. Provider/live buffers are independently bounded.
const MAXIMUM_STORED_BARS: usize = MAXIMUM_SERIES * HISTORY_SERIES_HIGH_WATERMARK;
const MAXIMUM_STUDY_POINTS_PER_OUTPUT: usize = HISTORY_SERIES_HIGH_WATERMARK;
const MAXIMUM_STUDY_TOTAL_OUTPUT_POINTS: usize = MAXIMUM_STORED_BARS;
// Stateful formulas may retain one transfer buffer per declared output in
// addition to sparse recursive checkpoints. Bound one instance at twice the
// worst-case f64 transfer footprint for the canonical per-series watermark,
// while the runtime-wide cap prevents many studies from multiplying that peak.
const MAXIMUM_STUDY_STATE_BYTES_PER_INSTANCE: usize =
    MAXIMUM_STUDY_OUTPUTS * HISTORY_SERIES_HIGH_WATERMARK * size_of::<f64>() * 2;
const MAXIMUM_STUDY_TOTAL_STATE_BYTES: usize = 64 * 1024 * 1024;
const INITIAL_HISTORY_BARS: usize = 600;
const VIEWPORT_LIVE_TAIL_RESERVE: usize = 512;
const MAXIMUM_CATALOG_INSTRUMENTS: usize = 4_096;
const MAXIMUM_CATALOG_FIELD_BYTES: usize = 256;
const LIVE_BUFFER_CAPACITY: usize = 2_048;
const LIVE_HANDOFF_HISTORY_BARS: usize = VIEWPORT_LIVE_TAIL_RESERVE + 1;

type Reply<T> = SyncSender<Result<T, String>>;

/// Cloneable command boundary for the desktop-owned market coordinator.
#[derive(Clone)]
pub struct MarketService {
    commands: SyncSender<Command>,
    runtime: Arc<MarketRuntime>,
}

/// Bounded coordinator-owned lifecycle and memory snapshot for authenticated diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub struct MarketServiceStatus {
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
    /// Provider workers use this zero-payload control only to interrupt the
    /// coordinator's bounded idle wait after publishing an event. The actual
    /// provider event remains in its dedicated bounded lane and is drained at
    /// the top of the next coordinator iteration.
    ProviderWake,
    Status(Reply<MarketServiceStatus>),
    Attach(ClientId, Reply<()>),
    Detach(ClientId, Reply<()>),
    Register(ConsumerIdentity, Reply<()>),
    Remove(ClientId, ConsumerId, Reply<()>),
    Viewport(ClientId, ConsumerId, GenerationId, Viewport, Reply<()>),
    ResourceClass(ClientId, ConsumerId, ConsumerResourceClass, Reply<()>),
    Streams(
        ClientId,
        ConsumerId,
        GenerationId,
        StreamRequirements,
        Reply<()>,
    ),
    Demand(
        ClientId,
        ConsumerId,
        GenerationId,
        BarSeriesKey,
        StreamRequirements,
        Reply<()>,
    ),
    RegisterStudy(
        ClientId,
        ConsumerId,
        NativeStudyRegistration,
        Reply<StudyInstanceId>,
    ),
    ReinitializeStudy(
        ClientId,
        StudyInstanceId,
        NativeStudyRegistration,
        Reply<Vec<StudyInstanceId>>,
    ),
    RemoveStudy(ClientId, StudyInstanceId, Reply<Vec<StudyInstanceId>>),
    SearchProviderInstruments(ClientId, SearchProviderInstruments, Reply<()>),
    SelectProviderInstrument(ClientId, SelectProviderInstrument, Reply<()>),
    InstallProviderInstrument(InstallProviderInstrument, Reply<()>),
    ReplacePriceAlerts(
        ClientId,
        ConsumerId,
        Vec<crate::MarketPriceAlert>,
        Reply<()>,
    ),
    Poll(ClientId, ConsumerId, Reply<Option<MarketRuntimeEvent>>),
    PollClient(
        ClientId,
        Vec<(ConsumerId, usize)>,
        Reply<Vec<(u64, MarketRuntimeEvent)>>,
    ),
    HistoryCompleted(
        BarSeriesKey,
        ProviderGeneration,
        Option<HistoryRange>,
        Result<HistorySnapshot, String>,
    ),
}

/// Cloneable no-payload wake edge shared with provider workers.
///
/// A full command queue already guarantees the coordinator is runnable, so a
/// dropped wake in that case cannot delay provider-event draining.
#[derive(Clone)]
pub(crate) struct ProviderCoordinatorWake {
    commands: SyncSender<Command>,
}

impl ProviderCoordinatorWake {
    fn new(commands: SyncSender<Command>) -> Self {
        Self { commands }
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        let (commands, _receiver) = mpsc::sync_channel(1);
        Self::new(commands)
    }

    pub(crate) fn notify(&self) {
        match self.commands.try_send(Command::ProviderWake) {
            Ok(()) | Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {}
        }
    }
}

struct HistoryRequest {
    series: BarSeriesKey,
    provider_generation: ProviderGeneration,
    instrument: Option<InstallProviderInstrument>,
    maximum_bars: usize,
    range: Option<HistoryRange>,
    stop: Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeferredHistoryRequest {
    Full,
    Range(HistoryRange),
}

#[derive(Clone, Copy)]
pub(crate) struct HistoryFetchWindow {
    pub(crate) maximum_bars: usize,
    pub(crate) range: Option<HistoryRange>,
}

impl HistoryRequest {
    const fn req_window(&self) -> HistoryFetchWindow {
        HistoryFetchWindow {
            maximum_bars: self.maximum_bars,
            range: self.range,
        }
    }
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
}

/// Everything one Hyperliquid series needs to close its history/live seam.
struct HyperliquidHandoffSeed<'a> {
    price_scale: u8,
    quantity_scale: u8,
    /// Periods the provider closed, already installed as canonical history.
    bars: &'a [MarketBar],
    /// The period the provider caught open, held live rather than in history.
    forming: Option<FormingBar>,
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

struct DemandWaiter {
    consumer_id: ConsumerId,
    generation: GenerationId,
    started_at: Instant,
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
    provider: Option<MarketRuntimeEvent>,
    series: VecDeque<MarketRuntimeEvent>,
    /// Set when the series queue could not hold one more distinct bar. The
    /// coordinator resolves it by replacing the whole queue with a covering
    /// snapshot, because dropping the oldest update opens a sequence gap the
    /// consumer can only read as corruption.
    series_overflowed: bool,
    series_state: Option<MarketRuntimeEvent>,
    demand_error: Option<MarketRuntimeEvent>,
    /// Latest immutable snapshot per study output. A newer generation fully
    /// supersedes an older queued image for the same output.
    study_outputs: BTreeMap<StudyOutputId, MarketRuntimeEvent>,
    study_invalidated: Option<MarketRuntimeEvent>,
    /// Removal is infrequent and subtree-sized. Consecutive removals are merged
    /// into one bounded event before reaching presentation.
    study_removed: Option<MarketRuntimeEvent>,
    order_book: Option<MarketRuntimeEvent>,
    price_alerts: VecDeque<MarketRuntimeEvent>,
    catalog_search: Option<MarketRuntimeEvent>,
    catalog_selection: Option<MarketRuntimeEvent>,
}

struct ProviderOrderBook {
    instrument: InstallProviderInstrument,
    book: OrderBook,
    top_of_book: Option<TopOfBookQuote>,
    recent_trades: VecDeque<RecentAggressorTrade>,
    traded_volumes: BTreeMap<i64, AggressorTradeVolumes>,
    trade_session_generation: u64,
    last_trade_source_sequence: u64,
    retention_clock_unix_nanos: i64,
}

#[derive(Clone, Copy)]
struct RecentAggressorTrade {
    observed_unix_nanos: i64,
    price: i64,
    quantity: i64,
    aggressor: AggressorSide,
}

mod realtime;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiveHistoryState {
    AwaitingHistory,
    Ready,
    Reseeding,
}

/// Live candle handoff for one Hyperliquid series.
///
/// Unlike the trade-built Rithmic handoff, provider candles arrive whole:
/// history seeds closed bars plus the open period, and live replacements
/// merge by candle-open timestamp with exactly one forming candle. Sequence
/// numbers stay engine-owned so a redelivered update can never look new.
struct HyperliquidLiveHandoff {
    series: BarSeriesKey,
    generation: ProviderGeneration,
    wire_coin: String,
    interval: String,
    price_scale: u8,
    quantity_scale: u8,
    bars: Vec<MarketBar>,
    forming: Option<MarketBar>,
    /// Live updates that arrived before history seeded the seam, bounded.
    buffered: VecDeque<HyperliquidLiveCandle>,
    pending_publications: VecDeque<MarketBar>,
    connected: bool,
    history_state: LiveHistoryState,
    dirty: bool,
}

struct RithmicLiveHandoff {
    series: BarSeriesKey,
    generation: ProviderGeneration,
    cadence: RithmicLiveCadence,
    price_scale: u8,
    quantity_scale: u8,
    bars: Vec<MarketBar>,
    buffered: VecDeque<MarketTrade>,
    pending_publications: VecDeque<MarketBar>,
    connected: bool,
    history_state: LiveHistoryState,
    dirty: bool,
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

/// One live publication for a series. Live handoffs publish only incremental
/// tails; covering history remains owned by `MarketEngine` and is never copied
/// out of a handoff on the hot path.
enum LiveSeriesPublication {
    Tails(Vec<MarketBar>),
}

/// A live bar that neither continues the published tail nor revises it in place
/// cannot be appended, so the whole series is republished instead.
trait HistorySource: Send + 'static {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String>;
}

struct LiveRithmicHistory;

struct LiveHyperliquidHistory;

struct ProviderRuntimeSpec {
    provider_id: &'static str,
    history: Box<dyn HistorySource>,
    realtime: ProviderRealtimeSpec,
}

struct ProviderRealtimeSpec {
    enabled: bool,
}

impl ProviderRuntimeSpec {
    fn rithmic(history: Box<dyn HistorySource>, enabled: bool) -> Self {
        Self {
            provider_id: "rithmic",
            history,
            realtime: ProviderRealtimeSpec { enabled },
        }
    }

    fn hyperliquid(history: Box<dyn HistorySource>, enabled: bool) -> Self {
        Self {
            provider_id: "hyperliquid",
            history,
            realtime: ProviderRealtimeSpec { enabled },
        }
    }
}

#[derive(Default)]
struct ProviderRuntimeLifecycle {
    generation: AtomicU64,
    reconnecting: AtomicBool,
    transport_rtt_nanos: AtomicU64,
    terminal_failure: Mutex<Option<String>>,
}

impl ProviderRuntimeLifecycle {
    fn observe_generation(&self, generation: u64, reconnecting: bool) {
        let previous = self.generation.swap(generation, Ordering::AcqRel);
        if previous != generation || reconnecting {
            self.transport_rtt_nanos.store(0, Ordering::Release);
        }
        self.reconnecting.store(reconnecting, Ordering::Release);
    }

    fn observe_transport_rtt(&self, generation: u64, transport_rtt_nanos: u64) {
        if self.generation.load(Ordering::Acquire) == generation
            && !self.reconnecting.load(Ordering::Acquire)
        {
            self.transport_rtt_nanos
                .store(transport_rtt_nanos.max(1), Ordering::Release);
        }
    }

    fn transport_rtt_nanos(&self) -> Option<u64> {
        let value = self.transport_rtt_nanos.load(Ordering::Acquire);
        (value != 0).then_some(value)
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

struct ProviderRealtimeChannels {
    enabled: bool,
    channels: ProviderRealtimeChannelSet,
}

enum ProviderRealtimeChannelSet {
    Rithmic {
        controls: SyncSender<RithmicRealtimeControl>,
        events: Receiver<RithmicRealtimeEvent>,
    },
    Hyperliquid {
        controls: SyncSender<HyperliquidRealtimeControl>,
        events: Receiver<HyperliquidRealtimeEvent>,
    },
}

struct ProviderCatalogChannels {
    enabled: bool,
    channels: ProviderCatalogChannelSet,
}

enum ProviderCatalogChannelSet {
    Rithmic {
        controls: SyncSender<RithmicCatalogControl>,
        events: Receiver<RithmicCatalogEvent>,
    },
    Hyperliquid {
        controls: SyncSender<HyperliquidCatalogControl>,
        events: Receiver<HyperliquidCatalogEvent>,
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
    Rithmic {
        controls: &'a SyncSender<RithmicRealtimeControl>,
        events: &'a Receiver<RithmicRealtimeEvent>,
    },
    Hyperliquid {
        controls: &'a SyncSender<HyperliquidRealtimeControl>,
        events: &'a Receiver<HyperliquidRealtimeEvent>,
    },
    Disabled,
}

impl<'a> ProviderRealtimeDispatch<'a> {
    fn rithmic_controls(&self) -> Option<&'a SyncSender<RithmicRealtimeControl>> {
        match self {
            Self::Rithmic { controls, .. } => Some(controls),
            Self::Hyperliquid { .. } | Self::Disabled => None,
        }
    }

    fn hyperliquid_controls(&self) -> Option<&'a SyncSender<HyperliquidRealtimeControl>> {
        match self {
            Self::Hyperliquid { controls, .. } => Some(controls),
            Self::Rithmic { .. } | Self::Disabled => None,
        }
    }
}

enum ProviderCatalogDispatch<'a> {
    Rithmic {
        controls: &'a SyncSender<RithmicCatalogControl>,
        events: &'a Receiver<RithmicCatalogEvent>,
    },
    Hyperliquid {
        controls: &'a SyncSender<HyperliquidCatalogControl>,
        events: &'a Receiver<HyperliquidCatalogEvent>,
    },
    Disabled,
}

enum ProviderCatalogCommand {
    Search(SearchProviderInstruments),
    Select(SelectProviderInstrument),
}

enum ProviderRuntimeEvent {
    RithmicRealtime(RithmicRealtimeEvent),
    RithmicCatalog(RithmicCatalogEvent),
    HyperliquidRealtime(HyperliquidRealtimeEvent),
    HyperliquidCatalog(HyperliquidCatalogEvent),
}

mod runtime;
use runtime::join_runtime_workers;

mod coordinator;
use coordinator::{Coordinator, OwnedCoordinatorChannels, spawn_coordinator};

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

fn configured_engine() -> Result<MarketEngine, String> {
    let mut engine = MarketEngine::new(MarketEngineConfig {
        maximum_consumers: NonZeroUsize::new(MAXIMUM_CONSUMERS).unwrap_or(NonZeroUsize::MIN),
        maximum_series: NonZeroUsize::new(MAXIMUM_SERIES).unwrap_or(NonZeroUsize::MIN),
        // SeriesStore performs no eager allocation for this logical capacity;
        // runtime compaction keeps each series at/below its high watermark.
        maximum_bars: NonZeroUsize::new(MAXIMUM_STORED_BARS).unwrap_or(NonZeroUsize::MIN),
    });
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
    engine
        .register_provider(
            "hyperliquid".to_string(),
            ProviderConfig {
                account_id: HYPERLIQUID_PUBLIC_ACCOUNT_ID.to_string(),
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
    engine_install_failure_stage, fail_waiters, publish_state, series_state, series_state_payload,
    series_update_message,
};

mod instrument_selection;
use instrument_selection::{
    id, try_send_hyperliquid_catalog, try_send_rithmic_catalog, validate_provider_instrument,
    validate_provider_search, validate_provider_selection,
};

mod history;
use history::spawn_history_worker;

#[cfg(test)]
pub(crate) mod tests;
