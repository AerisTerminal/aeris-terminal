use std::fs;
use std::path::{Path, PathBuf};

use axiusflow_contracts::{
    InstallProviderInstrument, PriceAlertCondition, PriceAlertFrequency, PriceAlertStatus,
    SeriesCadence, SeriesKey, WorkspaceChartAppearanceState, WorkspaceChartStudyState,
    WorkspaceLayoutState, WorkspacePaneKind, WorkspacePaneState, WorkspaceSplitAxis,
    WorkspaceState, WorkspaceStudyDependencyKind, WorkspaceStudyMarketStream, WorkspaceTabState,
    workspace_study_setting_state,
};
use prost::Message as _;

const WORKSPACE_FILE: &str = "workspace-state.pb";
const LEGACY_WORKSPACE_FILE: &str = "workspace-state.frame";
const LEGACY_WORKSPACE_PROTOCOL_VERSION: u32 = 21;
const MAXIMUM_WORKSPACE_FILE_BYTES: usize = 12 * 1_048_576;
const MAXIMUM_CHART_COLOR_BYTES: usize = 64;

#[derive(Clone, PartialEq, prost::Message)]
struct LegacyWorkspaceEnvelope {
    #[prost(uint32, tag = "1")]
    protocol_version: u32,
    #[prost(oneof = "legacy_workspace_envelope::Payload", tags = "5")]
    payload: Option<legacy_workspace_envelope::Payload>,
}

mod legacy_workspace_envelope {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Payload {
        #[prost(message, tag = "5")]
        WorkspaceState(super::WorkspaceState),
    }
}

pub(super) fn load_workspace() -> WorkspaceState {
    load_workspace_result().unwrap_or_else(|error| {
        eprintln!("Axiusflow workspace restore skipped: {error}");
        default_workspace()
    })
}

/// Loads persisted workspace state for the release activation readiness gate.
///
/// Unlike normal interactive startup this path is fallible and deliberately
/// does not migrate or rewrite legacy persistence. A candidate must prove it
/// can read the user's current state before it can pass activation health.
pub(super) fn load_workspace_for_readiness() -> Result<WorkspaceState, String> {
    let path = workspace_path()?;
    if path.is_file() {
        return load_workspace_from_path(&path);
    }
    let legacy = path.with_file_name(LEGACY_WORKSPACE_FILE);
    if legacy.is_file() {
        load_legacy_workspace_from_path(&legacy)
    } else {
        Ok(default_workspace())
    }
}

fn load_workspace_result() -> Result<WorkspaceState, String> {
    let path = workspace_path()?;
    if path.is_file() {
        return load_workspace_from_path(&path);
    }
    let legacy = path.with_file_name(LEGACY_WORKSPACE_FILE);
    if !legacy.is_file() {
        return Ok(default_workspace());
    }
    let workspace = load_legacy_workspace_from_path(&legacy)?;
    match save_workspace_to_path(&workspace, &path) {
        Ok(()) => {
            if let Err(error) = fs::remove_file(&legacy)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!("Axiusflow legacy workspace cleanup deferred: {error}");
            }
        }
        Err(error) => {
            eprintln!(
                "Axiusflow workspace migration could not persist the current format: {error}"
            );
        }
    }
    Ok(workspace)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    let length = usize::try_from(metadata.len())
        .map_err(|_| "workspace file length exceeds platform capacity".to_string())?;
    if length == 0 || length > MAXIMUM_WORKSPACE_FILE_BYTES + 4 {
        return Err("workspace file exceeds the local persistence bound".to_string());
    }
    fs::read(path).map_err(|error| error.to_string())
}

fn load_workspace_from_path(path: &Path) -> Result<WorkspaceState, String> {
    let bytes = read_bounded(path)?;
    if bytes.len() > MAXIMUM_WORKSPACE_FILE_BYTES {
        return Err("workspace file exceeds the local persistence bound".to_string());
    }
    WorkspaceState::decode(bytes.as_slice())
        .map(sanitize_workspace)
        .map_err(|error| error.to_string())
}

