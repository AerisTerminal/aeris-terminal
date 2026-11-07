//! Scan builder state on the Screener page: the draft scan the user edits, the saved scan
//! library, and the latest results of the scan being run. Scans go through the screener's one
//! market consumer; a pass is sent only while the Scans tab is shown, and a completed pass is
//! repeated on a bounded cadence so matches follow the forming bar.

use super::market_screener::{UNAVAILABLE_VALUE, compare_keys, fixed_to_f64, grouped_decimal};
use super::saved_scans::{
    MAXIMUM_SAVED_SCAN_NAME_BYTES, MAXIMUM_SAVED_SCANS, SavedScan, saved_scan_name_key,
    valid_saved_scan_name,
};
use super::*;
use aeris_contracts::WorkspaceScreenerScanState;
use aeris_localization::{Text, panels, tr, tr_count};
use aeris_market_runtime::{
    MAXIMUM_SCAN_CONDITIONS, MAXIMUM_SCAN_NUMBER_SCALE, MAXIMUM_SCAN_RESULTS,
    ProviderCatalogRejected, ProviderMarketScan, ProviderMarketScanMatch,
    ProviderPresentationDescriptor, ScanComparison, ScanCondition, ScanConditionValues,
    ScanDefinition, ScanDefinitionError, ScanMatchMode, ScanNumber, ScanOperand, ScanPriceField,
    ScanProgress, ScanProviderMarkets, ScanStudy, ScanStudyFamily, ScanStudyOperand,
    ScanStudyOutputName, ScanStudyParameter, ScanStudyParameterKind, ScanStudyParameterName,
    ScanValueUnit, scan_studies,
};
use gpui::{ScrollStrategy, UniformListScrollHandle};
use std::cmp::Ordering as CmpOrdering;

/// How many of the most traded markets a scan can cover.
pub(super) const SCAN_UNIVERSE_CHOICES: [u32; 4] = [50, 100, 200, 500];
const DEFAULT_SCAN_UNIVERSE: u32 = 100;
/// A completed pass runs again after this delay while the Scans tab is shown.
const SCAN_RESCAN_DELAY: Duration = Duration::from_secs(30);
/// A refused or unsendable pass is tried again after this delay.
const SCAN_RETRY_DELAY: Duration = Duration::from_secs(5);
/// A pass that publishes nothing for this long is presumed lost and replaced by a new one.
/// The provider publishes progress at most once per second, so only a stalled pass waits this
/// long.
const SCAN_RESPONSE_TIMEOUT: Duration = Duration::from_secs(90);

pub(super) const fn match_mode_label(mode: ScanMatchMode) -> Text {
    match mode {
        ScanMatchMode::All => panels::SCAN_MATCH_ALL,
        ScanMatchMode::Any => panels::SCAN_MATCH_ANY,
    }
}

pub(super) const MATCH_MODES: [ScanMatchMode; 2] = [ScanMatchMode::All, ScanMatchMode::Any];

pub(super) const COMPARISONS: [ScanComparison; 5] = [
    ScanComparison::Above,
    ScanComparison::Below,
    ScanComparison::CrossesAbove,
    ScanComparison::CrossesBelow,
    ScanComparison::Between,
];

pub(super) const fn comparison_label(comparison: ScanComparison) -> Text {
    match comparison {
        ScanComparison::Above => panels::SCAN_COMPARISON_ABOVE,
        ScanComparison::Below => panels::SCAN_COMPARISON_BELOW,
        ScanComparison::CrossesAbove => panels::SCAN_COMPARISON_CROSSES_ABOVE,
        ScanComparison::CrossesBelow => panels::SCAN_COMPARISON_CROSSES_BELOW,
        ScanComparison::Between => panels::SCAN_COMPARISON_BETWEEN,
    }
}

/// A study's short name, as a trader writes it on a chart, such as `RSI`.
pub(super) fn study_label(study: ScanStudy) -> &'static str {
    tr!(match study {
        ScanStudy::Sma => panels::SCAN_STUDY_SMA,
        ScanStudy::Ema => panels::SCAN_STUDY_EMA,
        ScanStudy::Wma => panels::SCAN_STUDY_WMA,
        ScanStudy::Dema => panels::SCAN_STUDY_DEMA,
        ScanStudy::Tema => panels::SCAN_STUDY_TEMA,
        ScanStudy::Smma => panels::SCAN_STUDY_SMMA,
        ScanStudy::Hma => panels::SCAN_STUDY_HMA,
        ScanStudy::Vwma => panels::SCAN_STUDY_VWMA,
        ScanStudy::Kama => panels::SCAN_STUDY_KAMA,
        ScanStudy::McGinley => panels::SCAN_STUDY_MCGINLEY,
        ScanStudy::Alma => panels::SCAN_STUDY_ALMA,
        ScanStudy::LinearRegression => panels::SCAN_STUDY_LINEAR_REGRESSION,
        ScanStudy::Rsi => panels::SCAN_STUDY_RSI,
        ScanStudy::Macd => panels::SCAN_STUDY_MACD,
        ScanStudy::Stochastic => panels::SCAN_STUDY_STOCHASTIC,
        ScanStudy::StochasticRsi => panels::SCAN_STUDY_STOCHASTIC_RSI,
        ScanStudy::Cci => panels::SCAN_STUDY_CCI,
        ScanStudy::WilliamsR => panels::SCAN_STUDY_WILLIAMS_R,
        ScanStudy::Momentum => panels::SCAN_STUDY_MOMENTUM,
        ScanStudy::RateOfChange => panels::SCAN_STUDY_ROC,
        ScanStudy::ChandeMomentum => panels::SCAN_STUDY_CHANDE_MOMENTUM,
        ScanStudy::Dpo => panels::SCAN_STUDY_DPO,
        ScanStudy::AwesomeOscillator => panels::SCAN_STUDY_AWESOME_OSCILLATOR,
        ScanStudy::Aroon => panels::SCAN_STUDY_AROON,
        ScanStudy::Trix => panels::SCAN_STUDY_TRIX,
        ScanStudy::Kst => panels::SCAN_STUDY_KST,
        ScanStudy::Tsi => panels::SCAN_STUDY_TSI,
        ScanStudy::FisherTransform => panels::SCAN_STUDY_FISHER_TRANSFORM,
        ScanStudy::UltimateOscillator => panels::SCAN_STUDY_ULTIMATE_OSCILLATOR,
        ScanStudy::CoppockCurve => panels::SCAN_STUDY_COPPOCK_CURVE,
        ScanStudy::AdxDmi => panels::SCAN_STUDY_ADX_DMI,
        ScanStudy::ParabolicSar => panels::SCAN_STUDY_PARABOLIC_SAR,
        ScanStudy::SuperTrend => panels::SCAN_STUDY_SUPERTREND,
        ScanStudy::Ichimoku => panels::SCAN_STUDY_ICHIMOKU,
        ScanStudy::Vortex => panels::SCAN_STUDY_VORTEX,
        ScanStudy::Choppiness => panels::SCAN_STUDY_CHOPPINESS,
        ScanStudy::MassIndex => panels::SCAN_STUDY_MASS_INDEX,
        ScanStudy::Atr => panels::SCAN_STUDY_ATR,
        ScanStudy::StandardDeviation => panels::SCAN_STUDY_STANDARD_DEVIATION,
        ScanStudy::HistoricalVolatility => panels::SCAN_STUDY_HISTORICAL_VOLATILITY,
        ScanStudy::Bollinger => panels::SCAN_STUDY_BOLLINGER,
        ScanStudy::BollingerMetrics => panels::SCAN_STUDY_BOLLINGER_METRICS,
        ScanStudy::Donchian => panels::SCAN_STUDY_DONCHIAN,
        ScanStudy::Keltner => panels::SCAN_STUDY_KELTNER,
        ScanStudy::Envelopes => panels::SCAN_STUDY_ENVELOPES,
        ScanStudy::AtrBands => panels::SCAN_STUDY_ATR_BANDS,
        ScanStudy::HighestHigh => panels::SCAN_OPERAND_HIGHEST_HIGH,
        ScanStudy::LowestLow => panels::SCAN_OPERAND_LOWEST_LOW,
        ScanStudy::ChaikinOscillator => panels::SCAN_STUDY_CHAIKIN_OSCILLATOR,
        ScanStudy::RelativeVolume => panels::SCAN_STUDY_RELATIVE_VOLUME,
        ScanStudy::Klinger => panels::SCAN_STUDY_KLINGER,
        ScanStudy::VolumeOscillator => panels::SCAN_STUDY_VOLUME_OSCILLATOR,
        ScanStudy::ElderForce => panels::SCAN_STUDY_ELDER_FORCE,
        ScanStudy::EaseOfMovement => panels::SCAN_STUDY_EASE_OF_MOVEMENT,
        ScanStudy::Cmf => panels::SCAN_STUDY_CMF,
        ScanStudy::Mfi => panels::SCAN_STUDY_MFI,
    })
}

pub(super) fn parameter_label(name: ScanStudyParameterName) -> &'static str {
    use ScanStudyParameterName as Name;
    tr!(match name {
        Name::Period => panels::SCAN_PARAMETER_PERIOD,
        Name::Deviation => panels::SCAN_PARAMETER_DEVIATION,
        Name::Multiplier => panels::SCAN_PARAMETER_MULTIPLIER,
        Name::Percent => panels::SCAN_PARAMETER_PERCENT,
        Name::Exponential => panels::SCAN_PARAMETER_EXPONENTIAL,
        Name::Fast => panels::SCAN_PARAMETER_FAST,
        Name::Slow => panels::SCAN_PARAMETER_SLOW,
        Name::Signal => panels::SCAN_PARAMETER_SIGNAL,
        Name::KPeriod => panels::SCAN_PARAMETER_K_PERIOD,
        Name::DPeriod => panels::SCAN_PARAMETER_D_PERIOD,
        Name::RsiPeriod => panels::SCAN_PARAMETER_RSI_PERIOD,
        Name::StochasticPeriod => panels::SCAN_PARAMETER_STOCHASTIC_PERIOD,
        Name::Offset => panels::SCAN_PARAMETER_OFFSET,
        Name::Sigma => panels::SCAN_PARAMETER_SIGMA,
        Name::Divisor => panels::SCAN_PARAMETER_DIVISOR,
        Name::Annualization => panels::SCAN_PARAMETER_ANNUALIZATION,
        Name::RocPeriod1 => panels::SCAN_PARAMETER_ROC_1,
        Name::RocPeriod2 => panels::SCAN_PARAMETER_ROC_2,
        Name::RocPeriod3 => panels::SCAN_PARAMETER_ROC_3,
        Name::RocPeriod4 => panels::SCAN_PARAMETER_ROC_4,
        Name::Smoothing1 => panels::SCAN_PARAMETER_SMOOTHING_1,
        Name::Smoothing2 => panels::SCAN_PARAMETER_SMOOTHING_2,
        Name::Smoothing3 => panels::SCAN_PARAMETER_SMOOTHING_3,
        Name::Smoothing4 => panels::SCAN_PARAMETER_SMOOTHING_4,
        Name::Long => panels::SCAN_PARAMETER_LONG,
        Name::Short => panels::SCAN_PARAMETER_SHORT,
        Name::LongPeriod => panels::SCAN_PARAMETER_LONG_PERIOD,
        Name::MediumPeriod => panels::SCAN_PARAMETER_MEDIUM_PERIOD,
        Name::ShortPeriod => panels::SCAN_PARAMETER_SHORT_PERIOD,
        Name::Smoothing => panels::SCAN_PARAMETER_SMOOTHING,
        Name::EmaPeriod => panels::SCAN_PARAMETER_EMA_PERIOD,
        Name::SumPeriod => panels::SCAN_PARAMETER_SUM_PERIOD,
    })
}

