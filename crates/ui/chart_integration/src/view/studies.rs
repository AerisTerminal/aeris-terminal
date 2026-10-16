//! Thin GPUI integration for engine-owned external-study projections.

use super::{
    AerisChartView, ChartStudyInputRequirements, ChartStudyOutputDescriptor, ChartStudyOutputError,
};
use aeris_charts_engine::IndicatorOutputStyle;
use aeris_charts_render::color::Color;

/// Stroke width Aeris Charts gives a study output until the host chooses one.
pub const DEFAULT_STUDY_LINE_WIDTH: u8 = 2;
/// Widest selectable study stroke, matching the chart's other line-width controls.
pub const MAXIMUM_STUDY_LINE_WIDTH: u8 = 4;

/// Host-chosen presentation of one external study, kept for outputs installed later.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct StudyStyle {
    line_width: Option<u8>,
    /// Indexed by output; `None` keeps the chart's default color for that output.
    output_colors: Vec<Option<String>>,
}

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
        if created {
            // The engine paints a new output in its default color; record it before any host
            // color replaces it so choosing the default again restores exactly that color.
            let default_color = self
                .engine
                .external_study_output_style(study_id, output_index)
                .and_then(|style| style.line_color);
            self.study_default_colors
                .insert((study_id, output_index), default_color);
            self.apply_study_style(study_id, output_index);
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
        let style = self.study_styles.entry(study_id).or_default();
        if style.line_width == Some(width) {
            return false;
        }
        style.line_width = Some(width);
        self.apply_study_styles(study_id)
    }

    /// Sets each output's line color by output index, including outputs installed later. `None`,
    /// a missing entry, or a color the chart cannot parse keeps the chart's default color.
    pub fn set_study_output_colors(&mut self, study_id: u64, colors: &[Option<String>]) -> bool {
        let colors = colors
            .iter()
            .map(|color| {
                color
                    .as_ref()
                    .filter(|color| Color::parse_css(color).is_some())
                    .cloned()
            })
            .collect::<Vec<_>>();
        let style = self.study_styles.entry(study_id).or_default();
        if style.output_colors == colors {
            return false;
        }
        style.output_colors = colors;
        self.apply_study_styles(study_id)
    }

    /// The color one output paints its line with.
    #[must_use]
    pub fn study_output_color(&self, study_id: u64, output_index: usize) -> Option<String> {
        self.engine
            .external_study_output_style(study_id, output_index)
            .and_then(|style| style.line_color)
    }

    /// The color Aeris Charts gave one output before any host color.
    #[must_use]
    pub fn study_output_default_color(&self, study_id: u64, output_index: usize) -> Option<String> {
        self.study_default_colors
            .get(&(study_id, output_index))
            .cloned()
            .flatten()
    }

    /// Restores every study's host-chosen style after the engine restyles its series.
    pub(super) fn reapply_study_styles(&mut self) {
        for output in self.engine.external_study_outputs() {
            self.apply_study_style(output.study_id, output.output_index);
        }
    }

    #[must_use]
    pub fn study_line_width(&self, study_id: u64) -> u8 {
        self.study_styles
            .get(&study_id)
            .and_then(|style| style.line_width)
            .unwrap_or(DEFAULT_STUDY_LINE_WIDTH)
    }

    pub fn remove_study_outputs(&mut self, study_ids: &[u64]) -> bool {
        for study_id in study_ids {
            self.study_styles.remove(study_id);
        }
        self.study_default_colors
            .retain(|(study_id, _), _| !study_ids.contains(study_id));
        let changed = self.engine.remove_external_studies(study_ids);
        if changed {
            self.invalidate_series_layout();
        }
        changed
    }

    fn apply_study_styles(&mut self, study_id: u64) -> bool {
        let outputs = self
            .engine
            .external_study_outputs()
            .into_iter()
            .filter(|output| output.study_id == study_id)
            .map(|output| output.output_index)
            .collect::<Vec<_>>();
        let mut changed = false;
        for output_index in outputs {
            changed |= self.apply_study_style(study_id, output_index);
        }
        if changed {
            self.invalidate_series_frame();
        }
        changed
    }

    /// Writes the host width and color onto one output through the engine's style API,
    /// leaving visibility and every other style field as the engine holds them.
    fn apply_study_style(&mut self, study_id: u64, output_index: usize) -> bool {
        let Some(current) = self
            .engine
            .external_study_output_style(study_id, output_index)
        else {
            return false;
        };
        let style = self.study_styles.get(&study_id);
        let line_width = style
            .and_then(|style| style.line_width)
            .map(f64::from)
            .or(current.line_width);
        let line_color = style
            .and_then(|style| style.output_colors.get(output_index).cloned().flatten())
            .or_else(|| self.study_output_default_color(study_id, output_index))
            .or_else(|| current.line_color.clone());
        let desired = IndicatorOutputStyle {
            line_width,
            line_color,
            ..current.clone()
        };
        desired != current
            && self
                .engine
                .set_external_study_output_style(study_id, output_index, desired)
    }
}
