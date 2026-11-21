use super::*;
use aeris_localization::{tr, workspace};

pub(super) fn runtime_study_registration(
    state: &WorkspaceChartStudyState,
    current_series: &BarSeriesKey,
    active: &[RuntimeStudyState],
) -> Result<NativeStudyRegistration, String> {
    let registry = super::super::study_packages::product_study_registry()
        .map_err(|error| tr!(workspace::STUDY_REGISTRY_INVALID, error = error))?;
    runtime_study_registration_with_registry(state, current_series, active, &registry)
}

pub(super) fn runtime_study_registration_with_registry(
    state: &WorkspaceChartStudyState,
    current_series: &BarSeriesKey,
    active: &[RuntimeStudyState],
    registry: &aeris_study_sdk::TrustedStudyRegistry<'_>,
) -> Result<NativeStudyRegistration, String> {
    let dependencies = state
        .dependencies
        .iter()
        .map(|dependency| runtime_study_dependency(dependency, current_series, active))
        .collect::<Result<Vec<_>, _>>()?;
    let settings = state
        .settings
        .iter()
        .map(|setting| {
            let value = setting.value.as_ref().ok_or_else(|| {
                tr!(
                    workspace::STUDY_SETTING_NO_VALUE,
                    setting = setting.identifier
                )
            })?;
            Ok((
                setting.identifier.clone(),
                runtime_study_setting_value(value)?,
            ))
        })
        .collect::<Result<std::collections::BTreeMap<_, _>, String>>()?;
    let registration = registry
        .restore(
            &state.identifier,
            state.implementation_revision,
            dependencies,
            settings,
        )
        .map_err(|error| error.to_string())?;
    let output_identifiers = registration
        .definition
        .outputs
        .iter()
        .map(|output| output.identifier.as_str())
        .collect::<Vec<_>>();
    if output_identifiers
        != state
            .output_identifiers
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    {
        return Err(tr!(
            workspace::STUDY_OUTPUT_INTERFACE_MISMATCH,
            study = state.identifier
        ));
    }
    Ok(registration)
}

pub(super) fn runtime_study_setting_value(
    value: &workspace_study_setting_state::Value,
) -> Result<StudySettingValue, String> {
    Ok(match value {
        workspace_study_setting_state::Value::Boolean(value) => StudySettingValue::Boolean(*value),
        workspace_study_setting_state::Value::Integer(value) => StudySettingValue::Integer(*value),
        workspace_study_setting_state::Value::Decimal(value) => {
            StudySettingValue::Decimal(StudyDecimal {
                mantissa: value.mantissa,
                scale: u8::try_from(value.scale)
                    .map_err(|_| tr!(workspace::STUDY_DECIMAL_SCALE_RANGE).to_string())?,
            })
        }
        workspace_study_setting_state::Value::Text(value) => StudySettingValue::Text(value.clone()),
        workspace_study_setting_state::Value::Choice(value) => {
            StudySettingValue::Choice(value.clone())
        }
    })
}

pub(super) fn persisted_study_setting_value(
    value: &StudySettingValue,
) -> workspace_study_setting_state::Value {
    match value {
        StudySettingValue::Boolean(value) => workspace_study_setting_state::Value::Boolean(*value),
        StudySettingValue::Integer(value) => workspace_study_setting_state::Value::Integer(*value),
        StudySettingValue::Decimal(value) => {
            workspace_study_setting_state::Value::Decimal(WorkspaceStudyDecimalState {
                mantissa: value.mantissa,
                scale: u32::from(value.scale),
            })
        }
        StudySettingValue::Text(value) => workspace_study_setting_state::Value::Text(value.clone()),
        StudySettingValue::Choice(value) => {
            workspace_study_setting_state::Value::Choice(value.clone())
        }
    }
}

pub(in crate::desktop) fn study_decimal_text(value: StudyDecimal) -> String {
    let scale = usize::from(value.scale);
    if scale == 0 {
        return value.mantissa.to_string();
    }
    let negative = value.mantissa < 0;
    let magnitude = i128::from(value.mantissa).abs();
    let factor = 10_i128.pow(u32::from(value.scale));
    let whole = magnitude / factor;
    let fraction = magnitude % factor;
    format!(
        "{}{whole}.{fraction:0width$}",
        if negative { "-" } else { "" },
        width = scale
    )
}

