use std::rc::Rc;

use aeris_design_system::{AerisTheme, RadiusToken, TypographyRole};
use gpui::{
    App, ElementId, Entity, Hsla, IntoElement, MouseButton, RenderOnce, Rgba, SharedString, Window,
    checkerboard, div, hsla, linear_color_stop, linear_gradient, prelude::*, px,
};
use num_traits::ToPrimitive;

use super::{
    input::{Input, InputState},
    menu::{PopupAnimationOrigin, animate_popup_from_origin},
    platform_font_weight,
    theme::{gpui_color, platform_border_width},
};

type SelectionHandler = Rc<dyn Fn(String, &mut Window, &mut App)>;

const FIELD_WIDTH: f32 = 280.0;
const FIELD_HEIGHT: f32 = 190.0;
const FIELD_COLUMNS: u16 = 20;
const FIELD_ROWS: u16 = 14;
const SLIDER_STEPS: u16 = 36;
const RECOMMENDED_COLORS: [&str; 8] = [
    "#6B7280", "#335CFF", "#FF7A45", "#FB3748", "#18B66A", "#F5A623", "#7C4DFF", "#45B8F2",
];

#[derive(Clone, Copy, Debug, PartialEq)]
struct Hsva {
    hue: f32,
    saturation: f32,
    value: f32,
    alpha: f32,
}

/// Reusable native color picker with continuous color fields and exact hex entry.
///
/// The hit targets are an invisible regular lattice over smooth GPUI gradients. This keeps the
/// control crisp at every scale while avoiding a custom renderer or a collection of bordered color
/// buttons. All selection paths emit the same normalized CSS hex value.
#[derive(IntoElement)]
pub(crate) struct ColorPicker {
    id: ElementId,
    current: String,
    input: Entity<InputState>,
    error: Option<SharedString>,
    theme: AerisTheme,
    on_select: Option<SelectionHandler>,
}

impl ColorPicker {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        current: impl Into<String>,
        input: &Entity<InputState>,
        theme: &AerisTheme,
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
        let animation_id = self.id.clone();
        let normalized = normalize_hex_color(&self.current).unwrap_or_else(|| "#335CFF".into());
        let selected = parse_hex_color(&normalized).unwrap_or(Hsva {
            hue: 0.63,
            saturation: 0.8,
            value: 1.0,
            alpha: 1.0,
        });
        let handler = self.on_select;

        let panel = div()
            .id(self.id)
            .absolute()
            .top(px(34.0))
            .right_0()
            .w(px(320.0))
            .flex()
            .flex_col()
            .rounded(px(f32::from(RadiusToken::Default.logical_pixels()) + 4.0))
            .border(platform_border_width(&self.theme))
            .border_color(gpui_color(colors.border_secondary))
            .bg(gpui_color(colors.surface))
            .shadow_lg()
            .occlude()
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(saturation_value_field(
                        selected,
                        &self.input,
                        handler.as_ref(),
                        &self.theme,
                    ))
                    .child(hue_slider(
                        selected,
                        &self.input,
                        handler.as_ref(),
                        &self.theme,
                    ))
                    .child(alpha_slider(
                        selected,
                        &self.input,
                        handler.as_ref(),
                        &self.theme,
                    ))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(platform_font_weight(TypographyRole::Emphasis))
                            .text_color(gpui_color(colors.text_primary))
                            .child("HEX"),
                    )
                    .child(hex_input_row(
                        &self.input,
                        selected,
                        handler.as_ref(),
                        &self.theme,
                    ))
                    .children(self.error.map(|error| {
                        div()
                            .text_xs()
                            .text_color(gpui_color(colors.danger))
                            .child(error)
                    })),
            )
            .child(
                div()
                    .border_t(platform_border_width(&self.theme))
                    .border_color(gpui_color(colors.border_secondary))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(gpui_color(colors.text_secondary))
                            .child("Recommended colors"),
                    )
                    .child(recommended_colors(
                        &normalized,
                        &self.input,
                        handler.as_ref(),
                        &self.theme,
                    )),
            );
        animate_popup_from_origin(
            panel,
            (animation_id, "enter"),
            PopupAnimationOrigin::TOP_RIGHT,
        )
    }
}

