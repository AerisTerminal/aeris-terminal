use std::{rc::Rc, sync::Arc};

use aeris_design_system::{AerisTheme, RadiusToken, ThemeColor, ThemeColors};
use gpui::{
    App, Bounds, CursorStyle, DispatchPhase, ElementId, Entity, EntityId, HitboxBehavior, Hsla,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    RenderOnce, Rgba, SharedString, Subscription, WeakEntity, Window, canvas, checkerboard, div,
    hsla, linear_color_stop, linear_gradient, prelude::*, px, relative,
};
use num_traits::ToPrimitive;

use super::{
    button::{Button, ButtonSize, ButtonVariant},
    input::{Input, InputEvent, InputState},
    menu::{ANCHOR_GAP, PopupAnimationOrigin, animate_popup_from_origin},
    popup_fit::fit_to_window,
    rem_scale::design_rems,
    theme::{gpui_color, platform_border_width, popover_surface},
};

type SelectionHandler = Rc<dyn Fn(String, &mut Window, &mut App)>;
type DismissHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Wide enough that the hex field shows a full `#RRGGBBAA` value beside the preview and Set.
const PANEL_WIDTH: f32 = 288.0;
const PANEL_PADDING: f32 = 12.0;
const FIELD_HEIGHT: f32 = 132.0;
const SLIDER_HEIGHT: f32 = 10.0;
/// The hex row is the `Default` button height, in design rems like the button, so the preview,
/// the field and Set stay the same height inside a rem-scaled panel.
const HEX_ROW_HEIGHT: f32 = ButtonSize::Default.logical_height();
const SWATCH_SIZE: f32 = 20.0;

/// The swatches offered under the picker, in display order.
const fn recommended_colors_of(colors: &ThemeColors) -> [ThemeColor; 8] {
    [
        colors.swatch_gray,
        colors.swatch_blue,
        colors.swatch_orange,
        colors.swatch_red,
        colors.swatch_green,
        colors.swatch_amber,
        colors.swatch_violet,
        colors.swatch_sky,
    ]
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Hsva {
    hue: f32,
    saturation: f32,
    value: f32,
    alpha: f32,
}

/// The picker control a press started on; it keeps following the pointer until release.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PointerTarget {
    Field,
    Hue,
    Alpha,
}

impl PointerTarget {
    /// The color the pointer at `position` picks on this control, changing only its channels.
    fn pick(self, base: Hsva, bounds: Bounds<Pixels>, position: Point<Pixels>) -> Hsva {
        let x = fraction(position.x - bounds.left(), bounds.size.width);
        match self {
            Self::Field => Hsva {
                saturation: x,
                value: 1.0 - fraction(position.y - bounds.top(), bounds.size.height),
                ..base
            },
            Self::Hue => Hsva { hue: x, ..base },
            Self::Alpha => Hsva { alpha: x, ..base },
        }
    }

    const fn cursor(self) -> CursorStyle {
        match self {
            Self::Field => CursorStyle::Crosshair,
            Self::Hue | Self::Alpha => CursorStyle::PointingHand,
        }
    }
}

fn fraction(offset: Pixels, length: Pixels) -> f32 {
    let length = f32::from(length);
    if length <= 0.0 {
        0.0
    } else {
        (f32::from(offset) / length).clamp(0.0, 1.0)
    }
}

/// Pointer state that outlives a frame: the control being dragged, and the last color picked,
/// which keeps the hue and the exact handle positions that a hex value rounds away.
#[derive(Default)]
struct PickerPointer {
    drag: Option<PointerTarget>,
    picked: Option<Hsva>,
}

/// Selects a hex value as it is typed. The hosting dialog owns the input and recreates it each
/// time the picker opens, so the subscription follows the input it was made for.
#[derive(Default)]
struct HexTyping {
    handler: Option<SelectionHandler>,
    watched: Option<(EntityId, Subscription)>,
}

/// Selects the typed value once it is a complete `#RRGGBB` or `#RRGGBBAA` color. Shorthand
/// forms are prefixes of longer ones, so they wait for Set rather than flash a different color.
fn select_typed_hex(
    input: &Entity<InputState>,
    typing: WeakEntity<HexTyping>,
    window: &mut Window,
    cx: &mut App,
) -> Subscription {
    window.subscribe(input, cx, move |input, event: &InputEvent, window, cx| {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let value = input.read(cx).value().to_string();
        let digits = value.trim().strip_prefix('#').unwrap_or(value.trim()).len();
        if !matches!(digits, 6 | 8) || normalize_hex_color(&value).is_none() {
            return;
        }
        let handler = typing
            .upgrade()
            .and_then(|typing| typing.read(cx).handler.clone());
        if let Some(handler) = handler {
            handler(value, window, cx);
        }
    })
}

