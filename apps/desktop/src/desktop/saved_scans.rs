//! Saved screener scans: the user's bounded scan library as the workspace document stores it,
//! and its exact conversion to and from the runtime scan definition. A saved scan that does
//! not convert, or whose definition fails validation, is dropped alone and never fails the
//! workspace.

use std::collections::BTreeSet;

use aeris_contracts::{
    WorkspaceScanBand, WorkspaceScanComparison, WorkspaceScanConditionState, WorkspaceScanMacdLine,
    WorkspaceScanMatchMode, WorkspaceScanOperandKind, WorkspaceScanOperandState,
    WorkspaceScanTimeframe, WorkspaceScreenerScanState,
};
use aeris_market_runtime::{
    MAXIMUM_SCAN_UNIVERSE, ScanBand, ScanComparison, ScanCondition, ScanDefinition, ScanIndicator,
    ScanMacdLine, ScanMatchMode, ScanNumber, ScanOperand, ScanPriceField, ScanTimeframe,
};

pub(super) const MAXIMUM_SAVED_SCANS: usize = 64;
pub(super) const MAXIMUM_SAVED_SCAN_NAME_BYTES: usize = 64;

/// One saved scan with a validated definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SavedScan {
    pub(super) name: String,
    pub(super) definition: ScanDefinition,
    pub(super) universe_limit: u32,
}

/// The identity two saved scan names share when they differ only in case or outer spaces.
pub(super) fn saved_scan_name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Whether `name` can name a saved scan once trimmed.
pub(super) fn valid_saved_scan_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty() && name.len() <= MAXIMUM_SAVED_SCAN_NAME_BYTES
}

impl SavedScan {
    pub(super) fn from_state(state: &WorkspaceScreenerScanState) -> Option<Self> {
        if !valid_saved_scan_name(&state.name)
            || !(1..=MAXIMUM_SCAN_UNIVERSE).contains(&state.universe_limit)
        {
            return None;
        }
        let definition = ScanDefinition {
            timeframe: timeframe(WorkspaceScanTimeframe::try_from(state.timeframe).ok()?)?,
            match_mode: match_mode(WorkspaceScanMatchMode::try_from(state.match_mode).ok()?)?,
            conditions: state
                .conditions
                .iter()
                .map(condition)
                .collect::<Option<Vec<_>>>()?,
        };
        definition.validate().ok()?;
        Some(Self {
            name: state.name.trim().to_string(),
            definition,
            universe_limit: state.universe_limit,
        })
    }

    pub(super) fn to_state(&self) -> WorkspaceScreenerScanState {
        WorkspaceScreenerScanState {
            name: self.name.clone(),
            timeframe: timeframe_state(self.definition.timeframe) as i32,
            match_mode: match_mode_state(self.definition.match_mode) as i32,
            universe_limit: self.universe_limit,
            conditions: self
                .definition
                .conditions
                .iter()
                .map(condition_state)
                .collect(),
        }
    }
}

/// The saved scans a document holds, in order: invalid scans and later duplicate names are
/// dropped and the library is bounded.
pub(super) fn sanitized_saved_scans(states: &[WorkspaceScreenerScanState]) -> Vec<SavedScan> {
    let mut names = BTreeSet::new();
    states
        .iter()
        .filter_map(SavedScan::from_state)
        .filter(|scan| names.insert(saved_scan_name_key(&scan.name)))
        .take(MAXIMUM_SAVED_SCANS)
        .collect()
}

/// The document's saved scans rewritten in their canonical form, so a field its kind does
/// not read is never carried forward.
pub(super) fn sanitize_saved_scans(
    states: &[WorkspaceScreenerScanState],
) -> Vec<WorkspaceScreenerScanState> {
    sanitized_saved_scans(states)
        .iter()
        .map(SavedScan::to_state)
        .collect()
}

const fn timeframe(state: WorkspaceScanTimeframe) -> Option<ScanTimeframe> {
    Some(match state {
        WorkspaceScanTimeframe::Unspecified => return None,
        WorkspaceScanTimeframe::Minutes15 => ScanTimeframe::Minutes15,
        WorkspaceScanTimeframe::Hour1 => ScanTimeframe::Hour1,
        WorkspaceScanTimeframe::Hour4 => ScanTimeframe::Hour4,
        WorkspaceScanTimeframe::Day1 => ScanTimeframe::Day1,
        WorkspaceScanTimeframe::Week1 => ScanTimeframe::Week1,
    })
}

