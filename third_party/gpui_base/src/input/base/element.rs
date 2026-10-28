use crate::input::InputModeKind;
use gpui::Corners;
use gpui::Half;
use gpui::{
    AnyElement, App, Bounds, Edges, Element, ElementId, ElementInputHandler, Entity,
    GlobalElementId,
};
use gpui::{
    Hsla, IntoElement, LayoutId, MouseButton, MouseMoveEvent, MouseUpEvent, Path, Pixels, Point,
    Position, ShapedLine, SharedString, Size, Style, TextAlign, TextRun, TextStyle, UnderlineStyle,
    Window, fill, point, px, relative, size,
};
use ropey::Rope;
use smallvec::SmallVec;
use std::{ops::Range, rc::Rc};

use crate::{
    Scrollbar,
    input::{RopeExt as _, blink_cursor::CURSOR_WIDTH, display_map::LineLayout},
};

use super::{
    InputBaseState,
    layout::{LastLayout, WhitespaceIndicators},
    mode::LayoutMode,
};

const BOTTOM_MARGIN_ROWS: usize = 3;
pub(super) const RIGHT_MARGIN: Pixels = px(10.);

#[derive(Clone, Copy, Debug, PartialEq)]
struct EditorScrollbarLayout {
    bounds: Bounds<Pixels>,
    scroll_size: Size<Pixels>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct EditorScrollbarSnapshot {
    layout: EditorScrollbarLayout,
    cursor_scroll_offset: Point<Pixels>,
    soft_wrap: bool,
}

impl EditorScrollbarSnapshot {
    fn new<M: InputModeKind>(
        input_bounds: Bounds<Pixels>,
        scroll_size: Size<Pixels>,
        cursor_scroll_offset: Point<Pixels>,
        state: &InputBaseState<M>,
    ) -> Self {
        Self {
            layout: EditorScrollbarLayout::new(input_bounds, scroll_size, state.editor_paddings),
            cursor_scroll_offset,
            soft_wrap: state.soft_wrap,
        }
    }
}

impl EditorScrollbarLayout {
    fn new(
        input_bounds: Bounds<Pixels>,
        scroll_size: Size<Pixels>,
        paddings: Edges<Pixels>,
    ) -> Self {
        Self {
            bounds: Bounds::new(
                point(input_bounds.origin.x, input_bounds.origin.y - paddings.top),
                size(
                    input_bounds.size.width + paddings.right,
                    input_bounds.size.height + paddings.top + paddings.bottom,
                ),
            ),
            scroll_size: size(
                scroll_size.width + paddings.right + RIGHT_MARGIN,
                scroll_size.height,
            ),
        }
    }
}

pub(super) struct EditorScrollbar<M: InputModeKind> {
    state: Entity<InputBaseState<M>>,
}

impl<M: InputModeKind> EditorScrollbar<M> {
    pub(super) fn new(state: Entity<InputBaseState<M>>) -> Self {
        Self { state }
    }
}

impl<M: InputModeKind> IntoElement for EditorScrollbar<M> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<M: InputModeKind> Element for EditorScrollbar<M> {
    type RequestLayoutState = ();
    type PrepaintState = Option<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        Some("editor-scrollbar".into())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            position: Position::Absolute,
            size: Size {
                width: relative(1.).into(),
                height: relative(1.).into(),
            },
            ..Style::default()
        };

        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let state = self.state.read(cx);
        let snapshot = state.editor_scrollbar_snapshot.get()?;
        let scroll_handle = state.scroll_handle.clone();

        if scroll_handle.offset() != snapshot.cursor_scroll_offset {
            scroll_handle.set_offset(snapshot.cursor_scroll_offset);
        }

        let mut scrollbar = if !snapshot.soft_wrap {
            Scrollbar::new(&scroll_handle)
        } else {
            Scrollbar::vertical(&scroll_handle)
        }
        .viewport_bounds(snapshot.layout.bounds)
        .scroll_size(snapshot.layout.scroll_size)
        .into_any_element();

        scrollbar.prepaint_as_root(
            snapshot.layout.bounds.origin,
            snapshot.layout.bounds.size.into(),
            window,
            cx,
        );
        Some(scrollbar)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(scrollbar) = prepaint.as_mut() {
            scrollbar.paint(window, cx);
        }
    }
}

fn clamp_auto_grow_vertical_scroll_offset(
    mode: &LayoutMode,
    scroll_top: Pixels,
    scroll_height: Pixels,
    input_height: Pixels,
) -> Pixels {
    if mode.is_auto_grow() {
        scroll_top.clamp((input_height - scroll_height).min(px(0.)), px(0.))
    } else {
        scroll_top
    }
}

use super::MASK_CHAR;

/// Convert a byte offset in the original text to a byte offset in the masked display string.
///
/// The masked string consists of `MASK_CHAR` repeated once per character in the original text.
/// Since `MASK_CHAR` may be multi-byte in UTF-8, the byte offset in the masked string is
/// `char_index * MASK_CHAR.len_utf8()`.
fn masked_display_offset(text: &Rope, original_offset: usize) -> usize {
    text.offset_to_char_index(original_offset) * MASK_CHAR.len_utf8()
}

/// Move the IME marked range (tracked against the original text) into the display text
/// coordinate space, so that a run boundary can't land inside a multi-byte `MASK_CHAR`
/// and panic text shaping on a non-char-boundary slice.
fn ime_marked_display_range(
    text: &Rope,
    marked_range: Option<Range<usize>>,
    masked: bool,
) -> Option<Range<usize>> {
    let marked = marked_range?;
    if masked {
        Some(masked_display_offset(text, marked.start)..masked_display_offset(text, marked.end))
    } else {
        Some(marked)
    }
}

/// Minimum pixel padding the cursor is kept clear of the viewport's
/// top/bottom edges before auto-scroll engages. Backs
/// [`InputBaseState::cursor_surrounding_lines`].
///
/// Auto-grow uses one line. Otherwise `None` falls back to the historical
/// heuristic ([`BOTTOM_MARGIN_ROWS`] lines, or one line on small
/// viewports); `Some(n)` uses `n` lines. The result is saturated against
/// half the viewport so an oversized override can't invert the
/// top/bottom thresholds into a scroll feedback loop.
pub(super) fn cursor_surrounding_padding(
    is_auto_grow: bool,
    override_lines: Option<usize>,
    visible_lines: usize,
    line_height: Pixels,
) -> Pixels {
    if is_auto_grow {
        return line_height;
    }
    let raw = match override_lines {
        Some(lines) => lines as f32 * line_height,
        None => {
            if visible_lines < BOTTOM_MARGIN_ROWS * 8 {
                line_height
            } else {
                BOTTOM_MARGIN_ROWS * line_height
            }
        }
    };
    // Saturate against half the viewport so top + bottom margins can coexist.
    let viewport_half = (visible_lines as f32 * line_height).half();
    raw.min(viewport_half)
}

