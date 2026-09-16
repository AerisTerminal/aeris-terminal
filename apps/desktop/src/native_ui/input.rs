use std::{ops::Range, time::Duration};

use axiusflow_design_system::{AxiusflowTheme, RadiusToken, TypographyRole, platform_font_family};
use gpui::{
    App, Bounds, BoxShadow, ClipboardItem, Context, CursorStyle, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Render, RenderOnce, Role, ShapedLine,
    SharedString, Style, Subscription, Task, TextAlign, TextRun, UTF16Selection, UnderlineStyle,
    Window, div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation as _;

use super::{
    platform_font_weight,
    theme::{gpui_color, input_appearance, platform_border_width},
};

const CARET_BLINK_INTERVAL: Duration = Duration::from_millis(530);

/// Events emitted by Axiusflow's single-line text input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InputEvent {
    Change,
    PressEnter { secondary: bool, shift: bool },
    Focus,
    Blur,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TextBuffer {
    text: String,
    selection: Range<usize>,
    selection_reversed: bool,
    marked: Option<Range<usize>>,
}

impl TextBuffer {
    fn with_text(text: impl Into<String>) -> Self {
        let text = sanitize_single_line(&text.into());
        let end = text.len();
        Self {
            text,
            selection: end..end,
            selection_reversed: false,
            marked: None,
        }
    }

    fn cursor(&self) -> usize {
        if self.selection_reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    fn selected_text(&self) -> Option<&str> {
        (!self.selection.is_empty()).then_some(&self.text[self.selection.clone()])
    }

    fn move_to(&mut self, offset: usize) {
        let offset = clamp_byte_offset(&self.text, offset);
        self.selection = offset..offset;
        self.selection_reversed = false;
        self.marked = None;
    }

    fn select_to(&mut self, offset: usize) {
        let offset = clamp_byte_offset(&self.text, offset);
        let anchor = if self.selection_reversed {
            self.selection.end
        } else {
            self.selection.start
        };
        self.selection = anchor.min(offset)..anchor.max(offset);
        self.selection_reversed = offset < anchor;
        self.marked = None;
    }

    fn select_all(&mut self) {
        self.selection = 0..self.text.len();
        self.selection_reversed = false;
        self.marked = None;
    }

    fn move_left(&mut self, selecting: bool) {
        if selecting {
            self.select_to(previous_boundary(&self.text, self.cursor()));
        } else if self.selection.is_empty() {
            self.move_to(previous_boundary(&self.text, self.cursor()));
        } else {
            self.move_to(self.selection.start);
        }
    }

    fn move_right(&mut self, selecting: bool) {
        if selecting {
            self.select_to(next_boundary(&self.text, self.cursor()));
        } else if self.selection.is_empty() {
            self.move_to(next_boundary(&self.text, self.cursor()));
        } else {
            self.move_to(self.selection.end);
        }
    }

    fn move_home(&mut self, selecting: bool) {
        if selecting {
            self.select_to(0);
        } else {
            self.move_to(0);
        }
    }

    fn move_end(&mut self, selecting: bool) {
        if selecting {
            self.select_to(self.text.len());
        } else {
            self.move_to(self.text.len());
        }
    }

    fn backspace(&mut self) -> bool {
        if self.selection.is_empty() {
            let cursor = self.cursor();
            let previous = previous_boundary(&self.text, cursor);
            if previous == cursor {
                return false;
            }
            self.selection = previous..cursor;
        }
        self.replace(None, "", None, false)
    }

    fn delete(&mut self) -> bool {
        if self.selection.is_empty() {
            let cursor = self.cursor();
            let next = next_boundary(&self.text, cursor);
            if next == cursor {
                return false;
            }
            self.selection = cursor..next;
        }
        self.replace(None, "", None, false)
    }

    /// Replaces a UTF-16 range, the marked range, or the current selection (in that order).
    /// `marked_selection_utf16` is relative to the newly inserted marked text.
    fn replace(
        &mut self,
        range_utf16: Option<&Range<usize>>,
        new_text: &str,
        marked_selection_utf16: Option<Range<usize>>,
        mark: bool,
    ) -> bool {
        let replacement = range_utf16
            .map(|range| range_from_utf16(&self.text, range))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        let new_text = sanitize_single_line(new_text);
        let old_text = self.text.clone();
        self.text.replace_range(replacement.clone(), &new_text);

        let inserted = replacement.start..replacement.start + new_text.len();
        self.marked = (mark && !new_text.is_empty()).then_some(inserted.clone());
        if let Some(relative_utf16) = marked_selection_utf16 {
            let relative = range_from_utf16(&new_text, &relative_utf16);
            self.selection = replacement.start + relative.start..replacement.start + relative.end;
        } else {
            self.selection = inserted.end..inserted.end;
        }
        self.selection_reversed = false;
        old_text != self.text
    }

    fn set_text(&mut self, text: impl Into<String>) {
        *self = Self::with_text(text);
    }

    fn selection_utf16(&self) -> Range<usize> {
        range_to_utf16(&self.text, &self.selection)
    }

    fn marked_utf16(&self) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|range| range_to_utf16(&self.text, range))
    }
}

