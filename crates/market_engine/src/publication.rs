use crate::series_store::SeriesSnapshot;
use crate::{ConsumerId, EngineError, GenerationId};
use axiusflow_market_data::BarSeriesKey;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsumerPublication {
    pub consumer_id: ConsumerId,
    pub generation: GenerationId,
    pub publication_generation: u64,
    pub snapshot: Arc<SeriesSnapshot>,
}

pub(crate) struct PublicationManager {
    latest: BTreeMap<ConsumerId, ConsumerPublication>,
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
        let publication_generation = match self.latest.get(&consumer_id) {
            Some(publication) => publication
                .publication_generation
                .checked_add(1)
                .ok_or(EngineError::CapacityOverflow)?,
            None => 1,
        };
        let publication = ConsumerPublication {
            consumer_id,
            generation,
            publication_generation,
            snapshot,
        };
        self.latest.insert(consumer_id, publication.clone());
        Ok(publication)
    }

    pub(crate) fn latest(&self, consumer_id: ConsumerId) -> Option<&ConsumerPublication> {
        self.latest.get(&consumer_id)
    }

    pub(crate) fn remove(&mut self, consumer_id: ConsumerId) {
        self.latest.remove(&consumer_id);
    }

    pub(crate) fn invalidate_series(&mut self, series: &BarSeriesKey) {
        self.latest
            .retain(|_, publication| &publication.snapshot.series != series);
    }
}
