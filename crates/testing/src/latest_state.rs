//! Bounded partition-owned canonical latest-state projection evidence.
//!
//! This module owns latest-state installation, identity-change rejection, duplicate and
//! gap handling that mints no publication token, stale client snapshot behavior, and
//! atomic canonical snapshot chunk installation. It reuses the shared canonical
//! market-bar fixtures and the shared bounded queue policy.

use crate::binary_fixture::{
    BINARY_FIXTURE_SUBSCRIPTION_ID, BinaryMarketStreamFixture, binary_market_stream_fixture,
};
use crate::canonical_market_bar::{
    accept_publish_pop_direct, canonical_market_bar_event, canonical_market_bar_snapshot,
    publish_direct_to_latest,
};
use crate::harness_error::{
    ConformanceHarnessError, market_stream_error, realtime_error, websocket_error,
};
use crate::loopback_fixture::{websocket_fixture_config, websocket_fixture_session};
use crate::realtime_recovery::queue_policy;
use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarClientModel, MarketBarModelOutcome,
    MarketBarReplayPort, ReplayProvenance, ReplayStreamUpdate,
};
use axiusflow_market_protocol_adapter::{
    CanonicalMarketBarProjectionError, encode_market_bar_stream_frame,
    try_encode_replay_snapshot_chunk_envelopes, try_project_canonical_market_bar,
    try_project_canonical_market_bar_snapshot,
};
use axiusflow_realtime::{
    BoundedEventBranch, CanonicalLatestState, CanonicalMarketEvent, CanonicalSnapshot,
    DirectDurableFanout, FencedPartition, LatestStateSnapshotRequest, OverflowAction,
    PartitionAcceptance, PartitionDecision, PartitionOwner, RealtimeError, SemanticClass,
};
use axiusflow_stream_websocket_adapter::{MarketWebSocketSession, WebSocketMessageOutcome};
use std::num::NonZeroUsize;
use tungstenite::Message;

/// Headless evidence for one bounded partition-owned canonical latest-state projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LatestStateSnapshotConformance {
    passed_checks: u8,
}

impl LatestStateSnapshotConformance {
    const REQUIRED_CHECKS: u8 = 0b0011_1111;

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }
}