pub(super) fn parse_study_decimal(text: &str) -> Result<StudyDecimal, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(tr!(workspace::DECIMAL_EMPTY).to_string());
    }
    let (negative, unsigned) = text
        .strip_prefix('-')
        .map_or((false, text), |value| (true, value));
    let unsigned = unsigned.strip_prefix('+').unwrap_or(unsigned);
    let mut parts = unsigned.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || (whole.is_empty() && fraction.is_empty())
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len()
            > usize::from(aeris_market_runtime::study::MAXIMUM_STUDY_SETTING_DECIMAL_SCALE)
    {
        return Err(tr!(workspace::DECIMAL_INVALID).to_string());
    }
    let whole = if whole.is_empty() { "0" } else { whole };
    let scale = u8::try_from(fraction.len())
        .map_err(|_| tr!(workspace::DECIMAL_SCALE_INVALID).to_string())?;
    let factor = 10_i128.pow(u32::from(scale));
    let whole = whole
        .parse::<i128>()
        .map_err(|_| tr!(workspace::DECIMAL_OUT_OF_RANGE).to_string())?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<i128>()
            .map_err(|_| "decimal setting is out of range".to_string())?
    };
    let magnitude = whole
        .checked_mul(factor)
        .and_then(|value| value.checked_add(fraction))
        .ok_or_else(|| tr!(workspace::DECIMAL_OUT_OF_RANGE).to_string())?;
    let signed = if negative { -magnitude } else { magnitude };
    let mantissa =
        i64::try_from(signed).map_err(|_| tr!(workspace::DECIMAL_OUT_OF_RANGE).to_string())?;
    Ok(StudyDecimal { mantissa, scale })
}

pub(super) fn study_setting_input_text(value: &StudySettingValue) -> Option<String> {
    match value {
        StudySettingValue::Integer(value) => Some(value.to_string()),
        StudySettingValue::Decimal(value) => Some(study_decimal_text(*value)),
        StudySettingValue::Text(value) => Some(value.clone()),
        StudySettingValue::Boolean(_) | StudySettingValue::Choice(_) => None,
    }
}

pub(super) fn study_setting_value_from_dialog(
    dialog: &StudySettingsDialogState,
    spec: &StudySettingSpec,
    cx: &App,
) -> Result<StudySettingValue, String> {
    match &spec.presentation.control {
        StudySettingControl::Boolean | StudySettingControl::Choice { .. } => dialog
            .draft_values
            .get(&spec.identifier)
            .cloned()
            .ok_or_else(|| tr!(workspace::SETTING_NO_VALUE, label = spec.presentation.label)),
        StudySettingControl::Integer { .. } => {
            let input = dialog.inputs.get(&spec.identifier).ok_or_else(|| {
                tr!(
                    workspace::SETTING_EDITOR_UNAVAILABLE,
                    label = spec.presentation.label
                )
            })?;
            input
                .read(cx)
                .value()
                .trim()
                .parse::<i64>()
                .map(StudySettingValue::Integer)
                .map_err(|_| {
                    tr!(
                        workspace::SETTING_WHOLE_NUMBER,
                        label = spec.presentation.label
                    )
                })
        }
        StudySettingControl::Decimal { .. } => {
            let input = dialog.inputs.get(&spec.identifier).ok_or_else(|| {
                tr!(
                    workspace::SETTING_EDITOR_UNAVAILABLE,
                    label = spec.presentation.label
                )
            })?;
            parse_study_decimal(input.read(cx).value().as_ref())
                .map(StudySettingValue::Decimal)
                .map_err(|error| {
                    tr!(
                        workspace::SETTING_PARSE_ERROR,
                        label = spec.presentation.label,
                        error = error
                    )
                })
        }
        StudySettingControl::Text => {
            let input = dialog.inputs.get(&spec.identifier).ok_or_else(|| {
                tr!(
                    workspace::SETTING_EDITOR_UNAVAILABLE,
                    label = spec.presentation.label
                )
            })?;
            Ok(StudySettingValue::Text(input.read(cx).value().to_string()))
        }
    }
}

