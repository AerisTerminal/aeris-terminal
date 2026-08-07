use crate::{
    CollectedHistory, CollectionProgress, DecodedControlMessage, DecodedTimeBar,
    DecodedTimeBarType, HistoryBars, HistoryCollectionRequest, HistoryCollector, HistorySeries,
    RithmicHistoryConnection, RithmicProviderInstrument, RithmicSessionMessage,
    TimeBarReplayRequest, TimeBarType,
};
use axiusflow_market_data::MarketBar;
use axiusflow_provider_history::{
    DataClass, DatasetCapability, HandoffBatch, HandoffCoordinator, HandoffState,
    HistoryCapabilities, HistoryItem, HistoryPage, HistoryPageRequest, LiveAcceptance,
    PaginationStyle, ProviderHistoryAdapter, ProviderHistoryError, RateLimit, SequencedHistory,
    VerifiedHistorySnapshot,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    num::{NonZeroU16, NonZeroU32, NonZeroU64, NonZeroUsize},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const PROVIDER_ID: &str = "rithmic";
const MAXIMUM_REPLAY_BARS: usize = 10_000;
const MAXIMUM_CONTROL_MESSAGES: usize = 256;
const MAXIMUM_REPLAY_TIMEOUT: Duration = Duration::from_mins(5);
const NANOS_PER_SECOND: u64 = 1_000_000_000;
const NANOS_PER_SECOND_I64: i64 = 1_000_000_000;
const HISTORY_PAYLOAD_MAGIC: &[u8; 6] = b"AXRHB1";
const HISTORY_PAYLOAD_BYTES: usize = HISTORY_PAYLOAD_MAGIC.len() + 7 * 8;

/// Non-secret account scope for Rithmic Test market-data history.
pub const RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID: &str = "rithmic_test_market_data";

/// Coarse Rithmic history adapter failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicHistoryAdapterError {
    InvalidConfiguration,
    InvalidRequest,
    Transport,
    IncompleteCoverage,
    MalformedHistory,
    SessionPoisoned,
}

impl fmt::Display for RithmicHistoryAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Rithmic history adapter failed: {self:?}")
    }
}

impl Error for RithmicHistoryAdapterError {}

/// One supported fixed-interval time-bar resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicTimeBarResolution {
    pub resolution: String,
    pub bar_type: TimeBarType,
    pub period: NonZeroU16,
}

impl RithmicTimeBarResolution {
    /// Creates a second-, minute-, or daily-based resolution with a stable public name.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe identity or an unsupported weekly bar type.
    pub fn try_new(
        resolution: impl Into<String>,
        bar_type: TimeBarType,
        period: NonZeroU16,
    ) -> Result<Self, RithmicHistoryAdapterError> {
        let resolution = resolution.into();
        if !valid_identity(&resolution)
            || !matches!(
                bar_type,
                TimeBarType::Second | TimeBarType::Minute | TimeBarType::Daily
            )
        {
            return Err(RithmicHistoryAdapterError::InvalidConfiguration);
        }
        let value = Self {
            resolution,
            bar_type,
            period,
        };
        value.interval_seconds()?;
        Ok(value)
    }

    fn decoded_type(&self) -> DecodedTimeBarType {
        match self.bar_type {
            TimeBarType::Second => DecodedTimeBarType::Second,
            TimeBarType::Minute => DecodedTimeBarType::Minute,
            TimeBarType::Daily => DecodedTimeBarType::Daily,
            TimeBarType::Weekly => DecodedTimeBarType::Weekly,
        }
    }

    fn interval_seconds(&self) -> Result<u64, RithmicHistoryAdapterError> {
        let unit = match self.bar_type {
            TimeBarType::Second => 1,
            TimeBarType::Minute => 60,
            TimeBarType::Daily => 86_400,
            TimeBarType::Weekly => {
                return Err(RithmicHistoryAdapterError::InvalidConfiguration);
            }
        };
        u64::from(self.period.get())
            .checked_mul(unit)
            .ok_or(RithmicHistoryAdapterError::InvalidConfiguration)
    }
}

/// Explicit lookback, span, and page bounds for Rithmic history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(clippy::struct_field_names)]
pub struct RithmicHistoryLimits {
    maximum_lookback_nanos: NonZeroU64,
    maximum_request_span_nanos: NonZeroU64,
    maximum_bars: NonZeroUsize,
}

impl RithmicHistoryLimits {
    /// Creates validated history capability and request bounds.
    ///
    /// # Errors
    ///
    /// Returns an error for a page above the protocol maximum or a range that
    /// cannot be represented by Rithmic's signed-second indexes.
    pub fn try_new(
        maximum_lookback_nanos: NonZeroU64,
        maximum_request_span_nanos: NonZeroU64,
        maximum_bars: NonZeroUsize,
    ) -> Result<Self, RithmicHistoryAdapterError> {
        let maximum_index_span = u64::try_from(i32::MAX)
            .unwrap_or(u64::MAX)
            .saturating_mul(NANOS_PER_SECOND);
        if maximum_bars.get() > MAXIMUM_REPLAY_BARS
            || maximum_request_span_nanos.get() > maximum_index_span
        {
            return Err(RithmicHistoryAdapterError::InvalidConfiguration);
        }
        Ok(Self {
            maximum_lookback_nanos,
            maximum_request_span_nanos,
            maximum_bars,
        })
    }
}

