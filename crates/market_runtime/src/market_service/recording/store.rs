//! On-disk layout and size accounting of market recordings.
//!
//! ```text
//! <data root>/recordings/<instrument key>/instrument.json
//! <data root>/recordings/<instrument key>/session-<unix ms>/segment-<n>.arec
//! ```
//!
//! The store indexes only directories carrying a valid `instrument.json` and
//! files matching the segment naming; anything else in the directory is left
//! untouched and is not counted. Removal deletes only indexed files and the
//! directories they leave empty.

use super::MarketRecordingFailure;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

pub(super) const INSTRUMENT_INDEX_FILE: &str = "instrument.json";
const INSTRUMENT_INDEX_FORMAT: u64 = 1;
const SESSION_PREFIX: &str = "session-";
const SEGMENT_PREFIX: &str = "segment-";
const SEGMENT_EXTENSION: &str = ".arec";
/// Directory entries one scan may visit, which bounds startup work and the index.
pub(super) const MAXIMUM_SCANNED_ENTRIES: usize = 65_536;
const MAXIMUM_INSTRUMENT_INDEX_BYTES: u64 = 16 * 1024;
const MAXIMUM_KEY_TEXT: usize = 40;
const HEX: &[u8; 16] = b"0123456789abcdef";

/// Oldest-first eviction order across every instrument.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct SegmentOrder {
    pub(super) session_millis: u64,
    pub(super) segment: u32,
    pub(super) instrument_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct StoredInstrument {
    pub(super) provider_id: String,
    pub(super) instrument_id: String,
    pub(super) display_symbol: String,
    pub(super) bytes: u64,
}

pub(super) struct RecordingStore {
    directory: PathBuf,
    segments: BTreeMap<SegmentOrder, u64>,
    instruments: BTreeMap<String, StoredInstrument>,
    used_bytes: u64,
}

