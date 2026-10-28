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
    EngineFaultCode, FailureStage, ProviderCatalogRejected, ProviderCatalogRejectionReason,
    ProviderCatalogSymbol, ProviderConnectionState, ProviderInstrumentSearchResult,
    ProviderMarketScreen, ProviderPresentationDescriptor, ProviderState, ScreenProviderMarkets,
    SearchProviderInstruments, SelectProviderInstrument, SeriesLoadState,
};
use crate::{
    MarketRuntimeEvent,
    study::{
        MAXIMUM_STUDY_DEPENDENCIES_PER_INSTANCE, MAXIMUM_STUDY_OUTPUTS_PER_INSTANCE,
        NativeStudyRegistration, StudyInstanceId, StudyMarketLeaseChangeKind, StudyOutputId,
        StudyRuntime, StudyRuntimeConfig,
    },
};
use aeris_contracts::InstallProviderInstrument;
use aeris_hyperliquid_market_adapter::hyperliquid_interval_for_period;
use aeris_market_data::{
    AggressorSide, AggressorTradeVolumes, BarPeriod, BarSeriesKey, DepthSnapshot, MarketBar,
    MarketTrade, OrderBook, OrderBookApplyOutcome, OrderBookState as CanonicalOrderBookState,
    OrderLevelApplyOutcome, OrderLevelBook, OrderLevelEvent, OrderLevelInterruption,
    OrderLevelState, TopOfBookQuote, merge_live_candle,
};
use aeris_market_engine::{
    ClientId, ConsumerId, ConsumerIdentity, ConsumerResourceClass, EngineError, GenerationId,
    InstrumentStreamKey, LeaseOwner, MarketDataLeaseId, MarketEngine, MarketEngineConfig,
    MarketStream, ProviderCapabilities as EngineProviderCapabilities, ProviderConfig,
    ProviderGeneration, ProviderHealth, ProviderRequest, SeriesSnapshot, SeriesTailOperation,
    StreamRequirements, Viewport, WorkspaceId,
};
use aeris_provider_history::HistoryRange;
use aeris_rithmic_protocol_adapter::RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID;

use crate::hyperliquid_display_depth::{
    HyperliquidDisplayDepthControl, HyperliquidDisplayDepthEvent,
};
use crate::hyperliquid_realtime::{
    HYPERLIQUID_PUBLIC_ACCOUNT_ID, HyperliquidCandleDemand, HyperliquidCatalogEvent,
    HyperliquidDemand, HyperliquidInstrumentDemand, HyperliquidRealtimeControl,
    HyperliquidRealtimeEvent,
};
use crate::rithmic_realtime::{
    RithmicCatalogControl, RithmicCatalogEvent, RithmicInstrumentDemand, RithmicRealtimeControl,
    RithmicRealtimeDemand, RithmicRealtimeEvent,
};

mod alerts;
pub mod descriptors;
pub use descriptors::{
    InstrumentDescriptor, InstrumentRequest, ResolvedSeries, SelectionOutcome, SelectionRequest,
};
mod quotes;
pub use quotes::{QuoteSourceKind, QuoteState};
mod trading_feed;
pub use trading_feed::TradingFeedLink;
pub use trading_feed::instrument_terms_from_install;
#[cfg(any(test, feature = "replay"))]
mod scripted_provider;
#[cfg(any(test, feature = "replay"))]
pub use scripted_provider::ScriptedQuoteProvider;
pub(crate) mod clock;
pub(crate) mod reconnect;
use alerts::PriceAlertRegistry;

const COMMAND_CAPACITY: usize = 64;
const HISTORY_CAPACITY: usize = 8;
const REALTIME_CAPACITY: usize = 2_048;
const RITHMIC_REALTIME_CONTROL_CAPACITY: usize = 2;
const REALTIME_DRAIN_BUDGET: usize = 256;
/// Bar updates a consumer may fall behind by before the oldest is dropped.
const CONSUMER_SERIES_QUEUE_CAPACITY: usize = 1_024;
/// Incremental tape updates a consumer may fall behind by before the whole
/// queue is replaced with one covering `Reset`, since dropping the oldest
/// append would open an undetectable hole in a strictly sequenced tape.
const CONSUMER_TAPE_UPDATE_CAPACITY: usize = 64;
const COORDINATOR_TICK: Duration = Duration::from_millis(16);
const HISTORY_RETRY_DELAY: Duration = Duration::from_secs(1);
/// After the bounded automatic retry budget is exhausted, keep the identical
/// ranged request in a failed cooldown like Flowsurface's `RequestHandler`.
const HISTORY_FAILED_RETRY_COOLDOWN: Duration = Duration::from_secs(30);
/// An exhausted covering request belongs to a series that has gone live-only
/// from the trade stream; it keeps one slow probe so a recovered provider
/// rejoins without restarting the fast retry budget.
const HISTORY_UNAVAILABLE_RETRY_DELAY: Duration = Duration::from_secs(300);
const HISTORY_CAPACITY_EXHAUSTED: &str = "provider history capacity is temporarily exhausted";
const MAXIMUM_HISTORY_RETRIES: u8 = 3;
const MAXIMUM_CONSUMERS: usize = 256;
const MAXIMUM_SERIES: usize = 64;
const MAXIMUM_DATA_LEASES: usize = MAXIMUM_SERIES + aeris_trading::market::MAXIMUM_QUOTE_INTEREST;
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
const LIVE_HANDOFF_HISTORY_BARS: usize = VIEWPORT_LIVE_TAIL_RESERVE + 1;
pub const MAXIMUM_RECENT_LADDER_TRADES: usize = 65_536;

