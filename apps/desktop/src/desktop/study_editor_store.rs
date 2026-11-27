//! Durable on-disk store and static pre-compile validator for Phase L user-authored
//! Rust study projects.
//!
//! Each project lives in `<root>/study_projects/<identifier>/` with:
//! - `manifest.json` (versioned [`StudyProjectManifest`])
//! - `lib.rs` (current editable Rust source)
//! - `last_good.rs` (optional last-known-good source preserved across failed edits)
//!
//! This module owns source persistence and static sandbox-policy diagnostics only;
//! it never calculates studies, opens provider sessions, or loads native libraries.

use aeris_contracts::{WorkspaceChartStudyState, workspace_study_setting_state};
use aeris_study_sdk::builders::{StudyDefinitionBuilder, StudyOutputBuilder, StudySettingBuilder};
use aeris_study_sdk::project::{
    MAXIMUM_STUDY_DIAGNOSTICS, MAXIMUM_STUDY_SOURCE_BYTES, MAXIMUM_USER_STUDY_PROJECTS,
    StudyDiagnostic, StudyDiagnosticSeverity, StudyProject, StudyProjectCapability,
    StudyProjectLibrary, StudyProjectManifest, StudyProjectTemplate, validate_study_source,
};
use aeris_study_sdk::sandbox::{STUDY_SANDBOX_ABI_VERSION, StudySandboxLimits};
use aeris_study_sdk::toolchain::{
    PreviewFixturePreset, STUDY_SANDBOX_TARGET_TRIPLE, STUDY_TOOLCHAIN_POLICY_VERSION,
    StudyArtifactCacheKey, StudyDevPackStatus, StudyPreviewFixture,
};
use aeris_study_sdk::wasm_runtime::{
    CompiledWasmStudy, MAX_WASM_SECONDARY_SERIES, WasmSecondaryRequestSpec, WasmStudyTelemetry,
    WasmStudyTelemetrySink,
};
use aeris_study_sdk::{
    BarSeriesKey, NativeStudyRegistration, StreamRequirements, StudyColor, StudyDecimal,
    StudyFillSpec, StudyInvalidationPolicy, StudyLevelSpec, StudyLineStyle, StudyMtfConfirmation,
    StudyMtfGapPolicy, StudyPaneTarget, StudyPlotKind, StudyPointStyle, StudySession,
    StudySettingChoiceOption, StudySettingSpec, StudySettingValue, StudySettings, StudySource,
    StudyTimeframe,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

const PROJECTS_DIR_NAME: &str = "study_projects";
const ARTIFACT_CACHE_DIR_NAME: &str = ".artifact_cache";
const MAXIMUM_RETAINED_ARTIFACT_CACHE_ENTRIES: usize = 128;
const MANIFEST_FILE_NAME: &str = "manifest.json";
const SOURCE_FILE_NAME: &str = "lib.rs";
const LAST_GOOD_FILE_NAME: &str = "last_good.rs";
const LAST_GOOD_WASM_FILE_NAME: &str = "last_good.wasm";

static BUILD_DIR_NONCE: AtomicU64 = AtomicU64::new(1);
static DEV_PACK_CACHE: Mutex<Option<StudyDevPackStatus>> = Mutex::new(None);

/// Creates a `Command` that never flashes a console window on Windows GUI builds.
fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Resolves a Rust toolchain binary (`rustc` or `rustup`) from `$CARGO_HOME/bin` or
/// `~/.cargo/bin` first so an in-app Dev Pack install works immediately without restarting Aeris,
/// falling back to standard `PATH` resolution.
fn resolve_toolchain_binary(name: &str) -> PathBuf {
    #[cfg(windows)]
    let file_name = format!("{name}.exe");
    #[cfg(not(windows))]
    let file_name = name.to_string();

    if let Ok(cargo_home) = std::env::var("CARGO_HOME") {
        let candidate = PathBuf::from(cargo_home).join("bin").join(&file_name);
        if candidate.is_file() {
            return candidate;
        }
    }
    #[cfg(windows)]
    let home = std::env::var("USERPROFILE").ok();
    #[cfg(not(windows))]
    let home = std::env::var("HOME").ok();

    if let Some(home_dir) = home {
        let candidate = PathBuf::from(home_dir)
            .join(".cargo")
            .join("bin")
            .join(&file_name);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(name)
}

#[derive(Debug, Deserialize, Serialize)]
struct StoredManifest {
    manifest_version: u32,
    identifier: String,
    name: String,
    sdk_compatibility_epoch: u32,
    implementation_revision: u32,
    capabilities: Vec<String>,
    last_good_source_sha256: Option<String>,
    last_good_artifact_sha256: Option<String>,
}

fn capability_to_str(cap: StudyProjectCapability) -> &'static str {
    match cap {
        StudyProjectCapability::MarketBars => "market_bars",
        StudyProjectCapability::MarketMicrostructure => "market_microstructure",
        StudyProjectCapability::MultiContextRequests => "multi_context_requests",
        StudyProjectCapability::SemanticScene => "semantic_scene",
        StudyProjectCapability::ChartViewportContext => "chart_viewport_context",
        StudyProjectCapability::StudyAlerts => "study_alerts",
    }
}

fn capability_from_str(value: &str) -> Option<StudyProjectCapability> {
    match value {
        "market_bars" => Some(StudyProjectCapability::MarketBars),
        "market_microstructure" => Some(StudyProjectCapability::MarketMicrostructure),
        "multi_context_requests" => Some(StudyProjectCapability::MultiContextRequests),
        "semantic_scene" => Some(StudyProjectCapability::SemanticScene),
        "chart_viewport_context" => Some(StudyProjectCapability::ChartViewportContext),
        "study_alerts" => Some(StudyProjectCapability::StudyAlerts),
        _ => None,
    }
}

impl From<&StudyProjectManifest> for StoredManifest {
    fn from(manifest: &StudyProjectManifest) -> Self {
        Self {
            manifest_version: manifest.manifest_version,
            identifier: manifest.identifier.clone(),
            name: manifest.name.clone(),
            sdk_compatibility_epoch: manifest.sdk_compatibility_epoch,
            implementation_revision: manifest.implementation_revision,
            capabilities: manifest
                .capabilities
                .iter()
                .map(|&cap| capability_to_str(cap).to_string())
                .collect(),
            last_good_source_sha256: manifest.last_good_source_sha256.clone(),
            last_good_artifact_sha256: manifest.last_good_artifact_sha256.clone(),
        }
    }
}

impl TryFrom<StoredManifest> for StudyProjectManifest {
    type Error = String;

    fn try_from(stored: StoredManifest) -> Result<Self, Self::Error> {
        let mut capabilities = BTreeSet::new();
        for raw in stored.capabilities {
            let cap = capability_from_str(&raw)
                .ok_or_else(|| format!("unknown study capability `{raw}`"))?;
            capabilities.insert(cap);
        }
        let manifest = Self {
            manifest_version: stored.manifest_version,
            identifier: stored.identifier,
            name: stored.name,
            sdk_compatibility_epoch: stored.sdk_compatibility_epoch,
            implementation_revision: stored.implementation_revision,
            capabilities,
            last_good_source_sha256: stored.last_good_source_sha256,
            last_good_artifact_sha256: stored.last_good_artifact_sha256,
        };
        manifest
            .validate()
            .map_err(|error| format!("invalid study manifest: {error:?}"))?;
        Ok(manifest)
    }
}

/// One parsed `@output:` declaration from a user study's header comments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedStudyOutputSpec {
    pub(crate) identifier: String,
    pub(crate) title: String,
    pub(crate) plot: StudyPlotKind,
    /// Optional CSS `#RRGGBB` or `#RRGGBBAA` color override.
    pub(crate) color: Option<String>,
}

/// One parsed secondary market request (`@request_tf:` or `@request_symbol:`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedSecondaryRequest {
    pub(crate) alias: String,
    /// Optional explicit symbol (`"instrument_id"` or `"provider_id/instrument_id"`). `None` = primary chart symbol.
    pub(crate) symbol: Option<String>,
    pub(crate) timeframe: StudyTimeframe,
    pub(crate) confirmation: StudyMtfConfirmation,
    pub(crate) gaps: StudyMtfGapPolicy,
}

/// Complete declarative presentation metadata parsed from a user study's header comments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedStudySpec {
    pub(crate) pane: StudyPaneTarget,
    pub(crate) line_width: u32,
    pub(crate) inputs: Vec<StudySettingSpec>,
    pub(crate) outputs: Vec<ParsedStudyOutputSpec>,
    pub(crate) fills: Vec<StudyFillSpec>,
    pub(crate) levels: Vec<StudyLevelSpec>,
    pub(crate) secondary_requests: Vec<ParsedSecondaryRequest>,
    pub(crate) requires_order_flow: bool,
    pub(crate) uses_per_point_colors: bool,
}

fn parse_decimal_literal(raw: &str) -> Option<StudyDecimal> {
    let trimmed = raw.trim();
    if let Some((whole, frac)) = trimmed.split_once('.') {
        let scale = u8::try_from(frac.len()).ok()?;
        let digits = format!("{whole}{frac}");
        let mantissa = digits.parse::<i64>().ok()?;
        Some(StudyDecimal { mantissa, scale })
    } else {
        let mantissa = trimmed.parse::<i64>().ok()?;
        Some(StudyDecimal { mantissa, scale: 0 })
    }
}

fn parse_line_style(raw: &str) -> StudyLineStyle {
    match raw.trim().to_ascii_lowercase().as_str() {
        "dashed" | "dash" => StudyLineStyle::Dashed,
        "dotted" | "dot" => StudyLineStyle::Dotted,
        _ => StudyLineStyle::Solid,
    }
}

fn parse_plot_kind(raw: &str) -> StudyPlotKind {
    match raw.trim().to_ascii_lowercase().as_str() {
        "histogram" | "hist" => StudyPlotKind::Histogram,
        "area" => StudyPlotKind::Area,
        "columns" | "bars" => StudyPlotKind::Columns,
        "stepline" | "step" => StudyPlotKind::StepLine,
        "circles" | "dots" => StudyPlotKind::Circles,
        "cross" => StudyPlotKind::Cross,
        _ => StudyPlotKind::Line,
    }
}

fn parse_study_source_literal(raw: &str) -> Option<StudySource> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "open" => Some(StudySource::Open),
        "high" => Some(StudySource::High),
        "low" => Some(StudySource::Low),
        "close" => Some(StudySource::Close),
        "hl2" => Some(StudySource::Hl2),
        "hlc3" => Some(StudySource::Hlc3),
        "ohlc4" => Some(StudySource::Ohlc4),
        "volume" | "vol" => Some(StudySource::Volume),
        _ => None,
    }
}

fn parse_hex_study_color(raw: &str) -> Option<StudyColor> {
    let hex = raw.trim().strip_prefix('#')?;
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(StudyColor { r, g, b, a: 255 })
        }
        8 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
            Some(StudyColor { r, g, b, a })
        }
        _ => None,
    }
}

/// Parses declarative presentation directives from the header comments of a user study:
/// - `// @pane: separate | price` (or `// @overlay: false | true`)
/// - `// @plot: line | histogram | columns | stepline | circles | cross`
/// - `// @width: 1..4`
/// - `// @output: <id> | <title> | <plot> | <#RRGGBB or #RRGGBBAA>` (up to 16 outputs)
/// - `// @fill: <first_idx>, <second_idx> | <#RRGGBBAA>` (up to 16 fills)
#[must_use]
pub(crate) fn parse_study_source_spec(source: &str, default_title: &str) -> ParsedStudySpec {
    let mut pane = StudyPaneTarget::Price;
    let mut default_plot = StudyPlotKind::Line;
    let mut line_width: u32 = 0;
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut fills = Vec::new();
    let mut levels = Vec::new();
    let mut secondary_requests = Vec::new();
    let mut requires_order_flow = false;

    for raw_line in source.lines().take(80) {
        let trimmed = raw_line.trim();
        let Some(comment) = trimmed.strip_prefix("//") else {
            continue;
        };
        let content = comment.trim();
        let lower = content.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("@pane:") {
            match value.trim() {
                "separate" | "dedicated" | "subpane" | "oscillator" => {
                    pane = StudyPaneTarget::Dedicated { group: 0 };
                }
                "price" | "overlay" => pane = StudyPaneTarget::Price,
                _ => {}
            }
        } else if let Some(value) = lower.strip_prefix("@overlay:") {
            if value.trim() == "false" {
                pane = StudyPaneTarget::Dedicated { group: 0 };
            } else if value.trim() == "true" {
                pane = StudyPaneTarget::Price;
            }
        } else if let Some(value) = lower.strip_prefix("@plot:") {
            default_plot = parse_plot_kind(value);
        } else if let Some(value) = lower.strip_prefix("@width:") {
            if let Ok(w) = value.trim().parse::<u32>() {
                line_width = w.clamp(1, 4);
            }
        } else if lower.starts_with("@input:") && inputs.len() < 32 {
            // Format: // @input: <id> | <int|float|bool|color|choice|source|string|timeframe|session> | <default> | <label> [| key=val ...]
            let body = content[7..].trim();
            let parts: Vec<&str> = body.split('|').map(str::trim).collect();
            if parts.len() >= 3 && !parts[0].is_empty() {
                let id = parts[0];
                let kind = parts[1].to_ascii_lowercase();
                let default_raw = parts[2];
                let mut label = id;
                let mut min_raw: Option<&str> = None;
                let mut max_raw: Option<&str> = None;
                let mut step_raw: Option<&str> = None;
                let mut options_raw: Option<&str> = None;
                let mut group_raw: Option<&str> = None;
                let mut tooltip_raw: Option<&str> = None;

                for (idx, part) in parts.iter().enumerate().skip(3) {
                    if let Some((k, v)) = part.split_once('=') {
                        let key = k.trim().to_ascii_lowercase();
                        let val = v.trim();
                        match key.as_str() {
                            "min" => min_raw = Some(val),
                            "max" => max_raw = Some(val),
                            "step" => step_raw = Some(val),
                            "options" | "choices" => options_raw = Some(val),
                            "group" => group_raw = Some(val),
                            "tooltip" | "desc" | "description" => tooltip_raw = Some(val),
                            _ => {}
                        }
                    } else if idx == 3 && !part.is_empty() {
                        label = part;
                    }
                }

                let mut builder = match kind.as_str() {
                    "int" | "integer" => default_raw.parse::<i64>().ok().map(|val| {
                        StudySettingBuilder::integer(id, val).integer_bounds(
                            min_raw.and_then(|s| s.parse::<i64>().ok()),
                            max_raw.and_then(|s| s.parse::<i64>().ok()),
                            step_raw.and_then(|s| s.parse::<i64>().ok()),
                        )
                    }),
                    "float" | "decimal" | "f64" => parse_decimal_literal(default_raw).map(|dec| {
                        StudySettingBuilder::decimal(id, dec).decimal_bounds(
                            min_raw.and_then(parse_decimal_literal),
                            max_raw.and_then(parse_decimal_literal),
                            step_raw.and_then(parse_decimal_literal),
                        )
                    }),
                    "bool" | "boolean" => default_raw
                        .parse::<bool>()
                        .ok()
                        .map(|b| StudySettingBuilder::boolean(id, b)),
                    "color" => {
                        parse_hex_study_color(default_raw).map(|c| StudySettingBuilder::color(id, c))
                    }
                    "choice" | "select" | "options" => {
                        let opts: Vec<StudySettingChoiceOption> = options_raw
                            .map(|raw| {
                                raw.split(',')
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty())
                                    .map(|s| StudySettingChoiceOption {
                                        identifier: s.to_string(),
                                        label: s.to_string(),
                                    })
                                    .collect()
                            })
                            .unwrap_or_else(|| {
                                vec![StudySettingChoiceOption {
                                    identifier: default_raw.to_string(),
                                    label: default_raw.to_string(),
                                }]
                            });
                        if opts.is_empty() {
                            None
                        } else {
                            Some(StudySettingBuilder::choice(id, default_raw, opts))
                        }
                    }
                    "source" | "src" => parse_study_source_literal(default_raw)
                        .map(|src| StudySettingBuilder::source(id, src, false)),
                    "string" | "text" | "str" => Some(StudySettingBuilder::text(id, default_raw)),
                    "timeframe" | "tf" => StudyTimeframe::parse_label(default_raw)
                        .map(|tf| StudySettingBuilder::timeframe(id, tf, true)),
                    "session" => StudySession::parse_label(default_raw)
                        .map(|sess| StudySettingBuilder::session(id, sess)),
                    _ => None,
                };
                if let Some(mut b) = builder.take() {
                    b = b.label(label);
                    if let Some(group) = group_raw.filter(|g| !g.is_empty()) {
                        b = b.group(group);
                    }
                    if let Some(tooltip) = tooltip_raw.filter(|t| !t.is_empty()) {
                        b = b.description(tooltip);
                    }
                    inputs.push(b.build());
                }
            }
        } else if lower.starts_with("@output:") && outputs.len() < 32 {
            let body = content[8..].trim();
            let parts: Vec<&str> = body.split('|').map(str::trim).collect();
            if let Some(id) = parts.first().filter(|s| !s.is_empty()) {
                let title = parts
                    .get(1)
                    .filter(|s| !s.is_empty())
                    .copied()
                    .unwrap_or(id);
                let plot = parts
                    .get(2)
                    .map_or(default_plot, |raw| parse_plot_kind(raw));
                let color = parts
                    .get(3)
                    .filter(|raw| parse_hex_study_color(raw).is_some())
                    .map(|raw| (*raw).to_string());
                outputs.push(ParsedStudyOutputSpec {
                    identifier: (*id).to_string(),
                    title: title.to_string(),
                    plot,
                    color,
                });
            }
        } else if lower.starts_with("@fill:") && fills.len() < 16 {
            let body = content[6..].trim();
            let parts: Vec<&str> = body.split('|').map(str::trim).collect();
            if parts.len() >= 2 {
                let pair: Vec<&str> = parts[0].split(',').map(str::trim).collect();
                if pair.len() == 2
                    && let (Ok(first), Ok(second), Some(color)) = (
                        pair[0].parse::<usize>(),
                        pair[1].parse::<usize>(),
                        parse_hex_study_color(parts[1]),
                    )
                {
                    fills.push(StudyFillSpec {
                        first_output_index: first,
                        second_output_index: second,
                        color,
                    });
                }
            }
        } else if lower.starts_with("@level:") && levels.len() < 16 {
            // Format: // @level: <price> | <title> | <solid|dashed|dotted> | <#RRGGBBAA>
            let body = content[7..].trim();
            let parts: Vec<&str> = body.split('|').map(str::trim).collect();
            if let Some(value) = parts.first().and_then(|s| parse_decimal_literal(s)) {
                let title = parts
                    .get(1)
                    .filter(|s| !s.is_empty())
                    .map(|s| (*s).to_string());
                let style = parts
                    .get(2)
                    .map_or(StudyLineStyle::Dashed, |s| parse_line_style(s));
                let color = parts
                    .get(3)
                    .and_then(|s| parse_hex_study_color(s))
                    .unwrap_or(StudyColor {
                        r: 148,
                        g: 163,
                        b: 184,
                        a: 180,
                    });
                levels.push(StudyLevelSpec {
                    value,
                    color,
                    style,
                    title,
                });
            }
        } else if lower.starts_with("@request_tf:")
            && secondary_requests.len() < MAX_WASM_SECONDARY_SERIES
        {
            // Format: // @request_tf: <alias> | <timeframe> [| confirmed|forming] [| carry|gaps]
            let body = content[12..].trim();
            let parts: Vec<&str> = body.split('|').map(str::trim).collect();
            if parts.len() >= 2
                && !parts[0].is_empty()
                && let Some(tf) = StudyTimeframe::parse_label(parts[1])
            {
                let confirmation = parts
                    .get(2)
                    .map(|s| s.to_ascii_lowercase())
                    .map_or(StudyMtfConfirmation::ConfirmedOnly, |s| {
                        if s == "forming" || s == "latest" {
                            StudyMtfConfirmation::LatestIncludingForming
                        } else {
                            StudyMtfConfirmation::ConfirmedOnly
                        }
                    });
                let gaps = parts
                    .get(3)
                    .map(|s| s.to_ascii_lowercase())
                    .map_or(StudyMtfGapPolicy::CarryForward, |s| {
                        if s == "gaps" || s == "gaps_on" {
                            StudyMtfGapPolicy::Gaps
                        } else {
                            StudyMtfGapPolicy::CarryForward
                        }
                    });
                secondary_requests.push(ParsedSecondaryRequest {
                    alias: parts[0].to_string(),
                    symbol: None,
                    timeframe: tf,
                    confirmation,
                    gaps,
                });
            }
        } else if lower.starts_with("@request_symbol:")
            && secondary_requests.len() < MAX_WASM_SECONDARY_SERIES
        {
            // Format: // @request_symbol: <alias> | <symbol> [| <timeframe>]
            let body = content[16..].trim();
            let parts: Vec<&str> = body.split('|').map(str::trim).collect();
            if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
                let tf = parts
                    .get(2)
                    .and_then(|s| StudyTimeframe::parse_label(s))
                    .unwrap_or(StudyTimeframe::CurrentChart);
                secondary_requests.push(ParsedSecondaryRequest {
                    alias: parts[0].to_string(),
                    symbol: Some(parts[1].to_string()),
                    timeframe: tf,
                    confirmation: StudyMtfConfirmation::ConfirmedOnly,
                    gaps: StudyMtfGapPolicy::CarryForward,
                });
            }
        } else if let Some(val) = lower.strip_prefix("@require:") {
            let cap = val.trim();
            if cap == "order_flow" || cap == "orderflow" || cap == "trades" {
                requires_order_flow = true;
            }
        }
    }

    if outputs.is_empty() {
        outputs.push(ParsedStudyOutputSpec {
            identifier: "value".to_string(),
            title: default_title.to_string(),
            plot: default_plot,
            color: None,
        });
    }
    let output_count = outputs.len();
    fills.retain(|f| {
        f.first_output_index < output_count
            && f.second_output_index < output_count
            && f.first_output_index != f.second_output_index
    });

    let uses_per_point_colors = source.lines().any(|line| {
        let code = line.split("//").next().unwrap_or("");
        code.contains("set_plot_color")
    });

    ParsedStudySpec {
        pane,
        line_width,
        inputs,
        outputs,
        fills,
        levels,
        secondary_requests,
        requires_order_flow,
        uses_per_point_colors,
    }
}

