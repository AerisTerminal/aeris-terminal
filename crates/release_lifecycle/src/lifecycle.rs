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

use aeris_platform_runtime::data_root::{
    DataRootError, native_data_root, native_legacy_data_root, validate_owned_root,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
#[cfg(any(test, feature = "producer"))]
use ed25519_dalek::{Signer as _, SigningKey};
use same_file::Handle as FileIdentity;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest as _, Sha256};

use crate::{LAUNCHER_PROTOCOL_VERSION, LiveStoreSchema};

pub const RELEASE_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const RELEASE_CHANNEL_SCHEMA_VERSION: u32 = 1;
pub const ROLLBACK_COMPATIBILITY_SCHEMA_VERSION: u32 = 1;
pub const ROLLBACK_COMPATIBILITY_FILENAME: &str = "rollback-compatibility.json";
/// Bump only when persisted local state is no longer backward-readable by the
/// immediately preceding release. Additive protobuf fields, preserved field
/// numbers/types/meaning, and permanently retired tags stay in the same epoch.
pub const CURRENT_STATE_COMPATIBILITY_EPOCH: u32 = 1;
const INVENTORY_SCHEMA_VERSION: u32 = 1;
/// Orphaned-entry batches kept under the install root's `orphaned/`; older ones are removed.
const MAXIMUM_ORPHAN_BATCHES: usize = 8;
// The base64url copy of the raw manifest (4/3 of these bytes) must still fit
// in the 1 MiB signed-manifest file and launcher channel-pointer bounds.
const MAXIMUM_MANIFEST_BYTES: usize = 512 * 1024;
const MAXIMUM_SIGNED_MANIFEST_FILE_BYTES: u64 = 1024 * 1024;
// Changing this prefix invalidates every published manifest signature. The signed message is
// this prefix and the SHA-256 of the manifest bytes, so it stays far below the 4 KB message
// bound of a cloud key service.
const RELEASE_MANIFEST_SIGNING_DOMAIN: &[u8] = b"AERIS_RELEASE_MANIFEST_V2\0";
const MAXIMUM_ROLLBACK_COMPATIBILITY_BYTES: u64 = 1024;
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
    pub minimum_launcher_protocol: u32,
    pub platform: String,
    pub architecture: String,
    pub files: Vec<ReleaseFile>,
    pub rollout: RolloutMetadata,
}

/// The exact signed manifest bytes, their Ed25519 signature and the manifest
/// they decode to.
///
/// The signature covers `AERIS_RELEASE_MANIFEST_V2\0` followed by the SHA-256
/// of the raw bytes, never a re-serialization, so a serde or field-order change
/// cannot alter what was signed. The bytes decode with `deny_unknown_fields`, and the
/// fields are private so the decoded manifest always matches the bytes. No
/// field is trusted until [`verify_release_manifest`] has checked the
/// signature.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    try_from = "SignedReleaseManifestWire",
    into = "SignedReleaseManifestWire"
)]
pub struct SignedReleaseManifest {
    manifest: ReleaseManifest,
    manifest_bytes: Vec<u8>,
    signature: String,
}

impl SignedReleaseManifest {
    #[must_use]
    pub const fn manifest(&self) -> &ReleaseManifest {
        &self.manifest
    }

