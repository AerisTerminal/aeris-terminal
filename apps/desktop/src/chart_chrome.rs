use aeris_chart_integration::{
    ChartDrawingKind, ChartDrawingStamp, ChartIndicatorChrome, ChartType,
};
use aeris_contracts::InstrumentSearchCategories;
use aeris_observability::diagnostic;
use aeris_platform_runtime::{
    DurableCandidate, DurableContainer, DurableFormat, DurableNewerVersion, DurableRejection,
    DurableSource,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub const CHART_CHROME_HEIGHT: f32 = 44.0;
pub const CHART_CONTROL_SIZE: f32 = 32.0;
pub const HEADER_CONTROL_CONTENT_SIZE: f32 = 24.0;
pub const HEADER_ICON_SIZE: f32 = HEADER_CONTROL_CONTENT_SIZE * 0.75;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndicatorKind {
    Sma,
    Ema,
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

pub const INDICATOR_SPECS: [IndicatorSpec; 11] = [
    IndicatorSpec {
        kind: IndicatorKind::Sma,
        label: "Simple Moving Average",
        parameters: IndicatorParameters::Period { period: 20 },
        parameter_description: "Period 20",
        location: IndicatorLocation::MainChartOverlay,
    },
    IndicatorSpec {
        kind: IndicatorKind::Ema,
        label: "Exponential Moving Average",
        parameters: IndicatorParameters::Period { period: 20 },
        parameter_description: "Period 20",
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

/// One provider exchange code, kept inline so the chrome preferences stay `Copy`.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ExchangeCode {
    bytes: [u8; Self::MAXIMUM_BYTES],
    len: u8,
}

impl ExchangeCode {
    pub const MAXIMUM_BYTES: usize = 16;

    /// A non-empty code of at most [`Self::MAXIMUM_BYTES`] visible ASCII characters; longer or
    /// other codes are never offered as a search scope.
    #[must_use]
    pub fn new(code: &str) -> Option<Self> {
        let source = code.as_bytes();
        if source.is_empty()
            || source.len() > Self::MAXIMUM_BYTES
            || !source.iter().all(u8::is_ascii_graphic)
        {
            return None;
        }
        let mut bytes = [0; Self::MAXIMUM_BYTES];
        bytes[..source.len()].copy_from_slice(source);
        Some(Self {
            bytes,
            len: u8::try_from(source.len()).ok()?,
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or_default()
    }
}

impl std::fmt::Debug for ExchangeCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Which markets the symbol menu searches. Providers are named by their registered id.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SymbolSearchScope {
    /// Every available provider, each answering in its own section.
    #[default]
    All,
    Provider(&'static str),
    /// One exchange of one provider, as named by that provider's results.
    Exchange {
        provider: &'static str,
        exchange: ExchangeCode,
    },
}

impl SymbolSearchScope {
    #[must_use]
    pub const fn provider(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Provider(provider) | Self::Exchange { provider, .. } => Some(provider),
        }
    }

    #[must_use]
    pub const fn exchange(self) -> Option<ExchangeCode> {
        match self {
            Self::Exchange { exchange, .. } => Some(exchange),
            Self::All | Self::Provider(_) => None,
        }
    }

    fn encode(self) -> String {
        match self {
            Self::All => "all".to_string(),
            Self::Provider(provider) => format!("provider:{provider}"),
            Self::Exchange { provider, exchange } => {
                format!("exchange:{provider}:{}", exchange.as_str())
            }
        }
    }

    /// A saved scope naming a provider this build does not register falls back to all.
    fn parse(value: &str) -> Self {
        let registered = |id: &str| {
            aeris_market_runtime::built_in_provider_presentations()
                .iter()
                .find(|descriptor| descriptor.id == id)
                .map(|descriptor| descriptor.id)
        };
        let value = value.trim();
        if let Some(provider) = value.strip_prefix("provider:") {
            return registered(provider).map_or(Self::All, Self::Provider);
        }
        if let Some((provider, exchange)) = value
            .strip_prefix("exchange:")
            .and_then(|rest| rest.split_once(':'))
            && let (Some(provider), Some(exchange)) =
                (registered(provider), ExchangeCode::new(exchange))
        {
            return Self::Exchange { provider, exchange };
        }
        Self::All
    }
}

/// Durable shell chrome that follows the user across charts and workspaces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartChromePreferences {
    pub indicator_name_labels_visible: bool,
    pub indicator_value_labels_visible: bool,
    pub indicator_price_lines_visible: bool,
    pub chart_type: ChartType,
    /// Instrument categories the symbol menu searches; at least one is always included.
    pub symbol_search_categories: InstrumentSearchCategories,
    pub symbol_search_scope: SymbolSearchScope,
    pub window_frame: WindowFrame,
    /// The drawing tools sidebar along the left edge of the charts.
    pub drawing_toolbar: ChromeVisibility,
    pub drawing_favorites: DrawingFavorites,
    pub trading_shortcuts: TradingShortcutMode,
    /// The Buy / quantity / Sell bar under each chart's symbol legend.
    pub chart_trade_buttons: ChromeVisibility,
    /// Window-relative logical origin the selected-drawing toolbar was dragged to; `None` is
    /// the default spot.
    pub drawing_context_toolbar_origin: Option<(f32, f32)>,
}

const STAMP_FAVORITE_PREFIX: &str = "stamp:";

/// The drawing tools the user starred and the floating toolbar that offers them.
///
/// Tools are a fixed bitset (engine wire id for kinds, catalog position for built-in stamps) so
/// the preferences stay `Copy`; on disk they are the engine's stable kind names and stamp keys,
/// and unknown names are skipped so a newer file never fails to load.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawingFavorites {
    kinds: [u64; 4],
    stamps: u16,
    pub toolbar_visible: bool,
    /// Window-relative logical origin the toolbar was dragged to; `None` is the default spot.
    pub toolbar_origin: Option<(f32, f32)>,
}

impl Default for DrawingFavorites {
    fn default() -> Self {
        Self {
            kinds: [0; 4],
            stamps: 0,
            toolbar_visible: true,
            toolbar_origin: None,
        }
    }
}

impl DrawingFavorites {
    fn kind_slot(kind: ChartDrawingKind) -> (usize, u64) {
        let id = kind.to_u8();
        (usize::from(id / 64), 1 << (id % 64))
    }

    fn stamp_bit(stamp: ChartDrawingStamp) -> u16 {
        ChartDrawingStamp::ALL
            .iter()
            .position(|candidate| *candidate == stamp)
            .map_or(0, |index| 1 << index)
    }

    #[must_use]
    pub fn contains_kind(&self, kind: ChartDrawingKind) -> bool {
        let (word, bit) = Self::kind_slot(kind);
        self.kinds[word] & bit != 0
    }

    #[must_use]
    pub fn contains_stamp(&self, stamp: ChartDrawingStamp) -> bool {
        self.stamps & Self::stamp_bit(stamp) != 0
    }

    pub fn toggle_kind(&mut self, kind: ChartDrawingKind) {
        let (word, bit) = Self::kind_slot(kind);
        self.kinds[word] ^= bit;
    }

    pub fn toggle_stamp(&mut self, stamp: ChartDrawingStamp) {
        self.stamps ^= Self::stamp_bit(stamp);
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stamps == 0 && self.kinds.iter().all(|word| *word == 0)
    }

    fn parse_tools(&mut self, value: &str) {
        for name in value.split(',').map(str::trim) {
            if let Some(key) = name.strip_prefix(STAMP_FAVORITE_PREFIX) {
                if let Some(stamp) = ChartDrawingStamp::ALL
                    .into_iter()
                    .find(|stamp| stamp.key() == key)
                    && !self.contains_stamp(stamp)
                {
                    self.toggle_stamp(stamp);
                }
            } else if let Some(kind) = ChartDrawingKind::from_name(name)
                && !self.contains_kind(kind)
            {
                self.toggle_kind(kind);
            }
        }
    }

    fn encode_tools(&self) -> String {
        let kinds = (0..=u8::MAX)
            .filter_map(ChartDrawingKind::from_u8)
            .filter(|kind| self.contains_kind(*kind))
            .map(|kind| kind.name().to_string());
        let stamps = ChartDrawingStamp::ALL
            .into_iter()
            .filter(|stamp| self.contains_stamp(*stamp))
            .map(|stamp| format!("{STAMP_FAVORITE_PREFIX}{}", stamp.key()));
        kinds.chain(stamps).collect::<Vec<_>>().join(",")
    }
}

fn parse_toolbar_origin(value: &str) -> Option<(f32, f32)> {
    let (x, y) = value.split_once(',')?;
    let x = x.trim().parse::<f32>().ok()?;
    let y = y.trim().parse::<f32>().ok()?;
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

/// How the workspace title bar frames the window.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WindowFrame {
    /// The title bar keeps its row above the workspace.
    #[default]
    Framed,
    /// The title bar leaves the layout and reveals on hover at the top edge.
    Frameless,
}

impl WindowFrame {
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::Framed => "framed",
            Self::Frameless => "frameless",
        }
    }

    #[must_use]
    pub fn from_identifier(value: &str) -> Option<Self> {
        match value.trim() {
            "framed" => Some(Self::Framed),
            "frameless" => Some(Self::Frameless),
            _ => None,
        }
    }

    #[must_use]
    pub const fn frameless(self) -> bool {
        matches!(self, Self::Frameless)
    }
}

/// Whether an optional piece of shell chrome is shown.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ChromeVisibility {
    #[default]
    Shown,
    Hidden,
}

