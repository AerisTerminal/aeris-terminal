//! Indicators.

use super::*;

const CVD_LEGEND_IDENTITY: u64 = 2;
const DELTA_LEGEND_IDENTITY: u64 = 3;

impl AerisChartView {
    /// Adds an indicator with the defaults shown by the legacy native catalog. Volume already on
    /// the chart is shown again with its settings.
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
            let settings = self.volume_settings().unwrap_or_default();
            self.install_volume(settings, true)
                .ok_or(ChartIndicatorError::CreationRejected(indicator))?
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
            let reads_volume = match indicator {
                ChartIndicator::Vwap => true,
                ChartIndicator::Builtin(builtin) => builtin.reads_volume(),
                _ => false,
            };
            self.engine
                .add_indicator_kind(0, kind, reads_volume.then_some(self.volume_series))
        };
        if ids.is_empty() {
            return Err(ChartIndicatorError::CreationRejected(indicator));
        }
        self.invalidate_series_layout();
        self.mark_user_state_changed();
        Ok(ids)
    }
    /// Returns active indicator instances without engine-local series identities.
    #[must_use]
    pub fn indicator_states(&self) -> Vec<ChartIndicatorState> {
        let mut states = Vec::new();
        if let Some(volume) = &self.volume {
            states.push(ChartIndicatorState {
                indicator: ChartIndicator::Volume,
                visible: self.volume_visible(),
                volume: Some(volume.settings.clone()),
            });
        }
        if let Some(visible) = self.volume_profile_visible() {
            states.push(ChartIndicatorState {
                indicator: ChartIndicator::VolumeProfile,
                visible,
                volume: None,
            });
        }
        for binding in self.engine.indicator_bindings() {
            let Some(indicator) = ChartIndicator::from_engine_kind(&binding.kind) else {
                continue;
            };
            let visible = binding.styles.iter().any(|style| style.visible);
            states.push(ChartIndicatorState {
                indicator,
                visible,
                volume: None,
            });
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
            if state.indicator == ChartIndicator::Volume {
                self.restore_volume(state)?;
                continue;
            }
            let ids = self.add_indicator(state.indicator)?;
            if state.visible {
                continue;
            }
            let item = if state.indicator == ChartIndicator::VolumeProfile {
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
    /// Returns whether any indicator is on the chart.
    #[must_use]
    pub fn has_indicators(&self) -> bool {
        self.volume.is_some()
            || self.volume_profile.is_some()
            || self.engine.has_indicator_bindings()
            || self.engine.has_external_studies()
            || [OrderFlowStudy::CumulativeDelta, OrderFlowStudy::Delta]
                .into_iter()
                .any(|study| self.has_order_flow_study(study))
            || self.order_flow_settings.big_trades.is_some()
            || self.order_flow_settings.order_book_depth.is_some()
            || self.order_flow_settings.depth_heatmap.is_some()
    }
    /// Removes every indicator. The product-owned price and volume series stay in place.
    pub fn clear_indicators(&mut self) -> bool {
        let mut cleared = self.engine.clear_indicator_bindings();
        // Clearing the bindings above already removed the volume outputs.
        cleared |= self.volume.take().is_some();
        cleared |= self.remove_volume_profile();
        for study in [OrderFlowStudy::CumulativeDelta, OrderFlowStudy::Delta] {
            if self.has_order_flow_study(study) {
                cleared |= self.remove_order_flow_study(study);
            }
        }
        cleared |= self.remove_big_trades();
        cleared |= self.remove_order_book_depth();
        cleared |= self.remove_depth_heatmap();
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
                        settings_available: true,
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
                // The footprint carries the bar values over its whitespace price series.
                primary_values_series: self.footprint_series_id(),
                leading_series: &[],
                trailing_series: &trailing,
            })
            .into_iter()
            .filter_map(|row| legend_row(row, self.volume.as_ref().map(|volume| volume.columns)))
            .collect::<Vec<_>>();
        if self.chart_type == ChartType::Footprint
            && !self.has_footprint_bars()
            && let Some(asset) = rows.iter_mut().find(|row| row.item == LegendItem::Asset)
        {
            asset.values = vec![LegendValue {
                text: "Waiting for order flow".to_string(),
                color: None,
            }];
        }
        if let Some(visible) = self.volume_profile_visible() {
            let item = LegendItem::VolumeProfile;
            insert_price_pane_legend_row(&mut rows, item, "Volume Profile", visible, false);
        }
        // The row follows the durable settings rather than the drawn indicator, so a chart whose
        // tape is unavailable can still edit or remove it.
        if let Some(big_trades) = self.order_flow_settings.big_trades {
            let item = LegendItem::BigTrades;
            insert_price_pane_legend_row(&mut rows, item, "Big Trades", big_trades.visible, true);
        }
        if let Some(book) = self.order_flow_settings.order_book_depth {
            let item = LegendItem::OrderBookDepth;
            insert_price_pane_legend_row(&mut rows, item, "Order Book", book.visible, true);
        }
        if let Some(heatmap) = self.order_flow_settings.depth_heatmap {
            let item = LegendItem::DepthHeatmap;
            let title = "Depth Heatmap";
            insert_price_pane_legend_row(&mut rows, item, title, heatmap.visible, false);
        }
        apply_legend_options(&mut rows, self.legend_options);
        rows
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
        if item == LegendItem::BigTrades {
            return self.set_big_trades_visible(visible);
        }
        if item == LegendItem::OrderBookDepth {
            return self.set_order_book_depth_visible(visible);
        }
        if item == LegendItem::DepthHeatmap {
            return self.set_depth_heatmap_visible(visible);
        }
        let ids: Vec<u32> = match item {
            // The footprint presents the same product price series, so they toggle together.
            LegendItem::Asset => std::iter::once(0)
                .chain(self.footprint_series_id())
                .collect(),
            LegendItem::Volume => {
                let changed = self.volume.is_some() && self.volume_visible() != visible;
                if changed {
                    self.apply_volume_style(visible);
                    self.invalidate_series_layout();
                    self.mark_user_state_changed();
                }
                return changed;
            }
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
            LegendItem::Study { .. }
            | LegendItem::OrderFlow(_)
            | LegendItem::BigTrades
            | LegendItem::OrderBookDepth
            | LegendItem::DepthHeatmap => {
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
            LegendItem::Volume => self.remove_volume(),
            LegendItem::OrderFlow(study) => return self.remove_order_flow_study(study),
            LegendItem::BigTrades => return self.remove_big_trades(),
            LegendItem::OrderBookDepth => return self.remove_order_book_depth(),
            LegendItem::DepthHeatmap => return self.remove_depth_heatmap(),
            LegendItem::VolumeProfile => self.remove_volume_profile(),
            LegendItem::Asset | LegendItem::Study { .. } => false,
            LegendItem::Indicator(binding) => self.engine.remove_indicator_binding(binding),
        };
        if removed {
            self.invalidate_series_layout();
            self.mark_user_state_changed();
        }
        removed
    }
    /// The volume indicator's settings while it is on the chart.
    #[must_use]
    pub fn volume_settings(&self) -> Option<VolumeSettings> {
        self.volume.as_ref().map(|volume| volume.settings.clone())
    }

    /// Rebuilds the volume indicator from new settings, keeping whether it is shown. Returns
    /// whether the chart changed: `false` when volume is not on the chart, the settings are
    /// unchanged, or Aeris Charts rejects them.
    pub fn set_volume_settings(&mut self, settings: VolumeSettings) -> bool {
        let Some(volume) = &self.volume else {
            return false;
        };
        if volume.settings == settings {
            return false;
        }
        let visible = self.volume_visible();
        if self.install_volume(settings, visible).is_none() {
            return false;
        }
        self.invalidate_series_layout();
        self.mark_user_state_changed();
        true
    }

    fn restore_volume(&mut self, state: &ChartIndicatorState) -> Result<(), ChartIndicatorError> {
        if !self.has_market_data() {
            return Err(ChartIndicatorError::MarketDataUnavailable);
        }
        let settings = state.volume.clone().unwrap_or_default();
        if self.install_volume(settings, state.visible).is_none() {
            return Err(ChartIndicatorError::CreationRejected(
                ChartIndicator::Volume,
            ));
        }
        self.invalidate_series_layout();
        self.mark_user_state_changed();
        Ok(())
    }

    /// Replaces the volume indicator with one Aeris Charts builds from `settings` and returns its
    /// outputs. The engine computes, places and draws the columns and their average; the host
    /// supplies only the product volume weights and the durable settings.
    fn install_volume(&mut self, settings: VolumeSettings, visible: bool) -> Option<Vec<u32>> {
        if !VolumeSettings::AVERAGE_LENGTHS.contains(&settings.average.length) {
            return None;
        }
        if let Some(previous) = self.volume.take() {
            let _ = self.engine.remove_indicator_binding(previous.columns);
        }
        // The weights are both the source and the volume input, so the columns follow traded
        // volume even where a footprint leaves the price row as whitespace.
        let outputs = self.engine.add_indicator_kind(
            self.volume_series,
            IndicatorKind::Volume {
                period: usize::from(settings.average.length),
                exponential: settings.average.kind == VolumeAverageKind::Exponential,
            },
            Some(self.volume_series),
        );
        let [columns, average] = outputs[..] else {
            if let Some(&binding) = outputs.first() {
                let _ = self.engine.remove_indicator_binding(binding);
            }
            return None;
        };
        let pane = match settings.placement {
            VolumePlacement::PricePane => IndicatorPane::Source,
            VolumePlacement::OwnPane => IndicatorPane::Separate,
        };
        let _ = self.engine.set_indicator_binding_pane(columns, pane);
        self.volume = Some(VolumeIndicator {
            columns,
            average,
            settings,
        });
        self.apply_volume_style(visible);
        Some(outputs)
    }

    /// Applies the durable colors and visibility to the volume outputs; the average shows only
    /// while enabled.
    pub(super) fn apply_volume_style(&mut self, visible: bool) {
        let Some(VolumeIndicator {
            columns,
            average,
            settings,
        }) = self.volume.clone()
        else {
            return;
        };
        if let Some(mut style) = self.engine.indicator_info(columns).map(|info| info.style) {
            style.visible = visible;
            style.up_color = settings.up_color;
            style.down_color = settings.down_color;
            let _ = self.engine.set_indicator_output_style(columns, style);
        }
        if let Some(mut style) = self.engine.indicator_info(average).map(|info| info.style) {
            style.visible = visible && settings.average.enabled;
            style.line_color = settings.average.color;
            let _ = self.engine.set_indicator_output_style(average, style);
        }
    }

    pub(super) fn volume_visible(&self) -> bool {
        self.volume
            .as_ref()
            .is_some_and(|volume| self.engine.series_visible(volume.columns) == Some(true))
    }

    pub(super) fn is_volume_output(&self, series: u32) -> bool {
        self.volume
            .as_ref()
            .is_some_and(|volume| series == volume.columns || series == volume.average)
    }

    /// Records an indicator the chart removed itself, such as one deleted from the keyboard, so
    /// it is not restored on the next instrument or interval.
    pub(super) fn indicator_removed(&mut self, binding: u32) {
        if self
            .volume
            .as_ref()
            .is_some_and(|volume| volume.columns == binding)
        {
            self.volume = None;
        }
        self.invalidate_series_layout();
        self.mark_user_state_changed();
    }

    /// Removes the volume indicator; the product volume weights stay for the other readers.
    pub(super) fn remove_volume(&mut self) -> bool {
        self.volume
            .take()
            .is_some_and(|volume| self.engine.remove_indicator_binding(volume.columns))
    }

    /// Indicator name-chip chrome for every indicator on this chart, owned by Aeris Charts.
    #[must_use]
    pub const fn indicator_name_labels_visible(&self) -> bool {
        self.engine.indicator_chrome_options().name_labels_visible
    }
    /// Indicator last-value chrome for every indicator on this chart, owned by Aeris Charts.
    #[must_use]
    pub const fn indicator_value_labels_visible(&self) -> bool {
        self.engine.indicator_chrome_options().value_labels_visible
    }
    /// Last-value line chrome for every indicator plot on this chart, owned by Aeris Charts.
    #[must_use]
    pub const fn indicator_price_lines_visible(&self) -> bool {
        self.engine.indicator_chrome_options().price_lines_visible
    }
    /// Applies indicator name, value, and price-line preferences to current and later indicators.
    pub fn apply_indicator_chrome_preferences(
        &mut self,
        names: bool,
        values: bool,
        price_lines: bool,
    ) {
        let changed = self
            .engine
            .set_indicator_chrome_options(IndicatorChromeOptions {
                name_labels_visible: names,
                value_labels_visible: values,
                price_lines_visible: price_lines,
            });
        self.invalidate_series_layout();
        if changed {
            self.mark_user_state_changed();
        }
    }
    pub(super) fn toggle_indicator_name_labels(&mut self) -> bool {
        let chrome = self.engine.indicator_chrome_options();
        self.apply_indicator_chrome_preferences(
            !chrome.name_labels_visible,
            chrome.value_labels_visible,
            chrome.price_lines_visible,
        );
        true
    }
    pub(super) fn toggle_indicator_value_labels(&mut self) -> bool {
        let chrome = self.engine.indicator_chrome_options();
        self.apply_indicator_chrome_preferences(
            chrome.name_labels_visible,
            !chrome.value_labels_visible,
            chrome.price_lines_visible,
        );
        true
    }
    pub(super) fn toggle_indicator_price_lines(&mut self) -> bool {
        let chrome = self.engine.indicator_chrome_options();
        self.apply_indicator_chrome_preferences(
            chrome.name_labels_visible,
            chrome.value_labels_visible,
            !chrome.price_lines_visible,
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

/// Maps one engine legend row to its host control. The volume binding is a native indicator the
/// host gives its own settings dialog.
fn legend_row(
    row: aeris_charts_engine::FinancialLegendRow,
    volume_binding: Option<u32>,
) -> Option<LegendRow> {
    let item = match row.identity {
        FinancialLegendIdentity::Primary => LegendItem::Asset,
        FinancialLegendIdentity::Indicator(binding) if Some(binding) == volume_binding => {
            LegendItem::Volume
        }
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
        visible: row.visible,
        // Native indicator bindings other than volume have no host settings dialog yet.
        settings_available: item == LegendItem::Volume
            || (row.settings_available && !matches!(item, LegendItem::Indicator(_))),
    })
}

fn apply_legend_options(rows: &mut Vec<LegendRow>, options: ChartLegendOptions) {
    rows.retain(|row| {
        if row.item == LegendItem::Asset {
            options.symbol
        } else {
            options.indicators
        }
    });
    for row in rows {
        let values_shown = if row.item == LegendItem::Asset {
            options.symbol_values
        } else {
            options.indicator_values
        };
        if !values_shown {
            row.values.clear();
        }
    }
}

/// The volume profile and big trades are engine-owned price-pane primitives, not series, so the
/// financial legend has no row for them; the host adds one so they can be hidden or removed.
fn insert_price_pane_legend_row(
    rows: &mut Vec<LegendRow>,
    item: LegendItem,
    title: &str,
    visible: bool,
    settings_available: bool,
) {
    let position = rows
        .iter()
        .position(|row| row.pane != 0)
        .unwrap_or(rows.len());
    rows.insert(
        position,
        LegendRow {
            item,
            pane: 0,
            title: title.to_string(),
            values: Vec::new(),
            visible,
            settings_available,
        },
    );
}
