use core::fmt;
use std::{error::Error, mem::size_of, num::NonZeroU64};

/// Maximum bytes accepted in a non-secret diagnostics identity field.
pub const MAXIMUM_DIAGNOSTICS_IDENTITY_BYTES: usize = 128;
/// Fastest allowed immutable diagnostics publication cadence.
pub const MINIMUM_DIAGNOSTICS_SNAPSHOT_INTERVAL_NANOS: u64 = 250_000_000;

const LOCAL_LATENCY_BUCKETS: usize = 64;

/// Non-secret provider identity included in every feed-health snapshot.
#[derive(Clone, Eq, PartialEq)]
pub struct FeedIdentity {
    provider: Box<str>,
    system: Box<str>,
    environment: Box<str>,
}

impl FeedIdentity {
    /// Creates a bounded identity without retaining rejected field contents.
    ///
    /// # Errors
    ///
    /// Returns a field-class error for empty, oversized, or control-bearing input.
    pub fn try_new(
        provider: impl Into<String>,
        system: impl Into<String>,
        environment: impl Into<String>,
    ) -> Result<Self, DiagnosticsError> {
        let provider = provider.into();
        let system = system.into();
        let environment = environment.into();
        validate_identity("provider", &provider)?;
        validate_identity("system", &system)?;
        validate_identity("environment", &environment)?;
        Ok(Self {
            provider: provider.into_boxed_str(),
            system: system.into_boxed_str(),
            environment: environment.into_boxed_str(),
        })
    }

    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    #[must_use]
    pub fn system(&self) -> &str {
        &self.system
    }

    #[must_use]
    pub fn environment(&self) -> &str {
        &self.environment
    }

    fn allocated_bytes(&self) -> usize {
        self.provider
            .len()
            .saturating_add(self.system.len())
            .saturating_add(self.environment.len())
    }
}

impl fmt::Debug for FeedIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FeedIdentity")
            .field("provider", &self.provider)
            .field("system", &self.system)
            .field("environment", &self.environment)
            .finish()
    }
}

fn validate_identity(field: &'static str, value: &str) -> Result<(), DiagnosticsError> {
    if value.trim().is_empty() {
        return Err(DiagnosticsError::EmptyIdentity(field));
    }
    if value.len() > MAXIMUM_DIAGNOSTICS_IDENTITY_BYTES {
        return Err(DiagnosticsError::IdentityTooLong {
            field,
            maximum: MAXIMUM_DIAGNOSTICS_IDENTITY_BYTES,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(DiagnosticsError::ControlCharacter(field));
    }
    Ok(())
}

/// Coarse provider lifecycle state safe for user-facing diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedConnectionState {
    Disconnected,
    Discovering,
    Authenticating,
    Streaming,
    Recovering,
    Stopped,
}

/// Coarse feed recovery reason without provider payload or account text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedRecoveryReason {
    Transport,
    Authentication,
    AgreementRequired,
    UnsupportedSystem,
    SchemaMismatch,
    HeartbeatSilence,
    MessageSilence,
    SequenceGap,
    QueueOverflow,
    MalformedMessage,
    HistoryContinuity,
    OrderBookSnapshot,
}

/// Current history handoff state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryDiagnosticsState {
    Idle,
    Hydrating,
    BufferingLive,
    Ready,
    Recovering,
    Unavailable,
}

/// Current provider-neutral order-book state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderBookDiagnosticsState {
    AwaitingSnapshot,
    Ready,
    Stale,
    Recovering,
}

/// Every always-on counter retained by the diagnostics owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum FeedCounter {
    Trades,
    Quotes,
    DepthSnapshots,
    DepthDeltas,
    Publications,
    Gaps,
    Duplicates,
    MalformedMessages,
    StaleCallbacks,
    Overflows,
    CoalescedUiUpdates,
}

impl FeedCounter {
    pub const COUNT: usize = 11;
    pub const ALL: [Self; Self::COUNT] = [
        Self::Trades,
        Self::Quotes,
        Self::DepthSnapshots,
        Self::DepthDeltas,
        Self::Publications,
        Self::Gaps,
        Self::Duplicates,
        Self::MalformedMessages,
        Self::StaleCallbacks,
        Self::Overflows,
        Self::CoalescedUiUpdates,
    ];
}

/// Named queue gauges included in every snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum DiagnosticsQueue {
    ProviderCallback,
    SemanticEvent,
    HistoryHandoff,
    ModelPublication,
    UiUpdate,
}

impl DiagnosticsQueue {
    pub const COUNT: usize = 5;
    pub const ALL: [Self; Self::COUNT] = [
        Self::ProviderCallback,
        Self::SemanticEvent,
        Self::HistoryHandoff,
        Self::ModelPublication,
        Self::UiUpdate,
    ];
}

/// Local processing intervals eligible for opt-in detailed histograms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum LocalLatencyMetric {
    SocketReadToDecode,
    DecodeToCanonicalAccept,
    CanonicalAcceptToModelPublish,
    ModelPublishToUiEnqueue,
    UiEnqueueToFrameSubmit,
    FrameSubmitToFrameCallback,
    FrameSubmitToPresent,
}

