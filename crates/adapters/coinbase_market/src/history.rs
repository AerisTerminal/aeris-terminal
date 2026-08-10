use crate::{
    CoinbaseNetworkFirstCache, CoinbaseSpotProduct, ENTITLEMENT_CLASS, FixedPointValue,
    coinbase_instrument_id,
};
use axiusflow_market_data::MarketBar;
use axiusflow_provider_history::{
    Continuation, DataClass, DatasetCapability, HistoryCapabilities, HistoryItem, HistoryPage,
    HistoryPageRequest, PaginationStyle, ProviderHistoryAdapter, ProviderHistoryError, RateLimit,
    SequencedHistory,
};
use rustls::{ClientConfig, RootCertStore};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap},
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const REST_HOST: &str = "api.coinbase.com";
const RESPONSE_BYTE_LIMIT: usize = 1_048_576;
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const NETWORK_POLL_INTERVAL: Duration = Duration::from_millis(100);
const NETWORK_RETRY_PAUSE: Duration = Duration::from_millis(1);
const ONE_SECOND_NANOS: i64 = 1_000_000_000;
const ONE_MINUTE_SECONDS: i64 = 60;
const MAXIMUM_PAGE_ITEMS: usize = 350;
const MAXIMUM_PAGINATION_PAGES: usize = 4_096;
const PUBLIC_REQUEST_INTERVAL: Duration = Duration::from_millis(100);
const MAXIMUM_RATE_LIMIT_RETRIES: u32 = 3;
const SUPPORTED_RESOLUTIONS: [&str; 14] = [
    "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "8h", "12h", "1D", "3D", "1W", "1M",
];
const HISTORY_PAYLOAD_MAGIC: &[u8; 6] = b"AXCBH1";
const HISTORY_PAYLOAD_BYTES: usize = HISTORY_PAYLOAD_MAGIC.len() + 7 * 8;
const HISTORY_SEGMENT_MAGIC: &[u8; 6] = b"AXCBS1";
const HISTORY_SEGMENT_HEADER_BYTES: usize = HISTORY_SEGMENT_MAGIC.len() + 4;
static HISTORY_AGENT: OnceLock<ureq::Agent> = OnceLock::new();

type ResolutionResult = io::Result<Vec<SocketAddr>>;

struct ResolutionRequest {
    host: String,
    port: u16,
    maximum_addresses: usize,
    response: mpsc::SyncSender<ResolutionResult>,
}

static RESOLVER: OnceLock<Result<mpsc::SyncSender<ResolutionRequest>, &'static str>> =
    OnceLock::new();

