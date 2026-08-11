//! Single-owner resident market coordinator and provider history/realtime workers.

use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::sync::atomic::AtomicUsize;

use axiusflow_coinbase_market_adapter::{
    COINBASE_PUBLIC_ACCOUNT_ID, CanonicalTrade, CoinbaseBarAggregator, CoinbaseBarAggregatorConfig,
    CoinbaseConfig, CoinbaseHistoryCapabilityAdapter, CoinbaseInterval, CoinbaseSession,
    ENTITLEMENT_CLASS, aggregate_coinbase_bars, decode_history_bar,
};
use axiusflow_local_engine_protocol::{
    DemandError, EngineFaultCode, InstallProviderInstrument, MarketBar as IpcMarketBar,
    OrderBookLevel as IpcOrderBookLevel, OrderBookSnapshot as IpcOrderBookSnapshot,
    OrderBookState as IpcOrderBookState, PersistenceState, ProviderConnectionState, ProviderState,
    SeriesCadence, SeriesKey, SeriesLoadState, SeriesSnapshot as IpcSeriesSnapshot, SeriesState,
    envelope,
};
use axiusflow_market_data::{
    BarPeriod, BarSeriesKey, DepthLevel, DepthSnapshot, MarketBar, MarketTrade, OrderBook,
    OrderBookApplyOutcome, OrderBookRecoveryReason, OrderBookState as CanonicalOrderBookState,
};
use axiusflow_market_engine::{
    ClientId, ConsumerId, ConsumerIdentity, EngineError, GenerationId, MarketEngine,
    MarketEngineConfig, ProviderCapabilities, ProviderGeneration, ProviderHealth, Viewport,
    WorkspaceId,
};
use axiusflow_provider_history::{DataClass, HistoryPageRequest, HistoryRange};

use crate::local_history::{LocalHistoryStore, StoredHistory};
use crate::rithmic_realtime::{RithmicRealtimeControl, RithmicRealtimeEvent};

const COMMAND_CAPACITY: usize = 64;
const HISTORY_CAPACITY: usize = 8;
const STORAGE_CAPACITY: usize = 16;
const REALTIME_CAPACITY: usize = 2_048;
const REALTIME_DRAIN_BUDGET: usize = 256;
const LIVE_BUFFER_CAPACITY: usize = 4_096;
const COORDINATOR_TICK: Duration = Duration::from_millis(16);
const RECONNECT_DELAY: Duration = Duration::from_millis(250);
const MAXIMUM_CONSUMERS: usize = 256;
const MAXIMUM_SERIES: usize = 128;
const HISTORY_BARS_PER_SERIES: usize = 350;
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
}

enum Command {
    Attach(ClientId, Reply<()>),
    Detach(ClientId, Reply<()>),
    Register(ConsumerIdentity, Reply<()>),
    Remove(ClientId, ConsumerId, Reply<()>),
    Viewport(ClientId, ConsumerId, GenerationId, Viewport, Reply<()>),
    Visibility(ClientId, ConsumerId, bool, Reply<()>),
    Demand(ClientId, ConsumerId, GenerationId, BarSeriesKey, Reply<()>),
    InstallProviderInstrument(InstallProviderInstrument, Reply<()>),
    Poll(ClientId, ConsumerId, Reply<Option<envelope::Payload>>),
    HistoryCompleted(
        BarSeriesKey,
        ProviderGeneration,
        Result<HistorySnapshot, String>,
    ),
    LocalHistoryCompleted(
        BarSeriesKey,
        ProviderGeneration,
        Result<Option<StoredHistory>, String>,
    ),
    PersistenceCompleted(BarSeriesKey, ProviderGeneration, Result<(), String>),
}

struct HistoryRequest {
    series: BarSeriesKey,
    provider_generation: ProviderGeneration,
    instrument: Option<InstallProviderInstrument>,
    stop: Arc<AtomicBool>,
}

enum StorageRequest {
    Read(BarSeriesKey, ProviderGeneration),
    Persist(BarSeriesKey, ProviderGeneration, Vec<MarketBar>, bool),
}

struct HistorySnapshot {
    price_scale: u8,
    quantity_scale: u8,
    bars: Vec<MarketBar>,
}

struct DemandWaiter {
    consumer_id: ConsumerId,
    generation: GenerationId,
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
    demand_error: Option<envelope::Payload>,
    order_book: Option<envelope::Payload>,
}

