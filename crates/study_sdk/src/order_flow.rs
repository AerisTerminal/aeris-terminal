//! Canonical bounded order-flow footprint and volume-profile primitives for Study SDK authors.
//!
//! All aggregations operate deterministically over borrowed canonical `StudyTradeWindow`
//! samples using exact fixed-point integer prices and volumes. No provider types or
//! secondary market buffers are created.

use crate::{AggressorSide, StudyTradeSample, StudyTradeWindow};
use std::collections::BTreeMap;

/// Hard bound on distinct price levels retained in one footprint bar.
pub const MAXIMUM_FOOTPRINT_LEVELS: usize = 256;
/// Hard bound on distinct price bins retained in one volume profile.
pub const MAXIMUM_PROFILE_BINS: usize = 512;

/// One fixed-point price level inside a [`StudyFootprintBar`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StudyFootprintLevel {
    pub price_ticks: i64,
    pub buy_volume: i64,
    pub sell_volume: i64,
    pub trade_count: u32,
}

impl StudyFootprintLevel {
    /// Returns total volume traded at this level (`buy_volume + sell_volume`).
    #[must_use]
    pub const fn total_volume(self) -> i64 {
        self.buy_volume.saturating_add(self.sell_volume)
    }

    /// Returns signed aggressor delta (`buy_volume - sell_volume`).
    #[must_use]
    pub const fn delta(self) -> i64 {
        self.buy_volume.saturating_sub(self.sell_volume)
    }
}

/// Bounded bid/ask volume-at-price footprint summary for one bar interval.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StudyFootprintBar {
    pub levels: BTreeMap<i64, StudyFootprintLevel>,
    pub total_buy: i64,
    pub total_sell: i64,
    pub poc_price_ticks: Option<i64>,
}

impl StudyFootprintBar {
    /// Returns signed bar delta (`total_buy - total_sell`).
    #[must_use]
    pub const fn net_delta(&self) -> i64 {
        self.total_buy.saturating_sub(self.total_sell)
    }

    /// Detects classic diagonal buy/sell imbalances where one side exceeds the diagonally
    /// adjacent opposite side by at least `ratio_percent` (e.g. `300` for 3:1).
    ///
    /// Returns `(price_ticks, is_buy_imbalance)` pairs sorted by price.
    #[must_use]
    pub fn diagonal_imbalances(&self, ratio_percent: u32) -> Vec<(i64, bool)> {
        if ratio_percent < 100 {
            return Vec::new();
        }
        let scale = i128::from(ratio_percent);
        let mut out = Vec::new();
        for (&price, level) in &self.levels {
            if let Some(lower) = self.levels.get(&(price.saturating_sub(1))) {
                let buy = i128::from(level.buy_volume.max(0));
                let diag_sell = i128::from(lower.sell_volume.max(0));
                if buy > 0 && buy * 100 >= diag_sell * scale {
                    out.push((price, true));
                }
            }
            if let Some(upper) = self.levels.get(&(price.saturating_add(1))) {
                let sell_vol = i128::from(level.sell_volume.max(0));
                let diag_buy = i128::from(upper.buy_volume.max(0));
                if sell_vol > 0 && sell_vol * 100 >= diag_buy * scale {
                    out.push((price, false));
                }
            }
        }
        out
    }
}

/// Bounded volume-at-price profile with Point of Control (POC) and Value Area bounds.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StudyVolumeProfile {
    pub bins: BTreeMap<i64, i64>,
    pub total_volume: i64,
    pub poc_price_ticks: Option<i64>,
    pub value_area_low: Option<i64>,
    pub value_area_high: Option<i64>,
}

/// Aggregates a bounded [`StudyFootprintBar`] from a canonical [`StudyTradeWindow`] over
/// `[start_unix_nanos, end_unix_nanos_exclusive)` at fixed-point `tick_size > 0`.
///
/// # Errors
/// Returns an error if `tick_size <= 0` or the distinct price level count exceeds
/// [`MAXIMUM_FOOTPRINT_LEVELS`].
pub fn build_footprint_from_window(
    trades: StudyTradeWindow<'_>,
    tick_size: i64,
    start_unix_nanos: i64,
    end_unix_nanos_exclusive: i64,
) -> Result<StudyFootprintBar, String> {
    build_footprint_from_samples(
        trades.iter().filter(|t| {
            t.observed_unix_nanos >= start_unix_nanos
                && t.observed_unix_nanos < end_unix_nanos_exclusive
        }),
        tick_size,
    )
}

/// Aggregates a bounded [`StudyFootprintBar`] from any iterator of [`StudyTradeSample`].
///
/// # Errors
/// Returns an error if `tick_size <= 0` or the distinct price level count exceeds
/// [`MAXIMUM_FOOTPRINT_LEVELS`].
pub fn build_footprint_from_samples(
    samples: impl IntoIterator<Item = StudyTradeSample>,
    tick_size: i64,
) -> Result<StudyFootprintBar, String> {
    if tick_size <= 0 {
        return Err("footprint tick_size must be positive".to_string());
    }
    let mut bar = StudyFootprintBar::default();
    for trade in samples {
        if trade.quantity <= 0 {
            continue;
        }
        let price_ticks = trade.price.div_euclid(tick_size);
        if !bar.levels.contains_key(&price_ticks) && bar.levels.len() >= MAXIMUM_FOOTPRINT_LEVELS {
            return Err(format!(
                "footprint exceeded maximum of {MAXIMUM_FOOTPRINT_LEVELS} price levels"
            ));
        }
        let entry = bar
            .levels
            .entry(price_ticks)
            .or_insert_with(|| StudyFootprintLevel {
                price_ticks,
                buy_volume: 0,
                sell_volume: 0,
                trade_count: 0,
            });
        match trade.aggressor {
            AggressorSide::Buy => {
                entry.buy_volume = entry.buy_volume.saturating_add(trade.quantity);
                bar.total_buy = bar.total_buy.saturating_add(trade.quantity);
            }
            AggressorSide::Sell => {
                entry.sell_volume = entry.sell_volume.saturating_add(trade.quantity);
                bar.total_sell = bar.total_sell.saturating_add(trade.quantity);
            }
            AggressorSide::Unknown => {
                let half = trade.quantity / 2;
                let rem = trade.quantity - half;
                entry.buy_volume = entry.buy_volume.saturating_add(rem);
                entry.sell_volume = entry.sell_volume.saturating_add(half);
                bar.total_buy = bar.total_buy.saturating_add(rem);
                bar.total_sell = bar.total_sell.saturating_add(half);
            }
        }
        entry.trade_count = entry.trade_count.saturating_add(1);
    }
    bar.poc_price_ticks = bar
        .levels
        .values()
        .max_by_key(|lvl| (lvl.total_volume(), -lvl.price_ticks))
        .map(|lvl| lvl.price_ticks);
    Ok(bar)
}

