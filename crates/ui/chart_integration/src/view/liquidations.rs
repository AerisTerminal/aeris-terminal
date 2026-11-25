//! Thin host bridge forwarding exchange Liquidation bars and durable style/unit/color settings
//! to Aeris Charts' engine-owned Liquidations study (`ChartEngine::set_liquidations_with_options`,
//! `ChartEngine::set_liquidation_options`).

use aeris_charts_engine::{LiquidationBar, LiquidationOptions};

use super::{AerisChartView, LiquidationUnit, LiquidationsSettings, OrderFlowSettings};

/// Reserved engine study identity for the built-in Liquidations sub-pane (disjoint from
/// `OPEN_INTEREST_STUDY_ID = u64::MAX` and `StudyInstanceId` allocations minted from 1).
pub(super) const LIQUIDATIONS_STUDY_ID: u64 = u64::MAX - 1;

/// One bar-aligned liquidation volume sample carrying both USD notional and base-asset contracts
/// so unit switches preview immediately on the chart without another fetch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartLiquidationSample {
    pub timestamp_unix_nanos: i64,
    pub long_usd: f64,
    pub short_usd: f64,
    pub long_contracts: f64,
    pub short_contracts: f64,
}

#[derive(Default)]
pub(super) struct LiquidationsChartState {
    generation: u64,
    last_samples: Vec<ChartLiquidationSample>,
}

const fn liquidation_options(settings: LiquidationsSettings) -> LiquidationOptions {
    LiquidationOptions {
        style: settings.style,
        long_color: settings.long_color,
        short_color: settings.short_color,
    }
}

fn projected_bars(
    samples: &[ChartLiquidationSample],
    unit: LiquidationUnit,
) -> Vec<LiquidationBar> {
    samples
        .iter()
        .map(|sample| {
            let (long_volume, short_volume) = match unit {
                LiquidationUnit::Usd => (sample.long_usd, sample.short_usd),
                LiquidationUnit::Contracts => (sample.long_contracts, sample.short_contracts),
            };
            LiquidationBar {
                timestamp_unix_nanos: sample.timestamp_unix_nanos,
                long_volume,
                short_volume,
            }
        })
        .collect()
}

impl AerisChartView {
    /// Installs or refreshes the engine-owned Liquidations sub-pane from bar-aligned
    /// [`ChartLiquidationSample`]s, applying the durable style, unit, and color settings.
    pub fn apply_liquidations(&mut self, samples: &[ChartLiquidationSample]) -> bool {
        let Some(settings) = self.order_flow_settings.liquidations else {
            return self.clear_liquidations();
        };
        if samples.is_empty() || !self.has_market_data() {
            self.liquidations_state.last_samples.clear();
            return self.clear_liquidations();
        }
        self.liquidations_state.last_samples = samples.to_vec();
        self.push_liquidations_to_engine(settings)
    }

    fn push_liquidations_to_engine(&mut self, settings: LiquidationsSettings) -> bool {
        if self.liquidations_state.last_samples.is_empty() || !self.has_market_data() {
            return false;
        }
        let created = self
            .engine
            .liquidation_series(LIQUIDATIONS_STUDY_ID)
            .is_none();
        self.liquidations_state.generation =
            self.liquidations_state.generation.wrapping_add(1).max(1);
        let generation = self.liquidations_state.generation;
        let bars = projected_bars(&self.liquidations_state.last_samples, settings.unit);
        match self.engine.set_liquidations_with_options(
            LIQUIDATIONS_STUDY_ID,
            generation,
            &bars,
            liquidation_options(settings),
        ) {
            Ok(changed) => {
                let _ = self
                    .engine
                    .set_external_study_visible(LIQUIDATIONS_STUDY_ID, settings.visible);
                if created {
                    self.invalidate_series_layout();
                } else if changed {
                    self.invalidate_series_frame();
                }
                changed || created
            }
            Err(_) => false,
        }
    }

