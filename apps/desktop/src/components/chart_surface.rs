//! Chart pane host and chart-surface notices.

use super::{
    AerisChartView, AerisTheme, ChartNoticePlacement, ChartNoticeTone, ChartState,
    ChartSurfaceNotice, Div, Entity, HugeIcon, InteractiveElement, IntoElement, Loader,
    ParentElement, Role, StatefulInteractiveElement, Styled, chart_chrome, chart_surface_notice,
    div, gpui_color, px,
};

pub(super) struct MarketWorkspaceState<'a> {
    pub(super) pane_id: u64,
    pub(super) chart: Option<&'a Entity<AerisChartView>>,
    pub(super) chart_has_market_data: bool,
    pub(super) chart_is_superseded: bool,
    pub(super) chart_state: ChartState,
    pub(super) chart_status_detail: String,
    pub(super) theme: &'a AerisTheme,
}

#[allow(clippy::too_many_lines)]
pub(super) fn market_workspace(state: MarketWorkspaceState<'_>) -> impl IntoElement + use<> {
    let MarketWorkspaceState {
        pane_id,
        chart,
        chart_has_market_data,
        chart_is_superseded,
        chart_state,
        chart_status_detail,
        theme,
    } = state;
    let colors = theme.colors;
    let notice = chart_surface_notice(
        chart_state,
        chart_has_market_data,
        chart_is_superseded,
        &chart_status_detail,
    );
    let chart_surface = chart_pane_host(chart)
        .id(("primary_chart", pane_id))
        .bg(gpui_color(colors.surface))
        .children(notice.map(|notice| chart_notice(notice, theme)));
    div().size_full().overflow_hidden().child(chart_surface)
}

pub(super) fn chart_pane_host(chart: Option<&Entity<AerisChartView>>) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .size_full()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .children(chart.cloned())
}

pub(super) fn chart_notice(
    notice: ChartSurfaceNotice,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    if notice.label == ChartState::Loading.label() {
        if notice.placement != ChartNoticePlacement::Center {
            // A repair behind the chart the trader is still reading is announced by
            // the symbol legend's own spinner, beside the symbol it belongs to. A
            // second one in the corner lands on top of that legend.
            return div().into_any_element();
        }
        let spinner = Loader::from_path("chart_notice_loader", HugeIcon::Loader.path())
            .with_size(px(40.0))
            .color(gpui_color(colors.icon));
        let overlay = div()
            .id("chart_loading_status")
            .absolute()
            .occlude()
            .role(Role::Status)
            .aria_label(notice.label)
            // A loading surface is deliberately opaque. On first launch there
            // is no chart to read, and during a switch the retained chart belongs
            // to the previous selection.
            .bg(gpui_color(colors.surface))
            .gap_2()
            .child(spinner)
            .child(
                div()
                    .text_sm()
                    .text_color(gpui_color(colors.text_primary))
                    .child(notice.label),
            )
            .children(notice.detail.clone().map(|detail| {
                div()
                    .text_xs()
                    .text_color(gpui_color(colors.text_secondary))
                    .child(detail)
            }));
        return overlay
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .into_any_element();
    }
    let tone = match notice.tone {
        ChartNoticeTone::Muted => colors.text_secondary,
        ChartNoticeTone::Warning => colors.warning,
        ChartNoticeTone::Loss => colors.danger,
    };
    let label = div()
        .flex()
        .flex_col()
        .gap_1()
        .px_2()
        .py_1()
        .border_1()
        .rounded(px(f32::from(
            chart_chrome::CHART_SURFACE_RADIUS.logical_pixels(),
        )))
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface.with_alpha(0.94)))
        .text_xs()
        .text_color(gpui_color(tone))
        .child(notice.label)
        .children(notice.detail.map(|detail| {
            div()
                .text_color(gpui_color(colors.text_secondary))
                .child(detail)
        }));
    match notice.placement {
        ChartNoticePlacement::Center => div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(label)
            .into_any_element(),
        ChartNoticePlacement::BottomRight => div()
            .absolute()
            .right_2()
            .bottom_2()
            .child(label)
            .into_any_element(),
    }
}
