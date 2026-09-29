//! Typed native mapping of the `Aeris` `platform.css` contract.
//!
//! Token source expressions and resolved sRGB values share one registry. The
//! checked CSS manifest uses generated custom-property names, while painting
//! code consumes typed values without string lookup.

#[cfg(test)]
mod token_compiler;

include!(concat!(env!("OUT_DIR"), "/platform_tokens.rs"));

/// Canonical portable stylesheet shared with the native presentation layer.
pub const PLATFORM_CSS: &str = include_str!("../platform.css");

/// Bundled platform faces referenced by `platform.css`.
pub static PLATFORM_FONT_BYTES: [&[u8]; 2] = [
    include_bytes!("../assets/fonts/HKGrotesk-Medium.ttf"),
    include_bytes!("../assets/fonts/HKGrotesk-Bold.ttf"),
];

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
    PlatformTypography {
        stack: FONT_STACK,
        family: FONT_FAMILY,
        normal_weight: FONT_WEIGHT_NORMAL,
        emphasis_weight: FONT_WEIGHT_EMPHASIS,
        strong_weight: FONT_WEIGHT_STRONG,
        tabular_numerals_feature: TABULAR_FEATURE,
    }
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
    pub text_primary: ThemeColor,
    pub text_secondary: ThemeColor,
    pub text_muted: ThemeColor,
    pub text_positive: ThemeColor,
    pub text_negative: ThemeColor,
    pub hover_bg: ThemeColor,
    pub active_bg: ThemeColor,
    pub icon: ThemeColor,
    pub icon_active: ThemeColor,
    pub primary: ThemeColor,
    pub primary_foreground: ThemeColor,
    pub danger: ThemeColor,
    pub danger_foreground: ThemeColor,
    pub danger_disabled: ThemeColor,
    pub danger_disabled_foreground: ThemeColor,
    pub danger_ring: ThemeColor,
    pub warning: ThemeColor,
    pub positive: ThemeColor,
    pub positive_subtle: ThemeColor,
    pub negative_subtle: ThemeColor,
    pub button_fill: ThemeColor,
    pub button_fill_hover: ThemeColor,
    pub button_fill_active: ThemeColor,
    pub button_fill_foreground: ThemeColor,
    pub button_fill_subtle: ThemeColor,
    pub buy: ThemeColor,
    pub buy_hover: ThemeColor,
    pub buy_active: ThemeColor,
    pub buy_disabled: ThemeColor,
    pub buy_disabled_foreground: ThemeColor,
    pub buy_ring: ThemeColor,
    pub buy_foreground: ThemeColor,
    pub sell: ThemeColor,
    pub sell_hover: ThemeColor,
    pub sell_active: ThemeColor,
    pub sell_disabled: ThemeColor,
    pub sell_disabled_foreground: ThemeColor,
    pub sell_ring: ThemeColor,
    pub sell_foreground: ThemeColor,
    pub ring: ThemeColor,
    /// Portable chart bullish token. Aeris Charts remains authoritative for chart rendering.
    pub bullish: ThemeColor,
    /// Portable chart bearish token. Aeris Charts remains authoritative for chart rendering.
    pub bearish: ThemeColor,
}

/// Native application dimensions that are not part of the portable CSS token contract.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeDimensions {
    pub app_header_height: f32,
    pub border_width: f32,
}

/// A fully resolved `Aeris` theme suitable for a single paint revision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AerisTheme {
    pub mode: ThemeMode,
    pub colors: ThemeColors,
    pub dimensions: ThemeDimensions,
}

