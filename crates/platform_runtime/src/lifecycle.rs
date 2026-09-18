//! Signed, transactional install/update/uninstall mechanics.
//!
//! Packaging supplies the small stable launcher that calls this boundary. The
//! desktop never replaces or deletes itself.

#[cfg(target_os = "windows")]
#[cfg_attr(not(test), allow(unused_imports))]
use std::os::windows::fs::OpenOptionsExt;
use std::{
    collections::BTreeSet,
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
#[cfg(target_os = "windows")]
use std::{
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use same_file::Handle as FileIdentity;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest as _, Sha256};

pub const RELEASE_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const RELEASE_CHANNEL_SCHEMA_VERSION: u32 = 1;
pub const ROLLBACK_COMPATIBILITY_SCHEMA_VERSION: u32 = 1;
pub const ROLLBACK_COMPATIBILITY_FILENAME: &str = "rollback-compatibility.json";
/// Bump only when persisted local state is no longer backward-readable by the
/// immediately preceding release. Additive protobuf fields, preserved field
/// numbers/types/meaning, and permanently retired tags stay in the same epoch.
pub const CURRENT_STATE_COMPATIBILITY_EPOCH: u32 = 1;
const INVENTORY_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_MANIFEST_BYTES: usize = 1024 * 1024;
const MAXIMUM_ROLLBACK_COMPATIBILITY_BYTES: u64 = 1024;
const MAXIMUM_RELEASE_FILES: usize = 256;
const MAXIMUM_OWNED_ROOTS: usize = 64;
const MAXIMUM_VAULT_KEYS: usize = 64;
const MAXIMUM_RELEASE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
#[cfg(target_os = "windows")]
const AUTHENTICODE_AUDIT_TIMEOUT: Duration = Duration::from_secs(15);

/// Role of one immutable release file.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseFileRole {
    Desktop,
    Engine,
    RuntimeAsset,
}

/// One exact file in a signed release inventory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseFile {
    pub role: ReleaseFileRole,
    pub path: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}

/// Bounded server-controlled rollout metadata carried by the signed manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RolloutMetadata {
    pub cohort: String,
    pub percentage: u8,
}

/// Canonical immutable release description signed by offline packaging.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema_version: u32,
    pub release_identity: String,
    pub install_generation: u64,
    pub channel: String,
    pub minimum_version: String,
    pub platform: String,
    pub architecture: String,
    pub files: Vec<ReleaseFile>,
    pub rollout: RolloutMetadata,
}

/// Manifest plus an Ed25519 signature over its canonical JSON representation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedReleaseManifest {
    pub manifest: ReleaseManifest,
    pub signature: String,
}

/// Mutable channel metadata. The embedded signed release remains the trust
/// root; every duplicated field is cross-checked by the launcher before use.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseChannelPointer {
    pub schema_version: u32,
    pub channel: String,
    pub platform: String,
    #[serde(rename = "arch")]
    pub architecture: String,
    pub release_identity: String,
    #[serde(rename = "generation")]
    pub install_generation: u64,
    pub version: String,
    pub published_at: String,
    pub manifest_url: String,
    pub signed_release: SignedReleaseManifest,
    pub installer: ReleaseInstallerMetadata,
}

/// Website-visible setup metadata carried beside the signed release payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseInstallerMetadata {
    pub filename: String,
    pub url: String,
    pub size: u64,
    pub sha256_b64url: String,
}

/// Local policy applied in addition to the signed manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleasePolicy {
    pub platform: String,
    pub architecture: String,
    pub current_version: String,
    pub minimum_install_generation: u64,
    pub maximum_release_bytes: u64,
}

impl ReleasePolicy {
    #[must_use]
    pub fn native(minimum_install_generation: u64) -> Self {
        Self {
            platform: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            minimum_install_generation,
            maximum_release_bytes: MAXIMUM_RELEASE_BYTES,
        }
    }
}

/// Exact active release selected by the stable launcher.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveRelease {
    pub release_identity: String,
    pub install_generation: u64,
    pub directory_name: String,
}

/// Signed-inventory asset that fences automatic rollback across incompatible
/// persisted-state migrations without changing the release manifest schema.
///
/// The current workspace file is protobuf: releases that only add fields while
/// preserving existing field numbers, wire types, meaning, and retired tags use
/// the same epoch. Any forward-only/destructive migration must increment the
/// epoch, making automatic rollback fail closed before an older binary sees the
/// new state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RollbackCompatibilityMetadata {
    pub schema_version: u32,
    pub state_compatibility_epoch: u32,
}

impl RollbackCompatibilityMetadata {
    #[must_use]
    pub const fn current() -> Self {
        Self {
            schema_version: ROLLBACK_COMPATIBILITY_SCHEMA_VERSION,
            state_compatibility_epoch: CURRENT_STATE_COMPATIBILITY_EPOCH,
        }
    }
}

/// One exact native credential-vault namespace and non-secret key.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultEntry {
    pub service: String,
    pub key: String,
}

/// Versioned inventory of every local artifact owned by Axiusflow.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationInventory {
    pub schema_version: u32,
    pub install_root: PathBuf,
    pub data_roots: Vec<PathBuf>,
    pub cache_roots: Vec<PathBuf>,
    pub log_roots: Vec<PathBuf>,
    #[serde(default, alias = "ipc_paths", skip_serializing_if = "Vec::is_empty")]
    pub legacy_cleanup_roots: Vec<PathBuf>,
    pub vault_entries: Vec<VaultEntry>,
    pub registrations: Vec<String>,
}

/// Platform operations which cannot be implemented as portable filesystem work.
pub trait LifecycleHooks {
    /// Blocks relaunch and stops the exact active desktop identity.
    ///
    /// # Errors
    /// Returns a redacted platform error if owned processes cannot be stopped.
    fn prepare_activation(&self, previous: Option<&ActiveRelease>) -> Result<(), String>;
    /// Performs the bounded desktop in-process runtime readiness probe.
    ///
    /// # Errors
    /// Returns a redacted readiness error for rollback.
    fn health_check(&self, candidate: &ActiveRelease) -> Result<(), String>;
    /// Disables every exact native service, shortcut, and autostart registration.
    ///
    /// # Errors
    /// Returns a redacted platform error if a registration remains enabled.
    fn disable_registrations(&self, registrations: &[String]) -> Result<(), String>;
    /// Stops all verified Axiusflow process identities before uninstall.
    ///
    /// # Errors
    /// Returns a redacted error if an owned process remains active.
    fn stop_owned_processes(&self) -> Result<(), String>;
    /// Deletes one exact native-vault identifier idempotently.
    ///
    /// # Errors
    /// Returns a redacted vault error if deletion cannot be confirmed.
    fn delete_vault_entry(&self, entry: &VaultEntry) -> Result<(), String>;
    /// Audits process, registration, and vault absence after local deletion.
    ///
    /// # Errors
    /// Returns a redacted remaining-artifact category.
    fn audit_external_absence(&self, inventory: &InstallationInventory) -> Result<(), String>;
}

/// Successful update state. Success keeps at most one verified predecessor as
/// the retained known-good release and removes any older retained generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateOutcome {
    pub active: ActiveRelease,
    pub removed_release: Option<ActiveRelease>,
}

/// Successful complete local removal audit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UninstallOutcome {
    pub removed_roots: usize,
    pub removed_vault_keys: usize,
}

/// Redacted lifecycle failure category suitable for user-visible recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleError {
    InvalidManifest,
    InvalidSignature,
    IncompatibleRelease,
    DowngradeRejected,
    InvalidInventory,
    UpdateLocked,
    StagingFailed,
    VerificationFailed,
    ShutdownFailed,
    HealthCheckFailed,
    RollbackFailed,
    UpdatePendingCleanup,
    UninstallPendingCleanup,
    ExternalArtifactRemaining,
    JournalCorrupt,
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidManifest => "release manifest is invalid",
            Self::InvalidSignature => "release signature verification failed",
            Self::IncompatibleRelease => "release does not match this platform",
            Self::DowngradeRejected => "release downgrade policy rejected activation",
            Self::InvalidInventory => "Axiusflow ownership inventory is invalid",
            Self::UpdateLocked => "another lifecycle transaction is active",
            Self::StagingFailed => "release staging did not complete",
            Self::VerificationFailed => "staged release inventory verification failed",
            Self::ShutdownFailed => "owned processes did not stop cleanly",
            Self::HealthCheckFailed => "candidate release failed its readiness check",
            Self::RollbackFailed => "candidate rollback requires remediation",
            Self::UpdatePendingCleanup => "update is pending superseded-file cleanup",
            Self::UninstallPendingCleanup => "uninstall is pending local-artifact cleanup",
            Self::ExternalArtifactRemaining => "an external Axiusflow artifact remains",
            Self::JournalCorrupt => "lifecycle recovery journal is invalid",
        })
    }
}

impl Error for LifecycleError {}

/// Signs one validated manifest. The signing key stays in release packaging.
///
/// # Errors
/// Returns an error if the manifest cannot be validated or serialized.
pub fn sign_release_manifest(
    manifest: ReleaseManifest,
    key: &SigningKey,
) -> Result<SignedReleaseManifest, LifecycleError> {
    validate_manifest_shape(&manifest)?;
    let canonical = canonical_manifest(&manifest)?;
    Ok(SignedReleaseManifest {
        manifest,
        signature: URL_SAFE_NO_PAD.encode(key.sign(&canonical).to_bytes()),
    })
}

/// Verifies signature, bounds, platform, architecture, and downgrade policy.
///
/// # Errors
/// Returns a redacted validation category without exposing manifest payloads.
pub fn verify_release_manifest(
    signed: &SignedReleaseManifest,
    key: &VerifyingKey,
    policy: &ReleasePolicy,
) -> Result<(), LifecycleError> {
    validate_manifest_shape(&signed.manifest)?;
    let canonical = canonical_manifest(&signed.manifest)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(&signed.signature)
        .map_err(|_| LifecycleError::InvalidSignature)?;
    let signature = Signature::from_slice(&bytes).map_err(|_| LifecycleError::InvalidSignature)?;
    key.verify(&canonical, &signature)
        .map_err(|_| LifecycleError::InvalidSignature)?;
    if signed.manifest.platform != policy.platform
        || signed.manifest.architecture != policy.architecture
    {
        return Err(LifecycleError::IncompatibleRelease);
    }
    let minimum_version = semver::Version::parse(&signed.manifest.minimum_version)
        .map_err(|_| LifecycleError::InvalidManifest)?;
    let current_version = semver::Version::parse(&policy.current_version)
        .map_err(|_| LifecycleError::IncompatibleRelease)?;
    if current_version < minimum_version {
        return Err(LifecycleError::IncompatibleRelease);
    }
    if signed.manifest.install_generation < policy.minimum_install_generation {
        return Err(LifecycleError::DowngradeRejected);
    }
    let total = signed
        .manifest
        .files
        .iter()
        .try_fold(0_u64, |sum, file| sum.checked_add(file.size))
        .ok_or(LifecycleError::InvalidManifest)?;
    if total > policy.maximum_release_bytes {
        return Err(LifecycleError::InvalidManifest);
    }
    Ok(())
}

/// Verifies only the cryptographic signature over a release manifest.
///
/// This is intentionally narrower than [`verify_release_manifest`]. Release
/// publication uses it only to authenticate an already-published predecessor
/// whose inventory may belong to a retired architecture. New candidates must
/// still pass the full current manifest-shape and policy validation.
///
/// # Errors
/// Returns an error when the canonical manifest cannot be serialized or the
/// signature is malformed or does not match the supplied key.
pub fn verify_release_manifest_signature(
    signed: &SignedReleaseManifest,
    key: &VerifyingKey,
) -> Result<(), LifecycleError> {
    let canonical = canonical_manifest(&signed.manifest)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(&signed.signature)
        .map_err(|_| LifecycleError::InvalidSignature)?;
    let signature = Signature::from_slice(&bytes).map_err(|_| LifecycleError::InvalidSignature)?;
    key.verify(&canonical, &signature)
        .map_err(|_| LifecycleError::InvalidSignature)
}

fn canonical_manifest(manifest: &ReleaseManifest) -> Result<Vec<u8>, LifecycleError> {
    let bytes = serde_json::to_vec(manifest).map_err(|_| LifecycleError::InvalidManifest)?;
    if bytes.len() > MAXIMUM_MANIFEST_BYTES {
        return Err(LifecycleError::InvalidManifest);
    }
    Ok(bytes)
}

fn validate_manifest_shape(manifest: &ReleaseManifest) -> Result<(), LifecycleError> {
    if manifest.schema_version != RELEASE_MANIFEST_SCHEMA_VERSION
        || manifest.install_generation == 0
        || !valid_identifier(&manifest.release_identity, 128)
        || !valid_identifier(&manifest.channel, 32)
        || manifest.minimum_version.len() > 64
        || semver::Version::parse(&manifest.minimum_version).is_err()
        || !valid_identifier(&manifest.platform, 32)
        || !valid_identifier(&manifest.architecture, 32)
        || !valid_identifier(&manifest.rollout.cohort, 64)
        || manifest.rollout.percentage > 100
        || manifest.files.is_empty()
        || manifest.files.len() > MAXIMUM_RELEASE_FILES
    {
        return Err(LifecycleError::InvalidManifest);
    }
    let mut paths = BTreeSet::new();
    let mut desktop = 0;
    let mut engine = 0;
    let mut previous = None;
    for file in &manifest.files {
        if file.size == 0
            || !safe_relative_path(Path::new(&file.path))
            || !file.url.starts_with("https://")
            || URL_SAFE_NO_PAD.decode(&file.sha256).is_err_and(|_| true)
            || !paths.insert(file.path.as_str())
            || previous.is_some_and(|path| path >= file.path.as_str())
        {
            return Err(LifecycleError::InvalidManifest);
        }
        let digest = URL_SAFE_NO_PAD
            .decode(&file.sha256)
            .map_err(|_| LifecycleError::InvalidManifest)?;
        if digest.len() != 32 {
            return Err(LifecycleError::InvalidManifest);
        }
        previous = Some(file.path.as_str());
        match file.role {
            ReleaseFileRole::Desktop => desktop += 1,
            ReleaseFileRole::Engine => engine += 1,
            ReleaseFileRole::RuntimeAsset => {}
        }
    }
    if desktop != 1 || engine != 0 {
        return Err(LifecycleError::InvalidManifest);
    }
    Ok(())
}

fn valid_identifier(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(value) if !value.is_empty()))
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
enum UpdateState {
    Preparing,
    Staged,
    ProcessesStopped,
    Activated,
    HealthChecked,
    Cleanup,
    RollingBack,
}

impl UpdateState {
    const ALL: [Self; 7] = [
        Self::Preparing,
        Self::Staged,
        Self::ProcessesStopped,
        Self::Activated,
        Self::HealthChecked,
        Self::Cleanup,
        Self::RollingBack,
    ];
}