/// Axiusflow-owned state for a native GPUI, single-line text field.
pub(crate) struct InputState {
    focus_handle: FocusHandle,
    buffer: TextBuffer,
    placeholder: SharedString,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    scroll_x: Pixels,
    centered: bool,
    selecting: bool,
    caret_visible: bool,
    caret_blink_task: Option<Task<()>>,
    _focus_subscriptions: Vec<Subscription>,
}

impl InputState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle().tab_stop(true);
        let focused = cx.on_focus(&focus_handle, window, |input, _, cx| {
            input.restart_caret_blink(cx);
            cx.emit(InputEvent::Focus);
            cx.notify();
        });
        let blurred = cx.on_blur(&focus_handle, window, |input, _, cx| {
            input.selecting = false;
            input.buffer.marked = None;
            input.caret_visible = false;
            input.caret_blink_task = None;
            cx.emit(InputEvent::Blur);
            cx.notify();
        });

        Self {
            focus_handle,
            buffer: TextBuffer::default(),
            placeholder: SharedString::default(),
            last_layout: None,
            last_bounds: None,
            scroll_x: px(0.0),
            centered: false,
            selecting: false,
            caret_visible: false,
            caret_blink_task: None,
            _focus_subscriptions: vec![focused, blurred],
        }
    }

    pub(crate) fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    pub(crate) fn centered(mut self) -> Self {
        self.centered = true;
        self
    }

    pub(crate) fn value(&self) -> SharedString {
        self.buffer.text.clone().into()
    }

    /// Programmatic replacement intentionally does not emit `Change`.
    pub(crate) fn set_value(
        &mut self,
        value: impl Into<SharedString>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.set_text(value.into().to_string());
        self.scroll_x = px(0.0);
        self.invalidate_layout(cx);
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
    }

    fn invalidate_layout(&mut self, cx: &mut Context<Self>) {
        self.last_layout = None;
        cx.notify();
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.last_layout = None;
        self.restart_caret_blink(cx);
        cx.emit(InputEvent::Change);
        cx.notify();
    }

    fn restart_caret_blink(&mut self, cx: &mut Context<Self>) {
        self.caret_visible = true;
        self.caret_blink_task = Some(cx.spawn(async move |input, cx| {
            loop {
                cx.background_executor().timer(CARET_BLINK_INTERVAL).await;
                if input
                    .update(cx, |input, input_cx| {
                        input.caret_visible = !input.caret_visible;
                        input_cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn replace_from_user(
        &mut self,
        range_utf16: Option<&Range<usize>>,
        text: &str,
        marked_selection_utf16: Option<Range<usize>>,
        mark: bool,
        cx: &mut Context<Self>,
    ) {
        if self
            .buffer
            .replace(range_utf16, text, marked_selection_utf16, mark)
        {
            self.changed(cx);
        } else {
            self.invalidate_layout(cx);
        }
    }

    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(line)) = (self.last_bounds, self.last_layout.as_ref()) else {
            return 0;
        };
        if position.x <= bounds.left() {
            return 0;
        }
        if position.x >= bounds.right() {
            return self.buffer.text.len();
        }
        clamp_byte_offset(
            &self.buffer.text,
            line.closest_index_for_x(
                position.x - bounds.left() + self.scroll_x
                    - centered_text_offset(self.centered, bounds.size.width, line.width()),
            ),
        )
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus(window, cx);
        self.selecting = true;
        if event.click_count >= 2 {
            self.buffer.select_all();
        } else if event.modifiers.shift {
            self.buffer
                .select_to(self.index_for_position(event.position));
        } else {
            self.buffer.move_to(self.index_for_position(event.position));
        }
        self.restart_caret_blink(cx);
        cx.stop_propagation();
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selecting && event.pressed_button == Some(MouseButton::Left) {
            self.buffer
                .select_to(self.index_for_position(event.position));
            cx.notify();
        }
    }

    fn on_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.selecting = false;
    }

    fn on_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.to_ascii_lowercase();
        let modifiers = event.keystroke.modifiers;
        let command = modifiers.secondary();
        let mut changed = false;
        let handled = match key.as_str() {
            "backspace" => {
                changed = self.buffer.backspace();
                true
            }
            "delete" => {
                changed = self.buffer.delete();
                true
            }
            "left" => {
                self.buffer.move_left(modifiers.shift);
                true
            }
            "right" => {
                self.buffer.move_right(modifiers.shift);
                true
            }
            "home" => {
                self.buffer.move_home(modifiers.shift);
                true
            }
            "end" => {
                self.buffer.move_end(modifiers.shift);
                true
            }
            "a" if command => {
                self.buffer.select_all();
                true
            }
            "c" if command => {
                if let Some(text) = self.buffer.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
                }
                true
            }
            "x" if command => {
                if let Some(text) = self.buffer.selected_text().map(str::to_owned) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    changed = self.buffer.replace(None, "", None, false);
                }
                true
            }
            "v" if command => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    changed = self.buffer.replace(None, &text, None, false);
                }
                true
            }
            "enter" | "return" => {
                cx.emit(InputEvent::PressEnter {
                    secondary: command,
                    shift: modifiers.shift,
                });
                true
            }
            _ => false,
        };

        if changed {
            self.changed(cx);
        } else if handled {
            self.restart_caret_blink(cx);
            self.invalidate_layout(cx);
        }
        if handled {
            cx.stop_propagation();
            window.prevent_default();
        }
    }
}

