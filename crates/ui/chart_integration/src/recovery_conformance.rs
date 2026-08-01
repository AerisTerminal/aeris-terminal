//! Headless evidence for the bounded UI recovery lifecycle.

use crate::bridge::{ChartDataBridge, MAX_RECOVERY_DISPATCH_ATTEMPTS};
use crate::origin_bridge::install_replay;
use crate::provenance::DisplayedProvenance;
use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarReplayPort, ReplaySnapshot,
    ReplayStreamUpdate, ReplayValidationError,
};
use axiusflow_protocols::StreamProtocolError;
use origin_engine::ChartEngine;
use std::num::NonZeroUsize;

/// Headless result for the bounded UI recovery lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChartRecoveryConformance {
    passed_checks: u8,
}

impl ChartRecoveryConformance {
    const REQUIRED_CHECKS: u8 = u8::MAX;

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }
}

/// Exercises overflow recovery, correlation, integrity, and displayed provenance retention.
///
/// # Errors
///
/// Returns an error only if the embedded application fixture is invalid.
pub fn run_chart_bridge_recovery_conformance()
-> Result<ChartRecoveryConformance, ReplayValidationError> {
    let source = EmbeddedReplaySource;
    let baseline = source.load_snapshot(LoadEmbeddedReplay { bar_count: 1 })?;
    let recovered = source.load_snapshot(LoadEmbeddedReplay { bar_count: 3 })?;
    let mut bridge = ChartDataBridge::try_new(NonZeroUsize::MIN, &baseline)?;
    let delta_2 = source
        .load_delta(1)?
        .ok_or(StreamProtocolError::SequenceOverflow)?;
    let delta_3 = source
        .load_delta(2)?
        .ok_or(StreamProtocolError::SequenceOverflow)?;
    let first_push_ok = bridge.try_push(ReplayStreamUpdate::Delta(delta_2)).is_ok();
    let overflowed = bridge.try_push(ReplayStreamUpdate::Delta(delta_3)).is_err();
    let overflow_metrics = bridge.metrics();
    let command = bridge
        .pending_resnapshot_request()
        .ok_or(StreamProtocolError::EmptySnapshot)?;
    let rejected_dispatch = bridge.try_dispatch_recovery(|_| Err::<(), ()>(())).is_err();
    let remained_retryable = bridge.pending_resnapshot_request() == Some(command)
        && !bridge.metrics().recovery_dispatched;
    let accepted_dispatch = bridge
        .try_dispatch_recovery(|offered| if offered == command { Ok(()) } else { Err(()) })
        .is_ok_and(|dispatched| dispatched);
    let ordinary_recovery_snapshot_rejected = bridge
        .try_push(ReplayStreamUpdate::Snapshot(recovered.clone()))
        .is_ok()
        && bridge.drain_merged()?.is_some()
        && bridge.requires_snapshot();
    let stale_response_rejected =
        !bridge.install_recovery_snapshot(command.request_id.saturating_add(1), &recovered)?;
    let correlated_response_installed =
        bridge.install_recovery_snapshot(command.request_id, &recovered)?;
    let duplicate_snapshot_rejected = matches!(
        bridge.install_snapshot(&recovered),
        Err(ReplayValidationError::StaleSnapshot { .. })
    );
    let cross_generation_rollback_rejected =
        cross_generation_rollback_check(&baseline, &recovered)?;
    let recovered_metrics = bridge.metrics();

    let bounded_retry_cancellation = bounded_retry_cancellation_check(&baseline)?;

    let (corrupted_snapshot_rejected, mismatched_pair_rejected) =
        snapshot_integrity_checks(&recovered);

    let displayed = DisplayedProvenance::from_snapshot(&recovered);
    let older_and_latest_provenance_retained = displayed.get(1).is_some_and(|provenance| {
        provenance.source_sequence == 1
            && provenance.entitlement_revision == "embedded_fixture_entitlement_v1"
    }) && displayed
        .latest()
        .is_some_and(|provenance| provenance.source_sequence == 3);
    let evidence_retained = recovered.evidence().first_sequence == 1
        && recovered.evidence().last_sequence == 3
        && recovered.evidence().generation == 1
        && recovered.evidence().checksum != [0; 32];
    let bounded_retention = bounded_retention_check()?;

    let mut passed_checks = 0_u8;
    if first_push_ok
        && overflowed
        && overflow_metrics.snapshot_required
        && overflow_metrics.recovery_pending
    {
        passed_checks |= 0b0000_0001;
    }
    if rejected_dispatch && remained_retryable && bounded_retry_cancellation {
        passed_checks |= 0b0000_0010;
    }
    if accepted_dispatch && ordinary_recovery_snapshot_rejected && stale_response_rejected {
        passed_checks |= 0b0000_0100;
    }
    if correlated_response_installed
        && duplicate_snapshot_rejected
        && cross_generation_rollback_rejected
        && !recovered_metrics.snapshot_required
        && !recovered_metrics.recovery_pending
        && recovered_metrics.completed_recoveries == 1
        && recovered_metrics.rejected_uncorrelated_snapshots == 2
        && recovered_metrics.rejected_stale_snapshots == 1
    {
        passed_checks |= 0b0000_1000;
    }
    if corrupted_snapshot_rejected {
        passed_checks |= 0b0001_0000;
    }
    if mismatched_pair_rejected {
        passed_checks |= 0b0010_0000;
    }
    if older_and_latest_provenance_retained {
        passed_checks |= 0b0100_0000;
    }
    if evidence_retained && bounded_retention {
        passed_checks |= 0b1000_0000;
    }
    Ok(ChartRecoveryConformance { passed_checks })
}

