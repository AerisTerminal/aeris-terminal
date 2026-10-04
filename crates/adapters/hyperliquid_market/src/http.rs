//! Bounded background HTTP work over the public info endpoint.
//!
//! All calls are unauthenticated `POST /info` requests with explicit byte
//! and time bounds. The engine runs them on bounded background workers with
//! cancellation. Only the catalog bundle fans out, over a fixed number of
//! scoped threads, because its independent metadata calls dominate startup.

use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

use crate::candles::{HyperliquidCandlePage, decode_candle_page, hyperliquid_interval_for_period};
use crate::endpoints::HYPERLIQUID_INFO_URL;
use crate::meta::{HyperliquidCatalog, RawMetaBundle, decode_catalog};
use aeris_market_data::BarPeriod;

/// Bounds for one info request.
#[derive(Clone, Copy, Debug)]
pub struct HyperliquidHttpConfig {
    /// Request timeout; the engine cancels slower work.
    pub timeout: Duration,
    /// Maximum response bytes accepted before failing closed.
    pub maximum_bytes: usize,
}

impl Default for HyperliquidHttpConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            maximum_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Executes one `POST /info` request and returns the raw response bytes.
///
/// Bytes stay unparsed so callers with fractional decimals can decode from
/// raw text without any intermediate `f64` conversion.
///
/// # Errors
///
/// Returns an error for oversized requests, transport failures, or
/// over-bound or unreadable responses.
pub fn post_info(
    body: &serde_json::Value,
    config: HyperliquidHttpConfig,
) -> Result<Vec<u8>, String> {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let raw = body.to_string();
    if raw.len() > 64 * 1024 {
        return Err("hyperliquid info request is too large".to_string());
    }
    // Catalog requests use the same bounded DNS/transport implementation.
    // This shared agent has no mutable request token; history owns its own client.
    let agent =
        AGENT.get_or_init(|| aeris_platform_runtime::CancellableHttpClient::default().into_agent());
    post_with_agent(agent, raw, config)
}

fn post_with_agent(
    agent: &ureq::Agent,
    raw: String,
    config: HyperliquidHttpConfig,
) -> Result<Vec<u8>, String> {
    let mut response = agent
        .post(HYPERLIQUID_INFO_URL)
        .config()
        .timeout_global(Some(config.timeout))
        .build()
        .header("Content-Type", "application/json")
        .send(raw)
        .map_err(info_request_error)?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(config.maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "hyperliquid info response is unreadable".to_string())?;
    if bytes.len() > config.maximum_bytes {
        return Err("hyperliquid info response exceeds the bound".to_string());
    }
    Ok(bytes)
}

fn info_request_error(error: ureq::Error) -> String {
    // Preserve actionable transport/status classes without forwarding response bodies,
    // URLs, proxy configuration, or arbitrary provider text into engine diagnostics.
    match error {
        ureq::Error::StatusCode(status) => {
            format!("hyperliquid info request failed: HTTP {status}")
        }
        ureq::Error::Timeout(_) => "hyperliquid info request timed out".to_string(),
        ureq::Error::HostNotFound => "hyperliquid info host could not be resolved".to_string(),
        ureq::Error::Io(error) => format!("hyperliquid info transport failed: {:?}", error.kind()),
        ureq::Error::ConnectionFailed => "hyperliquid info connection failed".to_string(),
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) => "hyperliquid info TLS failed".to_string(),
        _ => "hyperliquid info request failed".to_string(),
    }
}

fn parse_info_bytes(bytes: &[u8]) -> Result<serde_json::Value, String> {
    serde_json::from_slice(bytes).map_err(|_| "hyperliquid info response is malformed".to_string())
}

/// Extracts one builder DEX name from a `perpDexs` entry.
///
/// Live entries are objects carrying the name (e.g. `{"name": "xyz", ...}`);
/// plain strings are accepted for older payloads. JSON null and blank names
/// mark the core DEX, whose meta is the base call below — those are skipped.
/// Anything else is malformed and fails the refresh: silently dropping a
/// builder DEX would report an incomplete catalog as complete.
///
/// # Errors
///
/// Returns an error for a non-null entry without a usable name.
fn dex_name(entry: &serde_json::Value) -> Result<Option<String>, String> {
    if entry.is_null() {
        return Ok(None);
    }
    if let Some(name) = entry.as_str() {
        return if name.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(name.to_string()))
        };
    }
    let name = entry
        .get("name")
        .and_then(|name| name.as_str())
        .ok_or_else(|| "hyperliquid perp dex entry is malformed".to_string())?;
    if name.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(name.to_string()))
    }
}

