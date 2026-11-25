//! Coinbase Advanced Trade `level2` (`l2_data`) snapshot and incremental order book state.

use aeris_crypto_common::{
    BookLevelUpdate, CryptoError, CryptoResult, LocalOrderBook, NORMALIZED_PRICE_SCALE,
    NORMALIZED_QUANTITY_SCALE, RawDecimal,
};
use aeris_market_data::{DepthSnapshot, EventMetadata, QualifiedTimestamp, TopOfBookQuote};
use serde::Deserialize;

use crate::trades::parse_rfc3339_nanos;

/// Maximum depth levels per side retained for one Coinbase L2 order book.
pub const MAXIMUM_COINBASE_BOOK_LEVELS: usize = 1_000;

/// Decoded Coinbase `l2_data` snapshot or incremental update for one product.
#[derive(Clone, Debug)]
pub struct CoinbaseBookMessage {
    pub product_id: String,
    pub is_snapshot: bool,
    pub ts_nanos: i64,
    pub bids: Vec<BookLevelUpdate>,
    pub asks: Vec<BookLevelUpdate>,
}

/// Result of applying one Coinbase book message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoinbaseBookApplyOutcome {
    /// Book is synced and updated; caller may publish.
    Updated,
    /// Waiting for initial snapshot or empty update.
    NoChange,
    /// Crossed or invalid book state; caller must resubscribe for a fresh snapshot.
    NeedsResync,
}

/// Maintains one local Coinbase L2 order book synchronized from `l2_data` snapshots and deltas.
#[derive(Debug)]
pub struct CoinbaseBookState {
    book: LocalOrderBook,
}

impl Default for CoinbaseBookState {
    fn default() -> Self {
        Self::new()
    }
}

impl CoinbaseBookState {
    /// Creates an empty unsynced book bounded to 1,000 levels per side.
    #[must_use]
    pub fn new() -> Self {
        Self {
            book: LocalOrderBook::new(MAXIMUM_COINBASE_BOOK_LEVELS),
        }
    }

    /// Returns true once synchronized with a snapshot.
    #[must_use]
    pub const fn is_synced(&self) -> bool {
        self.book.is_synced()
    }

    /// Clears the book back to the unsynced state.
    pub fn reset(&mut self) {
        self.book.clear();
    }

    /// Borrows the underlying [`LocalOrderBook`].
    #[must_use]
    pub const fn local_book(&self) -> &LocalOrderBook {
        &self.book
    }

    /// Applies one `l2_data` snapshot or delta update.
    pub fn apply(&mut self, msg: &CoinbaseBookMessage) -> CoinbaseBookApplyOutcome {
        if msg.is_snapshot {
            if self.book.apply_snapshot(&msg.bids, &msg.asks).is_err() {
                self.reset();
                return CoinbaseBookApplyOutcome::NeedsResync;
            }
            return CoinbaseBookApplyOutcome::Updated;
        }
        if !self.book.is_synced() {
            return CoinbaseBookApplyOutcome::NoChange;
        }
        if msg.bids.is_empty() && msg.asks.is_empty() {
            return CoinbaseBookApplyOutcome::NoChange;
        }
        if self.book.apply_delta(&msg.bids, &msg.asks).is_err() {
            self.reset();
            return CoinbaseBookApplyOutcome::NeedsResync;
        }
        CoinbaseBookApplyOutcome::Updated
    }

    /// Projects the book into a canonical [`DepthSnapshot`].
    ///
    /// # Errors
    /// Returns an error if domain validation fails.
    pub fn to_depth_snapshot(
        &self,
        instrument_id: &str,
        entitlement_id: &str,
        session_generation: u64,
        source_sequence: u64,
        exchange_nanos: i64,
        received_unix_nanos: i64,
    ) -> CryptoResult<DepthSnapshot> {
        let metadata = EventMetadata {
            provider_id: "coinbase".to_string(),
            instrument_id: instrument_id.to_string(),
            entitlement_id: entitlement_id.to_string(),
            source_sequence,
            session_generation,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(exchange_nanos),
                provider_unix_nanos: Some(exchange_nanos),
                received_unix_nanos,
            },
        };
        self.book
            .to_depth_snapshot(metadata, MAXIMUM_COINBASE_BOOK_LEVELS)
    }

    /// Projects the top of book into a [`TopOfBookQuote`].
    #[must_use]
    pub fn to_top_of_book(
        &self,
        instrument_id: &str,
        entitlement_id: &str,
        session_generation: u64,
        source_sequence: u64,
        exchange_nanos: i64,
        received_unix_nanos: i64,
    ) -> Option<TopOfBookQuote> {
        let metadata = EventMetadata {
            provider_id: "coinbase".to_string(),
            instrument_id: instrument_id.to_string(),
            entitlement_id: entitlement_id.to_string(),
            source_sequence,
            session_generation,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(exchange_nanos),
                provider_unix_nanos: Some(exchange_nanos),
                received_unix_nanos,
            },
        };
        self.book.to_top_of_book(metadata)
    }
}

