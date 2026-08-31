//! Headless single-owner market state for the resident Axiusflow engine.
//!
//! This crate contains no GPUI, IPC serialization, provider sockets, storage,
//! threads, or global mutable state. The process shell owns one `MarketEngine`
//! and passes ordinary typed commands and canonical provider results into it.

mod demand;
mod hot_set;
mod order_flow;
mod provider_manager;
mod publication;
mod resource_policy;
mod series_store;
mod subscription_registry;

pub use demand::{
    ConsumerDemand, ConsumerIdentity, ConsumerResourceClass, MarketStream, StreamRequirements,
};
pub use hot_set::{HotSetDescriptor, HotSetEntry, HotSetManager, HotSetRetention, HotSetTier};
pub use order_flow::{
    ConsumerOrderFlowPublication, OrderFlowLevel, OrderFlowPublicationKind, OrderFlowSnapshot,
    OrderFlowTrade, OrderFlowUpdate,
};
pub use provider_manager::{
    ProviderCapabilities, ProviderConfig, ProviderHealth, ProviderRequest, ProviderStatus,
};
pub use publication::{ConsumerPublication, ConsumerSeriesUpdate};
pub use resource_policy::{
    EngineResourceMode, ResourcePolicyDecision, ResourcePolicyInput,
    decide as decide_resource_policy,
};
pub use series_store::{SeriesSnapshot, SeriesTailOperation};
pub use subscription_registry::SubscriptionStatus;

