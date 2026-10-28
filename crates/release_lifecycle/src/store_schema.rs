//! The trading store's schema as the launcher sees it on disk. The launcher cannot open the
//! store (only `trading_runtime` owns SQLite), so it reads the schema version from the database
//! header, and it treats a write-ahead log that still holds frames as a possible newer schema.

use std::{
    fs::{self, File},
    io::Read as _,
    path::{Path, PathBuf},
};

use crate::LifecycleError;

const SQLITE_HEADER_MAGIC: &[u8; 16] = b"SQLite format 3\0";
const SQLITE_HEADER_BYTES: usize = 100;
const USER_VERSION_OFFSET: usize = 60;

/// The trading database below an Aeris data root. The desktop opens it there and the launcher
/// reads its schema from it before a rollback.
#[must_use]
pub fn trading_database_path(data_root: &Path) -> PathBuf {
    data_root.join("trading").join("trading.sqlite3")
}

/// The trading store schema a rollback must respect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiveStoreSchema {
    /// No trading database exists yet.
    Absent,
    /// The database header holds every committed change: the write-ahead log is empty.
    Checkpointed(u32),
    /// The header holds this version, but the write-ahead log still holds frames that a
    /// release may have committed after it, including a migration.
    Uncheckpointed(u32),
}

impl LiveStoreSchema {
    /// Whether a release that opens stores up to `target_max_store_schema` can open this store
    /// after `failed_max_store_schema` ran on it. A store with pending log frames counts as
    /// migrated to the failed release's schema, because the header cannot rule that out.
    #[must_use]
    pub fn allows_rollback(
        self,
        failed_max_store_schema: u32,
        target_max_store_schema: u32,
    ) -> bool {
        match self {
            Self::Absent => true,
            Self::Checkpointed(version) => version <= target_max_store_schema,
            Self::Uncheckpointed(version) => {
                version.max(failed_max_store_schema) <= target_max_store_schema
            }
        }
    }
}

/// Reads the schema version of the trading database at `database`.
///
/// # Errors
/// Returns [`LifecycleError::RollbackFailed`] when the database or its log cannot be read or the
/// file is not a SQLite database, so a rollback fails closed.
pub fn read_live_store_schema(database: &Path) -> Result<LiveStoreSchema, LifecycleError> {
    let metadata = match fs::symlink_metadata(database) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LiveStoreSchema::Absent);
        }
        Err(_) => return Err(LifecycleError::RollbackFailed),
    };
    if !metadata.is_file() {
        return Err(LifecycleError::RollbackFailed);
    }
    // SQLite treats an empty file as an empty database with user_version 0.
    let version = if metadata.len() == 0 {
        0
    } else {
        let mut header = [0_u8; SQLITE_HEADER_BYTES];
        File::open(database)
            .and_then(|mut file| file.read_exact(&mut header))
            .map_err(|_| LifecycleError::RollbackFailed)?;
        if !header.starts_with(SQLITE_HEADER_MAGIC) {
            return Err(LifecycleError::RollbackFailed);
        }
        let mut user_version = [0_u8; 4];
        user_version.copy_from_slice(&header[USER_VERSION_OFFSET..USER_VERSION_OFFSET + 4]);
        u32::from_be_bytes(user_version)
    };
    let mut log = database.as_os_str().to_owned();
    log.push("-wal");
    match fs::symlink_metadata(PathBuf::from(log)) {
        Ok(metadata) if metadata.len() > 0 => Ok(LiveStoreSchema::Uncheckpointed(version)),
        Ok(_) => Ok(LiveStoreSchema::Checkpointed(version)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(LiveStoreSchema::Checkpointed(version))
        }
        Err(_) => Err(LifecycleError::RollbackFailed),
    }
}

#[cfg(test)]
mod tests {
    use super::{LiveStoreSchema, read_live_store_schema, trading_database_path};
    use crate::LifecycleError;
    use std::{fs, path::PathBuf};

    fn temporary_root(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!("aeris-store-schema-{name}-{nanos}"));
        fs::create_dir_all(&root).expect("temporary root");
        root
    }

    fn header(user_version: u32) -> Vec<u8> {
        let mut header = vec![0_u8; 4096];
        header[..16].copy_from_slice(b"SQLite format 3\0");
        header[60..64].copy_from_slice(&user_version.to_be_bytes());
        header
    }

    #[test]
    fn schema_comes_from_the_header_and_a_nonempty_log_marks_it_uncheckpointed() {
        let root = temporary_root("header");
        let database = trading_database_path(&root);
        assert_eq!(
            read_live_store_schema(&database),
            Ok(LiveStoreSchema::Absent)
        );
        fs::create_dir_all(database.parent().expect("trading folder")).expect("trading folder");
        fs::write(&database, b"").expect("empty database");
        assert_eq!(
            read_live_store_schema(&database),
            Ok(LiveStoreSchema::Checkpointed(0))
        );
        fs::write(&database, header(30)).expect("database header");
        assert_eq!(
            read_live_store_schema(&database),
            Ok(LiveStoreSchema::Checkpointed(30))
        );
        let log = database.with_file_name("trading.sqlite3-wal");
        fs::write(&log, b"").expect("truncated log");
        assert_eq!(
            read_live_store_schema(&database),
            Ok(LiveStoreSchema::Checkpointed(30))
        );
        fs::write(&log, [1_u8; 32]).expect("log with frames");
        assert_eq!(
            read_live_store_schema(&database),
            Ok(LiveStoreSchema::Uncheckpointed(30))
        );
        fs::write(&database, [b'x'; 200]).expect("foreign file");
        assert_eq!(
            read_live_store_schema(&database),
            Err(LifecycleError::RollbackFailed)
        );
        fs::write(&database, b"SQLite format 3\0short").expect("short header");
        assert_eq!(
            read_live_store_schema(&database),
            Err(LifecycleError::RollbackFailed)
        );
        fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn rollback_needs_a_target_that_opens_the_live_schema() {
        assert!(LiveStoreSchema::Absent.allows_rollback(31, 30));
        assert!(LiveStoreSchema::Checkpointed(30).allows_rollback(31, 30));
        assert!(!LiveStoreSchema::Checkpointed(31).allows_rollback(31, 30));
        assert!(LiveStoreSchema::Uncheckpointed(30).allows_rollback(30, 30));
        assert!(!LiveStoreSchema::Uncheckpointed(30).allows_rollback(31, 30));
        assert!(!LiveStoreSchema::Uncheckpointed(32).allows_rollback(30, 31));
    }
}
