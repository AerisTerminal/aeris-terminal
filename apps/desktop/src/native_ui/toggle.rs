use std::rc::Rc;

use axiusflow_design_system::{AxiusflowTheme, RadiusToken};
use gpui::{
    App, ClickEvent, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce, Role,
    SharedString, StatefulInteractiveElement, Styled, Toggled, Window, div, prelude::*, px,
};

const TRACK_WIDTH: f32 = 36.0;
const TRACK_HEIGHT: f32 = 20.0;
const THUMB_SIZE: f32 = 16.0;
const THUMB_INSET: f32 = 2.0;

type Activation = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A binary switch painted from platform tokens.
///
/// On uses `--primary` / `--primary-foreground`. Off uses muted ink over the
/// resting secondary-surface fill of menus and settings.
#[derive(IntoElement)]
pub(crate) struct Toggle {
    id: ElementId,
    selected: bool,
    disabled: bool,
    theme: AxiusflowTheme,
    activation: Option<Activation>,
    aria_label: SharedString,
}

impl Toggle {
    pub(crate) fn new(id: impl Into<ElementId>, theme: &AxiusflowTheme) -> Self {
        Self {
            id: id.into(),
            selected: false,
            disabled: false,
            theme: *theme,
            activation: None,
            aria_label: SharedString::from("Toggle"),
        }
    }

    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub(crate) fn aria_label(mut self, label: impl Into<SharedString>) -> Self {
        self.aria_label = label.into();
        self
    }

    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.activation = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Toggle {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let colors = self.theme.colors;
        let selected = self.selected;
        let disabled = self.disabled;
        let track = if selected {
            gpui_color(colors.primary)
        } else {
            gpui_color(colors.text_muted)
        };
        let thumb = if selected {
            gpui_color(colors.primary_foreground)
        } else {
            gpui_color(colors.surface)
        };
        let thumb_x = if selected {
            TRACK_WIDTH - THUMB_SIZE - THUMB_INSET
        } else {
            THUMB_INSET
        };
        let activation = self.activation.filter(|_| !disabled);
        div()
            .id(self.id)
            .occlude()
            .relative()
            .w(px(TRACK_WIDTH))
            .h(px(TRACK_HEIGHT))
            .flex_none()
            .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
            .bg(track)
            .role(Role::Switch)
            .aria_label(self.aria_label)
            .aria_toggled(if selected {
                Toggled::True
            } else {
                Toggled::False
            })
            .when(disabled, |toggle| toggle.opacity(0.55).cursor_not_allowed())
            .when_some(activation, |toggle, handler| {
                toggle.cursor_pointer().on_click(move |event, window, cx| {
                    handler(event, window, cx);
                    cx.stop_propagation();
                })
            })
            .child(
                div()
                    .absolute()
                    .top(px(THUMB_INSET))
                    .left(px(thumb_x))
                    .size(px(THUMB_SIZE))
                    .rounded_full()
                    .bg(thumb),
            )
    }
}

fn gpui_color(color: axiusflow_design_system::ThemeColor) -> gpui::Hsla {
    let (h, s, l, a) = color.hsla_components();
    gpui::Hsla { h, s, l, a }
}

#[cfg(test)]
mod tests {
    use super::{THUMB_INSET, THUMB_SIZE, TRACK_HEIGHT, TRACK_WIDTH};

    #[test]
    fn switch_geometry_keeps_the_thumb_inside_the_track() {
        const {
            assert!(THUMB_SIZE + THUMB_INSET * 2.0 <= TRACK_HEIGHT);
            assert!(THUMB_SIZE + THUMB_INSET * 2.0 <= TRACK_WIDTH);
            assert!(TRACK_WIDTH - THUMB_SIZE - THUMB_INSET >= THUMB_INSET);
        }
    }
}