fn update_journal_name(state: UpdateState) -> &'static str {
    match state {
        UpdateState::Preparing => "update-0-preparing.json",
        UpdateState::Staged => "update-1-staged.json",
        UpdateState::ProcessesStopped => "update-2-processes-stopped.json",
        UpdateState::Activated => "update-3-activated.json",
        UpdateState::HealthChecked => "update-4-health-checked.json",
        UpdateState::Cleanup => "update-5-cleanup.json",
        UpdateState::RollingBack => "update-6-rolling-back.json",
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateJournal {
    state: UpdateState,
    candidate: ActiveRelease,
    previous: Option<ActiveRelease>,
    #[serde(default)]
    previous_retained: Option<ActiveRelease>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
enum RetainedRollbackState {
    Preparing,
    ProcessesStopped,
    Activated,
    Cleanup,
}

impl RetainedRollbackState {
    const ALL: [Self; 4] = [
        Self::Preparing,
        Self::ProcessesStopped,
        Self::Activated,
        Self::Cleanup,
    ];
}

fn retained_rollback_journal_name(state: RetainedRollbackState) -> &'static str {
    match state {
        RetainedRollbackState::Preparing => "rollback-0-preparing.json",
        RetainedRollbackState::ProcessesStopped => "rollback-1-processes-stopped.json",
        RetainedRollbackState::Activated => "rollback-2-activated.json",
        RetainedRollbackState::Cleanup => "rollback-3-cleanup.json",
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedRollbackJournal {
    state: RetainedRollbackState,
    failed: ActiveRelease,
    target: ActiveRelease,
}

/// Filesystem transaction owner used by the packaging launcher/updater.
pub struct ReleaseInstaller {
    install_root: PathBuf,
    lifecycle_root: PathBuf,
    lifecycle_lock_path: PathBuf,
    verifying_key: VerifyingKey,
    policy: ReleasePolicy,
}

impl ReleaseInstaller {
    /// Creates an updater for one exact absolute installation root.
    ///
    /// # Errors
    /// Rejects broad, relative, or symlinked ownership roots.
    pub fn new(
        install_root: impl Into<PathBuf>,
        verifying_key: VerifyingKey,
        policy: ReleasePolicy,
    ) -> Result<Self, LifecycleError> {
        let install_root = install_root.into();
        validate_owned_root(&install_root)?;
        let parent = install_root
            .parent()
            .ok_or(LifecycleError::InvalidInventory)?;
        let name = install_root
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| valid_identifier(value, 64))
            .ok_or(LifecycleError::InvalidInventory)?;
        Ok(Self {
            lifecycle_root: parent.join(format!(".{name}-lifecycle")),
            lifecycle_lock_path: parent.join(format!(".{name}-lifecycle.lock")),
            install_root,
            verifying_key,
            policy,
        })
    }

    /// Resolves the highest committed active pointer.
    ///
    /// # Errors
    /// Returns an error if a committed pointer cannot be decoded safely.
    pub fn active_release(&self) -> Result<Option<ActiveRelease>, LifecycleError> {
        let directory = self.lifecycle_root.join("active");
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(LifecycleError::JournalCorrupt),
        };
        let mut active = None;
        for entry in entries {
            let entry = entry.map_err(|_| LifecycleError::JournalCorrupt)?;
            let file_name = entry
                .file_name()
                .into_string()
                .map_err(|_| LifecycleError::JournalCorrupt)?;
            if file_name.starts_with('.') && file_name.ends_with(".json.next") {
                continue;
            }
            let metadata = entry
                .file_type()
                .map_err(|_| LifecycleError::JournalCorrupt)?;
            if !metadata.is_file() || metadata.is_symlink() {
                return Err(LifecycleError::JournalCorrupt);
            }
            let candidate: ActiveRelease = read_json(&entry.path())?;
            if !valid_active_release(&candidate) || file_name != pointer_name(&candidate) {
                return Err(LifecycleError::JournalCorrupt);
            }
            if active.as_ref().is_none_or(|current: &ActiveRelease| {
                candidate.install_generation > current.install_generation
            }) {
                active = Some(candidate);
            }
        }
        Ok(active)
    }

    /// Resolves and verifies the single locally retained known-good release.
    ///
    /// The retained release is not selected by remote channel data. It must be
    /// older than the current active release and still pass its signed manifest
    /// and exact inventory verification before a launcher may consider it for
    /// a later rollback decision.
    ///
    /// # Errors
    /// Returns an error when the retained record, signature, inventory, or its
    /// relationship to the active release is invalid.
    pub fn retained_known_good_release(&self) -> Result<Option<ActiveRelease>, LifecycleError> {
        let Some(retained) = self.read_retained_known_good()? else {
            return Ok(None);
        };
        let active = self
            .active_release()?
            .ok_or(LifecycleError::JournalCorrupt)?;
        if retained.install_generation >= active.install_generation || retained == active {
            return Err(LifecycleError::JournalCorrupt);
        }
        let mut policy = self.policy.clone();
        policy.minimum_install_generation = 0;
        self.audit_release(&retained, &policy)?;
        Ok(Some(retained))
    }

    /// Atomically re-selects the locally retained, verified known-good release.
    ///
    /// The rollback target cannot be supplied by channel data or the caller: it
    /// is resolved only from the bounded local known-good record written by a
    /// previously successful update. Both its signed manifest and exact file
    /// inventory are reverified before any active pointer changes. The failed
    /// newer release is removed after the older release becomes active.
    ///
    /// # Errors
    /// Returns an error if no verified retained release exists, another
    /// lifecycle transaction is pending, the active process cannot stop, or
    /// the journaled pointer/cleanup transaction cannot complete safely.
    pub fn rollback_to_retained_known_good<H: LifecycleHooks>(
        &self,
        hooks: &H,
    ) -> Result<ActiveRelease, LifecycleError> {
        let _lock = LifecycleLock::acquire(&self.lifecycle_lock_path)?;
        if self.lifecycle_root.join("uninstall.json").exists() {
            return Err(LifecycleError::UninstallPendingCleanup);
        }
        if self.read_update_journal()?.is_some() || self.read_retained_rollback_journal()?.is_some()
        {
            return Err(LifecycleError::UpdatePendingCleanup);
        }
        let failed = self
            .audit_active_release()?
            .ok_or(LifecycleError::RollbackFailed)?;
        let target = self
            .retained_known_good_release()?
            .ok_or(LifecycleError::RollbackFailed)?;
        self.verify_retained_rollback_compatibility(&failed, &target)?;
        let journal = RetainedRollbackJournal {
            state: RetainedRollbackState::Preparing,
            failed,
            target: target.clone(),
        };
        self.write_retained_rollback_journal(&journal)?;
        self.resume_retained_rollback(journal, hooks)?;
        Ok(target)
    }

    /// Returns the authenticated signed manifest for the currently owned active
    /// or retained known-good release after re-running its exact local audit.
    ///
    /// This does not accept arbitrary version directories: the supplied release
    /// must be one of the lifecycle owner's bounded current records.
    ///
    /// # Errors
    /// Returns a lifecycle verification error if the release is not currently
    /// owned, or if its signature, identity, platform policy, or exact local
    /// inventory no longer verifies.
    pub fn verified_release_manifest(
        &self,
        release: &ActiveRelease,
    ) -> Result<SignedReleaseManifest, LifecycleError> {
        let active = self.audit_active_release()?;
        if active.as_ref() == Some(release) {
            return self.read_verified_release_manifest(release, &self.policy);
        }
        if self.retained_known_good_release()?.as_ref() == Some(release) {
            let mut policy = self.policy.clone();
            policy.minimum_install_generation = 0;
            return self.read_verified_release_manifest(release, &policy);
        }
        Err(LifecycleError::VerificationFailed)
    }

    /// Verifies the active pointer, signed manifest, complete file inventory,
    /// platform policy, and executable permissions before normal launch.
    ///
    /// # Errors
    /// Rejects any missing, extra, modified, symlinked, or incompatible artifact.
    pub fn audit_active_release(&self) -> Result<Option<ActiveRelease>, LifecycleError> {
        let Some(active) = self.active_release()? else {
            if self.read_retained_known_good()?.is_some() {
                return Err(LifecycleError::JournalCorrupt);
            }
            return Ok(None);
        };
        self.audit_release(&active, &self.policy)?;
        let retained = self.retained_known_good_release()?;
        let expected_release_count = usize::from(retained.is_some()).saturating_add(1);
        if count_entries(&self.lifecycle_root.join("active"))? != 1
            || count_entries(&self.lifecycle_root.join("manifests"))? != expected_release_count
            || count_entries(&self.install_root.join("versions"))? != expected_release_count
        {
            return Err(LifecycleError::UpdatePendingCleanup);
        }
        Ok(Some(active))
    }

    /// Resolves one validated release to its exact version directory.
    ///
    /// # Errors
    /// Rejects release identities which did not originate from a valid pointer.
    pub fn release_directory(&self, release: &ActiveRelease) -> Result<PathBuf, LifecycleError> {
        self.version_path(release)
    }

    /// Stages, verifies, activates, health-checks, and cleans one release.
    ///
    /// `bundle_root` is a packaging-owned download directory and is never used
    /// as a deletion root by this API.
    ///
    /// # Errors
    /// Current activation remains selected for every pre-activation failure;
    /// post-activation health failure rolls back before returning.
    pub fn install<H: LifecycleHooks>(
        &self,
        signed: &SignedReleaseManifest,
        bundle_root: &Path,
        hooks: &H,
    ) -> Result<UpdateOutcome, LifecycleError> {
        let _lock = LifecycleLock::acquire(&self.lifecycle_lock_path)?;
        if self.lifecycle_root.join("uninstall.json").exists() {
            return Err(LifecycleError::UninstallPendingCleanup);
        }
        let previous = self.audit_active_release()?;
        let previous_retained = self.retained_known_good_release()?;
        let mut policy = self.policy.clone();
        if let Some(active) = &previous {
            policy.minimum_install_generation = policy
                .minimum_install_generation
                .max(active.install_generation.saturating_add(1));
        }
        verify_release_manifest(signed, &self.verifying_key, &policy)?;
        let directory_name = format!(
            "{:020}-{}",
            signed.manifest.install_generation, signed.manifest.release_identity
        );
        let candidate = ActiveRelease {
            release_identity: signed.manifest.release_identity.clone(),
            install_generation: signed.manifest.install_generation,
            directory_name,
        };
        let candidate_root = self.version_path(&candidate)?;
        if candidate_root.exists() {
            return Err(LifecycleError::StagingFailed);
        }
        let mut journal = UpdateJournal {
            state: UpdateState::Preparing,
            candidate: candidate.clone(),
            previous: previous.clone(),
            previous_retained: previous_retained.clone(),
        };
        self.write_update_journal(&journal)?;
        if let Err(error) = Self::stage(signed, bundle_root, &candidate_root) {
            let _ = remove_owned_path(&candidate_root);
            let _ = self.remove_update_journal();
            return Err(error);
        }
        journal.state = UpdateState::Staged;
        self.write_update_journal(&journal)?;
        hooks
            .prepare_activation(previous.as_ref())
            .map_err(|_| LifecycleError::ShutdownFailed)?;
        journal.state = UpdateState::ProcessesStopped;
        self.write_update_journal(&journal)?;
        self.commit_manifest(signed, &candidate)?;
        if let Err(error) = self.audit_release(&candidate, &policy) {
            if self.remove_manifest(&candidate).is_err()
                || remove_owned_path(&candidate_root).is_err()
                || self.remove_release_record_temporaries(&candidate).is_err()
                || self.remove_update_journal().is_err()
            {
                return Err(LifecycleError::UpdatePendingCleanup);
            }
            return Err(error);
        }
        self.commit_pointer(&candidate)?;
        journal.state = UpdateState::Activated;
        self.write_update_journal(&journal)?;
        if hooks.health_check(&candidate).is_err() {
            journal.state = UpdateState::RollingBack;
            self.write_update_journal(&journal)?;
            self.restore_previous_after_candidate_failure(&journal)?;
            self.remove_update_journal()?;
            return Err(LifecycleError::HealthCheckFailed);
        }
        journal.state = UpdateState::HealthChecked;
        self.write_update_journal(&journal)?;
        journal.state = UpdateState::Cleanup;
        self.write_update_journal(&journal)?;
        self.finish_successful_update(&journal)?;
        self.audit_single_active(&candidate)?;
        self.remove_update_journal()?;
        Ok(UpdateOutcome {
            active: candidate,
            removed_release: previous_retained,
        })
    }

    /// Resumes or rolls back an interrupted transaction deterministically.
    ///
    /// # Errors
    /// Returns a pending-cleanup category until exact owned artifacts are gone.
    pub fn recover<H: LifecycleHooks>(&self, hooks: &H) -> Result<(), LifecycleError> {
        let _lock = LifecycleLock::acquire(&self.lifecycle_lock_path)?;
        if self.lifecycle_root.join("uninstall.json").exists() {
            return Err(LifecycleError::UninstallPendingCleanup);
        }
        let update_journal = self.read_update_journal()?;
        let rollback_journal = self.read_retained_rollback_journal()?;
        if update_journal.is_some() && rollback_journal.is_some() {
            return Err(LifecycleError::JournalCorrupt);
        }
        if let Some(journal) = rollback_journal {
            return self.resume_retained_rollback(journal, hooks);
        }
        let Some(journal) = update_journal else {
            return Ok(());
        };
        let candidate_root = self.version_path(&journal.candidate)?;
        match journal.state {
            UpdateState::Preparing | UpdateState::Staged | UpdateState::ProcessesStopped => {
                remove_owned_path(&candidate_root)
                    .map_err(|_| LifecycleError::UpdatePendingCleanup)?;
                self.remove_pointer(&journal.candidate)?;
                self.remove_manifest(&journal.candidate)?;
                self.remove_release_record_temporaries(&journal.candidate)?;
            }
            UpdateState::Activated => {
                // Activation may have been interrupted after the candidate was
                // audited but before its health result became durable. Re-run
                // the exact signed inventory audit before recovery executes any
                // candidate code. If the candidate changed, stop with the
                // journal intact: it may already have executed before the crash,
                // so automatically downgrading persisted state is not provably safe.
                self.audit_release(&journal.candidate, &self.policy)?;
                if hooks.health_check(&journal.candidate).is_err() {
                    let mut journal = journal;
                    journal.state = UpdateState::RollingBack;
                    self.write_update_journal(&journal)?;
                    self.restore_previous_after_candidate_failure(&journal)?;
                } else {
                    let mut journal = journal;
                    journal.state = UpdateState::HealthChecked;
                    self.write_update_journal(&journal)?;
                    journal.state = UpdateState::Cleanup;
                    self.write_update_journal(&journal)?;
                    self.finish_successful_update(&journal)?;
                    self.audit_single_active(&journal.candidate)?;
                    self.remove_release_record_temporaries(&journal.candidate)?;
                }
            }
            UpdateState::HealthChecked => {
                let mut journal = journal;
                journal.state = UpdateState::Cleanup;
                self.write_update_journal(&journal)?;
                self.finish_successful_update(&journal)?;
                self.audit_single_active(&journal.candidate)?;
                self.remove_release_record_temporaries(&journal.candidate)?;
            }
            UpdateState::Cleanup => {
                self.finish_successful_update(&journal)?;
                self.audit_single_active(&journal.candidate)?;
                self.remove_release_record_temporaries(&journal.candidate)?;
            }
            UpdateState::RollingBack => {
                self.restore_previous_after_candidate_failure(&journal)?;
                self.remove_release_record_temporaries(&journal.candidate)?;
            }
        }
        self.remove_update_journal()
    }

    /// Removes every inventoried local artifact and credential, then audits absence.
    ///
    /// # Errors
    /// Leaves a resumable journal until all exact owned targets are absent.
    pub fn uninstall<H: LifecycleHooks>(
        &self,
        inventory: &InstallationInventory,
        hooks: &H,
    ) -> Result<UninstallOutcome, LifecycleError> {
        self.validate_inventory(inventory)?;
        let lock = LifecycleLock::acquire(&self.lifecycle_lock_path)?;
        let uninstall_journal = self.lifecycle_root.join("uninstall.json");
        if !uninstall_journal.exists() {
            write_json_atomic(
                &self.lifecycle_root,
                "uninstall.json",
                &UninstallJournal { started: true },
            )?;
        }
        hooks
            .disable_registrations(&inventory.registrations)
            .map_err(|_| LifecycleError::UninstallPendingCleanup)?;
        hooks
            .stop_owned_processes()
            .map_err(|_| LifecycleError::UninstallPendingCleanup)?;
        for entry in &inventory.vault_entries {
            hooks
                .delete_vault_entry(entry)
                .map_err(|_| LifecycleError::UninstallPendingCleanup)?;
        }
        let roots = inventory_roots(inventory);
        for root in &roots {
            remove_owned_path(root).map_err(|_| LifecycleError::UninstallPendingCleanup)?;
        }
        hooks
            .audit_external_absence(inventory)
            .map_err(|_| LifecycleError::ExternalArtifactRemaining)?;
        if roots.iter().any(|path| path.exists()) {
            return Err(LifecycleError::UninstallPendingCleanup);
        }
        remove_lifecycle_root_after_uninstall(&self.lifecycle_root)
            .map_err(|_| LifecycleError::UninstallPendingCleanup)?;
        lock.release()?;
        Ok(UninstallOutcome {
            removed_roots: roots.len().saturating_add(1),
            removed_vault_keys: inventory.vault_entries.len(),
        })
    }

    fn stage(
        signed: &SignedReleaseManifest,
        bundle_root: &Path,
        candidate_root: &Path,
    ) -> Result<(), LifecycleError> {
        fs::create_dir_all(candidate_root).map_err(|_| LifecycleError::StagingFailed)?;
        for expected in &signed.manifest.files {
            let relative = Path::new(&expected.path);
            let source = safe_bundle_file(bundle_root, relative)?;
            let destination = candidate_root.join(relative);
            let parent = destination.parent().ok_or(LifecycleError::StagingFailed)?;
            fs::create_dir_all(parent).map_err(|_| LifecycleError::StagingFailed)?;
            copy_new_file(&source, &destination)?;
            verify_release_file(&destination, expected)?;
            #[cfg(unix)]
            set_executable(&destination, expected.executable)?;
            #[cfg(not(unix))]
            set_executable(&destination, expected.executable);
        }
        #[cfg(unix)]
        sync_directory(candidate_root)?;
        #[cfg(not(unix))]
        sync_directory(candidate_root);
        Ok(())
    }

    fn version_path(&self, release: &ActiveRelease) -> Result<PathBuf, LifecycleError> {
        if !valid_active_release(release) {
            return Err(LifecycleError::JournalCorrupt);
        }
        Ok(self
            .install_root
            .join("versions")
            .join(&release.directory_name))
    }

    fn commit_pointer(&self, release: &ActiveRelease) -> Result<PathBuf, LifecycleError> {
        let active_root = self.lifecycle_root.join("active");
        fs::create_dir_all(&active_root).map_err(|_| LifecycleError::StagingFailed)?;
        let name = pointer_name(release);
        write_json_atomic(&active_root, &name, release)?;
        Ok(active_root.join(name))
    }

    fn commit_manifest(
        &self,
        signed: &SignedReleaseManifest,
        release: &ActiveRelease,
    ) -> Result<(), LifecycleError> {
        write_json_atomic(
            &self.lifecycle_root.join("manifests"),
            &pointer_name(release),
            signed,
        )
    }

    fn audit_release(
        &self,
        release: &ActiveRelease,
        policy: &ReleasePolicy,
    ) -> Result<(), LifecycleError> {
        self.read_verified_release_manifest(release, policy)
            .map(|_| ())
    }

    fn read_verified_release_manifest(
        &self,
        release: &ActiveRelease,
        policy: &ReleasePolicy,
    ) -> Result<SignedReleaseManifest, LifecycleError> {
        let signed: SignedReleaseManifest = read_json(&self.manifest_path(release))?;
        verify_release_manifest(&signed, &self.verifying_key, policy)?;
        if signed.manifest.release_identity != release.release_identity
            || signed.manifest.install_generation != release.install_generation
        {
            return Err(LifecycleError::VerificationFailed);
        }
        let root = self.version_path(release)?;
        verify_candidate_inventory(&root, &signed.manifest.files)?;
        Ok(signed)
    }

    fn rollback_compatibility_metadata(
        &self,
        release: &ActiveRelease,
    ) -> Result<RollbackCompatibilityMetadata, LifecycleError> {
        let mut policy = self.policy.clone();
        policy.minimum_install_generation = 0;
        let signed = self.read_verified_release_manifest(release, &policy)?;
        let Some(expected) = signed.manifest.files.iter().find(|file| {
            file.role == ReleaseFileRole::RuntimeAsset
                && file.path == ROLLBACK_COMPATIBILITY_FILENAME
        }) else {
            return Err(LifecycleError::RollbackFailed);
        };
        let path = self
            .version_path(release)?
            .join(ROLLBACK_COMPATIBILITY_FILENAME);
        let metadata = fs::symlink_metadata(&path).map_err(|_| LifecycleError::RollbackFailed)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() == 0
            || metadata.len() > MAXIMUM_ROLLBACK_COMPATIBILITY_BYTES
            || metadata.len() != expected.size
        {
            return Err(LifecycleError::RollbackFailed);
        }
        let bytes = fs::read(&path).map_err(|_| LifecycleError::RollbackFailed)?;
        if URL_SAFE_NO_PAD.encode(Sha256::digest(&bytes)) != expected.sha256 {
            return Err(LifecycleError::RollbackFailed);
        }
        let compatibility: RollbackCompatibilityMetadata =
            serde_json::from_slice(&bytes).map_err(|_| LifecycleError::RollbackFailed)?;
        if compatibility.schema_version != ROLLBACK_COMPATIBILITY_SCHEMA_VERSION
            || compatibility.state_compatibility_epoch == 0
        {
            return Err(LifecycleError::RollbackFailed);
        }
        Ok(compatibility)
    }

    fn verify_retained_rollback_compatibility(
        &self,
        failed: &ActiveRelease,
        target: &ActiveRelease,
    ) -> Result<(), LifecycleError> {
        let failed = self.rollback_compatibility_metadata(failed)?;
        let target = self.rollback_compatibility_metadata(target)?;
        if failed.state_compatibility_epoch != target.state_compatibility_epoch {
            return Err(LifecycleError::RollbackFailed);
        }
        Ok(())
    }

    fn remove_release_record_temporaries(
        &self,
        release: &ActiveRelease,
    ) -> Result<(), LifecycleError> {
        let name = pointer_name(release);
        for directory in [
            self.lifecycle_root.join("active"),
            self.lifecycle_root.join("manifests"),
        ] {
            remove_file_if_present(&directory.join(format!(".{name}.next")))
                .map_err(|_| LifecycleError::UpdatePendingCleanup)?;
        }
        Ok(())
    }

    fn retained_known_good_path(&self) -> PathBuf {
        self.lifecycle_root.join("known-good.json")
    }

    fn read_retained_known_good(&self) -> Result<Option<ActiveRelease>, LifecycleError> {
        let path = self.retained_known_good_path();
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                let release: ActiveRelease = read_json(&path)?;
                if !valid_active_release(&release) {
                    return Err(LifecycleError::JournalCorrupt);
                }
                Ok(Some(release))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Ok(_) | Err(_) => Err(LifecycleError::JournalCorrupt),
        }
    }

    fn commit_retained_known_good(&self, release: &ActiveRelease) -> Result<(), LifecycleError> {
        if !valid_active_release(release) {
            return Err(LifecycleError::JournalCorrupt);
        }
        write_json_atomic(&self.lifecycle_root, "known-good.json", release)
    }

    fn remove_retained_known_good(&self) -> Result<(), LifecycleError> {
        remove_file_if_present(&self.retained_known_good_path())
            .map_err(|_| LifecycleError::UpdatePendingCleanup)
    }

    fn finish_successful_update(&self, journal: &UpdateJournal) -> Result<(), LifecycleError> {
        if let Some(previous) = &journal.previous {
            self.commit_retained_known_good(previous)?;
            self.remove_pointer(previous)?;
        } else {
            self.remove_retained_known_good()?;
        }
        if let Some(old_retained) = &journal.previous_retained
            && journal.previous.as_ref() != Some(old_retained)
        {
            remove_owned_path(&self.version_path(old_retained)?)
                .map_err(|_| LifecycleError::UpdatePendingCleanup)?;
            self.remove_manifest(old_retained)?;
        }
        Ok(())
    }

    fn restore_previous_after_candidate_failure(
        &self,
        journal: &UpdateJournal,
    ) -> Result<(), LifecycleError> {
        let mut rollback_policy = self.policy.clone();
        rollback_policy.minimum_install_generation = 0;
        if let Some(previous) = &journal.previous {
            // Candidate code has already executed by the time a health check
            // can fail. Never select an older binary after that point unless
            // both signed releases declare the same persisted-state epoch.
            self.verify_retained_rollback_compatibility(&journal.candidate, previous)?;
            self.audit_release(previous, &rollback_policy)?;
            self.commit_pointer(previous)
                .map_err(|_| LifecycleError::RollbackFailed)?;
        }

        let retained_to_restore =
            journal.previous_retained.as_ref().and_then(|retained| {
                if journal.previous.as_ref().is_some_and(|previous| {
                    retained.install_generation >= previous.install_generation
                }) {
                    return None;
                }
                self.audit_release(retained, &rollback_policy)
                    .ok()
                    .map(|()| retained)
            });

        self.remove_pointer(&journal.candidate)
            .map_err(|_| LifecycleError::RollbackFailed)?;
        self.remove_manifest(&journal.candidate)
            .map_err(|_| LifecycleError::RollbackFailed)?;
        remove_owned_path(&self.version_path(&journal.candidate)?)
            .map_err(|_| LifecycleError::RollbackFailed)?;

        if let Some(retained) = retained_to_restore {
            self.commit_retained_known_good(retained)
                .map_err(|_| LifecycleError::RollbackFailed)?;
        } else {
            self.remove_retained_known_good()
                .map_err(|_| LifecycleError::RollbackFailed)?;
            if let Some(retained) = &journal.previous_retained {
                remove_owned_path(&self.version_path(retained)?)
                    .map_err(|_| LifecycleError::RollbackFailed)?;
                self.remove_manifest(retained)
                    .map_err(|_| LifecycleError::RollbackFailed)?;
            }
        }

        if let Some(previous) = &journal.previous {
            self.audit_single_active(previous)?;
        } else if self.active_release()?.is_some() {
            return Err(LifecycleError::RollbackFailed);
        }
        Ok(())
    }

    fn write_retained_rollback_journal(
        &self,
        journal: &RetainedRollbackJournal,
    ) -> Result<(), LifecycleError> {
        let name = retained_rollback_journal_name(journal.state);
        write_json_atomic(&self.lifecycle_root, name, journal)?;
        for state in RetainedRollbackState::ALL {
            if state != journal.state {
                remove_file_if_present(
                    &self
                        .lifecycle_root
                        .join(retained_rollback_journal_name(state)),
                )
                .map_err(|_| LifecycleError::UpdatePendingCleanup)?;
            }
        }
        Ok(())
    }

    fn read_retained_rollback_journal(
        &self,
    ) -> Result<Option<RetainedRollbackJournal>, LifecycleError> {
        for state in RetainedRollbackState::ALL.into_iter().rev() {
            let path = self
                .lifecycle_root
                .join(retained_rollback_journal_name(state));
            if path.exists() {
                let journal: RetainedRollbackJournal = read_json(&path)?;
                if journal.state != state
                    || !valid_active_release(&journal.failed)
                    || !valid_active_release(&journal.target)
                    || journal.target.install_generation >= journal.failed.install_generation
                {
                    return Err(LifecycleError::JournalCorrupt);
                }
                return Ok(Some(journal));
            }
        }
        Ok(None)
    }

    fn remove_retained_rollback_journal(&self) -> Result<(), LifecycleError> {
        for state in RetainedRollbackState::ALL {
            remove_file_if_present(
                &self
                    .lifecycle_root
                    .join(retained_rollback_journal_name(state)),
            )
            .map_err(|_| LifecycleError::UpdatePendingCleanup)?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn retained_rollback_journal_exists(&self) -> bool {
        RetainedRollbackState::ALL.into_iter().any(|state| {
            self.lifecycle_root
                .join(retained_rollback_journal_name(state))
                .exists()
        })
    }

    fn resume_retained_rollback<H: LifecycleHooks>(
        &self,
        mut journal: RetainedRollbackJournal,
        hooks: &H,
    ) -> Result<(), LifecycleError> {
        let retained_marker = self.read_retained_known_good()?;
        match journal.state {
            RetainedRollbackState::Preparing
            | RetainedRollbackState::ProcessesStopped
            | RetainedRollbackState::Activated
                if retained_marker.as_ref() != Some(&journal.target) =>
            {
                return Err(LifecycleError::JournalCorrupt);
            }
            RetainedRollbackState::Cleanup
                if retained_marker
                    .as_ref()
                    .is_some_and(|retained| retained != &journal.target) =>
            {
                return Err(LifecycleError::JournalCorrupt);
            }
            _ => {}
        }
        let mut rollback_policy = self.policy.clone();
        rollback_policy.minimum_install_generation = 0;
        self.audit_release(&journal.target, &rollback_policy)?;
        if journal.state != RetainedRollbackState::Cleanup {
            self.audit_release(&journal.failed, &rollback_policy)?;
            self.verify_retained_rollback_compatibility(&journal.failed, &journal.target)?;
        }

        if journal.state == RetainedRollbackState::Preparing {
            hooks
                .prepare_activation(Some(&journal.failed))
                .map_err(|_| LifecycleError::ShutdownFailed)?;
            journal.state = RetainedRollbackState::ProcessesStopped;
            self.write_retained_rollback_journal(&journal)?;
        }
        if journal.state == RetainedRollbackState::ProcessesStopped {
            let selected = self.active_release()?;
            if selected.as_ref() != Some(&journal.failed)
                && selected.as_ref() != Some(&journal.target)
            {
                return Err(LifecycleError::JournalCorrupt);
            }
            self.commit_pointer(&journal.target)?;
            self.remove_pointer(&journal.failed)
                .map_err(|_| LifecycleError::RollbackFailed)?;
            journal.state = RetainedRollbackState::Activated;
            self.write_retained_rollback_journal(&journal)?;
        }
        if journal.state == RetainedRollbackState::Activated {
            if self.active_release()?.as_ref() != Some(&journal.target) {
                return Err(LifecycleError::JournalCorrupt);
            }
            journal.state = RetainedRollbackState::Cleanup;
            self.write_retained_rollback_journal(&journal)?;
        }
        if journal.state == RetainedRollbackState::Cleanup {
            if self.active_release()?.as_ref() != Some(&journal.target) {
                return Err(LifecycleError::JournalCorrupt);
            }
            self.remove_retained_known_good()?;
            remove_owned_path(&self.version_path(&journal.failed)?)
                .map_err(|_| LifecycleError::UpdatePendingCleanup)?;
            self.remove_manifest(&journal.failed)?;
            self.audit_single_active(&journal.target)?;
            self.remove_release_record_temporaries(&journal.failed)?;
            self.remove_release_record_temporaries(&journal.target)?;
            self.remove_retained_rollback_journal()?;
        }
        Ok(())
    }

    fn remove_pointer(&self, release: &ActiveRelease) -> Result<(), LifecycleError> {
        remove_file_if_present(
            &self
                .lifecycle_root
                .join("active")
                .join(pointer_name(release)),
        )
        .map_err(|_| LifecycleError::UpdatePendingCleanup)
    }

    fn manifest_path(&self, release: &ActiveRelease) -> PathBuf {
        self.lifecycle_root
            .join("manifests")
            .join(pointer_name(release))
    }

    fn remove_manifest(&self, release: &ActiveRelease) -> Result<(), LifecycleError> {
        remove_file_if_present(&self.manifest_path(release))
            .map_err(|_| LifecycleError::UpdatePendingCleanup)
    }

    fn write_update_journal(&self, journal: &UpdateJournal) -> Result<(), LifecycleError> {
        let name = update_journal_name(journal.state);
        write_json_atomic(&self.lifecycle_root, name, journal)?;
        for state in UpdateState::ALL {
            if state != journal.state {
                remove_file_if_present(&self.lifecycle_root.join(update_journal_name(state)))
                    .map_err(|_| LifecycleError::UpdatePendingCleanup)?;
            }
        }
        Ok(())
    }

    fn read_update_journal(&self) -> Result<Option<UpdateJournal>, LifecycleError> {
        for state in UpdateState::ALL.into_iter().rev() {
            let path = self.lifecycle_root.join(update_journal_name(state));
            if path.exists() {
                let journal: UpdateJournal = read_json(&path)?;
                if journal.state != state {
                    return Err(LifecycleError::JournalCorrupt);
                }
                return Ok(Some(journal));
            }
        }
        Ok(None)
    }

    #[cfg(test)]
    fn update_journal_exists(&self) -> bool {
        UpdateState::ALL.into_iter().any(|state| {
            self.lifecycle_root
                .join(update_journal_name(state))
                .exists()
        })
    }

    fn remove_update_journal(&self) -> Result<(), LifecycleError> {
        for state in UpdateState::ALL {
            remove_file_if_present(&self.lifecycle_root.join(update_journal_name(state)))
                .map_err(|_| LifecycleError::UpdatePendingCleanup)?;
        }
        Ok(())
    }

    fn audit_single_active(&self, expected: &ActiveRelease) -> Result<(), LifecycleError> {
        if self.active_release()?.as_ref() != Some(expected) {
            return Err(LifecycleError::UpdatePendingCleanup);
        }
        let active_root = self.lifecycle_root.join("active");
        if count_entries(&active_root)? != 1 {
            return Err(LifecycleError::UpdatePendingCleanup);
        }
        let retained = self.retained_known_good_release()?;
        let expected_release_count = usize::from(retained.is_some()).saturating_add(1);
        if count_entries(&self.lifecycle_root.join("manifests"))? != expected_release_count {
            return Err(LifecycleError::UpdatePendingCleanup);
        }
        let versions = self.install_root.join("versions");
        if count_entries(&versions)? != expected_release_count
            || !self.version_path(expected)?.is_dir()
        {
            return Err(LifecycleError::UpdatePendingCleanup);
        }
        self.audit_active_release().map(|_| ())
    }

    fn validate_inventory(&self, inventory: &InstallationInventory) -> Result<(), LifecycleError> {
        if inventory.schema_version != INVENTORY_SCHEMA_VERSION
            || inventory.install_root != self.install_root
            || inventory.vault_entries.len() > MAXIMUM_VAULT_KEYS
            || inventory.vault_entries.iter().any(|entry| {
                !valid_identifier(&entry.service, 128) || !valid_identifier(&entry.key, 128)
            })
        {
            return Err(LifecycleError::InvalidInventory);
        }
        let roots = inventory_roots(inventory);
        if roots.is_empty() || roots.len() > MAXIMUM_OWNED_ROOTS {
            return Err(LifecycleError::InvalidInventory);
        }
        for root in roots {
            validate_owned_root(&root)?;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct UninstallJournal {
    started: bool,
}

fn valid_active_release(release: &ActiveRelease) -> bool {
    release.install_generation > 0
        && valid_identifier(&release.release_identity, 128)
        && release.directory_name
            == format!(
                "{:020}-{}",
                release.install_generation, release.release_identity
            )
}

fn pointer_name(release: &ActiveRelease) -> String {
    format!(
        "{:020}-{}.json",
        release.install_generation, release.release_identity
    )
}

fn inventory_roots(inventory: &InstallationInventory) -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    roots.insert(inventory.install_root.clone());
    roots.extend(inventory.data_roots.iter().cloned());
    roots.extend(inventory.cache_roots.iter().cloned());
    roots.extend(inventory.log_roots.iter().cloned());
    roots.extend(inventory.legacy_cleanup_roots.iter().cloned());
    let mut roots = roots.into_iter().collect::<Vec<_>>();
    roots.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    roots
}

/// Resolves the one native Axiusflow data root used by the desktop runtime.
///
/// # Errors
/// Returns an error when the current user's native data directory is unavailable.
pub fn native_data_root() -> Result<PathBuf, LifecycleError> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|root| root.join("Axiusflow"))
            .ok_or(LifecycleError::InvalidInventory)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|root| root.join("Library/Application Support/Axiusflow"))
            .ok_or(LifecycleError::InvalidInventory)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|root| PathBuf::from(root).join(".local/share"))
            })
            .map(|root| root.join("axiusflow"))
            .ok_or(LifecycleError::InvalidInventory)
    }
}