#[derive(Deserialize)]
struct WireL2Event {
    #[serde(rename = "type")]
    event_type: String,
    product_id: String,
    #[serde(default)]
    updates: Vec<WireL2Update>,
}

#[derive(Deserialize)]
struct WireL2Update {
    side: String,
    price_level: RawDecimal,
    new_quantity: RawDecimal,
    #[serde(default)]
    event_time: Option<String>,
}

/// Decodes a Coinbase `l2_data` `events` array into [`CoinbaseBookMessage`] items.
///
/// # Errors
/// Returns an error if the JSON or level numbers are malformed.
pub fn decode_l2_events(
    events_raw: &serde_json::value::RawValue,
    frame_ts_nanos: i64,
) -> CryptoResult<Vec<CoinbaseBookMessage>> {
    let events: Vec<WireL2Event> = serde_json::from_str(events_raw.get()).map_err(|_| {
        CryptoError::MalformedPayload("coinbase l2_data payload is malformed".to_string())
    })?;
    let mut out = Vec::with_capacity(events.len());
    for event in events {
        let is_snapshot = event.event_type.eq_ignore_ascii_case("snapshot");
        let mut bids = Vec::new();
        let mut asks = Vec::new();
        let mut ts_nanos = frame_ts_nanos;
        for upd in event.updates {
            let price = upd.price_level.to_fixed(NORMALIZED_PRICE_SCALE)?;
            let quantity = upd.new_quantity.to_fixed(NORMALIZED_QUANTITY_SCALE)?;
            if let Some(ref time) = upd.event_time
                && let Ok(parsed) = parse_rfc3339_nanos(time)
            {
                ts_nanos = ts_nanos.max(parsed);
            }
            let level = BookLevelUpdate {
                price,
                quantity,
                order_count: None,
            };
            if upd.side.eq_ignore_ascii_case("bid") {
                bids.push(level);
            } else if upd.side.eq_ignore_ascii_case("offer") || upd.side.eq_ignore_ascii_case("ask")
            {
                asks.push(level);
            }
        }
        out.push(CoinbaseBookMessage {
            product_id: event.product_id.to_ascii_uppercase(),
            is_snapshot,
            ts_nanos,
            bids,
            asks,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_snapshot_updates_and_removes_depleted_levels() {
        let snap_raw = serde_json::value::RawValue::from_string(
            r#"[{"type":"snapshot","product_id":"BTC-USD","updates":[
                {"side":"bid","price_level":"65000.00","new_quantity":"1.5"},
                {"side":"offer","price_level":"65001.00","new_quantity":"2.0"},
                {"side":"offer","price_level":"65002.00","new_quantity":"3.0"}
            ]}]"#
                .to_string(),
        )
        .unwrap();
        let msgs = decode_l2_events(&snap_raw, 1_700_000_000_000_000_000).expect("decode snap");
        let mut state = CoinbaseBookState::new();
        assert_eq!(state.apply(&msgs[0]), CoinbaseBookApplyOutcome::Updated);
        assert_eq!(
            state.local_book().best_bid(),
            Some((6_500_000_000_000, 150_000_000))
        );
        assert_eq!(
            state.local_book().best_ask(),
            Some((6_500_100_000_000, 200_000_000))
        );

        let upd_raw = serde_json::value::RawValue::from_string(
            r#"[{"type":"update","product_id":"BTC-USD","updates":[
                {"side":"offer","price_level":"65001.00","new_quantity":"0"}
            ]}]"#
                .to_string(),
        )
        .unwrap();
        let upd = decode_l2_events(&upd_raw, 1_700_000_001_000_000_000).expect("decode upd");
        assert_eq!(state.apply(&upd[0]), CoinbaseBookApplyOutcome::Updated);
        assert_eq!(
            state.local_book().best_ask(),
            Some((6_500_200_000_000, 300_000_000))
        );
    }
}