    #[must_use]
    pub fn into_manifest(self) -> ReleaseManifest {
        self.manifest
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedReleaseManifestWire {
    manifest_b64url: String,
    signature: String,
}

impl TryFrom<SignedReleaseManifestWire> for SignedReleaseManifest {
    type Error = LifecycleError;

    fn try_from(wire: SignedReleaseManifestWire) -> Result<Self, Self::Error> {
        if wire.manifest_b64url.len() > MAXIMUM_MANIFEST_BYTES.div_ceil(3) * 4 {
            return Err(LifecycleError::InvalidManifest);
        }
        let manifest_bytes = URL_SAFE_NO_PAD
            .decode(&wire.manifest_b64url)
            .map_err(|_| LifecycleError::InvalidManifest)?;
        Ok(Self {
            manifest: decode_manifest_bytes(&manifest_bytes)?,
            manifest_bytes,
            signature: wire.signature,
        })
    }
}

impl From<SignedReleaseManifest> for SignedReleaseManifestWire {
    fn from(signed: SignedReleaseManifest) -> Self {
        Self {
            manifest_b64url: URL_SAFE_NO_PAD.encode(&signed.manifest_bytes),
            signature: signed.signature,
        }
    }
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
    pub launcher_protocol: u32,
    pub minimum_install_generation: u64,
    pub maximum_release_bytes: u64,
}

impl ReleasePolicy {
    #[must_use]
    pub fn native(minimum_install_generation: u64) -> Self {
        Self {
            platform: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            launcher_protocol: LAUNCHER_PROTOCOL_VERSION,
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
///
/// `max_store_schema` is the newest trading store schema the release opens. A
/// rollback is refused when the live store's schema is newer than the target's.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RollbackCompatibilityMetadata {
    pub schema_version: u32,
    pub state_compatibility_epoch: u32,
    pub max_store_schema: u32,
}

impl RollbackCompatibilityMetadata {
    /// The current epoch for a release whose trading store schema is `max_store_schema`.
    #[must_use]
    pub const fn for_store_schema(max_store_schema: u32) -> Self {
        Self {
            schema_version: ROLLBACK_COMPATIBILITY_SCHEMA_VERSION,
            state_compatibility_epoch: CURRENT_STATE_COMPATIBILITY_EPOCH,
            max_store_schema,
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

/// Versioned inventory of every local artifact owned by `Aeris`.
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
    /// Stops all verified `Aeris` process identities before uninstall.
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
    RollbackStoreSchemaNewer,
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
            Self::InvalidInventory => "Aeris ownership inventory is invalid",
            Self::UpdateLocked => "another lifecycle transaction is active",
            Self::StagingFailed => "release staging did not complete",
            Self::VerificationFailed => "staged release inventory verification failed",
            Self::ShutdownFailed => "owned processes did not stop cleanly",
            Self::HealthCheckFailed => "candidate release failed its readiness check",
            Self::RollbackFailed => "candidate rollback requires remediation",
            Self::RollbackStoreSchemaNewer => {
                "the retained release cannot open the trading data a newer release migrated"
            }
            Self::UpdatePendingCleanup => "update is pending superseded-file cleanup",
            Self::UninstallPendingCleanup => "uninstall is pending local-artifact cleanup",
            Self::ExternalArtifactRemaining => "an external Aeris artifact remains",
            Self::JournalCorrupt => "lifecycle recovery journal is invalid",
        })
    }
}

impl Error for LifecycleError {}

impl From<DataRootError> for LifecycleError {
    fn from(_: DataRootError) -> Self {
        Self::InvalidInventory
    }
}

/// One validated manifest, serialized once, waiting for the Ed25519 signature of
/// [`Self::signing_message`] from whichever signer holds the release key.
#[cfg(any(test, feature = "producer"))]
pub struct UnsignedReleaseManifest {
    manifest: ReleaseManifest,
    manifest_bytes: Vec<u8>,
    message: Vec<u8>,
}

#[cfg(any(test, feature = "producer"))]
impl UnsignedReleaseManifest {
    /// # Errors
    /// Returns an error if the manifest cannot be validated or serialized.
    pub fn new(manifest: ReleaseManifest) -> Result<Self, LifecycleError> {
        validate_manifest_shape(&manifest)?;
        let manifest_bytes =
            serde_json::to_vec(&manifest).map_err(|_| LifecycleError::InvalidManifest)?;
        let message = manifest_signing_message(&manifest_bytes)?;
        Ok(Self {
            manifest,
            manifest_bytes,
            message,
        })
    }

    #[must_use]
    pub fn signing_message(&self) -> &[u8] {
        &self.message
    }

    /// The signed manifest. Nothing checks the signature here; a publisher verifies the
    /// result with [`verify_release_manifest`] exactly as a launcher does.
    #[must_use]
    pub fn with_signature(self, signature: &Signature) -> SignedReleaseManifest {
        SignedReleaseManifest {
            manifest: self.manifest,
            manifest_bytes: self.manifest_bytes,
            signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
        }
    }
}

/// Signs one validated manifest with a local key.
///
/// # Errors
/// Returns an error if the manifest cannot be validated or serialized.
#[cfg(any(test, feature = "producer"))]
pub fn sign_release_manifest(
    manifest: ReleaseManifest,
    key: &SigningKey,
) -> Result<SignedReleaseManifest, LifecycleError> {
    let unsigned = UnsignedReleaseManifest::new(manifest)?;
    let signature = key.sign(unsigned.signing_message());
    Ok(unsigned.with_signature(&signature))
}

/// Verifies the signature over the exact manifest bytes first, then the
/// manifest shape, bounds, platform, architecture, launcher protocol and
/// downgrade policy.
///
/// # Errors
/// Returns a redacted validation category without exposing manifest payloads.
pub fn verify_release_manifest(
    signed: &SignedReleaseManifest,
    key: &VerifyingKey,
    policy: &ReleasePolicy,
) -> Result<(), LifecycleError> {
    let message = manifest_signing_message(&signed.manifest_bytes)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(&signed.signature)
        .map_err(|_| LifecycleError::InvalidSignature)?;
    let signature = Signature::from_slice(&bytes).map_err(|_| LifecycleError::InvalidSignature)?;
    key.verify(&message, &signature)
        .map_err(|_| LifecycleError::InvalidSignature)?;
    validate_manifest_shape(&signed.manifest)?;
    if signed.manifest.platform != policy.platform
        || signed.manifest.architecture != policy.architecture
    {
        return Err(LifecycleError::IncompatibleRelease);
    }
    if policy.launcher_protocol < signed.manifest.minimum_launcher_protocol {
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

fn manifest_signing_message(manifest_bytes: &[u8]) -> Result<Vec<u8>, LifecycleError> {
    if manifest_bytes.is_empty() || manifest_bytes.len() > MAXIMUM_MANIFEST_BYTES {
        return Err(LifecycleError::InvalidManifest);
    }
    let mut message = Vec::with_capacity(RELEASE_MANIFEST_SIGNING_DOMAIN.len() + 32);
    message.extend_from_slice(RELEASE_MANIFEST_SIGNING_DOMAIN);
    message.extend_from_slice(&Sha256::digest(manifest_bytes));
    Ok(message)
}

fn decode_manifest_bytes(manifest_bytes: &[u8]) -> Result<ReleaseManifest, LifecycleError> {
    if manifest_bytes.is_empty() || manifest_bytes.len() > MAXIMUM_MANIFEST_BYTES {
        return Err(LifecycleError::InvalidManifest);
    }
    serde_json::from_slice(manifest_bytes).map_err(|_| LifecycleError::InvalidManifest)
}

fn validate_manifest_shape(manifest: &ReleaseManifest) -> Result<(), LifecycleError> {
    if manifest.schema_version != RELEASE_MANIFEST_SCHEMA_VERSION
        || manifest.install_generation == 0
        || !valid_identifier(&manifest.release_identity, 128)
        || !valid_identifier(&manifest.channel, 32)
        || manifest.minimum_launcher_protocol == 0
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
    /// The candidate passed readiness on a copy of the user's data; the active pointer may
    /// not have moved yet. Recovery finishes the update without running candidate code.
    ReadinessPassed,
    Cleanup,
    RollingBack,
}

impl UpdateState {
    const ALL: [Self; 6] = [
        Self::Preparing,
        Self::Staged,
        Self::ProcessesStopped,
        Self::ReadinessPassed,
        Self::Cleanup,
        Self::RollingBack,
    ];
}

fn update_journal_name(state: UpdateState) -> &'static str {
    match state {
        UpdateState::Preparing => "update-0-preparing.json",
        UpdateState::Staged => "update-1-staged.json",
        UpdateState::ProcessesStopped => "update-2-processes-stopped.json",
        UpdateState::ReadinessPassed => "update-3-readiness-passed.json",
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
        let (lifecycle_root, lifecycle_lock_path) = lifecycle_paths(&install_root)?;
        Ok(Self {
            install_root,
            lifecycle_root,
            lifecycle_lock_path,
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
            if aeris_platform_runtime::fs::staged_temporary_destination_name(
                entry.file_name().as_ref(),
            )
            .is_some()
            {
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
    /// inventory are reverified before any active pointer changes, and the target
    /// must open `live_store`. The failed newer release is removed after the older
    /// release becomes active.
    ///
    /// # Errors
    /// Returns an error if no verified retained release exists, it cannot open the
    /// live trading store, another lifecycle transaction is pending, the active
    /// process cannot stop, or the journaled pointer/cleanup transaction cannot
    /// complete safely.
    pub fn rollback_to_retained_known_good<H: LifecycleHooks>(
        &self,
        hooks: &H,
        live_store: LiveStoreSchema,
    ) -> Result<ActiveRelease, LifecycleError> {
        let _lock = LifecycleLock::acquire(&self.lifecycle_lock_path)?;
        if self.lifecycle_root.join("uninstall.json").exists() {
            return Err(LifecycleError::UninstallPendingCleanup);
        }
        if self.read_update_journal()?.is_some() || self.read_retained_rollback_journal()?.is_some()
        {
            return Err(LifecycleError::UpdatePendingCleanup);
        }
        let (failed, target) = self.retained_rollback(live_store)?;
        let journal = RetainedRollbackJournal {
            state: RetainedRollbackState::Preparing,
            failed,
            target: target.clone(),
        };
        self.write_retained_rollback_journal(&journal)?;
        self.resume_retained_rollback(journal, hooks)?;
        Ok(target)
    }

    /// Returns the retained release a rollback from the active release would
    /// select, after the checks [`Self::rollback_to_retained_known_good`] runs
    /// before it changes anything. A caller uses it to decide on a rollback
    /// before recording that decision.
    ///
    /// # Errors
    /// Returns the error the rollback itself would return for its target.
    pub fn verify_retained_rollback(
        &self,
        live_store: LiveStoreSchema,
    ) -> Result<ActiveRelease, LifecycleError> {
        self.retained_rollback(live_store).map(|(_, target)| target)
    }

    fn retained_rollback(
        &self,
        live_store: LiveStoreSchema,
    ) -> Result<(ActiveRelease, ActiveRelease), LifecycleError> {
        let failed = self
            .audit_active_release()?
            .ok_or(LifecycleError::RollbackFailed)?;
        let target = self
            .retained_known_good_release()?
            .ok_or(LifecycleError::RollbackFailed)?;
        let (failed_compatibility, target_compatibility) =
            self.verify_retained_rollback_compatibility(&failed, &target)?;
        if !live_store.allows_rollback(
            failed_compatibility.max_store_schema,
            target_compatibility.max_store_schema,
        ) {
            return Err(LifecycleError::RollbackStoreSchemaNewer);
        }
        Ok((failed, target))
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
        if count_release_record_entries(&self.lifecycle_root.join("active"))? != 1
            || count_release_record_entries(&self.lifecycle_root.join("manifests"))?
                != expected_release_count
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
        // Readiness runs the candidate on a copy of the user's data before the active
        // pointer moves, and its outcome is journaled first, so recovery never has to run
        // candidate code to decide whether the update stands.
        if hooks.health_check(&candidate).is_err() {
            journal.state = UpdateState::RollingBack;
            self.write_update_journal(&journal)?;
            self.restore_previous_after_candidate_failure(&journal)?;
            self.remove_update_journal()?;
            return Err(LifecycleError::HealthCheckFailed);
        }
        journal.state = UpdateState::ReadinessPassed;
        self.write_update_journal(&journal)?;
        self.commit_pointer(&candidate)?;
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

    /// Resumes or rolls back an interrupted transaction deterministically, without running
    /// candidate code, then moves entries no release record owns into `orphaned/`.
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
        match (update_journal, rollback_journal) {
            (Some(_), Some(_)) => return Err(LifecycleError::JournalCorrupt),
            (None, Some(journal)) => self.resume_retained_rollback(journal, hooks)?,
            (Some(journal), None) => self.resume_update(journal)?,
            (None, None) => {}
        }
        self.move_orphaned_entries()
    }

    fn resume_update(&self, mut journal: UpdateJournal) -> Result<(), LifecycleError> {
        let candidate_root = self.version_path(&journal.candidate)?;
        match journal.state {
            UpdateState::Preparing | UpdateState::Staged | UpdateState::ProcessesStopped => {
                // Readiness had not passed; the candidate ran at most on a copy.
                remove_owned_path(&candidate_root)
                    .map_err(|_| LifecycleError::UpdatePendingCleanup)?;
                self.remove_pointer(&journal.candidate)?;
                self.remove_manifest(&journal.candidate)?;
                self.remove_release_record_temporaries(&journal.candidate)?;
            }
            UpdateState::ReadinessPassed => {
                // A candidate that changed after readiness has never run against the
                // user's data, so it is discarded rather than activated.
                if self
                    .audit_release(&journal.candidate, &self.policy)
                    .is_err()
                {
                    aeris_observability::diagnostic!(
                        "Aeris lifecycle discarded release {} because it changed after its readiness check",
                        journal.candidate.install_generation
                    );
                    journal.state = UpdateState::RollingBack;
                    self.write_update_journal(&journal)?;
                    self.restore_previous_after_candidate_failure(&journal)?;
                } else {
                    self.commit_pointer(&journal.candidate)?;
                    journal.state = UpdateState::Cleanup;
                    self.write_update_journal(&journal)?;
                    self.finish_successful_update(&journal)?;
                    self.audit_single_active(&journal.candidate)?;
                }
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

    /// Moves entries that neither the active nor the retained release owns out of
    /// `versions/`, `active/` and `manifests/` into `orphaned/<time>/`, so a stray entry
    /// does not block launch. Only the newest [`MAXIMUM_ORPHAN_BATCHES`] batches are kept.
    fn move_orphaned_entries(&self) -> Result<(), LifecycleError> {
        let Some(active) = self.active_release()? else {
            return Ok(());
        };
        let retained = self.read_retained_known_good()?;
        let owned = std::iter::once(&active)
            .chain(retained.as_ref())
            .collect::<Vec<_>>();
        let directories = owned
            .iter()
            .map(|release| release.directory_name.clone())
            .collect::<BTreeSet<_>>();
        let records = owned
            .iter()
            .map(|release| pointer_name(release))
            .collect::<BTreeSet<_>>();
        let active_record = BTreeSet::from([pointer_name(&active)]);
        let mut orphans = Vec::new();
        for (category, directory, known, records_only) in [
            (
                "versions",
                self.install_root.join("versions"),
                &directories,
                false,
            ),
            (
                "active",
                self.lifecycle_root.join("active"),
                &active_record,
                true,
            ),
            (
                "manifests",
                self.lifecycle_root.join("manifests"),
                &records,
                true,
            ),
        ] {
            for path in unknown_entries(&directory, known, records_only)? {
                orphans.push((category, path));
            }
        }
        if orphans.is_empty() {
            return Ok(());
        }
        let batch = self.create_orphan_batch()?;
        for (category, source) in &orphans {
            let destination = batch.join(category);
            fs::create_dir_all(&destination).map_err(|_| LifecycleError::UpdatePendingCleanup)?;
            let name = source
                .file_name()
                .ok_or(LifecycleError::UpdatePendingCleanup)?;
            aeris_platform_runtime::fs::move_file(source, &destination.join(name))
                .map_err(|_| LifecycleError::UpdatePendingCleanup)?;
        }
        aeris_observability::diagnostic!(
            "Aeris lifecycle moved {} orphaned install entries aside",
            orphans.len()
        );
        self.prune_orphan_batches()
    }

    fn orphaned_root(&self) -> PathBuf {
        self.install_root.join("orphaned")
    }

    fn create_orphan_batch(&self) -> Result<PathBuf, LifecycleError> {
        let root = self.orphaned_root();
        fs::create_dir_all(&root).map_err(|_| LifecycleError::UpdatePendingCleanup)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        // Zero-padded milliseconds sort in creation order; a same-millisecond batch takes
        // the next free name.
        for offset in 0..16_u128 {
            let batch = root.join(format!("{:020}", now.saturating_add(offset)));
            match fs::create_dir(&batch) {
                Ok(()) => return Ok(batch),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(LifecycleError::UpdatePendingCleanup),
            }
        }
        Err(LifecycleError::UpdatePendingCleanup)
    }

    fn prune_orphan_batches(&self) -> Result<(), LifecycleError> {
        let mut batches = fs::read_dir(self.orphaned_root())
            .map_err(|_| LifecycleError::UpdatePendingCleanup)?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|_| LifecycleError::UpdatePendingCleanup)
            })
            .collect::<Result<Vec<_>, _>>()?;
        batches.sort();
        let excess = batches.len().saturating_sub(MAXIMUM_ORPHAN_BATCHES);
        for batch in &batches[..excess] {
            remove_owned_path(batch).map_err(|_| LifecycleError::UpdatePendingCleanup)?;
        }
        Ok(())
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
            || compatibility.max_store_schema == 0
        {
            return Err(LifecycleError::RollbackFailed);
        }
        Ok(compatibility)
    }

    fn verify_retained_rollback_compatibility(
        &self,
        failed: &ActiveRelease,
        target: &ActiveRelease,
    ) -> Result<(RollbackCompatibilityMetadata, RollbackCompatibilityMetadata), LifecycleError>
    {
        let failed = self.rollback_compatibility_metadata(failed)?;
        let target = self.rollback_compatibility_metadata(target)?;
        if failed.state_compatibility_epoch != target.state_compatibility_epoch {
            return Err(LifecycleError::RollbackFailed);
        }
        Ok((failed, target))
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
            aeris_platform_runtime::fs::remove_stale_temporaries(&directory, &name)
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
            // The candidate never ran against the user's data (readiness uses a copy), so
            // the previous release can always be selected again.
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
        // The store schema gate ran before the journal was written, and no
        // release runs while the journal is pending, so it is not re-read here.
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
        if count_release_record_entries(&active_root)? != 1 {
            return Err(LifecycleError::UpdatePendingCleanup);
        }
        let retained = self.retained_known_good_release()?;
        let expected_release_count = usize::from(retained.is_some()).saturating_add(1);
        if count_release_record_entries(&self.lifecycle_root.join("manifests"))?
            != expected_release_count
        {
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
}

/// Uninstall owner for one installation root. Removing local data never
/// verifies release signatures, so it needs no release verifying key and works
/// even when the active release cannot be audited.
pub struct ReleaseUninstaller {
    install_root: PathBuf,
    lifecycle_root: PathBuf,
    lifecycle_lock_path: PathBuf,
}

impl ReleaseUninstaller {
    /// Creates an uninstaller for one exact absolute installation root.
    ///
    /// # Errors
    /// Rejects broad, relative, or symlinked ownership roots.
    pub fn new(install_root: impl Into<PathBuf>) -> Result<Self, LifecycleError> {
        let install_root = install_root.into();
        let (lifecycle_root, lifecycle_lock_path) = lifecycle_paths(&install_root)?;
        Ok(Self {
            install_root,
            lifecycle_root,
            lifecycle_lock_path,
        })
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

    /// Removes the installed application (its release versions, launcher and lifecycle
    /// records) and disables its registrations, keeping every data, cache, log and
    /// legacy root and every vault entry.
    ///
    /// # Errors
    /// Refuses while a full uninstall is pending, since that request must finish first.
    /// A failed removal leaves the lifecycle records, so the next attempt resumes it.
    pub fn remove_application<H: LifecycleHooks>(
        &self,
        inventory: &InstallationInventory,
        hooks: &H,
    ) -> Result<UninstallOutcome, LifecycleError> {
        self.validate_inventory(inventory)?;
        let lock = LifecycleLock::acquire(&self.lifecycle_lock_path)?;
        if self.lifecycle_root.join("uninstall.json").exists() {
            return Err(LifecycleError::UninstallPendingCleanup);
        }
        hooks
            .disable_registrations(&inventory.registrations)
            .map_err(|_| LifecycleError::UninstallPendingCleanup)?;
        hooks
            .stop_owned_processes()
            .map_err(|_| LifecycleError::UninstallPendingCleanup)?;
        remove_owned_path(&self.install_root)
            .map_err(|_| LifecycleError::UninstallPendingCleanup)?;
        if self.install_root.exists() {
            return Err(LifecycleError::UninstallPendingCleanup);
        }
        remove_lifecycle_root_after_uninstall(&self.lifecycle_root)
            .map_err(|_| LifecycleError::UninstallPendingCleanup)?;
        lock.release()?;
        Ok(UninstallOutcome {
            removed_roots: 2,
            removed_vault_keys: 0,
        })
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

/// Returns the lifecycle journal directory and lock path beside one validated
/// installation root.
fn lifecycle_paths(install_root: &Path) -> Result<(PathBuf, PathBuf), LifecycleError> {
    validate_owned_root(install_root)?;
    let parent = install_root
        .parent()
        .ok_or(LifecycleError::InvalidInventory)?;
    let name = install_root
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| valid_identifier(value, 64))
        .ok_or(LifecycleError::InvalidInventory)?;
    Ok((
        parent.join(format!(".{name}-lifecycle")),
        parent.join(format!(".{name}-lifecycle.lock")),
    ))
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

/// Resolves the stable per-user `Aeris` installation root used by the
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
            .map(|root| root.join("Programs/Aeris"))
            .ok_or(LifecycleError::InvalidInventory)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|root| root.join("Applications/Aeris"))
            .ok_or(LifecycleError::InvalidInventory)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|root| PathBuf::from(root).join(".local/share"))
            })
            .map(|root| root.join("aeris/app"))
            .ok_or(LifecycleError::InvalidInventory)
    }
}

fn native_legacy_install_root() -> Result<PathBuf, LifecycleError> {
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

fn native_legacy_cache_root() -> Result<PathBuf, LifecycleError> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|root| root.join("Axiusflow/cache"))
            .ok_or(LifecycleError::InvalidInventory)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|root| root.join("Library/Caches/Axiusflow"))
            .ok_or(LifecycleError::InvalidInventory)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|root| PathBuf::from(root).join(".cache")))
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
                .map(|root| root.join("Aeris/cache"))
                .ok_or(LifecycleError::InvalidInventory)?
        }
        #[cfg(target_os = "macos")]
        {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|root| root.join("Library/Caches/Aeris"))
                .ok_or(LifecycleError::InvalidInventory)?
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|root| PathBuf::from(root).join(".cache")))
                .map(|root| root.join("aeris"))
                .ok_or(LifecycleError::InvalidInventory)?
        }
    };
    let logs = data.join("logs");
    let legacy_install = native_legacy_install_root()?;
    let legacy_install_parent = legacy_install
        .parent()
        .map(Path::to_path_buf)
        .ok_or(LifecycleError::InvalidInventory)?;
    let legacy_cleanup_roots = vec![
        native_legacy_data_root("TradingPlot")?,
        native_legacy_data_root("Axiusflow")?,
        native_legacy_cache_root()?,
        legacy_install,
        legacy_install_parent.join(".Axiusflow-lifecycle"),
        legacy_install_parent.join(".Axiusflow-lifecycle.lock"),
        legacy_install_parent.join("Axiusflow-Uninstall"),
    ];
    let registrations = vec!["start-menu:Aeris".to_string()];
    Ok(InstallationInventory {
        schema_version: INVENTORY_SCHEMA_VERSION,
        install_root,
        data_roots: vec![data],
        cache_roots: vec![cache],
        log_roots: vec![logs],
        legacy_cleanup_roots,
        vault_entries: vec![
            VaultEntry {
                service: "com.aeris.terminal".to_string(),
                key: "provider-rithmic-login-v2".to_string(),
            },
            // Earlier builds kept a Rithmic Test-only login under this key.
            VaultEntry {
                service: "com.aeris.terminal".to_string(),
                key: "provider-rithmic-test-default-v1".to_string(),
            },
            VaultEntry {
                service: "com.aeris.account".to_string(),
                key: "account-refresh-default-v1".to_string(),
            },
            VaultEntry {
                service: "com.aeris.account".to_string(),
                key: "account-entitlement-lease-v1".to_string(),
            },
            VaultEntry {
                service: "com.aeris.account".to_string(),
                key: "account-entitlement-directory-v1".to_string(),
            },
            VaultEntry {
                service: "com.aeris.account".to_string(),
                key: "account-device-key-v1".to_string(),
            },
            VaultEntry {
                service: "com.aeris.account".to_string(),
                key: "account-profile-v1".to_string(),
            },
        ],
        registrations,
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
    #[cfg(target_os = "windows")]
    let verify_native_executable = verify_installed_executable_authenticode;
    // Authenticode is the only native publisher check implemented; other
    // platforms rely on the signed manifest digests verified for every file.
    #[cfg(not(target_os = "windows"))]
    let verify_native_executable = |_: &Path, _: &ReleaseFile| Ok(());
    verify_candidate_inventory_with(root, expected, verify_native_executable)
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

#[cfg(target_os = "windows")]
fn verify_installed_executable_authenticode(
    path: &Path,
    expected: &ReleaseFile,
) -> Result<(), LifecycleError> {
    verify_installed_executable_authenticode_with(
        path,
        expected,
        embedded_authenticode_thumbprint(),
    )
}

#[cfg(target_os = "windows")]
fn verify_installed_executable_authenticode_with(
    path: &Path,
    expected: &ReleaseFile,
    expected_signer: Option<&[u8; 20]>,
) -> Result<(), LifecycleError> {
    if !expected.executable
        || !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return Ok(());
    }
    let Some(signer) = expected_signer else {
        // Only a development build lacks the signer: build.rs refuses a
        // Windows release configuration without it, so every release build's
        // installed .exe audit fails closed on any trust/signature mismatch.
        return Ok(());
    };
    verify_windows_publisher(path, signer)
}

#[cfg(target_os = "windows")]
fn embedded_authenticode_thumbprint() -> Option<&'static [u8; 20]> {
    crate::release_config().map(|config| &config.authenticode_thumbprint)
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
    verify_windows_publisher(
        path,
        embedded_authenticode_thumbprint().ok_or(LifecycleError::VerificationFailed)?,
    )
}

