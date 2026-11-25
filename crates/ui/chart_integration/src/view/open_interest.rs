//! Chart-owned Open Interest sub-pane projection backed by Aeris Charts' external-study engine.

use aeris_charts_engine::{
    ExternalStudyInputRequirements, ExternalStudyOutputDescriptor, ExternalStudyPaneTarget,
    ExternalStudyPlotKind, ExternalStudyPointStyle, ExternalStudyScaleTarget,
};
use aeris_localization::{chart_ui, tr};

use super::{AerisChartView, OpenInterestSettings, OrderFlowSettings};

/// Reserved engine study identity for the built-in Open Interest sub-pane (disjoint from
/// `StudyInstanceId` allocations minted sequentially from 1 by `market_runtime`).
pub(super) const OPEN_INTEREST_STUDY_ID: u64 = u64::MAX;

#[derive(Default)]
pub(super) struct OpenInterestChartState {
    generation: u64,
}

impl AerisChartView {
    /// Installs or refreshes the Open Interest sub-pane series from bar-aligned `(timestamp_unix_nanos, contracts)` pairs.
    pub fn apply_open_interest(&mut self, samples: &[(i64, f64)]) -> bool {
        let Some(settings) = self.order_flow_settings.open_interest else {
            return self.clear_open_interest();
        };
        if samples.is_empty() || !self.has_market_data() {
            return self.clear_open_interest();
        }
        let mut timestamps = Vec::with_capacity(samples.len());
        let mut values = Vec::with_capacity(samples.len());
        for &(ts, value) in samples {
            if value.is_finite() && value >= 0.0 {
                timestamps.push(ts);
                values.push(Some(value));
            }
        }
        if timestamps.is_empty() {
            return self.clear_open_interest();
        }
        let created = self
            .engine
            .external_study_output_info(OPEN_INTEREST_STUDY_ID, 0)
            .is_none();
        self.open_interest_state.generation =
            self.open_interest_state.generation.wrapping_add(1).max(1);
        let generation = self.open_interest_state.generation;
        let descriptor = ExternalStudyOutputDescriptor {
            title: tr!(chart_ui::LEGEND_OPEN_INTEREST),
            legend_label: None,
            plot: ExternalStudyPlotKind::Area,
            pane: ExternalStudyPaneTarget::Dedicated { group: 0 },
            scale: ExternalStudyScaleTarget::Primary,
            settings_available: false,
            threshold_region: None,
            point_style: ExternalStudyPointStyle::default(),
            input_requirements: ExternalStudyInputRequirements::default(),
        };
        match self.engine.install_external_study_output(
            OPEN_INTEREST_STUDY_ID,
            0,
            descriptor,
            generation,
            &timestamps,
            &values,
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
            .external_study_output_info(OPEN_INTEREST_STUDY_ID, 0)
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
