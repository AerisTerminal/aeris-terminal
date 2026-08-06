//! Bounded low-overhead latency vocabulary and deterministic reports.

mod feed_diagnostics;
mod overhead;

pub use feed_diagnostics::{
    ClockRelativeAge, DiagnosticsError, DiagnosticsQueue, FeedConnectionState, FeedCounter,
    FeedCounterSnapshot, FeedDiagnostics, FeedDiagnosticsSnapshot, FeedIdentity, FeedRateSnapshot,
    FeedRecoveryReason, HistoryDiagnosticsState, LatencyHistogramSnapshot, LocalLatencyMetric,
    MAXIMUM_DIAGNOSTICS_IDENTITY_BYTES, MINIMUM_DIAGNOSTICS_SNAPSHOT_INTERVAL_NANOS,
    MemoryDiagnostics, OrderBookDiagnosticsState, QueueDiagnostics, TimingLabel,
};
pub use overhead::{
    DiagnosticsBenchmarkArm, DiagnosticsBenchmarkContext, DiagnosticsOverheadAssessment,
    DiagnosticsOverheadError, DiagnosticsOverheadEvidence, MAXIMUM_P99_9_REGRESSION_BASIS_POINTS,
    MAXIMUM_P99_REGRESSION_BASIS_POINTS,
};

use core::fmt;
use std::error::Error;
use std::num::NonZeroUsize;

/// Ordered timestamp boundaries for market-data delivery.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(usize)]
pub enum LatencyBoundary {
    Exchange,
    ProviderReceive,
    NicReceive,
    SocketRead,
    AxiusflowReceive,
    Decode,
    Normalized,
    CanonicalAccept,
    FanoutEnqueue,
    GatewaySend,
    ClientReceive,
    ModelApply,
    ModelPublish,
    UiEnqueue,
    FrameSubmit,
    FrameCallback,
    Present,
}

impl LatencyBoundary {
    pub const COUNT: usize = 17;
    pub const ALL: [Self; Self::COUNT] = [
        Self::Exchange,
        Self::ProviderReceive,
        Self::NicReceive,
        Self::SocketRead,
        Self::AxiusflowReceive,
        Self::Decode,
        Self::Normalized,
        Self::CanonicalAccept,
        Self::FanoutEnqueue,
        Self::GatewaySend,
        Self::ClientReceive,
        Self::ModelApply,
        Self::ModelPublish,
        Self::UiEnqueue,
        Self::FrameSubmit,
        Self::FrameCallback,
        Self::Present,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Exchange => "exchange_timestamp",
            Self::ProviderReceive => "provider_receive_timestamp",
            Self::NicReceive => "nic_receive_timestamp",
            Self::SocketRead => "socket_read_timestamp",
            Self::AxiusflowReceive => "axiusflow_receive_timestamp",
            Self::Decode => "decode_timestamp",
            Self::Normalized => "normalized_timestamp",
            Self::CanonicalAccept => "canonical_accept_timestamp",
            Self::FanoutEnqueue => "fanout_enqueue_timestamp",
            Self::GatewaySend => "gateway_send_timestamp",
            Self::ClientReceive => "client_receive_timestamp",
            Self::ModelApply => "model_apply_timestamp",
            Self::ModelPublish => "model_publish_timestamp",
            Self::UiEnqueue => "ui_enqueue_timestamp",
            Self::FrameSubmit => "frame_submit_timestamp",
            Self::FrameCallback => "post_render_frame_callback_timestamp",
            Self::Present => "present_timestamp_if_measurable",
        }
    }
}

/// Fixed-size timestamp chain for one event. Missing boundaries remain explicit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LatencyTimestampChain {
    timestamps: [Option<i64>; LatencyBoundary::COUNT],
}