type Reply<T> = SyncSender<Result<T, String>>;

/// Cloneable command boundary for the desktop-owned market coordinator.
#[derive(Clone)]
pub struct MarketService {
    commands: SyncSender<Command>,
    runtime: Arc<MarketRuntime>,
}

/// Releases instrument demand on drop without blocking the caller's thread.
/// The coordinator drains the bounded pending-release slot on its next turn.
pub struct QuoteSubscription {
    lease_id: MarketDataLeaseId,
    owner: LeaseOwner,
    key: InstrumentStreamKey,
    runtime: Arc<MarketRuntime>,
}

impl QuoteSubscription {
    #[must_use]
    pub const fn owner(&self) -> LeaseOwner {
        self.owner
    }

    #[must_use]
    pub fn latest(&self) -> Option<QuoteState> {
        self.runtime
            .quotes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .latest(&self.key)
    }
}

impl Drop for QuoteSubscription {
    fn drop(&mut self) {
        let mut releases = self
            .runtime
            .quote_releases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // No more than maximum_data_leases distinct live guards can exist.
        assert!(
            releases.len() < MAXIMUM_DATA_LEASES,
            "quote release capacity exceeded"
        );
        releases.push(self.lease_id);
        drop(releases);
        let _ = self.runtime.shutdown_wake.try_send(Command::ProviderWake);
    }
}

/// Bounded coordinator-owned lifecycle and memory snapshot for authenticated diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub struct MarketServiceStatus {
    pub connected_desktop_clients: usize,
    /// Calculation lane is isolated while a calculation abandoned at its deadline
    /// finishes, or permanently after the worker failed to start.
    pub study_execution_failed: bool,
    pub providers: Vec<ProviderState>,
    pub retained_series: usize,
    pub retained_bars: usize,
    pub approximate_series_bytes: usize,
    #[cfg(feature = "probe")]
    pub probe: ProbeStatus,
}

/// Read-only, bounded coordinator diagnostics compiled only for headless probes.
#[cfg(feature = "probe")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeStatus {
    pub hyperliquid_sessions: u64,
    pub stale_events: u64,
    pub stale_history: u64,
    pub maximum_queued_series_updates: usize,
    pub retry_attempts: Vec<ProbeRetry>,
}

#[cfg(feature = "probe")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeRetry {
    pub attempt: u32,
    pub delay_ms: u64,
    pub failed_age_ms: u64,
    pub resumed_age_ms: Option<u64>,
}

struct MarketRuntime {
    shutdown: Arc<AtomicBool>,
    shutdown_wake: SyncSender<Command>,
    provider_presentations: Vec<ProviderPresentationDescriptor>,
    rithmic_login: rithmic_login::RithmicLogin,
    active_provider_workers: Arc<Mutex<BTreeSet<String>>>,
    workers: Mutex<Option<Vec<thread::JoinHandle<()>>>>,
    quote_releases: Arc<Mutex<Vec<MarketDataLeaseId>>>,
    quotes: Arc<Mutex<quotes::QuoteBook>>,
    trading_feed: Arc<trading_feed::TradingFeedOutbox>,
    outbox: Arc<outbox::ConsumerOutbox>,
}

struct MarketServiceProviders {
    presentations: Vec<ProviderPresentationDescriptor>,
}

