//! Shared tab components. [`TabList`] + [`Tab`] are the Theme System `.ui-tabs` / `.ui-tab`
//! underlined tabs for navigation and one-tap presets; [`TabList::track`] with [`Tab::track`] is
//! the `.ui-tabs-normal` / `.ui-tab-normal` pill-track switcher; [`TabList::sidebar`] with
//! [`Tab::sidebar`] is the vertical section navigation of settings panels. Callers add only
//! layout and spacing, never colors, borders or radii.

use aeris_design_system::{
    AerisTheme, RadiusToken, ThemeColor, TypographyRole, platform_font_family,
};
use gpui::{
    AnyElement, App, Bounds, Div, ElementId, InteractiveElement, Interactivity, IntoElement,
    Orientation, ParentElement, PathBuilder, Pixels, RenderOnce, SharedString, Stateful,
    StyleRefinement, Styled, Window, canvas, div, point, prelude::*, px,
};
use gpui_base::Button as BaseButton;

use super::{
    platform_font_weight,
    rem_scale::design_rems,
    theme::{gpui_color, platform_border_width},
};

/// Theme System tab geometry: the list is `h-7 gap-4` over a `--border` baseline and each
/// tab is `h-7 text-xs` with a 2px underline that turns `--primary` while selected.
const TAB_LIST_HEIGHT: Pixels = px(28.0);
const TAB_GAP: Pixels = px(16.0);
const TAB_UNDERLINE_WIDTH: Pixels = px(2.0);
/// Compact tabs for dense panel headers: a shorter list with tighter spacing.
const COMPACT_TAB_LIST_HEIGHT: Pixels = px(22.0);
const COMPACT_TAB_GAP: Pixels = px(12.0);
/// Normal (track) tabs (`.ui-tabs-normal` / `.ui-tab-normal`): 2px inset track with 24px pill tabs.
const TRACK_PADDING: Pixels = px(2.0);
const TRACK_TAB_GAP: Pixels = px(2.0);
const TRACK_TAB_HEIGHT: Pixels = px(24.0);
const TRACK_TAB_PADDING_X: Pixels = px(10.0);
const TRACK_TAB_LINE_HEIGHT: Pixels = px(14.0);
const TRACK_SURFACE_ALPHA: f32 = 0.6;
const TRACK_BORDER_ALPHA: f32 = 0.7;
const TRACK_SELECTED_BORDER_ALPHA: f32 = 0.7;
/// Sidebar sections are spaced like the former segmented track inset.
const SIDEBAR_TAB_GAP: Pixels = px(2.0);
/// Sidebar sections are a comfortable 36 px click target that grows with a scaled panel.
const SIDEBAR_TAB_HEIGHT: f32 = 36.0;
/// Attached browser-style chrome tab geometry (sits flush to the bottom edge of the 42px title bar).
const CHROME_TAB_ACTIVE_HEIGHT: f32 = 37.0;
const CHROME_TAB_ACTIVE_PADDING_BOTTOM: f32 = 3.0;
const CHROME_TAB_INACTIVE_HEIGHT: f32 = 32.0;
const CHROME_TAB_INACTIVE_BOTTOM_MARGIN: f32 = 4.0;
const CHROME_TAB_TOP_RADIUS: f32 = 10.0;
const CHROME_TAB_FLARE_RADIUS: f32 = 9.0;
const CHROME_TAB_INACTIVE_RADIUS: f32 = 8.0;
/// Cubic Bezier handle length for a true quarter-circle arc: `4/3 * (sqrt(2) - 1)`.
const QUARTER_CIRCLE_KAPPA: f32 = 0.552_284_75;

fn tab_radius() -> Pixels {
    px(f32::from(RadiusToken::Full.logical_pixels()))
}

fn sidebar_tab_radius() -> Pixels {
    px(f32::from(RadiusToken::Default.logical_pixels()))
}

/// The `.ui-tabs` list: [`Tab`]s side by side over a 1px `border` baseline.
#[derive(IntoElement)]
pub(crate) struct TabList {
    base: Stateful<Div>,
}

