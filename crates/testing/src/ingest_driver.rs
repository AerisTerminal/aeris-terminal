//! Profile-agnostic ingest driver conformance and semantic-equivalence comparison.
//!
//! This module owns the bounded receive/release, startup/shutdown, and decoder lifecycle
//! exercised by every ingest profile adapter, plus the canonical-outcome comparison used
//! to prove that different profiles produce equivalent events.

use crate::harness_error::ConformanceHarnessError;
use crate::packet_corpus::{ConformanceOutcome, FixtureProviderDecoder};
use axiusflow_transport::{IngestDriver, QueueBinding, ReceiveBatch};
use std::num::NonZeroUsize;

/// Runs the same bounded receive/release, startup/shutdown, and decoder lifecycle for any
/// profile adapter.
///
/// # Errors
///
/// Returns an error when queue binding, startup, receive, or shutdown fails.
pub fn run_ingest_conformance<Driver>(
    driver: &mut Driver,
) -> Result<Vec<ConformanceOutcome>, ConformanceHarnessError>
where
    Driver: IngestDriver,
{
    run_ingest_conformance_after_start(driver, |_| Ok(()))
}

/// Runs ingest conformance after allowing a caller to seed a started native adapter.
///
/// The callback is intended for deterministic loopback senders. It executes only after
/// queue binding and startup, and before the first receive.
///
/// # Errors
///
/// Returns an error from lifecycle operations, the startup callback, or receive.
pub fn run_ingest_conformance_after_start<Driver>(
    driver: &mut Driver,
    after_start: impl FnOnce(&mut Driver) -> Result<(), ConformanceHarnessError>,
) -> Result<Vec<ConformanceOutcome>, ConformanceHarnessError>
where
    Driver: IngestDriver,
{
    driver
        .bind_queue(QueueBinding {
            queue_id: 0,
            maximum_batch_items: NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN),
            maximum_frame_bytes: NonZeroUsize::new(2_048).unwrap_or(NonZeroUsize::MIN),
        })
        .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
    driver
        .start()
        .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
    after_start(driver)?;

    let mut outcomes = Vec::new();
    let mut decoder = FixtureProviderDecoder::default();
    loop {
        let batch = driver
            .receive_batch()
            .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
        let count = batch.frame_count();
        for index in 0..count {
            let Some(frame) = batch.frame(index) else {
                return Err(ConformanceHarnessError::MissingFrame(index));
            };
            outcomes.push(decoder.decode(frame.bytes, frame.metadata));
        }
        batch.release();
        if count == 0 {
            break;
        }
    }
    driver
        .shutdown()
        .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
    Ok(outcomes)
}

/// Compares adapter outcomes while excluding wall-clock receive values that cannot be
/// identical across independently executed native adapters.
///
/// Exchange timestamps, canonical identity, ordering, provenance, payloads, and every
/// non-accepted decision remain exact-match requirements.
#[must_use]
pub fn ingest_outcomes_semantically_equivalent(
    left: &[ConformanceOutcome],
    right: &[ConformanceOutcome],
) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| match (left, right) {
                (ConformanceOutcome::Accepted(left), ConformanceOutcome::Accepted(right)) => {
                    let left_header = left.header();
                    let right_header = right.header();
                    left.payload() == right.payload()
                        && left_header.event_id == right_header.event_id
                        && left_header.event_time_unix_nanos == right_header.event_time_unix_nanos
                        && left_header.producer == right_header.producer
                        && left_header.correlation_id == right_header.correlation_id
                        && left_header.causation_id == right_header.causation_id
                        && left_header.entitlement_revision == right_header.entitlement_revision
                        && left_header.instrument_id == right_header.instrument_id
                        && left_header.venue_id == right_header.venue_id
                        && left_header.source_id == right_header.source_id
                        && left_header.series_identity == right_header.series_identity
                        && left_header.source_sequence == right_header.source_sequence
                        && left_header.partition_id == right_header.partition_id
                        && left_header.ownership_epoch == right_header.ownership_epoch
                        && left_header.timestamps.exchange_unix_nanos
                            == right_header.timestamps.exchange_unix_nanos
                        && left_header.nic_timestamp_source == right_header.nic_timestamp_source
                        && left_header.correction_flags == right_header.correction_flags
                        && left_header.quality_flags == right_header.quality_flags
                        && left_header.schema_version == right_header.schema_version
                        && left_header.semantic_class == right_header.semantic_class
                }
                _ => left == right,
            })
}
