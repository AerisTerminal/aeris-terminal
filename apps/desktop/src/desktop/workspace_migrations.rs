//! The ordered workspace document migration chain (design 11 §4.3).
//!
//! Authority is the durable-container envelope `schema_version`; the payload
//! `schema_revision` (tag 8) carries the same value for diagnostics. Every 1.0.0-1.2.0
//! release wrote a bare protobuf file, which predates the envelope, so a bare document's
//! schema is its payload revision; the legacy `workspace-state.frame` is schema 0. Each step
//! is a pure function on the document, later steps only append, and a step never edits an
//! earlier one. `local_state`'s sanitize pass stays the last step after the chain.

use aeris_chart_integration::{AerisChartTheme, ChartIndicator, ChartThemeColors};
use aeris_contracts::{
    WorkspaceChartAppearanceState, WorkspaceChartState, WorkspaceChartStudyState,
    WorkspacePaneState, WorkspaceState,
};
use prost::Message as _;

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
pub(super) const CURRENT_WORKSPACE_SCHEMA: u32 = 3;

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
/// the caller's choice.
pub(super) fn migrate_workspace(mut workspace: WorkspaceState, from: u32) -> WorkspaceState {
    debug_assert!(from <= CURRENT_WORKSPACE_SCHEMA);
    if from < 2 {
        migrate_1_to_2(&mut workspace);
    }
    if from < 3 {
        migrate_2_to_3(&mut workspace);
    }
    workspace.schema_revision = CURRENT_WORKSPACE_SCHEMA;
    workspace
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

        let migrated = migrate_workspace(workspace, 1);
        assert_eq!(migrated.schema_revision, CURRENT_WORKSPACE_SCHEMA);
        let chart = migrated.workspace_tabs[0].panes[0]
            .chart
            .as_ref()
            .expect("migrated chart");
        assert_eq!(chart.studies.len(), 1);
        assert_eq!(chart.indicators, Vec::new());

        let again = migrate_workspace(migrated.clone(), CURRENT_WORKSPACE_SCHEMA);
        assert_eq!(again, migrated, "running the chain again changes nothing");
    }

    #[test]
    fn a_revision_2_payload_under_the_first_envelope_keeps_its_ticket_closed() {
        // Pre-release container files can carry a revision 2 payload under envelope schema 1;
        // the payload revision already records the order entry move, so it is not re-applied.
        let mut workspace = default_workspace();
        workspace.schema_revision = 2;
        workspace.workspace_tabs[0].panes[0].side_panel_visibility = 0b001;

        let migrated = migrate_workspace(workspace, 1);

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

        let migrated = migrate_workspace(workspace, 2);

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

        let migrated = migrate_workspace(workspace, 2);
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

        let migrated = migrate_workspace(second, 2);
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

        let migrated = migrate_workspace(workspace, 2);
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
}
