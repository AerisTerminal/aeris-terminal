//! Canonical coordinator-shaped events for disconnected desktop fixtures.

use std::{num::NonZeroU64, sync::Arc};

use aeris_contracts::InstallProviderInstrument;
use aeris_market_data::{
    AggressorSide, BarSeriesKey, EventMetadata, MarketBar, MarketTrade, OrderBookPublication,
    QualifiedTimestamp,
};
use aeris_market_runtime::{
    CanonicalMarketSeriesSnapshot, MarketConsumerEvent, MarketConsumerId, MarketDemandError,
    MarketDemandErrorScope, MarketGenerationId, MarketOrderBookSnapshot, MarketProviderGeneration,
    MarketRuntimeEvent, MarketSeriesSnapshot, MarketSeriesState, MarketSeriesUpdate,
    MarketStudyRemoved, MarketTradeTapeOperation, MarketTradeTapeUpdate, RetainedMarketTrade,
    SeriesTailOperation,
    fence::{ConsumerTag, EpochSequence, Fenced},
};
use aeris_market_runtime::{EngineFaultCode, FailureStage, SeriesLoadState};

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

/// A covering tape `Reset` of buy prints with ordinals `1..=last_ordinal` for `product`
/// under selection `generation`, at provider generation 1 and scale 0.
///
/// # Panics
/// Panics if `generation` is zero.
#[must_use]
pub fn fixture_trade_tape(
    product: &InstallProviderInstrument,
    generation: u64,
    revision: u64,
    last_ordinal: u64,
) -> MarketTradeTapeUpdate {
    let trades = (1..=last_ordinal)
        .map(|ordinal| {
            let seconds = 1_700_000_000 + i64::try_from(ordinal).expect("small ordinal");
            RetainedMarketTrade {
                ingestion_ordinal: ordinal,
                observed_unix_nanos: seconds * 1_000_000_000,
                trading_day: None,
                trade: Arc::new(MarketTrade {
                    metadata: EventMetadata {
                        provider_id: product.provider.clone(),
                        instrument_id: product.instrument_id.clone(),
                        entitlement_id: product.entitlement_id.clone(),
                        session_generation: 1,
                        source_sequence: ordinal,
                        timestamps: QualifiedTimestamp {
                            exchange_unix_nanos: Some(seconds * 1_000_000_000),
                            provider_unix_nanos: None,
                            received_unix_nanos: seconds * 1_000_000_000,
                        },
                    },
                    trade_id: format!("trade-{ordinal}"),
                    price: 100,
                    quantity: 2,
                    aggressor: AggressorSide::Buy,
                }),
            }
        })
        .collect::<Vec<_>>();
    MarketTradeTapeUpdate {
        consumer_id: MarketConsumerId(NonZeroU64::MIN),
        generation: FixtureMarketEvents::generation(generation),
        provider_id: product.provider.clone(),
        instrument_id: product.instrument_id.clone(),
        entitlement_id: product.entitlement_id.clone(),
        provider_generation: 1,
        revision,
        source_watermark: last_ordinal,
        rewrite_generation: 0,
        price_scale: 0,
        quantity_scale: 0,
        sweeps: Arc::from([]),
        operation: MarketTradeTapeOperation::Reset {
            trades: trades.into(),
        },
    }
}

impl Default for FixtureMarketEvents {
    fn default() -> Self {
        Self::new(MarketConsumerId(NonZeroU64::MIN))
    }
}

impl FixtureMarketEvents {
    #[must_use]
    pub fn catalog(&self, event: MarketRuntimeEvent) -> MarketConsumerEvent {
        Fenced {
            epoch: self.envelope.epoch,
            value: event,
        }
    }

    #[must_use]
    pub fn trade_tape(&self, mut update: MarketTradeTapeUpdate) -> MarketConsumerEvent {
        update.consumer_id = self.consumer_id;
        Fenced {
            epoch: self.envelope.epoch,
            value: MarketRuntimeEvent::TradeTapeUpdate(update),
        }
    }

    #[must_use]
    pub fn study_removed(&self, mut removed: MarketStudyRemoved) -> MarketConsumerEvent {
        removed.consumer_id = self.consumer_id;
        Fenced {
            epoch: self.envelope.epoch,
            value: MarketRuntimeEvent::StudyRemoved(removed),
        }
    }

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
                order_levels: None,
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
