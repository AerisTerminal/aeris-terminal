use crate::series_store::SeriesSnapshot;
use crate::{ConsumerId, EngineError, GenerationId, ProviderGeneration};
use axiusflow_market_data::{BarSeriesKey, MarketBar};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsumerPublication {
    pub consumer_id: ConsumerId,
    pub generation: GenerationId,
    pub publication_generation: u64,
    pub snapshot: Arc<SeriesSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsumerSeriesUpdate {
    pub consumer_id: ConsumerId,
    pub generation: GenerationId,
    pub publication_generation: u64,
    pub series: BarSeriesKey,
    pub provider_generation: ProviderGeneration,
    pub forming: bool,
    pub bar: MarketBar,
}

struct PublicationState {
    series: BarSeriesKey,
    publication_generation: u64,
    active: bool,
}

pub(crate) struct PublicationManager {
    latest: BTreeMap<ConsumerId, PublicationState>,
}

impl PublicationManager {
    pub(crate) fn new() -> Self {
        Self {
            latest: BTreeMap::new(),
        }
    }

    pub(crate) fn publish(
        &mut self,
        consumer_id: ConsumerId,
        generation: GenerationId,
        snapshot: Arc<SeriesSnapshot>,
    ) -> Result<ConsumerPublication, EngineError> {
        let publication_generation = self.next_generation(consumer_id)?;
        let publication = ConsumerPublication {
            consumer_id,
            generation,
            publication_generation,
            snapshot,
        };
        self.latest.insert(
            consumer_id,
            PublicationState {
                series: publication.snapshot.series.clone(),
                publication_generation,
                active: true,
            },
        );
        Ok(publication)
    }

    pub(crate) fn publish_update(
        &mut self,
        consumer_id: ConsumerId,
        generation: GenerationId,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        forming: bool,
        bar: MarketBar,
    ) -> Result<ConsumerSeriesUpdate, EngineError> {
        let publication_generation = self.next_generation(consumer_id)?;
        self.latest.insert(
            consumer_id,
            PublicationState {
                series: series.clone(),
                publication_generation,
                active: true,
            },
        );
        Ok(ConsumerSeriesUpdate {
            consumer_id,
            generation,
            publication_generation,
            series: series.clone(),
            provider_generation,
            forming,
            bar,
        })
    }

    pub(crate) fn contains(&self, consumer_id: ConsumerId) -> bool {
        self.latest
            .get(&consumer_id)
            .is_some_and(|publication| publication.active)
    }

    pub(crate) fn suspend(&mut self, consumer_id: ConsumerId) {
        if let Some(publication) = self.latest.get_mut(&consumer_id) {
            publication.active = false;
        }
    }

    pub(crate) fn remove(&mut self, consumer_id: ConsumerId) {
        self.latest.remove(&consumer_id);
    }

    /// Retires the publications bound to one series without rewinding anyone.
    ///
    /// The consumer's publication generation is its ordering contract with the
    /// client: a client rejects a snapshot that does not advance it. Dropping the
    /// entry restarted that counter at 1, so the covering snapshot that follows a
    /// series invalidation read as stale and the chart stopped on it.
    pub(crate) fn invalidate_series(&mut self, series: &BarSeriesKey) {
        for publication in self.latest.values_mut() {
            if &publication.series == series {
                publication.active = false;
            }
        }
    }

    fn next_generation(&self, consumer_id: ConsumerId) -> Result<u64, EngineError> {
        self.latest.get(&consumer_id).map_or(Ok(1), |current| {
            current
                .publication_generation
                .checked_add(1)
                .ok_or(EngineError::CapacityOverflow)
        })
    }
}
