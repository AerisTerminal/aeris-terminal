//! The chart's study library as scan operands.
//!
//! Each study is one descriptor: its parameters (kind, default, bounds), its output lines and
//! its calculation, which for every chart study builds the same `aeris_charts_indicators`
//! incremental state the chart builds over the same bars, so the scan's value is the chart's
//! value. The evaluator, the desktop builder and saved-scan persistence all read this table, so
//! adding a study is one entry here. Identifiers are the Aeris Charts schema names of the study
//! and its parameters, and saved scans store them.
//!
//! Not listed: studies whose value depends on where loaded history starts (on balance volume,
//! accumulation/distribution, price volume trend), which a scan cannot match to the chart until
//! operands can read a change over N bars, and studies that need a session calendar (VWAP and
//! its bands, pivots, session levels).

use aeris_charts_indicators::IncrementalState;

use super::{
    MAXIMUM_SCAN_NUMBER_SCALE, MAXIMUM_SCAN_PERIOD, ScanBars, ScanDefinitionError, ScanNumber,
};

/// Most parameters one study takes: Know Sure Thing reads four rates of change, four
/// smoothings and a signal.
pub const MAXIMUM_SCAN_STUDY_PARAMETERS: usize = 9;

/// Recursive formulas (EMA, RSI, ATR, ...) depend on how much history seeds them. This many
/// periods of warm-up bring their current value within display precision of a chart that
/// loaded longer history.
const RECURSIVE_WARMUP_PERIODS: usize = 10;
/// Parabolic SAR has no period and restarts at every reversal; this many bars span several.
const PARABOLIC_SAR_HISTORY_BARS: usize = 250;
/// Ichimoku's longest window, leading span B's 52 bars.
const ICHIMOKU_HISTORY_BARS: usize = 52;
/// The awesome oscillator's slow 34-bar average.
const AWESOME_OSCILLATOR_HISTORY_BARS: usize = 34;

/// One study a scan operand can read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanStudy {
    Sma,
    Ema,
    Wma,
    Dema,
    Tema,
    Smma,
    Hma,
    Vwma,
    Kama,
    McGinley,
    Alma,
    LinearRegression,
    Rsi,
    Macd,
    Stochastic,
    StochasticRsi,
    Cci,
    WilliamsR,
    Momentum,
    RateOfChange,
    ChandeMomentum,
    Dpo,
    AwesomeOscillator,
    Aroon,
    Trix,
    Kst,
    Tsi,
    FisherTransform,
    UltimateOscillator,
    CoppockCurve,
    AdxDmi,
    ParabolicSar,
    SuperTrend,
    Ichimoku,
    Vortex,
    Choppiness,
    MassIndex,
    Atr,
    StandardDeviation,
    HistoricalVolatility,
    Bollinger,
    BollingerMetrics,
    Donchian,
    Keltner,
    Envelopes,
    AtrBands,
    /// Highest high of the `period` bars before the current bar.
    HighestHigh,
    /// Lowest low of the `period` bars before the current bar.
    LowestLow,
    ChaikinOscillator,
    /// Current volume divided by the mean volume of the previous `period` bars.
    RelativeVolume,
    Klinger,
    VolumeOscillator,
    ElderForce,
    EaseOfMovement,
    Cmf,
    Mfi,
}

impl ScanStudy {
    #[must_use]
    pub fn descriptor(self) -> &'static ScanStudyDescriptor {
        &SCAN_STUDIES[self as usize]
    }

    /// The study a saved scan names, if this build offers it.
    #[must_use]
    pub fn from_identifier(identifier: &str) -> Option<Self> {
        SCAN_STUDIES
            .iter()
            .find(|descriptor| descriptor.identifier == identifier)
            .map(|descriptor| descriptor.study)
    }
}

/// Every study, in menu order within each family.
#[must_use]
pub fn scan_studies() -> &'static [ScanStudyDescriptor] {
    &SCAN_STUDIES
}

/// The menu group a study is listed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanStudyFamily {
    MovingAverages,
    Oscillators,
    Trend,
    Volatility,
    BandsAndChannels,
    Volume,
}

impl ScanStudyFamily {
    pub const ALL: [Self; 6] = [
        Self::MovingAverages,
        Self::Oscillators,
        Self::Trend,
        Self::Volatility,
        Self::BandsAndChannels,
        Self::Volume,
    ];
}

/// How a parameter's value is entered and checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanStudyParameterKind {
    /// A whole number of bars.
    Period,
    /// A decimal such as a band width or a multiplier.
    Decimal,
    /// Off (0) or on (1).
    Toggle,
}

/// What a parameter sets; its identifier is the chart's parameter name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanStudyParameterName {
    Period,
    Deviation,
    Multiplier,
    Percent,
    Exponential,
    Fast,
    Slow,
    Signal,
    KPeriod,
    DPeriod,
    RsiPeriod,
    StochasticPeriod,
    Offset,
    Sigma,
    Divisor,
    Annualization,
    RocPeriod1,
    RocPeriod2,
    RocPeriod3,
    RocPeriod4,
    Smoothing1,
    Smoothing2,
    Smoothing3,
    Smoothing4,
    Long,
    Short,
    LongPeriod,
    MediumPeriod,
    ShortPeriod,
    Smoothing,
    EmaPeriod,
    SumPeriod,
}

impl ScanStudyParameterName {
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::Period => "period",
            Self::Deviation => "deviation",
            Self::Multiplier => "multiplier",
            Self::Percent => "percent",
            Self::Exponential => "exponential",
            Self::Fast => "fast",
            Self::Slow => "slow",
            Self::Signal => "signal",
            Self::KPeriod => "k_period",
            Self::DPeriod => "d_period",
            Self::RsiPeriod => "rsi_period",
            Self::StochasticPeriod => "stochastic_period",
            Self::Offset => "offset",
            Self::Sigma => "sigma",
            Self::Divisor => "divisor",
            Self::Annualization => "annualization",
            Self::RocPeriod1 => "roc_1",
            Self::RocPeriod2 => "roc_2",
            Self::RocPeriod3 => "roc_3",
            Self::RocPeriod4 => "roc_4",
            Self::Smoothing1 => "smoothing_1",
            Self::Smoothing2 => "smoothing_2",
            Self::Smoothing3 => "smoothing_3",
            Self::Smoothing4 => "smoothing_4",
            Self::Long => "long",
            Self::Short => "short",
            Self::LongPeriod => "long_period",
            Self::MediumPeriod => "medium_period",
            Self::ShortPeriod => "short_period",
            Self::Smoothing => "smoothing",
            Self::EmaPeriod => "ema_period",
            Self::SumPeriod => "sum_period",
        }
    }
}

