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

/// Opens the local file at `path` with the application the user chose for its type, such as
/// the default browser for an HTML report. Only absolute paths of existing files are accepted.
///
/// # Errors
///
/// Returns an error when the path is not an absolute file path or the opener cannot start.
pub fn open_file_in_default_app(path: &Path) -> std::io::Result<()> {
    if !path.is_absolute() || !path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the file path is unavailable",
        ));
    }
    open_file(path)
}

// The path travels as one argument, never through a shell, and the opener is never waited on.
#[cfg(target_os = "windows")]
fn open_file(path: &Path) -> std::io::Result<()> {
    spawn_detached(std::process::Command::new("explorer.exe").arg(path))
}

#[cfg(target_os = "macos")]
fn open_file(path: &Path) -> std::io::Result<()> {
    spawn_detached(std::process::Command::new("open").arg(path))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_file(path: &Path) -> std::io::Result<()> {
    spawn_detached(std::process::Command::new("xdg-open").arg(path))
}

#[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
fn open_file(_: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no file opener on this platform",
    ))
}

#[cfg(any(target_os = "windows", unix))]
fn spawn_detached(command: &mut std::process::Command) -> std::io::Result<()> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
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

    #[test]
    fn opening_a_relative_or_missing_file_fails_closed_without_spawning() {
        let missing = std::env::temp_dir().join("aeris-missing-report-for-open-test.html");
        for path in [
            std::path::Path::new("report.html"),
            std::path::Path::new(""),
            missing.as_path(),
            std::env::temp_dir().as_path(),
        ] {
            assert!(super::open_file_in_default_app(path).is_err(), "{path:?}");
        }
    }
}