impl EventEmitter<InputEvent> for InputState {}

impl Focusable for InputState {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for InputState {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = range_from_utf16(&self.buffer.text, &range_utf16);
        adjusted_range.replace(range_to_utf16(&self.buffer.text, &range));
        Some(self.buffer.text[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.buffer.selection_utf16(),
            reversed: self.buffer.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.buffer.marked_utf16()
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_from_user(range_utf16.as_ref(), text, None, false, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_from_user(
            range_utf16.as_ref(),
            new_text,
            new_selected_range_utf16,
            true,
            cx,
        );
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.last_layout.as_ref()?;
        let range = range_from_utf16(&self.buffer.text, &range_utf16);
        let text_offset =
            centered_text_offset(self.centered, element_bounds.size.width, line.width());
        Some(Bounds::from_corners(
            point(
                element_bounds.left() - self.scroll_x + text_offset + line.x_for_index(range.start),
                element_bounds.top(),
            ),
            point(
                element_bounds.left() - self.scroll_x + text_offset + line.x_for_index(range.end),
                element_bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let index = self.index_for_position(position);
        Some(utf16_offset_from_byte(&self.buffer.text, index))
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.selection = range_from_utf16(&self.buffer.text, &range_utf16);
        self.buffer.selection_reversed = false;
        self.buffer.marked = None;
        cx.notify();
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.buffer.text.encode_utf16().count())
    }
}

impl Render for InputState {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = if self.placeholder.is_empty() {
            SharedString::from("Text input")
        } else {
            self.placeholder.clone()
        };
        div()
            .id(("axiusflow_input", cx.entity_id()))
            .size_full()
            .min_w(px(0.0))
            .overflow_hidden()
            .flex()
            .items_center()
            .cursor(CursorStyle::IBeam)
            .track_focus(&self.focus_handle)
            .role(Role::TextInput)
            .aria_label(label)
            .aria_value(self.buffer.text.clone())
            .aria_placeholder(self.placeholder.clone())
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .child(InputTextElement { input: cx.entity() })
    }
}

/// A compatibility-sized presentation wrapper around [`InputState`].
///
/// The builder surface intentionally contains only the four options used by the
/// desktop search header.
#[derive(IntoElement)]
pub(crate) struct Input {
    state: Entity<InputState>,
    presentation: u8,
    fill: Option<Hsla>,
    border_color: Option<Hsla>,
    focus_ring_color: Option<Hsla>,
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
            border_width: None,
        }
    }

    pub(crate) fn platform(mut self, theme: &AxiusflowTheme) -> Self {
        let (fill, border, focus) = input_appearance(theme);
        self.fill = Some(gpui_color(fill));
        self.border_color = Some(gpui_color(border));
        self.focus_ring_color = Some(gpui_color(focus));
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
        let focused = self.state.read(cx).focus_handle.is_focused(window);
        let color = window.text_style().color;
        let border_color = self.border_color.unwrap_or_else(|| color.opacity(0.25));
        let focus_ring_color = self.focus_ring_color.unwrap_or_else(|| color.opacity(0.65));
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
            .child(self.state)
    }
}

struct InputTextElement {
    input: Entity<InputState>,
}

struct InputTextPrepaint {
    line: Option<ShapedLine>,
    selection: Option<PaintQuad>,
    caret: Option<PaintQuad>,
    text_origin: Point<Pixels>,
    scroll_x: Pixels,
}

impl IntoElement for InputTextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for InputTextElement {
    type RequestLayoutState = ();
    type PrepaintState = InputTextPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let style = window.text_style();
        let (display_text, color) = if input.buffer.text.is_empty() {
            // platform.css defines placeholders as --text-secondary, which is
            // the primary text color at 74% alpha in both light and dark modes.
            (input.placeholder.clone(), style.color.opacity(0.74))
        } else {
            (SharedString::from(input.buffer.text.clone()), style.color)
        };
        let base_run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = marked_runs(base_run, display_text.len(), input.buffer.marked.as_ref());
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let caret_x = line.x_for_index(input.buffer.cursor());
        let text_offset = centered_text_offset(input.centered, bounds.size.width, line.width());
        let scroll_x = if text_offset > px(0.0) {
            px(0.0)
        } else {
            horizontal_scroll_for_caret(caret_x, bounds.size.width, input.scroll_x)
        };
        let text_origin = point(bounds.left() + text_offset - scroll_x, bounds.top());