fn saturation_value_field(
    selected: Hsva,
    input: &Entity<InputState>,
    handler: Option<&SelectionHandler>,
    theme: &AerisTheme,
) -> impl IntoElement {
    let pure_hue = hsla(selected.hue, 1.0, 0.5, 1.0);
    let transparent_white = hsla(0.0, 0.0, 1.0, 0.0);
    let transparent_black = hsla(0.0, 0.0, 0.0, 0.0);
    let mut hits = div().absolute().inset_0().flex().flex_col();
    for row in 0..FIELD_ROWS {
        let mut hit_row = div().flex().flex_1();
        for column in 0..FIELD_COLUMNS {
            let saturation = (f32::from(column) + 0.5) / f32::from(FIELD_COLUMNS);
            let value = 1.0 - (f32::from(row) + 0.5) / f32::from(FIELD_ROWS);
            let input = input.clone();
            let handler = handler.cloned();
            let drag_input = input.clone();
            let drag_handler = handler.clone();
            let selection = Hsva {
                saturation,
                value,
                ..selected
            };
            hit_row = hit_row.child(
                div()
                    .id((
                        "native_color_field",
                        usize::from(row * FIELD_COLUMNS + column),
                    ))
                    .flex_1()
                    .h_full()
                    .cursor_crosshair()
                    .on_mouse_move(move |event, window, cx| {
                        if event.pressed_button == Some(MouseButton::Left) {
                            emit_selection(
                                selection,
                                &drag_input,
                                drag_handler.as_ref(),
                                window,
                                cx,
                            );
                        }
                    })
                    .on_click(move |_, window, cx| {
                        emit_selection(selection, &input, handler.as_ref(), window, cx);
                    }),
            );
        }
        hits = hits.child(hit_row);
    }

    div()
        .relative()
        .w(px(FIELD_WIDTH))
        .h(px(FIELD_HEIGHT))
        .child(
            div()
                .absolute()
                .inset_0()
                .overflow_hidden()
                .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                .bg(pure_hue)
                .child(
                    div()
                        .size_full()
                        .bg(linear_gradient(
                            90.0,
                            linear_color_stop(gpui::white(), 0.0),
                            linear_color_stop(transparent_white, 1.0),
                        ))
                        .child(div().size_full().bg(linear_gradient(
                            180.0,
                            linear_color_stop(transparent_black, 0.0),
                            linear_color_stop(gpui::black(), 1.0),
                        ))),
                ),
        )
        .child(hits)
        .child(selection_handle(
            selected.saturation * FIELD_WIDTH,
            (1.0 - selected.value) * FIELD_HEIGHT,
            selected_color(selected),
            theme,
        ))
}

fn hue_slider(
    selected: Hsva,
    input: &Entity<InputState>,
    handler: Option<&SelectionHandler>,
    theme: &AerisTheme,
) -> impl IntoElement {
    let stops = [
        hsla(0.0, 1.0, 0.5, 1.0),
        hsla(1.0 / 6.0, 1.0, 0.5, 1.0),
        hsla(2.0 / 6.0, 1.0, 0.5, 1.0),
        hsla(3.0 / 6.0, 1.0, 0.5, 1.0),
        hsla(4.0 / 6.0, 1.0, 0.5, 1.0),
        hsla(5.0 / 6.0, 1.0, 0.5, 1.0),
        hsla(1.0, 1.0, 0.5, 1.0),
    ];
    let mut gradient = div()
        .absolute()
        .inset_0()
        .flex()
        .overflow_hidden()
        .rounded_full();
    for pair in stops.windows(2) {
        gradient = gradient.child(div().flex_1().h_full().bg(linear_gradient(
            90.0,
            linear_color_stop(pair[0], 0.0),
            linear_color_stop(pair[1], 1.0),
        )));
    }
    let mut hits = div().absolute().inset_0().flex();
    for step in 0..SLIDER_STEPS {
        let input = input.clone();
        let handler = handler.cloned();
        let drag_input = input.clone();
        let drag_handler = handler.clone();
        let selection = Hsva {
            hue: (f32::from(step) + 0.5) / f32::from(SLIDER_STEPS),
            ..selected
        };
        hits = hits.child(
            div()
                .id(("native_color_hue", usize::from(step)))
                .flex_1()
                .h_full()
                .cursor_pointer()
                .on_mouse_move(move |event, window, cx| {
                    if event.pressed_button == Some(MouseButton::Left) {
                        emit_selection(selection, &drag_input, drag_handler.as_ref(), window, cx);
                    }
                })
                .on_click(move |_, window, cx| {
                    emit_selection(selection, &input, handler.as_ref(), window, cx);
                }),
        );
    }
    slider_shell(theme)
        .child(gradient)
        .child(hits)
        .child(slider_handle(
            selected.hue * FIELD_WIDTH,
            selected_color(selected),
            theme,
        ))
}

