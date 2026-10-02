//! Theme-token tooltip shared by desktop chrome and chart product glue.

use std::{sync::Arc, time::Duration};

use aeris_design_system::{
    AerisTheme, RadiusToken, ThemeColor, TypographyRole, platform_font_family, platform_typography,
};
use gpui::{
    AnyView, App, Context, ElementId, Hsla, IntoElement, Render, SharedString, Task, Window, div,
    prelude::*, px,
};

fn gpui_color(color: ThemeColor) -> Hsla {
    let (h, s, l, a) = color.hsla_components();
    Hsla { h, s, l, a }
}

/// Data needed to build a native GPUI tooltip without process-global theme state.
#[derive(Clone)]
pub struct TooltipSpec {
    content: TooltipContent,
    theme: AerisTheme,
    show_delay: Duration,
}

#[derive(Clone)]
enum TooltipContent {
    Static(SharedString),
    Dynamic(Arc<dyn Fn(&App) -> SharedString>),
}

impl TooltipSpec {
    #[must_use]
    pub fn new(label: impl Into<SharedString>, theme: &AerisTheme) -> Self {
        Self {
            content: TooltipContent::Static(label.into()),
            theme: *theme,
            show_delay: Duration::from_millis(500),
        }
    }

    /// Recomputes text while the tooltip is visible; its one-second wake is
    /// lifecycle-owned by the tooltip view and cancels when hover ends.
    #[must_use]
    pub fn dynamic(label: impl Fn(&App) -> SharedString + 'static, theme: &AerisTheme) -> Self {
        Self {
            content: TooltipContent::Dynamic(Arc::new(label)),
            theme: *theme,
            show_delay: Duration::from_millis(500),
        }
    }

    #[must_use]
    pub fn show_delay(mut self, delay: Duration) -> Self {
        self.show_delay = delay;
        self
    }

    #[must_use]
    pub const fn delay(&self) -> Duration {
        self.show_delay
    }

    pub fn builder(&self) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let content = self.content.clone();
        let theme = self.theme;
        move |_, cx| {
            cx.new(|_| TooltipView {
                content: content.clone(),
                theme,
                tick: None,
            })
            .into()
        }
    }
}

struct TooltipView {
    content: TooltipContent,
    theme: AerisTheme,
    tick: Option<Task<()>>,
}

impl Render for TooltipView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if matches!(self.content, TooltipContent::Dynamic(_)) && self.tick.is_none() {
            self.tick = Some(cx.spawn(async move |view, cx| {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let _ = view.update(cx, |view, view_cx| {
                    view.tick = None;
                    view_cx.notify();
                });
            }));
        }
        let label = match &self.content {
            TooltipContent::Static(label) => label.clone(),
            TooltipContent::Dynamic(label) => label(cx),
        };
        let colors = self.theme.colors;
        div().pl_2().pt_2().child(
            div()
                .max_w(px(320.0))
                .px_2()
                .py_1()
                .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                .border(px(self.theme.dimensions.border_width))
                .border_color(gpui_color(colors.border))
                .bg(gpui_color(colors.surface))
                .font_family(platform_font_family())
                .font_weight(gpui::FontWeight(f32::from(
                    platform_typography().weight(TypographyRole::Normal),
                )))
                .text_xs()
                .text_color(gpui_color(colors.text_primary))
                .child(label),
        )
    }
}

/// Attaches GPUI's lifecycle-owned tooltip to a trigger.
pub fn with_tooltip(
    id: impl Into<ElementId>,
    trigger: impl IntoElement + 'static,
    spec: &TooltipSpec,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .child(trigger)
        .tooltip(spec.builder())
        .tooltip_show_delay(spec.delay())
}

#[cfg(test)]
mod tests {
    use super::TooltipSpec;
    use aeris_design_system::AerisTheme;
    use std::time::Duration;

    #[test]
    fn tooltip_delay_is_explicit_and_bounded_by_the_owner() {
        let tooltip =
            TooltipSpec::new("Close", &AerisTheme::dark()).show_delay(Duration::from_millis(275));
        assert_eq!(tooltip.delay(), Duration::from_millis(275));
    }
}
