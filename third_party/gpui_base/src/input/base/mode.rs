use super::DisplayMap;
use crate::input::TabSize;

/// How the input lays its text out: rows and growth.
///
/// This does not say what *kind* of input this is — that is fixed at the type
/// level by [`crate::input::InputModeKind`], and asking it here is what used to
/// let the two disagree.
#[derive(Clone)]
pub(crate) enum LayoutMode {
    /// A plain text input mode.
    PlainText { tab: TabSize, rows: usize },
    /// An auto grow input mode.
    AutoGrow {
        rows: usize,
        min_rows: usize,
        max_rows: usize,
    },
}

impl Default for LayoutMode {
    fn default() -> Self {
        LayoutMode::plain_text()
    }
}

impl LayoutMode {
    /// Create a plain input mode with default settings.
    pub(super) fn plain_text() -> Self {
        LayoutMode::PlainText {
            tab: TabSize::default(),
            rows: 1,
        }
    }

    /// Create an auto grow input mode with given min and max rows.
    pub(super) fn auto_grow(min_rows: usize, max_rows: usize) -> Self {
        LayoutMode::AutoGrow {
            rows: min_rows,
            min_rows,
            max_rows,
        }
    }

    #[inline]
    pub(super) fn is_auto_grow(&self) -> bool {
        matches!(self, LayoutMode::AutoGrow { .. })
    }

    pub(super) fn set_rows(&mut self, new_rows: usize) {
        match self {
            LayoutMode::PlainText { rows, .. } => {
                *rows = new_rows;
            }
            LayoutMode::AutoGrow {
                rows,
                min_rows,
                max_rows,
            } => {
                *rows = new_rows.clamp(*min_rows, *max_rows);
            }
        }
    }

    /// Grow the row count to fit the content.
    ///
    /// Callers gate this on the input being multi-line; a single-line field
    /// keeps its one row.
    pub(super) fn update_auto_grow(&mut self, display_map: &DisplayMap) {
        self.set_rows(display_map.display_row_count());
    }

    /// At least 1 row be return.
    pub(super) fn rows(&self) -> usize {
        match self {
            LayoutMode::PlainText { rows, .. } => *rows,
            LayoutMode::AutoGrow { rows, .. } => *rows,
        }
        .max(1)
    }

    pub(super) fn max_rows(&self) -> usize {
        match self {
            LayoutMode::AutoGrow { max_rows, .. } => *max_rows,
            _ => usize::MAX,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::input::{TabSize, mode::LayoutMode};

    #[test]
    fn test_plain() {
        let mode = LayoutMode::PlainText {
            tab: TabSize::default(),
            rows: 5,
        };
        assert_eq!(mode.rows(), 5);
        assert_eq!(mode.max_rows(), usize::MAX);

        let mode = LayoutMode::plain_text();
        assert_eq!(mode.rows(), 1);
    }

    #[test]
    fn test_auto_grow() {
        let mut mode = LayoutMode::auto_grow(2, 5);
        assert_eq!(mode.rows(), 2);
        assert_eq!(mode.max_rows(), 5);

        mode.set_rows(4);
        assert_eq!(mode.rows(), 4);

        mode.set_rows(1);
        assert_eq!(mode.rows(), 2);

        mode.set_rows(10);
        assert_eq!(mode.rows(), 5);
    }
}