    /// Reconfigures the live Liquidations series' style, unit, colors, and visibility in place
    /// without dropping its pane or bar history.
    pub(super) fn sync_liquidations_settings(
        &mut self,
        previous: Option<LiquidationsSettings>,
        settings: LiquidationsSettings,
    ) {
        let unit_changed = previous.is_some_and(|prev| prev.unit != settings.unit);
        if unit_changed && !self.liquidations_state.last_samples.is_empty() {
            if self.push_liquidations_to_engine(settings) {
                self.invalidate_series_layout();
            }
            return;
        }
        let restyled = self
            .engine
            .set_liquidation_options(LIQUIDATIONS_STUDY_ID, liquidation_options(settings));
        let visibility_changed = self
            .engine
            .set_external_study_visible(LIQUIDATIONS_STUDY_ID, settings.visible);
        if restyled || visibility_changed {
            self.invalidate_series_layout();
        }
    }

    /// Removes the drawn Liquidations sub-pane without changing the durable indicator setting.
    pub fn clear_liquidations(&mut self) -> bool {
        self.liquidations_state.last_samples.clear();
        let removed = self
            .engine
            .remove_external_studies(&[LIQUIDATIONS_STUDY_ID]);
        if removed {
            self.invalidate_series_layout();
        }
        removed
    }

    pub(super) fn has_liquidations_series(&self) -> bool {
        self.engine
            .liquidation_series(LIQUIDATIONS_STUDY_ID)
            .is_some()
    }

    pub(super) fn remove_liquidations(&mut self) -> bool {
        if self.order_flow_settings.liquidations.is_none() {
            return false;
        }
        self.edit_order_flow_settings(OrderFlowSettings {
            liquidations: None,
            ..self.order_flow_settings
        })
        .unwrap_or(false)
    }

