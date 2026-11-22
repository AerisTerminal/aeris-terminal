//! Bybit V5 WebSocket `publicTrade` decoding.

use aeris_crypto_common::{
    CryptoError, CryptoResult, NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE, RawDecimal,
};
use aeris_market_data::{AggressorSide, EventMetadata, MarketTrade, QualifiedTimestamp};
use serde::Deserialize;

const NANOS_PER_MILLI: i64 = 1_000_000;

#[derive(Deserialize)]
struct WireTradeItem {
    #[serde(rename = "T")]
    ts_ms: i64,
    #[serde(rename = "S")]
    side: String,
    #[serde(rename = "v")]
    size: RawDecimal,
    #[serde(rename = "p")]
    price: RawDecimal,
    #[serde(rename = "i")]
    trade_id: String,
}

/// Decodes a Bybit `publicTrade.{SYM}` `data` array into `(trade_id, MarketTrade)` pairs.
///
/// # Errors
/// Returns an error if the JSON or any trade field is invalid.
pub fn decode_public_trades(
    data: &serde_json::value::RawValue,
    instrument_id: &str,
    entitlement_id: &str,
    session_generation: u64,
    next_sequence: &mut u64,
    received_unix_nanos: i64,
) -> CryptoResult<Vec<(String, MarketTrade)>> {
    let items: Vec<WireTradeItem> = serde_json::from_str(data.get()).map_err(|_| {
        CryptoError::MalformedPayload("bybit publicTrade payload is malformed".to_string())
    })?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let price = item.price.to_fixed(NORMALIZED_PRICE_SCALE)?;
        let quantity = item.size.to_fixed(NORMALIZED_QUANTITY_SCALE)?;
        if price <= 0 || quantity <= 0 {
            continue;
        }
        let aggressor = match item.side.as_str() {
            "Buy" => AggressorSide::Buy,
            "Sell" => AggressorSide::Sell,
            _ => AggressorSide::Unknown,
        };
        let exchange_nanos = item.ts_ms.checked_mul(NANOS_PER_MILLI).ok_or_else(|| {
            CryptoError::MalformedPayload("bybit trade timestamp overflowed".to_string())
        })?;
        *next_sequence = next_sequence.saturating_add(1).max(1);
        let trade = MarketTrade {
            metadata: EventMetadata {
                provider_id: "bybit".to_string(),
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
