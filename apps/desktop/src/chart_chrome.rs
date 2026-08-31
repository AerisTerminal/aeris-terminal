use axiusflow_chart_integration::ChartType;
use axiusflow_design_system::RadiusToken;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const CHART_CHROME_HEIGHT: f32 = 44.0;
pub const CHART_CONTROL_SIZE: f32 = 32.0;
pub const HEADER_CONTROL_CONTENT_SIZE: f32 = 24.0;
pub const HEADER_ICON_SIZE: f32 = HEADER_CONTROL_CONTENT_SIZE * 0.75;
pub const CHART_CONTROL_RADIUS: RadiusToken = RadiusToken::Sm;
pub const CHART_SURFACE_RADIUS: RadiusToken = RadiusToken::Default;
pub const SYMBOL_TRIGGER_RADIUS: RadiusToken = RadiusToken::Full;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndicatorKind {
    Sma,
    Ema,
    EmaRibbon,
    Wma,
    BollingerBands,
    Vwap,
    Volume,
    Rsi,
    Macd,
    Stochastic,
    Atr,
}

impl IndicatorKind {
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::Sma => "sma",
            Self::Ema => "ema",
            Self::EmaRibbon => "ema_ribbon",
            Self::Wma => "wma",
            Self::BollingerBands => "bollinger",
            Self::Vwap => "vwap",
            Self::Volume => "volume",
            Self::Rsi => "rsi",
            Self::Macd => "macd",
            Self::Stochastic => "stochastic",
            Self::Atr => "atr",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndicatorParameters {
    None,
    Period {
        period: u16,
    },
    BollingerBands {
        period: u16,
        deviation: u16,
    },
    Macd {
        fast_period: u16,
        slow_period: u16,
        signal_period: u16,
    },
    Stochastic {
        k_period: u16,
        d_period: u16,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndicatorLocation {
    MainChartOverlay,
    VolumePane,
    OscillatorPane,
}

impl IndicatorLocation {
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::MainChartOverlay => "Overlay on the main chart",
            Self::VolumePane => "Volume histogram pane",
            Self::OscillatorPane => "Separate oscillator pane",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndicatorSpec {
    pub kind: IndicatorKind,
    pub label: &'static str,
    pub parameters: IndicatorParameters,
    pub parameter_description: &'static str,
    pub location: IndicatorLocation,
}

impl IndicatorSpec {
    #[must_use]
    pub const fn location_description(self) -> &'static str {
        self.location.description()
    }

    fn matches(self, normalized_query: &str) -> bool {
        [
            self.kind.identifier(),
            self.label,
            self.parameter_description,
            self.location_description(),
        ]
        .into_iter()
        .any(|value| value.to_ascii_lowercase().contains(normalized_query))
    }
}

pub const INDICATOR_SPECS: [IndicatorSpec; 11] = [
    IndicatorSpec {
        kind: IndicatorKind::Sma,
        label: "Moving Average",
        parameters: IndicatorParameters::Period { period: 20 },
        parameter_description: "Period 20",
        location: IndicatorLocation::MainChartOverlay,
    },
    IndicatorSpec {
        kind: IndicatorKind::Ema,
        label: "Moving Average Exponential",
        parameters: IndicatorParameters::Period { period: 20 },
        parameter_description: "Period 20",
        location: IndicatorLocation::MainChartOverlay,
    },
    IndicatorSpec {
        kind: IndicatorKind::EmaRibbon,
        label: "EMA Ribbon",
        parameters: IndicatorParameters::None,
        parameter_description: "Periods 5 - 10 - 20 - 50 - 200",
        location: IndicatorLocation::MainChartOverlay,
    },
    IndicatorSpec {
        kind: IndicatorKind::Wma,
        label: "Weighted Moving Average",
        parameters: IndicatorParameters::Period { period: 20 },
        parameter_description: "Period 20",
        location: IndicatorLocation::MainChartOverlay,
    },
    IndicatorSpec {
        kind: IndicatorKind::BollingerBands,
        label: "Bollinger Bands",
        parameters: IndicatorParameters::BollingerBands {
            period: 20,
            deviation: 2,
        },
        parameter_description: "Period 20 - Deviation 2",
        location: IndicatorLocation::MainChartOverlay,
    },
    IndicatorSpec {
        kind: IndicatorKind::Vwap,
        label: "Volume Weighted Average Price",
        parameters: IndicatorParameters::None,
        parameter_description: "Session volume weighted price",
        location: IndicatorLocation::MainChartOverlay,
    },
    IndicatorSpec {
        kind: IndicatorKind::Volume,
        label: "Volume",
        parameters: IndicatorParameters::None,
        parameter_description: "Bar volume",
        location: IndicatorLocation::VolumePane,
    },
    IndicatorSpec {
        kind: IndicatorKind::Rsi,
        label: "Relative Strength Index",
        parameters: IndicatorParameters::Period { period: 14 },
        parameter_description: "Period 14",
        location: IndicatorLocation::OscillatorPane,
    },
    IndicatorSpec {
        kind: IndicatorKind::Macd,
        label: "MACD",
        parameters: IndicatorParameters::Macd {
            fast_period: 12,
            slow_period: 26,
            signal_period: 9,
        },
        parameter_description: "Fast 12 - Slow 26 - Signal 9",
        location: IndicatorLocation::OscillatorPane,
    },
    IndicatorSpec {
        kind: IndicatorKind::Stochastic,
        label: "Stochastic",
        parameters: IndicatorParameters::Stochastic {
            k_period: 14,
            d_period: 3,
        },
        parameter_description: "%K 14 - %D 3",
        location: IndicatorLocation::OscillatorPane,
    },
    IndicatorSpec {
        kind: IndicatorKind::Atr,
        label: "Average True Range",
        parameters: IndicatorParameters::Period { period: 14 },
        parameter_description: "Period 14",
        location: IndicatorLocation::OscillatorPane,
    },
];

#[must_use]
pub fn filter_indicator_specs(query: &str) -> Vec<&'static IndicatorSpec> {
    let normalized_query = query.trim().to_ascii_lowercase();
    INDICATOR_SPECS
        .iter()
        .filter(|spec| spec.matches(&normalized_query))
        .collect()
}

/// Durable shell chrome that follows the user across charts and workspaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChartChromePreferences {
    pub indicator_name_labels_visible: bool,
    pub indicator_value_labels_visible: bool,
    pub indicator_price_lines_visible: bool,
    pub chart_type: ChartType,
}

impl Default for ChartChromePreferences {
    fn default() -> Self {
        Self {
            indicator_name_labels_visible: true,
            indicator_value_labels_visible: true,
            indicator_price_lines_visible: true,
            chart_type: ChartType::Candles,
        }
    }
}

fn parse_chrome_flag(value: &str) -> bool {
    value.trim() != "0"
}

#[must_use]
pub fn parse_chart_chrome_preferences(contents: &str) -> ChartChromePreferences {
    let mut preferences = ChartChromePreferences::default();
    let mut names_from_split_key = false;
    let mut values_from_split_key = false;
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("indicator_name_labels=") {
            preferences.indicator_name_labels_visible = parse_chrome_flag(value);
            names_from_split_key = true;
        } else if let Some(value) = line.strip_prefix("indicator_value_labels=") {
            preferences.indicator_value_labels_visible = parse_chrome_flag(value);
            values_from_split_key = true;
        } else if let Some(value) = line.strip_prefix("indicator_labels=") {
            let visible = parse_chrome_flag(value);
            if !names_from_split_key {
                preferences.indicator_name_labels_visible = visible;
            }
            if !values_from_split_key {
                preferences.indicator_value_labels_visible = visible;
            }
        } else if let Some(value) = line.strip_prefix("indicator_price_lines=") {
            preferences.indicator_price_lines_visible = parse_chrome_flag(value);
        } else if let Some(value) = line.strip_prefix("chart_type=")
            && let Some(chart_type) = ChartType::from_identifier(value)
        {
            preferences.chart_type = chart_type;
        }
    }
    preferences
}