/// Fetches and decodes the full catalog bundle (meta, spotMeta, perpDexs).
///
/// A failed `perpDexs` call fails the whole refresh rather than reporting
/// a builder-less catalog as complete; the engine retains its last valid
/// catalog and reports the failure through provider state.
///
/// # Errors
///
/// Returns an error when metadata is unavailable or the catalog is
/// malformed or empty.
pub fn fetch_meta_bundle(config: HyperliquidHttpConfig) -> Result<HyperliquidCatalog, String> {
    let mut roots = fetch_info_concurrently(
        &[
            serde_json::json!({"type": "metaAndAssetCtxs"}),
            serde_json::json!({"type": "spotMetaAndAssetCtxs"}),
            serde_json::json!({"type": "perpDexs"}),
        ],
        config,
    )?
    .into_iter();
    let (Some(core), Some(spot), Some(dexs)) = (roots.next(), roots.next(), roots.next()) else {
        return Err("hyperliquid metadata bundle is incomplete".to_string());
    };
    let entries = dexs
        .as_array()
        .ok_or_else(|| "hyperliquid perp dexes are malformed".to_string())?;
    let mut dex_names = Vec::new();
    for entry in entries.iter().take(MAXIMUM_BUILDER_DEXES) {
        if let Some(dex) = dex_name(entry)? {
            dex_names.push(dex);
        }
    }
    let builder_requests = dex_names
        .iter()
        .map(|dex| serde_json::json!({"type": "metaAndAssetCtxs", "dex": dex}))
        .collect::<Vec<_>>();
    let builder_metas = dex_names
        .into_iter()
        .zip(fetch_info_concurrently(&builder_requests, config)?)
        .collect::<Vec<_>>();
    let core_meta = combined_meta(&core)?;
    let spot_meta = combined_meta(&spot)?;
    let builder_meta_only = builder_metas
        .iter()
        .map(|(dex, combined)| Ok((dex.clone(), combined_meta(combined)?)))
        .collect::<Result<Vec<_>, String>>()?;
    let mut catalog = decode_catalog(&RawMetaBundle {
        core_perp_meta: core_meta,
        spot_meta,
        perp_dexs: dexs,
        builder_metas: builder_meta_only,
    })?;
    apply_asset_contexts(&mut catalog, &core)?;
    apply_asset_contexts(&mut catalog, &spot)?;
    for (_, combined) in &builder_metas {
        apply_asset_contexts(&mut catalog, combined)?;
    }
    Ok(catalog)
}

/// Builder perp dexes included in one catalog bundle.
const MAXIMUM_BUILDER_DEXES: usize = 32;
/// Concurrent metadata requests; a bundle is at most two dependent rounds of these.
const MAXIMUM_CONCURRENT_INFO_REQUESTS: usize = 8;

/// Executes independent info requests on bounded scoped threads, preserving order.
///
/// The first failure fails the whole batch, matching the all-or-nothing bundle.
fn fetch_info_concurrently(
    bodies: &[serde_json::Value],
    config: HyperliquidHttpConfig,
) -> Result<Vec<serde_json::Value>, String> {
    let mut responses = Vec::with_capacity(bodies.len());
    for chunk in bodies.chunks(MAXIMUM_CONCURRENT_INFO_REQUESTS) {
        let results = std::thread::scope(|scope| {
            let workers = chunk
                .iter()
                .map(|body| scope.spawn(move || parse_info_bytes(&post_info(body, config)?)))
                .collect::<Vec<_>>();
            workers
                .into_iter()
                .map(|worker| {
                    worker
                        .join()
                        .unwrap_or_else(|_| Err("hyperliquid info worker panicked".to_string()))
                })
                .collect::<Vec<_>>()
        });
        for result in results {
            responses.push(result?);
        }
    }
    Ok(responses)
}

fn combined_meta(response: &serde_json::Value) -> Result<serde_json::Value, String> {
    response
        .as_array()
        .and_then(|parts| parts.first())
        .cloned()
        .ok_or_else(|| "hyperliquid metadata and contexts are malformed".to_string())
}

fn apply_asset_contexts(
    catalog: &mut HyperliquidCatalog,
    response: &serde_json::Value,
) -> Result<(), String> {
    let parts = response
        .as_array()
        .filter(|parts| parts.len() == 2)
        .ok_or_else(|| "hyperliquid metadata and contexts are malformed".to_string())?;
    let universe = parts[0]
        .get("universe")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "hyperliquid metadata universe is malformed".to_string())?;
    let contexts = parts[1]
        .as_array()
        .ok_or_else(|| "hyperliquid asset contexts are malformed".to_string())?;
    let contexts_carry_identity = contexts.iter().all(|context| {
        context
            .get("coin")
            .and_then(serde_json::Value::as_str)
            .is_some()
    });
    if contexts_carry_identity {
        for context in contexts {
            apply_asset_context(catalog, context, None);
        }
        return Ok(());
    }
    if universe.len() != contexts.len() {
        return Err("hyperliquid metadata and contexts are inconsistent".to_string());
    }
    for (market, context) in universe.iter().zip(contexts) {
        apply_asset_context(catalog, context, market.get("name"));
    }
    Ok(())
}