/// The name of a study line; a single-line study's only line has none.
pub(super) fn line_label(name: ScanStudyOutputName) -> Option<&'static str> {
    use ScanStudyOutputName as Line;
    Some(tr!(match name {
        Line::Value => return None,
        Line::Upper => panels::SCAN_LINE_UPPER,
        Line::Basis => panels::SCAN_LINE_BASIS,
        Line::Lower => panels::SCAN_LINE_LOWER,
        Line::Curve => panels::SCAN_LINE_CURVE,
        Line::Macd => panels::SCAN_LINE_MACD,
        Line::Signal => panels::SCAN_LINE_SIGNAL,
        Line::Histogram => panels::SCAN_LINE_HISTOGRAM,
        Line::PercentK => panels::SCAN_LINE_PERCENT_K,
        Line::PercentD => panels::SCAN_LINE_PERCENT_D,
        Line::AroonUp => panels::SCAN_LINE_AROON_UP,
        Line::AroonDown => panels::SCAN_LINE_AROON_DOWN,
        Line::Trix => panels::SCAN_LINE_TRIX,
        Line::Kst => panels::SCAN_LINE_KST,
        Line::Tsi => panels::SCAN_LINE_TSI,
        Line::Fisher => panels::SCAN_LINE_FISHER,
        Line::Trigger => panels::SCAN_LINE_TRIGGER,
        Line::PlusDi => panels::SCAN_LINE_PLUS_DI,
        Line::MinusDi => panels::SCAN_LINE_MINUS_DI,
        Line::Adx => panels::SCAN_LINE_ADX,
        Line::Conversion => panels::SCAN_LINE_CONVERSION,
        Line::Base => panels::SCAN_LINE_BASE,
        Line::LeadingA => panels::SCAN_LINE_LEADING_A,
        Line::LeadingB => panels::SCAN_LINE_LEADING_B,
        Line::PlusVi => panels::SCAN_LINE_PLUS_VI,
        Line::MinusVi => panels::SCAN_LINE_MINUS_VI,
        Line::PercentB => panels::SCAN_LINE_PERCENT_B,
        Line::BandWidth => panels::SCAN_LINE_BANDWIDTH,
        Line::Klinger => panels::SCAN_LINE_KLINGER,
        Line::Pvo => panels::SCAN_LINE_PVO,
    }))
}

/// Whether a line carries its study's own name, such as MACD's MACD line, so a label naming
/// the study already names the line.
const fn line_names_its_study(name: ScanStudyOutputName) -> bool {
    use ScanStudyOutputName as Line;
    matches!(
        name,
        Line::Value
            | Line::Macd
            | Line::Trix
            | Line::Kst
            | Line::Tsi
            | Line::Fisher
            | Line::Klinger
            | Line::Pvo
    )
}

pub(super) fn family_label(family: Option<ScanStudyFamily>) -> &'static str {
    tr!(match family {
        None => panels::SCAN_FAMILY_BAR,
        Some(ScanStudyFamily::MovingAverages) => panels::SCAN_FAMILY_MOVING_AVERAGES,
        Some(ScanStudyFamily::Oscillators) => panels::SCAN_FAMILY_OSCILLATORS,
        Some(ScanStudyFamily::Trend) => panels::SCAN_FAMILY_TREND,
        Some(ScanStudyFamily::Volatility) => panels::SCAN_FAMILY_VOLATILITY,
        Some(ScanStudyFamily::BandsAndChannels) => panels::SCAN_FAMILY_BANDS,
        Some(ScanStudyFamily::Volume) => panels::SCAN_FAMILY_VOLUME,
    })
}

/// What one side of a condition reads, as the operand menu lists it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OperandKind {
    Price(ScanPriceField),
    Volume,
    ChangePercent,
    Study(ScanStudy),
    /// A constant the user types; never the left side.
    Number,
}

/// One text input of an operand: a study parameter by its index, or the constant's value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OperandPart {
    Parameter(usize),
    Value,
}

impl OperandKind {
    /// The bar values a condition can measure, in menu order.
    pub(super) const BAR_VALUES: [Self; 6] = [
        Self::Price(ScanPriceField::Close),
        Self::Price(ScanPriceField::Open),
        Self::Price(ScanPriceField::High),
        Self::Price(ScanPriceField::Low),
        Self::Volume,
        Self::ChangePercent,
    ];

    /// The short name a trigger and a menu row show.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Price(ScanPriceField::Open) => tr!(panels::SCAN_OPERAND_OPEN),
            Self::Price(ScanPriceField::High) => tr!(panels::SCAN_OPERAND_HIGH),
            Self::Price(ScanPriceField::Low) => tr!(panels::SCAN_OPERAND_LOW),
            Self::Price(ScanPriceField::Close) => tr!(panels::SCAN_OPERAND_CLOSE),
            Self::Volume => tr!(panels::SCAN_OPERAND_VOLUME),
            Self::ChangePercent => tr!(panels::SCAN_OPERAND_CHANGE),
            Self::Study(study) => study_label(study),
            Self::Number => tr!(panels::SCAN_OPERAND_VALUE),
        }
    }

    /// The full name a menu row adds after a study's short name: the chart's name for the
    /// study, so the menu reads like the chart's indicator menu. The prior-bar extremes the
    /// chart does not draw have none.
    pub(super) fn description(self) -> Option<&'static str> {
        let Self::Study(study) = self else {
            return None;
        };
        ChartIndicator::from_identifier(study.descriptor().identifier).map(ChartIndicator::label)
    }

    /// Whether the kind's short or full name, or its chart identifier, contains `query`, which
    /// is already lowercase. Matching the identifier finds `rsi` in every display language.
    fn matches(self, query: &str) -> bool {
        query.is_empty()
            || [Some(self.label()), self.description()]
                .into_iter()
                .flatten()
                .any(|name| name.to_lowercase().contains(query))
            || matches!(self, Self::Study(study) if study.descriptor().identifier.contains(query))
    }

    /// The inputs the kind shows, in order; a study's on/off parameter shows a switch instead.
    pub(super) fn parts(self) -> Vec<OperandPart> {
        match self {
            Self::Price(_) | Self::Volume | Self::ChangePercent => Vec::new(),
            Self::Number => vec![OperandPart::Value],
            Self::Study(study) => study
                .descriptor()
                .parameters
                .iter()
                .enumerate()
                .filter(|(_, parameter)| parameter.kind != ScanStudyParameterKind::Toggle)
                .map(|(index, _)| OperandPart::Parameter(index))
                .collect(),
        }
    }

    const fn of(operand: ScanOperand) -> Self {
        match operand {
            ScanOperand::Price(field) => Self::Price(field),
            ScanOperand::Volume => Self::Volume,
            ScanOperand::ChangePercent => Self::ChangePercent,
            ScanOperand::Number(_) => Self::Number,
            ScanOperand::Study(study) => Self::Study(study.study),
        }
    }
}

/// One heading of the operand menu and the kinds under it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct OperandGroup {
    /// `None` for the bar values, otherwise the study family.
    pub(super) family: Option<ScanStudyFamily>,
    pub(super) kinds: Vec<OperandKind>,
}

/// The operand menu of `side`: the bar values, then every study by family, keeping only the
/// kinds `query` names and the groups left with any.
pub(super) fn operand_groups(side: ScanSide, query: &str) -> Vec<OperandGroup> {
    let query = query.trim().to_lowercase();
    // A constant never varies by market, so only the compared sides offer one.
    let bar = (side != ScanSide::Left)
        .then_some(OperandKind::Number)
        .into_iter()
        .chain(OperandKind::BAR_VALUES)
        .filter(|kind| kind.matches(&query))
        .collect();
    std::iter::once(OperandGroup {
        family: None,
        kinds: bar,
    })
    .chain(ScanStudyFamily::ALL.into_iter().map(|family| {
        OperandGroup {
            family: Some(family),
            kinds: scan_studies()
                .iter()
                .filter(|descriptor| descriptor.family == family)
                .map(|descriptor| OperandKind::Study(descriptor.study))
                .filter(|kind| kind.matches(&query))
                .collect(),
        }
    }))
    .filter(|group| !group.kinds.is_empty())
    .collect()
}

/// One side of a draft condition, with the text its inputs hold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct OperandDraft {
    pub(super) kind: OperandKind,
    /// The study line read, an index into the study's outputs.
    pub(super) output: usize,
    /// One text per study parameter, in the study's order.
    parameters: Vec<String>,
    value: String,
}

impl OperandDraft {
    pub(super) fn new(kind: OperandKind) -> Self {
        let parameters = match kind {
            OperandKind::Study(study) => study
                .descriptor()
                .parameters
                .iter()
                .map(|parameter| scan_number_text(parameter.default))
                .collect(),
            _ => Vec::new(),
        };
        Self {
            kind,
            output: 0,
            parameters,
            value: "0".to_string(),
        }
    }

    fn from_operand(operand: ScanOperand) -> Self {
        let mut draft = Self::new(OperandKind::of(operand));
        match operand {
            ScanOperand::Number(number) => draft.value = scan_number_text(number),
            ScanOperand::Study(study) => {
                draft.output = study.output;
                for (text, value) in draft.parameters.iter_mut().zip(study.parameters) {
                    *text = scan_number_text(value);
                }
            }
            ScanOperand::Price(_) | ScanOperand::Volume | ScanOperand::ChangePercent => {}
        }
        draft
    }

    pub(super) fn text(&self, part: OperandPart) -> &str {
        match part {
            OperandPart::Parameter(index) => self.parameters.get(index).map_or("", String::as_str),
            OperandPart::Value => &self.value,
        }
    }

    fn text_mut(&mut self, part: OperandPart) -> Option<&mut String> {
        match part {
            OperandPart::Parameter(index) => self.parameters.get_mut(index),
            OperandPart::Value => Some(&mut self.value),
        }
    }

    /// Whether the on/off parameter at `index` is on.
    pub(super) fn toggle(&self, index: usize) -> bool {
        parse_scan_number(self.text(OperandPart::Parameter(index)))
            .is_some_and(|value| value.units != 0)
    }

    /// Switches the kind; a different kind starts from its own defaults.
    fn set_kind(&mut self, kind: OperandKind) -> bool {
        if self.kind == kind {
            return false;
        }
        *self = Self::new(kind);
        true
    }

    fn set_output(&mut self, output: usize) -> bool {
        let OperandKind::Study(study) = self.kind else {
            return false;
        };
        if output == self.output || output >= study.descriptor().outputs.len() {
            return false;
        }
        self.output = output;
        true
    }

