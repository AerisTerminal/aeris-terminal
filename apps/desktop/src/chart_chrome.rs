use aeris_chart_integration::ChartType;
use aeris_contracts::InstrumentSearchCategories;
use aeris_design_system::RadiusToken;
use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
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
    VolumeProfile,
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
            Self::VolumeProfile => "volume_profile",
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

pub const INDICATOR_SPECS: [IndicatorSpec; 12] = [
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
        kind: IndicatorKind::VolumeProfile,
        label: "Volume Profile (Visible Range)",
        parameters: IndicatorParameters::None,
        parameter_description: "Rows 48 - Value area 70% - POC",
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

/// An instrument category the symbol menu can include or exclude from search.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolSearchCategory {
    Futures,
    Equities,
}

impl SymbolSearchCategory {
    pub const ALL: [Self; 2] = [Self::Futures, Self::Equities];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Futures => "Futures",
            Self::Equities => "Stocks",
        }
    }

    #[must_use]
    pub const fn included(self, categories: InstrumentSearchCategories) -> bool {
        match self {
            Self::Futures => categories.futures,
            Self::Equities => categories.equities,
        }
    }
}

/// Durable shell chrome that follows the user across charts and workspaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChartChromePreferences {
    pub indicator_name_labels_visible: bool,
    pub indicator_value_labels_visible: bool,
    pub indicator_price_lines_visible: bool,
    pub chart_type: ChartType,
    /// Instrument categories the symbol menu searches; at least one is always included.
    pub symbol_search_categories: InstrumentSearchCategories,
}

impl Default for ChartChromePreferences {
    fn default() -> Self {
        Self {
            indicator_name_labels_visible: true,
            indicator_value_labels_visible: true,
            indicator_price_lines_visible: true,
            chart_type: ChartType::Candles,
            symbol_search_categories: InstrumentSearchCategories::ALL,
        }
    }
}

static CHART_CHROME_FILE_LOCK: Mutex<()> = Mutex::new(());
static CHART_CHROME_SAVE_STATE: Mutex<ChartChromeSaveState> = Mutex::new(ChartChromeSaveState {
    worker_active: false,
    latest: None,
    latest_requested_generation: 0,
    completed: None,
});

#[derive(Default)]
struct ChartChromeSaveState {
    worker_active: bool,
    latest: Option<(u64, ChartChromePreferences)>,
    latest_requested_generation: u64,
    completed: Option<ChartChromeSaveCompletion>,
}

#[derive(Clone)]
struct ChartChromeSaveCompletion {
    generation: u64,
    result: Result<(), String>,
}

impl ChartChromeSaveState {
    fn request(&mut self, preferences: ChartChromePreferences) -> Result<bool, String> {
        let generation = self
            .latest_requested_generation
            .checked_add(1)
            .ok_or_else(|| {
                "desktop chart chrome persistence generation is exhausted".to_string()
            })?;
        self.latest_requested_generation = generation;
        self.latest = Some((generation, preferences));
        if self.worker_active {
            Ok(false)
        } else {
            self.worker_active = true;
            Ok(true)
        }
    }

    fn take_next(&mut self) -> Option<(u64, ChartChromePreferences)> {
        self.latest.take()
    }

    fn finish(&mut self) {
        self.worker_active = false;
    }

    fn fail_latest(&mut self, error: String) {
        let generation = self.latest_requested_generation;
        self.latest = None;
        if generation != 0 {
            self.completed = Some(ChartChromeSaveCompletion {
                generation,
                result: Err(error),
            });
        }
        self.finish();
    }
}

pub(crate) struct ChartChromeShutdownWait {
    expected_generation: Result<u64, String>,
}

impl ChartChromeShutdownWait {
    pub(crate) fn wait(self, timeout: Duration) -> Result<u64, String> {
        let expected_generation = self.expected_generation?;
        wait_for_chart_chrome_generation(&CHART_CHROME_SAVE_STATE, expected_generation, timeout)
    }
}

