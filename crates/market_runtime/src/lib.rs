//! In-process market runtime.
//!
//! `Aeris` owns provider connections, canonical market state, on-demand
//! history, and realtime fanout inside the desktop process. There is no secondary local process, serialized runtime transport, or restart-replay layer here.

mod cadence;
/// Consumer-event fence types exposed without a desktop dependency on the
/// market engine implementation crate.
pub mod fence {
    pub use aeris_market_engine::fence::{ConsumerTag, EpochSequence, Fenced};
}
mod hyperliquid_display_depth;
mod hyperliquid_history;
mod hyperliquid_market_screen;
mod hyperliquid_realtime;
pub mod market_service;
mod rithmic_history;
mod rithmic_realtime;
mod rithmic_venue;
pub mod study;

pub use cadence::{
    CadenceError, legacy_interval, legacy_series_period, persisted_interval, resolve_cadence,
    resolve_series_key,
};

/// Maximum durable alerts owned by one market consumer.
pub const MAXIMUM_PRICE_ALERTS_PER_CONSUMER: usize = 32;

/// Returns a safe provider-owned replacement for a legacy durable display
/// symbol when the old identity carries enough information to migrate it.
///
/// Routing identity is never rewritten here. This exists so durable desktop
/// state can refresh presentation aliases without learning provider-specific
/// symbol rules.
#[must_use]
pub fn migrate_retained_provider_display_symbol(
    instrument: &aeris_contracts::InstallProviderInstrument,
) -> Option<String> {
    match instrument.provider.as_str() {
        "hyperliquid" => aeris_hyperliquid_market_adapter::legacy_display_label(
            &instrument.instrument_id,
            &instrument.provider_symbol,
            &instrument.display_symbol,
        ),
        "rithmic" => aeris_rithmic_protocol_adapter::legacy_display_label(
            &instrument.instrument_id,
            &instrument.display_symbol,
        ),
        _ => None,
    }
}

/// One bounded, fixed-point price alert evaluated by the market coordinator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketPriceAlert {
    pub id: String,
    pub instrument: aeris_contracts::InstallProviderInstrument,
    pub price: i64,
    pub condition: aeris_contracts::PriceAlertCondition,
    pub frequency: aeris_contracts::PriceAlertFrequency,
    pub active: bool,
}

/// Exact trade observation that satisfied one runtime-owned price alert.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketPriceAlertTrigger {
    pub consumer_id: aeris_market_engine::ConsumerId,
    pub alert_id: String,
    pub instrument: aeris_contracts::InstallProviderInstrument,
    pub threshold_price: i64,
    pub observed_price: i64,
    pub condition: aeris_contracts::PriceAlertCondition,
    pub frequency: aeris_contracts::PriceAlertFrequency,
    pub observed_unix_nanos: i64,
    pub remains_active: bool,
}

/// Direct in-process order-book image for one consumer generation.
///
/// The canonical depth publication is owned and validated by the market-data
/// domain. Consumer and demand generation are the only routing metadata added
/// here; display precision and UI selection identity remain presentation state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketDisplayDepth {
    pub provider_generation: u64,
    pub display_generation: u64,
    pub source_sequence: u64,
    pub bids: Vec<aeris_market_data::DepthLevel>,
    pub asks: Vec<aeris_market_data::DepthLevel>,
}

/// Most prices per side one [`MarketOrderLevelView`] carries, best first.
pub const MAXIMUM_ORDER_LEVEL_VIEW_PRICES: usize = 32;
/// Most resting orders one [`MarketOrderQueue`] lists.
pub const MAXIMUM_ORDER_LEVEL_VIEW_QUEUE: usize = 64;

/// Resting orders at one price, front of the queue first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketOrderQueue {
    pub price: i64,
    /// At most [`MAXIMUM_ORDER_LEVEL_VIEW_QUEUE`] orders in ascending provider
    /// priority, each with its exchange order id and quantity.
    pub orders: Vec<aeris_market_data::RestingOrder>,
    /// Every order resting at the price, including those beyond `orders`.
    pub order_count: usize,
    /// Total quantity resting at the price.
    pub quantity: i64,
}

/// Authority of the order-level image behind a [`MarketOrderLevelView`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketOrderLevelStatus {
    /// The provider was asked for order levels and no covering image arrived yet.
    AwaitingSnapshot,
    Ready,
    /// The image was discarded and the provider is sending a covering image.
    Recovering(aeris_market_data::OrderLevelInterruption),
    /// The provider turned order levels off for its session.
    Unavailable(aeris_market_data::OrderLevelInterruption),
    /// The provider session that owned the image ended.
    Stale,
    /// Other instruments hold every order-level subscription the provider allows.
    BudgetExhausted,
}

