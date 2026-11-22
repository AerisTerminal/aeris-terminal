//! Binance WebSocket aggregate trade (`aggTrade`) decoding.

use aeris_crypto_common::{
    CryptoError, CryptoResult, NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE, RawDecimal,
};
use aeris_market_data::{AggressorSide, EventMetadata, MarketTrade, QualifiedTimestamp};
use serde::Deserialize;

const NANOS_PER_MILLI: i64 = 1_000_000;

#[derive(Deserialize)]
struct WireAggTrade {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "a")]
    agg_trade_id: u64,
    #[serde(rename = "p")]
    price: RawDecimal,
    #[serde(rename = "q")]
    quantity: RawDecimal,
    #[serde(rename = "T")]
    trade_time_ms: i64,
    #[serde(rename = "m")]
    buyer_is_maker: bool,
}

/// Decodes one Binance `aggTrade` payload into `(wire_symbol, agg_trade_id, MarketTrade)`.
///
/// # Errors
/// Returns an error if the JSON payload or trade fields are invalid.
pub fn decode_agg_trade(
    payload: &serde_json::value::RawValue,
    instrument_id: &str,
    entitlement_id: &str,
    session_generation: u64,
    source_sequence: u64,
    received_unix_nanos: i64,
) -> CryptoResult<(String, u64, MarketTrade)> {
    let wire: WireAggTrade = serde_json::from_str(payload.get()).map_err(|_| {
        CryptoError::MalformedPayload("binance aggTrade payload is malformed".to_string())
    })?;
    let price = wire.price.to_fixed(NORMALIZED_PRICE_SCALE)?;
    let quantity = wire.quantity.to_fixed(NORMALIZED_QUANTITY_SCALE)?;
    if price <= 0 || quantity <= 0 {
        return Err(CryptoError::MalformedPayload(
            "binance aggTrade price or quantity is non-positive".to_string(),
        ));
    }
    let exchange_nanos = wire
        .trade_time_ms
        .checked_mul(NANOS_PER_MILLI)
        .ok_or_else(|| {
            CryptoError::MalformedPayload("binance aggTrade timestamp overflowed".to_string())
        })?;
    let aggressor = if wire.buyer_is_maker {
        AggressorSide::Sell
    } else {
        AggressorSide::Buy
    };
    let trade = MarketTrade {
        metadata: EventMetadata {
            provider_id: "binance".to_string(),
            instrument_id: instrument_id.to_string(),
            entitlement_id: entitlement_id.to_string(),
            source_sequence,
            session_generation,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(exchange_nanos),
                provider_unix_nanos: Some(exchange_nanos),
                received_unix_nanos,
            },
        },
        trade_id: wire.agg_trade_id.to_string(),
        price,
        quantity,
        aggressor,
    };
    trade
        .validate()
        .map_err(|error| CryptoError::MalformedPayload(error.to_string()))?;
    Ok((wire.symbol.to_ascii_uppercase(), wire.agg_trade_id, trade))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_agg_trade_aggressor_side_correctly() {
        let raw = serde_json::value::RawValue::from_string(
            r#"{"e":"aggTrade","E":1700000000001,"s":"BTCUSDT","a":42,"p":"65000.5","q":"0.25","T":1700000000000,"m":true}"#.to_string(),
        )
        .unwrap();
        let (sym, id, trade) = decode_agg_trade(
            &raw,
            "binance:usdm:BTCUSDT",
            "binance-public",
            1,
            1,
            1_700_000_000_002_000_000,
        )
        .unwrap();
        assert_eq!(sym, "BTCUSDT");
        assert_eq!(id, 42);
        assert_eq!(trade.aggressor, AggressorSide::Sell);
        assert_eq!(trade.price, 6_500_050_000_000);
        assert_eq!(trade.quantity, 25_000_000);
    }
}
