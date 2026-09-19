//! BBO and L2 snapshot decoding at full available precision.
//!
//! The public API documents at most 20 book levels per side and the
//! WebSocket book is a periodically published snapshot. Each message replaces
//! the previous image; pending older snapshots may be replaced. Empty sides
//! are legal (one-sided markets) and surface as an empty level list with a
//! valid sequence.

use serde::Deserialize;
use tradingplot_market_data::{
    DepthLevel, DepthSnapshot, EventMetadata, QualifiedTimestamp, TopOfBookQuote,
};

use crate::decimal::{NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE, parse_decimal_to_fixed};

/// Maximum book levels accepted per side (the documented public depth).
pub const MAXIMUM_HYPERLIQUID_BOOK_LEVELS: usize = 20;

#[derive(Debug, Deserialize)]
struct WireLevel {
    px: String,
    sz: String,
    n: u32,
}

#[derive(Debug, Deserialize)]
struct WireBook {
    coin: String,
    time: i64,
    #[serde(default)]
    levels: Option<Box<serde_json::value::RawValue>>,
}

/// Normalized decoded book with its source sequence.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedBook {
    /// Canonical snapshot replacing the previous image.
    pub snapshot: DepthSnapshot,
}

/// Decodes one `l2Book` payload into a canonical snapshot image.
///
/// The payload arrives as raw JSON text so fractional numbers never pass
/// through `f64`.
///
/// Empty sides decode to empty level lists. Levels arrive best-first and are
/// validated sorted, unique, and uncrossed. More than 20 levels per side is
/// rejected rather than silently truncated.
///
/// # Errors
///
/// Returns an error for invalid identity, coin mismatch, malformed levels,
/// invalid or misordered levels, crossed books, or depth beyond the
/// documented 20 levels per side.
pub fn decode_book_snapshot(
    payload: &serde_json::value::RawValue,
    wire_coin: &str,
    instrument_id: &str,
    entitlement_id: &str,
    session_generation: u64,
    source_sequence: u64,
    received_unix_nanos: i64,
) -> Result<DecodedBook, String> {
    if session_generation == 0 || source_sequence == 0 || received_unix_nanos <= 0 {
        return Err("hyperliquid book identity is invalid".to_string());
    }
    let book: WireBook = serde_json::from_str(payload.get())
        .map_err(|_| "hyperliquid book is malformed".to_string())?;
    if book.coin != wire_coin {
        return Err("hyperliquid book coin does not match the subscription".to_string());
    }
    if book.time < 0 {
        return Err("hyperliquid book timestamp is invalid".to_string());
    }
    let Some(levels) = book.levels else {
        return Err("hyperliquid book levels are malformed".to_string());
    };
    let sides: Vec<Vec<WireLevel>> = serde_json::from_str(levels.get())
        .map_err(|_| "hyperliquid book levels are malformed".to_string())?;
    if sides.len() != 2 {
        return Err("hyperliquid book levels are malformed".to_string());
    }
    let bids = decode_side(&sides[0], true)?;
    let asks = decode_side(&sides[1], false)?;
    if bids.len() > MAXIMUM_HYPERLIQUID_BOOK_LEVELS || asks.len() > MAXIMUM_HYPERLIQUID_BOOK_LEVELS
    {
        return Err("hyperliquid book exceeds the documented depth".to_string());
    }
    let exchange_nanos = book
        .time
        .checked_mul(1_000_000)
        .ok_or_else(|| "hyperliquid book timestamp overflowed".to_string())?;
    let snapshot = DepthSnapshot {
        metadata: EventMetadata {
            provider_id: "hyperliquid".to_string(),
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
        bids,
        asks,
    };
    snapshot
        .validate(MAXIMUM_HYPERLIQUID_BOOK_LEVELS)
        .map_err(|error| error.to_string())?;
    Ok(DecodedBook { snapshot })
}

fn decode_side(levels: &[WireLevel], is_bid: bool) -> Result<Vec<DepthLevel>, String> {
    let mut decoded = Vec::with_capacity(levels.len());
    for level in levels {
        let price = parse_decimal_to_fixed(&level.px, NORMALIZED_PRICE_SCALE)?;
        let quantity = parse_decimal_to_fixed(&level.sz, NORMALIZED_QUANTITY_SCALE)?;
        if price <= 0 || quantity <= 0 {
            return Err("hyperliquid book level is invalid".to_string());
        }
        decoded.push(DepthLevel {
            price,
            quantity,
            order_count: Some(level.n),
        });
    }
    // The wire order is best-first; enforce the direction explicitly so a
    // misordered image fails closed instead of silently flipping the book.
    for pair in decoded.windows(2) {
        let ordered = if is_bid {
            pair[0].price > pair[1].price
        } else {
            pair[0].price < pair[1].price
        };
        if !ordered {
            return Err("hyperliquid book levels are misordered".to_string());
        }
    }
    Ok(decoded)
}

/// Decodes the documented `bbo` channel envelope with validated identity and time.
///
/// Missing sides remain absent. The sequence is local ingestion order, not
/// exchange continuity evidence.
///
/// # Errors
///
/// Returns an error for malformed payloads or invalid level values.
pub fn decode_bbo_quote(
    payload: &serde_json::value::RawValue,
    wire_coin: &str,
    instrument_id: &str,
    entitlement_id: &str,
    session_generation: u64,
    source_sequence: u64,
    received_unix_nanos: i64,
) -> Result<TopOfBookQuote, String> {
    #[derive(Deserialize)]
    struct WireBboEnvelope {
        coin: String,
        time: i64,
        bbo: (Option<WireLevel>, Option<WireLevel>),
    }
    let envelope: WireBboEnvelope = serde_json::from_str(payload.get())
        .map_err(|_| "hyperliquid bbo is malformed".to_string())?;
    if envelope.coin != wire_coin || envelope.time < 0 {
        return Err("hyperliquid bbo identity or timestamp is invalid".to_string());
    }
    let exchange_unix_nanos = envelope
        .time
        .checked_mul(1_000_000)
        .ok_or_else(|| "hyperliquid bbo timestamp overflowed".to_string())?;
    let quote = TopOfBookQuote {
        metadata: EventMetadata {
            provider_id: "hyperliquid".to_string(),
            instrument_id: instrument_id.to_string(),
            entitlement_id: entitlement_id.to_string(),
            source_sequence,
            session_generation,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(exchange_unix_nanos),
                provider_unix_nanos: None,
                received_unix_nanos,
            },
        },
        bid: decode_bbo_side(envelope.bbo.0.as_ref())?,
        ask: decode_bbo_side(envelope.bbo.1.as_ref())?,
    };
    quote.validate().map_err(|error| error.to_string())?;
    Ok(quote)
}