/// Resolves the stable per-user Axiusflow installation root used by the
/// website bootstrap and the persisted launcher. It intentionally requires no
/// administrator-owned system directory.
///
/// # Errors
/// Returns an error when the current user's native home directory is unavailable.
pub fn native_install_root() -> Result<PathBuf, LifecycleError> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|root| root.join("Programs/Axiusflow"))
            .ok_or(LifecycleError::InvalidInventory)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|root| root.join("Applications/Axiusflow"))
            .ok_or(LifecycleError::InvalidInventory)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|root| PathBuf::from(root).join(".local/share"))
            })
            .map(|root| root.join("axiusflow/app"))
            .ok_or(LifecycleError::InvalidInventory)
    }
}

/// Builds the versioned inventory for all currently owned native artifacts.
///
/// # Errors
/// Returns an error when an exact native ownership root cannot be resolved.
pub fn native_installation_inventory(
    install_root: impl Into<PathBuf>,
) -> Result<InstallationInventory, LifecycleError> {
    let install_root = install_root.into();
    validate_owned_root(&install_root)?;
    let data = native_data_root()?;
    let cache = {
        #[cfg(target_os = "windows")]
        {
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .map(|root| root.join("Axiusflow/cache"))
                .ok_or(LifecycleError::InvalidInventory)?
        }
        #[cfg(target_os = "macos")]
        {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|root| root.join("Library/Caches/Axiusflow"))
                .ok_or(LifecycleError::InvalidInventory)?
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|root| PathBuf::from(root).join(".cache")))
                .map(|root| root.join("axiusflow"))
                .ok_or(LifecycleError::InvalidInventory)?
        }
    };
    let logs = data.join("logs");
    let registrations = vec!["start-menu:Axiusflow".to_string()];
    Ok(InstallationInventory {
        schema_version: INVENTORY_SCHEMA_VERSION,
        install_root,
        data_roots: vec![data],
        cache_roots: vec![cache],
        log_roots: vec![logs],
        legacy_cleanup_roots: Vec::new(),
        vault_entries: vec![
            VaultEntry {
                service: "com.axiusflow.terminal".to_string(),
                key: "provider-rithmic-test-default-v1".to_string(),
            },
            VaultEntry {
                service: "com.axiusflow.account".to_string(),
                key: "account-refresh-default-v1".to_string(),
            },
            VaultEntry {
                service: "com.axiusflow.account".to_string(),
                key: "account-entitlement-lease-v1".to_string(),
            },
            VaultEntry {
                service: "com.axiusflow.account".to_string(),
                key: "account-entitlement-directory-v1".to_string(),
            },
            VaultEntry {
                service: "com.axiusflow.account".to_string(),
                key: "account-device-key-v1".to_string(),
            },
            VaultEntry {
                service: "com.axiusflow.account".to_string(),
                key: "account-profile-v1".to_string(),
            },
        ],
        registrations,
    })
}