impl Drop for MarketRuntime {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // A full queue already makes the coordinator runnable.
        let _ = self.shutdown_wake.try_send(Command::ProviderWake);
        self.trading_feed.wake();
        self.outbox.notify_attached();
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
    #[cfg(feature = "probe")]
    ProbeFailWebSocket(u8, Reply<()>),
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
    SelectAndDemand(ClientId, SelectionRequest, Reply<()>),
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
    ScreenProviderMarkets(ClientId, ScreenProviderMarkets, Reply<()>),
    InstallProviderInstrument(InstallProviderInstrument, Reply<()>),
    ReplacePriceAlerts(
        ClientId,
        ConsumerId,
        Vec<crate::MarketPriceAlert>,
        Reply<()>,
    ),
    AcquireQuote(InstrumentStreamKey, Reply<MarketDataLeaseId>),
    SetTradingInterest(aeris_trading::market::QuoteInterest, Reply<()>),
    Drain(
        ClientId,
        ConsumerId,
        Reply<Option<crate::MarketConsumerEvent>>,
    ),
    DrainClient(
        ClientId,
        Vec<(ConsumerId, usize)>,
        Reply<Vec<(u64, crate::MarketConsumerEvent)>>,
    ),
    HistoryCompleted(
        BarSeriesKey,
        ProviderGeneration,
        Option<HistoryRange>,
        Result<HistorySnapshot, String>,
        history::tracker::HistoryTicket,
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
    pending_failure: AtomicU64,
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
        Self::new(commands, ["rithmic", "hyperliquid"])
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

    #[cfg(test)]
    pub(crate) fn report_failure(&self, provider: &str, generation: u64) {
        if let Some(slot) = self.slots.get(provider) {
            slot.pending_failure.fetch_max(generation, Ordering::AcqRel);
        }
        self.notify();
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
    ticket: history::tracker::HistoryTicket,
    provider_generation: ProviderGeneration,
    series: BarSeriesKey,
    instrument: Option<InstallProviderInstrument>,
    maximum_bars: usize,
    range: Option<HistoryRange>,
    stop: Arc<AtomicBool>,
}

#[cfg(test)]
fn fixture_history_ticket(series: &BarSeriesKey) -> history::tracker::HistoryTicket {
    use aeris_market_engine::fence::{DemandTag, EpochSequence, ProviderTag};
    history::tracker::HistoryTicket {
        series: series.clone(),
        provider: EpochSequence::<ProviderTag>::default().next(),
        demand: EpochSequence::<DemandTag>::default().next(),
        lane: history::tracker::LaneKind::Current,
        seq: 1,
    }
}

/// A current (unranged) covering request that the provider answered
/// successfully with no bars, e.g. a closed market.
///
/// Repeating the identical request returns the identical nothing, and to
/// providers such as Rithmic that repetition is forbidden polling. The answer
/// is final for its provider generation except for one request justified by
/// new evidence: a live trade newer than the empty replay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmptyCurrentHistory {
    /// Covering requests are suppressed until a live trade after this
    /// provider replay boundary arrives.
    AwaitingLiveTrade { boundary_unix_nanos: i64 },
    /// The one trade-triggered covering request has been issued.
    LiveTradeRequested,
    /// The trade-triggered request was empty too; covering requests stay
    /// suppressed until the provider generation or the demanded series changes.
    Final,
}

impl EmptyCurrentHistory {
    const fn suppresses_current_requests(self) -> bool {
        matches!(self, Self::AwaitingLiveTrade { .. } | Self::Final)
    }
}

const HISTORY_UNAVAILABLE_LIVE_DETAIL: &str = "History unavailable; showing live bars";
const HISTORY_UNAVAILABLE_WAITING_DETAIL: &str = "History unavailable; waiting for live trades";
const EMPTY_CURRENT_HISTORY_DETAIL: &str =
    "No market history is available for this period yet; waiting for live trades";
const EMPTY_CURRENT_HISTORY_RETAINED_DETAIL: &str =
    "No current market history is available yet; showing retained history until live trades arrive";

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
    display_depth: bool,
    /// Order levels are demanded and the instrument holds one of the
    /// provider's order-level subscriptions.
    order_levels: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ProviderDemand {
    instruments: Vec<ProviderInstrumentDemand>,
    candle_series: Vec<(BarSeriesKey, StreamRequirements, InstallProviderInstrument)>,
    explicit_trade_ids: BTreeSet<String>,
    missing_catalog: bool,
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
                    let trades = requested.streams.contains(MarketStream::Bars)
                        || requested.streams.contains(MarketStream::Trades)
                        || requested.streams.contains(MarketStream::Depth);
                    let quotes = requested.streams.contains(MarketStream::Quotes);
                    let order_book = requested.streams.contains(MarketStream::Depth);
                    let order_levels = requested.order_levels;
                    (trades || quotes || order_book || order_levels).then_some(
                        RithmicInstrumentDemand {
                            instrument: requested.instrument.clone(),
                            trades,
                            quotes,
                            order_book,
                            order_levels,
                        },
                    )
                })
                .collect(),
        })
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
        for requested in &self.instruments {
            let instrument = &requested.instrument;
            let (Ok(price_scale), Ok(quantity_scale)) = (
                u8::try_from(instrument.price_scale),
                u8::try_from(instrument.quantity_scale),
            ) else {
                continue;
            };
            let mapping = HyperliquidInstrumentDemand {
                wire_coin: instrument.provider_symbol.clone(),
                instrument_id: instrument.instrument_id.clone(),
                entitlement_id: instrument.entitlement_id.clone(),
                price_scale,
                quantity_scale,
            };
            if requested.streams.contains(MarketStream::Quotes)
                || requested.streams.contains(MarketStream::Depth)
            {
                quotes.insert(mapping.clone());
            }
            if requested.streams.contains(MarketStream::Depth) {
                books.insert(mapping);
            }
        }
        for requested in &self.instruments {
            if requested.streams.contains(MarketStream::Trades) {
                let instrument = &requested.instrument;
                if let (Ok(price_scale), Ok(quantity_scale)) = (
                    u8::try_from(instrument.price_scale),
                    u8::try_from(instrument.quantity_scale),
                ) {
                    trades.insert(HyperliquidInstrumentDemand {
                        wire_coin: instrument.provider_symbol.clone(),
                        instrument_id: instrument.instrument_id.clone(),
                        entitlement_id: instrument.entitlement_id.clone(),
                        price_scale,
                        quantity_scale,
                    });
                }
            }
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
struct ProviderControlState {
    accepted: Option<ProviderDemand>,
    pending: Option<ProviderDemand>,
    stop_pending: Option<ProviderGeneration>,
    engaged: bool,
    demand_dirty: bool,
    catalog_degraded: Option<ProviderGeneration>,
}

/// Everything one trade-built series needs to close its history/live seam.
#[derive(Clone, Copy)]
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

pub(crate) use lifecycle::FormingBar;

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
#[derive(Clone, Default)]
struct ConsumerEvents {
    selection_outcomes: VecDeque<MarketRuntimeEvent>,
    catalog_ready: Option<MarketRuntimeEvent>,
    provider: Option<MarketRuntimeEvent>,
    series: VecDeque<MarketRuntimeEvent>,
    #[cfg(feature = "probe")]
    maximum_series_depth: usize,
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
    /// The consumer's incremental tape stream. A `Reset` always covers
    /// everything queued before it, so overflow replaces the queue with one
    /// covering reset instead of dropping an append.
    tape_updates: VecDeque<MarketRuntimeEvent>,
    /// Continuity cursor of the incremental tape stream queued for this
    /// consumer; a new selection, provider session or prefix rewrite starts a
    /// new stream with a `Reset`.
    tape_cursor: Option<TapeStreamCursor>,
    price_alerts: VecDeque<MarketRuntimeEvent>,
    /// Latest interactive search answer per provider, so a picker searching
    /// several providers at once receives every provider's answer. Keys are
    /// fenced provider identities, bounded by the registered providers.
    catalog_search: BTreeMap<String, MarketRuntimeEvent>,
    catalog_selection: Option<MarketRuntimeEvent>,
    /// Startup re-resolution runs beside interactive picker commands on the same
    /// consumer; a shared slot let a picker result silently replace it.
    startup_catalog_search: Option<MarketRuntimeEvent>,
    startup_catalog_selection: Option<MarketRuntimeEvent>,
    catalog_screen: Option<MarketRuntimeEvent>,
}

/// Identity of one consumer's incremental tape stream plus the newest
/// ingestion ordinal queued on it. Any field change restarts the stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TapeStreamCursor {
    generation: GenerationId,
    provider_generation: u64,
    rewrite_generation: u64,
    through_ordinal: u64,
}