/// Builds a `NativeStudyRegistration` for a compiled sandboxed WASM study using its parsed spec
/// and optional typed setting overrides.
#[must_use]
pub(crate) fn build_wasm_study_registration(
    identifier: &str,
    series: &BarSeriesKey,
    spec: &ParsedStudySpec,
    overrides: BTreeMap<String, StudySettingValue>,
    compiled: CompiledWasmStudy,
) -> NativeStudyRegistration {
    build_wasm_study_registration_with_telemetry(identifier, series, spec, overrides, compiled, None)
}

/// Builds a `NativeStudyRegistration` for a compiled sandboxed WASM study with an optional
/// bounded telemetry sink so the Study Editor Console displays live fuel, timing, and `log_*` output.
#[must_use]
pub(crate) fn build_wasm_study_registration_with_telemetry(
    identifier: &str,
    series: &BarSeriesKey,
    spec: &ParsedStudySpec,
    overrides: BTreeMap<String, StudySettingValue>,
    compiled: CompiledWasmStudy,
    telemetry_sink: Option<WasmStudyTelemetrySink>,
) -> NativeStudyRegistration {
    let primary_streams = if spec.requires_order_flow {
        StreamRequirements::BARS.with(aeris_study_sdk::MarketStream::Trades)
    } else {
        StreamRequirements::BARS
    };
    let mut builder = StudyDefinitionBuilder::new(identifier)
        .market(series.clone(), primary_streams)
        .invalidation(StudyInvalidationPolicy::FromFirstChanged);
    let mut secondary_specs = Vec::new();
    for req in spec
        .secondary_requests
        .iter()
        .take(MAX_WASM_SECONDARY_SERIES)
    {
        let mut sec_series = series.clone();
        sec_series.period = req.timeframe.resolve(series.period);
        if let Some(sym) = &req.symbol {
            if let Some((prov, inst)) = sym.split_once('/') {
                sec_series.provider_id = prov.trim().to_string();
                sec_series.instrument_id = inst.trim().to_string();
            } else {
                sec_series.instrument_id = sym.trim().to_string();
            }
        }
        builder = builder.market(sec_series, StreamRequirements::BARS);
        secondary_specs.push(WasmSecondaryRequestSpec {
            confirmation: req.confirmation,
            gaps: req.gaps,
        });
    }
    for input in &spec.inputs {
        builder = builder.setting(input.clone());
    }
    for out in &spec.outputs {
        let mut out_builder =
            StudyOutputBuilder::new(&out.identifier, &out.title, out.plot).pane(spec.pane);
        if spec.uses_per_point_colors {
            out_builder = out_builder.point_style(StudyPointStyle::PerPoint);
        }
        builder = builder.output(out_builder.build());
    }
    let definition = builder.build();
    let settings = StudySettings::with_overrides(&definition.settings, overrides)
        .or_else(|_| StudySettings::defaults(&definition.settings))
        .unwrap_or_default();
    let param_identifiers: Vec<String> = spec.inputs.iter().map(|s| s.identifier.clone()).collect();
    NativeStudyRegistration {
        settings,
        definition,
        program: compiled.into_native_program_full(
            spec.outputs.len(),
            param_identifiers,
            secondary_specs,
            spec.requires_order_flow,
            spec.fills.clone(),
            spec.levels.clone(),
            StudySandboxLimits::default(),
            telemetry_sink,
        ),
    }
}

/// Computes the canonical lowercase SHA-256 hex digest of an arbitrary byte slice.
#[must_use]
pub(crate) fn bytes_sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// Computes the canonical lowercase SHA-256 hex digest of a Rust source string.
#[must_use]
pub(crate) fn source_sha256_hex(source: &str) -> String {
    bytes_sha256_hex(source.as_bytes())
}

/// Static pre-compile sandbox-policy check producing exact `line:column` diagnostics.
///
/// Rejects ambient OS access (`unsafe`, filesystem, network, subprocess, environment,
/// wall-clock system time, compile-time file inclusion) before invoking any toolchain.
#[must_use]
pub(crate) fn validate_study_source_static(source: &str) -> Vec<StudyDiagnostic> {
    let mut diagnostics = Vec::new();
    if let Err(error) = validate_study_source(source) {
        diagnostics.push(StudyDiagnostic {
            severity: StudyDiagnosticSeverity::Error,
            line: 1,
            column: 1,
            code: Some("AERIS_SOURCE_BOUNDS".to_string()),
            message: format!("source failed bounds validation: {error:?}"),
        });
        return diagnostics;
    }

    const FORBIDDEN_PATTERNS: [(&str, &str, &str); 8] = [
        (
            "unsafe",
            "AERIS_NO_UNSAFE",
            "`unsafe` blocks are prohibited in sandboxed user studies",
        ),
        (
            "std::fs",
            "AERIS_NO_FS",
            "filesystem access (`std::fs`) is denied in sandboxed studies",
        ),
        (
            "std::net",
            "AERIS_NO_NET",
            "network access (`std::net`) is denied in sandboxed studies",
        ),
        (
            "std::process",
            "AERIS_NO_PROC",
            "subprocess access (`std::process`) is denied in sandboxed studies",
        ),
        (
            "std::env",
            "AERIS_NO_ENV",
            "environment access (`std::env`) is denied in sandboxed studies",
        ),
        (
            "SystemTime",
            "AERIS_NO_WALL_CLOCK",
            "use `ctx.meta().evaluation_time_unix_nanos` instead of `SystemTime`",
        ),
        (
            "include_bytes!",
            "AERIS_NO_INCLUDE",
            "`include_bytes!` is denied in sandboxed studies",
        ),
        (
            "include_str!",
            "AERIS_NO_INCLUDE",
            "`include_str!` is denied in sandboxed studies",
        ),
    ];

    for (line_idx, raw_line) in source.lines().enumerate() {
        let code = raw_line.split("//").next().unwrap_or("");
        for (pattern, code_id, message) in FORBIDDEN_PATTERNS {
            if let Some(byte_col) = code.find(pattern) {
                if diagnostics.len() >= MAXIMUM_STUDY_DIAGNOSTICS {
                    return diagnostics;
                }
                diagnostics.push(StudyDiagnostic {
                    severity: StudyDiagnosticSeverity::Error,
                    line: u32::try_from(line_idx + 1).unwrap_or(u32::MAX),
                    column: u32::try_from(byte_col + 1).unwrap_or(u32::MAX),
                    code: Some(code_id.to_string()),
                    message: message.to_string(),
                });
            }
        }
    }
    diagnostics
}

/// Durable file-system owner for user-authored study projects under one data root.
#[derive(Clone, Debug)]
pub(crate) struct StudyProjectStore {
    root: PathBuf,
}

impl StudyProjectStore {
    #[must_use]
    pub(crate) fn new(data_root: &Path) -> Self {
        Self {
            root: data_root.join(PROJECTS_DIR_NAME),
        }
    }

    /// Loads all valid projects from disk up to [`MAXIMUM_USER_STUDY_PROJECTS`].
    ///
    /// One malformed or corrupt project directory is skipped and reported in the returned
    /// warnings vector without blocking unrelated valid projects.
    pub(crate) fn load_library(&self) -> (StudyProjectLibrary, Vec<String>) {
        let mut library = StudyProjectLibrary::new();
        let mut warnings = Vec::new();
        let Ok(entries) = fs::read_dir(&self.root) else {
            return (library, warnings);
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        dirs.sort();

        for dir in dirs {
            if library.len() >= MAXIMUM_USER_STUDY_PROJECTS {
                warnings.push(format!(
                    "study project library reached maximum of {MAXIMUM_USER_STUDY_PROJECTS} projects"
                ));
                break;
            }
            match load_one_project(&dir) {
                Ok(project) => {
                    let id = project.manifest.identifier.clone();
                    let name = project.manifest.name.clone();
                    if library
                        .create_from_template(&id, &name, StudyProjectTemplate::StatelessOverlay)
                        .is_ok()
                    {
                        let _ = library.update_source(&id, project.source);
                        if let (Some(src_sha), Some(art_sha)) = (
                            project.manifest.last_good_source_sha256,
                            project.manifest.last_good_artifact_sha256,
                        ) {
                            if let Some(last_good) = project.last_good_source {
                                let current = library.get(&id).map(|p| p.source.clone());
                                let _ = library.update_source(&id, last_good);
                                let _ = library.record_build_success(&id, src_sha, art_sha);
                                if let Some(current) = current {
                                    let _ = library.update_source(&id, current);
                                }
                            } else {
                                let _ = library.record_build_success(&id, src_sha, art_sha);
                            }
                        }
                    }
                }
                Err(detail) => warnings.push(detail),
            }
        }
        (library, warnings)
    }

    /// Atomically persists one validated study project to `<root>/<identifier>/`.
    ///
    /// # Errors
    /// Returns an error if validation fails or disk I/O fails.
    pub(crate) fn save_project(&self, project: &StudyProject) -> Result<(), String> {
        project
            .manifest
            .validate()
            .map_err(|error| format!("invalid project manifest: {error:?}"))?;
        validate_study_source(&project.source)
            .map_err(|error| format!("invalid project source: {error:?}"))?;
        if let Some(last_good) = &project.last_good_source {
            validate_study_source(last_good)
                .map_err(|error| format!("invalid last-good source: {error:?}"))?;
        }
        let dir = self.project_dir(&project.manifest.identifier)?;
        fs::create_dir_all(&dir).map_err(|error| format!("create project dir failed: {error}"))?;

        let stored = StoredManifest::from(&project.manifest);
        let manifest_json = serde_json::to_string_pretty(&stored)
            .map_err(|error| format!("serialize manifest failed: {error}"))?;
        atomic_write(&dir.join(MANIFEST_FILE_NAME), manifest_json.as_bytes())?;
        atomic_write(&dir.join(SOURCE_FILE_NAME), project.source.as_bytes())?;
        if let Some(last_good) = &project.last_good_source {
            atomic_write(&dir.join(LAST_GOOD_FILE_NAME), last_good.as_bytes())?;
        }
        Ok(())
    }

    /// Atomically saves the last-known-good compiled `.wasm` artifact for `identifier`
    /// and records its cache receipt under `.artifact_cache/`.
    pub(crate) fn save_last_good_wasm(
        &self,
        identifier: &str,
        source_sha256: &str,
        toolchain_identity: &str,
        wasm_bytes: &[u8],
    ) -> Result<(), String> {
        let dir = self.project_dir(identifier)?;
        fs::create_dir_all(&dir).map_err(|error| format!("create project dir failed: {error}"))?;
        atomic_write(&dir.join(LAST_GOOD_WASM_FILE_NAME), wasm_bytes)?;

        let cache_dir = self.root.join(ARTIFACT_CACHE_DIR_NAME);
        if fs::create_dir_all(&cache_dir).is_ok() {
            let key =
                StudyArtifactCacheKey::for_host(toolchain_identity, source_sha256, source_sha256);
            let stored = StoredCacheKey {
                policy_version: STUDY_TOOLCHAIN_POLICY_VERSION,
                sdk_compatibility_epoch: key.sdk_compatibility_epoch,
                sandbox_abi_version: STUDY_SANDBOX_ABI_VERSION,
                toolchain_identity: key.toolchain_identity.clone(),
                target_triple: key.target_triple.clone(),
                source_sha256: key.source_sha256.clone(),
                lockfile_sha256: key.lockfile_sha256.clone(),
            };
            if let Ok(json) = serde_json::to_string_pretty(&stored) {
                let _ = atomic_write(
                    &cache_dir.join(format!("{}.json", key.cache_slug())),
                    json.as_bytes(),
                );
            }
        }
        Ok(())
    }

    /// Loads the persisted `last_good.wasm` artifact for `identifier`, if present.
    pub(crate) fn load_last_good_wasm(&self, identifier: &str) -> Result<Option<Vec<u8>>, String> {
        let dir = self.project_dir(identifier)?;
        let path = dir.join(LAST_GOOD_WASM_FILE_NAME);
        if !path.exists() {
            return Ok(None);
        }
        fs::read(&path)
            .map(Some)
            .map_err(|error| format!("read last_good.wasm failed: {error}"))
    }

    /// Restores a persisted user-authored WASM study into a `NativeStudyRegistration`
    /// using its last-known-good `.wasm` artifact on disk so chart instances survive restarts.
    pub(crate) fn restore_wasm_study_registration(
        &self,
        state: &WorkspaceChartStudyState,
        current_series: &BarSeriesKey,
    ) -> Result<NativeStudyRegistration, String> {
        if state.identifier.starts_with("builtin.") {
            return Err("reserved built-in identifier cannot restore from user wasm store".into());
        }
        let Some(wasm_bytes) = self.load_last_good_wasm(&state.identifier)? else {
            return Err(format!(
                "no compiled wasm artifact found for user study `{}`",
                state.identifier
            ));
        };
        let compiled = CompiledWasmStudy::from_bytes(&wasm_bytes, StudySandboxLimits::default())
            .map_err(|err| format!("sandbox wasm validation failed: {err}"))?;
        let loaded_project = self
            .project_dir(&state.identifier)
            .ok()
            .and_then(|dir| load_one_project(&dir).ok());
        let display_name = loaded_project
            .as_ref()
            .map_or_else(|| state.identifier.clone(), |p| p.manifest.name.clone());
        let source_text = loaded_project
            .as_ref()
            .map(|p| p.last_good_source.as_deref().unwrap_or(&p.source))
            .unwrap_or("");
        let spec = parse_study_source_spec(source_text, &display_name);
        let mut overrides = BTreeMap::new();
        for setting in &state.settings {
            if let Some(val) = setting.value.as_ref() {
                let parsed = match val {
                    workspace_study_setting_state::Value::Integer(v) => {
                        Some(StudySettingValue::Integer(*v))
                    }
                    workspace_study_setting_state::Value::Boolean(b) => {
                        Some(StudySettingValue::Boolean(*b))
                    }
                    workspace_study_setting_state::Value::Decimal(d) => {
                        u8::try_from(d.scale).ok().map(|scale| {
                            StudySettingValue::Decimal(StudyDecimal {
                                mantissa: d.mantissa,
                                scale,
                            })
                        })
                    }
                    _ => None,
                };
                if let Some(v) = parsed {
                    overrides.insert(setting.identifier.clone(), v);
                }
            }
        }
        Ok(build_wasm_study_registration(
            &state.identifier,
            current_series,
            &spec,
            overrides,
            compiled,
        ))
    }

    /// Deletes one persisted study project directory.
    ///
    /// # Errors
    /// Returns an error if the identifier is invalid or removal fails.
    pub(crate) fn delete_project(&self, identifier: &str) -> Result<(), String> {
        let dir = self.project_dir(identifier)?;
        if dir.exists() {
            fs::remove_dir_all(&dir)
                .map_err(|error| format!("delete project dir failed: {error}"))?;
        }
        Ok(())
    }

    /// Prunes stale or incompatible cached artifact receipts under `.artifact_cache/` while
    /// keeping user project source files untouched.
    pub(crate) fn prune_stale_artifact_cache(&self, host_toolchain_identity: &str) -> usize {
        let cache_dir = self.root.join(ARTIFACT_CACHE_DIR_NAME);
        let Ok(entries) = fs::read_dir(&cache_dir) else {
            return 0;
        };
        let mut kept: Vec<PathBuf> = Vec::new();
        let mut removed = 0;
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let compatible = fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<StoredCacheKey>(&raw).ok())
                .is_some_and(|stored| {
                    StudyArtifactCacheKey {
                        policy_version: stored.policy_version,
                        sdk_compatibility_epoch: stored.sdk_compatibility_epoch,
                        sandbox_abi_version: stored.sandbox_abi_version,
                        toolchain_identity: stored.toolchain_identity,
                        target_triple: stored.target_triple,
                        source_sha256: stored.source_sha256,
                        lockfile_sha256: stored.lockfile_sha256,
                    }
                    .is_compatible_with_host(host_toolchain_identity)
                });
            if !compatible {
                if fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            } else {
                kept.push(path);
            }
        }
        kept.sort();
        if kept.len() > MAXIMUM_RETAINED_ARTIFACT_CACHE_ENTRIES {
            let excess = kept.len() - MAXIMUM_RETAINED_ARTIFACT_CACHE_ENTRIES;
            for path in kept.into_iter().take(excess) {
                if fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
        removed
    }

    fn project_dir(&self, identifier: &str) -> Result<PathBuf, String> {
        if identifier.is_empty()
            || identifier.contains('/')
            || identifier.contains('\\')
            || identifier.contains("..")
        {
            return Err("invalid study project identifier path".to_string());
        }
        Ok(self.root.join(identifier))
    }
}

fn load_one_project(dir: &Path) -> Result<StudyProject, String> {
    let manifest_raw = fs::read_to_string(dir.join(MANIFEST_FILE_NAME))
        .map_err(|error| format!("read manifest in {}: {error}", dir.display()))?;
    let stored: StoredManifest = serde_json::from_str(&manifest_raw)
        .map_err(|error| format!("parse manifest in {}: {error}", dir.display()))?;
    let manifest = StudyProjectManifest::try_from(stored)?;

    let source_path = dir.join(SOURCE_FILE_NAME);
    let metadata = fs::metadata(&source_path)
        .map_err(|error| format!("stat source in {}: {error}", dir.display()))?;
    if usize::try_from(metadata.len()).unwrap_or(usize::MAX) > MAXIMUM_STUDY_SOURCE_BYTES {
        return Err(format!("source in {} exceeds size bound", dir.display()));
    }
    let source = fs::read_to_string(&source_path)
        .map_err(|error| format!("read source in {}: {error}", dir.display()))?;
    validate_study_source(&source)
        .map_err(|error| format!("invalid source in {}: {error:?}", dir.display()))?;

    let last_good_path = dir.join(LAST_GOOD_FILE_NAME);
    let last_good_source = if last_good_path.exists() {
        let content = fs::read_to_string(&last_good_path)
            .map_err(|error| format!("read last_good in {}: {error}", dir.display()))?;
        validate_study_source(&content).ok().map(|()| content)
    } else {
        None
    };

    Ok(StudyProject {
        manifest,
        source,
        last_good_source,
        diagnostics: Vec::new(),
    })
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(|error| format!("write {} failed: {error}", tmp.display()))?;
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(&tmp, path).map_err(|error| format!("commit {} failed: {error}", path.display()))
}

const STUDY_PROJECT_BUNDLE_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredCacheKey {
    policy_version: u32,
    sdk_compatibility_epoch: u32,
    sandbox_abi_version: u32,
    toolchain_identity: String,
    target_triple: String,
    source_sha256: String,
    lockfile_sha256: String,
}

/// Probes whether a local Rust compiler and `wasm32-unknown-unknown` sandbox target are available.
/// Caches the detected status in-process to avoid spawning repeated `rustc` checks.
#[must_use]
pub(crate) fn probe_study_dev_pack() -> StudyDevPackStatus {
    if let Ok(guard) = DEV_PACK_CACHE.lock()
        && let Some(cached) = guard.as_ref()
    {
        return cached.clone();
    }
    probe_study_dev_pack_fresh()
}

/// Forces a fresh probe of the local Rust compiler and `wasm32-unknown-unknown` target,
/// updating the in-process cache.
#[must_use]
pub(crate) fn probe_study_dev_pack_fresh() -> StudyDevPackStatus {
    let status = probe_study_dev_pack_uncached();
    if let Ok(mut guard) = DEV_PACK_CACHE.lock() {
        *guard = Some(status.clone());
    }
    status
}

fn probe_study_dev_pack_uncached() -> StudyDevPackStatus {
    let rustc = resolve_toolchain_binary("rustc");
    let Ok(output) = hidden_command(&rustc).arg("--version").output() else {
        return StudyDevPackStatus::Unavailable;
    };
    if !output.status.success() {
        return StudyDevPackStatus::Unavailable;
    }
    let rustc_version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if rustc_version.is_empty() {
        return StudyDevPackStatus::Unavailable;
    }
    let target_ok = hidden_command(&rustc)
        .args([
            "--print",
            "target-libdir",
            "--target",
            STUDY_SANDBOX_TARGET_TRIPLE,
        ])
        .output()
        .is_ok_and(|out| {
            out.status.success() && {
                let libdir = String::from_utf8_lossy(&out.stdout);
                let trimmed = libdir.trim();
                !trimmed.is_empty() && Path::new(trimmed).exists()
            }
        });
    if target_ok {
        StudyDevPackStatus::Ready {
            rustc_version,
            target_triple: STUDY_SANDBOX_TARGET_TRIPLE.to_string(),
        }
    } else {
        StudyDevPackStatus::MissingSandboxTarget {
            rustc_version,
            required_target: STUDY_SANDBOX_TARGET_TRIPLE.to_string(),
        }
    }
}

/// Attempts to install the pinned zero-authority WebAssembly sandbox target via `rustup`
/// and re-probes the local Development Pack status.
pub(crate) fn install_wasm_sandbox_target() -> Result<StudyDevPackStatus, String> {
    let rustup = resolve_toolchain_binary("rustup");
    let output = hidden_command(&rustup)
        .args(["target", "add", STUDY_SANDBOX_TARGET_TRIPLE])
        .output()
        .map_err(|err| format!("failed to run rustup: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "rustup target add {STUDY_SANDBOX_TARGET_TRIPLE} failed: {}",
            stderr.trim()
        ));
    }
    Ok(probe_study_dev_pack_fresh())
}