pub(in crate::desktop) fn study_setting_condition_matches(
    dialog: &StudySettingsDialogState,
    condition: Option<&StudySettingCondition>,
    cx: &App,
) -> bool {
    let Some(condition) = condition else {
        return true;
    };
    let Some(spec) = dialog
        .specs
        .iter()
        .find(|spec| spec.identifier == condition.setting_identifier)
    else {
        return false;
    };
    study_setting_value_from_dialog(dialog, spec, cx).is_ok_and(|value| value == condition.equals)
}

pub(in crate::desktop) fn study_display_name(identifier: &str) -> String {
    match identifier {
        aeris_study_sdk::BUILTIN_SMA_IDENTIFIER => tr!(workspace::STUDY_NAME_SMA).to_string(),
        aeris_study_sdk::BUILTIN_EMA_IDENTIFIER => tr!(workspace::STUDY_NAME_EMA).to_string(),
        aeris_study_sdk::BUILTIN_WMA_IDENTIFIER => tr!(workspace::STUDY_NAME_WMA).to_string(),
        aeris_study_sdk::BUILTIN_BOLLINGER_IDENTIFIER => {
            tr!(workspace::STUDY_NAME_BOLLINGER).to_string()
        }
        aeris_study_sdk::BUILTIN_VWAP_IDENTIFIER => tr!(workspace::STUDY_NAME_VWAP).to_string(),
        aeris_study_sdk::BUILTIN_RSI_IDENTIFIER => tr!(workspace::STUDY_NAME_RSI).to_string(),
        aeris_study_sdk::BUILTIN_MACD_IDENTIFIER => tr!(workspace::STUDY_NAME_MACD).to_string(),
        aeris_study_sdk::BUILTIN_STOCHASTIC_IDENTIFIER => {
            tr!(workspace::STUDY_NAME_STOCHASTIC).to_string()
        }
        aeris_study_sdk::BUILTIN_ATR_IDENTIFIER => tr!(workspace::STUDY_NAME_ATR).to_string(),
        _ => identifier.to_string(),
    }
}

fn runtime_study_streams(streams: &[i32]) -> Result<StreamRequirements, String> {
    let mut requirements = StreamRequirements::NONE;
    for stream in streams {
        let stream = WorkspaceStudyMarketStream::try_from(*stream)
            .map_err(|_| tr!(workspace::STUDY_STREAM_UNKNOWN).to_string())?;
        let stream = match stream {
            WorkspaceStudyMarketStream::Bars => MarketStream::Bars,
            WorkspaceStudyMarketStream::Trades => MarketStream::Trades,
            WorkspaceStudyMarketStream::Quotes => MarketStream::Quotes,
            WorkspaceStudyMarketStream::Depth => MarketStream::Depth,
            WorkspaceStudyMarketStream::Unspecified => {
                return Err(tr!(workspace::STUDY_STREAM_UNSPECIFIED).to_string());
            }
        };
        requirements = requirements.with(stream);
    }
    if requirements.is_empty() {
        return Err(tr!(workspace::STUDY_STREAM_EMPTY).to_string());
    }
    Ok(requirements)
}

fn runtime_study_series(series: &SeriesKey) -> Result<BarSeriesKey, String> {
    let period = aeris_market_runtime::legacy_series_period(series)
        .ok_or_else(|| tr!(workspace::STUDY_CADENCE_UNSUPPORTED).to_string())?;
    let series = BarSeriesKey {
        provider_id: series.provider.clone(),
        instrument_id: series.instrument_id.clone(),
        entitlement_id: series.entitlement_id.clone(),
        period,
        definition_version: series.definition_revision,
    };
    series.validate().map_err(|error| error.to_string())?;
    Ok(series)
}

