//! Chart colors projected from the platform theme. The default Aeris preset leaves Aeris
//! Charts' own theme colors untouched; every other preset pins the chart's theme-following
//! colors to its platform tokens, so the engine never knows presets exist.

use super::export::chart_color;
use super::{ChartAppearanceSettings, chart_color_hex};
use aeris_charts_engine::{AppearanceColor, ChartTheme, FinancialThemeColors};
use aeris_design_system::{AerisTheme, ThemeColor, ThemeMode, ThemePreset};

/// The Aeris Charts theme for a platform color mode.
pub(super) const fn chart_theme(mode: ThemeMode) -> ChartTheme {
    match mode {
        ThemeMode::Light => ChartTheme::Light,
        ThemeMode::Dark => ChartTheme::Dark,
    }
}

/// `color` when pinned, else the color its theme role paints.
pub(super) fn effective_color(color: &AppearanceColor, theme_color: &str) -> String {
    match color {
        AppearanceColor::Theme => theme_color.to_owned(),
        AppearanceColor::Custom(color) => color.clone(),
    }
}

/// Durable axis colors. `Theme` follows the chart: the axis border paints the theme border
/// and the price-axis labels paint the chart text color.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChartAxisAppearance {
    /// The price- and time-axis border lines, which pane separators also follow.
    pub border_color: AppearanceColor,
    /// The price-axis labels. Time-axis labels keep the chart text color.
    pub text_color: AppearanceColor,
}

impl Default for ChartAxisAppearance {
    fn default() -> Self {
        Self {
            border_color: AppearanceColor::Theme,
            text_color: AppearanceColor::Theme,
        }
    }
}

/// The colors theme-following chart appearance fields paint under the chart's platform theme,
/// as CSS colors for settings swatches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChartThemeDefaults {
    pub background: String,
    pub text: String,
    pub grid: String,
    pub crosshair: String,
    pub bullish: String,
    pub bearish: String,
    pub axis_border: String,
}

impl ChartThemeDefaults {
    fn engine(theme: ChartTheme) -> Self {
        let colors = FinancialThemeColors::for_theme(theme);
        Self {
            background: colors.background.to_owned(),
            text: colors.text.to_owned(),
            grid: colors.grid.to_owned(),
            crosshair: colors.crosshair.to_owned(),
            bullish: colors.bullish.to_owned(),
            bearish: colors.bearish.to_owned(),
            // Aeris Charts paints axis borders with its border token, the grid's default.
            axis_border: colors.grid.to_owned(),
        }
    }
}

/// Chart colors of a non-default platform preset, derived from its tokens the way Aeris
/// Charts derives its own theme from the default Aeris tokens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PresetPalette {
    defaults: ChartThemeDefaults,
    muted_text: String,
    separator_hover: String,
}

impl PresetPalette {
    /// `None` for the Aeris preset, whose chart colors Aeris Charts owns.
    pub(super) fn for_platform(theme: &AerisTheme) -> Option<Self> {
        if theme.preset == ThemePreset::Aeris {
            return None;
        }
        let colors = theme.colors;
        let opaque = |color: ThemeColor| css(color.over(colors.surface));
        let border = opaque(colors.border);
        Some(Self {
            defaults: ChartThemeDefaults {
                background: css(colors.surface),
                text: css(colors.text_default),
                grid: border.clone(),
                // Aeris Charts' crosshair is a translucent ink over the canvas.
                crosshair: css(colors.active_bg),
                bullish: css(colors.bullish),
                bearish: css(colors.bearish),
                axis_border: border,
            },
            muted_text: opaque(colors.text_secondary),
            separator_hover: opaque(colors.hover_bg),
        })
    }

    pub(super) const fn defaults(&self) -> &ChartThemeDefaults {
        &self.defaults
    }

    /// `appearance` with every theme-following canvas color and candle body pinned to this
    /// palette. Wicks and borders that follow their body keep following it.
    pub(super) fn pin(&self, appearance: &ChartAppearanceSettings) -> ChartAppearanceSettings {
        let pinned = |color: &AppearanceColor, theme_color: &str| {
            AppearanceColor::Custom(effective_color(color, theme_color))
        };
        let defaults = &self.defaults;
        ChartAppearanceSettings {
            background_color: pinned(&appearance.background_color, &defaults.background),
            text_color: pinned(&appearance.text_color, &defaults.text),
            grid_color: pinned(&appearance.grid_color, &defaults.grid),
            crosshair_color: pinned(&appearance.crosshair_color, &defaults.crosshair),
            up_color: pinned(&appearance.up_color, &defaults.bullish),
            down_color: pinned(&appearance.down_color, &defaults.bearish),
            ..appearance.clone()
        }
    }

    /// The theme-owned layout colors the typed appearance does not carry.
    pub(super) fn layout_patch(&self) -> serde_json::Value {
        serde_json::json!({
            "mutedTextColor": self.muted_text,
            "bullishColor": self.defaults.bullish,
            "bearishColor": self.defaults.bearish,
            "panes": {
                "separatorColor": self.defaults.axis_border,
                "separatorHoverColor": self.separator_hover,
            },
        })
    }
}

/// The defaults theme-following fields paint: the preset palette's, else Aeris Charts' own.
pub(super) fn theme_defaults(
    palette: Option<&PresetPalette>,
    theme: ChartTheme,
) -> ChartThemeDefaults {
    palette.map_or_else(
        || ChartThemeDefaults::engine(theme),
        |palette| palette.defaults().clone(),
    )
}

fn css(color: ThemeColor) -> String {
    chart_color_hex(chart_color(color))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_aeris_preset_keeps_the_engine_theme() {
        assert_eq!(PresetPalette::for_platform(&AerisTheme::dark()), None);
        let defaults = theme_defaults(None, ChartTheme::Light);
        assert_eq!(defaults, ChartThemeDefaults::engine(ChartTheme::Light));
        assert_eq!(defaults.axis_border, defaults.grid);
    }

    #[test]
    fn a_preset_pins_only_theme_following_colors_to_its_tokens() {
        let theme = AerisTheme::resolve(ThemePreset::Dracula, ThemeMode::Dark);
        let palette = PresetPalette::for_platform(&theme).expect("Dracula is a preset");
        assert_eq!(palette.defaults().background, css(theme.colors.surface));
        assert_eq!(palette.defaults().bullish, css(theme.colors.bullish));
        let custom = AppearanceColor::Custom("#123456".into());
        let appearance = ChartAppearanceSettings {
            grid_color: custom.clone(),
            ..ChartAppearanceSettings::default()
        };
        let pinned = palette.pin(&appearance);
        assert_eq!(pinned.grid_color, custom);
        assert_eq!(
            pinned.background_color,
            AppearanceColor::Custom(palette.defaults().background.clone())
        );
        assert_eq!(
            pinned.down_color,
            AppearanceColor::Custom(palette.defaults().bearish.clone())
        );
        assert_eq!(pinned.wick_up_color, AppearanceColor::Theme);
    }
}
