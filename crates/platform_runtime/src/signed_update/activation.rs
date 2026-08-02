//! Crash-recoverable Unix staging of verified update artifacts.

use super::{
    InstalledRelease, SHA256_BYTES, SignedUpdateError, SignedUpdateVerifier, UpdateRollbackState,
    read_retry_interrupted,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const ACTIVATION_SCHEMA_VERSION: u32 = 1;
const JOURNAL_FILE: &str = "activation.journal";
const RELEASE_DIRECTORY: &str = "releases";
const STAGING_DIRECTORY: &str = "staging";
const MAX_JOURNAL_BYTES: u64 = 8 * 1_024 * 1_024;
const MAX_JOURNAL_RECORD_BYTES: usize = 16 * 1_024;

/// Owns an exclusively locked, crash-recoverable Unix update staging journal.
///
/// Verified artifacts are synced into content-addressed release files before an
/// activation record is appended and synced. A trailing partial record is discarded.
pub struct DurableUpdateActivator {
    root: PathBuf,
    journal: File,
    state: UpdateRollbackState,
    generation: u64,
    journal_usable: bool,
}

impl DurableUpdateActivator {
    /// Creates a durable staging store from one verified release and its exact artifact.
    ///
    /// `root` must be an existing directory whose entry was durably provisioned by
    /// the caller. This adapter never creates the store root because it cannot prove
    /// that an absent root's parent directory has reached durable storage.
    ///
    /// # Errors
    ///
    /// Returns an error when the root is absent or not a directory, the store is already
    /// initialized, another process owns it, the artifact no longer matches its verified
    /// manifest, or durable I/O fails.
    pub fn initialize(
        root: impl AsRef<Path>,
        release: InstalledRelease,
        artifact: impl Read,
    ) -> Result<Self, UpdateActivationError> {
        let root = root.as_ref().to_path_buf();
        prepare_directories(&root)?;
        let mut journal = open_journal(&root, true)?;
        lock_journal(&journal)?;
        recover_interrupted_initialization(&mut journal)?;
        remove_stale_staging_files(&root)?;
        remove_all_release_files(&root)?;

        stage_release(&root, &release, artifact)?;
        let state = UpdateRollbackState::new(release);
        append_record(&mut journal, 1, &state)?;
        sync_directory(&root)?;

        Ok(Self {
            root,
            journal,
            state,
            generation: 1,
            journal_usable: true,
        })
    }

    /// Opens and validates an existing activation store.
    ///
    /// A final record without a newline is treated as an interrupted append and
    /// truncated while the journal lock is held. Every retained artifact is hashed.
    /// The verifier floor must equal the latest durable floor and must therefore come
    /// from trusted release policy rather than from the journal being opened.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing or locked store, malformed committed state,
    /// missing or changed artifacts, or durable I/O failure.
    pub fn open(
        root: impl AsRef<Path>,
        verifier: &SignedUpdateVerifier,
    ) -> Result<Self, UpdateActivationError> {
        let root = root.as_ref().to_path_buf();
        validate_existing_store(&root)?;
        let mut journal = open_journal(&root, false)?;
        lock_journal(&journal)?;
        let (generation, state, valid_bytes) = read_last_record(&mut journal, verifier)?;
        let journal_bytes = journal.metadata().map_err(UpdateActivationError::Io)?.len();
        if valid_bytes < journal_bytes {
            journal
                .set_len(valid_bytes)
                .map_err(UpdateActivationError::Io)?;
            journal.sync_all().map_err(UpdateActivationError::Io)?;
        }
        validate_retained_artifacts(&root, &state)?;
        remove_stale_staging_files(&root)?;
        remove_unretained_release_files(&root, &state)?;

        Ok(Self {
            root,
            journal,
            state,
            generation,
            journal_usable: true,
        })
    }

    #[must_use]
    pub const fn state(&self) -> &UpdateRollbackState {
        &self.state
    }

    #[must_use]
    pub fn active_artifact_path(&self) -> PathBuf {
        release_path(&self.root, self.state.active())
    }

    /// Durably stages and activates a newer verified release.
    ///
    /// The in-memory active state changes only after the artifact and journal record
    /// have both reached durable storage.
    ///
    /// # Errors
    ///
    /// Returns an error when the release violates rollback policy, the artifact does
    /// not match, the journal requires recovery, or durable I/O fails. A
    /// [`UpdateActivationError::CommittedCleanup`] error means activation committed
    /// durably but removal of an unretained artifact failed.
    pub fn activate(
        &mut self,
        candidate: &InstalledRelease,
        artifact: impl Read,
    ) -> Result<(), UpdateActivationError> {
        self.ensure_usable()?;
        if candidate.manifest.signing_key_identity
            != self.state.active().manifest.signing_key_identity
        {
            return Err(SignedUpdateError::RotationPersistenceUnsupported.into());
        }
        let mut next_state = self.state.clone();
        next_state.activate(candidate.clone())?;
        remove_unretained_release_files(&self.root, &self.state)?;
        stage_release(&self.root, candidate, artifact)?;
        self.commit(next_state)
    }

    /// Durably restores the retained verified predecessor without lowering the floor.
    ///
    /// # Errors
    ///
    /// Returns an error when rollback is unavailable, the journal requires recovery,
    /// or durable I/O fails. A [`UpdateActivationError::CommittedCleanup`] error means
    /// rollback committed durably but removal of an unretained artifact failed.
    pub fn rollback(&mut self) -> Result<(), UpdateActivationError> {
        self.ensure_usable()?;
        let mut next_state = self.state.clone();
        next_state.rollback()?;
        self.commit(next_state)
    }

    fn commit(&mut self, next_state: UpdateRollbackState) -> Result<(), UpdateActivationError> {
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(UpdateActivationError::GenerationExhausted)?;
        if let Err(error) = append_record(&mut self.journal, generation, &next_state) {
            self.journal_usable = false;
            return Err(error);
        }
        self.state = next_state;
        self.generation = generation;
        remove_unretained_release_files(&self.root, &self.state)
            .map_err(|error| UpdateActivationError::CommittedCleanup(Box::new(error)))
    }

    fn ensure_usable(&self) -> Result<(), UpdateActivationError> {
        if self.journal_usable {
            Ok(())
        } else {
            Err(UpdateActivationError::RecoveryRequired)
        }
    }
}

impl fmt::Debug for DurableUpdateActivator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DurableUpdateActivator")
            .field("root", &self.root)
            .field("state", &self.state)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// Errors returned by durable update staging, activation, and recovery.
#[derive(Debug)]
pub enum UpdateActivationError {
    Io(std::io::Error),
    SignedUpdate(SignedUpdateError),
    AlreadyInitialized,
    StoreNotInitialized,
    StoreLocked,
    InvalidJournal,
    JournalFull,
    InvalidPersistedRelease,
    MissingArtifact,
    ArtifactChanged,
    GenerationExhausted,
    RecoveryRequired,
    InvalidStoreRoot,
    InvalidStoreEntry,
    CommittedCleanup(Box<UpdateActivationError>),
}

impl fmt::Display for UpdateActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "durable update I/O failed: {error}"),
            Self::SignedUpdate(error) => write!(formatter, "verified update rejected: {error}"),
            Self::AlreadyInitialized => formatter.write_str("update store is already initialized"),
            Self::StoreNotInitialized => formatter.write_str("update store is not initialized"),
            Self::StoreLocked => formatter.write_str("update store is owned by another process"),
            Self::InvalidJournal => formatter.write_str("update activation journal is invalid"),
            Self::JournalFull => formatter.write_str("update activation journal is full"),
            Self::InvalidPersistedRelease => {
                formatter.write_str("persisted update release metadata is invalid")
            }
            Self::MissingArtifact => formatter.write_str("retained update artifact is missing"),
            Self::ArtifactChanged => formatter.write_str("retained update artifact has changed"),
            Self::GenerationExhausted => {
                formatter.write_str("update activation generation is exhausted")
            }
            Self::RecoveryRequired => {
                formatter.write_str("update activation journal must be reopened for recovery")
            }
            Self::InvalidStoreRoot => {
                formatter.write_str("update staging root must be an existing directory")
            }
            Self::InvalidStoreEntry => {
                formatter.write_str("update staging store contains an invalid fixed entry")
            }
            Self::CommittedCleanup(error) => {
                write!(
                    formatter,
                    "update state committed but artifact cleanup failed: {error}"
                )
            }
        }
    }
}

