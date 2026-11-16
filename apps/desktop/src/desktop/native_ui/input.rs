use aeris_design_system::{AerisTheme, RadiusToken, TypographyRole, platform_font_family};
use gpui::{
    App, BoxShadow, Entity, Focusable, Hsla, IntoElement, Pixels, RenderOnce, Window, div,
    prelude::*, px,
};
use gpui_base::input::{Input as BaseInput, InputEditorStyle, Textarea as BaseTextarea};

pub(crate) use gpui_base::input::{InputEvent, InputState, TextareaState};

use super::{
    platform_font_weight,
    theme::{gpui_color, input_appearance, platform_border_width},
};

/// The invalid halo, Theme System `Input` `aria-invalid:ring-[3px]` in `danger-ring`.
const INVALID_RING_WIDTH: f32 = 3.0;

/// `Aeris`'s presentation wrapper around `gpui-base`'s single-line editing
/// engine. Base owns text editing, selection, IME, clipboard, focus, keyboard,
/// and accessibility behavior; `Aeris` owns every visual decision here.
#[derive(IntoElement)]
pub(crate) struct Input {
    state: Entity<InputState>,
    presentation: u8,
    fill: Option<Hsla>,
    /// Field fill on hover and while focused.
    hover_fill: Option<Hsla>,
    border_color: Option<Hsla>,
    focus_ring_color: Option<Hsla>,
    invalid_border_color: Option<Hsla>,
    invalid_ring_color: Option<Hsla>,
    muted_text_color: Option<Hsla>,
    border_width: Option<Pixels>,
}

impl Input {
    const APPEARANCE: u8 = 1 << 0;
    const BORDERED: u8 = 1 << 1;
    const FOCUS_BORDERED: u8 = 1 << 2;
    const GROW: u8 = 1 << 3;
    const INVALID: u8 = 1 << 4;

    pub(crate) fn new(state: &Entity<InputState>) -> Self {
        Self {
            state: state.clone(),
            presentation: Self::APPEARANCE | Self::BORDERED | Self::FOCUS_BORDERED,
            fill: None,
            hover_fill: None,
            border_color: None,
            focus_ring_color: None,
            invalid_border_color: None,
            invalid_ring_color: None,
            muted_text_color: None,
            border_width: None,
        }
    }

    /// Applies the Theme System text-field tokens for `theme`.
    pub(crate) fn platform(mut self, theme: &AerisTheme) -> Self {
        let appearance = input_appearance(theme);
        self.fill = Some(gpui_color(appearance.fill));
        self.hover_fill = Some(gpui_color(appearance.hover_fill));
        self.border_color = Some(gpui_color(appearance.border));
        self.focus_ring_color = Some(gpui_color(appearance.focus_ring));
        self.invalid_border_color = Some(gpui_color(appearance.invalid_border));
        self.invalid_ring_color = Some(gpui_color(appearance.invalid_ring));
        self.muted_text_color = Some(gpui_color(theme.colors.text_secondary));
        self.border_width = Some(platform_border_width(theme));
        self
    }

    pub(crate) fn appearance(mut self, appearance: bool) -> Self {
        self.set_presentation(Self::APPEARANCE, appearance);
        self
    }

    pub(crate) fn bordered(mut self, bordered: bool) -> Self {
        self.set_presentation(Self::BORDERED, bordered);
        self
    }

    pub(crate) fn focus_bordered(mut self, focus_bordered: bool) -> Self {
        self.set_presentation(Self::FOCUS_BORDERED, focus_bordered);
        self
    }

    /// Marks the value as rejected: the border uses `danger` and the halo uses `danger-ring`,
    /// focused or not, until the owner clears the flag.
    pub(crate) fn invalid(mut self, invalid: bool) -> Self {
        self.set_presentation(Self::INVALID, invalid);
        self
    }

    pub(crate) fn flex_1(mut self) -> Self {
        self.set_presentation(Self::GROW, true);
        self
    }

    fn set_presentation(&mut self, flag: u8, enabled: bool) {
        if enabled {
            self.presentation |= flag;
        } else {
            self.presentation &= !flag;
        }
    }

    const fn has_presentation(&self, flag: u8) -> bool {
        self.presentation & flag != 0
    }
}

impl RenderOnce for Input {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focused = self.state.read(cx).focus_handle(cx).is_focused(window);
        let color = window.text_style().color;
        let invalid = self.has_presentation(Self::INVALID);
        let focus_shown = focused && self.has_presentation(Self::FOCUS_BORDERED);
        let resting_border = self.border_color.unwrap_or_else(|| color.opacity(0.25));
        // Focus recolours the border itself, so the focus outline is exactly the 1px border.
        let border_color = if invalid {
            self.invalid_border_color.unwrap_or(resting_border)
        } else if focus_shown {
            self.focus_ring_color.unwrap_or_else(|| color.opacity(0.65))
        } else {
            resting_border
        };
        let ring = if invalid {
            self.invalid_ring_color
                .map(|ring_color| (ring_color, INVALID_RING_WIDTH))
        } else {
            None
        };
        let fill = self.fill;
        let hover_fill = self.hover_fill;
        let muted_text_color = self.muted_text_color.unwrap_or_else(|| color.opacity(0.74));

        self.state.update(cx, |state, _| {
            state.set_editor_style(InputEditorStyle {
                foreground: color,
                muted_foreground: muted_text_color,
                selection: color.opacity(0.22),
                caret: color,
            });
        });

