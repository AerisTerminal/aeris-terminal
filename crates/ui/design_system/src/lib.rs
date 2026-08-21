//! Typed native mapping of the Axiusflow `platform.css` contract.
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
                mode_source(dark, "oklch(1 0 0)", "oklch(0.1913 0 0)"),
                colors.surface,
            ),
            ColorToken::new(
                "surface-secondary",
                mode_source(dark, "oklch(0.9911 0 0)", "oklch(0.2221 0 0)"),
                colors.surface_secondary,
            ),
            ColorToken::new(
                "border",
                mode_source(dark, "oklch(0.9702 0 0)", "oklch(0.235 0 0)"),
                colors.border,
            ),
            ColorToken::new(
                "border-secondary",
                mode_source(dark, "oklch(0.9642 0 0)", "oklch(0.2603 0 0)"),
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
                mode_source(dark, "oklch(0.3715 0 0)", "oklch(0.9158 0 0)"),
                colors.text_primary,
            ),
            ColorToken::new(
                "text-secondary",
                mode_source(dark, "oklch(0.5795 0 0)", "oklch(0.7122 0 0)"),
                colors.text_secondary,
            ),
            ColorToken::new(
                "text-muted",
                mode_source(dark, "oklch(0.9006 0 0)", "oklch(0.3791 0 0)"),
                colors.text_muted,
            ),
            ColorToken::new(
                "hover-bg",
                mode_source(dark, "oklch(0.9521 0 0 / 35%)", "oklch(0.4926 0 0 / 20%)"),
                colors.hover_bg,
            ),
            ColorToken::new(
                "active-bg",
                mode_source(dark, "oklch(0.9521 0 0 / 45%)", "oklch(0.4926 0 0 / 26%)"),
                colors.active_bg,
            ),
            ColorToken::new(
                "icon",
                mode_source(dark, "oklch(0.5999 0 0)", "oklch(0.7155 0 0)"),
                colors.icon,
            ),
            ColorToken::new(
                "icon-active",
                mode_source(dark, "oklch(0.3753 0 0)", "oklch(0.9219 0 0)"),
                colors.icon_active,
            ),
            ColorToken::new("primary", "oklch(0.5438 0.191 267.005)", colors.primary),
            ColorToken::new(
                "primary-foreground",
                "oklch(0.97 0.014 254.604)",
                colors.primary_foreground,
            ),
            ColorToken::new("danger", "oklch(0.6471 0.2288 22.47)", colors.danger),
            ColorToken::new(
                "danger-foreground",
                "oklch(1 0 0)",
                colors.danger_foreground,
            ),
            ColorToken::new(
                "ring",
                mode_source(dark, "oklch(0.708 0 0)", "oklch(0.556 0 0)"),
                colors.ring,
            ),
            ColorToken::new("chart-1", "oklch(0.8699 0 0)", colors.chart_1),
            ColorToken::new("chart-2", "oklch(0.5795 0 0)", colors.chart_2),
            ColorToken::new("chart-3", "oklch(0.4855 0 0)", colors.chart_3),
            ColorToken::new("chart-4", "oklch(0.4054 0 0)", colors.chart_4),
            ColorToken::new("chart-5", "oklch(0.325 0 0)", colors.chart_5),
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

fn gray(lightness: f32) -> ThemeColor {
    ThemeColor::from_oklch(lightness, 0.0, 0.0)
}

fn light_colors() -> ThemeColors {
    let surface = gray(1.0);
    let surface_secondary = gray(0.991_1);
    let border_secondary = gray(0.964_2);
    let hover = gray(0.952_1);
    ThemeColors {
        surface,
        surface_secondary,
        border: gray(0.970_2),
        border_secondary,
        input_fill: surface_secondary,
        input_border: border_secondary,
        text_primary: gray(0.371_5),
        text_secondary: gray(0.579_5),
        text_muted: gray(0.900_6),
        hover_bg: hover.with_alpha(0.35),
        active_bg: hover.with_alpha(0.45),
        icon: gray(0.599_9),
        icon_active: gray(0.375_3),
        primary: ThemeColor::from_oklch(0.543_8, 0.191, 267.005),
        primary_foreground: ThemeColor::from_oklch(0.97, 0.014, 254.604),
        danger: ThemeColor::from_oklch(0.647_1, 0.228_8, 22.47),
        danger_foreground: surface,
        ring: gray(0.708),
        chart_1: gray(0.869_9),
        chart_2: gray(0.579_5),
        chart_3: gray(0.485_5),
        chart_4: gray(0.405_4),
        chart_5: gray(0.325),
        positive: ThemeColor::from_rgb8(8, 153, 129),
        warning: ThemeColor::from_oklch(0.768_578, 0.164_801, 70.108),
    }
}

