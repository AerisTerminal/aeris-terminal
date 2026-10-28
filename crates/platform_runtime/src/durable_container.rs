//! Versioned, checksummed single-file container for durable local state.
//!
//! A container file is `magic | envelope_version | schema_version | payload_len | sha256 |
//! payload`, with little-endian integers, written through [`StagedFile`]. Each write keeps the
//! previous valid file as a last-good backup. Loads try the primary, then a valid staged
//! temporary newer than the backup (a crash between staging and replacement), then the backup.
//! A recovering load quarantines an unusable primary, keeping the newest
//! [`MAXIMUM_QUARANTINED_COPIES`].

use aeris_observability::diagnostic;
use aeris_platform_sys::fs::{
    StagedFile, move_file, parse_staged_temporary_name, write_file_durably,
};
use sha2::{Digest as _, Sha256};
use std::{
    ffi::OsString,
    fmt,
    fs::{self, File},
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    time::SystemTime,
};

/// The bytes every container file starts with.
///
/// The first byte is NUL so no file written before the container existed can start like it.
/// Released versions wrote the workspace as a bare protobuf message, whose first byte is a
/// field key and is never zero because field number 0 is invalid, and chart chrome as UTF-8
/// `key=value` lines that start with an ASCII key. A file starting with NUL is therefore a
/// container and must carry the whole magic; any other non-empty file is a legacy payload
/// and is handed to the decoder unchanged.
pub const DURABLE_CONTAINER_MAGIC: [u8; 8] = *b"\0AERISDC";
/// The envelope layout version this build reads and writes.
pub const DURABLE_CONTAINER_VERSION: u16 = 1;
/// How many quarantined copies of an unusable primary are kept, newest first.
pub const MAXIMUM_QUARANTINED_COPIES: usize = 3;

const DIGEST_BYTES: usize = 32;
const HEADER_BYTES: usize = DURABLE_CONTAINER_MAGIC.len() + 2 + 4 + 8 + DIGEST_BYTES;
/// Bounds the staged-temporary scan of the destination directory.
const MAXIMUM_SCANNED_DIRECTORY_ENTRIES: usize = 1_024;
const MAXIMUM_STAGED_CANDIDATES: usize = 4;

/// Where a loaded payload came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DurableSource {
    Primary,
    /// A valid staged temporary left by an interrupted write.
    Staged(PathBuf),
    Backup,
}

/// How a candidate payload was stored.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurableFormat {
    Envelope {
        schema_version: u32,
    },
    /// A file written before the container existed; the whole file is the payload.
    Legacy,
}

/// A payload whose envelope, length and checksum verified, offered to the decoder.
#[derive(Clone, Copy, Debug)]
pub struct DurableCandidate<'a> {
    pub payload: &'a [u8],
    pub format: DurableFormat,
}

/// A candidate file that could not be used.
#[derive(Debug)]
pub struct DurableRejection {
    pub path: PathBuf,
    pub reason: String,
    /// Where an unusable primary was moved by a recovering load.
    pub quarantined_as: Option<PathBuf>,
}

impl fmt::Display for DurableRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.path.display(), self.reason)?;
        if let Some(quarantine) = &self.quarantined_as {
            write!(formatter, "; quarantined as {}", quarantine.display())?;
        }
        Ok(())
    }
}

/// The outcome of a load: the decoded value, if any candidate was accepted, and every
/// candidate rejected before it.
#[derive(Debug)]
pub struct DurableLoad<T> {
    pub restored: Option<(T, DurableSource)>,
    pub rejected: Vec<DurableRejection>,
}

/// One durable file, its last-good backup and its quarantined copies.
#[derive(Clone, Debug)]
pub struct DurableContainer {
    path: PathBuf,
    maximum_payload_bytes: usize,
}