/// A filesystem-safe, collision-resistant directory name for one instrument.
pub(super) fn instrument_key(provider_id: &str, instrument_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(provider_id.as_bytes());
    digest.update([0]);
    digest.update(instrument_id.as_bytes());
    let digest = digest.finalize();
    let mut hash = String::with_capacity(12);
    for byte in &digest[..6] {
        hash.push(char::from(HEX[usize::from(byte >> 4)]));
        hash.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    format!(
        "{}-{}-{hash}",
        key_text(provider_id),
        key_text(instrument_id)
    )
}

fn key_text(value: &str) -> String {
    value
        .chars()
        .take(MAXIMUM_KEY_TEXT)
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

pub(super) fn session_directory_name(session_millis: u64) -> String {
    format!("{SESSION_PREFIX}{session_millis:013}")
}

pub(super) fn segment_file_name(segment: u32) -> String {
    format!("{SEGMENT_PREFIX}{segment:06}{SEGMENT_EXTENSION}")
}

fn parse_number<T: std::str::FromStr>(name: &str, prefix: &str, suffix: &str) -> Option<T> {
    let digits = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

pub(super) fn instrument_index_bytes(
    provider_id: &str,
    instrument_id: &str,
    display_symbol: &str,
) -> Vec<u8> {
    serde_json::json!({
        "format": INSTRUMENT_INDEX_FORMAT,
        "provider_id": provider_id,
        "instrument_id": instrument_id,
        "display_symbol": display_symbol,
    })
    .to_string()
    .into_bytes()
}

fn read_instrument_index(path: &Path) -> Option<StoredInstrument> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAXIMUM_INSTRUMENT_INDEX_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if value.get("format")?.as_u64()? != INSTRUMENT_INDEX_FORMAT {
        return None;
    }
    let text = |field: &str| value.get(field)?.as_str().map(str::to_owned);
    Some(StoredInstrument {
        provider_id: text("provider_id")?,
        instrument_id: text("instrument_id")?,
        display_symbol: text("display_symbol")?,
        bytes: 0,
    })
}

impl RecordingStore {
    /// Indexes the existing recordings without creating anything.
    pub(super) fn open(directory: PathBuf) -> Result<Self, MarketRecordingFailure> {
        let mut store = Self {
            directory,
            segments: BTreeMap::new(),
            instruments: BTreeMap::new(),
            used_bytes: 0,
        };
        let mut visited = 0_usize;
        let mut visit = || {
            visited += 1;
            if visited > MAXIMUM_SCANNED_ENTRIES {
                Err(MarketRecordingFailure::TooManyFiles)
            } else {
                Ok(())
            }
        };
        let instruments = match std::fs::read_dir(&store.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(store),
            Err(_) => return Err(MarketRecordingFailure::StorageUnavailable),
        };
        for entry in instruments {
            visit()?;
            let entry = entry.map_err(|_| MarketRecordingFailure::StorageUnavailable)?;
            let Some(key) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(instrument) = read_instrument_index(&path.join(INSTRUMENT_INDEX_FILE)) else {
                continue;
            };
            if instrument_key(&instrument.provider_id, &instrument.instrument_id) != key {
                continue;
            }
            store.instruments.insert(key.clone(), instrument);
            let Ok(sessions) = std::fs::read_dir(&path) else {
                continue;
            };
            for session in sessions {
                visit()?;
                let Ok(session) = session else { continue };
                let Some(session_millis) = session
                    .file_name()
                    .to_str()
                    .and_then(|name| parse_number::<u64>(name, SESSION_PREFIX, ""))
                else {
                    continue;
                };
                let Ok(segments) = std::fs::read_dir(session.path()) else {
                    continue;
                };
                for segment in segments {
                    visit()?;
                    let Ok(segment) = segment else { continue };
                    let Some(number) = segment.file_name().to_str().and_then(|name| {
                        parse_number::<u32>(name, SEGMENT_PREFIX, SEGMENT_EXTENSION)
                    }) else {
                        continue;
                    };
                    let Ok(metadata) = segment.metadata() else {
                        continue;
                    };
                    if metadata.is_file() {
                        store.insert(
                            SegmentOrder {
                                session_millis,
                                segment: number,
                                instrument_key: key.clone(),
                            },
                            metadata.len(),
                        );
                    }
                }
            }
        }
        Ok(store)
    }

    pub(super) fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    pub(super) fn instruments(&self) -> impl Iterator<Item = &StoredInstrument> {
        self.instruments.values()
    }

    pub(super) fn instrument_directory(&self, key: &str) -> PathBuf {
        self.directory.join(key)
    }

    pub(super) fn segment_path(&self, order: &SegmentOrder) -> PathBuf {
        self.directory
            .join(&order.instrument_key)
            .join(session_directory_name(order.session_millis))
            .join(segment_file_name(order.segment))
    }

    pub(super) fn has_instrument(&self, key: &str) -> bool {
        self.instruments.contains_key(key)
    }

    pub(super) fn add_instrument(&mut self, key: String, instrument: StoredInstrument) {
        self.instruments.entry(key).or_insert(instrument);
    }

    pub(super) fn insert(&mut self, order: SegmentOrder, bytes: u64) {
        if let Some(instrument) = self.instruments.get_mut(&order.instrument_key) {
            instrument.bytes = instrument.bytes.saturating_add(bytes);
        }
        self.used_bytes = self.used_bytes.saturating_add(bytes);
        let previous = self.segments.insert(order, bytes);
        debug_assert!(previous.is_none(), "segment indexed twice");
    }

    pub(super) fn grow(&mut self, order: &SegmentOrder, bytes: u64) {
        if let Some(size) = self.segments.get_mut(order) {
            *size = size.saturating_add(bytes);
            self.used_bytes = self.used_bytes.saturating_add(bytes);
            if let Some(instrument) = self.instruments.get_mut(&order.instrument_key) {
                instrument.bytes = instrument.bytes.saturating_add(bytes);
            }
        }
    }

    pub(super) fn oldest(&self) -> Option<&SegmentOrder> {
        self.segments.keys().next()
    }

    /// Deletes one indexed segment and the session directory it leaves empty.
    pub(super) fn remove_segment(&mut self, order: &SegmentOrder) -> io::Result<()> {
        let path = self.segment_path(order);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Some(bytes) = self.segments.remove(order) {
            self.used_bytes = self.used_bytes.saturating_sub(bytes);
            if let Some(instrument) = self.instruments.get_mut(&order.instrument_key) {
                instrument.bytes = instrument.bytes.saturating_sub(bytes);
            }
        }
        if let Some(session) = path.parent() {
            remove_empty_directory(session)?;
        }
        Ok(())
    }

    pub(super) fn segments_of(&self, key: &str) -> Vec<SegmentOrder> {
        self.segments
            .keys()
            .filter(|order| order.instrument_key == key)
            .cloned()
            .collect()
    }

    /// Deletes every indexed segment of the instrument, its index file and the
    /// directories left empty.
    pub(super) fn remove_instrument(&mut self, key: &str) -> io::Result<()> {
        for order in self.segments_of(key) {
            self.remove_segment(&order)?;
        }
        if self.instruments.remove(key).is_some() {
            let directory = self.instrument_directory(key);
            match std::fs::remove_file(directory.join(INSTRUMENT_INDEX_FILE)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            remove_empty_directory(&directory)?;
        }
        Ok(())
    }

    pub(super) fn instrument_keys(&self) -> Vec<String> {
        self.instruments.keys().cloned().collect()
    }
}

/// Removes `directory` only when it exists and holds nothing.
fn remove_empty_directory(directory: &Path) -> io::Result<()> {
    let mut entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if entries.next().is_some() {
        return Ok(());
    }
    std::fs::remove_dir(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instrument_keys_are_safe_and_distinguish_case_and_punctuation() {
        let key = instrument_key("provider-a", "ES.FUT/CME:202612");
        assert!(key.starts_with("provider_a-es_fut_cme_202612-"));
        assert!(
            key.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        );
        assert_ne!(key, instrument_key("provider-a", "es.fut/cme:202612"));
        assert_ne!(key, instrument_key("provider-a", "ES_FUT_CME_202612"));
        assert!(instrument_key("p", &"x".repeat(1_000)).len() < 64);
    }

    #[test]
    fn names_parse_only_their_exact_shape() {
        assert_eq!(
            parse_number::<u64>(&session_directory_name(42), SESSION_PREFIX, ""),
            Some(42)
        );
        assert_eq!(
            parse_number::<u32>(&segment_file_name(7), SEGMENT_PREFIX, SEGMENT_EXTENSION),
            Some(7)
        );
        assert_eq!(parse_number::<u64>("session-", SESSION_PREFIX, ""), None);
        assert_eq!(parse_number::<u64>("session-1a", SESSION_PREFIX, ""), None);
        assert_eq!(
            parse_number::<u32>("segment-1.arec.tmp", SEGMENT_PREFIX, SEGMENT_EXTENSION),
            None
        );
    }
}
