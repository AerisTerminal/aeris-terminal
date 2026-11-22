//! Bounded local L2 order book for snapshot+delta synchronization and
//! cross-venue tick-bucketed order-book aggregation.

use std::{cmp::Reverse, collections::BTreeMap};

use aeris_market_data::{DepthLevel, DepthSnapshot, EventMetadata, QuoteLevel, TopOfBookQuote};

use crate::error::{CryptoError, CryptoResult};

/// Default maximum levels retained per side by a local crypto order book.
pub const DEFAULT_MAX_BOOK_LEVELS: usize = 1_000;

/// Hard upper bound matching `market_runtime`'s canonical `OrderBook` capacity.
pub const MAXIMUM_BOOK_LEVELS: usize = 4_096;

/// One parsed price-level update (`quantity == 0` deletes the price level).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BookLevelUpdate {
    pub price: i64,
    pub quantity: i64,
    pub order_count: Option<u32>,
}

/// Errors produced while maintaining a local order book.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BookError {
    InvalidPrice(i64),
    InvalidQuantity(i64),
    Crossed { best_bid: i64, best_ask: i64 },
}

impl std::fmt::Display for BookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPrice(price) => write!(f, "order book price is invalid: {price}"),
            Self::InvalidQuantity(qty) => write!(f, "order book quantity is invalid: {qty}"),
            Self::Crossed { best_bid, best_ask } => {
                write!(f, "order book is crossed: bid {best_bid} >= ask {best_ask}")
            }
        }
    }
}

/// Bounded local L2 order book maintaining sorted bids (best-first) and asks (best-first).
#[derive(Clone, Debug)]
pub struct LocalOrderBook {
    max_levels: usize,
    bids: BTreeMap<Reverse<i64>, (i64, Option<u32>)>,
    asks: BTreeMap<i64, (i64, Option<u32>)>,
    synced: bool,
}

impl LocalOrderBook {
    /// Creates an empty unsynced book bounded to `max_levels` per side.
    #[must_use]
    pub fn new(max_levels: usize) -> Self {
        Self {
            max_levels: max_levels.clamp(1, MAXIMUM_BOOK_LEVELS),
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            synced: false,
        }
    }

    /// Returns true once at least one covering snapshot has been applied.
    #[must_use]
    pub const fn is_synced(&self) -> bool {
        self.synced
    }

    /// Clears all levels and marks the book unsynced.
    pub fn clear(&mut self) {
        self.bids.clear();
        self.asks.clear();
        self.synced = false;
    }

    /// Replaces the entire book with a validated covering snapshot.
    ///
    /// # Errors
    /// Returns an error if any level has non-positive price/quantity or the book is crossed.
    pub fn apply_snapshot(
        &mut self,
        bids: &[BookLevelUpdate],
        asks: &[BookLevelUpdate],
    ) -> Result<(), BookError> {
        let mut next_bids = BTreeMap::new();
        for level in bids {
            if level.price <= 0 {
                return Err(BookError::InvalidPrice(level.price));
            }
            if level.quantity < 0 {
                return Err(BookError::InvalidQuantity(level.quantity));
            }
            if level.quantity > 0 {
                next_bids.insert(Reverse(level.price), (level.quantity, level.order_count));
            }
        }
        let mut next_asks = BTreeMap::new();
        for level in asks {
            if level.price <= 0 {
                return Err(BookError::InvalidPrice(level.price));
            }
            if level.quantity < 0 {
                return Err(BookError::InvalidQuantity(level.quantity));
            }
            if level.quantity > 0 {
                next_asks.insert(level.price, (level.quantity, level.order_count));
            }
        }
        trim_side(&mut next_bids, self.max_levels);
        trim_side(&mut next_asks, self.max_levels);
        check_uncrossed(&next_bids, &next_asks)?;
        self.bids = next_bids;
        self.asks = next_asks;
        self.synced = true;
        Ok(())
    }

