//! Provider aggregate (market-by-price) book.
//!
//! Rithmic Test was observed on MNQ (2026-10-26) to answer an aggregate
//! subscription with one snapshot image followed by single-level updates
//! whose zero size removes the level. Clear, no-book and the begin/middle/end
//! group frames follow the provider's documented meaning (clear the book, no
//! levels, and one atomic group of updates); they were not observed.

use crate::OrderBookUpdateKind;
use std::collections::BTreeMap;

/// Most price levels one side may hold.
pub(crate) const MAXIMUM_PRICE_LEVELS_PER_SIDE: usize = 4_096;

/// One fixed-point aggregate level from a provider frame. Zero quantity
/// removes the level.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PriceLevel {
    pub(crate) price: i64,
    pub(crate) quantity: i64,
    pub(crate) order_count: Option<u32>,
}

/// One aggregate-book frame with fixed-point levels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PriceLevelFrame {
    pub(crate) kind: OrderBookUpdateKind,
    pub(crate) bids: Vec<PriceLevel>,
    pub(crate) asks: Vec<PriceLevel>,
}

/// What one frame did to the image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PriceLevelOutcome {
    /// The image changed and is authoritative.
    Changed,
    /// The frame opened or continued a group, or arrived before the first image.
    Pending,
}

/// A frame sequence the image cannot represent. The image is discarded and
/// only a new provider image can make it authoritative again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PriceLevelFault {
    /// A group frame arrived without an open group, or a group was reopened.
    BrokenGroup,
    /// A side would hold more than [`MAXIMUM_PRICE_LEVELS_PER_SIDE`] levels.
    LevelLimitExceeded,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Level {
    quantity: i64,
    order_count: Option<u32>,
}

/// Bounded aggregate book. No level is authoritative before the first
/// snapshot image, clear or no-book frame.
#[derive(Default)]
pub(crate) struct PriceLevelBook {
    ready: bool,
    bids: BTreeMap<i64, Level>,
    asks: BTreeMap<i64, Level>,
    group: Option<Vec<PriceLevelFrame>>,
    group_levels: usize,
}

impl PriceLevelBook {
    /// Applies one frame in arrival order.
    ///
    /// # Errors
    ///
    /// Returns the fault after discarding the image.
    pub(crate) fn apply(
        &mut self,
        frame: PriceLevelFrame,
    ) -> Result<PriceLevelOutcome, PriceLevelFault> {
        let outcome = self.apply_frame(frame);
        if outcome.is_err() {
            *self = Self::default();
        }
        outcome
    }

    fn apply_frame(
        &mut self,
        frame: PriceLevelFrame,
    ) -> Result<PriceLevelOutcome, PriceLevelFault> {
        match frame.kind {
            OrderBookUpdateKind::Clear | OrderBookUpdateKind::Unavailable => {
                *self = Self {
                    ready: true,
                    ..Self::default()
                };
                Ok(PriceLevelOutcome::Changed)
            }
            OrderBookUpdateKind::Snapshot => {
                *self = Self {
                    ready: true,
                    ..Self::default()
                };
                self.upsert(&frame)?;
                Ok(PriceLevelOutcome::Changed)
            }
            OrderBookUpdateKind::Solo | OrderBookUpdateKind::Begin if self.group.is_some() => {
                Err(PriceLevelFault::BrokenGroup)
            }
            OrderBookUpdateKind::Solo if self.ready => {
                self.upsert(&frame)?;
                Ok(PriceLevelOutcome::Changed)
            }
            OrderBookUpdateKind::Begin if self.ready => {
                self.group_levels = 0;
                self.extend_group(Vec::new(), frame)?;
                Ok(PriceLevelOutcome::Pending)
            }
            OrderBookUpdateKind::Middle | OrderBookUpdateKind::End if self.ready => {
                let Some(group) = self.group.take() else {
                    return Err(PriceLevelFault::BrokenGroup);
                };
                let complete = frame.kind == OrderBookUpdateKind::End;
                self.extend_group(group, frame)?;
                if !complete {
                    return Ok(PriceLevelOutcome::Pending);
                }
                for frame in self.group.take().unwrap_or_default() {
                    self.upsert(&frame)?;
                }
                Ok(PriceLevelOutcome::Changed)
            }
            // Before the first image no update has a base to apply to.
            OrderBookUpdateKind::Solo
            | OrderBookUpdateKind::Begin
            | OrderBookUpdateKind::Middle
            | OrderBookUpdateKind::End => Ok(PriceLevelOutcome::Pending),
        }
    }

    fn extend_group(
        &mut self,
        mut group: Vec<PriceLevelFrame>,
        frame: PriceLevelFrame,
    ) -> Result<(), PriceLevelFault> {
        self.group_levels = self
            .group_levels
            .saturating_add(frame.bids.len())
            .saturating_add(frame.asks.len());
        if self.group_levels > MAXIMUM_PRICE_LEVELS_PER_SIDE.saturating_mul(2) {
            return Err(PriceLevelFault::LevelLimitExceeded);
        }
        group.push(frame);
        self.group = Some(group);
        Ok(())
    }

    fn upsert(&mut self, frame: &PriceLevelFrame) -> Result<(), PriceLevelFault> {
        for (levels, updates) in [(&mut self.bids, &frame.bids), (&mut self.asks, &frame.asks)] {
            for update in updates {
                if update.quantity <= 0 {
                    levels.remove(&update.price);
                    continue;
                }
                levels.insert(
                    update.price,
                    Level {
                        quantity: update.quantity,
                        order_count: update.order_count,
                    },
                );
                if levels.len() > MAXIMUM_PRICE_LEVELS_PER_SIDE {
                    return Err(PriceLevelFault::LevelLimitExceeded);
                }
            }
        }
        Ok(())
    }