#[must_use]
pub fn encode_chart_chrome_preferences(preferences: ChartChromePreferences) -> String {
    format!(
        "indicator_name_labels={}\nindicator_value_labels={}\nindicator_price_lines={}\nchart_type={}\n",
        u8::from(preferences.indicator_name_labels_visible),
        u8::from(preferences.indicator_value_labels_visible),
        u8::from(preferences.indicator_price_lines_visible),
        preferences.chart_type.identifier()
    )
}

#[must_use]
pub fn chart_chrome_state_path() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("LOCALAPPDATA") {
        return Some(
            PathBuf::from(root)
                .join("Axiusflow")
                .join("desktop")
                .join("chart-chrome"),
        );
    }
    if let Some(root) = std::env::var_os("XDG_DATA_HOME") {
        return Some(
            PathBuf::from(root)
                .join("axiusflow")
                .join("desktop")
                .join("chart-chrome"),
        );
    }
    std::env::var_os("HOME").map(|home| {
        let home = PathBuf::from(home);
        if cfg!(target_os = "macos") {
            home.join("Library")
                .join("Application Support")
                .join("Axiusflow")
                .join("desktop")
                .join("chart-chrome")
        } else {
            home.join(".local")
                .join("share")
                .join("axiusflow")
                .join("desktop")
                .join("chart-chrome")
        }
    })
}

