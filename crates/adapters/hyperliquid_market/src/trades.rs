//! Trade decoding with provider identity, aggressor side, and gaps.
//!
//! Collected trades drive tick charts and trade-derived statistics. Trade
//! IDs and timestamps are preserved; the local ingestion sequence is never
//! presented as exchange continuity.
//!
//! Hyperliquid's `tid` is a 50-bit hash of the matching order ids, not a
//! consecutive sequence, so jumps between `tid` values are normal traffic and
//! never signal missing trades. The globally unique provider identity is the
//! documented `(block_time, coin, tid)` triple, which is what the decoded
//! trade id carries. Disconnects and overflows produce explicit gap
//! boundaries; missing ticks are never reconstructed from OHLC candles and
//! trade-count aggregation restarts at a new explicit boundary when
//! continuity cannot be established.

use std::collections::{BTreeMap, BTreeSet};

use axiusflow_market_data::{AggressorSide, EventMetadata, MarketTrade, QualifiedTimestamp};
use serde::Deserialize;

use crate::decimal::{NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE, parse_decimal_to_fixed};

#[derive(Debug, Deserialize)]
struct WireTrade {
    #[serde(default)]
    coin: String,
    #[serde(default)]
    px: String,
    #[serde(default)]
    sz: String,
    #[serde(default)]
    side: String,
    #[serde(default)]
    time: i64,
    #[serde(default)]
    tid: Option<u64>,
    #[serde(default)]
    hash: Option<String>,
}

/// One decoded batch of provider trades in wire order.
#[derive(Clone, Debug, PartialEq)]
pub struct HyperliquidTradeBatch {
    /// Normalized trades with provider continuity preserved.
    pub trades: Vec<MarketTrade>,
}

/// Decodes one `trades` WebSocket payload into normalized domain trades.
///
/// The payload arrives as raw JSON text (`WsTrade[]`). Provider trade
/// identity is the documented `(block_time, coin, tid)` triple when `tid`
/// is supplied, the L1 `hash` otherwise, and never the local sequence.
///
/// `instrument_id` is the stable engine id, `entitlement_id` the catalog
/// entitlement, `session_generation` the engine provider generation, and
/// `first_sequence` the next local ingestion sequence to assign.
///
/// # Errors
///
/// Returns an error for invalid identity, malformed rows, coin mismatch,
/// invalid timestamps, non-positive prices/sizes, or sequence overflow.
pub fn decode_trades_batch(
    payload: &serde_json::value::RawValue,
    wire_coin: &str,
    instrument_id: &str,
    entitlement_id: &str,
    session_generation: u64,
    received_unix_nanos: i64,
    first_sequence: u64,
) -> Result<HyperliquidTradeBatch, String> {
    if session_generation == 0 || first_sequence == 0 || received_unix_nanos <= 0 {
        return Err("hyperliquid trade batch identity is invalid".to_string());
    }
    if wire_coin.trim().is_empty() || instrument_id.trim().is_empty() {
        return Err("hyperliquid trade batch identity is invalid".to_string());
    }
    let rows: Vec<WireTrade> = serde_json::from_str(payload.get())
        .map_err(|_| "hyperliquid trades are malformed".to_string())?;
    if rows.len() > 1_000 {
        return Err("hyperliquid trade batch exceeds the provider bound".to_string());
    }
    let mut trades = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        if row.coin != wire_coin {
            return Err("hyperliquid trade coin does not match the subscription".to_string());
        }
        if row.time < 0 {
            return Err("hyperliquid trade timestamp is invalid".to_string());
        }
        let price = parse_decimal_to_fixed(&row.px, NORMALIZED_PRICE_SCALE)?;
        let quantity = parse_decimal_to_fixed(&row.sz, NORMALIZED_QUANTITY_SCALE)?;
        if price <= 0 || quantity <= 0 {
            return Err("hyperliquid trade price or size is invalid".to_string());
        }
        let aggressor = match row.side.as_str() {
            "B" => AggressorSide::Buy,
            "A" => AggressorSide::Sell,
            _ => AggressorSide::Unknown,
        };
        // `tid` is a match hash, unique only with its block time and coin.
        let trade_id = match (row.tid, row.hash.as_ref()) {
            (Some(tid), _) => format!("hl:{}:{}:{tid}", row.coin, row.time),
            (None, Some(hash)) if !hash.trim().is_empty() => format!("hl:{hash}"),
            _ => format!("hl:{}:{}:{index}", row.coin, row.time),
        };
        let sequence = first_sequence
            .checked_add(u64::try_from(index).map_err(|_| "sequence overflow".to_string())?)
            .ok_or_else(|| "hyperliquid trade sequence overflowed".to_string())?;
        if sequence == 0 {
            return Err("hyperliquid trade sequence overflowed".to_string());
        }
        let exchange_nanos = row
            .time
            .checked_mul(1_000_000)
            .ok_or_else(|| "hyperliquid trade timestamp overflowed".to_string())?;
        let trade = MarketTrade {
            metadata: EventMetadata {
                provider_id: "hyperliquid".to_string(),
                instrument_id: instrument_id.to_string(),
                entitlement_id: entitlement_id.to_string(),
                source_sequence: sequence,
                session_generation,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(exchange_nanos),
                    provider_unix_nanos: Some(exchange_nanos),
                    received_unix_nanos,
                },
            },
            trade_id,
            price,
            quantity,
            aggressor,
        };
        trade.validate().map_err(|error| error.to_string())?;
        trades.push(trade);
    }
    Ok(HyperliquidTradeBatch { trades })
}

