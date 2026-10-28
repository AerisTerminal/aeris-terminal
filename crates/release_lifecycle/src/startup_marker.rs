//! The startup-complete marker. An installed desktop records it once its window is open on
//! the opened local stores, and the launcher accepts a new release generation only after the
//! desktop it started recorded the marker within the probation bound. The marker holds the
//! process ID, so a marker left by an earlier process never vouches for the current one.

use std::{
    fs,
    path::{Path, PathBuf},
};

use aeris_platform_runtime::fs::write_file_durably;

use crate::valid_release_identity;

/// The marker's file name in the install root.
pub const STARTUP_COMPLETE_FILENAME: &str = ".startup-complete.json";
pub const STARTUP_COMPLETE_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_STARTUP_COMPLETE_BYTES: u64 = 4 * 1024;

/// One desktop process that finished starting.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartupComplete {
    pub schema_version: u32,
    pub install_generation: u64,
    pub release_identity: String,
    pub process_id: u32,
}

impl StartupComplete {
    /// The marker for this process running the release `install_generation`.
    #[must_use]
    pub fn for_current_process(install_generation: u64, release_identity: &str) -> Self {
        Self {
            schema_version: STARTUP_COMPLETE_SCHEMA_VERSION,
            install_generation,
            release_identity: release_identity.to_string(),
            process_id: std::process::id(),
        }
    }

    /// The marker for this process running the compiled release; `None` in a development
    /// build, which no launcher installed.
    #[must_use]
    pub fn for_current_release() -> Option<Self> {
        let release = crate::current_release_identity();
        (release.install_generation > 0).then(|| {
            Self::for_current_process(release.install_generation, &release.release_identity)
        })
    }

    /// Whether this marker records a start of the release `install_generation`.
    #[must_use]
    pub fn is_release(&self, install_generation: u64, release_identity: &str) -> bool {
        self.install_generation == install_generation && self.release_identity == release_identity
    }
}

/// The marker path in `install_root`.
#[must_use]
pub fn startup_complete_path(install_root: &Path) -> PathBuf {
    install_root.join(STARTUP_COMPLETE_FILENAME)
}

/// Records `marker` in `install_root`, replacing any earlier marker durably.
///
/// # Errors
/// Returns a redacted description when the marker is invalid or cannot be written.
pub fn record_startup_complete(
    install_root: &Path,
    marker: &StartupComplete,
) -> Result<(), String> {
    if !valid_marker(marker) {
        return Err("the startup-complete marker is invalid".to_string());
    }
    let encoded = serde_json::to_vec(marker)
        .map_err(|_| "the startup-complete marker could not be encoded".to_string())?;
    write_file_durably(&startup_complete_path(install_root), &encoded)
        .map_err(|_| "the startup-complete marker could not be written".to_string())
}

/// Reads the marker in `install_root`; `None` when no desktop recorded one.
///
/// # Errors
/// Returns a redacted description when the marker exists but is not a valid marker.
pub fn read_startup_complete(install_root: &Path) -> Result<Option<StartupComplete>, String> {
    let path = startup_complete_path(install_root);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("the startup-complete marker is unavailable".to_string()),
    };
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAXIMUM_STARTUP_COMPLETE_BYTES
    {
        return Err("the startup-complete marker is invalid".to_string());
    }
    let bytes =
        fs::read(path).map_err(|_| "the startup-complete marker is unreadable".to_string())?;
    let marker: StartupComplete = serde_json::from_slice(&bytes)
        .map_err(|_| "the startup-complete marker is malformed".to_string())?;
    if !valid_marker(&marker) {
        return Err("the startup-complete marker is invalid".to_string());
    }
    Ok(Some(marker))
}

fn valid_marker(marker: &StartupComplete) -> bool {
    marker.schema_version == STARTUP_COMPLETE_SCHEMA_VERSION
        && marker.install_generation > 0
        && marker.process_id > 0
        && valid_release_identity(&marker.release_identity)
}

#[cfg(test)]
mod tests {
    use super::{
        StartupComplete, read_startup_complete, record_startup_complete, startup_complete_path,
    };
    use std::fs;

    #[test]
    fn a_recorded_marker_reads_back_and_a_damaged_one_is_refused() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!("aeris-startup-complete-{nanos}"));
        fs::create_dir_all(&root).expect("install root");
        assert_eq!(read_startup_complete(&root), Ok(None));

        let marker = StartupComplete::for_current_process(7, "probation-release-0007");
        record_startup_complete(&root, &marker).expect("marker records");
        let read = read_startup_complete(&root)
            .expect("marker reads")
            .expect("marker exists");
        assert_eq!(read, marker);
        assert!(read.is_release(7, "probation-release-0007"));
        assert!(!read.is_release(8, "probation-release-0007"));
        assert!(!read.is_release(7, "probation-release-0008"));

        let invalid = StartupComplete::for_current_process(0, "probation-release-0000");
        assert!(record_startup_complete(&root, &invalid).is_err());
        fs::write(startup_complete_path(&root), b"{\"schema_version\":1}").expect("damage");
        assert!(read_startup_complete(&root).is_err());
        fs::remove_dir_all(root).expect("remove install root");
    }
}