impl LatencyTimestampChain {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            timestamps: [None; LatencyBoundary::COUNT],
        }
    }

    /// Records or replaces one named timestamp without allocation.
    pub fn set(&mut self, boundary: LatencyBoundary, timestamp_nanos: i64) {
        self.timestamps[boundary as usize] = Some(timestamp_nanos);
    }

    #[must_use]
    pub const fn get(&self, boundary: LatencyBoundary) -> Option<i64> {
        self.timestamps[boundary as usize]
    }

    /// Constructs one interval only when both named boundaries are present.
    ///
    /// # Errors
    ///
    /// Returns a missing-boundary, reversed-boundary, or timestamp-regression error.
    pub fn try_sample(
        &self,
        start: LatencyBoundary,
        end: LatencyBoundary,
    ) -> Result<LatencySample, LatencyError> {
        let start_nanos = self
            .get(start)
            .ok_or(LatencyError::MissingTimestamp(start))?;
        let end_nanos = self.get(end).ok_or(LatencyError::MissingTimestamp(end))?;
        LatencySample::try_new(start, end, start_nanos, end_nanos)
    }
}

impl Default for LatencyTimestampChain {
    fn default() -> Self {
        Self::new()
    }
}

/// One measured interval with named boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LatencySample {
    pub start: LatencyBoundary,
    pub end: LatencyBoundary,
    pub elapsed_nanos: u64,
}

impl LatencySample {
    /// Creates a non-negative, forward boundary sample.
    ///
    /// # Errors
    ///
    /// Returns an error for reversed boundaries or timestamps.
    pub fn try_new(
        start: LatencyBoundary,
        end: LatencyBoundary,
        start_nanos: i64,
        end_nanos: i64,
    ) -> Result<Self, LatencyError> {
        if end <= start {
            return Err(LatencyError::InvalidBoundaryOrder { start, end });
        }
        let elapsed =
            end_nanos
                .checked_sub(start_nanos)
                .ok_or(LatencyError::TimestampRegression {
                    start_nanos,
                    end_nanos,
                })?;
        let elapsed_nanos =
            u64::try_from(elapsed).map_err(|_| LatencyError::TimestampRegression {
                start_nanos,
                end_nanos,
            })?;
        Ok(Self {
            start,
            end,
            elapsed_nanos,
        })
    }
}

impl FeedDiagnostics {
    /// Records one validated timestamp-chain interval into an opt-in local histogram.
    ///
    /// Disabled detailed diagnostics still validate the timestamp chain but retain
    /// no sample. Provider-clock-relative age is intentionally not accepted here.
    ///
    /// # Errors
    ///
    /// Returns a missing-boundary, reversed-boundary, or timestamp-regression error.
    pub fn record_latency_chain(
        &mut self,
        metric: LocalLatencyMetric,
        chain: &LatencyTimestampChain,
    ) -> Result<(), LatencyError> {
        let (start, end) = metric.boundaries();
        let sample = chain.try_sample(start, end)?;
        self.record_local_latency(metric, sample.elapsed_nanos);
        Ok(())
    }
}

impl LocalLatencyMetric {
    /// Returns the only timestamp boundary pair valid for this published label.
    #[must_use]
    pub const fn boundaries(self) -> (LatencyBoundary, LatencyBoundary) {
        match self {
            Self::SocketReadToDecode => (LatencyBoundary::SocketRead, LatencyBoundary::Decode),
            Self::DecodeToCanonicalAccept => {
                (LatencyBoundary::Decode, LatencyBoundary::CanonicalAccept)
            }
            Self::CanonicalAcceptToModelPublish => (
                LatencyBoundary::CanonicalAccept,
                LatencyBoundary::ModelPublish,
            ),
            Self::ModelPublishToUiEnqueue => {
                (LatencyBoundary::ModelPublish, LatencyBoundary::UiEnqueue)
            }
            Self::UiEnqueueToFrameSubmit => {
                (LatencyBoundary::UiEnqueue, LatencyBoundary::FrameSubmit)
            }
            Self::FrameSubmitToFrameCallback => {
                (LatencyBoundary::FrameSubmit, LatencyBoundary::FrameCallback)
            }
            Self::FrameSubmitToPresent => (LatencyBoundary::FrameSubmit, LatencyBoundary::Present),
        }
    }
}

