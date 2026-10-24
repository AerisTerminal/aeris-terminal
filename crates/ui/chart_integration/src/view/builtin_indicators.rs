//! Aeris Charts built-in indicators offered beyond the legacy native catalog.
//!
//! Aeris Charts computes, places and draws every one of these; the host only names them, keeps a
//! durable identifier for workspace persistence and supplies the product volume weights to the
//! studies that read them.

use aeris_charts_engine::IndicatorKind;

/// One Aeris Charts built-in indicator, created with the engine's canonical defaults.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BuiltinIndicator {
    Dema,
    Tema,
    Smma,
    Hma,
    Alma,
    Kama,
    McGinley,
    Vwma,
    LinearRegression,
    Envelopes,
    Donchian,
    Keltner,
    AtrBands,
    VwapBands,
    ParabolicSar,
    SuperTrend,
    Ichimoku,
    ZigZag,
    PivotStandard,
    PivotFibonacci,
    PivotCamarilla,
    PivotWoodie,
    PivotDeMark,
    SwingPoints,
    MarketStructure,
    FairValueGaps,
    OrderBlocks,
    SessionLevels,
    PreviousPeriodLevels,
    OpeningRange,
    Aroon,
    AwesomeOscillator,
    Dpo,
    ChandeMomentum,
    BollingerMetrics,
    StandardDeviation,
    Cci,
    WilliamsR,
    StochasticRsi,
    Momentum,
    RateOfChange,
    AdxDmi,
    Choppiness,
    HistoricalVolatility,
    Trix,
    Kst,
    Tsi,
    MassIndex,
    Vortex,
    CoppockCurve,
    FisherTransform,
    UltimateOscillator,
    Obv,
    AccumulationDistribution,
    PriceVolumeTrend,
    ChaikinOscillator,
    Klinger,
    RelativeVolume,
    VolumeOscillator,
    ElderForce,
    EaseOfMovement,
    Cmf,
    Mfi,
}

/// The legacy schema query's implicit arguments, which select each study's canonical defaults.
const DEFAULT_PERIOD: usize = 14;
const DEFAULT_DEVIATION: f64 = 2.0;

impl BuiltinIndicator {
    /// Every built-in indicator in menu order: overlays, market structure, oscillators, volume.
    pub const ALL: [Self; 63] = [
        Self::Dema,
        Self::Tema,
        Self::Smma,
        Self::Hma,
        Self::Alma,
        Self::Kama,
        Self::McGinley,
        Self::Vwma,
        Self::LinearRegression,
        Self::Envelopes,
        Self::Donchian,
        Self::Keltner,
        Self::AtrBands,
        Self::VwapBands,
        Self::ParabolicSar,
        Self::SuperTrend,
        Self::Ichimoku,
        Self::ZigZag,
        Self::PivotStandard,
        Self::PivotFibonacci,
        Self::PivotCamarilla,
        Self::PivotWoodie,
        Self::PivotDeMark,
        Self::SwingPoints,
        Self::MarketStructure,
        Self::FairValueGaps,
        Self::OrderBlocks,
        Self::SessionLevels,
        Self::PreviousPeriodLevels,
        Self::OpeningRange,
        Self::Aroon,
        Self::AwesomeOscillator,
        Self::Dpo,
        Self::ChandeMomentum,
        Self::BollingerMetrics,
        Self::StandardDeviation,
        Self::Cci,
        Self::WilliamsR,
        Self::StochasticRsi,
        Self::Momentum,
        Self::RateOfChange,
        Self::AdxDmi,
        Self::Choppiness,
        Self::HistoricalVolatility,
        Self::Trix,
        Self::Kst,
        Self::Tsi,
        Self::MassIndex,
        Self::Vortex,
        Self::CoppockCurve,
        Self::FisherTransform,
        Self::UltimateOscillator,
        Self::Obv,
        Self::AccumulationDistribution,
        Self::PriceVolumeTrend,
        Self::ChaikinOscillator,
        Self::Klinger,
        Self::RelativeVolume,
        Self::VolumeOscillator,
        Self::ElderForce,
        Self::EaseOfMovement,
        Self::Cmf,
        Self::Mfi,
    ];

