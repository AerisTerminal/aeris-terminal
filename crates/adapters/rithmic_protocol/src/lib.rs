//! Bounded R|Protocol encoding, decoding, and plant sessions for market data,
//! history, orders, and P&L.

/// Canonical application identity registered with Rithmic and sent in every
/// R|Protocol login. Keep this as the single source of truth so production,
/// smoke tools, and protocol fixtures cannot drift in capitalization.
pub const RITHMIC_APPLICATION_NAME: &str = "Aeris";

#[cfg(rithmic_kit)]
#[allow(dead_code, clippy::all, clippy::pedantic)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/rithmic.protobuf.rs"));
}

mod book;
mod calendar;
mod catalog;
mod collectors;
mod credentials;
mod decimal;
mod endpoint;
mod history;
mod history_adapter;
mod market;
mod network;
mod order_plant;
mod order_session;
mod plant_common;
mod pnl_plant;
mod protocol;
mod provider_runtime;
mod provider_session;
mod session;
mod session_contract;
#[cfg(test)]
mod session_tests;

pub use book::{
    AggregateBookAssembler, AggregateBookConfigError, AggregateBookImage, AggregateBookLimits,
    AggregateBookOutcome, AggregateBookRecoveryReason,
};
pub use calendar::{RithmicCalendarBucket, RithmicCalendarPeriod, RithmicExchangeCalendar};
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
pub use decimal::{MAXIMUM_RITHMIC_DECIMAL_SCALE, RithmicDecimal};
pub use endpoint::{RetryDisposition, RithmicSessionError, RithmicSessionLimits};
pub use history::{
    BarIdentity, DecodedHistoryMessage, DecodedTickBar, DecodedTimeBar, DecodedTimeBarType,
    HistorySource, Ohlc, ReplayKind, TickBarKey,
};
pub use history_adapter::{
    RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID, RithmicBarContinuity, RithmicCoveringRecoveryEvidence,
    RithmicHistoryAdapterError, RithmicHistoryCapabilityAdapter, RithmicHistoryLimits,
    RithmicHistorySessionTransport, RithmicHistoryTransport, RithmicTimeBarResolution,
    canonical_rithmic_tick_bar, canonical_rithmic_time_bar, collect_rithmic_chart_history,
    collect_rithmic_covering_recovery_evidence, collect_rithmic_trade_history,
    covering_snapshot_from_page, decode_rithmic_history_bar,
};
pub use market::{
    DecodedMarketMessage, DepthByOrderEndEvent, DepthByOrderMutation, DepthByOrderMutationKind,
    DepthByOrderSide, DepthByOrderSnapshotLevel, DepthByOrderSnapshotMessage,
    DepthByOrderSnapshotOrder, DepthByOrderUpdate, MarketIdentity, OrderBookLevel, OrderBookSides,
    OrderBookUpdate, OrderBookUpdateKind, ProviderTimestamp, QuoteLevel, QuoteSideUpdate,
    QuoteUpdate, TradeAggressor, TradeUpdate,
};
pub use order_plant::{
    AccountListRequest, CancelAllOrdersRequest, CancelOrderRequest, DecodedOrderMessage,
    ExecutionReplayRequest, FillHistoryRequest, MAXIMUM_FILL_HISTORY_RECORDS,
    MAXIMUM_FILL_HISTORY_WINDOW_SECONDS, ModifyOrderRequest, NewOrderRequest, OrderPlantRequest,
    RithmicAccount, RithmicExchangeNotifyType, RithmicExchangeOrderNotification, RithmicFill,
    RithmicFillHistoryRow, RithmicLoginInfo, RithmicOrderCommandReply, RithmicOrderDetails,
    RithmicOrderDuration, RithmicOrderNotification, RithmicOrderNotifyType, RithmicOrderPlacement,
    RithmicOrderSide, RithmicOrderType, RithmicReportedPriceType, RithmicReportedSide,
    RithmicRequestCompletion, RithmicTradeRoute, RithmicUserType, TradeRoutesRequest,
};
pub use order_session::{
    MAXIMUM_STREAM_START_MESSAGES, MAXIMUM_TRACKED_RITHMIC_ACCOUNTS, RithmicOrderConnection,
    RithmicOrderPlantMessage, RithmicPnlConnection, RithmicPnlPlantMessage,
};
pub use plant_common::{
    RithmicAccountKey, RithmicAccountRef, RithmicRequestKind, RithmicRequestOutcome,
};
pub use pnl_plant::{
    DecodedPnlMessage, PnlPlantRequest, PnlPositionUpdatesRequest, RithmicAccountPnl,
    RithmicInstrumentPnl, RithmicPositionQuantities,
};
pub use protocol::{
    DecodedControlMessage, DepthByOrderSnapshotRequest, DepthByOrderSubscription,
    InstrumentReferenceRequest, InstrumentType, LoginRequest, MarketDataSubscription,
    OutboundRequest, ProtocolError, RithmicKitUnavailable, RithmicPlant, RithmicProtocolBackend,
    RithmicProtocolCodec, SearchPattern, SensitiveFrame, SubscriptionAction, SymbolSearchRequest,
    TickBarReplayRequest, TickBarSubscription, TimeBarReplayRequest, TimeBarSubscription,
    TimeBarType,
};
pub use provider_runtime::{
    ConnectTrigger, NetworkEvent, ProviderCredentialRequirement, ProviderSessionDriver,
    RecoveryReason, RithmicProviderRuntime, RithmicProviderRuntimeConfig,
    RithmicProviderRuntimeError, RithmicProviderRuntimeState, SessionGeneration,
};
pub use provider_session::{
    AppliedRithmicEvent, MAXIMUM_RITHMIC_SEARCH_RESULTS, RITHMIC_TEST_VAULT_KEY,
    RITHMIC_TEST_VAULT_SERVICE, RithmicAuthorizedSilenceEvidenceFault, RithmicCallbackLimits,
    RithmicCatalogCallback, RithmicCatalogEvent, RithmicCatalogRejection, RithmicEnvironmentEvent,
    RithmicInstrumentSelection, RithmicProviderCallback, RithmicProviderCommandError,
    RithmicProviderConfig, RithmicProviderConfigError, RithmicProviderDriver,
    RithmicProviderDriverError, RithmicProviderEventError, RithmicProviderEvents,
    RithmicProviderInstrument, RithmicReadOnlySubscription, RithmicRetryScheduler,
    RithmicRetryTicket, RithmicSessionTiming, RithmicSymbolSearch, apply_rithmic_environment_event,
    try_recv_rithmic_event,
};
pub use session::{
    RithmicApplication, RithmicCredentials, RithmicHistoryConnection, RithmicLoginMetadata,
    RithmicSessionMessage, RithmicTestSession, RithmicTickerConnection,
};
pub use session_contract::{
    AuthenticationState, InstrumentContractMetadata, InstrumentDescriptor,
    MAXIMUM_DISCOVERY_FIELD_BYTES, ProviderContractError, ProviderEnvironment,
    ProviderInvalidationReason, ProviderSessionCommand, ProviderSessionEvent, ProviderSubscription,
};
