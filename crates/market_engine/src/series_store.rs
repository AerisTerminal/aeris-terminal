use crate::{EngineError, ProviderGeneration, Viewport};
use axiusflow_market_data::{BarPeriod, BarSeriesKey, MarketBar};
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SeriesTail {
    pub provider_generation: ProviderGeneration,
    pub publication_generation: u64,
    pub price_scale: u8,
    pub quantity_scale: u8,
    pub forming: bool,
    pub bar: MarketBar,
}

struct StoredSeries {
    covering: Arc<SeriesSnapshot>,
    tail: Option<SeriesTail>,
}

pub(crate) struct SeriesStore {
    maximum_series: NonZeroUsize,
    maximum_bars: NonZeroUsize,
    total_bars: usize,
    series: BTreeMap<BarSeriesKey, StoredSeries>,
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
            if provider_generation < current.covering.provider_generation {
                return Err(EngineError::StaleSeriesGeneration {
                    current: current.covering.provider_generation,
                    received: provider_generation,
                });
            }
            if provider_generation == current.covering.provider_generation {
                let current_snapshot = current.snapshot();
                if current_snapshot.bars.as_ref() == bars
                    && current_snapshot.price_scale == price_scale
                    && current_snapshot.quantity_scale == quantity_scale
                    && current_snapshot.forming == forming
                {
                    return Ok(current_snapshot);
                }
                if realtime {
                    validate_realtime_transition(
                        &current_snapshot,
                        price_scale,
                        quantity_scale,
                        &bars,
                    )?;
                } else {
                    validate_history_transition(&current_snapshot, &bars)?;
                }
            }
        } else if self.series.len() == self.maximum_series.get() {
            return Err(EngineError::SeriesLimitExceeded {
                maximum: self.maximum_series,
            });
        }
        let retained = current.map_or(0, StoredSeries::bar_count);
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
                .covering
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
        let stored = stored_series(&snapshot, realtime)?;
        self.series.insert(series, stored);
        Ok(snapshot)
    }

    pub(crate) fn install_realtime_tail(
        &mut self,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        price_scale: u8,
        quantity_scale: u8,
        bar: MarketBar,
        forming: bool,
    ) -> Result<SeriesTail, EngineError> {
        bar.validate()?;
        let current = self
            .series
            .get_mut(series)
            .ok_or(EngineError::EmptySeries)?;
        if provider_generation != current.covering.provider_generation {
            return Err(EngineError::StaleSeriesGeneration {
                current: current.covering.provider_generation,
                received: provider_generation,
            });
        }
        if current.covering.price_scale != price_scale
            || current.covering.quantity_scale != quantity_scale
        {
            return Err(EngineError::ConflictingSeriesGeneration(
                provider_generation,
            ));
        }

        let previous = current.tail.map_or_else(
            || current.covering.bars.last().copied(),
            |tail| Some(tail.bar),
        );
        let previous = previous.ok_or(EngineError::EmptySeries)?;
        if bar.source_sequence == previous.source_sequence {
            if current.tail.is_none()
                || bar.exchange_timestamp_unix_nanos != previous.exchange_timestamp_unix_nanos
            {
                return Err(EngineError::ConflictingSeriesGeneration(
                    provider_generation,
                ));
            }
        } else {
            let expected = previous
                .source_sequence
                .checked_add(1)
                .ok_or(EngineError::CapacityOverflow)?;
            if bar.source_sequence != expected
                || bar.exchange_timestamp_unix_nanos <= previous.exchange_timestamp_unix_nanos
            {
                return Err(EngineError::ConflictingSeriesGeneration(
                    provider_generation,
                ));
            }
            if let Some(tail) = current.tail.take() {
                let mut completed = current.covering.bars.to_vec();
                completed.push(tail.bar);
                current.covering = Arc::new(SeriesSnapshot {
                    series: current.covering.series.clone(),
                    provider_generation,
                    publication_generation: tail.publication_generation,
                    price_scale,
                    quantity_scale,
                    forming: false,
                    bars: completed.into(),
                });
            } else {
                self.total_bars = self
                    .total_bars
                    .checked_add(1)
                    .ok_or(EngineError::CapacityOverflow)?;
                if self.total_bars > self.maximum_bars.get() {
                    self.total_bars = self.total_bars.saturating_sub(1);
                    return Err(EngineError::BarLimitExceeded {
                        maximum: self.maximum_bars,
                        requested: self.total_bars.saturating_add(1),
                    });
                }
            }
        }
        let publication_generation = current
            .tail
            .map_or(current.covering.publication_generation, |tail| {
                tail.publication_generation
            })
            .checked_add(1)
            .ok_or(EngineError::CapacityOverflow)?;
        let tail = SeriesTail {
            provider_generation,
            publication_generation,
            price_scale,
            quantity_scale,
            forming,
            bar,
        };
        current.tail = Some(tail);
        Ok(tail)
    }

    pub(crate) fn get(&self, series: &BarSeriesKey) -> Option<Arc<SeriesSnapshot>> {
        self.series.get(series).map(StoredSeries::snapshot)
    }

    pub(crate) fn compatible_source(
        &self,
        target: &BarSeriesKey,
        provider_generation: ProviderGeneration,
    ) -> Option<Arc<SeriesSnapshot>> {
        let BarPeriod::Time {
            seconds: target_seconds,
        } = target.period
        else {
            return None;
        };
        self.series
            .iter()
            .filter_map(|(candidate, stored)| {
                let BarPeriod::Time {
                    seconds: source_seconds,
                } = candidate.period
                else {
                    return None;
                };
                (candidate.provider_id == target.provider_id
                    && candidate.instrument_id == target.instrument_id
                    && candidate.entitlement_id == target.entitlement_id
                    && candidate.definition_version == target.definition_version
                    && source_seconds < target_seconds
                    && target_seconds % source_seconds == 0
                    && stored.covering.provider_generation == provider_generation)
                    .then_some((source_seconds, stored.snapshot()))
            })
            .max_by_key(|(source_seconds, _)| *source_seconds)
            .map(|(_, snapshot)| snapshot)
    }

    pub(crate) fn range(
        &self,
        series: &BarSeriesKey,
        viewport: Viewport,
    ) -> Option<Arc<SeriesSnapshot>> {
        let source = self.get(series)?;
        let bars = source
            .bars
            .iter()
            .copied()
            .filter(|bar| {
                bar.exchange_timestamp_unix_nanos >= viewport.start_unix_nanos
                    && bar.exchange_timestamp_unix_nanos < viewport.end_unix_nanos
            })
            .collect::<Vec<_>>();
        let last_is_source_tail = bars.last() == source.bars.last();
        (!bars.is_empty()).then(|| {
            Arc::new(SeriesSnapshot {
                series: source.series.clone(),
                provider_generation: source.provider_generation,
                publication_generation: source.publication_generation,
                price_scale: source.price_scale,
                quantity_scale: source.quantity_scale,
                forming: source.forming && last_is_source_tail,
                bars: bars.into(),
            })
        })
    }

    pub(crate) fn invalidate(&mut self, series: &BarSeriesKey) -> bool {
        let Some(removed) = self.series.remove(series) else {
            return false;
        };
        self.total_bars = self.total_bars.saturating_sub(removed.bar_count());
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

    #[cfg(test)]
    pub(crate) fn completed_bars(&self, series: &BarSeriesKey) -> Option<Arc<[MarketBar]>> {
        self.series
            .get(series)
            .map(|stored| Arc::clone(&stored.covering.bars))
    }
}

