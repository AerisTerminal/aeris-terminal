//! OKX V5 public HTTPS client (instruments, tickers, candles).

use std::{
    io::Read,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use aeris_crypto_common::{CryptoError, CryptoResult};
use aeris_market_data::BarPeriod;
use aeris_platform_runtime::CancellableHttpClient;

use crate::{
    candles::{MAXIMUM_OKX_CANDLES, OkxCandlePage, decode_candle_page, okx_interval_for_period},
    endpoints::OKX_REST_URL,
    identity::OkxMarketKind,
    meta::{OkxCatalog, decode_instruments, decode_ticker_volumes},
};

/// Bounds for one OKX REST request.
#[derive(Clone, Copy, Debug)]
pub struct OkxHttpConfig {
    pub timeout: Duration,
    pub maximum_bytes: usize,
}

impl Default for OkxHttpConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            maximum_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Reusable cancellable HTTP client owned by one worker thread.
#[derive(Default)]
pub struct OkxHttpClient {
    http: CancellableHttpClient,
}

impl OkxHttpClient {
    /// Fetches a bounded page of historical candles from `/api/v5/market/candles`
    /// (or `/api/v5/market/history-candles` when `after_millis` is provided for older ranges).
    ///
    /// # Errors
    /// Returns an error on cancellation, unsupported interval, transport failure, or malformed JSON.
    #[allow(clippy::too_many_arguments)]
    pub fn fetch_candles(
        &mut self,
        kind: OkxMarketKind,
        inst_id: &str,
        period: BarPeriod,
        after_millis: Option<i64>,
        limit: usize,
        config: OkxHttpConfig,
        stop: &Arc<AtomicBool>,
    ) -> CryptoResult<OkxCandlePage> {
        if stop.load(Ordering::Acquire) {
            return Err(CryptoError::History(
                "okx history request was cancelled".to_string(),
            ));
        }
        let bar = okx_interval_for_period(period)?;
        let cap = limit.clamp(1, MAXIMUM_OKX_CANDLES);
        let url = match after_millis {
            Some(after) => format!(
                "{OKX_REST_URL}/api/v5/market/history-candles?instId={inst_id}&bar={bar}&after={after}&limit={cap}"
            ),
            None => format!(
                "{OKX_REST_URL}/api/v5/market/candles?instId={inst_id}&bar={bar}&limit={cap}"
            ),
        };
        self.http.set_cancellation(stop);
        let bytes = get_with_agent(self.http.agent(), &url, config)?;
        decode_candle_page(&bytes, kind, period)
    }
}

/// Fetches the combined OKX Linear Perpetual Swap + Spot catalog.
///
/// # Errors
/// Returns an error only when both SWAP and SPOT catalog fetches fail.
pub fn fetch_catalog(config: OkxHttpConfig) -> CryptoResult<OkxCatalog> {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| CancellableHttpClient::default().into_agent());

    let mut instruments = Vec::new();
    let mut errors = Vec::new();

    let (swap_tickers, swap_info, spot_tickers, spot_info) = std::thread::scope(|scope| {
        let swap_tickers_task = scope.spawn(|| {
            get_with_agent(
                agent,
                &format!(
                    "{OKX_REST_URL}/api/v5/market/tickers?instType={}",
                    OkxMarketKind::Swap.inst_type()
                ),
                config,
            )
            .map(|bytes| decode_ticker_volumes(&bytes, OkxMarketKind::Swap))
            .unwrap_or_default()
        });
        let swap_info_task = scope.spawn(|| {
            get_with_agent(
                agent,
                &format!(
                    "{OKX_REST_URL}/api/v5/public/instruments?instType={}",
                    OkxMarketKind::Swap.inst_type()
                ),
                config,
            )
        });
        let spot_tickers_task = scope.spawn(|| {
            get_with_agent(
                agent,
                &format!(
                    "{OKX_REST_URL}/api/v5/market/tickers?instType={}",
                    OkxMarketKind::Spot.inst_type()
                ),
                config,
            )
            .map(|bytes| decode_ticker_volumes(&bytes, OkxMarketKind::Spot))
            .unwrap_or_default()
        });
        let spot_info_task = scope.spawn(|| {
            get_with_agent(
                agent,
                &format!(
                    "{OKX_REST_URL}/api/v5/public/instruments?instType={}",
                    OkxMarketKind::Spot.inst_type()
                ),
                config,
            )
        });
        (
            swap_tickers_task.join().unwrap_or_default(),
            swap_info_task.join().unwrap_or_else(|_| {
                Err(CryptoError::Transport(
                    "okx swap instruments worker panicked".to_string(),
                ))
            }),
            spot_tickers_task.join().unwrap_or_default(),
            spot_info_task.join().unwrap_or_else(|_| {
                Err(CryptoError::Transport(
                    "okx spot instruments worker panicked".to_string(),
                ))
            }),
        )
    });

    match swap_info.and_then(|bytes| decode_instruments(&bytes, OkxMarketKind::Swap, &swap_tickers))
    {
        Ok(mut items) => instruments.append(&mut items),
        Err(error) => errors.push(error),
    }
    match spot_info.and_then(|bytes| decode_instruments(&bytes, OkxMarketKind::Spot, &spot_tickers))
    {
        Ok(mut items) => instruments.append(&mut items),
        Err(error) => errors.push(error),
    }

    if instruments.is_empty() {
        return Err(errors
            .into_iter()
            .next()
            .unwrap_or_else(|| CryptoError::Transport("okx catalog is unavailable".to_string())));
    }
    Ok(OkxCatalog::from_instruments(instruments))
}

fn get_with_agent(agent: &ureq::Agent, url: &str, config: OkxHttpConfig) -> CryptoResult<Vec<u8>> {
    let mut response = agent
        .get(url)
        .config()
        .timeout_global(Some(config.timeout))
        .build()
        .call()
        .map_err(|error| {
            CryptoError::Transport(match error {
                ureq::Error::StatusCode(code) => format!("okx HTTP request failed (HTTP {code})"),
                ureq::Error::Timeout(_) => "okx HTTP request timed out".to_string(),
                _ => "okx HTTP request failed".to_string(),
            })
        })?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(config.maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CryptoError::Transport("okx HTTP response is unreadable".to_string()))?;
    if bytes.len() > config.maximum_bytes {
        return Err(CryptoError::Transport(
            "okx HTTP response exceeds the bound".to_string(),
        ));
    }
    Ok(bytes)
}
