//! Canonical bar resampling for derived chart timeframes (e.g. `2m`, `10m`, `45m`, `3h`, `6h`,
//! `2D`, `5D`, `2W`, `3M`, `6M`, `12M`) on providers that expose a finer native base cadence.
//!
//! Preserves exact OHLCV aggregation, monotonic UTC bucket alignment, contiguous sequence numbers,
//! and forming-candle handoff invariants without duplicating provider sessions.

use std::collections::BTreeMap;

use aeris_market_data::{BarPeriod, MarketBar};
use aeris_market_runtime::plugin::{HistoryRequest, HistorySnapshot};
use aeris_provider_spi::FormingBar;

const NANOS_PER_SECOND: i64 = 1_000_000_000;
const SECONDS_PER_DAY: i64 = 86_400;
const NANOS_PER_DAY: i64 = SECONDS_PER_DAY * NANOS_PER_SECOND;
/// Maximum base-period bars requested when deriving one resampled history window.
const MAXIMUM_RESAMPLE_BASE_BARS: usize = 5_000;

/// Full TradingView-class crypto interval catalog offered across providers whose native REST
/// endpoints cover at least `1m`, `5m`, `15m`, `1h`, and `1D` (derived via [`resample_snapshot`]).
pub(crate) const DERIVED_CRYPTO_INTERVAL_LABELS: &[&str] = &[
    "1m", "2m", "3m", "5m", "10m", "15m", "30m", "45m", "1h", "2h", "3h", "4h", "6h", "8h", "12h",
    "1D", "2D", "3D", "5D", "1W", "2W", "1M", "3M", "6M", "12M",
];

/// Chooses the coarsest native base period in `native_seconds` that cleanly aggregates into
/// `target`, along with the integer expansion multiplier for scaling the base bar limit.
#[must_use]
pub(crate) fn select_base_period(
    target: BarPeriod,
    native_seconds: &[u32],
) -> Option<(BarPeriod, usize)> {
    match target {
        BarPeriod::Time { seconds } => {
            let best = native_seconds
                .iter()
                .copied()
                .filter(|&s| s > 0 && s < seconds && seconds.is_multiple_of(s))
                .max()?;
            let ratio = (seconds / best) as usize;
            Some((BarPeriod::time(best).ok()?, ratio.max(1)))
        }
        BarPeriod::Session { days } => {
            let secs = days.checked_mul(86_400)?;
            let best = native_seconds
                .iter()
                .copied()
                .filter(|&s| s > 0 && s <= secs && secs.is_multiple_of(s))
                .max()?;
            let ratio = (secs / best) as usize;
            Some((BarPeriod::time(best).ok()?, ratio.max(1)))
        }
        BarPeriod::Week { weeks } => {
            if native_seconds.contains(&86_400) {
                Some((BarPeriod::time(86_400).ok()?, (weeks as usize).saturating_mul(7).max(1)))
            } else {
                None
            }
        }
        BarPeriod::Month { months } => {
            if native_seconds.contains(&86_400) {
                Some((BarPeriod::time(86_400).ok()?, (months as usize).saturating_mul(31).max(1)))
            } else {
                None
            }
        }
        BarPeriod::Tick { .. } => None,
    }
}

/// Returns whether `period` is either natively supported by `native_seconds` or cleanly derivable
/// via [`select_base_period`].
pub(crate) fn supports_or_derives(period: BarPeriod, native_seconds: &[u32]) -> Result<(), String> {
    if let BarPeriod::Time { seconds } = period
        && native_seconds.contains(&seconds)
    {
        return Ok(());
    }
    if select_base_period(period, native_seconds).is_some() {
        Ok(())
    } else {
        Err("Provider history is unavailable for this interval".to_string())
    }
}

/// Builds a derived [`HistoryRequest`] targeting `base_period` with an expanded bar limit so the
/// resampled result still covers the caller's requested bar count.
#[must_use]
pub(crate) fn derive_base_request(
    request: &HistoryRequest,
    base_period: BarPeriod,
    ratio: usize,
) -> HistoryRequest {
    let mut base = request.clone();
    base.series.period = base_period;
    base.maximum_bars = request
        .maximum_bars
        .saturating_mul(ratio.max(1))
        .min(MAXIMUM_RESAMPLE_BASE_BARS);
    base
}

fn bucket_start_unix_nanos(ts: i64, period: BarPeriod) -> Option<i64> {
    match period {
        BarPeriod::Time { seconds } => {
            let dur = i64::from(seconds).checked_mul(NANOS_PER_SECOND)?;
            Some(ts.div_euclid(dur).saturating_mul(dur))
        }
        BarPeriod::Session { days } => {
            let dur = i64::from(days).checked_mul(NANOS_PER_DAY)?;
            Some(ts.div_euclid(dur).saturating_mul(dur))
        }
        BarPeriod::Week { weeks } => {
            let span_days = i64::from(weeks).checked_mul(7)?;
            let day = ts.div_euclid(NANOS_PER_DAY);
            // 1970-01-01 was Thursday (offset +3 days from Monday).
            let week_idx = (day + 3).div_euclid(span_days);
            let bucket_day = week_idx.saturating_mul(span_days) - 3;
            Some(bucket_day.saturating_mul(NANOS_PER_DAY))
        }
        BarPeriod::Month { months } => {
            let day = ts.div_euclid(NANOS_PER_DAY);
            let (year, month, _) = civil_from_days(day);
            let m0 = i32::from(month) - 1;
            let step = i32::try_from(months).ok()?.max(1);
            let total_months = year * 12 + m0;
            let bucket_total = total_months.div_euclid(step) * step;
            let b_year = bucket_total.div_euclid(12);
            let b_month = (bucket_total.rem_euclid(12) + 1) as u8;
            let b_day = days_from_civil(b_year, b_month, 1);
            Some(b_day.saturating_mul(NANOS_PER_DAY))
        }
        BarPeriod::Tick { .. } => None,
    }
}