impl LocalLatencyMetric {
    pub const COUNT: usize = 7;
    pub const ALL: [Self; Self::COUNT] = [
        Self::SocketReadToDecode,
        Self::DecodeToCanonicalAccept,
        Self::CanonicalAcceptToModelPublish,
        Self::ModelPublishToUiEnqueue,
        Self::UiEnqueueToFrameSubmit,
        Self::FrameSubmitToFrameCallback,
        Self::FrameSubmitToPresent,
    ];
}

/// Honest classification attached to every published age or latency value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimingLabel {
    LocalMonotonicAge,
    LocalProcessingLatency,
    ProviderClockRelativeAge,
}

/// One signed provider-clock-relative age. It is never labelled network latency.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockRelativeAge {
    pub nanos: i64,
    pub label: TimingLabel,
}

/// Current and high-water occupancy for one bounded queue.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueueDiagnostics {
    pub current_items: usize,
    pub high_water_items: usize,
    pub item_capacity: usize,
    pub current_bytes: usize,
    pub high_water_bytes: usize,
    pub byte_capacity: usize,
}

impl QueueDiagnostics {
    fn observe(
        &mut self,
        current_items: usize,
        item_capacity: usize,
        current_bytes: usize,
        byte_capacity: usize,
    ) -> Result<(), DiagnosticsError> {
        if item_capacity == 0 || byte_capacity == 0 {
            return Err(DiagnosticsError::ZeroCapacity);
        }
        if current_items > item_capacity || current_bytes > byte_capacity {
            return Err(DiagnosticsError::OccupancyExceedsCapacity);
        }
        if (self.item_capacity != 0 && self.item_capacity != item_capacity)
            || (self.byte_capacity != 0 && self.byte_capacity != byte_capacity)
        {
            return Err(DiagnosticsError::CapacityChanged);
        }
        self.current_items = current_items;
        self.high_water_items = self.high_water_items.max(current_items);
        self.item_capacity = item_capacity;
        self.current_bytes = current_bytes;
        self.high_water_bytes = self.high_water_bytes.max(current_bytes);
        self.byte_capacity = byte_capacity;
        Ok(())
    }
}

/// Current and high-water approximate runtime memory.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MemoryDiagnostics {
    pub current_bytes: usize,
    pub high_water_bytes: usize,
    pub configured_bound_bytes: usize,
}

impl MemoryDiagnostics {
    fn observe(
        &mut self,
        current_bytes: usize,
        bound_bytes: usize,
    ) -> Result<(), DiagnosticsError> {
        if bound_bytes == 0 {
            return Err(DiagnosticsError::ZeroCapacity);
        }
        if current_bytes > bound_bytes {
            return Err(DiagnosticsError::MemoryBoundExceeded {
                requested: current_bytes,
                maximum: bound_bytes,
            });
        }
        if self.configured_bound_bytes != 0 && self.configured_bound_bytes != bound_bytes {
            return Err(DiagnosticsError::CapacityChanged);
        }
        self.current_bytes = current_bytes;
        self.high_water_bytes = self.high_water_bytes.max(current_bytes);
        self.configured_bound_bytes = bound_bytes;
        Ok(())
    }
}

/// Snapshot-friendly named counter values.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FeedCounterSnapshot {
    pub trades: u64,
    pub quotes: u64,
    pub depth_snapshots: u64,
    pub depth_deltas: u64,
    pub publications: u64,
    pub gaps: u64,
    pub duplicates: u64,
    pub malformed_messages: u64,
    pub stale_callbacks: u64,
    pub overflows: u64,
    pub coalesced_ui_updates: u64,
}

impl From<[u64; FeedCounter::COUNT]> for FeedCounterSnapshot {
    fn from(values: [u64; FeedCounter::COUNT]) -> Self {
        Self {
            trades: values[FeedCounter::Trades as usize],
            quotes: values[FeedCounter::Quotes as usize],
            depth_snapshots: values[FeedCounter::DepthSnapshots as usize],
            depth_deltas: values[FeedCounter::DepthDeltas as usize],
            publications: values[FeedCounter::Publications as usize],
            gaps: values[FeedCounter::Gaps as usize],
            duplicates: values[FeedCounter::Duplicates as usize],
            malformed_messages: values[FeedCounter::MalformedMessages as usize],
            stale_callbacks: values[FeedCounter::StaleCallbacks as usize],
            overflows: values[FeedCounter::Overflows as usize],
            coalesced_ui_updates: values[FeedCounter::CoalescedUiUpdates as usize],
        }
    }
}

/// Per-second rates expressed in milli-events per second without floating point.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FeedRateSnapshot {
    pub trades_per_second_milli: u64,
    pub quotes_per_second_milli: u64,
    pub depth_updates_per_second_milli: u64,
    pub publications_per_second_milli: u64,
}

