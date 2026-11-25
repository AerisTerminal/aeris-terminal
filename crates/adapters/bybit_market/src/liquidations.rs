//! Bybit V5 Linear Perpetual liquidation stream decoder (`allLiquidation.<symbol>`) and
//! provider-neutral liquidation query adapter.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use aeris_crypto_common::{CryptoError, CryptoResult, LiquidationEvent, LiquidationLedger};
use aeris_provider_spi::{LiquidationRequest, LiquidationSample};
use serde::Deserialize;

use crate::identity::{BybitMarketKind, parse_provider_symbol};

#[derive(Deserialize)]
struct WireBybitLiquidation {
    #[serde(rename = "T")]
    timestamp_ms: i64,
    #[serde(rename = "S")]
    side: String,
    #[serde(rename = "v")]
    size: String,
    #[serde(rename = "p")]
    price: String,
}

/// Decodes one Bybit V5 `allLiquidation.<symbol>` `data` array into normalized liquidation events.
///
/// Bybit V5 reports `S` as the liquidated position side: `"Buy"` means a long position was
/// liquidated (`is_long = true`) and `"Sell"` means a short position was liquidated
/// (`is_long = false`).
///
/// # Errors
/// Returns an error if the JSON array is malformed.
pub fn decode_liquidations(
    raw: &serde_json::value::RawValue,
) -> CryptoResult<Vec<LiquidationEvent>> {
    let items: Vec<WireBybitLiquidation> = serde_json::from_str(raw.get()).map_err(|_| {
        CryptoError::MalformedPayload("bybit allLiquidation payload is malformed".to_string())
    })?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let is_long = match item.side.as_str() {
            "Buy" | "BUY" | "buy" => true,
            "Sell" | "SELL" | "sell" => false,
            _ => continue,
        };
        let Ok(contracts) = item.size.parse::<f64>() else {
            continue;
        };
        let Ok(price) = item.price.parse::<f64>() else {
            continue;
        };
        if item.timestamp_ms > 0
            && contracts.is_finite()
            && contracts > 0.0
            && price.is_finite()
            && price > 0.0
        {
            out.push(LiquidationEvent {
                timestamp_ms: item.timestamp_ms,
                is_long,
                contracts,
                usd: contracts * price,
            });
        }
    }
    Ok(out)
}

/// Returns bar-aligned liquidation samples recorded for a Bybit Linear Perpetual instrument.
/// Spot instruments return an empty series.
///
/// # Errors
/// Returns an error on cancellation or invalid provider symbol.
pub fn fetch_liquidations(
    ledger: &LiquidationLedger,
    request: &LiquidationRequest,
    stop: &Arc<AtomicBool>,
) -> Result<Vec<LiquidationSample>, String> {
    if stop.load(Ordering::Acquire) {
        return Err("Bybit liquidations request was cancelled".to_string());
    }
    let sym_ref = parse_provider_symbol(&request.instrument.provider_symbol)
        .map_err(|error| error.to_string())?;
    if sym_ref.kind != BybitMarketKind::Linear {
        return Ok(Vec::new());
    }
    Ok(ledger
        .snapshot_buckets(&sym_ref.wire_symbol, request.period, request.maximum_points)
        .into_iter()
        .map(|bucket| LiquidationSample {
            timestamp_unix_nanos: bucket.timestamp_unix_nanos,
            long_usd: bucket.long_usd,
            short_usd: bucket.short_usd,
            long_contracts: bucket.long_contracts,
            short_contracts: bucket.short_contracts,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_bybit_all_liquidation_array() {
        let raw: Box<serde_json::value::RawValue> = serde_json::from_str(
            r#"[
                {"T":1739502302929,"s":"BTCUSDT","S":"Buy","v":"0.15","p":"76500.50"},
                {"T":1739502303929,"s":"BTCUSDT","S":"Sell","v":"0.20","p":"76600.00"}
            ]"#,
        )
        .unwrap();
        let events = decode_liquidations(&raw).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events[0].is_long, "Bybit Buy indicates long liquidated");
        assert!((events[0].usd - 11_475.075).abs() < 1e-6);
        assert!(!events[1].is_long, "Bybit Sell indicates short liquidated");
        assert!((events[1].usd - 15_320.0).abs() < 1e-6);
    }
}