/// Account scope used by Coinbase's credential-free public market-data APIs.
pub const COINBASE_PUBLIC_ACCOUNT_ID: &str = "coinbase_public_market_data";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CoinbaseHistoryDiagnostics {
    pub pages: u64,
    pub candles_received: u64,
    pub duplicates_dropped: u64,
    pub gaps: u64,
    pub requested_start_unix_nanos: i64,
    pub requested_end_unix_nanos: i64,
    pub returned_start_unix_nanos: Option<i64>,
    pub returned_end_unix_nanos: Option<i64>,
    pub provider_rejections: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoinbaseHistoryBatch {
    pub items: Vec<HistoryItem>,
    pub diagnostics: CoinbaseHistoryDiagnostics,
}

#[derive(Deserialize)]
struct CandlesResponse {
    candles: Vec<CandleMessage>,
}

#[derive(Deserialize)]
struct CandleMessage {
    start: String,
    high: String,
    low: String,
    open: String,
    close: String,
    volume: String,
}

/// Bounded HTTP boundary used by the Coinbase history adapter.
pub trait CoinbaseHistoryTransport {
    /// Executes one public provider-relative GET and returns only the response body.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport or HTTP protocol failure.
    fn get(&mut self, path: &str) -> Result<Vec<u8>, String>;
}

/// Pooled HTTPS transport to Coinbase's public Advanced Trade API.
#[derive(Clone, Default)]
pub struct CoinbaseHttpsHistoryTransport {
    stop: Option<Arc<AtomicBool>>,
}

impl CoinbaseHttpsHistoryTransport {
    /// Builds the direct transport without cooperative cancellation.
    #[must_use]
    pub const fn new() -> Self {
        Self { stop: None }
    }

    /// Builds the direct transport cancelled by the shared stop signal.
    #[must_use]
    pub fn with_stop(stop: Arc<AtomicBool>) -> Self {
        Self { stop: Some(stop) }
    }
}

impl CoinbaseHistoryTransport for CoinbaseHttpsHistoryTransport {
    fn get(&mut self, path: &str) -> Result<Vec<u8>, String> {
        https_get(path, self.stop.as_deref())
    }
}

/// Paces, retries, and cooperatively cancels public REST requests.
pub(crate) struct PublicRequestGate {
    interval: Duration,
    stop: Option<Arc<AtomicBool>>,
    last_request: Option<Instant>,
}

impl PublicRequestGate {
    pub(crate) const fn new(stop: Option<Arc<AtomicBool>>) -> Self {
        Self {
            interval: PUBLIC_REQUEST_INTERVAL,
            stop,
            last_request: None,
        }
    }

    pub(crate) fn get<T: CoinbaseHistoryTransport>(
        &mut self,
        transport: &mut T,
        path: &str,
    ) -> Result<Vec<u8>, String> {
        if let Some(last) = self.last_request {
            let remaining = self.interval.saturating_sub(last.elapsed());
            if !remaining.is_zero() {
                sleep_cancellable(remaining, self.stop.as_deref())?;
            }
        }
        self.last_request = Some(Instant::now());
        let mut retries = 0_u32;
        loop {
            match transport.get(path) {
                Err(error) if retries < MAXIMUM_RATE_LIMIT_RETRIES && is_rate_limited(&error) => {
                    retries = retries.saturating_add(1);
                    let backoff = self
                        .interval
                        .max(Duration::from_millis(500))
                        .saturating_mul(2_u32.saturating_pow(retries));
                    sleep_cancellable(backoff, self.stop.as_deref())?;
                    self.last_request = Some(Instant::now());
                }
                result => return result,
            }
        }
    }
}

fn is_rate_limited(error: &str) -> bool {
    error.contains("HTTP 429")
}

fn sleep_cancellable(duration: Duration, stop: Option<&AtomicBool>) -> Result<(), String> {
    let started = Instant::now();
    loop {
        if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            return Err("Coinbase request cancelled".to_string());
        }
        let remaining = duration.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Ok(());
        }
        thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

/// Direct Coinbase public candle-history adapter for desktop background workers.
pub struct CoinbaseHistoryCapabilityAdapter<T = CoinbaseHttpsHistoryTransport> {
    capabilities: HistoryCapabilities,
    transport: T,
    cache: CoinbaseNetworkFirstCache,
    gate: PublicRequestGate,
    products: HashMap<String, ProductPrecision>,
    diagnostics: CoinbaseHistoryDiagnostics,
}

#[derive(Clone)]
struct ProductPrecision {
    product_id: String,
    price_scale: u8,
    quantity_scale: u8,
}

impl CoinbaseHistoryCapabilityAdapter<CoinbaseHttpsHistoryTransport> {
    /// Builds the direct public HTTPS adapter.
    ///
    /// # Errors
    ///
    /// Returns an error if the static capability profile violates shared bounds.
    pub fn try_new() -> Result<Self, ProviderHistoryError> {
        Self::try_with_transport(CoinbaseHttpsHistoryTransport::new())
    }
}

impl<T> CoinbaseHistoryCapabilityAdapter<T> {
    /// Builds the adapter around an explicit bounded transport.
    ///
    /// # Errors
    ///
    /// Returns an error if the static capability profile violates shared bounds.
    pub fn try_with_transport(transport: T) -> Result<Self, ProviderHistoryError> {
        let bars = DatasetCapability::supported(
            SUPPORTED_RESOLUTIONS.map(str::to_string),
            NonZeroU64::MAX,
            NonZeroU64::MAX,
            NonZeroUsize::new(MAXIMUM_PAGE_ITEMS).unwrap_or(NonZeroUsize::MIN),
            PaginationStyle::EndTime,
            RateLimit {
                requests: NonZeroU32::new(10).unwrap_or(NonZeroU32::MIN),
                window_nanos: NonZeroU64::new(ONE_SECOND_NANOS as u64).unwrap_or(NonZeroU64::MIN),
                maximum_inflight: NonZeroUsize::MIN,
            },
        )?;
        let capabilities = HistoryCapabilities::try_new(
            "coinbase".to_string(),
            bars,
            DatasetCapability::unsupported("historical ticks are not implemented"),
            DatasetCapability::unsupported("historical depth is not implemented"),
        )?;
        let products = [("BTC-USD", 2, 8), ("ETH-USD", 2, 8)]
            .into_iter()
            .map(|(product_id, price_scale, quantity_scale)| {
                let instrument_id = coinbase_instrument_id(product_id)
                    .map_err(|_| ProviderHistoryError::InvalidConfiguration("Coinbase product"))?;
                Ok((
                    instrument_id,
                    ProductPrecision {
                        product_id: product_id.to_string(),
                        price_scale,
                        quantity_scale,
                    },
                ))
            })
            .collect::<Result<HashMap<_, _>, ProviderHistoryError>>()?;
        Ok(Self {
            capabilities,
            transport,
            cache: CoinbaseNetworkFirstCache::new(),
            gate: PublicRequestGate::new(None),
            products,
            diagnostics: CoinbaseHistoryDiagnostics::default(),
        })
    }

    /// Installs a cooperative cancellation signal observed between pages,
    /// during pacing sleeps, and by stop-aware transports.
    pub fn set_stop(&mut self, stop: Arc<AtomicBool>) {
        self.gate = PublicRequestGate::new(Some(stop));
    }

    #[must_use]
    pub const fn capabilities(&self) -> &HistoryCapabilities {
        &self.capabilities
    }

    pub fn register_product(&mut self, product: &CoinbaseSpotProduct) {
        self.products.insert(
            product.instrument_id.clone(),
            ProductPrecision {
                product_id: product.product_id.clone(),
                price_scale: product.price_scale,
                quantity_scale: product.quantity_scale,
            },
        );
    }

    #[must_use]
    pub const fn diagnostics(&self) -> CoinbaseHistoryDiagnostics {
        self.diagnostics
    }
}

impl<T: CoinbaseHistoryTransport> ProviderHistoryAdapter for CoinbaseHistoryCapabilityAdapter<T> {
    fn capabilities(&self) -> &HistoryCapabilities {
        &self.capabilities
    }

    fn fetch_page(&mut self, request: &HistoryPageRequest) -> Result<HistoryPage, String> {
        validate_request(request)?;
        let source = history_source_resolution(&request.resolution)?;
        let profile = self
            .products
            .get(&request.instrument_id)
            .cloned()
            .ok_or_else(|| "Coinbase instrument precision profile is unavailable".to_string())?;
        let effective_end = match request.continuation {
            Some(Continuation::EndBeforeUnixNanos(end)) => end,
            _ => request.range.end_unix_nanos,
        };
        let source_nanos = source
            .seconds
            .checked_mul(ONE_SECOND_NANOS)
            .ok_or_else(|| "Coinbase history source interval overflow".to_string())?;
        let page_span = source_nanos
            .checked_mul(
                i64::try_from(request.maximum_items.get()).map_err(|error| error.to_string())?,
            )
            .ok_or_else(|| "Coinbase history page span overflow".to_string())?;
        let effective_start = request
            .range
            .start_unix_nanos
            .max(effective_end.saturating_sub(page_span));
        let start_seconds = effective_start / ONE_SECOND_NANOS;
        let end_seconds = effective_end
            .checked_sub(source_nanos)
            .map(|value| value / ONE_SECOND_NANOS)
            .ok_or_else(|| "Coinbase history end underflow".to_string())?;
        let path = format!(
            "/api/v3/brokerage/market/products/{}/candles?start={start_seconds}&end={end_seconds}&granularity={}&limit={}",
            profile.product_id, source.granularity, request.maximum_items
        );
        let gate = &mut self.gate;
        let transport = &mut self.transport;
        let body = self
            .cache
            .network_first(&path, || gate.get(transport, &path))?;
        let page = parse_page(
            request,
            &body,
            effective_start,
            effective_end,
            profile.price_scale,
            profile.quantity_scale,
            source.seconds,
        )?;
        self.diagnostics.pages = self.diagnostics.pages.saturating_add(1);
        self.diagnostics.candles_received = self
            .diagnostics
            .candles_received
            .saturating_add(page.items.len() as u64);
        Ok(page)
    }
}

impl<T: CoinbaseHistoryTransport> CoinbaseHistoryCapabilityAdapter<T> {
    /// Fetches every end-time page required to cover the requested range.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid requests, transport failures, malformed candles, or bounds.
    pub fn fetch_paginated(
        &mut self,
        request: &HistoryPageRequest,
    ) -> Result<CoinbaseHistoryBatch, String> {
        validate_request(request)?;
        let mut current = request.clone();
        let mut items = BTreeMap::<i64, HistoryItem>::new();
        let mut diagnostics = CoinbaseHistoryDiagnostics {
            requested_start_unix_nanos: request.range.start_unix_nanos,
            requested_end_unix_nanos: request.range.end_unix_nanos,
            ..CoinbaseHistoryDiagnostics::default()
        };
        for _ in 0..MAXIMUM_PAGINATION_PAGES {
            if self
                .gate
                .stop
                .as_ref()
                .is_some_and(|stop| stop.load(Ordering::Acquire))
            {
                return Err("Coinbase history fetch cancelled".to_string());
            }
            let page = match self.fetch_page(&current) {
                Ok(page) => page,
                Err(error) => {
                    self.diagnostics.provider_rejections =
                        self.diagnostics.provider_rejections.saturating_add(1);
                    return Err(error);
                }
            };
            diagnostics.pages = diagnostics.pages.saturating_add(1);
            diagnostics.candles_received = diagnostics
                .candles_received
                .saturating_add(page.items.len() as u64);
            for item in page.items {
                if items.insert(item.event_time_unix_nanos, item).is_some() {
                    diagnostics.duplicates_dropped =
                        diagnostics.duplicates_dropped.saturating_add(1);
                }
            }
            let Some(next) = page.next else {
                let values = items.into_values().collect::<Vec<_>>();
                diagnostics.returned_start_unix_nanos =
                    values.first().map(|item| item.event_time_unix_nanos);
                diagnostics.returned_end_unix_nanos =
                    values.last().map(|item| item.event_time_unix_nanos);
                for pair in values.windows(2) {
                    if pair[1].event_time_unix_nanos - pair[0].event_time_unix_nanos
                        != history_source_resolution(&request.resolution)?.seconds
                            * ONE_SECOND_NANOS
                    {
                        diagnostics.gaps = diagnostics.gaps.saturating_add(1);
                    }
                }
                self.diagnostics.duplicates_dropped = self
                    .diagnostics
                    .duplicates_dropped
                    .saturating_add(diagnostics.duplicates_dropped);
                self.diagnostics.gaps = self.diagnostics.gaps.saturating_add(diagnostics.gaps);
                self.diagnostics.requested_start_unix_nanos =
                    diagnostics.requested_start_unix_nanos;
                self.diagnostics.requested_end_unix_nanos = diagnostics.requested_end_unix_nanos;
                self.diagnostics.returned_start_unix_nanos = diagnostics.returned_start_unix_nanos;
                self.diagnostics.returned_end_unix_nanos = diagnostics.returned_end_unix_nanos;
                return Ok(CoinbaseHistoryBatch {
                    items: values,
                    diagnostics,
                });
            };
            current.continuation = Some(next);
        }
        Err("Coinbase history pagination exceeds its page bound".to_string())
    }
}

/// Decodes one adapter-owned history payload into a validated canonical bar.
///
/// # Errors
///
/// Returns an error if the payload schema, identity, timestamp, or OHLCV values are invalid.
pub fn decode_history_bar(item: &HistoryItem) -> Result<MarketBar, String> {
    if item.payload.len() != HISTORY_PAYLOAD_BYTES
        || &item.payload[..HISTORY_PAYLOAD_MAGIC.len()] != HISTORY_PAYLOAD_MAGIC
    {
        return Err("unsupported Coinbase history payload".to_string());
    }
    let mut offset = HISTORY_PAYLOAD_MAGIC.len();
    let sequence = read_u64(&item.payload, &mut offset)?;
    let timestamp = read_i64(&item.payload, &mut offset)?;
    let bar = MarketBar {
        source_sequence: sequence,
        exchange_timestamp_seconds: timestamp,
        open: read_i64(&item.payload, &mut offset)?,
        high: read_i64(&item.payload, &mut offset)?,
        low: read_i64(&item.payload, &mut offset)?,
        close: read_i64(&item.payload, &mut offset)?,
        volume: read_i64(&item.payload, &mut offset)?,
    };
    let event_time = timestamp
        .checked_mul(ONE_SECOND_NANOS)
        .ok_or_else(|| "Coinbase history timestamp overflow".to_string())?;
    if sequence != item.sequence || event_time != item.event_time_unix_nanos {
        return Err("Coinbase history payload identity mismatch".to_string());
    }
    bar.validate().map_err(|error| error.to_string())?;
    Ok(bar)
}

/// Encodes one bounded, contiguous Coinbase history page for encrypted local retention.
///
/// # Errors
///
/// Returns an error for empty, oversized, malformed, or discontinuous input.
pub fn encode_history_segment(items: &[HistoryItem]) -> Result<Vec<u8>, String> {
    if items.is_empty() || items.len() > MAXIMUM_PAGE_ITEMS {
        return Err("Coinbase history segment item count is invalid".to_string());
    }
    let count = u32::try_from(items.len())
        .map_err(|_| "Coinbase history segment item count overflow".to_string())?;
    let capacity = HISTORY_SEGMENT_HEADER_BYTES
        .checked_add(
            HISTORY_PAYLOAD_BYTES
                .checked_mul(items.len())
                .ok_or_else(|| "Coinbase history segment size overflow".to_string())?,
        )
        .ok_or_else(|| "Coinbase history segment size overflow".to_string())?;
    let mut encoded = Vec::with_capacity(capacity);
    encoded.extend_from_slice(HISTORY_SEGMENT_MAGIC);
    encoded.extend_from_slice(&count.to_le_bytes());
    let mut previous_sequence: Option<u64> = None;
    for item in items {
        let bar = decode_history_bar(item)?;
        if previous_sequence.is_some_and(|previous| previous.checked_add(1) != Some(item.sequence))
        {
            return Err("Coinbase history segment is not contiguous".to_string());
        }
        previous_sequence = Some(bar.source_sequence);
        encoded.extend_from_slice(&item.payload);
    }
    Ok(encoded)
}

/// Decodes one bounded retained Coinbase history segment into canonical bars.
///
/// # Errors
///
/// Returns an error for a wrong version, invalid size, malformed bar, or discontinuity.
pub fn decode_history_segment(encoded: &[u8]) -> Result<Vec<SequencedHistory<MarketBar>>, String> {
    let count = history_segment_item_count(encoded)?;
    let mut values = Vec::with_capacity(count);
    let mut previous_sequence: Option<u64> = None;
    for payload in encoded[HISTORY_SEGMENT_HEADER_BYTES..].chunks_exact(HISTORY_PAYLOAD_BYTES) {
        let mut offset = HISTORY_PAYLOAD_MAGIC.len();
        let sequence = read_u64(payload, &mut offset)?;
        let timestamp = read_i64(payload, &mut offset)?;
        let item = HistoryItem {
            sequence,
            event_time_unix_nanos: timestamp
                .checked_mul(ONE_SECOND_NANOS)
                .ok_or_else(|| "Coinbase history timestamp overflow".to_string())?,
            payload: payload.to_vec(),
        };
        let bar = decode_history_bar(&item)?;
        if previous_sequence.is_some_and(|previous| previous.checked_add(1) != Some(sequence)) {
            return Err("Coinbase history segment is not contiguous".to_string());
        }
        previous_sequence = Some(sequence);
        values.push(SequencedHistory {
            sequence: NonZeroU64::new(sequence)
                .ok_or_else(|| "Coinbase history segment has a zero sequence".to_string())?,
            value: bar,
        });
    }
    Ok(values)
}

/// Validates a retained segment envelope and returns its bounded item count without decoding.
///
/// # Errors
///
/// Returns an error for a wrong version, invalid count, or mismatched encoded size.
pub fn history_segment_item_count(encoded: &[u8]) -> Result<usize, String> {
    if encoded.len() < HISTORY_SEGMENT_HEADER_BYTES
        || &encoded[..HISTORY_SEGMENT_MAGIC.len()] != HISTORY_SEGMENT_MAGIC
    {
        return Err("unsupported Coinbase history segment".to_string());
    }
    let count = u32::from_le_bytes(
        encoded[HISTORY_SEGMENT_MAGIC.len()..HISTORY_SEGMENT_HEADER_BYTES]
            .try_into()
            .map_err(|_| "Coinbase history segment header is truncated".to_string())?,
    );
    let count = usize::try_from(count)
        .ok()
        .filter(|count| *count != 0 && *count <= MAXIMUM_PAGE_ITEMS)
        .ok_or_else(|| "Coinbase history segment item count is invalid".to_string())?;
    let expected = HISTORY_SEGMENT_HEADER_BYTES
        .checked_add(
            HISTORY_PAYLOAD_BYTES
                .checked_mul(count)
                .ok_or_else(|| "Coinbase history segment size overflow".to_string())?,
        )
        .ok_or_else(|| "Coinbase history segment size overflow".to_string())?;
    if encoded.len() != expected {
        return Err("Coinbase history segment size is invalid".to_string());
    }
    Ok(count)
}

fn validate_request(request: &HistoryPageRequest) -> Result<(), String> {
    if request.provider_id != "coinbase"
        || request.account_id != COINBASE_PUBLIC_ACCOUNT_ID
        || request.entitlement_revision != ENTITLEMENT_CLASS
    {
        return Err("Coinbase public history identity mismatch".to_string());
    }
    if request.data_class != DataClass::Bars {
        return Err("unsupported Coinbase history dataset".to_string());
    }
    let source = history_source_resolution(&request.resolution)?;
    let source_nanos = source.seconds * ONE_SECOND_NANOS;
    if let Some(Continuation::EndBeforeUnixNanos(end)) = request.continuation {
        if end <= request.range.start_unix_nanos || end >= request.range.end_unix_nanos {
            return Err("Coinbase history continuation is invalid".to_string());
        }
    } else if request.continuation.is_some() {
        return Err("Coinbase history continuation is invalid".to_string());
    }
    if request.maximum_items.get() > MAXIMUM_PAGE_ITEMS {
        return Err("Coinbase history page exceeds provider limit".to_string());
    }
    request
        .range
        .end_unix_nanos
        .checked_sub(request.range.start_unix_nanos)
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| *value != 0)
        .ok_or_else(|| "Coinbase history range exceeds adapter limit".to_string())?;
    if request.range.start_unix_nanos < 0
        || request.range.start_unix_nanos % source_nanos != 0
        || request.range.end_unix_nanos % source_nanos != 0
    {
        return Err("Coinbase history range is not minute-aligned".to_string());
    }
    let now_unix_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .ok_or_else(|| "system time is unavailable for Coinbase history".to_string())?;
    if request.range.end_unix_nanos > now_unix_nanos {
        return Err("Coinbase history range ends in the future".to_string());
    }
    if !request.instrument_id.starts_with("instrument:coinbase:") {
        return Err("unsupported Coinbase instrument identity".to_string());
    }
    Ok(())
}

