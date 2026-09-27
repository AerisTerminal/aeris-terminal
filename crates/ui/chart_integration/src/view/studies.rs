//! Runtime-owned study output projection into Aeris Charts chart series.

use super::*;

const NANOS_PER_SECOND: i64 = 1_000_000_000;
const STUDY_PANE_STRETCH: f64 = 0.3;
// Mirrors the pinned Aeris Charts scalar validation ceiling. The explicit host-side
// check keeps an invalid live study publication from partially replacing the
// last valid chart image before Aeris Charts reports dropped rows.
const NUCLEUS_MAX_SAFE_SCALAR: f64 = 9_007_199_254_740_991.0 / 100.0;

struct PreparedStudyColumns {
    times: Vec<f64>,
    scalar: Vec<f64>,
}

impl AerisChartView {
    /// Returns the runtime-owned stream requirements retained for one output.
    #[must_use]
    pub fn study_output_input_requirements(
        &self,
        study_id: u64,
        output_index: usize,
    ) -> Option<ChartStudyInputRequirements> {
        self.study_series
            .get(&(study_id, output_index))
            .map(|state| state.input_requirements)
    }

    /// Returns whether every currently installed output for one runtime study is
    /// visible. `None` means the runtime study has not published an output yet.
    #[must_use]
    pub fn study_visible(&self, study_id: u64) -> Option<bool> {
        let mut found = false;
        let mut visible = true;
        for ((candidate, _), state) in &self.study_series {
            if *candidate != study_id {
                continue;
            }
            let Some(series) = self
                .engine
                .series_entries()
                .iter()
                .find(|series| series.id == state.series_id && !series.removed)
            else {
                continue;
            };
            found = true;
            visible &= series.visible;
        }
        found.then_some(visible)
    }

    /// Applies one study-level visibility choice to every currently installed
    /// output belonging to that runtime study.
    pub fn set_study_visible(&mut self, study_id: u64, visible: bool) -> bool {
        let series_ids = self
            .study_series
            .iter()
            .filter_map(|((candidate, _), state)| {
                (*candidate == study_id).then_some(state.series_id)
            })
            .collect::<Vec<_>>();
        let changed = series_ids.iter().any(|series_id| {
            self.engine.series_entries().iter().any(|series| {
                series.id == *series_id && !series.removed && series.visible != visible
            })
        });
        for series_id in series_ids {
            if self.engine.series_entries().iter().any(|series| {
                series.id == series_id && !series.removed && series.visible != visible
            }) {
                self.engine.set_series_visible(series_id, visible);
            }
        }
        if changed {
            self.invalidate_series_layout();
            self.mark_user_state_changed();
        }
        changed
    }