impl ConsumerEvents {
    fn pop(&mut self) -> Option<envelope::Payload> {
        self.provider
            .take()
            .or_else(|| self.snapshot.take())
            .or_else(|| self.series_state.take())
            .or_else(|| self.demand_error.take())
            .or_else(|| self.order_book.take())
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
    live_session_generation: Option<u64>,
    last_trade_sequence: Option<u64>,
    history_boundary_unix_nanos: i64,
}

enum RithmicLiveCadence {
    Fixed { seconds: i64 },
    Tick { trades: u32, forming: u32 },
}

impl RithmicLiveHandoff {
    fn new(series: &BarSeriesKey, generation: ProviderGeneration) -> Option<Self> {
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
    ) -> Result<(), String> {
        let boundary = bars
            .last()
            .ok_or_else(|| "Rithmic live handoff requires history".to_string())?
            .exchange_timestamp_unix_nanos;
        self.price_scale = price_scale;
        self.quantity_scale = quantity_scale;
        self.bars = bars.to_vec();
        self.history_boundary_unix_nanos = boundary;
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
    exchange_timestamp_unix_nanos: i64,
) -> Result<MarketBar, String> {
    bar.high = bar.high.max(trade.price);
    bar.low = bar.low.min(trade.price);
    bar.close = trade.price;
    bar.volume = bar
        .volume
        .checked_add(trade.quantity)
        .ok_or_else(|| "Rithmic live volume overflowed".to_string())?;
    bar.exchange_timestamp_seconds = exchange_timestamp_unix_nanos.div_euclid(1_000_000_000);
    bar.exchange_timestamp_unix_nanos = exchange_timestamp_unix_nanos;
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
    fn try_new(series: &BarSeriesKey, generation: ProviderGeneration) -> Result<Self, String> {
        let profile = coinbase_series_profile(series)?;
        Ok(Self {
            generation,
            aggregator: coinbase_aggregator(profile)?,
            buffered: VecDeque::with_capacity(LIVE_BUFFER_CAPACITY),
            connected: false,
            history_ready: false,
            dirty: false,
        })
    }

    fn reset(&mut self, generation: ProviderGeneration) {
        self.generation = generation;
        self.aggregator.reset();
        self.buffered.clear();
        self.connected = false;
        self.history_ready = false;
        self.dirty = false;
    }
}

trait HistorySource: Send + 'static {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String>;
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
    config: CoinbaseConfig,
}

#[cfg(test)]
struct FixtureHistory {
    bars: Vec<MarketBar>,
    fetches: Option<Arc<AtomicUsize>>,
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
    history_fetches: Arc<AtomicUsize>,
}

#[cfg(test)]
impl HistorySource for FixtureHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        if let Some(fetches) = &self.fetches {
            fetches.fetch_add(1, Ordering::AcqRel);
        }
        let (interval_seconds, price_scale, quantity_scale) =
            if request.series.provider_id == "coinbase" {
                let profile = coinbase_series_profile(&request.series)?;
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
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        let series = &request.series;
        let profile = coinbase_series_profile(series)?;
        let interval_seconds = u64::from(profile.interval_seconds);
        let now_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock is unavailable".to_string())?
            .as_secs();
        let end_seconds = now_seconds - now_seconds % interval_seconds;
        let end_unix_nanos = i64::try_from(end_seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000_000_000))
            .ok_or_else(|| "Coinbase history end time overflowed".to_string())?;
        let span_nanos = i64::try_from(HISTORY_BARS_PER_SERIES)
            .ok()
            .and_then(|count| count.checked_mul(i64::from(profile.interval_seconds)))
            .and_then(|seconds| seconds.checked_mul(1_000_000_000))
            .ok_or_else(|| "Coinbase history span overflowed".to_string())?;
        let request = HistoryPageRequest {
            provider_id: "coinbase".to_string(),
            account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
            entitlement_revision: ENTITLEMENT_CLASS.to_string(),
            instrument_id: series.instrument_id.clone(),
            data_class: DataClass::Bars,
            resolution: profile.resolution.to_string(),
            range: HistoryRange {
                start_unix_nanos: end_unix_nanos.saturating_sub(span_nanos),
                end_unix_nanos,
            },
            maximum_items: NonZeroUsize::new(HISTORY_BARS_PER_SERIES).unwrap_or(NonZeroUsize::MIN),
            continuation: None,
        };
        let batch = self.adapter.fetch_paginated(&request)?;
        let mut bars = batch
            .items
            .iter()
            .map(decode_history_bar)
            .collect::<Result<Vec<_>, _>>()?;
        if bars.is_empty() {
            return Err("Coinbase returned no completed historical bars".to_string());
        }
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
        &request.stop,
    )?;
    Ok(HistorySnapshot {
        price_scale: snapshot.price_scale,
        quantity_scale: snapshot.quantity_scale,
        bars: snapshot.bars,
    })
}

