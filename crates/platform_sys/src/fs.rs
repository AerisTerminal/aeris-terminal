use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const MAXIMUM_CREATE_ATTEMPTS: u64 = 1_024;

/// A same-directory temporary file that replaces its destination only on commit.
///
/// Dropping an uncommitted value removes its temporary file.
pub struct StagedFile {
    destination: PathBuf,
    temporary: PathBuf,
    file: Option<File>,
    committed: bool,
}

impl StagedFile {
    /// Creates a uniquely named temporary file beside `destination`.
    ///
    /// # Errors
    /// Returns an error when the destination has no file name or the temporary
    /// file cannot be created.
    pub fn create(destination: &Path) -> io::Result<Self> {
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        let file_name = destination.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name")
        })?;

        for _ in 0..MAXIMUM_CREATE_ATTEMPTS {
            let sequence = NEXT_TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let temporary = parent.join(temporary_name(file_name, std::process::id(), sequence));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Ok(file) => {
                    return Ok(Self {
                        destination: destination.to_path_buf(),
                        temporary,
                        file: Some(file),
                        committed: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique staged-file name",
        ))
    }

    /// Synchronizes the staged bytes and atomically replaces the destination.
    ///
    /// On Unix, the destination's parent directory is synchronized after the
    /// replacement so the directory entry is durable.
    ///
    /// # Errors
    /// Returns an error when data synchronization, replacement, or parent
    /// directory synchronization fails.
    pub fn commit(mut self) -> io::Result<()> {
        let file = self
            .file
            .take()
            .ok_or_else(|| io::Error::other("staged file is already closed"))?;
        file.sync_all()?;
        drop(file);
        replace_file(&self.temporary, &self.destination)?;
        self.committed = true;
        Ok(())
    }

    #[cfg(test)]
    fn temporary_path(&self) -> &Path {
        &self.temporary
    }
}

impl Write for StagedFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("staged file is closed"))?
            .write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("staged file is closed"))?
            .flush()
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        self.file.take();
        let _ = fs::remove_file(&self.temporary);
    }
}

/// Durably writes `bytes` by staging them beside `destination` and committing.
///
/// # Errors
/// Returns an error when staging, writing, synchronization, or replacement
/// fails.
pub fn write_file_durably(destination: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut staged = StagedFile::create(destination)?;
    staged.write_all(bytes)?;
    staged.commit()
}

/// Replaces `destination` with `source` without an intermediate missing-file window.
///
/// # Errors
/// Returns the operating-system error if replacement or required directory
/// synchronization fails.
pub fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    platform_replace_file(source, destination)?;
    #[cfg(unix)]
    sync_parent_directory(destination)?;
    Ok(())
}

/// Moves a file for bounded housekeeping such as diagnostic-log rotation.
///
/// # Errors
/// Returns the operating-system error when the move fails.
pub fn move_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

/// Removes staged temporaries for `file_name` from `directory`.
///
/// # Errors
/// Returns an error when the directory cannot be read or a matching temporary
/// cannot be removed.
pub fn remove_stale_temporaries(directory: &Path, file_name: &str) -> io::Result<usize> {
    let prefix = format!(".{file_name}.");
    let mut removed = 0;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(".tmp") && entry.file_type()?.is_file() {
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn temporary_name(file_name: &std::ffi::OsStr, process_id: u32, sequence: u64) -> OsString {
    let mut name = OsString::from(".");
    name.push(file_name);
    name.push(format!(".{process_id}-{sequence}.tmp"));
    name
}

#[cfg(target_os = "windows")]
fn platform_replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    crate::windows::replace_file(source, destination)
}

#[cfg(not(target_os = "windows"))]
fn platform_replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(unix)]
fn sync_parent_directory(destination: &Path) -> io::Result<()> {
    File::open(destination.parent().unwrap_or_else(|| Path::new(".")))?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    const CRASH_CHILD_ENVIRONMENT: &str = "AERIS_PLATFORM_SYS_CRASH_CHILD";

    fn fixture_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "aeris-platform-sys-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn committed_file_contains_staged_bytes() {
        let root = fixture_root("commit");
        fs::create_dir_all(&root).expect("fixture root");
        let destination = root.join("state");

        write_file_durably(&destination, b"complete").expect("durable write");

        assert_eq!(fs::read(destination).expect("destination"), b"complete");
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn commit_replaces_an_existing_destination() {
        let root = fixture_root("replace");
        fs::create_dir_all(&root).expect("fixture root");
        let destination = root.join("state");
        fs::write(&destination, b"old").expect("old destination");

        write_file_durably(&destination, b"new").expect("durable replacement");

        assert_eq!(fs::read(destination).expect("destination"), b"new");
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn dropping_a_stage_removes_its_temporary() {
        let root = fixture_root("drop");
        fs::create_dir_all(&root).expect("fixture root");
        let temporary = {
            let staged = StagedFile::create(&root.join("state")).expect("stage");
            staged.temporary_path().to_path_buf()
        };

        assert!(!temporary.exists());
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn stale_temporary_cleanup_is_scoped_to_the_destination_name() {
        let root = fixture_root("stale");
        fs::create_dir_all(&root).expect("fixture root");
        fs::write(root.join(".state.7-4.tmp"), b"stale").expect("stale temporary");
        fs::write(root.join(".other.7-4.tmp"), b"other").expect("other temporary");
        fs::create_dir(root.join(".state.8-5.tmp")).expect("matching directory");

        assert_eq!(
            remove_stale_temporaries(&root, "state").expect("remove stale"),
            1
        );
        assert!(root.join(".other.7-4.tmp").exists());
        assert!(root.join(".state.8-5.tmp").is_dir());
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn child_killed_before_commit_preserves_existing_destination() {
        if let Some(root) = std::env::var_os(CRASH_CHILD_ENVIRONMENT) {
            let root = PathBuf::from(root);
            let mut staged = StagedFile::create(&root.join("state")).expect("child stage");
            staged.write_all(b"new").expect("child write");
            fs::write(root.join("staged"), b"ready").expect("child marker");
            thread::sleep(Duration::from_secs(30));
            panic!("parent did not terminate child");
        }

        let root = fixture_root("crash");
        fs::create_dir_all(&root).expect("fixture root");
        let destination = root.join("state");
        fs::write(&destination, b"old").expect("old destination");
        let executable = std::env::current_exe().expect("test executable");
        let mut child = Command::new(executable)
            .args([
                "--exact",
                "fs::tests::child_killed_before_commit_preserves_existing_destination",
                "--nocapture",
            ])
            .env(CRASH_CHILD_ENVIRONMENT, &root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");

        let deadline = Instant::now() + Duration::from_secs(10);
        while !root.join("staged").exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(root.join("staged").exists(), "child did not stage the file");
        child.kill().expect("kill child");
        child.wait().expect("reap child");

        assert_eq!(fs::read(&destination).expect("destination"), b"old");
        assert_eq!(
            remove_stale_temporaries(&root, "state").expect("remove abandoned stage"),
            1
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }
}
