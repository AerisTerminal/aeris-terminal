//! Phase L Foundation: versioned user-study project manifest, capability declarations,
//! starter templates, and structured compiler/runtime diagnostics for the in-app Rust Study Editor.
//!
//! This module owns pure, deterministic, bounded project metadata and template generation.
//! It performs no disk I/O, spawns no compiler processes, and loads no native libraries;
//! execution of user-authored studies remains gated behind the versioned sandbox boundary.

use crate::{MAXIMUM_STUDY_IDENTIFIER_BYTES, STUDY_SDK_COMPATIBILITY_EPOCH};
use std::collections::BTreeSet;

/// Maximum user-authored study projects retained in one workspace library.
pub const MAXIMUM_USER_STUDY_PROJECTS: usize = 64;
/// Maximum UTF-8 bytes accepted in one user study source file.
pub const MAXIMUM_STUDY_SOURCE_BYTES: usize = 256 * 1024;
/// Maximum structured diagnostics retained for one build/validation attempt.
pub const MAXIMUM_STUDY_DIAGNOSTICS: usize = 128;
/// Maximum UTF-8 bytes in a project display name.
pub const MAXIMUM_PROJECT_NAME_BYTES: usize = 96;
/// Current schema version for user study project manifests.
pub const STUDY_PROJECT_MANIFEST_VERSION: u32 = 1;

/// Explicit sandbox capabilities a user-authored study project may request.
///
/// Ambient OS authority (filesystem, network, subprocess, environment, credentials,
/// system wall-clock, randomness, native UI) is intentionally absent and permanently denied.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StudyProjectCapability {
    /// Read canonical bar history and live bar updates for declared market dependencies.
    MarketBars,
    /// Read borrowed live quotes, trades, and order-book depth for declared market dependencies.
    MarketMicrostructure,
    /// Request higher/lower timeframe or cross-symbol market contexts via `MarketEngine`.
    MultiContextRequests,
    /// Mutate the bounded transactional semantic drawing scene and dashboard tables.
    SemanticScene,
    /// Observe read-only visible chart viewport range (`StudyChartContext`).
    ChartViewportContext,
    /// Emit bounded typed alert events for host-owned delivery.
    StudyAlerts,
}

/// Versioned durable manifest for one user-authored Rust study project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudyProjectManifest {
    pub manifest_version: u32,
    /// Stable durable study identifier (must not use the reserved `builtin.` prefix).
    pub identifier: String,
    /// Human-readable display name shown in the Study Editor and indicator menu.
    pub name: String,
    /// Exact Study SDK compatibility epoch this project targets.
    pub sdk_compatibility_epoch: u32,
    /// Monotonic durable implementation revision (`>= 1`).
    pub implementation_revision: u32,
    /// Explicit capabilities requested by this study.
    pub capabilities: BTreeSet<StudyProjectCapability>,
    /// SHA-256 hex digest of the last successfully built source, if any.
    pub last_good_source_sha256: Option<String>,
    /// SHA-256 hex digest of the last successfully admitted sandbox artifact, if any.
    pub last_good_artifact_sha256: Option<String>,
}

/// Validation error for a user-authored study project manifest or source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StudyProjectValidationError {
    UnsupportedManifestVersion(u32),
    InvalidIdentifier,
    ReservedIdentifier,
    InvalidName,
    IncompatibleSdkEpoch { project_epoch: u32, host_epoch: u32 },
    InvalidImplementationRevision,
    EmptyCapabilities,
    SourceTooLarge { maximum: usize, actual: usize },
    EmptySource,
    InvalidDigest,
}

