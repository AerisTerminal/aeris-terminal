//! Public OKX V5 Perpetual Swap historical and live open interest over HTTPS.

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
    endpoints::OKX_REST_URL,
    http::{OkxHttpConfig, get_with_agent},
    identity::{OkxMarketKind, parse_provider_symbol},
};

const NANOS_PER_MILLI: i64 = 1_000_000;
const MAXIMUM_OKX_OI_POINTS: usize = 100;

/// Fetches historical and live open interest for an OKX Perpetual Swap instrument.
/// Spot instruments return an empty series.
///
/// # Errors
/// Returns an error on cancellation, transport failure, or malformed payload.
pub fn fetch_open_interest(
    request: &OpenInterestRequest,
    stop: &Arc<AtomicBool>,
) -> Result<Vec<OpenInterestSample>, String> {
    if stop.load(Ordering::Acquire) {
        return Err("OKX open interest request was cancelled".to_string());
    }
    let sym_ref = parse_provider_symbol(&request.instrument.provider_symbol)
        .map_err(|error| error.to_string())?;
    if sym_ref.kind != OkxMarketKind::Swap {
        return Ok(Vec::new());
    }
    fetch_swap_open_interest(
        &sym_ref.inst_id,
        request.period,
        request.maximum_points,
        stop,
    )
}

/// Fetches historical and live open interest for an OKX SWAP `instId` (e.g. `BTC-USDT-SWAP`).
///
/// # Errors
/// Returns an error on cancellation, transport failure, or malformed payload.
pub fn fetch_swap_open_interest(
    inst_id: &str,
    period: BarPeriod,
    maximum_points: usize,
    stop: &Arc<AtomicBool>,
) -> Result<Vec<OpenInterestSample>, String> {
    if stop.load(Ordering::Acquire) {
        return Err("OKX open interest request was cancelled".to_string());
    }
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| CancellableHttpClient::default().into_agent());
    let config = OkxHttpConfig::default();
    let oi_period = okx_oi_period(period);
    let cap = maximum_points.clamp(1, MAXIMUM_OKX_OI_POINTS);
    let hist_url = format!(
        "{OKX_REST_URL}/api/v5/rubik/stat/contracts/open-interest-history?instId={inst_id}&period={oi_period}&limit={cap}"
    );
    let live_url =
        format!("{OKX_REST_URL}/api/v5/public/open-interest?instType=SWAP&instId={inst_id}");

    let (hist_res, live_res) = std::thread::scope(|scope| {
        let hist_task = scope.spawn(|| get_with_agent(agent, &hist_url, config));
        let live_task = scope.spawn(|| get_with_agent(agent, &live_url, config));
        (
            hist_task.join().unwrap_or_else(|_| {
                Err(aeris_crypto_common::CryptoError::Transport(
                    "panicked".into(),
                ))
            }),
            live_task.join().unwrap_or_else(|_| {
                Err(aeris_crypto_common::CryptoError::Transport(
                    "panicked".into(),
                ))
            }),
        )
    });

    if stop.load(Ordering::Acquire) {
        return Err("OKX open interest request was cancelled".to_string());
    }

    let mut by_bucket = BTreeMap::new();
    let hist_bytes = hist_res.map_err(|error| error.to_string())?;
    for sample in decode_open_interest_history(&hist_bytes)? {
        let bucket = align_bucket(sample.timestamp_unix_nanos, period);
        by_bucket.insert(
            bucket,
            OpenInterestSample {
                timestamp_unix_nanos: bucket,
                ..sample
            },
        );
    }

    if let Ok(live_bytes) = live_res
        && let Ok(live) = decode_current_open_interest(&live_bytes)
    {
        let bucket = align_bucket(live.timestamp_unix_nanos, period);
        by_bucket.insert(
            bucket,
            OpenInterestSample {
                timestamp_unix_nanos: bucket,
                ..live
            },
        );
    }

    Ok(by_bucket.into_values().collect())
}

