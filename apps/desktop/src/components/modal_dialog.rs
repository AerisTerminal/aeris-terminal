//! The one modal shell every centered desktop dialog is built on, and the shared confirmation
//! dialog layered on it.
//!
//! A modal covers the nearest positioned ancestor with the `surface-overlay` scrim and centers
//! one panel in it. Pressing the scrim dismisses; presses inside the panel never reach the
//! surfaces underneath.

use super::*;

type ModalHandler = Rc<dyn Fn(&mut Window, &mut App)>;

const CONFIRMATION_DIALOG_WIDTH: f32 = 420.0;

#[derive(IntoElement)]
pub(super) struct ModalLayer {
    id: &'static str,
    width: Pixels,
    max_height: Option<Pixels>,
    radius: RadiusToken,
    theme: AerisTheme,
    on_dismiss: ModalHandler,
    children: Vec<AnyElement>,
}

impl ModalLayer {
    pub(super) fn new(
        id: &'static str,
        width: Pixels,
        theme: &AerisTheme,
        on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id,
            width,
            max_height: None,
            radius: RadiusToken::Default,
            theme: *theme,
            on_dismiss: Rc::new(on_dismiss),
            children: Vec::new(),
        }
    }

    /// Caps the panel height; without it the panel may grow to the full layer height.
    pub(super) fn max_height(mut self, height: Pixels) -> Self {
        self.max_height = Some(height);
        self
    }

    pub(super) fn radius(mut self, radius: RadiusToken) -> Self {
        self.radius = radius;
        self
    }
}

impl ParentElement for ModalLayer {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for ModalLayer {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let colors = self.theme.colors;
        let dismiss = self.on_dismiss;
        div()
            .id(self.id)
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .bg(gpui_color(colors.surface_overlay))
            .on_any_mouse_down(move |_, window, cx| {
                dismiss(window, cx);
                cx.stop_propagation();
            })
            .child(
                div()
                    .id("modal_panel")
                    .w(self.width)
                    .max_w(relative(1.0))
                    .map(|panel| match self.max_height {
                        Some(height) => panel.max_h(height),
                        None => panel.max_h(relative(1.0)),
                    })
                    .flex()
                    .flex_col()
                    .rounded(px(f32::from(self.radius.logical_pixels())))
                    .border(px(self.theme.dimensions.border_width))
                    .border_color(gpui_color(colors.border_secondary))
                    .bg(gpui_color(colors.surface))
                    .shadow_lg()
                    .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                    .children(self.children),
            )
    }
}

/// Standard header row for a modal: a title, an optional muted subtitle and the close control.
pub(super) fn modal_header(
    close_id: &'static str,
    title: impl Into<SharedString>,
    subtitle: Option<AnyElement>,
    theme: &AerisTheme,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) -> Div {
    let colors = theme.colors;
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .gap_2()
        .px_3()
        .py_2()
        .border_b(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border_secondary))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .text_color(gpui_color(colors.text_primary))
                        .child(title.into()),
                )
                .children(subtitle.map(|subtitle| {
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.text_muted))
                        .child(subtitle)
                })),
        )
        .child(close_button(close_id, theme, on_close))
}

/// Standard footer row for a modal; callers add the action buttons.
pub(super) fn modal_footer(theme: &AerisTheme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_end()
        .gap_2()
        .px_3()
        .py_2()
        .border_t(px(theme.dimensions.border_width))
        .border_color(gpui_color(theme.colors.border_secondary))
}

/// Which kind of action the confirm button commits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConfirmationTone {
    /// Irreversible removal; uses the `danger` button.
    Destructive,
    /// Creating or committing something; uses the positive button.
    Positive,
}

impl ConfirmationTone {
    const fn button_variant(self) -> ButtonVariant {
        match self {
            Self::Destructive => ButtonVariant::Destructive,
            Self::Positive => ButtonVariant::Positive,
        }
    }
}

/// The shared confirmation dialog: a title, an optional message and form body, Cancel, and one
/// confirm button styled by its [`ConfirmationTone`]. Cancel, the close control and the scrim all
/// run the same cancel handler.
#[derive(IntoElement)]
pub(super) struct ConfirmationDialog {
    id: &'static str,
    title: SharedString,
    message: Option<SharedString>,
    tone: ConfirmationTone,
    confirm_label: SharedString,
    theme: AerisTheme,
    on_cancel: ModalHandler,
    on_confirm: ModalHandler,
    children: Vec<AnyElement>,
}

