//! In-process market runtime.
//!
//! Axiusflow owns provider connections, canonical market state, on-demand
//! history, and realtime fanout inside the desktop process. There is no local
//! socket, resident process, transport protocol, or restart-replay layer here.

mod hyperliquid_history;
mod hyperliquid_realtime;
pub mod market_service;
mod rithmic_history;
mod rithmic_realtime;

/// Direct in-process order-book image for one consumer generation.
///
/// The canonical depth publication is owned and validated by the market-data
/// domain. Consumer and demand generation are the only routing metadata added
/// here; display precision and UI selection identity remain presentation state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketOrderBookSnapshot {
    pub consumer_id: axiusflow_market_engine::ConsumerId,
    pub generation: axiusflow_market_engine::GenerationId,
    pub publication: axiusflow_market_data::OrderBookPublication,
}

/// Direct completed provider selection for one runtime consumer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketProviderInstrumentSelection {
    pub consumer_id: axiusflow_market_engine::ConsumerId,
    pub instrument: axiusflow_engine_protocol::InstallProviderInstrument,
    pub command_generation: u64,
}

/// Direct generation-fenced readiness state for one canonical series.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketSeriesState {
    pub consumer_id: axiusflow_market_engine::ConsumerId,
    pub generation: axiusflow_market_engine::GenerationId,
    pub series: Option<axiusflow_market_data::BarSeriesKey>,
    pub state: axiusflow_engine_protocol::SeriesLoadState,
    pub detail: Option<String>,
}

/// Direct structured failure for one canonical series demand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketDemandError {
    pub consumer_id: axiusflow_market_engine::ConsumerId,
    pub generation: axiusflow_market_engine::GenerationId,
    pub code: axiusflow_engine_protocol::EngineFaultCode,
    pub stage: axiusflow_engine_protocol::FailureStage,
    pub detail: String,
    pub series: Option<axiusflow_market_data::BarSeriesKey>,
    pub cause: String,
    pub elapsed_millis: Option<u64>,
}

/// Direct in-process market event delivered from the runtime to desktop panes.
///
/// This deliberately is not a transport envelope. The variants are ordinary
/// typed Rust values shared in one process; no framing, decoding, request IDs,
/// or reconnect replay exists on this boundary.
#[derive(Clone, Debug)]
pub enum MarketRuntimeEvent {
    ProviderState(axiusflow_engine_protocol::ProviderState),
    SeriesSnapshot(axiusflow_market_engine::ConsumerPublication),
    SeriesUpdate(axiusflow_market_engine::ConsumerSeriesUpdate),
    SeriesState(MarketSeriesState),
    DemandError(MarketDemandError),
    OrderBookSnapshot(MarketOrderBookSnapshot),
    ProviderInstrumentSearchResult(axiusflow_engine_protocol::ProviderInstrumentSearchResult),
    ProviderInstrumentSelection(MarketProviderInstrumentSelection),
    ProviderCatalogRejected(axiusflow_engine_protocol::ProviderCatalogRejected),
    Fault(axiusflow_engine_protocol::Fault),
}

pub use market_service::{MarketService, MarketServiceStatus};
pub use axiusflow_market_engine::{
    ConsumerId as MarketConsumerId, ConsumerPublication as MarketSeriesSnapshot,
    ConsumerResourceClass as MarketConsumerResourceClass, ConsumerSeriesUpdate as MarketSeriesUpdate,
    GenerationId as MarketGenerationId, MarketStream, ProviderGeneration as MarketProviderGeneration,
    SeriesSnapshot as CanonicalMarketSeriesSnapshot, SeriesTailOperation, StreamRequirements,
};
