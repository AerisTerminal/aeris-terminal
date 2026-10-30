//! The ordered workspace document migration chain (design 11 §4.3).
//!
//! Authority is the durable-container envelope `schema_version`; the payload
//! `schema_revision` (tag 8) carries the same value for diagnostics. Every 1.0.0-1.2.0
//! release wrote a bare protobuf file, which predates the envelope, so a bare document's
//! schema is its payload revision; the legacy `workspace-state.frame` is schema 0. Each step
//! is a pure function on the document, later steps only append, and a step never edits an
//! earlier one; the schema 4 step needs the legacy chart-chrome file, so it takes that file's
//! payload as input and `local_state` reads it at the chain boundary. `local_state`'s
//! sanitize pass stays the last step after the chain.

use aeris_chart_integration::{AerisChartTheme, ChartColor, ChartIndicator, ChartThemeColors};
use aeris_contracts::{
    WorkspaceChartAppearanceState, WorkspaceChartState, WorkspaceChartStudyState,
    WorkspacePaneState, WorkspaceState,
};
use aeris_observability::diagnostic;
use aeris_platform_runtime::{DurableCandidate, DurableContainer, DurableFormat, DurableSource};
use prost::Message as _;
use std::path::Path;

use super::workspace_surface::{
    THEME_APPEARANCE_COLOR, default_runtime_study, runtime_managed_indicator,
};

/// The schema this build reads and writes; the envelope `schema_version` and the payload
/// `schema_revision` carry the same value.
///
/// - 1: the first `WorkspaceState` proto, with the order ticket docked inside the order book.
/// - 2: the order ticket moved to its own order entry panel.
/// - 3: chart appearance colors carry explicit theme provenance, and runtime-managed legacy
///   indicator entries are materialized into the native study graph, so restore reads both
///   plainly without re-deriving either.
/// - 4: chart chrome preferences fold from the legacy chart-chrome `key=value` file into the
///   document; the old file is never written again and stays in place.
/// - 5: chart colors still holding the retired Aeris Charts market palette follow the
///   theme's current up and down colors.
/// - 6: execution marks and left-extended order lines start off; every chart and template
///   turns both off once, and a later choice to show them persists.
pub(super) const CURRENT_WORKSPACE_SCHEMA: u32 = 6;

/// The schema whose step folds the legacy chart-chrome file into the document.
const CHART_CHROME_FOLD_SCHEMA: u32 = 4;

/// Whether the chain from `from` folds the legacy chart-chrome file, so the caller reads it.
pub(super) const fn folds_legacy_chart_chrome(from: u32) -> bool {
    from < CHART_CHROME_FOLD_SCHEMA
}

pub(super) const LEGACY_WORKSPACE_PROTOCOL_VERSION: u32 = 21;

/// The legacy `workspace-state.frame` payload envelope (document schema 0).
#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct LegacyWorkspaceEnvelope {
    #[prost(uint32, tag = "1")]
    pub(super) protocol_version: u32,
    #[prost(oneof = "legacy_workspace_envelope::Payload", tags = "5")]
    pub(super) payload: Option<legacy_workspace_envelope::Payload>,
}

pub(super) mod legacy_workspace_envelope {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Payload {
        #[prost(message, tag = "5")]
        WorkspaceState(super::WorkspaceState),
    }
}

/// The panes of every open tab and of every saved layout's arrangement.
pub(super) fn workspace_panes(
    workspace: &mut WorkspaceState,
) -> impl Iterator<Item = &mut WorkspacePaneState> {
    workspace
        .workspace_tabs
        .iter_mut()
        .chain(
            workspace
                .saved_layouts
                .iter_mut()
                .filter_map(|layout| layout.tab.as_mut()),
        )
        .flat_map(|tab| tab.panes.iter_mut())
}

/// Step 0: a legacy `workspace-state.frame` (big-endian length prefix, protocol 21 envelope)
/// becomes the schema 1 document.
pub(super) fn migrate_legacy_frame(
    bytes: &[u8],
    max_bytes: usize,
) -> Result<WorkspaceState, String> {
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
    if declared == 0 || declared > max_bytes || declared != bytes.len() - 4 {
        return Err("legacy workspace frame length is invalid".to_string());
    }
    let envelope =
        LegacyWorkspaceEnvelope::decode(&bytes[4..]).map_err(|error| error.to_string())?;
    if envelope.protocol_version != LEGACY_WORKSPACE_PROTOCOL_VERSION {
        return Err("legacy workspace protocol version is unsupported".to_string());
    }
    match envelope.payload {
        Some(legacy_workspace_envelope::Payload::WorkspaceState(workspace)) => Ok(workspace),
        None => Err("legacy workspace file does not contain workspace state".to_string()),
    }
}

/// The schema of a bare protobuf document: its payload revision, which the bare format never
/// carried past 2. A bare file claiming a newer revision is not a document any release wrote
/// and is rejected rather than guessed at.
pub(super) fn raw_proto_document_schema(workspace: &WorkspaceState) -> Result<u32, String> {
    match workspace.schema_revision {
        0..=2 => Ok(workspace.schema_revision.max(1)),
        revision => Err(format!(
            "bare workspace payload claims schema revision {revision}, which no bare file carries"
        )),
    }
}

