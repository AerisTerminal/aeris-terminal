use axiusflow_desktop_storage::{
    AvailabilityReason, RecoveryAction, SegmentEncryptionKey, SegmentIdentity,
};
use axiusflow_provider_history::SequencedHistory;
use std::sync::Arc;

/// Stable local identifier for one chart consumer.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ChartId(pub u64);

/// Declared operating-system cache condition for one deterministic matrix case.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupCacheState {
    Cold,
    Warm,
}

/// Provider availability during local hydration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderConnectionState {
    Online,
    Offline,
}

/// Inputs for one exact visible-range hydration attempt.
#[derive(Clone, Copy, Debug)]
pub struct HydrationRequest<'a> {
    pub identity: &'a SegmentIdentity,
    pub encryption_key: &'a SegmentEncryptionKey,
    pub now_unix_seconds: i64,
    pub startup_cache_state: StartupCacheState,
    pub provider_state: ProviderConnectionState,
    pub missing_recovery: RecoveryAction,
}

/// Origin of one immutable chart publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheSource {
    LocalSegment,
    ProviderSnapshot,
    ProviderLive,
}

/// Atomically published history generation shared by chart consumers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryPublication<T> {
    pub generation: u64,
    pub watermark: u64,
    pub source: CacheSource,
    pub startup_cache_state: StartupCacheState,
    pub values: Vec<SequencedHistory<T>>,
}

/// Result of one visible-range hydration request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HydrationOutcome<T> {
    Ready {
        publication: Arc<HistoryPublication<T>>,
        memory_cache_hit: bool,
    },
    ProviderFetchRequired {
        reason: AvailabilityReason,
    },
    OfflineUnavailable {
        reason: AvailabilityReason,
        recovery: RecoveryAction,
    },
    LiveOnly {
        reason: AvailabilityReason,
    },
}

/// Bounded work counters suitable for redacted diagnostics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorkerMetrics {
    pub storage_writes: u64,
    pub storage_bytes_written: u64,
    pub storage_write_nanos: u64,
    pub storage_reads: u64,
    pub storage_bytes_read: u64,
    pub storage_read_nanos: u64,
    pub memory_cache_hits: u64,
    pub memory_cache_misses: u64,
    pub decode_operations: u64,
    pub decoded_bytes: u64,
    pub decode_nanos: u64,
    pub provider_snapshots: u64,
    pub snapshot_install_nanos: u64,
    pub live_items: u64,
    pub live_publish_nanos: u64,
    pub duplicate_live_items: u64,
}
