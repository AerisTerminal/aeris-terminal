use aeris_design_system::{AerisTheme, RadiusToken, TypographyRole, platform_font_family};
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

/// The shared focus outline, `outline: 2px solid var(--ring-primary)` at offset 0. GPUI has no
/// outline, so it is drawn as a spread shadow hugging the border.
const FOCUS_RING_WIDTH: f32 = 2.0;
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
        // Focus keeps the resting border and adds the ring; only an invalid value recolours it.
        let border_color = if invalid {
            self.invalid_border_color.unwrap_or(resting_border)
        } else {
            resting_border
        };
        let ring = if invalid {
            self.invalid_ring_color
                .map(|ring_color| (ring_color, INVALID_RING_WIDTH))
        } else if focus_shown {
            Some((
                self.focus_ring_color.unwrap_or_else(|| color.opacity(0.65)),
                FOCUS_RING_WIDTH,
            ))
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