/// Installs the minimal local Rust + `wasm32-unknown-unknown` Development Pack via the official
/// `rustup` bootstrapper (`--profile minimal`, skipping docs/clippy/rustfmt for fastest setup and
/// requiring no Visual Studio C++ Build Tools on Windows because WASM links via bundled `rust-lld`).
pub(crate) fn install_rust_dev_pack() -> Result<StudyDevPackStatus, String> {
    let rustup = resolve_toolchain_binary("rustup");
    if hidden_command(&rustup)
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
    {
        let out = hidden_command(&rustup)
            .args([
                "toolchain",
                "install",
                "stable",
                "--profile",
                "minimal",
                "--target",
                STUDY_SANDBOX_TARGET_TRIPLE,
            ])
            .output()
            .map_err(|err| format!("failed to run rustup: {err}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "rustup toolchain install failed: {}",
                stderr.trim()
            ));
        }
        let _ = hidden_command(&rustup).args(["default", "stable"]).output();
        let status = probe_study_dev_pack_fresh();
        return match status {
            StudyDevPackStatus::Ready { .. } => Ok(status),
            _ => install_wasm_sandbox_target(),
        };
    }

    #[cfg(windows)]
    {
        let nonce = BUILD_DIR_NONCE.fetch_add(1, Ordering::Relaxed);
        let init_exe = std::env::temp_dir().join(format!(
            "aeris_rustup_init_{}_{nonce}.exe",
            std::process::id()
        ));
        let dl = hidden_command("curl.exe")
            .args([
                "-fsSL",
                "--retry",
                "2",
                "-o",
                &init_exe.to_string_lossy(),
                "https://win.rustup.rs/x86_64",
            ])
            .output()
            .map_err(|err| format!("failed to download rustup-init.exe: {err}"))?;
        if !dl.status.success() || !init_exe.is_file() {
            let _ = fs::remove_file(&init_exe);
            let stderr = String::from_utf8_lossy(&dl.stderr);
            return Err(format!(
                "failed to download official rustup installer: {}",
                stderr.trim()
            ));
        }
        let install = hidden_command(&init_exe)
            .args([
                "-y",
                "--profile",
                "minimal",
                "--default-toolchain",
                "stable",
                "--target",
                STUDY_SANDBOX_TARGET_TRIPLE,
            ])
            .output();
        let _ = fs::remove_file(&init_exe);
        let install = install.map_err(|err| format!("failed to launch rustup-init.exe: {err}"))?;
        if !install.status.success() {
            let stderr = String::from_utf8_lossy(&install.stderr);
            return Err(format!(
                "rustup minimal installation failed: {}",
                stderr.trim()
            ));
        }
    }

    #[cfg(not(windows))]
    {
        let script = format!(
            "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable --target {STUDY_SANDBOX_TARGET_TRIPLE}"
        );
        let install = hidden_command("sh")
            .args(["-c", &script])
            .output()
            .map_err(|err| format!("failed to launch rustup bootstrapper: {err}"))?;
        if !install.status.success() {
            let stderr = String::from_utf8_lossy(&install.stderr);
            return Err(format!(
                "rustup minimal installation failed: {}",
                stderr.trim()
            ));
        }
    }

    let status = probe_study_dev_pack_fresh();
    if matches!(status, StudyDevPackStatus::Ready { .. }) {
        Ok(status)
    } else {
        Err("Rust installer completed, but `rustc` with `wasm32-unknown-unknown` was not detected yet. Try Re-check Rust toolchain.".to_string())
    }
}

/// Validated output of one controlled Rust -> WebAssembly study compilation.
#[derive(Clone, Debug)]
pub(crate) struct CompiledStudyArtifact {
    pub(crate) wasm_bytes: Vec<u8>,
    pub(crate) artifact_sha256: String,
    pub(crate) source_sha256: String,
    pub(crate) toolchain_identity: String,
    pub(crate) compiled: CompiledWasmStudy,
}

const WASM_HOST_API_PRELUDE: &str = r#"#[forbid(unsafe_code)]
mod user_study;

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

static PARAMS: [AtomicU64; 32] = [const { AtomicU64::new(f64::NAN.to_bits()) }; 32];
static STR_PARAMS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LINE_COUNTER: AtomicU32 = AtomicU32::new(0);
static VIEWPORT_LEFT: AtomicU32 = AtomicU32::new(0);
static VIEWPORT_RIGHT: AtomicU32 = AtomicU32::new(u32::MAX);
static META_TIMEFRAME_SECS: AtomicU32 = AtomicU32::new(60);
static META_PRICE_SCALE: AtomicU32 = AtomicU32::new(2);
static META_QTY_SCALE: AtomicU32 = AtomicU32::new(0);
static META_FLAGS: AtomicU32 = AtomicU32::new(0);
static META_SYMBOL: Mutex<String> = Mutex::new(String::new());
static TIMESTAMPS: Mutex<Vec<i64>> = Mutex::new(Vec::new());
static SECONDARY_DATA: Mutex<[Vec<f64>; 4]> = Mutex::new([Vec::new(), Vec::new(), Vec::new(), Vec::new()]);
static SECONDARY_LEN: [AtomicU32; 4] = [const { AtomicU32::new(0) }; 4];
static ORDER_FLOW_DATA: Mutex<Vec<f64>> = Mutex::new(Vec::new());
static ORDER_FLOW_LEN: AtomicU32 = AtomicU32::new(0);
static SCENE_BUF: Mutex<Vec<u8>> = Mutex::new(Vec::new());

const MAX_SCENE_BYTES: usize = 512 * 1024;
const OP_BOX: u8 = 1;
const OP_LINE: u8 = 2;
const OP_POLYLINE: u8 = 3;
const OP_LABEL: u8 = 4;
const OP_TABLE_CELL: u8 = 5;
const OP_BAR_COLOR: u8 = 6;
const OP_BG_COLOR: u8 = 7;
const OP_ALERT: u8 = 8;
const OP_LOG: u8 = 9;
const OP_BOX_EX: u8 = 10;
const OP_LINE_FILL: u8 = 11;
const OP_PLOT_COLOR: u8 = 12;
const OP_PLOT_SHAPE: u8 = 13;
const OP_STRATEGY_ENTRY: u8 = 14;
const OP_STRATEGY_EXIT: u8 = 15;
const OP_STRATEGY_CLOSE: u8 = 16;
const OP_STRATEGY_CLOSE_ALL: u8 = 17;

#[allow(dead_code)]
pub const LABEL_UP: u8 = 0;
#[allow(dead_code)]
pub const LABEL_DOWN: u8 = 1;
#[allow(dead_code)]
pub const LABEL_LEFT: u8 = 2;
#[allow(dead_code)]
pub const LABEL_RIGHT: u8 = 3;
#[allow(dead_code)]
pub const LABEL_TEXT_ONLY: u8 = 4;
#[allow(dead_code)]
pub const LABEL_CIRCLE: u8 = 5;
#[allow(dead_code)]
pub const LABEL_TRIANGLE_UP: u8 = 6;
#[allow(dead_code)]
pub const LABEL_TRIANGLE_DOWN: u8 = 7;
#[allow(dead_code)]
pub const LABEL_ARROW_UP: u8 = 8;
#[allow(dead_code)]
pub const LABEL_ARROW_DOWN: u8 = 9;
#[allow(dead_code)]
pub const LABEL_CROSS: u8 = 10;

#[allow(dead_code)]
pub const LOC_ABOVE_BAR: u8 = 0;
#[allow(dead_code)]
pub const LOC_BELOW_BAR: u8 = 1;
#[allow(dead_code)]
pub const LOC_ABSOLUTE: u8 = 2;

#[allow(dead_code)]
pub const LINE_SOLID: u8 = 0;
#[allow(dead_code)]
pub const LINE_DASHED: u8 = 1;
#[allow(dead_code)]
pub const LINE_DOTTED: u8 = 2;

#[allow(dead_code)]
pub const EXTEND_NONE: u8 = 0;
#[allow(dead_code)]
pub const EXTEND_RIGHT: u8 = 1;
#[allow(dead_code)]
pub const EXTEND_LEFT: u8 = 2;
#[allow(dead_code)]
pub const EXTEND_BOTH: u8 = 3;

#[allow(dead_code)]
pub const ALERT_ONCE_PER_BAR: u8 = 0;
#[allow(dead_code)]
pub const ALERT_ONCE_PER_BAR_CLOSE: u8 = 1;
#[allow(dead_code)]
pub const ALERT_EVERY_TICK: u8 = 2;

#[allow(dead_code)]
pub fn param(index: usize, default: f64) -> f64 {
    if let Some(slot) = PARAMS.get(index) {
        let v = f64::from_bits(slot.load(Ordering::Relaxed));
        if v.is_finite() {
            return v;
        }
    }
    default
}

#[allow(dead_code)]
pub fn param_int(index: usize, default: i64) -> i64 {
    let v = param(index, default as f64);
    if v.is_finite() { v.round() as i64 } else { default }
}

#[allow(dead_code)]
pub fn param_bool(index: usize, default: bool) -> bool {
    let v = param(index, if default { 1.0 } else { 0.0 });
    v != 0.0
}

#[allow(dead_code)]
pub fn param_color(index: usize, default_rgba: u32) -> u32 {
    let v = param(index, f64::from(default_rgba));
    if v.is_finite() && v >= 0.0 {
        v.round() as u32
    } else {
        default_rgba
    }
}

#[allow(dead_code)]
pub fn param_str(index: usize, default: &str) -> String {
    STR_PARAMS
        .lock()
        .ok()
        .and_then(|v| v.get(index).filter(|s| !s.is_empty()).cloned())
        .unwrap_or_else(|| default.to_string())
}

/// Resolves a `@input: ... | source | close | ...` parameter into a `Vec<f64>` series.
#[allow(dead_code)]
pub fn param_source(
    index: usize,
    open: &[f64],
    high: &[f64],
    low: &[f64],
    close: &[f64],
    volume: &[f64],
) -> Vec<f64> {
    let n = close.len();
    match param_int(index, 3) {
        0 => open.to_vec(),
        1 => high.to_vec(),
        2 => low.to_vec(),
        4 => (0..n).map(|i| (high[i] + low[i]) * 0.5).collect(),
        5 => (0..n).map(|i| (high[i] + low[i] + close[i]) / 3.0).collect(),
        6 => (0..n).map(|i| (open[i] + high[i] + low[i] + close[i]) * 0.25).collect(),
        7 => volume.to_vec(),
        _ => close.to_vec(),
    }
}

#[allow(dead_code)]
pub fn visible_range() -> (usize, usize) {
    (
        VIEWPORT_LEFT.load(Ordering::Relaxed) as usize,
        VIEWPORT_RIGHT.load(Ordering::Relaxed) as usize,
    )
}

/// Returns the bar's exchange timestamp in Unix nanoseconds (`0` if out of bounds).
#[allow(dead_code)]
pub fn time(bar_index: usize) -> i64 {
    TIMESTAMPS
        .lock()
        .ok()
        .and_then(|ts| ts.get(bar_index).copied())
        .unwrap_or(0)
}

/// Returns the bar's exchange timestamp in Unix milliseconds (`0` if out of bounds).
#[allow(dead_code)]
pub fn time_ms(bar_index: usize) -> i64 {
    time(bar_index) / 1_000_000
}

/// Returns the bar's exchange timestamp in Unix seconds (`0` if out of bounds).
#[allow(dead_code)]
pub fn time_secs(bar_index: usize) -> i64 {
    time(bar_index) / 1_000_000_000
}

/// Returns the UTC `(hour, minute, second)` of `bar_index`.
#[allow(dead_code)]
pub fn bar_time_hms(bar_index: usize) -> (u8, u8, u8) {
    let sec_of_day = time_secs(bar_index).rem_euclid(86_400);
    let h = (sec_of_day / 3600) as u8;
    let m = ((sec_of_day % 3600) / 60) as u8;
    let s = (sec_of_day % 60) as u8;
    (h, m, s)
}

/// Returns the UTC civil day index since 1970-01-01 for `bar_index`.
#[allow(dead_code)]
pub fn bar_day_number(bar_index: usize) -> i64 {
    time_secs(bar_index).div_euclid(86_400)
}

/// Returns the UTC `(year, month, day)` of `bar_index` (1-based month and day).
#[allow(dead_code)]
pub fn bar_ymd(bar_index: usize) -> (i32, u8, u8) {
    let z = bar_day_number(bar_index) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    let year = (y + i64::from(m <= 2)) as i32;
    (year, m, d)
}

/// Returns `true` if `bar_index` starts a new UTC calendar day relative to `bar_index - 1`.
#[allow(dead_code)]
pub fn is_new_day(bar_index: usize) -> bool {
    bar_index == 0 || bar_day_number(bar_index) != bar_day_number(bar_index - 1)
}

/// Returns `true` if `bar_index` starts a new Monday-aligned UTC week relative to `bar_index - 1`.
#[allow(dead_code)]
pub fn is_new_week(bar_index: usize) -> bool {
    if bar_index == 0 {
        return true;
    }
    let w_cur = (bar_day_number(bar_index) + 3).div_euclid(7);
    let w_prev = (bar_day_number(bar_index - 1) + 3).div_euclid(7);
    w_cur != w_prev
}

/// Returns `true` if `bar_index` starts a new UTC calendar month relative to `bar_index - 1`.
#[allow(dead_code)]
pub fn is_new_month(bar_index: usize) -> bool {
    if bar_index == 0 {
        return true;
    }
    let (y1, m1, _) = bar_ymd(bar_index);
    let (y0, m0, _) = bar_ymd(bar_index - 1);
    y1 != y0 || m1 != m0
}

/// Read-only instrument and execution metadata for the current chart.
#[allow(dead_code)]
pub mod syminfo {
    use super::*;

