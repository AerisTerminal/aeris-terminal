//! The chart legend: one row per series or study, with its visibility, settings and remove controls.

use super::{
    AerisChartView, ChartHostEvent, ChartSettingsRequest, OrderFlowStudy, SessionProfileKind,
    gpui_theme_color,
};
use aeris_charts_render::color::Color;
use aeris_design_system::{AerisTheme, platform_numeric_font_family, platform_typography};
use aeris_localization::{chart_ui, tr};
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
    OpenInterest,
    Liquidations,
    SessionProfile(SessionProfileKind),
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
            Self::OpenInterest => 1_u64 << 56,
            Self::Liquidations => 1_u64 << 55,
            Self::SessionProfile(kind) => (1_u64 << 57) | kind as u64,
        }
    }
}

/// The legend rows the options let through, and the price pane's collapse toggle.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct LegendPresentation {
    pub(super) rows: Vec<LegendRow>,
    /// Shown while the price pane has indicator rows to fold away or bring back.
    pub(super) indicator_toggle: Option<IndicatorLegendToggle>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IndicatorLegendToggle {
    Expanded,
    Collapsed,
}

/// Host views the legend positions; the host owns their state and behavior.
#[derive(Clone, Copy, Default)]
pub(super) struct LegendHostContent<'a> {
    /// Under the symbol row of the price pane.
    pub(super) accessory: Option<&'a AnyView>,
    /// Ahead of the symbol row's title.
    pub(super) symbol_mark: Option<&'a AnyView>,
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
    pub(super) surface: Rgba,
    pub(super) border: Rgba,
    pub(super) border_width: f32,
}

const LEGEND_VIEW_ICON: &str = "aeris/icons/ui/view.svg";
const LEGEND_VIEW_OFF_ICON: &str = "aeris/icons/ui/view-off.svg";
const LEGEND_SETTINGS_ICON: &str = "aeris/icons/ui/settings.svg";
const LEGEND_REMOVE_ICON: &str = "aeris/icons/ui/close.svg";
const LEGEND_LOADING_ICON: &str = "aeris/icons/ui/loader.svg";
const LEGEND_COLLAPSE_DOWN_ICON: &str = "aeris/icons/ui/chevron-down.svg";
const LEGEND_COLLAPSE_UP_ICON: &str = "aeris/icons/ui/chevron-up.svg";
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
        surface: gpui_theme_color(colors.surface),
        border: gpui_theme_color(colors.border_secondary),
        border_width: theme.dimensions.border_width,
    }
}

impl AerisChartView {
    /// Toggles `item` as the click-activated legend row so its action controls stay steadily
    /// visible and clickable even while crosshair values change.
    pub(super) fn toggle_active_legend_item(&mut self, item: LegendItem) {
        if self.active_legend_item == Some(item) {
            self.active_legend_item = None;
        } else {
            self.active_legend_item = Some(item);
        }
    }

    pub(super) fn clear_active_legend_item(&mut self) -> bool {
        self.active_legend_item.take().is_some()
    }
}

pub(super) fn chart_legend_layers(
    chart: &Entity<AerisChartView>,
    presentation: &LegendPresentation,
    panes: &[LegendPaneLayout],
    active_item: Option<LegendItem>,
    theme: &AerisTheme,
    loading: bool,
    host: LegendHostContent<'_>,
) -> Vec<AnyElement> {
    let palette = legend_palette(theme);
    let rows = &presentation.rows;
    panes
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(pane_index, pane)| {
            let pane_rows = rows.iter().filter(|row| row.pane == pane_index);
            let mut accessory = host.accessory.filter(|_| pane_index == 0).cloned();
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
                layer = layer.child(chart_legend_row(
                    chart,
                    row,
                    active_item == Some(row.item),
                    palette,
                    loading,
                    host.symbol_mark,
                ));
                if row.item == LegendItem::Asset
                    && let Some(accessory) = accessory.take()
                {
                    layer = layer.child(legend_accessory(chart, accessory));
                }
            }
            if let Some(toggle) = presentation.indicator_toggle.filter(|_| pane_index == 0) {
                row_count += 1;
                layer = layer.child(indicator_legend_toggle(chart, toggle, palette));
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

/// The price pane's fold control under its last legend row: the chevron points up to fold the
/// indicator rows away and down to bring them back. Folding never hides an indicator.
fn indicator_legend_toggle(
    chart: &Entity<AerisChartView>,
    toggle: IndicatorLegendToggle,
    palette: LegendPalette,
) -> AnyElement {
    let action_chart = chart.clone();
    let collapsed = toggle == IndicatorLegendToggle::Collapsed;
    let label = if collapsed {
        tr!(chart_ui::LEGEND_EXPAND_INDICATORS)
    } else {
        tr!(chart_ui::LEGEND_COLLAPSE_INDICATORS)
    };
    let glyph = svg()
        .path(if collapsed {
            LEGEND_COLLAPSE_DOWN_ICON
        } else {
            LEGEND_COLLAPSE_UP_ICON
        })
        .size(px(12.0))
        .flex_none()
        .text_color(palette.text);
    div()
        .id("chart_legend_indicator_toggle")
        .flex_none()
        .mx_1()
        .my_1()
        .w(px(24.0))
        .h(px(18.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(
            aeris_design_system::RadiusToken::Sm.logical_pixels(),
        )))
        .border(px(palette.border_width))
        .border_color(palette.border)
        .bg(palette.surface)
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(label)
        .hover(|style| style.bg(palette.hover))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| {
            action_chart.update(cx, |chart, chart_cx| {
                if chart.toggle_indicator_legend_collapsed() {
                    chart_cx.notify();
                }
            });
            cx.stop_propagation();
        })
        .child(glyph)
        .into_any_element()
}

fn chart_legend_row(
    chart: &Entity<AerisChartView>,
    row: &LegendRow,
    active: bool,
    palette: LegendPalette,
    loading: bool,
    symbol_mark: Option<&AnyView>,
) -> AnyElement {
    let group: SharedString = format!("chart-legend-row-{}", row.item.key()).into();
    let controls = legend_row_controls(chart, row, active, palette, &group);
    let activate_chart = chart.clone();
    let click_chart = chart.clone();
    let row_item = row.item;
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
            .font_family(platform_numeric_font_family())
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
        .border(px(palette.border_width))
        .border_color(if active {
            gpui::Hsla::from(palette.border)
        } else {
            gpui::transparent_black()
        })
        .when(active, |style| style.bg(palette.surface))
        .text_xs()
        .text_color(if row.visible {
            palette.text
        } else {
            palette.muted
        })
        .cursor_pointer()
        .hover(move |style| {
            style.bg(if active {
                palette.surface
            } else {
                palette.hover
            })
        })
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            activate_chart.update(cx, |_, chart_cx| chart_cx.emit(ChartHostEvent::Activate));
            cx.stop_propagation();
        })
        .on_click(move |_, _, cx| {
            click_chart.update(cx, |chart, chart_cx| {
                chart.toggle_active_legend_item(row_item);
                chart_cx.notify();
            });
            cx.stop_propagation();
        })
        .children((loading && row.item == LegendItem::Asset).then(|| legend_loading_glyph(palette)))
        .children(
            symbol_mark
                .filter(|_| row.item == LegendItem::Asset)
                .map(|mark| {
                    div()
                        .flex_none()
                        .h(px(LEGEND_ROW_HEIGHT))
                        .flex()
                        .items_center()
                        .child(mark.clone())
                }),
        )
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

