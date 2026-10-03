//! Indicators.

use super::*;

const VOLUME_LEGEND_IDENTITY: u64 = 1;
const CVD_LEGEND_IDENTITY: u64 = 2;
const DELTA_LEGEND_IDENTITY: u64 = 3;

impl AerisChartView {
    /// Adds an indicator with the defaults shown by the legacy native catalog.
    ///
    /// # Errors
    ///
    /// Returns an error when no market snapshot has populated the primary series or Aeris Charts
    /// cannot create every output required by the selected indicator.
    pub fn add_indicator(
        &mut self,
        indicator: ChartIndicator,
    ) -> Result<Vec<u32>, ChartIndicatorError> {
        if !self.has_market_data() {
            return Err(ChartIndicatorError::MarketDataUnavailable);
        }
        let ids = if indicator == ChartIndicator::Volume {
            self.volume_legend = LegendPresence::Present;
            self.engine.set_series_visible(self.volume_series, true);
            vec![self.volume_series]
        } else if indicator == ChartIndicator::VolumeProfile {
            if self.volume_profile.is_some() {
                return Err(ChartIndicatorError::CreationRejected(indicator));
            }
            // Aeris Charts owns the profile's binning, value area and rendering; the host only
            // binds it to the product price series and the always-allocated volume weights.
            let Some(id) = self.engine.add_volume_profile_indicator(
                0,
                self.volume_series,
                aeris_charts_engine::VolumeProfileIndicatorOptions::default(),
            ) else {
                return Err(ChartIndicatorError::CreationRejected(indicator));
            };
            self.volume_profile = Some(id);
            // A profile owns price bins, not a time series, so it has no series output.
            self.invalidate_series_layout();
            self.mark_user_state_changed();
            return Ok(Vec::new());
        } else {
            let Some(kind) = indicator.engine_kind() else {
                return Err(ChartIndicatorError::CreationRejected(indicator));
            };
            self.engine.add_indicator_kind(
                0,
                kind,
                (indicator == ChartIndicator::Vwap).then_some(self.volume_series),
            )
        };
        if ids.is_empty() {
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
        let mut states = Vec::new();
        if self.volume_legend.is_present() {
            let visible = self.engine.series_visible(self.volume_series) == Some(true);
            states.push(ChartIndicatorState {
                indicator: ChartIndicator::Volume,
                visible,
            });
        }
        if let Some(visible) = self.volume_profile_visible() {
            states.push(ChartIndicatorState {
                indicator: ChartIndicator::VolumeProfile,
                visible,
            });
        }
        for binding in self.engine.indicator_bindings() {
            let Some(indicator) = ChartIndicator::from_engine_kind(&binding.kind) else {
                continue;
            };
            let visible = binding.styles.iter().any(|style| style.visible);
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
            } else if state.indicator == ChartIndicator::VolumeProfile {
                LegendItem::VolumeProfile
            } else {
                let Some(binding_id) = ids.first().copied() else {
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
        self.volume_legend.is_present()
            || self.volume_profile.is_some()
            || self.engine.has_indicator_bindings()
            || self.engine.has_external_studies()
            || [OrderFlowStudy::CumulativeDelta, OrderFlowStudy::Delta]
                .into_iter()
                .any(|study| self.has_order_flow_study(study))
    }
    /// Removes every native indicator and hides the reusable volume series.
    ///
    /// The product-owned price series is left in place. Volume stays allocated so the catalog can
    /// show it again without rebuilding live weights.
    pub fn clear_indicators(&mut self) -> bool {
        let mut cleared = self.engine.clear_indicator_bindings();
        if self.volume_legend.is_present() {
            self.engine.set_series_visible(self.volume_series, false);
            self.volume_legend = LegendPresence::Absent;
            cleared = true;
        }
        cleared |= self.remove_volume_profile();
        for study in [OrderFlowStudy::CumulativeDelta, OrderFlowStudy::Delta] {
            if self.has_order_flow_study(study) {
                cleared |= self.remove_order_flow_study(study);
            }
        }
        if !cleared {
            return false;
        }
        self.invalidate_series_layout();
        self.mark_user_state_changed();
        true
    }
    pub(super) fn legend_rows(&self) -> Vec<LegendRow> {
        self.legend_rows_at(
            self.engine
                .crosshair
                .map(|(x, _)| self.engine.time_scale.coordinate_to_index(x)),
        )
    }

    /// Legend rows reading out `logical_index`, or the latest bar when `None`.
    pub(super) fn legend_rows_at(&self, logical_index: Option<i64>) -> Vec<LegendRow> {
        let leading = self
            .volume_legend
            .is_present()
            .then_some(HostLegendSeries {
                identity: VOLUME_LEGEND_IDENTITY,
                series_id: self.volume_series,
                title: "Volume",
                settings_available: false,
                tone_from_primary: true,
            })
            .into_iter()
            .collect::<Vec<_>>();
        let trailing = self
            .order_flow_state
            .iter()
            .flat_map(|state| {
                [
                    (OrderFlowStudy::CumulativeDelta, CVD_LEGEND_IDENTITY),
                    (OrderFlowStudy::Delta, DELTA_LEGEND_IDENTITY),
                ]
                .into_iter()
                .filter_map(|(study, identity)| {
                    state.study_series(study).map(|series_id| HostLegendSeries {
                        identity,
                        series_id,
                        title: study.title(),
                        settings_available: false,
                        tone_from_primary: false,
                    })
                })
            })
            .collect::<Vec<_>>();
        let mut rows = self
            .engine
            .financial_legend(FinancialLegendRequest {
                logical_index,
                primary_title: &self.asset_legend_title,
                show_primary_ohlc: self.chart_type.shows_ohlc_legend(),
                leading_series: &leading,
                trailing_series: &trailing,
            })
            .into_iter()
            .filter_map(|row| {
                let item = match row.identity {
                    FinancialLegendIdentity::Primary => LegendItem::Asset,
                    FinancialLegendIdentity::Host(VOLUME_LEGEND_IDENTITY) => LegendItem::Volume,
                    FinancialLegendIdentity::Host(CVD_LEGEND_IDENTITY) => {
                        LegendItem::OrderFlow(OrderFlowStudy::CumulativeDelta)
                    }
                    FinancialLegendIdentity::Host(DELTA_LEGEND_IDENTITY) => {
                        LegendItem::OrderFlow(OrderFlowStudy::Delta)
                    }
                    FinancialLegendIdentity::Host(_) => return None,
                    FinancialLegendIdentity::Indicator(binding) => LegendItem::Indicator(binding),
                    FinancialLegendIdentity::ExternalStudy(study_id) => LegendItem::Study {
                        study_id,
                        series_id: row.first_series_id,
                    },
                };
                Some(LegendRow {
                    item,
                    pane: row.pane,
                    title: row.title,
                    values: row
                        .values
                        .into_iter()
                        .map(|value| LegendValue {
                            text: value.text,
                            color: value.color,
                        })
                        .collect(),
                    values_tone: match row.tone {
                        FinancialLegendTone::Neutral => LegendValueTone::Neutral,
                        FinancialLegendTone::Bullish => LegendValueTone::Bullish,
                        FinancialLegendTone::Bearish => LegendValueTone::Bearish,
                    },
                    visible: row.visible,
                    settings_available: row.settings_available,
                })
            })
            .collect::<Vec<_>>();
        self.insert_volume_profile_legend_row(&mut rows);
        rows
    }
    /// The profile is an engine-owned price-pane primitive, not a series, so the financial
    /// legend has no row for it; the host adds one so it can be hidden or removed.
    fn insert_volume_profile_legend_row(&self, rows: &mut Vec<LegendRow>) {
        let Some(visible) = self.volume_profile_visible() else {
            return;
        };
        let position = rows
            .iter()
            .position(|row| row.pane != 0)
            .unwrap_or(rows.len());
        rows.insert(
            position,
            LegendRow {
                item: LegendItem::VolumeProfile,
                pane: 0,
                title: "Volume Profile".to_string(),
                values: Vec::new(),
                values_tone: LegendValueTone::Neutral,
                visible,
                settings_available: false,
            },
        );
    }
    fn volume_profile_visible(&self) -> Option<bool> {
        self.volume_profile
            .and_then(|id| self.engine.volume_profile_indicator_options(id))
            .map(|options| options.visible)
    }
    fn remove_volume_profile(&mut self) -> bool {
        let Some(id) = self.volume_profile.take() else {
            return false;
        };
        self.engine.remove_native_primitive(id)
    }
    pub(super) fn set_legend_item_visible(&mut self, item: LegendItem, visible: bool) -> bool {
        if let LegendItem::Study { study_id, .. } = item {
            return self.set_study_visible(study_id, visible);
        }
        if let LegendItem::OrderFlow(study) = item {
            let changed = self.set_order_flow_study_visible(study, visible);
            if changed {
                self.mark_user_state_changed();
            }
            return changed;
        }
        let ids: Vec<u32> = match item {
            // The footprint presents the same product price series, so they toggle together.
            LegendItem::Asset => std::iter::once(0)
                .chain(self.footprint_series_id())
                .collect(),
            LegendItem::Volume if self.volume_legend.is_present() => vec![self.volume_series],
            LegendItem::Volume => return false,
            LegendItem::VolumeProfile => {
                let Some(id) = self.volume_profile else {
                    return false;
                };
                let Some(mut options) = self.engine.volume_profile_indicator_options(id).cloned()
                else {
                    return false;
                };
                if options.visible == visible {
                    return false;
                }
                options.visible = visible;
                let changed = self
                    .engine
                    .set_volume_profile_indicator_options(id, options);
                if changed {
                    self.invalidate_series_frame();
                    self.mark_user_state_changed();
                }
                return changed;
            }
            LegendItem::Indicator(binding) => {
                let changed = self.engine.set_indicator_binding_visible(binding, visible);
                if changed {
                    self.invalidate_series_layout();
                    self.mark_user_state_changed();
                }
                return changed;
            }
            LegendItem::Study { .. } | LegendItem::OrderFlow(_) => {
                unreachable!("study and order-flow visibility handled above")
            }
        };
        if ids.is_empty() {
            return false;
        }
        let changed = ids.iter().any(|&id| {
            self.engine
                .series_visible(id)
                .is_some_and(|value| value != visible)
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
            LegendItem::OrderFlow(study) => return self.remove_order_flow_study(study),
            LegendItem::VolumeProfile => self.remove_volume_profile(),
            LegendItem::Asset | LegendItem::Volume | LegendItem::Study { .. } => false,
            LegendItem::Indicator(binding) => self.engine.remove_indicator_binding(binding),
        };
        if removed {
            self.invalidate_series_layout();
            self.mark_user_state_changed();
        }
        removed
    }
    fn apply_volume_chrome(&mut self) {
        if !self.volume_legend.is_present() {
            return;
        }
        let names = self.indicator_name_labels.visible();
        let values = self.indicator_value_labels.visible();
        let price_lines = self.indicator_price_lines.visible();
        let json = format!(
            r#"{{"last_value_visible":{values},"title_visible":{names},"price_line_visible":{price_lines}}}"#
        );
        let _ = self
            .engine
            .series_apply_options_json(self.volume_series, &json);
    }
    pub(super) fn apply_indicator_chrome_options(&mut self) {
        let options = IndicatorChromeOptions {
            name_labels_visible: self.indicator_name_labels.visible(),
            value_labels_visible: self.indicator_value_labels.visible(),
            price_lines_visible: self.indicator_price_lines.visible(),
        };
        let _ = self.engine.set_indicator_chrome_options(options);
        // Volume is a Terminal product series; native indicators, external studies, and order-flow
        // studies receive this policy from the engine-owned transaction above.
        self.apply_volume_chrome();
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
