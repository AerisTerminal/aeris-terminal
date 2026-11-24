use super::*;

/// The color a newly set watchlist flag starts with, as the hex value the flag stores.
pub(super) fn default_watchlist_flag_color(theme: &AerisTheme) -> String {
    theme.colors.swatch_default.css_hex()
}

/// One watchlist row as the desktop owns it: the instrument and the user's flag.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct WatchlistEntry {
    pub(super) instrument: InstallProviderInstrument,
    /// A normalized CSS hex color; `None` while unflagged.
    pub(super) flag: Option<String>,
}

impl WatchlistEntry {
    pub(super) fn is(&self, provider: &str, instrument_id: &str) -> bool {
        self.instrument.provider == provider && self.instrument.instrument_id == instrument_id
    }
}

/// The open color picker of one watchlist row's flag.
#[derive(Clone)]
pub(super) struct WatchlistFlagPicker {
    pub(super) provider: String,
    pub(super) instrument_id: String,
    pub(super) input: Entity<InputState>,
    pub(super) error: Option<String>,
}

impl WatchlistFlagPicker {
    pub(super) fn is_for(&self, entry: &WatchlistEntry) -> bool {
        entry.is(&self.provider, &self.instrument_id)
    }
}

/// Indices of the entries the watchlist shows: all of them, or only those flagged with
/// `filter`. The row whose flag picker is open stays shown while its color changes.
pub(super) fn visible_watchlist_indices(
    entries: &[WatchlistEntry],
    filter: Option<&str>,
    picker: Option<&WatchlistFlagPicker>,
) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            filter.is_none_or(|filter| entry.flag.as_deref() == Some(filter))
                || picker.is_some_and(|picker| picker.is_for(entry))
        })
        .map(|(index, _)| index)
        .collect()
}