impl ChromeVisibility {
    #[must_use]
    pub const fn from_shown(shown: bool) -> Self {
        if shown { Self::Shown } else { Self::Hidden }
    }

    #[must_use]
    pub const fn shown(self) -> bool {
        matches!(self, Self::Shown)
    }
}

/// Whether trading shortcuts ask before they send.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TradingShortcutMode {
    /// Each shortcut opens a confirmation first.
    #[default]
    Confirm,
    /// One-click trading: shortcuts send at once.
    OneClick,
}

impl TradingShortcutMode {
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::Confirm => "confirm",
            Self::OneClick => "one_click",
        }
    }

    /// Only the exact one-click identifier skips confirmation; anything else keeps it.
    #[must_use]
    pub fn from_identifier(value: &str) -> Self {
        match value.trim() {
            "one_click" => Self::OneClick,
            _ => Self::Confirm,
        }
    }

    #[must_use]
    pub const fn one_click(self) -> bool {
        matches!(self, Self::OneClick)
    }
}

impl Default for ChartChromePreferences {
    fn default() -> Self {
        let indicator_chrome = ChartIndicatorChrome::default();
        Self {
            indicator_name_labels_visible: indicator_chrome.name_labels_visible,
            indicator_value_labels_visible: indicator_chrome.value_labels_visible,
            indicator_price_lines_visible: indicator_chrome.price_lines_visible,
            chart_type: ChartType::Candles,
            symbol_search_categories: InstrumentSearchCategories::ALL,
            symbol_search_scope: SymbolSearchScope::All,
            window_frame: WindowFrame::Framed,
            drawing_toolbar: ChromeVisibility::Shown,
            drawing_favorites: DrawingFavorites::default(),
            trading_shortcuts: TradingShortcutMode::Confirm,
            chart_trade_buttons: ChromeVisibility::Shown,
            drawing_context_toolbar_origin: None,
        }
    }
}