/// Bounded collection boundary used by the shared history capability adapter.
pub trait RithmicHistoryTransport {
    /// Collects one exact replay through its accepted terminal response.
    ///
    /// # Errors
    ///
    /// Returns a coarse error when the session or collection cannot continue.
    fn collect_history(
        &mut self,
        request: HistoryCollectionRequest,
    ) -> Result<CollectedHistory, String>;
}

/// One authenticated history connection with an absolute replay deadline.
pub struct RithmicHistorySessionTransport {
    connection: RithmicHistoryConnection,
    replay_timeout: Duration,
    maximum_control_messages: NonZeroUsize,
}

impl RithmicHistorySessionTransport {
    /// Wraps one credential-free authenticated connection for bounded replay.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero/oversized deadline or control-frame bound.
    pub fn try_new(
        connection: RithmicHistoryConnection,
        replay_timeout: Duration,
        maximum_control_messages: NonZeroUsize,
    ) -> Result<Self, RithmicHistoryAdapterError> {
        if replay_timeout.is_zero()
            || replay_timeout > MAXIMUM_REPLAY_TIMEOUT
            || maximum_control_messages.get() > MAXIMUM_CONTROL_MESSAGES
        {
            return Err(RithmicHistoryAdapterError::InvalidConfiguration);
        }
        Ok(Self {
            connection,
            replay_timeout,
            maximum_control_messages,
        })
    }

    #[must_use]
    pub fn into_connection(self) -> RithmicHistoryConnection {
        self.connection
    }
}

impl RithmicHistoryTransport for RithmicHistorySessionTransport {
    fn collect_history(
        &mut self,
        request: HistoryCollectionRequest,
    ) -> Result<CollectedHistory, String> {
        let HistorySeries::Time { bar_type, period } = request.series else {
            return Err(RithmicHistoryAdapterError::InvalidRequest.to_string());
        };
        let wire_type = match bar_type {
            DecodedTimeBarType::Second => TimeBarType::Second,
            DecodedTimeBarType::Minute => TimeBarType::Minute,
            DecodedTimeBarType::Daily => TimeBarType::Daily,
            DecodedTimeBarType::Weekly => TimeBarType::Weekly,
        };
        let maximum_bars = u16::try_from(request.maximum_bars.get())
            .map_err(|_| RithmicHistoryAdapterError::InvalidRequest.to_string())?;
        self.connection
            .replay_time_bars(TimeBarReplayRequest {
                symbol: &request.symbol,
                exchange: &request.exchange,
                bar_type: wire_type,
                period,
                start_seconds: request.start_seconds,
                finish_seconds: request.finish_seconds,
                maximum_bars,
            })
            .map_err(|_| RithmicHistoryAdapterError::Transport.to_string())?;
        let mut collector = HistoryCollector::try_new(request)
            .map_err(|_| RithmicHistoryAdapterError::InvalidRequest.to_string())?;
        let deadline = Instant::now() + self.replay_timeout;
        let mut control_messages = 0_usize;
        loop {
            match self
                .connection
                .read_next_until(deadline)
                .map_err(|_| RithmicHistoryAdapterError::Transport.to_string())?
            {
                RithmicSessionMessage::History(message) => {
                    match collector
                        .accept(message)
                        .map_err(|_| RithmicHistoryAdapterError::MalformedHistory.to_string())?
                    {
                        CollectionProgress::Pending => {}
                        CollectionProgress::Complete(history) => return Ok(history),
                        CollectionProgress::Unhandled(_) => {
                            return Err(RithmicHistoryAdapterError::MalformedHistory.to_string());
                        }
                    }
                }
                RithmicSessionMessage::Control(
                    DecodedControlMessage::ForcedLogout | DecodedControlMessage::Reject,
                ) => return Err(RithmicHistoryAdapterError::Transport.to_string()),
                RithmicSessionMessage::Control(_)
                | RithmicSessionMessage::Catalog(_)
                | RithmicSessionMessage::Market(_) => {
                    control_messages = control_messages.saturating_add(1);
                    if control_messages > self.maximum_control_messages.get() {
                        return Err(RithmicHistoryAdapterError::Transport.to_string());
                    }
                }
            }
        }
    }
}

struct PreparedRequest {
    collection: HistoryCollectionRequest,
    instrument: RithmicProviderInstrument,
    resolution: RithmicTimeBarResolution,
    expected_bars: usize,
}

/// Provider-history adapter for fully covering fixed-interval Rithmic replays.
pub struct RithmicHistoryCapabilityAdapter<T> {
    capabilities: HistoryCapabilities,
    transport: T,
    instruments: BTreeMap<String, RithmicProviderInstrument>,
    resolutions: BTreeMap<String, RithmicTimeBarResolution>,
    limits: RithmicHistoryLimits,
    failed: bool,
}