fn decode_bbo_side(level: Option<&WireLevel>) -> Result<Option<DepthLevel>, String> {
    level
        .map(|level| {
            Ok::<_, String>(DepthLevel {
                price: parse_decimal_to_fixed(&level.px, NORMALIZED_PRICE_SCALE)?,
                quantity: parse_decimal_to_fixed(&level.sz, NORMALIZED_QUANTITY_SCALE)?,
                order_count: Some(level.n),
            })
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Re-encodes a fixture exactly as the socket delivers it: raw text the
    /// decoder parses without any intermediate `Value` float conversion.
    fn raw(value: &serde_json::Value) -> Box<serde_json::value::RawValue> {
        serde_json::value::RawValue::from_string(value.to_string()).expect("fixture encodes")
    }

    fn book() -> serde_json::Value {
        json!({
            "coin": "BTC",
            "time": 1_700_000_000_000_i64,
            "levels": [
                [{"px": "67000.5", "sz": "1.25", "n": 3},
                 {"px": "67000.0", "sz": "0.5", "n": 1}],
                [{"px": "67001.0", "sz": "0.75", "n": 2},
                 {"px": "67002.0", "sz": "2.0", "n": 1}],
            ],
        })
    }

    #[test]
    fn book_replacement_keeps_full_precision_and_sequence() {
        let first = decode_book_snapshot(
            &raw(&book()),
            "BTC",
            "hyperliquid:perp:BTC",
            "hyperliquid:public",
            4,
            9,
            1_700_000_000_001_000_000,
        )
        .expect("book");
        assert_eq!(first.snapshot.bids.len(), 2);
        assert_eq!(first.snapshot.asks.len(), 2);
        assert_eq!(first.snapshot.bids[0].price, 6_700_050_000_000);
        // A newer snapshot replaces the older image wholesale.
        let mut newer = book();
        newer["time"] = json!(1_700_000_000_100_i64);
        let second = decode_book_snapshot(
            &raw(&newer),
            "BTC",
            "hyperliquid:perp:BTC",
            "hyperliquid:public",
            4,
            10,
            1_700_000_000_101_000_000,
        )
        .expect("newer");
        assert!(second.snapshot.metadata.source_sequence > first.snapshot.metadata.source_sequence);
    }

    #[test]
    fn empty_sides_and_bbo_updates_decode_honestly() {
        let one_sided = json!({
            "coin": "BTC", "time": 1_700_000_000_000_i64,
            "levels": [[], [{"px": "1.0", "sz": "1.0", "n": 1}]],
        });
        let decoded = decode_book_snapshot(
            &raw(&one_sided),
            "BTC",
            "hyperliquid:perp:BTC",
            "hyperliquid:public",
            1,
            1,
            2,
        )
        .expect("one-sided");
        assert!(decoded.snapshot.bids.is_empty());
        assert_eq!(decoded.snapshot.asks.len(), 1);
    }

    #[test]
    fn bbo_validates_the_documented_envelope_and_missing_sides() {
        // Documented channel form: the whole `WsBbo` object with a
        // two-element `bbo` array, `null` for a missing side.
        let decode = |value: serde_json::Value| {
            decode_bbo_quote(
                &raw(&value),
                "BTC",
                "hyperliquid:perp:BTC",
                "hyperliquid:public",
                1,
                2,
                3,
            )
        };
        let quote = decode(json!({
            "coin": "BTC", "time": 1_700_000_000_000_i64,
            "bbo": [{"px": "1.0", "sz": "2.0", "n": 1},
                    {"px": "1.5", "sz": "1.0", "n": 1}],
        }))
        .expect("envelope bbo");
        assert!(quote.bid.is_some() && quote.ask.is_some());
        assert_eq!(
            quote.metadata.timestamps.exchange_unix_nanos,
            Some(1_700_000_000_000_000_000)
        );
        // One-sided envelope update never fabricates the absent side.
        let quote = decode(json!({
            "coin": "BTC", "time": 1_700_000_000_000_i64,
            "bbo": [null, {"px": "1.5", "sz": "1.0", "n": 1}],
        }))
        .expect("one-sided bbo");
        assert!(quote.bid.is_none() && quote.ask.is_some());
        for invalid in [
            json!({"coin": "BTC", "bbo": [null, null]}),
            json!({"coin": "ETH", "time": 1, "bbo": [null, null]}),
            json!({"coin": "BTC", "time": -1, "bbo": [null, null]}),
            json!({"coin": "BTC", "time": i64::MAX, "bbo": [null, null]}),
            json!({"coin": "BTC", "time": 1, "bbo": [{"px": "1", "sz": "1"}, null]}),
            json!({"coin": "BTC", "time": 1, "bbo": [{"px": "2", "sz": "1", "n": 1},
                {"px": "1", "sz": "1", "n": 1}]}),
        ] {
            assert!(decode(invalid).is_err());
        }
    }

    #[test]
    fn crossed_misordered_and_overdeep_books_fail_closed() {
        let mut crossed = book();
        crossed["levels"][1][0]["px"] = json!("66000.0");
        assert!(
            decode_book_snapshot(
                &raw(&crossed),
                "BTC",
                "hyperliquid:perp:BTC",
                "hyperliquid:public",
                1,
                1,
                2
            )
            .is_err()
        );
        let deep = json!({
            "coin": "BTC", "time": 1_i64,
            "levels": [
                (0..21).map(|i| json!({"px": format!("{}.0", 67000 - i),
                    "sz": "1.0", "n": 1})).collect::<Vec<_>>(),
                [],
            ],
        });
        assert!(
            decode_book_snapshot(
                &raw(&deep),
                "BTC",
                "hyperliquid:perp:BTC",
                "hyperliquid:public",
                1,
                1,
                2
            )
            .is_err()
        );
    }
}
