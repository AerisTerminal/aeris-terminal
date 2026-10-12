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

use std::{
    collections::{HashSet, VecDeque},
    sync::Arc,
};

use aeris_market_data::{AggressorSide, EventMetadata, MarketTrade, QualifiedTimestamp};
use serde::Deserialize;

use crate::decimal::{NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE, parse_decimal_to_fixed};

#[derive(Debug, Deserialize)]
struct WireTrade {
    coin: String,
    px: String,
    sz: String,
    side: String,
    time: i64,
    tid: u64,
    hash: String,
    users: [String; 2],
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
        if row.time <= 0
            || row.hash.trim().is_empty()
            || row.users.iter().any(|user| user.trim().is_empty())
        {
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
            _ => return Err("hyperliquid trade aggressor side is invalid".to_string()),
        };
        // `tid` is a match hash, unique only with its block time and coin.
        let trade_id = format!("hl:{}:{}:{}", row.coin, row.time, row.tid);
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

/// Provider trade identities one connection already delivered for one coin.
///
/// Hyperliquid answers every `trades` subscription with a snapshot of the
/// coin's most recent trades, sent as an ordinary `trades` frame with no
/// snapshot marker. A coin resubscribed on the same connection therefore
/// replays trades an earlier subscription already delivered. Those replays
/// are the newest trades delivered before the unsubscribe, so a bounded
/// window of the newest delivered identities is enough to deliver each trade
/// once. Identity is the documented `(block_time, coin, tid)` triple carried
/// by the decoded trade id; `tid` order is never consulted.
#[derive(Debug, Default)]
pub struct DeliveredTradeIds {
    newest: VecDeque<Arc<str>>,
    delivered: HashSet<Arc<str>>,
}

impl DeliveredTradeIds {
    /// Newest delivered identities remembered per coin. A subscription
    /// snapshot has been observed to carry 30 trades.
    pub const CAPACITY: usize = 256;

    /// Removes trades already delivered and remembers the remaining ones as
    /// delivered, preserving their wire order.
    pub fn retain_undelivered(&mut self, trades: &mut Vec<MarketTrade>) {
        trades.retain(|trade| {
            if self.delivered.contains(trade.trade_id.as_str()) {
                return false;
            }
            let id: Arc<str> = Arc::from(trade.trade_id.as_str());
            self.delivered.insert(Arc::clone(&id));
            self.newest.push_back(id);
            if self.newest.len() > Self::CAPACITY
                && let Some(oldest) = self.newest.pop_front()
            {
                self.delivered.remove(&oldest);
            }
            true
        });
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
    fn replayed_trades_are_delivered_once_regardless_of_tid_spacing() {
        let decode = |sequence| {
            decode_trades_batch(
                &raw(&payload()),
                "BTC",
                "hyperliquid:perp:BTC",
                "hyperliquid:public",
                3,
                1_700_000_000_200_000_000,
                sequence,
            )
            .expect("batch")
            .trades
        };
        let mut delivered = DeliveredTradeIds::default();
        // The two tids differ by ~8e14; as hashes that carries no meaning.
        let mut first = decode(10);
        delivered.retain_undelivered(&mut first);
        assert_eq!(first.len(), 2);
        let mut replay = decode(12);
        delivered.retain_undelivered(&mut replay);
        assert!(replay.is_empty(), "a resubscription snapshot is a replay");
    }

    #[test]
    fn delivered_identities_stay_bounded_to_the_newest_trades() {
        let trade = |time: i64| {
            let row = json!([{"coin": "BTC", "px": "1.0", "sz": "1.0", "side": "B",
                "time": time, "tid": 7, "hash": "0xabc", "users": ["0x1", "0x2"]}]);
            decode_trades_batch(
                &raw(&row),
                "BTC",
                "hyperliquid:perp:BTC",
                "hyperliquid:public",
                1,
                2,
                1,
            )
            .expect("trade")
            .trades
        };
        let capacity = i64::try_from(DeliveredTradeIds::CAPACITY).expect("capacity fits i64");
        let mut delivered = DeliveredTradeIds::default();
        for time in 1..=capacity + 1 {
            delivered.retain_undelivered(&mut trade(time));
        }
        let mut newest = trade(capacity + 1);
        delivered.retain_undelivered(&mut newest);
        assert!(newest.is_empty(), "the newest identity is remembered");
        let mut evicted = trade(1);
        delivered.retain_undelivered(&mut evicted);
        assert_eq!(evicted.len(), 1, "the oldest identity was evicted");
    }

    #[test]
    fn malformed_trades_fail_without_fabrication() {
        let wrong_coin = json!([
            {"coin": "ETH", "px": "1.0", "sz": "1.0", "side": "B", "time": 1,
             "tid": 7, "hash": "0xabc", "users": ["0xbuyer", "0xseller"]},
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
        let bad_side = json!([{
            "coin": "BTC",
            "px": "1.0",
            "sz": "1.0",
            "side": "?",
            "time": 1,
            "tid": 7,
            "hash": "0xabc",
            "users": ["0xbuyer", "0xseller"]
        }]);
        assert!(
            decode_trades_batch(
                &raw(&bad_side),
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
            {"coin": "BTC", "px": "abc", "sz": "1.0", "side": "B", "time": 1,
             "tid": 7, "hash": "0xabc", "users": ["0xbuyer", "0xseller"]},
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
        let missing_required_identity = json!([{
            "coin": "BTC", "px": "1.0", "sz": "1.0", "side": "B", "time": 1,
            "tid": 7, "hash": "0xabc"
        }]);
        assert!(
            decode_trades_batch(
                &raw(&missing_required_identity),
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