impl TabList {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        theme: &AerisTheme,
    ) -> Self {
        Self {
            base: div()
                .id(id)
                .role(gpui::Role::TabList)
                .aria_label(label)
                .flex()
                .flex_none()
                .items_end()
                .h(TAB_LIST_HEIGHT)
                .gap(TAB_GAP)
                .border_b(platform_border_width(theme))
                .border_color(gpui_color(theme.colors.border)),
        }
    }

    /// The `.ui-tabs-normal` pill track: 60%-alpha `surface` fill, 70%-alpha `border` edge,
    /// `2px` padding and `2px` gap. Pair it with [`Tab::track`] tabs.
    pub(crate) fn track(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        theme: &AerisTheme,
    ) -> Self {
        Self {
            base: div()
                .id(id)
                .role(gpui::Role::TabList)
                .aria_label(label)
                .flex()
                .flex_none()
                .items_center()
                .gap(TRACK_TAB_GAP)
                .p(TRACK_PADDING)
                .border(platform_border_width(theme))
                .border_color(gpui_color(
                    theme.colors.border.with_alpha(TRACK_BORDER_ALPHA),
                ))
                .rounded(tab_radius())
                .bg(gpui_color(
                    theme.colors.surface.with_alpha(TRACK_SURFACE_ALPHA),
                )),
        }
    }

    /// A vertical, trackless list of [`Tab::sidebar`] sections.
    pub(crate) fn sidebar(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            base: div()
                .id(id)
                .role(gpui::Role::TabList)
                .aria_label(label)
                .aria_orientation(Orientation::Vertical)
                .flex()
                .flex_col()
                .gap(SIDEBAR_TAB_GAP),
        }
    }

    /// The short list for dense panel headers; pair it with [`Tab::compact`] tabs.
    pub(crate) fn compact(mut self) -> Self {
        self.base = self.base.h(COMPACT_TAB_LIST_HEIGHT).gap(COMPACT_TAB_GAP);
        self
    }
}

impl Styled for TabList {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl ParentElement for TabList {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl RenderOnce for TabList {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.base
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TabSurface {
    /// Theme System `.ui-tab` inside a [`TabList`].
    Segmented,
    /// Theme System `.ui-tab-normal` pill inside a [`TabList::track`].
    Track,
    /// A workspace document tab that rests on window chrome instead of a track.
    Chrome {
        resting: ThemeColor,
        selected: ThemeColor,
    },
    /// A full-width section in a [`TabList::sidebar`].
    Sidebar,
}

/// Theme System tab colors for one selection state. Unselected tabs keep a transparent
/// underline so selecting one never shifts the layout; hover and press change the text only.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SegmentedTabColors {
    fill: Option<ThemeColor>,
    border: Option<ThemeColor>,
    text: ThemeColor,
    hover_text: ThemeColor,
    active_text: ThemeColor,
}

fn segmented_tab_colors(theme: &AerisTheme, selected: bool) -> SegmentedTabColors {
    let colors = theme.colors;
    if selected {
        SegmentedTabColors {
            fill: None,
            border: Some(colors.primary),
            text: colors.primary,
            hover_text: colors.primary,
            active_text: colors.primary,
        }
    } else {
        SegmentedTabColors {
            fill: None,
            border: None,
            text: colors.text_secondary,
            hover_text: colors.text_default,
            active_text: colors.text_default,
        }
    }
}

/// Theme System `.ui-tab-normal` colors: transparent while resting, `--surface-secondary` with a
/// 70%-alpha `--text-default` border while selected; inactive tabs brighten to `--text-default`
/// on hover and press.
fn track_tab_colors(theme: &AerisTheme, selected: bool) -> SegmentedTabColors {
    let colors = theme.colors;
    if selected {
        SegmentedTabColors {
            fill: Some(colors.surface_secondary),
            border: Some(colors.text_default.with_alpha(TRACK_SELECTED_BORDER_ALPHA)),
            text: colors.text_default,
            hover_text: colors.text_default,
            active_text: colors.text_default,
        }
    } else {
        SegmentedTabColors {
            fill: None,
            border: None,
            text: colors.text_secondary,
            hover_text: colors.text_default,
            active_text: colors.text_default,
        }
    }
}

/// Sidebar sections rest without a fill, take `hover-bg` on hover and stay on `active-bg` while
/// selected.
fn sidebar_tab_colors(theme: &AerisTheme, selected: bool) -> SegmentedTabColors {
    let colors = theme.colors;
    SegmentedTabColors {
        fill: selected.then_some(colors.active_bg),
        border: None,
        text: if selected {
            colors.text_default
        } else {
            colors.text_secondary
        },
        hover_text: colors.text_default,
        active_text: colors.text_default,
    }
}

/// Shared `Aeris` tab. A tab owns its role, selected state, colors, focus treatment and the
/// canonical pill radius; callers own content and activation.
#[derive(IntoElement)]
pub(crate) struct Tab {
    base: BaseButton,
    style: StyleRefinement,
    theme: AerisTheme,
    selected: bool,
    compact: bool,
    surface: TabSurface,
    children: Vec<AnyElement>,
}

impl Tab {
    pub(crate) fn new(id: impl Into<ElementId>, theme: &AerisTheme) -> Self {
        Self {
            base: BaseButton::new(id),
            style: StyleRefinement::default(),
            theme: *theme,
            selected: false,
            compact: false,
            surface: TabSurface::Segmented,
            children: Vec::new(),
        }
    }