/// Exercises direct-event projection into bounded canonical latest state and atomic
/// chunked recovery consumption using deterministic in-memory fixtures only.
///
/// This does not provide or claim a network snapshot service, live provider,
/// durable broker/archive, TLS, authentication, entitlement enforcement, WAN,
/// desktop production wiring, hardware evidence, or production readiness.
///
/// # Errors
///
/// Returns an error when a valid partition, fanout, snapshot, or client transition fails.
#[allow(clippy::too_many_lines)]
pub fn run_latest_state_snapshot_conformance()
-> Result<LatestStateSnapshotConformance, ConformanceHarnessError> {
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
    let source_six = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 6 })
        .map_err(market_stream_error)?;
    let projection_identity_rejected =
        canonical_projection_identity_mismatches_are_rejected(&source_four)?;

    let owner = PartitionOwner::try_new(7, "latest_state_owner_1", 1).map_err(realtime_error)?;
    let mut partition = FencedPartition::new(owner);
    let fence_one = partition.publication_fence();
    let initial = canonical_market_bar_snapshot(&source_two, 1, 1)?;
    let mut latest = CanonicalLatestState::try_new(
        &mut partition,
        &fence_one,
        &initial,
        NonZeroUsize::new(3).unwrap_or(NonZeroUsize::MIN),
    )
    .map_err(realtime_error)?;
    let initial_served = latest
        .serve_snapshot(
            &partition,
            &fence_one,
            LatestStateSnapshotRequest::new(NonZeroUsize::MIN),
        )
        .map_err(realtime_error)?;
    let initial_installed = latest.generation() == 1
        && latest.retained_items() == 2
        && initial_served.descriptor().first_sequence.get() == 2
        && initial_served.descriptor().last_sequence.get() == 2;
    let rejected_acceptance_mints_no_token =
        duplicate_and_gap_do_not_mint_publication_tokens(&initial, &source_two, &source_four)?;
    let changed_event_rejected =
        latest_state_identity_changes_are_rejected(&initial, &source_four)?;

    let direct = BoundedEventBranch::try_new(queue_policy("latest_state_direct", 2))
        .map_err(realtime_error)?;
    let durable = BoundedEventBranch::try_new(queue_policy("latest_state_durable", 4))
        .map_err(realtime_error)?;
    let mut fanout =
        DirectDurableFanout::try_new(fence_one.clone(), direct, durable).map_err(realtime_error)?;
    let event_three = canonical_market_bar_event(&source_four.bars()[2], &source_four, 1)?;
    publish_direct_to_latest(
        &mut partition,
        &mut fanout,
        &mut latest,
        &fence_one,
        event_three,
    )?;
    let immutable_generation_two = latest
        .serve_snapshot(
            &partition,
            &fence_one,
            LatestStateSnapshotRequest::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN)),
        )
        .map_err(realtime_error)?;
    let event_four = canonical_market_bar_event(&source_four.bars()[3], &source_four, 1)?;
    publish_direct_to_latest(
        &mut partition,
        &mut fanout,
        &mut latest,
        &fence_one,
        event_four,
    )?;
    let generation_three = latest
        .serve_snapshot(
            &partition,
            &fence_one,
            LatestStateSnapshotRequest::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN)),
        )
        .map_err(realtime_error)?;
    let direct_advanced = immutable_generation_two.descriptor().generation.get() == 2
        && immutable_generation_two.descriptor().first_sequence.get() == 2
        && immutable_generation_two.descriptor().last_sequence.get() == 3
        && generation_three.descriptor().generation.get() == 3
        && generation_three.descriptor().first_sequence.get() == 3
        && generation_three.descriptor().last_sequence.get() == 4
        && latest.retained_items() == 3;

    let skipped_event = canonical_market_bar_event(&source_five.bars()[4], &source_five, 1)?;
    let _skipped_direct =
        accept_publish_pop_direct(&mut partition, &mut fanout, &fence_one, skipped_event)?;
    let gap_event = canonical_market_bar_event(&source_six.bars()[5], &source_six, 1)?;
    let gap_direct = accept_publish_pop_direct(&mut partition, &mut fanout, &fence_one, gap_event)?;
    let gap_latched = latest
        .apply_direct_event(&partition, &fence_one, gap_direct)
        .map_err(realtime_error)?
        == PartitionDecision::Gap {
            expected: 5,
            actual: 6,
        }
        && latest.requires_snapshot()
        && matches!(
            latest.serve_snapshot(
                &partition,
                &fence_one,
                LatestStateSnapshotRequest::new(NonZeroUsize::MIN),
            ),
            Err(RealtimeError::LatestStateSnapshotRequired)
        );

    let recovery = canonical_market_bar_snapshot(&source_six, 1, 4)?;
    latest
        .install_snapshot(&mut partition, &fence_one, &recovery)
        .map_err(realtime_error)?;
    fanout
        .recover_from_snapshot(&fence_one, &recovery)
        .map_err(realtime_error)?;
    let recovered = latest
        .serve_snapshot(
            &partition,
            &fence_one,
            LatestStateSnapshotRequest::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN)),
        )
        .map_err(realtime_error)?;
    let recovered_snapshot = !latest.requires_snapshot()
        && recovered.descriptor().generation.get() == 4
        && recovered.descriptor().first_sequence.get() == 5
        && recovered.descriptor().last_sequence.get() == 6;
    let chunked_installed =
        canonical_snapshot_chunks_install_atomically(&fixture, &source_six, &recovered)?;
    let stale_client_snapshot_preserved =
        stale_client_snapshots_preserve_generation(&source_five, &source_six, &recovery)?;

    partition
        .handoff(PartitionOwner::try_new(7, "latest_state_owner_2", 2).map_err(realtime_error)?)
        .map_err(realtime_error)?;
    let fence_two = partition.publication_fence();
    let handoff_fenced = latest
        .serve_snapshot(
            &partition,
            &fence_one,
            LatestStateSnapshotRequest::new(NonZeroUsize::MIN),
        )
        .is_err()
        && latest
            .serve_snapshot(
                &partition,
                &fence_two,
                LatestStateSnapshotRequest::new(NonZeroUsize::MIN),
            )
            .is_err();
    let replacement = canonical_market_bar_snapshot(&source_five, 2, 1)?;
    latest
        .install_snapshot(&mut partition, &fence_two, &replacement)
        .map_err(realtime_error)?;
    fanout
        .activate_fence(fence_two.clone())
        .map_err(realtime_error)?;
    fanout
        .recover_from_snapshot(&fence_two, &replacement)
        .map_err(realtime_error)?;
    let epoch_two_event = canonical_market_bar_event(&source_six.bars()[5], &source_six, 2)?;
    publish_direct_to_latest(
        &mut partition,
        &mut fanout,
        &mut latest,
        &fence_two,
        epoch_two_event,
    )?;
    let epoch_two = latest
        .serve_snapshot(
            &partition,
            &fence_two,
            LatestStateSnapshotRequest::new(NonZeroUsize::new(3).unwrap_or(NonZeroUsize::MIN)),
        )
        .map_err(realtime_error)?;
    let handoff_recovered = epoch_two.descriptor().ownership_epoch.get() == 2
        && epoch_two.descriptor().generation.get() == 2
        && epoch_two.descriptor().first_sequence.get() == 4
        && epoch_two.descriptor().last_sequence.get() == 6;

    let passed_checks = [
        initial_installed && projection_identity_rejected,
        direct_advanced && rejected_acceptance_mints_no_token,
        gap_latched && changed_event_rejected,
        recovered_snapshot,
        chunked_installed && stale_client_snapshot_preserved,
        handoff_fenced && handoff_recovered,
    ]
    .into_iter()
    .enumerate()
    .fold(0_u8, |checks, (index, passed)| {
        checks | (u8::from(passed) << index)
    });
    Ok(LatestStateSnapshotConformance { passed_checks })
}

