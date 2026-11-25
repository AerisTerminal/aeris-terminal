//! Public Binance USDT-M Futures historical and current open interest over HTTPS.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use aeris_market_data::BarPeriod;
use aeris_platform_runtime::CancellableHttpClient;
use aeris_provider_spi::{OpenInterestRequest, OpenInterestSample};

use crate::{
    endpoints::BINANCE_FUTURES_REST_URL,
    http::{BinanceHttpConfig, get_with_agent},
    identity::{BinanceMarketKind, parse_provider_symbol},
};

const NANOS_PER_MILLI: i64 = 1_000_000;
const MAXIMUM_BINANCE_OI_POINTS: usize = 500;

/// Fetches historical and live open interest for a Binance USDT-M Perpetual instrument.
/// Spot instruments return an empty series.
///
/// # Errors
/// Returns an error on cancellation, transport failure, or malformed payload.
pub fn fetch_open_interest(
    request: &OpenInterestRequest,
    stop: &Arc<AtomicBool>,
) -> Result<Vec<OpenInterestSample>, String> {
    if stop.load(Ordering::Acquire) {
        return Err("Binance open interest request was cancelled".to_string());
    }
    let sym_ref = parse_provider_symbol(&request.instrument.provider_symbol)
        .map_err(|error| error.to_string())?;
    if sym_ref.kind != BinanceMarketKind::UsdmPerp {
        return Ok(Vec::new());
    }
    fetch_usdm_open_interest(
        &sym_ref.wire_symbol,
        request.period,
        request.maximum_points,
        stop,
    )
}

/// Fetches historical and live open interest for a raw Binance USDT-M symbol (e.g. `BTCUSDT`).
///
/// # Errors
/// Returns an error on cancellation, transport failure, or malformed payload.
pub fn fetch_usdm_open_interest(
    wire_symbol: &str,
    period: BarPeriod,
    maximum_points: usize,
    stop: &Arc<AtomicBool>,
) -> Result<Vec<OpenInterestSample>, String> {
    if stop.load(Ordering::Acquire) {
        return Err("Binance open interest request was cancelled".to_string());
    }
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| CancellableHttpClient::default().into_agent());
    let config = BinanceHttpConfig::default();
    let oi_period = binance_oi_period(period);
    let cap = maximum_points.clamp(1, MAXIMUM_BINANCE_OI_POINTS);
    let hist_url = format!(
        "{BINANCE_FUTURES_REST_URL}/futures/data/openInterestHist?symbol={wire_symbol}&period={oi_period}&limit={cap}"
    );
    let live_url = format!("{BINANCE_FUTURES_REST_URL}/fapi/v1/openInterest?symbol={wire_symbol}");

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
        return Err("Binance open interest request was cancelled".to_string());
    }

    let mut by_bucket = BTreeMap::new();
    let hist_bytes = hist_res.map_err(|error| error.to_string())?;
    let mut hist_samples = decode_open_interest_history(&hist_bytes)?;
    hist_samples.sort_by_key(|sample| sample.timestamp_unix_nanos);
    for sample in hist_samples {
        let bucket = align_bucket(sample.timestamp_unix_nanos, period);
        fold_bucket_sample(&mut by_bucket, bucket, sample);
    }

    if let Ok(live_bytes) = live_res
        && let Ok(live) = decode_current_open_interest(&live_bytes)
    {
        let bucket = align_bucket(live.timestamp_unix_nanos, period);
        let notional_usd = by_bucket.values().next_back().and_then(|prev| {
            if prev.contracts > 0.0 {
                prev.notional_usd
                    .map(|usd| (usd / prev.contracts) * live.contracts)
            } else {
                None
            }
        });
        fold_bucket_sample(
            &mut by_bucket,
            bucket,
            OpenInterestSample {
                timestamp_unix_nanos: bucket,
                contracts: live.contracts,
                high_contracts: Some(live.contracts),
                low_contracts: Some(live.contracts),
                notional_usd,
            },
        );
    }

    Ok(by_bucket.into_values().collect())
}

