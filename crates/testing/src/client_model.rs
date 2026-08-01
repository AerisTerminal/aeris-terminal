//! Bounded single-writer market-bar client model evidence.
//!
//! This module owns immutable generation publication, schema-change resnapshot latching,
//! sequence-gap recovery, and ownership-handoff rejection for the bounded client model.
//! It reuses the shared binary fixture and its projection comparison helper.

use crate::binary_fixture::{
    BINARY_FIXTURE_SUBSCRIPTION_ID, BinaryMarketStreamFixture, FixtureMarketGeneration,
    binary_fixture_decoder, binary_market_stream_fixture, expect_published_generation,
    passed_check_bits, replay_snapshot_with_generation,
};
use crate::harness_error::{ConformanceHarnessError, market_stream_error};
use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarClientModel, MarketBarModelOutcome,
    MarketBarReplayPort, ReplayStreamUpdate, ResnapshotReason,
};
use std::num::NonZeroUsize;

/// Headless evidence for the bounded single-writer market-bar client model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarketBarClientModelConformance {
    passed_checks: u8,
}

impl MarketBarClientModelConformance {
    const REQUIRED_CHECKS: u8 = 0b0111_1111;

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }
}

/// Exercises binary projection into immutable bounded client-model generations.
///
/// This proves deterministic fixture/model semantics only. It does not prove a
/// WebSocket connection, live provider, entitlement service, or physical display.
///
/// # Errors
///
/// Returns an error when valid fixture data is rejected by decoding or model validation.
pub fn run_market_bar_client_model_conformance()
-> Result<MarketBarClientModelConformance, ConformanceHarnessError> {
    let fixture = binary_market_stream_fixture()?;
    let mut decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    let mut updates = decoder
        .push(&fixture.joined_frames)
        .map_err(market_stream_error)?;
    let binary_projection_complete = updates.len() == 2
        && updates
            .iter()
            .all(|update| update.subscription_id == BINARY_FIXTURE_SUBSCRIPTION_ID);
    let delta_update = updates.pop().ok_or_else(|| {
        ConformanceHarnessError::MarketStream("missing projected fixture delta".to_string())
    })?;
    let snapshot_update = updates.pop().ok_or_else(|| {
        ConformanceHarnessError::MarketStream("missing projected fixture snapshot".to_string())
    })?;

    let mut model = axiusflow_application::MarketBarClientModel::new(
        NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN),
    );
    let snapshot_generation = expect_published_generation(
        model
            .apply_update(snapshot_update.update)
            .map_err(market_stream_error)?,
        "fixture snapshot",
    )?;
    let snapshot_published = snapshot_generation.sequence_range() == (1, 2)
        && snapshot_generation.items() == fixture.baseline.bars();
    let delta_generation = expect_published_generation(
        model
            .apply_update(delta_update.update)
            .map_err(market_stream_error)?,
        "fixture delta",
    )?;
    let bounded_delta_published = delta_generation.sequence_range() == (2, 3)
        && delta_generation.generation() == snapshot_generation.generation().saturating_add(1)
        && delta_generation.items().last() == Some(fixture.delta.item());
    let prior_generation_immutable = snapshot_generation.sequence_range() == (1, 2)
        && snapshot_generation.items() == fixture.baseline.bars();
    let provenance_retained = delta_generation
        .items()
        .iter()
        .all(|item| axiusflow_application::validate_provenanced_market_bar(item).is_ok());
    let schema_change_latched = client_schema_change_requires_snapshot(&fixture)?;
    let (gap_latched, snapshot_recovered, recovered_generation) =
        exercise_gap_recovery(&mut model, &delta_generation)?;
    let ownership_handoff_blocked = exercise_ownership_handoff(&mut model, &recovered_generation)?;

    Ok(MarketBarClientModelConformance {
        passed_checks: passed_check_bits([
            binary_projection_complete,
            snapshot_published,
            bounded_delta_published,
            prior_generation_immutable,
            provenance_retained && schema_change_latched,
            gap_latched,
            snapshot_recovered && ownership_handoff_blocked,
        ]),
    })
}

