//! Typed native mapping of the Axiusflow `brand.css` contract.
//!
//! Token source expressions and resolved sRGB values share one registry. The
//! checked CSS manifest uses generated custom-property names, while painting
//! code consumes typed values without string lookup.

use std::f32::consts::PI;

/// The application-wide color mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThemeMode {
    Light,
    Dark,
}

impl ThemeMode {
    /// Returns the opposite application color mode.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }

    /// Returns the stable label used by settings and accessibility text.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// A resolved sRGB color with an independent alpha channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeColor {
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
}

impl ThemeColor {
    /// Resolves an eight-bit sRGB color.
    #[must_use]
    pub const fn from_rgb8(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red: red as f32 / 255.0,
            green: green as f32 / 255.0,
            blue: blue as f32 / 255.0,
            alpha: 1.0,
        }
    }

    /// Resolves a CSS Color 4 OKLCH value into clamped sRGB.
    #[must_use]
    pub fn from_oklch(lightness: f32, chroma: f32, hue_degrees: f32) -> Self {
        let hue_radians = hue_degrees * PI / 180.0;
        let ok_a = chroma * hue_radians.cos();
        let ok_b = chroma * hue_radians.sin();

        let light_response = lightness + 0.396_337_78 * ok_a + 0.215_803_76 * ok_b;
        let medium_response = lightness - 0.105_561_346 * ok_a - 0.063_854_17 * ok_b;
        let short_response = lightness - 0.089_484_18 * ok_a - 1.291_485_5 * ok_b;

        let light_linear = light_response.powi(3);
        let medium_linear = medium_response.powi(3);
        let short_linear = short_response.powi(3);

        let red_linear =
            4.076_741_7 * light_linear - 3.307_711_6 * medium_linear + 0.230_969_94 * short_linear;
        let green_linear =
            -1.268_438 * light_linear + 2.609_757_4 * medium_linear - 0.341_319_4 * short_linear;
        let blue_linear = -0.004_196_086_3 * light_linear - 0.703_418_6 * medium_linear
            + 1.707_614_7 * short_linear;

        Self {
            red: linear_to_srgb(red_linear),
            green: linear_to_srgb(green_linear),
            blue: linear_to_srgb(blue_linear),
            alpha: 1.0,
        }
    }

    /// Returns this color with a replaced alpha channel.
    #[must_use]
    pub fn with_alpha(self, alpha: f32) -> Self {
        Self {
            alpha: alpha.clamp(0.0, 1.0),
            ..self
        }
    }

    /// Returns the red sRGB channel in the inclusive `0.0..=1.0` range.
    #[must_use]
    pub const fn red(self) -> f32 {
        self.red
    }

    /// Returns the green sRGB channel in the inclusive `0.0..=1.0` range.
    #[must_use]
    pub const fn green(self) -> f32 {
        self.green
    }

    /// Returns the blue sRGB channel in the inclusive `0.0..=1.0` range.
    #[must_use]
    pub const fn blue(self) -> f32 {
        self.blue
    }

    /// Returns the alpha channel in the inclusive `0.0..=1.0` range.
    #[must_use]
    pub const fn alpha(self) -> f32 {
        self.alpha
    }

    /// Returns a packed `0xRRGGBB` value for native UI color constructors.
    #[must_use]
    pub fn rgb_u32(self) -> u32 {
        (u32::from(channel_to_u8(self.red)) << 16)
            | (u32::from(channel_to_u8(self.green)) << 8)
            | u32::from(channel_to_u8(self.blue))
    }
}

