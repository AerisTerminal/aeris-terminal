//! Binance public HTTPS client (exchangeInfo, tickers, klines, depth snapshots).

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
    book::{BinanceDepthSnapshot, decode_depth_snapshot},
    candles::{
        BinanceCandlePage, MAXIMUM_BINANCE_FUTURES_CANDLES, MAXIMUM_BINANCE_SPOT_CANDLES,
        binance_interval_for_period, decode_candle_page,
    },
    endpoints::{BINANCE_FUTURES_REST_URL, BINANCE_SPOT_FALLBACK_REST_URL, BINANCE_SPOT_REST_URL},
    identity::BinanceMarketKind,
    meta::{BinanceCatalog, decode_exchange_info, decode_ticker_volumes},
    trades::decode_rest_agg_trades,
};

/// Bounds for one Binance REST request.
#[derive(Clone, Copy, Debug)]
pub struct BinanceHttpConfig {
    pub timeout: Duration,
    pub maximum_bytes: usize,
}

impl Default for BinanceHttpConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            maximum_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Reusable cancellable HTTP client owned by one worker thread.
#[derive(Default)]
pub struct BinanceHttpClient {
    http: CancellableHttpClient,
}

impl BinanceHttpClient {
    /// Fetches a bounded page of historical candles for a Futures or Spot symbol.
    ///
    /// # Errors
    /// Returns an error for unsupported intervals, cancellation, transport failure, or malformed JSON.
    #[allow(clippy::too_many_arguments)]
    pub fn fetch_klines(
        &mut self,
        kind: BinanceMarketKind,
        wire_symbol: &str,
        period: BarPeriod,
        start_millis: i64,
        end_millis: i64,
        limit: usize,
        now_millis: i64,
        config: BinanceHttpConfig,
        stop: &Arc<AtomicBool>,
    ) -> CryptoResult<BinanceCandlePage> {
        if stop.load(Ordering::Acquire) {
            return Err(CryptoError::History(
                "binance history request was cancelled".to_string(),
            ));
        }
        let interval = binance_interval_for_period(period)?;
        let cap = match kind {
            BinanceMarketKind::UsdmPerp => limit.clamp(1, MAXIMUM_BINANCE_FUTURES_CANDLES),
            BinanceMarketKind::Spot => limit.clamp(1, MAXIMUM_BINANCE_SPOT_CANDLES),
        };
        let query = format!(
            "symbol={wire_symbol}&interval={interval}&startTime={start_millis}&endTime={end_millis}&limit={cap}"
        );
        self.http.set_cancellation(stop);
        let bytes = match kind {
            BinanceMarketKind::UsdmPerp => {
                let url = format!("{BINANCE_FUTURES_REST_URL}/fapi/v1/klines?{query}");
                get_with_agent(self.http.agent(), &url, config)?
            }
            BinanceMarketKind::Spot => {
                let primary = format!("{BINANCE_SPOT_REST_URL}/api/v3/klines?{query}");
                if let Ok(bytes) = get_with_agent(self.http.agent(), &primary, config) {
                    bytes
                } else {
                    let fallback =
                        format!("{BINANCE_SPOT_FALLBACK_REST_URL}/api/v3/klines?{query}");
                    get_with_agent(self.http.agent(), &fallback, config)?
                }
            }
        };
        decode_candle_page(&bytes, now_millis)
    }

    /// Fetches a REST order-book snapshot (`/depth?limit=1000`).
    ///
    /// # Errors
    /// Returns an error on cancellation, transport failure, or malformed payload.
    pub fn fetch_depth_snapshot(
        &mut self,
        kind: BinanceMarketKind,
        wire_symbol: &str,
        limit: usize,
        config: BinanceHttpConfig,
        stop: &Arc<AtomicBool>,
    ) -> CryptoResult<BinanceDepthSnapshot> {
        if stop.load(Ordering::Acquire) {
            return Err(CryptoError::Transport(
                "binance depth snapshot request was cancelled".to_string(),
            ));
        }
        let cap = limit.clamp(5, 1_000);
        self.http.set_cancellation(stop);
        let bytes = match kind {
            BinanceMarketKind::UsdmPerp => {
                let url = format!(
                    "{BINANCE_FUTURES_REST_URL}/fapi/v1/depth?symbol={wire_symbol}&limit={cap}"
                );
                get_with_agent(self.http.agent(), &url, config)?
            }
            BinanceMarketKind::Spot => {
                let primary = format!(
                    "{BINANCE_SPOT_REST_URL}/api/v3/depth?symbol={wire_symbol}&limit={cap}"
                );
                if let Ok(bytes) = get_with_agent(self.http.agent(), &primary, config) {
                    bytes
                } else {
                    let fallback = format!(
                        "{BINANCE_SPOT_FALLBACK_REST_URL}/api/v3/depth?symbol={wire_symbol}&limit={cap}"
                    );
                    get_with_agent(self.http.agent(), &fallback, config)?
                }
            }
        };
        decode_depth_snapshot(&bytes)
    }

