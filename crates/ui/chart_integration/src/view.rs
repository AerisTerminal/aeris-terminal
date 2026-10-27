//! GPUI entity hosting one authoritative Aeris Charts chart engine and renderer.

use crate::bridge::{ChartBridgeMetrics, ChartDataBridge};
use crate::engine_bridge::{
    ProductPriceBars, apply_merged_chart_data, apply_product_series_markers,
    chart_data_queue_capacity, install_product_price_series, install_replay, install_volume_series,
    price_display_precision, replay_display_precision, replay_legend_title, replay_price_divisor,
    replay_quantity_divisor,
};
use crate::provenance::DisplayedProvenance;
use aeris_application::ReplayRecoveryCommand;
use aeris_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketEventProvenance, ReplaySnapshot,
    ReplayStreamUpdate, ReplayValidationError,
};
use aeris_charts_engine::AppearanceColor;
pub use aeris_charts_engine::DrawingKind as ChartDrawingKind;
#[cfg(test)]
use aeris_charts_engine::FinancialThemeColors;
use aeris_charts_engine::{
    AlertCreateRequest, AlertSnapshot, ChartEngine, ChartFrame, ChartTheme, DeltaTooltipOptions,
    DrawingId, DrawingInterval, DrawingIntervalUnit, FinancialAppearance, FinancialLegendIdentity,
    FinancialLegendRequest, FinancialSeriesColors, HostLegendSeries, IndicatorChromeOptions,
    IndicatorKind, IndicatorPane, InteractionOptions, PriceScaleMode, PriceScaleTarget,
    SeriesChromeFlag, TradingIntent, TradingSnapshot,
};
pub use aeris_charts_engine::{
    BigTradesFilter, BigTradesIntensity, BigTradesSize, CumulativeDeltaStyle,
    DepthLevel as ChartDepthLevel,
};
pub use aeris_charts_engine::{
    ExternalStudyError as ChartStudyOutputError,
    ExternalStudyInputRequirements as ChartStudyInputRequirements,
    ExternalStudyInputStream as ChartStudyInputStream,
    ExternalStudyOutputDescriptor as ChartStudyOutputDescriptor,
    ExternalStudyPaneTarget as ChartStudyPaneTarget, ExternalStudyPlotKind as ChartStudyPlotKind,
    ExternalStudyPointStyle as ChartStudyPointStyle,
    ExternalStudyScaleTarget as ChartStudyScaleTarget,
    ExternalStudyThresholdRegion as ChartStudyThresholdRegion,
};
use aeris_charts_render::color::Color;
pub use aeris_charts_render::color::Color as ChartColor;
use aeris_charts_render::draw_list::{LineStyle, Prim};
use aeris_charts_render_gpui::backend::measure_text;
use aeris_charts_render_gpui::input::{GpuiChartInput, install_text_metrics};
use aeris_charts_render_gpui::{AerisViewport, GpuiChartRenderer, PreparedAerisFrame};
use aeris_design_system::{
    AerisTheme, ThemeColor, TypographyRole, platform_date_label, platform_font_family,
    platform_font_stack, platform_typography,
};
use aeris_observability::diagnostic;
use gpui::{
    Animation, AnimationExt, AnyElement, AnyView, App, Bounds, Context, CursorStyle, Entity,
    FocusHandle, KeyDownEvent, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Render, Rgba, Role, ScrollWheelEvent, SharedString, Task,
    Transformation, Window, canvas, div, percentage, prelude::*, px, rgba, svg,
};
use num_traits::ToPrimitive;
use order_flow::{OrderFlowChartState, OrderFlowStudy};
use std::fmt;
use std::sync::Arc;
#[cfg(feature = "diagnostics")]
use std::time::Instant;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How often the surface wakes itself so the candle countdown keeps moving.
const CHART_CLOCK_INTERVAL: Duration = Duration::from_secs(1);
const MAXIMUM_SESSION_PLAN_LEVELS: usize = 32;
const SESSION_PLAN_LEVEL_COLOR: Color = Color::rgb(245, 166, 35);

#[cfg(test)]
fn aeris_charts_grid_color(theme: ChartTheme) -> String {
    FinancialThemeColors::for_theme(theme).grid.to_string()
}

fn gpui_theme_color(color: ThemeColor) -> Rgba {
    Rgba {
        r: color.red(),
        g: color.green(),
        b: color.blue(),
        a: color.alpha(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TimeAxisLabels {
    time_visible: bool,
    seconds_visible: bool,
}

impl TimeAxisLabels {
    const INTRADAY: Self = Self {
        time_visible: true,
        seconds_visible: false,
    };

    fn for_replay(replay: &ReplaySnapshot) -> Self {
        let definition = replay.bar_definition();
        let time_bars = definition.trades_per_bar.is_none() && definition.calendar_months.is_none();
        Self {
            time_visible: definition.trades_per_bar.is_some()
                || (time_bars && definition.interval_seconds < 86_400),
            seconds_visible: time_bars && definition.interval_seconds < 60,
        }
    }

    fn of(engine: &ChartEngine) -> Self {
        Self {
            time_visible: engine.time_visible,
            seconds_visible: engine.seconds_visible,
        }
    }
}

/// Bars a freshly installed market opens on, matching the runtime's visible share of its
/// first history page.
const INITIAL_VISIBLE_BARS: f64 = 600.0;
/// Trading-style breathing room to the right of the newest real bar. Future timestamps are
/// display-only; no candle or volume data is synthesized for these logical slots.
const REAL_TIME_RIGHT_OFFSET_BARS: f64 = 24.0;

/// [`platform_date_label`]'s `DD-MM-YY` in Aeris Charts' `localization.dateFormat` syntax, so the
/// crosshair time label reads dates like every other platform surface.
const PLATFORM_DATE_FORMAT: &str = "dd-MM-yy";

fn apply_platform_chrome_contract(engine: &mut ChartEngine, labels: TimeAxisLabels) {
    let options = serde_json::json!({
        "layout": {
            "fontFamily": platform_font_stack(),
        },
        "localization": {
            "dateFormat": PLATFORM_DATE_FORMAT,
        },
        "watermark": {
            "fontFamily": platform_font_stack(),
        },
        "timeScale": {
            "timeVisible": labels.time_visible,
            "secondsVisible": labels.seconds_visible,
        },
    })
    .to_string();
    engine
        .apply_options(&options)
        .expect("the platform text options derived from platform.css are valid");
}

/// The per-series engine contract a market snapshot implies: axis label precision and the
/// interval drawings test their timeframe visibility against.
fn apply_replay_contract(engine: &mut ChartEngine, replay: &ReplaySnapshot) {
    apply_platform_chrome_contract(engine, TimeAxisLabels::for_replay(replay));
    engine.set_drawing_interval(drawing_interval(replay));
}

/// The bar cadence in the largest whole unit, so a 60-minute series matches a drawing's
/// "1 hour" visibility the way traders name the interval.
fn drawing_interval(replay: &ReplaySnapshot) -> Option<DrawingInterval> {
    let definition = replay.bar_definition();
    bar_drawing_interval(
        definition.trades_per_bar,
        definition.calendar_months,
        definition.interval_seconds,
    )
}

/// A bar's span in the largest unit that divides it evenly, the form Aeris Charts matches
/// against a drawing's interval visibility.
fn bar_drawing_interval(
    trades_per_bar: Option<u32>,
    calendar_months: Option<u32>,
    seconds: u32,
) -> Option<DrawingInterval> {
    let interval = |unit, value: u32| DrawingInterval {
        unit,
        value: f64::from(value),
    };
    if let Some(trades) = trades_per_bar.filter(|&trades| trades > 0) {
        return Some(interval(DrawingIntervalUnit::Ticks, trades));
    }
    if let Some(months) = calendar_months.filter(|&months| months > 0) {
        return Some(interval(DrawingIntervalUnit::Months, months));
    }
    if seconds == 0 {
        return None;
    }
    let (unit, value) = [
        (DrawingIntervalUnit::Weeks, 604_800),
        (DrawingIntervalUnit::Days, 86_400),
        (DrawingIntervalUnit::Hours, 3_600),
        (DrawingIntervalUnit::Minutes, 60),
    ]
    .into_iter()
    .find(|&(_, unit_seconds)| seconds.is_multiple_of(unit_seconds))
    .map_or(
        (DrawingIntervalUnit::Seconds, seconds),
        |(unit, unit_seconds)| (unit, seconds / unit_seconds),
    );
    Some(interval(unit, value))
}

/// Terminal interaction policy: the reference defaults plus wheel zoom over a price axis, which
/// traders expect from professional platforms.
fn apply_platform_interaction(engine: &mut ChartEngine) {
    engine.set_interaction_options(InteractionOptions {
        price_axis_wheel_zoom: true,
        ..InteractionOptions::default()
    });
}

fn apply_replay_time_scale_defaults(engine: &mut ChartEngine, replay: &ReplaySnapshot) {
    let definition = replay.bar_definition();
    let right_offset = if definition.interval_seconds > 0
        && definition.trades_per_bar.is_none()
        && definition.calendar_months.is_none()
    {
        REAL_TIME_RIGHT_OFFSET_BARS
    } else {
        0.0
    };
    let options = serde_json::json!({ "timeScale": { "rightOffset": right_offset } }).to_string();
    engine
        .apply_options(&options)
        .expect("the platform time-scale defaults are valid");
}

const LEGEND_INSET: f32 = 8.0;
const LEGEND_ROW_HEIGHT: f32 = 24.0;
const LEGEND_MAX_WIDTH: f32 = 640.0;

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
    VolumeProfile,
    /// An Aeris Charts built-in study beyond the legacy native catalog.
    Builtin(BuiltinIndicator),
}

/// Host-portable state for one indicator instance on a chart surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChartIndicatorState {
    pub indicator: ChartIndicator,
    pub visible: bool,
    /// The volume indicator's settings; `None` for every other indicator, and restores the
    /// volume defaults.
    pub volume: Option<VolumeSettings>,
}

impl ChartIndicator {
    /// All indicators that can be calculated truthfully from the installed OHLC columns.
    pub const ALL: [Self; 11] = [
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
        Self::VolumeProfile,
    ];

    /// Distinct indicator kinds a chart can hold: the legacy catalog plus every built-in.
    pub const CATALOG_LEN: usize = Self::ALL.len() + BuiltinIndicator::ALL.len();

    /// Returns the durable workspace identifier for this indicator kind.
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::Volume => "volume",
            Self::Vwap => "vwap",
            Self::Sma => "sma",
            Self::Ema => "ema",
            Self::Wma => "wma",
            Self::Bollinger => "bollinger",
            Self::Rsi => "rsi",
            Self::Macd => "macd",
            Self::Stochastic => "stochastic",
            Self::Atr => "atr",
            Self::VolumeProfile => "volume_profile",
            Self::Builtin(indicator) => indicator.identifier(),
        }
    }

    /// Parses a durable workspace indicator identifier.
    #[must_use]
    pub fn from_identifier(value: &str) -> Option<Self> {
        Some(match value {
            "volume" => Self::Volume,
            "vwap" => Self::Vwap,
            "sma" => Self::Sma,
            "ema" => Self::Ema,
            "wma" => Self::Wma,
            "bollinger" => Self::Bollinger,
            "rsi" => Self::Rsi,
            "macd" => Self::Macd,
            "stochastic" => Self::Stochastic,
            "atr" => Self::Atr,
            "volume_profile" => Self::VolumeProfile,
            _ => return BuiltinIndicator::from_identifier(value).map(Self::Builtin),
        })
    }

    /// Returns the user-facing legacy catalog label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Volume => "Volume",
            Self::Vwap => "Volume Weighted Average Price",
            Self::Sma => "Simple Moving Average",
            Self::Ema => "Exponential Moving Average",
            Self::Wma => "Weighted Moving Average",
            Self::Bollinger => "Bollinger Bands",
            Self::Rsi => "Relative Strength Index",
            Self::Macd => "MACD",
            Self::Stochastic => "Stochastic",
            Self::Atr => "Average True Range",
            Self::VolumeProfile => "Volume Profile (Visible Range)",
            Self::Builtin(indicator) => indicator.label(),
        }
    }

    /// Returns the fixed parameters shown by the legacy indicator catalog.
    #[must_use]
    pub const fn parameters(self) -> &'static str {
        match self {
            Self::Volume => "Up/down volume · optional moving average",
            Self::Vwap => "Session anchored",
            Self::Sma | Self::Ema | Self::Wma => "Period 20",
            Self::Bollinger => "Period 20 · Deviation 2",
            Self::Rsi | Self::Atr => "Period 14",
            Self::Macd => "Fast 12 · Slow 26 · Signal 9",
            Self::Stochastic => "%K 14 · %D 3",
            Self::VolumeProfile => "Rows 48 · Value area 70%",
            Self::Builtin(indicator) => indicator.parameters(),
        }
    }

    fn from_engine_kind(kind: &IndicatorKind) -> Option<Self> {
        Some(match kind {
            IndicatorKind::Vwap => Self::Vwap,
            IndicatorKind::Sma { .. } => Self::Sma,
            IndicatorKind::Ema { .. } => Self::Ema,
            IndicatorKind::Wma { .. } => Self::Wma,
            IndicatorKind::Bollinger { .. } => Self::Bollinger,
            IndicatorKind::Rsi { .. } => Self::Rsi,
            IndicatorKind::Macd { .. } => Self::Macd,
            IndicatorKind::Stochastic { .. } => Self::Stochastic,
            IndicatorKind::Atr { .. } => Self::Atr,
            _ => return BuiltinIndicator::from_engine_kind(kind).map(Self::Builtin),
        })
    }

    fn engine_kind(self) -> Option<IndicatorKind> {
        Some(match self {
            Self::Volume | Self::VolumeProfile => return None,
            Self::Vwap => IndicatorKind::Vwap,
            Self::Sma => IndicatorKind::Sma { period: 20 },
            Self::Ema => IndicatorKind::Ema { period: 20 },
            Self::Wma => IndicatorKind::Wma { period: 20 },
            Self::Bollinger => IndicatorKind::Bollinger {
                period: 20,
                deviation: 2.0,
            },
            Self::Rsi => IndicatorKind::Rsi { period: 14 },
            Self::Macd => IndicatorKind::Macd {
                fast: 12,
                slow: 26,
                signal: 9,
            },
            Self::Stochastic => IndicatorKind::Stochastic {
                k_period: 14,
                d_period: 3,
            },
            Self::Atr => IndicatorKind::Atr { period: 14 },
            Self::Builtin(indicator) => return indicator.engine_kind(),
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
                    "Aeris Charts rejected the {} indicator",
                    indicator.label()
                )
            }
        }
    }
}