impl StudyProjectManifest {
    /// Creates a new project manifest with sensible default capabilities (`MarketBars`).
    #[must_use]
    pub fn new(identifier: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            manifest_version: STUDY_PROJECT_MANIFEST_VERSION,
            identifier: identifier.into(),
            name: name.into(),
            sdk_compatibility_epoch: STUDY_SDK_COMPATIBILITY_EPOCH,
            implementation_revision: 1,
            capabilities: BTreeSet::from([StudyProjectCapability::MarketBars]),
            last_good_source_sha256: None,
            last_good_artifact_sha256: None,
        }
    }

    /// Validates all structural, identity, epoch, capability, and digest invariants.
    ///
    /// # Errors
    /// Returns [`StudyProjectValidationError`] when any manifest invariant is violated.
    pub fn validate(&self) -> Result<(), StudyProjectValidationError> {
        if self.manifest_version != STUDY_PROJECT_MANIFEST_VERSION {
            return Err(StudyProjectValidationError::UnsupportedManifestVersion(
                self.manifest_version,
            ));
        }
        let id = self.identifier.trim();
        if id.is_empty()
            || id != self.identifier
            || id.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(StudyProjectValidationError::InvalidIdentifier);
        }
        if id.starts_with("builtin.") {
            return Err(StudyProjectValidationError::ReservedIdentifier);
        }
        let name = self.name.trim();
        if name.is_empty() || name.len() > MAXIMUM_PROJECT_NAME_BYTES {
            return Err(StudyProjectValidationError::InvalidName);
        }
        if self.sdk_compatibility_epoch != STUDY_SDK_COMPATIBILITY_EPOCH {
            return Err(StudyProjectValidationError::IncompatibleSdkEpoch {
                project_epoch: self.sdk_compatibility_epoch,
                host_epoch: STUDY_SDK_COMPATIBILITY_EPOCH,
            });
        }
        if self.implementation_revision == 0 {
            return Err(StudyProjectValidationError::InvalidImplementationRevision);
        }
        if self.capabilities.is_empty() {
            return Err(StudyProjectValidationError::EmptyCapabilities);
        }
        for digest in [
            self.last_good_source_sha256.as_deref(),
            self.last_good_artifact_sha256.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(StudyProjectValidationError::InvalidDigest);
            }
        }
        Ok(())
    }
}

/// Validates user study Rust source text bounds before saving or scheduling a build.
///
/// # Errors
/// Returns [`StudyProjectValidationError::EmptySource`] or [`StudyProjectValidationError::SourceTooLarge`].
pub fn validate_study_source(source: &str) -> Result<(), StudyProjectValidationError> {
    if source.trim().is_empty() {
        return Err(StudyProjectValidationError::EmptySource);
    }
    if source.len() > MAXIMUM_STUDY_SOURCE_BYTES {
        return Err(StudyProjectValidationError::SourceTooLarge {
            maximum: MAXIMUM_STUDY_SOURCE_BYTES,
            actual: source.len(),
        });
    }
    Ok(())
}

/// Severity level for a structured Study Editor diagnostic.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StudyDiagnosticSeverity {
    Error,
    Warning,
    Note,
}

/// Structured compiler, validator, or runtime diagnostic anchored to source coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudyDiagnostic {
    pub severity: StudyDiagnosticSeverity,
    pub line: u32,
    pub column: u32,
    pub code: Option<String>,
    pub message: String,
}

/// Starter templates offered by the in-app Rust Study Editor.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StudyProjectTemplate {
    StatelessOverlay,
    StatefulRecursive,
    MultiTimeframe,
    DrawingTableDashboard,
    AlertStudy,
}

fn stateless_overlay_source(identifier: &str, name: &str) -> String {
    format!(
        r#"use aeris_study_sdk::prelude::*;

pub fn definition(series: BarSeriesKey) -> StudyDefinition {{
    StudyDefinitionBuilder::new("{identifier}")
        .market(series, StreamRequirements::BARS)
        .output(StudyOutputBuilder::line("mid", "{name}").build())
        .invalidation(StudyInvalidationPolicy::SameRange)
        .build()
}}

pub fn calculate(ctx: &mut StudyExecutionContext<'_>) -> Result<(), String> {{
    let (inputs, outputs) = ctx.split();
    let Some(StudyInputSeries::Market(market)) = inputs.input(0) else {{
        return Ok(());
    }};
    let high = market.field(StudyBarField::High);
    let low = market.field(StudyBarField::Low);
    let dirty = inputs.dirty_range();
    let end = dirty.end_exclusive.unwrap_or(outputs[0].len());
    for row in dirty.start..end {{
        let mid = high.value(row).zip(low.value(row)).and_then(|(h, l)| {{
            let hf = fixed_point_to_f64(h, high.scale())?;
            let lf = fixed_point_to_f64(l, low.scale())?;
            Some((hf + lf) * 0.5)
        }});
        outputs[0].set(row, mid)?;
    }}
    Ok(())
}}
"#
    )
}

