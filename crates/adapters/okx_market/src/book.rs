//! OKX V5 `books` (400-level) snapshot+delta synchronization with strict
//! `seqId`/`prevSeqId` continuity checking and `ctVal` base-unit scaling.

use aeris_crypto_common::{
    BookLevelUpdate, CryptoError, CryptoResult, LocalOrderBook, NORMALIZED_PRICE_SCALE,
    NORMALIZED_QUANTITY_SCALE, RawDecimal, multiply_fixed_decimal,
};
use aeris_market_data::{DepthSnapshot, EventMetadata, QualifiedTimestamp, TopOfBookQuote};
use serde::Deserialize;

/// Maximum depth levels per side on OKX `books` channel.
pub const MAXIMUM_OKX_BOOK_LEVELS: usize = 400;
const NANOS_PER_MILLI: i64 = 1_000_000;
const ONE_FIXED: i64 = 100_000_000;

/// Decoded OKX V5 `books` snapshot or incremental update.
#[derive(Clone, Debug)]
pub struct OkxBookMessage {
    pub is_snapshot: bool,
    pub seq_id: i64,
    pub prev_seq_id: i64,
    pub ts_ms: i64,
    pub bids: Vec<BookLevelUpdate>,
    pub asks: Vec<BookLevelUpdate>,
}

/// Result of applying one OKX book message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OkxBookApplyOutcome {
    /// Book is synced and changed; caller may publish.
    Updated,
    /// No-op heartbeat (`prevSeqId == seqId`) or waiting for initial snapshot.
    NoChange,
    /// Sequence gap or crossed book; caller must resubscribe for a fresh snapshot.
    NeedsResync,
}

/// Maintains one local OKX L2 order book synchronized from `books` snapshots and deltas.
#[derive(Debug)]
pub struct OkxBookState {
    book: LocalOrderBook,
    last_seq_id: Option<i64>,
}

impl Default for OkxBookState {
    fn default() -> Self {
        Self::new()
    }
}

impl OkxBookState {
    /// Creates an empty unsynced book bounded to 400 levels per side.
    #[must_use]
    pub fn new() -> Self {
        Self {
            book: LocalOrderBook::new(MAXIMUM_OKX_BOOK_LEVELS),
            last_seq_id: None,
        }
    }

    /// Returns true once synchronized with a snapshot.
    #[must_use]
    pub const fn is_synced(&self) -> bool {
        self.book.is_synced() && self.last_seq_id.is_some()
    }

    /// Clears the book and sequence cursor.
    pub fn reset(&mut self) {
        self.book.clear();
        self.last_seq_id = None;
    }

    /// Borrows the underlying [`LocalOrderBook`].
    #[must_use]
    pub const fn local_book(&self) -> &LocalOrderBook {
        &self.book
    }