impl AerisTheme {
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
                border_width: BORDER_WIDTH,
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
    pub fn color_tokens(self) -> [ColorToken; 46] {
        let colors = self.colors;
        let sources = match self.mode {
            ThemeMode::Light => LIGHT_COLOR_SOURCES,
            ThemeMode::Dark => DARK_COLOR_SOURCES,
        };
        [
            ColorToken::new("surface", sources[0], colors.surface),
            ColorToken::new("surface-secondary", sources[1], colors.surface_secondary),
            ColorToken::new("border", sources[2], colors.border),
            ColorToken::new("border-secondary", sources[3], colors.border_secondary),
            ColorToken::new("text-primary", sources[4], colors.text_primary),
            ColorToken::new("text-secondary", sources[5], colors.text_secondary),
            ColorToken::new("text-muted", sources[6], colors.text_muted),
            ColorToken::new("text-positive", sources[7], colors.text_positive),
            ColorToken::new("text-negative", sources[8], colors.text_negative),
            ColorToken::new("hover-bg", sources[9], colors.hover_bg),
            ColorToken::new("active-bg", sources[10], colors.active_bg),
            ColorToken::new("icon", sources[11], colors.icon),
            ColorToken::new("icon-active", sources[12], colors.icon_active),
            ColorToken::new("primary", sources[13], colors.primary),
            ColorToken::new("primary-foreground", sources[14], colors.primary_foreground),
            ColorToken::new("danger", sources[15], colors.danger),
            ColorToken::new("danger-foreground", sources[16], colors.danger_foreground),
            ColorToken::new("danger-disabled", sources[17], colors.danger_disabled),
            ColorToken::new(
                "danger-disabled-foreground",
                sources[18],
                colors.danger_disabled_foreground,
            ),
            ColorToken::new("danger-ring", sources[19], colors.danger_ring),
            ColorToken::new("warning", sources[20], colors.warning),
            ColorToken::new("positive", sources[21], colors.positive),
            ColorToken::new("positive-subtle", sources[22], colors.positive_subtle),
            ColorToken::new("negative-subtle", sources[23], colors.negative_subtle),
            ColorToken::new("button-fill", sources[24], colors.button_fill),
            ColorToken::new("button-fill-hover", sources[25], colors.button_fill_hover),
            ColorToken::new("button-fill-active", sources[26], colors.button_fill_active),
            ColorToken::new(
                "button-fill-foreground",
                sources[27],
                colors.button_fill_foreground,
            ),
            ColorToken::new("button-fill-subtle", sources[28], colors.button_fill_subtle),
            ColorToken::new("buy", sources[29], colors.buy),
            ColorToken::new("buy-hover", sources[30], colors.buy_hover),
            ColorToken::new("buy-active", sources[31], colors.buy_active),
            ColorToken::new("buy-disabled", sources[32], colors.buy_disabled),
            ColorToken::new(
                "buy-disabled-foreground",
                sources[33],
                colors.buy_disabled_foreground,
            ),
            ColorToken::new("buy-ring", sources[34], colors.buy_ring),
            ColorToken::new("buy-foreground", sources[35], colors.buy_foreground),
            ColorToken::new("sell", sources[36], colors.sell),
            ColorToken::new("sell-hover", sources[37], colors.sell_hover),
            ColorToken::new("sell-active", sources[38], colors.sell_active),
            ColorToken::new("sell-disabled", sources[39], colors.sell_disabled),
            ColorToken::new(
                "sell-disabled-foreground",
                sources[40],
                colors.sell_disabled_foreground,
            ),
            ColorToken::new("sell-ring", sources[41], colors.sell_ring),
            ColorToken::new("sell-foreground", sources[42], colors.sell_foreground),
            ColorToken::new("ring", sources[43], colors.ring),
            ColorToken::new("bullish", sources[44], colors.bullish),
            ColorToken::new("bearish", sources[45], colors.bearish),
        ]
    }
}

impl Default for AerisTheme {
    fn default() -> Self {
        Self::dark()
    }
}

fn light_colors() -> ThemeColors {
    generated_colors(LIGHT_COLORS)
}

fn dark_colors() -> ThemeColors {
    generated_colors(DARK_COLORS)
}

fn generated_colors(values: [[u8; 4]; 46]) -> ThemeColors {
    let color = |index: usize| {
        let [red, green, blue, alpha] = values[index];
        ThemeColor::from_rgba8(red, green, blue, alpha)
    };
    ThemeColors {
        surface: color(0),
        surface_secondary: color(1),
        border: color(2),
        border_secondary: color(3),
        text_primary: color(4),
        text_secondary: color(5),
        text_muted: color(6),
        text_positive: color(7),
        text_negative: color(8),
        hover_bg: color(9),
        active_bg: color(10),
        icon: color(11),
        icon_active: color(12),
        primary: color(13),
        primary_foreground: color(14),
        danger: color(15),
        danger_foreground: color(16),
        danger_disabled: color(17),
        danger_disabled_foreground: color(18),
        danger_ring: color(19),
        warning: color(20),
        positive: color(21),
        positive_subtle: color(22),
        negative_subtle: color(23),
        button_fill: color(24),
        button_fill_hover: color(25),
        button_fill_active: color(26),
        button_fill_foreground: color(27),
        button_fill_subtle: color(28),
        buy: color(29),
        buy_hover: color(30),
        buy_active: color(31),
        buy_disabled: color(32),
        buy_disabled_foreground: color(33),
        buy_ring: color(34),
        buy_foreground: color(35),
        sell: color(36),
        sell_hover: color(37),
        sell_active: color(38),
        sell_disabled: color(39),
        sell_disabled_foreground: color(40),
        sell_ring: color(41),
        sell_foreground: color(42),
        ring: color(43),
        bullish: color(44),
        bearish: color(45),
    }
}

