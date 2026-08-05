//! Candle backfill over HTTPS with the exact fixed-point parser.
//!
//! Fetches recent one-minute candles from the advanced-trade REST API so a
//! client snapshot has history before the first live bar completes. The HTTP
//! client is deliberately minimal: one bounded GET per product over rustls.

use axiusflow_coinbase_market_adapter::{FixedPointValue, mantissa_at_scale};
use axiusflow_market_data::MarketBar;
use serde::Deserialize;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

const MAXIMUM_RESPONSE_BYTES: usize = 1_048_576;
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const REST_HOST: &str = "api.coinbase.com";
const REST_PORT: u16 = 443;

#[derive(Deserialize)]
struct CandlesResponse {
    candles: Vec<CandleMessage>,
}

#[derive(Deserialize)]
struct CandleMessage {
    start: String,
    high: String,
    low: String,
    open: String,
    close: String,
    volume: String,
}

/// Fetches up to `maximum` recent one-minute bars for one product, oldest
/// first, with source sequences left for the aggregator to assign.
///
/// # Errors
///
/// Returns an error for transport, protocol, or parse failures.
pub fn backfill_bars(product: &str, maximum: usize) -> Result<Vec<MarketBar>, String> {
    let response = https_get(&format!(
        "/api/v3/brokerage/market/products/{product}/candles?granularity=ONE_MINUTE&limit={maximum}"
    ))?;
    let parsed: CandlesResponse = serde_json::from_slice(&response)
        .map_err(|error| format!("candles parse failed: {error}"))?;
    let mut bars: Vec<MarketBar> = parsed
        .candles
        .iter()
        .map(|candle| {
            Ok(MarketBar {
                source_sequence: 0,
                exchange_timestamp_seconds: candle
                    .start
                    .parse::<i64>()
                    .map_err(|_| "candle start is not a unix timestamp".to_string())?,
                open: mantissa_at_scale(parse_fixed(&candle.open)?, 2)
                    .map_err(|error| error.to_string())?,
                high: mantissa_at_scale(parse_fixed(&candle.high)?, 2)
                    .map_err(|error| error.to_string())?,
                low: mantissa_at_scale(parse_fixed(&candle.low)?, 2)
                    .map_err(|error| error.to_string())?,
                close: mantissa_at_scale(parse_fixed(&candle.close)?, 2)
                    .map_err(|error| error.to_string())?,
                volume: mantissa_at_scale(parse_fixed(&candle.volume)?, 8)
                    .map_err(|error| error.to_string())?,
            })
        })
        .collect::<Result<_, String>>()?;
    bars.sort_by_key(|bar| bar.exchange_timestamp_seconds);
    // Source sequences are assigned by the aggregator, which validates after
    // assignment; a bare backfill bar carries no sequence of its own yet.
    Ok(bars)
}

fn parse_fixed(source: &str) -> Result<FixedPointValue, String> {
    FixedPointValue::parse(source).map_err(|error| error.to_string())
}

fn https_get(path: &str) -> Result<Vec<u8>, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = rustls::pki_types::ServerName::try_from(REST_HOST)
        .map_err(|_| "invalid REST host name".to_string())?;
    let mut connection = rustls::ClientConnection::new(Arc::new(config), server_name)
        .map_err(|error| error.to_string())?;
    let tcp = TcpStream::connect((REST_HOST, REST_PORT)).map_err(|error| error.to_string())?;
    tcp.set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|error| error.to_string())?;
    tcp.set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|error| error.to_string())?;
    let mut tcp = tcp;
    let mut tls = rustls::Stream::new(&mut connection, &mut tcp);
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {REST_HOST}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    tls.write_all(request.as_bytes())
        .map_err(|error| error.to_string())?;
    let mut response = Vec::new();
    tls.read_to_end(&mut response)
        .map_err(|error| error.to_string())?;
    if response.len() > MAXIMUM_RESPONSE_BYTES {
        return Err("candles response too large".to_string());
    }
    let text = String::from_utf8_lossy(&response).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| "malformed candles response".to_string())?;
    if status != 200 {
        return Err(format!("candles request returned {status}"));
    }
    let header_end = text
        .find("\r\n\r\n")
        .ok_or_else(|| "malformed candles response".to_string())?;
    let body_start = header_end + 4;
    let head = &text[..header_end];
    let raw = response[body_start..].to_vec();
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        decode_chunked(&raw)
    } else {
        Ok(raw)
    }
}

fn decode_chunked(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoded = Vec::new();
    let mut offset = 0_usize;
    loop {
        let line_end = raw[offset..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| "malformed chunk size".to_string())?
            + offset;
        let size_text = std::str::from_utf8(&raw[offset..line_end])
            .map_err(|_| "malformed chunk size".to_string())?
            .split(';')
            .next()
            .ok_or_else(|| "malformed chunk size".to_string())?;
        let size =
            usize::from_str_radix(size_text, 16).map_err(|_| "malformed chunk size".to_string())?;
        offset = line_end + 2;
        if size == 0 {
            return Ok(decoded);
        }
        if decoded.len() + size > MAXIMUM_RESPONSE_BYTES {
            return Err("candles response too large".to_string());
        }
        if offset + size + 2 > raw.len() {
            return Err("chunked body truncated".to_string());
        }
        decoded.extend_from_slice(&raw[offset..offset + size]);
        offset += size + 2;
    }
}