#[cfg(target_os = "windows")]
fn verify_windows_publisher(path: &Path, signer: &[u8; 20]) -> Result<(), LifecycleError> {
    aeris_platform_runtime::trust::verify_publisher(path, signer).map_err(|error| {
        aeris_observability::diagnostic!(
            "Aeris lifecycle: an installed executable failed Authenticode verification: {error}"
        );
        LifecycleError::VerificationFailed
    })
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
    let destination = directory.join(name);
    aeris_platform_runtime::fs::write_file_durably(&destination, &bytes)
        .map_err(|_| LifecycleError::StagingFailed)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, LifecycleError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| LifecycleError::JournalCorrupt)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAXIMUM_SIGNED_MANIFEST_FILE_BYTES
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

/// Entries of `directory` whose names are not in `known`; `records_only` skips staged
/// temporaries, which their release-record cleanup removes.
fn unknown_entries(
    directory: &Path,
    known: &BTreeSet<String>,
    records_only: bool,
) -> Result<Vec<PathBuf>, LifecycleError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(LifecycleError::UpdatePendingCleanup),
    };
    let mut unknown = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| LifecycleError::UpdatePendingCleanup)?;
        let name = entry.file_name();
        if records_only
            && aeris_platform_runtime::fs::staged_temporary_destination_name(name.as_ref())
                .is_some()
        {
            continue;
        }
        if !name.to_str().is_some_and(|name| known.contains(name)) {
            unknown.push(entry.path());
        }
    }
    Ok(unknown)
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

