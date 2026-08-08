//! Bounded, read-only R|Protocol encoding and decoding.

#[cfg(rithmic_kit)]
#[allow(dead_code, clippy::all, clippy::pedantic)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/rithmic.protobuf.rs"));
}

mod book;
mod catalog;
mod collectors;
mod credentials;
mod desktop_driver;
mod endpoint;
mod history;
mod history_adapter;
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
pub use credentials::{
    MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES, MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
    RithmicCredentialBytes, RithmicCredentialError,
};
pub use desktop_driver::{
    AppliedRithmicEvent, RITHMIC_TEST_VAULT_KEY, RITHMIC_TEST_VAULT_SERVICE, RithmicCallbackLimits,
    RithmicCatalogCallback, RithmicCatalogEvent, RithmicCatalogRejection, RithmicDesktopEventError,
    RithmicEnvironmentEvent, RithmicInstrumentSelection, RithmicProviderCallback,
    RithmicProviderCommandError, RithmicProviderConfig, RithmicProviderConfigError,
    RithmicProviderDriver, RithmicProviderDriverError, RithmicProviderEvents,
    RithmicProviderInstrument, RithmicReadOnlySubscription, RithmicRetryScheduler,
    RithmicRetryTicket, RithmicSymbolSearch, apply_rithmic_environment_event,
    try_recv_rithmic_event,
};
pub use endpoint::{RetryDisposition, RithmicSessionError, RithmicSessionLimits};
pub use history::{
    BarIdentity, DecodedHistoryMessage, DecodedTickBar, DecodedTimeBar, DecodedTimeBarType,
    HistorySource, Ohlc, ReplayKind, TickBarKey,
};
pub use history_adapter::{
    CanonicalRithmicTickBar, RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID, RithmicBarContinuity,
    RithmicHistoryAdapterError, RithmicHistoryCapabilityAdapter, RithmicHistoryLimits,
    RithmicHistorySessionTransport, RithmicHistoryTransport, RithmicTimeBarResolution,
    canonical_rithmic_tick_bar, canonical_rithmic_time_bar, covering_snapshot_from_page,
    decode_rithmic_history_bar,
};
pub use market::{
    DecodedMarketMessage, MarketIdentity, OrderBookLevel, OrderBookSides, OrderBookUpdate,
    OrderBookUpdateKind, ProviderTimestamp, QuoteLevel, QuoteSideUpdate, QuoteUpdate,
    TradeAggressor, TradeUpdate,
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
