use crate::{EngineError, ProviderGeneration};
use axiusflow_market_data::{BarSeriesKey, MarketBar};
use std::{collections::BTreeMap, mem::size_of, num::NonZeroUsize, sync::Arc};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeriesSnapshot {
    pub series: BarSeriesKey,
    pub provider_generation: ProviderGeneration,
    pub publication_generation: u64,
    pub price_scale: u8,
    pub quantity_scale: u8,
    pub forming: bool,
    pub bars: Arc<[MarketBar]>,
}

pub(crate) struct SeriesStore {
    maximum_series: NonZeroUsize,
    maximum_bars: NonZeroUsize,
    total_bars: usize,
    series: BTreeMap<BarSeriesKey, Arc<SeriesSnapshot>>,
}

#[derive(Clone, Copy)]
enum InstallMode {
    History,
    Realtime { forming: bool },
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
        price_scale: u8,
        quantity_scale: u8,
        bars: Vec<MarketBar>,
    ) -> Result<Arc<SeriesSnapshot>, EngineError> {
        self.install_inner(
            series,
            provider_generation,
            price_scale,
            quantity_scale,
            bars,
            InstallMode::History,
        )
    }

    pub(crate) fn install_realtime(
        &mut self,
        series: BarSeriesKey,
        provider_generation: ProviderGeneration,
        price_scale: u8,
        quantity_scale: u8,
        bars: Vec<MarketBar>,
        forming: bool,
    ) -> Result<Arc<SeriesSnapshot>, EngineError> {
        self.install_inner(
            series,
            provider_generation,
            price_scale,
            quantity_scale,
            bars,
            InstallMode::Realtime { forming },
        )
    }

    fn install_inner(
        &mut self,
        series: BarSeriesKey,
        provider_generation: ProviderGeneration,
        price_scale: u8,
        quantity_scale: u8,
        bars: Vec<MarketBar>,
        mode: InstallMode,
    ) -> Result<Arc<SeriesSnapshot>, EngineError> {
        let (forming, realtime) = match mode {
            InstallMode::History => (false, false),
            InstallMode::Realtime { forming } => (forming, true),
        };
        series.validate()?;
        if price_scale > 18 || quantity_scale > 18 {
            return Err(EngineError::InvalidSeriesPrecision);
        }
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
                if current.bars.as_ref() == bars
                    && current.price_scale == price_scale
                    && current.quantity_scale == quantity_scale
                    && current.forming == forming
                {
                    return Ok(Arc::clone(current));
                }
                if realtime {
                    validate_realtime_transition(current, price_scale, quantity_scale, &bars)?;
                } else {
                    validate_history_transition(current, &bars)?;
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
            price_scale,
            quantity_scale,
            forming,
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

fn validate_history_transition(
    current: &SeriesSnapshot,
    bars: &[MarketBar],
) -> Result<(), EngineError> {
    let current_first = current
        .bars
        .first()
        .ok_or(EngineError::EmptySeries)?
        .source_sequence;
    let current_last = current
        .bars
        .last()
        .ok_or(EngineError::EmptySeries)?
        .source_sequence;
    let new_first = bars
        .first()
        .ok_or(EngineError::EmptySeries)?
        .source_sequence;
    let new_last = bars.last().ok_or(EngineError::EmptySeries)?.source_sequence;
    let minimum_last = if current.forming {
        current_last.saturating_sub(1)
    } else {
        current_last.saturating_add(1)
    };
    if new_last < minimum_last
        || new_first
            > current_last
                .checked_add(1)
                .ok_or(EngineError::CapacityOverflow)?
    {
        return Err(EngineError::ConflictingSeriesGeneration(
            current.provider_generation,
        ));
    }
    for bar in bars {
        if bar.source_sequence < current_first || bar.source_sequence > current_last {
            continue;
        }
        let offset = bar
            .source_sequence
            .checked_sub(current_first)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(EngineError::CapacityOverflow)?;
        let previous = current
            .bars
            .get(offset)
            .ok_or(EngineError::CapacityOverflow)?;
        if previous != bar && !(current.forming && bar.source_sequence == current_last) {
            return Err(EngineError::ConflictingSeriesGeneration(
                current.provider_generation,
            ));
        }
    }
    Ok(())
}

fn validate_realtime_transition(
    current: &SeriesSnapshot,
    price_scale: u8,
    quantity_scale: u8,
    bars: &[MarketBar],
) -> Result<(), EngineError> {
    if current.price_scale != price_scale || current.quantity_scale != quantity_scale {
        return Err(EngineError::ConflictingSeriesGeneration(
            current.provider_generation,
        ));
    }
    let current_first = current
        .bars
        .first()
        .ok_or(EngineError::EmptySeries)?
        .source_sequence;
    let current_last = current
        .bars
        .last()
        .ok_or(EngineError::EmptySeries)?
        .source_sequence;
    let new_first = bars
        .first()
        .ok_or(EngineError::EmptySeries)?
        .source_sequence;
    let new_last = bars.last().ok_or(EngineError::EmptySeries)?.source_sequence;
    if new_last < current_last
        || new_first
            > current_last
                .checked_add(1)
                .ok_or(EngineError::CapacityOverflow)?
        || new_last == current_last && !current.forming
    {
        return Err(EngineError::ConflictingSeriesGeneration(
            current.provider_generation,
        ));
    }
    for bar in bars {
        if bar.source_sequence < current_first || bar.source_sequence > current_last {
            continue;
        }
        let offset = bar
            .source_sequence
            .checked_sub(current_first)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(EngineError::CapacityOverflow)?;
        let previous = current
            .bars
            .get(offset)
            .ok_or(EngineError::CapacityOverflow)?;
        if previous != bar && !(current.forming && bar.source_sequence == current_last) {
            return Err(EngineError::ConflictingSeriesGeneration(
                current.provider_generation,
            ));
        }
    }
    Ok(())
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
        if pair[1].exchange_timestamp_unix_nanos <= pair[0].exchange_timestamp_unix_nanos {
            return Err(EngineError::NonIncreasingSeriesTime);
        }
    }
    Ok(())
}
