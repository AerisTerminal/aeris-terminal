//! Chart pane host and chart-surface notices.

use super::toaster::chart_status_card;
use super::{
    AerisChartView, AerisTheme, AnyElement, ChartNoticePlacement, ChartState, ChartSurfaceNotice,
    Div, Entity, HugeIcon, InteractiveElement, IntoElement, Loader, ParentElement, Role,
    StatefulInteractiveElement, Styled, div, gpui_color, px,
};

pub(super) struct MarketWorkspaceState<'a> {
    pub(super) pane_id: u64,
    pub(super) chart: Option<&'a Entity<AerisChartView>>,
    /// The chart's notice when it covers or centres on the surface; corner conditions are
    /// toasts in `toasts`.
    pub(super) notice: Option<ChartSurfaceNotice>,
    /// The pane's toast stack, top right.
    pub(super) toasts: Option<AnyElement>,
    pub(super) theme: &'a AerisTheme,
}

pub(super) fn market_workspace(state: MarketWorkspaceState<'_>) -> impl IntoElement + use<> {
    let MarketWorkspaceState {
        pane_id,
        chart,
        notice,
        toasts,
        theme,
    } = state;
    let chart_surface = chart_pane_host(chart)
        .id(("primary_chart", pane_id))
        .bg(gpui_color(theme.colors.surface))
        .children(
            notice
                .filter(|notice| notice.placement == ChartNoticePlacement::Center)
                .map(|notice| chart_notice(notice, theme)),
        )
        .children(toasts);
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

fn opaque_status_overlay(
    id: &'static str,
    notice: ChartSurfaceNotice,
    leading: Option<Loader>,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    div()
        .id(id)
        .absolute()
        .occlude()
        .role(Role::Status)
        .aria_label(notice.label)
        // A centered status surface is deliberately opaque. On first launch
        // there is no chart to read, and during a switch the retained chart
        // belongs to the previous selection.
        .bg(gpui_color(colors.surface))
        .gap_2()
        .children(leading)
        .child(
            div()
                .text_sm()
                .text_color(gpui_color(colors.text_primary))
                .child(notice.label),
        )
        .children(notice.detail.map(|detail| {
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_secondary))
                .child(detail)
        }))
        .inset_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
}

/// A notice centred on a chart with no data of its own to read.
fn chart_notice(notice: ChartSurfaceNotice, theme: &AerisTheme) -> AnyElement {
    if notice.label == ChartState::Loading.label() {
        let spinner = Loader::from_path("chart_notice_loader", HugeIcon::Loader.path())
            .with_size(px(40.0))
            .color(gpui_color(theme.colors.icon));
        return opaque_status_overlay("chart_loading_status", notice, Some(spinner), theme)
            .into_any_element();
    }
    if notice.label == ChartState::AwaitingData.label() {
        // Settled, not in progress: the same opaque surface as loading, without
        // a spinner that would suggest the wait is ours.
        return opaque_status_overlay("chart_awaiting_data_status", notice, None, theme)
            .into_any_element();
    }
    let card = chart_status_card(notice, theme);
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(card)
        .into_any_element()
}
