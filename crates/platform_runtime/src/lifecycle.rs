//! Signed, transactional install/update/uninstall mechanics.
//!
//! Packaging supplies the small stable launcher that calls this boundary. The
//! desktop and resident engine never replace or delete themselves.

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

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest as _, Sha256};
use sysinfo::{Pid, ProcessesToUpdate, System};

const MANIFEST_SCHEMA_VERSION: u32 = 1;
const INVENTORY_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_MANIFEST_BYTES: usize = 1024 * 1024;
const MAXIMUM_RELEASE_FILES: usize = 256;
const MAXIMUM_OWNED_ROOTS: usize = 64;
const MAXIMUM_VAULT_KEYS: usize = 64;
const MAXIMUM_RELEASE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

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
    pub ipc_paths: Vec<PathBuf>,
    pub vault_entries: Vec<VaultEntry>,
    pub registrations: Vec<String>,
}

/// Platform operations which cannot be implemented as portable filesystem work.
pub trait LifecycleHooks {
    /// Blocks relaunch and stops the exact active desktop/engine identities.
    ///
    /// # Errors
    /// Returns a redacted platform error if owned processes cannot be stopped.
    fn prepare_activation(&self, previous: Option<&ActiveRelease>) -> Result<(), String>;
    /// Performs the bounded desktop/engine IPC and market-readiness probe.
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

/// Successful update state. Success always means superseded files are absent.
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

fn canonical_manifest(manifest: &ReleaseManifest) -> Result<Vec<u8>, LifecycleError> {
    let bytes = serde_json::to_vec(manifest).map_err(|_| LifecycleError::InvalidManifest)?;
    if bytes.len() > MAXIMUM_MANIFEST_BYTES {
        return Err(LifecycleError::InvalidManifest);
    }
    Ok(bytes)
}

fn validate_manifest_shape(manifest: &ReleaseManifest) -> Result<(), LifecycleError> {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION
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
    if desktop != 1 || engine != 1 {
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
}

impl UpdateState {
    const ALL: [Self; 6] = [
        Self::Preparing,
        Self::Staged,
        Self::ProcessesStopped,
        Self::Activated,
        Self::HealthChecked,
        Self::Cleanup,
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
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateJournal {
    state: UpdateState,
    candidate: ActiveRelease,
    previous: Option<ActiveRelease>,
}

/// Filesystem transaction owner used by the packaging launcher/updater.
pub struct ReleaseInstaller {
    install_root: PathBuf,
    lifecycle_root: PathBuf,
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
            let metadata = entry
                .file_type()
                .map_err(|_| LifecycleError::JournalCorrupt)?;
            if !metadata.is_file() || metadata.is_symlink() {
                return Err(LifecycleError::JournalCorrupt);
            }
            let candidate: ActiveRelease = read_json(&entry.path())?;
            if !valid_active_release(&candidate) {
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

    /// Verifies the active pointer, signed manifest, complete file inventory,
    /// platform policy, and executable permissions before normal launch.
    ///
    /// # Errors
    /// Rejects any missing, extra, modified, symlinked, or incompatible artifact.
    pub fn audit_active_release(&self) -> Result<Option<ActiveRelease>, LifecycleError> {
        let Some(active) = self.active_release()? else {
            return Ok(None);
        };
        let signed: SignedReleaseManifest = read_json(&self.manifest_path(&active))?;
        verify_release_manifest(&signed, &self.verifying_key, &self.policy)?;
        if signed.manifest.release_identity != active.release_identity
            || signed.manifest.install_generation != active.install_generation
        {
            return Err(LifecycleError::VerificationFailed);
        }
        let root = self.version_path(&active)?;
        verify_candidate_inventory(&root, &signed.manifest.files)?;
        if count_entries(&self.lifecycle_root.join("active"))? != 1
            || count_entries(&self.lifecycle_root.join("manifests"))? != 1
            || count_entries(&self.install_root.join("versions"))? != 1
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
        let _lock = LifecycleLock::acquire(&self.lifecycle_root)?;
        let previous = self.audit_active_release()?;
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
        let pointer = self.commit_pointer(&candidate)?;
        journal.state = UpdateState::Activated;
        self.write_update_journal(&journal)?;
        if hooks.health_check(&candidate).is_err() {
            if fs::remove_file(&pointer).is_err()
                || self.remove_manifest(&candidate).is_err()
                || remove_owned_path(&candidate_root).is_err()
            {
                return Err(LifecycleError::RollbackFailed);
            }
            self.remove_update_journal()?;
            return Err(LifecycleError::HealthCheckFailed);
        }
        journal.state = UpdateState::HealthChecked;
        self.write_update_journal(&journal)?;
        journal.state = UpdateState::Cleanup;
        self.write_update_journal(&journal)?;
        if let Some(old) = &previous {
            let old_root = self.version_path(old)?;
            if remove_owned_path(&old_root).is_err()
                || self.remove_pointer(old).is_err()
                || self.remove_manifest(old).is_err()
            {
                return Err(LifecycleError::UpdatePendingCleanup);
            }
        }
        self.remove_update_journal()?;
        self.audit_single_active(&candidate)?;
        Ok(UpdateOutcome {
            active: candidate,
            removed_release: previous,
        })
    }

    /// Resumes or rolls back an interrupted transaction deterministically.
    ///
    /// # Errors
    /// Returns a pending-cleanup category until exact owned artifacts are gone.
    pub fn recover<H: LifecycleHooks>(&self, hooks: &H) -> Result<(), LifecycleError> {
        let _lock = LifecycleLock::acquire(&self.lifecycle_root)?;
        if self.lifecycle_root.join("uninstall.json").exists() {
            return Err(LifecycleError::UninstallPendingCleanup);
        }
        let Some(journal) = self.read_update_journal()? else {
            return Ok(());
        };
        let candidate_root = self.version_path(&journal.candidate)?;
        match journal.state {
            UpdateState::Preparing | UpdateState::Staged | UpdateState::ProcessesStopped => {
                remove_owned_path(&candidate_root)
                    .map_err(|_| LifecycleError::UpdatePendingCleanup)?;
                self.remove_pointer(&journal.candidate)?;
                self.remove_manifest(&journal.candidate)?;
            }
            UpdateState::Activated | UpdateState::HealthChecked | UpdateState::Cleanup => {
                if hooks.health_check(&journal.candidate).is_err() {
                    self.remove_pointer(&journal.candidate)
                        .map_err(|_| LifecycleError::RollbackFailed)?;
                    self.remove_manifest(&journal.candidate)
                        .map_err(|_| LifecycleError::RollbackFailed)?;
                    remove_owned_path(&candidate_root)
                        .map_err(|_| LifecycleError::RollbackFailed)?;
                } else if let Some(previous) = &journal.previous {
                    remove_owned_path(&self.version_path(previous)?)
                        .map_err(|_| LifecycleError::UpdatePendingCleanup)?;
                    self.remove_pointer(previous)?;
                    self.remove_manifest(previous)?;
                }
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
        let lock = LifecycleLock::acquire(&self.lifecycle_root)?;
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
        let _ = fs::remove_file(self.lifecycle_root.join("uninstall.json"));
        lock.release()?;
        remove_owned_path(&self.lifecycle_root)
            .map_err(|_| LifecycleError::UninstallPendingCleanup)?;
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
            verify_file(&destination, expected)?;
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
        if count_entries(&self.lifecycle_root.join("manifests"))? != 1 {
            return Err(LifecycleError::UpdatePendingCleanup);
        }
        let versions = self.install_root.join("versions");
        if count_entries(&versions)? != 1 || !self.version_path(expected)?.is_dir() {
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
    roots.extend(inventory.ipc_paths.iter().cloned());
    let mut roots = roots.into_iter().collect::<Vec<_>>();
    roots.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    roots
}

/// Resolves the one native Axiusflow data root used by desktop and engine.
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
    let registrations = if cfg!(target_os = "windows") {
        vec!["windows-run:Axiusflow Engine".to_string()]
    } else if cfg!(target_os = "macos") {
        vec!["launch-agent:com.axiusflow.engine".to_string()]
    } else {
        vec!["xdg-autostart:axiusflow-engine.desktop".to_string()]
    };
    Ok(InstallationInventory {
        schema_version: INVENTORY_SCHEMA_VERSION,
        install_root,
        data_roots: vec![data],
        cache_roots: vec![cache],
        log_roots: vec![logs],
        ipc_paths: Vec::new(),
        vault_entries: vec![
            VaultEntry {
                service: "com.axiusflow.engine".to_string(),
                key: "local-ipc-token-v1".to_string(),
            },
            VaultEntry {
                service: "com.axiusflow.engine.history".to_string(),
                key: "history-catalog-key-v1".to_string(),
            },
            VaultEntry {
                service: "com.axiusflow.engine.history".to_string(),
                key: "coinbase-public-bars-key-v1".to_string(),
            },
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
                key: "account-device-key-v1".to_string(),
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

fn verify_file(path: &Path, expected: &ReleaseFile) -> Result<(), LifecycleError> {
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
        verify_file(&path, file)?;
        #[cfg(unix)]
        verify_executable(&path, file.executable)?;
        #[cfg(not(unix))]
        verify_executable(&path, file.executable);
    }
    Ok(())
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
// cannot open a directory with `File::open` and offers no directory `fsync`;
// durability there rests on per-file `sync_all` plus an atomic same-directory
// rename, which NTFS orders before the handle closes. Keep this a typed
// no-op so no caller can mistake it for a synced Unix directory.
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
/// POSIX rename replaces an existing destination atomically. Windows does not
/// expose that guarantee through `std::fs`; its destination must be removed
/// before rename, so callers still hold the lifecycle lock across this short
/// platform-specific replacement window.
#[cfg(unix)]
fn replace_file_atomic(temporary: &Path, destination: &Path) -> Result<(), LifecycleError> {
    fs::rename(temporary, destination).map_err(|_| LifecycleError::StagingFailed)
}

#[cfg(not(unix))]
fn replace_file_atomic(temporary: &Path, destination: &Path) -> Result<(), LifecycleError> {
    if destination.exists() {
        remove_file_if_present(destination).map_err(|_| LifecycleError::StagingFailed)?;
    }
    fs::rename(temporary, destination).map_err(|_| LifecycleError::StagingFailed)
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
    path: Option<PathBuf>,
}

impl LifecycleLock {
    fn acquire(root: &Path) -> Result<Self, LifecycleError> {
        fs::create_dir_all(root).map_err(|_| LifecycleError::UpdateLocked)?;
        let path = root.join("transaction.lock");
        for _ in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    writeln!(file, "{}", std::process::id())
                        .and_then(|()| file.sync_all())
                        .map_err(|_| LifecycleError::UpdateLocked)?;
                    return Ok(Self { path: Some(path) });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let pid = fs::read_to_string(&path)
                        .ok()
                        .and_then(|value| value.trim().parse::<usize>().ok());
                    let live = pid.is_some_and(process_is_live);
                    if live || remove_file_if_present(&path).is_err() {
                        return Err(LifecycleError::UpdateLocked);
                    }
                }
                Err(_) => return Err(LifecycleError::UpdateLocked),
            }
        }
        Err(LifecycleError::UpdateLocked)
    }

    fn release(mut self) -> Result<(), LifecycleError> {
        let Some(path) = self.path.take() else {
            return Ok(());
        };
        remove_file_if_present(&path).map_err(|_| LifecycleError::UninstallPendingCleanup)
    }
}

impl Drop for LifecycleLock {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = remove_file_if_present(&path);
        }
    }
}

fn process_is_live(pid: usize) -> bool {
    let mut system = System::new();
    let pid = Pid::from(pid);
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Hooks {
        fail_prepare: bool,
        fail_health: bool,
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

    fn release(root: &Path, generation: u64) -> (SignedReleaseManifest, SigningKey, PathBuf) {
        let bundle = root.join(format!("bundle-{generation}"));
        fs::create_dir_all(&bundle).expect("bundle");
        let desktop = format!("desktop-{generation}").into_bytes();
        let engine = format!("engine-{generation}").into_bytes();
        fs::write(bundle.join("axiusflow_desktop"), &desktop).expect("desktop");
        fs::write(bundle.join("axiusflow_engine"), &engine).expect("engine");
        let files = [
            (ReleaseFileRole::Desktop, "axiusflow_desktop", desktop),
            (ReleaseFileRole::Engine, "axiusflow_engine", engine),
        ]
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
            schema_version: MANIFEST_SCHEMA_VERSION,
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
        fs::write(bundle.join("axiusflow_engine"), b"tampered").expect("tamper bundle");
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
    fn interrupted_bundle_fails_closed_without_activation() {
        let root = temporary_root("interrupted-bundle");
        let (signed, key, bundle) = release(&root, 1);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        fs::remove_file(bundle.join("axiusflow_engine")).expect("drop one bundle file");
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
        fs::write(bundle.join("axiusflow_engine"), b"truncated").expect("truncate bundle file");
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
    fn successful_upgrade_leaves_one_matching_release_and_no_staging() {
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
        assert_eq!(count_entries(&root.join("install/versions")), Ok(1));
        assert!(!installer.update_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn successive_upgrades_leave_one_matching_release_and_no_staging() {
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
        for generation in [2_u64, 3] {
            let (next, _, next_bundle) = release(&root, generation);
            let next = sign_release_manifest(next.manifest, &key).expect("same release key");
            let outcome = installer
                .install(&next, &next_bundle, &Hooks::default())
                .expect("upgrade");
            assert_eq!(outcome.active.install_generation, generation);
        }
        assert_eq!(
            installer
                .active_release()
                .expect("active")
                .expect("one active release")
                .install_generation,
            3
        );
        assert_eq!(count_entries(&root.join("install/versions")), Ok(1));
        assert!(!installer.update_journal_exists());
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
        let engine = installer
            .release_directory(&active)
            .expect("release directory")
            .join("axiusflow_engine");
        fs::write(engine, b"post-install mutation").expect("mutate active engine");
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
        };
        let activated = UpdateJournal {
            state: UpdateState::Activated,
            candidate,
            previous: None,
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
        let previous = installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install")
            .active;
        let old_engine = installer
            .release_directory(&previous)
            .expect("old release directory")
            .join("axiusflow_engine");
        // FILE_SHARE_READ lets the pre-install audit read the old release
        // while still blocking its deletion, mirroring a locked executable.
        let lock = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&old_engine)
            .expect("hold old engine without delete sharing");
        let (second, _, second_bundle) = release(&root, 2);
        let second = sign_release_manifest(second.manifest, &key).expect("same key");
        assert_eq!(
            installer.install(&second, &second_bundle, &Hooks::default()),
            Err(LifecycleError::UpdatePendingCleanup)
        );
        assert_eq!(
            installer
                .active_release()
                .expect("active state")
                .expect("candidate pointer"),
            ActiveRelease {
                release_identity: second.manifest.release_identity.clone(),
                install_generation: second.manifest.install_generation,
                directory_name: format!(
                    "{:020}-{}",
                    second.manifest.install_generation, second.manifest.release_identity
                ),
            }
        );
        assert!(installer.update_journal_exists());
        assert_eq!(count_entries(&root.join("install/versions")), Ok(2));
        drop(lock);
        installer
            .recover(&Hooks::default())
            .expect("resume cleanup");
        assert_eq!(count_entries(&root.join("install/versions")), Ok(1));
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
        let (_, key, _) = release(&root, 1);
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
            "account-device-key-v1",
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
    fn uninstall_is_idempotent_and_removes_exact_roots_and_vault_keys() {
        let root = temporary_root("uninstall");
        let (_, key, _) = release(&root, 1);
        let install_root = root.join("install");
        let data_root = root.join("data");
        fs::create_dir_all(&install_root).expect("install root");
        fs::create_dir_all(&data_root).expect("data root");
        fs::write(data_root.join("history"), b"encrypted").expect("history");
        let installer =
            ReleaseInstaller::new(&install_root, key.verifying_key(), ReleasePolicy::native(0))
                .expect("installer");
        let inventory = InstallationInventory {
            schema_version: INVENTORY_SCHEMA_VERSION,
            install_root,
            data_roots: vec![data_root],
            cache_roots: Vec::new(),
            log_roots: Vec::new(),
            ipc_paths: Vec::new(),
            vault_entries: vec![
                VaultEntry {
                    service: "com.axiusflow.engine".to_string(),
                    key: "local-ipc-token-v1".to_string(),
                },
                VaultEntry {
                    service: "com.axiusflow.engine.history".to_string(),
                    key: "history-key-v1".to_string(),
                },
            ],
            registrations: vec!["engine-autostart".to_string()],
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
