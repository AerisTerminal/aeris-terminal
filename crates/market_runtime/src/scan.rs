//! Provider-neutral market scans.
//!
//! A scan is a bounded list of conditions on one timeframe, evaluated for every listed
//! instrument of one provider over that instrument's most recent provider bars, the still
//! forming bar last, exactly as a chart shows them. Study values come from the same Aeris
//! Charts indicator state the chart builds (`studies`), so a scan and a chart agree on every
//! value over the same bars. Evaluation is pure: providers fetch bars, this module decides
//! matches.

mod studies;

use aeris_charts_indicators::IndicatorInput;
use aeris_market_data::{ChartInterval, MarketBar};

use crate::ProviderInstrumentSummary;

pub use studies::{
    MAXIMUM_SCAN_STUDY_PARAMETERS, ScanStudy, ScanStudyDescriptor, ScanStudyFamily,
    ScanStudyOperand, ScanStudyOutput, ScanStudyOutputName, ScanStudyParameter,
    ScanStudyParameterKind, ScanStudyParameterName, ScanValueUnit, scan_studies,
};

/// Most conditions one scan definition holds.
pub const MAXIMUM_SCAN_CONDITIONS: usize = 8;
/// Largest study period a condition may name.
pub const MAXIMUM_SCAN_PERIOD: u16 = 250;
/// Most matches one scan publication carries.
pub const MAXIMUM_SCAN_RESULTS: u32 = 500;
/// Most instruments one scan covers, most traded first.
pub const MAXIMUM_SCAN_UNIVERSE: u32 = 1_024;
/// Largest decimal scale of a user-entered scan number.
pub const MAXIMUM_SCAN_NUMBER_SCALE: u8 = 9;
/// Most bars a scan requests for one instrument.
pub const MAXIMUM_SCAN_HISTORY_BARS: usize = 3_000;
/// Fewest bars a scan requests, so short-period scans still see recent structure.
const MINIMUM_SCAN_HISTORY_BARS: usize = 100;

/// Whether every condition or any one condition must hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ScanMatchMode {
    #[default]
    All,
    Any,
}

/// One price of a bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ScanPriceField {
    Open,
    High,
    Low,
    #[default]
    Close,
}

/// A user-entered constant, exactly `units / 10^scale`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ScanNumber {
    pub units: i64,
    pub scale: u8,
}

impl ScanNumber {
    #[allow(clippy::cast_precision_loss)]
    fn value(self) -> f64 {
        self.units as f64 / 10_f64.powi(i32::from(self.scale))
    }

    /// The number as a whole number, when it is one.
    fn whole(self) -> Option<i64> {
        let divisor = 10_i64.checked_pow(u32::from(self.scale))?;
        (self.units % divisor == 0).then_some(self.units / divisor)
    }
}

impl From<i64> for ScanNumber {
    fn from(units: i64) -> Self {
        Self { units, scale: 0 }
    }
}

/// One side of a condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanOperand {
    Price(ScanPriceField),
    /// Bar volume in the instrument's quantity unit.
    Volume,
    /// Percent change of the close from the previous bar's close.
    ChangePercent,
    Study(ScanStudyOperand),
    Number(ScanNumber),
}

impl ScanOperand {
    fn history_bars(self) -> usize {
        match self {
            Self::Price(_) | Self::Volume | Self::Number(_) => 1,
            Self::ChangePercent => 2,
            Self::Study(study) => study.history_bars(),
        }
    }
}

/// How a condition compares its operands on the current bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ScanComparison {
    #[default]
    Above,
    Below,
    /// At or below on the previous bar and above on the current bar.
    CrossesAbove,
    /// At or above on the previous bar and below on the current bar.
    CrossesBelow,
    /// Within `right` (lower) and `upper`, inclusive.
    Between,
}

/// One scan condition, such as close above SMA(20).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScanCondition {
    pub left: ScanOperand,
    pub comparison: ScanComparison,
    /// The compared value, or the lower bound for [`ScanComparison::Between`].
    pub right: ScanOperand,
    /// Upper bound for [`ScanComparison::Between`]; absent for every other comparison.
    pub upper: Option<ScanOperand>,
}

