//! Typed native mapping of the Axiusflow `platform.css` contract.
//!
//! Token source expressions and resolved sRGB values share one registry. The
//! checked CSS manifest uses generated custom-property names, while painting
//! code consumes typed values without string lookup.

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
        Self::from_rgba8(red, green, blue, 255)
    }

    /// Resolves an eight-bit sRGB color with an eight-bit alpha channel.
    #[must_use]
    pub const fn from_rgba8(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red: red as f32 / 255.0,
            green: green as f32 / 255.0,
            blue: blue as f32 / 255.0,
            alpha: alpha as f32 / 255.0,
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

    /// Composites this token over an opaque backdrop in sRGB and returns an
    /// opaque result.
    ///
    /// CSS interaction tokens such as `--hover-bg` describe a translucent mix
    /// over the resting fill. Painting them as a replacement fill lets the GPU
    /// blend through HSL and shift the hue, so native hover/active states
    /// resolve the mix here. Achromatic results keep identical channels so the
    /// overlay cannot introduce a conversion tint.
    #[must_use]
    pub fn over(self, backdrop: ThemeColor) -> Self {
        let alpha = self.alpha;
        let mix = |top: f32, under: f32| top * alpha + under * (1.0 - alpha);
        let red = mix(self.red, backdrop.red);
        let green = mix(self.green, backdrop.green);
        let blue = mix(self.blue, backdrop.blue);
        let max = red.max(green.max(blue));
        let min = red.min(green.min(blue));
        let (red, green, blue) = if max - min < 0.005 {
            let gray = (red + green + blue) / 3.0;
            (gray, gray, gray)
        } else {
            (red, green, blue)
        };
        Self {
            red,
            green,
            blue,
            alpha: 1.0,
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

    /// Returns HSLA components in GPUI's `0.0..=1.0` ranges.
    /// Near-gray tokens keep zero saturation so translucent hover cannot tint.
    #[must_use]
    pub fn hsla_components(self) -> (f32, f32, f32, f32) {
        let (red, green, blue) = (self.red, self.green, self.blue);
        let max = red.max(green.max(blue));
        let min = red.min(green.min(blue));
        let lightness = f32::midpoint(max, min);
        let delta = max - min;
        if delta < 0.02 {
            return (0.0, 0.0, lightness, self.alpha);
        }
        let saturation = if lightness <= 0.0 || lightness >= 1.0 {
            0.0
        } else if lightness < 0.5 {
            delta / (2.0 * lightness)
        } else {
            delta / (2.0 - 2.0 * lightness)
        };
        let hue = if (max - red).abs() <= f32::EPSILON {
            ((green - blue) / delta).rem_euclid(6.0) / 6.0
        } else if (max - green).abs() <= f32::EPSILON {
            ((blue - red) / delta + 2.0) / 6.0
        } else {
            ((red - green) / delta + 4.0) / 6.0
        };
        (hue, saturation, lightness, self.alpha)
    }
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
    pub bullish: ThemeColor,
    pub bearish: ThemeColor,
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
                "surface",
                mode_source(dark, "#ffffff", "#141414"),
                colors.surface,
            ),
            ColorToken::new(
                "surface-secondary",
                mode_source(dark, "#fafafa", "#181818"),
                colors.surface_secondary,
            ),
            ColorToken::new(
                "border",
                mode_source(dark, MIX_INK_6, MIX_PAPER_8),
                colors.border,
            ),
            ColorToken::new("border-secondary", "var(--border)", colors.border_secondary),
            ColorToken::new("input-fill", "var(--surface-secondary)", colors.input_fill),
            ColorToken::new(
                "input-border",
                "var(--border-secondary)",
                colors.input_border,
            ),
            ColorToken::new(
                "text-primary",
                mode_source(dark, "#141414", "#f0f0f0"),
                colors.text_primary,
            ),
            ColorToken::new(
                "text-secondary",
                mode_source(dark, MIX_INK_74, MIX_PAPER_74),
                colors.text_secondary,
            ),
            ColorToken::new(
                "text-muted",
                mode_source(dark, MIX_INK_36, MIX_PAPER_36),
                colors.text_muted,
            ),
            ColorToken::new(
                "hover-bg",
                mode_source(dark, MIX_INK_3_5, MIX_PAPER_8),
                colors.hover_bg,
            ),
            ColorToken::new(
                "active-bg",
                mode_source(dark, MIX_INK_5, MIX_PAPER_14),
                colors.active_bg,
            ),
            ColorToken::new(
                "icon",
                mode_source(dark, MIX_INK_50, MIX_PAPER_66),
                colors.icon,
            ),
            ColorToken::new(
                "icon-active",
                mode_source(dark, "#141414", "#f0f0f0"),
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
                mode_source(dark, MIX_INK_20, MIX_PAPER_15),
                colors.ring,
            ),
            ColorToken::new("bullish", "#089981", colors.bullish),
            ColorToken::new("bearish", "#f7525f", colors.bearish),
        ]
    }
}