fn parse_page(
    request: &HistoryPageRequest,
    body: &[u8],
    effective_start: i64,
    effective_end: i64,
    price_scale: u8,
    quantity_scale: u8,
    source_seconds: i64,
) -> Result<HistoryPage, String> {
    if body.len() > RESPONSE_BYTE_LIMIT {
        return Err("Coinbase history response exceeds byte limit".to_string());
    }
    let parsed: CandlesResponse = serde_json::from_slice(body)
        .map_err(|_| "Coinbase history response is malformed".to_string())?;
    if parsed.candles.len() > MAXIMUM_PAGE_ITEMS {
        return Err("Coinbase history response exceeds item limit".to_string());
    }
    let mut items = parsed
        .candles
        .iter()
        .map(|candle| parse_candle(candle, price_scale, quantity_scale, source_seconds))
        .collect::<Result<Vec<_>, _>>()?;
    items.retain(|item| {
        item.event_time_unix_nanos >= effective_start && item.event_time_unix_nanos < effective_end
    });
    items.sort_by_key(|item| item.sequence);
    if items.len() > request.maximum_items.get() {
        return Err("Coinbase history response exceeds requested item limit".to_string());
    }
    for pair in items.windows(2) {
        if pair[0].sequence >= pair[1].sequence {
            return Err("Coinbase history response contains duplicate candles".to_string());
        }
    }
    Ok(HistoryPage {
        request: request.clone(),
        items,
        next: (effective_start > request.range.start_unix_nanos)
            .then_some(Continuation::EndBeforeUnixNanos(effective_start)),
    })
}

