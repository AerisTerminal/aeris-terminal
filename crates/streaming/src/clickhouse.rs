//! `ClickHouse` analytical projection sink over the HTTP interface.
//!
//! One sink consumes durable events and writes idempotent batches carrying the
//! event ID and projection version, per Section 11.5. Projections are rebuildable
//! analytical copies: a `ReplacingMergeTree(projection_version)` keyed on the
//! stable bar identity lets a rebuilt batch supersede its predecessor, so a
//! repeated insert never duplicates and a newer version always wins under
//! `FINAL`. Plaintext HTTP loopback is the local lane; TLS deployment is tracked
//! separately.

use crate::errors::StreamingError;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Largest query or row batch this sink accepts.
pub const MAXIMUM_BATCH_BYTES: usize = 16 * 1_048_576;
/// Largest rows one insert batch carries.
pub const MAXIMUM_BATCH_ROWS: usize = 65_536;
const MAXIMUM_HEADER_BYTES: usize = 16_384;
const IO_TIMEOUT: Duration = Duration::from_secs(15);

/// Validated `ClickHouse` HTTP endpoint with optional basic authentication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClickHouseEndpoint {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub credentials: Option<(String, String)>,
}

impl ClickHouseEndpoint {
    /// Builds one validated endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error for empty or control-bearing fields.
    pub fn try_new(host: &str, port: u16, database: &str) -> Result<Self, StreamingError> {
        for value in [host, database] {
            if value.is_empty()
                || value.len() > 256
                || value
                    .chars()
                    .any(|character| character.is_control() || character == '/')
            {
                return Err(StreamingError::InvalidTopicSegment);
            }
        }
        Ok(Self {
            host: host.to_string(),
            port,
            database: database.to_string(),
            credentials: None,
        })
    }

    /// Attaches basic-auth credentials.
    ///
    /// # Errors
    ///
    /// Returns an error for empty or control-bearing fields.
    pub fn with_credentials(mut self, user: &str, password: &str) -> Result<Self, StreamingError> {
        for value in [user, password] {
            if value.is_empty()
                || value.len() > 256
                || value
                    .chars()
                    .any(|character| character.is_control() || character == ':')
            {
                return Err(StreamingError::InvalidEventId);
            }
        }
        self.credentials = Some((user.to_string(), password.to_string()));
        Ok(self)
    }
}

/// One deterministic bar projection row with fixed-point values.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BarProjectionRow {
    pub event_id: String,
    pub projection_version: u32,
    pub instrument_id: String,
    pub definition_id: String,
    pub bar_start_unix_nanos: i64,
    pub source_sequence: u64,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub volume: i64,
    pub price_scale: u32,
}

/// One `ClickHouse` projection sink bound to one endpoint.
pub struct ClickHouseSink {
    endpoint: ClickHouseEndpoint,
}

impl ClickHouseSink {
    /// Creates a sink for one validated endpoint.
    #[must_use]
    pub const fn new(endpoint: ClickHouseEndpoint) -> Self {
        Self { endpoint }
    }

    /// Creates the projection database and bars table when absent.
    ///
    /// # Errors
    ///
    /// Returns an error when the DDL fails.
    pub fn create_tables(&self) -> Result<(), StreamingError> {
        self.execute(&format!(
            "CREATE DATABASE IF NOT EXISTS {}",
            self.endpoint.database
        ))?;
        self.execute(&format!(
            "CREATE TABLE IF NOT EXISTS {}.market_bars (\
                event_id String, \
                projection_version UInt32, \
                instrument_id String, \
                definition_id String, \
                bar_start_unix_nanos Int64, \
                source_sequence UInt64, \
                open Int64, \
                high Int64, \
                low Int64, \
                close Int64, \
                volume Int64, \
                price_scale UInt32\
            ) ENGINE = ReplacingMergeTree(projection_version) \
            ORDER BY (instrument_id, bar_start_unix_nanos, source_sequence)",
            self.endpoint.database
        ))
    }

