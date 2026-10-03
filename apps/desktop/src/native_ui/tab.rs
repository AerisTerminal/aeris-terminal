//! Shared tab components. [`TabList`] + [`Tab`] are the Theme System `.ui-tabs` / `.ui-tab`
//! segmented control; callers add only layout and spacing, never colors, borders or radii.

use aeris_design_system::{
    AerisTheme, RadiusToken, ThemeColor, TypographyRole, platform_font_family,
};
use gpui::{
    AnyElement, App, Div, ElementId, InteractiveElement, Interactivity, IntoElement, ParentElement,
    Pixels, RenderOnce, SharedString, Stateful, StyleRefinement, Styled, Window, div, prelude::*,
    px,
};
use gpui_base::Button as BaseButton;

use super::{
    platform_font_weight,
    theme::{gpui_color, platform_border_width},
};

/// Theme System tab geometry: the track is `h-7 p-0.5 gap-0.5`, each tab `h-6 px-2 text-xs`.
const TAB_LIST_HEIGHT: Pixels = px(28.0);
const TAB_LIST_INSET: Pixels = px(2.0);
const TAB_HEIGHT: Pixels = px(24.0);
const TAB_PADDING_X: Pixels = px(8.0);

fn tab_radius() -> Pixels {
    px(f32::from(RadiusToken::Full.logical_pixels()))
}

/// The `.ui-tabs` track: a `surface-raised` pill with no border that holds [`Tab`]s.
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
                .items_center()
                .h(TAB_LIST_HEIGHT)
                .p(TAB_LIST_INSET)
                .gap(TAB_LIST_INSET)
                .rounded(tab_radius())
                .bg(gpui_color(theme.colors.surface_raised)),
        }
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
    /// A workspace document tab that rests on window chrome instead of a track.
    Chrome {
        resting: ThemeColor,
        selected: ThemeColor,
    },
}

/// Theme System tab colors for one selection state. Unselected tabs have no fill and a
/// transparent border so selecting one never shifts the layout; hover and press change the
/// text only.
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
            fill: Some(colors.surface),
            border: Some(colors.border),
            text: colors.text_active,
            hover_text: colors.text_active,
            active_text: colors.text_active,
        }
    } else {
        SegmentedTabColors {
            fill: None,
            border: None,
            text: colors.text_interactive,
            hover_text: colors.text_hover,
            active_text: colors.text_active,
        }
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
            surface: TabSurface::Segmented,
            children: Vec::new(),
        }
    }

    pub(crate) const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Renders a workspace document tab on window chrome: it rests on `resting` and is raised
    /// onto `selected` when active. Callers own its size.
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
            .border(platform_border_width(&self.theme))
            .font_family(platform_font_family())
            .font_weight(platform_font_weight(TypographyRole::Normal))
            .cursor_pointer();
        let mut tab = match self.surface {
            TabSurface::Segmented => {
                let state = segmented_tab_colors(&self.theme, selected);
                tab.flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .h(TAB_HEIGHT)
                    .px(TAB_PADDING_X)
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
                    .focus_visible(move |tab| tab.border_color(gpui_color(colors.border_strong)))
            }
            TabSurface::Chrome {
                resting,
                selected: selected_fill,
            } => tab
                .border_color(gpui_color(if selected { colors.border } else { resting }))
                .bg(gpui_color(if selected { selected_fill } else { resting }))
                .text_color(gpui_color(if selected {
                    colors.text_primary
                } else {
                    colors.text_secondary
                }))
                .when(!selected, |tab| {
                    tab.hover(move |tab| {
                        tab.bg(gpui_color(colors.hover_bg.over(resting)))
                            .text_color(gpui_color(colors.text_primary))
                    })
                })
                .focus_visible(move |tab| tab.border_color(gpui_color(colors.ring)).border_2()),
        }
        .children(self.children);
        tab.style().refine(&self.style);
        // Radius is component-owned: callers can size/layout a tab, but every
        // semantic tab remains the canonical 999px pill from platform.css.
        tab.rounded(tab_radius())
    }
}

#[cfg(test)]
mod tests {
    use aeris_design_system::AerisTheme;
    use gpui::px;

    use super::{Tab, TabSurface, segmented_tab_colors, tab_radius};

    #[test]
    fn shared_tabs_use_the_platform_full_radius_token() {
        assert_eq!(tab_radius(), px(999.0));
    }

    #[test]
    fn segmented_tabs_follow_the_theme_system_ui_tab_contract() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let colors = theme.colors;
            let resting = segmented_tab_colors(&theme, false);
            assert_eq!(resting.fill, None);
            assert_eq!(resting.border, None);
            assert_eq!(resting.text, colors.text_interactive);
            assert_eq!(resting.hover_text, colors.text_hover);
            assert_eq!(resting.active_text, colors.text_active);

            let selected = segmented_tab_colors(&theme, true);
            assert_eq!(selected.fill, Some(colors.surface));
            assert_eq!(selected.border, Some(colors.border));
            assert_eq!(selected.text, colors.text_active);
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