fn parse_candle(
    candle: &CandleMessage,
    price_scale: u8,
    quantity_scale: u8,
    source_seconds: i64,
) -> Result<HistoryItem, String> {
    let timestamp = candle
        .start
        .parse::<i64>()
        .ok()
        .filter(|value| *value >= 0 && *value % source_seconds == 0)
        .ok_or_else(|| "Coinbase candle timestamp is invalid".to_string())?;
    let sequence = u64::try_from(timestamp / ONE_MINUTE_SECONDS)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| "Coinbase candle sequence overflow".to_string())?;
    let bar = MarketBar {
        source_sequence: sequence,
        exchange_timestamp_seconds: timestamp,
        open: fixed_at_scale(&candle.open, u32::from(price_scale))?,
        high: fixed_at_scale(&candle.high, u32::from(price_scale))?,
        low: fixed_at_scale(&candle.low, u32::from(price_scale))?,
        close: fixed_at_scale(&candle.close, u32::from(price_scale))?,
        volume: fixed_at_scale(&candle.volume, u32::from(quantity_scale))?,
    };
    bar.validate().map_err(|error| error.to_string())?;
    let event_time_unix_nanos = timestamp
        .checked_mul(ONE_SECOND_NANOS)
        .ok_or_else(|| "Coinbase candle timestamp overflow".to_string())?;
    Ok(HistoryItem {
        sequence,
        event_time_unix_nanos,
        payload: encode_history_bar(bar),
    })
}