    /// Inserts one bounded batch of projection rows.
    ///
    /// # Errors
    ///
    /// Returns an error for oversized batches or a rejected insert.
    pub fn insert_bars(&self, rows: &[BarProjectionRow]) -> Result<(), StreamingError> {
        if rows.len() > MAXIMUM_BATCH_ROWS {
            return Err(StreamingError::PayloadTooLarge(rows.len()));
        }
        let mut body = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut body, row)
                .map_err(|error| StreamingError::Client(error.to_string()))?;
            body.push(b'\n');
        }
        if body.len() > MAXIMUM_BATCH_BYTES {
            return Err(StreamingError::PayloadTooLarge(body.len()));
        }
        self.post(
            &format!(
                "INSERT INTO {}.market_bars FORMAT JSONEachRow",
                self.endpoint.database
            ),
            &body,
        )
        .map(|_| ())
    }

    /// Reads every projected bar under `FINAL` in deterministic order.
    ///
    /// # Errors
    ///
    /// Returns an error when the query fails or a row does not parse.
    pub fn read_bars_final(&self) -> Result<Vec<BarProjectionRow>, StreamingError> {
        let body = self.post(
            &format!(
                "SELECT * FROM {}.market_bars FINAL ORDER BY source_sequence FORMAT JSONEachRow",
                self.endpoint.database
            ),
            &[],
        )?;
        let mut rows = Vec::new();
        for line in body.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            rows.push(
                serde_json::from_slice::<BarProjectionRow>(line)
                    .map_err(|error| StreamingError::Client(error.to_string()))?,
            );
        }
        Ok(rows)
    }

    /// Runs one bounded query that returns bytes.
    ///
    /// # Errors
    ///
    /// Returns an error for transport failures or a non-200 status.
    pub fn query_bytes(&self, query: &str) -> Result<Vec<u8>, StreamingError> {
        self.post(query, &[])
    }

    fn execute(&self, query: &str) -> Result<(), StreamingError> {
        self.post(query, &[]).map(|_| ())
    }

    fn post(&self, query: &str, body: &[u8]) -> Result<Vec<u8>, StreamingError> {
        let encoded_query = uri_encode_query(query);
        let authorization = self
            .endpoint
            .credentials
            .as_ref()
            .map(|(user, password)| {
                use base64::Engine as _;
                format!(
                    "Authorization: Basic {}\r\n",
                    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
                )
            })
            .unwrap_or_default();
        let request = format!(
            "POST /?query={encoded_query} HTTP/1.1\r\nHost: {}:{}\r\nContent-Length: {}\r\nConnection: close\r\n{authorization}\r\n",
            self.endpoint.host,
            self.endpoint.port,
            body.len()
        );
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
        let (status, response) = read_response(&mut stream)?;
        if status != 200 {
            return Err(StreamingError::Delivery(format!(
                "ClickHouse returned {status}: {}",
                String::from_utf8_lossy(&response)
                    .chars()
                    .take(256)
                    .collect::<String>()
            )));
        }
        Ok(response)
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
        if let Some(end) = buffer[..filled]
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
        {
            break end;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| StreamingError::Delivery("malformed status line".into()))?;
    let content_length = head.lines().find_map(|line| {
        line.to_ascii_lowercase()
            .strip_prefix("content-length:")
            .and_then(|value| value.trim().parse::<usize>().ok())
    });
    let chunked = head.lines().any(|line| {
        line.to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
    });
    let body_start = header_end + 4;
    let mut body = buffer[body_start..filled].to_vec();
    if chunked {
        return read_chunked(stream, &mut body).map(|body| (status, body));
    }
    let content_length =
        content_length.ok_or_else(|| StreamingError::Delivery("missing content length".into()))?;
    if content_length > MAXIMUM_BATCH_BYTES {
        return Err(StreamingError::PayloadTooLarge(content_length));
    }
    while body.len() < content_length {
        let mut chunk = vec![0_u8; 65_536];
        let read = stream
            .read(&mut chunk)
            .map_err(|error| StreamingError::Client(error.to_string()))?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Ok((status, body))
}

fn read_chunked(stream: &mut TcpStream, body: &mut Vec<u8>) -> Result<Vec<u8>, StreamingError> {
    let mut decoded = Vec::new();
    loop {
        let size_line = read_line(stream, body)?;
        let size_text = size_line
            .split(';')
            .next()
            .ok_or_else(|| StreamingError::Delivery("malformed chunk size".into()))?;
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| StreamingError::Delivery("malformed chunk size".into()))?;
        if decoded.len() + size > MAXIMUM_BATCH_BYTES {
            return Err(StreamingError::PayloadTooLarge(decoded.len() + size));
        }
        if size == 0 {
            while !read_line(stream, body)?.is_empty() {}
            return Ok(decoded);
        }
        while body.len() < size + 2 {
            let mut chunk = vec![0_u8; 65_536];
            let read = stream
                .read(&mut chunk)
                .map_err(|error| StreamingError::Client(error.to_string()))?;
            if read == 0 {
                return Err(StreamingError::Delivery(
                    "connection closed mid-chunk".into(),
                ));
            }
            body.extend_from_slice(&chunk[..read]);
        }
        decoded.extend_from_slice(&body[..size]);
        body.drain(..size + 2);
    }
}

fn read_line(stream: &mut TcpStream, body: &mut Vec<u8>) -> Result<String, StreamingError> {
    loop {
        if let Some(end) = body.windows(2).position(|window| window == b"\r\n") {
            let line = String::from_utf8_lossy(&body[..end]).into_owned();
            body.drain(..end + 2);
            return Ok(line);
        }
        let mut chunk = vec![0_u8; 4_096];
        let read = stream
            .read(&mut chunk)
            .map_err(|error| StreamingError::Client(error.to_string()))?;
        if read == 0 {
            return Err(StreamingError::Delivery(
                "connection closed mid-line".into(),
            ));
        }
        body.extend_from_slice(&chunk[..read]);
        if body.len() > MAXIMUM_HEADER_BYTES {
            return Err(StreamingError::Delivery("chunk line too large".into()));
        }
    }
}

fn uri_encode_query(query: &str) -> String {
    let mut output = String::with_capacity(query.len());
    for byte in query.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(byte as char);
        } else {
            let _ = std::fmt::Write::write_fmt(&mut output, format_args!("%{byte:02X}"));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{ClickHouseEndpoint, uri_encode_query};

    #[test]
    fn endpoint_rejects_control_and_path_fields() {
        assert!(ClickHouseEndpoint::try_new("host", 8_123, "axiusflow").is_ok());
        assert!(ClickHouseEndpoint::try_new("ho/st", 8_123, "axiusflow").is_err());
        assert!(ClickHouseEndpoint::try_new("host", 8_123, "").is_err());
    }

    #[test]
    fn query_encoding_escapes_every_reserved_byte() {
        assert_eq!(
            uri_encode_query("SELECT * FROM t WHERE a='b c'"),
            "SELECT%20%2A%20FROM%20t%20WHERE%20a%3D%27b%20c%27"
        );
    }
}