/// The ordered chain from `from` to the current schema, in memory; persisting the result is
/// the caller's choice. `legacy_chrome` is the legacy chart-chrome file's payload when the
/// caller read one; only the schema 4 chart-chrome fold uses it.
pub(super) fn migrate_workspace(
    mut workspace: WorkspaceState,
    from: u32,
    legacy_chrome: Option<&str>,
) -> WorkspaceState {
    debug_assert!(from <= CURRENT_WORKSPACE_SCHEMA);
    if from < 2 {
        migrate_1_to_2(&mut workspace);
    }
    if from < 3 {
        migrate_2_to_3(&mut workspace);
    }
    if folds_legacy_chart_chrome(from) {
        fold_chart_chrome(&mut workspace, legacy_chrome);
    }
    if from < 5 {
        follow_current_market_palette(&mut workspace);
    }
    if from < 6 {
        start_trading_overlays_off(&mut workspace);
    }
    workspace.schema_revision = CURRENT_WORKSPACE_SCHEMA;
    workspace
}

/// The same bound as the workspace file.
const MAXIMUM_CHART_CHROME_FILE_BYTES: usize = 12 * 1_048_576;
/// The durable-container schema the legacy chart-chrome `key=value` payload was enveloped
/// under; a released 1.x build's bare file holds the same text.
const LEGACY_CHART_CHROME_CONTAINER_SCHEMA: u32 = 1;

/// The legacy chart-chrome `key=value` payload beside the workspace file, when one is
/// readable. The fold inspects read-only: the old file is never written, moved or
/// quarantined, and a payload a newer version wrote stays untouched and unfolded.
pub(super) fn legacy_chart_chrome_payload(workspace_path: &Path) -> Option<String> {
    let chrome_path = aeris_desktop::chart_chrome::chart_chrome_path(workspace_path.parent()?);
    let load = DurableContainer::new(
        chrome_path,
        MAXIMUM_CHART_CHROME_FILE_BYTES,
        LEGACY_CHART_CHROME_CONTAINER_SCHEMA,
    )
    .inspect(decode_legacy_chart_chrome);
    for rejection in &load.rejected {
        diagnostic!("Aeris chart preferences file rejected during the workspace fold: {rejection}");
    }
    if let Some(newer) = &load.newer_version {
        diagnostic!(
            "Aeris chart preferences {} were written by a newer version (schema {}); the workspace fold leaves them untouched",
            newer.path.display(),
            newer.schema_version
        );
        return None;
    }
    let (contents, source) = load.restored?;
    if source != DurableSource::Primary {
        diagnostic!("Aeris chart preferences folded from {source:?}");
    }
    Some(contents)
}

/// Both the enveloped schema-1 payload and a released version's bare file are the same
/// `key=value` text.
fn decode_legacy_chart_chrome(candidate: DurableCandidate<'_>) -> Result<String, String> {
    match candidate.format {
        DurableFormat::Legacy
        | DurableFormat::Envelope {
            schema_version: LEGACY_CHART_CHROME_CONTAINER_SCHEMA,
        } => {}
        DurableFormat::Envelope { schema_version } => {
            return Err(format!(
                "chart chrome schema version {schema_version} is unsupported"
            ));
        }
    }
    std::str::from_utf8(candidate.payload)
        .map(str::to_owned)
        .map_err(|error| error.to_string())
}

/// Step 3 to 4: the legacy chart-chrome file's preferences join the document. A document
/// that already carries chrome keeps it; without a readable legacy file the preferences
/// default on open.
fn fold_chart_chrome(workspace: &mut WorkspaceState, legacy_chrome: Option<&str>) {
    if workspace.chart_chrome.is_some() {
        return;
    }
    let Some(contents) = legacy_chrome else {
        return;
    };
    let preferences = aeris_desktop::chart_chrome::parse_chart_chrome_preferences(contents);
    workspace.chart_chrome = Some(preferences.to_workspace_chrome());
}

/// Revision 1 docked the order ticket inside the order book; revision 2 gave it the order
/// entry panel, so a revision 1 layout with the book open keeps its ticket. Pre-release
/// container files can carry a revision 2 payload under envelope schema 1 and stay untouched.
fn migrate_1_to_2(workspace: &mut WorkspaceState) {
    if workspace.schema_revision >= 2 {
        return;
    }
    let order_book = u32::from(super::SidePanel::OrderBook.persisted_bit());
    let order_entry = u32::from(super::SidePanel::OrderEntry.persisted_bit());
    for pane in workspace_panes(workspace) {
        if pane.side_panel_visibility & order_book != 0 {
            pane.side_panel_visibility |= order_entry;
        }
    }
}

/// Revision 3 records chart appearance colors and runtime-managed studies with their final
/// provenance, in every open tab, saved layout and chart settings template.
fn migrate_2_to_3(workspace: &mut WorkspaceState) {
    for pane in workspace_panes(workspace) {
        let Some(chart) = pane.chart.as_mut() else {
            continue;
        };
        if let Some(appearance) = chart.appearance.as_mut() {
            canonicalize_chart_appearance(appearance);
        }
        materialize_legacy_indicators(chart);
    }
    for template in &mut workspace.chart_settings_templates {
        if let Some(appearance) = template.appearance.as_mut() {
            canonicalize_chart_appearance(appearance);
        }
    }
}

/// The colors Aeris Charts reported for an unset line, area or baseline color before those
/// fields carried provenance (its line, market up and market down fallbacks). A layout of
/// that era pinned whichever fallback the engine reported, which painted an opaque area fill.
const LEGACY_SERIES_DEFAULT_COLORS: [&str; 3] = ["#2196f3", "#089981", "#f7525f"];