fn client_schema_change_requires_snapshot(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let mut model = MarketBarClientModel::new(NonZeroUsize::new(3).unwrap_or(NonZeroUsize::MIN));
    let installed = expect_published_generation(
        model
            .apply_update(ReplayStreamUpdate::Snapshot(fixture.baseline.clone()))
            .map_err(market_stream_error)?,
        "schema fixture snapshot",
    )?;
    let mut provenance = fixture.delta.item().provenance().clone();
    provenance.schema_version = provenance.schema_version.saturating_add(1);
    let changed_item =
        axiusflow_application::Provenanced::new(*fixture.delta.item().value(), provenance);
    let changed_delta = axiusflow_application::StreamDelta::try_new(
        fixture.delta.previous_sequence(),
        fixture.delta.sequence(),
        changed_item,
    )
    .map_err(market_stream_error)?;
    let outcome = model
        .apply_update(ReplayStreamUpdate::Delta(changed_delta))
        .map_err(market_stream_error)?;
    Ok(matches!(
        outcome,
        MarketBarModelOutcome::ResnapshotRequired(ResnapshotReason::SchemaChanged)
    ) && model.current_generation() == Some(&installed)
        && model.requires_snapshot())
}

fn exercise_gap_recovery(
    model: &mut axiusflow_application::MarketBarClientModel,
    frozen_generation: &FixtureMarketGeneration,
) -> Result<(bool, bool, FixtureMarketGeneration), ConformanceHarnessError> {
    let source = EmbeddedReplaySource;
    let gap_delta = source
        .load_delta(4)
        .map_err(market_stream_error)?
        .ok_or_else(|| ConformanceHarnessError::MarketStream("missing gap delta".to_string()))?;
    let gap_outcome = model
        .apply_update(ReplayStreamUpdate::Delta(gap_delta))
        .map_err(market_stream_error)?;
    let contiguous_delta = source
        .load_delta(3)
        .map_err(market_stream_error)?
        .ok_or_else(|| {
            ConformanceHarnessError::MarketStream("missing contiguous delta".to_string())
        })?;
    let latched_outcome = model
        .apply_update(ReplayStreamUpdate::Delta(contiguous_delta))
        .map_err(market_stream_error)?;
    let gap_latched = matches!(
        gap_outcome,
        axiusflow_application::MarketBarModelOutcome::ResnapshotRequired(
            axiusflow_application::ResnapshotReason::SequenceGap
        )
    ) && matches!(
        latched_outcome,
        axiusflow_application::MarketBarModelOutcome::ResnapshotRequired(
            axiusflow_application::ResnapshotReason::SequenceGap
        )
    ) && model.current_generation() == Some(frozen_generation);

    let recovery = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 4 })
        .map_err(market_stream_error)?;
    let recovery = replay_snapshot_with_generation(&recovery, 3)?;
    let recovered_generation = expect_published_generation(
        model
            .apply_update(ReplayStreamUpdate::Snapshot(recovery.clone()))
            .map_err(market_stream_error)?,
        "gap recovery snapshot",
    )?;
    let snapshot_recovered = !model.requires_snapshot()
        && recovered_generation.sequence_range() == (3, 4)
        && recovered_generation.items().len() == 2
        && recovered_generation
            .items()
            .iter()
            .eq(recovery.bars().iter().skip(2));
    Ok((gap_latched, snapshot_recovered, recovered_generation))
}

fn exercise_ownership_handoff(
    model: &mut axiusflow_application::MarketBarClientModel,
    frozen_generation: &FixtureMarketGeneration,
) -> Result<bool, ConformanceHarnessError> {
    let source_delta = EmbeddedReplaySource
        .load_delta(4)
        .map_err(market_stream_error)?
        .ok_or_else(|| {
            ConformanceHarnessError::MarketStream("missing ownership delta".to_string())
        })?;
    let mut changed_provenance = source_delta.item().provenance().clone();
    changed_provenance.ownership_epoch = changed_provenance.ownership_epoch.saturating_add(1);
    let changed_item =
        axiusflow_application::Provenanced::new(*source_delta.item().value(), changed_provenance);
    let changed_delta = axiusflow_application::StreamDelta::try_new(
        source_delta.previous_sequence(),
        source_delta.sequence(),
        changed_item,
    )
    .map_err(market_stream_error)?;
    let outcome = model
        .apply_update(ReplayStreamUpdate::Delta(changed_delta))
        .map_err(market_stream_error)?;
    Ok(matches!(
        outcome,
        axiusflow_application::MarketBarModelOutcome::ResnapshotRequired(
            axiusflow_application::ResnapshotReason::OwnershipHandoff
        )
    ) && model.requires_snapshot()
        && model.current_generation() == Some(frozen_generation))
}