fn validate_owned_root(path: &Path) -> Result<(), LifecycleError> {
    if !path.is_absolute()
        || path.components().count() < 3
        || path.parent().is_none()
        || path == Path::new("/")
        || broad_native_root(path)
    {
        return Err(LifecycleError::InvalidInventory);
    }
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(LifecycleError::InvalidInventory);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(LifecycleError::InvalidInventory),
        }
    }
    Ok(())
}

fn broad_native_root(path: &Path) -> bool {
    if path == std::env::temp_dir() {
        return true;
    }
    for variable in [
        "HOME",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
    ] {
        if std::env::var_os(variable).is_some_and(|root| path.as_os_str() == root) {
            return true;
        }
    }
    std::env::var_os("HOME").is_some_and(|home| {
        let home = PathBuf::from(home);
        [
            home.join(".local/share"),
            home.join(".cache"),
            home.join("Library/Application Support"),
            home.join("Library/Caches"),
        ]
        .iter()
        .any(|root| path == root)
    })
}

fn safe_bundle_file(bundle_root: &Path, relative: &Path) -> Result<PathBuf, LifecycleError> {
    if !bundle_root.is_absolute() || !safe_relative_path(relative) {
        return Err(LifecycleError::StagingFailed);
    }
    let mut current = bundle_root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(LifecycleError::StagingFailed);
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|_| LifecycleError::StagingFailed)?;
        if metadata.file_type().is_symlink() {
            return Err(LifecycleError::StagingFailed);
        }
    }
    if !current.is_file() {
        return Err(LifecycleError::StagingFailed);
    }
    Ok(current)
}

fn copy_new_file(source: &Path, destination: &Path) -> Result<(), LifecycleError> {
    let mut input = File::open(source).map_err(|_| LifecycleError::StagingFailed)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|_| LifecycleError::StagingFailed)?;
    std::io::copy(&mut input, &mut output).map_err(|_| LifecycleError::StagingFailed)?;
    output.sync_all().map_err(|_| LifecycleError::StagingFailed)
}

/// Verifies one downloaded release file against its signed size and SHA-256.
///
/// # Errors
/// Returns [`LifecycleError::VerificationFailed`] when the file is absent,
/// symlinked, has the wrong size, cannot be read, or does not match its digest.
pub fn verify_release_file(path: &Path, expected: &ReleaseFile) -> Result<(), LifecycleError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| LifecycleError::VerificationFailed)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != expected.size {
        return Err(LifecycleError::VerificationFailed);
    }
    let mut file = File::open(path).map_err(|_| LifecycleError::VerificationFailed)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| LifecycleError::VerificationFailed)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    if URL_SAFE_NO_PAD.encode(digest.finalize()) != expected.sha256 {
        return Err(LifecycleError::VerificationFailed);
    }
    Ok(())
}

fn verify_candidate_inventory(root: &Path, expected: &[ReleaseFile]) -> Result<(), LifecycleError> {
    verify_candidate_inventory_with(root, expected, verify_installed_executable_authenticode)
}

fn verify_candidate_inventory_with<F>(
    root: &Path,
    expected: &[ReleaseFile],
    mut verify_native_executable: F,
) -> Result<(), LifecycleError>
where
    F: FnMut(&Path, &ReleaseFile) -> Result<(), LifecycleError>,
{
    let mut actual = BTreeSet::new();
    collect_relative_files(root, root, &mut actual)?;
    let declared = expected
        .iter()
        .map(|file| PathBuf::from(&file.path))
        .collect::<BTreeSet<_>>();
    if actual != declared {
        return Err(LifecycleError::VerificationFailed);
    }
    for file in expected {
        let path = root.join(&file.path);
        verify_release_file(&path, file)?;
        #[cfg(unix)]
        verify_executable(&path, file.executable)?;
        #[cfg(not(unix))]
        verify_executable(&path, file.executable);
        verify_native_executable(&path, file)?;
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn verify_installed_executable_authenticode(
    _path: &Path,
    _expected: &ReleaseFile,
) -> Result<(), LifecycleError> {
    Ok(())
}

#[cfg(target_os = "windows")]
fn verify_installed_executable_authenticode(
    path: &Path,
    expected: &ReleaseFile,
) -> Result<(), LifecycleError> {
    verify_installed_executable_authenticode_with(
        path,
        expected,
        embedded_authenticode_thumbprint()?,
        run_windows_authenticode_check,
    )
}

#[cfg(target_os = "windows")]
fn verify_installed_executable_authenticode_with<F>(
    path: &Path,
    expected: &ReleaseFile,
    expected_thumbprint: Option<&str>,
    verify: F,
) -> Result<(), LifecycleError>
where
    F: FnOnce(&Path, &Path, &Path, &str) -> Result<(), LifecycleError>,
{
    if !expected.executable
        || !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return Ok(());
    }
    let Some(thumbprint) = expected_thumbprint else {
        // Development/test builds may intentionally omit production signing.
        // Once packaging embeds the expected signer, every installed .exe
        // inventory audit below fails closed on any trust/signature mismatch.
        return Ok(());
    };
    verify_windows_authenticode_with(path, thumbprint, verify)
}

#[cfg(target_os = "windows")]
fn embedded_authenticode_thumbprint() -> Result<Option<&'static str>, LifecycleError> {
    let Some(thumbprint) = option_env!("AXIUSFLOW_AUTHENTICODE_CERT_SHA1") else {
        return Ok(None);
    };
    if !valid_authenticode_thumbprint(thumbprint) {
        return Err(LifecycleError::VerificationFailed);
    }
    Ok(Some(thumbprint))
}

