//! `Aeris`'s on/off control: [`Switch`] alone, or [`SwitchRow`] with its label and an optional
//! description. Every boolean setting uses one of them; an "On"/"Off" button pair is never a
//! substitute.

use std::rc::Rc;

use aeris_design_system::AerisTheme;
use gpui::{
    App, ClickEvent, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled,
    Window, div, prelude::*,
};
use gpui_base::{Switch as BaseSwitch, SwitchThumb, SwitchTrack};

use super::{rem_scale::design_rems, theme::gpui_color};

type ChangeHandler = Rc<dyn Fn(bool, &ClickEvent, &mut Window, &mut App)>;

/// The desktop switch appearance, backed by the shared switch interaction model.
#[derive(IntoElement)]
pub(crate) struct Switch {
    id: ElementId,
    label: SharedString,
    checked: bool,
    disabled: bool,
    theme: AerisTheme,
    on_change: Option<ChangeHandler>,
}

impl Switch {
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

    /// A switch that shows its value but cannot be changed.
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

impl RenderOnce for Switch {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let colors = self.theme.colors;
        let disabled = self.disabled;
        let track = if disabled {
            colors.disabled_bg
        } else if self.checked {
            colors.primary
        } else {
            colors.border_secondary
        };
        BaseSwitch::new(self.id.clone())
            .checked(self.checked)
            .disabled(disabled)
            .accessibility_label(self.label)
            .w(design_rems(44.0))
            .h(design_rems(19.0))
            .map(|switch| {
                if disabled {
                    switch.cursor_not_allowed()
                } else {
                    switch.cursor_pointer()
                }
            })
            .when_some(self.on_change.filter(|_| !disabled), |switch, on_change| {
                switch.on_change(move |checked, event, window, cx| {
                    on_change(checked, event, window, cx);
                    cx.stop_propagation();
                })
            })
            .child(
                SwitchTrack::new((self.id, "track"))
                    .checked(self.checked)
                    .size_full()
                    .p(design_rems(2.0))
                    .flex()
                    .items_center()
                    .map(|track| {
                        if self.checked {
                            track.justify_end()
                        } else {
                            track.justify_start()
                        }
                    })
                    .bg(gpui_color(track))
                    .rounded_full()
                    .child(
                        SwitchThumb::new(self.checked)
                            .w(design_rems(23.0))
                            .h(design_rems(15.0))
                            .rounded_full()
                            .bg(gpui_color(colors.primary_foreground)),
                    ),
            )
    }
}

/// A labelled setting row: the label and an optional muted description on the left, the switch
/// on the right.
#[derive(IntoElement)]
pub(crate) struct SwitchRow {
    switch: Switch,
    label: SharedString,
    description: Option<SharedString>,
    theme: AerisTheme,
}

impl SwitchRow {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        checked: bool,
        theme: &AerisTheme,
    ) -> Self {
        let label = label.into();
        Self {
            switch: Switch::new(id, label.clone(), checked, theme),
            label,
            description: None,
            theme: *theme,
        }
    }

    pub(crate) fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub(crate) fn on_change(
        mut self,
        handler: impl Fn(bool, &ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.switch = self.switch.on_change(handler);
        self
    }
}

impl RenderOnce for SwitchRow {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let colors = self.theme.colors;
        div()
            .min_h(design_rems(38.0))
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_sm()
                            .text_color(gpui_color(colors.text_primary))
                            .child(self.label),
                    )
                    .children(self.description.map(|description| {
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(gpui_color(colors.text_muted))
                            .child(description)
                    })),
            )
            .child(self.switch)
    }
}