#[derive(Clone, Copy)]
struct HistorySourceResolution {
    granularity: &'static str,
    seconds: i64,
}

fn history_source_resolution(resolution: &str) -> Result<HistorySourceResolution, String> {
    let source = match resolution {
        "1m" | "3m" => ("ONE_MINUTE", 60),
        "5m" => ("FIVE_MINUTE", 300),
        "15m" => ("FIFTEEN_MINUTE", 900),
        "30m" => ("THIRTY_MINUTE", 1_800),
        "1h" => ("ONE_HOUR", 3_600),
        "2h" | "4h" | "8h" => ("TWO_HOUR", 7_200),
        "12h" => ("SIX_HOUR", 21_600),
        "1D" | "3D" | "1W" | "1M" => ("ONE_DAY", 86_400),
        _ => return Err("unsupported Coinbase history resolution".to_string()),
    };
    Ok(HistorySourceResolution {
        granularity: source.0,
        seconds: source.1,
    })
}

fn fixed_at_scale(source: &str, target_scale: u32) -> Result<i64, String> {
    let value = FixedPointValue::parse(source).map_err(|error| error.to_string())?;
    if value.scale == target_scale {
        return Ok(value.mantissa);
    }
    if value.scale < target_scale {
        let shift = target_scale - value.scale;
        return value
            .mantissa
            .checked_mul(10_i64.pow(shift))
            .ok_or_else(|| "Coinbase candle fixed-point overflow".to_string());
    }
    let divisor = 10_i64.pow(value.scale - target_scale);
    if value.mantissa % divisor != 0 {
        return Err("Coinbase candle precision exceeds instrument scale".to_string());
    }
    Ok(value.mantissa / divisor)
}

#[must_use]
pub fn encode_history_bar(bar: MarketBar) -> Vec<u8> {
    let mut payload = Vec::with_capacity(HISTORY_PAYLOAD_BYTES);
    payload.extend_from_slice(HISTORY_PAYLOAD_MAGIC);
    payload.extend_from_slice(&bar.source_sequence.to_le_bytes());
    payload.extend_from_slice(&bar.exchange_timestamp_seconds.to_le_bytes());
    for value in [bar.open, bar.high, bar.low, bar.close, bar.volume] {
        payload.extend_from_slice(&value.to_le_bytes());
    }
    payload
}

fn read_u64(payload: &[u8], offset: &mut usize) -> Result<u64, String> {
    let bytes = payload
        .get(*offset..*offset + 8)
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .ok_or_else(|| "Coinbase history payload is truncated".to_string())?;
    *offset += 8;
    Ok(u64::from_le_bytes(bytes))
}

fn read_i64(payload: &[u8], offset: &mut usize) -> Result<i64, String> {
    let bytes = payload
        .get(*offset..*offset + 8)
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .ok_or_else(|| "Coinbase history payload is truncated".to_string())?;
    *offset += 8;
    Ok(i64::from_le_bytes(bytes))
}

fn https_get(path: &str, stop: Option<&AtomicBool>) -> Result<Vec<u8>, String> {
    if !path.starts_with('/') || path.bytes().any(|byte| byte == b'\r' || byte == b'\n') {
        return Err("invalid Coinbase history path".to_string());
    }
    if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
        return Err("Coinbase request cancelled".to_string());
    }
    let url = format!("https://{REST_HOST}{path}");
    let agent = HISTORY_AGENT.get_or_init(|| {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(IO_TIMEOUT))
            .build();
        ureq::Agent::new_with_config(config)
    });
    let mut response = agent
        .get(&url)
        .header("Accept", "application/json")
        .header("Cache-Control", "no-cache")
        .call()
        .map_err(|error| map_ureq_error(&error))?;
    if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
        return Err("Coinbase request cancelled".to_string());
    }
    response
        .body_mut()
        .with_config()
        .limit(RESPONSE_BYTE_LIMIT as u64)
        .read_to_vec()
        .map_err(|error| format!("Coinbase REST response read failed: {error}"))
}

