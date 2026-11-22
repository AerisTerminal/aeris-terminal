//! Bybit V5 public HTTPS client (instruments-info, tickers, klines).

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
    candles::{
        BybitCandlePage, MAXIMUM_BYBIT_CANDLES, bybit_interval_for_period, decode_candle_page,
    },
    endpoints::BYBIT_REST_URL,
    identity::BybitMarketKind,
    meta::{BybitCatalog, decode_instruments_page, decode_ticker_volumes},
};

/// Bounds for one Bybit REST request.
#[derive(Clone, Copy, Debug)]
pub struct BybitHttpConfig {
    pub timeout: Duration,
    pub maximum_bytes: usize,
}

impl Default for BybitHttpConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            maximum_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Reusable cancellable HTTP client owned by one worker thread.
#[derive(Default)]
pub struct BybitHttpClient {
    http: CancellableHttpClient,
}

impl BybitHttpClient {
    /// Fetches a bounded page of historical klines from `/v5/market/kline`.
    ///
    /// # Errors
    /// Returns an error on cancellation, unsupported interval, transport failure, or malformed JSON.
    #[allow(clippy::too_many_arguments)]
    pub fn fetch_klines(
        &mut self,
        kind: BybitMarketKind,
        wire_symbol: &str,
        period: BarPeriod,
        start_millis: i64,
        end_millis: i64,
        limit: usize,
        now_millis: i64,
        config: BybitHttpConfig,
        stop: &Arc<AtomicBool>,
    ) -> CryptoResult<BybitCandlePage> {
        if stop.load(Ordering::Acquire) {
            return Err(CryptoError::History(
                "bybit history request was cancelled".to_string(),
            ));
        }
        let interval = bybit_interval_for_period(period)?;
        let cap = limit.clamp(1, MAXIMUM_BYBIT_CANDLES);
        let url = format!(
            "{BYBIT_REST_URL}/v5/market/kline?category={}&symbol={wire_symbol}&interval={interval}&start={start_millis}&end={end_millis}&limit={cap}",
            kind.category()
        );
        self.http.set_cancellation(stop);
        let bytes = get_with_agent(self.http.agent(), &url, config)?;
        decode_candle_page(&bytes, period, now_millis)
    }
}

/// Fetches the combined Bybit Linear Perpetual + Spot catalog with 24h turnover ranking.
///
/// # Errors
/// Returns an error only when both Linear and Spot catalog fetches fail.
pub fn fetch_catalog(config: BybitHttpConfig) -> CryptoResult<BybitCatalog> {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| CancellableHttpClient::default().into_agent());

    let mut instruments = Vec::new();
    let mut errors = Vec::new();

    for kind in [BybitMarketKind::Linear, BybitMarketKind::Spot] {
        let volumes = get_with_agent(
            agent,
            &format!(
                "{BYBIT_REST_URL}/v5/market/tickers?category={}",
                kind.category()
            ),
            config,
        )
        .map(|bytes| decode_ticker_volumes(&bytes))
        .unwrap_or_default();

        let mut cursor: Option<String> = None;
        for _ in 0..8 {
            let url = match &cursor {
                Some(c) => format!(
                    "{BYBIT_REST_URL}/v5/market/instruments-info?category={}&limit=1000&cursor={c}",
                    kind.category()
                ),
                None => format!(
                    "{BYBIT_REST_URL}/v5/market/instruments-info?category={}&limit=1000",
                    kind.category()
                ),
            };
            match get_with_agent(agent, &url, config)
                .and_then(|bytes| decode_instruments_page(&bytes, kind, &volumes))
            {
                Ok((mut page, next)) => {
                    instruments.append(&mut page);
                    cursor = next;
                    if cursor.is_none() {
                        break;
                    }
                }
                Err(error) => {
                    errors.push(error);
                    break;
                }
            }
        }
    }

    if instruments.is_empty() {
        return Err(errors.into_iter().next().unwrap_or_else(|| {
            CryptoError::Transport("bybit catalog is unavailable".to_string())
        }));
    }
    Ok(BybitCatalog::from_instruments(instruments))
}

fn get_with_agent(
    agent: &ureq::Agent,
    url: &str,
    config: BybitHttpConfig,
) -> CryptoResult<Vec<u8>> {
    let mut response = agent
        .get(url)
        .config()
        .timeout_global(Some(config.timeout))
        .build()
        .call()
        .map_err(|error| {
            CryptoError::Transport(match error {
                ureq::Error::StatusCode(code) => format!("bybit HTTP request failed (HTTP {code})"),
                ureq::Error::Timeout(_) => "bybit HTTP request timed out".to_string(),
                _ => "bybit HTTP request failed".to_string(),
            })
        })?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(config.maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CryptoError::Transport("bybit HTTP response is unreadable".to_string()))?;
    if bytes.len() > config.maximum_bytes {
        return Err(CryptoError::Transport(
            "bybit HTTP response exceeds the bound".to_string(),
        ));
    }
    Ok(bytes)
}