fn alpha_slider(
    selected: Hsva,
    input: &Entity<InputState>,
    handler: Option<&SelectionHandler>,
    theme: &AerisTheme,
) -> impl IntoElement {
    let opaque = selected_color(Hsva {
        alpha: 1.0,
        ..selected
    });
    let transparent = Hsla { a: 0.0, ..opaque };
    let mut hits = div().absolute().inset_0().flex();
    for step in 0..SLIDER_STEPS {
        let input = input.clone();
        let handler = handler.cloned();
        let drag_input = input.clone();
        let drag_handler = handler.clone();
        let selection = Hsva {
            alpha: (f32::from(step) + 0.5) / f32::from(SLIDER_STEPS),
            ..selected
        };
        hits = hits.child(
            div()
                .id(("native_color_alpha", usize::from(step)))
                .flex_1()
                .h_full()
                .cursor_pointer()
                .on_mouse_move(move |event, window, cx| {
                    if event.pressed_button == Some(MouseButton::Left) {
                        emit_selection(selection, &drag_input, drag_handler.as_ref(), window, cx);
                    }
                })
                .on_click(move |_, window, cx| {
                    emit_selection(selection, &input, handler.as_ref(), window, cx);
                }),
        );
    }
    slider_shell(theme)
        .child(div().absolute().inset_0().rounded_full().bg(checkerboard(
            gpui_color(theme.colors.text_muted).opacity(0.35),
            4.0,
        )))
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded_full()
                .bg(linear_gradient(
                    90.0,
                    linear_color_stop(transparent, 0.0),
                    linear_color_stop(opaque, 1.0),
                )),
        )
        .child(hits)
        .child(slider_handle(
            selected.alpha * FIELD_WIDTH,
            selected_color(selected),
            theme,
        ))
}

fn slider_shell(theme: &AerisTheme) -> gpui::Div {
    div()
        .relative()
        .w(px(FIELD_WIDTH))
        .h(px(12.0))
        .rounded_full()
        .border(platform_border_width(theme))
        .border_color(gpui_color(theme.colors.border_secondary))
}

fn selection_handle(x: f32, y: f32, color: Hsla, theme: &AerisTheme) -> impl IntoElement {
    div()
        .absolute()
        .left(px((x - 7.0).clamp(-1.0, FIELD_WIDTH - 13.0)))
        .top(px((y - 7.0).clamp(-1.0, FIELD_HEIGHT - 13.0)))
        .size(px(14.0))
        .rounded_full()
        .border(px(2.0))
        .border_color(gpui_color(theme.colors.primary_foreground))
        .bg(color)
        .shadow_sm()
}

fn slider_handle(x: f32, color: Hsla, theme: &AerisTheme) -> impl IntoElement {
    div()
        .absolute()
        .left(px((x - 6.0).clamp(-1.0, FIELD_WIDTH - 11.0)))
        .top(px(-2.0))
        .size(px(14.0))
        .rounded_full()
        .border(px(2.0))
        .border_color(gpui_color(theme.colors.primary_foreground))
        .bg(color)
        .shadow_sm()
}

