//! Canonical market events, fenced partition ownership, and bounded fanout contracts.

mod bounded_queue;
mod canonical_event;
mod errors;
mod fanout;
mod latest_state;
mod partition;
mod snapshot;
#[cfg(test)]
mod test_fixture;

pub use bounded_queue::{
    BoundedEventBranch, OverflowAction, QueueGap, QueueMetrics, QueueOutcome, QueuePolicy,
};
pub use canonical_event::{
    CanonicalEventHeader, CanonicalMarketEvent, CanonicalSeriesIdentity, CanonicalTimestamps,
    MAX_CANONICAL_PAYLOAD_BYTES, NicTimestampSource, SemanticClass,
};
pub use errors::RealtimeError;
pub use fanout::{DirectDurableFanout, FanoutOutcome};
pub use latest_state::{CanonicalLatestState, LatestStateSnapshotRequest};
pub use partition::{
    AcceptedCanonicalEvent, AcceptedDirectEvent, FencedPartition, PartitionAcceptance,
    PartitionDecision, PartitionOwner, PublicationFence,
};
pub use snapshot::{CanonicalSnapshot, SnapshotDescriptor};