/// Howard Hinnant's civil-from-days algorithm (UTC).
const fn civil_from_days(days_since_epoch: i64) -> (i32, u8, u8) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    let year = (y + if m <= 2 { 1 } else { 0 }) as i32;
    (year, m, d)
}

/// Howard Hinnant's days-from-civil algorithm (UTC).
const fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let y = (year as i64) - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = month as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + (day as u64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + (doe as i64) - 719_468
}

/// Aggregates a finer-cadence [`HistorySnapshot`] into `target_period`, preserving exact OHLCV
/// semantics, monotonic bucket timestamps, contiguous sequence numbers, and forming-bar handoff.
pub(crate) fn resample_snapshot(
    target_period: BarPeriod,
    base: HistorySnapshot,
) -> Result<HistorySnapshot, String> {
    let mut buckets: BTreeMap<i64, MarketBar> = BTreeMap::new();
    let mut forming_bucket: Option<i64> = None;

    let mut fold_bar = |bar: MarketBar, is_forming: bool| {
        let Some(bucket) = bucket_start_unix_nanos(bar.exchange_timestamp_unix_nanos, target_period)
        else {
            return;
        };
        if is_forming {
            forming_bucket = Some(bucket);
        }
        buckets
            .entry(bucket)
            .and_modify(|acc| {
                acc.high = acc.high.max(bar.high);
                acc.low = acc.low.min(bar.low);
                acc.close = bar.close;
                acc.volume = acc.volume.saturating_add(bar.volume);
            })
            .or_insert_with(|| MarketBar {
                exchange_timestamp_seconds: bucket.div_euclid(NANOS_PER_SECOND),
                exchange_timestamp_unix_nanos: bucket,
                ..bar
            });
    };

    for bar in base.bars {
        fold_bar(bar, false);
    }
    let forming_trades = base.forming.and_then(|forming| {
        fold_bar(forming.bar, true);
        forming.trades
    });

    let forming = forming_bucket.and_then(|bucket| buckets.remove(&bucket));
    let mut bars: Vec<MarketBar> = buckets.into_values().collect();
    for (idx, bar) in bars.iter_mut().enumerate() {
        bar.source_sequence = (idx as u64) + 1;
        bar.validate().map_err(|err| err.to_string())?;
    }
    let forming = forming.map(|mut bar| {
        bar.source_sequence = (bars.len() as u64) + 1;
        FormingBar {
            bar,
            trades: forming_trades,
        }
    });

    Ok(HistorySnapshot {
        price_scale: base.price_scale,
        quantity_scale: base.quantity_scale,
        bars,
        forming,
        handoff_boundary_unix_nanos: base.handoff_boundary_unix_nanos,
        backwards_exhausted: base.backwards_exhausted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts_sec: i64, o: i64, h: i64, l: i64, c: i64, v: i64) -> MarketBar {
        MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: ts_sec,
            exchange_timestamp_unix_nanos: ts_sec * NANOS_PER_SECOND,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: v,
        }
    }

    #[test]
    fn resamples_5m_bars_into_exact_10m_bars_and_preserves_forming_tail() {
        let base = HistorySnapshot {
            price_scale: 2,
            quantity_scale: 0,
            bars: vec![
                bar(0, 100, 110, 95, 105, 10),
                bar(300, 105, 115, 100, 112, 20),
                bar(600, 112, 120, 108, 118, 15),
                bar(900, 118, 125, 110, 122, 25),
            ],
            forming: Some(FormingBar {
                bar: bar(1_200, 122, 126, 120, 124, 5),
                trades: Some(3),
            }),
            handoff_boundary_unix_nanos: None,
            backwards_exhausted: false,
        };
        let out = resample_snapshot(BarPeriod::time(600).unwrap(), base).expect("resamples 10m");
        assert_eq!(out.bars.len(), 2);
        assert_eq!(out.bars[0].exchange_timestamp_unix_nanos, 0);
        assert_eq!(out.bars[0].open, 100);
        assert_eq!(out.bars[0].high, 115);
        assert_eq!(out.bars[0].low, 95);
        assert_eq!(out.bars[0].close, 112);
        assert_eq!(out.bars[0].volume, 30);
        assert_eq!(out.bars[1].exchange_timestamp_unix_nanos, 600 * NANOS_PER_SECOND);
        assert_eq!(out.bars[1].close, 122);
        assert_eq!(out.bars[1].volume, 40);
        let forming = out.forming.expect("forming preserved");
        assert_eq!(
            forming.bar.exchange_timestamp_unix_nanos,
            1_200 * NANOS_PER_SECOND
        );
        assert_eq!(forming.bar.close, 124);
        assert_eq!(forming.trades, Some(3));
    }

    #[test]
    fn selects_coarsest_dividing_base_period() {
        let native = [60, 300, 900, 1_800, 3_600, 7_200, 21_600, 86_400];
        assert_eq!(
            select_base_period(BarPeriod::time(600).unwrap(), &native),
            Some((BarPeriod::time(300).unwrap(), 2))
        );
        assert_eq!(
            select_base_period(BarPeriod::time(2_700).unwrap(), &native),
            Some((BarPeriod::time(900).unwrap(), 3))
        );
        assert_eq!(
            select_base_period(BarPeriod::time(10_800).unwrap(), &native),
            Some((BarPeriod::time(3_600).unwrap(), 3))
        );
        assert_eq!(
            select_base_period(BarPeriod::month(3).unwrap(), &native),
            Some((BarPeriod::time(86_400).unwrap(), 93))
        );
    }
}