        div()
            .h_full()
            .min_w(px(0.0))
            .flex()
            .items_center()
            .font_family(platform_font_family())
            .font_weight(platform_font_weight(TypographyRole::Normal))
            .when(self.has_presentation(Self::GROW), gpui::Styled::flex_1)
            .when(self.has_presentation(Self::APPEARANCE), gpui::Styled::px_2)
            .when_some(fill, gpui::Styled::bg)
            .when_some(hover_fill, |element, hover_fill| {
                element.hover(move |style| style.bg(hover_fill))
            })
            .when(self.has_presentation(Self::BORDERED), |element| {
                element
                    .border(self.border_width.unwrap_or(px(1.0)))
                    .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                    .border_color(border_color)
            })
            .when_some(ring, |element, (ring_color, width)| {
                element.shadow(vec![
                    BoxShadow::new(px(0.0), px(0.0), ring_color).spread_radius(px(width)),
                ])
            })
            .child(BaseInput::new(&self.state))
    }
}

/// The multi-line text field: the Theme System text-field tokens of [`Input`] around
/// `gpui-base`'s textarea engine, `height` tall with the text starting at the top.
#[derive(IntoElement)]
pub(crate) struct TextArea {
    state: Entity<TextareaState>,
    theme: AerisTheme,
    height: Pixels,
}

impl TextArea {
    pub(crate) fn new(state: &Entity<TextareaState>, height: Pixels, theme: &AerisTheme) -> Self {
        Self {
            state: state.clone(),
            theme: *theme,
            height,
        }
    }
}

impl RenderOnce for TextArea {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let appearance = input_appearance(&self.theme);
        let focused = self.state.read(cx).focus_handle(cx).is_focused(window);
        let color = window.text_style().color;
        let border = gpui_color(if focused {
            appearance.focus_ring
        } else {
            appearance.border
        });
        let hover_fill = gpui_color(appearance.hover_fill);
        self.state.update(cx, |state, _| {
            state.set_editor_style(InputEditorStyle {
                foreground: color,
                muted_foreground: gpui_color(self.theme.colors.text_secondary),
                selection: color.opacity(0.22),
                caret: color,
            });
        });
        div()
            .w_full()
            .h(self.height)
            .flex()
            .px_2()
            .py_1p5()
            .font_family(platform_font_family())
            .font_weight(platform_font_weight(TypographyRole::Normal))
            .bg(gpui_color(appearance.fill))
            .hover(move |style| style.bg(hover_fill))
            .border(platform_border_width(&self.theme))
            .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
            .border_color(border)
            .child(BaseTextarea::new(&self.state))
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use aeris_design_system::AerisTheme;
    use gpui::{
        AppContext, Context, Entity, IntoElement, Modifiers, ParentElement, Pixels, Render, Styled,
        TestAppContext, Window, canvas, div, point, px,
    };

    use super::{Input, InputState};

    /// A field followed by a probe that, painting after it, records where each frame put the text.
    struct Field {
        state: Entity<InputState>,
        painted_tops: Rc<RefCell<Vec<Pixels>>>,
    }

    impl Render for Field {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let state = self.state.clone();
            let painted_tops = self.painted_tops.clone();
            div()
                .size_full()
                .p_4()
                .child(
                    div().w(px(160.0)).h(px(28.0)).flex().child(
                        Input::new(&self.state)
                            .platform(&AerisTheme::dark())
                            .flex_1(),
                    ),
                )
                .child(canvas(
                    |_, _, _| {},
                    move |_, (), _, cx| {
                        if let Some(text) = state.read(cx).text_bounds() {
                            painted_tops.borrow_mut().push(text.origin.y);
                        }
                    },
                ))
        }
    }

    #[gpui::test]
    fn clicking_a_field_never_paints_its_text_off_its_line(cx: &mut TestAppContext) {
        let painted_tops: Rc<RefCell<Vec<Pixels>>> = Rc::default();
        let field_tops = painted_tops.clone();
        let (view, cx) = cx.add_window_view(move |window, cx| {
            gpui_base::init(cx);
            let state = cx.new(|cx| InputState::new(window, cx));
            state.update(cx, |state, cx| state.set_value("25", window, cx));
            Field {
                state,
                painted_tops: field_tops,
            }
        });
        // At 125% the text box rounds to whole device pixels a fraction below its line height.
        cx.simulate_scale_factor_change(1.25);
        cx.run_until_parked();
        let state = cx.read(|cx| view.read(cx).state.clone());
        let (field, text, line_height) = cx.read(|cx| {
            let state = state.read(cx);
            (
                state.input_bounds(),
                state.text_bounds().expect("the field painted"),
                state.line_height().expect("the field laid out"),
            )
        });
        assert!(
            field.size.height < line_height,
            "the scale must round the box below a line"
        );

        painted_tops.borrow_mut().clear();
        cx.simulate_click(
            point(text.origin.x + px(1.0), text.center().y),
            Modifiers::none(),
        );
        cx.run_until_parked();

        let painted_tops = painted_tops.borrow();
        assert!(!painted_tops.is_empty(), "the click repainted the field");
        assert!(
            painted_tops.iter().all(|top| *top == field.origin.y),
            "every painted frame keeps the text on its line: {painted_tops:?} vs {:?}",
            field.origin.y
        );
    }
}