    pub fn symbol() -> String {
        META_SYMBOL
            .lock()
            .ok()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    pub fn timeframe_seconds() -> u32 {
        META_TIMEFRAME_SECS.load(Ordering::Relaxed)
    }

    pub fn price_scale() -> u8 {
        META_PRICE_SCALE.load(Ordering::Relaxed) as u8
    }

    pub fn quantity_scale() -> u8 {
        META_QTY_SCALE.load(Ordering::Relaxed) as u8
    }

    pub fn tick_size() -> f64 {
        10_f64.powi(-i32::from(price_scale()))
    }

    pub fn is_realtime() -> bool {
        (META_FLAGS.load(Ordering::Relaxed) & 1) != 0
    }

    pub fn is_confirmed_bar() -> bool {
        (META_FLAGS.load(Ordering::Relaxed) & 2) != 0
    }

    pub fn is_new_bar() -> bool {
        (META_FLAGS.load(Ordering::Relaxed) & 4) != 0
    }
}

fn read_secondary_col(slot: usize, col: usize, bar_index: usize) -> f64 {
    if slot >= 4 {
        return f64::NAN;
    }
    let n = SECONDARY_LEN[slot].load(Ordering::Relaxed) as usize;
    if bar_index >= n {
        return f64::NAN;
    }
    SECONDARY_DATA
        .lock()
        .ok()
        .and_then(|s| s[slot].get(col * n + bar_index).copied())
        .unwrap_or(f64::NAN)
}

#[allow(dead_code)]
pub fn sec_open(slot: usize, bar_index: usize) -> f64 {
    read_secondary_col(slot, 0, bar_index)
}
#[allow(dead_code)]
pub fn sec_high(slot: usize, bar_index: usize) -> f64 {
    read_secondary_col(slot, 1, bar_index)
}
#[allow(dead_code)]
pub fn sec_low(slot: usize, bar_index: usize) -> f64 {
    read_secondary_col(slot, 2, bar_index)
}
#[allow(dead_code)]
pub fn sec_close(slot: usize, bar_index: usize) -> f64 {
    read_secondary_col(slot, 3, bar_index)
}
#[allow(dead_code)]
pub fn sec_volume(slot: usize, bar_index: usize) -> f64 {
    read_secondary_col(slot, 4, bar_index)
}

/// Returns the full primary-aligned close slice of secondary series `slot` (`0..4`).
#[allow(dead_code)]
pub fn sec_close_series(slot: usize) -> Vec<f64> {
    if slot >= 4 {
        return Vec::new();
    }
    let n = SECONDARY_LEN[slot].load(Ordering::Relaxed) as usize;
    if n == 0 {
        return Vec::new();
    }
    SECONDARY_DATA
        .lock()
        .ok()
        .and_then(|s| s[slot].get(3 * n..4 * n).map(<[f64]>::to_vec))
        .unwrap_or_default()
}

/// Per-bar order-flow & microstructure accessors (`super::of::*`, requires `// @require: order_flow`).
#[allow(dead_code)]
pub mod of {
    use super::*;

    fn read_col(col: usize, bar_index: usize) -> f64 {
        let n = ORDER_FLOW_LEN.load(Ordering::Relaxed) as usize;
        if bar_index >= n {
            return f64::NAN;
        }
        ORDER_FLOW_DATA
            .lock()
            .ok()
            .and_then(|v| v.get(col * n + bar_index).copied())
            .unwrap_or(f64::NAN)
    }

    pub fn buy_vol(bar_index: usize) -> f64 {
        read_col(0, bar_index)
    }
    pub fn sell_vol(bar_index: usize) -> f64 {
        read_col(1, bar_index)
    }
    pub fn delta(bar_index: usize) -> f64 {
        read_col(2, bar_index)
    }
    pub fn trade_count(bar_index: usize) -> f64 {
        read_col(3, bar_index)
    }
    pub fn poc(bar_index: usize) -> f64 {
        read_col(4, bar_index)
    }

    /// Cumulative Volume Delta across bars `0..len`.
    pub fn cvd(len: usize) -> Vec<f64> {
        let mut out = Vec::with_capacity(len);
        let mut acc = 0.0;
        for i in 0..len {
            let d = delta(i);
            if d.is_finite() {
                acc += d;
            }
            out.push(acc);
        }
        out
    }
}

/// Backtesting strategy order intent helpers (`super::strategy::*`).
#[allow(dead_code)]
pub mod strategy {
    use super::*;

    pub fn entry(bar_index: usize, id: &str, is_long: bool, qty: f64) {
        entry_limit(bar_index, id, is_long, qty, f64::NAN);
    }

    pub fn entry_limit(bar_index: usize, id: &str, is_long: bool, qty: f64, limit_price: f64) {
        let Ok(idx) = u32::try_from(bar_index) else {
            return;
        };
        let bytes = id.as_bytes();
        let len = bytes.len().min(64);
        if let Ok(mut buf) = SCENE_BUF.lock()
            && buf.len() + 24 + len <= MAX_SCENE_BYTES
        {
            buf.push(OP_STRATEGY_ENTRY);
            buf.extend_from_slice(&idx.to_le_bytes());
            buf.push(if is_long { 0 } else { 1 });
            buf.extend_from_slice(&qty.to_le_bytes());
            buf.extend_from_slice(&limit_price.to_le_bytes());
            buf.extend_from_slice(&(len as u16).to_le_bytes());
            buf.extend_from_slice(&bytes[..len]);
        }
    }

    pub fn exit(
        bar_index: usize,
        id: &str,
        from_entry: &str,
        profit_price: f64,
        loss_price: f64,
    ) {
        let Ok(idx) = u32::try_from(bar_index) else {
            return;
        };
        let id_b = id.as_bytes();
        let id_len = id_b.len().min(64);
        let from_b = from_entry.as_bytes();
        let from_len = from_b.len().min(64);
        if let Ok(mut buf) = SCENE_BUF.lock()
            && buf.len() + 25 + id_len + from_len <= MAX_SCENE_BYTES
        {
            buf.push(OP_STRATEGY_EXIT);
            buf.extend_from_slice(&idx.to_le_bytes());
            buf.extend_from_slice(&profit_price.to_le_bytes());
            buf.extend_from_slice(&loss_price.to_le_bytes());
            buf.extend_from_slice(&(id_len as u16).to_le_bytes());
            buf.extend_from_slice(&id_b[..id_len]);
            buf.extend_from_slice(&(from_len as u16).to_le_bytes());
            buf.extend_from_slice(&from_b[..from_len]);
        }
    }

    pub fn close(bar_index: usize, id: &str) {
        let Ok(idx) = u32::try_from(bar_index) else {
            return;
        };
        let bytes = id.as_bytes();
        let len = bytes.len().min(64);
        if let Ok(mut buf) = SCENE_BUF.lock()
            && buf.len() + 7 + len <= MAX_SCENE_BYTES
        {
            buf.push(OP_STRATEGY_CLOSE);
            buf.extend_from_slice(&idx.to_le_bytes());
            buf.extend_from_slice(&(len as u16).to_le_bytes());
            buf.extend_from_slice(&bytes[..len]);
        }
    }

    pub fn close_all(bar_index: usize) {
        let Ok(idx) = u32::try_from(bar_index) else {
            return;
        };
        if let Ok(mut buf) = SCENE_BUF.lock()
            && buf.len() + 5 <= MAX_SCENE_BYTES
        {
            buf.push(OP_STRATEGY_CLOSE_ALL);
            buf.extend_from_slice(&idx.to_le_bytes());
        }
    }
}

/// Emits a deduplicated semantic alert event routed to Aeris's notification center & toaster.
#[allow(dead_code)]
pub fn emit_alert(bar_index: usize, condition_id: &str, message: &str, frequency: u8) {
    let Ok(idx) = u32::try_from(bar_index) else {
        return;
    };
    let cid = condition_id.as_bytes();
    let cid_len = cid.len().min(64);
    if cid_len == 0 {
        return;
    }
    let msg = message.as_bytes();
    let msg_len = msg.len().min(256);
    if let Ok(mut buf) = SCENE_BUF.lock() {
        if buf.len() + 10 + cid_len + msg_len > MAX_SCENE_BYTES {
            return;
        }
        buf.push(OP_ALERT);
        buf.extend_from_slice(&idx.to_le_bytes());
        buf.push(frequency);
        buf.extend_from_slice(&(cid_len as u16).to_le_bytes());
        buf.extend_from_slice(&cid[..cid_len]);
        buf.extend_from_slice(&(msg_len as u16).to_le_bytes());
        buf.extend_from_slice(&msg[..msg_len]);
    }
}

fn push_log(level: u8, message: &str) {
    let bytes = message.as_bytes();
    let len = bytes.len().min(256);
    if let Ok(mut buf) = SCENE_BUF.lock() {
        if buf.len() + 4 + len > MAX_SCENE_BYTES {
            return;
        }
        buf.push(OP_LOG);
        buf.push(level);
        buf.extend_from_slice(&(len as u16).to_le_bytes());
        buf.extend_from_slice(&bytes[..len]);
    }
}

#[allow(dead_code)]
pub fn log_info(message: &str) {
    push_log(0, message);
}

#[allow(dead_code)]
pub fn log_warn(message: &str) {
    push_log(1, message);
}

#[allow(dead_code)]
pub fn log_error(message: &str) {
    push_log(2, message);
}

/// Built-in Technical Analysis standard library (`super::ta::*`).
#[allow(dead_code)]
pub mod ta {
    pub fn sma(src: &[f64], period: usize) -> Vec<f64> {
        let n = src.len();
        let mut out = vec![f64::NAN; n];
        if period == 0 || n == 0 {
            return out;
        }
        let mut sum = 0.0;
        let mut bad = 0usize;
        for i in 0..n {
            let v = src[i];
            if v.is_finite() {
                sum += v;
            } else {
                bad += 1;
            }
            if i >= period {
                let old = src[i - period];
                if old.is_finite() {
                    sum -= old;
                } else {
                    bad = bad.saturating_sub(1);
                }
            }
            if i + 1 >= period && bad == 0 {
                out[i] = sum / (period as f64);
            }
        }
        out
    }

    pub fn ema(src: &[f64], period: usize) -> Vec<f64> {
        let n = src.len();
        let mut out = vec![f64::NAN; n];
        if period == 0 || n == 0 {
            return out;
        }
        let alpha = 2.0 / ((period as f64) + 1.0);
        let mut prev = f64::NAN;
        let mut seed_sum = 0.0;
        let mut seed_cnt = 0usize;
        for i in 0..n {
            let v = src[i];
            if !v.is_finite() {
                continue;
            }
            if !prev.is_finite() {
                seed_sum += v;
                seed_cnt += 1;
                if seed_cnt == period {
                    prev = seed_sum / (period as f64);
                    out[i] = prev;
                }
            } else {
                prev = alpha * v + (1.0 - alpha) * prev;
                out[i] = prev;
            }
        }
        out
    }

    /// Wilder's smoothing (Pine `ta.rma`).
    pub fn rma(src: &[f64], period: usize) -> Vec<f64> {
        let n = src.len();
        let mut out = vec![f64::NAN; n];
        if period == 0 || n == 0 {
            return out;
        }
        let alpha = 1.0 / (period as f64);
        let mut prev = f64::NAN;
        let mut seed_sum = 0.0;
        let mut seed_cnt = 0usize;
        for i in 0..n {
            let v = src[i];
            if !v.is_finite() {
                continue;
            }
            if !prev.is_finite() {
                seed_sum += v;
                seed_cnt += 1;
                if seed_cnt == period {
                    prev = seed_sum / (period as f64);
                    out[i] = prev;
                }
            } else {
                prev = alpha * v + (1.0 - alpha) * prev;
                out[i] = prev;
            }
        }
        out
    }

    pub fn wma(src: &[f64], period: usize) -> Vec<f64> {
        let n = src.len();
        let mut out = vec![f64::NAN; n];
        if period == 0 || n < period {
            return out;
        }
        let denom = ((period * (period + 1)) / 2) as f64;
        for i in (period - 1)..n {
            let mut sum = 0.0;
            let mut ok = true;
            for k in 0..period {
                let v = src[i + 1 - period + k];
                if !v.is_finite() {
                    ok = false;
                    break;
                }
                sum += v * ((k + 1) as f64);
            }
            if ok {
                out[i] = sum / denom;
            }
        }
        out
    }

    pub fn vwma(src: &[f64], volume: &[f64], period: usize) -> Vec<f64> {
        let n = src.len().min(volume.len());
        let mut out = vec![f64::NAN; n];
        if period == 0 || n < period {
            return out;
        }
        for i in (period - 1)..n {
            let mut pv = 0.0;
            let mut vsum = 0.0;
            for j in (i + 1 - period)..=i {
                if src[j].is_finite() && volume[j].is_finite() {
                    pv += src[j] * volume[j];
                    vsum += volume[j];
                }
            }
            if vsum > 0.0 {
                out[i] = pv / vsum;
            }
        }
        out
    }

    pub fn hma(src: &[f64], period: usize) -> Vec<f64> {
        if period <= 1 {
            return src.to_vec();
        }
        let half = (period / 2).max(1);
        let sqrt_p = (period as f64).sqrt().round().max(1.0) as usize;
        let w1 = wma(src, half);
        let w2 = wma(src, period);
        let diff: Vec<f64> = w1
            .iter()
            .zip(w2.iter())
            .map(|(&a, &b)| 2.0 * a - b)
            .collect();
        wma(&diff, sqrt_p)
    }

    pub fn vwap(high: &[f64], low: &[f64], close: &[f64], volume: &[f64]) -> Vec<f64> {
        let n = high.len().min(low.len()).min(close.len()).min(volume.len());
        let mut out = vec![f64::NAN; n];
        let mut pv = 0.0;
        let mut vsum = 0.0;
        for i in 0..n {
            let tp = (high[i] + low[i] + close[i]) / 3.0;
            if tp.is_finite() && volume[i].is_finite() && volume[i] >= 0.0 {
                pv += tp * volume[i];
                vsum += volume[i];
            }
            if vsum > 0.0 {
                out[i] = pv / vsum;
            }
        }
        out
    }

    /// Daily-resetting session VWAP using UTC calendar day boundaries (`super::is_new_day`).
    pub fn session_vwap(high: &[f64], low: &[f64], close: &[f64], volume: &[f64]) -> Vec<f64> {
        let n = high.len().min(low.len()).min(close.len()).min(volume.len());
        let mut out = vec![f64::NAN; n];
        let mut pv = 0.0;
        let mut vsum = 0.0;
        for i in 0..n {
            if super::is_new_day(i) {
                pv = 0.0;
                vsum = 0.0;
            }
            let tp = (high[i] + low[i] + close[i]) / 3.0;
            if tp.is_finite() && volume[i].is_finite() && volume[i] >= 0.0 {
                pv += tp * volume[i];
                vsum += volume[i];
            }
            if vsum > 0.0 {
                out[i] = pv / vsum;
            }
        }
        out
    }

    pub fn tr(high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
        let n = high.len().min(low.len()).min(close.len());
        let mut out = vec![f64::NAN; n];
        for i in 0..n {
            let hl = high[i] - low[i];
            if i == 0 || !close[i - 1].is_finite() {
                out[i] = hl;
            } else {
                let hc = (high[i] - close[i - 1]).abs();
                let lc = (low[i] - close[i - 1]).abs();
                out[i] = hl.max(hc).max(lc);
            }
        }
        out
    }

    pub fn atr(high: &[f64], low: &[f64], close: &[f64], period: usize) -> Vec<f64> {
        let true_range = tr(high, low, close);
        rma(&true_range, period)
    }

    pub fn variance(src: &[f64], period: usize) -> Vec<f64> {
        let n = src.len();
        let mut out = vec![f64::NAN; n];
        if period == 0 || n < period {
            return out;
        }
        let mean = sma(src, period);
        for i in (period - 1)..n {
            let m = mean[i];
            if !m.is_finite() {
                continue;
            }
            let mut acc = 0.0;
            for j in (i + 1 - period)..=i {
                let d = src[j] - m;
                acc += d * d;
            }
            out[i] = acc / (period as f64);
        }
        out
    }

    pub fn stdev(src: &[f64], period: usize) -> Vec<f64> {
        variance(src, period)
            .into_iter()
            .map(|v| if v.is_finite() { v.max(0.0).sqrt() } else { f64::NAN })
            .collect()
    }

    /// Bollinger Bands: returns `(upper, middle, lower)`.
    pub fn bb(src: &[f64], period: usize, mult: f64) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let mid = sma(src, period);
        let sd = stdev(src, period);
        let upper: Vec<f64> = mid.iter().zip(&sd).map(|(&m, &s)| m + mult * s).collect();
        let lower: Vec<f64> = mid.iter().zip(&sd).map(|(&m, &s)| m - mult * s).collect();
        (upper, mid, lower)
    }

