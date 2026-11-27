//! Phase L Foundation: versioned user-study project manifest, capability declarations,
//! starter templates, and structured compiler/runtime diagnostics for the in-app Rust Study Editor.
//!
//! This module owns pure, deterministic, bounded project metadata and template generation.
//! It performs no disk I/O, spawns no compiler processes, and loads no native libraries;
//! execution of user-authored studies remains gated behind the versioned sandbox boundary.

use crate::{MAXIMUM_STUDY_IDENTIFIER_BYTES, STUDY_SDK_COMPATIBILITY_EPOCH};
use std::collections::{BTreeMap, BTreeSet};

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

/// One user-authored Rust study project held by the Study Editor library.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudyProject {
    pub manifest: StudyProjectManifest,
    pub source: String,
    /// Last-known-good source preserved across failed edits/builds so Revert always works.
    pub last_good_source: Option<String>,
    /// Bounded diagnostics from the latest build or validation attempt.
    pub diagnostics: Vec<StudyDiagnostic>,
}

/// Errors returned by [`StudyProjectLibrary`] mutations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StudyProjectLibraryError {
    LibraryFull { maximum: usize },
    DuplicateIdentifier(String),
    UnknownProject(String),
    NoLastGoodBuild(String),
    Validation(StudyProjectValidationError),
}

impl From<StudyProjectValidationError> for StudyProjectLibraryError {
    fn from(error: StudyProjectValidationError) -> Self {
        Self::Validation(error)
    }
}

/// Bounded in-memory owner of user-authored Rust study projects for the Study Editor.
///
/// Capacity is capped at [`MAXIMUM_USER_STUDY_PROJECTS`]; creating past the cap is refused
/// rather than silently evicting a user's study.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StudyProjectLibrary {
    projects: BTreeMap<String, StudyProject>,
}

impl StudyProjectLibrary {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            projects: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.projects.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.projects.is_empty()
    }

    #[must_use]
    pub fn get(&self, identifier: &str) -> Option<&StudyProject> {
        self.projects.get(identifier)
    }

    pub fn iter(&self) -> impl Iterator<Item = &StudyProject> {
        self.projects.values()
    }

    /// Creates a new project from a starter template without evicting existing projects.
    ///
    /// # Errors
    /// Returns an error if the library is full, `identifier` already exists, or validation fails.
    pub fn create_from_template(
        &mut self,
        identifier: &str,
        name: &str,
        template: StudyProjectTemplate,
    ) -> Result<&StudyProject, StudyProjectLibraryError> {
        if self.projects.len() >= MAXIMUM_USER_STUDY_PROJECTS {
            return Err(StudyProjectLibraryError::LibraryFull {
                maximum: MAXIMUM_USER_STUDY_PROJECTS,
            });
        }
        if self.projects.contains_key(identifier) {
            return Err(StudyProjectLibraryError::DuplicateIdentifier(
                identifier.to_string(),
            ));
        }
        let (manifest, source) = template.scaffold(identifier, name);
        manifest.validate()?;
        validate_study_source(&source)?;
        let key = manifest.identifier.clone();
        Ok(self.projects.entry(key).or_insert(StudyProject {
            manifest,
            source,
            last_good_source: None,
            diagnostics: Vec::new(),
        }))
    }

    /// Replaces the editable source text while preserving `last_good_source`.
    ///
    /// # Errors
    /// Returns an error if the project does not exist or the source exceeds bounds.
    pub fn update_source(
        &mut self,
        identifier: &str,
        source: String,
    ) -> Result<(), StudyProjectLibraryError> {
        validate_study_source(&source)?;
        let project = self
            .projects
            .get_mut(identifier)
            .ok_or_else(|| StudyProjectLibraryError::UnknownProject(identifier.to_string()))?;
        project.source = source;
        Ok(())
    }

    /// Records a successful build, advancing `last_good_source` and digest metadata.
    ///
    /// # Errors
    /// Returns an error if the project does not exist or digests are malformed.
    pub fn record_build_success(
        &mut self,
        identifier: &str,
        source_sha256: String,
        artifact_sha256: String,
    ) -> Result<(), StudyProjectLibraryError> {
        let project = self
            .projects
            .get_mut(identifier)
            .ok_or_else(|| StudyProjectLibraryError::UnknownProject(identifier.to_string()))?;
        let mut candidate = project.manifest.clone();
        candidate.last_good_source_sha256 = Some(source_sha256);
        candidate.last_good_artifact_sha256 = Some(artifact_sha256);
        candidate.validate()?;
        project.manifest = candidate;
        project.last_good_source = Some(project.source.clone());
        project.diagnostics.clear();
        Ok(())
    }

    /// Records bounded diagnostics from a failed build without overwriting `last_good_source`.
    ///
    /// # Errors
    /// Returns an error if the project does not exist.
    pub fn record_build_failure(
        &mut self,
        identifier: &str,
        mut diagnostics: Vec<StudyDiagnostic>,
    ) -> Result<(), StudyProjectLibraryError> {
        let project = self
            .projects
            .get_mut(identifier)
            .ok_or_else(|| StudyProjectLibraryError::UnknownProject(identifier.to_string()))?;
        diagnostics.truncate(MAXIMUM_STUDY_DIAGNOSTICS);
        project.diagnostics = diagnostics;
        Ok(())
    }

    /// Reverts the editable source back to the last successfully built source.
    ///
    /// # Errors
    /// Returns an error if the project does not exist or has never built successfully.
    pub fn revert_to_last_good(
        &mut self,
        identifier: &str,
    ) -> Result<(), StudyProjectLibraryError> {
        let project = self
            .projects
            .get_mut(identifier)
            .ok_or_else(|| StudyProjectLibraryError::UnknownProject(identifier.to_string()))?;
        let good = project
            .last_good_source
            .clone()
            .ok_or_else(|| StudyProjectLibraryError::NoLastGoodBuild(identifier.to_string()))?;
        project.source = good;
        project.diagnostics.clear();
        Ok(())
    }

    /// Removes one project from the library.
    pub fn delete(&mut self, identifier: &str) -> bool {
        self.projects.remove(identifier).is_some()
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

    #[test]
    fn library_enforces_capacity_and_reverts_failed_edits_to_last_good_build() {
        let mut library = StudyProjectLibrary::new();
        library
            .create_from_template(
                "user.alpha",
                "Alpha",
                StudyProjectTemplate::StatelessOverlay,
            )
            .expect("creates project");
        let original_source = library.get("user.alpha").unwrap().source.clone();
        let digest = "a".repeat(64);
        library
            .record_build_success("user.alpha", digest.clone(), digest)
            .expect("records good build");
        library
            .update_source("user.alpha", "broken rust code".to_string())
            .expect("updates editable source");
        library
            .record_build_failure(
                "user.alpha",
                vec![StudyDiagnostic {
                    severity: StudyDiagnosticSeverity::Error,
                    line: 1,
                    column: 1,
                    code: Some("E0425".to_string()),
                    message: "expected item".to_string(),
                }],
            )
            .expect("records failure");
        assert_eq!(library.get("user.alpha").unwrap().diagnostics.len(), 1);
        library
            .revert_to_last_good("user.alpha")
            .expect("reverts cleanly");
        assert_eq!(library.get("user.alpha").unwrap().source, original_source);
        assert_eq!(
            library.get("user.alpha").unwrap().diagnostics,
            Vec::<StudyDiagnostic>::new()
        );
    }
}
