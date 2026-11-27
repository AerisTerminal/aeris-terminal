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
use aeris_study_sdk::sandbox::{STUDY_SANDBOX_ABI_VERSION, StudySandboxLimits};
use aeris_study_sdk::toolchain::{
    PreviewFixturePreset, STUDY_SANDBOX_TARGET_TRIPLE, STUDY_TOOLCHAIN_POLICY_VERSION,
    StudyArtifactCacheKey, StudyDevPackStatus, StudyPreviewFixture,
};
use aeris_study_sdk::wasm_runtime::CompiledWasmStudy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const PROJECTS_DIR_NAME: &str = "study_projects";
const ARTIFACT_CACHE_DIR_NAME: &str = ".artifact_cache";
const MAXIMUM_RETAINED_ARTIFACT_CACHE_ENTRIES: usize = 128;
const MANIFEST_FILE_NAME: &str = "manifest.json";
const SOURCE_FILE_NAME: &str = "lib.rs";
const LAST_GOOD_FILE_NAME: &str = "last_good.rs";
const LAST_GOOD_WASM_FILE_NAME: &str = "last_good.wasm";

static BUILD_DIR_NONCE: AtomicU64 = AtomicU64::new(1);

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

/// Probes whether a local Rust compiler and `wasm32-wasip2` sandbox target are available.
#[must_use]
pub(crate) fn probe_study_dev_pack() -> StudyDevPackStatus {
    let Ok(output) = std::process::Command::new("rustc")
        .arg("--version")
        .output()
    else {
        return StudyDevPackStatus::Unavailable;
    };
    if !output.status.success() {
        return StudyDevPackStatus::Unavailable;
    }
    let rustc_version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if rustc_version.is_empty() {
        return StudyDevPackStatus::Unavailable;
    }
    let target_ok = std::process::Command::new("rustc")
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

/// Validated output of one controlled Rust -> WebAssembly study compilation.
#[derive(Clone, Debug)]
pub(crate) struct CompiledStudyArtifact {
    pub(crate) wasm_bytes: Vec<u8>,
    pub(crate) artifact_sha256: String,
    pub(crate) source_sha256: String,
    pub(crate) toolchain_identity: String,
    pub(crate) compiled: CompiledWasmStudy,
}

const WASM_HARNESS_SOURCE: &str = r#"#[forbid(unsafe_code)]
mod user_study;

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_abi_version() -> u32 {
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_alloc(byte_len: u32) -> u32 {
    let len = byte_len as usize;
    let mut buf = Vec::<u8>::with_capacity(len);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn aeris_study_calculate(bars_ptr: u32, bar_count: u32, out_ptr: u32) -> i32 {
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
    if let Err(err) =
        fs::write(&harness_path, WASM_HARNESS_SOURCE).and_then(|()| fs::write(&user_path, source))
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

    let output = std::process::Command::new("rustc")
        .args([
            "--edition",
            "2024",
            "--crate-type",
            "cdylib",
            "--target",
            STUDY_SANDBOX_TARGET_TRIPLE,
            "-O",
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
) -> (Vec<StudyDiagnostic>, String) {
    let fixture = StudyPreviewFixture::generate(preset, 120, 60);
    match compile_study_source_to_wasm(source) {
        Ok(artifact) => {
            match artifact
                .compiled
                .execute_columnar(&fixture.bars, StudySandboxLimits::default())
            {
                Ok(result) => {
                    let finite_count = result.values.iter().filter(|v| v.is_some()).count();
                    let last_label = result
                        .values
                        .iter()
                        .rev()
                        .find_map(|v| *v)
                        .map_or_else(|| "NaN".to_string(), |v| format!("{v:.2}"));
                    (
                        Vec::new(),
                        format!(
                            "WASM Preview [{preset:?}]: {finite_count}/{} pts, last={last_label}, fuel={}",
                            fixture.bars.len(),
                            result.fuel_consumed
                        ),
                    )
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
                ),
            }
        }
        Err(diagnostics) => {
            let summary = format!(
                "Preview [{preset:?}] blocked: {} diagnostic(s) must be fixed",
                diagnostics.len()
            );
            (diagnostics, summary)
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
        let (diags, summary) = run_offline_preview_check(&clean_src, PreviewFixturePreset::Uptrend);
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        assert!(
            summary.contains("120/120 pts"),
            "unexpected summary: {summary}"
        );

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
        let (loop_diags, loop_summary) =
            run_offline_preview_check(loopy, PreviewFixturePreset::Uptrend);
        assert_eq!(loop_diags.len(), 1);
        assert_eq!(loop_diags[0].code.as_deref(), Some("AERIS_WASM_TRAP"));
        assert!(loop_summary.contains("fuel"), "summary: {loop_summary}");
    }
}