    fn set_toggle(&mut self, index: usize, on: bool) -> bool {
        let OperandKind::Study(study) = self.kind else {
            return false;
        };
        let is_toggle = study
            .descriptor()
            .parameters
            .get(index)
            .is_some_and(|parameter| parameter.kind == ScanStudyParameterKind::Toggle);
        let Some(text) = self.parameters.get_mut(index).filter(|_| is_toggle) else {
            return false;
        };
        *text = if on { "1" } else { "0" }.to_string();
        true
    }

    fn operand(&self, condition: usize) -> Result<ScanOperand, ScanDraftProblem> {
        Ok(match self.kind {
            OperandKind::Price(field) => ScanOperand::Price(field),
            OperandKind::Volume => ScanOperand::Volume,
            OperandKind::ChangePercent => ScanOperand::ChangePercent,
            OperandKind::Number => ScanOperand::Number(
                parse_scan_number(&self.value).ok_or(ScanDraftProblem::Number { condition })?,
            ),
            OperandKind::Study(study) => {
                let mut operand = ScanStudyOperand::new(study);
                operand.output = self.output;
                for (index, parameter) in study.descriptor().parameters.iter().enumerate() {
                    operand.parameters[index] =
                        parse_scan_number(self.text(OperandPart::Parameter(index)))
                            .filter(|value| parameter.admits(*value))
                            .ok_or(ScanDraftProblem::Parameter {
                                condition,
                                parameter: *parameter,
                            })?;
                }
                ScanOperand::Study(operand)
            }
        })
    }
}

/// Which side of a condition an operand fills.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ScanSide {
    Left,
    Right,
    /// The upper bound of a between condition.
    Upper,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ConditionDraft {
    pub(super) left: OperandDraft,
    pub(super) comparison: ScanComparison,
    pub(super) right: OperandDraft,
    /// Kept while another comparison is chosen, so switching back keeps the bound.
    pub(super) upper: OperandDraft,
}

impl Default for ConditionDraft {
    fn default() -> Self {
        Self {
            left: OperandDraft::new(OperandKind::Price(ScanPriceField::Close)),
            comparison: ScanComparison::Above,
            right: OperandDraft::new(OperandKind::Study(ScanStudy::Sma)),
            upper: OperandDraft::new(OperandKind::Number),
        }
    }
}

impl ConditionDraft {
    fn from_condition(condition: &ScanCondition) -> Self {
        Self {
            left: OperandDraft::from_operand(condition.left),
            comparison: condition.comparison,
            right: OperandDraft::from_operand(condition.right),
            upper: condition.upper.map_or_else(
                || OperandDraft::new(OperandKind::Number),
                OperandDraft::from_operand,
            ),
        }
    }

    pub(super) const fn operand(&self, side: ScanSide) -> &OperandDraft {
        match side {
            ScanSide::Left => &self.left,
            ScanSide::Right => &self.right,
            ScanSide::Upper => &self.upper,
        }
    }

    const fn operand_mut(&mut self, side: ScanSide) -> &mut OperandDraft {
        match side {
            ScanSide::Left => &mut self.left,
            ScanSide::Right => &mut self.right,
            ScanSide::Upper => &mut self.upper,
        }
    }

    /// The sides this condition shows, in order.
    pub(super) fn sides(&self) -> &'static [ScanSide] {
        if self.comparison == ScanComparison::Between {
            &[ScanSide::Left, ScanSide::Right, ScanSide::Upper]
        } else {
            &[ScanSide::Left, ScanSide::Right]
        }
    }

    fn condition(&self, index: usize) -> Result<ScanCondition, ScanDraftProblem> {
        let condition = ScanCondition {
            left: self.left.operand(index)?,
            comparison: self.comparison,
            right: self.right.operand(index)?,
            upper: if self.comparison == ScanComparison::Between {
                Some(self.upper.operand(index)?)
            } else {
                None
            },
        };
        ScanDefinition {
            conditions: vec![condition],
            ..ScanDefinition::default()
        }
        .validate()
        .map_err(|error| ScanDraftProblem::of_condition(error, index))?;
        Ok(condition)
    }
}

/// One editable text of the draft, the identity its input is bound to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ScanField {
    Name,
    Operand {
        condition: usize,
        side: ScanSide,
        part: OperandPart,
    },
}

/// The scan being edited.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ScanDraft {
    pub(super) name: String,
    pub(super) timeframe: ChartInterval,
    pub(super) match_mode: ScanMatchMode,
    pub(super) universe_limit: u32,
    pub(super) conditions: Vec<ConditionDraft>,
}

impl Default for ScanDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            timeframe: ScanDefinition::default().timeframe,
            match_mode: ScanMatchMode::default(),
            universe_limit: DEFAULT_SCAN_UNIVERSE,
            conditions: vec![ConditionDraft::default()],
        }
    }
}

impl ScanDraft {
    fn from_saved(scan: &SavedScan) -> Self {
        Self {
            name: scan.name.clone(),
            timeframe: scan.definition.timeframe,
            match_mode: scan.definition.match_mode,
            universe_limit: scan.universe_limit,
            conditions: scan
                .definition
                .conditions
                .iter()
                .map(ConditionDraft::from_condition)
                .collect(),
        }
    }

    /// Every text the draft shows, in display order.
    pub(super) fn fields(&self) -> Vec<ScanField> {
        let mut fields = vec![ScanField::Name];
        for (condition, draft) in self.conditions.iter().enumerate() {
            for side in draft.sides() {
                fields.extend(draft.operand(*side).kind.parts().into_iter().map(|part| {
                    ScanField::Operand {
                        condition,
                        side: *side,
                        part,
                    }
                }));
            }
        }
        fields
    }

    pub(super) fn field(&self, field: ScanField) -> Option<&str> {
        match field {
            ScanField::Name => Some(&self.name),
            ScanField::Operand {
                condition,
                side,
                part,
            } => Some(self.conditions.get(condition)?.operand(side).text(part)),
        }
    }

    /// Stores an input's text; reports whether the field exists.
    pub(super) fn set_field(&mut self, field: ScanField, value: String) -> bool {
        let slot = match field {
            ScanField::Name => Some(&mut self.name),
            ScanField::Operand {
                condition,
                side,
                part,
            } => self
                .conditions
                .get_mut(condition)
                .and_then(|draft| draft.operand_mut(side).text_mut(part)),
        };
        let Some(slot) = slot else {
            return false;
        };
        *slot = value;
        true
    }

    fn operand_mut(&mut self, condition: usize, side: ScanSide) -> Option<&mut OperandDraft> {
        Some(self.conditions.get_mut(condition)?.operand_mut(side))
    }

    pub(super) fn set_operand_kind(
        &mut self,
        condition: usize,
        side: ScanSide,
        kind: OperandKind,
    ) -> bool {
        // A constant never varies by market, so it cannot be what a condition measures.
        if side == ScanSide::Left && kind == OperandKind::Number {
            return false;
        }
        self.operand_mut(condition, side)
            .is_some_and(|draft| draft.set_kind(kind))
    }

    /// Chooses which line of a study operand's study is read.
    pub(super) fn set_operand_output(
        &mut self,
        condition: usize,
        side: ScanSide,
        output: usize,
    ) -> bool {
        self.operand_mut(condition, side)
            .is_some_and(|draft| draft.set_output(output))
    }

    /// Turns a study operand's on/off parameter on or off.
    pub(super) fn set_operand_toggle(
        &mut self,
        condition: usize,
        side: ScanSide,
        (index, on): (usize, bool),
    ) -> bool {
        self.operand_mut(condition, side)
            .is_some_and(|draft| draft.set_toggle(index, on))
    }

    pub(super) fn set_comparison(&mut self, condition: usize, comparison: ScanComparison) -> bool {
        let Some(draft) = self.conditions.get_mut(condition) else {
            return false;
        };
        draft.comparison = comparison;
        true
    }

    pub(super) fn can_add_condition(&self) -> bool {
        self.conditions.len() < MAXIMUM_SCAN_CONDITIONS
    }

    pub(super) fn add_condition(&mut self) -> bool {
        if !self.can_add_condition() {
            return false;
        }
        self.conditions.push(ConditionDraft::default());
        true
    }

    /// Removes a condition; the last one stays so the draft always has something to edit.
    pub(super) fn remove_condition(&mut self, index: usize) -> bool {
        if self.conditions.len() <= 1 || index >= self.conditions.len() {
            return false;
        }
        self.conditions.remove(index);
        true
    }

    /// The runtime definition the draft describes, or the first thing to correct.
    pub(super) fn definition(&self) -> Result<ScanDefinition, ScanDraftProblem> {
        if self.conditions.is_empty() {
            return Err(ScanDraftProblem::NoConditions);
        }
        let conditions = self
            .conditions
            .iter()
            .enumerate()
            .map(|(index, condition)| condition.condition(index))
            .collect::<Result<Vec<_>, _>>()?;
        let definition = ScanDefinition {
            timeframe: self.timeframe,
            match_mode: self.match_mode,
            conditions,
        };
        definition
            .validate()
            .map_err(|error| ScanDraftProblem::of_condition(error, 0))?;
        Ok(definition)
    }
}

/// Something in the draft that keeps it from running.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ScanDraftProblem {
    NoConditions,
    /// A study parameter is not a value within its bounds.
    Parameter {
        condition: usize,
        parameter: ScanStudyParameter,
    },
    Number {
        condition: usize,
    },
    FastNotBelowSlow {
        condition: usize,
    },
    Invalid {
        condition: usize,
    },
}

impl ScanDraftProblem {
    const fn of_condition(error: ScanDefinitionError, condition: usize) -> Self {
        match error {
            ScanDefinitionError::NoConditions => Self::NoConditions,
            ScanDefinitionError::FastPeriodNotBelowSlow => Self::FastNotBelowSlow { condition },
            ScanDefinitionError::NumberOutOfRange => Self::Number { condition },
            ScanDefinitionError::ParameterOutOfRange
            | ScanDefinitionError::OutputOutOfRange
            | ScanDefinitionError::TooManyConditions
            | ScanDefinitionError::ConstantLeftOperand
            | ScanDefinitionError::UpperBoundMismatch => Self::Invalid { condition },
        }
    }

    pub(super) fn message(self) -> String {
        match self {
            Self::NoConditions => tr!(panels::SCAN_PROBLEM_NO_CONDITIONS).to_string(),
            Self::Parameter {
                condition,
                parameter,
            } => {
                let key = if parameter.kind == ScanStudyParameterKind::Decimal {
                    panels::SCAN_PROBLEM_PARAMETER
                } else {
                    panels::SCAN_PROBLEM_WHOLE_PARAMETER
                };
                tr!(
                    key,
                    number = condition + 1,
                    parameter = parameter_label(parameter.name),
                    minimum = scan_number_text(parameter.minimum),
                    maximum = scan_number_text(parameter.maximum)
                )
            }
            Self::Number { condition } => tr!(panels::SCAN_PROBLEM_NUMBER, number = condition + 1),
            Self::FastNotBelowSlow { condition } => {
                tr!(panels::SCAN_PROBLEM_FAST_SLOW, number = condition + 1)
            }
            Self::Invalid { condition } => {
                tr!(panels::SCAN_PROBLEM_INVALID, number = condition + 1)
            }
        }
    }
}