/// What a pointer surface needs to turn a press or drag into a selection.
#[derive(Clone)]
struct Picking {
    base: Hsva,
    pointer: Entity<PickerPointer>,
    input: Entity<InputState>,
    handler: Option<SelectionHandler>,
}

impl Picking {
    fn pick(&self, color: Hsva, window: &mut Window, cx: &mut App) {
        self.pointer
            .update(cx, |pointer, _| pointer.picked = Some(color));
        emit_selection(color, &self.input, self.handler.as_ref(), window, cx);
    }
}

/// Reusable native color picker with continuous color fields and exact hex entry.
///
/// The field and sliders are smooth GPUI gradients under one pointer surface each, which maps
/// the pointer straight to the color while pressed. All selection paths emit the same
/// normalized CSS hex value.
#[derive(IntoElement)]
pub(crate) struct ColorPicker {
    id: ElementId,
    current: String,
    input: Entity<InputState>,
    error: Option<SharedString>,
    theme: AerisTheme,
    on_select: Option<SelectionHandler>,
    on_dismiss: Option<DismissHandler>,
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
            on_dismiss: None,
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

    /// Closes the picker when a press lands outside it and its swatch trigger.
    pub(crate) fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ColorPicker {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = self.theme.colors;
        let theme = self.theme;
        let animation_id = self.id.clone();
        let pointer = window.use_keyed_state(
            ElementId::NamedChild(Arc::new(self.id.clone()), "pointer".into()),
            cx,
            |_, _| PickerPointer::default(),
        );
        let typing = window.use_keyed_state(
            ElementId::NamedChild(Arc::new(self.id.clone()), "typing".into()),
            cx,
            |_, _| HexTyping::default(),
        );
        let input_id = self.input.entity_id();
        let watched = typing.read(cx).watched.as_ref().map(|(id, _)| *id);
        let subscription = (watched != Some(input_id))
            .then(|| select_typed_hex(&self.input, typing.downgrade(), window, cx));
        let handler = self.on_select.clone();
        typing.update(cx, |typing, _| {
            typing.handler = handler;
            if let Some(subscription) = subscription {
                typing.watched = Some((input_id, subscription));
            }
        });
        let default = colors.swatch_default;
        let normalized = normalize_hex_color(&self.current).unwrap_or_else(|| default.css_hex());
        // The last picked color is exact while it is still the current value; a hex value alone
        // loses the hue of greys and rounds the handles to 8-bit steps.
        let selected = pointer
            .read(cx)
            .picked
            .filter(|picked| same_hex_color(&hsva_hex(*picked), &normalized))
            .or_else(|| parse_hex_color(&normalized))
            .unwrap_or_else(|| {
                rgb_hsva(
                    default.red(),
                    default.green(),
                    default.blue(),
                    default.alpha(),
                )
            });
        let picking = Picking {
            base: selected,
            pointer,
            input: self.input.clone(),
            handler: self.on_select,
        };

        let radius = px(f32::from(RadiusToken::Compact.logical_pixels()));
        let panel = popover_surface(div().id(self.id), &theme, radius)
            .absolute()
            .top_full()
            .mt(ANCHOR_GAP)
            .right_0()
            .w(px(PANEL_WIDTH))
            .p(px(PANEL_PADDING))
            .flex()
            .flex_col()
            .gap_2p5()
            .occlude()
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .child(saturation_value_field(&picking, &theme))
            .child(hue_slider(&picking, &theme))
            .child(alpha_slider(&picking, &theme))
            .child(hex_input_row(&picking, &theme))
            .children(self.error.map(|error| {
                div()
                    .text_xs()
                    .text_color(gpui_color(colors.danger))
                    .child(error)
            }))
            .child(
                div()
                    .mx(px(-PANEL_PADDING))
                    .h(platform_border_width(&theme))
                    .bg(gpui_color(colors.border_secondary)),
            )
            .child(recommended_colors(&normalized, &picking, &theme));
        let popup = fit_to_window(animate_popup_from_origin(
            panel,
            (animation_id, "enter"),
            PopupAnimationOrigin::TOP_RIGHT,
        ));
        match self.on_dismiss {
            Some(dismiss) => popup.on_dismiss(move |window, cx| dismiss(window, cx)),
            None => popup,
        }
    }
}