fn count_release_record_entries(path: &Path) -> Result<usize, LifecycleError> {
    fs::read_dir(path)
        .map_err(|_| LifecycleError::UpdatePendingCleanup)?
        .try_fold(0_usize, |count, entry| {
            let entry = entry.map_err(|_| LifecycleError::UpdatePendingCleanup)?;
            Ok(count
                + usize::from(
                    aeris_platform_runtime::fs::staged_temporary_destination_name(
                        entry.file_name().as_ref(),
                    )
                    .is_none(),
                ))
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

    const LIFECYCLE_LOCK_CHILD_PATH_ENV: &str = "AERIS_TEST_LIFECYCLE_LOCK_PATH";

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
            "aeris-lifecycle-{name}-{}-{}",
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
        let _ = aeris_observability::flush_diagnostics();
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
        fs::write(bundle.join("aeris_desktop"), &desktop).expect("desktop");
        fs::write(bundle.join("aeris_launcher"), &launcher).expect("launcher");
        if let Some(bytes) = &rollback_compatibility {
            fs::write(bundle.join(ROLLBACK_COMPATIBILITY_FILENAME), bytes)
                .expect("rollback compatibility");
        }
        let mut files = vec![
            (ReleaseFileRole::Desktop, "aeris_desktop", desktop),
            (ReleaseFileRole::RuntimeAsset, "aeris_launcher", launcher),
        ];
        if let Some(bytes) = rollback_compatibility {
            files.push((
                ReleaseFileRole::RuntimeAsset,
                ROLLBACK_COMPATIBILITY_FILENAME,
                bytes,
            ));
        }
        let mut files = files
            .into_iter()
            .map(|(role, path, bytes)| ReleaseFile {
                role,
                path: path.to_string(),
                url: format!("https://releases.aeris.test/{generation}/{path}"),
                size: bytes.len() as u64,
                sha256: URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)),
                executable: true,
            })
            .collect::<Vec<_>>();
        // Manifest validation requires path-sorted inventories. Under the old
        // product name the desktop/launcher entries happened to sort before
        // rollback-compatibility.json; the Aeris prefix does not.
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let manifest = ReleaseManifest {
            schema_version: RELEASE_MANIFEST_SCHEMA_VERSION,
            release_identity: format!("release-{generation}"),
            install_generation: generation,
            channel: "stable".to_string(),
            minimum_launcher_protocol: LAUNCHER_PROTOCOL_VERSION,
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
                serde_json::to_vec(&RollbackCompatibilityMetadata::for_store_schema(30))
                    .expect("rollback compatibility encodes"),
            ),
        )
    }

    fn release_with_store_schema(
        root: &Path,
        generation: u64,
        max_store_schema: u32,
    ) -> (SignedReleaseManifest, SigningKey, PathBuf) {
        release_with_rollback_bytes(
            root,
            generation,
            Some(
                serde_json::to_vec(&RollbackCompatibilityMetadata::for_store_schema(
                    max_store_schema,
                ))
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
                    max_store_schema: 30,
                })
                .expect("rollback compatibility encodes"),
            ),
        )
    }

    #[cfg(windows)]
    #[test]
    fn installed_exe_inventory_requires_the_embedded_publisher_signature() {
        let root = temporary_root("authenticode-inventory");
        let executable = root.join("fixture.exe");
        let bytes = b"unsigned-fixture";
        fs::write(&executable, bytes).expect("fixture executable");
        let expected = ReleaseFile {
            role: ReleaseFileRole::Desktop,
            path: "fixture.exe".to_string(),
            url: "https://releases.aeris.test/fixture.exe".to_string(),
            size: bytes.len() as u64,
            sha256: URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)),
            executable: true,
        };
        let signer = [0x5a_u8; 20];
        assert_eq!(
            verify_candidate_inventory_with(
                &root,
                std::slice::from_ref(&expected),
                |path, file| {
                    verify_installed_executable_authenticode_with(path, file, Some(&signer))
                }
            ),
            Err(LifecycleError::VerificationFailed),
            "an unsigned installed executable fails a release build's audit"
        );
        verify_candidate_inventory_with(&root, std::slice::from_ref(&expected), |path, file| {
            verify_installed_executable_authenticode_with(path, file, None)
        })
        .expect("a development build without a signer audits digests only");
        let data_file = ReleaseFile {
            executable: false,
            ..expected
        };
        verify_installed_executable_authenticode_with(&executable, &data_file, Some(&signer))
            .expect("non-executable release files carry no Authenticode signature");
        let _ = remove_owned_path(&root);
    }

    fn with_manifest_bytes(
        signed: &SignedReleaseManifest,
        manifest_bytes: &[u8],
    ) -> Result<SignedReleaseManifest, serde_json::Error> {
        serde_json::from_value(serde_json::json!({
            "manifest_b64url": URL_SAFE_NO_PAD.encode(manifest_bytes),
            "signature": signed.signature,
        }))
    }

    #[test]
    fn tampered_manifest_and_bundle_fail_closed() {
        let root = temporary_root("tamper");
        let (signed, key, _bundle) = release(&root, 1);
        let mut manifest = signed.manifest.clone();
        manifest.channel = "preview".to_string();
        let tampered_bytes = serde_json::to_vec(&manifest).expect("tampered manifest encodes");
        let signed = with_manifest_bytes(&signed, &tampered_bytes).expect("tampered bytes decode");
        assert_eq!(signed.manifest().channel, "preview");
        assert_eq!(
            verify_release_manifest(&signed, &key.verifying_key(), &ReleasePolicy::native(0)),
            Err(LifecycleError::InvalidSignature)
        );
        let (signed, key, bundle) = release(&root, 2);
        fs::write(bundle.join("aeris_desktop"), b"tampered").expect("tamper bundle");
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
    fn signature_is_checked_over_raw_bytes_before_the_manifest_shape() {
        let root = temporary_root("signature-order");
        let (signed, key, _bundle) = release(&root, 1);
        let mut manifest = signed.manifest;
        manifest.files.insert(
            1,
            ReleaseFile {
                role: ReleaseFileRole::Engine,
                path: "aeris_engine".to_string(),
                url: "https://releases.aeris.test/1/aeris_engine".to_string(),
                size: 6,
                sha256: URL_SAFE_NO_PAD.encode(Sha256::digest(b"engine")),
                executable: true,
            },
        );
        let manifest_bytes = serde_json::to_vec(&manifest).expect("engine manifest encodes");
        let message = manifest_signing_message(&manifest_bytes).expect("bounded manifest");
        let engine = SignedReleaseManifest {
            manifest,
            manifest_bytes,
            signature: URL_SAFE_NO_PAD.encode(key.sign(&message).to_bytes()),
        };

        assert_eq!(
            verify_release_manifest(&engine, &key.verifying_key(), &ReleasePolicy::native(0)),
            Err(LifecycleError::InvalidManifest),
            "an authentic manifest with a retired engine file fails its shape check"
        );
        let other_key = SigningKey::from_bytes(&[0xa5; 32]);
        assert_eq!(
            verify_release_manifest(
                &engine,
                &other_key.verifying_key(),
                &ReleasePolicy::native(0)
            ),
            Err(LifecycleError::InvalidSignature),
            "the signature is checked before any manifest field"
        );
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn manifest_signature_vector_covers_the_domain_and_exact_bytes() {
        const MANIFEST: &str = concat!(
            r#"{"schema_version":1,"release_identity":"release-1","install_generation":1,"#,
            r#""channel":"stable","minimum_launcher_protocol":1,"platform":"windows","#,
            r#""architecture":"x86_64","files":[{"role":"desktop","path":"aeris_desktop","#,
            r#""url":"https://releases.aeris.test/1/aeris_desktop","size":7,"#,
            r#""sha256":"AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE","executable":true}],"#,
            r#""rollout":{"cohort":"all","percentage":100}}"#
        );
        const SIGNATURE: &str = "bVNeORWM8P2PNFmfKysRPtSfaFGvG852pEYasRXun5U1D9dDrVtOpnF2skdoCq1L33F9CaDWQCNWiIdmEc18CQ";
        assert_eq!(
            RELEASE_MANIFEST_SIGNING_DOMAIN,
            b"AERIS_RELEASE_MANIFEST_V2\0"
        );
        let key = SigningKey::from_bytes(&[7; 32]);
        let message = manifest_signing_message(MANIFEST.as_bytes()).expect("bounded manifest");
        let mut expected = RELEASE_MANIFEST_SIGNING_DOMAIN.to_vec();
        expected.extend_from_slice(&Sha256::digest(MANIFEST.as_bytes()));
        assert_eq!(
            message, expected,
            "the message is the domain and the bytes' SHA-256"
        );
        assert_eq!(
            URL_SAFE_NO_PAD.encode(key.sign(&message).to_bytes()),
            SIGNATURE
        );
        assert_ne!(
            URL_SAFE_NO_PAD.encode(key.sign(MANIFEST.as_bytes()).to_bytes()),
            SIGNATURE,
            "the signature is domain separated"
        );
        let wire = serde_json::json!({
            "manifest_b64url": URL_SAFE_NO_PAD.encode(MANIFEST),
            "signature": SIGNATURE,
        });
        let signed: SignedReleaseManifest =
            serde_json::from_value(wire.clone()).expect("vector decodes");
        let policy = ReleasePolicy {
            platform: "windows".to_string(),
            architecture: "x86_64".to_string(),
            launcher_protocol: LAUNCHER_PROTOCOL_VERSION,
            minimum_install_generation: 0,
            maximum_release_bytes: MAXIMUM_RELEASE_BYTES,
        };
        verify_release_manifest(&signed, &key.verifying_key(), &policy)
            .expect("published bytes keep verifying");
        assert_eq!(signed.manifest().release_identity, "release-1");
        assert_eq!(
            serde_json::to_value(&signed).expect("signed manifest encodes"),
            wire,
            "the exact signed bytes round-trip unchanged"
        );
    }

    #[test]
    fn signed_manifest_rejects_unknown_fields_inside_and_outside_the_signature() {
        let root = temporary_root("unknown-fields");
        let (signed, key, _bundle) = release(&root, 1);
        let mut value = serde_json::to_value(&signed.manifest).expect("manifest encodes");
        value["unsigned_extension"] = serde_json::json!(true);
        let manifest_bytes = serde_json::to_vec(&value).expect("extended manifest encodes");
        let message = manifest_signing_message(&manifest_bytes).expect("bounded manifest");
        let wire = serde_json::json!({
            "manifest_b64url": URL_SAFE_NO_PAD.encode(&manifest_bytes),
            "signature": URL_SAFE_NO_PAD.encode(key.sign(&message).to_bytes()),
        });
        assert!(
            serde_json::from_value::<SignedReleaseManifest>(wire).is_err(),
            "a signed manifest field this release does not know is refused"
        );

        let mut wire = serde_json::to_value(&signed).expect("signed manifest encodes");
        wire["manifest"] = serde_json::to_value(&signed.manifest).expect("manifest encodes");
        assert!(
            serde_json::from_value::<SignedReleaseManifest>(wire).is_err(),
            "an envelope field outside the signature is refused"
        );
        let oversized = vec![b' '; MAXIMUM_MANIFEST_BYTES + 1];
        assert!(with_manifest_bytes(&signed, &oversized).is_err());
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
        fs::remove_file(bundle.join("aeris_launcher")).expect("drop one bundle file");
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
        fs::write(bundle.join("aeris_desktop"), b"truncated").expect("truncate bundle file");
        assert_eq!(
            installer.install(&signed, &bundle, &Hooks::default()),
            Err(LifecycleError::VerificationFailed)
        );
        assert_eq!(installer.active_release().expect("active state"), None);
        assert!(!installer.update_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn release_minimum_launcher_protocol_is_enforced_after_signature_verification() {
        let root = temporary_root("launcher-protocol");
        let (signed, key, _) = release(&root, 1);
        let mut manifest = signed.manifest;
        manifest.minimum_launcher_protocol = LAUNCHER_PROTOCOL_VERSION + 1;
        let signed = sign_release_manifest(manifest, &key).expect("sign incompatible release");
        assert_eq!(
            verify_release_manifest(&signed, &key.verifying_key(), &ReleasePolicy::native(0)),
            Err(LifecycleError::IncompatibleRelease)
        );
        let mut manifest = signed.manifest;
        manifest.minimum_launcher_protocol = 0;
        assert_eq!(
            sign_release_manifest(manifest, &key),
            Err(LifecycleError::InvalidManifest)
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
                .join("aeris_desktop"),
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
            .join("aeris_desktop");
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
    fn recovery_moves_orphaned_entries_aside_and_keeps_bounded_batches() {
        let root = temporary_root("orphaned-entries");
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
        let orphaned = root.join("install/orphaned");
        for attempt in 0..=MAXIMUM_ORPHAN_BATCHES {
            fs::create_dir_all(root.join(format!("install/versions/stray-{attempt}")))
                .expect("stray release directory");
            fs::write(
                installer
                    .lifecycle_root
                    .join("manifests")
                    .join(format!("stray-{attempt}.json")),
                b"{}",
            )
            .expect("stray manifest");
            assert_eq!(
                installer.audit_active_release(),
                Err(LifecycleError::UpdatePendingCleanup)
            );
            installer
                .recover(&Hooks::default())
                .expect("recovery moves strays aside");
            assert_eq!(installer.audit_active_release(), Ok(Some(active.clone())));
            let mut batches = fs::read_dir(&orphaned)
                .expect("orphaned root")
                .map(|entry| entry.expect("batch").path())
                .collect::<Vec<_>>();
            batches.sort();
            assert_eq!(batches.len(), (attempt + 1).min(MAXIMUM_ORPHAN_BATCHES));
            let newest = batches.last().expect("newest batch");
            assert!(newest.join(format!("versions/stray-{attempt}")).is_dir());
            assert!(
                newest
                    .join(format!("manifests/stray-{attempt}.json"))
                    .is_file()
            );
        }
        installer
            .recover(&Hooks::default())
            .expect("recovery without strays");
        assert_eq!(
            fs::read_dir(&orphaned).expect("orphaned root").count(),
            MAXIMUM_ORPHAN_BATCHES,
            "a recovery with nothing to move creates no batch"
        );
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn active_release_ignores_an_abandoned_staged_pointer() {
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
        let destination = installer
            .lifecycle_root
            .join("active")
            .join(pointer_name(&uncommitted));
        let mut staged =
            aeris_platform_runtime::fs::StagedFile::create(&destination).expect("stage pointer");
        staged
            .write_all(&serde_json::to_vec(&uncommitted).expect("pointer encodes"))
            .expect("stage pointer bytes");
        std::mem::forget(staged);

        assert_eq!(
            installer.active_release().expect("active read"),
            Some(active.clone())
        );
        assert_eq!(
            installer
                .audit_active_release()
                .expect("audit ignores staging"),
            Some(active)
        );
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn preactivation_recovery_removes_abandoned_staged_pointer_and_manifest_files() {
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
        for directory in [
            installer.lifecycle_root.join("active"),
            installer.lifecycle_root.join("manifests"),
        ] {
            let mut staged = aeris_platform_runtime::fs::StagedFile::create(&directory.join(&name))
                .expect("stage release record");
            staged
                .write_all(b"partial release record")
                .expect("stage partial record");
            std::mem::forget(staged);
        }
        installer
            .write_update_journal(&UpdateJournal {
                state: UpdateState::ProcessesStopped,
                candidate,
                previous: Some(previous.clone()),
                previous_retained: None,
            })
            .expect("preactivation journal");

        installer.recover(&Hooks::default()).expect("recover");
        for directory in [
            installer.lifecycle_root.join("active"),
            installer.lifecycle_root.join("manifests"),
        ] {
            assert!(
                fs::read_dir(directory)
                    .expect("release record directory")
                    .all(
                        |entry| aeris_platform_runtime::fs::staged_temporary_destination_name(
                            entry.expect("release record entry").file_name().as_ref()
                        ) != Some(name.as_str())
                    ),
                "recovery must remove abandoned staged release records"
            );
        }
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
    fn readiness_passed_recovery_discards_a_changed_candidate_without_running_it() {
        let root = temporary_root("readiness-passed-recovery-audit");
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
                state: UpdateState::ReadinessPassed,
                candidate: candidate.clone(),
                previous: Some(previous.clone()),
                previous_retained: None,
            })
            .expect("write readiness-passed journal");

        fs::write(
            candidate_root.join("aeris_desktop"),
            b"post-readiness mutation",
        )
        .expect("mutate candidate desktop");
        let hooks = Hooks::default();
        installer
            .recover(&hooks)
            .expect("a changed candidate is discarded");
        assert_eq!(
            *hooks.health_checks.lock().expect("health counter locks"),
            0,
            "recovery never runs candidate code"
        );
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
    fn failed_readiness_keeps_the_previous_release_across_state_compatibility_epochs() {
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
            Err(LifecycleError::HealthCheckFailed)
        );
        assert_eq!(
            installer
                .audit_active_release()
                .expect("active audit")
                .expect("previous remains selected")
                .install_generation,
            1
        );
        assert!(!installer.update_journal_exists());
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn readiness_passed_recovery_finishes_after_partial_known_good_rotation_without_running_the_candidate()
     {
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
            state: UpdateState::ReadinessPassed,
            candidate: candidate.clone(),
            previous: Some(previous.clone()),
            previous_retained: None,
        };
        installer
            .write_update_journal(&journal)
            .expect("write readiness-passed journal");

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

        let hooks = Hooks {
            fail_health: true,
            ..Hooks::default()
        };
        installer
            .recover(&hooks)
            .expect("the passed candidate finishes activating");
        assert_eq!(
            *hooks.health_checks.lock().expect("health counter locks"),
            0
        );
        assert_eq!(
            installer
                .audit_active_release()
                .expect("active audit")
                .expect("candidate active"),
            candidate
        );
        assert_eq!(
            installer
                .retained_known_good_release()
                .expect("retained state"),
            Some(previous)
        );
        assert_eq!(count_entries(&root.join("install/versions")), Ok(2));
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
            .rollback_to_retained_known_good(&Hooks::default(), LiveStoreSchema::Absent)
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
    fn retained_rollback_requires_a_target_that_opens_the_live_store_schema() {
        let root = temporary_root("rollback-store-schema");
        let (first, key, first_bundle) = release_with_store_schema(&root, 1, 30);
        let installer = ReleaseInstaller::new(
            root.join("install"),
            key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        installer
            .install(&first, &first_bundle, &Hooks::default())
            .expect("first install");
        let (second, _, second_bundle) = release_with_store_schema(&root, 2, 31);
        let second = sign_release_manifest(second.manifest, &key).expect("same release key");
        let active = installer
            .install(&second, &second_bundle, &Hooks::default())
            .expect("second install")
            .active;

        for refused in [
            LiveStoreSchema::Checkpointed(31),
            LiveStoreSchema::Uncheckpointed(30),
        ] {
            assert_eq!(
                installer.verify_retained_rollback(refused),
                Err(LifecycleError::RollbackStoreSchemaNewer)
            );
            assert_eq!(
                installer.rollback_to_retained_known_good(&Hooks::default(), refused),
                Err(LifecycleError::RollbackStoreSchemaNewer)
            );
            assert_eq!(
                installer.active_release().expect("active read"),
                Some(active.clone())
            );
            assert!(!installer.retained_rollback_journal_exists());
        }
        let live = LiveStoreSchema::Checkpointed(30);
        let target = installer
            .verify_retained_rollback(live)
            .expect("the retained release opens schema 30");
        assert_eq!(target.install_generation, 1);
        assert_eq!(
            installer.rollback_to_retained_known_good(&Hooks::default(), live),
            Ok(target)
        );
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
            installer.rollback_to_retained_known_good(&Hooks::default(), LiveStoreSchema::Absent),
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
            installer.rollback_to_retained_known_good(&Hooks::default(), LiveStoreSchema::Absent),
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
            installer.rollback_to_retained_known_good(&Hooks::default(), LiveStoreSchema::Absent),
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
    fn readiness_passed_and_cleanup_recovery_never_rechecks_candidate_health() {
        for state in [UpdateState::ReadinessPassed, UpdateState::Cleanup] {
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
            UpdateState::ReadinessPassed,
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
            if matches!(state, UpdateState::ReadinessPassed | UpdateState::Cleanup) {
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
            let expected = if matches!(state, UpdateState::ReadinessPassed | UpdateState::Cleanup) {
                &candidate
            } else {
                &previous
            };
            assert_eq!(&active, expected);
            let retained = installer
                .retained_known_good_release()
                .expect("retained release");
            if matches!(state, UpdateState::ReadinessPassed | UpdateState::Cleanup) {
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
            state: UpdateState::ReadinessPassed,
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
            UpdateState::ReadinessPassed
        );

        fs::write(
            installer
                .lifecycle_root
                .join(update_journal_name(UpdateState::ReadinessPassed)),
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
            .join("aeris_desktop");
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
                    .any(|entry| entry.service == "com.aeris.account" && entry.key == key),
                "native inventory lost account vault key {key}"
            );
        }
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn native_inventory_owns_legacy_axiusflow_roots_for_cleanup() {
        let root = temporary_root("inventory-legacy-roots");
        let inventory =
            native_installation_inventory(root.join("install")).expect("native inventory builds");
        for expected in [
            native_legacy_data_root("Axiusflow").expect("legacy data root"),
            native_legacy_cache_root().expect("legacy cache root"),
            native_legacy_install_root().expect("legacy install root"),
        ] {
            assert!(
                inventory.legacy_cleanup_roots.contains(&expected),
                "native inventory lost legacy cleanup root {}",
                expected.display()
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
        let install_root = root.join("install");
        let data_root = root.join("data");
        fs::create_dir_all(&install_root).expect("install root");
        fs::create_dir_all(&data_root).expect("data root");
        fs::write(data_root.join("workspace-state.pb"), b"workspace").expect("workspace state");
        let uninstaller = ReleaseUninstaller::new(&install_root).expect("uninstaller");
        let inventory = InstallationInventory {
            schema_version: INVENTORY_SCHEMA_VERSION,
            install_root,
            data_roots: vec![data_root],
            cache_roots: Vec::new(),
            log_roots: Vec::new(),
            legacy_cleanup_roots: Vec::new(),
            vault_entries: vec![
                VaultEntry {
                    service: "com.aeris.account".to_string(),
                    key: "account-refresh-default-v1".to_string(),
                },
                VaultEntry {
                    service: "com.aeris.terminal".to_string(),
                    key: "provider-rithmic-test-default-v1".to_string(),
                },
            ],
            registrations: vec!["start-menu:Aeris".to_string()],
        };
        let hooks = Hooks::default();
        let outcome = uninstaller
            .uninstall(&inventory, &hooks)
            .expect("uninstall");
        assert_eq!(outcome.removed_vault_keys, 2);
        assert!(!inventory.install_root.exists());
        assert!(!inventory.data_roots[0].exists());
        uninstaller
            .uninstall(&inventory, &hooks)
            .expect("repeated uninstall remains idempotent");
        let _ = remove_owned_path(&root);
    }

    #[test]
    fn application_removal_keeps_user_data_and_vault_entries() {
        let root = temporary_root("remove-application");
        let (signed, key, bundle) = release(&root, 1);
        let install_root = root.join("install");
        let data_root = root.join("data");
        let legacy_root = root.join("legacy");
        let installer =
            ReleaseInstaller::new(&install_root, key.verifying_key(), ReleasePolicy::native(0))
                .expect("installer");
        installer
            .install(&signed, &bundle, &Hooks::default())
            .expect("install");
        fs::create_dir_all(&data_root).expect("data root");
        fs::create_dir_all(&legacy_root).expect("legacy root");
        fs::write(data_root.join("trading.sqlite3"), b"journal").expect("trading journal");
        let (lifecycle_root, lock_path) = lifecycle_paths(&install_root).expect("lifecycle paths");
        assert!(lifecycle_root.exists());
        let inventory = InstallationInventory {
            schema_version: INVENTORY_SCHEMA_VERSION,
            install_root: install_root.clone(),
            data_roots: vec![data_root.clone()],
            cache_roots: Vec::new(),
            log_roots: Vec::new(),
            legacy_cleanup_roots: vec![legacy_root.clone()],
            vault_entries: vec![VaultEntry {
                service: "com.aeris.account".to_string(),
                key: "account-refresh-default-v1".to_string(),
            }],
            registrations: vec!["start-menu:Aeris".to_string()],
        };
        let uninstaller = ReleaseUninstaller::new(&install_root).expect("uninstaller");
        let hooks = Hooks::default();

        let outcome = uninstaller
            .remove_application(&inventory, &hooks)
            .expect("application removal");
        assert_eq!(outcome.removed_vault_keys, 0);
        assert!(!install_root.exists());
        assert!(!lifecycle_root.exists());
        assert!(!lock_path.exists());
        assert_eq!(
            fs::read(data_root.join("trading.sqlite3")).expect("journal kept"),
            b"journal"
        );
        assert!(legacy_root.exists());
        assert!(hooks.deleted.lock().expect("deleted entries").is_empty());
        uninstaller
            .remove_application(&inventory, &hooks)
            .expect("repeated application removal remains idempotent");

        fs::create_dir_all(&install_root).expect("reinstalled root");
        write_json_atomic(
            &lifecycle_root,
            "uninstall.json",
            &UninstallJournal { started: true },
        )
        .expect("pending full uninstall");
        assert_eq!(
            uninstaller.remove_application(&inventory, &hooks),
            Err(LifecycleError::UninstallPendingCleanup),
            "a pending full uninstall is never downgraded to keeping data"
        );
        assert!(data_root.exists());
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