/// Aggregates a bounded [`StudyVolumeProfile`] with POC and Value Area (`value_area_percent`
/// clamped to `1..=100`, typically `70`) from any iterator of [`StudyTradeSample`].
///
/// # Errors
/// Returns an error if `tick_size <= 0` or the distinct bin count exceeds [`MAXIMUM_PROFILE_BINS`].
pub fn build_volume_profile_from_samples(
    samples: impl IntoIterator<Item = StudyTradeSample>,
    tick_size: i64,
    value_area_percent: u8,
) -> Result<StudyVolumeProfile, String> {
    if tick_size <= 0 {
        return Err("volume profile tick_size must be positive".to_string());
    }
    let mut profile = StudyVolumeProfile::default();
    for trade in samples {
        if trade.quantity <= 0 {
            continue;
        }
        let price_ticks = trade.price.div_euclid(tick_size);
        if !profile.bins.contains_key(&price_ticks) && profile.bins.len() >= MAXIMUM_PROFILE_BINS {
            return Err(format!(
                "volume profile exceeded maximum of {MAXIMUM_PROFILE_BINS} bins"
            ));
        }
        let vol = profile.bins.entry(price_ticks).or_insert(0);
        *vol = vol.saturating_add(trade.quantity);
        profile.total_volume = profile.total_volume.saturating_add(trade.quantity);
    }
    if profile.bins.is_empty() {
        return Ok(profile);
    }
    let sorted: Vec<(i64, i64)> = profile.bins.iter().map(|(&p, &v)| (p, v)).collect();
    let Some((poc_idx, &(poc_price, poc_vol))) = sorted
        .iter()
        .enumerate()
        .max_by_key(|&(_, &(p, v))| (v, -p))
    else {
        return Ok(profile);
    };
    profile.poc_price_ticks = Some(poc_price);

    let pct = i128::from(value_area_percent.clamp(1, 100));
    let target = (i128::from(profile.total_volume) * pct + 99) / 100;
    let mut accumulated = i128::from(poc_vol);
    let mut low_idx = poc_idx;
    let mut high_idx = poc_idx;
    while accumulated < target && (low_idx > 0 || high_idx + 1 < sorted.len()) {
        let down_vol = if low_idx > 0 {
            sorted[low_idx - 1].1
        } else {
            -1
        };
        let up_vol = if high_idx + 1 < sorted.len() {
            sorted[high_idx + 1].1
        } else {
            -1
        };
        if up_vol >= down_vol && high_idx + 1 < sorted.len() {
            high_idx += 1;
            accumulated += i128::from(sorted[high_idx].1);
        } else if low_idx > 0 {
            low_idx -= 1;
            accumulated += i128::from(sorted[low_idx].1);
        } else {
            break;
        }
    }
    profile.value_area_low = Some(sorted[low_idx].0);
    profile.value_area_high = Some(sorted[high_idx].0);
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(ts: i64, price: i64, qty: i64, side: AggressorSide) -> StudyTradeSample {
        StudyTradeSample {
            observed_unix_nanos: ts,
            price,
            quantity: qty,
            aggressor: side,
        }
    }

    #[test]
    fn footprint_computes_delta_poc_and_diagonal_imbalances() {
        let samples = [
            trade(1, 1000, 10, AggressorSide::Sell),
            trade(2, 1025, 40, AggressorSide::Buy),
            trade(3, 1025, 20, AggressorSide::Sell),
            trade(4, 1050, 5, AggressorSide::Buy),
        ];
        let fp = build_footprint_from_samples(samples, 25).expect("footprint builds");
        assert_eq!(fp.total_buy, 45);
        assert_eq!(fp.total_sell, 30);
        assert_eq!(fp.net_delta(), 15);
        assert_eq!(fp.poc_price_ticks, Some(41)); // 1025 / 25 = 41
        let imbalances = fp.diagonal_imbalances(300);
        assert!(imbalances.contains(&(41, true)));
    }

    #[test]
    fn volume_profile_computes_poc_and_70_percent_value_area() {
        let samples = [
            trade(1, 100, 10, AggressorSide::Buy),
            trade(2, 101, 50, AggressorSide::Buy),
            trade(3, 102, 30, AggressorSide::Sell),
            trade(4, 103, 10, AggressorSide::Sell),
        ];
        let vp = build_volume_profile_from_samples(samples, 1, 70).expect("profile builds");
        assert_eq!(vp.total_volume, 100);
        assert_eq!(vp.poc_price_ticks, Some(101));
        assert_eq!(vp.value_area_low, Some(101));
        assert_eq!(vp.value_area_high, Some(102));
    }
}
