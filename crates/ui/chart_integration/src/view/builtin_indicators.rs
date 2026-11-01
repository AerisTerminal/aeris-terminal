//! Aeris Charts built-in indicators offered beyond the legacy native catalog.
//!
//! Aeris Charts computes, places and draws every one of these; the host only names them, keeps a
//! durable identifier for workspace persistence and supplies the product volume weights to the
//! studies that read them.

use aeris_charts_engine::IndicatorKind;
use aeris_localization::{chart_ui, tr};

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
    pub const ALL: [Self; 60] = [
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
    pub fn label(self) -> &'static str {
        match self {
            Self::Dema => tr!(chart_ui::INDICATOR_DEMA),
            Self::Tema => tr!(chart_ui::INDICATOR_TEMA),
            Self::Smma => tr!(chart_ui::INDICATOR_SMMA),
            Self::Hma => tr!(chart_ui::INDICATOR_HMA),
            Self::Alma => tr!(chart_ui::INDICATOR_ALMA),
            Self::Kama => tr!(chart_ui::INDICATOR_KAMA),
            Self::McGinley => tr!(chart_ui::INDICATOR_MCGINLEY),
            Self::Vwma => tr!(chart_ui::INDICATOR_VWMA),
            Self::LinearRegression => tr!(chart_ui::INDICATOR_LINEAR_REGRESSION),
            Self::Envelopes => tr!(chart_ui::INDICATOR_ENVELOPES),
            Self::Donchian => tr!(chart_ui::INDICATOR_DONCHIAN),
            Self::Keltner => tr!(chart_ui::INDICATOR_KELTNER),
            Self::AtrBands => tr!(chart_ui::INDICATOR_ATR_BANDS),
            Self::VwapBands => tr!(chart_ui::INDICATOR_VWAP_BANDS),
            Self::ParabolicSar => tr!(chart_ui::INDICATOR_PARABOLIC_SAR),
            Self::SuperTrend => tr!(chart_ui::INDICATOR_SUPERTREND),
            Self::Ichimoku => tr!(chart_ui::INDICATOR_ICHIMOKU),
            Self::ZigZag => tr!(chart_ui::INDICATOR_ZIGZAG),
            Self::PivotStandard => tr!(chart_ui::INDICATOR_PIVOT_STANDARD),
            Self::PivotFibonacci => tr!(chart_ui::INDICATOR_PIVOT_FIBONACCI),
            Self::PivotCamarilla => tr!(chart_ui::INDICATOR_PIVOT_CAMARILLA),
            Self::PivotWoodie => tr!(chart_ui::INDICATOR_PIVOT_WOODIE),
            Self::PivotDeMark => tr!(chart_ui::INDICATOR_PIVOT_DEMARK),
            Self::SwingPoints => tr!(chart_ui::INDICATOR_SWING_POINTS),
            Self::SessionLevels => tr!(chart_ui::INDICATOR_SESSION_LEVELS),
            Self::PreviousPeriodLevels => tr!(chart_ui::INDICATOR_PREVIOUS_PERIOD_LEVELS),
            Self::OpeningRange => tr!(chart_ui::INDICATOR_OPENING_RANGE),
            Self::Aroon => tr!(chart_ui::INDICATOR_AROON),
            Self::AwesomeOscillator => tr!(chart_ui::INDICATOR_AWESOME_OSCILLATOR),
            Self::Dpo => tr!(chart_ui::INDICATOR_DPO),
            Self::ChandeMomentum => tr!(chart_ui::INDICATOR_CHANDE_MOMENTUM),
            Self::BollingerMetrics => tr!(chart_ui::INDICATOR_BOLLINGER_METRICS),
            Self::StandardDeviation => tr!(chart_ui::INDICATOR_STANDARD_DEVIATION),
            Self::Cci => tr!(chart_ui::INDICATOR_CCI),
            Self::WilliamsR => tr!(chart_ui::INDICATOR_WILLIAMS_R),
            Self::StochasticRsi => tr!(chart_ui::INDICATOR_STOCHASTIC_RSI),
            Self::Momentum => tr!(chart_ui::INDICATOR_MOMENTUM),
            Self::RateOfChange => tr!(chart_ui::INDICATOR_ROC),
            Self::AdxDmi => tr!(chart_ui::INDICATOR_ADX_DMI),
            Self::Choppiness => tr!(chart_ui::INDICATOR_CHOPPINESS),
            Self::HistoricalVolatility => tr!(chart_ui::INDICATOR_HISTORICAL_VOLATILITY),
            Self::Trix => tr!(chart_ui::INDICATOR_TRIX),
            Self::Kst => tr!(chart_ui::INDICATOR_KST),
            Self::Tsi => tr!(chart_ui::INDICATOR_TSI),
            Self::MassIndex => tr!(chart_ui::INDICATOR_MASS_INDEX),
            Self::Vortex => tr!(chart_ui::INDICATOR_VORTEX),
            Self::CoppockCurve => tr!(chart_ui::INDICATOR_COPPOCK),
            Self::FisherTransform => tr!(chart_ui::INDICATOR_FISHER),
            Self::UltimateOscillator => tr!(chart_ui::INDICATOR_ULTIMATE_OSCILLATOR),
            Self::Obv => tr!(chart_ui::INDICATOR_OBV),
            Self::AccumulationDistribution => tr!(chart_ui::INDICATOR_ACCUMULATION_DISTRIBUTION),
            Self::PriceVolumeTrend => tr!(chart_ui::INDICATOR_PRICE_VOLUME_TREND),
            Self::ChaikinOscillator => tr!(chart_ui::INDICATOR_CHAIKIN_OSCILLATOR),
            Self::Klinger => tr!(chart_ui::INDICATOR_KLINGER),
            Self::RelativeVolume => tr!(chart_ui::INDICATOR_RELATIVE_VOLUME),
            Self::VolumeOscillator => tr!(chart_ui::INDICATOR_VOLUME_OSCILLATOR),
            Self::ElderForce => tr!(chart_ui::INDICATOR_ELDER_FORCE),
            Self::EaseOfMovement => tr!(chart_ui::INDICATOR_EASE_OF_MOVEMENT),
            Self::Cmf => tr!(chart_ui::INDICATOR_CMF),
            Self::Mfi => tr!(chart_ui::INDICATOR_MFI),
        }
    }

    /// The canonical defaults the indicator is created with.
    #[must_use]
    pub fn parameters(self) -> &'static str {
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
            | Self::Mfi => tr!(chart_ui::PARAMS_PERIOD_14),
            Self::Alma => tr!(chart_ui::PARAMS_ALMA),
            Self::Kama => tr!(chart_ui::PARAMS_KAMA),
            Self::LinearRegression => tr!(chart_ui::PARAMS_PERIOD_20_DEVIATION_2),
            Self::Envelopes => tr!(chart_ui::PARAMS_ENVELOPES),
            Self::Keltner | Self::AtrBands => tr!(chart_ui::PARAMS_MULTIPLIER_2),
            Self::VwapBands => tr!(chart_ui::PARAMS_VWAP_BANDS),
            Self::SuperTrend => tr!(chart_ui::PARAMS_MULTIPLIER_3),
            Self::ZigZag => tr!(chart_ui::PARAMS_DEVIATION_2_PERCENT),
            Self::ParabolicSar | Self::Ichimoku | Self::AwesomeOscillator => {
                tr!(chart_ui::PARAMS_STANDARD)
            }
            Self::PivotStandard
            | Self::PivotFibonacci
            | Self::PivotCamarilla
            | Self::PivotWoodie
            | Self::PivotDeMark => tr!(chart_ui::PARAMS_PRIOR_PERIOD_LEVELS),
            Self::SwingPoints => tr!(chart_ui::PARAMS_SWING_POINTS),
            Self::SessionLevels => tr!(chart_ui::PARAMS_UTC_SESSIONS),
            Self::PreviousPeriodLevels => tr!(chart_ui::PARAMS_UTC_DAYS),
            Self::OpeningRange => tr!(chart_ui::PARAMS_OPENING_RANGE),
            Self::BollingerMetrics => tr!(chart_ui::PARAMS_BOLLINGER),
            Self::StochasticRsi => tr!(chart_ui::PARAMS_STOCHASTIC_RSI),
            Self::HistoricalVolatility => tr!(chart_ui::PARAMS_HISTORICAL_VOLATILITY),
            Self::Trix => tr!(chart_ui::PARAMS_TRIX),
            Self::Kst => tr!(chart_ui::PARAMS_KST),
            Self::Tsi => tr!(chart_ui::PARAMS_TSI),
            Self::MassIndex => tr!(chart_ui::PARAMS_MASS_INDEX),
            Self::CoppockCurve => tr!(chart_ui::PARAMS_COPPOCK),
            Self::UltimateOscillator => tr!(chart_ui::PARAMS_ULTIMATE_OSCILLATOR),
            Self::Obv | Self::AccumulationDistribution | Self::PriceVolumeTrend => {
                tr!(chart_ui::PARAMS_CUMULATIVE)
            }
            Self::ChaikinOscillator => tr!(chart_ui::PARAMS_CHAIKIN_OSCILLATOR),
            Self::Klinger => tr!(chart_ui::PARAMS_KLINGER),
            Self::VolumeOscillator => tr!(chart_ui::PARAMS_MACD),
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