fn linear_to_srgb(channel: f32) -> f32 {
    let encoded = if channel <= 0.003_130_8 {
        12.92 * channel
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    };
    encoded.clamp(0.0, 1.0)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn channel_to_u8(channel: f32) -> u8 {
    (channel.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn css_custom_property(canonical_identifier: &str) -> String {
    format!("--{canonical_identifier}")
}

/// One canonical color token and its resolved theme value.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorToken {
    pub canonical_identifier: &'static str,
    pub source_expression: &'static str,
    pub resolved: ThemeColor,
}

impl ColorToken {
    const fn new(
        canonical_identifier: &'static str,
        source_expression: &'static str,
        resolved: ThemeColor,
    ) -> Self {
        Self {
            canonical_identifier,
            source_expression,
            resolved,
        }
    }

    /// Derives the CSS custom-property name from the canonical identifier.
    #[must_use]
    pub fn css_custom_property(&self) -> String {
        css_custom_property(self.canonical_identifier)
    }
}

/// Core, semantic, and trading colors resolved for one application mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeColors {
    pub surface: ThemeColor,
    pub surface_secondary: ThemeColor,
    pub border: ThemeColor,
    pub border_secondary: ThemeColor,
    pub input_fill: ThemeColor,
    pub input_border: ThemeColor,
    pub text_primary: ThemeColor,
    pub text_secondary: ThemeColor,
    pub text_muted: ThemeColor,
    pub hover_bg: ThemeColor,
    pub active_bg: ThemeColor,
    pub icon: ThemeColor,
    pub icon_active: ThemeColor,
    pub primary: ThemeColor,
    pub primary_foreground: ThemeColor,
    pub danger: ThemeColor,
    pub danger_foreground: ThemeColor,
    pub ring: ThemeColor,
    pub chart_1: ThemeColor,
    pub chart_2: ThemeColor,
    pub chart_3: ThemeColor,
    pub chart_4: ThemeColor,
    pub chart_5: ThemeColor,
    pub positive: ThemeColor,
    pub warning: ThemeColor,
}

/// A canonical logical length token.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LengthToken {
    pub canonical_identifier: &'static str,
    pub source_expression: &'static str,
    pub logical_pixels: f32,
}

impl LengthToken {
    /// Derives the CSS custom-property name from the canonical identifier.
    #[must_use]
    pub fn css_custom_property(self) -> String {
        css_custom_property(self.canonical_identifier)
    }
}

/// Application dimensions that are shared across native and browser shells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeDimensions {
    pub app_header_height: LengthToken,
}

/// A fully resolved Axiusflow theme suitable for a single paint revision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxiusflowTheme {
    pub mode: ThemeMode,
    pub colors: ThemeColors,
    pub dimensions: ThemeDimensions,
}

impl AxiusflowTheme {
    /// Resolves all foundational tokens for a mode.
    #[must_use]
    pub fn for_mode(mode: ThemeMode) -> Self {
        Self {
            mode,
            colors: match mode {
                ThemeMode::Light => light_colors(),
                ThemeMode::Dark => dark_colors(),
            },
            dimensions: ThemeDimensions {
                app_header_height: LengthToken {
                    canonical_identifier: "app_header_height",
                    source_expression: "2.75rem",
                    logical_pixels: 44.0,
                },
            },
        }
    }

    /// Resolves the dark theme used by the desktop terminal initially.
    #[must_use]
    pub fn dark() -> Self {
        Self::for_mode(ThemeMode::Dark)
    }

    /// Resolves the light theme.
    #[must_use]
    pub fn light() -> Self {
        Self::for_mode(ThemeMode::Light)
    }

    /// Resolves the opposite mode as one complete theme value.
    #[must_use]
    pub fn toggled(self) -> Self {
        Self::for_mode(self.mode.toggled())
    }