impl Default for AxiusflowTheme {
    fn default() -> Self {
        Self::dark()
    }
}

const MIX_INK_3_5: &str = "color-mix(in srgb, #141414 3.5%, transparent)";
const MIX_INK_5: &str = "color-mix(in srgb, #141414 5%, transparent)";
const MIX_INK_6: &str = "color-mix(in srgb, #141414 6%, transparent)";
const MIX_INK_20: &str = "color-mix(in srgb, #141414 20%, transparent)";
const MIX_INK_36: &str = "color-mix(in srgb, #141414 36%, transparent)";
const MIX_INK_50: &str = "color-mix(in srgb, #141414 50%, transparent)";
const MIX_INK_74: &str = "color-mix(in srgb, #141414 74%, transparent)";
const MIX_PAPER_8: &str = "color-mix(in srgb, #f0f0f0 8%, transparent)";
const MIX_PAPER_14: &str = "color-mix(in srgb, #f0f0f0 14%, transparent)";
const MIX_PAPER_15: &str = "color-mix(in srgb, #f0f0f0 15%, transparent)";
const MIX_PAPER_36: &str = "color-mix(in srgb, #f0f0f0 36%, transparent)";
const MIX_PAPER_66: &str = "color-mix(in srgb, #f0f0f0 66%, transparent)";
const MIX_PAPER_74: &str = "color-mix(in srgb, #f0f0f0 74%, transparent)";

const fn mode_source(
    dark: bool,
    light_source: &'static str,
    dark_source: &'static str,
) -> &'static str {
    if dark { dark_source } else { light_source }
}

fn mix_srgb(red: u8, green: u8, blue: u8, percent: f32) -> ThemeColor {
    ThemeColor::from_rgb8(red, green, blue).with_alpha(percent / 100.0)
}

fn light_colors() -> ThemeColors {
    let ink = ThemeColor::from_rgb8(20, 20, 20);
    let surface = ThemeColor::from_rgb8(255, 255, 255);
    let surface_secondary = ThemeColor::from_rgb8(250, 250, 250);
    let border = mix_srgb(20, 20, 20, 6.0);
    ThemeColors {
        surface,
        surface_secondary,
        border,
        border_secondary: border,
        input_fill: surface_secondary,
        input_border: border,
        text_primary: ink,
        text_secondary: mix_srgb(20, 20, 20, 74.0),
        text_muted: mix_srgb(20, 20, 20, 36.0),
        hover_bg: mix_srgb(20, 20, 20, 3.5),
        active_bg: mix_srgb(20, 20, 20, 5.0),
        icon: mix_srgb(20, 20, 20, 50.0),
        icon_active: ink,
        primary: ThemeColor::from_rgb8(62, 99, 221),
        primary_foreground: ThemeColor::from_rgb8(239, 246, 255),
        danger: ThemeColor::from_rgb8(251, 55, 72),
        danger_foreground: ThemeColor::from_rgb8(255, 255, 255),
        ring: mix_srgb(20, 20, 20, 20.0),
        bullish: ThemeColor::from_rgb8(8, 153, 129),
        bearish: ThemeColor::from_rgb8(247, 82, 95),
    }
}

