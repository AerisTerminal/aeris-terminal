//! Canonical coordinator-shaped events for disconnected desktop fixtures.

use std::{num::NonZeroU64, sync::Arc};

use aeris_contracts::{EngineFaultCode, FailureStage, SeriesLoadState};
use aeris_market_data::{BarSeriesKey, MarketBar, OrderBookPublication};
use aeris_market_runtime::{
    CanonicalMarketSeriesSnapshot, MarketConsumerEvent, MarketConsumerId, MarketDemandError,
    MarketDemandErrorScope, MarketGenerationId, MarketOrderBookSnapshot, MarketProviderGeneration,
    MarketRuntimeEvent, MarketSeriesSnapshot, MarketSeriesState, MarketSeriesUpdate,
    SeriesTailOperation,
    fence::{ConsumerTag, EpochSequence, Fenced},
};

/// One registered fixture consumer lifetime, with its own coordinator epoch.
pub struct FixtureMarketEvents {
    envelope: Fenced<ConsumerTag, ()>,
    consumer_id: MarketConsumerId,
}

/// Validated fixture routing and precision for one canonical bar series.
pub struct FixtureSeries {
    pub series: BarSeriesKey,
    pub provider_generation: u64,
    pub price_scale: u8,
    pub quantity_scale: u8,
}

impl Default for FixtureMarketEvents {
    fn default() -> Self {
        Self::new(MarketConsumerId(NonZeroU64::MIN))
    }
}

impl FixtureMarketEvents {
    #[must_use]
    pub fn order_book(
        &self,
        selection: u64,
        publication: OrderBookPublication,
    ) -> MarketConsumerEvent {
        Fenced {
            epoch: self.envelope.epoch,
            value: MarketRuntimeEvent::OrderBookSnapshot(MarketOrderBookSnapshot {
                consumer_id: self.consumer_id,
                generation: Self::generation(selection),
                publication,
                display_depth: None,
            }),
        }
    }
    #[must_use]
    pub fn new(consumer_id: MarketConsumerId) -> Self {
        Self {
            envelope: Fenced {
                epoch: EpochSequence::<ConsumerTag>::default().next(),
                value: (),
            },
            consumer_id,
        }
    }

    fn generation(selection: u64) -> MarketGenerationId {
        MarketGenerationId(NonZeroU64::new(selection).expect("fixture selection is nonzero"))
    }

    /// # Panics
    /// Panics if the fixture series or provider/selection generation is invalid.
    #[must_use]
    pub fn snapshot(
        &self,
        route: &FixtureSeries,
        selection: u64,
        publication_generation: u64,
        bars: impl Into<Arc<[MarketBar]>>,
    ) -> MarketConsumerEvent {
        route
            .series
            .validate()
            .expect("fixture series identity is valid");
        let snapshot = CanonicalMarketSeriesSnapshot {
            series: route.series.clone(),
            provider_generation: MarketProviderGeneration(
                NonZeroU64::new(route.provider_generation).expect("fixture provider generation"),
            ),
            publication_generation,
            price_scale: route.price_scale,
            quantity_scale: route.quantity_scale,
            forming: false,
            bars: bars.into(),
        };
        Fenced {
            epoch: self.envelope.epoch,
            value: MarketRuntimeEvent::SeriesSnapshot(MarketSeriesSnapshot {
                consumer_id: self.consumer_id,
                generation: Self::generation(selection),
                publication_generation,
                snapshot: Arc::new(snapshot),
            }),
        }
    }

    /// # Panics
    /// Panics if the fixture series or provider/selection generation is invalid.
    #[must_use]
    pub fn update(
        &self,
        route: &FixtureSeries,
        selection: u64,
        publication_generation: u64,
        operation: SeriesTailOperation,
        bar: MarketBar,
        forming: bool,
    ) -> MarketConsumerEvent {
        route
            .series
            .validate()
            .expect("fixture series identity is valid");
        Fenced {
            epoch: self.envelope.epoch,
            value: MarketRuntimeEvent::SeriesUpdate(MarketSeriesUpdate {
                consumer_id: self.consumer_id,
                generation: Self::generation(selection),
                publication_generation,
                series: route.series.clone(),
                provider_generation: MarketProviderGeneration(
                    NonZeroU64::new(route.provider_generation)
                        .expect("fixture provider generation"),
                ),
                forming,
                operation,
                bar,
            }),
        }
    }

    #[must_use]
    pub fn readiness(
        &self,
        selection: u64,
        state: SeriesLoadState,
        detail: impl Into<String>,
    ) -> MarketConsumerEvent {
        Fenced {
            epoch: self.envelope.epoch,
            value: MarketRuntimeEvent::SeriesState(MarketSeriesState {
                consumer_id: self.consumer_id,
                generation: Self::generation(selection),
                series: None,
                state,
                detail: Some(detail.into()),
                presentation: None,
            }),
        }
    }

    #[must_use]
    pub fn readiness_with_presentation(
        &self,
        selection: u64,
        state: SeriesLoadState,
        detail: impl Into<String>,
        presentation: aeris_market_runtime::SeriesPresentationStatus,
    ) -> MarketConsumerEvent {
        Fenced {
            epoch: self.envelope.epoch,
            value: MarketRuntimeEvent::SeriesState(MarketSeriesState {
                consumer_id: self.consumer_id,
                generation: Self::generation(selection),
                series: None,
                state,
                detail: Some(detail.into()),
                presentation: Some(presentation),
            }),
        }
    }

    #[must_use]
    pub fn demand_error(
        &self,
        selection: u64,
        scope: MarketDemandErrorScope,
        detail: impl Into<String>,
    ) -> MarketConsumerEvent {
        Fenced {
            epoch: self.envelope.epoch,
            value: MarketRuntimeEvent::DemandError(MarketDemandError {
                consumer_id: self.consumer_id,
                generation: Self::generation(selection),
                scope,
                code: EngineFaultCode::Retryable,
                stage: FailureStage::ProviderHistory,
                detail: detail.into(),
                series: None,
                cause: "fixture".into(),
                elapsed_millis: None,
            }),
        }
    }
}