        let selection = (!input.buffer.selection.is_empty()).then(|| {
            fill(
                Bounds::from_corners(
                    point(
                        text_origin.x + line.x_for_index(input.buffer.selection.start),
                        bounds.top(),
                    ),
                    point(
                        text_origin.x + line.x_for_index(input.buffer.selection.end),
                        bounds.bottom(),
                    ),
                ),
                color.opacity(0.22),
            )
        });
        let caret = (input.buffer.selection.is_empty() && input.caret_visible).then(|| {
            fill(
                caret_bounds(text_origin.x + caret_x, bounds, window.scale_factor()),
                color,
            )
        });

        InputTextPrepaint {
            line: Some(line),
            selection,
            caret,
            text_origin,
            scroll_x,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }
        let Some(line) = prepaint.line.take() else {
            return;
        };
        let _ = line.paint(
            prepaint.text_origin,
            window.line_height(),
            TextAlign::Left,
            None,
            window,
            cx,
        );
        if focus.is_focused(window)
            && let Some(caret) = prepaint.caret.take()
        {
            window.paint_quad(caret);
        }
        self.input.update(cx, |input, _| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
            input.scroll_x = prepaint.scroll_x;
        });
    }
}

fn marked_runs(base: TextRun, text_len: usize, marked: Option<&Range<usize>>) -> Vec<TextRun> {
    let Some(marked) = marked.filter(|range| range.start < range.end && range.end <= text_len)
    else {
        return vec![base];
    };
    let mut runs = Vec::with_capacity(3);
    if marked.start > 0 {
        runs.push(TextRun {
            len: marked.start,
            ..base.clone()
        });
    }
    runs.push(TextRun {
        len: marked.end - marked.start,
        underline: Some(UnderlineStyle {
            color: Some(base.color),
            thickness: px(1.0),
            wavy: false,
        }),
        ..base.clone()
    });
    if marked.end < text_len {
        runs.push(TextRun {
            len: text_len - marked.end,
            ..base
        });
    }
    runs
}

