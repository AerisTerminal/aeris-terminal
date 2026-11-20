#[cfg(test)]
use std::cmp::Ordering;

use aeris_market_data::AggressorTradeVolumes;

use crate::order_book::{compact_quantity_text, grouped_fixed_point_text};
use crate::{OrderBookColumnLevel, OrderBookFrame};

use super::{
    BookColumnSide, HYPERLIQUID_DISPLAY_SIGNIFICANT_FIGURES, MAXIMUM_PRICE_GRID_ROWS,
    MAXIMUM_TRADE_PRICES_FOR_GRID_INFERENCE, MINIMUM_PRICE_GRID_ROWS_PER_SIDE,
};

pub(super) fn ladder_recenter_index(frame: &OrderBookFrame) -> Option<usize> {
    price_grid_layout(frame)
        .map(PriceGridLayout::recenter_index)
        .or_else(|| ladder_layout(frame).recenter_index())
}

/// Presentation-only fixed-tick layout. Prices without provider depth become
/// blank price rows; quantities/orders are looked up only from the immutable
/// real frame. If any real price is off the provider-declared lattice, fail
/// closed to the existing real-level ladder instead of snapping it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PriceGridLayout {
    pub(super) best_bid: i64,
    pub(super) tick: i64,
    pub(super) ask_rows: usize,
    pub(super) bid_rows: usize,
}

impl PriceGridLayout {
    pub(super) const fn item_count(self) -> usize {
        self.ask_rows + 1 + self.bid_rows
    }

    pub(super) const fn recenter_index(self) -> usize {
        self.ask_rows
    }
}

pub(super) fn price_grid_layout(frame: &OrderBookFrame) -> Option<PriceGridLayout> {
    let tick = if frame.provider_id == "hyperliquid" {
        hyperliquid_display_tick(frame)?
    } else {
        frame
            .price_increment
            .filter(|increment| *increment > 0)
            .or_else(|| observed_price_increment(frame))?
    };
    let best_bid = price_grid_anchor_best_bid(frame, tick)?;
    let ask_start = best_bid.checked_add(tick)?;

    let mut bid_rows = 1usize;
    let mut ask_rows = 1usize;

    if let Some(best_ask) = display_best_ask(frame).map(|level| level.price) {
        let best_ask = ceil_price_to_tick(best_ask, tick)?;
        if best_ask < ask_start {
            return None;
        }
        ask_rows = ask_rows.max(
            usize::try_from((best_ask - ask_start) / tick)
                .ok()?
                .saturating_add(1),
        );
    }

    for level in frame.rows.iter().filter_map(|row| row.bid.as_ref()) {
        let price = floor_price_to_tick(level.price, tick)?;
        if price > best_bid {
            return None;
        }
        let distance = best_bid.checked_sub(price)?;
        bid_rows = bid_rows.max(usize::try_from(distance / tick).ok()?.saturating_add(1));
    }
    for level in frame.rows.iter().filter_map(|row| row.ask.as_ref()) {
        let price = ceil_price_to_tick(level.price, tick)?;
        if price < ask_start {
            return None;
        }
        let distance = price.checked_sub(ask_start)?;
        ask_rows = ask_rows.max(usize::try_from(distance / tick).ok()?.saturating_add(1));
    }
    // Grid rows are presentation ticks, but their prices still obey the
    // domain's positive-price invariant. Cap the empty runway at the natural
    // numeric boundary so `uniform_list` never receives an index that cannot
    // map to a concrete display price.
    let bid_capacity = usize::try_from((best_bid - 1) / tick)
        .ok()?
        .saturating_add(1);
    let ask_capacity = usize::try_from((i64::MAX - ask_start) / tick)
        .ok()?
        .saturating_add(1);
    bid_rows = bid_rows
        .max(MINIMUM_PRICE_GRID_ROWS_PER_SIDE)
        .min(bid_capacity);
    ask_rows = ask_rows
        .max(MINIMUM_PRICE_GRID_ROWS_PER_SIDE)
        .min(ask_capacity);
    let item_count = ask_rows.checked_add(1)?.checked_add(bid_rows)?;
    if item_count > MAXIMUM_PRICE_GRID_ROWS {
        return None;
    }
    Some(PriceGridLayout {
        best_bid,
        tick,
        ask_rows,
        bid_rows,
    })
}