/// The complete set of concrete radius tokens. No additional radius is valid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadiusToken {
    Sm,
    Default,
    Medium,
    Button,
    Full,
}

impl RadiusToken {
    /// Returns the canonical platform identifier.
    #[must_use]
    pub const fn canonical_identifier(self) -> &'static str {
        match self {
            Self::Sm => "radius-small",
            Self::Default => "radius-default",
            Self::Medium => "radius-medium",
            Self::Button => "radius-button",
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
            Self::Sm => RADIUS_SMALL,
            Self::Default => RADIUS_DEFAULT,
            Self::Medium => RADIUS_MEDIUM,
            Self::Button => RADIUS_BUTTON,
            Self::Full => RADIUS_LARGE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AerisTheme, ColorToken, RadiusToken, ThemeColor, TypographyRole, platform_font_family,
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
        let light = AerisTheme::light().colors;
        let dark = AerisTheme::dark().colors;

        assert_eq!(light.surface, ThemeColor::from_rgb8(255, 255, 255));
        assert_eq!(
            light.surface_secondary,
            ThemeColor::from_rgb8(250, 250, 250)
        );
        assert_eq!(light.border_secondary, light.border);
        assert_eq!(light.border, ThemeColor::from_rgb8(229, 229, 229));
        assert_eq!(
            light.danger_foreground,
            ThemeColor::from_rgb8(255, 255, 255)
        );
        assert_eq!(dark.surface, ThemeColor::from_rgb8(31, 31, 31));
        assert_eq!(dark.surface_secondary, ThemeColor::from_rgb8(34, 34, 34));
        assert_eq!(dark.border_secondary, dark.border);
        assert_eq!(light.text_primary, ThemeColor::from_rgb8(34, 34, 34));
        assert_eq!(light.text_secondary, ThemeColor::from_rgb8(100, 100, 101));
        assert_eq!(light.text_muted, ThemeColor::from_rgb8(194, 194, 194));
        assert_eq!(dark.text_secondary, ThemeColor::from_rgb8(194, 194, 194));
        assert_eq!(light.primary, ThemeColor::from_rgb8(0, 145, 255));
        assert_eq!(dark.primary, light.primary);
        assert_eq!(light.warning, ThemeColor::from_rgb8(255, 105, 0));
        assert_eq!(dark.warning, light.warning);
        assert_eq!(light.positive, ThemeColor::from_rgb8(8, 153, 129));
        assert_eq!(dark.positive, light.positive);
        assert_eq!(
            light.primary_foreground,
            ThemeColor::from_rgb8(255, 255, 255)
        );
        assert_eq!(light.button_fill, ThemeColor::from_rgb8(51, 51, 51));
        assert_eq!(dark.button_fill, ThemeColor::from_rgb8(245, 245, 245));
        assert_eq!(dark.button_fill_hover, ThemeColor::from_rgb8(224, 224, 224));
        assert_eq!(
            dark.button_fill_active,
            ThemeColor::from_rgb8(212, 212, 212)
        );
        assert_eq!(
            light.button_fill_foreground,
            ThemeColor::from_rgb8(255, 255, 255)
        );
        assert_eq!(
            dark.button_fill_foreground,
            ThemeColor::from_rgb8(64, 64, 64)
        );
        assert_eq!(light.bullish, ThemeColor::from_rgb8(8, 153, 129));
        assert_eq!(light.bearish, ThemeColor::from_rgb8(247, 82, 95));
        assert_eq!(dark.bullish, light.bullish);
        assert_eq!(dark.bearish, light.bearish);
        assert_eq!(light.hover_bg, ThemeColor::from_rgb8(240, 240, 240));
        assert_eq!(dark.active_bg, ThemeColor::from_rgb8(64, 64, 64));
        let (_, hover_saturation, _, _) = light.hover_bg.hsla_components();
        assert!(hover_saturation.abs() < f32::EPSILON);

        for composited in [
            light.hover_bg.over(light.surface),
            light.active_bg.over(light.surface),
            dark.hover_bg.over(dark.surface),
            dark.active_bg.over(dark.surface),
            light.hover_bg.over(light.surface_secondary),
            light.active_bg.over(light.surface_secondary),
        ] {
            assert!((composited.alpha() - 1.0).abs() < f32::EPSILON);
            assert!((composited.red() - composited.green()).abs() < f32::EPSILON);
            assert!((composited.green() - composited.blue()).abs() < f32::EPSILON);
        }
        assert!((light.hover_bg.over(light.surface).red() - light.surface.red()).abs() > 0.01);
        assert!((dark.hover_bg.over(dark.surface).red() - dark.surface.red()).abs() > 0.01);

        let light_tokens = AerisTheme::light().color_tokens();
        let dark_tokens = AerisTheme::dark().color_tokens();
        assert_eq!(token_source(&light_tokens, "surface"), "#ffffff");
        assert_eq!(token_source(&light_tokens, "border"), "#e5e5e5");
        assert_eq!(token_source(&dark_tokens, "surface"), "#1f1f1f");
        assert_eq!(token_source(&dark_tokens, "hover-bg"), "#333333");
        assert_eq!(token_source(&light_tokens, "text-muted"), "#c2c2c2");
        assert_eq!(token_source(&dark_tokens, "text-secondary"), "#c2c2c2");
        assert_eq!(token_source(&dark_tokens, "danger"), "#f7525f");
        assert_eq!(token_source(&dark_tokens, "warning"), "#ff6900");
        assert_eq!(token_source(&dark_tokens, "positive"), "#089981");
        assert_eq!(token_source(&dark_tokens, "primary"), "#0091ff");
        assert_eq!(token_source(&light_tokens, "button-fill"), "#333333");
        assert_eq!(token_source(&dark_tokens, "button-fill"), "#f5f5f5");
        assert_eq!(token_source(&light_tokens, "danger-foreground"), "#ffffff");
        assert_eq!(token_source(&light_tokens, "bullish"), "#089981");
        assert_eq!(token_source(&light_tokens, "bearish"), "#f7525f");
        assert_eq!(token_source(&dark_tokens, "bullish"), "#089981");
        assert_eq!(token_source(&dark_tokens, "bearish"), "#f7525f");
        assert_eq!(
            token_source(&light_tokens, "ring"),
            "color-mix(in srgb, #c2c2c2 50%, transparent)"
        );
    }

