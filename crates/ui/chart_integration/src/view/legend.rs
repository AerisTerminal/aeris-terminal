//! The chart legend: one row per series or study, with its visibility, settings and remove controls.

use super::{
    AerisChartView, ChartHostEvent, ChartSettingsRequest, OrderFlowStudy, gpui_theme_color,
};
use aeris_charts_render::color::Color;
use aeris_design_system::{AerisTheme, platform_typography};
use gpui::{
    Animation, AnimationExt, AnyElement, AnyView, CursorStyle, Entity, MouseButton, Rgba, Role,
    SharedString, Transformation, div, percentage, prelude::*, px, rgba, svg,
};
use std::sync::Arc;
use std::time::Duration;

pub(super) const LEGEND_INSET: f32 = 8.0;
pub(super) const LEGEND_ROW_HEIGHT: f32 = 24.0;
pub(super) const LEGEND_MAX_WIDTH: f32 = 640.0;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum LegendItem {
    Asset,
    Volume,
    VolumeProfile,
    Indicator(u32),
    Study { study_id: u64, series_id: u32 },
    OrderFlow(OrderFlowStudy),
    BigTrades,
    OrderBookDepth,
    DepthHeatmap,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum LegendPresence {
    #[default]
    Absent,
    Present,
}

impl LegendPresence {
    pub(super) const fn is_present(self) -> bool {
        matches!(self, Self::Present)
    }
}

impl LegendItem {
    pub(super) fn key(self) -> u64 {
        match self {
            Self::Asset => 0,
            Self::Volume => 1,
            Self::Indicator(binding) => u64::from(binding) + 2,
            Self::Study { series_id, .. } => (1_u64 << 63) | u64::from(series_id),
            Self::OrderFlow(study) => (1_u64 << 62) | study as u64,
            Self::VolumeProfile => 1_u64 << 61,
            Self::BigTrades => 1_u64 << 60,
            Self::OrderBookDepth => 1_u64 << 59,
            Self::DepthHeatmap => 1_u64 << 58,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LegendRow {
    pub(super) item: LegendItem,
    pub(super) pane: usize,
    pub(super) title: String,
    /// One entry per readout (`O 77,876.69`, `%K 36.95`, …) so a legend that outgrows its pane
    /// wraps value by value instead of spilling over the price axis.
    pub(super) values: Vec<LegendValue>,
    pub(super) visible: bool,
    pub(super) settings_available: bool,
}

/// A readout without its own series color reads neutral. The symbol's OHLC never takes the bar
/// direction: flipping the values between the up and down colors reads as the text jumping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LegendValue {
    pub(super) text: String,
    pub(super) color: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct LegendPaneLayout {
    pub(super) left: f32,
    pub(super) top: f32,
    pub(super) width: f32,
    pub(super) height: f32,
}

#[derive(Clone, Copy)]
pub(super) struct LegendPalette {
    pub(super) text: Rgba,
    pub(super) muted: Rgba,
    pub(super) hover: Rgba,
    pub(super) danger: Rgba,
}

const LEGEND_VIEW_ICON: &str = "aeris/icons/ui/view.svg";
const LEGEND_VIEW_OFF_ICON: &str = "aeris/icons/ui/view-off.svg";
const LEGEND_SETTINGS_ICON: &str = "aeris/icons/ui/settings.svg";
const LEGEND_REMOVE_ICON: &str = "aeris/icons/ui/close.svg";
const LEGEND_LOADING_ICON: &str = "aeris/icons/ui/loader.svg";
/// One rotation of the legend's loading glyph.
const LEGEND_LOADING_PERIOD: Duration = Duration::from_millis(700);

fn platform_tabular_numerals() -> gpui::FontFeatures {
    gpui::FontFeatures(Arc::new(vec![(
        platform_typography().tabular_numerals_feature().to_owned(),
        1,
    )]))
}

pub(super) fn legend_palette(theme: &AerisTheme) -> LegendPalette {
    let colors = theme.colors;
    LegendPalette {
        text: gpui_theme_color(colors.text_default),
        muted: gpui_theme_color(colors.text_secondary),
        hover: gpui_theme_color(colors.hover_bg),
        danger: gpui_theme_color(colors.danger),
    }
}

pub(super) fn chart_legend_layers(
    chart: &Entity<AerisChartView>,
    rows: &[LegendRow],
    panes: &[LegendPaneLayout],
    theme: &AerisTheme,
    loading: bool,
    accessory: Option<&AnyView>,
) -> Vec<AnyElement> {
    let palette = legend_palette(theme);
    panes
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(pane_index, pane)| {
            let pane_rows = rows.iter().filter(|row| row.pane == pane_index);
            let mut accessory = accessory.filter(|_| pane_index == 0).cloned();
            let follows_asset = rows
                .iter()
                .any(|row| row.pane == pane_index && row.item == LegendItem::Asset);
            // Cap the legend at the plot area so it wraps inside the pane instead of running
            // under the price axis.
            let available_width = (pane.width - LEGEND_INSET * 2.0).clamp(0.0, LEGEND_MAX_WIDTH);
            let mut layer = div()
                .id(("chart_legend_pane", pane_index))
                .absolute()
                .left(px(pane.left + LEGEND_INSET))
                .top(px(pane.top + LEGEND_INSET))
                .max_w(px(available_width))
                .max_h(px((pane.height - LEGEND_INSET * 2.0).max(0.0)))
                .overflow_hidden()
                .flex()
                .flex_col()
                .items_start()
                .cursor(CursorStyle::Arrow);
            let mut row_count = 0;
            if !follows_asset && let Some(accessory) = accessory.take() {
                row_count += 1;
                layer = layer.child(legend_accessory(chart, accessory));
            }
            for row in pane_rows {
                row_count += 1;
                layer = layer.child(chart_legend_row(chart, row, palette, loading));
                if row.item == LegendItem::Asset
                    && let Some(accessory) = accessory.take()
                {
                    layer = layer.child(legend_accessory(chart, accessory));
                }
            }
            (row_count > 0).then(|| layer.into_any_element())
        })
        .collect()
}

/// Host content under the symbol row. Presses stop here so a click on a host control never
/// starts a chart gesture or moves keyboard focus to the chart; like any press on the chart,
/// it still asks the host to activate the chart's pane.
fn legend_accessory(chart: &Entity<AerisChartView>, accessory: AnyView) -> AnyElement {
    let activate = chart.clone();
    div()
        .id("chart_legend_accessory")
        .flex_none()
        .px_1()
        .py_1()
        .on_any_mouse_down(move |_, _, cx| {
            activate.update(cx, |_, chart_cx| chart_cx.emit(ChartHostEvent::Activate));
            cx.stop_propagation();
        })
        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(accessory)
        .into_any_element()
}

fn chart_legend_row(
    chart: &Entity<AerisChartView>,
    row: &LegendRow,
    palette: LegendPalette,
    loading: bool,
) -> AnyElement {
    let group: SharedString = format!("chart-legend-row-{}", row.item.key()).into();
    let controls = legend_row_controls(chart, row, palette, &group);
    let values = if row.visible {
        row.values.clone()
    } else {
        Vec::new()
    };
    let values = values.into_iter().map(|value| {
        let color = value
            .color
            .as_deref()
            .and_then(Color::parse_css)
            .map_or(palette.muted, |color| rgba(color.0));
        // A wrapping text element shapes at whatever width the row's flex-wrap pass offers it,
        // so a labelled value such as "C 6812.25" could break at its space and jump inside the
        // fixed-height cell as its digits change. Each value moves to a new row line as a whole.
        div()
            .flex_none()
            .h(px(LEGEND_ROW_HEIGHT))
            .flex()
            .items_center()
            .whitespace_nowrap()
            .font_features(platform_tabular_numerals())
            .text_color(color)
            .child(value.text)
            .into_any_element()
    });
    div()
        .id(("chart_legend_row", row.item.key()))
        .group(group.clone())
        .min_h(px(LEGEND_ROW_HEIGHT))
        .max_w_full()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x_2()
        .px_1()
        .rounded(px(f32::from(
            aeris_design_system::RadiusToken::Sm.logical_pixels(),
        )))
        .text_xs()
        .text_color(if row.visible {
            palette.text
        } else {
            palette.muted
        })
        .cursor(CursorStyle::Arrow)
        .hover(|style| style.bg(palette.hover))
        .children((loading && row.item == LegendItem::Asset).then(|| legend_loading_glyph(palette)))
        .child(
            div()
                .flex_none()
                .h(px(LEGEND_ROW_HEIGHT))
                .flex()
                .items_center()
                .whitespace_nowrap()
                // The asset title (symbol, venue and interval) leads the legend one step up the
                // type scale, at the surface's regular weight like every other legend row.
                .when(row.item == LegendItem::Asset, gpui::Styled::text_sm)
                .child(row.title.clone()),
        )
        .children(values)
        .child(controls)
        .into_any_element()
}

fn legend_row_controls(
    chart: &Entity<AerisChartView>,
    row: &LegendRow,
    palette: LegendPalette,
    group: &SharedString,
) -> impl IntoElement {
    let visibility = legend_control(
        chart,
        row.item,
        LegendControl::Visibility(row.visible),
        palette,
    )
    .when(row.visible, |control| {
        control
            .invisible()
            .group_hover(group.clone(), gpui::Styled::visible)
    });
    let mut controls = div()
        .flex_none()
        .h(px(LEGEND_ROW_HEIGHT))
        .flex()
        .items_center()
        .gap_1()
        .child(visibility);
    if row.settings_available
        && matches!(
            row.item,
            LegendItem::Study { .. }
                | LegendItem::BigTrades
                | LegendItem::Volume
                | LegendItem::OrderFlow(_)
                | LegendItem::OrderBookDepth
        )
    {
        controls = controls.child(
            legend_control(chart, row.item, LegendControl::Settings, palette)
                .invisible()
                .group_hover(group.clone(), gpui::Styled::visible),
        );
    }
    if row.item != LegendItem::Asset {
        controls = controls.child(
            legend_control(chart, row.item, LegendControl::Remove, palette)
                .invisible()
                .group_hover(group.clone(), gpui::Styled::visible),
        );
    }
    controls
}

/// The symbol row's own load indicator, sized to the legend text and placed ahead
/// of the title: a load in flight belongs to the symbol the trader is reading, not
/// to a second notice sitting over the plot.
fn legend_loading_glyph(palette: LegendPalette) -> AnyElement {
    div()
        .id("chart_legend_loading")
        .flex_none()
        .h(px(LEGEND_ROW_HEIGHT))
        .flex()
        .items_center()
        .role(Role::Status)
        .aria_label("Loading")
        .child(
            svg()
                .path(LEGEND_LOADING_ICON)
                .size(px(11.0))
                .flex_none()
                .text_color(palette.muted)
                .with_animation(
                    "chart_legend_loading",
                    Animation::new(LEGEND_LOADING_PERIOD).repeat(),
                    |glyph, delta| {
                        glyph.with_transformation(Transformation::rotate(percentage(delta)))
                    },
                ),
        )
        .into_any_element()
}

#[derive(Clone, Copy)]
pub(super) enum LegendControl {
    Visibility(bool),
    Settings,
    Remove,
}

fn legend_control(
    chart: &Entity<AerisChartView>,
    item: LegendItem,
    control: LegendControl,
    palette: LegendPalette,
) -> gpui::Stateful<gpui::Div> {
    let action_chart = chart.clone();
    let remove = matches!(control, LegendControl::Remove);
    let (label, path, color) = match control {
        LegendControl::Visibility(true) => ("Hide", LEGEND_VIEW_ICON, palette.text),
        LegendControl::Visibility(false) => ("Show", LEGEND_VIEW_OFF_ICON, palette.muted),
        LegendControl::Settings => ("Settings", LEGEND_SETTINGS_ICON, palette.text),
        LegendControl::Remove => ("Remove", LEGEND_REMOVE_ICON, palette.danger),
    };
    div()
        .id(legend_control_element_id(item, control))
        .size(px(20.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .text_color(color)
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(label)
        .hover(|style| style.bg(palette.hover))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| {
            action_chart.update(cx, |chart, chart_cx| {
                if matches!(control, LegendControl::Settings) {
                    let request = match item {
                        LegendItem::Study { study_id, .. } => {
                            Some(ChartSettingsRequest::Study(study_id))
                        }
                        LegendItem::BigTrades => Some(ChartSettingsRequest::BigTrades),
                        LegendItem::Volume => Some(ChartSettingsRequest::Volume),
                        LegendItem::OrderFlow(OrderFlowStudy::CumulativeDelta) => {
                            Some(ChartSettingsRequest::CumulativeDelta)
                        }
                        LegendItem::OrderFlow(OrderFlowStudy::Delta) => {
                            Some(ChartSettingsRequest::DeltaHistogram)
                        }
                        LegendItem::OrderBookDepth => Some(ChartSettingsRequest::OrderBookDepth),
                        _ => None,
                    };
                    if let Some(request) = request {
                        chart_cx.emit(ChartHostEvent::Settings(request));
                    }
                    return;
                }
                if remove && let LegendItem::Study { study_id, .. } = item {
                    chart_cx.emit(ChartHostEvent::RemoveStudy(study_id));
                    return;
                }
                let changed = if remove {
                    chart.remove_legend_indicator(item)
                } else {
                    let LegendControl::Visibility(visible) = control else {
                        return;
                    };
                    chart.set_legend_item_visible(item, !visible)
                };
                if changed {
                    chart_cx.notify();
                }
            });
            cx.stop_propagation();
        })
        .child(svg().path(path).size(px(14.0)).text_color(color))
}

pub(super) fn legend_control_element_id(
    item: LegendItem,
    control: LegendControl,
) -> (&'static str, u64) {
    let namespace = match control {
        LegendControl::Visibility(_) => "chart_legend_visibility_control",
        LegendControl::Settings => "chart_legend_settings_control",
        LegendControl::Remove => "chart_legend_remove_control",
    };
    (namespace, item.key())
}