/// Bounded approximate percentile evidence from one fixed histogram.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LatencyHistogramSnapshot {
    pub metric: LocalLatencyMetric,
    pub label: TimingLabel,
    pub p50_upper_bound_nanos: u64,
    pub p95_upper_bound_nanos: u64,
    pub p99_upper_bound_nanos: u64,
    pub p99_9_upper_bound_nanos: u64,
    pub maximum_nanos: u64,
    pub sample_count: u64,
    pub rejected_samples: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FixedLatencyHistogram {
    buckets: [u64; LOCAL_LATENCY_BUCKETS],
    sample_count: u64,
    rejected_samples: u64,
    maximum_nanos: u64,
}

impl Default for FixedLatencyHistogram {
    fn default() -> Self {
        Self {
            buckets: [0; LOCAL_LATENCY_BUCKETS],
            sample_count: 0,
            rejected_samples: 0,
            maximum_nanos: 0,
        }
    }
}

impl FixedLatencyHistogram {
    fn record(&mut self, elapsed_nanos: u64, maximum_sample_nanos: u64) {
        if elapsed_nanos > maximum_sample_nanos {
            self.rejected_samples = self.rejected_samples.saturating_add(1);
            return;
        }
        let index = elapsed_nanos
            .checked_ilog2()
            .map_or(0, |value| usize::try_from(value).unwrap_or(usize::MAX))
            .min(LOCAL_LATENCY_BUCKETS - 1);
        self.buckets[index] = self.buckets[index].saturating_add(1);
        self.sample_count = self.sample_count.saturating_add(1);
        self.maximum_nanos = self.maximum_nanos.max(elapsed_nanos);
    }

    fn snapshot(
        self,
        metric: LocalLatencyMetric,
        maximum_sample_nanos: u64,
    ) -> Option<LatencyHistogramSnapshot> {
        if self.sample_count == 0 && self.rejected_samples == 0 {
            return None;
        }
        if self.sample_count == 0 {
            return Some(LatencyHistogramSnapshot {
                metric,
                label: TimingLabel::LocalProcessingLatency,
                p50_upper_bound_nanos: 0,
                p95_upper_bound_nanos: 0,
                p99_upper_bound_nanos: 0,
                p99_9_upper_bound_nanos: 0,
                maximum_nanos: 0,
                sample_count: 0,
                rejected_samples: self.rejected_samples,
            });
        }
        Some(LatencyHistogramSnapshot {
            metric,
            label: TimingLabel::LocalProcessingLatency,
            p50_upper_bound_nanos: self.percentile_upper_bound(500, maximum_sample_nanos),
            p95_upper_bound_nanos: self.percentile_upper_bound(950, maximum_sample_nanos),
            p99_upper_bound_nanos: self.percentile_upper_bound(990, maximum_sample_nanos),
            p99_9_upper_bound_nanos: self.percentile_upper_bound(999, maximum_sample_nanos),
            maximum_nanos: self.maximum_nanos,
            sample_count: self.sample_count,
            rejected_samples: self.rejected_samples,
        })
    }

    fn percentile_upper_bound(self, permille: u64, maximum_sample_nanos: u64) -> u64 {
        let rank = self
            .sample_count
            .saturating_mul(permille)
            .saturating_add(999)
            / 1_000;
        let mut cumulative = 0_u64;
        for (index, count) in self.buckets.into_iter().enumerate() {
            cumulative = cumulative.saturating_add(count);
            if cumulative >= rank {
                return bucket_upper_bound(index).min(maximum_sample_nanos);
            }
        }
        self.maximum_nanos
    }
}

