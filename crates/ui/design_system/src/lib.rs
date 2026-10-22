//! Typed native mapping of the `Aeris` `platform.css` contract.
//!
//! Token source expressions and resolved sRGB values share one registry. The
//! checked CSS manifest uses generated custom-property names, while painting
//! code consumes typed values without string lookup.

#[macro_use]
mod color_registry;
#[cfg(test)]
mod token_compiler;

include!(concat!(env!("OUT_DIR"), "/platform_tokens.rs"));

/// Canonical portable stylesheet shared with the native presentation layer.
pub const PLATFORM_CSS: &str = include_str!("../platform.css");

/// The bundled platform faces referenced by `platform.css`: Inter (`--font-sans`)
/// and Geist Mono (`--font-mono`) at the regular, medium and bold weights.
pub static PLATFORM_FONT_BYTES: [&[u8]; 6] = [
    include_bytes!("../assets/fonts/inter_regular.ttf"),
    include_bytes!("../assets/fonts/inter_medium.ttf"),
    include_bytes!("../assets/fonts/inter_bold.ttf"),
    include_bytes!("../assets/fonts/geist_mono_regular.ttf"),
    include_bytes!("../assets/fonts/geist_mono_medium.ttf"),
    include_bytes!("../assets/fonts/geist_mono_bold.ttf"),
];

/// Bundled Aeris brand face. It is intentionally separate from the platform
/// typography so only explicit brand lockups opt into it.
pub static BRAND_FONT_BYTES: &[u8] =
    include_bytes!("../assets/fonts/faculty-glyphic/FacultyGlyphic-Regular.ttf");

/// The display face used for the Aeris wordmark in native chrome.
pub const BRAND_FONT_FAMILY: &str = "Faculty Glyphic";

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
    mono_family: &'static str,
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

    /// The `--font-mono` family used for numeric data cells.
    #[must_use]
    pub const fn mono_family(self) -> &'static str {
        self.mono_family
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
        mono_family: FONT_MONO_FAMILY,
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

/// Returns the primary family from the canonical `--font-mono` CSS value.
#[must_use]
pub fn platform_mono_font_family() -> &'static str {
    platform_typography().mono_family()
}

/// Returns the dedicated Aeris brand font family.
#[must_use]
pub const fn brand_font_family() -> &'static str {
    BRAND_FONT_FAMILY
}