const fn timeframe_state(timeframe: ScanTimeframe) -> WorkspaceScanTimeframe {
    match timeframe {
        ScanTimeframe::Minutes15 => WorkspaceScanTimeframe::Minutes15,
        ScanTimeframe::Hour1 => WorkspaceScanTimeframe::Hour1,
        ScanTimeframe::Hour4 => WorkspaceScanTimeframe::Hour4,
        ScanTimeframe::Day1 => WorkspaceScanTimeframe::Day1,
        ScanTimeframe::Week1 => WorkspaceScanTimeframe::Week1,
    }
}

const fn match_mode(state: WorkspaceScanMatchMode) -> Option<ScanMatchMode> {
    match state {
        WorkspaceScanMatchMode::Unspecified => None,
        WorkspaceScanMatchMode::All => Some(ScanMatchMode::All),
        WorkspaceScanMatchMode::Any => Some(ScanMatchMode::Any),
    }
}

const fn match_mode_state(mode: ScanMatchMode) -> WorkspaceScanMatchMode {
    match mode {
        ScanMatchMode::All => WorkspaceScanMatchMode::All,
        ScanMatchMode::Any => WorkspaceScanMatchMode::Any,
    }
}

const fn comparison(state: WorkspaceScanComparison) -> Option<ScanComparison> {
    Some(match state {
        WorkspaceScanComparison::Unspecified => return None,
        WorkspaceScanComparison::Above => ScanComparison::Above,
        WorkspaceScanComparison::Below => ScanComparison::Below,
        WorkspaceScanComparison::CrossesAbove => ScanComparison::CrossesAbove,
        WorkspaceScanComparison::CrossesBelow => ScanComparison::CrossesBelow,
        WorkspaceScanComparison::Between => ScanComparison::Between,
    })
}

const fn comparison_state(comparison: ScanComparison) -> WorkspaceScanComparison {
    match comparison {
        ScanComparison::Above => WorkspaceScanComparison::Above,
        ScanComparison::Below => WorkspaceScanComparison::Below,
        ScanComparison::CrossesAbove => WorkspaceScanComparison::CrossesAbove,
        ScanComparison::CrossesBelow => WorkspaceScanComparison::CrossesBelow,
        ScanComparison::Between => WorkspaceScanComparison::Between,
    }
}

fn condition(state: &WorkspaceScanConditionState) -> Option<ScanCondition> {
    Some(ScanCondition {
        left: operand(state.left.as_ref()?)?,
        comparison: comparison(WorkspaceScanComparison::try_from(state.comparison).ok()?)?,
        right: operand(state.right.as_ref()?)?,
        upper: match state.upper.as_ref() {
            Some(upper) => Some(operand(upper)?),
            None => None,
        },
    })
}

fn condition_state(condition: &ScanCondition) -> WorkspaceScanConditionState {
    WorkspaceScanConditionState {
        left: Some(operand_state(condition.left)),
        comparison: comparison_state(condition.comparison) as i32,
        right: Some(operand_state(condition.right)),
        upper: condition.upper.map(operand_state),
    }
}

