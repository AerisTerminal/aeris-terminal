//! Sanitized stream evidence for `--mbp-only`.
//!
//! Records how Rithmic price-level (template 156) frames behave after a
//! subscription: the update-kind sequence, level counts, zero-size levels and
//! whether a book built under each candidate reading matches the best bid and
//! offer stream. Prices and sizes are compared in memory and never printed.

use aeris_rithmic_protocol_adapter::{
    OrderBookLevel, OrderBookUpdate, OrderBookUpdateKind, QuoteSideUpdate, QuoteUpdate,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;

const SEQUENCE_PREFIX_FRAMES: usize = 48;

#[derive(Default)]
pub(super) struct MbpStreamEvidence {
    frames: usize,
    kinds: KindCounts,
    sequence: String,
    sequence_frames: usize,
    frames_before_first_solo: Option<usize>,
    zero_size_levels: KindCounts,
    open_group: bool,
    unbalanced_groups: usize,
    upsert: CandidateBook,
    snapshot_replaces: CandidateBook,
    best_bid: Option<i64>,
    best_ask: Option<i64>,
    quote_frames: usize,
}

#[derive(Default)]
struct KindCounts {
    clear: usize,
    unavailable: usize,
    snapshot: usize,
    begin: usize,
    middle: usize,
    end: usize,
    solo: usize,
}

impl KindCounts {
    fn add(&mut self, kind: OrderBookUpdateKind, count: usize) {
        let slot = match kind {
            OrderBookUpdateKind::Clear => &mut self.clear,
            OrderBookUpdateKind::Unavailable => &mut self.unavailable,
            OrderBookUpdateKind::Snapshot => &mut self.snapshot,
            OrderBookUpdateKind::Begin => &mut self.begin,
            OrderBookUpdateKind::Middle => &mut self.middle,
            OrderBookUpdateKind::End => &mut self.end,
            OrderBookUpdateKind::Solo => &mut self.solo,
        };
        *slot = slot.saturating_add(count);
    }
}

impl std::fmt::Display for KindCounts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "clear={} no_book={} snapshot={} begin={} middle={} end={} solo={}",
            self.clear,
            self.unavailable,
            self.snapshot,
            self.begin,
            self.middle,
            self.end,
            self.solo
        )
    }
}

/// One reading of the stream, scored against the best bid and offer.
#[derive(Default)]
struct CandidateBook {
    bids: BTreeMap<i64, u32>,
    asks: BTreeMap<i64, u32>,
    top_matches: usize,
    top_mismatches: usize,
    crossed: usize,
    maximum_bid_levels: usize,
    maximum_ask_levels: usize,
}

impl CandidateBook {
    fn apply(&mut self, update: &OrderBookUpdate, replace_on_snapshot: bool) {
        if update.kind == OrderBookUpdateKind::Clear
            || update.kind == OrderBookUpdateKind::Unavailable
        {
            self.bids.clear();
            self.asks.clear();
            return;
        }
        let replace = replace_on_snapshot && update.kind == OrderBookUpdateKind::Snapshot;
        if update.present_sides.bids {
            apply_side(&mut self.bids, &update.bids, replace);
        }
        if update.present_sides.asks {
            apply_side(&mut self.asks, &update.asks, replace);
        }
        self.maximum_bid_levels = self.maximum_bid_levels.max(self.bids.len());
        self.maximum_ask_levels = self.maximum_ask_levels.max(self.asks.len());
        if let (Some(bid), Some(ask)) = (self.best_bid(), self.best_ask())
            && bid >= ask
        {
            self.crossed = self.crossed.saturating_add(1);
        }
    }

    fn best_bid(&self) -> Option<i64> {
        self.bids.keys().next_back().copied()
    }

    fn best_ask(&self) -> Option<i64> {
        self.asks.keys().next().copied()
    }

    fn score(&mut self, best_bid: Option<i64>, best_ask: Option<i64>) {
        if best_bid.is_none() && best_ask.is_none() {
            return;
        }
        if self.best_bid() == best_bid && self.best_ask() == best_ask {
            self.top_matches = self.top_matches.saturating_add(1);
        } else {
            self.top_mismatches = self.top_mismatches.saturating_add(1);
        }
    }
}

impl std::fmt::Display for CandidateBook {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "top_matches={} top_mismatches={} crossed={} bid_levels={} ask_levels={} maximum_bid_levels={} maximum_ask_levels={}",
            self.top_matches,
            self.top_mismatches,
            self.crossed,
            self.bids.len(),
            self.asks.len(),
            self.maximum_bid_levels,
            self.maximum_ask_levels
        )
    }
}

