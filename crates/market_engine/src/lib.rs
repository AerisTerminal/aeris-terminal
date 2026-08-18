//! Headless single-owner market state for the resident Axiusflow engine.
//!
//! This crate contains no GPUI, IPC serialization, provider sockets, storage,
//! threads, or global mutable state. The process shell owns one `MarketEngine`
//! and passes ordinary typed commands and canonical provider results into it.

mod demand;
mod provider_manager;
mod publication;
mod series_store;

pub use demand::{ConsumerDemand, ConsumerIdentity};
pub use provider_manager::{ProviderCapabilities, ProviderHealth, ProviderStatus};
pub use publication::{ConsumerPublication, ConsumerSeriesUpdate};
pub use series_store::SeriesSnapshot;

use axiusflow_market_data::{BarSeriesKey, MarketBar, MarketDataValidationError};
use demand::DemandRegistry;
use provider_manager::ProviderManager;
use publication::PublicationManager;
use series_store::SeriesStore;
use std::{
    error::Error,
    fmt,
    num::{NonZeroU64, NonZeroUsize},
};

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
    pub stored_series: usize,
    pub stored_bars: usize,
    pub approximate_series_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineError {
    InvalidProviderIdentity,
    DuplicateProvider(String),
    UnknownProvider(String),
    StaleProviderGeneration {
        current: Option<ProviderGeneration>,
        received: ProviderGeneration,
    },
    DuplicateConsumer(ConsumerId),
    UnknownConsumer(ConsumerId),
    ConsumerHasNoSeries(ConsumerId),
    ConsumerLimitExceeded {
        maximum: NonZeroUsize,
    },
    StaleConsumerGeneration {
        consumer_id: ConsumerId,
        current: GenerationId,
        received: GenerationId,
    },
    InvalidViewport,
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
            Self::DuplicateProvider(provider) => {
                write!(formatter, "provider {provider} is already registered")
            }
            Self::UnknownProvider(provider) => {
                write!(formatter, "provider {provider} is not registered")
            }
            Self::StaleProviderGeneration { .. } => {
                formatter.write_str("provider generation is stale")
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
            Self::StaleConsumerGeneration { .. } => {
                formatter.write_str("consumer generation is stale")
            }
            Self::InvalidViewport => formatter.write_str("viewport start must precede end"),
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
    publications: PublicationManager,
}

impl MarketEngine {
    #[must_use]
    pub fn new(config: MarketEngineConfig) -> Self {
        Self {
            demands: DemandRegistry::new(config.maximum_consumers),
            providers: ProviderManager::new(),
            series: SeriesStore::new(config.maximum_series, config.maximum_bars),
            publications: PublicationManager::new(),
        }
    }

    /// Registers one provider capability boundary.
    ///
    /// # Errors
    /// Returns an error for invalid or duplicate provider identity.
    pub fn register_provider(
        &mut self,
        provider: String,
        capabilities: ProviderCapabilities,
    ) -> Result<(), EngineError> {
        self.providers.register(provider, capabilities)
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

    #[must_use]
    pub fn provider_status(&self, provider: &str) -> Option<ProviderStatus> {
        self.providers.status(provider)
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
        self.demands.register(identity, visible)
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
        let changed = self
            .demands
            .set_series(consumer_id, generation, series.clone())?;
        if changed {
            self.publications.remove(consumer_id);
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
        self.demands.set_visible(consumer_id, visible)
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
        let snapshot = self.series.install(
            series.clone(),
            provider_generation,
            price_scale,
            quantity_scale,
            bars,
        )?;
        self.demands
            .matching(series)
            .into_iter()
            .map(|(consumer_id, generation)| {
                self.publications
                    .publish(consumer_id, generation, Arc::clone(&snapshot))
            })
            .collect()
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
        let snapshot = self.series.install_realtime(
            series.clone(),
            provider_generation,
            price_scale,
            quantity_scale,
            bars,
            forming,
        )?;
        self.demands
            .matching(series)
            .into_iter()
            .map(|(consumer_id, generation)| {
                self.publications
                    .publish(consumer_id, generation, Arc::clone(&snapshot))
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
                self.publications.publish_update(
                    consumer_id,
                    generation,
                    series,
                    tail.provider_generation,
                    tail.forming,
                    tail.bar,
                )
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

    pub fn remove_consumer(&mut self, consumer_id: ConsumerId) -> bool {
        self.publications.remove(consumer_id);
        self.demands.remove(consumer_id)
    }

    pub fn detach_client(&mut self, client_id: ClientId) -> usize {
        let removed = self.demands.remove_client(client_id);
        for consumer_id in &removed {
            self.publications.remove(*consumer_id);
        }
        removed.len()
    }

    pub fn invalidate_series(&mut self, series: &BarSeriesKey) -> bool {
        self.publications.invalidate_series(series);
        self.series.invalidate(series)
    }

    #[must_use]
    pub fn metrics(&self) -> MarketEngineMetrics {
        MarketEngineMetrics {
            active_consumers: self.demands.len(),
            registered_providers: self.providers.len(),
            stored_series: self.series.len(),
            stored_bars: self.series.total_bars(),
            approximate_series_bytes: self.series.approximate_bytes(),
        }
    }
}

use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_market_data::BarPeriod;

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
        BarSeriesKey {
            provider_id: "coinbase".to_string(),
            instrument_id: instrument.to_string(),
            entitlement_id: "public".to_string(),
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
                ProviderCapabilities {
                    historical_bars: true,
                    realtime_bars: true,
                    order_book: false,
                },
            )
            .expect("provider registers");
        engine
            .begin_provider_session("coinbase", provider_generation(1))
            .expect("provider session begins");
        engine
    }

    fn register(engine: &mut MarketEngine, consumer: u64, client: u64) {
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(nonzero(client)),
                    workspace_id: WorkspaceId(nonzero(1)),
                    consumer_id: id(consumer),
                },
                true,
            )
            .expect("consumer registers");
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
            register(&mut engine, consumer, consumer);
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
        assert_eq!(engine.detach_client(ClientId(nonzero(1))), 2);
        assert!(engine.current_demand(id(1)).is_none());
        assert!(engine.current_demand(id(2)).is_none());
        assert!(engine.current_demand(id(3)).is_some());
        assert_eq!(engine.metrics().active_consumers, 1);
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
}
