//! GPUI entity hosting one authoritative Nucleus chart engine and renderer.

use crate::bridge::{ChartBridgeMetrics, ChartDataBridge};
use crate::nucleus_bridge::{
    ProductPriceBars, apply_merged_chart_data, chart_data_queue_capacity,
    install_product_price_series, install_replay, install_volume_series, replay_legend_title,
    replay_price_divisor,
};
use crate::provenance::{DEFAULT_CHART_SERIES_MAX_POINTS, DisplayedProvenance};
use axiusflow_application::ReplayRecoveryCommand;
use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketEventProvenance, ReplaySnapshot,
    ReplayStreamUpdate, ReplayValidationError,
};
use gpui::{
    Animation, AnimationExt, AnyElement, App, Bounds, Context, CursorStyle, Entity, FocusHandle,
    KeyDownEvent, Modifiers, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Render, Rgba, Role, ScrollWheelEvent, SharedString, Window,
    canvas, div, prelude::*, px, rgba, svg,
};
use nucleuscharts_engine::{
    BrushRange, BrushStyle, ChartEngine, ChartFrame, ChartTheme, DeltaTooltipOptions, DrawingId,
    DrawingKind, DrawingModifiers, FeatureSeriesOptionsPatch, NativePrimitiveId, PriceScaleTarget,
};
use nucleuscharts_render::color::Color;
use nucleuscharts_render::draw_list::Prim;
use nucleuscharts_render_gpui::backend::measure_text;
use nucleuscharts_render_gpui::{GpuiChartRenderer, NucleusViewport, PreparedNucleusFrame};
use num_traits::ToPrimitive;
use std::collections::HashSet;
use std::fmt;
#[cfg(feature = "diagnostics")]
use std::time::Instant;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SCALE_FACTOR_EPSILON: f32 = 1.0e-4;
const WHEEL_LINE_HEIGHT: f32 = 32.0;
const KEYBOARD_PAGE_FRACTION: f64 = 0.8;
const PANE_SEPARATOR_HIT: f64 = 4.0;
const BRUSHABLE_LINE: (u8, u8, u8) = (40, 98, 255);
const BRUSHABLE_UP: (u8, u8, u8) = (4, 153, 129);
const BRUSHABLE_DOWN: (u8, u8, u8) = (239, 83, 80);
const LEGEND_INSET: f32 = 8.0;
const LEGEND_ROW_HEIGHT: f32 = 24.0;
const TEXT_CARET_PERIOD: Duration = Duration::from_secs(1);
const TEXT_EDIT_PAD: f32 = 4.0;

fn text_edit_char(event: &KeyDownEvent) -> Option<char> {
    if let Some(text) = event.keystroke.key_char.as_deref() {
        let mut chars = text.chars();
        let ch = chars.next()?;
        return (chars.next().is_none() && !ch.is_control()).then_some(ch);
    }
    match event.keystroke.key.as_str() {
        "space" => Some(' '),
        key => {
            let mut chars = key.chars();
            let ch = chars.next()?;
            if chars.next().is_some() || ch.is_control() {
                return None;
            }
            if ch.is_ascii_alphabetic() && event.keystroke.modifiers.shift {
                Some(ch.to_ascii_uppercase())
            } else {
                Some(ch)
            }
        }
    }
}

/// A native indicator supported by the chart's current OHLCV data bridge.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChartIndicator {
    Volume,
    Vwap,
    Sma,
    Ema,
    Wma,
    Bollinger,
    Rsi,
    Macd,
    Stochastic,
    Atr,
}

/// Host-portable state for one indicator instance on a chart surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChartIndicatorState {
    pub indicator: ChartIndicator,
    pub visible: bool,
}

impl ChartIndicator {
    /// All indicators that can be calculated truthfully from the installed OHLC columns.
    pub const ALL: [Self; 10] = [
        Self::Volume,
        Self::Vwap,
        Self::Sma,
        Self::Ema,
        Self::Wma,
        Self::Bollinger,
        Self::Rsi,
        Self::Macd,
        Self::Stochastic,
        Self::Atr,
    ];

    /// Returns the user-facing legacy catalog label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Volume => "Volume",
            Self::Vwap => "Volume Weighted Average Price",
            Self::Sma => "Moving Average",
            Self::Ema => "Moving Average Exponential",
            Self::Wma => "Weighted Moving Average",
            Self::Bollinger => "Bollinger Bands",
            Self::Rsi => "Relative Strength Index",
            Self::Macd => "MACD",
            Self::Stochastic => "Stochastic",
            Self::Atr => "Average True Range",
        }
    }

    /// Returns the fixed parameters shown by the legacy indicator catalog.
    #[must_use]
    pub const fn parameters(self) -> &'static str {
        match self {
            Self::Volume => "Up/down volume",
            Self::Vwap => "Session anchored",
            Self::Sma | Self::Ema | Self::Wma => "Period 20",
            Self::Bollinger => "Period 20 · Deviation 2",
            Self::Rsi | Self::Atr => "Period 14",
            Self::Macd => "Fast 12 · Slow 26 · Signal 9",
            Self::Stochastic => "%K 14 · %D 3",
        }
    }

    fn from_nucleus_kind(kind: &str) -> Option<Self> {
        Some(match kind {
            "vwap" => Self::Vwap,
            "sma" => Self::Sma,
            "ema" => Self::Ema,
            "wma" => Self::Wma,
            "bollinger" => Self::Bollinger,
            "rsi" => Self::Rsi,
            "macd" => Self::Macd,
            "stochastic" => Self::Stochastic,
            "atr" => Self::Atr,
            _ => return None,
        })
    }
}

/// Failure to create a native indicator on the chart's primary series.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChartIndicatorError {
    MarketDataUnavailable,
    CreationRejected(ChartIndicator),
}

impl fmt::Display for ChartIndicatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MarketDataUnavailable => {
                formatter.write_str("an indicator requires installed chart market data")
            }
            Self::CreationRejected(indicator) => {
                write!(
                    formatter,
                    "Nucleus rejected the {} indicator",
                    indicator.label()
                )
            }
        }
    }
}

impl std::error::Error for ChartIndicatorError {}

/// Product-owned price-series presentation forwarded to Nucleus `SeriesKind`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ChartType {
    #[default]
    Candles,
    Bars,
    Line,
    Area,
    Baseline,
    BrushableArea,
}

impl ChartType {
    /// Built-in OHLC chart types Nucleus can render from the product price series.
    pub const ALL: [Self; 6] = [
        Self::Candles,
        Self::Bars,
        Self::Line,
        Self::Area,
        Self::Baseline,
        Self::BrushableArea,
    ];

    /// Returns the header and menu label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Candles => "Candles",
            Self::Bars => "Bars",
            Self::Line => "Line",
            Self::Area => "Area",
            Self::Baseline => "Baseline",
            Self::BrushableArea => "Brushable area",
        }
    }

    /// Returns the durable preference identifier.
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::Candles => "candles",
            Self::Bars => "bars",
            Self::Line => "line",
            Self::Area => "area",
            Self::Baseline => "baseline",
            Self::BrushableArea => "brushable_area",
        }
    }

    /// Parses a stored identifier, ignoring unknown values.
    #[must_use]
    pub fn from_identifier(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|chart_type| chart_type.identifier() == value.trim())
    }

    pub(crate) const fn series_kind(self) -> Option<nucleuscharts_engine::SeriesKind> {
        match self {
            Self::Candles => Some(nucleuscharts_engine::SeriesKind::Candlestick),
            Self::Bars => Some(nucleuscharts_engine::SeriesKind::Bar),
            Self::Line => Some(nucleuscharts_engine::SeriesKind::Line),
            Self::Area => Some(nucleuscharts_engine::SeriesKind::Area),
            Self::Baseline => Some(nucleuscharts_engine::SeriesKind::Baseline),
            Self::BrushableArea => None,
        }
    }

    const fn shows_ohlc_legend(self) -> bool {
        matches!(self, Self::Candles | Self::Bars)
    }
}

/// Distinguishes a pane-canvas right-click from a price-axis right-click.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChartContextKind {
    Pane,
    PriceAxis { pane: usize, left: bool },
}

/// A chart-surface right-click waiting for the shell to present a menu.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartContextRequest {
    pub position: Point<Pixels>,
    pub kind: ChartContextKind,
}

/// Nucleus-owned price-axis chrome the Y-axis menu presents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PriceAxisMenuState {
    pub flags: u16,
    pub mode: u8,
    pub left: bool,
    pub precision: Option<u8>,
}

impl PriceAxisMenuState {
    pub const PRICE_LINE: u16 = 1;
    pub const LAST_VALUE: u16 = 2;
    pub const TITLE: u16 = 4;
    pub const COUNTDOWN: u16 = 8;
    pub const INDICATOR_NAMES: u16 = 16;
    pub const INDICATOR_VALUES: u16 = 32;
    pub const AUTO_SCALE: u16 = 64;
    pub const INVERT_SCALE: u16 = 128;
    pub const BID_ASK: u16 = 256;
    pub const ALIGN_LABELS: u16 = 512;
    pub const INDICATOR_PRICE_LINES: u16 = 1024;

    #[must_use]
    pub const fn enabled(self, flag: u16) -> bool {
        self.flags & flag != 0
    }
}

/// A user command from the Y-axis menu.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PriceAxisMenuAction {
    TogglePriceLine,
    ToggleLastValue,
    ToggleTitle,
    ToggleCountdown,
    ToggleIndicatorNameLabels,
    ToggleIndicatorValueLabels,
    ToggleIndicatorPriceLines,
    ToggleBidAsk,
    ToggleAlignLabels,
    ToggleAutoScale,
    ToggleInvertScale,
    SetMode(u8),
    SetLeft(bool),
    SetPrecision(Option<u8>),
}

const PRICE_AXIS_PRECISION_CHOICES: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 8];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum IndicatorLabels {
    #[default]
    Shown,
    Hidden,
}

impl IndicatorLabels {
    const fn visible(self) -> bool {
        matches!(self, Self::Shown)
    }

    const fn from_visible(visible: bool) -> Self {
        if visible { Self::Shown } else { Self::Hidden }
    }
}

fn price_axis_target(left: bool) -> PriceScaleTarget {
    if left {
        PriceScaleTarget::Left
    } else {
        PriceScaleTarget::Right
    }
}

fn price_format_min_move(precision: u8) -> f64 {
    10_f64.powi(-i32::from(precision.min(18)))
}

fn json_value<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\":");
    Some(json.split_once(&needle)?.1.trim_start())
}

