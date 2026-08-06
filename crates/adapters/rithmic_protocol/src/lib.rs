//! Bounded, read-only R|Protocol encoding and decoding.

#[cfg(rithmic_kit)]
#[allow(dead_code, clippy::all, clippy::pedantic)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/rithmic.protobuf.rs"));
}

mod book;
mod catalog;
mod collectors;
mod endpoint;
mod history;
mod market;
mod network;
mod protocol;
mod session;
#[cfg(test)]
mod session_tests;

pub use book::{
    AggregateBookAssembler, AggregateBookConfigError, AggregateBookImage, AggregateBookLimits,
    AggregateBookOutcome, AggregateBookRecoveryReason,
};
pub use catalog::{DecodedCatalogMessage, InstrumentReference, SymbolSearchResult};
pub use collectors::{
    CollectedHistory, CollectedSymbols, CollectionProgress, CollectorError, HistoryBars,
    HistoryCollectionRequest, HistoryCollector, HistorySeries, ObservedHistoryRange,
    SymbolSearchCollectionRequest, SymbolSearchCollector,
};
pub use endpoint::{RetryDisposition, RithmicSessionError, RithmicSessionLimits};
pub use history::{
    BarIdentity, DecodedHistoryMessage, DecodedTickBar, DecodedTimeBar, DecodedTimeBarType,
    HistorySource, Ohlc, ReplayKind, TickBarKey,
};
pub use market::{
    DecodedMarketMessage, MarketIdentity, OrderBookLevel, OrderBookSides, OrderBookUpdate,
    OrderBookUpdateKind, ProviderTimestamp, QuoteLevel, QuoteUpdate, TradeAggressor, TradeUpdate,
};
pub use protocol::{
    DecodedControlMessage, InstrumentReferenceRequest, InstrumentType, LoginRequest,
    MarketDataSubscription, OutboundRequest, ProtocolError, ReadOnlyPlant, RithmicKitUnavailable,
    RithmicProtocolBackend, RithmicProtocolCodec, SearchPattern, SensitiveFrame,
    SubscriptionAction, SymbolSearchRequest, TickBarReplayRequest, TickBarSubscription,
    TimeBarReplayRequest, TimeBarSubscription, TimeBarType,
};
pub use session::{
    RithmicApplication, RithmicCredentials, RithmicHistoryConnection, RithmicSessionMessage,
    RithmicTestSession, RithmicTickerConnection,
};