/// An invisible layer over a control that picks a color from the pointer: the press picks at
/// once, and the drag keeps picking, clamped to the control, wherever the pointer moves until
/// the button is released.
fn pointer_surface(target: PointerTarget, picking: &Picking) -> impl IntoElement {
    let picking = picking.clone();
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            window.set_cursor_style(target.cursor(), &hitbox);
            let press = picking.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && event.button == MouseButton::Left
                    && hitbox.is_hovered(window)
                {
                    press
                        .pointer
                        .update(cx, |pointer, _| pointer.drag = Some(target));
                    press.pick(target.pick(press.base, bounds, event.position), window, cx);
                }
            });
            let drag = picking.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture
                    && event.pressed_button == Some(MouseButton::Left)
                    && drag.pointer.read(cx).drag == Some(target)
                {
                    drag.pick(target.pick(drag.base, bounds, event.position), window, cx);
                }
            });
            let release = picking.pointer.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture && release.read(cx).drag == Some(target) {
                    release.update(cx, |pointer, _| pointer.drag = None);
                }
            });
        },
    )
    .absolute()
    .inset_0()
}

fn saturation_value_field(picking: &Picking, theme: &AerisTheme) -> impl IntoElement {
    let selected = picking.base;
    let pure_hue = hsla(selected.hue, 1.0, 0.5, 1.0);
    let transparent_white = hsla(0.0, 0.0, 1.0, 0.0);
    let transparent_black = hsla(0.0, 0.0, 0.0, 0.0);

    // GPUI clips children to a rectangle, not a rounded one, so every layer carries the
    // field's radius itself.
    let radius = px(f32::from(RadiusToken::Default.logical_pixels()));
    div()
        .relative()
        .w_full()
        .h(px(FIELD_HEIGHT))
        .debug_selector(|| "color_picker_field".into())
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded(radius)
                .bg(pure_hue)
                .child(
                    div()
                        .size_full()
                        .rounded(radius)
                        .bg(linear_gradient(
                            90.0,
                            linear_color_stop(gpui::white(), 0.0),
                            linear_color_stop(transparent_white, 1.0),
                        ))
                        .child(div().size_full().rounded(radius).bg(linear_gradient(
                            180.0,
                            linear_color_stop(transparent_black, 0.0),
                            linear_color_stop(gpui::black(), 1.0),
                        ))),
                ),
        )
        .child(pointer_surface(PointerTarget::Field, picking))
        .child(selection_handle(selected, theme))
}

fn hue_slider(picking: &Picking, theme: &AerisTheme) -> impl IntoElement {
    let selected = picking.base;
    let stops = [
        hsla(0.0, 1.0, 0.5, 1.0),
        hsla(1.0 / 6.0, 1.0, 0.5, 1.0),
        hsla(2.0 / 6.0, 1.0, 0.5, 1.0),
        hsla(3.0 / 6.0, 1.0, 0.5, 1.0),
        hsla(4.0 / 6.0, 1.0, 0.5, 1.0),
        hsla(5.0 / 6.0, 1.0, 0.5, 1.0),
        hsla(1.0, 1.0, 0.5, 1.0),
    ];
    // The bar is six gradient segments; the end segments carry the bar's rounded ends because
    // GPUI clips children to a rectangle.
    let mut gradient = div().absolute().inset_0().flex();
    let last = stops.len() - 2;
    for (index, pair) in stops.windows(2).enumerate() {
        gradient = gradient.child(
            div()
                .flex_1()
                .h_full()
                .when(index == 0, gpui::Styled::rounded_l_full)
                .when(index == last, gpui::Styled::rounded_r_full)
                .bg(linear_gradient(
                    90.0,
                    linear_color_stop(pair[0], 0.0),
                    linear_color_stop(pair[1], 1.0),
                )),
        );
    }
    slider_shell()
        .child(gradient)
        .child(slider_outline(theme))
        .child(pointer_surface(PointerTarget::Hue, picking))
        .child(slider_handle(selected.hue, selected, theme))
}

fn alpha_slider(picking: &Picking, theme: &AerisTheme) -> impl IntoElement {
    let selected = picking.base;
    let opaque = selected_color(Hsva {
        alpha: 1.0,
        ..selected
    });
    let transparent = Hsla { a: 0.0, ..opaque };
    slider_shell()
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded_full()
                .bg(transparency_backdrop(theme)),
        )
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
        .child(slider_outline(theme))
        .child(pointer_surface(PointerTarget::Alpha, picking))
        .child(slider_handle(selected.alpha, selected, theme))
}

/// The checkerboard every color preview sits on, so a translucent color reads as translucent.
pub(crate) fn transparency_backdrop(theme: &AerisTheme) -> gpui::Background {
    checkerboard(gpui_color(theme.colors.text_muted).opacity(0.35), 4.0)
}