/// Bounded market-by-order view of the runtime's canonical order-level book.
///
/// Queues are present only while `status` is `Ready`: a gap, refusal or bound
/// violation empties them until a covering image arrives. `image`, `revision`
/// and `provider_sequence` are the canonical continuity evidence of the image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketOrderLevelView {
    pub status: MarketOrderLevelStatus,
    pub provider_generation: u64,
    pub image: u64,
    pub revision: u64,
    pub provider_sequence: Option<u64>,
    /// Images discarded since the runtime opened this instrument's book.
    pub recoveries: u64,
    /// At most [`MAXIMUM_ORDER_LEVEL_VIEW_PRICES`] bid prices, best first.
    pub bids: Vec<MarketOrderQueue>,
    /// At most [`MAXIMUM_ORDER_LEVEL_VIEW_PRICES`] ask prices, best first.
    pub asks: Vec<MarketOrderQueue>,
}

/// Direct in-process order-book image for one consumer generation.
///
/// `display_depth` is an optional provider-aggregated presentation sidecar. It
/// never enters the canonical `OrderBook`; consumers may use it for a coarse
/// DOM while retaining canonical raw depth for correctness and fallback.
/// `order_levels` is present only while the consumer demands
/// `MarketStream::OrderLevels` from a provider that publishes them; it is boxed
/// so depth-only snapshots stay small.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketOrderBookSnapshot {
    pub consumer_id: aeris_market_engine::ConsumerId,
    pub generation: aeris_market_engine::GenerationId,
    pub publication: aeris_market_data::OrderBookPublication,
    pub display_depth: Option<MarketDisplayDepth>,
    pub order_levels: Option<Box<MarketOrderLevelView>>,
}

/// One exact canonical trade retained by the market runtime's bounded tape.
///
/// Provider source ordering and the runtime's local ingestion ordering remain
/// separate evidence. `observed_unix_nanos` is the monotonic retention clock;
/// the provider and exchange timestamps remain unchanged in `trade.metadata`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedMarketTrade {
    pub ingestion_ordinal: u64,
    pub observed_unix_nanos: i64,
    /// Exchange trading date of the print, in days since the Unix epoch, from the venue's
    /// session calendar. `None` when the venue has no supported calendar; none is guessed.
    pub trading_day: Option<i64>,
    pub trade: std::sync::Arc<aeris_market_data::MarketTrade>,
}

/// Latest bounded immutable trade-tape image for one consumer generation.
///
/// A newer revision completely supersedes an older queued image. The shared
/// `Arc` keeps chart, tape panel, and study projections from copying the same
/// retained trade collection inside the desktop process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketTradeTapeSnapshot {
    pub consumer_id: aeris_market_engine::ConsumerId,
    pub generation: aeris_market_engine::GenerationId,
    pub provider_id: String,
    pub instrument_id: String,
    pub entitlement_id: String,
    pub provider_generation: u64,
    pub revision: u64,
    pub source_watermark: u64,
    /// Changes when history or a correction rewrites the retained prefix.
    pub rewrite_generation: u64,
    pub price_scale: u8,
    pub quantity_scale: u8,
    pub trades: std::sync::Arc<[RetainedMarketTrade]>,
}

/// One runtime-classified sweep on the retained trade tape.
///
/// Computed by `market_runtime` once per ingested trade over the trailing
/// 512-print window (90th-percentile size threshold, 100 ms grouping). The
/// ordinal span addresses trades on the same tape; `terminal_price` and
/// `total_volume` are the scaled presentation values the classification ran
/// on, while the raw fixed-point prints stay authoritative in
/// [`RetainedMarketTrade`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarketTradeSweep {
    pub first_ingestion_ordinal: u64,
    pub last_ingestion_ordinal: u64,
    pub timestamp_micros: i64,
    pub terminal_price: f64,
    pub total_volume: f64,
    pub price_levels: u32,
    pub aggressor: aeris_market_data::AggressorSide,
}

/// How one incremental trade-tape publication changes the consumer's tape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketTradeTapeOperation {
    /// A covering window: the consumer replaces its whole tape. Sent for the
    /// first publication of a selection, a provider-session restart, a
    /// retained-prefix rewrite, an eviction gap, or an outbox overflow.
    Reset {
        trades: std::sync::Arc<[RetainedMarketTrade]>,
    },
    /// Trades continuing the consumer's tape exactly at `from_ordinal`, which
    /// is one past the newest ordinal the runtime queued for this tape stream.
    Append {
        from_ordinal: u64,
        trades: std::sync::Arc<[RetainedMarketTrade]>,
    },
}

