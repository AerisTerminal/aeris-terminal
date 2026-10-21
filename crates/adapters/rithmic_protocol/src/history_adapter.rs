use crate::{
    CollectedHistory, CollectionProgress, CollectorError, DecodedControlMessage, DecodedTimeBar,
    DecodedTimeBarType, HistoryBars, HistoryCollectionRequest, HistoryCollector, HistorySeries,
    RithmicHistoryConnection, RithmicProviderInstrument, RithmicSessionError,
    RithmicSessionMessage, TimeBarReplayRequest, TimeBarType, collectors::rithmic_wire_period,
};
use aeris_instruments::calendar::{
    ExchangeCalendar, ExchangeCalendarBucket, ExchangeCalendarPeriod,
};
use aeris_market_data::{
    ChartInterval, MarketBar, RithmicChartAggregation, RithmicDailyAggregation, RithmicTimeUnit,
};
use aeris_provider_history::{
    DataClass, DatasetCapability, HandoffBatch, HandoffCoordinator, HandoffState,
    HistoryCapabilities, HistoryItem, HistoryPage, HistoryPageRequest, HistoryRange,
    LiveAcceptance, PaginationStyle, ProviderHistoryAdapter, ProviderHistoryError, RateLimit,
    SequencedHistory, VerifiedHistorySnapshot,
};
use chrono::{DateTime, Datelike, NaiveDate};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    num::{NonZeroU16, NonZeroU32, NonZeroU64, NonZeroUsize},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const PROVIDER_ID: &str = "rithmic";
const MAXIMUM_REPLAY_BARS: usize = 10_000;
/// Bars a padded time replay may return outside the requested window.
const TIME_REPLAY_PADDING_BARS: usize = 3;
const MAXIMUM_CONTROL_MESSAGES: usize = 256;
const MAXIMUM_REPLAY_TIMEOUT: Duration = Duration::from_mins(5);
const NANOS_PER_SECOND: u64 = 1_000_000_000;
const NANOS_PER_SECOND_I64: i64 = 1_000_000_000;
const SECONDS_PER_DAY: i64 = 86_400;
const HISTORY_PAYLOAD_MAGIC: &[u8; 6] = b"AXRHB2";
const LEGACY_HISTORY_PAYLOAD_MAGIC: &[u8; 6] = b"AXRHB1";
const HISTORY_PAYLOAD_BYTES: usize = HISTORY_PAYLOAD_MAGIC.len() + 8 * 8;
const LEGACY_HISTORY_PAYLOAD_BYTES: usize = LEGACY_HISTORY_PAYLOAD_MAGIC.len() + 7 * 8;

/// Non-secret account scope for Rithmic Test market-data history.
pub const RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID: &str = "rithmic_test_market_data";

/// Exact provider replay aggregation for a chart interval.
#[must_use]
pub fn rithmic_chart_aggregation(interval: ChartInterval) -> RithmicChartAggregation {
    use ChartInterval as I;
    match interval {
        I::Tick100 => RithmicChartAggregation::Trades {
            trades_per_bar: NonZeroU16::new(100).unwrap_or(NonZeroU16::MIN),
        },
        I::Second1 => rithmic_time(RithmicTimeUnit::Second, 1),
        I::Second5 => rithmic_time(RithmicTimeUnit::Second, 5),
        I::Second10 => rithmic_time(RithmicTimeUnit::Second, 10),
        I::Second15 => rithmic_time(RithmicTimeUnit::Second, 15),
        I::Second30 => rithmic_time(RithmicTimeUnit::Second, 30),
        I::Minute1 => rithmic_time(RithmicTimeUnit::Minute, 1),
        I::Minute3 => rithmic_time(RithmicTimeUnit::Minute, 3),
        I::Minute5 => rithmic_time(RithmicTimeUnit::Minute, 5),
        I::Minute15 => rithmic_time(RithmicTimeUnit::Minute, 15),
        I::Minute30 => rithmic_time(RithmicTimeUnit::Minute, 30),
        I::Hour1 => rithmic_time(RithmicTimeUnit::Minute, 60),
        I::Hour2 => rithmic_time(RithmicTimeUnit::Minute, 120),
        I::Hour4 => rithmic_time(RithmicTimeUnit::Minute, 240),
        I::Hour8 => rithmic_time(RithmicTimeUnit::Minute, 480),
        I::Hour12 => rithmic_time(RithmicTimeUnit::Minute, 720),
        I::Day1 => rithmic_sessions(1),
        I::Day3 => rithmic_sessions(3),
        I::Week1 => RithmicChartAggregation::DailySessions {
            period: RithmicDailyAggregation::Week,
        },
        I::Month1 => RithmicChartAggregation::DailySessions {
            period: RithmicDailyAggregation::Month,
        },
    }
}

fn rithmic_time(unit: RithmicTimeUnit, period: u16) -> RithmicChartAggregation {
    RithmicChartAggregation::Time {
        unit,
        period: NonZeroU16::new(period).unwrap_or(NonZeroU16::MIN),
    }
}

fn rithmic_sessions(days: u16) -> RithmicChartAggregation {
    RithmicChartAggregation::DailySessions {
        period: RithmicDailyAggregation::Days(NonZeroU16::new(days).unwrap_or(NonZeroU16::MIN)),
    }
}

#[cfg(test)]
mod chart_interval_tests {
    use super::*;

    #[test]
    fn every_native_interval_has_exact_rithmic_aggregation() {
        let expected = [
            (ChartInterval::Second1, RithmicTimeUnit::Second, 1),
            (ChartInterval::Second5, RithmicTimeUnit::Second, 5),
            (ChartInterval::Second10, RithmicTimeUnit::Second, 10),
            (ChartInterval::Second15, RithmicTimeUnit::Second, 15),
            (ChartInterval::Second30, RithmicTimeUnit::Second, 30),
            (ChartInterval::Minute1, RithmicTimeUnit::Minute, 1),
            (ChartInterval::Minute3, RithmicTimeUnit::Minute, 3),
            (ChartInterval::Minute5, RithmicTimeUnit::Minute, 5),
            (ChartInterval::Minute15, RithmicTimeUnit::Minute, 15),
            (ChartInterval::Minute30, RithmicTimeUnit::Minute, 30),
            (ChartInterval::Hour1, RithmicTimeUnit::Minute, 60),
            (ChartInterval::Hour2, RithmicTimeUnit::Minute, 120),
            (ChartInterval::Hour4, RithmicTimeUnit::Minute, 240),
            (ChartInterval::Hour8, RithmicTimeUnit::Minute, 480),
            (ChartInterval::Hour12, RithmicTimeUnit::Minute, 720),
        ];
        for (interval, unit, period) in expected {
            assert_eq!(
                rithmic_chart_aggregation(interval),
                RithmicChartAggregation::Time {
                    unit,
                    period: NonZeroU16::new(period).unwrap()
                }
            );
        }
        assert_eq!(
            rithmic_chart_aggregation(ChartInterval::Tick100),
            RithmicChartAggregation::Trades {
                trades_per_bar: NonZeroU16::new(100).unwrap()
            }
        );
        for (interval, period) in [
            (
                ChartInterval::Day1,
                RithmicDailyAggregation::Days(NonZeroU16::MIN),
            ),
            (
                ChartInterval::Day3,
                RithmicDailyAggregation::Days(NonZeroU16::new(3).unwrap()),
            ),
            (ChartInterval::Week1, RithmicDailyAggregation::Week),
            (ChartInterval::Month1, RithmicDailyAggregation::Month),
        ] {
            assert_eq!(
                rithmic_chart_aggregation(interval),
                RithmicChartAggregation::DailySessions { period }
            );
        }
    }
}