/// Verifies one Windows executable against the production publisher signer and
/// timestamp embedded by release packaging.
///
/// This is used for the stable launcher copy, which may legitimately remain
/// newer than the active version directory after a retained-known-good rollback.
///
/// # Errors
/// Fails closed when the production signer is not embedded or Windows trust,
/// signer-thumbprint, or timestamp verification fails.
#[cfg(target_os = "windows")]
pub fn verify_windows_publisher_signature(path: &Path) -> Result<(), LifecycleError> {
    let thumbprint =
        embedded_authenticode_thumbprint()?.ok_or(LifecycleError::VerificationFailed)?;
    verify_windows_authenticode_with(path, thumbprint, run_windows_authenticode_check)
}

#[cfg(target_os = "windows")]
fn valid_authenticode_thumbprint(thumbprint: &str) -> bool {
    thumbprint.len() == 40 && thumbprint.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(target_os = "windows")]
fn system32_powershell() -> Result<(PathBuf, PathBuf), LifecycleError> {
    let system_root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(LifecycleError::VerificationFailed)?;
    let powershell = system_root.join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let security_module = system_root.join(
        "System32/WindowsPowerShell/v1.0/Modules/Microsoft.PowerShell.Security/Microsoft.PowerShell.Security.psd1",
    );
    for path in [&powershell, &security_module] {
        let metadata =
            fs::symlink_metadata(path).map_err(|_| LifecycleError::VerificationFailed)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(LifecycleError::VerificationFailed);
        }
    }
    Ok((powershell, security_module))
}

#[cfg(target_os = "windows")]
fn verify_windows_authenticode_with<F>(
    path: &Path,
    expected_thumbprint: &str,
    verify: F,
) -> Result<(), LifecycleError>
where
    F: FnOnce(&Path, &Path, &Path, &str) -> Result<(), LifecycleError>,
{
    if !valid_authenticode_thumbprint(expected_thumbprint) {
        return Err(LifecycleError::VerificationFailed);
    }
    let (powershell, security_module) = system32_powershell()?;
    verify(&powershell, &security_module, path, expected_thumbprint)
}

#[cfg(target_os = "windows")]
fn run_windows_authenticode_check(
    powershell: &Path,
    security_module: &Path,
    path: &Path,
    expected_thumbprint: &str,
) -> Result<(), LifecycleError> {
    const SCRIPT: &str = "$ErrorActionPreference='Stop'; Import-Module -Name $env:AXIUSFLOW_AUTHENTICODE_SECURITY_MODULE -Force -ErrorAction Stop; $s=Microsoft.PowerShell.Security\\Get-AuthenticodeSignature -LiteralPath $env:AXIUSFLOW_AUTHENTICODE_PATH -ErrorAction Stop; if ($s.Status -ne [System.Management.Automation.SignatureStatus]::Valid -or $null -eq $s.SignerCertificate -or $s.SignerCertificate.Thumbprint -ine $env:AXIUSFLOW_AUTHENTICODE_CERT_SHA1 -or $null -eq $s.TimeStamperCertificate) { exit 1 }; exit 0";
    let mut child = Command::new(powershell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            SCRIPT,
        ])
        .env("AXIUSFLOW_AUTHENTICODE_SECURITY_MODULE", security_module)
        .env("AXIUSFLOW_AUTHENTICODE_PATH", path)
        .env("AXIUSFLOW_AUTHENTICODE_CERT_SHA1", expected_thumbprint)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| LifecycleError::VerificationFailed)?;
    let deadline = Instant::now() + AUTHENTICODE_AUDIT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) | Err(_) => return Err(LifecycleError::VerificationFailed),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(LifecycleError::VerificationFailed);
            }
        }
    }
}

fn collect_relative_files(
    root: &Path,
    current: &Path,
    files: &mut BTreeSet<PathBuf>,
) -> Result<(), LifecycleError> {
    let metadata = fs::symlink_metadata(current).map_err(|_| LifecycleError::VerificationFailed)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LifecycleError::VerificationFailed);
    }
    for entry in fs::read_dir(current).map_err(|_| LifecycleError::VerificationFailed)? {
        let entry = entry.map_err(|_| LifecycleError::VerificationFailed)?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|_| LifecycleError::VerificationFailed)?;
        if file_type.is_symlink() {
            return Err(LifecycleError::VerificationFailed);
        }
        if file_type.is_dir() {
            collect_relative_files(root, &path, files)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| LifecycleError::VerificationFailed)?
                .to_path_buf();
            files.insert(relative);
        } else {
            return Err(LifecycleError::VerificationFailed);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn verify_executable(path: &Path, executable: bool) -> Result<(), LifecycleError> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = fs::metadata(path)
        .map_err(|_| LifecycleError::VerificationFailed)?
        .permissions()
        .mode();
    if (mode & 0o111 != 0) != executable {
        return Err(LifecycleError::VerificationFailed);
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_executable(_path: &Path, _executable: bool) {
    // Windows has no executable permission bit; launchability is established
    // by the verified file inventory, native executable names, and OS
    // execution semantics instead.
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> Result<(), LifecycleError> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|_| LifecycleError::StagingFailed)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) {
    // Windows carries no POSIX executable bit to set; see `verify_executable`.
}

// Durability guarantee per platform: on Unix, directory `fsync` ensures the
// staged file inventory and atomic pointer rename survive a crash. Windows
// uses MoveFileExW with REPLACE_EXISTING + WRITE_THROUGH after syncing the
// staged file. Keep directory sync as a typed no-op there because Windows
// does not expose the same directory-fsync contract through std.
#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), LifecycleError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| LifecycleError::StagingFailed)
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) {}

fn write_json_atomic<T: Serialize>(
    directory: &Path,
    name: &str,
    value: &T,
) -> Result<(), LifecycleError> {
    fs::create_dir_all(directory).map_err(|_| LifecycleError::StagingFailed)?;
    let bytes = serde_json::to_vec(value).map_err(|_| LifecycleError::JournalCorrupt)?;
    if bytes.len() > MAXIMUM_MANIFEST_BYTES || !valid_identifier(name, 192) {
        return Err(LifecycleError::JournalCorrupt);
    }
    let temporary = directory.join(format!(".{name}.next"));
    remove_file_if_present(&temporary).map_err(|_| LifecycleError::StagingFailed)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| LifecycleError::StagingFailed)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| LifecycleError::StagingFailed)?;
    let destination = directory.join(name);
    if let Err(error) = replace_file_atomic(&temporary, &destination) {
        let _ = remove_file_if_present(&temporary);
        return Err(error);
    }
    #[cfg(unix)]
    return sync_directory(directory);
    #[cfg(not(unix))]
    {
        sync_directory(directory);
        Ok(())
    }
}

/// Replaces a small lifecycle record without exposing a partially written file.
///
fn replace_file_atomic(temporary: &Path, destination: &Path) -> Result<(), LifecycleError> {
    crate::replace_file_atomically(temporary, destination)
        .map_err(|_| LifecycleError::StagingFailed)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, LifecycleError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| LifecycleError::JournalCorrupt)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAXIMUM_MANIFEST_BYTES as u64
    {
        return Err(LifecycleError::JournalCorrupt);
    }
    let bytes = fs::read(path).map_err(|_| LifecycleError::JournalCorrupt)?;
    serde_json::from_slice(&bytes).map_err(|_| LifecycleError::JournalCorrupt)
}

fn remove_file_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_owned_path(path: &Path) -> std::io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return fs::remove_file(path);
    }
    for entry in fs::read_dir(path)? {
        remove_owned_path(&entry?.path())?;
    }
    fs::remove_dir(path)
}

fn remove_lifecycle_root_after_uninstall(root: &Path) -> std::io::Result<()> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return remove_owned_path(root);
    }
    let uninstall_marker = root.join("uninstall.json");
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path == uninstall_marker {
            continue;
        }
        remove_owned_path(&path)?;
    }
    remove_file_if_present(&uninstall_marker)?;
    fs::remove_dir(root)
}

fn count_entries(path: &Path) -> Result<usize, LifecycleError> {
    fs::read_dir(path)
        .map_err(|_| LifecycleError::UpdatePendingCleanup)?
        .try_fold(0_usize, |count, entry| {
            entry
                .map(|_| count.saturating_add(1))
                .map_err(|_| LifecycleError::UpdatePendingCleanup)
        })
}

struct LifecycleLock {
    path: PathBuf,
    file: Option<File>,
}

impl LifecycleLock {
    fn acquire(path: &Path) -> Result<Self, LifecycleError> {
        let parent = path.parent().ok_or(LifecycleError::UpdateLocked)?;
        fs::create_dir_all(parent).map_err(|_| LifecycleError::UpdateLocked)?;
        for _ in 0..3 {
            if fs::symlink_metadata(path)
                .is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
            {
                return Err(LifecycleError::UpdateLocked);
            }
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)
                .map_err(|_| LifecycleError::UpdateLocked)?;
            file.try_lock().map_err(|_| LifecycleError::UpdateLocked)?;
            if !lock_handle_matches_path(&file, path) {
                let _ = file.unlock();
                continue;
            }
            file.set_len(0)
                .and_then(|()| writeln!(file, "{}", std::process::id()))
                .and_then(|()| file.sync_all())
                .map_err(|_| LifecycleError::UpdateLocked)?;
            return Ok(Self {
                path: path.to_path_buf(),
                file: Some(file),
            });
        }
        Err(LifecycleError::UpdateLocked)
    }

    fn release(mut self) -> Result<(), LifecycleError> {
        self.release_inner()
            .map_err(|_| LifecycleError::UninstallPendingCleanup)
    }

    fn release_inner(&mut self) -> std::io::Result<()> {
        let Some(file) = self.file.take() else {
            return Ok(());
        };
        let remove_result = remove_file_if_present(&self.path);
        let unlock_result = file.unlock();
        remove_result.and(unlock_result)
    }
}

impl Drop for LifecycleLock {
    fn drop(&mut self) {
        let _ = self.release_inner();
    }
}

