//! Typed native mapping of the Axiusflow `platform.css` contract.
//!
//! Token source expressions and resolved sRGB values share one registry. The
//! checked CSS manifest uses generated custom-property names, while painting
//! code consumes typed values without string lookup.

use std::sync::OnceLock;

/// Canonical portable stylesheet shared with the native presentation layer.
pub const PLATFORM_CSS: &str = include_str!("../platform.css");

/// Bundled platform faces referenced by `platform.css`.
pub static PLATFORM_FONT_BYTES: [&[u8]; 2] = [
    include_bytes!("../assets/fonts/HKGrotesk-Medium.ttf"),
    include_bytes!("../assets/fonts/HKGrotesk-Bold.ttf"),
];

static PLATFORM_TYPOGRAPHY: OnceLock<PlatformTypography> = OnceLock::new();

fn css_custom_property_value(declaration: &'static str) -> &'static str {
    let Some(start) = PLATFORM_CSS.find(declaration) else {
        panic!("platform.css must declare {declaration}");
    };
    let value = &PLATFORM_CSS[start + declaration.len()..];
    let Some((value, _)) = value.split_once(';') else {
        panic!("platform.css {declaration} declaration must end with a semicolon");
    };
    value.trim()
}

fn primary_font_family(stack: &'static str, declaration: &'static str) -> &'static str {
    let Some(quoted) = stack.strip_prefix('"') else {
        panic!("platform.css {declaration} must begin with a quoted family");
    };
    let Some((family, _)) = quoted.split_once('"') else {
        panic!("platform.css {declaration} must contain a closing quote");
    };
    family
}

fn css_weight(declaration: &'static str) -> u16 {
    css_custom_property_value(declaration)
        .parse()
        .unwrap_or_else(|_| panic!("platform.css {declaration} must be an integer font weight"))
}

fn quoted_css_value(declaration: &'static str) -> &'static str {
    let value = css_custom_property_value(declaration);
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or_else(|| panic!("platform.css {declaration} must be a quoted string"))
}

/// Semantic roles in the canonical platform typography hierarchy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypographyRole {
    Normal,
    Emphasis,
    Strong,
}

/// Typed native projection of the typography values owned by `platform.css`.
///
/// GPUI does not consume CSS, so native views ask this projection for the same
/// family, semantic weights, and OpenType feature tag instead of duplicating
/// those decisions in presentation code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlatformTypography {
    stack: &'static str,
    family: &'static str,
    normal_weight: u16,
    emphasis_weight: u16,
    strong_weight: u16,
    tabular_numerals_feature: &'static str,
}

impl PlatformTypography {
    #[must_use]
    pub const fn stack(self) -> &'static str {
        self.stack
    }

    #[must_use]
    pub const fn family(self) -> &'static str {
        self.family
    }

    #[must_use]
    pub const fn weight(self, role: TypographyRole) -> u16 {
        match role {
            TypographyRole::Normal => self.normal_weight,
            TypographyRole::Emphasis => self.emphasis_weight,
            TypographyRole::Strong => self.strong_weight,
        }
    }

    #[must_use]
    pub const fn tabular_numerals_feature(self) -> &'static str {
        self.tabular_numerals_feature
    }
}

/// Returns the canonical typography contract projected from `platform.css`.
#[must_use]
pub fn platform_typography() -> PlatformTypography {
    *PLATFORM_TYPOGRAPHY.get_or_init(|| {
        let stack = css_custom_property_value("--font-sans:");
        PlatformTypography {
            stack,
            family: primary_font_family(stack, "--font-sans"),
            normal_weight: css_weight("--font-weight-normal:"),
            emphasis_weight: css_weight("--font-weight-emphasis:"),
            strong_weight: css_weight("--font-weight-strong:"),
            tabular_numerals_feature: quoted_css_value("--font-feature-tabular-numerals:"),
        }
    })
}

/// Returns the `--font-sans` value from `platform.css`.
///
/// Native GPUI does not interpret CSS directly, so native consumers resolve
/// the same canonical declaration here instead of duplicating a font name.
#[must_use]
pub fn platform_font_stack() -> &'static str {
    platform_typography().stack()
}

/// Returns the primary family from the canonical `--font-sans` CSS value.
#[must_use]
pub fn platform_font_family() -> &'static str {
    platform_typography().family()
}

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
}