fn sanitize_single_line(text: &str) -> String {
    text.replace("\r\n", " ").replace(['\r', '\n'], " ")
}

fn horizontal_scroll_for_caret(caret_x: Pixels, width: Pixels, current: Pixels) -> Pixels {
    let trailing_inset = px(2.0);
    let mut scroll_x = current;
    if caret_x < scroll_x {
        scroll_x = caret_x;
    } else if caret_x > scroll_x + width - trailing_inset {
        scroll_x = caret_x - width + trailing_inset;
    }
    if scroll_x < px(0.0) {
        px(0.0)
    } else {
        scroll_x
    }
}

fn centered_text_offset(centered: bool, width: Pixels, text_width: Pixels) -> Pixels {
    if centered && text_width < width {
        (width - text_width) / 2.0
    } else {
        px(0.0)
    }
}

fn caret_bounds(x: Pixels, text_bounds: Bounds<Pixels>, scale_factor: f32) -> Bounds<Pixels> {
    let scale_factor = scale_factor.max(1.0);
    let snapped_x = px((f32::from(x) * scale_factor).round() / scale_factor);
    let physical_pixel = px(1.0 / scale_factor);
    let inset = px(2.0_f32.min(f32::from(text_bounds.size.height) / 4.0));
    Bounds::new(
        point(snapped_x, text_bounds.top() + inset),
        size(
            physical_pixel,
            (text_bounds.size.height - inset * 2.0).max(physical_pixel),
        ),
    )
}

fn clamp_byte_offset(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn previous_boundary(text: &str, offset: usize) -> usize {
    let offset = clamp_byte_offset(text, offset);
    text.grapheme_indices(true)
        .rev()
        .find_map(|(index, _)| (index < offset).then_some(index))
        .unwrap_or(0)
}

fn next_boundary(text: &str, offset: usize) -> usize {
    let offset = clamp_byte_offset(text, offset);
    text.grapheme_indices(true)
        .find_map(|(index, _)| (index > offset).then_some(index))
        .unwrap_or(text.len())
}

fn utf16_offset_from_byte(text: &str, byte_offset: usize) -> usize {
    text[..clamp_byte_offset(text, byte_offset)]
        .encode_utf16()
        .count()
}

fn byte_offset_from_utf16(text: &str, utf16_offset: usize) -> usize {
    let mut utf16_count = 0;
    for (byte_offset, ch) in text.char_indices() {
        if utf16_count >= utf16_offset || utf16_count + ch.len_utf16() > utf16_offset {
            return byte_offset;
        }
        utf16_count += ch.len_utf16();
    }
    text.len()
}

fn range_to_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    utf16_offset_from_byte(text, range.start)..utf16_offset_from_byte(text, range.end)
}