pub(crate) fn chart_chrome_shutdown_generation_is_current(generation: u64) -> bool {
    CHART_CHROME_SAVE_STATE
        .lock()
        .is_ok_and(|state| chart_chrome_generation_is_current(&state, generation))
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
        } else if let Some(value) = line.strip_prefix("symbol_search_futures=") {
            preferences.symbol_search_categories.futures = parse_chrome_flag(value);
        } else if let Some(value) = line.strip_prefix("symbol_search_equities=") {
            preferences.symbol_search_categories.equities = parse_chrome_flag(value);
        }
    }
    // A search that excludes every category would never return anything.
    if !preferences.symbol_search_categories.futures
        && !preferences.symbol_search_categories.equities
    {
        preferences.symbol_search_categories = InstrumentSearchCategories::ALL;
    }
    preferences
}

#[must_use]
pub fn encode_chart_chrome_preferences(preferences: ChartChromePreferences) -> String {
    format!(
        "indicator_name_labels={}\nindicator_value_labels={}\nindicator_price_lines={}\nchart_type={}\nsymbol_search_futures={}\nsymbol_search_equities={}\n",
        u8::from(preferences.indicator_name_labels_visible),
        u8::from(preferences.indicator_value_labels_visible),
        u8::from(preferences.indicator_price_lines_visible),
        preferences.chart_type.identifier(),
        u8::from(preferences.symbol_search_categories.futures),
        u8::from(preferences.symbol_search_categories.equities),
    )
}

#[must_use]
pub fn chart_chrome_state_path() -> Option<PathBuf> {
    aeris_platform_runtime::native_data_root()
        .ok()
        .map(|root| root.join("desktop").join("chart-chrome"))
}

#[must_use]
pub fn load_chart_chrome_preferences() -> ChartChromePreferences {
    chart_chrome_state_path().map_or_else(ChartChromePreferences::default, |path| {
        load_chart_chrome_preferences_from(&path)
    })
}

fn load_chart_chrome_preferences_from(path: &Path) -> ChartChromePreferences {
    fs::read_to_string(path)
        .or_else(|_| fs::read_to_string(chart_chrome_backup_path(path)))
        .ok()
        .as_deref()
        .map(parse_chart_chrome_preferences)
        .unwrap_or_default()
}

/// Coalesces one UI-owned preference snapshot. Returns whether the caller must
/// start the single background saver. The lock protects only this tiny in-memory
/// slot; filesystem work remains entirely off the GPUI thread.
pub(crate) fn request_chart_chrome_preferences_save(
    preferences: ChartChromePreferences,
) -> Result<bool, String> {
    let mut state = CHART_CHROME_SAVE_STATE
        .lock()
        .map_err(|_| "desktop chart chrome persistence is unavailable".to_string())?;
    state.request(preferences)
}

pub(crate) fn chart_chrome_shutdown_wait() -> ChartChromeShutdownWait {
    ChartChromeShutdownWait {
        expected_generation: CHART_CHROME_SAVE_STATE
            .lock()
            .map(|state| state.latest_requested_generation)
            .map_err(|_| "desktop chart chrome persistence is unavailable".to_string()),
    }
}

/// Drains coalesced chart-chrome persistence until no newer UI snapshot remains.
///
/// # Errors
///
/// Returns an error when the per-user desktop directory or persistence worker
/// state is unavailable, or when the latest pending snapshot cannot be saved.
pub(crate) fn run_chart_chrome_preferences_save_worker() -> Result<(), String> {
    let Some(path) = chart_chrome_state_path() else {
        let error = "desktop chart chrome directory is unavailable".to_string();
        if let Ok(mut state) = CHART_CHROME_SAVE_STATE.lock() {
            state.fail_latest(error.clone());
        }
        return Err(error);
    };
    run_chart_chrome_preferences_save_worker_to(&path, &CHART_CHROME_SAVE_STATE)
}