    /// Returns canonical source metadata for every foundational color resolved here.
    #[must_use]
    pub fn color_tokens(self) -> [ColorToken; 25] {
        let colors = self.colors;
        let dark = self.mode == ThemeMode::Dark;
        [
            ColorToken::new(
                "surface",
                mode_source(dark, "#ffffff", "#141414"),
                colors.surface,
            ),
            ColorToken::new(
                "surface-secondary",
                mode_source(dark, "#fcfcfc", "#1b1b1b"),
                colors.surface_secondary,
            ),
            ColorToken::new(
                "border",
                mode_source(dark, "#f5f5f5", "#1e1e1e"),
                colors.border,
            ),
            ColorToken::new(
                "border-secondary",
                mode_source(dark, "#f3f3f3", "#242424"),
                colors.border_secondary,
            ),
            ColorToken::new("input-fill", "var(--surface-secondary)", colors.input_fill),
            ColorToken::new(
                "input-border",
                "var(--border-secondary)",
                colors.input_border,
            ),
            ColorToken::new(
                "text-primary",
                mode_source(dark, "#404040", "#e3e3e3"),
                colors.text_primary,
            ),
            ColorToken::new(
                "text-secondary",
                mode_source(dark, "#7a7a7a", "#a2a2a2"),
                colors.text_secondary,
            ),
            ColorToken::new(
                "text-muted",
                mode_source(dark, "#dedede", "#424242"),
                colors.text_muted,
            ),
            ColorToken::new(
                "hover-bg",
                mode_source(dark, "rgb(239 239 239 / 35%)", "rgb(97 97 97 / 20%)"),
                colors.hover_bg,
            ),
            ColorToken::new(
                "active-bg",
                mode_source(dark, "rgb(239 239 239 / 45%)", "rgb(97 97 97 / 26%)"),
                colors.active_bg,
            ),
            ColorToken::new("icon", mode_source(dark, "#808080", "#a3a3a3"), colors.icon),
            ColorToken::new(
                "icon-active",
                mode_source(dark, "#414141", "#e5e5e5"),
                colors.icon_active,
            ),
            ColorToken::new("primary", "#3e63dd", colors.primary),
            ColorToken::new(
                "primary-foreground",
                "oklch(0.97 0.014 254.604)",
                colors.primary_foreground,
            ),
            ColorToken::new("danger", "oklch(0.6471 0.2288 22.47)", colors.danger),
            ColorToken::new("danger-foreground", "#ffffff", colors.danger_foreground),
            ColorToken::new(
                "ring",
                mode_source(dark, "oklch(0.708 0 0)", "oklch(0.556 0 0)"),
                colors.ring,
            ),
            ColorToken::new("chart-1", "#d4d4d4", colors.chart_1),
            ColorToken::new("chart-2", "#7a7a7a", colors.chart_2),
            ColorToken::new("chart-3", "#5f5f5f", colors.chart_3),
            ColorToken::new("chart-4", "#494949", colors.chart_4),
            ColorToken::new("chart-5", "#343434", colors.chart_5),
            ColorToken::new("positive", "#089981", colors.positive),
            ColorToken::new("warning", "oklch(0.768578 0.164801 70.108)", colors.warning),
        ]
    }
}

impl Default for AxiusflowTheme {
    fn default() -> Self {
        Self::dark()
    }
}

const fn mode_source(
    dark: bool,
    light_source: &'static str,
    dark_source: &'static str,
) -> &'static str {
    if dark { dark_source } else { light_source }
}

fn light_colors() -> ThemeColors {
    let surface = ThemeColor::from_rgb8(255, 255, 255);
    let surface_secondary = ThemeColor::from_rgb8(252, 252, 252);
    let border_secondary = ThemeColor::from_rgb8(243, 243, 243);
    let hover = ThemeColor::from_rgb8(239, 239, 239);
    ThemeColors {
        surface,
        surface_secondary,
        border: ThemeColor::from_rgb8(245, 245, 245),
        border_secondary,
        input_fill: surface_secondary,
        input_border: border_secondary,
        text_primary: ThemeColor::from_rgb8(64, 64, 64),
        text_secondary: ThemeColor::from_rgb8(122, 122, 122),
        text_muted: ThemeColor::from_rgb8(222, 222, 222),
        hover_bg: hover.with_alpha(0.35),
        active_bg: hover.with_alpha(0.45),
        icon: ThemeColor::from_rgb8(128, 128, 128),
        icon_active: ThemeColor::from_rgb8(65, 65, 65),
        primary: ThemeColor::from_rgb8(62, 99, 221),
        primary_foreground: ThemeColor::from_oklch(0.97, 0.014, 254.604),
        danger: ThemeColor::from_oklch(0.647_1, 0.228_8, 22.47),
        danger_foreground: surface,
        ring: ThemeColor::from_oklch(0.708, 0.0, 0.0),
        chart_1: ThemeColor::from_rgb8(212, 212, 212),
        chart_2: ThemeColor::from_rgb8(122, 122, 122),
        chart_3: ThemeColor::from_rgb8(95, 95, 95),
        chart_4: ThemeColor::from_rgb8(73, 73, 73),
        chart_5: ThemeColor::from_rgb8(52, 52, 52),
        positive: ThemeColor::from_rgb8(8, 153, 129),
        warning: ThemeColor::from_oklch(0.768_578, 0.164_801, 70.108),
    }
}

