use aeris_chart_integration::{
    ChartDrawingKind, ChartDrawingStamp, ChartIndicatorChrome, ChartType,
};
use aeris_contracts::{WorkspaceChromePoint, WorkspaceChromeState};
use aeris_market_runtime::InstrumentSearchCategories;
use std::path::{Path, PathBuf};

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

fn workspace_chrome_point(origin: Option<(f32, f32)>) -> Option<WorkspaceChromePoint> {
    origin.map(|(x, y)| WorkspaceChromePoint { x, y })
}

fn chrome_point_origin(point: Option<&WorkspaceChromePoint>) -> Option<(f32, f32)> {
    let point = point?;
    (point.x.is_finite() && point.y.is_finite()).then_some((point.x, point.y))
}

impl ChartChromePreferences {
    /// The durable workspace-document form of the preferences.
    #[must_use]
    pub fn to_workspace_chrome(&self) -> WorkspaceChromeState {
        WorkspaceChromeState {
            indicator_name_labels_visible: self.indicator_name_labels_visible,
            indicator_value_labels_visible: self.indicator_value_labels_visible,
            indicator_price_lines_visible: self.indicator_price_lines_visible,
            chart_type: self.chart_type.identifier().to_string(),
            symbol_search_futures: self.symbol_search_categories.futures,
            symbol_search_equities: self.symbol_search_categories.equities,
            symbol_search_scope: self.symbol_search_scope.encode(),
            window_frame: self.window_frame.identifier().to_string(),
            drawing_toolbar_visible: self.drawing_toolbar.shown(),
            drawing_favorites: self.drawing_favorites.encode_tools(),
            drawing_favorites_toolbar_visible: self.drawing_favorites.toolbar_visible,
            drawing_favorites_origin: workspace_chrome_point(self.drawing_favorites.toolbar_origin),
            trading_shortcuts: self.trading_shortcuts.identifier().to_string(),
            chart_trade_buttons_visible: self.chart_trade_buttons.shown(),
            drawing_context_toolbar_origin: workspace_chrome_point(
                self.drawing_context_toolbar_origin,
            ),
        }
    }

    /// Restores the preferences from their document form; missing or unrecognized values
    /// fall back to the defaults, exactly like the legacy key=value parser.
    #[must_use]
    pub fn from_workspace_chrome(state: Option<&WorkspaceChromeState>) -> Self {
        let mut preferences = Self::default();
        let Some(state) = state else {
            return preferences;
        };
        preferences.indicator_name_labels_visible = state.indicator_name_labels_visible;
        preferences.indicator_value_labels_visible = state.indicator_value_labels_visible;
        preferences.indicator_price_lines_visible = state.indicator_price_lines_visible;
        if let Some(chart_type) = ChartType::from_identifier(&state.chart_type) {
            preferences.chart_type = chart_type;
        }
        preferences.symbol_search_categories = InstrumentSearchCategories {
            futures: state.symbol_search_futures,
            equities: state.symbol_search_equities,
        };
        // A search that excludes every category would never return anything.
        if !preferences.symbol_search_categories.futures
            && !preferences.symbol_search_categories.equities
        {
            preferences.symbol_search_categories = InstrumentSearchCategories::ALL;
        }
        preferences.symbol_search_scope = SymbolSearchScope::parse(&state.symbol_search_scope);
        if let Some(window_frame) = WindowFrame::from_identifier(&state.window_frame) {
            preferences.window_frame = window_frame;
        }
        preferences.drawing_toolbar = ChromeVisibility::from_shown(state.drawing_toolbar_visible);
        let mut favorites = DrawingFavorites {
            toolbar_visible: state.drawing_favorites_toolbar_visible,
            toolbar_origin: chrome_point_origin(state.drawing_favorites_origin.as_ref()),
            ..DrawingFavorites::default()
        };
        favorites.parse_tools(&state.drawing_favorites);
        preferences.drawing_favorites = favorites;
        preferences.trading_shortcuts =
            TradingShortcutMode::from_identifier(&state.trading_shortcuts);
        preferences.chart_trade_buttons =
            ChromeVisibility::from_shown(state.chart_trade_buttons_visible);
        preferences.drawing_context_toolbar_origin =
            chrome_point_origin(state.drawing_context_toolbar_origin.as_ref());
        preferences
    }
}