fn run_chart_chrome_preferences_save_worker_to(
    path: &Path,
    state: &Mutex<ChartChromeSaveState>,
) -> Result<(), String> {
    loop {
        let (generation, next) = {
            let mut state = state
                .lock()
                .map_err(|_| "desktop chart chrome persistence is unavailable".to_string())?;
            let Some(next) = state.take_next() else {
                state.finish();
                return Ok(());
            };
            next
        };
        let result = save_chart_chrome_preferences_to(path, next);
        let mut state = state
            .lock()
            .map_err(|_| "desktop chart chrome persistence is unavailable".to_string())?;
        state.completed = Some(ChartChromeSaveCompletion {
            generation,
            result: result.clone(),
        });
        if let Err(error) = result
            && state.latest.is_none()
        {
            state.finish();
            return Err(error);
        }
    }
}

fn wait_for_chart_chrome_generation(
    state: &Mutex<ChartChromeSaveState>,
    expected_generation: u64,
    timeout: Duration,
) -> Result<u64, String> {
    if expected_generation == 0 {
        return Ok(0);
    }
    let deadline = Instant::now() + timeout;
    loop {
        {
            let state = state
                .lock()
                .map_err(|_| "desktop chart chrome persistence is unavailable".to_string())?;
            if state.latest_requested_generation != expected_generation {
                return Err(
                    "chart preferences changed after shutdown persistence was claimed".to_string(),
                );
            }
            if let Some(completed) = state.completed.as_ref()
                && completed.generation == expected_generation
            {
                return completed.result.clone().map(|()| expected_generation);
            }
            if !state.worker_active && state.latest.is_none() {
                return Err(
                    "desktop chart chrome persistence stopped before the latest save".to_string(),
                );
            }
        }
        if Instant::now() >= deadline {
            return Err("desktop chart chrome persistence timed out during shutdown".to_string());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn chart_chrome_generation_is_current(state: &ChartChromeSaveState, generation: u64) -> bool {
    state.latest_requested_generation == generation
        && (generation == 0
            || state.completed.as_ref().is_some_and(|completed| {
                completed.generation == generation && completed.result.is_ok()
            }))
}

/// # Errors
///
/// Returns an error when the target file cannot be created or replaced.
pub fn save_chart_chrome_preferences_to(
    path: &Path,
    preferences: ChartChromePreferences,
) -> Result<(), String> {
    let _save_guard = CHART_CHROME_FILE_LOCK
        .lock()
        .map_err(|_| "desktop chart chrome persistence is unavailable".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|_| "desktop chart chrome directory could not be created".to_string())?;
    }
    let staging = chart_chrome_staging_path(path);
    let backup = chart_chrome_backup_path(path);
    let encoded = encode_chart_chrome_preferences(preferences);
    let mut staging_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&staging)
        .map_err(|_| "desktop chart chrome could not be written".to_string())?;
    staging_file
        .write_all(encoded.as_bytes())
        .and_then(|()| staging_file.sync_all())
        .map_err(|_| "desktop chart chrome could not be written".to_string())?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        sync_chart_chrome_directory(parent)?;
    }
    if path.exists() {
        remove_chart_chrome_file_if_present(&backup)?;
        fs::rename(path, &backup)
            .map_err(|_| "desktop chart chrome could not be replaced".to_string())?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            sync_chart_chrome_directory(parent)?;
        }
    }
    if fs::rename(&staging, path).is_err() {
        if !path.exists() && backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        return Err("desktop chart chrome could not be published".to_string());
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        sync_chart_chrome_directory(parent)?;
    }
    remove_chart_chrome_file_if_present(&backup)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        sync_chart_chrome_directory(parent)?;
    }
    Ok(())
}

fn chart_chrome_staging_path(path: &Path) -> PathBuf {
    path.with_extension("tmp")
}

fn chart_chrome_backup_path(path: &Path) -> PathBuf {
    path.with_extension("bak")
}

fn remove_chart_chrome_file_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("desktop chart chrome could not be replaced".to_string()),
    }
}