    /// Keltner Channels: returns `(upper, middle, lower)`.
    pub fn kc(
        high: &[f64],
        low: &[f64],
        close: &[f64],
        period: usize,
        mult: f64,
    ) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let mid = ema(close, period);
        let a = atr(high, low, close, period);
        let upper: Vec<f64> = mid.iter().zip(&a).map(|(&m, &v)| m + mult * v).collect();
        let lower: Vec<f64> = mid.iter().zip(&a).map(|(&m, &v)| m - mult * v).collect();
        (upper, mid, lower)
    }

    /// Donchian Channels: returns `(upper, middle, lower)`.
    pub fn dc(high: &[f64], low: &[f64], period: usize) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let upper = highest(high, period);
        let lower = lowest(low, period);
        let mid: Vec<f64> = upper
            .iter()
            .zip(&lower)
            .map(|(&u, &l)| (u + l) * 0.5)
            .collect();
        (upper, mid, lower)
    }

    pub fn rsi(src: &[f64], period: usize) -> Vec<f64> {
        let n = src.len();
        let mut out = vec![f64::NAN; n];
        if period == 0 || n <= period {
            return out;
        }
        let mut gains = vec![0.0; n];
        let mut losses = vec![0.0; n];
        for i in 1..n {
            let d = src[i] - src[i - 1];
            if d.is_finite() {
                if d > 0.0 {
                    gains[i] = d;
                } else {
                    losses[i] = -d;
                }
            }
        }
        let avg_gain = rma(&gains[1..], period);
        let avg_loss = rma(&losses[1..], period);
        for i in 0..avg_gain.len() {
            let g = avg_gain[i];
            let l = avg_loss[i];
            if g.is_finite() && l.is_finite() {
                out[i + 1] = if l == 0.0 {
                    if g == 0.0 { 50.0 } else { 100.0 }
                } else {
                    100.0 - (100.0 / (1.0 + g / l))
                };
            }
        }
        out
    }

    /// MACD: returns `(macd_line, signal_line, histogram)`.
    pub fn macd(
        src: &[f64],
        fast: usize,
        slow: usize,
        signal: usize,
    ) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let f = ema(src, fast);
        let s = ema(src, slow);
        let line: Vec<f64> = f.iter().zip(&s).map(|(&a, &b)| a - b).collect();
        let sig = ema(&line, signal);
        let hist: Vec<f64> = line.iter().zip(&sig).map(|(&a, &b)| a - b).collect();
        (line, sig, hist)
    }

    /// Stochastic oscillator: returns `(%K, %D)`.
    pub fn stoch(
        high: &[f64],
        low: &[f64],
        close: &[f64],
        k_period: usize,
        d_period: usize,
    ) -> (Vec<f64>, Vec<f64>) {
        let n = high.len().min(low.len()).min(close.len());
        let hh = highest(high, k_period);
        let ll = lowest(low, k_period);
        let mut k = vec![f64::NAN; n];
        for i in 0..n {
            let range = hh[i] - ll[i];
            if range.is_finite() && range > 0.0 && close[i].is_finite() {
                k[i] = 100.0 * (close[i] - ll[i]) / range;
            }
        }
        let d = sma(&k, d_period);
        (k, d)
    }

    /// Directional Movement Index: returns `(adx, plus_di, minus_di)`.
    pub fn dmi(
        high: &[f64],
        low: &[f64],
        close: &[f64],
        di_len: usize,
        adx_len: usize,
    ) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let n = high.len().min(low.len()).min(close.len());
        let mut plus_dm = vec![0.0; n];
        let mut minus_dm = vec![0.0; n];
        for i in 1..n {
            let up = high[i] - high[i - 1];
            let down = low[i - 1] - low[i];
            if up > down && up > 0.0 {
                plus_dm[i] = up;
            }
            if down > up && down > 0.0 {
                minus_dm[i] = down;
            }
        }
        let tr_s = rma(&tr(high, low, close), di_len);
        let p_s = rma(&plus_dm, di_len);
        let m_s = rma(&minus_dm, di_len);
        let mut pdi = vec![f64::NAN; n];
        let mut mdi = vec![f64::NAN; n];
        let mut dx = vec![f64::NAN; n];
        for i in 0..n {
            if tr_s[i].is_finite() && tr_s[i] > 0.0 {
                pdi[i] = 100.0 * p_s[i] / tr_s[i];
                mdi[i] = 100.0 * m_s[i] / tr_s[i];
                let sum = pdi[i] + mdi[i];
                if sum > 0.0 {
                    dx[i] = 100.0 * (pdi[i] - mdi[i]).abs() / sum;
                }
            }
        }
        let adx = rma(&dx, adx_len);
        (adx, pdi, mdi)
    }

    pub fn roc(src: &[f64], period: usize) -> Vec<f64> {
        let n = src.len();
        let mut out = vec![f64::NAN; n];
        if period == 0 {
            return out;
        }
        for i in period..n {
            let prev = src[i - period];
            if prev.is_finite() && prev != 0.0 && src[i].is_finite() {
                out[i] = 100.0 * (src[i] - prev) / prev;
            }
        }
        out
    }

    pub fn mom(src: &[f64], period: usize) -> Vec<f64> {
        let n = src.len();
        let mut out = vec![f64::NAN; n];
        if period == 0 {
            return out;
        }
        for i in period..n {
            out[i] = src[i] - src[i - period];
        }
        out
    }

    pub fn cci(high: &[f64], low: &[f64], close: &[f64], period: usize) -> Vec<f64> {
        let n = high.len().min(low.len()).min(close.len());
        let tp: Vec<f64> = (0..n).map(|i| (high[i] + low[i] + close[i]) / 3.0).collect();
        let mean = sma(&tp, period);
        let mut out = vec![f64::NAN; n];
        if period == 0 || n < period {
            return out;
        }
        for i in (period - 1)..n {
            let m = mean[i];
            if !m.is_finite() {
                continue;
            }
            let mut md = 0.0;
            for j in (i + 1 - period)..=i {
                md += (tp[j] - m).abs();
            }
            md /= period as f64;
            if md > 0.0 {
                out[i] = (tp[i] - m) / (0.015 * md);
            }
        }
        out
    }

    pub fn highest(src: &[f64], period: usize) -> Vec<f64> {
        let n = src.len();
        let mut out = vec![f64::NAN; n];
        if period == 0 || n == 0 {
            return out;
        }
        for i in (period - 1).min(n)..n {
            let mut best = f64::NEG_INFINITY;
            for j in (i + 1 - period)..=i {
                if src[j].is_finite() && src[j] > best {
                    best = src[j];
                }
            }
            if best.is_finite() {
                out[i] = best;
            }
        }
        out
    }

    pub fn lowest(src: &[f64], period: usize) -> Vec<f64> {
        let n = src.len();
        let mut out = vec![f64::NAN; n];
        if period == 0 || n == 0 {
            return out;
        }
        for i in (period - 1).min(n)..n {
            let mut best = f64::INFINITY;
            for j in (i + 1 - period)..=i {
                if src[j].is_finite() && src[j] < best {
                    best = src[j];
                }
            }
            if best.is_finite() {
                out[i] = best;
            }
        }
        out
    }

    /// Bar offset `<= 0` to the highest value in `src[i+1-period..=i]` (matches Pine `ta.highestbars`).
    pub fn highestbars(src: &[f64], bar_index: usize, period: usize) -> isize {
        if period == 0 || bar_index >= src.len() {
            return 0;
        }
        let start = bar_index.saturating_sub(period - 1);
        let mut best_val = f64::NEG_INFINITY;
        let mut best_idx = bar_index;
        for j in start..=bar_index {
            if src[j].is_finite() && src[j] >= best_val {
                best_val = src[j];
                best_idx = j;
            }
        }
        (best_idx as isize) - (bar_index as isize)
    }

    /// Bar offset `<= 0` to the lowest value in `src[i+1-period..=i]` (matches Pine `ta.lowestbars`).
    pub fn lowestbars(src: &[f64], bar_index: usize, period: usize) -> isize {
        if period == 0 || bar_index >= src.len() {
            return 0;
        }
        let start = bar_index.saturating_sub(period - 1);
        let mut best_val = f64::INFINITY;
        let mut best_idx = bar_index;
        for j in start..=bar_index {
            if src[j].is_finite() && src[j] <= best_val {
                best_val = src[j];
                best_idx = j;
            }
        }
        (best_idx as isize) - (bar_index as isize)
    }

    /// Pivot high confirmed at `bar_index` (pivot bar is `bar_index - right`).
    pub fn pivothigh(src: &[f64], bar_index: usize, left: usize, right: usize) -> Option<f64> {
        if bar_index < left + right || bar_index >= src.len() {
            return None;
        }
        let p = bar_index - right;
        let pv = src[p];
        if !pv.is_finite() {
            return None;
        }
        for j in (p - left)..p {
            if src[j] > pv {
                return None;
            }
        }
        for j in (p + 1)..=bar_index {
            if src[j] >= pv {
                return None;
            }
        }
        Some(pv)
    }

    /// Pivot low confirmed at `bar_index` (pivot bar is `bar_index - right`).
    pub fn pivotlow(src: &[f64], bar_index: usize, left: usize, right: usize) -> Option<f64> {
        if bar_index < left + right || bar_index >= src.len() {
            return None;
        }
        let p = bar_index - right;
        let pv = src[p];
        if !pv.is_finite() {
            return None;
        }
        for j in (p - left)..p {
            if src[j] < pv {
                return None;
            }
        }
        for j in (p + 1)..=bar_index {
            if src[j] <= pv {
                return None;
            }
        }
        Some(pv)
    }

    pub fn crossover(a: &[f64], b: &[f64], i: usize) -> bool {
        i > 0 && i < a.len() && i < b.len() && a[i] > b[i] && a[i - 1] <= b[i - 1]
    }

    pub fn crossunder(a: &[f64], b: &[f64], i: usize) -> bool {
        i > 0 && i < a.len() && i < b.len() && a[i] < b[i] && a[i - 1] >= b[i - 1]
    }

    pub fn rising(src: &[f64], i: usize, len: usize) -> bool {
        if len == 0 || i < len || i >= src.len() {
            return false;
        }
        let cur = src[i];
        cur.is_finite() && ((i - len)..i).all(|j| src[j].is_finite() && cur > src[j])
    }

    pub fn falling(src: &[f64], i: usize, len: usize) -> bool {
        if len == 0 || i < len || i >= src.len() {
            return false;
        }
        let cur = src[i];
        cur.is_finite() && ((i - len)..i).all(|j| src[j].is_finite() && cur < src[j])
    }

    pub fn cum(src: &[f64]) -> Vec<f64> {
        let mut out = Vec::with_capacity(src.len());
        let mut acc = 0.0;
        for &v in src {
            if v.is_finite() {
                acc += v;
            }
            out.push(acc);
        }
        out
    }

    pub fn change(src: &[f64], i: usize, len: usize) -> f64 {
        if i >= len && i < src.len() {
            src[i] - src[i - len]
        } else {
            f64::NAN
        }
    }

    pub fn linreg(src: &[f64], period: usize, offset: usize) -> Vec<f64> {
        let n = src.len();
        let mut out = vec![f64::NAN; n];
        if period < 2 || n < period {
            return out;
        }
        let p = period as f64;
        let sum_x = p * (p - 1.0) * 0.5;
        let sum_xx = p * (p - 1.0) * (2.0 * p - 1.0) / 6.0;
        let denom = p * sum_xx - sum_x * sum_x;
        let eval_x = (period.saturating_sub(1 + offset)) as f64;
        for i in (period - 1)..n {
            let mut sum_y = 0.0;
            let mut sum_xy = 0.0;
            let mut ok = true;
            for k in 0..period {
                let y = src[i + 1 - period + k];
                if !y.is_finite() {
                    ok = false;
                    break;
                }
                sum_y += y;
                sum_xy += (k as f64) * y;
            }
            if ok && denom != 0.0 {
                let slope = (p * sum_xy - sum_x * sum_y) / denom;
                let intercept = (sum_y - slope * sum_x) / p;
                out[i] = intercept + slope * eval_x;
            }
        }
        out
    }
}

#[allow(dead_code)]
pub const fn rgba(hex_rgb: u32, alpha: u8) -> u32 {
    ((hex_rgb & 0x00FF_FFFF) << 8) | (alpha as u32)
}

fn clear_scene_buf() {
    LINE_COUNTER.store(0, Ordering::Relaxed);
    if let Ok(mut buf) = SCENE_BUF.lock() {
        buf.clear();
    }
}

#[allow(dead_code)]
pub fn draw_box(
    x1_bar: f64,
    top: f64,
    x2_bar: f64,
    bottom: f64,
    border_rgba: u32,
    fill_rgba: u32,
    border_width: u8,
) {
    if !x1_bar.is_finite() || !x2_bar.is_finite() || !top.is_finite() || !bottom.is_finite() {
        return;
    }
    if let Ok(mut buf) = SCENE_BUF.lock() {
        if buf.len() + 42 > MAX_SCENE_BYTES {
            return;
        }
        buf.push(OP_BOX);
        buf.extend_from_slice(&x1_bar.to_le_bytes());
        buf.extend_from_slice(&top.to_le_bytes());
        buf.extend_from_slice(&x2_bar.to_le_bytes());
        buf.extend_from_slice(&bottom.to_le_bytes());
        buf.extend_from_slice(&border_rgba.to_le_bytes());
        buf.extend_from_slice(&fill_rgba.to_le_bytes());
        buf.push(border_width);
    }
}

#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub fn draw_box_text(
    x1_bar: f64,
    top: f64,
    x2_bar: f64,
    bottom: f64,
    border_rgba: u32,
    fill_rgba: u32,
    border_width: u8,
    border_style: u8,
    text_rgba: u32,
    text: &str,
) {
    if !x1_bar.is_finite() || !x2_bar.is_finite() || !top.is_finite() || !bottom.is_finite() {
        return;
    }
    let bytes = text.as_bytes();
    let len = bytes.len().min(256);
    if let Ok(mut buf) = SCENE_BUF.lock() {
        if buf.len() + 49 + len > MAX_SCENE_BYTES {
            return;
        }
        buf.push(OP_BOX_EX);
        buf.extend_from_slice(&x1_bar.to_le_bytes());
        buf.extend_from_slice(&top.to_le_bytes());
        buf.extend_from_slice(&x2_bar.to_le_bytes());
        buf.extend_from_slice(&bottom.to_le_bytes());
        buf.extend_from_slice(&border_rgba.to_le_bytes());
        buf.extend_from_slice(&fill_rgba.to_le_bytes());
        buf.push(border_width);
        buf.push(border_style);
        buf.extend_from_slice(&text_rgba.to_le_bytes());
        buf.extend_from_slice(&(len as u16).to_le_bytes());
        buf.extend_from_slice(&bytes[..len]);
    }
}

/// Draws a line/ray and returns its wave-local handle for use with `draw_line_fill`.
#[allow(dead_code)]
pub fn draw_line(
    x1_bar: f64,
    y1: f64,
    x2_bar: f64,
    y2: f64,
    color_rgba: u32,
    width: u8,
    style: u8,
    extend: u8,
) -> u32 {
    if !x1_bar.is_finite() || !y1.is_finite() || !x2_bar.is_finite() || !y2.is_finite() {
        return u32::MAX;
    }
    if let Ok(mut buf) = SCENE_BUF.lock() {
        if buf.len() + 40 > MAX_SCENE_BYTES {
            return u32::MAX;
        }
        buf.push(OP_LINE);
        buf.extend_from_slice(&x1_bar.to_le_bytes());
        buf.extend_from_slice(&y1.to_le_bytes());
        buf.extend_from_slice(&x2_bar.to_le_bytes());
        buf.extend_from_slice(&y2.to_le_bytes());
        buf.extend_from_slice(&color_rgba.to_le_bytes());
        buf.push(width);
        buf.push(style);
        buf.push(extend);
        return LINE_COUNTER.fetch_add(1, Ordering::Relaxed);
    }
    u32::MAX
}

/// Shades the region between two lines returned by `draw_line` in the same wave.
#[allow(dead_code)]
pub fn draw_line_fill(first_line_handle: u32, second_line_handle: u32, color_rgba: u32) {
    if first_line_handle == u32::MAX || second_line_handle == u32::MAX || first_line_handle == second_line_handle {
        return;
    }
    if let Ok(mut buf) = SCENE_BUF.lock()
        && buf.len() + 13 <= MAX_SCENE_BYTES
    {
        buf.push(OP_LINE_FILL);
        buf.extend_from_slice(&first_line_handle.to_le_bytes());
        buf.extend_from_slice(&second_line_handle.to_le_bytes());
        buf.extend_from_slice(&color_rgba.to_le_bytes());
    }
}

/// Overrides the color of one bar on output series `output_index` (`StudyPointStyle::PerPoint`).
#[allow(dead_code)]
pub fn set_plot_color(output_index: usize, bar_index: usize, color_rgba: u32) {
    if let (Ok(out_u8), Ok(bar_u32)) = (u8::try_from(output_index), u32::try_from(bar_index))
        && let Ok(mut buf) = SCENE_BUF.lock()
        && buf.len() + 10 <= MAX_SCENE_BYTES
    {
        buf.push(OP_PLOT_COLOR);
        buf.push(out_u8);
        buf.extend_from_slice(&bar_u32.to_le_bytes());
        buf.extend_from_slice(&color_rgba.to_le_bytes());
    }
}

/// Plots a semantic marker/shape at `bar_index` (`LOC_ABOVE_BAR`, `LOC_BELOW_BAR`, or `LOC_ABSOLUTE`).
#[allow(dead_code)]
pub fn plot_shape(
    bar_index: usize,
    location: u8,
    price: f64,
    shape: u8,
    color_rgba: u32,
    text_rgba: u32,
    text: &str,
) {
    let Ok(idx) = u32::try_from(bar_index) else {
        return;
    };
    let bytes = text.as_bytes();
    let len = bytes.len().min(256);
    if let Ok(mut buf) = SCENE_BUF.lock()
        && buf.len() + 25 + len <= MAX_SCENE_BYTES
    {
        buf.push(OP_PLOT_SHAPE);
        buf.extend_from_slice(&idx.to_le_bytes());
        buf.push(location);
        buf.extend_from_slice(&price.to_le_bytes());
        buf.push(shape);
        buf.extend_from_slice(&color_rgba.to_le_bytes());
        buf.extend_from_slice(&text_rgba.to_le_bytes());
        buf.extend_from_slice(&(len as u16).to_le_bytes());
        buf.extend_from_slice(&bytes[..len]);
    }
}

pub fn draw_polyline(xs_bars: &[f64], ys_prices: &[f64], color_rgba: u32, width: u8, closed: bool) {
    let count = xs_bars.len().min(ys_prices.len()).min(512);
    if count < 2 {
        return;
    }
    if let Ok(mut buf) = SCENE_BUF.lock() {
        let needed = 9 + count * 16;
        if buf.len() + needed > MAX_SCENE_BYTES {
            return;
        }
        buf.push(OP_POLYLINE);
        buf.extend_from_slice(&(count as u16).to_le_bytes());
        buf.extend_from_slice(&color_rgba.to_le_bytes());
        buf.push(width);
        buf.push(u8::from(closed));
        for i in 0..count {
            buf.extend_from_slice(&xs_bars[i].to_le_bytes());
            buf.extend_from_slice(&ys_prices[i].to_le_bytes());
        }
    }
}

pub fn draw_label(
    x_bar: f64,
    y: f64,
    text: &str,
    bg_rgba: u32,
    text_rgba: u32,
    style: u8,
) {
    if !x_bar.is_finite() || !y.is_finite() {
        return;
    }
    let bytes = text.as_bytes();
    let len = bytes.len().min(256);
    if let Ok(mut buf) = SCENE_BUF.lock() {
        if buf.len() + 28 + len > MAX_SCENE_BYTES {
            return;
        }
        buf.push(OP_LABEL);
        buf.extend_from_slice(&x_bar.to_le_bytes());
        buf.extend_from_slice(&y.to_le_bytes());
        buf.extend_from_slice(&bg_rgba.to_le_bytes());
        buf.extend_from_slice(&text_rgba.to_le_bytes());
        buf.push(style);
        buf.extend_from_slice(&(len as u16).to_le_bytes());
        buf.extend_from_slice(&bytes[..len]);
    }
}

pub fn draw_table_cell(
    position: u8,
    cols: u16,
    rows: u16,
    col: u16,
    row: u16,
    text: &str,
    text_rgba: u32,
    bg_rgba: u32,
    align: u8,
) {
    let bytes = text.as_bytes();
    let len = bytes.len().min(256);
    if let Ok(mut buf) = SCENE_BUF.lock() {
        if buf.len() + 21 + len > MAX_SCENE_BYTES {
            return;
        }
        buf.push(OP_TABLE_CELL);
        buf.push(position);
        buf.extend_from_slice(&cols.to_le_bytes());
        buf.extend_from_slice(&rows.to_le_bytes());
        buf.extend_from_slice(&col.to_le_bytes());
        buf.extend_from_slice(&row.to_le_bytes());
        buf.extend_from_slice(&text_rgba.to_le_bytes());
        buf.extend_from_slice(&bg_rgba.to_le_bytes());
        buf.push(align);
        buf.extend_from_slice(&(len as u16).to_le_bytes());
        buf.extend_from_slice(&bytes[..len]);
    }
}

pub fn set_bar_color(bar_index: usize, color_rgba: u32) {
    if let Ok(idx) = u32::try_from(bar_index)
        && let Ok(mut buf) = SCENE_BUF.lock()
        && buf.len() + 9 <= MAX_SCENE_BYTES
    {
        buf.push(OP_BAR_COLOR);
        buf.extend_from_slice(&idx.to_le_bytes());
        buf.extend_from_slice(&color_rgba.to_le_bytes());
    }
}

