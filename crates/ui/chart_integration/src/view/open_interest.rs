//! Thin host bridge forwarding exchange Open Interest bars to Aeris Charts' engine-owned
//! candlestick Open Interest study (`ChartEngine::set_open_interest`).

use aeris_charts_engine::OpenInterestBar;

use super::{AerisChartView, OpenInterestSettings, OrderFlowSettings};

/// Reserved engine study identity for the built-in Open Interest sub-pane (disjoint from
/// `StudyInstanceId` allocations minted sequentially from 1 by `market_runtime`).
pub(super) const OPEN_INTEREST_STUDY_ID: u64 = u64::MAX;

#[derive(Default)]
pub(super) struct OpenInterestChartState {
    generation: u64,
}

impl AerisChartView {
    /// Installs or refreshes the engine-owned Open Interest candlestick sub-pane from bar-aligned
    /// [`OpenInterestBar`] samples.
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
        match self
            .engine
            .set_open_interest(OPEN_INTEREST_STUDY_ID, generation, bars)
        {
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
        if self
            .order_flow_settings
            .open_interest
            .is_none_or(|oi| oi.visible == visible)
        {
            return false;
        }
        let changed = self
            .set_order_flow_settings(OrderFlowSettings {
                open_interest: Some(OpenInterestSettings { visible }),
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
    use aeris_charts_engine::{PriceFormatKind, SeriesKind};

    use super::*;
    use crate::view::LegendItem;

    const NANOS_PER_SECOND: i64 = 1_000_000_000;

    #[test]
    fn open_interest_delegates_to_engine_candlestick_study_and_legend() {
        let mut chart = AerisChartView::new();
        assert!(
            chart
                .set_order_flow_settings(OrderFlowSettings {
                    open_interest: Some(OpenInterestSettings { visible: true }),
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
        assert_eq!(entry.kind, SeriesKind::Candlestick);
        assert_eq!(entry.price_format.kind, PriceFormatKind::Volume);
        assert!(entry.pane_index > 0);
        assert_eq!(chart.engine.panes.len(), initial_panes + 1);

        let rows = chart.legend_rows();
        let oi_row = rows
            .iter()
            .find(|row| row.item == LegendItem::OpenInterest)
            .expect("open interest legend row");
        assert!(oi_row.pane > 0);
        assert_eq!(oi_row.values.len(), 4);
        assert!(oi_row.values[0].text.starts_with("O "));
        assert!(oi_row.values[1].text.starts_with("H "));
        assert!(oi_row.values[2].text.starts_with("L "));
        assert!(oi_row.values[3].text.starts_with("C "));

        assert!(chart.set_open_interest_visible(false));
        assert_eq!(chart.engine.series_visible(series_id), Some(false));
        assert!(chart.clear_open_interest());
        assert_eq!(
            chart.engine.open_interest_series(OPEN_INTEREST_STUDY_ID),
            None
        );
        assert_eq!(chart.engine.panes.len(), initial_panes);
    }
}