fn range_from_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    let start = byte_offset_from_utf16(text, range.start);
    let end = byte_offset_from_utf16(text, range.end);
    start.min(end)..start.max(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_ranges_round_trip_unicode_without_splitting_utf8() {
        let text = "A📈東";
        assert_eq!(range_to_utf16(text, &(1..5)), 1..3);
        assert_eq!(range_from_utf16(text, &(1..3)), 1..5);
        assert_eq!(byte_offset_from_utf16(text, 2), 1);
    }

    #[test]
    fn unicode_editing_moves_and_deletes_on_grapheme_boundaries() {
        let mut buffer = TextBuffer::with_text("A📈東");
        buffer.move_left(false);
        assert_eq!(buffer.cursor(), 5);
        assert!(buffer.backspace());
        assert_eq!(buffer.text, "A東");
        assert_eq!(buffer.cursor(), 1);
        assert!(buffer.delete());
        assert_eq!(buffer.text, "A");
    }

    #[test]
    fn combining_marks_and_zwj_emoji_delete_as_single_graphemes() {
        let family = "👩‍👩‍👧‍👧";
        let mut buffer = TextBuffer::with_text(format!("e\u{301}{family}"));

        assert!(buffer.backspace());
        assert_eq!(buffer.text, "e\u{301}");
        assert!(buffer.backspace());
        assert!(buffer.text.is_empty());
    }

    #[test]
    fn selection_replacement_and_reversed_extension_are_deterministic() {
        let mut buffer = TextBuffer::with_text("abcdef");
        buffer.move_to(4);
        buffer.select_to(1);
        assert_eq!(buffer.selection, 1..4);
        assert!(buffer.selection_reversed);
        assert!(buffer.replace(None, "XY", None, false));
        assert_eq!(buffer.text, "aXYef");
        assert_eq!(buffer.selection, 3..3);
        assert!(!buffer.selection_reversed);
    }

    #[test]
    fn ime_marking_uses_utf16_selection_relative_to_inserted_text() {
        let mut buffer = TextBuffer::with_text("ab");
        buffer.move_to(1);
        assert!(buffer.replace(None, "📈東", Some(2..3), true));
        assert_eq!(buffer.text, "a📈東b");
        assert_eq!(buffer.marked, Some(1..8));
        assert_eq!(buffer.marked_utf16(), Some(1..4));
        assert_eq!(buffer.selection, 5..8);

        assert!(buffer.replace(None, "東", None, false));
        assert_eq!(buffer.text, "a東b");
        assert_eq!(buffer.marked, None);
    }

    #[test]
    fn single_line_input_normalizes_all_line_endings() {
        assert_eq!(
            sanitize_single_line("one\r\ntwo\nthree\rfour"),
            "one two three four"
        );
    }

    #[test]
    fn home_end_and_shift_selection_preserve_anchor() {
        let mut buffer = TextBuffer::with_text("market");
        buffer.move_home(false);
        buffer.move_right(true);
        buffer.move_right(true);
        assert_eq!(buffer.selection, 0..2);
        buffer.move_end(true);
        assert_eq!(buffer.selection, 0..6);
        buffer.move_left(false);
        assert_eq!(buffer.selection, 0..0);
    }

    #[test]
    fn horizontal_scroll_keeps_caret_inside_the_input_viewport() {
        assert_eq!(
            horizontal_scroll_for_caret(px(120.0), px(100.0), px(0.0)),
            px(22.0)
        );
        assert_eq!(
            horizontal_scroll_for_caret(px(10.0), px(100.0), px(22.0)),
            px(10.0)
        );
        assert_eq!(
            horizontal_scroll_for_caret(px(1.0), px(1.0), px(0.0)),
            px(2.0)
        );
    }

    #[test]
    fn caret_is_one_physical_pixel_and_stays_inside_the_text_line() {
        let text_bounds = Bounds::new(point(px(0.0), px(10.0)), size(px(100.0), px(20.0)));
        let caret = caret_bounds(px(12.37), text_bounds, 1.25);

        assert_eq!(caret.origin.x, px(12.0));
        assert_eq!(caret.size.width, px(0.8));
        assert!(caret.top() > text_bounds.top());
        assert!(caret.bottom() < text_bounds.bottom());
    }

    #[test]
    fn centered_input_offsets_only_text_that_fits() {
        assert_eq!(centered_text_offset(true, px(100.0), px(40.0)), px(30.0));
        assert_eq!(centered_text_offset(true, px(100.0), px(120.0)), px(0.0));
        assert_eq!(centered_text_offset(false, px(100.0), px(40.0)), px(0.0));
    }
}