/// Incremental generation-fenced trade-tape publication for one consumer.
///
/// Carries the same continuity metadata as [`MarketTradeTapeSnapshot`] plus
/// the sweeps classified over the trailing retained window at publication.
/// The full-snapshot [`MarketRuntimeEvent::TradeTapeSnapshot`] stays
/// authoritative until the desktop consumes these updates (stabilization
/// M7.11).
#[derive(Clone, Debug, PartialEq)]
pub struct MarketTradeTapeUpdate {
    pub consumer_id: aeris_market_engine::ConsumerId,
    pub generation: aeris_market_engine::GenerationId,
    pub provider_id: String,
    pub instrument_id: String,
    pub entitlement_id: String,
    pub provider_generation: u64,
    pub revision: u64,
    pub source_watermark: u64,
    /// Changes when history or a correction rewrites the retained prefix.
    pub rewrite_generation: u64,
    pub price_scale: u8,
    pub quantity_scale: u8,
    /// Sweeps over the trailing classification window of the current tape.
    pub sweeps: std::sync::Arc<[MarketTradeSweep]>,
    pub operation: MarketTradeTapeOperation,
}

/// Direct completed provider selection for one runtime consumer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketProviderInstrumentSelection {
    pub consumer_id: aeris_market_engine::ConsumerId,
    pub instrument: aeris_contracts::InstallProviderInstrument,
    pub command_generation: u64,
}

/// Direct generation-fenced readiness state for one canonical series.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeriesPresentationStatus {
    Loading,
    ConnectingLive,
    Live,
    Recovering,
    AwaitingData,
    /// Covering history exhausted its retry budget; the series is built from
    /// the live trade stream alone while one slow history probe stays armed.
    HistoryUnavailable,
}

/// Direct generation-fenced readiness state for one canonical series.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketSeriesState {
    pub consumer_id: aeris_market_engine::ConsumerId,
    pub generation: aeris_market_engine::GenerationId,
    pub series: Option<aeris_market_data::BarSeriesKey>,
    pub state: aeris_contracts::SeriesLoadState,
    pub detail: Option<String>,
    /// Runtime-owned presentation phase; clients never derive this from provider text.
    pub presentation: Option<SeriesPresentationStatus>,
}

/// Part of a canonical series demand that a [`MarketDemandError`] invalidates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketDemandErrorScope {
    /// The current series could not be served; its published state is unusable.
    Series,
    /// Only history older than the published series could not be loaded. The
    /// current series, its forming candle and live updates remain valid.
    OlderHistory,
}

/// Direct structured failure for one canonical series demand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketDemandError {
    pub consumer_id: aeris_market_engine::ConsumerId,
    pub generation: aeris_market_engine::GenerationId,
    pub scope: MarketDemandErrorScope,
    pub code: aeris_contracts::EngineFaultCode,
    pub stage: aeris_contracts::FailureStage,
    pub detail: String,
    pub series: Option<aeris_market_data::BarSeriesKey>,
    pub cause: String,
    pub elapsed_millis: Option<u64>,
}

/// Latest committed scalar output for one runtime-owned study output.
///
/// Output state is immutable and structurally shared across study generations.
/// Delivery therefore does not copy a whole indicator on every forming-bar
/// revision, while consumers can still materialize stable full timestamp/value
/// slices when required. A newer snapshot for the same output completely
/// supersedes an older queued one.
#[derive(Clone, Debug, PartialEq)]
pub struct MarketStudyOutputSnapshot {
    pub consumer_id: aeris_market_engine::ConsumerId,
    /// The exact demand selection that produced these immutable study points.
    pub generation: Option<aeris_market_engine::GenerationId>,
    pub study_id: study::StudyInstanceId,
    pub output_id: study::StudyOutputId,
    pub study_identifier: String,
    pub output: study::StudyOutputSpec,
    /// Transitive market streams required by this study's dependency graph.
    ///
    /// The runtime remains the owner of those streams; this metadata lets a
    /// presentation host retain the typed binding contract without receiving
    /// or duplicating the canonical tape/book.
    pub stream_requirements: aeris_market_engine::StreamRequirements,
    pub series: study::StudyOutputSeries,
}

/// Study instances removed from one chart consumer.
///
/// Explicit removal can delete a dependency subtree. The complete bounded set is
/// published so presentation can discard every chart-local output without
/// reconstructing runtime dependency state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketStudyRemoved {
    pub consumer_id: aeris_market_engine::ConsumerId,
    pub generation: Option<aeris_market_engine::GenerationId>,
    pub study_ids: Vec<study::StudyInstanceId>,
}

/// Study outputs that became invalid because the instance was reinitialized in
/// place. Definitions/identities remain live; presentation should clear these
/// output series until replacement snapshots arrive.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketStudyOutputsInvalidated {
    pub consumer_id: aeris_market_engine::ConsumerId,
    pub generation: Option<aeris_market_engine::GenerationId>,
    pub study_ids: Vec<study::StudyInstanceId>,
}

