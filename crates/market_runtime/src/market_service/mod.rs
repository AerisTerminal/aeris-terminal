//! Single-owner in-process market runtime and provider workers.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
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
use aeris_contracts::{
    EngineFaultCode, FailureStage, InstallProviderInstrument, ProviderCatalogRejected,
    ProviderCatalogRejectionReason, ProviderCatalogSymbol, ProviderConnectionState,
    ProviderInstrumentSearchResult, ProviderPresentationDescriptor, ProviderState,
    SearchProviderInstruments, SelectProviderInstrument, SeriesLoadState,
};
use aeris_hyperliquid_market_adapter::hyperliquid_interval_for_period;
use aeris_market_data::{
    AggressorSide, AggressorTradeVolumes, BarPeriod, BarSeriesKey, DepthSnapshot, MarketBar,
    MarketTrade, OrderBook, OrderBookApplyOutcome, OrderBookState as CanonicalOrderBookState,
    TopOfBookQuote, merge_live_candle,
};
use aeris_market_engine::{
    ClientId, ConsumerId, ConsumerIdentity, ConsumerResourceClass, EngineError, GenerationId,
    MarketEngine, MarketEngineConfig, MarketStream, ProviderCapabilities, ProviderConfig,
    ProviderGeneration, ProviderHealth, ProviderRequest, SeriesSnapshot, SeriesTailOperation,
    StreamRequirements, Viewport, WorkspaceId,
};
use aeris_provider_history::HistoryRange;
use aeris_rithmic_protocol_adapter::{
    RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID, RithmicCalendarPeriod, RithmicExchangeCalendar,
};

use crate::hyperliquid_display_depth::{
    HyperliquidDisplayDepthControl, HyperliquidDisplayDepthEvent,
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
/// First history page for a newly selected series, served in one provider round trip.
/// Charts open on their latest ~600 bars; the remaining 1,000 bars are indicator warm-up
/// (five periods of a 200-bar average), so studies are converged at the visible left edge
/// instead of filling in after a second viewport-driven fetch.
const INITIAL_HISTORY_BARS: usize = 1_600;
const VIEWPORT_LIVE_TAIL_RESERVE: usize = 512;
const MAXIMUM_CATALOG_INSTRUMENTS: usize = 4_096;
const MAXIMUM_CATALOG_FIELD_BYTES: usize = 256;
const LIVE_BUFFER_CAPACITY: usize = 2_048;
const MAXIMUM_TRADE_BAR_OVERLAY: usize = 16_384;
const LIVE_HANDOFF_HISTORY_BARS: usize = VIEWPORT_LIVE_TAIL_RESERVE + 1;
const MAXIMUM_RECENT_LADDER_TRADES: usize = 65_536;

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
    /// Calculation lane was isolated after an execution deadline or worker failure.
    pub study_execution_failed: bool,
    pub providers: Vec<ProviderState>,
    pub retained_series: usize,
    pub retained_bars: usize,
    pub approximate_series_bytes: usize,
}

struct MarketRuntime {
    shutdown: Arc<AtomicBool>,
    broker_api: Arc<tastytrade::BrokerApi>,
    provider_presentations: Vec<ProviderPresentationDescriptor>,
    provider_search_preparers: BTreeMap<&'static str, ProviderSearchPreparer>,
    broker_authorization: broker_authorization::BrokerAuthorization,
    active_provider_workers: Arc<Mutex<BTreeSet<String>>>,
    workers: Mutex<Option<Vec<thread::JoinHandle<()>>>>,
}

impl Drop for MarketRuntime {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.broker_api.cancel_searches();
    }
}