fn canonical_projection_identity_mismatches_are_rejected(
    source: &axiusflow_application::ReplaySnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let event = canonical_market_bar_event(&source.bars()[2], source, 1)?;
    let mut wrong_instrument = source.instrument().clone();
    wrong_instrument.revision = wrong_instrument.revision.saturating_add(1);
    let revision_rejected = matches!(
        try_project_canonical_market_bar(&event, &wrong_instrument, source.bar_definition()),
        Err(CanonicalMarketBarProjectionError::SeriesIdentityMismatch(
            "instrument_revision"
        ))
    );
    let mut wrong_definition = source.bar_definition().clone();
    wrong_definition.version = wrong_definition.version.saturating_add(1);
    let definition_rejected = matches!(
        try_project_canonical_market_bar(&event, source.instrument(), &wrong_definition),
        Err(CanonicalMarketBarProjectionError::SeriesIdentityMismatch(
            "definition_version"
        ))
    );
    let mut missing_header = event.header().clone();
    missing_header.series_identity = None;
    let missing_identity =
        CanonicalMarketEvent::try_new(missing_header, event.payload()).map_err(realtime_error)?;
    let missing_rejected = matches!(
        try_project_canonical_market_bar(
            &missing_identity,
            source.instrument(),
            source.bar_definition(),
        ),
        Err(CanonicalMarketBarProjectionError::MissingSeriesIdentity)
    );
    Ok(revision_rejected && definition_rejected && missing_rejected)
}

fn duplicate_and_gap_do_not_mint_publication_tokens(
    initial: &CanonicalSnapshot,
    source_two: &axiusflow_application::ReplaySnapshot,
    source_four: &axiusflow_application::ReplaySnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let owner =
        PartitionOwner::try_new(7, "acceptance_negative_owner", 1).map_err(realtime_error)?;
    let mut partition = FencedPartition::new(owner);
    let fence = partition.publication_fence();
    partition
        .install_snapshot(&fence, initial)
        .map_err(realtime_error)?;
    let duplicate = canonical_market_bar_event(&source_two.bars()[1], source_two, 1)?;
    let duplicate_rejected = matches!(
        partition
            .accept_for_publication(&fence, duplicate)
            .map_err(realtime_error)?,
        PartitionAcceptance::Duplicate
    );
    let gap = canonical_market_bar_event(&source_four.bars()[3], source_four, 1)?;
    let gap_rejected = matches!(
        partition
            .accept_for_publication(&fence, gap)
            .map_err(realtime_error)?,
        PartitionAcceptance::Gap {
            expected: 3,
            actual: 4,
        }
    );
    Ok(duplicate_rejected && gap_rejected)
}

fn latest_state_identity_changes_are_rejected(
    initial: &CanonicalSnapshot,
    source: &axiusflow_application::ReplaySnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let base = canonical_market_bar_event(&source.bars()[2], source, 1)?;

    let mut series_header = base.header().clone();
    let series = series_header.series_identity.as_mut().ok_or_else(|| {
        ConformanceHarnessError::Realtime("fixture event lacks series identity".to_string())
    })?;
    series.instrument_revision = series.instrument_revision.saturating_add(1);
    let series_changed =
        CanonicalMarketEvent::try_new(series_header, base.payload()).map_err(realtime_error)?;

    let mut schema_header = base.header().clone();
    schema_header.schema_version = schema_header.schema_version.saturating_add(1);
    let schema_changed =
        CanonicalMarketEvent::try_new(schema_header, base.payload()).map_err(realtime_error)?;

    let mut semantic_header = base.header().clone();
    semantic_header.semantic_class = SemanticClass::StateReplace;
    let semantic_changed =
        CanonicalMarketEvent::try_new(semantic_header, base.payload()).map_err(realtime_error)?;

    Ok(
        latest_state_rejects_changed_event(initial, series_changed, "series_identity")?
            && latest_state_rejects_changed_event(initial, schema_changed, "schema_version")?
            && latest_state_rejects_changed_event(initial, semantic_changed, "semantic_class")?,
    )
}

