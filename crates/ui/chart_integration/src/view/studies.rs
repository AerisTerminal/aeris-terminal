//! Thin GPUI integration for engine-owned external-study projections.

use super::{
    AerisChartView, ChartStudyInputRequirements, ChartStudyOutputDescriptor, ChartStudyOutputError,
};

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
        if changed {
            if created {
                self.invalidate_series_layout();
            } else {
                self.invalidate_series_frame();
            }
        }
        Ok(changed)
    }

    pub fn remove_study_outputs(&mut self, study_ids: &[u64]) -> bool {
        let changed = self.engine.remove_external_studies(study_ids);
        if changed {
            self.invalidate_series_layout();
        }
        changed
    }
}
