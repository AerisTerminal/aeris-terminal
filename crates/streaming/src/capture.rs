//! Minimal synchronous S3 object capture with `SigV4` signing.
//!
//! Raw frame batches are immutable objects: one bounded PUT writes them and one
//! bounded GET reads them back for integrity verification. This client implements
//! only that surface — no listing, no multipart, no deletes — so every request
//! stays explicit and auditable. TLS deployment is tracked separately; the local
//! lane runs plaintext loopback against `MinIO` and records `tls=not_exercised`.

use crate::errors::StreamingError;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Largest object the capture path writes or reads.
pub const MAXIMUM_CAPTURE_BYTES: usize = 64 * 1_048_576;
const MAXIMUM_HEADER_BYTES: usize = 16_384;
const IO_TIMEOUT: Duration = Duration::from_secs(15);

/// Static credentials and endpoint for one capture bucket.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureEndpoint {
    pub host: String,
    pub port: u16,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
}

impl CaptureEndpoint {
    /// Builds one validated endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error for empty or control-bearing fields.
    pub fn try_new(
        host: &str,
        port: u16,
        region: &str,
        bucket: &str,
        access_key: &str,
        secret_key: &str,
    ) -> Result<Self, StreamingError> {
        for (value, error) in [
            (host, StreamingError::InvalidTopicSegment),
            (region, StreamingError::InvalidTopicSegment),
            (bucket, StreamingError::InvalidTopicSegment),
            (access_key, StreamingError::InvalidEventId),
            (secret_key, StreamingError::InvalidEventId),
        ] {
            if value.is_empty()
                || value.len() > 256
                || value
                    .chars()
                    .any(|character| character.is_control() || character == '/')
            {
                return Err(error);
            }
        }
        Ok(Self {
            host: host.to_string(),
            port,
            region: region.to_string(),
            bucket: bucket.to_string(),
            access_key: access_key.to_string(),
            secret_key: secret_key.to_string(),
        })
    }
}

/// One S3-compatible raw capture client for PUT/GET integrity.
pub struct RawCaptureClient {
    endpoint: CaptureEndpoint,
}

/// Outcome of one object read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedObject {
    pub bytes: Vec<u8>,
    pub sha256_hex: String,
}

impl RawCaptureClient {
    /// Creates a client for one validated endpoint.
    #[must_use]
    pub const fn new(endpoint: CaptureEndpoint) -> Self {
        Self { endpoint }
    }

    /// Creates the bucket when absent; an existing bucket is reported, not an error.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or is rejected.
    pub fn ensure_bucket(&self) -> Result<bool, StreamingError> {
        let (status, body) =
            self.request_with_body("PUT", &self.endpoint.bucket.clone(), &[], &[])?;
        match status {
            200 => Ok(true),
            409 => Ok(false),
            other => Err(StreamingError::Delivery(format!(
                "bucket creation returned {other}: {}",
                String::from_utf8_lossy(&body)
                    .chars()
                    .take(256)
                    .collect::<String>()
            ))),
        }
    }

    /// Writes one immutable object.
    ///
    /// # Errors
    ///
    /// Returns an error for oversized payloads, transport failures, or rejection.
    pub fn put_object(&self, key: &str, bytes: &[u8]) -> Result<(), StreamingError> {
        validate_key(key)?;
        if bytes.len() > MAXIMUM_CAPTURE_BYTES {
            return Err(StreamingError::PayloadTooLarge(bytes.len()));
        }
        let path = format!("{}/{key}", self.endpoint.bucket);
        let status = self.request("PUT", &path, bytes, &[])?;
        if status != 200 {
            return Err(StreamingError::Delivery(format!(
                "object write returned {status}"
            )));
        }
        Ok(())
    }

    /// Reads one object back with its payload digest.
    ///
    /// # Errors
    ///
    /// Returns an error for transport failures, rejection, or oversize responses.
    pub fn get_object(&self, key: &str) -> Result<CapturedObject, StreamingError> {
        validate_key(key)?;
        let path = format!("{}/{key}", self.endpoint.bucket);
        let (status, body) = self.request_with_body("GET", &path, &[], &[])?;
        if status != 200 {
            return Err(StreamingError::Delivery(format!(
                "object read returned {status}"
            )));
        }
        if body.len() > MAXIMUM_CAPTURE_BYTES {
            return Err(StreamingError::PayloadTooLarge(body.len()));
        }
        let digest = Sha256::digest(&body);
        Ok(CapturedObject {
            bytes: body,
            sha256_hex: hex_encode(&digest),
        })
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        extra_headers: &[(&str, &str)],
    ) -> Result<u16, StreamingError> {
        let (status, _) = self.request_with_body(method, path, body, extra_headers)?;
        Ok(status)
    }