#[cfg(unix)]
fn sync_chart_chrome_directory(path: &Path) -> Result<(), String> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "desktop chart chrome directory could not be synchronized".to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        ChartChromePreferences, ChartChromeSaveState, INDICATOR_SPECS, IndicatorKind,
        IndicatorLocation, IndicatorParameters, InstrumentSearchCategories,
        chart_chrome_backup_path, chart_chrome_staging_path, encode_chart_chrome_preferences,
        filter_indicator_specs, load_chart_chrome_preferences_from, parse_chart_chrome_preferences,
        run_chart_chrome_preferences_save_worker_to, save_chart_chrome_preferences_to,
        wait_for_chart_chrome_generation,
    };
    use aeris_chart_integration::ChartType;
    use std::sync::Mutex;
    use std::time::Duration;

    fn temporary_chart_chrome_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "aeris-chart-chrome-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ))
    }

    fn remove_chart_chrome_test_files(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(chart_chrome_staging_path(path));
        let _ = std::fs::remove_file(chart_chrome_backup_path(path));
    }

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
                IndicatorKind::VolumeProfile,
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
            INDICATOR_SPECS[..7]
                .iter()
                .all(|spec| spec.location == IndicatorLocation::MainChartOverlay)
        );
        assert!(
            INDICATOR_SPECS[8..]
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
            symbol_search_categories: InstrumentSearchCategories {
                futures: true,
                equities: false,
            },
        };
        assert_eq!(
            encode_chart_chrome_preferences(hidden),
            "indicator_name_labels=0\nindicator_value_labels=1\nindicator_price_lines=0\nchart_type=bars\nsymbol_search_futures=1\nsymbol_search_equities=0\n"
        );
        assert_eq!(
            defaults.symbol_search_categories,
            InstrumentSearchCategories::ALL
        );
        assert_eq!(
            parse_chart_chrome_preferences("symbol_search_futures=0\nsymbol_search_equities=0\n")
                .symbol_search_categories,
            InstrumentSearchCategories::ALL,
            "excluding every category falls back to all"
        );
        let path = temporary_chart_chrome_path("round-trip");
        save_chart_chrome_preferences_to(&path, hidden).expect("temp chrome file writes");
        let restored = parse_chart_chrome_preferences(
            &std::fs::read_to_string(&path).expect("temp chrome file reads"),
        );
        remove_chart_chrome_test_files(&path);
        assert_eq!(restored, hidden);
        let marked_line = ChartChromePreferences {
            chart_type: ChartType::LineWithMarkers,
            ..hidden
        };
        assert_eq!(
            parse_chart_chrome_preferences(&encode_chart_chrome_preferences(marked_line)),
            marked_line
        );
    }

    #[test]
    fn chart_chrome_load_recovers_committed_preferences_from_backup_boundary() {
        let path = temporary_chart_chrome_path("backup-recovery");
        let backup = chart_chrome_backup_path(&path);
        let staging = chart_chrome_staging_path(&path);
        let committed = ChartChromePreferences {
            indicator_name_labels_visible: false,
            indicator_value_labels_visible: true,
            indicator_price_lines_visible: false,
            chart_type: ChartType::Bars,
            symbol_search_categories: InstrumentSearchCategories::ALL,
        };
        let pending = ChartChromePreferences {
            indicator_name_labels_visible: true,
            indicator_value_labels_visible: false,
            indicator_price_lines_visible: true,
            chart_type: ChartType::Line,
            symbol_search_categories: InstrumentSearchCategories::ALL,
        };
        save_chart_chrome_preferences_to(&path, committed).expect("committed preferences save");

        std::fs::rename(&path, &backup).expect("crash boundary moves current to backup");
        std::fs::write(&staging, encode_chart_chrome_preferences(pending))
            .expect("pending staging writes");

        assert_eq!(
            load_chart_chrome_preferences_from(&path),
            committed,
            "a crash after current-to-backup must retain the last committed preferences"
        );
        remove_chart_chrome_test_files(&path);
    }

    #[test]
    fn chart_chrome_save_queue_coalesces_to_latest_ui_snapshot() {
        let path = temporary_chart_chrome_path("coalesced");
        let first = ChartChromePreferences {
            indicator_name_labels_visible: false,
            indicator_value_labels_visible: false,
            indicator_price_lines_visible: true,
            chart_type: ChartType::Line,
            symbol_search_categories: InstrumentSearchCategories::ALL,
        };
        let second = ChartChromePreferences {
            indicator_name_labels_visible: true,
            indicator_value_labels_visible: true,
            indicator_price_lines_visible: false,
            chart_type: ChartType::Bars,
            symbol_search_categories: InstrumentSearchCategories::ALL,
        };
        let state = Mutex::new(ChartChromeSaveState::default());
        let (_, inflight) = {
            let mut state = state.lock().expect("save state locks");
            assert!(
                state.request(first).expect("first request queues"),
                "the first request starts one worker"
            );
            state.take_next().expect("worker claims first request")
        };
        let expected_generation = {
            let mut state = state.lock().expect("save state locks");
            assert!(
                !state.request(second).expect("second request queues"),
                "a newer request coalesces while the single saver is active"
            );
            state.latest_requested_generation
        };
        assert!(
            wait_for_chart_chrome_generation(&state, expected_generation, Duration::ZERO).is_err(),
            "shutdown cannot report durability before the newest queued snapshot completes"
        );
        save_chart_chrome_preferences_to(&path, inflight).expect("inflight save succeeds");
        run_chart_chrome_preferences_save_worker_to(&path, &state)
            .expect("worker drains the newer coalesced request");

        assert_eq!(load_chart_chrome_preferences_from(&path), second);
        assert_eq!(
            wait_for_chart_chrome_generation(&state, expected_generation, Duration::ZERO),
            Ok(expected_generation)
        );
        let state = state.lock().expect("save state locks");
        assert!(!state.worker_active);
        assert!(state.latest.is_none());
        assert!(!chart_chrome_staging_path(&path).exists());
        assert!(!chart_chrome_backup_path(&path).exists());
        drop(state);
        remove_chart_chrome_test_files(&path);
    }

    #[test]
    fn chart_chrome_shutdown_wait_surfaces_failure_and_newer_requests() {
        let first = ChartChromePreferences {
            indicator_name_labels_visible: false,
            indicator_value_labels_visible: false,
            indicator_price_lines_visible: true,
            chart_type: ChartType::Line,
            symbol_search_categories: InstrumentSearchCategories::ALL,
        };
        let second = ChartChromePreferences {
            indicator_name_labels_visible: true,
            indicator_value_labels_visible: true,
            indicator_price_lines_visible: false,
            chart_type: ChartType::Bars,
            symbol_search_categories: InstrumentSearchCategories::ALL,
        };
        let state = Mutex::new(ChartChromeSaveState::default());
        let failed_generation = {
            let mut state = state.lock().expect("save state locks");
            assert!(state.request(first).expect("first request queues"));
            let generation = state.latest_requested_generation;
            state.fail_latest("chart storage unavailable".to_string());
            generation
        };

        assert_eq!(
            wait_for_chart_chrome_generation(&state, failed_generation, Duration::ZERO),
            Err("chart storage unavailable".to_string())
        );

        let claimed_generation = {
            let mut state = state.lock().expect("save state locks");
            assert!(state.request(first).expect("retry request queues"));
            state.latest_requested_generation
        };
        {
            let mut state = state.lock().expect("save state locks");
            assert!(
                !state.request(second).expect("newer request coalesces"),
                "the existing worker remains the single persistence owner"
            );
        }
        assert_eq!(
            wait_for_chart_chrome_generation(&state, claimed_generation, Duration::ZERO),
            Err("chart preferences changed after shutdown persistence was claimed".to_string())
        );
    }
}