impl std::error::Error for ChartIndicatorError {}

/// Product-owned price-series presentation forwarded to Aeris Charts `SeriesKind`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ChartType {
    #[default]
    Candles,
    /// Candles with both bodies hollow.
    HollowCandles,
    /// Candles with hollow bullish bodies and solid bearish ones.
    HollowCandlesBullish,
    /// Candles with hollow bearish bodies and solid bullish ones.
    HollowCandlesBearish,
    Footprint,
    Bars,
    Line,
    LineWithMarkers,
    Area,
    Baseline,
    BrushableArea,
}

/// Renderer-neutral footprint variants persisted by the host.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FootprintDisplayMode {
    #[default]
    BidAsk,
    Total,
    Delta,
    ProfileInBar,
    VolumeLadder,
    HorizontalImbalance,
    BidAskHistogram,
}

/// Durable order-flow presentation settings. Market data and aggregation remain runtime-owned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrderFlowSettings {
    pub display_mode: FootprintDisplayMode,
    pub show_cumulative_delta: bool,
    pub cumulative_delta: CumulativeDeltaSettings,
    pub show_delta_histogram: bool,
    pub delta_histogram: DeltaHistogramSettings,
    /// Big-trades bubbles over the price series; `None` while the indicator is not added.
    pub big_trades: Option<BigTradesSettings>,
    /// Current-book bars at the price pane's right edge; `None` while the indicator is not
    /// added.
    pub order_book_depth: Option<OrderBookDepthSettings>,
    /// Recorded resting-book history drawn as a price by time heatmap under the price series;
    /// `None` while the indicator is not added.
    pub depth_heatmap: Option<DepthHeatmapSettings>,
    /// Instrument ticks per footprint row. Zero selects an automatic row size from recent bar
    /// ranges so cells stay legible on every timeframe.
    pub ticks_per_row: u32,
}

impl Default for OrderFlowSettings {
    fn default() -> Self {
        Self {
            display_mode: FootprintDisplayMode::BidAsk,
            show_cumulative_delta: false,
            cumulative_delta: CumulativeDeltaSettings::default(),
            show_delta_histogram: false,
            delta_histogram: DeltaHistogramSettings::default(),
            big_trades: None,
            order_book_depth: None,
            depth_heatmap: None,
            ticks_per_row: 0,
        }
    }
}

/// Where the cumulative delta starts again from zero.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CumulativeDeltaReset {
    /// At each trading session open.
    #[default]
    Session,
    /// Never: the delta accumulates across every bar on the chart.
    Continuous,
}

/// Durable CVD study settings. A color left at `None` follows the chart default: the series
/// line color for lines and the chart's up and down colors for candles.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CumulativeDeltaSettings {
    pub style: CumulativeDeltaStyle,
    pub reset: CumulativeDeltaReset,
    pub line_color: Option<ChartColor>,
    pub up_color: Option<ChartColor>,
    pub down_color: Option<ChartColor>,
}

/// Durable delta histogram settings. A color left at `None` follows the order-flow buy (positive)
/// and sell (negative) colors.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeltaHistogramSettings {
    pub up_color: Option<ChartColor>,
    pub down_color: Option<ChartColor>,
}

/// Durable order-book indicator settings. A color left at `None` follows the chart's up (bid)
/// and down (ask) colors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrderBookDepthSettings {
    pub visible: bool,
    pub levels_per_side: u16,
    /// Length in logical pixels of the largest level's bar; every other bar scales against it.
    pub max_width: u16,
    pub opacity_percent: u8,
    pub show_sizes: bool,
    pub bid_color: Option<ChartColor>,
    pub ask_color: Option<ChartColor>,
}

impl OrderBookDepthSettings {
    /// Book levels drawn on each side the settings accept.
    pub const LEVELS_PER_SIDE: std::ops::RangeInclusive<u16> = 1..=200;
    /// Largest-bar lengths the settings accept, in logical pixels.
    pub const MAX_WIDTHS: std::ops::RangeInclusive<u16> = 20..=400;
    /// Bar opacities the settings accept, in percent.
    pub const OPACITY_PERCENTS: std::ops::RangeInclusive<u8> = 10..=100;

    /// Whether every bounded value is inside its accepted range.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        Self::LEVELS_PER_SIDE.contains(&self.levels_per_side)
            && Self::MAX_WIDTHS.contains(&self.max_width)
            && Self::OPACITY_PERCENTS.contains(&self.opacity_percent)
    }
}

impl Default for OrderBookDepthSettings {
    fn default() -> Self {
        Self {
            visible: true,
            levels_per_side: 40,
            max_width: 120,
            opacity_percent: 50,
            show_sizes: true,
            bid_color: None,
            ask_color: None,
        }
    }
}

/// Durable depth heatmap settings. Aeris Charts draws the recorded book in the chart's up (bid)
/// and down (ask) colors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DepthHeatmapSettings {
    pub visible: bool,
}

impl Default for DepthHeatmapSettings {
    fn default() -> Self {
        Self { visible: true }
    }
}

/// The colors order-flow indicators draw where their settings leave a color unset, as
/// `#RRGGBB` or `#RRGGBBAA` for the color picker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderFlowDefaultColors {
    pub cumulative_delta_line: String,
    /// The chart's up color: rising CVD candles and order-book bids.
    pub bullish: String,
    /// The chart's down color: falling CVD candles and order-book asks.
    pub bearish: String,
    pub delta_positive: String,
    pub delta_negative: String,
}

/// Durable big-trades indicator settings. Aeris Charts rebuilds and filters the orders; the
/// bubble colors always follow the platform theme tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BigTradesSettings {
    pub filter: BigTradesFilter,
    pub size: BigTradesSize,
    pub show_volume: bool,
    pub visible: bool,
}

impl Default for BigTradesSettings {
    fn default() -> Self {
        let defaults = aeris_charts_engine::BigTradesOptions::default();
        Self {
            filter: defaults.filter,
            size: defaults.size,
            show_volume: defaults.show_volume,
            visible: defaults.visible,
        }
    }
}

/// Durable volume indicator settings. Aeris Charts computes and draws the columns and their
/// moving average; a color left at `None` follows the Aeris Charts default.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VolumeSettings {
    pub up_color: Option<String>,
    pub down_color: Option<String>,
    pub average: VolumeAverageSettings,
    pub placement: VolumePlacement,
}

/// The moving average drawn over the volume columns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolumeAverageSettings {
    pub enabled: bool,
    pub kind: VolumeAverageKind,
    pub length: u16,
    pub color: Option<String>,
}

impl Default for VolumeAverageSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            kind: VolumeAverageKind::Simple,
            length: 20,
            color: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VolumeAverageKind {
    #[default]
    Simple,
    Exponential,
}

/// Where the volume columns draw.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VolumePlacement {
    /// Along the bottom of the price pane, without a scale of their own.
    #[default]
    PricePane,
    /// In a pane of their own below the price pane.
    OwnPane,
}

impl VolumeSettings {
    /// Average lengths the settings accept, in bars.
    pub const AVERAGE_LENGTHS: std::ops::RangeInclusive<u16> = 1..=500;

    /// The rising-bar column color as drawn, as `#RRGGBB` or `#RRGGBBAA`.
    #[must_use]
    pub fn effective_up_color(&self) -> String {
        self.up_color
            .clone()
            .unwrap_or_else(|| chart_color_hex(aeris_charts_engine::DEFAULT_VOLUME_UP_COLOR))
    }

    /// The falling-bar column color as drawn, as `#RRGGBB` or `#RRGGBBAA`.
    #[must_use]
    pub fn effective_down_color(&self) -> String {
        self.down_color
            .clone()
            .unwrap_or_else(|| chart_color_hex(aeris_charts_engine::DEFAULT_VOLUME_DOWN_COLOR))
    }

    /// The moving-average line color as drawn, as `#RRGGBB` or `#RRGGBBAA`.
    #[must_use]
    pub fn effective_average_color(&self) -> String {
        self.average
            .color
            .clone()
            .unwrap_or_else(|| chart_color_hex(aeris_charts_engine::DEFAULT_LINE_COLOR))
    }
}

/// A chart color as `#RRGGBB`, or `#RRGGBBAA` when it is translucent.
#[must_use]
pub fn chart_color_hex(color: Color) -> String {
    if color.a() == u8::MAX {
        format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b())
    } else {
        format!(
            "#{:02X}{:02X}{:02X}{:02X}",
            color.r(),
            color.g(),
            color.b(),
            color.a()
        )
    }
}