fn operand(state: &WorkspaceScanOperandState) -> Option<ScanOperand> {
    let period = || u16::try_from(state.period).ok();
    Some(match WorkspaceScanOperandKind::try_from(state.kind).ok()? {
        WorkspaceScanOperandKind::Unspecified => return None,
        WorkspaceScanOperandKind::Open => ScanOperand::Price(ScanPriceField::Open),
        WorkspaceScanOperandKind::High => ScanOperand::Price(ScanPriceField::High),
        WorkspaceScanOperandKind::Low => ScanOperand::Price(ScanPriceField::Low),
        WorkspaceScanOperandKind::Close => ScanOperand::Price(ScanPriceField::Close),
        WorkspaceScanOperandKind::Volume => ScanOperand::Volume,
        WorkspaceScanOperandKind::ChangePercent => ScanOperand::ChangePercent,
        WorkspaceScanOperandKind::Number => ScanOperand::Number(ScanNumber {
            units: state.number_units,
            scale: u8::try_from(state.number_scale).ok()?,
        }),
        WorkspaceScanOperandKind::Sma => indicator(ScanIndicator::Sma { period: period()? }),
        WorkspaceScanOperandKind::Ema => indicator(ScanIndicator::Ema { period: period()? }),
        WorkspaceScanOperandKind::Rsi => indicator(ScanIndicator::Rsi { period: period()? }),
        WorkspaceScanOperandKind::Atr => indicator(ScanIndicator::Atr { period: period()? }),
        WorkspaceScanOperandKind::Adx => indicator(ScanIndicator::Adx { period: period()? }),
        WorkspaceScanOperandKind::RelativeVolume => {
            indicator(ScanIndicator::RelativeVolume { period: period()? })
        }
        WorkspaceScanOperandKind::HighestHigh => {
            indicator(ScanIndicator::HighestHigh { period: period()? })
        }
        WorkspaceScanOperandKind::LowestLow => {
            indicator(ScanIndicator::LowestLow { period: period()? })
        }
        WorkspaceScanOperandKind::Macd => indicator(ScanIndicator::Macd {
            fast: period()?,
            slow: u16::try_from(state.second_period).ok()?,
            signal: u16::try_from(state.third_period).ok()?,
            line: match WorkspaceScanMacdLine::try_from(state.macd_line).ok()? {
                WorkspaceScanMacdLine::Unspecified => return None,
                WorkspaceScanMacdLine::Macd => ScanMacdLine::Macd,
                WorkspaceScanMacdLine::Signal => ScanMacdLine::Signal,
                WorkspaceScanMacdLine::Histogram => ScanMacdLine::Histogram,
            },
        }),
        WorkspaceScanOperandKind::Bollinger => indicator(ScanIndicator::Bollinger {
            period: period()?,
            deviation_tenths: u16::try_from(state.deviation_tenths).ok()?,
            band: match WorkspaceScanBand::try_from(state.band).ok()? {
                WorkspaceScanBand::Unspecified => return None,
                WorkspaceScanBand::Upper => ScanBand::Upper,
                WorkspaceScanBand::Middle => ScanBand::Middle,
                WorkspaceScanBand::Lower => ScanBand::Lower,
            },
        }),
    })
}

const fn indicator(indicator: ScanIndicator) -> ScanOperand {
    ScanOperand::Indicator(indicator)
}

