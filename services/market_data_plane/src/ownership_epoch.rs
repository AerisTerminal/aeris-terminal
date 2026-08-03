//! Crash-safe ownership epoch reservation for durable partition writers.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const MAXIMUM_EPOCH_FILE_BYTES: u64 = 32;

/// Exclusive ownership of one durable-writer epoch.
///
/// The state-file lock is retained until this value is dropped, so another
/// process cannot reserve or publish under a newer epoch while this writer is
/// still active.
pub struct OwnershipLease {
    epoch: u64,
    _locked_file: File,
}

impl OwnershipLease {
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }
}

/// Atomically reserves the next process ownership epoch in a caller-owned file.
///
/// The parent directory must already exist. The exclusive file lock prevents two
/// concurrently starting writers from receiving the same epoch, and the synced
/// value is advanced before the caller can publish.
///
/// # Errors
///
/// Returns an error for missing parents, malformed state, overflow, or I/O failure.
pub fn reserve_next(path: &Path) -> Result<OwnershipLease, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "ownership epoch path requires a parent directory".to_string())?;
    if !parent.is_dir() {
        return Err("ownership epoch parent directory does not exist".to_string());
    }
    let existed = path.exists();
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("cannot open ownership epoch state: {error}"))?;
    file.try_lock()
        .map_err(|error| format!("ownership epoch state already has an active writer: {error}"))?;
    let length = file
        .metadata()
        .map_err(|error| format!("cannot inspect ownership epoch state: {error}"))?
        .len();
    if length > MAXIMUM_EPOCH_FILE_BYTES {
        return Err("ownership epoch state is oversized".to_string());
    }
    let mut encoded = String::new();
    file.read_to_string(&mut encoded)
        .map_err(|error| format!("cannot read ownership epoch state: {error}"))?;
    let previous = if encoded.trim().is_empty() {
        0
    } else {
        encoded
            .trim()
            .parse::<u64>()
            .map_err(|_| "ownership epoch state is malformed".to_string())?
    };
    let next = previous
        .checked_add(1)
        .ok_or_else(|| "ownership epoch is exhausted".to_string())?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("cannot seek ownership epoch state: {error}"))?;
    file.set_len(0)
        .map_err(|error| format!("cannot truncate ownership epoch state: {error}"))?;
    writeln!(file, "{next}")
        .map_err(|error| format!("cannot write ownership epoch state: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("cannot sync ownership epoch state: {error}"))?;
    if !existed {
        sync_parent_directory(parent)?;
    }
    Ok(OwnershipLease {
        epoch: next,
        _locked_file: file,
    })
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), String> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("cannot sync ownership epoch directory: {error}"))
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::reserve_next;

    #[test]
    fn reservations_advance_durably_and_reject_malformed_state() {
        let root = std::env::temp_dir().join(format!(
            "axiusflow-ownership-epoch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("post-epoch time")
                .as_nanos()
        ));
        std::fs::create_dir(&root).expect("test directory");
        let state = root.join("epoch");
        let first = reserve_next(&state).expect("first reservation");
        assert_eq!(first.epoch(), 1);
        assert!(reserve_next(&state).is_err());
        drop(first);
        let second = reserve_next(&state).expect("second reservation");
        assert_eq!(second.epoch(), 2);
        drop(second);
        std::fs::write(&state, "not-an-epoch\n").expect("replace test state");
        assert!(reserve_next(&state).is_err());
        std::fs::remove_file(state).expect("remove state");
        std::fs::remove_dir(root).expect("remove test directory");
    }
}