fn okx_oi_period(period: BarPeriod) -> &'static str {
    let seconds = period
        .duration_nanos()
        .map_or(86_400, |nanos| nanos / 1_000_000_000);
    if seconds <= 300 {
        "5m"
    } else if seconds <= 900 {
        "15m"
    } else if seconds <= 1_800 {
        "30m"
    } else if seconds <= 3_600 {
        "1H"
    } else if seconds <= 7_200 {
        "2H"
    } else if seconds <= 14_400 {
        "4H"
    } else if seconds <= 21_600 {
        "6H"
    } else if seconds <= 43_200 {
        "12H"
    } else {
        "1D"
    }
}

fn align_bucket(timestamp_unix_nanos: i64, period: BarPeriod) -> i64 {
    let bucket = period
        .duration_nanos()
        .filter(|&nanos| nanos > 0)
        .unwrap_or(86_400 * 1_000_000_000);
    timestamp_unix_nanos.div_euclid(bucket) * bucket
}

fn decode_open_interest_history(bytes: &[u8]) -> Result<Vec<OpenInterestSample>, String> {
    let root: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| "OKX open interest history payload is malformed".to_string())?;
    let code = root.get("code").and_then(serde_json::Value::as_str);
    if code != Some("0") {
        return Err(format!(
            "OKX open interest history rejected (code={code:?})"
        ));
    }
    let rows = root
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "OKX open interest history data missing".to_string())?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(cols) = row.as_array() else {
            continue;
        };
        let Some(ts_ms) = cols
            .first()
            .and_then(serde_json::Value::as_str)
            .and_then(|s| s.parse::<i64>().ok())
        else {
            continue;
        };
        // OKX rubik open-interest-history columns: [ts, oi (contracts), oiCcy (base coin), oiUsd]
        let Some(contracts) = cols
            .get(2)
            .and_then(serde_json::Value::as_str)
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
        else {
            continue;
        };
        let notional_usd = cols
            .get(3)
            .and_then(serde_json::Value::as_str)
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0);
        if let Some(timestamp_unix_nanos) = ts_ms.checked_mul(NANOS_PER_MILLI) {
            out.push(OpenInterestSample {
                timestamp_unix_nanos,
                contracts,
                high_contracts: Some(contracts),
                low_contracts: Some(contracts),
                notional_usd,
            });
        }
    }
    Ok(out)
}

fn decode_current_open_interest(bytes: &[u8]) -> Result<OpenInterestSample, String> {
    let root: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| "OKX live open interest payload is malformed".to_string())?;
    let item = root
        .get("data")
        .and_then(serde_json::Value::as_array)
        .and_then(|arr| arr.first())
        .ok_or_else(|| "OKX live open interest entry missing".to_string())?;
    let contracts = item
        .get("oiCcy")
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or_else(|| "OKX live open interest oiCcy missing".to_string())?;
    let notional_usd = item
        .get("oiUsd")
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0);
    let ts_ms = item
        .get("ts")
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| "OKX live open interest ts missing".to_string())?;
    let timestamp_unix_nanos = ts_ms
        .checked_mul(NANOS_PER_MILLI)
        .ok_or_else(|| "OKX live open interest timestamp overflow".to_string())?;
    Ok(OpenInterestSample {
        timestamp_unix_nanos,
        contracts,
        high_contracts: Some(contracts),
        low_contracts: Some(contracts),
        notional_usd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_okx_open_interest_history_and_live_payloads() {
        let hist = br#"{
            "code": "0",
            "data": [
                ["1795604400000", "3184234.86", "31842.3486", "1862478075.02"],
                ["1795600800000", "3193506.35", "31935.0635", "1877896700.02"]
            ],
            "msg": ""
        }"#;
        let samples = decode_open_interest_history(hist).expect("decode history");
        assert_eq!(samples.len(), 2);
        assert!((samples[0].contracts - 31_842.3486).abs() < 1e-4);
        assert!(samples[0].notional_usd.is_some());

        let live = br#"{
            "code": "0",
            "data": [{"instId": "BTC-USDT-SWAP", "instType": "SWAP", "oi": "3174800.51", "oiCcy": "31748.0051", "oiUsd": "1857582128.00", "ts": "1795606778339"}],
            "msg": ""
        }"#;
        let current = decode_current_open_interest(live).expect("decode live");
        assert!((current.contracts - 31_748.0051).abs() < 1e-4);
        assert_eq!(current.timestamp_unix_nanos, 1_795_606_778_339_000_000);
    }
}
