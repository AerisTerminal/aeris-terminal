use aeris_design_system::{
    AerisTheme, RadiusToken, ThemeColor, TypographyRole, platform_font_family,
};
use gpui::{
    AnyElement, App, ElementId, InteractiveElement, Interactivity, IntoElement, ParentElement,
    RenderOnce, StyleRefinement, Styled, Window, prelude::*, px,
};
use gpui_base::Button as BaseButton;

use super::{
    platform_font_weight,
    theme::{gpui_color, platform_border_width},
};

fn tab_radius() -> gpui::Pixels {
    px(f32::from(RadiusToken::Full.logical_pixels()))
}

/// Shared `Aeris` tab surface. Tabs own their semantic role, selected-state
/// treatment, focus treatment, and the canonical pill radius while callers own
/// layout, content, and activation behavior.
#[derive(IntoElement)]
pub(crate) struct Tab {
    base: BaseButton,
    style: StyleRefinement,
    theme: AerisTheme,
    selected: bool,
    segmented: bool,
    resting_fill: Option<ThemeColor>,
    children: Vec<AnyElement>,
}

impl Tab {
    pub(crate) fn new(id: impl Into<ElementId>, theme: &AerisTheme) -> Self {
        Self {
            base: BaseButton::new(id),
            style: StyleRefinement::default(),
            theme: *theme,
            selected: false,
            segmented: false,
            resting_fill: None,
            children: Vec::new(),
        }
    }

    pub(crate) const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Uses the shared tab interaction/radius contract inside a segmented
    /// control. The surrounding group owns the border while each tab remains a
    /// full-radius pill and uses the canonical input/interaction/text tokens.
    pub(crate) const fn segmented(mut self) -> Self {
        self.segmented = true;
        self
    }

    /// Sets the authoritative surface beneath this tab and its interaction
    /// overlays. Callers use this when a tab belongs to a surface other than
    /// the shared secondary tab container.
    pub(crate) const fn resting_fill(mut self, fill: ThemeColor) -> Self {
        self.resting_fill = Some(fill);
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
        let resting_fill = self.resting_fill.unwrap_or(colors.surface_secondary);
        let selected_fill = colors.active_bg.over(resting_fill);
        let resting_border = resting_fill;
        let mut tab = self
            .base
            .occlude()
            .role(gpui::Role::Tab)
            .aria_selected(self.selected)
            .rounded(tab_radius())
            .border(platform_border_width(&self.theme))
            .border_color(gpui_color(if self.selected {
                if self.segmented {
                    resting_border
                } else {
                    colors.border
                }
            } else {
                resting_border
            }))
            .bg(gpui_color(if self.selected {
                selected_fill
            } else {
                resting_fill
            }))
            .text_color(gpui_color(if self.selected {
                colors.text_primary
            } else {
                colors.text_secondary
            }))
            .font_family(platform_font_family())
            .font_weight(platform_font_weight(TypographyRole::Normal))
            .cursor_pointer()
            .when(!self.selected, |tab| {
                tab.hover(move |tab| {
                    tab.bg(gpui_color(colors.hover_bg.over(resting_fill)))
                        .text_color(gpui_color(colors.text_primary))
                })
            })
            .focus_visible(move |tab| tab.border_color(gpui_color(colors.ring)).border_2())
            .children(self.children);
        tab.style().refine(&self.style);
        // Radius is component-owned: callers can size/layout a tab, but every
        // semantic tab remains the canonical 999px pill from platform.css.
        tab.rounded(tab_radius())
    }
}

#[cfg(test)]
mod tests {
    use gpui::px;

    use super::{Tab, tab_radius};

    #[test]
    fn shared_tabs_use_the_platform_full_radius_token() {
        assert_eq!(tab_radius(), px(999.0));
    }

    #[test]
    fn tabs_can_bind_interaction_states_to_their_own_surface() {
        for theme in [
            aeris_design_system::AerisTheme::light(),
            aeris_design_system::AerisTheme::dark(),
        ] {
            let tab = Tab::new("surface_tab", &theme).resting_fill(theme.colors.surface);
            assert_eq!(tab.resting_fill, Some(theme.colors.surface));
        }
    }
}