/// Direct in-process market event delivered from the runtime to desktop panes.
///
/// This is an ordinary typed in-process event boundary. The variants are
/// typed Rust values shared in one process; no framing, decoding, request IDs,
/// or process-reconnect replay exists on this boundary.
#[derive(Clone, Debug)]
pub enum MarketRuntimeEvent {
    SelectionOutcome(market_service::SelectionOutcome),
    CatalogReady {
        provider: String,
    },
    ProviderState(aeris_contracts::ProviderState),
    SeriesSnapshot(aeris_market_engine::ConsumerPublication),
    SeriesUpdate(aeris_market_engine::ConsumerSeriesUpdate),
    SeriesState(MarketSeriesState),
    DemandError(MarketDemandError),
    OrderBookSnapshot(MarketOrderBookSnapshot),
    TradeTapeSnapshot(MarketTradeTapeSnapshot),
    TradeTapeUpdate(MarketTradeTapeUpdate),
    StudyOutputSnapshot(MarketStudyOutputSnapshot),
    StudyOutputsInvalidated(MarketStudyOutputsInvalidated),
    StudyRemoved(MarketStudyRemoved),
    PriceAlertTriggered(MarketPriceAlertTrigger),
    ProviderInstrumentSearchResult(aeris_contracts::ProviderInstrumentSearchResult),
    ProviderInstrumentSelection(MarketProviderInstrumentSelection),
    /// A search or selection the provider or runtime refused. `selection`
    /// says which command lane failed, because the rejection reason alone
    /// (for example an unavailable dispatch) does not identify it.
    ProviderCatalogRejected {
        rejection: aeris_contracts::ProviderCatalogRejected,
        selection: bool,
    },
    ProviderMarketScreen(aeris_contracts::ProviderMarketScreen),
    ProviderMarketScreenRejected(aeris_contracts::ProviderCatalogRejected),
    Fault(aeris_contracts::Fault),
}

impl MarketRuntimeEvent {
    fn selection_generation(&self) -> Option<aeris_market_engine::GenerationId> {
        match self {
            Self::SeriesSnapshot(event) => Some(event.generation),
            Self::SeriesUpdate(event) => Some(event.generation),
            Self::SeriesState(event) => Some(event.generation),
            Self::DemandError(event) => Some(event.generation),
            Self::OrderBookSnapshot(event) => Some(event.generation),
            Self::TradeTapeSnapshot(event) => Some(event.generation),
            Self::TradeTapeUpdate(event) => Some(event.generation),
            Self::StudyOutputSnapshot(event) => event.generation,
            Self::StudyOutputsInvalidated(event) => event.generation,
            Self::StudyRemoved(event) => event.generation,
            Self::SelectionOutcome(_)
            | Self::CatalogReady { .. }
            | Self::ProviderState(_)
            | Self::PriceAlertTriggered(_)
            | Self::ProviderInstrumentSearchResult(_)
            | Self::ProviderInstrumentSelection(_)
            | Self::ProviderCatalogRejected { .. }
            | Self::ProviderMarketScreen(_)
            | Self::ProviderMarketScreenRejected(_)
            | Self::Fault(_) => None,
        }
    }
}

/// The required envelope for every runtime-to-client consumer event variant.
/// Its epoch identifies the registered consumer lifetime, not an adapter wire
/// generation. Demand-bound payloads additionally carry their selection epoch.
pub type MarketConsumerEvent =
    aeris_market_engine::fence::Fenced<aeris_market_engine::fence::ConsumerTag, MarketRuntimeEvent>;

pub use aeris_market_engine::{
    ConsumerId as MarketConsumerId, ConsumerPublication as MarketSeriesSnapshot,
    ConsumerResourceClass as MarketConsumerResourceClass,
    ConsumerSeriesUpdate as MarketSeriesUpdate, GenerationId as MarketGenerationId,
    InstrumentStreamKey, LeaseOwner, MarketStream, ProviderGeneration as MarketProviderGeneration,
    SeriesSnapshot as CanonicalMarketSeriesSnapshot, SeriesTailOperation, StreamRequirements,
};
pub use market_service::descriptors::{
    ConnectMethod, DescriptorError, InstrumentDescriptor, InstrumentRef, InstrumentRequest,
    LoginField, LoginFieldValue, LoginSchema, ProviderDescriptor, ProviderLoginServer,
    ResolvedSeries, SavedProviderLogin, SelectionError, SelectionOutcome, SelectionRequest,
};
pub use market_service::{
    BrokerVenueLink, MarketService, MarketServiceStatus, QuoteSubscription, RithmicLoginSummary,
    RithmicServer, built_in_provider_presentations, published_provider_descriptors,
};