/// A complete scan: conditions over one chart interval.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ScanDefinition {
    /// The bars every condition reads; a provider refuses an interval it does not offer.
    pub timeframe: ChartInterval,
    pub match_mode: ScanMatchMode,
    pub conditions: Vec<ScanCondition>,
}

impl Default for ScanDefinition {
    fn default() -> Self {
        Self {
            timeframe: ChartInterval::Day1,
            match_mode: ScanMatchMode::default(),
            conditions: Vec::new(),
        }
    }
}

/// Why a scan definition was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanDefinitionError {
    NoConditions,
    TooManyConditions,
    /// A study parameter is outside its descriptor's bounds, or set where the study has none.
    ParameterOutOfRange,
    /// A study with fast and slow periods needs the fast one shorter.
    FastPeriodNotBelowSlow,
    /// A study operand reads a line its study does not draw.
    OutputOutOfRange,
    /// A number's scale is above [`MAXIMUM_SCAN_NUMBER_SCALE`].
    NumberOutOfRange,
    /// The left side is a constant, which never varies by instrument.
    ConstantLeftOperand,
    /// `Between` needs an upper bound; other comparisons must not carry one.
    UpperBoundMismatch,
}

impl std::fmt::Display for ScanDefinitionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NoConditions => "a scan needs at least one condition",
            Self::TooManyConditions => "a scan has too many conditions",
            Self::ParameterOutOfRange => "a scan study parameter is out of range",
            Self::FastPeriodNotBelowSlow => {
                "a scan study's fast period must be below its slow period"
            }
            Self::OutputOutOfRange => "a scan study has no such line",
            Self::NumberOutOfRange => "a scan number is out of range",
            Self::ConstantLeftOperand => "a scan condition cannot start with a number",
            Self::UpperBoundMismatch => "a scan range condition needs exactly one upper bound",
        })
    }
}

impl std::error::Error for ScanDefinitionError {}

impl ScanDefinition {
    /// Checks every bound a provider relies on before fetching bars.
    ///
    /// # Errors
    ///
    /// Returns the first violated bound.
    pub fn validate(&self) -> Result<(), ScanDefinitionError> {
        if self.conditions.is_empty() {
            return Err(ScanDefinitionError::NoConditions);
        }
        if self.conditions.len() > MAXIMUM_SCAN_CONDITIONS {
            return Err(ScanDefinitionError::TooManyConditions);
        }
        for condition in &self.conditions {
            if matches!(condition.left, ScanOperand::Number(_)) {
                return Err(ScanDefinitionError::ConstantLeftOperand);
            }
            if (condition.comparison == ScanComparison::Between) != condition.upper.is_some() {
                return Err(ScanDefinitionError::UpperBoundMismatch);
            }
            for operand in condition.operands() {
                validate_operand(operand)?;
            }
        }
        Ok(())
    }

    /// Bars one instrument needs so every condition sees a settled current value.
    #[must_use]
    pub fn history_bars(&self) -> usize {
        let deepest = self
            .conditions
            .iter()
            .flat_map(ScanCondition::operands)
            .map(ScanOperand::history_bars)
            .max()
            .unwrap_or(1);
        // One more bar so a crossing can read the previous bar's values.
        (deepest + 1).clamp(MINIMUM_SCAN_HISTORY_BARS, MAXIMUM_SCAN_HISTORY_BARS)
    }
}

impl ScanCondition {
    fn operands(&self) -> impl Iterator<Item = ScanOperand> {
        [Some(self.left), Some(self.right), self.upper]
            .into_iter()
            .flatten()
    }
}

fn validate_operand(operand: ScanOperand) -> Result<(), ScanDefinitionError> {
    match operand {
        ScanOperand::Number(number) if number.scale > MAXIMUM_SCAN_NUMBER_SCALE => {
            Err(ScanDefinitionError::NumberOutOfRange)
        }
        ScanOperand::Study(study) => study.validate(),
        ScanOperand::Price(_)
        | ScanOperand::Volume
        | ScanOperand::ChangePercent
        | ScanOperand::Number(_) => Ok(()),
    }
}