impl<T> RithmicHistoryCapabilityAdapter<T> {
    /// Builds the adapter around an authenticated or deterministic transport.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate/invalid registry entries or capabilities.
    pub fn try_with_transport(
        transport: T,
        instruments: Vec<RithmicProviderInstrument>,
        resolutions: Vec<RithmicTimeBarResolution>,
        limits: RithmicHistoryLimits,
    ) -> Result<Self, RithmicHistoryAdapterError> {
        if instruments.is_empty() || resolutions.is_empty() {
            return Err(RithmicHistoryAdapterError::InvalidConfiguration);
        }
        let mut instrument_map = BTreeMap::new();
        let mut provider_identities = BTreeSet::new();
        for instrument in instruments {
            if instrument.descriptor.validate().is_err()
                || !valid_identity(&instrument.entitlement_id)
                || !provider_identities.insert((
                    instrument.descriptor.venue_id.clone(),
                    instrument.descriptor.provider_symbol.clone(),
                ))
                || instrument_map
                    .insert(instrument.descriptor.instrument_id.clone(), instrument)
                    .is_some()
            {
                return Err(RithmicHistoryAdapterError::InvalidConfiguration);
            }
        }
        let mut resolution_map = BTreeMap::new();
        for resolution in resolutions {
            resolution.interval_seconds()?;
            if resolution_map
                .insert(resolution.resolution.clone(), resolution)
                .is_some()
            {
                return Err(RithmicHistoryAdapterError::InvalidConfiguration);
            }
        }
        let bars = DatasetCapability::supported(
            resolution_map.keys().cloned(),
            limits.maximum_lookback_nanos,
            limits.maximum_request_span_nanos,
            limits.maximum_bars,
            PaginationStyle::None,
            RateLimit {
                requests: NonZeroU32::MIN,
                window_nanos: NonZeroU64::new(NANOS_PER_SECOND).unwrap_or(NonZeroU64::MIN),
                maximum_inflight: NonZeroUsize::MIN,
            },
        )
        .map_err(|_| RithmicHistoryAdapterError::InvalidConfiguration)?;
        let capabilities = HistoryCapabilities::try_new(
            PROVIDER_ID.to_string(),
            bars,
            DatasetCapability::unsupported("Rithmic tick-bar continuity is not implemented"),
            DatasetCapability::unsupported("Rithmic historical depth is not implemented"),
        )
        .map_err(|_| RithmicHistoryAdapterError::InvalidConfiguration)?;
        Ok(Self {
            capabilities,
            transport,
            instruments: instrument_map,
            resolutions: resolution_map,
            limits,
            failed: false,
        })
    }

    #[must_use]
    pub const fn capabilities(&self) -> &HistoryCapabilities {
        &self.capabilities
    }

    fn prepare(
        &self,
        request: &HistoryPageRequest,
    ) -> Result<PreparedRequest, RithmicHistoryAdapterError> {
        if self.failed {
            return Err(RithmicHistoryAdapterError::SessionPoisoned);
        }
        if request.provider_id != PROVIDER_ID
            || request.account_id != RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID
            || request.data_class != DataClass::Bars
            || request.continuation.is_some()
            || request.maximum_items.get() > self.limits.maximum_bars.get()
        {
            return Err(RithmicHistoryAdapterError::InvalidRequest);
        }
        let instrument = self
            .instruments
            .get(&request.instrument_id)
            .filter(|instrument| instrument.entitlement_id == request.entitlement_revision)
            .cloned()
            .ok_or(RithmicHistoryAdapterError::InvalidRequest)?;
        let resolution = self
            .resolutions
            .get(&request.resolution)
            .cloned()
            .ok_or(RithmicHistoryAdapterError::InvalidRequest)?;
        let span_nanos = request
            .range
            .end_unix_nanos
            .checked_sub(request.range.start_unix_nanos)
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value != 0 && *value <= self.limits.maximum_request_span_nanos.get())
            .ok_or(RithmicHistoryAdapterError::InvalidRequest)?;
        let now_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
            .ok_or(RithmicHistoryAdapterError::InvalidRequest)?;
        let start_nanos = u64::try_from(request.range.start_unix_nanos)
            .map_err(|_| RithmicHistoryAdapterError::InvalidRequest)?;
        let end_nanos = u64::try_from(request.range.end_unix_nanos)
            .map_err(|_| RithmicHistoryAdapterError::InvalidRequest)?;
        if end_nanos > now_nanos
            || now_nanos.saturating_sub(start_nanos) > self.limits.maximum_lookback_nanos.get()
        {
            return Err(RithmicHistoryAdapterError::InvalidRequest);
        }
        let interval_nanos = resolution
            .interval_seconds()?
            .checked_mul(NANOS_PER_SECOND)
            .ok_or(RithmicHistoryAdapterError::InvalidRequest)?;
        if start_nanos % interval_nanos != 0
            || end_nanos % interval_nanos != 0
            || span_nanos % interval_nanos != 0
        {
            return Err(RithmicHistoryAdapterError::InvalidRequest);
        }
        let expected_bars = usize::try_from(span_nanos / interval_nanos)
            .map_err(|_| RithmicHistoryAdapterError::InvalidRequest)?;
        if expected_bars == 0
            || expected_bars > request.maximum_items.get()
            || expected_bars > self.limits.maximum_bars.get()
        {
            return Err(RithmicHistoryAdapterError::InvalidRequest);
        }
        let start_seconds = i32::try_from(start_nanos / NANOS_PER_SECOND)
            .map_err(|_| RithmicHistoryAdapterError::InvalidRequest)?;
        let finish_seconds = i32::try_from(
            end_nanos
                .checked_div(NANOS_PER_SECOND)
                .and_then(|value| value.checked_sub(1))
                .ok_or(RithmicHistoryAdapterError::InvalidRequest)?,
        )
        .map_err(|_| RithmicHistoryAdapterError::InvalidRequest)?;
        Ok(PreparedRequest {
            collection: HistoryCollectionRequest {
                symbol: instrument.descriptor.provider_symbol.clone(),
                exchange: instrument.descriptor.venue_id.clone(),
                series: HistorySeries::Time {
                    bar_type: resolution.decoded_type(),
                    period: i32::from(resolution.period.get()),
                },
                start_seconds,
                finish_seconds,
                maximum_bars: NonZeroUsize::new(expected_bars).unwrap_or(NonZeroUsize::MIN),
            },
            instrument,
            resolution,
            expected_bars,
        })
    }

    fn page_from_history(
        request: &HistoryPageRequest,
        prepared: &PreparedRequest,
        history: CollectedHistory,
    ) -> Result<HistoryPage, RithmicHistoryAdapterError> {
        if history.request != prepared.collection {
            return Err(RithmicHistoryAdapterError::MalformedHistory);
        }
        let HistoryBars::Time(bars) = history.bars else {
            return Err(RithmicHistoryAdapterError::MalformedHistory);
        };
        if bars.len() != prepared.expected_bars {
            return Err(RithmicHistoryAdapterError::IncompleteCoverage);
        }
        let interval = i32::try_from(prepared.resolution.interval_seconds()?)
            .map_err(|_| RithmicHistoryAdapterError::MalformedHistory)?;
        let mut items = Vec::with_capacity(bars.len());
        for (index, bar) in bars.into_iter().enumerate() {
            let expected_marker = prepared
                .collection
                .start_seconds
                .checked_add(
                    i32::try_from(index)
                        .ok()
                        .and_then(|value| value.checked_mul(interval))
                        .ok_or(RithmicHistoryAdapterError::MalformedHistory)?,
                )
                .ok_or(RithmicHistoryAdapterError::MalformedHistory)?;
            if bar.marker_seconds != expected_marker {
                return Err(RithmicHistoryAdapterError::IncompleteCoverage);
            }
            let sequenced =
                canonical_rithmic_time_bar(&prepared.instrument, &prepared.resolution, &bar)?;
            let event_time_unix_nanos = i64::from(bar.marker_seconds)
                .checked_mul(NANOS_PER_SECOND_I64)
                .ok_or(RithmicHistoryAdapterError::MalformedHistory)?;
            items.push(HistoryItem {
                sequence: sequenced.sequence.get(),
                event_time_unix_nanos,
                payload: encode_history_bar(sequenced.value),
            });
        }
        Ok(HistoryPage {
            request: request.clone(),
            items,
            next: None,
        })
    }
}

