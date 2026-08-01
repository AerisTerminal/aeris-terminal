//! Canonical direct-fanout evidence through market-bar wire consumption.
//!
//! This module owns wire-level canonical delta delivery, publication timestamp
//! validation, invalid payload rejection, and direct-branch overflow recovery. It reuses
//! the shared canonical market-bar fixtures, the shared bounded queue policy, and the
//! shared loopback session fixture.

use crate::binary_fixture::{
    BINARY_FIXTURE_SUBSCRIPTION_ID, BinaryMarketStreamFixture, binary_fixture_decoder,
    binary_market_stream_fixture,
};
use crate::canonical_market_bar::{
    accept_for_fanout, canonical_enqueue_time, canonical_market_bar_event,
    canonical_market_bar_snapshot, canonical_market_bar_snapshot_prefix,
};
use crate::harness_error::{
    ConformanceHarnessError, market_stream_error, realtime_error, websocket_error,
};
use crate::loopback_fixture::{websocket_fixture_config, websocket_fixture_session};
use crate::realtime_recovery::queue_policy;
use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarReplayPort, ReplayProvenance,
    ResnapshotReason,
};
use axiusflow_market_protocol_adapter::{
    CanonicalMarketBarProjectionError, encode_market_bar_stream_frame,
    try_encode_canonical_market_bar_delta_envelope, try_encode_replay_snapshot_envelope,
    try_project_canonical_market_bar, try_project_canonical_market_bar_snapshot,
};
use axiusflow_protocols::protobuf::axiusflow::market::v1::market_bar_stream_envelope;
use axiusflow_realtime::{
    BoundedEventBranch, CanonicalMarketEvent, CanonicalSnapshot, DirectDurableFanout,
    FencedPartition, PartitionAcceptance, PartitionOwner, QueueOutcome, RealtimeError,
};
use axiusflow_stream_websocket_adapter::{
    MarketWebSocketPublication, MarketWebSocketSession, WebSocketMessageOutcome,
};
use std::num::NonZeroUsize;
use tungstenite::Message;

/// Headless evidence for canonical direct fanout through market-bar wire consumption.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectMarketBarWireConformance {
    passed_checks: u8,
}

impl DirectMarketBarWireConformance {
    const REQUIRED_CHECKS: u8 = u8::MAX;

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }
}

