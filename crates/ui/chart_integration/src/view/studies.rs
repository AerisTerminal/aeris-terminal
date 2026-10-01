//! Thin GPUI integration for engine-owned external-study projections.

use super::{
    AerisChartView, ChartStudyInputRequirements, ChartStudyOutputDescriptor, ChartStudyOutputError,
};

/// Stroke width Aeris Charts gives a study output until the host chooses one.
pub const DEFAULT_STUDY_LINE_WIDTH: u8 = 2;
/// Widest selectable study stroke, matching the chart's other line-width controls.
pub const MAXIMUM_STUDY_LINE_WIDTH: u8 = 4;

impl AerisChartView {
    #[must_use]
    pub fn study_output_input_requirements(
        &self,
        study_id: u64,
        output_index: usize,
    ) -> Option<ChartStudyInputRequirements> {
        self.engine
            .external_study_output_info(study_id, output_index)
            .map(|output| output.input_requirements)
    }

    #[must_use]
    pub fn study_visible(&self, study_id: u64) -> Option<bool> {
        self.engine.external_study_visible(study_id)
    }

    pub fn set_study_visible(&mut self, study_id: u64, visible: bool) -> bool {
        let changed = self.engine.set_external_study_visible(study_id, visible);
        if changed {
            self.invalidate_series_layout();
            self.mark_user_state_changed();
        }
        changed
    }

    /// Installs one generation-fenced host-computed output through Aeris Charts.
    ///
    /// # Errors
    /// Returns the engine's typed validation or bounded-resource failure without partially
    /// replacing the previously installed output.
    pub fn install_study_output(
        &mut self,
        study_id: u64,
        output_index: usize,
        descriptor: ChartStudyOutputDescriptor<'_>,
        generation: u64,
        timestamps_unix_nanos: &[i64],
        values: &[Option<f64>],
    ) -> Result<bool, ChartStudyOutputError> {
        let created = self
            .engine
            .external_study_output_info(study_id, output_index)
            .is_none();
        let changed = self.engine.install_external_study_output(
            study_id,
            output_index,
            descriptor,
            generation,
            timestamps_unix_nanos,
            values,
        )?;
        if created
            && let Some(width) = self.study_line_widths.get(&study_id).copied()
            && let Some(output) = self
                .engine
                .external_study_output_info(study_id, output_index)
        {
            self.engine
                .series_apply_options_json(output.series_id, &study_line_width_patch(width));
        }
        if changed {
            if created {
                self.invalidate_series_layout();
            } else {
                self.invalidate_series_frame();
            }
        }
        Ok(changed)
    }

    /// Sets the stroke width of every output of one study, including outputs installed later.
    pub fn set_study_line_width(&mut self, study_id: u64, width: u8) -> bool {
        let width = width.clamp(1, MAXIMUM_STUDY_LINE_WIDTH);
        if self.study_line_widths.insert(study_id, width) == Some(width) {
            return false;
        }
        let patch = study_line_width_patch(width);
        let series = self
            .engine
            .external_study_outputs()
            .into_iter()
            .filter(|output| output.study_id == study_id)
            .map(|output| output.series_id)
            .collect::<Vec<_>>();
        let mut changed = false;
        for series_id in series {
            changed |= self.engine.series_apply_options_json(series_id, &patch);
        }
        if changed {
            self.invalidate_series_frame();
        }
        changed
    }

    /// Restores every study's host-chosen stroke width after the engine restyles its series.
    pub(super) fn reapply_study_line_widths(&mut self) {
        for output in self.engine.external_study_outputs() {
            if let Some(width) = self.study_line_widths.get(&output.study_id).copied() {
                self.engine
                    .series_apply_options_json(output.series_id, &study_line_width_patch(width));
            }
        }
    }

    #[must_use]
    pub fn study_line_width(&self, study_id: u64) -> u8 {
        self.study_line_widths
            .get(&study_id)
            .copied()
            .unwrap_or(DEFAULT_STUDY_LINE_WIDTH)
    }

    pub fn remove_study_outputs(&mut self, study_ids: &[u64]) -> bool {
        for study_id in study_ids {
            self.study_line_widths.remove(study_id);
        }
        let changed = self.engine.remove_external_studies(study_ids);
        if changed {
            self.invalidate_series_layout();
        }
        changed
    }
}

fn study_line_width_patch(width: u8) -> String {
    format!("{{\"line_width\":{width}}}")
}
