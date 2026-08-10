//! Provider-neutral stream envelope encoding and bounded binary framing.
//!
//! This module owns snapshot, chunked-snapshot, and delta envelope construction plus the
//! bounded binary frame encoder. It never decodes; decoding lives in the stream decoder
//! and wire codec modules.

use crate::errors::{BinaryMarketStreamError, ProtobufAdapterError};
use crate::wire_codec::{encode_decimal_i64, try_encode_instrument_revision};
use crate::{DecimalConvention, MAX_MARKET_BAR_SNAPSHOT_CHUNKS};
use axiusflow_application::{
    ProvenancedMarketBar, ReplaySnapshot, validate_provenanced_market_bar,
};
use axiusflow_instruments::InstrumentRevision;
use axiusflow_market_data::BarDefinition;
use axiusflow_protocols::{
    MAX_STREAM_SNAPSHOT_ITEMS, StreamDelta,
    protobuf::axiusflow::{
        common::v1 as common_wire,
        market::v1::{self as market_wire, market_bar_stream_envelope},
    },
};
use axiusflow_transport::encode_binary_frame;
use prost::Message;
use std::num::NonZeroUsize;

/// Encodes one validated replay snapshot into the provider-neutral stream envelope.
///
/// # Errors
///
/// Returns an error for an empty subscription or invalid snapshot evidence.
pub fn try_encode_replay_snapshot_envelope(
    subscription_id: impl Into<String>,
    replay: &ReplaySnapshot,
    decimal_convention: &DecimalConvention,
) -> Result<market_wire::MarketBarStreamEnvelope, ProtobufAdapterError> {
    let subscription_id = subscription_id.into();
    if subscription_id.trim().is_empty() {
        return Err(ProtobufAdapterError::EmptySubscriptionId);
    }
    let bars = replay
        .bars()
        .iter()
        .map(|item| {
            try_encode_market_bar(
                item,
                replay.instrument(),
                replay.bar_definition(),
                decimal_convention,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let evidence = replay.evidence();
    Ok(market_wire::MarketBarStreamEnvelope {
        update: Some(market_bar_stream_envelope::Update::Snapshot(
            market_wire::MarketBarSnapshot {
                subscription_id,
                instrument: Some(try_encode_instrument_revision(replay.instrument())?),
                bars,
                first_sequence: evidence.first_sequence,
                last_sequence: evidence.last_sequence,
                session_generation: evidence.session_generation,
                publication_generation: evidence.publication_generation,
                checksum: evidence.checksum.to_vec(),
                schema_version: evidence.schema_version,
                snapshot_id: String::new(),
                chunk_index: 0,
                chunk_count: 0,
                total_item_count: 0,
            },
        )),
    })
}

/// Encodes a validated replay snapshot into ordered, bounded snapshot envelopes.
///
/// Every chunk repeats immutable whole-snapshot evidence. Consumers must emit no
/// application snapshot until all chunks arrive and the existing global checksum
/// validates over the complete assembly.
///
/// # Errors
///
/// Returns an error for empty identities, invalid replay evidence, or a chunk plan
/// that exceeds the bounded protocol limits.
pub fn try_encode_replay_snapshot_chunk_envelopes(
    subscription_id: impl Into<String>,
    snapshot_id: impl Into<String>,
    replay: &ReplaySnapshot,
    decimal_convention: &DecimalConvention,
    maximum_chunk_items: NonZeroUsize,
) -> Result<Vec<market_wire::MarketBarStreamEnvelope>, ProtobufAdapterError> {
    let snapshot_id = snapshot_id.into();
    if snapshot_id.trim().is_empty() {
        return Err(ProtobufAdapterError::EmptySnapshotId);
    }
    let complete =
        try_encode_replay_snapshot_envelope(subscription_id, replay, decimal_convention)?;
    let Some(market_bar_stream_envelope::Update::Snapshot(snapshot)) = complete.update else {
        return Err(ProtobufAdapterError::MissingUpdate);
    };
    let chunk_count = snapshot.bars.len().div_ceil(maximum_chunk_items.get());
    if chunk_count == 0 || chunk_count > MAX_MARKET_BAR_SNAPSHOT_CHUNKS {
        return Err(ProtobufAdapterError::SnapshotChunkCountLimitExceeded {
            actual: chunk_count,
            maximum: MAX_MARKET_BAR_SNAPSHOT_CHUNKS,
        });
    }
    let chunk_count_u32 = u32::try_from(chunk_count).map_err(|_| {
        ProtobufAdapterError::SnapshotChunkCountLimitExceeded {
            actual: chunk_count,
            maximum: MAX_MARKET_BAR_SNAPSHOT_CHUNKS,
        }
    })?;
    let total_item_count = u32::try_from(snapshot.bars.len()).map_err(|_| {
        ProtobufAdapterError::SnapshotTotalItemCountInvalid {
            actual: snapshot.bars.len(),
            maximum: MAX_STREAM_SNAPSHOT_ITEMS,
        }
    })?;
    snapshot
        .bars
        .chunks(maximum_chunk_items.get())
        .enumerate()
        .map(|(index, bars)| {
            let chunk_index = u32::try_from(index).map_err(|_| {
                ProtobufAdapterError::SnapshotChunkCountLimitExceeded {
                    actual: chunk_count,
                    maximum: MAX_MARKET_BAR_SNAPSHOT_CHUNKS,
                }
            })?;
            Ok(market_wire::MarketBarStreamEnvelope {
                update: Some(market_bar_stream_envelope::Update::Snapshot(
                    market_wire::MarketBarSnapshot {
                        subscription_id: snapshot.subscription_id.clone(),
                        instrument: snapshot.instrument.clone(),
                        bars: bars.to_vec(),
                        first_sequence: snapshot.first_sequence,
                        last_sequence: snapshot.last_sequence,
                        session_generation: snapshot.session_generation,
                        publication_generation: snapshot.publication_generation,
                        checksum: snapshot.checksum.clone(),
                        schema_version: snapshot.schema_version,
                        snapshot_id: snapshot_id.clone(),
                        chunk_index,
                        chunk_count: chunk_count_u32,
                        total_item_count,
                    },
                )),
            })
        })
        .collect()
}

/// Encodes one validated replay delta into the provider-neutral stream envelope.
///
/// # Errors
///
/// Returns an error for an empty subscription or invalid value/provenance evidence.
pub fn try_encode_replay_delta_envelope(
    subscription_id: impl Into<String>,
    instrument: &InstrumentRevision,
    bar_definition: &BarDefinition,
    delta: &StreamDelta<ProvenancedMarketBar>,
    decimal_convention: &DecimalConvention,
) -> Result<market_wire::MarketBarStreamEnvelope, ProtobufAdapterError> {
    let subscription_id = subscription_id.into();
    if subscription_id.trim().is_empty() {
        return Err(ProtobufAdapterError::EmptySubscriptionId);
    }
    validate_provenanced_market_bar(delta.item()).map_err(ProtobufAdapterError::Application)?;
    Ok(market_wire::MarketBarStreamEnvelope {
        update: Some(market_bar_stream_envelope::Update::Delta(
            market_wire::MarketBarDelta {
                subscription_id,
                previous_sequence: delta.previous_sequence(),
                bar: Some(try_encode_market_bar(
                    delta.item(),
                    instrument,
                    bar_definition,
                    decimal_convention,
                )?),
            },
        )),
    })
}

fn try_encode_market_bar(
    item: &ProvenancedMarketBar,
    instrument: &InstrumentRevision,
    bar_definition: &BarDefinition,
    decimal_convention: &DecimalConvention,
) -> Result<market_wire::MarketBar, ProtobufAdapterError> {
    validate_provenanced_market_bar(item).map_err(ProtobufAdapterError::Application)?;
    instrument.validate()?;
    bar_definition.validate()?;
    let provenance = item.provenance();
    let semantic_class = market_wire::SemanticClass::try_from(provenance.semantic_class)
        .map_err(|_| ProtobufAdapterError::InvalidSemanticClass(provenance.semantic_class))?;
    if semantic_class == market_wire::SemanticClass::Unspecified {
        return Err(ProtobufAdapterError::InvalidSemanticClass(
            provenance.semantic_class,
        ));
    }
    let nic_source = market_wire::TimestampSource::try_from(provenance.nic_timestamp_source)
        .map_err(|_| {
            ProtobufAdapterError::InvalidTimestampSource(provenance.nic_timestamp_source)
        })?;
    if provenance.nic_receive_timestamp_unix_nanos.is_some()
        != (nic_source != market_wire::TimestampSource::Unspecified)
    {
        return Err(ProtobufAdapterError::NicTimestampProvenanceMismatch);
    }
    let bar = item.value();
    Ok(market_wire::MarketBar {
        header: Some(market_wire::MarketEventHeader {
            metadata: Some(common_wire::EventMetadata {
                event_id: provenance.event_id.clone(),
                event_time_unix_nanos: provenance.event_time_unix_nanos,
                publication_time_unix_nanos: provenance.publication_time_unix_nanos,
                producer: provenance.producer.clone(),
                schema_version: provenance.schema_version,
                correlation_id: provenance.correlation_id.clone(),
                causation_id: provenance.causation_id.clone(),
            }),
            instrument_id: instrument.instrument_id.as_str().to_string(),
            instrument_revision: instrument.revision,
            venue_id: instrument.venue_id.clone(),
            source_id: provenance.source_id.clone(),
            source_sequence: provenance.source_sequence,
            exchange_timestamp_unix_nanos: provenance.exchange_timestamp_unix_nanos,
            provider_receive_timestamp_unix_nanos: provenance.provider_receive_timestamp_unix_nanos,
            axiusflow_receive_timestamp_unix_nanos: provenance
                .axiusflow_receive_timestamp_unix_nanos,
            publication_timestamp_unix_nanos: provenance.publication_time_unix_nanos,
            correction_flags: provenance.correction_flags,
            quality_flags: provenance.quality_flags,
            session_generation: provenance.session_generation,
            nic_receive_timestamp_unix_nanos: provenance.nic_receive_timestamp_unix_nanos,
            normalized_timestamp_unix_nanos: provenance.normalized_timestamp_unix_nanos,
            nic_timestamp_source: provenance.nic_timestamp_source,
            semantic_class: provenance.semantic_class,
            entitlement_revision: provenance.entitlement_revision.clone(),
            fanout_enqueue_timestamp_unix_nanos: provenance.fanout_enqueue_timestamp_unix_nanos,
        }),
        interval_seconds: bar_definition.interval_seconds,
        open: Some(encode_decimal_i64(
            bar.open,
            instrument.precision.price_scale(),
            decimal_convention.price_unit(),
        )),
        high: Some(encode_decimal_i64(
            bar.high,
            instrument.precision.price_scale(),
            decimal_convention.price_unit(),
        )),
        low: Some(encode_decimal_i64(
            bar.low,
            instrument.precision.price_scale(),
            decimal_convention.price_unit(),
        )),
        close: Some(encode_decimal_i64(
            bar.close,
            instrument.precision.price_scale(),
            decimal_convention.price_unit(),
        )),
        volume: Some(encode_decimal_i64(
            bar.volume,
            instrument.precision.quantity_scale(),
            decimal_convention.quantity_unit(),
        )),
        bar_definition_id: bar_definition.definition_id.clone(),
        bar_definition_version: bar_definition.version,
    })
}

/// Encodes one Protobuf stream envelope into the bounded binary frame contract.
///
/// # Errors
///
/// Returns an error when the encoded payload exceeds the declared frame bound.
pub fn encode_market_bar_stream_frame(
    envelope: &market_wire::MarketBarStreamEnvelope,
    maximum_frame_bytes: NonZeroUsize,
) -> Result<Vec<u8>, BinaryMarketStreamError> {
    Ok(encode_binary_frame(
        &envelope.encode_to_vec(),
        maximum_frame_bytes,
    )?)
}
