use std::fs;
use std::path::{Path, PathBuf};

use axiusflow_engine_protocol::{
    Envelope, EnvelopeDecoder, InstallProviderInstrument, PROTOCOL_VERSION, SeriesCadence,
    SeriesKey, WorkspaceLayoutState, WorkspacePaneKind, WorkspacePaneState, WorkspaceSplitAxis,
    WorkspaceState, WorkspaceTabState, encode_envelope, envelope,
};

const WORKSPACE_FILE: &str = "workspace-state.frame";

pub(super) fn load_workspace() -> WorkspaceState {
    load_workspace_result().unwrap_or_else(|error| {
        eprintln!("Axiusflow workspace restore skipped: {error}");
        default_workspace()
    })
}

fn load_workspace_result() -> Result<WorkspaceState, String> {
    let path = workspace_path()?;
    load_workspace_from_path(&path)
}

fn load_workspace_from_path(path: &Path) -> Result<WorkspaceState, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(default_workspace());
        }
        Err(error) => return Err(error.to_string()),
    };
    let mut decoder = EnvelopeDecoder::try_new().map_err(|error| error.to_string())?;
    let envelopes = decoder.push(&bytes).map_err(|error| error.to_string())?;
    let Some(workspace) = envelopes
        .into_iter()
        .find_map(|envelope| match envelope.payload {
            Some(envelope::Payload::WorkspaceState(workspace)) => Some(workspace),
            _ => None,
        })
    else {
        return Err("workspace file does not contain workspace state".to_string());
    };
    Ok(sanitize_workspace(workspace))
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
    let bytes = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        payload: Some(envelope::Payload::WorkspaceState(workspace)),
    })
    .map_err(|error| error.to_string())?;
    let temporary = path.with_extension("frame.tmp");
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

pub(super) fn sanitize_workspace(workspace: WorkspaceState) -> WorkspaceState {
    if workspace.workspace_tabs.is_empty() {
        return default_workspace();
    }
    workspace
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let instrument = first.instrument.as_mut().expect("default instrument");
        instrument.instrument_id = "hyperliquid:perp:ETH".to_string();
        instrument.provider_symbol = "ETH".to_string();
        instrument.display_symbol = "ETH-PERP".to_string();
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
}
