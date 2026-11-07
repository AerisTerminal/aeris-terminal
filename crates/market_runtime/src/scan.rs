//! Provider-neutral market scans.
//!
//! A scan is a bounded list of conditions on one timeframe, evaluated for every listed
//! instrument of one provider over that instrument's most recent provider bars, the still
//! forming bar last, exactly as a chart shows them. Indicator values come from the same
//! Aeris Charts indicator functions the chart uses, so a scan and a chart agree on every value
//! over the same bars. Evaluation is pure: providers fetch bars, this module decides matches.

use aeris_charts_indicators as indicators;
use aeris_market_data::{ChartInterval, MarketBar};

use crate::ProviderInstrumentSummary;

/// Most conditions one scan definition holds.
pub const MAXIMUM_SCAN_CONDITIONS: usize = 8;
/// Largest indicator period a condition may name.
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
/// Recursive indicators (EMA, RSI, MACD, ATR, ADX) depend on how much history seeds them.
/// This many periods of warm-up bring their current value within display precision of a
/// chart that loaded longer history.
const RECURSIVE_WARMUP_PERIODS: usize = 10;
/// Largest Bollinger deviation, in tenths.
const MAXIMUM_DEVIATION_TENTHS: u16 = 100;

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

/// One line of a MACD study.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ScanMacdLine {
    #[default]
    Macd,
    Signal,
    Histogram,
}

/// One band of a Bollinger study.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ScanBand {
    Upper,
    #[default]
    Middle,
    Lower,
}

/// An indicator computed over the bar closes, highs, lows or volumes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanIndicator {
    Sma {
        period: u16,
    },
    Ema {
        period: u16,
    },
    Rsi {
        period: u16,
    },
    Macd {
        fast: u16,
        slow: u16,
        signal: u16,
        line: ScanMacdLine,
    },
    Bollinger {
        period: u16,
        /// Band width in standard deviations, in tenths (20 is 2.0).
        deviation_tenths: u16,
        band: ScanBand,
    },
    Atr {
        period: u16,
    },
    Adx {
        period: u16,
    },
    /// Current volume divided by the mean volume of the previous `period` bars.
    RelativeVolume {
        period: u16,
    },
    /// Highest high of the `period` bars before the current bar.
    HighestHigh {
        period: u16,
    },
    /// Lowest low of the `period` bars before the current bar.
    LowestLow {
        period: u16,
    },
}

impl ScanIndicator {
    fn periods(self) -> [u16; 3] {
        match self {
            Self::Macd {
                fast, slow, signal, ..
            } => [fast, slow, signal],
            Self::Sma { period }
            | Self::Ema { period }
            | Self::Rsi { period }
            | Self::Bollinger { period, .. }
            | Self::Atr { period }
            | Self::Adx { period }
            | Self::RelativeVolume { period }
            | Self::HighestHigh { period }
            | Self::LowestLow { period } => [period, period, period],
        }
    }

    fn history_bars(self) -> usize {
        let recursive = |period: u16| usize::from(period) * RECURSIVE_WARMUP_PERIODS;
        match self {
            Self::Sma { period } | Self::Bollinger { period, .. } => usize::from(period),
            Self::Ema { period } | Self::Rsi { period } | Self::Atr { period } => recursive(period),
            // ADX smooths a Wilder-smoothed directional index a second time.
            Self::Adx { period } => recursive(period) * 2,
            Self::Macd { slow, signal, .. } => recursive(slow) + usize::from(signal),
            Self::RelativeVolume { period }
            | Self::HighestHigh { period }
            | Self::LowestLow { period } => usize::from(period) + 1,
        }
    }
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
}

/// One side of a condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanOperand {
    Price(ScanPriceField),
    /// Bar volume in the instrument's quantity unit.
    Volume,
    /// Percent change of the close from the previous bar's close.
    ChangePercent,
    Indicator(ScanIndicator),
    Number(ScanNumber),
}

impl ScanOperand {
    fn history_bars(self) -> usize {
        match self {
            Self::Price(_) | Self::Volume | Self::Number(_) => 1,
            Self::ChangePercent => 2,
            Self::Indicator(indicator) => indicator.history_bars(),
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
    /// A period is zero or above [`MAXIMUM_SCAN_PERIOD`].
    PeriodOutOfRange,
    /// MACD needs a fast period shorter than its slow period.
    MacdPeriodsOutOfOrder,
    /// A Bollinger deviation is zero or too wide.
    DeviationOutOfRange,
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
            Self::PeriodOutOfRange => "a scan indicator period is out of range",
            Self::MacdPeriodsOutOfOrder => "a scan MACD fast period must be below its slow period",
            Self::DeviationOutOfRange => "a scan Bollinger deviation is out of range",
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
        ScanOperand::Indicator(indicator) => {
            if indicator
                .periods()
                .iter()
                .any(|period| *period == 0 || *period > MAXIMUM_SCAN_PERIOD)
            {
                return Err(ScanDefinitionError::PeriodOutOfRange);
            }
            match indicator {
                ScanIndicator::Macd { fast, slow, .. } if fast >= slow => {
                    Err(ScanDefinitionError::MacdPeriodsOutOfOrder)
                }
                ScanIndicator::Bollinger {
                    deviation_tenths, ..
                } if deviation_tenths == 0 || deviation_tenths > MAXIMUM_DEVIATION_TENTHS => {
                    Err(ScanDefinitionError::DeviationOutOfRange)
                }
                _ => Ok(()),
            }
        }
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

/// Computes each distinct operand's series once per instrument.
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
            self.computed
                .push((operand, operand_series(operand, self.bars)));
            self.computed.len() - 1
        };
        self.computed[index]
            .1
            .get(row)
            .copied()
            .flatten()
            .filter(|value| value.is_finite())
    }
}