fn map_ureq_error(error: &ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(code) => format!("Coinbase candle request returned HTTP {code}"),
        ureq::Error::Timeout(_) => "Coinbase REST request timed out".to_string(),
        _ => format!("Coinbase REST request failed: {error}"),
    }
}

pub(crate) fn coinbase_tls_config() -> Result<ClientConfig, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    ClientConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|_| "Coinbase TLS protocol selection failed".to_string())
        .map(|builder| builder.with_root_certificates(roots).with_no_client_auth())
}

pub(crate) struct DeadlineTcpStream {
    stream: TcpStream,
    deadline: Instant,
    next_read_at: Instant,
    stop: Option<Arc<AtomicBool>>,
}

impl DeadlineTcpStream {
    pub(crate) const fn set_deadline(&mut self, deadline: Instant) {
        self.deadline = deadline;
    }

    fn prepare_read(&self) -> io::Result<()> {
        self.stream
            .set_read_timeout(Some(self.operation_timeout()?))
    }

    fn prepare_write(&self) -> io::Result<()> {
        self.stream
            .set_write_timeout(Some(self.operation_timeout()?))
    }

    fn operation_timeout(&self) -> io::Result<Duration> {
        if self
            .stop
            .as_ref()
            .is_some_and(|stop| stop.load(Ordering::Acquire))
        {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Coinbase connection cancelled",
            ));
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Coinbase network deadline exceeded",
            ));
        }
        Ok(remaining.min(NETWORK_POLL_INTERVAL))
    }
}

impl Read for DeadlineTcpStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let now = Instant::now();
        if now < self.next_read_at {
            thread::sleep(self.next_read_at - now);
        }
        self.next_read_at = Instant::now() + NETWORK_RETRY_PAUSE;
        loop {
            self.prepare_read()?;
            match self.stream.read(buffer) {
                Ok(0) if !buffer.is_empty() => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Coinbase connection closed",
                    ));
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    thread::sleep(NETWORK_RETRY_PAUSE);
                }
                result => return result,
            }
        }
    }
}

impl Write for DeadlineTcpStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        loop {
            self.prepare_write()?;
            match self.stream.write(buffer) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    thread::sleep(NETWORK_RETRY_PAUSE);
                }
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        loop {
            self.prepare_write()?;
            match self.stream.flush() {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    thread::sleep(NETWORK_RETRY_PAUSE);
                }
                result => return result,
            }
        }
    }
}

pub(crate) fn connect_coinbase_endpoint_cancellable(
    host: &str,
    port: u16,
    deadline: Instant,
    stop: Option<Arc<AtomicBool>>,
) -> Result<DeadlineTcpStream, String> {
    const MAXIMUM_RESOLVED_ADDRESSES: usize = 16;

    let addresses = resolve_addresses(
        host,
        port,
        deadline,
        MAXIMUM_RESOLVED_ADDRESSES,
        stop.as_deref(),
    )?;
    if addresses.is_empty() {
        return Err("Coinbase endpoint address resolution returned no address".to_string());
    }
    for (index, address) in addresses.iter().enumerate() {
        if stop
            .as_ref()
            .is_some_and(|stop| stop.load(Ordering::Acquire))
        {
            return Err("Coinbase endpoint connection cancelled".to_string());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let attempts_left = u32::try_from(addresses.len() - index)
            .map_err(|_| "Coinbase endpoint address count overflow".to_string())?;
        let attempt_deadline = Instant::now() + remaining / attempts_left;
        match connect_address_cancellable(*address, attempt_deadline, stop.as_deref()) {
            Ok(stream) => {
                stream.set_nodelay(true).map_err(|error| {
                    format!(
                        "Coinbase endpoint socket setup failed ({:?}, os={:?})",
                        error.kind(),
                        error.raw_os_error()
                    )
                })?;
                return Ok(DeadlineTcpStream {
                    stream,
                    deadline,
                    next_read_at: Instant::now(),
                    stop,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                return Err("Coinbase endpoint connection cancelled".to_string());
            }
            Err(_) => {}
        }
    }
    Err("Coinbase endpoint connection failed".to_string())
}

fn connect_address_cancellable(
    address: SocketAddr,
    deadline: Instant,
    stop: Option<&AtomicBool>,
) -> io::Result<TcpStream> {
    const MAXIMUM_CONNECT_ATTEMPTS: usize = 4;
    let mut attempts = 0_usize;
    loop {
        if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Coinbase endpoint connection cancelled",
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Coinbase endpoint connection deadline exceeded",
            ));
        }
        attempts = attempts.saturating_add(1);
        match TcpStream::connect_timeout(&address, remaining.min(NETWORK_POLL_INTERVAL)) {
            Ok(stream) => return Ok(stream),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) && attempts < MAXIMUM_CONNECT_ATTEMPTS => {}
            Err(error) => return Err(error),
        }
    }
}

fn resolve_addresses(
    host: &str,
    port: u16,
    deadline: Instant,
    maximum_addresses: usize,
    stop: Option<&AtomicBool>,
) -> Result<Vec<SocketAddr>, String> {
    let resolver = RESOLVER.get_or_init(start_resolver);
    let resolver = resolver.as_ref().map_err(|error| (*error).to_string())?;
    let (sender, receiver) = mpsc::sync_channel(1);
    let mut request = ResolutionRequest {
        host: host.to_string(),
        port,
        maximum_addresses,
        response: sender,
    };
    loop {
        if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            return Err("Coinbase endpoint resolution cancelled".to_string());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("Coinbase endpoint resolution deadline exceeded".to_string());
        }
        match resolver.try_send(request) {
            Ok(()) => break,
            Err(mpsc::TrySendError::Full(returned)) => {
                request = returned;
                thread::sleep(remaining.min(NETWORK_POLL_INTERVAL));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return Err("Coinbase endpoint resolver failed".to_string());
            }
        }
    }
    loop {
        if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            return Err("Coinbase endpoint resolution cancelled".to_string());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("Coinbase endpoint resolution deadline exceeded".to_string());
        }
        match receiver.recv_timeout(remaining.min(NETWORK_POLL_INTERVAL)) {
            Ok(result) => {
                return result
                    .map_err(|_| "Coinbase endpoint address resolution failed".to_string());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Coinbase endpoint resolver failed".to_string());
            }
        }
    }
}