fn runtime_study_dependency(
    dependency: &WorkspaceStudyDependencyState,
    current_series: &BarSeriesKey,
    active: &[RuntimeStudyState],
) -> Result<StudyDependency, String> {
    match WorkspaceStudyDependencyKind::try_from(dependency.kind)
        .map_err(|_| tr!(workspace::STUDY_DEPENDENCY_KIND_UNKNOWN).to_string())?
    {
        WorkspaceStudyDependencyKind::CurrentChartSeries => {
            Ok(StudyDependency::Market(StudyMarketInput {
                series: current_series.clone(),
                streams: runtime_study_streams(&dependency.streams)?,
            }))
        }
        WorkspaceStudyDependencyKind::ExplicitSeries => {
            let series = dependency
                .series
                .as_ref()
                .ok_or_else(|| tr!(workspace::STUDY_DEPENDENCY_NO_SERIES).to_string())?;
            Ok(StudyDependency::Market(StudyMarketInput {
                series: runtime_study_series(series)?,
                streams: runtime_study_streams(&dependency.streams)?,
            }))
        }
        WorkspaceStudyDependencyKind::StudyOutput => {
            let upstream = active
                .iter()
                .find(|state| state.persisted.local_id == dependency.study_local_id)
                .ok_or_else(|| {
                    tr!(
                        workspace::STUDY_DEPENDENCY_NOT_RESTORED,
                        dependency = dependency.study_local_id
                    )
                })?;
            let output_index = upstream
                .persisted
                .output_identifiers
                .iter()
                .position(|identifier| identifier == &dependency.output_identifier)
                .ok_or_else(|| {
                    tr!(
                        workspace::STUDY_DEPENDENCY_OUTPUT_UNAVAILABLE,
                        output = dependency.output_identifier
                    )
                })?;
            Ok(StudyDependency::Output(
                upstream.study_id.output(output_index),
            ))
        }
        WorkspaceStudyDependencyKind::Unspecified => {
            Err(tr!(workspace::STUDY_DEPENDENCY_KIND_UNSPECIFIED).to_string())
        }
    }
}

pub(super) fn runtime_study_uses_current_chart(state: &WorkspaceChartStudyState) -> bool {
    state.dependencies.iter().any(|dependency| {
        WorkspaceStudyDependencyKind::try_from(dependency.kind)
            .is_ok_and(|kind| kind == WorkspaceStudyDependencyKind::CurrentChartSeries)
    })
}

fn integer_setting(identifier: &str, value: i64) -> WorkspaceStudySettingState {
    WorkspaceStudySettingState {
        identifier: identifier.to_string(),
        value: Some(workspace_study_setting_state::Value::Integer(value)),
    }
}

