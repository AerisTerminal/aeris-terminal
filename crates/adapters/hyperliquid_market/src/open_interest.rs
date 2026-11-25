//! Public Hyperliquid live open-interest snapshot via `POST /info` (`metaAndAssetCtxs`).

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use aeris_market_data::BarPeriod;
use aeris_provider_spi::{OpenInterestRequest, OpenInterestSample};

use crate::http::{HyperliquidHttpConfig, post_info};

/// Fetches current open interest for a Hyperliquid perpetual instrument aligned to the current bar.
/// Spot markets return an empty series.
///
/// # Errors
/// Returns an error on cancellation, transport failure, or malformed payload.
pub fn fetch_open_interest(
    request: &OpenInterestRequest,
    stop: &Arc<AtomicBool>,
) -> Result<Vec<OpenInterestSample>, String> {
    if stop.load(Ordering::Acquire) {
        return Err("Hyperliquid open interest request was cancelled".to_string());
    }
    let coin = request.instrument.provider_symbol.as_str();
    if coin.starts_with('@') || coin.contains('/') {
        return Ok(Vec::new());
    }
    let body = if let Some((dex, _)) = coin.split_once(':') {
        serde_json::json!({ "type": "metaAndAssetCtxs", "dex": dex })
    } else {
        serde_json::json!({ "type": "metaAndAssetCtxs" })
    };
    let bytes = post_info(&body, HyperliquidHttpConfig::default())?;
    if stop.load(Ordering::Acquire) {
        return Err("Hyperliquid open interest request was cancelled".to_string());
    }
    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0);
    let Some((contracts, notional_usd)) = decode_perp_open_interest(&bytes, coin)? else {
        return Ok(Vec::new());
    };
    Ok(vec![OpenInterestSample {
        timestamp_unix_nanos: align_bucket(now_nanos, request.period),
        contracts,
        high_contracts: Some(contracts),
        low_contracts: Some(contracts),
        notional_usd,
    }])
}

fn align_bucket(timestamp_unix_nanos: i64, period: BarPeriod) -> i64 {
    let bucket = period
        .duration_nanos()
        .filter(|&nanos| nanos > 0)
        .unwrap_or(86_400 * 1_000_000_000);
    timestamp_unix_nanos.div_euclid(bucket) * bucket
}

fn decode_perp_open_interest(
    bytes: &[u8],
    coin: &str,
) -> Result<Option<(f64, Option<f64>)>, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| "Hyperliquid metaAndAssetCtxs payload is malformed".to_string())?;
    let pair = value
        .as_array()
        .filter(|arr| arr.len() >= 2)
        .ok_or_else(|| "Hyperliquid metaAndAssetCtxs response is not a pair".to_string())?;
    let universe = pair[0]
        .get("universe")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Hyperliquid universe missing".to_string())?;
    let ctxs = pair[1]
        .as_array()
        .ok_or_else(|| "Hyperliquid asset contexts missing".to_string())?;
    let Some(index) = universe.iter().position(|entry| {
        entry
            .get("name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(coin))
    }) else {
        return Ok(None);
    };
    let Some(ctx) = ctxs.get(index) else {
        return Ok(None);
    };
    let Some(contracts) = ctx
        .get("openInterest")
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
    else {
        return Ok(None);
    };
    let mark = ctx
        .get("markPx")
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0);
    Ok(Some((contracts, mark.map(|px| contracts * px))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_hyperliquid_perp_open_interest() {
        let payload = br#"[
            {"universe": [{"name": "BTC"}, {"name": "ETH"}]},
            [
                {"openInterest": "35241.5", "markPx": "60000.0"},
                {"openInterest": "120000.0", "markPx": "3000.0"}
            ]
        ]"#;
        let (oi, usd) = decode_perp_open_interest(payload, "BTC")
            .expect("decode")
            .expect("BTC present");
        assert!((oi - 35_241.5).abs() < 1e-6);
        assert!((usd.expect("notional") - 2_114_490_000.0).abs() < 1.0);
    }
}
