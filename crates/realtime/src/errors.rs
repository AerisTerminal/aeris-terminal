//! Contract rejection reasons for the real-time plane.

use core::fmt;
use std::error::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RealtimeError {
    EmptyIdentity(&'static str),
    ZeroSourceSequence,
    ZeroOwnershipEpoch,
    ZeroSnapshotGeneration,
    ZeroSchemaVersion,
    ZeroInstrumentRevision,
    ZeroSeriesDefinitionVersion,
    ZeroSeriesInterval,
    EmptySnapshot,
    SnapshotItemCountMismatch { expected: u64, actual: usize },
    SnapshotEventMismatch { sequence: u64, field: &'static str },
    PayloadLimitExceeded { requested: usize, maximum: usize },
    TimestampRegression { earlier: i64, later: i64 },
    PartitionMismatch { expected: u32, actual: u32 },
    OwnerMismatch,
    StaleOwnershipEpoch { active: u64, attempted: u64 },
    StalePublicationFence,
    FanoutEnqueueTimestampMismatch { declared: Option<i64>, actual: i64 },
    StaleSnapshotGeneration { active: u64, attempted: u64 },
    SnapshotSequenceRegression { active: u64, attempted: u64 },
    LatestStateSequenceRegression { active: u64, attempted: u64 },
    LatestStateSnapshotRequired,
    InvalidSnapshotRange { first: u64, last: u64 },
    SnapshotChecksumMismatch,
    SequenceOverflow,
    AlertThresholdExceedsCapacity,
    IncompatibleOverflowAction,
    SemanticClassMismatch,
    FanoutPolicyMismatch,
}

impl fmt::Display for RealtimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "real-time contract rejected input: {self:?}")
    }
}

impl Error for RealtimeError {}