    fn request_with_body(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        extra_headers: &[(&str, &str)],
    ) -> Result<(u16, Vec<u8>), StreamingError> {
        let timestamp = unix_seconds_now();
        let (amz_date, scope_date) = format_amz_dates(timestamp);
        let payload_hash = hex_encode(&Sha256::digest(body));
        let host_header = format!("{}:{}", self.endpoint.host, self.endpoint.port);
        let mut headers: Vec<(String, String)> = vec![
            ("host".to_string(), host_header.clone()),
            ("x-amz-content-sha256".to_string(), payload_hash.clone()),
            ("x-amz-date".to_string(), amz_date.clone()),
        ];
        for (name, value) in extra_headers {
            headers.push((name.to_string(), (*value).to_string()));
        }
        headers.sort_by(|left, right| left.0.cmp(&right.0));
        let signed_names: Vec<&str> = headers.iter().map(|(name, _)| name.as_str()).collect();
        let mut canonical_headers = String::new();
        for (name, value) in &headers {
            let _ = std::fmt::Write::write_fmt(
                &mut canonical_headers,
                format_args!("{name}:{value}\n"),
            );
        }
        let canonical_request = format!(
            "{method}\n/{}\n\n{}\n{}\n{payload_hash}",
            uri_encode_path(path),
            canonical_headers,
            signed_names.join(";")
        );
        let scope = format!("{scope_date}/{}/s3/aws4_request", self.endpoint.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex_encode(&Sha256::digest(canonical_request.as_bytes()))
        );
        let signature = signature_v4(
            &self.endpoint.secret_key,
            &scope_date,
            &self.endpoint.region,
            "s3",
            &string_to_sign,
        )?;
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={}, Signature={signature}",
            self.endpoint.access_key,
            signed_names.join(";")
        );

