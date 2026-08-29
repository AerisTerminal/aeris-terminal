use crate::{CoinbaseError, FixedPointValue, coinbase_instrument_id, mantissa_at_scale};
use axiusflow_market_data::{
    BookSide, DepthDelta, DepthLevel, DepthSnapshot, EventMetadata, QualifiedTimestamp,
};
use serde::Deserialize;
use std::{collections::BTreeMap, num::NonZeroUsize};

pub const COINBASE_MAXIMUM_LEVELS_PER_SIDE: usize = 256;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CoinbaseLevel2Diagnostics {
    pub snapshots: u64,
    pub deltas: u64,
    pub deletes: u64,
    pub sequence_gaps: u64,
    pub ignored_while_recovering: u64,
    pub levels_trimmed: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoinbaseLevel2Outcome {
    Snapshot(DepthSnapshot),
    Deltas {
        deltas: Vec<DepthDelta>,
        book: DepthSnapshot,
    },
    RecoveryRequired,
    Ignored,
}

#[derive(Deserialize)]
struct Level2Message {
    channel: String,
    timestamp: String,
    sequence_num: u64,
    #[serde(default)]
    events: Vec<Level2Event>,
}

#[derive(Deserialize)]
struct Level2Event {
    #[serde(rename = "type")]
    event_type: String,
    product_id: String,
    #[serde(default)]
    updates: Vec<Level2Update>,
}

#[derive(Deserialize)]
struct Level2Update {
    side: String,
    event_time: String,
    price_level: String,
    new_quantity: String,
}

pub struct CoinbaseLevel2Book {
    product_id: String,
    instrument_id: String,
    price_scale: u8,
    quantity_scale: u8,
    session_generation: u64,
    next_provider_sequence: Option<u64>,
    next_canonical_sequence: u64,
    bids: BTreeMap<i64, i64>,
    asks: BTreeMap<i64, i64>,
    ready: bool,
    diagnostics: CoinbaseLevel2Diagnostics,
}

impl CoinbaseLevel2Book {
    /// Creates one generation-fenced bounded Coinbase Level 2 book.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid product identity, precision, or generation.
    pub fn try_new(
        product_id: impl Into<String>,
        price_scale: u8,
        quantity_scale: u8,
        session_generation: u64,
    ) -> Result<Self, CoinbaseError> {
        let product_id = product_id.into();
        if session_generation == 0 || price_scale > 18 || quantity_scale > 18 {
            return Err(CoinbaseError::InvalidConfiguration);
        }
        Ok(Self {
            instrument_id: coinbase_instrument_id(&product_id)?,
            product_id,
            price_scale,
            quantity_scale,
            session_generation,
            next_provider_sequence: None,
            next_canonical_sequence: 1,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            ready: false,
            diagnostics: CoinbaseLevel2Diagnostics::default(),
        })
    }

    #[must_use]
    pub const fn diagnostics(&self) -> CoinbaseLevel2Diagnostics {
        self.diagnostics
    }

    /// Clears retained depth and binds the book to a newer session generation.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero session generation.
    pub fn reset(&mut self, session_generation: u64) -> Result<(), CoinbaseError> {
        if session_generation == 0 {
            return Err(CoinbaseError::InvalidConfiguration);
        }
        self.session_generation = session_generation;
        self.next_provider_sequence = None;
        self.next_canonical_sequence = 1;
        self.bids.clear();
        self.asks.clear();
        self.ready = false;
        Ok(())
    }

    /// Applies one provider Level 2 message or fails closed into snapshot recovery.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed messages, invalid precision, or timestamp failures.
    pub fn apply_message(
        &mut self,
        bytes: &[u8],
        received_unix_nanos: i64,
    ) -> Result<CoinbaseLevel2Outcome, CoinbaseError> {
        let message: Level2Message =
            serde_json::from_slice(bytes).map_err(|_| CoinbaseError::InvalidMessage)?;
        if message.channel != "l2_data" {
            return Ok(CoinbaseLevel2Outcome::Ignored);
        }
        // Coinbase numbers every message on the connection, not per channel, so
        // heartbeats and trades consume sequence numbers this book never sees.
        // A gap is therefore normal; only a replayed or reordered message is a
        // real fault. Level 2 updates carry absolute quantities per price level,
        // so a dropped message leaves one stale level the next update corrects.
        if self
            .next_provider_sequence
            .is_some_and(|expected| message.sequence_num < expected)
        {
            self.require_recovery();
            self.diagnostics.sequence_gaps = self.diagnostics.sequence_gaps.saturating_add(1);
            return Ok(CoinbaseLevel2Outcome::RecoveryRequired);
        }
        self.next_provider_sequence = message.sequence_num.checked_add(1);
        let provider_time = crate::decoder::parse_rfc3339_nanos(&message.timestamp)?;
        let Some((saw_snapshot, saw_update, decoded)) =
            self.apply_events(message.events, provider_time, received_unix_nanos)?
        else {
            return Ok(CoinbaseLevel2Outcome::RecoveryRequired);
        };
        if !saw_snapshot && !saw_update {
            return Ok(CoinbaseLevel2Outcome::Ignored);
        }
        self.trim();
        let snapshot = self.snapshot(provider_time, received_unix_nanos)?;
        if saw_snapshot {
            self.ready = true;
            self.diagnostics.snapshots = self.diagnostics.snapshots.saturating_add(1);
            Ok(CoinbaseLevel2Outcome::Snapshot(snapshot))
        } else {
            self.diagnostics.deltas = self.diagnostics.deltas.saturating_add(decoded.len() as u64);
            Ok(CoinbaseLevel2Outcome::Deltas {
                deltas: decoded,
                book: snapshot,
            })
        }
    }

    fn apply_events(
        &mut self,
        events: Vec<Level2Event>,
        provider_time: i64,
        received_unix_nanos: i64,
    ) -> Result<Option<(bool, bool, Vec<DepthDelta>)>, CoinbaseError> {
        let mut saw_snapshot = false;
        let mut saw_update = false;
        let mut decoded = Vec::new();
        for event in events {
            if event.product_id != self.product_id {
                continue;
            }
            match event.event_type.as_str() {
                "snapshot" => {
                    if saw_snapshot || saw_update {
                        return Err(CoinbaseError::InvalidMessage);
                    }
                    saw_snapshot = true;
                    self.bids.clear();
                    self.asks.clear();
                }
                "update" => {
                    saw_update = true;
                    if !self.ready && !saw_snapshot {
                        self.diagnostics.ignored_while_recovering =
                            self.diagnostics.ignored_while_recovering.saturating_add(1);
                        return Ok(None);
                    }
                }
                _ => return Err(CoinbaseError::InvalidMessage),
            }
            for update in event.updates {
                let side = match update.side.to_ascii_lowercase().as_str() {
                    "bid" => BookSide::Bid,
                    "offer" | "ask" => BookSide::Ask,
                    _ => return Err(CoinbaseError::InvalidMessage),
                };
                let price = mantissa_at_scale(
                    FixedPointValue::parse(&update.price_level)?,
                    self.price_scale,
                )
                .map_err(|_| CoinbaseError::InvalidMessage)?;
                let quantity = mantissa_at_scale(
                    FixedPointValue::parse(&update.new_quantity)?,
                    self.quantity_scale,
                )
                .map_err(|_| CoinbaseError::InvalidMessage)?;
                let exchange_time = crate::decoder::parse_rfc3339_nanos(&update.event_time)?;
                let map = match side {
                    BookSide::Bid => &mut self.bids,
                    BookSide::Ask => &mut self.asks,
                };
                if quantity == 0 {
                    map.remove(&price);
                    self.diagnostics.deletes = self.diagnostics.deletes.saturating_add(1);
                } else if price > 0 && quantity > 0 {
                    map.insert(price, quantity);
                } else {
                    return Err(CoinbaseError::InvalidMessage);
                }
                if !saw_snapshot {
                    let metadata =
                        self.metadata(exchange_time, provider_time, received_unix_nanos)?;
                    decoded.push(DepthDelta {
                        metadata,
                        side,
                        level: DepthLevel {
                            price,
                            quantity,
                            order_count: None,
                        },
                    });
                }
            }
        }
        Ok(Some((saw_snapshot, saw_update, decoded)))
    }

    fn metadata(
        &mut self,
        exchange_time: i64,
        provider_time: i64,
        received_time: i64,
    ) -> Result<EventMetadata, CoinbaseError> {
        let sequence = self.next_canonical_sequence;
        self.next_canonical_sequence = sequence
            .checked_add(1)
            .ok_or(CoinbaseError::InvalidMessage)?;
        Ok(EventMetadata {
            provider_id: crate::PROVIDER.to_string(),
            instrument_id: self.instrument_id.clone(),
            entitlement_id: crate::ENTITLEMENT_CLASS.to_string(),
            source_sequence: sequence,
            session_generation: self.session_generation,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(exchange_time),
                provider_unix_nanos: Some(provider_time),
                received_unix_nanos: received_time,
            },
        })
    }

    fn snapshot(
        &mut self,
        provider_time: i64,
        received_time: i64,
    ) -> Result<DepthSnapshot, CoinbaseError> {
        let metadata = self.metadata(provider_time, provider_time, received_time)?;
        let snapshot = DepthSnapshot {
            metadata,
            bids: self
                .bids
                .iter()
                .rev()
                .map(|(&price, &quantity)| DepthLevel {
                    price,
                    quantity,
                    order_count: None,
                })
                .collect(),
            asks: self
                .asks
                .iter()
                .map(|(&price, &quantity)| DepthLevel {
                    price,
                    quantity,
                    order_count: None,
                })
                .collect(),
        };
        snapshot
            .validate(COINBASE_MAXIMUM_LEVELS_PER_SIDE)
            .map_err(|_| CoinbaseError::InvalidMessage)?;
        Ok(snapshot)
    }

    fn trim(&mut self) {
        while self.bids.len() > COINBASE_MAXIMUM_LEVELS_PER_SIDE {
            if let Some(price) = self.bids.keys().next().copied() {
                self.bids.remove(&price);
                self.diagnostics.levels_trimmed = self.diagnostics.levels_trimmed.saturating_add(1);
            }
        }
        while self.asks.len() > COINBASE_MAXIMUM_LEVELS_PER_SIDE {
            if let Some(price) = self.asks.keys().next_back().copied() {
                self.asks.remove(&price);
                self.diagnostics.levels_trimmed = self.diagnostics.levels_trimmed.saturating_add(1);
            }
        }
    }

    fn require_recovery(&mut self) {
        self.bids.clear();
        self.asks.clear();
        self.ready = false;
        self.next_provider_sequence = None;
    }
}