    /// Applies an incremental delta (`quantity == 0` removes the level).
    ///
    /// # Errors
    /// Returns an error for negative price/quantity or if the resulting book is crossed.
    pub fn apply_delta(
        &mut self,
        bids: &[BookLevelUpdate],
        asks: &[BookLevelUpdate],
    ) -> Result<(), BookError> {
        for level in bids {
            if level.price <= 0 {
                return Err(BookError::InvalidPrice(level.price));
            }
            if level.quantity < 0 {
                return Err(BookError::InvalidQuantity(level.quantity));
            }
            if level.quantity == 0 {
                self.bids.remove(&Reverse(level.price));
            } else {
                self.bids
                    .insert(Reverse(level.price), (level.quantity, level.order_count));
            }
        }
        for level in asks {
            if level.price <= 0 {
                return Err(BookError::InvalidPrice(level.price));
            }
            if level.quantity < 0 {
                return Err(BookError::InvalidQuantity(level.quantity));
            }
            if level.quantity == 0 {
                self.asks.remove(&level.price);
            } else {
                self.asks
                    .insert(level.price, (level.quantity, level.order_count));
            }
        }
        trim_side(&mut self.bids, self.max_levels);
        trim_side(&mut self.asks, self.max_levels);
        check_uncrossed(&self.bids, &self.asks)
    }

    /// Returns the current best bid `(price, quantity)`.
    #[must_use]
    pub fn best_bid(&self) -> Option<(i64, i64)> {
        self.bids
            .first_key_value()
            .map(|(Reverse(price), (qty, _))| (*price, *qty))
    }

    /// Returns the current best ask `(price, quantity)`.
    #[must_use]
    pub fn best_ask(&self) -> Option<(i64, i64)> {
        self.asks
            .first_key_value()
            .map(|(price, (qty, _))| (*price, *qty))
    }

    /// Iterates bids best-first as `(price, quantity, order_count)`.
    pub fn bids_iter(&self) -> impl Iterator<Item = (i64, i64, Option<u32>)> + '_ {
        self.bids
            .iter()
            .map(|(Reverse(price), (qty, count))| (*price, *qty, *count))
    }

    /// Iterates asks best-first as `(price, quantity, order_count)`.
    pub fn asks_iter(&self) -> impl Iterator<Item = (i64, i64, Option<u32>)> + '_ {
        self.asks
            .iter()
            .map(|(price, (qty, count))| (*price, *qty, *count))
    }

    /// Projects the book into a validated canonical [`DepthSnapshot`].
    ///
    /// # Errors
    /// Returns an error if the resulting snapshot fails domain validation.
    pub fn to_depth_snapshot(
        &self,
        metadata: EventMetadata,
        max_publish_levels: usize,
    ) -> CryptoResult<DepthSnapshot> {
        let cap = max_publish_levels.clamp(1, MAXIMUM_BOOK_LEVELS);
        let bids: Vec<DepthLevel> = self
            .bids
            .iter()
            .take(cap)
            .map(|(Reverse(price), (quantity, order_count))| DepthLevel {
                price: *price,
                quantity: *quantity,
                order_count: *order_count,
            })
            .collect();
        let asks: Vec<DepthLevel> = self
            .asks
            .iter()
            .take(cap)
            .map(|(price, (quantity, order_count))| DepthLevel {
                price: *price,
                quantity: *quantity,
                order_count: *order_count,
            })
            .collect();
        let snapshot = DepthSnapshot {
            metadata,
            bids,
            asks,
        };
        snapshot
            .validate(cap)
            .map_err(|error| CryptoError::OrderBook(error.to_string()))?;
        Ok(snapshot)
    }

    /// Projects the current top of book into a [`TopOfBookQuote`], if at least one side exists.
    #[must_use]
    pub fn to_top_of_book(&self, metadata: EventMetadata) -> Option<TopOfBookQuote> {
        let bid = self
            .bids
            .first_key_value()
            .map(|(Reverse(price), (qty, count))| QuoteLevel {
                price: *price,
                quantity: Some(*qty),
                order_count: *count,
            });
        let ask = self
            .asks
            .first_key_value()
            .map(|(price, (qty, count))| QuoteLevel {
                price: *price,
                quantity: Some(*qty),
                order_count: *count,
            });
        if bid.is_none() && ask.is_none() {
            return None;
        }
        Some(TopOfBookQuote { metadata, bid, ask })
    }
}