fn slider_shell() -> gpui::Div {
    div()
        .relative()
        .w_full()
        .h(px(SLIDER_HEIGHT))
        .rounded_full()
}

/// The slider's outline as its own layer under the handle. GPUI paints a div's border after
/// its children, so a border on the shell would cross the handle.
fn slider_outline(theme: &AerisTheme) -> gpui::Div {
    div()
        .absolute()
        .inset_0()
        .rounded_full()
        .border(platform_border_width(theme))
        .border_color(gpui_color(theme.colors.border_secondary))
}

const HANDLE_SIZE: f32 = 14.0;
const HANDLE_HOLE: f32 = 6.0;

/// A solid disc with a centered hole that shows the selected color, edged so it stays visible
/// over light colors without a shadow.
fn handle_disc(color: Hsla, theme: &AerisTheme) -> gpui::Div {
    div()
        .absolute()
        .size(px(HANDLE_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border(platform_border_width(theme))
        .border_color(gpui_color(theme.colors.border_secondary))
        .bg(gpui_color(theme.colors.primary_foreground))
        .child(div().size(px(HANDLE_HOLE)).rounded_full().bg(color))
}

/// Centered on the selected saturation and value.
fn selection_handle(color: Hsva, theme: &AerisTheme) -> impl IntoElement {
    handle_disc(selected_color(color), theme)
        .left(relative(color.saturation))
        .top(relative(1.0 - color.value))
        .ml(px(-HANDLE_SIZE / 2.0))
        .mt(px(-HANDLE_SIZE / 2.0))
}

/// Centered on `position`, a fraction of the slider's width.
fn slider_handle(position: f32, color: Hsva, theme: &AerisTheme) -> impl IntoElement {
    handle_disc(selected_color(color), theme)
        .left(relative(position))
        .ml(px(-HANDLE_SIZE / 2.0))
        .top(px((SLIDER_HEIGHT - HANDLE_SIZE) / 2.0))
}

fn hex_input_row(picking: &Picking, theme: &AerisTheme) -> impl IntoElement {
    let selected = picking.base;
    let input = &picking.input;
    let submit_input = input.clone();
    let submit_handler = picking.handler.clone();
    div()
        .h(design_rems(HEX_ROW_HEIGHT))
        .flex()
        .items_center()
        .gap_2()
        // The input shapes its text from the inherited style; pin it so typed text stays the
        // size it opened at, whatever the picker is drawn over.
        .text_sm()
        .child(
            div()
                .size(design_rems(HEX_ROW_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                .border(platform_border_width(theme))
                .border_color(gpui_color(theme.colors.border_secondary))
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
                .min_w_0()
                .flex()
                .child(Input::new(input).platform(theme).flex_1()),
        )
        .child(
            Button::new("native_color_apply", theme)
                .variant(ButtonVariant::Primary)
                .button_size(ButtonSize::Default)
                .label("Set")
                .on_click(move |_, window, cx| {
                    if let Some(handler) = &submit_handler {
                        handler(submit_input.read(cx).value().to_string(), window, cx);
                    }
                }),
        )
}

fn recommended_colors(current: &str, picking: &Picking, theme: &AerisTheme) -> impl IntoElement {
    let mut row = div().flex().items_center().justify_between();
    for (index, color) in recommended_colors_of(&theme.colors).into_iter().enumerate() {
        let value = color.css_hex();
        let selected = same_hex_color(current, &value);
        let fill = parse_hex_hsla(&value).unwrap_or_else(|| gpui_color(theme.colors.text_muted));
        let input = picking.input.clone();
        let handler = picking.handler.clone();
        row = row.child(
            div()
                .id(("native_color_recommended", index))
                .size(px(SWATCH_SIZE))
                .p(px(2.0))
                .rounded_full()
                .border(platform_border_width(theme))
                .border_color(if selected {
                    gpui_color(theme.colors.primary)
                } else {
                    gpui::transparent_black()
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
    Some(rgb_hsva(r, g, b, alpha))
}

fn rgb_hsva(r: f32, g: f32, b: f32, alpha: f32) -> Hsva {
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
    Hsva {
        hue,
        saturation: if maximum <= f32::EPSILON {
            0.0
        } else {
            delta / maximum
        },
        value: maximum,
        alpha,
    }
}

fn selected_color(color: Hsva) -> Hsla {
    Hsla::from(hsva_rgba(color))
}

/// The exact color a CSS hex value (`#RGB`, `#RGBA`, `#RRGGBB` or `#RRGGBBAA`) names.
pub(crate) fn parse_hex_hsla(value: &str) -> Option<Hsla> {
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
    use std::{cell::RefCell, rc::Rc};

    use aeris_design_system::AerisTheme;
    use gpui::{
        AppContext, Bounds, Context, Entity, IntoElement, Modifiers, MouseButton, ParentElement,
        Pixels, Point, Render, Styled, TestAppContext, Window, deferred, div, point, px,
    };

    use super::{ColorPicker, Hsva, InputState, hsva_hex, normalize_hex_color, parse_hex_color};

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

    /// A picker opened under a swatch-sized trigger that feeds every pick back as its value,
    /// like the dialogs that host it.
    struct PickerHarness {
        current: String,
        input: Entity<InputState>,
        picks: Rc<RefCell<Vec<String>>>,
    }

    impl Render for PickerHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let view = cx.entity();
            let picks = self.picks.clone();
            div().size_full().relative().child(
                div()
                    .absolute()
                    .top(px(20.0))
                    .right(px(20.0))
                    .w(px(80.0))
                    .h(px(28.0))
                    .child(deferred(
                        ColorPicker::new(
                            "picker",
                            self.current.clone(),
                            &self.input,
                            &AerisTheme::dark(),
                        )
                        .on_select(move |value, _, cx| {
                            picks.borrow_mut().push(value.clone());
                            view.update(cx, |harness, cx| {
                                harness.current = value;
                                cx.notify();
                            });
                        }),
                    )),
            )
        }
    }

    fn field_point(field: Bounds<Pixels>, x: f32, y: f32) -> Point<Pixels> {
        point(
            field.left() + field.size.width * x,
            field.top() + field.size.height * y,
        )
    }

    #[gpui::test]
    fn dragging_on_the_color_field_follows_the_pointer_continuously(cx: &mut TestAppContext) {
        let picks = Rc::new(RefCell::new(Vec::new()));
        let harness_picks = picks.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            gpui_base::init(cx);
            PickerHarness {
                current: "#335CFF".into(),
                input: cx.new(|cx| InputState::new(window, cx)),
                picks: harness_picks,
            }
        });
        cx.run_until_parked();
        let field = cx.debug_bounds("color_picker_field").expect("field bounds");
        let base = parse_hex_color("#335CFF").expect("base color");
        let at = |saturation: f32, value: f32| {
            hsva_hex(Hsva {
                saturation,
                value,
                ..base
            })
        };

        cx.simulate_mouse_down(
            field_point(field, 0.25, 0.5),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_move(
            field_point(field, 0.75, 0.25),
            MouseButton::Left,
            Modifiers::none(),
        );
        // Leaving the field keeps the drag, clamped to its edge.
        cx.simulate_mouse_move(
            point(field.right() + px(60.0), field.top() - px(60.0)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_up(field.center(), MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(field.center(), None, Modifiers::none());

        let picks = picks.borrow();
        assert_eq!(picks.len(), 3, "moves after release pick nothing");
        // Within a hundredth of the pointer position: far finer than any grid of hit cells.
        for (pick, (saturation, value)) in picks.iter().zip([(0.25, 0.5), (0.75, 0.75)]) {
            let picked = parse_hex_color(pick).expect("picked color");
            assert!((picked.saturation - saturation).abs() < 0.01, "{pick}");
            assert!((picked.value - value).abs() < 0.01, "{pick}");
            assert!((picked.hue - base.hue).abs() < 0.01, "{pick} keeps the hue");
        }
        assert_eq!(
            picks[2],
            at(1.0, 1.0),
            "a drag past the field clamps to its edge"
        );
    }

    #[gpui::test]
    fn a_typed_hex_color_is_selected_once_it_is_complete(cx: &mut TestAppContext) {
        let picks = Rc::new(RefCell::new(Vec::new()));
        let harness_picks = picks.clone();
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            gpui_base::init(cx);
            PickerHarness {
                current: "#335CFF".into(),
                input: cx.new(|cx| InputState::new(window, cx)),
                picks: harness_picks,
            }
        });
        cx.run_until_parked();
        let input = cx.read(|cx| harness.read(cx).input.clone());
        for typed in ["#12", "#123", "#12345", "#123456", "#1234567", "#12345680"] {
            cx.update(|window, cx| {
                input.update(cx, |input, cx| input.replace_all(typed, window, cx));
            });
            cx.run_until_parked();
        }
        assert_eq!(
            *picks.borrow(),
            ["#123456", "#12345680"],
            "only complete RGB and RGBA values are selected while typing"
        );
    }
}
