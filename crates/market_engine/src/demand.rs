use crate::{ClientId, ConsumerId, EngineError, GenerationId, Viewport, WorkspaceId};
use axiusflow_market_data::BarSeriesKey;
use std::{collections::BTreeMap, num::NonZeroUsize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketStream {
    Bars,
    Trades,
    Quotes,
    Depth,
}

/// Exact presentation/retention state for one engine consumer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsumerResourceClass {
    Foreground,
    Background,
    Detached,
}

impl ConsumerResourceClass {
    #[must_use]
    pub const fn publishes_ui(self) -> bool {
        matches!(self, Self::Foreground)
    }

    #[must_use]
    pub const fn retains_subscription(self) -> bool {
        matches!(self, Self::Foreground)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamRequirements(u8);

impl StreamRequirements {
    const BARS_BIT: u8 = 1 << 0;
    const TRADES_BIT: u8 = 1 << 1;
    const QUOTES_BIT: u8 = 1 << 2;
    const DEPTH_BIT: u8 = 1 << 3;

    pub const NONE: Self = Self(0);
    pub const BARS: Self = Self(Self::BARS_BIT);

    #[must_use]
    pub const fn with(self, stream: MarketStream) -> Self {
        let bit = match stream {
            MarketStream::Bars => Self::BARS_BIT,
            MarketStream::Trades => Self::TRADES_BIT,
            MarketStream::Quotes => Self::QUOTES_BIT,
            MarketStream::Depth => Self::DEPTH_BIT,
        };
        Self(self.0 | bit)
    }

    #[must_use]
    pub const fn contains(self, stream: MarketStream) -> bool {
        let bit = match stream {
            MarketStream::Bars => Self::BARS_BIT,
            MarketStream::Trades => Self::TRADES_BIT,
            MarketStream::Quotes => Self::QUOTES_BIT,
            MarketStream::Depth => Self::DEPTH_BIT,
        };
        self.0 & bit != 0
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConsumerIdentity {
    pub client_id: ClientId,
    pub workspace_id: WorkspaceId,
    pub consumer_id: ConsumerId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsumerDemand {
    pub identity: ConsumerIdentity,
    pub generation: Option<GenerationId>,
    pub series: Option<BarSeriesKey>,
    pub streams: Option<StreamRequirements>,
    pub viewport: Option<Viewport>,
    pub resource_class: ConsumerResourceClass,
}

pub(crate) struct DemandRegistry {
    maximum_consumers: NonZeroUsize,
    consumers: BTreeMap<ConsumerId, ConsumerDemand>,
}

impl DemandRegistry {
    pub(crate) fn new(maximum_consumers: NonZeroUsize) -> Self {
        Self {
            maximum_consumers,
            consumers: BTreeMap::new(),
        }
    }

    pub(crate) fn register(
        &mut self,
        identity: ConsumerIdentity,
        resource_class: ConsumerResourceClass,
    ) -> Result<(), EngineError> {
        if self.consumers.contains_key(&identity.consumer_id) {
            return Err(EngineError::DuplicateConsumer(identity.consumer_id));
        }
        if self.consumers.len() == self.maximum_consumers.get() {
            return Err(EngineError::ConsumerLimitExceeded {
                maximum: self.maximum_consumers,
            });
        }
        self.consumers.insert(
            identity.consumer_id,
            ConsumerDemand {
                identity,
                generation: None,
                series: None,
                streams: None,
                viewport: None,
                resource_class,
            },
        );
        Ok(())
    }

    pub(crate) fn set_series(
        &mut self,
        consumer_id: ConsumerId,
        generation: GenerationId,
        series: BarSeriesKey,
        streams: StreamRequirements,
    ) -> Result<bool, EngineError> {
        series.validate()?;
        if streams.is_empty() {
            return Err(EngineError::EmptyStreamRequirements);
        }
        let demand = self
            .consumers
            .get_mut(&consumer_id)
            .ok_or(EngineError::UnknownConsumer(consumer_id))?;
        if let Some(current) = demand.generation {
            if generation < current
                || generation == current
                    && (demand.series.as_ref() != Some(&series) || demand.streams != Some(streams))
            {
                return Err(EngineError::StaleConsumerGeneration {
                    consumer_id,
                    current,
                    received: generation,
                });
            }
            if generation == current {
                return Ok(false);
            }
        }
        demand.generation = Some(generation);
        demand.series = Some(series);
        demand.streams = Some(streams);
        demand.viewport = None;
        Ok(true)
    }

    pub(crate) fn set_viewport(
        &mut self,
        consumer_id: ConsumerId,
        generation: GenerationId,
        viewport: Viewport,
    ) -> Result<(), EngineError> {
        let demand = self
            .consumers
            .get_mut(&consumer_id)
            .ok_or(EngineError::UnknownConsumer(consumer_id))?;
        let current = demand
            .generation
            .ok_or(EngineError::ConsumerHasNoSeries(consumer_id))?;
        if generation != current {
            return Err(EngineError::StaleConsumerGeneration {
                consumer_id,
                current,
                received: generation,
            });
        }
        demand.viewport = Some(viewport);
        Ok(())
    }

    pub(crate) fn set_streams(
        &mut self,
        consumer_id: ConsumerId,
        generation: GenerationId,
        streams: StreamRequirements,
    ) -> Result<bool, EngineError> {
        if streams.is_empty() {
            return Err(EngineError::EmptyStreamRequirements);
        }
        let demand = self
            .consumers
            .get_mut(&consumer_id)
            .ok_or(EngineError::UnknownConsumer(consumer_id))?;
        let current = demand
            .generation
            .ok_or(EngineError::ConsumerHasNoSeries(consumer_id))?;
        if generation != current {
            return Err(EngineError::StaleConsumerGeneration {
                consumer_id,
                current,
                received: generation,
            });
        }
        if demand.streams == Some(streams) {
            return Ok(false);
        }
        demand.streams = Some(streams);
        Ok(true)
    }

    pub(crate) fn set_resource_class(
        &mut self,
        consumer_id: ConsumerId,
        resource_class: ConsumerResourceClass,
    ) -> Result<bool, EngineError> {
        let demand = self
            .consumers
            .get_mut(&consumer_id)
            .ok_or(EngineError::UnknownConsumer(consumer_id))?;
        if demand.resource_class == resource_class {
            return Ok(false);
        }
        demand.resource_class = resource_class;
        Ok(true)
    }

    pub(crate) fn current(&self, consumer_id: ConsumerId) -> Option<&ConsumerDemand> {
        self.consumers.get(&consumer_id)
    }

    pub(crate) fn retained_viewports(
        &self,
        series: &BarSeriesKey,
    ) -> Vec<(ConsumerId, GenerationId, Viewport)> {
        self.consumers
            .values()
            .filter_map(|demand| {
                (demand.resource_class.retains_subscription()
                    && demand.series.as_ref() == Some(series))
                .then_some((
                    demand.identity.consumer_id,
                    demand.generation?,
                    demand.viewport?,
                ))
            })
            .collect()
    }

    pub(crate) fn primary_retained_viewport(
        &self,
        series: &BarSeriesKey,
    ) -> Option<(ConsumerId, GenerationId, Viewport)> {
        self.consumers.values().find_map(|demand| {
            (demand.resource_class.publishes_ui() && demand.series.as_ref() == Some(series))
                .then_some((
                    demand.identity.consumer_id,
                    demand.generation?,
                    demand.viewport?,
                ))
        })
    }

    pub(crate) fn matching(&self, series: &BarSeriesKey) -> Vec<(ConsumerId, GenerationId)> {
        self.consumers
            .values()
            .filter_map(|demand| {
                (demand.resource_class.publishes_ui() && demand.series.as_ref() == Some(series))
                    .then_some(demand.generation)
                    .flatten()
                    .map(|generation| (demand.identity.consumer_id, generation))
            })
            .collect()
    }

    pub(crate) fn referenced_series(&self) -> Vec<BarSeriesKey> {
        self.consumers
            .values()
            .filter_map(|demand| demand.series.clone())
            .collect()
    }

    pub(crate) fn remove(&mut self, consumer_id: ConsumerId) -> Option<ConsumerDemand> {
        self.consumers.remove(&consumer_id)
    }

    pub(crate) fn remove_client(&mut self, client_id: ClientId) -> Vec<ConsumerDemand> {
        let removed = self
            .consumers
            .values()
            .filter(|demand| demand.identity.client_id == client_id)
            .cloned()
            .collect::<Vec<_>>();
        self.consumers
            .retain(|_, demand| demand.identity.client_id != client_id);
        removed
    }

    pub(crate) fn len(&self) -> usize {
        self.consumers.len()
    }

    pub(crate) fn foreground_len(&self) -> usize {
        self.consumers
            .values()
            .filter(|demand| demand.resource_class == ConsumerResourceClass::Foreground)
            .count()
    }
}