struct ProviderOrderBook {
    instrument: InstallProviderInstrument,
    calendar: calendar::MarketCalendarService,
    top_of_book_only: bool,
    /// Set while no current demand subscribes depth. Providers stop maintaining the
    /// canonical image then, so consumers receive only the live top of book.
    depth_unsubscribed: bool,
    book: OrderBook,
    top_of_book: Option<TopOfBookQuote>,
    recent_trades: VecDeque<crate::RetainedMarketTrade>,
    /// Sweeps classified once per ingested trade over the trailing window of
    /// `recent_trades`; shared by every tape publication until reclassified.
    trade_sweeps: Arc<[crate::MarketTradeSweep]>,
    traded_volumes: BTreeMap<i64, AggressorTradeVolumes>,
    trade_session_generation: u64,
    last_trade_source_sequence: u64,
    next_trade_ingestion_ordinal: u64,
    trade_tape_revision: u64,
    trade_tape_rewrite_generation: u64,
    trade_tape_dirty: bool,
    trade_tape_published_at: Option<std::time::Instant>,
    retention_clock_unix_nanos: i64,
    /// When the current session last delivered a newer top of book. Trade-built
    /// series on the same instrument treat it as feed liveness between trades.
    quote_observed_at: Option<std::time::Instant>,
    /// Canonical market-by-order image. It mirrors the provider's order-level
    /// stream, which flows only while the instrument holds a subscription.
    order_levels: OrderLevelBook,
    order_level_subscription: OrderLevelSubscription,
    /// Images discarded since this book opened, for consumer diagnostics.
    order_level_recoveries: u64,
}

/// Whether current demand holds one of a provider's bounded order-level
/// subscriptions for an instrument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrderLevelSubscription {
    Off,
    Admitted,
    /// Demanded while other instruments hold the provider's whole budget.
    BudgetExhausted,
}

mod lifecycle;
mod trade_sweeps;
use lifecycle::TradeModel;
mod provider_event;
pub(crate) mod provider_session;
mod realtime;
mod watchdog;
#[cfg(test)]
use provider_event::WireProviderEvent;
use provider_event::{
    ProviderDisconnect, ProviderEpochRegistry, ProviderEvent, ProviderEventKind, ProviderTradeBatch,
};

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
    presentation: ProviderPresentation,
    login_schema: Option<&'static descriptors::LoginSchema>,
    capabilities: ProviderCapabilities,
    start: ProviderRuntimeStarter,
    flush_demand: for<'a> fn(&mut Coordinator<'a>),
}