fn start_resolver() -> Result<mpsc::SyncSender<ResolutionRequest>, &'static str> {
    let (sender, receiver) = mpsc::sync_channel::<ResolutionRequest>(1);
    thread::Builder::new()
        .name("coinbase-dns".to_string())
        .spawn(move || {
            while let Ok(request) = receiver.recv() {
                let result =
                    (request.host.as_str(), request.port)
                        .to_socket_addrs()
                        .map(|addresses| {
                            addresses
                                .take(request.maximum_addresses)
                                .collect::<Vec<_>>()
                        });
                let _ = request.response.send(result);
            }
        })
        .map_err(|_| "Coinbase endpoint resolver startup failed")?;
    Ok(sender)
}

#[cfg(test)]
mod tests {
    use super::{
        COINBASE_PUBLIC_ACCOUNT_ID, CoinbaseHistoryCapabilityAdapter, CoinbaseHistoryTransport,
        decode_history_bar, decode_history_segment, encode_history_segment,
    };
    use crate::ENTITLEMENT_CLASS;
    use axiusflow_provider_history::{
        DataClass, DatasetCapability, HistoryPageRequest, HistoryRange, ProviderHistoryAdapter,
    };
    use std::{
        cell::RefCell,
        num::NonZeroUsize,
        rc::Rc,
        sync::{Arc, atomic::AtomicBool},
    };

    #[derive(Clone)]
    struct FixtureTransport {
        response: Vec<u8>,
        paths: Rc<RefCell<Vec<String>>>,
    }

    impl CoinbaseHistoryTransport for FixtureTransport {
        fn get(&mut self, path: &str) -> Result<Vec<u8>, String> {
            self.paths.borrow_mut().push(path.to_string());
            Ok(self.response.clone())
        }
    }

    fn request(maximum_items: usize) -> HistoryPageRequest {
        HistoryPageRequest {
            provider_id: "coinbase".to_string(),
            account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
            entitlement_revision: ENTITLEMENT_CLASS.to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            data_class: DataClass::Bars,
            resolution: "1m".to_string(),
            range: HistoryRange {
                start_unix_nanos: 1_700_000_040_000_000_000,
                end_unix_nanos: 1_700_000_220_000_000_000,
            },
            maximum_items: NonZeroUsize::new(maximum_items).expect("nonzero fixture limit"),
            continuation: None,
        }
    }

