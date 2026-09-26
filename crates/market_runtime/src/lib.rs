//! In-process market runtime.
//!
//! `Aeris` owns provider connections, canonical market state, on-demand
//! history, and realtime fanout inside the desktop process. There is no secondary local process, serialized runtime transport, or restart-replay layer here.

mod hyperliquid_display_depth;
mod hyperliquid_history;
mod hyperliquid_realtime;
pub mod market_service;
mod rithmic_history;
mod rithmic_realtime;
pub mod study;

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

/// Direct in-process order-book image for one consumer generation.
///
/// `display_depth` is an optional provider-aggregated presentation sidecar. It
/// never enters the canonical `OrderBook`; consumers may use it for a coarse
/// DOM while retaining canonical raw depth for correctness and fallback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketOrderBookSnapshot {
    pub consumer_id: aeris_market_engine::ConsumerId,
    pub generation: aeris_market_engine::GenerationId,
    pub publication: aeris_market_data::OrderBookPublication,
    pub display_depth: Option<MarketDisplayDepth>,
}

/// Direct completed provider selection for one runtime consumer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketProviderInstrumentSelection {
    pub consumer_id: aeris_market_engine::ConsumerId,
    pub instrument: aeris_contracts::InstallProviderInstrument,
    pub command_generation: u64,
}

/// Direct generation-fenced readiness state for one canonical series.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketSeriesState {
    pub consumer_id: aeris_market_engine::ConsumerId,
    pub generation: aeris_market_engine::GenerationId,
    pub series: Option<aeris_market_data::BarSeriesKey>,
    pub state: aeris_contracts::SeriesLoadState,
    pub detail: Option<String>,
}

/// Direct structured failure for one canonical series demand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketDemandError {
    pub consumer_id: aeris_market_engine::ConsumerId,
    pub generation: aeris_market_engine::GenerationId,
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
    pub study_ids: Vec<study::StudyInstanceId>,
}

/// Study outputs that became invalid because the instance was reinitialized in
/// place. Definitions/identities remain live; presentation should clear these
/// output series until replacement snapshots arrive.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketStudyOutputsInvalidated {
    pub consumer_id: aeris_market_engine::ConsumerId,
    pub study_ids: Vec<study::StudyInstanceId>,
}

/// Direct in-process market event delivered from the runtime to desktop panes.
///
/// This is an ordinary typed in-process event boundary. The variants are
/// typed Rust values shared in one process; no framing, decoding, request IDs,
/// or process-reconnect replay exists on this boundary.
#[derive(Clone, Debug)]
pub enum MarketRuntimeEvent {
    ProviderState(aeris_contracts::ProviderState),
    SeriesSnapshot(aeris_market_engine::ConsumerPublication),
    SeriesUpdate(aeris_market_engine::ConsumerSeriesUpdate),
    SeriesState(MarketSeriesState),
    DemandError(MarketDemandError),
    OrderBookSnapshot(MarketOrderBookSnapshot),
    StudyOutputSnapshot(MarketStudyOutputSnapshot),
    StudyOutputsInvalidated(MarketStudyOutputsInvalidated),
    StudyRemoved(MarketStudyRemoved),
    PriceAlertTriggered(MarketPriceAlertTrigger),
    ProviderInstrumentSearchResult(aeris_contracts::ProviderInstrumentSearchResult),
    ProviderInstrumentSelection(MarketProviderInstrumentSelection),
    ProviderCatalogRejected(aeris_contracts::ProviderCatalogRejected),
    Fault(aeris_contracts::Fault),
}

pub use aeris_market_engine::{
    ConsumerId as MarketConsumerId, ConsumerPublication as MarketSeriesSnapshot,
    ConsumerResourceClass as MarketConsumerResourceClass,
    ConsumerSeriesUpdate as MarketSeriesUpdate, GenerationId as MarketGenerationId, MarketStream,
    ProviderGeneration as MarketProviderGeneration,
    SeriesSnapshot as CanonicalMarketSeriesSnapshot, SeriesTailOperation, StreamRequirements,
};
pub use market_service::{MarketService, MarketServiceStatus};