fn bucket_upper_bound(index: usize) -> u64 {
    u32::try_from(index.saturating_add(1))
        .ok()
        .and_then(|shift| 1_u64.checked_shl(shift))
        .map_or(u64::MAX, |value| value.saturating_sub(1))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DetailedLatencyRecorder {
    maximum_sample_nanos: NonZeroU64,
    histograms: [FixedLatencyHistogram; LocalLatencyMetric::COUNT],
}

impl DetailedLatencyRecorder {
    fn new(maximum_sample_nanos: NonZeroU64) -> Self {
        Self {
            maximum_sample_nanos,
            histograms: [FixedLatencyHistogram::default(); LocalLatencyMetric::COUNT],
        }
    }

    fn record(&mut self, metric: LocalLatencyMetric, elapsed_nanos: u64) {
        self.histograms[metric as usize].record(elapsed_nanos, self.maximum_sample_nanos.get());
    }

    fn snapshots(&self) -> [Option<LatencyHistogramSnapshot>; LocalLatencyMetric::COUNT] {
        std::array::from_fn(|index| {
            self.histograms[index].snapshot(
                LocalLatencyMetric::ALL[index],
                self.maximum_sample_nanos.get(),
            )
        })
    }
}

/// Immutable feed-health publication produced no faster than four times per second.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedDiagnosticsSnapshot {
    pub identity: FeedIdentity,
    pub connection_state: FeedConnectionState,
    pub session_generation: Option<NonZeroU64>,
    pub uptime_nanos: u64,
    pub reconnect_count: u64,
    pub recovery_reason: Option<FeedRecoveryReason>,
    pub heartbeat_age_nanos: Option<u64>,
    pub last_message_age_nanos: Option<u64>,
    pub provider_timestamp_age: Option<ClockRelativeAge>,
    pub counters: FeedCounterSnapshot,
    pub rates: FeedRateSnapshot,
    pub queues: [QueueDiagnostics; DiagnosticsQueue::COUNT],
    pub history_state: HistoryDiagnosticsState,
    pub order_book_state: OrderBookDiagnosticsState,
    pub memory: MemoryDiagnostics,
    pub diagnostics_owner_bytes: usize,
    pub detailed_latency: [Option<LatencyHistogramSnapshot>; LocalLatencyMetric::COUNT],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RateBaseline {
    monotonic_nanos: u64,
    counters: [u64; FeedCounter::COUNT],
}

/// Single-writer allocation-free diagnostics accumulator with bounded snapshots.
pub struct FeedDiagnostics {
    identity: FeedIdentity,
    connection_state: FeedConnectionState,
    session_generation: Option<NonZeroU64>,
    session_started_monotonic_nanos: Option<u64>,
    reconnect_count: u64,
    recovery_reason: Option<FeedRecoveryReason>,
    last_heartbeat_monotonic_nanos: Option<u64>,
    last_message_monotonic_nanos: Option<u64>,
    last_provider_timestamp_unix_nanos: Option<i64>,
    counters: [u64; FeedCounter::COUNT],
    queues: [QueueDiagnostics; DiagnosticsQueue::COUNT],
    history_state: HistoryDiagnosticsState,
    order_book_state: OrderBookDiagnosticsState,
    memory: MemoryDiagnostics,
    detailed_latency: Option<DetailedLatencyRecorder>,
    last_snapshot_monotonic_nanos: Option<u64>,
    rate_baseline: Option<RateBaseline>,
}

impl fmt::Debug for FeedDiagnostics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FeedDiagnostics")
            .field("identity", &self.identity)
            .field("connection_state", &self.connection_state)
            .field("session_generation", &self.session_generation)
            .field("reconnect_count", &self.reconnect_count)
            .field("recovery_reason", &self.recovery_reason)
            .field("detailed_latency_enabled", &self.detailed_latency.is_some())
            .finish_non_exhaustive()
    }
}

impl FeedDiagnostics {
    /// Creates an always-on diagnostics owner. Detailed histograms are opt-in.
    #[must_use]
    pub fn new(identity: FeedIdentity, detailed_latency_maximum_nanos: Option<NonZeroU64>) -> Self {
        Self {
            identity,
            connection_state: FeedConnectionState::Disconnected,
            session_generation: None,
            session_started_monotonic_nanos: None,
            reconnect_count: 0,
            recovery_reason: None,
            last_heartbeat_monotonic_nanos: None,
            last_message_monotonic_nanos: None,
            last_provider_timestamp_unix_nanos: None,
            counters: [0; FeedCounter::COUNT],
            queues: [QueueDiagnostics::default(); DiagnosticsQueue::COUNT],
            history_state: HistoryDiagnosticsState::Idle,
            order_book_state: OrderBookDiagnosticsState::AwaitingSnapshot,
            memory: MemoryDiagnostics::default(),
            detailed_latency: detailed_latency_maximum_nanos.map(DetailedLatencyRecorder::new),
            last_snapshot_monotonic_nanos: None,
            rate_baseline: None,
        }
    }

    /// Starts a strictly newer generation and resets generation-scoped ages.
    ///
    /// # Errors
    ///
    /// Returns an error for a repeated or regressed generation.
    pub fn begin_session(
        &mut self,
        generation: NonZeroU64,
        monotonic_nanos: u64,
    ) -> Result<(), DiagnosticsError> {
        if self
            .session_generation
            .is_some_and(|current| generation <= current)
        {
            return Err(DiagnosticsError::GenerationDidNotAdvance);
        }
        if self.session_generation.is_some() {
            self.reconnect_count = self.reconnect_count.saturating_add(1);
        }
        self.session_generation = Some(generation);
        self.session_started_monotonic_nanos = Some(monotonic_nanos);
        self.last_heartbeat_monotonic_nanos = None;
        self.last_message_monotonic_nanos = None;
        self.last_provider_timestamp_unix_nanos = None;
        self.connection_state = FeedConnectionState::Discovering;
        self.recovery_reason = None;
        self.history_state = HistoryDiagnosticsState::Idle;
        self.order_book_state = OrderBookDiagnosticsState::AwaitingSnapshot;
        Ok(())
    }

    pub const fn set_connection_state(&mut self, state: FeedConnectionState) {
        self.connection_state = state;
        if matches!(state, FeedConnectionState::Streaming) {
            self.recovery_reason = None;
        }
    }

    pub const fn set_history_state(&mut self, state: HistoryDiagnosticsState) {
        self.history_state = state;
    }

    pub const fn set_order_book_state(&mut self, state: OrderBookDiagnosticsState) {
        self.order_book_state = state;
    }

    pub const fn require_recovery(&mut self, reason: FeedRecoveryReason) {
        self.connection_state = FeedConnectionState::Recovering;
        self.recovery_reason = Some(reason);
    }