/// Per-desktop durable state. Clones share one bounded saver.
#[derive(Clone)]
pub struct DesktopPersistence {
    inner: std::sync::Arc<DesktopPersistenceInner>,
}

struct DesktopPersistenceInner {
    root: PathBuf,
    save_state: Mutex<ChartChromeSaveState>,
    /// Set when the saved file was written by a newer version: this session never writes it.
    chrome_read_only: AtomicBool,
}

impl DesktopPersistence {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            inner: std::sync::Arc::new(DesktopPersistenceInner {
                root,
                save_state: Mutex::new(ChartChromeSaveState::default()),
                chrome_read_only: AtomicBool::new(false),
            }),
        }
    }

    #[must_use]
    pub fn workspace_path(&self) -> PathBuf {
        self.inner.root.join("workspace-state.pb")
    }

    #[must_use]
    pub fn chart_chrome_path(&self) -> PathBuf {
        self.inner.root.join("desktop").join("chart-chrome")
    }

    #[must_use]
    pub fn load_chart_chrome_preferences(&self) -> ChartChromePreferences {
        let (preferences, _, newer) = restore_chart_chrome_preferences(&self.chart_chrome_path());
        if newer.is_some() {
            self.inner.chrome_read_only.store(true, Ordering::Release);
        }
        preferences
    }

    /// Queues the latest preferences without writing on the UI thread.
    ///
    /// # Errors
    /// Returns an error if the save state is poisoned or its generation is exhausted.
    pub fn request_chart_chrome_preferences_save(
        &self,
        preferences: ChartChromePreferences,
    ) -> Result<bool, String> {
        if self.inner.chrome_read_only.load(Ordering::Acquire) {
            // A newer version owns the saved file; changes apply in memory for the session.
            return Ok(false);
        }
        self.inner
            .save_state
            .lock()
            .map_err(|_| "desktop chart chrome persistence is unavailable".to_string())?
            .request(preferences)
    }

    #[must_use]
    pub fn chart_chrome_shutdown_wait(&self) -> ChartChromeShutdownWait {
        ChartChromeShutdownWait {
            inner: self.inner.clone(),
            expected_generation: self
                .inner
                .save_state
                .lock()
                .map(|state| state.latest_requested_generation)
                .map_err(|_| "desktop chart chrome persistence is unavailable".to_string()),
        }
    }

    #[must_use]
    pub fn chart_chrome_shutdown_generation_is_current(&self, generation: u64) -> bool {
        self.inner
            .save_state
            .lock()
            .is_ok_and(|state| chart_chrome_generation_is_current(&state, generation))
    }

    /// # Errors
    /// Returns an error if the queued save cannot reach durable storage.
    pub fn run_chart_chrome_preferences_save_worker(&self) -> Result<(), String> {
        run_chart_chrome_preferences_save_worker_to(
            &self.chart_chrome_path(),
            &self.inner.save_state,
        )
    }
}

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

    #[cfg(test)]
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

