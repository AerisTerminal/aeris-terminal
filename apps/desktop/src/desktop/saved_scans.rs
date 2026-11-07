//! Saved screener scans: the user's bounded scan library as the workspace document stores it,
//! and its exact conversion to and from the runtime scan definition. A saved scan that does
//! not convert, or whose definition fails validation, is dropped alone and never fails the
//! workspace.

use std::collections::BTreeSet;

use aeris_contracts::{
    WorkspaceScanComparison, WorkspaceScanConditionState, WorkspaceScanMatchMode,
    WorkspaceScanOperandKind, WorkspaceScanOperandState, WorkspaceScanStudyParameterState,
    WorkspaceScreenerScanState,
};
use aeris_market_runtime::{
    MAXIMUM_SCAN_STUDY_PARAMETERS, MAXIMUM_SCAN_UNIVERSE, ScanComparison, ScanCondition,
    ScanDefinition, ScanMatchMode, ScanNumber, ScanOperand, ScanPriceField, ScanStudy,
    ScanStudyOperand, persisted_interval,
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
            timeframe: persisted_interval(&state.interval)?,
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
            interval: self.definition.timeframe.label().to_string(),
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
        WorkspaceScanOperandKind::Study => ScanOperand::Study(study_operand(state)?),
    })
}

/// A stored study by its identifiers. A parameter not stored keeps its default; an unknown
/// study, line or parameter, or a parameter stored twice, makes the operand unreadable rather
/// than silently reading something else.
fn study_operand(state: &WorkspaceScanOperandState) -> Option<ScanStudyOperand> {
    let study = ScanStudy::from_identifier(&state.study)?;
    let descriptor = study.descriptor();
    let mut operand = ScanStudyOperand::new(study);
    operand.output = descriptor
        .outputs
        .iter()
        .position(|output| output.name.identifier() == state.study_output)?;
    let mut stored = [false; MAXIMUM_SCAN_STUDY_PARAMETERS];
    for parameter in &state.study_parameters {
        let index = descriptor
            .parameters
            .iter()
            .position(|known| known.name.identifier() == parameter.name)?;
        if std::mem::replace(&mut stored[index], true) {
            return None;
        }
        operand.parameters[index] = ScanNumber {
            units: parameter.units,
            scale: u8::try_from(parameter.scale).ok()?,
        };
    }
    Some(operand)
}