fn hyperliquid_display_tick(frame: &OrderBookFrame) -> Option<i64> {
    let price = display_best_bid(frame)
        .or_else(|| display_best_ask(frame))
        .map(|level| level.price)
        .or_else(|| {
            frame
                .traded_volumes
                .last_key_value()
                .map(|(price, _)| *price)
        })?;
    if price <= 0 {
        return None;
    }
    let digits = price.ilog10().saturating_add(1);
    10_i64.checked_pow(digits.saturating_sub(HYPERLIQUID_DISPLAY_SIGNIFICANT_FIGURES))
}

pub(super) fn floor_price_to_tick(price: i64, tick: i64) -> Option<i64> {
    if price <= 0 || tick <= 0 {
        return None;
    }
    price.div_euclid(tick).checked_mul(tick)
}

pub(super) fn ceil_price_to_tick(price: i64, tick: i64) -> Option<i64> {
    let floor = floor_price_to_tick(price, tick)?;
    if floor == price {
        Some(price)
    } else {
        floor.checked_add(tick)
    }
}

pub(super) fn observed_price_increment(frame: &OrderBookFrame) -> Option<i64> {
    let mut anchor = None;
    let mut increment = 0;
    for price in frame
        .best_bid
        .iter()
        .chain(frame.best_ask.iter())
        .map(|level| level.price)
        .chain(
            frame
                .rows
                .iter()
                .flat_map(|row| row.bid.iter().chain(row.ask.iter()))
                .map(|level| level.price),
        )
    {
        observe_price_increment(&mut anchor, &mut increment, price);
    }
    // Depth is authoritative for the presentation lattice. Trade retention can
    // contain tens of thousands of distinct prices, so scanning the whole map on
    // every 16 ms presentation frame is both unnecessary and expensive. Only use
    // a bounded trade sample when depth cannot establish a lattice at all (for
    // example, during a trade-only recovery frame).
    if increment == 0 {
        let sample_per_edge = MAXIMUM_TRADE_PRICES_FOR_GRID_INFERENCE / 2;
        for price in frame
            .traded_volumes
            .keys()
            .take(sample_per_edge)
            .chain(frame.traded_volumes.keys().rev().take(sample_per_edge))
            .copied()
        {
            observe_price_increment(&mut anchor, &mut increment, price);
        }
    }
    (increment > 0).then_some(increment)
}

fn observe_price_increment(anchor: &mut Option<i64>, increment: &mut i64, price: i64) {
    if price <= 0 {
        return;
    }
    let Some(anchor_price) = *anchor else {
        *anchor = Some(price);
        return;
    };
    let gap = (price - anchor_price).abs();
    if gap == 0 {
        return;
    }
    *increment = if *increment == 0 {
        gap
    } else {
        greatest_common_divisor(*increment, gap)
    };
}

const fn greatest_common_divisor(mut left: i64, mut right: i64) -> i64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.abs()
}

/// Flowsurface keeps the ladder usable with only one depth side, and can even
/// anchor from retained trades while a covering depth image is absent. Mirror
/// that presentation behavior without promoting quotes/trades into canonical
/// depth. Every generated row still uses the provider-declared increment.
fn price_grid_anchor_best_bid(frame: &OrderBookFrame, tick: i64) -> Option<i64> {
    if let Some(best_bid) = display_best_bid(frame).map(|level| level.price) {
        return floor_price_to_tick(best_bid, tick).filter(|price| *price > 0);
    }
    if let Some(best_ask) = display_best_ask(frame).map(|level| level.price) {
        return ceil_price_to_tick(best_ask, tick)?
            .checked_sub(tick)
            .filter(|price| *price > 0);
    }

    let (&minimum_trade, _) = frame.traded_volumes.first_key_value()?;
    let (&maximum_trade, _) = frame.traded_volumes.last_key_value()?;
    let distance = maximum_trade.checked_sub(minimum_trade)?;
    if minimum_trade <= 0 || distance % tick != 0 {
        return None;
    }
    let inclusive_steps = distance.checked_div(tick)?.checked_add(1)?;
    floor_price_to_tick(
        maximum_trade
            .checked_sub(tick.checked_mul(inclusive_steps / 2)?)
            .filter(|price| *price > 0)?,
        tick,
    )
}

pub(super) fn display_best_bid(frame: &OrderBookFrame) -> Option<&OrderBookColumnLevel> {
    frame
        .rows
        .iter()
        .find_map(|row| row.bid.as_ref())
        .or(frame.best_bid.as_ref())
}

