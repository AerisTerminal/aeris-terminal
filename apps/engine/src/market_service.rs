//! Single-owner resident market coordinator and provider history/realtime workers.

use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet, VecDeque, btree_map::Entry},
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axiusflow_coinbase_market_adapter::{
    COINBASE_PUBLIC_ACCOUNT_ID, CanonicalTrade, CoinbaseBarAggregator, CoinbaseBarAggregatorConfig,
    CoinbaseConfig, CoinbaseHistoryCapabilityAdapter, CoinbaseInterval, CoinbaseSession,
    CoinbaseSpotProduct, ENTITLEMENT_CLASS, aggregate_coinbase_bars, coinbase_instrument_id,
    decode_history_bar,
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
    WorkspaceState, envelope,
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
    StreamRequirements, Viewport, WorkspaceId, decide_resource_policy,
};
use axiusflow_provider_history::{CoverageSnapshot, DataClass, HistoryPageRequest, HistoryRange};
use axiusflow_rithmic_protocol_adapter::{
    RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID, RithmicCalendarPeriod, RithmicExchangeCalendar,
};
use sysinfo::System;

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
const COORDINATOR_TICK: Duration = Duration::from_millis(16);
const LOCAL_HISTORY_READ_TIMEOUT: Duration = Duration::from_secs(2);
const EMPTY_REPAIR_RETRY_DELAY: Duration = Duration::from_secs(1);
const PROVIDER_RECONNECT_DELAY: Duration = Duration::from_millis(250);
const MAXIMUM_CONSUMERS: usize = 256;
const MAXIMUM_SERIES: usize = 128;
const HISTORY_BARS_PER_SERIES: usize = 32_768;
const VIEWPORT_LIVE_TAIL_RESERVE: usize = 512;
const VIEWPORT_BACKFILL_BARS: usize = HISTORY_BARS_PER_SERIES - VIEWPORT_LIVE_TAIL_RESERVE;
const MAXIMUM_STORED_BARS: usize = MAXIMUM_SERIES * (HISTORY_BARS_PER_SERIES + 1);
const MAXIMUM_CATALOG_INSTRUMENTS: usize = 4_096;
const MAXIMUM_CATALOG_FIELD_BYTES: usize = 256;
const MAXIMUM_PUBLISHED_DEPTH_LEVELS: usize = 20;
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
    realtime_stop: Arc<AtomicBool>,
    workers: Mutex<Option<Vec<thread::JoinHandle<()>>>>,
}

impl Drop for MarketRuntime {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.realtime_stop.store(true, Ordering::Release);
    }
}

enum Command {
    RestoreHotSet(Vec<WarmSeries>, Reply<()>),
    SetResourceMode(ResourceMode, Reply<()>),
    Status(Reply<MarketServiceStatus>),
    Attach(ClientId, Reply<()>),
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
    ViewportBackfill,
    SeamRepair,
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
    bars: Vec<MarketBar>,
    handoff_boundary_unix_nanos: Option<i64>,
    confirmed_empty: bool,
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
    Start(Vec<String>),
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
    demand_error: Option<envelope::Payload>,
    order_book: Option<envelope::Payload>,
    order_flow: Option<envelope::Payload>,
    catalog_search: Option<envelope::Payload>,
    catalog_selection: Option<envelope::Payload>,
}

impl ConsumerEvents {
    fn pop(&mut self) -> Option<envelope::Payload> {
        self.provider
            .take()
            .or_else(|| self.snapshot.take())
            .or_else(|| self.series_state.take())
            .or_else(|| self.demand_error.take())
            .or_else(|| self.order_book.take())
            .or_else(|| self.order_flow.take())
            .or_else(|| self.catalog_selection.take())
            .or_else(|| self.catalog_search.take())
    }

    fn publish_series_update(&mut self, update: envelope::Payload) {
        let envelope::Payload::SeriesUpdate(next) = update else {
            return;
        };
        if let Some(envelope::Payload::SeriesSnapshot(snapshot)) = self.snapshot.as_mut()
            && snapshot.consumer_id == next.consumer_id
            && snapshot.generation == next.generation
            && snapshot.series == next.series
            && let Some(bar) = next.bar.as_ref()
        {
            match snapshot.bars.last_mut() {
                Some(current) if current.source_sequence == bar.source_sequence => *current = *bar,
                Some(current)
                    if current.source_sequence.checked_add(1) == Some(bar.source_sequence) =>
                {
                    snapshot.bars.push(*bar);
                }
                _ => return,
            }
            snapshot.provider_generation = next.provider_generation;
            snapshot.publication_generation = next.publication_generation;
            snapshot.forming = next.forming;
            return;
        }
        self.snapshot = Some(envelope::Payload::SeriesUpdate(next));
    }
}

struct RithmicOrderBook {
    instrument: InstallProviderInstrument,
    book: OrderBook,
}

impl RithmicOrderBook {
    fn new(instrument: InstallProviderInstrument) -> Self {
        Self {
            instrument,
            book: OrderBook::new(
                NonZeroUsize::new(MAXIMUM_PUBLISHED_DEPTH_LEVELS).unwrap_or(NonZeroUsize::MIN),
            ),
        }
    }
}

struct LiveHandoff {
    generation: ProviderGeneration,
    aggregator: CoinbaseBarAggregator,
    buffered: VecDeque<CanonicalTrade>,
    connected: bool,
    history_ready: bool,
    dirty: bool,
    published: Option<PublishedTailState>,
    seam_repair: Option<HistoryRange>,
    viewport_position: ViewportPosition,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewportPosition {
    Live,
    Historical,
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

enum LiveSeriesPublication {
    Tail(MarketBar),
    Covering(Vec<MarketBar>),
}

#[derive(Clone, Copy)]
enum PublishedTailState {
    Covering(u64),
    Forming(u64),
}

fn requires_covering_publication(
    published: Option<PublishedTailState>,
    active_sequence: u64,
) -> bool {
    match published {
        Some(PublishedTailState::Covering(sequence)) => {
            sequence.checked_add(1) != Some(active_sequence)
        }
        Some(PublishedTailState::Forming(sequence)) => sequence != active_sequence,
        None => true,
    }
}

impl RithmicLiveHandoff {
    fn new(series: &BarSeriesKey, generation: ProviderGeneration, venue_id: &str) -> Option<Self> {
        let cadence = match series.period {
            BarPeriod::Tick { trades } => RithmicLiveCadence::Tick {
                trades,
                forming: trades,
            },
            BarPeriod::Time { seconds } => RithmicLiveCadence::Fixed {
                seconds: i64::from(seconds),
            },
            BarPeriod::Session { days } => RithmicLiveCadence::Fixed {
                seconds: i64::from(days) * 86_400,
            },
            BarPeriod::Week { weeks: 1 } => RithmicLiveCadence::Calendar {
                calendar: RithmicExchangeCalendar::for_venue(venue_id)?,
                period: RithmicCalendarPeriod::Week,
            },
            BarPeriod::Month { months: 1 } => RithmicLiveCadence::Calendar {
                calendar: RithmicExchangeCalendar::for_venue(venue_id)?,
                period: RithmicCalendarPeriod::Month,
            },
            BarPeriod::Week { .. } | BarPeriod::Month { .. } => return None,
        };
        Some(Self {
            series: series.clone(),
            generation,
            cadence,
            price_scale: 0,
            quantity_scale: 0,
            bars: Vec::new(),
            buffered: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            connected: false,
            history_ready: false,
            dirty: false,
            published: None,
            live_session_generation: None,
            last_trade_sequence: None,
            history_boundary_unix_nanos: i64::MIN,
        })
    }

    fn reset(&mut self, generation: ProviderGeneration) {
        self.generation = generation;
        self.bars.clear();
        self.buffered.clear();
        self.connected = false;
        self.history_ready = false;
        self.dirty = false;
        self.published = None;
        self.live_session_generation = None;
        self.last_trade_sequence = None;
        self.history_boundary_unix_nanos = i64::MIN;
        if let RithmicLiveCadence::Tick { trades, forming } = &mut self.cadence {
            *forming = *trades;
        }
    }

    fn seed(
        &mut self,
        price_scale: u8,
        quantity_scale: u8,
        bars: &[MarketBar],
        handoff_boundary_unix_nanos: Option<i64>,
    ) -> Result<(), String> {
        let last_bar_boundary = bars
            .last()
            .ok_or_else(|| "Rithmic live handoff requires history".to_string())?
            .exchange_timestamp_unix_nanos;
        self.price_scale = price_scale;
        self.quantity_scale = quantity_scale;
        self.bars = bars.to_vec();
        self.history_boundary_unix_nanos = handoff_boundary_unix_nanos
            .unwrap_or(last_bar_boundary)
            .max(last_bar_boundary);
        self.published = bars
            .last()
            .map(|bar| PublishedTailState::Covering(bar.source_sequence));
        self.live_session_generation = None;
        self.last_trade_sequence = None;
        if let RithmicLiveCadence::Tick { trades, forming } = &mut self.cadence {
            *forming = *trades;
        }
        let buffered = std::mem::take(&mut self.buffered);
        self.history_ready = true;
        for trade in &buffered {
            if self.apply_trade(trade)? {
                self.dirty = true;
            }
        }
        Ok(())
    }

    fn take_publication(&mut self) -> Option<LiveSeriesPublication> {
        if !self.connected || !self.history_ready || !self.dirty {
            return None;
        }
        let active = self.bars.last().copied()?;
        self.dirty = false;
        let covering = requires_covering_publication(self.published, active.source_sequence);
        self.published = Some(PublishedTailState::Forming(active.source_sequence));
        if covering {
            Some(LiveSeriesPublication::Covering(self.bars.clone()))
        } else {
            Some(LiveSeriesPublication::Tail(active))
        }
    }

    fn accept_trade(&mut self, trade: &MarketTrade) -> Result<(), String> {
        if self.history_ready {
            if self.apply_trade(trade)? {
                self.dirty = true;
            }
        } else if self.buffered.len() == LIVE_BUFFER_CAPACITY {
            return Err("Rithmic history/live buffer overflowed".to_string());
        } else {
            self.buffered.push_back(trade.clone());
        }
        Ok(())
    }

    fn apply_trade(&mut self, trade: &MarketTrade) -> Result<bool, String> {
        trade.validate().map_err(|error| error.to_string())?;
        if trade.metadata.provider_id != self.series.provider_id
            || trade.metadata.instrument_id != self.series.instrument_id
            || trade.metadata.entitlement_id != self.series.entitlement_id
        {
            return Ok(false);
        }
        if let Some(session_generation) = self.live_session_generation
            && session_generation != trade.metadata.session_generation
        {
            return Err("Rithmic live session generation changed".to_string());
        }
        if self
            .last_trade_sequence
            .is_some_and(|sequence| trade.metadata.source_sequence <= sequence)
        {
            return Ok(false);
        }
        let exchange_nanos = trade
            .metadata
            .timestamps
            .exchange_unix_nanos
            .ok_or_else(|| "Rithmic live trade has no exchange timestamp".to_string())?;
        if self.last_trade_sequence.is_none() && exchange_nanos <= self.history_boundary_unix_nanos
        {
            return Ok(false);
        }
        let Some(last) = self.bars.last().copied() else {
            return Err("Rithmic live handoff has no history".to_string());
        };
        let next = match self.cadence {
            RithmicLiveCadence::Fixed { seconds } => {
                let trade_seconds = exchange_nanos.div_euclid(1_000_000_000);
                if trade_seconds < last.exchange_timestamp_seconds {
                    return Ok(false);
                }
                let elapsed = trade_seconds - last.exchange_timestamp_seconds;
                if elapsed < seconds {
                    if self.last_trade_sequence.is_none() {
                        return Ok(false);
                    }
                    updated_rithmic_bar(last, trade, last.exchange_timestamp_unix_nanos)?
                } else {
                    let intervals = elapsed.div_euclid(seconds);
                    let start_seconds = last
                        .exchange_timestamp_seconds
                        .checked_add(intervals.saturating_mul(seconds))
                        .ok_or_else(|| "Rithmic live timestamp overflowed".to_string())?;
                    started_rithmic_bar(
                        last,
                        trade,
                        start_seconds
                            .checked_mul(1_000_000_000)
                            .ok_or_else(|| "Rithmic live timestamp overflowed".to_string())?,
                    )?
                }
            }
            RithmicLiveCadence::Tick {
                trades,
                ref mut forming,
            } => {
                if *forming >= trades {
                    *forming = 1;
                    started_rithmic_bar(last, trade, exchange_nanos)?
                } else {
                    *forming = forming.saturating_add(1);
                    updated_rithmic_bar(last, trade, exchange_nanos)?
                }
            }
            RithmicLiveCadence::Calendar { calendar, period } => {
                let last_bucket = calendar.bucket(last.exchange_timestamp_seconds, period);
                let trade_bucket =
                    calendar.bucket(exchange_nanos.div_euclid(1_000_000_000), period);
                if last_bucket == trade_bucket {
                    updated_rithmic_bar(last, trade, exchange_nanos)?
                } else {
                    started_rithmic_bar(last, trade, exchange_nanos)?
                }
            }
        };
        if next.source_sequence == last.source_sequence {
            if let Some(forming) = self.bars.last_mut() {
                *forming = next;
            }
        } else {
            self.bars.push(next);
            if self.bars.len() > HISTORY_BARS_PER_SERIES + 1 {
                self.bars.remove(0);
            }
        }
        self.live_session_generation = Some(trade.metadata.session_generation);
        self.last_trade_sequence = Some(trade.metadata.source_sequence);
        Ok(true)
    }
}

fn updated_rithmic_bar(
    mut bar: MarketBar,
    trade: &MarketTrade,
    _exchange_timestamp_unix_nanos: i64,
) -> Result<MarketBar, String> {
    bar.high = bar.high.max(trade.price);
    bar.low = bar.low.min(trade.price);
    bar.close = trade.price;
    bar.volume = bar
        .volume
        .checked_add(trade.quantity)
        .ok_or_else(|| "Rithmic live volume overflowed".to_string())?;
    Ok(bar)
}

fn started_rithmic_bar(
    completed: MarketBar,
    trade: &MarketTrade,
    exchange_timestamp_unix_nanos: i64,
) -> Result<MarketBar, String> {
    Ok(MarketBar {
        source_sequence: completed
            .source_sequence
            .checked_add(1)
            .ok_or_else(|| "Rithmic live sequence overflowed".to_string())?,
        exchange_timestamp_seconds: exchange_timestamp_unix_nanos.div_euclid(1_000_000_000),
        exchange_timestamp_unix_nanos,
        open: trade.price,
        high: trade.price,
        low: trade.price,
        close: trade.price,
        volume: trade.quantity,
    })
}

impl LiveHandoff {
    fn try_new(
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        installed: &InstallProviderInstrument,
    ) -> Result<Self, String> {
        let profile = coinbase_series_profile(series, installed)?;
        Ok(Self {
            generation,
            aggregator: coinbase_aggregator(profile)?,
            buffered: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            connected: false,
            history_ready: false,
            dirty: false,
            published: None,
            seam_repair: None,
            viewport_position: ViewportPosition::Live,
        })
    }

    fn reset(&mut self, generation: ProviderGeneration) {
        self.generation = generation;
        self.aggregator.reset();
        self.buffered.clear();
        self.connected = false;
        self.history_ready = false;
        self.dirty = false;
        self.published = None;
        self.seam_repair = None;
        self.viewport_position = ViewportPosition::Live;
    }

    fn take_publication(&mut self) -> Option<LiveSeriesPublication> {
        if !self.connected || !self.history_ready || !self.dirty {
            return None;
        }
        let active = self.aggregator.in_flight()?;
        self.dirty = false;
        let covering = requires_covering_publication(self.published, active.source_sequence);
        self.published = Some(PublishedTailState::Forming(active.source_sequence));
        if covering {
            let mut bars = self.aggregator.history();
            bars.push(active);
            Some(LiveSeriesPublication::Covering(bars))
        } else {
            Some(LiveSeriesPublication::Tail(active))
        }
    }
}

trait HistorySource: Send + 'static {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String>;
}

trait RealtimeSource: Send + 'static {
    fn configure(&mut self, _products: Vec<String>) -> Result<(), String> {
        Ok(())
    }

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

struct LiveRithmicHistory;

enum HistorySources {
    #[cfg(test)]
    Shared(Box<dyn HistorySource>),
    Split {
        coinbase: Box<dyn HistorySource>,
        rithmic: Box<dyn HistorySource>,
    },
}

struct LiveCoinbaseRealtime {
    config: Option<CoinbaseConfig>,
}

impl LiveCoinbaseHistory {
    fn try_new() -> Result<Self, String> {
        CoinbaseHistoryCapabilityAdapter::try_new()
            .map(|adapter| Self { adapter })
            .map_err(|error| error.to_string())
    }
}

impl HistorySource for LiveCoinbaseHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        self.adapter.set_stop(Arc::clone(&request.stop));
        let series = &request.series;
        let installed = request
            .instrument
            .as_ref()
            .ok_or_else(|| "Coinbase instrument is not installed".to_string())?;
        let profile = coinbase_series_profile(series, installed)?;
        let product = CoinbaseSpotProduct {
            product_id: installed.provider_symbol.clone(),
            instrument_id: installed.instrument_id.clone(),
            display_symbol: installed.display_symbol.clone(),
            base_currency: installed
                .provider_symbol
                .split_once('-')
                .map_or_else(String::new, |(base, _)| base.to_string()),
            quote_currency: installed
                .provider_symbol
                .split_once('-')
                .map_or_else(String::new, |(_, quote)| quote.to_string()),
            price_scale: u8::try_from(installed.price_scale)
                .map_err(|_| "Coinbase price scale is invalid".to_string())?,
            quantity_scale: u8::try_from(installed.quantity_scale)
                .map_err(|_| "Coinbase quantity scale is invalid".to_string())?,
        };
        self.adapter.register_product(&product);
        let now_seconds = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "system clock is unavailable".to_string())?
                .as_secs(),
        )
        .map_err(|_| "Coinbase history end time overflowed".to_string())?;
        let end_seconds = profile.interval.bucket_start(now_seconds)?;
        let end_unix_nanos = end_seconds
            .checked_mul(1_000_000_000)
            .ok_or_else(|| "Coinbase history end time overflowed".to_string())?;
        let range = request.range.unwrap_or_else(|| {
            let start_seconds = i64::try_from(request.maximum_bars)
                .ok()
                .and_then(|count| profile.interval.shift_bucket(end_seconds, -count).ok())
                .unwrap_or(0);
            HistoryRange {
                start_unix_nanos: start_seconds.saturating_mul(1_000_000_000),
                end_unix_nanos,
            }
        });
        let request = HistoryPageRequest {
            provider_id: "coinbase".to_string(),
            account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
            entitlement_revision: ENTITLEMENT_CLASS.to_string(),
            instrument_id: series.instrument_id.clone(),
            data_class: DataClass::Bars,
            resolution: profile.resolution.to_string(),
            range,
            maximum_items: NonZeroUsize::new(request.maximum_bars.min(350))
                .unwrap_or(NonZeroUsize::MIN),
            continuation: None,
        };
        let batch = self.adapter.fetch_paginated(&request)?;
        let source_bars = batch
            .items
            .iter()
            .map(decode_history_bar)
            .collect::<Result<Vec<_>, _>>()?;
        let confirmed_empty = source_bars.is_empty();
        let (mut bars, _) = if source_bars.is_empty() {
            (
                Vec::new(),
                axiusflow_coinbase_market_adapter::CoinbaseAggregationDiagnostics::default(),
            )
        } else {
            aggregate_coinbase_bars(&source_bars, profile.interval)?
        };
        retain_completed_coinbase_bars(
            profile.interval,
            end_seconds.min(range.end_unix_nanos.div_euclid(1_000_000_000)),
            &mut bars,
        );
        for (index, bar) in bars.iter_mut().enumerate() {
            bar.source_sequence = u64::try_from(index)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| "Coinbase history sequence overflowed".to_string())?;
        }
        Ok(HistorySnapshot {
            price_scale: profile.price_scale,
            quantity_scale: profile.quantity_scale,
            bars,
            handoff_boundary_unix_nanos: Some(range.end_unix_nanos),
            confirmed_empty,
        })
    }
}

impl HistorySource for LiveRithmicHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        if request.series.provider_id != "rithmic" {
            return Err("Rithmic history received another provider".to_string());
        }
        fetch_rithmic_history(request)
    }
}

fn fetch_rithmic_history(request: &HistoryRequest) -> Result<HistorySnapshot, String> {
    let installed = request
        .instrument
        .as_ref()
        .ok_or_else(|| "Rithmic instrument is not installed".to_string())?;
    let snapshot = crate::rithmic_history::fetch(
        &request.series,
        request.provider_generation.0.get(),
        installed,
        request.maximum_bars,
        &request.stop,
    )?;
    Ok(HistorySnapshot {
        price_scale: snapshot.price_scale,
        quantity_scale: snapshot.quantity_scale,
        bars: snapshot.bars,
        handoff_boundary_unix_nanos: Some(snapshot.handoff_boundary_unix_nanos),
        confirmed_empty: false,
    })
}

impl LiveCoinbaseRealtime {
    fn try_new() -> Self {
        Self { config: None }
    }
}

impl RealtimeSource for LiveCoinbaseRealtime {
    fn configure(&mut self, products: Vec<String>) -> Result<(), String> {
        self.config = Some(
            CoinbaseConfig::try_new(products)
                .map_err(|error| error.to_string())?
                .with_level2(false),
        );
        Ok(())
    }

    fn run_generation(
        &mut self,
        generation: ProviderGeneration,
        events: &SyncSender<RealtimeEvent>,
        overflow: &AtomicBool,
        stop: &Arc<AtomicBool>,
    ) {
        let Some(config) = self.config.clone() else {
            return;
        };
        let Ok(connection) = CoinbaseSession::new(config).connect_cancellable(Arc::clone(stop))
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
    /// Starts the process-owned market coordinator and its bounded provider-history worker.
    ///
    /// # Errors
    /// Returns an error when provider configuration or either bounded worker cannot start.
    pub fn start(workspace: &WorkspaceState) -> Result<Self, String> {
        let hot_series = retained_hot_series(workspace, available_memory_bytes())?;
        let storage = LocalHistoryStore::open(
            &crate::default_engine_state_root()?
                .join("market-history")
                .join("coinbase"),
        )
        .map_err(|error| error.to_string());
        let service = Self::start_composed(
            HistorySources::Split {
                coinbase: Box::new(LiveCoinbaseHistory::try_new()?),
                rithmic: Box::new(LiveRithmicHistory),
            },
            Some(Box::new(LiveCoinbaseRealtime::try_new())),
            Some(storage),
            true,
            hot_series.len(),
        )?;
        service.restore_hot_set(&hot_series)?;
        Ok(service)
    }

    fn start_composed(
        sources: HistorySources,
        realtime: Option<Box<dyn RealtimeSource>>,
        storage: Option<Result<LocalHistoryStore, String>>,
        rithmic_realtime: bool,
        hot_set_priority_count: usize,
    ) -> Result<Self, String> {
        let engine = configured_engine()?;
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (coinbase_history_tx, coinbase_history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
        let (coinbase_source, rithmic_source, rithmic_history_tx, rithmic_history_rx) =
            match sources {
                #[cfg(test)]
                HistorySources::Shared(source) => (source, None, coinbase_history_tx.clone(), None),
                HistorySources::Split { coinbase, rithmic } => {
                    let (rithmic_tx, rithmic_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
                    (coinbase, Some(rithmic), rithmic_tx, Some(rithmic_rx))
                }
            };
        let (storage_tx, storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
        let (realtime_tx, realtime_rx) = mpsc::sync_channel(REALTIME_CAPACITY);
        let (realtime_control_tx, realtime_control_rx) = mpsc::sync_channel(1);
        let (rithmic_realtime_tx, rithmic_realtime_rx) = mpsc::sync_channel(REALTIME_CAPACITY);
        let (rithmic_realtime_control_tx, rithmic_realtime_control_rx) =
            mpsc::sync_channel(RITHMIC_REALTIME_CONTROL_CAPACITY);
        let (rithmic_catalog_tx, rithmic_catalog_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (rithmic_catalog_control_tx, rithmic_catalog_control_rx) =
            mpsc::sync_channel(COMMAND_CAPACITY);
        let realtime_overflow = Arc::new(AtomicBool::new(false));
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (coinbase_catalog_control_tx, coinbase_catalog_rx, coinbase_catalog_worker) =
            crate::coinbase_catalog::start(Arc::clone(&shutdown))?;
        let available_memory_bytes = available_memory_bytes();
        let mut workers = Vec::with_capacity(7);
        workers.push(spawn_history_worker(
            "axiusflow-coinbase-history",
            coinbase_source,
            coinbase_history_rx,
            command_tx.clone(),
            Arc::clone(&shutdown),
        )?);
        workers.push(coinbase_catalog_worker);
        if let Some((source, requests)) = rithmic_source.zip(rithmic_history_rx) {
            workers.push(spawn_history_worker(
                "axiusflow-rithmic-history",
                source,
                requests,
                command_tx.clone(),
                Arc::clone(&shutdown),
            )?);
        }
        workers.push(spawn_storage_worker(
            storage,
            storage_rx,
            command_tx.clone(),
            Arc::clone(&shutdown),
        )?);
        workers.extend(spawn_optional_realtime_worker(
            realtime,
            realtime_control_rx,
            realtime_tx,
            &realtime_overflow,
            &realtime_stop,
            configured_reconnect_delay(&engine, "coinbase")?,
        )?);
        workers.extend(start_rithmic_workers(
            rithmic_realtime,
            rithmic_catalog_control_rx,
            rithmic_catalog_tx,
            rithmic_realtime_control_rx,
            rithmic_realtime_tx,
            configured_reconnect_delay(&engine, "rithmic")?,
        )?);
        workers.push(spawn_coordinator(
            engine,
            OwnedCoordinatorChannels {
                commands: command_rx,
                coinbase_history: coinbase_history_tx,
                rithmic_history: rithmic_history_tx,
                storage: storage_tx,
                realtime_control: realtime_control_tx,
                realtime: realtime_rx,
                rithmic_realtime_control: rithmic_realtime_control_tx,
                rithmic_realtime: rithmic_realtime_rx,
                rithmic_catalog_control: rithmic_catalog_control_tx,
                rithmic_catalog: rithmic_catalog_rx,
                coinbase_catalog_control: coinbase_catalog_control_tx,
                coinbase_catalog: coinbase_catalog_rx,
                rithmic_enabled: rithmic_realtime,
            },
            Arc::clone(&realtime_overflow),
            Arc::clone(&realtime_stop),
            Arc::clone(&shutdown),
            available_memory_bytes,
            hot_set_priority_count,
        )?);
        let service = Self::build_market_service(command_tx, shutdown, realtime_stop, workers);
        #[cfg(test)]
        service.install_provider_instrument(&test_coinbase_instrument())?;
        #[cfg(test)]
        service.install_provider_instrument(&test_coinbase_eth_instrument())?;
        Ok(service)
    }

    fn build_market_service(
        commands: SyncSender<Command>,
        shutdown: Arc<AtomicBool>,
        realtime_stop: Arc<AtomicBool>,
        workers: Vec<thread::JoinHandle<()>>,
    ) -> Self {
        Self {
            commands,
            runtime: Arc::new(MarketRuntime {
                shutdown,
                realtime_stop,
                workers: Mutex::new(Some(workers)),
            }),
        }
    }

    /// Cancels provider work, drains accepted persistence, and joins owned workers.
    ///
    /// # Errors
    /// Returns an error when a worker panics or the complete shutdown exceeds `timeout`.
    pub fn shutdown(&self, timeout: Duration) -> Result<(), String> {
        self.runtime.shutdown.store(true, Ordering::Release);
        self.runtime.realtime_stop.store(true, Ordering::Release);
        let mut workers = self
            .runtime
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| "market engine shutdown is already in progress".to_string())?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| "market engine shutdown deadline overflowed".to_string())?;
        let mut panicked = Vec::new();
        loop {
            let mut index = 0;
            while index < workers.len() {
                if workers[index].is_finished() {
                    let worker = workers.swap_remove(index);
                    let name = worker.thread().name().unwrap_or("unnamed").to_string();
                    if worker.join().is_err() {
                        panicked.push(name);
                    }
                } else {
                    index += 1;
                }
            }
            if workers.is_empty() {
                return if panicked.is_empty() {
                    Ok(())
                } else {
                    Err(format!(
                        "market engine workers panicked during shutdown: {}",
                        panicked.join(", ")
                    ))
                };
            }
            let now = Instant::now();
            if now >= deadline {
                let pending = workers
                    .iter()
                    .map(|worker| worker.thread().name().unwrap_or("unnamed"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!(
                    "market engine shutdown deadline expired with active workers: {pending}"
                ));
            }
            thread::sleep(Duration::from_millis(5).min(deadline.duration_since(now)));
        }
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

    /// Applies the engine-owned background market retention policy.
    ///
    /// # Errors
    /// Returns an error when the coordinator is unavailable.
    pub fn set_resource_mode(&self, mode: ResourceMode) -> Result<(), String> {
        self.request(|reply| Ok(Command::SetResourceMode(mode, reply)))
    }

    fn restore_hot_set(&self, hot_series: &[HotSeries]) -> Result<(), String> {
        let restored = hot_series
            .iter()
            .filter_map(|series| {
                if let Ok(series) = warm_series(series) {
                    Some(series)
                } else {
                    eprintln!("Axiusflow engine skipped unsupported hot-set metadata");
                    None
                }
            })
            .collect::<Vec<_>>();
        self.request(|reply| Ok(Command::RestoreHotSet(restored, reply)))
    }

    /// Returns one bounded coordinator-owned lifecycle and memory snapshot.
    ///
    /// # Errors
    /// Returns an error when the coordinator is unavailable.
    pub fn status(&self) -> Result<MarketServiceStatus, String> {
        self.request(|reply| Ok(Command::Status(reply)))
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
        self.set_resource_class(
            client_id,
            consumer_id,
            if visible {
                ConsumerResourceClass::Foreground
            } else {
                ConsumerResourceClass::Background
            },
        )
    }

    /// Updates one consumer's exact resource class.
    ///
    /// # Errors
    /// Returns an error for invalid identity, missing consumer, or coordinator failure.
    pub fn set_resource_class(
        &self,
        client_id: u64,
        consumer_id: u64,
        resource_class: ConsumerResourceClass,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::ResourceClass(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                resource_class,
                reply,
            ))
        })
    }

    /// Accepts one generation-fenced series demand without waiting for provider I/O.
    ///
    /// # Errors
    /// Returns an error for invalid demand or an unavailable coordinator.
    pub fn set_demand(
        &self,
        client_id: u64,
        consumer_id: u64,
        generation: u64,
        series: &SeriesKey,
    ) -> Result<(), String> {
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

    /// Schedules one bounded exact provider-instrument search for an owned consumer.
    ///
    /// # Errors
    /// Returns an error for invalid identity, unsupported input, ownership, or coordinator failure.
    pub fn search_provider_instruments(
        &self,
        client_id: u64,
        search: SearchProviderInstruments,
    ) -> Result<(), String> {
        validate_provider_search(&search)?;
        self.request(|reply| {
            Ok(Command::SearchProviderInstruments(
                ClientId(id(client_id)?),
                search,
                reply,
            ))
        })
    }

    /// Schedules one exact provider-instrument selection for an owned consumer.
    ///
    /// # Errors
    /// Returns an error for invalid identity, unsupported input, ownership, or coordinator failure.
    pub fn select_provider_instrument(
        &self,
        client_id: u64,
        selection: SelectProviderInstrument,
    ) -> Result<(), String> {
        validate_provider_selection(&selection)?;
        self.request(|reply| {
            Ok(Command::SelectProviderInstrument(
                ClientId(id(client_id)?),
                selection,
                reply,
            ))
        })
    }

    /// Installs one bounded adapter-resolved instrument in the engine-owned catalog.
    ///
    /// # Errors
    /// Returns an error for invalid identity, stale generations, capacity, or coordinator failure.
    pub fn install_provider_instrument(
        &self,
        instrument: &InstallProviderInstrument,
    ) -> Result<(), String> {
        validate_provider_instrument(instrument)?;
        self.request(|reply| {
            Ok(Command::InstallProviderInstrument(
                instrument.clone(),
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

fn spawn_history_worker(
    name: &'static str,
    source: Box<dyn HistorySource>,
    requests: Receiver<HistoryRequest>,
    completions: SyncSender<Command>,
    shutdown: Arc<AtomicBool>,
) -> Result<thread::JoinHandle<()>, String> {
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || run_history_worker(source, &requests, &completions, &shutdown))
        .map_err(|error| error.to_string())
}

fn spawn_storage_worker(
    storage: Option<Result<LocalHistoryStore, String>>,
    requests: Receiver<StorageRequest>,
    completions: SyncSender<Command>,
    shutdown: Arc<AtomicBool>,
) -> Result<thread::JoinHandle<()>, String> {
    thread::Builder::new()
        .name("axiusflow-local-history".to_string())
        .spawn(move || run_storage_worker(storage, &requests, &completions, &shutdown))
        .map_err(|error| error.to_string())
}

fn spawn_realtime_worker(
    realtime: Box<dyn RealtimeSource>,
    controls: Receiver<RealtimeControl>,
    events: SyncSender<RealtimeEvent>,
    overflow: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    reconnect_delay: Duration,
) -> Result<thread::JoinHandle<()>, String> {
    thread::Builder::new()
        .name("axiusflow-coinbase-realtime".to_string())
        .spawn(move || {
            run_realtime_worker(
                realtime,
                &controls,
                &events,
                &overflow,
                &stop,
                reconnect_delay,
            );
        })
        .map_err(|error| error.to_string())
}

fn spawn_optional_realtime_worker(
    realtime: Option<Box<dyn RealtimeSource>>,
    controls: Receiver<RealtimeControl>,
    events: SyncSender<RealtimeEvent>,
    overflow: &Arc<AtomicBool>,
    stop: &Arc<AtomicBool>,
    reconnect_delay: Duration,
) -> Result<Option<thread::JoinHandle<()>>, String> {
    realtime
        .map(|realtime| {
            spawn_realtime_worker(
                realtime,
                controls,
                events,
                Arc::clone(overflow),
                Arc::clone(stop),
                reconnect_delay,
            )
        })
        .transpose()
}

fn start_rithmic_workers(
    enabled: bool,
    catalog_controls: Receiver<RithmicCatalogControl>,
    catalog_events: SyncSender<RithmicCatalogEvent>,
    realtime_controls: Receiver<RithmicRealtimeControl>,
    realtime_events: SyncSender<RithmicRealtimeEvent>,
    reconnect_delay: Duration,
) -> Result<Vec<thread::JoinHandle<()>>, String> {
    if !enabled {
        return Ok(Vec::new());
    }
    let provider = thread::Builder::new()
        .name("axiusflow-rithmic-provider".to_string())
        .spawn(move || {
            crate::rithmic_realtime::run(
                &catalog_controls,
                &catalog_events,
                &realtime_controls,
                &realtime_events,
                reconnect_delay,
            );
        })
        .map_err(|error| error.to_string())?;
    Ok(vec![provider])
}

fn run_history_worker(
    mut source: Box<dyn HistorySource>,
    requests: &Receiver<HistoryRequest>,
    completions: &SyncSender<Command>,
    shutdown: &AtomicBool,
) {
    while let Ok(request) = requests.recv() {
        if shutdown.load(Ordering::Acquire) {
            request.stop.store(true, Ordering::Release);
            return;
        }
        let result = source.fetch(&request);
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        if completions
            .send(Command::HistoryCompleted(
                request.series,
                request.provider_generation,
                request.range,
                request.kind,
                result,
            ))
            .is_err()
        {
            return;
        }
    }
}

fn run_storage_worker(
    mut storage: Option<Result<LocalHistoryStore, String>>,
    requests: &Receiver<StorageRequest>,
    completions: &SyncSender<Command>,
    shutdown: &AtomicBool,
) {
    while let Ok(request) = requests.recv() {
        let completion = storage_completion(&mut storage, request);
        if !shutdown.load(Ordering::Acquire)
            && completions.send(completion).is_err()
            && !shutdown.load(Ordering::Acquire)
        {
            return;
        }
    }
}

fn storage_completion(
    storage: &mut Option<Result<LocalHistoryStore, String>>,
    request: StorageRequest,
) -> Command {
    match request {
        StorageRequest::Read(series, generation) => {
            let result = match storage.as_mut() {
                Some(Ok(storage)) => read_local_history(storage, &series),
                Some(Err(error)) => Err(error.clone()),
                None => Ok(None),
            };
            Command::LocalHistoryCompleted(series, generation, result)
        }
        StorageRequest::ReadRange(series, generation, range) => {
            let result = match storage.as_mut() {
                Some(Ok(storage)) => local_history_scope(&series).and_then(|scope| {
                    let stored = storage
                        .read_range(
                            &scope,
                            &series,
                            range.start_unix_nanos,
                            range.end_unix_nanos,
                        )
                        .map_err(|error| error.to_string())?;
                    let confirmed_empty = storage
                        .confirmed_empty_ranges(&scope, &series)
                        .map_err(|error| error.to_string())?
                        .into_iter()
                        .map(|range| HistoryRange {
                            start_unix_nanos: range.start_unix_nanos,
                            end_unix_nanos: range.end_unix_nanos,
                        })
                        .collect();
                    Ok(LocalRangeHistory {
                        stored,
                        confirmed_empty,
                    })
                }),
                Some(Err(error)) => Err(error.clone()),
                None => Ok(LocalRangeHistory {
                    stored: None,
                    confirmed_empty: Vec::new(),
                }),
            };
            Command::ViewportHistoryLocalCompleted(series, generation, range, result)
        }
        StorageRequest::Persist(
            series,
            generation,
            bars,
            derived,
            protected_ranges,
            started_at,
        ) => {
            let result = match storage.as_mut() {
                Some(Ok(storage)) => {
                    persist_local_history(storage, &series, &bars, derived, &protected_ranges)
                }
                Some(Err(_)) => Err(LocalHistoryError::Unavailable),
                None => Ok(()),
            };
            Command::PersistenceCompleted(
                series,
                generation,
                result,
                u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            )
        }
        StorageRequest::RecordConfirmedEmpty(series, range) => {
            let result = match storage.as_mut() {
                Some(Ok(storage)) => local_history_scope(&series)
                    .map_err(|_| LocalHistoryError::InvalidSeries)
                    .and_then(|scope| {
                        storage.record_confirmed_empty(
                            &scope,
                            &series,
                            range.start_unix_nanos,
                            range.end_unix_nanos,
                        )
                    }),
                Some(Err(_)) => Err(LocalHistoryError::Unavailable),
                None => Ok(()),
            };
            Command::ConfirmedEmptyRecorded(series, result)
        }
        StorageRequest::ResolveConfirmedEmpty(series, range) => {
            let result = match storage.as_mut() {
                Some(Ok(storage)) => local_history_scope(&series)
                    .map_err(|_| LocalHistoryError::InvalidSeries)
                    .and_then(|scope| {
                        storage.resolve_confirmed_empty(
                            &scope,
                            &series,
                            range.start_unix_nanos,
                            range.end_unix_nanos,
                        )
                    }),
                Some(Err(_)) => Err(LocalHistoryError::Unavailable),
                None => Ok(()),
            };
            Command::ConfirmedEmptyResolved(series, range, result)
        }
    }
}

fn read_local_history(
    storage: &mut LocalHistoryStore,
    series: &BarSeriesKey,
) -> Result<Option<StoredHistory>, String> {
    let scope = local_history_scope(series)?;
    if let Some(stored) = storage
        .read_latest(&scope, series)
        .map_err(|error| error.to_string())?
    {
        return Ok(Some(stored));
    }
    let target_seconds = match series.period {
        BarPeriod::Time { seconds } if series.provider_id == "coinbase" && seconds > 60 => seconds,
        BarPeriod::Time { .. }
        | BarPeriod::Tick { .. }
        | BarPeriod::Session { .. }
        | BarPeriod::Week { .. }
        | BarPeriod::Month { .. } => return Ok(None),
    };
    let interval = coinbase_interval(target_seconds)?;
    let source_series = BarSeriesKey {
        period: BarPeriod::time(60).map_err(|error| error.to_string())?,
        ..series.clone()
    };
    let Some(source) = storage
        .read_latest(&scope, &source_series)
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let (bars, _) = aggregate_coinbase_bars(&source.bars, interval)?;
    if bars.is_empty() {
        return Ok(None);
    }
    let durable = storage.persist(&scope, series, &bars, true).is_ok();
    Ok(Some(StoredHistory {
        bars,
        derived: true,
        durable,
    }))
}

fn persist_local_history(
    storage: &mut LocalHistoryStore,
    series: &BarSeriesKey,
    bars: &[MarketBar],
    derived: bool,
    protected_ranges: &[(BarSeriesKey, HistoryRange)],
) -> Result<(), LocalHistoryError> {
    let scope = local_history_scope(series).map_err(|_| LocalHistoryError::InvalidSeries)?;
    let protected = protected_ranges
        .iter()
        .map(|(protected_series, range)| {
            Ok((
                local_history_scope(protected_series)
                    .map_err(|_| LocalHistoryError::InvalidSeries)?,
                protected_series.clone(),
                RetainedRange {
                    start_unix_nanos: range.start_unix_nanos,
                    end_unix_nanos: range.end_unix_nanos,
                },
            ))
        })
        .collect::<Result<Vec<_>, LocalHistoryError>>()?;
    storage.persist_with_protected_series_ranges(&scope, series, bars, derived, &protected)
}

fn local_history_scope(series: &BarSeriesKey) -> Result<HistoryScope, String> {
    let account_id = match series.provider_id.as_str() {
        "coinbase" if series.entitlement_id == ENTITLEMENT_CLASS => COINBASE_PUBLIC_ACCOUNT_ID,
        "rithmic" => RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID,
        _ => return Err("local history provider scope is unsupported".to_string()),
    };
    Ok(HistoryScope {
        provider_id: series.provider_id.clone(),
        account_id: account_id.to_string(),
        entitlement_revision: series.entitlement_id.clone(),
    })
}

fn run_realtime_worker(
    mut source: Box<dyn RealtimeSource>,
    control: &Receiver<RealtimeControl>,
    events: &SyncSender<RealtimeEvent>,
    overflow: &AtomicBool,
    stop: &Arc<AtomicBool>,
    reconnect_delay: Duration,
) {
    let mut generation = ProviderGeneration(
        NonZeroU64::new(COINBASE_PROVIDER_GENERATION).unwrap_or(NonZeroU64::MIN),
    );
    loop {
        let Ok(RealtimeControl::Start(products)) = control.recv() else {
            return;
        };
        stop.store(false, Ordering::Release);
        if source.configure(products).is_err() {
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
            thread::park_timeout(reconnect_delay);
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

#[derive(Clone, Copy)]
struct CoordinatorChannels<'a> {
    commands: &'a Receiver<Command>,
    coinbase_history: &'a SyncSender<HistoryRequest>,
    rithmic_history: &'a SyncSender<HistoryRequest>,
    storage: &'a SyncSender<StorageRequest>,
    realtime_control: &'a SyncSender<RealtimeControl>,
    realtime: &'a Receiver<RealtimeEvent>,
    rithmic_realtime_control: Option<&'a SyncSender<RithmicRealtimeControl>>,
    rithmic_realtime: &'a Receiver<RithmicRealtimeEvent>,
    rithmic_catalog_control: Option<&'a SyncSender<RithmicCatalogControl>>,
    rithmic_catalog: &'a Receiver<RithmicCatalogEvent>,
    coinbase_catalog_control: Option<&'a CoinbaseCatalogControl>,
    coinbase_catalog: &'a Receiver<CoinbaseCatalogEvent>,
}

struct OwnedCoordinatorChannels {
    commands: Receiver<Command>,
    coinbase_history: SyncSender<HistoryRequest>,
    rithmic_history: SyncSender<HistoryRequest>,
    storage: SyncSender<StorageRequest>,
    realtime_control: SyncSender<RealtimeControl>,
    realtime: Receiver<RealtimeEvent>,
    rithmic_realtime_control: SyncSender<RithmicRealtimeControl>,
    rithmic_realtime: Receiver<RithmicRealtimeEvent>,
    rithmic_catalog_control: SyncSender<RithmicCatalogControl>,
    rithmic_catalog: Receiver<RithmicCatalogEvent>,
    coinbase_catalog_control: CoinbaseCatalogControl,
    coinbase_catalog: Receiver<CoinbaseCatalogEvent>,
    rithmic_enabled: bool,
}

fn spawn_coordinator(
    engine: MarketEngine,
    channels: OwnedCoordinatorChannels,
    realtime_overflow: Arc<AtomicBool>,
    realtime_stop: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    available_memory_bytes: u64,
    hot_set_priority_count: usize,
) -> Result<thread::JoinHandle<()>, String> {
    thread::Builder::new()
        .name("axiusflow-market-engine".to_string())
        .spawn(move || {
            run_coordinator(
                engine,
                CoordinatorChannels {
                    commands: &channels.commands,
                    coinbase_history: &channels.coinbase_history,
                    rithmic_history: &channels.rithmic_history,
                    storage: &channels.storage,
                    realtime_control: &channels.realtime_control,
                    realtime: &channels.realtime,
                    rithmic_realtime_control: channels
                        .rithmic_enabled
                        .then_some(&channels.rithmic_realtime_control),
                    rithmic_realtime: &channels.rithmic_realtime,
                    rithmic_catalog_control: channels
                        .rithmic_enabled
                        .then_some(&channels.rithmic_catalog_control),
                    rithmic_catalog: &channels.rithmic_catalog,
                    coinbase_catalog_control: Some(&channels.coinbase_catalog_control),
                    coinbase_catalog: &channels.coinbase_catalog,
                },
                &realtime_overflow,
                &realtime_stop,
                &shutdown,
                available_memory_bytes,
                hot_set_priority_count,
            );
        })
        .map_err(|error| error.to_string())
}

fn run_coordinator(
    engine: MarketEngine,
    channels: CoordinatorChannels<'_>,
    realtime_overflow: &AtomicBool,
    realtime_stop: &Arc<AtomicBool>,
    shutdown: &AtomicBool,
    available_memory_bytes: u64,
    hot_set_priority_count: usize,
) {
    let resource_policy = decide_resource_policy(ResourcePolicyInput {
        mode: EngineResourceMode::Warm,
        available_memory_bytes,
        consumer_count: 0,
        visible_consumer_count: 0,
        provider_series_limit: MAXIMUM_SERIES,
        hot_set_priority_count,
    });
    let mut coordinator = Coordinator {
        engine,
        coinbase_history: channels.coinbase_history,
        rithmic_history: channels.rithmic_history,
        storage: channels.storage,
        realtime_control: channels.realtime_control,
        rithmic_realtime_control: channels.rithmic_realtime_control,
        rithmic_catalog_control: channels.rithmic_catalog_control,
        coinbase_catalog_control: channels.coinbase_catalog_control,
        realtime_stop,
        resource_mode: ResourceMode::Warm,
        resource_policy,
        available_memory_bytes,
        hot_set_priority_count,
        last_consumer_activity: Instant::now(),
        attached: BTreeSet::new(),
        pending: BTreeMap::new(),
        history_inflight: BTreeMap::new(),
        history_cancellations: BTreeMap::new(),
        history_coverage: BTreeMap::new(),
        viewport_history_ranges: BTreeMap::new(),
        active_viewports: BTreeMap::new(),
        viewport_history_local_inflight: BTreeSet::new(),
        deferred_publications: BTreeSet::new(),
        pending_empty_repairs: BTreeMap::new(),
        empty_repair_retry_at: Instant::now(),
        local_history_deadlines: BTreeMap::new(),
        local_loaded: BTreeSet::new(),
        warming: BTreeSet::new(),
        warm_series: BTreeMap::new(),
        warm_priority: Vec::new(),
        retained_history: BTreeMap::new(),
        prewarmed: BTreeSet::new(),
        retained_live: BTreeSet::new(),
        warm_rithmic_search_generation: 0,
        events: BTreeMap::new(),
        live: BTreeMap::new(),
        rithmic_live: BTreeMap::new(),
        rithmic_order_books: BTreeMap::new(),
        catalog: BTreeMap::new(),
        catalog_sessions: BTreeMap::new(),
        catalog_selections: BTreeMap::new(),
        realtime_started: false,
        realtime_connected: false,
        rithmic_realtime_started: false,
        realtime_products: BTreeSet::new(),
    };
    loop {
        if shutdown.load(Ordering::Acquire) {
            coordinator.begin_shutdown();
            return;
        }
        drain_coordinator_events(&mut coordinator, &channels);
        if realtime_overflow.swap(false, Ordering::AcqRel) {
            coordinator
                .realtime_interrupted(FailureStage::Handoff, "Coinbase realtime queue overflowed");
        }
        coordinator.publish_live();
        coordinator.publish_rithmic_live();
        if !coordinator.live.is_empty() {
            let _ = coordinator.sync_coinbase_realtime();
        }
        coordinator.expire_local_history_reads();
        coordinator.retry_pending_empty_repairs();
        coordinator.enforce_resource_policy();
        match channels.commands.recv_timeout(COORDINATOR_TICK) {
            Ok(command) if !shutdown.load(Ordering::Acquire) => {
                coordinator.handle_command(command);
            }
            Ok(_) => {
                coordinator.begin_shutdown();
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn drain_coordinator_events(coordinator: &mut Coordinator<'_>, channels: &CoordinatorChannels<'_>) {
    for _ in 0..REALTIME_DRAIN_BUDGET {
        match channels.realtime.try_recv() {
            Ok(event) => coordinator.handle_realtime(event),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
    for _ in 0..REALTIME_DRAIN_BUDGET {
        match channels.rithmic_realtime.try_recv() {
            Ok(event) => coordinator.handle_rithmic_realtime(event),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
    for _ in 0..REALTIME_DRAIN_BUDGET {
        match channels.rithmic_catalog.try_recv() {
            Ok(event) => coordinator.handle_rithmic_catalog(event),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
    for _ in 0..REALTIME_DRAIN_BUDGET {
        match channels.coinbase_catalog.try_recv() {
            Ok(event) => coordinator.handle_coinbase_catalog(event),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
}

struct Coordinator<'a> {
    engine: MarketEngine,
    coinbase_history: &'a SyncSender<HistoryRequest>,
    rithmic_history: &'a SyncSender<HistoryRequest>,
    storage: &'a SyncSender<StorageRequest>,
    realtime_control: &'a SyncSender<RealtimeControl>,
    rithmic_realtime_control: Option<&'a SyncSender<RithmicRealtimeControl>>,
    rithmic_catalog_control: Option<&'a SyncSender<RithmicCatalogControl>>,
    coinbase_catalog_control: Option<&'a CoinbaseCatalogControl>,
    realtime_stop: &'a Arc<AtomicBool>,
    resource_mode: ResourceMode,
    resource_policy: ResourcePolicyDecision,
    available_memory_bytes: u64,
    hot_set_priority_count: usize,
    last_consumer_activity: Instant,
    attached: BTreeSet<ClientId>,
    pending: BTreeMap<BarSeriesKey, Vec<DemandWaiter>>,
    history_inflight: BTreeMap<(BarSeriesKey, ProviderGeneration), Option<HistoryRange>>,
    history_cancellations: BTreeMap<(BarSeriesKey, ProviderGeneration), Arc<AtomicBool>>,
    history_coverage: BTreeMap<BarSeriesKey, Vec<HistoryRange>>,
    viewport_history_ranges: BTreeMap<(BarSeriesKey, ProviderGeneration), HistoryRange>,
    active_viewports: BTreeMap<ConsumerId, ActiveViewport>,
    viewport_history_local_inflight: BTreeSet<(BarSeriesKey, ProviderGeneration, HistoryRange)>,
    deferred_publications: BTreeSet<BarSeriesKey>,
    pending_empty_repairs: BTreeMap<BarSeriesKey, HistoryRange>,
    empty_repair_retry_at: Instant,
    local_history_deadlines: BTreeMap<(BarSeriesKey, ProviderGeneration), Instant>,
    local_loaded: BTreeSet<(BarSeriesKey, ProviderGeneration)>,
    warming: BTreeSet<(BarSeriesKey, ProviderGeneration)>,
    warm_series: BTreeMap<BarSeriesKey, WarmSeries>,
    warm_priority: Vec<BarSeriesKey>,
    retained_history: BTreeMap<BarSeriesKey, StoredHistory>,
    prewarmed: BTreeSet<BarSeriesKey>,
    retained_live: BTreeSet<BarSeriesKey>,
    warm_rithmic_search_generation: u64,
    events: BTreeMap<ConsumerId, ConsumerEvents>,
    live: BTreeMap<BarSeriesKey, LiveHandoff>,
    rithmic_live: BTreeMap<BarSeriesKey, RithmicLiveHandoff>,
    rithmic_order_books: BTreeMap<String, RithmicOrderBook>,
    catalog: BTreeMap<(String, String), InstallProviderInstrument>,
    catalog_sessions: BTreeMap<String, u64>,
    catalog_selections: BTreeMap<String, u64>,
    realtime_started: bool,
    realtime_connected: bool,
    rithmic_realtime_started: bool,
    realtime_products: BTreeSet<String>,
}

impl Coordinator<'_> {
    fn detach_client(&mut self, client_id: ClientId) {
        for consumer_id in self.engine.detach_client(client_id) {
            if let Some(control) = self.coinbase_catalog_control {
                control.release_consumer(consumer_id.0.get());
            }
            self.events.remove(&consumer_id);
            self.remove_waiter(consumer_id);
            self.active_viewports.remove(&consumer_id);
        }
    }

    fn begin_shutdown(&self) {
        for stop in self.history_cancellations.values() {
            stop.store(true, Ordering::Release);
        }
        self.realtime_stop.store(true, Ordering::Release);
    }

    fn handle_command(&mut self, command: Command) {
        match command {
            Command::HistoryCompleted(series, generation, range, kind, result) => {
                self.history_completed(&series, generation, range, kind, result);
            }
            Command::LocalHistoryCompleted(series, generation, result) => {
                self.local_history_completed(&series, generation, result);
            }
            Command::ViewportHistoryLocalCompleted(series, generation, range, result) => {
                self.viewport_history_local_completed(&series, generation, range, result);
            }
            Command::PersistenceCompleted(series, generation, result, elapsed_millis) => {
                self.persistence_completed(&series, generation, result, elapsed_millis);
            }
            Command::ConfirmedEmptyRecorded(series, result) => {
                if let Err(error) = result {
                    self.broadcast_persistence_for(
                        &series,
                        PersistenceState::Degraded,
                        Some("Confirmed-empty history persistence is degraded"),
                    );
                    self.broadcast_demand_error_for(
                        &series,
                        local_history_failure_stage(error),
                        &error.to_string(),
                        None,
                    );
                }
            }
            Command::ConfirmedEmptyResolved(series, range, result) => {
                if result.is_err() {
                    self.remember_pending_empty_repair(&series, range);
                    self.empty_repair_retry_at = Instant::now() + EMPTY_REPAIR_RETRY_DELAY;
                }
            }
            command @ (Command::RestoreHotSet(..)
            | Command::SetResourceMode(..)
            | Command::Status(..)
            | Command::Attach(..)
            | Command::Detach(..)) => self.handle_service_command(command),
            command => self.handle_consumer_command(command),
        }
        self.refresh_resource_policy();
    }

    fn handle_service_command(&mut self, command: Command) {
        match command {
            Command::RestoreHotSet(series, reply) => {
                self.restore_hot_series(series);
                let _ = reply.send(Ok(()));
            }
            Command::SetResourceMode(mode, reply) => {
                self.apply_resource_mode(mode);
                let _ = reply.send(Ok(()));
            }
            Command::Status(reply) => {
                let _ = reply.send(Ok(self.status()));
            }
            Command::Attach(client_id, reply) => {
                self.handle_attach(client_id, &reply);
            }
            Command::Detach(client_id, reply) => {
                self.attached.remove(&client_id);
                self.detach_client(client_id);
                self.release_unused_live_market_data();
                let _ = reply.send(Ok(()));
            }
            _ => unreachable!("only service commands reach service dispatch"),
        }
    }

    fn handle_consumer_command(&mut self, command: Command) {
        match command {
            Command::Register(identity, reply) => {
                let _ = reply.send(self.handle_register(identity));
            }
            Command::Remove(client_id, consumer_id, reply) => {
                let _ = reply.send(self.handle_remove(client_id, consumer_id));
            }
            Command::Viewport(client_id, consumer_id, generation, viewport, reply) => {
                let result =
                    authorize_consumer(&self.engine, client_id, consumer_id).and_then(|()| {
                        match self.engine.set_viewport(consumer_id, generation, viewport) {
                            Ok(()) => {
                                self.request_viewport_history(consumer_id, generation, viewport)
                            }
                            Err(EngineError::StaleConsumerGeneration { .. }) => Ok(()),
                            Err(error) => Err(error.to_string()),
                        }
                    });
                let _ = reply.send(result);
            }
            Command::ResourceClass(client_id, consumer_id, resource_class, reply) => {
                let result =
                    authorize_consumer(&self.engine, client_id, consumer_id).and_then(|()| {
                        let publication = self
                            .engine
                            .set_resource_class(consumer_id, resource_class)
                            .map_err(|error| error.to_string())?;
                        if let Some(publication) = publication
                            && let Some(events) = self.events.get_mut(&consumer_id)
                        {
                            publish_ready(events, &publication);
                        }
                        Ok(())
                    });
                self.reconcile_rithmic_order_books();
                self.release_unused_live_market_data();
                let _ = reply.send(result);
            }
            Command::Demand(client_id, consumer_id, generation, series, reply) => {
                self.handle_demand(
                    client_id,
                    &series,
                    DemandWaiter {
                        consumer_id,
                        generation,
                        started_at: Instant::now(),
                    },
                    &reply,
                );
            }
            Command::SearchProviderInstruments(client_id, search, reply) => {
                self.handle_provider_search(client_id, search, &reply);
            }
            Command::SelectProviderInstrument(client_id, selection, reply) => {
                self.handle_provider_selection(client_id, selection, &reply);
            }
            Command::InstallProviderInstrument(instrument, reply) => {
                let _ = reply.send(self.install_provider_instrument(&instrument));
            }
            Command::Poll(client_id, consumer_id, reply) => {
                self.handle_poll(client_id, consumer_id, &reply);
            }
            Command::HistoryCompleted(..)
            | Command::LocalHistoryCompleted(..)
            | Command::ViewportHistoryLocalCompleted(..)
            | Command::PersistenceCompleted(..)
            | Command::ConfirmedEmptyRecorded(..)
            | Command::ConfirmedEmptyResolved(..)
            | Command::RestoreHotSet(..)
            | Command::SetResourceMode(..)
            | Command::Status(..)
            | Command::Attach(..)
            | Command::Detach(..) => unreachable!("command was routed to the wrong dispatcher"),
        }
    }

    fn handle_register(&mut self, identity: ConsumerIdentity) -> Result<(), String> {
        if !self.attached.contains(&identity.client_id) {
            return Err("client must attach before registering consumers".to_string());
        }
        self.engine
            .register_consumer(identity, true)
            .map_err(|error| error.to_string())?;
        if let Some(control) = self.coinbase_catalog_control
            && let Err(error) = control.authorize_consumer(identity.consumer_id.0.get())
        {
            self.engine.remove_consumer(identity.consumer_id);
            return Err(error);
        }
        self.events
            .insert(identity.consumer_id, ConsumerEvents::default());
        Ok(())
    }

    fn handle_remove(
        &mut self,
        client_id: ClientId,
        consumer_id: ConsumerId,
    ) -> Result<(), String> {
        authorize_consumer(&self.engine, client_id, consumer_id)?;
        self.events.remove(&consumer_id);
        self.remove_waiter(consumer_id);
        self.active_viewports.remove(&consumer_id);
        self.engine.remove_consumer(consumer_id);
        if let Some(control) = self.coinbase_catalog_control {
            control.release_consumer(consumer_id.0.get());
        }
        self.release_unused_live_market_data();
        Ok(())
    }

    fn handle_provider_search(
        &self,
        client_id: ClientId,
        search: SearchProviderInstruments,
        reply: &Reply<()>,
    ) {
        let consumer_id = search.consumer_id;
        let provider = search.provider.clone();
        let result = self
            .authorize_catalog_consumer(client_id, consumer_id)
            .and_then(|()| {
                if provider == "coinbase" {
                    self.coinbase_catalog_control
                        .ok_or_else(|| "coinbase catalog worker is unavailable".to_string())?
                        .try_search(search)
                        .map_err(coinbase_catalog_dispatch_error)
                } else {
                    self.dispatch_rithmic_catalog_control(
                        RithmicCatalogControl::Search(search),
                        &provider,
                    )
                }
            });
        let _ = reply.send(result);
    }

    fn handle_provider_selection(
        &self,
        client_id: ClientId,
        selection: SelectProviderInstrument,
        reply: &Reply<()>,
    ) {
        let consumer_id = selection.consumer_id;
        let provider = selection.provider.clone();
        let result = self
            .authorize_catalog_consumer(client_id, consumer_id)
            .and_then(|()| {
                if provider == "coinbase" {
                    self.coinbase_catalog_control
                        .ok_or_else(|| "coinbase catalog worker is unavailable".to_string())?
                        .try_select(selection)
                        .map_err(coinbase_catalog_dispatch_error)
                } else {
                    self.dispatch_rithmic_catalog_control(
                        RithmicCatalogControl::Select(selection),
                        &provider,
                    )
                }
            });
        let _ = reply.send(result);
    }

    fn handle_attach(&mut self, client_id: ClientId, reply: &Reply<()>) {
        let result = self
            .attached
            .insert(client_id)
            .then_some(())
            .ok_or_else(|| "client identity is already attached".to_string());
        let _ = reply.send(result);
    }

    fn apply_resource_mode(&mut self, mode: ResourceMode) {
        self.resource_mode = mode;
        self.refresh_resource_policy();
        if mode == ResourceMode::OfflineSuspended {
            self.suspend_provider_work();
        } else if mode == ResourceMode::MarketsLive {
            self.activate_markets_live_hot_set();
        }
        self.release_unused_live_market_data();
    }

    fn restore_hot_series(&mut self, series: Vec<WarmSeries>) {
        for warm in series
            .into_iter()
            .take(self.resource_policy.maximum_derived_series.max(1))
        {
            let generation = ProviderGeneration(
                NonZeroU64::new(warm.provider_watermark.max(1)).unwrap_or(NonZeroU64::MIN),
            );
            if warm.series.provider_id == "coinbase" {
                self.catalog.insert(
                    (
                        warm.instrument.provider.clone(),
                        warm.instrument.instrument_id.clone(),
                    ),
                    warm.instrument.clone(),
                );
                self.catalog_sessions
                    .insert("coinbase".to_string(), COINBASE_PROVIDER_GENERATION);
            }
            self.warm_priority.push(warm.series.clone());
            self.warm_series.insert(warm.series.clone(), warm.clone());
            if self
                .storage
                .try_send(StorageRequest::Read(warm.series.clone(), generation))
                .is_ok()
            {
                self.warming.insert((warm.series, generation));
            }
        }
    }

    fn activate_markets_live_hot_set(&mut self) {
        self.retained_live
            .extend(self.live.keys().chain(self.rithmic_live.keys()).cloned());
        let priority = self.warm_priority.clone();
        for series in priority
            .iter()
            .filter(|series| series.provider_id == "coinbase")
        {
            self.retained_live.insert(series.clone());
            if let Ok(generation) = self.provider_generation_for_series(series) {
                let _ = self.ensure_realtime(series);
                let _ = self.enqueue_history(series, generation);
            }
        }
        let Some(series) = priority
            .into_iter()
            .find(|series| series.provider_id == "rithmic")
        else {
            return;
        };
        let Some(warm) = self.warm_series.get(&series) else {
            return;
        };
        let Some(control) = self.rithmic_catalog_control else {
            return;
        };
        self.warm_rithmic_search_generation = self
            .warm_rithmic_search_generation
            .checked_add(1)
            .unwrap_or(1);
        let _ = control.try_send(RithmicCatalogControl::Search(SearchProviderInstruments {
            consumer_id: 0,
            search_generation: self.warm_rithmic_search_generation,
            provider: "rithmic".to_string(),
            query: warm.instrument.provider_symbol.clone(),
            maximum_results: 16,
        }));
    }

    fn refresh_resource_policy(&mut self) {
        let metrics = self.engine.metrics();
        if metrics.active_consumers > 0 {
            self.last_consumer_activity = Instant::now();
        }
        self.resource_policy = decide_resource_policy(ResourcePolicyInput {
            mode: resource_policy_mode(self.resource_mode),
            available_memory_bytes: self.available_memory_bytes,
            consumer_count: metrics.active_consumers,
            visible_consumer_count: self.engine.visible_consumer_count(),
            provider_series_limit: MAXIMUM_SERIES,
            hot_set_priority_count: self.hot_set_priority_count,
        });
        self.enforce_resource_policy();
    }

    fn enforce_resource_policy(&mut self) {
        let metrics = self.engine.metrics();
        let retention_expired = metrics.active_consumers == 0
            && self.last_consumer_activity.elapsed()
                >= Duration::from_secs(self.resource_policy.warm_retention_seconds);
        let retained_market_series = if self.resource_mode == ResourceMode::MarketsLive {
            self.live.len().saturating_add(self.rithmic_live.len())
        } else {
            0
        };
        let maximum_cached_series = if retention_expired {
            0
        } else {
            self.resource_policy
                .maximum_derived_series
                .max(retained_market_series)
        };
        let maximum_decoded_bars = if retention_expired {
            0
        } else {
            self.resource_policy.maximum_decoded_bars
        };
        let warm_limit = if retention_expired {
            0
        } else {
            self.resource_policy.maximum_derived_series
        };
        let protected = self
            .live
            .keys()
            .chain(self.rithmic_live.keys())
            .chain(self.warm_priority.iter().take(warm_limit))
            .cloned()
            .collect::<Vec<_>>();
        self.engine.evict_unsubscribed_series(
            maximum_cached_series,
            maximum_decoded_bars,
            &protected,
        );
        self.reconcile_rithmic_order_books();
    }

    fn reconcile_rithmic_order_books(&mut self) {
        let mut required_identities = BTreeSet::new();
        if self.resource_mode != ResourceMode::OfflineSuspended {
            for consumer_id in self.events.keys() {
                let Some(demand) = self.engine.current_demand(*consumer_id) else {
                    continue;
                };
                let Some(series) = demand.series.as_ref() else {
                    continue;
                };
                if series.provider_id == "rithmic"
                    && demand.streams.is_some_and(|streams| {
                        streams.contains(MarketStream::Depth)
                            && (demand.resource_class == ConsumerResourceClass::Foreground
                                || self.resource_policy.retain_hidden_depth)
                    })
                {
                    required_identities
                        .insert((series.instrument_id.clone(), series.entitlement_id.clone()));
                }
            }
            if self.resource_mode == ResourceMode::MarketsLive {
                required_identities.extend(
                    self.retained_live
                        .iter()
                        .filter(|series| {
                            series.provider_id == "rithmic"
                                && chart_stream_requirements(series).contains(MarketStream::Depth)
                        })
                        .map(|series| {
                            (series.instrument_id.clone(), series.entitlement_id.clone())
                        }),
                );
            }
        }
        let required = self
            .catalog
            .values()
            .filter(|instrument| {
                instrument.provider == "rithmic"
                    && required_identities.contains(&(
                        instrument.instrument_id.clone(),
                        instrument.entitlement_id.clone(),
                    ))
            })
            .cloned()
            .collect::<Vec<_>>();
        self.rithmic_order_books.retain(|instrument_id, book| {
            required.iter().any(|instrument| {
                instrument.instrument_id == *instrument_id && instrument == &book.instrument
            })
        });
        for instrument in required {
            let replace = self
                .rithmic_order_books
                .get(&instrument.instrument_id)
                .is_none_or(|book| book.instrument != instrument);
            if replace {
                self.rithmic_order_books.insert(
                    instrument.instrument_id.clone(),
                    RithmicOrderBook::new(instrument),
                );
            }
        }
    }

    fn suspend_provider_work(&mut self) {
        for stop in self.history_cancellations.values() {
            stop.store(true, Ordering::Release);
        }
        self.realtime_stop.store(true, Ordering::Release);
        if let Some(control) = self.rithmic_realtime_control {
            let _ = control.try_send(RithmicRealtimeControl::Stop);
        }
        self.realtime_started = false;
        self.realtime_connected = false;
        self.rithmic_realtime_started = false;
        self.realtime_products.clear();
        self.live.clear();
        self.rithmic_live.clear();
        self.rithmic_order_books.clear();
    }

    fn status(&self) -> MarketServiceStatus {
        let metrics = self.engine.metrics();
        MarketServiceStatus {
            resource_mode: self.resource_mode,
            connected_desktop_clients: self.attached.len(),
            providers: ["coinbase", "rithmic"]
                .into_iter()
                .filter_map(|provider| self.provider_state(provider))
                .collect(),
            retained_series: metrics.stored_series,
            retained_bars: metrics.stored_bars,
            approximate_series_bytes: metrics.approximate_series_bytes,
        }
    }

    fn provider_state(&self, provider: &str) -> Option<ProviderState> {
        let status = self.engine.provider_status(provider)?;
        let state = match status.health {
            ProviderHealth::Disconnected => ProviderConnectionState::Disconnected,
            ProviderHealth::Connecting => ProviderConnectionState::Connecting,
            ProviderHealth::Online => ProviderConnectionState::Online,
            ProviderHealth::Recovering => ProviderConnectionState::Recovering,
            ProviderHealth::Failed => ProviderConnectionState::Failed,
        };
        Some(ProviderState {
            provider: provider.to_string(),
            state: state as i32,
            generation: status.generation.map_or(0, |generation| generation.0.get()),
            detail: None,
        })
    }

    fn authorize_catalog_consumer(
        &self,
        client_id: ClientId,
        consumer_id: u64,
    ) -> Result<(), String> {
        authorize_consumer(&self.engine, client_id, ConsumerId(id(consumer_id)?))
    }

    fn handle_poll(
        &mut self,
        client_id: ClientId,
        consumer_id: ConsumerId,
        reply: &Reply<Option<envelope::Payload>>,
    ) {
        let result = authorize_consumer(&self.engine, client_id, consumer_id).map(|()| {
            self.events
                .get_mut(&consumer_id)
                .and_then(ConsumerEvents::pop)
        });
        let _ = reply.send(result);
    }

    fn dispatch_rithmic_catalog_control(
        &self,
        control: RithmicCatalogControl,
        provider: &str,
    ) -> Result<(), String> {
        let sender = self
            .rithmic_catalog_control
            .ok_or_else(|| format!("{provider} catalog worker is unavailable"))?;
        sender.try_send(control).map_err(|error| match error {
            TrySendError::Full(_) => {
                format!("{provider} catalog command capacity is exhausted")
            }
            TrySendError::Disconnected(_) => format!("{provider} catalog worker is unavailable"),
        })
    }

    fn install_provider_instrument(
        &mut self,
        instrument: &InstallProviderInstrument,
    ) -> Result<(), String> {
        validate_provider_instrument(instrument)?;
        let provider = instrument.provider.clone();
        let provider_generation = ProviderGeneration(
            NonZeroU64::new(instrument.session_generation)
                .ok_or_else(|| "provider instrument session is invalid".to_string())?,
        );
        let engine_generation = self
            .engine
            .provider_status(&provider)
            .and_then(|status| status.generation);
        let session = self.catalog_sessions.get(&provider).copied();
        if session.is_some_and(|current| instrument.session_generation < current) {
            return Err("provider instrument session is stale".to_string());
        }
        let newer_session = session.is_none_or(|current| instrument.session_generation > current);
        let selection = (provider == "rithmic" && !newer_session)
            .then(|| self.catalog_selections.get(&provider).copied())
            .flatten();
        if selection.is_some_and(|current| instrument.selection_generation < current) {
            return Err("provider instrument selection is stale".to_string());
        }
        let key = (provider.clone(), instrument.instrument_id.clone());
        if selection == Some(instrument.selection_generation) {
            return self
                .catalog
                .get(&key)
                .filter(|installed| *installed == instrument)
                .map(|_| ())
                .ok_or_else(|| "provider instrument selection conflicts".to_string());
        }
        let retained_catalog_len = if newer_session {
            self.catalog
                .keys()
                .filter(|(installed_provider, _)| installed_provider != &provider)
                .count()
        } else {
            self.catalog.len()
        };
        let key_exists_after_reset = !newer_session && self.catalog.contains_key(&key);
        if !key_exists_after_reset && retained_catalog_len >= MAXIMUM_CATALOG_INSTRUMENTS {
            return Err("provider instrument catalog capacity is exhausted".to_string());
        }
        // A newer install generation proves the provider opened a newer catalog
        // session, so the engine session must advance with it: Rithmic history
        // demands are fenced against the engine generation, and the realtime
        // worker only announces its generation after history succeeds.
        if engine_generation.is_none_or(|current| provider_generation > current) {
            self.engine
                .begin_provider_session(&provider, provider_generation)
                .map_err(|error| error.to_string())?;
        }
        if newer_session {
            for ((series, _), stop) in &self.history_cancellations {
                if series.provider_id == provider {
                    stop.store(true, Ordering::Release);
                }
            }
            self.catalog
                .retain(|(installed_provider, _), _| installed_provider != &provider);
            self.catalog_sessions
                .insert(provider.clone(), instrument.session_generation);
            self.catalog_selections.remove(&provider);
        }
        if provider == "rithmic" {
            self.catalog_selections
                .insert(provider.clone(), instrument.selection_generation);
        }
        let instrument_id = instrument.instrument_id.clone();
        self.catalog.insert(key, instrument.clone());
        self.reconcile_rithmic_order_books();
        self.activate_retained_instrument(instrument, provider_generation);
        if self.resource_mode == ResourceMode::MarketsLive
            && let Some(series) = self
                .warm_priority
                .iter()
                .find(|series| {
                    series.provider_id == provider && series.instrument_id == instrument_id
                })
                .cloned()
        {
            self.retained_live.insert(series.clone());
            let _ = self.enqueue_history(&series, provider_generation);
        }
        Ok(())
    }

    fn activate_retained_instrument(
        &mut self,
        instrument: &InstallProviderInstrument,
        generation: ProviderGeneration,
    ) {
        let series = self
            .retained_history
            .keys()
            .filter(|series| {
                series.provider_id == instrument.provider
                    && series.instrument_id == instrument.instrument_id
                    && series.entitlement_id == instrument.entitlement_id
            })
            .cloned()
            .collect::<Vec<_>>();
        let (Ok(price_scale), Ok(quantity_scale)) = (
            u8::try_from(instrument.price_scale),
            u8::try_from(instrument.quantity_scale),
        ) else {
            return;
        };
        for series in series {
            let Some(stored) = self.retained_history.remove(&series) else {
                continue;
            };
            if self
                .engine
                .install_retained_history(
                    generation,
                    &series,
                    price_scale,
                    quantity_scale,
                    stored.bars,
                )
                .is_ok()
            {
                self.prewarmed.insert(series.clone());
                self.local_loaded.insert((series, generation));
            }
        }
    }

    fn accept_series_demand(
        &mut self,
        client_id: ClientId,
        series: &BarSeriesKey,
        waiter: &DemandWaiter,
    ) -> Result<
        (
            ProviderGeneration,
            Option<axiusflow_market_engine::ConsumerPublication>,
        ),
        String,
    > {
        authorize_consumer(&self.engine, client_id, waiter.consumer_id)?;
        let provider_generation = self.provider_generation_for_series(series)?;
        let streams = chart_stream_requirements(series);
        let publication = self
            .engine
            .set_series_demand_with_streams(waiter.consumer_id, waiter.generation, series, streams)
            .map_err(|error| error.to_string())?;
        self.reconcile_rithmic_order_books();
        self.remove_waiter(waiter.consumer_id);
        if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
            events.snapshot = None;
            events.series_state = None;
            events.demand_error = None;
        }
        Ok((provider_generation, publication))
    }

    fn handle_demand(
        &mut self,
        client_id: ClientId,
        series: &BarSeriesKey,
        waiter: DemandWaiter,
        reply: &Reply<()>,
    ) {
        if self.resource_mode == ResourceMode::OfflineSuspended {
            let _ = reply.send(Err(
                "engine market work is offline and suspended".to_string()
            ));
            return;
        }
        if let Err(error) = self.validate_coinbase_realtime_capacity(series, waiter.consumer_id) {
            let _ = reply.send(Err(error));
            return;
        }
        let (provider_generation, publication) =
            match self.accept_series_demand(client_id, series, &waiter) {
                Ok(accepted) => accepted,
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            };
        self.active_viewports.remove(&waiter.consumer_id);
        self.prune_unused_live_series();
        self.prune_history_tracking();
        let realtime = if series.provider_id == "rithmic" {
            Ok(())
        } else {
            self.ensure_realtime(series)
        };
        self.stop_realtime_if_idle();
        if let Err(error) = realtime {
            let _ = reply.send(Err(error));
            return;
        }
        self.publish_order_book_to_consumer(waiter.consumer_id);
        let result = match publication {
            None => {
                if let Some(snapshot) = self.engine.series_snapshot(series) {
                    self.prepare_cached_demand(series, provider_generation, &snapshot)
                        .map(|_| ())
                } else {
                    self.start_uncached_demand(series, provider_generation, waiter);
                    Ok(())
                }
            }
            Some(publication) => {
                self.publish_cached_demand(series, provider_generation, &waiter, &publication)
            }
        };
        let _ = reply.send(result);
    }

    fn publish_cached_demand(
        &mut self,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        waiter: &DemandWaiter,
        publication: &axiusflow_market_engine::ConsumerPublication,
    ) -> Result<(), String> {
        let needs_covering_repair =
            self.prepare_cached_demand(series, provider_generation, &publication.snapshot)?;
        if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
            if needs_covering_repair {
                publish_state(
                    events,
                    publication,
                    SeriesLoadState::Partial,
                    PersistenceState::Durable,
                    Some("Showing retained local history while provider coverage repairs"),
                );
            } else {
                publish_ready(events, publication);
            }
        }
        Ok(())
    }

    fn prepare_cached_demand(
        &mut self,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        snapshot: &Arc<axiusflow_market_engine::SeriesSnapshot>,
    ) -> Result<bool, String> {
        let needs_covering_repair =
            snapshot.provider_generation != provider_generation || self.prewarmed.remove(series);
        if series.provider_id == "rithmic"
            && !needs_covering_repair
            && let Err(error) = self.start_rithmic_realtime_from_snapshot(series, snapshot)
        {
            return Err(error);
        }
        if series.provider_id == "coinbase"
            && !self.live.get_mut(series).is_none_or(|live| {
                if live.history_ready {
                    return true;
                }
                live.aggregator.reset();
                let seeded = if snapshot.forming {
                    live.aggregator.seed_canonical_backfill(&snapshot.bars)
                } else {
                    live.aggregator.seed_canonical_history(&snapshot.bars)
                };
                if seeded.is_ok() {
                    live.history_ready = true;
                }
                seeded.is_ok()
            })
        {
            return Err("Coinbase cached history/live handoff failed".to_string());
        }
        if needs_covering_repair {
            self.engine.invalidate_series(series);
            self.local_loaded
                .insert((series.clone(), provider_generation));
            if let Err(detail) = self.enqueue_history(series, provider_generation) {
                self.broadcast_series_resolution_for(
                    series,
                    SeriesLoadState::Partial,
                    PersistenceState::Durable,
                    Some(detail),
                );
            }
        } else {
            self.series_live_if_ready(series);
        }
        Ok(needs_covering_repair)
    }

    fn start_uncached_demand(
        &mut self,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        waiter: DemandWaiter,
    ) {
        let first = !self.pending.contains_key(series);
        if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
            events.series_state = Some(series_state(
                waiter.consumer_id,
                waiter.generation,
                ipc_series(series),
                SeriesLoadState::Resolving,
                None,
            ));
        }
        self.pending.entry(series.clone()).or_default().push(waiter);
        if !first {
            return;
        }
        let derived = series.provider_id == "coinbase"
            && matches!(
                self.derive_compatible_history(series, provider_generation),
                Ok(true)
            );
        let local_unavailable = !derived
            && self
                .enqueue_local_history(series, provider_generation)
                .is_err();
        if (derived || local_unavailable)
            && let Err(detail) = self.enqueue_history(series, provider_generation)
            && let Some(waiters) = self.pending.remove(series)
        {
            fail_waiters(
                &mut self.events,
                waiters,
                series,
                FailureStage::ProviderHistory,
                detail,
            );
        }
    }

    fn derive_compatible_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) -> Result<bool, String> {
        let target_seconds = match series.period {
            BarPeriod::Time { seconds } if seconds > 60 && seconds % 60 == 0 => seconds,
            BarPeriod::Time { .. }
            | BarPeriod::Tick { .. }
            | BarPeriod::Session { .. }
            | BarPeriod::Week { .. }
            | BarPeriod::Month { .. } => return Ok(false),
        };
        let Some(source) = self.engine.compatible_series_snapshot(series, generation) else {
            return Ok(false);
        };
        let mut source_bars = source.bars.to_vec();
        if source.forming {
            source_bars.pop();
        }
        let interval = coinbase_interval(target_seconds)?;
        let (bars, _) = aggregate_coinbase_bars(&source_bars, interval)?;
        if bars.is_empty() {
            return Ok(false);
        }
        let publications = self
            .engine
            .install_history(
                generation,
                series,
                source.price_scale,
                source.quantity_scale,
                bars.clone(),
            )
            .map_err(|error| error.to_string())?;
        self.record_coinbase_history_coverage(series, &bars)?;
        self.local_loaded.insert((series.clone(), generation));
        for publication in publications {
            if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                publish_state(
                    events,
                    &publication,
                    SeriesLoadState::Partial,
                    PersistenceState::Pending,
                    Some("Showing compatible in-memory history while provider repair runs"),
                );
            }
        }
        self.enqueue_persistence(
            series,
            generation,
            bars,
            true,
            "Derived history persistence is unavailable",
        );
        Ok(true)
    }

    fn enqueue_persistence(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        bars: Vec<MarketBar>,
        derived: bool,
        unavailable: &'static str,
    ) {
        let repaired_range = (!derived && series.provider_id == "coinbase")
            .then(|| coinbase_bar_coverage_ranges(series, &bars).ok())
            .flatten()
            .and_then(|ranges| {
                Some(HistoryRange {
                    start_unix_nanos: ranges.first()?.start_unix_nanos,
                    end_unix_nanos: ranges.last()?.end_unix_nanos,
                })
            });
        if self
            .storage
            .try_send(StorageRequest::Persist(
                series.clone(),
                generation,
                bars,
                derived,
                self.protected_history_ranges(),
                Instant::now(),
            ))
            .is_err()
        {
            if let Some(range) = repaired_range {
                self.remember_pending_empty_repair(series, range);
            }
            self.broadcast_persistence_for(series, PersistenceState::Degraded, Some(unavailable));
            self.broadcast_demand_error_for(
                series,
                FailureStage::FilesystemWrite,
                unavailable,
                Some(0),
            );
        }
    }

    fn remember_pending_empty_repair(&mut self, series: &BarSeriesKey, range: HistoryRange) {
        self.pending_empty_repairs
            .entry(series.clone())
            .and_modify(|pending| {
                pending.start_unix_nanos = pending.start_unix_nanos.min(range.start_unix_nanos);
                pending.end_unix_nanos = pending.end_unix_nanos.max(range.end_unix_nanos);
            })
            .or_insert(range);
    }

    fn retry_pending_empty_repairs(&mut self) {
        if self.pending_empty_repairs.is_empty() || Instant::now() < self.empty_repair_retry_at {
            return;
        }
        let Some((series, range)) = self
            .pending_empty_repairs
            .iter()
            .next()
            .map(|(series, range)| (series.clone(), *range))
        else {
            return;
        };
        match self
            .storage
            .try_send(StorageRequest::ResolveConfirmedEmpty(series.clone(), range))
        {
            Ok(()) => {
                self.pending_empty_repairs.remove(&series);
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.empty_repair_retry_at = Instant::now() + EMPTY_REPAIR_RETRY_DELAY;
            }
        }
    }

    fn protected_history_ranges(&self) -> Vec<(BarSeriesKey, HistoryRange)> {
        let mut protected = BTreeSet::new();
        for (consumer_id, viewport) in &self.active_viewports {
            if self
                .engine
                .current_demand(*consumer_id)
                .is_some_and(|demand| {
                    demand.generation == Some(viewport.consumer_generation)
                        && demand.series.as_ref() == Some(&viewport.series)
                })
                && self
                    .engine
                    .provider_status(&viewport.series.provider_id)
                    .and_then(|status| status.generation)
                    == Some(viewport.provider_generation)
            {
                protected.insert((viewport.series.clone(), viewport.range));
            }
        }
        for series in self.live.keys().chain(self.rithmic_live.keys()) {
            if !self.engine.has_subscription(series) {
                continue;
            }
            let Some(snapshot) = self.engine.series_snapshot(series) else {
                continue;
            };
            let Some(first) = snapshot.bars.get(
                snapshot
                    .bars
                    .len()
                    .saturating_sub(VIEWPORT_LIVE_TAIL_RESERVE),
            ) else {
                continue;
            };
            let Some(last) = snapshot.bars.last() else {
                continue;
            };
            let end = last
                .exchange_timestamp_unix_nanos
                .saturating_add(series.period.duration_nanos().unwrap_or(1).max(1));
            if first.exchange_timestamp_unix_nanos < end {
                protected.insert((
                    series.clone(),
                    HistoryRange {
                        start_unix_nanos: first.exchange_timestamp_unix_nanos,
                        end_unix_nanos: end,
                    },
                ));
            }
        }
        protected.into_iter().collect()
    }

    fn enqueue_local_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) -> Result<(), &'static str> {
        let key = (series.clone(), generation);
        if self.local_history_deadlines.contains_key(&key) {
            return Ok(());
        }
        match self
            .storage
            .try_send(StorageRequest::Read(series.clone(), generation))
        {
            Ok(()) => {
                self.local_history_deadlines
                    .insert(key, Instant::now() + LOCAL_HISTORY_READ_TIMEOUT);
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err("local history capacity is temporarily exhausted"),
            Err(TrySendError::Disconnected(_)) => Err("local history worker is unavailable"),
        }
    }

    fn local_history_completed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        result: Result<Option<StoredHistory>, String>,
    ) {
        if self.warming.remove(&(series.clone(), generation)) {
            self.install_warm_local_history(series, generation, result);
            return;
        }
        let expected = self
            .local_history_deadlines
            .remove(&(series.clone(), generation))
            .is_some();
        if !expected && !self.pending.contains_key(series) {
            return;
        }
        if self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        match result {
            Ok(Some(stored)) if !stored.bars.is_empty() => {
                let Ok((price_scale, quantity_scale)) = self.series_precision(series) else {
                    return;
                };
                let persistence = if stored.durable {
                    PersistenceState::Durable
                } else {
                    PersistenceState::Degraded
                };
                let coverage = coinbase_bar_coverage_ranges(series, &stored.bars);
                if let Ok(publications) = self.engine.install_history(
                    generation,
                    series,
                    price_scale,
                    quantity_scale,
                    stored.bars,
                ) {
                    if let Ok(ranges) = coverage {
                        for range in ranges {
                            record_covered_range(&mut self.history_coverage, series, range);
                        }
                    }
                    self.local_loaded.insert((series.clone(), generation));
                    for publication in publications {
                        if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                            publish_state(
                                events,
                                &publication,
                                SeriesLoadState::Partial,
                                persistence,
                                Some(if stored.derived && !stored.durable {
                                    "Showing derived history from retained one-minute data; derived-cache persistence is unavailable"
                                } else if stored.derived {
                                    "Showing retained derived history while provider repair runs"
                                } else {
                                    "Showing retained local history while provider repair runs"
                                }),
                            );
                        }
                    }
                }
            }
            Ok(Some(_) | None) => {}
            Err(_) => {
                self.broadcast_persistence_for(
                    series,
                    PersistenceState::Degraded,
                    Some("Local history is unavailable; provider repair continues"),
                );
            }
        }
        if let Err(detail) = self.enqueue_history(series, generation)
            && let Some(waiters) = self.pending.remove(series)
        {
            fail_waiters(
                &mut self.events,
                waiters,
                series,
                FailureStage::ProviderHistory,
                detail,
            );
        }
    }

    fn viewport_history_local_completed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: HistoryRange,
        result: Result<LocalRangeHistory, String>,
    ) {
        self.viewport_history_local_inflight
            .remove(&(series.clone(), generation, range));
        if self
            .viewport_history_ranges
            .get(&(series.clone(), generation))
            != Some(&range)
            || self
                .engine
                .provider_status(&series.provider_id)
                .and_then(|status| status.generation)
                != Some(generation)
        {
            return;
        }
        let Ok(local) = result else {
            if let Err(detail) = self.schedule_coinbase_history(
                series,
                generation,
                HistoryRequestKind::ViewportBackfill,
            ) {
                self.broadcast_demand_error_for(
                    series,
                    FailureStage::ProviderHistory,
                    &detail,
                    None,
                );
            }
            return;
        };
        for confirmed_empty in local.confirmed_empty {
            record_covered_range(&mut self.history_coverage, series, confirmed_empty);
        }
        if let Some(stored) = local.stored
            && !stored.bars.is_empty()
        {
            let bars = self
                .engine
                .series_snapshot(series)
                .and_then(|current| {
                    reconcile_history_repair(
                        &current,
                        stored.bars.clone(),
                        HISTORY_BARS_PER_SERIES,
                        coinbase_series_interval(series).ok(),
                        self.viewport_history_ranges
                            .get(&(series.clone(), generation))
                            .copied(),
                    )
                    .ok()
                })
                .unwrap_or(stored.bars);
            if let Ok((price_scale, quantity_scale)) = self.series_precision(series)
                && let Ok(publications) = self.engine.replace_covering_history(
                    generation,
                    series,
                    price_scale,
                    quantity_scale,
                    bars.clone(),
                    true,
                )
            {
                if let Ok(ranges) = coinbase_bar_coverage_ranges(series, &bars) {
                    for covered in ranges {
                        record_covered_range(&mut self.history_coverage, series, covered);
                    }
                }
                for publication in publications {
                    if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                        publish_state(
                            events,
                            &publication,
                            SeriesLoadState::Partial,
                            PersistenceState::Durable,
                            Some("Showing retained local history while visible coverage repairs"),
                        );
                    }
                }
            }
        }
        if let Err(detail) =
            self.schedule_coinbase_history(series, generation, HistoryRequestKind::ViewportBackfill)
        {
            self.broadcast_demand_error_for(series, FailureStage::ProviderHistory, &detail, None);
        }
    }

    fn install_warm_local_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        result: Result<Option<StoredHistory>, String>,
    ) {
        let Ok(Some(stored)) = result else {
            return;
        };
        if stored.bars.is_empty() {
            return;
        }
        if series.provider_id == "rithmic" {
            self.retained_history.insert(series.clone(), stored);
            return;
        }
        let Some(warm) = self.warm_series.get(series) else {
            return;
        };
        let (Ok(price_scale), Ok(quantity_scale)) = (
            u8::try_from(warm.instrument.price_scale),
            u8::try_from(warm.instrument.quantity_scale),
        ) else {
            return;
        };
        let coverage = coinbase_bar_coverage_ranges(series, &stored.bars);
        if self
            .engine
            .install_retained_history(generation, series, price_scale, quantity_scale, stored.bars)
            .is_err()
        {
            return;
        }
        if let Ok(ranges) = coverage {
            for range in ranges {
                record_covered_range(&mut self.history_coverage, series, range);
            }
        }
        self.prewarmed.insert(series.clone());
        self.local_loaded.insert((series.clone(), generation));
    }

    fn expire_local_history_reads(&mut self) {
        let now = Instant::now();
        let expired = self
            .local_history_deadlines
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for (series, generation) in expired {
            self.local_history_deadlines
                .remove(&(series.clone(), generation));
            self.broadcast_persistence_for(
                &series,
                PersistenceState::Degraded,
                Some("Local history read timed out; provider repair continues"),
            );
            if let Err(detail) = self.enqueue_history(&series, generation)
                && let Some(waiters) = self.pending.remove(&series)
            {
                fail_waiters(
                    &mut self.events,
                    waiters,
                    &series,
                    FailureStage::ProviderHistory,
                    detail,
                );
            }
        }
    }

    fn provider_generation_for_series(
        &self,
        series: &BarSeriesKey,
    ) -> Result<ProviderGeneration, String> {
        match series.provider_id.as_str() {
            "coinbase" => {
                self.coinbase_instrument(series)
                    .and_then(|instrument| coinbase_series_profile(series, instrument))?;
                Ok(self.coinbase_provider_generation())
            }
            "rithmic" => {
                crate::rithmic_history::chart_interval(series.period)?;
                if series.definition_version != 1 {
                    return Err("unsupported Rithmic engine series definition".to_string());
                }
                let installed = self.rithmic_instrument(series)?;
                if installed.entitlement_id != series.entitlement_id {
                    return Err("Rithmic series entitlement is inconsistent".to_string());
                }
                let generation = self
                    .engine
                    .provider_status("rithmic")
                    .and_then(|status| status.generation)
                    .ok_or_else(|| "Rithmic engine session is unavailable".to_string())?;
                Ok(generation)
            }
            _ => Err("resident engine market provider is unsupported".to_string()),
        }
    }

    fn series_precision(&self, series: &BarSeriesKey) -> Result<(u8, u8), String> {
        match series.provider_id.as_str() {
            "coinbase" => {
                let profile = coinbase_series_profile(series, self.coinbase_instrument(series)?)?;
                Ok((profile.price_scale, profile.quantity_scale))
            }
            "rithmic" => {
                let installed = self.rithmic_instrument(series)?;
                if installed.entitlement_id != series.entitlement_id {
                    return Err("Rithmic series entitlement is inconsistent".to_string());
                }
                Ok((
                    u8::try_from(installed.price_scale)
                        .map_err(|_| "Rithmic price scale is invalid".to_string())?,
                    u8::try_from(installed.quantity_scale)
                        .map_err(|_| "Rithmic quantity scale is invalid".to_string())?,
                ))
            }
            _ => Err("resident engine market provider is unsupported".to_string()),
        }
    }

    fn rithmic_instrument(
        &self,
        series: &BarSeriesKey,
    ) -> Result<&InstallProviderInstrument, String> {
        self.catalog
            .get(&(series.provider_id.clone(), series.instrument_id.clone()))
            .ok_or_else(|| "Rithmic instrument is not installed".to_string())
    }

    fn coinbase_instrument(
        &self,
        series: &BarSeriesKey,
    ) -> Result<&InstallProviderInstrument, String> {
        self.catalog
            .get(&(series.provider_id.clone(), series.instrument_id.clone()))
            .filter(|instrument| instrument.provider == "coinbase")
            .ok_or_else(|| "Coinbase instrument is not installed".to_string())
    }

    fn ensure_realtime(&mut self, series: &BarSeriesKey) -> Result<(), String> {
        let streams = self
            .engine
            .subscription_status(series)
            .map(|status| status.streams)
            .or_else(|| {
                (self.resource_mode == ResourceMode::MarketsLive
                    && self.retained_live.contains(series))
                .then(|| chart_stream_requirements(series))
            })
            .ok_or_else(|| "series has no accepted upstream subscription".to_string())?;
        if streams.contains(MarketStream::Trades) {
            self.engine
                .verify_provider_request(&series.provider_id, ProviderRequest::Trades)
                .map_err(|error| error.to_string())?;
        }
        if streams.contains(MarketStream::Quotes) {
            self.engine
                .verify_provider_request(&series.provider_id, ProviderRequest::Quotes)
                .map_err(|error| error.to_string())?;
        }
        if streams.contains(MarketStream::Depth) {
            self.engine
                .verify_provider_request(&series.provider_id, ProviderRequest::Depth)
                .map_err(|error| error.to_string())?;
        }
        if !streams.contains(MarketStream::Trades)
            && !streams.contains(MarketStream::Quotes)
            && !streams.contains(MarketStream::Depth)
        {
            return Ok(());
        }
        if series.provider_id == "rithmic" {
            if !self.rithmic_realtime_started
                && let Some(control) = self.rithmic_realtime_control
            {
                let instrument = self
                    .catalog
                    .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                    .cloned()
                    .ok_or_else(|| "Rithmic instrument is not installed".to_string())?;
                match control.try_send(RithmicRealtimeControl::Select(instrument)) {
                    Ok(()) => self.rithmic_realtime_started = true,
                    Err(TrySendError::Full(_)) => {
                        return Err("Rithmic live selection capacity is exhausted".to_string());
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        return Err("Rithmic live worker is unavailable".to_string());
                    }
                }
            }
            if !self.rithmic_live.contains_key(series) {
                let venue_id = self
                    .catalog
                    .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                    .ok_or_else(|| "Rithmic instrument is not installed".to_string())?
                    .venue_id
                    .clone();
                if let Some(mut handoff) = RithmicLiveHandoff::new(
                    series,
                    self.provider_generation_for_series(series)?,
                    &venue_id,
                ) {
                    handoff.connected = self
                        .engine
                        .provider_status("rithmic")
                        .is_some_and(|status| status.health == ProviderHealth::Online);
                    self.rithmic_live.insert(series.clone(), handoff);
                }
            }
            return Ok(());
        }
        if series.provider_id != "coinbase" {
            return Err("resident engine realtime provider is unsupported".to_string());
        }
        if !self.live.contains_key(series) {
            let instrument = self.coinbase_instrument(series)?.clone();
            let mut handoff =
                LiveHandoff::try_new(series, self.coinbase_provider_generation(), &instrument)?;
            handoff.connected = self.realtime_connected;
            self.live.insert(series.clone(), handoff);
        }
        self.sync_coinbase_realtime()?;
        Ok(())
    }

    fn validate_coinbase_realtime_capacity(
        &self,
        series: &BarSeriesKey,
        consumer_id: ConsumerId,
    ) -> Result<(), String> {
        if series.provider_id != "coinbase"
            || !chart_stream_requirements(series).contains(MarketStream::Trades)
        {
            return Ok(());
        }
        let replaced = self
            .engine
            .current_demand(consumer_id)
            .and_then(|demand| demand.series.as_ref());
        let mut products = BTreeSet::new();
        for active in self.live.keys() {
            let replaced_last_reference = replaced == Some(active)
                && self
                    .engine
                    .subscription_status(active)
                    .is_some_and(|status| status.consumer_count == 1);
            if !replaced_last_reference {
                products.insert(self.coinbase_instrument(active)?.provider_symbol.clone());
            }
        }
        products.insert(self.coinbase_instrument(series)?.provider_symbol.clone());
        if products.len() > axiusflow_coinbase_market_adapter::MAXIMUM_PRODUCTS {
            return Err("Coinbase realtime product capacity is exhausted".to_string());
        }
        Ok(())
    }

    fn enqueue_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) -> Result<(), &'static str> {
        let range = if series.provider_id == "coinbase" {
            recent_coinbase_history_range(series, self.resource_policy.history_prefetch_bars.max(1))
                .ok()
        } else {
            None
        };
        self.enqueue_history_request(series, generation, range, HistoryRequestKind::Initial)
    }

    fn request_viewport_history(
        &mut self,
        consumer_id: ConsumerId,
        generation: GenerationId,
        viewport: Viewport,
    ) -> Result<(), String> {
        let Some(demand) = self.engine.current_demand(consumer_id) else {
            return Err("market consumer is unavailable".to_string());
        };
        if demand.generation != Some(generation) {
            return Ok(());
        }
        let Some(series) = demand.series.clone() else {
            return Ok(());
        };
        if series.provider_id != "coinbase" {
            return Ok(());
        }
        let provider_generation = self.provider_generation_for_series(&series)?;
        let range = viewport_coinbase_history_range(&series, viewport)?;
        self.active_viewports.insert(
            consumer_id,
            ActiveViewport {
                consumer_generation: generation,
                provider_generation,
                series: series.clone(),
                range,
            },
        );
        if let Some(live) = self.live.get_mut(&series) {
            live.viewport_position = if viewport_follows_live(&series, range)? {
                ViewportPosition::Live
            } else {
                ViewportPosition::Historical
            };
        }
        let key = (series.clone(), provider_generation);
        let replaced = self.viewport_history_ranges.insert(key.clone(), range);
        if replaced.is_some_and(|previous| previous != range)
            && let Some(stop) = self.history_cancellations.get(&key)
        {
            stop.store(true, Ordering::Release);
        }
        let key = (series.clone(), provider_generation, range);
        if !self.viewport_history_local_inflight.insert(key.clone()) {
            return Ok(());
        }
        match self.storage.try_send(StorageRequest::ReadRange(
            series.clone(),
            provider_generation,
            range,
        )) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.viewport_history_local_inflight.remove(&key);
            }
        }
        self.schedule_coinbase_history(
            &series,
            provider_generation,
            HistoryRequestKind::ViewportBackfill,
        )
        .map_err(|error| error.clone())
    }

    fn arm_initial_viewport_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) {
        let Ok(range) = recent_coinbase_history_range(series, HISTORY_BARS_PER_SERIES) else {
            self.broadcast_demand_error_for(
                series,
                FailureStage::ProviderHistory,
                "Coinbase initial-history range is unavailable",
                None,
            );
            return;
        };
        if let Entry::Vacant(entry) = self
            .viewport_history_ranges
            .entry((series.clone(), generation))
        {
            entry.insert(range);
        }
    }

    fn should_arm_initial_viewport_history(
        &self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        kind: HistoryRequestKind,
        snapshot: &HistorySnapshot,
    ) -> bool {
        series.provider_id == "coinbase"
            && kind == HistoryRequestKind::Initial
            && !self.local_loaded.contains(&(series.clone(), generation))
            && snapshot.bars.len() >= self.resource_policy.history_prefetch_bars.max(1)
    }

    fn complete_coinbase_live_handoff(
        &mut self,
        series: &BarSeriesKey,
        bars: &[MarketBar],
    ) -> bool {
        let Some(live) = self.live.get_mut(series) else {
            return true;
        };
        let connected = live.connected;
        let buffered = std::mem::take(&mut live.buffered);
        let interval = coinbase_interval_nanos(series).ok();
        let seam_repair = bars.last().and_then(|last| {
            let interval = interval?;
            buffered
                .iter()
                .filter_map(|trade| {
                    coinbase_seam_repair(
                        last.exchange_timestamp_unix_nanos,
                        trade.trade_time_unix_nanos,
                        interval,
                    )
                })
                .min_by_key(|range| range.start_unix_nanos)
        });
        live.aggregator.reset();
        if live.aggregator.seed_canonical_history(bars).is_err() {
            self.realtime_interrupted(
                FailureStage::Handoff,
                "Coinbase history/live handoff failed",
            );
            return false;
        }
        live.connected = connected;
        live.seam_repair = seam_repair;
        live.history_ready = live.seam_repair.is_none();
        live.dirty = false;
        if live.history_ready
            && buffered
                .iter()
                .any(|trade| live.aggregator.apply_trade(trade).is_err())
        {
            self.realtime_interrupted(
                FailureStage::Handoff,
                "Coinbase history/live handoff failed",
            );
            return false;
        }
        if live.history_ready {
            live.dirty = live.aggregator.in_flight().is_some();
        } else {
            live.buffered = buffered.into_iter().collect();
        }
        live.published = bars
            .last()
            .map(|bar| PublishedTailState::Covering(bar.source_sequence));
        true
    }

    fn schedule_coinbase_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        kind: HistoryRequestKind,
    ) -> Result<(), String> {
        let key = (series.clone(), generation);
        if !self.engine.has_subscription(series) {
            self.viewport_history_ranges.remove(&key);
            return Ok(());
        }
        if self.history_inflight.contains_key(&key) {
            return Ok(());
        }
        let Some(requested) = self.viewport_history_ranges.get(&key).copied() else {
            return Ok(());
        };
        let coverage = CoverageSnapshot::try_new(
            self.history_coverage
                .get(series)
                .cloned()
                .unwrap_or_default(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .map_err(|error| error.to_string())?;
        let plan = coverage
            .plan(requested)
            .map_err(|error| error.to_string())?;
        let Some(missing) = plan.repair_ranges().first().copied() else {
            self.viewport_history_ranges.remove(&key);
            self.flush_deferred_publication(series);
            return Ok(());
        };
        let page = coinbase_history_page_range(series, missing)?;
        self.enqueue_history_request(series, generation, Some(page), kind)
            .map_err(str::to_string)
    }

    fn enqueue_history_request(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
        kind: HistoryRequestKind,
    ) -> Result<(), &'static str> {
        if self
            .engine
            .verify_provider_request(&series.provider_id, ProviderRequest::HistoricalBars)
            .is_err()
        {
            return Err("provider does not support historical bars");
        }
        let key = (series.clone(), generation);
        if self.history_inflight.contains_key(&key) {
            return Ok(());
        }
        let instrument = if matches!(series.provider_id.as_str(), "coinbase" | "rithmic") {
            self.catalog
                .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                .cloned()
        } else {
            None
        };
        let stop = Arc::new(AtomicBool::new(false));
        let request = HistoryRequest {
            series: series.clone(),
            provider_generation: generation,
            instrument,
            maximum_bars: self.resource_policy.history_prefetch_bars.max(1),
            range,
            kind,
            stop: Arc::clone(&stop),
        };
        let history = if series.provider_id == "rithmic" {
            self.rithmic_history
        } else {
            self.coinbase_history
        };
        match try_enqueue_history(history, request) {
            Ok(()) => {
                self.history_inflight.insert(key.clone(), range);
                self.history_cancellations.insert(key, stop);
                if kind == HistoryRequestKind::SeamRepair
                    && let Some(live) = self.live.get_mut(series)
                {
                    live.history_ready = false;
                }
                if kind == HistoryRequestKind::SeamRepair {
                    self.broadcast_series_recovery_for(
                        series,
                        "Repairing the Coinbase history/live seam",
                    );
                }
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn history_failed(&mut self, series: &BarSeriesKey, generation: ProviderGeneration) {
        if self.local_loaded.contains(&(series.clone(), generation)) {
            self.pending.remove(series);
            self.broadcast_series_resolution_for(
                series,
                SeriesLoadState::Partial,
                PersistenceState::Durable,
                Some("Retained local history is usable; provider repair is unavailable"),
            );
            if series.provider_id == "rithmic"
                && let Some(snapshot) = self.engine.series_snapshot(series)
            {
                let _ = self.start_rithmic_realtime_from_snapshot(series, &snapshot);
            }
        } else if let Some(waiters) = self.pending.remove(series) {
            fail_waiters(
                &mut self.events,
                waiters,
                series,
                FailureStage::ProviderHistory,
                if series.provider_id == "rithmic" {
                    "Rithmic historical bars are unavailable"
                } else {
                    "Coinbase historical bars are unavailable"
                },
            );
        }
        if series.provider_id == "coinbase" {
            self.broadcast_provider_for(
                "coinbase",
                ProviderConnectionState::Recovering,
                generation,
                Some("Coinbase history repair is retrying"),
            );
        }
    }

    fn handle_empty_history_snapshot(
        &mut self,
        series: &BarSeriesKey,
        kind: HistoryRequestKind,
        key: &(BarSeriesKey, ProviderGeneration),
        requested: Option<HistoryRange>,
        snapshot: &HistorySnapshot,
    ) -> bool {
        if kind != HistoryRequestKind::ViewportBackfill {
            return false;
        }
        if !snapshot.confirmed_empty {
            return false;
        }
        if let Some(requested) = requested
            && self
                .storage
                .try_send(StorageRequest::RecordConfirmedEmpty(
                    series.clone(),
                    requested,
                ))
                .is_err()
        {
            self.broadcast_persistence_for(
                series,
                PersistenceState::Degraded,
                Some("Confirmed-empty history persistence is unavailable"),
            );
        }
        if let Some(requested) = requested {
            record_covered_range(&mut self.history_coverage, series, requested);
        }
        if let Err(error) =
            self.schedule_coinbase_history(series, key.1, HistoryRequestKind::ViewportBackfill)
        {
            self.viewport_history_ranges.remove(key);
            self.broadcast_demand_error_for(series, FailureStage::ProviderHistory, &error, None);
        } else if !self.viewport_history_ranges.contains_key(key) {
            self.broadcast_series_resolution_for(
                series,
                SeriesLoadState::Partial,
                PersistenceState::Durable,
                Some("Coinbase has no bars in the requested range"),
            );
        }
        true
    }

    fn accept_history_completion(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
        kind: HistoryRequestKind,
        result: Result<HistorySnapshot, String>,
    ) -> Option<HistorySnapshot> {
        let key = (series.clone(), generation);
        self.history_inflight.remove(&key);
        let cancelled = self
            .history_cancellations
            .remove(&key)
            .is_some_and(|stop| stop.load(Ordering::Acquire));
        let current = self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation);
        if current != Some(generation) {
            let pending_viewport = self.viewport_history_ranges.remove(&key);
            if series.provider_id == "coinbase"
                && let Some(current) = current
                && self.live.contains_key(series)
            {
                if let Some(range) = pending_viewport {
                    self.viewport_history_ranges
                        .insert((series.clone(), current), range);
                }
                let _ = self.enqueue_history(series, current);
            }
            return None;
        }
        let Ok(snapshot) = result else {
            if cancelled {
                if self.viewport_history_ranges.contains_key(&key) {
                    let _ = self.schedule_coinbase_history(
                        series,
                        generation,
                        HistoryRequestKind::ViewportBackfill,
                    );
                } else if self.engine.has_subscription(series) {
                    let _ = self.enqueue_history(series, generation);
                }
            } else if kind == HistoryRequestKind::Initial {
                self.history_failed(series, generation);
            } else {
                self.viewport_backfill_failed(
                    series,
                    generation,
                    range,
                    kind,
                    "Visible history backfill is unavailable; retained data remains usable",
                );
            }
            return None;
        };
        if cancelled
            && kind != HistoryRequestKind::Initial
            && self.viewport_history_ranges.get(&key).copied() != range
        {
            let _ = self.schedule_coinbase_history(
                series,
                generation,
                HistoryRequestKind::ViewportBackfill,
            );
            return None;
        }
        Some(snapshot)
    }

    fn history_completed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        range: Option<HistoryRange>,
        kind: HistoryRequestKind,
        result: Result<HistorySnapshot, String>,
    ) {
        let key = (series.clone(), generation);
        let Some(snapshot) =
            self.accept_history_completion(series, generation, range, kind, result)
        else {
            return;
        };
        if snapshot.bars.is_empty()
            && self.handle_empty_history_snapshot(series, kind, &key, range, &snapshot)
        {
            return;
        }
        let arm_initial_viewport =
            self.should_arm_initial_viewport_history(series, generation, kind, &snapshot);
        let repair = kind != HistoryRequestKind::Initial;
        let replace_covering = series.provider_id == "coinbase" && repair;
        let provider_bars = snapshot.bars.clone();
        let Some(snapshot) = self.prepare_history_repair(series, generation, snapshot, repair)
        else {
            if repair {
                self.viewport_backfill_failed(
                    series,
                    generation,
                    range,
                    kind,
                    "Visible history could not be merged; retained data remains usable",
                );
            }
            return;
        };
        let persisted_bars = if replace_covering {
            provider_bars
        } else {
            snapshot.bars.clone()
        };
        let publish = !replace_covering || self.coinbase_repair_publishes(series, kind, range);
        if publish {
            self.deferred_publications.remove(series);
        } else {
            self.deferred_publications.insert(series.clone());
        }
        let Some(bars) = self.install_completed_history(
            series,
            generation,
            snapshot,
            replace_covering,
            persisted_bars,
            publish,
        ) else {
            if repair {
                self.viewport_backfill_failed(
                    series,
                    generation,
                    range,
                    kind,
                    "Visible history could not be installed; retained data remains usable",
                );
            }
            return;
        };
        let should_complete_handoff = kind != HistoryRequestKind::ViewportBackfill
            || self
                .live
                .get(series)
                .is_none_or(|live| live.viewport_position == ViewportPosition::Live);
        if should_complete_handoff && !self.complete_coinbase_live_handoff(series, &bars) {
            return;
        }
        let seam_repair = self.live.get(series).and_then(|live| live.seam_repair);
        self.pending.remove(series);
        self.series_live_if_ready(series);
        if let Err(error) = self.record_coinbase_history_coverage(series, &bars) {
            self.broadcast_demand_error_for(series, FailureStage::ProviderHistory, &error, None);
        }
        if arm_initial_viewport {
            self.arm_initial_viewport_history(series, generation);
        }
        if let Some(range) = seam_repair {
            self.viewport_history_ranges
                .insert((series.clone(), generation), range);
        }
        let continuation = if seam_repair.is_some() {
            self.schedule_coinbase_history(series, generation, HistoryRequestKind::SeamRepair)
        } else {
            self.schedule_coinbase_history(series, generation, HistoryRequestKind::ViewportBackfill)
        };
        if let Err(error) = continuation {
            self.viewport_history_ranges
                .remove(&(series.clone(), generation));
            self.broadcast_demand_error_for(series, FailureStage::ProviderHistory, &error, None);
        }
    }

    fn record_coinbase_history_coverage(
        &mut self,
        series: &BarSeriesKey,
        bars: &[MarketBar],
    ) -> Result<(), String> {
        for range in coinbase_bar_coverage_ranges(series, bars)? {
            record_covered_range(&mut self.history_coverage, series, range);
        }
        Ok(())
    }

    /// Reports whether one Coinbase covering repair is consumer-visible. Seam
    /// repairs sit at the live edge; interior pages publish only while a
    /// consumer viewport actually overlaps the fetched range, so background
    /// working-window fills stay off the IPC path until their plan resolves.
    fn coinbase_repair_publishes(
        &self,
        series: &BarSeriesKey,
        kind: HistoryRequestKind,
        page: Option<HistoryRange>,
    ) -> bool {
        if kind == HistoryRequestKind::SeamRepair {
            return true;
        }
        let Some(page) = page else {
            return false;
        };
        self.active_viewports.values().any(|viewport| {
            viewport.series == *series
                && viewport.range.start_unix_nanos < page.end_unix_nanos
                && page.start_unix_nanos < viewport.range.end_unix_nanos
        })
    }

    fn flush_deferred_publication(&mut self, series: &BarSeriesKey) {
        if !self.deferred_publications.remove(series) {
            return;
        }
        match self.engine.publish_series_snapshot(series) {
            Ok(publications) => {
                for publication in publications {
                    if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                        publish_state(
                            events,
                            &publication,
                            SeriesLoadState::Ready,
                            PersistenceState::Pending,
                            None,
                        );
                    }
                }
            }
            Err(error) => {
                eprintln!("Axiusflow engine deferred history publication failed: {error}");
            }
        }
    }

    fn viewport_backfill_failed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        failed_range: Option<HistoryRange>,
        kind: HistoryRequestKind,
        detail: &'static str,
    ) {
        let key = (series.clone(), generation);
        if self.viewport_history_ranges.get(&key).copied() == failed_range {
            self.viewport_history_ranges.remove(&key);
        }
        if kind != HistoryRequestKind::SeamRepair {
            self.resume_live_after_viewport_failure(series);
        }
        if self.viewport_history_ranges.contains_key(&key) {
            if let Err(error) = self.schedule_coinbase_history(
                series,
                generation,
                HistoryRequestKind::ViewportBackfill,
            ) {
                self.viewport_history_ranges.remove(&key);
                self.broadcast_demand_error_for(
                    series,
                    FailureStage::ProviderHistory,
                    &error,
                    None,
                );
            }
            return;
        }
        self.flush_deferred_publication(series);
        self.broadcast_series_resolution_for(
            series,
            SeriesLoadState::Partial,
            PersistenceState::Durable,
            Some(detail),
        );
    }

    fn resume_live_after_viewport_failure(&mut self, series: &BarSeriesKey) {
        let failed = self.live.get_mut(series).is_some_and(|live| {
            let buffered = std::mem::take(&mut live.buffered);
            if buffered
                .iter()
                .any(|trade| live.aggregator.apply_trade(trade).is_err())
            {
                return true;
            }
            live.history_ready = true;
            live.seam_repair = None;
            live.dirty = live.aggregator.in_flight().is_some();
            false
        });
        if failed {
            self.realtime_interrupted(
                FailureStage::Handoff,
                "Coinbase visible-history recovery could not resume the live handoff",
            );
        }
    }

    fn install_completed_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        snapshot: HistorySnapshot,
        replace_covering: bool,
        persisted_bars: Vec<MarketBar>,
        publish: bool,
    ) -> Option<Vec<MarketBar>> {
        let price_scale = snapshot.price_scale;
        let quantity_scale = snapshot.quantity_scale;
        let handoff_boundary_unix_nanos = snapshot.handoff_boundary_unix_nanos;
        let bars = snapshot.bars;
        let installed = if replace_covering {
            self.engine.replace_covering_history(
                generation,
                series,
                price_scale,
                quantity_scale,
                bars.clone(),
                publish,
            )
        } else {
            self.engine.install_history(
                generation,
                series,
                price_scale,
                quantity_scale,
                bars.clone(),
            )
        };
        let publications = match installed {
            Ok(publications) => publications,
            Err(error) => {
                if let Some(waiters) = self.pending.remove(series) {
                    fail_waiters(
                        &mut self.events,
                        waiters,
                        series,
                        engine_install_failure_stage(&error),
                        &error.to_string(),
                    );
                }
                return None;
            }
        };
        for publication in publications {
            if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                publish_state(
                    events,
                    &publication,
                    SeriesLoadState::Ready,
                    PersistenceState::Pending,
                    None,
                );
            }
        }
        self.enqueue_persistence(
            series,
            generation,
            persisted_bars,
            false,
            "Local history persistence is unavailable",
        );
        if let Err(error) = self.finish_rithmic_history_handoff(
            series,
            generation,
            price_scale,
            quantity_scale,
            handoff_boundary_unix_nanos,
            &bars,
        ) {
            if let Some(waiters) = self.pending.remove(series) {
                fail_waiters(
                    &mut self.events,
                    waiters,
                    series,
                    FailureStage::Handoff,
                    &error,
                );
            }
            return None;
        }
        Some(bars)
    }

    fn prepare_history_repair(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        mut snapshot: HistorySnapshot,
        repair: bool,
    ) -> Option<HistorySnapshot> {
        if repair {
            let Some(current) = self.engine.series_snapshot(series) else {
                return Some(snapshot);
            };
            let Ok(bars) = reconcile_history_repair(
                &current,
                snapshot.bars,
                HISTORY_BARS_PER_SERIES,
                Some(coinbase_series_interval(series).ok()?),
                self.viewport_history_ranges
                    .get(&(series.clone(), generation))
                    .copied(),
            ) else {
                return None;
            };
            snapshot.bars = bars;
            self.local_loaded.remove(&(series.clone(), generation));
            return Some(snapshot);
        }
        if !self.local_loaded.contains(&(series.clone(), generation)) {
            return Some(snapshot);
        }
        let Some(current) = self.engine.series_snapshot(series) else {
            self.history_failed(series, generation);
            return None;
        };
        let merged =
            reconcile_history_repair(&current, snapshot.bars, HISTORY_BARS_PER_SERIES, None, None);
        let Ok(bars) = merged else {
            self.history_failed(series, generation);
            return None;
        };
        snapshot.bars = bars;
        self.local_loaded.remove(&(series.clone(), generation));
        self.engine.invalidate_series(series);
        Some(snapshot)
    }

    fn start_rithmic_realtime_from_snapshot(
        &mut self,
        series: &BarSeriesKey,
        snapshot: &axiusflow_market_engine::SeriesSnapshot,
    ) -> Result<(), String> {
        self.ensure_realtime(series)?;
        let handoff_boundary_unix_nanos = snapshot
            .bars
            .last()
            .map(|bar| bar.exchange_timestamp_unix_nanos);
        if self.seed_rithmic_history(
            series,
            snapshot.provider_generation,
            snapshot.price_scale,
            snapshot.quantity_scale,
            handoff_boundary_unix_nanos,
            &snapshot.bars,
        ) {
            Ok(())
        } else {
            Err("Rithmic cached history/live handoff failed".to_string())
        }
    }

    fn finish_rithmic_history_handoff(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        price_scale: u8,
        quantity_scale: u8,
        handoff_boundary_unix_nanos: Option<i64>,
        bars: &[MarketBar],
    ) -> Result<(), String> {
        if series.provider_id != "rithmic" {
            return Ok(());
        }
        self.ensure_realtime(series)?;
        self.seed_rithmic_history(
            series,
            generation,
            price_scale,
            quantity_scale,
            handoff_boundary_unix_nanos,
            bars,
        )
        .then_some(())
        .ok_or_else(|| "Rithmic history/live handoff failed".to_string())
    }

    fn seed_rithmic_history(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        price_scale: u8,
        quantity_scale: u8,
        handoff_boundary_unix_nanos: Option<i64>,
        bars: &[MarketBar],
    ) -> bool {
        let Some(live) = self.rithmic_live.get_mut(series) else {
            return true;
        };
        if live
            .seed(
                price_scale,
                quantity_scale,
                bars,
                handoff_boundary_unix_nanos,
            )
            .is_ok()
        {
            return true;
        }
        live.history_ready = false;
        live.dirty = false;
        self.broadcast_provider_for(
            "rithmic",
            ProviderConnectionState::Recovering,
            generation,
            Some("Rithmic history/live handoff failed"),
        );
        false
    }

    fn persistence_completed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        result: Result<(), LocalHistoryError>,
        elapsed_millis: u64,
    ) {
        if self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        let (state, detail) = if result.is_ok() {
            (PersistenceState::Durable, None)
        } else {
            (
                PersistenceState::Degraded,
                Some("Local history persistence is degraded"),
            )
        };
        self.broadcast_persistence_for(series, state, detail);
        if let Err(error) = result {
            self.broadcast_demand_error_for(
                series,
                local_history_failure_stage(error),
                &error.to_string(),
                Some(elapsed_millis),
            );
        }
    }

    fn broadcast_persistence_for(
        &mut self,
        selected: &BarSeriesKey,
        persistence: PersistenceState,
        detail: Option<&str>,
    ) {
        let current_provider_generation = self
            .engine
            .provider_status(&selected.provider_id)
            .and_then(|status| status.generation)
            .unwrap_or(ProviderGeneration(NonZeroU64::MIN));
        let local_loaded = &self.local_loaded;
        let live = &self.live;
        let engine = &self.engine;
        for (consumer_id, events) in &mut self.events {
            let Some(demand) = self.engine.current_demand(*consumer_id) else {
                continue;
            };
            let (Some(generation), Some(series)) = (demand.generation, demand.series.as_ref())
            else {
                continue;
            };
            if series == selected {
                let state = if local_loaded.contains(&(series.clone(), current_provider_generation))
                {
                    SeriesLoadState::Partial
                } else if live
                    .get(series)
                    .is_some_and(|live| live.connected && live.history_ready)
                {
                    SeriesLoadState::Live
                } else if engine.has_publication(*consumer_id) {
                    SeriesLoadState::Ready
                } else {
                    SeriesLoadState::Resolving
                };
                events.series_state = Some(series_state_with_persistence(
                    *consumer_id,
                    generation,
                    ipc_series(series),
                    state,
                    persistence,
                    detail.map(str::to_string),
                ));
            }
        }
    }

    fn broadcast_demand_error_for(
        &mut self,
        selected: &BarSeriesKey,
        stage: FailureStage,
        detail: &str,
        elapsed_millis: Option<u64>,
    ) {
        for (consumer_id, events) in &mut self.events {
            let Some(demand) = self.engine.current_demand(*consumer_id) else {
                continue;
            };
            let (Some(generation), Some(series)) = (demand.generation, demand.series.as_ref())
            else {
                continue;
            };
            if series == selected {
                events.demand_error = Some(envelope::Payload::DemandError(DemandError {
                    consumer_id: consumer_id.0.get(),
                    generation: generation.0.get(),
                    code: EngineFaultCode::Retryable as i32,
                    stage: failure_stage_name(stage).to_string(),
                    detail: detail.to_string(),
                    series: Some(ipc_series(series)),
                    stage_code: stage as i32,
                    cause: failure_stage_cause(stage).to_string(),
                    elapsed_millis,
                }));
            }
        }
    }

    fn broadcast_series_resolution_for(
        &mut self,
        selected: &BarSeriesKey,
        state: SeriesLoadState,
        persistence: PersistenceState,
        detail: Option<&str>,
    ) {
        for (consumer_id, events) in &mut self.events {
            let Some(demand) = self.engine.current_demand(*consumer_id) else {
                continue;
            };
            let (Some(generation), Some(series)) = (demand.generation, demand.series.as_ref())
            else {
                continue;
            };
            if series == selected {
                events.series_state = Some(series_state_with_persistence(
                    *consumer_id,
                    generation,
                    ipc_series(series),
                    state,
                    persistence,
                    detail.map(str::to_string),
                ));
            }
        }
    }

    fn handle_realtime(&mut self, event: RealtimeEvent) {
        match event {
            RealtimeEvent::Connecting(generation) => self.realtime_connecting(generation),
            RealtimeEvent::Connected(generation) => self.realtime_connected(generation),
            RealtimeEvent::Trade(generation, trade) => self.realtime_trade(generation, &trade),
            RealtimeEvent::Heartbeat(generation) => self.realtime_heartbeat(generation),
            RealtimeEvent::Disconnected(generation) => {
                if generation == self.coinbase_provider_generation() {
                    self.realtime_interrupted(
                        FailureStage::ProviderRealtime,
                        "Coinbase realtime disconnected",
                    );
                }
            }
        }
    }

    fn handle_rithmic_realtime(&mut self, event: RithmicRealtimeEvent) {
        match event {
            RithmicRealtimeEvent::Connecting(generation) => self.rithmic_connecting(generation),
            RithmicRealtimeEvent::Connected(generation)
            | RithmicRealtimeEvent::Heartbeat(generation) => self.rithmic_online(generation),
            RithmicRealtimeEvent::Trade(generation, trade) => {
                self.rithmic_trade(generation, &trade);
            }
            RithmicRealtimeEvent::Depth(generation, snapshot) => {
                self.rithmic_depth(generation, &snapshot);
            }
            RithmicRealtimeEvent::Recovering(generation) => {
                self.rithmic_recovering(generation, "Rithmic live session is recovering");
            }
            RithmicRealtimeEvent::Disconnected(generation) => {
                self.rithmic_realtime_started = false;
                self.rithmic_recovering(generation, "Rithmic live session is recovering");
            }
        }
    }

    fn handle_rithmic_catalog(&mut self, event: RithmicCatalogEvent) {
        match event {
            RithmicCatalogEvent::SearchCompleted(result) => self.handle_catalog_search(result),
            RithmicCatalogEvent::SelectionResolved {
                consumer_id,
                instrument,
            } => self.handle_catalog_selection(consumer_id, instrument),
            RithmicCatalogEvent::Rejected {
                rejection,
                selection,
            } => self.handle_catalog_rejection(rejection, selection),
        }
    }

    fn handle_coinbase_catalog(&mut self, event: CoinbaseCatalogEvent) {
        match event {
            CoinbaseCatalogEvent::SearchCompleted {
                result,
                authorization_generation,
            } => {
                if self
                    .coinbase_catalog_event_is_current(result.consumer_id, authorization_generation)
                {
                    self.handle_catalog_search(result);
                }
            }
            CoinbaseCatalogEvent::SelectionResolved {
                consumer_id,
                instrument,
                authorization_generation,
            } => {
                if self.coinbase_catalog_event_is_current(consumer_id, authorization_generation) {
                    self.handle_catalog_selection(consumer_id, instrument);
                }
            }
            CoinbaseCatalogEvent::Rejected {
                rejection,
                selection,
                authorization_generation,
            } => {
                if self.coinbase_catalog_event_is_current(
                    rejection.consumer_id,
                    authorization_generation,
                ) {
                    self.handle_catalog_rejection(rejection, selection);
                }
            }
        }
    }

    fn coinbase_catalog_event_is_current(
        &self,
        consumer_id: u64,
        authorization_generation: u64,
    ) -> bool {
        let Ok(consumer_id) = id(consumer_id).map(ConsumerId) else {
            return false;
        };
        self.events.contains_key(&consumer_id)
            && self.coinbase_catalog_control.is_some_and(|control| {
                control.is_authorized(consumer_id.0.get(), authorization_generation)
            })
    }

    fn handle_catalog_search(
        &mut self,
        result: axiusflow_engine_protocol::ProviderInstrumentSearchResult,
    ) {
        if result.consumer_id == 0 {
            self.select_warm_rithmic_instrument(&result);
            return;
        }
        let Ok(consumer_id) = id(result.consumer_id).map(ConsumerId) else {
            return;
        };
        if let Some(events) = self.events.get_mut(&consumer_id) {
            events.catalog_search = Some(envelope::Payload::ProviderInstrumentSearchResult(result));
        }
    }

    fn select_warm_rithmic_instrument(
        &self,
        result: &axiusflow_engine_protocol::ProviderInstrumentSearchResult,
    ) {
        if result.search_generation != self.warm_rithmic_search_generation {
            return;
        }
        let Some(warm) = self
            .warm_priority
            .iter()
            .find(|series| series.provider_id == "rithmic")
            .and_then(|series| self.warm_series.get(series))
        else {
            return;
        };
        if !result.instruments.iter().any(|candidate| {
            candidate.symbol == warm.instrument.provider_symbol
                && candidate.exchange == warm.instrument.venue_id
        }) {
            return;
        }
        if let Some(control) = self.rithmic_catalog_control {
            let _ = control.try_send(RithmicCatalogControl::Select(SelectProviderInstrument {
                consumer_id: 0,
                selection_generation: result.search_generation,
                search_generation: result.search_generation,
                provider: "rithmic".to_string(),
                symbol: warm.instrument.provider_symbol.clone(),
                exchange: warm.instrument.venue_id.clone(),
                entitlement_id: warm.instrument.entitlement_id.clone(),
            }));
        }
    }

    fn handle_catalog_selection(
        &mut self,
        consumer_id: u64,
        instrument: InstallProviderInstrument,
    ) {
        if consumer_id == 0 {
            let exact = self
                .warm_priority
                .iter()
                .find(|series| series.provider_id == "rithmic")
                .and_then(|series| self.warm_series.get(series))
                .is_some_and(|warm| {
                    instrument.instrument_id == warm.instrument.instrument_id
                        && instrument.provider_symbol == warm.instrument.provider_symbol
                        && instrument.venue_id == warm.instrument.venue_id
                        && instrument.entitlement_id == warm.instrument.entitlement_id
                });
            if exact {
                let _ = self.install_provider_instrument(&instrument);
            }
            return;
        }
        let Ok(id) = id(consumer_id).map(ConsumerId) else {
            return;
        };
        let command_generation = instrument.selection_generation;
        let provider = instrument.provider.clone();
        let publication = match self.install_provider_instrument(&instrument) {
            Ok(()) => envelope::Payload::ProviderInstrumentSelection(ProviderInstrumentSelection {
                consumer_id,
                instrument: Some(instrument),
            }),
            Err(_) => envelope::Payload::ProviderCatalogRejected(ProviderCatalogRejected {
                consumer_id,
                provider,
                provider_generation: Some(instrument.session_generation),
                command_generation,
                reason: ProviderCatalogRejectionReason::SubscriptionRejected as i32,
            }),
        };
        if let Some(events) = self.events.get_mut(&id) {
            events.catalog_selection = Some(publication);
        }
    }

    fn handle_catalog_rejection(&mut self, rejection: ProviderCatalogRejected, selection: bool) {
        if rejection.consumer_id == 0 {
            if self.resource_mode == ResourceMode::MarketsLive
                && self.warm_rithmic_search_generation < 3
            {
                self.activate_markets_live_hot_set();
            }
            return;
        }
        let Ok(consumer_id) = id(rejection.consumer_id).map(ConsumerId) else {
            return;
        };
        if let Some(events) = self.events.get_mut(&consumer_id) {
            let slot = if selection {
                &mut events.catalog_selection
            } else {
                &mut events.catalog_search
            };
            *slot = Some(envelope::Payload::ProviderCatalogRejected(rejection));
        }
    }

    fn rithmic_connecting(&mut self, generation: u64) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        let current = self
            .engine
            .provider_status("rithmic")
            .and_then(|status| status.generation);
        if current.is_some_and(|current| generation < current) {
            return;
        }
        if current.is_none_or(|current| generation > current)
            && self
                .engine
                .begin_provider_session("rithmic", generation)
                .is_err()
        {
            return;
        }
        if current.is_some_and(|current| generation > current) {
            for ((series, _), stop) in &self.history_cancellations {
                if series.provider_id == "rithmic" {
                    stop.store(true, Ordering::Release);
                }
            }
            let series = self.rithmic_live.keys().cloned().collect::<Vec<_>>();
            for selected in &series {
                if let Some(live) = self.rithmic_live.get_mut(selected) {
                    live.reset(generation);
                }
                self.broadcast_series_recovery_for(
                    selected,
                    "Rithmic live session changed; covering history is reloading",
                );
            }
            for selected in series {
                let _ = self.enqueue_local_history(&selected, generation);
            }
        }
        let _ = self
            .engine
            .set_provider_health("rithmic", generation, ProviderHealth::Connecting);
        self.broadcast_provider_for(
            "rithmic",
            ProviderConnectionState::Connecting,
            generation,
            None,
        );
    }

    fn rithmic_online(&mut self, generation: u64) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status("rithmic")
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        let _ = self
            .engine
            .set_provider_health("rithmic", generation, ProviderHealth::Online);
        self.broadcast_provider_for("rithmic", ProviderConnectionState::Online, generation, None);
        let missing = self
            .rithmic_live
            .iter_mut()
            .filter_map(|(series, live)| {
                if live.generation != generation {
                    return None;
                }
                live.connected = true;
                (!live.history_ready).then(|| series.clone())
            })
            .collect::<Vec<_>>();
        for series in missing {
            if !self
                .history_inflight
                .contains_key(&(series.clone(), generation))
            {
                let _ = self.enqueue_history(&series, generation);
            }
        }
    }

    fn rithmic_trade(&mut self, generation: u64, trade: &MarketTrade) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status("rithmic")
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        if trade.metadata.provider_id != "rithmic"
            || trade.metadata.session_generation != generation.0.get()
        {
            self.rithmic_recovering(
                generation.0.get(),
                "Rithmic live session identity requires recovery",
            );
            for live in self.rithmic_live.values_mut() {
                live.history_ready = false;
                live.dirty = false;
                live.buffered.clear();
            }
            return;
        }
        let order_flow_series = self
            .rithmic_live
            .keys()
            .filter(|series| {
                series.instrument_id == trade.metadata.instrument_id
                    && series.entitlement_id == trade.metadata.entitlement_id
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut order_flow_failed = BTreeSet::new();
        for series in order_flow_series {
            match self
                .engine
                .install_order_flow_trade(generation, &series, trade)
            {
                Ok(publications) => {
                    for publication in publications {
                        if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                            events.order_flow = Some(order_flow_payload(&publication));
                        }
                    }
                }
                Err(_) => {
                    order_flow_failed.insert(series);
                }
            }
        }
        for series in &order_flow_failed {
            self.rithmic_series_recovering(
                series,
                generation,
                FailureStage::CanonicalValidation,
                "Rithmic order-flow reconstruction requires covering history",
            );
        }
        let failed = self
            .rithmic_live
            .iter_mut()
            .filter(|(_, live)| {
                live.generation == generation
                    && live.connected
                    && live.series.instrument_id == trade.metadata.instrument_id
                    && live.series.entitlement_id == trade.metadata.entitlement_id
                    && !order_flow_failed.contains(&live.series)
            })
            .filter_map(|(series, live)| live.accept_trade(trade).is_err().then(|| series.clone()))
            .collect::<Vec<_>>();
        for series in failed {
            self.rithmic_series_recovering(
                &series,
                generation,
                FailureStage::Aggregation,
                "Rithmic instrument aggregation requires covering history",
            );
        }
    }

    fn rithmic_series_recovering(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        stage: FailureStage,
        detail: &str,
    ) {
        if let Some(live) = self.rithmic_live.get_mut(series) {
            live.history_ready = false;
            live.dirty = false;
            live.buffered.clear();
        }
        self.broadcast_demand_error_for(series, stage, detail, None);
        self.broadcast_series_recovery_for(series, detail);
        if !self
            .history_inflight
            .contains_key(&(series.clone(), generation))
        {
            let _ = self.enqueue_history(series, generation);
        }
    }

    fn rithmic_depth(&mut self, generation: u64, snapshot: &DepthSnapshot) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status("rithmic")
            .and_then(|status| status.generation)
            != Some(generation)
            || snapshot.metadata.provider_id != "rithmic"
            || snapshot.metadata.session_generation != generation.0.get()
        {
            return;
        }
        let instrument_id = snapshot.metadata.instrument_id.clone();
        let should_publish = self
            .rithmic_order_books
            .get_mut(&instrument_id)
            .filter(|order_book| {
                order_book.instrument.entitlement_id == snapshot.metadata.entitlement_id
            })
            .is_some_and(|order_book| {
                matches!(
                    order_book.book.install_snapshot(snapshot),
                    Ok(OrderBookApplyOutcome::Published(_)
                        | OrderBookApplyOutcome::RecoveryRequired(_))
                ) || matches!(
                    order_book.book.state(),
                    CanonicalOrderBookState::Recovering(_)
                )
            });
        if should_publish {
            self.broadcast_rithmic_order_book(&instrument_id);
        }
    }

    fn rithmic_recovering(&mut self, generation: u64, detail: &'static str) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        if self
            .engine
            .provider_status("rithmic")
            .and_then(|status| status.generation)
            != Some(generation)
        {
            return;
        }
        let _ = self
            .engine
            .set_provider_health("rithmic", generation, ProviderHealth::Recovering);
        self.broadcast_provider_for(
            "rithmic",
            ProviderConnectionState::Recovering,
            generation,
            Some(detail),
        );
        for live in self.rithmic_live.values_mut() {
            live.connected = false;
        }
        let stale_books = self
            .rithmic_order_books
            .iter_mut()
            .filter_map(|(instrument_id, order_book)| {
                order_book.book.mark_stale();
                matches!(order_book.book.state(), CanonicalOrderBookState::Stale)
                    .then(|| instrument_id.clone())
            })
            .collect::<Vec<_>>();
        for instrument_id in stale_books {
            self.broadcast_rithmic_order_book(&instrument_id);
        }
    }

    fn publish_order_book_to_consumer(&mut self, consumer_id: ConsumerId) {
        let Some(demand) = self.engine.current_demand(consumer_id) else {
            return;
        };
        let Some(series) = demand.series.as_ref() else {
            return;
        };
        let Some(generation) = demand.generation else {
            return;
        };
        if series.provider_id != "rithmic" {
            return;
        }
        let Some(order_book) = self.rithmic_order_books.get(&series.instrument_id) else {
            return;
        };
        if order_book.instrument.entitlement_id != series.entitlement_id {
            return;
        }
        if let Some(events) = self.events.get_mut(&consumer_id) {
            events.order_book = Some(order_book_snapshot(consumer_id, generation, order_book));
        }
    }

    fn broadcast_rithmic_order_book(&mut self, instrument_id: &str) {
        let consumers = self
            .events
            .keys()
            .filter_map(|consumer_id| {
                let demand = self.engine.current_demand(*consumer_id)?;
                let generation = demand.generation?;
                let series = demand.series.as_ref()?;
                (series.provider_id == "rithmic" && series.instrument_id == instrument_id)
                    .then_some((*consumer_id, generation))
            })
            .collect::<Vec<_>>();
        let Some(order_book) = self.rithmic_order_books.get(instrument_id) else {
            return;
        };
        for (consumer_id, generation) in consumers {
            if let Some(events) = self.events.get_mut(&consumer_id) {
                events.order_book = Some(order_book_snapshot(consumer_id, generation, order_book));
            }
        }
    }

    fn realtime_connecting(&mut self, generation: ProviderGeneration) {
        let current = self.coinbase_provider_generation();
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
            for ((series, request_generation), stop) in &self.history_cancellations {
                if series.provider_id == "coinbase" && *request_generation < generation {
                    stop.store(true, Ordering::Release);
                }
            }
            for live in self.live.values_mut() {
                live.reset(generation);
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
        self.realtime_connected = false;
        self.broadcast_provider(state, generation, None);
    }

    fn realtime_connected(&mut self, generation: ProviderGeneration) {
        if generation != self.coinbase_provider_generation() {
            return;
        }
        self.realtime_connected = true;
        let mut missing = Vec::new();
        for (series, live) in &mut self.live {
            live.connected = true;
            if !live.history_ready {
                missing.push(series.clone());
            }
        }
        for series in missing {
            let _ = self.enqueue_history(&series, generation);
        }
        self.provider_online_if_all_series_ready();
    }

    fn realtime_trade(&mut self, generation: ProviderGeneration, trade: &CanonicalTrade) {
        let mut interrupted = None;
        let mut seam_repairs = Vec::new();
        for (series, live) in self.live.iter_mut().filter(|(_, live)| {
            live.generation == generation
                && live.connected
                && live.aggregator.product_id() == trade.product_id
        }) {
            if live.history_ready {
                let Ok(interval) = coinbase_interval_nanos(series) else {
                    interrupted = Some("Coinbase realtime interval is invalid");
                    break;
                };
                let anchor = live
                    .aggregator
                    .in_flight()
                    .map(|bar| bar.exchange_timestamp_unix_nanos)
                    .or_else(|| {
                        live.aggregator
                            .history()
                            .last()
                            .map(|bar| bar.exchange_timestamp_unix_nanos)
                    });
                if let Some(range) = anchor.and_then(|anchor| {
                    coinbase_seam_repair(anchor, trade.trade_time_unix_nanos, interval)
                }) {
                    if live.buffered.len() == LIVE_BUFFER_CAPACITY {
                        interrupted = Some("Coinbase history/live buffer overflowed");
                        break;
                    }
                    live.history_ready = false;
                    live.dirty = false;
                    live.seam_repair = Some(range);
                    live.buffered.push_back(trade.clone());
                    seam_repairs.push((series.clone(), live.generation, range));
                } else if live.aggregator.apply_trade(trade).is_err() {
                    interrupted = Some("Coinbase realtime aggregation failed");
                    break;
                }
                if live.history_ready {
                    live.dirty = true;
                }
            } else if live.buffered.len() == LIVE_BUFFER_CAPACITY {
                interrupted = Some("Coinbase history/live buffer overflowed");
                break;
            } else {
                live.buffered.push_back(trade.clone());
            }
        }
        for (series, generation, range) in seam_repairs {
            self.viewport_history_ranges
                .insert((series.clone(), generation), range);
            if let Err(error) =
                self.schedule_coinbase_history(&series, generation, HistoryRequestKind::SeamRepair)
            {
                self.viewport_history_ranges
                    .remove(&(series.clone(), generation));
                self.broadcast_demand_error_for(
                    &series,
                    FailureStage::ProviderHistory,
                    &error,
                    None,
                );
            }
        }
        if let Some(detail) = interrupted {
            self.realtime_interrupted(FailureStage::Aggregation, detail);
        }
    }

    fn realtime_heartbeat(&mut self, generation: ProviderGeneration) {
        let missing = self
            .live
            .iter()
            .filter(|(_, live)| {
                live.generation == generation && live.connected && !live.history_ready
            })
            .map(|(series, _)| series.clone())
            .collect::<Vec<_>>();
        for series in missing {
            let _ = self.enqueue_history(&series, generation);
        }
    }

    fn realtime_interrupted(&mut self, stage: FailureStage, detail: &str) {
        let generation = self.coinbase_provider_generation();
        let _ = self
            .engine
            .set_provider_health("coinbase", generation, ProviderHealth::Recovering);
        self.realtime_connected = false;
        let affected = self.live.keys().cloned().collect::<Vec<_>>();
        for series in &affected {
            self.broadcast_demand_error_for(series, stage, detail, None);
        }
        for live in self.live.values_mut() {
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

    fn provider_online_if_all_series_ready(&mut self) {
        if !self.realtime_connected
            || self.live.is_empty()
            || self.live.values().any(|live| !live.history_ready)
        {
            return;
        }
        let generation = self.coinbase_provider_generation();
        let _ = self
            .engine
            .set_provider_health("coinbase", generation, ProviderHealth::Online);
        self.broadcast_provider(ProviderConnectionState::Online, generation, None);
        self.broadcast_series_state(SeriesLoadState::Live);
    }

    fn series_live_if_ready(&mut self, series: &BarSeriesKey) {
        if !self
            .live
            .get(series)
            .is_some_and(|live| live.connected && live.history_ready)
        {
            return;
        }
        if self
            .engine
            .provider_status("coinbase")
            .is_some_and(|status| status.health == ProviderHealth::Online)
        {
            self.broadcast_series_state_for(series, SeriesLoadState::Live);
        } else {
            self.provider_online_if_all_series_ready();
        }
    }

    fn publish_live(&mut self) {
        let ready = self
            .live
            .iter_mut()
            .filter_map(|(series, live)| {
                if live.viewport_position != ViewportPosition::Live {
                    return None;
                }
                Some((
                    series.clone(),
                    live.generation,
                    live.aggregator.price_scale(),
                    live.aggregator.quantity_scale(),
                    live.take_publication()?,
                ))
            })
            .collect::<Vec<_>>();
        for (series, generation, price_scale, quantity_scale, update) in ready {
            let published = match update {
                LiveSeriesPublication::Tail(bar) => self
                    .engine
                    .install_realtime_tail(
                        generation,
                        &series,
                        price_scale,
                        quantity_scale,
                        bar,
                        true,
                    )
                    .map(|publications| {
                        for publication in publications {
                            if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                                events.publish_series_update(series_update_message(&publication));
                            }
                        }
                    }),
                LiveSeriesPublication::Covering(bars) => self
                    .engine
                    .install_realtime(generation, &series, price_scale, quantity_scale, bars, true)
                    .map(|publications| {
                        for publication in publications {
                            if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                                events.snapshot = Some(snapshot_message(&publication));
                            }
                        }
                    }),
            };
            if let Err(error) = published {
                eprintln!("Axiusflow engine Coinbase live publication failed: {error}");
                self.realtime_interrupted(
                    FailureStage::Publication,
                    "Coinbase live publication failed",
                );
                return;
            }
        }
    }

    fn publish_rithmic_live(&mut self) {
        let ready = self
            .rithmic_live
            .values_mut()
            .filter_map(|live| {
                Some((
                    live.series.clone(),
                    live.generation,
                    live.price_scale,
                    live.quantity_scale,
                    live.take_publication()?,
                ))
            })
            .collect::<Vec<_>>();
        for (series, generation, price_scale, quantity_scale, update) in ready {
            let published = match update {
                LiveSeriesPublication::Tail(bar) => self
                    .engine
                    .install_realtime_tail(
                        generation,
                        &series,
                        price_scale,
                        quantity_scale,
                        bar,
                        true,
                    )
                    .map(|publications| {
                        for publication in publications {
                            if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                                events.publish_series_update(series_update_message(&publication));
                                events.series_state = Some(series_state_with_persistence(
                                    publication.consumer_id,
                                    publication.generation,
                                    ipc_series(&publication.series),
                                    SeriesLoadState::Live,
                                    PersistenceState::Durable,
                                    None,
                                ));
                            }
                        }
                    }),
                LiveSeriesPublication::Covering(bars) => self
                    .engine
                    .install_realtime(generation, &series, price_scale, quantity_scale, bars, true)
                    .map(|publications| {
                        for publication in publications {
                            if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                                publish_state(
                                    events,
                                    &publication,
                                    SeriesLoadState::Live,
                                    PersistenceState::Durable,
                                    None,
                                );
                            }
                        }
                    }),
            };
            if let Err(error) = published {
                if let Some(live) = self.rithmic_live.get_mut(&series) {
                    live.history_ready = false;
                }
                eprintln!("Axiusflow engine Rithmic live publication failed: {error}");
                self.broadcast_provider_for(
                    "rithmic",
                    ProviderConnectionState::Recovering,
                    generation,
                    Some("Rithmic live publication requires covering history"),
                );
                self.broadcast_demand_error_for(
                    &series,
                    FailureStage::Publication,
                    "Rithmic live publication requires covering history",
                    None,
                );
            }
        }
    }

    fn broadcast_provider_for(
        &mut self,
        provider: &str,
        state: ProviderConnectionState,
        generation: ProviderGeneration,
        detail: Option<&str>,
    ) {
        let payload = envelope::Payload::ProviderState(ProviderState {
            provider: provider.to_string(),
            state: state as i32,
            generation: generation.0.get(),
            detail: detail.map(str::to_string),
        });
        for (consumer_id, events) in &mut self.events {
            if self
                .engine
                .current_demand(*consumer_id)
                .and_then(|demand| demand.series.as_ref())
                .is_some_and(|series| series.provider_id == provider)
            {
                events.provider = Some(payload.clone());
            }
        }
    }

    fn broadcast_provider(
        &mut self,
        state: ProviderConnectionState,
        generation: ProviderGeneration,
        detail: Option<&str>,
    ) {
        self.broadcast_provider_for("coinbase", state, generation, detail);
    }

    #[cfg(test)]
    fn broadcast_rithmic_provider(
        &mut self,
        state: ProviderConnectionState,
        generation: ProviderGeneration,
        detail: Option<&str>,
    ) {
        self.broadcast_provider_for("rithmic", state, generation, detail);
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
            if series.provider_id != "coinbase"
                || !chart_stream_requirements(series).contains(MarketStream::Trades)
            {
                continue;
            }
            events.series_state = Some(series_state(
                *consumer_id,
                generation,
                ipc_series(series),
                state,
                None,
            ));
        }
    }

    fn broadcast_series_state_for(&mut self, selected: &BarSeriesKey, state: SeriesLoadState) {
        for (consumer_id, events) in &mut self.events {
            let Some(demand) = self.engine.current_demand(*consumer_id) else {
                continue;
            };
            let (Some(generation), Some(series)) = (demand.generation, demand.series.as_ref())
            else {
                continue;
            };
            if series == selected {
                events.series_state = Some(series_state(
                    *consumer_id,
                    generation,
                    ipc_series(series),
                    state,
                    None,
                ));
            }
        }
    }

    fn broadcast_series_recovery_for(&mut self, selected: &BarSeriesKey, detail: &str) {
        let state = if self.engine.series_snapshot(selected).is_some() {
            SeriesLoadState::Partial
        } else {
            SeriesLoadState::Resolving
        };
        for (consumer_id, events) in &mut self.events {
            let Some(demand) = self.engine.current_demand(*consumer_id) else {
                continue;
            };
            let (Some(generation), Some(series)) = (demand.generation, demand.series.as_ref())
            else {
                continue;
            };
            if series != selected {
                continue;
            }
            let persistence = match events.series_state.as_ref() {
                Some(envelope::Payload::SeriesState(current)) => {
                    PersistenceState::try_from(current.persistence)
                        .unwrap_or(PersistenceState::NotRequested)
                }
                _ => PersistenceState::NotRequested,
            };
            events.series_state = Some(series_state_with_persistence(
                *consumer_id,
                generation,
                ipc_series(series),
                state,
                persistence,
                Some(detail.to_string()),
            ));
        }
    }

    fn coinbase_provider_generation(&self) -> ProviderGeneration {
        self.engine
            .provider_status("coinbase")
            .and_then(|status| status.generation)
            .unwrap_or(ProviderGeneration(NonZeroU64::MIN))
    }

    fn remove_waiter(&mut self, consumer_id: ConsumerId) {
        for waiters in self.pending.values_mut() {
            waiters.retain(|waiter| waiter.consumer_id != consumer_id);
        }
        let unobserved = self
            .pending
            .iter()
            .filter(|(_, waiters)| waiters.is_empty())
            .map(|(series, _)| series.clone())
            .collect::<Vec<_>>();
        self.pending.retain(|_, waiters| !waiters.is_empty());
        if self.resource_mode == ResourceMode::MarketsLive {
            return;
        }
        self.local_history_deadlines
            .retain(|(series, _), _| !unobserved.contains(series));
        for series in unobserved {
            for ((active, _), stop) in &self.history_cancellations {
                if active == &series {
                    stop.store(true, Ordering::Release);
                }
            }
        }
    }

    fn prune_unused_live_series(&mut self) {
        for series in self.rithmic_live.keys().filter(|series| {
            !(self.engine.has_subscription(series)
                || self.resource_mode == ResourceMode::MarketsLive
                    && self.retained_live.contains(series))
        }) {
            for ((active, _), stop) in &self.history_cancellations {
                if active == series {
                    stop.store(true, Ordering::Release);
                }
            }
        }
        self.live.retain(|series, _| {
            self.engine.has_subscription(series)
                || self.resource_mode == ResourceMode::MarketsLive
                    && self.retained_live.contains(series)
        });
        self.rithmic_live.retain(|series, _| {
            self.engine.has_subscription(series)
                || self.resource_mode == ResourceMode::MarketsLive
                    && self.retained_live.contains(series)
        });
    }

    fn prune_history_tracking(&mut self) {
        self.active_viewports.retain(|consumer_id, viewport| {
            self.engine
                .current_demand(*consumer_id)
                .is_some_and(|demand| {
                    demand.generation == Some(viewport.consumer_generation)
                        && demand.series.as_ref() == Some(&viewport.series)
                })
                && self
                    .engine
                    .provider_status(&viewport.series.provider_id)
                    .and_then(|status| status.generation)
                    == Some(viewport.provider_generation)
        });
        let unused_coverage = self
            .history_coverage
            .keys()
            .filter(|series| {
                !self.engine.has_subscription(series)
                    && self.engine.series_snapshot(series).is_none()
            })
            .cloned()
            .collect::<Vec<_>>();
        for series in unused_coverage {
            self.history_coverage.remove(&series);
        }
        let obsolete_viewports = self
            .viewport_history_ranges
            .keys()
            .filter(|(series, generation)| {
                !self.engine.has_subscription(series)
                    || self
                        .engine
                        .provider_status(&series.provider_id)
                        .and_then(|status| status.generation)
                        != Some(*generation)
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in obsolete_viewports {
            self.viewport_history_ranges.remove(&key);
        }
        self.deferred_publications
            .retain(|series| self.engine.has_subscription(series));
    }

    fn release_unused_live_market_data(&mut self) {
        self.prune_history_tracking();
        if self.resource_mode == ResourceMode::MarketsLive {
            return;
        }
        self.prune_unused_live_series();
        if !self.live.is_empty() {
            let _ = self.sync_coinbase_realtime();
        }
        self.stop_realtime_if_idle();
    }

    fn sync_coinbase_realtime(&mut self) -> Result<(), String> {
        let mut products = BTreeSet::new();
        for series in self.live.keys() {
            let instrument = self.coinbase_instrument(series)?;
            products.insert(instrument.provider_symbol.clone());
        }
        if products.is_empty()
            || products.len() > axiusflow_coinbase_market_adapter::MAXIMUM_PRODUCTS
        {
            return Err("Coinbase realtime subscription set is invalid".to_string());
        }
        if products != self.realtime_products {
            self.realtime_stop.store(true, Ordering::Release);
            self.realtime_started = false;
            self.realtime_connected = false;
        }
        if !self.realtime_started {
            match self
                .realtime_control
                .try_send(RealtimeControl::Start(products.iter().cloned().collect()))
            {
                Ok(()) => {
                    self.realtime_products = products;
                    self.realtime_started = true;
                }
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => {
                    self.realtime_stop.store(true, Ordering::Release);
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn stop_realtime_if_idle(&mut self) {
        if self.live.is_empty() && self.realtime_started {
            self.realtime_stop.store(true, Ordering::Release);
            self.realtime_started = false;
            self.realtime_connected = false;
            self.realtime_products.clear();
            self.live.clear();
            let generation = self.coinbase_provider_generation();
            let _ = self.engine.end_provider_session("coinbase", generation);
        }
        if self.rithmic_live.is_empty() && self.rithmic_realtime_started {
            if let Some(control) = self.rithmic_realtime_control {
                let _ = control.try_send(RithmicRealtimeControl::Stop);
            }
            self.rithmic_realtime_started = false;
            if let Some(generation) = self
                .engine
                .provider_status("rithmic")
                .and_then(|status| status.generation)
            {
                let _ = self.engine.end_provider_session("rithmic", generation);
            }
        }
    }
}

fn try_enqueue_history(
    history: &SyncSender<HistoryRequest>,
    request: HistoryRequest,
) -> Result<(), &'static str> {
    match history.try_send(request) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => Err("provider history capacity is temporarily exhausted"),
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
                    streams: StreamRequirements::BARS.with(MarketStream::Trades),
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

fn fail_waiters(
    events: &mut BTreeMap<ConsumerId, ConsumerEvents>,
    waiters: Vec<DemandWaiter>,
    series: &BarSeriesKey,
    stage: FailureStage,
    detail: &str,
) {
    for waiter in waiters {
        if let Some(events) = events.get_mut(&waiter.consumer_id) {
            events.series_state = Some(series_state(
                waiter.consumer_id,
                waiter.generation,
                ipc_series(series),
                SeriesLoadState::Failed,
                Some(detail.to_string()),
            ));
            events.demand_error = Some(envelope::Payload::DemandError(DemandError {
                consumer_id: waiter.consumer_id.0.get(),
                generation: waiter.generation.0.get(),
                code: EngineFaultCode::Retryable as i32,
                stage: failure_stage_name(stage).to_string(),
                detail: detail.to_string(),
                series: Some(ipc_series(series)),
                stage_code: stage as i32,
                cause: failure_stage_cause(stage).to_string(),
                elapsed_millis: Some(
                    u64::try_from(waiter.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                ),
            }));
        }
    }
}

const fn failure_stage_name(stage: FailureStage) -> &'static str {
    match stage {
        FailureStage::Unspecified => "unspecified",
        FailureStage::ProviderHistory => "provider_history",
        FailureStage::CanonicalValidation => "canonical_validation",
        FailureStage::MemoryInstall => "memory_install",
        FailureStage::Aggregation => "aggregation",
        FailureStage::SegmentEncode => "segment_encode",
        FailureStage::Encryption => "encryption",
        FailureStage::FilesystemWrite => "filesystem_write",
        FailureStage::CatalogCommit => "catalog_commit",
        FailureStage::Handoff => "handoff",
        FailureStage::Publication => "publication",
        FailureStage::IpcSend => "ipc_send",
        FailureStage::ChartInstall => "chart_install",
        FailureStage::ProviderRealtime => "provider_realtime",
    }
}

const fn failure_stage_cause(stage: FailureStage) -> &'static str {
    match stage {
        FailureStage::ProviderHistory => "provider history did not produce usable canonical bars",
        FailureStage::CanonicalValidation => "market data failed canonical validation",
        FailureStage::MemoryInstall => "validated market data could not enter bounded memory",
        FailureStage::Aggregation => "canonical events could not be aggregated",
        FailureStage::SegmentEncode => "canonical history could not be encoded for persistence",
        FailureStage::Encryption => "history encryption could not complete",
        FailureStage::FilesystemWrite => "encrypted history could not be written durably",
        FailureStage::CatalogCommit => "history catalog publication could not commit",
        FailureStage::Handoff => "history and realtime state could not be joined safely",
        FailureStage::Publication => "validated market state could not be published",
        FailureStage::IpcSend => "local protocol delivery could not complete",
        FailureStage::ChartInstall => "desktop presentation rejected the engine publication",
        FailureStage::ProviderRealtime => "provider realtime delivery was interrupted",
        FailureStage::Unspecified => "the owning processing stage is unavailable",
    }
}

const fn engine_install_failure_stage(error: &EngineError) -> FailureStage {
    match error {
        EngineError::EmptySeries
        | EngineError::DiscontinuousSeries { .. }
        | EngineError::NonIncreasingSeriesTime
        | EngineError::ConflictingSeriesGeneration(_)
        | EngineError::InvalidMarketData(_) => FailureStage::CanonicalValidation,
        _ => FailureStage::MemoryInstall,
    }
}

const fn local_history_failure_stage(error: LocalHistoryError) -> FailureStage {
    match error {
        LocalHistoryError::SegmentEncode
        | LocalHistoryError::InvalidSeries
        | LocalHistoryError::EmptySeries
        | LocalHistoryError::InvalidRange
        | LocalHistoryError::InvalidSegment => FailureStage::SegmentEncode,
        LocalHistoryError::Encryption | LocalHistoryError::InvalidVaultKey => {
            FailureStage::Encryption
        }
        LocalHistoryError::FilesystemWrite
        | LocalHistoryError::InvalidRoot
        | LocalHistoryError::Unavailable => FailureStage::FilesystemWrite,
        LocalHistoryError::CatalogCommit => FailureStage::CatalogCommit,
    }
}

fn publish_ready(
    events: &mut ConsumerEvents,
    publication: &axiusflow_market_engine::ConsumerPublication,
) {
    publish_state(
        events,
        publication,
        SeriesLoadState::Ready,
        PersistenceState::NotRequested,
        None,
    );
}

fn publish_state(
    events: &mut ConsumerEvents,
    publication: &axiusflow_market_engine::ConsumerPublication,
    state: SeriesLoadState,
    persistence: PersistenceState,
    detail: Option<&str>,
) {
    let series = ipc_series(&publication.snapshot.series);
    events.snapshot = Some(snapshot_message(publication));
    events.series_state = Some(series_state_with_persistence(
        publication.consumer_id,
        publication.generation,
        series,
        state,
        persistence,
        detail.map(str::to_string),
    ));
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

fn series_update_message(
    publication: &axiusflow_market_engine::ConsumerSeriesUpdate,
) -> envelope::Payload {
    envelope::Payload::SeriesUpdate(axiusflow_engine_protocol::SeriesUpdate {
        consumer_id: publication.consumer_id.0.get(),
        generation: publication.generation.0.get(),
        series: Some(ipc_series(&publication.series)),
        provider_generation: publication.provider_generation.0.get(),
        bar: Some(ipc_bar(publication.bar)),
        forming: publication.forming,
        publication_generation: publication.publication_generation,
    })
}

fn order_flow_payload(
    publication: &axiusflow_market_engine::ConsumerOrderFlowPublication,
) -> envelope::Payload {
    match &publication.kind {
        OrderFlowPublicationKind::Snapshot(snapshot) => {
            envelope::Payload::OrderFlowSnapshot(IpcOrderFlowSnapshot {
                consumer_id: publication.consumer_id.0.get(),
                generation: publication.generation.0.get(),
                series: Some(ipc_series(&publication.series)),
                provider_generation: snapshot.provider_generation.0.get(),
                publication_generation: publication.publication_generation,
                source_watermark: snapshot.source_watermark,
                cumulative_delta: snapshot.cumulative_delta,
                levels: snapshot
                    .levels
                    .iter()
                    .copied()
                    .map(ipc_order_flow_level)
                    .collect(),
                tape: snapshot
                    .tape
                    .iter()
                    .copied()
                    .map(ipc_order_flow_trade)
                    .collect(),
            })
        }
        OrderFlowPublicationKind::Update(update) => {
            envelope::Payload::OrderFlowUpdate(IpcOrderFlowUpdate {
                consumer_id: publication.consumer_id.0.get(),
                generation: publication.generation.0.get(),
                series: Some(ipc_series(&publication.series)),
                provider_generation: update.provider_generation.0.get(),
                publication_generation: publication.publication_generation,
                cumulative_delta: update.cumulative_delta,
                level: Some(ipc_order_flow_level(update.level)),
                trade: Some(ipc_order_flow_trade(update.trade)),
            })
        }
    }
}

fn ipc_order_flow_level(level: axiusflow_market_engine::OrderFlowLevel) -> IpcOrderFlowLevel {
    IpcOrderFlowLevel {
        price: level.price,
        bid_volume: level.bid_volume,
        ask_volume: level.ask_volume,
        trade_count: level.trade_count,
        time_at_price_count: level.time_at_price_count,
    }
}

fn ipc_order_flow_trade(trade: axiusflow_market_engine::OrderFlowTrade) -> IpcOrderFlowTrade {
    IpcOrderFlowTrade {
        source_sequence: trade.source_sequence,
        exchange_timestamp_unix_nanos: trade.exchange_timestamp_unix_nanos,
        price: trade.price,
        quantity: trade.quantity,
        aggressor: match trade.aggressor {
            axiusflow_market_data::AggressorSide::Unknown => OrderFlowAggressor::Unknown,
            axiusflow_market_data::AggressorSide::Buy => OrderFlowAggressor::Buy,
            axiusflow_market_data::AggressorSide::Sell => OrderFlowAggressor::Sell,
        } as i32,
    }
}

fn order_book_snapshot(
    consumer_id: ConsumerId,
    generation: GenerationId,
    order_book: &RithmicOrderBook,
) -> envelope::Payload {
    let publication = order_book.book.publication();
    let provider_generation = if publication.session_generation == 0 {
        order_book.instrument.session_generation
    } else {
        publication.session_generation
    };
    envelope::Payload::OrderBookSnapshot(IpcOrderBookSnapshot {
        consumer_id: consumer_id.0.get(),
        generation: generation.0.get(),
        provider: order_book.instrument.provider.clone(),
        instrument_id: order_book.instrument.instrument_id.clone(),
        entitlement_id: order_book.instrument.entitlement_id.clone(),
        provider_generation,
        selection_generation: order_book.instrument.selection_generation,
        revision: publication.revision,
        source_watermark: publication.source_watermark,
        state: ipc_order_book_state(publication.state) as i32,
        bids: ipc_order_book_levels(&publication.bids),
        asks: ipc_order_book_levels(&publication.asks),
    })
}

fn ipc_order_book_levels(levels: &[DepthLevel]) -> Vec<IpcOrderBookLevel> {
    levels
        .iter()
        .map(|level| IpcOrderBookLevel {
            price: level.price,
            quantity: level.quantity,
            order_count: level.order_count,
        })
        .collect()
}

const fn ipc_order_book_state(state: CanonicalOrderBookState) -> IpcOrderBookState {
    match state {
        CanonicalOrderBookState::Ready => IpcOrderBookState::Ready,
        CanonicalOrderBookState::Stale => IpcOrderBookState::Stale,
        CanonicalOrderBookState::Recovering(OrderBookRecoveryReason::AwaitingSnapshot) => {
            IpcOrderBookState::AwaitingSnapshot
        }
        CanonicalOrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap) => {
            IpcOrderBookState::SequenceGap
        }
        CanonicalOrderBookState::Recovering(OrderBookRecoveryReason::CrossedBook) => {
            IpcOrderBookState::CrossedBook
        }
        CanonicalOrderBookState::Recovering(OrderBookRecoveryReason::InvalidUpdate) => {
            IpcOrderBookState::InvalidUpdate
        }
    }
}

fn series_state(
    consumer_id: ConsumerId,
    generation: GenerationId,
    series: SeriesKey,
    state: SeriesLoadState,
    detail: Option<String>,
) -> envelope::Payload {
    series_state_with_persistence(
        consumer_id,
        generation,
        series,
        state,
        PersistenceState::NotRequested,
        detail,
    )
}

fn series_state_with_persistence(
    consumer_id: ConsumerId,
    generation: GenerationId,
    series: SeriesKey,
    state: SeriesLoadState,
    persistence: PersistenceState,
    detail: Option<String>,
) -> envelope::Payload {
    envelope::Payload::SeriesState(SeriesState {
        consumer_id: consumer_id.0.get(),
        generation: generation.0.get(),
        series: Some(series),
        state: state as i32,
        persistence: persistence as i32,
        detail,
    })
}

fn id(value: u64) -> Result<NonZeroU64, String> {
    NonZeroU64::new(value).ok_or_else(|| "market identity must be non-zero".to_string())
}

fn validate_provider_search(search: &SearchProviderInstruments) -> Result<(), String> {
    id(search.consumer_id)?;
    id(search.search_generation)?;
    if !matches!(search.provider.as_str(), "coinbase" | "rithmic")
        || search.maximum_results == 0
        || usize::try_from(search.maximum_results).unwrap_or(usize::MAX)
            > MAXIMUM_CATALOG_INSTRUMENTS
        || (!search.query.trim().is_empty() && !valid_catalog_field(&search.query))
    {
        return Err("provider instrument search is invalid".to_string());
    }
    Ok(())
}

fn validate_provider_selection(selection: &SelectProviderInstrument) -> Result<(), String> {
    id(selection.consumer_id)?;
    id(selection.selection_generation)?;
    id(selection.search_generation)?;
    if !matches!(selection.provider.as_str(), "coinbase" | "rithmic")
        || ![
            &selection.symbol,
            &selection.exchange,
            &selection.entitlement_id,
        ]
        .into_iter()
        .all(|value| valid_catalog_field(value))
    {
        return Err("provider instrument selection is invalid".to_string());
    }
    Ok(())
}

fn valid_catalog_field(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAXIMUM_CATALOG_FIELD_BYTES
        && !value.chars().any(char::is_control)
}

fn coinbase_catalog_dispatch_error(error: CoinbaseCatalogDispatchError) -> String {
    match error {
        CoinbaseCatalogDispatchError::Unauthorized => {
            "coinbase catalog consumer authorization is unavailable".to_string()
        }
        CoinbaseCatalogDispatchError::Full => {
            "coinbase catalog command capacity is exhausted".to_string()
        }
        CoinbaseCatalogDispatchError::Disconnected => {
            "coinbase catalog worker is unavailable".to_string()
        }
    }
}

fn chart_stream_requirements(series: &BarSeriesKey) -> StreamRequirements {
    let live_bars = series.provider_id == "rithmic"
        || !matches!(
            series.period,
            BarPeriod::Week { .. } | BarPeriod::Month { .. }
        );
    let mut streams = StreamRequirements::BARS;
    if live_bars {
        streams = streams.with(MarketStream::Trades);
    }
    if series.provider_id == "rithmic" {
        streams = streams.with(MarketStream::Depth);
    }
    streams
}

fn validate_provider_instrument(instrument: &InstallProviderInstrument) -> Result<(), String> {
    if instrument.session_generation == 0 || instrument.selection_generation == 0 {
        return Err("provider instrument generation must be non-zero".to_string());
    }
    for value in [
        &instrument.provider,
        &instrument.instrument_id,
        &instrument.provider_symbol,
        &instrument.display_symbol,
        &instrument.venue_id,
        &instrument.entitlement_id,
    ] {
        if !valid_catalog_field(value) {
            return Err("provider instrument identity is invalid".to_string());
        }
    }
    if instrument.price_scale > 18 || instrument.quantity_scale > 18 {
        return Err("provider instrument precision is invalid".to_string());
    }
    if instrument.provider == "coinbase" {
        let expected_id = coinbase_instrument_id(&instrument.provider_symbol)
            .map_err(|_| "Coinbase instrument symbol is invalid".to_string())?;
        let expected_display = instrument.provider_symbol.replace('-', "/");
        if instrument.venue_id != "coinbase"
            || instrument.entitlement_id != ENTITLEMENT_CLASS
            || instrument.instrument_id != expected_id
            || instrument.display_symbol != expected_display
        {
            return Err("Coinbase instrument identity is inconsistent".to_string());
        }
    }
    Ok(())
}

fn reconcile_interval_history(
    current: &SeriesSnapshot,
    repair: Vec<MarketBar>,
    maximum_bars: usize,
    interval: CoinbaseInterval,
    anchor: Option<HistoryRange>,
) -> Result<Vec<MarketBar>, String> {
    let mut merged = BTreeMap::new();
    for bar in current.bars.iter().copied() {
        bar.validate().map_err(|error| error.to_string())?;
        if !coinbase_bar_is_aligned(interval, &bar)
            || merged
                .insert(bar.exchange_timestamp_unix_nanos, bar)
                .is_some()
        {
            return Err("Coinbase current history is not canonical".to_string());
        }
    }
    for mut bar in repair {
        bar.validate().map_err(|error| error.to_string())?;
        if !coinbase_bar_is_aligned(interval, &bar) {
            return Err("Coinbase repair is not interval aligned".to_string());
        }
        bar.source_sequence = 1;
        merged
            .entry(bar.exchange_timestamp_unix_nanos)
            .or_insert(bar);
    }
    let bars = retain_anchored_history(merged.into_values().collect(), maximum_bars, anchor);
    let mut bars = bars;
    for (index, bar) in bars.iter_mut().enumerate() {
        bar.source_sequence = u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| "Coinbase covering repair sequence overflowed".to_string())?;
        bar.validate().map_err(|error| error.to_string())?;
    }
    Ok(bars)
}

fn retain_anchored_history(
    bars: Vec<MarketBar>,
    maximum_bars: usize,
    anchor: Option<HistoryRange>,
) -> Vec<MarketBar> {
    if bars.len() <= maximum_bars {
        return bars;
    }
    let Some(anchor) = anchor else {
        return bars[bars.len().saturating_sub(maximum_bars)..].to_vec();
    };
    let before_end =
        bars.partition_point(|bar| bar.exchange_timestamp_unix_nanos < anchor.end_unix_nanos);
    let tail_start = bars.len().saturating_sub(VIEWPORT_LIVE_TAIL_RESERVE);
    let tail_count = bars.len().saturating_sub(tail_start);
    let viewport_capacity = maximum_bars.saturating_sub(tail_count);
    let viewport_start = before_end.saturating_sub(viewport_capacity);
    if tail_start <= before_end {
        return bars[bars.len().saturating_sub(maximum_bars)..].to_vec();
    }
    let mut retained = Vec::with_capacity(maximum_bars);
    retained.extend_from_slice(&bars[viewport_start..before_end]);
    retained.extend_from_slice(&bars[tail_start..]);
    retained
}

fn reconcile_history_repair(
    current: &SeriesSnapshot,
    repair: Vec<MarketBar>,
    maximum_bars: usize,
    interval: Option<CoinbaseInterval>,
    anchor: Option<HistoryRange>,
) -> Result<Vec<MarketBar>, String> {
    if maximum_bars == 0 || current.bars.is_empty() {
        return Err("covering history repair is empty".to_string());
    }
    if let Some(interval) = interval {
        return reconcile_interval_history(current, repair, maximum_bars, interval, anchor);
    }
    if repair.is_empty() {
        return Err("covering history repair is empty".to_string());
    }
    let mut merged = current.bars.to_vec();
    for mut repaired in repair {
        match merged.binary_search_by_key(&repaired.exchange_timestamp_unix_nanos, |bar| {
            bar.exchange_timestamp_unix_nanos
        }) {
            Ok(index) => {
                repaired.source_sequence = merged[index].source_sequence;
                merged[index] = repaired;
            }
            Err(0)
                if repaired.exchange_timestamp_unix_nanos
                    < merged[0].exchange_timestamp_unix_nanos => {}
            Err(index) if index == merged.len() => {
                repaired.source_sequence = merged
                    .last()
                    .and_then(|bar| bar.source_sequence.checked_add(1))
                    .ok_or_else(|| "covering history repair sequence overflowed".to_string())?;
                merged.push(repaired);
            }
            Err(_) => {
                return Err(
                    "covering history repair introduced interior sequence drift".to_string()
                );
            }
        }
    }
    let retained = retain_anchored_history(merged, maximum_bars, anchor);
    for bar in &retained {
        bar.validate().map_err(|error| error.to_string())?;
    }
    if retained.windows(2).any(|pair| {
        pair[0].source_sequence.checked_add(1) != Some(pair[1].source_sequence)
            || pair[0].exchange_timestamp_unix_nanos >= pair[1].exchange_timestamp_unix_nanos
    }) {
        return Err("covering history repair is not canonical".to_string());
    }
    Ok(retained)
}

fn recent_coinbase_history_range(
    series: &BarSeriesKey,
    maximum_bars: usize,
) -> Result<HistoryRange, String> {
    let interval = coinbase_series_interval(series)?;
    let end_seconds = interval.bucket_start(current_unix_nanos()?.div_euclid(1_000_000_000))?;
    let start_seconds = interval.shift_bucket(
        end_seconds,
        -i64::try_from(maximum_bars)
            .map_err(|_| "Coinbase recent-history span overflowed".to_string())?,
    )?;
    Ok(HistoryRange {
        start_unix_nanos: start_seconds.max(0).saturating_mul(1_000_000_000),
        end_unix_nanos: end_seconds.saturating_mul(1_000_000_000),
    })
}

fn coinbase_seam_repair(anchor: i64, trade_time: i64, interval: i64) -> Option<HistoryRange> {
    let start = anchor.checked_add(interval)?;
    let end = align_down(trade_time, interval);
    (end > start).then_some(HistoryRange {
        start_unix_nanos: start,
        end_unix_nanos: end,
    })
}

fn viewport_coinbase_history_range(
    series: &BarSeriesKey,
    viewport: Viewport,
) -> Result<HistoryRange, String> {
    let interval = coinbase_series_interval(series)?;
    let now_end = interval.bucket_start(current_unix_nanos()?.div_euclid(1_000_000_000))?;
    let visible_start =
        interval.bucket_start(viewport.start_unix_nanos.max(0).div_euclid(1_000_000_000))?;
    let viewport_end = viewport.end_unix_nanos.max(0).div_euclid(1_000_000_000);
    let mut visible_end = interval.bucket_start(viewport_end)?;
    if visible_end < viewport_end {
        visible_end = interval.shift_bucket(visible_end, 1)?;
    }
    visible_end = visible_end.min(now_end);
    if visible_start >= visible_end {
        return recent_coinbase_history_range(series, VIEWPORT_LIVE_TAIL_RESERVE);
    }
    let visible_bars = interval.buckets_between(visible_start, visible_end)?;
    let prefetched_start = interval.shift_bucket(visible_start, -visible_bars)?;
    let bounded_start = interval.shift_bucket(
        visible_end,
        -i64::try_from(VIEWPORT_BACKFILL_BARS)
            .map_err(|_| "Coinbase viewport-history span overflowed".to_string())?,
    )?;
    Ok(HistoryRange {
        start_unix_nanos: prefetched_start
            .max(bounded_start)
            .max(0)
            .saturating_mul(1_000_000_000),
        end_unix_nanos: visible_end.saturating_mul(1_000_000_000),
    })
}

fn viewport_follows_live(series: &BarSeriesKey, range: HistoryRange) -> Result<bool, String> {
    let recent = recent_coinbase_history_range(series, VIEWPORT_LIVE_TAIL_RESERVE)?;
    Ok(range.end_unix_nanos > recent.start_unix_nanos)
}

fn coinbase_history_page_range(
    series: &BarSeriesKey,
    missing: HistoryRange,
) -> Result<HistoryRange, String> {
    let interval = coinbase_series_interval(series)?;
    let source_bars_per_bucket = match interval {
        CoinbaseInterval::Minute1
        | CoinbaseInterval::Minute5
        | CoinbaseInterval::Minute15
        | CoinbaseInterval::Minute30
        | CoinbaseInterval::Hour1
        | CoinbaseInterval::Hour2
        | CoinbaseInterval::Day1 => 1,
        CoinbaseInterval::Hour4 | CoinbaseInterval::Hour12 => 2,
        CoinbaseInterval::Minute3 => 3,
        CoinbaseInterval::Hour8 => 4,
        CoinbaseInterval::Week1 => 7,
        CoinbaseInterval::Month1 => 31,
        CoinbaseInterval::Day3 => {
            return Err("unsupported Coinbase engine interval".to_string());
        }
    };
    let maximum_buckets = i64::from(350 / source_bars_per_bucket).max(1);
    let end_seconds = missing.end_unix_nanos.div_euclid(1_000_000_000);
    let page_start = interval
        .shift_bucket(end_seconds, -maximum_buckets)?
        .saturating_mul(1_000_000_000)
        .max(missing.start_unix_nanos);
    Ok(HistoryRange {
        start_unix_nanos: page_start,
        end_unix_nanos: missing.end_unix_nanos,
    })
}

fn coinbase_interval_nanos(series: &BarSeriesKey) -> Result<i64, String> {
    let seconds = match series.period {
        BarPeriod::Time { seconds } => i64::from(seconds),
        BarPeriod::Week { weeks: 1 } => 7 * 86_400,
        BarPeriod::Tick { .. }
        | BarPeriod::Session { .. }
        | BarPeriod::Week { .. }
        | BarPeriod::Month { .. } => return Err("Coinbase interval is not fixed".to_string()),
    };
    seconds
        .checked_mul(1_000_000_000)
        .ok_or_else(|| "Coinbase interval overflowed".to_string())
}

fn current_unix_nanos() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .ok_or_else(|| "system clock is unavailable".to_string())
}

const fn align_down(value: i64, interval: i64) -> i64 {
    value - value.rem_euclid(interval)
}

fn coinbase_bar_coverage_ranges(
    series: &BarSeriesKey,
    bars: &[MarketBar],
) -> Result<Vec<HistoryRange>, String> {
    if series.provider_id != "coinbase" || bars.is_empty() {
        return Ok(Vec::new());
    }
    let interval = coinbase_series_interval(series)?;
    let mut ranges = Vec::new();
    let mut start = bars[0].exchange_timestamp_unix_nanos;
    let mut previous = start;
    if !coinbase_bar_is_aligned(interval, &bars[0]) {
        return Err("Coinbase history timestamp is not interval aligned".to_string());
    }
    for bar in &bars[1..] {
        let timestamp = bar.exchange_timestamp_unix_nanos;
        if timestamp <= previous || !coinbase_bar_is_aligned(interval, bar) {
            return Err("Coinbase history timestamps are not strictly increasing".to_string());
        }
        let next = coinbase_shift_nanos(interval, previous, 1)?;
        if next != timestamp {
            ranges.push(HistoryRange {
                start_unix_nanos: start,
                end_unix_nanos: next,
            });
            start = timestamp;
        }
        previous = timestamp;
    }
    ranges.push(HistoryRange {
        start_unix_nanos: start,
        end_unix_nanos: coinbase_shift_nanos(interval, previous, 1)?,
    });
    Ok(ranges)
}

fn record_covered_range(
    coverage: &mut BTreeMap<BarSeriesKey, Vec<HistoryRange>>,
    series: &BarSeriesKey,
    range: HistoryRange,
) {
    let ranges = coverage.entry(series.clone()).or_default();
    ranges.push(range);
    ranges.sort_unstable();
    let mut merged: Vec<HistoryRange> = Vec::with_capacity(ranges.len());
    for range in ranges.drain(..) {
        if let Some(previous) = merged.last_mut()
            && range.start_unix_nanos <= previous.end_unix_nanos
        {
            previous.end_unix_nanos = previous.end_unix_nanos.max(range.end_unix_nanos);
        } else {
            merged.push(range);
        }
    }
    *ranges = merged;
}

fn coinbase_aggregator(profile: CoinbaseSeriesProfile) -> Result<CoinbaseBarAggregator, String> {
    CoinbaseBarAggregatorConfig::try_new_interval(
        profile.product_id,
        profile.price_scale,
        profile.quantity_scale,
        NonZeroU32::new(profile.interval_seconds).unwrap_or(NonZeroU32::MIN),
        NonZeroUsize::new(HISTORY_BARS_PER_SERIES).unwrap_or(NonZeroUsize::MIN),
    )
    .map(CoinbaseBarAggregator::new)
    .map_err(|error| error.to_string())
}

#[derive(Clone)]
struct CoinbaseSeriesProfile {
    product_id: String,
    resolution: &'static str,
    interval_seconds: u32,
    price_scale: u8,
    quantity_scale: u8,
    interval: CoinbaseInterval,
}

fn coinbase_series_profile(
    series: &BarSeriesKey,
    installed: &InstallProviderInstrument,
) -> Result<CoinbaseSeriesProfile, String> {
    if series.provider_id != "coinbase" || series.definition_version != 1 {
        return Err("unsupported Coinbase engine series identity".to_string());
    }
    if installed.provider != "coinbase"
        || installed.instrument_id != series.instrument_id
        || installed.entitlement_id != series.entitlement_id
        || installed.venue_id != "coinbase"
        || installed.price_scale > 18
        || installed.quantity_scale > 18
    {
        return Err("Coinbase installed instrument does not match the series".to_string());
    }
    let interval = coinbase_series_interval(series)?;
    Ok(CoinbaseSeriesProfile {
        product_id: installed.provider_symbol.clone(),
        resolution: interval.id(),
        interval_seconds: u32::try_from(interval.fixed_seconds().unwrap_or(86_400))
            .map_err(|_| "Coinbase interval is invalid".to_string())?,
        price_scale: u8::try_from(installed.price_scale)
            .map_err(|_| "Coinbase price scale is invalid".to_string())?,
        quantity_scale: u8::try_from(installed.quantity_scale)
            .map_err(|_| "Coinbase quantity scale is invalid".to_string())?,
        interval,
    })
}

fn coinbase_interval(seconds: u32) -> Result<CoinbaseInterval, String> {
    match seconds {
        60 => Ok(CoinbaseInterval::Minute1),
        180 => Ok(CoinbaseInterval::Minute3),
        300 => Ok(CoinbaseInterval::Minute5),
        900 => Ok(CoinbaseInterval::Minute15),
        1_800 => Ok(CoinbaseInterval::Minute30),
        3_600 => Ok(CoinbaseInterval::Hour1),
        7_200 => Ok(CoinbaseInterval::Hour2),
        14_400 => Ok(CoinbaseInterval::Hour4),
        28_800 => Ok(CoinbaseInterval::Hour8),
        43_200 => Ok(CoinbaseInterval::Hour12),
        86_400 => Ok(CoinbaseInterval::Day1),
        _ => Err("unsupported Coinbase engine derivation interval".to_string()),
    }
}

fn coinbase_series_interval(series: &BarSeriesKey) -> Result<CoinbaseInterval, String> {
    match series.period {
        BarPeriod::Time { seconds } => coinbase_interval(seconds),
        BarPeriod::Week { weeks: 1 } => Ok(CoinbaseInterval::Week1),
        BarPeriod::Month { months: 1 } => Ok(CoinbaseInterval::Month1),
        _ => Err("unsupported Coinbase engine interval".to_string()),
    }
}

fn coinbase_bar_is_aligned(interval: CoinbaseInterval, bar: &MarketBar) -> bool {
    bar.exchange_timestamp_unix_nanos.rem_euclid(1_000_000_000) == 0
        && interval.bucket_start(bar.exchange_timestamp_seconds)
            == Ok(bar.exchange_timestamp_seconds)
}

fn retain_completed_coinbase_bars(
    interval: CoinbaseInterval,
    completed_before_seconds: i64,
    bars: &mut Vec<MarketBar>,
) {
    if matches!(interval, CoinbaseInterval::Week1 | CoinbaseInterval::Month1) {
        bars.retain(|bar| bar.exchange_timestamp_seconds < completed_before_seconds);
    }
}

fn coinbase_shift_nanos(
    interval: CoinbaseInterval,
    timestamp_nanos: i64,
    buckets: i64,
) -> Result<i64, String> {
    interval
        .shift_bucket(timestamp_nanos.div_euclid(1_000_000_000), buckets)?
        .checked_mul(1_000_000_000)
        .ok_or_else(|| "Coinbase history coverage overflowed".to_string())
}

fn internal_series(series: &SeriesKey) -> Result<BarSeriesKey, String> {
    if series.provider.trim().is_empty()
        || series.instrument_id.trim().is_empty()
        || series.entitlement_id.trim().is_empty()
        || series.definition_revision == 0
    {
        return Err("market series identity is invalid".to_string());
    }
    let period = match SeriesCadence::try_from(series.cadence)
        .map_err(|_| "market series cadence is invalid".to_string())?
    {
        SeriesCadence::FixedSeconds => BarPeriod::time(series.cadence_value),
        SeriesCadence::Trades => BarPeriod::tick(series.cadence_value),
        SeriesCadence::SessionDays => BarPeriod::session(series.cadence_value),
        SeriesCadence::CalendarWeeks => BarPeriod::week(series.cadence_value),
        SeriesCadence::CalendarMonths => BarPeriod::month(series.cadence_value),
        SeriesCadence::Unspecified => {
            Err(axiusflow_market_data::MarketDataValidationError::InvalidPeriod)
        }
    }
    .map_err(|error| error.to_string())?;
    Ok(BarSeriesKey {
        provider_id: series.provider.clone(),
        instrument_id: series.instrument_id.clone(),
        entitlement_id: series.entitlement_id.clone(),
        period,
        definition_version: series.definition_revision,
    })
}

fn retained_hot_series(
    workspace: &WorkspaceState,
    available_memory_bytes: u64,
) -> Result<Vec<HotSeries>, String> {
    const MAXIMUM_HOT_SET_BYTES: u64 = 512 * 1024 * 1024;
    let estimated_entry_bytes = (HISTORY_BARS_PER_SERIES + 1)
        .saturating_mul(std::mem::size_of::<MarketBar>())
        .saturating_add(1_024);
    let memory_budget_bytes =
        usize::try_from((available_memory_bytes / 16).min(MAXIMUM_HOT_SET_BYTES))
            .unwrap_or(usize::MAX);
    let mut manager =
        HotSetManager::new(NonZeroUsize::new(MAXIMUM_SERIES).unwrap_or(NonZeroUsize::MIN));
    manager
        .restore(
            workspace
                .hot_series
                .iter()
                .map(crate::protocol_hot_entry)
                .collect::<Result<Vec<_>, _>>()?,
        )
        .map_err(|error| error.to_string())?;
    let active_workspaces = NonZeroU64::new(workspace.active_workspace_id)
        .map(WorkspaceId)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let watchlist = workspace
        .hot_series
        .iter()
        .filter(|series| {
            workspace
                .watchlist
                .iter()
                .any(|market| market == &series.market || market == &series.provider_symbol)
        })
        .map(|series| {
            internal_series(&SeriesKey {
                provider: series.provider.clone(),
                instrument_id: series.instrument_id.clone(),
                cadence_value: series.cadence_value,
                definition_revision: series.definition_revision,
                entitlement_id: series.entitlement_id.clone(),
                cadence: series.cadence,
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(manager
        .classify(
            &active_workspaces,
            &watchlist,
            memory_budget_bytes,
            estimated_entry_bytes,
        )
        .into_iter()
        .filter(|retention| retention.tier != HotSetTier::Cold)
        .map(|retention| crate::hot_entry_to_protocol(retention.entry))
        .collect())
}

fn warm_series(series: &HotSeries) -> Result<WarmSeries, String> {
    let expected_account = match series.provider.as_str() {
        "coinbase" => COINBASE_PUBLIC_ACCOUNT_ID,
        "rithmic" => RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID,
        _ => return Err("hot-set provider is unsupported".to_string()),
    };
    if series.account_id != expected_account
        || series.provider_symbol.trim().is_empty()
        || series.venue_id.trim().is_empty()
        || series.display_symbol.trim().is_empty()
        || series.price_scale > 18
        || series.quantity_scale > 18
    {
        return Err("hot-set instrument metadata is invalid".to_string());
    }
    let canonical = internal_series(&SeriesKey {
        provider: series.provider.clone(),
        instrument_id: series.instrument_id.clone(),
        cadence_value: series.cadence_value,
        definition_revision: series.definition_revision,
        entitlement_id: series.entitlement_id.clone(),
        cadence: series.cadence,
    })?;
    Ok(WarmSeries {
        series: canonical,
        instrument: InstallProviderInstrument {
            provider: series.provider.clone(),
            session_generation: series.provider_watermark.max(1),
            selection_generation: 1,
            instrument_id: series.instrument_id.clone(),
            provider_symbol: series.provider_symbol.clone(),
            display_symbol: series.display_symbol.clone(),
            venue_id: series.venue_id.clone(),
            price_scale: series.price_scale,
            quantity_scale: series.quantity_scale,
            entitlement_id: series.entitlement_id.clone(),
        },
        provider_watermark: series.provider_watermark,
    })
}

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
mod tests {
    use super::*;
    use axiusflow_coinbase_market_adapter::FixedPointValue;
    use axiusflow_market_data::{AggressorSide, EventMetadata, QualifiedTimestamp};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::Instant;

    struct FixtureHistory {
        bars: Vec<MarketBar>,
        fetches: Option<Arc<AtomicUsize>>,
    }

    enum FixtureRealtimeAction {
        Connected,
        Trade(CanonicalTrade),
        Heartbeat,
        Disconnect,
    }

    struct FixtureRealtime {
        actions: Receiver<FixtureRealtimeAction>,
        generations: SyncSender<ProviderGeneration>,
        stops: SyncSender<ProviderGeneration>,
        configured_products: Option<SyncSender<Vec<String>>>,
    }

    struct FixtureRealtimeHarness {
        service: MarketService,
        actions: SyncSender<FixtureRealtimeAction>,
        generations: Receiver<ProviderGeneration>,
        stops: Receiver<ProviderGeneration>,
        configured_products: Receiver<Vec<String>>,
        history_fetches: Arc<AtomicUsize>,
    }

    impl HistorySource for FixtureHistory {
        fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
            if request.kind != HistoryRequestKind::Initial {
                return Err("fixture history does not provide viewport backfill".to_string());
            }
            if let Some(fetches) = &self.fetches {
                fetches.fetch_add(1, Ordering::AcqRel);
            }
            let (interval_seconds, price_scale, quantity_scale) =
                if request.series.provider_id == "coinbase" {
                    let instrument = request
                        .instrument
                        .as_ref()
                        .ok_or_else(|| "fixture Coinbase instrument is unavailable".to_string())?;
                    let profile = coinbase_series_profile(&request.series, instrument)?;
                    (
                        profile.interval_seconds,
                        profile.price_scale,
                        profile.quantity_scale,
                    )
                } else {
                    let instrument = request
                        .instrument
                        .as_ref()
                        .ok_or_else(|| "fixture provider instrument is unavailable".to_string())?;
                    let interval_seconds = request
                        .series
                        .period
                        .duration_nanos()
                        .and_then(|nanos| u32::try_from(nanos / 1_000_000_000).ok())
                        .unwrap_or(1);
                    (
                        interval_seconds,
                        u8::try_from(instrument.price_scale)
                            .map_err(|_| "fixture price scale is invalid".to_string())?,
                        u8::try_from(instrument.quantity_scale)
                            .map_err(|_| "fixture quantity scale is invalid".to_string())?,
                    )
                };
            let mut bars = self.bars.clone();
            for (index, bar) in bars.iter_mut().enumerate() {
                bar.exchange_timestamp_seconds = i64::try_from(index + 1)
                    .ok()
                    .and_then(|value| value.checked_mul(i64::from(interval_seconds)))
                    .ok_or_else(|| "fixture history timestamp overflow".to_string())?;
                bar.exchange_timestamp_unix_nanos = bar
                    .exchange_timestamp_seconds
                    .checked_mul(1_000_000_000)
                    .ok_or_else(|| "fixture history timestamp overflow".to_string())?;
            }
            Ok(HistorySnapshot {
                price_scale,
                quantity_scale,
                bars,
                handoff_boundary_unix_nanos: None,
                confirmed_empty: false,
            })
        }
    }

    impl RealtimeSource for FixtureRealtime {
        fn configure(&mut self, products: Vec<String>) -> Result<(), String> {
            if let Some(configured_products) = &self.configured_products {
                configured_products
                    .send(products)
                    .map_err(|_| "configured-product observer disconnected".to_string())?;
            }
            Ok(())
        }

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

    impl MarketService {
        /// Starts a deterministic in-memory history source for IPC integration tests.
        pub(crate) fn start_fixture(bars: Vec<MarketBar>) -> Result<Self, String> {
            Self::start_with_sources(
                FixtureHistory {
                    bars,
                    fetches: None,
                },
                None,
                None,
            )
        }

        fn start_fixture_realtime(bars: Vec<MarketBar>) -> Result<FixtureRealtimeHarness, String> {
            Self::start_fixture_realtime_with_storage(bars, None)
        }

        fn start_fixture_realtime_with_storage(
            bars: Vec<MarketBar>,
            storage: Option<Result<LocalHistoryStore, String>>,
        ) -> Result<FixtureRealtimeHarness, String> {
            let (action_tx, action_rx) = mpsc::sync_channel(16);
            let (generation_tx, generation_rx) = mpsc::sync_channel(4);
            let (stop_tx, stop_rx) = mpsc::sync_channel(4);
            let (configured_products_tx, configured_products_rx) = mpsc::sync_channel(16);
            let history_fetches = Arc::new(AtomicUsize::new(0));
            let service = Self::start_with_sources(
                FixtureHistory {
                    bars,
                    fetches: Some(Arc::clone(&history_fetches)),
                },
                Some(Box::new(FixtureRealtime {
                    actions: action_rx,
                    generations: generation_tx,
                    stops: stop_tx,
                    configured_products: Some(configured_products_tx),
                })),
                storage,
            )?;
            Ok(FixtureRealtimeHarness {
                service,
                actions: action_tx,
                generations: generation_rx,
                stops: stop_rx,
                configured_products: configured_products_rx,
                history_fetches,
            })
        }

        fn start_with_source(source: impl HistorySource) -> Result<Self, String> {
            Self::start_with_sources(source, None, None)
        }

        fn start_with_sources(
            source: impl HistorySource,
            realtime: Option<Box<dyn RealtimeSource>>,
            storage: Option<Result<LocalHistoryStore, String>>,
        ) -> Result<Self, String> {
            let service = Self::start_composed(
                HistorySources::Shared(Box::new(source)),
                realtime,
                storage,
                false,
                0,
            )?;
            let series = internal_series(&btc())?;
            service.install_provider_instrument(&coinbase_instrument(&series))?;
            Ok(service)
        }
    }

    struct ControlledHistory {
        fetches: Arc<AtomicUsize>,
        release: Receiver<()>,
    }

    struct SwitchingHistory {
        requested: SyncSender<BarSeriesKey>,
        release: Receiver<()>,
    }

    struct DelayedCancellationHistory {
        started: SyncSender<(BarSeriesKey, ProviderGeneration)>,
        cancellation_observed: SyncSender<()>,
        release_cancellation: Receiver<()>,
        block_first: bool,
    }

    struct ControlledHistoryFailure {
        started: SyncSender<()>,
        release: Receiver<()>,
    }

    struct BlockingRithmicHistory {
        started: SyncSender<()>,
        cancelled: SyncSender<()>,
    }

    struct UncancellableHistory {
        started: SyncSender<()>,
        release: Receiver<()>,
        exited: SyncSender<()>,
    }

    fn retained_history_coordinator<'a>(
        engine: MarketEngine,
        history: &'a SyncSender<HistoryRequest>,
        storage: &'a SyncSender<StorageRequest>,
        realtime: &'a SyncSender<RealtimeControl>,
        realtime_stop: &'a Arc<AtomicBool>,
        consumer_id: ConsumerId,
        series: &BarSeriesKey,
    ) -> Coordinator<'a> {
        Coordinator {
            engine,
            coinbase_history: history,
            rithmic_history: history,
            storage,
            realtime_control: realtime,
            rithmic_realtime_control: None,
            rithmic_catalog_control: None,
            coinbase_catalog_control: None,
            realtime_stop,
            resource_mode: ResourceMode::Warm,
            resource_policy: decide_resource_policy(ResourcePolicyInput {
                mode: EngineResourceMode::Warm,
                available_memory_bytes: u64::MAX,
                consumer_count: 1,
                visible_consumer_count: 1,
                provider_series_limit: MAXIMUM_SERIES,
                hot_set_priority_count: 1,
            }),
            available_memory_bytes: u64::MAX,
            hot_set_priority_count: 1,
            last_consumer_activity: Instant::now(),
            attached: BTreeSet::new(),
            pending: BTreeMap::from([(
                series.clone(),
                vec![DemandWaiter {
                    consumer_id,
                    generation: GenerationId(id(1).expect("generation")),
                    started_at: Instant::now(),
                }],
            )]),
            history_inflight: BTreeMap::new(),
            history_cancellations: BTreeMap::new(),
            history_coverage: BTreeMap::new(),
            viewport_history_ranges: BTreeMap::new(),
            active_viewports: BTreeMap::new(),
            viewport_history_local_inflight: BTreeSet::new(),
            deferred_publications: BTreeSet::new(),
            pending_empty_repairs: BTreeMap::new(),
            empty_repair_retry_at: Instant::now(),
            local_history_deadlines: BTreeMap::new(),
            local_loaded: BTreeSet::new(),
            warming: BTreeSet::new(),
            warm_series: BTreeMap::new(),
            warm_priority: Vec::new(),
            retained_history: BTreeMap::new(),
            prewarmed: BTreeSet::new(),
            retained_live: BTreeSet::new(),
            warm_rithmic_search_generation: 0,
            events: BTreeMap::from([(consumer_id, ConsumerEvents::default())]),
            live: BTreeMap::new(),
            rithmic_live: BTreeMap::new(),
            rithmic_order_books: BTreeMap::new(),
            catalog: if series.provider_id == "coinbase" {
                BTreeMap::from([(
                    ("coinbase".to_string(), series.instrument_id.clone()),
                    coinbase_instrument(series),
                )])
            } else {
                BTreeMap::new()
            },
            catalog_sessions: BTreeMap::new(),
            catalog_selections: BTreeMap::new(),
            realtime_started: false,
            realtime_connected: false,
            rithmic_realtime_started: false,
            realtime_products: BTreeSet::new(),
        }
    }

    fn hot_coinbase() -> HotSeries {
        HotSeries {
            provider: "coinbase".to_string(),
            market: "BTC-USD".to_string(),
            interval_seconds: 60,
            score: 1,
            last_used_unix_seconds: 1,
            provider_watermark: 1,
            series_watermark: 1,
            viewport_start_unix_nanos: Some(1),
            viewport_end_unix_nanos: Some(2),
            account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            entitlement_id: ENTITLEMENT_CLASS.to_string(),
            cadence: SeriesCadence::FixedSeconds as i32,
            cadence_value: 60,
            definition_revision: 1,
            pinned: false,
            workspace_ids: vec![1],
            coverage_start_unix_nanos: Some(60_000_000_000),
            coverage_end_unix_nanos: Some(60_000_000_001),
            provider_symbol: "BTC-USD".to_string(),
            venue_id: "coinbase".to_string(),
            display_symbol: "BTC/USD".to_string(),
            price_scale: 2,
            quantity_scale: 8,
        }
    }

    fn hot_coinbase_series(
        market: &str,
        cadence_value: u32,
        workspace_id: u64,
        score: u32,
    ) -> HotSeries {
        let mut hot = hot_coinbase();
        let (base, display) = if market == "ETH-USD" {
            ("eth", "ETH/USD")
        } else {
            ("btc", "BTC/USD")
        };
        hot.market = market.to_string();
        hot.instrument_id = format!("instrument:coinbase:{base}:usd");
        hot.provider_symbol = market.to_string();
        hot.display_symbol = display.to_string();
        hot.interval_seconds = cadence_value;
        hot.cadence_value = cadence_value;
        hot.workspace_ids = vec![workspace_id];
        hot.score = score;
        hot.last_used_unix_seconds = u64::from(score);
        hot
    }

    #[test]
    fn persisted_hot_set_tiers_bound_real_startup_priority_by_workspace_pin_watchlist_and_memory() {
        let mut active = hot_coinbase_series("BTC-USD", 60, 7, 1);
        active.last_used_unix_seconds = 1;
        let mut pinned = hot_coinbase_series("BTC-USD", 300, 8, 2);
        pinned.pinned = true;
        let watchlist = hot_coinbase_series("ETH-USD", 60, 9, 3);
        let recent = hot_coinbase_series("BTC-USD", 900, 10, 4);
        let workspace = WorkspaceState {
            watchlist: vec!["ETH-USD".to_string()],
            active_workspace_id: 7,
            hot_series: vec![active, pinned, watchlist, recent],
            ..crate::default_workspace()
        };
        let estimated_entry_bytes = (HISTORY_BARS_PER_SERIES + 1)
            .saturating_mul(std::mem::size_of::<MarketBar>())
            .saturating_add(1_024);
        let retained = retained_hot_series(
            &workspace,
            u64::try_from(estimated_entry_bytes.saturating_mul(2).saturating_mul(16))
                .expect("memory budget fits"),
        )
        .expect("hot set classifies");
        assert_eq!(retained.len(), 2);
        assert_eq!(retained[0].workspace_ids, vec![7]);
        assert_eq!(retained[1].cadence_value, 300);
        assert!(retained[1].pinned);
    }

    #[test]
    fn coordinator_eviction_retains_only_the_current_bounded_warm_priority() {
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        let mut engine = configured_engine().expect("engine configures");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        let series = [60, 300, 900]
            .into_iter()
            .map(|cadence_value| {
                internal_series(&SeriesKey {
                    cadence_value,
                    ..btc()
                })
                .expect("series validates")
            })
            .collect::<Vec<_>>();
        for item in &series {
            engine
                .install_history(
                    ProviderGeneration(id(1).expect("provider generation")),
                    item,
                    2,
                    8,
                    vec![history_bar()],
                )
                .expect("cached history installs");
        }
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series[0],
        );
        coordinator.hot_set_priority_count = 3;
        coordinator.warm_priority = series.clone();
        coordinator.apply_resource_mode(ResourceMode::Constrained);

        assert!(coordinator.engine.series_snapshot(&series[0]).is_some());
        assert!(coordinator.engine.series_snapshot(&series[1]).is_some());
        assert!(coordinator.engine.series_snapshot(&series[2]).is_none());
        assert_eq!(coordinator.engine.metrics().stored_series, 2);
    }

    #[test]
    fn warm_restore_prefetches_local_history_and_installs_it_before_reattach() {
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        let (storage_tx, storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let series = internal_series(&btc()).expect("series");
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        let mut engine = configured_engine().expect("engine configures");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        engine
            .set_series_demand(
                consumer_id,
                GenerationId(id(1).expect("generation")),
                &series,
            )
            .expect("demand installs");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        coordinator.restore_hot_series(vec![warm_series(&hot_coinbase()).expect("hot series")]);
        let (requested, generation) = match storage_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("local warm read is requested")
        {
            StorageRequest::Read(series, generation) => (series, generation),
            StorageRequest::ReadRange(..) => panic!("warm restore uses exact history reads"),
            StorageRequest::Persist(..)
            | StorageRequest::RecordConfirmedEmpty(..)
            | StorageRequest::ResolveConfirmedEmpty(..) => {
                panic!("warm restore reads before persisting")
            }
        };
        assert_eq!(requested, series);
        coordinator.local_history_completed(
            &requested,
            generation,
            Ok(Some(StoredHistory {
                bars: vec![history_bar()],
                derived: false,
                durable: true,
            })),
        );
        assert!(coordinator.engine.series_snapshot(&series).is_some());
        assert!(coordinator.prewarmed.contains(&series));
    }

    #[test]
    fn constrained_policy_sets_the_real_provider_history_bound() {
        let (history_tx, history_rx) = mpsc::sync_channel(1);
        let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let series = internal_series(&btc()).expect("series");
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        let mut engine = configured_engine().expect("engine configures");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        coordinator.apply_resource_mode(ResourceMode::Constrained);
        coordinator
            .enqueue_history(
                &series,
                ProviderGeneration(id(1).expect("provider generation")),
            )
            .expect("bounded history enqueues");
        assert!(matches!(
            history_rx.recv_timeout(Duration::from_secs(1)),
            Ok(HistoryRequest {
                maximum_bars: 160,
                ..
            })
        ));
    }

    fn provider_instrument(
        session_generation: u64,
        selection_generation: u64,
    ) -> InstallProviderInstrument {
        InstallProviderInstrument {
            provider: "rithmic".to_string(),
            session_generation,
            selection_generation,
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            provider_symbol: "MNQU6".to_string(),
            display_symbol: "MNQU6".to_string(),
            venue_id: "CME".to_string(),
            price_scale: 2,
            quantity_scale: 0,
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
        }
    }

    fn rithmic_series_key(instrument_id: &str, entitlement_id: &str) -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: instrument_id.to_string(),
            entitlement_id: entitlement_id.to_string(),
            period: BarPeriod::time(60).expect("minute cadence"),
            definition_version: 1,
        }
    }

    fn install_rithmic_test_series(
        engine: &mut MarketEngine,
        raw_consumer: u64,
        generation: ProviderGeneration,
        series: &BarSeriesKey,
    ) -> axiusflow_market_engine::ConsumerPublication {
        let consumer_id = ConsumerId(id(raw_consumer).expect("consumer"));
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        engine
            .set_series_demand_with_streams(
                consumer_id,
                GenerationId(id(1).expect("generation")),
                series,
                chart_stream_requirements(series),
            )
            .expect("demand installs");
        engine
            .install_history(generation, series, 2, 0, vec![history_bar()])
            .expect("history installs")
            .pop()
            .expect("matching consumer receives history")
    }

    fn seeded_rithmic_live(
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) -> RithmicLiveHandoff {
        let mut live = RithmicLiveHandoff::new(series, generation, "CME").expect("live cadence");
        live.seed(2, 0, &[history_bar()], None)
            .expect("history seeds handoff");
        live.connected = true;
        live
    }

    fn complete_rithmic_test_history(
        coordinator: &mut Coordinator<'_>,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) {
        coordinator.history_completed(
            series,
            generation,
            None,
            HistoryRequestKind::Initial,
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 0,
                bars: vec![history_bar()],
                handoff_boundary_unix_nanos: None,
                confirmed_empty: false,
            }),
        );
    }

    fn rithmic_trade(sequence: u64, session: u64, nanos: i64, price: i64) -> MarketTrade {
        MarketTrade {
            metadata: EventMetadata {
                provider_id: "rithmic".to_string(),
                instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
                entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
                source_sequence: sequence,
                session_generation: session,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(nanos),
                    provider_unix_nanos: Some(nanos + 1),
                    received_unix_nanos: nanos + 2,
                },
            },
            trade_id: format!("trade-{sequence}"),
            price,
            quantity: 2,
            aggressor: AggressorSide::Buy,
        }
    }

    fn rithmic_depth_snapshot(
        series: &BarSeriesKey,
        source_sequence: u64,
        bid_quantity: i64,
    ) -> DepthSnapshot {
        let timestamp = i64::try_from(source_sequence).unwrap_or(i64::MAX);
        DepthSnapshot {
            metadata: EventMetadata {
                provider_id: "rithmic".to_string(),
                instrument_id: series.instrument_id.clone(),
                entitlement_id: series.entitlement_id.clone(),
                source_sequence,
                session_generation: 7,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(timestamp),
                    provider_unix_nanos: None,
                    received_unix_nanos: timestamp,
                },
            },
            bids: vec![DepthLevel {
                price: 20_000,
                quantity: bid_quantity,
                order_count: Some(3),
            }],
            asks: vec![DepthLevel {
                price: 20_025,
                quantity: 4,
                order_count: Some(2),
            }],
        }
    }

    fn pop_order_book(
        coordinator: &mut Coordinator<'_>,
        consumer_id: ConsumerId,
    ) -> IpcOrderBookSnapshot {
        let envelope::Payload::OrderBookSnapshot(snapshot) = coordinator
            .events
            .get_mut(&consumer_id)
            .and_then(ConsumerEvents::pop)
            .expect("order book publishes")
        else {
            panic!("expected order-book publication");
        };
        snapshot
    }

    #[test]
    fn pending_covering_snapshot_is_never_replaced_by_an_out_of_order_tail() {
        let series = btc();
        let covering_bar = history_bar();
        let mut events = ConsumerEvents {
            snapshot: Some(envelope::Payload::SeriesSnapshot(IpcSeriesSnapshot {
                consumer_id: 1,
                generation: 1,
                series: Some(series.clone()),
                provider_generation: 1,
                price_scale: 2,
                quantity_scale: 8,
                bars: vec![ipc_bar(covering_bar)],
                publication_generation: 4,
                forming: false,
            })),
            ..ConsumerEvents::default()
        };
        let stale = MarketBar {
            source_sequence: covering_bar.source_sequence.saturating_sub(1),
            ..covering_bar
        };
        events.publish_series_update(envelope::Payload::SeriesUpdate(
            axiusflow_engine_protocol::SeriesUpdate {
                consumer_id: 1,
                generation: 1,
                series: Some(series),
                provider_generation: 1,
                bar: Some(ipc_bar(stale)),
                forming: true,
                publication_generation: 5,
            },
        ));

        assert!(matches!(
            events.snapshot,
            Some(envelope::Payload::SeriesSnapshot(ref snapshot))
                if snapshot.bars == vec![ipc_bar(covering_bar)]
                    && snapshot.publication_generation == 4
        ));
    }

    #[test]
    fn rithmic_live_handoff_continues_fixed_and_tick_history_without_desktop_aggregation() {
        let generation = ProviderGeneration(id(7).expect("provider generation"));
        let fixed_series = BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            period: BarPeriod::time(60).expect("minute"),
            definition_version: 1,
        };
        let history = MarketBar {
            source_sequence: 40,
            exchange_timestamp_seconds: 60,
            exchange_timestamp_unix_nanos: 60_000_000_000,
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 7,
        };
        let mut fixed = RithmicLiveHandoff::new(&fixed_series, generation, "CME")
            .expect("fixed cadence streams");
        fixed.seed(2, 0, &[history], None).expect("history seeds");
        fixed
            .accept_trade(&rithmic_trade(1, 1, 70_000_000_000, 108))
            .expect("trade inside completed history is stale");
        assert_eq!(fixed.bars.len(), 1);
        assert_eq!(fixed.bars[0].close, 105);
        assert_eq!(fixed.bars[0].exchange_timestamp_unix_nanos, 60_000_000_000);
        fixed
            .accept_trade(&rithmic_trade(2, 1, 121_000_000_000, 115))
            .expect("next minute starts");
        assert_eq!(fixed.bars.len(), 2);
        assert_eq!(fixed.bars[1].source_sequence, 41);
        assert_eq!(fixed.bars[1].exchange_timestamp_seconds, 120);
        fixed
            .accept_trade(&rithmic_trade(3, 1, 125_000_000_000, 116))
            .expect("forming minute updates");
        assert_eq!(fixed.bars[1].close, 116);
        fixed.connected = true;
        assert!(matches!(
            fixed.take_publication(),
            Some(LiveSeriesPublication::Tail(bar))
                if bar.source_sequence == 41 && bar.close == 116
        ));
        fixed
            .accept_trade(&rithmic_trade(4, 1, 126_000_000_000, 117))
            .expect("forming minute receives its final revision");
        fixed
            .accept_trade(&rithmic_trade(5, 1, 181_000_000_000, 118))
            .expect("following minute starts before the next publication");
        assert!(matches!(
            fixed.take_publication(),
            Some(LiveSeriesPublication::Covering(bars))
                if bars.len() == 3
                    && bars[1].source_sequence == 41
                    && bars[1].close == 117
                    && bars[2].source_sequence == 42
                    && bars[2].close == 118
        ));
        let mut engine = configured_engine().expect("engine configures");
        engine
            .begin_provider_session("rithmic", generation)
            .expect("Rithmic generation begins");
        engine
            .install_history(generation, &fixed_series, 2, 0, vec![history])
            .expect("completed history installs");
        engine
            .install_realtime(generation, &fixed_series, 2, 0, fixed.bars.clone(), true)
            .expect("engine accepts the live forming suffix");

        let tick_series = BarSeriesKey {
            period: BarPeriod::tick(100).expect("tick"),
            ..fixed_series
        };
        let tick_history = MarketBar {
            exchange_timestamp_unix_nanos: 60_123_456_789,
            ..history
        };
        let mut tick =
            RithmicLiveHandoff::new(&tick_series, generation, "CME").expect("tick cadence streams");
        tick.seed(2, 0, &[tick_history], None)
            .expect("tick history seeds");
        tick.accept_trade(&rithmic_trade(1, 1, 60_500_000_000, 120))
            .expect("first trade starts a new tick bar");
        tick.accept_trade(&rithmic_trade(2, 1, 60_600_000_000, 121))
            .expect("second trade updates tick bar");
        assert_eq!(tick.bars.len(), 2);
        assert_eq!(tick.bars[1].source_sequence, 41);
        assert_eq!(tick.bars[1].close, 121);
        assert_eq!(tick.bars[1].volume, 4);
        assert_eq!(tick.bars[1].exchange_timestamp_unix_nanos, 60_500_000_000);
        assert!(
            tick.accept_trade(&rithmic_trade(4, 2, 60_700_000_000, 122))
                .is_err()
        );
    }

    #[test]
    fn rithmic_live_handoff_uses_the_exchange_calendar_for_weeks_and_months() {
        let generation = ProviderGeneration(id(7).expect("provider generation"));
        let cases = [
            (
                BarPeriod::week(1).expect("week"),
                1_787_529_600_i64,
                1_788_048_000_i64,
                1_788_125_400_000_000_000_i64,
                1_788_129_000_000_000_000_i64,
            ),
            (
                BarPeriod::month(1).expect("month"),
                1_785_542_400_i64,
                1_788_134_400_i64,
                1_788_211_800_000_000_000_i64,
                1_788_215_400_000_000_000_i64,
            ),
        ];
        for (period, marker_seconds, boundary_seconds, same_bucket, next_bucket) in cases {
            let series = BarSeriesKey {
                provider_id: "rithmic".to_string(),
                instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
                entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
                period,
                definition_version: 1,
            };
            let history = MarketBar {
                source_sequence: 40,
                exchange_timestamp_seconds: marker_seconds,
                exchange_timestamp_unix_nanos: marker_seconds * 1_000_000_000,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            };
            let mut live = RithmicLiveHandoff::new(&series, generation, "CME")
                .expect("calendar cadence streams");
            live.seed(2, 0, &[history], Some(boundary_seconds * 1_000_000_000))
                .expect("calendar history seeds");

            live.accept_trade(&rithmic_trade(1, 1, same_bucket, 112))
                .expect("same calendar bucket updates");
            assert_eq!(live.bars.len(), 1);
            assert_eq!(live.bars[0].close, 112);
            assert_eq!(live.bars[0].volume, 9);

            live.accept_trade(&rithmic_trade(2, 1, next_bucket, 115))
                .expect("next exchange session bucket starts");
            assert_eq!(live.bars.len(), 2);
            assert_eq!(live.bars[1].source_sequence, 41);
            assert_eq!(live.bars[1].close, 115);
            assert_eq!(live.bars[1].volume, 2);
        }
    }

    #[test]
    fn rithmic_depth_is_reconstructed_once_and_published_as_a_conflated_engine_book() {
        let consumer_id = ConsumerId(id(9).expect("consumer"));
        let client_id = ClientId(id(7).expect("client"));
        let series = BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            period: BarPeriod::time(60).expect("period"),
            definition_version: 1,
        };
        let mut engine = configured_engine().expect("engine");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id,
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        engine
            .set_series_demand_with_streams(
                consumer_id,
                GenerationId(id(3).expect("generation")),
                &series,
                chart_stream_requirements(&series),
            )
            .expect("depth demand installs");
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &stop,
            consumer_id,
            &series,
        );
        coordinator
            .install_provider_instrument(&provider_instrument(7, 2))
            .expect("instrument installs");
        coordinator.rithmic_depth(
            7,
            &DepthSnapshot {
                metadata: EventMetadata {
                    provider_id: "rithmic".to_string(),
                    instrument_id: series.instrument_id.clone(),
                    entitlement_id: series.entitlement_id.clone(),
                    source_sequence: 11,
                    session_generation: 7,
                    timestamps: QualifiedTimestamp {
                        exchange_unix_nanos: Some(20),
                        provider_unix_nanos: None,
                        received_unix_nanos: 21,
                    },
                },
                bids: vec![DepthLevel {
                    price: 20_000,
                    quantity: 7,
                    order_count: Some(3),
                }],
                asks: vec![DepthLevel {
                    price: 20_025,
                    quantity: 4,
                    order_count: Some(2),
                }],
            },
        );

        let envelope::Payload::OrderBookSnapshot(snapshot) = coordinator
            .events
            .get_mut(&consumer_id)
            .and_then(ConsumerEvents::pop)
            .expect("book publishes")
        else {
            panic!("engine must publish the order book");
        };
        assert_eq!(snapshot.consumer_id, 9);
        assert_eq!(snapshot.generation, 3);
        assert_eq!(snapshot.provider_generation, 7);
        assert_eq!(snapshot.selection_generation, 2);
        assert_eq!(snapshot.source_watermark, 11);
        assert_eq!(snapshot.state, IpcOrderBookState::Ready as i32);
        assert_eq!(snapshot.bids[0].quantity, 7);
        assert_eq!(snapshot.asks[0].price, 20_025);
    }

    #[test]
    fn unrelated_hidden_consumer_cannot_evict_visible_rithmic_depth() {
        let depth_consumer = ConsumerId(id(9).expect("depth consumer"));
        let unrelated_consumer = ConsumerId(id(10).expect("unrelated consumer"));
        let client_id = ClientId(id(7).expect("client"));
        let series = rithmic_series_key("instrument:rithmic:CME:MNQU6", "rithmic-test:CME:MNQU6");
        let mut engine = configured_engine().expect("engine");
        for consumer_id in [depth_consumer, unrelated_consumer] {
            engine
                .register_consumer(
                    ConsumerIdentity {
                        client_id,
                        workspace_id: WorkspaceId(id(1).expect("workspace")),
                        consumer_id,
                    },
                    true,
                )
                .expect("consumer registers");
        }
        engine
            .set_series_demand_with_streams(
                depth_consumer,
                GenerationId(id(3).expect("generation")),
                &series,
                chart_stream_requirements(&series),
            )
            .expect("depth demand installs");
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &stop,
            depth_consumer,
            &series,
        );
        coordinator
            .events
            .insert(unrelated_consumer, ConsumerEvents::default());
        coordinator
            .install_provider_instrument(&provider_instrument(7, 2))
            .expect("instrument installs");
        coordinator.rithmic_depth(7, &rithmic_depth_snapshot(&series, 11, 7));
        let first = pop_order_book(&mut coordinator, depth_consumer);
        assert_eq!(first.source_watermark, 11);
        assert_eq!(first.bids[0].quantity, 7);

        coordinator
            .engine
            .set_visibility(unrelated_consumer, false)
            .expect("unrelated consumer hides");
        coordinator.refresh_resource_policy();
        assert!(
            coordinator
                .rithmic_order_books
                .contains_key(&series.instrument_id)
        );
        coordinator.rithmic_depth(7, &rithmic_depth_snapshot(&series, 12, 9));
        let advanced = pop_order_book(&mut coordinator, depth_consumer);
        assert_eq!(advanced.source_watermark, 12);
        assert_eq!(advanced.bids[0].quantity, 9);

        coordinator
            .engine
            .set_visibility(depth_consumer, false)
            .expect("final visible depth reference hides");
        coordinator.refresh_resource_policy();
        assert!(coordinator.rithmic_order_books.is_empty());

        coordinator.apply_resource_mode(ResourceMode::MarketsLive);
        assert!(
            coordinator
                .rithmic_order_books
                .contains_key(&series.instrument_id)
        );
        coordinator.apply_resource_mode(ResourceMode::Warm);
        assert!(coordinator.rithmic_order_books.is_empty());

        coordinator
            .engine
            .set_visibility(depth_consumer, true)
            .expect("depth demand returns");
        coordinator.refresh_resource_policy();
        assert!(matches!(
            coordinator
                .rithmic_order_books
                .get(&series.instrument_id)
                .map(|book| book.book.state()),
            Some(CanonicalOrderBookState::Recovering(
                OrderBookRecoveryReason::AwaitingSnapshot
            ))
        ));
        coordinator.rithmic_depth(7, &rithmic_depth_snapshot(&series, 20, 12));
        let recovered = pop_order_book(&mut coordinator, depth_consumer);
        assert_eq!(recovered.source_watermark, 20);
        assert_eq!(recovered.state, IpcOrderBookState::Ready as i32);

        assert!(coordinator.engine.remove_consumer(depth_consumer));
        coordinator.events.remove(&depth_consumer);
        coordinator.refresh_resource_policy();
        assert!(coordinator.rithmic_order_books.is_empty());
    }

    #[test]
    fn provider_instrument_catalog_rejects_stale_and_conflicting_installs() {
        let service = MarketService::start_fixture(vec![history_bar()]).expect("market service");
        let installed = provider_instrument(2, 3);
        service
            .install_provider_instrument(&installed)
            .expect("install current instrument");
        service
            .install_provider_instrument(&installed)
            .expect("repeat idempotent install");

        assert!(
            service
                .install_provider_instrument(&provider_instrument(1, 4))
                .is_err()
        );
        assert!(
            service
                .install_provider_instrument(&provider_instrument(2, 2))
                .is_err()
        );
        let mut conflicting = installed;
        conflicting.provider_symbol = "NQU6".to_string();
        assert!(service.install_provider_instrument(&conflicting).is_err());
    }

    #[test]
    fn coinbase_catalog_does_not_share_selection_fences_between_consumers() {
        let service = MarketService::start_fixture(vec![history_bar()]).expect("market service");
        let product = |base: &str, selection_generation: u64| InstallProviderInstrument {
            provider: "coinbase".to_string(),
            session_generation: COINBASE_PROVIDER_GENERATION,
            selection_generation,
            instrument_id: format!("instrument:coinbase:{}:usd", base.to_ascii_lowercase()),
            provider_symbol: format!("{base}-USD"),
            display_symbol: format!("{base}/USD"),
            venue_id: "coinbase".to_string(),
            price_scale: 2,
            quantity_scale: 8,
            entitlement_id: ENTITLEMENT_CLASS.to_string(),
        };

        service
            .install_provider_instrument(&product("BTC", 2))
            .expect("BTC installs");
        service
            .install_provider_instrument(&product("ETH", 1))
            .expect("independent ETH generation installs");
        service
            .install_provider_instrument(&product("BTC", 1))
            .expect("independent consumer generation installs");
    }

    #[test]
    fn removing_or_detaching_consumers_releases_coinbase_catalog_authorization() {
        for detach in [false, true] {
            let client_id = ClientId(id(7).expect("client"));
            let consumer_id = ConsumerId(id(9).expect("consumer"));
            let series = internal_series(&btc()).expect("series");
            let mut engine = configured_engine().expect("engine configures");
            engine
                .register_consumer(
                    ConsumerIdentity {
                        client_id,
                        workspace_id: WorkspaceId(id(1).expect("workspace")),
                        consumer_id,
                    },
                    true,
                )
                .expect("consumer registers");
            let (history_tx, _history_rx) = mpsc::sync_channel(1);
            let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
            let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
            let realtime_stop = Arc::new(AtomicBool::new(false));
            let catalog = CoinbaseCatalogControl::test_control();
            catalog
                .authorize_consumer(consumer_id.0.get())
                .expect("catalog authorizes consumer");
            let mut coordinator = retained_history_coordinator(
                engine,
                &history_tx,
                &storage_tx,
                &realtime_tx,
                &realtime_stop,
                consumer_id,
                &series,
            );
            coordinator.coinbase_catalog_control = Some(&catalog);
            coordinator.attached.insert(client_id);
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);

            if detach {
                coordinator.handle_service_command(Command::Detach(client_id, reply_tx));
            } else {
                coordinator.handle_consumer_command(Command::Remove(
                    client_id,
                    consumer_id,
                    reply_tx,
                ));
            }

            assert_eq!(reply_rx.recv().expect("command replies"), Ok(()));
            assert!(!catalog.is_consumer_authorized(consumer_id.0.get()));
        }
    }

    #[test]
    fn reauthorized_consumer_discards_queued_coinbase_selection_from_old_generation() {
        let client_id = ClientId(id(7).expect("client"));
        let consumer_id = ConsumerId(id(9).expect("consumer"));
        let series = internal_series(&btc()).expect("series");
        let mut engine = configured_engine().expect("engine configures");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id,
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(false));
        let catalog = CoinbaseCatalogControl::test_control();
        catalog
            .authorize_consumer(consumer_id.0.get())
            .expect("first authorization installs");
        catalog.release_consumer(consumer_id.0.get());
        catalog
            .authorize_consumer(consumer_id.0.get())
            .expect("replacement authorization installs");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        coordinator.coinbase_catalog_control = Some(&catalog);
        let instrument = InstallProviderInstrument {
            provider: "coinbase".to_string(),
            session_generation: COINBASE_PROVIDER_GENERATION,
            selection_generation: 1,
            instrument_id: "instrument:coinbase:sol:usd".to_string(),
            provider_symbol: "SOL-USD".to_string(),
            display_symbol: "SOL/USD".to_string(),
            venue_id: "coinbase".to_string(),
            price_scale: 2,
            quantity_scale: 8,
            entitlement_id: ENTITLEMENT_CLASS.to_string(),
        };

        coordinator.handle_coinbase_catalog(CoinbaseCatalogEvent::SelectionResolved {
            consumer_id: consumer_id.0.get(),
            instrument: instrument.clone(),
            authorization_generation: 1,
        });

        assert!(!coordinator.catalog.contains_key(&(
            instrument.provider.clone(),
            instrument.instrument_id.clone()
        )));
        assert!(coordinator.events[&consumer_id].catalog_selection.is_none());
    }

    #[test]
    fn engine_catalog_selection_installs_identity_before_publication() {
        let mut engine = configured_engine().expect("engine configures");
        let client_id = ClientId(id(7).expect("client"));
        let consumer_id = ConsumerId(id(9).expect("consumer"));
        let series = BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            period: BarPeriod::tick(100).expect("tick period"),
            definition_version: 1,
        };
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id,
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &stop,
            consumer_id,
            &series,
        );
        let instrument = provider_instrument(7, 3);
        coordinator.handle_rithmic_catalog(RithmicCatalogEvent::SelectionResolved {
            consumer_id: consumer_id.0.get(),
            instrument: instrument.clone(),
        });

        let envelope::Payload::ProviderInstrumentSelection(selection) = coordinator
            .events
            .get_mut(&consumer_id)
            .and_then(ConsumerEvents::pop)
            .expect("selection publishes")
        else {
            panic!("engine must publish the installed selection");
        };
        assert_eq!(selection.consumer_id, consumer_id.0.get());
        assert_eq!(selection.instrument, Some(instrument.clone()));
        assert_eq!(
            coordinator.catalog.get(&(
                instrument.provider.clone(),
                instrument.instrument_id.clone()
            )),
            Some(&instrument)
        );
    }

    #[test]
    fn protocol_series_identity_roundtrips_every_rithmic_chart_cadence() {
        let periods = [
            BarPeriod::tick(100).expect("tick"),
            BarPeriod::time(60).expect("1m"),
            BarPeriod::time(180).expect("3m"),
            BarPeriod::time(300).expect("5m"),
            BarPeriod::time(900).expect("15m"),
            BarPeriod::time(1_800).expect("30m"),
            BarPeriod::time(3_600).expect("1h"),
            BarPeriod::time(7_200).expect("2h"),
            BarPeriod::time(14_400).expect("4h"),
            BarPeriod::time(28_800).expect("8h"),
            BarPeriod::time(43_200).expect("12h"),
            BarPeriod::session(1).expect("1D"),
            BarPeriod::session(3).expect("3D"),
            BarPeriod::week(1).expect("1W"),
            BarPeriod::month(1).expect("1M"),
        ];
        for period in periods {
            let internal = BarSeriesKey {
                provider_id: "rithmic".to_string(),
                instrument_id: "rithmic:CME:MNQU6".to_string(),
                entitlement_id: "rithmic-test:CME-Delayed:MNQU6".to_string(),
                period,
                definition_version: 1,
            };
            assert!(crate::rithmic_history::chart_interval(period).is_ok());
            assert_eq!(internal_series(&ipc_series(&internal)), Ok(internal));
        }

        let mut invalid = ipc_series(&BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "rithmic:CME:MNQU6".to_string(),
            entitlement_id: "rithmic-test:CME-Delayed:MNQU6".to_string(),
            period: BarPeriod::time(60).expect("1m"),
            definition_version: 1,
        });
        invalid.cadence = SeriesCadence::Unspecified as i32;
        assert!(internal_series(&invalid).is_err());
    }

    #[test]
    fn installed_rithmic_demand_uses_the_engine_history_owner() {
        let service = MarketService::start_fixture(vec![history_bar()]).expect("market service");
        service.attach(7).expect("client attaches");
        service
            .register_consumer(7, 1, 9)
            .expect("consumer registers");
        let series = SeriesKey {
            provider: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            cadence_value: 100,
            definition_revision: 1,
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            cadence: SeriesCadence::Trades as i32,
        };
        assert!(service.set_demand(7, 9, 1, &series).is_err());
        service
            .install_provider_instrument(&provider_instrument(11, 4))
            .expect("Rithmic instrument installs");
        service
            .set_demand(7, 9, 2, &series)
            .expect("Rithmic history demand is accepted");

        let event = poll_until(&service, 7, 9, |event| {
            matches!(event, envelope::Payload::SeriesSnapshot(_))
        });
        assert!(matches!(
            event,
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.generation == 2
                    && snapshot.provider_generation == 11
                    && snapshot.price_scale == 2
                    && snapshot.quantity_scale == 0
                    && snapshot.series == Some(series)
                    && snapshot.bars.len() == 1
        ));
    }

    #[test]
    fn newer_rithmic_catalog_session_install_advances_the_engine_generation() {
        let service = MarketService::start_fixture(vec![history_bar()]).expect("market service");
        service.attach(7).expect("client attaches");
        service
            .register_consumer(7, 1, 9)
            .expect("consumer registers");
        service
            .install_provider_instrument(&provider_instrument(11, 4))
            .expect("first catalog session installs");
        // A resident engine outlives desktop sessions, so a reopened catalog
        // session installs with a newer session generation. History demand is
        // fenced against the engine generation and must observe the new one.
        service
            .install_provider_instrument(&provider_instrument(12, 1))
            .expect("newer catalog session installs");
        let series = SeriesKey {
            provider: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            cadence_value: 100,
            definition_revision: 1,
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            cadence: SeriesCadence::Trades as i32,
        };
        service
            .set_demand(7, 9, 2, &series)
            .expect("Rithmic history demand is accepted");
        let event = poll_until(&service, 7, 9, |event| {
            matches!(event, envelope::Payload::SeriesSnapshot(_))
        });
        assert!(matches!(
            event,
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.provider_generation == 12 && snapshot.bars.len() == 1
        ));
    }

    #[test]
    fn rithmic_history_cancels_without_blocking_coinbase_history() {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (cancelled_tx, cancelled_rx) = mpsc::sync_channel(1);
        let service = MarketService::start_composed(
            HistorySources::Split {
                coinbase: Box::new(FixtureHistory {
                    bars: vec![history_bar()],
                    fetches: None,
                }),
                rithmic: Box::new(BlockingRithmicHistory {
                    started: started_tx,
                    cancelled: cancelled_tx,
                }),
            },
            None,
            None,
            false,
            0,
        )
        .expect("split provider history starts");
        service.attach(1).expect("client attaches");
        service
            .register_consumer(1, 1, 1)
            .expect("Rithmic consumer registers");
        service
            .register_consumer(1, 1, 2)
            .expect("Coinbase consumer registers");
        service
            .install_provider_instrument(&provider_instrument(5, 2))
            .expect("Rithmic instrument installs");
        let rithmic = SeriesKey {
            provider: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            cadence_value: 100,
            definition_revision: 1,
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            cadence: SeriesCadence::Trades as i32,
        };
        service
            .set_demand(1, 1, 1, &rithmic)
            .expect("Rithmic demand starts");
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Rithmic history is active");

        service
            .set_demand(1, 2, 1, &btc())
            .expect("Coinbase demand starts independently");
        assert!(matches!(
            poll_until(&service, 1, 2, |event| matches!(
                event,
                envelope::Payload::SeriesSnapshot(_)
            )),
            envelope::Payload::SeriesSnapshot(_)
        ));
        service
            .remove_consumer(1, 1)
            .expect("Rithmic consumer removes");
        cancelled_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Rithmic history cancellation reaches its worker");
    }

    impl HistorySource for ControlledHistory {
        fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
            let series = &request.series;
            self.fetches.fetch_add(1, Ordering::AcqRel);
            self.release
                .recv()
                .map_err(|_| "test history release disconnected".to_string())?;
            let profile = coinbase_series_profile(series, &coinbase_instrument(series))?;
            Ok(HistorySnapshot {
                price_scale: profile.price_scale,
                quantity_scale: profile.quantity_scale,
                bars: vec![MarketBar {
                    source_sequence: 1,
                    exchange_timestamp_seconds: i64::from(profile.interval_seconds),
                    exchange_timestamp_unix_nanos: i64::from(profile.interval_seconds)
                        * 1_000_000_000,
                    open: 100,
                    high: 110,
                    low: 90,
                    close: 105,
                    volume: 7,
                }],
                handoff_boundary_unix_nanos: None,
                confirmed_empty: false,
            })
        }
    }

    impl HistorySource for SwitchingHistory {
        fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
            let series = &request.series;
            self.requested
                .send(series.clone())
                .map_err(|_| "switch history observer disconnected".to_string())?;
            self.release
                .recv()
                .map_err(|_| "switch history release disconnected".to_string())?;
            let profile = coinbase_series_profile(series, &coinbase_instrument(series))?;
            Ok(HistorySnapshot {
                price_scale: profile.price_scale,
                quantity_scale: profile.quantity_scale,
                bars: vec![MarketBar {
                    source_sequence: 1,
                    exchange_timestamp_seconds: 0,
                    exchange_timestamp_unix_nanos: 0,
                    open: i64::from(profile.interval_seconds),
                    high: i64::from(profile.interval_seconds),
                    low: i64::from(profile.interval_seconds),
                    close: i64::from(profile.interval_seconds),
                    volume: 1,
                }],
                handoff_boundary_unix_nanos: None,
                confirmed_empty: false,
            })
        }
    }

    impl HistorySource for DelayedCancellationHistory {
        fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
            self.started
                .send((request.series.clone(), request.provider_generation))
                .map_err(|_| "history start observer disconnected".to_string())?;
            if std::mem::take(&mut self.block_first) {
                let deadline = Instant::now() + Duration::from_secs(2);
                while !request.stop.load(Ordering::Acquire) {
                    if Instant::now() >= deadline {
                        return Err("superseded history was not cancelled".to_string());
                    }
                    thread::yield_now();
                }
                self.cancellation_observed
                    .send(())
                    .map_err(|_| "cancellation observer disconnected".to_string())?;
                self.release_cancellation
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|_| "cancellation release timed out".to_string())?;
                return Err("superseded history was cancelled".to_string());
            }
            let instrument = request
                .instrument
                .as_ref()
                .ok_or_else(|| "fixture Coinbase instrument is unavailable".to_string())?;
            let profile = coinbase_series_profile(&request.series, instrument)?;
            Ok(HistorySnapshot {
                price_scale: profile.price_scale,
                quantity_scale: profile.quantity_scale,
                bars: vec![MarketBar {
                    exchange_timestamp_seconds: i64::from(profile.interval_seconds),
                    exchange_timestamp_unix_nanos: i64::from(profile.interval_seconds)
                        * 1_000_000_000,
                    ..history_bar()
                }],
                handoff_boundary_unix_nanos: None,
                confirmed_empty: false,
            })
        }
    }

    impl HistorySource for ControlledHistoryFailure {
        fn fetch(&mut self, _request: &HistoryRequest) -> Result<HistorySnapshot, String> {
            self.started
                .send(())
                .map_err(|_| "history failure observer disconnected".to_string())?;
            self.release
                .recv()
                .map_err(|_| "history failure release disconnected".to_string())?;
            Err("fixture provider history failed".to_string())
        }
    }

    impl HistorySource for BlockingRithmicHistory {
        fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
            self.started
                .send(())
                .map_err(|_| "Rithmic start observer disconnected".to_string())?;
            let deadline = Instant::now() + Duration::from_secs(2);
            while !request.stop.load(Ordering::Acquire) {
                if Instant::now() >= deadline {
                    return Err("Rithmic cancellation timed out".to_string());
                }
                thread::sleep(Duration::from_millis(1));
            }
            self.cancelled
                .send(())
                .map_err(|_| "Rithmic cancellation observer disconnected".to_string())?;
            Err("Rithmic history request was cancelled".to_string())
        }
    }

    impl HistorySource for UncancellableHistory {
        fn fetch(&mut self, _request: &HistoryRequest) -> Result<HistorySnapshot, String> {
            self.started
                .send(())
                .map_err(|_| "history start observer disconnected".to_string())?;
            self.release
                .recv()
                .map_err(|_| "history release disconnected".to_string())?;
            self.exited
                .send(())
                .map_err(|_| "history exit observer disconnected".to_string())?;
            Err("fixture history stopped".to_string())
        }
    }

    #[test]
    fn shutdown_cancels_inflight_history_and_joins_owned_workers() {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (cancelled_tx, cancelled_rx) = mpsc::sync_channel(1);
        let service = MarketService::start_with_source(BlockingRithmicHistory {
            started: started_tx,
            cancelled: cancelled_tx,
        })
        .expect("market service starts");
        service.attach(1).expect("client attaches");
        service
            .register_consumer(1, 1, 1)
            .expect("consumer registers");
        service
            .set_demand(1, 1, 1, &btc())
            .expect("history demand starts");
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("history worker starts");

        service
            .shutdown(Duration::from_secs(1))
            .expect("owned market workers stop before the deadline");

        cancelled_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("inflight history observes cancellation");
        assert!(service.attach(2).is_err());
    }

    #[test]
    fn shutdown_deadline_reports_an_uncancellable_worker_without_waiting_forever() {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let (exited_tx, exited_rx) = mpsc::sync_channel(1);
        let service = MarketService::start_with_source(UncancellableHistory {
            started: started_tx,
            release: release_rx,
            exited: exited_tx,
        })
        .expect("market service starts");
        service.attach(1).expect("client attaches");
        service
            .register_consumer(1, 1, 1)
            .expect("consumer registers");
        service
            .set_demand(1, 1, 1, &btc())
            .expect("history demand starts");
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("history worker starts");

        let error = service
            .shutdown(Duration::from_millis(20))
            .expect_err("uncancellable history must hit the process deadline");

        assert!(error.contains("axiusflow-coinbase-history"));
        release_tx.send(()).expect("blocked history releases");
        exited_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("detached worker exits after release");
    }

    fn btc() -> SeriesKey {
        SeriesKey {
            provider: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            cadence_value: 60,
            definition_revision: 1,
            entitlement_id: ENTITLEMENT_CLASS.to_string(),
            cadence: SeriesCadence::FixedSeconds as i32,
        }
    }

    fn coinbase_instrument(series: &BarSeriesKey) -> InstallProviderInstrument {
        let base = series
            .instrument_id
            .strip_prefix("instrument:coinbase:")
            .and_then(|value| value.strip_suffix(":usd"))
            .unwrap_or("btc")
            .to_ascii_uppercase();
        InstallProviderInstrument {
            provider: "coinbase".to_string(),
            session_generation: 1,
            selection_generation: 1,
            instrument_id: series.instrument_id.clone(),
            provider_symbol: format!("{base}-USD"),
            display_symbol: format!("{base}/USD"),
            venue_id: "coinbase".to_string(),
            price_scale: 2,
            quantity_scale: 8,
            entitlement_id: ENTITLEMENT_CLASS.to_string(),
        }
    }

    #[test]
    fn coinbase_calendar_history_drops_the_forming_bucket_before_persistence() {
        for interval in [CoinbaseInterval::Week1, CoinbaseInterval::Month1] {
            let completed_before = interval
                .bucket_start(1_787_299_200)
                .expect("calendar boundary");
            let previous = interval
                .shift_bucket(completed_before, -1)
                .expect("previous bucket");
            let mut bars = vec![
                MarketBar {
                    exchange_timestamp_seconds: previous,
                    exchange_timestamp_unix_nanos: previous * 1_000_000_000,
                    ..history_bar()
                },
                MarketBar {
                    exchange_timestamp_seconds: completed_before,
                    exchange_timestamp_unix_nanos: completed_before * 1_000_000_000,
                    ..history_bar()
                },
            ];

            retain_completed_coinbase_bars(interval, completed_before, &mut bars);

            assert_eq!(bars.len(), 1);
            assert_eq!(bars[0].exchange_timestamp_seconds, previous);
        }
    }

    fn selected_series(instrument_id: &str, interval_seconds: u32) -> SeriesKey {
        SeriesKey {
            provider: "coinbase".to_string(),
            instrument_id: instrument_id.to_string(),
            cadence_value: interval_seconds,
            definition_revision: 1,
            entitlement_id: ENTITLEMENT_CLASS.to_string(),
            cadence: SeriesCadence::FixedSeconds as i32,
        }
    }

    fn synthetic_coinbase_product(index: usize) -> (BarSeriesKey, InstallProviderInstrument) {
        let provider_symbol = format!("P{index:02}-USD");
        let instrument_id = format!("instrument:coinbase:p{index:02}:usd");
        (
            BarSeriesKey {
                provider_id: "coinbase".to_string(),
                instrument_id: instrument_id.clone(),
                entitlement_id: ENTITLEMENT_CLASS.to_string(),
                period: BarPeriod::time(60).expect("interval"),
                definition_version: 1,
            },
            InstallProviderInstrument {
                provider: "coinbase".to_string(),
                session_generation: 1,
                selection_generation: u64::try_from(index + 1).expect("selection generation"),
                instrument_id,
                provider_symbol: provider_symbol.clone(),
                display_symbol: provider_symbol,
                venue_id: "coinbase".to_string(),
                price_scale: 2,
                quantity_scale: 8,
                entitlement_id: ENTITLEMENT_CLASS.to_string(),
            },
        )
    }

    fn history_bar() -> MarketBar {
        MarketBar {
            source_sequence: 2,
            exchange_timestamp_seconds: 60,
            exchange_timestamp_unix_nanos: 60_000_000_000,
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 7,
        }
    }

    fn sequential_history(first_sequence: u64, count: usize, first_minute: i64) -> Vec<MarketBar> {
        (0..count)
            .map(|index| {
                let offset = u64::try_from(index).expect("history index fits");
                let minute = first_minute + i64::try_from(index).expect("history minute fits");
                MarketBar {
                    source_sequence: first_sequence + offset,
                    exchange_timestamp_seconds: minute * 60,
                    exchange_timestamp_unix_nanos: minute * 60_000_000_000,
                    open: 100,
                    high: 110,
                    low: 90,
                    close: 105,
                    volume: 7,
                }
            })
            .collect()
    }

    fn expect_range_read(request: &StorageRequest) -> HistoryRange {
        let StorageRequest::ReadRange(_, _, range) = request else {
            panic!("viewport uses a range read");
        };
        *range
    }

    #[test]
    fn viewport_demand_enqueues_one_bounded_missing_history_range() {
        let (history_tx, history_rx) = mpsc::sync_channel(1);
        let (storage_tx, storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let mut engine = configured_engine().expect("engine configures");
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        let generation = GenerationId(id(1).expect("generation"));
        let series = internal_series(&btc()).expect("series");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        engine
            .set_series_demand(consumer_id, generation, &series)
            .expect("demand installs");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        let interval = 60_000_000_000_i64;
        let end = align_down(current_unix_nanos().expect("clock"), interval)
            .saturating_sub(100 * interval);
        let viewport =
            Viewport::try_new(end.saturating_sub(200 * interval), end).expect("viewport");

        coordinator
            .request_viewport_history(consumer_id, generation, viewport)
            .expect("viewport demand applies");

        let local = storage_rx.try_recv().expect("local range read is queued");
        let range = match local {
            StorageRequest::ReadRange(_, _, range) => range,
            StorageRequest::Read(..)
            | StorageRequest::Persist(..)
            | StorageRequest::RecordConfirmedEmpty(..)
            | StorageRequest::ResolveConfirmedEmpty(..) => {
                panic!("viewport uses a range read")
            }
        };
        coordinator.viewport_history_local_completed(
            &series,
            ProviderGeneration(NonZeroU64::MIN),
            range,
            Err("fixture local history is unavailable".to_string()),
        );
        let request = history_rx.try_recv().expect("missing range is queued");
        let range = request.range.expect("Coinbase range is explicit");
        assert_eq!(request.kind, HistoryRequestKind::ViewportBackfill);
        assert!(range.start_unix_nanos <= viewport.start_unix_nanos);
        assert!(range.end_unix_nanos >= viewport.end_unix_nanos);
        assert!(
            range.end_unix_nanos - range.start_unix_nanos
                <= i64::try_from(VIEWPORT_BACKFILL_BARS).expect("bound fits") * interval
        );
        assert!(history_rx.try_recv().is_err());
    }

    #[test]
    fn cache_protection_retains_active_viewport_after_repair_and_includes_live_tail() {
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let mut engine = configured_engine().expect("engine configures");
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        let consumer_generation = GenerationId(id(1).expect("generation"));
        let provider_generation = ProviderGeneration(NonZeroU64::MIN);
        let series = internal_series(&btc()).expect("series");
        let bars = sequential_history(1, 1_000, 1);
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        engine
            .set_series_demand(consumer_id, consumer_generation, &series)
            .expect("demand installs");
        engine
            .install_history(provider_generation, &series, 2, 8, bars.clone())
            .expect("history installs");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        coordinator.live.insert(
            series.clone(),
            LiveHandoff::try_new(&series, provider_generation, &coinbase_instrument(&series))
                .expect("live handoff"),
        );
        let viewport = HistoryRange {
            start_unix_nanos: 120_000_000_000,
            end_unix_nanos: 240_000_000_000,
        };
        coordinator.active_viewports.insert(
            consumer_id,
            ActiveViewport {
                consumer_generation,
                provider_generation,
                series: series.clone(),
                range: viewport,
            },
        );
        coordinator.viewport_history_ranges.clear();

        let protected = coordinator.protected_history_ranges();
        assert!(protected.contains(&(series.clone(), viewport)));
        assert!(
            protected.contains(&(
                series.clone(),
                HistoryRange {
                    start_unix_nanos: bars[bars.len() - VIEWPORT_LIVE_TAIL_RESERVE]
                        .exchange_timestamp_unix_nanos,
                    end_unix_nanos: bars
                        .last()
                        .expect("live tail")
                        .exchange_timestamp_unix_nanos
                        + 60_000_000_000,
                },
            ))
        );
        assert_eq!(protected.len(), 2);
    }

    #[test]
    fn saturated_persistence_lane_retries_real_bar_empty_evidence_repair() {
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        let (storage_tx, storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let engine = configured_engine().expect("engine configures");
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        let series = internal_series(&btc()).expect("series");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        storage_tx
            .try_send(StorageRequest::Read(
                series.clone(),
                ProviderGeneration(NonZeroU64::MIN),
            ))
            .expect("storage lane fills");

        coordinator.enqueue_persistence(
            &series,
            ProviderGeneration(NonZeroU64::MIN),
            vec![history_bar()],
            false,
            "fixture persistence is unavailable",
        );

        assert!(coordinator.pending_empty_repairs.contains_key(&series));
        let _ = storage_rx.try_recv().expect("filler request drains");
        coordinator.empty_repair_retry_at = Instant::now();
        coordinator.retry_pending_empty_repairs();
        assert!(matches!(
            storage_rx.try_recv(),
            Ok(StorageRequest::ResolveConfirmedEmpty(ref repaired, range))
                if repaired == &series
                    && range.start_unix_nanos == 60_000_000_000
                    && range.end_unix_nanos == 120_000_000_000
        ));
        assert!(coordinator.pending_empty_repairs.is_empty());
    }

    #[test]
    fn confirmed_empty_viewport_page_is_persisted_and_skipped_after_restore() {
        let (history_tx, history_rx) = mpsc::sync_channel(1);
        let (storage_tx, storage_rx) = mpsc::sync_channel(2);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let mut engine = configured_engine().expect("engine configures");
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        let generation = GenerationId(id(1).expect("generation"));
        let series = internal_series(&btc()).expect("series");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        engine
            .set_series_demand(consumer_id, generation, &series)
            .expect("demand installs");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        let provider_generation = coordinator
            .provider_generation_for_series(&series)
            .expect("provider generation");
        let interval = 60_000_000_000_i64;
        let end = align_down(current_unix_nanos().expect("clock"), interval)
            .saturating_sub(2_000 * interval);
        let viewport =
            Viewport::try_new(end.saturating_sub(200 * interval), end).expect("viewport");

        coordinator
            .request_viewport_history(consumer_id, generation, viewport)
            .expect("viewport demand applies");
        let local_range = expect_range_read(&storage_rx.try_recv().expect("local read is queued"));
        coordinator.viewport_history_local_completed(
            &series,
            provider_generation,
            local_range,
            Err("fixture local history is unavailable".to_string()),
        );
        let provider_request = history_rx.try_recv().expect("provider page is queued");
        let page = provider_request.range.expect("provider page is explicit");
        coordinator.history_completed(
            &series,
            provider_generation,
            Some(page),
            HistoryRequestKind::ViewportBackfill,
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 8,
                bars: Vec::new(),
                handoff_boundary_unix_nanos: Some(page.end_unix_nanos),
                confirmed_empty: true,
            }),
        );
        assert!(matches!(
            storage_rx.try_recv().expect("empty page is persisted"),
            StorageRequest::RecordConfirmedEmpty(ref requested, range)
                if requested == &series && range == page
        ));
        let continuation = history_rx
            .try_recv()
            .expect("empty range does not suppress other history");
        assert_ne!(continuation.range, Some(page));

        coordinator.history_inflight.clear();
        coordinator.history_cancellations.clear();
        coordinator.history_coverage.clear();
        coordinator.viewport_history_ranges.clear();
        coordinator.viewport_history_local_inflight.clear();
        coordinator
            .request_viewport_history(consumer_id, generation, viewport)
            .expect("restored viewport checks local coverage");
        let restored_range = expect_range_read(
            &storage_rx
                .try_recv()
                .expect("restored local read is queued"),
        );
        coordinator.viewport_history_local_completed(
            &series,
            provider_generation,
            restored_range,
            Ok(LocalRangeHistory {
                stored: None,
                confirmed_empty: vec![page],
            }),
        );
        let restored_request = history_rx
            .try_recv()
            .expect("uncovered history continues after restored empty range");
        assert_ne!(restored_request.range, Some(page));
    }

    #[test]
    fn initial_coinbase_request_with_explicit_range_arms_bounded_viewport_repair() {
        let (history_tx, history_rx) = mpsc::sync_channel(1);
        let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let mut engine = configured_engine().expect("engine configures");
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        let generation = GenerationId(id(1).expect("generation"));
        let series = internal_series(&btc()).expect("series");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        engine
            .set_series_demand(consumer_id, generation, &series)
            .expect("demand installs");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        let provider_generation = coordinator
            .provider_generation_for_series(&series)
            .expect("provider generation");
        let interval = 60_000_000_000_i64;
        let end_minute = current_unix_nanos().expect("clock") / interval;
        let initial_range = recent_coinbase_history_range(
            &series,
            coordinator.resource_policy.history_prefetch_bars.max(1),
        )
        .expect("initial range");
        coordinator.history_completed(
            &series,
            provider_generation,
            Some(initial_range),
            HistoryRequestKind::Initial,
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 8,
                bars: sequential_history(1, 350, end_minute - 350),
                handoff_boundary_unix_nanos: Some(end_minute * interval),
                confirmed_empty: false,
            }),
        );

        let request = history_rx
            .try_recv()
            .expect("initial viewport repair queues");
        let range = request.range.expect("initial repair range is explicit");
        assert_eq!(request.kind, HistoryRequestKind::ViewportBackfill);
        assert!(
            range.end_unix_nanos - range.start_unix_nanos
                <= i64::try_from(HISTORY_BARS_PER_SERIES).expect("history bound fits") * interval
        );
        assert!(request.maximum_bars <= 350);
    }
    /// One consumer demanding BTC-USD 1m with a shorter-than-prefetch initial
    /// window installed, so the working-window backfill stays disarmed.
    #[allow(clippy::type_complexity)]
    fn disarmed_backfill_fixture<'a>(
        history: &'a SyncSender<HistoryRequest>,
        storage: &'a SyncSender<StorageRequest>,
        realtime: &'a SyncSender<RealtimeControl>,
        realtime_stop: &'a Arc<AtomicBool>,
        storage_rx: &mpsc::Receiver<StorageRequest>,
    ) -> (
        Coordinator<'a>,
        ConsumerId,
        GenerationId,
        ProviderGeneration,
        BarSeriesKey,
        i64,
        fn(Vec<MarketBar>) -> HistorySnapshot,
    ) {
        let mut engine = configured_engine().expect("engine configures");
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        let generation = GenerationId(id(1).expect("generation"));
        let series = internal_series(&btc()).expect("series");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        engine
            .set_series_demand(consumer_id, generation, &series)
            .expect("demand installs");
        let mut coordinator = retained_history_coordinator(
            engine,
            history,
            storage,
            realtime,
            realtime_stop,
            consumer_id,
            &series,
        );
        let provider_generation = coordinator
            .provider_generation_for_series(&series)
            .expect("provider generation");
        let interval = 60_000_000_000_i64;
        let end_minute = current_unix_nanos().expect("clock") / interval;
        coordinator.history_completed(
            &series,
            provider_generation,
            Some(
                recent_coinbase_history_range(
                    &series,
                    coordinator.resource_policy.history_prefetch_bars.max(1),
                )
                .expect("initial range"),
            ),
            HistoryRequestKind::Initial,
            Ok(backfill_snapshot(sequential_history(
                1,
                200,
                end_minute - 200,
            ))),
        );
        assert!(coordinator.events[&consumer_id].snapshot.is_some());
        coordinator
            .events
            .get_mut(&consumer_id)
            .expect("events")
            .snapshot = None;
        let _ = storage_rx.try_recv().expect("initial history persists");
        (
            coordinator,
            consumer_id,
            generation,
            provider_generation,
            series,
            end_minute,
            backfill_snapshot,
        )
    }

    fn backfill_snapshot(bars: Vec<MarketBar>) -> HistorySnapshot {
        HistorySnapshot {
            price_scale: 2,
            quantity_scale: 8,
            bars,
            handoff_boundary_unix_nanos: None,
            confirmed_empty: false,
        }
    }

    #[test]
    fn visible_viewport_pages_publish_and_background_pages_defer() {
        let (history_tx, history_rx) = mpsc::sync_channel(1);
        let (storage_tx, storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let (
            mut coordinator,
            consumer_id,
            generation,
            provider_generation,
            series,
            end_minute,
            snapshot,
        ) = disarmed_backfill_fixture(
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            &storage_rx,
        );
        let interval = 60_000_000_000_i64;
        let visible = Viewport::try_new(
            (end_minute - 2_000) * interval,
            (end_minute - 1_900) * interval,
        )
        .expect("visible viewport");
        coordinator
            .request_viewport_history(consumer_id, generation, visible)
            .expect("viewport demand queues");
        let StorageRequest::ReadRange(_, _, local_range) =
            storage_rx.try_recv().expect("local range read")
        else {
            panic!("viewport reads local coverage first");
        };
        coordinator.viewport_history_local_completed(
            &series,
            provider_generation,
            local_range,
            Err("fixture cache miss".to_string()),
        );
        let page = history_rx
            .recv()
            .expect("visible viewport schedules provider repair")
            .range
            .expect("provider page is explicit");
        let minutes = usize::try_from((page.end_unix_nanos - page.start_unix_nanos) / interval)
            .expect("page width fits");
        let start_minute = page.start_unix_nanos / interval;
        coordinator.history_completed(
            &series,
            provider_generation,
            Some(page),
            HistoryRequestKind::ViewportBackfill,
            Ok(snapshot(sequential_history(1, minutes, start_minute))),
        );
        assert!(matches!(
            coordinator.events[&consumer_id].snapshot,
            Some(envelope::Payload::SeriesSnapshot(ref snapshot)) if snapshot.bars.len() == 200 + minutes
        ));

        coordinator
            .events
            .get_mut(&consumer_id)
            .expect("events")
            .snapshot = None;
        let _ = storage_rx.try_recv().expect("visible page persists");
        coordinator.history_completed(
            &series,
            provider_generation,
            Some(HistoryRange {
                start_unix_nanos: (end_minute - 5_000) * interval,
                end_unix_nanos: (end_minute - 4_800) * interval,
            }),
            HistoryRequestKind::ViewportBackfill,
            Ok(snapshot(sequential_history(1, 200, end_minute - 5_000))),
        );
        assert!(
            coordinator.events[&consumer_id].snapshot.is_none(),
            "a disjoint background page defers its snapshot"
        );
        assert_eq!(
            coordinator
                .engine
                .series_snapshot(&series)
                .expect("background page installs")
                .bars
                .len(),
            400 + minutes
        );
        assert!(matches!(
            storage_rx.try_recv(),
            Ok(StorageRequest::Persist(_, _, ref bars, _, _, _)) if bars.len() == 200
        ));
    }

    #[test]
    fn resolving_the_demanded_repair_range_flushes_one_accumulated_covering_snapshot() {
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        let (storage_tx, storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let (mut coordinator, consumer_id, generation, provider_generation, series, end_minute, _) =
            disarmed_backfill_fixture(
                &history_tx,
                &storage_tx,
                &realtime_tx,
                &realtime_stop,
                &storage_rx,
            );
        let interval = 60_000_000_000_i64;
        // Install one background page silently while no plan is pending.
        coordinator.history_completed(
            &series,
            provider_generation,
            Some(HistoryRange {
                start_unix_nanos: (end_minute - 5_000) * interval,
                end_unix_nanos: (end_minute - 4_800) * interval,
            }),
            HistoryRequestKind::ViewportBackfill,
            Ok(backfill_snapshot(sequential_history(
                1,
                200,
                end_minute - 5_000,
            ))),
        );
        assert_eq!(
            coordinator
                .engine
                .series_snapshot(&series)
                .expect("background page installs")
                .bars
                .len(),
            400
        );
        let _ = storage_rx.try_recv().expect("background page persists");

        // Demanding the same region and failing its repair resolves the plan
        // and must flush one covering snapshot carrying the silent install.
        let resolution = Viewport::try_new(
            (end_minute - 5_100) * interval,
            (end_minute - 4_900) * interval,
        )
        .expect("resolution viewport");
        coordinator
            .request_viewport_history(consumer_id, generation, resolution)
            .expect("resolution demand queues");
        let StorageRequest::ReadRange(_, _, resolution_range) =
            storage_rx.try_recv().expect("resolution reads coverage")
        else {
            panic!("resolution reads local coverage first");
        };
        coordinator.viewport_history_local_completed(
            &series,
            provider_generation,
            resolution_range,
            Err("fixture cache miss".to_string()),
        );
        coordinator
            .events
            .get_mut(&consumer_id)
            .expect("events")
            .demand_error = None;
        coordinator.history_completed(
            &series,
            provider_generation,
            coordinator
                .viewport_history_ranges
                .get(&(series.clone(), provider_generation))
                .copied(),
            HistoryRequestKind::ViewportBackfill,
            Err("fixture backfill superseded".to_string()),
        );
        assert!(matches!(
            coordinator.events[&consumer_id].snapshot,
            Some(envelope::Payload::SeriesSnapshot(ref snapshot))
                if snapshot.bars.len() == 400
        ));
        assert!(coordinator.deferred_publications.is_empty());
    }

    #[test]
    fn newer_viewport_rearms_after_an_older_backfill_fails() {
        let (history_tx, history_rx) = mpsc::sync_channel(1);
        let (storage_tx, storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let mut engine = configured_engine().expect("engine configures");
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        let generation = GenerationId(id(1).expect("generation"));
        let series = internal_series(&btc()).expect("series");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        engine
            .set_series_demand(consumer_id, generation, &series)
            .expect("demand installs");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        let provider_generation = coordinator
            .provider_generation_for_series(&series)
            .expect("provider generation");
        let interval = 60_000_000_000_i64;
        let end = align_down(current_unix_nanos().expect("clock"), interval);
        let older =
            Viewport::try_new(end - 800 * interval, end - 700 * interval).expect("older viewport");
        let newest = Viewport::try_new(end - 1_600 * interval, end - 1_500 * interval)
            .expect("newest viewport");

        coordinator
            .request_viewport_history(consumer_id, generation, older)
            .expect("older viewport queues");
        let first = match storage_rx.try_recv().expect("older local request") {
            StorageRequest::ReadRange(_, _, range) => range,
            StorageRequest::Read(..)
            | StorageRequest::Persist(..)
            | StorageRequest::RecordConfirmedEmpty(..)
            | StorageRequest::ResolveConfirmedEmpty(..) => {
                panic!("viewport uses a range read")
            }
        };
        coordinator
            .request_viewport_history(consumer_id, generation, newest)
            .expect("newer viewport is retained");
        coordinator.viewport_history_local_completed(
            &series,
            provider_generation,
            first,
            Err("fixture failure".to_string()),
        );

        let replacement = match storage_rx.try_recv().expect("newer local request") {
            StorageRequest::ReadRange(_, _, range) => range,
            StorageRequest::Read(..)
            | StorageRequest::Persist(..)
            | StorageRequest::RecordConfirmedEmpty(..)
            | StorageRequest::ResolveConfirmedEmpty(..) => {
                panic!("viewport uses a range read")
            }
        };
        coordinator.viewport_history_local_completed(
            &series,
            provider_generation,
            replacement,
            Err("fixture local history is unavailable".to_string()),
        );
        let replacement = history_rx.try_recv().expect("newer viewport rearms");
        assert_eq!(replacement.kind, HistoryRequestKind::ViewportBackfill);
        assert_ne!(replacement.range, Some(first));
        assert!(
            replacement
                .range
                .is_some_and(|range| range.start_unix_nanos <= newest.start_unix_nanos)
        );
    }

    #[test]
    fn viewport_history_merge_retains_visible_backfill_and_latest_live_tail() {
        let series = internal_series(&btc()).expect("series");
        let backfill = sequential_history(1, VIEWPORT_BACKFILL_BARS + 100, 1);
        let current = SeriesSnapshot {
            series,
            provider_generation: ProviderGeneration(NonZeroU64::MIN),
            publication_generation: 1,
            price_scale: 2,
            quantity_scale: 8,
            forming: true,
            bars: sequential_history(100_000, 1_000, 100_000).into(),
        };

        let anchor = HistoryRange {
            start_unix_nanos: 60_000_000_000,
            end_unix_nanos: i64::try_from(VIEWPORT_BACKFILL_BARS + 101)
                .expect("history bound fits")
                * 60_000_000_000,
        };
        let merged = reconcile_history_repair(
            &current,
            backfill,
            HISTORY_BARS_PER_SERIES,
            Some(CoinbaseInterval::Minute1),
            Some(anchor),
        )
        .expect("merge");

        assert_eq!(merged.len(), HISTORY_BARS_PER_SERIES);
        assert_eq!(merged[0].exchange_timestamp_seconds, 101 * 60);
        assert_eq!(
            merged[HISTORY_BARS_PER_SERIES - VIEWPORT_LIVE_TAIL_RESERVE].exchange_timestamp_seconds,
            100_488 * 60
        );
        assert_eq!(
            merged.last().map(|bar| bar.exchange_timestamp_seconds),
            Some(100_999 * 60)
        );
        assert_eq!(merged[0].source_sequence, 1);
        assert_eq!(
            merged.last().map(|bar| bar.source_sequence),
            Some(u64::try_from(HISTORY_BARS_PER_SERIES).expect("bound fits"))
        );
    }

    #[test]
    fn coinbase_coverage_keeps_interior_missing_bars_repairable() {
        let series = internal_series(&btc()).expect("series");
        let mut bars = sequential_history(1, 5, 1);
        bars.remove(2);

        let ranges = coinbase_bar_coverage_ranges(&series, &bars).expect("coverage");

        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].start_unix_nanos, 60 * 1_000_000_000);
        assert_eq!(ranges[0].end_unix_nanos, 3 * 60 * 1_000_000_000);
        assert_eq!(ranges[1].start_unix_nanos, 4 * 60 * 1_000_000_000);
        assert_eq!(ranges[1].end_unix_nanos, 6 * 60 * 1_000_000_000);
        let coverage = CoverageSnapshot::try_new(ranges, Vec::new(), Vec::new(), Vec::new())
            .expect("snapshot");
        let requested = HistoryRange {
            start_unix_nanos: 60 * 1_000_000_000,
            end_unix_nanos: 6 * 60 * 1_000_000_000,
        };
        assert_eq!(
            coverage
                .plan(requested)
                .expect("repair plan")
                .repair_ranges(),
            &[HistoryRange {
                start_unix_nanos: 3 * 60 * 1_000_000_000,
                end_unix_nanos: 4 * 60 * 1_000_000_000,
            }]
        );
    }

    #[test]
    fn coinbase_covering_repair_fills_holes_and_preserves_current_overlap() {
        let series = internal_series(&btc()).expect("series");
        let mut current_bars = sequential_history(1, 5, 1);
        current_bars.remove(2);
        let current = SeriesSnapshot {
            series: series.clone(),
            provider_generation: ProviderGeneration(NonZeroU64::MIN),
            publication_generation: 1,
            price_scale: 2,
            quantity_scale: 8,
            forming: true,
            bars: current_bars.into(),
        };
        let mut repair = sequential_history(1, 1, 3);
        repair[0].close = 103;
        let mut overlap = sequential_history(2, 1, 1);
        overlap[0].close = 102;
        repair.extend(overlap);

        let merged = reconcile_history_repair(
            &current,
            repair,
            HISTORY_BARS_PER_SERIES,
            Some(coinbase_series_interval(&series).expect("interval")),
            None,
        )
        .expect("repair merges");

        assert_eq!(merged.len(), 5);
        assert_eq!(merged[1].close, 105);
        assert_eq!(merged[2].close, 103);
        assert!(merged.windows(2).all(|pair| {
            pair[0].source_sequence.checked_add(1) == Some(pair[1].source_sequence)
        }));
    }

    fn trade(minute: i64, price: &str, provider_sequence: u64) -> CanonicalTrade {
        trade_for("BTC-USD", minute, price, provider_sequence)
    }

    fn trade_for(
        product_id: &str,
        minute: i64,
        price: &str,
        provider_sequence: u64,
    ) -> CanonicalTrade {
        CanonicalTrade {
            product_id: product_id.to_string(),
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

    fn assert_persisted_bars(
        request: Result<StorageRequest, mpsc::TryRecvError>,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        local: MarketBar,
        repaired: MarketBar,
    ) {
        let reconciled = MarketBar {
            source_sequence: local.source_sequence + 1,
            ..repaired
        };
        assert!(matches!(
            request,
            Ok(StorageRequest::Persist(persisted, current, bars, false, _, _))
                if persisted == *series && current == generation && bars == [local, reconciled]
        ));
    }

    fn is_live_update(
        event: &envelope::Payload,
        generation: u64,
        provider_generation: u64,
        close: i64,
    ) -> bool {
        matches!(
            event,
            envelope::Payload::SeriesUpdate(update)
                if update.generation == generation
                    && update.provider_generation == provider_generation
                    && update.forming
                    && update.bar.as_ref().is_some_and(|bar| bar.close == close)
        ) || matches!(
            event,
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.generation == generation
                    && snapshot.provider_generation == provider_generation
                    && snapshot.forming
                    && snapshot.bars.last().is_some_and(|bar| bar.close == close)
        )
    }

    fn attach_fixture_consumers(harness: &FixtureRealtimeHarness) {
        for client in 1..=2 {
            harness.service.attach(client).expect("client attaches");
            harness
                .service
                .register_consumer(client, 1, client)
                .expect("consumer registers");
            harness
                .service
                .set_demand(client, client, 1, &btc())
                .expect("history demand is accepted");
            let snapshot = poll_until(&harness.service, client, client, |event| {
                matches!(event, envelope::Payload::SeriesSnapshot(_))
            });
            assert!(matches!(
                snapshot,
                envelope::Payload::SeriesSnapshot(snapshot)
                    if snapshot.provider_generation == 1 && !snapshot.forming
            ));
        }
    }

    fn expect_realtime_generation(
        harness: &FixtureRealtimeHarness,
        detail: &'static str,
        expected: u64,
    ) {
        assert_eq!(
            harness
                .generations
                .recv_timeout(Duration::from_secs(1))
                .expect(detail)
                .0
                .get(),
            expected
        );
    }

    fn expect_configured_products(
        harness: &FixtureRealtimeHarness,
        detail: &'static str,
        expected: &[&str],
    ) {
        assert_eq!(
            harness
                .configured_products
                .recv_timeout(Duration::from_secs(1))
                .expect(detail),
            expected
        );
    }

    fn phase_five_series() -> [SeriesKey; 8] {
        [
            selected_series("instrument:coinbase:btc:usd", 60),
            selected_series("instrument:coinbase:btc:usd", 300),
            selected_series("instrument:coinbase:btc:usd", 900),
            selected_series("instrument:coinbase:btc:usd", 3_600),
            selected_series("instrument:coinbase:eth:usd", 60),
            selected_series("instrument:coinbase:eth:usd", 300),
            selected_series("instrument:coinbase:eth:usd", 900),
            selected_series("instrument:coinbase:eth:usd", 3_600),
        ]
    }

    fn attach_twenty_chart_consumers(
        service: &MarketService,
        client_id: u64,
        series: &[SeriesKey; 8],
    ) {
        service.attach(client_id).expect("client attaches");
        for consumer_id in 1..=20 {
            let workspace_id = (consumer_id - 1) / 4 + 1;
            service
                .register_consumer(client_id, workspace_id, consumer_id)
                .expect("chart consumer registers");
            service
                .set_demand(
                    client_id,
                    consumer_id,
                    1,
                    &series[usize::try_from((consumer_id - 1) % 8).expect("series index")],
                )
                .expect("chart demand is accepted");
        }
    }

    fn assert_initial_chart_snapshots(
        service: &MarketService,
        client_id: u64,
        series: &[SeriesKey; 8],
    ) {
        for consumer_id in 1..=20 {
            let expected = &series[usize::try_from((consumer_id - 1) % 8).expect("series index")];
            assert!(matches!(
                poll_until(service, client_id, consumer_id, |event| matches!(
                    event,
                    envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 1
                )),
                envelope::Payload::SeriesSnapshot(snapshot)
                    if snapshot.consumer_id == consumer_id
                        && snapshot.series.as_ref() == Some(expected)
            ));
        }
    }

    #[test]
    fn twenty_chart_consumers_share_one_provider_and_remain_independent() {
        let harness = MarketService::start_fixture_realtime(vec![history_bar()])
            .expect("realtime fixture starts");
        let client_id = 1;
        let series = phase_five_series();
        attach_twenty_chart_consumers(&harness.service, client_id, &series);
        assert_initial_chart_snapshots(&harness.service, client_id, &series);
        assert_eq!(
            harness.history_fetches.load(Ordering::Acquire),
            series.len()
        );
        assert_eq!(
            harness
                .generations
                .recv_timeout(Duration::from_secs(1))
                .expect("one shared realtime generation starts")
                .0
                .get(),
            1
        );
        assert_eq!(
            harness
                .configured_products
                .recv_timeout(Duration::from_secs(1))
                .expect("initial product set configures"),
            ["BTC-USD"]
        );
        assert_eq!(
            harness
                .generations
                .recv_timeout(Duration::from_secs(1))
                .expect("product-set change reconnects the provider")
                .0
                .get(),
            2
        );
        assert_eq!(
            harness
                .configured_products
                .recv_timeout(Duration::from_secs(1))
                .expect("replacement product set configures"),
            ["BTC-USD", "ETH-USD"]
        );
        harness
            .actions
            .send(FixtureRealtimeAction::Connected)
            .expect("connect fixture");

        harness
            .service
            .set_demand(client_id, 1, 2, &series[7])
            .expect("one chart switches series");
        assert!(matches!(
            poll_until(&harness.service, client_id, 1, |event| matches!(
                event,
                envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2
            )),
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.generation == 2 && snapshot.series.as_ref() == Some(&series[7])
        ));
        harness
            .actions
            .send(FixtureRealtimeAction::Trade(trade(2, "2.00", 1)))
            .expect("shared BTC trade");
        let live = poll_until(&harness.service, client_id, 9, |event| {
            is_live_update(event, 1, 2, 200)
        });
        assert!(is_live_update(&live, 1, 2, 200));

        harness
            .service
            .remove_consumer(client_id, 1)
            .expect("switched chart closes");
        harness
            .actions
            .send(FixtureRealtimeAction::Trade(trade(2, "2.10", 2)))
            .expect("remaining chart trade");
        let live = poll_until(&harness.service, client_id, 9, |event| {
            is_live_update(event, 1, 2, 210)
        });
        assert!(is_live_update(&live, 1, 2, 210));
        assert!(harness.service.poll_event(client_id, 1).is_err());
        assert_eq!(
            harness.history_fetches.load(Ordering::Acquire),
            series.len() * 2
        );
        assert!(matches!(
            harness.generations.try_recv(),
            Err(TryRecvError::Empty)
        ));
    }

    #[test]
    fn realtime_capacity_rejects_a_new_product_but_allows_an_exact_replacement() {
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        let btc_series = internal_series(&btc()).expect("BTC series");
        let mut engine = configured_engine().expect("engine configures");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        engine
            .set_series_demand(
                consumer_id,
                GenerationId(id(1).expect("generation")),
                &btc_series,
            )
            .expect("BTC demand installs");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &btc_series,
        );
        let generation = ProviderGeneration(NonZeroU64::MIN);
        let btc_instrument = coinbase_instrument(&btc_series);
        coordinator.live.insert(
            btc_series.clone(),
            LiveHandoff::try_new(&btc_series, generation, &btc_instrument)
                .expect("BTC live handoff"),
        );
        coordinator
            .realtime_products
            .insert(btc_instrument.provider_symbol);

        for index in 1..axiusflow_coinbase_market_adapter::MAXIMUM_PRODUCTS {
            let (series, instrument) = synthetic_coinbase_product(index);
            coordinator.catalog.insert(
                ("coinbase".to_string(), instrument.instrument_id.clone()),
                instrument.clone(),
            );
            coordinator.live.insert(
                series.clone(),
                LiveHandoff::try_new(&series, generation, &instrument).expect("live handoff"),
            );
            coordinator
                .realtime_products
                .insert(instrument.provider_symbol);
        }
        let (candidate, candidate_instrument) = synthetic_coinbase_product(64);
        coordinator.catalog.insert(
            ("coinbase".to_string(), candidate.instrument_id.clone()),
            candidate_instrument,
        );
        let live_before = coordinator.live.len();
        let products_before = coordinator.realtime_products.clone();

        assert_eq!(
            coordinator
                .validate_coinbase_realtime_capacity(
                    &candidate,
                    ConsumerId(id(2).expect("other consumer")),
                )
                .expect_err("sixty-fifth product is rejected"),
            "Coinbase realtime product capacity is exhausted"
        );
        assert_eq!(coordinator.live.len(), live_before);
        assert_eq!(coordinator.realtime_products, products_before);
        coordinator
            .validate_coinbase_realtime_capacity(&candidate, consumer_id)
            .expect("replacing the sole BTC reference remains at capacity");
    }

    #[test]
    fn shared_realtime_stops_at_last_market_reference_with_idle_consumer() {
        let harness = MarketService::start_fixture_realtime(vec![history_bar()])
            .expect("realtime fixture starts");
        attach_fixture_consumers(&harness);
        harness
            .service
            .register_consumer(2, 1, 3)
            .expect("unrelated idle consumer registers");
        assert_eq!(harness.history_fetches.load(Ordering::Acquire), 1);
        assert_eq!(
            harness
                .generations
                .recv_timeout(Duration::from_secs(1))
                .expect("one shared realtime generation starts")
                .0
                .get(),
            1
        );
        assert_eq!(
            harness
                .configured_products
                .recv_timeout(Duration::from_secs(1))
                .expect("BTC product set configures"),
            ["BTC-USD"]
        );

        harness
            .service
            .remove_consumer(1, 1)
            .expect("first market reference closes");
        assert!(
            harness
                .stops
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "the remaining market reference retains realtime"
        );
        harness
            .service
            .remove_consumer(2, 2)
            .expect("last market reference closes");
        assert_eq!(
            harness
                .stops
                .recv_timeout(Duration::from_secs(1))
                .expect("last market reference releases realtime")
                .0
                .get(),
            1
        );
    }

    #[test]
    fn rithmic_upstream_starts_on_first_demand_and_stops_after_final_release() {
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let (rithmic_control_tx, rithmic_control_rx) = mpsc::sync_channel(2);
        let realtime_stop = Arc::new(AtomicBool::new(false));
        let generation = ProviderGeneration(id(7).expect("provider generation"));
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        let series = rithmic_series_key("instrument:rithmic:CME:MNQU6", "rithmic-test:CME:MNQU6");
        let mut engine = configured_engine().expect("engine configures");
        engine
            .begin_provider_session("rithmic", generation)
            .expect("Rithmic session begins");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        engine
            .set_series_demand_with_streams(
                consumer_id,
                GenerationId(id(1).expect("generation")),
                &series,
                chart_stream_requirements(&series),
            )
            .expect("Rithmic demand installs");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        coordinator.rithmic_realtime_control = Some(&rithmic_control_tx);
        coordinator.catalog.insert(
            (series.provider_id.clone(), series.instrument_id.clone()),
            provider_instrument(7, 1),
        );

        coordinator
            .ensure_realtime(&series)
            .expect("first demand starts Rithmic upstream");
        assert!(matches!(
            rithmic_control_rx.try_recv(),
            Ok(RithmicRealtimeControl::Select(_))
        ));
        assert!(coordinator.engine.remove_consumer(consumer_id));
        coordinator.release_unused_live_market_data();
        assert!(matches!(
            rithmic_control_rx.try_recv(),
            Ok(RithmicRealtimeControl::Stop)
        ));
        assert_eq!(
            coordinator
                .engine
                .provider_status("rithmic")
                .map(|status| status.health),
            Some(ProviderHealth::Disconnected)
        );
    }

    #[test]
    fn slow_consumer_conflates_live_state_without_blocking_control() {
        let harness = MarketService::start_fixture_realtime(vec![history_bar()])
            .expect("realtime fixture starts");
        attach_fixture_consumers(&harness);
        assert_eq!(
            harness
                .generations
                .recv_timeout(Duration::from_secs(1))
                .expect("shared realtime generation starts")
                .0
                .get(),
            1
        );
        harness
            .actions
            .send(FixtureRealtimeAction::Connected)
            .expect("connect fixture");
        assert!(matches!(
            poll_until(&harness.service, 2, 2, |event| matches!(
                event,
                envelope::Payload::ProviderState(state)
                    if state.state == ProviderConnectionState::Online as i32
            )),
            envelope::Payload::ProviderState(state) if state.generation == 1
        ));

        for sequence in 1..=32 {
            let price = if sequence == 32 { "3.00" } else { "2.00" };
            harness
                .actions
                .send(FixtureRealtimeAction::Trade(trade(2, price, sequence)))
                .expect("live trade enters the bounded provider queue");
        }
        assert!(matches!(
            poll_until(&harness.service, 2, 2, |event| {
                is_live_update(event, 1, 1, 300)
            }),
            envelope::Payload::SeriesUpdate(update) if update.provider_generation == 1
        ));

        harness
            .service
            .set_visibility(2, 2, false)
            .expect("control command remains responsive");
        harness
            .service
            .set_demand(2, 2, 2, &btc())
            .expect("new demand remains responsive");
        harness
            .service
            .set_visibility(2, 2, true)
            .expect("foreground restoration remains responsive");
        assert!(matches!(
            poll_until(&harness.service, 2, 2, |event| {
                matches!(
                    event,
                    envelope::Payload::SeriesSnapshot(snapshot)
                        if snapshot.generation == 2
                            && snapshot.bars.last().is_some_and(|bar| bar.close == 300)
                )
            }),
            envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2
        ));

        let mut pending = 0;
        let mut latest_snapshot = false;
        while let Some(event) = harness
            .service
            .poll_event(1, 1)
            .expect("slow consumer polls after the burst")
        {
            pending += 1;
            assert!(pending <= 7, "consumer publication state remains bounded");
            latest_snapshot |= match event {
                envelope::Payload::SeriesSnapshot(snapshot) => {
                    snapshot.bars.last().is_some_and(|bar| bar.close == 300)
                }
                envelope::Payload::SeriesUpdate(update) => {
                    update.forming && update.bar.as_ref().is_some_and(|bar| bar.close == 300)
                }
                _ => false,
            };
        }
        assert!(
            latest_snapshot,
            "slow consumer receives the latest covering state"
        );
    }

    #[test]
    fn storage_failure_degrades_persistence_without_hiding_provider_history() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let service = MarketService::start_with_sources(
            FixtureHistory {
                bars: vec![history_bar()],
                fetches: Some(Arc::clone(&fetches)),
            },
            None,
            Some(Err("fixture storage failure".to_string())),
        )
        .expect("service starts with degraded storage");
        service.attach(1).expect("client attaches");
        service
            .register_consumer(1, 1, 1)
            .expect("consumer registers");
        service
            .set_demand(1, 1, 1, &btc())
            .expect("demand remains usable");
        assert!(matches!(
            poll_until(&service, 1, 1, |event| matches!(
                event,
                envelope::Payload::SeriesSnapshot(_)
            )),
            envelope::Payload::SeriesSnapshot(snapshot) if snapshot.bars.len() == 1
        ));
        assert!(matches!(
            poll_until(&service, 1, 1, |event| matches!(
                event,
                envelope::Payload::SeriesState(state)
                    if state.persistence == PersistenceState::Degraded as i32
            )),
            envelope::Payload::SeriesState(state)
                if state.state == SeriesLoadState::Ready as i32
                    && state.persistence == PersistenceState::Degraded as i32
        ));
        assert!(matches!(
            poll_until(&service, 1, 1, |event| matches!(
                event,
                envelope::Payload::DemandError(_)
            )),
            envelope::Payload::DemandError(error)
                if error.stage_code == FailureStage::FilesystemWrite as i32
                    && error.stage == "filesystem_write"
                    && error.series == Some(btc())
                    && error.elapsed_millis.is_some()
        ));
        service
            .set_demand(1, 1, 2, &btc())
            .expect("degraded persistence does not invalidate memory");
        assert!(matches!(
            poll_until(&service, 1, 1, |event| matches!(
                event,
                envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2
            )),
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.generation == 2 && snapshot.bars[0].close == 105
        ));
        assert_eq!(
            fetches.load(Ordering::Acquire),
            1,
            "usable in-memory history avoids another provider request"
        );
    }

    #[test]
    fn storage_degradation_preserves_provider_and_live_progress() {
        let harness = MarketService::start_fixture_realtime_with_storage(
            vec![history_bar()],
            Some(Err("fixture storage failure".to_string())),
        )
        .expect("realtime service starts with degraded storage");
        harness.service.attach(1).expect("client attaches");
        harness
            .service
            .register_consumer(1, 1, 1)
            .expect("consumer registers");
        harness
            .service
            .set_demand(1, 1, 1, &btc())
            .expect("demand starts independently of storage");
        assert_eq!(
            harness
                .generations
                .recv_timeout(Duration::from_secs(1))
                .expect("realtime generation starts")
                .0
                .get(),
            1
        );
        assert!(matches!(
            poll_until(&harness.service, 1, 1, |event| matches!(
                event,
                envelope::Payload::ProviderState(state)
                    if state.state == ProviderConnectionState::Connecting as i32
            )),
            envelope::Payload::ProviderState(state) if state.generation == 1
        ));
        assert!(matches!(
            poll_until(&harness.service, 1, 1, |event| matches!(
                event,
                envelope::Payload::SeriesState(state)
                    if state.state == SeriesLoadState::Ready as i32
                        && state.persistence == PersistenceState::Degraded as i32
            )),
            envelope::Payload::SeriesState(state)
                if state.state == SeriesLoadState::Ready as i32
                    && state.generation == 1
        ));

        harness
            .actions
            .send(FixtureRealtimeAction::Connected)
            .expect("provider connects after storage degradation");
        assert!(matches!(
            poll_until(&harness.service, 1, 1, |event| matches!(
                event,
                envelope::Payload::ProviderState(state)
                    if state.state == ProviderConnectionState::Online as i32
            )),
            envelope::Payload::ProviderState(state) if state.generation == 1
        ));
        assert!(matches!(
            poll_until(&harness.service, 1, 1, |event| matches!(
                event,
                envelope::Payload::SeriesState(state)
                    if state.state == SeriesLoadState::Live as i32
            )),
            envelope::Payload::SeriesState(state)
                if state.persistence == PersistenceState::NotRequested as i32
                    && state.generation == 1
        ));
        harness
            .actions
            .send(FixtureRealtimeAction::Trade(trade(2, "2.00", 1)))
            .expect("live trade follows storage degradation");
        assert!(matches!(
            poll_until(&harness.service, 1, 1, |event| {
                is_live_update(event, 1, 1, 200)
            }),
            envelope::Payload::SeriesUpdate(update)
                if update.provider_generation == 1 && update.generation == 1
        ));
    }

    #[test]
    fn stalled_local_history_degrades_and_starts_provider_repair() {
        let (history_tx, history_rx) = mpsc::sync_channel(1);
        let (storage_tx, storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let mut engine = configured_engine().expect("engine configures");
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        let series = internal_series(&btc()).expect("series");
        let generation = ProviderGeneration(NonZeroU64::MIN);
        engine
            .set_series_demand(
                consumer_id,
                GenerationId(id(1).expect("generation")),
                &series,
            )
            .expect("demand installs");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        coordinator
            .enqueue_local_history(&series, generation)
            .expect("local history read starts");
        assert!(matches!(
            storage_rx.try_recv(),
            Ok(StorageRequest::Read(ref requested, current))
                if requested == &series && current == generation
        ));
        coordinator
            .local_history_deadlines
            .insert((series.clone(), generation), Instant::now());
        coordinator.expire_local_history_reads();
        assert!(matches!(
            coordinator.events[&consumer_id].series_state,
            Some(envelope::Payload::SeriesState(ref state))
                if state.state == SeriesLoadState::Resolving as i32
                    && state.persistence == PersistenceState::Degraded as i32
                    && state.detail.as_deref()
                        == Some("Local history read timed out; provider repair continues")
        ));
        assert!(matches!(
            history_rx.try_recv(),
            Ok(HistoryRequest {
                ref series,
                provider_generation,
                ..
            }) if series == &internal_series(&btc()).expect("requested series")
                && provider_generation == generation
        ));

        coordinator.local_history_completed(
            &series,
            generation,
            Ok(Some(StoredHistory {
                bars: vec![history_bar()],
                derived: false,
                durable: true,
            })),
        );
        assert!(matches!(
            coordinator.events[&consumer_id].series_state,
            Some(envelope::Payload::SeriesState(ref state))
                if state.state == SeriesLoadState::Partial as i32
                    && state.persistence == PersistenceState::Durable as i32
        ));
        assert!(coordinator.events[&consumer_id].snapshot.is_some());
    }

    #[test]
    fn late_local_history_cannot_replace_completed_provider_repair() {
        let (history_tx, history_rx) = mpsc::sync_channel(1);
        let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let mut engine = configured_engine().expect("engine configures");
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        let series = internal_series(&btc()).expect("series");
        let generation = ProviderGeneration(NonZeroU64::MIN);
        engine
            .set_series_demand(
                consumer_id,
                GenerationId(id(1).expect("generation")),
                &series,
            )
            .expect("demand installs");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        coordinator.history_completed(
            &series,
            generation,
            None,
            HistoryRequestKind::Initial,
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 8,
                bars: vec![history_bar()],
                handoff_boundary_unix_nanos: None,
                confirmed_empty: false,
            }),
        );
        let late = MarketBar {
            close: 99,
            ..history_bar()
        };
        coordinator.local_history_completed(
            &series,
            generation,
            Ok(Some(StoredHistory {
                bars: vec![late],
                derived: false,
                durable: true,
            })),
        );
        assert!(matches!(
            coordinator.events[&consumer_id].snapshot,
            Some(envelope::Payload::SeriesSnapshot(ref snapshot))
                if snapshot.bars[0].close == 105
        ));
        assert!(
            history_rx.try_recv().is_err(),
            "a late local completion must not schedule duplicate provider work"
        );
    }

    #[test]
    fn retained_history_publishes_before_provider_repair_and_survives_its_failure() {
        let (history_tx, history_rx) = mpsc::sync_channel(1);
        let (storage_tx, storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let mut engine = configured_engine().expect("engine configures");
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        let series = internal_series(&btc()).expect("series");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        engine
            .set_series_demand(
                consumer_id,
                GenerationId(id(1).expect("generation")),
                &series,
            )
            .expect("demand installs");
        let generation = ProviderGeneration(NonZeroU64::MIN);
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        let local = MarketBar {
            close: 99,
            ..history_bar()
        };
        coordinator.local_history_completed(
            &series,
            generation,
            Ok(Some(StoredHistory {
                bars: vec![local],
                derived: false,
                durable: true,
            })),
        );
        assert!(matches!(
            coordinator.events[&consumer_id].snapshot,
            Some(envelope::Payload::SeriesSnapshot(ref snapshot))
                if snapshot.bars[0].close == 99
        ));
        assert!(history_rx.try_recv().is_ok());
        coordinator.history_completed(
            &series,
            generation,
            None,
            HistoryRequestKind::Initial,
            Err("provider unavailable".to_string()),
        );
        assert!(matches!(
            coordinator.events[&consumer_id].series_state,
            Some(envelope::Payload::SeriesState(ref state))
                if state.state == SeriesLoadState::Partial as i32
                    && state.persistence == PersistenceState::Durable as i32
        ));
        assert!(coordinator.events[&consumer_id].snapshot.is_some());
        coordinator.history_completed(
            &series,
            generation,
            None,
            HistoryRequestKind::Initial,
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 8,
                bars: vec![history_bar()],
                handoff_boundary_unix_nanos: None,
                confirmed_empty: false,
            }),
        );
        assert!(matches!(
            coordinator.events[&consumer_id].series_state,
            Some(envelope::Payload::SeriesState(ref state))
                if state.state == SeriesLoadState::Ready as i32
                    && state.persistence == PersistenceState::Pending as i32
        ));
        assert!(matches!(
            coordinator.events[&consumer_id].snapshot,
            Some(envelope::Payload::SeriesSnapshot(ref snapshot))
                if snapshot.bars[0].close == 105
        ));
        assert!(matches!(
            storage_rx.try_recv(),
            Ok(StorageRequest::Persist(ref persisted, current, ref bars, false, _, _))
                if persisted == &series && current == generation && bars == &[history_bar()]
        ));
    }

    #[test]
    fn shorter_provider_repair_preserves_the_published_canonical_sequence() {
        let (history_tx, history_rx) = mpsc::sync_channel(1);
        let (storage_tx, storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let mut engine = configured_engine().expect("engine configures");
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        let series = internal_series(&btc()).expect("series");
        engine
            .set_series_demand(
                consumer_id,
                GenerationId(id(1).expect("generation")),
                &series,
            )
            .expect("demand installs");
        let generation = ProviderGeneration(NonZeroU64::MIN);
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        coordinator.local_history_completed(
            &series,
            generation,
            Ok(Some(StoredHistory {
                bars: sequential_history(1, 350, 1),
                derived: false,
                durable: true,
            })),
        );
        history_rx.recv().expect("provider repair is queued");

        coordinator.history_completed(
            &series,
            generation,
            None,
            HistoryRequestKind::Initial,
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 8,
                bars: sequential_history(1, 250, 102),
                handoff_boundary_unix_nanos: None,
                confirmed_empty: false,
            }),
        );

        let Some(envelope::Payload::SeriesSnapshot(snapshot)) =
            coordinator.events[&consumer_id].snapshot.as_ref()
        else {
            panic!("covering repair publishes a snapshot");
        };
        assert_eq!(snapshot.bars.len(), 351);
        assert_eq!(snapshot.bars[0].source_sequence, 1);
        assert_eq!(snapshot.bars[0].exchange_timestamp_seconds, 60);
        assert_eq!(snapshot.bars[350].source_sequence, 351);
        assert_eq!(snapshot.bars[350].exchange_timestamp_seconds, 21_060);
        assert!(matches!(
            storage_rx.try_recv(),
            Ok(StorageRequest::Persist(_, _, ref bars, false, _, _))
                if bars.first().is_some_and(|bar| bar.source_sequence == 1)
                    && bars.last().is_some_and(|bar| bar.source_sequence == 351)
        ));
    }

    #[test]
    fn retained_rithmic_history_uses_installed_precision_and_is_persisted_after_repair() {
        let (history_tx, history_rx) = mpsc::sync_channel(1);
        let (storage_tx, storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let mut engine = configured_engine().expect("engine configures");
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        let generation = ProviderGeneration(id(7).expect("provider generation"));
        engine
            .begin_provider_session("rithmic", generation)
            .expect("Rithmic generation begins");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        let series = BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            period: BarPeriod::tick(100).expect("tick cadence"),
            definition_version: 1,
        };
        engine
            .set_series_demand(
                consumer_id,
                GenerationId(id(1).expect("generation")),
                &series,
            )
            .expect("demand installs");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        let installed = provider_instrument(7, 1);
        coordinator.catalog.insert(
            (installed.provider.clone(), installed.instrument_id.clone()),
            installed,
        );
        let local = MarketBar {
            exchange_timestamp_unix_nanos: 60_123_456_789,
            close: 99,
            ..history_bar()
        };
        coordinator.local_history_completed(
            &series,
            generation,
            Ok(Some(StoredHistory {
                bars: vec![local],
                derived: false,
                durable: true,
            })),
        );
        assert!(matches!(
            coordinator.events[&consumer_id].snapshot,
            Some(envelope::Payload::SeriesSnapshot(ref snapshot))
                if snapshot.provider_generation == 7
                    && snapshot.price_scale == 2
                    && snapshot.quantity_scale == 0
                    && snapshot.bars[0].exchange_timestamp_unix_nanos == 60_123_456_789
        ));
        assert!(history_rx.try_recv().is_ok());

        let repaired = MarketBar {
            exchange_timestamp_seconds: 61,
            exchange_timestamp_unix_nanos: 61_987_654_321,
            ..history_bar()
        };
        coordinator.history_completed(
            &series,
            generation,
            None,
            HistoryRequestKind::Initial,
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 0,
                bars: vec![repaired],
                handoff_boundary_unix_nanos: None,
                confirmed_empty: false,
            }),
        );
        assert_persisted_bars(storage_rx.try_recv(), &series, generation, local, repaired);
        assert!(matches!(
            coordinator.events[&consumer_id].series_state,
            Some(envelope::Payload::SeriesState(ref state))
                if state.state == SeriesLoadState::Ready as i32
                    && state.persistence == PersistenceState::Pending as i32
        ));
    }

    #[test]
    fn provider_state_and_live_readiness_are_scoped_to_matching_consumers() {
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(false));
        let mut engine = configured_engine().expect("engine configures");
        let rithmic_generation = ProviderGeneration(id(7).expect("Rithmic generation"));
        engine
            .begin_provider_session("rithmic", rithmic_generation)
            .expect("Rithmic session begins");
        let coinbase_consumer = ConsumerId(id(1).expect("Coinbase consumer"));
        let rithmic_consumer = ConsumerId(id(2).expect("Rithmic consumer"));
        let rithmic = rithmic_series_key("instrument:rithmic:CME:MNQU6", "rithmic-test:CME:MNQU6");
        for (consumer_id, series) in [
            (coinbase_consumer, internal_series(&btc()).expect("BTC")),
            (rithmic_consumer, rithmic.clone()),
        ] {
            engine
                .register_consumer(
                    ConsumerIdentity {
                        client_id: ClientId(id(1).expect("client")),
                        workspace_id: WorkspaceId(id(1).expect("workspace")),
                        consumer_id,
                    },
                    true,
                )
                .expect("consumer registers");
            engine
                .set_series_demand(
                    consumer_id,
                    GenerationId(id(1).expect("generation")),
                    &series,
                )
                .expect("series demand installs");
        }
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            coinbase_consumer,
            &internal_series(&btc()).expect("BTC series"),
        );
        coordinator
            .events
            .insert(rithmic_consumer, ConsumerEvents::default());

        coordinator.broadcast_provider(
            ProviderConnectionState::Recovering,
            ProviderGeneration(NonZeroU64::MIN),
            Some("Coinbase fixture recovery"),
        );
        coordinator.broadcast_series_state(SeriesLoadState::Live);
        coordinator.broadcast_rithmic_provider(
            ProviderConnectionState::Online,
            rithmic_generation,
            None,
        );

        assert!(matches!(
            coordinator.events[&coinbase_consumer].provider,
            Some(envelope::Payload::ProviderState(ref state))
                if state.provider == "coinbase"
                    && state.state == ProviderConnectionState::Recovering as i32
        ));
        assert!(matches!(
            coordinator.events[&coinbase_consumer].series_state,
            Some(envelope::Payload::SeriesState(ref state))
                if state.state == SeriesLoadState::Live as i32
                    && state.series.as_ref().is_some_and(|series| series.provider == "coinbase")
        ));
        assert!(matches!(
            coordinator.events[&rithmic_consumer].provider,
            Some(envelope::Payload::ProviderState(ref state))
                if state.provider == "rithmic"
                    && state.state == ProviderConnectionState::Online as i32
        ));
        assert!(
            coordinator.events[&rithmic_consumer].series_state.is_none(),
            "Coinbase readiness must not mutate a Rithmic series"
        );
    }

    #[test]
    fn rithmic_reconnect_retains_covering_history_until_repair_replaces_it() {
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        let (storage_tx, storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(false));
        let mut engine = configured_engine().expect("engine configures");
        let consumer_id = ConsumerId(id(1).expect("consumer"));
        let generation = ProviderGeneration(id(7).expect("provider generation"));
        let series = rithmic_series_key("instrument:rithmic:CME:MNQU6", "rithmic-test:CME:MNQU6");
        engine
            .begin_provider_session("rithmic", generation)
            .expect("Rithmic session begins");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        engine
            .set_series_demand(
                consumer_id,
                GenerationId(id(1).expect("generation")),
                &series,
            )
            .expect("demand installs");
        let publication = engine
            .install_history(generation, &series, 2, 0, vec![history_bar()])
            .expect("history installs")
            .pop()
            .expect("consumer receives history");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        publish_state(
            coordinator
                .events
                .get_mut(&consumer_id)
                .expect("consumer events"),
            &publication,
            SeriesLoadState::Live,
            PersistenceState::Durable,
            None,
        );
        let mut live = RithmicLiveHandoff::new(&series, generation, "CME").expect("live cadence");
        live.seed(2, 0, &[history_bar()], None)
            .expect("history seeds live handoff");
        live.connected = true;
        coordinator.rithmic_live.insert(series.clone(), live);

        coordinator.rithmic_connecting(8);

        assert!(matches!(
            coordinator.engine.series_snapshot(&series),
            Some(snapshot) if snapshot.provider_generation == generation
        ));
        assert!(matches!(
            coordinator.events[&consumer_id].snapshot,
            Some(envelope::Payload::SeriesSnapshot(ref snapshot))
                if snapshot.provider_generation == generation.0.get()
        ));
        assert!(matches!(
            coordinator.events[&consumer_id].series_state,
            Some(envelope::Payload::SeriesState(ref state))
                if state.state == SeriesLoadState::Partial as i32
                    && state.persistence == PersistenceState::Durable as i32
        ));
        assert!(matches!(
            storage_rx.try_recv(),
            Ok(StorageRequest::Read(ref requested, current))
                if requested == &series && current.0.get() == 8
        ));
    }

    fn assert_order_flow_snapshot(coordinator: &Coordinator<'_>, consumer_id: ConsumerId) {
        assert!(matches!(
            coordinator.events[&consumer_id].order_flow,
            Some(envelope::Payload::OrderFlowSnapshot(ref snapshot))
                if snapshot.consumer_id == consumer_id.0.get()
                    && snapshot.generation == 1
                    && snapshot.provider_generation == 7
                    && snapshot.source_watermark == 1
                    && snapshot.cumulative_delta == 2
                    && snapshot.levels.len() == 1
                    && snapshot.tape.len() == 1
        ));
    }

    fn assert_order_flow_update(coordinator: &Coordinator<'_>, consumer_id: ConsumerId) {
        assert!(matches!(
            coordinator.events[&consumer_id].order_flow,
            Some(envelope::Payload::OrderFlowUpdate(ref update))
                if update.consumer_id == consumer_id.0.get()
                    && update.provider_generation == 7
                    && update.publication_generation == 2
                    && update.cumulative_delta == 4
                    && update.trade.as_ref().is_some_and(|trade| trade.source_sequence == 2)
        ));
    }

    fn assert_rithmic_failure_is_scoped(
        coordinator: &Coordinator<'_>,
        affected: &BarSeriesKey,
        unaffected: &BarSeriesKey,
        affected_consumer: ConsumerId,
        unaffected_consumer: ConsumerId,
    ) {
        assert!(!coordinator.rithmic_live[affected].history_ready);
        assert!(coordinator.rithmic_live[unaffected].history_ready);
        assert_eq!(
            coordinator
                .engine
                .provider_status("rithmic")
                .map(|status| status.health),
            Some(ProviderHealth::Online),
            "instrument-local failure does not degrade the provider session"
        );
        assert!(matches!(
            coordinator.events[&affected_consumer].series_state,
            Some(envelope::Payload::SeriesState(ref state))
                if state.state == SeriesLoadState::Partial as i32
        ));
        assert!(matches!(
            coordinator.events[&unaffected_consumer].series_state,
            Some(envelope::Payload::SeriesState(ref state))
                if state.state == SeriesLoadState::Live as i32
        ));
    }

    #[test]
    fn rithmic_instrument_failure_repairs_only_the_affected_series() {
        let (history_tx, history_rx) = mpsc::sync_channel(2);
        let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(false));
        let mut engine = configured_engine().expect("engine configures");
        let generation = ProviderGeneration(id(7).expect("provider generation"));
        engine
            .begin_provider_session("rithmic", generation)
            .expect("Rithmic session begins");
        engine
            .set_provider_health("rithmic", generation, ProviderHealth::Online)
            .expect("Rithmic is online");
        let affected = rithmic_series_key("instrument:rithmic:CME:MNQU6", "rithmic-test:CME:MNQU6");
        let unaffected = rithmic_series_key("instrument:rithmic:CME:ESU6", "rithmic-test:CME:ESU6");
        let publications = [
            install_rithmic_test_series(&mut engine, 1, generation, &affected),
            install_rithmic_test_series(&mut engine, 2, generation, &unaffected),
        ];
        let affected_consumer = ConsumerId(id(1).expect("affected consumer"));
        let unaffected_consumer = ConsumerId(id(2).expect("unaffected consumer"));
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            affected_consumer,
            &affected,
        );
        coordinator
            .events
            .insert(unaffected_consumer, ConsumerEvents::default());
        for publication in &publications {
            publish_state(
                coordinator
                    .events
                    .get_mut(&publication.consumer_id)
                    .expect("publication consumer exists"),
                publication,
                SeriesLoadState::Live,
                PersistenceState::Durable,
                None,
            );
        }
        for series in [&affected, &unaffected] {
            coordinator
                .rithmic_live
                .insert(series.clone(), seeded_rithmic_live(series, generation));
        }
        let first = rithmic_trade(1, 7, 121_000_000_000, 110);
        coordinator.rithmic_trade(7, &first);
        assert_order_flow_snapshot(&coordinator, affected_consumer);
        coordinator.publish_rithmic_live();
        coordinator
            .rithmic_live
            .get_mut(&affected)
            .expect("affected handoff")
            .bars
            .last_mut()
            .expect("forming bar")
            .volume = i64::MAX;

        coordinator.rithmic_trade(7, &rithmic_trade(2, 7, 125_000_000_000, 111));
        assert_order_flow_update(&coordinator, affected_consumer);
        assert_rithmic_failure_is_scoped(
            &coordinator,
            &affected,
            &unaffected,
            affected_consumer,
            unaffected_consumer,
        );
        assert!(matches!(
            history_rx.try_recv(),
            Ok(HistoryRequest { series, provider_generation, .. })
                if series == affected && provider_generation == generation
        ));
        assert!(matches!(history_rx.try_recv(), Err(TryRecvError::Empty)));
        complete_rithmic_test_history(&mut coordinator, &affected, generation);
        assert!(
            coordinator.rithmic_live[&affected].history_ready
                && coordinator.rithmic_live[&unaffected].history_ready
        );
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
        service
            .set_demand(1, 1, 1, &btc())
            .expect("first demand is accepted without provider completion");
        while fetches.load(Ordering::Acquire) == 0 {
            thread::yield_now();
        }
        service
            .set_demand(2, 2, 1, &btc())
            .expect("matching demand joins the in-flight history request");
        release_tx.send(()).expect("history released");
        for (client, consumer) in [(1, 1), (2, 2)] {
            assert!(matches!(
                poll_until(&service, client, consumer, |event| matches!(
                    event,
                    envelope::Payload::SeriesSnapshot(_)
                )),
                envelope::Payload::SeriesSnapshot(snapshot) if snapshot.bars.len() == 1
            ));
        }
        service
            .set_demand(1, 1, 2, &btc())
            .expect("newer selection reuses cache");
        assert!(matches!(
            poll_until(&service, 1, 1, |event| matches!(
                event,
                envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2
            )),
            envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2
        ));
        service
            .set_viewport(1, 1, 1, 60, 120)
            .expect("retired viewport is a fenced no-op");
        assert_eq!(fetches.load(Ordering::Acquire), 1);
    }

    #[test]
    fn compatible_minute_history_publishes_and_caches_a_coarser_series() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = mpsc::sync_channel(2);
        let service = MarketService::start_with_source(ControlledHistory {
            fetches: Arc::clone(&fetches),
            release: release_rx,
        })
        .expect("test service starts");
        service.attach(1).expect("client attaches");
        service
            .register_consumer(1, 1, 1)
            .expect("consumer registers");
        service
            .set_demand(1, 1, 1, &btc())
            .expect("minute demand starts");
        while fetches.load(Ordering::Acquire) == 0 {
            thread::yield_now();
        }
        release_tx.send(()).expect("minute history released");
        poll_until(
            &service,
            1,
            1,
            |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 1),
        );

        let five_minute = selected_series("instrument:coinbase:btc:usd", 300);
        service
            .set_demand(1, 1, 2, &five_minute)
            .expect("coarser demand starts");
        assert!(matches!(
            poll_until(&service, 1, 1, |event| matches!(
                event,
                envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2
            )),
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.series.as_ref() == Some(&five_minute)
                    && snapshot.bars.len() == 1
                    && snapshot.bars[0].exchange_timestamp_seconds == 0
        ));
        while fetches.load(Ordering::Acquire) < 2 {
            thread::yield_now();
        }
        release_tx.send(()).expect("provider repair released");
        poll_until(&service, 1, 1, |event| {
            matches!(event, envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.generation == 2
                    && snapshot.bars.len() == 2
                    && snapshot.bars[0].exchange_timestamp_seconds == 0
                    && snapshot.bars[1].exchange_timestamp_seconds == 300
                    && snapshot.bars[1].source_sequence == 2)
        });

        service
            .set_demand(1, 1, 3, &five_minute)
            .expect("repeated demand hits cache");
        assert!(matches!(
            poll_until(&service, 1, 1, |event| matches!(
                event,
                envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 3
            )),
            envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 3
        ));
        assert_eq!(fetches.load(Ordering::Acquire), 2);
    }

    #[test]
    fn finer_history_never_derives_from_a_coarser_cached_series() {
        let (requested_tx, requested_rx) = mpsc::sync_channel(2);
        let (release_tx, release_rx) = mpsc::sync_channel(2);
        let service = MarketService::start_with_source(SwitchingHistory {
            requested: requested_tx,
            release: release_rx,
        })
        .expect("directional history service starts");
        service.attach(1).expect("client attaches");
        service
            .register_consumer(1, 1, 1)
            .expect("consumer registers");
        let five_minute = selected_series("instrument:coinbase:btc:usd", 300);
        service
            .set_demand(1, 1, 1, &five_minute)
            .expect("coarse history demand starts");
        assert_eq!(
            requested_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("coarse provider request starts"),
            internal_series(&five_minute).expect("coarse series")
        );
        release_tx.send(()).expect("coarse history completes");
        poll_until(
            &service,
            1,
            1,
            |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 1),
        );

        service
            .set_demand(1, 1, 2, &btc())
            .expect("finer history demand starts");
        assert_eq!(
            requested_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("finer demand reaches the provider"),
            internal_series(&btc()).expect("finer series")
        );
        while let Some(event) = service.poll_event(1, 1).expect("pending event polls") {
            assert!(
                !matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2),
                "coarser cached data must not synthesize a finer snapshot"
            );
        }
        release_tx.send(()).expect("finer history completes");
        assert!(matches!(
            poll_until(&service, 1, 1, |event| matches!(
                event,
                envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2
            )),
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.series == Some(btc()) && snapshot.generation == 2
        ));
    }

    #[test]
    fn provider_history_failure_resolves_to_explicit_terminal_state() {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let service = MarketService::start_with_source(ControlledHistoryFailure {
            started: started_tx,
            release: release_rx,
        })
        .expect("failing history service starts");
        service.attach(1).expect("client attaches");
        service
            .register_consumer(1, 1, 1)
            .expect("consumer registers");
        service
            .set_demand(1, 1, 1, &btc())
            .expect("history demand is accepted asynchronously");
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("provider history starts");
        assert!(matches!(
            poll_until(&service, 1, 1, |event| matches!(
                event,
                envelope::Payload::SeriesState(state)
                    if state.state == SeriesLoadState::Resolving as i32
            )),
            envelope::Payload::SeriesState(state)
                if state.generation == 1
                    && state.series == Some(btc())
                    && state.persistence == PersistenceState::NotRequested as i32
        ));
        release_tx.send(()).expect("provider failure released");
        assert!(matches!(
            poll_until(&service, 1, 1, |event| matches!(
                event,
                envelope::Payload::SeriesState(state)
                    if state.state == SeriesLoadState::Failed as i32
            )),
            envelope::Payload::SeriesState(state)
                if state.generation == 1
                    && state.detail.as_deref() == Some("Coinbase historical bars are unavailable")
        ));
        assert!(matches!(
            poll_until(&service, 1, 1, |event| matches!(
                event,
                envelope::Payload::DemandError(_)
            )),
            envelope::Payload::DemandError(error)
                if error.generation == 1
                    && error.stage == "provider_history"
                    && error.code == EngineFaultCode::Retryable as i32
                    && error.stage_code == FailureStage::ProviderHistory as i32
                    && error.series == Some(btc())
                    && error.cause == "provider history did not produce usable canonical bars"
                    && error.elapsed_millis.is_some()
        ));
    }

    #[test]
    fn demand_failures_preserve_series_stage_cause_and_elapsed_time_without_raw_payloads() {
        let consumer_id = ConsumerId(id(7).expect("consumer"));
        let generation = GenerationId(id(9).expect("generation"));
        let series = internal_series(&btc()).expect("series");
        let mut events = BTreeMap::from([(consumer_id, ConsumerEvents::default())]);
        fail_waiters(
            &mut events,
            vec![DemandWaiter {
                consumer_id,
                generation,
                started_at: Instant::now()
                    .checked_sub(Duration::from_millis(5))
                    .expect("five milliseconds is representable"),
            }],
            &series,
            FailureStage::Handoff,
            "history/live handoff failed",
        );
        let envelope::Payload::DemandError(error) = events
            .get_mut(&consumer_id)
            .and_then(ConsumerEvents::pop)
            .and_then(|_| events.get_mut(&consumer_id).and_then(ConsumerEvents::pop))
            .expect("structured failure follows terminal state")
        else {
            panic!("expected structured demand failure");
        };
        assert_eq!(error.consumer_id, 7);
        assert_eq!(error.generation, 9);
        assert_eq!(error.series, Some(btc()));
        assert_eq!(error.stage, "handoff");
        assert_eq!(error.stage_code, FailureStage::Handoff as i32);
        assert_eq!(
            error.cause,
            "history and realtime state could not be joined safely"
        );
        assert!(error.elapsed_millis.is_some_and(|elapsed| elapsed >= 5));
        assert!(!error.detail.contains("token"));
    }

    #[test]
    fn full_history_queue_fails_waiters_without_blocking_the_coordinator() {
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        history_tx
            .try_send(HistoryRequest {
                series: internal_series(&btc()).expect("first series"),
                provider_generation: ProviderGeneration(id(1).expect("provider generation")),
                instrument: None,
                maximum_bars: HISTORY_BARS_PER_SERIES,
                range: None,
                kind: HistoryRequestKind::Initial,
                stop: Arc::new(AtomicBool::new(false)),
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
                    instrument: None,
                    maximum_bars: HISTORY_BARS_PER_SERIES,
                    range: None,
                    kind: HistoryRequestKind::Initial,
                    stop: Arc::new(AtomicBool::new(false)),
                },
            ),
            Err("provider history capacity is temporarily exhausted")
        );
    }

    #[test]
    fn rapid_switch_churn_publishes_only_the_latest_generation() {
        let (requested_tx, requested_rx) = mpsc::sync_channel(8);
        let (release_tx, release_rx) = mpsc::sync_channel(8);
        let service = MarketService::start_with_source(SwitchingHistory {
            requested: requested_tx,
            release: release_rx,
        })
        .expect("switching service starts");
        service.attach(1).expect("client attaches");
        service
            .register_consumer(1, 1, 1)
            .expect("consumer registers");
        let required = [
            selected_series("instrument:coinbase:btc:usd", 60),
            selected_series("instrument:coinbase:btc:usd", 300),
            selected_series("instrument:coinbase:btc:usd", 900),
            selected_series("instrument:coinbase:btc:usd", 3_600),
            selected_series("instrument:coinbase:btc:usd", 60),
            selected_series("instrument:coinbase:eth:usd", 60),
            selected_series("instrument:coinbase:btc:usd", 60),
        ];
        for (index, series) in required.iter().enumerate() {
            service
                .set_demand(1, 1, u64::try_from(index + 1).expect("generation"), series)
                .expect("rapid demand is accepted");
        }
        let latest_series = internal_series(required.last().expect("latest demand"))
            .expect("latest internal series");
        loop {
            let requested = requested_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("latest history request starts");
            release_tx.send(()).expect("observed history completes");
            if requested == latest_series {
                break;
            }
        }
        let latest = poll_until(
            &service,
            1,
            1,
            |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 7),
        );
        assert!(matches!(
            latest,
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.generation == 7
                    && snapshot.series.as_ref().is_some_and(|series| {
                        series.instrument_id == "instrument:coinbase:btc:usd"
                            && series.cadence_value == 60
                    })
        ));

        for _ in 0..4 {
            release_tx
                .send(())
                .expect("any queued obsolete history completes");
        }
        let deadline = Instant::now() + Duration::from_millis(100);
        while Instant::now() < deadline {
            while let Some(event) = service.poll_event(1, 1).expect("final consumer polls") {
                assert!(
                    !matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation != 7),
                    "obsolete history reached the active consumer"
                );
            }
            thread::yield_now();
        }
    }

    #[test]
    fn newer_demand_cancels_history_without_waiting_for_cleanup() {
        let (started_tx, started_rx) = mpsc::sync_channel(2);
        let (cancellation_tx, cancellation_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let service = MarketService::start_with_source(DelayedCancellationHistory {
            started: started_tx,
            cancellation_observed: cancellation_tx,
            release_cancellation: release_rx,
            block_first: true,
        })
        .expect("cancellation fixture starts");
        service.attach(1).expect("client attaches");
        service
            .register_consumer(1, 1, 1)
            .expect("consumer registers");
        service
            .set_demand(1, 1, 1, &btc())
            .expect("first history demand starts");
        assert_eq!(
            started_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("first history fetch starts"),
            (
                internal_series(&btc()).expect("first internal series"),
                ProviderGeneration(NonZeroU64::MIN),
            )
        );

        let eth = selected_series("instrument:coinbase:eth:usd", 60);
        let switched_service = service.clone();
        let (switched_tx, switched_rx) = mpsc::sync_channel(1);
        let switcher = thread::spawn(move || {
            let result = switched_service.set_demand(1, 1, 2, &eth);
            let _ = switched_tx.send(result);
        });
        cancellation_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("obsolete history observes cancellation");
        match switched_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Ok(())) => {}
            result => {
                let _ = release_tx.send(());
                let _ = switcher.join();
                panic!("new demand did not supersede old cleanup: {result:?}");
            }
        }
        release_tx
            .send(())
            .expect("obsolete history cleanup completes");
        switcher.join().expect("demand switch thread joins");
        assert_eq!(
            started_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("new history fetch starts"),
            (
                internal_series(&selected_series("instrument:coinbase:eth:usd", 60))
                    .expect("new internal series"),
                ProviderGeneration(NonZeroU64::MIN),
            )
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(event) = service.poll_event(1, 1).expect("consumer polls") {
                assert!(
                    !matches!(&event, envelope::Payload::ProviderState(state)
                        if state.state == ProviderConnectionState::Recovering as i32),
                    "intentional cancellation must not report provider recovery"
                );
                if matches!(&event, envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.generation == 2
                    && snapshot.series.as_ref().is_some_and(|series| {
                        series.instrument_id == "instrument:coinbase:eth:usd"
                    }))
                {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "new history publication timed out"
            );
            thread::yield_now();
        }
    }

    #[test]
    fn coinbase_reconnect_cancels_blocked_obsolete_history() {
        let (started_tx, started_rx) = mpsc::sync_channel(2);
        let (cancellation_tx, cancellation_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let (action_tx, action_rx) = mpsc::sync_channel(1);
        let (realtime_generation_tx, realtime_generation_rx) = mpsc::sync_channel(2);
        let (realtime_stop_tx, _realtime_stop_rx) = mpsc::sync_channel(2);
        let service = MarketService::start_with_sources(
            DelayedCancellationHistory {
                started: started_tx,
                cancellation_observed: cancellation_tx,
                release_cancellation: release_rx,
                block_first: true,
            },
            Some(Box::new(FixtureRealtime {
                actions: action_rx,
                generations: realtime_generation_tx,
                stops: realtime_stop_tx,
                configured_products: None,
            })),
            None,
        )
        .expect("reconnect cancellation fixture starts");
        service.attach(1).expect("client attaches");
        service
            .register_consumer(1, 1, 1)
            .expect("consumer registers");
        service
            .set_demand(1, 1, 1, &btc())
            .expect("history demand starts");
        let series = internal_series(&btc()).expect("internal series");
        assert_eq!(
            started_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("generation-one history blocks"),
            (series.clone(), ProviderGeneration(NonZeroU64::MIN))
        );
        assert_eq!(
            realtime_generation_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("generation-one realtime starts")
                .0
                .get(),
            1
        );

        action_tx
            .send(FixtureRealtimeAction::Disconnect)
            .expect("realtime disconnects");
        assert_eq!(
            realtime_generation_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("realtime reconnects")
                .0
                .get(),
            2
        );
        cancellation_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("obsolete history observes generation cancellation");
        release_tx
            .send(())
            .expect("obsolete history cleanup completes");
        assert_eq!(
            started_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("current history enters the freed lane"),
            (
                series,
                ProviderGeneration(NonZeroU64::new(2).expect("generation")),
            )
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
        let first_live = poll_until(&harness.service, 1, 1, |event| {
            is_live_update(event, 1, 1, 200)
        });
        assert!(matches!(
            first_live,
            envelope::Payload::SeriesUpdate(update)
                if update.bar.as_ref().is_some_and(|bar| bar.source_sequence == 3 && bar.close == 200)
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
            envelope::Payload::ProviderState(state)
                if state.provider == "coinbase" && (1..=2).contains(&state.generation)
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
            let resumed = poll_until(&harness.service, client, consumer, |event| {
                is_live_update(event, 1, 2, 210)
            });
            assert!(is_live_update(&resumed, 1, 2, 210));
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
    fn markets_live_retains_and_advances_the_hot_series_without_ui_consumers() {
        let harness = MarketService::start_fixture_realtime(vec![history_bar()])
            .expect("realtime fixture starts");
        harness.service.attach(1).expect("client attaches");
        harness
            .service
            .register_consumer(1, 1, 1)
            .expect("consumer registers");
        harness
            .service
            .set_demand(1, 1, 1, &btc())
            .expect("history demand is accepted");
        poll_until(
            &harness.service,
            1,
            1,
            |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if !snapshot.forming),
        );
        assert_eq!(
            harness
                .generations
                .recv_timeout(Duration::from_secs(1))
                .expect("realtime generation starts")
                .0
                .get(),
            1
        );
        assert_eq!(
            harness
                .configured_products
                .recv_timeout(Duration::from_secs(1))
                .expect("BTC product set configures"),
            ["BTC-USD"]
        );
        harness
            .actions
            .send(FixtureRealtimeAction::Connected)
            .expect("realtime connects");
        harness
            .service
            .set_resource_mode(ResourceMode::MarketsLive)
            .expect("markets-live mode applies");
        harness.service.detach(1).expect("desktop detaches");
        assert!(
            harness
                .stops
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "markets-live mode retains the provider session"
        );

        harness
            .actions
            .send(FixtureRealtimeAction::Trade(trade(2, "2.00", 3)))
            .expect("detached live trade arrives");

        harness.service.attach(2).expect("new desktop attaches");
        harness
            .service
            .register_consumer(2, 1, 2)
            .expect("new consumer registers");
        harness
            .service
            .set_demand(2, 2, 1, &btc())
            .expect("hot-series demand is accepted");
        let hot = poll_until(
            &harness.service,
            2,
            2,
            |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.forming),
        );
        assert!(matches!(
            hot,
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.bars.last().is_some_and(|bar| bar.close == 200)
        ));
        assert_eq!(harness.history_fetches.load(Ordering::Acquire), 1);

        harness.service.detach(2).expect("new desktop detaches");
        harness
            .service
            .set_resource_mode(ResourceMode::Warm)
            .expect("warm mode applies");
        assert_eq!(
            harness
                .stops
                .recv_timeout(Duration::from_secs(1))
                .expect("warm mode releases realtime")
                .0
                .get(),
            1
        );
    }

    #[test]
    fn symbol_and_interval_switch_reuses_the_shared_realtime_session() {
        let harness = MarketService::start_fixture_realtime(vec![history_bar()])
            .expect("realtime fixture starts");
        harness.service.attach(1).expect("client attaches");
        harness
            .service
            .register_consumer(1, 1, 1)
            .expect("consumer registers");
        harness
            .service
            .set_demand(1, 1, 1, &btc())
            .expect("BTC demand is accepted");
        poll_until(
            &harness.service,
            1,
            1,
            |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 1),
        );
        expect_realtime_generation(&harness, "shared realtime starts", 1);
        expect_configured_products(&harness, "BTC product set configures", &["BTC-USD"]);
        harness
            .actions
            .send(FixtureRealtimeAction::Connected)
            .expect("realtime connects");

        let eth_five = selected_series("instrument:coinbase:eth:usd", 300);
        harness
            .service
            .set_demand(1, 1, 2, &eth_five)
            .expect("ETH five-minute demand is accepted");
        let history = poll_until(
            &harness.service,
            1,
            1,
            |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2 && !snapshot.forming),
        );
        assert!(matches!(
            history,
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.series.as_ref().is_some_and(|series| {
                    series.instrument_id == "instrument:coinbase:eth:usd"
                        && series.cadence_value == 300
                })
        ));
        expect_realtime_generation(&harness, "product-set change reconnects the provider", 2);
        expect_configured_products(&harness, "ETH product set replaces BTC", &["ETH-USD"]);
        harness
            .actions
            .send(FixtureRealtimeAction::Connected)
            .expect("reconnected realtime session connects");
        harness
            .actions
            .send(FixtureRealtimeAction::Trade(trade_for(
                "ETH-USD", 10, "2000.00", 1,
            )))
            .expect("ETH live trade");
        let live = poll_until(&harness.service, 1, 1, |event| {
            is_live_update(event, 2, 2, 200_000)
        });
        assert!(is_live_update(&live, 2, 2, 200_000));

        let eth_fifteen = selected_series("instrument:coinbase:eth:usd", 900);
        harness
            .service
            .set_demand(1, 1, 3, &eth_fifteen)
            .expect("ETH interval switch is accepted");
        poll_until(&harness.service, 1, 1, |event| {
            matches!(
                event,
                envelope::Payload::SeriesSnapshot(snapshot)
                    if snapshot.generation == 3 && !snapshot.forming
            )
        });
        assert!(
            harness
                .configured_products
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "same-product cadence changes do not reconfigure realtime"
        );
        assert!(
            harness
                .generations
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "same-product cadence changes reuse the realtime generation"
        );
        harness.service.detach(1).expect("client detaches");
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
                    configured_products: None,
                }),
                &control_rx,
                &event_tx,
                &worker_overflow,
                &worker_stop,
                PROVIDER_RECONNECT_DELAY,
            );
        });

        control_tx
            .send(RealtimeControl::Start(vec!["BTC-USD".to_string()]))
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
