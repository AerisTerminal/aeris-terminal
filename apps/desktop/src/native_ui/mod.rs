pub(crate) mod color_picker;
pub(crate) mod control;
pub(crate) mod icon;
pub(crate) mod input;
pub(crate) mod loader;
pub(crate) mod menu;
pub(crate) mod scroll;
pub(crate) mod tab;
pub(crate) mod theme;
pub(crate) mod tooltip;

use std::sync::Arc;

use gpui::{FontFeatures, FontWeight};
use tradingplot_design_system::{TypographyRole, platform_typography};

/// GPUI adapter for the semantic weights owned by `platform.css`.
pub(crate) fn platform_font_weight(role: TypographyRole) -> FontWeight {
    FontWeight(f32::from(platform_typography().weight(role)))
}

/// GPUI OpenType settings for changing numeric readouts.
pub(crate) fn platform_tabular_numerals() -> FontFeatures {
    FontFeatures(Arc::new(vec![(
        platform_typography().tabular_numerals_feature().to_owned(),
        1,
    )]))
}