/// Requests one bounded scan across a provider's listed instruments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanProviderMarkets {
    pub consumer_id: u64,
    pub scan_generation: u64,
    pub provider: String,
    pub definition: ScanDefinition,
    /// Instruments covered, most traded first, at most [`MAXIMUM_SCAN_UNIVERSE`].
    pub universe_limit: u32,
    pub maximum_results: u32,
}

/// One condition's operand values on the current bar; absent until an indicator has warmed up.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScanConditionValues {
    pub left: Option<f64>,
    /// The compared value, or the lower bound for [`ScanComparison::Between`].
    pub right: Option<f64>,
}

/// One instrument that satisfies a scan, with its current bar's exact close and volume.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderMarketScanMatch {
    /// Selection identity and presentation metadata, identical to a search result.
    pub instrument: ProviderInstrumentSummary,
    /// Decimal scale for `close`.
    pub price_scale: u32,
    /// Fractional digits the evaluated bars' prices use, at most `price_scale`.
    pub price_decimals: u32,
    /// Close of the current (possibly still forming) bar.
    pub close: i64,
    /// Decimal scale for `volume`.
    pub quantity_scale: u32,
    /// Volume of the current bar.
    pub volume: i64,
    /// Percent change of the close from the previous bar's close.
    pub change_percent: Option<f64>,
    /// Each condition's values on the current bar, in definition order.
    pub values: Vec<ScanConditionValues>,
    /// Open time of the current bar.
    pub bar_open_unix_millis: i64,
}

/// How far a scan has progressed through the provider's universe.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanProgress {
    /// Instruments whose bars were evaluated.
    pub evaluated: u32,
    /// Instruments the scan covers.
    pub universe: u32,
    /// Instruments whose bars could not be fetched this pass.
    pub unavailable: u32,
}

/// One bounded, progressively published scan answer. Each publication replaces the previous
/// one for the same scan generation; `complete` marks the end of a full pass.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderMarketScan {
    pub consumer_id: u64,
    pub provider: String,
    pub provider_generation: u64,
    pub scan_generation: u64,
    pub progress: ScanProgress,
    pub complete: bool,
    /// Wall-clock time of the newest bars evaluated.
    pub captured_at_unix_millis: i64,
    /// Matches ordered by percent change, largest first, bounded to the request.
    pub matches: Vec<ProviderMarketScanMatch>,
}

/// One instrument's bars, oldest first, converted to the chart's values.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ScanBars {
    /// Open times in Unix seconds.
    times: Vec<i64>,
    open: Vec<f64>,
    high: Vec<f64>,
    low: Vec<f64>,
    close: Vec<f64>,
    volume: Vec<f64>,
}

impl ScanBars {
    /// Converts fixed-point bars the way the chart does: each value divided by its scale.
    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn from_market_bars(
        bars: &[MarketBar],
        price_scale: u8,
        quantity_scale: u8,
    ) -> Self {
        let price = 10_f64.powi(i32::from(price_scale));
        let quantity = 10_f64.powi(i32::from(quantity_scale));
        let column = |value: fn(&MarketBar) -> i64, divisor: f64| {
            bars.iter()
                .map(|bar| value(bar) as f64 / divisor)
                .collect::<Vec<_>>()
        };
        Self {
            times: bars
                .iter()
                .map(|bar| bar.exchange_timestamp_seconds)
                .collect(),
            open: column(|bar| bar.open, price),
            high: column(|bar| bar.high, price),
            low: column(|bar| bar.low, price),
            close: column(|bar| bar.close, price),
            volume: column(|bar| bar.volume, quantity),
        }
    }

    fn len(&self) -> usize {
        self.close.len()
    }

    fn input(&self) -> IndicatorInput<'_> {
        IndicatorInput {
            times: &self.times,
            open: &self.open,
            high: &self.high,
            low: &self.low,
            close: &self.close,
            volume: &self.volume,
        }
    }
}