    pub fn increment(&mut self, counter: FeedCounter) {
        self.increment_by(counter, 1);
    }

    pub fn increment_by(&mut self, counter: FeedCounter, amount: u64) {
        let value = &mut self.counters[counter as usize];
        *value = value.saturating_add(amount);
    }

    pub fn record_heartbeat(&mut self, monotonic_nanos: u64) {
        self.last_heartbeat_monotonic_nanos = Some(monotonic_nanos);
        self.record_message(monotonic_nanos, None);
    }

    pub fn record_message(
        &mut self,
        monotonic_nanos: u64,
        provider_timestamp_unix_nanos: Option<i64>,
    ) {
        self.last_message_monotonic_nanos = Some(monotonic_nanos);
        if let Some(timestamp) = provider_timestamp_unix_nanos {
            self.last_provider_timestamp_unix_nanos = Some(timestamp);
        }
    }

    /// Observes one queue without accepting an impossible or changing bound.
    ///
    /// # Errors
    ///
    /// Returns an error when occupancy exceeds capacity or a registered bound changes.
    pub fn observe_queue(
        &mut self,
        queue: DiagnosticsQueue,
        current_items: usize,
        item_capacity: usize,
        current_bytes: usize,
        byte_capacity: usize,
    ) -> Result<(), DiagnosticsError> {
        self.queues[queue as usize].observe(
            current_items,
            item_capacity,
            current_bytes,
            byte_capacity,
        )
    }

    /// Observes bounded approximate runtime memory.
    ///
    /// # Errors
    ///
    /// Returns an error if usage exceeds or changes its configured bound.
    pub fn observe_runtime_memory(
        &mut self,
        current_bytes: usize,
        bound_bytes: usize,
    ) -> Result<(), DiagnosticsError> {
        self.memory.observe(current_bytes, bound_bytes)
    }

    /// Records one local processing sample when detailed diagnostics are enabled.
    pub fn record_local_latency(&mut self, metric: LocalLatencyMetric, elapsed_nanos: u64) {
        if let Some(recorder) = &mut self.detailed_latency {
            recorder.record(metric, elapsed_nanos);
        }
    }

    /// Publishes at most one immutable snapshot per 250 ms.
    ///
    /// # Errors
    ///
    /// Returns an error for monotonic-clock regression.
    pub fn try_snapshot(
        &mut self,
        monotonic_nanos: u64,
        wall_clock_unix_nanos: i64,
    ) -> Result<Option<FeedDiagnosticsSnapshot>, DiagnosticsError> {
        if self
            .last_snapshot_monotonic_nanos
            .is_some_and(|last| monotonic_nanos < last)
        {
            return Err(DiagnosticsError::MonotonicClockRegressed);
        }
        if self.last_snapshot_monotonic_nanos.is_some_and(|last| {
            monotonic_nanos.saturating_sub(last) < MINIMUM_DIAGNOSTICS_SNAPSHOT_INTERVAL_NANOS
        }) {
            return Ok(None);
        }
        let rates = self.rate_snapshot(monotonic_nanos);
        let snapshot = FeedDiagnosticsSnapshot {
            identity: self.identity.clone(),
            connection_state: self.connection_state,
            session_generation: self.session_generation,
            uptime_nanos: age_since(self.session_started_monotonic_nanos, monotonic_nanos)?,
            reconnect_count: self.reconnect_count,
            recovery_reason: self.recovery_reason,
            heartbeat_age_nanos: optional_age_since(
                self.last_heartbeat_monotonic_nanos,
                monotonic_nanos,
            )?,
            last_message_age_nanos: optional_age_since(
                self.last_message_monotonic_nanos,
                monotonic_nanos,
            )?,
            provider_timestamp_age: self.last_provider_timestamp_unix_nanos.map(|timestamp| {
                ClockRelativeAge {
                    nanos: wall_clock_unix_nanos.saturating_sub(timestamp),
                    label: TimingLabel::ProviderClockRelativeAge,
                }
            }),
            counters: self.counters.into(),
            rates,
            queues: self.queues,
            history_state: self.history_state,
            order_book_state: self.order_book_state,
            memory: self.memory,
            diagnostics_owner_bytes: self.approximate_owner_bytes(),
            detailed_latency: self
                .detailed_latency
                .as_ref()
                .map_or([None; LocalLatencyMetric::COUNT], |recorder| {
                    recorder.snapshots()
                }),
        };
        self.last_snapshot_monotonic_nanos = Some(monotonic_nanos);
        self.rate_baseline = Some(RateBaseline {
            monotonic_nanos,
            counters: self.counters,
        });
        Ok(Some(snapshot))
    }