#[must_use]
pub fn load_chart_chrome_preferences() -> ChartChromePreferences {
    chart_chrome_state_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .as_deref()
        .map(parse_chart_chrome_preferences)
        .unwrap_or_default()
}

/// # Errors
///
/// Returns an error when the per-user desktop directory cannot be created or replaced.
pub fn save_chart_chrome_preferences(preferences: ChartChromePreferences) -> Result<(), String> {
    let path = chart_chrome_state_path()
        .ok_or_else(|| "desktop chart chrome directory is unavailable".to_string())?;
    save_chart_chrome_preferences_to(&path, preferences)
}

/// # Errors
///
/// Returns an error when the target file cannot be created or replaced.
pub fn save_chart_chrome_preferences_to(
    path: &Path,
    preferences: ChartChromePreferences,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|_| "desktop chart chrome directory could not be created".to_string())?;
    }
    let staging = path.with_extension("tmp");
    fs::write(
        &staging,
        encode_chart_chrome_preferences(preferences).as_bytes(),
    )
    .map_err(|_| "desktop chart chrome could not be written".to_string())?;
    if path.exists() {
        fs::remove_file(path)
            .map_err(|_| "desktop chart chrome could not be replaced".to_string())?;
    }
    fs::rename(&staging, path)
        .map_err(|_| "desktop chart chrome could not be published".to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        ChartChromePreferences, INDICATOR_SPECS, IndicatorKind, IndicatorLocation,
        IndicatorParameters, encode_chart_chrome_preferences, filter_indicator_specs,
        parse_chart_chrome_preferences, save_chart_chrome_preferences_to,
    };
    use axiusflow_chart_integration::ChartType;

    #[test]
    fn catalog_order_matches_the_platform_menu() {
        assert_eq!(
            INDICATOR_SPECS.map(|spec| spec.kind),
            [
                IndicatorKind::Sma,
                IndicatorKind::Ema,
                IndicatorKind::EmaRibbon,
                IndicatorKind::Wma,
                IndicatorKind::BollingerBands,
                IndicatorKind::Vwap,
                IndicatorKind::Volume,
                IndicatorKind::Rsi,
                IndicatorKind::Macd,
                IndicatorKind::Stochastic,
                IndicatorKind::Atr,
            ]
        );
    }

    #[test]
    fn catalog_preserves_supported_defaults_and_locations() {
        assert_eq!(
            INDICATOR_SPECS.map(|spec| spec.parameters),
            [
                IndicatorParameters::Period { period: 20 },
                IndicatorParameters::Period { period: 20 },
                IndicatorParameters::None,
                IndicatorParameters::Period { period: 20 },
                IndicatorParameters::BollingerBands {
                    period: 20,
                    deviation: 2,
                },
                IndicatorParameters::None,
                IndicatorParameters::None,
                IndicatorParameters::Period { period: 14 },
                IndicatorParameters::Macd {
                    fast_period: 12,
                    slow_period: 26,
                    signal_period: 9,
                },
                IndicatorParameters::Stochastic {
                    k_period: 14,
                    d_period: 3,
                },
                IndicatorParameters::Period { period: 14 },
            ]
        );
        assert!(
            INDICATOR_SPECS[..6]
                .iter()
                .all(|spec| spec.location == IndicatorLocation::MainChartOverlay)
        );
        assert!(
            INDICATOR_SPECS[7..]
                .iter()
                .all(|spec| spec.location == IndicatorLocation::OscillatorPane)
        );
    }

    #[test]
    fn filtering_is_trimmed_case_insensitive_and_ordered() {
        assert_eq!(
            filter_indicator_specs("  mAcD  ")
                .into_iter()
                .map(|spec| spec.kind)
                .collect::<Vec<_>>(),
            vec![IndicatorKind::Macd]
        );
        assert_eq!(
            filter_indicator_specs("oscillator")
                .into_iter()
                .map(|spec| spec.kind)
                .collect::<Vec<_>>(),
            vec![
                IndicatorKind::Rsi,
                IndicatorKind::Macd,
                IndicatorKind::Stochastic,
                IndicatorKind::Atr,
            ]
        );
        assert_eq!(filter_indicator_specs("").len(), INDICATOR_SPECS.len());
    }

    #[test]
    fn chart_chrome_preferences_round_trip_through_durable_file() {
        let defaults = parse_chart_chrome_preferences("");
        assert!(defaults.indicator_name_labels_visible);
        assert!(defaults.indicator_value_labels_visible);
        assert!(defaults.indicator_price_lines_visible);
        assert_eq!(defaults.chart_type, ChartType::Candles);
        let legacy = parse_chart_chrome_preferences("indicator_labels=0\n");
        assert!(!legacy.indicator_name_labels_visible);
        assert!(!legacy.indicator_value_labels_visible);
        assert!(legacy.indicator_price_lines_visible);
        assert_eq!(legacy.chart_type, ChartType::Candles);
        let mixed = parse_chart_chrome_preferences(
            "indicator_labels=0\nindicator_name_labels=1\nindicator_value_labels=0\nindicator_price_lines=0\nchart_type=line\n",
        );
        assert!(mixed.indicator_name_labels_visible);
        assert!(!mixed.indicator_value_labels_visible);
        assert!(!mixed.indicator_price_lines_visible);
        assert_eq!(mixed.chart_type, ChartType::Line);
        let hidden = ChartChromePreferences {
            indicator_name_labels_visible: false,
            indicator_value_labels_visible: true,
            indicator_price_lines_visible: false,
            chart_type: ChartType::Bars,
        };
        assert_eq!(
            encode_chart_chrome_preferences(hidden),
            "indicator_name_labels=0\nindicator_value_labels=1\nindicator_price_lines=0\nchart_type=bars\n"
        );
        let path = std::env::temp_dir().join(format!(
            "axiusflow-chart-chrome-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ));
        save_chart_chrome_preferences_to(&path, hidden).expect("temp chrome file writes");
        let restored = parse_chart_chrome_preferences(
            &std::fs::read_to_string(&path).expect("temp chrome file reads"),
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("tmp"));
        assert_eq!(restored, hidden);
    }
}