/// Result of one instrument's evaluation.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ScanOutcome {
    pub(crate) matched: bool,
    pub(crate) values: Vec<ScanConditionValues>,
    pub(crate) change_percent: Option<f64>,
}

/// Evaluates a validated definition on the current (last) bar of `bars`.
pub(crate) fn evaluate(definition: &ScanDefinition, bars: &ScanBars) -> ScanOutcome {
    let mut series = OperandSeries::new(bars);
    let mut held = Vec::with_capacity(definition.conditions.len());
    let mut values = Vec::with_capacity(definition.conditions.len());
    for condition in &definition.conditions {
        values.push(ScanConditionValues {
            left: series.current(condition.left),
            right: series.current(condition.right),
        });
        held.push(condition_holds(condition, &mut series));
    }
    let matched = match definition.match_mode {
        ScanMatchMode::All => !held.is_empty() && held.iter().all(|held| *held),
        ScanMatchMode::Any => held.iter().any(|held| *held),
    };
    ScanOutcome {
        matched,
        values,
        change_percent: series.current(ScanOperand::ChangePercent),
    }
}

/// Fractional digits the bars' prices use: the scale less the trailing zeros all of them share.
pub(crate) fn price_decimals(bars: &[MarketBar], price_scale: u32) -> u32 {
    let mut decimals = 0;
    for price in bars
        .iter()
        .flat_map(|bar| [bar.open, bar.high, bar.low, bar.close])
    {
        let (mut used, mut rest) = (price_scale, price);
        while used > decimals && rest % 10 == 0 {
            rest /= 10;
            used -= 1;
        }
        decimals = used.max(decimals);
        if decimals == price_scale {
            break;
        }
    }
    decimals
}

fn condition_holds(condition: &ScanCondition, series: &mut OperandSeries<'_>) -> bool {
    let (Some(left), Some(right)) = (
        series.current(condition.left),
        series.current(condition.right),
    ) else {
        return false;
    };
    match condition.comparison {
        ScanComparison::Above => left > right,
        ScanComparison::Below => left < right,
        ScanComparison::Between => condition
            .upper
            .and_then(|upper| series.current(upper))
            .is_some_and(|upper| right <= left && left <= upper),
        ScanComparison::CrossesAbove | ScanComparison::CrossesBelow => {
            let (Some(previous_left), Some(previous_right)) = (
                series.previous(condition.left),
                series.previous(condition.right),
            ) else {
                return false;
            };
            if condition.comparison == ScanComparison::CrossesAbove {
                previous_left <= previous_right && left > right
            } else {
                previous_left >= previous_right && left < right
            }
        }
    }
}

/// Computes each distinct operand's series once per instrument; every line of a study is
/// kept from its one computation.
struct OperandSeries<'a> {
    bars: &'a ScanBars,
    computed: Vec<(ScanOperand, Vec<Option<f64>>)>,
}

impl<'a> OperandSeries<'a> {
    fn new(bars: &'a ScanBars) -> Self {
        Self {
            bars,
            computed: Vec::new(),
        }
    }

    fn current(&mut self, operand: ScanOperand) -> Option<f64> {
        let length = self.bars.len();
        self.at(operand, length.checked_sub(1)?)
    }

    fn previous(&mut self, operand: ScanOperand) -> Option<f64> {
        let length = self.bars.len();
        self.at(operand, length.checked_sub(2)?)
    }

    fn at(&mut self, operand: ScanOperand, row: usize) -> Option<f64> {
        if let ScanOperand::Number(number) = operand {
            return Some(number.value());
        }
        let known = self
            .computed
            .iter()
            .position(|(known, _)| *known == operand);
        let index = if let Some(index) = known {
            index
        } else {
            self.computed.extend(operand_lines(operand, self.bars));
            self.computed
                .iter()
                .rposition(|(line, _)| *line == operand)?
        };
        self.computed[index]
            .1
            .get(row)
            .copied()
            .flatten()
            .filter(|value| value.is_finite())
    }
}