    #[test]
    fn trade_button_tokens_match_the_platform_contract() {
        let light = AerisTheme::light().colors;
        let dark = AerisTheme::dark().colors;
        assert_eq!(light.buy, ThemeColor::from_rgb8(8, 153, 129));
        assert_eq!(light.buy_hover, ThemeColor::from_rgb8(7, 135, 111));
        assert_eq!(light.buy_active, ThemeColor::from_rgb8(5, 111, 92));
        assert_eq!(dark.buy_hover, ThemeColor::from_rgb8(10, 173, 146));
        assert_eq!(dark.buy_active, ThemeColor::from_rgb8(11, 192, 162));
        assert_eq!(light.sell, ThemeColor::from_rgb8(247, 82, 95));
        assert_eq!(light.sell_hover, ThemeColor::from_rgb8(229, 64, 77));
        assert_eq!(light.sell_active, ThemeColor::from_rgb8(201, 48, 60));
        assert_eq!(dark.sell_hover, ThemeColor::from_rgb8(249, 106, 117));
        assert_eq!(dark.sell_active, ThemeColor::from_rgb8(251, 131, 140));

        let light_tokens = AerisTheme::light().color_tokens();
        let dark_tokens = AerisTheme::dark().color_tokens();
        assert_eq!(token_source(&light_tokens, "buy"), "#089981");
        assert_eq!(token_source(&light_tokens, "buy-hover"), "#07876f");
        assert_eq!(token_source(&dark_tokens, "buy-hover"), "#0aad92");
        assert_eq!(token_source(&light_tokens, "sell"), "#f7525f");
        assert_eq!(token_source(&light_tokens, "sell-hover"), "#e5404d");
        assert_eq!(token_source(&dark_tokens, "sell-hover"), "#f96a75");
    }

