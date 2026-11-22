//! Binance deep L2 order book synchronization (REST snapshot + WS diff-depth)
//! and `bookTicker` top-of-book decoding.

use std::collections::VecDeque;

use aeris_crypto_common::{
    BookLevelUpdate, CryptoError, CryptoResult, LocalOrderBook, NORMALIZED_PRICE_SCALE,
    NORMALIZED_QUANTITY_SCALE, RawDecimal,
};
use aeris_market_data::{
    DepthSnapshot, EventMetadata, QualifiedTimestamp, QuoteLevel, TopOfBookQuote,
};
use serde::Deserialize;

use crate::identity::BinanceMarketKind;

/// Maximum depth levels synchronized per side (Binance `/depth?limit=1000`).
pub const MAXIMUM_BINANCE_BOOK_LEVELS: usize = 1_000;
const MAXIMUM_BUFFERED_DELTAS: usize = 256;
const NANOS_PER_MILLI: i64 = 1_000_000;

/// One decoded Binance `depthUpdate` WebSocket message.
#[derive(Clone, Debug)]
pub struct BinanceDepthUpdate {
    pub symbol: String,
    pub event_time_ms: i64,
    pub first_update_id: u64,
    pub final_update_id: u64,
    /// Present on USDT-M Futures (`pu`); `None` on Spot.
    pub prev_final_update_id: Option<u64>,
    pub bids: Vec<BookLevelUpdate>,
    pub asks: Vec<BookLevelUpdate>,
}

/// One decoded Binance REST `/depth` snapshot.
#[derive(Clone, Debug)]
pub struct BinanceDepthSnapshot {
    pub last_update_id: u64,
    pub bids: Vec<BookLevelUpdate>,
    pub asks: Vec<BookLevelUpdate>,
}

/// Outcome of applying one WS `depthUpdate` to [`BinanceOrderBookSync`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookSyncOutcome {
    /// Book is synced and updated; caller may publish a new `DepthSnapshot`.
    Updated,
    /// Waiting for the initial REST snapshot; delta was buffered.
    BufferedAwaitingSnapshot,
    /// Sequence gap or crossed book detected; caller must fetch a fresh REST snapshot.
    NeedsResync,
}

/// Strict Binance snapshot+delta order book synchronizer.
#[derive(Debug)]
pub struct BinanceOrderBookSync {
    kind: BinanceMarketKind,
    book: LocalOrderBook,
    last_update_id: Option<u64>,
    buffered: VecDeque<BinanceDepthUpdate>,
}

impl BinanceOrderBookSync {
    /// Creates a new unsynced book synchronizer for a Binance market segment.
    #[must_use]
    pub fn new(kind: BinanceMarketKind) -> Self {
        Self {
            kind,
            book: LocalOrderBook::new(MAXIMUM_BINANCE_BOOK_LEVELS),
            last_update_id: None,
            buffered: VecDeque::new(),
        }
    }

    /// Returns true once synchronized with a REST snapshot and live deltas.
    #[must_use]
    pub const fn is_synced(&self) -> bool {
        self.book.is_synced() && self.last_update_id.is_some()
    }

    /// Resets the synchronizer, clearing the local book and buffered deltas.
    pub fn reset(&mut self) {
        self.book.clear();
        self.last_update_id = None;
        self.buffered.clear();
    }

    /// Feeds one live WS `depthUpdate` message.
    pub fn on_ws_delta(&mut self, delta: BinanceDepthUpdate) -> BookSyncOutcome {
        let Some(prev_u) = self.last_update_id else {
            if self.buffered.len() >= MAXIMUM_BUFFERED_DELTAS {
                self.buffered.pop_front();
            }
            self.buffered.push_back(delta);
            return BookSyncOutcome::BufferedAwaitingSnapshot;
        };

        if delta.final_update_id <= prev_u {
            return BookSyncOutcome::Updated;
        }

        let contiguous = match self.kind {
            BinanceMarketKind::UsdmPerp => {
                delta.prev_final_update_id.is_some_and(|pu| pu == prev_u)
            }
            BinanceMarketKind::Spot => delta.first_update_id <= prev_u.saturating_add(1),
        };
        if !contiguous {
            self.reset();
            self.buffered.push_back(delta);
            return BookSyncOutcome::NeedsResync;
        }

        if self.book.apply_delta(&delta.bids, &delta.asks).is_err() {
            self.reset();
            return BookSyncOutcome::NeedsResync;
        }
        self.last_update_id = Some(delta.final_update_id);
        BookSyncOutcome::Updated
    }