fn apply_asset_context(
    catalog: &mut HyperliquidCatalog,
    context: &serde_json::Value,
    fallback_coin: Option<&serde_json::Value>,
) {
    let wire_coin = context
        .get("coin")
        .and_then(serde_json::Value::as_str)
        .or_else(|| fallback_coin.and_then(serde_json::Value::as_str))
        .unwrap_or("");
    if let Some(mark_price) = context.get("markPx").and_then(serde_json::Value::as_str) {
        catalog.set_reference_price(wire_coin, mark_price);
    }
    let Some(volume) = context.get("dayNtlVlm").and_then(serde_json::Value::as_str) else {
        return;
    };
    catalog.set_day_notional_volume(wire_coin, volume);
}

/// One bounded candle-window request.
pub struct CandleSnapshotRequest<'a> {
    /// Exact wire `coin` for the info call.
    pub wire_coin: &'a str,
    /// Canonical period, mapped to a native Hyperliquid interval.
    pub period: BarPeriod,
    /// Inclusive window start in epoch millis.
    pub start_millis: i64,
    /// Inclusive window end in epoch millis.
    pub end_millis: i64,
    /// Reference time deciding which trailing candle is still forming.
    pub now_millis: i64,
    /// Fixed-point price scale for OHLC values.
    pub price_scale: u32,
    /// Fixed-point quantity scale for volumes.
    pub quantity_scale: u32,
    /// Bounds for the underlying info request.
    pub config: HyperliquidHttpConfig,
}