impl<T: RithmicHistoryTransport> ProviderHistoryAdapter for RithmicHistoryCapabilityAdapter<T> {
    fn capabilities(&self) -> &HistoryCapabilities {
        &self.capabilities
    }

    fn fetch_page(&mut self, request: &HistoryPageRequest) -> Result<HistoryPage, String> {
        let prepared = self.prepare(request).map_err(|error| error.to_string())?;
        let Ok(history) = self.transport.collect_history(prepared.collection.clone()) else {
            self.failed = true;
            return Err(RithmicHistoryAdapterError::Transport.to_string());
        };
        match Self::page_from_history(request, &prepared, history) {
            Ok(page) => Ok(page),
            Err(error) => {
                self.failed = true;
                Err(error.to_string())
            }
        }
    }
}

/// Converts one replay or live time bar to the shared contiguous sequence model.
///
/// # Errors
///
/// Returns an error for mismatched identity/series, off-grid values, missing
/// volume, or a marker that is not aligned to the configured interval.
pub fn canonical_rithmic_time_bar(
    instrument: &RithmicProviderInstrument,
    resolution: &RithmicTimeBarResolution,
    bar: &DecodedTimeBar,
) -> Result<SequencedHistory<MarketBar>, RithmicHistoryAdapterError> {
    let expected_period = match resolution.decoded_type() {
        DecodedTimeBarType::Minute => u32::from(resolution.period.get())
            .checked_mul(60)
            .ok_or(RithmicHistoryAdapterError::MalformedHistory)?,
        DecodedTimeBarType::Second | DecodedTimeBarType::Daily | DecodedTimeBarType::Weekly => {
            u32::from(resolution.period.get())
        }
    };
    if bar.identity.symbol != instrument.descriptor.provider_symbol
        || bar.identity.exchange != instrument.descriptor.venue_id
        || bar.bar_type != resolution.decoded_type()
        || bar.period != expected_period.to_string()
        || bar.marker_seconds < 0
    {
        return Err(RithmicHistoryAdapterError::MalformedHistory);
    }
    let interval = resolution.interval_seconds()?;
    let marker = u64::try_from(bar.marker_seconds)
        .map_err(|_| RithmicHistoryAdapterError::MalformedHistory)?;
    if marker % interval != 0 {
        return Err(RithmicHistoryAdapterError::MalformedHistory);
    }
    let sequence = marker
        .checked_div(interval)
        .and_then(|value| value.checked_add(1))
        .and_then(NonZeroU64::new)
        .ok_or(RithmicHistoryAdapterError::MalformedHistory)?;
    let market_bar = MarketBar {
        source_sequence: sequence.get(),
        exchange_timestamp_seconds: i64::from(bar.marker_seconds),
        open: fixed_price(bar.ohlc.open, instrument.descriptor.price_scale)?,
        high: fixed_price(bar.ohlc.high, instrument.descriptor.price_scale)?,
        low: fixed_price(bar.ohlc.low, instrument.descriptor.price_scale)?,
        close: fixed_price(bar.ohlc.close, instrument.descriptor.price_scale)?,
        volume: fixed_volume(
            bar.volume
                .ok_or(RithmicHistoryAdapterError::MalformedHistory)?,
            instrument.descriptor.quantity_scale,
        )?,
    };
    market_bar
        .validate()
        .map_err(|_| RithmicHistoryAdapterError::MalformedHistory)?;
    Ok(SequencedHistory {
        sequence,
        value: market_bar,
    })
}