/// Native application dimensions that are not part of the portable CSS token contract.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeDimensions {
    pub app_header_height: f32,
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
                app_header_height: 44.0,
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
    pub fn color_tokens(self) -> [ColorToken; 18] {
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
                mode_source(dark, LIGHT_BORDER, DARK_BORDER),
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
                mode_source(dark, LIGHT_TEXT_SECONDARY, DARK_TEXT_SECONDARY),
                colors.text_secondary,
            ),
            ColorToken::new(
                "text-muted",
                mode_source(dark, LIGHT_TEXT_MUTED, DARK_TEXT_MUTED),
                colors.text_muted,
            ),
            ColorToken::new(
                "hover-bg",
                mode_source(dark, LIGHT_HOVER, DARK_HOVER),
                colors.hover_bg,
            ),
            ColorToken::new(
                "active-bg",
                mode_source(dark, LIGHT_ACTIVE, DARK_ACTIVE),
                colors.active_bg,
            ),
            ColorToken::new(
                "icon",
                mode_source(dark, LIGHT_ICON, DARK_ICON),
                colors.icon,
            ),
            ColorToken::new(
                "icon-active",
                mode_source(dark, "#141414", "#f0f0f0"),
                colors.icon_active,
            ),
            ColorToken::new("primary", "#3e63dd", colors.primary),
            ColorToken::new("primary-foreground", "#eff6ff", colors.primary_foreground),
            ColorToken::new("danger", "#fb3748", colors.danger),
            ColorToken::new("danger-foreground", "#ffffff", colors.danger_foreground),
            ColorToken::new(
                "ring",
                mode_source(dark, LIGHT_RING, DARK_RING),
                colors.ring,
            ),
        ]
    }
}

impl Default for AxiusflowTheme {
    fn default() -> Self {
        Self::dark()
    }
}

const LIGHT_BORDER: &str = "#1414140f";
const LIGHT_TEXT_SECONDARY: &str = "#141414bd";
const LIGHT_TEXT_MUTED: &str = "#1414145c";
const LIGHT_HOVER: &str = "#14141409";
const LIGHT_ACTIVE: &str = "#1414140d";
const LIGHT_ICON: &str = "#14141480";
const LIGHT_RING: &str = "#14141433";
const DARK_BORDER: &str = "#f0f0f014";
const DARK_TEXT_SECONDARY: &str = "#f0f0f0bd";
const DARK_TEXT_MUTED: &str = "#f0f0f05c";
const DARK_HOVER: &str = "#f0f0f014";
const DARK_ACTIVE: &str = "#f0f0f024";
const DARK_ICON: &str = "#f0f0f0a8";
const DARK_RING: &str = "#f0f0f026";

const fn mode_source(
    dark: bool,
    light_source: &'static str,
    dark_source: &'static str,
) -> &'static str {
    if dark { dark_source } else { light_source }
}

fn light_colors() -> ThemeColors {
    let ink = ThemeColor::from_rgb8(20, 20, 20);
    let surface = ThemeColor::from_rgb8(255, 255, 255);
    let surface_secondary = ThemeColor::from_rgb8(250, 250, 250);
    let border = ThemeColor::from_rgba8(20, 20, 20, 0x0f);
    ThemeColors {
        surface,
        surface_secondary,
        border,
        border_secondary: border,
        input_fill: surface_secondary,
        input_border: border,
        text_primary: ink,
        text_secondary: ThemeColor::from_rgba8(20, 20, 20, 0xbd),
        text_muted: ThemeColor::from_rgba8(20, 20, 20, 0x5c),
        hover_bg: ThemeColor::from_rgba8(20, 20, 20, 0x09),
        active_bg: ThemeColor::from_rgba8(20, 20, 20, 0x0d),
        icon: ThemeColor::from_rgba8(20, 20, 20, 0x80),
        icon_active: ink,
        primary: ThemeColor::from_rgb8(62, 99, 221),
        primary_foreground: ThemeColor::from_rgb8(239, 246, 255),
        danger: ThemeColor::from_rgb8(251, 55, 72),
        danger_foreground: ThemeColor::from_rgb8(255, 255, 255),
        ring: ThemeColor::from_rgba8(20, 20, 20, 0x33),
    }
}

