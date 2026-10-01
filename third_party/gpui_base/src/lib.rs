//! Behavior and infrastructure foundations for GPUI applications.
//!
//! Primitives deliberately avoid presentation styles. Layout, positioning,
//! colors, sizing, and motion belong to applications or the
//! `gpui-component` façade.
//!
//! This pinned copy retains only the behavior used by Aeris: `init`, global
//! state, theme tokens, buttons, switches, the text-input engine, and their
//! bounded interaction helpers. Upstream presentation modules such as dock,
//! calendar, picker, table, tree, sheet, text-view, and animation stacks are
//! intentionally omitted from this crate.

pub mod actions;
pub mod animation;
#[doc(hidden)]
pub mod async_util;
mod auto_scroll;
mod button;
pub mod component_traits;
mod element_ext;
mod event;
mod geometry;
mod global_state;
pub mod input;
#[cfg(all(target_os = "macos", not(test)))]
mod macos_accessibility;
mod measure;
mod scrollbar;
mod state_style;
mod styled;
mod switch;
mod text_boundary;
mod theme;
pub mod theme_tokens;

pub use auto_scroll::AutoScroll;
pub use button::{Button, ButtonStyles};
pub use component_traits::FocusableExt;
pub use component_traits::{Disableable, Selectable};
pub use element_ext::ElementExt;
pub use event::{InteractiveElementExt, OngoingScrollExt};
pub use geometry::*;
pub use global_state::GlobalState;
pub use input::{
    Editor, Input, InputBase, InputStyles, NumberInputEvent, NumberStep, StepAction, Textarea,
};
#[cfg(all(target_os = "macos", not(test)))]
#[doc(hidden)]
pub use macos_accessibility::install_window_hit_test_forwarder;
pub use measure::{Measure, measure, measure_if};
pub use scrollbar::{Scrollbar, ScrollbarHandle, ScrollbarMode, ScrollbarMotion, ScrollbarStyles};
pub use state_style::StateStyle;
#[cfg(any(feature = "inspector", debug_assertions))]
pub use styled::styled_ext_reflection_methods;
pub use styled::{RoleOverride, StyledExt, box_shadow, h_flex, v_flex};
pub use switch::{
    Switch, SwitchStyles, SwitchThumb, SwitchThumbStyles, SwitchTrack, SwitchTrackStyles,
};
pub use theme::{ScrollbarTheme, Theme, ThemeAppearance};
pub use theme_tokens::{
    ColorTokens, RadiusTokens, SemanticThemeTokens, ShadowTokens, SpacingTokens, TextStyleToken,
    TypographyTokens,
};

use gpui::App;

/// Initializes global infrastructure owned by the base layer.
pub fn init(cx: &mut App) {
    let _ = Theme::global_mut(cx);
    GlobalState::init(cx);
    input::init(cx);
}