    /// Fetches up to `max_pages` (capped at 5) recent 1,000-trade pages from `/aggTrades`
    /// in chronological order to backfill footprints, CVD, and big trades on subscription.
    ///
    /// # Errors
    /// Returns an error on cancellation, transport failure, or malformed payload.
    #[allow(clippy::too_many_arguments)]
    pub fn fetch_recent_agg_trades(
        &mut self,
        kind: BinanceMarketKind,
        wire_symbol: &str,
        instrument_id: &str,
        entitlement_id: &str,
        session_generation: u64,
        next_sequence: &mut u64,
        received_unix_nanos: i64,
        max_pages: usize,
        config: BinanceHttpConfig,
        stop: &Arc<AtomicBool>,
    ) -> CryptoResult<Vec<(u64, aeris_market_data::MarketTrade)>> {
        let pages = max_pages.clamp(1, 5);
        let mut by_id = std::collections::BTreeMap::new();
        let mut next_from_id: Option<u64> = None;

        for _ in 0..pages {
            if stop.load(Ordering::Acquire) {
                return Err(CryptoError::Transport(
                    "binance aggTrades backfill request was cancelled".to_string(),
                ));
            }
            let query = match next_from_id {
                Some(from_id) => format!("symbol={wire_symbol}&fromId={from_id}&limit=1000"),
                None => format!("symbol={wire_symbol}&limit=1000"),
            };
            self.http.set_cancellation(stop);
            let bytes = match kind {
                BinanceMarketKind::UsdmPerp => {
                    let url = format!("{BINANCE_FUTURES_REST_URL}/fapi/v1/aggTrades?{query}");
                    get_with_agent(self.http.agent(), &url, config)?
                }
                BinanceMarketKind::Spot => {
                    let primary = format!("{BINANCE_SPOT_REST_URL}/api/v3/aggTrades?{query}");
                    if let Ok(bytes) = get_with_agent(self.http.agent(), &primary, config) {
                        bytes
                    } else {
                        let fallback =
                            format!("{BINANCE_SPOT_FALLBACK_REST_URL}/api/v3/aggTrades?{query}");
                        get_with_agent(self.http.agent(), &fallback, config)?
                    }
                }
            };
            let decoded = decode_rest_agg_trades(
                &bytes,
                instrument_id,
                entitlement_id,
                session_generation,
                next_sequence,
                received_unix_nanos,
            )?;
            let Some(earliest_id) = decoded.iter().map(|(id, _)| *id).min() else {
                break;
            };
            let page_len = decoded.len();
            for (id, trade) in decoded {
                by_id.insert(id, trade);
            }
            if page_len < 1_000 || earliest_id == 0 {
                break;
            }
            let prev_from = earliest_id.saturating_sub(1_000);
            if next_from_id == Some(prev_from) {
                break;
            }
            next_from_id = Some(prev_from);
        }

        // Re-sequence chronologically so source_sequence is strictly increasing.
        let mut out = Vec::with_capacity(by_id.len());
        for (id, mut trade) in by_id {
            *next_sequence = next_sequence.saturating_add(1).max(1);
            trade.metadata.source_sequence = *next_sequence;
            out.push((id, trade));
        }
        Ok(out)
    }
}