/// User-facing metadata; never used to decide market-session behavior.
type ProviderPresentation = &'static ProviderPresentationDescriptor;

/// Provider-owned market behavior consumed by the generic coordinator.
#[derive(Clone, Copy)]
struct ProviderCapabilities {
    account_id: &'static str,
    engine: EngineProviderCapabilities,
    always_open: bool,
    concurrency: ConcurrencyLimits,
    depth: Option<DepthCapabilities>,
    quotes: Option<QuoteCapability>,
    reconnect: reconnect::ReconnectPolicy,
    history: ProviderHistoryCapabilities,
    recovery_policy: ProviderRecoveryPolicy,
    alert_demand_update: AlertDemandUpdate,
    live_model: LiveModel,
    /// Timestamp convention of the provider's session, week and month bars,
    /// which trade-built bars must reproduce to stay one bar per period.
    calendar_bar_stamp: calendar::CalendarBarStamp,
    overflow_recovery_detail: &'static str,
    candle_requires_connected: bool,
    candle_correction_detail: &'static str,
    candle_wire_interval: Option<fn(BarPeriod) -> Result<String, String>>,
    candle_demand_policy: CandleDemandPolicy,
    trade_demand_policy: TradeDemandPolicy,
    instrument_missing_detail: &'static str,
    catalog_session_binding: CatalogSessionBinding,
    /// Instruments that may hold an order-level subscription at once. Each
    /// canonical order-level book holds at most `MAXIMUM_ORDER_LEVEL_ORDERS`.
    maximum_order_level_instruments: usize,
}

/// Which provider session a resolved catalog instrument belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CatalogSessionBinding {
    /// The metadata is valid only for the session that resolved it, so a
    /// resolution from a retired session is rejected as stale.
    ResolvingSession,
    /// The metadata does not depend on the transport session. The adapter
    /// stamps the session it last saw, which a concurrent selection may have
    /// replaced before the resolution installs; it joins the current session.
    CurrentSession,
}

#[derive(Clone, Copy)]
struct ConcurrencyLimits {
    maximum_sessions: u8,
}

#[derive(Clone, Copy)]
struct DepthCapabilities;

/// Runtime dispatch and supported periods; the adapter's historical dataset
/// limits remain in `aeris_provider_history::HistoryCapabilities`.
#[derive(Clone, Copy)]
struct ProviderHistoryCapabilities {
    /// Name of the dedicated history worker thread.
    worker: &'static str,
    supported_period: fn(BarPeriod) -> Result<(), String>,
}

#[derive(Clone, Copy)]
enum QuoteCapability {
    DedicatedTopOfBook,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiveModel {
    TradeBuilt,
    ProviderCandles,
}

/// Shared provider-owned state that runtime starters attach to.
struct ProviderStartContext {
    rithmic_login: crate::rithmic_realtime::RithmicLoginRevision,
}

type ProviderRuntimeStarter = fn(
    ProviderRuntimeSpec,
    &SyncSender<Command>,
    ProviderCoordinatorWake,
    &MarketEngine,
    &Arc<Mutex<BTreeSet<String>>>,
    &ProviderStartContext,
) -> Result<ProviderRuntimeRecord, String>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderRecoveryPolicy {
    CoordinatorReissuesDemand,
    WorkerReconcilesDemand,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AlertDemandUpdate {
    Immediate,
    MarkDirty,
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
        "1s", "5s", "10s", "15s", "30s", "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "8h",
        "12h", "1D", "1W", "1M",
    ],
    daily_bars: crate::ProviderDailyBarCadence::TradingSession,
    search_hint: "Search Rithmic symbols",
    logo_key: "rithmic",
    broker_venue_id: Some(aeris_trading::venue::BROKER_VENUE_ID),
    attribution: Some(crate::ProviderAttribution {
        market_data_mark_key: "market_data_by_rithmic",
        powered_by_mark_key: "powered_by_omne",
        notices: &[
            "The R | Protocol API™ software is Copyright © {year} by Rithmic, LLC. All rights reserved.",
            "Trading Platform by Rithmic™ is a trademark of Rithmic, LLC. All rights reserved.",
            "The OMNE™ software is Copyright © {year} by Omnesys, LLC and Omnesys Technologies, Inc. All rights reserved.",
        ],
        powered_by_notice: "is a trademark of Omnesys, LLC and Omnesys Technologies, Inc. All rights reserved.",
    }),
    catalog_symbol: ProviderCatalogSymbol::ProviderSymbol,
    selection_entitlement_id: RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID,
    catalog_refresh_on_startup: false,
    ready_label_suffix: "",
    // Rithmic streams depth by order, not only aggregated price levels.
    feed_level: crate::ProviderFeedLevel::OrderLevels,
    search_categories_available: false,
    market_screen_available: false,
    trades_available: true,
    connection_kind: crate::ProviderConnectionKind::Credentials,
};

const RITHMIC_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: "rithmic",
    presentation: &RITHMIC_PRESENTATION,
    login_schema: Some(&rithmic_login::LOGIN_SCHEMA),
    capabilities: ProviderCapabilities {
        account_id: RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID,
        engine: EngineProviderCapabilities {
            historical_bars: true,
            realtime_bars: true,
            streams: StreamRequirements::BARS
                .with(MarketStream::Trades)
                .with(MarketStream::Quotes)
                .with(MarketStream::Depth)
                .with(MarketStream::OrderLevels),
        },
        always_open: false,
        concurrency: ConcurrencyLimits {
            maximum_sessions: 1,
        },
        depth: Some(DepthCapabilities),
        quotes: Some(QuoteCapability::DedicatedTopOfBook),
        reconnect: reconnect::ReconnectPolicy::rithmic_recovery(),
        history: ProviderHistoryCapabilities {
            worker: "aeris-rithmic-history",
            supported_period: rithmic_supported_period,
        },
        recovery_policy: ProviderRecoveryPolicy::CoordinatorReissuesDemand,
        alert_demand_update: AlertDemandUpdate::Immediate,
        live_model: LiveModel::TradeBuilt,
        calendar_bar_stamp: calendar::CalendarBarStamp::SessionOpen,
        overflow_recovery_detail: "Local market event queue overflow; repairing continuity",
        candle_requires_connected: false,
        candle_correction_detail: "Provider candle correction requires covering history",
        candle_wire_interval: None,
        candle_demand_policy: CandleDemandPolicy::SessionManaged,
        trade_demand_policy: TradeDemandPolicy::Immediate,
        instrument_missing_detail: "Rithmic instrument is not installed",
        catalog_session_binding: CatalogSessionBinding::ResolvingSession,
        maximum_order_level_instruments: 8,
    },
    start: ProviderRuntimeRegistry::start_rithmic_runtime,
    flush_demand: flush_rithmic_provider_demand,
};