fn operand_state(operand: ScanOperand) -> WorkspaceScanOperandState {
    let kind = |kind: WorkspaceScanOperandKind| WorkspaceScanOperandState {
        kind: kind as i32,
        ..WorkspaceScanOperandState::default()
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
        ScanOperand::Study(study) => {
            let descriptor = study.study.descriptor();
            WorkspaceScanOperandState {
                study: descriptor.identifier.to_string(),
                study_parameters: descriptor
                    .parameters
                    .iter()
                    .zip(study.parameters)
                    .map(|(parameter, value)| WorkspaceScanStudyParameterState {
                        name: parameter.name.identifier().to_string(),
                        units: value.units,
                        scale: u32::from(value.scale),
                    })
                    .collect(),
                study_output: study
                    .output_descriptor()
                    .map(|output| output.name.identifier().to_string())
                    .unwrap_or_default(),
                ..kind(WorkspaceScanOperandKind::Study)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_market_data::ChartInterval;
    use aeris_market_runtime::scan_studies;

    const CLOSE: ScanOperand = ScanOperand::Price(ScanPriceField::Close);

    fn number(units: i64, scale: u8) -> ScanOperand {
        ScanOperand::Number(ScanNumber { units, scale })
    }

    fn study(study: ScanStudy) -> ScanStudyOperand {
        ScanStudyOperand::new(study)
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
                timeframe: ChartInterval::Hour4,
                match_mode: ScanMatchMode::Any,
                conditions,
            },
            universe_limit: 200,
        }
    }

    /// Every bar value, and every line of every study at its defaults, plus studies with
    /// changed whole and decimal parameters.
    fn every_operand() -> Vec<ScanOperand> {
        let mut operands = vec![
            ScanOperand::Price(ScanPriceField::Open),
            ScanOperand::Price(ScanPriceField::High),
            ScanOperand::Price(ScanPriceField::Low),
            CLOSE,
            ScanOperand::Volume,
            ScanOperand::ChangePercent,
        ];
        for descriptor in scan_studies() {
            for output in 0..descriptor.outputs.len() {
                operands.push(ScanOperand::Study(ScanStudyOperand {
                    output,
                    ..study(descriptor.study)
                }));
            }
        }
        let mut macd = study(ScanStudy::Macd);
        macd.parameters[..3].copy_from_slice(&[8, 21, 5].map(ScanNumber::from));
        macd.output = 1;
        let mut bollinger = study(ScanStudy::Bollinger);
        bollinger.parameters[1] = ScanNumber {
            units: 25,
            scale: 1,
        };
        operands.extend([macd, bollinger].map(ScanOperand::Study));
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
            ..above(CLOSE, ScanOperand::Study(study(ScanStudy::Sma)))
        });
        let between = ScanCondition {
            comparison: ScanComparison::Between,
            upper: Some(number(70, 0)),
            ..above(ScanOperand::Study(study(ScanStudy::Rsi)), number(30, 0))
        };
        let mut saved = scan(
            "Comparisons",
            comparisons.into_iter().chain([between]).collect(),
        );
        for timeframe in ChartInterval::ALL {
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
    fn a_study_is_stored_by_its_chart_identifiers() {
        let mut macd = study(ScanStudy::Macd);
        macd.output = 1;
        let state = operand_state(ScanOperand::Study(macd));
        assert_eq!(state.kind, WorkspaceScanOperandKind::Study as i32);
        assert_eq!(state.study, "macd");
        assert_eq!(state.study_output, "signal");
        let names: Vec<_> = state
            .study_parameters
            .iter()
            .map(|parameter| (parameter.name.as_str(), parameter.units))
            .collect();
        assert_eq!(names, [("fast", 12), ("slow", 26), ("signal", 9)]);
    }

    #[test]
    fn a_study_parameter_not_stored_keeps_its_default() {
        let mut bollinger = study(ScanStudy::Bollinger);
        bollinger.output = 2;
        let mut state = operand_state(ScanOperand::Study(bollinger));
        state
            .study_parameters
            .retain(|parameter| parameter.name != "deviation");
        state.study_parameters[0].units = 30;
        let mut expected = bollinger;
        expected.parameters[0] = ScanNumber::from(30);
        assert_eq!(operand(&state), Some(ScanOperand::Study(expected)));
    }

    #[test]
    fn an_unknown_or_missing_value_makes_only_that_scan_invalid() {
        fn left(state: &mut WorkspaceScreenerScanState) -> &mut WorkspaceScanOperandState {
            state.conditions[0].left.as_mut().expect("left")
        }
        let valid = scan("Valid", vec![above(CLOSE, number(1, 0))]).to_state();
        let mut unknown_interval = valid.clone();
        unknown_interval.interval = "7h".to_string();
        let mut missing_interval = valid.clone();
        missing_interval.interval.clear();
        let mut unspecified_mode = valid.clone();
        unspecified_mode.match_mode = WorkspaceScanMatchMode::Unspecified as i32;
        let mut unknown_operand = valid.clone();
        unknown_operand.conditions[0].left = Some(WorkspaceScanOperandState {
            kind: 400,
            ..WorkspaceScanOperandState::default()
        });
        let mut retired_kind = valid.clone();
        retired_kind.conditions[0].left = Some(WorkspaceScanOperandState {
            kind: 7,
            ..WorkspaceScanOperandState::default()
        });
        let mut missing_right = valid.clone();
        missing_right.conditions[0].right = None;
        let study_scan = scan(
            "Macd",
            vec![above(
                ScanOperand::Study(study(ScanStudy::Macd)),
                number(0, 0),
            )],
        )
        .to_state();
        let mut unknown_study = study_scan.clone();
        left(&mut unknown_study).study = "macd_v2".to_string();
        let mut unknown_line = study_scan.clone();
        left(&mut unknown_line).study_output = "upper".to_string();
        let mut missing_line = study_scan.clone();
        left(&mut missing_line).study_output.clear();
        let mut unknown_parameter = study_scan.clone();
        left(&mut unknown_parameter).study_parameters[0].name = "period".to_string();
        let mut repeated_parameter = study_scan.clone();
        let first = left(&mut repeated_parameter).study_parameters[0].clone();
        left(&mut repeated_parameter).study_parameters.push(first);
        let mut wide_scale = study_scan.clone();
        left(&mut wide_scale).study_parameters[0].scale = 300;
        for invalid in [
            unknown_interval,
            missing_interval,
            unspecified_mode,
            unknown_operand,
            retired_kind,
            missing_right,
            unknown_study,
            unknown_line,
            missing_line,
            unknown_parameter,
            repeated_parameter,
            wide_scale,
        ] {
            assert_eq!(SavedScan::from_state(&invalid), None);
        }
        assert!(SavedScan::from_state(&valid).is_some());
        assert!(SavedScan::from_state(&study_scan).is_some());
    }

    #[test]
    fn a_definition_failing_validation_is_invalid() {
        let mut zero_period = study(ScanStudy::Sma);
        zero_period.parameters[0] = ScanNumber::default();
        let mut fast_above_slow = study(ScanStudy::Macd);
        fast_above_slow.parameters[0] = ScanNumber::from(30);
        let zero_period = scan("Zero", vec![above(CLOSE, ScanOperand::Study(zero_period))]);
        let fast_above_slow = scan(
            "Fast",
            vec![above(ScanOperand::Study(fast_above_slow), number(0, 0))],
        );
        let constant_left = scan("Constant", vec![above(number(1, 0), CLOSE)]);
        let no_conditions = scan("Empty", Vec::new());
        let wide_number = scan("Wide", vec![above(CLOSE, number(1, 12))]);
        for invalid in [
            zero_period,
            fast_above_slow,
            constant_left,
            no_conditions,
            wide_number,
        ] {
            assert_eq!(SavedScan::from_state(&invalid.to_state()), None);
        }
        let mut huge_period = scan(
            "Huge",
            vec![above(CLOSE, ScanOperand::Study(study(ScanStudy::Sma)))],
        )
        .to_state();
        huge_period.conditions[0]
            .right
            .as_mut()
            .expect("right")
            .study_parameters[0]
            .units = 70_000;
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
        left.study = "sma".to_string();
        left.study_output = "value".to_string();
        left.number_units = 20;
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