fn json_u8(json: &str, key: &str) -> Option<u8> {
    json_value(json, key)?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

fn json_bool(json: &str, key: &str) -> Option<bool> {
    let rest = json_value(json, key)?;
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// A drawing tool exposed by the native chart surface.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ChartDrawingTool {
    /// Selects, moves, and pans without creating a drawing.
    #[default]
    Cursor,
    TrendLine,
    HorizontalLine,
    VerticalLine,
    Ray,
    Rectangle,
    Brush,
    Text,
}

impl ChartDrawingTool {
    const fn drawing_kind(self) -> Option<DrawingKind> {
        match self {
            Self::Cursor => None,
            Self::TrendLine => Some(DrawingKind::TrendLine),
            Self::HorizontalLine => Some(DrawingKind::HorizontalLine),
            Self::VerticalLine => Some(DrawingKind::VerticalLine),
            Self::Ray => Some(DrawingKind::HorizontalRay),
            Self::Rectangle => Some(DrawingKind::Rectangle),
            Self::Brush => Some(DrawingKind::Brush),
            Self::Text => Some(DrawingKind::Text),
        }
    }
}

/// Aggregate state used by drawing-toolbar lock controls.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrawingsLockSummary {
    pub total: usize,
    pub locked_count: usize,
    pub all_locked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ChartDrag {
    Pane {
        price_pan: Option<(usize, PriceScaleTarget)>,
    },
    BrushableRange,
    TimeAxis,
    PriceAxis {
        pane: usize,
        target: PriceScaleTarget,
    },
    PaneSeparator {
        index: usize,
        last_y: f64,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SeriesMutation {
    #[default]
    None,
    Snapshot,
    Append,
    TailReplace,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum LegendItem {
    Asset,
    Volume,
    Indicator(u32),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum LegendPresence {
    #[default]
    Absent,
    Present,
}

impl LegendPresence {
    const fn is_present(self) -> bool {
        matches!(self, Self::Present)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ActivationRequest {
    #[default]
    None,
    Pending,
}

impl LegendItem {
    fn key(self) -> u64 {
        match self {
            Self::Asset => 0,
            Self::Volume => 1,
            Self::Indicator(binding) => u64::from(binding) + 2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LegendRow {
    item: LegendItem,
    pane: usize,
    title: String,
    values: String,
    values_tone: LegendValueTone,
    visible: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum LegendValueTone {
    #[default]
    Neutral,
    Bullish,
    Bearish,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct LegendPaneLayout {
    left: f32,
    top: f32,
    height: f32,
}

#[derive(Clone, Copy)]
struct LegendPalette {
    text: Rgba,
    muted: Rgba,
    bullish: Rgba,
    bearish: Rgba,
    hover: Rgba,
    danger: Rgba,
}

const LEGEND_VIEW_ICON: &str = "axiusflow/icons/ui/view.svg";
const LEGEND_VIEW_OFF_ICON: &str = "axiusflow/icons/ui/view-off.svg";
const LEGEND_REMOVE_ICON: &str = "axiusflow/icons/ui/cancel-01.svg";

fn legend_series_value(snapshots: &[nucleuscharts_engine::SeriesValueSnapshot], id: u32) -> String {
    snapshots
        .iter()
        .find(|snapshot| snapshot.series_id == id)
        .and_then(|snapshot| {
            snapshot
                .formatted_value
                .as_ref()
                .or(snapshot.formatted_close.as_ref())
        })
        .cloned()
        .unwrap_or_default()
}

fn asset_legend_value_tone(
    snapshot: &nucleuscharts_engine::SeriesValueSnapshot,
) -> LegendValueTone {
    match (snapshot.open, snapshot.close) {
        (Some(open), Some(close)) if close >= open => LegendValueTone::Bullish,
        (Some(_), Some(_)) => LegendValueTone::Bearish,
        _ => LegendValueTone::Neutral,
    }
}

impl SeriesMutation {
    #[cfg(feature = "diagnostics")]
    const fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Snapshot => "snapshot",
            Self::Append => "append",
            Self::TailReplace => "tail_replace",
        }
    }
}

const fn should_stop_mouse_up_propagation(outside_chart: bool) -> bool {
    !outside_chart
}

/// A GPUI entity hosting one authoritative Nucleus chart engine and renderer.
pub struct NucleusChartView {
    engine: ChartEngine,
    theme: ChartTheme,
    renderer: GpuiChartRenderer,
    data_bridge: Option<ChartDataBridge>,
    displayed_provenance: DisplayedProvenance,
    price_divisor: f64,
    volume_series: u32,
    volume_legend: LegendPresence,
    asset_legend_title: String,
    legend_panes: Vec<LegendPaneLayout>,
    frame: ChartFrame,
    axis_prims: Vec<Prim>,
    built_for: (f32, f32, f32),
    layout_dirty: bool,
    fitted: bool,
    viewport_origin: (f32, f32),
    drag: Option<ChartDrag>,
    drawing_tool: ChartDrawingTool,
    locked_drawings: HashSet<DrawingId>,
    focus_handle: Option<FocusHandle>,
    cursor_style: CursorStyle,
    pending_context_menu: Option<ChartContextRequest>,
    pending_activate: ActivationRequest,
    instrument_price_precision: u8,
    price_precision_override: Option<u8>,
    chart_type: ChartType,
    product_bars: ProductPriceBars,
    brushable_tooltip: Option<NativePrimitiveId>,
    pending_brush_point: Option<(f64, f64)>,
    indicator_name_labels: IndicatorLabels,
    indicator_value_labels: IndicatorLabels,
    indicator_price_lines: IndicatorLabels,
    #[cfg(feature = "diagnostics")]
    last_snapshot_installation_nanos: Option<u64>,
    #[cfg(feature = "diagnostics")]
    live_evidence_enabled: bool,
    #[cfg(feature = "diagnostics")]
    live_evidence_rebuilds: u16,
    #[cfg(feature = "diagnostics")]
    live_evidence_mouse_downs: u8,
}

impl NucleusChartView {
    /// Creates an empty Nucleus-owned surface without inventing market data.
    #[must_use]
    pub fn empty() -> Self {
        Self::empty_with_theme(ChartTheme::Dark)
    }

    /// Creates an empty chart using Nucleus's canonical theme tokens.
    #[must_use]
    pub fn empty_with_theme(theme: ChartTheme) -> Self {
        let mut engine = ChartEngine::new(1024.0, 640.0, 1.0);
        engine.set_theme(theme);
        let volume_series = install_volume_series(&mut engine);
        let retention_applied =
            engine.set_series_max_points(0, Some(DEFAULT_CHART_SERIES_MAX_POINTS));
        debug_assert!(retention_applied);
        let volume_retention_applied =
            engine.set_series_max_points(volume_series, Some(DEFAULT_CHART_SERIES_MAX_POINTS));
        debug_assert!(volume_retention_applied);
        Self {
            engine,
            theme,
            renderer: GpuiChartRenderer::new(),
            data_bridge: None,
            displayed_provenance: DisplayedProvenance::empty(),
            price_divisor: 1.0,
            volume_series,
            volume_legend: LegendPresence::Absent,
            asset_legend_title: String::new(),
            legend_panes: Vec::new(),
            frame: ChartFrame::default(),
            axis_prims: Vec::new(),
            built_for: (0.0, 0.0, 0.0),
            layout_dirty: true,
            fitted: false,
            viewport_origin: (0.0, 0.0),
            drag: None,
            drawing_tool: ChartDrawingTool::Cursor,
            locked_drawings: HashSet::new(),
            focus_handle: None,
            cursor_style: CursorStyle::Crosshair,
            pending_context_menu: None,
            pending_activate: ActivationRequest::None,
            instrument_price_precision: 2,
            price_precision_override: None,
            chart_type: ChartType::Candles,
            product_bars: ProductPriceBars::default(),
            brushable_tooltip: None,
            pending_brush_point: None,
            indicator_name_labels: IndicatorLabels::Shown,
            indicator_value_labels: IndicatorLabels::Shown,
            indicator_price_lines: IndicatorLabels::Shown,
            #[cfg(feature = "diagnostics")]
            last_snapshot_installation_nanos: None,
            #[cfg(feature = "diagnostics")]
            live_evidence_enabled: std::env::var_os("AXIUSFLOW_LIVE_EVIDENCE").is_some(),
            #[cfg(feature = "diagnostics")]
            live_evidence_rebuilds: 0,
            #[cfg(feature = "diagnostics")]
            live_evidence_mouse_downs: 0,
        }
    }

    /// Creates a chart from the bounded embedded replay using Nucleus's own styling.
    ///
    /// # Panics
    ///
    /// Panics only if the application-owned embedded fixture violates its own
    /// validation contract.
    #[must_use]
    pub fn new() -> Self {
        let replay = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 600 })
            .expect("the embedded replay is validated application data");
        Self::with_replay(&replay)
    }

    /// Creates a chart from one validated application replay snapshot.
    ///
    /// A snapshot that cannot establish resumable sequence state installs
    /// without a live bridge instead of panicking the UI thread.
    #[must_use]
    pub fn with_replay(replay: &ReplaySnapshot) -> Self {
        Self::with_replay_and_theme(replay, ChartTheme::Dark)
    }

    /// Creates a replay-backed chart using Nucleus's canonical theme tokens.
    #[must_use]
    pub fn with_replay_and_theme(replay: &ReplaySnapshot, theme: ChartTheme) -> Self {
        let mut engine = ChartEngine::new(1024.0, 640.0, 1.0);
        engine.set_theme(theme);
        let volume_series = install_volume_series(&mut engine);
        let mut product_bars = ProductPriceBars::default();
        install_replay(
            &mut engine,
            volume_series,
            replay,
            ChartType::Candles,
            &mut product_bars,
        );
        let retention_applied =
            engine.set_series_max_points(0, Some(DEFAULT_CHART_SERIES_MAX_POINTS));
        debug_assert!(retention_applied);
        let volume_retention_applied =
            engine.set_series_max_points(volume_series, Some(DEFAULT_CHART_SERIES_MAX_POINTS));
        debug_assert!(volume_retention_applied);
        let data_bridge = ChartDataBridge::try_new(chart_data_queue_capacity(), replay).ok();
        debug_assert!(data_bridge.is_some());
        let mut chart = Self {
            engine,
            theme,
            renderer: GpuiChartRenderer::new(),
            data_bridge,
            displayed_provenance: DisplayedProvenance::from_snapshot(replay),
            price_divisor: replay_price_divisor(replay),
            volume_series,
            volume_legend: LegendPresence::Absent,
            asset_legend_title: replay_legend_title(replay),
            legend_panes: Vec::new(),
            frame: ChartFrame::default(),
            axis_prims: Vec::new(),
            built_for: (0.0, 0.0, 0.0),
            layout_dirty: true,
            fitted: false,
            viewport_origin: (0.0, 0.0),
            drag: None,
            drawing_tool: ChartDrawingTool::Cursor,
            locked_drawings: HashSet::new(),
            focus_handle: None,
            cursor_style: CursorStyle::Crosshair,
            pending_context_menu: None,
            pending_activate: ActivationRequest::None,
            instrument_price_precision: replay.instrument().precision.price_scale(),
            price_precision_override: None,
            chart_type: ChartType::Candles,
            product_bars,
            brushable_tooltip: None,
            pending_brush_point: None,
            indicator_name_labels: IndicatorLabels::Shown,
            indicator_value_labels: IndicatorLabels::Shown,
            indicator_price_lines: IndicatorLabels::Shown,
            #[cfg(feature = "diagnostics")]
            last_snapshot_installation_nanos: None,
            #[cfg(feature = "diagnostics")]
            live_evidence_enabled: std::env::var_os("AXIUSFLOW_LIVE_EVIDENCE").is_some(),
            #[cfg(feature = "diagnostics")]
            live_evidence_rebuilds: 0,
            #[cfg(feature = "diagnostics")]
            live_evidence_mouse_downs: 0,
        };
        chart.apply_selected_price_format();
        chart
    }

    /// Replaces Nucleus's authoritative series data with one validated snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot establish resumable sequence state.
    pub fn load_replay(&mut self, replay: &ReplaySnapshot) -> Result<(), ReplayValidationError> {
        #[cfg(feature = "diagnostics")]
        let snapshot_install_started = Instant::now();
        if let Some(bridge) = &mut self.data_bridge {
            bridge.install_snapshot(replay)?;
        } else {
            self.data_bridge = Some(ChartDataBridge::try_new(
                chart_data_queue_capacity(),
                replay,
            )?);
        }
        install_replay(
            &mut self.engine,
            self.volume_series,
            replay,
            self.chart_type,
            &mut self.product_bars,
        );
        self.apply_price_series_kind();
        self.displayed_provenance.replace_snapshot(replay);
        self.asset_legend_title = replay_legend_title(replay);
        self.price_divisor = replay_price_divisor(replay);
        self.instrument_price_precision = replay.instrument().precision.price_scale();
        self.apply_selected_price_format();
        self.invalidate_series_layout();
        self.fitted = false;
        #[cfg(feature = "diagnostics")]
        {
            self.last_snapshot_installation_nanos = Some(
                u64::try_from(snapshot_install_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            );
        }
        Ok(())
    }

    /// Restores Nucleus's native default time scale and automatic price scales.
    pub fn reset_view(&mut self) {
        self.engine.reset_view();
        self.invalidate_series_layout();
        self.fitted = true;
    }

    /// Takes a pending chart right-click request in window coordinates.
    pub fn take_context_menu_request(&mut self) -> Option<ChartContextRequest> {
        self.pending_context_menu.take()
    }

    /// Takes a pending request to make this chart's workspace pane active.
    pub fn take_activate_request(&mut self) -> bool {
        let pending = self.pending_activate == ActivationRequest::Pending;
        self.pending_activate = ActivationRequest::None;
        pending
    }

    /// Reads Nucleus-owned Y-axis chrome for the hit-tested price scale.
    #[must_use]
    pub fn price_axis_menu_state(&self, pane: usize, left: bool) -> Option<PriceAxisMenuState> {
        let target = price_axis_target(left);
        let (_, _, last_value, title, countdown, bid_ask) =
            self.primary_series_on_scale(pane, target)?;
        let options = self.engine.price_scale_options_json(pane, target)?;
        let mut flags = 0;
        if self.product_price_line_visible() {
            flags |= PriceAxisMenuState::PRICE_LINE;
        }
        if last_value {
            flags |= PriceAxisMenuState::LAST_VALUE;
        }
        if title {
            flags |= PriceAxisMenuState::TITLE;
        }
        if countdown {
            flags |= PriceAxisMenuState::COUNTDOWN;
        }
        if self.indicator_name_labels.visible() {
            flags |= PriceAxisMenuState::INDICATOR_NAMES;
        }
        if self.indicator_value_labels.visible() {
            flags |= PriceAxisMenuState::INDICATOR_VALUES;
        }
        if self.indicator_price_lines.visible() {
            flags |= PriceAxisMenuState::INDICATOR_PRICE_LINES;
        }
        if self.engine.price_scale_auto_scale_for(pane, target)? {
            flags |= PriceAxisMenuState::AUTO_SCALE;
        }
        if self.engine.price_scale_inverted_for(pane, target)? {
            flags |= PriceAxisMenuState::INVERT_SCALE;
        }
        if bid_ask {
            flags |= PriceAxisMenuState::BID_ASK;
        }
        if json_bool(&options, "align_labels").unwrap_or(true) {
            flags |= PriceAxisMenuState::ALIGN_LABELS;
        }
        Some(PriceAxisMenuState {
            flags,
            mode: json_u8(&options, "mode").unwrap_or(0),
            left,
            precision: self.price_precision_override,
        })
    }

    /// Applies one Y-axis menu command through Nucleus's scale and series APIs.
    pub fn apply_price_axis_menu_action(
        &mut self,
        pane: usize,
        left: bool,
        action: PriceAxisMenuAction,
    ) -> bool {
        let target = price_axis_target(left);
        let Some(primary_id) = self.primary_series_id_on_scale(pane, target) else {
            return false;
        };
        let applied = match action {
            PriceAxisMenuAction::TogglePriceLine => {
                self.toggle_series_flag(0, "price_line_visible")
            }
            PriceAxisMenuAction::ToggleLastValue => {
                self.toggle_series_flag(primary_id, "last_value_visible")
            }
            PriceAxisMenuAction::ToggleTitle => {
                self.toggle_series_flag(primary_id, "title_visible")
            }
            PriceAxisMenuAction::ToggleCountdown => {
                self.toggle_series_flag(primary_id, "countdown_visible")
            }
            PriceAxisMenuAction::ToggleIndicatorNameLabels => self.toggle_indicator_name_labels(),
            PriceAxisMenuAction::ToggleIndicatorValueLabels => self.toggle_indicator_value_labels(),
            PriceAxisMenuAction::ToggleIndicatorPriceLines => self.toggle_indicator_price_lines(),
            PriceAxisMenuAction::ToggleBidAsk => {
                self.toggle_series_flag(primary_id, "bid_ask_visible")
            }
            PriceAxisMenuAction::ToggleAlignLabels => {
                self.toggle_price_scale_flag(pane, target, "align_labels", true)
            }
            PriceAxisMenuAction::ToggleAutoScale => {
                let enabled = self.engine.price_scale_auto_scale_for(pane, target) != Some(true);
                self.engine
                    .set_price_scale_auto_scale_for(pane, target, enabled);
                true
            }
            PriceAxisMenuAction::ToggleInvertScale => {
                let inverted = self.engine.price_scale_inverted_for(pane, target) != Some(true);
                self.engine
                    .set_price_scale_inverted_for(pane, target, inverted);
                true
            }
            PriceAxisMenuAction::SetMode(mode) if mode <= 3 => self
                .engine
                .price_scale_apply_options_json(pane, target, &format!(r#"{{"mode":{mode}}}"#)),
            PriceAxisMenuAction::SetMode(_) => false,
            PriceAxisMenuAction::SetLeft(next_left) => self.move_price_axis(pane, left, next_left),
            PriceAxisMenuAction::SetPrecision(precision) => self.set_price_precision(precision),
        };
        if applied {
            self.invalidate_series_layout();
        }
        applied
    }

    /// Selects a Nucleus-owned theme without changing chart data or viewport.
    pub fn set_theme(&mut self, theme: ChartTheme) {
        self.theme = theme;
        self.engine.set_theme(theme);
        self.invalidate_series_layout();
    }

    /// Returns the time scale to the newest bar without changing its zoom.
    pub fn scroll_to_latest(&mut self) {
        self.engine.scroll_to_real_time();
        self.invalidate_series_layout();
    }

    /// Returns the settled visible time range in Unix nanoseconds.
    #[must_use]
    pub fn visible_time_range_unix_nanos(&self) -> Option<(i64, i64)> {
        let (start, end) = self.engine.visible_time_range()?;
        let start = (start * 1_000_000_000.0).round().to_i64()?;
        let end = (end * 1_000_000_000.0).round().to_i64()?;
        (start < end).then_some((start, end))
    }

    /// Restores a persisted visible time range without replacing chart data.
    pub fn set_visible_time_range_unix_nanos(&mut self, start: i64, end: i64) -> bool {
        if start >= end {
            return false;
        }
        self.engine.set_visible_time_range(
            start.to_f64().unwrap_or(0.0) / 1_000_000_000.0,
            end.to_f64().unwrap_or(0.0) / 1_000_000_000.0,
        );
        self.invalidate_series_layout();
        self.fitted = true;
        true
    }

    /// Adds an indicator with the defaults shown by the legacy native catalog.
    ///
    /// # Errors
    ///
    /// Returns an error when no market snapshot has populated the primary series or Nucleus
    /// cannot create every output required by the selected indicator.
    pub fn add_indicator(
        &mut self,
        indicator: ChartIndicator,
    ) -> Result<Vec<u32>, ChartIndicatorError> {
        if !self.has_market_data() {
            return Err(ChartIndicatorError::MarketDataUnavailable);
        }
        let ids = match indicator {
            ChartIndicator::Volume => {
                self.volume_legend = LegendPresence::Present;
                self.engine.set_series_visible(self.volume_series, true);
                vec![self.volume_series]
            }
            ChartIndicator::Vwap => self
                .engine
                .add_vwap(0, Some(self.volume_series))
                .into_iter()
                .collect(),
            ChartIndicator::Sma => self.engine.add_sma(0, 20).into_iter().collect(),
            ChartIndicator::Ema => self.engine.add_ema(0, 20).into_iter().collect(),
            ChartIndicator::Wma => self.engine.add_wma(0, 20).into_iter().collect(),
            ChartIndicator::Bollinger => self.engine.add_bollinger(0, 20, 2.0),
            ChartIndicator::Rsi => self.engine.add_rsi(0, 14).into_iter().collect(),
            ChartIndicator::Macd => self.engine.add_macd(0, 12, 26, 9),
            ChartIndicator::Stochastic => self.engine.add_stochastic(0, 14, 3),
            ChartIndicator::Atr => self.engine.add_atr(0, 14).into_iter().collect(),
        };
        let expected_outputs = match indicator {
            ChartIndicator::Bollinger | ChartIndicator::Macd => 3,
            ChartIndicator::Stochastic => 2,
            ChartIndicator::Volume
            | ChartIndicator::Vwap
            | ChartIndicator::Sma
            | ChartIndicator::Ema
            | ChartIndicator::Wma
            | ChartIndicator::Rsi
            | ChartIndicator::Atr => 1,
        };
        if ids.len() != expected_outputs {
            for &id in &ids {
                self.engine.remove_series(id);
            }
            return Err(ChartIndicatorError::CreationRejected(indicator));
        }
        self.apply_indicator_chrome_options();
        self.invalidate_series_layout();
        Ok(ids)
    }

    /// Returns active indicator instances without engine-local series identities.
    #[must_use]
    pub fn indicator_states(&self) -> Vec<ChartIndicatorState> {
        let entries = self.engine.series_entries();
        let mut bindings = HashSet::new();
        let mut states = Vec::new();
        for &id in self.engine.series_order() {
            if id == self.volume_series {
                if self.volume_legend.is_present() {
                    let visible = entries
                        .iter()
                        .find(|series| series.id == id && !series.removed)
                        .is_some_and(|series| series.visible);
                    states.push(ChartIndicatorState {
                        indicator: ChartIndicator::Volume,
                        visible,
                    });
                }
                continue;
            }
            let Some(info) = self.engine.indicator_info(id) else {
                continue;
            };
            if !bindings.insert(info.binding_id) {
                continue;
            }
            let Some(indicator) = ChartIndicator::from_nucleus_kind(info.kind) else {
                continue;
            };
            let visible = entries.iter().any(|series| {
                !series.removed
                    && series.visible
                    && self
                        .engine
                        .indicator_info(series.id)
                        .is_some_and(|output| output.binding_id == info.binding_id)
            });
            states.push(ChartIndicatorState { indicator, visible });
        }
        states
    }

    /// Recreates indicator instances captured from an earlier engine for this chart surface.
    ///
    /// # Errors
    ///
    /// Returns the first native indicator creation error.
    pub fn restore_indicator_states(
        &mut self,
        states: &[ChartIndicatorState],
    ) -> Result<(), ChartIndicatorError> {
        for state in states {
            let ids = self.add_indicator(state.indicator)?;
            if state.visible {
                continue;
            }
            let item = if state.indicator == ChartIndicator::Volume {
                LegendItem::Volume
            } else {
                let Some(binding_id) = ids
                    .first()
                    .and_then(|id| self.engine.indicator_info(*id))
                    .map(|info| info.binding_id)
                else {
                    return Err(ChartIndicatorError::CreationRejected(state.indicator));
                };
                LegendItem::Indicator(binding_id)
            };
            let _ = self.set_legend_item_visible(item, false);
        }
        Ok(())
    }

    /// Returns the drawing tool currently armed on the chart surface.
    #[must_use]
    pub const fn drawing_tool(&self) -> ChartDrawingTool {
        self.drawing_tool
    }

    /// Arms a drawing tool, replacing any unfinished drawing gesture.
    ///
    /// Creation stays idle until the first click. Starting it on arm would paint a
    /// pre-click handle that magnet-snaps instead of the OHLC crosshair.
    pub fn set_drawing_tool(&mut self, tool: ChartDrawingTool) {
        self.end_drag(-1.0, -1.0);
        let _ = self.finish_text_edit();
        self.cancel_drawing_gesture();
        self.drawing_tool = tool;
        if tool.drawing_kind().is_none() {
            self.engine.crosshair_ohlc_magnet = false;
        }
        self.cursor_style = CursorStyle::Crosshair;
        self.invalidate_series_frame();
    }

    /// Cancels creation or movement and returns to the cursor tool.
    pub fn cancel_drawing(&mut self) {
        self.end_drag(-1.0, -1.0);
        let _ = self.finish_text_edit();
        self.cancel_drawing_gesture();
        self.drawing_tool = ChartDrawingTool::Cursor;
        self.cursor_style = CursorStyle::Crosshair;
        self.invalidate_series_frame();
    }

    /// Whether the host is editing a Nucleus text drawing.
    #[must_use]
    pub fn is_editing_text(&self) -> bool {
        self.engine.editing_drawing().is_some()
    }

    fn begin_text_edit(&mut self, id: nucleuscharts_engine::DrawingId) {
        self.engine.set_editing_drawing(Some(id));
        self.engine.set_selected_drawing(Some(id));
        self.invalidate_series_frame();
    }

    /// Commit or discard the active text edit. Empty text drawings are removed.
    pub fn finish_text_edit(&mut self) -> bool {
        let Some(id) = self.engine.editing_drawing() else {
            return false;
        };
        self.engine.set_editing_drawing(None);
        let empty = self
            .engine
            .drawings()
            .iter()
            .find(|drawing| drawing.id == id)
            .is_some_and(|drawing| drawing.text.is_empty());
        if empty {
            self.engine.set_selected_drawing(Some(id));
            let _ = self.engine.remove_selected_drawing();
            self.locked_drawings.remove(&id);
        }
        self.invalidate_series_frame();
        true
    }

    fn editing_text_value(&self) -> Option<String> {
        let id = self.engine.editing_drawing()?;
        self.engine
            .drawings()
            .iter()
            .find(|drawing| drawing.id == id)
            .map(|drawing| drawing.text.clone())
    }

    fn set_editing_text_value(&mut self, text: &str) -> bool {
        let Some(id) = self.engine.editing_drawing() else {
            return false;
        };
        let Ok(encoded) = serde_json::to_string(text) else {
            return false;
        };
        let json = format!(r#"{{"text":{encoded}}}"#);
        if !self.engine.drawing_apply_options(id, &json) {
            return false;
        }
        self.invalidate_series_frame();
        true
    }

    fn text_caret_overlay(&self, window: &Window) -> Option<AnyElement> {
        let id = self.engine.editing_drawing()?;
        let drawing = self
            .engine
            .drawings()
            .iter()
            .find(|drawing| drawing.id == id)?;
        let (anchor_x, anchor_y) = self.engine.drawing_point_to_coordinate(id, 0)?;
        let layout = &self.engine.options.get().layout;
        let size = drawing.resolved_text_size(layout.font_size).to_f32()?;
        let metrics = measure_text(
            window,
            &drawing.text,
            &layout.font_family,
            size,
            drawing.text_weight.unwrap_or(400),
            drawing.text_italic,
        );
        let options: serde_json::Value =
            serde_json::from_str(&self.engine.drawing_options_json(id)?).ok()?;
        let (left, top, height) = text_caret_geometry(
            anchor_x.to_f32()? + self.engine.pane_left.to_f32()?,
            anchor_y.to_f32()?,
            metrics.width,
            size,
            options["text_h_align"].as_str().unwrap_or("center"),
            options["text_v_align"].as_str().unwrap_or("middle"),
            drawing.text.is_empty(),
        );
        let color = legend_palette(self.theme).text;
        Some(
            div()
                .id(("chart_text_caret", id))
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(1.5))
                .h(px(height))
                .bg(color)
                .with_animation(
                    ("chart_text_caret_blink", id),
                    Animation::new(TEXT_CARET_PERIOD).repeat(),
                    |caret, delta| caret.opacity(if delta < 0.5 { 1.0 } else { 0.0 }),
                )
                .into_any_element(),
        )
    }

    fn apply_text_edit_key(&mut self, event: &KeyDownEvent) -> bool {
        if !self.is_editing_text() {
            return false;
        }
        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return false;
        }
        match event.keystroke.key.as_str() {
            "escape" | "enter" => self.finish_text_edit(),
            "backspace" => {
                let Some(mut text) = self.editing_text_value() else {
                    return false;
                };
                text.pop();
                self.set_editing_text_value(&text)
            }
            "delete" => {
                // Host typing mode treats Delete like Backspace for the caret-at-end model.
                let Some(mut text) = self.editing_text_value() else {
                    return false;
                };
                text.pop();
                self.set_editing_text_value(&text)
            }
            _ => {
                let Some(ch) = text_edit_char(event) else {
                    return false;
                };
                let Some(mut text) = self.editing_text_value() else {
                    return false;
                };
                text.push(ch);
                self.set_editing_text_value(&text)
            }
        }
    }

    /// Returns the number of committed drawings.
    #[must_use]
    pub fn drawing_count(&self) -> usize {
        self.engine.drawings().len()
    }

    /// Returns the selected drawing identifier, if any.
    #[must_use]
    pub fn selected_drawing_id(&self) -> Option<DrawingId> {
        self.engine.selected_drawing()
    }

    /// Returns whether the selected drawing is locked against pointer movement.
    #[must_use]
    pub fn selected_drawing_locked(&self) -> bool {
        self.engine
            .selected_drawing()
            .is_some_and(|id| self.locked_drawings.contains(&id))
    }

    /// Locks or unlocks the selected drawing against pointer movement.
    pub fn set_selected_drawing_locked(&mut self, locked: bool) -> bool {
        let Some(id) = self.engine.selected_drawing() else {
            return false;
        };
        let changed = if locked {
            self.locked_drawings.insert(id)
        } else {
            self.locked_drawings.remove(&id)
        };
        if changed {
            self.engine.drawing_drag_end();
            self.invalidate_series_frame();
        }
        changed
    }

    /// Returns aggregate lock state for the drawing toolbar.
    #[must_use]
    pub fn drawings_lock_summary(&self) -> DrawingsLockSummary {
        let total = self.engine.drawings().len();
        let locked_count = self
            .engine
            .drawings()
            .iter()
            .filter(|drawing| self.locked_drawings.contains(&drawing.id))
            .count();
        DrawingsLockSummary {
            total,
            locked_count,
            all_locked: total > 0 && total == locked_count,
        }
    }

    /// Locks or unlocks every committed drawing against pointer movement.
    pub fn set_all_drawings_locked(&mut self, locked: bool) -> bool {
        let previous = self.drawings_lock_summary();
        if locked {
            self.locked_drawings
                .extend(self.engine.drawings().iter().map(|drawing| drawing.id));
        } else {
            self.locked_drawings.clear();
        }
        let changed = previous.locked_count != self.drawings_lock_summary().locked_count;
        if changed {
            self.engine.drawing_drag_end();
            self.invalidate_series_frame();
        }
        changed
    }

    /// Removes the selected drawing, if one exists.
    pub fn remove_selected_drawing(&mut self) -> bool {
        let selected = self.engine.selected_drawing();
        let removed = self.engine.remove_selected_drawing();
        if removed {
            if let Some(id) = selected {
                self.locked_drawings.remove(&id);
            }
            self.invalidate_series_frame();
        }
        removed
    }

    /// Returns whether the current chart-object selection can be deleted by product chrome.
    #[must_use]
    pub fn has_deletable_selection(&self) -> bool {
        self.engine.selected_drawing().is_some()
            || self
                .engine
                .selected_series()
                .is_some_and(|series| series != 0)
    }

    /// Removes the selected drawing or native Nucleus indicator/volume series.
    ///
    /// The price series is product-owned and cannot be deleted. Volume is hidden rather than
    /// tombstoned so selecting it again from the indicator catalog can reuse its live data.
    pub fn remove_selected_chart_object(&mut self) -> bool {
        if self.remove_selected_drawing() {
            self.cancel_drawing();
            return true;
        }
        let Some(series) = self.engine.selected_series() else {
            return false;
        };
        if series == 0 {
            return false;
        }
        if series == self.volume_series {
            self.engine.set_series_visible(series, false);
            self.volume_legend = LegendPresence::Absent;
            self.engine.set_selected_series(None);
        } else if !self.engine.remove_series(series) {
            return false;
        }
        self.invalidate_series_layout();
        true
    }

    /// Returns whether any native indicator or the reusable volume series is currently shown.
    #[must_use]
    pub fn has_indicators(&self) -> bool {
        self.engine.series_entries().iter().any(|series| {
            !series.removed
                && series.id != 0
                && (series.id != self.volume_series || self.volume_legend.is_present())
        })
    }

    /// Removes every native indicator and hides the reusable volume series.
    ///
    /// The product-owned price series is left in place. Volume stays allocated so the catalog can
    /// show it again without rebuilding live weights.
    pub fn clear_indicators(&mut self) -> bool {
        if !self.has_indicators() {
            return false;
        }
        if self
            .engine
            .selected_series()
            .is_some_and(|series| series != 0)
        {
            self.engine.set_selected_series(None);
        }
        let ids: Vec<u32> = self
            .engine
            .series_entries()
            .iter()
            .filter(|series| !series.removed && series.id != 0)
            .map(|series| series.id)
            .collect();
        for id in ids {
            if id == self.volume_series {
                self.engine.set_series_visible(id, false);
                self.volume_legend = LegendPresence::Absent;
            } else {
                let _ = self.engine.remove_series(id);
            }
        }
        self.invalidate_series_layout();
        true
    }

    fn legend_rows(&self) -> Vec<LegendRow> {
        let logical_index = self
            .engine
            .crosshair
            .map(|(x, _)| self.engine.time_scale.coordinate_to_index(x));
        let snapshots = self.engine.value_snapshot(logical_index);
        let entries = self.engine.series_entries();
        let mut rows = Vec::new();
        if let Some(asset) = entries
            .iter()
            .find(|series| series.id == 0 && !series.removed)
        {
            let snapshot = snapshots.iter().find(|snapshot| snapshot.series_id == 0);
            let (values, values_tone) = if asset.visible && self.chart_type.shows_ohlc_legend() {
                (
                    snapshot
                        .map(|snapshot| {
                            let value = |label: &str, value: &Option<String>| {
                                format!("{label} {}", value.as_deref().unwrap_or("--"))
                            };
                            [
                                value("O", &snapshot.formatted_open),
                                value("H", &snapshot.formatted_high),
                                value("L", &snapshot.formatted_low),
                                value("C", &snapshot.formatted_close),
                            ]
                            .join("  ")
                        })
                        .unwrap_or_default(),
                    snapshot.map_or(LegendValueTone::Neutral, asset_legend_value_tone),
                )
            } else {
                (String::new(), LegendValueTone::Neutral)
            };
            rows.push(LegendRow {
                item: LegendItem::Asset,
                pane: asset.pane_index,
                title: if !self.asset_legend_title.is_empty() {
                    self.asset_legend_title.clone()
                } else if asset.title.is_empty() {
                    "Asset".to_string()
                } else {
                    asset.title.clone()
                },
                values,
                values_tone,
                visible: asset.visible,
            });
        }
        if self.volume_legend.is_present()
            && let Some(volume) = entries
                .iter()
                .find(|series| series.id == self.volume_series && !series.removed)
        {
            rows.push(LegendRow {
                item: LegendItem::Volume,
                pane: volume.pane_index,
                title: "Volume".to_string(),
                values: if volume.visible {
                    legend_series_value(&snapshots, self.volume_series)
                } else {
                    String::new()
                },
                values_tone: LegendValueTone::Neutral,
                visible: volume.visible,
            });
        }

        let mut bindings = HashSet::new();
        for &id in self.engine.series_order() {
            let Some(info) = self.engine.indicator_info(id) else {
                continue;
            };
            if !bindings.insert(info.binding_id) {
                continue;
            }
            let outputs: Vec<_> = entries
                .iter()
                .filter(|series| {
                    !series.removed
                        && self
                            .engine
                            .indicator_info(series.id)
                            .is_some_and(|output| output.binding_id == info.binding_id)
                })
                .collect();
            let Some(first) = outputs.first() else {
                continue;
            };
            let visible = outputs.iter().any(|series| series.visible);
            let values = if visible {
                outputs
                    .iter()
                    .filter(|series| series.visible)
                    .filter_map(|series| {
                        let output = self.engine.indicator_info(series.id)?;
                        let value = legend_series_value(&snapshots, series.id);
                        if value.is_empty() {
                            None
                        } else if output.output_count > 1 {
                            Some(format!("{} {value}", output.output_name))
                        } else {
                            Some(value)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("  ")
            } else {
                String::new()
            };
            rows.push(LegendRow {
                item: LegendItem::Indicator(info.binding_id),
                pane: first.pane_index,
                title: first.title.clone(),
                values,
                values_tone: LegendValueTone::Neutral,
                visible,
            });
        }
        rows
    }

    fn set_legend_item_visible(&mut self, item: LegendItem, visible: bool) -> bool {
        let ids: Vec<u32> = match item {
            LegendItem::Asset => vec![0],
            LegendItem::Volume if self.volume_legend.is_present() => vec![self.volume_series],
            LegendItem::Volume => return false,
            LegendItem::Indicator(binding) => self
                .engine
                .series_order()
                .iter()
                .copied()
                .filter(|&id| {
                    self.engine
                        .indicator_info(id)
                        .is_some_and(|info| info.binding_id == binding)
                })
                .collect(),
        };
        if ids.is_empty() {
            return false;
        }
        let changed = ids.iter().any(|&id| {
            self.engine
                .series_entries()
                .iter()
                .any(|series| series.id == id && !series.removed && series.visible != visible)
        });
        for id in ids {
            self.engine.set_series_visible(id, visible);
        }
        if changed {
            self.invalidate_series_layout();
        }
        changed
    }

    fn remove_legend_indicator(&mut self, item: LegendItem) -> bool {
        let removed = match item {
            LegendItem::Volume if self.volume_legend.is_present() => {
                self.volume_legend = LegendPresence::Absent;
                self.engine.set_series_visible(self.volume_series, false);
                true
            }
            LegendItem::Asset | LegendItem::Volume => false,
            LegendItem::Indicator(binding) => self.engine.remove_series(binding),
        };
        if removed {
            self.invalidate_series_layout();
        }
        removed
    }

    /// Removes every committed drawing.
    pub fn clear_drawings(&mut self) {
        self.engine.set_editing_drawing(None);
        self.cancel_drawing_gesture();
        self.engine.clear_drawings();
        self.locked_drawings.clear();
        self.invalidate_series_frame();
    }

    /// Returns whether a committed drawing edit can be reversed.
    #[must_use]
    pub fn can_undo_drawing(&self) -> bool {
        self.engine.can_undo_drawing()
    }

    /// Returns whether a reversed drawing edit can be reapplied.
    #[must_use]
    pub fn can_redo_drawing(&self) -> bool {
        self.engine.can_redo_drawing()
    }

    /// Reverses the newest committed drawing edit.
    pub fn undo_drawing(&mut self) -> bool {
        self.step_drawing_history(true)
    }

    /// Reapplies the newest reversed drawing edit.
    pub fn redo_drawing(&mut self) -> bool {
        self.step_drawing_history(false)
    }

    /// Steps the engine's chart-local drawing history one command in either direction.
    ///
    /// An in-flight gesture is settled first: the engine drops its own pending creation and brush
    /// capture on a step. The toolbar selection stays armed; the next click starts a fresh
    /// placement so Ctrl magnet can still snap the crosshair instead of a leftover handle.
    /// Locks are keyed by drawing id and are host state, so a tombstoned id is dropped and a
    /// restored drawing comes back unlocked.
    fn step_drawing_history(&mut self, undo: bool) -> bool {
        let _ = self.finish_text_edit();
        self.cancel_drawing_gesture();
        let stepped = if undo {
            self.engine.undo_drawing()
        } else {
            self.engine.redo_drawing()
        };
        if !stepped {
            return false;
        }
        self.locked_drawings.retain(|locked| {
            self.engine
                .drawings()
                .iter()
                .any(|drawing| drawing.id == *locked)
        });
        self.invalidate_series_frame();
        true
    }

    /// Returns whether the newest bar is aligned to the real-time edge.
    #[must_use]
    pub fn is_at_latest(&self) -> bool {
        self.engine.scroll_position().abs() < f64::EPSILON
    }

    /// Returns whether a provider snapshot has populated the chart surface.
    #[must_use]
    pub const fn has_market_data(&self) -> bool {
        self.data_bridge.is_some()
    }

    /// Enqueues one replay update for the next chart frame.
    ///
    /// # Errors
    ///
    /// Returns the unchanged update if the bounded bridge is full.
    pub fn try_queue_replay_update(
        &mut self,
        update: ReplayStreamUpdate,
    ) -> Result<(), Box<ReplayStreamUpdate>> {
        let Some(bridge) = &mut self.data_bridge else {
            return Err(Box::new(update));
        };
        bridge.try_push(update)
    }

    /// Returns the number of replay commands waiting for the next frame.
    #[must_use]
    pub fn queued_replay_update_count(&self) -> usize {
        self.data_bridge
            .as_ref()
            .map_or(0, ChartDataBridge::queued_update_count)
    }

    /// Returns the next sequence expected by the chart bridge.
    #[must_use]
    pub fn expected_replay_sequence(&self) -> Option<u64> {
        self.data_bridge
            .as_ref()
            .and_then(ChartDataBridge::expected_sequence)
    }

    /// Returns bounded queue and resnapshot telemetry for this chart subscription.
    #[must_use]
    pub fn replay_bridge_metrics(&self) -> ChartBridgeMetrics {
        self.data_bridge
            .as_ref()
            .map_or_else(ChartBridgeMetrics::default, ChartDataBridge::metrics)
    }

    /// Takes the most recent foreground duration for installing one queued snapshot.
    #[cfg(feature = "diagnostics")]
    pub fn take_snapshot_installation_nanos(&mut self) -> Option<u64> {
        self.last_snapshot_installation_nanos.take()
    }

    /// Returns the CSS viewport dimensions observed by the most recent native canvas prepaint.
    #[cfg(feature = "diagnostics")]
    #[must_use]
    pub const fn rendered_viewport_size(&self) -> (f32, f32) {
        (self.built_for.0, self.built_for.1)
    }

    /// Offers recovery to a bounded worker queue and marks dispatch only after acceptance.
    ///
    /// # Errors
    ///
    /// Returns the worker queue's rejection without changing bridge dispatch state.
    pub fn try_dispatch_replay_recovery<DispatchError>(
        &mut self,
        dispatch: impl FnOnce(ReplayRecoveryCommand) -> Result<(), DispatchError>,
    ) -> Result<bool, DispatchError> {
        let Some(bridge) = &mut self.data_bridge else {
            return Ok(false);
        };
        bridge.try_dispatch_recovery(dispatch)
    }

    /// Installs a response only when its request ID matches the active dispatched recovery.
    ///
    /// # Errors
    ///
    /// Returns an error if the correlated snapshot cannot establish resumable chart state.
    pub fn install_replay_recovery(
        &mut self,
        request_id: u64,
        replay: &ReplaySnapshot,
    ) -> Result<bool, ReplayValidationError> {
        let Some(bridge) = &mut self.data_bridge else {
            return Ok(false);
        };
        if !bridge.install_recovery_snapshot(request_id, replay)? {
            return Ok(false);
        }
        install_replay(
            &mut self.engine,
            self.volume_series,
            replay,
            self.chart_type,
            &mut self.product_bars,
        );
        self.apply_price_series_kind();
        self.displayed_provenance.replace_snapshot(replay);
        self.asset_legend_title = replay_legend_title(replay);
        self.price_divisor = replay_price_divisor(replay);
        self.instrument_price_precision = replay.instrument().precision.price_scale();
        self.apply_selected_price_format();
        self.invalidate_series_layout();
        Ok(true)
    }

    /// Records failure only for the active correlated recovery request.
    pub fn mark_replay_recovery_failed(&mut self, request_id: u64) -> bool {
        self.data_bridge
            .as_mut()
            .is_some_and(|bridge| bridge.mark_recovery_failed(request_id))
    }

    /// Blocks ordered chart updates and requests a correlated replacement snapshot.
    pub fn mark_replay_stream_invalid(&mut self) {
        if let Some(bridge) = &mut self.data_bridge {
            bridge.mark_stream_invalid();
        }
    }

    /// Returns canonical evidence for the latest value installed into Nucleus.
    #[must_use]
    pub fn latest_market_provenance(&self) -> Option<&MarketEventProvenance> {
        self.displayed_provenance.latest()
    }

    fn apply_pending_data(&mut self) -> SeriesMutation {
        let Some(bridge) = &mut self.data_bridge else {
            return SeriesMutation::None;
        };
        match bridge.drain_merged() {
            Ok(Some(update)) if update.mutates_series() => {
                #[cfg(feature = "diagnostics")]
                let snapshot_install_started = update.snapshot().map(|_| Instant::now());
                let previous_timestamp = self
                    .displayed_provenance
                    .latest()
                    .map(|provenance| provenance.exchange_timestamp_unix_nanos);
                let mutation = if update.snapshot().is_some() {
                    SeriesMutation::Snapshot
                } else if update.accepted_deltas().iter().any(|item| {
                    previous_timestamp.is_none_or(|previous| {
                        item.provenance().exchange_timestamp_unix_nanos > previous
                    })
                }) {
                    SeriesMutation::Append
                } else {
                    SeriesMutation::TailReplace
                };
                if let Some(snapshot) = update.snapshot() {
                    self.displayed_provenance.replace_snapshot(snapshot);
                    self.asset_legend_title = replay_legend_title(snapshot);
                    self.instrument_price_precision = snapshot.instrument().precision.price_scale();
                }
                self.displayed_provenance.extend(update.accepted_deltas());
                apply_merged_chart_data(
                    &mut self.engine,
                    self.volume_series,
                    &mut self.price_divisor,
                    self.chart_type,
                    &mut self.product_bars,
                    &update,
                );
                if mutation == SeriesMutation::Snapshot {
                    self.sync_brushable_interaction();
                    self.apply_price_series_kind();
                    self.apply_selected_price_format();
                }
                if mutation == SeriesMutation::TailReplace {
                    self.invalidate_series_frame();
                } else {
                    self.invalidate_series_layout();
                }
                #[cfg(feature = "diagnostics")]
                if let Some(started) = snapshot_install_started {
                    self.last_snapshot_installation_nanos =
                        Some(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
                }
                mutation
            }
            Ok(_) => SeriesMutation::None,
            Err(error) => {
                bridge.mark_stream_invalid();
                eprintln!("replay update rejected; snapshot required: {error}");
                SeriesMutation::None
            }
        }
    }

    fn invalidate_series_frame(&mut self) {
        self.frame = ChartFrame::default();
        self.axis_prims.clear();
    }

    fn pin_host_clock(&mut self) {
        let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
            return;
        };
        self.engine.set_now_seconds(now.as_secs_f64());
    }

    fn apply_selected_price_format(&mut self) {
        let precision = self
            .price_precision_override
            .unwrap_or(self.instrument_price_precision)
            .min(18);
        let min_move = price_format_min_move(precision);
        let json = format!(r#"{{"type":"price","precision":{precision},"min_move":{min_move}}}"#);
        let applied = self.engine.series_apply_price_format_json(0, &json);
        debug_assert!(applied);
    }

    fn primary_series_id_on_scale(&self, pane: usize, target: PriceScaleTarget) -> Option<u32> {
        self.primary_series_on_scale(pane, target)
            .map(|(id, ..)| id)
    }

    fn primary_series_on_scale(
        &self,
        pane: usize,
        target: PriceScaleTarget,
    ) -> Option<(u32, bool, bool, bool, bool, bool)> {
        let mut fallback = None;
        for series in self.engine.series_entries() {
            if series.removed || series.pane_index != pane || series.price_scale_target != target {
                continue;
            }
            let chrome = (
                series.id,
                series.price_line_visible,
                series.last_value_visible,
                series.title_visible,
                series.countdown_visible,
                series.bid_ask_visible,
            );
            if series.id == 0 {
                return Some(chrome);
            }
            if series.visible && fallback.is_none() {
                fallback = Some(chrome);
            }
        }
        fallback
    }

    fn indicator_series_ids(&self) -> Vec<u32> {
        self.engine
            .series_entries()
            .iter()
            .filter(|series| {
                !series.removed
                    && series.id != 0
                    && (series.id != self.volume_series || series.visible)
            })
            .map(|series| series.id)
            .collect()
    }

    fn product_price_line_visible(&self) -> bool {
        self.engine
            .series_entries()
            .iter()
            .any(|series| series.id == 0 && !series.removed && series.price_line_visible)
    }

    fn apply_indicator_chrome_options(&mut self) {
        let names = self.indicator_name_labels.visible();
        let values = self.indicator_value_labels.visible();
        let price_lines = self.indicator_price_lines.visible();
        let json = format!(
            r#"{{"last_value_visible":{values},"title_visible":{names},"price_line_visible":{price_lines}}}"#
        );
        for id in self.indicator_series_ids() {
            let _ = self.engine.series_apply_options_json(id, &json);
        }
    }

    /// Host-owned price-series chart type forwarded to Nucleus.
    #[must_use]
    pub const fn chart_type(&self) -> ChartType {
        self.chart_type
    }

    /// Applies a built-in Nucleus price-series kind without changing market data.
    pub fn set_chart_type(&mut self, chart_type: ChartType) {
        self.chart_type = chart_type;
        self.apply_price_series_kind();
    }

    fn apply_price_series_kind(&mut self) {
        if self.chart_type != ChartType::BrushableArea {
            self.teardown_brushable_interaction();
        }
        install_product_price_series(&mut self.engine, self.chart_type, &self.product_bars);
        self.sync_brushable_interaction();
        self.invalidate_series_layout();
    }

    fn sync_brushable_interaction(&mut self) {
        if self.chart_type != ChartType::BrushableArea {
            self.teardown_brushable_interaction();
            return;
        }
        if self.brushable_tooltip.is_none() {
            self.brushable_tooltip = self
                .engine
                .add_delta_tooltip(0, DeltaTooltipOptions::default());
        }
        self.sync_brushable_range();
    }

    fn teardown_brushable_interaction(&mut self) {
        if let Some(id) = self.brushable_tooltip.take() {
            let _ = self.engine.clear_delta_tooltip(id);
            let _ = self.engine.remove_native_primitive(id);
        }
        if matches!(self.drag, Some(ChartDrag::BrushableRange)) {
            self.drag = None;
        }
    }

    fn sync_brushable_range(&mut self) {
        let Some(id) = self.brushable_tooltip else {
            return;
        };
        let (base_style, ranges) = match self.engine.delta_tooltip_active_range(id) {
            Some(range) => (
                Self::brush_style(BRUSHABLE_LINE, 51, 13, 2.0),
                vec![BrushRange {
                    from: f64::from(i32::try_from(range.from).unwrap_or(0)),
                    to: f64::from(i32::try_from(range.to).unwrap_or(0)),
                    style: if range.positive {
                        Self::brush_style(BRUSHABLE_UP, 255, 102, 3.0)
                    } else {
                        Self::brush_style(BRUSHABLE_DOWN, 255, 102, 3.0)
                    },
                }],
            ),
            None => (Self::brush_style(BRUSHABLE_LINE, 255, 102, 2.0), Vec::new()),
        };
        let _ = self.engine.apply_feature_series_options(
            0,
            FeatureSeriesOptionsPatch {
                line_color: Some(base_style.line_color),
                top_color: Some(base_style.top_color),
                bottom_color: Some(base_style.bottom_color),
                line_width: Some(base_style.line_width),
                brush_ranges: Some(ranges),
                ..FeatureSeriesOptionsPatch::default()
            },
        );
    }

    const fn brush_style(
        rgb: (u8, u8, u8),
        line_alpha: u8,
        top_alpha: u8,
        width: f64,
    ) -> BrushStyle {
        BrushStyle {
            line_color: Color::rgba(rgb.0, rgb.1, rgb.2, line_alpha),
            top_color: Color::rgba(rgb.0, rgb.1, rgb.2, top_alpha),
            bottom_color: Color::rgba(rgb.0, rgb.1, rgb.2, 0),
            line_width: width,
        }
    }

    fn begin_brushable_range(&mut self, pane_x: f64, y: f64) {
        self.end_drag(pane_x, y);
        if self.engine.delta_tooltip_mouse_down(pane_x) {
            self.drag = Some(ChartDrag::BrushableRange);
            self.sync_brushable_range();
        }
        self.update_cursor(pane_x, y);
        self.update_crosshair(pane_x, y);
    }

    fn clear_brushable_range(&mut self) {
        if let Some(id) = self.brushable_tooltip {
            let _ = self.engine.clear_delta_tooltip(id);
            self.sync_brushable_range();
        }
    }

    /// Host-owned indicator name-chip chrome for every native indicator on this chart.
    #[must_use]
    pub const fn indicator_name_labels_visible(&self) -> bool {
        self.indicator_name_labels.visible()
    }

    /// Host-owned indicator last-value chrome for every native indicator on this chart.
    #[must_use]
    pub const fn indicator_value_labels_visible(&self) -> bool {
        self.indicator_value_labels.visible()
    }

    /// Host-owned last-value line chrome for every native indicator plot on this chart.
    #[must_use]
    pub const fn indicator_price_lines_visible(&self) -> bool {
        self.indicator_price_lines.visible()
    }

    /// Applies indicator name, value, and price-line preferences to current and later indicators.
    pub fn apply_indicator_chrome_preferences(
        &mut self,
        names: bool,
        values: bool,
        price_lines: bool,
    ) {
        self.indicator_name_labels = IndicatorLabels::from_visible(names);
        self.indicator_value_labels = IndicatorLabels::from_visible(values);
        self.indicator_price_lines = IndicatorLabels::from_visible(price_lines);
        self.apply_indicator_chrome_options();
        self.invalidate_series_layout();
    }

    fn toggle_indicator_name_labels(&mut self) -> bool {
        self.apply_indicator_chrome_preferences(
            !self.indicator_name_labels.visible(),
            self.indicator_value_labels.visible(),
            self.indicator_price_lines.visible(),
        );
        true
    }

    fn toggle_indicator_value_labels(&mut self) -> bool {
        self.apply_indicator_chrome_preferences(
            self.indicator_name_labels.visible(),
            !self.indicator_value_labels.visible(),
            self.indicator_price_lines.visible(),
        );
        true
    }

    fn toggle_indicator_price_lines(&mut self) -> bool {
        self.apply_indicator_chrome_preferences(
            self.indicator_name_labels.visible(),
            self.indicator_value_labels.visible(),
            !self.indicator_price_lines.visible(),
        );
        true
    }

    fn toggle_series_flag(&mut self, id: u32, key: &str) -> bool {
        let current = self.engine.series_entries().iter().find_map(|series| {
            if series.id != id || series.removed {
                return None;
            }
            match key {
                "price_line_visible" => Some(series.price_line_visible),
                "last_value_visible" => Some(series.last_value_visible),
                "title_visible" => Some(series.title_visible),
                "countdown_visible" => Some(series.countdown_visible),
                "bid_ask_visible" => Some(series.bid_ask_visible),
                _ => None,
            }
        });
        let Some(current) = current else {
            return false;
        };
        self.engine
            .series_apply_options_json(id, &format!(r#"{{"{key}":{}}}"#, !current))
    }

    fn toggle_price_scale_flag(
        &mut self,
        pane: usize,
        target: PriceScaleTarget,
        key: &str,
        default: bool,
    ) -> bool {
        let current = self
            .engine
            .price_scale_options_json(pane, target)
            .as_deref()
            .and_then(|options| json_bool(options, key))
            .unwrap_or(default);
        self.engine.price_scale_apply_options_json(
            pane,
            target,
            &format!(r#"{{"{key}":{}}}"#, !current),
        )
    }

    fn move_price_axis(&mut self, pane: usize, from_left: bool, to_left: bool) -> bool {
        if from_left == to_left {
            return true;
        }
        let from = price_axis_target(from_left);
        let to = price_axis_target(to_left);
        let ids: Vec<u32> = self
            .engine
            .series_entries()
            .iter()
            .filter(|series| {
                !series.removed && series.pane_index == pane && series.price_scale_target == from
            })
            .map(|series| series.id)
            .collect();
        if ids.is_empty() {
            return false;
        }
        let options = self.engine.price_scale_options_json(pane, from);
        for id in ids {
            self.engine.set_series_price_scale(id, to);
        }
        if !self.engine.set_price_scale_visible_for(pane, to, true) {
            return false;
        }
        if let Some(options) = options {
            let _ = self
                .engine
                .price_scale_apply_options_json(pane, to, &options);
        }
        let still_used = self.engine.series_entries().iter().any(|series| {
            !series.removed && series.pane_index == pane && series.price_scale_target == from
        });
        if !still_used {
            let _ = self.engine.set_price_scale_visible_for(pane, from, false);
        }
        true
    }

    fn set_price_precision(&mut self, precision: Option<u8>) -> bool {
        if let Some(digits) = precision
            && !PRICE_AXIS_PRECISION_CHOICES.contains(&digits)
        {
            return false;
        }
        self.price_precision_override = precision;
        self.apply_selected_price_format();
        true
    }

    fn invalidate_series_layout(&mut self) {
        self.invalidate_series_frame();
        self.layout_dirty = true;
    }

    fn local_position(&self, position: gpui::Point<gpui::Pixels>) -> (f64, f64) {
        let window_x: f32 = position.x.into();
        let window_y: f32 = position.y.into();
        let chart_x = f64::from(window_x - self.viewport_origin.0);
        let y = f64::from(window_y - self.viewport_origin.1);
        (chart_x - self.engine.pane_left, y)
    }

    fn drawing_modifiers(modifiers: Modifiers) -> DrawingModifiers {
        DrawingModifiers {
            magnet: modifiers.control || modifiers.platform,
            straighten: modifiers.shift,
        }
    }

    fn update_crosshair_magnet(&mut self, magnet: bool) {
        let enabled = magnet && self.drawing_tool.drawing_kind().is_some();
        if self.engine.crosshair_ohlc_magnet != enabled {
            self.engine.crosshair_ohlc_magnet = enabled;
            self.invalidate_series_frame();
        }
    }

    fn place_drawing_anchor(&mut self, x: f64, y: f64, modifiers: DrawingModifiers) -> i64 {
        let Some(kind) = self
            .drawing_tool
            .drawing_kind()
            .filter(|kind| *kind != DrawingKind::Brush)
        else {
            return 0;
        };
        if !self.engine.drawing_create_active() && !self.engine.drawing_create_begin(kind, None) {
            return 0;
        }
        self.engine.drawing_create_click(x, y, modifiers)
    }

    fn cancel_drawing_gesture(&mut self) {
        self.engine.drawing_drag_end();
        self.engine.drawing_create_cancel();
        self.engine.brush_create_cancel();
        self.pending_brush_point = None;
    }

    /// Capture at most the newest brush sample per painted frame.
    ///
    /// Wayland delivers per-HID-report motion, often one axis per event. Adding
    /// every sample records that staircase as stroke knots. Browser hosts
    /// coalesce to display frames; this GPUI host does the same.
    fn flush_pending_brush(&mut self) -> bool {
        let Some((x, y)) = self.pending_brush_point.take() else {
            return false;
        };
        if !self.engine.brush_create_active() {
            return false;
        }
        if self.engine.brush_create_add(x, y) {
            self.invalidate_series_frame();
            true
        } else {
            false
        }
    }

    fn drawing_pointer_down(
        &mut self,
        pane_x: f64,
        y: f64,
        modifiers: DrawingModifiers,
        click_count: usize,
    ) -> bool {
        if self.is_editing_text() {
            let editing = self.engine.editing_drawing();
            let hit_id = self.engine.hit_test_drawing(pane_x, y).map(|hit| hit.id);
            if click_count >= 2
                && let Some(id) = hit_id.filter(|&id| self.drawing_is_text(id))
            {
                self.begin_text_edit(id);
                return true;
            }
            if hit_id != editing {
                let _ = self.finish_text_edit();
            } else if hit_id.is_some() {
                // Keep typing focus on the current text drawing; avoid starting a drag.
                return true;
            }
        }

        match self.drawing_tool {
            ChartDrawingTool::Cursor => {
                let hit = self.engine.hit_test_drawing(pane_x, y);
                if click_count >= 2
                    && let Some(hit) = hit
                    && self.drawing_is_text(hit.id)
                {
                    self.begin_text_edit(hit.id);
                    return true;
                }
                if let Some(hit) = hit
                    && self.locked_drawings.contains(&hit.id)
                {
                    self.engine.set_selected_drawing(Some(hit.id));
                    self.engine.drawing_drag_end();
                    return true;
                }
                if self.engine.drawing_drag_start_at(pane_x, y) {
                    return true;
                }
                self.engine.set_selected_drawing(None);
                false
            }
            ChartDrawingTool::Brush => self.engine.brush_create_start(None, pane_x, y),
            tool => {
                let placing_text = tool == ChartDrawingTool::Text;
                let result = self.place_drawing_anchor(pane_x, y, modifiers);
                if result > 0 {
                    self.drawing_tool = ChartDrawingTool::Cursor;
                    self.cursor_style = CursorStyle::Crosshair;
                    self.engine.crosshair_ohlc_magnet = false;
                    if placing_text && let Ok(id) = DrawingId::try_from(result) {
                        self.begin_text_edit(id);
                    }
                }
                result != 0
            }
        }
    }

    fn drawing_is_text(&self, id: DrawingId) -> bool {
        self.engine
            .drawings()
            .iter()
            .any(|drawing| drawing.id == id && drawing.kind == DrawingKind::Text)
    }

    fn drawing_pointer_move(
        &mut self,
        pane_x: f64,
        y: f64,
        dragging: bool,
        modifiers: DrawingModifiers,
    ) -> bool {
        if self.engine.brush_create_active() {
            if dragging {
                self.pending_brush_point = Some((pane_x, y));
            } else {
                self.pending_brush_point = None;
                self.engine.brush_create_cancel();
            }
            return true;
        }
        if self.engine.drawing_drag_active() {
            if dragging {
                self.engine.drawing_drag_to(pane_x, y, modifiers);
            } else {
                self.engine.drawing_drag_end();
            }
            return true;
        }
        if self.engine.drawing_create_active() {
            self.engine.drawing_create_move(pane_x, y, modifiers);
            return true;
        }
        false
    }

    fn drawing_pointer_up(&mut self, pane_x: f64, y: f64, modifiers: DrawingModifiers) -> bool {
        if self.engine.brush_create_active() {
            self.pending_brush_point = Some((pane_x, y));
            self.flush_pending_brush();
            self.engine.brush_create_end();
            self.drawing_tool = ChartDrawingTool::Cursor;
            self.cursor_style = CursorStyle::Crosshair;
            self.engine.crosshair_ohlc_magnet = false;
            return true;
        }
        if self.engine.drawing_drag_active() {
            self.engine.drawing_drag_to(pane_x, y, modifiers);
            self.engine.drawing_drag_end();
            return true;
        }
        false
    }

    fn update_crosshair(&mut self, pane_x: f64, y: f64) {
        self.engine.crosshair = (self.separator_at(y).is_none()
            && pane_x >= 0.0
            && pane_x <= self.engine.pane_w
            && y >= 0.0
            && y <= self.engine.pane_h)
            .then_some((pane_x, y));
        self.invalidate_series_frame();
    }

    fn separator_at(&self, y: f64) -> Option<usize> {
        self.engine
            .panes
            .iter()
            .skip(1)
            .position(|pane| (y - pane.top).abs() <= PANE_SEPARATOR_HIT)
    }

    fn update_cursor(&mut self, pane_x: f64, y: f64) {
        let active_separator = matches!(self.drag, Some(ChartDrag::PaneSeparator { .. }));
        let separator = self.separator_at(y);
        let separator_hover = (!active_separator).then_some(separator).flatten();
        if self.engine.separator_hover != separator_hover {
            self.engine.set_separator_hover(separator_hover);
            self.invalidate_series_frame();
        }
        let drawing_cursor = (self.drawing_tool == ChartDrawingTool::Cursor
            && self.drag.is_none()
            && separator.is_none())
        .then(|| self.engine.hit_test_drawing(pane_x, y))
        .flatten()
        .map(|hit| hit.cursor);
        let hovered_series = (self.drawing_tool == ChartDrawingTool::Cursor
            && self.drag.is_none()
            && separator.is_none()
            && drawing_cursor.is_none())
        .then(|| self.engine.hit_test_series(pane_x, y))
        .flatten();
        if self.engine.hovered_series() != hovered_series {
            self.engine.set_hovered_series(hovered_series);
            self.invalidate_series_frame();
        }
        self.cursor_style = if active_separator || separator.is_some() {
            CursorStyle::ResizeRow
        } else if self.engine.drawing_drag_active() {
            CursorStyle::ClosedHand
        } else if self.drawing_tool != ChartDrawingTool::Cursor {
            CursorStyle::Crosshair
        } else if let Some(cursor) = drawing_cursor {
            match cursor {
                "pointer" => CursorStyle::PointingHand,
                "move" => CursorStyle::OpenHand,
                "ns-resize" => CursorStyle::ResizeUpDown,
                "ew-resize" => CursorStyle::ResizeLeftRight,
                "nwse-resize" => CursorStyle::ResizeUpRightDownLeft,
                "nesw-resize" => CursorStyle::ResizeUpLeftDownRight,
                _ => CursorStyle::Crosshair,
            }
        } else if hovered_series.is_some() {
            CursorStyle::PointingHand
        } else {
            match self.drag {
                Some(ChartDrag::Pane { .. }) => CursorStyle::ClosedHand,
                Some(ChartDrag::TimeAxis) => CursorStyle::ResizeLeftRight,
                Some(ChartDrag::PriceAxis { .. }) => CursorStyle::ResizeUpDown,
                Some(ChartDrag::PaneSeparator { .. }) => CursorStyle::ResizeRow,
                None if y > self.engine.pane_h => CursorStyle::ResizeLeftRight,
                None if self.price_axis_at(pane_x, y).is_some() => CursorStyle::ResizeUpDown,
                Some(ChartDrag::BrushableRange) | None => CursorStyle::Crosshair,
            }
        };
    }

    fn select_series_at(&mut self, pane_x: f64, y: f64) -> bool {
        let selected = self.engine.hit_test_series(pane_x, y);
        let previous_series = self.engine.selected_series();
        let previous_drawing = self.engine.selected_drawing();
        self.engine.set_selected_series(selected);
        if selected.is_some() {
            self.engine.set_selected_drawing(None);
        }
        if previous_series != selected || (selected.is_some() && previous_drawing.is_some()) {
            self.invalidate_series_frame();
        }
        selected.is_some()
    }

    fn pointer_on_axis(&self, pane_x: f64, y: f64) -> bool {
        y > self.engine.pane_h || self.price_axis_at(pane_x, y).is_some()
    }

    fn price_axis_at(&self, pane_x: f64, y: f64) -> Option<(usize, PriceScaleTarget)> {
        if y > self.engine.pane_h {
            return None;
        }
        let pane = self.engine.pane_index_at_y(y);
        self.engine
            .price_axis_target_at(pane, pane_x)
            .map(|target| (pane, target))
    }

    fn unlocked_price_pan_target(&self, pane: usize) -> Option<PriceScaleTarget> {
        [PriceScaleTarget::Right, PriceScaleTarget::Left]
            .into_iter()
            .find(|&target| {
                self.engine.price_scale_auto_scale_for(pane, target) == Some(false)
                    && self.engine.price_axis_scalable(pane, target)
            })
    }

    fn begin_price_axis_scale(&mut self, pane: usize, target: PriceScaleTarget, y: f64) {
        self.engine
            .set_price_scale_auto_scale_for(pane, target, false);
        self.engine.price_axis_start_scale(pane, target, y);
        self.drag = Some(ChartDrag::PriceAxis { pane, target });
    }

    fn begin_drag(&mut self, pane_x: f64, y: f64, click_count: usize) {
        self.end_drag(pane_x, y);
        let pane = self.engine.pane_index_at_y(y);
        if click_count >= 2 {
            if y > self.engine.pane_h {
                self.engine.reset_time_scale();
            } else if self.price_axis_at(pane_x, y).is_some() {
                self.engine.reset_price_scales();
            } else if self.chart_type == ChartType::BrushableArea {
                self.clear_brushable_range();
            }
            self.update_crosshair(pane_x, y);
            return;
        }
        if let Some(index) = self.separator_at(y) {
            self.engine.set_separator_hover(None);
            self.drag = Some(ChartDrag::PaneSeparator { index, last_y: y });
        } else if y > self.engine.pane_h {
            self.engine.time_axis_start_scale(pane_x);
            self.drag = Some(ChartDrag::TimeAxis);
        } else if let Some((pane, target)) = self.price_axis_at(pane_x, y) {
            if self.engine.price_axis_scalable(pane, target) {
                self.begin_price_axis_scale(pane, target, y);
            } else {
                self.drag = None;
            }
        } else if pane_x >= 0.0 && y >= 0.0 && y <= self.engine.pane_h {
            if self.chart_type == ChartType::BrushableArea
                && self.drawing_tool == ChartDrawingTool::Cursor
            {
                self.begin_brushable_range(pane_x, y);
                return;
            }
            self.engine.time_scale.start_scroll(pane_x);
            let price_pan = self
                .engine
                .begin_price_pan_at(pane, pane_x, y)
                .or_else(|| {
                    let target = self.unlocked_price_pan_target(pane)?;
                    self.engine.price_axis_start_scroll(pane, target, y);
                    Some(target)
                })
                .map(|target| (pane, target));
            self.drag = Some(ChartDrag::Pane { price_pan });
        } else {
            self.drag = None;
        }
        self.update_cursor(pane_x, y);
        self.update_crosshair(pane_x, y);
    }

    fn drag_to(&mut self, pane_x: f64, y: f64) {
        match self.drag {
            Some(ChartDrag::Pane { price_pan }) => {
                self.engine.time_scale.scroll_to(pane_x);
                if let Some((pane, target)) = price_pan {
                    self.engine.price_axis_scroll_to(pane, target, y);
                }
            }
            Some(ChartDrag::BrushableRange) => {
                self.engine.delta_tooltip_mouse_move(pane_x);
                self.sync_brushable_range();
            }
            Some(ChartDrag::TimeAxis) => self.engine.time_axis_scale_to(pane_x),
            Some(ChartDrag::PriceAxis { pane, target }) => {
                self.engine.price_axis_scale_to(pane, target, y);
            }
            Some(ChartDrag::PaneSeparator { index, last_y }) => {
                self.engine.drag_pane_separator(index, y - last_y);
                self.drag = Some(ChartDrag::PaneSeparator { index, last_y: y });
                self.invalidate_series_frame();
            }
            None => {}
        }
        self.update_cursor(pane_x, y);
        if matches!(self.drag, Some(ChartDrag::PaneSeparator { .. })) {
            self.engine.crosshair = None;
            self.invalidate_series_frame();
        } else {
            self.update_crosshair(pane_x, y);
        }
    }

    fn end_drag(&mut self, pane_x: f64, y: f64) {
        match self.drag.take() {
            Some(ChartDrag::Pane { price_pan }) => {
                self.engine.time_scale.end_scroll();
                if let Some((pane, target)) = price_pan {
                    self.engine.price_axis_end_scroll(pane, target);
                }
            }
            Some(ChartDrag::BrushableRange) => {
                self.engine.delta_tooltip_mouse_up();
                self.sync_brushable_range();
            }
            Some(ChartDrag::TimeAxis) => self.engine.time_axis_end_scale(),
            Some(ChartDrag::PriceAxis { pane, target }) => {
                self.engine.price_axis_end_scale(pane, target);
            }
            Some(ChartDrag::PaneSeparator { .. }) | None => {}
        }
        self.update_cursor(pane_x, y);
        self.update_crosshair(pane_x, y);
    }

    fn apply_wheel(&mut self, pane_x: f64, y: f64, normalized_x: f64, normalized_y: f64) {
        if normalized_y != 0.0 {
            let zoom = nucleuscharts_engine::wheel_zoom_scale(normalized_y);
            if let Some((pane, target)) = self.price_axis_at(pane_x, y) {
                self.engine.price_axis_wheel_zoom(pane, target, y, zoom);
            } else {
                self.engine.time_scale.zoom(pane_x, zoom);
            }
        }
        if normalized_x != 0.0 {
            self.engine.time_scale.start_scroll(0.0);
            self.engine
                .time_scale
                .scroll_to(nucleuscharts_engine::WHEEL_SCROLL_PX_PER_DELTA * normalized_x);
            self.engine.time_scale.end_scroll();
        }
        self.update_cursor(pane_x, y);
        self.update_crosshair(pane_x, y);
    }

    fn clear_pointer(&mut self, cx: &mut Context<Self>) {
        self.cancel_pointer_gesture();
        cx.notify();
    }

    fn cancel_pointer_gesture(&mut self) {
        self.end_drag(-1.0, -1.0);
        self.engine.drawing_drag_end();
        self.engine.brush_create_cancel();
        self.pending_brush_point = None;
        self.engine.crosshair_ohlc_magnet = false;
        self.engine.crosshair = None;
        self.engine.set_separator_hover(None);
        self.engine.set_hovered_series(None);
        self.cursor_style = CursorStyle::Crosshair;
        self.invalidate_series_frame();
    }

    fn move_pointer(&mut self, pane_x: f64, y: f64, dragging: bool, modifiers: DrawingModifiers) {
        if self.drag.is_some() {
            if dragging {
                self.drag_to(pane_x, y);
            } else {
                self.end_drag(pane_x, y);
            }
            return;
        }
        if self.drawing_pointer_move(pane_x, y, dragging, modifiers) {
            self.update_crosshair(pane_x, y);
            return;
        }
        self.update_cursor(pane_x, y);
        self.update_crosshair(pane_x, y);
    }

    fn apply_key(&mut self, key: &str, accelerated: bool) -> bool {
        let step = if accelerated { 10.0 } else { 1.0 };
        let page =
            (self.engine.pane_w / self.engine.bar_spacing() * KEYBOARD_PAGE_FRACTION).max(1.0);
        let center = self.engine.pane_w / 2.0;
        match key {
            "left" => self
                .engine
                .scroll_to_position(self.engine.scroll_position() - step),
            "right" => self
                .engine
                .scroll_to_position(self.engine.scroll_position() + step),
            "pageup" => self
                .engine
                .scroll_to_position(self.engine.scroll_position() - page),
            "pagedown" => self
                .engine
                .scroll_to_position(self.engine.scroll_position() + page),
            "+" | "=" => self.engine.time_scale.zoom(center, 0.5),
            "-" | "_" => self.engine.time_scale.zoom(center, -0.5),
            "home" => self.reset_view(),
            "end" => self.scroll_to_latest(),
            "delete" | "backspace" => {
                if !self.remove_selected_chart_object() {
                    return false;
                }
            }
            "escape" => self.cancel_drawing(),
            _ => return false,
        }
        self.invalidate_series_frame();
        true
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(feature = "diagnostics")]
        if self.live_evidence_enabled && self.live_evidence_mouse_downs < 8 {
            self.live_evidence_mouse_downs = self.live_evidence_mouse_downs.saturating_add(1);
            let x: f32 = event.position.x.into();
            let y: f32 = event.position.y.into();
            eprintln!("AXIUSFLOW_CHART_MOUSE_DOWN {{\"x\":{x},\"y\":{y}}}");
        }
        if let Some(focus_handle) = &self.focus_handle {
            window.focus(focus_handle, cx);
        }
        self.pending_activate = ActivationRequest::Pending;
        self.update_crosshair_magnet(event.modifiers.control || event.modifiers.platform);
        let (pane_x, y) = self.local_position(event.position);
        if self.separator_at(y).is_some() {
            self.begin_drag(pane_x, y, event.click_count);
        } else if !self.pointer_on_axis(pane_x, y)
            && self.drawing_pointer_down(
                pane_x,
                y,
                Self::drawing_modifiers(event.modifiers),
                event.click_count,
            )
        {
            self.engine.set_selected_series(None);
            self.update_cursor(pane_x, y);
            self.update_crosshair(pane_x, y);
        } else if !self.pointer_on_axis(pane_x, y)
            && self.drawing_tool == ChartDrawingTool::Cursor
            && self.chart_type != ChartType::BrushableArea
            && self.select_series_at(pane_x, y)
        {
            self.update_cursor(pane_x, y);
            self.update_crosshair(pane_x, y);
        } else {
            self.begin_drag(pane_x, y, event.click_count);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn on_context_menu(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(focus_handle) = &self.focus_handle {
            window.focus(focus_handle, cx);
        }
        self.pending_activate = ActivationRequest::Pending;
        let _ = self.finish_text_edit();
        self.cancel_pointer_gesture();
        let (pane_x, y) = self.local_position(event.position);
        let kind = match self.price_axis_at(pane_x, y) {
            Some((pane, PriceScaleTarget::Left)) => {
                ChartContextKind::PriceAxis { pane, left: true }
            }
            Some((pane, PriceScaleTarget::Right)) => {
                ChartContextKind::PriceAxis { pane, left: false }
            }
            _ => ChartContextKind::Pane,
        };
        self.pending_context_menu = Some(ChartContextRequest {
            position: event.position,
            kind,
        });
        cx.stop_propagation();
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.apply_text_edit_key(event) {
            window.prevent_default();
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let modifiers = event.keystroke.modifiers;
        if self.apply_key(
            event.keystroke.key.as_str(),
            modifiers.control || modifiers.shift,
        ) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_crosshair_magnet(event.modifiers.control || event.modifiers.platform);
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_crosshair_magnet(event.modifiers.control || event.modifiers.platform);
        let (pane_x, y) = self.local_position(event.position);
        self.move_pointer(
            pane_x,
            y,
            event.dragging(),
            Self::drawing_modifiers(event.modifiers),
        );
        cx.notify();
    }

    fn finish_mouse_up(&mut self, event: &MouseUpEvent) {
        let (pane_x, y) = self.local_position(event.position);
        if !self.drawing_pointer_up(pane_x, y, Self::drawing_modifiers(event.modifiers)) {
            self.end_drag(pane_x, y);
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.finish_mouse_up(event);
        if should_stop_mouse_up_propagation(false) {
            cx.stop_propagation();
        }
        cx.notify();
    }

    fn on_mouse_up_out(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_mouse_up(event);
        if should_stop_mouse_up_propagation(true) {
            cx.stop_propagation();
        }
        cx.notify();
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (pane_x, y) = self.local_position(event.position);
        let delta = event.delta.pixel_delta(px(WHEEL_LINE_HEIGHT));
        let dx: f32 = delta.x.into();
        let dy: f32 = delta.y.into();
        let normalized_x = f64::from(dx) / 100.0;
        let normalized_y = f64::from(dy) / 100.0;
        self.apply_wheel(pane_x, y, normalized_x, normalized_y);
        cx.stop_propagation();
        cx.notify();
    }

    fn rebuild(
        &mut self,
        width: f32,
        height: f32,
        scale_factor: f32,
        _mutation: SeriesMutation,
        window: &Window,
    ) -> bool {
        self.flush_pending_brush();
        #[cfg(feature = "diagnostics")]
        let rebuild_started = Instant::now();
        self.pin_host_clock();
        let dimensions = (width, height, scale_factor);
        let dimensions_changed = self.built_for != dimensions;
        let layout_recomputed = dimensions_changed || self.layout_dirty;
        if !layout_recomputed
            && !self.engine.frame_requires_layout()
            && !self.engine.frame_requires_axis()
            && !self.frame.panes.is_empty()
        {
            return false;
        }

        if dimensions_changed {
            if self.built_for.2 > 0.0
                && (self.built_for.2 - scale_factor).abs() > SCALE_FACTOR_EPSILON
            {
                self.renderer.invalidate_caches();
            }
            self.built_for = dimensions;
            self.engine.css_width = f64::from(width);
            self.engine.css_height = f64::from(height);
            self.engine.dpr = f64::from(scale_factor);
        }

        let layout = self.engine.options.get().layout.clone();
        let font_size = layout.font_size.to_f32().unwrap_or(12.0);
        let measure = |text: &str| {
            f64::from(measure_text(window, text, &layout.font_family, font_size, 400, false).width)
        };

        if layout_recomputed {
            self.engine.recompute_layout_with_measure(true, measure);
            if !self.fitted {
                self.engine.fit_content();
                self.fitted = true;
                self.engine.recompute_layout_with_measure(true, measure);
            }
            self.layout_dirty = false;
        }

        let max_label_width = (layout.font_size + 4.0) * 5.0 / 8.0
            * f64::from(self.engine.tick_mark_max_character_length.max(1));
        let axis_frame = self.engine.build_axis_frame(max_label_width, measure);
        self.engine.build_frame_into(&mut self.frame);
        self.engine
            .build_axis_primitives_into(&axis_frame, &mut self.axis_prims, |_| 0.0);
        let legend_layout_changed = self.sync_legend_pane_layout();
        #[cfg(feature = "diagnostics")]
        {
            let elapsed = rebuild_started.elapsed();
            if self.live_evidence_enabled && self.live_evidence_rebuilds < 256 {
                self.live_evidence_rebuilds = self.live_evidence_rebuilds.saturating_add(1);
                eprintln!(
                    "AXIUSFLOW_CHART_REBUILD {{\"micros\":{},\"layout\":{},\"data\":\"{}\"}}",
                    elapsed.as_micros(),
                    layout_recomputed,
                    _mutation.label()
                );
            }
            if elapsed.as_millis() >= 4 {
                eprintln!(
                    "chart rebuild: {} ms (layout={})",
                    elapsed.as_millis(),
                    layout_recomputed
                );
            }
        }
        legend_layout_changed
    }

    fn sync_legend_pane_layout(&mut self) -> bool {
        let left = self.engine.pane_left.to_f32().unwrap_or_default();
        let panes = self
            .engine
            .panes
            .iter()
            .map(|pane| LegendPaneLayout {
                left,
                top: pane.top.to_f32().unwrap_or_default(),
                height: pane.height.to_f32().unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        if panes == self.legend_panes {
            return false;
        }
        self.legend_panes = panes;
        true
    }

    fn paint(&mut self, bounds: Bounds<gpui::Pixels>, window: &mut Window, cx: &mut App) {
        #[cfg(feature = "diagnostics")]
        let paint_started = Instant::now();
        let viewport = NucleusViewport::from_bounds(
            bounds.origin.x.into(),
            bounds.origin.y.into(),
            bounds.size.width.into(),
            bounds.size.height.into(),
        );
        let prepared = PreparedNucleusFrame::from_engine(&self.frame, &self.engine)
            .with_axis(&self.axis_prims, &[]);
        if let Err(error) =
            self.renderer
                .paint_frame(&prepared, viewport, window.scale_factor(), window, cx)
        {
            eprintln!("nucleus frame skipped: {error}");
        }
        #[cfg(feature = "diagnostics")]
        {
            let elapsed = paint_started.elapsed();
            if elapsed.as_millis() >= 4 {
                eprintln!("chart paint: {} ms", elapsed.as_millis());
            }
        }
    }
}

impl Default for NucleusChartView {
    fn default() -> Self {
        Self::new()
    }
}

fn legend_palette(theme: ChartTheme) -> LegendPalette {
    match theme {
        ChartTheme::Light => LegendPalette {
            text: rgba(0x1414_14ff),
            muted: rgba(0x6666_66ff),
            bullish: rgba(0x0899_81ff),
            bearish: rgba(0xf236_45ff),
            hover: rgba(0x0000_000a),
            danger: rgba(0xc43c_35ff),
        },
        ChartTheme::Dark => LegendPalette {
            text: rgba(0xf0f0_f0ff),
            muted: rgba(0x9999_99ff),
            bullish: rgba(0x0899_81ff),
            bearish: rgba(0xf236_45ff),
            hover: rgba(0xffff_ff0d),
            danger: rgba(0xef53_50ff),
        },
    }
}

fn text_caret_geometry(
    anchor_x: f32,
    anchor_y: f32,
    text_width: f32,
    size: f32,
    horizontal: &str,
    vertical: &str,
    empty: bool,
) -> (f32, f32, f32) {
    let run_width = if empty { size } else { text_width };
    let left = match horizontal {
        "left" => anchor_x + TEXT_EDIT_PAD,
        "right" => anchor_x - TEXT_EDIT_PAD - run_width,
        _ => anchor_x - run_width / 2.0,
    };
    let run_y = match vertical {
        "top" => anchor_y - TEXT_EDIT_PAD - size / 2.0,
        "bottom" => anchor_y + TEXT_EDIT_PAD + size / 2.0,
        _ => anchor_y,
    };
    let caret_x = if empty { left } else { left + text_width };
    (caret_x, run_y - size * 0.6, size * 1.2)
}

fn chart_legend_layers(
    chart: &Entity<NucleusChartView>,
    rows: &[LegendRow],
    panes: &[LegendPaneLayout],
    theme: ChartTheme,
) -> Vec<AnyElement> {
    let palette = legend_palette(theme);
    panes
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(pane_index, pane)| {
            let pane_rows = rows.iter().filter(|row| row.pane == pane_index);
            let mut layer = div()
                .id(("chart_legend_pane", pane_index))
                .absolute()
                .left(px(pane.left + LEGEND_INSET))
                .top(px(pane.top + LEGEND_INSET))
                .max_w(px(640.0))
                .max_h(px((pane.height - LEGEND_INSET * 2.0).max(0.0)))
                .overflow_hidden()
                .flex()
                .flex_col()
                .items_start()
                .cursor(CursorStyle::Arrow);
            let mut row_count = 0;
            for row in pane_rows {
                row_count += 1;
                layer = layer.child(chart_legend_row(chart, row, palette));
            }
            (row_count > 0).then(|| layer.into_any_element())
        })
        .collect()
}

fn chart_legend_row(
    chart: &Entity<NucleusChartView>,
    row: &LegendRow,
    palette: LegendPalette,
) -> impl IntoElement {
    let group: SharedString = format!("chart-legend-row-{}", row.item.key()).into();
    let visibility = legend_control(
        chart,
        row.item,
        LegendControl::Visibility(row.visible),
        palette,
    )
    .when(row.visible, |control| {
        control
            .invisible()
            .group_hover(group.clone(), gpui::Styled::visible)
    });
    let mut controls = div()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .child(visibility);
    if row.item != LegendItem::Asset {
        controls = controls.child(
            legend_control(chart, row.item, LegendControl::Remove, palette)
                .invisible()
                .group_hover(group.clone(), gpui::Styled::visible),
        );
    }
    div()
        .id(("chart_legend_row", row.item.key()))
        .group(group)
        .h(px(LEGEND_ROW_HEIGHT))
        .max_w_full()
        .flex()
        .items_center()
        .gap_2()
        .px_1()
        .rounded(px(3.0))
        .text_xs()
        .text_color(if row.visible {
            palette.text
        } else {
            palette.muted
        })
        .cursor(CursorStyle::Arrow)
        .hover(|style| style.bg(palette.hover))
        .child(
            div()
                .flex_none()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(row.title.clone()),
        )
        .when(row.visible && !row.values.is_empty(), |legend| {
            legend.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(match row.values_tone {
                        LegendValueTone::Neutral => palette.muted,
                        LegendValueTone::Bullish => palette.bullish,
                        LegendValueTone::Bearish => palette.bearish,
                    })
                    .child(row.values.clone()),
            )
        })
        .child(controls)
}

#[derive(Clone, Copy)]
enum LegendControl {
    Visibility(bool),
    Remove,
}

fn legend_control(
    chart: &Entity<NucleusChartView>,
    item: LegendItem,
    control: LegendControl,
    palette: LegendPalette,
) -> gpui::Stateful<gpui::Div> {
    let action_chart = chart.clone();
    let remove = matches!(control, LegendControl::Remove);
    let (label, path, color) = match control {
        LegendControl::Visibility(true) => ("Hide", LEGEND_VIEW_ICON, palette.text),
        LegendControl::Visibility(false) => ("Show", LEGEND_VIEW_OFF_ICON, palette.muted),
        LegendControl::Remove => ("Remove", LEGEND_REMOVE_ICON, palette.danger),
    };
    let id = item.key() * 2 + u64::from(remove);
    div()
        .id(("chart_legend_control", id))
        .size(px(20.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .text_color(color)
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(label)
        .hover(|style| style.bg(palette.hover))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| {
            action_chart.update(cx, |chart, chart_cx| {
                let changed = if remove {
                    chart.remove_legend_indicator(item)
                } else {
                    let LegendControl::Visibility(visible) = control else {
                        return;
                    };
                    chart.set_legend_item_visible(item, !visible)
                };
                if changed {
                    chart_cx.notify();
                }
            });
            cx.stop_propagation();
        })
        .child(svg().path(path).size(px(14.0)).text_color(color))
}

impl Render for NucleusChartView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mutation = self.apply_pending_data();
        let entity: Entity<Self> = cx.entity();
        let prepaint_entity = entity.clone();
        let hover_entity = entity.clone();
        let focus_handle = self
            .focus_handle
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        let legends =
            chart_legend_layers(&entity, &self.legend_rows(), &self.legend_panes, self.theme);
        let text_caret = self.text_caret_overlay(window);

        div()
            .id(("nucleus_chart_surface", cx.entity_id()))
            .relative()
            .size_full()
            .cursor(self.cursor_style)
            .track_focus(&focus_handle)
            .key_context("NucleusChart")
            .on_hover(move |hovered, _, cx| {
                if !*hovered {
                    hover_entity.update(cx, NucleusChartView::clear_pointer);
                }
            })
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_context_menu))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up_out))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                canvas(
                    move |bounds: Bounds<gpui::Pixels>, window, cx| {
                        let width = bounds.size.width.into();
                        let height = bounds.size.height.into();
                        let scale_factor = window.scale_factor();
                        prepaint_entity.update(cx, |chart, chart_cx| {
                            chart.viewport_origin =
                                (bounds.origin.x.into(), bounds.origin.y.into());
                            if chart.rebuild(width, height, scale_factor, mutation, window) {
                                chart_cx.notify();
                            }
                        });
                        bounds
                    },
                    move |_bounds: Bounds<gpui::Pixels>, prepainted, window, cx| {
                        entity.update(cx, |chart, cx| {
                            chart.paint(prepainted, window, cx);
                        });
                    },
                )
                .size_full(),
            )
            .children(text_caret)
            .children(legends)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_application::{Provenanced, ReplayTailUpdate};

    fn interactive_chart() -> NucleusChartView {
        let mut chart = NucleusChartView::new();
        chart.engine.recompute_layout_with_measure(true, |_| 48.0);
        chart.engine.fit_content();
        chart.engine.recompute_layout_with_measure(true, |_| 48.0);
        chart.fitted = true;
        chart
    }

    fn series_entry(chart: &NucleusChartView, id: u32) -> &nucleuscharts_engine::SeriesEntry {
        chart
            .engine
            .series_entries()
            .iter()
            .find(|series| series.id == id && !series.removed)
            .expect("live series identity resolves")
    }

    fn assert_nucleus_theme(chart: &NucleusChartView, theme: ChartTheme) {
        let (surface, foreground, border, crosshair, label_background) = match theme {
            ChartTheme::Light => ("#ffffff", "#141414", "#f1f1f1", "#141414", "#141414"),
            ChartTheme::Dark => ("#141414", "#f0f0f0", "#262626", "#262626", "#181818"),
        };
        let options = chart.engine.options.get();
        assert_eq!(options.layout.background.color, surface);
        assert_eq!(options.layout.text_color, foreground);
        assert_eq!(
            options.layout.font_family,
            "-apple-system, BlinkMacSystemFont, 'Trebuchet MS', Roboto, Ubuntu, sans-serif"
        );
        assert_eq!(options.grid.vert_lines.color, border);
        assert_eq!(options.crosshair.vert_line.color, crosshair);
        assert_eq!(
            options.crosshair.vert_line.label_background_color,
            label_background
        );
    }

    fn visible_series_point(chart: &NucleusChartView, id: u32) -> (f64, f64) {
        chart
            .engine
            .series_data(id)
            .into_iter()
            .rev()
            .find_map(|point| {
                let x = chart.engine.time_to_coordinate(point.time.to_f64()?)?;
                let y = chart.engine.series_price_to_coordinate(id, point.close)?;
                (x >= 0.0 && x <= chart.engine.pane_w && y >= 0.0 && y <= chart.engine.pane_h)
                    .then_some((x, y))
            })
            .expect("series has a visible point")
    }

    #[test]
    fn empty_chart_surface_accepts_its_first_real_snapshot() {
        let mut chart = NucleusChartView::empty();
        assert!(!chart.has_market_data());
        assert_eq!(chart.queued_replay_update_count(), 0);
        assert_eq!(chart.expected_replay_sequence(), None);
        assert_eq!(chart.replay_bridge_metrics(), ChartBridgeMetrics::default());
        assert!(chart.latest_market_provenance().is_none());

        let replay = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
            .expect("embedded replay validates");
        chart.load_replay(&replay).expect("first snapshot installs");
        assert!(chart.has_market_data());
        assert_eq!(series_entry(&chart, 0).title, "AXF");
        assert_eq!(chart.legend_rows()[0].title, "AXF · 1m · XNAS");
        assert_eq!(
            chart.expected_replay_sequence(),
            replay.stream().last_sequence().checked_add(1)
        );
        assert!(chart.latest_market_provenance().is_some());
    }

    #[test]
    fn host_chart_type_survives_snapshot_install() {
        let replay = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
            .expect("embedded replay validates");
        let mut chart = NucleusChartView::empty();
        assert_eq!(chart.chart_type(), ChartType::Candles);
        chart.set_chart_type(ChartType::Line);
        assert_eq!(chart.chart_type(), ChartType::Line);
        assert_eq!(
            series_entry(&chart, 0).kind,
            nucleuscharts_engine::SeriesKind::Line
        );

        chart.load_replay(&replay).expect("first snapshot installs");
        assert_eq!(chart.chart_type(), ChartType::Line);
        assert_eq!(
            series_entry(&chart, 0).kind,
            nucleuscharts_engine::SeriesKind::Line
        );
        assert_eq!(
            series_entry(&chart, chart.volume_series).kind,
            nucleuscharts_engine::SeriesKind::Histogram
        );
    }

    #[test]
    fn brushable_area_uses_nucleus_feature_series_and_restores_ohlc() {
        let replay = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
            .expect("embedded replay validates");
        let mut chart = NucleusChartView::with_replay(&replay);
        let original = chart.engine.series_data(0);
        assert!(!original.is_empty());
        let original_high = original[0].high;

        chart.set_chart_type(ChartType::BrushableArea);
        assert_eq!(chart.chart_type(), ChartType::BrushableArea);
        assert_eq!(
            series_entry(&chart, 0).kind,
            nucleuscharts_engine::SeriesKind::Feature
        );
        assert_eq!(
            chart.engine.feature_series_kind(0),
            Some(nucleuscharts_engine::FeatureSeriesKind::BrushableArea)
        );
        chart.engine.recompute_layout_with_measure(true, |_| 48.0);
        let start = chart.engine.time_scale.index_to_coordinate(2);
        let end = chart.engine.time_scale.index_to_coordinate(8);
        chart.begin_drag(start, 200.0, 1);
        assert_eq!(chart.drag, Some(ChartDrag::BrushableRange));
        chart.drag_to(end, 200.0);
        chart.end_drag(end, 200.0);
        assert!(chart.drag.is_none());
        let options = chart
            .engine
            .feature_series_options_json(0)
            .expect("brushable options");
        assert!(options.contains("brush_ranges"));
        assert!(options.contains("\"from\""));

        chart.set_chart_type(ChartType::Candles);
        assert_eq!(
            series_entry(&chart, 0).kind,
            nucleuscharts_engine::SeriesKind::Candlestick
        );
        assert_eq!(
            chart.engine.series_data(0)[0].high.to_bits(),
            original_high.to_bits()
        );
    }

    #[test]
    fn calendar_month_snapshot_reaches_nucleus_with_variable_month_spacing() {
        let baseline = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 3 })
            .expect("fixture snapshot");
        let month_starts = [1_704_067_200_i64, 1_706_745_600, 1_709_251_200];
        let bars = baseline
            .bars()
            .iter()
            .zip(month_starts)
            .map(|(item, timestamp)| {
                let mut bar = *item.value();
                bar.exchange_timestamp_seconds = timestamp;
                bar.exchange_timestamp_unix_nanos = timestamp.saturating_mul(1_000_000_000);
                bar
            })
            .collect();
        let mut definition = baseline.bar_definition().clone();
        definition.definition_id = "coinbase:fixture:calendar-months:1".to_string();
        definition.interval_seconds = 0;
        definition.trades_per_bar = None;
        definition.calendar_months = Some(1);
        let replay = ReplaySnapshot::try_new(
            baseline.instrument().clone(),
            baseline.provenance(),
            definition,
            bars,
        )
        .expect("calendar snapshot validates");

        let chart = NucleusChartView::with_replay(&replay);
        assert_eq!(series_entry(&chart, 0).title, "AXF");
        assert_eq!(chart.legend_rows()[0].title, "AXF · 1M · XNAS");
        let installed_times = chart
            .engine
            .series_data(0)
            .into_iter()
            .map(|point| point.time)
            .collect::<Vec<_>>();

        assert!(chart.has_market_data());
        assert_eq!(installed_times, month_starts);
        assert_ne!(
            month_starts[1] - month_starts[0],
            month_starts[2] - month_starts[1]
        );
    }

    #[test]
    fn series_updates_dirty_layout_without_discarding_viewport_dimensions() {
        let mut chart = NucleusChartView::new();
        chart.built_for = (1280.0, 720.0, 1.25);
        chart.layout_dirty = false;

        chart.invalidate_series_layout();

        assert_eq!(chart.built_for, (1280.0, 720.0, 1.25));
        assert!(chart.layout_dirty);
        assert!(chart.frame.panes.is_empty());
        assert!(chart.axis_prims.is_empty());
    }

    #[test]
    fn mouse_up_out_finishes_chart_gesture_without_stopping_window_propagation() {
        assert!(should_stop_mouse_up_propagation(false));
        assert!(!should_stop_mouse_up_propagation(true));
    }

    #[test]
    fn chart_applies_live_tail_replace_and_append_in_one_frame_boundary() {
        let replay = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
            .expect("embedded replay validates");
        let mut chart = NucleusChartView::with_replay(&replay);
        let initial_expected = chart
            .expected_replay_sequence()
            .expect("snapshot establishes sequence");
        let source = replay.bars().last().cloned().expect("tail exists");
        let mut replacement_bar = *source.value();
        replacement_bar.close = replacement_bar.close.saturating_add(1);
        let mut replacement_provenance = source.provenance().clone();
        replacement_provenance.event_id = "tail-replacement".to_string();
        let replacement = ReplayTailUpdate::try_new(
            Provenanced::new(replacement_bar, replacement_provenance),
            replay.evidence().publication_generation + 1,
            true,
        )
        .expect("replacement validates");
        chart
            .try_queue_replay_update(ReplayStreamUpdate::Tail(replacement))
            .expect("replacement queues");
        chart.layout_dirty = false;
        assert_eq!(chart.apply_pending_data(), SeriesMutation::TailReplace);
        assert!(!chart.layout_dirty);
        assert_eq!(chart.expected_replay_sequence(), Some(initial_expected));
        assert_eq!(
            chart
                .latest_market_provenance()
                .map(|provenance| provenance.event_id.as_str()),
            Some("tail-replacement")
        );

        let appended = EmbeddedReplaySource
            .load_delta(initial_expected.saturating_sub(1))
            .expect("fixture delta loads")
            .expect("fixture delta exists");
        let appended = ReplayTailUpdate::try_new(
            appended.item().clone(),
            replay.evidence().publication_generation + 2,
            true,
        )
        .expect("append validates");
        chart
            .try_queue_replay_update(ReplayStreamUpdate::Tail(appended))
            .expect("append queues");
        assert_eq!(chart.apply_pending_data(), SeriesMutation::Append);
        assert!(chart.layout_dirty);
        assert_eq!(
            chart.expected_replay_sequence(),
            initial_expected.checked_add(1)
        );
    }

    #[cfg(feature = "diagnostics")]
    #[test]
    fn snapshot_installation_records_queued_and_direct_foreground_durations() {
        let replay = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 64 })
            .expect("fixture validates");
        let replacement = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 65 })
            .expect("replacement fixture validates")
            .try_with_publication_generation(
                replay.evidence().publication_generation.saturating_add(1),
            )
            .expect("replacement generation validates");
        let mut chart = NucleusChartView::with_replay(&replay);

        assert!(
            chart
                .try_queue_replay_update(ReplayStreamUpdate::Snapshot(replacement))
                .is_ok()
        );
        chart.apply_pending_data();

        assert!(chart.take_snapshot_installation_nanos().is_some());
        assert_eq!(chart.take_snapshot_installation_nanos(), None);

        let direct_replacement = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 66 })
            .expect("direct replacement fixture validates")
            .try_with_publication_generation(
                replay.evidence().publication_generation.saturating_add(2),
            )
            .expect("direct replacement generation validates");
        chart
            .load_replay(&direct_replacement)
            .expect("direct replacement installs");
        assert!(chart.take_snapshot_installation_nanos().is_some());
        assert_eq!(chart.take_snapshot_installation_nanos(), None);
    }

    #[test]
    fn visible_time_range_roundtrips_through_persistent_unix_nanos() {
        let mut chart = interactive_chart();
        let (start, end) = chart
            .visible_time_range_unix_nanos()
            .expect("interactive chart has a viewport");
        let restored = (start + 60_000_000_000, end - 60_000_000_000);
        assert!(chart.set_visible_time_range_unix_nanos(restored.0, restored.1));
        assert_eq!(chart.visible_time_range_unix_nanos(), Some(restored));
    }

    #[test]
    fn nucleus_theme_owns_chart_cosmetics_and_series_defaults() {
        let chart = NucleusChartView::empty();
        let series = &chart.engine.series[0];
        assert_nucleus_theme(&chart, ChartTheme::Dark);
        assert!(series.line_color.is_none());
        assert!(series.up_color.is_none());
        assert!(series.down_color.is_none());
        assert!(series.wick_up_color.is_none());
        assert!(series.wick_down_color.is_none());
        assert!(series.border_up_color.is_none());
        assert!(series.border_down_color.is_none());
    }

    #[test]
    fn last_value_cluster_uses_instrument_title_and_host_clock() {
        let mut chart = interactive_chart();
        let series = series_entry(&chart, 0);
        assert_eq!(series.title, "AXF");
        assert!(series.title_visible);
        assert!(series.countdown_visible);
        assert!(series.last_value_visible);

        let last_time = chart
            .engine
            .series_data(0)
            .last()
            .expect("replay has bars")
            .time
            .to_f64()
            .expect("bar time fits f64");
        let measure = |text: &str| f64::from(u32::try_from(text.len()).unwrap_or(u32::MAX)) * 7.0;
        let _ = chart.engine.build_axis_frame(80.0, measure);
        assert!(!chart.engine.frame_requires_axis());
        chart.pin_host_clock();
        assert!(chart.engine.frame_requires_axis());
        chart.engine.set_now_seconds(last_time + 10.0);
        let texts: Vec<String> = chart
            .engine
            .build_axis_frame(80.0, measure)
            .labels
            .into_iter()
            .map(|label| label.text)
            .collect();
        assert!(
            texts.iter().any(|text| text == "AXF"),
            "title chip missing from last-value cluster: {texts:?}"
        );
        assert!(
            texts.iter().any(|text| text == "00:50"),
            "countdown missing from last-value cluster: {texts:?}"
        );
    }

    #[test]
    fn price_axis_menu_controls_nucleus_series_chrome_and_scale() {
        let mut chart = interactive_chart();
        let sma = chart
            .add_indicator(ChartIndicator::Sma)
            .expect("sma is available");
        let state = chart
            .price_axis_menu_state(0, false)
            .expect("right price scale");
        assert!(state.enabled(PriceAxisMenuState::PRICE_LINE));
        assert!(state.enabled(PriceAxisMenuState::LAST_VALUE));
        assert!(state.enabled(PriceAxisMenuState::TITLE));
        assert!(state.enabled(PriceAxisMenuState::COUNTDOWN));
        assert!(state.enabled(PriceAxisMenuState::AUTO_SCALE));
        assert!(!state.enabled(PriceAxisMenuState::INVERT_SCALE));
        assert!(!state.left);
        assert_eq!(state.mode, 0);
        assert_eq!(state.precision, None);
        assert!(state.enabled(PriceAxisMenuState::INDICATOR_NAMES));
        assert!(state.enabled(PriceAxisMenuState::INDICATOR_VALUES));
        assert!(state.enabled(PriceAxisMenuState::INDICATOR_PRICE_LINES));
        assert!(state.enabled(PriceAxisMenuState::ALIGN_LABELS));
        assert!(!state.enabled(PriceAxisMenuState::BID_ASK));

        assert!(chart.apply_price_axis_menu_action(0, false, PriceAxisMenuAction::ToggleTitle));
        assert!(
            !chart
                .price_axis_menu_state(0, false)
                .unwrap()
                .enabled(PriceAxisMenuState::TITLE)
        );
        assert!(chart.apply_price_axis_menu_action(
            0,
            false,
            PriceAxisMenuAction::ToggleIndicatorNameLabels
        ));
        assert!(!series_entry(&chart, sma[0]).title_visible);
        assert!(series_entry(&chart, sma[0]).last_value_visible);
        assert!(chart.apply_price_axis_menu_action(0, false, PriceAxisMenuAction::ToggleBidAsk));
        assert!(series_entry(&chart, 0).bid_ask_visible);
        assert!(chart.apply_price_axis_menu_action(
            0,
            false,
            PriceAxisMenuAction::ToggleAlignLabels
        ));
        assert!(
            !chart
                .price_axis_menu_state(0, false)
                .unwrap()
                .enabled(PriceAxisMenuState::ALIGN_LABELS)
        );
        assert!(chart.apply_price_axis_menu_action(0, false, PriceAxisMenuAction::SetMode(1)));
        assert!(chart.apply_price_axis_menu_action(0, false, PriceAxisMenuAction::SetLeft(true)));
        let moved = chart
            .price_axis_menu_state(0, true)
            .expect("left price scale");
        assert!(moved.left);
        assert_eq!(moved.mode, 1);
        assert_eq!(
            series_entry(&chart, 0).price_scale_target,
            PriceScaleTarget::Left
        );
        assert!(chart.apply_price_axis_menu_action(
            0,
            true,
            PriceAxisMenuAction::SetPrecision(Some(4))
        ));
        assert_eq!(
            chart.price_axis_menu_state(0, true).unwrap().precision,
            Some(4)
        );
        assert_eq!(series_entry(&chart, 0).price_format.precision, 4);
    }

    #[test]
    fn indicator_label_preference_applies_to_every_indicator_and_later_additions() {
        let mut chart = interactive_chart();
        assert!(chart.apply_price_axis_menu_action(
            0,
            false,
            PriceAxisMenuAction::ToggleIndicatorNameLabels
        ));
        assert!(chart.apply_price_axis_menu_action(
            0,
            false,
            PriceAxisMenuAction::ToggleIndicatorValueLabels
        ));
        assert!(!chart.indicator_name_labels_visible());
        assert!(!chart.indicator_value_labels_visible());
        let state = chart
            .price_axis_menu_state(0, false)
            .expect("right price scale");
        assert!(!state.enabled(PriceAxisMenuState::INDICATOR_NAMES));
        assert!(!state.enabled(PriceAxisMenuState::INDICATOR_VALUES));

        let sma = chart
            .add_indicator(ChartIndicator::Sma)
            .expect("sma is available");
        let rsi = chart
            .add_indicator(ChartIndicator::Rsi)
            .expect("rsi is available");
        assert_ne!(series_entry(&chart, rsi[0]).pane_index, 0);
        for id in sma.iter().chain(rsi.iter()).copied() {
            let series = series_entry(&chart, id);
            assert!(!series.last_value_visible, "labels stayed on {id}");
            assert!(!series.title_visible, "title stayed on {id}");
            assert!(
                series.price_line_visible,
                "price line followed labels on {id}"
            );
        }

        assert!(chart.apply_price_axis_menu_action(
            0,
            false,
            PriceAxisMenuAction::ToggleIndicatorNameLabels
        ));
        for id in sma.iter().chain(rsi.iter()).copied() {
            let series = series_entry(&chart, id);
            assert!(!series.last_value_visible);
            assert!(series.title_visible);
        }
        assert!(chart.apply_price_axis_menu_action(
            0,
            false,
            PriceAxisMenuAction::ToggleIndicatorValueLabels
        ));
        for id in sma.iter().chain(rsi.iter()).copied() {
            let series = series_entry(&chart, id);
            assert!(series.last_value_visible);
            assert!(series.title_visible);
        }
    }

    #[test]
    fn indicator_price_line_preference_applies_to_every_plot() {
        let mut chart = interactive_chart();
        let ema_fast = chart
            .add_indicator(ChartIndicator::Ema)
            .expect("ema is available");
        let ema_slow = chart
            .add_indicator(ChartIndicator::Ema)
            .expect("second ema is available");
        let macd = chart
            .add_indicator(ChartIndicator::Macd)
            .expect("macd is available");
        assert_eq!(ema_fast.len(), 1);
        assert_eq!(ema_slow.len(), 1);
        assert_eq!(macd.len(), 3);
        let macd_pane = series_entry(&chart, macd[0]).pane_index;
        assert_ne!(macd_pane, 0);
        let indicator_ids: Vec<u32> = ema_fast
            .iter()
            .chain(ema_slow.iter())
            .chain(macd.iter())
            .copied()
            .collect();
        for id in &indicator_ids {
            assert!(
                series_entry(&chart, *id).price_line_visible,
                "price line missing on {id}"
            );
        }

        assert!(chart.apply_price_axis_menu_action(0, false, PriceAxisMenuAction::TogglePriceLine));
        assert!(!series_entry(&chart, 0).price_line_visible);
        for id in &indicator_ids {
            assert!(
                series_entry(&chart, *id).price_line_visible,
                "overlay or oscillator plot {id} followed the symbol price line"
            );
        }

        assert!(chart.apply_price_axis_menu_action(
            macd_pane,
            false,
            PriceAxisMenuAction::TogglePriceLine
        ));
        assert!(series_entry(&chart, 0).price_line_visible);
        for id in &macd {
            assert!(
                series_entry(&chart, *id).price_line_visible,
                "macd plot {id} followed the symbol price line"
            );
        }

        assert!(chart.apply_price_axis_menu_action(
            0,
            false,
            PriceAxisMenuAction::ToggleIndicatorPriceLines
        ));
        assert!(!chart.indicator_price_lines_visible());
        assert!(
            !chart
                .price_axis_menu_state(0, false)
                .unwrap()
                .enabled(PriceAxisMenuState::INDICATOR_PRICE_LINES)
        );
        assert!(series_entry(&chart, 0).price_line_visible);
        for id in &indicator_ids {
            assert!(
                !series_entry(&chart, *id).price_line_visible,
                "indicator plot {id} kept its price line"
            );
        }

        let sma = chart
            .add_indicator(ChartIndicator::Sma)
            .expect("sma is available");
        assert!(!series_entry(&chart, sma[0]).price_line_visible);

        assert!(chart.apply_price_axis_menu_action(
            0,
            false,
            PriceAxisMenuAction::ToggleIndicatorPriceLines
        ));
        for id in indicator_ids.iter().chain(sma.iter()).copied() {
            assert!(
                series_entry(&chart, id).price_line_visible,
                "indicator plot {id} stayed hidden"
            );
        }
    }

    #[test]
    fn nucleus_theme_switch_is_atomic_for_data_viewport_drawings_and_indicators() {
        let mut chart = interactive_chart();
        let volume = chart
            .add_indicator(ChartIndicator::Volume)
            .expect("volume is available");
        let vwap = chart
            .add_indicator(ChartIndicator::Vwap)
            .expect("vwap is available");
        assert!(
            chart
                .engine
                .drawing_create_begin(DrawingKind::HorizontalLine, None)
        );
        assert_eq!(
            chart
                .engine
                .drawing_create_click(300.0, 200.0, DrawingModifiers::default()),
            1
        );
        chart.engine.time_scale.zoom(300.0, 0.5);
        chart.engine.scroll_to_position(-12.0);

        let price_data = chart
            .engine
            .data_layer()
            .series_data(0)
            .map(|(times, columns)| (times.to_vec(), columns.map(<[f64]>::to_vec)))
            .expect("price data");
        let volume_data = chart
            .engine
            .data_layer()
            .series_data(chart.volume_series)
            .map(|(times, columns)| (times.to_vec(), columns.map(<[f64]>::to_vec)))
            .expect("volume data");
        let spacing = chart.engine.bar_spacing();
        let offset = chart.engine.right_offset();
        let drawings = chart.engine.drawings_json();
        let pane_count = chart.engine.panes.len();
        let series_count = chart.engine.series.len();

        chart.set_theme(ChartTheme::Light);
        assert_nucleus_theme(&chart, ChartTheme::Light);
        chart.set_theme(ChartTheme::Dark);
        assert_nucleus_theme(&chart, ChartTheme::Dark);
        assert_eq!(
            chart
                .engine
                .data_layer()
                .series_data(0)
                .map(|(times, columns)| { (times.to_vec(), columns.map(<[f64]>::to_vec)) }),
            Some(price_data)
        );
        assert_eq!(
            chart
                .engine
                .data_layer()
                .series_data(chart.volume_series)
                .map(|(times, columns)| { (times.to_vec(), columns.map(<[f64]>::to_vec)) }),
            Some(volume_data)
        );
        assert_eq!(chart.engine.bar_spacing().to_bits(), spacing.to_bits());
        assert_eq!(chart.engine.right_offset().to_bits(), offset.to_bits());
        assert_eq!(chart.engine.drawings_json(), drawings);
        assert_eq!(chart.engine.panes.len(), pane_count);
        assert_eq!(chart.engine.series.len(), series_count);
        assert_eq!(volume, vec![chart.volume_series]);
        assert!(series_entry(&chart, chart.volume_series).visible);
        assert_eq!(
            chart.engine.indicator_info(vwap[0]).map(|info| info.kind),
            Some("vwap")
        );
    }

    #[test]
    fn wheel_zoom_and_horizontal_scroll_mutate_nucleus_without_refitting() {
        let mut chart = interactive_chart();
        let spacing = chart.engine.bar_spacing();
        chart.apply_wheel(400.0, 200.0, 0.0, 1.0);
        assert!((chart.engine.bar_spacing() - spacing).abs() > f64::EPSILON);
        let offset = chart.engine.right_offset();
        chart.apply_wheel(400.0, 200.0, 1.0, 0.0);
        assert!((chart.engine.right_offset() - offset).abs() > f64::EPSILON);
        assert!(chart.fitted);
    }

    #[test]
    fn mouse_pan_and_crosshair_have_bounded_lifecycle() {
        let mut chart = interactive_chart();
        chart.begin_drag(300.0, 200.0, 1);
        assert_eq!(chart.drag, Some(ChartDrag::Pane { price_pan: None }));
        assert_eq!(chart.engine.crosshair, Some((300.0, 200.0)));
        let offset = chart.engine.right_offset();
        chart.drag_to(340.0, 200.0);
        assert!((chart.engine.right_offset() - offset).abs() > f64::EPSILON);
        chart.end_drag(340.0, 200.0);
        assert!(chart.drag.is_none());
        chart.update_crosshair(-1.0, 200.0);
        assert!(chart.engine.crosshair.is_none());
    }

    #[test]
    fn axes_drag_and_double_click_reset_through_nucleus() {
        let mut chart = interactive_chart();
        chart.begin_drag(300.0, chart.engine.pane_h + 10.0, 1);
        assert_eq!(chart.drag, Some(ChartDrag::TimeAxis));
        chart.drag_to(340.0, chart.engine.pane_h + 10.0);
        chart.end_drag(340.0, chart.engine.pane_h + 10.0);
        assert!(chart.drag.is_none());

        let right_axis_x = chart.engine.pane_w + 1.0;
        assert_eq!(
            chart
                .engine
                .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
            Some(true)
        );
        chart.begin_drag(right_axis_x, 200.0, 1);
        assert!(matches!(
            chart.drag,
            Some(ChartDrag::PriceAxis {
                target: PriceScaleTarget::Right,
                ..
            })
        ));
        assert_eq!(
            chart
                .engine
                .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
            Some(false)
        );
        chart.drag_to(right_axis_x, 240.0);
        assert_eq!(
            chart
                .engine
                .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
            Some(false)
        );
        chart.end_drag(right_axis_x, 240.0);
        assert!(chart.drag.is_none());
        assert_eq!(
            chart
                .engine
                .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
            Some(false)
        );

        let locked_range = chart
            .engine
            .price_scale_visible_range_for(0, PriceScaleTarget::Right);
        chart.begin_drag(300.0, 200.0, 1);
        assert!(matches!(
            chart.drag,
            Some(ChartDrag::Pane {
                price_pan: Some((_, PriceScaleTarget::Right))
            })
        ));
        chart.drag_to(300.0, 260.0);
        chart.end_drag(300.0, 260.0);
        assert_ne!(
            chart
                .engine
                .price_scale_visible_range_for(0, PriceScaleTarget::Right),
            locked_range
        );
        assert_eq!(
            chart
                .engine
                .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
            Some(false)
        );

        chart.engine.time_scale.start_scroll(0.0);
        chart.engine.time_scale.scroll_to(80.0);
        chart.engine.time_scale.end_scroll();
        let offset = chart.engine.right_offset();
        assert!(offset.abs() > f64::EPSILON);
        chart.begin_drag(300.0, chart.engine.pane_h + 10.0, 2);
        assert!(chart.engine.right_offset().abs() < offset.abs());
        assert!(chart.drag.is_none());
    }

    #[test]
    fn reset_view_restores_native_time_defaults_and_automatic_price_scaling() {
        let mut chart = interactive_chart();
        chart.engine.time_scale.start_scroll(0.0);
        chart.engine.time_scale.scroll_to(80.0);
        chart.engine.time_scale.end_scroll();
        chart
            .engine
            .set_price_scale_auto_scale_for(0, PriceScaleTarget::Right, false);
        assert!(chart.engine.right_offset().abs() > f64::EPSILON);
        assert_eq!(
            chart
                .engine
                .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
            Some(false)
        );

        chart.reset_view();

        assert!(chart.engine.right_offset().abs() < f64::EPSILON);
        assert_eq!(
            chart
                .engine
                .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
            Some(true)
        );
        assert!(chart.fitted);
    }

    #[test]
    fn scroll_to_latest_preserves_zoom_and_returns_to_real_time_edge() {
        let mut chart = interactive_chart();
        chart.engine.time_scale.zoom(300.0, 0.5);
        chart.engine.scroll_to_position(-12.0);
        let spacing = chart.engine.bar_spacing();

        chart.scroll_to_latest();

        assert!(chart.is_at_latest());
        assert!((chart.engine.bar_spacing() - spacing).abs() < f64::EPSILON);
    }

    #[test]
    fn indicator_catalog_maps_to_nucleus_with_legacy_defaults() {
        let cases = [
            (ChartIndicator::Sma, 1, "sma", "SMA 20"),
            (ChartIndicator::Ema, 1, "ema", "EMA 20"),
            (ChartIndicator::Wma, 1, "wma", "WMA 20"),
            (ChartIndicator::Bollinger, 3, "bollinger", "Bollinger 20 2"),
            (ChartIndicator::Rsi, 1, "rsi", "RSI 14"),
            (ChartIndicator::Macd, 3, "macd", "MACD 12 26 9"),
            (
                ChartIndicator::Stochastic,
                2,
                "stochastic",
                "Stochastic 14 3",
            ),
            (ChartIndicator::Atr, 1, "atr", "ATR 14"),
        ];

        for (indicator, output_count, kind, title) in cases {
            let mut chart = interactive_chart();
            let ids = chart
                .add_indicator(indicator)
                .expect("supported indicator is created");

            assert_eq!(ids.len(), output_count);
            assert_eq!(series_entry(&chart, ids[0]).title, title);
            for id in ids {
                assert_eq!(
                    chart
                        .engine
                        .indicator_info(id)
                        .expect("indicator lineage")
                        .kind,
                    kind
                );
            }
        }
    }

    #[test]
    fn chart_legends_group_outputs_and_follow_native_indicator_panes() {
        let mut chart = interactive_chart();
        let bollinger = chart
            .add_indicator(ChartIndicator::Bollinger)
            .expect("Bollinger is created");
        let macd = chart
            .add_indicator(ChartIndicator::Macd)
            .expect("MACD is created");
        chart.engine.recompute_layout_with_measure(true, |_| 48.0);

        let rows = chart.legend_rows();
        let asset = rows.first().expect("asset legend is always first");
        assert_eq!(asset.item, LegendItem::Asset);
        assert_eq!(asset.pane, 0);
        assert!(asset.values.contains("O "));
        assert!(asset.values.contains("C "));
        assert_eq!(
            rows.iter()
                .filter(|row| row.item == LegendItem::Indicator(bollinger[0]))
                .count(),
            1
        );
        let macd_row = rows
            .iter()
            .find(|row| row.item == LegendItem::Indicator(macd[0]))
            .expect("grouped MACD legend");
        assert!(macd_row.pane > 0);
        assert!(macd_row.values.contains("MACD"));
        assert!(macd_row.values.contains("Signal"));
        assert!(macd_row.values.contains("Histogram"));
    }

    #[test]
    fn asset_legend_shows_ohlc_only_on_candles_and_bars() {
        let mut chart = interactive_chart();
        let asset_values = |chart: &NucleusChartView| {
            chart
                .legend_rows()
                .into_iter()
                .find(|row| row.item == LegendItem::Asset)
                .expect("asset legend")
                .values
        };
        assert!(asset_values(&chart).contains("O "));
        assert!(asset_values(&chart).contains("C "));

        chart.set_chart_type(ChartType::Bars);
        assert!(asset_values(&chart).contains("O "));
        assert!(asset_values(&chart).contains("C "));

        for chart_type in [
            ChartType::Line,
            ChartType::Area,
            ChartType::Baseline,
            ChartType::BrushableArea,
        ] {
            chart.set_chart_type(chart_type);
            assert!(
                asset_values(&chart).is_empty(),
                "OHLC stayed on {chart_type:?}"
            );
        }

        chart.set_chart_type(ChartType::Candles);
        assert!(asset_values(&chart).contains("O "));
        assert!(asset_values(&chart).contains("C "));
    }

    #[test]
    fn legend_visibility_preserves_rows_and_indicator_removal_clears_bindings() {
        let mut chart = interactive_chart();
        let sma = chart
            .add_indicator(ChartIndicator::Sma)
            .expect("SMA is created")[0];

        assert!(!chart.legend_rows()[0].values.is_empty());
        assert!(chart.set_legend_item_visible(LegendItem::Asset, false));
        assert!(!series_entry(&chart, 0).visible);
        assert_eq!(chart.legend_rows()[0].item, LegendItem::Asset);
        assert!(!chart.legend_rows()[0].visible);
        assert!(chart.legend_rows()[0].values.is_empty());
        assert!(chart.set_legend_item_visible(LegendItem::Indicator(sma), false));
        assert!(!series_entry(&chart, sma).visible);
        let sma_row = chart
            .legend_rows()
            .into_iter()
            .find(|row| row.item == LegendItem::Indicator(sma))
            .expect("SMA legend remains while hidden");
        assert!(!sma_row.visible);
        assert!(sma_row.values.is_empty());
        assert!(chart.set_legend_item_visible(LegendItem::Indicator(sma), true));
        assert!(series_entry(&chart, sma).visible);
        assert!(
            chart
                .legend_rows()
                .iter()
                .any(|row| row.item == LegendItem::Indicator(sma)
                    && row.visible
                    && !row.values.is_empty())
        );
        assert!(chart.remove_legend_indicator(LegendItem::Indicator(sma)));
        assert!(
            chart
                .legend_rows()
                .iter()
                .all(|row| row.item != LegendItem::Indicator(sma))
        );
        assert!(!chart.remove_legend_indicator(LegendItem::Asset));
    }

    #[test]
    fn hidden_volume_keeps_its_legend_until_removed() {
        let mut chart = interactive_chart();
        let volume = chart
            .add_indicator(ChartIndicator::Volume)
            .expect("volume is created")[0];

        assert!(chart.set_legend_item_visible(LegendItem::Volume, false));
        assert!(!series_entry(&chart, volume).visible);
        assert!(chart.has_indicators());
        let volume_row = chart
            .legend_rows()
            .into_iter()
            .find(|row| row.item == LegendItem::Volume)
            .expect("volume legend remains while hidden");
        assert!(!volume_row.visible);
        assert!(volume_row.values.is_empty());
        assert!(chart.remove_legend_indicator(LegendItem::Volume));
        assert!(!chart.has_indicators());
        assert!(
            chart
                .legend_rows()
                .iter()
                .all(|row| row.item != LegendItem::Volume)
        );
    }

    #[test]
    fn native_indicator_hover_selection_and_delete_reach_nucleus() {
        let mut chart = interactive_chart();
        let indicator = chart
            .add_indicator(ChartIndicator::Sma)
            .expect("SMA is created")[0];
        chart.engine.recompute_layout_with_measure(true, |_| 48.0);
        let (x, y) = visible_series_point(&chart, indicator);
        assert_eq!(chart.engine.hit_test_series(x, y), Some(indicator));

        chart.update_cursor(x, y);
        assert_eq!(chart.engine.hovered_series(), Some(indicator));
        assert_eq!(chart.cursor_style, CursorStyle::PointingHand);
        assert!(chart.select_series_at(x, y));
        assert_eq!(chart.engine.selected_series(), Some(indicator));
        assert!(chart.has_deletable_selection());

        assert!(chart.remove_selected_chart_object());
        assert!(chart.engine.indicator_info(indicator).is_none());
        assert!(
            chart
                .engine
                .series_entries()
                .iter()
                .all(|series| series.id != indicator || series.removed)
        );
    }

    #[test]
    fn grouped_indicator_delete_removes_every_native_output() {
        let mut chart = interactive_chart();
        let outputs = chart
            .add_indicator(ChartIndicator::Macd)
            .expect("MACD is created");
        chart.engine.set_selected_series(Some(outputs[1]));

        assert!(chart.remove_selected_chart_object());
        for output in outputs {
            assert!(
                chart
                    .engine
                    .series_entries()
                    .iter()
                    .all(|series| series.id != output || series.removed)
            );
        }
    }

    #[test]
    fn clear_indicators_removes_every_native_output_and_hides_volume() {
        let mut chart = interactive_chart();
        assert!(!chart.has_indicators());
        let volume = chart
            .add_indicator(ChartIndicator::Volume)
            .expect("volume is shown")[0];
        let macd = chart
            .add_indicator(ChartIndicator::Macd)
            .expect("MACD is created");
        assert!(chart.has_indicators());

        assert!(chart.clear_indicators());
        assert!(!chart.has_indicators());
        assert!(!series_entry(&chart, volume).visible);
        for output in macd {
            assert!(
                chart
                    .engine
                    .series_entries()
                    .iter()
                    .all(|series| series.id != output || series.removed)
            );
        }
        assert!(!chart.clear_indicators());
        assert_eq!(
            chart
                .add_indicator(ChartIndicator::Volume)
                .expect("volume can be shown again"),
            vec![volume]
        );
        assert!(chart.has_indicators());
    }

    #[test]
    fn product_series_are_protected_and_volume_remains_reusable() {
        let mut chart = interactive_chart();
        chart.engine.set_selected_series(Some(0));
        assert!(!chart.has_deletable_selection());
        assert!(!chart.remove_selected_chart_object());
        assert!(series_entry(&chart, 0).visible);

        let volume = chart
            .add_indicator(ChartIndicator::Volume)
            .expect("volume is shown")[0];
        chart.engine.set_selected_series(Some(volume));
        assert!(chart.has_deletable_selection());
        assert!(chart.remove_selected_chart_object());
        assert!(!series_entry(&chart, volume).visible);
        assert_eq!(
            chart
                .add_indicator(ChartIndicator::Volume)
                .expect("volume can be shown again"),
            vec![volume]
        );
        assert!(series_entry(&chart, volume).visible);
    }

    #[test]
    fn replay_volume_drives_histogram_and_vwap_with_real_weights() {
        let replay = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
            .expect("embedded replay validates");
        let mut chart = NucleusChartView::with_replay(&replay);
        let (_, volume_columns) = chart
            .engine
            .data_layer()
            .series_data(chart.volume_series)
            .expect("parallel volume series");
        let expected_volume = replay
            .bars()
            .iter()
            .map(|item| {
                item.value()
                    .volume
                    .to_f64()
                    .expect("fixture volume fits f64")
            })
            .collect::<Vec<_>>();
        assert_eq!(volume_columns[3], expected_volume);
        assert!(!series_entry(&chart, chart.volume_series).visible);
        assert!(series_entry(&chart, chart.volume_series).histogram_updown);
        assert_eq!(
            series_entry(&chart, chart.volume_series).price_scale_target,
            PriceScaleTarget::Overlay
        );

        assert_eq!(
            chart
                .add_indicator(ChartIndicator::Volume)
                .expect("volume histogram"),
            vec![chart.volume_series]
        );
        assert!(series_entry(&chart, chart.volume_series).visible);

        let vwap = chart
            .add_indicator(ChartIndicator::Vwap)
            .expect("volume-weighted average");
        assert_eq!(vwap.len(), 1);
        assert_eq!(
            chart.engine.indicator_info(vwap[0]).map(|info| info.kind),
            Some("vwap")
        );
        let (_, vwap_columns) = chart
            .engine
            .data_layer()
            .series_data(vwap[0])
            .expect("vwap output data");
        assert_eq!(vwap_columns[3].len(), replay.bars().len());
        assert!(vwap_columns[3].iter().all(|value| value.is_finite()));
    }

    #[test]
    fn indicator_api_rejects_an_empty_chart_without_inventing_series() {
        let mut chart = NucleusChartView::empty();
        let initial_series = chart.engine.series.len();

        for indicator in ChartIndicator::ALL {
            assert_eq!(
                chart.add_indicator(indicator),
                Err(ChartIndicatorError::MarketDataUnavailable)
            );
        }

        assert_eq!(chart.engine.series.len(), initial_series);
    }

    #[test]
    fn indicator_metadata_matches_the_legacy_picker_copy() {
        assert_eq!(ChartIndicator::ALL.len(), 10);
        assert_eq!(ChartIndicator::Volume.label(), "Volume");
        assert_eq!(ChartIndicator::Vwap.parameters(), "Session anchored");
        assert_eq!(ChartIndicator::Sma.label(), "Moving Average");
        assert_eq!(ChartIndicator::Sma.parameters(), "Period 20");
        assert_eq!(
            ChartIndicator::Bollinger.parameters(),
            "Period 20 · Deviation 2"
        );
        assert_eq!(
            ChartIndicator::Macd.parameters(),
            "Fast 12 · Slow 26 · Signal 9"
        );
        assert_eq!(ChartIndicator::Stochastic.parameters(), "%K 14 · %D 3");
        assert_eq!(ChartIndicator::Atr.parameters(), "Period 14");
    }

    #[test]
    fn anchored_drawing_tools_commit_real_nucleus_drawings_and_return_to_cursor() {
        let mut chart = interactive_chart();
        let tools = [
            (ChartDrawingTool::TrendLine, 2, 160.0),
            (ChartDrawingTool::HorizontalLine, 1, 180.0),
            (ChartDrawingTool::VerticalLine, 1, 200.0),
            (ChartDrawingTool::Ray, 1, 220.0),
            (ChartDrawingTool::Rectangle, 2, 240.0),
            (ChartDrawingTool::Text, 1, 260.0),
        ];
        let anchor_x = [260.0, 340.0];

        for (index, (tool, anchors, y)) in tools.into_iter().enumerate() {
            chart.set_drawing_tool(tool);
            assert_eq!(chart.drawing_tool(), tool);
            assert!(
                !chart.engine.drawing_create_active(),
                "arming must not start a pre-click handle"
            );
            for &x in anchor_x.iter().take(anchors) {
                let handled = chart.drawing_pointer_down(x, y, DrawingModifiers::default(), 1);
                assert!(handled);
            }
            assert_eq!(chart.drawing_count(), index + 1);
            assert_eq!(chart.drawing_tool(), ChartDrawingTool::Cursor);
            assert!(!chart.engine.drawing_create_active());
        }
    }

    #[test]
    fn armed_ctrl_magnet_snaps_the_crosshair_without_a_preview_dot() {
        let mut chart = interactive_chart();
        chart.set_drawing_tool(ChartDrawingTool::TrendLine);
        assert!(!chart.engine.drawing_create_active());

        let x = chart.engine.time_scale.logical_to_coordinate(32.0);
        let y = 200.0;
        chart.update_crosshair(x, y);
        let free = chart.engine.build_frame();
        chart.update_crosshair_magnet(true);
        let snapped = chart.engine.build_frame();
        let crosshair_color =
            Color::parse_css(&chart.engine.options.get().crosshair.horz_line.color)
                .expect("the package crosshair color is valid");
        let crosshair_y = |frame: &ChartFrame| {
            frame.panes[0].main.iter().find_map(|prim| match prim {
                Prim::HLine { y, color, .. } if *color == crosshair_color => Some(*y),
                _ => None,
            })
        };

        assert_ne!(crosshair_y(&free), crosshair_y(&snapped));
        assert_eq!(
            snapped.panes[0]
                .main
                .iter()
                .filter(|prim| matches!(prim, Prim::Circle { .. }))
                .count(),
            0,
            "arming a tool must not create a pre-click anchor handle"
        );
    }

    #[test]
    fn text_tool_place_enters_edit_mode_and_keeps_typed_label() {
        let mut chart = interactive_chart();
        chart.set_drawing_tool(ChartDrawingTool::Text);
        assert!(chart.drawing_pointer_down(300.0, 200.0, DrawingModifiers::default(), 1));
        assert!(chart.is_editing_text());
        assert_eq!(chart.drawing_tool(), ChartDrawingTool::Cursor);

        assert!(chart.set_editing_text_value("NQ"));
        assert_eq!(chart.editing_text_value().as_deref(), Some("NQ"));
        assert!(chart.finish_text_edit());
        assert!(!chart.is_editing_text());
        assert_eq!(chart.drawing_count(), 1);
        assert_eq!(chart.engine.drawings()[0].text, "NQ");
    }

    #[test]
    fn pointer_exit_keeps_the_active_text_edit_session() {
        let mut chart = interactive_chart();
        chart.set_drawing_tool(ChartDrawingTool::Text);
        assert!(chart.drawing_pointer_down(300.0, 200.0, DrawingModifiers::default(), 1));

        chart.cancel_pointer_gesture();

        assert!(chart.is_editing_text());
        assert!(chart.set_editing_text_value("ES"));
        assert_eq!(chart.editing_text_value().as_deref(), Some("ES"));
    }

    #[test]
    fn text_caret_tracks_the_end_of_centered_and_empty_labels() {
        assert_eq!(
            text_caret_geometry(100.0, 100.0, 40.0, 20.0, "center", "middle", false,),
            (120.0, 88.0, 24.0)
        );
        assert_eq!(
            text_caret_geometry(100.0, 100.0, 0.0, 20.0, "center", "middle", true,),
            (90.0, 88.0, 24.0)
        );
    }

    #[test]
    fn activate_request_is_latched_until_taken() {
        let mut chart = interactive_chart();
        assert!(!chart.take_activate_request());
        chart.pending_activate = ActivationRequest::Pending;
        assert!(chart.take_activate_request());
        assert!(!chart.take_activate_request());
    }

    #[test]
    fn unfinished_empty_text_edit_is_removed_on_finish() {
        let mut chart = interactive_chart();
        chart.set_drawing_tool(ChartDrawingTool::Text);
        assert!(chart.drawing_pointer_down(300.0, 200.0, DrawingModifiers::default(), 1));
        assert!(chart.is_editing_text());
        assert!(chart.finish_text_edit());
        assert!(!chart.is_editing_text());
        assert_eq!(chart.drawing_count(), 0);
    }

    #[test]
    fn brush_capture_commits_on_release_and_returns_to_cursor() {
        let mut chart = interactive_chart();
        chart.set_drawing_tool(ChartDrawingTool::Brush);

        assert!(chart.drawing_pointer_down(240.0, 180.0, DrawingModifiers::default(), 1));
        assert!(chart.drawing_pointer_move(280.0, 210.0, true, DrawingModifiers::default()));
        assert!(chart.drawing_pointer_up(320.0, 240.0, DrawingModifiers::default()));

        assert_eq!(chart.drawing_count(), 1);
        assert_eq!(chart.drawing_tool(), ChartDrawingTool::Cursor);
        assert!(!chart.engine.brush_create_active());
    }

    #[test]
    fn brush_capture_coalesces_pointer_samples_to_one_knot_per_flush() {
        let mut chart = interactive_chart();
        chart.set_drawing_tool(ChartDrawingTool::Brush);
        let modifiers = DrawingModifiers::default();

        assert!(chart.drawing_pointer_down(100.0, 100.0, modifiers, 1));
        for offset in 1..=8 {
            let x = 100.0 + f64::from(offset);
            assert!(chart.drawing_pointer_move(x, 100.0 + x, true, modifiers));
        }
        assert!(
            chart.flush_pending_brush(),
            "the newest pending sample is captured once"
        );
        assert!(
            !chart.flush_pending_brush(),
            "an idle flush without a pending sample captures nothing"
        );
        assert!(chart.drawing_pointer_up(180.0, 180.0, modifiers));

        let drawing = &chart.engine.drawings()[0];
        assert_eq!(drawing.kind, DrawingKind::Brush);
        assert_eq!(
            drawing.points.len(),
            3,
            "start + one coalesced move + release; intermediate staircase samples must not become knots"
        );
    }

    #[test]
    fn cursor_selects_and_moves_unlocked_drawings_but_locked_drawings_do_not_drag() {
        let mut chart = interactive_chart();
        chart.set_drawing_tool(ChartDrawingTool::HorizontalLine);
        assert!(chart.drawing_pointer_down(300.0, 200.0, DrawingModifiers::default(), 1));
        let id = chart.selected_drawing_id().expect("drawing selected");
        let (_, drawing_y) = chart
            .engine
            .drawing_point_to_coordinate(id, 0)
            .expect("drawing coordinate");
        chart.set_drawing_tool(ChartDrawingTool::Cursor);

        assert!(chart.set_selected_drawing_locked(true));
        assert!(chart.drawing_pointer_down(500.0, drawing_y, DrawingModifiers::default(), 1));
        assert!(!chart.engine.drawing_drag_active());
        assert_eq!(chart.selected_drawing_id(), Some(id));

        assert!(chart.set_selected_drawing_locked(false));
        assert!(chart.drawing_pointer_down(500.0, drawing_y, DrawingModifiers::default(), 1));
        assert!(chart.engine.drawing_drag_active());
        assert!(chart.drawing_pointer_up(500.0, drawing_y + 30.0, DrawingModifiers::default()));
        assert!(!chart.engine.drawing_drag_active());
    }

    #[test]
    fn lock_summary_delete_clear_and_escape_follow_toolbar_contract() {
        let mut chart = interactive_chart();
        chart.set_drawing_tool(ChartDrawingTool::HorizontalLine);
        assert!(chart.drawing_pointer_down(300.0, 180.0, DrawingModifiers::default(), 1));
        chart.set_drawing_tool(ChartDrawingTool::HorizontalLine);
        assert!(chart.drawing_pointer_down(300.0, 240.0, DrawingModifiers::default(), 1));
        assert_eq!(chart.drawing_count(), 2);

        assert!(chart.set_all_drawings_locked(true));
        assert_eq!(
            chart.drawings_lock_summary(),
            DrawingsLockSummary {
                total: 2,
                locked_count: 2,
                all_locked: true,
            }
        );
        assert!(chart.apply_key("delete", false));
        assert_eq!(chart.drawing_count(), 1);
        assert_eq!(chart.drawings_lock_summary().locked_count, 1);

        assert!(chart.apply_key("escape", false));
        assert_eq!(chart.drawing_tool(), ChartDrawingTool::Cursor);
        assert!(!chart.engine.drawing_create_active());
        chart.clear_drawings();
        assert_eq!(
            chart.drawings_lock_summary(),
            DrawingsLockSummary::default()
        );
        assert!(!chart.apply_key("backspace", false));
    }

    #[test]
    fn drawing_history_steps_back_and_forward_and_keeps_the_armed_tool() {
        let mut chart = interactive_chart();
        assert!(!chart.can_undo_drawing());
        assert!(!chart.can_redo_drawing());
        assert!(!chart.undo_drawing());

        chart.set_drawing_tool(ChartDrawingTool::HorizontalLine);
        assert!(chart.drawing_pointer_down(300.0, 180.0, DrawingModifiers::default(), 1));
        chart.set_drawing_tool(ChartDrawingTool::HorizontalLine);
        assert!(chart.drawing_pointer_down(300.0, 240.0, DrawingModifiers::default(), 1));
        assert_eq!(chart.drawing_count(), 2);
        assert!(chart.can_undo_drawing());

        chart.set_drawing_tool(ChartDrawingTool::HorizontalLine);
        assert!(chart.undo_drawing());
        assert_eq!(chart.drawing_count(), 1);
        assert!(chart.can_redo_drawing());
        assert_eq!(chart.drawing_tool(), ChartDrawingTool::HorizontalLine);
        assert!(
            !chart.engine.drawing_create_active(),
            "stepping history must keep the tool selected without a pre-click handle"
        );

        assert!(chart.redo_drawing());
        assert_eq!(chart.drawing_count(), 2);
        assert!(!chart.can_redo_drawing());
        assert!(!chart.redo_drawing());
    }

    #[test]
    fn cursor_mode_still_falls_through_to_chart_pan_on_a_drawing_miss() {
        let mut chart = interactive_chart();
        assert!(!chart.drawing_pointer_down(300.0, 200.0, DrawingModifiers::default(), 1));

        chart.begin_drag(300.0, 200.0, 1);

        assert_eq!(chart.drag, Some(ChartDrag::Pane { price_pan: None }));
    }

    #[test]
    fn keyboard_navigation_scrolls_zooms_resets_and_ignores_unknown_keys() {
        let mut chart = interactive_chart();
        let offset = chart.engine.scroll_position();
        assert!(chart.apply_key("right", false));
        assert!((chart.engine.scroll_position() - offset - 1.0).abs() < f64::EPSILON);
        assert!(chart.apply_key("left", true));
        assert!((chart.engine.scroll_position() - offset + 9.0).abs() < f64::EPSILON);

        let page = chart.engine.pane_w / chart.engine.bar_spacing() * KEYBOARD_PAGE_FRACTION;
        let before_page = chart.engine.scroll_position();
        assert!(chart.apply_key("pageup", false));
        assert!((chart.engine.scroll_position() - before_page + page).abs() < f64::EPSILON);
        assert!(chart.apply_key("pagedown", false));
        assert!((chart.engine.scroll_position() - before_page).abs() < f64::EPSILON);

        let spacing = chart.engine.bar_spacing();
        assert!(chart.apply_key("+", false));
        assert!((chart.engine.bar_spacing() - spacing).abs() > f64::EPSILON);

        chart.engine.crosshair = Some((100.0, 100.0));
        assert!(chart.apply_key("escape", false));
        assert!(chart.engine.crosshair.is_none());
        assert!(chart.apply_key("home", false));
        assert!(chart.engine.scroll_position().abs() < f64::EPSILON);
        chart.engine.scroll_to_position(-4.0);
        assert!(!chart.is_at_latest());
        assert!(chart.apply_key("end", false));
        assert!(chart.is_at_latest());
        assert!(!chart.apply_key("a", false));
    }

    #[test]
    fn native_pointer_state_ends_a_drag_when_mouse_up_was_lost() {
        let mut chart = interactive_chart();
        chart.begin_drag(300.0, 200.0, 1);
        assert_eq!(chart.drag, Some(ChartDrag::Pane { price_pan: None }));

        chart.move_pointer(340.0, 200.0, false, DrawingModifiers::default());

        assert!(chart.drag.is_none());
        assert_eq!(chart.cursor_style, CursorStyle::Crosshair);
        assert_eq!(chart.engine.crosshair, Some((340.0, 200.0)));
    }

    #[test]
    fn indicator_separator_resize_has_bounded_native_pointer_state() {
        let mut chart = interactive_chart();
        chart
            .add_indicator(ChartIndicator::Rsi)
            .expect("RSI creates its indicator pane");
        chart.engine.recompute_layout_with_measure(true, |_| 48.0);
        assert_eq!(chart.engine.panes.len(), 2);
        let separator_y = chart.engine.panes[1].top;
        let first_stretch = chart.engine.panes[0].stretch_factor;

        chart.update_cursor(300.0, separator_y);
        assert_eq!(chart.cursor_style, CursorStyle::ResizeRow);
        assert_eq!(chart.engine.separator_hover, Some(0));

        chart.begin_drag(300.0, separator_y, 1);
        assert!(matches!(
            chart.drag,
            Some(ChartDrag::PaneSeparator { index: 0, .. })
        ));
        assert!(chart.engine.crosshair.is_none());
        chart.drag_to(300.0, separator_y + 20.0);
        assert!(chart.engine.panes[0].stretch_factor > first_stretch);

        chart.move_pointer(
            300.0,
            separator_y + 40.0,
            false,
            DrawingModifiers::default(),
        );
        assert!(chart.drag.is_none());
        assert_eq!(chart.cursor_style, CursorStyle::Crosshair);
    }

    #[test]
    fn escape_cancels_every_active_gesture_and_clears_pointer_state() {
        let mut chart = interactive_chart();
        chart.begin_drag(300.0, chart.engine.pane_h + 10.0, 1);
        assert_eq!(chart.drag, Some(ChartDrag::TimeAxis));

        assert!(chart.apply_key("escape", false));

        assert!(chart.drag.is_none());
        assert!(chart.engine.crosshair.is_none());
        assert_eq!(chart.cursor_style, CursorStyle::Crosshair);
    }

    #[test]
    fn pointer_cursor_truthfully_tracks_chart_and_axis_gestures() {
        let mut chart = interactive_chart();
        chart.update_cursor(300.0, 200.0);
        assert_eq!(chart.cursor_style, CursorStyle::Crosshair);
        chart.update_cursor(300.0, chart.engine.pane_h + 1.0);
        assert_eq!(chart.cursor_style, CursorStyle::ResizeLeftRight);
        chart.update_cursor(chart.engine.pane_w + 1.0, 200.0);
        assert_eq!(chart.cursor_style, CursorStyle::ResizeUpDown);

        chart.begin_drag(300.0, 200.0, 1);
        assert_eq!(chart.cursor_style, CursorStyle::ClosedHand);
        chart.end_drag(300.0, 200.0);
        assert_eq!(chart.cursor_style, CursorStyle::Crosshair);
    }
}