/// Required percentile report for one named boundary pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LatencyReport {
    pub start: LatencyBoundary,
    pub end: LatencyBoundary,
    pub p50_nanos: u64,
    pub p95_nanos: u64,
    pub p99_nanos: u64,
    pub p99_9_nanos: u64,
    pub maximum_nanos: u64,
    pub sample_count: usize,
}

/// A bounded recorder. Overflow is explicit instead of allocating without limit.
#[derive(Debug)]
pub struct BoundedLatencyRecorder {
    capacity: NonZeroUsize,
    samples: Vec<LatencySample>,
    rejected_samples: u64,
}

impl BoundedLatencyRecorder {
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            samples: Vec::with_capacity(capacity.get()),
            rejected_samples: 0,
        }
    }

    /// Records a sample if capacity remains.
    ///
    /// # Errors
    ///
    /// Returns the sample unchanged when the bounded recorder is full.
    pub fn try_record(&mut self, sample: LatencySample) -> Result<(), LatencySample> {
        if self.samples.len() == self.capacity.get() {
            self.rejected_samples = self.rejected_samples.saturating_add(1);
            return Err(sample);
        }
        self.samples.push(sample);
        Ok(())
    }

    #[must_use]
    pub const fn rejected_samples(&self) -> u64 {
        self.rejected_samples
    }

    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// Records one named interval from a fixed-size timestamp chain.
    ///
    /// # Errors
    ///
    /// Returns a chain validation error or the unchanged sample when capacity is full.
    pub fn try_record_chain(
        &mut self,
        chain: &LatencyTimestampChain,
        start: LatencyBoundary,
        end: LatencyBoundary,
    ) -> Result<(), LatencyRecordError> {
        let sample = chain
            .try_sample(start, end)
            .map_err(LatencyRecordError::InvalidSample)?;
        self.try_record(sample)
            .map_err(LatencyRecordError::CapacityExceeded)
    }

    /// Builds a deterministic nearest-rank report for one boundary pair.
    ///
    /// # Errors
    ///
    /// Returns an error when no matching samples exist.
    pub fn report(
        &self,
        start: LatencyBoundary,
        end: LatencyBoundary,
    ) -> Result<LatencyReport, LatencyError> {
        let mut values: Vec<_> = self
            .samples
            .iter()
            .filter(|sample| sample.start == start && sample.end == end)
            .map(|sample| sample.elapsed_nanos)
            .collect();
        if values.is_empty() {
            return Err(LatencyError::NoSamples { start, end });
        }
        values.sort_unstable();
        Ok(LatencyReport {
            start,
            end,
            p50_nanos: percentile(&values, 500),
            p95_nanos: percentile(&values, 950),
            p99_nanos: percentile(&values, 990),
            p99_9_nanos: percentile(&values, 999),
            maximum_nanos: *values.last().unwrap_or(&0),
            sample_count: values.len(),
        })
    }
}

fn percentile(sorted: &[u64], permille: usize) -> u64 {
    let rank = sorted.len().saturating_mul(permille).saturating_add(999) / 1_000;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LatencyRecordError {
    InvalidSample(LatencyError),
    CapacityExceeded(LatencySample),
}

impl fmt::Display for LatencyRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "latency record rejected: {self:?}")
    }
}

impl Error for LatencyRecordError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidSample(error) => Some(error),
            Self::CapacityExceeded(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LatencyError {
    MissingTimestamp(LatencyBoundary),
    InvalidBoundaryOrder {
        start: LatencyBoundary,
        end: LatencyBoundary,
    },
    TimestampRegression {
        start_nanos: i64,
        end_nanos: i64,
    },
    NoSamples {
        start: LatencyBoundary,
        end: LatencyBoundary,
    },
}

impl fmt::Display for LatencyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid latency sample: {self:?}")
    }
}

impl Error for LatencyError {}
