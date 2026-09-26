//! Workspace surface.

use super::*;

fn restore_price_alerts(
    restored_chart_state: Option<&WorkspaceChartState>,
    market_worker: &MarketDataWorker,
) -> (Vec<WorkspacePriceAlertState>, Option<String>) {
    let alerts = restored_chart_state
        .map(|state| state.price_alerts.clone())
        .unwrap_or_default();
    let message = (!alerts.is_empty())
        .then(|| {
            market_worker
                .try_replace_price_alerts(runtime_price_alerts(&alerts))
                .err()
        })
        .flatten()
        .map(|_| "Price alerts could not be queued for live monitoring".to_string());
    (alerts, message)
}

fn chart_bridge_label(chart: Option<&Entity<NucleusChartView>>, cx: &App) -> String {
    chart.map_or_else(
        || "bridge awaiting snapshot".to_string(),
        |chart| bridge_status(chart.read(cx).replay_bridge_metrics()),
    )
}

fn initial_symbol_browser(startup: &MarketWorkerStartup) -> rithmic_shell::RithmicSymbolBrowser {
    match startup {
        MarketWorkerStartup::Rithmic => rithmic_shell::RithmicSymbolBrowser::default(),
        MarketWorkerStartup::Loading(_) => demand_startup_symbol_browser(),
    }
}

/// Market-demand endpoints issue their own startup catalog search as request 1.
fn demand_startup_symbol_browser() -> rithmic_shell::RithmicSymbolBrowser {
    rithmic_shell::RithmicSymbolBrowser::rithmic_catalog_awaiting_search(
        std::num::NonZeroUsize::MIN,
        "",
    )
}

fn restored_market_selection(
    startup: &MarketWorkerStartup,
) -> Option<(ChartInterval, Option<(i64, i64)>)> {
    match startup {
        MarketWorkerStartup::Loading(startup) => {
            Some((startup.interval, startup.restored_viewport))
        }
        MarketWorkerStartup::Rithmic => None,
    }
}

pub(super) fn persisted_chart_appearance(
    appearance: &ChartAppearanceSettings,
) -> WorkspaceChartAppearanceState {
    WorkspaceChartAppearanceState {
        grid_visible: appearance.grid_visible,
        grid_color: appearance.grid_color.clone(),
        grid_style: u32::from(appearance.grid_style),
        crosshair_color: appearance.crosshair_color.clone(),
        crosshair_width: u32::from(appearance.crosshair_width),
        crosshair_style: u32::from(appearance.crosshair_style),
        up_color: appearance.up_color.clone(),
        down_color: appearance.down_color.clone(),
        wick_up_color: appearance.wick_up_color.clone(),
        wick_down_color: appearance.wick_down_color.clone(),
        border_up_color: appearance.border_up_color.clone(),
        border_down_color: appearance.border_down_color.clone(),
        wick_visible: appearance.wick_visible,
        border_visible: appearance.border_visible,
        open_visible: appearance.open_visible,
        thin_bars: appearance.thin_bars,
        line_color: appearance.line_color.clone(),
        line_width: u32::from(appearance.line_width),
        line_style: u32::from(appearance.line_style),
        area_top_color: appearance.area_top_color.clone(),
        baseline_top_color: appearance.baseline_top_color.clone(),
        baseline_bottom_color: appearance.baseline_bottom_color.clone(),
    }
}

pub(super) fn restored_chart_appearance(
    appearance: &WorkspaceChartAppearanceState,
) -> Option<ChartAppearanceSettings> {
    let grid_style = u8::try_from(appearance.grid_style).ok()?.min(4);
    let crosshair_width = u8::try_from(appearance.crosshair_width).ok()?.clamp(1, 4);
    let crosshair_style = u8::try_from(appearance.crosshair_style).ok()?.min(4);
    let line_width = u8::try_from(appearance.line_width).ok()?.clamp(1, 4);
    let line_style = u8::try_from(appearance.line_style).ok()?.min(4);
    Some(ChartAppearanceSettings {
        grid_visible: appearance.grid_visible,
        grid_color: appearance.grid_color.clone(),
        grid_style,
        crosshair_color: appearance.crosshair_color.clone(),
        crosshair_width,
        crosshair_style,
        up_color: appearance.up_color.clone(),
        down_color: appearance.down_color.clone(),
        wick_up_color: appearance.wick_up_color.clone(),
        wick_down_color: appearance.wick_down_color.clone(),
        border_up_color: appearance.border_up_color.clone(),
        border_down_color: appearance.border_down_color.clone(),
        wick_visible: appearance.wick_visible,
        border_visible: appearance.border_visible,
        open_visible: appearance.open_visible,
        thin_bars: appearance.thin_bars,
        line_color: appearance.line_color.clone(),
        line_width,
        line_style,
        area_top_color: appearance.area_top_color.clone(),
        baseline_top_color: appearance.baseline_top_color.clone(),
        baseline_bottom_color: appearance.baseline_bottom_color.clone(),
    })
}

const fn chart_study_plot(plot: StudyPlotKind) -> ChartStudyPlotKind {
    match plot {
        StudyPlotKind::Line => ChartStudyPlotKind::Line,
        StudyPlotKind::Histogram => ChartStudyPlotKind::Histogram,
        StudyPlotKind::Area => ChartStudyPlotKind::Area,
    }
}

const fn chart_study_pane(pane: StudyPaneTarget) -> ChartStudyPaneTarget {
    match pane {
        StudyPaneTarget::Price => ChartStudyPaneTarget::Price,
        StudyPaneTarget::Dedicated { group } => ChartStudyPaneTarget::Dedicated { group },
    }
}

const fn chart_study_scale(scale: StudyScaleTarget) -> ChartStudyScaleTarget {
    match scale {
        StudyScaleTarget::Primary => ChartStudyScaleTarget::Primary,
        StudyScaleTarget::Left => ChartStudyScaleTarget::Left,
        StudyScaleTarget::Overlay => ChartStudyScaleTarget::Overlay,
    }
}

const fn chart_study_point_style(style: StudyPointStyle) -> ChartStudyPointStyle {
    match style {
        StudyPointStyle::Uniform => ChartStudyPointStyle::Uniform,
        StudyPointStyle::MomentumHistogram => ChartStudyPointStyle::MomentumHistogram,
    }
}

const fn chart_study_input_requirements(
    requirements: StreamRequirements,
) -> ChartStudyInputRequirements {
    let mut mapped = ChartStudyInputRequirements::NONE;
    if requirements.contains(MarketStream::Bars) {
        mapped = mapped.with(ChartStudyInputStream::Bars);
    }
    if requirements.contains(MarketStream::Trades) {
        mapped = mapped.with(ChartStudyInputStream::Trades);
    }
    if requirements.contains(MarketStream::Quotes) {
        mapped = mapped.with(ChartStudyInputStream::Quotes);
    }
    if requirements.contains(MarketStream::Depth) {
        mapped = mapped.with(ChartStudyInputStream::Depth);
    }
    mapped
}

fn chart_study_threshold(
    region: Option<StudyThresholdRegion>,
) -> Option<ChartStudyThresholdRegion> {
    region.and_then(|region| {
        Some(ChartStudyThresholdRegion {
            lower: region.lower.mantissa.to_f64()? / 10_f64.powi(i32::from(region.lower.scale)),
            upper: region.upper.mantissa.to_f64()? / 10_f64.powi(i32::from(region.upper.scale)),
        })
    })
}

const fn runtime_managed_indicator(indicator: ChartIndicator) -> bool {
    matches!(
        indicator,
        ChartIndicator::Sma
            | ChartIndicator::Ema
            | ChartIndicator::EmaRibbon
            | ChartIndicator::Wma
            | ChartIndicator::Bollinger
            | ChartIndicator::Vwap
            | ChartIndicator::Rsi
            | ChartIndicator::Macd
            | ChartIndicator::Stochastic
            | ChartIndicator::Atr
    )
}

fn persisted_legacy_indicator_states(
    states: impl IntoIterator<Item = ChartIndicatorState>,
) -> Vec<WorkspaceChartIndicatorState> {
    states
        .into_iter()
        .filter(|state| !runtime_managed_indicator(state.indicator))
        .map(|state| WorkspaceChartIndicatorState {
            kind: state.indicator.identifier().to_string(),
            visible: state.visible,
        })
        .collect()
}

fn runtime_study_count(studies: &RuntimeStudiesState) -> usize {
    studies
        .active
        .iter()
        .filter(|state| {
            !studies.removing.contains(&state.study_id)
                && !studies.automatic_removals.contains(&state.study_id)
        })
        .count()
        + studies
            .pending
            .values()
            .filter(|state| !state.remove_on_registration)
            .count()
        + studies.deferred.len()
}

fn persisted_runtime_study_states(
    studies: &RuntimeStudiesState,
    mut visible: impl FnMut(StudyInstanceId) -> Option<bool>,
) -> Vec<WorkspaceChartStudyState> {
    let mut persisted = studies
        .active
        .iter()
        .filter(|state| !studies.automatic_removals.contains(&state.study_id))
        .map(|state| {
            let mut persisted = state.persisted.clone();
            persisted.visible = visible(state.study_id).unwrap_or(persisted.visible);
            persisted
        })
        .chain(
            studies
                .pending
                .values()
                .filter(|state| !state.remove_on_registration)
                .map(|state| state.persisted.clone()),
        )
        .chain(
            studies
                .deferred
                .iter()
                .filter(|state| !state.remove_on_registration)
                .map(|state| state.persisted.clone()),
        )
        .collect::<Vec<_>>();
    persisted.sort_by_key(|state| state.local_id);
    persisted
}

fn dispatch_automatic_study_removals(
    studies: &mut RuntimeStudiesState,
    mut try_remove: impl FnMut(StudyInstanceId) -> Result<(), TrySendError<StudyInstanceId>>,
) -> bool {
    let mut retry = studies
        .automatic_removals
        .iter()
        .filter(|study_id| !studies.removing.contains(study_id))
        .copied()
        .collect::<Vec<_>>();
    retry.sort_unstable();
    for study_id in retry {
        match try_remove(study_id) {
            Ok(()) => {
                studies.removing.insert(study_id);
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => return false,
        }
    }
    true
}

fn study_removal_failed(studies: &mut RuntimeStudiesState, study_id: StudyInstanceId) -> bool {
    studies.removing.remove(&study_id);
    studies.automatic_removals.contains(&study_id)
}

fn discard_unregistered_runtime_studies(studies: &mut RuntimeStudiesState) -> bool {
    let changed = !studies.deferred.is_empty()
        || studies
            .pending
            .values()
            .any(|state| !state.remove_on_registration);
    for pending in studies.pending.values_mut() {
        pending.remove_on_registration = true;
    }
    studies.deferred.clear();
    changed
}

fn next_deferred_runtime_study_index(studies: &RuntimeStudiesState) -> Option<usize> {
    studies.deferred.iter().position(|state| !state.blocked)
}

fn runtime_study_registration(
    state: &WorkspaceChartStudyState,
    current_series: &BarSeriesKey,
    active: &[RuntimeStudyState],
) -> Result<NativeStudyRegistration, String> {
    let registry = super::study_packages::product_study_registry()
        .map_err(|error| format!("trusted study package registry is invalid: {error}"))?;
    runtime_study_registration_with_registry(state, current_series, active, &registry)
}

fn runtime_study_registration_with_registry(
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
            let value = setting
                .value
                .as_ref()
                .ok_or_else(|| format!("study setting {} has no value", setting.identifier))?;
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
        return Err(format!(
            "study {} output interface no longer matches this workspace",
            state.identifier
        ));
    }
    Ok(registration)
}

fn runtime_study_setting_value(
    value: &workspace_study_setting_state::Value,
) -> Result<StudySettingValue, String> {
    Ok(match value {
        workspace_study_setting_state::Value::Boolean(value) => StudySettingValue::Boolean(*value),
        workspace_study_setting_state::Value::Integer(value) => StudySettingValue::Integer(*value),
        workspace_study_setting_state::Value::Decimal(value) => {
            StudySettingValue::Decimal(StudyDecimal {
                mantissa: value.mantissa,
                scale: u8::try_from(value.scale)
                    .map_err(|_| "study decimal scale is out of range".to_string())?,
            })
        }
        workspace_study_setting_state::Value::Text(value) => StudySettingValue::Text(value.clone()),
        workspace_study_setting_state::Value::Choice(value) => {
            StudySettingValue::Choice(value.clone())
        }
    })
}

fn persisted_study_setting_value(
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

pub(super) fn study_decimal_text(value: StudyDecimal) -> String {
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

fn parse_study_decimal(text: &str) -> Result<StudyDecimal, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("decimal setting cannot be empty".to_string());
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
        return Err("decimal setting is invalid".to_string());
    }
    let whole = if whole.is_empty() { "0" } else { whole };
    let scale = u8::try_from(fraction.len()).map_err(|_| "decimal scale is invalid".to_string())?;
    let factor = 10_i128.pow(u32::from(scale));
    let whole = whole
        .parse::<i128>()
        .map_err(|_| "decimal setting is out of range".to_string())?;
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
        .ok_or_else(|| "decimal setting is out of range".to_string())?;
    let signed = if negative { -magnitude } else { magnitude };
    let mantissa =
        i64::try_from(signed).map_err(|_| "decimal setting is out of range".to_string())?;
    Ok(StudyDecimal { mantissa, scale })
}

fn study_setting_input_text(value: &StudySettingValue) -> Option<String> {
    match value {
        StudySettingValue::Integer(value) => Some(value.to_string()),
        StudySettingValue::Decimal(value) => Some(study_decimal_text(*value)),
        StudySettingValue::Text(value) => Some(value.clone()),
        StudySettingValue::Boolean(_) | StudySettingValue::Choice(_) => None,
    }
}

fn study_setting_value_from_dialog(
    dialog: &StudySettingsDialogState,
    spec: &StudySettingSpec,
    cx: &App,
) -> Result<StudySettingValue, String> {
    match &spec.presentation.control {
        StudySettingControl::Boolean | StudySettingControl::Choice { .. } => dialog
            .draft_values
            .get(&spec.identifier)
            .cloned()
            .ok_or_else(|| format!("{} has no value", spec.presentation.label)),
        StudySettingControl::Integer { .. } => {
            let input = dialog
                .inputs
                .get(&spec.identifier)
                .ok_or_else(|| format!("{} editor is unavailable", spec.presentation.label))?;
            input
                .read(cx)
                .value()
                .trim()
                .parse::<i64>()
                .map(StudySettingValue::Integer)
                .map_err(|_| format!("{} must be a whole number", spec.presentation.label))
        }
        StudySettingControl::Decimal { .. } => {
            let input = dialog
                .inputs
                .get(&spec.identifier)
                .ok_or_else(|| format!("{} editor is unavailable", spec.presentation.label))?;
            parse_study_decimal(input.read(cx).value().as_ref())
                .map(StudySettingValue::Decimal)
                .map_err(|error| format!("{}: {error}", spec.presentation.label))
        }
        StudySettingControl::Text => {
            let input = dialog
                .inputs
                .get(&spec.identifier)
                .ok_or_else(|| format!("{} editor is unavailable", spec.presentation.label))?;
            Ok(StudySettingValue::Text(input.read(cx).value().to_string()))
        }
    }
}

pub(super) fn study_setting_condition_matches(
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

pub(super) fn study_display_name(identifier: &str) -> String {
    match identifier {
        aeris_study_sdk::BUILTIN_SMA_IDENTIFIER => "Simple Moving Average".to_string(),
        aeris_study_sdk::BUILTIN_EMA_IDENTIFIER => "Exponential Moving Average".to_string(),
        aeris_study_sdk::BUILTIN_EMA_RIBBON_IDENTIFIER => "EMA Ribbon".to_string(),
        aeris_study_sdk::BUILTIN_WMA_IDENTIFIER => "Weighted Moving Average".to_string(),
        aeris_study_sdk::BUILTIN_BOLLINGER_IDENTIFIER => "Bollinger Bands".to_string(),
        aeris_study_sdk::BUILTIN_VWAP_IDENTIFIER => "Volume Weighted Average Price".to_string(),
        aeris_study_sdk::BUILTIN_RSI_IDENTIFIER => "Relative Strength Index".to_string(),
        aeris_study_sdk::BUILTIN_MACD_IDENTIFIER => "MACD".to_string(),
        aeris_study_sdk::BUILTIN_STOCHASTIC_IDENTIFIER => "Stochastic".to_string(),
        aeris_study_sdk::BUILTIN_ATR_IDENTIFIER => "Average True Range".to_string(),
        _ => identifier.to_string(),
    }
}

fn runtime_study_streams(streams: &[i32]) -> Result<StreamRequirements, String> {
    let mut requirements = StreamRequirements::NONE;
    for stream in streams {
        let stream = WorkspaceStudyMarketStream::try_from(*stream)
            .map_err(|_| "study market dependency contains an unknown stream".to_string())?;
        let stream = match stream {
            WorkspaceStudyMarketStream::Bars => MarketStream::Bars,
            WorkspaceStudyMarketStream::Trades => MarketStream::Trades,
            WorkspaceStudyMarketStream::Quotes => MarketStream::Quotes,
            WorkspaceStudyMarketStream::Depth => MarketStream::Depth,
            WorkspaceStudyMarketStream::Unspecified => {
                return Err("study market dependency contains an unspecified stream".to_string());
            }
        };
        requirements = requirements.with(stream);
    }
    if requirements.is_empty() {
        return Err("study market dependency has no streams".to_string());
    }
    Ok(requirements)
}

fn runtime_study_series(series: &SeriesKey) -> Result<BarSeriesKey, String> {
    let cadence = SeriesCadence::try_from(series.cadence)
        .map_err(|_| "study series cadence is unknown".to_string())?;
    let period = match cadence {
        SeriesCadence::FixedSeconds => aeris_market_data::BarPeriod::time(series.cadence_value),
        SeriesCadence::Trades => aeris_market_data::BarPeriod::tick(series.cadence_value),
        SeriesCadence::SessionDays => aeris_market_data::BarPeriod::session(series.cadence_value),
        SeriesCadence::CalendarWeeks => aeris_market_data::BarPeriod::week(series.cadence_value),
        SeriesCadence::CalendarMonths => aeris_market_data::BarPeriod::month(series.cadence_value),
        SeriesCadence::Unspecified => {
            return Err("study series cadence is unspecified".to_string());
        }
    }
    .map_err(|error| error.to_string())?;
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
        .map_err(|_| "study dependency kind is unknown".to_string())?
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
                .ok_or_else(|| "study explicit-series dependency has no series".to_string())?;
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
                    format!(
                        "study dependency {} has not been restored yet",
                        dependency.study_local_id
                    )
                })?;
            let output_index = upstream
                .persisted
                .output_identifiers
                .iter()
                .position(|identifier| identifier == &dependency.output_identifier)
                .ok_or_else(|| {
                    format!(
                        "study dependency output {} is unavailable",
                        dependency.output_identifier
                    )
                })?;
            Ok(StudyDependency::Output(
                upstream.study_id.output(output_index),
            ))
        }
        WorkspaceStudyDependencyKind::Unspecified => {
            Err("study dependency kind is unspecified".to_string())
        }
    }
}

fn runtime_study_uses_current_chart(state: &WorkspaceChartStudyState) -> bool {
    state.dependencies.iter().any(|dependency| {
        WorkspaceStudyDependencyKind::try_from(dependency.kind)
            .is_ok_and(|kind| kind == WorkspaceStudyDependencyKind::CurrentChartSeries)
    })
}

fn legacy_integer_setting(identifier: &str, value: i64) -> WorkspaceStudySettingState {
    WorkspaceStudySettingState {
        identifier: identifier.to_string(),
        value: Some(workspace_study_setting_state::Value::Integer(value)),
    }
}

