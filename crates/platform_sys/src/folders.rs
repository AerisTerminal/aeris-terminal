//! Per-user folders the operating system names.

use std::path::PathBuf;

/// The user's Downloads folder. On Windows it is the Downloads known folder, which follows
/// a folder the user moved; elsewhere it is `Downloads` in the home directory. `None` when
/// the platform names no such folder.
#[must_use]
pub fn downloads_directory() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        crate::windows::folders::downloads_directory()
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .map(|home| home.join("Downloads"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_downloads_folder_is_an_absolute_path() {
        if let Some(folder) = super::downloads_directory() {
            assert!(folder.is_absolute(), "{folder:?}");
        }
    }
}
