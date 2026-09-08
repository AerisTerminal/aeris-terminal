use std::fs;
use std::path::PathBuf;

use axiusflow_engine_protocol::{
    Envelope, EnvelopeDecoder, InstallProviderInstrument, PROTOCOL_VERSION, SeriesCadence, SeriesKey,
    WorkspaceLayoutState, WorkspacePaneKind, WorkspacePaneState, WorkspaceSplitAxis, WorkspaceState,
    WorkspaceTabState, encode_envelope, envelope,
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
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(default_workspace()),
        Err(error) => return Err(error.to_string()),
    };
    let mut decoder = EnvelopeDecoder::try_new().map_err(|error| error.to_string())?;
    let envelopes = decoder.push(&bytes).map_err(|error| error.to_string())?;
    let Some(workspace) = envelopes.into_iter().find_map(|envelope| match envelope.payload {
        Some(envelope::Payload::WorkspaceState(workspace)) => Some(workspace),
        _ => None,
    }) else {
        return Err("workspace file does not contain workspace state".to_string());
    };
    Ok(sanitize_workspace(workspace))
}

pub(super) fn save_workspace(workspace: &WorkspaceState) -> Result<(), String> {
    let workspace = sanitize_workspace(workspace.clone());
    let path = workspace_path()?;
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
        fs::remove_file(&path).map_err(|error| error.to_string())?;
    }
    fs::rename(&temporary, &path).map_err(|error| error.to_string())
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