use axiusflow_market_data::{BarSeriesKey, MarketBar, MarketDataValidationError, MarketTrade};
use demand::DemandRegistry;
use provider_manager::ProviderManager;
use publication::PublicationManager;
use series_store::SeriesStore;
use std::{
    error::Error,
    fmt,
    num::{NonZeroU64, NonZeroUsize},
};
use subscription_registry::SubscriptionRegistry;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClientId(pub NonZeroU64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceId(pub NonZeroU64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ConsumerId(pub NonZeroU64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GenerationId(pub NonZeroU64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProviderGeneration(pub NonZeroU64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Viewport {
    pub start_unix_nanos: i64,
    pub end_unix_nanos: i64,
}

impl Viewport {
    /// Creates a non-empty visible range.
    ///
    /// # Errors
    /// Returns [`EngineError::InvalidViewport`] unless start is before end.
    pub fn try_new(start_unix_nanos: i64, end_unix_nanos: i64) -> Result<Self, EngineError> {
        if start_unix_nanos >= end_unix_nanos {
            return Err(EngineError::InvalidViewport);
        }
        Ok(Self {
            start_unix_nanos,
            end_unix_nanos,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarketEngineConfig {
    pub maximum_consumers: NonZeroUsize,
    pub maximum_series: NonZeroUsize,
    pub maximum_bars: NonZeroUsize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarketEngineMetrics {
    pub active_consumers: usize,
    pub registered_providers: usize,
    pub active_subscriptions: usize,
    pub stored_series: usize,
    pub stored_bars: usize,
    pub approximate_series_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineError {
    InvalidProviderIdentity,
    InvalidProviderReconnectPolicy,
    DuplicateProvider(String),
    UnknownProvider(String),
    StaleProviderGeneration {
        current: Option<ProviderGeneration>,
        received: ProviderGeneration,
    },
    ProviderSessionUnavailable(String),
    UnsupportedProviderRequest {
        provider: String,
        request: ProviderRequest,
    },
    DuplicateConsumer(ConsumerId),
    UnknownConsumer(ConsumerId),
    ConsumerHasNoSeries(ConsumerId),
    ConsumerLimitExceeded {
        maximum: NonZeroUsize,
    },
    EmptyStreamRequirements,
    StaleConsumerGeneration {
        consumer_id: ConsumerId,
        current: GenerationId,
        received: GenerationId,
    },
    InvalidViewport,
    InvalidHotSeries,
    DuplicateHotSeries,
    UnknownHotSeries,
    HotSetPinnedLimitExceeded {
        maximum: NonZeroUsize,
    },
    InvalidSeriesPrecision,
    EmptySeries,
    DiscontinuousSeries {
        expected: u64,
        received: u64,
    },
    NonIncreasingSeriesTime,
    SeriesLimitExceeded {
        maximum: NonZeroUsize,
    },
    BarLimitExceeded {
        maximum: NonZeroUsize,
        requested: usize,
    },
    StaleSeriesGeneration {
        current: ProviderGeneration,
        received: ProviderGeneration,
    },
    ConflictingSeriesGeneration(ProviderGeneration),
    CapacityOverflow,
    OrderFlowIdentityMismatch,
    NonIncreasingOrderFlowSequence,
    OrderFlowLevelLimitExceeded {
        maximum: usize,
    },
    OrderFlowHistoryLimitExceeded {
        maximum: usize,
    },
    InvalidMarketData(MarketDataValidationError),
}

impl From<MarketDataValidationError> for EngineError {
    fn from(error: MarketDataValidationError) -> Self {
        Self::InvalidMarketData(error)
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProviderIdentity => formatter.write_str("provider identity is invalid"),
            Self::InvalidProviderReconnectPolicy => {
                formatter.write_str("provider reconnect policy is invalid")
            }
            Self::DuplicateProvider(provider) => {
                write!(formatter, "provider {provider} is already registered")
            }
            Self::UnknownProvider(provider) => {
                write!(formatter, "provider {provider} is not registered")
            }
            Self::StaleProviderGeneration { .. } => {
                formatter.write_str("provider generation is stale")
            }
            Self::ProviderSessionUnavailable(provider) => {
                write!(formatter, "provider {provider} has no active session")
            }
            Self::UnsupportedProviderRequest { provider, request } => {
                write!(
                    formatter,
                    "provider {provider} does not support {request:?}"
                )
            }
            Self::DuplicateConsumer(consumer) => {
                write!(formatter, "consumer {} is already registered", consumer.0)
            }
            Self::UnknownConsumer(consumer) => {
                write!(formatter, "consumer {} is not registered", consumer.0)
            }
            Self::ConsumerHasNoSeries(consumer) => {
                write!(formatter, "consumer {} has no series demand", consumer.0)
            }
            Self::ConsumerLimitExceeded { maximum } => {
                write!(formatter, "consumer limit {maximum} exceeded")
            }
            Self::EmptyStreamRequirements => {
                formatter.write_str("consumer demand must request at least one stream")
            }
            Self::StaleConsumerGeneration { .. } => {
                formatter.write_str("consumer generation is stale")
            }
            Self::InvalidViewport => formatter.write_str("viewport start must precede end"),
            Self::InvalidHotSeries => formatter.write_str("hot-set series metadata is invalid"),
            Self::DuplicateHotSeries => {
                formatter.write_str("hot-set series identity is duplicated")
            }
            Self::UnknownHotSeries => formatter.write_str("hot-set series identity is unknown"),
            Self::HotSetPinnedLimitExceeded { maximum } => {
                write!(formatter, "pinned hot-set entries exceed bound {maximum}")
            }
            Self::InvalidSeriesPrecision => {
                formatter.write_str("series decimal precision exceeds 18 places")
            }
            Self::EmptySeries => formatter.write_str("series snapshot must contain bars"),
            Self::DiscontinuousSeries { expected, received } => write!(
                formatter,
                "series sequence gap: expected {expected}, received {received}"
            ),
            Self::NonIncreasingSeriesTime => formatter.write_str("series timestamps must increase"),
            Self::SeriesLimitExceeded { maximum } => {
                write!(formatter, "series limit {maximum} exceeded")
            }
            Self::BarLimitExceeded { maximum, requested } => write!(
                formatter,
                "bar limit {maximum} exceeded by requested total {requested}"
            ),
            Self::StaleSeriesGeneration { .. } => {
                formatter.write_str("series provider generation is stale")
            }
            Self::ConflictingSeriesGeneration(_) => {
                formatter.write_str("one provider generation produced conflicting series snapshots")
            }
            Self::CapacityOverflow => {
                formatter.write_str("market engine capacity arithmetic overflowed")
            }
            Self::OrderFlowIdentityMismatch => {
                formatter.write_str("order-flow trade identity does not match its series")
            }
            Self::NonIncreasingOrderFlowSequence => {
                formatter.write_str("order-flow source sequence did not increase")
            }
            Self::OrderFlowLevelLimitExceeded { maximum } => {
                write!(formatter, "order-flow level limit {maximum} exceeded")
            }
            Self::OrderFlowHistoryLimitExceeded { maximum } => {
                write!(
                    formatter,
                    "order-flow reconstruction limit {maximum} exceeded"
                )
            }
            Self::InvalidMarketData(error) => {
                write!(formatter, "canonical market data is invalid: {error}")
            }
        }
    }
}

impl Error for EngineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidMarketData(error) => Some(error),
            _ => None,
        }
    }
}

pub struct MarketEngine {
    demands: DemandRegistry,
    providers: ProviderManager,
    series: SeriesStore,
    subscriptions: SubscriptionRegistry,
    publications: PublicationManager,
    order_flow: order_flow::OrderFlowStore,
}

impl MarketEngine {
    #[must_use]
    pub fn new(config: MarketEngineConfig) -> Self {
        Self {
            demands: DemandRegistry::new(config.maximum_consumers),
            providers: ProviderManager::new(),
            series: SeriesStore::new(config.maximum_series, config.maximum_bars),
            subscriptions: SubscriptionRegistry::new(),
            publications: PublicationManager::new(),
            order_flow: order_flow::OrderFlowStore::new(),
        }
    }

    /// Registers one provider capability boundary.
    ///
    /// # Errors
    /// Returns an error for invalid or duplicate provider identity.
    pub fn register_provider(
        &mut self,
        provider: String,
        config: ProviderConfig,
    ) -> Result<(), EngineError> {
        self.providers.register(provider, config)
    }

    /// Begins a strictly newer provider session generation.
    ///
    /// # Errors
    /// Returns an error for an unknown provider or stale generation.
    pub fn begin_provider_session(
        &mut self,
        provider: &str,
        generation: ProviderGeneration,
    ) -> Result<(), EngineError> {
        self.providers.begin_session(provider, generation)
    }

    /// Applies health only to the exact active provider generation.
    ///
    /// # Errors
    /// Returns an error for an unknown provider or stale generation.
    pub fn set_provider_health(
        &mut self,
        provider: &str,
        generation: ProviderGeneration,
        health: ProviderHealth,
    ) -> Result<(), EngineError> {
        self.providers.set_health(provider, generation, health)
    }

    /// Ends the exact active provider session without discarding its generation fence.
    ///
    /// # Errors
    /// Returns an error for an unknown provider or stale generation.
    pub fn end_provider_session(
        &mut self,
        provider: &str,
        generation: ProviderGeneration,
    ) -> Result<(), EngineError> {
        self.providers.end_session(provider, generation)
    }

    /// Verifies that the configured provider session can route one request class.
    ///
    /// # Errors
    /// Returns an error for an unknown, inactive, or incapable provider session.
    pub fn verify_provider_request(
        &self,
        provider: &str,
        request: ProviderRequest,
    ) -> Result<(), EngineError> {
        self.providers.verify_request(provider, request)
    }

    #[must_use]
    pub fn provider_status(&self, provider: &str) -> Option<ProviderStatus> {
        self.providers.status(provider)
    }

    #[must_use]
    pub fn provider_account_id(&self, provider: &str) -> Option<&str> {
        self.providers.account_id(provider)
    }

    #[must_use]
    pub fn provider_reconnect_delay(&self, provider: &str) -> Option<std::time::Duration> {
        self.providers.reconnect_delay(provider)
    }

    /// Registers one independently generated chart or DOM consumer.
    ///
    /// # Errors
    /// Returns an error for duplicate identity or the configured consumer bound.
    pub fn register_consumer(
        &mut self,
        identity: ConsumerIdentity,
        visible: bool,
    ) -> Result<(), EngineError> {
        self.register_consumer_with_class(
            identity,
            if visible {
                ConsumerResourceClass::Foreground
            } else {
                ConsumerResourceClass::Background
            },
        )
    }

    /// Registers one independently generated consumer with an exact resource class.
    ///
    /// # Errors
    /// Returns an error for duplicate identity or the configured consumer bound.
    pub fn register_consumer_with_class(
        &mut self,
        identity: ConsumerIdentity,
        resource_class: ConsumerResourceClass,
    ) -> Result<(), EngineError> {
        self.demands.register(identity, resource_class)
    }

    /// Makes a newer series generation authoritative immediately.
    ///
    /// A memory hit returns a covering publication without provider work.
    ///
    /// # Errors
    /// Returns an error for invalid series, unknown consumer, or stale generation.
    pub fn set_series_demand(
        &mut self,
        consumer_id: ConsumerId,
        generation: GenerationId,
        series: &BarSeriesKey,
    ) -> Result<Option<ConsumerPublication>, EngineError> {
        self.set_series_demand_with_streams(
            consumer_id,
            generation,
            series,
            StreamRequirements::BARS,
        )
    }

    /// Makes a newer series and its explicit upstream stream requirements authoritative.
    ///
    /// # Errors
    /// Returns an error for invalid series, unsupported streams, unknown consumer, or stale generation.
    pub fn set_series_demand_with_streams(
        &mut self,
        consumer_id: ConsumerId,
        generation: GenerationId,
        series: &BarSeriesKey,
        streams: StreamRequirements,
    ) -> Result<Option<ConsumerPublication>, EngineError> {
        self.providers
            .verify_streams(&series.provider_id, streams)?;
        let previous = self.demands.current(consumer_id).and_then(|demand| {
            demand
                .series
                .as_ref()
                .zip(demand.streams)
                .map(|(series, streams)| (series.clone(), streams))
        });
        let changed = self
            .demands
            .set_series(consumer_id, generation, series.clone(), streams)?;
        if changed {
            let resource_class = self
                .demands
                .current(consumer_id)
                .map(|demand| demand.resource_class)
                .ok_or(EngineError::UnknownConsumer(consumer_id))?;
            if resource_class.retains_subscription() {
                self.subscriptions.replace(
                    consumer_id,
                    previous
                        .as_ref()
                        .map(|(series, streams)| (series, *streams)),
                    series,
                    streams,
                );
            } else if let Some((previous_series, _)) = previous.as_ref() {
                self.subscriptions.remove(consumer_id, previous_series);
            }
            self.publications.remove(consumer_id);
            self.order_flow.remove_consumer(consumer_id);
        }
        if !self
            .demands
            .current(consumer_id)
            .is_some_and(|demand| demand.resource_class.publishes_ui())
        {
            return Ok(None);
        }
        self.series.get(series).map_or(Ok(None), |snapshot| {
            self.publications
                .publish(consumer_id, generation, snapshot)
                .map(Some)
        })
    }

    /// Updates viewport demand only for the current consumer generation.
    ///
    /// # Errors
    /// Returns an error for unknown consumer or stale generation.
    pub fn set_viewport(
        &mut self,
        consumer_id: ConsumerId,
        generation: GenerationId,
        viewport: Viewport,
    ) -> Result<(), EngineError> {
        self.demands.set_viewport(consumer_id, generation, viewport)
    }

    /// Updates resource priority without recreating market state.
    ///
    /// # Errors
    /// Returns an error for an unknown consumer.
    pub fn set_visibility(
        &mut self,
        consumer_id: ConsumerId,
        visible: bool,
    ) -> Result<(), EngineError> {
        self.set_resource_class(
            consumer_id,
            if visible {
                ConsumerResourceClass::Foreground
            } else {
                ConsumerResourceClass::Background
            },
        )
        .map(|_| ())
    }

    /// Applies a resource-class transition without recreating provider state.
    ///
    /// Background consumers retain shared upstream demand without UI publication.
    /// Warm and detached consumers also release the upstream subscription. Re-entering
    /// foreground publishes a cached covering snapshot immediately.
    ///
    /// # Errors
    /// Returns an error for an unknown consumer or publication generation overflow.
    pub fn set_resource_class(
        &mut self,
        consumer_id: ConsumerId,
        resource_class: ConsumerResourceClass,
    ) -> Result<Option<ConsumerPublication>, EngineError> {
        let previous = self
            .demands
            .current(consumer_id)
            .cloned()
            .ok_or(EngineError::UnknownConsumer(consumer_id))?;
        if !self
            .demands
            .set_resource_class(consumer_id, resource_class)?
        {
            return Ok(None);
        }
        let Some(series) = previous.series.as_ref() else {
            return Ok(None);
        };
        let streams = previous.streams.unwrap_or(StreamRequirements::NONE);
        if previous.resource_class.retains_subscription() && !resource_class.retains_subscription()
        {
            self.subscriptions.remove(consumer_id, series);
        } else if !previous.resource_class.retains_subscription()
            && resource_class.retains_subscription()
            && !streams.is_empty()
        {
            self.subscriptions
                .replace(consumer_id, None, series, streams);
        }
        if !resource_class.publishes_ui() {
            self.publications.suspend(consumer_id);
            self.order_flow.suspend_consumer(consumer_id);
            return Ok(None);
        }
        let Some(generation) = previous.generation else {
            return Ok(None);
        };
        self.series.get(series).map_or(Ok(None), |snapshot| {
            self.publications
                .publish(consumer_id, generation, snapshot)
                .map(Some)
        })
    }

    /// Installs one canonical covering snapshot and publishes it to every current matching demand.
    ///
    /// Valid results remain useful even if the requesting consumer has since moved on.
    ///
    /// # Errors
    /// Returns an error for provider-generation, canonical-data, sequence, or capacity failure.
    pub fn install_history(
        &mut self,
        provider_generation: ProviderGeneration,
        series: &BarSeriesKey,
        price_scale: u8,
        quantity_scale: u8,
        bars: Vec<MarketBar>,
    ) -> Result<Vec<ConsumerPublication>, EngineError> {
        self.providers
            .verify_generation(&series.provider_id, provider_generation)?;
        self.providers
            .verify_request(&series.provider_id, ProviderRequest::HistoricalBars)?;
        let bars = bars.into_boxed_slice();
        let snapshot = self.series.install(
            series.clone(),
            provider_generation,
            price_scale,
            quantity_scale,
            &bars,
        )?;
        self.publish_snapshot(series, &snapshot)
    }

    /// Replaces a canonical series with one validated covering image.
    ///
    /// Unlike ordinary history installation this permits an interior timestamp
    /// repair and keeps consumer publication generations monotonic. With
    /// `publish` set to false the image installs silently, so a background
    /// working-window fill reaches consumers through one later
    /// [`MarketEngine::publish_series_snapshot`] step instead of per page.
    ///
    /// # Errors
    /// Returns an error for stale provider generation, precision, canonical
    /// data, or configured capacity.
    pub fn replace_covering_history(
        &mut self,
        provider_generation: ProviderGeneration,
        series: &BarSeriesKey,
        price_scale: u8,
        quantity_scale: u8,
        bars: Vec<MarketBar>,
        publish: bool,
    ) -> Result<Vec<ConsumerPublication>, EngineError> {
        self.providers
            .verify_generation(&series.provider_id, provider_generation)?;
        self.providers
            .verify_request(&series.provider_id, ProviderRequest::HistoricalBars)?;
        let bars = bars.into_boxed_slice();
        let snapshot = self.series.replace_covering(
            series,
            provider_generation,
            price_scale,
            quantity_scale,
            &bars,
        )?;
        if publish {
            self.publish_snapshot(series, &snapshot)
        } else {
            Ok(Vec::new())
        }
    }

    /// Publishes the current covering snapshot of one installed series to every
    /// matching consumer; a series without installed history publishes nothing.
    ///
    /// # Errors
    /// Returns an error for publication-manager capacity failure.
    pub fn publish_series_snapshot(
        &mut self,
        series: &BarSeriesKey,
    ) -> Result<Vec<ConsumerPublication>, EngineError> {
        match self.series.get(series) {
            Some(snapshot) => self.publish_snapshot(series, &snapshot),
            None => Ok(Vec::new()),
        }
    }

    /// Installs validated local history before a provider session is available.
    ///
    /// Retained data keeps its historical provenance generation and remains usable
    /// while a fresh provider session is established and covering repair runs.
    ///
    /// # Errors
    /// Returns an error for an unknown provider, invalid data, or configured capacity exhaustion.
    pub fn install_retained_history(
        &mut self,
        provider_generation: ProviderGeneration,
        series: &BarSeriesKey,
        price_scale: u8,
        quantity_scale: u8,
        bars: Vec<MarketBar>,
    ) -> Result<Vec<ConsumerPublication>, EngineError> {
        if self.providers.status(&series.provider_id).is_none() {
            return Err(EngineError::UnknownProvider(series.provider_id.clone()));
        }
        let bars = bars.into_boxed_slice();
        let snapshot = self.series.install(
            series.clone(),
            provider_generation,
            price_scale,
            quantity_scale,
            &bars,
        )?;
        self.publish_snapshot(series, &snapshot)
    }

    /// Installs one bounded live covering image and publishes it to matching demand.
    ///
    /// The newest forming bar may be revised within the same provider generation;
    /// completed overlap remains immutable.
    ///
    /// # Errors
    /// Returns an error for stale generation, invalid continuity, precision, or capacity.
    pub fn install_realtime(
        &mut self,
        provider_generation: ProviderGeneration,
        series: &BarSeriesKey,
        price_scale: u8,
        quantity_scale: u8,
        bars: Vec<MarketBar>,
        forming: bool,
    ) -> Result<Vec<ConsumerPublication>, EngineError> {
        self.providers
            .verify_generation(&series.provider_id, provider_generation)?;
        self.providers
            .verify_request(&series.provider_id, ProviderRequest::RealtimeBars)?;
        let bars = bars.into_boxed_slice();
        let snapshot = self.series.install_realtime(
            series.clone(),
            provider_generation,
            price_scale,
            quantity_scale,
            &bars,
            forming,
        )?;
        self.publish_snapshot(series, &snapshot)
    }

    fn publish_snapshot(
        &mut self,
        series: &BarSeriesKey,
        snapshot: &Arc<SeriesSnapshot>,
    ) -> Result<Vec<ConsumerPublication>, EngineError> {
        self.demands
            .matching(series)
            .into_iter()
            .map(|(consumer_id, generation)| {
                self.publications
                    .publish(consumer_id, generation, Arc::clone(snapshot))
            })
            .collect()
    }

    /// Installs only the changed live tail and publishes one incremental update.
    ///
    /// Completed history remains shared and immutable while revisions to the forming
    /// bar replace the prior tail without rebuilding the covering series.
    ///
    /// # Errors
    /// Returns an error for stale generation, invalid continuity, precision, or capacity.
    pub fn install_realtime_tail(
        &mut self,
        provider_generation: ProviderGeneration,
        series: &BarSeriesKey,
        price_scale: u8,
        quantity_scale: u8,
        bar: MarketBar,
        forming: bool,
    ) -> Result<Vec<ConsumerSeriesUpdate>, EngineError> {
        self.providers
            .verify_generation(&series.provider_id, provider_generation)?;
        self.providers
            .verify_request(&series.provider_id, ProviderRequest::RealtimeBars)?;
        let tail = self.series.install_realtime_tail(
            series,
            provider_generation,
            price_scale,
            quantity_scale,
            bar,
            forming,
        )?;
        self.demands
            .matching(series)
            .into_iter()
            .map(|(consumer_id, generation)| {
                self.publications
                    .publish_update(consumer_id, generation, series, tail)
            })
            .collect()
    }

    /// Applies one canonical trade to bounded engine-owned order-flow state and
    /// publishes a covering image for a new series or an incremental update thereafter.
    ///
    /// # Errors
    /// Returns an error for provider generation, identity, sequence, validation, or capacity.
    pub fn install_order_flow_trade(
        &mut self,
        provider_generation: ProviderGeneration,
        series: &BarSeriesKey,
        trade: &MarketTrade,
    ) -> Result<Vec<ConsumerOrderFlowPublication>, EngineError> {
        self.providers
            .verify_generation(&series.provider_id, provider_generation)?;
        self.providers
            .verify_request(&series.provider_id, ProviderRequest::Trades)?;
        let (covering, update, snapshot) =
            self.order_flow
                .apply_trade(series, provider_generation, trade)?;
        let consumers = self
            .demands
            .matching(series)
            .into_iter()
            .filter(|(consumer_id, _)| {
                self.demands.current(*consumer_id).is_some_and(|demand| {
                    demand
                        .streams
                        .is_some_and(|streams| streams.contains(MarketStream::Trades))
                })
            })
            .collect::<Vec<_>>();
        consumers
            .into_iter()
            .map(|(consumer_id, generation)| {
                let (publication_generation, first_for_consumer) =
                    self.order_flow.next_publication_generation(consumer_id)?;
                Ok(ConsumerOrderFlowPublication {
                    consumer_id,
                    generation,
                    publication_generation,
                    series: series.clone(),
                    kind: if covering || first_for_consumer {
                        OrderFlowPublicationKind::Snapshot(Arc::clone(&snapshot))
                    } else {
                        OrderFlowPublicationKind::Update(update)
                    },
                })
            })
            .collect()
    }

    /// Atomically reconstructs bounded engine-owned order-flow state from canonical
    /// historical trades and publishes a covering image to current trade consumers.
    ///
    /// # Errors
    /// Returns an error for provider generation, identity, ordering, validation, or bounds.
    pub fn install_order_flow_history(
        &mut self,
        provider_generation: ProviderGeneration,
        series: &BarSeriesKey,
        trades: &[MarketTrade],
    ) -> Result<Vec<ConsumerOrderFlowPublication>, EngineError> {
        self.providers
            .verify_generation(&series.provider_id, provider_generation)?;
        self.providers
            .verify_request(&series.provider_id, ProviderRequest::Trades)?;
        let snapshot = self
            .order_flow
            .replace_history(series, provider_generation, trades)?;
        self.demands
            .matching(series)
            .into_iter()
            .filter(|(consumer_id, _)| {
                self.demands.current(*consumer_id).is_some_and(|demand| {
                    demand
                        .streams
                        .is_some_and(|streams| streams.contains(MarketStream::Trades))
                })
            })
            .map(|(consumer_id, generation)| {
                let (publication_generation, _) =
                    self.order_flow.next_publication_generation(consumer_id)?;
                Ok(ConsumerOrderFlowPublication {
                    consumer_id,
                    generation,
                    publication_generation,
                    series: series.clone(),
                    kind: OrderFlowPublicationKind::Snapshot(Arc::clone(&snapshot)),
                })
            })
            .collect()
    }

    #[must_use]
    pub fn current_demand(&self, consumer_id: ConsumerId) -> Option<&ConsumerDemand> {
        self.demands.current(consumer_id)
    }

    #[must_use]
    pub fn has_publication(&self, consumer_id: ConsumerId) -> bool {
        self.publications.contains(consumer_id)
    }

    /// Returns one immutable cached series for compatible in-memory derivation.
    #[must_use]
    pub fn series_snapshot(&self, series: &BarSeriesKey) -> Option<Arc<SeriesSnapshot>> {
        self.series.get(series)
    }

    /// Returns the closest compatible finer fixed-time source for derivation.
    #[must_use]
    pub fn compatible_series_snapshot(
        &self,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
    ) -> Option<Arc<SeriesSnapshot>> {
        self.series.compatible_source(series, provider_generation)
    }

    /// Returns an immutable in-memory range from one canonical series.
    #[must_use]
    pub fn series_range(
        &self,
        series: &BarSeriesKey,
        viewport: Viewport,
    ) -> Option<Arc<SeriesSnapshot>> {
        self.series.range(series, viewport)
    }

    #[must_use]
    pub fn subscription_status(&self, series: &BarSeriesKey) -> Option<SubscriptionStatus> {
        self.subscriptions.status(series)
    }

    #[must_use]
    pub fn has_subscription(&self, series: &BarSeriesKey) -> bool {
        self.subscriptions.contains(series)
    }

    pub fn remove_consumer(&mut self, consumer_id: ConsumerId) -> bool {
        self.publications.remove(consumer_id);
        self.order_flow.remove_consumer(consumer_id);
        let Some(removed) = self.demands.remove(consumer_id) else {
            return false;
        };
        if let Some(series) = removed.series.as_ref() {
            self.subscriptions.remove(consumer_id, series);
        }
        true
    }

    pub fn detach_client(&mut self, client_id: ClientId) -> Vec<ConsumerId> {
        let removed = self.demands.remove_client(client_id);
        let mut consumer_ids = Vec::with_capacity(removed.len());
        for demand in removed {
            let consumer_id = demand.identity.consumer_id;
            if let Some(series) = demand.series.as_ref() {
                self.subscriptions.remove(consumer_id, series);
            }
            self.publications.remove(consumer_id);
            self.order_flow.remove_consumer(consumer_id);
            consumer_ids.push(consumer_id);
        }
        consumer_ids
    }

    pub fn invalidate_series(&mut self, series: &BarSeriesKey) -> bool {
        self.publications.invalidate_series(series);
        self.order_flow.invalidate(series);
        self.series.invalidate(series)
    }

    /// Evicts only unsubscribed cached series until both policy bounds are met.
    ///
    /// Active subscriptions are never removed to satisfy a resource preference.
    pub fn evict_unsubscribed_series(
        &mut self,
        maximum_cached_series: usize,
        maximum_decoded_bars: usize,
        protected: &[BarSeriesKey],
    ) -> Vec<BarSeriesKey> {
        let mut cached = self
            .series
            .retained()
            .into_iter()
            .filter(|(series, _)| {
                !self.subscriptions.contains(series) && !protected.contains(series)
            })
            .collect::<Vec<_>>();
        let mut removed = Vec::new();
        while self.series.len() > maximum_cached_series
            || self.series.total_bars() > maximum_decoded_bars
        {
            let Some((series, _)) = cached.first().cloned() else {
                break;
            };
            cached.remove(0);
            if self.series.invalidate(&series) {
                self.publications.invalidate_series(&series);
                removed.push(series);
            }
        }
        removed
    }

    #[must_use]
    pub fn metrics(&self) -> MarketEngineMetrics {
        MarketEngineMetrics {
            active_consumers: self.demands.len(),
            registered_providers: self.providers.len(),
            active_subscriptions: self.subscriptions.len(),
            stored_series: self.series.len(),
            stored_bars: self.series.total_bars(),
            approximate_series_bytes: self.series.approximate_bytes(),
        }
    }

    #[must_use]
    pub fn visible_consumer_count(&self) -> usize {
        self.demands.foreground_len()
    }
}

use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_market_data::{AggressorSide, BarPeriod, EventMetadata, QualifiedTimestamp};

    fn nonzero(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("test identity is non-zero")
    }

    fn id(value: u64) -> ConsumerId {
        ConsumerId(nonzero(value))
    }

    fn generation(value: u64) -> GenerationId {
        GenerationId(nonzero(value))
    }

    fn provider_generation(value: u64) -> ProviderGeneration {
        ProviderGeneration(nonzero(value))
    }

    fn series(instrument: &str) -> BarSeriesKey {
        provider_series("coinbase", instrument, "public")
    }

    fn provider_series(provider: &str, instrument: &str, entitlement: &str) -> BarSeriesKey {
        BarSeriesKey {
            provider_id: provider.to_string(),
            instrument_id: instrument.to_string(),
            entitlement_id: entitlement.to_string(),
            period: BarPeriod::time(60).expect("test period validates"),
            definition_version: 1,
        }
    }

    fn bars(count: usize) -> Vec<MarketBar> {
        (0..count)
            .map(|index| MarketBar {
                source_sequence: u64::try_from(index + 1).expect("test sequence fits"),
                exchange_timestamp_seconds: 1_700_000_000
                    + i64::try_from(index).expect("test time fits") * 60,
                exchange_timestamp_unix_nanos: (1_700_000_000
                    + i64::try_from(index).expect("test time fits") * 60)
                    * 1_000_000_000,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            })
            .collect()
    }

    fn engine(
        maximum_consumers: usize,
        maximum_series: usize,
        maximum_bars: usize,
    ) -> MarketEngine {
        let mut engine = MarketEngine::new(MarketEngineConfig {
            maximum_consumers: NonZeroUsize::new(maximum_consumers).expect("consumer bound"),
            maximum_series: NonZeroUsize::new(maximum_series).expect("series bound"),
            maximum_bars: NonZeroUsize::new(maximum_bars).expect("bar bound"),
        });
        engine
            .register_provider(
                "coinbase".to_string(),
                ProviderConfig {
                    account_id: "coinbase:public".to_string(),
                    capabilities: ProviderCapabilities {
                        historical_bars: true,
                        realtime_bars: true,
                        streams: StreamRequirements::BARS.with(MarketStream::Trades),
                    },
                    reconnect_delay: std::time::Duration::from_millis(250),
                },
            )
            .expect("provider registers");
        engine
            .begin_provider_session("coinbase", provider_generation(1))
            .expect("provider session begins");
        engine
    }

    fn register(engine: &mut MarketEngine, consumer: u64, client: u64) {
        register_workspace(engine, consumer, client, 1);
    }

    fn register_workspace(engine: &mut MarketEngine, consumer: u64, client: u64, workspace: u64) {
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(nonzero(client)),
                    workspace_id: WorkspaceId(nonzero(workspace)),
                    consumer_id: id(consumer),
                },
                true,
            )
            .expect("consumer registers");
    }

    fn trade(series: &BarSeriesKey, sequence: u64, price: i64, side: AggressorSide) -> MarketTrade {
        MarketTrade {
            metadata: EventMetadata {
                provider_id: series.provider_id.clone(),
                instrument_id: series.instrument_id.clone(),
                entitlement_id: series.entitlement_id.clone(),
                source_sequence: sequence,
                session_generation: 1,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(
                        1_700_000_000_000_000_000
                            + i64::try_from(sequence).expect("test sequence fits"),
                    ),
                    provider_unix_nanos: None,
                    received_unix_nanos: 1_700_000_000_000_000_000
                        + i64::try_from(sequence).expect("test sequence fits"),
                },
            },
            trade_id: format!("trade-{sequence}"),
            price,
            quantity: 2,
            aggressor: side,
        }
    }

    #[test]
    fn stale_history_never_overwrites_a_new_consumer_generation() {
        let mut engine = engine(2, 2, 10);
        register(&mut engine, 1, 1);
        let btc = series("coinbase:spot:BTC-USD");
        let eth = series("coinbase:spot:ETH-USD");
        assert_eq!(
            engine
                .set_series_demand(id(1), generation(1), &btc)
                .expect("first demand installs"),
            None
        );
        assert_eq!(
            engine
                .set_series_demand(id(1), generation(2), &eth)
                .expect("replacement demand installs"),
            None
        );
        assert!(
            engine
                .install_history(provider_generation(1), &btc, 2, 8, bars(2))
                .expect("late BTC remains cacheable")
                .is_empty()
        );
        assert!(!engine.has_publication(id(1)));
        let publication = engine
            .install_history(provider_generation(1), &eth, 2, 8, bars(2))
            .expect("current ETH publishes")
            .pop()
            .expect("one current consumer");
        assert_eq!(publication.generation, generation(2));
    }

    #[test]
    fn twenty_consumers_share_one_immutable_series_snapshot() {
        let mut engine = engine(20, 1, 10);
        let btc = series("coinbase:spot:BTC-USD");
        for consumer in 1..=20 {
            register_workspace(&mut engine, consumer, consumer, ((consumer - 1) / 4) + 1);
            assert!(
                engine
                    .set_series_demand(id(consumer), generation(1), &btc)
                    .expect("demand installs")
                    .is_none()
            );
        }
        let publications = engine
            .install_history(provider_generation(1), &btc, 2, 8, bars(3))
            .expect("history installs once");
        assert_eq!(publications.len(), 20);
        for publication in &publications[1..] {
            assert!(Arc::ptr_eq(
                &publications[0].snapshot,
                &publication.snapshot
            ));
        }
        assert_eq!(engine.metrics().stored_series, 1);
        assert_eq!(engine.metrics().stored_bars, 3);
        assert_eq!(
            (1..=20)
                .filter_map(|consumer| engine.current_demand(id(consumer)))
                .map(|demand| demand.identity.workspace_id)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            5
        );
    }

    #[test]
    fn trade_count_bars_may_order_within_one_exchange_second() {
        let mut engine = engine(1, 1, 4);
        let mut tick_series = series("coinbase:spot:BTC-USD");
        tick_series.period = BarPeriod::tick(100).expect("tick period validates");
        let mut tick_bars = bars(2);
        tick_bars[1].exchange_timestamp_seconds = tick_bars[0].exchange_timestamp_seconds;
        tick_bars[1].exchange_timestamp_unix_nanos = tick_bars[0].exchange_timestamp_unix_nanos + 1;
        engine
            .install_history(provider_generation(1), &tick_series, 2, 0, tick_bars)
            .expect("exact timestamps order same-second bars");
    }

    #[test]
    fn memory_and_consumer_bounds_fail_without_mutating_current_state() {
        let mut engine = engine(1, 1, 2);
        register(&mut engine, 1, 1);
        assert!(matches!(
            engine.register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(nonzero(1)),
                    workspace_id: WorkspaceId(nonzero(1)),
                    consumer_id: id(2),
                },
                true,
            ),
            Err(EngineError::ConsumerLimitExceeded { .. })
        ));
        let btc = series("coinbase:spot:BTC-USD");
        engine
            .install_history(provider_generation(1), &btc, 2, 8, bars(2))
            .expect("bounded series installs");
        assert!(matches!(
            engine.install_history(
                provider_generation(1),
                &series("coinbase:spot:ETH-USD"),
                2,
                8,
                bars(1)
            ),
            Err(EngineError::SeriesLimitExceeded { .. })
        ));
        assert_eq!(engine.metrics().stored_series, 1);
        assert_eq!(engine.metrics().stored_bars, 2);
        assert!(engine.invalidate_series(&btc));
        assert_eq!(engine.metrics().stored_bars, 0);
    }

    /// A covering repair invalidates the series it is about to replace. The
    /// consumer's publication generation is its ordering contract with the
    /// client, so it has to survive that: rewinding it made the repaired snapshot
    /// read as stale, and the chart stopped on the snapshot that fixed it.
    #[test]
    fn a_series_invalidation_does_not_rewind_the_publication_generation() {
        let mut engine = engine(1, 3, 8);
        register(&mut engine, 1, 1);
        let btc = series("coinbase:spot:BTC-USD");
        engine
            .install_history(provider_generation(1), &btc, 2, 8, bars(2))
            .expect("history installs");
        let first = engine
            .set_series_demand(id(1), generation(1), &btc)
            .expect("demand resolves")
            .expect("the cached series publishes");

        assert!(engine.invalidate_series(&btc));
        let repaired = engine
            .install_history(provider_generation(1), &btc, 2, 8, bars(3))
            .expect("repaired history installs");

        let repaired = repaired
            .into_iter()
            .find(|publication| publication.consumer_id == id(1))
            .expect("the subscribed consumer is published to");
        assert!(
            repaired.publication_generation > first.publication_generation,
            "the repair must advance the client past {}, not restart at {}",
            first.publication_generation,
            repaired.publication_generation
        );
    }

    #[test]
    fn resource_eviction_preserves_active_and_explicitly_retained_series() {
        let mut engine = engine(1, 3, 6);
        let active = series("coinbase:spot:BTC-USD");
        let retained = series("coinbase:spot:ETH-USD");
        let cold = series("coinbase:spot:SOL-USD");
        for current in [&active, &retained, &cold] {
            engine
                .install_history(provider_generation(1), current, 2, 8, bars(2))
                .expect("series history installs");
        }
        register(&mut engine, 1, 1);
        assert!(
            engine
                .set_series_demand(id(1), generation(1), &active)
                .expect("active demand resolves")
                .is_some()
        );

        let removed = engine.evict_unsubscribed_series(0, 0, std::slice::from_ref(&retained));

        assert_eq!(removed, vec![cold.clone()]);
        assert_eq!(engine.metrics().stored_series, 2);
        assert_eq!(engine.metrics().stored_bars, 4);
        assert!(engine.invalidate_series(&retained));
        assert!(engine.invalidate_series(&active));
    }

    #[test]
    fn provider_and_viewport_generations_are_exactly_fenced() {
        let mut engine = engine(1, 1, 2);
        register(&mut engine, 1, 1);
        let btc = series("coinbase:spot:BTC-USD");
        engine
            .set_series_demand(id(1), generation(2), &btc)
            .expect("demand installs");
        assert!(matches!(
            engine.set_viewport(
                id(1),
                generation(1),
                Viewport::try_new(1, 2).expect("viewport validates")
            ),
            Err(EngineError::StaleConsumerGeneration { .. })
        ));
        assert!(matches!(
            engine.install_history(provider_generation(2), &btc, 2, 8, bars(1)),
            Err(EngineError::StaleProviderGeneration { .. })
        ));
        assert!(matches!(
            engine.begin_provider_session("coinbase", provider_generation(1)),
            Err(EngineError::StaleProviderGeneration { .. })
        ));
    }

    #[test]
    fn disconnect_removes_only_the_owning_clients_consumers() {
        let mut engine = engine(3, 1, 2);
        register(&mut engine, 1, 1);
        register(&mut engine, 2, 1);
        register(&mut engine, 3, 2);
        assert_eq!(engine.detach_client(ClientId(nonzero(1))).len(), 2);
        assert!(engine.current_demand(id(1)).is_none());
        assert!(engine.current_demand(id(2)).is_none());
        assert!(engine.current_demand(id(3)).is_some());
        assert_eq!(engine.metrics().active_consumers, 1);
    }

    #[test]
    fn visibility_changes_only_the_selected_workspace_consumer() {
        let mut engine = engine(2, 1, 2);
        register(&mut engine, 1, 1);
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(nonzero(1)),
                    workspace_id: WorkspaceId(nonzero(2)),
                    consumer_id: id(2),
                },
                true,
            )
            .expect("second workspace consumer registers");
        engine
            .set_visibility(id(1), false)
            .expect("first consumer becomes hidden");
        assert_eq!(
            engine
                .current_demand(id(1))
                .map(|demand| demand.resource_class),
            Some(ConsumerResourceClass::Background)
        );
        assert_eq!(
            engine
                .current_demand(id(2))
                .map(|demand| demand.resource_class),
            Some(ConsumerResourceClass::Foreground)
        );
        assert_eq!(
            engine
                .current_demand(id(2))
                .map(|demand| demand.identity.workspace_id),
            Some(WorkspaceId(nonzero(2)))
        );
    }

    #[test]
    fn decimal_precision_is_part_of_one_provider_generation() {
        let mut engine = engine(1, 1, 2);
        let btc = series("coinbase:spot:BTC-USD");
        engine
            .install_history(provider_generation(1), &btc, 2, 8, bars(1))
            .expect("first precision installs");
        assert!(matches!(
            engine.install_history(provider_generation(1), &btc, 3, 8, bars(1)),
            Err(EngineError::ConflictingSeriesGeneration(_))
        ));
        register(&mut engine, 1, 1);
        let snapshot = engine
            .set_series_demand(id(1), generation(1), &btc)
            .expect("cached demand resolves")
            .expect("snapshot is cached");
        assert_eq!(snapshot.snapshot.price_scale, 2);
        assert_eq!(snapshot.snapshot.quantity_scale, 8);
    }

    #[test]
    fn realtime_may_revise_only_the_forming_tail() {
        let mut engine = engine(1, 1, 8);
        let btc = series("coinbase:spot:BTC-USD");
        engine
            .install_history(provider_generation(1), &btc, 2, 8, bars(2))
            .expect("history installs");
        engine
            .install_realtime(provider_generation(1), &btc, 2, 8, bars(3), true)
            .expect("forming bar appends");
        let mut revised = bars(3);
        revised[2].close = 106;
        let publication = engine
            .install_realtime(provider_generation(1), &btc, 2, 8, revised, true)
            .expect("forming bar revises");
        assert!(publication.is_empty());
        let mut corrupted = bars(4);
        corrupted[0].close = 106;
        assert!(matches!(
            engine.install_realtime(provider_generation(1), &btc, 2, 8, corrupted, true),
            Err(EngineError::ConflictingSeriesGeneration(_))
        ));
    }

    #[test]
    fn covering_realtime_install_keeps_the_forming_bar_as_an_incremental_tail() {
        let mut engine = engine(1, 1, 8);
        let btc = series("coinbase:spot:BTC-USD");
        engine
            .install_history(provider_generation(1), &btc, 2, 8, bars(2))
            .expect("history installs");
        engine
            .install_realtime(provider_generation(1), &btc, 2, 8, bars(3), true)
            .expect("covering realtime state installs");
        let mut revised = bars(3).pop().expect("forming bar exists");
        revised.close = 106;
        engine
            .install_realtime_tail(provider_generation(1), &btc, 2, 8, revised, true)
            .expect("forming tail remains incrementally revisable");
        let snapshot = engine
            .series_snapshot(&btc)
            .expect("revised series remains materializable");
        assert_eq!(snapshot.bars.len(), 3);
        assert_eq!(snapshot.bars[2].close, 106);
    }

    #[test]
    fn covering_history_repair_preserves_the_current_forming_tail() {
        let mut engine = engine(1, 1, 8);
        let btc = series("coinbase:spot:BTC-USD");
        engine
            .install_history(provider_generation(1), &btc, 2, 8, bars(2))
            .expect("history installs");
        engine
            .install_realtime(provider_generation(1), &btc, 2, 8, bars(3), true)
            .expect("forming bar appends");
        let mut repaired = bars(3);
        repaired[2].close = 107;
        engine
            .replace_covering_history(provider_generation(1), &btc, 2, 8, repaired, true)
            .expect("covering repair installs");

        let snapshot = engine
            .series_snapshot(&btc)
            .expect("series remains materialized");
        assert!(snapshot.forming);
        assert_eq!(snapshot.bars[2].close, 105);
        assert_eq!(snapshot.publication_generation, 3);
    }

    #[test]
    fn realtime_tail_revisions_share_completed_history_until_bucket_roll() {
        let mut engine = engine(2, 1, 8);
        let btc = series("coinbase:spot:BTC-USD");
        register(&mut engine, 1, 1);
        register(&mut engine, 2, 1);
        for consumer in 1..=2 {
            engine
                .set_series_demand(id(consumer), generation(1), &btc)
                .expect("demand installs");
        }
        engine
            .install_history(provider_generation(1), &btc, 2, 8, bars(2))
            .expect("history installs");
        let completed = engine
            .series
            .completed_bars(&btc)
            .expect("completed history is retained");

        let mut tail = bars(3).pop().expect("tail exists");
        let first = engine
            .install_realtime_tail(provider_generation(1), &btc, 2, 8, tail, true)
            .expect("tail appends");
        assert_eq!(first.len(), 2);
        assert_eq!(engine.metrics().stored_bars, 3);
        assert!(Arc::ptr_eq(
            &completed,
            &engine
                .series
                .completed_bars(&btc)
                .expect("completed history remains shared")
        ));

        tail.close = 106;
        let revised = engine
            .install_realtime_tail(provider_generation(1), &btc, 2, 8, tail, true)
            .expect("forming tail revises");
        assert_eq!(revised.len(), 2);
        assert!(revised.iter().all(|update| update.bar.close == 106));
        assert!(Arc::ptr_eq(
            &completed,
            &engine
                .series
                .completed_bars(&btc)
                .expect("tail revision does not rebuild history")
        ));

        let next = bars(4).pop().expect("next tail exists");
        engine
            .install_realtime_tail(provider_generation(1), &btc, 2, 8, next, true)
            .expect("next bucket appends");
        assert_eq!(engine.metrics().stored_bars, 4);
        assert!(!Arc::ptr_eq(
            &completed,
            &engine
                .series
                .completed_bars(&btc)
                .expect("completed tail rolls into history once")
        ));
        let snapshot = engine
            .series_snapshot(&btc)
            .expect("covering state materializes");
        assert_eq!(snapshot.bars.len(), 4);
        assert_eq!(snapshot.bars[2].close, 106);

        let mut repaired = snapshot.bars.to_vec();
        repaired[2].close = 107;
        engine
            .replace_covering_history(provider_generation(1), &btc, 2, 8, repaired, true)
            .expect("closed tail repairs after a live bucket roll");
        assert_eq!(engine.metrics().stored_bars, 4);
        assert_eq!(
            engine
                .series_snapshot(&btc)
                .expect("repaired series materializes")
                .bars[2]
                .close,
            107
        );
    }

    #[test]
    fn covering_history_may_replace_only_the_forming_tail() {
        let mut engine = engine(1, 1, 8);
        let btc = series("coinbase:spot:BTC-USD");
        engine
            .install_history(provider_generation(1), &btc, 2, 8, bars(2))
            .expect("history installs");
        engine
            .install_realtime(provider_generation(1), &btc, 2, 8, bars(3), true)
            .expect("forming tail appends");
        let completed = engine
            .install_history(provider_generation(1), &btc, 2, 8, bars(2))
            .expect("covering history may remove the forming tail");
        assert!(completed.is_empty());
        let snapshot = engine
            .series_snapshot(&btc)
            .expect("history remains cached");
        assert!(!snapshot.forming);
        assert_eq!(snapshot.bars.len(), 2);

        engine
            .install_realtime(provider_generation(1), &btc, 2, 8, bars(3), true)
            .expect("forming tail resumes");
        let mut finalized = bars(3);
        finalized[2].close = 106;
        engine
            .install_history(provider_generation(1), &btc, 2, 8, finalized)
            .expect("covering history may finalize the forming tail");
        let mut corrupted = bars(4);
        corrupted[0].close = 106;
        assert!(matches!(
            engine.install_history(provider_generation(1), &btc, 2, 8, corrupted),
            Err(EngineError::ConflictingSeriesGeneration(_))
        ));
        assert_eq!(
            engine
                .series_snapshot(&btc)
                .expect("rejected repair preserves current history")
                .bars[0]
                .close,
            105
        );
    }

    #[test]
    fn shared_subscriptions_ref_count_streams_and_release_only_the_last_consumer() {
        let mut engine = engine(3, 2, 8);
        let btc = series("coinbase:spot:BTC-USD");
        let eth = series("coinbase:spot:ETH-USD");
        for consumer in 1..=3 {
            register(&mut engine, consumer, if consumer < 3 { 1 } else { 2 });
        }
        engine
            .set_series_demand_with_streams(id(1), generation(1), &btc, StreamRequirements::BARS)
            .expect("first demand installs");
        engine
            .set_series_demand_with_streams(
                id(2),
                generation(1),
                &btc,
                StreamRequirements::BARS.with(MarketStream::Trades),
            )
            .expect("duplicate demand coalesces");
        assert_eq!(
            engine.subscription_status(&btc),
            Some(SubscriptionStatus {
                consumer_count: 2,
                streams: StreamRequirements::BARS.with(MarketStream::Trades),
            })
        );

        engine
            .set_series_demand_with_streams(id(1), generation(2), &eth, StreamRequirements::BARS)
            .expect("one chart switches atomically");
        assert_eq!(
            engine
                .subscription_status(&btc)
                .map(|status| status.consumer_count),
            Some(1)
        );
        assert_eq!(
            engine
                .subscription_status(&eth)
                .map(|status| status.consumer_count),
            Some(1)
        );

        assert!(engine.remove_consumer(id(2)));
        assert!(!engine.has_subscription(&btc));
        assert!(engine.has_subscription(&eth));
        assert_eq!(engine.detach_client(ClientId(nonzero(1))), vec![id(1)]);
        assert!(!engine.has_subscription(&eth));
        assert!(engine.current_demand(id(3)).is_some());
    }

    #[test]
    fn warm_and_detached_consumers_release_publication_and_reattach_from_cached_state() {
        let mut engine = engine(2, 1, 8);
        let btc = series("coinbase:spot:BTC-USD");
        register(&mut engine, 1, 1);
        engine
            .set_series_demand_with_streams(
                id(1),
                generation(1),
                &btc,
                StreamRequirements::BARS.with(MarketStream::Trades),
            )
            .expect("demand installs");
        engine
            .install_history(provider_generation(1), &btc, 2, 8, bars(2))
            .expect("history publishes");
        assert!(engine.has_subscription(&btc));
        assert!(engine.has_publication(id(1)));

        assert_eq!(
            engine
                .set_resource_class(id(1), ConsumerResourceClass::Warm)
                .expect("consumer becomes warm"),
            None
        );
        assert!(!engine.has_subscription(&btc));
        assert!(!engine.has_publication(id(1)));
        assert!(
            engine
                .install_realtime(provider_generation(1), &btc, 2, 8, bars(3), true)
                .expect("warm state remains cacheable")
                .is_empty()
        );

        assert_eq!(
            engine
                .set_resource_class(id(1), ConsumerResourceClass::Background)
                .expect("background consumer reattaches upstream"),
            None
        );
        assert!(engine.has_subscription(&btc));
        assert!(!engine.has_publication(id(1)));
        assert!(
            engine
                .install_realtime(provider_generation(1), &btc, 2, 8, bars(4), true)
                .expect("background state remains current without UI publication")
                .is_empty()
        );

        let publication = engine
            .set_resource_class(id(1), ConsumerResourceClass::Foreground)
            .expect("foreground consumer reattaches presentation")
            .expect("cached covering state publishes immediately");
        assert_eq!(publication.snapshot.bars.len(), 4);
        assert_eq!(publication.publication_generation, 2);
        assert!(engine.has_subscription(&btc));

        engine
            .set_resource_class(id(1), ConsumerResourceClass::Detached)
            .expect("consumer detaches");
        assert!(!engine.has_subscription(&btc));
        assert!(!engine.has_publication(id(1)));
    }

    #[test]
    fn order_flow_reconstructs_bounded_view_ready_state_and_resnapshots_each_consumer() {
        let mut engine = engine(2, 1, 8);
        let btc = series("coinbase:spot:BTC-USD");
        register_workspace(&mut engine, 1, 1, 1);
        engine
            .set_series_demand_with_streams(
                id(1),
                generation(1),
                &btc,
                StreamRequirements::BARS.with(MarketStream::Trades),
            )
            .expect("first trade consumer installs");
        let first = engine
            .install_order_flow_trade(
                provider_generation(1),
                &btc,
                &trade(&btc, 1, 10_000, AggressorSide::Buy),
            )
            .expect("first trade applies");
        assert!(matches!(
            first.as_slice(),
            [ConsumerOrderFlowPublication {
                consumer_id,
                publication_generation: 1,
                kind: OrderFlowPublicationKind::Snapshot(snapshot),
                ..
            }] if *consumer_id == id(1)
                && snapshot.cumulative_delta == 2
                && snapshot.levels[0].ask_volume == 2
        ));

        let second = engine
            .install_order_flow_trade(
                provider_generation(1),
                &btc,
                &trade(&btc, 2, 10_000, AggressorSide::Sell),
            )
            .expect("second trade applies");
        assert!(matches!(
            second[0].kind,
            OrderFlowPublicationKind::Update(OrderFlowUpdate {
                cumulative_delta: 0,
                level: OrderFlowLevel {
                    bid_volume: 2,
                    ask_volume: 2,
                    trade_count: 2,
                    ..
                },
                ..
            })
        ));

        register_workspace(&mut engine, 2, 2, 2);
        engine
            .set_series_demand_with_streams(
                id(2),
                generation(7),
                &btc,
                StreamRequirements::BARS.with(MarketStream::Trades),
            )
            .expect("late trade consumer installs");
        let publications = engine
            .install_order_flow_trade(
                provider_generation(1),
                &btc,
                &trade(&btc, 3, 10_100, AggressorSide::Buy),
            )
            .expect("third trade applies");
        let late = publications
            .iter()
            .find(|publication| publication.consumer_id == id(2))
            .expect("late consumer publishes");
        assert_eq!(late.generation, generation(7));
        assert!(matches!(
            &late.kind,
            OrderFlowPublicationKind::Snapshot(snapshot)
                if snapshot.source_watermark == 3
                    && snapshot.cumulative_delta == 2
                    && snapshot.levels.len() == 2
                    && snapshot.tape.len() == 3
        ));

        engine
            .set_resource_class(id(2), ConsumerResourceClass::Detached)
            .expect("late consumer detaches");
        let detached = engine
            .install_order_flow_trade(
                provider_generation(1),
                &btc,
                &trade(&btc, 4, 10_100, AggressorSide::Buy),
            )
            .expect("detached state still accumulates for shared demand");
        assert!(
            detached
                .iter()
                .all(|publication| publication.consumer_id != id(2))
        );
    }

    #[test]
    fn order_flow_tape_and_price_levels_are_strictly_bounded() {
        let mut engine = engine(1, 1, 8);
        let btc = series("coinbase:spot:BTC-USD");
        register(&mut engine, 1, 1);
        engine
            .set_series_demand_with_streams(
                id(1),
                generation(1),
                &btc,
                StreamRequirements::BARS.with(MarketStream::Trades),
            )
            .expect("trade demand installs");
        for sequence in 1..=2_048 {
            engine
                .install_order_flow_trade(
                    provider_generation(1),
                    &btc,
                    &trade(
                        &btc,
                        sequence,
                        10_000 + i64::try_from(sequence).expect("sequence fits"),
                        AggressorSide::Unknown,
                    ),
                )
                .expect("bounded level applies");
        }
        assert!(matches!(
            engine.install_order_flow_trade(
                provider_generation(1),
                &btc,
                &trade(&btc, 2_049, 20_000, AggressorSide::Unknown),
            ),
            Err(EngineError::OrderFlowLevelLimitExceeded { maximum: 2_048 })
        ));

        engine
            .set_resource_class(id(1), ConsumerResourceClass::Warm)
            .expect("publication generation resets");
        engine
            .set_resource_class(id(1), ConsumerResourceClass::Foreground)
            .expect("consumer reattaches to presentation");
        let snapshot = engine
            .install_order_flow_trade(
                provider_generation(1),
                &btc,
                &trade(&btc, 2_049, 10_001, AggressorSide::Buy),
            )
            .expect("existing level remains writable")
            .pop()
            .expect("reattached order-flow snapshot publishes");
        assert!(matches!(
            snapshot.kind,
            OrderFlowPublicationKind::Snapshot(ref state) if state.tape.len() == 256
        ));
    }

    #[test]
    fn historical_order_flow_reconstruction_is_atomic_and_publishes_a_covering_image() {
        let mut engine = engine(1, 1, 8);
        let btc = series("coinbase:spot:BTC-USD");
        register(&mut engine, 1, 1);
        engine
            .set_series_demand_with_streams(
                id(1),
                generation(1),
                &btc,
                StreamRequirements::BARS.with(MarketStream::Trades),
            )
            .expect("trade demand installs");
        let history = [
            trade(&btc, 1, 10_000, AggressorSide::Buy),
            trade(&btc, 2, 10_000, AggressorSide::Sell),
        ];
        let covering = engine
            .install_order_flow_history(provider_generation(1), &btc, &history)
            .expect("historical trades reconstruct")
            .pop()
            .expect("consumer receives covering image");
        assert!(matches!(
            covering.kind,
            OrderFlowPublicationKind::Snapshot(ref snapshot)
                if snapshot.source_watermark == 2
                    && snapshot.cumulative_delta == 0
                    && snapshot.levels[0].bid_volume == 2
                    && snapshot.levels[0].ask_volume == 2
                    && snapshot.tape.len() == 2
        ));

        let invalid = [
            trade(&btc, 4, 10_000, AggressorSide::Buy),
            trade(&btc, 3, 10_000, AggressorSide::Buy),
        ];
        assert!(matches!(
            engine.install_order_flow_history(provider_generation(1), &btc, &invalid),
            Err(EngineError::NonIncreasingOrderFlowSequence)
        ));
        let continued = engine
            .install_order_flow_trade(
                provider_generation(1),
                &btc,
                &trade(&btc, 3, 10_000, AggressorSide::Buy),
            )
            .expect("failed replacement preserves the prior accumulator")
            .pop()
            .expect("consumer receives the continuation");
        assert!(matches!(
            continued.kind,
            OrderFlowPublicationKind::Update(OrderFlowUpdate {
                cumulative_delta: 2,
                ..
            })
        ));
    }

    #[test]
    fn equal_instrument_labels_from_different_providers_never_share_series_state() {
        let mut engine = engine(2, 2, 8);
        engine
            .register_provider(
                "rithmic".to_string(),
                ProviderConfig {
                    account_id: "rithmic:test".to_string(),
                    capabilities: ProviderCapabilities {
                        historical_bars: true,
                        realtime_bars: true,
                        streams: StreamRequirements::BARS,
                    },
                    reconnect_delay: std::time::Duration::from_millis(500),
                },
            )
            .expect("second provider registers");
        engine
            .begin_provider_session("rithmic", provider_generation(1))
            .expect("second provider session begins");
        register(&mut engine, 1, 1);
        register(&mut engine, 2, 1);

        let coinbase = provider_series("coinbase", "BTC", "public");
        let rithmic = provider_series("rithmic", "BTC", "rithmic:test");
        engine
            .set_series_demand(id(1), generation(1), &coinbase)
            .expect("Coinbase demand installs");
        engine
            .set_series_demand(id(2), generation(1), &rithmic)
            .expect("Rithmic demand installs");

        let mut coinbase_bars = bars(2);
        coinbase_bars[1].close = 101;
        let mut rithmic_bars = bars(2);
        rithmic_bars[1].open = 200;
        rithmic_bars[1].high = 210;
        rithmic_bars[1].low = 190;
        rithmic_bars[1].close = 202;
        engine
            .install_history(provider_generation(1), &coinbase, 2, 8, coinbase_bars)
            .expect("Coinbase history installs");
        engine
            .install_history(provider_generation(1), &rithmic, 2, 8, rithmic_bars)
            .expect("Rithmic history installs");

        assert_eq!(
            engine
                .series_snapshot(&coinbase)
                .expect("Coinbase series remains available")
                .bars[1]
                .close,
            101
        );
        assert_eq!(
            engine
                .series_snapshot(&rithmic)
                .expect("Rithmic series remains available")
                .bars[1]
                .close,
            202
        );
        assert_eq!(engine.metrics().stored_series, 2);
        assert_eq!(engine.metrics().active_subscriptions, 2);
    }

    #[test]
    fn provider_configuration_routes_only_supported_generation_fenced_requests() {
        let mut engine = engine(1, 1, 2);
        assert_eq!(
            engine.provider_account_id("coinbase"),
            Some("coinbase:public")
        );
        assert_eq!(
            engine.provider_reconnect_delay("coinbase"),
            Some(std::time::Duration::from_millis(250))
        );
        engine
            .verify_provider_request("coinbase", ProviderRequest::Trades)
            .expect("trade requests route");
        assert!(matches!(
            engine.verify_provider_request("coinbase", ProviderRequest::Quotes),
            Err(EngineError::UnsupportedProviderRequest { .. })
        ));
        engine
            .end_provider_session("coinbase", provider_generation(1))
            .expect("active session disconnects");
        assert_eq!(
            engine
                .provider_status("coinbase")
                .map(|status| status.health),
            Some(ProviderHealth::Disconnected)
        );
        assert!(matches!(
            engine.begin_provider_session("coinbase", provider_generation(1)),
            Err(EngineError::StaleProviderGeneration { .. })
        ));
        engine
            .begin_provider_session("coinbase", provider_generation(2))
            .expect("reconnect advances the provider generation");
    }

    #[test]
    fn series_store_answers_ranges_and_selects_the_closest_compatible_interval() {
        let mut engine = engine(1, 3, 16);
        let minute = series("coinbase:spot:BTC-USD");
        let mut five_minute = minute.clone();
        five_minute.period = BarPeriod::time(300).expect("five-minute period");
        let mut fifteen_minute = minute.clone();
        fifteen_minute.period = BarPeriod::time(900).expect("fifteen-minute period");
        engine
            .install_history(provider_generation(1), &minute, 2, 8, bars(4))
            .expect("minute history installs");
        engine
            .install_history(provider_generation(1), &five_minute, 2, 8, bars(3))
            .expect("five-minute history installs");

        let source = engine
            .compatible_series_snapshot(&fifteen_minute, provider_generation(1))
            .expect("closest finer interval is found");
        assert_eq!(source.series.period, five_minute.period);
        let range = engine
            .series_range(
                &minute,
                Viewport::try_new(1_700_000_060_000_000_000, 1_700_000_180_000_000_000)
                    .expect("range validates"),
            )
            .expect("range intersects history");
        assert_eq!(range.bars.len(), 2);
        assert_eq!(range.bars[0].source_sequence, 2);
        assert_eq!(range.bars[1].source_sequence, 3);
    }

    #[test]
    fn repeated_current_demand_recovers_with_a_new_covering_publication() {
        let mut engine = engine(1, 1, 4);
        let btc = series("coinbase:spot:BTC-USD");
        register(&mut engine, 1, 1);
        engine
            .set_series_demand(id(1), generation(1), &btc)
            .expect("demand installs");
        let first = engine
            .install_history(provider_generation(1), &btc, 2, 8, bars(2))
            .expect("history publishes")
            .pop()
            .expect("covering publication exists");
        let recovery = engine
            .set_series_demand(id(1), generation(1), &btc)
            .expect("current demand reasserts")
            .expect("covering recovery publishes");
        assert_eq!(recovery.publication_generation, 2);
        assert!(Arc::ptr_eq(&first.snapshot, &recovery.snapshot));
    }
}
