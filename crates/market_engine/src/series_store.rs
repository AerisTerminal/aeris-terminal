use crate::{EngineError, ProviderGeneration, Viewport};
use axiusflow_market_data::{BarPeriod, BarSeriesKey, MarketBar};
use std::{collections::BTreeMap, mem::size_of, num::NonZeroUsize, sync::Arc};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeriesTailOperation {
    Revise,
    Append,
}

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
    pub operation: SeriesTailOperation,
    pub bar: MarketBar,
}

struct StoredSeries {
    covering: Arc<SeriesSnapshot>,
    completed_tail: Vec<MarketBar>,
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
    Repair,
    Window,
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
        bars: &[MarketBar],
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
        bars: &[MarketBar],
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

    pub(crate) fn replace_covering(
        &mut self,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        price_scale: u8,
        quantity_scale: u8,
        bars: &[MarketBar],
    ) -> Result<Arc<SeriesSnapshot>, EngineError> {
        self.install_inner(
            series.clone(),
            provider_generation,
            price_scale,
            quantity_scale,
            bars,
            InstallMode::Repair,
        )
    }

    pub(crate) fn replace_window(
        &mut self,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        price_scale: u8,
        quantity_scale: u8,
        bars: &[MarketBar],
    ) -> Result<Arc<SeriesSnapshot>, EngineError> {
        self.install_inner(
            series.clone(),
            provider_generation,
            price_scale,
            quantity_scale,
            bars,
            InstallMode::Window,
        )
    }

    fn install_inner(
        &mut self,
        series: BarSeriesKey,
        provider_generation: ProviderGeneration,
        price_scale: u8,
        quantity_scale: u8,
        bars: &[MarketBar],
        mode: InstallMode,
    ) -> Result<Arc<SeriesSnapshot>, EngineError> {
        let (forming, realtime) = match mode {
            InstallMode::History | InstallMode::Repair | InstallMode::Window => (false, false),
            InstallMode::Realtime { forming } => (forming, true),
        };
        validate_install_inputs(&series, price_scale, quantity_scale, bars)?;
        let current = self.series.get(&series);
        if let Some(snapshot) = Self::validate_current_install(
            current,
            provider_generation,
            price_scale,
            quantity_scale,
            bars,
            mode,
        )? {
            return Ok(snapshot);
        }
        if current.is_none() && self.series.len() == self.maximum_series.get() {
            return Err(EngineError::SeriesLimitExceeded {
                maximum: self.maximum_series,
            });
        }
        let (mut retained_tail, tail_overlaps_last) = retained_tail_for_install(
            current,
            mode,
            provider_generation,
            price_scale,
            quantity_scale,
            bars,
        );
        let projected = self.projected_replacement_bars(
            current,
            bars.len(),
            retained_tail.is_some() && !tail_overlaps_last,
        )?;
        let publication_generation = next_publication_generation(current)?;
        let covering_bars = if tail_overlaps_last {
            &bars[..bars.len() - 1]
        } else {
            bars
        };
        if matches!(mode, InstallMode::Repair)
            && let Some(tail) = retained_tail.as_mut()
        {
            // Backwards repair renumbers the completed covering image from one.
            // The retained forming tail must move with that local sequence base;
            // otherwise sufficiently deep backfill eventually creates a
            // duplicate/regressing sequence at the history/live seam.
            tail.bar.source_sequence = covering_bars.last().map_or(Ok(1), |bar| {
                bar.source_sequence
                    .checked_add(1)
                    .ok_or(EngineError::CapacityOverflow)
            })?;
        }
        let covering = Arc::new(SeriesSnapshot {
            series: series.clone(),
            provider_generation,
            publication_generation,
            price_scale,
            quantity_scale,
            forming,
            bars: covering_bars.to_vec().into(),
        });
        self.total_bars = projected;
        let stored = replacement_series(
            covering,
            retained_tail,
            provider_generation,
            publication_generation,
            price_scale,
            quantity_scale,
            realtime,
        )?;
        let snapshot = stored.snapshot();
        self.series.insert(series, stored);
        Ok(snapshot)
    }