fn dark_colors() -> ThemeColors {
    let surface_secondary = ThemeColor::from_rgb8(27, 27, 27);
    let border_secondary = ThemeColor::from_rgb8(36, 36, 36);
    let hover = ThemeColor::from_rgb8(97, 97, 97);
    ThemeColors {
        surface: ThemeColor::from_rgb8(20, 20, 20),
        surface_secondary,
        border: ThemeColor::from_rgb8(30, 30, 30),
        border_secondary,
        input_fill: surface_secondary,
        input_border: border_secondary,
        text_primary: ThemeColor::from_rgb8(227, 227, 227),
        text_secondary: ThemeColor::from_rgb8(162, 162, 162),
        text_muted: ThemeColor::from_rgb8(66, 66, 66),
        hover_bg: hover.with_alpha(0.20),
        active_bg: hover.with_alpha(0.26),
        icon: ThemeColor::from_rgb8(163, 163, 163),
        icon_active: ThemeColor::from_rgb8(229, 229, 229),
        primary: ThemeColor::from_rgb8(62, 99, 221),
        primary_foreground: ThemeColor::from_oklch(0.97, 0.014, 254.604),
        danger: ThemeColor::from_oklch(0.647_1, 0.228_8, 22.47),
        danger_foreground: ThemeColor::from_rgb8(255, 255, 255),
        ring: ThemeColor::from_oklch(0.556, 0.0, 0.0),
        chart_1: ThemeColor::from_rgb8(212, 212, 212),
        chart_2: ThemeColor::from_rgb8(122, 122, 122),
        chart_3: ThemeColor::from_rgb8(95, 95, 95),
        chart_4: ThemeColor::from_rgb8(73, 73, 73),
        chart_5: ThemeColor::from_rgb8(52, 52, 52),
        positive: ThemeColor::from_rgb8(8, 153, 129),
        warning: ThemeColor::from_oklch(0.768_578, 0.164_801, 70.108),
    }
}

/// The complete set of concrete radius tokens. No additional radius is valid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadiusToken {
    Sm,
    Default,
    Full,
}