/// The theme palette slot an appearance role color binds to.
type ThemeRoleColor = fn(ChartThemeColors) -> &'static str;

/// A color that followed the theme when the document was written becomes the durable theme
/// marker; a concrete value is the user's pick and survives.
fn canonicalize_chart_appearance(appearance: &mut WorkspaceChartAppearanceState) {
    // Layouts saved before `area_bottom_color` existed have it empty; only those pinned the
    // engine's opaque series fallbacks.
    let legacy_series = appearance.area_bottom_color.is_empty();
    let roles: [(&mut String, ThemeRoleColor); 10] = [
        (&mut appearance.background_color, |colors| colors.background),
        (&mut appearance.text_color, |colors| colors.text),
        (&mut appearance.grid_color, |colors| colors.grid),
        (&mut appearance.crosshair_color, |colors| colors.crosshair),
        (&mut appearance.up_color, |colors| colors.bullish),
        (&mut appearance.down_color, |colors| colors.bearish),
        (&mut appearance.wick_up_color, |colors| colors.bullish),
        (&mut appearance.wick_down_color, |colors| colors.bearish),
        (&mut appearance.border_up_color, |colors| colors.bullish),
        (&mut appearance.border_down_color, |colors| colors.bearish),
    ];
    for (color, role) in roles {
        *color = canonical_role_color(color, role);
    }
    for color in [
        &mut appearance.line_color,
        &mut appearance.area_top_color,
        &mut appearance.area_bottom_color,
        &mut appearance.baseline_top_color,
        &mut appearance.baseline_bottom_color,
    ] {
        *color = canonical_series_color(color, legacy_series);
    }
}

/// Layouts written before typed provenance flattened a theme-following color into that
/// theme's concrete default, so either theme's default for the role follows the theme again.
fn canonical_role_color(color: &str, role: fn(ChartThemeColors) -> &'static str) -> String {
    let theme_default = [AerisChartTheme::Light, AerisChartTheme::Dark]
        .into_iter()
        .any(|theme| role(ChartThemeColors::for_theme(theme)).eq_ignore_ascii_case(color));
    if color.is_empty() || color == THEME_APPEARANCE_COLOR || theme_default {
        THEME_APPEARANCE_COLOR.to_string()
    } else {
        color.to_string()
    }
}

fn canonical_series_color(color: &str, legacy: bool) -> String {
    let legacy_default = legacy
        && LEGACY_SERIES_DEFAULT_COLORS
            .iter()
            .any(|fallback| fallback.eq_ignore_ascii_case(color));
    if color.is_empty() || color == THEME_APPEARANCE_COLOR || legacy_default {
        THEME_APPEARANCE_COLOR.to_string()
    } else {
        color.to_string()
    }
}

/// The market up colors Aeris Charts painted in both themes through schema 4: the up color,
/// its half-alpha volume tint and its light and dark subtle tints.
const RETIRED_MARKET_UP_COLORS: [&str; 4] = ["#089981", "#08998180", "#dcf5f0", "#193c37"];
/// The matching retired market down colors.
const RETIRED_MARKET_DOWN_COLORS: [&str; 4] = ["#f7525f", "#f7525f80", "#ffe2e2", "#532b2e"];

/// Whether `color` is one of `retired`, in any CSS spelling; an alpha within one step of a
/// retired one matches the rounding of a fractional `rgba()` alpha.
fn retired_market_color(color: &str, retired: &[&str]) -> bool {
    let Some(color) = ChartColor::parse_css(color) else {
        return false;
    };
    retired
        .iter()
        .filter_map(|retired| ChartColor::parse_css(retired))
        .any(|retired| {
            (retired.r(), retired.g(), retired.b()) == (color.r(), color.g(), color.b())
                && retired.a().abs_diff(color.a()) <= 1
        })
}

/// `color` replaced by `follow` when it holds a retired market color of its direction.
fn follow_retired(color: &mut String, retired: &[&str], follow: &str) {
    if retired_market_color(color, retired) {
        *color = follow.to_string();
    }
}

/// Step 4 to 5: Aeris Charts replaced its market up/down pair. Every chart color that still
/// holds the former pair (or a tint derived from it) recorded that era's default rather than
/// a pick, so it follows the theme again: appearance roles take the theme marker, and
/// indicator colors whose empty value follows the chart's up and down colors are cleared.
/// Any other color is the user's and survives.
fn follow_current_market_palette(workspace: &mut WorkspaceState) {
    let (up, down) = (
        &RETIRED_MARKET_UP_COLORS[..],
        &RETIRED_MARKET_DOWN_COLORS[..],
    );
    for pane in workspace_panes(workspace) {
        let Some(chart) = pane.chart.as_mut() else {
            continue;
        };
        if let Some(appearance) = chart.appearance.as_mut() {
            follow_current_market_appearance(appearance);
        }
        for volume in chart
            .indicators
            .iter_mut()
            .filter_map(|indicator| indicator.volume.as_mut())
        {
            follow_retired(&mut volume.up_color, up, "");
            follow_retired(&mut volume.down_color, down, "");
        }
        let Some(order_flow) = chart.order_flow.as_mut() else {
            continue;
        };
        if let Some(delta) = order_flow.cumulative_delta.as_mut() {
            follow_retired(&mut delta.up_color, up, "");
            follow_retired(&mut delta.down_color, down, "");
        }
        if let Some(book) = order_flow.order_book_depth_options.as_mut() {
            follow_retired(&mut book.bid_color, up, "");
            follow_retired(&mut book.ask_color, down, "");
        }
    }
    for template in &mut workspace.chart_settings_templates {
        if let Some(appearance) = template.appearance.as_mut() {
            follow_current_market_appearance(appearance);
        }
    }
}