fn hex_input_row(
    input: &Entity<InputState>,
    selected: Hsva,
    handler: Option<&SelectionHandler>,
    theme: &AerisTheme,
) -> impl IntoElement {
    let submit_input = input.clone();
    let submit_handler = handler.cloned();
    div()
        .h(px(38.0))
        .flex()
        .items_center()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border(platform_border_width(theme))
        .border_color(gpui_color(theme.colors.input_border))
        .bg(gpui_color(theme.colors.input_fill))
        .overflow_hidden()
        .child(
            div()
                .w(px(34.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .size(px(14.0))
                        .rounded_full()
                        .bg(selected_color(selected)),
                ),
        )
        .child(
            div()
                .h_full()
                .flex_1()
                .child(Input::new(input).platform(theme).bordered(false).flex_1()),
        )
        .child(
            div()
                .id("native_color_apply")
                .h_full()
                .w(px(48.0))
                .flex()
                .items_center()
                .justify_center()
                .border_l(platform_border_width(theme))
                .border_color(gpui_color(theme.colors.border_secondary))
                .text_xs()
                .font_weight(platform_font_weight(TypographyRole::Emphasis))
                .text_color(gpui_color(theme.colors.text_secondary))
                .cursor_pointer()
                .hover(|button| button.bg(gpui_color(theme.colors.hover_bg)))
                .on_click(move |_, window, cx| {
                    if let Some(handler) = &submit_handler {
                        handler(submit_input.read(cx).value().to_string(), window, cx);
                    }
                })
                .child("Set"),
        )
}

fn recommended_colors(
    current: &str,
    input: &Entity<InputState>,
    handler: Option<&SelectionHandler>,
    theme: &AerisTheme,
) -> impl IntoElement {
    let mut row = div().flex().items_center().gap_3();
    for (index, value) in RECOMMENDED_COLORS.into_iter().enumerate() {
        let selected = same_hex_color(current, value);
        let fill = parse_hex_hsla(value).unwrap_or_else(|| gpui_color(theme.colors.text_muted));
        let input = input.clone();
        let handler = handler.cloned();
        let value = value.to_string();
        row = row.child(
            div()
                .id(("native_color_recommended", index))
                .size(px(22.0))
                .p(px(if selected { 3.0 } else { 1.0 }))
                .rounded_full()
                .when(selected, |swatch| {
                    swatch
                        .border(px(2.0))
                        .border_color(gpui_color(theme.colors.primary))
                })
                .cursor_pointer()
                .hover(|swatch| swatch.bg(gpui_color(theme.colors.hover_bg)))
                .on_click(move |_, window, cx| {
                    emit_hex_selection(value.clone(), &input, handler.as_ref(), window, cx);
                })
                .child(div().size_full().rounded_full().bg(fill)),
        );
    }
    row
}

fn emit_selection(
    selection: Hsva,
    input: &Entity<InputState>,
    handler: Option<&SelectionHandler>,
    window: &mut Window,
    cx: &mut App,
) {
    emit_hex_selection(hsva_hex(selection), input, handler, window, cx);
}

fn emit_hex_selection(
    value: String,
    input: &Entity<InputState>,
    handler: Option<&SelectionHandler>,
    window: &mut Window,
    cx: &mut App,
) {
    input.update(cx, |input, input_cx| {
        input.set_value(value.clone(), window, input_cx);
    });
    if let Some(handler) = handler {
        handler(value, window, cx);
    }
}

pub(crate) fn normalize_hex_color(value: &str) -> Option<String> {
    let value = value.trim().strip_prefix('#').unwrap_or(value.trim());
    let expanded = match value.len() {
        3 | 4 if value.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
            let mut result = String::with_capacity(value.len() * 2);
            for character in value.chars() {
                result.push(character);
                result.push(character);
            }
            result
        }
        6 | 8 if value.bytes().all(|byte| byte.is_ascii_hexdigit()) => value.to_string(),
        _ => return None,
    };
    Some(format!("#{}", expanded.to_ascii_uppercase()))
}