impl RadiusToken {
    /// Returns the canonical platform identifier.
    #[must_use]
    pub const fn canonical_identifier(self) -> &'static str {
        match self {
            Self::Sm => "radius-small",
            Self::Default => "radius-default",
            Self::Full => "radius-large",
        }
    }

    /// Derives the CSS custom-property name from the canonical identifier.
    #[must_use]
    pub fn css_custom_property(self) -> String {
        css_custom_property(self.canonical_identifier())
    }

    /// Returns the governing concrete radius in logical pixels.
    #[must_use]
    pub const fn logical_pixels(self) -> u16 {
        match self {
            Self::Sm => 4,
            Self::Default => 6,
            Self::Full => 999,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AxiusflowTheme, ColorToken, RadiusToken, ThemeColor};

    fn token_source<'a>(tokens: &'a [ColorToken], identifier: &str) -> &'a str {
        tokens
            .iter()
            .find(|token| token.canonical_identifier == identifier)
            .map(|token| token.source_expression)
            .expect("theme color token exists")
    }

    #[test]
    fn native_palettes_match_the_brand_contract() {
        let light = AxiusflowTheme::light().colors;
        let dark = AxiusflowTheme::dark().colors;

        assert_eq!(light.surface, ThemeColor::from_rgb8(255, 255, 255));
        assert_eq!(light.input_fill, light.surface_secondary);
        assert_eq!(light.input_border, light.border_secondary);
        assert_eq!(light.danger_foreground, light.surface);
        assert_eq!(dark.surface, ThemeColor::from_rgb8(20, 20, 20));
        assert_eq!(dark.surface_secondary, ThemeColor::from_rgb8(27, 27, 27));
        assert_eq!(dark.input_fill, dark.surface_secondary);
        assert_eq!(dark.input_border, dark.border_secondary);
        assert_eq!(dark.border, ThemeColor::from_rgb8(30, 30, 30));
        assert_eq!(dark.border_secondary, ThemeColor::from_rgb8(36, 36, 36));
        assert_eq!(light.positive, ThemeColor::from_rgb8(8, 153, 129));
        assert_eq!(light.primary, ThemeColor::from_rgb8(62, 99, 221));
        assert_eq!(dark.primary, light.primary);
        assert_eq!(dark.chart_1, light.chart_1);
        assert!((light.hover_bg.alpha() - 0.35).abs() < f32::EPSILON);
        assert!((dark.active_bg.alpha() - 0.26).abs() < f32::EPSILON);

        let light_tokens = AxiusflowTheme::light().color_tokens();
        let dark_tokens = AxiusflowTheme::dark().color_tokens();
        assert_eq!(token_source(&light_tokens, "surface"), "#ffffff");
        assert_eq!(token_source(&dark_tokens, "surface"), "#141414");
        assert_eq!(
            token_source(&dark_tokens, "hover-bg"),
            "rgb(97 97 97 / 20%)"
        );
        assert_eq!(
            token_source(&light_tokens, "input-fill"),
            "var(--surface-secondary)"
        );
        assert_eq!(
            token_source(&dark_tokens, "danger"),
            "oklch(0.6471 0.2288 22.47)"
        );
        assert_eq!(token_source(&dark_tokens, "primary"), "#3e63dd");
    }

    #[test]
    fn radius_and_header_dimensions_match_the_platform_contract() {
        assert_eq!(RadiusToken::Sm.logical_pixels(), 4);
        assert_eq!(RadiusToken::Default.logical_pixels(), 6);
        assert_eq!(RadiusToken::Full.logical_pixels(), 999);
        assert_eq!(RadiusToken::Sm.css_custom_property(), "--radius-small");
        assert_eq!(
            RadiusToken::Default.css_custom_property(),
            "--radius-default"
        );
        assert_eq!(RadiusToken::Full.css_custom_property(), "--radius-large");
        let theme = AxiusflowTheme::dark();
        assert_eq!(
            theme.dimensions.app_header_height.css_custom_property(),
            "--app_header_height"
        );
    }

    #[test]
    fn css_manifest_contains_every_rust_color_token_and_mode_value() {
        let css = include_str!("../brand.css");

        for theme in [AxiusflowTheme::light(), AxiusflowTheme::dark()] {
            for token in theme.color_tokens() {
                let declaration = format!(
                    "{}: {};",
                    token.css_custom_property(),
                    token.source_expression
                );
                assert!(
                    css.contains(&declaration),
                    "CSS manifest is missing `{declaration}`"
                );
            }
        }

        for retired in [
            "--background:",
            "--foreground:",
            "--card:",
            "--card-foreground",
            "--primary-hover",
            "--muted:",
            "--muted-foreground",
            "--disabled-foreground",
            "--accent:",
            "--muted-border",
            "--overlay",
            "--negative",
            "--destructive",
            "--radius-sm:",
            "--radius-df",
            "--radius-lg",
            "--surface_",
            "--primary_",
            "--card_",
            "--muted_",
            "--text_",
            "--icon_",
            "--border_",
            "--chart_",
            "--profit",
            "--loss",
            "--radius_",
        ] {
            assert!(!css.contains(retired), "retired token `{retired}` returned");
        }
    }

    #[test]
    fn css_manifest_carries_the_portable_interaction_contract() {
        let css = include_str!("../brand.css");
        for required in [
            "--font-sans: \"Inter\", sans-serif;",
            "font-synthesis: none;",
            "font-weight: 400;",
            "outline: 2px solid var(--ring);",
            "outline-offset: 2px;",
            "transition: background-color 150ms ease, color 150ms ease;",
            "cursor: not-allowed;",
            "@media (prefers-reduced-motion: reduce)",
        ] {
            assert!(css.contains(required), "CSS is missing `{required}`");
        }
    }
}