/// The distinct flag colors in watchlist order, which the flag filter offers.
pub(super) fn watchlist_flag_colors(entries: &[WatchlistEntry]) -> Vec<String> {
    let mut colors: Vec<String> = Vec::new();
    for flag in entries.iter().filter_map(|entry| entry.flag.as_ref()) {
        if !colors.contains(flag) {
            colors.push(flag.clone());
        }
    }
    colors
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct WatchlistDragState {
    pub(super) provider: String,
    pub(super) instrument_id: String,
    pub(super) cursor_offset_y: f32,
    pub(super) pointer_y: Option<f32>,
    pub(super) body_top: f32,
    pub(super) scroll_offset_y: f32,
}

#[derive(Clone, Copy)]
pub(super) struct ChartContextMenuState {
    pub(super) pane_count: usize,
    pub(super) flags: u8,
}

impl ChartContextMenuState {
    pub(super) const READY: u8 = 1;
    pub(super) const SPLIT: u8 = 2;
    pub(super) const DRAWINGS: u8 = 4;
    pub(super) const INDICATORS: u8 = 8;
    pub(super) const COPY_PRICE: u8 = 16;
    pub(super) const HIDDEN_DRAWINGS: u8 = 32;

    pub(super) const fn enabled(self, flag: u8) -> bool {
        self.flags & flag != 0
    }
}

#[derive(Clone, Copy)]
pub(super) struct ChartContextMenuItem {
    pub(super) id: &'static str,
    pub(super) icon: HugeIcon,
    pub(super) label: &'static str,
    pub(super) enabled: bool,
    pub(super) action: ChartContextAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChartContextAction {
    CopyPrice,
    /// Opens the capture chart submenu; the capture is taken by its Copy or Save rows.
    CaptureMenu,
    CopyCapture,
    SaveCapture,
    Reset,
    ClearDrawings,
    ShowHiddenDrawings,
    ClearIndicators,
    Split(ChartSplitDirection),
    Close,
    Settings,
}

impl ChartContextAction {
    pub(super) const fn is_destructive(self) -> bool {
        matches!(self, Self::ClearDrawings | Self::ClearIndicators)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum PriceAxisMenuFlyout {
    #[default]
    None,
    Labels,
    Lines,
    ScaleMode,
    YAxis,
    Precision,
}

impl PriceAxisMenuFlyout {
    /// The flyout's row count and the index of the root row that opens it.
    pub(super) const fn geometry(self) -> (f32, f32) {
        match self {
            Self::None => (0.0, 0.0),
            Self::Labels => (10.0, 0.0),
            Self::Lines => (5.0, 1.0),
            Self::ScaleMode => (4.0, 4.0),
            Self::YAxis => (2.0, 5.0),
            Self::Precision => (9.0, 6.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ChartContextMenu {
    pub(super) workspace_id: u64,
    pub(super) pane_id: u64,
    pub(super) position: gpui::Point<Pixels>,
    pub(super) kind: ChartContextKind,
    pub(super) flyout: PriceAxisMenuFlyout,
    pub(super) capture_flyout_open: bool,
    pub(super) copy_price: Option<SharedString>,
    pub(super) copy_feedback: Option<ChartCopyFeedback>,
}

/// The "Copied" confirmation on the row whose copy finished; the generation fences the timer
/// that closes the menu afterwards.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ChartCopyFeedback {
    pub(super) action: ChartContextAction,
    pub(super) generation: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ChartSettingsSection {
    #[default]
    Symbol,
    Canvas,
    Legend,
    Trading,
}

impl ChartSettingsSection {
    pub(super) const ALL: [Self; 4] = [Self::Symbol, Self::Canvas, Self::Legend, Self::Trading];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Symbol => tr!(workspace::SETTINGS_SECTION_SYMBOL),
            Self::Canvas => tr!(workspace::SETTINGS_SECTION_CANVAS),
            Self::Legend => tr!(workspace::SETTINGS_SECTION_LEGEND),
            Self::Trading => tr!(workspace::SETTINGS_SECTION_TRADING),
        }
    }

    pub(super) fn description(self) -> &'static str {
        match self {
            Self::Symbol => tr!(workspace::SETTINGS_SECTION_SYMBOL_DESCRIPTION),
            Self::Canvas => tr!(workspace::SETTINGS_SECTION_CANVAS_DESCRIPTION),
            Self::Legend => tr!(workspace::SETTINGS_SECTION_LEGEND_DESCRIPTION),
            Self::Trading => tr!(workspace::SETTINGS_SECTION_TRADING_DESCRIPTION),
        }
    }

    pub(super) const fn icon(self) -> HugeIcon {
        match self {
            Self::Symbol => HugeIcon::Chart,
            Self::Canvas => HugeIcon::View,
            Self::Legend => HugeIcon::DataPanel,
            Self::Trading => HugeIcon::OrderEntry,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) enum ChartColorSetting {
    Up,
    Down,
    WickUp,
    WickDown,
    BorderUp,
    BorderDown,
    Line,
    AreaTop,
    AreaBottom,
    BaselineTop,
    BaselineBottom,
    Background,
    Text,
    Grid,
    Crosshair,
    AxisBorder,
    AxisText,
}

pub(super) struct ChartColorPickerState {
    pub(super) setting: ChartColorSetting,
    pub(super) input: Entity<InputState>,
    pub(super) error: Option<String>,
}

impl ChartColorSetting {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Up => tr!(workspace::COLOR_UP_CANDLES),
            Self::Down => tr!(workspace::COLOR_DOWN_CANDLES),
            Self::WickUp => tr!(workspace::COLOR_UP_WICKS),
            Self::WickDown => tr!(workspace::COLOR_DOWN_WICKS),
            Self::BorderUp => tr!(workspace::COLOR_UP_BORDERS),
            Self::BorderDown => tr!(workspace::COLOR_DOWN_BORDERS),
            Self::Line => tr!(workspace::COLOR_LINE),
            Self::AreaTop => tr!(workspace::COLOR_AREA_TOP),
            Self::AreaBottom => tr!(workspace::COLOR_AREA_BOTTOM),
            Self::BaselineTop => tr!(workspace::COLOR_ABOVE_BASELINE),
            Self::BaselineBottom => tr!(workspace::COLOR_BELOW_BASELINE),
            Self::Background => tr!(workspace::COLOR_BACKGROUND),
            Self::Text => tr!(workspace::COLOR_TEXT),
            Self::Grid => tr!(workspace::COLOR_GRID),
            Self::Crosshair => tr!(workspace::COLOR_CROSSHAIR),
            Self::AxisBorder => tr!(workspace::COLOR_AXIS_BORDER),
            Self::AxisText => tr!(workspace::COLOR_AXIS_TEXT),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChartSettingsAction {
    ToggleGrid,
    ToggleAxisBorders,
    GridStyle(u8),
    CrosshairMode(u8),
    CrosshairWidth(u8),
    CrosshairStyle(u8),
    ToggleWicks,
    ToggleBorders,
    ToggleOpen,
    ToggleThinBars,
    LineWidth(u8),
    LineStyle(u8),
    FootprintMode(FootprintDisplayMode),
    /// Instrument ticks per footprint row; zero is automatic.
    FootprintTicksPerRow(u32),
    ToggleFootprintNakedPocs,
    ToggleFootprintStackedImbalanceZones,
    ToggleFootprintUnfinishedAuctions,
    ToggleFootprintExtendedSummary,
    TpoCells(TpoCells),
    /// Minutes each TPO letter covers.
    TpoPeriod(u16),
    TpoValueArea(u8),
    TpoInitialBalance(u8),
    ToggleOrderManagementLines,
    ToggleExecutionMarks,
    ToggleExtendOrderLinesLeft,
    ToggleSymbolLegend,
    ToggleSymbolLegendValues,
    ToggleSymbolLegendLogo,
    ToggleIndicatorLegend,
    ToggleIndicatorLegendValues,
    /// Applies to every chart: the Buy / quantity / Sell bar under the symbol legend.
    ToggleChartTradeButtons,
    /// Applies to the whole window: the edge glow on a new fill.
    ToggleFillGlow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ChartTradingVisibilitySettings {
    pub(super) show_order_management_lines: bool,
    pub(super) show_execution_marks: bool,
    pub(super) extend_order_lines_left: bool,
}

impl Default for ChartTradingVisibilitySettings {
    fn default() -> Self {
        Self {
            show_order_management_lines: true,
            show_execution_marks: false,
            extend_order_lines_left: false,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct ChartSettingsSnapshot {
    pub(super) chart_type: ChartType,
    pub(super) appearance: ChartAppearanceSettings,
    /// What theme-following appearance and axis colors paint under the chart's theme.
    pub(super) theme_defaults: ChartThemeDefaults,
    pub(super) axis: ChartAxisAppearance,
    pub(super) series_colors: ChartSeriesColors,
    pub(super) crosshair_mode: u8,
    pub(super) order_flow: OrderFlowSettings,
    pub(super) tpo: TpoSettings,
    pub(super) time_zone: String,
    pub(super) trading_visibility: ChartTradingVisibilitySettings,
    pub(super) chart_trade_buttons_visible: bool,
    pub(super) fill_glow_enabled: bool,
    pub(super) legend: ChartLegendOptions,
}