    /// Bids best first.
    pub(crate) fn bids(&self) -> impl Iterator<Item = PriceLevel> + '_ {
        self.bids.iter().rev().map(level)
    }

    /// Asks best first.
    pub(crate) fn asks(&self) -> impl Iterator<Item = PriceLevel> + '_ {
        self.asks.iter().map(level)
    }
}

fn level((price, level): (&i64, &Level)) -> PriceLevel {
    PriceLevel {
        price: *price,
        quantity: level.quantity,
        order_count: level.order_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        kind: OrderBookUpdateKind,
        bids: &[(i64, i64)],
        asks: &[(i64, i64)],
    ) -> PriceLevelFrame {
        let levels = |levels: &[(i64, i64)]| {
            levels
                .iter()
                .map(|(price, quantity)| PriceLevel {
                    price: *price,
                    quantity: *quantity,
                    order_count: None,
                })
                .collect()
        };
        PriceLevelFrame {
            kind,
            bids: levels(bids),
            asks: levels(asks),
        }
    }

    type Sides = (Vec<(i64, i64)>, Vec<(i64, i64)>);

    fn sides(book: &PriceLevelBook) -> Sides {
        (
            book.bids()
                .map(|level| (level.price, level.quantity))
                .collect(),
            book.asks()
                .map(|level| (level.price, level.quantity))
                .collect(),
        )
    }

    #[test]
    fn snapshot_image_then_single_level_upserts_and_zero_size_removals() {
        let mut book = PriceLevelBook::default();
        assert_eq!(
            book.apply(frame(OrderBookUpdateKind::Solo, &[(100, 1)], &[])),
            Ok(PriceLevelOutcome::Pending),
            "no update applies before the first image"
        );
        assert_eq!(
            book.apply(frame(
                OrderBookUpdateKind::Snapshot,
                &[(100, 5), (99, 7)],
                &[(101, 3), (102, 4)]
            )),
            Ok(PriceLevelOutcome::Changed)
        );
        assert_eq!(
            book.apply(frame(OrderBookUpdateKind::Solo, &[(100, 6)], &[])),
            Ok(PriceLevelOutcome::Changed)
        );
        assert_eq!(
            book.apply(frame(OrderBookUpdateKind::Solo, &[], &[(101, 0)])),
            Ok(PriceLevelOutcome::Changed)
        );
        assert_eq!(sides(&book), (vec![(100, 6), (99, 7)], vec![(102, 4)]));
        book.apply(frame(OrderBookUpdateKind::Snapshot, &[(98, 1)], &[]))
            .expect("a new image replaces the book");
        assert_eq!(sides(&book), (vec![(98, 1)], vec![]));
    }

    #[test]
    fn grouped_updates_apply_atomically_and_broken_groups_discard_the_image() {
        let mut book = PriceLevelBook::default();
        book.apply(frame(
            OrderBookUpdateKind::Snapshot,
            &[(100, 5)],
            &[(101, 3)],
        ))
        .expect("image installs");
        assert_eq!(
            book.apply(frame(OrderBookUpdateKind::Begin, &[(100, 0)], &[])),
            Ok(PriceLevelOutcome::Pending)
        );
        assert_eq!(
            book.apply(frame(OrderBookUpdateKind::Middle, &[(99, 2)], &[])),
            Ok(PriceLevelOutcome::Pending)
        );
        assert_eq!(sides(&book), (vec![(100, 5)], vec![(101, 3)]));
        assert_eq!(
            book.apply(frame(OrderBookUpdateKind::End, &[], &[(101, 9)])),
            Ok(PriceLevelOutcome::Changed)
        );
        assert_eq!(sides(&book), (vec![(99, 2)], vec![(101, 9)]));

        for broken in [
            vec![frame(OrderBookUpdateKind::End, &[(98, 1)], &[])],
            vec![
                frame(OrderBookUpdateKind::Begin, &[(98, 1)], &[]),
                frame(OrderBookUpdateKind::Begin, &[(97, 1)], &[]),
            ],
            vec![
                frame(OrderBookUpdateKind::Begin, &[(98, 1)], &[]),
                frame(OrderBookUpdateKind::Solo, &[(97, 1)], &[]),
            ],
        ] {
            let mut book = PriceLevelBook::default();
            book.apply(frame(OrderBookUpdateKind::Clear, &[], &[]))
                .expect("a clear makes the empty book authoritative");
            let outcome = broken
                .into_iter()
                .map(|frame| book.apply(frame))
                .find(Result::is_err);
            assert_eq!(outcome, Some(Err(PriceLevelFault::BrokenGroup)));
            assert_eq!(
                book.apply(frame(OrderBookUpdateKind::Solo, &[(96, 1)], &[])),
                Ok(PriceLevelOutcome::Pending),
                "a discarded image waits for the next one"
            );
        }
    }

    #[test]
    fn side_level_bound_fails_closed() {
        let mut book = PriceLevelBook::default();
        let levels = (1..=i64::try_from(MAXIMUM_PRICE_LEVELS_PER_SIDE + 1).unwrap_or(i64::MAX))
            .map(|price| (price, 1))
            .collect::<Vec<_>>();
        assert_eq!(
            book.apply(frame(OrderBookUpdateKind::Snapshot, &levels, &[])),
            Err(PriceLevelFault::LevelLimitExceeded)
        );
        assert_eq!(sides(&book), (vec![], vec![]));
    }
}