impl Error for UpdateActivationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::SignedUpdate(error) => Some(error),
            Self::CommittedCleanup(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

impl From<SignedUpdateError> for UpdateActivationError {
    fn from(error: SignedUpdateError) -> Self {
        Self::SignedUpdate(error)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ActivationRecord {
    schema_version: u32,
    generation: u64,
    active: ReleaseRecord,
    rollback: Option<ReleaseRecord>,
    minimum_release_sequence: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReleaseRecord {
    signed_manifest: String,
    signature: String,
}

impl From<&InstalledRelease> for ReleaseRecord {
    fn from(release: &InstalledRelease) -> Self {
        Self {
            signed_manifest: URL_SAFE_NO_PAD.encode(&release.manifest.signed_json),
            signature: URL_SAFE_NO_PAD.encode(release.manifest.signature),
        }
    }
}

impl ReleaseRecord {
    fn verify(
        self,
        verifier: &SignedUpdateVerifier,
    ) -> Result<InstalledRelease, UpdateActivationError> {
        let signed_manifest = URL_SAFE_NO_PAD
            .decode(self.signed_manifest)
            .map_err(|_| UpdateActivationError::InvalidPersistedRelease)?;
        let signature = URL_SAFE_NO_PAD
            .decode(self.signature)
            .map_err(|_| UpdateActivationError::InvalidPersistedRelease)?;
        let manifest = verifier
            .verify_persisted_manifest(&signed_manifest, &signature)
            .map_err(UpdateActivationError::SignedUpdate)?;
        Ok(InstalledRelease { manifest })
    }
}

fn prepare_directories(root: &Path) -> Result<(), UpdateActivationError> {
    validate_store_root(root)?;
    ensure_store_directory(root, RELEASE_DIRECTORY)?;
    ensure_store_directory(root, STAGING_DIRECTORY)?;
    sync_directory(root)?;
    Ok(())
}

fn validate_existing_store(root: &Path) -> Result<(), UpdateActivationError> {
    validate_store_root(root)?;
    validate_store_directory(root, RELEASE_DIRECTORY)?;
    validate_store_directory(root, STAGING_DIRECTORY)
}

fn validate_store_root(root: &Path) -> Result<(), UpdateActivationError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(UpdateActivationError::InvalidStoreRoot),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(UpdateActivationError::InvalidStoreRoot)
        }
        Err(error) => Err(UpdateActivationError::Io(error)),
    }
}

fn validate_store_directory(root: &Path, name: &str) -> Result<(), UpdateActivationError> {
    match fs::symlink_metadata(root.join(name)) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(UpdateActivationError::InvalidStoreEntry),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(UpdateActivationError::InvalidStoreEntry)
        }
        Err(error) => Err(UpdateActivationError::Io(error)),
    }
}