const HYPERLIQUID_PRESENTATION: ProviderPresentationDescriptor = ProviderPresentationDescriptor {
    id: "hyperliquid",
    display_name: "Hyperliquid",
    chart_interval_labels: &[
        "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "8h", "12h", "1D", "3D", "1W", "1M",
    ],
    daily_bars: crate::ProviderDailyBarCadence::FixedDay,
    search_hint: "Search Hyperliquid markets",
    logo_key: "hyperliquid",
    broker_venue_id: None,
    attribution: None,
    catalog_symbol: ProviderCatalogSymbol::ProviderSymbol,
    selection_entitlement_id: "hyperliquid-public",
    catalog_refresh_on_startup: true,
    ready_label_suffix: "",
    feed_level: crate::ProviderFeedLevel::PriceLevels,
    search_categories_available: false,
    market_screen_available: true,
    trades_available: true,
    connection_kind: crate::ProviderConnectionKind::Public,
};

const HYPERLIQUID_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: "hyperliquid",
    presentation: &HYPERLIQUID_PRESENTATION,
    login_schema: None,
    capabilities: HYPERLIQUID_CAPABILITIES,
    start: ProviderRuntimeRegistry::start_hyperliquid_runtime,
    flush_demand: flush_hyperliquid_provider_demand,
};

const HYPERLIQUID_CAPABILITIES: ProviderCapabilities = ProviderCapabilities {
    account_id: HYPERLIQUID_PUBLIC_ACCOUNT_ID,
    engine: EngineProviderCapabilities {
        historical_bars: true,
        realtime_bars: true,
        streams: StreamRequirements::BARS
            .with(MarketStream::Trades)
            .with(MarketStream::Quotes)
            .with(MarketStream::Depth),
    },
    always_open: true,
    concurrency: ConcurrencyLimits {
        maximum_sessions: 1,
    },
    depth: Some(DepthCapabilities),
    quotes: Some(QuoteCapability::DedicatedTopOfBook),
    reconnect: reconnect::ReconnectPolicy::hyperliquid(),
    history: ProviderHistoryCapabilities {
        worker: "aeris-hyperliquid-history",
        supported_period: hyperliquid_supported_period,
    },
    recovery_policy: ProviderRecoveryPolicy::WorkerReconcilesDemand,
    alert_demand_update: AlertDemandUpdate::MarkDirty,
    live_model: LiveModel::ProviderCandles,
    calendar_bar_stamp: calendar::CalendarBarStamp::TradingDateMidnightUtc,
    overflow_recovery_detail: "Local market event queue overflow; repairing continuity",
    candle_requires_connected: true,
    candle_correction_detail: "Hyperliquid candle replacement requires covering history",
    candle_wire_interval: Some(hyperliquid_candle_interval),
    candle_demand_policy: CandleDemandPolicy::ReconcileAfterSelection,
    trade_demand_policy: TradeDemandPolicy::SessionManaged,
    instrument_missing_detail: "Hyperliquid instrument is not installed",
    catalog_session_binding: CatalogSessionBinding::CurrentSession,
    maximum_order_level_instruments: 0,
};

