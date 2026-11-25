//! Engine-owned Coinbase candle-history collection over public HTTPS.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use aeris_crypto_common::{
    CryptoError, CryptoResult, NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE, history_window,
    history_window_for_range,
};
use aeris_market_data::BarSeriesKey;
use aeris_provider_spi::{
    FormingBar, HistoryFetchWindow, HistoryInstrument, ProviderHistorySnapshot,
};

use crate::{
    candles::{MAXIMUM_COINBASE_CANDLES, coinbase_interval_for_period},
    http::{CoinbaseHttpClient, CoinbaseHttpConfig},
    identity::parse_provider_symbol,
};

const NANOS_PER_MILLI: i64 = 1_000_000;
const MILLIS_PER_SECOND: i64 = 1_000;

fn now_unix_millis() -> CryptoResult<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CryptoError::History("system clock precedes UNIX epoch".to_string()))?;
    i64::try_from(duration.as_millis())
        .map_err(|_| CryptoError::History("system clock overflowed milliseconds".to_string()))
}

/// Fetches one bounded candle page for a demanded Coinbase series.
///
/// # Errors
/// Returns an error for inconsistent identity, unsupported intervals, cancellation,
/// transport failure, or an empty initial page.
pub fn fetch(
    client: &mut CoinbaseHttpClient,
    series: &BarSeriesKey,
    _provider_generation: u64,
    installed: &HistoryInstrument,
    window: HistoryFetchWindow,
    stop: &Arc<AtomicBool>,
) -> CryptoResult<ProviderHistorySnapshot> {
    if stop.load(Ordering::Acquire) {
        return Err(CryptoError::History(
            "Coinbase history request was cancelled".to_string(),
        ));
    }
    if installed.provider != "coinbase"
        || installed.instrument_id != series.instrument_id
        || installed.entitlement_id != series.entitlement_id
        || series.definition_version != 1
    {
        return Err(CryptoError::History(
            "Coinbase history identity is inconsistent".to_string(),
        ));
    }
    coinbase_interval_for_period(series.period)?;
    let sym_ref = parse_provider_symbol(&installed.provider_symbol)?;
    let maximum_bars = window.maximum_bars.clamp(1, MAXIMUM_COINBASE_CANDLES);
    let now_millis = now_unix_millis()?;
    let ranged = window.range.is_some();
    let (start_millis, end_millis) = match window.range {
        Some(range) => history_window_for_range(series.period, maximum_bars, range, now_millis)?,
        None => history_window(series.period, maximum_bars, now_millis)?,
    };
    let start_secs = start_millis.div_euclid(MILLIS_PER_SECOND);
    let end_secs = end_millis
        .div_euclid(MILLIS_PER_SECOND)
        .max(start_secs.saturating_add(1));

    let page = client.fetch_candles(
        &sym_ref.product_id,
        series.period,
        start_secs,
        end_secs,
        maximum_bars,
        now_millis,
        CoinbaseHttpConfig::default(),
        stop,
    )?;
    if stop.load(Ordering::Acquire) {
        return Err(CryptoError::History(
            "Coinbase history request was cancelled".to_string(),
        ));
    }
    let price_scale = u8::try_from(NORMALIZED_PRICE_SCALE)
        .map_err(|_| CryptoError::History("Coinbase price scale is invalid".to_string()))?;
    let quantity_scale = u8::try_from(NORMALIZED_QUANTITY_SCALE)
        .map_err(|_| CryptoError::History("Coinbase quantity scale is invalid".to_string()))?;
    if page.bars.is_empty() && page.forming.is_none() {
        if ranged {
            return Ok(ProviderHistorySnapshot {
                price_scale,
                quantity_scale,
                bars: Vec::new(),
                forming: None,
                handoff_boundary_unix_nanos: None,
                backwards_exhausted: true,
            });
        }
        return Err(CryptoError::History(
            "Coinbase returned no historical bars".to_string(),
        ));
    }
    let handoff_boundary_unix_nanos = page
        .handoff_boundary_millis
        .and_then(|millis| millis.checked_mul(NANOS_PER_MILLI))
        .ok_or_else(|| CryptoError::History("Coinbase history handoff is missing".to_string()))?;
    Ok(ProviderHistorySnapshot {
        price_scale,
        quantity_scale,
        bars: page.bars,
        forming: page.forming.map(|bar| FormingBar { bar, trades: None }),
        handoff_boundary_unix_nanos: Some(handoff_boundary_unix_nanos),
        backwards_exhausted: false,
    })
}
