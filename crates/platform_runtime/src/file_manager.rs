//! Reveals a local file or folder in the platform's file manager.
//!
//! The manager process is fire-and-forget: the call returns once it starts and never waits
//! on it, so callers on any thread stay unblocked. It never handles credentials or tokens.

use std::path::Path;

/// Opens the file manager at `path`: the containing folder with the file selected, or the
/// folder itself when `path` is a directory. Only absolute paths are accepted.
///
/// # Errors
///
/// Returns an error when the path is not absolute or the manager process cannot start.
pub fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || path.as_os_str().is_empty() {
        return Err("the folder path is unavailable".to_string());
    }
    reveal(path)
}

#[cfg(target_os = "windows")]
fn reveal(path: &Path) -> Result<(), String> {
    // The path travels as one argument, never through a shell. Explorer exits nonzero even
    // for a successful open, so its exit status is never waited on or checked.
    let mut command = std::process::Command::new("explorer.exe");
    if path.is_file() {
        let mut select = std::ffi::OsString::from("/select,");
        select.push(path);
        command.arg(select);
    } else {
        command.arg(path);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| "the file manager could not be opened".to_string())
}

#[cfg(target_os = "macos")]
fn reveal(path: &Path) -> Result<(), String> {
    let mut command = std::process::Command::new("open");
    if path.is_file() {
        command.arg("-R");
    }
    command
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| "the file manager could not be opened".to_string())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn reveal(path: &Path) -> Result<(), String> {
    let folder = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    std::process::Command::new("xdg-open")
        .arg(folder)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| "the file manager could not be opened".to_string())
}

#[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
fn reveal(_: &Path) -> Result<(), String> {
    Err("the file manager could not be opened".to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn relative_and_empty_paths_fail_closed_without_spawning() {
        for path in [
            std::path::Path::new("workspace-state.pb"),
            std::path::Path::new(""),
        ] {
            assert!(super::reveal_in_file_manager(path).is_err());
        }
    }
}
