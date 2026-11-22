//! OKX V5 WebSocket `trades` decoding with exact `ctVal` base-unit scaling.

use aeris_crypto_common::{
    CryptoError, CryptoResult, NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE, RawDecimal,
    multiply_fixed_decimal,
};
use aeris_market_data::{AggressorSide, EventMetadata, MarketTrade, QualifiedTimestamp};
use serde::Deserialize;

const NANOS_PER_MILLI: i64 = 1_000_000;
const ONE_FIXED: i64 = 100_000_000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTradeItem {
    trade_id: String,
    px: RawDecimal,
    sz: RawDecimal,
    side: String,
    ts: RawDecimal,
}

/// Decodes an OKX `trades` `data` array into `(trade_id, MarketTrade)` pairs,
/// scaling SWAP contract counts by `ct_val_fixed` to base-asset units.
///
/// # Errors
/// Returns an error if the JSON or any trade field is invalid.
#[allow(clippy::too_many_arguments)]
pub fn decode_trades(
    data: &serde_json::value::RawValue,
    instrument_id: &str,
    entitlement_id: &str,
    ct_val_fixed: i64,
    session_generation: u64,
    next_sequence: &mut u64,
    received_unix_nanos: i64,
) -> CryptoResult<Vec<(String, MarketTrade)>> {
    let items: Vec<WireTradeItem> = serde_json::from_str(data.get()).map_err(|_| {
        CryptoError::MalformedPayload("okx trades payload is malformed".to_string())
    })?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let price = item.px.to_fixed(NORMALIZED_PRICE_SCALE)?;
        let raw_qty = item.sz.to_fixed(NORMALIZED_QUANTITY_SCALE)?;
        let quantity = if ct_val_fixed == ONE_FIXED {
            raw_qty
        } else {
            multiply_fixed_decimal(raw_qty, ct_val_fixed, NORMALIZED_QUANTITY_SCALE)?
        };
        if price <= 0 || quantity <= 0 {
            continue;
        }
        let aggressor = match item.side.as_str() {
            "buy" => AggressorSide::Buy,
            "sell" => AggressorSide::Sell,
            _ => AggressorSide::Unknown,
        };
        let ts_ms: i64 = item.ts.as_str().parse().map_err(|_| {
            CryptoError::MalformedPayload("okx trade timestamp is malformed".to_string())
        })?;
        let exchange_nanos = ts_ms.checked_mul(NANOS_PER_MILLI).ok_or_else(|| {
            CryptoError::MalformedPayload("okx trade timestamp overflowed".to_string())
        })?;
        *next_sequence = next_sequence.saturating_add(1).max(1);
        let trade = MarketTrade {
            metadata: EventMetadata {
                provider_id: "okx".to_string(),
                instrument_id: instrument_id.to_string(),
                entitlement_id: entitlement_id.to_string(),
                source_sequence: *next_sequence,
                session_generation,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(exchange_nanos),
                    provider_unix_nanos: Some(exchange_nanos),
                    received_unix_nanos,
                },
            },
            trade_id: item.trade_id.clone(),
            price,
            quantity,
            aggressor,
        };
        trade
            .validate()
            .map_err(|error| CryptoError::MalformedPayload(error.to_string()))?;
        out.push((item.trade_id, trade));
    }
    Ok(out)
}
