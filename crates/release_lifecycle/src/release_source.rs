//! Where the launcher reads release channels, block plans and artifacts.
//!
//! The launcher checks every URL against its release base before it asks a source, and it
//! verifies every byte it receives against signed metadata, so a source only moves bytes.
//! Production reads over HTTPS; a test can serve a local directory.

use std::{error::Error, fmt, io::Read, time::Duration};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_mins(10);

/// Why a source could not supply the requested bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReleaseSourceError {
    /// The request failed before a response arrived.
    Request(String),
    /// The response status is not the one the request requires.
    Status(u16),
    /// The response body could not be read.
    Unreadable,
    /// The response body exceeds the caller's bound.
    TooLarge,
    /// The response does not cover exactly the requested byte range.
    RangeMismatch,
}

impl fmt::Display for ReleaseSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(error) => write!(formatter, "the request failed: {error}"),
            Self::Status(status) => write!(formatter, "the response status was {status}"),
            Self::Unreadable => formatter.write_str("the response body could not be read"),
            Self::TooLarge => formatter.write_str("the response exceeds its size bound"),
            Self::RangeMismatch => {
                formatter.write_str("the response does not cover the requested range")
            }
        }
    }
}

impl Error for ReleaseSourceError {}

/// One artifact response body.
pub struct ArtifactBody {
    /// Whether the body continues at the requested offset. When `false` it starts at byte 0.
    pub resumed: bool,
    pub reader: Box<dyn Read>,
}

/// Reads release channels, block plans and artifacts for the launcher.
pub trait ReleaseSource {
    /// Reads the mutable release channel pointer at `url`, at most `maximum` bytes.
    ///
    /// # Errors
    /// Returns why the pointer could not be read in full within the bound.
    fn read_channel(&self, url: &str, maximum: u64) -> Result<Vec<u8>, ReleaseSourceError>;

    /// Reads the immutable release object at `url` in full, at most `maximum` bytes.
    ///
    /// # Errors
    /// Returns why the object could not be read in full within the bound.
    fn read_object(&self, url: &str, maximum: u64) -> Result<Vec<u8>, ReleaseSourceError>;

    /// Reads exactly `length` bytes at `offset` of the `total`-byte artifact at `url`.
    ///
    /// # Errors
    /// Returns why that exact range could not be read.
    fn read_range(
        &self,
        url: &str,
        offset: u64,
        length: u32,
        total: u64,
    ) -> Result<Vec<u8>, ReleaseSourceError>;

    /// Opens the `size`-byte artifact at `url`, continuing at `offset` when the source can.
    ///
    /// # Errors
    /// Returns why the artifact could not be opened.
    fn open_artifact(
        &self,
        url: &str,
        offset: u64,
        size: u64,
    ) -> Result<ArtifactBody, ReleaseSourceError>;
}

/// Reads releases over HTTPS with bounded timeouts, redirects and idle connections.
pub(crate) struct HttpsReleaseSource {
    discovery: ureq::Agent,
    download: ureq::Agent,
}

impl HttpsReleaseSource {
    pub(crate) fn new() -> Self {
        Self {
            discovery: https_agent(DISCOVERY_TIMEOUT),
            download: https_agent(DOWNLOAD_TIMEOUT),
        }
    }
}

pub(crate) fn https_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(3)
        .max_idle_connections(2)
        .max_idle_connections_per_host(2)
        .timeout_global(Some(timeout))
        .build()
        .into()
}

fn get(
    agent: &ureq::Agent,
    url: &str,
    range: Option<String>,
) -> Result<ureq::http::Response<ureq::Body>, ReleaseSourceError> {
    let mut request = agent.get(url);
    if let Some(range) = range {
        request = request.header("Range", range);
    }
    request.call().map_err(|error| match error {
        ureq::Error::StatusCode(status) => ReleaseSourceError::Status(status),
        error => ReleaseSourceError::Request(error.to_string()),
    })
}

pub(crate) fn read_complete(
    agent: &ureq::Agent,
    url: &str,
    maximum: u64,
) -> Result<Vec<u8>, ReleaseSourceError> {
    let response = get(agent, url, None)?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(ReleaseSourceError::Status(status));
    }
    read_bounded(response.into_body().into_reader(), maximum)
}