impl LiveCoinbaseRealtime {
    fn try_new() -> Result<Self, String> {
        CoinbaseConfig::try_new(vec!["BTC-USD".to_string(), "ETH-USD".to_string()])
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
    /// Starts the process-owned market coordinator and its bounded provider-history worker.
    ///
    /// # Errors
    /// Returns an error when provider configuration or either bounded worker cannot start.
    pub fn start() -> Result<Self, String> {
        let storage = LocalHistoryStore::open(
            &crate::default_engine_state_root()?
                .join("market-history")
                .join("coinbase"),
        );
        Self::start_composed(
            HistorySources::Split {
                coinbase: Box::new(LiveCoinbaseHistory::try_new()?),
                rithmic: Box::new(LiveRithmicHistory),
            },
            Some(Box::new(LiveCoinbaseRealtime::try_new()?)),
            Some(storage),
            true,
        )
    }

    #[cfg(test)]
    /// Starts a deterministic in-memory history source for IPC integration tests.
    ///
    /// # Errors
    /// Returns an error when either bounded worker cannot start.
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

    #[cfg(test)]
    fn start_fixture_realtime(bars: Vec<MarketBar>) -> Result<FixtureRealtimeHarness, String> {
        let (action_tx, action_rx) = mpsc::sync_channel(16);
        let (generation_tx, generation_rx) = mpsc::sync_channel(4);
        let (stop_tx, stop_rx) = mpsc::sync_channel(4);
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
            })),
            None,
        )?;
        Ok(FixtureRealtimeHarness {
            service,
            actions: action_tx,
            generations: generation_rx,
            stops: stop_rx,
            history_fetches,
        })
    }

    #[cfg(test)]
    fn start_with_source(source: impl HistorySource) -> Result<Self, String> {
        Self::start_with_sources(source, None, None)
    }

    #[cfg(test)]
    fn start_with_sources(
        source: impl HistorySource,
        realtime: Option<Box<dyn RealtimeSource>>,
        storage: Option<Result<LocalHistoryStore, String>>,
    ) -> Result<Self, String> {
        Self::start_composed(
            HistorySources::Shared(Box::new(source)),
            realtime,
            storage,
            false,
        )
    }

    fn start_composed(
        sources: HistorySources,
        realtime: Option<Box<dyn RealtimeSource>>,
        storage: Option<Result<LocalHistoryStore, String>>,
        rithmic_realtime: bool,
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
        let (rithmic_realtime_control_tx, rithmic_realtime_control_rx) = mpsc::sync_channel(1);
        let realtime_overflow = Arc::new(AtomicBool::new(false));
        let realtime_stop = Arc::new(AtomicBool::new(true));
        let completion_tx = command_tx.clone();
        thread::Builder::new()
            .name("axiusflow-coinbase-history".to_string())
            .spawn(move || {
                run_history_worker(coinbase_source, &coinbase_history_rx, &completion_tx);
            })
            .map_err(|error| error.to_string())?;
        if let Some((source, requests)) = rithmic_source.zip(rithmic_history_rx) {
            let completion_tx = command_tx.clone();
            thread::Builder::new()
                .name("axiusflow-rithmic-history".to_string())
                .spawn(move || run_history_worker(source, &requests, &completion_tx))
                .map_err(|error| error.to_string())?;
        }
        let storage_completion_tx = command_tx.clone();
        thread::Builder::new()
            .name("axiusflow-local-history".to_string())
            .spawn(move || {
                run_storage_worker(storage, &storage_rx, &storage_completion_tx);
            })
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
        if rithmic_realtime {
            thread::Builder::new()
                .name("axiusflow-rithmic-realtime".to_string())
                .spawn(move || {
                    crate::rithmic_realtime::run(
                        &rithmic_realtime_control_rx,
                        &rithmic_realtime_tx,
                    );
                })
                .map_err(|error| error.to_string())?;
        }
        thread::Builder::new()
            .name("axiusflow-market-engine".to_string())
            .spawn(move || {
                run_coordinator(
                    engine,
                    CoordinatorChannels {
                        commands: &command_rx,
                        coinbase_history: &coinbase_history_tx,
                        rithmic_history: &rithmic_history_tx,
                        storage: &storage_tx,
                        realtime_control: &realtime_control_tx,
                        realtime: &realtime_rx,
                        rithmic_realtime_control: rithmic_realtime
                            .then_some(&rithmic_realtime_control_tx),
                        rithmic_realtime: &rithmic_realtime_rx,
                    },
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

fn run_history_worker(
    mut source: Box<dyn HistorySource>,
    requests: &Receiver<HistoryRequest>,
    completions: &SyncSender<Command>,
) {
    while let Ok(request) = requests.recv() {
        let result = source.fetch(&request);
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

fn run_storage_worker(
    mut storage: Option<Result<LocalHistoryStore, String>>,
    requests: &Receiver<StorageRequest>,
    completions: &SyncSender<Command>,
) {
    while let Ok(request) = requests.recv() {
        let completion = match request {
            StorageRequest::Read(series, generation) => {
                let result = match storage.as_mut() {
                    Some(Ok(storage)) => storage.read_latest(&series),
                    Some(Err(error)) => Err(error.clone()),
                    None => Ok(None),
                };
                Command::LocalHistoryCompleted(series, generation, result)
            }
            StorageRequest::Persist(series, generation, bars, derived) => {
                let result = match storage.as_mut() {
                    Some(Ok(storage)) => storage.persist(&series, &bars, derived),
                    Some(Err(error)) => Err(error.clone()),
                    None => Ok(()),
                };
                Command::PersistenceCompleted(series, generation, result)
            }
        };
        if completions.send(completion).is_err() {
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
}

fn run_coordinator(
    engine: MarketEngine,
    channels: CoordinatorChannels<'_>,
    realtime_overflow: &AtomicBool,
    realtime_stop: &Arc<AtomicBool>,
) {
    let mut coordinator = Coordinator {
        engine,
        coinbase_history: channels.coinbase_history,
        rithmic_history: channels.rithmic_history,
        storage: channels.storage,
        realtime_control: channels.realtime_control,
        rithmic_realtime_control: channels.rithmic_realtime_control,
        realtime_stop,
        attached: BTreeSet::new(),
        pending: BTreeMap::new(),
        history_inflight: BTreeSet::new(),
        history_cancellations: BTreeMap::new(),
        local_loaded: BTreeSet::new(),
        events: BTreeMap::new(),
        live: BTreeMap::new(),
        rithmic_live: BTreeMap::new(),
        rithmic_order_books: BTreeMap::new(),
        catalog: BTreeMap::new(),
        catalog_sessions: BTreeMap::new(),
        catalog_selections: BTreeMap::new(),
        realtime_started: false,
        realtime_connected: false,
    };
    loop {
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
        if realtime_overflow.swap(false, Ordering::AcqRel) {
            coordinator.realtime_interrupted("Coinbase realtime queue overflowed");
        }
        coordinator.publish_live();
        coordinator.publish_rithmic_live();
        match channels.commands.recv_timeout(COORDINATOR_TICK) {
            Ok(command) => coordinator.handle_command(command),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
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
    realtime_stop: &'a Arc<AtomicBool>,
    attached: BTreeSet<ClientId>,
    pending: BTreeMap<BarSeriesKey, Vec<DemandWaiter>>,
    history_inflight: BTreeSet<(BarSeriesKey, ProviderGeneration)>,
    history_cancellations: BTreeMap<(BarSeriesKey, ProviderGeneration), Arc<AtomicBool>>,
    local_loaded: BTreeSet<(BarSeriesKey, ProviderGeneration)>,
    events: BTreeMap<ConsumerId, ConsumerEvents>,
    live: BTreeMap<BarSeriesKey, LiveHandoff>,
    rithmic_live: BTreeMap<BarSeriesKey, RithmicLiveHandoff>,
    rithmic_order_books: BTreeMap<String, RithmicOrderBook>,
    catalog: BTreeMap<(String, String), InstallProviderInstrument>,
    catalog_sessions: BTreeMap<String, u64>,
    catalog_selections: BTreeMap<String, u64>,
    realtime_started: bool,
    realtime_connected: bool,
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
                self.prune_unused_live_series();
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
                    self.prune_unused_live_series();
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
                    },
                    &reply,
                );
            }
            Command::InstallProviderInstrument(instrument, reply) => {
                let result = self.install_provider_instrument(instrument);
                let _ = reply.send(result);
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
            Command::LocalHistoryCompleted(series, generation, result) => {
                self.local_history_completed(&series, generation, result);
            }
            Command::PersistenceCompleted(series, generation, result) => {
                self.persistence_completed(&series, generation, &result);
            }
        }
    }

    fn install_provider_instrument(
        &mut self,
        instrument: InstallProviderInstrument,
    ) -> Result<(), String> {
        validate_provider_instrument(&instrument)?;
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
        let selection = (!newer_session)
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
                .filter(|installed| *installed == &instrument)
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
        if provider == "rithmic"
            && let Some(control) = self.rithmic_realtime_control
        {
            match control.try_send(RithmicRealtimeControl::Select(instrument.clone())) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    return Err("Rithmic live selection capacity is exhausted".to_string());
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err("Rithmic live worker is unavailable".to_string());
                }
            }
        }
        if engine_generation.is_none()
            || provider != "rithmic"
                && engine_generation.is_some_and(|current| provider_generation > current)
        {
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
        self.catalog_selections
            .insert(provider.clone(), instrument.selection_generation);
        if provider == "rithmic" {
            self.rithmic_order_books.clear();
            self.rithmic_order_books.insert(
                instrument.instrument_id.clone(),
                RithmicOrderBook::new(instrument.clone()),
            );
        }
        self.catalog.insert(key, instrument);
        Ok(())
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
        let mut publication = self
            .engine
            .set_series_demand(waiter.consumer_id, waiter.generation, series)
            .map_err(|error| error.to_string())?;
        self.remove_waiter(waiter.consumer_id);
        self.prune_unused_live_series();
        if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
            events.snapshot = None;
            events.series_state = None;
            events.demand_error = None;
        }
        if publication.as_ref().is_some_and(|publication| {
            publication.snapshot.provider_generation != provider_generation
        }) {
            self.engine.invalidate_series(series);
            publication = None;
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
        let (provider_generation, publication) =
            match self.accept_series_demand(client_id, series, &waiter) {
                Ok(accepted) => accepted,
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            };
        if let Err(error) = self.ensure_realtime(series) {
            let _ = reply.send(Err(error));
            return;
        }
        self.publish_order_book_to_consumer(waiter.consumer_id);
        if let Some(publication) = publication {
            let seeded = series.provider_id != "coinbase"
                || self.live.get_mut(series).is_none_or(|live| {
                    if live.history_ready {
                        return true;
                    }
                    live.aggregator.reset();
                    let result = if publication.snapshot.forming {
                        live.aggregator
                            .seed_canonical_backfill(&publication.snapshot.bars)
                    } else {
                        live.aggregator
                            .seed_canonical_history(&publication.snapshot.bars)
                    };
                    if result.is_ok() {
                        live.history_ready = true;
                    }
                    result.is_ok()
                });
            if !seeded {
                let _ = reply.send(Err(
                    "Coinbase cached history/live handoff failed".to_string()
                ));
                return;
            }
            if let Some(events) = self.events.get_mut(&waiter.consumer_id) {
                publish_ready(events, &publication);
            }
            self.series_live_if_ready(series);
        } else {
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
            if first {
                if series.provider_id == "coinbase" {
                    let derived = self.derive_compatible_history(series, provider_generation);
                    if matches!(derived, Ok(true)) {
                        if let Err(detail) = self.enqueue_history(series, provider_generation)
                            && let Some(waiters) = self.pending.remove(series)
                        {
                            fail_waiters(&mut self.events, waiters, detail);
                        }
                    } else if self
                        .enqueue_local_history(series, provider_generation)
                        .is_err()
                        && let Err(detail) = self.enqueue_history(series, provider_generation)
                        && let Some(waiters) = self.pending.remove(series)
                    {
                        fail_waiters(&mut self.events, waiters, detail);
                    }
                } else if self
                    .enqueue_local_history(series, provider_generation)
                    .is_err()
                    && let Err(detail) = self.enqueue_history(series, provider_generation)
                    && let Some(waiters) = self.pending.remove(series)
                {
                    fail_waiters(&mut self.events, waiters, detail);
                }
            }
        }
        let _ = reply.send(Ok(()));
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
        let source_series = BarSeriesKey {
            period: BarPeriod::time(60).map_err(|error| error.to_string())?,
            ..series.clone()
        };
        let Some(source) = self.engine.series_snapshot(&source_series) else {
            return Ok(false);
        };
        if source.provider_generation != generation {
            return Ok(false);
        }
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
        if self
            .storage
            .try_send(StorageRequest::Persist(
                series.clone(),
                generation,
                bars,
                true,
            ))
            .is_err()
        {
            self.broadcast_persistence_for(
                series,
                PersistenceState::Degraded,
                Some("Derived history persistence is unavailable"),
            );
        }
        Ok(true)
    }

    fn enqueue_local_history(
        &self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
    ) -> Result<(), &'static str> {
        match self
            .storage
            .try_send(StorageRequest::Read(series.clone(), generation))
        {
            Ok(()) => Ok(()),
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
                if let Ok(publications) = self.engine.install_history(
                    generation,
                    series,
                    price_scale,
                    quantity_scale,
                    stored.bars,
                ) {
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
            fail_waiters(&mut self.events, waiters, detail);
        }
    }

    fn provider_generation_for_series(
        &self,
        series: &BarSeriesKey,
    ) -> Result<ProviderGeneration, String> {
        match series.provider_id.as_str() {
            "coinbase" => {
                coinbase_series_profile(series)?;
                Ok(self.coinbase_provider_generation())
            }
            "rithmic" => {
                crate::rithmic_history::chart_interval(series.period)?;
                if series.definition_version != 1 {
                    return Err("unsupported Rithmic engine series definition".to_string());
                }
                let installed = self
                    .catalog
                    .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                    .ok_or_else(|| "Rithmic instrument is not installed".to_string())?;
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
                let profile = coinbase_series_profile(series)?;
                Ok((profile.price_scale, profile.quantity_scale))
            }
            "rithmic" => {
                let installed = self
                    .catalog
                    .get(&(series.provider_id.clone(), series.instrument_id.clone()))
                    .ok_or_else(|| "Rithmic instrument is not installed".to_string())?;
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

    fn ensure_realtime(&mut self, series: &BarSeriesKey) -> Result<(), String> {
        if series.provider_id == "rithmic" {
            if !self.rithmic_live.contains_key(series)
                && let Some(mut handoff) =
                    RithmicLiveHandoff::new(series, self.provider_generation_for_series(series)?)
            {
                handoff.connected = self
                    .engine
                    .provider_status("rithmic")
                    .is_some_and(|status| status.health == ProviderHealth::Online);
                self.rithmic_live.insert(series.clone(), handoff);
            }
            return Ok(());
        }
        if series.provider_id != "coinbase" {
            return Err("resident engine realtime provider is unsupported".to_string());
        }
        if !self.live.contains_key(series) {
            let mut handoff = LiveHandoff::try_new(series, self.coinbase_provider_generation())?;
            handoff.connected = self.realtime_connected;
            self.live.insert(series.clone(), handoff);
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
        let instrument = if series.provider_id == "rithmic" {
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
            stop: Arc::clone(&stop),
        };
        let history = if series.provider_id == "rithmic" {
            self.rithmic_history
        } else {
            self.coinbase_history
        };
        match try_enqueue_history(history, request) {
            Ok(()) => {
                self.history_inflight.insert(key.clone());
                self.history_cancellations.insert(key, stop);
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
        } else if let Some(waiters) = self.pending.remove(series) {
            fail_waiters(
                &mut self.events,
                waiters,
                if series.provider_id == "rithmic" {
                    "Rithmic historical bars are unavailable"
                } else {
                    "Coinbase historical bars are unavailable"
                },
            );
        }
        if series.provider_id == "coinbase" {
            self.broadcast_provider(
                ProviderConnectionState::Recovering,
                generation,
                Some("Coinbase history repair is retrying"),
            );
        }
    }

    fn history_completed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        result: Result<HistorySnapshot, String>,
    ) {
        let key = (series.clone(), generation);
        self.history_inflight.remove(&key);
        self.history_cancellations.remove(&key);
        let current = self
            .engine
            .provider_status(&series.provider_id)
            .and_then(|status| status.generation);
        if current != Some(generation) {
            if series.provider_id == "coinbase"
                && let Some(current) = current
                && self.live.contains_key(series)
            {
                let _ = self.enqueue_history(series, current);
            }
            return;
        }
        let Ok(snapshot) = result else {
            self.history_failed(series, generation);
            return;
        };
        let bars = snapshot.bars;
        if self.local_loaded.remove(&(series.clone(), generation)) {
            self.engine.invalidate_series(series);
        }
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
                    fail_waiters(&mut self.events, waiters, &error.to_string());
                }
                return;
            }
        };
        let persistence = PersistenceState::Pending;
        for publication in publications {
            if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                publish_state(
                    events,
                    &publication,
                    SeriesLoadState::Ready,
                    persistence,
                    None,
                );
            }
        }
        match self.storage.try_send(StorageRequest::Persist(
            series.clone(),
            generation,
            bars.clone(),
            false,
        )) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.broadcast_persistence_for(
                    series,
                    PersistenceState::Degraded,
                    Some("Local history persistence is unavailable"),
                );
            }
        }
        if let Some(live) = self.rithmic_live.get_mut(series)
            && live
                .seed(snapshot.price_scale, snapshot.quantity_scale, &bars)
                .is_err()
        {
            live.history_ready = false;
            live.dirty = false;
            self.broadcast_rithmic_provider(
                ProviderConnectionState::Recovering,
                generation,
                Some("Rithmic history/live handoff failed"),
            );
            return;
        }
        if let Some(live) = self.live.get_mut(series) {
            let connected = live.connected;
            let buffered = std::mem::take(&mut live.buffered);
            live.aggregator.reset();
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
        self.pending.remove(series);
        self.series_live_if_ready(series);
    }

    fn persistence_completed(
        &mut self,
        series: &BarSeriesKey,
        generation: ProviderGeneration,
        result: &Result<(), String>,
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
                } else if engine.latest_publication(*consumer_id).is_some() {
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
                    self.realtime_interrupted("Coinbase realtime disconnected");
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
            RithmicRealtimeEvent::Recovering(generation)
            | RithmicRealtimeEvent::Disconnected(generation) => {
                self.rithmic_recovering(generation, "Rithmic live session is recovering");
            }
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
                self.engine.invalidate_series(selected);
                if let Some(live) = self.rithmic_live.get_mut(selected) {
                    live.reset(generation);
                }
                self.broadcast_series_resolution_for(
                    selected,
                    SeriesLoadState::Resolving,
                    PersistenceState::Durable,
                    Some("Rithmic live session changed; covering history is reloading"),
                );
            }
            for selected in series {
                let _ = self.enqueue_local_history(&selected, generation);
            }
        }
        let _ = self
            .engine
            .set_provider_health("rithmic", generation, ProviderHealth::Connecting);
        self.broadcast_rithmic_provider(ProviderConnectionState::Connecting, generation, None);
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
        self.broadcast_rithmic_provider(ProviderConnectionState::Online, generation, None);
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
                .contains(&(series.clone(), generation))
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
        let failed = self
            .rithmic_live
            .values_mut()
            .filter(|live| live.generation == generation && live.connected)
            .any(|live| live.accept_trade(trade).is_err());
        if failed {
            self.rithmic_recovering(
                generation.0.get(),
                "Rithmic live aggregation requires covering history",
            );
            for live in self.rithmic_live.values_mut() {
                live.history_ready = false;
                live.dirty = false;
                live.buffered.clear();
            }
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
        self.broadcast_rithmic_provider(
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

    fn broadcast_rithmic_provider(
        &mut self,
        state: ProviderConnectionState,
        generation: ProviderGeneration,
        detail: Option<&str>,
    ) {
        let payload = envelope::Payload::ProviderState(ProviderState {
            provider: "rithmic".to_string(),
            state: state as i32,
            generation: generation.0.get(),
            detail: detail.map(str::to_string),
        });
        for (consumer_id, events) in &mut self.events {
            if self
                .engine
                .current_demand(*consumer_id)
                .and_then(|demand| demand.series.as_ref())
                .is_some_and(|series| series.provider_id == "rithmic")
            {
                events.provider = Some(payload.clone());
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
        for live in self.live.values_mut().filter(|live| {
            live.generation == generation
                && live.connected
                && live.aggregator.product_id() == trade.product_id
        }) {
            if live.history_ready {
                if live.aggregator.apply_trade(trade).is_err() {
                    interrupted = Some("Coinbase realtime aggregation failed");
                    break;
                }
                live.dirty = true;
            } else if live.buffered.len() == LIVE_BUFFER_CAPACITY {
                interrupted = Some("Coinbase history/live buffer overflowed");
                break;
            } else {
                live.buffered.push_back(trade.clone());
            }
        }
        if let Some(detail) = interrupted {
            self.realtime_interrupted(detail);
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

    fn realtime_interrupted(&mut self, detail: &str) {
        let generation = self.coinbase_provider_generation();
        let _ = self
            .engine
            .set_provider_health("coinbase", generation, ProviderHealth::Recovering);
        self.realtime_connected = false;
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
            .filter(|(_, live)| live.connected && live.history_ready && live.dirty)
            .filter_map(|(series, live)| {
                let active = live.aggregator.in_flight()?;
                let mut bars = live.aggregator.history();
                bars.push(active);
                live.dirty = false;
                Some((
                    series.clone(),
                    live.generation,
                    live.aggregator.price_scale(),
                    live.aggregator.quantity_scale(),
                    bars,
                ))
            })
            .collect::<Vec<_>>();
        for (series, generation, price_scale, quantity_scale, bars) in ready {
            if let Ok(publications) = self.engine.install_realtime(
                generation,
                &series,
                price_scale,
                quantity_scale,
                bars,
                true,
            ) {
                for publication in publications {
                    if let Some(events) = self.events.get_mut(&publication.consumer_id) {
                        events.snapshot = Some(snapshot_message(&publication));
                    }
                }
            } else {
                self.realtime_interrupted("Coinbase live publication failed");
                return;
            }
        }
    }

    fn publish_rithmic_live(&mut self) {
        let ready = self
            .rithmic_live
            .values_mut()
            .filter(|live| live.connected && live.history_ready && live.dirty)
            .map(|live| {
                live.dirty = false;
                (
                    live.series.clone(),
                    live.generation,
                    live.price_scale,
                    live.quantity_scale,
                    live.bars.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (series, generation, price_scale, quantity_scale, bars) in ready {
            if let Ok(publications) = self.engine.install_realtime(
                generation,
                &series,
                price_scale,
                quantity_scale,
                bars,
                true,
            ) {
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
            } else {
                if let Some(live) = self.rithmic_live.get_mut(&series) {
                    live.history_ready = false;
                }
                self.broadcast_rithmic_provider(
                    ProviderConnectionState::Recovering,
                    generation,
                    Some("Rithmic live publication requires covering history"),
                );
            }
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

    fn coinbase_provider_generation(&self) -> ProviderGeneration {
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
        let unobserved = self
            .pending
            .iter()
            .filter(|(_, waiters)| waiters.is_empty())
            .map(|(series, _)| series.clone())
            .collect::<Vec<_>>();
        self.pending.retain(|_, waiters| !waiters.is_empty());
        for series in unobserved {
            if series.provider_id != "rithmic" {
                continue;
            }
            for ((active, _), stop) in &self.history_cancellations {
                if active == &series {
                    stop.store(true, Ordering::Release);
                }
            }
        }
    }

    fn prune_unused_live_series(&mut self) {
        let demanded = self
            .events
            .keys()
            .filter_map(|consumer_id| {
                self.engine
                    .current_demand(*consumer_id)
                    .and_then(|demand| demand.series.clone())
            })
            .collect::<BTreeSet<_>>();
        self.live.retain(|series, _| demanded.contains(series));
        self.rithmic_live
            .retain(|series, _| demanded.contains(series));
    }

    fn stop_realtime_if_idle(&mut self) {
        if !self.events.is_empty() || !self.realtime_started {
            return;
        }
        self.realtime_stop.store(true, Ordering::Release);
        self.realtime_started = false;
        self.realtime_connected = false;
        self.live.clear();
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
                order_book: false,
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
            ProviderCapabilities {
                historical_bars: true,
                realtime_bars: true,
                order_book: true,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(engine)
}

fn fail_waiters(
    events: &mut BTreeMap<ConsumerId, ConsumerEvents>,
    waiters: Vec<DemandWaiter>,
    detail: &str,
) {
    for waiter in waiters {
        if let Some(events) = events.get_mut(&waiter.consumer_id) {
            events.series_state = Some(series_state(
                waiter.consumer_id,
                waiter.generation,
                SeriesKey::default(),
                SeriesLoadState::Failed,
                Some(detail.to_string()),
            ));
            events.demand_error = Some(envelope::Payload::DemandError(DemandError {
                consumer_id: waiter.consumer_id.0.get(),
                generation: waiter.generation.0.get(),
                code: EngineFaultCode::Retryable as i32,
                stage: "provider_history".to_string(),
                detail: detail.to_string(),
            }));
        }
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
        if value.trim().is_empty()
            || value.len() > MAXIMUM_CATALOG_FIELD_BYTES
            || value.chars().any(char::is_control)
        {
            return Err("provider instrument identity is invalid".to_string());
        }
    }
    if instrument.price_scale > 18 || instrument.quantity_scale > 18 {
        return Err("provider instrument precision is invalid".to_string());
    }
    Ok(())
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

#[derive(Clone, Copy)]
struct CoinbaseSeriesProfile {
    product_id: &'static str,
    resolution: &'static str,
    interval_seconds: u32,
    price_scale: u8,
    quantity_scale: u8,
}

fn coinbase_series_profile(series: &BarSeriesKey) -> Result<CoinbaseSeriesProfile, String> {
    if series.provider_id != "coinbase" || series.definition_version != 1 {
        return Err("unsupported Coinbase engine series identity".to_string());
    }
    let (product_id, price_scale, quantity_scale) = match series.instrument_id.as_str() {
        "instrument:coinbase:btc:usd" => ("BTC-USD", 2, 8),
        "instrument:coinbase:eth:usd" => ("ETH-USD", 2, 8),
        _ => return Err("unsupported Coinbase engine instrument".to_string()),
    };
    let interval_seconds = match series.period {
        BarPeriod::Time { seconds } => seconds,
        BarPeriod::Tick { .. }
        | BarPeriod::Session { .. }
        | BarPeriod::Week { .. }
        | BarPeriod::Month { .. } => {
            return Err("unsupported Coinbase engine interval".to_string());
        }
    };
    let resolution = match interval_seconds {
        60 => "1m",
        300 => "5m",
        900 => "15m",
        3_600 => "1h",
        _ => return Err("unsupported Coinbase engine interval".to_string()),
    };
    Ok(CoinbaseSeriesProfile {
        product_id,
        resolution,
        interval_seconds,
        price_scale,
        quantity_scale,
    })
}

fn coinbase_interval(seconds: u32) -> Result<CoinbaseInterval, String> {
    match seconds {
        60 => Ok(CoinbaseInterval::Minute1),
        300 => Ok(CoinbaseInterval::Minute5),
        900 => Ok(CoinbaseInterval::Minute15),
        3_600 => Ok(CoinbaseInterval::Hour1),
        _ => Err("unsupported Coinbase engine derivation interval".to_string()),
    }
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

    struct ControlledHistory {
        fetches: Arc<AtomicUsize>,
        release: Receiver<()>,
    }

    struct SwitchingHistory {
        requested: SyncSender<BarSeriesKey>,
        release: Receiver<()>,
    }

    struct BlockingRithmicHistory {
        started: SyncSender<()>,
        cancelled: SyncSender<()>,
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
            realtime_stop,
            attached: BTreeSet::new(),
            pending: BTreeMap::from([(
                series.clone(),
                vec![DemandWaiter {
                    consumer_id,
                    generation: GenerationId(id(1).expect("generation")),
                }],
            )]),
            history_inflight: BTreeSet::new(),
            history_cancellations: BTreeMap::new(),
            local_loaded: BTreeSet::new(),
            events: BTreeMap::from([(consumer_id, ConsumerEvents::default())]),
            live: BTreeMap::new(),
            rithmic_live: BTreeMap::new(),
            rithmic_order_books: BTreeMap::new(),
            catalog: BTreeMap::new(),
            catalog_sessions: BTreeMap::new(),
            catalog_selections: BTreeMap::new(),
            realtime_started: false,
            realtime_connected: false,
        }
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
        let mut fixed =
            RithmicLiveHandoff::new(&fixed_series, generation).expect("fixed cadence streams");
        fixed.seed(2, 0, &[history]).expect("history seeds");
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
            RithmicLiveHandoff::new(&tick_series, generation).expect("tick cadence streams");
        tick.seed(2, 0, &[tick_history])
            .expect("tick history seeds");
        tick.accept_trade(&rithmic_trade(1, 1, 60_500_000_000, 120))
            .expect("first trade starts a new tick bar");
        tick.accept_trade(&rithmic_trade(2, 1, 60_600_000_000, 121))
            .expect("second trade updates tick bar");
        assert_eq!(tick.bars.len(), 2);
        assert_eq!(tick.bars[1].source_sequence, 41);
        assert_eq!(tick.bars[1].close, 121);
        assert_eq!(tick.bars[1].volume, 4);
        assert_eq!(tick.bars[1].exchange_timestamp_unix_nanos, 60_600_000_000);
        assert!(
            tick.accept_trade(&rithmic_trade(4, 2, 60_700_000_000, 122))
                .is_err()
        );
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
            .set_series_demand(
                consumer_id,
                GenerationId(id(3).expect("generation")),
                &series,
            )
            .expect("demand installs");
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
            .install_provider_instrument(provider_instrument(7, 2))
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
            let profile = coinbase_series_profile(series)?;
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
            let profile = coinbase_series_profile(series)?;
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
            })
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
        assert!(matches!(
            harness.generations.try_recv(),
            Err(TryRecvError::Empty)
        ));
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
        assert!(matches!(
            poll_until(&harness.service, client_id, 9, |event| matches!(
                event,
                envelope::Payload::SeriesSnapshot(snapshot)
                    if snapshot.forming && snapshot.generation == 1
            )),
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.bars.last().is_some_and(|bar| bar.close == 200)
        ));

        harness
            .service
            .remove_consumer(client_id, 1)
            .expect("switched chart closes");
        harness
            .actions
            .send(FixtureRealtimeAction::Trade(trade(2, "2.10", 2)))
            .expect("remaining chart trade");
        assert!(matches!(
            poll_until(&harness.service, client_id, 9, |event| matches!(
                event,
                envelope::Payload::SeriesSnapshot(snapshot)
                    if snapshot.forming
                        && snapshot.bars.last().is_some_and(|bar| bar.close == 210)
            )),
            envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 1
        ));
        assert!(harness.service.poll_event(client_id, 1).is_err());
        assert_eq!(
            harness.history_fetches.load(Ordering::Acquire),
            series.len()
        );
        assert!(matches!(
            harness.generations.try_recv(),
            Err(TryRecvError::Empty)
        ));
    }

    #[test]
    fn storage_failure_degrades_persistence_without_hiding_provider_history() {
        let service = MarketService::start_with_sources(
            FixtureHistory {
                bars: vec![history_bar()],
                fetches: None,
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
    }

    #[test]
    fn retained_history_publishes_before_provider_repair_and_survives_its_failure() {
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
        engine
            .set_series_demand(
                consumer_id,
                GenerationId(id(1).expect("generation")),
                &internal_series(&btc()).expect("series"),
            )
            .expect("demand installs");
        let series = internal_series(&btc()).expect("series");
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
            coordinator.events[&consumer_id].series_state,
            Some(envelope::Payload::SeriesState(ref state))
                if state.state == SeriesLoadState::Partial as i32
                    && state.persistence == PersistenceState::Durable as i32
        ));
        assert!(
            history_rx.try_recv().is_ok(),
            "provider repair is queued after local publication"
        );
        coordinator.history_completed(&series, generation, Err("provider unavailable".to_string()));
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
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 8,
                bars: vec![history_bar()],
            }),
        );
        assert!(matches!(
            coordinator.events[&consumer_id].series_state,
            Some(envelope::Payload::SeriesState(ref state))
                if state.state == SeriesLoadState::Ready as i32
                    && state.persistence == PersistenceState::Pending as i32
        ));
        assert!(matches!(
            storage_rx.try_recv(),
            Ok(StorageRequest::Persist(ref persisted, current, ref bars, false))
                if persisted == &series && current == generation && bars == &[history_bar()]
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
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 0,
                bars: vec![repaired],
            }),
        );
        assert!(matches!(
            storage_rx.try_recv(),
            Ok(StorageRequest::Persist(ref persisted, current, ref bars, false))
                if persisted == &series && current == generation && bars == &[repaired]
        ));
        assert!(matches!(
            coordinator.events[&consumer_id].series_state,
            Some(envelope::Payload::SeriesState(ref state))
                if state.state == SeriesLoadState::Ready as i32
                    && state.persistence == PersistenceState::Pending as i32
        ));
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
                    && snapshot.bars[0].exchange_timestamp_seconds == 300)
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
    fn full_history_queue_fails_waiters_without_blocking_the_coordinator() {
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        history_tx
            .try_send(HistoryRequest {
                series: internal_series(&btc()).expect("first series"),
                provider_generation: ProviderGeneration(id(1).expect("provider generation")),
                instrument: None,
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
        let first = requested_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first history request starts");
        assert_eq!(first, internal_series(&required[0]).expect("first series"));
        release_tx.send(()).expect("first history completes");
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

        let mut completed = 1;
        while completed < 5 {
            requested_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("queued obsolete history starts");
            release_tx.send(()).expect("obsolete history completes");
            completed += 1;
        }
        thread::sleep(Duration::from_millis(20));
        while let Some(event) = service.poll_event(1, 1).expect("final consumer polls") {
            assert!(
                !matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation != 7),
                "obsolete history reached the active consumer"
            );
        }
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
        assert_eq!(
            harness
                .generations
                .recv_timeout(Duration::from_secs(1))
                .expect("shared realtime starts")
                .0
                .get(),
            1
        );
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
        assert!(
            harness
                .generations
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "switching reuses the existing provider session"
        );
        harness
            .actions
            .send(FixtureRealtimeAction::Trade(trade_for(
                "ETH-USD", 10, "2000.00", 1,
            )))
            .expect("ETH live trade");
        let live = poll_until(
            &harness.service,
            1,
            1,
            |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2 && snapshot.forming),
        );
        assert!(matches!(
            live,
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.bars.last().is_some_and(|bar| {
                    bar.exchange_timestamp_seconds == 600 && bar.close == 200_000
                })
        ));
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
