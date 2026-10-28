//! The release origin's layout: where the stable channel pointer and each immutable release
//! live below the release base URL. The launcher reads this layout and release packaging
//! writes it, so both take every path from here.

/// The one release channel a build follows.
pub const RELEASE_CHANNEL: &str = "stable";

/// File name of the signed release manifest inside a release directory.
pub const RELEASE_MANIFEST_FILENAME: &str = "manifest.json";

/// Path of the channel pointer for `platform` and `architecture` below the base URL.
#[must_use]
pub fn channel_pointer_path(platform: &str, architecture: &str) -> String {
    format!("channels/{RELEASE_CHANNEL}/{platform}/{architecture}.json")
}

/// Path of one immutable release directory below the base URL. The signed manifest, the
/// block plan, the setup program and every release file sit directly inside it.
#[must_use]
pub fn release_directory_path(
    platform: &str,
    architecture: &str,
    install_generation: u64,
    release_identity: &str,
) -> String {
    format!("{platform}/{architecture}/{install_generation}-{release_identity}")
}

/// File name of this platform's setup program inside a release directory.
#[must_use]
pub fn installer_filename() -> String {
    format!("Aeris-Setup{}", std::env::consts::EXE_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_paths_are_the_published_origin_shape() {
        assert_eq!(
            channel_pointer_path("windows", "x86_64"),
            "channels/stable/windows/x86_64.json"
        );
        assert_eq!(
            release_directory_path("windows", "x86_64", 7, "release_identity-0001"),
            "windows/x86_64/7-release_identity-0001"
        );
        assert!(installer_filename().starts_with("Aeris-Setup"));
    }
}