    /// Applies a REST `/depth` snapshot and replays any covering buffered deltas.
    ///
    /// # Errors
    /// Returns an error if the snapshot itself is invalid or crossed.
    pub fn apply_rest_snapshot(&mut self, snapshot: &BinanceDepthSnapshot) -> CryptoResult<()> {
        self.book
            .apply_snapshot(&snapshot.bids, &snapshot.asks)
            .map_err(|error| CryptoError::OrderBook(error.to_string()))?;
        let mut cursor = snapshot.last_update_id;
        let mut first_applied = false;

        while let Some(delta) = self.buffered.pop_front() {
            if delta.final_update_id <= cursor {
                continue;
            }
            if !first_applied {
                let bridges = match self.kind {
                    BinanceMarketKind::UsdmPerp => {
                        delta.first_update_id <= cursor && delta.final_update_id >= cursor
                    }
                    BinanceMarketKind::Spot => {
                        let next = cursor.saturating_add(1);
                        delta.first_update_id <= next && delta.final_update_id >= next
                    }
                };
                if !bridges {
                    // Snapshot is older than buffered deltas; keep the snapshot as baseline
                    // only if no newer deltas exist, otherwise resync on next delta.
                    self.buffered.clear();
                    break;
                }
                first_applied = true;
            }
            if self.book.apply_delta(&delta.bids, &delta.asks).is_err() {
                self.reset();
                return Err(CryptoError::OrderBook(
                    "binance buffered delta crossed the order book".to_string(),
                ));
            }
            cursor = delta.final_update_id;
        }

        self.last_update_id = Some(cursor);
        Ok(())
    }

    /// Borrows the underlying [`LocalOrderBook`] (used by the aggregated crypto feed).
    #[must_use]
    pub const fn local_book(&self) -> &LocalOrderBook {
        &self.book
    }

    /// Projects the synced book into a canonical [`DepthSnapshot`].
    ///
    /// # Errors
    /// Returns an error if validation fails.
    pub fn to_depth_snapshot(
        &self,
        instrument_id: &str,
        entitlement_id: &str,
        session_generation: u64,
        source_sequence: u64,
        exchange_time_ms: i64,
        received_unix_nanos: i64,
    ) -> CryptoResult<DepthSnapshot> {
        let exchange_nanos = exchange_time_ms
            .checked_mul(NANOS_PER_MILLI)
            .ok_or_else(|| {
                CryptoError::OrderBook("binance book timestamp overflowed".to_string())
            })?;
        let metadata = EventMetadata {
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
        };
        self.book
            .to_depth_snapshot(metadata, MAXIMUM_BINANCE_BOOK_LEVELS)
    }

    /// Projects the synced book's top level into a [`TopOfBookQuote`].
    #[must_use]
    pub fn to_top_of_book(
        &self,
        instrument_id: &str,
        entitlement_id: &str,
        session_generation: u64,
        source_sequence: u64,
        exchange_time_ms: i64,
        received_unix_nanos: i64,
    ) -> Option<TopOfBookQuote> {
        let exchange_nanos = exchange_time_ms.checked_mul(NANOS_PER_MILLI)?;
        let metadata = EventMetadata {
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
        };
        self.book.to_top_of_book(metadata)
    }
}

#[derive(Deserialize)]
struct WireDepthSnapshot {
    #[serde(rename = "lastUpdateId")]
    last_update_id: u64,
    bids: Vec<[RawDecimal; 2]>,
    asks: Vec<[RawDecimal; 2]>,
}

#[derive(Deserialize)]
struct WireDepthUpdate {
    #[serde(rename = "E")]
    event_time_ms: i64,
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "U")]
    first_update_id: u64,
    #[serde(rename = "u")]
    final_update_id: u64,
    #[serde(rename = "pu", default)]
    prev_final_update_id: Option<u64>,
    #[serde(rename = "b")]
    bids: Vec<[RawDecimal; 2]>,
    #[serde(rename = "a")]
    asks: Vec<[RawDecimal; 2]>,
}

#[derive(Deserialize)]
struct WireBookTicker {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "b")]
    bid_price: RawDecimal,
    #[serde(rename = "B")]
    bid_qty: RawDecimal,
    #[serde(rename = "a")]
    ask_price: RawDecimal,
    #[serde(rename = "A")]
    ask_qty: RawDecimal,
    #[serde(rename = "T", default)]
    transaction_time_ms: Option<i64>,
    #[serde(rename = "E", default)]
    event_time_ms: Option<i64>,
}

fn decode_levels(raw: &[[RawDecimal; 2]]) -> CryptoResult<Vec<BookLevelUpdate>> {
    let mut out = Vec::with_capacity(raw.len());
    for pair in raw {
        let price = pair[0].to_fixed(NORMALIZED_PRICE_SCALE)?;
        let quantity = pair[1].to_fixed(NORMALIZED_QUANTITY_SCALE)?;
        out.push(BookLevelUpdate {
            price,
            quantity,
            order_count: None,
        });
    }
    Ok(out)
}

/// Decodes a REST `/depth` snapshot JSON response.
///
/// # Errors
/// Returns an error if the JSON or any price/quantity is malformed.
pub fn decode_depth_snapshot(raw: &[u8]) -> CryptoResult<BinanceDepthSnapshot> {
    let wire: WireDepthSnapshot = serde_json::from_slice(raw).map_err(|_| {
        CryptoError::MalformedPayload("binance depth snapshot payload is malformed".to_string())
    })?;
    Ok(BinanceDepthSnapshot {
        last_update_id: wire.last_update_id,
        bids: decode_levels(&wire.bids)?,
        asks: decode_levels(&wire.asks)?,
    })
}