/// Exercises canonical ordered fanout, strict projection, framing, client consumption,
/// epoch handoff, and direct-branch overflow recovery using deterministic fixtures only.
///
/// This does not exercise or claim Redpanda, object storage, TLS, authentication,
/// entitlement enforcement, provider connectivity, WAN behavior, desktop wiring,
/// evented production operation, hardware timestamps, or production readiness.
///
/// # Errors
///
/// Returns an error when any valid canonical, queue, projection, or client transition fails.
#[allow(clippy::too_many_lines)]
pub fn run_direct_market_bar_wire_conformance()
-> Result<DirectMarketBarWireConformance, ConformanceHarnessError> {
    let fixture = binary_market_stream_fixture()?;
    let source = EmbeddedReplaySource;
    let source_two = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
        .map_err(market_stream_error)?;
    let source_four = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 4 })
        .map_err(market_stream_error)?;
    let source_five = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 5 })
        .map_err(market_stream_error)?;

    let initial = canonical_market_bar_snapshot(&source_two, 1, 1)?;
    let invalid_payload_rejected =
        invalid_canonical_bar_payload_is_rejected(&initial, &source_two)?
            && wire_publication_timestamp_mismatch_is_rejected(&initial, &source_two, &fixture)?;
    let mut session = websocket_fixture_session(&fixture, websocket_fixture_config(&fixture, 8)?)?;
    install_canonical_snapshot(&mut session, &fixture, &initial, &source_two)?;
    let initial_generation = session.current_generation().is_some_and(|generation| {
        generation.ownership_epoch() == 1
            && generation.generation() == 1
            && generation.sequence_range() == (1, 2)
    });
    let _ = session.drain_publications(NonZeroUsize::MIN);

    let owner = PartitionOwner::try_new(7, "wire_fixture_owner_1", 1).map_err(realtime_error)?;
    let mut partition = FencedPartition::new(owner);
    let fence_one = partition.publication_fence();
    partition
        .install_snapshot(&fence_one, &initial)
        .map_err(realtime_error)?;
    let direct =
        BoundedEventBranch::try_new(queue_policy("wire_direct", 2)).map_err(realtime_error)?;
    let durable =
        BoundedEventBranch::try_new(queue_policy("wire_durable", 1)).map_err(realtime_error)?;
    let mut fanout =
        DirectDurableFanout::try_new(fence_one.clone(), direct, durable).map_err(realtime_error)?;

    let fanout_timestamp_bound = fanout_timestamp_mismatch_is_rejected(&initial, &source_four)?;
    let event_three = canonical_market_bar_event(&source_four.bars()[2], &source_four, 1)?;
    let accepted_three = accept_for_fanout(&mut partition, &fence_one, event_three)?;
    fanout
        .publish(
            &mut partition,
            &fence_one,
            accepted_three,
            canonical_enqueue_time(&source_four, 2),
        )
        .map_err(realtime_error)?;
    let (direct_three, _) = fanout
        .pop_direct(canonical_enqueue_time(&source_four, 2).saturating_add(10))
        .ok_or_else(|| {
            ConformanceHarnessError::Realtime("missing direct sequence 3".to_string())
        })?;
    let publication_three = deliver_canonical_delta(
        &mut session,
        &fixture,
        &source_four,
        2,
        direct_three.event(),
    )?;
    let direct_delivery = fanout_timestamp_bound
        && initial_generation
        && publication_three.generation.generation() == 2
        && publication_three.generation.sequence_range() == (1, 3);

    let event_four = canonical_market_bar_event(&source_four.bars()[3], &source_four, 1)?;
    let accepted_four = accept_for_fanout(&mut partition, &fence_one, event_four)?;
    let outcomes = fanout
        .publish(
            &mut partition,
            &fence_one,
            accepted_four,
            canonical_enqueue_time(&source_four, 3),
        )
        .map_err(realtime_error)?;
    let branch_independence = outcomes.direct == QueueOutcome::Enqueued
        && outcomes.durable == QueueOutcome::SnapshotRequired
        && fanout
            .durable_metrics(canonical_enqueue_time(&source_four, 3))
            .snapshot_required;
    let (direct_four, _) = fanout
        .pop_direct(canonical_enqueue_time(&source_four, 3).saturating_add(10))
        .ok_or_else(|| {
            ConformanceHarnessError::Realtime("missing direct sequence 4".to_string())
        })?;
    let expected_fanout = direct_four
        .event()
        .header()
        .timestamps
        .fanout_enqueue_unix_nanos;
    let publication_four =
        deliver_canonical_delta(&mut session, &fixture, &source_four, 3, direct_four.event())?;
    let retained = publication_four
        .generation
        .items()
        .last()
        .map(axiusflow_protocols::Provenanced::provenance);
    let provenance_retained = retained.is_some_and(|provenance| {
        provenance.event_id == direct_four.event().header().event_id
            && provenance.source_sequence == 4
            && provenance.partition_id == 7
            && provenance.ownership_epoch == 1
            && provenance.fanout_enqueue_timestamp_unix_nanos == expected_fanout
            && provenance.nic_timestamp_source == 1
    }) && publication_four.generation.generation() == 3
        && publication_four.generation.sequence_range() == (1, 4);

    let stale_event = canonical_market_bar_event(&source_five.bars()[4], &source_five, 1)?;
    let stale_enqueue = canonical_enqueue_time(&source_five, 4);
    let accepted_before_handoff =
        accept_for_fanout(&mut partition, &fence_one, stale_event.clone())?;
    partition
        .handoff(PartitionOwner::try_new(7, "wire_fixture_owner_2", 2).map_err(realtime_error)?)
        .map_err(realtime_error)?;
    let fence_two = partition.publication_fence();
    let partition_stale_rejected = partition.accept(&fence_one, &stale_event).is_err();
    let split_window_rejected = fanout
        .publish(
            &mut partition,
            &fence_one,
            accepted_before_handoff,
            stale_enqueue,
        )
        .is_err()
        && fanout.direct_metrics(stale_enqueue).items == 0;

    let mut reconstructed_old_partition = FencedPartition::new(
        PartitionOwner::try_new(7, "wire_fixture_owner_1", 1).map_err(realtime_error)?,
    );
    let reconstructed_fence = reconstructed_old_partition.publication_fence();
    reconstructed_old_partition
        .install_snapshot(&reconstructed_fence, &initial)
        .map_err(realtime_error)?;
    let reconstructed_event = canonical_market_bar_event(&source_four.bars()[2], &source_four, 1)?;
    let reconstructed_token = accept_for_fanout(
        &mut reconstructed_old_partition,
        &reconstructed_fence,
        reconstructed_event,
    )?;
    let reconstructed_authority_rejected = fanout
        .publish(
            &mut reconstructed_old_partition,
            &reconstructed_fence,
            reconstructed_token,
            canonical_enqueue_time(&source_four, 2),
        )
        .is_err();
    fanout
        .activate_fence(fence_two.clone())
        .map_err(realtime_error)?;
    let stale_writer_fenced =
        partition_stale_rejected && split_window_rejected && reconstructed_authority_rejected;

    let blocked_event = canonical_market_bar_event(&source_five.bars()[4], &source_five, 2)?;
    let blocked_by_handoff = matches!(
        partition
            .accept_for_publication(&fence_two, blocked_event)
            .map_err(realtime_error)?,
        PartitionAcceptance::SnapshotRequired
    ) && fanout
        .pop_direct(canonical_enqueue_time(&source_five, 4))
        .is_none();

    let replacement = canonical_market_bar_snapshot(&source_four, 2, 2)?;
    partition
        .install_snapshot(&fence_two, &replacement)
        .map_err(realtime_error)?;
    fanout
        .recover_from_snapshot(&fence_two, &replacement)
        .map_err(realtime_error)?;
    session.require_resnapshot(ResnapshotReason::OwnershipHandoff);
    session.begin_reconnect();
    install_canonical_snapshot(&mut session, &fixture, &replacement, &source_four)?;
    let _ = session.drain_publications(NonZeroUsize::MIN);
    let event_five = canonical_market_bar_event(&source_five.bars()[4], &source_five, 2)?;
    let accepted_five = accept_for_fanout(&mut partition, &fence_two, event_five)?;
    fanout
        .publish(
            &mut partition,
            &fence_two,
            accepted_five,
            canonical_enqueue_time(&source_five, 4),
        )
        .map_err(realtime_error)?;
    let (direct_five, _) = fanout
        .pop_direct(canonical_enqueue_time(&source_five, 4).saturating_add(10))
        .ok_or_else(|| {
            ConformanceHarnessError::Realtime("missing epoch-2 direct delta".to_string())
        })?;
    let publication_five =
        deliver_canonical_delta(&mut session, &fixture, &source_five, 4, direct_five.event())?;
    let handoff_recovered = blocked_by_handoff
        && publication_five.generation.ownership_epoch() == 2
        && publication_five.generation.generation() == 3
        && publication_five.generation.sequence_range() == (1, 5);

    let (direct_overflow_latched, direct_overflow_recovered) =
        run_direct_branch_overflow_recovery(&fixture, &source_four, &source_five)?;
    let passed_checks = [
        invalid_payload_rejected,
        direct_delivery,
        branch_independence,
        provenance_retained,
        stale_writer_fenced,
        handoff_recovered,
        direct_overflow_latched,
        direct_overflow_recovered,
    ]
    .into_iter()
    .enumerate()
    .fold(0_u8, |checks, (index, passed)| {
        checks | (u8::from(passed) << index)
    });
    Ok(DirectMarketBarWireConformance { passed_checks })
}