fn load_legacy_workspace_from_path(path: &Path) -> Result<WorkspaceState, String> {
    let bytes = read_bounded(path)?;
    if bytes.len() < 5 {
        return Err("legacy workspace frame is truncated".to_string());
    }
    let declared = u32::from_be_bytes(
        bytes[..4]
            .try_into()
            .map_err(|_| "legacy workspace frame header is invalid".to_string())?,
    );
    let declared = usize::try_from(declared)
        .map_err(|_| "legacy workspace frame length is invalid".to_string())?;
    if declared == 0 || declared > MAXIMUM_WORKSPACE_FILE_BYTES || declared != bytes.len() - 4 {
        return Err("legacy workspace frame length is invalid".to_string());
    }
    let envelope =
        LegacyWorkspaceEnvelope::decode(&bytes[4..]).map_err(|error| error.to_string())?;
    if envelope.protocol_version != LEGACY_WORKSPACE_PROTOCOL_VERSION {
        return Err("legacy workspace protocol version is unsupported".to_string());
    }
    match envelope.payload {
        Some(legacy_workspace_envelope::Payload::WorkspaceState(workspace)) => {
            Ok(sanitize_workspace(workspace))
        }
        None => Err("legacy workspace file does not contain workspace state".to_string()),
    }
}

pub(super) fn save_workspace(workspace: &WorkspaceState) -> Result<(), String> {
    let path = workspace_path()?;
    save_workspace_to_path(workspace, &path)
}