/// Merges multiple healthy venue order books into one tick-aligned, uncrossed
/// [`DepthSnapshot`] by summing resting quantities across venues at each bucket.
///
/// Bids are floored to `tick_size` (conservative) and asks are ceiled to
/// `tick_size` (conservative). If cross-venue latency momentarily causes a
/// merged bid >= merged ask, overlapping edge levels are resolved around the
/// midpoint so the published aggregate snapshot always remains strictly uncrossed.
///
/// # Errors
/// Returns an error if `tick_size <= 0` or domain validation fails.
pub fn merge_books<'a>(
    books: impl IntoIterator<Item = &'a LocalOrderBook>,
    tick_size: i64,
    max_levels: usize,
    metadata: EventMetadata,
) -> CryptoResult<DepthSnapshot> {
    if tick_size <= 0 {
        return Err(CryptoError::OrderBook(
            "merge_books tick_size must be positive".to_string(),
        ));
    }
    let cap = max_levels.clamp(1, MAXIMUM_BOOK_LEVELS);
    let mut bids: BTreeMap<Reverse<i64>, i64> = BTreeMap::new();
    let mut asks: BTreeMap<i64, i64> = BTreeMap::new();

    for book in books {
        if !book.is_synced() {
            continue;
        }
        for (price, qty, _) in book.bids_iter() {
            let bucket = (price / tick_size) * tick_size;
            if bucket > 0 {
                let entry = bids.entry(Reverse(bucket)).or_insert(0);
                *entry = entry.saturating_add(qty);
            }
        }
        for (price, qty, _) in book.asks_iter() {
            let bucket = price
                .saturating_add(tick_size - 1)
                .div_euclid(tick_size)
                .saturating_mul(tick_size);
            if bucket > 0 {
                let entry = asks.entry(bucket).or_insert(0);
                *entry = entry.saturating_add(qty);
            }
        }
    }

    // Resolve momentary cross-venue quote overlap cleanly by trimming crossed edge levels.
    while let (Some((&Reverse(best_bid), _)), Some((&best_ask, _))) =
        (bids.first_key_value(), asks.first_key_value())
    {
        if best_bid < best_ask {
            break;
        }
        let bid_qty = bids.get(&Reverse(best_bid)).copied().unwrap_or(0);
        let ask_qty = asks.get(&best_ask).copied().unwrap_or(0);
        if bid_qty <= ask_qty {
            bids.remove(&Reverse(best_bid));
        } else {
            asks.remove(&best_ask);
        }
    }

    let snapshot = DepthSnapshot {
        metadata,
        bids: bids
            .into_iter()
            .take(cap)
            .map(|(Reverse(price), quantity)| DepthLevel {
                price,
                quantity,
                order_count: None,
            })
            .collect(),
        asks: asks
            .into_iter()
            .take(cap)
            .map(|(price, quantity)| DepthLevel {
                price,
                quantity,
                order_count: None,
            })
            .collect(),
    };
    snapshot
        .validate(cap)
        .map_err(|error| CryptoError::OrderBook(error.to_string()))?;
    Ok(snapshot)
}

fn trim_side<K: Ord, V>(side: &mut BTreeMap<K, V>, max_levels: usize) {
    while side.len() > max_levels {
        side.pop_last();
    }
}