/// The series `operand` reads, beside every other line of the same study.
fn operand_lines(operand: ScanOperand, bars: &ScanBars) -> Vec<(ScanOperand, Vec<Option<f64>>)> {
    let series = match operand {
        ScanOperand::Price(field) => {
            let column = match field {
                ScanPriceField::Open => &bars.open,
                ScanPriceField::High => &bars.high,
                ScanPriceField::Low => &bars.low,
                ScanPriceField::Close => &bars.close,
            };
            column.iter().copied().map(Some).collect()
        }
        ScanOperand::Volume => bars.volume.iter().copied().map(Some).collect(),
        ScanOperand::ChangePercent => std::iter::once(None)
            .chain(
                bars.close
                    .windows(2)
                    .map(|pair| (pair[0] > 0.0).then(|| (pair[1] / pair[0] - 1.0) * 100.0)),
            )
            .take(bars.len())
            .collect(),
        ScanOperand::Study(study) => {
            return study
                .series(bars)
                .into_iter()
                .enumerate()
                .map(|(output, series)| {
                    (
                        ScanOperand::Study(ScanStudyOperand { output, ..study }),
                        series,
                    )
                })
                .collect();
        }
        ScanOperand::Number(number) => vec![Some(number.value()); bars.len()],
    };
    vec![(operand, series)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_charts_indicators as indicators;

    fn bars(closes: &[f64]) -> ScanBars {
        ScanBars {
            times: (0_i64..)
                .take(closes.len())
                .map(|day| day * 86_400)
                .collect(),
            open: closes.to_vec(),
            high: closes.iter().map(|close| close + 1.0).collect(),
            low: closes.iter().map(|close| close - 1.0).collect(),
            close: closes.to_vec(),
            volume: vec![10.0; closes.len()],
        }
    }

    /// `kind` with its leading parameters set to whole `values`, reading line `output`.
    fn study(kind: ScanStudy, values: &[i64], output: usize) -> ScanOperand {
        let mut operand = ScanStudyOperand::new(kind);
        for (slot, value) in operand.parameters.iter_mut().zip(values) {
            *slot = ScanNumber {
                units: *value,
                scale: 0,
            };
        }
        operand.output = output;
        ScanOperand::Study(operand)
    }

    fn number(value: i64) -> ScanOperand {
        ScanOperand::Number(ScanNumber {
            units: value,
            scale: 0,
        })
    }

    fn condition(
        left: ScanOperand,
        comparison: ScanComparison,
        right: ScanOperand,
    ) -> ScanCondition {
        ScanCondition {
            left,
            comparison,
            right,
            upper: None,
        }
    }

    fn definition(conditions: Vec<ScanCondition>) -> ScanDefinition {
        ScanDefinition {
            timeframe: ChartInterval::Day1,
            match_mode: ScanMatchMode::All,
            conditions,
        }
    }

    const CLOSE: ScanOperand = ScanOperand::Price(ScanPriceField::Close);

    #[test]
    fn close_above_its_moving_average_uses_the_chart_formula() {
        let rising = (1..=30).map(f64::from).collect::<Vec<_>>();
        let sma = study(ScanStudy::Sma, &[20], 0);
        let scan = definition(vec![condition(CLOSE, ScanComparison::Above, sma)]);
        let outcome = evaluate(&scan, &bars(&rising));
        assert!(outcome.matched);
        let expected = indicators::sma(&rising, 20)[29];
        assert_eq!(
            outcome.values,
            vec![ScanConditionValues {
                left: Some(30.0),
                right: expected,
            }],
            "the compared average is published beside the close"
        );
        let rising_bars = bars(&rising);
        let mut series = OperandSeries::new(&rising_bars);
        assert_eq!(series.current(sma), expected);

        let falling = rising.iter().rev().copied().collect::<Vec<_>>();
        assert!(!evaluate(&scan, &bars(&falling)).matched);
    }

    #[test]
    fn crossings_need_the_previous_bar_on_the_other_side() {
        let level = number(10);
        let crossed = definition(vec![condition(CLOSE, ScanComparison::CrossesAbove, level)]);
        assert!(evaluate(&crossed, &bars(&[8.0, 9.0, 11.0])).matched);
        assert!(!evaluate(&crossed, &bars(&[8.0, 11.0, 12.0])).matched);
        assert!(!evaluate(&crossed, &bars(&[11.0])).matched);
        let below = definition(vec![condition(CLOSE, ScanComparison::CrossesBelow, level)]);
        assert!(evaluate(&below, &bars(&[12.0, 10.0, 9.0])).matched);
    }

    #[test]
    fn between_is_inclusive_and_any_matches_one_condition() {
        let mut range = condition(CLOSE, ScanComparison::Between, number(5));
        range.upper = Some(number(10));
        assert!(evaluate(&definition(vec![range]), &bars(&[10.0])).matched);
        assert!(!evaluate(&definition(vec![range]), &bars(&[10.5])).matched);

        let mut either = definition(vec![
            condition(CLOSE, ScanComparison::Above, number(100)),
            condition(CLOSE, ScanComparison::Below, number(3)),
        ]);
        assert!(!evaluate(&either, &bars(&[2.0])).matched);
        either.match_mode = ScanMatchMode::Any;
        assert!(evaluate(&either, &bars(&[2.0])).matched);
    }

    #[test]
    fn missing_warm_up_never_matches() {
        let sma = study(ScanStudy::Sma, &[20], 0);
        let scan = definition(vec![condition(CLOSE, ScanComparison::Above, sma)]);
        let outcome = evaluate(&scan, &bars(&[1.0, 2.0, 3.0]));
        assert!(!outcome.matched);
        assert_eq!(
            evaluate(&scan, &ScanBars::default()).values,
            vec![ScanConditionValues::default()]
        );
    }

    #[test]
    fn breakouts_compare_against_prior_bars_only() {
        let highest = study(ScanStudy::HighestHigh, &[3], 0);
        let scan = definition(vec![condition(CLOSE, ScanComparison::Above, highest)]);
        // Prior highs are close + 1: 6, 7, 8. A close of 9 breaks out; the current high is ignored.
        assert!(evaluate(&scan, &bars(&[5.0, 6.0, 7.0, 9.0])).matched);
        assert!(!evaluate(&scan, &bars(&[5.0, 6.0, 7.0, 7.5])).matched);
    }

    #[test]
    fn change_percent_compares_with_the_previous_close() {
        let scan = definition(vec![condition(
            ScanOperand::ChangePercent,
            ScanComparison::Above,
            ScanOperand::Number(ScanNumber {
                units: 25,
                scale: 1,
            }),
        )]);
        let outcome = evaluate(&scan, &bars(&[100.0, 103.0]));
        assert!(outcome.matched);
        assert!((outcome.change_percent.unwrap() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn fixed_point_bars_convert_like_the_chart() {
        let bar = MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 0,
            exchange_timestamp_unix_nanos: 0,
            open: 12_345,
            high: 12_400,
            low: 12_300,
            close: 12_350,
            volume: 2_500,
        };
        let converted = ScanBars::from_market_bars(&[bar], 2, 3);
        assert_eq!(converted.close, vec![123.5]);
        assert_eq!(converted.volume, vec![2.5]);
    }

    #[test]
    fn price_decimals_follow_the_finest_bar_price() {
        let bar = |close| MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 0,
            exchange_timestamp_unix_nanos: 0,
            open: 11_000_000,
            high: 13_000_000,
            low: 10_000_000,
            close,
            volume: 0,
        };
        assert_eq!(price_decimals(&[bar(12_000_000)], 8), 2, "0.12 at scale 8");
        assert_eq!(price_decimals(&[bar(12_000_000), bar(12_345_600)], 8), 6);
        assert_eq!(price_decimals(&[bar(1)], 8), 8);
        assert_eq!(price_decimals(&[], 8), 0);
        let whole = MarketBar {
            open: 0,
            high: 0,
            low: 0,
            ..bar(9_700_000_000_000)
        };
        assert_eq!(price_decimals(&[whole], 8), 0, "a whole-number price");
    }

    #[test]
    fn validation_refuses_every_unbounded_shape() {
        let sma = |period| study(ScanStudy::Sma, &[period], 0);
        assert_eq!(
            definition(Vec::new()).validate(),
            Err(ScanDefinitionError::NoConditions)
        );
        assert_eq!(
            definition(vec![condition(CLOSE, ScanComparison::Above, sma(0))]).validate(),
            Err(ScanDefinitionError::ParameterOutOfRange)
        );
        assert_eq!(
            definition(vec![condition(CLOSE, ScanComparison::Above, sma(251))]).validate(),
            Err(ScanDefinitionError::ParameterOutOfRange)
        );
        assert_eq!(
            definition(vec![condition(number(1), ScanComparison::Above, CLOSE)]).validate(),
            Err(ScanDefinitionError::ConstantLeftOperand)
        );
        assert_eq!(
            definition(vec![condition(CLOSE, ScanComparison::Between, number(1))]).validate(),
            Err(ScanDefinitionError::UpperBoundMismatch)
        );
        let macd = study(ScanStudy::Macd, &[26, 12, 9], 0);
        assert_eq!(
            definition(vec![condition(macd, ScanComparison::Above, number(0))]).validate(),
            Err(ScanDefinitionError::FastPeriodNotBelowSlow)
        );
        let missing_line = study(ScanStudy::Macd, &[12, 26, 9], 3);
        assert_eq!(
            definition(vec![condition(
                missing_line,
                ScanComparison::Above,
                number(0)
            )])
            .validate(),
            Err(ScanDefinitionError::OutputOutOfRange)
        );
        let mut stray = ScanStudyOperand::new(ScanStudy::Rsi);
        stray.parameters[1] = ScanNumber { units: 3, scale: 0 };
        assert_eq!(
            definition(vec![condition(
                ScanOperand::Study(stray),
                ScanComparison::Above,
                number(0)
            )])
            .validate(),
            Err(ScanDefinitionError::ParameterOutOfRange),
            "a value past the study's parameters"
        );
        let mut fractional = ScanStudyOperand::new(ScanStudy::Sma);
        fractional.parameters[0] = ScanNumber {
            units: 205,
            scale: 1,
        };
        assert_eq!(
            definition(vec![condition(
                ScanOperand::Study(fractional),
                ScanComparison::Above,
                number(0)
            )])
            .validate(),
            Err(ScanDefinitionError::ParameterOutOfRange),
            "a period must be whole"
        );
        let mut wide = ScanStudyOperand::new(ScanStudy::Bollinger);
        wide.parameters[1] = ScanNumber {
            units: 105,
            scale: 1,
        };
        assert_eq!(
            definition(vec![condition(
                CLOSE,
                ScanComparison::Above,
                ScanOperand::Study(wide)
            )])
            .validate(),
            Err(ScanDefinitionError::ParameterOutOfRange),
            "a deviation above 10"
        );
        let too_many = vec![condition(CLOSE, ScanComparison::Above, number(1)); 9];
        assert_eq!(
            definition(too_many).validate(),
            Err(ScanDefinitionError::TooManyConditions)
        );
        assert_eq!(
            definition(vec![condition(CLOSE, ScanComparison::Above, sma(20))]).validate(),
            Ok(())
        );
    }

    #[test]
    fn history_covers_the_deepest_indicator_within_bounds() {
        let ema = study(ScanStudy::Ema, &[50], 0);
        assert_eq!(
            definition(vec![condition(CLOSE, ScanComparison::Above, ema)]).history_bars(),
            501
        );
        assert_eq!(
            definition(vec![condition(CLOSE, ScanComparison::Above, number(1))]).history_bars(),
            MINIMUM_SCAN_HISTORY_BARS
        );
        let adx = study(ScanStudy::AdxDmi, &[250], 2);
        assert_eq!(
            definition(vec![condition(adx, ScanComparison::Above, number(25))]).history_bars(),
            MAXIMUM_SCAN_HISTORY_BARS
        );
    }
}