fn dark_colors() -> ThemeColors {
    let ink = ThemeColor::from_rgb8(240, 240, 240);
    let surface_secondary = ThemeColor::from_rgb8(24, 24, 24);
    let border = mix_srgb(240, 240, 240, 8.0);
    ThemeColors {
        surface: ThemeColor::from_rgb8(20, 20, 20),
        surface_secondary,
        border,
        border_secondary: border,
        input_fill: surface_secondary,
        input_border: border,
        text_primary: ink,
        text_secondary: mix_srgb(240, 240, 240, 74.0),
        text_muted: mix_srgb(240, 240, 240, 36.0),
        hover_bg: mix_srgb(240, 240, 240, 8.0),
        active_bg: mix_srgb(240, 240, 240, 14.0),
        icon: mix_srgb(240, 240, 240, 66.0),
        icon_active: ink,
        primary: ThemeColor::from_rgb8(62, 99, 221),
        primary_foreground: ThemeColor::from_rgb8(239, 246, 255),
        danger: ThemeColor::from_rgb8(251, 55, 72),
        danger_foreground: ThemeColor::from_rgb8(255, 255, 255),
        ring: mix_srgb(240, 240, 240, 15.0),
        bullish: ThemeColor::from_rgb8(8, 153, 129),
        bearish: ThemeColor::from_rgb8(247, 82, 95),
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
            Self::Default => 8,
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

        assert_eq!(light.surface, ThemeColor::from_rgb8(255, 255, 255));
        assert_eq!(
            light.surface_secondary,
            ThemeColor::from_rgb8(250, 250, 250)
        );
        assert_eq!(light.input_fill, light.surface_secondary);
        assert_eq!(light.input_border, light.border);
        assert_eq!(light.border_secondary, light.border);
        assert_eq!(
            light.border,
            ThemeColor::from_rgb8(20, 20, 20).with_alpha(0.06)
        );
        assert_eq!(
            light.danger_foreground,
            ThemeColor::from_rgb8(255, 255, 255)
        );
        assert_eq!(dark.surface, ThemeColor::from_rgb8(20, 20, 20));
        assert_eq!(dark.surface_secondary, ThemeColor::from_rgb8(24, 24, 24));
        assert_eq!(dark.input_fill, dark.surface_secondary);
        assert_eq!(dark.input_border, dark.border);
        assert_eq!(dark.border_secondary, dark.border);
        assert_eq!(light.bullish, ThemeColor::from_rgb8(8, 153, 129));
        assert_eq!(light.bearish, ThemeColor::from_rgb8(247, 82, 95));
        assert_eq!(light.primary, ThemeColor::from_rgb8(62, 99, 221));
        assert_eq!(dark.primary, light.primary);
        assert_eq!(dark.bullish, light.bullish);
        assert_eq!(dark.bearish, light.bearish);
        assert_eq!(
            light.hover_bg,
            ThemeColor::from_rgb8(20, 20, 20).with_alpha(0.035)
        );
        assert_eq!(
            dark.active_bg,
            ThemeColor::from_rgb8(240, 240, 240).with_alpha(0.14)
        );
        let (_, hover_saturation, _, _) = light.hover_bg.hsla_components();
        assert!(hover_saturation.abs() < f32::EPSILON);

        for composited in [
            light.hover_bg.over(light.surface),
            light.active_bg.over(light.surface),
            dark.hover_bg.over(dark.surface),
            dark.active_bg.over(dark.surface),
            light.hover_bg.over(light.input_fill),
            light.active_bg.over(light.input_fill),
        ] {
            assert!((composited.alpha() - 1.0).abs() < f32::EPSILON);
            assert!((composited.red() - composited.green()).abs() < f32::EPSILON);
            assert!((composited.green() - composited.blue()).abs() < f32::EPSILON);
        }
        assert!((light.hover_bg.over(light.surface).red() - light.surface.red()).abs() > 0.01);
        assert!((dark.hover_bg.over(dark.surface).red() - dark.surface.red()).abs() > 0.01);

        let light_tokens = AxiusflowTheme::light().color_tokens();
        let dark_tokens = AxiusflowTheme::dark().color_tokens();
        assert_eq!(token_source(&light_tokens, "surface"), "#ffffff");
        assert_eq!(
            token_source(&light_tokens, "border"),
            "color-mix(in srgb, #141414 6%, transparent)"
        );
        assert_eq!(token_source(&dark_tokens, "surface"), "#141414");
        assert_eq!(
            token_source(&dark_tokens, "hover-bg"),
            "color-mix(in srgb, #f0f0f0 8%, transparent)"
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
        assert_eq!(token_source(&light_tokens, "danger-foreground"), "#ffffff");
        assert_eq!(token_source(&light_tokens, "bullish"), "#089981");
        assert_eq!(token_source(&dark_tokens, "bearish"), "#f7525f");
    }

    #[test]
    fn radius_and_header_dimensions_match_the_platform_contract() {
        assert_eq!(RadiusToken::Sm.logical_pixels(), 4);
        assert_eq!(RadiusToken::Default.logical_pixels(), 8);
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
            "--chart-1",
            "--positive",
            "--warning",
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
            "--font-sans: \"DM Sans\", sans-serif;",
            "DMSans-Regular.ttf",
            "font-weight: 400;",
            "font-synthesis: none;",
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