fn ensure_store_directory(root: &Path, name: &str) -> Result<(), UpdateActivationError> {
    let path = root.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(UpdateActivationError::InvalidStoreEntry),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&path).map_err(UpdateActivationError::Io)
        }
        Err(error) => Err(UpdateActivationError::Io(error)),
    }
}

fn open_journal(root: &Path, create: bool) -> Result<File, UpdateActivationError> {
    let path = root.join(JOURNAL_FILE);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err(UpdateActivationError::InvalidStoreEntry),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(UpdateActivationError::StoreNotInitialized);
        }
        Err(error) => return Err(UpdateActivationError::Io(error)),
    }
    let result = OpenOptions::new()
        .read(true)
        .append(true)
        .create(create)
        .open(path);
    match result {
        Ok(file)
            if file
                .metadata()
                .map_err(UpdateActivationError::Io)?
                .is_file() =>
        {
            Ok(file)
        }
        Ok(_) => Err(UpdateActivationError::InvalidStoreEntry),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(UpdateActivationError::StoreNotInitialized)
        }
        Err(error) => Err(UpdateActivationError::Io(error)),
    }
}

fn lock_journal(journal: &File) -> Result<(), UpdateActivationError> {
    match journal.try_lock() {
        Ok(()) => Ok(()),
        Err(std::fs::TryLockError::WouldBlock) => Err(UpdateActivationError::StoreLocked),
        Err(std::fs::TryLockError::Error(error)) => Err(UpdateActivationError::Io(error)),
    }
}

fn recover_interrupted_initialization(journal: &mut File) -> Result<(), UpdateActivationError> {
    let bytes = journal.metadata().map_err(UpdateActivationError::Io)?.len();
    if bytes == 0 {
        return Ok(());
    }
    if bytes > MAX_JOURNAL_BYTES {
        return Err(UpdateActivationError::JournalFull);
    }
    journal
        .seek(SeekFrom::Start(0))
        .map_err(UpdateActivationError::Io)?;
    let mut contents =
        Vec::with_capacity(usize::try_from(bytes).map_err(|_| UpdateActivationError::JournalFull)?);
    journal
        .read_to_end(&mut contents)
        .map_err(UpdateActivationError::Io)?;
    if contents.contains(&b'\n') {
        return Err(UpdateActivationError::AlreadyInitialized);
    }
    journal.set_len(0).map_err(UpdateActivationError::Io)?;
    journal.sync_all().map_err(UpdateActivationError::Io)
}

fn append_record(
    journal: &mut File,
    generation: u64,
    state: &UpdateRollbackState,
) -> Result<(), UpdateActivationError> {
    let record = ActivationRecord {
        schema_version: ACTIVATION_SCHEMA_VERSION,
        generation,
        active: ReleaseRecord::from(&state.active),
        rollback: state.rollback.as_ref().map(ReleaseRecord::from),
        minimum_release_sequence: state.minimum_release_sequence,
    };
    let mut encoded =
        serde_json::to_vec(&record).map_err(|_| UpdateActivationError::InvalidJournal)?;
    if encoded.len() + 1 > MAX_JOURNAL_RECORD_BYTES {
        return Err(UpdateActivationError::InvalidJournal);
    }
    let current_bytes = journal.metadata().map_err(UpdateActivationError::Io)?.len();
    if current_bytes
        .checked_add(
            u64::try_from(encoded.len() + 1).map_err(|_| UpdateActivationError::JournalFull)?,
        )
        .is_none_or(|bytes| bytes > MAX_JOURNAL_BYTES)
    {
        return Err(UpdateActivationError::JournalFull);
    }
    encoded.push(b'\n');
    journal
        .seek(SeekFrom::End(0))
        .map_err(UpdateActivationError::Io)?;
    journal
        .write_all(&encoded)
        .map_err(UpdateActivationError::Io)?;
    journal.sync_all().map_err(UpdateActivationError::Io)
}

fn read_last_record(
    journal: &mut File,
    verifier: &SignedUpdateVerifier,
) -> Result<(u64, UpdateRollbackState, u64), UpdateActivationError> {
    let bytes = journal.metadata().map_err(UpdateActivationError::Io)?.len();
    if bytes == 0 {
        return Err(UpdateActivationError::StoreNotInitialized);
    }
    if bytes > MAX_JOURNAL_BYTES {
        return Err(UpdateActivationError::JournalFull);
    }
    journal
        .seek(SeekFrom::Start(0))
        .map_err(UpdateActivationError::Io)?;
    let mut reader = BufReader::new(&mut *journal);
    let mut line = Vec::new();
    let mut last: Option<(u64, UpdateRollbackState)> = None;
    let mut expected_generation = 1_u64;
    let mut valid_bytes = 0_u64;

    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(UpdateActivationError::Io)?;
        if read == 0 {
            break;
        }
        if line.len() > MAX_JOURNAL_RECORD_BYTES {
            return Err(UpdateActivationError::InvalidJournal);
        }
        if line.last() != Some(&b'\n') {
            break;
        }
        line.pop();
        let record: ActivationRecord =
            serde_json::from_slice(&line).map_err(|_| UpdateActivationError::InvalidJournal)?;
        if record.schema_version != ACTIVATION_SCHEMA_VERSION
            || record.generation != expected_generation
        {
            return Err(UpdateActivationError::InvalidJournal);
        }
        let state = restore_state(record, verifier)?;
        if let Some((_, previous)) = &last {
            if !valid_transition(previous, &state) {
                return Err(UpdateActivationError::InvalidJournal);
            }
        } else if state.rollback.is_some()
            || state.minimum_release_sequence != state.active.manifest.release_sequence
        {
            return Err(UpdateActivationError::InvalidJournal);
        }
        valid_bytes = valid_bytes
            .checked_add(u64::try_from(read).map_err(|_| UpdateActivationError::InvalidJournal)?)
            .ok_or(UpdateActivationError::InvalidJournal)?;
        last = Some((expected_generation, state));
        expected_generation = expected_generation
            .checked_add(1)
            .ok_or(UpdateActivationError::GenerationExhausted)?;
    }

    let (generation, state) = last.ok_or(UpdateActivationError::StoreNotInitialized)?;
    if state.minimum_release_sequence != verifier.minimum_release_sequence {
        return Err(UpdateActivationError::InvalidJournal);
    }
    Ok((generation, state, valid_bytes))
}

