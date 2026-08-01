//! Fenced-partition recovery evidence for stale writers and bounded branch pressure.
//!
//! This module owns stale-writer rejection, partial direct/durable branch pressure
//! visibility, verified snapshot recovery, and snapshot checksum integrity for the
//! deterministic realtime fixtures. It also owns the shared bounded queue policy used by
//! the other partition/fanout scenarios.

use crate::canonical_market_bar::accept_for_fanout;
use crate::harness_error::ConformanceHarnessError;
use axiusflow_realtime::{
    BoundedEventBranch, CanonicalEventHeader, CanonicalMarketEvent, CanonicalSnapshot,
    CanonicalTimestamps, DirectDurableFanout, FanoutOutcome, FencedPartition, NicTimestampSource,
    OverflowAction, PartitionDecision, PartitionOwner, QueueOutcome, QueuePolicy, SemanticClass,
};
use std::num::{NonZeroU64, NonZeroUsize};

/// Result of deriving and verifying recovery snapshot integrity from immutable content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotIntegrityOutcome {
    CorruptionRejected,
}

/// Result proving stale-writer rejection, partial branch pressure visibility, and snapshot recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RealtimeRecoveryReport {
    pub stale_writer_rejected: bool,
    pub direct_outcome: QueueOutcome,
    pub durable_outcome: QueueOutcome,
    pub durable_gap_visible: bool,
    pub recovered: bool,
    pub snapshot_integrity: SnapshotIntegrityOutcome,
}

/// Exercises fencing, branch independence, gap telemetry, and checksum-bound recovery.
///
/// # Errors
///
/// Returns an error when any real-time contract rejects the valid fixture scenario.
pub fn run_realtime_recovery_conformance() -> Result<RealtimeRecoveryReport, ConformanceHarnessError>
{
    let owner = PartitionOwner::try_new(7, "fixture_owner_1", 1)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    let mut partition = FencedPartition::new(owner);
    let old_fence = partition.publication_fence();
    for sequence in [1, 2] {
        let event = fixture_canonical_event(sequence, 1, SemanticClass::OrderedDelta)?;
        if partition
            .accept(&old_fence, &event)
            .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?
            != PartitionDecision::Accepted
        {
            return Err(ConformanceHarnessError::Realtime(
                "contiguous event was not accepted".to_string(),
            ));
        }
    }

    partition
        .handoff(
            PartitionOwner::try_new(7, "fixture_owner_2", 2)
                .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?,
        )
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    let active_fence = partition.publication_fence();
    let stale_writer_rejected = partition
        .accept(
            &old_fence,
            &fixture_canonical_event(3, 1, SemanticClass::OrderedDelta)?,
        )
        .is_err();

    let initial_snapshot = fixture_snapshot(2, 1, 4)?;
    if !corrupted_snapshot_is_rejected(&initial_snapshot)? {
        return Err(ConformanceHarnessError::Realtime(
            "canonical snapshot corruption was accepted".to_string(),
        ));
    }
    let snapshot_integrity = SnapshotIntegrityOutcome::CorruptionRejected;
    partition
        .install_snapshot(&active_fence, &initial_snapshot)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;

    let direct = BoundedEventBranch::try_new(queue_policy("direct", 2))
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    let durable = BoundedEventBranch::try_new(queue_policy("durable", 1))
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    let mut fanout = DirectDurableFanout::try_new(active_fence.clone(), direct, durable)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;

    let event_5 = fixture_canonical_event(5, 2, SemanticClass::OrderedDelta)?;
    let accepted_5 = accept_for_fanout(&mut partition, &active_fence, event_5)?;
    fanout
        .publish(&mut partition, &active_fence, accepted_5, 5_200)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;

    let event_6 = fixture_canonical_event(6, 2, SemanticClass::OrderedDelta)?;
    let accepted_6 = accept_for_fanout(&mut partition, &active_fence, event_6)?;
    let FanoutOutcome {
        direct: direct_outcome,
        durable: durable_outcome,
    } = fanout
        .publish(&mut partition, &active_fence, accepted_6, 6_200)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    let durable_gap_visible = fanout.durable_metrics(12_000).last_gap.is_some();

    let recovery_snapshot = fixture_snapshot(2, 2, 6)?;
    partition
        .install_snapshot(&active_fence, &recovery_snapshot)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    fanout
        .recover_from_snapshot(&active_fence, &recovery_snapshot)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    let recovered = !fanout.durable_metrics(12_000).snapshot_required;

    Ok(RealtimeRecoveryReport {
        stale_writer_rejected,
        direct_outcome,
        durable_outcome,
        durable_gap_visible,
        recovered,
        snapshot_integrity,
    })
}