/// Fetches the combined Binance USDT-M Perpetual + Spot catalog with 24h volume ranking.
///
/// If one segment is temporarily unavailable (e.g., regional restriction on Futures),
/// the remaining healthy segment still loads cleanly.
///
/// # Errors
/// Returns an error only when both Futures and Spot catalog requests fail.
pub fn fetch_catalog(config: BinanceHttpConfig) -> CryptoResult<BinanceCatalog> {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| CancellableHttpClient::default().into_agent());

    let mut instruments = Vec::new();
    let mut errors = Vec::new();

    let spot_query =
        "/api/v3/exchangeInfo?permissions=SPOT&symbolStatus=TRADING&showPermissionSets=false";
    let (fut_info, fut_vol, spot_info, spot_vol) = std::thread::scope(|scope| {
        let futures_info_task = scope.spawn(|| {
            get_with_agent(
                agent,
                &format!("{BINANCE_FUTURES_REST_URL}/fapi/v1/exchangeInfo"),
                config,
            )
        });
        let futures_volumes_task = scope.spawn(|| {
            get_with_agent(
                agent,
                &format!("{BINANCE_FUTURES_REST_URL}/fapi/v1/ticker/24hr"),
                config,
            )
            .map(|bytes| decode_ticker_volumes(&bytes))
            .unwrap_or_default()
        });
        let spot_info_task = scope.spawn(|| {
            get_with_agent(
                agent,
                &format!("{BINANCE_SPOT_REST_URL}{spot_query}"),
                config,
            )
            .or_else(|_| {
                get_with_agent(
                    agent,
                    &format!("{BINANCE_SPOT_FALLBACK_REST_URL}{spot_query}"),
                    config,
                )
            })
        });
        let spot_volumes_task = scope.spawn(|| {
            get_with_agent(
                agent,
                &format!("{BINANCE_SPOT_REST_URL}/api/v3/ticker/24hr"),
                config,
            )
            .or_else(|_| {
                get_with_agent(
                    agent,
                    &format!("{BINANCE_SPOT_FALLBACK_REST_URL}/api/v3/ticker/24hr"),
                    config,
                )
            })
            .map(|bytes| decode_ticker_volumes(&bytes))
            .unwrap_or_default()
        });
        (
            futures_info_task.join().unwrap_or_else(|_| {
                Err(CryptoError::Transport(
                    "binance futures exchangeInfo worker panicked".to_string(),
                ))
            }),
            futures_volumes_task.join().unwrap_or_default(),
            spot_info_task.join().unwrap_or_else(|_| {
                Err(CryptoError::Transport(
                    "binance spot exchangeInfo worker panicked".to_string(),
                ))
            }),
            spot_volumes_task.join().unwrap_or_default(),
        )
    });

    match fut_info {
        Ok(info_bytes) => {
            match decode_exchange_info(&info_bytes, BinanceMarketKind::UsdmPerp, &fut_vol) {
                Ok(mut perps) => instruments.append(&mut perps),
                Err(error) => errors.push(error),
            }
        }
        Err(error) => errors.push(error),
    }

    match spot_info {
        Ok(info_bytes) => {
            match decode_exchange_info(&info_bytes, BinanceMarketKind::Spot, &spot_vol) {
                Ok(mut spot) => instruments.append(&mut spot),
                Err(error) => errors.push(error),
            }
        }
        Err(error) => errors.push(error),
    }

    if instruments.is_empty() {
        return Err(errors.into_iter().next().unwrap_or_else(|| {
            CryptoError::Transport("binance catalog is unavailable".to_string())
        }));
    }
    Ok(BinanceCatalog::from_instruments(instruments))
}

fn get_with_agent(
    agent: &ureq::Agent,
    url: &str,
    config: BinanceHttpConfig,
) -> CryptoResult<Vec<u8>> {
    let mut response = agent
        .get(url)
        .config()
        .timeout_global(Some(config.timeout))
        .build()
        .call()
        .map_err(|error| CryptoError::Transport(http_error_summary(&error)))?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(config.maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CryptoError::Transport("binance HTTP response is unreadable".to_string()))?;
    if bytes.len() > config.maximum_bytes {
        return Err(CryptoError::Transport(
            "binance HTTP response exceeds the bound".to_string(),
        ));
    }
    Ok(bytes)
}

fn http_error_summary(error: &ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(429) => "binance rate limit reached (HTTP 429)".to_string(),
        ureq::Error::StatusCode(451) => {
            "binance endpoint is restricted in this region (HTTP 451)".to_string()
        }
        ureq::Error::StatusCode(code) => format!("binance HTTP request failed (HTTP {code})"),
        ureq::Error::Timeout(_) => "binance HTTP request timed out".to_string(),
        _ => "binance HTTP request failed".to_string(),
    }
}
