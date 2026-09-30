use std::rc::Rc;

use aeris_design_system::AerisTheme;
use gpui::{App, ClickEvent, ElementId, IntoElement, RenderOnce, SharedString, Window, prelude::*};
use gpui_base::{Switch as BaseSwitch, SwitchThumb, SwitchTrack};

use super::{rem_scale::design_rems, theme::gpui_color};

type ChangeHandler = Rc<dyn Fn(bool, &ClickEvent, &mut Window, &mut App)>;

/// The desktop switch appearance, backed by the shared switch interaction model.
#[derive(IntoElement)]
pub(crate) struct Switch {
    id: ElementId,
    label: SharedString,
    checked: bool,
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
            theme: *theme,
            on_change: None,
        }
    }

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
        BaseSwitch::new(self.id.clone())
            .checked(self.checked)
            .accessibility_label(self.label)
            .w(design_rems(34.0))
            .h(design_rems(19.0))
            .cursor_pointer()
            .when_some(self.on_change, |switch, on_change| {
                switch.on_change(move |checked, event, window, cx| {
                    on_change(checked, event, window, cx);
                })
            })
            .child(
                SwitchTrack::new((self.id, "track"))
                    .checked(self.checked)
                    .size_full()
                    .p(design_rems(2.0))
                    .flex()
                    .items_center()
                    .when(self.checked, |track| {
                        track.justify_end().bg(gpui_color(colors.primary))
                    })
                    .when(!self.checked, |track| {
                        track
                            .justify_start()
                            .bg(gpui_color(colors.border_secondary))
                    })
                    .rounded_full()
                    .child(
                        SwitchThumb::new(self.checked)
                            .w(design_rems(19.0))
                            .h(design_rems(15.0))
                            .rounded_full()
                            .bg(gpui_color(colors.primary_foreground)),
                    ),
            )
    }
}
