use asceify_design_system::{AsceifyTheme, RadiusToken, TypographyRole, platform_font_family};
use gpui::{
    App, BoxShadow, Entity, Focusable, Hsla, IntoElement, Pixels, RenderOnce, Window, div,
    prelude::*, px,
};
use gpui_base::input::{Input as BaseInput, InputEditorStyle};

pub(crate) use gpui_base::input::{InputEvent, InputState};

use super::{
    platform_font_weight,
    theme::{gpui_color, input_appearance, platform_border_width},
};

/// `Asceify`'s presentation wrapper around `gpui-base`'s single-line editing
/// engine. Base owns text editing, selection, IME, clipboard, focus, keyboard,
/// and accessibility behavior; `Asceify` owns every visual decision here.
#[derive(IntoElement)]
pub(crate) struct Input {
    state: Entity<InputState>,
    presentation: u8,
    fill: Option<Hsla>,
    border_color: Option<Hsla>,
    focus_ring_color: Option<Hsla>,
    muted_text_color: Option<Hsla>,
    border_width: Option<Pixels>,
}

impl Input {
    const APPEARANCE: u8 = 1 << 0;
    const BORDERED: u8 = 1 << 1;
    const FOCUS_BORDERED: u8 = 1 << 2;
    const GROW: u8 = 1 << 3;
    const THICK_BORDER: u8 = 1 << 4;

    pub(crate) fn new(state: &Entity<InputState>) -> Self {
        Self {
            state: state.clone(),
            presentation: Self::APPEARANCE | Self::BORDERED | Self::FOCUS_BORDERED,
            fill: None,
            border_color: None,
            focus_ring_color: None,
            muted_text_color: None,
            border_width: None,
        }
    }

    pub(crate) fn platform(mut self, theme: &AsceifyTheme) -> Self {
        let (fill, border, focus) = input_appearance(theme);
        self.fill = Some(gpui_color(fill));
        self.border_color = Some(gpui_color(border));
        self.focus_ring_color = Some(gpui_color(focus));
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

    pub(crate) fn thick_border(mut self, thick_border: bool) -> Self {
        self.set_presentation(Self::THICK_BORDER, thick_border);
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
        let border_color = self.border_color.unwrap_or_else(|| color.opacity(0.25));
        let focus_ring_color = self.focus_ring_color.unwrap_or_else(|| color.opacity(0.65));
        let muted_text_color = self.muted_text_color.unwrap_or_else(|| color.opacity(0.74));

        self.state.update(cx, |state, _| {
            state.set_editor_style(InputEditorStyle {
                foreground: color,
                muted_foreground: muted_text_color,
                border: border_color,
                selection: color.opacity(0.22),
                caret: color,
                ..InputEditorStyle::default()
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
            .when_some(self.fill, gpui::Styled::bg)
            .when(self.has_presentation(Self::BORDERED), |element| {
                element
                    .when(
                        self.has_presentation(Self::THICK_BORDER),
                        gpui::Styled::border_2,
                    )
                    .when(!self.has_presentation(Self::THICK_BORDER), |element| {
                        element.border(self.border_width.unwrap_or(px(1.0)))
                    })
                    .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                    .border_color(border_color)
            })
            .when(
                self.has_presentation(Self::FOCUS_BORDERED) && focused,
                |element| {
                    element.shadow(vec![
                        BoxShadow::new(px(0.0), px(0.0), focus_ring_color).spread_radius(px(2.0)),
                    ])
                },
            )
            .child(BaseInput::new(&self.state))
    }
}
