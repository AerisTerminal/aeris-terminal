//! `Aeris`'s one modal dialog.
//!
//! A dialog covers the nearest positioned ancestor with the `surface-overlay` scrim and holds one
//! panel: an optional header (a title with the shared close control, or a custom header such as
//! a search field), a body, and an optional footer with a leading slot and right-aligned actions.
//! Pressing the scrim or the close control runs the dismiss handler; presses inside the panel
//! never reach the surfaces underneath. Callers fill the slots; they never restyle the scrim,
//! panel, header or footer.

use std::rc::Rc;

use aeris_design_system::{AerisTheme, RadiusToken, TypographyRole};
use gpui::{
    AnyElement, App, Div, ElementId, IntoElement, ParentElement, Pixels, RenderOnce, SharedString,
    Window, div, prelude::*, px, relative,
};

use super::{
    button::{Button, close_button},
    platform_font_weight,
    theme::{gpui_color, platform_border_width},
};

type DialogHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The three dialog widths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DialogSize {
    /// 420 px: confirmations and short forms.
    Sm,
    /// 480 px: settings forms.
    Md,
    /// 560 px: search palettes.
    Lg,
}

impl DialogSize {
    const fn width(self) -> Pixels {
        px(match self {
            Self::Sm => 420.0,
            Self::Md => 480.0,
            Self::Lg => 560.0,
        })
    }
}

/// Where the panel sits in the scrim.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum DialogAlign {
    #[default]
    Center,
    /// Near the top, so a palette's list grows downward without moving its search field.
    Top,
}

/// Space above a top-aligned dialog.
const TOP_ALIGNED_OFFSET: Pixels = px(96.0);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum DialogBody {
    /// Padded form content.
    #[default]
    Form,
    /// A tightly inset list of rows.
    List,
}

enum DialogHeader {
    Title {
        title: SharedString,
        subtitle: Option<AnyElement>,
    },
    Custom(AnyElement),
}

#[derive(IntoElement)]
pub(crate) struct Dialog {
    id: ElementId,
    size: DialogSize,
    align: DialogAlign,
    body: DialogBody,
    max_height: Option<Pixels>,
    theme: AerisTheme,
    on_dismiss: DialogHandler,
    header: Option<DialogHeader>,
    children: Vec<AnyElement>,
    footer_leading: Option<AnyElement>,
    actions: Vec<AnyElement>,
}

impl Dialog {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        size: DialogSize,
        theme: &AerisTheme,
        on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            size,
            align: DialogAlign::default(),
            body: DialogBody::default(),
            max_height: None,
            theme: *theme,
            on_dismiss: Rc::new(on_dismiss),
            header: None,
            children: Vec::new(),
            footer_leading: None,
            actions: Vec::new(),
        }
    }

    /// The standard header: this title, a muted subtitle and the close control.
    pub(crate) fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.header = Some(DialogHeader::Title {
            title: title.into(),
            subtitle: None,
        });
        self
    }

    pub(crate) fn subtitle(mut self, subtitle: impl IntoElement) -> Self {
        if let Some(DialogHeader::Title { subtitle: slot, .. }) = self.header.as_mut() {
            *slot = Some(subtitle.into_any_element());
        }
        self
    }

    /// A header of the caller's own, such as a palette's search field, in place of the title.
    pub(crate) fn header(mut self, header: impl IntoElement) -> Self {
        self.header = Some(DialogHeader::Custom(header.into_any_element()));
        self
    }

    pub(crate) fn align(mut self, align: DialogAlign) -> Self {
        self.align = align;
        self
    }

    /// Lays the body out as a list of rows instead of a padded form.
    pub(crate) fn list_body(mut self) -> Self {
        self.body = DialogBody::List;
        self
    }

    /// Caps the panel height; without it the panel may grow to the full scrim height.
    pub(crate) fn max_height(mut self, height: Pixels) -> Self {
        self.max_height = Some(height);
        self
    }

    /// Footer content at the leading edge, such as a reset action or a keyboard hint.
    pub(crate) fn footer_leading(mut self, element: impl IntoElement) -> Self {
        self.footer_leading = Some(element.into_any_element());
        self
    }

    /// A footer action. Actions sit at the trailing edge in the order added: Cancel first, then
    /// the action that commits.
    pub(crate) fn action(mut self, action: Button) -> Self {
        self.actions.push(action.into_any_element());
        self
    }
}