/// A plain decimal such as `-12.5`, exactly, with at most the runtime's number scale.
pub(super) fn parse_scan_number(text: &str) -> Option<ScanNumber> {
    let text = text.trim();
    let (negative, unsigned) = text.strip_prefix('-').map_or_else(
        || (false, text.strip_prefix('+').unwrap_or(text)),
        |unsigned| (true, unsigned),
    );
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if (whole.is_empty() && fraction.is_empty())
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let fraction = fraction.trim_end_matches('0');
    let scale = u8::try_from(fraction.len())
        .ok()
        .filter(|scale| *scale <= MAXIMUM_SCAN_NUMBER_SCALE)?;
    let mut units = 0_i64;
    for digit in whole.bytes().chain(fraction.bytes()) {
        units = units
            .checked_mul(10)?
            .checked_add(i64::from(digit - b'0'))?;
    }
    Some(ScanNumber {
        units: if negative { -units } else { units },
        scale,
    })
}

pub(super) fn scan_number_text(number: ScanNumber) -> String {
    let sign = if number.units < 0 { "-" } else { "" };
    let digits = number.units.unsigned_abs().to_string();
    let scale = usize::from(number.scale);
    if scale == 0 {
        return format!("{sign}{digits}");
    }
    let padded = format!("{digits:0>width$}", width = scale + 1);
    let (whole, fraction) = padded.split_at(padded.len() - scale);
    format!("{sign}{whole}.{fraction}")
}

/// The label a condition's measured value carries, such as `RSI(14)` or
/// `MACD Signal(12, 26, 9)`. An on/off parameter is named only while it is on.
pub(super) fn operand_label(operand: ScanOperand) -> String {
    let ScanOperand::Study(study) = operand else {
        return match operand {
            ScanOperand::Number(number) => scan_number_text(number),
            _ => OperandKind::of(operand).label().to_string(),
        };
    };
    let name = study_label(study.study);
    let name = match study
        .output_descriptor()
        .filter(|output| !line_names_its_study(output.name))
        .and_then(|output| line_label(output.name))
    {
        Some(line) => tr!(panels::SCAN_STUDY_LINE_LABEL, study = name, line = line),
        None => name.to_string(),
    };
    let parameters = study
        .study
        .descriptor()
        .parameters
        .iter()
        .zip(study.parameters)
        .filter_map(|(parameter, value)| {
            if parameter.kind == ScanStudyParameterKind::Toggle {
                (value.units != 0).then(|| parameter_label(parameter.name).to_string())
            } else {
                Some(scan_number_text(value))
            }
        })
        .collect::<Vec<_>>();
    if parameters.is_empty() {
        name
    } else {
        format!("{name}({})", parameters.join(", "))
    }
}
/// Whether a condition's column shows its compared value: the left side is the close or
/// volume the row already shows and the right side varies by market, as for close above
/// SMA(20). A range shows the value it bounds.
fn shows_compared_value(condition: &ScanCondition) -> bool {
    matches!(
        condition.left,
        ScanOperand::Price(ScanPriceField::Close) | ScanOperand::Volume
    ) && !matches!(condition.right, ScanOperand::Number(_))
        && condition.comparison != ScanComparison::Between
}

/// The operand a condition's results column shows.
fn shown_operand(condition: &ScanCondition) -> ScanOperand {
    if shows_compared_value(condition) {
        condition.right
    } else {
        condition.left
    }
}

fn shown_value(condition: &ScanCondition, values: Option<&ScanConditionValues>) -> Option<f64> {
    let values = values?;
    if shows_compared_value(condition) {
        values.right
    } else {
        values.left
    }
}

/// Decimals of a match's prices: the digits its bars use, and at least the two (or the
/// price scale, when smaller) every market price shows.
fn match_price_decimals(scan_match: &ProviderMarketScanMatch) -> usize {
    let decimals = scan_match
        .price_decimals
        .max(scan_match.price_scale.min(2))
        .min(scan_match.price_scale);
    usize::try_from(decimals).unwrap_or(2)
}

/// The exact close at `decimals` fractional digits, so a row's prices line up.
fn close_text(scan_match: &ProviderMarketScanMatch, decimals: usize) -> String {
    let mut text = market_price_text(scan_match.close, scan_match.price_scale);
    let shown = text
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    if shown < decimals {
        if shown == 0 {
            text.push('.');
        }
        text.extend(std::iter::repeat_n('0', decimals - shown));
    }
    grouped_decimal(&text)
}

/// A measured value in the units its operand reads; price-scale values use `price_decimals`.
fn value_text(operand: ScanOperand, value: Option<f64>, price_decimals: usize) -> String {
    let Some(value) = value else {
        return UNAVAILABLE_VALUE.to_string();
    };
    let price = || grouped_decimal(&format!("{value:.price_decimals$}"));
    match operand {
        ScanOperand::ChangePercent => format!("{value:+.2}%"),
        ScanOperand::Volume => compact_quantity(value),
        ScanOperand::Study(study) => match study.output_descriptor().map(|output| output.unit) {
            Some(ScanValueUnit::Number) => format!("{value:.2}"),
            Some(ScanValueUnit::Multiple) => format!("{value:.2}×"),
            Some(ScanValueUnit::Quantity) => compact_quantity(value),
            Some(ScanValueUnit::Price) | None => price(),
        },
        ScanOperand::Price(_) | ScanOperand::Number(_) => price(),
    }
}

/// A quantity with a thousands suffix, such as `12.35M`.
fn compact_quantity(value: f64) -> String {
    let sign = if value < 0.0 { "-" } else { "" };
    let magnitude = value.abs();
    for (threshold, suffix) in [
        (1_000_000_000_000.0, "T"),
        (1_000_000_000.0, "B"),
        (1_000_000.0, "M"),
        (1_000.0, "K"),
    ] {
        if magnitude >= threshold {
            return format!("{sign}{:.2}{suffix}", magnitude / threshold);
        }
    }
    format!("{sign}{magnitude:.2}")
}

/// A sortable results column.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ScanResultColumn {
    Market,
    Close,
    Change,
    Volume,
    /// The measured value of the condition at this index.
    Condition(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ScanSort {
    pub(super) column: ScanResultColumn,
    pub(super) descending: bool,
}

impl Default for ScanSort {
    fn default() -> Self {
        Self {
            column: ScanResultColumn::Change,
            descending: true,
        }
    }
}

impl ScanSort {
    /// Clicking the sorted column flips its direction; another column starts in its natural one.
    pub(super) fn toggled(self, column: ScanResultColumn) -> Self {
        Self {
            column,
            descending: if self.column == column {
                !self.descending
            } else {
                column != ScanResultColumn::Market
            },
        }
    }
}

/// One presented match; every value is formatted once when the rows are rebuilt.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ScanResultRow {
    pub(super) symbol: String,
    pub(super) exchange: String,
    pub(super) display_symbol: String,
    pub(super) close: String,
    pub(super) change: String,
    pub(super) change_direction: Option<CmpOrdering>,
    pub(super) volume: String,
    pub(super) values: Vec<String>,
}

fn match_key(
    scan_match: &ProviderMarketScanMatch,
    definition: &ScanDefinition,
    column: ScanResultColumn,
) -> Option<f64> {
    match column {
        ScanResultColumn::Market => None,
        ScanResultColumn::Close => Some(fixed_to_f64(scan_match.close, scan_match.price_scale)),
        ScanResultColumn::Change => scan_match.change_percent,
        ScanResultColumn::Volume => {
            Some(fixed_to_f64(scan_match.volume, scan_match.quantity_scale))
        }
        ScanResultColumn::Condition(index) => shown_value(
            definition.conditions.get(index)?,
            scan_match.values.get(index),
        ),
    }
}

fn compare_matches(
    left: &ProviderMarketScanMatch,
    right: &ProviderMarketScanMatch,
    definition: &ScanDefinition,
    sort: ScanSort,
) -> CmpOrdering {
    let symbol =
        |scan_match: &ProviderMarketScanMatch| scan_match.instrument.display_symbol.to_lowercase();
    let primary = if sort.column == ScanResultColumn::Market {
        let ordering = symbol(left).cmp(&symbol(right));
        if sort.descending {
            ordering.reverse()
        } else {
            ordering
        }
    } else {
        compare_keys(
            match_key(left, definition, sort.column),
            match_key(right, definition, sort.column),
            sort.descending,
        )
    };
    primary.then_with(|| {
        left.instrument
            .display_symbol
            .cmp(&right.instrument.display_symbol)
    })
}

/// Orders and formats the matches of `definition` for presentation.
pub(super) fn scan_rows(
    matches: &[ProviderMarketScanMatch],
    definition: &ScanDefinition,
    sort: ScanSort,
) -> Vec<ScanResultRow> {
    let mut ordered = matches.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| compare_matches(left, right, definition, sort));
    ordered
        .into_iter()
        .map(|scan_match| {
            let decimals = match_price_decimals(scan_match);
            ScanResultRow {
                symbol: scan_match.instrument.symbol.clone(),
                exchange: scan_match.instrument.exchange.clone(),
                display_symbol: scan_match.instrument.display_symbol.clone(),
                close: close_text(scan_match, decimals),
                change: scan_match.change_percent.map_or_else(
                    || UNAVAILABLE_VALUE.to_string(),
                    |change| format!("{change:+.2}%"),
                ),
                change_direction: scan_match
                    .change_percent
                    .map(|change| change.total_cmp(&0.0)),
                volume: compact_quantity(fixed_to_f64(
                    scan_match.volume,
                    scan_match.quantity_scale,
                )),
                values: definition
                    .conditions
                    .iter()
                    .enumerate()
                    .map(|(index, condition)| {
                        value_text(
                            shown_operand(condition),
                            shown_value(condition, scan_match.values.get(index)),
                            decimals,
                        )
                    })
                    .collect(),
            }
        })
        .collect()
}

/// A notice about the run that the progress line shows in place of the pass summary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ScanRunNotice {
    /// The command could not enter the market consumer's mailbox.
    Busy,
    /// The runtime refused the pass.
    Retrying,
}

/// The scan being run and its latest answer.
pub(super) struct ScanRun {
    definition: ScanDefinition,
    universe_limit: u32,
    columns: Vec<String>,
    /// The outstanding pass and when it was sent or last published.
    pending: Option<(u64, Instant)>,
    next_pass_at: Option<Instant>,
    /// Until a pass completes, progress shows the partial matches; afterwards the last
    /// complete pass stays shown while the next one runs.
    completed: bool,
    progress: ScanProgress,
    captured_at_unix_millis: Option<i64>,
    matches: Vec<ProviderMarketScanMatch>,
    notice: Option<ScanRunNotice>,
}

impl ScanRun {
    fn new(definition: ScanDefinition, universe_limit: u32) -> Self {
        Self {
            columns: definition
                .conditions
                .iter()
                .map(|condition| operand_label(shown_operand(condition)))
                .collect(),
            definition,
            universe_limit,
            pending: None,
            next_pass_at: None,
            completed: false,
            progress: ScanProgress::default(),
            captured_at_unix_millis: None,
            matches: Vec::new(),
            notice: None,
        }
    }

    /// Whether `scan_generation` is the outstanding pass.
    fn awaits(&self, scan_generation: u64) -> bool {
        self.pending
            .is_some_and(|(generation, _)| generation == scan_generation)
    }

    pub(super) fn columns(&self) -> &[String] {
        &self.columns
    }