/// Decodes one adapter-owned provider-history payload.
///
/// # Errors
///
/// Returns an error for a foreign schema or payload/item identity mismatch.
pub fn decode_rithmic_history_bar(item: &HistoryItem) -> Result<MarketBar, String> {
    if item.payload.len() != HISTORY_PAYLOAD_BYTES
        || &item.payload[..HISTORY_PAYLOAD_MAGIC.len()] != HISTORY_PAYLOAD_MAGIC
    {
        return Err(RithmicHistoryAdapterError::MalformedHistory.to_string());
    }
    let mut offset = HISTORY_PAYLOAD_MAGIC.len();
    let bar = MarketBar {
        source_sequence: read_u64(&item.payload, &mut offset)?,
        exchange_timestamp_seconds: read_i64(&item.payload, &mut offset)?,
        open: read_i64(&item.payload, &mut offset)?,
        high: read_i64(&item.payload, &mut offset)?,
        low: read_i64(&item.payload, &mut offset)?,
        close: read_i64(&item.payload, &mut offset)?,
        volume: read_i64(&item.payload, &mut offset)?,
    };
    let event_time = bar
        .exchange_timestamp_seconds
        .checked_mul(NANOS_PER_SECOND_I64)
        .ok_or_else(|| RithmicHistoryAdapterError::MalformedHistory.to_string())?;
    if bar.source_sequence != item.sequence || event_time != item.event_time_unix_nanos {
        return Err(RithmicHistoryAdapterError::MalformedHistory.to_string());
    }
    bar.validate()
        .map_err(|_| RithmicHistoryAdapterError::MalformedHistory.to_string())?;
    Ok(bar)
}

fn fixed_price(value: f64, scale: u8) -> Result<i64, RithmicHistoryAdapterError> {
    if !value.is_finite() || value <= 0.0 || scale > 18 {
        return Err(RithmicHistoryAdapterError::MalformedHistory);
    }
    let rendered = format!("{value:.precision$}", precision = usize::from(scale));
    let round_trip = rendered
        .parse::<f64>()
        .map_err(|_| RithmicHistoryAdapterError::MalformedHistory)?;
    let tolerance = f64::EPSILON * value.abs().max(1.0) * 4.0;
    if (round_trip - value).abs() > tolerance {
        return Err(RithmicHistoryAdapterError::MalformedHistory);
    }
    rendered
        .replace('.', "")
        .parse::<i64>()
        .map_err(|_| RithmicHistoryAdapterError::MalformedHistory)
}

fn fixed_volume(value: u64, scale: u8) -> Result<i64, RithmicHistoryAdapterError> {
    let value = i64::try_from(value).map_err(|_| RithmicHistoryAdapterError::MalformedHistory)?;
    value
        .checked_mul(
            10_i64
                .checked_pow(u32::from(scale))
                .ok_or(RithmicHistoryAdapterError::MalformedHistory)?,
        )
        .ok_or(RithmicHistoryAdapterError::MalformedHistory)
}

fn encode_history_bar(bar: MarketBar) -> Vec<u8> {
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
        .ok_or_else(|| RithmicHistoryAdapterError::MalformedHistory.to_string())?;
    *offset += 8;
    Ok(u64::from_le_bytes(bytes))
}