    pub(super) fn set_liquidations_visible(&mut self, visible: bool) -> bool {
        let Some(current) = self.order_flow_settings.liquidations else {
            return false;
        };
        if current.visible == visible {
            return false;
        }
        let changed = self
            .set_order_flow_settings(OrderFlowSettings {
                liquidations: Some(LiquidationsSettings { visible, ..current }),
                ..self.order_flow_settings
            })
            .unwrap_or(false);
        if changed {
            let _ = self
                .engine
                .set_external_study_visible(LIQUIDATIONS_STUDY_ID, visible);
            self.invalidate_series_layout();
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use aeris_charts_engine::{LiquidationStyle, PriceFormatKind, SeriesKind};

    use super::*;
    use crate::view::{ChartColor, LegendItem};

    const NANOS_PER_SECOND: i64 = 1_000_000_000;

    #[test]
    fn liquidations_delegates_to_engine_multi_mode_study_unit_and_legend() {
        let mut chart = AerisChartView::new();
        assert!(
            chart
                .set_order_flow_settings(OrderFlowSettings {
                    liquidations: Some(LiquidationsSettings::default()),
                    ..OrderFlowSettings::default()
                })
                .expect("enable liquidations")
        );

        let last_sec = chart.product_bars.last_time().expect("demo chart has bars") as i64;
        let initial_panes = chart.engine.panes.len();
        assert!(chart.apply_liquidations(&[
            ChartLiquidationSample {
                timestamp_unix_nanos: (last_sec - 120) * NANOS_PER_SECOND,
                long_usd: 150_000.0,
                short_usd: 40_000.0,
                long_contracts: 2.5,
                short_contracts: 0.6,
            },
            ChartLiquidationSample {
                timestamp_unix_nanos: (last_sec - 60) * NANOS_PER_SECOND,
                long_usd: 80_000.0,
                short_usd: 220_000.0,
                long_contracts: 1.3,
                short_contracts: 3.6,
            },
            ChartLiquidationSample {
                timestamp_unix_nanos: last_sec * NANOS_PER_SECOND,
                long_usd: 320_000.0,
                short_usd: 90_000.0,
                long_contracts: 5.2,
                short_contracts: 1.5,
            },
        ]));

        let [s0, s1] = chart
            .engine
            .liquidation_series(LIQUIDATIONS_STUDY_ID)
            .expect("engine liquidation series");
        let entry0 = chart
            .engine
            .series
            .iter()
            .find(|series| series.id == s0 && !series.removed)
            .expect("primary liquidation series");
        assert_eq!(entry0.kind, SeriesKind::Histogram);
        assert_eq!(entry0.price_format.kind, PriceFormatKind::Volume);
        assert!(entry0.pane_index > 0);
        assert_eq!(chart.engine.panes.len(), initial_panes + 1);

        let rows = chart.legend_rows();
        let liq_row = rows
            .iter()
            .find(|row| row.item == LegendItem::Liquidations)
            .expect("liquidations legend row");
        assert!(liq_row.pane > 0);
        assert!(liq_row.settings_available);
        assert_eq!(
            liq_row.item.settings_request(),
            Some(crate::view::ChartSettingsRequest::Liquidations)
        );
        assert_eq!(liq_row.values.len(), 2);

        // Switching across Stacked, Net, Split and Contracts unit updates in place.
        for (style, unit) in [
            (LiquidationStyle::Stacked, LiquidationUnit::Usd),
            (LiquidationStyle::Net, LiquidationUnit::Contracts),
            (LiquidationStyle::Split, LiquidationUnit::Usd),
        ] {
            let mut settings = chart.order_flow_settings();
            settings.liquidations = Some(LiquidationsSettings {
                visible: true,
                style,
                unit,
                long_color: ChartColor::from_hex("#FF1744"),
                short_color: ChartColor::from_hex("#00C853"),
            });
            assert!(
                chart
                    .set_order_flow_settings(settings)
                    .expect("restyle liquidations")
            );
            assert_eq!(
                chart.engine.liquidation_series(LIQUIDATIONS_STUDY_ID),
                Some([s0, s1])
            );
            assert_eq!(
                chart.engine.series_visible(s1),
                Some(style != LiquidationStyle::Net)
            );
        }

        // Visibility toggle preserves colors and style.
        assert!(chart.set_liquidations_visible(false));
        assert_eq!(chart.engine.series_visible(s0), Some(false));
        assert_eq!(
            chart.order_flow_settings().liquidations.unwrap().long_color,
            ChartColor::from_hex("#FF1744")
        );
        assert!(chart.clear_liquidations());
        assert_eq!(chart.engine.liquidation_series(LIQUIDATIONS_STUDY_ID), None);
        assert_eq!(chart.engine.panes.len(), initial_panes);
    }

    #[test]
    fn keyboard_delete_of_selected_liquidations_removes_both_series_without_runtime_error() {
        for selected_output in [0usize, 1usize] {
            let mut chart = AerisChartView::new();
            assert!(
                chart
                    .set_order_flow_settings(OrderFlowSettings {
                        liquidations: Some(LiquidationsSettings::default()),
                        ..OrderFlowSettings::default()
                    })
                    .expect("enable liquidations")
            );
            let last_sec = chart.product_bars.last_time().expect("demo chart has bars") as i64;
            assert!(chart.apply_liquidations(&[ChartLiquidationSample {
                timestamp_unix_nanos: last_sec * NANOS_PER_SECOND,
                long_usd: 250_000.0,
                short_usd: 150_000.0,
                long_contracts: 4.0,
                short_contracts: 2.5,
            }]));
            let series = chart
                .engine
                .liquidation_series(LIQUIDATIONS_STUDY_ID)
                .expect("liquidation series");
            chart
                .engine
                .set_selected_series(Some(series[selected_output]));
            assert!(chart.has_deletable_selection());
            assert!(chart.engine.input_key_down(
                aeris_charts_engine::ChartKey::Delete,
                aeris_charts_engine::InputModifiers::default(),
                false,
                0.0,
            ));
            let host_events = chart.process_input_events();
            assert_eq!(
                host_events,
                vec![crate::view::ChartHostEvent::OrderFlowSettingsChanged],
                "Liquidations Delete must remove through OrderFlowSettings, never emit RemoveStudy"
            );
            assert_eq!(chart.order_flow_settings().liquidations, None);
            assert_eq!(chart.engine.liquidation_series(LIQUIDATIONS_STUDY_ID), None);
            assert_eq!(chart.engine.selected_series(), None);
        }
    }
}
