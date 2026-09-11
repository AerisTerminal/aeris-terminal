//! Indicators.

use super::*;

impl NucleusChartView {
    /// Adds an indicator with the defaults shown by the legacy native catalog.
    ///
    /// # Errors
    ///
    /// Returns an error when no market snapshot has populated the primary series or Nucleus
    /// cannot create every output required by the selected indicator.
    pub fn add_indicator(
        &mut self,
        indicator: ChartIndicator,
    ) -> Result<Vec<u32>, ChartIndicatorError> {
        if !self.has_market_data() {
            return Err(ChartIndicatorError::MarketDataUnavailable);
        }
        let ids = match indicator {
            ChartIndicator::Volume => {
                self.volume_legend = LegendPresence::Present;
                self.engine.set_series_visible(self.volume_series, true);
                vec![self.volume_series]
            }
            ChartIndicator::Vwap => self
                .engine
                .add_vwap(0, Some(self.volume_series))
                .into_iter()
                .collect(),
            ChartIndicator::Sma => self.engine.add_sma(0, 20).into_iter().collect(),
            ChartIndicator::Ema => self.engine.add_ema(0, 20).into_iter().collect(),
            ChartIndicator::EmaRibbon => self.engine.add_ema_ribbon(0, EMA_RIBBON_DEFAULT_PERIODS),
            ChartIndicator::Wma => self.engine.add_wma(0, 20).into_iter().collect(),
            ChartIndicator::Bollinger => self.engine.add_bollinger(0, 20, 2.0),
            ChartIndicator::Rsi => self.engine.add_rsi(0, 14).into_iter().collect(),
            ChartIndicator::Macd => self.engine.add_macd(0, 12, 26, 9),
            ChartIndicator::Stochastic => self.engine.add_stochastic(0, 14, 3),
            ChartIndicator::Atr => self.engine.add_atr(0, 14).into_iter().collect(),
        };
        let expected_outputs = match indicator {
            ChartIndicator::EmaRibbon => 5,
            ChartIndicator::Bollinger | ChartIndicator::Macd => 3,
            ChartIndicator::Stochastic => 2,
            ChartIndicator::Volume
            | ChartIndicator::Vwap
            | ChartIndicator::Sma
            | ChartIndicator::Ema
            | ChartIndicator::Wma
            | ChartIndicator::Rsi
            | ChartIndicator::Atr => 1,
        };
        if ids.len() != expected_outputs {
            for &id in &ids {
                self.engine.remove_series(id);
            }
            return Err(ChartIndicatorError::CreationRejected(indicator));
        }
        self.apply_indicator_chrome_options();
        self.invalidate_series_layout();
        self.mark_user_state_changed();
        Ok(ids)
    }
    /// Returns active indicator instances without engine-local series identities.
    #[must_use]
    pub fn indicator_states(&self) -> Vec<ChartIndicatorState> {
        let entries = self.engine.series_entries();
        let mut bindings = HashSet::new();
        let mut states = Vec::new();
        for &id in self.engine.series_order() {
            if id == self.volume_series {
                if self.volume_legend.is_present() {
                    let visible = entries
                        .iter()
                        .find(|series| series.id == id && !series.removed)
                        .is_some_and(|series| series.visible);
                    states.push(ChartIndicatorState {
                        indicator: ChartIndicator::Volume,
                        visible,
                    });
                }
                continue;
            }
            let Some(info) = self.engine.indicator_info(id) else {
                continue;
            };
            if !bindings.insert(info.binding_id) {
                continue;
            }
            let Some(indicator) = ChartIndicator::from_nucleus_kind(info.kind) else {
                continue;
            };
            let visible = entries.iter().any(|series| {
                !series.removed
                    && series.visible
                    && self
                        .engine
                        .indicator_info(series.id)
                        .is_some_and(|output| output.binding_id == info.binding_id)
            });
            states.push(ChartIndicatorState { indicator, visible });
        }
        states
    }
    /// Recreates indicator instances captured from an earlier engine for this chart surface.
    ///
    /// # Errors
    ///
    /// Returns the first native indicator creation error.
    pub fn restore_indicator_states(
        &mut self,
        states: &[ChartIndicatorState],
    ) -> Result<(), ChartIndicatorError> {
        for state in states {
            let ids = self.add_indicator(state.indicator)?;
            if state.visible {
                continue;
            }
            let item = if state.indicator == ChartIndicator::Volume {
                LegendItem::Volume
            } else {
                let Some(binding_id) = ids
                    .first()
                    .and_then(|id| self.engine.indicator_info(*id))
                    .map(|info| info.binding_id)
                else {
                    return Err(ChartIndicatorError::CreationRejected(state.indicator));
                };
                LegendItem::Indicator(binding_id)
            };
            let _ = self.set_legend_item_visible(item, false);
        }
        Ok(())
    }
    /// Returns whether any native indicator or the reusable volume series is currently shown.
    #[must_use]
    pub fn has_indicators(&self) -> bool {
        let study_series = self
            .study_series
            .values()
            .map(|state| state.series_id)
            .collect::<HashSet<_>>();
        self.engine.series_entries().iter().any(|series| {
            !series.removed
                && series.id != 0
                && !study_series.contains(&series.id)
                && (series.id != self.volume_series || self.volume_legend.is_present())
        })
    }
    /// Removes every native indicator and hides the reusable volume series.
    ///
    /// The product-owned price series is left in place. Volume stays allocated so the catalog can
    /// show it again without rebuilding live weights.
    pub fn clear_indicators(&mut self) -> bool {
        if !self.has_indicators() {
            return false;
        }
        if self
            .engine
            .selected_series()
            .is_some_and(|series| series != 0)
        {
            self.engine.set_selected_series(None);
        }
        let study_series = self
            .study_series
            .values()
            .map(|state| state.series_id)
            .collect::<HashSet<_>>();
        let ids: Vec<u32> = self
            .engine
            .series_entries()
            .iter()
            .filter(|series| {
                !series.removed && series.id != 0 && !study_series.contains(&series.id)
            })
            .map(|series| series.id)
            .collect();
        for id in ids {
            if id == self.volume_series {
                self.engine.set_series_visible(id, false);
                self.volume_legend = LegendPresence::Absent;
            } else {
                let _ = self.engine.remove_series(id);
            }
        }
        self.invalidate_series_layout();
        self.mark_user_state_changed();
        true
    }
    /// Builds the price-series row, which carries the OHLC readout.
    pub(super) fn asset_legend_row(
        &self,
        entries: &[nucleuscharts_engine::SeriesEntry],
        snapshots: &[nucleuscharts_engine::SeriesValueSnapshot],
    ) -> Option<LegendRow> {
        let asset = entries
            .iter()
            .find(|series| series.id == 0 && !series.removed)?;
        let snapshot = snapshots.iter().find(|snapshot| snapshot.series_id == 0);
        let (values, values_tone) = if asset.visible && self.chart_type.shows_ohlc_legend() {
            (
                snapshot
                    .map(|snapshot| {
                        let value = |label: &str, value: &Option<String>| {
                            format!("{label} {}", value.as_deref().unwrap_or("--"))
                        };
                        vec![
                            value("O", &snapshot.formatted_open),
                            value("H", &snapshot.formatted_high),
                            value("L", &snapshot.formatted_low),
                            value("C", &snapshot.formatted_close),
                        ]
                        .into_iter()
                        .map(|text| LegendValue { text, color: None })
                        .collect()
                    })
                    .unwrap_or_default(),
                snapshot.map_or(LegendValueTone::Neutral, asset_legend_value_tone),
            )
        } else {
            (Vec::new(), LegendValueTone::Neutral)
        };
        Some(LegendRow {
            item: LegendItem::Asset,
            pane: asset.pane_index,
            title: if !self.asset_legend_title.is_empty() {
                self.asset_legend_title.clone()
            } else if asset.title.is_empty() {
                "Asset".to_string()
            } else {
                asset.title.clone()
            },
            values,
            values_tone,
            visible: asset.visible,
        })
    }
    pub(super) fn legend_rows(&self) -> Vec<LegendRow> {
        let logical_index = self
            .engine
            .crosshair
            .map(|(x, _)| self.engine.time_scale.coordinate_to_index(x));
        let snapshots = self.engine.value_snapshot(logical_index);
        let entries = self.engine.series_entries();
        let mut rows = Vec::new();
        rows.extend(self.asset_legend_row(entries, &snapshots));
        if self.volume_legend.is_present()
            && let Some(volume) = entries
                .iter()
                .find(|series| series.id == self.volume_series && !series.removed)
        {
            rows.push(LegendRow {
                item: LegendItem::Volume,
                pane: volume.pane_index,
                title: "Volume".to_string(),
                values: if volume.visible {
                    legend_series_values(&snapshots, self.volume_series)
                } else {
                    Vec::new()
                },
                values_tone: snapshots
                    .iter()
                    .find(|snapshot| snapshot.series_id == 0)
                    .map_or(LegendValueTone::Neutral, asset_legend_value_tone),
                visible: volume.visible,
            });
        }

        let mut bindings = HashSet::new();
        for &id in self.engine.series_order() {
            let Some(info) = self.engine.indicator_info(id) else {
                continue;
            };
            if !bindings.insert(info.binding_id) {
                continue;
            }
            let outputs: Vec<_> = entries
                .iter()
                .filter(|series| {
                    !series.removed
                        && self
                            .engine
                            .indicator_info(series.id)
                            .is_some_and(|output| output.binding_id == info.binding_id)
                })
                .collect();
            let Some(first) = outputs.first() else {
                continue;
            };
            let visible = outputs.iter().any(|series| series.visible);
            let values = if visible {
                outputs
                    .iter()
                    .filter(|series| series.visible)
                    .filter_map(|series| {
                        let output = self.engine.indicator_info(series.id)?;
                        let value = legend_series_value(&snapshots, series.id);
                        if value.is_empty() {
                            None
                        } else {
                            Some(LegendValue {
                                text: if output.output_count > 1 && output.kind != "ema_ribbon" {
                                    format!("{} {value}", output.output_name)
                                } else {
                                    value
                                },
                                color: Some(series.line_color.clone().unwrap_or_else(|| {
                                    nucleuscharts_engine::DEFAULT_LINE_COLOR.to_css()
                                })),
                            })
                        }
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            rows.push(LegendRow {
                item: LegendItem::Indicator(info.binding_id),
                pane: first.pane_index,
                title: if info.kind == "ema_ribbon" {
                    "EMA Ribbon".to_string()
                } else {
                    first.title.clone()
                },
                values,
                values_tone: LegendValueTone::Neutral,
                visible,
            });
        }
        self.append_study_legend_rows(entries, &snapshots, &mut rows);
        rows
    }
    pub(super) fn set_legend_item_visible(&mut self, item: LegendItem, visible: bool) -> bool {
        if let LegendItem::Study { study_id, .. } = item {
            return self.set_study_visible(study_id, visible);
        }
        let ids: Vec<u32> = match item {
            LegendItem::Asset => vec![0],
            LegendItem::Volume if self.volume_legend.is_present() => vec![self.volume_series],
            LegendItem::Volume => return false,
            LegendItem::Indicator(binding) => self
                .engine
                .series_order()
                .iter()
                .copied()
                .filter(|&id| {
                    self.engine
                        .indicator_info(id)
                        .is_some_and(|info| info.binding_id == binding)
                })
                .collect(),
            LegendItem::Study { .. } => unreachable!("study visibility handled above"),
        };
        if ids.is_empty() {
            return false;
        }
        let changed = ids.iter().any(|&id| {
            self.engine
                .series_entries()
                .iter()
                .any(|series| series.id == id && !series.removed && series.visible != visible)
        });
        for id in ids {
            self.engine.set_series_visible(id, visible);
        }
        if changed {
            self.invalidate_series_layout();
            self.mark_user_state_changed();
        }
        changed
    }
    pub(super) fn remove_legend_indicator(&mut self, item: LegendItem) -> bool {
        let removed = match item {
            LegendItem::Volume if self.volume_legend.is_present() => {
                self.volume_legend = LegendPresence::Absent;
                self.engine.set_series_visible(self.volume_series, false);
                true
            }
            LegendItem::Asset | LegendItem::Volume | LegendItem::Study { .. } => false,
            LegendItem::Indicator(binding) => self.engine.remove_series(binding),
        };
        if removed {
            self.invalidate_series_layout();
            self.mark_user_state_changed();
        }
        removed
    }
    pub(super) fn indicator_series_ids(&self) -> Vec<u32> {
        let study_series = self
            .study_series
            .values()
            .map(|state| state.series_id)
            .collect::<HashSet<_>>();
        self.engine
            .series_entries()
            .iter()
            .filter(|series| {
                !series.removed
                    && series.id != 0
                    && !study_series.contains(&series.id)
                    && (series.id != self.volume_series || series.visible)
            })
            .map(|series| series.id)
            .collect()
    }
    pub(super) fn apply_indicator_chrome_options(&mut self) {
        let names = self.indicator_name_labels.visible();
        let values = self.indicator_value_labels.visible();
        let price_lines = self.indicator_price_lines.visible();
        let json = format!(
            r#"{{"last_value_visible":{values},"title_visible":{names},"price_line_visible":{price_lines}}}"#
        );
        for id in self.indicator_series_ids() {
            let _ = self.engine.series_apply_options_json(id, &json);
        }
    }
    /// Host-owned indicator name-chip chrome for every native indicator on this chart.
    #[must_use]
    pub const fn indicator_name_labels_visible(&self) -> bool {
        self.indicator_name_labels.visible()
    }
    /// Host-owned indicator last-value chrome for every native indicator on this chart.
    #[must_use]
    pub const fn indicator_value_labels_visible(&self) -> bool {
        self.indicator_value_labels.visible()
    }
    /// Host-owned last-value line chrome for every native indicator plot on this chart.
    #[must_use]
    pub const fn indicator_price_lines_visible(&self) -> bool {
        self.indicator_price_lines.visible()
    }
    /// Applies indicator name, value, and price-line preferences to current and later indicators.
    pub fn apply_indicator_chrome_preferences(
        &mut self,
        names: bool,
        values: bool,
        price_lines: bool,
    ) {
        let changed = self.indicator_name_labels.visible() != names
            || self.indicator_value_labels.visible() != values
            || self.indicator_price_lines.visible() != price_lines;
        self.indicator_name_labels = IndicatorLabels::from_visible(names);
        self.indicator_value_labels = IndicatorLabels::from_visible(values);
        self.indicator_price_lines = IndicatorLabels::from_visible(price_lines);
        self.apply_indicator_chrome_options();
        self.invalidate_series_layout();
        if changed {
            self.mark_user_state_changed();
        }
    }
    pub(super) fn toggle_indicator_name_labels(&mut self) -> bool {
        self.apply_indicator_chrome_preferences(
            !self.indicator_name_labels.visible(),
            self.indicator_value_labels.visible(),
            self.indicator_price_lines.visible(),
        );
        true
    }
    pub(super) fn toggle_indicator_value_labels(&mut self) -> bool {
        self.apply_indicator_chrome_preferences(
            self.indicator_name_labels.visible(),
            !self.indicator_value_labels.visible(),
            self.indicator_price_lines.visible(),
        );
        true
    }
    pub(super) fn toggle_indicator_price_lines(&mut self) -> bool {
        self.apply_indicator_chrome_preferences(
            self.indicator_name_labels.visible(),
            self.indicator_value_labels.visible(),
            !self.indicator_price_lines.visible(),
        );
        true
    }
    pub(super) fn sync_legend_pane_layout(&mut self) -> bool {
        let left = self.engine.pane_left.to_f32().unwrap_or_default();
        let width = self.engine.pane_w.to_f32().unwrap_or_default();
        let panes = self
            .engine
            .panes
            .iter()
            .map(|pane| LegendPaneLayout {
                left,
                top: pane.top.to_f32().unwrap_or_default(),
                width,
                height: pane.height.to_f32().unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        if panes == self.legend_panes {
            return false;
        }
        self.legend_panes = panes;
        true
    }
}
