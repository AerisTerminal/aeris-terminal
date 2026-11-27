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

use aeris_study_sdk::project::{
    MAXIMUM_STUDY_DIAGNOSTICS, MAXIMUM_STUDY_SOURCE_BYTES, MAXIMUM_USER_STUDY_PROJECTS,
    StudyDiagnostic, StudyDiagnosticSeverity, StudyProject, StudyProjectCapability,
    StudyProjectLibrary, StudyProjectManifest, StudyProjectTemplate, validate_study_source,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

const PROJECTS_DIR_NAME: &str = "study_projects";
const MANIFEST_FILE_NAME: &str = "manifest.json";
const SOURCE_FILE_NAME: &str = "lib.rs";
const LAST_GOOD_FILE_NAME: &str = "last_good.rs";

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

/// Computes the canonical lowercase SHA-256 hex digest of a Rust source string.
#[must_use]
pub(crate) fn source_sha256_hex(source: &str) -> String {
    let digest = Sha256::digest(source.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
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
}
