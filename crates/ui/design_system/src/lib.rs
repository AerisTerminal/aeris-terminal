//! Typed native mapping of the portable Nucleus `brand.css` contract.
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

    /// Returns a CSS color accepted by Origin's options and series contracts.
    #[must_use]
    pub fn css_value(self) -> String {
        let red = channel_to_u8(self.red);
        let green = channel_to_u8(self.green);
        let blue = channel_to_u8(self.blue);
        if self.alpha >= 0.999_5 {
            format!("#{red:02x}{green:02x}{blue:02x}")
        } else {
            format!("rgba({red}, {green}, {blue}, {:.3})", self.alpha)
        }
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
    pub background: ThemeColor,
    pub foreground: ThemeColor,
    pub card: ThemeColor,
    pub card_foreground: ThemeColor,
    pub primary: ThemeColor,
    pub primary_foreground: ThemeColor,
    pub primary_hover: ThemeColor,
    pub muted: ThemeColor,
    pub muted_foreground: ThemeColor,
    pub disabled_foreground: ThemeColor,
    pub accent: ThemeColor,
    pub border: ThemeColor,
    pub muted_border: ThemeColor,
    pub input: ThemeColor,
    pub ring: ThemeColor,
    pub overlay: ThemeColor,
    pub positive: ThemeColor,
    pub negative: ThemeColor,
    pub destructive: ThemeColor,
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
    pub fn color_tokens(self) -> [ColorToken; 20] {
        let colors = self.colors;
        let dark = self.mode == ThemeMode::Dark;
        [
            ColorToken::new(
                "background",
                mode_source(dark, "oklch(1 0 0)", "#070a0f"),
                colors.background,
            ),
            ColorToken::new(
                "foreground",
                mode_source(dark, "oklch(0.321093 0 0)", "oklch(0.985 0 0)"),
                colors.foreground,
            ),
            ColorToken::new(
                "card",
                mode_source(dark, "oklch(1 0 0)", "#070a0f"),
                colors.card,
            ),
            ColorToken::new(
                "card-foreground",
                mode_source(dark, "oklch(0.321093 0 0)", "oklch(0.985 0 0)"),
                colors.card_foreground,
            ),
            ColorToken::new("primary", "oklch(0.54375 0.191015 267.005)", colors.primary),
            ColorToken::new(
                "primary-foreground",
                "oklch(0.97 0.014 254.604)",
                colors.primary_foreground,
            ),
            ColorToken::new(
                "primary-hover",
                mode_source(
                    dark,
                    "oklch(0.483663 0.190265 267.018)",
                    "oklch(0.603683 0.191341 267.047)",
                ),
                colors.primary_hover,
            ),
            ColorToken::new(
                "muted",
                mode_source(dark, "oklch(0.991063 0 0)", "#0c1115"),
                colors.muted,
            ),
            ColorToken::new(
                "muted-foreground",
                mode_source(dark, "oklch(0.556 0 0)", "#9da3aa"),
                colors.muted_foreground,
            ),
            ColorToken::new(
                "disabled-foreground",
                mode_source(dark, "oklch(0.74 0 0)", "oklch(0.52 0 0)"),
                colors.disabled_foreground,
            ),
            ColorToken::new(
                "accent",
                mode_source(dark, "oklch(0.97 0 0)", "#222224"),
                colors.accent,
            ),
            ColorToken::new(
                "border",
                mode_source(dark, "#f3f3f3", "#16191f"),
                colors.border,
            ),
            ColorToken::new(
                "muted-border",
                mode_source(dark, "#f5f5f5", "#131519"),
                colors.muted_border,
            ),
            ColorToken::new(
                "input",
                mode_source(dark, "oklch(0.991063 0 0)", "#0c1115"),
                colors.input,
            ),
            ColorToken::new(
                "ring",
                mode_source(dark, "oklch(0.708 0 0)", "oklch(0.556 0 0)"),
                colors.ring,
            ),
            ColorToken::new("overlay", "oklch(0 0 0 / 50%)", colors.overlay),
            ColorToken::new("positive", "#089981", colors.positive),
            ColorToken::new("negative", "#f7525f", colors.negative),
            ColorToken::new("destructive", "var(--negative)", colors.destructive),
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
    let background = ThemeColor::from_oklch(1.0, 0.0, 0.0);
    let foreground = ThemeColor::from_oklch(0.321_093, 0.0, 0.0);
    let muted = ThemeColor::from_oklch(0.991_063, 0.0, 0.0);
    let negative = ThemeColor::from_rgb8(247, 82, 95);
    ThemeColors {
        background,
        foreground,
        card: background,
        card_foreground: foreground,
        primary: ThemeColor::from_oklch(0.543_75, 0.191_015, 267.005),
        primary_foreground: ThemeColor::from_oklch(0.97, 0.014, 254.604),
        primary_hover: ThemeColor::from_oklch(0.483_663, 0.190_265, 267.018),
        muted,
        muted_foreground: ThemeColor::from_oklch(0.556, 0.0, 0.0),
        disabled_foreground: ThemeColor::from_oklch(0.74, 0.0, 0.0),
        accent: ThemeColor::from_oklch(0.97, 0.0, 0.0),
        border: ThemeColor::from_rgb8(243, 243, 243),
        muted_border: ThemeColor::from_rgb8(245, 245, 245),
        input: muted,
        ring: ThemeColor::from_oklch(0.708, 0.0, 0.0),
        overlay: ThemeColor::from_rgb8(0, 0, 0).with_alpha(0.5),
        positive: ThemeColor::from_rgb8(8, 153, 129),
        negative,
        destructive: negative,
        warning: ThemeColor::from_oklch(0.768_578, 0.164_801, 70.108),
    }
}

fn dark_colors() -> ThemeColors {
    let background = ThemeColor::from_rgb8(7, 10, 15);
    let foreground = ThemeColor::from_oklch(0.985, 0.0, 0.0);
    let muted = ThemeColor::from_rgb8(12, 17, 21);
    let negative = ThemeColor::from_rgb8(247, 82, 95);
    ThemeColors {
        background,
        foreground,
        card: background,
        card_foreground: foreground,
        primary: ThemeColor::from_oklch(0.543_75, 0.191_015, 267.005),
        primary_foreground: ThemeColor::from_oklch(0.97, 0.014, 254.604),
        primary_hover: ThemeColor::from_oklch(0.603_683, 0.191_341, 267.047),
        muted,
        muted_foreground: ThemeColor::from_rgb8(157, 163, 170),
        disabled_foreground: ThemeColor::from_oklch(0.52, 0.0, 0.0),
        accent: ThemeColor::from_rgb8(34, 34, 36),
        border: ThemeColor::from_rgb8(22, 25, 31),
        muted_border: ThemeColor::from_rgb8(19, 21, 25),
        input: muted,
        ring: ThemeColor::from_oklch(0.556, 0.0, 0.0),
        overlay: ThemeColor::from_rgb8(0, 0, 0).with_alpha(0.5),
        positive: ThemeColor::from_rgb8(8, 153, 129),
        negative,
        destructive: negative,
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
            Self::Sm => "radius-sm",
            Self::Default => "radius-df",
            Self::Full => "radius-lg",
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
    fn native_palettes_match_the_nucleus_contract() {
        let light = AxiusflowTheme::light().colors;
        let dark = AxiusflowTheme::dark().colors;

        assert_eq!(light.background, ThemeColor::from_oklch(1.0, 0.0, 0.0));
        assert_eq!(light.card, light.background);
        assert_eq!(light.input, light.muted);
        assert_eq!(dark.background, ThemeColor::from_rgb8(7, 10, 15));
        assert_eq!(dark.card, dark.background);
        assert_eq!(dark.muted, ThemeColor::from_rgb8(12, 17, 21));
        assert_eq!(dark.input, dark.muted);
        assert_eq!(dark.accent, ThemeColor::from_rgb8(34, 34, 36));
        assert_eq!(dark.border, ThemeColor::from_rgb8(22, 25, 31));
        assert_eq!(dark.muted_border, ThemeColor::from_rgb8(19, 21, 25));
        assert_eq!(light.positive, ThemeColor::from_rgb8(8, 153, 129));
        assert_eq!(dark.negative, ThemeColor::from_rgb8(247, 82, 95));
        assert_eq!(light.destructive, light.negative);
        assert_eq!(dark.destructive, dark.negative);
        assert!((light.overlay.alpha() - 0.5).abs() < f32::EPSILON);
        assert!((dark.overlay.alpha() - 0.5).abs() < f32::EPSILON);

        let light_tokens = AxiusflowTheme::light().color_tokens();
        let dark_tokens = AxiusflowTheme::dark().color_tokens();
        assert_eq!(token_source(&light_tokens, "background"), "oklch(1 0 0)");
        assert_eq!(token_source(&dark_tokens, "background"), "#070a0f");
        assert_eq!(token_source(&dark_tokens, "card"), "#070a0f");
        assert_eq!(token_source(&dark_tokens, "muted"), "#0c1115");
        assert_eq!(
            token_source(&dark_tokens, "primary-hover"),
            "oklch(0.603683 0.191341 267.047)"
        );
        assert_eq!(token_source(&dark_tokens, "muted-border"), "#131519");
        assert_eq!(token_source(&dark_tokens, "destructive"), "var(--negative)");
    }

    #[test]
    fn radius_and_header_dimensions_match_the_platform_contract() {
        assert_eq!(RadiusToken::Sm.logical_pixels(), 4);
        assert_eq!(RadiusToken::Default.logical_pixels(), 6);
        assert_eq!(RadiusToken::Full.logical_pixels(), 999);
        assert_eq!(RadiusToken::Sm.css_custom_property(), "--radius-sm");
        assert_eq!(RadiusToken::Default.css_custom_property(), "--radius-df");
        assert_eq!(RadiusToken::Full.css_custom_property(), "--radius-lg");
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
