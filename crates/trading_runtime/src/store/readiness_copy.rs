use super::StoreError;
use super::schema::{
    PREFLIGHT_SCRATCH, READINESS_SCRATCH, copy_database_files, is_migration_backup,
    is_owned_scratch_name, scratch_directory, scratch_prefix, sqlite_copy_names,
    sweep_stale_scratch_directories,
};
use std::{
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
};

/// A private copy of the trading database for a candidate build's readiness check.
///
/// The database file and its SQLite sidecars are copied without opening the original, so
/// the candidate opens and migrates only the copy: the user's database keeps its bytes and
/// `user_version`, and the release it came from can still open it after a failed or
/// rolled-back update. When no database exists yet the scratch directory stays empty, the
/// candidate starts a fresh store inside it and the real file is never created.
///
/// Dropping the copy removes its scratch directory. A removal failure is reported through
/// observability, and the next store open or readiness copy sweeps the leftover.
pub struct ReadinessDatabaseCopy {
    scratch: PathBuf,
    database: PathBuf,
}

impl ReadinessDatabaseCopy {
    /// Copies the trading database at `database` into a new scratch directory beside it.
    ///
    /// The caller holds the data root's instance lock, so no SQLite connection writes the
    /// database or its write-ahead log during the copy and the copied files are consistent.
    ///
    /// # Errors
    /// Returns an error when the scratch directory cannot be created or the database
    /// cannot be read or copied.
    pub fn create(database: &Path) -> Result<Self, StoreError> {
        let parent = database
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| {
                StoreError::Other("trading database has no parent directory".to_string())
            })?;
        let file_name = database
            .file_name()
            .ok_or_else(|| StoreError::Other("trading database has no file name".to_string()))?;
        fs::create_dir_all(parent).map_err(|error| {
            StoreError::Other(format!(
                "trading data directory could not be created: {error}"
            ))
        })?;
        sweep_stale_scratch_directories(database);
        let scratch = scratch_directory(parent, file_name, READINESS_SCRATCH);
        fs::create_dir(&scratch).map_err(|error| {
            StoreError::Other(format!(
                "trading readiness scratch directory could not be created: {error}"
            ))
        })?;
        let copy = Self {
            database: scratch.join(file_name),
            scratch,
        };
        if database.exists() {
            copy_database_files(database, &copy.scratch)
                .map_err(|error| StoreError::Other(format!("trading readiness {error}")))?;
        }
        Ok(copy)
    }

    /// The copy's database path; start the candidate trading service here.
    #[must_use]
    pub fn database_path(&self) -> &Path {
        &self.database
    }
}

impl Drop for ReadinessDatabaseCopy {
    fn drop(&mut self) {
        let Some(database_name) = self.database.file_name() else {
            return;
        };
        if let Err(error) = remove_readiness_scratch(&self.scratch, database_name) {
            aeris_observability::diagnostic!(
                "trading readiness copy cleanup failed for {}: {error}",
                self.scratch.display()
            );
        }
    }
}

/// Removes a readiness scratch directory only when it holds exactly what a candidate's
/// store open leaves there: the database copy and its SQLite sidecars, the copy's
/// migration backups and its store-open preflight scratch. Anything else keeps the whole
/// directory in place.
pub(super) fn remove_readiness_scratch(scratch: &Path, database_name: &OsStr) -> io::Result<()> {
    let metadata = fs::symlink_metadata(scratch)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::other(
            "readiness scratch is not a plain directory",
        ));
    }
    let mut copy_names = sqlite_copy_names(database_name).to_vec();
    copy_names.push(OsString::from(format!(
        "{}-journal",
        database_name.to_string_lossy()
    )));
    let preflight = scratch_prefix(database_name, PREFLIGHT_SCRATCH);
    let mut files = Vec::new();
    let mut directories = Vec::new();
    for entry in fs::read_dir(scratch)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        if file_type.is_file() && copy_names.contains(&name) {
            files.push(entry.path());
        } else if file_type.is_dir() && name == "backups" {
            collect_owned_files(&entry.path(), is_migration_backup, &mut files)?;
            directories.push(entry.path());
        } else if file_type.is_dir() && is_owned_scratch_name(&name, &preflight) {
            collect_owned_files(
                &entry.path(),
                |name| copy_names[..3].iter().any(|expected| expected == name),
                &mut files,
            )?;
            directories.push(entry.path());
        } else {
            return Err(io::Error::other(
                "readiness scratch contains an unexpected file or link",
            ));
        }
    }
    for file in files {
        fs::remove_file(file)?;
    }
    for directory in directories {
        fs::remove_dir(directory)?;
    }
    fs::remove_dir(scratch)
}