impl LegendItem {
    pub(super) const fn settings_request(self) -> Option<ChartSettingsRequest> {
        match self {
            Self::Study { study_id, .. } => Some(ChartSettingsRequest::Study(study_id)),
            Self::BigTrades => Some(ChartSettingsRequest::BigTrades),
            Self::Volume => Some(ChartSettingsRequest::Volume),
            Self::OrderFlow(OrderFlowStudy::CumulativeDelta) => {
                Some(ChartSettingsRequest::CumulativeDelta)
            }
            Self::OrderFlow(OrderFlowStudy::Delta) => Some(ChartSettingsRequest::DeltaHistogram),
            Self::OrderBookDepth => Some(ChartSettingsRequest::OrderBookDepth),
            Self::OpenInterest => Some(ChartSettingsRequest::OpenInterest),
            Self::Liquidations => Some(ChartSettingsRequest::Liquidations),
            Self::SessionProfile(kind) => Some(ChartSettingsRequest::SessionProfile(kind)),
            Self::Asset | Self::VolumeProfile | Self::DepthHeatmap | Self::Indicator(_) => None,
        }
    }
}

fn legend_row_controls(
    chart: &Entity<AerisChartView>,
    row: &LegendRow,
    active: bool,
    palette: LegendPalette,
    group: &SharedString,
) -> impl IntoElement {
    let visibility = legend_control(
        chart,
        row.item,
        LegendControl::Visibility(row.visible),
        palette,
    )
    .when(!active && row.visible, |control| {
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
    if row.settings_available && row.item.settings_request().is_some() {
        controls = controls.child(
            legend_control(chart, row.item, LegendControl::Settings, palette).when(
                !active,
                |control| {
                    control
                        .invisible()
                        .group_hover(group.clone(), gpui::Styled::visible)
                },
            ),
        );
    }
    if row.item != LegendItem::Asset {
        controls = controls.child(
            legend_control(chart, row.item, LegendControl::Remove, palette).when(
                !active,
                |control| {
                    control
                        .invisible()
                        .group_hover(group.clone(), gpui::Styled::visible)
                },
            ),
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
        .aria_label(tr!(chart_ui::LEGEND_LOADING))
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
        LegendControl::Visibility(true) => {
            (tr!(chart_ui::LEGEND_HIDE), LEGEND_VIEW_ICON, palette.text)
        }
        LegendControl::Visibility(false) => (
            tr!(chart_ui::LEGEND_SHOW),
            LEGEND_VIEW_OFF_ICON,
            palette.muted,
        ),
        LegendControl::Settings => (
            tr!(chart_ui::LEGEND_SETTINGS),
            LEGEND_SETTINGS_ICON,
            palette.text,
        ),
        LegendControl::Remove => (
            tr!(chart_ui::LEGEND_REMOVE),
            LEGEND_REMOVE_ICON,
            palette.danger,
        ),
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
                    chart.clear_active_legend_item();
                    if let Some(request) = item.settings_request() {
                        chart_cx.emit(ChartHostEvent::Settings(request));
                    }
                    chart_cx.notify();
                    return;
                }
                if remove && let LegendItem::Study { study_id, .. } = item {
                    if chart.active_legend_item == Some(item) {
                        chart.clear_active_legend_item();
                    }
                    chart_cx.emit(ChartHostEvent::RemoveStudy(study_id));
                    chart_cx.notify();
                    return;
                }
                let changed = if remove {
                    if chart.active_legend_item == Some(item) {
                        chart.clear_active_legend_item();
                    }
                    let removed = chart.remove_legend_indicator(item);
                    if removed
                        && matches!(
                            item,
                            LegendItem::OrderFlow(_)
                                | LegendItem::BigTrades
                                | LegendItem::OrderBookDepth
                                | LegendItem::DepthHeatmap
                                | LegendItem::OpenInterest
                                | LegendItem::Liquidations
                        )
                    {
                        chart_cx.emit(ChartHostEvent::OrderFlowSettingsChanged);
                    }
                    removed
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