/// One parameter of a study, with its default and inclusive bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScanStudyParameter {
    pub name: ScanStudyParameterName,
    pub kind: ScanStudyParameterKind,
    pub default: ScanNumber,
    pub minimum: ScanNumber,
    pub maximum: ScanNumber,
}

impl ScanStudyParameter {
    /// Whether `value` is a value of this parameter.
    #[must_use]
    pub fn admits(&self, value: ScanNumber) -> bool {
        if value.scale > MAXIMUM_SCAN_NUMBER_SCALE
            || (self.kind != ScanStudyParameterKind::Decimal && value.whole().is_none())
        {
            return false;
        }
        let value = value.value();
        self.minimum.value() <= value && value <= self.maximum.value()
    }
}

/// What one output line of a study is; its identifier is what saved scans store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanStudyOutputName {
    /// The only line of a single-line study.
    Value,
    Upper,
    Basis,
    Lower,
    Curve,
    Macd,
    Signal,
    Histogram,
    PercentK,
    PercentD,
    AroonUp,
    AroonDown,
    Trix,
    Kst,
    Tsi,
    Fisher,
    Trigger,
    PlusDi,
    MinusDi,
    Adx,
    Conversion,
    Base,
    LeadingA,
    LeadingB,
    PlusVi,
    MinusVi,
    PercentB,
    BandWidth,
    Klinger,
    Pvo,
}

impl ScanStudyOutputName {
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Upper => "upper",
            Self::Basis => "basis",
            Self::Lower => "lower",
            Self::Curve => "curve",
            Self::Macd => "macd",
            Self::Signal => "signal",
            Self::Histogram => "histogram",
            Self::PercentK => "percent_k",
            Self::PercentD => "percent_d",
            Self::AroonUp => "aroon_up",
            Self::AroonDown => "aroon_down",
            Self::Trix => "trix",
            Self::Kst => "kst",
            Self::Tsi => "tsi",
            Self::Fisher => "fisher",
            Self::Trigger => "trigger",
            Self::PlusDi => "plus_di",
            Self::MinusDi => "minus_di",
            Self::Adx => "adx",
            Self::Conversion => "conversion",
            Self::Base => "base",
            Self::LeadingA => "leading_a",
            Self::LeadingB => "leading_b",
            Self::PlusVi => "plus_vi",
            Self::MinusVi => "minus_vi",
            Self::PercentB => "percent_b",
            Self::BandWidth => "bandwidth",
            Self::Klinger => "klinger",
            Self::Pvo => "pvo",
        }
    }
}

/// The units an output's values are in, which decide how they are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanValueUnit {
    /// The instrument's price units, such as a moving average or a band.
    Price,
    /// A unitless level, such as an oscillator reading or a percentage.
    Number,
    /// A multiple of a reference, such as volume relative to its average.
    Multiple,
    /// Volume-weighted quantities, which reach millions.
    Quantity,
}

/// One output line of a study.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScanStudyOutput {
    pub name: ScanStudyOutputName,
    pub unit: ScanValueUnit,
}

/// How a study's values are computed.
#[derive(Clone, Copy, Debug)]
enum Calculation {
    /// The chart's own incremental formula state.
    Chart(fn(Values<'_>) -> IncrementalState),
    /// The extreme of the bars before the current one, which no chart study draws.
    PriorHighest,
    PriorLowest,
}

/// One study of the table.
#[derive(Clone, Copy, Debug)]
pub struct ScanStudyDescriptor {
    pub study: ScanStudy,
    /// The Aeris Charts schema name, stored by saved scans.
    pub identifier: &'static str,
    pub family: ScanStudyFamily,
    /// Parameters in the order a study operand holds their values.
    pub parameters: &'static [ScanStudyParameter],
    /// Output lines in the order a study operand indexes them.
    pub outputs: &'static [ScanStudyOutput],
    calculation: Calculation,
    /// Bars one instrument needs so the current value is settled.
    history: fn(Values<'_>) -> usize,
}

impl ScanStudyDescriptor {
    #[must_use]
    pub fn parameter_index(&self, name: ScanStudyParameterName) -> Option<usize> {
        self.parameters
            .iter()
            .position(|parameter| parameter.name == name)
    }

    #[must_use]
    pub fn output_index(&self, name: ScanStudyOutputName) -> Option<usize> {
        self.outputs.iter().position(|output| output.name == name)
    }
}

/// A study as one side of a condition: which study, its parameter values and the line read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScanStudyOperand {
    pub study: ScanStudy,
    /// Values in the descriptor's parameter order; the slots past its parameters are zero.
    pub parameters: [ScanNumber; MAXIMUM_SCAN_STUDY_PARAMETERS],
    /// Index into the descriptor's outputs.
    pub output: usize,
}

impl ScanStudyOperand {
    /// The study's first line with every parameter at its default.
    #[must_use]
    pub fn new(study: ScanStudy) -> Self {
        let mut parameters = [ScanNumber::default(); MAXIMUM_SCAN_STUDY_PARAMETERS];
        for (slot, parameter) in parameters.iter_mut().zip(study.descriptor().parameters) {
            *slot = parameter.default;
        }
        Self {
            study,
            parameters,
            output: 0,
        }
    }

    #[must_use]
    pub fn output_descriptor(&self) -> Option<&'static ScanStudyOutput> {
        self.study.descriptor().outputs.get(self.output)
    }

    pub(super) fn validate(&self) -> Result<(), ScanDefinitionError> {
        let descriptor = self.study.descriptor();
        if self.output >= descriptor.outputs.len() {
            return Err(ScanDefinitionError::OutputOutOfRange);
        }
        for (index, value) in self.parameters.iter().enumerate() {
            let admitted = descriptor.parameters.get(index).map_or_else(
                || *value == ScanNumber::default(),
                |parameter| parameter.admits(*value),
            );
            if !admitted {
                return Err(ScanDefinitionError::ParameterOutOfRange);
            }
        }
        let fast = descriptor.parameter_index(ScanStudyParameterName::Fast);
        let slow = descriptor.parameter_index(ScanStudyParameterName::Slow);
        if let (Some(fast), Some(slow)) = (fast, slow)
            && self.parameters[fast].value() >= self.parameters[slow].value()
        {
            return Err(ScanDefinitionError::FastPeriodNotBelowSlow);
        }
        Ok(())
    }

    pub(super) fn history_bars(&self) -> usize {
        (self.study.descriptor().history)(Values(&self.parameters))
    }

    /// Every output line of the study over `bars`, row-aligned; `None` before warm-up.
    pub(super) fn series(&self, bars: &ScanBars) -> Vec<Vec<Option<f64>>> {
        let descriptor = self.study.descriptor();
        let values = Values(&self.parameters);
        match descriptor.calculation {
            Calculation::Chart(build) => {
                let mut state = build(values);
                state.rebuild_from(bars.input(), 0);
                (0..descriptor.outputs.len())
                    .map(|output| {
                        let computed = state.output(output);
                        let from = state.output_from(output);
                        (0..bars.len())
                            .map(|row| {
                                row.checked_sub(from)
                                    .and_then(|offset| computed.get(offset))
                                    .copied()
                                    .filter(|value| value.is_finite())
                            })
                            .collect()
                    })
                    .collect()
            }
            Calculation::PriorHighest => {
                vec![prior_extreme(&bars.high, values.whole(0), f64::max)]
            }
            Calculation::PriorLowest => vec![prior_extreme(&bars.low, values.whole(0), f64::min)],
        }
    }
}

/// The extreme of the `period` values before each row; absent until `period` rows precede it.
fn prior_extreme(values: &[f64], period: usize, pick: fn(f64, f64) -> f64) -> Vec<Option<f64>> {
    (0..values.len())
        .map(|row| {
            (period > 0 && row >= period)
                .then(|| values[row - period..row].iter().copied().reduce(pick))
                .flatten()
        })
        .collect()
}

/// A validated operand's parameter values, read by position.
#[derive(Clone, Copy, Debug)]
struct Values<'a>(&'a [ScanNumber; MAXIMUM_SCAN_STUDY_PARAMETERS]);