pub(super) fn display_best_ask(frame: &OrderBookFrame) -> Option<&OrderBookColumnLevel> {
    frame
        .rows
        .iter()
        .find_map(|row| row.ask.as_ref())
        .or(frame.best_ask.as_ref())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PriceGridItem {
    Ask(i64),
    Spread,
    Bid(i64),
}

pub(super) fn price_grid_item(layout: PriceGridLayout, index: usize) -> Option<PriceGridItem> {
    if index >= layout.item_count() {
        return None;
    }
    if index < layout.ask_rows {
        let steps_from_ask_start = layout.ask_rows.checked_sub(index + 1)?;
        let steps = i64::try_from(steps_from_ask_start).ok()?;
        let price = layout
            .best_bid
            .checked_add(layout.tick)?
            .checked_add(layout.tick.checked_mul(steps)?)?;
        return Some(PriceGridItem::Ask(price));
    }
    if index == layout.ask_rows {
        return Some(PriceGridItem::Spread);
    }
    let steps = i64::try_from(index.checked_sub(layout.ask_rows + 1)?).ok()?;
    let price = layout
        .best_bid
        .checked_sub(layout.tick.checked_mul(steps)?)?;
    Some(PriceGridItem::Bid(price))
}

#[cfg(test)]
pub(super) fn real_level_at_price(
    frame: &OrderBookFrame,
    side: BookColumnSide,
    price: i64,
) -> Option<&OrderBookColumnLevel> {
    let index = frame
        .rows
        .binary_search_by(|row| match side {
            // Projection stores asks best-first in ascending price order; a
            // missing tail entry sorts after every concrete price.
            BookColumnSide::Ask => row
                .ask
                .as_ref()
                .map_or(Ordering::Greater, |level| level.price.cmp(&price)),
            // Bids are best-first in descending price order. Reverse the
            // comparison so binary_search still observes Less..Equal..Greater.
            BookColumnSide::Bid => row
                .bid
                .as_ref()
                .map_or(Ordering::Greater, |level| price.cmp(&level.price)),
        })
        .ok()?;
    match side {
        BookColumnSide::Bid => frame.rows.get(index)?.bid.as_ref(),
        BookColumnSide::Ask => frame.rows.get(index)?.ask.as_ref(),
    }
}

pub(super) fn grouped_level_at_price(
    frame: &OrderBookFrame,
    side: BookColumnSide,
    price: i64,
    tick: i64,
) -> Option<OrderBookColumnLevel> {
    let mut quantity = 0i64;
    let mut order_count = Some(0u32);
    let mut found = false;
    for level in frame.rows.iter().filter_map(|row| match side {
        BookColumnSide::Bid => row.bid.as_ref(),
        BookColumnSide::Ask => row.ask.as_ref(),
    }) {
        let grouped_price = match side {
            BookColumnSide::Bid => floor_price_to_tick(level.price, tick),
            BookColumnSide::Ask => ceil_price_to_tick(level.price, tick),
        }?;
        if grouped_price != price {
            continue;
        }
        found = true;
        quantity = quantity.saturating_add(level.quantity?);
        order_count = match (order_count, level.order_count) {
            (Some(total), Some(count)) => Some(total.saturating_add(count)),
            _ => None,
        };
    }
    found.then(|| OrderBookColumnLevel {
        price,
        quantity: Some(quantity),
        order_count,
        price_text: grouped_fixed_point_text(price, frame.price_scale),
        quantity_text: compact_quantity_text(quantity, frame.quantity_scale),
        traded_volume: 0,
        traded_volume_text: String::new(),
        relative_size_bps: 0,
    })
}

pub(super) fn grouped_trade_volumes_at_price(
    frame: &OrderBookFrame,
    price: i64,
    tick: i64,
) -> AggressorTradeVolumes {
    if tick <= 0 {
        return AggressorTradeVolumes::default();
    }
    let mut grouped = AggressorTradeVolumes::default();
    if let Some(sell_end) = price.checked_add(tick.saturating_sub(1)) {
        for volumes in frame
            .traded_volumes
            .range(price..=sell_end)
            .map(|(_, volumes)| *volumes)
        {
            grouped.sell = grouped.sell.saturating_add(volumes.sell);
        }
    }
    if let Some(buy_start) = price
        .checked_sub(tick)
        .and_then(|value| value.checked_add(1))
    {
        for volumes in frame
            .traded_volumes
            .range(buy_start..=price)
            .map(|(_, volumes)| *volumes)
        {
            grouped.buy = grouped.buy.saturating_add(volumes.buy);
        }
    }
    grouped
}

pub(super) fn price_grid_visible_max_quantity(
    frame: &OrderBookFrame,
    layout: PriceGridLayout,
    range: std::ops::Range<usize>,
) -> i64 {
    range
        .filter_map(|index| match price_grid_item(layout, index)? {
            PriceGridItem::Ask(price) => {
                grouped_level_at_price(frame, BookColumnSide::Ask, price, layout.tick)?.quantity
            }
            PriceGridItem::Bid(price) => {
                grouped_level_at_price(frame, BookColumnSide::Bid, price, layout.tick)?.quantity
            }
            PriceGridItem::Spread => None,
        })
        .max()
        .unwrap_or(0)
}

pub(super) fn price_grid_visible_max_trade_quantity(
    frame: &OrderBookFrame,
    layout: PriceGridLayout,
    range: std::ops::Range<usize>,
) -> i64 {
    range
        .filter_map(|index| match price_grid_item(layout, index)? {
            PriceGridItem::Ask(price) | PriceGridItem::Bid(price) => {
                Some(grouped_trade_volumes_at_price(frame, price, layout.tick).maximum_side())
            }
            PriceGridItem::Spread => None,
        })
        .max()
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LadderLayout {
    pub(super) ask_count: usize,
    pub(super) bid_count: usize,
    pub(super) spread: bool,
}

impl LadderLayout {
    pub(super) const fn item_count(self) -> usize {
        self.ask_count + self.bid_count + self.spread as usize
    }

    pub(super) const fn recenter_index(self) -> Option<usize> {
        if self.spread {
            Some(self.ask_count)
        } else if self.ask_count > 0 {
            Some(self.ask_count - 1)
        } else if self.bid_count > 0 {
            Some(0)
        } else {
            None
        }
    }
}

pub(super) fn ladder_layout(frame: &OrderBookFrame) -> LadderLayout {
    LadderLayout {
        ask_count: frame.rows.iter().filter(|row| row.ask.is_some()).count(),
        bid_count: frame.rows.iter().filter(|row| row.bid.is_some()).count(),
        spread: display_best_bid(frame).is_some() && display_best_ask(frame).is_some(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LadderItemIndex {
    Ask(usize),
    Spread,
    Bid(usize),
}

pub(super) fn ladder_item_index(layout: LadderLayout, index: usize) -> Option<LadderItemIndex> {
    if index < layout.ask_count {
        return Some(LadderItemIndex::Ask(layout.ask_count - index - 1));
    }
    let after_asks = index - layout.ask_count;
    if layout.spread {
        if after_asks == 0 {
            return Some(LadderItemIndex::Spread);
        }
        let bid_index = after_asks - 1;
        return if bid_index < layout.bid_count {
            Some(LadderItemIndex::Bid(bid_index))
        } else {
            None
        };
    }
    if after_asks < layout.bid_count {
        Some(LadderItemIndex::Bid(after_asks))
    } else {
        None
    }
}

pub(super) fn ladder_visible_max_quantity(
    frame: &OrderBookFrame,
    layout: LadderLayout,
    range: std::ops::Range<usize>,
) -> i64 {
    range
        .filter_map(|index| match ladder_item_index(layout, index)? {
            LadderItemIndex::Ask(row_index) => frame.rows.get(row_index)?.ask.as_ref(),
            LadderItemIndex::Bid(row_index) => frame.rows.get(row_index)?.bid.as_ref(),
            LadderItemIndex::Spread => None,
        })
        .filter_map(|level| level.quantity)
        .max()
        .unwrap_or(0)
}

pub(super) fn ladder_visible_max_trade_quantity(
    frame: &OrderBookFrame,
    layout: LadderLayout,
    range: std::ops::Range<usize>,
) -> i64 {
    range
        .filter_map(|index| match ladder_item_index(layout, index)? {
            LadderItemIndex::Ask(row_index) => frame.rows.get(row_index)?.ask.as_ref(),
            LadderItemIndex::Bid(row_index) => frame.rows.get(row_index)?.bid.as_ref(),
            LadderItemIndex::Spread => None,
        })
        .filter_map(|level| frame.traded_volumes.get(&level.price).copied())
        .map(AggressorTradeVolumes::maximum_side)
        .max()
        .unwrap_or(0)
}
