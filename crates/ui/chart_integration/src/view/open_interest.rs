//! Thin host bridge forwarding exchange Open Interest bars and durable style/color settings to
//! Aeris Charts' engine-owned Open Interest study (`ChartEngine::set_open_interest_with_options`,
//! `ChartEngine::set_open_interest_options`).

use aeris_charts_engine::{OpenInterestBar, OpenInterestOptions};

use super::{AerisChartView, OpenInterestSettings, OrderFlowSettings};

/// Reserved engine study identity for the built-in Open Interest sub-pane (disjoint from
/// `StudyInstanceId` allocations minted sequentially from 1 by `market_runtime`).
pub(super) const OPEN_INTEREST_STUDY_ID: u64 = u64::MAX;

#[derive(Default)]
pub(super) struct OpenInterestChartState {
    generation: u64,
}

const fn open_interest_options(settings: OpenInterestSettings) -> OpenInterestOptions {
    OpenInterestOptions {
        style: settings.style,
        line_color: settings.line_color,
        up_color: settings.up_color,
        down_color: settings.down_color,
    }
}

impl AerisChartView {
    /// Installs or refreshes the engine-owned Open Interest sub-pane from bar-aligned
    /// [`OpenInterestBar`] samples, applying the durable style and color settings.
    pub fn apply_open_interest(&mut self, bars: &[OpenInterestBar]) -> bool {
        let Some(settings) = self.order_flow_settings.open_interest else {
            return self.clear_open_interest();
        };
        if bars.is_empty() || !self.has_market_data() {
            return self.clear_open_interest();
        }
        let created = self
            .engine
            .open_interest_series(OPEN_INTEREST_STUDY_ID)
            .is_none();
        self.open_interest_state.generation =
            self.open_interest_state.generation.wrapping_add(1).max(1);
        let generation = self.open_interest_state.generation;
        match self.engine.set_open_interest_with_options(
            OPEN_INTEREST_STUDY_ID,
            generation,
            bars,
            open_interest_options(settings),
        ) {
            Ok(changed) => {
                let _ = self
                    .engine
                    .set_external_study_visible(OPEN_INTEREST_STUDY_ID, settings.visible);
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

    /// Reconfigures the live Open Interest series' style (candles, bars, line, area), colors, and
    /// visibility in place without dropping its pane or bar history.
    pub(super) fn sync_open_interest_settings(&mut self, settings: OpenInterestSettings) {
        let restyled = self
            .engine
            .set_open_interest_options(OPEN_INTEREST_STUDY_ID, open_interest_options(settings));
        let visibility_changed = self
            .engine
            .set_external_study_visible(OPEN_INTEREST_STUDY_ID, settings.visible);
        if restyled || visibility_changed {
            self.invalidate_series_layout();
        }
    }

    /// Removes the drawn Open Interest sub-pane without changing the durable indicator setting.
    pub fn clear_open_interest(&mut self) -> bool {
        let removed = self
            .engine
            .remove_external_studies(&[OPEN_INTEREST_STUDY_ID]);
        if removed {
            self.invalidate_series_layout();
        }
        removed
    }

    pub(super) fn has_open_interest_series(&self) -> bool {
        self.engine
            .open_interest_series(OPEN_INTEREST_STUDY_ID)
            .is_some()
    }

    pub(super) fn remove_open_interest(&mut self) -> bool {
        if self.order_flow_settings.open_interest.is_none() {
            return false;
        }
        self.edit_order_flow_settings(OrderFlowSettings {
            open_interest: None,
            ..self.order_flow_settings
        })
        .unwrap_or(false)
    }

    pub(super) fn set_open_interest_visible(&mut self, visible: bool) -> bool {
        let Some(current) = self.order_flow_settings.open_interest else {
            return false;
        };
        if current.visible == visible {
            return false;
        }
        let changed = self
            .set_order_flow_settings(OrderFlowSettings {
                open_interest: Some(OpenInterestSettings { visible, ..current }),
                ..self.order_flow_settings
            })
            .unwrap_or(false);
        if changed {
            let _ = self
                .engine
                .set_external_study_visible(OPEN_INTEREST_STUDY_ID, visible);
            self.invalidate_series_layout();
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use aeris_charts_engine::{OpenInterestStyle, PriceFormatKind, SeriesKind};

    use super::*;
    use crate::view::{ChartColor, LegendItem};

    const NANOS_PER_SECOND: i64 = 1_000_000_000;

    #[test]
    fn open_interest_delegates_to_engine_multi_mode_study_and_legend() {
        let mut chart = AerisChartView::new();
        assert!(
            chart
                .set_order_flow_settings(OrderFlowSettings {
                    open_interest: Some(OpenInterestSettings::default()),
                    ..OrderFlowSettings::default()
                })
                .expect("enable open interest")
        );

        let last_sec = chart.product_bars.last_time().expect("demo chart has bars") as i64;
        let initial_panes = chart.engine.panes.len();
        assert!(chart.apply_open_interest(&[
            OpenInterestBar {
                timestamp_unix_nanos: (last_sec - 120) * NANOS_PER_SECOND,
                open: None,
                high: Some(10_200.0),
                low: Some(9_900.0),
                close: 10_000.0,
            },
            OpenInterestBar {
                timestamp_unix_nanos: (last_sec - 60) * NANOS_PER_SECOND,
                open: None,
                high: Some(10_750.0),
                low: Some(9_950.0),
                close: 10_500.0,
            },
            OpenInterestBar {
                timestamp_unix_nanos: last_sec * NANOS_PER_SECOND,
                open: None,
                high: Some(10_600.0),
                low: Some(10_250.0),
                close: 10_300.0,
            },
        ]));

        let series_id = chart
            .engine
            .open_interest_series(OPEN_INTEREST_STUDY_ID)
            .expect("engine open interest series");
        let entry = chart
            .engine
            .series
            .iter()
            .find(|series| series.id == series_id && !series.removed)
            .expect("series entry");
        assert_eq!(entry.kind, SeriesKind::Line);
        assert_eq!(entry.price_format.kind, PriceFormatKind::Volume);
        assert!(entry.pane_index > 0);
        assert_eq!(chart.engine.panes.len(), initial_panes + 1);

        let rows = chart.legend_rows();
        let oi_row = rows
            .iter()
            .find(|row| row.item == LegendItem::OpenInterest)
            .expect("open interest legend row");
        assert!(oi_row.pane > 0);
        assert!(oi_row.settings_available);
        assert_eq!(
            oi_row.item.settings_request(),
            Some(crate::view::ChartSettingsRequest::OpenInterest)
        );
        assert_eq!(oi_row.values.len(), 1);

        // Clicking a legend row activates it and clicking again deactivates it.
        chart.toggle_active_legend_item(LegendItem::OpenInterest);
        assert_eq!(chart.active_legend_item, Some(LegendItem::OpenInterest));
        chart.toggle_active_legend_item(LegendItem::Asset);
        assert_eq!(chart.active_legend_item, Some(LegendItem::Asset));
        chart.toggle_active_legend_item(LegendItem::Asset);
        assert_eq!(chart.active_legend_item, None);

        // Switching across Candles, Bars, Area, and back to Line updates the series in place.
        for (style, expected_kind) in [
            (OpenInterestStyle::Candles, SeriesKind::Candlestick),
            (OpenInterestStyle::Bars, SeriesKind::Bar),
            (OpenInterestStyle::Area, SeriesKind::Area),
            (OpenInterestStyle::Line, SeriesKind::Line),
        ] {
            let mut settings = chart.order_flow_settings();
            settings.open_interest = Some(OpenInterestSettings {
                visible: true,
                style,
                line_color: ChartColor::from_hex("#2962FF"),
                up_color: ChartColor::from_hex("#00C853"),
                down_color: ChartColor::from_hex("#FF1744"),
            });
            assert!(chart.set_order_flow_settings(settings).expect("restyle oi"));
            let entry = chart
                .engine
                .series
                .iter()
                .find(|series| series.id == series_id && !series.removed)
                .expect("series stays alive across restyle");
            assert_eq!(entry.kind, expected_kind);
        }

        // Visibility toggle preserves style and colors.
        assert!(chart.set_open_interest_visible(false));
        assert_eq!(chart.engine.series_visible(series_id), Some(false));
        assert_eq!(
            chart.order_flow_settings().open_interest.unwrap().up_color,
            ChartColor::from_hex("#00C853")
        );
        assert!(chart.clear_open_interest());
        assert_eq!(
            chart.engine.open_interest_series(OPEN_INTEREST_STUDY_ID),
            None
        );
        assert_eq!(chart.engine.panes.len(), initial_panes);
    }

    #[test]
    fn keyboard_delete_of_selected_open_interest_removes_study_without_runtime_error() {
        let mut chart = AerisChartView::new();
        assert!(
            chart
                .set_order_flow_settings(OrderFlowSettings {
                    open_interest: Some(OpenInterestSettings::default()),
                    ..OrderFlowSettings::default()
                })
                .expect("enable open interest")
        );
        let last_sec = chart.product_bars.last_time().expect("demo chart has bars") as i64;
        assert!(chart.apply_open_interest(&[OpenInterestBar {
            timestamp_unix_nanos: last_sec * NANOS_PER_SECOND,
            open: None,
            high: Some(10_500.0),
            low: Some(10_000.0),
            close: 10_250.0,
        }]));
        let series_id = chart
            .engine
            .open_interest_series(OPEN_INTEREST_STUDY_ID)
            .expect("open interest series");
        chart.engine.set_selected_series(Some(series_id));
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
            "Open Interest Delete must remove through OrderFlowSettings, never emit RemoveStudy(u64::MAX)"
        );
        assert_eq!(chart.order_flow_settings().open_interest, None);
        assert_eq!(
            chart.engine.open_interest_series(OPEN_INTEREST_STUDY_ID),
            None
        );
        assert_eq!(chart.engine.selected_series(), None);
    }
}