impl StoredSeries {
    fn bar_count(&self) -> usize {
        self.covering.bars.len() + usize::from(self.tail.is_some())
    }

    fn snapshot(&self) -> Arc<SeriesSnapshot> {
        let Some(tail) = self.tail else {
            return Arc::clone(&self.covering);
        };
        let mut bars = self.covering.bars.to_vec();
        bars.push(tail.bar);
        Arc::new(SeriesSnapshot {
            series: self.covering.series.clone(),
            provider_generation: tail.provider_generation,
            publication_generation: tail.publication_generation,
            price_scale: tail.price_scale,
            quantity_scale: tail.quantity_scale,
            forming: tail.forming,
            bars: bars.into(),
        })
    }
}

fn stored_series(
    snapshot: &Arc<SeriesSnapshot>,
    realtime: bool,
) -> Result<StoredSeries, EngineError> {
    if !realtime || !snapshot.forming {
        return Ok(StoredSeries {
            covering: Arc::clone(snapshot),
            tail: None,
        });
    }
    let (tail, completed) = snapshot.bars.split_last().ok_or(EngineError::EmptySeries)?;
    Ok(StoredSeries {
        covering: Arc::new(SeriesSnapshot {
            series: snapshot.series.clone(),
            provider_generation: snapshot.provider_generation,
            publication_generation: snapshot.publication_generation,
            price_scale: snapshot.price_scale,
            quantity_scale: snapshot.quantity_scale,
            forming: false,
            bars: completed.into(),
        }),
        tail: Some(SeriesTail {
            provider_generation: snapshot.provider_generation,
            publication_generation: snapshot.publication_generation,
            price_scale: snapshot.price_scale,
            quantity_scale: snapshot.quantity_scale,
            forming: snapshot.forming,
            bar: *tail,
        }),
    })
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
