//! Bounded, read-only R|Protocol encoding and decoding.

#[cfg(rithmic_kit)]
#[allow(dead_code, clippy::all, clippy::pedantic)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/rithmic.protobuf.rs"));
}

mod catalog;
mod history;
mod market;
mod protocol;

pub use catalog::{DecodedCatalogMessage, InstrumentReference, SymbolSearchResult};
pub use history::{
    BarIdentity, DecodedHistoryMessage, DecodedTickBar, DecodedTimeBar, DecodedTimeBarType, Ohlc,
    ReplayKind, TickBarKey,
};
pub use market::{
    DecodedMarketMessage, MarketIdentity, OrderBookLevel, OrderBookUpdate, OrderBookUpdateKind,
    ProviderTimestamp, QuoteLevel, QuoteUpdate, TradeAggressor, TradeUpdate,
};
pub use protocol::{
    DecodedControlMessage, InstrumentReferenceRequest, InstrumentType, LoginRequest,
    MarketDataSubscription, OutboundRequest, ProtocolError, ReadOnlyPlant, RithmicKitUnavailable,
    RithmicProtocolBackend, RithmicProtocolCodec, SearchPattern, SensitiveFrame,
    SubscriptionAction, SymbolSearchRequest, TickBarReplayRequest, TickBarSubscription,
    TimeBarReplayRequest, TimeBarSubscription, TimeBarType,
};