fn restore_state(
    record: ActivationRecord,
    verifier: &SignedUpdateVerifier,
) -> Result<UpdateRollbackState, UpdateActivationError> {
    let active = record.active.verify(verifier)?;
    let rollback = record
        .rollback
        .map(|release| release.verify(verifier))
        .transpose()?;
    UpdateRollbackState::try_restore(active, rollback, record.minimum_release_sequence)
        .map_err(UpdateActivationError::SignedUpdate)
}

fn valid_transition(previous: &UpdateRollbackState, current: &UpdateRollbackState) -> bool {
    if current.minimum_release_sequence > previous.minimum_release_sequence {
        current.active.manifest.release_sequence == current.minimum_release_sequence
            && current.rollback.as_ref() == Some(&previous.active)
            && current.active.manifest.signing_key_identity
                == previous.active.manifest.signing_key_identity
    } else {
        current.minimum_release_sequence == previous.minimum_release_sequence
            && previous.rollback.as_ref() == Some(&current.active)
            && current.rollback.is_none()
    }
}

fn stage_release(
    root: &Path,
    release: &InstalledRelease,
    mut artifact: impl Read,
) -> Result<(), UpdateActivationError> {
    let final_path = release_path(root, release);
    if final_path.exists() {
        verify_artifact_file(&final_path, release)?;
        return sync_directory(&root.join(RELEASE_DIRECTORY));
    }

    let temporary_path = root.join(STAGING_DIRECTORY).join(format!(
        "{}-{}.partial",
        release.manifest.release_sequence,
        URL_SAFE_NO_PAD.encode(release.manifest.artifact_sha256)
    ));
    let mut temporary = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            fs::remove_file(&temporary_path).map_err(UpdateActivationError::Io)?;
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
                .map_err(UpdateActivationError::Io)?
        }
        Err(error) => return Err(UpdateActivationError::Io(error)),
    };
    let verification = copy_verified_artifact(release, &mut artifact, &mut temporary)
        .and_then(|()| temporary.sync_all().map_err(UpdateActivationError::Io));
    if let Err(error) = verification {
        drop(temporary);
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    drop(temporary);
    match fs::hard_link(&temporary_path, &final_path) {
        Ok(()) => {
            fs::remove_file(&temporary_path).map_err(UpdateActivationError::Io)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            fs::remove_file(&temporary_path).map_err(UpdateActivationError::Io)?;
            verify_artifact_file(&final_path, release)?;
        }
        Err(error) => return Err(UpdateActivationError::Io(error)),
    }
    sync_directory(&root.join(RELEASE_DIRECTORY))
}

fn copy_verified_artifact(
    release: &InstalledRelease,
    artifact: &mut impl Read,
    output: &mut impl Write,
) -> Result<(), UpdateActivationError> {
    let mut digest = Sha256::new();
    let mut remaining = release.manifest.artifact_bytes;
    let mut buffer = vec![0_u8; 64 * 1_024];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| UpdateActivationError::InvalidPersistedRelease)?;
        let read = read_retry_interrupted(artifact, &mut buffer[..requested])?;
        if read == 0 {
            return Err(SignedUpdateError::ArtifactTruncated.into());
        }
        output
            .write_all(&buffer[..read])
            .map_err(UpdateActivationError::Io)?;
        digest.update(&buffer[..read]);
        remaining -=
            u64::try_from(read).map_err(|_| UpdateActivationError::InvalidPersistedRelease)?;
    }
    let mut trailing = [0_u8; 1];
    if read_retry_interrupted(artifact, &mut trailing)? != 0 {
        return Err(SignedUpdateError::ArtifactHasTrailingBytes.into());
    }
    let actual: [u8; SHA256_BYTES] = digest.finalize().into();
    if actual != release.manifest.artifact_sha256 {
        return Err(SignedUpdateError::ArtifactDigestMismatch.into());
    }
    Ok(())
}

fn validate_retained_artifacts(
    root: &Path,
    state: &UpdateRollbackState,
) -> Result<(), UpdateActivationError> {
    verify_artifact_file(&release_path(root, &state.active), &state.active)?;
    if let Some(rollback) = &state.rollback {
        verify_artifact_file(&release_path(root, rollback), rollback)?;
    }
    Ok(())
}

