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
    Vwma,
    Donchian,
    Keltner,
    VwapBands,
    Twap,
    ParabolicSar,
    SuperTrend,
    Ichimoku,
    PivotStandard,
    PivotFibonacci,
    PivotCamarilla,
    PivotWoodie,
    PivotDeMark,
    SwingPoints,
    SessionLevels,
    PreviousPeriodLevels,
    OpeningRange,
    AdrLevels,
    Cci,
    StochasticRsi,
    AdxDmi,
    SqueezeMomentum,
    Obv,
    RelativeVolume,
    Cmf,
    Mfi,
}

/// The legacy schema query's implicit arguments, which select each study's canonical defaults.
const DEFAULT_PERIOD: usize = 14;
const DEFAULT_DEVIATION: f64 = 2.0;

impl BuiltinIndicator {
    /// Every built-in indicator in menu order: overlays, market structure, oscillators, volume.
    pub const ALL: [Self; 26] = [
        Self::Vwma,
        Self::Donchian,
        Self::Keltner,
        Self::VwapBands,
        Self::Twap,
        Self::ParabolicSar,
        Self::SuperTrend,
        Self::Ichimoku,
        Self::PivotStandard,
        Self::PivotFibonacci,
        Self::PivotCamarilla,
        Self::PivotWoodie,
        Self::PivotDeMark,
        Self::SwingPoints,
        Self::SessionLevels,
        Self::PreviousPeriodLevels,
        Self::OpeningRange,
        Self::AdrLevels,
        Self::Cci,
        Self::StochasticRsi,
        Self::AdxDmi,
        Self::SqueezeMomentum,
        Self::Obv,
        Self::RelativeVolume,
        Self::Cmf,
        Self::Mfi,
    ];

    /// Durable workspace identifier. Every identifier except the pivot variants is the Aeris
    /// Charts schema name of the study.
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::Vwma => "vwma",
            Self::Donchian => "donchian",
            Self::Keltner => "keltner",
            Self::VwapBands => "vwap_bands",
            Self::Twap => "twap",
            Self::ParabolicSar => "parabolic_sar",
            Self::SuperTrend => "supertrend",
            Self::Ichimoku => "ichimoku",
            Self::PivotStandard => "pivot_points_standard",
            Self::PivotFibonacci => "pivot_points_fibonacci",
            Self::PivotCamarilla => "pivot_points_camarilla",
            Self::PivotWoodie => "pivot_points_woodie",
            Self::PivotDeMark => "pivot_points_demark",
            Self::SwingPoints => "swing_points",
            Self::SessionLevels => "session_levels",
            Self::PreviousPeriodLevels => "previous_period_levels",
            Self::OpeningRange => "opening_range",
            Self::AdrLevels => "adr_levels",
            Self::Cci => "cci",
            Self::StochasticRsi => "stochastic_rsi",
            Self::AdxDmi => "adx_dmi",
            Self::SqueezeMomentum => "squeeze_momentum",
            Self::Obv => "obv",
            Self::RelativeVolume => "relative_volume",
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
            Self::Vwma => tr!(chart_ui::INDICATOR_VWMA),
            Self::Donchian => tr!(chart_ui::INDICATOR_DONCHIAN),
            Self::Keltner => tr!(chart_ui::INDICATOR_KELTNER),
            Self::VwapBands => tr!(chart_ui::INDICATOR_VWAP_BANDS),
            Self::Twap => tr!(chart_ui::INDICATOR_TWAP),
            Self::ParabolicSar => tr!(chart_ui::INDICATOR_PARABOLIC_SAR),
            Self::SuperTrend => tr!(chart_ui::INDICATOR_SUPERTREND),
            Self::Ichimoku => tr!(chart_ui::INDICATOR_ICHIMOKU),
            Self::PivotStandard => tr!(chart_ui::INDICATOR_PIVOT_STANDARD),
            Self::PivotFibonacci => tr!(chart_ui::INDICATOR_PIVOT_FIBONACCI),
            Self::PivotCamarilla => tr!(chart_ui::INDICATOR_PIVOT_CAMARILLA),
            Self::PivotWoodie => tr!(chart_ui::INDICATOR_PIVOT_WOODIE),
            Self::PivotDeMark => tr!(chart_ui::INDICATOR_PIVOT_DEMARK),
            Self::SwingPoints => tr!(chart_ui::INDICATOR_SWING_POINTS),
            Self::SessionLevels => tr!(chart_ui::INDICATOR_SESSION_LEVELS),
            Self::PreviousPeriodLevels => tr!(chart_ui::INDICATOR_PREVIOUS_PERIOD_LEVELS),
            Self::OpeningRange => tr!(chart_ui::INDICATOR_OPENING_RANGE),
            Self::AdrLevels => tr!(chart_ui::INDICATOR_ADR_LEVELS),
            Self::Cci => tr!(chart_ui::INDICATOR_CCI),
            Self::StochasticRsi => tr!(chart_ui::INDICATOR_STOCHASTIC_RSI),
            Self::AdxDmi => tr!(chart_ui::INDICATOR_ADX_DMI),
            Self::SqueezeMomentum => tr!(chart_ui::INDICATOR_SQUEEZE_MOMENTUM),
            Self::Obv => tr!(chart_ui::INDICATOR_OBV),
            Self::RelativeVolume => tr!(chart_ui::INDICATOR_RELATIVE_VOLUME),
            Self::Cmf => tr!(chart_ui::INDICATOR_CMF),
            Self::Mfi => tr!(chart_ui::INDICATOR_MFI),
        }
    }

    /// The canonical defaults the indicator is created with.
    #[must_use]
    pub fn parameters(self) -> &'static str {
        match self {
            Self::Vwma
            | Self::Donchian
            | Self::Cci
            | Self::AdxDmi
            | Self::RelativeVolume
            | Self::Cmf
            | Self::Mfi
            | Self::AdrLevels => tr!(chart_ui::PARAMS_PERIOD_14),
            Self::Keltner => tr!(chart_ui::PARAMS_MULTIPLIER_2),
            Self::VwapBands => tr!(chart_ui::PARAMS_VWAP_BANDS),
            Self::Twap => tr!(chart_ui::PARAMS_SESSION_ANCHORED),
            Self::SuperTrend => tr!(chart_ui::PARAMS_MULTIPLIER_3),
            Self::ParabolicSar | Self::Ichimoku => tr!(chart_ui::PARAMS_STANDARD),
            Self::PivotStandard
            | Self::PivotFibonacci
            | Self::PivotCamarilla
            | Self::PivotWoodie
            | Self::PivotDeMark => tr!(chart_ui::PARAMS_PRIOR_PERIOD_LEVELS),
            Self::SwingPoints => tr!(chart_ui::PARAMS_SWING_POINTS),
            Self::SessionLevels => tr!(chart_ui::PARAMS_UTC_SESSIONS),
            Self::PreviousPeriodLevels => tr!(chart_ui::PARAMS_UTC_DAYS),
            Self::OpeningRange => tr!(chart_ui::PARAMS_OPENING_RANGE),
            Self::StochasticRsi => tr!(chart_ui::PARAMS_STOCHASTIC_RSI),
            Self::SqueezeMomentum => tr!(chart_ui::PARAMS_SQUEEZE_MOMENTUM),
            Self::Obv => tr!(chart_ui::PARAMS_CUMULATIVE),
        }
    }

    /// Whether the study reads the product volume weights.
    #[must_use]
    pub const fn reads_volume(self) -> bool {
        matches!(
            self,
            Self::Vwma | Self::VwapBands | Self::Obv | Self::RelativeVolume | Self::Cmf | Self::Mfi
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
