//! Public OKX V5 Perpetual Swap historical and live liquidation orders over HTTPS and WebSocket.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use aeris_crypto_common::{
    CryptoError, CryptoResult, LiquidationEvent, LiquidationLedger, bucket_liquidation_events,
};
use aeris_market_data::BarPeriod;
use aeris_platform_runtime::CancellableHttpClient;
use aeris_provider_spi::{LiquidationRequest, LiquidationSample};
use serde::Deserialize;

use crate::{
    endpoints::OKX_REST_URL,
    http::{OkxHttpConfig, get_with_agent},
    identity::{OkxMarketKind, parse_provider_symbol},
};

const OKX_LIQ_MAX_PAGES: usize = 4;
const OKX_LIQ_REST_CACHE_TTL: Duration = Duration::from_secs(30);
const OKX_LIQ_REST_CACHE_CAPACITY: usize = 32;
const FIXED_SCALE: f64 = 100_000_000.0;

type RestCacheEntry = (Instant, Vec<LiquidationEvent>);
type RestCacheMap = BTreeMap<String, RestCacheEntry>;

static OKX_LIQ_REST_CACHE: OnceLock<Mutex<RestCacheMap>> = OnceLock::new();
static OKX_LIQ_HTTP_AGENT: OnceLock<ureq::Agent> = OnceLock::new();