    /// The interval the matches were found on, which an opened match's chart uses.
    pub(super) const fn timeframe(&self) -> ChartInterval {
        self.definition.timeframe
    }

    pub(super) const fn scanning(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) const fn completed(&self) -> bool {
        self.completed
    }

    pub(super) const fn progress(&self) -> ScanProgress {
        self.progress
    }

    pub(super) const fn captured_at_unix_millis(&self) -> Option<i64> {
        self.captured_at_unix_millis
    }

    pub(super) const fn notice(&self) -> Option<ScanRunNotice> {
        self.notice
    }

    pub(super) fn match_count(&self) -> usize {
        self.matches.len()
    }
}

/// A result of a library action, shown beside the builder until the next edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ScanStatus {
    Problem(ScanDraftProblem),
    NameMissing,
    NameTooLong,
    NameTaken(String),
    LibraryFull,
    Saved(String),
    Deleted(String),
}

impl ScanStatus {
    pub(super) fn message(&self) -> String {
        match self {
            Self::Problem(problem) => problem.message(),
            Self::NameMissing => tr!(panels::SCAN_NAME_MISSING).to_string(),
            Self::NameTooLong => tr!(panels::SCAN_NAME_TOO_LONG).to_string(),
            Self::NameTaken(name) => tr!(panels::SCAN_NAME_TAKEN, name = name),
            Self::LibraryFull => tr!(panels::SCAN_LIBRARY_FULL, maximum = MAXIMUM_SAVED_SCANS),
            Self::Saved(name) => tr!(panels::SCAN_SAVED, name = name),
            Self::Deleted(name) => tr!(panels::SCAN_DELETED, name = name),
        }
    }

    pub(super) const fn is_problem(&self) -> bool {
        !matches!(self, Self::Saved(_) | Self::Deleted(_))
    }
}

/// The builder menu that is open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ScanMenu {
    Timeframe,
    MatchMode,
    Universe,
    Actions,
    Comparison(usize),
    Operand(usize, ScanSide),
    /// The line a study operand reads.
    Line(usize, ScanSide),
}

pub(super) struct MarketScan {
    provider: Option<&'static ProviderPresentationDescriptor>,
    draft: ScanDraft,
    saved: Vec<SavedScan>,
    /// The saved scan the draft edits; `None` for a new scan.
    selected: Option<usize>,
    run: Option<ScanRun>,
    next_generation: u64,
    sort: ScanSort,
    rows: Rc<Vec<ScanResultRow>>,
    status: Option<ScanStatus>,
    menu: Option<ScanMenu>,
    /// The search field of the open operand menu.
    operand_search: Option<Entity<InputState>>,
    /// The text the open operand menu's search field holds.
    operand_query: String,
    confirm_delete: bool,
    inputs: Vec<(ScanField, Entity<InputState>)>,
    pub(super) scroll: UniformListScrollHandle,
}

impl Default for MarketScan {
    fn default() -> Self {
        Self::new(
            aeris_market_runtime::built_in_provider_presentations()
                .iter()
                .find(|descriptor| descriptor.market_scan_available),
        )
    }
}

impl MarketScan {
    fn new(provider: Option<&'static ProviderPresentationDescriptor>) -> Self {
        Self {
            provider,
            draft: ScanDraft::default(),
            saved: Vec::new(),
            selected: None,
            run: None,
            next_generation: 1,
            sort: ScanSort::default(),
            rows: Rc::new(Vec::new()),
            status: None,
            menu: None,
            operand_search: None,
            operand_query: String::new(),
            confirm_delete: false,
            inputs: Vec::new(),
            scroll: UniformListScrollHandle::new(),
        }
    }

    pub(super) const fn provider(&self) -> Option<&'static ProviderPresentationDescriptor> {
        self.provider
    }

    /// The intervals the scanning provider offers, finest first, as its charts list them.
    pub(super) fn intervals(&self) -> Vec<ChartInterval> {
        self.provider.map_or_else(Vec::new, |provider| {
            intervals_from_labels(provider.chart_interval_labels)
        })
    }

    /// Every scanned market is listed by the scanning provider, so its logo marks each match.
    pub(super) fn exchange_logo(&self) -> Option<assets::ExchangeLogo> {
        self.provider
            .and_then(|descriptor| assets::ExchangeLogo::for_logo_key(descriptor.logo_key))
    }

    pub(super) const fn draft(&self) -> &ScanDraft {
        &self.draft
    }

    pub(super) fn saved(&self) -> &[SavedScan] {
        &self.saved
    }

    pub(super) const fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub(super) const fn run(&self) -> Option<&ScanRun> {
        self.run.as_ref()
    }

    pub(super) const fn sort(&self) -> ScanSort {
        self.sort
    }

    pub(super) fn rows(&self) -> Rc<Vec<ScanResultRow>> {
        Rc::clone(&self.rows)
    }

    pub(super) const fn status(&self) -> Option<&ScanStatus> {
        self.status.as_ref()
    }

    pub(super) const fn menu(&self) -> Option<ScanMenu> {
        self.menu
    }

    pub(super) const fn confirm_delete(&self) -> bool {
        self.confirm_delete
    }

    pub(super) fn input(&self, field: ScanField) -> Option<&Entity<InputState>> {
        self.inputs
            .iter()
            .find_map(|(bound, input)| (*bound == field).then_some(input))
    }

    /// Whether the bound inputs already match `fields`, in order.
    pub(super) fn inputs_match(&self, fields: &[ScanField]) -> bool {
        self.inputs
            .iter()
            .map(|(field, _)| *field)
            .eq(fields.iter().copied())
    }

    pub(super) fn set_inputs(&mut self, inputs: Vec<(ScanField, Entity<InputState>)>) {
        self.inputs = inputs;
    }

    /// Drops every bound input so the next refresh shows the draft's current texts.
    pub(super) fn clear_inputs(&mut self) {
        self.inputs.clear();
    }

    pub(super) fn restore(&mut self, saved: Vec<SavedScan>) {
        self.saved = saved;
        self.selected = None;
    }

    /// The library as the workspace document stores it.
    pub(super) fn saved_states(&self) -> Vec<WorkspaceScreenerScanState> {
        self.saved.iter().map(SavedScan::to_state).collect()
    }

    pub(super) fn toggle_menu(&mut self, menu: ScanMenu) {
        self.menu = (self.menu != Some(menu)).then_some(menu);
        self.operand_search = None;
        self.operand_query.clear();
        if menu != ScanMenu::Actions {
            self.confirm_delete = false;
        }
    }

    pub(super) fn close_menu(&mut self) {
        self.menu = None;
        self.operand_search = None;
        self.operand_query.clear();
        self.confirm_delete = false;
    }

    /// The open operand menu's search field.
    pub(super) const fn operand_search_input(&self) -> Option<&Entity<InputState>> {
        self.operand_search.as_ref()
    }

    pub(super) fn operand_search_query(&self) -> &str {
        &self.operand_query
    }

    /// Binds the search field of the operand menu that is open; any other menu has none.
    pub(super) fn set_operand_search(&mut self, input: Entity<InputState>) {
        if matches!(self.menu, Some(ScanMenu::Operand(..))) {
            self.operand_search = Some(input);
            self.operand_query.clear();
        }
    }

    /// Stores the search text when `input` is still the open menu's field.
    pub(super) fn set_operand_search_query(
        &mut self,
        input: &Entity<InputState>,
        query: String,
    ) -> bool {
        if self.operand_search.as_ref() != Some(input) || self.operand_query == query {
            return false;
        }
        self.operand_query = query;
        true
    }

    /// The first kind the open operand menu lists, which Enter in its search field picks.
    pub(super) fn first_operand_match(&self) -> Option<(usize, ScanSide, OperandKind)> {
        let Some(ScanMenu::Operand(condition, side)) = self.menu else {
            return None;
        };
        operand_groups(side, self.operand_search_query())
            .first()
            .and_then(|group| group.kinds.first())
            .map(|kind| (condition, side, *kind))
    }

    /// Applies one edit of the draft and clears the previous action's result.
    pub(super) fn edit(&mut self, change: impl FnOnce(&mut ScanDraft) -> bool) -> bool {
        let changed = change(&mut self.draft);
        if changed {
            self.status = None;
            self.confirm_delete = false;
        }
        changed
    }

    pub(super) fn new_scan(&mut self) {
        self.draft = ScanDraft::default();
        self.selected = None;
        self.status = None;
        self.close_menu();
    }

    /// Opens a saved scan in the builder and runs it.
    pub(super) fn select_saved(&mut self, index: usize) -> bool {
        let Some(scan) = self.saved.get(index) else {
            return false;
        };
        self.draft = ScanDraft::from_saved(scan);
        self.selected = Some(index);
        self.status = None;
        self.close_menu();
        self.run_draft();
        true
    }

    /// Starts running the draft; the next refresh sends its first pass.
    pub(super) fn run_draft(&mut self) -> bool {
        self.close_menu();
        match self.draft.definition() {
            Ok(definition) => {
                self.run = Some(ScanRun::new(definition, self.draft.universe_limit));
                self.rows = Rc::new(Vec::new());
                self.status = None;
                self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                true
            }
            Err(problem) => {
                self.status = Some(ScanStatus::Problem(problem));
                false
            }
        }
    }

    /// The draft as a saved scan named `name`, or what keeps it from being saved.
    fn savable(&self, name: &str) -> Result<SavedScan, ScanStatus> {
        let definition = self.draft.definition().map_err(ScanStatus::Problem)?;
        let name = name.trim();
        if name.is_empty() {
            return Err(ScanStatus::NameMissing);
        }
        if !valid_saved_scan_name(name) {
            return Err(ScanStatus::NameTooLong);
        }
        let key = saved_scan_name_key(name);
        if self.saved.iter().enumerate().any(|(index, scan)| {
            Some(index) != self.selected && saved_scan_name_key(&scan.name) == key
        }) {
            return Err(ScanStatus::NameTaken(name.to_string()));
        }
        Ok(SavedScan {
            name: name.to_string(),
            definition,
            universe_limit: self.draft.universe_limit,
        })
    }

    /// Saves the draft over the selected scan, or as a new one. Reports whether the library
    /// changed.
    pub(super) fn save(&mut self) -> bool {
        self.close_menu();
        let scan = match self.savable(&self.draft.name) {
            Ok(scan) => scan,
            Err(status) => {
                self.status = Some(status);
                return false;
            }
        };
        if let Some(index) = self.selected.filter(|index| *index < self.saved.len()) {
            self.saved[index] = scan.clone();
        } else if self.saved.len() < MAXIMUM_SAVED_SCANS {
            self.saved.push(scan.clone());
            self.selected = Some(self.saved.len() - 1);
        } else {
            self.status = Some(ScanStatus::LibraryFull);
            return false;
        }
        self.draft.name.clone_from(&scan.name);
        self.status = Some(ScanStatus::Saved(scan.name));
        true
    }

    /// Saves the draft as a new scan under a free copy name and selects it.
    pub(super) fn duplicate(&mut self) -> bool {
        self.close_menu();
        if self.saved.len() >= MAXIMUM_SAVED_SCANS {
            self.status = Some(ScanStatus::LibraryFull);
            return false;
        }
        let base = self.draft.name.trim();
        if base.is_empty() {
            self.status = Some(ScanStatus::NameMissing);
            return false;
        }
        let copy = tr!(panels::SCAN_COPY_NAME, name = base);
        let candidates = std::iter::once(fitting_name(&copy)).chain(
            (2..=MAXIMUM_SAVED_SCANS + 1).map(|number| fitting_name(&format!("{copy} {number}"))),
        );
        let Some(name) = candidates.into_iter().find(|name| {
            self.saved
                .iter()
                .all(|scan| saved_scan_name_key(&scan.name) != saved_scan_name_key(name))
        }) else {
            self.status = Some(ScanStatus::NameTaken(copy));
            return false;
        };
        self.selected = None;
        self.draft.name = name;
        self.save()
    }

    /// Deletes the selected scan on the second request; the first only asks to confirm.
    pub(super) fn request_delete(&mut self) -> bool {
        let Some(index) = self.selected.filter(|index| *index < self.saved.len()) else {
            return false;
        };
        if !self.confirm_delete {
            self.confirm_delete = true;
            return false;
        }
        let removed = self.saved.remove(index);
        self.selected = None;
        self.close_menu();
        self.status = Some(ScanStatus::Deleted(removed.name));
        true
    }

    pub(super) fn set_sort_column(&mut self, column: ScanResultColumn) {
        self.sort = self.sort.toggled(column);
        self.rebuild_rows();
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn rebuild_rows(&mut self) {
        self.rows = Rc::new(self.run.as_ref().map_or_else(Vec::new, |run| {
            scan_rows(&run.matches, &run.definition, self.sort)
        }));
    }

    /// Sends the running scan's next pass when one is due. Returns whether the presentation
    /// changed.
    pub(super) fn refresh_if_due(
        &mut self,
        now: Instant,
        worker: Option<&MarketDataWorker>,
    ) -> bool {
        let (Some(worker), Some(provider), Some(run)) = (worker, self.provider, self.run.as_mut())
        else {
            return false;
        };
        if run
            .pending
            .is_some_and(|(_, heard)| now.saturating_duration_since(heard) < SCAN_RESPONSE_TIMEOUT)
            || run.next_pass_at.is_some_and(|due| now < due)
        {
            return false;
        }
        let scan_generation = self.next_generation;
        self.next_generation = scan_generation.checked_add(1).unwrap_or(1);
        let request = ScanProviderMarkets {
            consumer_id: 0,
            scan_generation,
            provider: provider.id.to_string(),
            definition: run.definition.clone(),
            universe_limit: run.universe_limit,
            maximum_results: MAXIMUM_SCAN_RESULTS,
        };
        if worker.try_scan_provider(request).is_ok() {
            run.pending = Some((scan_generation, now));
            run.next_pass_at = None;
            false
        } else {
            run.pending = None;
            run.next_pass_at = Some(now + SCAN_RETRY_DELAY);
            run.notice = Some(ScanRunNotice::Busy);
            true
        }
    }

    /// Applies a publication of the outstanding pass.
    pub(super) fn apply_update(&mut self, scan: ProviderMarketScan, now: Instant) -> bool {
        let Some(run) = self.run.as_mut() else {
            return false;
        };
        if !run.awaits(scan.scan_generation) {
            return false;
        }
        run.notice = None;
        run.progress = scan.progress;
        run.captured_at_unix_millis = Some(scan.captured_at_unix_millis);
        let shown = scan.complete || !run.completed;
        if scan.complete {
            run.completed = true;
            run.pending = None;
            run.next_pass_at = Some(now + SCAN_RESCAN_DELAY);
        } else {
            run.pending = Some((scan.scan_generation, now));
        }
        if shown {
            run.matches = scan.matches;
            self.rebuild_rows();
        }
        true
    }

    /// Applies the runtime's refusal of the outstanding pass; the pass is tried again later.
    pub(super) fn apply_rejection(
        &mut self,
        rejection: &ProviderCatalogRejected,
        now: Instant,
    ) -> bool {
        let Some(run) = self.run.as_mut() else {
            return false;
        };
        if !run.awaits(rejection.command_generation) {
            return false;
        }
        run.pending = None;
        run.next_pass_at = Some(now + SCAN_RETRY_DELAY);
        run.notice = Some(ScanRunNotice::Retrying);
        true
    }

    /// Forgets the outstanding pass after the market consumer failed; a later refresh
    /// starts a new one.
    pub(super) fn lose_pending(&mut self) {
        if let Some(run) = self.run.as_mut() {
            run.pending = None;
            run.next_pass_at = None;
        }
    }
}