    /// A slim segmented tab for a [`TabList::compact`] track in a dense panel header.
    pub(crate) const fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    pub(crate) const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Renders a `.ui-tab-normal` pill tab inside a [`TabList::track`].
    pub(crate) const fn track(mut self) -> Self {
        self.surface = TabSurface::Track;
        self
    }

    /// Renders a section of a [`TabList::sidebar`] on `--surface`.
    pub(crate) const fn sidebar(mut self) -> Self {
        self.surface = TabSurface::Sidebar;
        self
    }

    /// Renders an attached workspace document tab on window chrome: unselected tabs rest flat on
    /// `resting` and the selected tab merges seamlessly into `selected` below with concave
    /// bottom flares. Callers own its width.
    pub(crate) const fn chrome(mut self, resting: ThemeColor, selected: ThemeColor) -> Self {
        self.surface = TabSurface::Chrome { resting, selected };
        self
    }
}

impl Styled for Tab {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for Tab {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl InteractiveElement for Tab {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl gpui::StatefulInteractiveElement for Tab {}

impl RenderOnce for Tab {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let colors = self.theme.colors;
        let selected = self.selected;
        let tab = self
            .base
            .occlude()
            .role(gpui::Role::Tab)
            .aria_selected(selected)
            .font_family(platform_font_family())
            .font_weight(platform_font_weight(TypographyRole::Normal))
            .cursor_pointer();
        let border_width = platform_border_width(&self.theme);
        let mut tab = match self.surface {
            TabSurface::Segmented => {
                let state = segmented_tab_colors(&self.theme, selected);
                let height = if self.compact {
                    COMPACT_TAB_LIST_HEIGHT
                } else {
                    TAB_LIST_HEIGHT
                };
                // The underline covers the list's baseline, as the Theme System's inset
                // baseline shadow sits under the tab's own bottom border.
                tab.flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .h(height)
                    .mb(-border_width)
                    .border_b(TAB_UNDERLINE_WIDTH)
                    .text_xs()
                    .whitespace_nowrap()
                    .border_color(
                        state
                            .border
                            .map_or_else(gpui::transparent_black, gpui_color),
                    )
                    .when_some(state.fill, |tab, fill| tab.bg(gpui_color(fill)))
                    .text_color(gpui_color(state.text))
                    .hover(move |tab| tab.text_color(gpui_color(state.hover_text)))
                    .active(move |tab| tab.text_color(gpui_color(state.active_text)))
                    .focus_visible(move |tab| tab.border_color(gpui_color(colors.ring_primary)))
            }
            TabSurface::Track => render_track_tab(tab, &self.theme, selected, border_width),
            TabSurface::Sidebar => {
                let state = sidebar_tab_colors(&self.theme, selected);
                // The base button centers its content; section labels read from the leading edge.
                tab.w_full()
                    .flex()
                    .items_center()
                    .justify_start()
                    .h(design_rems(SIDEBAR_TAB_HEIGHT))
                    .px_3()
                    .text_sm()
                    .border(border_width)
                    .border_color(gpui::transparent_black())
                    .when_some(state.fill, |tab, fill| tab.bg(gpui_color(fill)))
                    .text_color(gpui_color(state.text))
                    .when(!selected, |tab| {
                        tab.hover(move |tab| {
                            tab.bg(gpui_color(colors.hover_bg))
                                .text_color(gpui_color(state.hover_text))
                        })
                    })
                    .focus_visible(move |tab| tab.border_color(gpui_color(colors.ring_primary)))
            }
            TabSurface::Chrome {
                resting,
                selected: selected_fill,
            } => render_chrome_tab(
                tab,
                &self.theme,
                selected,
                resting,
                selected_fill,
                border_width,
            ),
        }
        .children(self.children);
        tab.style().refine(&self.style);
        // Radius is component-owned: callers can size and lay out a tab, but an underlined tab
        // stays square, a track tab is a 999px pill, a sidebar section `--radius-default`, and a
        // chrome tab uses attached top/hover radii rather than a full pill.
        match self.surface {
            TabSurface::Segmented => tab.rounded(px(0.0)),
            TabSurface::Track => tab.rounded(tab_radius()),
            TabSurface::Sidebar => tab.rounded(sidebar_tab_radius()),
            TabSurface::Chrome { .. } if selected => tab
                .rounded_tl(px(CHROME_TAB_TOP_RADIUS))
                .rounded_tr(px(CHROME_TAB_TOP_RADIUS))
                .rounded_bl(px(0.0))
                .rounded_br(px(0.0)),
            TabSurface::Chrome { .. } => tab.rounded(px(CHROME_TAB_INACTIVE_RADIUS)),
        }
    }
}

fn render_chrome_tab(
    tab: BaseButton,
    theme: &AerisTheme,
    selected: bool,
    resting: ThemeColor,
    selected_fill: ThemeColor,
    border_width: Pixels,
) -> BaseButton {
    let colors = theme.colors;
    if selected {
        tab.relative()
            .h(px(CHROME_TAB_ACTIVE_HEIGHT))
            .pb(px(CHROME_TAB_ACTIVE_PADDING_BOTTOM))
            .bg(gpui::transparent_black())
            .text_color(gpui_color(colors.text_default))
            .focus_visible(move |tab| {
                tab.border_t(border_width)
                    .border_l(border_width)
                    .border_r(border_width)
                    .border_color(gpui_color(colors.ring_primary))
            })
            .child(attached_chrome_tab_backdrop(
                selected_fill,
                colors.border,
                border_width,
            ))
    } else {
        let hover_fill = if resting == colors.surface_secondary {
            colors.hover_bg_secondary
        } else {
            colors.hover_bg.over(resting)
        };
        tab.h(px(CHROME_TAB_INACTIVE_HEIGHT))
            .mb(px(CHROME_TAB_INACTIVE_BOTTOM_MARGIN))
            .border(border_width)
            .border_color(gpui::transparent_black())
            .bg(gpui::transparent_black())
            .text_color(gpui_color(colors.text_secondary))
            .hover(move |tab| {
                tab.bg(gpui_color(hover_fill))
                    .text_color(gpui_color(colors.text_default))
            })
            .focus_visible(move |tab| tab.border_color(gpui_color(colors.ring_primary)))
    }
}

/// Paints the active attached chrome tab as one unified vector silhouette (rounded top corners,
/// straight vertical sides, and concave bottom flares) so the fill seamlessly merges into the
/// primary surface below and the border stroke flows continuously into the title bar hairline.
fn attached_chrome_tab_backdrop(
    fill_color: ThemeColor,
    border_color: ThemeColor,
    border_width: Pixels,
) -> impl IntoElement {
    let flare = px(CHROME_TAB_FLARE_RADIUS);
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            paint_attached_chrome_tab(window, bounds, fill_color, border_color, border_width);
        },
    )
    .absolute()
    .left(-flare)
    .right(-flare)
    .top_0()
    .bottom_0()
}

fn paint_attached_chrome_tab(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    fill_color: ThemeColor,
    border_color: ThemeColor,
    border_width: Pixels,
) {
    let left = f32::from(bounds.left());
    let right = f32::from(bounds.right());
    let top = f32::from(bounds.top());
    let bottom = f32::from(bounds.bottom());
    let bw = f32::from(border_width);
    let half = bw * 0.5;
    let xl = left + CHROME_TAB_FLARE_RADIUS + half;
    let xr = right - CHROME_TAB_FLARE_RADIUS - half;
    let yt = top + half;
    let yb = bottom - half;
    let body_w = (xr - xl).max(0.0);
    let body_h = (yb - yt).max(0.0);
    if body_w <= 0.0 || body_h <= 0.0 {
        return;
    }
    let rb = CHROME_TAB_FLARE_RADIUS.min(body_h * 0.5);
    let rt = CHROME_TAB_TOP_RADIUS
        .min(body_w * 0.5)
        .min((body_h - rb).max(0.0));

    let mut fill_path = PathBuilder::fill();
    fill_path.move_to(point(px(xl - rb), px(bottom)));
    fill_path.line_to(point(px(xl - rb), px(yb)));
    trace_attached_chrome_tab_outline(&mut fill_path, xl, xr, yt, yb, rt, rb);
    fill_path.line_to(point(px(xr + rb), px(bottom)));
    fill_path.close();
    if let Ok(path) = fill_path.build() {
        window.paint_path(path, gpui_color(fill_color));
    }

    let mut stroke_path = PathBuilder::stroke(border_width);
    stroke_path.move_to(point(px(xl - rb), px(yb)));
    trace_attached_chrome_tab_outline(&mut stroke_path, xl, xr, yt, yb, rt, rb);
    if let Ok(path) = stroke_path.build() {
        window.paint_path(path, gpui_color(border_color));
    }
}

fn trace_attached_chrome_tab_outline(
    path: &mut PathBuilder,
    xl: f32,
    xr: f32,
    yt: f32,
    yb: f32,
    rt: f32,
    rb: f32,
) {
    let kb = rb * QUARTER_CIRCLE_KAPPA;
    let kt = rt * QUARTER_CIRCLE_KAPPA;
    // Left concave bottom flare: horizontal tangent at (xl - rb, yb) -> vertical at (xl, yb - rb).
    path.cubic_bezier_to(
        point(px(xl), px(yb - rb)),
        point(px(xl - rb + kb), px(yb)),
        point(px(xl), px(yb - rb + kb)),
    );
    path.line_to(point(px(xl), px(yt + rt)));
    // Top-left convex corner.
    path.cubic_bezier_to(
        point(px(xl + rt), px(yt)),
        point(px(xl), px(yt + rt - kt)),
        point(px(xl + rt - kt), px(yt)),
    );
    path.line_to(point(px(xr - rt), px(yt)));
    // Top-right convex corner.
    path.cubic_bezier_to(
        point(px(xr), px(yt + rt)),
        point(px(xr - rt + kt), px(yt)),
        point(px(xr), px(yt + rt - kt)),
    );
    path.line_to(point(px(xr), px(yb - rb)));
    // Right concave bottom flare: vertical tangent at (xr, yb - rb) -> horizontal at (xr + rb, yb).
    path.cubic_bezier_to(
        point(px(xr + rb), px(yb)),
        point(px(xr), px(yb - rb + kb)),
        point(px(xr + rb - kb), px(yb)),
    );
}

fn render_track_tab(
    tab: BaseButton,
    theme: &AerisTheme,
    selected: bool,
    border_width: Pixels,
) -> BaseButton {
    let colors = theme.colors;
    let state = track_tab_colors(theme, selected);
    tab.flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(TRACK_TAB_HEIGHT)
        .px(TRACK_TAB_PADDING_X)
        .border(border_width)
        .border_color(
            state
                .border
                .map_or_else(gpui::transparent_black, gpui_color),
        )
        .when_some(state.fill, |tab, fill| tab.bg(gpui_color(fill)))
        .text_xs()
        .line_height(TRACK_TAB_LINE_HEIGHT)
        .whitespace_nowrap()
        .text_color(gpui_color(state.text))
        .when(!selected, |tab| {
            tab.hover(move |tab| tab.text_color(gpui_color(state.hover_text)))
                .active(move |tab| tab.text_color(gpui_color(state.active_text)))
        })
        .focus_visible(move |tab| tab.border_color(gpui_color(colors.ring_primary)))
}

#[cfg(test)]
mod tests {
    use aeris_design_system::AerisTheme;
    use gpui::px;