impl Values<'_> {
    fn whole(self, index: usize) -> usize {
        self.0[index]
            .whole()
            .and_then(|whole| usize::try_from(whole).ok())
            .unwrap_or(0)
    }

    fn real(self, index: usize) -> f64 {
        self.0[index].value()
    }

    fn toggle(self, index: usize) -> bool {
        self.whole(index) != 0
    }
}

const fn recursive(period: usize) -> usize {
    period.saturating_mul(RECURSIVE_WARMUP_PERIODS)
}

const fn whole(units: i64) -> ScanNumber {
    ScanNumber { units, scale: 0 }
}

const fn decimal(units: i64, scale: u8) -> ScanNumber {
    ScanNumber { units, scale }
}

const fn period_named(name: ScanStudyParameterName, default: i64) -> ScanStudyParameter {
    ScanStudyParameter {
        name,
        kind: ScanStudyParameterKind::Period,
        default: whole(default),
        minimum: whole(1),
        maximum: whole(MAXIMUM_SCAN_PERIOD as i64),
    }
}

const fn period(default: i64) -> ScanStudyParameter {
    period_named(ScanStudyParameterName::Period, default)
}

const fn decimal_named(
    name: ScanStudyParameterName,
    default: ScanNumber,
    (minimum, maximum): (ScanNumber, ScanNumber),
) -> ScanStudyParameter {
    ScanStudyParameter {
        name,
        kind: ScanStudyParameterKind::Decimal,
        default,
        minimum,
        maximum,
    }
}

/// Standard deviations or ATR multiples of a band, from 0.1 to 10.
const BAND_WIDTH: (ScanNumber, ScanNumber) = (decimal(1, 1), whole(10));

const fn deviation(default: i64) -> ScanStudyParameter {
    decimal_named(
        ScanStudyParameterName::Deviation,
        whole(default),
        BAND_WIDTH,
    )
}

const fn multiplier(default: i64) -> ScanStudyParameter {
    decimal_named(
        ScanStudyParameterName::Multiplier,
        whole(default),
        BAND_WIDTH,
    )
}

const fn line(name: ScanStudyOutputName, unit: ScanValueUnit) -> ScanStudyOutput {
    ScanStudyOutput { name, unit }
}

use ScanStudyFamily as Family;
use ScanStudyOutputName as Output;
use ScanStudyParameterName as Name;
use ScanValueUnit as Unit;

const PRICE: &[ScanStudyOutput] = &[line(Output::Value, Unit::Price)];
const NUMBER: &[ScanStudyOutput] = &[line(Output::Value, Unit::Number)];
const QUANTITY: &[ScanStudyOutput] = &[line(Output::Value, Unit::Quantity)];
const PRICE_BANDS: &[ScanStudyOutput] = &[
    line(Output::Upper, Unit::Price),
    line(Output::Basis, Unit::Price),
    line(Output::Lower, Unit::Price),
];

const fn with_signal(main: ScanStudyOutputName, unit: ScanValueUnit) -> [ScanStudyOutput; 2] {
    [line(main, unit), line(Output::Signal, unit)]
}

