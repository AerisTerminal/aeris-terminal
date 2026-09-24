//! Engine-owned Hyperliquid candle-history collection over public HTTPS.
//!
//! No credentials: every call is an unauthenticated `candleSnapshot` window.
//! Provider candles are the authority for supported time-based intervals; the
//! page already splits closed history from the still-open period, which the
//! live handoff seeds as the in-flight bar.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use asceify_contracts::InstallProviderInstrument;
use asceify_hyperliquid_market_adapter::{
    CandleSnapshotRequest, HyperliquidHttpConfig, NORMALIZED_PRICE_SCALE,
    NORMALIZED_QUANTITY_SCALE, hyperliquid_interval_for_period,
};
use asceify_market_data::{BarPeriod, BarSeriesKey};
use asceify_provider_history::HistoryRange;

use crate::market_service::{FormingBar, HistoryFetchWindow};

const NANOS_PER_MILLI: i64 = 1_000_000;

pub(super) struct Snapshot {
    pub(super) price_scale: u8,
    pub(super) quantity_scale: u8,
    /// Periods the provider has closed. Only these become canonical history.
    pub(super) bars: Vec<asceify_market_data::MarketBar>,
    /// The period that was still open when the page was served.
    pub(super) forming: Option<FormingBar>,
    pub(super) handoff_boundary_unix_nanos: i64,
}

/// Fetches one bounded candle page for a demanded Hyperliquid series.
///
/// The generation does not gate the fetch itself — the public session is
/// stateless HTTPS with no session binding, so history stays valid across
/// WebSocket reconnects. It still fences the completion on the coordinator
/// side.
///
/// # Errors
///
/// Returns an error for inconsistent identity, unsupported intervals,
/// transport or decode failure, empty pages, or cancellation.
pub(super) fn fetch(
    client: &mut asceify_hyperliquid_market_adapter::HyperliquidHttpClient,
    series: &BarSeriesKey,
    _provider_generation: u64,
    installed: &InstallProviderInstrument,
    window: HistoryFetchWindow,
    stop: &Arc<AtomicBool>,
) -> Result<Snapshot, String> {
    if stop.load(Ordering::Acquire) {
        return Err("Hyperliquid history request was cancelled".to_string());
    }
    if installed.provider != "hyperliquid"
        || installed.instrument_id != series.instrument_id
        || installed.entitlement_id != series.entitlement_id
        || series.definition_version != 1
    {
        return Err("Hyperliquid history identity is inconsistent".to_string());
    }
    // The public session is stateless HTTPS: unlike credentialed sessions,
    // the install carries no session binding, so history stays valid across
    // WebSocket reconnects without reinstalling the instrument. The
    // generation still fences the completion on the coordinator side.
    // Fails explicitly for intervals with no native Hyperliquid candle
    // (notably tick periods) instead of reinterpreting the demand.
    hyperliquid_interval_for_period(series.period)?;
    let maximum_bars = window.maximum_bars.clamp(
        1,
        asceify_hyperliquid_market_adapter::MAXIMUM_HYPERLIQUID_CANDLES,
    );
    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is invalid".to_string())?
        .as_millis();
    let now_millis =
        i64::try_from(now_millis).map_err(|_| "system clock is invalid".to_string())?;
    let (start_millis, end_millis) = match window.range {
        Some(range) => history_window_for_range(series.period, maximum_bars, range, now_millis)?,
        None => history_window(series.period, maximum_bars, now_millis)?,
    };
    let page = client.fetch_candle_snapshot(
        &CandleSnapshotRequest {
            wire_coin: &installed.provider_symbol,
            period: series.period,
            start_millis,
            end_millis,
            now_millis,
            price_scale: NORMALIZED_PRICE_SCALE,
            quantity_scale: NORMALIZED_QUANTITY_SCALE,
            config: HyperliquidHttpConfig::default(),
        },
        stop,
    )?;
    if stop.load(Ordering::Acquire) {
        return Err("Hyperliquid history request was cancelled".to_string());
    }
    if page.bars.is_empty() && page.forming.is_none() {
        return Err("Hyperliquid returned no historical bars".to_string());
    }
    let handoff_boundary_unix_nanos = page
        .handoff_close_millis
        .and_then(|millis| millis.checked_mul(NANOS_PER_MILLI))
        .ok_or_else(|| "Hyperliquid history handoff is missing".to_string())?;
    Ok(Snapshot {
        price_scale: u8::try_from(NORMALIZED_PRICE_SCALE)
            .map_err(|_| "Hyperliquid price scale is invalid".to_string())?,
        quantity_scale: u8::try_from(NORMALIZED_QUANTITY_SCALE)
            .map_err(|_| "Hyperliquid quantity scale is invalid".to_string())?,
        bars: page.bars,
        forming: page.forming.map(|bar| FormingBar { bar, trades: None }),
        handoff_boundary_unix_nanos,
    })
}

