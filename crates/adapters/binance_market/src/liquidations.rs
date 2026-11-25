//! Binance USDT-M Futures forced liquidation stream decoder (`<symbol>@forceOrder`) and
//! provider-neutral liquidation query adapter.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use aeris_crypto_common::{CryptoError, CryptoResult, LiquidationEvent, LiquidationLedger};
use aeris_provider_spi::{LiquidationRequest, LiquidationSample};
use serde::Deserialize;

use crate::identity::{BinanceMarketKind, parse_provider_symbol};

#[derive(Deserialize)]
struct WireForceOrderEnvelope {
    #[serde(rename = "E", default)]
    event_time_ms: Option<i64>,
    #[serde(rename = "o")]
    order: WireForceOrder,
}

#[derive(Deserialize)]
struct WireForceOrder {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "S")]
    side: String,
    #[serde(rename = "q", default)]
    original_qty: String,
    #[serde(rename = "l", default)]
    last_qty: String,
    #[serde(rename = "z", default)]
    filled_qty: String,
    #[serde(rename = "p", default)]
    price: String,
    #[serde(rename = "ap", default)]
    avg_price: String,
    #[serde(rename = "T", default)]
    trade_time_ms: Option<i64>,
}

/// Decodes one Binance USDT-M Futures `forceOrder` payload into `(wire_symbol, LiquidationEvent)`.
///
/// Binance reports `S` as the forced market order side executed to close the liquidated position:
/// `"SELL"` closes a liquidated long position (`is_long = true`), while `"BUY"` closes a
/// liquidated short position (`is_long = false`).
///
/// # Errors
/// Returns an error if the JSON payload is malformed or lacks positive price/quantity fields.
pub fn decode_force_order(
    raw: &serde_json::value::RawValue,
) -> CryptoResult<(String, LiquidationEvent)> {
    let env: WireForceOrderEnvelope = serde_json::from_str(raw.get()).map_err(|_| {
        CryptoError::MalformedPayload("binance forceOrder payload is malformed".to_string())
    })?;
    let order = env.order;
    let is_long = match order.side.as_str() {
        "SELL" | "sell" => true,
        "BUY" | "buy" => false,
        _ => {
            return Err(CryptoError::MalformedPayload(
                "binance forceOrder side is unrecognized".to_string(),
            ));
        }
    };
    let contracts = parse_positive_f64(&order.filled_qty)
        .or_else(|| parse_positive_f64(&order.last_qty))
        .or_else(|| parse_positive_f64(&order.original_qty))
        .ok_or_else(|| {
            CryptoError::MalformedPayload("binance forceOrder quantity is invalid".to_string())
        })?;
    let price = parse_positive_f64(&order.avg_price)
        .or_else(|| parse_positive_f64(&order.price))
        .ok_or_else(|| {
            CryptoError::MalformedPayload("binance forceOrder price is invalid".to_string())
        })?;
    let timestamp_ms = order
        .trade_time_ms
        .or(env.event_time_ms)
        .filter(|&ts| ts > 0)
        .ok_or_else(|| {
            CryptoError::MalformedPayload("binance forceOrder timestamp is invalid".to_string())
        })?;
    let event = LiquidationEvent {
        timestamp_ms,
        is_long,
        contracts,
        usd: contracts * price,
    };
    Ok((order.symbol.to_ascii_uppercase(), event))
}

/// Returns bar-aligned liquidation samples recorded for a Binance USDT-M Perpetual instrument.
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
        return Err("Binance liquidations request was cancelled".to_string());
    }
    let sym_ref = parse_provider_symbol(&request.instrument.provider_symbol)
        .map_err(|error| error.to_string())?;
    if sym_ref.kind != BinanceMarketKind::UsdmPerp {
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

fn parse_positive_f64(value: &str) -> Option<f64> {
    value
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_binance_long_and_short_force_orders() {
        let sell_raw: Box<serde_json::value::RawValue> = serde_json::from_str(
            r#"{"e":"forceOrder","E":1710000000000,"o":{"s":"BTCUSDT","S":"SELL","o":"LIMIT","f":"IOC","q":"0.50","p":"60000.0","ap":"60100.0","X":"FILLED","l":"0.50","z":"0.50","T":1710000000123}}"#,
        )
        .unwrap();
        let (sym, evt) = decode_force_order(&sell_raw).unwrap();
        assert_eq!(sym, "BTCUSDT");
        assert!(evt.is_long, "SELL forceOrder closes a liquidated long");
        assert!((evt.contracts - 0.5).abs() < 1e-9);
        assert!((evt.usd - 30_050.0).abs() < 1e-6);

        let buy_raw: Box<serde_json::value::RawValue> = serde_json::from_str(
            r#"{"e":"forceOrder","E":1710000005000,"o":{"s":"ETHUSDT","S":"BUY","o":"LIMIT","f":"IOC","q":"10.0","p":"3000.0","ap":"3010.0","X":"FILLED","l":"10.0","z":"10.0","T":1710000005000}}"#,
        )
        .unwrap();
        let (sym, evt) = decode_force_order(&buy_raw).unwrap();
        assert_eq!(sym, "ETHUSDT");
        assert!(!evt.is_long, "BUY forceOrder closes a liquidated short");
        assert!((evt.usd - 30_100.0).abs() < 1e-6);
    }
}