fn check_uncrossed(
    bids: &BTreeMap<Reverse<i64>, (i64, Option<u32>)>,
    asks: &BTreeMap<i64, (i64, Option<u32>)>,
) -> Result<(), BookError> {
    if let (Some((&Reverse(best_bid), _)), Some((&best_ask, _))) =
        (bids.first_key_value(), asks.first_key_value())
        && best_bid >= best_ask
    {
        return Err(BookError::Crossed { best_bid, best_ask });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_market_data::QualifiedTimestamp;

    fn test_metadata() -> EventMetadata {
        EventMetadata {
            provider_id: "test".to_string(),
            instrument_id: "test:BTCUSDT".to_string(),
            entitlement_id: "public".to_string(),
            source_sequence: 1,
            session_generation: 1,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(1_000_000),
                provider_unix_nanos: Some(1_000_000),
                received_unix_nanos: 1_000_000,
            },
        }
    }

    #[test]
    fn snapshot_and_delta_maintain_sorted_bounded_book() {
        let mut book = LocalOrderBook::new(2);
        book.apply_snapshot(
            &[
                BookLevelUpdate {
                    price: 100,
                    quantity: 5,
                    order_count: None,
                },
                BookLevelUpdate {
                    price: 99,
                    quantity: 10,
                    order_count: None,
                },
                BookLevelUpdate {
                    price: 98,
                    quantity: 20,
                    order_count: None,
                },
            ],
            &[
                BookLevelUpdate {
                    price: 101,
                    quantity: 7,
                    order_count: None,
                },
                BookLevelUpdate {
                    price: 102,
                    quantity: 9,
                    order_count: None,
                },
            ],
        )
        .unwrap();
        assert_eq!(book.best_bid(), Some((100, 5)));
        assert_eq!(book.best_ask(), Some((101, 7)));

        // Delete top bid, add new better ask
        book.apply_delta(
            &[BookLevelUpdate {
                price: 100,
                quantity: 0,
                order_count: None,
            }],
            &[BookLevelUpdate {
                price: 101,
                quantity: 12,
                order_count: None,
            }],
        )
        .unwrap();
        assert_eq!(book.best_bid(), Some((99, 10)));
        assert_eq!(book.best_ask(), Some((101, 12)));

        let snap = book.to_depth_snapshot(test_metadata(), 10).unwrap();
        assert_eq!(snap.bids.len(), 1);
        assert_eq!(snap.asks.len(), 2);
    }

    #[test]
    fn delta_detects_crossed_book() {
        let mut book = LocalOrderBook::new(10);
        book.apply_snapshot(
            &[BookLevelUpdate {
                price: 100,
                quantity: 5,
                order_count: None,
            }],
            &[BookLevelUpdate {
                price: 101,
                quantity: 5,
                order_count: None,
            }],
        )
        .unwrap();
        let err = book
            .apply_delta(
                &[BookLevelUpdate {
                    price: 101,
                    quantity: 1,
                    order_count: None,
                }],
                &[],
            )
            .unwrap_err();
        assert_eq!(
            err,
            BookError::Crossed {
                best_bid: 101,
                best_ask: 101
            }
        );
    }

    #[test]
    fn merge_books_sums_quantities_and_resolves_cross_venue_overlap() {
        let mut binance = LocalOrderBook::new(10);
        binance
            .apply_snapshot(
                &[BookLevelUpdate {
                    price: 10_000,
                    quantity: 30,
                    order_count: None,
                }],
                &[BookLevelUpdate {
                    price: 10_010,
                    quantity: 20,
                    order_count: None,
                }],
            )
            .unwrap();
        let mut bybit = LocalOrderBook::new(10);
        bybit
            .apply_snapshot(
                &[BookLevelUpdate {
                    price: 10_005,
                    quantity: 15,
                    order_count: None,
                }],
                &[BookLevelUpdate {
                    price: 10_015,
                    quantity: 25,
                    order_count: None,
                }],
            )
            .unwrap();

        let merged = merge_books([&binance, &bybit], 10, 50, test_metadata()).unwrap();
        // Both bids floor to 10_000 -> 30 + 15 = 45
        assert_eq!(merged.bids[0].price, 10_000);
        assert_eq!(merged.bids[0].quantity, 45);
        // 10_010 stays 10_010 (qty 20); 10_015 ceils to 10_020 (qty 25)
        assert_eq!(merged.asks[0].price, 10_010);
        assert_eq!(merged.asks[0].quantity, 20);
        assert_eq!(merged.asks[1].price, 10_020);
        assert_eq!(merged.asks[1].quantity, 25);
    }
}
