//! `Aeris`'s checkbox: [`CheckboxRow`], a box with its label. It is for an option that refines
//! another setting, such as whether a legend shows its values; a setting of its own uses a
//! [`super::switch::Switch`].

use std::rc::Rc;

use aeris_design_system::{AerisTheme, RadiusToken, ThemeColor};
use gpui::{
    App, ClickEvent, ElementId, IntoElement, MouseButton, ParentElement, RenderOnce, Role,
    SharedString, Styled, Toggled, Window, div, prelude::*, px,
};

use super::{
    icon::Icon,
    rem_scale::design_rems,
    theme::{gpui_color, platform_border_width},
};
use crate::desktop::assets::UiIcon;

type ChangeHandler = Rc<dyn Fn(bool, &ClickEvent, &mut Window, &mut App)>;

const CHECKBOX_GROUP: &str = "aeris-checkbox";

/// The box's colours for one state.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CheckboxInk {
    fill: ThemeColor,
    border: ThemeColor,
    hover_fill: Option<ThemeColor>,
    tick: Option<ThemeColor>,
    label: ThemeColor,
}

fn checkbox_ink(theme: &AerisTheme, checked: bool, disabled: bool) -> CheckboxInk {
    let colors = theme.colors;
    match (checked, disabled) {
        (true, false) => CheckboxInk {
            fill: colors.primary,
            border: colors.primary,
            hover_fill: Some(colors.primary_hover),
            tick: Some(colors.primary_foreground),
            label: colors.text_primary,
        },
        (true, true) => CheckboxInk {
            fill: colors.primary_disabled,
            border: colors.primary_disabled,
            hover_fill: None,
            tick: Some(colors.primary_disabled_foreground),
            label: colors.text_muted,
        },
        (false, false) => CheckboxInk {
            fill: colors.surface,
            border: colors.border_strong,
            hover_fill: Some(colors.hover_bg),
            tick: None,
            label: colors.text_primary,
        },
        (false, true) => CheckboxInk {
            fill: colors.disabled_bg,
            border: colors.border_secondary,
            hover_fill: None,
            tick: None,
            label: colors.text_muted,
        },
    }
}

/// A checkbox and its label. The whole row toggles, by pointer or by Enter or Space when focused.
#[derive(IntoElement)]
pub(crate) struct CheckboxRow {
    id: ElementId,
    label: SharedString,
    checked: bool,
    disabled: bool,
    theme: AerisTheme,
    on_change: Option<ChangeHandler>,
}

impl CheckboxRow {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        checked: bool,
        theme: &AerisTheme,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            checked,
            disabled: false,
            theme: *theme,
            on_change: None,
        }
    }

    /// A checkbox that shows its value but cannot be changed.
    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Runs with the new value. The change never reaches the surfaces underneath.
    pub(crate) fn on_change(
        mut self,
        handler: impl Fn(bool, &ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for CheckboxRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ink = checkbox_ink(&self.theme, self.checked, self.disabled);
        let checked = self.checked;
        let disabled = self.disabled;
        let focus_ring = self.theme.colors.ring_primary;
        let focus_handle = window
            .use_keyed_state(self.id.clone(), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let radius = px(f32::from(RadiusToken::Sm.logical_pixels()));
        let check_box = div()
            .flex_none()
            .size(design_rems(16.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(radius)
            .border(platform_border_width(&self.theme))
            .border_color(gpui_color(ink.border))
            .bg(gpui_color(ink.fill))
            .when_some(ink.hover_fill, |check_box, hover_fill| {
                check_box.group_hover(CHECKBOX_GROUP, move |style| {
                    style.bg(gpui_color(hover_fill))
                })
            })
            .children(ink.tick.map(|tick| {
                Icon::new(UiIcon::Tick.path())
                    .with_size(px(12.0))
                    .color(gpui_color(tick))
            }));
        div()
            .id(self.id)
            .group(CHECKBOX_GROUP)
            .role(Role::CheckBox)
            .aria_toggled(if checked {
                Toggled::True
            } else {
                Toggled::False
            })
            .aria_label(self.label.clone())
            .min_h(design_rems(30.0))
            .flex()
            .items_center()
            .gap_2()
            .rounded(radius)
            .border(platform_border_width(&self.theme))
            .border_color(gpui::transparent_black())
            .focus_visible(move |style| style.border_2().border_color(gpui_color(focus_ring)))
            .map(|row| {
                if disabled {
                    row.cursor_not_allowed()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                } else {
                    row.cursor_pointer().track_focus(&focus_handle)
                }
            })
            .when_some(self.on_change.filter(|_| !disabled), |row, on_change| {
                row.on_click(move |event, window, cx| {
                    on_change(!checked, event, window, cx);
                    cx.stop_propagation();
                })
            })
            .child(check_box)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(gpui_color(ink.label))
                    .child(self.label),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_boxes_fill_with_primary_and_only_enabled_boxes_react_to_hover() {
        let theme = AerisTheme::dark();
        let colors = theme.colors;
        let checked = checkbox_ink(&theme, true, false);
        assert_eq!(checked.fill, colors.primary);
        assert_eq!(checked.tick, Some(colors.primary_foreground));
        assert_eq!(checked.hover_fill, Some(colors.primary_hover));

        let unchecked = checkbox_ink(&theme, false, false);
        assert_eq!(unchecked.tick, None);
        assert_eq!(unchecked.hover_fill, Some(colors.hover_bg));

        for checked in [true, false] {
            let disabled = checkbox_ink(&theme, checked, true);
            assert_eq!(disabled.hover_fill, None);
            assert_eq!(disabled.label, colors.text_muted);
        }
        assert_eq!(
            checkbox_ink(&theme, true, true).tick,
            Some(colors.primary_disabled_foreground)
        );
    }
}