/// The caret as painted: snapped to whole device pixels and at least one device pixel wide.
///
/// Unsnapped, a bar at a fractional position or display scale is antialiased across
/// neighbouring pixels, so it reads thicker than it is and its weight changes as it moves.
fn device_pixel_caret(caret: Bounds<Pixels>, scale_factor: f32) -> Bounds<Pixels> {
    let to_device = |value: Pixels| (f32::from(value) * scale_factor).round();
    let left = to_device(caret.left());
    let top = to_device(caret.top());
    let bottom = to_device(caret.bottom()).max(top + 1.);
    let width = to_device(caret.size.width).max(1.);
    Bounds::new(
        point(px(left / scale_factor), px(top / scale_factor)),
        size(px(width / scale_factor), px((bottom - top) / scale_factor)),
    )
}

pub(super) struct TextElement<M: InputModeKind> {
    pub(crate) state: Entity<InputBaseState<M>>,
    placeholder: SharedString,
}

impl<M: InputModeKind> TextElement<M> {
    pub(super) fn new(state: Entity<InputBaseState<M>>) -> Self {
        Self {
            state,
            placeholder: SharedString::default(),
        }
    }

    /// Set the placeholder text of the input field.
    pub(super) fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    fn paint_mouse_listeners(&mut self, window: &mut Window, _: &mut App) {
        window.on_mouse_event({
            let state = self.state.clone();

            move |event: &MouseMoveEvent, _, window, cx| {
                if event.pressed_button == Some(MouseButton::Left) {
                    state.update(cx, |state, cx| {
                        state.on_drag_move(event, window, cx);
                    });
                }
            }
        });

        window.on_mouse_event({
            let state = self.state.clone();
            move |_: &MouseUpEvent, phase, _, cx| {
                if !phase.bubble() {
                    return;
                }

                // Stop auto-scroll when mouse up, and also stop selecting.
                state.update(cx, |state, _| {
                    state.auto_scroll.stop();
                    state.selecting = false;
                });
            }
        });
    }

    /// Returns the cursor bounds and the scroll offset.
    ///
    /// This method also will update for track scroll to cursor.
    fn layout_cursor(
        &self,
        last_layout: &LastLayout,
        bounds: &mut Bounds<Pixels>,
        scroll_size: Size<Pixels>,
        _: &mut Window,
        cx: &mut App,
    ) -> (Option<Bounds<Pixels>>, Point<Pixels>) {
        let state = self.state.read(cx);

        let line_height = last_layout.line_height;
        let visible_range = &last_layout.visible_range;
        let lines = &last_layout.lines;

        let mut selected_range = state.selected_range;

        if let Some(ime_marked_range) = &state.ime_marked_range {
            selected_range = (ime_marked_range.end..ime_marked_range.end).into();
        }
        let is_selected_all = selected_range.len() == state.text.len();

        let mut cursor = state.cursor();
        // Buffer rows from the raw (pre-mask) offsets, used to locate the cursor line.
        let cursor_row = state.text.offset_to_point(cursor).row;
        let sel_start_row = state.text.offset_to_point(selected_range.start).row;
        let sel_end_row = state.text.offset_to_point(selected_range.end).row;
        if state.masked {
            selected_range.start = masked_display_offset(&state.text, selected_range.start);
            selected_range.end = masked_display_offset(&state.text, selected_range.end);
            cursor = masked_display_offset(&state.text, cursor);
        }

        let mut scroll_offset = state.scroll_handle.offset();

        // Padding kept between the cursor and the viewport's top/bottom
        // edges, used by the auto-scroll-into-view computation below.
        let top_bottom_margin = cursor_surrounding_padding(
            state.mode.is_auto_grow(),
            state.cursor_surrounding_lines,
            visible_range.len(),
            line_height,
        );

        // Resolve a cursor or selection endpoint to a content-space position.
        let visible_buffer_lines = &last_layout.visible_buffer_lines;
        let caret_for = |row: usize, offset: usize, affinity: bool| -> Point<Pixels> {
            // y of the top of buffer line `row` in content space.
            let top = line_height * state.display_map.buffer_line_to_display_row(row);
            let line_origin = point(px(0.), top);

            if let Some(vi) = visible_buffer_lines.iter().position(|&bl| bl == row) {
                let line = &lines[vi];
                let line_start = last_layout.visible_line_byte_offsets[vi];
                let local = offset.saturating_sub(line_start);
                if let Some(pos) = line.position_for_index(local, last_layout, affinity) {
                    return line_origin + pos;
                }
            }
            line_origin
        };

        let cursor_pos = caret_for(cursor_row, cursor, state.cursor_line_end_affinity);
        let cursor_start = caret_for(sel_start_row, selected_range.start, false);
        let cursor_end = caret_for(sel_end_row, selected_range.end, false);

        let cursor_bounds = {
            let selection_changed = state.last_selected_range != Some(selected_range);
            let auto_scrolling = state.auto_scroll.is_active();
            if selection_changed && !is_selected_all {
                // For Right alignment use 0 margin: cursor is clamped to bounds separately,
                // so we never scroll the text for cursor-at-edge, avoiding a first-click jump.
                let safety_margin = match last_layout.text_align {
                    TextAlign::Left => RIGHT_MARGIN,
                    TextAlign::Right => px(0.),
                    TextAlign::Center => CURSOR_WIDTH,
                };

                scroll_offset.x =
                    if scroll_offset.x + cursor_pos.x > (bounds.size.width - safety_margin) {
                        // cursor is out of right
                        bounds.size.width - safety_margin - cursor_pos.x
                    } else if scroll_offset.x + cursor_pos.x < px(0.) {
                        // cursor is out of left
                        scroll_offset.x - cursor_pos.x
                    } else {
                        scroll_offset.x
                    };

                // Vertical cursor-follow is suppressed while auto-scroll manages the y axis,
                // to prevent fighting the background scroll task.
                if !auto_scrolling {
                    // If we change the scroll_offset.y, GPUI will render and trigger the next run loop.
                    // So, here we just adjust offset by `line_height` for move smooth.
                    scroll_offset.y = if scroll_offset.y + cursor_pos.y
                        > bounds.size.height - top_bottom_margin
                    {
                        // cursor is out of bottom
                        scroll_offset.y - line_height
                    } else if scroll_offset.y + cursor_pos.y < top_bottom_margin {
                        // cursor is out of top
                        (scroll_offset.y + line_height).min(px(0.))
                    } else {
                        scroll_offset.y
                    };
                }

                // For selection to move scroll
                if state.selection_reversed {
                    if scroll_offset.x + cursor_start.x < px(0.) {
                        // selection start is out of left
                        scroll_offset.x = -cursor_start.x;
                    }
                    if !auto_scrolling && scroll_offset.y + cursor_start.y < px(0.) {
                        // selection start is out of top
                        scroll_offset.y = -cursor_start.y;
                    }
                } else {
                    // TODO: Consider to remove this part,
                    // maybe is not necessary (But selection_reversed is needed).
                    if scroll_offset.x + cursor_end.x <= px(0.) {
                        // selection end is out of left
                        scroll_offset.x = -cursor_end.x;
                    }
                    if !auto_scrolling && scroll_offset.y + cursor_end.y <= px(0.) {
                        // selection end is out of top
                        scroll_offset.y = -cursor_end.y;
                    }
                }
            }

            // cursor bounds
            let cursor_height = 0.85 * line_height;

            // Match the caret to the deferred scroll target (applied below) that
            // the text paints at; otherwise the caret follows the cursor-scroll
            // while the text uses the deferred offset, flashing it mid-field.
            let cursor_scroll_x = state
                .deferred_scroll_offset
                .map(|offset| offset.x)
                .unwrap_or(scroll_offset.x);

            // For Right alignment, clamp cursor within the right edge of bounds so it
            // stays visible without having to shift the text via scroll_offset.
            let cursor_x = bounds.left() + cursor_pos.x + cursor_scroll_x;
            let cursor_x = if last_layout.text_align == TextAlign::Right {
                cursor_x.min(bounds.right() - CURSOR_WIDTH)
            } else {
                cursor_x
            };
            Some(Bounds::new(
                point(
                    cursor_x,
                    bounds.top() + cursor_pos.y + ((line_height - cursor_height) / 2.),
                ),
                size(CURSOR_WIDTH, cursor_height),
            ))
        };

        if let Some(deferred_scroll_offset) = state.deferred_scroll_offset {
            scroll_offset = deferred_scroll_offset;
        }
        scroll_offset.y = clamp_auto_grow_vertical_scroll_offset(
            &state.mode,
            scroll_offset.y,
            scroll_size.height,
            bounds.size.height,
        );

        bounds.origin += scroll_offset;

        (cursor_bounds, scroll_offset)
    }