impl ParentElement for Dialog {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

fn title_header(
    title: SharedString,
    subtitle: Option<AnyElement>,
    theme: &AerisTheme,
    on_close: DialogHandler,
) -> Div {
    let colors = theme.colors;
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .gap_2()
        .px_4()
        .py_3()
        .border_b(platform_border_width(theme))
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
                        .child(title),
                )
                .children(subtitle.map(|subtitle| {
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.text_muted))
                        .child(subtitle)
                })),
        )
        .child(close_button("dialog_close", theme, move |window, cx| {
            on_close(window, cx);
        }))
}

fn custom_header(header: AnyElement, theme: &AerisTheme) -> Div {
    div()
        .flex_none()
        .p_3()
        .border_b(platform_border_width(theme))
        .border_color(gpui_color(theme.colors.border_secondary))
        .child(header)
}

fn footer(leading: Option<AnyElement>, actions: Vec<AnyElement>, theme: &AerisTheme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .gap_2()
        .px_4()
        .py_3()
        .border_t(platform_border_width(theme))
        .border_color(gpui_color(theme.colors.border_secondary))
        .text_xs()
        .text_color(gpui_color(theme.colors.text_muted))
        .child(div().min_w_0().children(leading))
        .child(div().flex().flex_none().gap_2().children(actions))
}

impl RenderOnce for Dialog {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let colors = self.theme.colors;
        let theme = self.theme;
        let dismiss = self.on_dismiss.clone();
        let header = self.header.map(|header| match header {
            DialogHeader::Title { title, subtitle } => {
                title_header(title, subtitle, &theme, self.on_dismiss.clone())
            }
            DialogHeader::Custom(header) => custom_header(header, &theme),
        });
        let has_footer = self.footer_leading.is_some() || !self.actions.is_empty();
        let footer = has_footer.then(|| footer(self.footer_leading, self.actions, &theme));
        let body = div()
            .id("dialog_body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .map(|body| match self.body {
                DialogBody::Form => body.gap_3().p_4(),
                DialogBody::List => body.gap_px().p_2(),
            })
            .children(self.children);
        div()
            .id(self.id)
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .justify_center()
            .p_4()
            .map(|scrim| match self.align {
                DialogAlign::Center => scrim.items_center(),
                DialogAlign::Top => scrim.items_start().pt(TOP_ALIGNED_OFFSET),
            })
            .bg(gpui_color(colors.surface_overlay))
            .on_any_mouse_down(move |_, window, cx| {
                dismiss(window, cx);
                cx.stop_propagation();
            })
            .child(
                div()
                    .id("dialog_panel")
                    .w(self.size.width())
                    .max_w(relative(1.0))
                    .map(|panel| match self.max_height {
                        Some(height) => panel.max_h(height),
                        None => panel.max_h(relative(1.0)),
                    })
                    .flex()
                    .flex_col()
                    .rounded(px(f32::from(RadiusToken::Medium.logical_pixels())))
                    .border(platform_border_width(&theme))
                    .border_color(gpui_color(colors.border_secondary))
                    .bg(gpui_color(colors.surface))
                    .shadow_lg()
                    .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                    .children(header)
                    .child(body)
                    .children(footer),
            )
    }
}

/// Which kind of action a confirmation commits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConfirmationTone {
    /// Irreversible removal; uses the `danger` button.
    Destructive,
    /// Creating or committing something; uses the positive button.
    Positive,
}

impl ConfirmationTone {
    const fn button_variant(self) -> super::button::ButtonVariant {
        match self {
            Self::Destructive => super::button::ButtonVariant::Destructive,
            Self::Positive => super::button::ButtonVariant::Positive,
        }
    }
}

