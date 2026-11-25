//! Public Bybit V5 Linear Perpetual historical open interest over HTTPS.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use aeris_market_data::BarPeriod;
use aeris_platform_runtime::CancellableHttpClient;
use aeris_provider_spi::{OpenInterestRequest, OpenInterestSample};

use crate::{
    endpoints::BYBIT_REST_URL,
    http::{BybitHttpConfig, get_with_agent},
    identity::{BybitMarketKind, parse_provider_symbol},
};

const NANOS_PER_MILLI: i64 = 1_000_000;
const BYBIT_OI_PAGE_LIMIT: usize = 200;
const BYBIT_OI_MAX_PAGES: usize = 2;

/// Fetches historical open interest for a Bybit Linear Perpetual instrument.
/// Spot instruments return an empty series.
///
/// # Errors
/// Returns an error on cancellation, transport failure, or malformed payload.
pub fn fetch_open_interest(
    request: &OpenInterestRequest,
    stop: &Arc<AtomicBool>,
) -> Result<Vec<OpenInterestSample>, String> {
    if stop.load(Ordering::Acquire) {
        return Err("Bybit open interest request was cancelled".to_string());
    }
    let sym_ref = parse_provider_symbol(&request.instrument.provider_symbol)
        .map_err(|error| error.to_string())?;
    if sym_ref.kind != BybitMarketKind::Linear {
        return Ok(Vec::new());
    }
    fetch_linear_open_interest(
        &sym_ref.wire_symbol,
        request.period,
        request.maximum_points,
        stop,
    )
}

/// Fetches historical open interest for a raw Bybit linear symbol (e.g. `BTCUSDT`).
///
/// # Errors
/// Returns an error on cancellation, transport failure, or malformed payload.
pub fn fetch_linear_open_interest(
    wire_symbol: &str,
    period: BarPeriod,
    maximum_points: usize,
    stop: &Arc<AtomicBool>,
) -> Result<Vec<OpenInterestSample>, String> {
    if stop.load(Ordering::Acquire) {
        return Err("Bybit open interest request was cancelled".to_string());
    }
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| CancellableHttpClient::default().into_agent());
    let config = BybitHttpConfig::default();
    let interval = bybit_oi_interval(period);
    let target = maximum_points.clamp(1, BYBIT_OI_PAGE_LIMIT * BYBIT_OI_MAX_PAGES);

    let mut by_bucket = BTreeMap::new();
    let mut cursor: Option<String> = None;

    for _ in 0..BYBIT_OI_MAX_PAGES {
        if stop.load(Ordering::Acquire) {
            return Err("Bybit open interest request was cancelled".to_string());
        }
        let page_limit = target
            .saturating_sub(by_bucket.len())
            .clamp(1, BYBIT_OI_PAGE_LIMIT);
        let url = match &cursor {
            Some(c) => format!(
                "{BYBIT_REST_URL}/v5/market/open-interest?category=linear&symbol={wire_symbol}&intervalTime={interval}&limit={page_limit}&cursor={c}"
            ),
            None => format!(
                "{BYBIT_REST_URL}/v5/market/open-interest?category=linear&symbol={wire_symbol}&intervalTime={interval}&limit={page_limit}"
            ),
        };
        let bytes = get_with_agent(agent, &url, config).map_err(|error| error.to_string())?;
        let (samples, next_cursor) = decode_open_interest_page(&bytes)?;
        if samples.is_empty() {
            break;
        }
        for sample in samples {
            let bucket = align_bucket(sample.timestamp_unix_nanos, period);
            by_bucket.entry(bucket).or_insert(OpenInterestSample {
                timestamp_unix_nanos: bucket,
                ..sample
            });
        }
        if by_bucket.len() >= target || next_cursor.is_none() {
            break;
        }
        cursor = next_cursor;
    }

    Ok(by_bucket.into_values().collect())
}

fn bybit_oi_interval(period: BarPeriod) -> &'static str {
    let seconds = period
        .duration_nanos()
        .map_or(86_400, |nanos| nanos / 1_000_000_000);
    if seconds <= 300 {
        "5min"
    } else if seconds <= 900 {
        "15min"
    } else if seconds <= 1_800 {
        "30min"
    } else if seconds <= 7_200 {
        "1h"
    } else if seconds <= 43_200 {
        "4h"
    } else {
        "1d"
    }
}

fn align_bucket(timestamp_unix_nanos: i64, period: BarPeriod) -> i64 {
    let bucket = period
        .duration_nanos()
        .filter(|&nanos| nanos > 0)
        .unwrap_or(86_400 * 1_000_000_000);
    timestamp_unix_nanos.div_euclid(bucket) * bucket
}

fn decode_open_interest_page(
    bytes: &[u8],
) -> Result<(Vec<OpenInterestSample>, Option<String>), String> {
    let root: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| "Bybit open interest payload is malformed".to_string())?;
    let ret_code = root.get("retCode").and_then(serde_json::Value::as_i64);
    if ret_code != Some(0) {
        return Err(format!(
            "Bybit open interest rejected (retCode={ret_code:?})"
        ));
    }
    let result = root
        .get("result")
        .ok_or_else(|| "Bybit open interest result missing".to_string())?;
    let next_cursor = result
        .get("nextPageCursor")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let list = result
        .get("list")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Bybit open interest list missing".to_string())?;
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        let Some(ts_ms) = item
            .get("timestamp")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| s.parse::<i64>().ok())
        else {
            continue;
        };
        let Some(contracts) = item
            .get("openInterest")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
        else {
            continue;
        };
        if let Some(timestamp_unix_nanos) = ts_ms.checked_mul(NANOS_PER_MILLI) {
            out.push(OpenInterestSample {
                timestamp_unix_nanos,
                contracts,
                notional_usd: None,
            });
        }
    }
    Ok((out, next_cursor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_bybit_open_interest_page() {
        let payload = br#"{
            "retCode": 0,
            "retMsg": "OK",
            "result": {
                "symbol": "BTCUSDT",
                "category": "linear",
                "list": [
                    {"openInterest": "69621.17000000", "timestamp": "1795604400000"},
                    {"openInterest": "69529.88500000", "timestamp": "1795600800000"}
                ],
                "nextPageCursor": "abc123"
            }
        }"#;
        let (samples, cursor) = decode_open_interest_page(payload).expect("decode page");
        assert_eq!(samples.len(), 2);
        assert_eq!(cursor.as_deref(), Some("abc123"));
        assert!((samples[0].contracts - 69_621.17).abs() < 1e-6);
        assert_eq!(samples[0].timestamp_unix_nanos, 1_795_604_400_000_000_000);
    }
}
