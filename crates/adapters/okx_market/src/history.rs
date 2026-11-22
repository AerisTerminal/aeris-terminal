//! Engine-owned OKX V5 candle-history collection over public HTTPS.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use aeris_crypto_common::{
    CryptoError, CryptoResult, NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE,
};
use aeris_market_data::BarSeriesKey;
use aeris_provider_spi::{
    FormingBar, HistoryFetchWindow, HistoryInstrument, ProviderHistorySnapshot,
};

use crate::{
    candles::{MAXIMUM_OKX_CANDLES, okx_interval_for_period},
    http::{OkxHttpClient, OkxHttpConfig},
    identity::parse_provider_symbol,
};

const NANOS_PER_MILLI: i64 = 1_000_000;

/// Fetches one bounded candle page for a demanded OKX series.
///
/// # Errors
/// Returns an error for inconsistent identity, unsupported intervals, cancellation,
/// transport failure, or an empty initial page.
pub fn fetch(
    client: &mut OkxHttpClient,
    series: &BarSeriesKey,
    _provider_generation: u64,
    installed: &HistoryInstrument,
    window: HistoryFetchWindow,
    stop: &Arc<AtomicBool>,
) -> CryptoResult<ProviderHistorySnapshot> {
    if stop.load(Ordering::Acquire) {
        return Err(CryptoError::History(
            "OKX history request was cancelled".to_string(),
        ));
    }
    if installed.provider != "okx"
        || installed.instrument_id != series.instrument_id
        || installed.entitlement_id != series.entitlement_id
        || series.definition_version != 1
    {
        return Err(CryptoError::History(
            "OKX history identity is inconsistent".to_string(),
        ));
    }
    okx_interval_for_period(series.period)?;
    let sym_ref = parse_provider_symbol(&installed.provider_symbol)?;
    let maximum_bars = window.maximum_bars.clamp(1, MAXIMUM_OKX_CANDLES);
    let ranged = window.range.is_some();
    let after_millis = window
        .range
        .map(|range| range.end_unix_nanos.div_euclid(NANOS_PER_MILLI));

    let page = client.fetch_candles(
        sym_ref.kind,
        &sym_ref.inst_id,
        series.period,
        after_millis,
        maximum_bars,
        OkxHttpConfig::default(),
        stop,
    )?;
    if stop.load(Ordering::Acquire) {
        return Err(CryptoError::History(
            "OKX history request was cancelled".to_string(),
        ));
    }
    let price_scale = u8::try_from(NORMALIZED_PRICE_SCALE)
        .map_err(|_| CryptoError::History("OKX price scale is invalid".to_string()))?;
    let quantity_scale = u8::try_from(NORMALIZED_QUANTITY_SCALE)
        .map_err(|_| CryptoError::History("OKX quantity scale is invalid".to_string()))?;
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
            "OKX returned no historical bars".to_string(),
        ));
    }
    let handoff_boundary_unix_nanos = page
        .handoff_close_millis
        .and_then(|millis| millis.checked_mul(NANOS_PER_MILLI))
        .ok_or_else(|| CryptoError::History("OKX history handoff is missing".to_string()))?;
    Ok(ProviderHistorySnapshot {
        price_scale,
        quantity_scale,
        bars: page.bars,
        forming: page.forming.map(|bar| FormingBar { bar, trades: None }),
        handoff_boundary_unix_nanos: Some(handoff_boundary_unix_nanos),
        backwards_exhausted: false,
    })
}
