use std::path::Path;

/// Replaces `destination` with `source` without an intermediate missing-file window.
///
/// # Errors
/// Returns the operating-system error if the same-filesystem replacement cannot be committed.
pub fn replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    platform_replace_file_atomically(source, destination)
}

#[cfg(unix)]
fn platform_replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
fn platform_replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let mut source = source.as_os_str().encode_wide().collect::<Vec<_>>();
    source.push(0);
    let mut destination = destination.as_os_str().encode_wide().collect::<Vec<_>>();
    destination.push(0);
    // SAFETY: both paths are stable, NUL-terminated UTF-16 buffers for the
    // duration of the call. MoveFileExW does not retain either pointer.
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(all(not(unix), not(target_os = "windows")))]
fn platform_replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_keeps_only_the_committed_destination() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!("asceify-atomic-file-{nonce}"));
        std::fs::create_dir_all(&root).expect("fixture root");
        let destination = root.join("current.json");
        let source = root.join("current.json.next");
        std::fs::write(&destination, b"old").expect("old destination");
        std::fs::write(&source, b"new").expect("new source");

        replace_file_atomically(&source, &destination).expect("atomic replacement");

        assert_eq!(std::fs::read(&destination).expect("destination"), b"new");
        assert!(!source.exists());
        std::fs::remove_dir_all(root).expect("remove fixture");
    }
}