pub(crate) fn queue_policy(name: &str, item_capacity: usize) -> QueuePolicy {
    QueuePolicy {
        name: name.to_string(),
        producer: "fixture_partition".to_string(),
        consumer: format!("fixture_{name}_consumer"),
        item_capacity: NonZeroUsize::new(item_capacity).unwrap_or(NonZeroUsize::MIN),
        byte_capacity: NonZeroUsize::new(4_096).unwrap_or(NonZeroUsize::MIN),
        semantic_class: SemanticClass::OrderedDelta,
        overflow_action: OverflowAction::RequestSnapshot,
        maximum_residence_nanos: NonZeroU64::new(10_000).unwrap_or(NonZeroU64::MIN),
        recovery: "install_verified_snapshot".to_string(),
        alert_threshold_items: NonZeroUsize::MIN,
    }
}

fn corrupted_snapshot_is_rejected(
    snapshot: &CanonicalSnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let mut events = snapshot.events().to_vec();
    let mut payload = events[0].payload().to_vec();
    payload[0] ^= 0xff;
    events[0] = CanonicalMarketEvent::try_new(events[0].header().clone(), &payload)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))?;
    Ok(CanonicalSnapshot::try_from_parts(snapshot.descriptor().clone(), events).is_err())
}

fn fixture_snapshot(
    ownership_epoch: u64,
    generation: u64,
    last_sequence: u64,
) -> Result<CanonicalSnapshot, ConformanceHarnessError> {
    let events = (1..=last_sequence)
        .map(|sequence| {
            fixture_canonical_event(sequence, ownership_epoch, SemanticClass::OrderedDelta)
        })
        .collect::<Result<Vec<_>, _>>()?;
    CanonicalSnapshot::try_from_events(7, ownership_epoch, generation, 1, events)
        .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))
}

fn fixture_canonical_event(
    sequence: u64,
    ownership_epoch: u64,
    semantic_class: SemanticClass,
) -> Result<CanonicalMarketEvent, ConformanceHarnessError> {
    let receive = i64::try_from(sequence)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000);
    CanonicalMarketEvent::try_new(
        CanonicalEventHeader {
            event_id: format!("recovery_fixture_{ownership_epoch}_{sequence}"),
            event_time_unix_nanos: receive.saturating_sub(500),
            publication_time_unix_nanos: receive.saturating_add(300),
            producer: "fixture_recovery_decoder".to_string(),
            correlation_id: "fixture_recovery".to_string(),
            causation_id: String::new(),
            entitlement_revision: "fixture_entitlement_v1".to_string(),
            instrument_id: "instrument_fixture_axf_xnas".to_string(),
            venue_id: "xnas".to_string(),
            source_id: "fixture_provider".to_string(),
            series_identity: None,
            source_sequence: sequence,
            partition_id: 7,
            ownership_epoch,
            timestamps: CanonicalTimestamps {
                exchange_unix_nanos: receive.saturating_sub(500),
                provider_receive_unix_nanos: receive.saturating_sub(300),
                nic_receive_unix_nanos: Some(receive),
                axiusflow_receive_unix_nanos: receive,
                normalized_unix_nanos: receive.saturating_add(100),
                fanout_enqueue_unix_nanos: Some(receive.saturating_add(200)),
            },
            nic_timestamp_source: Some(NicTimestampSource::SocketSoftware),
            correction_flags: 0,
            quality_flags: 0,
            schema_version: 1,
            semantic_class,
        },
        &sequence.to_be_bytes(),
    )
    .map_err(|error| ConformanceHarnessError::Realtime(error.to_string()))
}