fn verify_artifact_file(
    path: &Path,
    release: &InstalledRelease,
) -> Result<(), UpdateActivationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() == release.manifest.artifact_bytes => {
        }
        Ok(_) => return Err(UpdateActivationError::ArtifactChanged),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(UpdateActivationError::MissingArtifact);
        }
        Err(error) => return Err(UpdateActivationError::Io(error)),
    }
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(UpdateActivationError::MissingArtifact);
        }
        Err(error) => return Err(UpdateActivationError::Io(error)),
    };
    let metadata = file.metadata().map_err(UpdateActivationError::Io)?;
    if !metadata.is_file() || metadata.len() != release.manifest.artifact_bytes {
        return Err(UpdateActivationError::ArtifactChanged);
    }
    let mut digest = Sha256::new();
    let mut remaining = release.manifest.artifact_bytes;
    let mut buffer = vec![0_u8; 64 * 1_024];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| UpdateActivationError::InvalidPersistedRelease)?;
        let read = read_retry_interrupted(&mut file, &mut buffer[..requested])?;
        if read == 0 {
            return Err(UpdateActivationError::ArtifactChanged);
        }
        digest.update(&buffer[..read]);
        remaining -=
            u64::try_from(read).map_err(|_| UpdateActivationError::InvalidPersistedRelease)?;
    }
    let mut trailing = [0_u8; 1];
    if read_retry_interrupted(&mut file, &mut trailing)? != 0 {
        return Err(UpdateActivationError::ArtifactChanged);
    }
    let actual: [u8; SHA256_BYTES] = digest.finalize().into();
    if actual != release.manifest.artifact_sha256 {
        return Err(UpdateActivationError::ArtifactChanged);
    }
    Ok(())
}

fn release_path(root: &Path, release: &InstalledRelease) -> PathBuf {
    root.join(RELEASE_DIRECTORY).join(format!(
        "{}-{}-{}.release",
        release.manifest.release_sequence,
        URL_SAFE_NO_PAD.encode(release.manifest.signing_key_identity),
        URL_SAFE_NO_PAD.encode(release.manifest.artifact_sha256)
    ))
}

fn remove_stale_staging_files(root: &Path) -> Result<(), UpdateActivationError> {
    let staging = root.join(STAGING_DIRECTORY);
    for entry in fs::read_dir(&staging).map_err(UpdateActivationError::Io)? {
        let entry = entry.map_err(UpdateActivationError::Io)?;
        let file_type = entry.file_type().map_err(UpdateActivationError::Io)?;
        if file_type.is_file() {
            fs::remove_file(entry.path()).map_err(UpdateActivationError::Io)?;
        } else {
            return Err(UpdateActivationError::InvalidStoreEntry);
        }
    }
    sync_directory(&staging)
}

fn remove_all_release_files(root: &Path) -> Result<(), UpdateActivationError> {
    let releases = root.join(RELEASE_DIRECTORY);
    for entry in fs::read_dir(&releases).map_err(UpdateActivationError::Io)? {
        let entry = entry.map_err(UpdateActivationError::Io)?;
        if entry
            .file_type()
            .map_err(UpdateActivationError::Io)?
            .is_file()
        {
            fs::remove_file(entry.path()).map_err(UpdateActivationError::Io)?;
        } else {
            return Err(UpdateActivationError::InvalidStoreEntry);
        }
    }
    sync_directory(&releases)
}

fn remove_unretained_release_files(
    root: &Path,
    state: &UpdateRollbackState,
) -> Result<(), UpdateActivationError> {
    let active = release_path(root, &state.active);
    let rollback = state
        .rollback
        .as_ref()
        .map(|release| release_path(root, release));
    let releases = root.join(RELEASE_DIRECTORY);
    for entry in fs::read_dir(&releases).map_err(UpdateActivationError::Io)? {
        let entry = entry.map_err(UpdateActivationError::Io)?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(UpdateActivationError::Io)?;
        if !file_type.is_file() {
            return Err(UpdateActivationError::InvalidStoreEntry);
        }
        if path != active && rollback.as_ref() != Some(&path) {
            fs::remove_file(path).map_err(UpdateActivationError::Io)?;
        }
    }
    sync_directory(&releases)
}

fn sync_directory(path: &Path) -> Result<(), UpdateActivationError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(UpdateActivationError::Io)
}