    use super::{
        TRACK_SELECTED_BORDER_ALPHA, Tab, TabSurface, segmented_tab_colors, sidebar_tab_colors,
        sidebar_tab_radius, tab_radius, track_tab_colors,
    };

    #[test]
    fn track_tabs_follow_the_theme_system_ui_tab_normal_contract() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let colors = theme.colors;
            let resting = track_tab_colors(&theme, false);
            assert_eq!(resting.fill, None);
            assert_eq!(resting.border, None);
            assert_eq!(resting.text, colors.text_secondary);
            assert_eq!(resting.hover_text, colors.text_default);
            assert_eq!(resting.active_text, colors.text_default);

            let selected = track_tab_colors(&theme, true);
            assert_eq!(selected.fill, Some(colors.surface_secondary));
            assert_eq!(
                selected.border,
                Some(colors.text_default.with_alpha(TRACK_SELECTED_BORDER_ALPHA))
            );
            assert_eq!(selected.text, colors.text_default);
        }
    }

    #[test]
    fn shared_tabs_use_the_platform_radius_tokens() {
        assert_eq!(tab_radius(), px(999.0));
        assert_eq!(sidebar_tab_radius(), px(6.0));
    }

    #[test]
    fn segmented_tabs_follow_the_theme_system_ui_tab_contract() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let colors = theme.colors;
            let resting = segmented_tab_colors(&theme, false);
            assert_eq!(resting.fill, None);
            assert_eq!(resting.border, None);
            assert_eq!(resting.text, colors.text_secondary);
            assert_eq!(resting.hover_text, colors.text_default);
            assert_eq!(resting.active_text, colors.text_default);

            let selected = segmented_tab_colors(&theme, true);
            assert_eq!(selected.fill, None);
            assert_eq!(selected.border, Some(colors.primary));
            assert_eq!(selected.text, colors.primary);
        }
    }

    #[test]
    fn sidebar_sections_fill_only_while_selected() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let colors = theme.colors;
            let resting = sidebar_tab_colors(&theme, false);
            assert_eq!(resting.fill, None);
            assert_eq!(resting.text, colors.text_secondary);
            let selected = sidebar_tab_colors(&theme, true);
            assert_eq!(selected.fill, Some(colors.active_bg));
            assert_eq!(selected.text, colors.text_default);
        }
    }

    #[test]
    fn tabs_default_to_the_segmented_contract_and_opt_into_chrome() {
        let theme = AerisTheme::dark();
        assert_eq!(Tab::new("tab", &theme).surface, TabSurface::Segmented);
        let chrome = Tab::new("workspace", &theme)
            .chrome(theme.colors.surface_secondary, theme.colors.surface);
        assert_eq!(
            chrome.surface,
            TabSurface::Chrome {
                resting: theme.colors.surface_secondary,
                selected: theme.colors.surface,
            }
        );
    }
}