    fn validate_current_install(
        current: Option<&StoredSeries>,
        provider_generation: ProviderGeneration,
        price_scale: u8,
        quantity_scale: u8,
        bars: &[MarketBar],
        mode: InstallMode,
    ) -> Result<Option<Arc<SeriesSnapshot>>, EngineError> {
        let Some(current) = current else {
            return Ok(None);
        };
        let (forming, realtime, repair) = match mode {
            InstallMode::History => (false, false, false),
            InstallMode::Repair | InstallMode::Window => (false, false, true),
            InstallMode::Realtime { forming } => (forming, true, false),
        };
        if provider_generation < current.covering.provider_generation {
            return Err(EngineError::StaleSeriesGeneration {
                current: current.covering.provider_generation,
                received: provider_generation,
            });
        }
        if provider_generation != current.covering.provider_generation {
            return Ok(None);
        }
        let current_snapshot = current.snapshot();
        if current_snapshot.bars.as_ref() == bars
            && current_snapshot.price_scale == price_scale
            && current_snapshot.quantity_scale == quantity_scale
            && current_snapshot.forming == forming
        {
            return Ok(Some(current_snapshot));
        }
        if realtime {
            validate_realtime_transition(&current_snapshot, price_scale, quantity_scale, bars)?;
        } else if !repair {
            validate_history_transition(&current_snapshot, bars)?;
        }
        Ok(None)
    }

    fn projected_replacement_bars(
        &self,
        current: Option<&StoredSeries>,
        replacement_len: usize,
        retains_tail: bool,
    ) -> Result<usize, EngineError> {
        let retained = current.map_or(0, StoredSeries::bar_count);
        let replacement = replacement_len
            .checked_add(usize::from(retains_tail))
            .ok_or(EngineError::CapacityOverflow)?;
        let projected = self
            .total_bars
            .checked_sub(retained)
            .and_then(|count| count.checked_add(replacement))
            .ok_or(EngineError::CapacityOverflow)?;
        if projected > self.maximum_bars.get() {
            return Err(EngineError::BarLimitExceeded {
                maximum: self.maximum_bars,
                requested: projected,
            });
        }
        Ok(projected)
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
            || {
                current
                    .completed_tail
                    .last()
                    .copied()
                    .or_else(|| current.covering.bars.last().copied())
            },
            |tail| Some(tail.bar),
        );
        let previous = previous.ok_or(EngineError::EmptySeries)?;
        let previous_publication_generation = current.latest_publication_generation();
        let operation = if bar.source_sequence == previous.source_sequence {
            if current.tail.is_none()
                || bar.exchange_timestamp_unix_nanos != previous.exchange_timestamp_unix_nanos
            {
                return Err(EngineError::ConflictingSeriesGeneration(
                    provider_generation,
                ));
            }
            SeriesTailOperation::Revise
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
            let projected = self
                .total_bars
                .checked_add(1)
                .ok_or(EngineError::CapacityOverflow)?;
            if projected > self.maximum_bars.get() {
                return Err(EngineError::BarLimitExceeded {
                    maximum: self.maximum_bars,
                    requested: projected,
                });
            }
            if let Some(tail) = current.tail.take() {
                current.completed_tail.push(tail.bar);
            }
            self.total_bars = projected;
            SeriesTailOperation::Append
        };
        let publication_generation = previous_publication_generation
            .checked_add(1)
            .ok_or(EngineError::CapacityOverflow)?;
        let tail = SeriesTail {
            provider_generation,
            publication_generation,
            price_scale,
            quantity_scale,
            forming,
            operation,
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

    pub(crate) fn retained(&self) -> Vec<(BarSeriesKey, usize)> {
        self.series
            .iter()
            .map(|(series, stored)| (series.clone(), stored.bar_count()))
            .collect()
    }

    pub(crate) fn len(&self) -> usize {
        self.series.len()
    }

    pub(crate) fn total_bars(&self) -> usize {
        self.total_bars
    }

    pub(crate) fn bar_count(&self, series: &BarSeriesKey) -> Option<usize> {
        self.series.get(series).map(StoredSeries::bar_count)
    }

    pub(crate) fn approximate_bytes(&self) -> usize {
        self.total_bars.saturating_mul(size_of::<MarketBar>())
    }

    #[cfg(test)]
    pub(crate) fn completed_bars(&self, series: &BarSeriesKey) -> Option<Arc<[MarketBar]>> {
        self.series.get(series).map(|stored| {
            if stored.completed_tail.is_empty() {
                return Arc::clone(&stored.covering.bars);
            }
            let mut bars = Vec::with_capacity(
                stored
                    .covering
                    .bars
                    .len()
                    .saturating_add(stored.completed_tail.len()),
            );
            bars.extend_from_slice(&stored.covering.bars);
            bars.extend_from_slice(&stored.completed_tail);
            bars.into()
        })
    }
}

impl StoredSeries {
    fn latest_publication_generation(&self) -> u64 {
        self.tail
            .map_or(self.covering.publication_generation, |tail| {
                tail.publication_generation
            })
    }

    fn bar_count(&self) -> usize {
        self.covering
            .bars
            .len()
            .saturating_add(self.completed_tail.len())
            .saturating_add(usize::from(self.tail.is_some()))
    }

