//! Provider, series and session status the market runtime publishes in process.

/// Stable processing stage for actionable, redacted failures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum FailureStage {
    #[default]
    Unspecified = 0,
    ProviderHistory = 1,
    CanonicalValidation = 2,
    MemoryInstall = 3,
    Aggregation = 4,
    Handoff = 9,
    Publication = 10,
    ChartInstall = 12,
    ProviderRealtime = 13,
}

/// Machine-readable runtime fault classification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum EngineFaultCode {
    /// Transient failure.
    #[default]
    Retryable = 0,
    /// Engine is offline.
    Offline = 1,
    /// Operation was cancelled.
    Cancelled = 2,
    /// Unrecoverable failure.
    Permanent = 3,
    /// Persisted local state is corrupt.
    CorruptLocalState = 5,
    /// Credentials were rejected.
    Unauthenticated = 7,
}

/// Provider connection state published by the engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum ProviderConnectionState {
    /// No active provider connection.
    #[default]
    Disconnected = 0,
    /// A connection or authentication attempt is in progress.
    Connecting = 1,
    /// The provider is available for market work.
    Online = 2,
    /// Continuity is being re-established.
    Recovering = 3,
    /// The provider reached an explicit terminal failure.
    Failed = 4,
}

/// Authoritative readiness state for one consumer's current series demand.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum SeriesLoadState {
    /// No usable values are present yet.
    #[default]
    Empty = 0,
    /// Memory or provider work is in progress.
    Resolving = 1,
    /// A usable incomplete range is available.
    Partial = 2,
    /// A covering historical snapshot is available.
    Ready = 3,
    /// Historical state is ready and realtime is advancing.
    Live = 4,
    /// The demand reached an explicit failure.
    Failed = 5,
    /// A newer generation replaced this demand.
    Superseded = 6,
}

/// Provider-neutral phase of the dated market session for one instrument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketSessionPhase {
    Regular,
    PreMarket,
    PostMarket,
    Overnight,
    Closed,
    AlwaysOpen,
    Unknown,
}

/// Provenance of market hours; an unavailable calendar never implies 24/7 trading.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketSessionSource {
    ProviderCalendar,
    ProviderHours,
    Continuous,
    Unavailable,
}

/// One bounded dated status projection owned by the market runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketSessionStatus {
    pub instrument_id: String,
    pub phase: MarketSessionPhase,
    pub source: MarketSessionSource,
    pub session_start_unix_nanos: Option<i64>,
    pub session_end_unix_nanos: Option<i64>,
    pub next_open_unix_nanos: Option<i64>,
}

/// Runtime-owned provider health publication.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderState {
    pub provider: String,
    pub state: ProviderConnectionState,
    pub generation: u64,
    pub detail: Option<String>,
    pub transport_rtt_nanos: Option<u64>,
}

/// Redacted runtime fault.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fault {
    pub code: EngineFaultCode,
    pub redacted_detail: String,
}