pub struct ChartChromeShutdownWait {
    inner: std::sync::Arc<DesktopPersistenceInner>,
    expected_generation: Result<u64, String>,
}

impl ChartChromeShutdownWait {
    /// # Errors
    /// Returns an error on save failure, a newer request, or timeout.
    pub fn wait(self, timeout: Duration) -> Result<u64, String> {
        let expected_generation = self.expected_generation?;
        wait_for_chart_chrome_generation(&self.inner.save_state, expected_generation, timeout)
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
        } else if let Some(value) = line.strip_prefix("symbol_search_futures=") {
            preferences.symbol_search_categories.futures = parse_chrome_flag(value);
        } else if let Some(value) = line.strip_prefix("symbol_search_equities=") {
            preferences.symbol_search_categories.equities = parse_chrome_flag(value);
        } else if let Some(value) = line.strip_prefix("symbol_search_scope=") {
            preferences.symbol_search_scope = SymbolSearchScope::parse(value);
        } else if let Some(value) = line.strip_prefix("window_frame=")
            && let Some(window_frame) = WindowFrame::from_identifier(value)
        {
            preferences.window_frame = window_frame;
        } else if let Some(value) = line.strip_prefix("drawing_toolbar=") {
            preferences.drawing_toolbar = ChromeVisibility::from_shown(parse_chrome_flag(value));
        } else if let Some(value) = line.strip_prefix("drawing_favorites=") {
            preferences.drawing_favorites.parse_tools(value);
        } else if let Some(value) = line.strip_prefix("drawing_favorites_toolbar=") {
            preferences.drawing_favorites.toolbar_visible = parse_chrome_flag(value);
        } else if let Some(value) = line.strip_prefix("drawing_favorites_origin=") {
            preferences.drawing_favorites.toolbar_origin = parse_toolbar_origin(value);
        } else if let Some(value) = line.strip_prefix("trading_shortcuts=") {
            preferences.trading_shortcuts = TradingShortcutMode::from_identifier(value);
        } else if let Some(value) = line.strip_prefix("chart_trade_buttons=") {
            preferences.chart_trade_buttons =
                ChromeVisibility::from_shown(parse_chrome_flag(value));
        } else if let Some(value) = line.strip_prefix("drawing_context_toolbar_origin=") {
            preferences.drawing_context_toolbar_origin = parse_toolbar_origin(value);
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
    let favorites = preferences.drawing_favorites;
    let origin = favorites
        .toolbar_origin
        .map(|(x, y)| format!("drawing_favorites_origin={x},{y}\n"))
        .unwrap_or_default();
    let context_origin = preferences
        .drawing_context_toolbar_origin
        .map(|(x, y)| format!("drawing_context_toolbar_origin={x},{y}\n"))
        .unwrap_or_default();
    format!(
        "indicator_name_labels={}\nindicator_value_labels={}\nindicator_price_lines={}\nchart_type={}\nsymbol_search_futures={}\nsymbol_search_equities={}\nsymbol_search_scope={}\nwindow_frame={}\ndrawing_toolbar={}\ndrawing_favorites={}\ndrawing_favorites_toolbar={}\ntrading_shortcuts={}\nchart_trade_buttons={}\n{origin}{context_origin}",
        u8::from(preferences.indicator_name_labels_visible),
        u8::from(preferences.indicator_value_labels_visible),
        u8::from(preferences.indicator_price_lines_visible),
        preferences.chart_type.identifier(),
        u8::from(preferences.symbol_search_categories.futures),
        u8::from(preferences.symbol_search_categories.equities),
        preferences.symbol_search_scope.encode(),
        preferences.window_frame.identifier(),
        u8::from(preferences.drawing_toolbar.shown()),
        favorites.encode_tools(),
        u8::from(favorites.toolbar_visible),
        preferences.trading_shortcuts.identifier(),
        u8::from(preferences.chart_trade_buttons.shown()),
    )
}

/// The same bound as the workspace file.
const MAXIMUM_CHART_CHROME_FILE_BYTES: usize = 12 * 1_048_576;
/// The durable-container schema of the chart chrome payload, UTF-8 `key=value` lines.
const CHART_CHROME_CONTAINER_SCHEMA: u32 = 1;

fn chart_chrome_container(path: &Path) -> DurableContainer {
    DurableContainer::new(
        path.to_path_buf(),
        MAXIMUM_CHART_CHROME_FILE_BYTES,
        CHART_CHROME_CONTAINER_SCHEMA,
    )
}

/// Both the enveloped schema-1 payload and a released version's bare file are the same
/// `key=value` text.
fn decode_chart_chrome_candidate(
    candidate: DurableCandidate<'_>,
) -> Result<ChartChromePreferences, String> {
    match candidate.format {
        DurableFormat::Legacy
        | DurableFormat::Envelope {
            schema_version: CHART_CHROME_CONTAINER_SCHEMA,
        } => {}
        DurableFormat::Envelope { schema_version } => {
            return Err(format!(
                "chart chrome schema version {schema_version} is unsupported"
            ));
        }
    }
    std::str::from_utf8(candidate.payload)
        .map(parse_chart_chrome_preferences)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
fn load_chart_chrome_preferences_from(path: &Path) -> ChartChromePreferences {
    restore_chart_chrome_preferences(path).0
}

/// Restores the saved preferences, or the defaults when no file is usable, and reports
/// every rejected file through observability. A file written by a newer version is returned
/// as the third element: it is not read, moved or overwritten.
fn restore_chart_chrome_preferences(
    path: &Path,
) -> (
    ChartChromePreferences,
    Vec<DurableRejection>,
    Option<DurableNewerVersion>,
) {
    let load = chart_chrome_container(path).load(decode_chart_chrome_candidate);
    for rejection in &load.rejected {
        diagnostic!("Aeris chart preferences file rejected: {rejection}");
    }
    if let Some(newer) = &load.newer_version {
        diagnostic!(
            "Aeris chart preferences {} were written by a newer version (schema {}) and stay untouched",
            newer.path.display(),
            newer.schema_version
        );
    }
    let preferences = if let Some((preferences, source)) = load.restored {
        if source != DurableSource::Primary {
            diagnostic!("Aeris chart preferences restored from {source:?}");
        }
        preferences
    } else {
        if !load.rejected.is_empty() {
            diagnostic!("Aeris chart preferences use defaults: no saved file was usable");
        }
        ChartChromePreferences::default()
    };
    (preferences, load.rejected, load.newer_version)
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
fn save_chart_chrome_preferences_to(
    path: &Path,
    preferences: ChartChromePreferences,
) -> Result<(), String> {
    let encoded = encode_chart_chrome_preferences(preferences);
    chart_chrome_container(path)
        .write(CHART_CHROME_CONTAINER_SCHEMA, encoded.as_bytes())
        .map_err(|error| format!("desktop chart chrome could not be published: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        ChartChromePreferences, ChartChromeSaveState, ChromeVisibility, DesktopPersistence,
        DrawingFavorites, ExchangeCode, INDICATOR_SPECS, IndicatorKind, IndicatorLocation,
        IndicatorParameters, InstrumentSearchCategories, MAXIMUM_CHART_CHROME_FILE_BYTES,
        SymbolSearchScope, TradingShortcutMode, WindowFrame, chart_chrome_container,
        encode_chart_chrome_preferences, filter_indicator_specs,
        load_chart_chrome_preferences_from, parse_chart_chrome_preferences,
        restore_chart_chrome_preferences, run_chart_chrome_preferences_save_worker_to,
        save_chart_chrome_preferences_to, wait_for_chart_chrome_generation,
    };
    use aeris_chart_integration::{ChartDrawingKind, ChartDrawingStamp, ChartType};
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
        let container = chart_chrome_container(path);
        let quarantined = (1..=aeris_platform_runtime::MAXIMUM_QUARANTINED_COPIES)
            .map(|index| container.quarantine_path(index));
        for file in [path.to_path_buf(), container.backup_path()]
            .into_iter()
            .chain(quarantined)
        {
            if let Err(error) = std::fs::remove_file(file) {
                assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
            }
        }
    }

    #[test]
    fn two_desktop_persistence_owners_keep_preferences_and_shutdown_receipts_separate() {
        let first_root = temporary_chart_chrome_path("first-owner");
        let second_root = temporary_chart_chrome_path("second-owner");
        let first = DesktopPersistence::new(first_root.clone());
        let second = DesktopPersistence::new(second_root.clone());
        let selected = ChartChromePreferences {
            chart_type: ChartType::Line,
            ..ChartChromePreferences::default()
        };
        assert!(
            first
                .request_chart_chrome_preferences_save(selected)
                .expect("first queued")
        );
        assert!(
            first
                .chart_chrome_shutdown_wait()
                .wait(Duration::ZERO)
                .is_err()
        );
        assert_eq!(
            second.chart_chrome_shutdown_wait().wait(Duration::ZERO),
            Ok(0)
        );
        first
            .run_chart_chrome_preferences_save_worker()
            .expect("first durable");
        assert_eq!(
            first.chart_chrome_shutdown_wait().wait(Duration::ZERO),
            Ok(1)
        );
        assert_eq!(first.load_chart_chrome_preferences(), selected);
        assert_eq!(
            second.load_chart_chrome_preferences(),
            ChartChromePreferences::default()
        );
        assert_ne!(first.workspace_path(), second.workspace_path());
        std::fs::remove_file(first.chart_chrome_path()).expect("first fixture removed");
        std::fs::remove_dir_all(first_root).expect("first fixture directory removed");
        // The second composition never wrote any file.
        assert!(!second_root.exists());
    }

    fn starred_trend_line_and_star() -> DrawingFavorites {
        let mut favorites = DrawingFavorites::default();
        favorites.toggle_kind(ChartDrawingKind::TrendLine);
        favorites.toggle_stamp(ChartDrawingStamp::Star);
        favorites
    }

    #[test]
    fn drawing_favorites_toggle_and_survive_unknown_or_duplicate_names() {
        let mut favorites = DrawingFavorites::default();
        assert!(favorites.is_empty());
        assert!(favorites.toolbar_visible, "a first star shows the toolbar");
        for kind in (0..=u8::MAX).filter_map(ChartDrawingKind::from_u8) {
            favorites.toggle_kind(kind);
            assert!(favorites.contains_kind(kind), "{}", kind.name());
            favorites.toggle_kind(kind);
            assert!(!favorites.contains_kind(kind), "{}", kind.name());
        }
        for stamp in ChartDrawingStamp::ALL {
            favorites.toggle_stamp(stamp);
            assert!(favorites.contains_stamp(stamp));
        }
        assert!(!favorites.is_empty());

        let parsed = parse_chart_chrome_preferences(
            "drawing_favorites=stamp:star, trend_line,future_tool,trend_line,stamp:unknown\n\
             drawing_favorites_origin=NaN,4\n",
        )
        .drawing_favorites;
        assert_eq!(parsed, starred_trend_line_and_star());
        assert_eq!(
            parse_chart_chrome_preferences("drawing_favorites_origin=-12.5, 300\n")
                .drawing_favorites
                .toolbar_origin,
            Some((-12.5, 300.0))
        );
    }

    #[test]
    fn catalog_order_matches_the_platform_menu() {
        assert_eq!(
            INDICATOR_SPECS.map(|spec| spec.kind),
            [
                IndicatorKind::Sma,
                IndicatorKind::Ema,
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
    fn symbol_search_scope_parses_only_registered_providers_and_valid_exchanges() {
        let defaults = parse_chart_chrome_preferences("");
        assert_eq!(defaults.symbol_search_scope, SymbolSearchScope::All);
        for (saved, scope) in [
            (
                "symbol_search_scope=provider:hyperliquid\n",
                SymbolSearchScope::Provider("hyperliquid"),
            ),
            (
                "symbol_search_scope=provider:unknown\n",
                SymbolSearchScope::All,
            ),
            (
                "symbol_search_scope=exchange:rithmic:\n",
                SymbolSearchScope::All,
            ),
            (
                "symbol_search_scope=exchange:rithmic:CME GLOBEX\n",
                SymbolSearchScope::All,
            ),
            ("symbol_search_scope=garbage\n", SymbolSearchScope::All),
        ] {
            assert_eq!(
                parse_chart_chrome_preferences(saved).symbol_search_scope,
                scope,
                "{saved}"
            );
        }
        assert!(ExchangeCode::new(&"X".repeat(ExchangeCode::MAXIMUM_BYTES + 1)).is_none());
    }

    #[test]
    fn chart_chrome_preferences_round_trip_through_durable_file() {
        let defaults = parse_chart_chrome_preferences("");
        assert!(defaults.indicator_name_labels_visible);
        assert!(defaults.indicator_value_labels_visible);
        assert!(
            !defaults.indicator_price_lines_visible,
            "indicator price lines follow the Aeris Charts default and start hidden"
        );
        assert_eq!(defaults.chart_type, ChartType::Candles);
        let legacy = parse_chart_chrome_preferences("indicator_labels=0\n");
        assert!(!legacy.indicator_name_labels_visible);
        assert!(!legacy.indicator_value_labels_visible);
        assert!(!legacy.indicator_price_lines_visible);
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
            symbol_search_scope: SymbolSearchScope::Exchange {
                provider: "rithmic",
                exchange: ExchangeCode::new("CBOT").expect("exchange code"),
            },
            window_frame: WindowFrame::Frameless,
            drawing_toolbar: ChromeVisibility::Hidden,
            drawing_favorites: DrawingFavorites {
                toolbar_visible: false,
                toolbar_origin: Some((120.5, 80.0)),
                ..starred_trend_line_and_star()
            },
            trading_shortcuts: TradingShortcutMode::OneClick,
            chart_trade_buttons: ChromeVisibility::Hidden,
            drawing_context_toolbar_origin: Some((640.0, 96.5)),
        };
        assert_eq!(
            encode_chart_chrome_preferences(hidden),
            "indicator_name_labels=0\nindicator_value_labels=1\nindicator_price_lines=0\nchart_type=bars\nsymbol_search_futures=1\nsymbol_search_equities=0\nsymbol_search_scope=exchange:rithmic:CBOT\nwindow_frame=frameless\ndrawing_toolbar=0\ndrawing_favorites=trend_line,stamp:star\ndrawing_favorites_toolbar=0\ntrading_shortcuts=one_click\nchart_trade_buttons=0\ndrawing_favorites_origin=120.5,80\ndrawing_context_toolbar_origin=640,96.5\n"
        );
        assert_eq!(defaults.drawing_context_toolbar_origin, None);
        assert_eq!(
            defaults.trading_shortcuts,
            TradingShortcutMode::Confirm,
            "trading shortcuts ask first by default"
        );
        assert_eq!(
            parse_chart_chrome_preferences("trading_shortcuts=1\n").trading_shortcuts,
            TradingShortcutMode::Confirm,
            "only the exact one-click identifier skips confirmation"
        );
        assert_eq!(defaults.window_frame, WindowFrame::Framed);
        assert!(
            defaults.drawing_toolbar.shown() && defaults.chart_trade_buttons.shown(),
            "a file without the keys keeps the drawing toolbar and trade buttons shown"
        );
        assert_eq!(
            parse_chart_chrome_preferences("window_frame=unknown\n").window_frame,
            WindowFrame::Framed,
            "an unrecognized frame keeps the framed default"
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
        let restored = load_chart_chrome_preferences_from(&path);
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
    fn chart_chrome_loader_prefers_current_and_falls_back_to_read_only_backup() {
        let path = temporary_chart_chrome_path("backup-recovery");
        let backup = path.with_extension("bak");
        let committed = ChartChromePreferences {
            indicator_name_labels_visible: false,
            indicator_value_labels_visible: true,
            indicator_price_lines_visible: false,
            chart_type: ChartType::Bars,
            symbol_search_categories: InstrumentSearchCategories::ALL,
            symbol_search_scope: SymbolSearchScope::All,
            window_frame: WindowFrame::Framed,
            drawing_toolbar: ChromeVisibility::Shown,
            drawing_favorites: DrawingFavorites::default(),
            trading_shortcuts: TradingShortcutMode::Confirm,
            chart_trade_buttons: ChromeVisibility::Shown,
            drawing_context_toolbar_origin: None,
        };
        save_chart_chrome_preferences_to(&path, committed).expect("committed preferences save");
        std::fs::write(&backup, "chart_type=line\n").expect("obsolete backup fixture writes");

        assert_eq!(
            load_chart_chrome_preferences_from(&path),
            committed,
            "the durable destination is authoritative even when an obsolete backup exists"
        );
        std::fs::remove_file(&path).expect("current fixture removes");
        assert_eq!(
            load_chart_chrome_preferences_from(&path),
            ChartChromePreferences {
                chart_type: ChartType::Line,
                ..ChartChromePreferences::default()
            },
            "the loader must fall back to the existing backup"
        );
        assert!(
            backup.is_file(),
            "loading must not rotate or remove the backup"
        );
        remove_chart_chrome_test_files(&path);
    }

    #[test]
    fn a_released_chart_chrome_file_loads_and_becomes_the_backup_on_save() {
        let path = temporary_chart_chrome_path("released");
        std::fs::write(&path, "chart_type=line\nwindow_frame=frameless\n")
            .expect("released chrome file writes");
        let released = load_chart_chrome_preferences_from(&path);
        assert_eq!(released.chart_type, ChartType::Line);
        assert_eq!(released.window_frame, WindowFrame::Frameless);

        let next = ChartChromePreferences {
            chart_type: ChartType::Bars,
            ..released
        };
        save_chart_chrome_preferences_to(&path, next).expect("enveloped chrome saves");
        assert!(
            std::fs::read(&path)
                .expect("saved chrome")
                .starts_with(&aeris_platform_runtime::DURABLE_CONTAINER_MAGIC)
        );
        assert_eq!(
            std::fs::read_to_string(chart_chrome_container(&path).backup_path())
                .expect("released file kept as backup"),
            "chart_type=line\nwindow_frame=frameless\n"
        );
        assert_eq!(load_chart_chrome_preferences_from(&path), next);
        assert_ne!(
            encode_chart_chrome_preferences(ChartChromePreferences::default()).as_bytes()[0],
            aeris_platform_runtime::DURABLE_CONTAINER_MAGIC[0],
            "chrome text starts with an ASCII key, never the container's NUL"
        );
        remove_chart_chrome_test_files(&path);
    }

    #[test]
    fn a_truncated_chart_chrome_file_restores_the_backup_and_is_quarantined() {
        let path = temporary_chart_chrome_path("truncated");
        let first = ChartChromePreferences {
            chart_type: ChartType::Line,
            ..ChartChromePreferences::default()
        };
        save_chart_chrome_preferences_to(&path, first).expect("first chrome saves");
        save_chart_chrome_preferences_to(
            &path,
            ChartChromePreferences {
                chart_type: ChartType::Bars,
                ..first
            },
        )
        .expect("second chrome saves");
        let saved = std::fs::read(&path).expect("saved chrome");
        std::fs::write(&path, &saved[..saved.len() - 1]).expect("truncate chrome");

        let (restored, rejected, _) = restore_chart_chrome_preferences(&path);
        assert_eq!(restored, first);
        assert_eq!(rejected.len(), 1);
        assert_eq!(
            rejected[0].quarantined_as.as_deref(),
            Some(chart_chrome_container(&path).quarantine_path(1).as_path())
        );
        remove_chart_chrome_test_files(&path);
    }

    #[test]
    fn an_oversized_chart_chrome_file_loads_defaults_with_a_diagnostic() {
        let path = temporary_chart_chrome_path("oversized");
        std::fs::write(&path, vec![b'a'; MAXIMUM_CHART_CHROME_FILE_BYTES + 1])
            .expect("oversized chrome writes");

        let (restored, rejected, _) = restore_chart_chrome_preferences(&path);
        assert_eq!(restored, ChartChromePreferences::default());
        assert_eq!(rejected.len(), 1);
        assert!(
            rejected[0].to_string().contains("persistence bound"),
            "{}",
            rejected[0]
        );
        assert!(
            chart_chrome_container(&path).quarantine_path(1).is_file(),
            "the oversized file is kept aside, not deleted"
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
            symbol_search_scope: SymbolSearchScope::All,
            window_frame: WindowFrame::Framed,
            drawing_toolbar: ChromeVisibility::Shown,
            drawing_favorites: DrawingFavorites::default(),
            trading_shortcuts: TradingShortcutMode::Confirm,
            chart_trade_buttons: ChromeVisibility::Shown,
            drawing_context_toolbar_origin: None,
        };
        let second = ChartChromePreferences {
            indicator_name_labels_visible: true,
            indicator_value_labels_visible: true,
            indicator_price_lines_visible: false,
            chart_type: ChartType::Bars,
            symbol_search_categories: InstrumentSearchCategories::ALL,
            symbol_search_scope: SymbolSearchScope::All,
            window_frame: WindowFrame::Framed,
            drawing_toolbar: ChromeVisibility::Shown,
            drawing_favorites: DrawingFavorites::default(),
            trading_shortcuts: TradingShortcutMode::Confirm,
            chart_trade_buttons: ChromeVisibility::Shown,
            drawing_context_toolbar_origin: None,
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
            symbol_search_scope: SymbolSearchScope::All,
            window_frame: WindowFrame::Framed,
            drawing_toolbar: ChromeVisibility::Shown,
            drawing_favorites: DrawingFavorites::default(),
            trading_shortcuts: TradingShortcutMode::Confirm,
            chart_trade_buttons: ChromeVisibility::Shown,
            drawing_context_toolbar_origin: None,
        };
        let second = ChartChromePreferences {
            indicator_name_labels_visible: true,
            indicator_value_labels_visible: true,
            indicator_price_lines_visible: false,
            chart_type: ChartType::Bars,
            symbol_search_categories: InstrumentSearchCategories::ALL,
            symbol_search_scope: SymbolSearchScope::All,
            window_frame: WindowFrame::Framed,
            drawing_toolbar: ChromeVisibility::Shown,
            drawing_favorites: DrawingFavorites::default(),
            trading_shortcuts: TradingShortcutMode::Confirm,
            chart_trade_buttons: ChromeVisibility::Shown,
            drawing_context_toolbar_origin: None,
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