// 0 queued, 1 started, 2 cancelled. The start/cancel race has one winner.
struct RequestState(AtomicU8);
impl RequestState {
    fn start(&self) -> bool {
        self.0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    fn cancel(&self) -> bool {
        self.0
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

enum Command {
    Request(Box<Command>, Arc<RequestState>),
    /// Provider workers use this zero-payload control only to interrupt the
    /// coordinator's bounded idle wait after publishing an event. The actual
    /// provider event remains in its dedicated bounded lane and is drained at
    /// the top of the next coordinator iteration.
    ProviderWake,
    Status(Reply<MarketServiceStatus>),
    BrokerAuthorizationChanged(bool, Reply<()>),
    AvailableStreams(String, StreamRequirements, Reply<StreamRequirements>),
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
    slots: Arc<BTreeMap<&'static str, ProviderWakeSlot>>,
    pending: Arc<AtomicBool>,
    drain_cursor: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Default)]
struct ProviderWakeSlot {
    overflow: AtomicU64,
    pending_overflow: AtomicU64,
    catalog_overflow: AtomicBool,
}

impl ProviderCoordinatorWake {
    fn new(
        commands: SyncSender<Command>,
        provider_ids: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        Self {
            commands,
            pending: Arc::new(AtomicBool::new(false)),
            slots: Arc::new(
                provider_ids
                    .into_iter()
                    .map(|id| (id, ProviderWakeSlot::default()))
                    .collect(),
            ),
            drain_cursor: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        let (commands, _receiver) = mpsc::sync_channel(1);
        Self::new(commands, ["rithmic", "hyperliquid", "tastytrade"])
    }

    pub(crate) fn report_overflow(&self, provider: &str, generation: u64) {
        if let Some(slot) = self.slots.get(provider) {
            slot.overflow.fetch_max(generation, Ordering::AcqRel);
            slot.pending_overflow
                .fetch_max(generation, Ordering::AcqRel);
        }
        self.notify();
    }

    pub(crate) fn overflowed(&self, provider: &str, generation: u64) -> bool {
        generation != 0
            && self
                .slots
                .get(provider)
                .is_some_and(|slot| slot.overflow.load(Ordering::Acquire) >= generation)
    }

    pub(crate) fn notify(&self) {
        if self.pending.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.commands.try_send(Command::ProviderWake).is_err() {
            self.pending.store(false, Ordering::Release);
        }
    }
}

/// Catalog responses must never block the session owner. Saturation rejects
/// outstanding catalog requests through a fixed out-of-band flag.
pub(crate) struct CatalogPublisher<T> {
    events: SyncSender<T>,
    provider: &'static str,
    wake: ProviderCoordinatorWake,
}
impl<T> CatalogPublisher<T> {
    pub(crate) fn new(
        events: SyncSender<T>,
        provider: &'static str,
        wake: ProviderCoordinatorWake,
    ) -> Self {
        Self {
            events,
            provider,
            wake,
        }
    }
    pub(crate) fn send(&self, event: T) -> Result<(), mpsc::SendError<T>> {
        match self.events.try_send(event) {
            Ok(()) => {
                self.wake.notify();
                Ok(())
            }
            Err(TrySendError::Full(event)) => {
                if let Some(slot) = self.wake.slots.get(self.provider) {
                    slot.catalog_overflow.store(true, Ordering::Release);
                }
                self.wake.notify();
                Err(mpsc::SendError(event))
            }
            Err(TrySendError::Disconnected(event)) => Err(mpsc::SendError(event)),
        }
    }
}

#[derive(Clone)]
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
    /// The provider cannot serve any earlier candles for this series.
    backwards_exhausted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProviderInstrumentDemand {
    instrument: InstallProviderInstrument,
    streams: StreamRequirements,
    alert_trades: bool,
    alert_instrument: Option<InstallProviderInstrument>,
    display_depth: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ProviderDemand {
    instruments: Vec<ProviderInstrumentDemand>,
    candle_series: Vec<(BarSeriesKey, StreamRequirements, InstallProviderInstrument)>,
    explicit_trade_ids: BTreeSet<String>,
    missing_catalog: bool,
}

enum ProviderControl {
    Demand(ProviderDemand),
    Stop,
    AuthorizationChanged(bool),
    Recover(u64),
}

impl ProviderDemand {
    fn rithmic_wire(&self) -> Result<RithmicRealtimeDemand, String> {
        if self.missing_catalog {
            return Err("Rithmic instrument is not installed".to_string());
        }
        Ok(RithmicRealtimeDemand {
            instruments: self
                .instruments
                .iter()
                .filter_map(|requested| {
                    let trades = requested.alert_trades
                        || requested.streams.contains(MarketStream::Bars)
                        || requested.streams.contains(MarketStream::Trades)
                        || requested.streams.contains(MarketStream::Depth);
                    let quotes = requested.streams.contains(MarketStream::Quotes);
                    let order_book = requested.streams.contains(MarketStream::Depth);
                    (trades || quotes || order_book).then_some(RithmicInstrumentDemand {
                        instrument: requested.instrument.clone(),
                        trades,
                        quotes,
                        order_book,
                    })
                })
                .collect(),
        })
    }

    fn tastytrade_wire(&self) -> tastytrade::Demand {
        let mut wire = tastytrade::Demand {
            series: self
                .candle_series
                .iter()
                .map(|(series, _, instrument)| (series.clone(), instrument.clone()))
                .collect(),
            ..tastytrade::Demand::default()
        };
        for requested in &self.instruments {
            if self
                .explicit_trade_ids
                .contains(&requested.instrument.instrument_id)
            {
                wire.tape_instruments.push(requested.instrument.clone());
            }
            wire.instruments.push(requested.instrument.clone());
        }
        wire
    }

    fn hyperliquid_wire(&self) -> HyperliquidDemand {
        let mut candles = BTreeSet::new();
        let mut trades = BTreeSet::new();
        let mut quotes = BTreeSet::new();
        let mut books = BTreeSet::new();
        for (series, streams, instrument) in &self.candle_series {
            let (Ok(price_scale), Ok(quantity_scale), Ok(interval)) = (
                u8::try_from(instrument.price_scale),
                u8::try_from(instrument.quantity_scale),
                hyperliquid_interval_for_period(series.period),
            ) else {
                continue;
            };
            let mapping = HyperliquidInstrumentDemand {
                wire_coin: instrument.provider_symbol.clone(),
                instrument_id: series.instrument_id.clone(),
                entitlement_id: series.entitlement_id.clone(),
                price_scale,
                quantity_scale,
            };
            if streams.contains(MarketStream::Bars) {
                candles.insert(HyperliquidCandleDemand {
                    instrument: mapping.clone(),
                    interval: interval.to_string(),
                });
            }
            if streams.contains(MarketStream::Trades) {
                trades.insert(mapping.clone());
            }
            if streams.contains(MarketStream::Quotes) || streams.contains(MarketStream::Depth) {
                quotes.insert(mapping.clone());
            }
            if streams.contains(MarketStream::Depth) {
                books.insert(mapping);
            }
        }
        for requested in self.instruments.iter().filter(|item| item.alert_trades) {
            let Some(instrument) = requested.alert_instrument.as_ref() else {
                continue;
            };
            let (Ok(price_scale), Ok(quantity_scale)) = (
                u8::try_from(instrument.price_scale),
                u8::try_from(instrument.quantity_scale),
            ) else {
                continue;
            };
            trades.insert(HyperliquidInstrumentDemand {
                wire_coin: instrument.provider_symbol.clone(),
                instrument_id: instrument.instrument_id.clone(),
                entitlement_id: instrument.entitlement_id.clone(),
                price_scale,
                quantity_scale,
            });
        }
        HyperliquidDemand {
            candles: candles.into_iter().collect(),
            trades: trades.into_iter().collect(),
            quotes: quotes.into_iter().collect(),
            books: books.into_iter().collect(),
        }
    }
}

#[derive(Default)]
struct ProviderSessionSlot {
    accepted: Option<ProviderDemand>,
    pending: Option<ProviderDemand>,
    stop_pending: Option<ProviderGeneration>,
    engaged: bool,
    demand_dirty: bool,
    authorization: Option<bool>,
    suspended: bool,
    generation_floor: u64,
    recovery: Option<u64>,
    catalog_degraded: Option<ProviderGeneration>,
}

/// Everything one Hyperliquid series needs to close its history/live seam.
struct CandleHandoffSeed<'a> {
    price_scale: u8,
    quantity_scale: u8,
    /// Periods the provider closed, already installed as canonical history.
    bars: &'a [MarketBar],
    /// The period the provider caught open, held live rather than in history.
    forming: Option<FormingBar>,
}

/// Everything one trade-built series needs to close its history/live seam.
struct TradeHandoffSeed<'a> {
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
    /// Provider-observed trades already inside the open candle, when known.
    /// Tick cadence requires this to resume a partial bundle; clock-driven
    /// cadence uses it to reconcile an independent provider candle stream.
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
    trade_tape: Option<crate::MarketTradeTapeSnapshot>,
    delta_divergence: Option<MarketRuntimeEvent>,
    price_alerts: VecDeque<MarketRuntimeEvent>,
    catalog_search: Option<MarketRuntimeEvent>,
    catalog_selection: Option<MarketRuntimeEvent>,
}

struct ProviderOrderBook {
    instrument: InstallProviderInstrument,
    trade_continuity: TradeContinuity,
    book: OrderBook,
    top_of_book: Option<TopOfBookQuote>,
    recent_trades: VecDeque<crate::RetainedMarketTrade>,
    traded_volumes: BTreeMap<i64, AggressorTradeVolumes>,
    trade_session_generation: u64,
    last_trade_source_sequence: u64,
    next_trade_ingestion_ordinal: u64,
    trade_tape_revision: u64,
    trade_tape_rewrite_generation: u64,
    indexed_trade_ordinals: BTreeMap<String, u64>,
    trade_tape_dirty: bool,
    retention_clock_unix_nanos: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndexedTradeKind {
    New,
    Correction,
    Cancel,
}

#[derive(Clone)]
struct IndexedTradeMutation {
    index: String,
    kind: IndexedTradeKind,
    source_sequence: u64,
    trade: Option<MarketTrade>,
}

#[derive(Clone)]
enum TradeLiveUpdate {
    Append(MarketTrade),
    Indexed(IndexedTradeMutation),
}

mod provider_event;
mod realtime;
use provider_event::{
    ProviderCandle, ProviderDisconnect, ProviderEvent, ProviderEventKind, ProviderTradeBatch,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiveHistoryState {
    AwaitingHistory,
    Ready,
    Reseeding,
}

/// Shared canonical handoff for a series delivered as whole provider candles.
///
/// Unlike the trade-built Rithmic handoff, provider candles arrive whole:
/// history seeds closed bars plus the open period, and live replacements
/// merge by candle-open timestamp with exactly one forming candle. Sequence
/// ingestion sequence numbers stay runtime-owned so a redelivered update can never look new.
struct CandleLiveHandoff {
    series: BarSeriesKey,
    generation: ProviderGeneration,
    gap_policy: CandleGapPolicy,
    wire_coin: String,
    interval: String,
    price_scale: u8,
    quantity_scale: u8,
    bars: Vec<MarketBar>,
    forming: Option<MarketBar>,
    /// Live updates that arrived before history seeded the seam, bounded.
    buffered: VecDeque<MarketBar>,
    pending_publications: VecDeque<MarketBar>,
    connected: bool,
    history_state: LiveHistoryState,
    dirty: bool,
}

struct TradeLiveHandoff {
    series: BarSeriesKey,
    generation: ProviderGeneration,
    cadence: TradeLiveCadence,
    gap_policy: CandleGapPolicy,
    candle_symbol: Option<String>,
    price_scale: u8,
    quantity_scale: u8,
    bars: Vec<MarketBar>,
    buffered: VecDeque<TradeLiveUpdate>,
    pending_publications: VecDeque<MarketBar>,
    pending_corrections: VecDeque<MarketBar>,
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
    overlay_base: Option<MarketBar>,
    indexed_overlay: BTreeMap<String, MarketTrade>,
    provider_candle_count: Option<u64>,
    provider_candle_timestamp: Option<i64>,
}

enum TradeLiveCadence {
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
    CompletedCorrection(MarketBar),
}

/// A live bar that neither continues the published tail nor revises it in place
/// cannot be appended, so the whole series is republished instead.
trait HistorySource: Send + 'static {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String>;
}

struct LiveRithmicHistory;

#[derive(Default)]
struct LiveHyperliquidHistory(aeris_hyperliquid_market_adapter::HyperliquidHttpClient);

#[derive(Clone, Copy)]
struct ProviderDescriptor {
    id: &'static str,
    presentation: &'static ProviderPresentationDescriptor,
    account_id: &'static str,
    capabilities: ProviderCapabilities,
    reconnect_delay: Duration,
    gap_policy: CandleGapPolicy,
    history_source: HistorySourceKind,
    connection_kind: ProviderConnectionKind,
    recovery_policy: ProviderRecoveryPolicy,
    idle_stop_policy: IdleStopPolicy,
    alert_demand_update: AlertDemandUpdate,
    start: ProviderRuntimeStarter,
    flush_demand: for<'a> fn(&mut Coordinator<'a>),
    prepare_search: Option<ProviderSearchPreparer>,
    live_model: LiveModel,
    supported_period: fn(BarPeriod) -> Result<(), String>,
    alert_overrides_instrument: bool,
    overflow_recovery_detail: &'static str,
    history_range_policy: HistoryRangePolicy,
    trade_continuity: TradeContinuity,
    candle_requires_connected: bool,
    candle_correction_detail: &'static str,
    candle_wire_interval: Option<fn(BarPeriod) -> Result<String, String>>,
    candle_demand_policy: CandleDemandPolicy,
    trade_demand_policy: TradeDemandPolicy,
    instrument_missing_detail: &'static str,
}

enum SeriesLive {
    TradeBuilt(TradeLiveHandoff),
    ProviderCandles(CandleLiveHandoff),
}

#[derive(Default)]
struct SeriesLiveMap(BTreeMap<BarSeriesKey, SeriesLive>);

impl SeriesLiveMap {
    fn trade_contains_key(&self, series: &BarSeriesKey) -> bool {
        self.trade(series).is_some()
    }

    fn candle_contains_key(&self, series: &BarSeriesKey) -> bool {
        self.candle(series).is_some()
    }

    #[cfg(test)]
    fn trades_are_empty(&self) -> bool {
        self.trade_keys().next().is_none()
    }

    #[cfg(test)]
    fn candles_are_empty(&self) -> bool {
        self.candle_keys().next().is_none()
    }

    fn trade(&self, series: &BarSeriesKey) -> Option<&TradeLiveHandoff> {
        match self.0.get(series)? {
            SeriesLive::TradeBuilt(live) => Some(live),
            SeriesLive::ProviderCandles(_) => None,
        }
    }

    fn trade_mut(&mut self, series: &BarSeriesKey) -> Option<&mut TradeLiveHandoff> {
        match self.0.get_mut(series)? {
            SeriesLive::TradeBuilt(live) => Some(live),
            SeriesLive::ProviderCandles(_) => None,
        }
    }

    fn candle(&self, series: &BarSeriesKey) -> Option<&CandleLiveHandoff> {
        match self.0.get(series)? {
            SeriesLive::ProviderCandles(live) => Some(live),
            SeriesLive::TradeBuilt(_) => None,
        }
    }

    fn candle_mut(&mut self, series: &BarSeriesKey) -> Option<&mut CandleLiveHandoff> {
        match self.0.get_mut(series)? {
            SeriesLive::ProviderCandles(live) => Some(live),
            SeriesLive::TradeBuilt(_) => None,
        }
    }

    fn insert_trade(&mut self, series: BarSeriesKey, live: TradeLiveHandoff) {
        self.0.insert(series, SeriesLive::TradeBuilt(live));
    }

    fn insert_candle(&mut self, series: BarSeriesKey, live: CandleLiveHandoff) {
        self.0.insert(series, SeriesLive::ProviderCandles(live));
    }

    fn trade_keys(&self) -> impl Iterator<Item = &BarSeriesKey> {
        self.0.iter().filter_map(|(series, live)| {
            matches!(live, SeriesLive::TradeBuilt(_)).then_some(series)
        })
    }

    fn candle_keys(&self) -> impl Iterator<Item = &BarSeriesKey> {
        self.0.iter().filter_map(|(series, live)| {
            matches!(live, SeriesLive::ProviderCandles(_)).then_some(series)
        })
    }

    fn candle_iter(&self) -> impl Iterator<Item = (&BarSeriesKey, &CandleLiveHandoff)> {
        self.0.iter().filter_map(|(series, live)| match live {
            SeriesLive::ProviderCandles(live) => Some((series, live)),
            SeriesLive::TradeBuilt(_) => None,
        })
    }

    fn candle_iter_mut(&mut self) -> impl Iterator<Item = (&BarSeriesKey, &mut CandleLiveHandoff)> {
        self.0.iter_mut().filter_map(|(series, live)| match live {
            SeriesLive::ProviderCandles(live) => Some((series, live)),
            SeriesLive::TradeBuilt(_) => None,
        })
    }

    fn trade_values_mut(&mut self) -> impl Iterator<Item = &mut TradeLiveHandoff> {
        self.0.values_mut().filter_map(|live| match live {
            SeriesLive::TradeBuilt(live) => Some(live),
            SeriesLive::ProviderCandles(_) => None,
        })
    }

    fn candle_values_mut(&mut self) -> impl Iterator<Item = &mut CandleLiveHandoff> {
        self.0.values_mut().filter_map(|live| match live {
            SeriesLive::ProviderCandles(live) => Some(live),
            SeriesLive::TradeBuilt(_) => None,
        })
    }

    fn trade_iter_mut(&mut self) -> impl Iterator<Item = (&BarSeriesKey, &mut TradeLiveHandoff)> {
        self.0.iter_mut().filter_map(|(series, live)| match live {
            SeriesLive::TradeBuilt(live) => Some((series, live)),
            SeriesLive::ProviderCandles(_) => None,
        })
    }

    fn retain_trades(
        &mut self,
        mut keep: impl FnMut(&BarSeriesKey, &mut TradeLiveHandoff) -> bool,
    ) {
        self.0.retain(|series, live| match live {
            SeriesLive::TradeBuilt(live) => keep(series, live),
            SeriesLive::ProviderCandles(_) => true,
        });
    }

    fn retain_candles(
        &mut self,
        mut keep: impl FnMut(&BarSeriesKey, &mut CandleLiveHandoff) -> bool,
    ) {
        self.0.retain(|series, live| match live {
            SeriesLive::ProviderCandles(live) => keep(series, live),
            SeriesLive::TradeBuilt(_) => true,
        });
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiveModel {
    TradeBuilt,
    ProviderCandles,
}

type ProviderRuntimeStarter = fn(
    ProviderRuntimeSpec,
    &SyncSender<Command>,
    ProviderCoordinatorWake,
    &MarketEngine,
    &Arc<Mutex<BTreeSet<String>>>,
    &Arc<tastytrade::BrokerApi>,
) -> Result<ProviderRuntimeRecord, String>;

type ProviderSearchPreparer = fn(&tastytrade::BrokerApi, u64, u64) -> Result<(), String>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CandleGapPolicy {
    Contiguous,
    SessionGapsAllowed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistoryRangePolicy {
    Bounded,
    FromTimeOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistorySourceKind {
    DedicatedWorker(&'static str),
    ProviderSession,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderConnectionKind {
    NativeCredentials,
    Public,
    BrokerCapability,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderRecoveryPolicy {
    CoordinatorReissuesDemand,
    WorkerReconcilesDemand,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IdleStopPolicy {
    Coordinator,
    WorkerManaged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AlertDemandUpdate {
    Immediate,
    MarkDirty,
    WorkerManaged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TradeContinuity {
    Sequence,
    Indexed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CandleDemandPolicy {
    SessionManaged,
    ReconcileAfterSelection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TradeDemandPolicy {
    Immediate,
    SessionManaged,
}

const RITHMIC_PRESENTATION: ProviderPresentationDescriptor = ProviderPresentationDescriptor {
    id: "rithmic",
    display_name: "Rithmic",
    chart_interval_labels: &[
        "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "8h", "12h", "1D", "1W", "1M",
    ],
    default_listing: "MNQ",
    search_hint: "Search Rithmic symbols",
    logo_key: "rithmic",
    catalog_symbol: ProviderCatalogSymbol::ProviderSymbol,
    selection_entitlement_id: "crypto_public_realtime",
    catalog_refresh_on_startup: false,
    depth_available: true,
    connection_kind: aeris_contracts::ProviderConnectionKind::Credentials,
};

const RITHMIC_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: "rithmic",
    presentation: &RITHMIC_PRESENTATION,
    account_id: RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID,
    capabilities: ProviderCapabilities {
        historical_bars: true,
        realtime_bars: true,
        streams: StreamRequirements::BARS
            .with(MarketStream::Trades)
            .with(MarketStream::Quotes)
            .with(MarketStream::Depth),
    },
    reconnect_delay: PROVIDER_RECONNECT_DELAY,
    gap_policy: CandleGapPolicy::Contiguous,
    history_source: HistorySourceKind::DedicatedWorker("aeris-rithmic-history"),
    connection_kind: ProviderConnectionKind::NativeCredentials,
    recovery_policy: ProviderRecoveryPolicy::CoordinatorReissuesDemand,
    idle_stop_policy: IdleStopPolicy::Coordinator,
    alert_demand_update: AlertDemandUpdate::Immediate,
    start: ProviderRuntimeRegistry::start_rithmic_runtime,
    flush_demand: flush_rithmic_provider_demand,
    prepare_search: None,
    live_model: LiveModel::TradeBuilt,
    supported_period: rithmic_supported_period,
    alert_overrides_instrument: false,
    overflow_recovery_detail: "Local market event queue overflow; repairing continuity",
    history_range_policy: HistoryRangePolicy::Bounded,
    trade_continuity: TradeContinuity::Sequence,
    candle_requires_connected: false,
    candle_correction_detail: "Provider candle correction requires covering history",
    candle_wire_interval: None,
    candle_demand_policy: CandleDemandPolicy::SessionManaged,
    trade_demand_policy: TradeDemandPolicy::Immediate,
    instrument_missing_detail: "Rithmic instrument is not installed",
};

const HYPERLIQUID_PRESENTATION: ProviderPresentationDescriptor = ProviderPresentationDescriptor {
    id: "hyperliquid",
    display_name: "Hyperliquid",
    chart_interval_labels: &[
        "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "8h", "12h", "1D", "3D", "1W", "1M",
    ],
    default_listing: "",
    search_hint: "Search Hyperliquid markets",
    logo_key: "hyperliquid",
    catalog_symbol: ProviderCatalogSymbol::ProviderSymbol,
    selection_entitlement_id: "hyperliquid-public",
    catalog_refresh_on_startup: true,
    depth_available: true,
    connection_kind: aeris_contracts::ProviderConnectionKind::Public,
};

const HYPERLIQUID_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: "hyperliquid",
    presentation: &HYPERLIQUID_PRESENTATION,
    account_id: HYPERLIQUID_PUBLIC_ACCOUNT_ID,
    capabilities: RITHMIC_DESCRIPTOR.capabilities,
    reconnect_delay: PROVIDER_RECONNECT_DELAY,
    gap_policy: CandleGapPolicy::Contiguous,
    history_source: HistorySourceKind::DedicatedWorker("aeris-hyperliquid-history"),
    connection_kind: ProviderConnectionKind::Public,
    recovery_policy: ProviderRecoveryPolicy::WorkerReconcilesDemand,
    idle_stop_policy: IdleStopPolicy::Coordinator,
    alert_demand_update: AlertDemandUpdate::MarkDirty,
    start: ProviderRuntimeRegistry::start_hyperliquid_runtime,
    flush_demand: flush_hyperliquid_provider_demand,
    prepare_search: None,
    live_model: LiveModel::ProviderCandles,
    supported_period: hyperliquid_supported_period,
    alert_overrides_instrument: false,
    overflow_recovery_detail: "Local market event queue overflow; repairing continuity",
    history_range_policy: HistoryRangePolicy::Bounded,
    trade_continuity: TradeContinuity::Sequence,
    candle_requires_connected: true,
    candle_correction_detail: "Hyperliquid candle replacement requires covering history",
    candle_wire_interval: Some(hyperliquid_candle_interval),
    candle_demand_policy: CandleDemandPolicy::ReconcileAfterSelection,
    trade_demand_policy: TradeDemandPolicy::SessionManaged,
    instrument_missing_detail: "Hyperliquid instrument is not installed",
};

fn hyperliquid_candle_interval(period: BarPeriod) -> Result<String, String> {
    hyperliquid_interval_for_period(period)
        .map(str::to_owned)
        .map_err(|_| "Hyperliquid history is unavailable for this interval".to_string())
}

const BUILT_IN_PROVIDER_DESCRIPTORS: &[ProviderDescriptor] = &[
    RITHMIC_DESCRIPTOR,
    HYPERLIQUID_DESCRIPTOR,
    tastytrade::DESCRIPTOR,
];

/// Built-in provider presentation metadata for provider-neutral desktop UI.
#[must_use]
pub fn built_in_provider_presentations() -> &'static [ProviderPresentationDescriptor] {
    // Keep this projection in the runtime registry so desktop presentation does
    // not maintain a second provider metadata table.
    static PRESENTATIONS: std::sync::OnceLock<Vec<ProviderPresentationDescriptor>> =
        std::sync::OnceLock::new();
    PRESENTATIONS
        .get_or_init(|| {
            BUILT_IN_PROVIDER_DESCRIPTORS
                .iter()
                .map(|descriptor| *descriptor.presentation)
                .collect()
        })
        .as_slice()
}

fn flush_rithmic_provider_demand(coordinator: &mut Coordinator<'_>) {
    coordinator.flush_rithmic_demand();
}

fn flush_hyperliquid_provider_demand(coordinator: &mut Coordinator<'_>) {
    coordinator.flush_hyperliquid_demand();
}

fn rithmic_supported_period(period: BarPeriod) -> Result<(), String> {
    crate::rithmic_history::chart_interval(period).map(|_| ())
}

fn hyperliquid_supported_period(period: BarPeriod) -> Result<(), String> {
    hyperliquid_interval_for_period(period)
        .map(|_| ())
        .map_err(|_| "Provider history is unavailable for this interval".to_string())
}

fn tastytrade_supported_period(period: BarPeriod) -> Result<(), String> {
    tastytrade::candle_period(period).map(|_| ())
}

struct ProviderRuntimeSpec {
    descriptor: ProviderDescriptor,
    history: Option<Box<dyn HistorySource>>,
    realtime: ProviderRealtimeSpec,
}

struct ProviderRealtimeSpec {
    enabled: bool,
}

impl ProviderRuntimeSpec {
    fn tastytrade() -> Self {
        Self {
            descriptor: tastytrade::DESCRIPTOR,
            history: None,
            realtime: ProviderRealtimeSpec { enabled: true },
        }
    }
    fn rithmic(history: Box<dyn HistorySource>, enabled: bool) -> Self {
        Self {
            descriptor: RITHMIC_DESCRIPTOR,
            history: Some(history),
            realtime: ProviderRealtimeSpec { enabled },
        }
    }

    fn hyperliquid(history: Box<dyn HistorySource>, enabled: bool) -> Self {
        Self {
            descriptor: HYPERLIQUID_DESCRIPTOR,
            history: Some(history),
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
                    "Market data connection interrupted; reconnecting automatically.".to_string()
                })
            })
    }
}

struct ProviderRealtimeChannels {
    enabled: bool,
    channels: ProviderRealtimeChannelSet,
}

enum ProviderRealtimeChannelSet {
    Tastytrade {
        controls: SyncSender<tastytrade::RealtimeControl>,
        events: Receiver<tastytrade::RealtimeEvent>,
    },
    Rithmic {
        controls: SyncSender<RithmicRealtimeControl>,
        events: Receiver<RithmicRealtimeEvent>,
    },
    Hyperliquid {
        controls: SyncSender<HyperliquidRealtimeControl>,
        events: Receiver<HyperliquidRealtimeEvent>,
        display_controls: SyncSender<HyperliquidDisplayDepthControl>,
        display_events: Receiver<HyperliquidDisplayDepthEvent>,
    },
}

struct ProviderCatalogChannels {
    enabled: bool,
    channels: ProviderCatalogChannelSet,
}

enum ProviderCatalogChannelSet {
    Tastytrade {
        controls: SyncSender<tastytrade::CatalogControl>,
        events: Receiver<tastytrade::CatalogEvent>,
    },
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
    descriptor: ProviderDescriptor,
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
    wake: ProviderCoordinatorWake,
    records: BTreeMap<&'static str, ProviderRuntimeRecord>,
    order: Vec<&'static str>,
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
    wake: Option<&'a ProviderCoordinatorWake>,
    records: BTreeMap<&'static str, ProviderDispatchRecord<'a>>,
    lanes: Vec<ProviderEventLane>,
}

#[derive(Clone, Copy)]
enum ProviderEventLane {
    Realtime(&'static str),
    Catalog(&'static str),
    DisplayDepth(&'static str),
}

struct ProviderDispatchRecord<'a> {
    descriptor: ProviderDescriptor,
    history: &'a SyncSender<HistoryRequest>,
    lifecycle: Option<&'a ProviderRuntimeLifecycle>,
    realtime: ProviderRealtimeDispatch<'a>,
    catalog: ProviderCatalogDispatch<'a>,
}

enum ProviderRealtimeDispatch<'a> {
    Tastytrade {
        controls: &'a SyncSender<tastytrade::RealtimeControl>,
        events: &'a Receiver<tastytrade::RealtimeEvent>,
    },
    Rithmic {
        controls: &'a SyncSender<RithmicRealtimeControl>,
        events: &'a Receiver<RithmicRealtimeEvent>,
    },
    Hyperliquid {
        controls: &'a SyncSender<HyperliquidRealtimeControl>,
        events: &'a Receiver<HyperliquidRealtimeEvent>,
        display_controls: &'a SyncSender<HyperliquidDisplayDepthControl>,
        display_events: &'a Receiver<HyperliquidDisplayDepthEvent>,
    },
    Disabled,
}

impl<'a> ProviderRealtimeDispatch<'a> {
    fn rithmic_controls(&self) -> Option<&'a SyncSender<RithmicRealtimeControl>> {
        match self {
            Self::Rithmic { controls, .. } => Some(controls),
            Self::Tastytrade { .. } | Self::Hyperliquid { .. } | Self::Disabled => None,
        }
    }

    fn hyperliquid_controls(&self) -> Option<&'a SyncSender<HyperliquidRealtimeControl>> {
        match self {
            Self::Hyperliquid { controls, .. } => Some(controls),
            Self::Tastytrade { .. } | Self::Rithmic { .. } | Self::Disabled => None,
        }
    }

    fn hyperliquid_display_controls(
        &self,
    ) -> Option<&'a SyncSender<HyperliquidDisplayDepthControl>> {
        match self {
            Self::Hyperliquid {
                display_controls, ..
            } => Some(display_controls),
            Self::Tastytrade { .. } | Self::Rithmic { .. } | Self::Disabled => None,
        }
    }
}

enum ProviderCatalogDispatch<'a> {
    Tastytrade {
        controls: &'a SyncSender<tastytrade::CatalogControl>,
        events: &'a Receiver<tastytrade::CatalogEvent>,
    },
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
    Realtime(ProviderEvent),
    DisplayDepth(&'static str, ProviderDisplayDepthEvent),
    Catalog(&'static str, ProviderCatalogEvent),
}

enum ProviderDisplayDepthEvent {
    Reset {
        display_generation: u64,
    },
    Snapshot {
        provider_generation: u64,
        display_generation: u64,
        snapshot: DepthSnapshot,
    },
}

impl From<HyperliquidDisplayDepthEvent> for ProviderDisplayDepthEvent {
    fn from(event: HyperliquidDisplayDepthEvent) -> Self {
        match event {
            HyperliquidDisplayDepthEvent::Reset { display_generation } => {
                Self::Reset { display_generation }
            }
            HyperliquidDisplayDepthEvent::Snapshot(display) => Self::Snapshot {
                provider_generation: display.provider_generation,
                display_generation: display.display_generation,
                snapshot: display.snapshot,
            },
        }
    }
}

enum ProviderCatalogEvent {
    SearchCompleted(ProviderInstrumentSearchResult),
    SearchPreliminary(ProviderInstrumentSearchResult),
    SelectionResolved {
        consumer_id: u64,
        command_generation: u64,
        instrument: InstallProviderInstrument,
    },
    Rejected {
        rejection: ProviderCatalogRejected,
        selection: bool,
    },
    RefreshFailed(String),
}

impl From<RithmicCatalogEvent> for ProviderCatalogEvent {
    fn from(event: RithmicCatalogEvent) -> Self {
        match event {
            RithmicCatalogEvent::SearchCompleted(result) => Self::SearchCompleted(result),
            RithmicCatalogEvent::SelectionResolved {
                consumer_id,
                command_generation,
                instrument,
            } => Self::SelectionResolved {
                consumer_id,
                command_generation,
                instrument,
            },
            RithmicCatalogEvent::Rejected {
                rejection,
                selection,
            } => Self::Rejected {
                rejection,
                selection,
            },
        }
    }
}

impl From<HyperliquidCatalogEvent> for ProviderCatalogEvent {
    fn from(event: HyperliquidCatalogEvent) -> Self {
        match event {
            HyperliquidCatalogEvent::SearchCompleted(result) => Self::SearchCompleted(result),
            HyperliquidCatalogEvent::SelectionResolved {
                consumer_id,
                command_generation,
                instrument,
            } => Self::SelectionResolved {
                consumer_id,
                command_generation,
                instrument,
            },
            HyperliquidCatalogEvent::Rejected {
                rejection,
                selection,
            } => Self::Rejected {
                rejection,
                selection,
            },
            HyperliquidCatalogEvent::RefreshFailed { detail } => Self::RefreshFailed(detail),
        }
    }
}

impl From<tastytrade::CatalogEvent> for ProviderCatalogEvent {
    fn from(event: tastytrade::CatalogEvent) -> Self {
        match event {
            tastytrade::CatalogEvent::Search(result) => Self::SearchCompleted(result),
            tastytrade::CatalogEvent::SearchPreliminary(result) => Self::SearchPreliminary(result),
            tastytrade::CatalogEvent::Selection {
                consumer_id,
                command_generation,
                instrument,
            } => Self::SelectionResolved {
                consumer_id,
                command_generation,
                instrument,
            },
            tastytrade::CatalogEvent::Rejected {
                rejection,
                selection,
            } => Self::Rejected {
                rejection,
                selection,
            },
        }
    }
}

#[derive(Clone, Copy)]
struct ProviderEventMetadata {
    provider: &'static str,
    generation: u64,
    reconnecting: bool,
    transport_rtt_nanos: Option<u64>,
}

impl ProviderRuntimeEvent {
    fn metadata(&self) -> Option<ProviderEventMetadata> {
        let (provider, generation, reconnecting, transport_rtt_nanos) = match self {
            Self::Realtime(event) => (
                event.provider,
                event.generation,
                matches!(
                    event.kind,
                    ProviderEventKind::Connecting
                        | ProviderEventKind::Recovering { .. }
                        | ProviderEventKind::Failed(..)
                        | ProviderEventKind::Disconnected(..)
                ),
                match event.kind {
                    ProviderEventKind::Heartbeat(rtt) => rtt,
                    _ => None,
                },
            ),
            Self::Catalog(..) | Self::DisplayDepth(..) => return None,
        };
        Some(ProviderEventMetadata {
            provider,
            generation,
            reconnecting,
            transport_rtt_nanos,
        })
    }
}

mod broker_authorization;
mod runtime;
mod tastytrade;
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

#[cfg(test)]
fn configured_engine() -> Result<MarketEngine, String> {
    configured_engine_from_descriptors(BUILT_IN_PROVIDER_DESCRIPTORS)
}

fn configured_engine_from_descriptors(
    descriptors: &[ProviderDescriptor],
) -> Result<MarketEngine, String> {
    let mut engine = MarketEngine::new(MarketEngineConfig {
        maximum_consumers: NonZeroUsize::new(MAXIMUM_CONSUMERS).unwrap_or(NonZeroUsize::MIN),
        maximum_series: NonZeroUsize::new(MAXIMUM_SERIES).unwrap_or(NonZeroUsize::MIN),
        // SeriesStore performs no eager allocation for this logical capacity;
        // runtime compaction keeps each series at/below its high watermark.
        maximum_bars: NonZeroUsize::new(MAXIMUM_STORED_BARS).unwrap_or(NonZeroUsize::MIN),
    });
    for descriptor in descriptors {
        engine
            .register_provider(
                descriptor.id.to_string(),
                ProviderConfig {
                    account_id: descriptor.account_id.to_string(),
                    capabilities: descriptor.capabilities,
                    reconnect_delay: descriptor.reconnect_delay,
                },
            )
            .map_err(|error| error.to_string())?;
    }
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