/// Converts a fixed-point (`1e8`) OKX `ctVal` into a floating-point contract multiplier.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn ct_val_scale(fixed: i64) -> f64 {
    if fixed > 0 {
        fixed as f64 / FIXED_SCALE
    } else {
        1.0
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireOkxLiquidationGroup {
    #[serde(default)]
    inst_id: String,
    #[serde(default)]
    details: Vec<WireOkxLiquidationDetail>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireOkxLiquidationDetail {
    #[serde(default)]
    pos_side: String,
    #[serde(default)]
    side: String,
    #[serde(default)]
    bk_px: String,
    #[serde(default)]
    sz: String,
    #[serde(default)]
    ts: String,
}

#[derive(Deserialize)]
struct WireOkxRestResponse {
    #[serde(default)]
    code: String,
    #[serde(default)]
    data: Vec<WireOkxLiquidationGroup>,
}

/// Decodes one OKX V5 `liquidation-orders` WebSocket `data` array into
/// `(inst_id, LiquidationEvent)` entries, scaling contract counts by each instrument's `ctVal`.
///
/// # Errors
/// Returns an error if the JSON payload is malformed.
pub fn decode_ws_liquidations(
    raw: &serde_json::value::RawValue,
    ct_val_for: &dyn Fn(&str) -> f64,
) -> CryptoResult<Vec<(String, LiquidationEvent)>> {
    let groups: Vec<WireOkxLiquidationGroup> = serde_json::from_str(raw.get()).map_err(|_| {
        CryptoError::MalformedPayload("okx liquidation-orders payload is malformed".to_string())
    })?;
    let mut out = Vec::new();
    for group in groups {
        let inst_id = group.inst_id.to_ascii_uppercase();
        if inst_id.is_empty() {
            continue;
        }
        let ct_val = ct_val_for(&inst_id);
        for detail in group.details {
            if let Some(event) = decode_detail(&detail, ct_val) {
                out.push((inst_id.clone(), event));
            }
        }
    }
    Ok(out)
}

/// Fetches historical filled liquidation orders for an OKX SWAP `instId` (e.g. `BTC-USDT-SWAP`)
/// over OKX's public REST API, reusing a bounded 30-second per-symbol cache so rapid live refreshes
/// do not repeatedly hit REST.
///
/// # Errors
/// Returns an error on cancellation, transport failure, or malformed payload.
pub fn fetch_swap_liquidations_rest(
    inst_id: &str,
    ct_val: f64,
    stop: &Arc<AtomicBool>,
) -> CryptoResult<Vec<LiquidationEvent>> {
    if stop.load(Ordering::Acquire) {
        return Err(CryptoError::Transport(
            "OKX liquidations request was cancelled".to_string(),
        ));
    }
    let Some(uly) = inst_id.strip_suffix("-SWAP") else {
        return Ok(Vec::new());
    };
    let cache = OKX_LIQ_REST_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let now = Instant::now();
    if let Some((fetched_at, events)) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(inst_id)
        && now.saturating_duration_since(*fetched_at) <= OKX_LIQ_REST_CACHE_TTL
    {
        return Ok(events.clone());
    }

    let agent = OKX_LIQ_HTTP_AGENT.get_or_init(|| CancellableHttpClient::default().into_agent());
    let config = OkxHttpConfig::default();

    let mut events = Vec::new();
    let mut after_ts: Option<i64> = None;

    for _ in 0..OKX_LIQ_MAX_PAGES {
        if stop.load(Ordering::Acquire) {
            return Err(CryptoError::Transport(
                "OKX liquidations request was cancelled".to_string(),
            ));
        }
        let url = match after_ts {
            Some(ts) => format!(
                "{OKX_REST_URL}/api/v5/public/liquidation-orders?instType=SWAP&uly={uly}&state=filled&limit=100&after={ts}"
            ),
            None => format!(
                "{OKX_REST_URL}/api/v5/public/liquidation-orders?instType=SWAP&uly={uly}&state=filled&limit=100"
            ),
        };
        let bytes = get_with_agent(agent, &url, config)?;
        let (page, oldest_ts) = decode_rest_page(&bytes, inst_id, ct_val)?;
        let page_len = page.len();
        events.extend(page);
        let Some(next_after) = oldest_ts.filter(|&ts| after_ts.is_none_or(|prev| ts < prev)) else {
            break;
        };
        after_ts = Some(next_after);
        if page_len < 100 {
            break;
        }
    }

    let mut guard = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.len() >= OKX_LIQ_REST_CACHE_CAPACITY
        && !guard.contains_key(inst_id)
        && let Some(oldest_key) = guard
            .iter()
            .min_by_key(|(_, (fetched_at, _))| *fetched_at)
            .map(|(sym, _)| sym.clone())
    {
        guard.remove(&oldest_key);
    }
    guard.insert(inst_id.to_string(), (Instant::now(), events.clone()));
    Ok(events)
}

/// Fetches historical REST liquidations and merges them with live WebSocket liquidations for an
/// OKX Perpetual Swap instrument. Spot instruments return an empty series.
///
/// # Errors
/// Returns an error on cancellation, invalid symbol, or REST failure when no live events exist.
pub fn fetch_liquidations(
    ledger: &LiquidationLedger,
    ct_vals: &RwLock<BTreeMap<String, i64>>,
    request: &LiquidationRequest,
    stop: &Arc<AtomicBool>,
) -> Result<Vec<LiquidationSample>, String> {
    if stop.load(Ordering::Acquire) {
        return Err("OKX liquidations request was cancelled".to_string());
    }
    let sym_ref = parse_provider_symbol(&request.instrument.provider_symbol)
        .map_err(|error| error.to_string())?;
    if sym_ref.kind != OkxMarketKind::Swap {
        return Ok(Vec::new());
    }
    let ct_val = ct_vals
        .read()
        .ok()
        .and_then(|map| map.get(&sym_ref.inst_id).copied())
        .map_or(1.0, ct_val_scale);
    fetch_swap_liquidations(
        ledger,
        &sym_ref.inst_id,
        ct_val,
        request.period,
        request.maximum_points,
        stop,
    )
}

/// Fetches and merges REST + live WS liquidations for an OKX SWAP `instId`.
///
/// # Errors
/// Returns an error on cancellation or when REST fails and the live ledger is empty.
pub fn fetch_swap_liquidations(
    ledger: &LiquidationLedger,
    inst_id: &str,
    ct_val: f64,
    period: BarPeriod,
    maximum_points: usize,
    stop: &Arc<AtomicBool>,
) -> Result<Vec<LiquidationSample>, String> {
    let mut combined = ledger.events_for(inst_id);
    match fetch_swap_liquidations_rest(inst_id, ct_val, stop) {
        Ok(rest_events) => combined.extend(rest_events),
        Err(error) if combined.is_empty() => return Err(error.to_string()),
        Err(_) => {}
    }
    Ok(bucket_liquidation_events(&combined, period, maximum_points)
        .into_iter()
        .map(|bucket| LiquidationSample {
            timestamp_unix_nanos: bucket.timestamp_unix_nanos,
            long_usd: bucket.long_usd,
            short_usd: bucket.short_usd,
            long_contracts: bucket.long_contracts,
            short_contracts: bucket.short_contracts,
        })
        .collect())
}

fn decode_rest_page(
    bytes: &[u8],
    target_inst_id: &str,
    ct_val: f64,
) -> CryptoResult<(Vec<LiquidationEvent>, Option<i64>)> {
    let parsed: WireOkxRestResponse = serde_json::from_slice(bytes).map_err(|_| {
        CryptoError::MalformedPayload(
            "okx liquidation-orders REST payload is malformed".to_string(),
        )
    })?;
    if parsed.code != "0" {
        return Err(CryptoError::Transport(format!(
            "OKX liquidation-orders rejected (code={})",
            parsed.code
        )));
    }
    let mut out = Vec::new();
    let mut oldest_ts: Option<i64> = None;
    for group in parsed.data {
        if !group.inst_id.eq_ignore_ascii_case(target_inst_id) {
            continue;
        }
        for detail in group.details {
            if let Some(event) = decode_detail(&detail, ct_val) {
                oldest_ts =
                    Some(oldest_ts.map_or(event.timestamp_ms, |prev| prev.min(event.timestamp_ms)));
                out.push(event);
            }
        }
    }
    Ok((out, oldest_ts))
}

fn decode_detail(detail: &WireOkxLiquidationDetail, ct_val: f64) -> Option<LiquidationEvent> {
    let is_long = match detail.pos_side.as_str() {
        "long" => true,
        "short" => false,
        "net" | "" => match detail.side.as_str() {
            "sell" => true,
            "buy" => false,
            _ => return None,
        },
        _ => return None,
    };
    let timestamp_ms = detail.ts.parse::<i64>().ok().filter(|&ts| ts > 0)?;
    let raw_sz = detail
        .sz
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)?;
    let price = detail
        .bk_px
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)?;
    let scale = if ct_val.is_finite() && ct_val > 0.0 {
        ct_val
    } else {
        1.0
    };
    let contracts = raw_sz * scale;
    Some(LiquidationEvent {
        timestamp_ms,
        is_long,
        contracts,
        usd: contracts * price,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_okx_rest_and_ws_liquidation_orders() {
        let payload = br#"{
            "code": "0",
            "data": [
                {
                    "instId": "BTC-USDT-SWAP",
                    "instType": "SWAP",
                    "details": [
                        {"bkPx":"60000.0","posSide":"long","side":"sell","sz":"20","ts":"1710000001000"},
                        {"bkPx":"60500.0","posSide":"short","side":"buy","sz":"10","ts":"1710000002000"},
                        {"bkPx":"60200.0","posSide":"net","side":"sell","sz":"5","ts":"1710000003000"}
                    ]
                }
            ],
            "msg": ""
        }"#;
        let (events, oldest) = decode_rest_page(payload, "BTC-USDT-SWAP", 0.01).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(oldest, Some(1_710_000_001_000));
        assert!(events[0].is_long);
        assert!((events[0].contracts - 0.2).abs() < 1e-9);
        assert!((events[0].usd - 12_000.0).abs() < 1e-6);
        assert!(!events[1].is_long);
        assert!((events[1].contracts - 0.1).abs() < 1e-9);
        assert!((events[1].usd - 6_050.0).abs() < 1e-6);
        assert!(events[2].is_long);
    }

    #[test]
    #[ignore = "live network check against OKX public liquidation-orders API"]
    fn live_okx_swap_liquidations_rest_decodes_real_fills() {
        let stop = Arc::new(AtomicBool::new(false));
        let events = fetch_swap_liquidations_rest("BTC-USDT-SWAP", 0.01, &stop)
            .expect("live OKX liquidation-orders fetch");
        assert!(
            !events.is_empty(),
            "BTC-USDT-SWAP should return historical liquidations"
        );
        assert!(events.iter().all(LiquidationEvent::is_valid));
    }
}