fn dark_colors() -> ThemeColors {
    let ink = ThemeColor::from_rgb8(240, 240, 240);
    let surface_secondary = ThemeColor::from_rgb8(24, 24, 24);
    let border = ThemeColor::from_rgba8(240, 240, 240, 0x14);
    ThemeColors {
        surface: ThemeColor::from_rgb8(20, 20, 20),
        surface_secondary,
        border,
        border_secondary: border,
        input_fill: surface_secondary,
        input_border: border,
        text_primary: ink,
        text_secondary: ThemeColor::from_rgba8(240, 240, 240, 0xbd),
        text_muted: ThemeColor::from_rgba8(240, 240, 240, 0x5c),
        hover_bg: ThemeColor::from_rgba8(240, 240, 240, 0x14),
        active_bg: ThemeColor::from_rgba8(240, 240, 240, 0x24),
        icon: ThemeColor::from_rgba8(240, 240, 240, 0xa8),
        icon_active: ink,
        primary: ThemeColor::from_rgb8(62, 99, 221),
        primary_foreground: ThemeColor::from_rgb8(239, 246, 255),
        danger: ThemeColor::from_rgb8(251, 55, 72),
        danger_foreground: ThemeColor::from_rgb8(255, 255, 255),
        ring: ThemeColor::from_rgba8(240, 240, 240, 0x26),
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
    use super::{
        AxiusflowTheme, ColorToken, RadiusToken, ThemeColor, TypographyRole, platform_font_family,
        platform_font_stack, platform_typography,
    };

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
        assert_eq!(light.border, ThemeColor::from_rgba8(20, 20, 20, 0x0f));
        assert_eq!(
            light.danger_foreground,
            ThemeColor::from_rgb8(255, 255, 255)
        );
        assert_eq!(dark.surface, ThemeColor::from_rgb8(20, 20, 20));
        assert_eq!(dark.surface_secondary, ThemeColor::from_rgb8(24, 24, 24));
        assert_eq!(dark.input_fill, dark.surface_secondary);
        assert_eq!(dark.input_border, dark.border);
        assert_eq!(dark.border_secondary, dark.border);
        assert_eq!(light.primary, ThemeColor::from_rgb8(62, 99, 221));
        assert_eq!(dark.primary, light.primary);
        assert_eq!(light.hover_bg, ThemeColor::from_rgba8(20, 20, 20, 0x09));
        assert_eq!(dark.active_bg, ThemeColor::from_rgba8(240, 240, 240, 0x24));
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
        assert_eq!(token_source(&light_tokens, "border"), "#1414140f");
        assert_eq!(token_source(&dark_tokens, "surface"), "#141414");
        assert_eq!(token_source(&dark_tokens, "hover-bg"), "#f0f0f014");
        assert_eq!(
            token_source(&light_tokens, "input-fill"),
            "var(--surface-secondary)"
        );
        assert_eq!(token_source(&dark_tokens, "danger"), "#fb3748");
        assert_eq!(token_source(&dark_tokens, "primary"), "#3e63dd");
        assert_eq!(token_source(&light_tokens, "danger-foreground"), "#ffffff");
        assert!(
            !light_tokens
                .iter()
                .any(|token| matches!(token.canonical_identifier, "bullish" | "bearish"))
        );
    }

    #[test]
    fn radius_tokens_match_the_platform_contract() {
        assert_eq!(RadiusToken::Sm.logical_pixels(), 4);
        assert_eq!(RadiusToken::Default.logical_pixels(), 8);
        assert_eq!(RadiusToken::Full.logical_pixels(), 999);
        assert_eq!(RadiusToken::Sm.css_custom_property(), "--radius-small");
        assert_eq!(
            RadiusToken::Default.css_custom_property(),
            "--radius-default"
        );
        assert_eq!(RadiusToken::Full.css_custom_property(), "--radius-large");
    }

    #[test]
    fn bundled_platform_fonts_are_exactly_medium_and_bold() {
        let font_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
        let mut bundled_fonts = std::fs::read_dir(font_dir)
            .expect("platform font directory is readable")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|extension| {
                    matches!(
                        extension.to_string_lossy().to_ascii_lowercase().as_str(),
                        "ttf" | "otf" | "woff" | "woff2"
                    )
                })
            })
            .filter_map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect::<Vec<_>>();
        bundled_fonts.sort();
        assert_eq!(
            bundled_fonts,
            ["HKGrotesk-Bold.ttf", "HKGrotesk-Medium.ttf"]
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
            "--font-sans: \"HK Grotesk\", sans-serif;",
            "--font-weight-normal: 500;",
            "--font-weight-emphasis: 700;",
            "--font-weight-strong: 700;",
            "--font-feature-tabular-numerals: \"tnum\";",
            "HKGrotesk-Medium.ttf",
            "HKGrotesk-Bold.ttf",
            "font-weight: 500;",
            "font-weight: 700;",
            "font-variant-numeric: tabular-nums;",
            "-webkit-font-smoothing: antialiased;",
            "font-synthesis: none;",
            "transition: background-color 150ms ease, color 150ms ease;",
            "outline: 2px solid var(--ring);",
            "outline-offset: 2px;",
            "cursor: not-allowed;",
            "@media (prefers-reduced-motion: reduce)",
        ] {
            assert!(css.contains(required), "CSS is missing `{required}`");
        }
        assert!(!css.contains("HKGrotesk-SemiBold.ttf"));
        assert!(!css.contains("font-weight: 600;"));
        assert!(!css.contains("transform: scale("));
        assert!(!css.contains("/* Chart */"));
        assert!(!css.contains("--bullish:"));
        assert!(!css.contains("--bearish:"));
        assert_eq!(platform_font_family(), "HK Grotesk");
        assert_eq!(platform_font_stack(), "\"HK Grotesk\", sans-serif");

        let typography = platform_typography();
        assert_eq!(typography.weight(TypographyRole::Normal), 500);
        assert_eq!(typography.weight(TypographyRole::Emphasis), 700);
        assert_eq!(typography.weight(TypographyRole::Strong), 700);
        assert_eq!(typography.tabular_numerals_feature(), "tnum");
    }
}
