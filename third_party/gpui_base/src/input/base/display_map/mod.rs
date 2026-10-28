//! Soft-wrap projection of the buffer for multi-line inputs.
//!
//! Every buffer line occupies one or more display rows, one per soft-wrapped
//! segment. Single-line inputs never wrap, so their display rows are their
//! buffer lines.
mod text_wrapper;

use std::ops::Range;

use gpui::{App, Font, Pixels};
use ropey::Rope;

use self::text_wrapper::TextWrapper;
pub use self::text_wrapper::WrappingIndent;
pub(crate) use self::text_wrapper::{LineItem, LineLayout, WrapDisplayPoint};

/// Maps buffer offsets and lines to soft-wrapped display rows.
pub(crate) struct DisplayMap {
    wrapper: TextWrapper,
}

impl DisplayMap {
    pub(crate) fn new(font: Font, font_size: Pixels, wrap_width: Option<Pixels>) -> Self {
        Self {
            wrapper: TextWrapper::new(font, font_size, wrap_width),
        }
    }

    /// Total number of display rows.
    #[inline]
    pub(crate) fn display_row_count(&self) -> usize {
        self.wrapper.len()
    }

    /// Total number of buffer lines.
    #[inline]
    pub(crate) fn buffer_line_count(&self) -> usize {
        self.wrapper.lines_count()
    }

    /// The buffer line a display row belongs to, clamped to the last line.
    pub(crate) fn display_row_to_buffer_line(&self, display_row: usize) -> usize {
        self.wrapper.wrap_row_to_buffer_line(display_row)
    }

    /// The first display row of a buffer line.
    pub(crate) fn buffer_line_to_display_row(&self, line: usize) -> usize {
        self.wrapper.buffer_line_to_first_wrap_row(line)
    }

    /// Re-wraps the lines touched by an edit.
    pub(crate) fn on_text_changed(
        &mut self,
        changed_text: &Rope,
        range: &Range<usize>,
        new_text: &Rope,
        cx: &mut App,
    ) {
        self.wrapper.update(changed_text, range, new_text, cx);
    }

    /// Re-wraps every line for a new wrap width.
    pub(crate) fn on_layout_changed(&mut self, wrap_width: Option<Pixels>, cx: &mut App) {
        self.wrapper.set_wrap_width(wrap_width, cx);
    }

    /// Sets how continuation rows are indented.
    pub(crate) fn set_wrapping_indent(&mut self, wrapping_indent: WrappingIndent, cx: &mut App) {
        self.wrapper.set_wrapping_indent(wrapping_indent, cx);
    }

    pub(crate) fn set_font(&mut self, font: Font, font_size: Pixels, cx: &mut App) {
        self.wrapper.set_font(font, font_size, cx);
    }

    /// Wraps `text` the first time the input is laid out.
    pub(crate) fn ensure_text_prepared(&mut self, text: &Rope, cx: &mut App) {
        self.wrapper.prepare_if_need(text, cx);
    }

    /// Converts a byte offset to its wrapped display point.
    #[inline]
    pub(crate) fn offset_to_wrap_display_point(&self, offset: usize) -> WrapDisplayPoint {
        self.wrapper.offset_to_display_point(offset)
    }

    /// Like [`Self::offset_to_wrap_display_point`], but honours the caret's line-end affinity so
    /// an offset on a soft wrap boundary resolves to the row the caret is drawn on.
    #[inline]
    pub(crate) fn offset_to_wrap_display_point_with_affinity(
        &self,
        offset: usize,
        line_end_affinity: bool,
    ) -> WrapDisplayPoint {
        self.wrapper
            .offset_to_display_point_with_affinity(offset, line_end_affinity)
    }

    /// Converts a wrapped display point back to a byte offset.
    #[inline]
    pub(crate) fn wrap_display_point_to_offset(&self, point: WrapDisplayPoint) -> usize {
        self.wrapper.display_point_to_offset(point)
    }

    /// The buffer row of the longest line, by byte length.
    #[inline]
    pub(crate) fn longest_row(&self) -> usize {
        self.wrapper.longest_row()
    }

    /// The wrap information of a buffer line.
    #[inline]
    pub(crate) fn line(&self, row: usize) -> Option<&LineItem> {
        self.wrapper.line(row)
    }
}