    #[test]
    fn semantic_status_tokens_match_the_platform_contract() {
        let light = AerisTheme::light().colors;
        let dark = AerisTheme::dark().colors;
        assert_eq!(light.text_positive, light.positive);
        assert_eq!(dark.text_positive, dark.positive);
        assert_eq!(light.text_negative, ThemeColor::from_rgb8(247, 82, 95));
        assert_eq!(dark.text_negative, light.text_negative);
        assert_eq!(light.positive_subtle, ThemeColor::from_rgb8(220, 245, 240));
        assert_eq!(dark.positive_subtle, ThemeColor::from_rgb8(25, 60, 55));
        assert_eq!(light.negative_subtle, ThemeColor::from_rgb8(255, 226, 226));
        assert_eq!(dark.negative_subtle, ThemeColor::from_rgb8(83, 43, 46));

        let light_tokens = AerisTheme::light().color_tokens();
        let dark_tokens = AerisTheme::dark().color_tokens();
        assert_eq!(
            token_source(&light_tokens, "text-positive"),
            "var(--positive)"
        );
        assert_eq!(
            token_source(&light_tokens, "text-negative"),
            "var(--negative)"
        );
        assert_eq!(token_source(&light_tokens, "positive-subtle"), "#dcf5f0");
        assert_eq!(token_source(&dark_tokens, "positive-subtle"), "#193c37");
        assert_eq!(token_source(&light_tokens, "negative-subtle"), "#ffe2e2");
        assert_eq!(token_source(&dark_tokens, "negative-subtle"), "#532b2e");
    }

    #[test]
    fn radius_tokens_match_the_platform_contract() {
        assert_eq!(RadiusToken::Sm.logical_pixels(), 4);
        assert_eq!(RadiusToken::Default.logical_pixels(), 8);
        assert_eq!(RadiusToken::Medium.logical_pixels(), 12);
        assert_eq!(RadiusToken::Button.logical_pixels(), 6);
        assert_eq!(RadiusToken::Full.logical_pixels(), 999);
        assert_eq!(RadiusToken::Sm.css_custom_property(), "--radius-small");
        assert_eq!(
            RadiusToken::Default.css_custom_property(),
            "--radius-default"
        );
        assert_eq!(RadiusToken::Full.css_custom_property(), "--radius-large");
        assert_eq!(RadiusToken::Medium.css_custom_property(), "--radius-medium");
        assert_eq!(RadiusToken::Button.css_custom_property(), "--radius-button");
    }

    #[test]
    fn border_width_matches_the_platform_contract() {
        let css = include_str!("../platform.css");
        assert!((AerisTheme::light().dimensions.border_width - 0.5).abs() < f32::EPSILON);
        assert!((AerisTheme::dark().dimensions.border_width - 0.5).abs() < f32::EPSILON);
        assert_eq!(css.matches("--border-width: 0.5px;").count(), 2);
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

        for theme in [AerisTheme::light(), AerisTheme::dark()] {
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
            "--muted:",
            "--muted-foreground",
            "--disabled-foreground",
            "--accent:",
            "--muted-border",
            "--overlay",
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
            "--input-fill",
            "--input-border",
            "--success",
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
            "input:focus-visible",
            "outline-color: var(--ring);",
            "cursor: not-allowed;",
            "@media (prefers-reduced-motion: reduce)",
        ] {
            assert!(css.contains(required), "CSS is missing `{required}`");
        }
        assert!(!css.contains("HKGrotesk-SemiBold.ttf"));
        assert!(!css.contains("font-weight: 600;"));
        assert!(!css.contains("border-color: var(--primary);"));
        assert!(!css.contains("transform: scale("));
        assert!(css.contains("/* Chart */"));
        assert!(css.contains("--bullish: #089981;"));
        assert!(css.contains("--bearish: #f7525f;"));
        assert_eq!(platform_font_family(), "HK Grotesk");
        assert_eq!(platform_font_stack(), "\"HK Grotesk\", sans-serif");

        let typography = platform_typography();
        assert_eq!(typography.weight(TypographyRole::Normal), 500);
        assert_eq!(typography.weight(TypographyRole::Emphasis), 700);
        assert_eq!(typography.weight(TypographyRole::Strong), 700);
        assert_eq!(typography.tabular_numerals_feature(), "tnum");
    }
}