impl ConfirmationDialog {
    pub(super) fn new(
        id: &'static str,
        title: impl Into<SharedString>,
        tone: ConfirmationTone,
        theme: &AerisTheme,
        on_cancel: impl Fn(&mut Window, &mut App) + 'static,
        on_confirm: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            message: None,
            tone,
            confirm_label: match tone {
                ConfirmationTone::Destructive => "Delete".into(),
                ConfirmationTone::Positive => "Confirm".into(),
            },
            theme: *theme,
            on_cancel: Rc::new(on_cancel),
            on_confirm: Rc::new(on_confirm),
            children: Vec::new(),
        }
    }

    pub(super) fn message(mut self, message: impl Into<SharedString>) -> Self {
        self.message = Some(message.into());
        self
    }

    pub(super) fn confirm_label(mut self, label: impl Into<SharedString>) -> Self {
        self.confirm_label = label.into();
        self
    }
}

impl ParentElement for ConfirmationDialog {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for ConfirmationDialog {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let theme = self.theme;
        let colors = theme.colors;
        let dismiss = self.on_cancel.clone();
        let close = self.on_cancel.clone();
        let cancel = self.on_cancel;
        let confirm = self.on_confirm;
        ModalLayer::new(
            self.id,
            px(CONFIRMATION_DIALOG_WIDTH),
            &theme,
            move |window, cx| dismiss(window, cx),
        )
        .child(modal_header(
            "confirmation_dialog_close",
            self.title,
            None,
            &theme,
            move |window, cx| close(window, cx),
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .children(self.message.map(|message| {
                    div()
                        .text_sm()
                        .text_color(gpui_color(colors.text_secondary))
                        .child(message)
                }))
                .children(self.children),
        )
        .child(
            modal_footer(&theme)
                .child(
                    Button::new("confirmation_dialog_cancel", &theme)
                        .variant(ButtonVariant::Secondary)
                        .label("Cancel")
                        .on_click(move |_, window, cx| cancel(window, cx)),
                )
                .child(
                    Button::new("confirmation_dialog_confirm", &theme)
                        .variant(self.tone.button_variant())
                        .label(self.confirm_label)
                        .on_click(move |_, window, cx| confirm(window, cx)),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{AerisTheme, Button, ButtonVariant, ConfirmationTone, modal_footer};
    use gpui::{
        Context, InteractiveElement, IntoElement, ParentElement, Render, TestAppContext, Window,
    };

    struct FooterHarness(AerisTheme);

    impl Render for FooterHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            modal_footer(&self.0)
                .child(
                    Button::new("cancel", &self.0)
                        .variant(ButtonVariant::Secondary)
                        .label("Cancel")
                        .on_click(|_, _, _| {})
                        .debug_selector(|| "footer_cancel".into()),
                )
                .child(
                    Button::new("delete", &self.0)
                        .variant(ButtonVariant::Destructive)
                        .label("Delete")
                        .on_click(|_, _, _| {})
                        .debug_selector(|| "footer_delete".into()),
                )
        }
    }

    #[gpui::test]
    fn confirmation_actions_share_one_height(cx: &mut TestAppContext) {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let (_, cx) = cx.add_window_view(move |_, cx| {
                gpui_base::init(cx);
                FooterHarness(theme)
            });
            cx.run_until_parked();
            let cancel = cx.debug_bounds("footer_cancel").expect("cancel bounds");
            let delete = cx.debug_bounds("footer_delete").expect("delete bounds");
            assert_eq!(cancel.size.height, gpui::px(28.0));
            assert_eq!(cancel.size.height, delete.size.height);
            assert_eq!(cancel.origin.y, delete.origin.y);
        }
    }

    #[test]
    fn confirmation_tones_use_destructive_and_positive_buttons() {
        assert_eq!(
            ConfirmationTone::Destructive.button_variant(),
            ButtonVariant::Destructive
        );
        assert_eq!(
            ConfirmationTone::Positive.button_variant(),
            ButtonVariant::Positive
        );
    }
}