enum ReadOutcome {
    Missing,
    Bytes(Vec<u8>),
    Unusable(String),
    Failed(io::Error),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum LoadMode {
    Recovering,
    ReadOnly,
}

impl DurableContainer {
    #[must_use]
    pub fn new(path: PathBuf, maximum_payload_bytes: usize) -> Self {
        Self {
            path,
            maximum_payload_bytes,
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The last-good backup, `<file name>.bak`, the name released versions also read.
    #[must_use]
    pub fn backup_path(&self) -> PathBuf {
        self.sibling(".bak")
    }

    /// The quarantined copy at `index`, where 1 is the newest.
    #[must_use]
    pub fn quarantine_path(&self, index: usize) -> PathBuf {
        self.sibling(&format!(".corrupt-{index}"))
    }

    /// Loads the newest usable payload, quarantining an unusable primary.
    pub fn load<T, E: fmt::Display>(
        &self,
        decode: impl FnMut(DurableCandidate<'_>) -> Result<T, E>,
    ) -> DurableLoad<T> {
        self.load_with(LoadMode::Recovering, decode)
    }

    /// Loads like [`Self::load`] without moving, quarantining or removing any file.
    pub fn inspect<T, E: fmt::Display>(
        &self,
        decode: impl FnMut(DurableCandidate<'_>) -> Result<T, E>,
    ) -> DurableLoad<T> {
        self.load_with(LoadMode::ReadOnly, decode)
    }

    /// Durably replaces the primary with an enveloped `payload`, first copying a valid
    /// current primary to the backup.
    ///
    /// # Errors
    /// Returns an error when the payload exceeds the bound, the current primary cannot be
    /// read, or the backup or primary cannot be durably written.
    pub fn write(&self, schema_version: u32, payload: &[u8]) -> io::Result<()> {
        if payload.len() > self.maximum_payload_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "payload exceeds the {}-byte persistence bound",
                    self.maximum_payload_bytes
                ),
            ));
        }
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let envelope = encode_envelope(schema_version, payload)?;
        match self.read_bounded(&self.path) {
            ReadOutcome::Bytes(current) if current == envelope => return Ok(()),
            ReadOutcome::Bytes(current) if self.unwrap_envelope(&current).is_ok() => {
                write_file_durably(&self.backup_path(), &current)?;
            }
            ReadOutcome::Missing | ReadOutcome::Unusable(_) | ReadOutcome::Bytes(_) => {}
            ReadOutcome::Failed(error) => return Err(error),
        }
        let mut staged = StagedFile::create(&self.path)?;
        staged.write_all(&envelope)?;
        staged.commit()?;
        self.remove_abandoned_temporaries();
        Ok(())
    }

