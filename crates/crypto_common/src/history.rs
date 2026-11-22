//! Shared candle history window sizing for cryptocurrency exchanges.

use aeris_market_data::BarPeriod;
use aeris_provider_history::HistoryRange;

use crate::error::{CryptoError, CryptoResult};

const NANOS_PER_MILLI: i64 = 1_000_000;

/// Returns the nominal duration of one [`BarPeriod`] in milliseconds.
///
/// # Errors
/// Returns an error for tick periods or arithmetic overflow.
pub fn bar_period_millis(period: BarPeriod) -> CryptoResult<i64> {
    match period {
        BarPeriod::Time { seconds } => i64::from(seconds)
            .checked_mul(1_000)
            .ok_or_else(|| CryptoError::History("history interval overflowed".to_string())),
        BarPeriod::Session { days } => i64::from(days)
            .checked_mul(86_400_000)
            .ok_or_else(|| CryptoError::History("history interval overflowed".to_string())),
        BarPeriod::Week { weeks } => i64::from(weeks)
            .checked_mul(7 * 86_400_000)
            .ok_or_else(|| CryptoError::History("history interval overflowed".to_string())),
        BarPeriod::Month { months } => i64::from(months)
            .checked_mul(30 * 86_400_000)
            .ok_or_else(|| CryptoError::History("history interval overflowed".to_string())),
        BarPeriod::Tick { .. } => Err(CryptoError::Unsupported(
            "tick intervals are not supported for candle history".to_string(),
        )),
    }
}

/// Sizes one bounded history window `(start_millis, end_millis)` ending at `now_millis`.
///
/// # Errors
/// Returns an error for unsupported periods or arithmetic overflow/underflow.
pub fn history_window(
    period: BarPeriod,
    maximum_bars: usize,
    now_millis: i64,
) -> CryptoResult<(i64, i64)> {
    let interval_millis = bar_period_millis(period)?;
    let requested_intervals = maximum_bars.saturating_sub(1).max(1);
    let span_millis = i64::try_from(requested_intervals)
        .ok()
        .and_then(|bars| bars.checked_mul(interval_millis))
        .ok_or_else(|| CryptoError::History("history window overflowed".to_string()))?;
    let end_millis = now_millis;
    let start_millis = end_millis
        .checked_sub(span_millis)
        .ok_or_else(|| CryptoError::History("history window underflowed".to_string()))?
        .max(0);
    if start_millis >= end_millis {
        return Err(CryptoError::History("history window is empty".to_string()));
    }
    Ok((start_millis, end_millis))
}

/// Converts a half-open [`HistoryRange`] into bounded inclusive millisecond endpoints.
///
/// # Errors
/// Returns an error when the range is empty or outside available time.
pub fn history_window_for_range(
    period: BarPeriod,
    maximum_bars: usize,
    range: HistoryRange,
    now_millis: i64,
) -> CryptoResult<(i64, i64)> {
    if range.start_unix_nanos >= range.end_unix_nanos {
        return Err(CryptoError::History("history range is empty".to_string()));
    }
    let requested_start = range.start_unix_nanos.div_euclid(NANOS_PER_MILLI).max(0);
    let requested_end = range
        .end_unix_nanos
        .saturating_sub(1)
        .div_euclid(NANOS_PER_MILLI)
        .min(now_millis);
    if requested_start >= requested_end {
        return Err(CryptoError::History(
            "history range is outside available time".to_string(),
        ));
    }
    let (minimum_start, _) = history_window(period, maximum_bars, requested_end)?;
    Ok((requested_start.max(minimum_start), requested_end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_window_computes_expected_span() {
        let period = BarPeriod::time(60).unwrap();
        let (start, end) = history_window(period, 100, 1_700_000_000_000).unwrap();
        assert_eq!(end, 1_700_000_000_000);
        assert_eq!(end - start, 99 * 60_000);
    }
}