    /// Layout the match range to a Path.
    pub(crate) fn layout_match_range(
        range: Range<usize>,
        last_layout: &LastLayout,
        bounds: &Bounds<Pixels>,
    ) -> Option<Path<Pixels>> {
        if range.is_empty() {
            return None;
        }

        if range.start < last_layout.visible_range_offset.start
            || range.end > last_layout.visible_range_offset.end
        {
            return None;
        }

        let line_height = last_layout.line_height;
        let visible_top = last_layout.visible_top;
        let lines = &last_layout.lines;

        let start_ix = range.start;
        let end_ix = range.end;

        // Start from visible_top (which already accounts for all lines before visible range)
        let mut offset_y = visible_top;
        let mut line_corners = vec![];

        // Iterate only over visible (non-hidden) buffer lines
        for (prev_lines_offset, line) in last_layout
            .visible_line_byte_offsets
            .iter()
            .zip(lines.iter())
        {
            let prev_lines_offset = *prev_lines_offset;
            let line_size = line.size(line_height);
            let line_wrap_width = line_size.width;

            let line_origin = point(px(0.), offset_y);

            let line_cursor_start = line.position_for_index(
                start_ix.saturating_sub(prev_lines_offset),
                last_layout,
                false,
            );
            // The end of a range closes the row it lands on: a range ending exactly on a soft
            // wrap boundary highlights to the end of that row instead of opening a zero-width
            // sliver at the start of the next one.
            let line_cursor_end = line.position_for_index(
                end_ix.saturating_sub(prev_lines_offset),
                last_layout,
                true,
            );

            if line_cursor_start.is_some() || line_cursor_end.is_some() {
                let start = line_cursor_start
                    .unwrap_or_else(|| line.position_for_index(0, last_layout, false).unwrap());

                let end = line_cursor_end.unwrap_or_else(|| {
                    line.position_for_index(line.len(), last_layout, false)
                        .unwrap()
                });

                // Split the selection into multiple items
                let wrapped_lines =
                    (end.y / line_height).ceil() as usize - (start.y / line_height).ceil() as usize;

                let mut end_x = end.x;
                if wrapped_lines > 0 {
                    end_x = line_wrap_width;
                }

                // Ensure at least 6px width for the selection for empty lines.
                end_x = end_x.max(start.x + px(6.));

                line_corners.push(Corners {
                    top_left: line_origin + point(start.x, start.y),
                    top_right: line_origin + point(end_x, start.y),
                    bottom_left: line_origin + point(start.x, start.y + line_height),
                    bottom_right: line_origin + point(end_x, start.y + line_height),
                });

                // wrapped lines
                for i in 1..=wrapped_lines {
                    let indent = line.wrap_indent;
                    let start = point(indent, start.y + i as f32 * line_height);
                    let mut end = point(end.x, end.y + i as f32 * line_height);
                    if i < wrapped_lines {
                        end.x = line_size.width;
                    }

                    line_corners.push(Corners {
                        top_left: line_origin + point(start.x, start.y),
                        top_right: line_origin + point(end.x, start.y),
                        bottom_left: line_origin + point(start.x, start.y + line_height),
                        bottom_right: line_origin + point(end.x, start.y + line_height),
                    });
                }
            }

            if line_cursor_start.is_some() && line_cursor_end.is_some() {
                break;
            }

            offset_y += line_size.height;
        }

        let mut points = vec![];
        if line_corners.is_empty() {
            return None;
        }

        // Fix corners to make sure the left to right direction
        for corners in &mut line_corners {
            if corners.top_left.x > corners.top_right.x {
                std::mem::swap(&mut corners.top_left, &mut corners.top_right);
                std::mem::swap(&mut corners.bottom_left, &mut corners.bottom_right);
            }
        }

        for corners in &line_corners {
            points.push(corners.top_right);
            points.push(corners.bottom_right);
            points.push(corners.bottom_left);
        }

        let mut rev_line_corners = line_corners.iter().rev().peekable();
        while let Some(corners) = rev_line_corners.next() {
            points.push(corners.top_left);
            if let Some(next) = rev_line_corners.peek()
                && next.top_left.x != corners.top_left.x
            {
                points.push(point(next.top_left.x, corners.top_left.y));
            }
        }

        let path_origin = bounds.origin;
        let first_p = *points.first().unwrap();
        let mut builder = gpui::PathBuilder::fill();
        builder.move_to(path_origin + first_p);
        for p in points.iter().skip(1) {
            builder.line_to(path_origin + *p);
        }

        builder.build().ok()
    }

