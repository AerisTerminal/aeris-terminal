//! Provider-neutral plugin contract for composing concrete market providers into
//! [`crate::MarketServiceBuilder`].
//!
//! External provider composition crates (such as `aeris_market_providers`) depend
//! on this surface to register descriptors, history fetchers, realtime/catalog
//! workers, auth adapters, and broker venue runners without `aeris_market_runtime`
//! depending on any concrete provider adapter crate.

pub use super::calendar::CalendarBarStamp;
pub use super::clock::{Clock, SystemClock};
pub use super::descriptors::ProviderDescriptor as PublishedProviderDescriptor;
pub use super::lifecycle::RecoveryReason;
pub use super::provider_auth::ProviderAuthAdapter;
pub use super::provider_event::{
    ProviderCandleSymbol, ProviderDisconnect, ProviderEvent, ProviderEventKind, ProviderFault,
    ProviderFaultKind, ProviderTradeBatch, WireProviderEvent,
};
pub use super::provider_session::{
    ProviderEventSink, ProviderSession, ProviderSessionError, try_control,
};
pub use super::reconnect::{Backoff, Jitter, ReconnectPolicy, RetryClass, RetryDecision};
pub use super::{
    ActiveWorkerGuard, AlertDemandUpdate, COMMAND_CAPACITY, CandleDemandPolicy, CatalogPublisher,
    CatalogSessionBinding, ConcurrencyLimits, DemandFlushPolicy, DepthCapabilities,
    DisplayDepthRequest, HistoryRequest, HistorySnapshot, HistorySource, LiveModel,
    ProviderCapabilities, ProviderCatalogChannel, ProviderCatalogChannelImpl,
    ProviderCatalogChannels, ProviderCatalogCommand, ProviderCatalogEvent, ProviderCoordinatorWake,
    ProviderDemand, ProviderDescriptor, ProviderDisplayDepthChannels, ProviderDisplayDepthEvent,
    ProviderHistoryCapabilities, ProviderInstrumentDemand, ProviderRealtimeChannels,
    ProviderRecoveryPolicy, ProviderRuntimeLifecycle, ProviderRuntimeRecord, ProviderRuntimeSpec,
    ProviderStartHandle, QuoteCapability, REALTIME_CAPACITY, StartedProviderRuntime,
    TradeDemandPolicy, history_instrument_from_install, recording_replay_descriptor,
    try_send_catalog_control,
};

/// Projects a registered [`ProviderDescriptor`] into the desktop-facing
/// [`PublishedProviderDescriptor`].
#[must_use]
pub fn published_descriptor_from_registration(
    descriptor: &ProviderDescriptor,
) -> PublishedProviderDescriptor {
    PublishedProviderDescriptor::from_registration(descriptor)
}