fn lock_handle_matches_path(file: &File, path: &Path) -> bool {
    let Ok(opened) = file.try_clone().and_then(FileIdentity::from_file) else {
        return false;
    };
    FileIdentity::from_path(path).is_ok_and(|current| current == opened)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier, Mutex};

    const LIFECYCLE_LOCK_CHILD_PATH_ENV: &str = "AXIUSFLOW_TEST_LIFECYCLE_LOCK_PATH";

    #[derive(Default)]
    struct Hooks {
        fail_prepare: bool,
        fail_health: bool,
        health_checks: Mutex<usize>,
        deleted: Mutex<Vec<String>>,
    }

    impl LifecycleHooks for Hooks {
        fn prepare_activation(&self, _previous: Option<&ActiveRelease>) -> Result<(), String> {
            if self.fail_prepare {
                Err("fixture shutdown failure".to_string())
            } else {
                Ok(())
            }
        }

        fn health_check(&self, _candidate: &ActiveRelease) -> Result<(), String> {
            let mut health_checks = self
                .health_checks
                .lock()
                .map_err(|_| "health counter lock".to_string())?;
            *health_checks = health_checks.saturating_add(1);
            if self.fail_health {
                Err("fixture health failure".to_string())
            } else {
                Ok(())
            }
        }

        fn disable_registrations(&self, _registrations: &[String]) -> Result<(), String> {
            Ok(())
        }

        fn stop_owned_processes(&self) -> Result<(), String> {
            Ok(())
        }

        fn delete_vault_entry(&self, entry: &VaultEntry) -> Result<(), String> {
            self.deleted
                .lock()
                .map_err(|_| "lock".to_string())?
                .push(format!("{}:{}", entry.service, entry.key));
            Ok(())
        }

        fn audit_external_absence(&self, _inventory: &InstallationInventory) -> Result<(), String> {
            Ok(())
        }
    }

    fn temporary_root(name: &str) -> PathBuf {
        let root = fixture_base_dir().join(format!(
            "axiusflow-lifecycle-{name}-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        fs::create_dir_all(&root).expect("fixture root");
        root
    }

    /// Fixture base with symlinked ancestors resolved: installer roots
    /// reject symlinks, and macOS points `TMPDIR` under `/var`, which is a
    /// symlink to `/private/var`.
    #[cfg(unix)]
    fn fixture_base_dir() -> PathBuf {
        std::fs::canonicalize(std::env::temp_dir()).expect("canonical fixture base")
    }

    #[cfg(not(unix))]
    fn fixture_base_dir() -> PathBuf {
        std::env::temp_dir()
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    }

    #[test]
    fn lifecycle_lock_excludes_other_owners_and_recovers_a_crash_stale_file() {
        let root = temporary_root("transaction-lock");
        let lock_path = root.join("transaction.lock");
        fs::write(&lock_path, b"stale diagnostic pid\n").expect("stale lock fixture");

        let first = LifecycleLock::acquire(&lock_path).expect("first lock");
        assert!(matches!(
            LifecycleLock::acquire(&lock_path),
            Err(LifecycleError::UpdateLocked)
        ));
        assert!(lock_path.is_file());
        drop(first);
        assert!(!lock_path.exists(), "normal release removes lock artifact");

        let second = LifecycleLock::acquire(&lock_path).expect("lock after release");
        assert!(lock_path.is_file());
        drop(second);
        assert!(!lock_path.exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn lifecycle_lock_concurrent_attempts_have_exactly_one_owner() {
        const ATTEMPTS: usize = 8;
        let root = temporary_root("transaction-lock-concurrent");
        let lock_path = root.join("transaction.lock");
        let start = Arc::new(Barrier::new(ATTEMPTS));
        let attempted = Arc::new(Barrier::new(ATTEMPTS));
        let threads = (0..ATTEMPTS)
            .map(|_| {
                let lock_path = lock_path.clone();
                let start = Arc::clone(&start);
                let attempted = Arc::clone(&attempted);
                std::thread::spawn(move || {
                    start.wait();
                    let lock = LifecycleLock::acquire(&lock_path);
                    let acquired = lock.is_ok();
                    attempted.wait();
                    drop(lock);
                    acquired
                })
            })
            .collect::<Vec<_>>();
        let acquired = threads
            .into_iter()
            .map(|thread| thread.join().expect("lock contender joins"))
            .filter(|acquired| *acquired)
            .count();
        assert_eq!(acquired, 1);
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn lifecycle_lock_child_process() {
        let Some(path) = std::env::var_os(LIFECYCLE_LOCK_CHILD_PATH_ENV) else {
            return;
        };
        let _lock =
            LifecycleLock::acquire(Path::new(&path)).expect("child acquires lifecycle lock");
        std::process::exit(0);
    }

    #[test]
    fn lifecycle_lock_is_released_by_abrupt_process_exit() {
        let root = temporary_root("transaction-lock-crash");
        let lock_path = root.join("transaction.lock");
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .arg("--exact")
            .arg("lifecycle::tests::lifecycle_lock_child_process")
            .env(LIFECYCLE_LOCK_CHILD_PATH_ENV, &lock_path)
            .status()
            .expect("crash-lock child starts");
        assert!(status.success());
        assert!(
            lock_path.exists(),
            "crashed owner leaves only an unlocked file"
        );

        let lock = LifecycleLock::acquire(&lock_path)
            .expect("operating system releases lock when owner process exits");
        drop(lock);
        assert!(!lock_path.exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn lifecycle_lock_stays_authoritative_through_lifecycle_root_teardown() {
        let root = temporary_root("transaction-lock-teardown");
        let lifecycle_root = root.join("lifecycle");
        fs::create_dir_all(&lifecycle_root).expect("lifecycle root");
        fs::write(lifecycle_root.join("uninstall.json"), b"{}").expect("uninstall marker fixture");
        let lock_path = root.join("lifecycle.lock");
        let lock = LifecycleLock::acquire(&lock_path).expect("teardown lock");

        remove_lifecycle_root_after_uninstall(&lifecycle_root).expect("lifecycle root removes");
        assert!(!lifecycle_root.exists());
        assert!(matches!(
            LifecycleLock::acquire(&lock_path),
            Err(LifecycleError::UpdateLocked)
        ));

        lock.release().expect("teardown lock releases");
        assert!(!lock_path.exists());
        let next =
            LifecycleLock::acquire(&lock_path).expect("next lifecycle owner starts afterward");
        drop(next);
        let _ = remove_owned_path(&root);
    }

    fn release_with_rollback_bytes(
        root: &Path,
        generation: u64,
        rollback_compatibility: Option<Vec<u8>>,
    ) -> (SignedReleaseManifest, SigningKey, PathBuf) {
        let bundle = root.join(format!("bundle-{generation}"));
        fs::create_dir_all(&bundle).expect("bundle");
        let desktop = format!("desktop-{generation}").into_bytes();
        let launcher = format!("launcher-{generation}").into_bytes();
        fs::write(bundle.join("axiusflow_desktop"), &desktop).expect("desktop");
        fs::write(bundle.join("axiusflow_launcher"), &launcher).expect("launcher");
        if let Some(bytes) = &rollback_compatibility {
            fs::write(bundle.join(ROLLBACK_COMPATIBILITY_FILENAME), bytes)
                .expect("rollback compatibility");
        }
        let mut files = vec![
            (ReleaseFileRole::Desktop, "axiusflow_desktop", desktop),
            (
                ReleaseFileRole::RuntimeAsset,
                "axiusflow_launcher",
                launcher,
            ),
        ];
        if let Some(bytes) = rollback_compatibility {
            files.push((
                ReleaseFileRole::RuntimeAsset,
                ROLLBACK_COMPATIBILITY_FILENAME,
                bytes,
            ));
        }
        let files = files
            .into_iter()
            .map(|(role, path, bytes)| ReleaseFile {
                role,
                path: path.to_string(),
                url: format!("https://releases.axiusflow.test/{generation}/{path}"),
                size: bytes.len() as u64,
                sha256: URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)),
                executable: true,
            })
            .collect();
        let manifest = ReleaseManifest {
            schema_version: RELEASE_MANIFEST_SCHEMA_VERSION,
            release_identity: format!("release-{generation}"),
            install_generation: generation,
            channel: "stable".to_string(),
            minimum_version: "0.1.0".to_string(),
            platform: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            files,
            rollout: RolloutMetadata {
                cohort: "all".to_string(),
                percentage: 100,
            },
        };
        let key = SigningKey::from_bytes(&[u8::try_from(generation).unwrap_or(0); 32]);
        let signed = sign_release_manifest(manifest, &key).expect("sign manifest");
        (signed, key, bundle)
    }

    fn release(root: &Path, generation: u64) -> (SignedReleaseManifest, SigningKey, PathBuf) {
        release_with_rollback_bytes(
            root,
            generation,
            Some(
                serde_json::to_vec(&RollbackCompatibilityMetadata::current())
                    .expect("rollback compatibility encodes"),
            ),
        )
    }

    fn release_with_compatibility_epoch(
        root: &Path,
        generation: u64,
        epoch: u32,
    ) -> (SignedReleaseManifest, SigningKey, PathBuf) {
        release_with_rollback_bytes(
            root,
            generation,
            Some(
                serde_json::to_vec(&RollbackCompatibilityMetadata {
                    schema_version: ROLLBACK_COMPATIBILITY_SCHEMA_VERSION,
                    state_compatibility_epoch: epoch,
                })
                .expect("rollback compatibility encodes"),
            ),
        )
    }

    #[cfg(windows)]
    #[test]
    fn installed_exe_inventory_authenticode_gate_uses_absolute_system32_trust_check() {
        let root = temporary_root("authenticode-inventory");
        let executable = root.join("fixture.exe");
        let bytes = b"signed-fixture-shape";
        fs::write(&executable, bytes).expect("fixture executable");
        let expected = ReleaseFile {
            role: ReleaseFileRole::Desktop,
            path: "fixture.exe".to_string(),
            url: "https://releases.axiusflow.test/fixture.exe".to_string(),
            size: bytes.len() as u64,
            sha256: URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)),
            executable: true,
        };
        let calls = std::cell::Cell::new(0_u32);
        verify_candidate_inventory_with(&root, std::slice::from_ref(&expected), |path, file| {
            verify_installed_executable_authenticode_with(
                path,
                file,
                Some("0123456789ABCDEF0123456789ABCDEF01234567"),
                |powershell, security_module, checked, thumbprint| {
                    calls.set(calls.get() + 1);
                    assert!(powershell.is_absolute());
                    assert!(security_module.is_absolute());
                    assert!(powershell.to_string_lossy().contains("System32"));
                    assert!(security_module.to_string_lossy().contains("System32"));
                    assert_eq!(checked, executable);
                    assert_eq!(thumbprint, "0123456789ABCDEF0123456789ABCDEF01234567");
                    Ok(())
                },
            )
        })
        .expect("inventory accepts trusted signer seam");
        assert_eq!(calls.get(), 1);

        assert_eq!(
            verify_installed_executable_authenticode_with(
                &executable,
                &expected,
                Some("0123456789ABCDEF0123456789ABCDEF01234567"),
                |_, _, _, _| Err(LifecycleError::VerificationFailed),
            ),
            Err(LifecycleError::VerificationFailed)
        );
        assert_eq!(
            verify_installed_executable_authenticode_with(
                &executable,
                &expected,
                Some("not-a-thumbprint"),
                |_, _, _, _| Ok(()),
            ),
            Err(LifecycleError::VerificationFailed)
        );
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn tampered_manifest_and_bundle_fail_closed() {
        let root = temporary_root("tamper");
        let (mut signed, key, _bundle) = release(&root, 1);
        signed.manifest.channel = "preview".to_string();
        assert_eq!(
            verify_release_manifest(&signed, &key.verifying_key(), &ReleasePolicy::native(0)),
            Err(LifecycleError::InvalidSignature)
        );
        let (signed, key, bundle) = release(&root, 2);
        fs::write(bundle.join("axiusflow_desktop"), b"tampered").expect("tamper bundle");
        let install_root = root.join("install");
        let installer =
            ReleaseInstaller::new(&install_root, key.verifying_key(), ReleasePolicy::native(0))
                .expect("installer");
        assert_eq!(
            installer.install(&signed, &bundle, &Hooks::default()),
            Err(LifecycleError::VerificationFailed)
        );
        assert_eq!(installer.active_release().expect("active state"), None);
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn legacy_engine_manifest_signature_can_be_authenticated_without_reaccepting_its_shape() {
        let root = temporary_root("legacy-signature");
        let (signed, key, _bundle) = release(&root, 1);
        let mut manifest = signed.manifest;
        manifest.files.insert(
            1,
            ReleaseFile {
                role: ReleaseFileRole::Engine,
                path: "axiusflow_engine".to_string(),
                url: "https://releases.axiusflow.test/1/axiusflow_engine".to_string(),
                size: 6,
                sha256: URL_SAFE_NO_PAD.encode(Sha256::digest(b"engine")),
                executable: true,
            },
        );
        let canonical = canonical_manifest(&manifest).expect("legacy canonical manifest");
        let legacy = SignedReleaseManifest {
            manifest,
            signature: URL_SAFE_NO_PAD.encode(key.sign(&canonical).to_bytes()),
        };

        assert_eq!(
            verify_release_manifest(&legacy, &key.verifying_key(), &ReleasePolicy::native(0)),
            Err(LifecycleError::InvalidManifest)
        );
        verify_release_manifest_signature(&legacy, &key.verifying_key())
            .expect("legacy predecessor signature verifies cryptographically");
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn interrupted_bundle_fails_closed_without_activation() {
        let root = temporary_root("interrupted-bundle");
        let (signed, key, bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        fs::remove_file(bundle.join("axiusflow_launcher")).expect("drop one bundle file");
        assert_eq!(
            installer.install(&signed, &bundle, &Hooks::default()),
            Err(LifecycleError::StagingFailed)
        );
        assert_eq!(installer.active_release().expect("active state"), None);
        assert!(!installer.update_journal_exists());
        let _ = remove_owned_path(&root);

        let root = temporary_root("truncated-bundle");
        let (signed, key, bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        fs::write(bundle.join("axiusflow_desktop"), b"truncated").expect("truncate bundle file");
        assert_eq!(
            installer.install(&signed, &bundle, &Hooks::default()),
            Err(LifecycleError::VerificationFailed)
        );
        assert_eq!(installer.active_release().expect("active state"), None);
        assert!(!installer.update_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn release_minimum_version_is_enforced_after_signature_verification() {
        let root = temporary_root("minimum-version");
        let (signed, key, _) = release(&root, 1);
        let mut manifest = signed.manifest;
        manifest.minimum_version = "999.0.0".to_string();
        let signed = sign_release_manifest(manifest, &key).expect("sign incompatible release");
        assert_eq!(
            verify_release_manifest(&signed, &key.verifying_key(), &ReleasePolicy::native(0)),
            Err(LifecycleError::IncompatibleRelease)
        );
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn successful_upgrade_retains_one_verified_known_good_release() {
        let root = temporary_root("upgrade");
        let (first, key, first_bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install");
        let (second, _, second_bundle) = release(&root, 2);
        let second = sign_release_manifest(second.manifest, &key).expect("same release key");
        let outcome = installer
            .install(&second, &second_bundle, &Hooks::default())
            .expect("upgrade");
        assert_eq!(outcome.active.install_generation, 2);
        assert_eq!(outcome.removed_release, None);
        assert_eq!(
            installer
                .retained_known_good_release()
                .expect("retained release")
                .expect("known-good release")
                .install_generation,
            1
        );
        assert_eq!(count_entries(&root.join("install/versions")), Ok(2));
        assert!(!installer.update_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn verified_release_manifest_accepts_only_owned_audited_releases() {
        let root = temporary_root("verified-manifest");
        let (first, key, first_bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install");
        let (second, _, second_bundle) = release(&root, 2);
        let second = sign_release_manifest(second.manifest, &key).expect("same key");
        let active = installer
            .install(&second, &second_bundle, &Hooks::default())
            .expect("second install")
            .active;
        let retained = installer
            .retained_known_good_release()
            .expect("retained read")
            .expect("retained release");

        assert_eq!(
            installer
                .verified_release_manifest(&active)
                .expect("active manifest")
                .manifest
                .install_generation,
            2
        );
        assert_eq!(
            installer
                .verified_release_manifest(&retained)
                .expect("retained manifest")
                .manifest
                .install_generation,
            1
        );
        let unknown = ActiveRelease {
            release_identity: "release-9".to_string(),
            install_generation: 9,
            directory_name: "00000000000000000009-release-9".to_string(),
        };
        assert_eq!(
            installer.verified_release_manifest(&unknown),
            Err(LifecycleError::VerificationFailed)
        );
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn successive_upgrades_rotate_the_retained_release_and_stay_bounded() {
        let root = temporary_root("successive-upgrades");
        let (first, key, first_bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install");
        let mut removed = Vec::new();
        for generation in [2_u64, 3] {
            let (next, _, next_bundle) = release(&root, generation);
            let next = sign_release_manifest(next.manifest, &key).expect("same release key");
            let outcome = installer
                .install(&next, &next_bundle, &Hooks::default())
                .expect("upgrade");
            assert_eq!(outcome.active.install_generation, generation);
            removed.push(
                outcome
                    .removed_release
                    .map(|release| release.install_generation),
            );
        }
        assert_eq!(
            installer
                .active_release()
                .expect("active")
                .expect("one active release")
                .install_generation,
            3
        );
        assert_eq!(removed, vec![None, Some(1)]);
        assert_eq!(
            installer
                .retained_known_good_release()
                .expect("retained release")
                .expect("known-good release")
                .install_generation,
            2
        );
        assert_eq!(count_entries(&root.join("install/versions")), Ok(2));
        assert!(
            !root
                .join("install/versions/00000000000000000001-release-1")
                .exists()
        );
        assert!(!installer.update_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn retained_known_good_inventory_is_reverified_before_use() {
        let root = temporary_root("retained-audit");
        let (first, key, first_bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install");
        let (second, _, second_bundle) = release(&root, 2);
        let second = sign_release_manifest(second.manifest, &key).expect("same release key");
        installer
            .install(&second, &second_bundle, &Hooks::default())
            .expect("upgrade");
        let retained = installer
            .retained_known_good_release()
            .expect("retained release")
            .expect("known-good release");
        fs::write(
            installer
                .release_directory(&retained)
                .expect("retained directory")
                .join("axiusflow_desktop"),
            b"tampered retained desktop",
        )
        .expect("mutate retained desktop");
        assert_eq!(
            installer.retained_known_good_release(),
            Err(LifecycleError::VerificationFailed)
        );
        assert_eq!(
            installer.audit_active_release(),
            Err(LifecycleError::VerificationFailed)
        );
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn normal_launch_audit_rejects_post_install_file_mutation() {
        let root = temporary_root("launch-audit");
        let (signed, key, bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        let active = installer
            .install(&signed, &bundle, &Hooks::default())
            .expect("install")
            .active;
        let desktop = installer
            .release_directory(&active)
            .expect("release directory")
            .join("axiusflow_desktop");
        fs::write(desktop, b"post-install mutation").expect("mutate active desktop");
        assert_eq!(
            installer.audit_active_release(),
            Err(LifecycleError::VerificationFailed)
        );
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn normal_launch_audit_rejects_a_superseded_release_directory() {
        let root = temporary_root("launch-stale-release");
        let (signed, key, bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        installer
            .install(&signed, &bundle, &Hooks::default())
            .expect("install");
        fs::create_dir_all(root.join("install/versions/stale-release"))
            .expect("stale release directory");
        assert_eq!(
            installer.audit_active_release(),
            Err(LifecycleError::UpdatePendingCleanup)
        );
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn active_release_ignores_uncommitted_next_pointer() {
        let root = temporary_root("ignore-next-pointer");
        let (first, key, first_bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        let active = installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install")
            .active;
        let uncommitted = ActiveRelease {
            release_identity: "release-2".to_string(),
            install_generation: 2,
            directory_name: "00000000000000000002-release-2".to_string(),
        };
        let temporary = installer
            .lifecycle_root
            .join("active")
            .join(format!(".{}.next", pointer_name(&uncommitted)));
        fs::write(
            &temporary,
            serde_json::to_vec(&uncommitted).expect("pointer encodes"),
        )
        .expect("temporary pointer writes");

        assert_eq!(
            installer.active_release().expect("active read"),
            Some(active)
        );
        assert_eq!(
            installer.audit_active_release(),
            Err(LifecycleError::UpdatePendingCleanup)
        );
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn preactivation_recovery_removes_pointer_and_manifest_temporary_files() {
        let root = temporary_root("preactivation-next-cleanup");
        let (first, key, first_bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        let previous = installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install")
            .active;
        let candidate = ActiveRelease {
            release_identity: "release-2".to_string(),
            install_generation: 2,
            directory_name: "00000000000000000002-release-2".to_string(),
        };
        let name = pointer_name(&candidate);
        let pointer_temporary = installer
            .lifecycle_root
            .join("active")
            .join(format!(".{name}.next"));
        let manifest_temporary = installer
            .lifecycle_root
            .join("manifests")
            .join(format!(".{name}.next"));
        fs::write(&pointer_temporary, b"partial-pointer").expect("temporary pointer");
        fs::write(&manifest_temporary, b"partial-manifest").expect("temporary manifest");
        installer
            .write_update_journal(&UpdateJournal {
                state: UpdateState::ProcessesStopped,
                candidate,
                previous: Some(previous.clone()),
                previous_retained: None,
            })
            .expect("preactivation journal");

        installer.recover(&Hooks::default()).expect("recover");
        assert!(!pointer_temporary.exists());
        assert!(!manifest_temporary.exists());
        assert_eq!(
            installer
                .audit_active_release()
                .expect("active audit")
                .expect("previous active"),
            previous
        );
        assert!(!installer.update_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn failed_health_check_restores_the_previous_pointer() {
        let root = temporary_root("rollback");
        let (first, key, first_bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install");
        let (second, _, second_bundle) = release(&root, 2);
        let second = sign_release_manifest(second.manifest, &key).expect("same key");
        assert_eq!(
            installer.install(
                &second,
                &second_bundle,
                &Hooks {
                    fail_health: true,
                    ..Hooks::default()
                }
            ),
            Err(LifecycleError::HealthCheckFailed)
        );
        assert_eq!(
            installer
                .active_release()
                .expect("active")
                .expect("previous")
                .install_generation,
            1
        );
        assert_eq!(count_entries(&root.join("install/versions")), Ok(1));
        assert!(!installer.update_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn activated_recovery_reaudits_candidate_before_executing_health() {
        let root = temporary_root("activated-recovery-audit");
        let (first, key, first_bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        let previous = installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install")
            .active;

        let (second, _, second_bundle) = release(&root, 2);
        let second = sign_release_manifest(second.manifest, &key).expect("same release key");
        let candidate = ActiveRelease {
            release_identity: second.manifest.release_identity.clone(),
            install_generation: second.manifest.install_generation,
            directory_name: format!(
                "{:020}-{}",
                second.manifest.install_generation, second.manifest.release_identity
            ),
        };
        let candidate_root = installer.version_path(&candidate).expect("candidate path");
        ReleaseInstaller::stage(&second, &second_bundle, &candidate_root).expect("stage candidate");
        installer
            .commit_manifest(&second, &candidate)
            .expect("commit candidate manifest");
        installer
            .commit_pointer(&candidate)
            .expect("commit candidate pointer");
        installer
            .write_update_journal(&UpdateJournal {
                state: UpdateState::Activated,
                candidate: candidate.clone(),
                previous: Some(previous),
                previous_retained: None,
            })
            .expect("write activated journal");

        fs::write(
            candidate_root.join("axiusflow_desktop"),
            b"post-activation mutation",
        )
        .expect("mutate candidate desktop");
        let hooks = Hooks::default();
        assert_eq!(
            installer.recover(&hooks),
            Err(LifecycleError::VerificationFailed)
        );
        assert_eq!(
            *hooks.health_checks.lock().expect("health counter locks"),
            0,
            "recovery must not execute candidate health after inventory verification fails"
        );
        assert_eq!(
            installer
                .read_update_journal()
                .expect("update journal reads")
                .expect("activated journal remains")
                .state,
            UpdateState::Activated,
            "verification failure remains durable for explicit remediation"
        );
        assert_eq!(
            installer.active_release().expect("active pointer reads"),
            Some(candidate),
            "recovery must not guess that rollback is state-compatible after candidate tampering"
        );
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn failed_health_check_never_rolls_back_across_state_compatibility_epochs() {
        let root = temporary_root("health-rollback-compatibility");
        let (first, key, first_bundle) = release_with_compatibility_epoch(&root, 1, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install");
        let (second, _, second_bundle) = release_with_compatibility_epoch(&root, 2, 2);
        let second = sign_release_manifest(second.manifest, &key).expect("same key");
        assert_eq!(
            installer.install(
                &second,
                &second_bundle,
                &Hooks {
                    fail_health: true,
                    ..Hooks::default()
                }
            ),
            Err(LifecycleError::RollbackFailed)
        );
        assert_eq!(
            installer
                .active_release()
                .expect("active")
                .expect("candidate remains selected")
                .install_generation,
            2
        );
        assert!(installer.update_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn recovery_restores_previous_after_partial_known_good_rotation_then_health_failure() {
        let root = temporary_root("rollback-after-partial-cleanup");
        let (first, key, first_bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        let previous = installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install")
            .active;
        let (second, _, second_bundle) = release(&root, 2);
        let second = sign_release_manifest(second.manifest, &key).expect("same key");
        let candidate = ActiveRelease {
            release_identity: second.manifest.release_identity.clone(),
            install_generation: second.manifest.install_generation,
            directory_name: format!(
                "{:020}-{}",
                second.manifest.install_generation, second.manifest.release_identity
            ),
        };
        let candidate_root = installer.version_path(&candidate).expect("candidate path");
        ReleaseInstaller::stage(&second, &second_bundle, &candidate_root).expect("stage candidate");
        installer
            .commit_manifest(&second, &candidate)
            .expect("commit candidate manifest");
        installer
            .commit_pointer(&candidate)
            .expect("commit candidate pointer");
        let journal = UpdateJournal {
            state: UpdateState::Activated,
            candidate: candidate.clone(),
            previous: Some(previous.clone()),
            previous_retained: None,
        };
        installer
            .write_update_journal(&journal)
            .expect("write activated journal");

        // Reproduce the crash window: the previous release has already become
        // the known-good record and lost its active pointer, while the update
        // journal still requires recovery.
        installer
            .commit_retained_known_good(&previous)
            .expect("rotate known-good pointer");
        installer
            .remove_pointer(&previous)
            .expect("remove previous active pointer");
        assert_eq!(
            installer
                .active_release()
                .expect("active release")
                .expect("candidate active"),
            candidate
        );

        installer
            .recover(&Hooks {
                fail_health: true,
                ..Hooks::default()
            })
            .expect("failed candidate recovers previous");
        assert_eq!(
            installer
                .audit_active_release()
                .expect("active audit")
                .expect("previous restored"),
            previous
        );
        assert_eq!(
            installer
                .retained_known_good_release()
                .expect("retained state"),
            None
        );
        assert_eq!(count_entries(&root.join("install/versions")), Ok(1));
        assert_eq!(
            count_entries(&installer.lifecycle_root.join("manifests")),
            Ok(1)
        );
        assert!(!installer.update_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn rolling_back_update_state_never_reconsiders_candidate_health() {
        let root = temporary_root("rollback-decision-is-durable");
        let (first, key, first_bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        let previous = installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install")
            .active;
        let (second, _, second_bundle) = release(&root, 2);
        let second = sign_release_manifest(second.manifest, &key).expect("same key");
        let candidate = ActiveRelease {
            release_identity: second.manifest.release_identity.clone(),
            install_generation: second.manifest.install_generation,
            directory_name: format!(
                "{:020}-{}",
                second.manifest.install_generation, second.manifest.release_identity
            ),
        };
        let candidate_root = installer.version_path(&candidate).expect("candidate path");
        ReleaseInstaller::stage(&second, &second_bundle, &candidate_root).expect("stage candidate");
        installer
            .commit_manifest(&second, &candidate)
            .expect("commit candidate manifest");
        installer
            .commit_pointer(&candidate)
            .expect("commit candidate pointer");
        installer
            .write_update_journal(&UpdateJournal {
                state: UpdateState::RollingBack,
                candidate: candidate.clone(),
                previous: Some(previous.clone()),
                previous_retained: None,
            })
            .expect("persist rollback decision");

        // Default hooks would report the candidate healthy. Recovery must not
        // re-run that decision after RollingBack has been durably recorded.
        installer
            .recover(&Hooks::default())
            .expect("resume decided rollback");
        assert_eq!(
            installer
                .audit_active_release()
                .expect("active audit")
                .expect("previous active"),
            previous
        );
        assert!(!candidate_root.exists());
        assert!(!installer.update_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn retained_known_good_rollback_selects_only_the_verified_local_predecessor() {
        let root = temporary_root("retained-rollback");
        let (first, key, first_bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install");
        let (second, _, second_bundle) = release(&root, 2);
        let second = sign_release_manifest(second.manifest, &key).expect("same key");
        installer
            .install(&second, &second_bundle, &Hooks::default())
            .expect("second install");

        let restored = installer
            .rollback_to_retained_known_good(&Hooks::default())
            .expect("rollback to retained release");
        assert_eq!(restored.install_generation, 1);
        assert_eq!(
            installer
                .audit_active_release()
                .expect("active audit")
                .expect("active release"),
            restored
        );
        assert_eq!(
            installer
                .retained_known_good_release()
                .expect("retained state"),
            None
        );
        assert_eq!(count_entries(&root.join("install/versions")), Ok(1));
        assert_eq!(
            count_entries(&installer.lifecycle_root.join("manifests")),
            Ok(1)
        );
        assert!(!installer.retained_rollback_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn retained_rollback_rejects_mismatched_signed_state_compatibility_epochs() {
        let root = temporary_root("rollback-compatibility-epoch");
        let (first, key, first_bundle) = release_with_compatibility_epoch(&root, 1, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install");
        let (second, _, second_bundle) = release_with_compatibility_epoch(&root, 2, 2);
        let second = sign_release_manifest(second.manifest, &key).expect("same release key");
        let active = installer
            .install(&second, &second_bundle, &Hooks::default())
            .expect("second install")
            .active;

        assert_eq!(
            installer.rollback_to_retained_known_good(&Hooks::default()),
            Err(LifecycleError::RollbackFailed)
        );
        assert_eq!(
            installer.active_release().expect("active read"),
            Some(active)
        );
        assert!(!installer.retained_rollback_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn retained_rollback_rejects_missing_signed_state_compatibility_asset() {
        let root = temporary_root("rollback-compatibility-missing");
        let (first, key, first_bundle) = release_with_rollback_bytes(&root, 1, None);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install");
        let (second, _, second_bundle) = release(&root, 2);
        let second = sign_release_manifest(second.manifest, &key).expect("same release key");
        let active = installer
            .install(&second, &second_bundle, &Hooks::default())
            .expect("second install")
            .active;

        assert_eq!(
            installer.rollback_to_retained_known_good(&Hooks::default()),
            Err(LifecycleError::RollbackFailed)
        );
        assert_eq!(
            installer.active_release().expect("active read"),
            Some(active)
        );
        assert!(!installer.retained_rollback_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn retained_rollback_rejects_malformed_signed_state_compatibility_asset() {
        let root = temporary_root("rollback-compatibility-malformed");
        let (first, key, first_bundle) =
            release_with_rollback_bytes(&root, 1, Some(b"not-json".to_vec()));
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install");
        let (second, _, second_bundle) = release(&root, 2);
        let second = sign_release_manifest(second.manifest, &key).expect("same release key");
        let active = installer
            .install(&second, &second_bundle, &Hooks::default())
            .expect("second install")
            .active;

        assert_eq!(
            installer.rollback_to_retained_known_good(&Hooks::default()),
            Err(LifecycleError::RollbackFailed)
        );
        assert_eq!(
            installer.active_release().expect("active read"),
            Some(active)
        );
        assert!(!installer.retained_rollback_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn health_checked_and_cleanup_recovery_never_rechecks_candidate_health() {
        for state in [UpdateState::HealthChecked, UpdateState::Cleanup] {
            let root = temporary_root(&format!("durable-health-{state:?}"));
            let (first, key, first_bundle) = release(&root, 1);
            let installer = ReleaseInstaller::new(
                root.join("install"),
                key.verifying_key(),
                ReleasePolicy::native(0),
            )
            .expect("installer");
            let previous = installer
                .install(&first, &first_bundle, &Hooks::default())
                .expect("first install")
                .active;
            let (second, _, second_bundle) = release(&root, 2);
            let second = sign_release_manifest(second.manifest, &key).expect("same release key");
            let candidate = ActiveRelease {
                release_identity: second.manifest.release_identity.clone(),
                install_generation: second.manifest.install_generation,
                directory_name: format!(
                    "{:020}-{}",
                    second.manifest.install_generation, second.manifest.release_identity
                ),
            };
            let candidate_root = installer.version_path(&candidate).expect("candidate path");
            ReleaseInstaller::stage(&second, &second_bundle, &candidate_root)
                .expect("stage candidate");
            installer
                .commit_manifest(&second, &candidate)
                .expect("candidate manifest");
            installer
                .commit_pointer(&candidate)
                .expect("candidate pointer");
            installer
                .write_update_journal(&UpdateJournal {
                    state,
                    candidate: candidate.clone(),
                    previous: Some(previous.clone()),
                    previous_retained: None,
                })
                .expect("durable success journal");

            installer
                .recover(&Hooks {
                    fail_health: true,
                    ..Hooks::default()
                })
                .expect("durable success resumes without health recheck");
            assert_eq!(
                installer
                    .audit_active_release()
                    .expect("active audit")
                    .expect("candidate remains active"),
                candidate
            );
            assert_eq!(
                installer
                    .retained_known_good_release()
                    .expect("retained read")
                    .expect("previous retained"),
                previous
            );
            assert!(!installer.update_journal_exists());
            let _ = remove_owned_path(&root);
        }
    }

    #[test]
    fn interrupted_retained_rollback_recovers_at_every_transaction_boundary() {
        for state in RetainedRollbackState::ALL {
            let root = temporary_root(&format!("retained-rollback-recover-{state:?}"));
            let (first, key, first_bundle) = release(&root, 1);
            let installer = ReleaseInstaller::new(
                root.join("install"),
                key.verifying_key(),
                ReleasePolicy::native(0),
            )
            .expect("installer");
            installer
                .install(&first, &first_bundle, &Hooks::default())
                .expect("first install");
            let (second, _, second_bundle) = release(&root, 2);
            let second = sign_release_manifest(second.manifest, &key).expect("same key");
            let failed = installer
                .install(&second, &second_bundle, &Hooks::default())
                .expect("second install")
                .active;
            let target = installer
                .retained_known_good_release()
                .expect("retained release")
                .expect("rollback target");
            if matches!(
                state,
                RetainedRollbackState::Activated | RetainedRollbackState::Cleanup
            ) {
                installer
                    .commit_pointer(&target)
                    .expect("commit rollback target");
                installer
                    .remove_pointer(&failed)
                    .expect("remove failed pointer");
            }
            installer
                .write_retained_rollback_journal(&RetainedRollbackJournal {
                    state,
                    failed,
                    target: target.clone(),
                })
                .expect("write rollback journal");

            installer
                .recover(&Hooks::default())
                .expect("recover retained rollback");
            assert_eq!(
                installer
                    .audit_active_release()
                    .expect("active audit")
                    .expect("rollback target active"),
                target
            );
            assert_eq!(
                installer
                    .retained_known_good_release()
                    .expect("retained state"),
                None
            );
            assert_eq!(count_entries(&root.join("install/versions")), Ok(1));
            assert!(!installer.retained_rollback_journal_exists());
            let _ = remove_owned_path(&root);
        }
    }

    #[test]
    fn interrupted_update_recovers_at_every_transaction_boundary() {
        for state in [
            UpdateState::Preparing,
            UpdateState::Staged,
            UpdateState::ProcessesStopped,
            UpdateState::Activated,
            UpdateState::HealthChecked,
            UpdateState::Cleanup,
        ] {
            let root = temporary_root(&format!("recover-{state:?}"));
            let (first, key, first_bundle) = release(&root, 1);
            let installer = ReleaseInstaller::new(
                root.join("install"),
                key.verifying_key(),
                ReleasePolicy::native(0),
            )
            .expect("installer");
            let previous = installer
                .install(&first, &first_bundle, &Hooks::default())
                .expect("first install")
                .active;
            let (second, _, second_bundle) = release(&root, 2);
            let second = sign_release_manifest(second.manifest, &key).expect("same key");
            let candidate = ActiveRelease {
                release_identity: second.manifest.release_identity.clone(),
                install_generation: second.manifest.install_generation,
                directory_name: format!(
                    "{:020}-{}",
                    second.manifest.install_generation, second.manifest.release_identity
                ),
            };
            let candidate_root = installer.version_path(&candidate).expect("candidate path");
            fs::create_dir_all(candidate_root.parent().expect("versions root"))
                .expect("versions root");
            if state != UpdateState::Preparing {
                ReleaseInstaller::stage(&second, &second_bundle, &candidate_root)
                    .expect("stage interrupted candidate");
            }
            if matches!(
                state,
                UpdateState::Activated | UpdateState::HealthChecked | UpdateState::Cleanup
            ) {
                installer
                    .commit_manifest(&second, &candidate)
                    .expect("commit interrupted manifest");
                installer
                    .commit_pointer(&candidate)
                    .expect("commit interrupted candidate");
            }
            installer
                .write_update_journal(&UpdateJournal {
                    state,
                    candidate: candidate.clone(),
                    previous: Some(previous.clone()),
                    previous_retained: None,
                })
                .expect("write interrupted journal");
            installer.recover(&Hooks::default()).expect("recover");
            assert!(!installer.update_journal_exists());
            let active = installer
                .active_release()
                .expect("active")
                .expect("one active release");
            let expected = if matches!(
                state,
                UpdateState::Activated | UpdateState::HealthChecked | UpdateState::Cleanup
            ) {
                &candidate
            } else {
                &previous
            };
            assert_eq!(&active, expected);
            let retained = installer
                .retained_known_good_release()
                .expect("retained release");
            if matches!(
                state,
                UpdateState::Activated | UpdateState::HealthChecked | UpdateState::Cleanup
            ) {
                assert_eq!(retained.as_ref(), Some(&previous));
            } else {
                assert_eq!(retained, None);
            }
            let _ = remove_owned_path(&root);
        }
    }

    #[test]
    fn update_journal_reads_newest_state_slot_and_rejects_corrupt_newest_slot() {
        let root = temporary_root("journal-slots");
        let (_, key, _) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        let candidate = ActiveRelease {
            release_identity: "candidate".to_string(),
            install_generation: 2,
            directory_name: "00000000000000000002-candidate".to_string(),
        };
        let staged = UpdateJournal {
            state: UpdateState::Staged,
            candidate: candidate.clone(),
            previous: None,
            previous_retained: None,
        };
        let activated = UpdateJournal {
            state: UpdateState::Activated,
            candidate,
            previous: None,
            previous_retained: None,
        };

        installer
            .write_update_journal(&staged)
            .expect("write staged slot");
        installer
            .write_update_journal(&activated)
            .expect("write activated slot");
        assert_eq!(
            installer
                .read_update_journal()
                .expect("read journal")
                .expect("journal")
                .state,
            UpdateState::Activated
        );

        fs::write(
            installer
                .lifecycle_root
                .join(update_journal_name(UpdateState::Activated)),
            b"not-json",
        )
        .expect("corrupt newest slot");
        assert_eq!(
            installer.read_update_journal(),
            Err(LifecycleError::JournalCorrupt)
        );
        let _ = remove_owned_path(&root);
    }

    #[cfg(windows)]
    #[test]
    fn locked_update_journal_preserves_old_record_and_cleans_temporary_file() {
        let root = temporary_root("journal-locked");
        let (_, key, _) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        let journal = UpdateJournal {
            state: UpdateState::Preparing,
            candidate: ActiveRelease {
                release_identity: "candidate".to_string(),
                install_generation: 2,
                directory_name: "00000000000000000002-candidate".to_string(),
            },
            previous: None,
            previous_retained: None,
        };
        installer
            .write_update_journal(&journal)
            .expect("write original journal");
        let path = installer
            .lifecycle_root
            .join(update_journal_name(UpdateState::Preparing));
        let lock = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .expect("open journal without delete sharing");
        assert_eq!(
            installer.write_update_journal(&journal),
            Err(LifecycleError::StagingFailed)
        );
        drop(lock);
        assert_eq!(
            installer
                .read_update_journal()
                .expect("read preserved journal")
                .expect("journal")
                .state,
            UpdateState::Preparing
        );
        assert!(
            !installer
                .lifecycle_root
                .join(".update-0-preparing.json.next")
                .exists()
        );
        let _ = remove_owned_path(&root);
    }

    #[cfg(windows)]
    #[test]
    fn locked_old_release_defers_cleanup_without_completing_update() {
        let root = temporary_root("old-release-locked");
        let (first, key, first_bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        let first_active = installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install")
            .active;
        let (second, _, second_bundle) = release(&root, 2);
        let second = sign_release_manifest(second.manifest, &key).expect("same key");
        installer
            .install(&second, &second_bundle, &Hooks::default())
            .expect("second install");
        let old_desktop = installer
            .release_directory(&first_active)
            .expect("old release directory")
            .join("axiusflow_desktop");
        // FILE_SHARE_READ lets the pre-install audit read the old release
        // while still blocking its deletion, mirroring a locked executable.
        let lock = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&old_desktop)
            .expect("hold old desktop without delete sharing");
        let (third, _, third_bundle) = release(&root, 3);
        let third = sign_release_manifest(third.manifest, &key).expect("same key");
        assert_eq!(
            installer.install(&third, &third_bundle, &Hooks::default()),
            Err(LifecycleError::UpdatePendingCleanup)
        );
        assert_eq!(
            installer
                .active_release()
                .expect("active state")
                .expect("candidate pointer"),
            ActiveRelease {
                release_identity: third.manifest.release_identity.clone(),
                install_generation: third.manifest.install_generation,
                directory_name: format!(
                    "{:020}-{}",
                    third.manifest.install_generation, third.manifest.release_identity
                ),
            }
        );
        assert_eq!(
            installer
                .read_retained_known_good()
                .expect("known-good pointer")
                .expect("known-good release")
                .install_generation,
            2
        );
        assert!(installer.update_journal_exists());
        assert_eq!(count_entries(&root.join("install/versions")), Ok(3));
        drop(lock);
        installer
            .recover(&Hooks::default())
            .expect("resume cleanup");
        assert_eq!(count_entries(&root.join("install/versions")), Ok(2));
        assert_eq!(
            installer
                .retained_known_good_release()
                .expect("retained release")
                .expect("known-good release")
                .install_generation,
            2
        );
        assert!(!installer.update_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn shutdown_failure_preserves_current_release_for_recovery() {
        let root = temporary_root("shutdown-failure");
        let (first, key, first_bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install");
        let (second, _, second_bundle) = release(&root, 2);
        let second = sign_release_manifest(second.manifest, &key).expect("same key");
        assert_eq!(
            installer.install(
                &second,
                &second_bundle,
                &Hooks {
                    fail_prepare: true,
                    ..Hooks::default()
                }
            ),
            Err(LifecycleError::ShutdownFailed)
        );
        assert_eq!(
            installer
                .active_release()
                .expect("active")
                .expect("previous")
                .install_generation,
            1
        );
        installer
            .recover(&Hooks::default())
            .expect("recover staged candidate");
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn unfinished_uninstall_blocks_normal_launch_recovery() {
        let root = temporary_root("uninstall-recovery");
        let (signed, key, bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        write_json_atomic(
            &installer.lifecycle_root,
            "uninstall.json",
            &UninstallJournal { started: true },
        )
        .expect("write uninstall journal");
        assert_eq!(
            installer.recover(&Hooks::default()),
            Err(LifecycleError::UninstallPendingCleanup)
        );
        assert_eq!(
            installer.install(&signed, &bundle, &Hooks::default()),
            Err(LifecycleError::UninstallPendingCleanup)
        );
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn native_inventory_registers_account_vault_keys_for_complete_uninstall() {
        let root = temporary_root("inventory-account-keys");
        let inventory =
            native_installation_inventory(root.join("install")).expect("native inventory builds");
        for key in [
            "account-refresh-default-v1",
            "account-entitlement-lease-v1",
            "account-entitlement-directory-v1",
            "account-device-key-v1",
            "account-profile-v1",
        ] {
            assert!(
                inventory
                    .vault_entries
                    .iter()
                    .any(|entry| entry.service == "com.axiusflow.account" && entry.key == key),
                "native inventory lost account vault key {key}"
            );
        }
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn legacy_inventory_cleanup_roots_decode_without_preserving_the_old_field_name() {
        let legacy = serde_json::json!({
            "schema_version": INVENTORY_SCHEMA_VERSION,
            "install_root": "install",
            "data_roots": [],
            "cache_roots": [],
            "log_roots": [],
            "ipc_paths": ["legacy-market-runtime"],
            "vault_entries": [],
            "registrations": []
        });
        let inventory: InstallationInventory =
            serde_json::from_value(legacy).expect("legacy inventory decodes");
        assert_eq!(
            inventory.legacy_cleanup_roots,
            vec![PathBuf::from("legacy-market-runtime")]
        );
        let current = serde_json::to_value(&inventory).expect("current inventory encodes");
        assert!(current.get("ipc_paths").is_none());
        assert!(current.get("legacy_cleanup_roots").is_some());
    }
    #[test]
    fn uninstall_is_idempotent_and_removes_exact_roots_and_vault_keys() {
        let root = temporary_root("uninstall");
        let (_, key, _) = release(&root, 1);
        let install_root = root.join("install");
        let data_root = root.join("data");
        fs::create_dir_all(&install_root).expect("install root");
        fs::create_dir_all(&data_root).expect("data root");
        fs::write(data_root.join("workspace-state.pb"), b"workspace").expect("workspace state");
        let installer =
            ReleaseInstaller::new(&install_root, key.verifying_key(), ReleasePolicy::native(0))
                .expect("installer");
        let inventory = InstallationInventory {
            schema_version: INVENTORY_SCHEMA_VERSION,
            install_root,
            data_roots: vec![data_root],
            cache_roots: Vec::new(),
            log_roots: Vec::new(),
            legacy_cleanup_roots: Vec::new(),
            vault_entries: vec![
                VaultEntry {
                    service: "com.axiusflow.account".to_string(),
                    key: "account-refresh-default-v1".to_string(),
                },
                VaultEntry {
                    service: "com.axiusflow.terminal".to_string(),
                    key: "provider-rithmic-test-default-v1".to_string(),
                },
            ],
            registrations: vec!["start-menu:Axiusflow".to_string()],
        };
        let hooks = Hooks::default();
        let outcome = installer.uninstall(&inventory, &hooks).expect("uninstall");
        assert_eq!(outcome.removed_vault_keys, 2);
        assert!(!inventory.install_root.exists());
        assert!(!inventory.data_roots[0].exists());
        installer
            .uninstall(&inventory, &hooks)
            .expect("repeated uninstall remains idempotent");
        let _ = remove_owned_path(&root);
    }

    #[cfg(unix)]
    #[test]
    fn uninstall_never_follows_symlinks_outside_owned_roots() {
        use std::os::unix::fs::symlink;
        let root = temporary_root("symlink");
        let outside = root.join("outside");
        let install = root.join("install");
        fs::create_dir_all(&outside).expect("outside");
        fs::create_dir_all(&install).expect("install");
        fs::write(outside.join("keep"), b"keep").expect("outside file");
        symlink(&outside, install.join("link")).expect("symlink");
        remove_owned_path(&install).expect("remove exact owned tree");
        assert!(outside.join("keep").exists());
        let _ = remove_owned_path(&root);
    }

    #[cfg(unix)]
    #[test]
    fn owned_roots_reject_symlinked_ancestors() {
        use std::os::unix::fs::symlink;
        let root = temporary_root("symlinked-ancestor");
        let real = root.join("real");
        let linked = root.join("linked");
        fs::create_dir_all(&real).expect("real parent");
        symlink(&real, &linked).expect("linked parent");
        let (_, key, _) = release(&root, 1);
        assert!(matches!(
            ReleaseInstaller::new(
                linked.join("install"),
                key.verifying_key(),
                ReleasePolicy::native(0)
            ),
            Err(LifecycleError::InvalidInventory)
        ));
        let _ = remove_owned_path(&root);
    }
}
