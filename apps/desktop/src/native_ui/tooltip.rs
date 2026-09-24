use std::time::Duration;

use aeris_design_system::{AerisTheme, RadiusToken, TypographyRole, platform_font_family};
use gpui::{
    AnyView, App, Context, ElementId, IntoElement, Render, SharedString, Window, div, prelude::*,
    px,
};

use super::{
    platform_font_weight,
    theme::{gpui_color, platform_border_width},
};

/// Data needed to build a native GPUI tooltip without process-global theme
/// state.
#[derive(Clone)]
pub(crate) struct TooltipSpec {
    label: SharedString,
    theme: AerisTheme,
    show_delay: Duration,
}

impl TooltipSpec {
    pub(crate) fn new(label: impl Into<SharedString>, theme: &AerisTheme) -> Self {
        Self {
            label: label.into(),
            theme: *theme,
            show_delay: Duration::from_millis(500),
        }
    }

    pub(crate) fn show_delay(mut self, delay: Duration) -> Self {
        self.show_delay = delay;
        self
    }

    pub(crate) const fn delay(&self) -> Duration {
        self.show_delay
    }

    pub(crate) fn builder(&self) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let label = self.label.clone();
        let theme = self.theme;
        move |_, cx| {
            cx.new(|_| TooltipView {
                label: label.clone(),
                theme,
            })
            .into()
        }
    }
}

struct TooltipView {
    label: SharedString,
    theme: AerisTheme,
}

impl Render for TooltipView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        div().pl_2().pt_2().child(
            div()
                .max_w(px(320.0))
                .px_2()
                .py_1()
                .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                .border(platform_border_width(&self.theme))
                .border_color(gpui_color(colors.border))
                .bg(gpui_color(colors.surface))
                .font_family(platform_font_family())
                .font_weight(platform_font_weight(TypographyRole::Normal))
                .text_xs()
                .text_color(gpui_color(colors.text_primary))
                .child(self.label.clone()),
        )
    }
}

/// Attaches GPUI's lifecycle-owned tooltip to a trigger. GPUI cancels pending
/// and visible tooltips when the trigger disappears or the pointer leaves it.
pub(crate) fn with_tooltip(
    id: impl Into<ElementId>,
    trigger: impl IntoElement + 'static,
    spec: &TooltipSpec,
) -> impl IntoElement {
    let delay = spec.delay();
    div()
        .id(id)
        .flex()
        .child(trigger)
        .tooltip(spec.builder())
        .tooltip_show_delay(delay)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aeris_design_system::AerisTheme;

    use super::TooltipSpec;

    #[test]
    fn tooltip_delay_is_explicit_and_bounded_by_the_owner() {
        let tooltip =
            TooltipSpec::new("Close", &AerisTheme::dark()).show_delay(Duration::from_millis(275));
        assert_eq!(tooltip.delay(), Duration::from_millis(275));
    }
}