fn invalid_canonical_bar_payload_is_rejected(
    snapshot: &CanonicalSnapshot,
    source: &axiusflow_application::ReplaySnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let first = snapshot.events().first().ok_or_else(|| {
        ConformanceHarnessError::MarketStream("missing canonical fixture event".to_string())
    })?;
    let malformed = CanonicalMarketEvent::try_new(first.header().clone(), &[0_u8; 8])
        .map_err(realtime_error)?;
    Ok(matches!(
        try_project_canonical_market_bar(&malformed, source.instrument(), source.bar_definition()),
        Err(CanonicalMarketBarProjectionError::InvalidPayloadLength { actual: 8 })
    ))
}

fn wire_publication_timestamp_mismatch_is_rejected(
    snapshot: &CanonicalSnapshot,
    source: &axiusflow_application::ReplaySnapshot,
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let projected = try_project_canonical_market_bar_snapshot(
        snapshot,
        source.instrument(),
        ReplayProvenance::EmbeddedFixture,
        source.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let mut envelope = try_encode_replay_snapshot_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        &projected,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    let Some(market_bar_stream_envelope::Update::Snapshot(wire_snapshot)) =
        envelope.update.as_mut()
    else {
        return Ok(false);
    };
    let Some(metadata) = wire_snapshot
        .bars
        .first_mut()
        .and_then(|bar| bar.header.as_mut())
        .and_then(|header| header.metadata.as_mut())
    else {
        return Ok(false);
    };
    metadata.publication_time_unix_nanos = metadata.publication_time_unix_nanos.saturating_add(1);
    let frame = encode_market_bar_stream_frame(&envelope, fixture.maximum_frame_bytes)
        .map_err(market_stream_error)?;
    let mut decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    Ok(decoder.push(&frame).is_err())
}

fn run_direct_branch_overflow_recovery(
    fixture: &BinaryMarketStreamFixture,
    source_four: &axiusflow_application::ReplaySnapshot,
    source_five: &axiusflow_application::ReplaySnapshot,
) -> Result<(bool, bool), ConformanceHarnessError> {
    let owner = PartitionOwner::try_new(7, "overflow_fixture_owner", 1).map_err(realtime_error)?;
    let mut partition = FencedPartition::new(owner);
    let fence = partition.publication_fence();
    let baseline = canonical_market_bar_snapshot_prefix(source_four, 1, 1, 2)?;
    partition
        .install_snapshot(&fence, &baseline)
        .map_err(realtime_error)?;
    let direct =
        BoundedEventBranch::try_new(queue_policy("overflow_direct", 1)).map_err(realtime_error)?;
    let durable =
        BoundedEventBranch::try_new(queue_policy("overflow_durable", 2)).map_err(realtime_error)?;
    let mut fanout =
        DirectDurableFanout::try_new(fence.clone(), direct, durable).map_err(realtime_error)?;
    for index in [2_usize, 3] {
        let event = canonical_market_bar_event(&source_four.bars()[index], source_four, 1)?;
        let accepted = accept_for_fanout(&mut partition, &fence, event)?;
        fanout
            .publish(
                &mut partition,
                &fence,
                accepted,
                canonical_enqueue_time(source_four, index),
            )
            .map_err(realtime_error)?;
    }
    let direct_overflow_latched = fanout
        .direct_metrics(canonical_enqueue_time(source_four, 3))
        .snapshot_required
        && fanout
            .pop_direct(canonical_enqueue_time(source_four, 3).saturating_add(10))
            .is_none();
    let recovery = canonical_market_bar_snapshot(source_four, 1, 2)?;
    partition
        .install_snapshot(&fence, &recovery)
        .map_err(realtime_error)?;
    fanout
        .recover_from_snapshot(&fence, &recovery)
        .map_err(realtime_error)?;
    let healthy_durable_preserved = [3_u64, 4].into_iter().all(|expected| {
        fanout
            .pop_durable(canonical_enqueue_time(source_four, 3).saturating_add(10))
            .is_some_and(|(event, _)| event.header().source_sequence == expected)
    });
    let obsolete_delta_cleared = fanout
        .pop_direct(canonical_enqueue_time(source_four, 3))
        .is_none();
    let mut session = websocket_fixture_session(fixture, websocket_fixture_config(fixture, 4)?)?;
    install_canonical_snapshot(&mut session, fixture, &recovery, source_four)?;
    let _ = session.drain_publications(NonZeroUsize::MIN);
    let event_five = canonical_market_bar_event(&source_five.bars()[4], source_five, 1)?;
    let accepted_five = accept_for_fanout(&mut partition, &fence, event_five)?;
    fanout
        .publish(
            &mut partition,
            &fence,
            accepted_five,
            canonical_enqueue_time(source_five, 4),
        )
        .map_err(realtime_error)?;
    let (resumed, _) = fanout
        .pop_direct(canonical_enqueue_time(source_five, 4).saturating_add(10))
        .ok_or_else(|| {
            ConformanceHarnessError::Realtime("missing resumed direct delta".to_string())
        })?;
    let publication =
        deliver_canonical_delta(&mut session, fixture, source_five, 4, resumed.event())?;
    Ok((
        direct_overflow_latched && healthy_durable_preserved,
        obsolete_delta_cleared
            && publication.generation.generation() == 3
            && publication.generation.sequence_range() == (1, 5),
    ))
}

fn install_canonical_snapshot(
    session: &mut MarketWebSocketSession,
    fixture: &BinaryMarketStreamFixture,
    snapshot: &CanonicalSnapshot,
    source: &axiusflow_application::ReplaySnapshot,
) -> Result<(), ConformanceHarnessError> {
    let projected = try_project_canonical_market_bar_snapshot(
        snapshot,
        source.instrument(),
        ReplayProvenance::EmbeddedFixture,
        source.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let envelope = try_encode_replay_snapshot_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        &projected,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    let frame = encode_market_bar_stream_frame(&envelope, fixture.maximum_frame_bytes)
        .map_err(market_stream_error)?;
    match session
        .accept_message(Message::binary(frame))
        .map_err(websocket_error)?
    {
        WebSocketMessageOutcome::BinaryAccepted { published: 1 } => Ok(()),
        outcome => Err(ConformanceHarnessError::WebSocket(format!(
            "canonical snapshot produced unexpected outcome {outcome:?}"
        ))),
    }
}

fn deliver_canonical_delta(
    session: &mut MarketWebSocketSession,
    fixture: &BinaryMarketStreamFixture,
    source: &axiusflow_application::ReplaySnapshot,
    previous_sequence: u64,
    event: &CanonicalMarketEvent,
) -> Result<MarketWebSocketPublication, ConformanceHarnessError> {
    let envelope = try_encode_canonical_market_bar_delta_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        source.instrument(),
        source.bar_definition(),
        previous_sequence,
        event,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    let frame = encode_market_bar_stream_frame(&envelope, fixture.maximum_frame_bytes)
        .map_err(market_stream_error)?;
    let outcome = session
        .accept_message(Message::binary(frame))
        .map_err(websocket_error)?;
    if outcome != (WebSocketMessageOutcome::BinaryAccepted { published: 1 }) {
        return Err(ConformanceHarnessError::WebSocket(format!(
            "canonical delta produced unexpected outcome {outcome:?}"
        )));
    }
    session
        .drain_publications(NonZeroUsize::MIN)
        .pop()
        .ok_or_else(|| {
            ConformanceHarnessError::WebSocket("missing canonical publication".to_string())
        })
}

fn fanout_timestamp_mismatch_is_rejected(
    initial: &CanonicalSnapshot,
    source: &axiusflow_application::ReplaySnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let owner = PartitionOwner::try_new(7, "timestamp_fixture_owner", 1).map_err(realtime_error)?;
    let mut partition = FencedPartition::new(owner);
    let fence = partition.publication_fence();
    partition
        .install_snapshot(&fence, initial)
        .map_err(realtime_error)?;
    let direct =
        BoundedEventBranch::try_new(queue_policy("timestamp_direct", 1)).map_err(realtime_error)?;
    let durable = BoundedEventBranch::try_new(queue_policy("timestamp_durable", 1))
        .map_err(realtime_error)?;
    let mut fanout =
        DirectDurableFanout::try_new(fence.clone(), direct, durable).map_err(realtime_error)?;
    let event = canonical_market_bar_event(&source.bars()[2], source, 1)?;
    let accepted = accept_for_fanout(&mut partition, &fence, event)?;
    let declared = canonical_enqueue_time(source, 2);
    let rejected = matches!(
        fanout.publish(&mut partition, &fence, accepted, declared.saturating_add(1),),
        Err(RealtimeError::FanoutEnqueueTimestampMismatch { .. })
    );
    let retried = canonical_market_bar_event(&source.bars()[2], source, 1)?;
    let partition_latched = matches!(
        partition
            .accept_for_publication(&fence, retried)
            .map_err(realtime_error)?,
        PartitionAcceptance::SnapshotRequired
    );
    let direct_metrics = fanout.direct_metrics(declared);
    let durable_metrics = fanout.durable_metrics(declared);
    Ok(rejected
        && partition_latched
        && direct_metrics.items == 0
        && direct_metrics.snapshot_required
        && durable_metrics.items == 0
        && durable_metrics.snapshot_required)
}
