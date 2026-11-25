//! Coinbase Advanced Trade public HTTPS client (products catalog, candles, recent trades).

use std::{
    io::Read,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use aeris_crypto_common::{CryptoError, CryptoResult};
use aeris_market_data::{BarPeriod, MarketTrade};
use aeris_platform_runtime::CancellableHttpClient;

use crate::{
    candles::{
        CoinbaseCandlePage, MAXIMUM_COINBASE_CANDLES, coinbase_interval_for_period,
        decode_candle_page,
    },
    endpoints::COINBASE_REST_URL,
    meta::{CoinbaseCatalog, decode_products_page},
    trades::decode_ticker_trades,
};

const MAXIMUM_CATALOG_PAGES: usize = 4;
const PRODUCTS_PAGE_LIMIT: usize = 1_000;
const MAXIMUM_TICKER_TRADES: usize = 1_000;

/// Bounds for one Coinbase REST request.
#[derive(Clone, Copy, Debug)]
pub struct CoinbaseHttpConfig {
    pub timeout: Duration,
    pub maximum_bytes: usize,
}

impl Default for CoinbaseHttpConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            maximum_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Reusable cancellable HTTP client owned by one worker thread.
#[derive(Default)]
pub struct CoinbaseHttpClient {
    http: CancellableHttpClient,
}

impl CoinbaseHttpClient {
    /// Fetches a bounded page of historical candles from `/market/products/{id}/candles`.
    ///
    /// # Errors
    /// Returns an error on cancellation, unsupported interval, transport failure, or malformed JSON.
    #[allow(clippy::too_many_arguments)]
    pub fn fetch_candles(
        &mut self,
        product_id: &str,
        period: BarPeriod,
        start_secs: i64,
        end_secs: i64,
        limit: usize,
        now_millis: i64,
        config: CoinbaseHttpConfig,
        stop: &Arc<AtomicBool>,
    ) -> CryptoResult<CoinbaseCandlePage> {
        if stop.load(Ordering::Acquire) {
            return Err(CryptoError::History(
                "coinbase history request was cancelled".to_string(),
            ));
        }
        let granularity = coinbase_interval_for_period(period)?;
        let cap = limit.clamp(1, MAXIMUM_COINBASE_CANDLES);
        let url = format!(
            "{COINBASE_REST_URL}/products/{product_id}/candles?start={start_secs}&end={end_secs}&granularity={granularity}&limit={cap}"
        );
        self.http.set_cancellation(stop);
        let bytes = get_with_agent(self.http.agent(), &url, config)?;
        decode_candle_page(&bytes, period, now_millis)
    }

    /// Fetches recent public trades from `/market/products/{id}/ticker` for initial footprint backfill.
    ///
    /// # Errors
    /// Returns an error on cancellation, transport failure, or malformed JSON.
    #[allow(clippy::too_many_arguments)]
    pub fn fetch_recent_trades(
        &mut self,
        product_id: &str,
        instrument_id: &str,
        entitlement_id: &str,
        session_generation: u64,
        next_sequence: &mut u64,
        received_unix_nanos: i64,
        limit: usize,
        config: CoinbaseHttpConfig,
        stop: &Arc<AtomicBool>,
    ) -> CryptoResult<Vec<(String, MarketTrade)>> {
        if stop.load(Ordering::Acquire) {
            return Err(CryptoError::History(
                "coinbase trade backfill was cancelled".to_string(),
            ));
        }
        let cap = limit.clamp(1, MAXIMUM_TICKER_TRADES);
        let url = format!("{COINBASE_REST_URL}/products/{product_id}/ticker?limit={cap}");
        self.http.set_cancellation(stop);
        let bytes = get_with_agent(self.http.agent(), &url, config)?;
        decode_ticker_trades(
            &bytes,
            instrument_id,
            entitlement_id,
            session_generation,
            next_sequence,
            received_unix_nanos,
        )
    }
}

/// Fetches the public Coinbase Spot product catalog across bounded pagination pages.
///
/// # Errors
/// Returns an error if the catalog cannot be fetched or contains no usable instruments.
pub fn fetch_catalog(config: CoinbaseHttpConfig) -> CryptoResult<CoinbaseCatalog> {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| CancellableHttpClient::default().into_agent());

    let mut instruments = Vec::new();
    let mut cursor: Option<String> = None;

    for _ in 0..MAXIMUM_CATALOG_PAGES {
        let url = match cursor.as_deref() {
            Some(next) => format!(
                "{COINBASE_REST_URL}/products?product_type=SPOT&limit={PRODUCTS_PAGE_LIMIT}&cursor={next}"
            ),
            None => format!(
                "{COINBASE_REST_URL}/products?product_type=SPOT&limit={PRODUCTS_PAGE_LIMIT}"
            ),
        };
        let bytes = get_with_agent(agent, &url, config)?;
        let (mut page_items, next_cursor) = decode_products_page(&bytes)?;
        instruments.append(&mut page_items);
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }

    if instruments.is_empty() {
        return Err(CryptoError::Transport(
            "coinbase catalog is unavailable".to_string(),
        ));
    }
    Ok(CoinbaseCatalog::from_instruments(instruments))
}

