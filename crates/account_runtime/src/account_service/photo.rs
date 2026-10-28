//! Bounded profile-photo download for the signed-in account.
//!
//! The runtime fetches the verified profile's `picture` address itself so
//! the desktop renders bytes and needs no HTTP client of its own.

use std::{sync::Arc, time::Duration};

use aeris_account::MAXIMUM_PROFILE_PHOTO_URL_BYTES;

use crate::ProfilePhoto;

/// Upper bound for one photo download, including redirects.
const PHOTO_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
/// Wait before the same address is fetched again after a failure.
pub(super) const PHOTO_RETRY_DELAY: Duration = Duration::from_mins(5);

/// Downloads one validated photo for an address.
pub(super) type PhotoFetcher = Arc<dyn Fn(&str) -> Result<ProfilePhoto, String> + Send + Sync>;

/// The production fetcher: one `https`-only agent with a global timeout.
pub(super) fn https_photo_fetcher() -> PhotoFetcher {
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .timeout_global(Some(PHOTO_FETCH_TIMEOUT))
        .user_agent(concat!("Aeris/", env!("CARGO_PKG_VERSION")))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    Arc::new(move |url| fetch_profile_photo(&agent, url))
}

/// Whether a profile photo address may be fetched. Only bounded `https`
/// addresses qualify; anything else keeps the initials avatar.
pub(super) fn is_fetchable_photo_url(url: &str) -> bool {
    url.len() > "https://".len()
        && url.len() <= MAXIMUM_PROFILE_PHOTO_URL_BYTES
        && url.starts_with("https://")
        && !url.contains(|ch: char| ch.is_whitespace() || ch.is_control())
}

fn fetch_profile_photo(agent: &ureq::Agent, url: &str) -> Result<ProfilePhoto, String> {
    if !is_fetchable_photo_url(url) {
        return Err("profile photo address is not a bounded https address".to_string());
    }
    let mut response = agent
        .get(url)
        .call()
        .map_err(|_| "profile photo request failed".to_string())?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(ProfilePhoto::MAXIMUM_BYTES as u64)
        .read_to_vec()
        .map_err(|_| "profile photo download failed or exceeded 1 MiB".to_string())?;
    ProfilePhoto::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::{https_photo_fetcher, is_fetchable_photo_url};
    use aeris_account::MAXIMUM_PROFILE_PHOTO_URL_BYTES;
    use std::{io::ErrorKind, net::TcpListener};

    #[test]
    fn only_bounded_https_addresses_are_fetchable() {
        assert!(is_fetchable_photo_url(
            "https://auth.example.test/photo/a.png"
        ));
        assert!(is_fetchable_photo_url(
            "https://lh3.googleusercontent.com/a/photo?x=1&y=2"
        ));
        for bad in [
            "",
            "https://",
            "http://auth.example.test/photo/a.png",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://auth.example.test/has space",
            "https://auth.example.test/line\nbreak",
            "data:image/png;base64,AAA",
        ] {
            assert!(!is_fetchable_photo_url(bad), "photo must not load: {bad}");
        }
        let longest = format!(
            "https://auth.example.test/{}",
            "a".repeat(MAXIMUM_PROFILE_PHOTO_URL_BYTES - "https://auth.example.test/".len())
        );
        assert!(is_fetchable_photo_url(&longest));
        assert!(!is_fetchable_photo_url(&format!("{longest}a")));
    }

    #[test]
    fn plain_http_photo_addresses_never_connect() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener binds");
        listener
            .set_nonblocking(true)
            .expect("listener becomes non-blocking");
        let address = listener.local_addr().expect("loopback address resolves");
        let fetch = https_photo_fetcher();
        assert!(fetch(&format!("http://{address}/avatar.png")).is_err());
        assert_eq!(
            listener
                .accept()
                .map(|_| ())
                .expect_err("no connection reaches the listener")
                .kind(),
            ErrorKind::WouldBlock
        );
    }
}