fn save_workspace_to_path(workspace: &WorkspaceState, path: &Path) -> Result<(), String> {
    let workspace = sanitize_workspace(workspace.clone());
    let parent = path
        .parent()
        .ok_or_else(|| "workspace directory is unavailable".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let bytes = workspace.encode_to_vec();
    if bytes.is_empty() || bytes.len() > MAXIMUM_WORKSPACE_FILE_BYTES {
        return Err("workspace file exceeds the local persistence bound".to_string());
    }
    let temporary = path.with_extension("pb.tmp");
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

#[cfg(test)]
pub(super) fn load_workspace_fixture(path: &Path) -> Result<WorkspaceState, String> {
    load_workspace_from_path(path)
}

#[cfg(test)]
pub(super) fn save_workspace_fixture(
    workspace: &WorkspaceState,
    path: &Path,
) -> Result<(), String> {
    save_workspace_to_path(workspace, path)
}

fn workspace_path() -> Result<PathBuf, String> {
    axiusflow_platform_runtime::native_data_root()
        .map(|root| root.join(WORKSPACE_FILE))
        .map_err(|error| error.to_string())
}
pub(super) fn default_workspace() -> WorkspaceState {
    let instrument = InstallProviderInstrument {
        provider: "hyperliquid".to_string(),
        session_generation: 1,
        selection_generation: 1,
        instrument_id: "hyperliquid:perp:BTC".to_string(),
        provider_symbol: "BTC".to_string(),
        display_symbol: "BTC-PERP".to_string(),
        venue_id: "Hyperliquid".to_string(),
        price_scale: 8,
        quantity_scale: 8,
        entitlement_id: "hyperliquid-public".to_string(),
        price_increment: None,
    };
    let series = SeriesKey {
        provider: instrument.provider.clone(),
        instrument_id: instrument.instrument_id.clone(),
        cadence_value: 60,
        definition_revision: 1,
        entitlement_id: instrument.entitlement_id.clone(),
        cadence: SeriesCadence::FixedSeconds as i32,
    };
    let pane = WorkspacePaneState {
        pane_id: 1,
        consumer_id: 1,
        kind: WorkspacePaneKind::Chart as i32,
        instrument: Some(instrument),
        series: Some(series),
        viewport_start_unix_nanos: None,
        viewport_end_unix_nanos: None,
        size_basis_points: 10_000,
        generation: 1,
        chart: None,
    };
    WorkspaceState {
        provider: "hyperliquid".to_string(),
        market: "BTC-PERP".to_string(),
        interval_seconds: 60,
        watchlist: vec!["BTC-PERP".to_string()],
        workspace_revision: 1,
        schema_revision: 1,
        layout_generation: 1,
        active_workspace_id: 1,
        workspace_tabs: vec![WorkspaceTabState {
            workspace_id: 1,
            label: "Workspace 1".to_string(),
            split_axis: WorkspaceSplitAxis::Horizontal as i32,
            panes: vec![pane],
            active_pane_id: 1,
            generation: 1,
            layout: Some(WorkspaceLayoutState {
                pane_id: 1,
                split_axis: WorkspaceSplitAxis::Horizontal as i32,
                ratio_basis_points: 10_000,
                first: None,
                second: None,
            }),
        }],
    }
}

pub(super) fn sanitize_workspace(mut workspace: WorkspaceState) -> WorkspaceState {
    if workspace.workspace_tabs.is_empty() {
        return default_workspace();
    }
    for pane in workspace
        .workspace_tabs
        .iter_mut()
        .flat_map(|tab| tab.panes.iter_mut())
    {
        let Some(chart) = &mut pane.chart else {
            continue;
        };
        let mut ids = std::collections::BTreeSet::new();
        chart.price_alerts.retain(|alert| {
            !alert.id.is_empty()
                && alert.id.len() <= 128
                && ids.insert(alert.id.clone())
                && alert.instrument.as_ref().is_some_and(|instrument| {
                    !instrument.provider.is_empty()
                        && !instrument.instrument_id.is_empty()
                        && !instrument.entitlement_id.is_empty()
                        && instrument.price_scale <= 18
                })
                && PriceAlertCondition::try_from(alert.condition).is_ok()
                && PriceAlertFrequency::try_from(alert.frequency).is_ok()
                && PriceAlertStatus::try_from(alert.status).is_ok()
                && alert.price_scale_side <= 2
        });
        chart
            .price_alerts
            .truncate(axiusflow_market_runtime::MAXIMUM_PRICE_ALERTS_PER_CONSUMER);
        chart.studies = sanitize_studies(std::mem::take(&mut chart.studies));
        if chart
            .appearance
            .as_ref()
            .is_some_and(|appearance| !valid_chart_appearance(appearance))
        {
            chart.appearance = None;
        }
    }
    workspace
}

fn valid_chart_appearance(appearance: &WorkspaceChartAppearanceState) -> bool {
    let color_is_valid = |value: &str| {
        !value.is_empty()
            && value.len() <= MAXIMUM_CHART_COLOR_BYTES
            && !value.chars().any(char::is_control)
    };
    [
        appearance.grid_color.as_str(),
        appearance.crosshair_color.as_str(),
        appearance.up_color.as_str(),
        appearance.down_color.as_str(),
        appearance.wick_up_color.as_str(),
        appearance.wick_down_color.as_str(),
        appearance.border_up_color.as_str(),
        appearance.border_down_color.as_str(),
        appearance.line_color.as_str(),
        appearance.area_top_color.as_str(),
        appearance.area_bottom_color.as_str(),
        appearance.baseline_top_color.as_str(),
        appearance.baseline_bottom_color.as_str(),
    ]
    .into_iter()
    .all(color_is_valid)
        && appearance.grid_style <= 4
        && (1..=4).contains(&appearance.crosshair_width)
        && appearance.crosshair_style <= 4
        && (1..=4).contains(&appearance.line_width)
        && appearance.line_style <= 4
}

fn sanitize_studies(studies: Vec<WorkspaceChartStudyState>) -> Vec<WorkspaceChartStudyState> {
    let mut last_local_id = 0;
    let mut prior_outputs =
        std::collections::BTreeMap::<u64, std::collections::BTreeSet<String>>::new();
    let mut sanitized = Vec::with_capacity(studies.len());
    for study in studies {
        if !valid_study_header(&study, last_local_id)
            || !valid_study_settings(&study)
            || !valid_study_outputs(&study)
            || !valid_study_dependencies(&study, &prior_outputs)
        {
            continue;
        }
        last_local_id = study.local_id;
        prior_outputs.insert(
            study.local_id,
            study.output_identifiers.iter().cloned().collect(),
        );
        sanitized.push(study);
    }
    sanitized
}

fn valid_study_header(study: &WorkspaceChartStudyState, last_local_id: u64) -> bool {
    study.local_id > last_local_id
        && !study.identifier.is_empty()
        && study.identifier.len() <= axiusflow_market_runtime::study::MAXIMUM_STUDY_IDENTIFIER_BYTES
        && study.implementation_revision != 0
        && study.settings.len() <= axiusflow_market_runtime::study::MAXIMUM_STUDY_SETTINGS
        && study.dependencies.len()
            <= axiusflow_market_runtime::study::MAXIMUM_STUDY_DEPENDENCIES_PER_INSTANCE
        && study.output_identifiers.len()
            <= axiusflow_market_runtime::study::MAXIMUM_STUDY_OUTPUTS_PER_INSTANCE
}

fn valid_study_settings(study: &WorkspaceChartStudyState) -> bool {
    let mut identifiers = std::collections::BTreeSet::new();
    study.settings.iter().all(|setting| {
        !setting.identifier.is_empty()
            && setting.identifier.len()
                <= axiusflow_market_runtime::study::MAXIMUM_STUDY_SETTING_IDENTIFIER_BYTES
            && identifiers.insert(setting.identifier.as_str())
            && setting.value.as_ref().is_some_and(|value| match value {
                workspace_study_setting_state::Value::Boolean(_)
                | workspace_study_setting_state::Value::Integer(_) => true,
                workspace_study_setting_state::Value::Decimal(value) => {
                    value.scale
                        <= u32::from(
                            axiusflow_market_runtime::study::MAXIMUM_STUDY_SETTING_DECIMAL_SCALE,
                        )
                }
                workspace_study_setting_state::Value::Text(value)
                | workspace_study_setting_state::Value::Choice(value) => {
                    value.len() <= axiusflow_market_runtime::study::MAXIMUM_STUDY_SETTING_TEXT_BYTES
                }
            })
    })
}

fn valid_study_outputs(study: &WorkspaceChartStudyState) -> bool {
    if study.output_identifiers.is_empty() {
        return false;
    }
    let mut identifiers = std::collections::BTreeSet::new();
    study.output_identifiers.iter().all(|identifier| {
        !identifier.is_empty()
            && identifier.len() <= axiusflow_market_runtime::study::MAXIMUM_STUDY_IDENTIFIER_BYTES
            && identifiers.insert(identifier.as_str())
    })
}

fn valid_study_dependencies(
    study: &WorkspaceChartStudyState,
    prior_outputs: &std::collections::BTreeMap<u64, std::collections::BTreeSet<String>>,
) -> bool {
    study.dependencies.iter().all(|dependency| {
        let Ok(kind) = WorkspaceStudyDependencyKind::try_from(dependency.kind) else {
            return false;
        };
        match kind {
            WorkspaceStudyDependencyKind::CurrentChartSeries => {
                dependency.series.is_none()
                    && dependency.study_local_id == 0
                    && dependency.output_identifier.is_empty()
                    && valid_study_streams(&dependency.streams)
            }
            WorkspaceStudyDependencyKind::ExplicitSeries => {
                dependency.series.as_ref().is_some_and(valid_study_series)
                    && dependency.study_local_id == 0
                    && dependency.output_identifier.is_empty()
                    && valid_study_streams(&dependency.streams)
            }
            WorkspaceStudyDependencyKind::StudyOutput => {
                dependency.streams.is_empty()
                    && dependency.series.is_none()
                    && dependency.study_local_id != 0
                    && prior_outputs
                        .get(&dependency.study_local_id)
                        .is_some_and(|outputs| outputs.contains(&dependency.output_identifier))
            }
            WorkspaceStudyDependencyKind::Unspecified => false,
        }
    })
}

fn valid_study_streams(streams: &[i32]) -> bool {
    if streams.is_empty() {
        return false;
    }
    let mut distinct = std::collections::BTreeSet::new();
    streams.iter().all(|stream| {
        WorkspaceStudyMarketStream::try_from(*stream).is_ok_and(|stream| {
            stream != WorkspaceStudyMarketStream::Unspecified && distinct.insert(stream as i32)
        })
    })
}

fn valid_study_series(series: &SeriesKey) -> bool {
    !series.provider.is_empty()
        && !series.instrument_id.is_empty()
        && !series.entitlement_id.is_empty()
        && series.definition_revision != 0
        && series.cadence_value != 0
        && SeriesCadence::try_from(series.cadence)
            .is_ok_and(|cadence| cadence != SeriesCadence::Unspecified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_contracts::{
        WorkspaceChartIndicatorState, WorkspaceChartState, WorkspaceChartStudyState,
        WorkspacePriceAlertState, WorkspacePriceAxisState, WorkspaceStudyDecimalState,
        WorkspaceStudyDependencyKind, WorkspaceStudyDependencyState, WorkspaceStudyMarketStream,
        WorkspaceStudySettingState, workspace_study_setting_state,
    };

    fn temporary_workspace_path() -> PathBuf {
        let unique = format!(
            "axiusflow-workspace-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
        );
        std::env::temp_dir().join(unique).join(WORKSPACE_FILE)
    }

    fn price_alert(instrument: InstallProviderInstrument) -> WorkspacePriceAlertState {
        WorkspacePriceAlertState {
            id: "alert-1".to_string(),
            instrument: Some(instrument),
            price: 350_000_000_000,
            pane_index: 0,
            price_scale_side: 0,
            condition: PriceAlertCondition::CrossingUp as i32,
            frequency: PriceAlertFrequency::OnlyOnce as i32,
            status: PriceAlertStatus::Active as i32,
            created_at_unix_nanos: 42,
        }
    }

    fn persisted_sma_study(local_id: u64, period: i64, visible: bool) -> WorkspaceChartStudyState {
        WorkspaceChartStudyState {
            local_id,
            identifier: axiusflow_study_sdk::BUILTIN_SMA_IDENTIFIER.to_string(),
            implementation_revision: axiusflow_study_sdk::BUILTIN_SMA_IMPLEMENTATION_REVISION,
            settings: vec![WorkspaceStudySettingState {
                identifier: axiusflow_study_sdk::BUILTIN_SMA_PERIOD_SETTING.to_string(),
                value: Some(workspace_study_setting_state::Value::Integer(period)),
            }],
            dependencies: vec![WorkspaceStudyDependencyState {
                kind: WorkspaceStudyDependencyKind::CurrentChartSeries as i32,
                streams: vec![WorkspaceStudyMarketStream::Bars as i32],
                ..WorkspaceStudyDependencyState::default()
            }],
            visible,
            output_identifiers: vec![
                axiusflow_study_sdk::BUILTIN_SMA_OUTPUT_IDENTIFIER.to_string(),
            ],
        }
    }

    fn persisted_bollinger_study(local_id: u64, visible: bool) -> WorkspaceChartStudyState {
        WorkspaceChartStudyState {
            local_id,
            identifier: axiusflow_study_sdk::BUILTIN_BOLLINGER_IDENTIFIER.to_string(),
            implementation_revision: axiusflow_study_sdk::BUILTIN_BOLLINGER_IMPLEMENTATION_REVISION,
            settings: vec![
                WorkspaceStudySettingState {
                    identifier: axiusflow_study_sdk::BUILTIN_BOLLINGER_PERIOD_SETTING.to_string(),
                    value: Some(workspace_study_setting_state::Value::Integer(20)),
                },
                WorkspaceStudySettingState {
                    identifier: axiusflow_study_sdk::BUILTIN_BOLLINGER_DEVIATION_SETTING
                        .to_string(),
                    value: Some(workspace_study_setting_state::Value::Decimal(
                        WorkspaceStudyDecimalState {
                            mantissa: 2,
                            scale: 0,
                        },
                    )),
                },
            ],
            dependencies: vec![WorkspaceStudyDependencyState {
                kind: WorkspaceStudyDependencyKind::CurrentChartSeries as i32,
                streams: vec![WorkspaceStudyMarketStream::Bars as i32],
                ..WorkspaceStudyDependencyState::default()
            }],
            visible,
            output_identifiers: vec![
                axiusflow_study_sdk::BUILTIN_BOLLINGER_UPPER_OUTPUT_IDENTIFIER.to_string(),
                axiusflow_study_sdk::BUILTIN_BOLLINGER_MIDDLE_OUTPUT_IDENTIFIER.to_string(),
                axiusflow_study_sdk::BUILTIN_BOLLINGER_LOWER_OUTPUT_IDENTIFIER.to_string(),
            ],
        }
    }

    fn round_trip_chart_state() -> WorkspaceChartState {
        WorkspaceChartState {
            chart_type: "bars".to_string(),
            nucleus_state_json: r#"[{"id":1,"kind":"horizontal_line","pane_index":0,"points":[{"logical":0.0,"price":42000.0}]}]"#.to_string(),
            indicators: vec![WorkspaceChartIndicatorState {
                kind: "rsi".to_string(),
                visible: true,
            }],
            price_axis: Some(WorkspacePriceAxisState {
                flags: 1 | 2 | 64,
                mode: 0,
                left: false,
                precision: Some(2),
            }),
            locked_drawing_ids: vec![1],
            crosshair_mode: 1,
            price_alerts: Vec::new(),
            studies: vec![
                persisted_sma_study(1, 50, false),
                persisted_bollinger_study(2, true),
            ],
            appearance: Some(WorkspaceChartAppearanceState {
                grid_visible: false,
                grid_color: "#334155".to_string(),
                grid_style: 2,
                crosshair_color: "#94a3b8".to_string(),
                crosshair_width: 2,
                crosshair_style: 1,
                up_color: "#10b981".to_string(),
                down_color: "#ef4444".to_string(),
                wick_up_color: "#34d399".to_string(),
                wick_down_color: "#f87171".to_string(),
                border_up_color: "#059669".to_string(),
                border_down_color: "#dc2626".to_string(),
                wick_visible: true,
                border_visible: false,
                open_visible: true,
                thin_bars: false,
                line_color: "#3b82f6".to_string(),
                line_width: 3,
                line_style: 0,
                area_top_color: "#2563eb".to_string(),
                area_bottom_color: "#172554".to_string(),
                baseline_top_color: "#22c55e".to_string(),
                baseline_bottom_color: "#ef4444".to_string(),
            }),
        }
    }

    #[test]
    fn workspace_file_round_trip_preserves_layout_symbol_and_timeframe() {
        let path = temporary_workspace_path();
        let mut workspace = default_workspace();
        workspace.active_workspace_id = 42;
        workspace.workspace_revision = 9;
        workspace.layout_generation = 17;

        let tab = workspace.workspace_tabs.first_mut().expect("default tab");
        tab.workspace_id = 42;
        tab.label = "Execution".to_string();
        tab.active_pane_id = 8;
        tab.generation = 6;

        let first = tab.panes.first_mut().expect("default pane");
        first.pane_id = 7;
        first.consumer_id = 17;
        first.size_basis_points = 4_000;
        first.chart = Some(round_trip_chart_state());
        let instrument = first.instrument.as_mut().expect("default instrument");
        instrument.instrument_id = "hyperliquid:perp:ETH".to_string();
        instrument.provider_symbol = "ETH".to_string();
        instrument.display_symbol = "ETH-PERP".to_string();
        let alert_instrument = instrument.clone();
        first
            .chart
            .as_mut()
            .expect("chart state")
            .price_alerts
            .push(price_alert(alert_instrument));
        let series = first.series.as_mut().expect("default series");
        series.instrument_id = instrument.instrument_id.clone();
        series.cadence = SeriesCadence::FixedSeconds as i32;
        series.cadence_value = 300;

        let mut second = first.clone();
        second.pane_id = 8;
        second.consumer_id = 18;
        second.size_basis_points = 6_000;
        second
            .instrument
            .as_mut()
            .expect("second instrument")
            .instrument_id = "hyperliquid:perp:SOL".to_string();
        second
            .instrument
            .as_mut()
            .expect("second instrument")
            .provider_symbol = "SOL".to_string();
        second
            .instrument
            .as_mut()
            .expect("second instrument")
            .display_symbol = "SOL-PERP".to_string();
        let second_instrument_id = second
            .instrument
            .as_ref()
            .expect("second instrument")
            .instrument_id
            .clone();
        let second_series = second.series.as_mut().expect("second series");
        second_series.instrument_id = second_instrument_id;
        second_series.cadence = SeriesCadence::FixedSeconds as i32;
        second_series.cadence_value = 3_600;
        tab.panes.push(second);
        tab.layout = Some(WorkspaceLayoutState {
            pane_id: 0,
            split_axis: WorkspaceSplitAxis::Vertical as i32,
            ratio_basis_points: 4_000,
            first: Some(Box::new(WorkspaceLayoutState {
                pane_id: 7,
                split_axis: WorkspaceSplitAxis::Horizontal as i32,
                ratio_basis_points: 10_000,
                first: None,
                second: None,
            })),
            second: Some(Box::new(WorkspaceLayoutState {
                pane_id: 8,
                split_axis: WorkspaceSplitAxis::Horizontal as i32,
                ratio_basis_points: 10_000,
                first: None,
                second: None,
            })),
        });

        save_workspace_to_path(&workspace, &path).expect("workspace saves");
        let restored = load_workspace_from_path(&path).expect("workspace reloads");
        assert_eq!(restored, workspace);

        let parent = path.parent().expect("temporary workspace parent");
        std::fs::remove_dir_all(parent).expect("temporary workspace cleanup");
    }

    #[test]
    fn sanitizer_keeps_only_dependency_ordered_study_graph() {
        let mut workspace = default_workspace();
        let chart = workspace.workspace_tabs[0].panes[0]
            .chart
            .get_or_insert_with(WorkspaceChartState::default);
        chart.studies = vec![
            persisted_sma_study(1, 20, true),
            WorkspaceChartStudyState {
                local_id: 2,
                identifier: axiusflow_study_sdk::BUILTIN_SMA_IDENTIFIER.to_string(),
                implementation_revision: axiusflow_study_sdk::BUILTIN_SMA_IMPLEMENTATION_REVISION,
                settings: vec![WorkspaceStudySettingState {
                    identifier: axiusflow_study_sdk::BUILTIN_SMA_PERIOD_SETTING.to_string(),
                    value: Some(workspace_study_setting_state::Value::Integer(5)),
                }],
                dependencies: vec![WorkspaceStudyDependencyState {
                    kind: WorkspaceStudyDependencyKind::StudyOutput as i32,
                    study_local_id: 1,
                    output_identifier: "sma".to_string(),
                    ..WorkspaceStudyDependencyState::default()
                }],
                visible: true,
                output_identifiers: vec!["sma".to_string()],
            },
            WorkspaceChartStudyState {
                local_id: 3,
                identifier: axiusflow_study_sdk::BUILTIN_SMA_IDENTIFIER.to_string(),
                implementation_revision: axiusflow_study_sdk::BUILTIN_SMA_IMPLEMENTATION_REVISION,
                settings: vec![WorkspaceStudySettingState {
                    identifier: axiusflow_study_sdk::BUILTIN_SMA_PERIOD_SETTING.to_string(),
                    value: Some(workspace_study_setting_state::Value::Integer(5)),
                }],
                dependencies: vec![WorkspaceStudyDependencyState {
                    kind: WorkspaceStudyDependencyKind::StudyOutput as i32,
                    study_local_id: 4,
                    output_identifier: "sma".to_string(),
                    ..WorkspaceStudyDependencyState::default()
                }],
                visible: true,
                output_identifiers: vec!["sma".to_string()],
            },
        ];

        let sanitized = sanitize_workspace(workspace);
        let studies = &sanitized.workspace_tabs[0].panes[0]
            .chart
            .as_ref()
            .expect("chart remains")
            .studies;
        assert_eq!(studies.len(), 2);
        assert_eq!(studies[0].local_id, 1);
        assert_eq!(studies[1].local_id, 2);
    }

    #[test]
    fn sanitizer_drops_invalid_chart_appearance_without_dropping_the_chart() {
        let mut workspace = default_workspace();
        let chart = workspace.workspace_tabs[0].panes[0]
            .chart
            .get_or_insert_with(WorkspaceChartState::default);
        chart.appearance = Some(WorkspaceChartAppearanceState {
            grid_visible: true,
            grid_color: "#334155".to_string(),
            grid_style: 9,
            crosshair_color: "#94a3b8".to_string(),
            crosshair_width: 2,
            crosshair_style: 1,
            up_color: "#10b981".to_string(),
            down_color: "#ef4444".to_string(),
            wick_up_color: "#10b981".to_string(),
            wick_down_color: "#ef4444".to_string(),
            border_up_color: "#10b981".to_string(),
            border_down_color: "#ef4444".to_string(),
            wick_visible: true,
            border_visible: true,
            open_visible: true,
            thin_bars: true,
            line_color: "#3b82f6".to_string(),
            line_width: 2,
            line_style: 0,
            area_top_color: "#2563eb".to_string(),
            area_bottom_color: "#172554".to_string(),
            baseline_top_color: "#22c55e".to_string(),
            baseline_bottom_color: "#ef4444".to_string(),
        });

        let sanitized = sanitize_workspace(workspace);
        let chart = sanitized.workspace_tabs[0].panes[0]
            .chart
            .as_ref()
            .expect("chart remains available");
        assert!(chart.appearance.is_none());
    }
    #[test]
    fn legacy_framed_workspace_remains_readable_without_a_transport_runtime() {
        let current = temporary_workspace_path();
        let parent = current.parent().expect("temporary workspace parent");
        std::fs::create_dir_all(parent).expect("temporary workspace directory");
        let legacy_path = parent.join(LEGACY_WORKSPACE_FILE);
        let workspace = default_workspace();
        let envelope = LegacyWorkspaceEnvelope {
            protocol_version: LEGACY_WORKSPACE_PROTOCOL_VERSION,
            payload: Some(legacy_workspace_envelope::Payload::WorkspaceState(
                workspace.clone(),
            )),
        };
        let payload = envelope.encode_to_vec();
        let length = u32::try_from(payload.len()).expect("legacy payload fits");
        let mut framed = length.to_be_bytes().to_vec();
        framed.extend_from_slice(&payload);
        std::fs::write(&legacy_path, framed).expect("legacy workspace writes");

        let restored =
            load_legacy_workspace_from_path(&legacy_path).expect("legacy workspace migrates");
        assert_eq!(restored, workspace);

        std::fs::remove_dir_all(parent).expect("temporary workspace cleanup");
    }
}