/// Per-desktop durable state paths. Chart chrome preferences persist inside the workspace
/// document through the one workspace writer; the legacy chart-chrome file is read once by
/// the schema 4 migration fold and never written again.
#[derive(Clone)]
pub struct DesktopPersistence {
    root: PathBuf,
}

impl DesktopPersistence {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    #[must_use]
    pub fn workspace_path(&self) -> PathBuf {
        self.root.join("workspace-state.pb")
    }
}

/// The legacy chart-chrome key=value file under one desktop data root.
#[must_use]
pub fn chart_chrome_path(root: &Path) -> PathBuf {
    root.join("desktop").join("chart-chrome")
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

#[cfg(test)]
mod tests {
    use super::{
        ChartChromePreferences, ChromeVisibility, DesktopPersistence, DrawingFavorites,
        ExchangeCode, INDICATOR_SPECS, IndicatorKind, IndicatorLocation, IndicatorParameters,
        InstrumentSearchCategories, SymbolSearchScope, TradingShortcutMode, WindowFrame,
        chart_chrome_path, filter_indicator_specs, parse_chart_chrome_preferences,
    };
    use aeris_chart_integration::{ChartDrawingKind, ChartDrawingStamp, ChartType};
    use aeris_contracts::{WorkspaceChromePoint, WorkspaceChromeState};
    use std::path::PathBuf;

    #[test]
    fn persistence_paths_stay_under_the_instance_data_root() {
        let root = PathBuf::from("aeris-root");
        let persistence = DesktopPersistence::new(root.clone());
        assert_eq!(
            persistence.workspace_path(),
            root.join("workspace-state.pb")
        );
        assert_eq!(
            chart_chrome_path(&root),
            root.join("desktop").join("chart-chrome"),
            "the migration fold reads the legacy chrome file from its released location"
        );
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

    fn hidden_chrome_preferences() -> ChartChromePreferences {
        ChartChromePreferences {
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
        }
    }

    #[test]
    fn chart_chrome_preferences_parse_from_the_legacy_file() {
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
        let hidden = hidden_chrome_preferences();
        assert_eq!(
            parse_chart_chrome_preferences(
                "indicator_name_labels=0\nindicator_value_labels=1\nindicator_price_lines=0\nchart_type=bars\nsymbol_search_futures=1\nsymbol_search_equities=0\nsymbol_search_scope=exchange:rithmic:CBOT\nwindow_frame=frameless\ndrawing_toolbar=0\ndrawing_favorites=trend_line,stamp:star\ndrawing_favorites_toolbar=0\ntrading_shortcuts=one_click\nchart_trade_buttons=0\ndrawing_favorites_origin=120.5,80\ndrawing_context_toolbar_origin=640,96.5\n"
            ),
            hidden,
            "the legacy key=value file parses to the same preferences"
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
    }

    #[test]
    fn chart_chrome_preferences_convert_to_and_from_the_workspace_document() {
        let hidden = hidden_chrome_preferences();
        let state = hidden.to_workspace_chrome();
        assert_eq!(
            state,
            WorkspaceChromeState {
                indicator_name_labels_visible: false,
                indicator_value_labels_visible: true,
                indicator_price_lines_visible: false,
                chart_type: "bars".to_string(),
                symbol_search_futures: true,
                symbol_search_equities: false,
                symbol_search_scope: "exchange:rithmic:CBOT".to_string(),
                window_frame: "frameless".to_string(),
                drawing_toolbar_visible: false,
                drawing_favorites: "trend_line,stamp:star".to_string(),
                drawing_favorites_toolbar_visible: false,
                drawing_favorites_origin: Some(WorkspaceChromePoint { x: 120.5, y: 80.0 }),
                trading_shortcuts: "one_click".to_string(),
                chart_trade_buttons_visible: false,
                drawing_context_toolbar_origin: Some(WorkspaceChromePoint { x: 640.0, y: 96.5 }),
            },
            "the document form keeps the legacy identifiers"
        );
        assert_eq!(
            ChartChromePreferences::from_workspace_chrome(Some(&state)),
            hidden
        );
        assert_eq!(
            ChartChromePreferences::from_workspace_chrome(None),
            ChartChromePreferences::default()
        );
        let marked_line = ChartChromePreferences {
            chart_type: ChartType::LineWithMarkers,
            ..hidden
        };
        assert_eq!(
            ChartChromePreferences::from_workspace_chrome(Some(&marked_line.to_workspace_chrome())),
            marked_line
        );
    }
}
