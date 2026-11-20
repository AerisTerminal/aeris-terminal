use std::collections::BTreeMap;

use aeris_market_data::AggressorTradeVolumes;

use crate::OrderBookFrame;
use crate::order_book::compact_quantity_text;

use super::layout::{ceil_price_to_tick, floor_price_to_tick, price_grid_layout};

/// The side whose aggressors dominated the volume traded at one price.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TradeImbalance {
    Buyers,
    Sellers,
}

pub(super) fn trade_imbalance(
    volumes: AggressorTradeVolumes,
    ratio: Option<u32>,
) -> Option<TradeImbalance> {
    let ratio = i128::from(ratio?);
    if volumes.buy <= 0 || volumes.sell <= 0 {
        return None;
    }
    let buy = i128::from(volumes.buy);
    let sell = i128::from(volumes.sell);
    if buy >= sell * ratio {
        Some(TradeImbalance::Buyers)
    } else if sell >= buy * ratio {
        Some(TradeImbalance::Sellers)
    } else {
        None
    }
}

pub(super) fn is_large_lot(
    quantity: Option<i64>,
    quantity_scale: u8,
    threshold: Option<u32>,
) -> bool {
    let (Some(quantity), Some(threshold)) = (quantity, threshold) else {
        return false;
    };
    10_i128
        .checked_pow(u32::from(quantity_scale))
        .and_then(|unit| unit.checked_mul(i128::from(threshold)))
        .is_some_and(|minimum| i128::from(quantity) >= minimum)
}

/// Buy-aggressor minus sell-aggressor volume, the Delta column's value.
pub(super) const fn price_delta(volumes: AggressorTradeVolumes) -> i64 {
    volumes.buy.saturating_sub(volumes.sell)
}

pub(super) const fn price_volume(volumes: AggressorTradeVolumes) -> i64 {
    volumes.buy.saturating_add(volumes.sell)
}

/// The ladder row holding the most traded volume of the retained tape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct VolumeProfilePeak {
    pub(super) price: i64,
    pub(super) total: i64,
}

/// The point of control on the rows the ladder shows: price-grid rows group volume
/// exactly as `grouped_trade_volumes_at_price` does, and the real-level ladder reads
/// each exact price.
pub(super) fn volume_profile_peak(frame: &OrderBookFrame) -> Option<VolumeProfilePeak> {
    let tick = price_grid_layout(frame).map_or(1, |grid| grid.tick);
    grouped_volume_peak(&frame.traded_volumes, tick)
}

/// Sells group down and buys group up to their row. Both projections stay in
/// ascending row order, so one merge visits each row once; a tie keeps the lower price.
pub(super) fn grouped_volume_peak(
    traded_volumes: &BTreeMap<i64, AggressorTradeVolumes>,
    tick: i64,
) -> Option<VolumeProfilePeak> {
    let mut sells = traded_volumes
        .iter()
        .filter(|(_, volumes)| volumes.sell > 0)
        .filter_map(|(price, volumes)| Some((floor_price_to_tick(*price, tick)?, volumes.sell)))
        .peekable();
    let mut buys = traded_volumes
        .iter()
        .filter(|(_, volumes)| volumes.buy > 0)
        .filter_map(|(price, volumes)| Some((ceil_price_to_tick(*price, tick)?, volumes.buy)))
        .peekable();
    let mut peak: Option<VolumeProfilePeak> = None;
    let mut row: Option<VolumeProfilePeak> = None;
    loop {
        let sell_first = match (sells.peek(), buys.peek()) {
            (Some(sell), Some(buy)) => sell.0 <= buy.0,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        let next = if sell_first {
            sells.next()
        } else {
            buys.next()
        };
        let Some((price, volume)) = next else {
            break;
        };
        if let Some(current) = row.as_mut().filter(|current| current.price == price) {
            current.total = current.total.saturating_add(volume);
        } else if let Some(finished) = row.replace(VolumeProfilePeak {
            price,
            total: volume,
        }) {
            peak = Some(higher_peak(peak, finished));
        }
    }
    row.map_or(peak, |finished| Some(higher_peak(peak, finished)))
}

fn higher_peak(peak: Option<VolumeProfilePeak>, row: VolumeProfilePeak) -> VolumeProfilePeak {
    match peak {
        Some(peak) if peak.total >= row.total => peak,
        _ => row,
    }
}

pub(super) struct DepthTotals {
    pub(super) bid_pct: i128,
    pub(super) ask_pct: i128,
    pub(super) bid_ratio: f32,
    pub(super) ask_ratio: f32,
    pub(super) bid_total_text: String,
    pub(super) ask_total_text: String,
}

pub(super) fn depth_totals(frame: &OrderBookFrame) -> Option<DepthTotals> {
    let mut total_bid: i128 = 0;
    let mut total_ask: i128 = 0;
    for row in &frame.rows {
        if let Some(quantity) = row.bid.as_ref().and_then(|level| level.quantity)
            && quantity > 0
        {
            total_bid = total_bid.saturating_add(i128::from(quantity));
        }
        if let Some(quantity) = row.ask.as_ref().and_then(|level| level.quantity)
            && quantity > 0
        {
            total_ask = total_ask.saturating_add(i128::from(quantity));
        }
    }
    let combined = total_bid.saturating_add(total_ask);
    if combined <= 0 {
        return None;
    }
    let bid_basis_points = total_bid
        .saturating_mul(10_000)
        .checked_div(combined)
        .unwrap_or(5_000)
        .clamp(0, 10_000);
    let bid_pct = (bid_basis_points + 50) / 100;
    let bid_ratio = f32::from(u16::try_from(bid_basis_points).unwrap_or(5_000)) / 10_000.0;
    Some(DepthTotals {
        bid_pct,
        ask_pct: 100 - bid_pct,
        bid_ratio,
        ask_ratio: (1.0 - bid_ratio).clamp(0.0, 1.0),
        bid_total_text: compact_quantity_text(
            i64::try_from(total_bid).unwrap_or(i64::MAX),
            frame.quantity_scale,
        ),
        ask_total_text: compact_quantity_text(
            i64::try_from(total_ask).unwrap_or(i64::MAX),
            frame.quantity_scale,
        ),
    })
}
