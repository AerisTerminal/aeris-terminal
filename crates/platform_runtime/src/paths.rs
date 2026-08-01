//! Application-owned filesystem locations selected by a platform adapter.

use std::path::PathBuf;

/// Application-owned paths selected by a platform adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePaths {
    pub cache: PathBuf,
    pub configuration: PathBuf,
    pub logs: PathBuf,
}