        let mut request = format!(
            "{method} /{} HTTP/1.1\r\nHost: {host_header}\r\nx-amz-content-sha256: {payload_hash}\r\nx-amz-date: {amz_date}\r\nAuthorization: {authorization}\r\nContent-Length: {}\r\nConnection: close\r\n",
            uri_encode_path(path),
            body.len()
        );
        for (name, value) in extra_headers {
            let _ = std::fmt::Write::write_fmt(&mut request, format_args!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");

        let mut stream = TcpStream::connect((self.endpoint.host.as_str(), self.endpoint.port))
            .map_err(|error| StreamingError::Client(error.to_string()))?;
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .map_err(|error| StreamingError::Client(error.to_string()))?;
        stream
            .set_write_timeout(Some(IO_TIMEOUT))
            .map_err(|error| StreamingError::Client(error.to_string()))?;
        stream
            .write_all(request.as_bytes())
            .and_then(|()| stream.write_all(body))
            .map_err(|error| StreamingError::Client(error.to_string()))?;
        read_response(&mut stream)
    }
}

fn read_response(stream: &mut TcpStream) -> Result<(u16, Vec<u8>), StreamingError> {
    let mut buffer = vec![0_u8; MAXIMUM_HEADER_BYTES];
    let mut filled = 0_usize;
    let header_end = loop {
        if filled >= buffer.len() {
            return Err(StreamingError::Delivery(
                "response headers too large".into(),
            ));
        }
        let read = stream
            .read(&mut buffer[filled..])
            .map_err(|error| StreamingError::Client(error.to_string()))?;
        if read == 0 {
            return Err(StreamingError::Delivery(
                "connection closed mid-headers".into(),
            ));
        }
        filled += read;
        if let Some(end) = find_subslice(&buffer[..filled], b"\r\n\r\n") {
            break end;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| StreamingError::Delivery("malformed status line".into()))?;
    let content_length = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .ok_or_else(|| StreamingError::Delivery("missing content length".into()))?;
    if content_length > MAXIMUM_CAPTURE_BYTES {
        return Err(StreamingError::PayloadTooLarge(content_length));
    }
    let body_start = header_end + 4;
    let mut body = buffer[body_start..filled].to_vec();
    body.reserve(content_length.saturating_sub(body.len()));
    while body.len() < content_length {
        let mut chunk = vec![0_u8; 65_536];
        let read = stream
            .read(&mut chunk)
            .map_err(|error| StreamingError::Client(error.to_string()))?;
        if read == 0 {
            return Err(StreamingError::Delivery(
                "connection closed mid-body".into(),
            ));
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Ok((status, body))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn signature_v4(
    secret_key: &str,
    scope_date: &str,
    region: &str,
    service: &str,
    string_to_sign: &str,
) -> Result<String, StreamingError> {
    let mut key = hmac_sha256(
        format!("AWS4{secret_key}").as_bytes(),
        scope_date.as_bytes(),
    )?;
    for part in [region, service, "aws4_request"] {
        key = hmac_sha256(&key, part.as_bytes())?;
    }
    Ok(hex_encode(&hmac_sha256(&key, string_to_sign.as_bytes())?))
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<[u8; 32], StreamingError> {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(|_| StreamingError::InvalidEventId)?;
    mac.update(data);
    let bytes = mac.finalize().into_bytes();
    let mut output = [0_u8; 32];
    output.copy_from_slice(&bytes);
    Ok(output)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = std::fmt::Write::write_fmt(&mut output, format_args!("{byte:02x}"));
    }
    output
}

fn uri_encode_path(path: &str) -> String {
    let mut output = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            output.push(byte as char);
        } else {
            let _ = std::fmt::Write::write_fmt(&mut output, format_args!("%{byte:02X}"));
        }
    }
    output
}

fn validate_key(key: &str) -> Result<(), StreamingError> {
    if key.is_empty()
        || key.len() > 1_024
        || key.starts_with('/')
        || key.contains("..")
        || key.chars().any(char::is_control)
    {
        return Err(StreamingError::InvalidTopicSegment);
    }
    Ok(())
}

fn unix_seconds_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().cast_signed())
        .unwrap_or_default()
}

fn format_amz_dates(timestamp: i64) -> (String, String) {
    let (year, month, day, hour, minute, second) = civil_from_unix(timestamp);
    (
        format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z"),
        format!("{year:04}{month:02}{day:02}"),
    )
}

fn civil_from_unix(timestamp: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = timestamp.div_euclid(86_400);
    let seconds = timestamp.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    (
        year,
        month,
        day,
        u32::try_from(seconds / 3_600).unwrap_or(0),
        u32::try_from((seconds % 3_600) / 60).unwrap_or(0),
        u32::try_from(seconds % 60).unwrap_or(0),
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(0);
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(0);
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::{
        CaptureEndpoint, civil_from_unix, format_amz_dates, signature_v4, uri_encode_path,
    };

    #[test]
    fn sigv4_matches_the_aws_documented_example() {
        use sha2::{Digest, Sha256};

        let canonical_request = "GET\n/\nAction=ListUsers&Version=2010-05-08\ncontent-type:application/x-www-form-urlencoded; charset=utf-8\nhost:iam.amazonaws.com\nx-amz-date:20150830T123600Z\n\ncontent-type;host;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let canonical_hash =
            crate::capture::hex_encode(&Sha256::digest(canonical_request.as_bytes()));
        assert_eq!(
            canonical_hash, "f536975d06c0309214f805bb90ccff089219ecd68b2577efef23edd43b7e1a59",
            "canonical request must hash to the AWS documented value"
        );
        let string_to_sign =
            format!("AWS4-HMAC-SHA256\n20150830T123600Z\n20150830/us-east-1/iam\n{canonical_hash}");
        let signature = signature_v4(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20150830",
            "us-east-1",
            "iam",
            &string_to_sign,
        )
        .expect("signing works");
        assert_eq!(
            signature,
            "fb3c97980fbd18d9e4b8ec5682fd9cad21119eaeedfe221cadf8ad0a26228d6f"
        );
    }

    #[test]
    fn civil_dates_match_known_timestamps() {
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil_from_unix(1_440_938_160), (2015, 8, 30, 12, 36, 0));
        let (amz, scope) = format_amz_dates(1_440_938_160);
        assert_eq!(amz, "20150830T123600Z");
        assert_eq!(scope, "20150830");
    }

    #[test]
    fn uri_encoding_preserves_unreserved_and_escapes_the_rest() {
        assert_eq!(uri_encode_path("bucket/a b/c+d"), "bucket/a%20b/c%2Bd");
        assert_eq!(uri_encode_path("bucket/a-b_c.d~e"), "bucket/a-b_c.d~e");
    }

    #[test]
    fn endpoint_rejects_control_and_path_fields() {
        assert!(CaptureEndpoint::try_new("host", 9_000, "us-east-1", "bucket", "ak", "sk").is_ok());
        assert!(
            CaptureEndpoint::try_new("ho/st", 9_000, "us-east-1", "bucket", "ak", "sk").is_err()
        );
        assert!(CaptureEndpoint::try_new("host", 9_000, "us-east-1", "bucket", "", "sk").is_err());
    }
}
