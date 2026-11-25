//! Provider-neutral Service Provider Interface (SPI) shared by concrete market/broker
//! adapters and the in-process market and trading runtimes.
//!
//! This crate sits in the domain layer and depends only on leaf domain/contract crates,
//! allowing adapters to implement provider authentication, history fetching, catalog
//! discovery, and fault classification without depending on `market_runtime` or
//! `trading_runtime`.

pub mod auth;
pub mod catalog;
pub mod connection;
pub mod fault;
pub mod history;
pub mod instrument;
pub mod open_interest;
pub mod presentation;

pub use auth::{
    LoginField, LoginFieldKind, LoginFieldValue, LoginSchema, PendingAgreement,
    ProviderAuthAdapter, ProviderAuthError, ProviderLoginServer, SavedProviderLogin,
};
pub use catalog::{
    InstrumentSearchCategories, ProviderCatalogRejected, ProviderCatalogRejectionReason,
    ProviderInstrumentKind, ProviderInstrumentSearchResult, ProviderInstrumentSummary,
    ProviderMarketScreen, ProviderMarketStatistics, ScreenProviderMarkets,
    SearchProviderInstruments, SelectProviderInstrument,
};
pub use connection::{
    ConnectionDescriptor, ConnectionId, ConnectionIdError, CredentialSource, DEFAULT_CONNECTION_ID,
};
pub use fault::{ProviderFault, ProviderFaultKind};
pub use history::{
    FormingBar, HistoryContractMetadata, HistoryFetchWindow, HistoryInstrument,
    ProviderHistorySnapshot,
};
pub use instrument::{
    ResolvedContractMetadata, ResolvedInstrumentClass, ResolvedProviderInstrument,
    ResolvedSessionHours,
};
pub use open_interest::{
    MAXIMUM_OPEN_INTEREST_POINTS, OpenInterestRequest, OpenInterestSample, OpenInterestSource,
};
pub use presentation::{
    ProviderAttribution, ProviderCatalogSymbol, ProviderConnectionKind, ProviderDailyBarCadence,
    ProviderFeedLevel, ProviderPresentationDescriptor,
};
