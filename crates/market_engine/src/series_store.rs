use crate::{EngineError, ProviderGeneration};
use axiusflow_market_data::{BarSeriesKey, MarketBar};
use std::{collections::BTreeMap, mem::size_of, num::NonZeroUsize, sync::Arc};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeriesSnapshot {
    pub series: BarSeriesKey,
    pub provider_generation: ProviderGeneration,
    pub publication_generation: u64,
    pub bars: Arc<[MarketBar]>,
}

pub(crate) struct SeriesStore {
    maximum_series: NonZeroUsize,
    maximum_bars: NonZeroUsize,
    total_bars: usize,
    series: BTreeMap<BarSeriesKey, Arc<SeriesSnapshot>>,
}

impl SeriesStore {
    pub(crate) fn new(maximum_series: NonZeroUsize, maximum_bars: NonZeroUsize) -> Self {
        Self {
            maximum_series,
            maximum_bars,
            total_bars: 0,
            series: BTreeMap::new(),
        }
    }

    pub(crate) fn install(
        &mut self,
        series: BarSeriesKey,
        provider_generation: ProviderGeneration,
        bars: Vec<MarketBar>,
    ) -> Result<Arc<SeriesSnapshot>, EngineError> {
        series.validate()?;
        validate_bars(&bars)?;
        let current = self.series.get(&series);
        if let Some(current) = current {
            if provider_generation < current.provider_generation {
                return Err(EngineError::StaleSeriesGeneration {
                    current: current.provider_generation,
                    received: provider_generation,
                });
            }
            if provider_generation == current.provider_generation {
                if current.bars.as_ref() == bars {
                    return Ok(Arc::clone(current));
                }
                if bars.last().map(|bar| bar.source_sequence)
                    <= current.bars.last().map(|bar| bar.source_sequence)
                {
                    return Err(EngineError::ConflictingSeriesGeneration(
                        provider_generation,
                    ));
                }
            }
        } else if self.series.len() == self.maximum_series.get() {
            return Err(EngineError::SeriesLimitExceeded {
                maximum: self.maximum_series,
            });
        }
        let retained = current.map_or(0, |snapshot| snapshot.bars.len());
        let projected = self
            .total_bars
            .checked_sub(retained)
            .and_then(|count| count.checked_add(bars.len()))
            .ok_or(EngineError::CapacityOverflow)?;
        if projected > self.maximum_bars.get() {
            return Err(EngineError::BarLimitExceeded {
                maximum: self.maximum_bars,
                requested: projected,
            });
        }
        let publication_generation = match current {
            Some(snapshot) => snapshot
                .publication_generation
                .checked_add(1)
                .ok_or(EngineError::CapacityOverflow)?,
            None => 1,
        };
        let snapshot = Arc::new(SeriesSnapshot {
            series: series.clone(),
            provider_generation,
            publication_generation,
            bars: bars.into(),
        });
        self.total_bars = projected;
        self.series.insert(series, Arc::clone(&snapshot));
        Ok(snapshot)
    }

    pub(crate) fn get(&self, series: &BarSeriesKey) -> Option<Arc<SeriesSnapshot>> {
        self.series.get(series).map(Arc::clone)
    }

    pub(crate) fn invalidate(&mut self, series: &BarSeriesKey) -> bool {
        let Some(removed) = self.series.remove(series) else {
            return false;
        };
        self.total_bars = self.total_bars.saturating_sub(removed.bars.len());
        true
    }

    pub(crate) fn len(&self) -> usize {
        self.series.len()
    }

    pub(crate) fn total_bars(&self) -> usize {
        self.total_bars
    }

    pub(crate) fn approximate_bytes(&self) -> usize {
        self.total_bars.saturating_mul(size_of::<MarketBar>())
    }
}

fn validate_bars(bars: &[MarketBar]) -> Result<(), EngineError> {
    if bars.is_empty() {
        return Err(EngineError::EmptySeries);
    }
    for bar in bars {
        bar.validate()?;
    }
    for pair in bars.windows(2) {
        let expected = pair[0]
            .source_sequence
            .checked_add(1)
            .ok_or(EngineError::CapacityOverflow)?;
        if pair[1].source_sequence != expected {
            return Err(EngineError::DiscontinuousSeries {
                expected,
                received: pair[1].source_sequence,
            });
        }
        if pair[1].exchange_timestamp_seconds <= pair[0].exchange_timestamp_seconds {
            return Err(EngineError::NonIncreasingSeriesTime);
        }
    }
    Ok(())
}