fn read_bounded(reader: impl Read, maximum: u64) -> Result<Vec<u8>, ReleaseSourceError> {
    let mut bytes = Vec::new();
    reader
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ReleaseSourceError::Unreadable)?;
    if bytes.len() as u64 > maximum {
        return Err(ReleaseSourceError::TooLarge);
    }
    Ok(bytes)
}

fn require_content_range(
    headers: &ureq::http::HeaderMap,
    start: u64,
    end: u64,
    total: u64,
) -> Result<(), ReleaseSourceError> {
    let value = headers
        .get("Content-Range")
        .and_then(|value| value.to_str().ok())
        .ok_or(ReleaseSourceError::RangeMismatch)?;
    if value != format!("bytes {start}-{end}/{total}") {
        return Err(ReleaseSourceError::RangeMismatch);
    }
    Ok(())
}

impl ReleaseSource for HttpsReleaseSource {
    fn read_channel(&self, url: &str, maximum: u64) -> Result<Vec<u8>, ReleaseSourceError> {
        read_complete(&self.discovery, url, maximum)
    }

    fn read_object(&self, url: &str, maximum: u64) -> Result<Vec<u8>, ReleaseSourceError> {
        read_complete(&self.download, url, maximum)
    }

    fn read_range(
        &self,
        url: &str,
        offset: u64,
        length: u32,
        total: u64,
    ) -> Result<Vec<u8>, ReleaseSourceError> {
        let end = offset
            .checked_add(u64::from(length))
            .and_then(|end| end.checked_sub(1))
            .filter(|end| *end < total)
            .ok_or(ReleaseSourceError::RangeMismatch)?;
        let response = get(&self.download, url, Some(format!("bytes={offset}-{end}")))?;
        let status = response.status().as_u16();
        if status != 206 {
            return Err(ReleaseSourceError::Status(status));
        }
        require_content_range(response.headers(), offset, end, total)?;
        let bytes = read_bounded(response.into_body().into_reader(), u64::from(length))?;
        if bytes.len() as u64 != u64::from(length) {
            return Err(ReleaseSourceError::RangeMismatch);
        }
        Ok(bytes)
    }

    fn open_artifact(
        &self,
        url: &str,
        offset: u64,
        size: u64,
    ) -> Result<ArtifactBody, ReleaseSourceError> {
        let range = (offset > 0).then(|| format!("bytes={offset}-"));
        let response = get(&self.download, url, range)?;
        let resumed = match response.status().as_u16() {
            // Status 200 after a Range request means the origin ignored it; the caller
            // restarts from the full body rather than combining incompatible streams.
            200 => false,
            206 if offset > 0 => {
                require_content_range(response.headers(), offset, size.saturating_sub(1), size)?;
                true
            }
            status => return Err(ReleaseSourceError::Status(status)),
        };
        Ok(ArtifactBody {
            resumed,
            reader: Box::new(response.into_body().into_reader()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_must_match_the_exact_request() {
        let mut headers = ureq::http::HeaderMap::new();
        headers.insert(
            "Content-Range",
            ureq::http::HeaderValue::from_static("bytes 10-19/20"),
        );
        require_content_range(&headers, 10, 19, 20).expect("exact range accepted");
        assert!(require_content_range(&headers, 9, 19, 20).is_err());
        assert!(require_content_range(&headers, 10, 18, 20).is_err());
        assert!(require_content_range(&headers, 10, 19, 21).is_err());
        assert_eq!(
            require_content_range(&ureq::http::HeaderMap::new(), 10, 19, 20),
            Err(ReleaseSourceError::RangeMismatch)
        );
    }

    #[test]
    fn bounded_reads_accept_the_bound_and_refuse_one_byte_more() {
        assert_eq!(
            read_bounded(std::io::Cursor::new(b"abcd"), 4),
            Ok(b"abcd".to_vec())
        );
        assert_eq!(
            read_bounded(std::io::Cursor::new(b"abcde"), 4),
            Err(ReleaseSourceError::TooLarge)
        );
    }
}