pub fn set_bg_color(bar_index: usize, color_rgba: u32) {
    if let Ok(idx) = u32::try_from(bar_index)
        && let Ok(mut buf) = SCENE_BUF.lock()
        && buf.len() + 9 <= MAX_SCENE_BYTES
    {
        buf.push(OP_BG_COLOR);
        buf.extend_from_slice(&idx.to_le_bytes());
        buf.extend_from_slice(&color_rgba.to_le_bytes());
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_set_param(index: u32, value: f64) {
    if let Some(slot) = PARAMS.get(index as usize) {
        slot.store(value.to_bits(), Ordering::Relaxed);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_set_str_param(index: u32, ptr: u32, len: u32) {
    let idx = index as usize;
    if idx >= 32 {
        return;
    }
    if let Ok(mut table) = STR_PARAMS.lock() {
        if table.len() <= idx {
            table.resize_with(idx + 1, String::new);
        }
        table[idx].clear();
        if ptr != 0 && len > 0 {
            let clamped = (len as usize).min(256);
            let bytes = unsafe { std::slice::from_raw_parts(ptr as usize as *const u8, clamped) };
            table[idx].push_str(&String::from_utf8_lossy(bytes));
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_set_viewport(left: u32, right: u32) {
    VIEWPORT_LEFT.store(left, Ordering::Relaxed);
    VIEWPORT_RIGHT.store(right, Ordering::Relaxed);
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_set_timestamps(ptr: u32, count: u32) {
    if ptr == 0 || count == 0 {
        if let Ok(mut ts) = TIMESTAMPS.lock() {
            ts.clear();
        }
        return;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr as usize as *const i64, count as usize) };
    if let Ok(mut ts) = TIMESTAMPS.lock() {
        ts.clear();
        ts.extend_from_slice(slice);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_set_meta(
    timeframe_secs: u32,
    price_scale: u32,
    qty_scale: u32,
    flags: u32,
    symbol_ptr: u32,
    symbol_len: u32,
) {
    META_TIMEFRAME_SECS.store(timeframe_secs, Ordering::Relaxed);
    META_PRICE_SCALE.store(price_scale, Ordering::Relaxed);
    META_QTY_SCALE.store(qty_scale, Ordering::Relaxed);
    META_FLAGS.store(flags, Ordering::Relaxed);
    if let Ok(mut sym) = META_SYMBOL.lock() {
        sym.clear();
        if symbol_ptr != 0 && symbol_len > 0 {
            let len = (symbol_len as usize).min(64);
            let bytes = unsafe { std::slice::from_raw_parts(symbol_ptr as usize as *const u8, len) };
            sym.push_str(&String::from_utf8_lossy(bytes));
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_set_secondary(slot: u32, ptr: u32, bar_count: u32) {
    let s = slot as usize;
    let n = bar_count as usize;
    if s >= 4 || ptr == 0 || n == 0 {
        if s < 4 {
            SECONDARY_LEN[s].store(0, Ordering::Relaxed);
        }
        return;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr as usize as *const f64, n * 5) };
    if let Ok(mut sec) = SECONDARY_DATA.lock() {
        sec[s].clear();
        sec[s].extend_from_slice(slice);
        SECONDARY_LEN[s].store(bar_count, Ordering::Relaxed);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_set_order_flow(ptr: u32, bar_count: u32) {
    let n = bar_count as usize;
    if ptr == 0 || n == 0 {
        ORDER_FLOW_LEN.store(0, Ordering::Relaxed);
        return;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr as usize as *const f64, n * 5) };
    if let Ok(mut of) = ORDER_FLOW_DATA.lock() {
        of.clear();
        of.extend_from_slice(slice);
        ORDER_FLOW_LEN.store(bar_count, Ordering::Relaxed);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_scene_ptr() -> u32 {
    SCENE_BUF.lock().map_or(0, |buf| buf.as_ptr() as u32)
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_scene_len() -> u32 {
    SCENE_BUF.lock().map_or(0, |buf| buf.len() as u32)
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_abi_version() -> u32 {
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_alloc(byte_len: u32) -> u32 {
    let words = (byte_len as usize).div_ceil(std::mem::size_of::<f64>()).max(1);
    let mut buf = Vec::<f64>::with_capacity(words);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr as u32
}
"#;

const WASM_MULTI_ENTRYPOINT: &str = r#"
#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_calculate(bars_ptr: u32, bar_count: u32, out_ptr: u32) -> i32 {
    aeris_study_calculate_multi(bars_ptr, bar_count, out_ptr, 1)
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_calculate_multi(
    bars_ptr: u32,
    bar_count: u32,
    out_ptr: u32,
    output_count: u32,
) -> i32 {
    clear_scene_buf();
    let n = bar_count as usize;
    let count = (output_count as usize).clamp(1, 32);
    if n == 0 {
        return 0;
    }
    let base = bars_ptr as usize as *const f64;
    let out_base = out_ptr as usize as *mut f64;
    let (open, high, low, close, volume, flat_out) = unsafe {
        (
            std::slice::from_raw_parts(base, n),
            std::slice::from_raw_parts(base.add(n), n),
            std::slice::from_raw_parts(base.add(n * 2), n),
            std::slice::from_raw_parts(base.add(n * 3), n),
            std::slice::from_raw_parts(base.add(n * 4), n),
            std::slice::from_raw_parts_mut(out_base, n * count),
        )
    };
    let mut slices: Vec<&mut [f64]> = flat_out.chunks_exact_mut(n).collect();
    user_study::calculate_multi(open, high, low, close, volume, &mut slices);
    0
}
"#;

fn source_uses_multi_output_entrypoint(source: &str) -> bool {
    source.lines().any(|line| {
        let code = line.split("//").next().unwrap_or("");
        code.contains("fn calculate_multi")
    })
}

fn source_uses_incremental_entrypoint(source: &str) -> bool {
    source.lines().any(|line| {
        let code = line.split("//").next().unwrap_or("");
        code.contains("fn on_bar")
    })
}

const WASM_INCREMENTAL_ENTRYPOINT: &str = r#"
#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_on_bar(
    bar_index: u32,
    flags: u32,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    timestamp_ns: i64,
    out_ptr: u32,
    output_count: u32,
) -> i32 {
    if bar_index == 0 {
        clear_scene_buf();
    }
    let idx = bar_index as usize;
    if let Ok(mut ts) = TIMESTAMPS.lock() {
        if ts.len() <= idx {
            ts.resize(idx + 1, 0);
        }
        ts[idx] = timestamp_ns;
    }
    let count = (output_count as usize).clamp(1, 32);
    let out = unsafe { std::slice::from_raw_parts_mut(out_ptr as usize as *mut f64, count) };
    let is_new_bar = (flags & 1) != 0;
    let is_confirmed = (flags & 2) != 0;
    user_study::on_bar(idx, is_new_bar, is_confirmed, open, high, low, close, volume, out);
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_calculate(bars_ptr: u32, bar_count: u32, out_ptr: u32) -> i32 {
    aeris_study_calculate_multi(bars_ptr, bar_count, out_ptr, 1)
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_calculate_multi(
    bars_ptr: u32,
    bar_count: u32,
    out_ptr: u32,
    output_count: u32,
) -> i32 {
    clear_scene_buf();
    let n = bar_count as usize;
    let count = (output_count as usize).clamp(1, 32);
    if n == 0 {
        return 0;
    }
    let base = bars_ptr as usize as *const f64;
    let out_base = out_ptr as usize as *mut f64;
    let (open, high, low, close, volume, flat_out) = unsafe {
        (
            std::slice::from_raw_parts(base, n),
            std::slice::from_raw_parts(base.add(n), n),
            std::slice::from_raw_parts(base.add(n * 2), n),
            std::slice::from_raw_parts(base.add(n * 3), n),
            std::slice::from_raw_parts(base.add(n * 4), n),
            std::slice::from_raw_parts_mut(out_base, n * count),
        )
    };
    let mut row_out = vec![f64::NAN; count];
    for i in 0..n {
        row_out.fill(f64::NAN);
        user_study::on_bar(i, true, true, open[i], high[i], low[i], close[i], volume[i], &mut row_out);
        for c in 0..count {
            flat_out[c * n + i] = row_out[c];
        }
    }
    0
}
"#;

const WASM_SINGLE_ENTRYPOINT: &str = r#"
#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_calculate(bars_ptr: u32, bar_count: u32, out_ptr: u32) -> i32 {
    clear_scene_buf();
    let n = bar_count as usize;
    if n == 0 {
        return 0;
    }
    let base = bars_ptr as usize as *const f64;
    let out_base = out_ptr as usize as *mut f64;
    let (open, high, low, close, volume, out) = unsafe {
        (
            std::slice::from_raw_parts(base, n),
            std::slice::from_raw_parts(base.add(n), n),
            std::slice::from_raw_parts(base.add(n * 2), n),
            std::slice::from_raw_parts(base.add(n * 3), n),
            std::slice::from_raw_parts(base.add(n * 4), n),
            std::slice::from_raw_parts_mut(out_base, n),
        )
    };
    user_study::calculate(open, high, low, close, volume, out);
    0
}
"#;

#[derive(Deserialize)]
struct RustcJsonDiagnostic {
    #[serde(default)]
    level: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    code: Option<RustcJsonCode>,
    #[serde(default)]
    spans: Vec<RustcJsonSpan>,
}

#[derive(Deserialize)]
struct RustcJsonCode {
    code: String,
}

#[derive(Deserialize)]
struct RustcJsonSpan {
    #[serde(default)]
    file_name: String,
    #[serde(default)]
    line_start: u32,
    #[serde(default)]
    column_start: u32,
    #[serde(default)]
    is_primary: bool,
}

fn parse_rustc_json_diagnostics(stderr: &str) -> Vec<StudyDiagnostic> {
    let mut out = Vec::new();
    for line in stderr.lines() {
        if out.len() >= MAXIMUM_STUDY_DIAGNOSTICS {
            break;
        }
        let Ok(diag) = serde_json::from_str::<RustcJsonDiagnostic>(line) else {
            continue;
        };
        let severity = match diag.level.as_str() {
            "error" | "error: internal compiler error" => StudyDiagnosticSeverity::Error,
            "warning" => StudyDiagnosticSeverity::Warning,
            _ => continue,
        };
        let primary = diag
            .spans
            .iter()
            .find(|span| span.is_primary && span.file_name.ends_with("user_study.rs"))
            .or_else(|| diag.spans.iter().find(|span| span.is_primary))
            .or_else(|| diag.spans.first());
        let (line_no, col_no) = primary.map_or((1, 1), |span| {
            (span.line_start.max(1), span.column_start.max(1))
        });
        out.push(StudyDiagnostic {
            severity,
            line: line_no,
            column: col_no,
            code: diag.code.map(|c| c.code),
            message: diag.message,
        });
    }
    if out.is_empty() && !stderr.trim().is_empty() {
        out.push(StudyDiagnostic {
            severity: StudyDiagnosticSeverity::Error,
            line: 1,
            column: 1,
            code: Some("AERIS_RUSTC_ERROR".to_string()),
            message: stderr.lines().take(3).collect::<Vec<_>>().join(" | "),
        });
    }
    out
}

/// Compiles user-authored Rust study source into a zero-authority WebAssembly module
/// (`wasm32-unknown-unknown`) using a controlled Aeris-owned ABI v1 harness, and validates
/// the resulting `.wasm` bytes inside `wasmi`.
///
/// # Errors
/// Returns bounded structured [`StudyDiagnostic`]s on static policy failure, missing dev pack,
/// `rustc` compilation error, or `wasmi` zero-authority / ABI validation failure.
pub(crate) fn compile_study_source_to_wasm(
    source: &str,
) -> Result<CompiledStudyArtifact, Vec<StudyDiagnostic>> {
    let static_diags = validate_study_source_static(source);
    if !static_diags.is_empty() {
        return Err(static_diags);
    }

    let toolchain_identity = match probe_study_dev_pack() {
        StudyDevPackStatus::Ready { rustc_version, .. } => rustc_version,
        StudyDevPackStatus::MissingSandboxTarget {
            required_target, ..
        } => {
            return Err(vec![StudyDiagnostic {
                severity: StudyDiagnosticSeverity::Error,
                line: 1,
                column: 1,
                code: Some("AERIS_DEV_PACK_TARGET".to_string()),
                message: format!(
                    "Missing WebAssembly target `{required_target}`. Install with: rustup target add {required_target}"
                ),
            }]);
        }
        StudyDevPackStatus::Unavailable => {
            return Err(vec![StudyDiagnostic {
                severity: StudyDiagnosticSeverity::Error,
                line: 1,
                column: 1,
                code: Some("AERIS_DEV_PACK_MISSING".to_string()),
                message: format!(
                    "Rust compiler (`rustc`) not found. Install Rust and `{STUDY_SANDBOX_TARGET_TRIPLE}` to build user studies."
                ),
            }]);
        }
    };

    let nonce = BUILD_DIR_NONCE.fetch_add(1, Ordering::Relaxed);
    let build_dir =
        std::env::temp_dir().join(format!("aeris_study_wasm_{}_{nonce}", std::process::id()));
    let cleanup = |dir: &Path| {
        let _ = fs::remove_dir_all(dir);
    };
    if let Err(err) = fs::create_dir_all(&build_dir) {
        return Err(vec![StudyDiagnostic {
            severity: StudyDiagnosticSeverity::Error,
            line: 1,
            column: 1,
            code: Some("AERIS_BUILD_IO".to_string()),
            message: format!("failed to create temporary build directory: {err}"),
        }]);
    }

    let harness_path = build_dir.join("harness.rs");
    let user_path = build_dir.join("user_study.rs");
    let out_wasm_path = build_dir.join("study.wasm");
    let harness_src = if source_uses_incremental_entrypoint(source) {
        format!("{WASM_HOST_API_PRELUDE}{WASM_INCREMENTAL_ENTRYPOINT}")
    } else if source_uses_multi_output_entrypoint(source) {
        format!("{WASM_HOST_API_PRELUDE}{WASM_MULTI_ENTRYPOINT}")
    } else {
        format!("{WASM_HOST_API_PRELUDE}{WASM_SINGLE_ENTRYPOINT}")
    };
    if let Err(err) =
        fs::write(&harness_path, harness_src).and_then(|()| fs::write(&user_path, source))
    {
        cleanup(&build_dir);
        return Err(vec![StudyDiagnostic {
            severity: StudyDiagnosticSeverity::Error,
            line: 1,
            column: 1,
            code: Some("AERIS_BUILD_IO".to_string()),
            message: format!("failed to stage study source for compilation: {err}"),
        }]);
    }

    let rustc = resolve_toolchain_binary("rustc");
    let output = hidden_command(&rustc)
        .args([
            "--edition",
            "2024",
            "--crate-type",
            "cdylib",
            "--target",
            STUDY_SANDBOX_TARGET_TRIPLE,
            "-O",
            "-C",
            "panic=abort",
            "-C",
            "strip=symbols",
            "--error-format=json",
        ])
        .arg("-o")
        .arg(&out_wasm_path)
        .arg(&harness_path)
        .output();

    let output = match output {
        Ok(out) => out,
        Err(err) => {
            cleanup(&build_dir);
            return Err(vec![StudyDiagnostic {
                severity: StudyDiagnosticSeverity::Error,
                line: 1,
                column: 1,
                code: Some("AERIS_RUSTC_SPAWN".to_string()),
                message: format!("failed to invoke rustc: {err}"),
            }]);
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let diags = parse_rustc_json_diagnostics(&stderr);
        cleanup(&build_dir);
        return Err(diags);
    }

    let wasm_bytes = match fs::read(&out_wasm_path) {
        Ok(bytes) => bytes,
        Err(err) => {
            cleanup(&build_dir);
            return Err(vec![StudyDiagnostic {
                severity: StudyDiagnosticSeverity::Error,
                line: 1,
                column: 1,
                code: Some("AERIS_BUILD_IO".to_string()),
                message: format!("failed to read compiled wasm artifact: {err}"),
            }]);
        }
    };
    cleanup(&build_dir);

    let compiled = CompiledWasmStudy::from_bytes(&wasm_bytes, StudySandboxLimits::default())
        .map_err(|err| {
            vec![StudyDiagnostic {
                severity: StudyDiagnosticSeverity::Error,
                line: 1,
                column: 1,
                code: Some("AERIS_SANDBOX_REJECTED".to_string()),
                message: err.to_string(),
            }]
        })?;

    Ok(CompiledStudyArtifact {
        artifact_sha256: bytes_sha256_hex(&wasm_bytes),
        source_sha256: source_sha256_hex(source),
        toolchain_identity,
        wasm_bytes,
        compiled,
    })
}

/// Runs static sandbox validation, compiles to WASM when the dev pack is ready, and executes
/// the compiled study inside `wasmi` against a deterministic offline market preview fixture
/// without opening any provider session.
#[must_use]
pub(crate) fn run_offline_preview_check(
    source: &str,
    preset: PreviewFixturePreset,
) -> (Vec<StudyDiagnostic>, String, Option<WasmStudyTelemetry>) {
    let fixture = StudyPreviewFixture::generate(preset, 120, 60);
    let spec = parse_study_source_spec(source, "Preview");
    let limits = StudySandboxLimits::default();
    match compile_study_source_to_wasm(source) {
        Ok(artifact) => {
            match artifact
                .compiled
                .execute_columnar_outputs(&fixture.bars, spec.outputs.len(), limits)
            {
                Ok(result) => {
                    let primary = result.outputs.first().map_or(&[][..], Vec::as_slice);
                    let finite_count = primary.iter().filter(|v| v.is_some()).count();
                    let last_label = primary
                        .iter()
                        .rev()
                        .find_map(|v| *v)
                        .map_or_else(|| "NaN".to_string(), |v| format!("{v:.2}"));
                    let elapsed_ms = (result.elapsed_micros as f64) / 1000.0;
                    let summary = format!(
                        "WASM Preview [{preset:?}]: {} series, {finite_count}/{} pts, last={last_label}, {elapsed_ms:.2}ms, fuel={}",
                        result.outputs.len(),
                        fixture.bars.len(),
                        result.fuel_consumed
                    );
                    let telemetry = WasmStudyTelemetry {
                        fuel_consumed: result.fuel_consumed,
                        fuel_limit: limits.max_instruction_fuel,
                        elapsed_micros: result.elapsed_micros,
                        bar_count: fixture.bars.len(),
                        scene_object_count: result.scene_object_count,
                        alert_count: result.alerts.len(),
                        logs: result.logs,
                        strategy_report: result.strategy_report,
                    };
                    (Vec::new(), summary, Some(telemetry))
                }
                Err(err) => (
                    vec![StudyDiagnostic {
                        severity: StudyDiagnosticSeverity::Error,
                        line: 1,
                        column: 1,
                        code: Some("AERIS_WASM_TRAP".to_string()),
                        message: err.to_string(),
                    }],
                    format!("WASM Preview [{preset:?}] trapped: {err}"),
                    None,
                ),
            }
        }
        Err(diagnostics) => {
            let summary = format!(
                "Preview [{preset:?}] blocked: {} diagnostic(s) must be fixed",
                diagnostics.len()
            );
            (diagnostics, summary, None)
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct StudyProjectBundle {
    bundle_version: u32,
    manifest: StoredManifest,
    source: String,
    source_sha256: String,
}

/// Serializes one user-authored study project into a portable, integrity-checked JSON bundle.
pub(crate) fn export_project_bundle_json(project: &StudyProject) -> Result<String, String> {
    project
        .manifest
        .validate()
        .map_err(|error| format!("invalid manifest: {error:?}"))?;
    validate_study_source(&project.source).map_err(|error| format!("invalid source: {error:?}"))?;
    let bundle = StudyProjectBundle {
        bundle_version: STUDY_PROJECT_BUNDLE_VERSION,
        manifest: StoredManifest::from(&project.manifest),
        source_sha256: source_sha256_hex(&project.source),
        source: project.source.clone(),
    };
    serde_json::to_string_pretty(&bundle)
        .map_err(|error| format!("serialize study bundle failed: {error}"))
}

/// Validates and parses a portable JSON study bundle, rejecting tampered SHA-256 digests,
/// unsupported bundle versions, invalid manifests, or oversized sources.
pub(crate) fn import_project_bundle_json(
    raw: &str,
) -> Result<(StudyProjectManifest, String), String> {
    if raw.len() > MAXIMUM_STUDY_SOURCE_BYTES.saturating_add(16 * 1024) {
        return Err("study bundle exceeds maximum allowed size".to_string());
    }
    let bundle: StudyProjectBundle =
        serde_json::from_str(raw).map_err(|error| format!("invalid study bundle JSON: {error}"))?;
    if bundle.bundle_version != STUDY_PROJECT_BUNDLE_VERSION {
        return Err(format!(
            "unsupported study bundle version {}",
            bundle.bundle_version
        ));
    }
    let manifest = StudyProjectManifest::try_from(bundle.manifest)?;
    validate_study_source(&bundle.source)
        .map_err(|error| format!("invalid study source in bundle: {error:?}"))?;
    let actual_sha = source_sha256_hex(&bundle.source);
    if !bundle.source_sha256.eq_ignore_ascii_case(&actual_sha) {
        return Err("study bundle SHA-256 integrity check failed".to_string());
    }
    Ok((manifest, bundle.source))
}

/// Result of one background study validation/build job.
#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StudyBuildOutcome {
    pub(crate) job_id: u64,
    pub(crate) identifier: String,
    pub(crate) source_sha256: String,
    pub(crate) diagnostics: Vec<StudyDiagnostic>,
    pub(crate) cancelled: bool,
}

/// Bounded, cancellable background validation/build coordinator for Phase L user studies.
///
/// Runs off the GPUI thread with at most one active job at a time; submitting a newer job
/// cooperatively cancels any superseded in-flight job without spawning unbounded threads.
#[cfg(test)]
pub(crate) struct StudyBuildCoordinator {
    next_job_id: u64,
    cancel_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    latest_outcome: std::sync::Arc<std::sync::Mutex<Option<StudyBuildOutcome>>>,
}

#[cfg(test)]
impl StudyBuildCoordinator {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            next_job_id: 1,
            cancel_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            latest_outcome: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Cancels any active build job and starts one bounded background validation job.
    pub(crate) fn submit(&mut self, identifier: String, source: String) -> u64 {
        self.cancel_flag
            .store(true, std::sync::atomic::Ordering::Release);
        let job_id = self.next_job_id;
        self.next_job_id = self.next_job_id.saturating_add(1);
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.cancel_flag = std::sync::Arc::clone(&cancel);
        let slot = std::sync::Arc::clone(&self.latest_outcome);
        let _ = std::thread::Builder::new()
            .name(format!("aeris-study-build-{job_id}"))
            .spawn(move || {
                if cancel.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                let diagnostics = validate_study_source_static(&source);
                let source_sha256 = source_sha256_hex(&source);
                let cancelled = cancel.load(std::sync::atomic::Ordering::Acquire);
                let outcome = StudyBuildOutcome {
                    job_id,
                    identifier,
                    source_sha256,
                    diagnostics,
                    cancelled,
                };
                if let Ok(mut guard) = slot.lock() {
                    let should_replace = guard
                        .as_ref()
                        .is_none_or(|existing| outcome.job_id >= existing.job_id);
                    if should_replace {
                        *guard = Some(outcome);
                    }
                }
            });
        job_id
    }

    /// Cooperatively cancels the currently active job.
    pub(crate) fn cancel_active(&self) {
        self.cancel_flag
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Takes the newest completed build outcome, if any.
    pub(crate) fn take_outcome(&self) -> Option<StudyBuildOutcome> {
        self.latest_outcome
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn study_project_store_round_trips_projects_and_isolates_corrupt_directories() {
        let root = std::env::temp_dir().join(format!(
            "aeris_study_store_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = fs::remove_dir_all(&root);
        let store = StudyProjectStore::new(&root);

        let mut library = StudyProjectLibrary::new();
        library
            .create_from_template(
                "user.ksr6_custom",
                "KSR6 Custom",
                StudyProjectTemplate::StatelessOverlay,
            )
            .expect("creates project");
        let good_src = library.get("user.ksr6_custom").unwrap().source.clone();
        let digest = source_sha256_hex(&good_src);
        library
            .record_build_success("user.ksr6_custom", digest.clone(), digest)
            .expect("records good build");
        library
            .update_source(
                "user.ksr6_custom",
                "// edited draft\npub fn x() {}".to_string(),
            )
            .expect("updates draft");
        store
            .save_project(library.get("user.ksr6_custom").unwrap())
            .expect("saves project");

        // Inject one corrupt sibling directory to prove isolation.
        let corrupt_dir = root.join(PROJECTS_DIR_NAME).join("user.corrupt");
        fs::create_dir_all(&corrupt_dir).expect("corrupt dir");
        fs::write(corrupt_dir.join(MANIFEST_FILE_NAME), "{not json").expect("corrupt manifest");

        let (mut reloaded, warnings) = store.load_library();
        assert_eq!(warnings.len(), 1);
        assert_eq!(reloaded.len(), 1);
        let project = reloaded
            .get("user.ksr6_custom")
            .expect("valid project loads");
        assert_eq!(project.source, "// edited draft\npub fn x() {}");
        assert_eq!(project.last_good_source.as_deref(), Some(good_src.as_str()));

        reloaded
            .revert_to_last_good("user.ksr6_custom")
            .expect("reverts to persisted last-good source");
        assert_eq!(reloaded.get("user.ksr6_custom").unwrap().source, good_src);

        store
            .delete_project("user.ksr6_custom")
            .expect("deletes cleanly");
        let (after_delete, _) = store.load_library();
        assert!(after_delete.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn static_validator_flags_forbidden_ambient_os_apis_with_line_and_column() {
        let bad = "use std::fs;\nfn calc() {\n    let _ = unsafe { 1 };\n}\n";
        let diagnostics = validate_study_source_static(bad);
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].line, 1);
        assert_eq!(diagnostics[0].code.as_deref(), Some("AERIS_NO_FS"));
        assert_eq!(diagnostics[1].line, 3);
        assert_eq!(diagnostics[1].code.as_deref(), Some("AERIS_NO_UNSAFE"));

        let (_, clean_src) = StudyProjectTemplate::StatelessOverlay.scaffold("user.clean", "Clean");
        assert!(validate_study_source_static(&clean_src).is_empty());
    }

    #[test]
    fn study_project_bundle_round_trips_and_rejects_tampered_digest() {
        let mut library = StudyProjectLibrary::new();
        library
            .create_from_template(
                "user.portable",
                "Portable",
                StudyProjectTemplate::AlertStudy,
            )
            .expect("creates project");
        let project = library.get("user.portable").unwrap();
        let exported = export_project_bundle_json(project).expect("exports bundle");
        let (manifest, source) = import_project_bundle_json(&exported).expect("imports bundle");
        assert_eq!(manifest.identifier, "user.portable");
        assert_eq!(source, project.source);

        let tampered = exported
            .replace("Portable", "Tampered")
            .replace(&source_sha256_hex(&project.source), &"0".repeat(64));
        assert!(import_project_bundle_json(&tampered).is_err());
    }

    #[test]
    fn study_build_coordinator_runs_off_thread_and_produces_latest_outcome() {
        let mut coordinator = StudyBuildCoordinator::new();
        let (_, clean_src) = StudyProjectTemplate::StatelessOverlay.scaffold("user.bg", "BG");
        let job_id = coordinator.submit("user.bg".to_string(), clean_src);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let outcome = loop {
            if let Some(outcome) = coordinator.take_outcome() {
                break outcome;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "background build job timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(outcome.job_id, job_id);
        assert!(outcome.diagnostics.is_empty());
        assert!(!outcome.cancelled);
        coordinator.cancel_active();
    }

    #[test]
    fn artifact_cache_pruning_removes_stale_entries_and_preview_check_validates_source() {
        let root = std::env::temp_dir().join(format!(
            "aeris_study_cache_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = fs::remove_dir_all(&root);
        let store = StudyProjectStore::new(&root);
        let cache_dir = root.join(PROJECTS_DIR_NAME).join(ARTIFACT_CACHE_DIR_NAME);
        fs::create_dir_all(&cache_dir).expect("cache dir");

        let fresh = StoredCacheKey {
            policy_version: aeris_study_sdk::toolchain::STUDY_TOOLCHAIN_POLICY_VERSION,
            sdk_compatibility_epoch: aeris_study_sdk::STUDY_SDK_COMPATIBILITY_EPOCH,
            sandbox_abi_version: aeris_study_sdk::sandbox::STUDY_SANDBOX_ABI_VERSION,
            toolchain_identity: "rustc 1.99.0".to_string(),
            target_triple: STUDY_SANDBOX_TARGET_TRIPLE.to_string(),
            source_sha256: "a".repeat(64),
            lockfile_sha256: "b".repeat(64),
        };
        let stale = StoredCacheKey {
            sdk_compatibility_epoch: aeris_study_sdk::STUDY_SDK_COMPATIBILITY_EPOCH + 1,
            ..fresh.clone()
        };
        fs::write(
            cache_dir.join("fresh.json"),
            serde_json::to_string(&fresh).unwrap(),
        )
        .unwrap();
        fs::write(
            cache_dir.join("stale.json"),
            serde_json::to_string(&stale).unwrap(),
        )
        .unwrap();

        assert_eq!(store.prune_stale_artifact_cache("rustc 1.99.0"), 1);
        assert!(cache_dir.join("fresh.json").exists());
        assert!(!cache_dir.join("stale.json").exists());

        let (_, clean_src) = StudyProjectTemplate::StatelessOverlay.scaffold("user.prev", "Prev");
        let (diags, summary, telemetry) =
            run_offline_preview_check(&clean_src, PreviewFixturePreset::Uptrend);
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        assert!(
            summary.contains("120/120 pts"),
            "unexpected summary: {summary}"
        );
        assert!(telemetry.is_some());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn controlled_rustc_compiles_all_templates_to_zero_authority_wasm_and_traps_infinite_loops() {
        let fixture = StudyPreviewFixture::generate(PreviewFixturePreset::Uptrend, 40, 60);
        for template in [
            StudyProjectTemplate::StatelessOverlay,
            StudyProjectTemplate::StatefulRecursive,
            StudyProjectTemplate::MultiTimeframe,
            StudyProjectTemplate::DrawingTableDashboard,
            StudyProjectTemplate::AlertStudy,
        ] {
            let (_, src) = template.scaffold("user.tpl", "Template");
            let artifact = compile_study_source_to_wasm(&src).unwrap_or_else(|diags| {
                panic!("template {template:?} failed to compile: {diags:?}")
            });
            assert!(!artifact.wasm_bytes.is_empty());
            assert_eq!(artifact.artifact_sha256.len(), 64);
            let result = artifact
                .compiled
                .execute_columnar(&fixture.bars, StudySandboxLimits::default())
                .unwrap_or_else(|err| panic!("template {template:?} failed to execute: {err}"));
            assert_eq!(result.values.len(), 40);
            assert!(result.fuel_consumed > 0);
        }

        // Syntax/type error produces structured rustc JSON diagnostic with exact line number.
        let broken = "pub fn calculate(_o: &[f64], _h: &[f64], _l: &[f64], _c: &[f64], _v: &[f64], out: &mut [f64]) {\n    out[0] = \"not_a_float\";\n}\n";
        let diags = compile_study_source_to_wasm(broken).expect_err("type error must fail");
        assert!(!diags.is_empty());
        assert_eq!(diags[0].line, 2);
        assert_eq!(diags[0].code.as_deref(), Some("E0308"));

        // Infinite loop compiles, then deterministically traps on fuel exhaustion during execution.
        let loopy = "pub fn calculate(_o: &[f64], _h: &[f64], _l: &[f64], _c: &[f64], _v: &[f64], _out: &mut [f64]) { loop {} }";
        let (loop_diags, loop_summary, loop_telemetry) =
            run_offline_preview_check(loopy, PreviewFixturePreset::Uptrend);
        assert_eq!(loop_diags.len(), 1);
        assert_eq!(loop_diags[0].code.as_deref(), Some("AERIS_WASM_TRAP"));
        assert!(loop_summary.contains("fuel"), "summary: {loop_summary}");
        assert!(loop_telemetry.is_none());
    }

    #[test]
    fn phase_1_wasm_prelude_ta_time_syminfo_alerts_and_logs_execute_end_to_end() {
        let source = r#"
// @pane: price
// @output: ema | EMA 10 | line | #38bdf8
// @output: vwap | Session VWAP | line | #f59e0b
// @output: rsi | RSI 14 | line | #22c55e
pub fn calculate_multi(
    _open: &[f64],
    high: &[f64],
    low: &[f64],
    close: &[f64],
    volume: &[f64],
    outs: &mut [&mut [f64]],
) {
    let ema10 = super::ta::ema(close, 10);
    let svwap = super::ta::session_vwap(high, low, close, volume);
    let rsi14 = super::ta::rsi(close, 14);
    outs[0].copy_from_slice(&ema10);
    outs[1].copy_from_slice(&svwap);
    outs[2].copy_from_slice(&rsi14);

    let last = close.len().saturating_sub(1);
    let ts = super::time(last);
    let (h, m, _s) = super::bar_time_hms(last);
    let tf = super::syminfo::timeframe_seconds();
    super::log_info(&format!("ts={ts} hms={h:02}:{m:02} tf={tf}s"));
    super::log_warn("sample warning");
    super::emit_alert(
        last,
        "phase1_signal",
        "EMA + VWAP ready",
        super::ALERT_ONCE_PER_BAR_CLOSE,
    );
}
"#;
        let (diags, summary, telemetry) =
            run_offline_preview_check(source, PreviewFixturePreset::Uptrend);
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        assert!(summary.contains("3 series"), "summary: {summary}");
        let telemetry = telemetry.expect("telemetry must be populated");
        assert!(telemetry.fuel_consumed > 0);
        assert_eq!(telemetry.bar_count, 120);
        assert_eq!(telemetry.alert_count, 1);
        assert_eq!(telemetry.logs.len(), 2);
        assert!(telemetry.logs[0].message.contains("tf=60s"));
        assert!(!telemetry.logs[0].message.contains("ts=0 "));
        assert_eq!(telemetry.logs[1].message, "sample warning");
    }

    #[test]
    fn phase_2_typed_inputs_per_point_colors_box_text_line_fill_and_plot_shape_work() {
        let source = r#"
// @pane: price
// @input: len | int | 14 | Length | min=2 | max=200 | step=1 | group=Core | tooltip=MA period
// @input: src | source | hlc3 | Price Source | group=Core
// @input: mode | choice | EMA | Mode | options=SMA,EMA,RMA | group=Core
// @input: bull_col | color | #22c55eff | Bull Color | group=Style
// @input: tf | timeframe | 15m | Higher TF | group=MTF
// @input: sess | session | 09:30-16:00 | Session | group=MTF
// @output: ma | Dynamic MA | area | #22c55e
pub fn calculate(
    open: &[f64],
    high: &[f64],
    low: &[f64],
    close: &[f64],
    volume: &[f64],
    out: &mut [f64],
) {
    let len = super::param_int(0, 14).max(2) as usize;
    let src = super::param_source(1, open, high, low, close, volume);
    let mode = super::param_str(2, "EMA");
    let bull = super::param_color(3, super::rgba(0x22c55e, 255));
    let series = match mode.as_str() {
        "SMA" => super::ta::sma(&src, len),
        "RMA" => super::ta::rma(&src, len),
        _ => super::ta::ema(&src, len),
    };
    out.copy_from_slice(&series);
    for i in 1..out.len() {
        let c = if out[i] >= out[i - 1] { bull } else { super::rgba(0xef4444, 255) };
        super::set_plot_color(0, i, c);
    }
    let last = close.len().saturating_sub(1);
    super::draw_box_text(
        (last.saturating_sub(10)) as f64,
        high[last],
        last as f64,
        low[last],
        bull,
        super::rgba(0x22c55e, 48),
        1,
        super::LINE_DASHED,
        super::rgba(0xffffff, 255),
        "ZONE",
    );
    let l1 = super::draw_line(0.0, high[last], last as f64, high[last], bull, 1, super::LINE_SOLID, super::EXTEND_NONE);
    let l2 = super::draw_line(0.0, low[last], last as f64, low[last], bull, 1, super::LINE_SOLID, super::EXTEND_NONE);
    super::draw_line_fill(l1, l2, super::rgba(0x22c55e, 32));
    super::plot_shape(last, super::LOC_BELOW_BAR, 0.0, super::LABEL_TRIANGLE_UP, bull, super::rgba(0xffffff, 255), "BUY");
}
"#;
        let spec = parse_study_source_spec(source, "Phase2");
        assert_eq!(spec.inputs.len(), 6);
        assert!(spec.uses_per_point_colors);
        assert_eq!(spec.outputs[0].plot, StudyPlotKind::Area);
        assert_eq!(spec.inputs[0].presentation.group.as_deref(), Some("Core"));
        assert_eq!(
            spec.inputs[0].presentation.description.as_deref(),
            Some("MA period")
        );

        let (diags, summary, telemetry) =
            run_offline_preview_check(source, PreviewFixturePreset::Uptrend);
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        assert!(summary.contains("1 series"), "summary: {summary}");
        let telemetry = telemetry.expect("telemetry present");
        assert_eq!(telemetry.scene_object_count, 4); // box + 2 lines + shape
    }

    #[test]
    fn phase_3_mtf_multi_symbol_and_order_flow_compile_and_execute_in_wasm() {
        let source = r#"
// @pane: separate
// @request_tf: htf | 15m | confirmed | carry
// @request_symbol: bench | SPY | 1D
// @require: order_flow
// @output: rs | Relative Strength | line | #38bdf8
// @output: cvd | CVD | line | #22c55e
pub fn calculate_multi(
    _open: &[f64],
    _high: &[f64],
    _low: &[f64],
    close: &[f64],
    _volume: &[f64],
    outs: &mut [&mut [f64]],
) {
    let n = close.len();
    for i in 0..n {
        let htf_c = super::sec_close(0, i);
        outs[0][i] = if htf_c.is_finite() && htf_c > 0.0 {
            close[i] / htf_c
        } else {
            f64::NAN
        };
    }
    let cvd = super::of::cvd(n);
    outs[1].copy_from_slice(&cvd);
}
"#;
        let spec = parse_study_source_spec(source, "Phase3");
        assert_eq!(spec.secondary_requests.len(), 2);
        assert_eq!(spec.secondary_requests[0].alias, "htf");
        assert_eq!(spec.secondary_requests[1].symbol.as_deref(), Some("SPY"));
        assert!(spec.requires_order_flow);

        let artifact = compile_study_source_to_wasm(source).expect("Phase 3 study compiles");
        let fixture = StudyPreviewFixture::generate(PreviewFixturePreset::Uptrend, 20, 60);
        let n = fixture.bars.len();
        let secondary = aeris_study_sdk::wasm_runtime::WasmAlignedSecondary {
            open: vec![100.0; n],
            high: vec![105.0; n],
            low: vec![95.0; n],
            close: vec![100.0; n],
            volume: vec![1000.0; n],
        };
        let order_flow = aeris_study_sdk::wasm_runtime::WasmOrderFlowColumns {
            buy_vol: vec![30.0; n],
            sell_vol: vec![10.0; n],
            delta: vec![20.0; n],
            trade_count: vec![5.0; n],
            poc: vec![101.0; n],
        };
        let result = artifact
            .compiled
            .execute_columnar_extended(
                &fixture.bars,
                2,
                &[],
                &[],
                &[secondary],
                Some(&order_flow),
                None,
                &aeris_study_sdk::wasm_runtime::WasmStudyMeta::default(),
                StudySandboxLimits::default(),
            )
            .expect("Phase 3 extended execution succeeds");
        assert_eq!(result.outputs.len(), 2);
        assert!(result.outputs[0][0].is_some());
        // CVD of constant delta=20.0 over 20 bars -> bar 19 = 400.0
        let last_cvd = result.outputs[1][n - 1].expect("finite CVD");
        assert!((last_cvd - 400.0).abs() < 1e-6, "got {last_cvd}");
    }

    #[test]
    fn phase_4_incremental_on_bar_beats_full_recompute_by_10x_and_survives_5000_ticks() {
        let source = r#"
// @pane: price
// @output: ema | Fast Incremental EMA | line | #38bdf8
use std::sync::atomic::{AtomicU64, Ordering};

static EMA_STATE: AtomicU64 = AtomicU64::new(f64::NAN.to_bits());

pub fn on_bar(
    bar_idx: usize,
    _is_new_bar: bool,
    _is_confirmed: bool,
    _open: f64,
    _high: f64,
    _low: f64,
    close: f64,
    _volume: f64,
    out: &mut [f64],
) {
    let alpha = 2.0 / 21.0;
    let prev = if bar_idx == 0 {
        f64::NAN
    } else {
        f64::from_bits(EMA_STATE.load(Ordering::Relaxed))
    };
    let next = if prev.is_finite() {
        alpha * close + (1.0 - alpha) * prev
    } else {
        close
    };
    EMA_STATE.store(next.to_bits(), Ordering::Relaxed);
    out[0] = next;
}
"#;
        let artifact = compile_study_source_to_wasm(source).expect("incremental study compiles");
        assert!(artifact.compiled.supports_incremental());

        let fixture = StudyPreviewFixture::generate(PreviewFixturePreset::Uptrend, 1_000, 60);
        let full = artifact
            .compiled
            .execute_columnar_outputs(&fixture.bars, 1, StudySandboxLimits::default())
            .expect("full 1000-bar recompute succeeds");

        let mut inst = artifact
            .compiled
            .instantiate_incremental(
                &[],
                &[],
                &aeris_study_sdk::wasm_runtime::WasmStudyMeta::default(),
                StudySandboxLimits::default(),
            )
            .expect("incremental instance initializes");

        let (vals, tick_fuel) = inst
            .step_bar(1_000, true, true, 150.0, 152.0, 149.0, 151.0, 100.0, 999_000_000_000, 1)
            .expect("single incremental tick succeeds");
        assert!(vals[0].is_some());
        assert!(
            full.fuel_consumed > tick_fuel * 10,
            "expected >10x fuel savings: full={} vs single_tick={}",
            full.fuel_consumed,
            tick_fuel
        );

        // 5,000-tick soak test inside the same persistent store (bounded memory & fuel reset)
        for tick in 0..5_000usize {
            let price = 150.0 + (tick as f64) * 0.01;
            let (out, _) = inst
                .step_bar(1_000 + tick, true, true, price, price + 1.0, price - 1.0, price, 50.0, 1_000_000_000_000 + (tick as i64) * 60_000_000_000, 1)
                .expect("soak tick must succeed without memory growth or fuel leak");
            assert!(out[0].is_some());
        }
    }

    #[test]
    fn phase_5_wasm_strategy_backtest_computes_report_and_emits_chart_markers() {
        let source = r#"
// @pane: price
// @output: fast | Fast EMA | line | #38bdf8
// @output: slow | Slow EMA | line | #f59e0b
pub fn calculate_multi(
    _open: &[f64],
    _high: &[f64],
    _low: &[f64],
    close: &[f64],
    _volume: &[f64],
    outs: &mut [&mut [f64]],
) {
    let fast = super::ta::ema(close, 5);
    let slow = super::ta::ema(close, 15);
    outs[0].copy_from_slice(&fast);
    outs[1].copy_from_slice(&slow);
    for i in 15..close.len() {
        if i == 20 {
            super::strategy::entry(i, "Long1", true, 10.0);
        } else if i == 60 {
            super::strategy::close(i, "Long1");
        } else if i == 70 {
            super::strategy::entry(i, "Long2", true, 10.0);
        } else if i == 110 {
            super::strategy::close_all(i);
        }
    }
}
"#;
        let (diags, _summary, telemetry) =
            run_offline_preview_check(source, PreviewFixturePreset::Uptrend);
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        let telemetry = telemetry.expect("telemetry must exist");
        let report = telemetry
            .strategy_report
            .expect("strategy_report must be populated");
        assert_eq!(report.total_trades, 2);
        assert!(report.net_profit > 0.0, "uptrend longs should profit: {report:?}");
        assert!(telemetry.scene_object_count >= 4, "expected entry/exit chart markers");
    }

    #[test]
    fn wasm_study_survives_restart_restore_and_executes_1000_bars_within_budget() {
        let root = std::env::temp_dir().join(format!(
            "aeris_study_restart_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = fs::remove_dir_all(&root);
        let store = StudyProjectStore::new(&root);

        let mut library = StudyProjectLibrary::new();
        library
            .create_from_template(
                "user.restart_ema",
                "Restart EMA",
                StudyProjectTemplate::StatefulRecursive,
            )
            .expect("creates project");
        let src = library.get("user.restart_ema").unwrap().source.clone();

        let build_start = std::time::Instant::now();
        let artifact = compile_study_source_to_wasm(&src).expect("compiles to wasm");
        let build_elapsed = build_start.elapsed();
        assert!(
            build_elapsed < std::time::Duration::from_secs(15),
            "compile latency too high: {build_elapsed:?}"
        );

        store
            .save_last_good_wasm(
                "user.restart_ema",
                &artifact.source_sha256,
                &artifact.toolchain_identity,
                &artifact.wasm_bytes,
            )
            .expect("saves last_good.wasm");
        library
            .record_build_success(
                "user.restart_ema",
                artifact.source_sha256,
                artifact.artifact_sha256,
            )
            .expect("records build");
        store
            .save_project(library.get("user.restart_ema").unwrap())
            .expect("saves project");

        // Simulate app restart with a fresh StudyProjectStore and restore registration.
        let restarted_store = StudyProjectStore::new(&root);
        let persisted = WorkspaceChartStudyState {
            local_id: 1,
            identifier: "user.restart_ema".to_string(),
            implementation_revision: 1,
            output_identifiers: vec!["value".to_string()],
            visible: true,
            ..WorkspaceChartStudyState::default()
        };
        let series = BarSeriesKey {
            provider_id: "test".to_string(),
            instrument_id: "RESTART".to_string(),
            entitlement_id: "default".to_string(),
            period: aeris_study_sdk::BarPeriod::Time { seconds: 60 },
            definition_version: 1,
        };
        let restored = restarted_store
            .restore_wasm_study_registration(&persisted, &series)
            .expect("restores wasm study registration after restart");
        assert_eq!(restored.definition.identifier, "user.restart_ema");
        assert!(restored.program.dynamic_calculate.is_some());

        // Measure 1,000-bar wasmi execution latency on the restored artifact.
        let fixture = StudyPreviewFixture::generate(PreviewFixturePreset::Uptrend, 1_000, 60);
        let exec_start = std::time::Instant::now();
        let result = artifact
            .compiled
            .execute_columnar(&fixture.bars, StudySandboxLimits::default())
            .expect("executes 1,000 bars");
        let exec_elapsed = exec_start.elapsed();
        assert_eq!(result.values.len(), 1_000);
        assert!(result.values.last().copied().flatten().is_some());
        assert!(
            exec_elapsed < std::time::Duration::from_secs(2),
            "1,000-bar wasm execution too slow: {exec_elapsed:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn separate_pane_and_plot_directives_parse_and_compile_advanced_oscillator() {
        let src = r#"
// @pane: separate
// @plot: histogram
pub fn calculate(
    open: &[f64],
    high: &[f64],
    low: &[f64],
    close: &[f64],
    volume: &[f64],
    out: &mut [f64],
) {
    let len = 20usize;
    let n = out.len();
    let mut delta = vec![0.0; n];
    for i in 0..n {
        if i + 1 < len {
            out[i] = f64::NAN;
            continue;
        }
        let start = i + 1 - len;
        let mut hh = f64::NEG_INFINITY;
        let mut ll = f64::INFINITY;
        let mut sum_c = 0.0;
        let mut sum_v = 0.0;
        for j in start..=i {
            if high[j] > hh { hh = high[j]; }
            if low[j] < ll { ll = low[j]; }
            sum_c += close[j];
            sum_v += volume[j];
        }
        let mid = 0.5 * (0.5 * (hh + ll) + sum_c / (len as f64));
        let avg_v = (sum_v / (len as f64)).max(1e-9);
        let rel_vol = (volume[i] / avg_v).clamp(0.5, 2.5);
        let body_bias = if close[i] >= open[i] { 1.0 } else { -1.0 };
        delta[i] = (close[i] - mid) * rel_vol + 0.0 * body_bias;

        let mut sx = 0.0;
        let mut sy = 0.0;
        let mut sxx = 0.0;
        let mut sxy = 0.0;
        for k in 0..len {
            let x = k as f64;
            let y = delta[start + k];
            sx += x;
            sy += y;
            sxx += x * x;
            sxy += x * y;
        }
        let lf = len as f64;
        let denom = lf * sxx - sx * sx;
        let slope = if denom.abs() > 1e-12 { (lf * sxy - sx * sy) / denom } else { 0.0 };
        let intercept = (sy - slope * sx) / lf;
        out[i] = intercept + slope * (lf - 1.0);
    }
}
"#;
        let spec = parse_study_source_spec(src, "Squeeze");
        assert_eq!(spec.pane, StudyPaneTarget::Dedicated { group: 0 });
        assert_eq!(spec.outputs.len(), 1);
        assert_eq!(spec.outputs[0].plot, StudyPlotKind::Histogram);

        let artifact = compile_study_source_to_wasm(src).expect("oscillator compiles to wasm");
        let fixture = StudyPreviewFixture::generate(PreviewFixturePreset::RangeBound, 1_500, 60);
        let res = artifact
            .compiled
            .execute_columnar(&fixture.bars, StudySandboxLimits::default())
            .expect("oscillator executes cleanly over 1,500 bars in wasmi");
        assert_eq!(res.values.len(), 1_500);
        assert!(res.values[0].is_none());
        assert!(res.values[1_499].is_some());
    }

    #[test]
    fn multi_output_median_atr_bands_and_fills_compile_and_execute() {
        let hash = "#";
        let src = format!(
            r#"
// @pane: price
// @width: 2
// @input: length | int | 3 | Median Length
// @input: atr_length | int | 14 | ATR Length
// @input: atr_mult | float | 2.0 | ATR Multiplier
// @output: median | Median | line | {hash}ef4444
// @output: upper | Upper Band | line | {hash}22c55e
// @output: lower | Lower Band | line | {hash}d946ef
// @output: ema | Median EMA | line | {hash}3b82f6
// @output: bull_fill | Bull Cloud | line | {hash}00000000
// @output: bear_fill | Bear Cloud | line | {hash}00000000
// @fill: 4, 3 | {hash}22c55e33
// @fill: 5, 3 | {hash}d946ef33
// @level: 100.0 | Pivot | dashed | {hash}94a3b880
pub fn calculate_multi(
    _open: &[f64],
    high: &[f64],
    low: &[f64],
    close: &[f64],
    _volume: &[f64],
    outs: &mut [&mut [f64]],
) {{
    let length = (super::param(0, 3.0).round() as usize).clamp(1, 200);
    let atr_len = (super::param(1, 14.0).round() as usize).clamp(1, 200);
    let atr_mult = super::param(2, 2.0).max(0.0);
    let n = close.len();
    if n == 0 || outs.len() < 6 {{
        return;
    }}
    let mut hl2 = vec![0.0_f64; n];
    let mut tr = vec![0.0_f64; n];
    for i in 0..n {{
        hl2[i] = 0.5 * (high[i] + low[i]);
        let hl = high[i] - low[i];
        tr[i] = if i == 0 {{
            hl
        }} else {{
            hl.max((high[i] - close[i - 1]).abs())
                .max((low[i] - close[i - 1]).abs())
        }};
    }}
    let mut rma_atr = f64::NAN;
    let mut ema_med = f64::NAN;
    let alpha_ema = 2.0 / (length as f64 + 1.0);
    let mut win = vec![0.0_f64; length];
    for i in 0..n {{
        if i + 1 < atr_len {{
            rma_atr = f64::NAN;
        }} else if i + 1 == atr_len {{
            let mut sum = 0.0;
            for j in 0..atr_len {{
                sum += tr[j];
            }}
            rma_atr = sum / (atr_len as f64);
        }} else {{
            rma_atr = (rma_atr * (atr_len as f64 - 1.0) + tr[i]) / (atr_len as f64);
        }}
        if i + 1 < length {{
            continue;
        }}
        let start = i + 1 - length;
        win.copy_from_slice(&hl2[start..=i]);
        win.sort_by(|a, b| a.total_cmp(b));
        let rank_idx = ((0.5 * (length as f64)).ceil() as usize).saturating_sub(1).min(length - 1);
        let median = win[rank_idx];
        ema_med = if !ema_med.is_finite() {{
            median
        }} else {{
            alpha_ema * median + (1.0 - alpha_ema) * ema_med
        }};
        outs[0][i] = median;
        if rma_atr.is_finite() {{
            let band = atr_mult * rma_atr;
            outs[1][i] = median + band;
            outs[2][i] = median - band;
        }}
        outs[3][i] = ema_med;
        if median > ema_med {{
            outs[4][i] = median;
            outs[5][i] = f64::NAN;
        }} else {{
            outs[4][i] = f64::NAN;
            outs[5][i] = median;
        }}
    }}
}}
"#
        );
        let spec = parse_study_source_spec(&src, "Median");
        assert_eq!(spec.pane, StudyPaneTarget::Price);
        assert_eq!(spec.line_width, 2);
        assert_eq!(spec.inputs.len(), 3);
        assert_eq!(spec.inputs[0].identifier, "length");
        assert_eq!(spec.inputs[1].identifier, "atr_length");
        assert_eq!(spec.inputs[2].identifier, "atr_mult");
        assert_eq!(spec.outputs.len(), 6);
        assert_eq!(spec.fills.len(), 2);
        assert_eq!(spec.levels.len(), 1);

        let artifact = compile_study_source_to_wasm(&src).expect("median compiles to wasm");
        let fixture = StudyPreviewFixture::generate(PreviewFixturePreset::RangeBound, 120, 60);
        let res_default = artifact
            .compiled
            .execute_columnar_with_params(
                &fixture.bars,
                spec.outputs.len(),
                &[3.0, 14.0, 2.0],
                StudySandboxLimits::default(),
            )
            .expect("multi-output median executes cleanly with default params");
        assert_eq!(res_default.outputs.len(), 6);
        assert!(res_default.outputs[0][119].is_some());
        assert!(res_default.outputs[1][119].is_some());
        assert!(res_default.outputs[2][119].is_some());
        assert!(res_default.outputs[3][119].is_some());

        let res_wider = artifact
            .compiled
            .execute_columnar_with_params(
                &fixture.bars,
                spec.outputs.len(),
                &[3.0, 14.0, 4.0],
                StudySandboxLimits::default(),
            )
            .expect("multi-output median executes cleanly with overridden multiplier");
        let upper_2x = res_default.outputs[1][119].unwrap();
        let upper_4x = res_wider.outputs[1][119].unwrap();
        assert!(
            upper_4x > upper_2x,
            "increasing atr_mult param from 2.0 to 4.0 must widen upper band ({upper_4x} vs {upper_2x})"
        );
    }

    #[test]
    fn wasm_scene_api_emits_boxes_lines_polylines_labels_tables_and_bar_colors() {
        let src = r#"
// @pane: price
// @input: rows | int | 24 | Rows
pub fn calculate(
    _open: &[f64],
    high: &[f64],
    low: &[f64],
    close: &[f64],
    volume: &[f64],
    out: &mut [f64],
) {
    let n = close.len();
    if n < 10 {
        return;
    }
    let rows = (super::param(0, 24.0).round() as usize).clamp(4, 100);
    let (_vl, _vr) = super::visible_range();
    let mut hi = f64::NEG_INFINITY;
    let mut lo = f64::INFINITY;
    for i in 0..n {
        if high[i] > hi { hi = high[i]; }
        if low[i] < lo { lo = low[i]; }
        out[i] = close[i];
    }
    let step = ((hi - lo) / (rows as f64)).max(1e-6);
    let last = (n - 1) as f64;
    for r in 0..rows {
        let y0 = lo + (r as f64) * step;
        let y1 = y0 + step * 0.85;
        super::draw_box(
            last + 4.0,
            y1,
            last + 18.0,
            y0,
            0,
            super::rgba(0x2962ff, 180),
            0,
        );
    }
    super::draw_line(0.0, hi, last + 20.0, hi, super::rgba(0xf44336, 255), 2, 0, 0);
    super::draw_polyline(&[0.0, last * 0.5, last], &[lo, (lo + hi) * 0.5, hi], super::rgba(0x2962ff, 255), 1, false);
    super::draw_label(last + 20.0, hi, "Profile High", super::rgba(0x1e293b, 220), super::rgba(0xf8fafc, 255), 2);
    super::draw_table_cell(2, 2, 2, 0, 0, "Profile High", super::rgba(0xe2e8f0, 255), super::rgba(0x1e293b, 200), 0);
    super::draw_table_cell(2, 2, 2, 1, 0, &format!("{hi:.2}"), super::rgba(0xe2e8f0, 255), super::rgba(0x1e293b, 200), 2);
    if volume[n - 1] > 0.0 {
        super::set_bar_color(n - 1, super::rgba(0x26a69a, 255));
    }
}
"#;
        let artifact = compile_study_source_to_wasm(src).expect("scene study compiles to wasm");
        let fixture = StudyPreviewFixture::generate(PreviewFixturePreset::RangeBound, 80, 60);
        let res = artifact
            .compiled
            .execute_columnar_full(
                &fixture.bars,
                1,
                &[24.0],
                Some((20, 79)),
                StudySandboxLimits::default(),
            )
            .expect("scene study executes in wasmi");
        assert!(
            !res.scene_bytes.is_empty(),
            "scene study must emit binary scene commands"
        );
        assert!(res.scene_bytes.len() > 24 * 40);
    }
}