/// Bounded duplicate tracker for one instrument.
///
/// Remembers observed provider trade ids and reports duplicates. Because
/// `tid` values are hashes, this tracker never infers exchange gaps from
/// them: a gap is reported only when the caller signals an upstream
/// disconnect or overflow via `continuity_broken`. The local ingestion
/// sequence is tracked separately and never used to claim exchange gap
/// detection.
#[derive(Debug, Default)]
pub struct TradeDedup {
    seen: BTreeSet<String>,
    bounds: BTreeMap<String, usize>,
}

impl TradeDedup {
    /// Observes one batch, returning `(duplicates, has_gap)`.
    ///
    /// `continuity_broken` must be set on reconnect, buffer overflow, or any
    /// other break after which trade continuity cannot be established; the
    /// consumer then resumes aggregation at a new explicit boundary.
    pub fn observe(
        &mut self,
        batch: &HyperliquidTradeBatch,
        continuity_broken: bool,
    ) -> (usize, bool) {
        let mut duplicates = 0;
        for trade in &batch.trades {
            if !self.seen.insert(trade.trade_id.clone()) {
                duplicates += 1;
            }
            *self.bounds.entry(trade.trade_id.clone()).or_insert(0) += 1;
            if self.bounds.len() > 20_000
                && let Some(first) = self.bounds.keys().next().cloned()
            {
                self.bounds.remove(&first);
                self.seen.remove(&first);
            }
        }
        (duplicates, continuity_broken)
    }
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

    fn payload() -> serde_json::Value {
        json!([
            {"coin": "BTC", "px": "67000.5", "sz": "0.1", "side": "B",
             "time": 1_700_000_000_000_i64, "tid": 118_906_512_037_719_u64,
             "hash": "0xaaa", "users": ["0x1", "0x2"]},
            {"coin": "BTC", "px": "67001.0", "sz": "0.2", "side": "A",
             "time": 1_700_000_000_100_i64, "tid": 907_359_904_431_134_u64,
             "hash": "0xbbb", "users": ["0x3", "0x4"]},
        ])
    }

    #[test]
    fn trades_preserve_provider_identity_and_aggressor_side() {
        let batch = decode_trades_batch(
            &raw(&payload()),
            "BTC",
            "hyperliquid:perp:BTC",
            "hyperliquid:public",
            3,
            1_700_000_000_200_000_000,
            10,
        )
        .expect("batch");
        assert_eq!(batch.trades.len(), 2);
        // Documented uniqueness key: (block_time, coin, tid).
        assert_eq!(
            batch.trades[0].trade_id,
            "hl:BTC:1700000000000:118906512037719"
        );
        assert_eq!(
            batch.trades[1].trade_id,
            "hl:BTC:1700000000100:907359904431134"
        );
        assert_eq!(batch.trades[0].aggressor, AggressorSide::Buy);
        assert_eq!(batch.trades[1].aggressor, AggressorSide::Sell);
        assert_eq!(batch.trades[0].metadata.source_sequence, 10);
    }

    #[test]
    fn non_consecutive_tids_are_normal_traffic_not_gaps() {
        let batch = decode_trades_batch(
            &raw(&payload()),
            "BTC",
            "hyperliquid:perp:BTC",
            "hyperliquid:public",
            3,
            1_700_000_000_200_000_000,
            10,
        )
        .expect("batch");
        let mut dedup = TradeDedup::default();
        // The two tids differ by ~8e14; as hashes that carries no meaning.
        let (duplicates, gap) = dedup.observe(&batch, false);
        assert_eq!(duplicates, 0);
        assert!(!gap, "hash-spaced tids must not raise a gap");
        let (duplicates, _) = dedup.observe(&batch, false);
        assert_eq!(duplicates, 2);
    }

    #[test]
    fn disconnect_and_overflow_raise_explicit_gaps() {
        let batch = decode_trades_batch(
            &raw(&payload()),
            "BTC",
            "hyperliquid:perp:BTC",
            "hyperliquid:public",
            3,
            1_700_000_000_200_000_000,
            10,
        )
        .expect("batch");
        let mut dedup = TradeDedup::default();
        let (_, gap) = dedup.observe(&batch, true);
        assert!(gap, "disconnect must surface an explicit gap");
    }

    #[test]
    fn malformed_trades_fail_without_fabrication() {
        let wrong_coin = json!([
            {"coin": "ETH", "px": "1.0", "sz": "1.0", "side": "B", "time": 1},
        ]);
        assert!(
            decode_trades_batch(
                &raw(&wrong_coin),
                "BTC",
                "hyperliquid:perp:BTC",
                "hyperliquid:public",
                1,
                2,
                1
            )
            .is_err()
        );
        let bad_price = json!([
            {"coin": "BTC", "px": "abc", "sz": "1.0", "side": "B", "time": 1},
        ]);
        assert!(
            decode_trades_batch(
                &raw(&bad_price),
                "BTC",
                "hyperliquid:perp:BTC",
                "hyperliquid:public",
                1,
                2,
                1
            )
            .is_err()
        );
    }
}