fn latest_state_rejects_changed_event(
    initial: &CanonicalSnapshot,
    event: CanonicalMarketEvent,
    expected_field: &'static str,
) -> Result<bool, ConformanceHarnessError> {
    let owner = PartitionOwner::try_new(7, format!("latest_negative_{expected_field}"), 1)
        .map_err(realtime_error)?;
    let mut partition = FencedPartition::new(owner);
    let fence = partition.publication_fence();
    let mut latest = CanonicalLatestState::try_new(
        &mut partition,
        &fence,
        initial,
        NonZeroUsize::new(3).unwrap_or(NonZeroUsize::MIN),
    )
    .map_err(realtime_error)?;
    let semantic_class = event.header().semantic_class;
    let overflow_action = match semantic_class {
        SemanticClass::StateReplace => OverflowAction::ConflateNewest,
        SemanticClass::OrderedDelta => OverflowAction::RequestSnapshot,
        SemanticClass::ReliableEvent
        | SemanticClass::AuthoritativeEvent
        | SemanticClass::Snapshot => OverflowAction::RejectBeforeAcceptance,
    };
    let mut direct_policy = queue_policy("latest_negative_direct", 1);
    direct_policy.semantic_class = semantic_class;
    direct_policy.overflow_action = overflow_action;
    let mut durable_policy = queue_policy("latest_negative_durable", 1);
    durable_policy.semantic_class = semantic_class;
    durable_policy.overflow_action = overflow_action;
    let direct = BoundedEventBranch::try_new(direct_policy).map_err(realtime_error)?;
    let durable = BoundedEventBranch::try_new(durable_policy).map_err(realtime_error)?;
    let mut fanout =
        DirectDurableFanout::try_new(fence.clone(), direct, durable).map_err(realtime_error)?;
    let direct = accept_publish_pop_direct(&mut partition, &mut fanout, &fence, event)?;
    let rejected = matches!(
        latest.apply_direct_event(&partition, &fence, direct),
        Err(RealtimeError::SnapshotEventMismatch { field, .. }) if field == expected_field
    );
    let preserved = latest.generation() == 1
        && latest.retained_items() == 2
        && latest.requires_snapshot()
        && matches!(
            latest.serve_snapshot(
                &partition,
                &fence,
                LatestStateSnapshotRequest::new(NonZeroUsize::MIN),
            ),
            Err(RealtimeError::LatestStateSnapshotRequired)
        );
    Ok(rejected && preserved)
}