    fn layout_selections(
        &self,
        last_layout: &LastLayout,
        bounds: &mut Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Path<Pixels>> {
        let state = self.state.read(cx);
        if !state.focus_handle.is_focused(window) {
            return None;
        }

        let mut selected_range = state.selected_range;
        if let Some(ime_marked_range) = &state.ime_marked_range
            && !ime_marked_range.is_empty()
        {
            selected_range = (ime_marked_range.end..ime_marked_range.end).into();
        }
        if selected_range.is_empty() {
            return None;
        }

        if state.masked {
            selected_range.start = masked_display_offset(&state.text, selected_range.start);
            selected_range.end = masked_display_offset(&state.text, selected_range.end);
        }

        let (start_ix, end_ix) = if selected_range.start < selected_range.end {
            (selected_range.start, selected_range.end)
        } else {
            (selected_range.end, selected_range.start)
        };

        let range = start_ix.max(last_layout.visible_range_offset.start)
            ..end_ix.min(last_layout.visible_range_offset.end);

        Self::layout_match_range(range, last_layout, bounds)
    }

    /// Calculate the visible range of lines in the viewport.
    ///
    /// Returns
    ///
    /// - visible_range: The visible range is based on unwrapped lines (Zero based).
    /// - visible_buffer_lines: The buffer lines within the visible range.
    /// - visible_top: The top position of the first visible line in the scroll viewport.
    fn calculate_visible_range(
        &self,
        state: &InputBaseState<M>,
        line_height: Pixels,
        input_height: Pixels,
    ) -> (Range<usize>, Vec<usize>, Pixels) {
        // Add extra rows to avoid showing empty space when scroll to bottom.
        let extra_rows = 1;
        if state.is_single_line() {
            return (0..1, vec![0], px(0.));
        }

        let display_count = state.display_map.display_row_count();
        let buffer_line_count = state.display_map.buffer_line_count();
        if display_count == 0 || buffer_line_count == 0 {
            return (0..0, Vec::new(), px(0.));
        }

        let mut scroll_top = if let Some(deferred_scroll_offset) = state.deferred_scroll_offset {
            deferred_scroll_offset.y
        } else {
            state.scroll_handle.offset().y
        };
        scroll_top = clamp_auto_grow_vertical_scroll_offset(
            &state.mode,
            scroll_top,
            line_height * display_count,
            input_height,
        );

        // Display rows are uniformly `line_height` tall, so the visible window maps
        // directly to a display-row range.
        let viewport_top = (-scroll_top).max(px(0.));
        let viewport_bottom = viewport_top + input_height;
        let line_height_f = f32::from(line_height);
        let first_display =
            ((f32::from(viewport_top) / line_height_f).floor() as usize).min(display_count - 1);
        let last_display =
            ((f32::from(viewport_bottom) / line_height_f).ceil() as usize).min(display_count - 1);

        let start_line = state.display_map.display_row_to_buffer_line(first_display);
        let end_line = state.display_map.display_row_to_buffer_line(last_display);

        // y of the top of the first visible buffer line (in content space).
        let visible_top = line_height * state.display_map.buffer_line_to_display_row(start_line);
        let visible_range = start_line..(end_line + 1 + extra_rows).min(buffer_line_count);
        let visible_buffer_lines = visible_range.clone().collect();

        (visible_range, visible_buffer_lines, visible_top)
    }

    /// Layout shaped lines for whitespace indicators (space and tab).
    ///
    /// Returns `WhitespaceIndicators` with shaped lines for space and tab characters.
    fn layout_whitespace_indicators(
        state: &InputBaseState<M>,
        text_size: Pixels,
        style: &TextStyle,
        window: &mut Window,
        _cx: &App,
    ) -> Option<WhitespaceIndicators> {
        if !state.show_whitespaces {
            return None;
        }

        let invisible_color = state.editor_style.muted_foreground;

        let space_font_size = text_size.half();
        let tab_font_size = text_size;

        let space_text = SharedString::new_static("•");
        let space = window.text_system().shape_line(
            space_text.clone(),
            space_font_size,
            &[TextRun {
                len: space_text.len(),
                font: style.font(),
                color: invisible_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );

        let tab_text = SharedString::new_static("→");
        let tab = window.text_system().shape_line(
            tab_text.clone(),
            tab_font_size,
            &[TextRun {
                len: tab_text.len(),
                font: style.font(),
                color: invisible_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );

        Some(WhitespaceIndicators { space, tab })
    }

    fn layout_lines(
        state: &InputBaseState<M>,
        display_text: &Rope,
        last_layout: &LastLayout,
        font_size: Pixels,
        runs: &[TextRun],
        whitespace_indicators: Option<WhitespaceIndicators>,
        window: &mut Window,
    ) -> Vec<LineLayout> {
        let is_single_line = state.is_single_line();

        if is_single_line {
            let text: SharedString = display_text.to_string().into();
            let aligned_runs = align_runs_to_char_boundaries(&text, runs);
            let line_runs = aligned_runs.as_deref().unwrap_or(runs);
            let shaped_line = window
                .text_system()
                .shape_line(text, font_size, line_runs, None);

            let line_layout = LineLayout::new()
                .lines(smallvec::smallvec![shaped_line])
                .with_whitespaces(whitespace_indicators);
            return vec![line_layout];
        }

        // Empty to use placeholder, the placeholder is not in the wrapper map.
        if state.text.len() == 0 {
            let placeholder_text = display_text.to_string();
            let mut placeholder_lines = SmallVec::new();

            for (line, line_runs) in placeholder_line_runs(&placeholder_text, runs) {
                let shaped_line = window.text_system().shape_line(
                    line.to_string().into(),
                    font_size,
                    &line_runs,
                    None,
                );
                placeholder_lines.push(shaped_line);
            }

            // Keep placeholder lines in a single layout to stay parallel with visible_* metadata.
            let line_layout = LineLayout::new()
                .lines(placeholder_lines)
                .with_whitespaces(whitespace_indicators);
            return vec![line_layout];
        }

        let mut lines = Vec::with_capacity(last_layout.visible_buffer_lines.len());
        // Runs cover only the visible lines' bytes, so their offsets restart at the first
        // visible line rather than at the start of the text.
        let mut run_offset = 0;

        for &buffer_line in &last_layout.visible_buffer_lines {
            let line_text: String = display_text.slice_line(buffer_line).into();
            let line_item = state
                .display_map
                .line(buffer_line)
                .expect("line should exists in wrapper");

            debug_assert_eq!(line_item.len(), line_text.len());

            let mut wrapped_lines: SmallVec<[ShapedLine; 1]> = SmallVec::with_capacity(1);

            for range in &line_item.wrapped_lines {
                let line_runs = runs_for_range(runs, run_offset, range);

                let sub_line: SharedString = line_text[range.clone()].to_string().into();
                let line_runs =
                    align_runs_to_char_boundaries(&sub_line, &line_runs).unwrap_or(line_runs);
                let shaped_line = window
                    .text_system()
                    .shape_line(sub_line, font_size, &line_runs, None);

                wrapped_lines.push(shaped_line);
            }

            // Use the first visual line's indentation width for continuation lines.
            let wrap_indent = if line_item.indent > 0 && wrapped_lines.len() > 1 {
                let indent_byte_len = line_text
                    .char_indices()
                    .nth(line_item.indent as usize)
                    .map(|(ix, _)| ix)
                    .unwrap_or(line_text.len());
                wrapped_lines[0].x_for_index(indent_byte_len)
            } else {
                px(0.)
            };

            let line_layout = LineLayout::new()
                .lines(wrapped_lines)
                .wrap_indent(wrap_indent)
                .with_whitespaces(whitespace_indicators.clone());
            lines.push(line_layout);

            // +1 for the `\n`
            run_offset += line_text.len() + 1;
        }

        lines
    }
}

pub(super) struct PrepaintState {
    /// The lines of entire lines.
    last_layout: LastLayout,
    /// Size of the scrollable area by entire lines.
    scroll_size: Size<Pixels>,
    cursor_bounds: Option<Bounds<Pixels>>,
    cursor_scroll_offset: Point<Pixels>,
    selection_path: Option<Path<Pixels>>,
    bounds: Bounds<Pixels>,
}

impl PrepaintState {
    /// Returns cursor bounds adjusted for scroll offset, if available.
    fn cursor_bounds_with_scroll(&self) -> Option<Bounds<Pixels>> {
        self.cursor_bounds.map(|mut bounds| {
            bounds.origin.y += self.cursor_scroll_offset.y;
            bounds
        })
    }
}

impl<M: InputModeKind> IntoElement for TextElement<M> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<M: InputModeKind> Element for TextElement<M> {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let state = self.state.read(cx);
        let line_height = window.line_height();

        let mut style = Style::default();
        style.size.width = relative(1.).into();
        if state.is_multi_line() {
            style.flex_grow = 1.0;
            style.size.height = relative(1.).into();
            if state.mode.is_auto_grow() {
                // Auto grow to let height match to rows, but not exceed max rows.
                let rows = state.mode.max_rows().min(state.mode.rows());
                style.min_size.height = (rows * line_height).into();
            } else {
                style.min_size.height = line_height.into();
            }
        } else {
            // For single-line inputs, the minimum height should be the line height
            style.size.height = line_height.into();
        };

        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let style = window.text_style();
        let font = style.font();
        let text_size = style.font_size.to_pixels(window.rem_size());

        self.state.update(cx, |state, cx| {
            state.display_map.set_font(font, text_size, cx);
            state.display_map.ensure_text_prepared(&state.text, cx);
        });

        let state = self.state.read(cx);
        let multi_line = state.is_multi_line();
        let text = state.text.clone();
        let is_empty = text.len() == 0;
        let placeholder = self.placeholder.clone();

        let text_style = window.text_style();
        let disabled = state.disabled;
        let dim = |color: Hsla| if disabled { color.opacity(0.5) } else { color };
        let fg = dim(text_style.color);
        let (display_text, text_color) = if is_empty {
            (
                &Rope::from(placeholder.as_str()),
                dim(state.editor_style.muted_foreground),
            )
        } else if state.masked {
            (
                &Rope::from(MASK_CHAR.to_string().repeat(text.chars().count())),
                fg,
            )
        } else {
            (&text, fg)
        };

        let mut bounds = bounds;
        let wrap_width = if multi_line && state.soft_wrap {
            Some(bounds.size.width - RIGHT_MARGIN)
        } else {
            None
        };

        let wrapping_indent = state.wrapping_indent;
        let wrap_width_changed = state
            .last_layout
            .as_ref()
            .map(|l| l.wrap_width != wrap_width)
            .unwrap_or(true);

        let wrapping_indent_changed = state
            .last_layout
            .as_ref()
            .map(|l| l.wrapping_indent != wrapping_indent)
            .unwrap_or(true);

        if wrap_width_changed || wrapping_indent_changed {
            self.state.update(cx, |state, cx| {
                state.display_map.on_layout_changed(wrap_width, cx);
                state.display_map.set_wrapping_indent(wrapping_indent, cx);
            });
        }

        let state = self.state.read(cx);
        let line_height = window.line_height();

        let (visible_range, visible_buffer_lines, visible_top) =
            self.calculate_visible_range(state, line_height, bounds.size.height);
        let visible_start_offset = state.text.line_start_offset(visible_range.start);
        let visible_end_offset = state
            .text
            .line_end_offset(visible_range.end.saturating_sub(1));

        let state = self.state.read(cx);

        let visible_line_byte_offsets: Vec<usize> = visible_buffer_lines
            .iter()
            .map(|&bl| state.text.line_start_offset(bl))
            .collect();

        // For password input (masked: true), convert byte offsets to masked display byte offsets so that
        // layout_match_range and position_for_index work in the correct coordinate space.
        let (visible_line_byte_offsets, visible_range_offset) = if state.masked {
            let offsets = visible_line_byte_offsets
                .iter()
                .map(|&o| masked_display_offset(&text, o))
                .collect();
            let range_offset = masked_display_offset(&text, visible_start_offset)
                ..masked_display_offset(&text, visible_end_offset);
            (offsets, range_offset)
        } else {
            (
                visible_line_byte_offsets,
                visible_start_offset..visible_end_offset,
            )
        };

        let mut last_layout = LastLayout {
            visible_range,
            visible_buffer_lines,
            visible_line_byte_offsets,
            visible_top,
            visible_range_offset,
            line_height,
            wrap_width,
            wrapping_indent,
            lines: Rc::new(vec![]),
            cursor_bounds: None,
            text_align: state.text_align,
            content_width: bounds.size.width,
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let marked_run = TextRun {
            len: 0,
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(text_color),
                wavy: false,
            }),
            strikethrough: None,
        };

        let ime_marked_range = ime_marked_display_range(
            &text,
            state.ime_marked_range.as_ref().map(|m| m.start..m.end),
            state.masked,
        );

        let runs = split_run_for_ime_underline(
            run,
            0..display_text.len(),
            ime_marked_range,
            marked_run.underline,
        )
        .into_vec();

        // Create shaped lines for whitespace indicators before layout
        let whitespace_indicators =
            Self::layout_whitespace_indicators(state, text_size, &text_style, window, cx);

        let lines = Self::layout_lines(
            state,
            display_text,
            &last_layout,
            text_size,
            &runs,
            whitespace_indicators,
            window,
        );

        let mut longest_line_width = wrap_width.unwrap_or(px(0.));
        // 1. Single line
        // 2. Multi-line with soft wrap disabled.
        if state.is_single_line() || !state.soft_wrap {
            let longest_row = state.display_map.longest_row();
            let longest_line: SharedString = state.text.slice_line(longest_row).to_string().into();
            longest_line_width = window
                .text_system()
                .shape_line(
                    longest_line.clone(),
                    text_size,
                    &[TextRun {
                        len: longest_line.len(),
                        font: style.font(),
                        color: gpui::black(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    wrap_width,
                )
                .width;
        }
        last_layout.lines = Rc::new(lines);

        let mut scroll_size = size(
            if longest_line_width + RIGHT_MARGIN > bounds.size.width {
                longest_line_width + RIGHT_MARGIN
            } else {
                longest_line_width
            },
            (state.display_map.display_row_count() as f32 * line_height).max(bounds.size.height),
        );

        // TODO: should be add some gap to right, to convenient to focus on boundary position
        if last_layout.text_align == TextAlign::Right || last_layout.text_align == TextAlign::Center
        {
            scroll_size.width = longest_line_width;
        }

        // `position_for_index` for example
        //
        // #### text
        //
        // Hello 世界，this is GPUI component.
        // The GPUI Component is a collection of UI components for
        // GPUI framework, including Button, Input, Checkbox, Radio,
        // Dropdown, Tab, and more...
        //
        // wrap_width: 444px, line_height: 20px
        //
        // #### lines[0]
        //
        // | index | pos              | line |
        // |-------|------------------|------|
        // | 5     | (37 px, 0.0)     | 0    |
        // | 38    | (261.7 px, 20.0) | 0    |
        // | 40    | None             | -    |
        //
        // #### lines[1]
        //
        // | index | position              | line |
        // |-------|-----------------------|------|
        // | 5     | (43.578125 px, 0.0)   | 0    |
        // | 56    | (422.21094 px, 0.0)   | 0    |
        // | 57    | (11.6328125 px, 20.0) | 1    |
        // | 114   | (429.85938 px, 20.0)  | 1    |
        // | 115   | (11.3125 px, 40.0)    | 2    |

        // Calculate the scroll offset to keep the cursor in view
        let input_bounds = bounds;
        let (cursor_bounds, cursor_scroll_offset) =
            self.layout_cursor(&last_layout, &mut bounds, scroll_size, window, cx);
        last_layout.cursor_bounds = cursor_bounds;

        let selection_path = self.layout_selections(&last_layout, &mut bounds, window, cx);

        let state = self.state.read(cx);
        state
            .editor_scrollbar_snapshot
            .set(Some(EditorScrollbarSnapshot::new(
                input_bounds,
                scroll_size,
                cursor_scroll_offset,
                state,
            )));

        PrepaintState {
            bounds,
            last_layout,
            scroll_size,
            cursor_bounds,
            cursor_scroll_offset,
            selection_path,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        input_bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (focus_handle, show_cursor, selected_range, editor_style) = {
            let state = self.state.read(cx);
            (
                state.focus_handle.clone(),
                state.show_cursor(window, cx),
                state.selected_range,
                state.editor_style.clone(),
            )
        };
        let focused = focus_handle.is_focused(window);
        let bounds = prepaint.bounds;
        let text_align = prepaint.last_layout.text_align;

        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.state.clone()),
            cx,
        );

        let line_height = window.line_height();
        let origin = bounds.origin;

        // Keep scrollbar offset always be positive，Start from the left position
        let scroll_offset = if text_align == TextAlign::Right {
            (prepaint.scroll_size.width - prepaint.bounds.size.width).max(px(0.))
        } else if text_align == TextAlign::Center {
            (prepaint.scroll_size.width - prepaint.bounds.size.width)
                .half()
                .max(px(0.))
        } else {
            px(0.)
        };

        if window.is_window_active()
            && let Some(path) = prepaint.selection_path.take()
        {
            window.paint_path(path, editor_style.selection);
        }

        let mut offset_y = prepaint.last_layout.visible_top;
        for line in prepaint.last_layout.lines.iter() {
            let p = point(origin.x + scroll_offset, origin.y + offset_y);
            line.paint(
                p,
                line_height,
                text_align,
                Some(prepaint.last_layout.content_width),
                window,
                cx,
            );
            offset_y += line.size(line_height).height;
        }

        if focused
            && show_cursor
            && let Some(cursor_bounds) = prepaint.cursor_bounds_with_scroll()
        {
            let caret = device_pixel_caret(cursor_bounds, window.scale_factor());
            window.paint_quad(fill(caret, editor_style.caret));
        }

        self.state.update(cx, |state, cx| {
            state.last_layout = Some(prepaint.last_layout.clone());
            state.last_bounds = Some(bounds);
            state.last_cursor = Some(state.cursor());
            state.set_input_bounds(input_bounds, cx);
            state.last_selected_range = Some(selected_range);
            state.scroll_size = prepaint.scroll_size;
            state.update_scroll_offset(Some(prepaint.cursor_scroll_offset), cx);
            state.deferred_scroll_offset = None;

            cx.notify();
        });

        self.paint_mouse_listeners(window, cx);
    }
}

/// Split placeholder text into display lines and trim runs to each line.
fn placeholder_line_runs<'a>(
    display_text: &'a str,
    runs: &[TextRun],
) -> Vec<(&'a str, Vec<TextRun>)> {
    let mut result = Vec::new();
    let mut line_offset = 0;

    for line in display_text.split('\n') {
        let line_runs = runs_for_range(runs, line_offset, &(0..line.len()));
        debug_assert_eq!(
            line_runs.iter().map(|run| run.len).sum::<usize>(),
            line.len()
        );
        result.push((line, line_runs));
        // Advance in the whole-placeholder coordinate space, including the separator.
        line_offset += line.len() + 1;
    }

    result
}

/// Get the runs for the given range.
///
/// The range is the byte range of the wrapped line.
pub(super) fn runs_for_range(
    runs: &[TextRun],
    line_offset: usize,
    range: &Range<usize>,
) -> Vec<TextRun> {
    let mut result = vec![];
    let range = (line_offset + range.start)..(line_offset + range.end);
    let mut cursor = 0;

    for run in runs {
        let run_start = cursor;
        let run_end = cursor + run.len;

        if run_end <= range.start {
            cursor = run_end;
            continue;
        }

        if run_start >= range.end {
            break;
        }

        let start = range.start.max(run_start) - run_start;
        let end = range.end.min(run_end) - run_start;
        let len = end - start;

        if len > 0 {
            result.push(TextRun { len, ..run.clone() });
        }

        cursor = run_end;
    }

    result
}

/// Snap run boundaries to char boundaries of `text` and cap them at its
/// length. Style ranges are byte offsets that can drift off char boundaries
/// (a stale syntax tree, a range in the wrong coordinate space) and the
/// platform text system panics when a run splits a multi-byte character.
///
/// Returns `None` when the runs are already aligned (the common case).
pub(super) fn align_runs_to_char_boundaries(text: &str, runs: &[TextRun]) -> Option<Vec<TextRun>> {
    let mut end = 0;
    let aligned = runs.iter().all(|run| {
        end += run.len;
        end <= text.len() && text.is_char_boundary(end)
    });
    if aligned {
        return None;
    }

    let mut result = Vec::with_capacity(runs.len());
    let mut cursor = 0;
    let mut raw_end = 0;
    for run in runs {
        raw_end = (raw_end + run.len).min(text.len());
        let mut end = raw_end;
        while !text.is_char_boundary(end) {
            end += 1;
        }
        if end > cursor {
            result.push(TextRun {
                len: end - cursor,
                ..run.clone()
            });
            cursor = end;
        }
    }

    Some(result)
}

fn split_run_for_ime_underline(
    run: TextRun,
    run_range: Range<usize>,
    marked_range: Option<Range<usize>>,
    marked_underline: Option<UnderlineStyle>,
) -> SmallVec<[TextRun; 3]> {
    if run.len == 0 {
        return SmallVec::new();
    }

    let Some(marked) = marked_range else {
        return [run].into_iter().collect();
    };

    let intersection_start = run_range.start.max(marked.start);
    let intersection_end = run_range.end.min(marked.end);
    if intersection_start >= intersection_end {
        return [run].into_iter().collect();
    }

    [
        TextRun {
            len: intersection_start - run_range.start,
            ..run.clone()
        },
        TextRun {
            len: intersection_end - intersection_start,
            underline: marked_underline,
            ..run.clone()
        },
        TextRun {
            len: run_range.end - intersection_end,
            ..run
        },
    ]
    .into_iter()
    .filter(|run| run.len > 0)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_paints_on_whole_device_pixels_at_a_constant_width() {
        for (scale_factor, device_width) in [(1., 1.), (1.25, 1.), (1.5, 2.), (2., 2.)] {
            for step in 0..20 {
                let unsnapped = Bounds::new(
                    point(px(40. + step as f32 * 0.13), px(17.45)),
                    size(CURSOR_WIDTH, px(22.1)),
                );
                let caret = device_pixel_caret(unsnapped, scale_factor);
                let device = |value: Pixels| f32::from(value) * scale_factor;

                for edge in [caret.left(), caret.right(), caret.top(), caret.bottom()] {
                    let edge = device(edge);
                    assert!(
                        (edge - edge.round()).abs() < 1e-3,
                        "edge {edge} is between device pixels at {scale_factor}x"
                    );
                }
                assert!(
                    (device(caret.size.width) - device_width).abs() < 1e-3,
                    "caret is {} device pixels wide at {scale_factor}x",
                    device(caret.size.width)
                );
                assert!(
                    (device(caret.left()) - device(unsnapped.left())).abs() <= 0.5 + 1e-3,
                    "the caret stays at its text position"
                );
            }
        }
    }

    #[test]
    fn test_editor_scrollbar_layout_uses_current_scroll_size() {
        let input_bounds = Bounds::new(point(px(10.), px(20.)), size(px(300.), px(80.)));
        let paddings = Edges {
            top: px(2.),
            right: px(3.),
            bottom: px(5.),
            left: px(7.),
        };

        let layout = EditorScrollbarLayout::new(input_bounds, size(px(500.), px(120.)), paddings);

        assert_eq!(
            layout.bounds,
            Bounds::new(point(px(10.), px(18.)), size(px(303.), px(87.)))
        );
        assert_eq!(layout.scroll_size, size(px(513.), px(120.)));
    }

    #[test]
    fn test_auto_grow_scroll_offset_is_clamped_to_current_viewport() {
        let mode = LayoutMode::auto_grow(3, 8);

        assert_eq!(
            clamp_auto_grow_vertical_scroll_offset(&mode, px(-260.), px(340.), px(160.)),
            px(-180.)
        );
        assert_eq!(
            clamp_auto_grow_vertical_scroll_offset(&mode, px(-40.), px(340.), px(160.)),
            px(-40.)
        );
        assert_eq!(
            clamp_auto_grow_vertical_scroll_offset(&mode, px(20.), px(340.), px(160.)),
            px(0.)
        );

        let plain_text = LayoutMode::plain_text();
        assert_eq!(
            clamp_auto_grow_vertical_scroll_offset(&plain_text, px(-260.), px(340.), px(160.)),
            px(-260.)
        );
    }

    #[test]
    fn test_runs_for_range() {
        let run = TextRun {
            len: 0,
            font: gpui::font(".SystemUIFont"),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        // use hello this-is-test
        let runs = vec![
            // use
            TextRun {
                len: 3,
                ..run.clone()
            },
            // \s
            TextRun {
                len: 1,
                ..run.clone()
            },
            // hello
            TextRun {
                len: 5,
                ..run.clone()
            },
            // \s
            TextRun {
                len: 1,
                ..run.clone()
            },
            // this-is-test
            TextRun {
                len: 12,
                ..run.clone()
            },
        ];

        #[track_caller]
        fn assert_runs(actual: Vec<TextRun>, expected: &[usize]) {
            let left = actual.iter().map(|run| run.len).collect::<Vec<_>>();
            assert_eq!(left, expected);
        }

        assert_runs(runs_for_range(&runs, 0, &(0..0)), &[]);
        assert_runs(runs_for_range(&runs, 0, &(0..100)), &[3, 1, 5, 1, 12]);

        assert_runs(runs_for_range(&runs, 0, &(0..6)), &[3, 1, 2]);
        assert_runs(runs_for_range(&runs, 0, &(1..6)), &[2, 1, 2]);
        assert_runs(runs_for_range(&runs, 0, &(3..10)), &[1, 5, 1]);
        assert_runs(runs_for_range(&runs, 0, &(5..8)), &[3]);
        assert_runs(runs_for_range(&runs, 3, &(0..3)), &[1, 2]);
        assert_runs(runs_for_range(&runs, 3, &(2..10)), &[4, 1, 3]);
        assert_runs(runs_for_range(&runs, 9, &(0..8)), &[1, 7]);
    }

    #[test]
    fn test_split_runs_preserve_ime_underline_across_highlight_boundaries() {
        let underline = UnderlineStyle {
            thickness: px(1.),
            color: Some(gpui::black()),
            wavy: false,
        };

        let runs = [0..4, 4..10]
            .into_iter()
            .flat_map(|range| {
                split_run_for_ime_underline(
                    TextStyle::default().to_run(range.len()),
                    range,
                    Some(2..7),
                    Some(underline),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            runs.iter()
                .map(|run| (run.len, run.underline.is_some()))
                .collect::<Vec<_>>(),
            vec![(2, false), (2, true), (3, true), (3, false)]
        );
    }

    #[test]
    fn test_split_run_applies_ime_underline_without_highlighting() {
        let underline = UnderlineStyle {
            thickness: px(1.),
            color: Some(gpui::black()),
            wavy: false,
        };

        let runs = split_run_for_ime_underline(
            TextStyle::default().to_run(10),
            0..10,
            Some(2..7),
            Some(underline),
        );

        assert_eq!(
            runs.iter()
                .map(|run| (run.len, run.underline.is_some()))
                .collect::<Vec<_>>(),
            vec![(2, false), (5, true), (3, false)]
        );
        assert!(
            split_run_for_ime_underline(TextStyle::default().to_run(0), 0..0, None, None)
                .is_empty()
        );
    }

    #[test]
    fn test_masked_ime_underline_splits_on_mask_char_boundaries() {
        let underline = UnderlineStyle {
            thickness: px(1.),
            color: Some(gpui::black()),
            wavy: false,
        };
        let text = Rope::from("abcdef");
        let mask_len = MASK_CHAR.len_utf8();

        assert_eq!(
            ime_marked_display_range(&text, Some(4..6), false),
            Some(4..6)
        );
        assert_eq!(ime_marked_display_range(&text, None, true), None);
        assert_eq!(
            ime_marked_display_range(&text, Some(4..6), true),
            Some(4 * mask_len..6 * mask_len)
        );

        let display_text = MASK_CHAR.to_string().repeat(text.chars().count());
        let runs = split_run_for_ime_underline(
            TextStyle::default().to_run(display_text.len()),
            0..display_text.len(),
            ime_marked_display_range(&text, Some(4..6), true),
            Some(underline),
        );

        let mut offset = 0;
        for run in &runs {
            assert!(display_text.is_char_boundary(offset));
            offset += run.len;
        }
        assert_eq!(offset, display_text.len());
    }

    #[test]
    fn test_placeholder_line_runs() {
        let run = TextRun {
            len: 0,
            font: gpui::font(".SystemUIFont"),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        let runs = vec![
            TextRun {
                len: 2,
                ..run.clone()
            },
            TextRun {
                len: 2,
                ..run.clone()
            },
            TextRun { len: 1, ..run },
        ];

        let placeholder_runs = placeholder_line_runs("ab\n\nc", &runs);

        let lines = placeholder_runs
            .iter()
            .map(|(line, _)| *line)
            .collect::<Vec<_>>();
        assert_eq!(lines, vec!["ab", "", "c"]);

        let run_lengths = placeholder_runs
            .iter()
            .map(|(_, line_runs)| line_runs.iter().map(|run| run.len).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        assert_eq!(run_lengths, vec![vec![2], vec![], vec![1]]);
    }

    #[test]
    fn test_align_runs_to_char_boundaries() {
        let run = TextRun {
            len: 0,
            font: gpui::font(".SystemUIFont"),
            color: gpui::blue(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = |lens: &[usize]| {
            lens.iter()
                .map(|&len| TextRun { len, ..run.clone() })
                .collect::<Vec<_>>()
        };
        let lens = |runs: &[TextRun]| runs.iter().map(|run| run.len).collect::<Vec<_>>();

        // "你好，世界" is 15 bytes; boundaries at 0, 3, 6, 9, 12, 15.
        let text = "你好，世界";

        // Aligned runs pass through untouched.
        assert_eq!(align_runs_to_char_boundaries(text, &runs(&[3, 12])), None);

        // A mid-char boundary (4) snaps up to the next boundary (6).
        let aligned = align_runs_to_char_boundaries(text, &runs(&[4, 11])).unwrap();
        assert_eq!(lens(&aligned), vec![6, 9]);

        // Several boundaries inside the same char collapse into one run.
        let aligned = align_runs_to_char_boundaries(text, &runs(&[1, 1, 13])).unwrap();
        assert_eq!(lens(&aligned), vec![3, 12]);

        // Overshooting runs are capped at the text length.
        let aligned = align_runs_to_char_boundaries(text, &runs(&[3, 20])).unwrap();
        assert_eq!(lens(&aligned), vec![3, 12]);

        // Undershooting runs keep the shortfall (the tail is left unshaped,
        // matching `runs_for_range` clipping).
        assert_eq!(align_runs_to_char_boundaries(text, &runs(&[3, 3])), None);
    }

    #[test]
    fn test_cursor_surrounding_padding_auto_grow() {
        // Auto-grow inputs always pad by one line, regardless of any
        // override or visible-lines count.
        let line_height = px(20.);
        for override_lines in [None, Some(0), Some(3), Some(99)] {
            for visible_lines in [0_usize, 1, 8, 64] {
                assert_eq!(
                    cursor_surrounding_padding(true, override_lines, visible_lines, line_height,),
                    line_height,
                );
            }
        }
    }

    #[test]
    fn test_cursor_surrounding_padding_default() {
        // `None`: historical heuristic — `BOTTOM_MARGIN_ROWS` for normal
        // viewports, falls back to one line on small viewports (less
        // than `BOTTOM_MARGIN_ROWS × 8` rows tall).
        let line_height = px(20.);

        // Small viewport → 1-line fallback.
        let small = BOTTOM_MARGIN_ROWS * 8 - 1;
        assert_eq!(
            cursor_surrounding_padding(false, None, small, line_height),
            line_height,
        );

        // Boundary at `BOTTOM_MARGIN_ROWS × 8` flips to the full margin.
        let boundary = BOTTOM_MARGIN_ROWS * 8;
        assert_eq!(
            cursor_surrounding_padding(false, None, boundary, line_height),
            BOTTOM_MARGIN_ROWS * line_height,
        );

        // Comfortably-large viewport.
        assert_eq!(
            cursor_surrounding_padding(false, None, 100, line_height),
            BOTTOM_MARGIN_ROWS * line_height,
        );
    }

    #[test]
    fn test_cursor_surrounding_padding_explicit() {
        // `Some(n)`: exactly `n × line_height` when the viewport has
        // room for it; saturated against half the viewport when it
        // doesn't.
        let line_height = px(20.);

        for lines in [0_usize, 1, 2, 5, 50] {
            let raw = lines as f32 * line_height;
            for visible_lines in [0_usize, 1, 8, 100] {
                let viewport_half = (visible_lines as f32 * line_height).half();
                assert_eq!(
                    cursor_surrounding_padding(false, Some(lines), visible_lines, line_height,),
                    raw.min(viewport_half),
                );
            }
        }
    }

    #[test]
    fn test_cursor_surrounding_padding_saturates_against_viewport() {
        // An aggressive override on a small viewport must not produce a
        // padding larger than half the visible region — otherwise the
        // bottom-edge auto-scroll-into-view threshold sinks below the
        // top-edge threshold and the per-frame scroll adjustment loses
        // a stable fixed point.
        let line_height = px(20.);

        // Override much larger than viewport → clamped to half.
        let visible_lines = 10;
        let viewport_half = (visible_lines as f32 * line_height).half();
        assert_eq!(
            cursor_surrounding_padding(false, Some(50), visible_lines, line_height),
            viewport_half,
        );

        // Override that fits → returned unchanged.
        let visible_lines = 40;
        assert_eq!(
            cursor_surrounding_padding(false, Some(3), visible_lines, line_height),
            3.0 * line_height,
        );

        // Default heuristic still saturates if BOTTOM_MARGIN_ROWS would
        // exceed the half-viewport bound (only possible at extreme
        // sizes — kept for defensive completeness).
        let visible_lines = BOTTOM_MARGIN_ROWS * 8;
        let half = (visible_lines as f32 * line_height).half();
        let raw = BOTTOM_MARGIN_ROWS * line_height;
        assert_eq!(
            cursor_surrounding_padding(false, None, visible_lines, line_height),
            raw.min(half),
        );
    }
}
