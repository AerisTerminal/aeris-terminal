//! Canonical market-bar fixtures shared by the partition, fanout, and wire scenarios.
//!
//! These builders derive canonical snapshots, events, and enqueue timestamps from the
//! embedded replay fixture, and provide the shared fenced-partition acceptance and
//! direct/latest-state publication steps. They own no scenario assertions.

use crate::harness_error::{ConformanceHarnessError, market_stream_error, realtime_error};
use axiusflow_market_protocol_adapter::try_encode_canonical_market_bar_payload;
use axiusflow_realtime::{
    AcceptedCanonicalEvent, AcceptedDirectEvent, CanonicalEventHeader, CanonicalLatestState,
    CanonicalMarketEvent, CanonicalSeriesIdentity, CanonicalSnapshot, CanonicalTimestamps,
    DirectDurableFanout, FencedPartition, NicTimestampSource, PartitionAcceptance,
    PartitionDecision, QueueOutcome, SemanticClass,
};

pub(crate) fn publish_direct_to_latest(
    partition: &mut FencedPartition,
    fanout: &mut DirectDurableFanout,
    latest: &mut CanonicalLatestState,
    fence: &axiusflow_realtime::PublicationFence,
    event: CanonicalMarketEvent,
) -> Result<(), ConformanceHarnessError> {
    let direct = accept_publish_pop_direct(partition, fanout, fence, event)?;
    if latest
        .apply_direct_event(partition, fence, direct)
        .map_err(realtime_error)?
        != PartitionDecision::Accepted
    {
        return Err(ConformanceHarnessError::Realtime(
            "latest-state projection rejected a contiguous direct event".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn accept_for_fanout(
    partition: &mut FencedPartition,
    fence: &axiusflow_realtime::PublicationFence,
    event: CanonicalMarketEvent,
) -> Result<AcceptedCanonicalEvent, ConformanceHarnessError> {
    match partition
        .accept_for_publication(fence, event)
        .map_err(realtime_error)?
    {
        PartitionAcceptance::Accepted(accepted) => Ok(*accepted),
        outcome => Err(ConformanceHarnessError::Realtime(format!(
            "fixture event was not accepted for publication: {outcome:?}"
        ))),
    }
}

pub(crate) fn accept_publish_pop_direct(
    partition: &mut FencedPartition,
    fanout: &mut DirectDurableFanout,
    fence: &axiusflow_realtime::PublicationFence,
    event: CanonicalMarketEvent,
) -> Result<AcceptedDirectEvent, ConformanceHarnessError> {
    let accepted = accept_for_fanout(partition, fence, event)?;
    let enqueued = accepted
        .event()
        .header()
        .timestamps
        .fanout_enqueue_unix_nanos
        .ok_or_else(|| {
            ConformanceHarnessError::Realtime("fixture event lacks fanout time".to_string())
        })?;
    if fanout
        .publish(partition, fence, accepted, enqueued)
        .map_err(realtime_error)?
        .direct
        != QueueOutcome::Enqueued
    {
        return Err(ConformanceHarnessError::Realtime(
            "direct branch rejected an accepted fixture event".to_string(),
        ));
    }
    fanout
        .pop_direct(enqueued.saturating_add(10))
        .map(|(direct, _)| direct)
        .ok_or_else(|| ConformanceHarnessError::Realtime("missing direct event".to_string()))
}

pub(crate) fn canonical_market_bar_snapshot(
    source: &axiusflow_application::ReplaySnapshot,
    ownership_epoch: u64,
    generation: u64,
) -> Result<CanonicalSnapshot, ConformanceHarnessError> {
    canonical_market_bar_snapshot_prefix(source, ownership_epoch, generation, source.bars().len())
}

pub(crate) fn canonical_market_bar_snapshot_prefix(
    source: &axiusflow_application::ReplaySnapshot,
    ownership_epoch: u64,
    generation: u64,
    item_count: usize,
) -> Result<CanonicalSnapshot, ConformanceHarnessError> {
    let events = source
        .bars()
        .iter()
        .take(item_count)
        .map(|item| canonical_market_bar_event(item, source, ownership_epoch))
        .collect::<Result<Vec<_>, _>>()?;
    CanonicalSnapshot::try_from_events(7, ownership_epoch, generation, 1, events)
        .map_err(realtime_error)
}

pub(crate) fn canonical_market_bar_event(
    item: &axiusflow_application::ProvenancedMarketBar,
    source: &axiusflow_application::ReplaySnapshot,
    ownership_epoch: u64,
) -> Result<CanonicalMarketEvent, ConformanceHarnessError> {
    let bar = item.value();
    let original = item.provenance();
    let normalized = original.exchange_timestamp_unix_nanos.saturating_add(3_000);
    let payload = try_encode_canonical_market_bar_payload(bar).map_err(market_stream_error)?;
    CanonicalMarketEvent::try_new(
        CanonicalEventHeader {
            event_id: format!("direct_wire_{ownership_epoch}_{}", bar.source_sequence),
            event_time_unix_nanos: original.event_time_unix_nanos,
            publication_time_unix_nanos: original
                .exchange_timestamp_unix_nanos
                .saturating_add(5_000),
            producer: "canonical_market_bar_fixture".to_string(),
            correlation_id: "direct_market_bar_wire_fixture".to_string(),
            causation_id: original.event_id.clone(),
            entitlement_revision: original.entitlement_revision.clone(),
            instrument_id: source.instrument().instrument_id.as_str().to_string(),
            venue_id: source.instrument().venue_id.clone(),
            source_id: original.source_id.clone(),
            series_identity: Some(CanonicalSeriesIdentity {
                instrument_revision: source.instrument().revision,
                definition_id: source.bar_definition().definition_id.clone(),
                definition_version: source.bar_definition().version,
                interval_seconds: source.bar_definition().interval_seconds,
            }),
            source_sequence: bar.source_sequence,
            partition_id: 7,
            ownership_epoch,
            timestamps: CanonicalTimestamps {
                exchange_unix_nanos: original.exchange_timestamp_unix_nanos,
                provider_receive_unix_nanos: original
                    .exchange_timestamp_unix_nanos
                    .saturating_add(1_000),
                nic_receive_unix_nanos: Some(
                    original.exchange_timestamp_unix_nanos.saturating_add(2_000),
                ),
                axiusflow_receive_unix_nanos: original
                    .exchange_timestamp_unix_nanos
                    .saturating_add(2_000),
                normalized_unix_nanos: normalized,
                fanout_enqueue_unix_nanos: Some(normalized.saturating_add(1_000)),
            },
            nic_timestamp_source: Some(NicTimestampSource::SocketSoftware),
            correction_flags: original.correction_flags,
            quality_flags: original.quality_flags,
            schema_version: original.schema_version,
            semantic_class: SemanticClass::OrderedDelta,
        },
        &payload,
    )
    .map_err(realtime_error)
}

pub(crate) fn canonical_enqueue_time(
    source: &axiusflow_application::ReplaySnapshot,
    index: usize,
) -> i64 {
    source.bars()[index]
        .provenance()
        .exchange_timestamp_unix_nanos
        .saturating_add(4_000)
}
