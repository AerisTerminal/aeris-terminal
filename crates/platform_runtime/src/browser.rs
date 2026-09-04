//! System-browser opening for engine-supplied authentication URLs.
//!
//! The desktop sends `BeginLogin` over IPC and the engine replies with an
//! Axiusflow authentication URL. This boundary opens that URL in the user's
//! system browser. It performs process work, so callers must keep it off the
//! UI thread on background workers. It never handles credentials or tokens.

/// Maximum authorization URL length accepted before opening a browser.
pub const MAXIMUM_AUTHORIZATION_URL_BYTES: usize = 2048;

/// Opens an engine-supplied authentication URL in the system browser.
///
/// Only `https` URLs are opened; anything else fails closed without spawning
/// a process.
///
/// # Errors
///
/// Returns an error when the URL is empty, too long, not `https`, or the
/// native browser launcher cannot start.
pub fn open_system_browser(url: &str) -> Result<(), String> {
    let url = validate_authorization_url(url)?;
    spawn_browser(url)
}

fn validate_authorization_url(url: &str) -> Result<&str, String> {
    if url.is_empty() || url.len() > MAXIMUM_AUTHORIZATION_URL_BYTES {
        return Err("authentication URL is invalid".to_string());
    }
    if url.starts_with("https://") && !url.contains([' ', '\n', '\r', '\t']) {
        Ok(url)
    } else {
        Err("authentication URL is invalid".to_string())
    }
}

fn spawn_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map(|_| ())
            .map_err(|_| "system browser could not be opened".to_string())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|_| "system browser could not be opened".to_string())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|_| "system browser could not be opened".to_string())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        let _ = url;
        Err("system browser could not be opened".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{MAXIMUM_AUTHORIZATION_URL_BYTES, open_system_browser};

    #[test]
    fn non_https_and_malformed_urls_fail_closed_without_spawning() {
        for url in [
            "",
            "http://auth.axiusflow.com/authorize",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://auth.axiusflow.com/has space",
            "127.0.0.1:8080/callback",
        ] {
            assert!(
                open_system_browser(url).is_err(),
                "unsafe URL must not open: {url}"
            );
        }
    }

    #[test]
    fn overlong_urls_fail_closed() {
        let long = format!(
            "https://auth.axiusflow.com/{}",
            "a".repeat(MAXIMUM_AUTHORIZATION_URL_BYTES)
        );
        assert!(open_system_browser(&long).is_err());
    }
}
