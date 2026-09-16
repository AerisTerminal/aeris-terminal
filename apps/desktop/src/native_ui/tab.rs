use axiusflow_design_system::{AxiusflowTheme, RadiusToken, TypographyRole, platform_font_family};
use gpui::{
    AnyElement, App, Div, ElementId, InteractiveElement, Interactivity, IntoElement, ParentElement,
    RenderOnce, Stateful, StyleRefinement, Styled, Window, div, prelude::*, px,
};

use super::{
    platform_font_weight,
    theme::{gpui_color, platform_border_width},
};

fn tab_radius() -> gpui::Pixels {
    px(f32::from(RadiusToken::Full.logical_pixels()))
}

/// Shared Axiusflow tab surface. Tabs own their semantic role, selected-state
/// treatment, focus treatment, and the canonical pill radius while callers own
/// layout, content, and activation behavior.
#[derive(IntoElement)]
pub(crate) struct Tab {
    base: Stateful<Div>,
    style: StyleRefinement,
    theme: AxiusflowTheme,
    selected: bool,
    segmented: bool,
    children: Vec<AnyElement>,
}

impl Tab {
    pub(crate) fn new(id: impl Into<ElementId>, theme: &AxiusflowTheme) -> Self {
        Self {
            base: div().id(id.into()),
            style: StyleRefinement::default(),
            theme: *theme,
            selected: false,
            segmented: false,
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
        let resting_fill = if self.segmented {
            colors.input_fill
        } else {
            colors.surface_secondary
        };
        let selected_fill = colors.active_bg.over(resting_fill);
        let resting_border = if self.segmented {
            colors.input_fill
        } else {
            colors.surface_secondary
        };
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

    use super::tab_radius;

    #[test]
    fn shared_tabs_use_the_platform_full_radius_token() {
        assert_eq!(tab_radius(), px(999.0));
    }
}
