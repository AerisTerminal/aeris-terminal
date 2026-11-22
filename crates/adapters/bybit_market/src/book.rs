//! Bybit V5 `orderbook.200` snapshot+delta synchronization.

use aeris_crypto_common::{
    BookLevelUpdate, CryptoError, CryptoResult, LocalOrderBook, NORMALIZED_PRICE_SCALE,
    NORMALIZED_QUANTITY_SCALE, RawDecimal,
};
use aeris_market_data::{DepthSnapshot, EventMetadata, QualifiedTimestamp, TopOfBookQuote};
use serde::Deserialize;

/// Maximum depth levels subscribed per side (`orderbook.500` on Linear, `orderbook.200` on Spot).
pub const MAXIMUM_BYBIT_BOOK_LEVELS: usize = 500;
const NANOS_PER_MILLI: i64 = 1_000_000;

/// Decoded Bybit V5 orderbook frame (`snapshot` or `delta`).
#[derive(Clone, Debug)]
pub struct BybitBookMessage {
    pub symbol: String,
    pub is_snapshot: bool,
    pub update_id: u64,
    pub ts_ms: i64,
    pub bids: Vec<BookLevelUpdate>,
    pub asks: Vec<BookLevelUpdate>,
}

/// Maintains one local Bybit L2 order book synchronized from WS snapshots and deltas.
#[derive(Debug)]
pub struct BybitBookState {
    book: LocalOrderBook,
}

impl Default for BybitBookState {
    fn default() -> Self {
        Self::new()
    }
}

impl BybitBookState {
    /// Creates an empty unsynced book bounded to 200 levels per side.
    #[must_use]
    pub fn new() -> Self {
        Self {
            book: LocalOrderBook::new(MAXIMUM_BYBIT_BOOK_LEVELS),
        }
    }

    /// Returns true once a covering snapshot has been applied.
    #[must_use]
    pub const fn is_synced(&self) -> bool {
        self.book.is_synced()
    }

    /// Clears the book.
    pub fn reset(&mut self) {
        self.book.clear();
    }

    /// Borrows the underlying [`LocalOrderBook`].
    #[must_use]
    pub const fn local_book(&self) -> &LocalOrderBook {
        &self.book
    }

    /// Applies a Bybit snapshot or delta message. Returns `Ok(true)` if the book is synced.
    ///
    /// # Errors
    /// Returns an error if the update crosses the book (caller should resubscribe).
    pub fn apply(&mut self, msg: &BybitBookMessage) -> CryptoResult<bool> {
        if msg.is_snapshot || msg.update_id == 1 {
            self.book
                .apply_snapshot(&msg.bids, &msg.asks)
                .map_err(|error| CryptoError::OrderBook(error.to_string()))?;
            return Ok(true);
        }
        if !self.book.is_synced() {
            return Ok(false);
        }
        self.book
            .apply_delta(&msg.bids, &msg.asks)
            .map_err(|error| CryptoError::OrderBook(error.to_string()))?;
        Ok(true)
    }

    /// Projects the book into a canonical [`DepthSnapshot`].
    ///
    /// # Errors
    /// Returns an error if validation fails.
    pub fn to_depth_snapshot(
        &self,
        instrument_id: &str,
        entitlement_id: &str,
        session_generation: u64,
        source_sequence: u64,
        ts_ms: i64,
        received_unix_nanos: i64,
    ) -> CryptoResult<DepthSnapshot> {
        let exchange_nanos = ts_ms
            .checked_mul(NANOS_PER_MILLI)
            .ok_or_else(|| CryptoError::OrderBook("bybit book timestamp overflowed".to_string()))?;
        let metadata = EventMetadata {
            provider_id: "bybit".to_string(),
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
            .to_depth_snapshot(metadata, MAXIMUM_BYBIT_BOOK_LEVELS)
    }

    /// Projects the top of book into a [`TopOfBookQuote`].
    #[must_use]
    pub fn to_top_of_book(
        &self,
        instrument_id: &str,
        entitlement_id: &str,
        session_generation: u64,
        source_sequence: u64,
        ts_ms: i64,
        received_unix_nanos: i64,
    ) -> Option<TopOfBookQuote> {
        let exchange_nanos = ts_ms.checked_mul(NANOS_PER_MILLI)?;
        let metadata = EventMetadata {
            provider_id: "bybit".to_string(),
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
struct WireOrderbookData {
    s: String,
    b: Vec<[RawDecimal; 2]>,
    a: Vec<[RawDecimal; 2]>,
    u: u64,
}

fn decode_levels(raw: &[[RawDecimal; 2]]) -> CryptoResult<Vec<BookLevelUpdate>> {
    let mut out = Vec::with_capacity(raw.len());
    for pair in raw {
        out.push(BookLevelUpdate {
            price: pair[0].to_fixed(NORMALIZED_PRICE_SCALE)?,
            quantity: pair[1].to_fixed(NORMALIZED_QUANTITY_SCALE)?,
            order_count: None,
        });
    }
    Ok(out)
}

/// Decodes a Bybit V5 `orderbook.*` `data` object.
///
/// # Errors
/// Returns an error if the JSON or level numbers are malformed.
pub fn decode_orderbook(
    data: &serde_json::value::RawValue,
    msg_type: &str,
    ts_ms: i64,
) -> CryptoResult<BybitBookMessage> {
    let wire: WireOrderbookData = serde_json::from_str(data.get()).map_err(|_| {
        CryptoError::MalformedPayload("bybit orderbook payload is malformed".to_string())
    })?;
    Ok(BybitBookMessage {
        symbol: wire.s.to_ascii_uppercase(),
        is_snapshot: msg_type == "snapshot",
        update_id: wire.u,
        ts_ms,
        bids: decode_levels(&wire.b)?,
        asks: decode_levels(&wire.a)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_and_applies_bybit_snapshot_and_delta() {
        let snap_raw = serde_json::value::RawValue::from_string(
            r#"{"s":"BTCUSDT","b":[["65000.5","2.5"],["65000.0","1.0"]],"a":[["65001.0","3.0"]],"u":100}"#
                .to_string(),
        )
        .unwrap();
        let snap = decode_orderbook(&snap_raw, "snapshot", 1_700_000_000_000).unwrap();
        let mut state = BybitBookState::new();
        assert!(state.apply(&snap).unwrap());
        assert_eq!(
            state.local_book().best_bid(),
            Some((6_500_050_000_000, 250_000_000))
        );

        let delta_raw = serde_json::value::RawValue::from_string(
            r#"{"s":"BTCUSDT","b":[["65000.5","0"]],"a":[["65001.0","4.0"]],"u":101}"#.to_string(),
        )
        .unwrap();
        let delta = decode_orderbook(&delta_raw, "delta", 1_700_000_000_100).unwrap();
        assert!(state.apply(&delta).unwrap());
        assert_eq!(
            state.local_book().best_bid(),
            Some((6_500_000_000_000, 100_000_000))
        );
        assert_eq!(
            state.local_book().best_ask(),
            Some((6_500_100_000_000, 400_000_000))
        );
    }
}