/// Fetches one candle window for a wire coin.
///
/// # Errors
///
/// Returns an error for invalid windows, unsupported intervals, transport
/// failures, or malformed candle pages.
pub fn fetch_candle_snapshot(
    request: &CandleSnapshotRequest<'_>,
) -> Result<HyperliquidCandlePage, String> {
    HyperliquidHttpClient::default().fetch_candle_snapshot(
        request,
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
}

/// Exclusive reusable HTTP client owned by the Hyperliquid history worker.
#[derive(Default)]
pub struct HyperliquidHttpClient(aeris_platform_runtime::CancellableHttpClient);

impl HyperliquidHttpClient {
    /// Fetches a bounded candle page with cancellation through DNS, TLS and HTTP I/O.
    ///
    /// # Errors
    /// Returns transport, cancellation, or validated provider payload errors.
    pub fn fetch_candle_snapshot(
        &mut self,
        request: &CandleSnapshotRequest<'_>,
        stop: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<HyperliquidCandlePage, String> {
        self.0.set_cancellation(stop);
        if request.wire_coin.trim().is_empty()
            || request.wire_coin.len() > 96
            || request.start_millis < 0
            || request.end_millis <= request.start_millis
        {
            return Err("hyperliquid candle request is invalid".to_string());
        }
        let interval = hyperliquid_interval_for_period(request.period)?;
        let bytes = post_with_agent(
            self.0.agent(),
            serde_json::json!({
                "type": "candleSnapshot",
                "req": {
                    "coin": request.wire_coin,
                    "interval": interval,
                    "startTime": request.start_millis,
                    "endTime": request.end_millis,
                },
            })
            .to_string(),
            request.config,
        )?;
        // Decode from the raw response text: candle decimals never pass
        // through `f64` on this path either.
        let text = String::from_utf8(bytes)
            .map_err(|_| "hyperliquid info response is malformed".to_string())?;
        let payload = serde_json::value::RawValue::from_string(text)
            .map_err(|_| "hyperliquid info response is malformed".to_string())?;
        decode_candle_page(
            &payload,
            request.period,
            request.price_scale,
            request.quantity_scale,
            request.now_millis,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_failures_preserve_status_without_forwarding_transport_text() {
        assert_eq!(
            info_request_error(ureq::Error::StatusCode(429)),
            "hyperliquid info request failed: HTTP 429"
        );
        let error = std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "private proxy configuration",
        );
        assert_eq!(
            info_request_error(ureq::Error::Io(error)),
            "hyperliquid info transport failed: ConnectionReset"
        );
        assert_eq!(
            info_request_error(ureq::Error::BadUri("private URL".to_string())),
            "hyperliquid info request failed"
        );
    }

    #[test]
    fn asset_contexts_publish_mark_price_increments() {
        let core_meta = json!({"universe": [
            {"name": "BTC", "szDecimals": 5},
            {"name": "ETH", "szDecimals": 4},
            {"name": "DOGE", "szDecimals": 0},
        ]});
        let mut catalog = crate::decode_catalog(&crate::RawMetaBundle {
            core_perp_meta: core_meta.clone(),
            spot_meta: json!({
                "universe": [],
                "tokens": [{"name": "USDC", "szDecimals": 8, "index": 0}],
            }),
            perp_dexs: json!([null]),
            builder_metas: Vec::new(),
        })
        .expect("catalog");
        // `metaAndAssetCtxs` shape: contexts align with the universe by index.
        apply_asset_contexts(
            &mut catalog,
            &json!([core_meta, [
                {"markPx": "85031.0", "dayNtlVlm": "1.0"},
                {"markPx": "3012.4", "dayNtlVlm": "1.0"},
                {"dayNtlVlm": "1.0"},
            ]]),
        )
        .expect("contexts");
        assert_eq!(
            catalog.price_increment("hyperliquid:perp:BTC"),
            Some(100_000_000)
        );
        assert_eq!(
            catalog.price_increment("hyperliquid:perp:ETH"),
            Some(10_000_000)
        );
        assert_eq!(
            catalog.price_increment("hyperliquid:perp:DOGE"),
            None,
            "a context without a mark price leaves the increment unknown"
        );
    }

    #[test]
    fn dex_names_accept_live_objects_and_plain_strings() {
        // Observed `perpDexs` shape: `[null, {"name": "xyz", ...}, ...]`.
        assert_eq!(dex_name(&json!(null)), Ok(None));
        assert_eq!(
            dex_name(&json!({"name": "xyz", "fullName": "XYZ"})),
            Ok(Some("xyz".to_string()))
        );
        assert_eq!(dex_name(&json!("xyz")), Ok(Some("xyz".to_string())));
        assert_eq!(dex_name(&json!("")), Ok(None));
        assert_eq!(dex_name(&json!({"name": ""})), Ok(None));
        assert!(dex_name(&json!(42)).is_err());
        assert!(dex_name(&json!({"other": "xyz"})).is_err());
    }

    #[test]
    #[ignore = "drives the live Hyperliquid public info endpoint"]
    fn live_three_day_candle_snapshot_decodes() {
        let now_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_millis();
        let now_millis = i64::try_from(now_millis).expect("millis fit");
        let page = fetch_candle_snapshot(&CandleSnapshotRequest {
            wire_coin: "BTC",
            period: aeris_market_data::BarPeriod::session(3).expect("3d"),
            start_millis: 0,
            end_millis: now_millis,
            now_millis,
            price_scale: crate::NORMALIZED_PRICE_SCALE,
            quantity_scale: crate::NORMALIZED_QUANTITY_SCALE,
            config: HyperliquidHttpConfig::default(),
        })
        .expect("live 3D candle page decodes");
        println!(
            "hyperliquid_live_3d closed={} forming={} first={} last={}",
            page.bars.len(),
            page.forming.is_some(),
            page.bars
                .first()
                .map_or(0, |bar| bar.exchange_timestamp_unix_nanos),
            page.bars
                .last()
                .map_or(0, |bar| bar.exchange_timestamp_unix_nanos),
        );
        assert_ne!(page.bars, [] as [aeris_market_data::MarketBar; 0]);
    }
    #[test]
    #[ignore = "drives the live Hyperliquid public info endpoint"]
    fn live_month_candle_snapshot_decodes() {
        let now_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_millis();
        let now_millis = i64::try_from(now_millis).expect("millis fit");
        let page = fetch_candle_snapshot(&CandleSnapshotRequest {
            wire_coin: "BTC",
            period: aeris_market_data::BarPeriod::month(1).expect("month"),
            start_millis: 0,
            end_millis: now_millis,
            now_millis,
            price_scale: crate::NORMALIZED_PRICE_SCALE,
            quantity_scale: crate::NORMALIZED_QUANTITY_SCALE,
            config: HyperliquidHttpConfig::default(),
        })
        .expect("live 1M candle page decodes");
        println!(
            "hyperliquid_live_month closed={} forming={} first={} last={}",
            page.bars.len(),
            page.forming.is_some(),
            page.bars
                .first()
                .map_or(0, |bar| bar.exchange_timestamp_unix_nanos),
            page.bars
                .last()
                .map_or(0, |bar| bar.exchange_timestamp_unix_nanos),
        );
        assert_ne!(page.bars, [] as [aeris_market_data::MarketBar; 0]);
    }
    #[test]
    #[ignore = "drives the live Hyperliquid public info endpoint"]
    fn live_catalog_includes_volume_ranked_markets() {
        let catalog = fetch_meta_bundle(HyperliquidHttpConfig::default()).expect("live catalog");
        let popular = catalog.search("", 10);
        assert_eq!(popular.len(), 10);
        assert!(
            popular
                .iter()
                .any(|instrument| instrument.wire_coin == "BTC")
        );
    }
}