fn stale_client_snapshots_preserve_generation(
    source_five: &axiusflow_application::ReplaySnapshot,
    source_six: &axiusflow_application::ReplaySnapshot,
    current: &CanonicalSnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let current_replay = try_project_canonical_market_bar_snapshot(
        current,
        source_six.instrument(),
        ReplayProvenance::EmbeddedFixture,
        source_six.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let mut model = MarketBarClientModel::new(NonZeroUsize::new(6).unwrap_or(NonZeroUsize::MIN));
    let current_installed = matches!(
        model
            .apply_update(ReplayStreamUpdate::Snapshot(current_replay))
            .map_err(market_stream_error)?,
        MarketBarModelOutcome::Published(_)
    );
    let stale = canonical_market_bar_snapshot(source_five, 1, 3)?;
    let stale_replay = try_project_canonical_market_bar_snapshot(
        &stale,
        source_five.instrument(),
        ReplayProvenance::EmbeddedFixture,
        source_five.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let stale_rejected = model
        .apply_update(ReplayStreamUpdate::Snapshot(stale_replay))
        .is_err();
    let stale_preserved = model.current_generation().is_some_and(|generation| {
        generation.ownership_epoch() == 1
            && generation.generation() == 4
            && generation.sequence_range() == (1, 6)
    });

    let epoch_two = canonical_market_bar_snapshot(source_five, 2, 1)?;
    let epoch_two_replay = try_project_canonical_market_bar_snapshot(
        &epoch_two,
        source_five.instrument(),
        ReplayProvenance::EmbeddedFixture,
        source_five.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let epoch_two_installed = matches!(
        model
            .apply_update(ReplayStreamUpdate::Snapshot(epoch_two_replay))
            .map_err(market_stream_error)?,
        MarketBarModelOutcome::Published(_)
    );
    let lower_epoch = canonical_market_bar_snapshot(source_six, 1, 5)?;
    let lower_epoch_replay = try_project_canonical_market_bar_snapshot(
        &lower_epoch,
        source_six.instrument(),
        ReplayProvenance::EmbeddedFixture,
        source_six.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let lower_epoch_rejected = model
        .apply_update(ReplayStreamUpdate::Snapshot(lower_epoch_replay))
        .is_err();
    let lower_epoch_preserved = model.current_generation().is_some_and(|generation| {
        generation.ownership_epoch() == 2
            && generation.generation() == 1
            && generation.sequence_range() == (1, 5)
    });
    Ok(current_installed
        && stale_rejected
        && stale_preserved
        && epoch_two_installed
        && lower_epoch_rejected
        && lower_epoch_preserved)
}

fn canonical_snapshot_chunks_install_atomically(
    fixture: &BinaryMarketStreamFixture,
    source: &axiusflow_application::ReplaySnapshot,
    snapshot: &CanonicalSnapshot,
) -> Result<bool, ConformanceHarnessError> {
    let replay = try_project_canonical_market_bar_snapshot(
        snapshot,
        source.instrument(),
        ReplayProvenance::EmbeddedFixture,
        source.bar_definition(),
    )
    .map_err(market_stream_error)?;
    let envelopes = try_encode_replay_snapshot_chunk_envelopes(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        "latest_state_recovery_1",
        &replay,
        &fixture.convention,
        NonZeroUsize::MIN,
    )
    .map_err(market_stream_error)?;
    let matches_snapshot = |session: &MarketWebSocketSession| {
        session.current_generation().is_some_and(|generation| {
            generation.ownership_epoch() == snapshot.descriptor().ownership_epoch.get()
                && generation.generation() == snapshot.descriptor().generation.get()
                && generation.sequence_range()
                    == (
                        snapshot.descriptor().first_sequence.get(),
                        snapshot.descriptor().last_sequence.get(),
                    )
        })
    };

    let mut session = websocket_fixture_session(fixture, websocket_fixture_config(fixture, 2)?)?;
    let final_index = envelopes.len().saturating_sub(1);
    let mut atomic = true;
    for (index, envelope) in envelopes.iter().enumerate() {
        let frame = encode_market_bar_stream_frame(envelope, fixture.maximum_frame_bytes)
            .map_err(market_stream_error)?;
        let outcome = session
            .accept_message(Message::binary(frame))
            .map_err(websocket_error)?;
        let expected = usize::from(index == final_index);
        atomic &= outcome
            == WebSocketMessageOutcome::BinaryAccepted {
                published: expected,
            }
            && (index == final_index || session.current_generation().is_none());
    }
    let installed = atomic && matches_snapshot(&session);

    let reordered_rejected = if envelopes.len() > 1 {
        let reordered = encode_market_bar_stream_frame(&envelopes[1], fixture.maximum_frame_bytes)
            .map_err(market_stream_error)?;
        session.accept_message(Message::binary(reordered)).is_err() && matches_snapshot(&session)
    } else {
        false
    };

    let mut malformed_session =
        websocket_fixture_session(fixture, websocket_fixture_config(fixture, 2)?)?;
    for envelope in &envelopes {
        let frame = encode_market_bar_stream_frame(envelope, fixture.maximum_frame_bytes)
            .map_err(market_stream_error)?;
        malformed_session
            .accept_message(Message::binary(frame))
            .map_err(websocket_error)?;
    }
    let mut malformed = encode_market_bar_stream_frame(&envelopes[0], fixture.maximum_frame_bytes)
        .map_err(market_stream_error)?;
    let last = malformed.len().checked_sub(1).ok_or_else(|| {
        ConformanceHarnessError::MarketStream("empty encoded snapshot frame".to_string())
    })?;
    malformed[last] ^= 1;
    let malformed_rejected = malformed_session
        .accept_message(Message::binary(malformed))
        .is_err()
        && matches_snapshot(&malformed_session);

    Ok(installed && reordered_rejected && malformed_rejected)
}