    /// Durable workspace identifier. Every identifier except the pivot variants is the Aeris
    /// Charts schema name of the study.
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::Dema => "dema",
            Self::Tema => "tema",
            Self::Smma => "smma",
            Self::Hma => "hma",
            Self::Alma => "alma",
            Self::Kama => "kama",
            Self::McGinley => "mcginley",
            Self::Vwma => "vwma",
            Self::LinearRegression => "linear_regression",
            Self::Envelopes => "envelopes",
            Self::Donchian => "donchian",
            Self::Keltner => "keltner",
            Self::AtrBands => "atr_bands",
            Self::VwapBands => "vwap_bands",
            Self::ParabolicSar => "parabolic_sar",
            Self::SuperTrend => "supertrend",
            Self::Ichimoku => "ichimoku",
            Self::ZigZag => "zigzag",
            Self::PivotStandard => "pivot_points_standard",
            Self::PivotFibonacci => "pivot_points_fibonacci",
            Self::PivotCamarilla => "pivot_points_camarilla",
            Self::PivotWoodie => "pivot_points_woodie",
            Self::PivotDeMark => "pivot_points_demark",
            Self::SwingPoints => "swing_points",
            Self::MarketStructure => "market_structure",
            Self::FairValueGaps => "fair_value_gaps",
            Self::OrderBlocks => "order_blocks",
            Self::SessionLevels => "session_levels",
            Self::PreviousPeriodLevels => "previous_period_levels",
            Self::OpeningRange => "opening_range",
            Self::Aroon => "aroon",
            Self::AwesomeOscillator => "awesome_oscillator",
            Self::Dpo => "dpo",
            Self::ChandeMomentum => "chande_momentum",
            Self::BollingerMetrics => "bollinger_metrics",
            Self::StandardDeviation => "standard_deviation",
            Self::Cci => "cci",
            Self::WilliamsR => "williams_r",
            Self::StochasticRsi => "stochastic_rsi",
            Self::Momentum => "momentum",
            Self::RateOfChange => "roc",
            Self::AdxDmi => "adx_dmi",
            Self::Choppiness => "choppiness",
            Self::HistoricalVolatility => "historical_volatility",
            Self::Trix => "trix",
            Self::Kst => "kst",
            Self::Tsi => "tsi",
            Self::MassIndex => "mass_index",
            Self::Vortex => "vortex",
            Self::CoppockCurve => "coppock_curve",
            Self::FisherTransform => "fisher_transform",
            Self::UltimateOscillator => "ultimate_oscillator",
            Self::Obv => "obv",
            Self::AccumulationDistribution => "accumulation_distribution",
            Self::PriceVolumeTrend => "price_volume_trend",
            Self::ChaikinOscillator => "chaikin_oscillator",
            Self::Klinger => "klinger",
            Self::RelativeVolume => "relative_volume",
            Self::VolumeOscillator => "volume_oscillator",
            Self::ElderForce => "elder_force",
            Self::EaseOfMovement => "ease_of_movement",
            Self::Cmf => "cmf",
            Self::Mfi => "mfi",
        }
    }

    /// Parses a durable workspace identifier.
    #[must_use]
    pub fn from_identifier(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|indicator| indicator.identifier() == value)
    }

    /// User-facing indicator name.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Dema => "Double Exponential Moving Average",
            Self::Tema => "Triple Exponential Moving Average",
            Self::Smma => "Smoothed Moving Average (RMA)",
            Self::Hma => "Hull Moving Average",
            Self::Alma => "Arnaud Legoux Moving Average",
            Self::Kama => "Kaufman Adaptive Moving Average",
            Self::McGinley => "McGinley Dynamic",
            Self::Vwma => "Volume Weighted Moving Average",
            Self::LinearRegression => "Linear Regression Channel",
            Self::Envelopes => "Moving Average Envelopes",
            Self::Donchian => "Donchian Channels",
            Self::Keltner => "Keltner Channels",
            Self::AtrBands => "ATR Bands",
            Self::VwapBands => "VWAP Bands",
            Self::ParabolicSar => "Parabolic SAR",
            Self::SuperTrend => "SuperTrend",
            Self::Ichimoku => "Ichimoku Cloud",
            Self::ZigZag => "Zig Zag",
            Self::PivotStandard => "Pivot Points Standard",
            Self::PivotFibonacci => "Pivot Points Fibonacci",
            Self::PivotCamarilla => "Pivot Points Camarilla",
            Self::PivotWoodie => "Pivot Points Woodie",
            Self::PivotDeMark => "Pivot Points DeMark",
            Self::SwingPoints => "Swing Highs and Lows",
            Self::MarketStructure => "Market Structure",
            Self::FairValueGaps => "Fair Value Gaps",
            Self::OrderBlocks => "Order Blocks",
            Self::SessionLevels => "Session High and Low",
            Self::PreviousPeriodLevels => "Previous Day High and Low",
            Self::OpeningRange => "Opening Range",
            Self::Aroon => "Aroon",
            Self::AwesomeOscillator => "Awesome Oscillator",
            Self::Dpo => "Detrended Price Oscillator",
            Self::ChandeMomentum => "Chande Momentum Oscillator",
            Self::BollingerMetrics => "Bollinger %B and Bandwidth",
            Self::StandardDeviation => "Standard Deviation",
            Self::Cci => "Commodity Channel Index",
            Self::WilliamsR => "Williams %R",
            Self::StochasticRsi => "Stochastic RSI",
            Self::Momentum => "Momentum",
            Self::RateOfChange => "Rate of Change",
            Self::AdxDmi => "ADX and DMI",
            Self::Choppiness => "Choppiness Index",
            Self::HistoricalVolatility => "Historical Volatility",
            Self::Trix => "TRIX",
            Self::Kst => "Know Sure Thing",
            Self::Tsi => "True Strength Index",
            Self::MassIndex => "Mass Index",
            Self::Vortex => "Vortex Indicator",
            Self::CoppockCurve => "Coppock Curve",
            Self::FisherTransform => "Fisher Transform",
            Self::UltimateOscillator => "Ultimate Oscillator",
            Self::Obv => "On Balance Volume",
            Self::AccumulationDistribution => "Accumulation/Distribution",
            Self::PriceVolumeTrend => "Price Volume Trend",
            Self::ChaikinOscillator => "Chaikin Oscillator",
            Self::Klinger => "Klinger Oscillator",
            Self::RelativeVolume => "Relative Volume",
            Self::VolumeOscillator => "Volume Oscillator",
            Self::ElderForce => "Elder Force Index",
            Self::EaseOfMovement => "Ease of Movement",
            Self::Cmf => "Chaikin Money Flow",
            Self::Mfi => "Money Flow Index",
        }
    }

    /// The canonical defaults the indicator is created with.
    #[must_use]
    pub const fn parameters(self) -> &'static str {
        match self {
            Self::Dema
            | Self::Tema
            | Self::Smma
            | Self::Hma
            | Self::McGinley
            | Self::Vwma
            | Self::Donchian
            | Self::Aroon
            | Self::Dpo
            | Self::ChandeMomentum
            | Self::StandardDeviation
            | Self::Cci
            | Self::WilliamsR
            | Self::Momentum
            | Self::RateOfChange
            | Self::AdxDmi
            | Self::Choppiness
            | Self::Vortex
            | Self::FisherTransform
            | Self::RelativeVolume
            | Self::ElderForce
            | Self::EaseOfMovement
            | Self::Cmf
            | Self::Mfi => "Period 14",
            Self::Alma => "Period 14 · Offset 0.85 · Sigma 6",
            Self::Kama => "Period 10 · Fast 2 · Slow 30",
            Self::LinearRegression => "Period 20 · Deviation 2",
            Self::Envelopes => "Period 14 · 2%",
            Self::Keltner | Self::AtrBands => "Period 14 · Multiplier 2",
            Self::VwapBands => "Session anchored · 1 standard deviation",
            Self::SuperTrend => "Period 14 · Multiplier 3",
            Self::ZigZag => "Deviation 2%",
            Self::ParabolicSar | Self::Ichimoku | Self::AwesomeOscillator => "Standard settings",
            Self::PivotStandard
            | Self::PivotFibonacci
            | Self::PivotCamarilla
            | Self::PivotWoodie
            | Self::PivotDeMark => "Prior period levels",
            Self::SwingPoints => "Left 5 · Right 5",
            Self::MarketStructure => "Left 5 · Right 5 · Close breaks",
            Self::FairValueGaps => "Touch mitigation · 20 active",
            Self::OrderBlocks => "Left 5 · Right 5 · 20 active",
            Self::SessionLevels => "UTC sessions",
            Self::PreviousPeriodLevels => "UTC days",
            Self::OpeningRange => "30 minutes · UTC sessions",
            Self::BollingerMetrics => "Period 14 · Deviation 2",
            Self::StochasticRsi => "RSI 14 · Stochastic 14",
            Self::HistoricalVolatility => "Period 14 · 252 days",
            Self::Trix => "Period 14 · Signal 9",
            Self::Kst => "ROC 10 · 15 · 20 · 30 · Signal 9",
            Self::Tsi => "Long 25 · Short 13 · Signal 13",
            Self::MassIndex => "EMA 9 · Sum 25",
            Self::CoppockCurve => "Long 14 · Short 11 · Smoothing 10",
            Self::UltimateOscillator => "Periods 7 · 14 · 28",
            Self::Obv | Self::AccumulationDistribution | Self::PriceVolumeTrend => "Cumulative",
            Self::ChaikinOscillator => "Fast 3 · Slow 10",
            Self::Klinger => "Fast 34 · Slow 55 · Signal 13",
            Self::VolumeOscillator => "Fast 12 · Slow 26 · Signal 9",
        }
    }

    /// Whether the study reads the product volume weights.
    #[must_use]
    pub const fn reads_volume(self) -> bool {
        matches!(
            self,
            Self::Vwma
                | Self::VwapBands
                | Self::Obv
                | Self::AccumulationDistribution
                | Self::PriceVolumeTrend
                | Self::ChaikinOscillator
                | Self::Klinger
                | Self::RelativeVolume
                | Self::VolumeOscillator
                | Self::ElderForce
                | Self::EaseOfMovement
                | Self::Cmf
                | Self::Mfi
        )
    }

    /// The Aeris Charts definition with that study's canonical defaults.
    pub(super) fn engine_kind(self) -> Option<IndicatorKind> {
        let (schema, period) = match self {
            Self::PivotStandard => ("pivot_points", 1),
            Self::PivotFibonacci => ("pivot_points", 2),
            Self::PivotCamarilla => ("pivot_points", 3),
            Self::PivotWoodie => ("pivot_points", 4),
            Self::PivotDeMark => ("pivot_points", 5),
            _ => (self.identifier(), DEFAULT_PERIOD),
        };
        IndicatorKind::schema_definition(schema, period, DEFAULT_DEVIATION)
    }

    /// The built-in indicator an engine binding of `kind` was created from.
    pub(super) fn from_engine_kind(kind: &IndicatorKind) -> Option<Self> {
        Self::ALL.into_iter().find(|indicator| {
            indicator
                .engine_kind()
                .is_some_and(|candidate| same_study(&candidate, kind))
        })
    }
}

/// Bindings keep their study across parameter edits; only the pivot variant names a different
/// catalog entry.
fn same_study(left: &IndicatorKind, right: &IndicatorKind) -> bool {
    match (left, right) {
        (
            IndicatorKind::PivotPoints { variant: left },
            IndicatorKind::PivotPoints { variant: right },
        ) => left == right,
        _ => std::mem::discriminant(left) == std::mem::discriminant(right),
    }
}