    #[must_use]
    pub fn approximate_owner_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(self.identity.allocated_bytes())
    }

    fn rate_snapshot(&self, monotonic_nanos: u64) -> FeedRateSnapshot {
        let Some(baseline) = self.rate_baseline else {
            return FeedRateSnapshot::default();
        };
        let elapsed = monotonic_nanos.saturating_sub(baseline.monotonic_nanos);
        if elapsed == 0 {
            return FeedRateSnapshot::default();
        }
        let rate = |counter: FeedCounter| {
            let delta =
                self.counters[counter as usize].saturating_sub(baseline.counters[counter as usize]);
            events_per_second_milli(delta, elapsed)
        };
        let depth = self.counters[FeedCounter::DepthSnapshots as usize]
            .saturating_add(self.counters[FeedCounter::DepthDeltas as usize])
            .saturating_sub(
                baseline.counters[FeedCounter::DepthSnapshots as usize]
                    .saturating_add(baseline.counters[FeedCounter::DepthDeltas as usize]),
            );
        FeedRateSnapshot {
            trades_per_second_milli: rate(FeedCounter::Trades),
            quotes_per_second_milli: rate(FeedCounter::Quotes),
            depth_updates_per_second_milli: events_per_second_milli(depth, elapsed),
            publications_per_second_milli: rate(FeedCounter::Publications),
        }
    }
}

fn age_since(start: Option<u64>, now: u64) -> Result<u64, DiagnosticsError> {
    optional_age_since(start, now).map(Option::unwrap_or_default)
}

fn optional_age_since(start: Option<u64>, now: u64) -> Result<Option<u64>, DiagnosticsError> {
    start
        .map(|value| {
            now.checked_sub(value)
                .ok_or(DiagnosticsError::MonotonicClockRegressed)
        })
        .transpose()
}

fn events_per_second_milli(count: u64, elapsed_nanos: u64) -> u64 {
    let scaled = u128::from(count).saturating_mul(1_000_000_000_000);
    u64::try_from(scaled / u128::from(elapsed_nanos)).unwrap_or(u64::MAX)
}

/// Redacted diagnostics validation failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticsError {
    EmptyIdentity(&'static str),
    IdentityTooLong { field: &'static str, maximum: usize },
    ControlCharacter(&'static str),
    ZeroCapacity,
    OccupancyExceedsCapacity,
    CapacityChanged,
    MemoryBoundExceeded { requested: usize, maximum: usize },
    GenerationDidNotAdvance,
    MonotonicClockRegressed,
}

impl fmt::Display for DiagnosticsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "feed diagnostics rejected: {self:?}")
    }
}

