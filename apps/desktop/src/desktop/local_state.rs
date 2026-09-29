#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use aeris_contracts::{
    InstallProviderInstrument, PriceAlertCondition, PriceAlertFrequency, PriceAlertStatus,
    SeriesCadence, SeriesKey, WorkspaceChartAppearanceState, WorkspaceChartSettingsTemplateState,
    WorkspaceChartStudyState, WorkspaceLayoutState, WorkspacePaneKind, WorkspacePaneState,
    WorkspaceSplitAxis, WorkspaceState, WorkspaceStudyDependencyKind, WorkspaceStudyMarketStream,
    WorkspaceTabState, WorkspaceWatchlistEntryState, workspace_study_setting_state,
};
use prost::Message as _;

const WORKSPACE_FILE: &str = "workspace-state.pb";
const LEGACY_WORKSPACE_FILE: &str = "workspace-state.frame";
const LEGACY_WORKSPACE_PROTOCOL_VERSION: u32 = 21;
const MAXIMUM_WORKSPACE_FILE_BYTES: usize = 12 * 1_048_576;
const MAXIMUM_CHART_COLOR_BYTES: usize = 64;
pub(super) const MAXIMUM_CHART_SETTINGS_TEMPLATES: usize = 32;
pub(super) const MAXIMUM_CHART_SETTINGS_TEMPLATE_NAME_BYTES: usize = 64;
pub(super) const MAXIMUM_WATCHLIST_ENTRIES: usize = 64;

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
        eprintln!("Aeris workspace restore skipped: {error}");
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
    if let Some(workspace) = load_recoverable_workspace_from_path(&path)? {
        return Ok(workspace);
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
    if let Some(workspace) = load_recoverable_workspace_from_path(&path)? {
        return Ok(workspace);
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
                eprintln!("Aeris legacy workspace cleanup deferred: {error}");
            }
        }
        Err(error) => {
            eprintln!("Aeris workspace migration could not persist the current format: {error}");
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

fn load_recoverable_workspace_from_path(path: &Path) -> Result<Option<WorkspaceState>, String> {
    let backup = workspace_backup_path(path);
    let temporary = workspace_temporary_path(path);
    let mut first_error = None;
    let mut found = false;
    for candidate in [path, backup.as_path(), temporary.as_path()] {
        if !candidate.is_file() {
            continue;
        }
        found = true;
        match load_workspace_from_path(candidate) {
            Ok(workspace) => return Ok(Some(workspace)),
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    if found {
        Err(first_error.unwrap_or_else(|| "workspace persistence is unreadable".to_string()))
    } else {
        Ok(None)
    }
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
    let temporary = workspace_temporary_path(path);
    let backup = workspace_backup_path(path);
    let mut staging = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    staging
        .write_all(&bytes)
        .and_then(|()| staging.sync_all())
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    sync_workspace_directory(parent)?;

    if path.exists() {
        remove_workspace_file_if_present(&backup)?;
        fs::rename(path, &backup).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        sync_workspace_directory(parent)?;
    }
    fs::rename(&temporary, path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    sync_workspace_directory(parent)?;
    remove_workspace_file_if_present(&backup)?;
    #[cfg(unix)]
    sync_workspace_directory(parent)?;
    Ok(())
}

fn workspace_temporary_path(path: &Path) -> PathBuf {
    path.with_extension("pb.tmp")
}

fn workspace_backup_path(path: &Path) -> PathBuf {
    path.with_extension("pb.bak")
}

fn remove_workspace_file_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(unix)]
fn sync_workspace_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
pub(super) fn load_workspace_fixture(path: &Path) -> Result<WorkspaceState, String> {
    load_recoverable_workspace_from_path(path)?
        .ok_or_else(|| "workspace fixture is unavailable".to_string())
}

#[cfg(test)]
pub(super) fn save_workspace_fixture(
    workspace: &WorkspaceState,
    path: &Path,
) -> Result<(), String> {
    save_workspace_to_path(workspace, path)
}

fn workspace_path() -> Result<PathBuf, String> {
    aeris_platform_runtime::native_data_root()
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
        display_symbol: "BTC-USDC".to_string(),
        venue_id: "Hyperliquid".to_string(),
        price_scale: 8,
        quantity_scale: 8,
        entitlement_id: "hyperliquid-public".to_string(),
        price_increment: None,
        ..Default::default()
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
        instrument: Some(instrument.clone()),
        series: Some(series),
        viewport_start_unix_nanos: None,
        viewport_end_unix_nanos: None,
        size_basis_points: 10_000,
        generation: 1,
        chart: None,
        side_panel_visibility: 0,
        side_panel_width: 400,
        side_panel_split_basis_points: 5_000,
    };
    WorkspaceState {
        provider: "hyperliquid".to_string(),
        market: "BTC-USDC".to_string(),
        interval_seconds: 60,
        watchlist: vec!["BTC-USDC".to_string()],
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
        chart_settings_templates: Vec::new(),
        default_chart_settings: None,
        watchlist_entries: vec![WorkspaceWatchlistEntryState {
            instrument: Some(instrument),
        }],
    }
}

pub(super) fn sanitize_workspace(mut workspace: WorkspaceState) -> WorkspaceState {
    if workspace.workspace_tabs.is_empty() {
        return default_workspace();
    }
    // Provider session/selection generations fence one live runtime process.
    // They are not durable instrument identity. Persisting a generation > 1
    // across the desktop's in-process runtime restart can make that old value
    // authoritative before the new provider session starts at generation 1,
    // causing every real provider event and freshly resolved selection to be
    // rejected as stale. Rebase every durable instrument copy to the initial
    // process generation while preserving its stable provider/instrument route.
    for pane in workspace
        .workspace_tabs
        .iter_mut()
        .flat_map(|tab| tab.panes.iter_mut())
    {
        if let Some(instrument) = pane.instrument.as_mut() {
            rebase_durable_instrument(instrument);
        }
    }
    let mut template_names = std::collections::BTreeSet::new();
    workspace.chart_settings_templates.retain(|template| {
        valid_chart_settings_template(template)
            && template_names.insert(template.name.to_ascii_lowercase())
    });
    workspace
        .chart_settings_templates
        .truncate(MAXIMUM_CHART_SETTINGS_TEMPLATES);
    // Aeris Charts owns the canonical style defaults. Retire the legacy host-side
    // default template so reset cannot be overwritten after the engine API runs.
    workspace.default_chart_settings = None;
    sanitize_watchlist(&mut workspace);
    for pane in workspace
        .workspace_tabs
        .iter_mut()
        .flat_map(|tab| tab.panes.iter_mut())
    {
        // Order book, watchlist and Time & Sales panels.
        pane.side_panel_visibility &= 0b111;
        if pane.side_panel_width != 0 {
            pane.side_panel_width = pane.side_panel_width.clamp(360, 480);
        }
        if pane.side_panel_split_basis_points != 0 {
            pane.side_panel_split_basis_points =
                pane.side_panel_split_basis_points.clamp(500, 9_500);
        }
        let Some(chart) = &mut pane.chart else {
            continue;
        };
        for alert in &mut chart.price_alerts {
            if let Some(instrument) = alert.instrument.as_mut() {
                rebase_durable_instrument(instrument);
            }
        }
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
            .truncate(aeris_market_runtime::MAXIMUM_PRICE_ALERTS_PER_CONSUMER);
        chart.studies = sanitize_studies(std::mem::take(&mut chart.studies));
        if chart
            .appearance
            .as_ref()
            .is_some_and(|appearance| !valid_chart_appearance(appearance))
        {
            chart.appearance = None;
        }
        if !valid_chart_time_zone(&chart.time_zone) {
            chart.time_zone.clear();
        }
        if chart.context_panel_tab > 4 {
            chart.context_panel_tab = 0;
        }
        if chart.context_panel_height != 0 {
            chart.context_panel_height = chart.context_panel_height.clamp(140, 640);
        }
        if chart.chart_link_group > 4 {
            chart.chart_link_group = 0;
        }
        chart.chart_link_flags &= 0b11;
    }
    workspace
}

fn sanitize_watchlist(workspace: &mut WorkspaceState) {
    if workspace.watchlist_entries.is_empty() && !workspace.watchlist.is_empty() {
        workspace.watchlist_entries = workspace
            .workspace_tabs
            .iter()
            .flat_map(|tab| &tab.panes)
            .filter_map(|pane| pane.instrument.as_ref())
            .filter(|instrument| {
                workspace.watchlist.iter().any(|symbol| {
                    symbol.eq_ignore_ascii_case(&instrument.display_symbol)
                        || symbol.eq_ignore_ascii_case(&instrument.provider_symbol)
                })
            })
            .cloned()
            .map(|instrument| WorkspaceWatchlistEntryState {
                instrument: Some(instrument),
            })
            .collect();
    }
    for entry in &mut workspace.watchlist_entries {
        if let Some(instrument) = entry.instrument.as_mut() {
            rebase_durable_instrument(instrument);
        }
    }
    let mut identities = std::collections::BTreeSet::new();
    workspace.watchlist_entries.retain(|entry| {
        entry.instrument.as_ref().is_some_and(|instrument| {
            valid_watchlist_instrument(instrument)
                && identities.insert((
                    instrument.provider.to_ascii_lowercase(),
                    instrument.instrument_id.clone(),
                ))
        })
    });
    workspace
        .watchlist_entries
        .truncate(MAXIMUM_WATCHLIST_ENTRIES);
    workspace.watchlist = workspace
        .watchlist_entries
        .iter()
        .filter_map(|entry| {
            entry
                .instrument
                .as_ref()
                .map(|instrument| instrument.display_symbol.clone())
        })
        .collect();
}

fn rebase_durable_instrument(instrument: &mut InstallProviderInstrument) {
    instrument.session_generation = 1;
    instrument.selection_generation = 1;
    if let Some(display) =
        aeris_market_runtime::migrate_retained_provider_display_symbol(instrument)
    {
        instrument.display_symbol = display;
    }
}

fn valid_watchlist_instrument(instrument: &InstallProviderInstrument) -> bool {
    !instrument.provider.is_empty()
        && instrument.provider.len() <= 64
        && !instrument.instrument_id.is_empty()
        && instrument.instrument_id.len() <= 256
        && !instrument.provider_symbol.is_empty()
        && instrument.provider_symbol.len() <= 128
        && !instrument.display_symbol.is_empty()
        && instrument.display_symbol.len() <= 128
        && !instrument.entitlement_id.is_empty()
        && instrument.entitlement_id.len() <= 256
        && instrument.price_scale <= 18
        && instrument.quantity_scale <= 18
}

fn valid_chart_settings_template(template: &WorkspaceChartSettingsTemplateState) -> bool {
    let name = template.name.trim();
    !name.is_empty()
        && name.len() <= MAXIMUM_CHART_SETTINGS_TEMPLATE_NAME_BYTES
        && !template.name.chars().any(char::is_control)
        && aeris_chart_integration::ChartType::from_identifier(&template.chart_type).is_some()
        && template.crosshair_mode <= 3
        && valid_chart_time_zone(&template.time_zone)
        && template
            .appearance
            .as_ref()
            .is_some_and(valid_chart_appearance)
}

fn valid_chart_time_zone(time_zone: &str) -> bool {
    time_zone.is_empty() || aeris_chart_integration::TRADINGVIEW_TIME_ZONES.contains(&time_zone)
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
    for mut study in studies {
        study.line_width = study
            .line_width
            .min(u32::from(aeris_chart_integration::MAXIMUM_STUDY_LINE_WIDTH));
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
        && study.identifier.len() <= aeris_market_runtime::study::MAXIMUM_STUDY_IDENTIFIER_BYTES
        && study.implementation_revision != 0
        && study.settings.len() <= aeris_market_runtime::study::MAXIMUM_STUDY_SETTINGS
        && study.dependencies.len()
            <= aeris_market_runtime::study::MAXIMUM_STUDY_DEPENDENCIES_PER_INSTANCE
        && study.output_identifiers.len()
            <= aeris_market_runtime::study::MAXIMUM_STUDY_OUTPUTS_PER_INSTANCE
}

fn valid_study_settings(study: &WorkspaceChartStudyState) -> bool {
    let mut identifiers = std::collections::BTreeSet::new();
    study.settings.iter().all(|setting| {
        !setting.identifier.is_empty()
            && setting.identifier.len()
                <= aeris_market_runtime::study::MAXIMUM_STUDY_SETTING_IDENTIFIER_BYTES
            && identifiers.insert(setting.identifier.as_str())
            && setting.value.as_ref().is_some_and(|value| match value {
                workspace_study_setting_state::Value::Boolean(_)
                | workspace_study_setting_state::Value::Integer(_) => true,
                workspace_study_setting_state::Value::Decimal(value) => {
                    value.scale
                        <= u32::from(
                            aeris_market_runtime::study::MAXIMUM_STUDY_SETTING_DECIMAL_SCALE,
                        )
                }
                workspace_study_setting_state::Value::Text(value)
                | workspace_study_setting_state::Value::Choice(value) => {
                    value.len() <= aeris_market_runtime::study::MAXIMUM_STUDY_SETTING_TEXT_BYTES
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
            && identifier.len() <= aeris_market_runtime::study::MAXIMUM_STUDY_IDENTIFIER_BYTES
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
    use aeris_contracts::{
        WorkspaceChartIndicatorState, WorkspaceChartState, WorkspaceChartStudyState,
        WorkspaceOrderFlowSettingsState, WorkspacePriceAlertState, WorkspacePriceAxisState,
        WorkspaceStudyDecimalState, WorkspaceStudyDependencyKind, WorkspaceStudyDependencyState,
        WorkspaceStudyMarketStream, WorkspaceStudySettingState, workspace_study_setting_state,
    };

    fn temporary_workspace_path() -> PathBuf {
        let unique = format!(
            "aeris-workspace-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
        );
        std::env::temp_dir().join(unique).join(WORKSPACE_FILE)
    }

    fn workspace_with_revision(revision: u64) -> WorkspaceState {
        let mut workspace = default_workspace();
        workspace.workspace_revision = revision;
        workspace.layout_generation = revision;
        workspace
    }

    fn write_workspace_artifact(path: &Path, workspace: &WorkspaceState) {
        let parent = path.parent().expect("workspace artifact parent");
        std::fs::create_dir_all(parent).expect("workspace artifact directory");
        std::fs::write(path, sanitize_workspace(workspace.clone()).encode_to_vec())
            .expect("workspace artifact writes");
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
            line_width: 0,
            local_id,
            identifier: aeris_study_sdk::BUILTIN_SMA_IDENTIFIER.to_string(),
            implementation_revision: aeris_study_sdk::BUILTIN_SMA_IMPLEMENTATION_REVISION,
            settings: vec![WorkspaceStudySettingState {
                identifier: aeris_study_sdk::BUILTIN_SMA_PERIOD_SETTING.to_string(),
                value: Some(workspace_study_setting_state::Value::Integer(period)),
            }],
            dependencies: vec![WorkspaceStudyDependencyState {
                kind: WorkspaceStudyDependencyKind::CurrentChartSeries as i32,
                streams: vec![WorkspaceStudyMarketStream::Bars as i32],
                ..WorkspaceStudyDependencyState::default()
            }],
            visible,
            output_identifiers: vec![aeris_study_sdk::BUILTIN_SMA_OUTPUT_IDENTIFIER.to_string()],
        }
    }

    fn persisted_bollinger_study(local_id: u64, visible: bool) -> WorkspaceChartStudyState {
        WorkspaceChartStudyState {
            line_width: 0,
            local_id,
            identifier: aeris_study_sdk::BUILTIN_BOLLINGER_IDENTIFIER.to_string(),
            implementation_revision: aeris_study_sdk::BUILTIN_BOLLINGER_IMPLEMENTATION_REVISION,
            settings: vec![
                WorkspaceStudySettingState {
                    identifier: aeris_study_sdk::BUILTIN_BOLLINGER_PERIOD_SETTING.to_string(),
                    value: Some(workspace_study_setting_state::Value::Integer(20)),
                },
                WorkspaceStudySettingState {
                    identifier: aeris_study_sdk::BUILTIN_BOLLINGER_DEVIATION_SETTING.to_string(),
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
                aeris_study_sdk::BUILTIN_BOLLINGER_UPPER_OUTPUT_IDENTIFIER.to_string(),
                aeris_study_sdk::BUILTIN_BOLLINGER_MIDDLE_OUTPUT_IDENTIFIER.to_string(),
                aeris_study_sdk::BUILTIN_BOLLINGER_LOWER_OUTPUT_IDENTIFIER.to_string(),
            ],
        }
    }

    fn round_trip_chart_state() -> WorkspaceChartState {
        WorkspaceChartState {
            chart_type: "bars".to_string(),
            chart_state_json: r#"[{"id":1,"kind":"horizontal_line","pane_index":0,"points":[{"logical":0.0,"price":42000.0}]}]"#.to_string(),
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
                baseline_top_color: "#22c55e".to_string(),
                baseline_bottom_color: "#ef4444".to_string(),
            }),
            order_flow: Some(WorkspaceOrderFlowSettingsState {
                display_mode: 2,
                show_cumulative_delta: true,
                show_delta_histogram: true,
                show_trade_bubbles: true,
                trade_bubble_minimum_volume_bits: 25.0_f64.to_bits(),
                ticks_per_row: 5,
                study_visibility_revision: 1,
            }),
            context_panel_visible: true,
            context_panel_tab: 2,
            chart_link_group: 2,
            chart_link_flags: 3,
            context_panel_height: 320,
            time_zone: "America/New_York".to_string(),
            show_order_management_lines: Some(false),
            show_execution_marks: Some(true),
        }
    }

    fn chart_settings_template(name: impl Into<String>) -> WorkspaceChartSettingsTemplateState {
        WorkspaceChartSettingsTemplateState {
            name: name.into(),
            chart_type: "candles".to_string(),
            appearance: round_trip_chart_state().appearance,
            crosshair_mode: 1,
            time_zone: "Europe/London".to_string(),
            show_order_management_lines: Some(false),
            show_execution_marks: Some(true),
        }
    }

    #[test]
    fn sanitizer_preserves_supported_time_zones_and_drops_unknown_ones_to_legacy_default() {
        let mut workspace = default_workspace();
        let pane = workspace.workspace_tabs[0].panes.first_mut().expect("pane");
        pane.chart = Some(round_trip_chart_state());
        let sanitized = sanitize_workspace(workspace.clone());
        assert_eq!(
            sanitized.workspace_tabs[0].panes[0]
                .chart
                .as_ref()
                .expect("chart")
                .time_zone,
            "America/New_York"
        );

        workspace.workspace_tabs[0].panes[0]
            .chart
            .as_mut()
            .expect("chart")
            .time_zone = "Mars/Olympus_Mons".to_string();
        let sanitized = sanitize_workspace(workspace);
        assert!(
            sanitized.workspace_tabs[0].panes[0]
                .chart
                .as_ref()
                .expect("chart")
                .time_zone
                .is_empty()
        );
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
        first.side_panel_visibility = 3;
        first.side_panel_width = 420;
        first.side_panel_split_basis_points = 6_250;
        first.chart = Some(round_trip_chart_state());
        let instrument = first.instrument.as_mut().expect("default instrument");
        instrument.instrument_id = "hyperliquid:perp:ETH".to_string();
        instrument.provider_symbol = "ETH".to_string();
        instrument.display_symbol = "ETH-USDC".to_string();
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
            .display_symbol = "SOL-USDC".to_string();
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
    fn workspace_sanitizer_bounds_and_deduplicates_watchlist_state() {
        let mut workspace = default_workspace();
        let instrument = workspace.watchlist_entries[0]
            .instrument
            .clone()
            .expect("default watchlist instrument");
        workspace
            .watchlist_entries
            .push(WorkspaceWatchlistEntryState {
                instrument: Some(instrument),
            });
        workspace
            .watchlist_entries
            .push(WorkspaceWatchlistEntryState { instrument: None });
        let pane = &mut workspace.workspace_tabs[0].panes[0];
        pane.side_panel_visibility = u32::MAX;
        pane.side_panel_width = 9_000;
        pane.side_panel_split_basis_points = 10_000;

        let sanitized = sanitize_workspace(workspace);

        assert_eq!(sanitized.watchlist_entries.len(), 1);
        assert_eq!(sanitized.watchlist, vec!["BTC-USDC"]);
        let pane = &sanitized.workspace_tabs[0].panes[0];
        assert_eq!(pane.side_panel_visibility, 7);
        assert_eq!(pane.side_panel_width, 480);
        assert_eq!(pane.side_panel_split_basis_points, 9_500);
    }

    #[test]
    fn workspace_sanitizer_rebases_process_local_instrument_generations() {
        let mut workspace = default_workspace();
        let pane = &mut workspace.workspace_tabs[0].panes[0];
        let instrument = pane.instrument.as_mut().expect("default pane instrument");
        instrument.session_generation = 3;
        instrument.selection_generation = 6;
        instrument.display_symbol = "BTC-PERP".to_string();
        let mut alert_instrument = instrument.clone();
        alert_instrument.session_generation = 8;
        alert_instrument.selection_generation = 11;
        pane.chart = Some(round_trip_chart_state());
        pane.chart
            .as_mut()
            .expect("chart state")
            .price_alerts
            .push(price_alert(alert_instrument));
        let watchlist = workspace.watchlist_entries[0]
            .instrument
            .as_mut()
            .expect("default watchlist instrument");
        watchlist.session_generation = 5;
        watchlist.selection_generation = 9;
        watchlist.display_symbol = "BTC-PERP".to_string();

        let sanitized = sanitize_workspace(workspace);

        let pane = &sanitized.workspace_tabs[0].panes[0];
        let instrument = pane.instrument.as_ref().expect("pane instrument survives");
        assert_eq!(instrument.session_generation, 1);
        assert_eq!(instrument.selection_generation, 1);
        assert_eq!(instrument.display_symbol, "BTC-USDC");
        let alert = pane
            .chart
            .as_ref()
            .expect("chart survives")
            .price_alerts
            .first()
            .expect("alert survives");
        let alert_instrument = alert
            .instrument
            .as_ref()
            .expect("alert instrument survives");
        assert_eq!(alert_instrument.session_generation, 1);
        assert_eq!(alert_instrument.selection_generation, 1);
        assert_eq!(alert_instrument.display_symbol, "BTC-USDC");
        let watchlist = sanitized.watchlist_entries[0]
            .instrument
            .as_ref()
            .expect("watchlist instrument survives");
        assert_eq!(watchlist.session_generation, 1);
        assert_eq!(watchlist.selection_generation, 1);
        assert_eq!(watchlist.display_symbol, "BTC-USDC");
    }

    #[test]
    fn workspace_sanitizer_migrates_retained_builder_display_without_touching_wire_identity() {
        let mut workspace = default_workspace();
        let pane = &mut workspace.workspace_tabs[0].panes[0];
        let instrument = pane.instrument.as_mut().expect("default pane instrument");
        instrument.instrument_id = "hyperliquid:builder:xyz:XYZ100".to_string();
        instrument.provider_symbol = "xyz:XYZ100".to_string();
        instrument.display_symbol = "xyz:XYZ100-PERP".to_string();
        instrument.venue_id = "xyz".to_string();

        let sanitized = sanitize_workspace(workspace);
        let instrument = sanitized.workspace_tabs[0].panes[0]
            .instrument
            .as_ref()
            .expect("builder instrument survives");
        assert_eq!(instrument.provider_symbol, "xyz:XYZ100");
        assert_eq!(instrument.instrument_id, "hyperliquid:builder:xyz:XYZ100");
        assert_eq!(instrument.venue_id, "xyz");
        assert_eq!(instrument.display_symbol, "XYZ100-USDC");
    }

    #[test]
    fn workspace_sanitizer_does_not_guess_unknown_builder_collateral() {
        let mut workspace = default_workspace();
        let instrument = workspace.workspace_tabs[0].panes[0]
            .instrument
            .as_mut()
            .expect("default pane instrument");
        instrument.instrument_id = "hyperliquid:builder:flx:BTC".to_string();
        instrument.provider_symbol = "flx:BTC".to_string();
        instrument.display_symbol = "flx:BTC-PERP".to_string();
        instrument.venue_id = "flx".to_string();

        let sanitized = sanitize_workspace(workspace);
        let instrument = sanitized.workspace_tabs[0].panes[0]
            .instrument
            .as_ref()
            .expect("builder instrument survives");
        assert_eq!(instrument.display_symbol, "flx:BTC-PERP");
        assert_eq!(instrument.provider_symbol, "flx:BTC");
    }

    #[test]
    fn workspace_recovery_prefers_committed_current_over_backup_or_staging() {
        let path = temporary_workspace_path();
        let backup = workspace_backup_path(&path);
        let temporary = workspace_temporary_path(&path);
        let previous = workspace_with_revision(11);
        let current = workspace_with_revision(12);
        let staged = workspace_with_revision(13);
        write_workspace_artifact(&backup, &previous);
        write_workspace_artifact(&path, &current);
        write_workspace_artifact(&temporary, &staged);

        assert_eq!(
            load_recoverable_workspace_from_path(&path)
                .expect("workspace recovery succeeds")
                .expect("workspace is present"),
            current
        );

        std::fs::write(&temporary, b"corrupt staging").expect("corrupt staging writes");
        assert_eq!(
            load_recoverable_workspace_from_path(&path)
                .expect("valid current ignores corrupt staging")
                .expect("workspace is present"),
            current
        );
        std::fs::remove_dir_all(path.parent().expect("temporary workspace parent"))
            .expect("temporary workspace cleanup");
    }

    #[test]
    fn workspace_recovery_uses_backup_before_unpublished_staging() {
        let path = temporary_workspace_path();
        let backup = workspace_backup_path(&path);
        let temporary = workspace_temporary_path(&path);
        let previous = workspace_with_revision(21);
        let staged = workspace_with_revision(22);
        write_workspace_artifact(&backup, &previous);
        write_workspace_artifact(&temporary, &staged);

        assert_eq!(
            load_recoverable_workspace_from_path(&path)
                .expect("backup recovery succeeds")
                .expect("workspace is present"),
            previous
        );

        std::fs::write(&path, b"corrupt current").expect("corrupt current writes");
        assert_eq!(
            load_recoverable_workspace_from_path(&path)
                .expect("corrupt current falls back to backup")
                .expect("workspace is present"),
            previous
        );
        std::fs::remove_dir_all(path.parent().expect("temporary workspace parent"))
            .expect("temporary workspace cleanup");
    }

    #[test]
    fn workspace_recovery_accepts_synced_staging_only_for_an_interrupted_first_save() {
        let path = temporary_workspace_path();
        let temporary = workspace_temporary_path(&path);
        let staged = workspace_with_revision(31);
        write_workspace_artifact(&temporary, &staged);

        assert_eq!(
            load_recoverable_workspace_from_path(&path)
                .expect("first-save staging recovery succeeds")
                .expect("workspace is present"),
            staged
        );
        std::fs::remove_dir_all(path.parent().expect("temporary workspace parent"))
            .expect("temporary workspace cleanup");
    }

    #[test]
    fn successful_workspace_save_removes_recovery_artifacts_after_publish() {
        let path = temporary_workspace_path();
        let first = workspace_with_revision(41);
        let second = workspace_with_revision(42);
        save_workspace_to_path(&first, &path).expect("first workspace saves");
        save_workspace_to_path(&second, &path).expect("second workspace saves");

        assert!(path.is_file());
        assert!(!workspace_backup_path(&path).exists());
        assert!(!workspace_temporary_path(&path).exists());
        assert_eq!(
            load_recoverable_workspace_from_path(&path)
                .expect("published workspace reloads")
                .expect("workspace is present"),
            second
        );
        std::fs::remove_dir_all(path.parent().expect("temporary workspace parent"))
            .expect("temporary workspace cleanup");
    }

    #[test]
    fn workspace_file_round_trip_preserves_unavailable_trusted_native_study_state() {
        let path = temporary_workspace_path();
        let mut workspace = default_workspace();
        let chart = workspace.workspace_tabs[0].panes[0]
            .chart
            .get_or_insert_with(WorkspaceChartState::default);
        let custom = WorkspaceChartStudyState {
            line_width: 0,
            local_id: 41,
            identifier: "example.workspace_reconnect".to_string(),
            implementation_revision: 3,
            settings: vec![WorkspaceStudySettingState {
                identifier: "period".to_string(),
                value: Some(workspace_study_setting_state::Value::Integer(37)),
            }],
            dependencies: vec![WorkspaceStudyDependencyState {
                kind: WorkspaceStudyDependencyKind::CurrentChartSeries as i32,
                streams: vec![WorkspaceStudyMarketStream::Bars as i32],
                ..WorkspaceStudyDependencyState::default()
            }],
            visible: false,
            output_identifiers: vec!["value".to_string()],
        };
        chart.studies = vec![custom.clone()];

        save_workspace_to_path(&workspace, &path).expect("workspace saves");
        let restored = load_workspace_from_path(&path).expect("workspace reloads");
        assert_eq!(
            restored.workspace_tabs[0].panes[0]
                .chart
                .as_ref()
                .expect("restored chart")
                .studies,
            vec![custom]
        );

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
                line_width: 0,
                local_id: 2,
                identifier: aeris_study_sdk::BUILTIN_SMA_IDENTIFIER.to_string(),
                implementation_revision: aeris_study_sdk::BUILTIN_SMA_IMPLEMENTATION_REVISION,
                settings: vec![WorkspaceStudySettingState {
                    identifier: aeris_study_sdk::BUILTIN_SMA_PERIOD_SETTING.to_string(),
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
                line_width: 0,
                local_id: 3,
                identifier: aeris_study_sdk::BUILTIN_SMA_IDENTIFIER.to_string(),
                implementation_revision: aeris_study_sdk::BUILTIN_SMA_IMPLEMENTATION_REVISION,
                settings: vec![WorkspaceStudySettingState {
                    identifier: aeris_study_sdk::BUILTIN_SMA_PERIOD_SETTING.to_string(),
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
    fn sanitizer_bounds_and_validates_shared_chart_settings_templates() {
        let mut workspace = default_workspace();
        workspace.chart_settings_templates = (0..MAXIMUM_CHART_SETTINGS_TEMPLATES + 4)
            .map(|index| chart_settings_template(format!("Template {index}")))
            .collect();
        workspace
            .chart_settings_templates
            .insert(1, chart_settings_template("template 0"));
        workspace
            .chart_settings_templates
            .insert(2, chart_settings_template("\ninvalid"));
        workspace.default_chart_settings = Some(WorkspaceChartSettingsTemplateState {
            crosshair_mode: 99,
            ..chart_settings_template("Default")
        });

        let sanitized = sanitize_workspace(workspace);

        assert_eq!(
            sanitized.chart_settings_templates.len(),
            MAXIMUM_CHART_SETTINGS_TEMPLATES
        );
        assert_eq!(sanitized.chart_settings_templates[0].name, "Template 0");
        assert_eq!(sanitized.chart_settings_templates[1].name, "Template 1");
        assert!(sanitized.default_chart_settings.is_none());
    }

    #[test]
    fn workspace_file_round_trip_preserves_templates_and_retires_host_default() {
        let path = temporary_workspace_path();
        let mut workspace = default_workspace();
        let saved = chart_settings_template("Scalping");
        let default = chart_settings_template("Default");
        workspace.chart_settings_templates = vec![saved.clone()];
        workspace.default_chart_settings = Some(default.clone());

        save_workspace_to_path(&workspace, &path).expect("workspace saves");
        let restored = load_workspace_from_path(&path).expect("workspace reloads");

        assert_eq!(restored.chart_settings_templates, vec![saved]);
        assert_eq!(restored.default_chart_settings, None);
        std::fs::remove_dir_all(path.parent().expect("temporary workspace parent"))
            .expect("temporary workspace cleanup");
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