/// Indexed by [`ScanStudy`]; `the_table_is_indexed_by_study` checks the order.
static SCAN_STUDIES: [ScanStudyDescriptor; 56] = [
    ScanStudyDescriptor {
        study: ScanStudy::Sma,
        identifier: "sma",
        family: Family::MovingAverages,
        parameters: &[period(20)],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| IncrementalState::sma(values.whole(0))),
        history: |values| values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Ema,
        identifier: "ema",
        family: Family::MovingAverages,
        parameters: &[period(20)],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| IncrementalState::ema(values.whole(0))),
        history: |values| recursive(values.whole(0)),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Wma,
        identifier: "wma",
        family: Family::MovingAverages,
        parameters: &[period(20)],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| IncrementalState::wma(values.whole(0))),
        history: |values| values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Dema,
        identifier: "dema",
        family: Family::MovingAverages,
        parameters: &[period(20)],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| IncrementalState::dema(values.whole(0))),
        history: |values| recursive(values.whole(0)) + values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Tema,
        identifier: "tema",
        family: Family::MovingAverages,
        parameters: &[period(20)],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| IncrementalState::tema(values.whole(0))),
        history: |values| recursive(values.whole(0)) + 2 * values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Smma,
        identifier: "smma",
        family: Family::MovingAverages,
        parameters: &[period(20)],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| IncrementalState::smma(values.whole(0))),
        history: |values| recursive(values.whole(0)),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Hma,
        identifier: "hma",
        family: Family::MovingAverages,
        parameters: &[period(20)],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| IncrementalState::hma(values.whole(0))),
        // A weighted average of `period` bars, smoothed over the square root of `period`.
        history: |values| 2 * values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Vwma,
        identifier: "vwma",
        family: Family::MovingAverages,
        parameters: &[period(20)],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| IncrementalState::vwma(values.whole(0))),
        history: |values| values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Kama,
        identifier: "kama",
        family: Family::MovingAverages,
        parameters: &[
            period(10),
            period_named(Name::Fast, 2),
            period_named(Name::Slow, 30),
        ],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| {
            IncrementalState::kama(values.whole(0), values.whole(1), values.whole(2))
        }),
        history: |values| recursive(values.whole(2)) + values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::McGinley,
        identifier: "mcginley",
        family: Family::MovingAverages,
        parameters: &[period(20)],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| IncrementalState::mcginley(values.whole(0))),
        history: |values| recursive(values.whole(0)),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Alma,
        identifier: "alma",
        family: Family::MovingAverages,
        parameters: &[
            period(20),
            decimal_named(Name::Offset, decimal(85, 2), (whole(0), whole(1))),
            decimal_named(Name::Sigma, whole(6), (decimal(1, 2), whole(100))),
        ],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| {
            IncrementalState::alma(values.whole(0), values.real(1), values.real(2))
        }),
        history: |values| values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::LinearRegression,
        identifier: "linear_regression",
        family: Family::MovingAverages,
        parameters: &[period(20), deviation(2)],
        outputs: &[
            line(Output::Curve, Unit::Price),
            line(Output::Upper, Unit::Price),
            line(Output::Lower, Unit::Price),
        ],
        calculation: Calculation::Chart(|values| {
            IncrementalState::linear_regression(values.whole(0), values.real(1))
        }),
        history: |values| values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Rsi,
        identifier: "rsi",
        family: Family::Oscillators,
        parameters: &[period(14)],
        outputs: NUMBER,
        calculation: Calculation::Chart(|values| IncrementalState::rsi(values.whole(0))),
        history: |values| recursive(values.whole(0)),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Macd,
        identifier: "macd",
        family: Family::Oscillators,
        parameters: &[
            period_named(Name::Fast, 12),
            period_named(Name::Slow, 26),
            period_named(Name::Signal, 9),
        ],
        outputs: &[
            line(Output::Macd, Unit::Price),
            line(Output::Signal, Unit::Price),
            line(Output::Histogram, Unit::Price),
        ],
        calculation: Calculation::Chart(|values| {
            IncrementalState::macd(values.whole(0), values.whole(1), values.whole(2))
        }),
        history: |values| recursive(values.whole(1)) + values.whole(2),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Stochastic,
        identifier: "stochastic",
        family: Family::Oscillators,
        parameters: &[
            period_named(Name::KPeriod, 14),
            period_named(Name::DPeriod, 3),
        ],
        outputs: &[
            line(Output::PercentK, Unit::Number),
            line(Output::PercentD, Unit::Number),
        ],
        calculation: Calculation::Chart(|values| {
            IncrementalState::stochastic(values.whole(0), values.whole(1))
        }),
        history: |values| values.whole(0) + values.whole(1),
    },
    ScanStudyDescriptor {
        study: ScanStudy::StochasticRsi,
        identifier: "stochastic_rsi",
        family: Family::Oscillators,
        parameters: &[
            period_named(Name::RsiPeriod, 14),
            period_named(Name::StochasticPeriod, 14),
        ],
        outputs: NUMBER,
        calculation: Calculation::Chart(|values| {
            IncrementalState::stochastic_rsi(values.whole(0), values.whole(1))
        }),
        history: |values| recursive(values.whole(0)) + values.whole(1),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Cci,
        identifier: "cci",
        family: Family::Oscillators,
        parameters: &[period(20)],
        outputs: NUMBER,
        calculation: Calculation::Chart(|values| IncrementalState::cci(values.whole(0))),
        history: |values| values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::WilliamsR,
        identifier: "williams_r",
        family: Family::Oscillators,
        parameters: &[period(14)],
        outputs: NUMBER,
        calculation: Calculation::Chart(|values| IncrementalState::williams_r(values.whole(0))),
        history: |values| values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Momentum,
        identifier: "momentum",
        family: Family::Oscillators,
        parameters: &[period(10)],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| IncrementalState::momentum(values.whole(0))),
        history: |values| values.whole(0) + 1,
    },
    ScanStudyDescriptor {
        study: ScanStudy::RateOfChange,
        identifier: "roc",
        family: Family::Oscillators,
        parameters: &[period(10)],
        outputs: NUMBER,
        calculation: Calculation::Chart(|values| IncrementalState::rate_of_change(values.whole(0))),
        history: |values| values.whole(0) + 1,
    },
    ScanStudyDescriptor {
        study: ScanStudy::ChandeMomentum,
        identifier: "chande_momentum",
        family: Family::Oscillators,
        parameters: &[period(14)],
        outputs: NUMBER,
        calculation: Calculation::Chart(|values| {
            IncrementalState::chande_momentum(values.whole(0))
        }),
        history: |values| values.whole(0) + 1,
    },
    ScanStudyDescriptor {
        study: ScanStudy::Dpo,
        identifier: "dpo",
        family: Family::Oscillators,
        parameters: &[period(20)],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| IncrementalState::dpo(values.whole(0))),
        history: |values| values.whole(0) + values.whole(0) / 2 + 1,
    },
    ScanStudyDescriptor {
        study: ScanStudy::AwesomeOscillator,
        identifier: "awesome_oscillator",
        family: Family::Oscillators,
        parameters: &[],
        outputs: PRICE,
        calculation: Calculation::Chart(|_| IncrementalState::awesome_oscillator()),
        history: |_| AWESOME_OSCILLATOR_HISTORY_BARS,
    },
    ScanStudyDescriptor {
        study: ScanStudy::Aroon,
        identifier: "aroon",
        family: Family::Oscillators,
        parameters: &[period(25)],
        outputs: &[
            line(Output::AroonUp, Unit::Number),
            line(Output::AroonDown, Unit::Number),
        ],
        calculation: Calculation::Chart(|values| IncrementalState::aroon(values.whole(0))),
        history: |values| values.whole(0) + 1,
    },
    ScanStudyDescriptor {
        study: ScanStudy::Trix,
        identifier: "trix",
        family: Family::Oscillators,
        parameters: &[period(15), period_named(Name::Signal, 9)],
        outputs: &with_signal(Output::Trix, Unit::Number),
        calculation: Calculation::Chart(|values| {
            IncrementalState::trix(values.whole(0), values.whole(1))
        }),
        // A triple EMA, its one-bar change, then the signal.
        history: |values| recursive(values.whole(0)) + 2 * values.whole(0) + values.whole(1),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Kst,
        identifier: "kst",
        family: Family::Oscillators,
        parameters: &[
            period_named(Name::RocPeriod1, 10),
            period_named(Name::RocPeriod2, 15),
            period_named(Name::RocPeriod3, 20),
            period_named(Name::RocPeriod4, 30),
            period_named(Name::Smoothing1, 10),
            period_named(Name::Smoothing2, 10),
            period_named(Name::Smoothing3, 10),
            period_named(Name::Smoothing4, 15),
            period_named(Name::Signal, 9),
        ],
        outputs: &with_signal(Output::Kst, Unit::Number),
        calculation: Calculation::Chart(|values| {
            IncrementalState::kst(
                [0, 1, 2, 3].map(|index| values.whole(index)),
                [4, 5, 6, 7].map(|index| values.whole(index)),
                values.whole(8),
            )
        }),
        history: |values| {
            (0..4)
                .map(|index| values.whole(index) + values.whole(index + 4))
                .max()
                .unwrap_or(0)
                + values.whole(8)
        },
    },
    ScanStudyDescriptor {
        study: ScanStudy::Tsi,
        identifier: "tsi",
        family: Family::Oscillators,
        parameters: &[
            period_named(Name::Long, 25),
            period_named(Name::Short, 13),
            period_named(Name::Signal, 13),
        ],
        outputs: &with_signal(Output::Tsi, Unit::Number),
        calculation: Calculation::Chart(|values| {
            IncrementalState::tsi(values.whole(0), values.whole(1), values.whole(2))
        }),
        history: |values| recursive(values.whole(0)) + values.whole(1) + values.whole(2),
    },
    ScanStudyDescriptor {
        study: ScanStudy::FisherTransform,
        identifier: "fisher_transform",
        family: Family::Oscillators,
        parameters: &[period(10)],
        outputs: &[
            line(Output::Fisher, Unit::Number),
            line(Output::Trigger, Unit::Number),
        ],
        calculation: Calculation::Chart(|values| {
            IncrementalState::fisher_transform(values.whole(0))
        }),
        history: |values| recursive(values.whole(0)),
    },
    ScanStudyDescriptor {
        study: ScanStudy::UltimateOscillator,
        identifier: "ultimate_oscillator",
        family: Family::Oscillators,
        parameters: &[
            period_named(Name::ShortPeriod, 7),
            period_named(Name::MediumPeriod, 14),
            period_named(Name::LongPeriod, 28),
        ],
        outputs: NUMBER,
        calculation: Calculation::Chart(|values| {
            IncrementalState::ultimate_oscillator(values.whole(0), values.whole(1), values.whole(2))
        }),
        history: |values| values.whole(0).max(values.whole(1)).max(values.whole(2)) + 1,
    },
    ScanStudyDescriptor {
        study: ScanStudy::CoppockCurve,
        identifier: "coppock_curve",
        family: Family::Oscillators,
        parameters: &[
            period_named(Name::LongPeriod, 14),
            period_named(Name::ShortPeriod, 11),
            period_named(Name::Smoothing, 10),
        ],
        outputs: NUMBER,
        calculation: Calculation::Chart(|values| {
            IncrementalState::coppock_curve(values.whole(0), values.whole(1), values.whole(2))
        }),
        history: |values| values.whole(0).max(values.whole(1)) + values.whole(2) + 1,
    },
    ScanStudyDescriptor {
        study: ScanStudy::AdxDmi,
        identifier: "adx_dmi",
        family: Family::Trend,
        parameters: &[period(14)],
        outputs: &[
            line(Output::PlusDi, Unit::Number),
            line(Output::MinusDi, Unit::Number),
            line(Output::Adx, Unit::Number),
        ],
        calculation: Calculation::Chart(|values| IncrementalState::adx_dmi(values.whole(0))),
        // ADX smooths a Wilder-smoothed directional index a second time.
        history: |values| 2 * recursive(values.whole(0)),
    },
    ScanStudyDescriptor {
        study: ScanStudy::ParabolicSar,
        identifier: "parabolic_sar",
        family: Family::Trend,
        parameters: &[],
        outputs: PRICE,
        calculation: Calculation::Chart(|_| IncrementalState::parabolic_sar()),
        history: |_| PARABOLIC_SAR_HISTORY_BARS,
    },
    ScanStudyDescriptor {
        study: ScanStudy::SuperTrend,
        identifier: "supertrend",
        family: Family::Trend,
        parameters: &[period(10), multiplier(3)],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| {
            IncrementalState::supertrend(values.whole(0), values.real(1))
        }),
        history: |values| recursive(values.whole(0)),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Ichimoku,
        identifier: "ichimoku",
        family: Family::Trend,
        parameters: &[],
        // Values are aligned to their source bar, as the chart draws them. The lagging line
        // is the close itself, so it is not offered.
        outputs: &[
            line(Output::Conversion, Unit::Price),
            line(Output::Base, Unit::Price),
            line(Output::LeadingA, Unit::Price),
            line(Output::LeadingB, Unit::Price),
        ],
        calculation: Calculation::Chart(|_| IncrementalState::ichimoku()),
        history: |_| ICHIMOKU_HISTORY_BARS,
    },
    ScanStudyDescriptor {
        study: ScanStudy::Vortex,
        identifier: "vortex",
        family: Family::Trend,
        parameters: &[period(14)],
        outputs: &[
            line(Output::PlusVi, Unit::Number),
            line(Output::MinusVi, Unit::Number),
        ],
        calculation: Calculation::Chart(|values| IncrementalState::vortex(values.whole(0))),
        history: |values| values.whole(0) + 1,
    },
    ScanStudyDescriptor {
        study: ScanStudy::Choppiness,
        identifier: "choppiness",
        family: Family::Trend,
        parameters: &[ScanStudyParameter {
            minimum: whole(2),
            ..period(14)
        }],
        outputs: NUMBER,
        calculation: Calculation::Chart(|values| IncrementalState::choppiness(values.whole(0))),
        history: |values| values.whole(0) + 1,
    },
    ScanStudyDescriptor {
        study: ScanStudy::MassIndex,
        identifier: "mass_index",
        family: Family::Trend,
        parameters: &[
            period_named(Name::EmaPeriod, 9),
            period_named(Name::SumPeriod, 25),
        ],
        outputs: NUMBER,
        calculation: Calculation::Chart(|values| {
            IncrementalState::mass_index(values.whole(0), values.whole(1))
        }),
        // A ratio of an EMA of the range to an EMA of that EMA, summed.
        history: |values| recursive(values.whole(0)) + values.whole(0) + values.whole(1),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Atr,
        identifier: "atr",
        family: Family::Volatility,
        parameters: &[period(14)],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| IncrementalState::atr(values.whole(0))),
        history: |values| recursive(values.whole(0)),
    },
    ScanStudyDescriptor {
        study: ScanStudy::StandardDeviation,
        identifier: "standard_deviation",
        family: Family::Volatility,
        parameters: &[period(20)],
        outputs: PRICE,
        calculation: Calculation::Chart(|values| {
            IncrementalState::standard_deviation(values.whole(0))
        }),
        history: |values| values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::HistoricalVolatility,
        identifier: "historical_volatility",
        family: Family::Volatility,
        parameters: &[
            period(20),
            decimal_named(Name::Annualization, whole(252), (whole(1), whole(1_000))),
        ],
        outputs: NUMBER,
        calculation: Calculation::Chart(|values| {
            IncrementalState::historical_volatility(values.whole(0), values.real(1))
        }),
        history: |values| values.whole(0) + 1,
    },
    ScanStudyDescriptor {
        study: ScanStudy::Bollinger,
        identifier: "bollinger",
        family: Family::BandsAndChannels,
        parameters: &[period(20), deviation(2)],
        outputs: PRICE_BANDS,
        calculation: Calculation::Chart(|values| {
            IncrementalState::bollinger(values.whole(0), values.real(1))
        }),
        history: |values| values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::BollingerMetrics,
        identifier: "bollinger_metrics",
        family: Family::BandsAndChannels,
        parameters: &[period(20), deviation(2)],
        outputs: &[
            line(Output::PercentB, Unit::Number),
            line(Output::BandWidth, Unit::Number),
        ],
        calculation: Calculation::Chart(|values| {
            IncrementalState::bollinger_metrics(values.whole(0), values.real(1))
        }),
        history: |values| values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Donchian,
        identifier: "donchian",
        family: Family::BandsAndChannels,
        parameters: &[period(20)],
        outputs: PRICE_BANDS,
        calculation: Calculation::Chart(|values| IncrementalState::donchian(values.whole(0))),
        history: |values| values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Keltner,
        identifier: "keltner",
        family: Family::BandsAndChannels,
        parameters: &[period(20), multiplier(2)],
        outputs: PRICE_BANDS,
        calculation: Calculation::Chart(|values| {
            IncrementalState::keltner(values.whole(0), values.real(1))
        }),
        history: |values| recursive(values.whole(0)),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Envelopes,
        identifier: "envelopes",
        family: Family::BandsAndChannels,
        parameters: &[
            period(20),
            decimal_named(Name::Percent, whole(2), (decimal(1, 1), whole(50))),
            ScanStudyParameter {
                name: Name::Exponential,
                kind: ScanStudyParameterKind::Toggle,
                default: whole(0),
                minimum: whole(0),
                maximum: whole(1),
            },
        ],
        outputs: PRICE_BANDS,
        calculation: Calculation::Chart(|values| {
            IncrementalState::envelopes(values.whole(0), values.real(1), values.toggle(2))
        }),
        history: |values| {
            if values.toggle(2) {
                recursive(values.whole(0))
            } else {
                values.whole(0)
            }
        },
    },
    ScanStudyDescriptor {
        study: ScanStudy::AtrBands,
        identifier: "atr_bands",
        family: Family::BandsAndChannels,
        parameters: &[period(14), multiplier(2)],
        outputs: PRICE_BANDS,
        calculation: Calculation::Chart(|values| {
            IncrementalState::atr_bands(values.whole(0), values.real(1))
        }),
        history: |values| recursive(values.whole(0)),
    },
    ScanStudyDescriptor {
        study: ScanStudy::HighestHigh,
        identifier: "highest_high",
        family: Family::BandsAndChannels,
        parameters: &[period(20)],
        outputs: PRICE,
        calculation: Calculation::PriorHighest,
        history: |values| values.whole(0) + 1,
    },
    ScanStudyDescriptor {
        study: ScanStudy::LowestLow,
        identifier: "lowest_low",
        family: Family::BandsAndChannels,
        parameters: &[period(20)],
        outputs: PRICE,
        calculation: Calculation::PriorLowest,
        history: |values| values.whole(0) + 1,
    },
    ScanStudyDescriptor {
        study: ScanStudy::ChaikinOscillator,
        identifier: "chaikin_oscillator",
        family: Family::Volume,
        parameters: &[period_named(Name::Fast, 3), period_named(Name::Slow, 10)],
        outputs: QUANTITY,
        calculation: Calculation::Chart(|values| {
            IncrementalState::chaikin_oscillator(values.whole(0), values.whole(1))
        }),
        // The difference of two EMAs of accumulation/distribution, where its starting level
        // cancels.
        history: |values| recursive(values.whole(1)),
    },
    ScanStudyDescriptor {
        study: ScanStudy::RelativeVolume,
        identifier: "relative_volume",
        family: Family::Volume,
        parameters: &[period(20)],
        outputs: &[line(Output::Value, Unit::Multiple)],
        calculation: Calculation::Chart(|values| {
            IncrementalState::relative_volume(values.whole(0))
        }),
        history: |values| values.whole(0) + 1,
    },
    ScanStudyDescriptor {
        study: ScanStudy::Klinger,
        identifier: "klinger",
        family: Family::Volume,
        parameters: &[
            period_named(Name::Fast, 34),
            period_named(Name::Slow, 55),
            period_named(Name::Signal, 13),
        ],
        outputs: &with_signal(Output::Klinger, Unit::Quantity),
        calculation: Calculation::Chart(|values| {
            IncrementalState::klinger(values.whole(0), values.whole(1), values.whole(2))
        }),
        history: |values| recursive(values.whole(1)) + values.whole(2),
    },
    ScanStudyDescriptor {
        study: ScanStudy::VolumeOscillator,
        identifier: "volume_oscillator",
        family: Family::Volume,
        parameters: &[
            period_named(Name::Fast, 12),
            period_named(Name::Slow, 26),
            period_named(Name::Signal, 9),
        ],
        outputs: &[
            line(Output::Pvo, Unit::Number),
            line(Output::Signal, Unit::Number),
            line(Output::Histogram, Unit::Number),
        ],
        calculation: Calculation::Chart(|values| {
            IncrementalState::volume_oscillator(values.whole(0), values.whole(1), values.whole(2))
        }),
        history: |values| recursive(values.whole(1)) + values.whole(2),
    },
    ScanStudyDescriptor {
        study: ScanStudy::ElderForce,
        identifier: "elder_force",
        family: Family::Volume,
        parameters: &[period(13)],
        outputs: QUANTITY,
        calculation: Calculation::Chart(|values| IncrementalState::elder_force(values.whole(0))),
        history: |values| recursive(values.whole(0)) + 1,
    },
    ScanStudyDescriptor {
        study: ScanStudy::EaseOfMovement,
        identifier: "ease_of_movement",
        family: Family::Volume,
        parameters: &[
            period(14),
            decimal_named(
                Name::Divisor,
                whole(100_000_000),
                (whole(1), whole(1_000_000_000_000)),
            ),
        ],
        outputs: NUMBER,
        calculation: Calculation::Chart(|values| {
            IncrementalState::ease_of_movement(values.whole(0), values.real(1))
        }),
        history: |values| values.whole(0) + 1,
    },
    ScanStudyDescriptor {
        study: ScanStudy::Cmf,
        identifier: "cmf",
        family: Family::Volume,
        parameters: &[period(20)],
        outputs: NUMBER,
        calculation: Calculation::Chart(|values| IncrementalState::cmf(values.whole(0))),
        history: |values| values.whole(0),
    },
    ScanStudyDescriptor {
        study: ScanStudy::Mfi,
        identifier: "mfi",
        family: Family::Volume,
        parameters: &[period(14)],
        outputs: NUMBER,
        calculation: Calculation::Chart(|values| IncrementalState::mfi(values.whole(0))),
        history: |values| values.whole(0) + 1,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_charts_indicators as indicators;

    /// A deterministic random walk with ranges and volumes, `length` bars long.
    fn fixture(length: usize) -> ScanBars {
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            f64::from(u32::try_from(seed >> 32).expect("high half fits")) / f64::from(u32::MAX)
        };
        let mut bars = ScanBars::default();
        let mut close = 100.0;
        for day in 0..length {
            let open = close;
            close = open * (1.0 + (next() - 0.5) * 0.04);
            let high = open.max(close) * (1.0 + next() * 0.01);
            let low = open.min(close) * (1.0 - next() * 0.01);
            bars.times.push(i64::try_from(day).unwrap() * 86_400);
            bars.open.push(open);
            bars.high.push(high);
            bars.low.push(low);
            bars.close.push(close);
            bars.volume.push(1_000.0 + next() * 4_000.0);
        }
        bars
    }

    fn line<T: Copy>(points: Vec<T>, pick: impl Fn(T) -> Option<f64>) -> Vec<Option<f64>> {
        points.into_iter().map(pick).collect()
    }

    /// The library's batch formula for `study`'s line `output` at the table's defaults, written
    /// out by hand so a wrong parameter order, default or line index in the table fails.
    #[allow(clippy::too_many_lines)]
    fn reference(study: ScanStudy, output: usize, bars: &ScanBars) -> Vec<Option<f64>> {
        let (high, low, close, volume) = (&bars.high, &bars.low, &bars.close, &bars.volume);
        let prior = |values: &[f64], pick: fn(f64, f64) -> f64| {
            (0..values.len())
                .map(|row| {
                    (row >= 20).then(|| values[row - 20..row].iter().copied().reduce(pick))?
                })
                .collect()
        };
        match study {
            ScanStudy::Sma => indicators::sma(close, 20),
            ScanStudy::Ema => indicators::ema(close, 20),
            ScanStudy::Wma => indicators::wma(close, 20),
            ScanStudy::Dema => indicators::dema(close, 20),
            ScanStudy::Tema => indicators::tema(close, 20),
            ScanStudy::Smma => indicators::smma(close, 20),
            ScanStudy::Hma => indicators::hma(close, 20),
            ScanStudy::Vwma => indicators::vwma(close, volume, 20),
            ScanStudy::Kama => indicators::kama(close, 10, 2, 30),
            ScanStudy::McGinley => indicators::mcginley(close, 20),
            ScanStudy::Alma => indicators::alma(close, 20, 0.85, 6.0),
            ScanStudy::LinearRegression => {
                line(indicators::linear_regression(close, 20, 2.0), |point| {
                    [point.curve, point.upper, point.lower][output]
                })
            }
            ScanStudy::Rsi => indicators::rsi(close, 14),
            ScanStudy::Macd => line(indicators::macd(close, 12, 26, 9), |point| {
                [point.macd, point.signal, point.histogram][output]
            }),
            ScanStudy::Stochastic => {
                line(indicators::stochastic(high, low, close, 14, 3), |point| {
                    [point.k, point.d][output]
                })
            }
            ScanStudy::StochasticRsi => indicators::stochastic_rsi(close, 14, 14),
            ScanStudy::Cci => indicators::cci(high, low, close, 20),
            ScanStudy::WilliamsR => indicators::williams_r(high, low, close, 14),
            ScanStudy::Momentum => indicators::momentum(close, 10),
            ScanStudy::RateOfChange => indicators::rate_of_change(close, 10),
            ScanStudy::ChandeMomentum => indicators::chande_momentum(close, 14),
            ScanStudy::Dpo => indicators::dpo(close, 20),
            ScanStudy::AwesomeOscillator => indicators::awesome_oscillator(high, low),
            ScanStudy::Aroon => line(indicators::aroon(high, low, 25), |(up, down)| {
                [up, down][output]
            }),
            ScanStudy::Trix => line(indicators::trix(close, 15, 9), |point| {
                [point.line, point.signal][output]
            }),
            ScanStudy::Kst => line(
                indicators::kst(close, [10, 15, 20, 30], [10, 10, 10, 15], 9),
                |point| [point.line, point.signal][output],
            ),
            ScanStudy::Tsi => line(indicators::tsi(close, 25, 13, 13), |point| {
                [point.line, point.signal][output]
            }),
            ScanStudy::FisherTransform => {
                line(indicators::fisher_transform(high, low, 10), |point| {
                    [point.line, point.trigger][output]
                })
            }
            ScanStudy::UltimateOscillator => {
                indicators::ultimate_oscillator(high, low, close, 7, 14, 28)
            }
            ScanStudy::CoppockCurve => indicators::coppock_curve(close, 14, 11, 10),
            ScanStudy::AdxDmi => line(indicators::adx_dmi(high, low, close, 14), |point| {
                [point.plus_di, point.minus_di, point.adx][output]
            }),
            ScanStudy::ParabolicSar => indicators::parabolic_sar(high, low),
            ScanStudy::SuperTrend => indicators::supertrend(high, low, close, 10, 3.0),
            ScanStudy::Ichimoku => line(indicators::ichimoku(high, low, close), |point| {
                [
                    point.conversion,
                    point.base,
                    point.leading_a,
                    point.leading_b,
                ][output]
            }),
            ScanStudy::Vortex => line(indicators::vortex(high, low, close, 14), |point| {
                [point.plus, point.minus][output]
            }),
            ScanStudy::Choppiness => indicators::choppiness(high, low, close, 14),
            ScanStudy::MassIndex => indicators::mass_index(high, low, 9, 25),
            ScanStudy::Atr => indicators::atr(high, low, close, 14),
            ScanStudy::StandardDeviation => indicators::standard_deviation(close, 20),
            ScanStudy::HistoricalVolatility => indicators::historical_volatility(close, 20, 252.0),
            ScanStudy::Bollinger => line(indicators::bollinger(close, 20, 2.0), |point| {
                [point.upper, point.middle, point.lower][output]
            }),
            ScanStudy::BollingerMetrics => line(
                indicators::bollinger_metrics(close, 20, 2.0),
                |(percent_b, bandwidth)| [percent_b, bandwidth][output],
            ),
            ScanStudy::Donchian => line(indicators::donchian(high, low, 20), |point| {
                [point.upper, point.middle, point.lower][output]
            }),
            ScanStudy::Keltner => line(indicators::keltner(high, low, close, 20, 2.0), |point| {
                [point.upper, point.middle, point.lower][output]
            }),
            ScanStudy::Envelopes => line(
                indicators::envelopes(close, 20, 2.0, false),
                |(upper, basis, lower)| [upper, basis, lower][output],
            ),
            ScanStudy::AtrBands => {
                line(indicators::atr_bands(high, low, close, 14, 2.0), |point| {
                    [point.upper, point.basis, point.lower][output]
                })
            }
            ScanStudy::HighestHigh => prior(high, f64::max),
            ScanStudy::LowestLow => prior(low, f64::min),
            ScanStudy::ChaikinOscillator => {
                indicators::chaikin_oscillator(high, low, close, volume, 3, 10)
            }
            ScanStudy::RelativeVolume => indicators::relative_volume(volume, 20),
            ScanStudy::Klinger => line(
                indicators::klinger(high, low, close, volume, 34, 55, 13),
                |point| [point.line, point.signal][output],
            ),
            ScanStudy::VolumeOscillator => {
                line(indicators::volume_oscillator(volume, 12, 26, 9), |point| {
                    [point.line, point.signal, point.histogram][output]
                })
            }
            ScanStudy::ElderForce => indicators::elder_force(close, volume, 13),
            ScanStudy::EaseOfMovement => {
                indicators::ease_of_movement(high, low, volume, 14, 100_000_000.0)
            }
            ScanStudy::Cmf => indicators::cmf(high, low, close, volume, 20),
            ScanStudy::Mfi => indicators::mfi(high, low, close, volume, 14),
        }
    }

    fn close_to(left: f64, right: f64, relative: f64) -> bool {
        (left - right).abs() <= relative * left.abs().max(right.abs()).max(1.0)
    }

    #[test]
    fn the_table_is_indexed_by_study() {
        let mut identifiers = std::collections::HashSet::new();
        for (index, descriptor) in scan_studies().iter().enumerate() {
            assert_eq!(
                descriptor.study as usize, index,
                "{}",
                descriptor.identifier
            );
            assert_eq!(
                descriptor.study.descriptor().identifier,
                descriptor.identifier
            );
            assert!(identifiers.insert(descriptor.identifier));
            assert_eq!(
                ScanStudy::from_identifier(descriptor.identifier),
                Some(descriptor.study)
            );
            assert!(descriptor.parameters.len() <= MAXIMUM_SCAN_STUDY_PARAMETERS);
            assert_ne!(descriptor.outputs.len(), 0);
            let names = descriptor
                .parameters
                .iter()
                .map(|parameter| parameter.name.identifier())
                .collect::<std::collections::HashSet<_>>();
            assert_eq!(names.len(), descriptor.parameters.len());
            let lines = descriptor
                .outputs
                .iter()
                .map(|output| output.name)
                .collect::<std::collections::HashSet<_>>();
            assert_eq!(lines.len(), descriptor.outputs.len());
            for parameter in descriptor.parameters {
                assert!(
                    parameter.admits(parameter.default),
                    "{}",
                    descriptor.identifier
                );
                assert!(parameter.admits(parameter.minimum));
                assert!(parameter.admits(parameter.maximum));
            }
            for output in 0..descriptor.outputs.len() {
                let operand = ScanStudyOperand {
                    output,
                    ..ScanStudyOperand::new(descriptor.study)
                };
                assert_eq!(operand.validate(), Ok(()), "{}", descriptor.identifier);
            }
        }
        assert_eq!(ScanStudy::from_identifier("obv"), None);
        for family in ScanStudyFamily::ALL {
            assert!(scan_studies().iter().any(|study| study.family == family));
        }
    }

    #[test]
    fn every_chart_line_is_offered_except_the_lagging_close() {
        for descriptor in scan_studies() {
            let Calculation::Chart(build) = descriptor.calculation else {
                continue;
            };
            let operand = ScanStudyOperand::new(descriptor.study);
            let drawn = build(Values(&operand.parameters)).output_count();
            let dropped = usize::from(descriptor.study == ScanStudy::Ichimoku);
            assert_eq!(
                descriptor.outputs.len() + dropped,
                drawn,
                "{}",
                descriptor.identifier
            );
        }
    }

    #[test]
    fn every_line_equals_the_library_formula_on_the_same_bars() {
        let bars = fixture(400);
        for descriptor in scan_studies() {
            for output in 0..descriptor.outputs.len() {
                let operand = ScanStudyOperand {
                    output,
                    ..ScanStudyOperand::new(descriptor.study)
                };
                let scanned = operand.series(&bars).swap_remove(output);
                let expected = reference(descriptor.study, output, &bars);
                assert_eq!(scanned.len(), expected.len());
                assert!(
                    expected.last().copied().flatten().is_some(),
                    "{} line {output} settles within the fixture",
                    descriptor.identifier
                );
                for (row, (scanned, expected)) in scanned.iter().zip(&expected).enumerate() {
                    match (scanned, expected) {
                        (Some(scanned), Some(expected)) => assert!(
                            close_to(*scanned, *expected, 1e-9),
                            "{} line {output} row {row}: {scanned} against {expected}",
                            descriptor.identifier
                        ),
                        (None, None) => {}
                        _ => panic!(
                            "{} line {output} row {row}: {scanned:?} against {expected:?}",
                            descriptor.identifier
                        ),
                    }
                }
            }
        }
    }

    #[test]
    fn the_history_each_study_requests_settles_its_current_value() {
        let long = fixture(3_000);
        for descriptor in scan_studies() {
            let operand = ScanStudyOperand::new(descriptor.study);
            let history = operand.history_bars() + 1;
            let mut recent = ScanBars::default();
            let start = long.len() - history;
            recent.times = long.times[start..].to_vec();
            recent.open = long.open[start..].to_vec();
            recent.high = long.high[start..].to_vec();
            recent.low = long.low[start..].to_vec();
            recent.close = long.close[start..].to_vec();
            recent.volume = long.volume[start..].to_vec();
            for (output, (full, short)) in operand
                .series(&long)
                .into_iter()
                .zip(operand.series(&recent))
                .enumerate()
            {
                let full = full.last().copied().flatten().expect("long history value");
                let short = short.last().copied().flatten().unwrap_or_else(|| {
                    panic!("{} line {output} has no value", descriptor.identifier)
                });
                assert!(
                    close_to(full, short, 1e-4),
                    "{} line {output}: {short} from {history} bars against {full}",
                    descriptor.identifier
                );
            }
        }
    }
}