fn hyperliquid_candle_interval(period: BarPeriod) -> Result<String, String> {
    hyperliquid_interval_for_period(period)
        .map(str::to_owned)
        .map_err(|_| "Hyperliquid history is unavailable for this interval".to_string())
}

const BUILT_IN_PROVIDER_DESCRIPTORS: &[ProviderDescriptor] =
    &[RITHMIC_DESCRIPTOR, HYPERLIQUID_DESCRIPTOR];

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

/// Published provider metadata comes from the same registry that starts sessions.
#[must_use]
pub fn published_provider_descriptors() -> &'static [descriptors::ProviderDescriptor] {
    static DESCRIPTORS: std::sync::OnceLock<Vec<descriptors::ProviderDescriptor>> =
        std::sync::OnceLock::new();
    DESCRIPTORS
        .get_or_init(|| {
            BUILT_IN_PROVIDER_DESCRIPTORS
                .iter()
                .map(descriptors::ProviderDescriptor::from_registration)
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

struct ProviderRuntimeSpec {
    descriptor: ProviderDescriptor,
    history: Option<Box<dyn HistorySource>>,
    realtime: ProviderRealtimeSpec,
    #[cfg(any(test, feature = "replay"))]
    scripted: Option<scripted_provider::ScriptedChannels>,
}

struct ProviderRealtimeSpec {
    enabled: bool,
}

impl ProviderRuntimeSpec {
    fn rithmic(history: Box<dyn HistorySource>, enabled: bool) -> Self {
        Self {
            descriptor: RITHMIC_DESCRIPTOR,
            history: Some(history),
            realtime: ProviderRealtimeSpec { enabled },
            #[cfg(any(test, feature = "replay"))]
            scripted: None,
        }
    }

    fn hyperliquid(history: Box<dyn HistorySource>, enabled: bool) -> Self {
        Self {
            descriptor: HYPERLIQUID_DESCRIPTOR,
            history: Some(history),
            realtime: ProviderRealtimeSpec { enabled },
            #[cfg(any(test, feature = "replay"))]
            scripted: None,
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
                (self.generation.load(Ordering::Acquire) > 1
                    && self.reconnecting.load(Ordering::Acquire))
                .then(|| {
                    "Market data connection interrupted; reconnecting automatically.".to_string()
                })
            })
    }
}

struct HyperliquidChannels {
    controls: SyncSender<HyperliquidRealtimeControl>,
    events: Receiver<
        aeris_market_engine::fence::Fenced<
            aeris_market_engine::fence::ProviderTag,
            HyperliquidRealtimeEvent,
        >,
    >,
    display_controls: SyncSender<HyperliquidDisplayDepthControl>,
    display_events: Receiver<HyperliquidDisplayDepthEvent>,
}

struct ProviderCatalogChannels {
    enabled: bool,
    channels: Box<dyn ProviderCatalogChannel>,
}

trait ProviderCatalogChannel: Send {
    fn try_recv(&self) -> Option<ProviderCatalogEvent>;
    fn send(&self, provider_id: &str, command: ProviderCatalogCommand) -> Result<(), String>;
}

struct ProviderCatalogChannelImpl<C, E> {
    controls: SyncSender<C>,
    events: Receiver<E>,
    dispatch: fn(&SyncSender<C>, &str, ProviderCatalogCommand) -> Result<(), String>,
}

impl<C, E> ProviderCatalogChannelImpl<C, E> {
    fn new(
        controls: SyncSender<C>,
        events: Receiver<E>,
        dispatch: fn(&SyncSender<C>, &str, ProviderCatalogCommand) -> Result<(), String>,
    ) -> Self {
        Self {
            controls,
            events,
            dispatch,
        }
    }
}

impl<C: Send, E: Into<ProviderCatalogEvent> + Send> ProviderCatalogChannel
    for ProviderCatalogChannelImpl<C, E>
{
    fn try_recv(&self) -> Option<ProviderCatalogEvent> {
        self.events.try_recv().ok().map(Into::into)
    }

    fn send(&self, provider_id: &str, command: ProviderCatalogCommand) -> Result<(), String> {
        (self.dispatch)(&self.controls, provider_id, command)
    }
}

struct ProviderRuntimeRecord {
    descriptor: ProviderDescriptor,
    history: SyncSender<HistoryRequest>,
    cancellation: Arc<AtomicBool>,
    lifecycle: Arc<ProviderRuntimeLifecycle>,
    hyperliquid: Option<HyperliquidChannels>,
    rithmic: Option<RithmicChannels>,
    catalog: ProviderCatalogChannels,
    workers: Vec<thread::JoinHandle<()>>,
}
struct RithmicChannels {
    controls: SyncSender<RithmicRealtimeControl>,
    events: Receiver<RithmicRealtimeEvent>,
    binding: Arc<Mutex<provider_session::RithmicBinding>>,
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
    hyperliquid: Option<&'a HyperliquidChannels>,
    rithmic: Option<&'a RithmicChannels>,
    catalog: Option<&'a dyn ProviderCatalogChannel>,
}

enum ProviderCatalogCommand {
    Search(SearchProviderInstruments),
    Select(SelectProviderInstrument),
    Screen(ScreenProviderMarkets),
}

enum ProviderRuntimeEvent {
    #[cfg(test)]
    Realtime(WireProviderEvent),
    BoundRealtime(ProviderEvent),
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
    SelectionResolved {
        consumer_id: u64,
        command_generation: u64,
        instrument: InstallProviderInstrument,
    },
    Rejected {
        rejection: ProviderCatalogRejected,
        selection: bool,
    },
    ScreenCompleted(ProviderMarketScreen),
    ScreenRejected(ProviderCatalogRejected),
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
            HyperliquidCatalogEvent::ScreenCompleted(screen) => Self::ScreenCompleted(screen),
            HyperliquidCatalogEvent::ScreenRejected(rejection) => Self::ScreenRejected(rejection),
            HyperliquidCatalogEvent::RefreshFailed { detail } => Self::RefreshFailed(detail),
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
            #[cfg(test)]
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
            Self::BoundRealtime(event) => (
                event.provider,
                event.wire_generation,
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

mod broker_venue;
pub mod calendar;
mod rithmic_login;
pub use broker_venue::BrokerVenueLink;
mod runtime;
pub use rithmic_login::{RithmicLoginSummary, RithmicServer};
use runtime::join_runtime_workers;

mod command_fences;
mod consumer_registry;
mod coordinator;
use command_fences::{CommandFences, CommandKind};

/// Offline coordinator workload used by the release benchmark example.
///
/// It starts no provider workers or network sessions.
///
/// # Errors
/// Returns an error if fixture setup, a canonical update, or overflow recovery fails.
pub fn coordinator_performance_baseline() -> Result<serde_json::Value, String> {
    coordinator::performance_baseline()
}

/// Measures the coordinator-to-bridge wake and a real 30-second idle wait.
///
/// # Errors
/// Returns an error if the bounded benchmark cannot deliver a publication.
pub fn bridge_wake_performance() -> Result<serde_json::Value, String> {
    coordinator::bridge_wake_performance()
}
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

fn publishes_order_levels(descriptor: &ProviderDescriptor) -> bool {
    descriptor.presentation.feed_level == crate::ProviderFeedLevel::OrderLevels
}

fn configured_engine_from_descriptors(
    descriptors: &[ProviderDescriptor],
) -> Result<MarketEngine, String> {
    let mut engine = MarketEngine::with_lease_capacity(
        MarketEngineConfig {
            maximum_consumers: NonZeroUsize::new(MAXIMUM_CONSUMERS).unwrap_or(NonZeroUsize::MIN),
            maximum_series: NonZeroUsize::new(MAXIMUM_SERIES).unwrap_or(NonZeroUsize::MIN),
            // SeriesStore performs no eager allocation for this logical capacity;
            // runtime compaction keeps each series at/below its high watermark.
            maximum_bars: NonZeroUsize::new(MAXIMUM_STORED_BARS).unwrap_or(NonZeroUsize::MIN),
        },
        NonZeroUsize::new(MAXIMUM_DATA_LEASES).unwrap_or(NonZeroUsize::MIN),
    );
    for descriptor in descriptors {
        let capabilities = &descriptor.capabilities;
        if descriptor.presentation.id != descriptor.id
            || descriptor.presentation.feed_level.depth_available() != capabilities.depth.is_some()
            || descriptor.presentation.feed_level.quotes_available()
                != capabilities.quotes.is_some()
            || descriptor.presentation.trades_available
                != capabilities.engine.streams.contains(MarketStream::Trades)
            || capabilities.concurrency.maximum_sessions == 0
            || capabilities.depth.is_some()
                != capabilities.engine.streams.contains(MarketStream::Depth)
            || capabilities.quotes.is_some()
                != capabilities.engine.streams.contains(MarketStream::Quotes)
            || publishes_order_levels(descriptor)
                != capabilities
                    .engine
                    .streams
                    .contains(MarketStream::OrderLevels)
            || publishes_order_levels(descriptor)
                != (capabilities.maximum_order_level_instruments > 0)
        {
            return Err(format!(
                "{} has inconsistent market capabilities",
                descriptor.id
            ));
        }
        engine
            .register_provider(
                descriptor.id.to_string(),
                ProviderConfig {
                    account_id: descriptor.capabilities.account_id.to_string(),
                    capabilities: descriptor.capabilities.engine,
                    reconnect_delay: descriptor.capabilities.reconnect.initial_delay(),
                },
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(engine)
}

mod outbox;
mod publication;
use publication::{
    engine_install_failure_stage, fail_waiters, publish_state, series_state, series_state_payload,
    series_update_message,
};

mod instrument_selection;
use instrument_selection::{
    id, try_send_hyperliquid_catalog, try_send_rithmic_catalog, validate_provider_instrument,
    validate_provider_screen, validate_provider_search, validate_provider_selection,
};

mod history;
use history::spawn_history_worker;

#[cfg(test)]
pub(crate) mod tests;
