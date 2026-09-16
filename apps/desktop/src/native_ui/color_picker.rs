use std::rc::Rc;

use axiusflow_design_system::{AxiusflowTheme, RadiusToken, TypographyRole};
use gpui::{
    App, ElementId, Entity, Hsla, IntoElement, RenderOnce, SharedString, Window, div, hsla,
    prelude::*, px,
};
use num_traits::ToPrimitive;

use super::{
    control::Button,
    input::{Input, InputState},
    platform_font_weight,
    theme::{gpui_color, platform_border_width},
};

type SelectionHandler = Rc<dyn Fn(String, &mut Window, &mut App)>;
const LIGHTNESSES: [f32; 4] = [0.82, 0.67, 0.52, 0.37];
const HUES: [f32; 12] = [
    0.0,
    1.0 / 12.0,
    2.0 / 12.0,
    3.0 / 12.0,
    4.0 / 12.0,
    5.0 / 12.0,
    6.0 / 12.0,
    7.0 / 12.0,
    8.0 / 12.0,
    9.0 / 12.0,
    10.0 / 12.0,
    11.0 / 12.0,
];
const GRAYSCALE: [f32; 12] = [
    1.0, 0.91, 0.82, 0.73, 0.64, 0.55, 0.45, 0.36, 0.27, 0.18, 0.09, 0.0,
];

/// Axiusflow-native RGB color popover with a generated color field and exact hex entry.
#[derive(IntoElement)]
pub(crate) struct ColorPicker {
    id: ElementId,
    current: String,
    input: Entity<InputState>,
    error: Option<SharedString>,
    theme: AxiusflowTheme,
    on_select: Option<SelectionHandler>,
}

impl ColorPicker {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        current: impl Into<String>,
        input: &Entity<InputState>,
        theme: &AxiusflowTheme,
    ) -> Self {
        Self {
            id: id.into(),
            current: current.into(),
            input: input.clone(),
            error: None,
            theme: *theme,
            on_select: None,
        }
    }

    pub(crate) fn error(mut self, error: Option<&str>) -> Self {
        self.error = error.map(SharedString::from);
        self
    }

    pub(crate) fn on_select(
        mut self,
        handler: impl Fn(String, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ColorPicker {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let colors = self.theme.colors;
        let normalized_current = normalize_hex_color(&self.current);
        let handler = self.on_select;
        let field = color_field(normalized_current.as_deref(), handler.as_ref(), &self.theme);
        let grayscale =
            grayscale_field(normalized_current.as_deref(), handler.as_ref(), &self.theme);

        let submit_handler = handler.clone();
        let submit_input = self.input.clone();
        div()
            .id(self.id)
            .absolute()
            .top(px(34.0))
            .right_0()
            .w(px(252.0))
            .p_3()
            .flex()
            .flex_col()
            .gap_3()
            .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
            .border(platform_border_width(&self.theme))
            .border_color(gpui_color(colors.border_secondary))
            .bg(gpui_color(colors.surface))
            .occlude()
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .child(field)
            .child(grayscale)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(platform_font_weight(TypographyRole::Emphasis))
                            .text_color(gpui_color(colors.text_secondary))
                            .child("HEX"),
                    )
                    .child(Input::new(&self.input).platform(&self.theme).flex_1())
                    .child(
                        Button::new("native_color_apply")
                            .theme(&self.theme)
                            .resting_fill(colors.surface_secondary)
                            .label("Apply")
                            .h(px(30.0))
                            .on_click(move |_, window, cx| {
                                if let Some(handler) = &submit_handler {
                                    handler(submit_input.read(cx).value().to_string(), window, cx);
                                }
                            }),
                    ),
            )
            .children(self.error.map(|error| {
                div()
                    .text_xs()
                    .text_color(gpui_color(colors.danger))
                    .child(error)
            }))
    }
}

fn color_field(
    current: Option<&str>,
    handler: Option<&SelectionHandler>,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let mut field = div().flex().flex_col().gap(px(2.0));
    for (row, lightness) in LIGHTNESSES.into_iter().enumerate() {
        let mut spectrum_row = div().flex().gap(px(2.0));
        for (column, hue) in HUES.into_iter().enumerate() {
            let color = hsla(hue, 0.82, lightness, 1.0);
            let hex = hsla_hex(color);
            spectrum_row = spectrum_row.child(color_cell(
                ("native_color_spectrum", row * 12 + column),
                color,
                current == Some(hex.as_str()),
                hex,
                handler.cloned(),
                theme,
            ));
        }
        field = field.child(spectrum_row);
    }
    field
}

fn grayscale_field(
    current: Option<&str>,
    handler: Option<&SelectionHandler>,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let mut grayscale = div().flex().gap(px(2.0));
    for (index, lightness) in GRAYSCALE.into_iter().enumerate() {
        let color = hsla(0.0, 0.0, lightness, 1.0);
        let hex = hsla_hex(color);
        grayscale = grayscale.child(color_cell(
            ("native_color_grayscale", index),
            color,
            current == Some(hex.as_str()),
            hex,
            handler.cloned(),
            theme,
        ));
    }
    grayscale
}

fn color_cell(
    id: impl Into<ElementId>,
    color: Hsla,
    selected: bool,
    hex: String,
    handler: Option<SelectionHandler>,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    div()
        .id(id.into())
        .size(px(17.0))
        .p(px(if selected { 2.0 } else { 1.0 }))
        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
        .border(platform_border_width(theme))
        .border_color(gpui_color(if selected {
            colors.primary
        } else {
            colors.input_border
        }))
        .cursor_pointer()
        .hover(move |cell| cell.border_color(gpui_color(colors.ring)))
        .on_click(move |_, window, cx| {
            if let Some(handler) = &handler {
                handler(hex.clone(), window, cx);
            }
        })
        .child(
            div()
                .size_full()
                .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                .bg(color),
        )
}

pub(crate) fn normalize_hex_color(value: &str) -> Option<String> {
    let value = value.trim().strip_prefix('#').unwrap_or(value.trim());
    let expanded = match value.len() {
        3 if value.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
            let mut result = String::with_capacity(6);
            for character in value.chars() {
                result.push(character);
                result.push(character);
            }
            result
        }
        6 if value.bytes().all(|byte| byte.is_ascii_hexdigit()) => value.to_string(),
        _ => return None,
    };
    Some(format!("#{}", expanded.to_ascii_uppercase()))
}

fn hsla_hex(color: Hsla) -> String {
    let rgb = color.to_rgb();
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round().to_u8().unwrap_or(0);
    format!(
        "#{:02X}{:02X}{:02X}",
        channel(rgb.r),
        channel(rgb.g),
        channel(rgb.b)
    )
}

#[cfg(test)]
mod tests {
    use super::normalize_hex_color;

    #[test]
    fn hex_input_accepts_exact_and_shorthand_rgb_only() {
        assert_eq!(normalize_hex_color(" #09aBcD ").as_deref(), Some("#09ABCD"));
        assert_eq!(normalize_hex_color("#0af").as_deref(), Some("#00AAFF"));
        assert_eq!(normalize_hex_color("#12345678"), None);
        assert_eq!(normalize_hex_color("blue"), None);
    }
}