pub(crate) fn get_with_agent(
    agent: &ureq::Agent,
    url: &str,
    config: CoinbaseHttpConfig,
) -> CryptoResult<Vec<u8>> {
    let mut response = agent
        .get(url)
        .config()
        .timeout_global(Some(config.timeout))
        .build()
        .call()
        .map_err(|error| {
            CryptoError::Transport(match error {
                ureq::Error::StatusCode(code) => {
                    format!("coinbase HTTP request failed (HTTP {code})")
                }
                ureq::Error::Timeout(_) => "coinbase HTTP request timed out".to_string(),
                _ => "coinbase HTTP request failed".to_string(),
            })
        })?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(config.maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CryptoError::Transport("coinbase HTTP response is unreadable".to_string()))?;
    if bytes.len() > config.maximum_bytes {
        return Err(CryptoError::Transport(
            "coinbase HTTP response exceeds the bound".to_string(),
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::instrument_id_for;
    use aeris_market_data::BarSeriesKey;
    use aeris_provider_spi::{HistoryFetchWindow, HistoryInstrument};

    #[test]
    #[ignore = "drives live public Coinbase REST APIs"]
    fn live_coinbase_catalog_history_and_recent_trades_decode_cleanly() {
        let catalog = fetch_catalog(CoinbaseHttpConfig::default()).expect("coinbase catalog");
        let btc = catalog
            .get("spot:BTC-USD")
            .expect("Coinbase BTC-USD missing from catalog");
        assert_eq!(btc.product_id, "BTC-USD");
        assert!(btc.tick_size_fixed > 0);

        let mut client = CoinbaseHttpClient::default();
        let stop = Arc::new(AtomicBool::new(false));
        let series = BarSeriesKey {
            provider_id: "coinbase".to_string(),
            instrument_id: instrument_id_for(btc.kind, &btc.product_id),
            entitlement_id: "coinbase-public".to_string(),
            period: BarPeriod::time(60).expect("1m period"),
            definition_version: 1,
        };
        let installed = HistoryInstrument {
            provider: "coinbase".to_string(),
            session_generation: 1,
            instrument_id: series.instrument_id.clone(),
            provider_symbol: btc.provider_symbol.clone(),
            display_symbol: btc.display_symbol.clone(),
            entitlement_id: "coinbase-public".to_string(),
            price_scale: 8,
            quantity_scale: 8,
            venue_id: btc.kind.exchange_label().to_string(),
            price_increment: Some(btc.tick_size_fixed),
            contract: None,
        };
        let snapshot = crate::history::fetch(
            &mut client,
            &series,
            1,
            &installed,
            HistoryFetchWindow {
                maximum_bars: 50,
                range: None,
            },
            &stop,
        )
        .expect("coinbase 1m history snapshot");
        assert!(!snapshot.bars.is_empty(), "expected closed historical bars");
        assert!(snapshot.handoff_boundary_unix_nanos.is_some());

        let mut seq = 0;
        let trades = client
            .fetch_recent_trades(
                &btc.product_id,
                &series.instrument_id,
                "coinbase-public",
                1,
                &mut seq,
                snapshot.handoff_boundary_unix_nanos.unwrap_or(1),
                20,
                CoinbaseHttpConfig::default(),
                &stop,
            )
            .expect("coinbase recent trades");
        assert!(!trades.is_empty(), "expected recent trades");
    }
}
