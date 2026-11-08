//! Indicator templates on one chart: the chart's indicators captured under a name, and a saved
//! template applied in place of every indicator the chart shows.

use super::{
    App, ChartIndicatorState, Context, OrderFlowSettings, WorkspaceIndicatorTemplateState,
    WorkspaceSurface, persisted_legacy_indicator_states, persisted_order_flow_settings,
    provider_trades_available, restored_legacy_indicator_states, restored_order_flow_settings,
};
use aeris_localization::{tr, workspace};

impl WorkspaceSurface {
    /// Every indicator of the chart as a template named `name`; `None` until the chart has data.
    pub(in crate::desktop) fn indicator_template(
        &self,
        name: String,
        cx: &App,
    ) -> Option<WorkspaceIndicatorTemplateState> {
        let chart = self.chart.as_ref()?.read(cx);
        if !chart.has_market_data() {
            return None;
        }
        Some(WorkspaceIndicatorTemplateState {
            name,
            indicators: persisted_legacy_indicator_states(chart.indicator_states()),
            studies: self
                .studies
                .persisted_states(|study_id| chart.study_visible(study_id.get())),
            order_flow: Some(persisted_order_flow_settings(chart.order_flow_settings())),
            favorite: false,
        })
    }

    /// Replaces every indicator of the chart with the template's, as one undo step. The chart
    /// keeps its footprint presentation, and order-flow indicators built from trade prints are
    /// left out on a provider without a trade tape.
    pub(in crate::desktop) fn apply_indicator_template(
        &mut self,
        template: &WorkspaceIndicatorTemplateState,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(chart) = self.chart.clone() else {
            return false;
        };
        if !chart.read(cx).has_market_data() {
            Self::post_study_failure(tr!(workspace::CHART_DATA_UNAVAILABLE_YET), cx);
            return false;
        }
        let indicators: Vec<ChartIndicatorState> =
            restored_legacy_indicator_states(&template.indicators);
        chart.update(cx, |chart, _| chart.begin_history_group());
        self.clear_indicators(cx);
        let added = chart.update(cx, |chart, chart_cx| {
            let added = chart.add_indicator_states(&indicators);
            chart_cx.notify();
            added
        });
        if added.is_err() {
            Self::post_study_failure(tr!(workspace::INDICATOR_ADD_FAILED), cx);
        }
        if !template.studies.is_empty() {
            let studies = self.reinstate_runtime_studies(template.studies.clone(), cx);
            self.record_runtime_study_edit(true, studies, cx);
        }
        if let Some(saved) = template
            .order_flow
            .as_ref()
            .and_then(restored_order_flow_settings)
            && let Some(current) = self.chart_order_flow_settings(cx)
        {
            let mut settings = OrderFlowSettings {
                display_mode: current.display_mode,
                ticks_per_row: current.ticks_per_row,
                ..saved
            };
            if !provider_trades_available(self.provider) {
                settings.show_cumulative_delta = false;
                settings.show_delta_histogram = false;
                settings.big_trades = None;
            }
            self.set_chart_order_flow_settings(settings, cx);
        }
        chart.update(cx, |chart, _| chart.end_history_group());
        self.retain_chart_presentation(cx);
        self.persistence.mark_dirty();
        cx.notify();
        added.is_ok()
    }
}