    fn load_with<T, E: fmt::Display>(
        &self,
        mode: LoadMode,
        mut decode: impl FnMut(DurableCandidate<'_>) -> Result<T, E>,
    ) -> DurableLoad<T> {
        let mut rejected = Vec::new();
        match self.read_bounded(&self.path) {
            ReadOutcome::Missing => {}
            ReadOutcome::Failed(error) => rejected.push(DurableRejection {
                path: self.path.clone(),
                reason: format!("could not be read: {error}"),
                quarantined_as: None,
            }),
            ReadOutcome::Unusable(reason) => rejected.push(self.reject_primary(mode, reason)),
            ReadOutcome::Bytes(bytes) => match self.decode_candidate(&bytes, &mut decode) {
                Ok(value) => {
                    return DurableLoad {
                        restored: Some((value, DurableSource::Primary)),
                        rejected,
                    };
                }
                Err(reason) => rejected.push(self.reject_primary(mode, reason)),
            },
        }

        let backup = self.backup_path();
        let backup_modified = fs::metadata(&backup)
            .and_then(|metadata| metadata.modified())
            .ok();
        match self.staged_candidates(backup_modified) {
            Ok(candidates) => {
                for staged in candidates {
                    if let Some(value) = self.try_candidate(&staged, &mut decode, &mut rejected) {
                        return DurableLoad {
                            restored: Some((value, DurableSource::Staged(staged))),
                            rejected,
                        };
                    }
                }
            }
            Err(error) => rejected.push(DurableRejection {
                path: self.path.clone(),
                reason: format!("staged-file recovery scan failed: {error}"),
                quarantined_as: None,
            }),
        }

        let restored = self
            .try_candidate(&backup, &mut decode, &mut rejected)
            .map(|value| (value, DurableSource::Backup));
        DurableLoad { restored, rejected }
    }

    fn try_candidate<T, E: fmt::Display>(
        &self,
        path: &Path,
        decode: &mut impl FnMut(DurableCandidate<'_>) -> Result<T, E>,
        rejected: &mut Vec<DurableRejection>,
    ) -> Option<T> {
        let reason = match self.read_bounded(path) {
            ReadOutcome::Missing => return None,
            ReadOutcome::Bytes(bytes) => match self.decode_candidate(&bytes, decode) {
                Ok(value) => return Some(value),
                Err(reason) => reason,
            },
            ReadOutcome::Unusable(reason) => reason,
            ReadOutcome::Failed(error) => format!("could not be read: {error}"),
        };
        rejected.push(DurableRejection {
            path: path.to_path_buf(),
            reason,
            quarantined_as: None,
        });
        None
    }

    fn decode_candidate<T, E: fmt::Display>(
        &self,
        bytes: &[u8],
        decode: &mut impl FnMut(DurableCandidate<'_>) -> Result<T, E>,
    ) -> Result<T, String> {
        let (format, payload) = self.unwrap_envelope(bytes)?;
        decode(DurableCandidate { payload, format })
            .map_err(|error| format!("payload was rejected: {error}"))
    }

    fn unwrap_envelope<'a>(&self, bytes: &'a [u8]) -> Result<(DurableFormat, &'a [u8]), String> {
        match bytes.first() {
            None => return Err("is empty".to_string()),
            Some(&first) if first != DURABLE_CONTAINER_MAGIC[0] => {
                if bytes.len() > self.maximum_payload_bytes {
                    return Err(self.bound_reason());
                }
                return Ok((DurableFormat::Legacy, bytes));
            }
            Some(_) if !bytes.starts_with(&DURABLE_CONTAINER_MAGIC) => {
                return Err("has a damaged or truncated container magic".to_string());
            }
            Some(_) => {}
        }
        if bytes.len() < HEADER_BYTES {
            return Err("has a truncated container header".to_string());
        }
        let (header, payload) = bytes.split_at(HEADER_BYTES);
        let field = |range: std::ops::Range<usize>| &header[range];
        let envelope_version = u16::from_le_bytes(
            field(8..10)
                .try_into()
                .map_err(|_| "has an invalid envelope version field".to_string())?,
        );
        if envelope_version != DURABLE_CONTAINER_VERSION {
            return Err(format!(
                "uses unsupported container version {envelope_version}"
            ));
        }
        let schema_version = u32::from_le_bytes(
            field(10..14)
                .try_into()
                .map_err(|_| "has an invalid schema version field".to_string())?,
        );
        let declared = u64::from_le_bytes(
            field(14..22)
                .try_into()
                .map_err(|_| "has an invalid payload length field".to_string())?,
        );
        if u64::try_from(payload.len()).ok() != Some(declared) {
            return Err(format!(
                "holds {} payload bytes but declares {declared}",
                payload.len()
            ));
        }
        if payload.len() > self.maximum_payload_bytes {
            return Err(self.bound_reason());
        }
        if Sha256::digest(payload).as_slice() != field(22..HEADER_BYTES) {
            return Err("failed its payload checksum".to_string());
        }
        Ok((DurableFormat::Envelope { schema_version }, payload))
    }

    fn read_bounded(&self, path: &Path) -> ReadOutcome {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return ReadOutcome::Missing,
            Err(error) => return ReadOutcome::Failed(error),
        };
        let limit = self.maximum_payload_bytes.saturating_add(HEADER_BYTES);
        let Ok(read_limit) = u64::try_from(limit.saturating_add(1)) else {
            return ReadOutcome::Unusable(self.bound_reason());
        };
        let mut bytes = Vec::new();
        if let Err(error) = file.take(read_limit).read_to_end(&mut bytes) {
            return ReadOutcome::Failed(error);
        }
        if bytes.len() > limit {
            return ReadOutcome::Unusable(self.bound_reason());
        }
        ReadOutcome::Bytes(bytes)
    }

    fn bound_reason(&self) -> String {
        format!(
            "exceeds the {}-byte persistence bound",
            self.maximum_payload_bytes
        )
    }

    fn reject_primary(&self, mode: LoadMode, reason: String) -> DurableRejection {
        let (reason, quarantined_as) = match mode {
            LoadMode::ReadOnly => (reason, None),
            LoadMode::Recovering => match self.quarantine_primary() {
                Ok(quarantine) => (reason, Some(quarantine)),
                Err(error) => (format!("{reason}; quarantine failed: {error}"), None),
            },
        };
        DurableRejection {
            path: self.path.clone(),
            reason,
            quarantined_as,
        }
    }

    fn quarantine_primary(&self) -> io::Result<PathBuf> {
        for index in (1..MAXIMUM_QUARANTINED_COPIES).rev() {
            match move_file(
                &self.quarantine_path(index),
                &self.quarantine_path(index + 1),
            ) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let newest = self.quarantine_path(1);
        move_file(&self.path, &newest)?;
        Ok(newest)
    }

    /// Valid-looking staged temporaries of the primary, newest first, that are newer than
    /// the backup. Only exact [`StagedFile`] names for this file are considered.
    fn staged_candidates(&self, newer_than: Option<SystemTime>) -> io::Result<Vec<PathBuf>> {
        let Some(file_name) = self.path.file_name().and_then(|name| name.to_str()) else {
            return Ok(Vec::new());
        };
        let entries = match fs::read_dir(self.directory()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut candidates = Vec::new();
        for entry in entries.take(MAXIMUM_SCANNED_DIRECTORY_ENTRIES) {
            let entry = entry?;
            let name = entry.file_name();
            if parse_staged_temporary_name(&name).map(|staged| staged.destination)
                != Some(file_name)
            {
                continue;
            }
            let metadata = entry.metadata()?;
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            if metadata.is_file() && newer_than.is_none_or(|backup| modified > backup) {
                candidates.push((modified, entry.path()));
            }
        }
        candidates.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
        candidates.truncate(MAXIMUM_STAGED_CANDIDATES);
        Ok(candidates.into_iter().map(|(_, path)| path).collect())
    }

    /// Removes staged temporaries of the primary and backup that another process left.
    /// The per-data-root instance lock means no other process writes these files.
    fn remove_abandoned_temporaries(&self) {
        let Some(file_name) = self.path.file_name().and_then(|name| name.to_str()) else {
            return;
        };
        let backup_name = format!("{file_name}.bak");
        let entries = match fs::read_dir(self.directory()) {
            Ok(entries) => entries,
            Err(error) => {
                diagnostic!("Aeris staged-file cleanup skipped for {file_name}: {error}");
                return;
            }
        };
        for entry in entries.take(MAXIMUM_SCANNED_DIRECTORY_ENTRIES) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    diagnostic!("Aeris staged-file cleanup stopped for {file_name}: {error}");
                    return;
                }
            };
            let name = entry.file_name();
            let Some(staged) = parse_staged_temporary_name(&name) else {
                continue;
            };
            if (staged.destination == file_name || staged.destination == backup_name)
                && staged.process_id != std::process::id()
                && entry.file_type().is_ok_and(|kind| kind.is_file())
                && let Err(error) = fs::remove_file(entry.path())
            {
                diagnostic!(
                    "Aeris could not remove abandoned staged file {}: {error}",
                    entry.path().display()
                );
            }
        }
    }

