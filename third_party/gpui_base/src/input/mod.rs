//! Text input: the shared editing engine and the two states built on it.
//!
//! Nothing here should be `pub` unless it is reachable from outside the crate.
//! A `pub` on an item behind a private module says something the module path
//! contradicts, and it hides the real API surface from anyone reading it.
#![warn(unreachable_pub)]

use gpui::{App, EventEmitter};
use std::rc::Rc;

/// Character used by masked editor modes.
pub(crate) const MASK_CHAR: char = '•';

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepAction {
    Decrement,
    Increment,
}

/// Computes the step from the current value and the stepping direction.
type StepFn = Rc<dyn Fn(f64, StepAction, &mut App) -> f64>;

/// Strategy used by numeric editors when stepping their value.
#[derive(Clone)]
pub enum NumberStep {
    Fixed(f64),
    ByValue(StepFn),
}

impl NumberStep {
    pub fn by_value(f: impl Fn(f64, StepAction, &mut gpui::App) -> f64 + 'static) -> Self {
        Self::ByValue(Rc::new(f))
    }

    pub fn value(&self, current: f64, action: StepAction, cx: &mut gpui::App) -> f64 {
        match self {
            Self::Fixed(step) => *step,
            Self::ByValue(f) => f(current, action, cx),
        }
    }
}

impl From<f64> for NumberStep {
    fn from(step: f64) -> Self {
        Self::Fixed(step)
    }
}

#[derive(Clone)]
pub enum NumberInputEvent {
    Step(StepAction),
}

mod base;
#[path = "base/blink_cursor.rs"]
pub(crate) mod blink_cursor;
#[path = "base/change.rs"]
mod change;
#[path = "base/cursor.rs"]
mod cursor;
#[path = "base/display_map/mod.rs"]
mod display_map;
#[path = "base/editor_style.rs"]
mod editor_style;
#[path = "base/element.rs"]
mod element;
#[path = "base/indent.rs"]
mod indent;
#[path = "base/kind.rs"]
mod kind;
#[path = "base/layout.rs"]
mod layout;
#[path = "base/mask_pattern.rs"]
mod mask_pattern;
#[path = "base/mode.rs"]
mod mode;
#[path = "base/movement.rs"]
mod movement;
#[path = "base/native.rs"]
mod native;
#[path = "base/rope_ext.rs"]
mod rope_ext;
#[path = "base/selection.rs"]
mod selection;
mod single_line;
#[path = "base/state.rs"]
mod state;
mod textarea;
#[path = "base/undo_manager.rs"]
mod undo_manager;

pub(crate) fn init(cx: &mut App) {
    state::init(cx);
}

pub use base::{InputBase, InputContextMenuCapabilities, InputStyles};
pub use cursor::Selection;
pub(crate) use display_map::DisplayMap;
pub use display_map::WrappingIndent;
pub use editor_style::InputEditorStyle;
pub use indent::TabSize;
pub use single_line::{Input, InputState};
impl EventEmitter<NumberInputEvent> for InputState {}
pub use kind::{InputMode, InputModeKind, MultiLineMode, TextareaMode};
pub use mask_pattern::MaskPattern;
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub use native::set_text_content_type;
pub use native::{NativeMenu, NativeMenuItem};
pub use rope_ext::{Point, RopeExt, RopeLines};
pub use ropey::Rope;
pub use state::*;
pub use textarea::{Textarea, TextareaState};
