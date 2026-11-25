//! Coinbase Advanced Trade WebSocket `market_trades` and REST `/ticker` trade decoding.

use aeris_crypto_common::{
    CryptoError, CryptoResult, NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE, RawDecimal,
};
use aeris_market_data::{AggressorSide, EventMetadata, MarketTrade, QualifiedTimestamp};
use chrono::DateTime;
use serde::Deserialize;

#[derive(Deserialize)]
struct WireTradeEvent {
    #[serde(default)]
    trades: Vec<WireTrade>,
}

#[derive(Deserialize)]
struct WireTickerResponse {
    #[serde(default)]
    trades: Vec<WireTrade>,
}

#[derive(Deserialize)]
struct WireTrade {
    trade_id: String,
    product_id: String,
    price: RawDecimal,
    size: RawDecimal,
    time: String,
    #[serde(default)]
    side: String,
}

/// Parses an RFC3339 Coinbase timestamp string into Unix nanoseconds.
///
/// # Errors
/// Returns an error if the timestamp is malformed or out of `i64` nanosecond range.
pub fn parse_rfc3339_nanos(raw: &str) -> CryptoResult<i64> {
    let parsed = DateTime::parse_from_rfc3339(raw).map_err(|_| {
        CryptoError::MalformedPayload("coinbase timestamp is malformed".to_string())
    })?;
    parsed.timestamp_nanos_opt().ok_or_else(|| {
        CryptoError::MalformedPayload("coinbase timestamp overflowed nanoseconds".to_string())
    })
}

/// Converts Coinbase's resting maker `side` into the aggressive taker [`AggressorSide`].
///
/// Coinbase public trade messages report the resting maker order's side:
/// - `"SELL"` means a resting sell order was lifted by an aggressive buyer (`AggressorSide::Buy`).
/// - `"BUY"` means a resting buy order was hit by an aggressive seller (`AggressorSide::Sell`).
#[must_use]
pub fn aggressor_from_maker_side(side: &str) -> AggressorSide {
    if side.eq_ignore_ascii_case("SELL") {
        AggressorSide::Buy
    } else if side.eq_ignore_ascii_case("BUY") {
        AggressorSide::Sell
    } else {
        AggressorSide::Unknown
    }
}

fn build_trade(
    item: &WireTrade,
    instrument_id: &str,
    entitlement_id: &str,
    session_generation: u64,
    next_sequence: &mut u64,
    received_unix_nanos: i64,
) -> CryptoResult<Option<(String, MarketTrade)>> {
    let price = item.price.to_fixed(NORMALIZED_PRICE_SCALE)?;
    let quantity = item.size.to_fixed(NORMALIZED_QUANTITY_SCALE)?;
    if price <= 0 || quantity <= 0 {
        return Ok(None);
    }
    let exchange_nanos = parse_rfc3339_nanos(&item.time)?;
    let aggressor = aggressor_from_maker_side(&item.side);
    *next_sequence = next_sequence.saturating_add(1).max(1);
    let trade = MarketTrade {
        metadata: EventMetadata {
            provider_id: "coinbase".to_string(),
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
    Ok(Some((item.trade_id.clone(), trade)))
}

/// Decodes a Coinbase `market_trades` WebSocket `events` array into `(product_id, trade_id, MarketTrade)` items
/// ordered chronologically (oldest first within each event batch).
///
/// # Errors
/// Returns an error if the JSON or any trade field is malformed.
pub fn decode_ws_trades(
    events_raw: &serde_json::value::RawValue,
    resolve_instrument: impl Fn(&str) -> Option<(String, String)>,
    session_generation: u64,
    next_sequence: &mut u64,
    received_unix_nanos: i64,
) -> CryptoResult<Vec<(String, String, MarketTrade)>> {
    let events: Vec<WireTradeEvent> = serde_json::from_str(events_raw.get()).map_err(|_| {
        CryptoError::MalformedPayload("coinbase ws trades payload is malformed".to_string())
    })?;
    let mut out = Vec::new();
    for event in events {
        // Coinbase snapshot/update batches often arrive newest-first; emit oldest-first.
        for item in event.trades.into_iter().rev() {
            let product_id = item.product_id.to_ascii_uppercase();
            let Some((instrument_id, entitlement_id)) = resolve_instrument(&product_id) else {
                continue;
            };
            if let Some((trade_id, trade)) = build_trade(
                &item,
                &instrument_id,
                &entitlement_id,
                session_generation,
                next_sequence,
                received_unix_nanos,
            )? {
                out.push((product_id, trade_id, trade));
            }
        }
    }
    Ok(out)
}

/// Decodes a Coinbase REST `/market/products/{id}/ticker` trade backfill response
/// into chronological `(trade_id, MarketTrade)` pairs.
///
/// # Errors
/// Returns an error if the JSON or any trade field is malformed.
#[allow(clippy::too_many_arguments)]
pub fn decode_ticker_trades(
    raw: &[u8],
    instrument_id: &str,
    entitlement_id: &str,
    session_generation: u64,
    next_sequence: &mut u64,
    received_unix_nanos: i64,
) -> CryptoResult<Vec<(String, MarketTrade)>> {
    let response: WireTickerResponse = serde_json::from_slice(raw).map_err(|_| {
        CryptoError::MalformedPayload("coinbase ticker trades payload is malformed".to_string())
    })?;
    let mut out = Vec::with_capacity(response.trades.len());
    for item in response.trades.into_iter().rev() {
        if let Some(pair) = build_trade(
            &item,
            instrument_id,
            entitlement_id,
            session_generation,
            next_sequence,
            received_unix_nanos,
        )? {
            out.push(pair);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverts_maker_side_to_aggressor_side_and_orders_chronologically() {
        let raw = serde_json::value::RawValue::from_string(
            r#"[{"type":"update","trades":[
                {"trade_id":"102","product_id":"BTC-USD","price":"65001.50","size":"0.25","time":"2026-11-25T14:38:52.205602Z","side":"SELL"},
                {"trade_id":"101","product_id":"BTC-USD","price":"65000.00","size":"0.50","time":"2026-11-25T14:38:51.100000Z","side":"BUY"}
            ]}]"#
            .to_string(),
        )
        .unwrap();
        let mut seq = 0;
        let decoded = decode_ws_trades(
            &raw,
            |p| {
                (p == "BTC-USD").then(|| {
                    (
                        "coinbase:spot:BTC-USD".to_string(),
                        "coinbase-public".to_string(),
                    )
                })
            },
            1,
            &mut seq,
            1_795_617_533_000_000_000,
        )
        .expect("decode trades");
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].1, "101");
        assert_eq!(decoded[0].2.aggressor, AggressorSide::Sell);
        assert_eq!(decoded[1].1, "102");
        assert_eq!(decoded[1].2.aggressor, AggressorSide::Buy);
    }
}
