//! Engine-owned Binance candle-history collection over public HTTPS.

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
    candles::{
        MAXIMUM_BINANCE_FUTURES_CANDLES, MAXIMUM_BINANCE_SPOT_CANDLES, binance_interval_for_period,
    },
    http::{BinanceHttpClient, BinanceHttpConfig},
    identity::{BinanceMarketKind, parse_provider_symbol},
};

const NANOS_PER_MILLI: i64 = 1_000_000;

/// Fetches one bounded candle page for a demanded Binance series.
///
/// # Errors
/// Returns an error for inconsistent identity, unsupported intervals, cancellation,
/// transport failure, or an empty initial page.
pub fn fetch(
    client: &mut BinanceHttpClient,
    series: &BarSeriesKey,
    _provider_generation: u64,
    installed: &HistoryInstrument,
    window: HistoryFetchWindow,
    stop: &Arc<AtomicBool>,
) -> CryptoResult<ProviderHistorySnapshot> {
    if stop.load(Ordering::Acquire) {
        return Err(CryptoError::History(
            "Binance history request was cancelled".to_string(),
        ));
    }
    if installed.provider != "binance"
        || installed.instrument_id != series.instrument_id
        || installed.entitlement_id != series.entitlement_id
        || series.definition_version != 1
    {
        return Err(CryptoError::History(
            "Binance history identity is inconsistent".to_string(),
        ));
    }
    binance_interval_for_period(series.period)?;
    let sym_ref = parse_provider_symbol(&installed.provider_symbol)?;
    let max_limit = match sym_ref.kind {
        BinanceMarketKind::UsdmPerp => MAXIMUM_BINANCE_FUTURES_CANDLES,
        BinanceMarketKind::Spot => MAXIMUM_BINANCE_SPOT_CANDLES,
    };
    let maximum_bars = window.maximum_bars.clamp(1, max_limit);
    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CryptoError::History("system clock is invalid".to_string()))?
        .as_millis();
    let now_millis = i64::try_from(now_millis)
        .map_err(|_| CryptoError::History("system clock is invalid".to_string()))?;
    let ranged = window.range.is_some();
    let (start_millis, end_millis) = match window.range {
        Some(range) => history_window_for_range(series.period, maximum_bars, range, now_millis)?,
        None => history_window(series.period, maximum_bars, now_millis)?,
    };
    let page = client.fetch_klines(
        sym_ref.kind,
        &sym_ref.wire_symbol,
        series.period,
        start_millis,
        end_millis,
        maximum_bars,
        now_millis,
        BinanceHttpConfig::default(),
        stop,
    )?;
    if stop.load(Ordering::Acquire) {
        return Err(CryptoError::History(
            "Binance history request was cancelled".to_string(),
        ));
    }
    let price_scale = u8::try_from(NORMALIZED_PRICE_SCALE)
        .map_err(|_| CryptoError::History("Binance price scale is invalid".to_string()))?;
    let quantity_scale = u8::try_from(NORMALIZED_QUANTITY_SCALE)
        .map_err(|_| CryptoError::History("Binance quantity scale is invalid".to_string()))?;
    if page.bars.is_empty() && page.forming.is_none() {
        if ranged {
            return Ok(ProviderHistorySnapshot {
                price_scale,
                quantity_scale,
                bars: Vec::new(),
                forming: None,
                handoff_boundary_unix_nanos: None,
                backwards_exhausted: false,
            });
        }
        return Err(CryptoError::History(
            "Binance returned no historical bars".to_string(),
        ));
    }
    let handoff_boundary_unix_nanos = page
        .handoff_close_millis
        .and_then(|millis| millis.checked_mul(NANOS_PER_MILLI))
        .ok_or_else(|| CryptoError::History("Binance history handoff is missing".to_string()))?;
    Ok(ProviderHistorySnapshot {
        price_scale,
        quantity_scale,
        bars: page.bars,
        forming: page.forming.map(|bar| FormingBar { bar, trades: None }),
        handoff_boundary_unix_nanos: Some(handoff_boundary_unix_nanos),
        backwards_exhausted: false,
    })
}