/// A calendar date in the platform format, `DD-MM-YY`: day first, two-digit year.
#[must_use]
pub fn platform_date_label(year: i32, month: u32, day: u32) -> String {
    format!("{day:02}-{month:02}-{:02}", year.rem_euclid(100))
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

    /// Composites this token over `backdrop` in sRGB (source-over).
    ///
    /// CSS interaction tokens such as `--hover-bg` describe a translucent mix
    /// over the resting fill. Painting them as a replacement fill lets the GPU
    /// blend through HSL and shift the hue, so native hover/active states
    /// resolve the mix here. Achromatic results keep identical channels so the
    /// overlay cannot introduce a conversion tint.
    ///
    /// The result is opaque only when the backdrop is, so resolve a translucent
    /// fill over the opaque surface it sits on before using it as a replacement fill.
    #[must_use]
    pub fn over(self, backdrop: ThemeColor) -> Self {
        let top = self.alpha;
        let under = backdrop.alpha * (1.0 - top);
        let alpha = top + under;
        if alpha <= f32::EPSILON {
            return Self {
                alpha: 0.0,
                ..backdrop
            };
        }
        let mix = |top_channel: f32, under_channel: f32| {
            (top_channel * top + under_channel * under) / alpha
        };
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
            alpha: alpha.min(1.0),
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

    /// Returns the CSS `rgba()` form for renderers that take CSS color strings.
    #[must_use]
    pub fn css_rgba(self) -> String {
        format!(
            "rgba({}, {}, {}, {:.4})",
            channel_to_u8(self.red),
            channel_to_u8(self.green),
            channel_to_u8(self.blue),
            self.alpha.clamp(0.0, 1.0)
        )
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

macro_rules! theme_colors {
    ($($(#[$meta:meta])* $field:ident => $identifier:literal,)*) => {
        /// Every color token in `platform.css`, resolved for one application mode.
        #[derive(Clone, Copy, Debug, PartialEq)]
        pub struct ThemeColors {
            $($(#[$meta])* pub $field: ThemeColor,)*
        }

        /// Canonical identifiers in registry order, parallel to the generated arrays.
        const COLOR_IDENTIFIERS: [&str; PLATFORM_COLOR_COUNT] = [$($identifier),*];

        impl ThemeColors {
            fn from_generated(values: [[u8; 4]; PLATFORM_COLOR_COUNT]) -> Self {
                let [$($field),*] = values;
                Self {
                    $($field: ThemeColor::from_rgba8($field[0], $field[1], $field[2], $field[3]),)*
                }
            }

            const fn in_registry_order(self) -> [ThemeColor; PLATFORM_COLOR_COUNT] {
                [$(self.$field),*]
            }
        }
    };
}

platform_color_registry!(theme_colors);

/// Native application dimensions that are not part of the portable CSS token contract.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeDimensions {
    pub app_header_height: f32,
    /// Width of every control and layer edge: the `--border-width` token.
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
                border_width: f32::from(BORDER_WIDTH),
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

    /// Returns canonical source metadata for every color token resolved here.
    #[must_use]
    pub fn color_tokens(self) -> [ColorToken; PLATFORM_COLOR_COUNT] {
        let colors = self.colors.in_registry_order();
        let sources = match self.mode {
            ThemeMode::Light => LIGHT_COLOR_SOURCES,
            ThemeMode::Dark => DARK_COLOR_SOURCES,
        };
        std::array::from_fn(|index| {
            ColorToken::new(COLOR_IDENTIFIERS[index], sources[index], colors[index])
        })
    }
}

impl Default for AerisTheme {
    fn default() -> Self {
        Self::dark()
    }
}

fn light_colors() -> ThemeColors {
    ThemeColors::from_generated(LIGHT_COLORS)
}

fn dark_colors() -> ThemeColors {
    ThemeColors::from_generated(DARK_COLORS)
}

/// The complete set of concrete radius tokens. No additional radius is valid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadiusToken {
    Sm,
    Default,
    Medium,
    /// `--radius-compact`: compact controls such as small and default buttons.
    Compact,
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
            Self::Compact => "radius-compact",
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
            Self::Compact => RADIUS_COMPACT,
            Self::Full => RADIUS_LARGE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AerisTheme, ColorToken, RadiusToken, ThemeColor, TypographyRole, brand_font_family,
        platform_date_label, platform_font_family, platform_font_stack, platform_mono_font_family,
        platform_typography,
    };

    #[test]
    fn platform_dates_read_day_month_two_digit_year() {
        assert_eq!(platform_date_label(2026, 10, 9), "09-10-26");
        assert_eq!(platform_date_label(2000, 1, 1), "01-01-00");
    }

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
        assert_eq!(light.border, ThemeColor::from_rgba8(10, 10, 10, 51));
        assert_eq!(
            light.border_secondary,
            ThemeColor::from_rgba8(10, 10, 10, 26)
        );
        assert_eq!(
            light.danger_foreground,
            ThemeColor::from_rgb8(255, 255, 255)
        );
        assert_eq!(dark.surface, ThemeColor::from_rgb8(0, 0, 0));
        assert_eq!(dark.surface_secondary, ThemeColor::from_rgb8(23, 23, 23));
        assert_eq!(dark.border, ThemeColor::from_rgb8(44, 44, 44));
        assert_eq!(dark.border_secondary, ThemeColor::from_rgb8(64, 64, 64));
        assert_eq!(light.text_default, ThemeColor::from_rgb8(10, 10, 10));
        assert_eq!(dark.text_default, ThemeColor::from_rgb8(237, 237, 237));
        assert_eq!(
            light.text_secondary,
            ThemeColor::from_rgba8(10, 10, 10, 148)
        );
        assert_eq!(light.text_interactive, light.text_secondary);
        assert_eq!(light.icon, light.text_interactive);
        assert_eq!(light.text_muted, ThemeColor::from_rgba8(10, 10, 10, 64));
        assert_eq!(
            dark.text_secondary,
            ThemeColor::from_rgba8(250, 250, 250, 140)
        );
        assert_eq!(light.primary, ThemeColor::from_rgb8(0, 110, 221));
        assert_eq!(dark.primary, light.primary);
        assert_eq!(light.warning, ThemeColor::from_rgb8(255, 105, 0));
        assert_eq!(dark.warning, light.warning);
        assert_eq!(light.positive, ThemeColor::from_rgb8(8, 153, 129));
        assert_eq!(dark.positive, light.positive);
        assert_eq!(
            light.primary_foreground,
            ThemeColor::from_rgb8(255, 255, 255)
        );
        assert_eq!(light.bullish, ThemeColor::from_rgb8(8, 153, 129));
        assert_eq!(light.bearish, ThemeColor::from_rgb8(247, 82, 95));
        assert_eq!(dark.bullish, light.bullish);
        assert_eq!(dark.bearish, light.bearish);
        assert_eq!(light.hover_bg, ThemeColor::from_rgba8(10, 10, 10, 13));
        assert_eq!(dark.active_bg, ThemeColor::from_rgba8(250, 250, 250, 38));
        let (_, hover_saturation, _, _) = light.hover_bg.hsla_components();
        assert!(hover_saturation.abs() < f32::EPSILON);

        for composited in [
            light.hover_bg.over(light.surface),
            light.active_bg.over(light.surface),
            dark.hover_bg.over(dark.surface),
            dark.active_bg.over(dark.surface),
            light.hover_bg.over(light.surface_secondary),
            light.active_bg.over(light.surface_secondary),
            dark.hover_bg.over(dark.surface_secondary),
            dark.active_bg.over(dark.surface_secondary),
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
        assert_eq!(token_source(&light_tokens, "border"), "#0a0a0a33");
        assert_eq!(token_source(&dark_tokens, "surface"), "#000000");
        assert_eq!(token_source(&dark_tokens, "hover-bg"), "#fafafa1f");
        assert_eq!(token_source(&light_tokens, "text-muted"), "#0a0a0a40");
        assert_eq!(token_source(&light_tokens, "text-secondary"), "#0a0a0a94");
        assert_eq!(token_source(&dark_tokens, "text-secondary"), "#fafafa8c");
        assert_eq!(token_source(&dark_tokens, "danger"), "#f7525f");
        assert_eq!(token_source(&dark_tokens, "warning"), "#ff6900");
        assert_eq!(token_source(&dark_tokens, "positive"), "#089981");
        assert_eq!(token_source(&dark_tokens, "primary"), "#006EDD");
        assert_eq!(token_source(&light_tokens, "button-fill"), "#0a0a0af2");
        assert_eq!(token_source(&dark_tokens, "button-fill"), "#fafafad9");
        assert_eq!(token_source(&light_tokens, "danger-foreground"), "#ffffff");
        for tokens in [&light_tokens, &dark_tokens] {
            assert_eq!(token_source(tokens, "bullish"), "var(--positive)");
            assert_eq!(token_source(tokens, "bearish"), "var(--negative)");
        }
        assert_eq!(
            token_source(&light_tokens, "ring-primary"),
            "var(--primary)"
        );
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            assert_eq!(theme.colors.ring_primary, theme.colors.primary);
        }
    }

    #[test]
    fn compositing_over_a_translucent_tint_keeps_its_transparency() {
        let dark = AerisTheme::dark().colors;
        let tinted = dark.hover_bg.over(dark.disabled_bg);
        assert!(tinted.alpha() < 1.0);
        assert!(tinted.alpha() > dark.hover_bg.alpha());

        let hover = dark.hover_bg.over(dark.surface_secondary);
        assert!((hover.alpha() - 1.0).abs() < f32::EPSILON);
        // A hover over the dark secondary surface stays a subtle lift, never a near-white fill.
        assert!(hover.red() > dark.surface_secondary.red());
        assert!(hover.red() < 0.3);
    }

    #[test]
    fn button_fill_tokens_match_the_platform_contract() {
        let light = AerisTheme::light().colors;
        let dark = AerisTheme::dark().colors;

        assert_eq!(light.button_fill, ThemeColor::from_rgba8(10, 10, 10, 242));
        assert_eq!(dark.button_fill, ThemeColor::from_rgba8(250, 250, 250, 217));
        assert_eq!(
            dark.button_fill_hover,
            ThemeColor::from_rgba8(250, 250, 250, 195)
        );
        assert_eq!(
            dark.button_fill_active,
            ThemeColor::from_rgba8(250, 250, 250, 173)
        );
        assert_eq!(
            light.button_fill_foreground,
            ThemeColor::from_rgba8(250, 250, 250, 242)
        );
        assert_eq!(
            dark.button_fill_foreground,
            ThemeColor::from_rgba8(10, 10, 10, 217)
        );
    }

    #[test]
    fn popover_layer_tokens_match_the_platform_contract() {
        let light = AerisTheme::light().colors;
        let dark = AerisTheme::dark().colors;

        // Floating layers paint `surface-secondary` over arbitrary content, so it must stay
        // opaque in both modes.
        for popover in [light.surface_secondary, dark.surface_secondary] {
            assert!((popover.alpha() - 1.0).abs() < f32::EPSILON);
        }
        let dark_tokens = AerisTheme::dark().color_tokens();
        assert_eq!(token_source(&dark_tokens, "surface-secondary"), "#171717");
        assert_eq!(token_source(&dark_tokens, "border-secondary"), "#404040");
        // Floating layers draw their edge with `border-secondary`; the retired popover tokens
        // must not return.
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            for retired in [
                "surface-popover",
                "border-popover",
                "border-popover-highlight",
            ] {
                assert!(
                    theme
                        .color_tokens()
                        .iter()
                        .all(|token| token.canonical_identifier != retired),
                    "retired popover token --{retired} is back"
                );
            }
        }
    }

    /// `--ring-primary` is the only focus style; the retired neutral and per-action rings must
    /// not return.
    #[test]
    fn ring_primary_is_the_only_focus_token() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            for retired in ["ring", "primary-ring", "buy-ring", "sell-ring"] {
                assert!(
                    theme
                        .color_tokens()
                        .iter()
                        .all(|token| token.canonical_identifier != retired),
                    "retired focus token --{retired} is back"
                );
            }
        }
    }

    #[test]
    fn big_trade_bubbles_are_translucent_chart_direction_colors() {
        let light = AerisTheme::light().colors;
        let dark = AerisTheme::dark().colors;
        assert_eq!(light.buy_bubble, ThemeColor::from_rgba8(8, 153, 129, 89));
        assert_eq!(light.sell_bubble, ThemeColor::from_rgba8(247, 82, 95, 89));
        assert_eq!(dark.buy_bubble, light.buy_bubble);
        assert_eq!(dark.sell_bubble, light.sell_bubble);
        assert_eq!(light.buy_bubble.css_rgba(), "rgba(8, 153, 129, 0.3490)");
        assert_eq!(light.bearish.css_rgba(), "rgba(247, 82, 95, 1.0000)");
        assert_eq!(
            token_source(&AerisTheme::light().color_tokens(), "buy-bubble"),
            "color-mix(in srgb, #089981 35%, transparent)"
        );
        assert_eq!(
            token_source(&AerisTheme::dark().color_tokens(), "sell-bubble"),
            "color-mix(in srgb, #f7525f 35%, transparent)"
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
    fn order_book_tokens_match_the_platform_contract() {
        let light = AerisTheme::light().colors;
        let dark = AerisTheme::dark().colors;
        assert_eq!(light.book_bid_fill, ThemeColor::from_rgb8(153, 197, 241));
        assert_eq!(light.book_bid_text, light.primary);
        assert_eq!(light.book_ask_fill, ThemeColor::from_rgb8(255, 172, 191));
        assert_eq!(light.book_ask_text, ThemeColor::from_rgb8(191, 35, 71));
        assert_eq!(light.book_fill_text, ThemeColor::from_rgb8(0, 0, 0));
        assert_eq!(dark.book_bid_fill, ThemeColor::from_rgb8(0, 39, 77));
        assert_eq!(dark.book_bid_text, ThemeColor::from_rgb8(51, 139, 228));
        assert_eq!(dark.book_ask_fill, ThemeColor::from_rgb8(89, 16, 33));
        assert_eq!(dark.book_ask_text, ThemeColor::from_rgb8(255, 78, 118));
        assert_eq!(dark.book_fill_text, ThemeColor::from_rgb8(255, 255, 255));

        let light_tokens = AerisTheme::light().color_tokens();
        let dark_tokens = AerisTheme::dark().color_tokens();
        assert_eq!(
            token_source(&light_tokens, "book-bid-fill"),
            "color-mix(in srgb, var(--primary) 40%, var(--surface))"
        );
        assert_eq!(
            token_source(&light_tokens, "book-ask-text"),
            "color-mix(in srgb, #ff2f5e 75%, #000000)"
        );
        assert_eq!(
            token_source(&dark_tokens, "book-bid-text"),
            "color-mix(in srgb, var(--primary) 80%, #ffffff)"
        );
        assert_eq!(
            token_source(&dark_tokens, "book-ask-fill"),
            "color-mix(in srgb, #ff2f5e 35%, var(--surface))"
        );
        assert_eq!(token_source(&dark_tokens, "book-fill-text"), "#ffffff");
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
        assert_eq!(RadiusToken::Medium.logical_pixels(), 14);
        assert_eq!(RadiusToken::Compact.logical_pixels(), 6);
        assert_eq!(RadiusToken::Full.logical_pixels(), 999);
        assert_eq!(RadiusToken::Sm.css_custom_property(), "--radius-small");
        assert_eq!(
            RadiusToken::Default.css_custom_property(),
            "--radius-default"
        );
        assert_eq!(RadiusToken::Full.css_custom_property(), "--radius-large");
        assert_eq!(RadiusToken::Medium.css_custom_property(), "--radius-medium");
        assert_eq!(
            RadiusToken::Compact.css_custom_property(),
            "--radius-compact"
        );
    }

    #[test]
    fn border_width_matches_the_platform_contract() {
        let css = include_str!("../platform.css");
        assert!((AerisTheme::light().dimensions.border_width - 1.0).abs() < f32::EPSILON);
        assert!((AerisTheme::dark().dimensions.border_width - 1.0).abs() < f32::EPSILON);
        assert_eq!(css.matches("--border-width: 1px;").count(), 2);
    }

    #[test]
    fn bundled_platform_fonts_keep_platform_faces_separate_from_the_brand_face() {
        let font_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
        let mut bundled_fonts = std::fs::read_dir(&font_dir)
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
            [
                "geist_mono_bold.ttf",
                "geist_mono_medium.ttf",
                "geist_mono_regular.ttf",
                "inter_bold.ttf",
                "inter_medium.ttf",
                "inter_regular.ttf",
            ]
        );
        assert!(font_dir.join("inter_license.txt").is_file());
        assert!(font_dir.join("geist_license.txt").is_file());
        let brand_font = font_dir.join("faculty-glyphic/FacultyGlyphic-Regular.ttf");
        let brand_license = font_dir.join("faculty-glyphic/OFL.txt");
        assert!(brand_font.is_file());
        assert!(brand_license.is_file());
        assert_eq!(brand_font_family(), "Faculty Glyphic");
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
            "--font-sans: \"Inter\", sans-serif;",
            "--font-mono: \"Geist Mono\", monospace;",
            "--font-weight-normal: 400;",
            "--font-weight-emphasis: 700;",
            "--font-weight-strong: 700;",
            "--font-feature-tabular-numerals: \"tnum\";",
            "inter_regular.ttf",
            "inter_medium.ttf",
            "inter_bold.ttf",
            "geist_mono_regular.ttf",
            "geist_mono_medium.ttf",
            "geist_mono_bold.ttf",
            "font-weight: 400;",
            "font-weight: 500;",
            "font-weight: var(--font-weight-strong);",
            "font-variant-numeric: tabular-nums;",
            "-webkit-font-smoothing: antialiased;",
            "font-synthesis: none;",
            "transition: background-color 150ms ease, color 150ms ease;",
            "outline: 1px solid var(--ring-primary);",
            "outline-offset: -1px;",
            "input:focus-visible",
            "outline-color: var(--ring-primary);",
            "cursor: not-allowed;",
            "@media (prefers-reduced-motion: reduce)",
        ] {
            assert!(css.contains(required), "CSS is missing `{required}`");
        }
        assert!(!css.contains("HK Grotesk"));
        assert!(!css.contains("font-weight: 600;"));
        assert!(!css.contains("border-color: var(--primary);"));
        assert!(!css.contains("transform: scale("));
        assert!(css.contains("/* Chart */"));
        assert!(css.contains("--bullish: var(--positive);"));
        assert!(css.contains("--bearish: var(--negative);"));
        assert!(css.contains("--buy-bubble: color-mix(in srgb, #089981 35%, transparent);"));
        assert!(css.contains("--sell-bubble: color-mix(in srgb, #f7525f 35%, transparent);"));
        assert_eq!(platform_font_family(), "Inter");
        assert_eq!(platform_font_stack(), "\"Inter\", sans-serif");
        assert_eq!(platform_mono_font_family(), "Geist Mono");

        let typography = platform_typography();
        assert_eq!(typography.weight(TypographyRole::Normal), 400);
        assert_eq!(typography.weight(TypographyRole::Emphasis), 700);
        assert_eq!(typography.weight(TypographyRole::Strong), 700);
        assert_eq!(typography.tabular_numerals_feature(), "tnum");
    }
}