fn operand_series(operand: ScanOperand, bars: &ScanBars) -> Vec<Option<f64>> {
    match operand {
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
        ScanOperand::Indicator(indicator) => indicator_series(indicator, bars),
        ScanOperand::Number(number) => vec![Some(number.value()); bars.len()],
    }
}

fn indicator_series(indicator: ScanIndicator, bars: &ScanBars) -> Vec<Option<f64>> {
    let period = |period: u16| usize::from(period);
    match indicator {
        ScanIndicator::Sma { period: p } => indicators::sma(&bars.close, period(p)),
        ScanIndicator::Ema { period: p } => indicators::ema(&bars.close, period(p)),
        ScanIndicator::Rsi { period: p } => indicators::rsi(&bars.close, period(p)),
        ScanIndicator::Macd {
            fast,
            slow,
            signal,
            line,
        } => indicators::macd(&bars.close, period(fast), period(slow), period(signal))
            .into_iter()
            .map(|point| match line {
                ScanMacdLine::Macd => point.macd,
                ScanMacdLine::Signal => point.signal,
                ScanMacdLine::Histogram => point.histogram,
            })
            .collect(),
        ScanIndicator::Bollinger {
            period: p,
            deviation_tenths,
            band,
        } => indicators::bollinger(&bars.close, period(p), f64::from(deviation_tenths) / 10.0)
            .into_iter()
            .map(|point| match band {
                ScanBand::Upper => point.upper,
                ScanBand::Middle => point.middle,
                ScanBand::Lower => point.lower,
            })
            .collect(),
        ScanIndicator::Atr { period: p } => {
            indicators::atr(&bars.high, &bars.low, &bars.close, period(p))
        }
        ScanIndicator::Adx { period: p } => {
            indicators::adx_dmi(&bars.high, &bars.low, &bars.close, period(p))
                .into_iter()
                .map(|point| point.adx)
                .collect()
        }
        ScanIndicator::RelativeVolume { period: p } => {
            indicators::relative_volume(&bars.volume, period(p))
        }
        ScanIndicator::HighestHigh { period: p } => prior_extreme(&bars.high, period(p), f64::max),
        ScanIndicator::LowestLow { period: p } => prior_extreme(&bars.low, period(p), f64::min),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(closes: &[f64]) -> ScanBars {
        ScanBars {
            open: closes.to_vec(),
            high: closes.iter().map(|close| close + 1.0).collect(),
            low: closes.iter().map(|close| close - 1.0).collect(),
            close: closes.to_vec(),
            volume: vec![10.0; closes.len()],
        }
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
        let sma = ScanOperand::Indicator(ScanIndicator::Sma { period: 20 });
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
        let sma = ScanOperand::Indicator(ScanIndicator::Sma { period: 20 });
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
        let highest = ScanOperand::Indicator(ScanIndicator::HighestHigh { period: 3 });
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
        let sma = |period| ScanOperand::Indicator(ScanIndicator::Sma { period });
        assert_eq!(
            definition(Vec::new()).validate(),
            Err(ScanDefinitionError::NoConditions)
        );
        assert_eq!(
            definition(vec![condition(CLOSE, ScanComparison::Above, sma(0))]).validate(),
            Err(ScanDefinitionError::PeriodOutOfRange)
        );
        assert_eq!(
            definition(vec![condition(CLOSE, ScanComparison::Above, sma(251))]).validate(),
            Err(ScanDefinitionError::PeriodOutOfRange)
        );
        assert_eq!(
            definition(vec![condition(number(1), ScanComparison::Above, CLOSE)]).validate(),
            Err(ScanDefinitionError::ConstantLeftOperand)
        );
        assert_eq!(
            definition(vec![condition(CLOSE, ScanComparison::Between, number(1))]).validate(),
            Err(ScanDefinitionError::UpperBoundMismatch)
        );
        let macd = ScanOperand::Indicator(ScanIndicator::Macd {
            fast: 26,
            slow: 12,
            signal: 9,
            line: ScanMacdLine::Macd,
        });
        assert_eq!(
            definition(vec![condition(macd, ScanComparison::Above, number(0))]).validate(),
            Err(ScanDefinitionError::MacdPeriodsOutOfOrder)
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
        let ema = ScanOperand::Indicator(ScanIndicator::Ema { period: 50 });
        assert_eq!(
            definition(vec![condition(CLOSE, ScanComparison::Above, ema)]).history_bars(),
            501
        );
        assert_eq!(
            definition(vec![condition(CLOSE, ScanComparison::Above, number(1))]).history_bars(),
            MINIMUM_SCAN_HISTORY_BARS
        );
        let adx = ScanOperand::Indicator(ScanIndicator::Adx { period: 250 });
        assert_eq!(
            definition(vec![condition(adx, ScanComparison::Above, number(25))]).history_bars(),
            MAXIMUM_SCAN_HISTORY_BARS
        );
    }
}
