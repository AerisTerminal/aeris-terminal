//! Typed design-system contracts governed by `platform_design_system.md`.
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
    pub surface_secondary: ThemeColor,
    pub surface_tertiary: ThemeColor,
    pub surface_quaternary: ThemeColor,
    pub card: ThemeColor,
    pub card_foreground: ThemeColor,
    pub popover: ThemeColor,
    pub popover_foreground: ThemeColor,
    pub primary: ThemeColor,
    pub primary_foreground: ThemeColor,
    pub secondary: ThemeColor,
    pub secondary_foreground: ThemeColor,
    pub muted: ThemeColor,
    pub muted_foreground: ThemeColor,
    pub text_secondary: ThemeColor,
    pub text_muted: ThemeColor,
    pub text_placeholder: ThemeColor,
    pub text_unavailable: ThemeColor,
    pub icon_color: ThemeColor,
    pub accent: ThemeColor,
    pub accent_foreground: ThemeColor,
    pub destructive: ThemeColor,
    pub destructive_foreground: ThemeColor,
    pub border: ThemeColor,
    pub input: ThemeColor,
    pub input_surface: ThemeColor,
    pub ring: ThemeColor,
    pub interactive_neutral_hover_bg: ThemeColor,
    pub interactive_neutral_hover_fg: ThemeColor,
    pub interactive_neutral_active_bg: ThemeColor,
    pub interactive_neutral_active_fg: ThemeColor,
    pub chart_palette: [ThemeColor; 5],
    pub profit: ThemeColor,
    pub loss: ThemeColor,
    pub warning: ThemeColor,
    pub info: ThemeColor,
    pub feature: ThemeColor,
    pub chart_candle_up: ThemeColor,
    pub chart_candle_down: ThemeColor,
    pub chart_volume_up: ThemeColor,
    pub chart_volume_down: ThemeColor,
    pub chart_axis_text: ThemeColor,
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
    #[allow(clippy::too_many_lines)]
    pub fn color_tokens(self) -> [ColorToken; 47] {
        let colors = self.colors;
        let dark = self.mode == ThemeMode::Dark;
        [
            ColorToken::new(
                "background",
                mode_source(
                    dark,
                    "color(display-p3 1 1 1)",
                    "color(display-p3 0.09 0.09 0.09)",
                ),
                colors.background,
            ),
            ColorToken::new(
                "foreground",
                mode_source(
                    dark,
                    "color(display-p3 0.2 0.2 0.2)",
                    "color(display-p3 0.922 0.922 0.922)",
                ),
                colors.foreground,
            ),
            ColorToken::new(
                "surface_secondary",
                mode_source(
                    dark,
                    "color(display-p3 0.988 0.988 0.988)",
                    "color(display-p3 0.106 0.106 0.106)",
                ),
                colors.surface_secondary,
            ),
            ColorToken::new(
                "surface_tertiary",
                mode_source(
                    dark,
                    "color(display-p3 0.945 0.945 0.945)",
                    "color(display-p3 0.114 0.114 0.114)",
                ),
                colors.surface_tertiary,
            ),
            ColorToken::new(
                "surface_quaternary",
                mode_source(
                    dark,
                    "color(display-p3 0.922 0.922 0.922)",
                    "color(display-p3 0.133 0.133 0.133)",
                ),
                colors.surface_quaternary,
            ),
            ColorToken::new(
                "card",
                mode_source(dark, "var(--surface_secondary)", "var(--surface_secondary)"),
                colors.card,
            ),
            ColorToken::new(
                "card_foreground",
                "var(--foreground)",
                colors.card_foreground,
            ),
            ColorToken::new("popover", "var(--surface_secondary)", colors.popover),
            ColorToken::new(
                "popover_foreground",
                "var(--foreground)",
                colors.popover_foreground,
            ),
            ColorToken::new("primary", "#3e63dd", colors.primary),
            ColorToken::new(
                "primary_foreground",
                "oklch(0.97 0.014 254.604)",
                colors.primary_foreground,
            ),
            ColorToken::new("secondary", "var(--surface_tertiary)", colors.secondary),
            ColorToken::new(
                "secondary_foreground",
                "var(--foreground)",
                colors.secondary_foreground,
            ),
            ColorToken::new("muted", "var(--surface_tertiary)", colors.muted),
            ColorToken::new(
                "muted_foreground",
                mode_source(
                    dark,
                    "color(display-p3 0.6 0.6 0.6)",
                    "color(display-p3 0.506 0.506 0.506)",
                ),
                colors.muted_foreground,
            ),
            ColorToken::new(
                "text_secondary",
                mode_source(
                    dark,
                    "color(display-p3 0.4 0.4 0.4)",
                    "color(display-p3 0.702 0.702 0.702)",
                ),
                colors.text_secondary,
            ),
            ColorToken::new(
                "text_muted",
                mode_source(
                    dark,
                    "color(display-p3 0.6 0.6 0.6)",
                    "color(display-p3 0.506 0.506 0.506)",
                ),
                colors.text_muted,
            ),
            ColorToken::new(
                "text_placeholder",
                mode_source(
                    dark,
                    "color(display-p3 0.702 0.702 0.702)",
                    "color(display-p3 0.4 0.4 0.4)",
                ),
                colors.text_placeholder,
            ),
            ColorToken::new(
                "text_unavailable",
                mode_source(
                    dark,
                    "color(display-p3 0.8 0.8 0.8)",
                    "color(display-p3 0.298 0.298 0.298)",
                ),
                colors.text_unavailable,
            ),
            ColorToken::new(
                "icon_color",
                mode_source(
                    dark,
                    "color(display-p3 0.4 0.4 0.4)",
                    "color(display-p3 0.702 0.702 0.702)",
                ),
                colors.icon_color,
            ),
            ColorToken::new("accent", "var(--surface_tertiary)", colors.accent),
            ColorToken::new(
                "accent_foreground",
                "var(--foreground)",
                colors.accent_foreground,
            ),
            ColorToken::new(
                "destructive",
                mode_source(
                    dark,
                    "oklch(0.577 0.245 27.325)",
                    "oklch(0.704 0.191 22.216)",
                ),
                colors.destructive,
            ),
            ColorToken::new(
                "destructive_foreground",
                "oklch(0.985 0 0)",
                colors.destructive_foreground,
            ),
            ColorToken::new(
                "border",
                mode_source(
                    dark,
                    "color(display-p3 0.945 0.945 0.945)",
                    "color(display-p3 0.114 0.114 0.114)",
                ),
                colors.border,
            ),
            ColorToken::new(
                "input",
                mode_source(
                    dark,
                    "color(display-p3 0.922 0.922 0.922)",
                    "color(display-p3 0.133 0.133 0.133)",
                ),
                colors.input,
            ),
            ColorToken::new(
                "input_surface",
                mode_source(
                    dark,
                    "color(display-p3 0.988 0.988 0.988)",
                    "color(display-p3 0.106 0.106 0.106)",
                ),
                colors.input_surface,
            ),
            ColorToken::new(
                "ring",
                mode_source(dark, "oklch(0.708 0 0)", "oklch(0.556 0 0)"),
                colors.ring,
            ),
            ColorToken::new(
                "interactive_neutral_hover_bg",
                mode_source(
                    dark,
                    "color(display-p3 0 0 0 / 0.039)",
                    "color(display-p3 1 1 1 / 0.059)",
                ),
                colors.interactive_neutral_hover_bg,
            ),
            ColorToken::new(
                "interactive_neutral_hover_fg",
                "var(--foreground)",
                colors.interactive_neutral_hover_fg,
            ),
            ColorToken::new(
                "interactive_neutral_active_bg",
                mode_source(
                    dark,
                    "color(display-p3 0 0 0 / 0.078)",
                    "color(display-p3 1 1 1 / 0.102)",
                ),
                colors.interactive_neutral_active_bg,
            ),
            ColorToken::new(
                "interactive_neutral_active_fg",
                "var(--foreground)",
                colors.interactive_neutral_active_fg,
            ),
            ColorToken::new("chart_1", "oklch(0.87 0 0)", colors.chart_palette[0]),
            ColorToken::new("chart_2", "oklch(0.556 0 0)", colors.chart_palette[1]),
            ColorToken::new("chart_3", "oklch(0.439 0 0)", colors.chart_palette[2]),
            ColorToken::new("chart_4", "oklch(0.371 0 0)", colors.chart_palette[3]),
            ColorToken::new("chart_5", "oklch(0.269 0 0)", colors.chart_palette[4]),
            ColorToken::new("profit", "oklch(0.683 0.151 160.997)", colors.profit),
            ColorToken::new("loss", "oklch(0.674 0.215 18.124)", colors.loss),
            ColorToken::new("warning", "oklch(0.769 0.165 70.08)", colors.warning),
            ColorToken::new("info", "oklch(0.555 0.245 266.681)", colors.info),
            ColorToken::new("feature", "oklch(0.541 0.247 293.009)", colors.feature),
            ColorToken::new("chart_candle_up", "var(--profit)", colors.chart_candle_up),
            ColorToken::new("chart_candle_down", "var(--loss)", colors.chart_candle_down),
            ColorToken::new(
                "chart_volume_up",
                mode_source(
                    dark,
                    "oklch(from var(--profit) l c h / 34%)",
                    "oklch(from var(--profit) l c h / 32%)",
                ),
                colors.chart_volume_up,
            ),
            ColorToken::new(
                "chart_volume_down",
                mode_source(
                    dark,
                    "oklch(from var(--loss) l c h / 30%)",
                    "oklch(from var(--loss) l c h / 28%)",
                ),
                colors.chart_volume_down,
            ),
            ColorToken::new(
                "chart_axis_text",
                mode_source(dark, "#0a0a0a", "var(--foreground)"),
                colors.chart_axis_text,
            ),
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
    foundational_colors(
        ThemeMode::Light,
        ThemeColor::from_rgb8(255, 255, 255),
        ThemeColor::from_rgb8(51, 51, 51),
        ThemeColor::from_rgb8(252, 252, 252),
        ThemeColor::from_rgb8(241, 241, 241),
        ThemeColor::from_rgb8(235, 235, 235),
        ThemeColor::from_rgb8(102, 102, 102),
        ThemeColor::from_rgb8(153, 153, 153),
        ThemeColor::from_rgb8(179, 179, 179),
        ThemeColor::from_rgb8(204, 204, 204),
        ThemeColor::from_oklch(0.577, 0.245, 27.325),
        ThemeColor::from_rgb8(241, 241, 241),
        ThemeColor::from_rgb8(235, 235, 235),
        ThemeColor::from_oklch(0.708, 0.0, 0.0),
    )
}

fn dark_colors() -> ThemeColors {
    foundational_colors(
        ThemeMode::Dark,
        ThemeColor::from_rgb8(23, 23, 23),
        ThemeColor::from_rgb8(235, 235, 235),
        ThemeColor::from_rgb8(27, 27, 27),
        ThemeColor::from_rgb8(29, 29, 29),
        ThemeColor::from_rgb8(34, 34, 34),
        ThemeColor::from_rgb8(179, 179, 179),
        ThemeColor::from_rgb8(129, 129, 129),
        ThemeColor::from_rgb8(102, 102, 102),
        ThemeColor::from_rgb8(76, 76, 76),
        ThemeColor::from_oklch(0.704, 0.191, 22.216),
        ThemeColor::from_rgb8(29, 29, 29),
        ThemeColor::from_rgb8(34, 34, 34),
        ThemeColor::from_oklch(0.556, 0.0, 0.0),
    )
}

#[allow(clippy::too_many_arguments)]
fn foundational_colors(
    mode: ThemeMode,
    background: ThemeColor,
    foreground: ThemeColor,
    surface_secondary: ThemeColor,
    surface_tertiary: ThemeColor,
    surface_quaternary: ThemeColor,
    text_secondary: ThemeColor,
    text_muted: ThemeColor,
    text_placeholder: ThemeColor,
    text_unavailable: ThemeColor,
    destructive: ThemeColor,
    border: ThemeColor,
    input_border: ThemeColor,
    ring: ThemeColor,
) -> ThemeColors {
    let primary = ThemeColor::from_rgb8(62, 99, 221);
    let primary_foreground = ThemeColor::from_oklch(0.97, 0.014, 254.604);
    let destructive_foreground = ThemeColor::from_oklch(0.985, 0.0, 0.0);
    let profit = ThemeColor::from_oklch(0.683, 0.151, 160.997);
    let loss = ThemeColor::from_oklch(0.674, 0.215, 18.124);
    let volume_alpha = match mode {
        ThemeMode::Light => (0.34, 0.30),
        ThemeMode::Dark => (0.32, 0.28),
    };
    let (interactive_neutral_hover_bg, interactive_neutral_active_bg) = match mode {
        ThemeMode::Light => (
            ThemeColor::from_rgb8(0, 0, 0).with_alpha(0.039),
            ThemeColor::from_rgb8(0, 0, 0).with_alpha(0.078),
        ),
        ThemeMode::Dark => (
            ThemeColor::from_rgb8(255, 255, 255).with_alpha(0.059),
            ThemeColor::from_rgb8(255, 255, 255).with_alpha(0.102),
        ),
    };

    ThemeColors {
        background,
        foreground,
        surface_secondary,
        surface_tertiary,
        surface_quaternary,
        card: surface_secondary,
        card_foreground: foreground,
        popover: surface_secondary,
        popover_foreground: foreground,
        primary,
        primary_foreground,
        secondary: surface_tertiary,
        secondary_foreground: foreground,
        muted: surface_tertiary,
        muted_foreground: text_muted,
        text_secondary,
        text_muted,
        text_placeholder,
        text_unavailable,
        icon_color: text_secondary,
        accent: surface_tertiary,
        accent_foreground: foreground,
        destructive,
        destructive_foreground,
        border,
        input: input_border,
        input_surface: surface_secondary,
        ring,
        interactive_neutral_hover_bg,
        interactive_neutral_hover_fg: foreground,
        interactive_neutral_active_bg,
        interactive_neutral_active_fg: foreground,
        chart_palette: [
            ThemeColor::from_oklch(0.87, 0.0, 0.0),
            ThemeColor::from_oklch(0.556, 0.0, 0.0),
            ThemeColor::from_oklch(0.439, 0.0, 0.0),
            ThemeColor::from_oklch(0.371, 0.0, 0.0),
            ThemeColor::from_oklch(0.269, 0.0, 0.0),
        ],
        profit,
        loss,
        warning: ThemeColor::from_oklch(0.769, 0.165, 70.08),
        info: ThemeColor::from_oklch(0.555, 0.245, 266.681),
        feature: ThemeColor::from_oklch(0.541, 0.247, 293.009),
        chart_candle_up: profit,
        chart_candle_down: loss,
        chart_volume_up: profit.with_alpha(volume_alpha.0),
        chart_volume_down: loss.with_alpha(volume_alpha.1),
        chart_axis_text: match mode {
            ThemeMode::Light => ThemeColor::from_rgb8(10, 10, 10),
            ThemeMode::Dark => foreground,
        },
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
            Self::Sm => "radius_sm",
            Self::Default => "radius_default",
            Self::Full => "radius_full",
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
    fn application_palettes_use_twenty_neutrals_and_keep_axiusflow_semantics() {
        let light = AxiusflowTheme::light().colors;
        let dark = AxiusflowTheme::dark().colors;

        assert_eq!(light.background, ThemeColor::from_rgb8(255, 255, 255));
        assert_eq!(light.card, ThemeColor::from_rgb8(252, 252, 252));
        assert_eq!(light.input_surface, ThemeColor::from_rgb8(252, 252, 252));
        assert_eq!(light.foreground, ThemeColor::from_rgb8(51, 51, 51));
        assert_eq!(light.text_secondary, ThemeColor::from_rgb8(102, 102, 102));
        assert_eq!(light.text_muted, ThemeColor::from_rgb8(153, 153, 153));
        assert_eq!(light.text_placeholder, ThemeColor::from_rgb8(179, 179, 179));
        assert_eq!(light.text_unavailable, ThemeColor::from_rgb8(204, 204, 204));
        assert_eq!(dark.background, ThemeColor::from_rgb8(23, 23, 23));
        assert_eq!(dark.card, ThemeColor::from_rgb8(27, 27, 27));
        assert_eq!(dark.border, ThemeColor::from_rgb8(29, 29, 29));
        assert_eq!(dark.input, ThemeColor::from_rgb8(34, 34, 34));
        assert_eq!(dark.foreground, ThemeColor::from_rgb8(235, 235, 235));
        assert_eq!(dark.text_secondary, ThemeColor::from_rgb8(179, 179, 179));
        assert_eq!(dark.text_muted, ThemeColor::from_rgb8(129, 129, 129));
        assert_eq!(dark.text_placeholder, ThemeColor::from_rgb8(102, 102, 102));
        assert_eq!(dark.text_unavailable, ThemeColor::from_rgb8(76, 76, 76));
        assert_eq!(dark.muted, ThemeColor::from_rgb8(29, 29, 29));
        assert_eq!(dark.accent, ThemeColor::from_rgb8(29, 29, 29));
        assert_eq!(dark.icon_color, dark.text_secondary);
        assert_eq!(dark.chart_axis_text, dark.foreground);
        assert_eq!(light.primary, ThemeColor::from_rgb8(62, 99, 221));
        assert_eq!(dark.primary, ThemeColor::from_rgb8(62, 99, 221));
        assert_eq!(dark.profit, ThemeColor::from_oklch(0.683, 0.151, 160.997));
        assert_eq!(dark.loss, ThemeColor::from_oklch(0.674, 0.215, 18.124));
        assert_eq!(light.popover, light.card);
        assert_eq!(dark.popover, dark.card);
        assert_eq!(light.input_surface, light.card);
        assert_eq!(dark.input_surface, dark.card);
        assert_eq!(light.accent, light.muted);
        assert_eq!(dark.accent, dark.muted);
        assert_eq!(dark.chart_candle_up, dark.profit);
        assert_eq!(dark.chart_candle_down, dark.loss);
        assert!((light.chart_volume_up.alpha() - 0.34).abs() < f32::EPSILON);
        assert!((dark.chart_volume_down.alpha() - 0.28).abs() < f32::EPSILON);

        let light_tokens = AxiusflowTheme::light().color_tokens();
        let dark_tokens = AxiusflowTheme::dark().color_tokens();
        assert_eq!(
            token_source(&light_tokens, "background"),
            "color(display-p3 1 1 1)"
        );
        assert_eq!(
            token_source(&dark_tokens, "background"),
            "color(display-p3 0.09 0.09 0.09)"
        );
        assert_eq!(
            token_source(&dark_tokens, "card"),
            "var(--surface_secondary)"
        );
        assert_eq!(
            token_source(&dark_tokens, "foreground"),
            "color(display-p3 0.922 0.922 0.922)"
        );
        assert_eq!(
            token_source(&dark_tokens, "text_muted"),
            "color(display-p3 0.506 0.506 0.506)"
        );
        assert_eq!(
            token_source(&dark_tokens, "muted"),
            "var(--surface_tertiary)"
        );
        assert_eq!(token_source(&dark_tokens, "primary"), "#3e63dd");
        assert_eq!(
            token_source(&dark_tokens, "input_surface"),
            "color(display-p3 0.106 0.106 0.106)"
        );
        assert_eq!(
            token_source(&dark_tokens, "interactive_neutral_hover_bg"),
            "color(display-p3 1 1 1 / 0.059)"
        );
        assert_eq!(
            token_source(&dark_tokens, "chart_axis_text"),
            "var(--foreground)"
        );
        assert_eq!(token_source(&dark_tokens, "chart_1"), "oklch(0.87 0 0)");
    }

    #[test]
    fn radius_and_header_dimensions_match_the_platform_contract() {
        assert_eq!(RadiusToken::Sm.logical_pixels(), 4);
        assert_eq!(RadiusToken::Default.logical_pixels(), 6);
        assert_eq!(RadiusToken::Full.logical_pixels(), 999);
        assert_eq!(RadiusToken::Sm.css_custom_property(), "--radius_sm");
        let theme = AxiusflowTheme::dark();
        assert_eq!(
            theme.dimensions.app_header_height.css_custom_property(),
            "--app_header_height"
        );
        let candle_up = theme
            .color_tokens()
            .into_iter()
            .find(|token| token.canonical_identifier == "chart_candle_up")
            .expect("chart candle token exists");
        assert_eq!(candle_up.css_custom_property(), "--chart_candle_up");
    }

    #[test]
    fn css_manifest_contains_every_rust_color_token_and_mode_value() {
        let css = include_str!("../axiusflow_theme.css");

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
    }
}