/// The candle, wick, border and baseline roles that default to the market up or down color.
fn follow_current_market_appearance(appearance: &mut WorkspaceChartAppearanceState) {
    for color in [
        &mut appearance.up_color,
        &mut appearance.wick_up_color,
        &mut appearance.border_up_color,
        &mut appearance.baseline_top_color,
    ] {
        follow_retired(color, &RETIRED_MARKET_UP_COLORS, THEME_APPEARANCE_COLOR);
    }
    for color in [
        &mut appearance.down_color,
        &mut appearance.wick_down_color,
        &mut appearance.border_down_color,
        &mut appearance.baseline_bottom_color,
    ] {
        follow_retired(color, &RETIRED_MARKET_DOWN_COLORS, THEME_APPEARANCE_COLOR);
    }
}

/// Step 5 to 6: execution marks and left-extended order lines became opt-in. Through schema 5
/// both started shown and every save recorded the current value, so a stored value cannot
/// tell a choice from that default; every chart and template turns both off once. The order management lines keep
/// their stored value.
fn start_trading_overlays_off(workspace: &mut WorkspaceState) {
    for chart in workspace_panes(workspace).filter_map(|pane| pane.chart.as_mut()) {
        chart.show_execution_marks = Some(false);
        chart.extend_order_lines_left = Some(false);
    }
    for template in &mut workspace.chart_settings_templates {
        template.show_execution_marks = Some(false);
        template.extend_order_lines_left = Some(false);
    }
}