    #[test]
    fn paginated_fetch_retries_bounded_rate_limits() {
        struct FlakyTransport {
            failures: usize,
        }

        impl CoinbaseHistoryTransport for FlakyTransport {
            fn get(&mut self, _path: &str) -> Result<Vec<u8>, String> {
                if self.failures > 0 {
                    self.failures -= 1;
                    return Err("Coinbase candle request returned HTTP 429".to_string());
                }
                Ok(br#"{"candles":[
                    {"start":"1700000040","low":"1.00","high":"1.00","open":"1.00","close":"1.00","volume":"1.00000000"}
                ]}"#
                .to_vec())
            }
        }

        let mut adapter =
            CoinbaseHistoryCapabilityAdapter::try_with_transport(FlakyTransport { failures: 2 })
                .expect("flaky adapter validates");
        let page = adapter
            .fetch_paginated(&request(3))
            .expect("bounded rate limit rejections retry");
        assert_eq!(page.items.len(), 1);

        let mut exhausted =
            CoinbaseHistoryCapabilityAdapter::try_with_transport(FlakyTransport { failures: 5 })
                .expect("exhausted adapter validates");
        assert!(exhausted.fetch_paginated(&request(3)).is_err());
    }

    #[test]
    fn paginated_fetch_observes_cancellation_before_transport() {
        let paths = Rc::new(RefCell::new(Vec::new()));
        let mut adapter = CoinbaseHistoryCapabilityAdapter::try_with_transport(FixtureTransport {
            response: br#"{"candles":[]}"#.to_vec(),
            paths: paths.clone(),
        })
        .expect("cancellable adapter validates");
        let stop = Arc::new(AtomicBool::new(true));
        adapter.set_stop(stop);
        let error = adapter
            .fetch_paginated(&request(3))
            .expect_err("a stopped fetch cancels before transport");
        assert!(error.contains("cancelled"));
        assert!(paths.borrow().is_empty());
    }

    #[test]
    fn profile_enables_only_bounded_one_minute_bars() {
        let profile = CoinbaseHistoryCapabilityAdapter::try_new().expect("profile is valid");
        assert_eq!(profile.capabilities().provider_id(), "coinbase");
        assert!(matches!(
            profile.capabilities().dataset(DataClass::Bars),
            DatasetCapability::Supported { .. }
        ));
        for data_class in [DataClass::Ticks, DataClass::Depth] {
            assert!(matches!(
                profile.capabilities().dataset(data_class),
                DatasetCapability::Unsupported { .. }
            ));
        }
    }

    #[test]
    fn fetches_exact_range_and_decodes_sorted_canonical_bars() {
        let paths = Rc::new(RefCell::new(Vec::new()));
        let response = br#"{"candles":[
            {"start":"1700000160","low":"37000.00","high":"37100.00","open":"37010.00","close":"37090.00","volume":"1.25000000"},
            {"start":"1699999980","low":"36900.00","high":"37020.00","open":"36950.00","close":"37010.00","volume":"2.00000000"},
            {"start":"1700000220","low":"37100.00","high":"37200.00","open":"37110.00","close":"37190.00","volume":"3.00000000"},
            {"start":"1700000100","low":"37020.00","high":"37080.00","open":"37020.00","close":"37070.00","volume":"0.75000000"},
            {"start":"1700000040","low":"36950.00","high":"37050.00","open":"37000.00","close":"37020.00","volume":"0.50000000"}
        ]}"#
        .to_vec();
        let mut adapter = CoinbaseHistoryCapabilityAdapter::try_with_transport(FixtureTransport {
            response,
            paths: paths.clone(),
        })
        .expect("fixture adapter");
        let request = request(3);
        let page = adapter.fetch_page(&request).expect("page fetches");
        assert_eq!(page.request, request);
        assert_eq!(page.items.len(), 3);
        assert!(page.next.is_none());
        assert!(
            page.items
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        let bars = page
            .items
            .iter()
            .map(decode_history_bar)
            .collect::<Result<Vec<_>, _>>()
            .expect("payloads decode");
        assert_eq!(bars[0].exchange_timestamp_seconds, 1_700_000_040);
        assert_eq!(bars[0].open, 3_700_000);
        assert_eq!(bars[0].volume, 50_000_000);
        assert_eq!(bars[2].exchange_timestamp_seconds, 1_700_000_160);
        let encoded = encode_history_segment(&page.items).expect("segment encodes");
        let retained = decode_history_segment(&encoded).expect("segment decodes");
        assert_eq!(
            retained.iter().map(|item| item.value).collect::<Vec<_>>(),
            bars
        );
        assert_eq!(
            paths.borrow().as_slice(),
            [
                "/api/v3/brokerage/market/products/BTC-USD/candles?start=1700000040&end=1700000160&granularity=ONE_MINUTE&limit=3"
            ]
        );
    }

    #[test]
    fn retained_segment_rejects_truncation_corruption_and_discontinuity() {
        let first = super::parse_candle(
            &super::CandleMessage {
                start: "1700000040".to_string(),
                high: "37050.00".to_string(),
                low: "36950.00".to_string(),
                open: "37000.00".to_string(),
                close: "37020.00".to_string(),
                volume: "0.50000000".to_string(),
            },
            2,
            8,
            60,
        )
        .expect("first candle validates");
        let third = super::parse_candle(
            &super::CandleMessage {
                start: "1700000160".to_string(),
                high: "37100.00".to_string(),
                low: "37000.00".to_string(),
                open: "37010.00".to_string(),
                close: "37090.00".to_string(),
                volume: "1.25000000".to_string(),
            },
            2,
            8,
            60,
        )
        .expect("third candle validates");
        assert!(encode_history_segment(&[first.clone(), third]).is_err());

        let mut encoded = encode_history_segment(&[first]).expect("single item encodes");
        encoded.pop();
        assert!(decode_history_segment(&encoded).is_err());
        encoded.push(0);
        let last = encoded.len() - 1;
        encoded[last] ^= 0xff;
        assert!(decode_history_segment(&encoded).is_err());
    }

    #[test]
    fn rejects_unaligned_or_wrong_scope_requests_before_transport() {
        let paths = Rc::new(RefCell::new(Vec::new()));
        let mut adapter = CoinbaseHistoryCapabilityAdapter::try_with_transport(FixtureTransport {
            response: br#"{"candles":[]}"#.to_vec(),
            paths: paths.clone(),
        })
        .expect("fixture adapter");
        let mut wrong_scope = request(3);
        wrong_scope.account_id = "another-account".to_string();
        assert!(adapter.fetch_page(&wrong_scope).is_err());
        let mut unaligned = request(3);
        unaligned.range.start_unix_nanos += 1;
        assert!(adapter.fetch_page(&unaligned).is_err());
        let too_many_page_items = request(351);
        assert!(adapter.fetch_page(&too_many_page_items).is_err());
        let mut noncanonical = request(3);
        noncanonical.instrument_id = "instrument:coinbase:BTC:usd".to_string();
        assert!(adapter.fetch_page(&noncanonical).is_err());
        let mut unavailable_precision = request(3);
        unavailable_precision.instrument_id = "instrument:coinbase:shib:usd".to_string();
        assert!(adapter.fetch_page(&unavailable_precision).is_err());
        assert!(paths.borrow().is_empty());
    }

    #[test]
    fn rejects_duplicate_and_over_precise_candles() {
        let paths = Rc::new(RefCell::new(Vec::new()));
        let duplicate = br#"{"candles":[
            {"start":"1700000040","low":"1.00","high":"1.00","open":"1.00","close":"1.00","volume":"1.00000000"},
            {"start":"1700000040","low":"1.00","high":"1.00","open":"1.00","close":"1.00","volume":"1.00000000"}
        ]}"#
        .to_vec();
        let mut adapter = CoinbaseHistoryCapabilityAdapter::try_with_transport(FixtureTransport {
            response: duplicate,
            paths: paths.clone(),
        })
        .expect("fixture adapter");
        assert!(adapter.fetch_page(&request(3)).is_err());

        let over_precise = br#"{"candles":[
            {"start":"1700000040","low":"1.001","high":"1.001","open":"1.001","close":"1.001","volume":"1.00000000"}
        ]}"#
        .to_vec();
        let mut adapter = CoinbaseHistoryCapabilityAdapter::try_with_transport(FixtureTransport {
            response: over_precise,
            paths,
        })
        .expect("fixture adapter");
        assert!(adapter.fetch_page(&request(3)).is_err());
    }

    #[test]
    fn payload_identity_is_bound_to_the_history_item() {
        let paths = Rc::new(RefCell::new(Vec::new()));
        let response = br#"{"candles":[
            {"start":"1700000040","low":"1.00","high":"1.00","open":"1.00","close":"1.00","volume":"1.00000000"}
        ]}"#
        .to_vec();
        let mut adapter = CoinbaseHistoryCapabilityAdapter::try_with_transport(FixtureTransport {
            response,
            paths,
        })
        .expect("fixture adapter");
        let mut item = adapter
            .fetch_page(&request(3))
            .expect("page")
            .items
            .pop()
            .expect("item");
        assert!(decode_history_bar(&item).is_ok());
        item.sequence += 1;
        assert!(decode_history_bar(&item).is_err());
    }
}