fn dark_colors() -> ThemeColors {
    let surface_secondary = gray(0.222_1);
    let border_secondary = gray(0.260_3);
    let hover = gray(0.492_6);
    ThemeColors {
        surface: gray(0.191_3),
        surface_secondary,
        border: gray(0.235),
        border_secondary,
        input_fill: surface_secondary,
        input_border: border_secondary,
        text_primary: gray(0.915_8),
        text_secondary: gray(0.712_2),
        text_muted: gray(0.379_1),
        hover_bg: hover.with_alpha(0.20),
        active_bg: hover.with_alpha(0.26),
        icon: gray(0.715_5),
        icon_active: gray(0.921_9),
        primary: ThemeColor::from_oklch(0.543_8, 0.191, 267.005),
        primary_foreground: ThemeColor::from_oklch(0.97, 0.014, 254.604),
        danger: ThemeColor::from_oklch(0.647_1, 0.228_8, 22.47),
        danger_foreground: gray(1.0),
        ring: gray(0.556),
        chart_1: gray(0.869_9),
        chart_2: gray(0.579_5),
        chart_3: gray(0.485_5),
        chart_4: gray(0.405_4),
        chart_5: gray(0.325),
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
    fn native_palettes_match_the_platform_contract() {
        let light = AxiusflowTheme::light().colors;
        let dark = AxiusflowTheme::dark().colors;

        assert_eq!(light.surface, ThemeColor::from_oklch(1.0, 0.0, 0.0));
        assert_eq!(light.input_fill, light.surface_secondary);
        assert_eq!(light.input_border, light.border_secondary);
        assert_eq!(light.danger_foreground, light.surface);
        assert_eq!(dark.surface, ThemeColor::from_oklch(0.191_3, 0.0, 0.0));
        assert_eq!(
            dark.surface_secondary,
            ThemeColor::from_oklch(0.222_1, 0.0, 0.0)
        );
        assert_eq!(dark.input_fill, dark.surface_secondary);
        assert_eq!(dark.input_border, dark.border_secondary);
        assert_eq!(dark.border, ThemeColor::from_oklch(0.235, 0.0, 0.0));
        assert_eq!(
            dark.border_secondary,
            ThemeColor::from_oklch(0.260_3, 0.0, 0.0)
        );
        assert_eq!(light.positive, ThemeColor::from_rgb8(8, 153, 129));
        assert_eq!(
            light.primary,
            ThemeColor::from_oklch(0.543_8, 0.191, 267.005)
        );
        assert_eq!(dark.primary, light.primary);
        assert_eq!(dark.chart_1, light.chart_1);
        assert!((light.hover_bg.alpha() - 0.35).abs() < f32::EPSILON);
        assert!((dark.active_bg.alpha() - 0.26).abs() < f32::EPSILON);

        let light_tokens = AxiusflowTheme::light().color_tokens();
        let dark_tokens = AxiusflowTheme::dark().color_tokens();
        assert_eq!(token_source(&light_tokens, "surface"), "oklch(1 0 0)");
        assert_eq!(token_source(&dark_tokens, "surface"), "oklch(0.1913 0 0)");
        assert_eq!(
            token_source(&dark_tokens, "hover-bg"),
            "oklch(0.4926 0 0 / 20%)"
        );
        assert_eq!(
            token_source(&light_tokens, "input-fill"),
            "var(--surface-secondary)"
        );
        assert_eq!(
            token_source(&dark_tokens, "danger"),
            "oklch(0.6471 0.2288 22.47)"
        );
        assert_eq!(
            token_source(&dark_tokens, "primary"),
            "oklch(0.5438 0.191 267.005)"
        );
        assert_eq!(
            token_source(&light_tokens, "danger-foreground"),
            "oklch(1 0 0)"
        );
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
        let css = include_str!("../platform.css");

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
        let css = include_str!("../platform.css");
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