/// Each legacy indicator-list entry for a runtime-managed indicator becomes its default
/// durable study, unless the study graph already holds one with the same identifier.
/// Engine-managed entries (volume, profiles) and unrecognized kinds stay in the indicator
/// list the chart engine restores.
fn materialize_legacy_indicators(chart: &mut WorkspaceChartState) {
    if chart.indicators.is_empty() {
        return;
    }
    let mut durable_identifiers = std::collections::BTreeMap::<String, usize>::new();
    for study in &chart.studies {
        *durable_identifiers
            .entry(study.identifier.clone())
            .or_default() += 1;
    }
    // New identities sit above every existing one so the sanitizer's stable identity order
    // keeps them.
    let mut last_local_id = chart
        .studies
        .iter()
        .map(|study| study.local_id)
        .max()
        .unwrap_or(0);
    let mut retained = Vec::with_capacity(chart.indicators.len());
    for item in std::mem::take(&mut chart.indicators) {
        let Some(indicator) = ChartIndicator::from_identifier(&item.kind)
            .filter(|indicator| runtime_managed_indicator(*indicator))
        else {
            retained.push(item);
            continue;
        };
        let Some(defaults) = default_runtime_study(0, indicator, item.visible) else {
            retained.push(item);
            continue;
        };
        if let Some(remaining) = durable_identifiers.get_mut(&defaults.identifier)
            && *remaining > 0
        {
            *remaining -= 1;
            continue;
        }
        let Some(local_id) = last_local_id.checked_add(1) else {
            retained.push(item);
            continue;
        };
        last_local_id = local_id;
        chart.studies.push(WorkspaceChartStudyState {
            local_id,
            ..defaults
        });
    }
    chart.indicators = retained;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::local_state::default_workspace;

    fn workspace_with_chart(chart: WorkspaceChartState) -> WorkspaceState {
        let mut workspace = default_workspace();
        workspace.workspace_tabs[0].panes[0].chart = Some(chart);
        workspace
    }

    #[test]
    fn the_chain_is_idempotent_and_stamps_the_current_revision() {
        let mut workspace = workspace_with_chart(WorkspaceChartState {
            appearance: Some(WorkspaceChartAppearanceState {
                background_color: ChartThemeColors::for_theme(AerisChartTheme::Light)
                    .background
                    .to_string(),
                area_bottom_color: String::new(),
                line_color: "#2196f3".to_string(),
                ..WorkspaceChartAppearanceState::default()
            }),
            indicators: vec![aeris_contracts::WorkspaceChartIndicatorState {
                kind: ChartIndicator::Ema.identifier().to_string(),
                visible: true,
                volume: None,
            }],
            ..WorkspaceChartState::default()
        });
        workspace.schema_revision = 1;

        let migrated = migrate_workspace(workspace, 1, None);
        assert_eq!(migrated.schema_revision, CURRENT_WORKSPACE_SCHEMA);
        let chart = migrated.workspace_tabs[0].panes[0]
            .chart
            .as_ref()
            .expect("migrated chart");
        assert_eq!(chart.studies.len(), 1);
        assert_eq!(chart.indicators, Vec::new());

        let again = migrate_workspace(migrated.clone(), CURRENT_WORKSPACE_SCHEMA, None);
        assert_eq!(again, migrated, "running the chain again changes nothing");
    }

    #[test]
    fn a_revision_2_payload_under_the_first_envelope_keeps_its_ticket_closed() {
        // Pre-release container files can carry a revision 2 payload under envelope schema 1;
        // the payload revision already records the order entry move, so it is not re-applied.
        let mut workspace = default_workspace();
        workspace.schema_revision = 2;
        workspace.workspace_tabs[0].panes[0].side_panel_visibility = 0b001;

        let migrated = migrate_workspace(workspace, 1, None);

        assert_eq!(
            migrated.workspace_tabs[0].panes[0].side_panel_visibility, 0b001,
            "the order entry panel is not re-added to a payload that already records it"
        );
    }

    #[test]
    fn schema_2_flattened_theme_defaults_follow_the_theme_and_custom_picks_survive() {
        let flattened = WorkspaceChartAppearanceState {
            background_color: ChartThemeColors::for_theme(AerisChartTheme::Light)
                .background
                .to_string(),
            text_color: ChartThemeColors::for_theme(AerisChartTheme::Dark)
                .text
                .to_string(),
            up_color: "#abc123".to_string(),
            ..WorkspaceChartAppearanceState::default()
        };
        let mut workspace = workspace_with_chart(WorkspaceChartState {
            appearance: Some(flattened),
            ..WorkspaceChartState::default()
        });
        workspace.schema_revision = 2;
        workspace.chart_settings_templates =
            vec![aeris_contracts::WorkspaceChartSettingsTemplateState {
                name: "template".to_string(),
                appearance: Some(WorkspaceChartAppearanceState {
                    grid_color: ChartThemeColors::for_theme(AerisChartTheme::Dark)
                        .grid
                        .to_string(),
                    down_color: "#123abc".to_string(),
                    ..WorkspaceChartAppearanceState::default()
                }),
                ..Default::default()
            }];

        let migrated = migrate_workspace(workspace, 2, None);

        let chart = migrated.workspace_tabs[0].panes[0]
            .chart
            .as_ref()
            .expect("migrated chart")
            .appearance
            .as_ref()
            .expect("migrated appearance");
        assert_eq!(chart.background_color, THEME_APPEARANCE_COLOR);
        assert_eq!(chart.text_color, THEME_APPEARANCE_COLOR);
        assert_eq!(chart.up_color, "#abc123", "a custom pick survives");
        let template = migrated.chart_settings_templates[0]
            .appearance
            .as_ref()
            .expect("migrated template appearance");
        assert_eq!(template.grid_color, THEME_APPEARANCE_COLOR);
        assert_eq!(template.down_color, "#123abc");
    }

    #[test]
    fn schema_2_legacy_series_fallbacks_require_the_missing_bottom_fill() {
        let legacy = WorkspaceChartAppearanceState {
            line_color: "#2196F3".to_string(),
            area_top_color: "#089981".to_string(),
            area_bottom_color: String::new(),
            baseline_bottom_color: "#F7525F".to_string(),
            ..WorkspaceChartAppearanceState::default()
        };
        let mut newer = legacy.clone();
        newer.area_bottom_color = "#00000000".to_string();
        let mut workspace = workspace_with_chart(WorkspaceChartState {
            appearance: Some(legacy),
            ..WorkspaceChartState::default()
        });
        workspace.schema_revision = 2;
        let mut second = workspace_with_chart(WorkspaceChartState {
            appearance: Some(newer),
            ..WorkspaceChartState::default()
        });
        second.schema_revision = 2;

        let migrated = migrate_workspace(workspace, 2, None);
        let appearance = migrated.workspace_tabs[0].panes[0]
            .chart
            .as_ref()
            .and_then(|chart| chart.appearance.as_ref())
            .expect("migrated appearance");
        for color in [
            &appearance.line_color,
            &appearance.area_top_color,
            &appearance.area_bottom_color,
            &appearance.baseline_bottom_color,
        ] {
            assert_eq!(
                color, THEME_APPEARANCE_COLOR,
                "a layout without the bottom fill pinned the engine fallbacks"
            );
        }

        // Only the schema 3 step is under test: schema 5 retires the former market pair.
        let mut migrated = second;
        migrate_2_to_3(&mut migrated);
        let appearance = migrated.workspace_tabs[0].panes[0]
            .chart
            .as_ref()
            .and_then(|chart| chart.appearance.as_ref())
            .expect("migrated appearance");
        assert_eq!(
            appearance.line_color, "#2196F3",
            "a layout with a bottom fill saved those colors with intent"
        );
        assert_eq!(appearance.area_top_color, "#089981");
        assert_eq!(appearance.baseline_bottom_color, "#F7525F");
    }

    /// A schema 4 document whose open tab, saved layout and template hold the retired market
    /// pair in several spellings beside custom picks.
    fn schema_4_retired_market_palette_workspace() -> WorkspaceState {
        let appearance = WorkspaceChartAppearanceState {
            up_color: "#089981".to_string(),
            down_color: "#F7525F".to_string(),
            wick_up_color: "rgb(8, 153, 129)".to_string(),
            wick_down_color: THEME_APPEARANCE_COLOR.to_string(),
            border_up_color: "#abc123".to_string(),
            border_down_color: "#089981".to_string(),
            baseline_top_color: "#dcf5f0".to_string(),
            baseline_bottom_color: "#532b2e".to_string(),
            area_top_color: "#089981".to_string(),
            area_bottom_color: "#00000000".to_string(),
            ..WorkspaceChartAppearanceState::default()
        };
        let chart = WorkspaceChartState {
            appearance: Some(appearance.clone()),
            indicators: vec![aeris_contracts::WorkspaceChartIndicatorState {
                kind: ChartIndicator::Volume.identifier().to_string(),
                visible: true,
                volume: Some(aeris_contracts::WorkspaceVolumeSettingsState {
                    up_color: "#08998180".to_string(),
                    down_color: "rgba(247, 82, 95, 0.5)".to_string(),
                    average_color: "#089981".to_string(),
                    average_length: 20,
                    ..aeris_contracts::WorkspaceVolumeSettingsState::default()
                }),
            }],
            order_flow: Some(aeris_contracts::WorkspaceOrderFlowSettingsState {
                cumulative_delta: Some(aeris_contracts::WorkspaceCumulativeDeltaState {
                    up_color: "#089981".to_string(),
                    down_color: "#123456".to_string(),
                    ..aeris_contracts::WorkspaceCumulativeDeltaState::default()
                }),
                delta_histogram: Some(aeris_contracts::WorkspaceDeltaHistogramState {
                    up_color: "#089981".to_string(),
                    down_color: String::new(),
                }),
                order_book_depth_options: Some(aeris_contracts::WorkspaceOrderBookDepthState {
                    bid_color: "#089981".to_string(),
                    ask_color: "#F7525F80".to_string(),
                    ..aeris_contracts::WorkspaceOrderBookDepthState::default()
                }),
                ..aeris_contracts::WorkspaceOrderFlowSettingsState::default()
            }),
            ..WorkspaceChartState::default()
        };
        let mut workspace = workspace_with_chart(chart);
        workspace.schema_revision = 4;
        workspace.saved_layouts = vec![aeris_contracts::WorkspaceSavedLayoutState {
            layout_id: 1,
            tab: Some(workspace.workspace_tabs[0].clone()),
            ..aeris_contracts::WorkspaceSavedLayoutState::default()
        }];
        workspace.chart_settings_templates =
            vec![aeris_contracts::WorkspaceChartSettingsTemplateState {
                name: "template".to_string(),
                appearance: Some(appearance),
                ..Default::default()
            }];
        workspace
    }

    fn assert_follows_the_current_market_palette(chart: WorkspaceChartState) {
        let theme = THEME_APPEARANCE_COLOR;
        let appearance = chart.appearance.expect("migrated appearance");
        assert_eq!(appearance.up_color, theme);
        assert_eq!(
            appearance.down_color, theme,
            "any spelling of the retired pair"
        );
        assert_eq!(appearance.wick_up_color, theme);
        assert_eq!(appearance.wick_down_color, theme);
        assert_eq!(
            appearance.border_up_color, "#abc123",
            "a custom pick survives"
        );
        assert_eq!(
            appearance.border_down_color, "#089981",
            "the retired up color in a down role was the user's pick"
        );
        assert_eq!(
            appearance.baseline_top_color, theme,
            "a retired subtle tint"
        );
        assert_eq!(appearance.baseline_bottom_color, theme);
        assert_eq!(
            appearance.area_top_color, "#089981",
            "area colors default to the line color, so the pair was picked there"
        );
        let volume = chart.indicators[0].volume.clone().expect("volume settings");
        assert_eq!(
            volume.up_color, "",
            "the retired volume tint follows the chart"
        );
        assert_eq!(
            volume.down_color, "",
            "an rgba() spelling of the volume tint"
        );
        assert_eq!(
            volume.average_color, "#089981",
            "the average defaults to a line color"
        );
        let order_flow = chart.order_flow.expect("order flow");
        let delta = order_flow.cumulative_delta.expect("CVD settings");
        assert_eq!(delta.up_color, "");
        assert_eq!(delta.down_color, "#123456");
        assert_eq!(
            order_flow.delta_histogram.expect("delta settings").up_color,
            "#089981",
            "delta columns default to the order-flow colors, so the pair was picked there"
        );
        let book = order_flow
            .order_book_depth_options
            .expect("order book settings");
        assert_eq!((book.bid_color.as_str(), book.ask_color.as_str()), ("", ""));
    }

    #[test]
    fn schema_4_retired_market_colors_follow_the_theme_and_picks_survive() {
        let mut migrated = migrate_workspace(schema_4_retired_market_palette_workspace(), 4, None);

        let saved_chart = migrated.saved_layouts[0]
            .tab
            .as_ref()
            .map(|tab| tab.panes[0].chart.clone().expect("saved chart"))
            .expect("saved layout tab");
        assert_follows_the_current_market_palette(
            migrated.workspace_tabs[0].panes[0]
                .chart
                .clone()
                .expect("migrated chart"),
        );
        assert_follows_the_current_market_palette(saved_chart);
        let theme = THEME_APPEARANCE_COLOR;
        let template = migrated.chart_settings_templates[0]
            .appearance
            .as_ref()
            .expect("template appearance");
        assert_eq!(template.up_color, theme);
        assert_eq!(template.border_up_color, "#abc123");

        // A color picked after the migration is the user's, even when it is the retired one.
        if let Some(appearance) = migrated.workspace_tabs[0].panes[0]
            .chart
            .as_mut()
            .and_then(|chart| chart.appearance.as_mut())
        {
            appearance.up_color = "#089981".to_string();
        }
        let again = migrate_workspace(migrated.clone(), CURRENT_WORKSPACE_SCHEMA, None);
        assert_eq!(again, migrated, "a current document is never re-migrated");
    }

    #[test]
    fn schema_5_execution_marks_and_extended_order_lines_turn_off_once() {
        let mut workspace = workspace_with_chart(WorkspaceChartState {
            show_order_management_lines: Some(true),
            show_execution_marks: Some(true),
            extend_order_lines_left: Some(true),
            ..WorkspaceChartState::default()
        });
        workspace.schema_revision = 5;
        workspace.saved_layouts[0].tab = Some(workspace.workspace_tabs[0].clone());
        workspace.chart_settings_templates =
            vec![aeris_contracts::WorkspaceChartSettingsTemplateState {
                name: "template".to_string(),
                show_order_management_lines: Some(true),
                show_execution_marks: Some(true),
                extend_order_lines_left: Some(true),
                ..Default::default()
            }];

        let mut migrated = migrate_workspace(workspace, 5, None);

        let saved_chart = migrated.saved_layouts[0]
            .tab
            .as_ref()
            .and_then(|tab| tab.panes[0].chart.clone())
            .expect("saved layout chart");
        for chart in [
            migrated.workspace_tabs[0].panes[0]
                .chart
                .clone()
                .expect("migrated chart"),
            saved_chart,
        ] {
            assert_eq!(chart.show_execution_marks, Some(false));
            assert_eq!(chart.extend_order_lines_left, Some(false));
            assert_eq!(
                chart.show_order_management_lines,
                Some(true),
                "order management lines keep their stored value"
            );
        }
        let template = &migrated.chart_settings_templates[0];
        assert_eq!(template.show_execution_marks, Some(false));
        assert_eq!(template.extend_order_lines_left, Some(false));
        assert_eq!(template.show_order_management_lines, Some(true));

        // Showing them again after the migration is the user's choice and is never reverted.
        if let Some(chart) = migrated.workspace_tabs[0].panes[0].chart.as_mut() {
            chart.show_execution_marks = Some(true);
            chart.extend_order_lines_left = Some(true);
        }
        let again = migrate_workspace(migrated.clone(), CURRENT_WORKSPACE_SCHEMA, None);
        assert_eq!(again, migrated, "a current document is never re-migrated");
    }

    #[test]
    fn legacy_indicators_materialize_into_the_study_graph_above_existing_identities() {
        let durable_sma = default_runtime_study(7, ChartIndicator::Sma, true).expect("SMA study");
        let mut workspace = workspace_with_chart(WorkspaceChartState {
            studies: vec![durable_sma.clone()],
            indicators: vec![
                aeris_contracts::WorkspaceChartIndicatorState {
                    kind: ChartIndicator::Sma.identifier().to_string(),
                    visible: false,
                    volume: None,
                },
                aeris_contracts::WorkspaceChartIndicatorState {
                    kind: ChartIndicator::Ema.identifier().to_string(),
                    visible: true,
                    volume: None,
                },
                aeris_contracts::WorkspaceChartIndicatorState {
                    kind: ChartIndicator::Volume.identifier().to_string(),
                    visible: true,
                    volume: None,
                },
            ],
            ..WorkspaceChartState::default()
        });
        workspace.schema_revision = 2;

        let migrated = migrate_workspace(workspace, 2, None);
        let chart = migrated.workspace_tabs[0].panes[0]
            .chart
            .as_ref()
            .expect("migrated chart");

        assert_eq!(chart.studies.len(), 2);
        assert_eq!(chart.studies[0], durable_sma);
        assert_eq!(
            chart.studies[1],
            default_runtime_study(8, ChartIndicator::Ema, true).expect("EMA study"),
            "the unmatched legacy entry becomes its default study above the durable graph"
        );
        assert_eq!(
            chart
                .indicators
                .iter()
                .map(|item| item.kind.as_str())
                .collect::<Vec<_>>(),
            vec![ChartIndicator::Volume.identifier()],
            "engine-managed entries stay in the engine's list"
        );
    }

    /// A released 1.2.0 chart-chrome file, byte for byte.
    const GOLDEN_LEGACY_CHROME: &str = "indicator_name_labels=0\nindicator_value_labels=1\nindicator_price_lines=0\nchart_type=bars\nsymbol_search_futures=1\nsymbol_search_equities=0\nsymbol_search_scope=exchange:rithmic:CBOT\nwindow_frame=frameless\ndrawing_toolbar=0\ndrawing_favorites=trend_line,stamp:star\ndrawing_favorites_toolbar=0\ntrading_shortcuts=one_click\nchart_trade_buttons=0\ndrawing_favorites_origin=120.5,80\ndrawing_context_toolbar_origin=640,96.5\n";

    #[test]
    fn the_final_step_folds_the_legacy_chart_chrome_file_into_the_document() {
        let workspace = default_workspace();

        let migrated = migrate_workspace(workspace, 2, Some(GOLDEN_LEGACY_CHROME));

        assert_eq!(migrated.schema_revision, CURRENT_WORKSPACE_SCHEMA);
        let chrome = migrated.chart_chrome.as_ref().expect("chrome folded");
        assert_eq!(
            *chrome,
            aeris_contracts::WorkspaceChromeState {
                indicator_name_labels_visible: false,
                indicator_value_labels_visible: true,
                indicator_price_lines_visible: false,
                chart_type: "bars".to_string(),
                symbol_search_futures: true,
                symbol_search_equities: false,
                symbol_search_scope: "exchange:rithmic:CBOT".to_string(),
                window_frame: "frameless".to_string(),
                drawing_toolbar_visible: false,
                drawing_favorites: "trend_line,stamp:star".to_string(),
                drawing_favorites_toolbar_visible: false,
                drawing_favorites_origin: Some(aeris_contracts::WorkspaceChromePoint {
                    x: 120.5,
                    y: 80.0,
                }),
                trading_shortcuts: "one_click".to_string(),
                chart_trade_buttons_visible: false,
                drawing_context_toolbar_origin: Some(aeris_contracts::WorkspaceChromePoint {
                    x: 640.0,
                    y: 96.5,
                }),
            },
            "the fold carries the legacy values verbatim"
        );
        let preferences =
            aeris_desktop::chart_chrome::ChartChromePreferences::from_workspace_chrome(Some(
                chrome,
            ));
        assert_eq!(
            preferences.chart_type,
            aeris_chart_integration::ChartType::Bars,
            "the folded document restores the user's chart type"
        );

        // A minimal 1.0.0-era file folds with defaults for the keys it never had.
        let migrated = migrate_workspace(default_workspace(), 1, Some("chart_type=line\n"));
        let chrome = migrated.chart_chrome.as_ref().expect("chrome folded");
        assert_eq!(chrome.chart_type, "line");
        assert!(chrome.indicator_name_labels_visible);
        assert_eq!(chrome.window_frame, "framed");
        assert!(chrome.drawing_toolbar_visible);
    }

    #[test]
    fn the_chrome_fold_keeps_document_chrome_and_survives_a_missing_file() {
        let mut workspace = default_workspace();
        workspace.chart_chrome = Some(aeris_contracts::WorkspaceChromeState {
            chart_type: "line".to_string(),
            ..aeris_contracts::WorkspaceChromeState::default()
        });

        let migrated = migrate_workspace(workspace, 3, Some(GOLDEN_LEGACY_CHROME));
        assert_eq!(
            migrated
                .chart_chrome
                .as_ref()
                .map(|chrome| chrome.chart_type.as_str()),
            Some("line"),
            "a document that already carries chrome is never refolded"
        );

        let missing = migrate_workspace(default_workspace(), 3, None);
        assert_eq!(
            missing.chart_chrome, None,
            "without a readable legacy file the preferences default on open"
        );
        assert_eq!(missing.schema_revision, CURRENT_WORKSPACE_SCHEMA);
    }

    fn temporary_workspace_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "aeris-workspace-migrations-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ))
    }

    #[test]
    fn the_legacy_chrome_read_is_read_only_and_covers_bare_and_enveloped_files() {
        let root = temporary_workspace_root("chrome-fold");
        let workspace_path = root.join("workspace-state.pb");
        let chrome_path = aeris_desktop::chart_chrome::chart_chrome_path(&root);

        assert_eq!(legacy_chart_chrome_payload(&workspace_path), None);

        std::fs::create_dir_all(chrome_path.parent().expect("chrome directory"))
            .expect("chrome directory");
        std::fs::write(&chrome_path, GOLDEN_LEGACY_CHROME).expect("bare chrome file writes");
        assert_eq!(
            legacy_chart_chrome_payload(&workspace_path).as_deref(),
            Some(GOLDEN_LEGACY_CHROME),
            "a released bare key=value file folds"
        );
        assert_eq!(
            std::fs::read(&chrome_path).expect("chrome file remains"),
            GOLDEN_LEGACY_CHROME.as_bytes(),
            "the fold never writes the old chrome file"
        );
        assert!(
            !chrome_path.with_extension("bak").exists(),
            "a read-only fold leaves no backup behind"
        );

        // The enveloped schema 1 payload an M7.7-era build wrote folds the same way.
        DurableContainer::new(
            chrome_path.clone(),
            MAXIMUM_CHART_CHROME_FILE_BYTES,
            LEGACY_CHART_CHROME_CONTAINER_SCHEMA,
        )
        .write(LEGACY_CHART_CHROME_CONTAINER_SCHEMA, b"chart_type=line\n")
        .expect("enveloped chrome fixture writes");
        assert_eq!(
            legacy_chart_chrome_payload(&workspace_path).as_deref(),
            Some("chart_type=line\n")
        );

        // An unusable primary still folds the container's backup, exactly like the retired
        // loader recovered it, and nothing is quarantined or moved.
        let oversized = vec![b'a'; MAXIMUM_CHART_CHROME_FILE_BYTES + 1];
        std::fs::write(&chrome_path, &oversized).expect("oversized chrome fixture writes");
        assert_eq!(
            legacy_chart_chrome_payload(&workspace_path).as_deref(),
            Some(GOLDEN_LEGACY_CHROME),
            "the fold recovers the last good chrome from the container backup"
        );
        assert_eq!(
            std::fs::read(&chrome_path).expect("oversized chrome file remains"),
            oversized,
            "a rejected chrome file is not quarantined or moved by the fold"
        );

        // With no usable copy at all the fold is empty and the file stays put.
        std::fs::remove_file(chrome_path.with_extension("bak")).expect("backup removed");
        assert_eq!(legacy_chart_chrome_payload(&workspace_path), None);
        assert_eq!(
            std::fs::read(&chrome_path).expect("oversized chrome file remains"),
            oversized
        );

        std::fs::remove_dir_all(&root).expect("fixture root removed");
    }
}