    /// Applies one snapshot or delta message, checking `prevSeqId == last_seq_id`.
    pub fn apply(&mut self, msg: &OkxBookMessage) -> OkxBookApplyOutcome {
        if msg.is_snapshot {
            if self.book.apply_snapshot(&msg.bids, &msg.asks).is_err() {
                self.reset();
                return OkxBookApplyOutcome::NeedsResync;
            }
            self.last_seq_id = Some(msg.seq_id);
            return OkxBookApplyOutcome::Updated;
        }
        let Some(last) = self.last_seq_id else {
            return OkxBookApplyOutcome::NoChange;
        };
        if msg.prev_seq_id == msg.seq_id && msg.seq_id == last {
            return OkxBookApplyOutcome::NoChange;
        }
        if msg.prev_seq_id != last {
            self.reset();
            return OkxBookApplyOutcome::NeedsResync;
        }
        if self.book.apply_delta(&msg.bids, &msg.asks).is_err() {
            self.reset();
            return OkxBookApplyOutcome::NeedsResync;
        }
        self.last_seq_id = Some(msg.seq_id);
        OkxBookApplyOutcome::Updated
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
        ts_ms: i64,
        received_unix_nanos: i64,
    ) -> CryptoResult<DepthSnapshot> {
        let exchange_nanos = ts_ms
            .checked_mul(NANOS_PER_MILLI)
            .ok_or_else(|| CryptoError::OrderBook("okx book timestamp overflowed".to_string()))?;
        let metadata = EventMetadata {
            provider_id: "okx".to_string(),
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
            .to_depth_snapshot(metadata, MAXIMUM_OKX_BOOK_LEVELS)
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
            provider_id: "okx".to_string(),
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
#[serde(rename_all = "camelCase")]
struct WireBookEntry {
    bids: Vec<[RawDecimal; 4]>,
    asks: Vec<[RawDecimal; 4]>,
    ts: RawDecimal,
    seq_id: i64,
    prev_seq_id: i64,
}

fn decode_levels(raw: &[[RawDecimal; 4]], ct_val_fixed: i64) -> CryptoResult<Vec<BookLevelUpdate>> {
    let mut out = Vec::with_capacity(raw.len());
    for entry in raw {
        let price = entry[0].to_fixed(NORMALIZED_PRICE_SCALE)?;
        let raw_qty = entry[1].to_fixed(NORMALIZED_QUANTITY_SCALE)?;
        let quantity = if raw_qty == 0 || ct_val_fixed == ONE_FIXED {
            raw_qty
        } else {
            multiply_fixed_decimal(raw_qty, ct_val_fixed, NORMALIZED_QUANTITY_SCALE)?
        };
        let order_count = entry[3].as_str().parse::<u32>().ok().filter(|&n| n > 0);
        out.push(BookLevelUpdate {
            price,
            quantity,
            order_count,
        });
    }
    Ok(out)
}

/// Decodes an OKX `books` `data` array into [`OkxBookMessage`] items.
///
/// # Errors
/// Returns an error if the JSON or level numbers are malformed.
pub fn decode_books(
    data: &serde_json::value::RawValue,
    action: &str,
    ct_val_fixed: i64,
) -> CryptoResult<Vec<OkxBookMessage>> {
    let entries: Vec<WireBookEntry> = serde_json::from_str(data.get())
        .map_err(|_| CryptoError::MalformedPayload("okx books payload is malformed".to_string()))?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let ts_ms: i64 = entry.ts.as_str().parse().map_err(|_| {
            CryptoError::MalformedPayload("okx books timestamp is malformed".to_string())
        })?;
        out.push(OkxBookMessage {
            is_snapshot: action == "snapshot",
            seq_id: entry.seq_id,
            prev_seq_id: entry.prev_seq_id,
            ts_ms,
            bids: decode_levels(&entry.bids, ct_val_fixed)?,
            asks: decode_levels(&entry.asks, ct_val_fixed)?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn okx_books_scales_swap_ct_val_and_detects_seq_gap() {
        // ctVal = 0.01 BTC per contract -> 1_000_000 at scale 8
        let ct_val = 1_000_000_i64;
        let snap_raw = serde_json::value::RawValue::from_string(
            r#"[{"bids":[["65000.0","150","0","4"]],"asks":[["65001.0","200","0","2"]],"ts":"1700000000000","seqId":500,"prevSeqId":-1}]"#
                .to_string(),
        )
        .unwrap();
        let msgs = decode_books(&snap_raw, "snapshot", ct_val).unwrap();
        let mut state = OkxBookState::new();
        assert_eq!(state.apply(&msgs[0]), OkxBookApplyOutcome::Updated);
        // 150 contracts * 0.01 BTC = 1.5 BTC = 150_000_000 at scale 8
        assert_eq!(
            state.local_book().best_bid(),
            Some((6_500_000_000_000, 150_000_000))
        );

        // Sequence gap (prevSeqId != 500) forces resync
        let gap_raw = serde_json::value::RawValue::from_string(
            r#"[{"bids":[],"asks":[],"ts":"1700000000100","seqId":505,"prevSeqId":502}]"#
                .to_string(),
        )
        .unwrap();
        let gap = decode_books(&gap_raw, "update", ct_val).unwrap();
        assert_eq!(state.apply(&gap[0]), OkxBookApplyOutcome::NeedsResync);
        assert!(!state.is_synced());
    }
}