fn default_runtime_study_contract(
    indicator: ChartIndicator,
) -> Option<(
    &'static str,
    u32,
    Vec<WorkspaceStudySettingState>,
    Vec<String>,
)> {
    let contract = match indicator {
        ChartIndicator::Sma => (
            aeris_study_sdk::BUILTIN_SMA_IDENTIFIER,
            aeris_study_sdk::BUILTIN_SMA_IMPLEMENTATION_REVISION,
            vec![integer_setting(
                aeris_study_sdk::BUILTIN_SMA_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_SMA_DEFAULT_PERIOD,
            )],
            vec![aeris_study_sdk::BUILTIN_SMA_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::Ema => (
            aeris_study_sdk::BUILTIN_EMA_IDENTIFIER,
            aeris_study_sdk::BUILTIN_EMA_IMPLEMENTATION_REVISION,
            vec![integer_setting(
                aeris_study_sdk::BUILTIN_EMA_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_EMA_DEFAULT_PERIOD,
            )],
            vec![aeris_study_sdk::BUILTIN_EMA_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::Bollinger => default_bollinger_contract(),
        ChartIndicator::Vwap => (
            aeris_study_sdk::BUILTIN_VWAP_IDENTIFIER,
            aeris_study_sdk::BUILTIN_VWAP_IMPLEMENTATION_REVISION,
            Vec::new(),
            vec![aeris_study_sdk::BUILTIN_VWAP_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::Rsi => (
            aeris_study_sdk::BUILTIN_RSI_IDENTIFIER,
            aeris_study_sdk::BUILTIN_RSI_IMPLEMENTATION_REVISION,
            vec![integer_setting(
                aeris_study_sdk::BUILTIN_RSI_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_RSI_DEFAULT_PERIOD,
            )],
            vec![aeris_study_sdk::BUILTIN_RSI_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::Macd => default_macd_contract(),
        ChartIndicator::Stochastic => default_stochastic_contract(),
        ChartIndicator::Atr => (
            aeris_study_sdk::BUILTIN_ATR_IDENTIFIER,
            aeris_study_sdk::BUILTIN_ATR_IMPLEMENTATION_REVISION,
            vec![integer_setting(
                aeris_study_sdk::BUILTIN_ATR_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_ATR_DEFAULT_PERIOD,
            )],
            vec![aeris_study_sdk::BUILTIN_ATR_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::Volume
        | ChartIndicator::VolumeProfile
        | ChartIndicator::SessionVolumeProfile
        | ChartIndicator::Builtin(_) => {
            return None;
        }
    };
    Some(contract)
}

fn default_macd_contract() -> (
    &'static str,
    u32,
    Vec<WorkspaceStudySettingState>,
    Vec<String>,
) {
    (
        aeris_study_sdk::BUILTIN_MACD_IDENTIFIER,
        aeris_study_sdk::BUILTIN_MACD_IMPLEMENTATION_REVISION,
        vec![
            integer_setting(
                aeris_study_sdk::BUILTIN_MACD_FAST_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_MACD_DEFAULT_FAST_PERIOD,
            ),
            integer_setting(
                aeris_study_sdk::BUILTIN_MACD_SLOW_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_MACD_DEFAULT_SLOW_PERIOD,
            ),
            integer_setting(
                aeris_study_sdk::BUILTIN_MACD_SIGNAL_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_MACD_DEFAULT_SIGNAL_PERIOD,
            ),
        ],
        vec![
            aeris_study_sdk::BUILTIN_MACD_LINE_OUTPUT_IDENTIFIER.to_string(),
            aeris_study_sdk::BUILTIN_MACD_SIGNAL_OUTPUT_IDENTIFIER.to_string(),
            aeris_study_sdk::BUILTIN_MACD_HISTOGRAM_OUTPUT_IDENTIFIER.to_string(),
        ],
    )
}

fn default_stochastic_contract() -> (
    &'static str,
    u32,
    Vec<WorkspaceStudySettingState>,
    Vec<String>,
) {
    (
        aeris_study_sdk::BUILTIN_STOCHASTIC_IDENTIFIER,
        aeris_study_sdk::BUILTIN_STOCHASTIC_IMPLEMENTATION_REVISION,
        vec![
            integer_setting(
                aeris_study_sdk::BUILTIN_STOCHASTIC_K_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_STOCHASTIC_DEFAULT_K_PERIOD,
            ),
            integer_setting(
                aeris_study_sdk::BUILTIN_STOCHASTIC_D_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_STOCHASTIC_DEFAULT_D_PERIOD,
            ),
        ],
        vec![
            aeris_study_sdk::BUILTIN_STOCHASTIC_K_OUTPUT_IDENTIFIER.to_string(),
            aeris_study_sdk::BUILTIN_STOCHASTIC_D_OUTPUT_IDENTIFIER.to_string(),
        ],
    )
}

fn default_bollinger_contract() -> (
    &'static str,
    u32,
    Vec<WorkspaceStudySettingState>,
    Vec<String>,
) {
    (
        aeris_study_sdk::BUILTIN_BOLLINGER_IDENTIFIER,
        aeris_study_sdk::BUILTIN_BOLLINGER_IMPLEMENTATION_REVISION,
        vec![
            integer_setting(
                aeris_study_sdk::BUILTIN_BOLLINGER_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_BOLLINGER_DEFAULT_PERIOD,
            ),
            WorkspaceStudySettingState {
                identifier: aeris_study_sdk::BUILTIN_BOLLINGER_DEVIATION_SETTING.to_string(),
                value: Some(workspace_study_setting_state::Value::Decimal(
                    WorkspaceStudyDecimalState {
                        mantissa: aeris_study_sdk::BUILTIN_BOLLINGER_DEFAULT_DEVIATION.mantissa,
                        scale: u32::from(
                            aeris_study_sdk::BUILTIN_BOLLINGER_DEFAULT_DEVIATION.scale,
                        ),
                    },
                )),
            },
        ],
        vec![
            aeris_study_sdk::BUILTIN_BOLLINGER_UPPER_OUTPUT_IDENTIFIER.to_string(),
            aeris_study_sdk::BUILTIN_BOLLINGER_MIDDLE_OUTPUT_IDENTIFIER.to_string(),
            aeris_study_sdk::BUILTIN_BOLLINGER_LOWER_OUTPUT_IDENTIFIER.to_string(),
        ],
    )
}

/// Stroke width a persisted study draws with. Zero means the user never chose one (or the
/// state predates the setting), so the study's default applies.
pub(super) fn persisted_study_line_width(persisted: &WorkspaceChartStudyState) -> u8 {
    u8::try_from(persisted.line_width)
        .ok()
        .filter(|width| (1..=MAXIMUM_STUDY_LINE_WIDTH).contains(width))
        .unwrap_or_else(|| default_study_line_width(&persisted.identifier))
}

/// Line colors a persisted study draws with by output index; `None` keeps the chart default.
pub(in crate::desktop) fn persisted_study_output_colors(
    persisted: &WorkspaceChartStudyState,
) -> Vec<Option<String>> {
    persisted
        .output_colors
        .iter()
        .map(|color| (!color.is_empty()).then(|| color.clone()))
        .collect()
}

/// The durable form of a dialog's colors: one entry per output, or none when every output
/// keeps the chart default.
pub(in crate::desktop) fn persisted_output_colors(
    output_count: usize,
    drafts: &[StudyOutputColorDraft],
) -> Vec<String> {
    if drafts.iter().all(|draft| draft.color.is_none()) {
        return Vec::new();
    }
    let mut colors = vec![String::new(); output_count];
    for draft in drafts {
        if let (Some(slot), Some(color)) = (colors.get_mut(draft.output_index), &draft.color) {
            slot.clone_from(color);
        }
    }
    colors
}

/// Stroke width a study draws with until the user picks one. Exponential moving averages are
/// one-pixel lines, matching Aeris Charts' own EMA defaults; other studies keep the chart
/// default. Studies reach the chart as external outputs, so this host default is what applies.
pub(super) fn default_study_line_width(identifier: &str) -> u8 {
    match identifier {
        aeris_study_sdk::BUILTIN_EMA_IDENTIFIER => 1,
        _ => DEFAULT_STUDY_LINE_WIDTH,
    }
}

/// The default durable study state for a runtime-managed builtin indicator: the add-from-menu
/// path and the workspace migration chain both materialize it.
pub(in crate::desktop) fn default_runtime_study(
    local_id: u64,
    indicator: ChartIndicator,
    visible: bool,
) -> Option<WorkspaceChartStudyState> {
    let (identifier, implementation_revision, settings, output_identifiers) =
        default_runtime_study_contract(indicator)?;
    Some(WorkspaceChartStudyState {
        line_width: 0,
        output_colors: Vec::new(),
        local_id,
        identifier: identifier.to_string(),
        implementation_revision,
        settings,
        dependencies: vec![WorkspaceStudyDependencyState {
            kind: WorkspaceStudyDependencyKind::CurrentChartSeries as i32,
            streams: vec![WorkspaceStudyMarketStream::Bars as i32],
            ..WorkspaceStudyDependencyState::default()
        }],
        visible,
        output_identifiers,
    })
}

/// The pane's durable studies. The migration chain has already materialized runtime-managed
/// legacy indicator entries into the study graph, so restore reads it without re-deriving.
pub(super) fn persisted_runtime_studies(
    restored_chart_state: Option<&WorkspaceChartState>,
) -> Vec<PendingRuntimeStudyState> {
    restored_chart_state
        .map(|state| state.studies.clone())
        .unwrap_or_default()
        .into_iter()
        .map(|persisted| PendingRuntimeStudyState {
            persisted,
            resolved_chart_series: None,
            remove_on_registration: false,
            persist_on_registration: false,
            blocked: false,
        })
        .collect()
}