/// Decodes a WebSocket `depthUpdate` payload.
///
/// # Errors
/// Returns an error if the JSON or any price/quantity is malformed.
pub fn decode_depth_update(
    payload: &serde_json::value::RawValue,
) -> CryptoResult<BinanceDepthUpdate> {
    let wire: WireDepthUpdate = serde_json::from_str(payload.get()).map_err(|_| {
        CryptoError::MalformedPayload("binance depthUpdate payload is malformed".to_string())
    })?;
    Ok(BinanceDepthUpdate {
        symbol: wire.symbol.to_ascii_uppercase(),
        event_time_ms: wire.event_time_ms,
        first_update_id: wire.first_update_id,
        final_update_id: wire.final_update_id,
        prev_final_update_id: wire.prev_final_update_id,
        bids: decode_levels(&wire.bids)?,
        asks: decode_levels(&wire.asks)?,
    })
}

/// Decodes a WebSocket `bookTicker` payload into `(symbol, TopOfBookQuote)`.
///
/// # Errors
/// Returns an error if the JSON or quote values are invalid.
pub fn decode_book_ticker(
    payload: &serde_json::value::RawValue,
    instrument_id: &str,
    entitlement_id: &str,
    session_generation: u64,
    source_sequence: u64,
    received_unix_nanos: i64,
) -> CryptoResult<(String, TopOfBookQuote)> {
    let wire: WireBookTicker = serde_json::from_str(payload.get()).map_err(|_| {
        CryptoError::MalformedPayload("binance bookTicker payload is malformed".to_string())
    })?;
    let bid_price = wire.bid_price.to_fixed(NORMALIZED_PRICE_SCALE)?;
    let bid_qty = wire.bid_qty.to_fixed(NORMALIZED_QUANTITY_SCALE)?;
    let ask_price = wire.ask_price.to_fixed(NORMALIZED_PRICE_SCALE)?;
    let ask_qty = wire.ask_qty.to_fixed(NORMALIZED_QUANTITY_SCALE)?;
    let exchange_nanos = wire
        .transaction_time_ms
        .or(wire.event_time_ms)
        .and_then(|ms| ms.checked_mul(NANOS_PER_MILLI));
    let quote = TopOfBookQuote {
        metadata: EventMetadata {
            provider_id: "binance".to_string(),
            instrument_id: instrument_id.to_string(),
            entitlement_id: entitlement_id.to_string(),
            source_sequence,
            session_generation,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: exchange_nanos,
                provider_unix_nanos: exchange_nanos,
                received_unix_nanos,
            },
        },
        bid: (bid_price > 0 && bid_qty > 0).then_some(QuoteLevel {
            price: bid_price,
            quantity: Some(bid_qty),
            order_count: None,
        }),
        ask: (ask_price > 0 && ask_qty > 0).then_some(QuoteLevel {
            price: ask_price,
            quantity: Some(ask_qty),
            order_count: None,
        }),
    };
    quote
        .validate()
        .map_err(|error| CryptoError::MalformedPayload(error.to_string()))?;
    Ok((wire.symbol.to_ascii_uppercase(), quote))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn futures_sync_buffers_deltas_and_detects_pu_gap() {
        let mut sync = BinanceOrderBookSync::new(BinanceMarketKind::UsdmPerp);
        let d1 = BinanceDepthUpdate {
            symbol: "BTCUSDT".to_string(),
            event_time_ms: 1_700_000_000_000,
            first_update_id: 100,
            final_update_id: 105,
            prev_final_update_id: Some(99),
            bids: vec![BookLevelUpdate {
                price: 5_000_000_000_000,
                quantity: 200_000_000,
                order_count: None,
            }],
            asks: vec![],
        };
        assert_eq!(
            sync.on_ws_delta(d1),
            BookSyncOutcome::BufferedAwaitingSnapshot
        );

        let snap = BinanceDepthSnapshot {
            last_update_id: 102,
            bids: vec![BookLevelUpdate {
                price: 5_000_000_000_000,
                quantity: 100_000_000,
                order_count: None,
            }],
            asks: vec![BookLevelUpdate {
                price: 5_001_000_000_000,
                quantity: 100_000_000,
                order_count: None,
            }],
        };
        sync.apply_rest_snapshot(&snap).unwrap();
        assert!(sync.is_synced());
        assert_eq!(
            sync.local_book().best_bid(),
            Some((5_000_000_000_000, 200_000_000))
        );

        // Next delta with gap in pu triggers NeedsResync
        let gap = BinanceDepthUpdate {
            symbol: "BTCUSDT".to_string(),
            event_time_ms: 1_700_000_000_100,
            first_update_id: 108,
            final_update_id: 110,
            prev_final_update_id: Some(107),
            bids: vec![],
            asks: vec![],
        };
        assert_eq!(sync.on_ws_delta(gap), BookSyncOutcome::NeedsResync);
        assert!(!sync.is_synced());
    }
}