fn operand_state(operand: ScanOperand) -> WorkspaceScanOperandState {
    let kind = |kind: WorkspaceScanOperandKind| WorkspaceScanOperandState {
        kind: kind as i32,
        ..WorkspaceScanOperandState::default()
    };
    let with_period =
        |operand_kind: WorkspaceScanOperandKind, period: u16| WorkspaceScanOperandState {
            period: u32::from(period),
            ..kind(operand_kind)
        };
    match operand {
        ScanOperand::Price(ScanPriceField::Open) => kind(WorkspaceScanOperandKind::Open),
        ScanOperand::Price(ScanPriceField::High) => kind(WorkspaceScanOperandKind::High),
        ScanOperand::Price(ScanPriceField::Low) => kind(WorkspaceScanOperandKind::Low),
        ScanOperand::Price(ScanPriceField::Close) => kind(WorkspaceScanOperandKind::Close),
        ScanOperand::Volume => kind(WorkspaceScanOperandKind::Volume),
        ScanOperand::ChangePercent => kind(WorkspaceScanOperandKind::ChangePercent),
        ScanOperand::Number(number) => WorkspaceScanOperandState {
            number_units: number.units,
            number_scale: u32::from(number.scale),
            ..kind(WorkspaceScanOperandKind::Number)
        },
        ScanOperand::Indicator(indicator) => match indicator {
            ScanIndicator::Sma { period } => with_period(WorkspaceScanOperandKind::Sma, period),
            ScanIndicator::Ema { period } => with_period(WorkspaceScanOperandKind::Ema, period),
            ScanIndicator::Rsi { period } => with_period(WorkspaceScanOperandKind::Rsi, period),
            ScanIndicator::Atr { period } => with_period(WorkspaceScanOperandKind::Atr, period),
            ScanIndicator::Adx { period } => with_period(WorkspaceScanOperandKind::Adx, period),
            ScanIndicator::RelativeVolume { period } => {
                with_period(WorkspaceScanOperandKind::RelativeVolume, period)
            }
            ScanIndicator::HighestHigh { period } => {
                with_period(WorkspaceScanOperandKind::HighestHigh, period)
            }
            ScanIndicator::LowestLow { period } => {
                with_period(WorkspaceScanOperandKind::LowestLow, period)
            }
            ScanIndicator::Macd {
                fast,
                slow,
                signal,
                line,
            } => WorkspaceScanOperandState {
                second_period: u32::from(slow),
                third_period: u32::from(signal),
                macd_line: match line {
                    ScanMacdLine::Macd => WorkspaceScanMacdLine::Macd,
                    ScanMacdLine::Signal => WorkspaceScanMacdLine::Signal,
                    ScanMacdLine::Histogram => WorkspaceScanMacdLine::Histogram,
                } as i32,
                ..with_period(WorkspaceScanOperandKind::Macd, fast)
            },
            ScanIndicator::Bollinger {
                period,
                deviation_tenths,
                band,
            } => WorkspaceScanOperandState {
                deviation_tenths: u32::from(deviation_tenths),
                band: match band {
                    ScanBand::Upper => WorkspaceScanBand::Upper,
                    ScanBand::Middle => WorkspaceScanBand::Middle,
                    ScanBand::Lower => WorkspaceScanBand::Lower,
                } as i32,
                ..with_period(WorkspaceScanOperandKind::Bollinger, period)
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLOSE: ScanOperand = ScanOperand::Price(ScanPriceField::Close);

    fn number(units: i64, scale: u8) -> ScanOperand {
        ScanOperand::Number(ScanNumber { units, scale })
    }

    fn above(left: ScanOperand, right: ScanOperand) -> ScanCondition {
        ScanCondition {
            left,
            comparison: ScanComparison::Above,
            right,
            upper: None,
        }
    }

    fn scan(name: &str, conditions: Vec<ScanCondition>) -> SavedScan {
        SavedScan {
            name: name.to_string(),
            definition: ScanDefinition {
                timeframe: ScanTimeframe::Hour4,
                match_mode: ScanMatchMode::Any,
                conditions,
            },
            universe_limit: 200,
        }
    }

    fn every_operand() -> Vec<ScanOperand> {
        let mut operands = vec![
            ScanOperand::Price(ScanPriceField::Open),
            ScanOperand::Price(ScanPriceField::High),
            ScanOperand::Price(ScanPriceField::Low),
            CLOSE,
            ScanOperand::Volume,
            ScanOperand::ChangePercent,
        ];
        operands.extend(
            [
                ScanIndicator::Sma { period: 50 },
                ScanIndicator::Ema { period: 21 },
                ScanIndicator::Rsi { period: 14 },
                ScanIndicator::Atr { period: 10 },
                ScanIndicator::Adx { period: 14 },
                ScanIndicator::RelativeVolume { period: 20 },
                ScanIndicator::HighestHigh { period: 55 },
                ScanIndicator::LowestLow { period: 30 },
            ]
            .map(ScanOperand::Indicator),
        );
        for line in [
            ScanMacdLine::Macd,
            ScanMacdLine::Signal,
            ScanMacdLine::Histogram,
        ] {
            operands.push(ScanOperand::Indicator(ScanIndicator::Macd {
                fast: 12,
                slow: 26,
                signal: 9,
                line,
            }));
        }
        for band in [ScanBand::Upper, ScanBand::Middle, ScanBand::Lower] {
            operands.push(ScanOperand::Indicator(ScanIndicator::Bollinger {
                period: 20,
                deviation_tenths: 25,
                band,
            }));
        }
        operands
    }

    #[test]
    fn every_operand_comparison_and_timeframe_round_trips_exactly() {
        let operands = every_operand();
        for chunk in operands.chunks(8) {
            let conditions = chunk
                .iter()
                .map(|left| above(*left, number(-1_234_567, 3)))
                .collect();
            let saved = scan("Every operand", conditions);
            assert_eq!(SavedScan::from_state(&saved.to_state()), Some(saved));
        }
        let comparisons = [
            ScanComparison::Above,
            ScanComparison::Below,
            ScanComparison::CrossesAbove,
            ScanComparison::CrossesBelow,
        ]
        .map(|comparison| ScanCondition {
            comparison,
            ..above(
                CLOSE,
                ScanOperand::Indicator(ScanIndicator::Sma { period: 20 }),
            )
        });
        let between = ScanCondition {
            comparison: ScanComparison::Between,
            upper: Some(number(70, 0)),
            ..above(
                ScanOperand::Indicator(ScanIndicator::Rsi { period: 14 }),
                number(30, 0),
            )
        };
        let mut saved = scan(
            "Comparisons",
            comparisons.into_iter().chain([between]).collect(),
        );
        for timeframe in ScanTimeframe::ALL {
            saved.definition.timeframe = timeframe;
            for mode in [ScanMatchMode::All, ScanMatchMode::Any] {
                saved.definition.match_mode = mode;
                assert_eq!(
                    SavedScan::from_state(&saved.to_state()).as_ref(),
                    Some(&saved)
                );
            }
        }
    }

    #[test]
    fn an_unknown_or_missing_value_makes_only_that_scan_invalid() {
        let valid = scan("Valid", vec![above(CLOSE, number(1, 0))]).to_state();
        let mut unknown_timeframe = valid.clone();
        unknown_timeframe.timeframe = 99;
        let mut unspecified_mode = valid.clone();
        unspecified_mode.match_mode = WorkspaceScanMatchMode::Unspecified as i32;
        let mut unknown_operand = valid.clone();
        unknown_operand.conditions[0].left = Some(WorkspaceScanOperandState {
            kind: 400,
            ..WorkspaceScanOperandState::default()
        });
        let mut missing_right = valid.clone();
        missing_right.conditions[0].right = None;
        let mut unknown_line = scan(
            "Macd",
            vec![above(
                ScanOperand::Indicator(ScanIndicator::Macd {
                    fast: 12,
                    slow: 26,
                    signal: 9,
                    line: ScanMacdLine::Macd,
                }),
                number(0, 0),
            )],
        )
        .to_state();
        unknown_line.conditions[0]
            .left
            .as_mut()
            .expect("left")
            .macd_line = 7;
        for invalid in [
            unknown_timeframe,
            unspecified_mode,
            unknown_operand,
            missing_right,
            unknown_line,
        ] {
            assert_eq!(SavedScan::from_state(&invalid), None);
        }
        assert!(SavedScan::from_state(&valid).is_some());
    }

    #[test]
    fn a_definition_failing_validation_is_invalid() {
        let zero_period = scan(
            "Zero",
            vec![above(
                CLOSE,
                ScanOperand::Indicator(ScanIndicator::Sma { period: 0 }),
            )],
        );
        let constant_left = scan("Constant", vec![above(number(1, 0), CLOSE)]);
        let no_conditions = scan("Empty", Vec::new());
        let wide_number = scan("Wide", vec![above(CLOSE, number(1, 12))]);
        for invalid in [zero_period, constant_left, no_conditions, wide_number] {
            assert_eq!(SavedScan::from_state(&invalid.to_state()), None);
        }
        let mut huge_period = scan("Huge", vec![above(CLOSE, number(1, 0))]).to_state();
        huge_period.conditions[0].right = Some(WorkspaceScanOperandState {
            kind: WorkspaceScanOperandKind::Sma as i32,
            period: 70_000,
            ..WorkspaceScanOperandState::default()
        });
        assert_eq!(SavedScan::from_state(&huge_period), None);
    }

    #[test]
    fn the_sanitizer_bounds_names_universes_duplicates_and_count() {
        let condition = vec![above(CLOSE, number(1, 0))];
        let mut states = vec![
            scan("  Breakout  ", condition.clone()).to_state(),
            scan("BREAKOUT", condition.clone()).to_state(),
            scan("   ", condition.clone()).to_state(),
            scan(
                &"x".repeat(MAXIMUM_SAVED_SCAN_NAME_BYTES + 1),
                condition.clone(),
            )
            .to_state(),
        ];
        let mut no_universe = scan("No universe", condition.clone()).to_state();
        no_universe.universe_limit = 0;
        let mut wide_universe = scan("Wide universe", condition.clone()).to_state();
        wide_universe.universe_limit = MAXIMUM_SCAN_UNIVERSE + 1;
        states.extend([no_universe, wide_universe]);
        states.extend(
            (0..MAXIMUM_SAVED_SCANS + 5)
                .map(|index| scan(&format!("Scan {index}"), condition.clone()).to_state()),
        );
        let sanitized = sanitize_saved_scans(&states);
        assert_eq!(sanitized.len(), MAXIMUM_SAVED_SCANS);
        assert_eq!(
            sanitized[0].name, "Breakout",
            "the first duplicate is kept, trimmed"
        );
        assert_eq!(sanitized[1].name, "Scan 0");
        assert!(
            sanitized
                .iter()
                .all(|state| SavedScan::from_state(state).is_some())
        );
    }

    #[test]
    fn the_canonical_form_drops_fields_its_kind_does_not_read() {
        let mut state = scan("Close", vec![above(CLOSE, number(5, 1))]).to_state();
        let left = state.conditions[0].left.as_mut().expect("left");
        left.period = 20;
        left.band = WorkspaceScanBand::Upper as i32;
        let sanitized = sanitize_saved_scans(&[state]);
        assert_eq!(
            sanitized[0].conditions[0].left,
            Some(WorkspaceScanOperandState {
                kind: WorkspaceScanOperandKind::Close as i32,
                ..WorkspaceScanOperandState::default()
            })
        );
    }
}