/// Rithmic history adapter failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicHistoryAdapterError {
    InvalidConfiguration,
    InvalidRequest,
    /// The history session could not send the replay, read its reply or log out.
    Session(RithmicSessionError),
    /// Rithmic refused the replay request.
    Rejected,
    /// Rithmic ended the history session during the replay.
    ForcedLogout,
    /// A replay frame contradicted the request (identity, series, range, order or bound).
    Collection(CollectorError),
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
    /// Creates a second- or minute-based resolution with a stable public name.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe identity, or for daily and weekly bars,
    /// which are trading dates rather than fixed intervals.
    pub fn try_new(
        resolution: impl Into<String>,
        bar_type: TimeBarType,
        period: NonZeroU16,
    ) -> Result<Self, RithmicHistoryAdapterError> {
        let resolution = resolution.into();
        if !valid_identity(&resolution)
            || !matches!(bar_type, TimeBarType::Second | TimeBarType::Minute)
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

    /// Returns the exact fixed interval represented by this resolution.
    ///
    /// # Errors
    ///
    /// Returns an error when the period cannot be represented in seconds.
    pub fn interval_seconds(&self) -> Result<u64, RithmicHistoryAdapterError> {
        let unit = match self.bar_type {
            TimeBarType::Second => 1,
            TimeBarType::Minute => 60,
            TimeBarType::Daily | TimeBarType::Weekly => {
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
    /// Returns an error for a page that, with its replay padding, exceeds the
    /// protocol maximum, or a range that cannot be represented by Rithmic's
    /// signed-second indexes.
    pub fn try_new(
        maximum_lookback_nanos: NonZeroU64,
        maximum_request_span_nanos: NonZeroU64,
        maximum_bars: NonZeroUsize,
    ) -> Result<Self, RithmicHistoryAdapterError> {
        let maximum_index_span = u64::try_from(i32::MAX)
            .unwrap_or(u64::MAX)
            .saturating_mul(NANOS_PER_SECOND);
        if maximum_bars.get() > MAXIMUM_REPLAY_BARS - TIME_REPLAY_PADDING_BARS
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
    /// Returns why the session or collection could not continue.
    fn collect_history(
        &mut self,
        request: HistoryCollectionRequest,
    ) -> Result<CollectedHistory, RithmicHistoryAdapterError>;
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

    /// Logs out and closes the authenticated history session within its native deadline.
    ///
    /// # Errors
    /// Returns the redacted session failure when logout or socket close is not confirmed.
    pub fn close(self) -> Result<(), RithmicHistoryAdapterError> {
        self.connection
            .close()
            .map_err(RithmicHistoryAdapterError::Session)
    }
}

impl RithmicHistoryTransport for RithmicHistorySessionTransport {
    fn collect_history(
        &mut self,
        request: HistoryCollectionRequest,
    ) -> Result<CollectedHistory, RithmicHistoryAdapterError> {
        let maximum_bars = u16::try_from(request.maximum_bars.get())
            .map_err(|_| RithmicHistoryAdapterError::InvalidRequest)?;
        match request.series {
            HistorySeries::Time { bar_type, period } => {
                let wire_type = match bar_type {
                    DecodedTimeBarType::Second => TimeBarType::Second,
                    DecodedTimeBarType::Minute => TimeBarType::Minute,
                    DecodedTimeBarType::Daily => TimeBarType::Daily,
                    DecodedTimeBarType::Weekly => TimeBarType::Weekly,
                };
                self.connection.replay_time_bars(TimeBarReplayRequest {
                    symbol: &request.symbol,
                    exchange: &request.exchange,
                    bar_type: wire_type,
                    period,
                    start_seconds: request.start_seconds,
                    finish_seconds: request.finish_seconds,
                    maximum_bars,
                })
            }
            HistorySeries::Tick { trades_per_bar } => {
                self.connection
                    .replay_tick_bars(crate::TickBarReplayRequest {
                        symbol: &request.symbol,
                        exchange: &request.exchange,
                        trades_per_bar,
                        start_seconds: request.start_seconds,
                        finish_seconds: request.finish_seconds,
                        maximum_bars,
                    })
            }
        }
        .map_err(RithmicHistoryAdapterError::Session)?;
        let mut collector = HistoryCollector::try_new(request)
            .map_err(|_| RithmicHistoryAdapterError::InvalidRequest)?;
        let deadline = Instant::now() + self.replay_timeout;
        let mut control_messages = 0_usize;
        loop {
            match self
                .connection
                .read_next_until(deadline)
                .map_err(RithmicHistoryAdapterError::Session)?
            {
                RithmicSessionMessage::History(message) => match collector.accept(message) {
                    Ok(CollectionProgress::Pending) => {}
                    Ok(CollectionProgress::Complete(history)) => return Ok(history),
                    Ok(CollectionProgress::Unhandled(_)) => {
                        return Err(RithmicHistoryAdapterError::MalformedHistory);
                    }
                    Err(CollectorError::Rejected) => {
                        return Err(RithmicHistoryAdapterError::Rejected);
                    }
                    Err(error) => return Err(RithmicHistoryAdapterError::Collection(error)),
                },
                RithmicSessionMessage::Control(DecodedControlMessage::ForcedLogout) => {
                    return Err(RithmicHistoryAdapterError::ForcedLogout);
                }
                RithmicSessionMessage::Control(DecodedControlMessage::Reject) => {
                    return Err(RithmicHistoryAdapterError::Rejected);
                }
                RithmicSessionMessage::Control(_)
                | RithmicSessionMessage::Catalog(_)
                | RithmicSessionMessage::Market(_) => {
                    control_messages = control_messages.saturating_add(1);
                    if control_messages > self.maximum_control_messages.get() {
                        return Err(RithmicHistoryAdapterError::Session(
                            RithmicSessionError::UnexpectedMessage,
                        ));
                    }
                }
            }
        }
    }
}

struct PreparedRequest {
    instrument: RithmicProviderInstrument,
    resolution: RithmicTimeBarResolution,
    start_seconds: i32,
    end_seconds: i32,
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
        let end_seconds = i32::try_from(end_nanos / NANOS_PER_SECOND)
            .map_err(|_| RithmicHistoryAdapterError::InvalidRequest)?;
        Ok(PreparedRequest {
            instrument,
            resolution,
            start_seconds,
            end_seconds,
            expected_bars,
        })
    }

    fn page_from_bars(
        request: &HistoryPageRequest,
        prepared: &PreparedRequest,
        bars: Vec<SequencedHistory<MarketBar>>,
    ) -> Result<HistoryPage, RithmicHistoryAdapterError> {
        if bars.len() != prepared.expected_bars {
            return Err(RithmicHistoryAdapterError::IncompleteCoverage);
        }
        let interval = i64::try_from(prepared.resolution.interval_seconds()?)
            .map_err(|_| RithmicHistoryAdapterError::MalformedHistory)?;
        let mut items = Vec::with_capacity(bars.len());
        for (index, bar) in bars.into_iter().enumerate() {
            let expected_open = i64::try_from(index)
                .ok()
                .and_then(|value| value.checked_mul(interval))
                .and_then(|offset| offset.checked_add(i64::from(prepared.start_seconds)))
                .ok_or(RithmicHistoryAdapterError::MalformedHistory)?;
            if bar.value.exchange_timestamp_seconds != expected_open {
                return Err(RithmicHistoryAdapterError::IncompleteCoverage);
            }
            items.push(HistoryItem {
                sequence: bar.sequence.get(),
                event_time_unix_nanos: bar.value.exchange_timestamp_unix_nanos,
                payload: encode_history_bar(bar.value),
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
        let replay_bars = NonZeroUsize::new(
            prepared
                .expected_bars
                .saturating_add(TIME_REPLAY_PADDING_BARS),
        )
        .unwrap_or(NonZeroUsize::MIN);
        collect_time_bars(
            &mut self.transport,
            &prepared.instrument,
            &prepared.resolution,
            prepared.start_seconds,
            prepared.end_seconds,
            replay_bars,
        )
        .and_then(|bars| Self::page_from_bars(request, &prepared, bars))
        .map_err(|error| {
            self.failed = true;
            error.to_string()
        })
    }
}

/// Converts one replay or live time bar to the shared contiguous sequence model,
/// labelled and sequenced by the time its period opens.
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
    let interval = resolution.interval_seconds()?;
    if bar.identity.symbol != instrument.descriptor.provider_symbol
        || bar.identity.exchange != instrument.descriptor.venue_id
        || bar.bar_type != resolution.decoded_type()
        || rithmic_wire_period(bar.bar_type, i32::from(resolution.period.get())).as_deref()
            != Some(bar.period.as_str())
        || bar.marker_seconds < 0
    {
        return Err(RithmicHistoryAdapterError::MalformedHistory);
    }
    let marker = u64::try_from(bar.marker_seconds)
        .map_err(|_| RithmicHistoryAdapterError::MalformedHistory)?;
    if marker % interval != 0 {
        return Err(RithmicHistoryAdapterError::MalformedHistory);
    }
    // Rithmic stamps a second or minute bar with the time its period closes.
    let open = marker
        .checked_sub(interval)
        .ok_or(RithmicHistoryAdapterError::MalformedHistory)?;
    let sequence = open
        .checked_div(interval)
        .and_then(|value| value.checked_add(1))
        .and_then(NonZeroU64::new)
        .ok_or(RithmicHistoryAdapterError::MalformedHistory)?;
    let open_seconds =
        i64::try_from(open).map_err(|_| RithmicHistoryAdapterError::MalformedHistory)?;
    let market_bar = MarketBar {
        source_sequence: sequence.get(),
        exchange_timestamp_seconds: open_seconds,
        exchange_timestamp_unix_nanos: open_seconds
            .checked_mul(NANOS_PER_SECOND_I64)
            .ok_or(RithmicHistoryAdapterError::MalformedHistory)?,
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

/// Collects one bounded run of individual trades as one-trade bars.
///
/// A tick chart's current candle is a count of trades, not a clock, so the only
/// way to know how far into it the market already is — and what it looks like —
/// is to replay the trades since the newest complete bundle. There is no
/// `ChartInterval` for this because it is never a chart: it exists solely to
/// close the history/live seam on a tick chart.
///
/// # Errors
///
/// Returns an error for an invalid range, provider response, or canonical conversion.
pub fn collect_rithmic_trade_history<T: RithmicHistoryTransport>(
    transport: &mut T,
    instrument: &RithmicProviderInstrument,
    range: HistoryRange,
    maximum_trades: NonZeroUsize,
) -> Result<Vec<MarketBar>, RithmicHistoryAdapterError> {
    if range.start_unix_nanos % NANOS_PER_SECOND_I64 != 0
        || range.end_unix_nanos % NANOS_PER_SECOND_I64 != 0
        || range.start_unix_nanos >= range.end_unix_nanos
        || maximum_trades.get() > MAXIMUM_REPLAY_BARS
    {
        return Err(RithmicHistoryAdapterError::InvalidRequest);
    }
    let start_seconds = i32::try_from(range.start_unix_nanos / NANOS_PER_SECOND_I64)
        .map_err(|_| RithmicHistoryAdapterError::InvalidRequest)?;
    let finish_seconds = i32::try_from(range.end_unix_nanos / NANOS_PER_SECOND_I64)
        .map_err(|_| RithmicHistoryAdapterError::InvalidRequest)?;
    collect_tick_chart_history(
        transport,
        instrument,
        1,
        start_seconds,
        finish_seconds,
        maximum_trades,
    )
}

/// Collects one bounded canonical chart-history response for any supported Rithmic interval.
///
/// # Errors
///
/// Returns an error for an invalid range, provider response, or canonical conversion.
pub fn collect_rithmic_chart_history<T: RithmicHistoryTransport>(
    transport: &mut T,
    instrument: &RithmicProviderInstrument,
    interval: ChartInterval,
    range: HistoryRange,
    maximum_bars: NonZeroUsize,
) -> Result<Vec<MarketBar>, RithmicHistoryAdapterError> {
    let (start_seconds, end_seconds) = chart_range_seconds(range, maximum_bars)?;
    match rithmic_chart_aggregation(interval) {
        RithmicChartAggregation::Trades { trades_per_bar } => collect_tick_chart_history(
            transport,
            instrument,
            trades_per_bar.get(),
            start_seconds,
            end_seconds,
            maximum_bars,
        ),
        RithmicChartAggregation::Time { unit, period } => collect_time_chart_history(
            transport,
            instrument,
            &RithmicTimeBarResolution::try_new(
                interval.label(),
                match unit {
                    RithmicTimeUnit::Second => TimeBarType::Second,
                    RithmicTimeUnit::Minute => TimeBarType::Minute,
                },
                period,
            )?,
            start_seconds,
            end_seconds,
            maximum_bars,
        ),
        RithmicChartAggregation::DailySessions { period } => {
            let calendar = session_calendar(instrument)?;
            let days = collect_closed_trading_dates(
                transport,
                instrument,
                calendar,
                start_seconds,
                end_seconds,
            )?;
            let bars = aggregate_daily_chart_history(days, calendar_period(period), calendar)?;
            bounded_chart_bars(bars.len(), maximum_bars)?;
            Ok(bars)
        }
    }
}

/// Day, multi-day, week or month history whose newest calendar bucket may still be trading.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RithmicSessionHistory {
    /// Buckets that end before the trading date open at the end of the range.
    pub closed: Vec<MarketBar>,
    /// The bucket holding that trading date, with its trades up to the end of the range.
    pub open: Option<MarketBar>,
}

/// Collects session-interval history up to the end of `range`, normally now.
///
/// Rithmic replays only trading dates that have closed, so the date still
/// trading at the end of the range is folded from its minute bars, and the
/// bucket holding it is returned apart from the closed ones.
///
/// # Errors
///
/// Returns an error for a non-session interval, an invalid range or provider
/// response, or more than `maximum_bars` buckets.
pub fn collect_rithmic_session_history<T: RithmicHistoryTransport>(
    transport: &mut T,
    instrument: &RithmicProviderInstrument,
    interval: ChartInterval,
    range: HistoryRange,
    maximum_bars: NonZeroUsize,
) -> Result<RithmicSessionHistory, RithmicHistoryAdapterError> {
    let RithmicChartAggregation::DailySessions { period } = rithmic_chart_aggregation(interval)
    else {
        return Err(RithmicHistoryAdapterError::InvalidRequest);
    };
    let (start_seconds, end_seconds) = chart_range_seconds(range, maximum_bars)?;
    let calendar = session_calendar(instrument)?;
    let mut days =
        collect_closed_trading_dates(transport, instrument, calendar, start_seconds, end_seconds)?;
    let open_day = calendar.trading_day(i64::from(end_seconds) - 1);
    let open_day_start = calendar.trading_day_open(open_day);
    let open_day_closed = days
        .iter()
        .any(|day| day.exchange_timestamp_seconds == open_day_start);
    if !open_day_closed && open_day_start >= i64::from(start_seconds) {
        days.extend(collect_open_trading_date(
            transport,
            instrument,
            open_day,
            open_day_start,
            end_seconds,
        )?);
    }
    let period = calendar_period(period);
    let mut closed = aggregate_daily_chart_history(days, period, calendar)?;
    let open_bucket = calendar.bucket(open_day_start, period);
    let open =
        closed.pop_if(|bar| calendar.bucket(bar.exchange_timestamp_seconds, period) == open_bucket);
    bounded_chart_bars(closed.len() + usize::from(open.is_some()), maximum_bars)?;
    Ok(RithmicSessionHistory { closed, open })
}

fn chart_range_seconds(
    range: HistoryRange,
    maximum_bars: NonZeroUsize,
) -> Result<(i32, i32), RithmicHistoryAdapterError> {
    if range.start_unix_nanos % NANOS_PER_SECOND_I64 != 0
        || range.end_unix_nanos % NANOS_PER_SECOND_I64 != 0
        || range.start_unix_nanos >= range.end_unix_nanos
        || maximum_bars.get() > MAXIMUM_REPLAY_BARS
    {
        return Err(RithmicHistoryAdapterError::InvalidRequest);
    }
    let start_seconds = i32::try_from(range.start_unix_nanos / NANOS_PER_SECOND_I64)
        .map_err(|_| RithmicHistoryAdapterError::InvalidRequest)?;
    let end_seconds = i32::try_from(range.end_unix_nanos / NANOS_PER_SECOND_I64)
        .map_err(|_| RithmicHistoryAdapterError::InvalidRequest)?;
    Ok((start_seconds, end_seconds))
}

fn bounded_chart_bars(
    bars: usize,
    maximum_bars: NonZeroUsize,
) -> Result<(), RithmicHistoryAdapterError> {
    if bars > maximum_bars.get() {
        return Err(RithmicHistoryAdapterError::Collection(
            CollectorError::ResultLimitExceeded {
                maximum: maximum_bars.get(),
            },
        ));
    }
    Ok(())
}

fn collect_time_chart_history<T: RithmicHistoryTransport>(
    transport: &mut T,
    instrument: &RithmicProviderInstrument,
    resolution: &RithmicTimeBarResolution,
    start_seconds: i32,
    end_seconds: i32,
    maximum_bars: NonZeroUsize,
) -> Result<Vec<MarketBar>, RithmicHistoryAdapterError> {
    let bars = collect_time_bars(
        transport,
        instrument,
        resolution,
        start_seconds,
        end_seconds,
        maximum_bars,
    )?;
    Ok(bars
        .into_iter()
        .enumerate()
        .map(|(index, sequenced)| {
            let mut value = sequenced.value;
            value.source_sequence = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
            value
        })
        .collect())
}

/// Replays the canonical time bars that open in `[start_seconds, end_seconds)`.
///
/// Rithmic answers a minute replay with every bar whose period overlaps the
/// requested seconds, but a second replay only with the bars whose close
/// marker falls inside them. The replay is padded by one period on each side
/// and trimmed by open time, so both return exactly the bars of the window.
fn collect_time_bars<T: RithmicHistoryTransport>(
    transport: &mut T,
    instrument: &RithmicProviderInstrument,
    resolution: &RithmicTimeBarResolution,
    start_seconds: i32,
    end_seconds: i32,
    maximum_bars: NonZeroUsize,
) -> Result<Vec<SequencedHistory<MarketBar>>, RithmicHistoryAdapterError> {
    let interval_seconds = i32::try_from(resolution.interval_seconds()?)
        .map_err(|_| RithmicHistoryAdapterError::InvalidRequest)?;
    let request = HistoryCollectionRequest {
        symbol: instrument.descriptor.provider_symbol.clone(),
        exchange: instrument.descriptor.venue_id.clone(),
        series: HistorySeries::Time {
            bar_type: resolution.decoded_type(),
            period: i32::from(resolution.period.get()),
        },
        start_seconds: start_seconds
            .checked_sub(interval_seconds)
            .ok_or(RithmicHistoryAdapterError::InvalidRequest)?,
        finish_seconds: end_seconds
            .checked_add(interval_seconds)
            .ok_or(RithmicHistoryAdapterError::InvalidRequest)?,
        maximum_bars,
    };
    let collected = transport.collect_history(request.clone())?;
    if collected.request != request {
        return Err(RithmicHistoryAdapterError::MalformedHistory);
    }
    let HistoryBars::Time(decoded) = collected.bars else {
        return Err(RithmicHistoryAdapterError::MalformedHistory);
    };
    let window = i64::from(start_seconds)..i64::from(end_seconds);
    let mut bars = Vec::with_capacity(decoded.len());
    for bar in &decoded {
        let sequenced = canonical_rithmic_time_bar(instrument, resolution, bar)?;
        if window.contains(&sequenced.value.exchange_timestamp_seconds) {
            bars.push(sequenced);
        }
    }
    Ok(bars)
}

fn collect_tick_chart_history<T: RithmicHistoryTransport>(
    transport: &mut T,
    instrument: &RithmicProviderInstrument,
    trades_per_bar: u16,
    start_seconds: i32,
    finish_seconds: i32,
    maximum_bars: NonZeroUsize,
) -> Result<Vec<MarketBar>, RithmicHistoryAdapterError> {
    let collected = transport.collect_history(HistoryCollectionRequest {
        symbol: instrument.descriptor.provider_symbol.clone(),
        exchange: instrument.descriptor.venue_id.clone(),
        series: HistorySeries::Tick { trades_per_bar },
        start_seconds,
        finish_seconds,
        maximum_bars,
    })?;
    let HistoryBars::Tick(decoded) = collected.bars else {
        return Err(RithmicHistoryAdapterError::MalformedHistory);
    };
    decoded
        .iter()
        .enumerate()
        .map(|(index, bar)| {
            let sequence =
                NonZeroU64::new(u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1))
                    .unwrap_or(NonZeroU64::MIN);
            canonical_rithmic_tick_bar(instrument, trades_per_bar, sequence, bar)
        })
        .collect()
}

fn session_calendar(
    instrument: &RithmicProviderInstrument,
) -> Result<ExchangeCalendar, RithmicHistoryAdapterError> {
    ExchangeCalendar::for_code(&instrument.descriptor.venue_id)
        .ok_or(RithmicHistoryAdapterError::InvalidRequest)
}

const fn calendar_period(period: RithmicDailyAggregation) -> ExchangeCalendarPeriod {
    match period {
        RithmicDailyAggregation::Days(days) => ExchangeCalendarPeriod::Days(days),
        RithmicDailyAggregation::Week => ExchangeCalendarPeriod::Week,
        RithmicDailyAggregation::Month => ExchangeCalendarPeriod::Month,
    }
}

/// Replays the closed trading dates whose session opens in
/// `[start_seconds, end_seconds)`, each labelled by that opening instant as a
/// live session bar is.
///
/// A daily replay is indexed by YYYYMMDD trading date, not by time. A range
/// with more dates than one replay holds keeps its newest dates.
fn collect_closed_trading_dates<T: RithmicHistoryTransport>(
    transport: &mut T,
    instrument: &RithmicProviderInstrument,
    calendar: ExchangeCalendar,
    start_seconds: i32,
    end_seconds: i32,
) -> Result<Vec<MarketBar>, RithmicHistoryAdapterError> {
    let last_day = calendar.trading_day(i64::from(end_seconds) - 1);
    let replay_days = i64::try_from(MAXIMUM_REPLAY_BARS)
        .map_err(|_| RithmicHistoryAdapterError::InvalidRequest)?;
    let first_day = calendar
        .trading_day(i64::from(start_seconds))
        .max(last_day - replay_days + 1);
    let dates = usize::try_from(last_day - first_day + 1)
        .ok()
        .and_then(NonZeroUsize::new)
        .ok_or(RithmicHistoryAdapterError::InvalidRequest)?;
    let request = HistoryCollectionRequest {
        symbol: instrument.descriptor.provider_symbol.clone(),
        exchange: instrument.descriptor.venue_id.clone(),
        series: HistorySeries::Time {
            bar_type: DecodedTimeBarType::Daily,
            period: 1,
        },
        start_seconds: replay_trading_date(first_day)
            .ok_or(RithmicHistoryAdapterError::InvalidRequest)?,
        finish_seconds: replay_trading_date(last_day)
            .ok_or(RithmicHistoryAdapterError::InvalidRequest)?,
        maximum_bars: dates,
    };
    let collected = transport.collect_history(request.clone())?;
    if collected.request != request {
        return Err(RithmicHistoryAdapterError::MalformedHistory);
    }
    let HistoryBars::Time(decoded) = collected.bars else {
        return Err(RithmicHistoryAdapterError::MalformedHistory);
    };
    let window = i64::from(start_seconds)..i64::from(end_seconds);
    let mut days = Vec::with_capacity(decoded.len());
    for bar in &decoded {
        let day = canonical_rithmic_session_day(instrument, calendar, bar)?;
        if window.contains(&day.exchange_timestamp_seconds) {
            days.push(day);
        }
    }
    Ok(days)
}

/// Folds the minute bars of a trading date that has not closed into one bar
/// labelled by its session open; `None` before the date's first trade.
fn collect_open_trading_date<T: RithmicHistoryTransport>(
    transport: &mut T,
    instrument: &RithmicProviderInstrument,
    trading_day: i64,
    open_seconds: i64,
    end_seconds: i32,
) -> Result<Option<MarketBar>, RithmicHistoryAdapterError> {
    let start_seconds =
        i32::try_from(open_seconds).map_err(|_| RithmicHistoryAdapterError::InvalidRequest)?;
    let minutes = usize::try_from(end_seconds - start_seconds)
        .ok()
        .and_then(|seconds| seconds.div_ceil(60).checked_add(TIME_REPLAY_PADDING_BARS))
        .and_then(NonZeroUsize::new)
        .ok_or(RithmicHistoryAdapterError::InvalidRequest)?;
    let resolution = RithmicTimeBarResolution::try_new(
        ChartInterval::Minute1.label(),
        TimeBarType::Minute,
        NonZeroU16::MIN,
    )?;
    let mut bars = collect_time_bars(
        transport,
        instrument,
        &resolution,
        start_seconds,
        end_seconds,
        minutes,
    )?
    .into_iter()
    .map(|minute| minute.value);
    let Some(first) = bars.next() else {
        return Ok(None);
    };
    let mut day = MarketBar {
        source_sequence: session_day_sequence(trading_day)?,
        exchange_timestamp_seconds: open_seconds,
        exchange_timestamp_unix_nanos: open_seconds
            .checked_mul(NANOS_PER_SECOND_I64)
            .ok_or(RithmicHistoryAdapterError::MalformedHistory)?,
        ..first
    };
    for minute in bars {
        extend_bar(&mut day, &minute)?;
    }
    Ok(Some(day))
}

/// Converts one daily replay bar, whose marker is its YYYYMMDD trading date,
/// to a bar labelled by the instant that date's session opens.
fn canonical_rithmic_session_day(
    instrument: &RithmicProviderInstrument,
    calendar: ExchangeCalendar,
    bar: &DecodedTimeBar,
) -> Result<MarketBar, RithmicHistoryAdapterError> {
    if bar.identity.symbol != instrument.descriptor.provider_symbol
        || bar.identity.exchange != instrument.descriptor.venue_id
        || bar.bar_type != DecodedTimeBarType::Daily
        || rithmic_wire_period(DecodedTimeBarType::Daily, 1).as_deref() != Some(bar.period.as_str())
    {
        return Err(RithmicHistoryAdapterError::MalformedHistory);
    }
    let trading_day = replayed_trading_day(bar.marker_seconds)
        .ok_or(RithmicHistoryAdapterError::MalformedHistory)?;
    let open_seconds = calendar.trading_day_open(trading_day);
    let value = MarketBar {
        source_sequence: session_day_sequence(trading_day)?,
        exchange_timestamp_seconds: open_seconds,
        exchange_timestamp_unix_nanos: open_seconds
            .checked_mul(NANOS_PER_SECOND_I64)
            .ok_or(RithmicHistoryAdapterError::MalformedHistory)?,
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
    value
        .validate()
        .map_err(|_| RithmicHistoryAdapterError::MalformedHistory)?;
    Ok(value)
}

fn session_day_sequence(trading_day: i64) -> Result<u64, RithmicHistoryAdapterError> {
    u64::try_from(trading_day)
        .ok()
        .and_then(|day| day.checked_add(1))
        .ok_or(RithmicHistoryAdapterError::MalformedHistory)
}

/// The YYYYMMDD index Rithmic uses for a trading date (days since the Unix epoch).
fn replay_trading_date(trading_day: i64) -> Option<i32> {
    let date = DateTime::from_timestamp(trading_day.checked_mul(SECONDS_PER_DAY)?, 0)?.date_naive();
    let month_day = i32::try_from(date.month() * 100 + date.day()).ok()?;
    date.year().checked_mul(10_000)?.checked_add(month_day)
}

/// The trading date (days since the Unix epoch) of a YYYYMMDD replay marker.
fn replayed_trading_day(marker: i32) -> Option<i64> {
    let date = NaiveDate::from_ymd_opt(
        marker / 10_000,
        u32::try_from(marker / 100 % 100).ok()?,
        u32::try_from(marker % 100).ok()?,
    )?;
    let trading_day = date
        .and_hms_opt(0, 0, 0)?
        .and_utc()
        .timestamp()
        .div_euclid(SECONDS_PER_DAY);
    (replay_trading_date(trading_day)? == marker).then_some(trading_day)
}

fn extend_bar(bar: &mut MarketBar, next: &MarketBar) -> Result<(), RithmicHistoryAdapterError> {
    bar.high = bar.high.max(next.high);
    bar.low = bar.low.min(next.low);
    bar.close = next.close;
    bar.volume = bar
        .volume
        .checked_add(next.volume)
        .ok_or(RithmicHistoryAdapterError::MalformedHistory)?;
    Ok(())
}

fn aggregate_daily_chart_history(
    daily: Vec<MarketBar>,
    period: ExchangeCalendarPeriod,
    calendar: ExchangeCalendar,
) -> Result<Vec<MarketBar>, RithmicHistoryAdapterError> {
    let daily = daily
        .into_iter()
        .map(|bar| (bar.exchange_timestamp_unix_nanos, bar))
        .collect::<BTreeMap<_, _>>();
    let mut aggregated = Vec::new();
    let mut active = None::<(ExchangeCalendarBucket, MarketBar)>;
    for (_, daily_bar) in daily {
        let bucket = calendar.bucket(daily_bar.exchange_timestamp_seconds, period);
        match &mut active {
            Some((active_bucket, aggregate)) if *active_bucket == bucket => {
                extend_bar(aggregate, &daily_bar)?;
            }
            _ => {
                if let Some((_, completed)) = active.take() {
                    aggregated.push(completed);
                }
                active = Some((bucket, daily_bar));
            }
        }
    }
    if let Some((_, completed)) = active {
        aggregated.push(completed);
    }
    for (index, bar) in aggregated.iter_mut().enumerate() {
        bar.source_sequence = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
        bar.validate()
            .map_err(|_| RithmicHistoryAdapterError::MalformedHistory)?;
    }
    Ok(aggregated)
}

/// Converts one provider tick bar without discarding its subsecond ordering key.
///
/// # Errors
///
/// Returns an error for mismatched identity/specifier, invalid keys, missing
/// volume, or unrepresentable fixed-point values.
pub fn canonical_rithmic_tick_bar(
    instrument: &RithmicProviderInstrument,
    trades_per_bar: u16,
    source_sequence: NonZeroU64,
    bar: &crate::DecodedTickBar,
) -> Result<MarketBar, RithmicHistoryAdapterError> {
    let last = bar
        .keys
        .last()
        .ok_or(RithmicHistoryAdapterError::MalformedHistory)?;
    if bar.identity.symbol != instrument.descriptor.provider_symbol
        || bar.identity.exchange != instrument.descriptor.venue_id
        || bar.trades_per_bar != trades_per_bar.to_string()
        || last.seconds < 0
        || !(0..1_000_000).contains(&last.microseconds)
    {
        return Err(RithmicHistoryAdapterError::MalformedHistory);
    }
    let exchange_timestamp_unix_nanos = i64::from(last.seconds)
        .checked_mul(NANOS_PER_SECOND_I64)
        .and_then(|seconds| seconds.checked_add(i64::from(last.microseconds) * 1_000))
        .ok_or(RithmicHistoryAdapterError::MalformedHistory)?;
    let value = MarketBar {
        source_sequence: source_sequence.get(),
        exchange_timestamp_seconds: i64::from(last.seconds),
        exchange_timestamp_unix_nanos,
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
    value
        .validate()
        .map_err(|_| RithmicHistoryAdapterError::MalformedHistory)?;
    Ok(value)
}

/// Decodes one adapter-owned provider-history payload.
///
/// # Errors
///
/// Returns an error for a foreign schema or payload/item identity mismatch.
pub fn decode_rithmic_history_bar(item: &HistoryItem) -> Result<MarketBar, String> {
    let current = item.payload.len() == HISTORY_PAYLOAD_BYTES
        && item.payload.starts_with(HISTORY_PAYLOAD_MAGIC);
    let legacy = item.payload.len() == LEGACY_HISTORY_PAYLOAD_BYTES
        && item.payload.starts_with(LEGACY_HISTORY_PAYLOAD_MAGIC);
    if !current && !legacy {
        return Err(RithmicHistoryAdapterError::MalformedHistory.to_string());
    }
    let mut offset = HISTORY_PAYLOAD_MAGIC.len();
    let source_sequence = read_u64(&item.payload, &mut offset)?;
    let exchange_timestamp_seconds = read_i64(&item.payload, &mut offset)?;
    let exchange_timestamp_unix_nanos = if current {
        read_i64(&item.payload, &mut offset)?
    } else {
        exchange_timestamp_seconds
            .checked_mul(NANOS_PER_SECOND_I64)
            .ok_or_else(|| RithmicHistoryAdapterError::MalformedHistory.to_string())?
    };
    let bar = MarketBar {
        source_sequence,
        exchange_timestamp_seconds,
        exchange_timestamp_unix_nanos,
        open: read_i64(&item.payload, &mut offset)?,
        high: read_i64(&item.payload, &mut offset)?,
        low: read_i64(&item.payload, &mut offset)?,
        close: read_i64(&item.payload, &mut offset)?,
        volume: read_i64(&item.payload, &mut offset)?,
    };
    if bar.source_sequence != item.sequence
        || bar.exchange_timestamp_unix_nanos != item.event_time_unix_nanos
    {
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
    payload.extend_from_slice(&bar.exchange_timestamp_unix_nanos.to_le_bytes());
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
}

/// Named deterministic evidence that a newer covering page restores a gapped handoff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicCoveringRecoveryEvidence {
    pub failed_generation: u64,
    pub recovery_generation: u64,
    pub recovered_watermark: u64,
}

/// Exercises the production Rithmic history/live continuity state machine.
///
/// # Errors
///
/// Returns an error unless a live gap requires recovery and only a newer page
/// covering the observed watermark restores the handoff.
pub fn collect_rithmic_covering_recovery_evidence()
-> Result<RithmicCoveringRecoveryEvidence, RithmicHistoryAdapterError> {
    fn page(last_sequence: u64) -> HistoryPage {
        let request = HistoryPageRequest {
            provider_id: PROVIDER_ID.to_string(),
            account_id: RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID.to_string(),
            entitlement_revision: "deterministic-resilience-evidence-v1".to_string(),
            instrument_id: "evidence-instrument".to_string(),
            data_class: DataClass::Bars,
            resolution: "1m".to_string(),
            range: HistoryRange {
                start_unix_nanos: 0,
                end_unix_nanos: i64::try_from(last_sequence)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(NANOS_PER_SECOND_I64),
            },
            maximum_items: NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN),
            continuation: None,
        };
        let items = (1..=last_sequence)
            .map(|sequence| {
                let timestamp = i64::try_from(sequence).unwrap_or(i64::MAX);
                let bar = MarketBar {
                    source_sequence: sequence,
                    exchange_timestamp_seconds: timestamp,
                    exchange_timestamp_unix_nanos: timestamp * NANOS_PER_SECOND_I64,
                    open: 100,
                    high: 100,
                    low: 100,
                    close: 100,
                    volume: 1,
                };
                HistoryItem {
                    sequence,
                    event_time_unix_nanos: timestamp.saturating_mul(NANOS_PER_SECOND_I64),
                    payload: encode_history_bar(bar),
                }
            })
            .collect();
        HistoryPage {
            request,
            items,
            next: None,
        }
    }

    let mut continuity =
        RithmicBarContinuity::new(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
    continuity.install_covering_page(NonZeroU64::MIN, &page(2))?;
    let gap = SequencedHistory {
        sequence: NonZeroU64::new(4).unwrap_or(NonZeroU64::MIN),
        value: MarketBar {
            source_sequence: 4,
            exchange_timestamp_seconds: 4,
            exchange_timestamp_unix_nanos: 4 * NANOS_PER_SECOND_I64,
            open: 100,
            high: 100,
            low: 100,
            close: 100,
            volume: 1,
        },
    };
    if continuity.push_live(gap) != Err(RithmicHistoryAdapterError::IncompleteCoverage)
        || continuity.state()
            != (HandoffState::SnapshotRequired {
                minimum_generation: 1,
                minimum_watermark: 4,
            })
    {
        return Err(RithmicHistoryAdapterError::IncompleteCoverage);
    }
    continuity.install_covering_page(NonZeroU64::new(2).unwrap_or(NonZeroU64::MIN), &page(4))?;
    if continuity.state()
        != (HandoffState::Live {
            generation: 2,
            last_sequence: 4,
        })
    {
        return Err(RithmicHistoryAdapterError::IncompleteCoverage);
    }
    Ok(RithmicCoveringRecoveryEvidence {
        failed_generation: 1,
        recovery_generation: 2,
        recovered_watermark: 4,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InstrumentDescriptor;
    use crate::{BarIdentity, DecodedTickBar, ObservedHistoryRange, Ohlc, TickBarKey};
    use aeris_provider_history::HistoryRange;
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    #[derive(Clone)]
    struct FixtureTransport {
        responses: Rc<RefCell<VecDeque<Result<CollectedHistory, RithmicHistoryAdapterError>>>>,
    }

    impl RithmicHistoryTransport for FixtureTransport {
        fn collect_history(
            &mut self,
            request: HistoryCollectionRequest,
        ) -> Result<CollectedHistory, RithmicHistoryAdapterError> {
            let mut history = self.responses.borrow_mut().pop_front().ok_or(
                RithmicHistoryAdapterError::Session(RithmicSessionError::Transport),
            )??;
            history.request = request;
            Ok(history)
        }
    }

    struct EchoChartTransport;

    impl RithmicHistoryTransport for EchoChartTransport {
        fn collect_history(
            &mut self,
            request: HistoryCollectionRequest,
        ) -> Result<CollectedHistory, RithmicHistoryAdapterError> {
            let bars = match request.series {
                HistorySeries::Time { bar_type, period } => {
                    // The bar after the first one the request names: a second
                    // or minute marker is its close time, a daily marker its
                    // YYYYMMDD trading date.
                    let marker_seconds = match bar_type {
                        DecodedTimeBarType::Second => request
                            .start_seconds
                            .saturating_add(period.saturating_mul(2)),
                        DecodedTimeBarType::Minute => request
                            .start_seconds
                            .saturating_add(period.saturating_mul(120)),
                        DecodedTimeBarType::Daily | DecodedTimeBarType::Weekly => {
                            replayed_trading_day(request.start_seconds)
                                .and_then(|day| replay_trading_date(day + 1))
                                .expect("echo trading date")
                        }
                    };
                    HistoryBars::Time(vec![DecodedTimeBar {
                        identity: BarIdentity {
                            symbol: request.symbol.clone(),
                            exchange: request.exchange.clone(),
                        },
                        bar_type,
                        period: rithmic_wire_period(bar_type, period).expect("echo period"),
                        marker_seconds,
                        ohlc: Ohlc {
                            open: 5_100.0,
                            high: 5_101.0,
                            low: 5_099.0,
                            close: 5_100.5,
                        },
                        trades: Some(1),
                        volume: Some(2),
                        bid_volume: None,
                        ask_volume: None,
                    }])
                }
                HistorySeries::Tick { trades_per_bar } => HistoryBars::Tick(vec![DecodedTickBar {
                    identity: BarIdentity {
                        symbol: request.symbol.clone(),
                        exchange: request.exchange.clone(),
                    },
                    trades_per_bar: trades_per_bar.to_string(),
                    keys: vec![TickBarKey {
                        sequence: "echo".to_string(),
                        seconds: request.start_seconds,
                        microseconds: 123_456,
                    }],
                    ohlc: Ohlc {
                        open: 5_100.0,
                        high: 5_101.0,
                        low: 5_099.0,
                        close: 5_100.5,
                    },
                    trades: Some(u64::from(trades_per_bar)),
                    volume: Some(2),
                    bid_volume: None,
                    ask_volume: None,
                }]),
            };
            Ok(CollectedHistory {
                request,
                bars,
                observed_range: None,
                duplicate_count: 0,
            })
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
                price_increment: Some(25),
                contract: Some(Box::new(crate::InstrumentContractMetadata {
                    point_value: Some((5_000, 2)),
                    currency: Some("USD".to_string()),
                    expiration_date: Some("2027-06-18".to_string()),
                    first_notice_date: None,
                    last_trade_date: None,
                })),
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
    fn resolutions_are_fixed_intervals_not_trading_dates() {
        for (resolution, bar_type) in [("1d", TimeBarType::Daily), ("1w", TimeBarType::Weekly)] {
            assert_eq!(
                RithmicTimeBarResolution::try_new(resolution, bar_type, NonZeroU16::MIN),
                Err(RithmicHistoryAdapterError::InvalidConfiguration)
            );
        }
    }

    #[test]
    fn canonical_tick_bar_preserves_subsecond_ordering_and_fixed_point_values() {
        let decoded = DecodedTickBar {
            identity: BarIdentity {
                symbol: "ESM7".to_string(),
                exchange: "CME".to_string(),
            },
            trades_per_bar: "100".to_string(),
            keys: vec![TickBarKey {
                sequence: "provider-sequence".to_string(),
                seconds: 1_800_000_000,
                microseconds: 123_456,
            }],
            ohlc: Ohlc {
                open: 5_100.25,
                high: 5_101.0,
                low: 5_100.0,
                close: 5_100.75,
            },
            trades: Some(100),
            volume: Some(42),
            bid_volume: None,
            ask_volume: None,
        };
        let canonical = canonical_rithmic_tick_bar(
            &instrument(),
            100,
            NonZeroU64::new(7).unwrap_or(NonZeroU64::MIN),
            &decoded,
        )
        .expect("tick bar converts");
        assert_eq!(canonical.source_sequence, 7);
        assert_eq!(canonical.open, 510_025);
        assert_eq!(canonical.volume, 42);
        assert_eq!(
            canonical.exchange_timestamp_unix_nanos,
            1_800_000_000_123_456_000
        );
    }

    #[test]
    fn rithmic_payload_binds_exact_time_and_reads_legacy_whole_seconds() {
        let bar = MarketBar {
            source_sequence: 7,
            exchange_timestamp_seconds: 1_800_000_000,
            exchange_timestamp_unix_nanos: 1_800_000_000_123_456_000,
            open: 510_025,
            high: 510_100,
            low: 510_000,
            close: 510_075,
            volume: 42,
        };
        let current = HistoryItem {
            sequence: bar.source_sequence,
            event_time_unix_nanos: bar.exchange_timestamp_unix_nanos,
            payload: encode_history_bar(bar),
        };
        assert_eq!(decode_rithmic_history_bar(&current), Ok(bar));
        let mut mismatched = current.clone();
        mismatched.event_time_unix_nanos += 1;
        assert!(decode_rithmic_history_bar(&mismatched).is_err());

        let mut payload = Vec::with_capacity(LEGACY_HISTORY_PAYLOAD_BYTES);
        payload.extend_from_slice(LEGACY_HISTORY_PAYLOAD_MAGIC);
        payload.extend_from_slice(&bar.source_sequence.to_le_bytes());
        payload.extend_from_slice(&bar.exchange_timestamp_seconds.to_le_bytes());
        for value in [bar.open, bar.high, bar.low, bar.close, bar.volume] {
            payload.extend_from_slice(&value.to_le_bytes());
        }
        let legacy = HistoryItem {
            sequence: bar.source_sequence,
            event_time_unix_nanos: bar.exchange_timestamp_seconds * NANOS_PER_SECOND_I64,
            payload,
        };
        assert_eq!(
            decode_rithmic_history_bar(&legacy)
                .expect("legacy payload decodes")
                .exchange_timestamp_unix_nanos,
            legacy.event_time_unix_nanos
        );
    }

    #[test]
    fn chart_history_boundary_covers_every_rithmic_interval() {
        for interval in ChartInterval::ALL {
            let bars = collect_rithmic_chart_history(
                &mut EchoChartTransport,
                &instrument(),
                interval,
                HistoryRange {
                    start_unix_nanos: 0,
                    end_unix_nanos: 259_200 * NANOS_PER_SECOND_I64,
                },
                NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN),
            )
            .expect("chart interval collects");
            assert_eq!(bars.len(), 1, "{} response", interval.label());
            assert_eq!(bars[0].source_sequence, 1);
        }
    }

    fn daily_bar(timestamp: i64, ohlc: [i64; 4], volume: i64) -> MarketBar {
        MarketBar {
            source_sequence: 99,
            exchange_timestamp_seconds: timestamp,
            exchange_timestamp_unix_nanos: timestamp * NANOS_PER_SECOND_I64,
            open: ohlc[0],
            high: ohlc[1],
            low: ohlc[2],
            close: ohlc[3],
            volume,
        }
    }

    #[test]
    fn calendar_aggregation_uses_monday_month_and_leap_day_boundaries() {
        let monday = 1_704_067_200;
        let weekly = aggregate_daily_chart_history(
            vec![
                daily_bar(monday, [100, 110, 90, 105], 10),
                daily_bar(monday + 86_400, [105, 115, 95, 112], 20),
                daily_bar(monday + 4 * 86_400, [112, 120, 108, 118], 30),
                daily_bar(monday + 7 * 86_400, [118, 125, 115, 122], 40),
            ],
            ExchangeCalendarPeriod::Week,
            cme(),
        )
        .expect("weekly aggregation validates");
        assert_eq!(weekly.len(), 2);
        assert_eq!(weekly[0].source_sequence, 1);
        assert_eq!(weekly[0].exchange_timestamp_seconds, monday);
        assert_eq!(
            [
                weekly[0].open,
                weekly[0].high,
                weekly[0].low,
                weekly[0].close,
            ],
            [100, 120, 90, 118]
        );
        assert_eq!(weekly[0].volume, 60);

        let february_1 = 1_706_745_600;
        let march_1 = 1_709_251_200;
        let monthly = aggregate_daily_chart_history(
            vec![
                daily_bar(1_706_659_200, [90, 100, 80, 95], 5),
                daily_bar(february_1, [100, 110, 90, 105], 10),
                daily_bar(1_709_164_800, [105, 120, 85, 115], 20),
                daily_bar(march_1, [115, 130, 110, 125], 30),
            ],
            ExchangeCalendarPeriod::Month,
            cme(),
        )
        .expect("monthly aggregation validates");
        assert_eq!(monthly.len(), 3);
        assert_eq!(monthly[1].exchange_timestamp_seconds, february_1);
        assert_eq!(monthly[1].volume, 30);
        assert_eq!(monthly[2].exchange_timestamp_seconds, march_1);
    }

    struct RecordingTransport {
        replies: VecDeque<Vec<DecodedTimeBar>>,
        requests: Vec<HistoryCollectionRequest>,
    }

    impl RithmicHistoryTransport for RecordingTransport {
        fn collect_history(
            &mut self,
            request: HistoryCollectionRequest,
        ) -> Result<CollectedHistory, RithmicHistoryAdapterError> {
            self.requests.push(request.clone());
            let bars = self
                .replies
                .pop_front()
                .ok_or(RithmicHistoryAdapterError::Session(
                    RithmicSessionError::Transport,
                ))?;
            Ok(CollectedHistory {
                request,
                bars: HistoryBars::Time(bars),
                observed_range: None,
                duplicate_count: 0,
            })
        }
    }

    fn cme() -> ExchangeCalendar {
        ExchangeCalendar::for_code("CME").expect("CME calendar")
    }

    fn session_open(trading_date: i32) -> i64 {
        cme().trading_day_open(replayed_trading_day(trading_date).expect("trading date"))
    }

    fn daily_replay_bar(trading_date: i32, close: f64, volume: u64) -> DecodedTimeBar {
        DecodedTimeBar {
            bar_type: DecodedTimeBarType::Daily,
            period: "daily".to_string(),
            ..time_bar(trading_date, close, volume)
        }
    }

    #[test]
    fn session_history_replays_trading_dates_and_labels_bars_by_session_open() {
        assert_eq!(session_open(20_261_020), 1_792_447_200);
        let mut transport = RecordingTransport {
            replies: VecDeque::from([[13, 14, 15, 16, 19, 20, 21]
                .into_iter()
                .map(|day| daily_replay_bar(20_261_000 + day, 5_100.0 + f64::from(day), 1))
                .collect()]),
            requests: Vec::new(),
        };
        let bars = collect_rithmic_chart_history(
            &mut transport,
            &instrument(),
            ChartInterval::Day3,
            HistoryRange {
                start_unix_nanos: session_open(20_261_013) * NANOS_PER_SECOND_I64,
                end_unix_nanos: session_open(20_261_022) * NANOS_PER_SECOND_I64,
            },
            NonZeroUsize::new(8).unwrap(),
        )
        .expect("three-day history collects");
        let request = &transport.requests[0];
        assert_eq!(
            request.series,
            HistorySeries::Time {
                bar_type: DecodedTimeBarType::Daily,
                period: 1,
            }
        );
        assert_eq!(
            (
                request.start_seconds,
                request.finish_seconds,
                request.maximum_bars.get()
            ),
            (20_261_013, 20_261_021, 9)
        );
        // Three-day buckets count trading dates from the epoch, as live bars do.
        assert_eq!(
            bars.iter()
                .map(|bar| (
                    bar.source_sequence,
                    bar.exchange_timestamp_seconds,
                    bar.open,
                    bar.close,
                    bar.volume
                ))
                .collect::<Vec<_>>(),
            [
                (1, session_open(20_261_013), 511_300, 511_500, 3),
                (2, session_open(20_261_016), 511_600, 511_600, 1),
                (3, session_open(20_261_019), 511_900, 512_100, 3),
            ]
        );
    }

    #[test]
    fn session_history_folds_the_open_trading_date_from_minute_bars() {
        let today = session_open(20_261_022);
        let closed_days = || {
            [13, 14, 15, 16, 19, 20, 21]
                .into_iter()
                .map(|day| daily_replay_bar(20_261_000 + day, 5_100.0, 10))
                .collect::<Vec<_>>()
        };
        // Minute markers are close times; the padded replay also returns the
        // minute that closed as the session opened.
        let minutes = || {
            [
                (0, 5_090.0, 99),
                (60, 5_101.0, 1),
                (120, 5_099.0, 2),
                (180, 5_102.0, 3),
            ]
            .map(|(close_offset, close, volume)| {
                time_bar(i32::try_from(today + close_offset).unwrap(), close, volume)
            })
            .to_vec()
        };
        let range = HistoryRange {
            start_unix_nanos: session_open(20_261_013) * NANOS_PER_SECOND_I64,
            end_unix_nanos: (today + 3 * 3_600) * NANOS_PER_SECOND_I64,
        };
        let collect = |interval| {
            let mut transport = RecordingTransport {
                replies: VecDeque::from([closed_days(), minutes()]),
                requests: Vec::new(),
            };
            let history = collect_rithmic_session_history(
                &mut transport,
                &instrument(),
                interval,
                range,
                NonZeroUsize::new(8).unwrap(),
            )
            .expect("session history collects");
            (history, transport.requests)
        };

        let (daily, requests) = collect(ChartInterval::Day1);
        assert_eq!(
            (requests[0].start_seconds, requests[0].finish_seconds),
            (20_261_013, 20_261_022)
        );
        assert_eq!(
            requests[1].series,
            HistorySeries::Time {
                bar_type: DecodedTimeBarType::Minute,
                period: 1,
            }
        );
        assert_eq!(
            requests[1].start_seconds,
            i32::try_from(today - 60).unwrap()
        );
        assert_eq!(daily.closed.len(), 7);
        let open = daily.open.expect("the trading date is open");
        assert_eq!(
            (
                open.exchange_timestamp_seconds,
                open.open,
                open.high,
                open.low,
                open.close,
                open.volume
            ),
            (today, 510_100, 510_200, 509_900, 510_200, 6)
        );

        let (weekly, _) = collect(ChartInterval::Week1);
        assert_eq!(
            weekly
                .closed
                .iter()
                .map(|bar| (bar.exchange_timestamp_seconds, bar.volume))
                .collect::<Vec<_>>(),
            [(session_open(20_261_013), 40)]
        );
        let open = weekly.open.expect("the week is open");
        assert_eq!(
            (open.exchange_timestamp_seconds, open.close, open.volume),
            (session_open(20_261_019), 510_200, 36)
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
        responses: Vec<Result<CollectedHistory, RithmicHistoryAdapterError>>,
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
        // Markers are close times. The padded replay also returns the bar
        // before the window and the one opening at its end.
        let bars = [
            time_bar(start_seconds, 5099.75, 9),
            time_bar(start_seconds + 60, 5100.25, 1),
            time_bar(start_seconds + 120, 5101.00, 2),
            time_bar(start_seconds + 180, 5101.50, 3),
            time_bar(start_seconds + 240, 5102.00, 9),
        ];
        let mut capability = adapter(vec![Ok(collected(start_seconds, &bars))]);
        let page_request = request(start_nanos, end_nanos, 3);
        let page = capability
            .fetch_page(&page_request)
            .expect("covering page fetches");
        assert_eq!(page.request, page_request);
        assert_eq!(page.items.len(), 3);
        assert_eq!(
            page.items[0].sequence,
            u64::try_from(start_seconds / 60).unwrap() + 1
        );
        assert_eq!(page.items[0].event_time_unix_nanos, start_nanos);
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
    fn chart_history_labels_bars_by_open_time_and_trims_the_replay_padding() {
        let start = 1_800_000_000_i32;
        let second = |marker: i32| DecodedTimeBar {
            bar_type: DecodedTimeBarType::Second,
            period: "5".to_string(),
            ..time_bar(marker, 5_100.0, 1)
        };
        // A second replay returns the bars whose close marker falls inside
        // the padded window.
        let replay = (start - 5..=start + 20)
            .step_by(5)
            .map(second)
            .collect::<Vec<_>>();
        let mut transport = FixtureTransport {
            responses: Rc::new(RefCell::new(VecDeque::from([Ok(collected(
                start, &replay,
            ))]))),
        };
        let bars = collect_rithmic_chart_history(
            &mut transport,
            &instrument(),
            ChartInterval::Second5,
            HistoryRange {
                start_unix_nanos: i64::from(start) * NANOS_PER_SECOND_I64,
                end_unix_nanos: i64::from(start + 15) * NANOS_PER_SECOND_I64,
            },
            NonZeroUsize::new(8).unwrap(),
        )
        .expect("second history collects");
        assert_eq!(
            bars.iter()
                .map(|bar| (bar.source_sequence, bar.exchange_timestamp_seconds))
                .collect::<Vec<_>>(),
            [
                (1, i64::from(start)),
                (2, i64::from(start + 5)),
                (3, i64::from(start + 10)),
            ]
        );
    }

    #[test]
    fn chart_history_reports_why_the_replay_failed() {
        let (start_nanos, end_nanos, _) = aligned_range(2);
        for error in [
            RithmicHistoryAdapterError::Rejected,
            RithmicHistoryAdapterError::ForcedLogout,
            RithmicHistoryAdapterError::Collection(CollectorError::OutsideRequestedRange),
            RithmicHistoryAdapterError::Session(RithmicSessionError::Deadline),
        ] {
            let mut transport = FixtureTransport {
                responses: Rc::new(RefCell::new(VecDeque::from([Err(error)]))),
            };
            assert_eq!(
                collect_rithmic_chart_history(
                    &mut transport,
                    &instrument(),
                    ChartInterval::Minute1,
                    HistoryRange {
                        start_unix_nanos: start_nanos,
                        end_unix_nanos: end_nanos,
                    },
                    NonZeroUsize::new(8).unwrap(),
                ),
                Err(error)
            );
        }
    }

    #[test]
    fn incomplete_or_gapped_coverage_fails_closed_and_poisons_session() {
        let (start_nanos, end_nanos, start_seconds) = aligned_range(3);
        let incomplete = [
            time_bar(start_seconds + 60, 5100.00, 1),
            time_bar(start_seconds + 120, 5101.00, 2),
        ];
        let mut capability = adapter(vec![Ok(collected(start_seconds, &incomplete))]);
        let page_request = request(start_nanos, end_nanos, 3);
        assert!(capability.fetch_page(&page_request).is_err());
        assert!(capability.fetch_page(&page_request).is_err());

        let gapped = [
            time_bar(start_seconds, 5099.00, 1),
            time_bar(start_seconds + 60, 5100.00, 1),
            time_bar(start_seconds + 180, 5102.00, 2),
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
            time_bar(start_seconds + 60, 5100.00, 1),
            time_bar(start_seconds + 120, 5101.00, 2),
            time_bar(start_seconds + 180, 5102.00, 3),
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
            exchange_timestamp_unix_nanos: i64::from(start_seconds + 180) * NANOS_PER_SECOND_I64,
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
                    exchange_timestamp_unix_nanos: i64::from(start_seconds + 300)
                        * NANOS_PER_SECOND_I64,
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

    #[test]
    fn named_covering_recovery_evidence_requires_a_newer_complete_generation() {
        assert_eq!(
            collect_rithmic_covering_recovery_evidence(),
            Ok(RithmicCoveringRecoveryEvidence {
                failed_generation: 1,
                recovery_generation: 2,
                recovered_watermark: 4,
            })
        );
    }
}