fn history_window_for_range(
    period: BarPeriod,
    maximum_bars: usize,
    range: HistoryRange,
    now_millis: i64,
) -> Result<(i64, i64), String> {
    if range.start_unix_nanos >= range.end_unix_nanos {
        return Err("Hyperliquid history range is empty".to_string());
    }
    let requested_start = range.start_unix_nanos.div_euclid(NANOS_PER_MILLI).max(0);
    let requested_end = range
        .end_unix_nanos
        .div_euclid(NANOS_PER_MILLI)
        .min(now_millis);
    if requested_start >= requested_end {
        return Err("Hyperliquid history range is outside available time".to_string());
    }
    let (minimum_start, _) = history_window(period, maximum_bars, requested_end)?;
    Ok((requested_start.max(minimum_start), requested_end))
}

/// Sizes one bounded history window ending now.
///
/// The span is window sizing only: the provider returns what it has (at most
/// the latest 5,000 candles) and the decoder splits closed from forming.
fn history_window(
    period: BarPeriod,
    maximum_bars: usize,
    now_millis: i64,
) -> Result<(i64, i64), String> {
    let interval_millis = match period {
        BarPeriod::Time { seconds } => i64::from(seconds)
            .checked_mul(1_000)
            .ok_or_else(|| "Hyperliquid history window overflowed".to_string())?,
        BarPeriod::Session { days } => i64::from(days)
            .checked_mul(86_400_000)
            .ok_or_else(|| "Hyperliquid history window overflowed".to_string())?,
        BarPeriod::Week { weeks } => i64::from(weeks)
            .checked_mul(7 * 86_400_000)
            .ok_or_else(|| "Hyperliquid history window overflowed".to_string())?,
        BarPeriod::Month { .. } => 30 * 86_400_000,
        BarPeriod::Tick { .. } => {
            return Err("Hyperliquid history is unavailable for this interval".to_string());
        }
    };
    // Both endpoints are inclusive. N requested rows span N-1 bucket starts.
    let requested_intervals = maximum_bars.saturating_sub(1).max(1);
    let span_millis = i64::try_from(requested_intervals)
        .ok()
        .and_then(|bars| bars.checked_mul(interval_millis))
        .ok_or_else(|| "Hyperliquid history window overflowed".to_string())?;
    let end_millis = now_millis;
    let start_millis = end_millis
        .checked_sub(span_millis)
        .ok_or_else(|| "Hyperliquid history window underflowed".to_string())?
        .max(0);
    if start_millis >= end_millis {
        return Err("Hyperliquid history window is empty".to_string());
    }
    Ok((start_millis, end_millis))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_window_accounts_for_inclusive_endpoints() {
        let now = 10_000 * 60_000;
        let (start, end) =
            history_window(BarPeriod::time(60).expect("period"), 5_000, now).expect("window");
        assert_eq!((end - start) / 60_000 + 1, 5_000);
    }
}