/// The shared confirmation dialog: a title, an optional message and form body, Cancel, and one
/// confirm button styled by its [`ConfirmationTone`]. Cancel, the close control and the scrim all
/// run the same cancel handler.
#[derive(IntoElement)]
pub(crate) struct ConfirmationDialog {
    id: ElementId,
    title: SharedString,
    message: Option<SharedString>,
    tone: ConfirmationTone,
    confirm_label: SharedString,
    theme: AerisTheme,
    on_cancel: DialogHandler,
    on_confirm: DialogHandler,
    children: Vec<AnyElement>,
}

impl ConfirmationDialog {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        title: impl Into<SharedString>,
        tone: ConfirmationTone,
        theme: &AerisTheme,
        on_cancel: impl Fn(&mut Window, &mut App) + 'static,
        on_confirm: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
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

    pub(crate) fn message(mut self, message: impl Into<SharedString>) -> Self {
        self.message = Some(message.into());
        self
    }

    pub(crate) fn confirm_label(mut self, label: impl Into<SharedString>) -> Self {
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
        let dismiss = self.on_cancel.clone();
        let cancel = self.on_cancel;
        let confirm = self.on_confirm;
        Dialog::new(self.id, DialogSize::Sm, &theme, move |window, cx| {
            dismiss(window, cx);
        })
        .title(self.title)
        .children(self.message.map(|message| {
            div()
                .text_sm()
                .text_color(gpui_color(theme.colors.text_secondary))
                .child(message)
        }))
        .children(self.children)
        .action(
            Button::new("confirmation_dialog_cancel", &theme)
                .variant(super::button::ButtonVariant::Secondary)
                .label("Cancel")
                .on_click(move |_, window, cx| cancel(window, cx)),
        )
        .action(
            Button::new("confirmation_dialog_confirm", &theme)
                .variant(self.tone.button_variant())
                .label(self.confirm_label)
                .on_click(move |_, window, cx| confirm(window, cx)),
        )
    }
}

#[cfg(test)]
mod tests {
    use aeris_design_system::AerisTheme;
    use gpui::{
        Context, InteractiveElement, IntoElement, ParentElement, Render, Styled, TestAppContext,
        Window, div,
    };

    use super::{ConfirmationTone, Dialog, DialogSize};
    use crate::desktop::native_ui::button::{Button, ButtonVariant};

    struct DialogHarness(AerisTheme);

    impl Render for DialogHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                Dialog::new("harness_dialog", DialogSize::Sm, &self.0, |_, _| {})
                    .title("Delete")
                    .action(
                        Button::new("cancel", &self.0)
                            .variant(ButtonVariant::Secondary)
                            .label("Cancel")
                            .on_click(|_, _, _| {})
                            .debug_selector(|| "footer_cancel".into()),
                    )
                    .action(
                        Button::new("delete", &self.0)
                            .variant(ButtonVariant::Destructive)
                            .label("Delete")
                            .on_click(|_, _, _| {})
                            .debug_selector(|| "footer_delete".into()),
                    ),
            )
        }
    }

    #[gpui::test]
    fn footer_actions_share_one_height_and_row(cx: &mut TestAppContext) {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let (_, cx) = cx.add_window_view(move |_, cx| {
                gpui_base::init(cx);
                DialogHarness(theme)
            });
            cx.run_until_parked();
            let cancel = cx.debug_bounds("footer_cancel").expect("cancel bounds");
            let delete = cx.debug_bounds("footer_delete").expect("delete bounds");
            assert_eq!(cancel.size.height, gpui::px(28.0));
            assert_eq!(cancel.size.height, delete.size.height);
            assert_eq!(cancel.origin.y, delete.origin.y);
            assert!(cancel.origin.x < delete.origin.x, "Cancel comes first");
        }
    }

    #[test]
    fn dialog_sizes_follow_the_three_widths() {
        assert_eq!(DialogSize::Sm.width(), gpui::px(420.0));
        assert_eq!(DialogSize::Md.width(), gpui::px(480.0));
        assert_eq!(DialogSize::Lg.width(), gpui::px(560.0));
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
