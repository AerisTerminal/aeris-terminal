use std::fs;
use std::path::{Path, PathBuf};

use axiusflow_contracts::{
    InstallProviderInstrument, PriceAlertCondition, PriceAlertFrequency, PriceAlertStatus,
    SeriesCadence, SeriesKey, WorkspaceLayoutState, WorkspacePaneKind, WorkspacePaneState,
    WorkspaceSplitAxis, WorkspaceState, WorkspaceTabState,
};
use prost::Message as _;

const WORKSPACE_FILE: &str = "workspace-state.pb";
const LEGACY_WORKSPACE_FILE: &str = "workspace-state.frame";
const LEGACY_WORKSPACE_PROTOCOL_VERSION: u32 = 21;
const MAXIMUM_WORKSPACE_FILE_BYTES: usize = 12 * 1_048_576;

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
    }
    workspace
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_contracts::{
        WorkspaceChartIndicatorState, WorkspaceChartState, WorkspacePriceAlertState,
        WorkspacePriceAxisState,
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
        first.chart = Some(WorkspaceChartState {
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
        });
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
