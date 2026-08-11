use crate::{ClientId, ConsumerId, EngineError, GenerationId, Viewport, WorkspaceId};
use axiusflow_market_data::BarSeriesKey;
use std::{collections::BTreeMap, num::NonZeroUsize};

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
    pub viewport: Option<Viewport>,
    pub visible: bool,
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
        visible: bool,
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
                viewport: None,
                visible,
            },
        );
        Ok(())
    }

    pub(crate) fn set_series(
        &mut self,
        consumer_id: ConsumerId,
        generation: GenerationId,
        series: BarSeriesKey,
    ) -> Result<bool, EngineError> {
        series.validate()?;
        let demand = self
            .consumers
            .get_mut(&consumer_id)
            .ok_or(EngineError::UnknownConsumer(consumer_id))?;
        if let Some(current) = demand.generation {
            if generation < current
                || generation == current && demand.series.as_ref() != Some(&series)
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

    pub(crate) fn set_visible(
        &mut self,
        consumer_id: ConsumerId,
        visible: bool,
    ) -> Result<(), EngineError> {
        let demand = self
            .consumers
            .get_mut(&consumer_id)
            .ok_or(EngineError::UnknownConsumer(consumer_id))?;
        demand.visible = visible;
        Ok(())
    }

    pub(crate) fn current(&self, consumer_id: ConsumerId) -> Option<&ConsumerDemand> {
        self.consumers.get(&consumer_id)
    }

    pub(crate) fn matching(&self, series: &BarSeriesKey) -> Vec<(ConsumerId, GenerationId)> {
        self.consumers
            .values()
            .filter_map(|demand| {
                (demand.series.as_ref() == Some(series))
                    .then_some(demand.generation)
                    .flatten()
                    .map(|generation| (demand.identity.consumer_id, generation))
            })
            .collect()
    }

    pub(crate) fn remove(&mut self, consumer_id: ConsumerId) -> bool {
        self.consumers.remove(&consumer_id).is_some()
    }

    pub(crate) fn remove_client(&mut self, client_id: ClientId) -> Vec<ConsumerId> {
        let removed = self
            .consumers
            .values()
            .filter(|demand| demand.identity.client_id == client_id)
            .map(|demand| demand.identity.consumer_id)
            .collect::<Vec<_>>();
        self.consumers
            .retain(|_, demand| demand.identity.client_id != client_id);
        removed
    }

    pub(crate) fn len(&self) -> usize {
        self.consumers.len()
    }
}