    fn snapshot(&self) -> Arc<SeriesSnapshot> {
        if self.completed_tail.is_empty() && self.tail.is_none() {
            return Arc::clone(&self.covering);
        }
        let mut bars = Vec::with_capacity(self.bar_count());
        bars.extend_from_slice(&self.covering.bars);
        bars.extend_from_slice(&self.completed_tail);
        if let Some(tail) = self.tail {
            bars.push(tail.bar);
        }
        let (provider_generation, publication_generation, price_scale, quantity_scale, forming) =
            self.tail.map_or(
                (
                    self.covering.provider_generation,
                    self.covering.publication_generation,
                    self.covering.price_scale,
                    self.covering.quantity_scale,
                    false,
                ),
                |tail| {
                    (
                        tail.provider_generation,
                        tail.publication_generation,
                        tail.price_scale,
                        tail.quantity_scale,
                        tail.forming,
                    )
                },
            );
        Arc::new(SeriesSnapshot {
            series: self.covering.series.clone(),
            provider_generation,
            publication_generation,
            price_scale,
            quantity_scale,
            forming,
            bars: bars.into(),
        })
    }
}

fn validate_install_inputs(
    series: &BarSeriesKey,
    price_scale: u8,
    quantity_scale: u8,
    bars: &[MarketBar],
) -> Result<(), EngineError> {
    series.validate()?;
    if price_scale > 18 || quantity_scale > 18 {
        return Err(EngineError::InvalidSeriesPrecision);
    }
    validate_bars(bars)
}

fn retained_tail_for_install(
    current: Option<&StoredSeries>,
    mode: InstallMode,
    generation: ProviderGeneration,
    price_scale: u8,
    quantity_scale: u8,
    bars: &[MarketBar],
) -> (Option<SeriesTail>, bool) {
    let retained_tail =
        retained_history_tail(current, mode, generation, price_scale, quantity_scale, bars);
    let overlaps_last = retained_tail.is_some_and(|tail| {
        bars.last().is_some_and(|bar| {
            bar.exchange_timestamp_unix_nanos == tail.bar.exchange_timestamp_unix_nanos
        })
    });
    (retained_tail, overlaps_last)
}

fn next_publication_generation(current: Option<&StoredSeries>) -> Result<u64, EngineError> {
    current.map_or(Ok(1), |stored| {
        stored
            .latest_publication_generation()
            .checked_add(1)
            .ok_or(EngineError::CapacityOverflow)
    })
}

fn replacement_series(
    covering: Arc<SeriesSnapshot>,
    retained_tail: Option<SeriesTail>,
    provider_generation: ProviderGeneration,
    publication_generation: u64,
    price_scale: u8,
    quantity_scale: u8,
    realtime: bool,
) -> Result<StoredSeries, EngineError> {
    if let Some(tail) = retained_tail {
        return Ok(StoredSeries {
            covering,
            completed_tail: Vec::new(),
            tail: Some(SeriesTail {
                provider_generation,
                publication_generation,
                price_scale,
                quantity_scale,
                forming: tail.forming,
                operation: SeriesTailOperation::Revise,
                bar: tail.bar,
            }),
        });
    }
    stored_series(&covering, realtime)
}

fn stored_series(
    snapshot: &Arc<SeriesSnapshot>,
    realtime: bool,
) -> Result<StoredSeries, EngineError> {
    if !realtime || !snapshot.forming {
        return Ok(StoredSeries {
            covering: Arc::clone(snapshot),
            completed_tail: Vec::new(),
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
        completed_tail: Vec::new(),
        tail: Some(SeriesTail {
            provider_generation: snapshot.provider_generation,
            publication_generation: snapshot.publication_generation,
            price_scale: snapshot.price_scale,
            quantity_scale: snapshot.quantity_scale,
            forming: snapshot.forming,
            operation: SeriesTailOperation::Revise,
            bar: *tail,
        }),
    })
}

fn retained_history_tail(
    current: Option<&StoredSeries>,
    mode: InstallMode,
    generation: ProviderGeneration,
    price_scale: u8,
    quantity_scale: u8,
    bars: &[MarketBar],
) -> Option<SeriesTail> {
    if matches!(mode, InstallMode::Realtime { .. } | InstallMode::Window) {
        return None;
    }
    // Completed history cannot erase a newer forming candle. Preserve its
    // sequence fence until history reaches it, within the same session/scale.
    current.and_then(|current| current.tail).filter(|tail| {
        tail.provider_generation == generation
            && tail.price_scale == price_scale
            && tail.quantity_scale == quantity_scale
            && bars.last().is_some_and(|bar| {
                bar.exchange_timestamp_unix_nanos < tail.bar.exchange_timestamp_unix_nanos
                    || matches!(mode, InstallMode::Repair)
                        && bar.exchange_timestamp_unix_nanos
                            == tail.bar.exchange_timestamp_unix_nanos
            })
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
