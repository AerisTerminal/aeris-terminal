//! The newest build the website publishes, for an installation the launcher does not update.
//!
//! A build without a release configuration (macOS and the website installers) cannot update
//! itself, so it compares its version with the website's `latest.json` and offers the download
//! page. Nothing here is trusted for installation: the result only names a version to mention.

use std::{collections::BTreeMap, fmt, time::Duration};

use serde::Deserialize;

use crate::release_source::{ReleaseSourceError, https_agent, read_complete};

/// Where a user downloads a newer build by hand.
pub const DOWNLOAD_PAGE_URL: &str = "https://aeristerminal.com/download/";
const PUBLISHED_DOWNLOADS_URL: &str = "https://aeristerminal.com/downloads/latest.json";
const PUBLISHED_DOWNLOADS_TIMEOUT: Duration = Duration::from_secs(15);
const MAXIMUM_PUBLISHED_DOWNLOADS_BYTES: u64 = 16 * 1024;

/// Why the published downloads could not be compared.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadOfferError {
    /// The document could not be read.
    Source(ReleaseSourceError),
    /// The document or one of the compared versions is not in the published shape.
    Malformed,
}

impl fmt::Display for DownloadOfferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(error) => write!(formatter, "published downloads unavailable: {error}"),
            Self::Malformed => formatter.write_str("published downloads are malformed"),
        }
    }
}

impl std::error::Error for DownloadOfferError {}

#[derive(Deserialize)]
struct PublishedDownloads {
    builds: BTreeMap<String, PublishedBuild>,
}

#[derive(Deserialize)]
struct PublishedBuild {
    version: String,
}

/// Reads the website's published downloads and returns the version of this platform's build
/// when it is newer than `current_version`.
///
/// # Errors
/// Returns why the document could not be read or compared.
pub fn newer_published_download(
    current_version: &str,
) -> Result<Option<String>, DownloadOfferError> {
    let agent = https_agent(PUBLISHED_DOWNLOADS_TIMEOUT);
    let document = read_complete(
        &agent,
        PUBLISHED_DOWNLOADS_URL,
        MAXIMUM_PUBLISHED_DOWNLOADS_BYTES,
    )
    .map_err(DownloadOfferError::Source)?;
    newer_download_in(&document, std::env::consts::OS, current_version)
}

/// The version `document` publishes for `platform` when it is newer than `current_version`.
/// A platform the document has no build for has nothing newer.
///
/// # Errors
/// Returns [`DownloadOfferError::Malformed`] when the document or a version is not in the
/// published `major.minor.patch` shape.
pub fn newer_download_in(
    document: &[u8],
    platform: &str,
    current_version: &str,
) -> Result<Option<String>, DownloadOfferError> {
    let published: PublishedDownloads =
        serde_json::from_slice(document).map_err(|_| DownloadOfferError::Malformed)?;
    let current = release_version(current_version).ok_or(DownloadOfferError::Malformed)?;
    let Some(build) = published.builds.get(platform) else {
        return Ok(None);
    };
    let latest = release_version(&build.version).ok_or(DownloadOfferError::Malformed)?;
    Ok((latest > current).then(|| build.version.clone()))
}

fn release_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.').map(|part| {
        (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| part.parse::<u64>().ok())
            .flatten()
    });
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBLISHED: &[u8] = br#"{"builds":{"macos":{"version":"1.3.0","file":"Aeris-Terminal-1.3.0.dmg","bytes":1,"sha256":"00","publishedAt":"2026-11-01T00:00:00Z"},"windows":{"version":"1.2.0","file":"Aeris-Terminal-Setup-1.2.0.exe","bytes":1,"sha256":"00","publishedAt":"2026-10-30T00:00:00Z"}}}"#;

    #[test]
    fn only_a_newer_build_for_this_platform_is_offered() {
        assert_eq!(
            newer_download_in(PUBLISHED, "macos", "1.2.9"),
            Ok(Some("1.3.0".to_string()))
        );
        assert_eq!(newer_download_in(PUBLISHED, "macos", "1.3.0"), Ok(None));
        assert_eq!(newer_download_in(PUBLISHED, "windows", "1.10.0"), Ok(None));
        assert_eq!(newer_download_in(PUBLISHED, "linux", "1.0.0"), Ok(None));
    }

    #[test]
    fn versions_compare_numerically_not_as_text() {
        let document = br#"{"builds":{"windows":{"version":"1.10.0"}}}"#;
        assert_eq!(
            newer_download_in(document, "windows", "1.9.0"),
            Ok(Some("1.10.0".to_string()))
        );
    }

    #[test]
    fn malformed_documents_and_versions_are_refused() {
        for document in [
            &b"not json"[..],
            br#"{"builds":[]}"#,
            br#"{"builds":{"windows":{"version":"1.3"}}}"#,
            br#"{"builds":{"windows":{"version":"1.3.0-beta"}}}"#,
            br#"{"builds":{"windows":{"version":"1.+3.0"}}}"#,
            br#"{"builds":{"windows":{"version":"1.3.0.1"}}}"#,
        ] {
            assert_eq!(
                newer_download_in(document, "windows", "1.2.0"),
                Err(DownloadOfferError::Malformed)
            );
        }
        assert_eq!(
            newer_download_in(PUBLISHED, "windows", "dev"),
            Err(DownloadOfferError::Malformed)
        );
    }
}