fn cross_generation_rollback_check(
    baseline: &ReplaySnapshot,
    recovered: &ReplaySnapshot,
) -> Result<bool, ReplayValidationError> {
    let rollback =
        baseline.try_with_generation(recovered.evidence().generation.saturating_add(1))?;
    let mut ordinary = ChartDataBridge::try_new(NonZeroUsize::MIN, recovered)?;
    let ordinary_rejected = matches!(
        ordinary.install_snapshot(&rollback),
        Err(ReplayValidationError::StaleSnapshot { .. })
    );

    let mut correlated = ChartDataBridge::try_new(NonZeroUsize::MIN, recovered)?;
    correlated.mark_stream_invalid();
    let command = correlated
        .pending_resnapshot_request()
        .ok_or(StreamProtocolError::EmptySnapshot)?;
    let dispatched = correlated
        .try_dispatch_recovery(|offered| if offered == command { Ok(()) } else { Err(()) })
        .is_ok_and(|accepted| accepted);
    let correlated_rejected =
        !correlated.install_recovery_snapshot(command.request_id, &rollback)?;
    Ok(ordinary_rejected
        && dispatched
        && correlated_rejected
        && correlated.metrics().rejected_stale_snapshots == 1)
}

fn bounded_retry_cancellation_check(
    baseline: &ReplaySnapshot,
) -> Result<bool, ReplayValidationError> {
    let mut bridge = ChartDataBridge::try_new(NonZeroUsize::MIN, baseline)?;
    bridge.mark_stream_invalid();
    let command = bridge
        .pending_resnapshot_request()
        .ok_or(StreamProtocolError::EmptySnapshot)?;
    let mut bounded = true;
    for _ in 0..MAX_RECOVERY_DISPATCH_ATTEMPTS {
        bounded &= bridge
            .try_dispatch_recovery(|offered| if offered == command { Ok(()) } else { Err(()) })
            .is_ok_and(|dispatched| dispatched);
        bounded &= bridge.mark_recovery_failed(command.request_id);
    }
    let metrics = bridge.metrics();
    Ok(bounded
        && bridge.pending_resnapshot_request().is_none()
        && metrics.canceled_recoveries == 1
        && metrics.recovery_dispatch_attempts == 0)
}