/// `name` cut to the saved-name bound at a character boundary.
fn fitting_name(name: &str) -> String {
    let mut end = name.len().min(MAXIMUM_SAVED_SCAN_NAME_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].trim().to_string()
}

/// The text summarizing the run's latest pass.
pub(super) fn run_summary(run: &ScanRun) -> String {
    let progress = run.progress();
    if run.scanning() && !run.completed() {
        return tr!(
            panels::SCAN_SCANNING,
            evaluated = progress.evaluated,
            universe = progress.universe
        );
    }
    let mut parts = vec![
        tr_count!(panels::SCAN_MATCHES, run.match_count()),
        tr!(
            panels::SCAN_SCANNED,
            evaluated = progress.evaluated,
            universe = progress.universe
        ),
    ];
    if progress.unavailable > 0 {
        parts.push(tr_count!(
            panels::SCAN_UNAVAILABLE_COUNT,
            usize::try_from(progress.unavailable).unwrap_or(usize::MAX)
        ));
    }
    if let Some(captured) = run.captured_at_unix_millis() {
        parts.push(tr!(
            panels::SCREENER_UPDATED,
            time = market_screener::capture_time_label(captured)
        ));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_market_runtime::{ProviderCatalogRejectionReason, ProviderInstrumentSummary};

    fn number(units: i64, scale: u8) -> ScanNumber {
        ScanNumber { units, scale }
    }

    #[test]
    fn numbers_parse_exactly_and_print_back() {
        assert_eq!(parse_scan_number("30"), Some(number(30, 0)));
        assert_eq!(parse_scan_number(" -0.05 "), Some(number(-5, 2)));
        assert_eq!(parse_scan_number("+1.250"), Some(number(125, 2)));
        assert_eq!(parse_scan_number(".5"), Some(number(5, 1)));
        assert_eq!(parse_scan_number("2."), Some(number(2, 0)));
        assert_eq!(
            parse_scan_number("0.123456789"),
            Some(number(123_456_789, 9))
        );
        for invalid in ["", ".", "-", "1,000", "1e3", "0.1234567891", "abc", "1.2.3"] {
            assert_eq!(parse_scan_number(invalid), None, "{invalid:?}");
        }
        assert_eq!(parse_scan_number("99999999999999999999"), None);
        assert_eq!(scan_number_text(number(-5, 2)), "-0.05");
        assert_eq!(scan_number_text(number(125, 2)), "1.25");
        assert_eq!(scan_number_text(number(30, 0)), "30");
    }

    #[test]
    fn the_default_draft_is_close_above_its_twenty_bar_average() {
        let definition = ScanDraft::default()
            .definition()
            .expect("default draft runs");
        assert_eq!(definition.timeframe, ChartInterval::Day1);
        assert_eq!(
            definition.conditions,
            vec![ScanCondition {
                left: ScanOperand::Price(ScanPriceField::Close),
                comparison: ScanComparison::Above,
                right: ScanOperand::Study(ScanStudyOperand::new(ScanStudy::Sma)),
                upper: None,
            }]
        );
        assert_eq!(
            ScanStudyOperand::new(ScanStudy::Sma).parameters[0],
            number(20, 0)
        );
    }

    #[test]
    fn the_builder_offers_every_interval_the_scanning_provider_charts() {
        assert_eq!(MarketScan::new(None).intervals(), Vec::new());
        let provider = aeris_market_runtime::built_in_provider_presentations()
            .iter()
            .find(|presentation| presentation.market_scan_available)
            .expect("a provider answers market scans");
        let intervals = MarketScan::new(Some(provider)).intervals();
        assert_eq!(
            intervals
                .iter()
                .map(|interval| interval.label())
                .collect::<Vec<_>>(),
            provider.chart_interval_labels
        );
        assert!(intervals.contains(&ScanDraft::default().timeframe));
    }

    #[test]
    fn draft_problems_name_the_condition_to_fix() {
        let mut draft = ScanDraft::default();
        assert!(draft.add_condition());
        let period = ScanField::Operand {
            condition: 1,
            side: ScanSide::Right,
            part: OperandPart::Parameter(0),
        };
        let period_problem = ScanDraftProblem::Parameter {
            condition: 1,
            parameter: ScanStudy::Sma.descriptor().parameters[0],
        };
        for invalid in ["0", "251", "2.5", "abc"] {
            assert!(draft.set_field(period, invalid.to_string()));
            assert_eq!(draft.definition(), Err(period_problem), "{invalid:?}");
        }
        assert!(
            period_problem.message().contains("250"),
            "the problem names the bound"
        );
        assert!(draft.set_field(period, "50".to_string()));
        assert!(draft.definition().is_ok());

        assert!(draft.set_operand_kind(0, ScanSide::Right, OperandKind::Number));
        let value = ScanField::Operand {
            condition: 0,
            side: ScanSide::Right,
            part: OperandPart::Value,
        };
        assert!(draft.set_field(value, "abc".to_string()));
        assert_eq!(
            draft.definition(),
            Err(ScanDraftProblem::Number { condition: 0 })
        );
        assert!(draft.set_field(value, "100".to_string()));

        assert!(draft.set_operand_kind(1, ScanSide::Left, OperandKind::Study(ScanStudy::Macd)));
        let fast = ScanField::Operand {
            condition: 1,
            side: ScanSide::Left,
            part: OperandPart::Parameter(0),
        };
        assert!(draft.set_field(fast, "30".to_string()));
        assert_eq!(
            draft.definition(),
            Err(ScanDraftProblem::FastNotBelowSlow { condition: 1 })
        );
        assert!(
            !draft.set_operand_kind(0, ScanSide::Left, OperandKind::Number),
            "a constant is never measured"
        );
    }

    #[test]
    fn between_reads_the_upper_bound_only_while_chosen() {
        let mut draft = ScanDraft::default();
        assert!(draft.set_operand_kind(0, ScanSide::Left, OperandKind::Study(ScanStudy::Rsi)));
        assert!(draft.set_operand_kind(0, ScanSide::Right, OperandKind::Number));
        assert!(draft.set_comparison(0, ScanComparison::Between));
        let lower = ScanField::Operand {
            condition: 0,
            side: ScanSide::Right,
            part: OperandPart::Value,
        };
        let upper = ScanField::Operand {
            condition: 0,
            side: ScanSide::Upper,
            part: OperandPart::Value,
        };
        assert!(draft.fields().contains(&upper));
        assert!(draft.set_field(lower, "30".to_string()));
        assert!(draft.set_field(upper, "70".to_string()));
        let condition = draft.definition().expect("range runs").conditions[0];
        assert_eq!(
            condition.left,
            ScanOperand::Study(ScanStudyOperand::new(ScanStudy::Rsi))
        );
        assert_eq!(condition.upper, Some(ScanOperand::Number(number(70, 0))));

        assert!(draft.set_comparison(0, ScanComparison::Above));
        assert!(!draft.fields().contains(&upper));
        assert_eq!(
            draft.definition().expect("above runs").conditions[0].upper,
            None
        );
        assert_eq!(
            draft.field(upper),
            Some("70"),
            "the bound is kept for later"
        );
    }

    #[test]
    fn a_line_choice_keeps_the_parameters_and_another_study_resets_them() {
        let mut draft = ScanDraft::default();
        let field = |part| ScanField::Operand {
            condition: 0,
            side: ScanSide::Left,
            part,
        };
        let macd = OperandKind::Study(ScanStudy::Macd);
        assert!(draft.set_operand_kind(0, ScanSide::Left, macd));
        assert_eq!(
            macd.parts(),
            [0, 1, 2].map(OperandPart::Parameter),
            "fast, slow and signal"
        );
        assert_eq!(draft.field(field(OperandPart::Parameter(0))), Some("12"));
        assert!(draft.set_field(field(OperandPart::Parameter(1)), "30".to_string()));
        let signal = ScanStudy::Macd
            .descriptor()
            .output_index(ScanStudyOutputName::Signal)
            .expect("MACD has a signal line");
        assert!(draft.set_operand_output(0, ScanSide::Left, signal));
        assert!(
            !draft.set_operand_output(0, ScanSide::Left, signal),
            "unchanged"
        );
        assert!(
            !draft.set_operand_output(0, ScanSide::Left, 3),
            "MACD has three lines"
        );
        assert_eq!(draft.field(field(OperandPart::Parameter(1))), Some("30"));
        let ScanOperand::Study(operand) = draft.definition().expect("runs").conditions[0].left
        else {
            panic!("a study operand");
        };
        assert_eq!(operand.output, signal);
        assert_eq!(operand.parameters[..3], [12, 30, 9].map(ScanNumber::from));

        assert!(
            !draft.set_operand_kind(0, ScanSide::Left, macd),
            "the same study"
        );
        assert!(draft.set_operand_kind(0, ScanSide::Left, OperandKind::Study(ScanStudy::Rsi)));
        assert_eq!(draft.field(field(OperandPart::Parameter(0))), Some("14"));
        assert_eq!(draft.conditions[0].left.output, 0);
        assert!(
            !draft.set_operand_output(0, ScanSide::Right, 1),
            "SMA has one line"
        );
        assert!(
            !draft.set_operand_output(0, ScanSide::Upper, 0),
            "a constant has none"
        );
    }

    #[test]
    fn an_on_off_parameter_is_a_switch_not_a_text() {
        let mut draft = ScanDraft::default();
        let envelopes = OperandKind::Study(ScanStudy::Envelopes);
        let descriptor = ScanStudy::Envelopes.descriptor();
        let exponential = descriptor
            .parameter_index(ScanStudyParameterName::Exponential)
            .expect("envelopes can average exponentially");
        assert_eq!(
            descriptor.parameters[exponential].kind,
            ScanStudyParameterKind::Toggle
        );
        assert!(
            !envelopes
                .parts()
                .contains(&OperandPart::Parameter(exponential))
        );
        assert!(draft.set_operand_kind(0, ScanSide::Right, envelopes));
        assert!(!draft.conditions[0].right.toggle(exponential));
        assert!(draft.set_operand_toggle(0, ScanSide::Right, (exponential, true)));
        assert!(draft.conditions[0].right.toggle(exponential));
        assert!(
            !draft.set_operand_toggle(0, ScanSide::Right, (0, true)),
            "the period is not on/off"
        );
        let right = draft.definition().expect("runs").conditions[0].right;
        let ScanOperand::Study(operand) = right else {
            panic!("a study operand");
        };
        assert_eq!(operand.parameters[exponential], ScanNumber::from(1));
        assert_eq!(operand_label(right), "Envelopes Upper(20, 2, Exponential)");
        assert!(draft.set_operand_toggle(0, ScanSide::Right, (exponential, false)));
        assert_eq!(
            operand_label(draft.definition().expect("runs").conditions[0].right),
            "Envelopes Upper(20, 2)",
            "an off switch is not named"
        );
    }

    #[test]
    fn the_operand_menu_groups_by_family_and_searches_every_name() {
        let groups = operand_groups(ScanSide::Left, "");
        assert_eq!(groups[0].family, None);
        assert_eq!(
            groups[0].kinds,
            OperandKind::BAR_VALUES,
            "no constant on the left"
        );
        assert_eq!(
            operand_groups(ScanSide::Right, "")[0].kinds[0],
            OperandKind::Number
        );
        assert_eq!(
            groups
                .iter()
                .skip(1)
                .map(|group| group.family)
                .collect::<Vec<_>>(),
            ScanStudyFamily::ALL.map(Some)
        );
        assert_eq!(
            groups.iter().map(|group| group.kinds.len()).sum::<usize>(),
            OperandKind::BAR_VALUES.len() + scan_studies().len(),
            "every study is offered once"
        );

        let kinds = |query: &str| {
            operand_groups(ScanSide::Left, query)
                .into_iter()
                .flat_map(|group| group.kinds)
                .collect::<Vec<_>>()
        };
        assert!(kinds("  RSI ").contains(&OperandKind::Study(ScanStudy::Rsi)));
        assert!(kinds("rsi").contains(&OperandKind::Study(ScanStudy::StochasticRsi)));
        assert_eq!(
            kinds("relative strength"),
            [OperandKind::Study(ScanStudy::Rsi)],
            "the chart's full name"
        );
        assert_eq!(
            kinds("supertrend"),
            [OperandKind::Study(ScanStudy::SuperTrend)]
        );
        assert_eq!(kinds("no such study"), []);
        assert_eq!(operand_groups(ScanSide::Left, "no such study"), []);
    }

    #[test]
    fn the_search_field_belongs_to_the_open_operand_menu() {
        let mut scans = MarketScan::new(None);
        assert_eq!(scans.first_operand_match(), None);
        scans.toggle_menu(ScanMenu::Operand(0, ScanSide::Right));
        assert_eq!(
            scans.first_operand_match(),
            Some((0, ScanSide::Right, OperandKind::Number))
        );
        scans.operand_query = "macd".to_string();
        assert_eq!(
            scans.first_operand_match(),
            Some((0, ScanSide::Right, OperandKind::Study(ScanStudy::Macd)))
        );
        scans.toggle_menu(ScanMenu::Timeframe);
        assert_eq!(
            scans.operand_search_query(),
            "",
            "another menu drops the search"
        );
        assert_eq!(scans.first_operand_match(), None);
    }

    #[test]
    fn every_chart_study_is_described_by_its_chart_name() {
        for descriptor in scan_studies() {
            let kind = OperandKind::Study(descriptor.study);
            let chart = ChartIndicator::from_identifier(descriptor.identifier);
            assert_eq!(
                chart.is_some(),
                !matches!(
                    descriptor.study,
                    ScanStudy::HighestHigh | ScanStudy::LowestLow
                ),
                "{} shares the chart's identifier",
                descriptor.identifier
            );
            assert_eq!(kind.description(), chart.map(ChartIndicator::label));
            assert_ne!(kind.label(), "");
            for output in descriptor.outputs {
                assert_eq!(
                    line_label(output.name).is_none(),
                    output.name == ScanStudyOutputName::Value,
                    "{} {:?}",
                    descriptor.identifier,
                    output.name
                );
            }
        }
    }

    #[test]
    fn a_saved_scan_reopens_as_the_draft_it_was_saved_from() {
        let mut draft = ScanDraft {
            name: "Oversold bounce".to_string(),
            timeframe: ChartInterval::Hour4,
            match_mode: ScanMatchMode::Any,
            universe_limit: 200,
            ..ScanDraft::default()
        };
        assert!(draft.add_condition());
        assert!(draft.set_operand_kind(
            1,
            ScanSide::Left,
            OperandKind::Study(ScanStudy::Bollinger)
        ));
        let lower = ScanStudy::Bollinger
            .descriptor()
            .output_index(ScanStudyOutputName::Lower)
            .expect("Bollinger has a lower band");
        assert!(draft.set_operand_output(1, ScanSide::Left, lower));
        assert!(draft.set_field(
            ScanField::Operand {
                condition: 1,
                side: ScanSide::Left,
                part: OperandPart::Parameter(1),
            },
            "2.5".to_string(),
        ));
        assert!(draft.set_operand_kind(1, ScanSide::Right, OperandKind::Number));
        assert!(draft.set_field(
            ScanField::Operand {
                condition: 1,
                side: ScanSide::Right,
                part: OperandPart::Value,
            },
            "0.5".to_string(),
        ));
        let mut scans = MarketScan::new(None);
        scans.draft = draft.clone();
        assert!(scans.save());
        let saved = SavedScan::from_state(&scans.saved_states()[0]).expect("saved scan reloads");
        let reopened = ScanDraft::from_saved(&saved);
        assert_eq!(reopened.definition(), draft.definition());
        assert_eq!(reopened.conditions[1].left.output, lower);
        assert_eq!(
            reopened.field(ScanField::Operand {
                condition: 1,
                side: ScanSide::Left,
                part: OperandPart::Parameter(1),
            }),
            Some("2.5")
        );
        assert_eq!(reopened.name, draft.name);
        assert_eq!(reopened.universe_limit, 200);
    }

    #[test]
    fn condition_labels_name_the_measured_study() {
        let study = |study: ScanStudy, output: ScanStudyOutputName| {
            let mut operand = ScanStudyOperand::new(study);
            operand.output = study
                .descriptor()
                .output_index(output)
                .expect("the study draws the line");
            operand
        };
        assert_eq!(
            operand_label(ScanOperand::Study(ScanStudyOperand::new(ScanStudy::Rsi))),
            "RSI(14)"
        );
        assert_eq!(
            operand_label(ScanOperand::Study(study(
                ScanStudy::Macd,
                ScanStudyOutputName::Histogram
            ))),
            "MACD Histogram(12, 26, 9)"
        );
        assert_eq!(
            operand_label(ScanOperand::Study(study(
                ScanStudy::Macd,
                ScanStudyOutputName::Macd
            ))),
            "MACD(12, 26, 9)",
            "the MACD line is the study's own name"
        );
        let mut bollinger = study(ScanStudy::Bollinger, ScanStudyOutputName::Upper);
        bollinger.parameters[1] = number(25, 1);
        assert_eq!(
            operand_label(ScanOperand::Study(bollinger)),
            "Bollinger Upper(20, 2.5)"
        );
        assert_eq!(
            operand_label(ScanOperand::Study(ScanStudyOperand::new(
                ScanStudy::AwesomeOscillator
            ))),
            OperandKind::Study(ScanStudy::AwesomeOscillator).label(),
            "a study without parameters"
        );
        assert_eq!(
            operand_label(ScanOperand::Price(ScanPriceField::Close)),
            OperandKind::Price(ScanPriceField::Close).label()
        );
    }

    #[test]
    fn library_names_stay_unique_and_bounded() {
        let mut scans = MarketScan::new(None);
        scans.draft.name = "Breakout".to_string();
        assert!(scans.save());
        scans.new_scan();
        scans.draft.name = " breakout ".to_string();
        assert!(!scans.save());
        assert_eq!(
            scans.status(),
            Some(&ScanStatus::NameTaken("breakout".to_string()))
        );
        scans.draft.name = String::new();
        assert!(!scans.save());
        assert_eq!(scans.status(), Some(&ScanStatus::NameMissing));
        scans.draft.name = "x".repeat(MAXIMUM_SAVED_SCAN_NAME_BYTES + 1);
        assert!(!scans.save());
        assert_eq!(scans.status(), Some(&ScanStatus::NameTooLong));

        assert!(scans.select_saved(0));
        scans.draft.name = "Breakout v2".to_string();
        assert!(scans.save(), "renaming saves over the selected scan");
        assert_eq!(scans.saved().len(), 1);
        assert_eq!(scans.saved()[0].name, "Breakout v2");

        assert!(scans.duplicate());
        assert_eq!(scans.saved().len(), 2);
        assert_eq!(scans.selected(), Some(1));
        assert_ne!(
            saved_scan_name_key(&scans.saved()[1].name),
            saved_scan_name_key(&scans.saved()[0].name)
        );
        assert!(scans.select_saved(0));
        assert!(scans.duplicate());
        assert_eq!(
            scans.saved().len(),
            3,
            "a second copy takes the next free name"
        );

        assert!(
            !scans.request_delete(),
            "the first delete only asks to confirm"
        );
        assert!(scans.confirm_delete());
        assert!(scans.request_delete());
        assert_eq!(scans.saved().len(), 2);
        assert_eq!(scans.selected(), None);
        assert!(!scans.request_delete(), "nothing is selected");

        for index in scans.saved().len()..MAXIMUM_SAVED_SCANS {
            scans.new_scan();
            scans.draft.name = format!("Scan {index}");
            assert!(scans.save());
        }
        scans.new_scan();
        scans.draft.name = "One too many".to_string();
        assert!(!scans.save());
        assert_eq!(scans.status(), Some(&ScanStatus::LibraryFull));
    }

    fn scan_match(
        symbol: &str,
        close: i64,
        change: Option<f64>,
        rsi: Option<f64>,
    ) -> ProviderMarketScanMatch {
        ProviderMarketScanMatch {
            instrument: ProviderInstrumentSummary {
                symbol: symbol.to_string(),
                display_symbol: format!("{symbol}-USDC"),
                exchange: "Fixture".to_string(),
                ..Default::default()
            },
            price_scale: 2,
            price_decimals: 0,
            close,
            quantity_scale: 3,
            volume: 12_345_678,
            change_percent: change,
            values: vec![ScanConditionValues {
                left: rsi,
                right: Some(30.0),
            }],
            bar_open_unix_millis: 0,
        }
    }

    fn rsi_definition() -> ScanDefinition {
        ScanDefinition {
            conditions: vec![ScanCondition {
                left: ScanOperand::Study(ScanStudyOperand::new(ScanStudy::Rsi)),
                comparison: ScanComparison::Below,
                right: ScanOperand::Number(number(30, 0)),
                upper: None,
            }],
            ..ScanDefinition::default()
        }
    }

    #[test]
    fn rows_format_exactly_and_sort_by_any_column() {
        let matches = vec![
            scan_match("BTC", 10_000_000, Some(2.5), Some(28.0)),
            scan_match("ETH", 300_050, Some(-1.25), Some(21.456)),
            scan_match("SOL", 15_000, None, None),
        ];
        let definition = rsi_definition();
        let symbols = |rows: &[ScanResultRow]| {
            rows.iter()
                .map(|row| row.symbol.clone())
                .collect::<Vec<_>>()
        };
        let rows = scan_rows(&matches, &definition, ScanSort::default());
        assert_eq!(
            symbols(&rows),
            ["BTC", "ETH", "SOL"],
            "change, unknown last"
        );
        assert_eq!(rows[0].close, "100,000.00");
        assert_eq!(rows[0].change, "+2.50%");
        assert_eq!(rows[0].change_direction, Some(CmpOrdering::Greater));
        assert_eq!(rows[0].volume, "12.35K");
        assert_eq!(rows[1].values, ["21.46"]);
        assert_eq!(rows[2].change, UNAVAILABLE_VALUE);
        assert_eq!(rows[2].values, [UNAVAILABLE_VALUE]);

        let by_rsi = ScanSort::default().toggled(ScanResultColumn::Condition(0));
        assert!(by_rsi.descending);
        let rows = scan_rows(
            &matches,
            &definition,
            by_rsi.toggled(ScanResultColumn::Condition(0)),
        );
        assert_eq!(
            symbols(&rows),
            ["ETH", "BTC", "SOL"],
            "ascending RSI, unknown last"
        );
        let by_market = ScanSort::default().toggled(ScanResultColumn::Market);
        assert!(!by_market.descending);
        assert_eq!(
            symbols(&scan_rows(
                &matches,
                &definition,
                by_market.toggled(ScanResultColumn::Market)
            )),
            ["SOL", "ETH", "BTC"]
        );
        assert_eq!(
            value_text(ScanOperand::ChangePercent, Some(3.0), 6),
            "+3.00%"
        );
    }

    #[test]
    fn a_close_comparison_shows_its_compared_value_at_the_close_precision() {
        let sma = ScanOperand::Study(ScanStudyOperand::new(ScanStudy::Sma));
        let close = ScanOperand::Price(ScanPriceField::Close);
        let definition = ScanDefinition {
            conditions: vec![
                ScanCondition {
                    left: close,
                    comparison: ScanComparison::Above,
                    right: sma,
                    upper: None,
                },
                ScanCondition {
                    left: close,
                    comparison: ScanComparison::Above,
                    right: ScanOperand::Number(number(1, 0)),
                    upper: None,
                },
            ],
            ..ScanDefinition::default()
        };
        let run = ScanRun::new(definition.clone(), DEFAULT_SCAN_UNIVERSE);
        assert_eq!(run.columns(), [operand_label(sma), operand_label(close)]);

        let mut fine = scan_match("PEPE", 0, Some(1.0), None);
        fine.price_scale = 8;
        fine.price_decimals = 6;
        fine.close = 123_400;
        fine.values = vec![
            ScanConditionValues {
                left: Some(0.001_234),
                right: Some(0.001_198_765),
            },
            ScanConditionValues {
                left: Some(0.001_234),
                right: Some(1.0),
            },
        ];
        let mut whole = scan_match("BTC", 0, Some(0.5), None);
        whole.price_scale = 8;
        whole.close = 9_712_300_000_000;
        whole.values = vec![
            ScanConditionValues {
                left: Some(97_123.0),
                right: Some(95_432.123_4),
            },
            ScanConditionValues {
                left: Some(97_123.0),
                right: Some(1.0),
            },
        ];
        let matches = [fine, whole];
        let rows = scan_rows(&matches, &definition, ScanSort::default());
        assert_eq!(rows[0].close, "0.001234", "trailing digits the bars use");
        assert_eq!(rows[0].values, ["0.001199", "0.001234"]);
        assert_eq!(rows[1].close, "97,123.00");
        assert_eq!(rows[1].values, ["95,432.12", "97,123.00"]);

        let by_average = ScanSort::default().toggled(ScanResultColumn::Condition(0));
        let rows = scan_rows(&matches, &definition, by_average);
        assert_eq!(rows[0].symbol, "BTC", "sorted by the shown average");
    }

    fn running(scans: &mut MarketScan) {
        scans.draft = ScanDraft::default();
        assert!(scans.run_draft());
    }

    fn publication(scan_generation: u64, complete: bool, matches: usize) -> ProviderMarketScan {
        ProviderMarketScan {
            scan_generation,
            complete,
            progress: ScanProgress {
                evaluated: 10,
                universe: 100,
                unavailable: 0,
            },
            captured_at_unix_millis: 1_700_000_000_000,
            matches: (0..matches)
                .map(|index| scan_match(&format!("M{index}"), 100, Some(1.0), Some(50.0)))
                .collect(),
            ..ProviderMarketScan::default()
        }
    }

    fn pass(scans: &mut MarketScan, scan_generation: u64, sent: Instant) {
        let run = scans.run.as_mut().expect("running");
        run.pending = Some((scan_generation, sent));
        run.next_pass_at = None;
    }

    #[test]
    fn only_the_outstanding_pass_is_applied() {
        let start = Instant::now();
        let mut scans = MarketScan::new(None);
        running(&mut scans);
        pass(&mut scans, 4, start);
        assert!(
            !scans.apply_update(publication(3, true, 2), start),
            "an older pass"
        );
        assert!(scans.apply_update(publication(4, false, 1), start));
        assert_eq!(
            scans.rows().len(),
            1,
            "a first pass shows its partial matches"
        );
        assert!(scans.run().expect("run").scanning());
        assert!(scans.apply_update(publication(4, true, 2), start));
        assert_eq!(scans.rows().len(), 2);
        assert!(!scans.run().expect("run").scanning());
        assert!(
            !scans.apply_update(publication(4, true, 5), start),
            "a completed pass is fenced"
        );

        pass(&mut scans, 5, start);
        assert!(scans.apply_update(publication(5, false, 7), start));
        assert_eq!(
            scans.rows().len(),
            2,
            "a rescan keeps the last complete pass shown"
        );
        let rejection = ProviderCatalogRejected {
            command_generation: 5,
            reason: ProviderCatalogRejectionReason::DispatchUnavailable,
            ..ProviderCatalogRejected::default()
        };
        assert!(scans.apply_rejection(&rejection, start));
        assert_eq!(
            scans.run().expect("run").notice(),
            Some(ScanRunNotice::Retrying)
        );
        assert!(
            !scans.apply_rejection(&rejection, start),
            "the pass is no longer outstanding"
        );

        assert!(scans.run_draft(), "running again starts over");
        assert!(scans.rows().is_empty());
        assert!(!scans.apply_update(publication(5, true, 1), start));
    }

    #[test]
    fn passes_repeat_on_a_bounded_cadence() {
        let start = Instant::now();
        let mut scans = MarketScan::new(None);
        running(&mut scans);
        let due = |scans: &MarketScan, now: Instant| {
            let run = scans.run().expect("run");
            !(run.pending.is_some_and(|(_, heard)| {
                now.saturating_duration_since(heard) < SCAN_RESPONSE_TIMEOUT
            }) || run.next_pass_at.is_some_and(|at| now < at))
        };
        assert!(due(&scans, start), "a new run sends at once");
        pass(&mut scans, 1, start);
        assert!(!due(
            &scans,
            start + SCAN_RESPONSE_TIMEOUT.saturating_sub(Duration::from_secs(1))
        ));
        assert!(
            due(&scans, start + SCAN_RESPONSE_TIMEOUT),
            "a silent pass is replaced"
        );
        assert!(scans.apply_update(publication(1, false, 0), start + Duration::from_secs(80)));
        assert!(
            !due(&scans, start + SCAN_RESPONSE_TIMEOUT),
            "progress restarts the silence timeout"
        );
        let finished = start + Duration::from_secs(100);
        assert!(scans.apply_update(publication(1, true, 0), finished));
        assert!(!due(
            &scans,
            finished + SCAN_RESCAN_DELAY.saturating_sub(Duration::from_secs(1))
        ));
        assert!(due(&scans, finished + SCAN_RESCAN_DELAY));
        assert!(
            !scans.refresh_if_due(finished + SCAN_RESCAN_DELAY, None),
            "no consumer"
        );
    }
}