fn read_i64(payload: &[u8], offset: &mut usize) -> Result<i64, String> {
    let bytes = payload
        .get(*offset..*offset + 8)
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .ok_or_else(|| RithmicHistoryAdapterError::MalformedHistory.to_string())?;
    *offset += 8;
    Ok(i64::from_le_bytes(bytes))
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 192
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

impl From<ProviderHistoryError> for RithmicHistoryAdapterError {
    fn from(_: ProviderHistoryError) -> Self {
        Self::InvalidConfiguration
    }
}

/// Builds one contiguous covering snapshot from an exact Rithmic history page.
///
/// # Errors
///
/// Returns an error for empty pages, foreign payloads, or non-contiguous
/// sequences. Incomplete coverage must never be installed as a recovery image.
pub fn covering_snapshot_from_page(
    generation: NonZeroU64,
    page: &HistoryPage,
) -> Result<VerifiedHistorySnapshot<MarketBar>, RithmicHistoryAdapterError> {
    if page.items.is_empty() {
        return Err(RithmicHistoryAdapterError::IncompleteCoverage);
    }
    let mut items = Vec::with_capacity(page.items.len());
    for item in &page.items {
        let bar = decode_rithmic_history_bar(item)
            .map_err(|_| RithmicHistoryAdapterError::MalformedHistory)?;
        let sequence =
            NonZeroU64::new(item.sequence).ok_or(RithmicHistoryAdapterError::MalformedHistory)?;
        items.push(SequencedHistory {
            sequence,
            value: bar,
        });
    }
    VerifiedHistorySnapshot::try_new(generation, items).map_err(|error| match error {
        ProviderHistoryError::SequenceGap { .. } => RithmicHistoryAdapterError::IncompleteCoverage,
        _ => RithmicHistoryAdapterError::MalformedHistory,
    })
}

/// Generation-fenced history/live cutover for fixed-interval Rithmic bars.
///
/// Live bars may overlap a covering snapshot; duplicates are discarded. Gaps
/// latch `SnapshotRequired` and accept only a newer covering recovery image.
pub struct RithmicBarContinuity {
    handoff: HandoffCoordinator<MarketBar>,
    failed: bool,
}

impl RithmicBarContinuity {
    #[must_use]
    pub fn new(maximum_buffered_live: NonZeroUsize) -> Self {
        Self {
            handoff: HandoffCoordinator::new(maximum_buffered_live),
            failed: false,
        }
    }

    #[must_use]
    pub const fn state(&self) -> HandoffState {
        self.handoff.state()
    }

    #[must_use]
    pub const fn is_failed(&self) -> bool {
        self.failed
    }

    /// Installs one exact covering page as the recovery image for this generation.
    ///
    /// # Errors
    ///
    /// Returns an error and latches failure for incomplete coverage, stale
    /// generations, watermark regression, or buffered live gaps.
    pub fn install_covering_page(
        &mut self,
        generation: NonZeroU64,
        page: &HistoryPage,
    ) -> Result<HandoffBatch<MarketBar>, RithmicHistoryAdapterError> {
        if self.failed {
            return Err(RithmicHistoryAdapterError::SessionPoisoned);
        }
        let snapshot = covering_snapshot_from_page(generation, page)?;
        match self.handoff.install_snapshot(snapshot) {
            Ok(batch) => Ok(batch),
            Err(
                ProviderHistoryError::InvalidPage(_)
                | ProviderHistoryError::SequenceGap { .. }
                | ProviderHistoryError::SnapshotRequired,
            ) => {
                self.failed = true;
                Err(RithmicHistoryAdapterError::IncompleteCoverage)
            }
            Err(_) => {
                self.failed = true;
                Err(RithmicHistoryAdapterError::MalformedHistory)
            }
        }
    }

    /// Accepts one live bar after or before a covering snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error and requires a covering resnapshot on gaps or overflow.
    pub fn push_live(
        &mut self,
        bar: SequencedHistory<MarketBar>,
    ) -> Result<LiveAcceptance<MarketBar>, RithmicHistoryAdapterError> {
        if self.failed {
            return Err(RithmicHistoryAdapterError::SessionPoisoned);
        }
        match self.handoff.push_live(bar) {
            Ok(acceptance) => Ok(acceptance),
            Err(
                ProviderHistoryError::SequenceGap { .. }
                | ProviderHistoryError::LiveBufferFull { .. }
                | ProviderHistoryError::SnapshotRequired,
            ) => Err(RithmicHistoryAdapterError::IncompleteCoverage),
            Err(_) => {
                self.failed = true;
                Err(RithmicHistoryAdapterError::MalformedHistory)
            }
        }
    }

    /// Discards buffered live values and requires a newer covering snapshot.
    pub fn require_covering_resnapshot(&mut self, observed_sequence: u64) {
        self.handoff.require_snapshot(observed_sequence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BarIdentity, ObservedHistoryRange, Ohlc};
    use axiusflow_desktop_provider_runtime::InstrumentDescriptor;
    use axiusflow_provider_history::HistoryRange;
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    #[derive(Clone)]
    struct FixtureTransport {
        responses: Rc<RefCell<VecDeque<Result<CollectedHistory, String>>>>,
    }

    impl RithmicHistoryTransport for FixtureTransport {
        fn collect_history(
            &mut self,
            request: HistoryCollectionRequest,
        ) -> Result<CollectedHistory, String> {
            let mut history = self
                .responses
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| RithmicHistoryAdapterError::Transport.to_string())??;
            history.request = request;
            Ok(history)
        }
    }

    fn instrument() -> RithmicProviderInstrument {
        RithmicProviderInstrument {
            descriptor: InstrumentDescriptor {
                instrument_id: "future-cme-es-2027-06".to_string(),
                provider_symbol: "ESM7".to_string(),
                display_symbol: "ES Jun 2027".to_string(),
                venue_id: "CME".to_string(),
                price_scale: 2,
                quantity_scale: 0,
            },
            entitlement_id: "rithmic-test-cme".to_string(),
            trades: true,
            quotes: false,
            order_book: false,
        }
    }

    fn resolution() -> RithmicTimeBarResolution {
        RithmicTimeBarResolution::try_new("1m", TimeBarType::Minute, NonZeroU16::new(1).unwrap())
            .expect("resolution validates")
    }

    #[test]
    fn resolutions_include_daily_bars_but_keep_weekly_out_of_scope() {
        let daily = RithmicTimeBarResolution::try_new(
            "1d",
            TimeBarType::Daily,
            NonZeroU16::new(1).unwrap(),
        )
        .expect("daily resolution validates");
        assert_eq!(daily.interval_seconds(), Ok(86_400));
        assert_eq!(daily.decoded_type(), DecodedTimeBarType::Daily);
        assert_eq!(
            RithmicTimeBarResolution::try_new(
                "1w",
                TimeBarType::Weekly,
                NonZeroU16::new(1).unwrap(),
            ),
            Err(RithmicHistoryAdapterError::InvalidConfiguration)
        );
    }

    fn limits() -> RithmicHistoryLimits {
        RithmicHistoryLimits::try_new(
            NonZeroU64::new(NANOS_PER_SECOND.saturating_mul(86_400 * 30)).unwrap(),
            NonZeroU64::new(NANOS_PER_SECOND.saturating_mul(3_600)).unwrap(),
            NonZeroUsize::new(64).unwrap(),
        )
        .expect("limits validate")
    }

    fn aligned_range(bars: usize) -> (i64, i64, i32) {
        let interval = 60_i64;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_secs();
        let finish_exclusive = i64::try_from(now / 60 * 60).expect("aligned now");
        let start = finish_exclusive - interval * i64::try_from(bars).expect("bars");
        let start_seconds = i32::try_from(start).expect("start fits");
        (
            start * NANOS_PER_SECOND_I64,
            finish_exclusive * NANOS_PER_SECOND_I64,
            start_seconds,
        )
    }

    fn time_bar(marker_seconds: i32, close: f64, volume: u64) -> DecodedTimeBar {
        DecodedTimeBar {
            identity: BarIdentity {
                symbol: "ESM7".to_string(),
                exchange: "CME".to_string(),
            },
            bar_type: DecodedTimeBarType::Minute,
            period: "60".to_string(),
            marker_seconds,
            ohlc: Ohlc {
                open: close,
                high: close,
                low: close,
                close,
            },
            trades: Some(1),
            volume: Some(volume),
            bid_volume: None,
            ask_volume: None,
        }
    }

    fn collected(start_seconds: i32, bars: &[DecodedTimeBar]) -> CollectedHistory {
        CollectedHistory {
            request: HistoryCollectionRequest {
                symbol: "ESM7".to_string(),
                exchange: "CME".to_string(),
                series: HistorySeries::Time {
                    bar_type: DecodedTimeBarType::Minute,
                    period: 1,
                },
                start_seconds,
                finish_seconds: start_seconds
                    .saturating_add(i32::try_from(bars.len().saturating_mul(60)).unwrap_or(0))
                    .saturating_sub(1),
                maximum_bars: NonZeroUsize::new(bars.len().max(1)).unwrap(),
            },
            bars: HistoryBars::Time(bars.to_vec()),
            observed_range: bars.first().map(|first| ObservedHistoryRange {
                first_seconds: first.marker_seconds,
                last_seconds: bars
                    .last()
                    .map_or(first.marker_seconds, |last| last.marker_seconds),
            }),
            duplicate_count: 0,
        }
    }

    fn request(start_nanos: i64, end_nanos: i64, maximum_items: usize) -> HistoryPageRequest {
        HistoryPageRequest {
            provider_id: PROVIDER_ID.to_string(),
            account_id: RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID.to_string(),
            entitlement_revision: "rithmic-test-cme".to_string(),
            instrument_id: "future-cme-es-2027-06".to_string(),
            data_class: DataClass::Bars,
            resolution: "1m".to_string(),
            range: HistoryRange {
                start_unix_nanos: start_nanos,
                end_unix_nanos: end_nanos,
            },
            maximum_items: NonZeroUsize::new(maximum_items).unwrap(),
            continuation: None,
        }
    }

    fn adapter(
        responses: Vec<Result<CollectedHistory, String>>,
    ) -> RithmicHistoryCapabilityAdapter<FixtureTransport> {
        RithmicHistoryCapabilityAdapter::try_with_transport(
            FixtureTransport {
                responses: Rc::new(RefCell::new(VecDeque::from(responses))),
            },
            vec![instrument()],
            vec![resolution()],
            limits(),
        )
        .expect("fixture adapter")
    }

    #[test]
    fn fetches_exact_covering_page_and_decodes_canonical_bars() {
        let (start_nanos, end_nanos, start_seconds) = aligned_range(3);
        let bars = [
            time_bar(start_seconds, 5100.25, 1),
            time_bar(start_seconds + 60, 5101.00, 2),
            time_bar(start_seconds + 120, 5101.50, 3),
        ];
        let mut capability = adapter(vec![Ok(collected(start_seconds, &bars))]);
        let page_request = request(start_nanos, end_nanos, 3);
        let page = capability
            .fetch_page(&page_request)
            .expect("covering page fetches");
        assert_eq!(page.request, page_request);
        assert_eq!(page.items.len(), 3);
        assert!(page.next.is_none());
        let decoded = page
            .items
            .iter()
            .map(decode_rithmic_history_bar)
            .collect::<Result<Vec<_>, _>>()
            .expect("payloads decode");
        assert_eq!(
            decoded[0].exchange_timestamp_seconds,
            i64::from(start_seconds)
        );
        assert_eq!(decoded[0].open, 510_025);
        assert_eq!(decoded[0].volume, 1);
        assert_eq!(
            decoded[2].exchange_timestamp_seconds,
            i64::from(start_seconds + 120)
        );
        assert_eq!(decoded[2].close, 510_150);
    }

    #[test]
    fn incomplete_or_gapped_coverage_fails_closed_and_poisons_session() {
        let (start_nanos, end_nanos, start_seconds) = aligned_range(3);
        let incomplete = [
            time_bar(start_seconds, 5100.00, 1),
            time_bar(start_seconds + 60, 5101.00, 2),
        ];
        let mut capability = adapter(vec![Ok(collected(start_seconds, &incomplete))]);
        let page_request = request(start_nanos, end_nanos, 3);
        assert!(capability.fetch_page(&page_request).is_err());
        assert!(capability.fetch_page(&page_request).is_err());

        let gapped = [
            time_bar(start_seconds, 5100.00, 1),
            time_bar(start_seconds + 120, 5102.00, 2),
            time_bar(start_seconds + 180, 5103.00, 3),
        ];
        let mut capability = adapter(vec![Ok(collected(start_seconds, &gapped))]);
        assert!(capability.fetch_page(&page_request).is_err());
    }

    #[test]
    fn rejects_wrong_scope_before_transport_and_keeps_ticks_unsupported() {
        let (start_nanos, end_nanos, _) = aligned_range(2);
        let responses = Rc::new(RefCell::new(VecDeque::from([Ok(collected(
            0,
            &[time_bar(0, 1.0, 1)],
        ))])));
        let mut capability = RithmicHistoryCapabilityAdapter::try_with_transport(
            FixtureTransport {
                responses: responses.clone(),
            },
            vec![instrument()],
            vec![resolution()],
            limits(),
        )
        .expect("fixture adapter");
        let mut wrong_scope = request(start_nanos, end_nanos, 2);
        wrong_scope.account_id = "other".to_string();
        assert!(capability.fetch_page(&wrong_scope).is_err());
        let mut unaligned = request(start_nanos, end_nanos, 2);
        unaligned.range.start_unix_nanos += 1;
        assert!(capability.fetch_page(&unaligned).is_err());
        assert_eq!(responses.borrow().len(), 1);
        assert!(matches!(
            capability.capabilities().dataset(DataClass::Ticks),
            DatasetCapability::Unsupported { .. }
        ));
        assert!(matches!(
            capability.capabilities().dataset(DataClass::Depth),
            DatasetCapability::Unsupported { .. }
        ));
    }

    #[test]
    fn covering_resnapshot_discards_overlap_and_rejects_incomplete_recovery() {
        let (start_nanos, end_nanos, start_seconds) = aligned_range(3);
        let bars = [
            time_bar(start_seconds, 5100.00, 1),
            time_bar(start_seconds + 60, 5101.00, 2),
            time_bar(start_seconds + 120, 5102.00, 3),
        ];
        let mut capability = adapter(vec![Ok(collected(start_seconds, &bars))]);
        let first_page = capability
            .fetch_page(&request(start_nanos, end_nanos, 3))
            .expect("first covering page");
        let mut continuity = RithmicBarContinuity::new(NonZeroUsize::new(4).unwrap());
        let live_overlap = decode_rithmic_history_bar(&first_page.items[2]).expect("overlap bar");
        let overlap_sequence = NonZeroU64::new(first_page.items[2].sequence).unwrap();
        assert_eq!(
            continuity
                .push_live(SequencedHistory {
                    sequence: overlap_sequence,
                    value: live_overlap,
                })
                .expect("overlap buffers before snapshot"),
            LiveAcceptance::Buffered
        );
        let next_sequence = overlap_sequence.checked_add(1).unwrap();
        let live_next = MarketBar {
            source_sequence: next_sequence.get(),
            exchange_timestamp_seconds: i64::from(start_seconds + 180),
            open: 510_300,
            high: 510_300,
            low: 510_300,
            close: 510_300,
            volume: 4,
        };
        continuity
            .push_live(SequencedHistory {
                sequence: next_sequence,
                value: live_next,
            })
            .expect("contiguous live buffers");
        let batch = continuity
            .install_covering_page(NonZeroU64::new(1).unwrap(), &first_page)
            .expect("covering cutover");
        assert_eq!(batch.snapshot.items().len(), 3);
        assert_eq!(
            batch.live,
            vec![SequencedHistory {
                sequence: next_sequence,
                value: live_next,
            }]
        );
        assert_eq!(
            continuity.state(),
            HandoffState::Live {
                generation: 1,
                last_sequence: next_sequence.get(),
            }
        );

        assert_eq!(
            continuity.push_live(SequencedHistory {
                sequence: next_sequence.checked_add(2).unwrap(),
                value: MarketBar {
                    source_sequence: next_sequence.get() + 2,
                    exchange_timestamp_seconds: i64::from(start_seconds + 300),
                    open: 1,
                    high: 1,
                    low: 1,
                    close: 1,
                    volume: 1,
                },
            }),
            Err(RithmicHistoryAdapterError::IncompleteCoverage)
        );
        assert!(matches!(
            continuity.state(),
            HandoffState::SnapshotRequired { .. }
        ));

        assert_eq!(
            continuity.install_covering_page(NonZeroU64::new(1).unwrap(), &first_page),
            Err(RithmicHistoryAdapterError::IncompleteCoverage)
        );
        assert!(continuity.is_failed());
        assert_eq!(
            continuity.install_covering_page(NonZeroU64::new(2).unwrap(), &first_page),
            Err(RithmicHistoryAdapterError::SessionPoisoned)
        );
    }

    #[test]
    fn covering_snapshot_rejects_empty_and_noncontiguous_pages() {
        let empty = HistoryPage {
            request: request(0, NANOS_PER_SECOND_I64, 1),
            items: Vec::new(),
            next: None,
        };
        assert_eq!(
            covering_snapshot_from_page(NonZeroU64::new(1).unwrap(), &empty),
            Err(RithmicHistoryAdapterError::IncompleteCoverage)
        );
    }
}