impl StudyProjectTemplate {
    /// Returns the required capabilities and starter Rust source for this template.
    #[must_use]
    pub fn scaffold(self, identifier: &str, name: &str) -> (StudyProjectManifest, String) {
        let mut manifest = StudyProjectManifest::new(identifier, name);
        let source = match self {
            Self::StatelessOverlay => stateless_overlay_source(identifier, name),
            Self::StatefulRecursive => format!(
                r#"use aeris_study_sdk::prelude::*;

#[derive(Clone, Copy, Default)]
pub struct RunningState {{
    pub last_value: Option<f64>,
}}

pub fn definition(series: BarSeriesKey) -> StudyDefinition {{
    StudyDefinitionBuilder::new("{identifier}")
        .market(series, StreamRequirements::BARS)
        .output(StudyOutputBuilder::line("value", "{name}").build())
        .invalidation(StudyInvalidationPolicy::FromFirstChanged)
        .build()
}}
"#
            ),
            Self::MultiTimeframe => {
                manifest
                    .capabilities
                    .insert(StudyProjectCapability::MultiContextRequests);
                format!(
                    r#"use aeris_study_sdk::prelude::*;

pub fn definition(primary: BarSeriesKey, higher_tf: BarSeriesKey) -> StudyDefinition {{
    StudyDefinitionBuilder::new("{identifier}")
        .market(primary, StreamRequirements::BARS)
        .market(higher_tf, StreamRequirements::BARS)
        .setting(
            StudySettingBuilder::timeframe("htf", StudyTimeframe::parse_label("1D").unwrap(), false)
                .label("Higher Timeframe")
                .build(),
        )
        .output(StudyOutputBuilder::line("htf_close", "{name}").build())
        .invalidation(StudyInvalidationPolicy::FromFirstChanged)
        .build()
}}
"#
                )
            }
            Self::DrawingTableDashboard => {
                manifest
                    .capabilities
                    .insert(StudyProjectCapability::SemanticScene);
                format!(
                    r#"use aeris_study_sdk::prelude::*;

pub fn definition(series: BarSeriesKey) -> StudyDefinition {{
    StudyDefinitionBuilder::new("{identifier}")
        .market(series, StreamRequirements::BARS)
        .output(StudyOutputBuilder::line("anchor", "{name}").build())
        .invalidation(StudyInvalidationPolicy::FromFirstChanged)
        .build()
}}
"#
                )
            }
            Self::AlertStudy => {
                manifest
                    .capabilities
                    .insert(StudyProjectCapability::StudyAlerts);
                format!(
                    r#"use aeris_study_sdk::prelude::*;

pub fn definition(series: BarSeriesKey) -> StudyDefinition {{
    StudyDefinitionBuilder::new("{identifier}")
        .market(series, StreamRequirements::BARS)
        .output(StudyOutputBuilder::line("signal", "{name}").build())
        .invalidation(StudyInvalidationPolicy::SameRange)
        .build()
}}
"#
                )
            }
        };
        (manifest, source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_scaffold_valid_manifests_and_bounded_sources() {
        for template in [
            StudyProjectTemplate::StatelessOverlay,
            StudyProjectTemplate::StatefulRecursive,
            StudyProjectTemplate::MultiTimeframe,
            StudyProjectTemplate::DrawingTableDashboard,
            StudyProjectTemplate::AlertStudy,
        ] {
            let (manifest, source) = template.scaffold("user.sample_study", "Sample Study");
            manifest.validate().expect("template manifest validates");
            validate_study_source(&source).expect("template source validates");
        }
    }

    #[test]
    fn manifest_rejects_reserved_builtin_prefix_and_bad_digests() {
        let reserved = StudyProjectManifest::new("builtin.custom", "Reserved");
        assert_eq!(
            reserved.validate(),
            Err(StudyProjectValidationError::ReservedIdentifier)
        );

        let mut bad_digest = StudyProjectManifest::new("user.ok", "Good");
        bad_digest.last_good_source_sha256 = Some("not-a-sha256".to_string());
        assert_eq!(
            bad_digest.validate(),
            Err(StudyProjectValidationError::InvalidDigest)
        );
    }
}