fn bounded_retention_check() -> Result<bool, ReplayValidationError> {
    let replay = EmbeddedReplaySource.load_snapshot(LoadEmbeddedReplay { bar_count: 16 })?;
    let limit = NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN);
    let provenance = DisplayedProvenance::from_snapshot_with_limit(&replay, limit);
    let mut engine = ChartEngine::new(320.0, 200.0, 1.0);
    install_replay(&mut engine, &replay);
    Ok(engine.set_series_max_points(0, Some(limit.get()))
        && engine.series_max_points(0) == Some(limit.get())
        && provenance.len() == limit.get()
        && provenance.get(12).is_none()
        && provenance.get(13).is_some()
        && provenance
            .latest()
            .is_some_and(|item| item.source_sequence == 16))
}

fn snapshot_integrity_checks(recovered: &ReplaySnapshot) -> (bool, bool) {
    let mut corrupted_items = recovered.bars().to_vec();
    let (mut corrupted_bar, corrupted_provenance) = corrupted_items[0].clone().into_parts();
    corrupted_bar.volume = corrupted_bar.volume.saturating_add(1);
    corrupted_items[0] =
        axiusflow_application::Provenanced::new(corrupted_bar, corrupted_provenance);
    let corrupted_snapshot_rejected = matches!(
        ReplaySnapshot::try_new_provenanced(
            recovered.instrument().clone(),
            recovered.provenance(),
            recovered.bar_definition().clone(),
            recovered.evidence().clone(),
            corrupted_items,
        ),
        Err(ReplayValidationError::SnapshotChecksumMismatch)
    );
    let mut relabeled_definition = recovered.bar_definition().clone();
    relabeled_definition.definition_id.push_str(":relabeled");
    let relabeled_series_rejected = matches!(
        ReplaySnapshot::try_new_provenanced(
            recovered.instrument().clone(),
            recovered.provenance(),
            relabeled_definition,
            recovered.evidence().clone(),
            recovered.bars().to_vec(),
        ),
        Err(ReplayValidationError::SnapshotChecksumMismatch)
    );

    let mut mismatched_items = recovered.bars().to_vec();
    let (bar, mut mismatched_provenance) = mismatched_items[0].clone().into_parts();
    mismatched_provenance.source_sequence = mismatched_provenance.source_sequence.saturating_add(1);
    mismatched_items[0] = axiusflow_application::Provenanced::new(bar, mismatched_provenance);
    let sequence_mismatch_rejected = matches!(
        ReplaySnapshot::try_new_provenanced(
            recovered.instrument().clone(),
            recovered.provenance(),
            recovered.bar_definition().clone(),
            recovered.evidence().clone(),
            mismatched_items,
        ),
        Err(ReplayValidationError::ProvenanceSequenceMismatch { .. })
    );

    let mut timestamp_mismatched_items = recovered.bars().to_vec();
    let (bar, mut timestamp_mismatched_provenance) =
        timestamp_mismatched_items[0].clone().into_parts();
    timestamp_mismatched_provenance.exchange_timestamp_unix_nanos = timestamp_mismatched_provenance
        .exchange_timestamp_unix_nanos
        .saturating_add(1);
    timestamp_mismatched_items[0] =
        axiusflow_application::Provenanced::new(bar, timestamp_mismatched_provenance);
    let timestamp_mismatch_rejected = matches!(
        ReplaySnapshot::try_new_provenanced(
            recovered.instrument().clone(),
            recovered.provenance(),
            recovered.bar_definition().clone(),
            recovered.evidence().clone(),
            timestamp_mismatched_items,
        ),
        Err(ReplayValidationError::ProvenanceExchangeTimestampMismatch { .. })
    );
    (
        corrupted_snapshot_rejected && relabeled_series_rejected,
        sequence_mismatch_rejected && timestamp_mismatch_rejected,
    )
}