    fn directory(&self) -> &Path {
        match self.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        }
    }

    fn sibling(&self, suffix: &str) -> PathBuf {
        let mut name = self
            .path
            .file_name()
            .map_or_else(OsString::new, std::ffi::OsStr::to_os_string);
        name.push(suffix);
        self.path.with_file_name(name)
    }
}

fn encode_envelope(schema_version: u32, payload: &[u8]) -> io::Result<Vec<u8>> {
    let length = u64::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "payload length overflows"))?;
    let mut envelope = Vec::with_capacity(HEADER_BYTES + payload.len());
    envelope.extend_from_slice(&DURABLE_CONTAINER_MAGIC);
    envelope.extend_from_slice(&DURABLE_CONTAINER_VERSION.to_le_bytes());
    envelope.extend_from_slice(&schema_version.to_le_bytes());
    envelope.extend_from_slice(&length.to_le_bytes());
    envelope.extend_from_slice(Sha256::digest(payload).as_slice());
    envelope.extend_from_slice(payload);
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    const BOUND: usize = 1_024;

    fn fixture(label: &str) -> (PathBuf, DurableContainer) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "aeris-durable-container-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("fixture root");
        let container = DurableContainer::new(root.join("state.pb"), BOUND);
        (root, container)
    }

    /// Accepts every verified payload except an empty one, as the desktop decoders do.
    fn accept_any(candidate: DurableCandidate<'_>) -> Result<(Vec<u8>, DurableFormat), String> {
        if candidate.payload.is_empty() {
            return Err("empty payload".to_string());
        }
        Ok((candidate.payload.to_vec(), candidate.format))
    }

    fn restored(load: DurableLoad<(Vec<u8>, DurableFormat)>) -> (Vec<u8>, DurableSource) {
        let ((payload, _), source) = load.restored.expect("a candidate is restored");
        (payload, source)
    }

    fn set_modified(path: &Path, modified: SystemTime) {
        File::options()
            .write(true)
            .open(path)
            .expect("fixture opens")
            .set_modified(modified)
            .expect("fixture time sets");
    }

    #[test]
    fn written_payload_round_trips_in_the_envelope() {
        let (root, container) = fixture("round-trip");
        container.write(7, b"payload").expect("write");

        let bytes = fs::read(container.path()).expect("primary");
        assert!(bytes.starts_with(&DURABLE_CONTAINER_MAGIC));
        assert_eq!(bytes.len(), HEADER_BYTES + b"payload".len());
        let load = container.load(accept_any);
        assert!(load.rejected.is_empty());
        let ((payload, format), source) = load.restored.expect("primary loads");
        assert_eq!(payload, b"payload");
        assert_eq!(format, DurableFormat::Envelope { schema_version: 7 });
        assert_eq!(source, DurableSource::Primary);
        assert!(
            !container.backup_path().exists(),
            "a first write has no prior file"
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn a_missing_directory_is_a_fresh_file_without_rejections() {
        let (root, _) = fixture("missing");
        let container = DurableContainer::new(root.join("absent").join("state.pb"), BOUND);
        for load in [container.load(accept_any), container.inspect(accept_any)] {
            assert!(load.restored.is_none());
            assert!(load.rejected.is_empty(), "{:?}", load.rejected);
        }
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn a_legacy_primary_loads_unchanged_and_becomes_the_first_backup() {
        let (root, container) = fixture("legacy");
        fs::write(container.path(), b"\x08\x01legacy").expect("legacy primary");

        let load = container.load(accept_any);
        let ((payload, format), source) = load.restored.expect("legacy loads");
        assert_eq!(payload, b"\x08\x01legacy");
        assert_eq!(format, DurableFormat::Legacy);
        assert_eq!(source, DurableSource::Primary);

        container.write(1, b"current").expect("write");
        assert_eq!(
            fs::read(container.backup_path()).expect("backup"),
            b"\x08\x01legacy",
            "the released-version file is kept as the last-good backup"
        );
        assert_eq!(restored(container.load(accept_any)).0, b"current");
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn a_truncated_primary_restores_the_backup_and_is_quarantined() {
        let (root, container) = fixture("truncated");
        container.write(1, b"first").expect("first write");
        container.write(1, b"second").expect("second write");
        let primary = fs::read(container.path()).expect("primary");
        fs::write(container.path(), &primary[..primary.len() - 2]).expect("truncate");

        let load = container.load(accept_any);
        assert_eq!(load.rejected.len(), 1);
        assert!(
            load.rejected[0].reason.contains("declares"),
            "{}",
            load.rejected[0]
        );
        assert_eq!(
            load.rejected[0].quarantined_as.as_deref(),
            Some(container.quarantine_path(1).as_path())
        );
        assert_eq!(restored(load), (b"first".to_vec(), DurableSource::Backup));
        assert!(!container.path().exists());
        assert_eq!(
            fs::read(container.quarantine_path(1)).expect("quarantine"),
            &primary[..primary.len() - 2]
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn a_checksum_mismatch_and_a_partial_magic_are_rejected() {
        let (root, container) = fixture("checksum");
        container.write(1, b"payload").expect("write");
        let mut primary = fs::read(container.path()).expect("primary");
        let last = primary.len() - 1;
        primary[last] ^= 0xFF;
        fs::write(container.path(), &primary).expect("flip");
        let load = container.inspect(accept_any);
        assert!(load.restored.is_none());
        assert!(load.rejected[0].reason.contains("checksum"));
        assert!(container.path().exists(), "inspect never quarantines");

        fs::write(container.path(), &DURABLE_CONTAINER_MAGIC[..3]).expect("partial magic");
        assert!(
            container.inspect(accept_any).rejected[0]
                .reason
                .contains("magic")
        );
        fs::write(container.path(), b"").expect("empty");
        assert!(
            container.inspect(accept_any).rejected[0]
                .reason
                .contains("empty")
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn a_staged_temporary_left_by_a_crash_is_used() {
        let (root, container) = fixture("staged");
        container.write(1, b"old").expect("first write");
        container.write(1, b"older-backup").expect("second write");
        let backup_time = SystemTime::now() - Duration::from_secs(60);
        set_modified(&container.backup_path(), backup_time);
        fs::remove_file(container.path()).expect("primary lost");
        let staged = root.join(".state.pb.4000000000-9.tmp");
        fs::write(&staged, encode_envelope(1, b"crashed").expect("envelope")).expect("staged");
        let corrupt = root.join(".state.pb.4000000000-10.tmp");
        fs::write(&corrupt, b"\0AERISDC partial").expect("corrupt staged");
        let stale = root.join(".state.pb.4000000000-1.tmp");
        fs::write(&stale, encode_envelope(1, b"stale").expect("envelope")).expect("stale");
        set_modified(&stale, backup_time - Duration::from_secs(60));
        fs::write(
            root.join(".other.4000000000-11.tmp"),
            encode_envelope(1, b"other").expect("envelope"),
        )
        .expect("other");
        set_modified(&staged, backup_time + Duration::from_secs(10));
        set_modified(&corrupt, backup_time + Duration::from_secs(20));

        let load = container.load(accept_any);
        assert_eq!(
            load.rejected.len(),
            1,
            "only the corrupt temporary is rejected"
        );
        assert_eq!(load.rejected[0].path, corrupt);
        assert_eq!(
            restored(load),
            (b"crashed".to_vec(), DurableSource::Staged(staged.clone()))
        );

        container.write(1, b"next").expect("write after recovery");
        assert!(!staged.exists() && !corrupt.exists() && !stale.exists());
        assert!(root.join(".other.4000000000-11.tmp").exists());
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn quarantine_keeps_the_newest_three_copies() {
        let (root, container) = fixture("quarantine");
        for generation in 1..=4_u8 {
            fs::write(container.path(), [b'\0', generation]).expect("corrupt primary");
            let load = container.load(accept_any);
            assert!(load.restored.is_none());
        }
        for (index, generation) in [(1, 4_u8), (2, 3), (3, 2)] {
            assert_eq!(
                fs::read(container.quarantine_path(index)).expect("quarantined copy"),
                [b'\0', generation]
            );
        }
        assert!(!container.quarantine_path(4).exists());
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn an_oversized_primary_is_quarantined_and_an_oversized_write_is_refused() {
        let (root, container) = fixture("oversized");
        fs::write(container.path(), vec![b'a'; BOUND + HEADER_BYTES + 1]).expect("oversized");
        let load = container.load(accept_any);
        assert!(load.restored.is_none());
        assert!(load.rejected[0].reason.contains("bound"));
        assert!(container.quarantine_path(1).exists());

        fs::write(container.path(), vec![b'a'; BOUND + 1]).expect("oversized legacy");
        assert!(
            container.inspect(accept_any).rejected[0]
                .reason
                .contains("bound")
        );
        assert_eq!(
            container
                .write(1, &vec![0; BOUND + 1])
                .expect_err("oversized write")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn a_decoder_rejection_falls_back_and_an_invalid_primary_never_replaces_the_backup() {
        let (root, container) = fixture("decoder");
        container.write(1, b"good").expect("first write");
        container.write(1, b"bad").expect("second write");
        let decode = |candidate: DurableCandidate<'_>| {
            if candidate.payload == b"bad" {
                Err("not decodable")
            } else {
                Ok(candidate.payload.to_vec())
            }
        };
        let load = container.inspect(decode);
        assert_eq!(
            load.restored.map(|(value, _)| value),
            Some(b"good".to_vec())
        );

        fs::write(container.path(), b"\0garbage").expect("corrupt primary");
        container.write(1, b"third").expect("third write");
        assert_eq!(
            restored(DurableContainer::new(container.backup_path(), BOUND).inspect(accept_any)).0,
            b"good",
            "a corrupt primary is not rotated into the backup"
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }
}