fn legacy_runtime_study_contract(
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
            vec![legacy_integer_setting(
                aeris_study_sdk::BUILTIN_SMA_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_SMA_DEFAULT_PERIOD,
            )],
            vec![aeris_study_sdk::BUILTIN_SMA_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::Ema => (
            aeris_study_sdk::BUILTIN_EMA_IDENTIFIER,
            aeris_study_sdk::BUILTIN_EMA_IMPLEMENTATION_REVISION,
            vec![legacy_integer_setting(
                aeris_study_sdk::BUILTIN_EMA_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_EMA_DEFAULT_PERIOD,
            )],
            vec![aeris_study_sdk::BUILTIN_EMA_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::EmaRibbon => legacy_ema_ribbon_contract(),
        ChartIndicator::Wma => (
            aeris_study_sdk::BUILTIN_WMA_IDENTIFIER,
            aeris_study_sdk::BUILTIN_WMA_IMPLEMENTATION_REVISION,
            vec![legacy_integer_setting(
                aeris_study_sdk::BUILTIN_WMA_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_WMA_DEFAULT_PERIOD,
            )],
            vec![aeris_study_sdk::BUILTIN_WMA_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::Bollinger => legacy_bollinger_contract(),
        ChartIndicator::Vwap => (
            aeris_study_sdk::BUILTIN_VWAP_IDENTIFIER,
            aeris_study_sdk::BUILTIN_VWAP_IMPLEMENTATION_REVISION,
            Vec::new(),
            vec![aeris_study_sdk::BUILTIN_VWAP_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::Rsi => (
            aeris_study_sdk::BUILTIN_RSI_IDENTIFIER,
            aeris_study_sdk::BUILTIN_RSI_IMPLEMENTATION_REVISION,
            vec![legacy_integer_setting(
                aeris_study_sdk::BUILTIN_RSI_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_RSI_DEFAULT_PERIOD,
            )],
            vec![aeris_study_sdk::BUILTIN_RSI_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::Macd => legacy_macd_contract(),
        ChartIndicator::Stochastic => legacy_stochastic_contract(),
        ChartIndicator::Atr => (
            aeris_study_sdk::BUILTIN_ATR_IDENTIFIER,
            aeris_study_sdk::BUILTIN_ATR_IMPLEMENTATION_REVISION,
            vec![legacy_integer_setting(
                aeris_study_sdk::BUILTIN_ATR_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_ATR_DEFAULT_PERIOD,
            )],
            vec![aeris_study_sdk::BUILTIN_ATR_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::Volume => return None,
    };
    Some(contract)
}

fn legacy_ema_ribbon_contract() -> (
    &'static str,
    u32,
    Vec<WorkspaceStudySettingState>,
    Vec<String>,
) {
    (
        aeris_study_sdk::BUILTIN_EMA_RIBBON_IDENTIFIER,
        aeris_study_sdk::BUILTIN_EMA_RIBBON_IMPLEMENTATION_REVISION,
        aeris_study_sdk::BUILTIN_EMA_RIBBON_PERIOD_SETTINGS
            .iter()
            .zip(aeris_study_sdk::BUILTIN_EMA_RIBBON_DEFAULT_PERIODS)
            .map(|(identifier, period)| legacy_integer_setting(identifier, period))
            .collect(),
        aeris_study_sdk::BUILTIN_EMA_RIBBON_OUTPUT_IDENTIFIERS
            .iter()
            .map(|identifier| (*identifier).to_string())
            .collect(),
    )
}

fn legacy_macd_contract() -> (
    &'static str,
    u32,
    Vec<WorkspaceStudySettingState>,
    Vec<String>,
) {
    (
        aeris_study_sdk::BUILTIN_MACD_IDENTIFIER,
        aeris_study_sdk::BUILTIN_MACD_IMPLEMENTATION_REVISION,
        vec![
            legacy_integer_setting(
                aeris_study_sdk::BUILTIN_MACD_FAST_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_MACD_DEFAULT_FAST_PERIOD,
            ),
            legacy_integer_setting(
                aeris_study_sdk::BUILTIN_MACD_SLOW_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_MACD_DEFAULT_SLOW_PERIOD,
            ),
            legacy_integer_setting(
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

fn legacy_stochastic_contract() -> (
    &'static str,
    u32,
    Vec<WorkspaceStudySettingState>,
    Vec<String>,
) {
    (
        aeris_study_sdk::BUILTIN_STOCHASTIC_IDENTIFIER,
        aeris_study_sdk::BUILTIN_STOCHASTIC_IMPLEMENTATION_REVISION,
        vec![
            legacy_integer_setting(
                aeris_study_sdk::BUILTIN_STOCHASTIC_K_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_STOCHASTIC_DEFAULT_K_PERIOD,
            ),
            legacy_integer_setting(
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

fn legacy_bollinger_contract() -> (
    &'static str,
    u32,
    Vec<WorkspaceStudySettingState>,
    Vec<String>,
) {
    (
        aeris_study_sdk::BUILTIN_BOLLINGER_IDENTIFIER,
        aeris_study_sdk::BUILTIN_BOLLINGER_IMPLEMENTATION_REVISION,
        vec![
            legacy_integer_setting(
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

fn legacy_runtime_study(
    local_id: u64,
    indicator: ChartIndicator,
    visible: bool,
) -> Option<WorkspaceChartStudyState> {
    let (identifier, implementation_revision, settings, output_identifiers) =
        legacy_runtime_study_contract(indicator)?;
    Some(WorkspaceChartStudyState {
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

fn persisted_runtime_studies(
    restored_chart_state: Option<&WorkspaceChartState>,
) -> Vec<PendingRuntimeStudyState> {
    let Some(state) = restored_chart_state else {
        return Vec::new();
    };
    let mut persisted = state.studies.clone();
    let mut durable_identifiers = std::collections::BTreeMap::<String, usize>::new();
    for study in &persisted {
        *durable_identifiers
            .entry(study.identifier.clone())
            .or_default() += 1;
    }
    let mut used_local_ids = persisted
        .iter()
        .map(|study| study.local_id)
        .collect::<Vec<_>>();
    for item in &state.indicators {
        let Some(indicator) = ChartIndicator::from_identifier(&item.kind) else {
            continue;
        };
        if !runtime_managed_indicator(indicator) {
            continue;
        }
        let mut local_id = 1_u64;
        while used_local_ids.contains(&local_id) {
            let Some(next) = local_id.checked_add(1) else {
                break;
            };
            local_id = next;
        }
        if used_local_ids.contains(&local_id) {
            continue;
        }
        let Some(legacy) = legacy_runtime_study(local_id, indicator, item.visible) else {
            continue;
        };
        if let Some(remaining) = durable_identifiers.get_mut(&legacy.identifier)
            && *remaining > 0
        {
            *remaining -= 1;
            continue;
        }
        used_local_ids.push(local_id);
        persisted.push(legacy);
    }
    persisted
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

fn restored_runtime_studies(
    restored_chart_state: Option<&WorkspaceChartState>,
) -> RuntimeStudiesState {
    let deferred = persisted_runtime_studies(restored_chart_state);
    let next_local_id = deferred
        .iter()
        .map(|state| state.persisted.local_id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .unwrap_or(0);
    RuntimeStudiesState {
        deferred,
        next_local_id,
        ..RuntimeStudiesState::default()
    }
}

impl WorkspaceSurface {
    pub(super) fn take_chart_persistence_dirty(&mut self) -> bool {
        std::mem::take(&mut self.chart_persistence_dirty)
    }

    pub(super) fn workspace_chart_state(&self, cx: &App) -> Option<WorkspaceChartState> {
        let Some(chart) = self.chart.as_ref() else {
            return self.restored_chart_state.clone();
        };
        let chart = chart.read(cx);
        if !chart.has_market_data() {
            return self.restored_chart_state.clone();
        }
        let nucleus_state_json = match chart.export_semantic_state_json() {
            Ok(state) => state,
            Err(error) => {
                eprintln!("Aeris drawings could not be serialized: {error}");
                return self.restored_chart_state.clone();
            }
        };
        let price_axis = chart
            .price_axis_menu_state(0, false)
            .or_else(|| chart.price_axis_menu_state(0, true))
            .map(|state| WorkspacePriceAxisState {
                flags: u32::from(state.flags),
                mode: u32::from(state.mode),
                left: state.left,
                precision: state.precision.map(u32::from),
            });
        let indicators = persisted_legacy_indicator_states(chart.indicator_states());
        let studies = persisted_runtime_study_states(&self.studies, |study_id| {
            chart.study_visible(study_id.get())
        });
        Some(WorkspaceChartState {
            chart_type: chart.chart_type().identifier().to_string(),
            nucleus_state_json,
            indicators,
            price_axis,
            locked_drawing_ids: chart.locked_drawing_ids(),
            crosshair_mode: u32::from(chart.crosshair_mode()),
            price_alerts: self.price_alerts.clone(),
            studies,
            appearance: Some(persisted_chart_appearance(&chart.appearance_settings())),
        })
    }

    fn apply_restored_chart_state(
        chart: &Entity<NucleusChartView>,
        state: &WorkspaceChartState,
        restore_drawings: bool,
        cx: &mut Context<Self>,
    ) {
        let indicators = state
            .indicators
            .iter()
            .filter_map(|item| {
                ChartIndicator::from_identifier(&item.kind).map(|indicator| ChartIndicatorState {
                    indicator,
                    visible: item.visible,
                })
            })
            .filter(|state| !runtime_managed_indicator(state.indicator))
            .take(ChartIndicator::ALL.len())
            .collect::<Vec<_>>();
        let chart_type = ChartType::from_identifier(&state.chart_type);
        let price_axis = state.price_axis.as_ref().and_then(|axis| {
            Some(PriceAxisMenuState {
                flags: u16::try_from(axis.flags).ok()?,
                mode: u8::try_from(axis.mode).ok()?.min(3),
                left: axis.left,
                precision: axis.precision.and_then(|value| u8::try_from(value).ok()),
            })
        });
        let drawing_json = state.nucleus_state_json.clone();
        let locked = state.locked_drawing_ids.clone();
        let crosshair_mode = u8::try_from(state.crosshair_mode)
            .ok()
            .filter(|mode| *mode <= 3);
        let appearance = state
            .appearance
            .as_ref()
            .and_then(restored_chart_appearance);
        chart.update(cx, |chart, _| {
            if let Some(chart_type) = chart_type {
                chart.set_chart_type(chart_type);
            }
            if let Some(appearance) = &appearance {
                let _ = chart.set_appearance_settings(appearance);
            }
            if let Err(error) = chart.restore_indicator_states(&indicators) {
                eprintln!("Aeris persisted indicators could not be restored: {error}");
            }
            if restore_drawings
                && !drawing_json.is_empty()
                && let Err(error) = chart.import_semantic_state_json(&drawing_json, &locked)
            {
                eprintln!("Aeris persisted drawings could not be restored: {error}");
            }
            if let Some(price_axis) = price_axis {
                let _ = chart.restore_price_axis_menu_state(price_axis);
            }
            if let Some(mode) = crosshair_mode {
                let _ = chart.set_crosshair_mode(mode);
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        cx: &mut Context<Self>,
        startup: MarketWorkerStartup,
        market_worker: MarketDataWorker,
        lifecycle: DesktopLifecycle,
        symbol_input: Option<Entity<InputState>>,
        indicator_input: Entity<InputState>,
        timeframe_input: Entity<InputState>,
        chart_chrome: chart_chrome::ChartChromePreferences,
        restored_chart_state: Option<WorkspaceChartState>,
    ) -> Self {
        let theme = AerisTheme::dark();
        let restored_rithmic = restored_market_selection(&startup);
        let symbol_browser = initial_symbol_browser(&startup);
        let TerminalStartupState {
            chart,
            chart_state,
            chart_state_message,
            replay_label,
            worker_label,
            subscription_id,
            connection_state,
            connection_message,
            provider,
            product,
        } = terminal_startup_state(startup, cx);
        let interval = restored_rithmic.map_or(ChartInterval::Minute1, |restored| restored.0);
        let studies = restored_runtime_studies(restored_chart_state.as_ref());
        let (price_alerts, price_alert_message) =
            restore_price_alerts(restored_chart_state.as_ref(), &market_worker);
        initialize_chart_chrome(chart.as_ref(), chart_chrome, cx);
        replace_chart_price_alert_lines(chart.as_ref(), &price_alerts, product.as_ref(), cx);
        let bridge_label = chart_bridge_label(chart.as_ref(), cx);
        observe_chart(chart.as_ref(), cx);
        let order_book = cx.new(move |_| ReadOnlyOrderBookView::new(theme));
        Self {
            chart,
            order_book,
            side_panels: SidePanelVisibility::default(),
            side_panel_width: SIDE_PANEL_INITIAL_WIDTH,
            side_panel_split_basis_points: 5_000,
            menu_state: WorkspaceMenuState::default(),
            scrolls: WorkspaceScrollHandles::default(),
            chart_state,
            chart_state_message,
            theme,
            replay_label,
            worker_label,
            subscription_id,
            bridge_label,
            market_worker,
            lifecycle,
            pending_ui_diagnostics: None,
            connection_state,
            connection_message,
            provider_transport_rtt_nanos: None,
            trading_pnl: TradingPnlState::default(),
            symbol_browser,
            symbol_message: initial_symbol_message(provider),
            market_state: WorkspaceMarketState::default(),
            symbol_selection_target: SymbolSelectionTarget::Chart,
            pending_symbol_selection_target: None,
            pending_watchlist_instrument: None,
            series_message: "Select a symbol before choosing a series".to_string(),
            symbol_input,
            indicator_input,
            timeframe_input,
            indicator_message: None,
            studies,
            study_settings_dialog: None,
            chrome_overlay: None,
            chrome_overlay_phase: ChromeOverlayPhase::Opening,
            chrome_overlay_generation: 0,
            chrome_overlay_trigger_position: None,
            timeframe_menu_flyout: None,
            timeframe_flyout_close_token: 0,
            timeframe_hover_regions: 0,
            timeframe_trigger_bounds: None,
            chart_type_trigger_bounds: None,
            chrome_selection: 0,
            chrome_focus: cx.focus_handle().tab_stop(true),
            provider,
            product,
            rithmic_switch: RithmicSwitchState::Idle,
            interval,
            rithmic_pending_interval: None,
            rithmic_pending_product: None,
            rithmic_pending_sequence: None,
            rithmic_previous_selection: None,
            restored_viewport: restored_rithmic.and_then(|restored| restored.1),
            last_persisted_viewport: None,
            pending_chart_context_menu: None,
            pending_pane_activate: PaneActivationRequest::None,
            pending_study_settings_request: None,
            pending_study_remove_request: None,
            resource_class: ConsumerResourceClass::Foreground,
            chart_chrome,
            retained_chart_presentation: RetainedChartPresentation::default(),
            restored_chart_state,
            chart_persistence_dirty: false,
            last_chart_user_state_revision: 0,
            price_alerts,
            price_alert_dialog: None,
            price_alert_message,
            #[cfg(feature = "diagnostics")]
            foreground_interactions: ForegroundInteractionDiagnostics::default(),
            #[cfg(feature = "diagnostics")]
            live_evidence_enabled: std::env::var_os("AERIS_LIVE_EVIDENCE").is_some(),
            #[cfg(feature = "diagnostics")]
            live_evidence_publications: 0,
        }
    }

    pub(super) fn retire_market_worker(&mut self, cx: &App) {
        if let Some(retirement) = self.market_worker.begin_retirement() {
            self.lifecycle.retire_market_worker(retirement, cx);
        }
    }

    pub(super) fn set_market_resource_class(&mut self, resource_class: ConsumerResourceClass) {
        self.resource_class = resource_class;
        let _ = self
            .market_worker
            .try_set_market_resource_class(resource_class);
    }

    pub(super) fn set_market_message_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.market_worker.set_message_wake(wake);
    }

    pub(super) fn should_poll_market(&self) -> bool {
        self.resource_class == ConsumerResourceClass::Foreground
    }

    pub(super) fn available_intervals(&self) -> &'static [ChartInterval] {
        match self.provider {
            TerminalProvider::Rithmic => RITHMIC_INTERVALS,
            TerminalProvider::Hyperliquid => HYPERLIQUID_INTERVALS,
        }
    }

    pub(super) fn quick_timeframe_matches(&self, cx: &App) -> Vec<ChartInterval> {
        let query = self.timeframe_input.read(cx).value();
        self.available_intervals()
            .iter()
            .copied()
            .filter(|interval| interval.matches_typeahead(query.as_ref()))
            .collect()
    }

    fn sync_timeframe_menu_selection(&mut self) {
        self.timeframe_menu_flyout = None;
        self.menu_state.timeframe_flyout_keyboard = false;
        self.timeframe_hover_regions = 0;
        let selected_group = timeframe_interval_group(self.selected_interval());
        self.chrome_selection = timeframe_menu_groups(self.available_intervals())
            .iter()
            .position(|group| *group == selected_group)
            .unwrap_or(0);
    }

    pub(super) fn open_timeframe_group(
        &mut self,
        group: TimeframeMenuGroup,
        from_keyboard: bool,
        cx: &mut Context<Self>,
    ) {
        self.retain_timeframe_flyout();
        let already_open = self.timeframe_menu_flyout == Some(group);
        if !already_open {
            self.timeframe_menu_flyout = Some(group);
            self.chrome_selection = timeframe_group_intervals(group, self.available_intervals())
                .iter()
                .position(|interval| *interval == self.selected_interval())
                .unwrap_or(0);
        }
        if self.menu_state.timeframe_flyout_keyboard != from_keyboard || !already_open {
            self.menu_state.timeframe_flyout_keyboard = from_keyboard;
            cx.notify();
        }
    }

    fn retain_timeframe_flyout(&mut self) {
        self.timeframe_flyout_close_token = self.timeframe_flyout_close_token.saturating_add(1);
    }

    fn arm_timeframe_flyout_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.retain_timeframe_flyout();
        let token = self.timeframe_flyout_close_token;
        cx.spawn_in(window, async move |app, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let _ = app.update_in(cx, |app, _, app_cx| {
                if app.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && app.timeframe_flyout_close_token == token
                {
                    app.close_timeframe_flyout(app_cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn hover_timeframe_menu_region(
        &mut self,
        hovered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            self.timeframe_hover_regions = self.timeframe_hover_regions.saturating_add(1);
            self.retain_timeframe_flyout();
            return;
        }
        if self.timeframe_hover_regions == 0 {
            return;
        }
        self.timeframe_hover_regions -= 1;
        if self.timeframe_hover_regions == 0 {
            self.arm_timeframe_flyout_close(window, cx);
        }
    }

    fn close_timeframe_flyout(&mut self, cx: &mut Context<Self>) {
        let Some(group) = self.timeframe_menu_flyout.take() else {
            return;
        };
        self.menu_state.timeframe_flyout_keyboard = false;
        self.timeframe_hover_regions = 0;
        self.chrome_selection = timeframe_menu_groups(self.available_intervals())
            .iter()
            .position(|item| *item == group)
            .unwrap_or(0);
        cx.notify();
    }

    fn timeframe_menu_keyboard_count(&self) -> usize {
        if let Some(group) = self.timeframe_menu_flyout {
            timeframe_group_intervals(group, self.available_intervals()).len()
        } else {
            timeframe_menu_groups(self.available_intervals()).len()
        }
    }

    fn sync_chart_type_menu_selection(&mut self, cx: &App) {
        let selected = self.chart_type(cx);
        self.chrome_selection = ChartType::ALL
            .iter()
            .position(|chart_type| *chart_type == selected)
            .unwrap_or(0);
    }

    fn apply_highlighted_chart_type(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chart_type) = ChartType::ALL.get(self.chrome_selection).copied() {
            self.set_chart_type(chart_type, cx);
            self.close_chrome_overlay(window, cx);
        }
    }

    pub(super) fn sync_quick_timeframe_selection(&mut self, cx: &App) {
        let query = self.timeframe_input.read(cx).value();
        let intervals = self.quick_timeframe_matches(cx);
        self.chrome_selection = intervals
            .iter()
            .position(|interval| interval.label() == query.as_ref())
            .or_else(|| {
                intervals
                    .iter()
                    .position(|interval| *interval == self.selected_interval())
            })
            .unwrap_or(0);
    }

    pub(super) fn apply_highlighted_interval(
        &mut self,
        intervals: &[ChartInterval],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(interval) = intervals.get(self.chrome_selection).copied()
            && self.select_interval(interval, cx)
        {
            self.close_chrome_overlay(window, cx);
        }
    }

    pub(super) fn selected_interval(&self) -> ChartInterval {
        self.interval
    }

    pub(super) fn select_interval(
        &mut self,
        interval: ChartInterval,
        cx: &mut Context<Self>,
    ) -> bool {
        let starter_interval = matches!(
            interval,
            ChartInterval::Minute1
                | ChartInterval::Minute3
                | ChartInterval::Minute5
                | ChartInterval::Minute15
                | ChartInterval::Minute30
                | ChartInterval::Hour1
                | ChartInterval::Hour2
                | ChartInterval::Hour4
                | ChartInterval::Hour8
                | ChartInterval::Hour12
                | ChartInterval::Day1
        );
        if !current_plan_limits().extended_timeframes && !starter_interval {
            self.series_message = "This timeframe requires a paid plan".to_string();
            cx.notify();
            return false;
        }
        #[cfg(feature = "diagnostics")]
        let started = Instant::now();
        let selected = (|| {
            if self.interval == interval && self.rithmic_pending_interval.is_none() {
                return true;
            }
            if self.rithmic_pending_interval == Some(interval) {
                return true;
            }
            let Some(product) = self.product.clone() else {
                let display = terminal_provider_display(self.provider);
                self.series_message = format!("{display} market selection is unavailable");
                cx.notify();
                return false;
            };
            let Ok(sequence) = self.market_worker.try_select_engine(product, interval) else {
                self.series_message = format!("{} history could not start", interval.label());
                cx.notify();
                return false;
            };
            self.rithmic_pending_interval = Some(interval);
            self.rithmic_pending_sequence = Some(sequence);
            self.rithmic_switch = RithmicSwitchState::Pending;
            self.chart_state = ChartState::Loading;
            self.chart_state_message = format!("Loading {} history", interval.label());
            self.series_message = format!("Switching to {}", interval.label());
            cx.notify();
            true
        })();
        #[cfg(feature = "diagnostics")]
        self.foreground_interactions
            .record_interval_selection(elapsed_nanos(started));
        selected
    }

    pub(super) fn select_installed_instrument(
        &mut self,
        instrument: &InstallProviderInstrument,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.product.as_ref().is_some_and(|current| {
            current.provider == instrument.provider
                && current.instrument_id == instrument.instrument_id
        }) && self.rithmic_pending_product.is_none()
        {
            return true;
        }
        if self
            .rithmic_pending_product
            .as_ref()
            .is_some_and(|pending| {
                pending.provider == instrument.provider
                    && pending.instrument_id == instrument.instrument_id
            })
        {
            return true;
        }
        let interval = self.interval;
        let Ok(sequence) = self
            .market_worker
            .try_select_engine(instrument.clone(), interval)
        else {
            self.series_message = format!(
                "{} market history could not start",
                instrument.display_symbol
            );
            cx.notify();
            return false;
        };
        self.rithmic_pending_product = Some(instrument.clone());
        self.rithmic_pending_interval = Some(interval);
        self.rithmic_pending_sequence = Some(sequence);
        self.rithmic_switch = RithmicSwitchState::Pending;
        self.chart_state = ChartState::Loading;
        self.chart_state_message = format!("Loading {} market history", instrument.display_symbol);
        self.series_message = format!("Switching to {}", instrument.display_symbol);
        cx.notify();
        true
    }

    pub(super) fn instrument_entries(&self, _cx: &App) -> Vec<InstrumentMenuEntry> {
        self.symbol_browser
            .results()
            .iter()
            .enumerate()
            .map(|(index, instrument)| InstrumentMenuEntry {
                label: instrument.display_symbol.clone(),
                checked: self.symbol_browser.selected().is_some_and(|selected| {
                    selected.instrument.symbol == instrument.symbol
                        && selected.instrument.exchange == instrument.exchange
                }),
                selection: match self.provider {
                    TerminalProvider::Rithmic => InstrumentMenuSelection::Rithmic(index),
                    TerminalProvider::Hyperliquid => InstrumentMenuSelection::Hyperliquid(index),
                },
            })
            .collect()
    }

    fn activate_chrome_list_keyboard(&mut self) {
        if matches!(
            self.chrome_overlay,
            Some(ChromeOverlay::Instrument | ChromeOverlay::Indicator)
        ) {
            self.menu_state.chrome_list_keyboard = true;
        }
    }

    fn scroll_chrome_selection_into_view(&self, cx: &App) {
        match self.chrome_overlay {
            Some(ChromeOverlay::Instrument) if !self.symbol_browser.results().is_empty() => {
                self.scrolls
                    .instrument
                    .scroll_to_item(self.chrome_selection);
            }
            Some(ChromeOverlay::Indicator)
                if !chart_chrome::filter_indicator_specs(
                    self.indicator_input.read(cx).value().as_ref(),
                )
                .is_empty() =>
            {
                // The group heading is the scroll body's first child.
                self.scrolls
                    .indicator
                    .scroll_to_item(self.chrome_selection.saturating_add(1));
            }
            _ => {}
        }
    }

    fn sync_instrument_menu_keyboard(&mut self, cx: &App) {
        if self.menu_state.chrome_list_keyboard {
            return;
        }
        self.chrome_selection =
            current_instrument_menu_index(&self.instrument_entries(cx)).unwrap_or(0);
    }

    pub(super) fn select_instrument(
        &mut self,
        selection: InstrumentMenuSelection,
        target: SymbolSelectionTarget,
        cx: &mut Context<Self>,
    ) -> bool {
        if target == SymbolSelectionTarget::Watchlist && self.market_state.symbol_selection_pending
        {
            return false;
        }
        #[cfg(feature = "diagnostics")]
        let started = Instant::now();
        let selected = (|| match selection {
            InstrumentMenuSelection::Rithmic(index)
            | InstrumentMenuSelection::Hyperliquid(index) => {
                let provider = terminal_provider_id(self.provider);
                let entitlement_id = match self.provider {
                    TerminalProvider::Rithmic => RITHMIC_ENTITLEMENT_ID,
                    TerminalProvider::Hyperliquid => HYPERLIQUID_ENTITLEMENT_ID,
                };
                let display = terminal_provider_display(self.provider);
                let Some(selection) = self.symbol_browser.select(index) else {
                    return false;
                };
                let request = SelectProviderInstrument {
                    consumer_id: 0,
                    selection_generation: selection.generation.get() as u64,
                    search_generation: selection.search_generation.get() as u64,
                    provider: provider.to_string(),
                    symbol: selection.instrument.symbol.clone(),
                    exchange: selection.instrument.exchange.clone(),
                    entitlement_id: entitlement_id.to_string(),
                };
                if self.market_worker.try_select_provider(request).is_err() {
                    self.symbol_browser.reject_selection(selection.generation);
                    self.symbol_message = format!("{display} market selection is busy; try again");
                    cx.notify();
                    return false;
                }
                self.pending_symbol_selection_target = Some(target);
                self.market_state.symbol_selection_pending = true;
                self.symbol_message = format!("Selecting {}", selection.instrument.display_symbol);
                cx.notify();
                true
            }
        })();
        #[cfg(feature = "diagnostics")]
        self.foreground_interactions
            .record_instrument_selection(elapsed_nanos(started));
        selected
    }

    pub(super) fn open_chrome_overlay(
        &mut self,
        overlay: ChromeOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_chrome_overlay_with_trigger(overlay, None, window, cx);
    }

    pub(super) fn open_chrome_overlay_at(
        &mut self,
        overlay: ChromeOverlay,
        trigger_position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_chrome_overlay_with_trigger(overlay, Some(trigger_position), window, cx);
    }

    fn open_chrome_overlay_with_trigger(
        &mut self,
        overlay: ChromeOverlay,
        trigger_position: Option<gpui::Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if overlay == ChromeOverlay::Instrument {
            self.symbol_selection_target = SymbolSelectionTarget::Chart;
        }
        self.chrome_overlay_trigger_position = trigger_position;
        self.chrome_overlay_generation = self.chrome_overlay_generation.saturating_add(1);
        self.chrome_overlay_phase = ChromeOverlayPhase::Opening;
        self.chrome_overlay = Some(overlay);
        if overlay != ChromeOverlay::QuickTimeframe {
            self.timeframe_input.update(cx, |input, input_cx| {
                input.set_value("", window, input_cx);
            });
        }
        if overlay != ChromeOverlay::Timeframe {
            self.timeframe_menu_flyout = None;
            self.menu_state.timeframe_flyout_keyboard = false;
            self.timeframe_hover_regions = 0;
        }
        self.menu_state.chrome_list_keyboard = false;
        self.chrome_selection = match overlay {
            ChromeOverlay::Timeframe => {
                self.sync_timeframe_menu_selection();
                self.chrome_selection
            }
            ChromeOverlay::ChartType => {
                self.sync_chart_type_menu_selection(cx);
                self.chrome_selection
            }
            ChromeOverlay::QuickTimeframe => {
                self.sync_quick_timeframe_selection(cx);
                self.chrome_selection
            }
            ChromeOverlay::Instrument => {
                current_instrument_menu_index(&self.instrument_entries(cx)).unwrap_or(0)
            }
            ChromeOverlay::Indicator => 0,
        };
        match overlay {
            ChromeOverlay::Instrument => {
                self.scrolls.instrument.set_offset(point(px(0.0), px(0.0)));
                if let Some(input) = &self.symbol_input {
                    input.update(cx, |input, input_cx| input.focus(window, input_cx));
                }
                self.refresh_default_instrument_listing(cx);
                self.sync_instrument_menu_keyboard(cx);
            }
            ChromeOverlay::Indicator => {
                self.scrolls.indicator.set_offset(point(px(0.0), px(0.0)));
                self.indicator_input
                    .update(cx, |input, input_cx| input.focus(window, input_cx));
            }
            ChromeOverlay::QuickTimeframe => {
                self.timeframe_input
                    .update(cx, |input, input_cx| input.focus(window, input_cx));
            }
            ChromeOverlay::Timeframe | ChromeOverlay::ChartType => {
                self.chrome_focus.focus(window, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn close_chrome_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chrome_overlay.is_none() || self.chrome_overlay_phase == ChromeOverlayPhase::Closing
        {
            return;
        }
        if self.chrome_overlay == Some(ChromeOverlay::Instrument) {
            self.symbol_selection_target = instrument_target_after_close(
                self.symbol_selection_target,
                self.market_state.symbol_selection_pending,
            );
        }
        match self.chrome_overlay {
            Some(ChromeOverlay::Indicator) => {
                self.indicator_input.update(cx, |input, input_cx| {
                    input.set_value("", window, input_cx);
                });
            }
            Some(ChromeOverlay::QuickTimeframe) => {
                self.timeframe_input.update(cx, |input, input_cx| {
                    input.set_value("", window, input_cx);
                });
            }
            Some(
                ChromeOverlay::Instrument | ChromeOverlay::Timeframe | ChromeOverlay::ChartType,
            )
            | None => {}
        }
        self.chrome_focus.focus(window, cx);
        if cx.reduce_motion() {
            self.chrome_overlay = None;
            self.timeframe_menu_flyout = None;
            self.menu_state.timeframe_flyout_keyboard = false;
            self.menu_state.chrome_list_keyboard = false;
            self.timeframe_hover_regions = 0;
            cx.notify();
            return;
        }

        self.chrome_overlay_generation = self.chrome_overlay_generation.saturating_add(1);
        self.chrome_overlay_phase = ChromeOverlayPhase::Closing;
        let generation = self.chrome_overlay_generation;
        cx.spawn_in(window, async move |app, cx| {
            cx.background_executor()
                .timer(CHROME_OVERLAY_EXIT_DURATION)
                .await;
            let _ = app.update_in(cx, |app, _, app_cx| {
                if should_finish_chrome_overlay_close(
                    app.chrome_overlay_phase,
                    app.chrome_overlay_generation,
                    generation,
                ) {
                    app.chrome_overlay = None;
                    app.timeframe_menu_flyout = None;
                    app.menu_state.timeframe_flyout_keyboard = false;
                    app.menu_state.chrome_list_keyboard = false;
                    app.timeframe_hover_regions = 0;
                    app.chrome_overlay_phase = ChromeOverlayPhase::Opening;
                    app_cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn on_terminal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(command) =
            fullscreen_escape_command(event.keystroke.key.as_str(), window.is_fullscreen())
        {
            command.execute(window);
            return true;
        }
        if self.consume_chrome_typeahead(event, window, cx) {
            return true;
        }
        if self.chrome_overlay.is_none() || self.chrome_overlay_phase == ChromeOverlayPhase::Closing
        {
            return false;
        }
        match event.keystroke.key.as_str() {
            "escape" => {
                if self.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && self.timeframe_menu_flyout.is_some()
                {
                    self.close_timeframe_flyout(cx);
                } else {
                    self.close_chrome_overlay(window, cx);
                }
            }
            "up" => {
                if self.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && self.timeframe_menu_flyout.is_some()
                    && !self.menu_state.timeframe_flyout_keyboard
                {
                    self.menu_state.timeframe_flyout_keyboard = true;
                    cx.notify();
                    return true;
                }
                self.activate_chrome_list_keyboard();
                self.chrome_selection = self.chrome_selection.saturating_sub(1);
                self.scroll_chrome_selection_into_view(cx);
                cx.notify();
            }
            "down" => {
                if self.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && self.timeframe_menu_flyout.is_some()
                    && !self.menu_state.timeframe_flyout_keyboard
                {
                    self.menu_state.timeframe_flyout_keyboard = true;
                    cx.notify();
                    return true;
                }
                let count = match self.chrome_overlay {
                    Some(ChromeOverlay::Instrument) => self.instrument_entries(cx).len(),
                    Some(ChromeOverlay::Indicator) => chart_chrome::filter_indicator_specs(
                        self.indicator_input.read(cx).value().as_ref(),
                    )
                    .len(),
                    Some(ChromeOverlay::Timeframe) => self.timeframe_menu_keyboard_count(),
                    Some(ChromeOverlay::ChartType) => ChartType::ALL.len(),
                    Some(ChromeOverlay::QuickTimeframe) => self.quick_timeframe_matches(cx).len(),
                    None => 0,
                };
                self.activate_chrome_list_keyboard();
                self.chrome_selection = (self.chrome_selection + 1).min(count.saturating_sub(1));
                self.scroll_chrome_selection_into_view(cx);
                cx.notify();
            }
            "left" if self.chrome_overlay == Some(ChromeOverlay::Timeframe) => {
                self.close_timeframe_flyout(cx);
            }
            "right" if self.chrome_overlay == Some(ChromeOverlay::Timeframe) => {
                if self.timeframe_menu_flyout.is_none()
                    && let Some(group) = timeframe_menu_groups(self.available_intervals())
                        .get(self.chrome_selection)
                        .copied()
                {
                    self.open_timeframe_group(group, true, cx);
                }
            }
            "enter" => return self.handle_chrome_enter(window, cx),
            _ => return false,
        }
        true
    }

    fn handle_chrome_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.chrome_overlay {
            Some(ChromeOverlay::Timeframe) => {
                if let Some(group) = self.timeframe_menu_flyout {
                    if self.menu_state.timeframe_flyout_keyboard {
                        let intervals =
                            timeframe_group_intervals(group, self.available_intervals());
                        self.apply_highlighted_interval(&intervals, window, cx);
                    } else {
                        self.menu_state.timeframe_flyout_keyboard = true;
                        cx.notify();
                    }
                } else if let Some(group) = timeframe_menu_groups(self.available_intervals())
                    .get(self.chrome_selection)
                    .copied()
                {
                    self.open_timeframe_group(group, true, cx);
                }
            }
            Some(ChromeOverlay::ChartType) => self.apply_highlighted_chart_type(window, cx),
            Some(ChromeOverlay::QuickTimeframe) => {
                // InputState emits PressEnter before propagating the raw key.
                // Its subscription owns submission: handling both would submit
                // again after closing clears the query, selecting another interval.
                return false;
            }
            Some(ChromeOverlay::Instrument | ChromeOverlay::Indicator) | None => return false,
        }
        true
    }

    fn consume_chrome_typeahead(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if chrome_typeahead_blocked(event) {
            return false;
        }
        if self.chrome_overlay_phase == ChromeOverlayPhase::Closing {
            return false;
        }
        if self.drawing_toolbar_state(cx).active_tool == ChartDrawingTool::Text {
            return false;
        }
        if self
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).is_editing_text())
        {
            return false;
        }
        let Some(typed) = chrome_typeahead_char(event) else {
            return false;
        };
        if event.is_held && self.chrome_overlay.is_none() {
            return false;
        }
        match self.chrome_overlay {
            None if typed.is_ascii_digit() => {
                self.begin_quick_timeframe(typed, window, cx);
                true
            }
            None if typed.is_ascii_alphabetic() => self.begin_symbol_typeahead(typed, window, cx),
            Some(
                ChromeOverlay::Instrument
                | ChromeOverlay::Indicator
                | ChromeOverlay::Timeframe
                | ChromeOverlay::QuickTimeframe
                | ChromeOverlay::ChartType,
            )
            | None => false,
        }
    }

    fn begin_quick_timeframe(&mut self, typed: char, window: &mut Window, cx: &mut Context<Self>) {
        self.timeframe_input.update(cx, |input, input_cx| {
            input.set_value(typed.to_string(), window, input_cx);
            input.focus(window, input_cx);
        });
        self.open_chrome_overlay(ChromeOverlay::QuickTimeframe, window, cx);
    }

    fn begin_symbol_typeahead(
        &mut self,
        typed: char,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(input) = self.symbol_input.clone() else {
            return false;
        };
        let query = typed.to_string();
        input.update(cx, |input, input_cx| {
            input.set_value(&query, window, input_cx);
            input.focus(window, input_cx);
        });
        self.search_symbol_query(&query, cx);
        self.open_chrome_overlay(ChromeOverlay::Instrument, window, cx);
        true
    }

    #[cfg(feature = "diagnostics")]
    fn record_live_evidence_publication(&mut self, update: &ReplayStreamUpdate) {
        if !self.live_evidence_enabled || self.live_evidence_publications >= 256 {
            return;
        }
        self.live_evidence_publications = self.live_evidence_publications.saturating_add(1);
        match update {
            ReplayStreamUpdate::Snapshot(snapshot) => {
                let interval_nanos = (snapshot.bar_definition().interval_seconds > 0).then(|| {
                    i64::from(snapshot.bar_definition().interval_seconds)
                        .saturating_mul(1_000_000_000)
                });
                let interior_gaps = interval_nanos.map(|interval_nanos| {
                    snapshot
                        .bars()
                        .windows(2)
                        .filter(|pair| {
                            pair[1]
                                .provenance()
                                .exchange_timestamp_unix_nanos
                                .saturating_sub(pair[0].provenance().exchange_timestamp_unix_nanos)
                                != interval_nanos
                        })
                        .count()
                });
                let first_timestamp = snapshot
                    .bars()
                    .first()
                    .map_or(0, |bar| bar.provenance().exchange_timestamp_unix_nanos);
                let last_timestamp = snapshot
                    .bars()
                    .last()
                    .map_or(0, |bar| bar.provenance().exchange_timestamp_unix_nanos);
                let interior_gaps =
                    interior_gaps.map_or_else(|| "null".to_string(), |value| value.to_string());
                let interval_nanos =
                    interval_nanos.map_or_else(|| "null".to_string(), |value| value.to_string());
                eprintln!(
                    "AERIS_LIVE_SNAPSHOT {{\"bar_count\":{},\"first_timestamp\":{first_timestamp},\"last_timestamp\":{last_timestamp},\"interior_gaps\":{interior_gaps},\"interval_nanos\":{interval_nanos}}}",
                    snapshot.bars().len()
                );
            }
            ReplayStreamUpdate::Delta(delta) => eprintln!(
                "AERIS_LIVE_UPDATE {{\"kind\":\"delta\",\"timestamp\":{}}}",
                delta.item().provenance().exchange_timestamp_unix_nanos
            ),
            ReplayStreamUpdate::Tail(tail) => eprintln!(
                "AERIS_LIVE_UPDATE {{\"kind\":\"tail\",\"timestamp\":{},\"forming\":{}}}",
                tail.item().provenance().exchange_timestamp_unix_nanos,
                tail.forming()
            ),
        }
    }

    pub(super) fn apply_publication(
        &mut self,
        publication: MarketWorkerPublication,
        cx: &mut Context<Self>,
    ) {
        #[cfg(feature = "diagnostics")]
        self.record_live_evidence_publication(&publication.update);
        let MarketWorkerPublication {
            update,
            generation,
            subscription_id,
            worker_label,
            ui_diagnostics,
        } = publication;
        self.worker_label = worker_label;
        self.subscription_id = subscription_id;
        self.replay_label =
            generation_status(&self.worker_label, &self.subscription_id, generation);
        // A switch that has been committed but not yet drawn keeps the previous
        // chart on screen. That chart belongs to the previous series, so the
        // replacement's incremental updates must not reach it; only its covering
        // snapshot may, and that snapshot is what swaps the chart.
        let swapping = self.rithmic_switch.is_swapping();
        let next_state = match (&self.chart, update) {
            (existing, aeris_application::ReplayStreamUpdate::Snapshot(snapshot))
                if existing.is_none() || swapping =>
            {
                let chart_theme = nucleus_chart_theme(self.theme.mode);
                let chart = cx
                    .new(move |_| NucleusChartView::with_replay_and_theme(&snapshot, chart_theme));
                self.apply_chart_chrome_to_chart(&chart, cx);
                if let Some(restored) = self.restored_chart_state.take() {
                    Self::apply_restored_chart_state(&chart, &restored, true, cx);
                } else {
                    self.apply_retained_chart_state_to_chart(&chart, cx);
                }
                replace_chart_price_alert_lines(
                    Some(&chart),
                    &self.price_alerts,
                    self.product.as_ref(),
                    cx,
                );
                if let Some((start, end)) = self.restored_viewport {
                    chart.update(cx, |chart, _| {
                        chart.set_visible_time_range_unix_nanos(start, end);
                    });
                }
                self.last_chart_user_state_revision = chart.read(cx).user_state_revision();
                observe_chart(Some(&chart), cx);
                self.chart = Some(chart);
                self.synchronize_runtime_studies(cx);
                self.rithmic_switch = RithmicSwitchState::Initializing;
                ChartState::Ready
            }
            (Some(_), _) if swapping => {
                // The replacement has not arrived yet; the previous chart stays
                // as it is rather than being fed another series' bars.
                return;
            }
            (Some(chart), update) => {
                let (accepted, recovery_pending) = chart.update(cx, |chart, _| {
                    let accepted = chart.try_queue_replay_update(update).is_ok();
                    if !accepted {
                        eprintln!("bounded chart queue overflowed; canonical resnapshot required");
                    }
                    (accepted, chart.replay_bridge_metrics().recovery_pending)
                });
                publication_chart_state(accepted, recovery_pending)
            }
            (None, _) => {
                self.reject_incremental_publication(ui_diagnostics, cx);
                return;
            }
        };
        if let Some(diagnostics) = ui_diagnostics
            && let Some(replaced) = self.pending_ui_diagnostics.replace(diagnostics)
        {
            self.market_worker
                .send_ui_diagnostics(UiDiagnosticsFeedback::Coalesced {
                    generation: replaced.generation(),
                });
        }
        // A publication says bars arrived, not that they are current. The engine
        // reports readiness separately, and while it is still loading current
        // coverage the chart is showing retained history — promoting it here is
        // what presented a stale chart as ready for the seconds before the
        // provider page and the live handoff landed.
        if next_state == ChartState::Ready && self.chart_state == ChartState::Loading {
            cx.notify();
        } else if next_state == ChartState::Ready {
            self.chart_state = ChartState::Ready;
            self.chart_state_message = "market snapshot is current".to_string();
            if self.provider == TerminalProvider::Rithmic {
                self.market_state.symbol_selection_pending = false;
                self.symbol_message = self.product.as_ref().map_or_else(
                    || "Rithmic market ready".to_string(),
                    |product| format!("{} · Rithmic spot", product.provider_symbol),
                );
            } else if self.provider == TerminalProvider::Hyperliquid {
                self.market_state.symbol_selection_pending = false;
                self.symbol_message = self.product.as_ref().map_or_else(
                    || "Hyperliquid market ready".to_string(),
                    |product| format!("{} · Hyperliquid", product.display_symbol),
                );
            }
        } else {
            self.set_chart_state(
                ChartState::Recovering,
                "chart update requires a correlated covering snapshot".to_string(),
                cx,
            );
        }
    }

    fn reject_incremental_publication(
        &mut self,
        diagnostics: Option<PendingUiDiagnostics>,
        cx: &mut Context<Self>,
    ) {
        if let Some(diagnostics) = diagnostics {
            self.market_worker
                .send_ui_diagnostics(UiDiagnosticsFeedback::Coalesced {
                    generation: diagnostics.generation(),
                });
        }
        self.set_chart_state(
            ChartState::Error,
            "market update arrived before the initial covering snapshot".to_string(),
            cx,
        );
    }

    fn apply_recovery(
        &mut self,
        request_id: u64,
        result: Result<MarketWorkerBootstrap, String>,
        cx: &mut Context<Self>,
    ) {
        let bootstrap = match result {
            Ok(bootstrap) => bootstrap,
            Err(error) => {
                let current = self.chart.as_ref().is_some_and(|chart| {
                    chart.update(cx, |chart, chart_cx| {
                        let current = chart.mark_replay_recovery_failed(request_id);
                        if current {
                            chart_cx.notify();
                        }
                        current
                    })
                });
                if !current {
                    return;
                }
                self.apply_market_state_message(ChartState::Error, error.clone(), cx);
                eprintln!("fixture recovery {request_id} failed: {error}");
                return;
            }
        };
        let Some(chart) = &self.chart else {
            self.set_chart_state(
                ChartState::Error,
                "recovery response arrived before the initial snapshot".to_string(),
                cx,
            );
            return;
        };
        let install = chart.update(cx, |chart, chart_cx| {
            let installed = chart.install_replay_recovery(request_id, &bootstrap.snapshot);
            chart_cx.notify();
            installed
        });
        match install {
            Ok(true) => {
                self.replay_label = generation_status(
                    &self.worker_label,
                    &bootstrap.subscription_id,
                    MarketPublicationGeneration::from_generation(&bootstrap.generation),
                );
                self.apply_market_state_message(
                    ChartState::Ready,
                    "market snapshot is current".to_string(),
                    cx,
                );
            }
            Ok(false) => {
                let metrics = chart.read(cx).replay_bridge_metrics();
                if metrics.snapshot_required && !metrics.recovery_pending {
                    self.apply_market_state_message(
                        ChartState::Error,
                        "chart recovery exhausted its snapshot retry budget".to_string(),
                        cx,
                    );
                }
            }
            Err(error) => {
                chart.update(cx, |chart, chart_cx| {
                    chart.mark_replay_recovery_failed(request_id);
                    chart_cx.notify();
                });
                self.apply_market_state_message(ChartState::Error, error.to_string(), cx);
                eprintln!("fixture recovery {request_id} was rejected: {error}");
            }
        }
    }

    fn mark_market_stream_invalid(&mut self, message: &str, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.mark_replay_stream_invalid();
                chart_cx.notify();
            });
        }
        eprintln!("market worker invalidated the stream: {message}");
    }

    /// Reports whether an engine selection handoff must keep the chart covered.
    ///
    /// Before the replacement snapshot arrives, the pixels belong to the series
    /// the trader just left. After that first snapshot arrives, the replacement
    /// can still be retained/partial history until the provider-history/live
    /// handoff declares it current. Both phases are deliberately hidden behind
    /// the neutral loading surface so a stale range cannot appear to "wake up"
    /// and jump when current coverage lands.
    pub(super) fn showing_superseded_series(&self) -> bool {
        match self.provider {
            TerminalProvider::Rithmic | TerminalProvider::Hyperliquid => {
                switch_requires_chart_cover(self.chart.is_some(), self.rithmic_switch)
            }
        }
    }

    fn set_chart_state(&mut self, state: ChartState, message: String, cx: &mut Context<Self>) {
        if matches!(state, ChartState::Stale | ChartState::Recovering) {
            self.mark_market_stream_invalid(&message, cx);
        }
        self.chart_state = state;
        self.chart_state_message = message;
        cx.notify();
    }

    fn dispatch_recovery(&mut self, cx: &mut Context<Self>) {
        if !self.market_worker.is_connected() {
            return;
        }
        let Some(chart) = &self.chart else {
            return;
        };
        let worker = &self.market_worker;
        let dispatch = chart.update(cx, |chart, chart_cx| {
            let result =
                chart.try_dispatch_replay_recovery(|command| worker.try_send_recovery(command));
            if result.as_ref().is_ok_and(|dispatched| *dispatched) {
                chart_cx.notify();
            }
            result
        });
        match dispatch {
            Ok(_) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(command)) => {
                self.market_worker.mark_disconnected();
                chart.update(cx, |chart, chart_cx| {
                    chart.mark_replay_recovery_failed(command.request_id);
                    chart_cx.notify();
                });
            }
        }
    }

    fn apply_market_worker_message(
        &mut self,
        message: MarketWorkerMessage,
        cx: &mut Context<Self>,
    ) {
        match message {
            MarketWorkerMessage::Update(publication) => {
                self.apply_publication(publication, cx);
            }
            MarketWorkerMessage::Diagnostics(snapshot) => {
                #[cfg(feature = "diagnostics")]
                eprintln!("desktop market diagnostics: {snapshot:?}");
                #[cfg(not(feature = "diagnostics"))]
                drop(snapshot);
            }
            MarketWorkerMessage::Recovery { request_id, result } => {
                self.apply_recovery(request_id, result, cx);
            }
            MarketWorkerMessage::State { state, message } => {
                self.apply_market_state_message(state, message, cx);
            }
            MarketWorkerMessage::EngineSwitchMarker { sequence } => {
                self.apply_rithmic_switch_marker(sequence, cx);
            }
            MarketWorkerMessage::Connection {
                state,
                message,
                transport_rtt_nanos,
            } => {
                self.apply_market_connection_message(state, message, transport_rtt_nanos, cx);
            }
            MarketWorkerMessage::ProviderCatalog(event) => {
                self.apply_provider_catalog_event(event, cx);
            }
            MarketWorkerMessage::OrderBook(frame) => {
                self.order_book.update(cx, |order_book, order_book_cx| {
                    order_book.replace_frame(frame, order_book_cx)
                });
            }
            MarketWorkerMessage::StudyOutput(snapshot) => {
                self.apply_study_output(&snapshot, cx);
            }
            MarketWorkerMessage::StudyOutputsInvalidated(invalidated) => {
                self.apply_study_invalidated(&invalidated, cx);
            }
            MarketWorkerMessage::StudyRemoved(removed) => {
                self.apply_study_removed(&removed, cx);
            }
            MarketWorkerMessage::StudyRegistered {
                request_sequence,
                study_id,
            } => self.apply_study_registered(request_sequence, study_id, cx),
            MarketWorkerMessage::StudyReinitialized { study_id } => {
                self.apply_study_reinitialized(study_id, cx);
            }
            MarketWorkerMessage::StudyRegistrationFailed {
                request_sequence,
                message,
            } => self.apply_study_registration_failed(request_sequence, message, cx),
            MarketWorkerMessage::StudyReinitializationFailed { study_id, message } => {
                self.studies.cancel_reinitialization(study_id);
                if let Some(dialog) = &mut self.study_settings_dialog
                    && dialog.study_id == study_id
                {
                    dialog.message = Some(message.clone());
                }
                self.indicator_message = Some(message);
                cx.notify();
            }
            MarketWorkerMessage::StudyRemovalFailed { study_id, message } => {
                if study_removal_failed(&mut self.studies, study_id) {
                    self.indicator_message = None;
                } else {
                    self.indicator_message = Some(message);
                }
                cx.notify();
            }
            MarketWorkerMessage::PriceAlertTriggered(trigger) => {
                self.apply_price_alert_trigger(&trigger, cx);
            }
            MarketWorkerMessage::PriceAlertSyncFailed(error) => {
                self.price_alert_message = Some(error);
                cx.notify();
            }
            MarketWorkerMessage::ChartViewport {
                start_unix_nanos,
                end_unix_nanos,
            } => {
                let viewport = (start_unix_nanos, end_unix_nanos);
                self.restored_viewport = Some(viewport);
                self.last_persisted_viewport = Some(viewport);
                if let Some(chart) = &self.chart {
                    chart.update(cx, |chart, chart_cx| {
                        if chart.set_visible_time_range_unix_nanos(start_unix_nanos, end_unix_nanos)
                        {
                            chart_cx.notify();
                        }
                    });
                }
            }
        }
    }

    fn current_runtime_series(&self) -> Result<BarSeriesKey, String> {
        let product = self
            .product
            .as_ref()
            .ok_or_else(|| "market selection is unavailable for this study".to_string())?;
        engine_market_worker::series_key(product, self.interval)
    }

    fn retry_automatic_study_removals(&mut self) -> bool {
        let market_worker = &self.market_worker;
        dispatch_automatic_study_removals(&mut self.studies, |study_id| {
            market_worker.try_remove_study(study_id)
        })
    }

    fn enqueue_runtime_study(
        &mut self,
        mut state: PendingRuntimeStudyState,
    ) -> Result<u64, Box<(PendingRuntimeStudyState, String)>> {
        if !self.retry_automatic_study_removals() {
            return Err(Box::new((
                state,
                "Study cancellation is waiting for runtime capacity".to_string(),
            )));
        }
        let current_series = match self.current_runtime_series() {
            Ok(series) => series,
            Err(error) => return Err(Box::new((state, error))),
        };
        let registration = match runtime_study_registration(
            &state.persisted,
            &current_series,
            &self.studies.active,
        ) {
            Ok(registration) => registration,
            Err(error) => {
                state.blocked = true;
                return Err(Box::new((state, error)));
            }
        };
        state.resolved_chart_series =
            runtime_study_uses_current_chart(&state.persisted).then_some(current_series);
        match self.market_worker.try_register_study(registration) {
            Ok(sequence) => {
                self.studies.pending.insert(sequence, state);
                Ok(sequence)
            }
            Err(TrySendError::Full(_)) => Err(Box::new((
                state,
                "Study request queue is busy; try again".to_string(),
            ))),
            Err(TrySendError::Disconnected(_)) => Err(Box::new((
                state,
                "Study runtime is unavailable".to_string(),
            ))),
        }
    }

    fn dispatch_deferred_runtime_studies(&mut self, cx: &mut Context<Self>) {
        if self.chart.is_none() || !self.studies.pending.is_empty() {
            return;
        }
        let Some(index) = next_deferred_runtime_study_index(&self.studies) else {
            return;
        };
        let state = self.studies.deferred.remove(index);
        if let Err(error) = self.enqueue_runtime_study(state) {
            let (state, message) = *error;
            self.studies.deferred.insert(index, state);
            self.indicator_message = Some(message);
        }
        if !self.studies.deferred.is_empty() {
            cx.notify();
        }
    }

    fn queue_runtime_study_reinitialization(
        &mut self,
        study_id: StudyInstanceId,
        series: BarSeriesKey,
    ) -> Result<(), String> {
        if !self.retry_automatic_study_removals() {
            return Err("Study cancellation is waiting for runtime capacity".to_string());
        }
        let persisted = self
            .studies
            .active
            .iter()
            .find(|state| state.study_id == study_id)
            .map(|state| state.persisted.clone())
            .ok_or_else(|| "study is no longer active".to_string())?;
        let registration = runtime_study_registration(&persisted, &series, &self.studies.active)?;
        match self
            .market_worker
            .try_reinitialize_study(study_id, registration)
        {
            Ok(()) => {
                self.studies.begin_reinitialization(
                    study_id,
                    PendingStudyReinitialization {
                        series,
                        replacement_persisted: None,
                    },
                );
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                Err("Study reinitialization queue is busy; retrying".to_string())
            }
            Err(TrySendError::Disconnected(_)) => Err("Study runtime is unavailable".to_string()),
        }
    }

    fn synchronize_runtime_studies(&mut self, cx: &mut Context<Self>) {
        if !self.retry_automatic_study_removals() {
            return;
        }
        self.dispatch_deferred_runtime_studies(cx);
        let Ok(series) = self.current_runtime_series() else {
            return;
        };
        let stale = self
            .studies
            .active
            .iter()
            .filter(|state| {
                runtime_study_uses_current_chart(&state.persisted)
                    && state.resolved_chart_series.as_ref() != Some(&series)
                    && !self.studies.removing.contains(&state.study_id)
                    && !self.studies.automatic_removals.contains(&state.study_id)
                    && !self.studies.reinitializing.contains_key(&state.study_id)
            })
            .map(|state| state.study_id)
            .collect::<Vec<_>>();
        for study_id in stale {
            if let Err(message) =
                self.queue_runtime_study_reinitialization(study_id, series.clone())
            {
                self.indicator_message = Some(message);
                cx.notify();
            }
        }
    }

    fn apply_study_registered(
        &mut self,
        request_sequence: u64,
        study_id: StudyInstanceId,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.studies.pending.remove(&request_sequence) else {
            return;
        };
        let runtime_state = RuntimeStudyState {
            study_id,
            persisted: state.persisted.clone(),
            resolved_chart_series: state.resolved_chart_series,
        };
        if state.remove_on_registration {
            self.studies.active.push(runtime_state);
            self.studies.automatic_removals.insert(study_id);
            let _ = self.retry_automatic_study_removals();
            self.chart_persistence_dirty = true;
            cx.notify();
            return;
        }
        self.studies.active.push(runtime_state);
        if state.persist_on_registration {
            self.chart_persistence_dirty = true;
        }
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.set_study_visible(study_id.get(), state.persisted.visible) {
                    chart_cx.notify();
                }
            });
        }
        self.indicator_message = None;
        self.synchronize_runtime_studies(cx);
        cx.notify();
    }

    fn apply_study_registration_failed(
        &mut self,
        request_sequence: u64,
        message: String,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.studies.pending.remove(&request_sequence) else {
            return;
        };
        if state.persist_on_registration {
            self.chart_persistence_dirty = true;
        }
        if state.remove_on_registration {
            self.indicator_message = None;
            cx.notify();
            return;
        }
        if !state.persist_on_registration {
            let mut state = state;
            state.blocked = true;
            self.studies.deferred.insert(0, state);
        }
        self.indicator_message = Some(message);
        cx.notify();
    }

    fn apply_study_reinitialized(&mut self, study_id: StudyInstanceId, cx: &mut Context<Self>) {
        let Some(persisted_changed) = self.studies.complete_reinitialization(study_id) else {
            return;
        };
        if persisted_changed {
            self.chart_persistence_dirty = true;
        }
        if self
            .study_settings_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.study_id == study_id)
        {
            self.study_settings_dialog = None;
        }
        self.indicator_message = None;
        cx.notify();
    }

    fn apply_study_output(
        &self,
        snapshot: &aeris_market_runtime::MarketStudyOutputSnapshot,
        cx: &mut Context<Self>,
    ) {
        if self.studies.suppresses_output(snapshot.study_id) {
            return;
        }
        let Some(chart) = &self.chart else {
            return;
        };
        let visible = chart
            .read(cx)
            .study_visible(snapshot.study_id.get())
            .or_else(|| {
                self.studies
                    .active
                    .iter()
                    .find(|state| state.study_id == snapshot.study_id)
                    .map(|state| state.persisted.visible)
            });
        let settings_available = self
            .studies
            .active
            .iter()
            .find(|state| state.study_id == snapshot.study_id)
            .is_some_and(|state| !state.persisted.settings.is_empty());
        chart.update(cx, |chart, chart_cx| {
            match chart.install_study_output(
                snapshot.study_id.get(),
                snapshot.output_id.output_index,
                ChartStudyOutputDescriptor {
                    title: &snapshot.output.title,
                    legend_label: snapshot.output.legend_label.as_deref(),
                    plot: chart_study_plot(snapshot.output.plot),
                    pane: chart_study_pane(snapshot.output.pane),
                    scale: chart_study_scale(snapshot.output.scale),
                    settings_available,
                    threshold_region: chart_study_threshold(snapshot.output.threshold_region),
                    point_style: chart_study_point_style(snapshot.output.point_style),
                    input_requirements: chart_study_input_requirements(
                        snapshot.stream_requirements,
                    ),
                },
                snapshot.series.generation(),
                snapshot.series.timestamps(),
                snapshot.series.values(),
            ) {
                Ok(true) => {
                    if let Some(visible) = visible {
                        chart.set_study_visible(snapshot.study_id.get(), visible);
                    }
                    chart_cx.notify();
                }
                Ok(false) => {}
                Err(error) => {
                    eprintln!("Aeris study output could not be displayed: {error}");
                }
            }
        });
    }

    fn apply_study_removed(
        &mut self,
        removed: &aeris_market_runtime::MarketStudyRemoved,
        cx: &mut Context<Self>,
    ) {
        let study_ids = removed
            .study_ids
            .iter()
            .map(|study_id| study_id.get())
            .collect::<Vec<_>>();
        let removed_runtime = self.studies.remove_runtime_subtree(&removed.study_ids);
        if removed_runtime {
            self.chart_persistence_dirty = true;
        }
        if self
            .study_settings_dialog
            .as_ref()
            .is_some_and(|dialog| removed.study_ids.contains(&dialog.study_id))
        {
            self.study_settings_dialog = None;
        }
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.remove_study_outputs(&study_ids) {
                    chart_cx.notify();
                }
            });
        }
    }

    fn apply_study_invalidated(
        &mut self,
        invalidated: &aeris_market_runtime::MarketStudyOutputsInvalidated,
        cx: &mut Context<Self>,
    ) {
        self.studies
            .invalidate_study_outputs(&invalidated.study_ids);
        let Some(chart) = &self.chart else {
            return;
        };
        let study_ids = invalidated
            .study_ids
            .iter()
            .map(|study_id| study_id.get())
            .collect::<Vec<_>>();
        chart.update(cx, |chart, chart_cx| {
            if chart.remove_study_outputs(&study_ids) {
                chart_cx.notify();
            }
        });
    }

    fn apply_market_state_message(
        &mut self,
        state: ChartState,
        message: String,
        cx: &mut Context<Self>,
    ) {
        // The pending-switch tracker is provider-neutral: both engine
        // providers resolve selections through the same marker flow.
        let engine_provider = matches!(
            self.provider,
            TerminalProvider::Rithmic | TerminalProvider::Hyperliquid
        );
        if self.stale_ready_during_engine_switch(state, engine_provider) {
            return;
        }
        if state == ChartState::Error && engine_provider {
            let swapping = self.rithmic_switch.is_swapping();
            self.rithmic_switch = RithmicSwitchState::Idle;
            self.rithmic_pending_interval = None;
            self.rithmic_pending_product = None;
            self.rithmic_pending_sequence = None;
            self.market_state.symbol_selection_pending = false;
            if swapping {
                self.restore_rithmic_selection_after_failure(&message, cx);
            } else {
                self.rithmic_previous_selection = None;
            }
        } else if self.provider == TerminalProvider::Rithmic && state == ChartState::Ready {
            self.rithmic_switch = RithmicSwitchState::Idle;
            self.rithmic_previous_selection = None;
            self.market_state.symbol_selection_pending = false;
            self.symbol_message = self.product.as_ref().map_or_else(
                || "Rithmic market ready".to_string(),
                |product| format!("{} · Rithmic spot", product.provider_symbol),
            );
        } else if self.provider == TerminalProvider::Hyperliquid && state == ChartState::Ready {
            self.rithmic_switch = RithmicSwitchState::Idle;
            self.rithmic_previous_selection = None;
            self.market_state.symbol_selection_pending = false;
            self.symbol_message = self.product.as_ref().map_or_else(
                || "Hyperliquid market ready".to_string(),
                |product| format!("{} · Hyperliquid", product.display_symbol),
            );
        }
        self.set_chart_state(state, message, cx);
    }

    fn apply_market_connection_message(
        &mut self,
        state: FeedConnectionState,
        message: String,
        transport_rtt_nanos: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        self.provider_transport_rtt_nanos = if state == FeedConnectionState::Streaming {
            transport_rtt_nanos
        } else {
            None
        };
        self.apply_connection_state(state, message, cx);
    }

    fn stale_ready_during_engine_switch(&self, state: ChartState, engine_provider: bool) -> bool {
        // The previous series can report one last Ready after the catalog
        // response but before the worker processes the new EngineSelect
        // command. Keep the switch pending until its marker/snapshot lands.
        state == ChartState::Ready
            && engine_provider
            && !ready_state_can_complete_switch(self.rithmic_switch)
    }

    fn apply_provider_catalog_event(
        &mut self,
        event: ProviderCatalogEvent,
        cx: &mut Context<Self>,
    ) {
        self.apply_catalog_event(event, cx);
    }

    fn apply_catalog_results(
        &mut self,
        generation: u64,
        instruments: Vec<ProviderInstrumentSummary>,
    ) -> Option<usize> {
        let generation = usize_generation(generation)?;
        let count = instruments.len();
        self.symbol_browser
            .apply_results(generation, instruments)
            .then_some(count)
    }

    fn confirm_catalog_selection(&mut self, command_generation: u64) -> bool {
        usize_generation(command_generation)
            .is_some_and(|generation| self.symbol_browser.confirm_selection(generation))
    }

    fn consume_catalog_search_authorization(&mut self) {
        if let Some(search_generation) = self
            .symbol_browser
            .selected()
            .map(|selection| selection.search_generation)
        {
            self.symbol_browser
                .consume_completed_search(search_generation);
        }
    }

    /// Commits a Rithmic switch's identity without touching the chart.
    ///
    /// The marker only says "everything after this belongs to the new
    /// selection". The chart the trader is looking at is left on screen — still
    /// its own series, still correct — under a loading notice, and is replaced
    /// in `apply_publication` when the replacement's covering snapshot arrives.
    /// Dropping it here is what produced the blank surface on every switch.
    fn apply_rithmic_switch_marker(&mut self, sequence: u64, cx: &mut Context<Self>) {
        if !matches!(
            self.provider,
            TerminalProvider::Rithmic | TerminalProvider::Hyperliquid
        ) || !self.rithmic_switch.is_pending()
            || self.rithmic_pending_sequence != Some(sequence)
        {
            return;
        }
        self.rithmic_previous_selection = Some((self.product.clone(), self.interval));
        // Capture presentation against the chart's current instrument before
        // mutating the pending selection. Timeframe replacement can reuse
        // drawings; a symbol replacement must not carry price-anchored drawings
        // onto another instrument.
        self.retain_chart_presentation(cx);
        if let Some(interval) = self.rithmic_pending_interval.take() {
            self.interval = interval;
        }
        if let Some(product) = self.rithmic_pending_product.take() {
            let provider = terminal_provider_from_id(&product.provider);
            if provider != self.provider {
                self.provider = provider;
                self.symbol_browser = demand_startup_symbol_browser();
                self.symbol_message = initial_symbol_message(provider);
            }
            self.product = Some(product);
            // Price levels belong to one instrument: a product switch drops
            // the old book back to loading instead of showing BTC levels
            // under an ETH selection. Interval-only switches keep the book.
            self.order_book.update(cx, |order_book, order_book_cx| {
                order_book.clear(order_book_cx);
            });
        }
        self.rithmic_pending_sequence = None;
        self.rithmic_switch = if self.chart.is_some() {
            RithmicSwitchState::Swapping
        } else {
            RithmicSwitchState::Idle
        };
        self.restored_viewport = None;
        self.last_persisted_viewport = None;
        self.chart_state = ChartState::Loading;
        cx.notify();
    }

    /// Restores the selection a failed switch was replacing.
    ///
    /// The chart on screen is still the previous series, so restoring means
    /// re-stating its demand and reporting an actionable error over it — never
    /// leaving the trader on a surface with no data and no way back.
    fn restore_rithmic_selection_after_failure(&mut self, detail: &str, cx: &mut Context<Self>) {
        let Some((product, interval)) = self.rithmic_previous_selection.take() else {
            return;
        };
        self.product.clone_from(&product);
        self.interval = interval;
        self.rithmic_pending_interval = None;
        self.rithmic_pending_product = None;
        self.rithmic_pending_sequence = None;
        self.rithmic_switch = RithmicSwitchState::Idle;
        self.order_book.update(cx, |order_book, order_book_cx| {
            order_book.clear(order_book_cx);
        });
        let restored = product
            .and_then(|product| self.market_worker.try_select_engine(product, interval).ok());
        if let Some(sequence) = restored {
            self.rithmic_pending_sequence = Some(sequence);
            self.rithmic_pending_interval = Some(interval);
            self.rithmic_switch = RithmicSwitchState::Pending;
        }
        self.series_message = format!("{detail} — showing {}", interval.label());
        cx.notify();
    }

    pub(super) fn poll_market_worker(&mut self, cx: &mut Context<Self>) -> usize {
        let chart_was_missing = self.chart.is_none();
        let (messages, disconnected) = self
            .market_worker
            .drain_messages_up_to(MARKET_MESSAGES_PER_FRAME);
        let applied = messages.len();
        let chart_update_received = messages
            .iter()
            .any(|message| matches!(message, MarketWorkerMessage::Update(_)));
        for message in messages {
            self.apply_market_worker_message(message, cx);
        }
        if matches!(
            self.provider,
            TerminalProvider::Rithmic | TerminalProvider::Hyperliquid
        ) && disconnected
            && !matches!(self.connection_state, Some(FeedConnectionState::Stopped))
        {
            let display = terminal_provider_display(self.provider);
            let fallback = format!("{display} market worker stopped");
            let detail =
                stopped_worker_chart_detail(self.chart_state, &self.chart_state_message, &fallback);
            self.apply_connection_state(FeedConnectionState::Stopped, fallback, cx);
            if self.chart_state == ChartState::Error && self.chart_state_message != detail {
                self.chart_state_message = detail;
                cx.notify();
            }
        } else if disconnected && self.chart_state != ChartState::Error {
            let message = match self.provider {
                TerminalProvider::Rithmic => "Rithmic market worker stopped",
                TerminalProvider::Hyperliquid => "Hyperliquid market worker stopped",
            }
            .to_string();
            self.connection_state = Some(FeedConnectionState::Stopped);
            self.connection_message = Some(message.clone());
            self.set_chart_state(ChartState::Error, message, cx);
        }
        self.dispatch_recovery(cx);
        self.dispatch_retained_symbol_search(cx);
        self.synchronize_runtime_studies(cx);

        let status = self.chart.as_ref().map_or_else(
            || "bridge awaiting snapshot".to_string(),
            |chart| {
                let metrics = chart.read(cx).replay_bridge_metrics();
                let reconciled =
                    reconciled_bridge_state(self.chart_state, metrics.recovery_pending);
                if reconciled != self.chart_state {
                    self.chart_state = reconciled;
                    self.chart_state_message =
                        "chart validation requires a correlated covering snapshot".to_string();
                    cx.notify();
                }
                bridge_status(metrics)
            },
        );
        if self.bridge_label != status {
            self.bridge_label = status;
            cx.notify();
        }
        let loading = self.chart_state == ChartState::Loading;
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.set_asset_loading(loading) {
                    chart_cx.notify();
                }
            });
        }
        if applied > 0 {
            if chart_was_missing && self.chart.is_some() {
                cx.notify();
            } else if chart_update_received && let Some(chart) = &self.chart {
                chart.update(cx, |_, chart_cx| chart_cx.notify());
            }
        }
        applied + usize::from(disconnected)
    }

    fn apply_connection_state(
        &mut self,
        state: FeedConnectionState,
        message: String,
        cx: &mut Context<Self>,
    ) {
        let state = stabilized_connection_state(self.connection_state, state);
        if state != FeedConnectionState::Streaming {
            self.provider_transport_rtt_nanos = None;
        }
        let presentation = ProviderConnectionPresentation::from_connection(state);
        let retained_market_data = self
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).has_market_data());
        if let Some(chart_state) = presentation.chart_state(retained_market_data) {
            self.chart_state = chart_state;
            self.chart_state_message.clone_from(&message);
        }
        self.connection_state = Some(state);
        // Depth follows the same honesty rule as the empty panel: a fresh
        // demand restarts from loading, and only a concrete stop marks the
        // book unavailable. Provider recovery itself is owned by the market runtime.
        match state {
            FeedConnectionState::Disconnected => {
                self.order_book.update(cx, |order_book, order_book_cx| {
                    order_book.set_connection_state(
                        aeris_terminal_ui::OrderBookConnectionState::Offline,
                        order_book_cx,
                    );
                });
            }
            FeedConnectionState::Discovering
            | FeedConnectionState::Authenticating
            | FeedConnectionState::Recovering => {
                self.order_book.update(cx, |order_book, order_book_cx| {
                    order_book.set_connection_state(
                        aeris_terminal_ui::OrderBookConnectionState::Recovering,
                        order_book_cx,
                    );
                });
            }
            FeedConnectionState::Streaming => {
                self.order_book.update(cx, |order_book, order_book_cx| {
                    order_book.set_connection_state(
                        aeris_terminal_ui::OrderBookConnectionState::Online,
                        order_book_cx,
                    );
                });
            }
            FeedConnectionState::Stopped => {
                self.order_book.update(cx, |order_book, order_book_cx| {
                    order_book.mark_unavailable(order_book_cx);
                });
            }
        }
        let autoload_catalog = should_autoload_rithmic_catalog(
            state,
            &message,
            self.market_state.rithmic_autoload_started,
        );
        self.connection_message = Some(stable_connection_message(state, message));
        if autoload_catalog {
            self.market_state.rithmic_autoload_started = true;
            let _ = self.search_symbol_query(DEFAULT_RITHMIC_LISTING_QUERY, cx);
        }
        cx.notify();
    }

    pub(super) fn apply_theme(&mut self, theme: &AerisTheme, cx: &mut Context<Self>) {
        self.order_book.update(cx, |order_book, order_book_cx| {
            order_book.set_theme(*theme, order_book_cx);
        });
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_theme(nucleus_chart_theme(theme.mode));
                chart_cx.notify();
            });
        }
        self.theme = *theme;
        cx.notify();
    }

    pub(super) fn apply_indicator_chrome_preferences(
        &mut self,
        names: bool,
        values: bool,
        price_lines: bool,
        cx: &mut Context<Self>,
    ) {
        self.chart_chrome.indicator_name_labels_visible = names;
        self.chart_chrome.indicator_value_labels_visible = values;
        self.chart_chrome.indicator_price_lines_visible = price_lines;
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.apply_indicator_chrome_preferences(names, values, price_lines);
                chart_cx.notify();
            });
        }
    }

    fn apply_chart_chrome_to_chart(
        &self,
        chart: &Entity<NucleusChartView>,
        cx: &mut Context<Self>,
    ) {
        chart.update(cx, |chart, _| {
            chart.apply_indicator_chrome_preferences(
                self.chart_chrome.indicator_name_labels_visible,
                self.chart_chrome.indicator_value_labels_visible,
                self.chart_chrome.indicator_price_lines_visible,
            );
            chart.set_chart_type(self.chart_chrome.chart_type);
            let _ = chart.apply_price_axis_menu_action(
                0,
                false,
                PriceAxisMenuAction::SetPrecision(self.retained_chart_presentation.price_precision),
            );
        });
    }

    fn retain_chart_presentation(&mut self, cx: &App) {
        if let Some(chart) = &self.chart
            && chart.read(cx).has_market_data()
        {
            for state in &mut self.studies.active {
                if let Some(visible) = chart.read(cx).study_visible(state.study_id.get()) {
                    state.persisted.visible = visible;
                }
            }
            self.retained_chart_presentation.indicators = chart.read(cx).indicator_states();
            self.retained_chart_presentation.price_precision =
                chart.read(cx).selected_price_precision();
            self.retained_chart_presentation.chart_state = self.workspace_chart_state(cx);
            self.retained_chart_presentation.instrument_id = self
                .product
                .as_ref()
                .map(|product| product.instrument_id.clone());
        }
    }

    fn apply_retained_chart_state_to_chart(
        &self,
        chart: &Entity<NucleusChartView>,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = &self.retained_chart_presentation.chart_state {
            let same_instrument = self
                .product
                .as_ref()
                .map(|product| product.instrument_id.as_str())
                == self.retained_chart_presentation.instrument_id.as_deref();
            Self::apply_restored_chart_state(chart, state, same_instrument, cx);
            return;
        }
        self.apply_retained_indicators_to_chart(chart, cx);
    }

    fn apply_retained_indicators_to_chart(
        &self,
        chart: &Entity<NucleusChartView>,
        cx: &mut Context<Self>,
    ) {
        if self.retained_chart_presentation.indicators.is_empty() {
            return;
        }
        let states = self.retained_chart_presentation.indicators.clone();
        let result = chart.update(cx, |chart, _| chart.restore_indicator_states(&states));
        if let Err(error) = result {
            eprintln!("Aeris chart indicators could not be restored: {error}");
        }
    }

    pub(super) fn chart_type(&self, cx: &App) -> ChartType {
        self.chart
            .as_ref()
            .map_or(self.chart_chrome.chart_type, |chart| {
                chart.read(cx).chart_type()
            })
    }

    pub(super) fn chart_appearance(&self, cx: &App) -> Option<ChartAppearanceSettings> {
        self.chart
            .as_ref()
            .map(|chart| chart.read(cx).appearance_settings())
    }

    pub(super) fn chart_crosshair_mode(&self, cx: &App) -> Option<u8> {
        self.chart
            .as_ref()
            .map(|chart| chart.read(cx).crosshair_mode())
    }

    pub(super) fn suspend_chart_pointer(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, _| chart.suspend_pointer_interaction());
        }
    }

    pub(super) fn resume_chart_pointer(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, _| chart.resume_pointer_interaction());
        }
    }

    pub(super) fn set_chart_appearance(
        &mut self,
        appearance: &ChartAppearanceSettings,
        cx: &mut Context<Self>,
    ) {
        let Some(chart) = &self.chart else {
            return;
        };
        if chart.update(cx, |chart, chart_cx| {
            let changed = chart.set_appearance_settings(appearance);
            if changed {
                chart_cx.notify();
            }
            changed
        }) {
            self.chart_persistence_dirty = true;
            cx.notify();
        }
    }

    pub(super) fn set_chart_canvas_appearance(
        &mut self,
        appearance: &ChartAppearanceSettings,
        cx: &mut Context<Self>,
    ) {
        let Some(chart) = &self.chart else {
            return;
        };
        if chart.update(cx, |chart, chart_cx| {
            let changed = chart.set_canvas_appearance_settings(appearance);
            if changed {
                chart_cx.notify();
            }
            changed
        }) {
            self.chart_persistence_dirty = true;
            cx.notify();
        }
    }

    pub(super) fn set_chart_series_appearance(
        &mut self,
        appearance: &ChartAppearanceSettings,
        cx: &mut Context<Self>,
    ) {
        let Some(chart) = &self.chart else {
            return;
        };
        if chart.update(cx, |chart, chart_cx| {
            let changed = chart.set_series_appearance_settings(appearance);
            if changed {
                chart_cx.notify();
            }
            changed
        }) {
            self.chart_persistence_dirty = true;
            cx.notify();
        }
    }

    pub(super) fn reset_chart_appearance(&mut self, cx: &mut Context<Self>) {
        let Some(chart) = &self.chart else {
            return;
        };
        chart.update(cx, |chart, chart_cx| {
            chart.reset_appearance_settings();
            chart_cx.notify();
        });
        self.chart_persistence_dirty = true;
        cx.notify();
    }

    pub(super) fn set_chart_crosshair_mode(&mut self, mode: u8, cx: &mut Context<Self>) {
        let Some(chart) = &self.chart else {
            return;
        };
        if chart.update(cx, |chart, chart_cx| {
            let changed = chart.set_crosshair_mode(mode);
            if changed {
                chart_cx.notify();
            }
            changed
        }) {
            self.chart_persistence_dirty = true;
            cx.notify();
        }
    }

    pub(super) fn set_chart_type(&mut self, chart_type: ChartType, cx: &mut Context<Self>) {
        self.chart_chrome.chart_type = chart_type;
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_chart_type(chart_type);
                chart_cx.notify();
            });
        }
        let preferences = self.chart_chrome;
        match chart_chrome::request_chart_chrome_preferences_save(preferences) {
            Ok(true) => cx
                .background_executor()
                .spawn(async move {
                    if let Err(error) = chart_chrome::run_chart_chrome_preferences_save_worker() {
                        eprintln!("Aeris chart chrome could not be saved: {error}");
                    }
                })
                .detach(),
            Ok(false) => {}
            Err(error) => {
                eprintln!("Aeris chart chrome could not be saved: {error}");
            }
        }
        cx.notify();
    }

    fn refresh_default_instrument_listing(&mut self, cx: &mut Context<Self>) {
        if !instrument_listing_refresh_needed(
            &self.symbol_browser,
            self.market_state.symbol_selection_pending,
        ) {
            return;
        }
        let _ = self.search_symbol_query(default_listing_query(self.provider), cx);
    }

    fn search_symbol_query(&mut self, query: &str, cx: &mut Context<Self>) -> bool {
        if self.symbol_browser.search_pending() {
            match self.symbol_browser.retain_latest_search(query) {
                Ok(already_dispatched) => {
                    if !already_dispatched {
                        let display = terminal_provider_display(self.provider);
                        self.symbol_message =
                            format!("Waiting to search the latest {display} query");
                    }
                    cx.notify();
                    return already_dispatched;
                }
                Err(message) => {
                    self.symbol_message = message.to_string();
                    cx.notify();
                }
            }
            return false;
        }
        let request = match self.symbol_browser.begin_search(query) {
            Ok(request) => request,
            Err(message) => {
                self.symbol_message = message.to_string();
                cx.notify();
                return false;
            }
        };
        self.dispatch_symbol_search(request, cx)
    }

    fn dispatch_symbol_search(
        &mut self,
        request: rithmic_shell::RithmicSymbolSearchRequest,
        cx: &mut Context<Self>,
    ) -> bool {
        let provider = terminal_provider_id(self.provider);
        let retained_query = request.query.clone();
        let request_id = request.request_id;
        let search = SearchProviderInstruments {
            consumer_id: 0,
            search_generation: u64::try_from(request_id.get()).unwrap_or(u64::MAX),
            provider: provider.to_string(),
            query: request.query,
            maximum_results: u32::try_from(self.symbol_browser.maximum_results())
                .unwrap_or(u32::MAX),
        };
        let dispatched = if self.market_worker.try_search_provider(search).is_ok() {
            self.symbol_message = match self.provider {
                TerminalProvider::Rithmic => "Searching Rithmic spot markets".to_string(),
                TerminalProvider::Hyperliquid => "Searching Hyperliquid markets".to_string(),
            };
            true
        } else {
            self.symbol_browser.reject_search(request_id);
            let _ = self.symbol_browser.retain_latest_search(&retained_query);
            self.symbol_message = "Symbol search is busy; try again".to_string();
            false
        };
        cx.notify();
        dispatched
    }

    fn dispatch_retained_symbol_search(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(request) = self.symbol_browser.begin_retained_search() {
            return self.dispatch_symbol_search(request, cx);
        }
        false
    }

    pub(super) fn search_symbol_input(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(input) = &self.symbol_input else {
            return false;
        };
        let query = input.read(cx).value().to_string();
        self.search_symbol_query(&query, cx)
    }

    pub(super) fn submit_symbol_input(&mut self, cx: &mut Context<Self>) -> bool {
        let entries = self.instrument_entries(cx);
        match symbol_submit_decision(self.provider, entries.len(), self.chrome_selection) {
            SymbolSubmitDecision::Select(index) => entries.get(index).is_some_and(|entry| {
                self.select_instrument(entry.selection, self.symbol_selection_target, cx)
            }),
            SymbolSubmitDecision::Search => {
                self.search_symbol_input(cx);
                false
            }
        }
    }

    fn select_rithmic_symbol(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        let Some(selection) = self.symbol_browser.select(index) else {
            return false;
        };
        let entitlement_id = format!(
            "rithmic-test:{}:{}",
            selection.instrument.exchange, selection.instrument.symbol
        );
        let request = SelectProviderInstrument {
            consumer_id: 0,
            selection_generation: u64::try_from(selection.generation.get()).unwrap_or(u64::MAX),
            search_generation: u64::try_from(selection.search_generation.get()).unwrap_or(u64::MAX),
            provider: "rithmic".to_string(),
            symbol: selection.instrument.symbol.clone(),
            exchange: selection.instrument.exchange.clone(),
            entitlement_id,
        };
        let dispatched = if self.market_worker.try_select_provider(request).is_ok() {
            self.market_state.symbol_selection_pending = true;
            self.order_book
                .update(cx, aeris_terminal_ui::ReadOnlyOrderBookView::clear);
            self.symbol_message = format!(
                "Selecting {} · {}",
                selection.instrument.symbol, selection.instrument.exchange
            );
            true
        } else {
            self.symbol_browser.reject_selection(selection.generation);
            self.symbol_message = "Symbol selection is busy; try again".to_string();
            false
        };
        cx.notify();
        dispatched
    }

    fn apply_catalog_event(&mut self, event: ProviderCatalogEvent, cx: &mut Context<Self>) {
        if provider_catalog_event_provider(&event) != terminal_provider_id(self.provider) {
            return;
        }
        match event {
            ProviderCatalogEvent::SearchCompleted(result) => {
                self.apply_search_completed(result, cx);
            }
            ProviderCatalogEvent::SelectionInstalled {
                command_generation,
                instrument,
            } => {
                // Both engine providers resolve selections through the same
                // switch flow: the pending product replaces the chart only
                // when its covering snapshot arrives.
                if self.pending_symbol_selection_target == Some(SymbolSelectionTarget::Watchlist) {
                    let Some(_selection) = usize_generation(command_generation)
                        .and_then(|generation| self.symbol_browser.resolve_selection(generation))
                    else {
                        return;
                    };
                    self.pending_watchlist_instrument = Some(instrument);
                    self.pending_symbol_selection_target = None;
                    if self.chrome_overlay != Some(ChromeOverlay::Instrument)
                        || self.chrome_overlay_phase == ChromeOverlayPhase::Closing
                    {
                        self.symbol_selection_target = SymbolSelectionTarget::Chart;
                    }
                    self.market_state.symbol_selection_pending = false;
                    self.symbol_message = "Watchlist symbol resolved".to_string();
                    cx.notify();
                    return;
                }
                if !self.confirm_catalog_selection(command_generation) {
                    return;
                }
                self.pending_symbol_selection_target = None;
                self.consume_catalog_search_authorization();
                let display = terminal_provider_display(self.provider);
                let interval = self.rithmic_pending_interval.unwrap_or(self.interval);
                let Ok(sequence) = self
                    .market_worker
                    .try_select_engine(instrument.clone(), interval)
                else {
                    self.market_state.symbol_selection_pending = false;
                    self.symbol_message = format!("{display} market history could not start");
                    return;
                };
                self.rithmic_pending_product = Some(instrument);
                self.rithmic_pending_interval = Some(interval);
                self.rithmic_pending_sequence = Some(sequence);
                self.rithmic_switch = RithmicSwitchState::Pending;
                self.chart_state = ChartState::Loading;
                self.chart_state_message = format!("Loading {} market history", interval.label());
                self.symbol_message = format!("Loading the selected {display} market");
            }
            ProviderCatalogEvent::CommandRejected { rejection, command } => {
                self.apply_catalog_rejection(&rejection, command, cx);
            }
        }
        cx.notify();
    }

    fn apply_search_completed(
        &mut self,
        result: ProviderInstrumentSearchResult,
        cx: &mut Context<Self>,
    ) {
        let Some(count) = self.apply_catalog_results(result.search_generation, result.instruments)
        else {
            return;
        };
        self.symbol_message = match self.provider {
            TerminalProvider::Rithmic => {
                format!("{count} active Rithmic spot markets")
            }
            TerminalProvider::Hyperliquid => {
                format!("{count} Hyperliquid markets")
            }
        };
        if self.symbol_browser.has_retained_search() {
            self.dispatch_retained_symbol_search(cx);
            cx.notify();
            return;
        }
        if self.provider == TerminalProvider::Rithmic
            && self.market_state.rithmic_autoload_started
            && self.symbol_browser.selected().is_none()
            && let Some(index) = default_rithmic_contract_index(self.symbol_browser.results())
        {
            self.select_rithmic_symbol(index, cx);
        }
        self.dispatch_retained_symbol_search(cx);
        if self.chrome_overlay == Some(ChromeOverlay::Instrument) {
            self.sync_instrument_menu_keyboard(cx);
        }
    }

    fn apply_catalog_rejection(
        &mut self,
        rejection: &ProviderCatalogRejected,
        command: ProviderCatalogCommand,
        cx: &mut Context<Self>,
    ) {
        let Some(generation) = usize_generation(rejection.command_generation) else {
            return;
        };
        let selection = command == ProviderCatalogCommand::Selection;
        let rejected = if selection {
            self.symbol_browser.reject_selection(generation)
        } else {
            self.symbol_browser.reject_search(generation)
        };
        if !rejected {
            return;
        }
        if selection {
            self.pending_symbol_selection_target = None;
            if self.chrome_overlay != Some(ChromeOverlay::Instrument)
                || self.chrome_overlay_phase == ChromeOverlayPhase::Closing
            {
                self.symbol_selection_target = SymbolSelectionTarget::Chart;
            }
        }
        self.market_state.symbol_selection_pending = false;
        let reason = rejection.reason;
        self.symbol_message = catalog_rejection_message(reason, command, self.provider).to_string();
        self.dispatch_retained_symbol_search(cx);
    }

    /// Whether a market is selected. The header enables the Order Book toggle on this
    /// and `toggle_order_book` opens on it, so the two cannot drift apart again.
    pub(super) fn has_market_selection(&self) -> bool {
        self.symbol_browser.selected().is_some() || self.product.is_some()
    }

    pub(super) fn toggle_order_book(&mut self, cx: &mut Context<Self>) {
        if self.has_market_selection() {
            let visible = !self.side_panels.contains(SidePanel::OrderBook);
            self.set_order_book_visible(visible, cx);
        }
    }

    pub(super) fn set_order_book_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.side_panels.contains(SidePanel::OrderBook) == visible {
            return;
        }
        self.side_panels.set(SidePanel::OrderBook, visible);
        if visible {
            self.order_book
                .update(cx, aeris_terminal_ui::ReadOnlyOrderBookView::clear);
        } else {
            self.menu_state.order_book_column_open = false;
        }
        let _ = self.market_worker.try_set_order_book_visible(visible);
        self.chart_persistence_dirty = true;
        cx.notify();
    }

    pub(super) fn toggle_watchlist(&mut self, cx: &mut Context<Self>) {
        let visible = !self.side_panels.contains(SidePanel::Watchlist);
        self.set_watchlist_visible(visible, cx);
    }

    pub(super) fn set_watchlist_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.side_panels.contains(SidePanel::Watchlist) == visible {
            return;
        }
        self.side_panels.set(SidePanel::Watchlist, visible);
        self.chart_persistence_dirty = true;
        cx.notify();
    }

    pub(super) fn open_watchlist_symbol_menu_at(
        &mut self,
        trigger_position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_chrome_overlay_at(ChromeOverlay::Instrument, trigger_position, window, cx);
        self.symbol_selection_target = SymbolSelectionTarget::Watchlist;
    }

    pub(super) fn toggle_order_book_column_menu(&mut self, cx: &mut Context<Self>) {
        self.menu_state.order_book_column_open = !self.menu_state.order_book_column_open;
        cx.notify();
    }

    pub(super) fn close_order_book_column_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu_state.order_book_column_open {
            self.menu_state.order_book_column_open = false;
            cx.notify();
        }
    }

    pub(super) fn close_side_panel(&mut self, panel: SidePanel, cx: &mut Context<Self>) {
        match panel {
            SidePanel::OrderBook => self.set_order_book_visible(false, cx),
            SidePanel::Watchlist => self.set_watchlist_visible(false, cx),
        }
    }

    pub(super) fn set_side_panel_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let width = clamped_side_panel_width(width);
        if (width - self.side_panel_width).abs() > f32::EPSILON {
            self.side_panel_width = width;
            self.chart_persistence_dirty = true;
            cx.notify();
        }
    }

    pub(super) fn set_side_panel_split_ratio(&mut self, ratio: f32, cx: &mut Context<Self>) {
        let basis_points = (ratio.clamp(0.05, 0.95) * 10_000.0)
            .round()
            .to_u32()
            .unwrap_or(5_000);
        if basis_points != self.side_panel_split_basis_points {
            self.side_panel_split_basis_points = basis_points;
            self.chart_persistence_dirty = true;
            cx.notify();
        }
    }

    pub(super) fn reset_chart_view(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.reset_view();
                chart_cx.notify();
            });
        }
    }

    pub(super) fn select_drawing_tool(&mut self, tool: ChartDrawingTool, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_drawing_tool(tool);
                chart_cx.notify();
            });
            cx.notify();
        }
    }

    pub(super) fn remove_selected_chart_object(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.remove_selected_chart_object() {
                    chart_cx.notify();
                }
            });
            self.retain_chart_presentation(cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_selected_drawing_lock(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                let locked = chart.selected_drawing_locked();
                if chart.set_selected_drawing_locked(!locked) {
                    chart_cx.notify();
                }
            });
            cx.notify();
        }
    }

    pub(super) fn undo_drawing(&mut self, cx: &mut Context<Self>) {
        self.step_drawing_history(true, cx);
    }

    pub(super) fn redo_drawing(&mut self, cx: &mut Context<Self>) {
        self.step_drawing_history(false, cx);
    }

    fn step_drawing_history(&mut self, undo: bool, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            let stepped = chart.update(cx, |chart, chart_cx| {
                let stepped = if undo {
                    chart.undo_drawing()
                } else {
                    chart.redo_drawing()
                };
                if stepped {
                    chart_cx.notify();
                }
                stepped
            });
            if stepped {
                cx.notify();
            }
        }
    }

    pub(super) fn clear_drawings(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.clear_drawings();
                chart.cancel_drawing();
                chart_cx.notify();
            });
            cx.notify();
        }
    }

    pub(super) fn open_study_settings_dialog(
        &mut self,
        study_id: StudyInstanceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.studies.removing.contains(&study_id)
            || self.studies.reinitializing.contains_key(&study_id)
        {
            self.indicator_message = Some("Study settings are busy; try again".to_string());
            cx.notify();
            return;
        }
        let Some(active) = self
            .studies
            .active
            .iter()
            .find(|state| state.study_id == study_id)
            .cloned()
        else {
            self.indicator_message = Some("Study is no longer active".to_string());
            cx.notify();
            return;
        };
        let series = match self.current_runtime_series() {
            Ok(series) => series,
            Err(message) => {
                self.indicator_message = Some(message);
                cx.notify();
                return;
            }
        };
        let registration =
            match runtime_study_registration(&active.persisted, &series, &self.studies.active) {
                Ok(registration) => registration,
                Err(message) => {
                    self.indicator_message = Some(message);
                    cx.notify();
                    return;
                }
            };
        let mut draft_values = BTreeMap::new();
        let mut inputs = HashMap::new();
        let mut subscriptions = Vec::new();
        let surface = cx.entity();
        for spec in &registration.definition.settings {
            let Some(value) = registration.settings.get(&spec.identifier).cloned() else {
                self.indicator_message = Some("Study settings are incomplete".to_string());
                cx.notify();
                return;
            };
            if let Some(text) = study_setting_input_text(&value) {
                let input = cx.new(|input_cx| InputState::new(window, input_cx));
                input.update(cx, |input, input_cx| {
                    input.set_value(text, window, input_cx);
                });
                let notify_surface = surface.clone();
                subscriptions.push(window.subscribe(
                    &input,
                    cx,
                    move |_, event: &InputEvent, _, cx| {
                        if matches!(event, InputEvent::Change) {
                            notify_surface.update(cx, |_, surface_cx| surface_cx.notify());
                        }
                    },
                ));
                inputs.insert(spec.identifier.clone(), input);
            }
            draft_values.insert(spec.identifier.clone(), value);
        }
        self.study_settings_dialog = Some(StudySettingsDialogState {
            study_id,
            title: study_display_name(&active.persisted.identifier),
            specs: registration.definition.settings,
            draft_values,
            inputs,
            _subscriptions: subscriptions,
            message: None,
        });
        self.indicator_message = None;
        self.chrome_overlay = None;
        self.timeframe_menu_flyout = None;
        self.chrome_selection = 0;
        cx.notify();
    }

    pub(super) fn close_study_settings_dialog(&mut self, cx: &mut Context<Self>) {
        if self.study_settings_dialog.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn remove_runtime_study(
        &mut self,
        study_id: StudyInstanceId,
        cx: &mut Context<Self>,
    ) {
        if self.studies.removing.contains(&study_id) {
            return;
        }
        if !self
            .studies
            .active
            .iter()
            .any(|state| state.study_id == study_id)
        {
            self.indicator_message = Some("Study is no longer active".to_string());
            cx.notify();
            return;
        }
        match self.market_worker.try_remove_study(study_id) {
            Ok(()) => {
                self.studies.removing.insert(study_id);
                if self
                    .study_settings_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.study_id == study_id)
                {
                    self.study_settings_dialog = None;
                }
                self.indicator_message = None;
            }
            Err(_) => {
                self.indicator_message =
                    Some("Study removal queue is busy or unavailable".to_string());
            }
        }
        cx.notify();
    }

    pub(super) fn set_study_setting_boolean(
        &mut self,
        identifier: &str,
        value: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = &mut self.study_settings_dialog {
            dialog
                .draft_values
                .insert(identifier.to_string(), StudySettingValue::Boolean(value));
            dialog.message = None;
            cx.notify();
        }
    }

    pub(super) fn select_study_setting_choice(
        &mut self,
        identifier: &str,
        value: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = &mut self.study_settings_dialog {
            dialog.draft_values.insert(
                identifier.to_string(),
                StudySettingValue::Choice(value.to_string()),
            );
            dialog.message = None;
            cx.notify();
        }
    }

    pub(super) fn reset_study_settings_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = &mut self.study_settings_dialog else {
            return;
        };
        for spec in &dialog.specs {
            let value = spec.default.clone();
            if let Some(text) = study_setting_input_text(&value)
                && let Some(input) = dialog.inputs.get(&spec.identifier)
            {
                input.update(cx, |input, input_cx| {
                    input.set_value(text, window, input_cx);
                });
            }
            dialog.draft_values.insert(spec.identifier.clone(), value);
        }
        dialog.message = None;
        cx.notify();
    }

    pub(super) fn save_study_settings_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.study_settings_dialog.as_ref() else {
            return;
        };
        let study_id = dialog.study_id;
        if self.studies.reinitializing.contains_key(&study_id) {
            return;
        }
        let mut values = Vec::with_capacity(dialog.specs.len());
        for spec in &dialog.specs {
            let value = match study_setting_value_from_dialog(dialog, spec, cx) {
                Ok(value) => value,
                Err(message) => {
                    if let Some(dialog) = &mut self.study_settings_dialog {
                        dialog.message = Some(message);
                    }
                    cx.notify();
                    return;
                }
            };
            values.push((spec.identifier.clone(), value));
        }
        let Some(active) = self
            .studies
            .active
            .iter()
            .find(|state| state.study_id == study_id)
            .cloned()
        else {
            if let Some(dialog) = &mut self.study_settings_dialog {
                dialog.message = Some("Study is no longer active".to_string());
            }
            cx.notify();
            return;
        };
        let mut replacement = active.persisted;
        replacement.settings = values
            .iter()
            .map(|(identifier, value)| WorkspaceStudySettingState {
                identifier: identifier.clone(),
                value: Some(persisted_study_setting_value(value)),
            })
            .collect();
        let series = match self.current_runtime_series() {
            Ok(series) => series,
            Err(message) => {
                if let Some(dialog) = &mut self.study_settings_dialog {
                    dialog.message = Some(message);
                }
                cx.notify();
                return;
            }
        };
        let registration =
            match runtime_study_registration(&replacement, &series, &self.studies.active) {
                Ok(registration) => registration,
                Err(message) => {
                    if let Some(dialog) = &mut self.study_settings_dialog {
                        dialog.message = Some(message);
                    }
                    cx.notify();
                    return;
                }
            };
        if !self.retry_automatic_study_removals() {
            if let Some(dialog) = &mut self.study_settings_dialog {
                dialog.message =
                    Some("Study cancellation is waiting for runtime capacity".to_string());
            }
            cx.notify();
            return;
        }
        match self
            .market_worker
            .try_reinitialize_study(study_id, registration)
        {
            Ok(()) => {
                self.studies.begin_reinitialization(
                    study_id,
                    PendingStudyReinitialization {
                        series,
                        replacement_persisted: Some(replacement),
                    },
                );
                if let Some(dialog) = &mut self.study_settings_dialog {
                    dialog.message = None;
                }
            }
            Err(TrySendError::Full(_)) => {
                if let Some(dialog) = &mut self.study_settings_dialog {
                    dialog.message = Some("Study settings queue is busy; try again".to_string());
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                if let Some(dialog) = &mut self.study_settings_dialog {
                    dialog.message = Some("Study runtime is unavailable".to_string());
                }
            }
        }
        cx.notify();
    }

    pub(super) fn clear_indicators(&mut self, cx: &mut Context<Self>) {
        let study_ids = self
            .studies
            .active
            .iter()
            .filter(|state| !self.studies.removing.contains(&state.study_id))
            .map(|state| state.study_id)
            .collect::<Vec<_>>();
        for study_id in study_ids {
            match self.market_worker.try_remove_study(study_id) {
                Ok(()) => {
                    self.studies.removing.insert(study_id);
                }
                Err(_) => {
                    self.indicator_message =
                        Some("Study removal queue is busy or unavailable".to_string());
                }
            }
        }
        let unregistered_removed = discard_unregistered_runtime_studies(&mut self.studies);
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.clear_indicators() {
                    chart_cx.notify();
                }
            });
        }
        if unregistered_removed {
            self.chart_persistence_dirty = true;
        }
        self.retain_chart_presentation(cx);
        cx.notify();
    }

    pub(super) fn has_removable_indicators(&self, cx: &App) -> bool {
        !self.studies.active.is_empty()
            || !self.studies.pending.is_empty()
            || !self.studies.deferred.is_empty()
            || self
                .chart
                .as_ref()
                .is_some_and(|chart| chart.read(cx).has_indicators())
    }

    pub(super) fn add_indicator(
        &mut self,
        indicator: ChartIndicator,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(chart) = self.chart.clone() else {
            self.indicator_message = Some("Chart data is not available yet".to_string());
            cx.notify();
            return false;
        };
        let maximum = current_plan_limits().indicators_per_chart;
        let runtime_count = runtime_study_count(&self.studies);
        if chart.read(cx).indicator_states().len() + runtime_count >= maximum {
            self.indicator_message = Some(format!(
                "Your plan supports at most {maximum} indicators per chart"
            ));
            cx.notify();
            return false;
        }
        if runtime_managed_indicator(indicator) {
            if let Err(message) = self.current_runtime_series() {
                self.indicator_message = Some(message);
                cx.notify();
                return false;
            }
            let Some(local_id) = self.studies.allocate_local_id() else {
                self.indicator_message = Some("Study identity space is exhausted".to_string());
                cx.notify();
                return false;
            };
            let Some(persisted) = legacy_runtime_study(local_id, indicator, true) else {
                self.indicator_message = Some("Study implementation is unavailable".to_string());
                cx.notify();
                return false;
            };
            let state = PendingRuntimeStudyState {
                persisted,
                resolved_chart_series: None,
                remove_on_registration: false,
                persist_on_registration: true,
                blocked: false,
            };
            return match self.enqueue_runtime_study(state) {
                Ok(_) => {
                    self.indicator_message = None;
                    self.chart_persistence_dirty = true;
                    cx.notify();
                    true
                }
                Err(error) => {
                    let (_, message) = *error;
                    self.indicator_message = Some(message);
                    cx.notify();
                    false
                }
            };
        }
        let result = chart.update(cx, |chart, chart_cx| {
            let result = chart.add_indicator(indicator);
            if result.is_ok() {
                chart_cx.notify();
            }
            result
        });
        match result {
            Ok(_) => {
                self.indicator_message = None;
                self.retain_chart_presentation(cx);
                true
            }
            Err(error) => {
                self.indicator_message = Some(error.to_string());
                cx.notify();
                false
            }
        }
    }

    /// Reports what the header's undo and redo controls may offer for the active chart.
    pub(super) fn drawing_history_state(&self, cx: &App) -> DrawingHistoryState {
        self.chart
            .as_ref()
            .map_or_else(DrawingHistoryState::default, |chart| {
                let chart = chart.read(cx);
                DrawingHistoryState {
                    can_undo: chart.can_undo_drawing(),
                    can_redo: chart.can_redo_drawing(),
                }
            })
    }

    pub(super) fn drawing_toolbar_state(&self, cx: &App) -> DrawingToolbarState {
        self.chart
            .as_ref()
            .map_or_else(DrawingToolbarState::default, |chart| {
                DrawingToolbarState::from_chart(chart.read(cx))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom_package_calculate(
        context: &mut aeris_study_sdk::StudyExecutionContext<'_>,
    ) -> Result<(), String> {
        context
            .output(0)
            .map(|_| ())
            .ok_or_else(|| "custom-package test output is unavailable".to_string())
    }

    fn custom_package_restore(
        revision: u32,
        dependencies: Vec<StudyDependency>,
        settings: std::collections::BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, aeris_study_sdk::StudySdkError> {
        if revision != 1 {
            return Err(
                aeris_study_sdk::StudySdkError::UnsupportedImplementationRevision {
                    identifier: "example.workspace_reconnect".to_string(),
                    revision,
                },
            );
        }
        if settings.into_iter().next().is_some() {
            return Err(aeris_study_sdk::StudySdkError::InvalidDependencyContract(
                "example.workspace_reconnect".to_string(),
            ));
        }
        let definition = aeris_study_sdk::StudyDefinition {
            identifier: "example.workspace_reconnect".to_string(),
            dependencies,
            settings: Vec::new(),
            outputs: vec![aeris_study_sdk::StudyOutputSpec {
                identifier: "value".to_string(),
                title: "Workspace Reconnect Example".to_string(),
                legend_label: None,
                plot: StudyPlotKind::Line,
                pane: StudyPaneTarget::Price,
                scale: StudyScaleTarget::Primary,
                threshold_region: None,
                point_style: StudyPointStyle::Uniform,
            }],
            invalidation: aeris_study_sdk::StudyInvalidationPolicy::SameRange,
        };
        Ok(NativeStudyRegistration {
            settings: aeris_study_sdk::StudySettings::defaults(&definition.settings)?,
            definition,
            program: aeris_study_sdk::NativeStudyProgram::stateless(custom_package_calculate),
        })
    }

    fn custom_package_series(instrument: &str) -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: instrument.to_string(),
            entitlement_id: "test".to_string(),
            period: aeris_market_data::BarPeriod::time(60).expect("minute series"),
            definition_version: 1,
        }
    }

    #[test]
    fn trusted_custom_study_restores_across_workspace_reopen_and_current_series_rebind() {
        let packages = [aeris_study_sdk::TrustedStudyPackage::new(
            "example.workspace_reconnect",
            aeris_study_sdk::STUDY_SDK_COMPATIBILITY_EPOCH,
            1,
            custom_package_restore,
        )];
        let registry = aeris_study_sdk::TrustedStudyRegistry::from_packages(&packages)
            .expect("custom package registry");
        let persisted = WorkspaceChartStudyState {
            local_id: 7,
            identifier: "example.workspace_reconnect".to_string(),
            implementation_revision: 1,
            settings: Vec::new(),
            dependencies: vec![WorkspaceStudyDependencyState {
                kind: WorkspaceStudyDependencyKind::CurrentChartSeries as i32,
                streams: vec![WorkspaceStudyMarketStream::Bars as i32],
                ..WorkspaceStudyDependencyState::default()
            }],
            visible: true,
            output_identifiers: vec!["value".to_string()],
        };
        let workspace = WorkspaceChartState {
            studies: vec![persisted.clone()],
            ..WorkspaceChartState::default()
        };
        let restored = persisted_runtime_studies(Some(&workspace));
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].persisted, persisted);

        let first_series = custom_package_series("instrument:rithmic:CME:MNQ");
        let first = runtime_study_registration_with_registry(
            &restored[0].persisted,
            &first_series,
            &[],
            &registry,
        )
        .expect("custom study restores after workspace reopen");
        assert_eq!(
            first.definition.dependencies,
            vec![StudyDependency::Market(StudyMarketInput {
                series: first_series,
                streams: StreamRequirements::BARS,
            })]
        );

        let rebound_series = custom_package_series("instrument:rithmic:CME:MES");
        let rebound = runtime_study_registration_with_registry(
            &restored[0].persisted,
            &rebound_series,
            &[],
            &registry,
        )
        .expect("custom study rebinds after selected series/reconnect recovery");
        assert_eq!(rebound.definition.identifier, "example.workspace_reconnect");
        assert_eq!(rebound.definition.outputs[0].identifier, "value");
        assert_eq!(
            rebound.definition.dependencies,
            vec![StudyDependency::Market(StudyMarketInput {
                series: rebound_series,
                streams: StreamRequirements::BARS,
            })]
        );
    }

    #[test]
    fn missing_custom_package_does_not_starve_later_independent_study_restore() {
        let missing = WorkspaceChartStudyState {
            local_id: 1,
            identifier: "example.missing".to_string(),
            implementation_revision: 1,
            settings: Vec::new(),
            dependencies: Vec::new(),
            visible: true,
            output_identifiers: vec!["value".to_string()],
        };
        let builtin = legacy_runtime_study(2, ChartIndicator::Sma, true).expect("SMA study");
        let studies = RuntimeStudiesState {
            deferred: vec![
                PendingRuntimeStudyState {
                    persisted: missing,
                    resolved_chart_series: None,
                    blocked: true,
                    remove_on_registration: false,
                    persist_on_registration: false,
                },
                PendingRuntimeStudyState {
                    persisted: builtin,
                    resolved_chart_series: None,
                    blocked: false,
                    remove_on_registration: false,
                    persist_on_registration: false,
                },
            ],
            ..RuntimeStudiesState::default()
        };
        assert_eq!(next_deferred_runtime_study_index(&studies), Some(1));
    }

    #[test]
    fn study_decimal_editor_round_trips_exact_fixed_point_values() {
        for value in [
            StudyDecimal {
                mantissa: 25,
                scale: 1,
            },
            StudyDecimal {
                mantissa: -125,
                scale: 2,
            },
            StudyDecimal {
                mantissa: 42,
                scale: 0,
            },
        ] {
            let text = study_decimal_text(value);
            assert_eq!(parse_study_decimal(&text).expect("decimal parses"), value);
        }
        assert!(parse_study_decimal("1.2.3").is_err());
    }

    #[test]
    fn failed_study_setting_reinitialization_preserves_durable_configuration() {
        let study_id = StudyInstanceId::try_from_u64(1).expect("study id");
        let original = legacy_runtime_study(1, ChartIndicator::Wma, true).expect("WMA study");
        let mut replacement = original.clone();
        replacement.settings = vec![WorkspaceStudySettingState {
            identifier: aeris_study_sdk::BUILTIN_WMA_PERIOD_SETTING.to_string(),
            value: Some(workspace_study_setting_state::Value::Integer(42)),
        }];
        let series = BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
            entitlement_id: "test".to_string(),
            period: aeris_market_data::BarPeriod::time(60).expect("minute series"),
            definition_version: 1,
        };
        let mut studies = RuntimeStudiesState {
            active: vec![RuntimeStudyState {
                study_id,
                persisted: original.clone(),
                resolved_chart_series: Some(series.clone()),
            }],
            ..RuntimeStudiesState::default()
        };
        studies.begin_reinitialization(
            study_id,
            PendingStudyReinitialization {
                series,
                replacement_persisted: Some(replacement),
            },
        );

        // Runtime failure retires only the pending candidate. The active
        // durable configuration is not replaced until success is acknowledged.
        studies.cancel_reinitialization(study_id);
        assert_eq!(studies.active[0].persisted, original);
        assert!(!studies.suppressing_outputs.contains(&study_id));
    }

    fn reinitializing_dependency_chain() -> (
        RuntimeStudiesState,
        StudyInstanceId,
        StudyInstanceId,
        BarSeriesKey,
    ) {
        let root_id = StudyInstanceId::try_from_u64(1).expect("root study id");
        let downstream_id = StudyInstanceId::try_from_u64(2).expect("downstream study id");
        let old_series = custom_package_series("instrument:rithmic:CME:MNQ");
        let new_series = custom_package_series("instrument:rithmic:CME:MES");
        let root = legacy_runtime_study(1, ChartIndicator::Sma, true).expect("root study");
        let mut downstream =
            legacy_runtime_study(2, ChartIndicator::Sma, true).expect("downstream study");
        downstream.dependencies.push(WorkspaceStudyDependencyState {
            kind: WorkspaceStudyDependencyKind::StudyOutput as i32,
            study_local_id: 1,
            output_identifier: aeris_study_sdk::BUILTIN_SMA_OUTPUT_IDENTIFIER.to_string(),
            ..WorkspaceStudyDependencyState::default()
        });
        let mut studies = RuntimeStudiesState {
            active: vec![
                RuntimeStudyState {
                    study_id: root_id,
                    persisted: root,
                    resolved_chart_series: Some(old_series.clone()),
                },
                RuntimeStudyState {
                    study_id: downstream_id,
                    persisted: downstream,
                    resolved_chart_series: Some(old_series),
                },
            ],
            ..RuntimeStudiesState::default()
        };
        studies.begin_reinitialization(
            root_id,
            PendingStudyReinitialization {
                series: new_series.clone(),
                replacement_persisted: None,
            },
        );
        studies.begin_reinitialization(
            downstream_id,
            PendingStudyReinitialization {
                series: new_series.clone(),
                replacement_persisted: None,
            },
        );
        (studies, root_id, downstream_id, new_series)
    }

    fn dependent_output_descriptor() -> ChartStudyOutputDescriptor<'static> {
        ChartStudyOutputDescriptor {
            title: "Dependent",
            legend_label: None,
            plot: ChartStudyPlotKind::Line,
            pane: ChartStudyPaneTarget::Price,
            scale: ChartStudyScaleTarget::Primary,
            settings_available: false,
            threshold_region: None,
            point_style: ChartStudyPointStyle::Uniform,
            input_requirements: ChartStudyInputRequirements::BARS,
        }
    }

    fn install_dependent_output(
        chart: &mut NucleusChartView,
        study_id: StudyInstanceId,
        generation: u64,
        value: f64,
    ) {
        chart
            .install_study_output(
                study_id.get(),
                0,
                dependent_output_descriptor(),
                generation,
                &[60_i64 * 1_000_000_000],
                &[Some(value)],
            )
            .expect("dependent presentation installs");
    }

    #[test]
    fn dependency_chain_reinitialization_keeps_downstream_presentation_suppressed_until_own_invalidation()
     {
        let (mut studies, root_id, downstream_id, new_series) = reinitializing_dependency_chain();
        let mut chart = NucleusChartView::empty();
        install_dependent_output(&mut chart, downstream_id, 1, 1.0);
        assert_eq!(chart.study_visible(downstream_id.get()), Some(true));

        assert_eq!(studies.complete_reinitialization(root_id), Some(false));
        assert!(!studies.reinitializing.contains_key(&root_id));
        assert!(studies.reinitializing.contains_key(&downstream_id));
        assert!(studies.suppresses_output(root_id));
        assert!(studies.suppresses_output(downstream_id));

        studies.invalidate_study_outputs(&[root_id, downstream_id]);
        chart.remove_study_outputs(&[root_id.get(), downstream_id.get()]);
        assert!(!studies.suppresses_output(root_id));
        assert!(studies.suppresses_output(downstream_id));
        assert!(studies.reinitializing.contains_key(&downstream_id));
        assert_eq!(chart.study_visible(downstream_id.get()), None);

        if !studies.suppresses_output(downstream_id) {
            install_dependent_output(&mut chart, downstream_id, 2, 2.0);
        }
        assert_eq!(
            chart.study_visible(downstream_id.get()),
            None,
            "upstream subtree output must stay hidden while downstream reinit is pending"
        );

        assert_eq!(
            studies.complete_reinitialization(downstream_id),
            Some(false)
        );
        assert!(studies.reinitializing.is_empty());
        assert!(studies.suppresses_output(downstream_id));
        studies.invalidate_study_outputs(&[downstream_id]);
        assert!(!studies.suppresses_output(downstream_id));
        install_dependent_output(&mut chart, downstream_id, 3, 3.0);
        assert_eq!(chart.study_visible(downstream_id.get()), Some(true));
        assert!(
            studies
                .active
                .iter()
                .all(|state| state.resolved_chart_series.as_ref() == Some(&new_series))
        );
    }

    fn dependent_legacy_study(local_id: u64, upstream_local_id: u64) -> WorkspaceChartStudyState {
        let mut study =
            legacy_runtime_study(local_id, ChartIndicator::Sma, true).expect("dependent study");
        study.dependencies = vec![WorkspaceStudyDependencyState {
            kind: WorkspaceStudyDependencyKind::StudyOutput as i32,
            study_local_id: upstream_local_id,
            output_identifier: aeris_study_sdk::BUILTIN_SMA_OUTPUT_IDENTIFIER.to_string(),
            ..WorkspaceStudyDependencyState::default()
        }];
        study
    }

    fn pending_runtime_study(
        persisted: WorkspaceChartStudyState,
        blocked: bool,
    ) -> PendingRuntimeStudyState {
        PendingRuntimeStudyState {
            persisted,
            resolved_chart_series: None,
            remove_on_registration: false,
            persist_on_registration: false,
            blocked,
        }
    }

    fn runtime_subtree_removal_fixture() -> (RuntimeStudiesState, StudyInstanceId, StudyInstanceId)
    {
        let root_id = StudyInstanceId::try_from_u64(1).expect("root study id");
        let downstream_id = StudyInstanceId::try_from_u64(2).expect("downstream study id");
        let unrelated_id = StudyInstanceId::try_from_u64(3).expect("unrelated study id");
        let mut studies = RuntimeStudiesState {
            active: vec![
                RuntimeStudyState {
                    study_id: root_id,
                    persisted: legacy_runtime_study(1, ChartIndicator::Sma, true)
                        .expect("root study"),
                    resolved_chart_series: None,
                },
                RuntimeStudyState {
                    study_id: downstream_id,
                    persisted: dependent_legacy_study(2, 1),
                    resolved_chart_series: None,
                },
                RuntimeStudyState {
                    study_id: unrelated_id,
                    persisted: legacy_runtime_study(3, ChartIndicator::Wma, true)
                        .expect("unrelated study"),
                    resolved_chart_series: None,
                },
            ],
            pending: HashMap::from([(
                7,
                pending_runtime_study(dependent_legacy_study(4, 2), false),
            )]),
            deferred: vec![
                pending_runtime_study(dependent_legacy_study(5, 4), true),
                pending_runtime_study(
                    legacy_runtime_study(6, ChartIndicator::Wma, true)
                        .expect("unrelated deferred study"),
                    false,
                ),
            ],
            ..RuntimeStudiesState::default()
        };
        studies.removing.insert(root_id);
        studies.begin_reinitialization(
            downstream_id,
            PendingStudyReinitialization {
                series: custom_package_series("instrument:rithmic:CME:MES"),
                replacement_persisted: None,
            },
        );
        (studies, root_id, downstream_id)
    }

    #[test]
    fn manual_runtime_subtree_removal_waits_for_authoritative_ack_before_durable_cleanup() {
        let (mut studies, root_id, downstream_id) = runtime_subtree_removal_fixture();

        let persisted_before_ack = persisted_runtime_study_states(&studies, |_| None)
            .into_iter()
            .map(|state| state.local_id)
            .collect::<Vec<_>>();
        assert_eq!(
            persisted_before_ack,
            vec![1, 2, 3, 4, 5, 6],
            "queuing manual runtime removal must retain the durable root and every descendant until runtime ACK"
        );

        assert!(!study_removal_failed(&mut studies, root_id));
        assert!(!studies.removing.contains(&root_id));
        assert_eq!(
            persisted_runtime_study_states(&studies, |_| None)
                .into_iter()
                .map(|state| state.local_id)
                .collect::<Vec<_>>(),
            persisted_before_ack,
            "manual removal failure leaves the ordinary durable graph unchanged"
        );

        studies.removing.insert(root_id);
        assert_eq!(
            persisted_runtime_study_states(&studies, |_| None)
                .into_iter()
                .map(|state| state.local_id)
                .collect::<Vec<_>>(),
            persisted_before_ack,
            "retrying manual removal still cannot advance durable cleanup ahead of runtime authority"
        );

        assert!(studies.remove_runtime_subtree(&[root_id, downstream_id]));
        assert_eq!(
            studies
                .active
                .iter()
                .map(|state| state.persisted.local_id)
                .collect::<Vec<_>>(),
            vec![3]
        );
        assert!(
            studies
                .pending
                .get(&7)
                .is_some_and(|state| state.remove_on_registration)
        );
        assert_eq!(
            studies
                .deferred
                .iter()
                .map(|state| state.persisted.local_id)
                .collect::<Vec<_>>(),
            vec![6]
        );
        assert_eq!(runtime_study_count(&studies), 2);
        assert_eq!(
            persisted_runtime_study_states(&studies, |_| None)
                .into_iter()
                .map(|state| state.local_id)
                .collect::<Vec<_>>(),
            vec![3, 6],
            "persisted graph excludes the removed runtime subtree and every pending/deferred durable descendant"
        );
        assert!(!studies.removing.contains(&root_id));
        assert!(!studies.reinitializing.contains_key(&downstream_id));
        assert!(!studies.suppressing_outputs.contains(&downstream_id));
    }

    #[test]
    fn persisted_runtime_managed_indicators_are_restored_by_the_runtime_owner_only() {
        let state = WorkspaceChartState {
            indicators: vec![
                WorkspaceChartIndicatorState {
                    kind: "sma".to_string(),
                    visible: false,
                },
                WorkspaceChartIndicatorState {
                    kind: "ema".to_string(),
                    visible: true,
                },
                WorkspaceChartIndicatorState {
                    kind: "ema_ribbon".to_string(),
                    visible: false,
                },
                WorkspaceChartIndicatorState {
                    kind: "wma".to_string(),
                    visible: true,
                },
                WorkspaceChartIndicatorState {
                    kind: "bollinger".to_string(),
                    visible: false,
                },
                WorkspaceChartIndicatorState {
                    kind: "vwap".to_string(),
                    visible: true,
                },
                WorkspaceChartIndicatorState {
                    kind: "atr".to_string(),
                    visible: false,
                },
                WorkspaceChartIndicatorState {
                    kind: "rsi".to_string(),
                    visible: true,
                },
                WorkspaceChartIndicatorState {
                    kind: "macd".to_string(),
                    visible: false,
                },
                WorkspaceChartIndicatorState {
                    kind: "stochastic".to_string(),
                    visible: true,
                },
            ],
            ..WorkspaceChartState::default()
        };
        let restored = persisted_runtime_studies(Some(&state));
        assert_eq!(restored.len(), 10);
        let expected = [
            (aeris_study_sdk::BUILTIN_SMA_IDENTIFIER, false),
            (aeris_study_sdk::BUILTIN_EMA_IDENTIFIER, true),
            (aeris_study_sdk::BUILTIN_EMA_RIBBON_IDENTIFIER, false),
            (aeris_study_sdk::BUILTIN_WMA_IDENTIFIER, true),
            (aeris_study_sdk::BUILTIN_BOLLINGER_IDENTIFIER, false),
            (aeris_study_sdk::BUILTIN_VWAP_IDENTIFIER, true),
            (aeris_study_sdk::BUILTIN_ATR_IDENTIFIER, false),
            (aeris_study_sdk::BUILTIN_RSI_IDENTIFIER, true),
            (aeris_study_sdk::BUILTIN_MACD_IDENTIFIER, false),
            (aeris_study_sdk::BUILTIN_STOCHASTIC_IDENTIFIER, true),
        ];
        for (state, (identifier, visible)) in restored.iter().zip(expected) {
            assert_eq!(state.persisted.identifier, identifier);
            assert_eq!(state.persisted.visible, visible);
        }
        assert_eq!(
            restored[2].persisted.output_identifiers,
            aeris_study_sdk::BUILTIN_EMA_RIBBON_OUTPUT_IDENTIFIERS
                .iter()
                .map(|identifier| (*identifier).to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            restored[4].persisted.output_identifiers,
            vec![
                aeris_study_sdk::BUILTIN_BOLLINGER_UPPER_OUTPUT_IDENTIFIER.to_string(),
                aeris_study_sdk::BUILTIN_BOLLINGER_MIDDLE_OUTPUT_IDENTIFIER.to_string(),
                aeris_study_sdk::BUILTIN_BOLLINGER_LOWER_OUTPUT_IDENTIFIER.to_string(),
            ]
        );
        assert!(restored.iter().all(|state| {
            runtime_study_uses_current_chart(&state.persisted)
                && state.resolved_chart_series.is_none()
                && !state.remove_on_registration
                && !state.persist_on_registration
                && !state.blocked
        }));
    }

    #[test]
    fn durable_study_graph_merges_missing_legacy_runtime_indicators_without_duplicates() {
        let durable_wma = legacy_runtime_study(7, ChartIndicator::Wma, true).expect("WMA study");
        let durable_rsi = legacy_runtime_study(8, ChartIndicator::Rsi, true).expect("RSI study");
        let state = WorkspaceChartState {
            studies: vec![durable_wma.clone(), durable_rsi.clone()],
            indicators: vec![
                WorkspaceChartIndicatorState {
                    kind: "wma".to_string(),
                    visible: false,
                },
                WorkspaceChartIndicatorState {
                    kind: "rsi".to_string(),
                    visible: true,
                },
                WorkspaceChartIndicatorState {
                    kind: "rsi".to_string(),
                    visible: false,
                },
                WorkspaceChartIndicatorState {
                    kind: "bollinger".to_string(),
                    visible: false,
                },
            ],
            ..WorkspaceChartState::default()
        };

        let restored = persisted_runtime_studies(Some(&state));

        assert_eq!(restored.len(), 4);
        assert_eq!(restored[0].persisted, durable_wma);
        assert_eq!(restored[1].persisted, durable_rsi);
        assert_eq!(restored[2].persisted.local_id, 1);
        assert_eq!(
            restored[2].persisted.identifier,
            aeris_study_sdk::BUILTIN_RSI_IDENTIFIER
        );
        assert!(!restored[2].persisted.visible);
        assert_eq!(restored[3].persisted.local_id, 2);
        assert_eq!(
            restored[3].persisted.identifier,
            aeris_study_sdk::BUILTIN_BOLLINGER_IDENTIFIER
        );
        assert!(!restored[3].persisted.visible);
    }

    #[test]
    fn migrated_picker_indicators_are_runtime_managed() {
        assert!(runtime_managed_indicator(ChartIndicator::Sma));
        assert!(runtime_managed_indicator(ChartIndicator::Ema));
        assert!(runtime_managed_indicator(ChartIndicator::EmaRibbon));
        assert!(runtime_managed_indicator(ChartIndicator::Wma));
        assert!(runtime_managed_indicator(ChartIndicator::Bollinger));
        assert!(runtime_managed_indicator(ChartIndicator::Vwap));
        assert!(runtime_managed_indicator(ChartIndicator::Rsi));
        assert!(runtime_managed_indicator(ChartIndicator::Macd));
        assert!(runtime_managed_indicator(ChartIndicator::Stochastic));
        assert!(runtime_managed_indicator(ChartIndicator::Atr));
        assert!(!runtime_managed_indicator(ChartIndicator::Volume));
    }

    #[test]
    fn runtime_managed_indicators_are_not_written_to_the_legacy_indicator_field() {
        let persisted = persisted_legacy_indicator_states([
            ChartIndicatorState {
                indicator: ChartIndicator::Sma,
                visible: true,
            },
            ChartIndicatorState {
                indicator: ChartIndicator::Ema,
                visible: true,
            },
            ChartIndicatorState {
                indicator: ChartIndicator::EmaRibbon,
                visible: true,
            },
            ChartIndicatorState {
                indicator: ChartIndicator::Wma,
                visible: false,
            },
            ChartIndicatorState {
                indicator: ChartIndicator::Bollinger,
                visible: true,
            },
            ChartIndicatorState {
                indicator: ChartIndicator::Vwap,
                visible: true,
            },
            ChartIndicatorState {
                indicator: ChartIndicator::Atr,
                visible: true,
            },
            ChartIndicatorState {
                indicator: ChartIndicator::Rsi,
                visible: false,
            },
            ChartIndicatorState {
                indicator: ChartIndicator::Macd,
                visible: true,
            },
            ChartIndicatorState {
                indicator: ChartIndicator::Stochastic,
                visible: false,
            },
            ChartIndicatorState {
                indicator: ChartIndicator::Volume,
                visible: true,
            },
        ]);

        assert_eq!(
            persisted,
            vec![WorkspaceChartIndicatorState {
                kind: "volume".to_string(),
                visible: true,
            }]
        );
    }

    #[test]
    fn runtime_study_plan_count_excludes_removing_and_cancelled_pending_instances() {
        let first = StudyInstanceId::try_from_u64(1).expect("first study id");
        let removing = StudyInstanceId::try_from_u64(2).expect("removing study id");
        let mut studies = RuntimeStudiesState {
            active: vec![
                RuntimeStudyState {
                    study_id: first,
                    persisted: legacy_runtime_study(1, ChartIndicator::Wma, true)
                        .expect("WMA study"),
                    resolved_chart_series: None,
                },
                RuntimeStudyState {
                    study_id: removing,
                    persisted: legacy_runtime_study(2, ChartIndicator::Bollinger, true)
                        .expect("Bollinger study"),
                    resolved_chart_series: None,
                },
            ],
            ..RuntimeStudiesState::default()
        };
        studies.removing.insert(removing);
        studies.pending.insert(
            3,
            PendingRuntimeStudyState {
                persisted: legacy_runtime_study(3, ChartIndicator::Wma, true).expect("WMA study"),
                resolved_chart_series: None,
                remove_on_registration: false,
                persist_on_registration: true,
                blocked: false,
            },
        );
        studies.pending.insert(
            4,
            PendingRuntimeStudyState {
                persisted: legacy_runtime_study(4, ChartIndicator::Bollinger, true)
                    .expect("Bollinger study"),
                resolved_chart_series: None,
                remove_on_registration: true,
                persist_on_registration: true,
                blocked: false,
            },
        );
        studies.deferred.push(PendingRuntimeStudyState {
            persisted: legacy_runtime_study(5, ChartIndicator::Bollinger, true)
                .expect("Bollinger study"),
            resolved_chart_series: None,
            remove_on_registration: false,
            persist_on_registration: false,
            blocked: false,
        });

        assert_eq!(runtime_study_count(&studies), 3);
    }

    #[test]
    fn automatic_study_removal_retries_after_full_without_resurrecting_presentation_or_persistence()
    {
        let study_id = StudyInstanceId::try_from_u64(9).expect("study id");
        let mut studies = RuntimeStudiesState {
            active: vec![RuntimeStudyState {
                study_id,
                persisted: legacy_runtime_study(9, ChartIndicator::Sma, true).expect("SMA study"),
                resolved_chart_series: None,
            }],
            ..RuntimeStudiesState::default()
        };
        studies.automatic_removals.insert(study_id);

        let mut attempts = 0;
        assert!(!dispatch_automatic_study_removals(
            &mut studies,
            |received| {
                attempts += 1;
                Err(TrySendError::Full(received))
            }
        ));
        assert_eq!(attempts, 1);
        assert!(studies.automatic_removals.contains(&study_id));
        assert!(!studies.removing.contains(&study_id));
        assert!(studies.suppresses_output(study_id));
        assert_eq!(runtime_study_count(&studies), 0);
        assert!(persisted_runtime_study_states(&studies, |_| None).is_empty());

        assert!(dispatch_automatic_study_removals(
            &mut studies,
            |received| {
                attempts += 1;
                assert_eq!(received, study_id);
                Ok(())
            }
        ));
        assert_eq!(attempts, 2);
        assert!(studies.removing.contains(&study_id));
        assert!(dispatch_automatic_study_removals(&mut studies, |_| panic!(
            "an in-flight automatic removal must not be queued twice"
        )));

        assert!(studies.remove_runtime_subtree(&[study_id]));
        assert!(studies.active.is_empty());
        assert!(!studies.automatic_removals.contains(&study_id));
        assert!(!studies.removing.contains(&study_id));
    }

    #[test]
    fn automatic_study_removal_retries_after_runtime_failure() {
        let study_id = StudyInstanceId::try_from_u64(10).expect("study id");
        let mut studies = RuntimeStudiesState::default();
        studies.automatic_removals.insert(study_id);

        assert!(dispatch_automatic_study_removals(
            &mut studies,
            |received| {
                assert_eq!(received, study_id);
                Ok(())
            }
        ));
        assert!(studies.removing.contains(&study_id));
        assert!(study_removal_failed(&mut studies, study_id));
        assert!(!studies.removing.contains(&study_id));
        assert!(studies.automatic_removals.contains(&study_id));

        let mut retries = 0;
        assert!(dispatch_automatic_study_removals(
            &mut studies,
            |received| {
                retries += 1;
                assert_eq!(received, study_id);
                Ok(())
            }
        ));
        assert_eq!(retries, 1);
        assert!(studies.removing.contains(&study_id));
        assert!(studies.automatic_removals.contains(&study_id));
    }

    #[test]
    fn clearing_unregistered_runtime_studies_cancels_pending_and_drops_deferred_work() {
        let mut studies = RuntimeStudiesState::default();
        studies.pending.insert(
            1,
            PendingRuntimeStudyState {
                persisted: legacy_runtime_study(1, ChartIndicator::Wma, true).expect("WMA study"),
                resolved_chart_series: None,
                remove_on_registration: false,
                persist_on_registration: true,
                blocked: false,
            },
        );
        studies.deferred.push(PendingRuntimeStudyState {
            persisted: legacy_runtime_study(2, ChartIndicator::Bollinger, true)
                .expect("Bollinger study"),
            resolved_chart_series: None,
            remove_on_registration: false,
            persist_on_registration: false,
            blocked: false,
        });

        assert!(discard_unregistered_runtime_studies(&mut studies));
        assert!(studies.deferred.is_empty());
        assert!(
            studies
                .pending
                .values()
                .all(|state| state.remove_on_registration)
        );
        assert!(!discard_unregistered_runtime_studies(&mut studies));
    }

    #[test]
    fn durable_study_output_dependency_resolves_to_the_restored_runtime_identity() {
        let series = BarSeriesKey {
            provider_id: "hyperliquid".to_string(),
            instrument_id: "hyperliquid:perp:BTC".to_string(),
            entitlement_id: "hyperliquid-public".to_string(),
            period: aeris_market_data::BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        };
        let upstream_id = StudyInstanceId::try_from_u64(7).expect("runtime study id");
        let upstream = RuntimeStudyState {
            study_id: upstream_id,
            persisted: legacy_runtime_study(1, ChartIndicator::Sma, true).expect("SMA study"),
            resolved_chart_series: Some(series.clone()),
        };
        let mut downstream = legacy_runtime_study(2, ChartIndicator::Sma, true).expect("SMA study");
        downstream.dependencies = vec![WorkspaceStudyDependencyState {
            kind: WorkspaceStudyDependencyKind::StudyOutput as i32,
            study_local_id: 1,
            output_identifier: aeris_study_sdk::BUILTIN_SMA_OUTPUT_IDENTIFIER.to_string(),
            ..WorkspaceStudyDependencyState::default()
        }];

        let registration = runtime_study_registration(&downstream, &series, &[upstream])
            .expect("durable output dependency resolves");

        assert_eq!(
            registration.definition.dependencies,
            vec![StudyDependency::Output(upstream_id.output(0))]
        );
    }
}