fn apply_side(side: &mut BTreeMap<i64, u32>, levels: &[OrderBookLevel], replace: bool) {
    if replace {
        side.clear();
    }
    for level in levels {
        let price = price_key(level.price);
        if level.size == 0 {
            side.remove(&price);
        } else {
            side.insert(price, level.size);
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
fn price_key(price: f64) -> i64 {
    (price * 1_000_000.0).round() as i64
}

impl MbpStreamEvidence {
    pub(super) fn record_book(&mut self, update: &OrderBookUpdate) {
        self.frames = self.frames.saturating_add(1);
        self.kinds.add(update.kind, 1);
        let zero = update
            .bids
            .iter()
            .chain(&update.asks)
            .filter(|level| level.size == 0)
            .count();
        self.zero_size_levels.add(update.kind, zero);
        match update.kind {
            OrderBookUpdateKind::Begin => {
                if self.open_group {
                    self.unbalanced_groups = self.unbalanced_groups.saturating_add(1);
                }
                self.open_group = true;
            }
            OrderBookUpdateKind::Middle if !self.open_group => {
                self.unbalanced_groups = self.unbalanced_groups.saturating_add(1);
            }
            OrderBookUpdateKind::End => {
                if !self.open_group {
                    self.unbalanced_groups = self.unbalanced_groups.saturating_add(1);
                }
                self.open_group = false;
            }
            _ => {}
        }
        if update.kind == OrderBookUpdateKind::Solo && self.frames_before_first_solo.is_none() {
            self.frames_before_first_solo = Some(self.frames - 1);
        }
        if self.sequence_frames < SEQUENCE_PREFIX_FRAMES {
            self.sequence_frames += 1;
            let letter = match update.kind {
                OrderBookUpdateKind::Clear => 'C',
                OrderBookUpdateKind::Unavailable => 'N',
                OrderBookUpdateKind::Snapshot => 'S',
                OrderBookUpdateKind::Begin => 'B',
                OrderBookUpdateKind::Middle => 'M',
                OrderBookUpdateKind::End => 'E',
                OrderBookUpdateKind::Solo => 'O',
            };
            let _ = write!(
                self.sequence,
                "{letter}{}/{}z{} ",
                update.bids.len(),
                update.asks.len(),
                zero
            );
        }
        self.upsert.apply(update, false);
        self.snapshot_replaces.apply(update, true);
        if !matches!(
            update.kind,
            OrderBookUpdateKind::Begin | OrderBookUpdateKind::Middle
        ) {
            self.upsert.score(self.best_bid, self.best_ask);
            self.snapshot_replaces.score(self.best_bid, self.best_ask);
        }
    }

    pub(super) fn record_quote(&mut self, quote: &QuoteUpdate) {
        self.quote_frames = self.quote_frames.saturating_add(1);
        if quote.is_snapshot {
            self.best_bid = None;
            self.best_ask = None;
        }
        apply_quote_side(&mut self.best_bid, &quote.bid);
        apply_quote_side(&mut self.best_ask, &quote.ask);
    }

    pub(super) const fn book_frames(&self) -> usize {
        self.frames
    }

    pub(super) fn print(&self) {
        println!(
            "rithmic_mbp_frames total={} {} quote_frames={}",
            self.frames, self.kinds, self.quote_frames
        );
        println!("rithmic_mbp_zero_size_levels {}", self.zero_size_levels);
        println!(
            "rithmic_mbp_groups unbalanced={} open_at_end={} frames_before_first_solo={}",
            self.unbalanced_groups,
            self.open_group,
            self.frames_before_first_solo
                .map_or_else(|| "-".to_string(), |value| value.to_string())
        );
        println!("rithmic_mbp_prefix {}", self.sequence.trim_end());
        println!("rithmic_mbp_upsert_book {}", self.upsert);
        println!(
            "rithmic_mbp_snapshot_replaces_book {}",
            self.snapshot_replaces
        );
    }
}

fn apply_quote_side(state: &mut Option<i64>, side: &QuoteSideUpdate) {
    match side {
        QuoteSideUpdate::Unchanged => {}
        QuoteSideUpdate::Cleared => *state = None,
        QuoteSideUpdate::Value(level) if level.size == 0 => *state = None,
        QuoteSideUpdate::Value(level) => *state = Some(price_key(level.price)),
    }
}