fn fold_bucket_sample(
    by_bucket: &mut BTreeMap<i64, OpenInterestSample>,
    bucket: i64,
    sample: OpenInterestSample,
) {
    let sample_high = sample.high_contracts.unwrap_or(sample.contracts);
    let sample_low = sample.low_contracts.unwrap_or(sample.contracts);
    by_bucket
        .entry(bucket)
        .and_modify(|existing| {
            let prev_high = existing.high_contracts.unwrap_or(existing.contracts);
            let prev_low = existing.low_contracts.unwrap_or(existing.contracts);
            existing.contracts = sample.contracts;
            existing.high_contracts = Some(prev_high.max(sample_high).max(sample.contracts));
            existing.low_contracts = Some(prev_low.min(sample_low).min(sample.contracts));
            if sample.notional_usd.is_some() {
                existing.notional_usd = sample.notional_usd;
            }
        })
        .or_insert(OpenInterestSample {
            timestamp_unix_nanos: bucket,
            contracts: sample.contracts,
            high_contracts: Some(sample_high.max(sample.contracts)),
            low_contracts: Some(sample_low.min(sample.contracts)),
            notional_usd: sample.notional_usd,
        });
}

/// Picks one step finer than the chart interval where Binance supports it so higher-timeframe
/// Open Interest candles capture real intra-bar high/low wicks without sacrificing Binance's
/// 30-day history window.
fn binance_oi_period(period: BarPeriod) -> &'static str {
    let seconds = period
        .duration_nanos()
        .map_or(86_400, |nanos| nanos / 1_000_000_000);
    if seconds <= 1_800 {
        "5m"
    } else if seconds <= 3_600 {
        "15m"
    } else if seconds <= 7_200 {
        "30m"
    } else if seconds <= 21_600 {
        "1h"
    } else if seconds <= 43_200 {
        "2h"
    } else {
        "4h"
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
    let rows: Vec<serde_json::Value> = serde_json::from_slice(bytes)
        .map_err(|_| "Binance open interest history payload is malformed".to_string())?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(ts_ms) = row.get("timestamp").and_then(serde_json::Value::as_i64) else {
            continue;
        };
        let Some(contracts) = row
            .get("sumOpenInterest")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
        else {
            continue;
        };
        let notional_usd = row
            .get("sumOpenInterestValue")
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
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| "Binance live open interest payload is malformed".to_string())?;
    let contracts = value
        .get("openInterest")
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or_else(|| "Binance live open interest value missing".to_string())?;
    let ts_ms = value
        .get("time")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|d| i64::try_from(d.as_millis()).ok())
                .unwrap_or(0)
        });
    let timestamp_unix_nanos = ts_ms
        .checked_mul(NANOS_PER_MILLI)
        .ok_or_else(|| "Binance live open interest timestamp overflow".to_string())?;
    Ok(OpenInterestSample {
        timestamp_unix_nanos,
        contracts,
        high_contracts: Some(contracts),
        low_contracts: Some(contracts),
        notional_usd: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_binance_open_interest_history_and_live_payloads() {
        let hist = br#"[
            {"symbol":"BTCUSDT","sumOpenInterest":"98139.62400000","sumOpenInterestValue":"5830062875.4192","timestamp":1795597200000},
            {"symbol":"BTCUSDT","sumOpenInterest":"97734.14100000","sumOpenInterestValue":"5772432476.2266","timestamp":1795600800000}
        ]"#;
        let samples = decode_open_interest_history(hist).expect("decode history");
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].timestamp_unix_nanos, 1_795_597_200_000_000_000);
        assert!((samples[0].contracts - 98_139.624).abs() < 1e-6);
        assert!(samples[0].notional_usd.is_some());

        let live = br#"{"symbol":"BTCUSDT","openInterest":"98419.190","time":1795606774322}"#;
        let current = decode_current_open_interest(live).expect("decode live");
        assert!((current.contracts - 98_419.19).abs() < 1e-6);
        assert_eq!(current.timestamp_unix_nanos, 1_795_606_774_322_000_000);
    }

    #[test]
    fn folds_sub_bar_samples_into_intraday_high_low_wicks() {
        let mut by_bucket = BTreeMap::new();
        let bucket = 3_600 * 1_000_000_000;
        for (idx, contracts) in [100.0, 118.5, 92.25, 105.0].into_iter().enumerate() {
            fold_bucket_sample(
                &mut by_bucket,
                bucket,
                OpenInterestSample {
                    timestamp_unix_nanos: bucket + (idx as i64) * 900 * 1_000_000_000,
                    contracts,
                    high_contracts: Some(contracts),
                    low_contracts: Some(contracts),
                    notional_usd: None,
                },
            );
        }
        let sample = by_bucket.get(&bucket).expect("folded bucket");
        assert_eq!(sample.contracts, 105.0);
        assert_eq!(sample.high_contracts, Some(118.5));
        assert_eq!(sample.low_contracts, Some(92.25));
    }
}