#[cfg(test)]
mod tests {
    use super::{
        DurableUpdateActivator, JOURNAL_FILE, RELEASE_DIRECTORY, STAGING_DIRECTORY,
        UpdateActivationError, append_record,
    };
    use crate::{InstalledRelease, SignedUpdateError, SignedUpdateVerifier};
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    use std::{
        fs::{self, OpenOptions},
        io::{Cursor, Write},
        os::unix::fs::symlink,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static NEXT_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("test clock is after the Unix epoch")
                .as_nanos();
            for _ in 0..100 {
                let identifier = NEXT_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "axiusflow-update-{}-{timestamp}-{identifier}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("test directory can be created: {error}"),
                }
            }
            panic!("unique test directory could not be allocated")
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[83_u8; 32])
    }

    fn successor_key() -> SigningKey {
        SigningKey::from_bytes(&[87_u8; 32])
    }

    fn verifier(floor: u64) -> SignedUpdateVerifier {
        SignedUpdateVerifier::try_new(signing_key().verifying_key().to_bytes(), floor)
            .expect("test verification key is valid")
    }

    fn installed(sequence: u64, version: &str, artifact: &[u8]) -> InstalledRelease {
        let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(artifact));
        let manifest = format!(
            r#"{{"schema_version":1,"release_sequence":{sequence},"version":"{version}","artifact_bytes":{},"artifact_sha256":"{digest}"}}"#,
            artifact.len()
        )
        .into_bytes();
        let signature = signing_key().sign(&manifest).to_bytes();
        let verifier = verifier(sequence - 1);
        let manifest = verifier
            .verify_manifest(&manifest, &signature)
            .expect("test manifest is valid");
        verifier
            .verify_artifact(manifest, Cursor::new(artifact))
            .expect("test artifact is valid")
    }

    fn rotated_installed(sequence: u64, artifact: &[u8]) -> InstalledRelease {
        let current_key_identity = URL_SAFE_NO_PAD.encode(super::super::signing_key_identity(
            &signing_key().verifying_key(),
        ));
        let next_verifying_key = URL_SAFE_NO_PAD.encode(successor_key().verifying_key().to_bytes());
        let rotation = format!(
            r#"{{"schema_version":1,"current_key_identity":"{current_key_identity}","next_verifying_key":"{next_verifying_key}","minimum_release_sequence":{}}}"#,
            sequence - 1
        )
        .into_bytes();
        let rotation_signature = signing_key().sign(&rotation).to_bytes();
        let verifier = verifier(sequence - 1)
            .authorize_rotation(&rotation, &rotation_signature)
            .expect("successor signing key is authorized");
        let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(artifact));
        let manifest = format!(
            r#"{{"schema_version":1,"release_sequence":{sequence},"version":"2.0.0","artifact_bytes":{},"artifact_sha256":"{digest}"}}"#,
            artifact.len()
        )
        .into_bytes();
        let signature = successor_key().sign(&manifest).to_bytes();
        let manifest = verifier
            .verify_manifest(&manifest, &signature)
            .expect("successor manifest is valid");
        verifier
            .verify_artifact(manifest, Cursor::new(artifact))
            .expect("successor artifact is valid")
    }

    #[test]
    fn activation_and_rollback_survive_reopen_without_lowering_the_floor() {
        let directory = TestDirectory::new();
        let release_1 = installed(1, "1.0.0", b"release-one");
        let release_2 = installed(2, "2.0.0", b"release-two");
        let mut activator = DurableUpdateActivator::initialize(
            directory.path(),
            release_1,
            Cursor::new(b"release-one"),
        )
        .expect("activation store initializes");

        activator
            .activate(&release_2, Cursor::new(b"release-two"))
            .expect("new release activates");
        assert_eq!(activator.state().active().manifest().release_sequence(), 2);
        assert!(activator.state().rollback_available());
        activator.rollback().expect("predecessor rolls back");
        assert_eq!(activator.state().active().manifest().release_sequence(), 1);
        assert_eq!(activator.state().minimum_release_sequence(), 2);
        assert_eq!(
            fs::read_dir(directory.path().join(RELEASE_DIRECTORY))
                .expect("release directory exists")
                .count(),
            1
        );
        drop(activator);

        let mut recovered = DurableUpdateActivator::open(directory.path(), &verifier(2))
            .expect("durable state reopens");
        assert_eq!(recovered.state().active().manifest().release_sequence(), 1);
        assert_eq!(recovered.state().minimum_release_sequence(), 2);
        assert!(!recovered.state().rollback_available());
        assert!(matches!(
            recovered.activate(&release_2, Cursor::new(b"release-two")),
            Err(UpdateActivationError::SignedUpdate(
                SignedUpdateError::ReleaseSequenceNotIncreasing
            ))
        ));
    }

    #[test]
    fn repeated_activation_removes_the_displaced_predecessor() {
        let directory = TestDirectory::new();
        let mut activator = DurableUpdateActivator::initialize(
            directory.path(),
            installed(1, "1.0.0", b"release-one"),
            Cursor::new(b"release-one"),
        )
        .expect("activation store initializes");
        let release_2 = installed(2, "2.0.0", b"release-two");
        let release_3 = installed(3, "3.0.0", b"release-three");

        activator
            .activate(&release_2, Cursor::new(b"release-two"))
            .expect("second release activates");
        activator
            .activate(&release_3, Cursor::new(b"release-three"))
            .expect("third release activates");

        assert_eq!(activator.state().active(), &release_3);
        assert!(activator.state().rollback_available());
        assert_eq!(
            fs::read_dir(directory.path().join(RELEASE_DIRECTORY))
                .expect("release directory exists")
                .count(),
            2
        );
    }

    #[test]
    fn durable_activation_refuses_unpersisted_signing_key_rotation() {
        let directory = TestDirectory::new();
        let mut activator = DurableUpdateActivator::initialize(
            directory.path(),
            installed(1, "1.0.0", b"release-one"),
            Cursor::new(b"release-one"),
        )
        .expect("activation store initializes");
        let rotated = rotated_installed(2, b"rotated-release");

        assert!(matches!(
            activator.activate(&rotated, Cursor::new(b"rotated-release")),
            Err(UpdateActivationError::SignedUpdate(
                SignedUpdateError::RotationPersistenceUnsupported
            ))
        ));
        assert_eq!(activator.state().active().manifest().release_sequence(), 1);
    }

    #[test]
    fn open_recovers_a_partial_append_and_stale_staging_file() {
        let directory = TestDirectory::new();
        let activator = DurableUpdateActivator::initialize(
            directory.path(),
            installed(1, "1.0.0", b"release-one"),
            Cursor::new(b"release-one"),
        )
        .expect("activation store initializes");
        drop(activator);
        let journal_path = directory.path().join(JOURNAL_FILE);
        let committed_bytes = fs::metadata(&journal_path).expect("journal exists").len();
        let mut journal = OpenOptions::new()
            .append(true)
            .open(&journal_path)
            .expect("journal opens");
        journal
            .write_all(br#"{"schema_version":1"#)
            .expect("partial record is written");
        journal.sync_all().expect("partial record reaches storage");
        fs::write(
            directory
                .path()
                .join(STAGING_DIRECTORY)
                .join("stale.partial"),
            b"stale",
        )
        .expect("stale staging file is created");

        let recovered = DurableUpdateActivator::open(directory.path(), &verifier(1))
            .expect("partial append is discarded");
        assert_eq!(recovered.state().active().manifest().release_sequence(), 1);
        assert_eq!(
            fs::metadata(journal_path).expect("journal exists").len(),
            committed_bytes
        );
        assert_eq!(
            fs::read_dir(directory.path().join(STAGING_DIRECTORY))
                .expect("staging directory exists")
                .count(),
            0
        );
    }

    #[test]
    fn initialize_recovers_an_interrupted_first_record() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.path().join(RELEASE_DIRECTORY))
            .expect("release directory is created");
        fs::create_dir(directory.path().join(STAGING_DIRECTORY))
            .expect("staging directory is created");
        fs::write(
            directory
                .path()
                .join(RELEASE_DIRECTORY)
                .join("stale.release"),
            b"stale release",
        )
        .expect("stale release is created");
        fs::write(
            directory
                .path()
                .join(STAGING_DIRECTORY)
                .join("stale.partial"),
            b"stale staging artifact",
        )
        .expect("stale staging artifact is created");
        fs::write(
            directory.path().join(JOURNAL_FILE),
            br#"{"schema_version":1"#,
        )
        .expect("partial first record is written");

        let activator = DurableUpdateActivator::initialize(
            directory.path(),
            installed(1, "1.0.0", b"release-one"),
            Cursor::new(b"release-one"),
        )
        .expect("interrupted initialization is retried");
        assert_eq!(activator.state().active().manifest().release_sequence(), 1);
        assert_eq!(
            fs::read_dir(directory.path().join(RELEASE_DIRECTORY))
                .expect("release directory exists")
                .count(),
            1
        );
        assert_eq!(
            fs::read_dir(directory.path().join(STAGING_DIRECTORY))
                .expect("staging directory exists")
                .count(),
            0
        );
        drop(activator);
        assert!(DurableUpdateActivator::open(directory.path(), &verifier(1)).is_ok());
    }

    #[test]
    fn initialize_requires_a_preexisting_store_root() {
        let parent = TestDirectory::new();
        let missing = parent.path().join("missing");

        assert!(matches!(
            DurableUpdateActivator::initialize(
                &missing,
                installed(1, "1.0.0", b"release-one"),
                Cursor::new(b"release-one")
            ),
            Err(UpdateActivationError::InvalidStoreRoot)
        ));
        assert!(!missing.exists());
    }

    #[test]
    fn initialize_rejects_symlinked_fixed_store_entries() {
        let directory = TestDirectory::new();
        let outside = TestDirectory::new();
        symlink(outside.path(), directory.path().join(RELEASE_DIRECTORY))
            .expect("release symlink is created");

        assert!(matches!(
            DurableUpdateActivator::initialize(
                directory.path(),
                installed(1, "1.0.0", b"release-one"),
                Cursor::new(b"release-one")
            ),
            Err(UpdateActivationError::InvalidStoreEntry)
        ));
        assert_eq!(
            fs::read_dir(outside.path())
                .expect("outside directory exists")
                .count(),
            0
        );
    }

    #[test]
    fn open_rejects_replaced_fixed_directory_symlinks_without_traversal() {
        let directory = TestDirectory::new();
        let outside = TestDirectory::new();
        let activator = DurableUpdateActivator::initialize(
            directory.path(),
            installed(1, "1.0.0", b"release-one"),
            Cursor::new(b"release-one"),
        )
        .expect("activation store initializes");
        drop(activator);
        fs::write(outside.path().join("preserved"), b"outside").expect("outside file is created");
        fs::remove_dir(directory.path().join(STAGING_DIRECTORY))
            .expect("empty staging directory is removed");
        symlink(outside.path(), directory.path().join(STAGING_DIRECTORY))
            .expect("staging symlink is created");

        assert!(matches!(
            DurableUpdateActivator::open(directory.path(), &verifier(1)),
            Err(UpdateActivationError::InvalidStoreEntry)
        ));
        assert_eq!(
            fs::read(outside.path().join("preserved")).expect("outside file remains"),
            b"outside"
        );
    }

    #[test]
    fn exclusive_lock_rejects_a_second_store_owner() {
        let directory = TestDirectory::new();
        let _owner = DurableUpdateActivator::initialize(
            directory.path(),
            installed(1, "1.0.0", b"release-one"),
            Cursor::new(b"release-one"),
        )
        .expect("activation store initializes");

        assert!(matches!(
            DurableUpdateActivator::open(directory.path(), &verifier(1)),
            Err(UpdateActivationError::StoreLocked)
        ));
    }

    #[test]
    fn failed_artifact_staging_preserves_the_committed_release() {
        let directory = TestDirectory::new();
        let mut activator = DurableUpdateActivator::initialize(
            directory.path(),
            installed(1, "1.0.0", b"release-one"),
            Cursor::new(b"release-one"),
        )
        .expect("activation store initializes");

        assert!(matches!(
            activator.activate(
                &installed(2, "2.0.0", b"release-two"),
                Cursor::new(b"release-bad")
            ),
            Err(UpdateActivationError::SignedUpdate(
                SignedUpdateError::ArtifactDigestMismatch
            ))
        ));
        assert_eq!(activator.state().active().manifest().release_sequence(), 1);
        drop(activator);

        let recovered = DurableUpdateActivator::open(directory.path(), &verifier(1))
            .expect("previous committed release remains valid");
        assert_eq!(recovered.state().active().manifest().release_sequence(), 1);
        assert_eq!(
            fs::read_dir(directory.path().join(RELEASE_DIRECTORY))
                .expect("release directory exists")
                .count(),
            1
        );
    }

    #[test]
    fn recovery_rejects_changed_artifacts_and_foreign_signing_keys() {
        let directory = TestDirectory::new();
        let activator = DurableUpdateActivator::initialize(
            directory.path(),
            installed(1, "1.0.0", b"release-one"),
            Cursor::new(b"release-one"),
        )
        .expect("activation store initializes");
        let artifact_path = activator.active_artifact_path();
        drop(activator);
        let foreign_key = SigningKey::from_bytes(&[89_u8; 32]);
        let foreign_verifier =
            SignedUpdateVerifier::try_new(foreign_key.verifying_key().to_bytes(), 1)
                .expect("foreign verification key is valid");
        assert!(matches!(
            DurableUpdateActivator::open(directory.path(), &foreign_verifier),
            Err(UpdateActivationError::SignedUpdate(
                SignedUpdateError::InvalidManifestSignature
            ))
        ));

        fs::remove_file(&artifact_path).expect("test artifact is removed");
        fs::write(&artifact_path, b"release-evil").expect("test artifact is replaced");
        assert!(matches!(
            DurableUpdateActivator::open(directory.path(), &verifier(1)),
            Err(UpdateActivationError::ArtifactChanged)
        ));
    }

    #[test]
    fn recovery_rejects_oversized_artifacts_before_hashing_them() {
        let directory = TestDirectory::new();
        let activator = DurableUpdateActivator::initialize(
            directory.path(),
            installed(1, "1.0.0", b"release-one"),
            Cursor::new(b"release-one"),
        )
        .expect("activation store initializes");
        let artifact_path = activator.active_artifact_path();
        drop(activator);
        fs::remove_file(&artifact_path).expect("test artifact is removed");
        fs::write(&artifact_path, vec![0_u8; 1_024 * 1_024])
            .expect("oversized artifact is created");

        assert!(matches!(
            DurableUpdateActivator::open(directory.path(), &verifier(1)),
            Err(UpdateActivationError::ArtifactChanged)
        ));
    }

    #[test]
    fn recovery_rejects_signed_but_impossible_state_transitions() {
        let directory = TestDirectory::new();
        let activator = DurableUpdateActivator::initialize(
            directory.path(),
            installed(1, "1.0.0", b"release-one"),
            Cursor::new(b"release-one"),
        )
        .expect("activation store initializes");
        let unchanged = activator.state().clone();
        drop(activator);
        let mut journal = OpenOptions::new()
            .read(true)
            .append(true)
            .open(directory.path().join(JOURNAL_FILE))
            .expect("journal opens");
        append_record(&mut journal, 2, &unchanged).expect("signed duplicate state is appended");

        assert!(matches!(
            DurableUpdateActivator::open(directory.path(), &verifier(1)),
            Err(UpdateActivationError::InvalidJournal)
        ));
    }

    #[test]
    fn recovery_rejects_tampered_signed_manifest_metadata() {
        let directory = TestDirectory::new();
        let activator = DurableUpdateActivator::initialize(
            directory.path(),
            installed(1, "1.0.0", b"release-one"),
            Cursor::new(b"release-one"),
        )
        .expect("activation store initializes");
        drop(activator);
        let journal_path = directory.path().join(JOURNAL_FILE);
        let journal = fs::read_to_string(&journal_path).expect("journal is UTF-8 JSON");
        let mut record: Value = serde_json::from_str(journal.trim()).expect("record is JSON");
        let encoded = record["active"]["signed_manifest"]
            .as_str()
            .expect("signed manifest is encoded");
        let mut manifest = URL_SAFE_NO_PAD
            .decode(encoded)
            .expect("signed manifest is base64url");
        let version = manifest
            .windows(5)
            .position(|window| window == b"1.0.0")
            .expect("test version is present");
        manifest[version + 4] = b'1';
        record["active"]["signed_manifest"] = Value::String(URL_SAFE_NO_PAD.encode(manifest));
        fs::write(
            &journal_path,
            format!(
                "{}\n",
                serde_json::to_string(&record).expect("record encodes")
            ),
        )
        .expect("tampered journal is written");

        assert!(matches!(
            DurableUpdateActivator::open(directory.path(), &verifier(1)),
            Err(UpdateActivationError::SignedUpdate(
                SignedUpdateError::InvalidManifestSignature
            ))
        ));
    }

    #[test]
    fn recovery_requires_the_exact_durable_sequence_floor() {
        let directory = TestDirectory::new();
        let activator = DurableUpdateActivator::initialize(
            directory.path(),
            installed(1, "1.0.0", b"release-one"),
            Cursor::new(b"release-one"),
        )
        .expect("activation store initializes");
        drop(activator);

        assert!(matches!(
            DurableUpdateActivator::open(directory.path(), &verifier(0)),
            Err(UpdateActivationError::InvalidJournal)
        ));
        assert!(matches!(
            DurableUpdateActivator::open(directory.path(), &verifier(2)),
            Err(UpdateActivationError::InvalidJournal)
        ));
    }
}
