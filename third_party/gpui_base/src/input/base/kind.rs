//! Compile-time input modes.
//!
//! The two input states are one engine seen through a mode marker, one per
//! state:
//!
//! ```ignore
//! pub type InputState    = InputBaseState<InputMode>;
//! pub type TextareaState = InputBaseState<TextareaMode>;
//! ```
//!
//! A method that only makes sense for one mode lives in that mode's `impl`
//! block, so it does not exist on the other: `InputState` has no `auto_grow`
//! or `soft_wrap`, and `TextareaState` has no `masked`. Methods for
//! multi-line modes go on [`MultiLineMode`]. Reaching for the wrong one is a
//! compile error rather than a debug assertion.
//!
//! [`super::LayoutMode`] carries how many rows to show and how to grow, since
//! the engine branches on it while editing. The marker only decides which API
//! is reachable.

/// A single-line text field: the mode of [`crate::input::InputState`].
pub struct InputMode;

/// Ordinary multi-line text: the mode of [`crate::input::TextareaState`].
pub struct TextareaMode;

mod sealed {
    pub trait Sealed {}
}

impl sealed::Sealed for InputMode {}
impl sealed::Sealed for TextareaMode {}

/// The modes whose layout spans more than one line: [`TextareaMode`].
///
/// Soft wrap, wrapping indent and tab size are meaningless in a single-line
/// field. Bounding an `impl` block on this trait keeps those methods off
/// [`InputMode`].
pub trait MultiLineMode: InputModeKind {}

impl MultiLineMode for TextareaMode {}

/// The mode marker of an [`super::InputBaseState`].
///
/// Sealed: the engine branches on a closed set of runtime modes, so the
/// markers are a closed set too. The two above are all of them.
pub trait InputModeKind: sealed::Sealed + Sized + 'static {
    /// Whether this kind of input spans more than one line.
    ///
    /// The kind decides this, not the layout: [`super::LayoutMode`] carries
    /// how many rows to show and how to grow, which is a different question
    /// from whether the input is a text field or a document. Deriving it from
    /// the layout let the two disagree — an auto-growing textarea capped at
    /// one row used to report itself as single-line.
    const MULTI_LINE: bool;
}

impl InputModeKind for InputMode {
    const MULTI_LINE: bool = false;
}

impl InputModeKind for TextareaMode {
    const MULTI_LINE: bool = true;
}
