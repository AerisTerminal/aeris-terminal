//! Chart pane host and chart-surface notices.

use super::assets::ExchangeLogo;
use super::terminal_chrome::exchange_mark;
use super::toaster::chart_status_card;
use super::{
    AerisChartView, AerisTheme, AnyElement, ChartNoticePlacement, ChartState, ChartSurfaceNotice,
    Context, Div, Entity, HugeIcon, InteractiveElement, IntoElement, Loader, ParentElement, Render,
    Role, StatefulInteractiveElement, Styled, WeakEntity, Window, WorkspaceSurface, div,
    gpui_color, px,
};
use gpui::{AnyView, AppContext};

/// Matches the exchange logo the watchlist draws beside each symbol.
const LEGEND_SYMBOL_MARK_SIZE: f32 = 16.0;

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
                .text_color(gpui_color(colors.text_default))
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
            .color(gpui_color(theme.colors.text_secondary));
        return opaque_status_overlay("chart_loading_status", notice, Some(spinner), theme)
            .into_any_element();
    }
    if notice.label == ChartState::AwaitingData.label() {
        // Settled, not in progress: the same opaque surface as loading, without
        // a spinner that would suggest the wait is ours.
        return opaque_status_overlay("chart_awaiting_data_status", notice, None, theme)
            .into_any_element();
    }
    if notice.label == ChartState::HistoryUnavailable.label() {
        // Live trades have not arrived yet, so there is no chart to read; a
        // corner toast carries the notice once live bars exist.
        return opaque_status_overlay("chart_history_unavailable_status", notice, None, theme)
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

/// The exchange logo the chart shows ahead of its symbol legend title, drawn from the
/// surface's current instrument each frame so a symbol change on another provider follows.
pub(super) struct ChartLegendSymbolMark {
    surface: WeakEntity<WorkspaceSurface>,
}

/// Puts the surface's exchange logo ahead of the symbol legend of a chart the surface now
/// shows; the chart's legend options decide whether it is drawn.
pub(super) fn attach_chart_legend_symbol_mark(
    chart: &Entity<AerisChartView>,
    cx: &mut Context<WorkspaceSurface>,
) {
    let surface = cx.weak_entity();
    let mark = AnyView::from(cx.new(|_| ChartLegendSymbolMark { surface }));
    chart.update(cx, |chart, _| chart.set_legend_symbol_mark(Some(mark)));
}

impl Render for ChartLegendSymbolMark {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(surface) = self.surface.upgrade() else {
            return div().into_any_element();
        };
        let state = surface.read(cx);
        let logo = state
            .product
            .as_ref()
            .and_then(|_| super::provider_presentation(state.provider))
            .and_then(|descriptor| ExchangeLogo::for_logo_key(descriptor.logo_key));
        match logo {
            Some(logo) => exchange_mark(
                logo,
                px(LEGEND_SYMBOL_MARK_SIZE),
                false,
                &state.theme.colors,
            )
            .into_any_element(),
            None => div().into_any_element(),
        }
    }
}