    pub(super) fn append_study_legend_rows(
        &self,
        entries: &[aeris_charts_engine::SeriesEntry],
        snapshots: &[aeris_charts_engine::SeriesValueSnapshot],
        rows: &mut Vec<LegendRow>,
    ) {
        let mut emitted = HashSet::new();
        for (study_id, _) in self.study_series.keys() {
            if !emitted.insert(*study_id) {
                continue;
            }
            let outputs = self
                .study_series
                .iter()
                .filter_map(|((candidate, _), state)| {
                    if *candidate != *study_id {
                        return None;
                    }
                    entries
                        .iter()
                        .find(|series| series.id == state.series_id && !series.removed)
                        .map(|series| (state, series))
                })
                .collect::<Vec<_>>();
            let Some((first_state, first_series)) = outputs.first().copied() else {
                continue;
            };
            let visible = outputs.iter().any(|(_, series)| series.visible);
            let values = if visible {
                outputs
                    .iter()
                    .filter(|(_, series)| series.visible)
                    .filter_map(|(state, series)| {
                        let value = legend_series_value(snapshots, state.series_id);
                        if value.is_empty() {
                            return None;
                        }
                        Some(LegendValue {
                            text: state
                                .legend_label
                                .as_ref()
                                .map_or(value.clone(), |label| format!("{label} {value}")),
                            color: Some(series.line_color.clone().unwrap_or_else(|| {
                                aeris_charts_engine::DEFAULT_LINE_COLOR.to_css()
                            })),
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            };
            rows.push(LegendRow {
                item: LegendItem::Study {
                    study_id: *study_id,
                    series_id: first_state.series_id,
                },
                pane: first_series.pane_index,
                title: first_series.title.clone(),
                values,
                values_tone: LegendValueTone::Neutral,
                visible,
                settings_available: outputs.iter().any(|(state, _)| state.settings_available),
            });
        }
    }

    /// Installs the newest immutable scalar output for one runtime-owned study.
    ///
    /// `None` values remain explicit whitespace rows, so warm-up gaps preserve
    /// the primary study timeline. Sub-second timestamps are rejected instead of
    /// silently truncated because the pinned Aeris Charts scalar-series contract is
    /// whole UTC seconds.
    ///
    /// Returns `Ok(false)` for a duplicate/stale output generation.
    ///
    /// # Errors
    /// Returns a typed validation error before mutating an existing study series,
    /// or [`ChartStudyOutputError::InstallationRejected`] when Aeris Charts rejects
    /// otherwise valid columns.
    pub fn install_study_output(
        &mut self,
        study_id: u64,
        output_index: usize,
        descriptor: ChartStudyOutputDescriptor<'_>,
        generation: u64,
        timestamps_unix_nanos: &[i64],
        values: &[Option<f64>],
    ) -> Result<bool, ChartStudyOutputError> {
        let key = (study_id, output_index);
        if self
            .study_series
            .get(&key)
            .is_some_and(|state| state.generation >= generation)
        {
            return Ok(false);
        }
        let columns = prepare_study_columns(timestamps_unix_nanos, values)?;
        validate_study_presentation(descriptor)?;
        let existing = self.study_series.get(&key).cloned();
        let created = existing.is_none();
        let series_id = match existing {
            Some(state) => {
                set_study_series_data(&mut self.engine, state.series_id, &columns)?;
                state.series_id
            }
            None => self.install_new_study_series(study_id, descriptor, &columns)?,
        };
        apply_study_series_presentation(&mut self.engine, series_id, output_index, descriptor);
        self.study_series.insert(
            key,
            ChartStudySeriesState {
                series_id,
                generation,
                settings_available: descriptor.settings_available,
                legend_label: descriptor.legend_label.map(str::to_string),
                input_requirements: descriptor.input_requirements,
            },
        );
        if created {
            self.apply_indicator_chrome_to_series(series_id);
            self.invalidate_series_layout();
        } else {
            self.invalidate_series_frame();
        }
        Ok(true)
    }

    fn install_new_study_series(
        &mut self,
        study_id: u64,
        descriptor: ChartStudyOutputDescriptor<'_>,
        columns: &PreparedStudyColumns,
    ) -> Result<u32, ChartStudyOutputError> {
        let source_price_format = self
            .engine
            .series_entries()
            .iter()
            .find(|series| series.id == 0 && !series.removed)
            .map(|series| {
                (
                    series.price_format.kind,
                    series.price_format.precision,
                    series.price_format.min_move,
                )
            });
        let series_id = self.engine.add_series(study_series_kind(descriptor.plot));
        if let Err(error) = set_study_series_data(&mut self.engine, series_id, columns) {
            let _ = self.engine.remove_series(series_id);
            return Err(error);
        }
        if let Some(series) = self
            .engine
            .series
            .iter_mut()
            .find(|series| series.id == series_id && !series.removed)
        {
            series.title = descriptor.title.to_string();
            series.title_visible = true;
            series.line_width = Some(2.0);
            if let Some((kind, precision, min_move)) = source_price_format {
                series.price_format.kind = kind;
                series.price_format.precision = precision;
                series.price_format.min_move = min_move;
            }
        }
        let mut created_pane = None;
        let pane_index = match descriptor.pane {
            ChartStudyPaneTarget::Price => 0,
            ChartStudyPaneTarget::Dedicated { group } => {
                let pane_key = (study_id, group);
                if let Some(index) = self
                    .study_panes
                    .get(&pane_key)
                    .and_then(|pane_id| self.engine.pane_index_for_id(*pane_id))
                {
                    index
                } else {
                    self.study_panes.remove(&pane_key);
                    let Some(index) = self.engine.add_pane(false) else {
                        let _ = self.engine.remove_series(series_id);
                        return Err(ChartStudyOutputError::InstallationRejected);
                    };
                    let Some(pane_id) = self.engine.pane_stable_id(index) else {
                        let _ = self.engine.remove_series(series_id);
                        let _ = self.engine.remove_pane(index);
                        return Err(ChartStudyOutputError::InstallationRejected);
                    };
                    created_pane = Some((pane_key, pane_id, index));
                    index
                }
            }
        };
        if !self.engine.try_set_series_pane_and_scale(
            series_id,
            pane_index,
            STUDY_PANE_STRETCH,
            study_scale_id(descriptor.scale),
        ) {
            let _ = self.engine.remove_series(series_id);
            if let Some((_, _, created_index)) = created_pane {
                let _ = self.engine.remove_pane(created_index);
            }
            return Err(ChartStudyOutputError::InstallationRejected);
        }
        if let Some((pane_key, pane_id, _)) = created_pane {
            self.study_panes.insert(pane_key, pane_id);
        }
        Ok(series_id)
    }

    /// Removes every chart-local output belonging to the supplied runtime study
    /// identities. Runtime dependency state remains outside the chart.
    pub fn remove_study_outputs(&mut self, study_ids: &[u64]) -> bool {
        if study_ids.is_empty() {
            return false;
        }
        let keys = self
            .study_series
            .keys()
            .copied()
            .filter(|(study_id, _)| study_ids.contains(study_id))
            .collect::<Vec<_>>();
        if keys.is_empty() {
            return false;
        }
        for key in keys {
            let Some(state) = self.study_series.remove(&key) else {
                continue;
            };
            if self.engine.selected_series() == Some(state.series_id) {
                self.engine.set_selected_series(None);
            }
            let _ = self.engine.remove_series(state.series_id);
        }
        self.study_panes
            .retain(|(study_id, _), _| !study_ids.contains(study_id));
        self.invalidate_series_layout();
        true
    }
}

fn validate_study_presentation(
    descriptor: ChartStudyOutputDescriptor<'_>,
) -> Result<(), ChartStudyOutputError> {
    if descriptor.threshold_region.is_some_and(|region| {
        !region.lower.is_finite()
            || !region.upper.is_finite()
            || region.lower >= region.upper
            || !matches!(
                descriptor.plot,
                ChartStudyPlotKind::Line | ChartStudyPlotKind::Area
            )
    }) || (descriptor.point_style == ChartStudyPointStyle::MomentumHistogram
        && descriptor.plot != ChartStudyPlotKind::Histogram)
    {
        return Err(ChartStudyOutputError::InvalidPresentation);
    }
    Ok(())
}

fn apply_study_series_presentation(
    engine: &mut aeris_charts_engine::ChartEngine,
    series_id: u32,
    output_index: usize,
    descriptor: ChartStudyOutputDescriptor<'_>,
) {
    if let Some(series) = engine
        .series
        .iter_mut()
        .find(|series| series.id == series_id && !series.removed)
    {
        series.line_color = Some(
            EMA_RIBBON_DEFAULT_COLORS[output_index % EMA_RIBBON_DEFAULT_COLORS.len()].to_string(),
        );
    }
    let threshold =
        descriptor
            .threshold_region
            .map(|region| aeris_charts_engine::SeriesThresholdRegion {
                lower: region.lower,
                upper: region.upper,
            });
    let _ = engine.set_series_threshold_region(series_id, threshold);
    if descriptor.point_style == ChartStudyPointStyle::MomentumHistogram {
        let _ = engine.apply_momentum_histogram_colors(series_id);
    }
}

fn prepare_study_columns(
    timestamps_unix_nanos: &[i64],
    values: &[Option<f64>],
) -> Result<PreparedStudyColumns, ChartStudyOutputError> {
    if timestamps_unix_nanos.len() != values.len() {
        return Err(ChartStudyOutputError::LengthMismatch);
    }
    let mut times = Vec::with_capacity(timestamps_unix_nanos.len());
    let mut scalar = Vec::with_capacity(values.len());
    let mut previous_second = None;
    for (&timestamp, value) in timestamps_unix_nanos.iter().zip(values) {
        if timestamp.rem_euclid(NANOS_PER_SECOND) != 0 {
            return Err(ChartStudyOutputError::UnsupportedTimestampPrecision);
        }
        let second = timestamp.div_euclid(NANOS_PER_SECOND);
        if previous_second.is_some_and(|previous| previous >= second) {
            return Err(ChartStudyOutputError::NonIncreasingTimestamp);
        }
        previous_second = Some(second);
        times.push(
            second
                .to_f64()
                .ok_or(ChartStudyOutputError::UnsupportedTimestampPrecision)?,
        );
        scalar.push(match value {
            None => f64::NAN,
            Some(value) if value.is_finite() && value.abs() <= NUCLEUS_MAX_SAFE_SCALAR => *value,
            Some(_) => return Err(ChartStudyOutputError::InvalidValue),
        });
    }
    Ok(PreparedStudyColumns { times, scalar })
}

fn set_study_series_data(
    engine: &mut aeris_charts_engine::ChartEngine,
    series_id: u32,
    columns: &PreparedStudyColumns,
) -> Result<(), ChartStudyOutputError> {
    engine
        .set_series_data(
            series_id,
            &columns.times,
            &columns.scalar,
            &columns.scalar,
            &columns.scalar,
            &columns.scalar,
        )
        .map(|_| ())
        .map_err(|_| ChartStudyOutputError::InstallationRejected)
}

const fn study_series_kind(plot: ChartStudyPlotKind) -> aeris_charts_engine::SeriesKind {
    match plot {
        ChartStudyPlotKind::Line => aeris_charts_engine::SeriesKind::Line,
        ChartStudyPlotKind::Histogram => aeris_charts_engine::SeriesKind::Histogram,
        ChartStudyPlotKind::Area => aeris_charts_engine::SeriesKind::Area,
    }
}

const fn study_scale_id(scale: ChartStudyScaleTarget) -> &'static str {
    match scale {
        ChartStudyScaleTarget::Primary => "right",
        ChartStudyScaleTarget::Left => "left",
        ChartStudyScaleTarget::Overlay => "",
    }
}