fn collect_owned_files(
    directory: &Path,
    owned: impl Fn(&OsStr) -> bool,
    files: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || !owned(&entry.file_name()) {
            return Err(io::Error::other(
                "readiness scratch contains an unexpected file or link",
            ));
        }
        files.push(entry.path());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::tests::temporary_database;
    use super::super::{TradingStore, create_schema_at, open_read_only};
    use super::*;
    use crate::{
        ExecutionPolicy, SCHEMA_VERSION, TradingRetention, TradingService, TradingServiceConfig,
    };
    use rusqlite::Connection;
    use std::sync::Arc;

    fn user_version(path: &Path) -> u32 {
        open_read_only(path)
            .expect("database opens read-only")
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("user_version")
    }

    /// Reads `user_version` from the database header without a SQLite connection, which
    /// would create sidecar files beside it.
    fn header_user_version(path: &Path) -> u32 {
        let bytes = fs::read(path).expect("database bytes");
        u32::from_be_bytes(bytes[60..64].try_into().expect("header field"))
    }

    fn create_fixture(connection: &Connection) {
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .expect("fixture uses WAL like the production store");
        create_schema_at(connection, 20);
        connection
            .execute(
                "INSERT INTO user_records(kind,id,revision,updated_unix_nanos,json)
                 VALUES ('note','readiness-record',1,1,'{\"text\":\"fixture\"}')",
                [],
            )
            .expect("fixture row");
    }

    fn copied_record(copy: &ReadinessDatabaseCopy) -> String {
        open_read_only(copy.database_path())
            .expect("copy opens")
            .query_row(
                "SELECT id FROM user_records WHERE kind = 'note'",
                [],
                |row| row.get(0),
            )
            .expect("copy carries the user's records")
    }

    fn entries(directory: &Path) -> Vec<OsString> {
        let mut names = fs::read_dir(directory)
            .expect("directory lists")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn run_candidate(copy: &ReadinessDatabaseCopy) {
        let service = TradingService::start(TradingServiceConfig {
            database_path: copy.database_path().to_path_buf(),
            retention: TradingRetention::default(),
            clock: Arc::new(crate::clock::SystemClock::default()),
            execution: ExecutionPolicy::default(),
        })
        .expect("candidate trading owner starts on the copy");
        assert!(service.status().expect("owner status").schema_version > 0);
        service
            .shutdown_after_owner_exit_for_test()
            .expect("candidate trading owner stops");
        drop(service);
        assert_eq!(user_version(copy.database_path()), SCHEMA_VERSION);
    }

    #[test]
    fn readiness_migrates_its_copy_and_leaves_the_database_unchanged() {
        let path = temporary_database("readiness-copy");
        let parent = path.parent().expect("fixture parent").to_path_buf();
        create_fixture(&Connection::open(&path).expect("v20 fixture opens"));
        let before = fs::read(&path).expect("database bytes");

        let copy = ReadinessDatabaseCopy::create(&path).expect("readiness copy");
        run_candidate(&copy);
        assert_eq!(copied_record(&copy), "readiness-record");
        drop(copy);

        assert_eq!(fs::read(&path).expect("database bytes"), before);
        assert_eq!(header_user_version(&path), 20);
        assert_eq!(entries(&parent), [OsString::from("trading.sqlite3")]);
        fs::remove_dir_all(parent).expect("fixture cleanup");
    }

    #[test]
    fn readiness_copies_uncheckpointed_wal_frames_without_changing_them() {
        let path = temporary_database("readiness-wal");
        let parent = path.parent().expect("fixture parent").to_path_buf();
        // The open fixture connection keeps the schema and record in the WAL, as a crash
        // before the shutdown checkpoint would.
        let fixture = Connection::open(&path).expect("v20 fixture opens");
        create_fixture(&fixture);
        let sidecars = [
            "trading.sqlite3",
            "trading.sqlite3-shm",
            "trading.sqlite3-wal",
        ]
        .map(|name| parent.join(name));
        let before = sidecars
            .iter()
            .map(|file| fs::read(file).expect("fixture file"))
            .collect::<Vec<_>>();

        let copy = ReadinessDatabaseCopy::create(&path).expect("readiness copy");
        run_candidate(&copy);
        assert_eq!(copied_record(&copy), "readiness-record");
        drop(copy);

        for (file, bytes) in sidecars.iter().zip(before) {
            assert_eq!(
                fs::read(file).expect("fixture file"),
                bytes,
                "{}",
                file.display()
            );
        }
        assert_eq!(
            entries(&parent),
            sidecars.map(|file| file.file_name().expect("name").to_os_string())
        );
        let version: u32 = fixture
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("user_version");
        assert_eq!(version, 20);
        drop(fixture);
        fs::remove_dir_all(parent).expect("fixture cleanup");
    }

    #[test]
    fn fresh_install_readiness_never_creates_the_database() {
        let path = temporary_database("readiness-fresh");
        let parent = path.parent().expect("fixture parent").to_path_buf();

        let copy = ReadinessDatabaseCopy::create(&path).expect("readiness copy");
        run_candidate(&copy);
        drop(copy);

        assert!(!path.exists());
        assert_eq!(entries(&parent), Vec::<OsString>::new());
        fs::remove_dir_all(parent).expect("fixture cleanup");
    }

    #[test]
    fn store_open_sweeps_only_owned_stale_readiness_copies() {
        let path = temporary_database("stale-readiness");
        let parent = path.parent().expect("fixture parent").to_path_buf();
        let stale = parent.join(".trading.sqlite3.readiness-123-456");
        let preflight = stale.join(".trading.sqlite3.preflight-123-789");
        fs::create_dir_all(stale.join("backups")).expect("stale backups");
        fs::create_dir(&preflight).expect("stale nested preflight");
        fs::write(stale.join("trading.sqlite3"), b"old copy").expect("stale copy");
        fs::write(stale.join("trading.sqlite3-wal"), b"old WAL").expect("stale WAL");
        fs::write(
            stale.join("backups").join("trading-v20-1.sqlite3"),
            b"old backup",
        )
        .expect("stale backup");
        fs::write(preflight.join("trading.sqlite3"), b"old preflight").expect("preflight copy");
        let foreign = parent.join(".trading.sqlite3.readiness-123-999");
        fs::create_dir_all(foreign.join("backups")).expect("foreign directory");
        fs::write(foreign.join("backups").join("user-note"), b"keep").expect("foreign data");

        let store = TradingStore::open(&path, TradingRetention::default()).expect("new store");
        assert!(!stale.exists());
        assert_eq!(
            fs::read(foreign.join("backups").join("user-note")).expect("foreign data kept"),
            b"keep"
        );
        drop(store);
        fs::remove_dir_all(parent).expect("fixture cleanup");
    }
}