#[must_use]
pub const fn coinbase_depth_limit() -> NonZeroUsize {
    match NonZeroUsize::new(COINBASE_MAXIMUM_LEVELS_PER_SIDE) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}

#[cfg(test)]
mod tests {
    use super::{CoinbaseLevel2Book, CoinbaseLevel2Outcome};

    const RECEIVED: i64 = 1_700_000_000_100_000_000;

    #[test]
    fn snapshot_update_and_zero_quantity_delete_are_canonical() {
        let mut book = CoinbaseLevel2Book::try_new("BTC-USD", 2, 8, 7).expect("book");
        let snapshot = br#"{"channel":"l2_data","timestamp":"2023-11-14T22:13:20Z","sequence_num":0,"events":[{"type":"snapshot","product_id":"BTC-USD","updates":[{"side":"bid","event_time":"2023-11-14T22:13:20Z","price_level":"100.00","new_quantity":"2.00000000"},{"side":"offer","event_time":"2023-11-14T22:13:20Z","price_level":"101.00","new_quantity":"3.00000000"}]}]}"#;
        assert!(matches!(
            book.apply_message(snapshot, RECEIVED).expect("snapshot"),
            CoinbaseLevel2Outcome::Snapshot(_)
        ));
        let update = br#"{"channel":"l2_data","timestamp":"2023-11-14T22:13:21Z","sequence_num":1,"events":[{"type":"update","product_id":"BTC-USD","updates":[{"side":"bid","event_time":"2023-11-14T22:13:21Z","price_level":"100.00","new_quantity":"0"},{"side":"bid","event_time":"2023-11-14T22:13:21Z","price_level":"99.00","new_quantity":"1.00000000"}]}]}"#;
        let CoinbaseLevel2Outcome::Deltas { deltas, book } =
            book.apply_message(update, RECEIVED + 1).expect("update")
        else {
            panic!("update emits deltas");
        };
        assert_eq!(deltas.len(), 2);
        assert_eq!(book.bids.len(), 1);
        assert_eq!(book.bids[0].price, 9_900);
        assert_eq!(book.metadata.session_generation, 7);
        assert_eq!(book.bids.len(), 1);
    }