fn parse_hex_color(value: &str) -> Option<Hsva> {
    let normalized = normalize_hex_color(value)?;
    let hex = normalized.strip_prefix('#')?;
    let channel = |start| u8::from_str_radix(&hex[start..start + 2], 16).ok();
    let r = f32::from(channel(0)?) / 255.0;
    let g = f32::from(channel(2)?) / 255.0;
    let b = f32::from(channel(4)?) / 255.0;
    let alpha = if hex.len() == 8 {
        f32::from(channel(6)?) / 255.0
    } else {
        1.0
    };
    let maximum = r.max(g).max(b);
    let minimum = r.min(g).min(b);
    let delta = maximum - minimum;
    let hue = if delta <= f32::EPSILON {
        0.0
    } else if (maximum - r).abs() <= f32::EPSILON {
        ((g - b) / delta).rem_euclid(6.0) / 6.0
    } else if (maximum - g).abs() <= f32::EPSILON {
        ((b - r) / delta + 2.0) / 6.0
    } else {
        ((r - g) / delta + 4.0) / 6.0
    };
    Some(Hsva {
        hue,
        saturation: if maximum <= f32::EPSILON {
            0.0
        } else {
            delta / maximum
        },
        value: maximum,
        alpha,
    })
}

fn selected_color(color: Hsva) -> Hsla {
    Hsla::from(hsva_rgba(color))
}

fn parse_hex_hsla(value: &str) -> Option<Hsla> {
    parse_hex_color(value).map(selected_color)
}

fn hsva_rgba(color: Hsva) -> Rgba {
    let hue = (color.hue.rem_euclid(1.0) * 6.0).clamp(0.0, 6.0);
    let chroma = color.value * color.saturation;
    let secondary = chroma * (1.0 - (hue.rem_euclid(2.0) - 1.0).abs());
    let (red, green, blue) = if hue < 1.0 {
        (chroma, secondary, 0.0)
    } else if hue < 2.0 {
        (secondary, chroma, 0.0)
    } else if hue < 3.0 {
        (0.0, chroma, secondary)
    } else if hue < 4.0 {
        (0.0, secondary, chroma)
    } else if hue < 5.0 {
        (secondary, 0.0, chroma)
    } else {
        (chroma, 0.0, secondary)
    };
    let minimum = color.value - chroma;
    Rgba {
        r: red + minimum,
        g: green + minimum,
        b: blue + minimum,
        a: color.alpha,
    }
}

fn hsva_hex(color: Hsva) -> String {
    let rgba = hsva_rgba(color);
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round().to_u8().unwrap_or(0);
    let (r, g, b, a) = (
        channel(rgba.r),
        channel(rgba.g),
        channel(rgba.b),
        channel(rgba.a),
    );
    if a == u8::MAX {
        format!("#{r:02X}{g:02X}{b:02X}")
    } else {
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }
}

fn same_hex_color(left: &str, right: &str) -> bool {
    normalize_hex_color(left) == normalize_hex_color(right)
}

#[cfg(test)]
mod tests {
    use super::{Hsva, hsva_hex, normalize_hex_color, parse_hex_color};

    #[test]
    fn hex_input_accepts_rgb_and_rgba_long_and_shorthand_forms() {
        assert_eq!(normalize_hex_color(" #09aBcD ").as_deref(), Some("#09ABCD"));
        assert_eq!(normalize_hex_color("#0af").as_deref(), Some("#00AAFF"));
        assert_eq!(normalize_hex_color("#0af8").as_deref(), Some("#00AAFF88"));
        assert_eq!(
            normalize_hex_color("#12345678").as_deref(),
            Some("#12345678")
        );
        assert_eq!(normalize_hex_color("#12"), None);
        assert_eq!(normalize_hex_color("blue"), None);
    }

    #[test]
    fn picker_color_space_round_trips_primary_and_alpha_colors() {
        for value in ["#FF0000", "#00FF00", "#0000FF", "#335CFF80"] {
            let parsed = parse_hex_color(value).expect("valid picker color");
            assert_eq!(hsva_hex(parsed), value);
        }
        assert_eq!(
            hsva_hex(Hsva {
                hue: 0.0,
                saturation: 0.0,
                value: 1.0,
                alpha: 1.0,
            }),
            "#FFFFFF"
        );
    }
}