impl Error for DiagnosticsError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LatencyBoundary, LatencyTimestampChain};
    use std::num::NonZeroU64;

    fn identity() -> FeedIdentity {
        FeedIdentity::try_new("rithmic", "RITHMIC_TEST", "Test")
            .expect("diagnostics identity is valid")
    }

    fn diagnostics(detailed: bool) -> FeedDiagnostics {
        FeedDiagnostics::new(
            identity(),
            detailed.then_some(NonZeroU64::new(1_000_000).expect("nonzero")),
        )
    }

    #[test]
    fn identity_errors_never_echo_rejected_input() {
        let secret = "account-password\n";
        let error = FeedIdentity::try_new(secret, "system", "environment")
            .expect_err("control-bearing identity is rejected");
        let diagnostic = format!("{error:?} {error}");
        assert!(!diagnostic.contains(secret));
        assert_eq!(error, DiagnosticsError::ControlCharacter("provider"));
    }

    #[test]
    fn accepted_identity_discards_excess_input_capacity() {
        let mut provider = String::with_capacity(1_000_000);
        provider.push_str("rithmic");
        let retained = FeedIdentity::try_new(provider, "RITHMIC_TEST", "Test")
            .expect("short identity is accepted");
        assert_eq!(retained.provider.len(), "rithmic".len());
        assert_eq!(
            retained.allocated_bytes(),
            "rithmic".len() + "RITHMIC_TEST".len() + "Test".len()
        );
    }

    #[test]
    fn snapshots_are_rate_limited_to_four_hz_and_immutable() {
        let mut diagnostics = diagnostics(false);
        diagnostics.increment(FeedCounter::Trades);
        let first = diagnostics
            .try_snapshot(1_000_000_000, 2_000_000_000)
            .expect("first snapshot succeeds")
            .expect("first snapshot publishes");
        diagnostics.increment(FeedCounter::Trades);
        assert_eq!(
            diagnostics
                .try_snapshot(1_249_999_999, 2_249_999_999)
                .expect("suppressed snapshot succeeds"),
            None
        );
        let second = diagnostics
            .try_snapshot(1_250_000_000, 2_250_000_000)
            .expect("cadence boundary succeeds")
            .expect("cadence boundary publishes");
        assert_eq!(first.counters.trades, 1);
        assert_eq!(second.counters.trades, 2);
        assert_eq!(second.rates.trades_per_second_milli, 4_000);
    }

    #[test]
    fn counters_and_depth_rates_are_exact_and_saturating() {
        let mut diagnostics = diagnostics(false);
        diagnostics.counters[FeedCounter::Gaps as usize] = u64::MAX;
        diagnostics.increment(FeedCounter::Gaps);
        diagnostics
            .try_snapshot(1_000_000_000, 1_000_000_000)
            .expect("baseline snapshot succeeds");
        diagnostics.increment_by(FeedCounter::DepthSnapshots, 2);
        diagnostics.increment_by(FeedCounter::DepthDeltas, 3);
        diagnostics.increment_by(FeedCounter::Publications, 5);
        let snapshot = diagnostics
            .try_snapshot(1_500_000_000, 1_500_000_000)
            .expect("rate snapshot succeeds")
            .expect("rate snapshot publishes");
        assert_eq!(snapshot.counters.gaps, u64::MAX);
        assert_eq!(snapshot.rates.depth_updates_per_second_milli, 10_000);
        assert_eq!(snapshot.rates.publications_per_second_milli, 10_000);
    }

    #[test]
    fn queue_and_memory_high_water_values_never_exceed_bounds() {
        let mut diagnostics = diagnostics(false);
        diagnostics
            .observe_queue(DiagnosticsQueue::UiUpdate, 4, 8, 400, 800)
            .expect("initial queue observation succeeds");
        diagnostics
            .observe_queue(DiagnosticsQueue::UiUpdate, 2, 8, 200, 800)
            .expect("lower occupancy succeeds");
        diagnostics
            .observe_runtime_memory(4_096, 8_192)
            .expect("initial memory observation succeeds");
        diagnostics
            .observe_runtime_memory(2_048, 8_192)
            .expect("lower memory observation succeeds");
        let snapshot = diagnostics
            .try_snapshot(1, 1)
            .expect("snapshot succeeds")
            .expect("snapshot publishes");
        let queue = snapshot.queues[DiagnosticsQueue::UiUpdate as usize];
        assert_eq!(queue.current_items, 2);
        assert_eq!(queue.high_water_items, 4);
        assert_eq!(queue.high_water_bytes, 400);
        assert_eq!(snapshot.memory.current_bytes, 2_048);
        assert_eq!(snapshot.memory.high_water_bytes, 4_096);
        assert_eq!(
            diagnostics.observe_queue(DiagnosticsQueue::UiUpdate, 9, 8, 1, 800),
            Err(DiagnosticsError::OccupancyExceedsCapacity)
        );
        assert_eq!(
            diagnostics.observe_runtime_memory(8_193, 8_192),
            Err(DiagnosticsError::MemoryBoundExceeded {
                requested: 8_193,
                maximum: 8_192,
            })
        );
    }

    #[test]
    fn registered_queue_and_memory_bounds_cannot_change_silently() {
        let mut diagnostics = diagnostics(false);
        diagnostics
            .observe_queue(DiagnosticsQueue::SemanticEvent, 1, 4, 10, 100)
            .expect("queue registers");
        diagnostics
            .observe_runtime_memory(10, 100)
            .expect("memory registers");
        assert_eq!(
            diagnostics.observe_queue(DiagnosticsQueue::SemanticEvent, 1, 5, 10, 100),
            Err(DiagnosticsError::CapacityChanged)
        );
        assert_eq!(
            diagnostics.observe_runtime_memory(10, 101),
            Err(DiagnosticsError::CapacityChanged)
        );
    }

    #[test]
    fn generation_ages_and_reconnects_are_fenced() {
        let mut diagnostics = diagnostics(false);
        let first = NonZeroU64::MIN;
        diagnostics
            .begin_session(first, 1_000)
            .expect("first generation begins");
        diagnostics.record_heartbeat(1_100);
        diagnostics.record_message(1_200, Some(10_000));
        diagnostics.set_connection_state(FeedConnectionState::Streaming);
        let snapshot = diagnostics
            .try_snapshot(1_500, 11_500)
            .expect("snapshot succeeds")
            .expect("snapshot publishes");
        assert_eq!(snapshot.uptime_nanos, 500);
        assert_eq!(snapshot.heartbeat_age_nanos, Some(400));
        assert_eq!(snapshot.last_message_age_nanos, Some(300));
        assert_eq!(
            snapshot.provider_timestamp_age,
            Some(ClockRelativeAge {
                nanos: 1_500,
                label: TimingLabel::ProviderClockRelativeAge,
            })
        );
        assert_eq!(
            diagnostics.begin_session(first, 2_000),
            Err(DiagnosticsError::GenerationDidNotAdvance)
        );
        diagnostics
            .begin_session(NonZeroU64::new(2).expect("nonzero"), 2_000)
            .expect("new generation begins");
        assert_eq!(diagnostics.reconnect_count, 1);
        assert_eq!(diagnostics.last_message_monotonic_nanos, None);
        assert_eq!(diagnostics.history_state, HistoryDiagnosticsState::Idle);
        assert_eq!(
            diagnostics.order_book_state,
            OrderBookDiagnosticsState::AwaitingSnapshot
        );
    }

    #[test]
    fn provider_clock_age_can_be_negative_without_becoming_network_latency() {
        let mut diagnostics = diagnostics(false);
        diagnostics.record_message(100, Some(2_000));
        let snapshot = diagnostics
            .try_snapshot(100, 1_500)
            .expect("snapshot succeeds")
            .expect("snapshot publishes");
        assert_eq!(
            snapshot.provider_timestamp_age,
            Some(ClockRelativeAge {
                nanos: -500,
                label: TimingLabel::ProviderClockRelativeAge,
            })
        );
    }

    #[test]
    fn detailed_histograms_are_opt_in_bounded_and_labelled() {
        let mut enabled = diagnostics(true);
        for sample in [1, 2, 3, 4, 5, 1_000_001] {
            enabled.record_local_latency(LocalLatencyMetric::SocketReadToDecode, sample);
        }
        let snapshot = enabled
            .try_snapshot(1, 1)
            .expect("snapshot succeeds")
            .expect("snapshot publishes");
        let histogram = snapshot.detailed_latency[LocalLatencyMetric::SocketReadToDecode as usize]
            .expect("enabled histogram publishes");
        assert_eq!(histogram.sample_count, 5);
        assert_eq!(histogram.rejected_samples, 1);
        assert_eq!(histogram.maximum_nanos, 5);
        assert_eq!(histogram.p50_upper_bound_nanos, 3);
        assert_eq!(histogram.p99_9_upper_bound_nanos, 7);
        assert_eq!(histogram.label, TimingLabel::LocalProcessingLatency);

        let mut disabled = diagnostics(false);
        disabled.record_local_latency(LocalLatencyMetric::SocketReadToDecode, 1);
        let disabled_snapshot = disabled
            .try_snapshot(1, 1)
            .expect("disabled snapshot succeeds")
            .expect("disabled snapshot publishes");
        assert_eq!(
            disabled_snapshot.detailed_latency,
            [None; LocalLatencyMetric::COUNT]
        );

        let mut rejected_only = diagnostics(true);
        rejected_only.record_local_latency(LocalLatencyMetric::FrameSubmitToPresent, 1_000_001);
        let rejected_snapshot = rejected_only
            .try_snapshot(1, 1)
            .expect("rejected-only snapshot succeeds")
            .expect("rejected-only snapshot publishes");
        let rejected = rejected_snapshot.detailed_latency
            [LocalLatencyMetric::FrameSubmitToPresent as usize]
            .expect("rejections remain visible");
        assert_eq!(rejected.sample_count, 0);
        assert_eq!(rejected.rejected_samples, 1);
    }

    #[test]
    fn timestamp_chains_feed_only_valid_local_intervals() {
        let mut diagnostics = diagnostics(true);
        let mut chain = LatencyTimestampChain::new();
        chain.set(LatencyBoundary::CanonicalAccept, 1_000);
        chain.set(LatencyBoundary::ModelPublish, 1_250);
        diagnostics
            .record_latency_chain(LocalLatencyMetric::CanonicalAcceptToModelPublish, &chain)
            .expect("ordered local timestamps record");
        let missing = LatencyTimestampChain::new();
        assert!(
            diagnostics
                .record_latency_chain(LocalLatencyMetric::CanonicalAcceptToModelPublish, &missing,)
                .is_err()
        );
        let snapshot = diagnostics
            .try_snapshot(1, 1)
            .expect("snapshot succeeds")
            .expect("snapshot publishes");
        let histogram = snapshot.detailed_latency
            [LocalLatencyMetric::CanonicalAcceptToModelPublish as usize]
            .expect("valid sample publishes");
        assert_eq!(histogram.sample_count, 1);
        assert_eq!(histogram.maximum_nanos, 250);
    }

    #[test]
    fn recovery_and_component_states_are_explicit() {
        let mut diagnostics = diagnostics(false);
        diagnostics.set_history_state(HistoryDiagnosticsState::Recovering);
        diagnostics.set_order_book_state(OrderBookDiagnosticsState::Recovering);
        diagnostics.require_recovery(FeedRecoveryReason::SequenceGap);
        let snapshot = diagnostics
            .try_snapshot(1, 1)
            .expect("snapshot succeeds")
            .expect("snapshot publishes");
        assert_eq!(snapshot.connection_state, FeedConnectionState::Recovering);
        assert_eq!(
            snapshot.recovery_reason,
            Some(FeedRecoveryReason::SequenceGap)
        );
        assert_eq!(snapshot.history_state, HistoryDiagnosticsState::Recovering);
        assert_eq!(
            snapshot.order_book_state,
            OrderBookDiagnosticsState::Recovering
        );
    }

    #[test]
    fn monotonic_regressions_fail_without_publishing() {
        let mut diagnostics = diagnostics(false);
        diagnostics
            .begin_session(NonZeroU64::MIN, 1_000)
            .expect("session begins");
        assert_eq!(
            diagnostics.try_snapshot(999, 1_000),
            Err(DiagnosticsError::MonotonicClockRegressed)
        );
        diagnostics
            .try_snapshot(1_000, 1_000)
            .expect("snapshot succeeds");
        assert_eq!(
            diagnostics.try_snapshot(999, 1_000),
            Err(DiagnosticsError::MonotonicClockRegressed)
        );
    }

    #[test]
    fn owner_memory_is_fixed_after_identity_construction() {
        let diagnostics = diagnostics(false);
        let expected = size_of::<FeedDiagnostics>()
            + diagnostics.identity.provider.len()
            + diagnostics.identity.system.len()
            + diagnostics.identity.environment.len();
        assert_eq!(diagnostics.approximate_owner_bytes(), expected);
    }
}