    /// Live Coinbase interleaves heartbeats and trades on the same sequence, so
    /// the Level 2 book sees non-contiguous numbers constantly. Treating that as
    /// loss wedged the book after the first update and emptied the DOM.
    #[test]
    fn skipped_sequence_numbers_from_other_channels_keep_the_book_live() {
        let mut book = CoinbaseLevel2Book::try_new("BTC-USD", 2, 8, 1).expect("book");
        let snapshot = br#"{"channel":"l2_data","timestamp":"2023-11-14T22:13:20Z","sequence_num":0,"events":[{"type":"snapshot","product_id":"BTC-USD","updates":[{"side":"bid","event_time":"2023-11-14T22:13:20Z","price_level":"100.00","new_quantity":"1.00000000"},{"side":"offer","event_time":"2023-11-14T22:13:20Z","price_level":"101.00","new_quantity":"1.00000000"}]}]}"#;
        book.apply_message(snapshot, RECEIVED).expect("snapshot");
        let skipped = br#"{"channel":"l2_data","timestamp":"2023-11-14T22:13:22Z","sequence_num":9,"events":[{"type":"update","product_id":"BTC-USD","updates":[{"side":"bid","event_time":"2023-11-14T22:13:22Z","price_level":"99.00","new_quantity":"2.00000000"}]}]}"#;
        let outcome = book
            .apply_message(skipped, RECEIVED + 2)
            .expect("skipped sequence applies");
        assert!(
            matches!(outcome, CoinbaseLevel2Outcome::Deltas { .. }),
            "a gap left by another channel must not wedge the book: {outcome:?}"
        );
    }

    /// A replayed or reordered message is a real fault and must clear the book.
    #[test]
    fn replayed_sequence_number_requires_recovery() {
        let mut book = CoinbaseLevel2Book::try_new("BTC-USD", 2, 8, 1).expect("book");
        let snapshot = br#"{"channel":"l2_data","timestamp":"2023-11-14T22:13:20Z","sequence_num":5,"events":[{"type":"snapshot","product_id":"BTC-USD","updates":[{"side":"bid","event_time":"2023-11-14T22:13:20Z","price_level":"100.00","new_quantity":"1.00000000"},{"side":"offer","event_time":"2023-11-14T22:13:20Z","price_level":"101.00","new_quantity":"1.00000000"}]}]}"#;
        book.apply_message(snapshot, RECEIVED).expect("snapshot");
        let replayed = br#"{"channel":"l2_data","timestamp":"2023-11-14T22:13:22Z","sequence_num":4,"events":[{"type":"update","product_id":"BTC-USD","updates":[]}]}"#;
        assert_eq!(
            book.apply_message(replayed, RECEIVED + 2)
                .expect("replay classified"),
            CoinbaseLevel2Outcome::RecoveryRequired
        );
    }
}