/// A chart settings request (a legend settings control or a drawing double-click) the host
/// answers by opening that item's settings dialog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChartSettingsRequest {
    /// One runtime-managed study, by its runtime study identity.
    Study(u64),
    BigTrades,
    Volume,
    CumulativeDelta,
    DeltaHistogram,
    OrderBookDepth,
    /// One drawing, by its Aeris Charts identity.
    Drawing(DrawingId),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OrderFlowAggregation {
    TimeMicros(u64),
    Trades(u32),
    Volume(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrderFlowTrade {
    pub ingestion_ordinal: u64,
    pub timestamp_micros: i64,
    pub price: f64,
    pub volume: f64,
    pub aggressor: aeris_charts_engine::AggressorSide,
    /// Exchange trading session of the print. Session CVD resets and the footprint's session
    /// history budget follow it; `None` treats the tape as one continuous session.
    pub session_id: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrderFlowSweep {
    pub first_ingestion_ordinal: u64,
    pub last_ingestion_ordinal: u64,
    pub timestamp_micros: i64,
    pub terminal_price: f64,
    pub total_volume: f64,
    pub price_levels: u32,
    pub aggressor: aeris_charts_engine::AggressorSide,
}

/// Aeris Charts-owned typed financial appearance, re-exported under the Terminal API name.
pub type ChartAppearanceSettings = FinancialAppearance;
/// The painted primary-series colors, in the hex form [`AerisChartView::series_colors`] returns.
pub type ChartSeriesColors = FinancialSeriesColors;

/// Which parts of the chart legend show. Presentation only: hiding a legend never hides the
/// series or indicator it describes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // Four independent legend toggles.
pub struct ChartLegendOptions {
    /// The symbol row.
    pub symbol: bool,
    /// The symbol row's open, high, low and close readouts.
    pub symbol_values: bool,
    /// The rows of indicators, studies and order-flow tools.
    pub indicators: bool,
    /// The readouts on those rows.
    pub indicator_values: bool,
}

impl Default for ChartLegendOptions {
    fn default() -> Self {
        Self {
            symbol: true,
            symbol_values: true,
            indicators: true,
            indicator_values: true,
        }
    }
}

/// The CSS keyword Aeris Charts reads as "no fill"; a candle body set to it is hollow.
const HOLLOW_BODY: &str = "transparent";

/// Hollow candles from the user's appearance: each hollow side gets a transparent body, and
/// its border and wick are pinned to that side's visible color, because an unpinned part
/// follows the body and would vanish with it. Colors the user pinned are kept, and borders
/// stay on so the frame shows.
fn hollow_appearance(
    base: &ChartAppearanceSettings,
    bullish: bool,
    bearish: bool,
    theme: ChartTheme,
) -> ChartAppearanceSettings {
    fn pinned(part: &AppearanceColor, body: &str) -> AppearanceColor {
        match part {
            AppearanceColor::Custom(color) => AppearanceColor::Custom(color.clone()),
            AppearanceColor::Theme => AppearanceColor::Custom(body.to_string()),
        }
    }
    let mut hollow = base.clone();
    if bullish {
        let body = base.effective_up_color(theme);
        hollow.border_up_color = pinned(&base.border_up_color, &body);
        hollow.wick_up_color = pinned(&base.wick_up_color, &body);
        hollow.up_color = AppearanceColor::Custom(HOLLOW_BODY.to_string());
    }
    if bearish {
        let body = base.effective_down_color(theme);
        hollow.border_down_color = pinned(&base.border_down_color, &body);
        hollow.wick_down_color = pinned(&base.wick_down_color, &body);
        hollow.down_color = AppearanceColor::Custom(HOLLOW_BODY.to_string());
    }
    hollow.border_visible = true;
    hollow
}

/// `appearance` with its chart-canvas fields taken from `canvas`; every series field is kept.
fn with_canvas_of(
    mut appearance: ChartAppearanceSettings,
    canvas: &ChartAppearanceSettings,
) -> ChartAppearanceSettings {
    appearance
        .background_color
        .clone_from(&canvas.background_color);
    appearance.text_color.clone_from(&canvas.text_color);
    appearance.grid_visible = canvas.grid_visible;
    appearance.grid_color.clone_from(&canvas.grid_color);
    appearance.grid_style = canvas.grid_style;
    appearance
        .crosshair_color
        .clone_from(&canvas.crosshair_color);
    appearance.crosshair_width = canvas.crosshair_width;
    appearance.crosshair_style = canvas.crosshair_style;
    appearance
}

impl ChartType {
    const fn shows_ohlc_legend(self) -> bool {
        matches!(
            self,
            Self::Candles
                | Self::HollowCandles
                | Self::HollowCandlesBullish
                | Self::HollowCandlesBearish
                | Self::Footprint
                | Self::Bars
        )
    }

    /// Which candle bodies are hollow, as `(bullish, bearish)`; `None` for every solid type.
    /// Aeris Charts has no hollow series: a hollow body is a transparent body color.
    const fn hollow_sides(self) -> Option<(bool, bool)> {
        match self {
            Self::HollowCandles => Some((true, true)),
            Self::HollowCandlesBullish => Some((true, false)),
            Self::HollowCandlesBearish => Some((false, true)),
            _ => None,
        }
    }

    /// Built-in OHLC chart types Aeris Charts can render from the product price series.
    pub const ALL: [Self; 11] = [
        Self::Candles,
        Self::HollowCandles,
        Self::HollowCandlesBullish,
        Self::HollowCandlesBearish,
        Self::Footprint,
        Self::Bars,
        Self::Line,
        Self::LineWithMarkers,
        Self::Area,
        Self::Baseline,
        Self::BrushableArea,
    ];

    /// Returns the header and menu label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Candles => "Candles",
            Self::HollowCandles => "Hollow candles",
            Self::HollowCandlesBullish => "Hollow candles: bullish only",
            Self::HollowCandlesBearish => "Hollow candles: bearish only",
            Self::Footprint => "Footprint",
            Self::Bars => "Bars",
            Self::Line => "Line",
            Self::LineWithMarkers => "Line with markers",
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
            Self::HollowCandles => "hollow_candles",
            Self::HollowCandlesBullish => "hollow_candles_bullish",
            Self::HollowCandlesBearish => "hollow_candles_bearish",
            Self::Footprint => "footprint",
            Self::Bars => "bars",
            Self::Line => "line",
            Self::LineWithMarkers => "line_with_markers",
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

    /// Values the product price series holds for one OHLC bar. A footprint keeps only the bar
    /// grid on the price series (whitespace), so no candle exists behind or beside it and bars
    /// the trade tape does not cover stay empty.
    pub(crate) const fn primary_values(self, ohlc: [f64; 4]) -> [f64; 4] {
        match self {
            Self::Footprint => [f64::NAN; 4],
            _ => ohlc,
        }
    }

    pub(crate) const fn series_kind(self) -> aeris_charts_engine::SeriesKind {
        match self {
            Self::Candles
            | Self::HollowCandles
            | Self::HollowCandlesBullish
            | Self::HollowCandlesBearish
            | Self::Footprint => aeris_charts_engine::SeriesKind::Candlestick,
            Self::Bars => aeris_charts_engine::SeriesKind::Bar,
            Self::Line | Self::LineWithMarkers => aeris_charts_engine::SeriesKind::Line,
            Self::Area | Self::BrushableArea => aeris_charts_engine::SeriesKind::Area,
            Self::Baseline => aeris_charts_engine::SeriesKind::Baseline,
        }
    }
}

/// Distinguishes a pane-canvas right-click from a price-axis right-click.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChartContextKind {
    Pane,
    PriceAxis { pane: usize, left: bool },
}

/// A chart-surface right-click waiting for the shell to present a menu.
#[derive(Clone, Debug, PartialEq)]
pub struct ChartContextRequest {
    pub position: Point<Pixels>,
    pub kind: ChartContextKind,
    /// Aeris Charts-formatted price at the right-click, when the click was on the pane.
    pub copy_price: Option<SharedString>,
}

/// Aeris Charts-owned price-axis chrome the Y-axis menu presents.
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

const fn price_scale_mode_code(mode: PriceScaleMode) -> u8 {
    match mode {
        PriceScaleMode::Normal => 0,
        PriceScaleMode::Logarithmic => 1,
        PriceScaleMode::Percentage => 2,
        PriceScaleMode::IndexedTo100 => 3,
    }
}

/// Aggregate state used by drawing-toolbar lock controls.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrawingsLockSummary {
    pub total: usize,
    pub locked_count: usize,
    pub all_locked: bool,
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
    VolumeProfile,
    Indicator(u32),
    Study { study_id: u64, series_id: u32 },
    OrderFlow(OrderFlowStudy),
    BigTrades,
    OrderBookDepth,
    DepthHeatmap,
}

/// The volume indicator on a chart: the outputs of the Aeris Charts binding drawing it and the
/// settings it was built from. The columns output is also the binding identity.
#[derive(Clone, Debug)]
struct VolumeIndicator {
    columns: u32,
    average: u32,
    settings: VolumeSettings,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PointerInteractionState {
    #[default]
    Active,
    Suspended,
}

impl LegendItem {
    fn key(self) -> u64 {
        match self {
            Self::Asset => 0,
            Self::Volume => 1,
            Self::Indicator(binding) => u64::from(binding) + 2,
            Self::Study { series_id, .. } => (1_u64 << 63) | u64::from(series_id),
            Self::OrderFlow(study) => (1_u64 << 62) | study as u64,
            Self::VolumeProfile => 1_u64 << 61,
            Self::BigTrades => 1_u64 << 60,
            Self::OrderBookDepth => 1_u64 << 59,
            Self::DepthHeatmap => 1_u64 << 58,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LegendRow {
    item: LegendItem,
    pane: usize,
    title: String,
    /// One entry per readout (`O 77,876.69`, `%K 36.95`, …) so a legend that outgrows its pane
    /// wraps value by value instead of spilling over the price axis.
    values: Vec<LegendValue>,
    visible: bool,
    settings_available: bool,
}

/// A readout without its own series color reads neutral. The symbol's OHLC never takes the bar
/// direction: flipping the values between the up and down colors reads as the text jumping.
#[derive(Clone, Debug, Eq, PartialEq)]
struct LegendValue {
    text: String,
    color: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct LegendPaneLayout {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

#[derive(Clone, Copy)]
struct LegendPalette {
    text: Rgba,
    muted: Rgba,
    hover: Rgba,
    danger: Rgba,
}

const LEGEND_VIEW_ICON: &str = "aeris/icons/ui/view.svg";
const LEGEND_VIEW_OFF_ICON: &str = "aeris/icons/ui/view-off.svg";
const LEGEND_SETTINGS_ICON: &str = "aeris/icons/ui/settings.svg";
const LEGEND_REMOVE_ICON: &str = "aeris/icons/ui/close.svg";
const LEGEND_LOADING_ICON: &str = "aeris/icons/ui/loader.svg";
/// One rotation of the legend's loading glyph.
const LEGEND_LOADING_PERIOD: Duration = Duration::from_millis(700);

fn platform_tabular_numerals() -> gpui::FontFeatures {
    gpui::FontFeatures(Arc::new(vec![(
        platform_typography().tabular_numerals_feature().to_owned(),
        1,
    )]))
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

/// A GPUI entity hosting one authoritative Aeris Charts chart engine and renderer.
pub struct AerisChartView {
    engine: ChartEngine,
    /// The platform theme the chart paints in; its mode selects the Aeris Charts theme.
    platform: AerisTheme,
    /// Chart colors of a non-default platform preset; `None` keeps Aeris Charts' own theme.
    palette: Option<PresetPalette>,
    renderer: GpuiChartRenderer,
    data_bridge: Option<ChartDataBridge>,
    displayed_provenance: DisplayedProvenance,
    price_divisor: f64,
    quantity_divisor: f64,
    /// Product volume weights, always allocated and never drawn: the volume indicator, volume
    /// profile and data-bound drawings read it.
    volume_series: u32,
    volume: Option<VolumeIndicator>,
    /// Engine handle of the visible-range volume profile drawn on the price series.
    volume_profile: Option<aeris_charts_engine::NativePrimitiveId>,
    asset_legend_title: String,
    provider_display_name: Option<String>,
    /// Instrument symbol of the loaded series, naming exported chart images.
    asset_symbol: String,
    /// A load the trader is waiting on, shown by the symbol legend itself so the
    /// notice sits where they are already reading the symbol.
    asset_loading: LegendPresence,
    legend_panes: Vec<LegendPaneLayout>,
    frame: ChartFrame,
    axis_prims: Vec<Prim>,
    built_for: (f32, f32, f32),
    layout_dirty: bool,
    fitted: bool,
    /// Host-selected presentation per external study, kept for outputs installed later.
    study_styles: std::collections::BTreeMap<u64, studies::StudyStyle>,
    /// The color Aeris Charts gave each external study output when it was installed.
    study_default_colors: std::collections::BTreeMap<(u64, usize), Option<String>>,
    /// Separate-pane heights the user chose, restored onto panes as they appear.
    pane_heights: pane_heights::PaneHeights,
    /// Window bounds of the chart canvas from the latest prepaint.
    viewport_bounds: Bounds<Pixels>,
    /// GPUI event translation; every interaction decision lives in the Aeris Charts controller.
    input: GpuiChartInput,
    /// The single scheduled wake for deferred engine input work (trading-tooltip dwell).
    input_wake: Option<Task<()>>,
    focus_handle: Option<FocusHandle>,
    pointer_interaction: PointerInteractionState,
    pending_context_menu: Option<ChartContextRequest>,
    pending_activate: ActivationRequest,
    pending_settings_request: Option<ChartSettingsRequest>,
    pending_study_remove: Option<u64>,
    instrument_price_precision: u8,
    instrument_price_increment: Option<i64>,
    instrument_price_scale: u8,
    price_precision_override: Option<u8>,
    chart_type: ChartType,
    /// The user's own appearance while Aeris Charts paints a derived one: a preset palette
    /// pins theme-following colors and a hollow chart type makes bodies transparent. This keeps
    /// what the user chose, with its theme provenance, so readback, persistence, a preset
    /// switch and a return to solid candles never see the derivation.
    appearance_intent: Option<ChartAppearanceSettings>,
    /// Host-owned axis colors, painted after every engine theme or canvas change.
    axis_appearance: ChartAxisAppearance,
    order_flow_settings: OrderFlowSettings,
    order_flow_state: Option<OrderFlowChartState>,
    order_book_depth_state: Option<OrderBookDepthState>,
    depth_heatmap_state: Option<DepthHeatmapState>,
    product_bars: ProductPriceBars,
    legend_options: ChartLegendOptions,
    /// Host content placed under the symbol legend row, such as the trading host's order
    /// buttons. The chart only positions it; the host owns its state and behavior.
    legend_accessory: Option<AnyView>,
    /// Transient host-projected plan levels. These are deliberately separate
    /// from Aeris Charts-owned user drawings and are reinstalled with price series.
    session_plan_levels: Vec<(f64, String)>,
    session_plan_price_line_ids: Vec<u32>,
    /// Monotonic revision of stable user-authored chart presentation state.
    /// Market-data updates, hover, cursor and transient gestures never touch it.
    user_state_revision: u64,
    /// The pending one-second self-wake. Held so only one is ever in flight.
    clock_tick: Option<Task<()>>,
    /// Monotonic presentation-clock revision. It advances once per scheduled clock wake and is
    /// intentionally separate from durable user state so a ticking header never dirties storage.
    clock_revision: u64,
    #[cfg(feature = "diagnostics")]
    last_snapshot_installation_nanos: Option<u64>,
    #[cfg(feature = "diagnostics")]
    live_evidence_enabled: bool,
    #[cfg(feature = "diagnostics")]
    live_evidence_rebuilds: u16,
    #[cfg(feature = "diagnostics")]
    live_evidence_mouse_downs: u8,
}

impl AerisChartView {
    /// Drains bounded, user-originated crosshair/time-range events for a host link coordinator.
    pub fn take_sync_events(&mut self) -> Vec<aeris_charts_engine::ChartSyncEvent> {
        self.engine.take_sync_events()
    }

    /// Applies a coordinator-originated event without echoing it back to the coordinator.
    pub fn apply_external_sync_event(
        &mut self,
        kind: &aeris_charts_engine::ChartSyncEventKind,
    ) -> bool {
        let changed = self.engine.apply_external_sync_event(kind);
        if changed {
            self.layout_dirty = true;
        }
        changed
    }

    /// Creates an empty Aeris Charts-owned surface without inventing market data.
    #[must_use]
    pub fn empty() -> Self {
        Self::empty_with_theme(AerisTheme::dark())
    }

    /// Creates an empty chart using Aeris Charts' canonical theme tokens.
    ///
    /// # Panics
    ///
    /// Panics when the platform font options derived from `platform.css` stop
    /// parsing.
    #[must_use]
    pub fn empty_with_theme(theme: AerisTheme) -> Self {
        let mut engine = ChartEngine::new(1024.0, 640.0, 1.0);
        engine.set_theme(chart_theme(theme.mode));
        register_drawing_stamps(&mut engine, &theme);
        apply_platform_chrome_contract(&mut engine, TimeAxisLabels::INTRADAY);
        apply_platform_interaction(&mut engine);
        let volume_series = install_volume_series(&mut engine);
        let mut chart = Self {
            engine,
            platform: theme,
            palette: PresetPalette::for_platform(&theme),
            renderer: GpuiChartRenderer::new(),
            data_bridge: None,
            displayed_provenance: DisplayedProvenance::empty(),
            price_divisor: 1.0,
            quantity_divisor: 1.0,
            volume_series,
            volume: None,
            asset_legend_title: String::new(),
            provider_display_name: None,
            asset_symbol: String::new(),
            asset_loading: LegendPresence::Absent,
            legend_panes: Vec::new(),
            frame: ChartFrame::default(),
            axis_prims: Vec::new(),
            built_for: (0.0, 0.0, 0.0),
            layout_dirty: true,
            fitted: false,
            study_styles: std::collections::BTreeMap::new(),
            study_default_colors: std::collections::BTreeMap::new(),
            pane_heights: pane_heights::PaneHeights::default(),
            volume_profile: None,
            viewport_bounds: Bounds::default(),
            input: GpuiChartInput::default(),
            input_wake: None,
            focus_handle: None,
            pointer_interaction: PointerInteractionState::Active,
            pending_context_menu: None,
            pending_activate: ActivationRequest::None,
            pending_settings_request: None,
            pending_study_remove: None,
            instrument_price_precision: 2,
            instrument_price_increment: None,
            instrument_price_scale: 2,
            price_precision_override: None,
            chart_type: ChartType::Candles,
            appearance_intent: None,
            axis_appearance: ChartAxisAppearance::default(),
            order_flow_settings: OrderFlowSettings::default(),
            order_flow_state: None,
            order_book_depth_state: None,
            depth_heatmap_state: None,
            product_bars: ProductPriceBars::default(),
            legend_options: ChartLegendOptions::default(),
            legend_accessory: None,
            session_plan_levels: Vec::new(),
            session_plan_price_line_ids: Vec::new(),
            user_state_revision: 0,
            clock_tick: None,
            clock_revision: 0,
            #[cfg(feature = "diagnostics")]
            last_snapshot_installation_nanos: None,
            #[cfg(feature = "diagnostics")]
            live_evidence_enabled: std::env::var_os("AERIS_LIVE_EVIDENCE").is_some(),
            #[cfg(feature = "diagnostics")]
            live_evidence_rebuilds: 0,
            #[cfg(feature = "diagnostics")]
            live_evidence_mouse_downs: 0,
        };
        chart.sync_painted_appearance();
        chart
    }

    /// Creates a chart from the bounded embedded replay using Aeris Charts' own styling.
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
        Self::with_replay_and_theme(replay, AerisTheme::dark())
    }

    /// Creates a replay-backed chart using Aeris Charts' canonical theme tokens.
    ///
    /// # Panics
    ///
    /// Panics when the platform font options derived from `platform.css` stop
    /// parsing.
    #[must_use]
    pub fn with_replay_and_theme(replay: &ReplaySnapshot, theme: AerisTheme) -> Self {
        let mut engine = ChartEngine::new(1024.0, 640.0, 1.0);
        engine.set_theme(chart_theme(theme.mode));
        register_drawing_stamps(&mut engine, &theme);
        apply_replay_contract(&mut engine, replay);
        apply_platform_interaction(&mut engine);
        apply_replay_time_scale_defaults(&mut engine, replay);
        let volume_series = install_volume_series(&mut engine);
        let mut product_bars = ProductPriceBars::default();
        install_replay(
            &mut engine,
            volume_series,
            replay,
            ChartType::Candles,
            &mut product_bars,
        );
        let data_bridge = ChartDataBridge::try_new(chart_data_queue_capacity(), replay).ok();
        debug_assert!(data_bridge.is_some());
        let mut chart = Self {
            engine,
            platform: theme,
            palette: PresetPalette::for_platform(&theme),
            renderer: GpuiChartRenderer::new(),
            data_bridge,
            displayed_provenance: DisplayedProvenance::from_snapshot(replay),
            price_divisor: replay_price_divisor(replay),
            quantity_divisor: replay_quantity_divisor(replay),
            volume_series,
            volume: None,
            asset_legend_title: replay_legend_title(replay, None),
            provider_display_name: None,
            asset_symbol: replay.instrument().symbol.clone(),
            asset_loading: LegendPresence::Absent,
            legend_panes: Vec::new(),
            frame: ChartFrame::default(),
            axis_prims: Vec::new(),
            built_for: (0.0, 0.0, 0.0),
            layout_dirty: true,
            fitted: false,
            study_styles: std::collections::BTreeMap::new(),
            study_default_colors: std::collections::BTreeMap::new(),
            pane_heights: pane_heights::PaneHeights::default(),
            volume_profile: None,
            viewport_bounds: Bounds::default(),
            input: GpuiChartInput::default(),
            input_wake: None,
            focus_handle: None,
            pointer_interaction: PointerInteractionState::Active,
            pending_context_menu: None,
            pending_activate: ActivationRequest::None,
            pending_settings_request: None,
            pending_study_remove: None,
            instrument_price_precision: replay_display_precision(replay),
            instrument_price_increment: replay.instrument().price_increment,
            instrument_price_scale: replay.instrument().precision.price_scale(),
            price_precision_override: None,
            chart_type: ChartType::Candles,
            appearance_intent: None,
            axis_appearance: ChartAxisAppearance::default(),
            order_flow_settings: OrderFlowSettings::default(),
            order_flow_state: None,
            order_book_depth_state: None,
            depth_heatmap_state: None,
            product_bars,
            legend_options: ChartLegendOptions::default(),
            legend_accessory: None,
            session_plan_levels: Vec::new(),
            session_plan_price_line_ids: Vec::new(),
            user_state_revision: 0,
            clock_tick: None,
            clock_revision: 0,
            #[cfg(feature = "diagnostics")]
            last_snapshot_installation_nanos: None,
            #[cfg(feature = "diagnostics")]
            live_evidence_enabled: std::env::var_os("AERIS_LIVE_EVIDENCE").is_some(),
            #[cfg(feature = "diagnostics")]
            live_evidence_rebuilds: 0,
            #[cfg(feature = "diagnostics")]
            live_evidence_mouse_downs: 0,
        };
        chart.sync_painted_appearance();
        chart.apply_selected_price_format();
        chart
    }

    /// Uses the selected provider's published name rather than deriving it from a venue code.
    pub fn set_provider_display_name(&mut self, name: &str, replay: &ReplaySnapshot) {
        self.provider_display_name = Some(name.to_string());
        self.asset_legend_title = replay_legend_title(replay, Some(name));
    }

    #[must_use]
    pub fn legend_title(&self) -> &str {
        &self.asset_legend_title
    }

    /// Replaces Aeris Charts' authoritative series data with one validated snapshot.
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
        apply_replay_contract(&mut self.engine, replay);
        apply_replay_time_scale_defaults(&mut self.engine, replay);
        self.apply_price_series_kind();
        self.displayed_provenance.replace_snapshot(replay);
        self.asset_legend_title =
            replay_legend_title(replay, self.provider_display_name.as_deref());
        self.asset_symbol.clone_from(&replay.instrument().symbol);
        self.price_divisor = replay_price_divisor(replay);
        self.quantity_divisor = replay_quantity_divisor(replay);
        self.instrument_price_precision = replay_display_precision(replay);
        self.instrument_price_increment = replay.instrument().price_increment;
        self.instrument_price_scale = replay.instrument().precision.price_scale();
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

    /// Restores Aeris Charts' native default time scale and automatic price scales.
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

    /// Takes the latest pending host request to open a legend item's settings.
    pub fn take_settings_request(&mut self) -> Option<ChartSettingsRequest> {
        self.pending_settings_request.take()
    }

    /// Takes a pending host request to remove one runtime-managed study.
    pub fn take_study_remove_request(&mut self) -> Option<u64> {
        self.pending_study_remove.take()
    }

    /// Places host content under the symbol legend row of the main pane, or removes it.
    /// Presses on it stay with the host and never reach chart input.
    pub fn set_legend_accessory(&mut self, accessory: Option<AnyView>) {
        self.legend_accessory = accessory;
    }

    /// Reads Aeris Charts-owned Y-axis chrome for the hit-tested price scale.
    #[must_use]
    pub fn price_axis_menu_state(&self, pane: usize, left: bool) -> Option<PriceAxisMenuState> {
        let target = price_axis_target(left);
        let primary = self.engine.primary_series_on_price_scale(pane, target)?;
        let mut flags = 0;
        if self.product_price_line_visible() {
            flags |= PriceAxisMenuState::PRICE_LINE;
        }
        if primary.last_value_visible {
            flags |= PriceAxisMenuState::LAST_VALUE;
        }
        if primary.title_visible {
            flags |= PriceAxisMenuState::TITLE;
        }
        if primary.countdown_visible {
            flags |= PriceAxisMenuState::COUNTDOWN;
        }
        let chrome = self.engine.indicator_chrome_options();
        if chrome.name_labels_visible {
            flags |= PriceAxisMenuState::INDICATOR_NAMES;
        }
        if chrome.value_labels_visible {
            flags |= PriceAxisMenuState::INDICATOR_VALUES;
        }
        if chrome.price_lines_visible {
            flags |= PriceAxisMenuState::INDICATOR_PRICE_LINES;
        }
        if self.engine.price_scale_auto_scale_for(pane, target)? {
            flags |= PriceAxisMenuState::AUTO_SCALE;
        }
        if self.engine.price_scale_inverted_for(pane, target)? {
            flags |= PriceAxisMenuState::INVERT_SCALE;
        }
        if primary.bid_ask_visible {
            flags |= PriceAxisMenuState::BID_ASK;
        }
        if self.engine.price_scale_align_labels_for(pane, target)? {
            flags |= PriceAxisMenuState::ALIGN_LABELS;
        }
        Some(PriceAxisMenuState {
            flags,
            mode: price_scale_mode_code(self.engine.price_scale_mode_for(pane, target)?),
            left,
            precision: self.price_precision_override,
        })
    }

    /// Returns the explicit main price-axis precision selected by the host.
    ///
    /// `None` means the chart is intentionally following the instrument's automatic precision.
    #[must_use]
    pub const fn selected_price_precision(&self) -> Option<u8> {
        self.price_precision_override
    }

    /// Applies one Y-axis menu command through Aeris Charts' scale and series APIs.
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
            PriceAxisMenuAction::TogglePriceLine => self
                .engine
                .toggle_series_chrome(0, SeriesChromeFlag::PriceLine),
            PriceAxisMenuAction::ToggleLastValue => self
                .engine
                .toggle_series_chrome(primary_id, SeriesChromeFlag::LastValue),
            PriceAxisMenuAction::ToggleTitle => self
                .engine
                .toggle_series_chrome(primary_id, SeriesChromeFlag::Title),
            PriceAxisMenuAction::ToggleCountdown => self
                .engine
                .toggle_series_chrome(primary_id, SeriesChromeFlag::Countdown),
            PriceAxisMenuAction::ToggleIndicatorNameLabels => self.toggle_indicator_name_labels(),
            PriceAxisMenuAction::ToggleIndicatorValueLabels => self.toggle_indicator_value_labels(),
            PriceAxisMenuAction::ToggleIndicatorPriceLines => self.toggle_indicator_price_lines(),
            PriceAxisMenuAction::ToggleBidAsk => self
                .engine
                .toggle_series_chrome(primary_id, SeriesChromeFlag::BidAsk),
            PriceAxisMenuAction::ToggleAlignLabels => {
                self.engine.toggle_price_scale_align_labels(pane, target)
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
            PriceAxisMenuAction::SetMode(mode) if mode <= 3 => {
                let mode = match mode {
                    1 => PriceScaleMode::Logarithmic,
                    2 => PriceScaleMode::Percentage,
                    3 => PriceScaleMode::IndexedTo100,
                    _ => PriceScaleMode::Normal,
                };
                self.engine.set_price_scale_mode_for(pane, target, mode);
                true
            }
            PriceAxisMenuAction::SetMode(_) => false,
            PriceAxisMenuAction::SetLeft(next_left) => self.move_price_axis(pane, left, next_left),
            PriceAxisMenuAction::SetPrecision(precision) => self.set_price_precision(precision),
        };
        if applied {
            self.mark_user_state_changed();
            self.invalidate_series_layout();
        }
        applied
    }

    /// Restores a previously captured stable price-axis state by replaying the
    /// same public actions used by the menu, so Aeris Charts remains authoritative.
    pub fn restore_price_axis_menu_state(&mut self, desired: PriceAxisMenuState) -> bool {
        let mut current = self
            .price_axis_menu_state(0, false)
            .or_else(|| self.price_axis_menu_state(0, true));
        let Some(mut current_state) = current else {
            return false;
        };
        if current_state.left != desired.left {
            let _ = self.apply_price_axis_menu_action(
                0,
                current_state.left,
                PriceAxisMenuAction::SetLeft(desired.left),
            );
            current = self.price_axis_menu_state(0, desired.left);
            let Some(next) = current else {
                return false;
            };
            current_state = next;
        }
        let flags = [
            (
                PriceAxisMenuState::PRICE_LINE,
                PriceAxisMenuAction::TogglePriceLine,
            ),
            (
                PriceAxisMenuState::LAST_VALUE,
                PriceAxisMenuAction::ToggleLastValue,
            ),
            (PriceAxisMenuState::TITLE, PriceAxisMenuAction::ToggleTitle),
            (
                PriceAxisMenuState::COUNTDOWN,
                PriceAxisMenuAction::ToggleCountdown,
            ),
            (
                PriceAxisMenuState::INDICATOR_NAMES,
                PriceAxisMenuAction::ToggleIndicatorNameLabels,
            ),
            (
                PriceAxisMenuState::INDICATOR_VALUES,
                PriceAxisMenuAction::ToggleIndicatorValueLabels,
            ),
            (
                PriceAxisMenuState::INDICATOR_PRICE_LINES,
                PriceAxisMenuAction::ToggleIndicatorPriceLines,
            ),
            // AUTO_SCALE is deliberately not replayed: the manual price range it implies is
            // not part of this state, so disabling it on a new chart leaves no range to draw.
            (
                PriceAxisMenuState::INVERT_SCALE,
                PriceAxisMenuAction::ToggleInvertScale,
            ),
            (
                PriceAxisMenuState::BID_ASK,
                PriceAxisMenuAction::ToggleBidAsk,
            ),
            (
                PriceAxisMenuState::ALIGN_LABELS,
                PriceAxisMenuAction::ToggleAlignLabels,
            ),
        ];
        for (flag, action) in flags {
            if current_state.enabled(flag) != desired.enabled(flag) {
                let _ = self.apply_price_axis_menu_action(0, desired.left, action);
            }
        }
        if current_state.mode != desired.mode {
            let _ = self.apply_price_axis_menu_action(
                0,
                desired.left,
                PriceAxisMenuAction::SetMode(desired.mode),
            );
        }
        if current_state.precision != desired.precision {
            let _ = self.apply_price_axis_menu_action(
                0,
                desired.left,
                PriceAxisMenuAction::SetPrecision(desired.precision),
            );
        }
        true
    }

    /// Selects a Aeris Charts-owned theme without changing chart data or viewport.
    ///
    /// # Panics
    ///
    /// Panics when the platform font options derived from `platform.css` stop
    /// parsing.
    pub fn set_theme(&mut self, theme: AerisTheme) {
        let time_labels = TimeAxisLabels::of(&self.engine);
        if (self.platform.preset, self.platform.mode) != (theme.preset, theme.mode) {
            // Re-registering issues fresh raster keys, so it happens only on a real change.
            register_drawing_stamps(&mut self.engine, &theme);
        }
        self.platform = theme;
        self.palette = PresetPalette::for_platform(&theme);
        self.engine.set_theme(chart_theme(theme.mode));
        // Preset colors and a hollow frame are pinned to the theme, so they follow a switch.
        self.sync_painted_appearance();
        apply_platform_chrome_contract(&mut self.engine, time_labels);
        self.sync_big_trades_options();
        self.invalidate_series_layout();
    }

    /// Returns the time scale to the newest bar without changing its zoom.
    pub fn scroll_to_latest(&mut self) {
        self.engine.scroll_to_latest();
        self.invalidate_series_layout();
    }

    /// Returns the settled visible time range in Unix nanoseconds.
    #[must_use]
    pub fn visible_time_range_unix_nanos(&self) -> Option<(i64, i64)> {
        let (clamped_start, clamped_end) = self.engine.visible_time_range()?;
        let (logical_start, logical_end) = self.engine.visible_logical_range()?;
        let (times, _) = self.engine.data_layer().series_data(0)?;
        let first = times.first()?.to_f64()?;
        let last = times.last()?.to_f64()?;
        let first_index = self.engine.time_to_index(first, false)?.to_f64()?;
        let last_index = self.engine.time_to_index(last, false)?.to_f64()?;
        let logical_span = last_index - first_index;
        let time_span = last - first;
        let seconds_per_logical =
            (logical_span > 0.0 && time_span > 0.0).then_some(time_span / logical_span);
        let start = seconds_per_logical
            .filter(|_| logical_start < first_index)
            .map_or(clamped_start, |step| {
                first + (logical_start - first_index) * step
            });
        let end = seconds_per_logical
            .filter(|_| logical_end > last_index)
            .map_or(clamped_end, |step| last + (logical_end - last_index) * step);
        let start = (start * 1_000_000_000.0).round().to_i64()?;
        let end = (end * 1_000_000_000.0).round().to_i64()?;
        (start < end).then_some((start, end))
    }

    /// Restores a persisted visible time range without replacing chart data.
    pub fn set_visible_time_range_unix_nanos(&mut self, start: i64, end: i64) -> bool {
        if start >= end {
            return false;
        }
        let start_seconds = start.to_f64().unwrap_or(0.0) / 1_000_000_000.0;
        let end_seconds = end.to_f64().unwrap_or(0.0) / 1_000_000_000.0;
        if self.engine.has_future_time_projection()
            && let (Some(logical_start), Some(logical_end)) = (
                self.product_bars.logical_at_time(start_seconds),
                self.product_bars.logical_at_time(end_seconds),
            )
            && logical_start < logical_end
        {
            self.engine
                .set_visible_logical_range(logical_start, logical_end);
            self.invalidate_series_layout();
            return true;
        }
        self.engine
            .set_visible_time_range(start_seconds, end_seconds);
        self.invalidate_series_layout();
        self.fitted = true;
        true
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

    /// Removes the selected drawing or native Aeris Charts indicator.
    ///
    /// The price series is product-owned and cannot be deleted.
    pub fn remove_selected_chart_object(&mut self) -> bool {
        if self.remove_selected_drawing() {
            self.cancel_drawing();
            return true;
        }
        let Some(series) = self.engine.selected_series() else {
            return false;
        };
        let removed = self.remove_series_selection(series);
        if removed {
            self.engine.set_selected_series(None);
        }
        removed
    }

    /// Removes one selected product series. Studies are runtime-owned and become a host request;
    /// the price series, the footprint presentation and the volume weights are protected.
    fn remove_series_selection(&mut self, series: u32) -> bool {
        if series == 0 || series == self.volume_series || self.footprint_series_id() == Some(series)
        {
            return false;
        }
        if let Some(study_id) = self.engine.external_study_for_series(series) {
            self.pending_study_remove = Some(study_id);
            return true;
        }
        if let Some(study) = self.order_flow_study_for_series(series) {
            return self.remove_order_flow_study(study);
        }
        if self.is_volume_output(series) {
            self.remove_volume();
        } else if !self.engine.remove_indicator_for_series(series)
            && !self.engine.remove_series(series)
        {
            return false;
        }
        self.invalidate_series_layout();
        self.mark_user_state_changed();
        true
    }

    /// Whether the chart holds keyboard focus, so keys it leaves unconsumed may act on it.
    #[must_use]
    pub fn has_keyboard_focus(&self, window: &Window) -> bool {
        self.focus_handle
            .as_ref()
            .is_some_and(|handle| handle.is_focused(window))
    }

    /// Gives the chart keyboard focus, as a click on it would.
    pub fn focus_keyboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.focus_handle.get_or_insert_with(|| cx.focus_handle());
        window.focus(handle, cx);
    }

    /// Returns whether the newest bar is at the platform's real-time presentation edge, including
    /// its intentional right-side future-time margin.
    #[must_use]
    pub fn is_at_latest(&self) -> bool {
        self.engine.is_at_latest()
    }

    /// Returns whether a provider snapshot has populated the chart surface.
    #[must_use]
    pub const fn has_market_data(&self) -> bool {
        self.data_bridge.is_some()
    }

    /// Width of the right price axis as last laid out, so host overlays can stay clear of it.
    #[must_use]
    pub fn price_axis_width(&self) -> f32 {
        self.engine.axis_w.to_f32().unwrap_or_default()
    }

    /// Marks a load the trader is waiting on, so the symbol legend can carry it.
    ///
    /// Returns whether the surface changed, so a caller polling the host's
    /// lifecycle only repaints on a transition.
    pub const fn set_asset_loading(&mut self, loading: bool) -> bool {
        let changed = self.asset_loading.is_present() != loading;
        self.asset_loading = if loading {
            LegendPresence::Present
        } else {
            LegendPresence::Absent
        };
        changed
    }

    /// Enqueues one replay update for the next chart frame.
    ///
    /// Only a drawn frame drains the bridge, so a chart that is not on screen (a pane
    /// behind a maximized one, or behind another page) fills it. A full bridge
    /// is applied to the engine here before the update joins it, which keeps an undrawn
    /// chart current instead of discarding its updates for a full resnapshot.
    ///
    /// # Errors
    ///
    /// Returns the unchanged update when no market series is installed.
    pub fn try_queue_replay_update(
        &mut self,
        update: ReplayStreamUpdate,
    ) -> Result<(), Box<ReplayStreamUpdate>> {
        if self
            .data_bridge
            .as_ref()
            .is_some_and(ChartDataBridge::is_full)
        {
            let _ = self.apply_pending_data();
        }
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
        apply_replay_contract(&mut self.engine, replay);
        self.apply_price_series_kind();
        self.displayed_provenance.replace_snapshot(replay);
        self.asset_legend_title =
            replay_legend_title(replay, self.provider_display_name.as_deref());
        self.asset_symbol.clone_from(&replay.instrument().symbol);
        self.price_divisor = replay_price_divisor(replay);
        self.quantity_divisor = replay_quantity_divisor(replay);
        self.instrument_price_precision = replay_display_precision(replay);
        self.instrument_price_increment = replay.instrument().price_increment;
        self.instrument_price_scale = replay.instrument().precision.price_scale();
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

    /// Returns canonical evidence for the latest value installed into Aeris Charts.
    #[must_use]
    pub fn latest_market_provenance(&self) -> Option<&MarketEventProvenance> {
        self.displayed_provenance.latest()
    }

    /// Applies every replay command the bridge is holding.
    ///
    /// The frame does this before painting. A host that drives the chart without
    /// a frame, such as a headless test, calls it to read the queued updates back.
    pub fn apply_queued_replay_updates(&mut self) {
        let _ = self.apply_pending_data();
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
                    self.asset_legend_title =
                        replay_legend_title(snapshot, self.provider_display_name.as_deref());
                    self.asset_symbol.clone_from(&snapshot.instrument().symbol);
                    self.instrument_price_precision = replay_display_precision(snapshot);
                    self.instrument_price_increment = snapshot.instrument().price_increment;
                    self.instrument_price_scale = snapshot.instrument().precision.price_scale();
                }
                let previous_precision = self.instrument_price_precision;
                if self.instrument_price_increment.is_none() {
                    self.instrument_price_precision =
                        self.instrument_price_precision.max(price_display_precision(
                            update.accepted_deltas().iter().flat_map(|item| {
                                let bar = item.value();
                                [bar.open, bar.high, bar.low, bar.close]
                            }),
                            self.instrument_price_scale,
                        ));
                }
                self.displayed_provenance.extend(update.accepted_deltas());
                apply_merged_chart_data(
                    &mut self.engine,
                    self.volume_series,
                    &mut self.price_divisor,
                    &mut self.quantity_divisor,
                    self.chart_type,
                    &mut self.product_bars,
                    &update,
                );
                if let Some(snapshot) = update.snapshot() {
                    apply_replay_contract(&mut self.engine, snapshot);
                }
                if mutation == SeriesMutation::Snapshot {
                    self.sync_brushable_interaction();
                    self.apply_price_series_kind();
                }
                if mutation == SeriesMutation::Snapshot
                    || previous_precision != self.instrument_price_precision
                {
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
                diagnostic!("replay update rejected; snapshot required: {error}");
                SeriesMutation::None
            }
        }
    }

    fn invalidate_series_frame(&mut self) {
        self.frame = ChartFrame::default();
        self.axis_prims.clear();
    }

    /// Wakes the surface once a second so the countdown to the next bar moves on
    /// the clock instead of on market activity.
    ///
    /// Every other repaint here happens because a message arrived. On a quiet
    /// market the countdown simply held its last value and then jumped by
    /// however many seconds had passed when the next trade landed, which reads
    /// as a frozen feed on a connection that is perfectly healthy. The wake is
    /// aligned to the second boundary being counted down to, and the engine only
    /// rebuilds when that second actually changes.
    fn schedule_clock_tick(&mut self, cx: &mut Context<Self>) {
        if self.clock_tick.is_some() {
            return;
        }
        let delay = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|since_epoch| {
                CHART_CLOCK_INTERVAL
                    .checked_sub(Duration::from_nanos(u64::from(since_epoch.subsec_nanos())))
            })
            .unwrap_or(CHART_CLOCK_INTERVAL);
        self.clock_tick = Some(cx.spawn(async move |chart, cx| {
            cx.background_executor().timer(delay).await;
            let _ = chart.update(cx, |chart, chart_cx| {
                chart.clock_tick = None;
                chart.clock_revision = chart.clock_revision.wrapping_add(1);
                chart_cx.notify();
            });
        }));
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
        let Some((pane, target)) = self.engine.series_price_scale(0) else {
            return;
        };
        let applied =
            self.engine
                .set_price_format_for_scale(pane, target, u32::from(precision), min_move);
        debug_assert!(applied);
    }

    fn primary_series_id_on_scale(&self, pane: usize, target: PriceScaleTarget) -> Option<u32> {
        self.engine
            .primary_series_on_price_scale(pane, target)
            .map(|series| series.series_id)
    }

    fn product_price_line_visible(&self) -> bool {
        [PriceScaleTarget::Right, PriceScaleTarget::Left]
            .into_iter()
            .filter_map(|target| self.engine.primary_series_on_price_scale(0, target))
            .any(|series| series.series_id == 0 && series.price_line_visible)
    }

    /// Host-owned price-series chart type forwarded to Aeris Charts.
    #[must_use]
    pub const fn chart_type(&self) -> ChartType {
        self.chart_type
    }

    /// Returns the current durable chart appearance without exposing Aeris Charts
    /// series/options internals to the desktop shell.
    #[must_use]
    pub fn appearance_settings(&self) -> ChartAppearanceSettings {
        self.appearance_intent
            .clone()
            .unwrap_or_else(|| self.engine.financial_appearance(0).unwrap_or_default())
    }

    /// The colors theme-following appearance and axis fields paint under the chart's
    /// platform theme.
    #[must_use]
    pub fn theme_defaults(&self) -> ChartThemeDefaults {
        theme_defaults(self.palette.as_ref(), self.engine_theme())
    }

    /// The durable axis colors.
    #[must_use]
    pub fn axis_appearance(&self) -> ChartAxisAppearance {
        self.axis_appearance.clone()
    }

    /// Applies durable axis colors without touching any other appearance.
    pub fn set_axis_appearance(&mut self, axis: &ChartAxisAppearance) -> bool {
        if self.axis_appearance == *axis {
            return false;
        }
        self.axis_appearance = axis.clone();
        self.apply_platform_chart_colors();
        self.invalidate_series_frame();
        self.mark_user_state_changed();
        true
    }

    /// The colors the primary series paints for its line, area and baseline controls, pinned
    /// or default, as `#RRGGBB` or `#RRGGBBAA` for the color picker.
    #[must_use]
    pub fn series_colors(&self) -> Option<ChartSeriesColors> {
        let colors = self.engine.financial_series_colors(0)?;
        let hex = |css: String| drawing_color_hex(&css).unwrap_or(css);
        Some(ChartSeriesColors {
            line: hex(colors.line),
            area_top: hex(colors.area_top),
            area_bottom: hex(colors.area_bottom),
            baseline_top: hex(colors.baseline_top),
            baseline_bottom: hex(colors.baseline_bottom),
        })
    }

    /// Applies host-authored series/canvas presentation in place. Market data,
    /// viewport state and provider ownership are untouched.
    pub fn set_appearance_settings(&mut self, appearance: &ChartAppearanceSettings) -> bool {
        let canvas_changed = self.apply_canvas_appearance_settings(appearance);
        let series_changed = self.apply_series_appearance_settings(appearance);
        if !canvas_changed && !series_changed {
            return false;
        }
        self.mark_user_state_changed();
        true
    }

    /// Applies only chart-canvas presentation. Primary-series options are left
    /// byte-for-byte untouched so a grid/crosshair edit cannot perturb price
    /// series styling such as area fills, candle colors, or line appearance.
    pub fn set_canvas_appearance_settings(&mut self, appearance: &ChartAppearanceSettings) -> bool {
        if self.apply_canvas_appearance_settings(appearance) {
            self.mark_user_state_changed();
            true
        } else {
            false
        }
    }

    /// Applies only primary-series presentation. Canvas options are left
    /// untouched so series edits cannot rewrite grid or crosshair styling.
    pub fn set_series_appearance_settings(&mut self, appearance: &ChartAppearanceSettings) -> bool {
        if self.apply_series_appearance_settings(appearance) {
            self.mark_user_state_changed();
            true
        } else {
            false
        }
    }

    fn apply_canvas_appearance_settings(&mut self, appearance: &ChartAppearanceSettings) -> bool {
        let before = self.appearance_settings();
        if let Some(intent) = self.appearance_intent.take() {
            let intent = with_canvas_of(intent, appearance);
            self.engine
                .apply_financial_canvas_appearance(&self.painted_appearance(&intent));
            self.appearance_intent = Some(intent);
        } else {
            self.engine.apply_financial_canvas_appearance(appearance);
        }
        // A canvas edit rewrites the price-axis text, so the axis colors follow it.
        self.apply_platform_chart_colors();
        let changed = before != self.appearance_settings();
        if changed {
            self.invalidate_series_frame();
        }
        changed
    }

    fn apply_series_appearance_settings(&mut self, appearance: &ChartAppearanceSettings) -> bool {
        let before = self.appearance_settings();
        if let Some(intent) = self.appearance_intent.take() {
            let intent = with_canvas_of(appearance.clone(), &intent);
            self.engine
                .apply_financial_series_appearance(0, &self.painted_appearance(&intent));
            self.appearance_intent = Some(intent);
        } else {
            self.engine.apply_financial_series_appearance(0, appearance);
        }
        let changed = before != self.appearance_settings();
        if changed {
            self.invalidate_series_frame();
        }
        changed
    }

    const fn engine_theme(&self) -> ChartTheme {
        chart_theme(self.platform.mode)
    }

    /// What Aeris Charts paints for the user's `intent`: theme-following colors pinned to
    /// the preset palette, then transparent bodies for a hollow chart type.
    fn painted_appearance(&self, intent: &ChartAppearanceSettings) -> ChartAppearanceSettings {
        let pinned = self
            .palette
            .as_ref()
            .map_or_else(|| intent.clone(), |palette| palette.pin(intent));
        match self.chart_type.hollow_sides() {
            Some((bullish, bearish)) => {
                hollow_appearance(&pinned, bullish, bearish, self.engine_theme())
            }
            None => pinned,
        }
    }

    /// Paints the derived appearance for the current palette and chart type, or restores the
    /// user's own appearance once nothing is derived. The engine's colors are replaced only
    /// while something is derived or when leaving that state.
    fn sync_painted_appearance(&mut self) {
        let derived = self.palette.is_some() || self.chart_type.hollow_sides().is_some();
        match (self.appearance_intent.take(), derived) {
            (None, false) => {}
            (Some(intent), false) => {
                self.engine.apply_financial_canvas_appearance(&intent);
                self.engine.apply_financial_series_appearance(0, &intent);
            }
            (intent, true) => {
                let intent = intent.unwrap_or_else(|| self.appearance_settings());
                let painted = self.painted_appearance(&intent);
                self.engine.apply_financial_canvas_appearance(&painted);
                self.engine.apply_financial_series_appearance(0, &painted);
                self.appearance_intent = Some(intent);
            }
        }
        self.apply_platform_chart_colors();
    }

    /// Paints the platform-owned chart colors the typed appearance does not carry: a preset's
    /// layout colors and the axis colors. Aeris Charts resets them on every theme switch and
    /// canvas edit, so each of those repaints them.
    fn apply_platform_chart_colors(&mut self) {
        let defaults = self.theme_defaults();
        let text = effective_color(&self.appearance_settings().text_color, &defaults.text);
        let border = effective_color(&self.axis_appearance.border_color, &defaults.axis_border);
        let axis_text = effective_color(&self.axis_appearance.text_color, &text);
        let price_scale = serde_json::json!({ "borderColor": border, "textColor": axis_text });
        let mut patch = serde_json::json!({
            "leftPriceScale": price_scale,
            "rightPriceScale": price_scale,
            "timeScale": { "borderColor": border },
        });
        if let Some(palette) = &self.palette {
            patch["layout"] = palette.layout_patch();
        }
        if let Err(error) = self.engine.apply_options(&patch.to_string()) {
            diagnostic!("Aeris Charts rejected the platform chart colors: {error}");
        }
    }

    /// Restores Aeris Charts-owned styling through the chart engine's canonical reset API.
    /// Market data, viewport state, drawings, indicators, and other semantic state
    /// remain owned and preserved by Aeris Charts.
    pub fn reset_appearance_settings(&mut self) {
        let volume_visible = self.volume_visible();
        self.engine.reset_style_to_defaults();
        self.appearance_intent = None;
        self.axis_appearance = ChartAxisAppearance::default();
        self.sync_painted_appearance();
        apply_product_series_markers(&mut self.engine, self.chart_type);
        self.reapply_study_styles();
        // Volume colors are indicator settings, not chart appearance.
        self.apply_volume_style(volume_visible);
        self.invalidate_series_layout();
        self.mark_user_state_changed();
    }

    /// Applies a built-in Aeris Charts price-series kind without changing market data.
    pub fn set_chart_type(&mut self, chart_type: ChartType) {
        if self.chart_type == chart_type {
            return;
        }
        self.chart_type = chart_type;
        self.reconfigure_order_flow();
        self.apply_price_series_kind();
        // An unfitted chart opens the footprint zoom on its first layout instead.
        if chart_type == ChartType::Footprint && self.fitted {
            self.engine.fit_footprint_viewport();
        }
        self.mark_user_state_changed();
    }

    /// Returns the stable configured crosshair mode, excluding pointer position
    /// and temporary modifier-driven OHLC magnet state.
    #[must_use]
    pub fn crosshair_mode(&self) -> u8 {
        self.engine.configured_crosshair_mode()
    }

    /// Returns the engine-resolved height of the visible time-axis strip.
    pub fn time_axis_height(&self) -> f32 {
        self.engine.time_axis_height().to_f32().unwrap_or(0.0)
    }

    /// Drains host-facing alert-create requests produced by Aeris Charts'
    /// crosshair action chip.
    pub fn take_alert_create_requests(&mut self) -> Vec<AlertCreateRequest> {
        self.engine.take_alert_create_requests()
    }

    /// Replaces the chart trading layer from host-authoritative runtime state.
    ///
    /// # Errors
    /// Returns the chart engine validation error when the snapshot is malformed or over capacity.
    pub fn set_trading_snapshot(&mut self, mut snapshot: TradingSnapshot) -> Result<(), String> {
        // Aeris Charts snaps drawings to the instrument tick and otherwise falls back to the
        // display precision, so a projection without an account still names the chart's tick.
        if snapshot.instrument.tick_size.is_none() {
            snapshot.instrument.tick_size = Some(self.price_tick_size());
        }
        if self.engine.trading_snapshot() == snapshot {
            return Ok(());
        }
        self.engine
            .set_trading_snapshot(snapshot)
            .map_err(|error| error.to_string())?;
        self.invalidate_series_frame();
        Ok(())
    }

    /// Applies the host's order-line extension preference without changing trading state.
    ///
    /// # Errors
    /// Returns a chart engine style validation error.
    pub fn set_order_lines_extend_left(&mut self, extend: bool) -> Result<bool, String> {
        if self.engine.trading_style().extend_lines_left == extend {
            return Ok(false);
        }
        self.engine
            .apply_trading_style(aeris_charts_engine::TradingStyleOptions {
                extend_lines_left: Some(extend),
                ..Default::default()
            })
            .map_err(|error| error.to_string())?;
        self.invalidate_series_frame();
        Ok(true)
    }

    #[must_use]
    pub fn order_lines_extend_left(&self) -> bool {
        self.engine.trading_style().extend_lines_left
    }

    /// Replaces transient, host-owned event markers and time windows.
    ///
    /// # Errors
    /// Returns the engine validation error when an overlay is malformed or exceeds its caps.
    pub fn set_host_overlay(
        &mut self,
        snapshot: aeris_charts_engine::HostOverlaySnapshot,
    ) -> Result<(), String> {
        self.engine
            .set_host_overlay(snapshot)
            .map_err(|error| error.to_string())?;
        self.invalidate_series_frame();
        Ok(())
    }

    /// Returns the current transient host overlay projection.
    #[must_use]
    pub fn host_overlay(&self) -> aeris_charts_engine::HostOverlaySnapshot {
        self.engine.host_overlay().clone()
    }

    /// Replaces transient host-owned session-plan price levels.
    ///
    /// Plan levels do not enter Aeris Charts drawing persistence and are restored
    /// automatically whenever the host replaces the primary price series.
    ///
    /// # Errors
    /// Returns an error for invalid or over-capacity levels, or when a
    /// non-empty projection has no primary right-hand price series.
    pub fn replace_session_plan_levels(
        &mut self,
        levels: Vec<(f64, String)>,
    ) -> Result<(), String> {
        if levels.len() > MAXIMUM_SESSION_PLAN_LEVELS {
            return Err("session plan level projection exceeds its capacity".to_string());
        }
        if levels.iter().any(|(price, label)| {
            !price.is_finite() || *price <= 0.0 || label.trim().is_empty() || label.len() > 64
        }) {
            return Err("session plan level projection is invalid".to_string());
        }
        if levels == self.session_plan_levels {
            return Ok(());
        }
        if !levels.is_empty()
            && self
                .primary_series_id_on_scale(0, PriceScaleTarget::Right)
                .is_none()
        {
            return Err("session plan levels require a primary price series".to_string());
        }
        self.remove_session_plan_price_lines();
        self.session_plan_levels = levels;
        self.install_session_plan_price_lines();
        self.invalidate_series_frame();
        Ok(())
    }

    /// Returns the current transient session-plan price-level projection.
    #[must_use]
    pub fn session_plan_levels(&self) -> &[(f64, String)] {
        &self.session_plan_levels
    }

    /// Drains chart trading intents for the host's single command path.
    pub fn take_trading_intents(&mut self) -> Vec<TradingIntent> {
        self.engine.take_trading_intents()
    }

    /// Returns the chart's current host-projected trading snapshot.
    #[must_use]
    pub fn trading_snapshot(&self) -> TradingSnapshot {
        self.engine.trading_snapshot()
    }

    /// Resolves one host command acknowledgement or rejection.
    pub fn resolve_trading_intent(&mut self, sequence: u32, accepted: bool) -> bool {
        self.engine.resolve_trading_intent(sequence, accepted)
    }

    /// Replaces the chart-local alert indicators from host-authoritative state.
    ///
    /// # Errors
    /// Returns an error when Aeris Charts rejects invalid or over-capacity lines.
    pub fn replace_price_alert_lines(&mut self, snapshot: AlertSnapshot) -> Result<(), String> {
        self.engine
            .set_alert_snapshot(snapshot)
            .map_err(|error| error.to_string())?;
        self.invalidate_series_frame();
        Ok(())
    }

    /// Reports whether the instrument's market session is trading. A closed session hides the
    /// candle-close countdown; the user's countdown preference is unchanged.
    pub fn set_market_trading(&mut self, trading: bool) {
        self.engine.set_bar_countdown_active(trading);
        self.invalidate_series_frame();
    }

    /// Applies one stable Aeris Charts crosshair mode.
    pub fn set_crosshair_mode(&mut self, mode: u8) -> bool {
        if !self.engine.set_configured_crosshair_mode(mode) {
            return false;
        }
        self.mark_user_state_changed();
        true
    }

    /// Which parts of the legend show.
    #[must_use]
    pub const fn legend_options(&self) -> ChartLegendOptions {
        self.legend_options
    }

    /// Shows or hides parts of the legend. Returns whether anything changed.
    pub fn set_legend_options(&mut self, options: ChartLegendOptions) -> bool {
        if self.legend_options == options {
            return false;
        }
        self.legend_options = options;
        self.mark_user_state_changed();
        true
    }

    /// Selected IANA display time zone. Canonical chart timestamps remain UTC.
    #[must_use]
    pub fn time_zone_id(&self) -> &'static str {
        self.engine.time_zone_id()
    }

    /// TradingView-parity time zones exposed by Aeris Charts.
    #[must_use]
    pub const fn supported_time_zones() -> &'static [&'static str] {
        aeris_charts_engine::TRADINGVIEW_TIME_ZONES
    }

    /// Current DST-aware UTC-offset badge for a supported IANA identifier.
    ///
    /// The offset is resolved for the supplied UTC instant rather than being stored as a static
    /// property, so zones that observe daylight saving time remain accurate throughout the year.
    #[must_use]
    pub fn time_zone_badge_label(time_zone: &str, utc_seconds: i64) -> Option<String> {
        let zone = aeris_charts_engine::ChartTimeZone::parse(time_zone)?;
        let parts = zone.local_parts(utc_seconds)?;
        Some(format_utc_offset(parts.offset_seconds))
    }

    /// `HH:MM` wall time of a UTC instant in a supported IANA zone, for host surfaces such as
    /// the header market-hours tooltip that must read in the chart's selected zone.
    #[must_use]
    pub fn time_zone_wall_time_label(time_zone: &str, utc_seconds: i64) -> Option<String> {
        let parts =
            aeris_charts_engine::ChartTimeZone::parse(time_zone)?.local_parts(utc_seconds)?;
        Some(format!("{:02}:{:02}", parts.hour, parts.minute))
    }

    /// Applies a selected IANA display time zone and records it as durable presentation state.
    ///
    /// # Errors
    /// Returns an error when Aeris Charts rejects the supplied time-zone identifier.
    pub fn set_time_zone(&mut self, time_zone: &str) -> Result<bool, String> {
        let changed = self.engine.set_time_zone(time_zone)?;
        if changed {
            self.invalidate_series_layout();
            self.mark_user_state_changed();
        }
        Ok(changed)
    }

    /// Clock text for the selected zone using the same TradingView-style UTC-offset notation as
    /// the selector menu. Kept instant-addressable so the selected-state presentation is testable
    /// across DST boundaries.
    #[must_use]
    pub fn time_zone_clock_label_at(&self, utc_seconds: i64) -> String {
        Self::time_zone_clock_label_for(self.engine.time_zone_id(), utc_seconds)
    }

    /// Clock text for any supported zone, so host chrome can show the selected zone's time
    /// before a chart exists.
    #[must_use]
    pub fn time_zone_clock_label_for(time_zone: &str, utc_seconds: i64) -> String {
        let Some(zone) = aeris_charts_engine::ChartTimeZone::parse(time_zone) else {
            return "--:--:--".to_string();
        };
        let Some(parts) = zone.local_parts(utc_seconds) else {
            return "--:--:--".to_string();
        };
        let badge = Self::time_zone_badge_label(time_zone, utc_seconds)
            .unwrap_or_else(|| "UTC".to_string());
        format!(
            "{:02}:{:02}:{:02} {badge}",
            parts.hour, parts.minute, parts.second
        )
    }

    /// Calendar date and wall time of a UTC instant in the selected chart time zone, for host
    /// surfaces such as trade history that must read in the same zone as the time axis.
    #[must_use]
    pub fn time_zone_date_time_label_at(&self, utc_seconds: i64) -> String {
        let parts = aeris_charts_engine::ChartTimeZone::parse(self.engine.time_zone_id())
            .and_then(|zone| zone.local_parts(utc_seconds));
        parts.map_or_else(
            || "----------".to_string(),
            |parts| {
                format!(
                    "{} {:02}:{:02}:{:02}",
                    platform_date_label(parts.year, parts.month, parts.day),
                    parts.hour,
                    parts.minute,
                    parts.second
                )
            },
        )
    }

    /// Revision of the one-second presentation clock, separate from durable user state.
    #[must_use]
    pub const fn clock_revision(&self) -> u64 {
        self.clock_revision
    }

    /// Monotonic revision of durable user-authored presentation state, including every committed
    /// Aeris Charts drawing edit.
    #[must_use]
    pub fn user_state_revision(&self) -> u64 {
        self.user_state_revision
            .wrapping_add(self.drawing_revision())
    }

    /// Monotonic revision of committed Aeris Charts drawings alone: creates, edits, locks,
    /// clears, imports, undo and redo.
    #[must_use]
    pub fn drawing_revision(&self) -> u64 {
        self.engine.drawing_revision()
    }

    fn mark_user_state_changed(&mut self) {
        self.user_state_revision = self.user_state_revision.saturating_add(1);
    }

    /// Every committed drawing as an Aeris Charts drawings document, whose anchors carry the
    /// exchange time they denote so another interval or history window restores them in place.
    ///
    /// # Errors
    ///
    /// Returns an error if Aeris Charts cannot serialize its bounded drawing state.
    pub fn export_drawings_json(&self) -> Result<String, String> {
        self.engine
            .export_drawings_json()
            .map_err(|error| error.to_string())
    }

    /// Replaces every drawing with an Aeris Charts drawings document, after indicator panes
    /// have been recreated. Aeris Charts treats this as a restore, not an undoable edit.
    ///
    /// # Errors
    ///
    /// Returns an error, leaving the drawings untouched, when Aeris Charts rejects the document.
    pub fn import_drawings_json(&mut self, json: &str) -> Result<(), String> {
        self.engine
            .import_drawings_json(json)
            .map_err(|error| error.to_string())?;
        self.invalidate_series_layout();
        Ok(())
    }

    fn apply_price_series_kind(&mut self) {
        self.remove_session_plan_price_lines();
        install_product_price_series(&mut self.engine, self.chart_type, &self.product_bars);
        self.sync_painted_appearance();
        self.install_session_plan_price_lines();
        self.sync_brushable_interaction();
        self.invalidate_series_layout();
    }

    fn remove_session_plan_price_lines(&mut self) {
        for id in self.session_plan_price_line_ids.drain(..) {
            self.engine.remove_price_line(id);
        }
    }

    fn install_session_plan_price_lines(&mut self) {
        let Some(series_id) = self.primary_series_id_on_scale(0, PriceScaleTarget::Right) else {
            return;
        };
        for (price, label) in &self.session_plan_levels {
            let title = format!("PLAN · {label}");
            let id = self.engine.create_price_line(
                series_id,
                *price,
                SESSION_PLAN_LEVEL_COLOR,
                1,
                LineStyle::Dashed,
                &title,
            );
            self.session_plan_price_line_ids.push(id);
        }
    }

    /// Brushable Area is the Aeris Charts brushable-area composition over the product Area series.
    /// Shift+drag compares a range so a plain drag still pans the chart.
    fn sync_brushable_interaction(&mut self) {
        let wanted = self.chart_type == ChartType::BrushableArea;
        if wanted == self.engine.is_brushable_area(0) {
            return;
        }
        let options = wanted.then_some(DeltaTooltipOptions {
            requires_shift_drag: true,
            ..DeltaTooltipOptions::default()
        });
        let applied = self.engine.set_brushable_area(0, options);
        debug_assert!(
            applied,
            "the product Area series accepts the brushable area"
        );
    }

    fn move_price_axis(&mut self, pane: usize, from_left: bool, to_left: bool) -> bool {
        if from_left == to_left {
            return true;
        }
        let from = price_axis_target(from_left);
        let to = price_axis_target(to_left);
        self.engine.rebind_price_scale_series(pane, from, to)
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

    fn prepare_frame(
        &mut self,
        (width, height, scale_factor): (f32, f32, f32),
        force_layout: bool,
        fit_content: bool,
        measure: impl Fn(&str, bool) -> f64 + Copy,
        countdown_measure: impl Fn(&str, bool) -> f64 + Copy,
    ) -> aeris_charts_engine::FinancialFramePreparation {
        self.engine.prepare_financial_frame_with_measure(
            aeris_charts_engine::FinancialFrameRequest {
                width: f64::from(width),
                height: f64::from(height),
                dpr: f64::from(scale_factor),
                force_layout,
                allow_axis_shrink: force_layout,
                force_frame: false,
                force_axis: false,
                layout_only: false,
                fit_content,
                frame: &mut self.frame,
                axis_frame: None,
                axis_primitives: Some(&mut self.axis_prims),
            },
            measure,
            countdown_measure,
        )
    }

    /// Opens a freshly installed market: a footprint at its cluster zoom on the live edge,
    /// any other chart type on its latest bars.
    fn open_initial_viewport(&mut self) -> bool {
        if self.chart_type == ChartType::Footprint {
            self.engine.fit_footprint_viewport();
            return true;
        }
        self.narrow_to_initial_window()
    }

    /// Opens a freshly installed market on its latest bars instead of fitting everything
    /// loaded. Older history is indicator warm-up and back-scroll runway, so long studies
    /// are already converged at the visible left edge.
    fn narrow_to_initial_window(&mut self) -> bool {
        let Some((left, right)) = self.engine.visible_logical_range() else {
            return false;
        };
        if right - left <= INITIAL_VISIBLE_BARS {
            return false;
        }
        self.engine
            .set_visible_logical_range(right - INITIAL_VISIBLE_BARS, right);
        true
    }

    fn invalidate_series_layout(&mut self) {
        self.invalidate_series_frame();
        self.layout_dirty = true;
    }

    fn rebuild(
        &mut self,
        width: f32,
        height: f32,
        scale_factor: f32,
        mutation: SeriesMutation,
        window: &Window,
    ) -> bool {
        #[cfg(not(feature = "diagnostics"))]
        let _ = mutation;
        install_text_metrics(&mut self.engine, window);
        self.input.prepare_frame(&mut self.engine);
        #[cfg(feature = "diagnostics")]
        let rebuild_started = Instant::now();
        self.pin_host_clock();
        let dimensions = (width, height, scale_factor);
        let dimensions_changed = self.built_for != dimensions;
        if dimensions_changed {
            self.built_for = dimensions;
        }

        let layout = self.engine.options.get().layout.clone();
        let typography = platform_typography();
        let font_size = layout.font_size.to_f32().unwrap_or(12.0);
        let measure = |text: &str, bold: bool| {
            let weight = if bold {
                typography.weight(TypographyRole::Emphasis)
            } else {
                typography.weight(TypographyRole::Normal)
            };
            f64::from(
                measure_text(window, text, &layout.font_family, font_size, weight, false).width,
            )
        };
        let countdown_font_size = self.engine.countdown_font_size().to_f32().unwrap_or(10.0);
        let countdown_measure = |text: &str, bold: bool| {
            let weight = if bold {
                typography.weight(TypographyRole::Emphasis)
            } else {
                typography.weight(TypographyRole::Normal)
            };
            f64::from(
                measure_text(
                    window,
                    text,
                    &layout.font_family,
                    countdown_font_size,
                    weight,
                    false,
                )
                .width,
            )
        };

        self.sync_pane_heights();
        let fit_content = !self.fitted;
        let force_layout = self.layout_dirty;
        let mut preparation = self.prepare_frame(
            dimensions,
            force_layout,
            fit_content,
            measure,
            countdown_measure,
        );
        // The fit needs the laid-out width, so a freshly installed market that holds
        // more than its opening window is narrowed and laid out once more.
        if fit_content && preparation.layout_recomputed && self.open_initial_viewport() {
            preparation = self.prepare_frame(dimensions, true, false, measure, countdown_measure);
        }
        if !preparation.frame_built {
            return false;
        }
        if preparation.dpr_changed {
            self.renderer.invalidate_caches();
        }
        if preparation.layout_recomputed {
            self.layout_dirty = false;
            self.fitted = true;
        }
        #[cfg(feature = "diagnostics")]
        let layout_recomputed = preparation.layout_recomputed;
        let legend_layout_changed = self.sync_legend_pane_layout();
        #[cfg(feature = "diagnostics")]
        {
            let elapsed = rebuild_started.elapsed();
            if self.live_evidence_enabled && self.live_evidence_rebuilds < 256 {
                self.live_evidence_rebuilds = self.live_evidence_rebuilds.saturating_add(1);
                diagnostic!(
                    "AERIS_CHART_REBUILD {{\"micros\":{},\"layout\":{},\"data\":\"{}\"}}",
                    elapsed.as_micros(),
                    layout_recomputed,
                    mutation.label()
                );
            }
            if elapsed.as_millis() >= 4 {
                diagnostic!(
                    "chart rebuild: {} ms (layout={})",
                    elapsed.as_millis(),
                    layout_recomputed
                );
            }
        }
        legend_layout_changed
    }

    fn paint(&mut self, bounds: Bounds<gpui::Pixels>, window: &mut Window, cx: &mut App) {
        #[cfg(feature = "diagnostics")]
        let paint_started = Instant::now();
        let viewport = AerisViewport::from_bounds(
            bounds.origin.x.into(),
            bounds.origin.y.into(),
            bounds.size.width.into(),
            bounds.size.height.into(),
        );
        let prepared = PreparedAerisFrame::from_engine(&self.frame, &self.engine)
            .with_axis(&self.axis_prims, &[]);
        if let Err(error) =
            self.renderer
                .paint_frame(&prepared, viewport, window.scale_factor(), window, cx)
        {
            diagnostic!("Aeris Charts frame skipped: {error}");
        }
        #[cfg(feature = "diagnostics")]
        {
            let elapsed = paint_started.elapsed();
            if elapsed.as_millis() >= 4 {
                diagnostic!("chart paint: {} ms", elapsed.as_millis());
            }
        }
    }
}

fn format_utc_offset(offset_seconds: i32) -> String {
    if offset_seconds == 0 {
        return "UTC".to_string();
    }
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let total_minutes = offset_seconds.unsigned_abs() / 60;
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    if minutes == 0 {
        format!("UTC{sign}{hours}")
    } else {
        format!("UTC{sign}{hours}:{minutes:02}")
    }
}

impl Default for AerisChartView {
    fn default() -> Self {
        Self::new()
    }
}

fn legend_palette(theme: &AerisTheme) -> LegendPalette {
    let colors = theme.colors;
    LegendPalette {
        text: gpui_theme_color(colors.text_default),
        muted: gpui_theme_color(colors.text_secondary),
        hover: gpui_theme_color(colors.hover_bg),
        danger: gpui_theme_color(colors.danger),
    }
}

fn chart_legend_layers(
    chart: &Entity<AerisChartView>,
    rows: &[LegendRow],
    panes: &[LegendPaneLayout],
    theme: &AerisTheme,
    loading: bool,
    accessory: Option<&AnyView>,
) -> Vec<AnyElement> {
    let palette = legend_palette(theme);
    panes
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(pane_index, pane)| {
            let pane_rows = rows.iter().filter(|row| row.pane == pane_index);
            let mut accessory = accessory.filter(|_| pane_index == 0).cloned();
            let follows_asset = rows
                .iter()
                .any(|row| row.pane == pane_index && row.item == LegendItem::Asset);
            // Cap the legend at the plot area so it wraps inside the pane instead of running
            // under the price axis.
            let available_width = (pane.width - LEGEND_INSET * 2.0).clamp(0.0, LEGEND_MAX_WIDTH);
            let mut layer = div()
                .id(("chart_legend_pane", pane_index))
                .absolute()
                .left(px(pane.left + LEGEND_INSET))
                .top(px(pane.top + LEGEND_INSET))
                .max_w(px(available_width))
                .max_h(px((pane.height - LEGEND_INSET * 2.0).max(0.0)))
                .overflow_hidden()
                .flex()
                .flex_col()
                .items_start()
                .cursor(CursorStyle::Arrow);
            let mut row_count = 0;
            if !follows_asset && let Some(accessory) = accessory.take() {
                row_count += 1;
                layer = layer.child(legend_accessory(chart, accessory));
            }
            for row in pane_rows {
                row_count += 1;
                layer = layer.child(chart_legend_row(chart, row, palette, loading));
                if row.item == LegendItem::Asset
                    && let Some(accessory) = accessory.take()
                {
                    layer = layer.child(legend_accessory(chart, accessory));
                }
            }
            (row_count > 0).then(|| layer.into_any_element())
        })
        .collect()
}

/// Host content under the symbol row. Presses stop here so a click on a host control never
/// starts a chart gesture or moves keyboard focus to the chart; like any press on the chart,
/// it still asks the host to activate the chart's pane.
fn legend_accessory(chart: &Entity<AerisChartView>, accessory: AnyView) -> AnyElement {
    let activate = chart.clone();
    div()
        .id("chart_legend_accessory")
        .flex_none()
        .px_1()
        .py_1()
        .on_any_mouse_down(move |_, _, cx| {
            activate.update(cx, |chart, chart_cx| {
                chart.pending_activate = ActivationRequest::Pending;
                chart_cx.notify();
            });
            cx.stop_propagation();
        })
        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(accessory)
        .into_any_element()
}

fn chart_legend_row(
    chart: &Entity<AerisChartView>,
    row: &LegendRow,
    palette: LegendPalette,
    loading: bool,
) -> AnyElement {
    let group: SharedString = format!("chart-legend-row-{}", row.item.key()).into();
    let controls = legend_row_controls(chart, row, palette, &group);
    let values = if row.visible {
        row.values.clone()
    } else {
        Vec::new()
    };
    let values = values.into_iter().map(|value| {
        let color = value
            .color
            .as_deref()
            .and_then(Color::parse_css)
            .map_or(palette.muted, |color| rgba(color.0));
        // A wrapping text element shapes at whatever width the row's flex-wrap pass offers it,
        // so a labelled value such as "C 6812.25" could break at its space and jump inside the
        // fixed-height cell as its digits change. Each value moves to a new row line as a whole.
        div()
            .flex_none()
            .h(px(LEGEND_ROW_HEIGHT))
            .flex()
            .items_center()
            .whitespace_nowrap()
            .font_features(platform_tabular_numerals())
            .text_color(color)
            .child(value.text)
            .into_any_element()
    });
    div()
        .id(("chart_legend_row", row.item.key()))
        .group(group.clone())
        .min_h(px(LEGEND_ROW_HEIGHT))
        .max_w_full()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x_2()
        .px_1()
        .rounded(px(f32::from(
            aeris_design_system::RadiusToken::Sm.logical_pixels(),
        )))
        .text_xs()
        .text_color(if row.visible {
            palette.text
        } else {
            palette.muted
        })
        .cursor(CursorStyle::Arrow)
        .hover(|style| style.bg(palette.hover))
        .children((loading && row.item == LegendItem::Asset).then(|| legend_loading_glyph(palette)))
        .child(
            div()
                .flex_none()
                .h(px(LEGEND_ROW_HEIGHT))
                .flex()
                .items_center()
                .whitespace_nowrap()
                // The asset title (symbol, venue and interval) leads the legend one step up the
                // type scale, at the surface's regular weight like every other legend row.
                .when(row.item == LegendItem::Asset, gpui::Styled::text_sm)
                .child(row.title.clone()),
        )
        .children(values)
        .child(controls)
        .into_any_element()
}

fn legend_row_controls(
    chart: &Entity<AerisChartView>,
    row: &LegendRow,
    palette: LegendPalette,
    group: &SharedString,
) -> impl IntoElement {
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
        .h(px(LEGEND_ROW_HEIGHT))
        .flex()
        .items_center()
        .gap_1()
        .child(visibility);
    if row.settings_available
        && matches!(
            row.item,
            LegendItem::Study { .. }
                | LegendItem::BigTrades
                | LegendItem::Volume
                | LegendItem::OrderFlow(_)
                | LegendItem::OrderBookDepth
        )
    {
        controls = controls.child(
            legend_control(chart, row.item, LegendControl::Settings, palette)
                .invisible()
                .group_hover(group.clone(), gpui::Styled::visible),
        );
    }
    if row.item != LegendItem::Asset {
        controls = controls.child(
            legend_control(chart, row.item, LegendControl::Remove, palette)
                .invisible()
                .group_hover(group.clone(), gpui::Styled::visible),
        );
    }
    controls
}

/// The symbol row's own load indicator, sized to the legend text and placed ahead
/// of the title: a load in flight belongs to the symbol the trader is reading, not
/// to a second notice sitting over the plot.
fn legend_loading_glyph(palette: LegendPalette) -> AnyElement {
    div()
        .id("chart_legend_loading")
        .flex_none()
        .h(px(LEGEND_ROW_HEIGHT))
        .flex()
        .items_center()
        .role(Role::Status)
        .aria_label("Loading")
        .child(
            svg()
                .path(LEGEND_LOADING_ICON)
                .size(px(11.0))
                .flex_none()
                .text_color(palette.muted)
                .with_animation(
                    "chart_legend_loading",
                    Animation::new(LEGEND_LOADING_PERIOD).repeat(),
                    |glyph, delta| {
                        glyph.with_transformation(Transformation::rotate(percentage(delta)))
                    },
                ),
        )
        .into_any_element()
}

#[derive(Clone, Copy)]
enum LegendControl {
    Visibility(bool),
    Settings,
    Remove,
}

fn legend_control(
    chart: &Entity<AerisChartView>,
    item: LegendItem,
    control: LegendControl,
    palette: LegendPalette,
) -> gpui::Stateful<gpui::Div> {
    let action_chart = chart.clone();
    let remove = matches!(control, LegendControl::Remove);
    let (label, path, color) = match control {
        LegendControl::Visibility(true) => ("Hide", LEGEND_VIEW_ICON, palette.text),
        LegendControl::Visibility(false) => ("Show", LEGEND_VIEW_OFF_ICON, palette.muted),
        LegendControl::Settings => ("Settings", LEGEND_SETTINGS_ICON, palette.text),
        LegendControl::Remove => ("Remove", LEGEND_REMOVE_ICON, palette.danger),
    };
    div()
        .id(legend_control_element_id(item, control))
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
                if matches!(control, LegendControl::Settings) {
                    let request = match item {
                        LegendItem::Study { study_id, .. } => {
                            Some(ChartSettingsRequest::Study(study_id))
                        }
                        LegendItem::BigTrades => Some(ChartSettingsRequest::BigTrades),
                        LegendItem::Volume => Some(ChartSettingsRequest::Volume),
                        LegendItem::OrderFlow(OrderFlowStudy::CumulativeDelta) => {
                            Some(ChartSettingsRequest::CumulativeDelta)
                        }
                        LegendItem::OrderFlow(OrderFlowStudy::Delta) => {
                            Some(ChartSettingsRequest::DeltaHistogram)
                        }
                        LegendItem::OrderBookDepth => Some(ChartSettingsRequest::OrderBookDepth),
                        _ => None,
                    };
                    if let Some(request) = request {
                        chart.pending_settings_request = Some(request);
                        chart_cx.notify();
                    }
                    return;
                }
                if remove && let LegendItem::Study { study_id, .. } = item {
                    chart.pending_study_remove = Some(study_id);
                    chart_cx.notify();
                    return;
                }
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

fn legend_control_element_id(item: LegendItem, control: LegendControl) -> (&'static str, u64) {
    let namespace = match control {
        LegendControl::Visibility(_) => "chart_legend_visibility_control",
        LegendControl::Settings => "chart_legend_settings_control",
        LegendControl::Remove => "chart_legend_remove_control",
    };
    (namespace, item.key())
}

impl Render for AerisChartView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.schedule_clock_tick(cx);
        // Kinetic coasts, held keyboard pans, and animated scrolls advance once per frame.
        if self.engine.input_animating() {
            window.request_animation_frame();
        }
        let mutation = self.apply_pending_data();
        let entity: Entity<Self> = cx.entity();
        let prepaint_entity = entity.clone();
        let hover_entity = entity.clone();
        let focus_handle = self
            .focus_handle
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        let legends = chart_legend_layers(
            &entity,
            &self.legend_rows(),
            &self.legend_panes,
            &self.platform,
            self.asset_loading.is_present(),
            self.legend_accessory.as_ref(),
        );

        div()
            .id(("aeris_chart_surface", cx.entity_id()))
            .relative()
            .size_full()
            .font_family(platform_font_family())
            .font_weight(gpui::FontWeight(f32::from(
                platform_typography().weight(TypographyRole::Normal),
            )))
            .cursor(self.cursor_style())
            .track_focus(&focus_handle)
            .key_context("AerisChart")
            .on_hover(move |hovered, _, cx| {
                if !*hovered {
                    hover_entity.update(cx, AerisChartView::on_pointer_left);
                }
            })
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_context_menu))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up_out))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_pinch(cx.listener(Self::on_pinch))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .child(
                canvas(
                    move |bounds: Bounds<gpui::Pixels>, window, cx| {
                        let width = bounds.size.width.into();
                        let height = bounds.size.height.into();
                        let scale_factor = window.scale_factor();
                        prepaint_entity.update(cx, |chart, chart_cx| {
                            chart.viewport_bounds = bounds;
                            chart.input.set_canvas_bounds(bounds);
                            if chart.rebuild(width, height, scale_factor, mutation, window) {
                                chart_cx.notify();
                            }
                            // The frame's input tick can arm a new deadline (the text caret
                            // blink does); without a wake for it the caret never toggles.
                            if chart.input_wake.is_none() {
                                chart.schedule_input_wake(chart_cx);
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
            .children(legends)
    }
}

#[cfg(test)]
mod tests;

mod drawing_stamps;
pub use drawing_stamps::ChartDrawingStamp;
use drawing_stamps::register_drawing_stamps;
mod drawings;
pub use drawings::{
    ChartDrawingAnchorError, ChartDrawingAnchorText, ChartDrawingSettings, drawing_color_hex,
};
mod builtin_indicators;
pub use builtin_indicators::BuiltinIndicator;
mod export;
mod indicators;
mod input;
mod order_book_depth;
use order_book_depth::OrderBookDepthState;
mod depth_heatmap;
use depth_heatmap::DepthHeatmapState;
mod order_flow;
pub use order_flow::classify_order_flow_sweeps;
mod palette;
pub use palette::{ChartAxisAppearance, ChartThemeDefaults};
mod pane_heights;
use palette::{PresetPalette, chart_theme, effective_color, theme_defaults};
pub use pane_heights::{ChartPaneContent, ChartPaneHeight};
mod studies;
pub use studies::{DEFAULT_STUDY_LINE_WIDTH, MAXIMUM_STUDY_LINE_WIDTH};
