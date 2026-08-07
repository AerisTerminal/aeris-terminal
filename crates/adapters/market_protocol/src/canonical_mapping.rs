//! Canonical realtime event mapping to validated market-bar domain values.
//!
//! This module owns the provider-neutral canonical OHLCV payload layout, its encode and
//! decode steps, semantic-class and timestamp-source translation, and projection of
//! canonical events and snapshots into provenanced domain bars.

use crate::NANOS_PER_SECOND;
use crate::errors::CanonicalMarketBarProjectionError;
use axiusflow_application::{
    ProvenancedMarketBar, ReplayProvenance, ReplaySnapshot, validate_provenanced_market_bar,
};
use axiusflow_instruments::InstrumentRevision;
use axiusflow_market_data::{BarDefinition, MarketBar};
use axiusflow_protocols::{
    MarketEventProvenance, MarketSnapshotIdentityRef, MarketValueChecksumRef, Provenanced,
    SnapshotEvidence, compute_market_snapshot_checksum,
    protobuf::axiusflow::market::v1 as market_wire,
};
use axiusflow_realtime::{
    CanonicalMarketEvent, CanonicalSnapshot, NicTimestampSource, SemanticClass,
};

/// Exact provider-neutral payload size: five signed big-endian OHLCV mantissas.
pub const CANONICAL_MARKET_BAR_PAYLOAD_BYTES: usize = 5 * size_of::<i64>();

/// Encodes only OHLCV mantissas; sequence and exchange time remain header-authoritative.
///
/// # Errors
///
/// Returns an error when the supplied bar violates domain OHLCV invariants.
pub fn try_encode_canonical_market_bar_payload(
    bar: &MarketBar,
) -> Result<[u8; CANONICAL_MARKET_BAR_PAYLOAD_BYTES], CanonicalMarketBarProjectionError> {
    bar.validate()?;
    let mut payload = [0_u8; CANONICAL_MARKET_BAR_PAYLOAD_BYTES];
    for (destination, value) in payload
        .chunks_exact_mut(size_of::<i64>())
        .zip([bar.open, bar.high, bar.low, bar.close, bar.volume])
    {
        destination.copy_from_slice(&value.to_be_bytes());
    }
    Ok(payload)
}

/// Projects one strict canonical OHLCV event without inventing identity or provenance.
///
/// # Errors
///
/// Returns an error for payload shape, series identity, timestamp precision,
/// semantic-class, NIC provenance, domain, or application-evidence violations.
pub fn try_project_canonical_market_bar(
    event: &CanonicalMarketEvent,
    instrument: &InstrumentRevision,
    bar_definition: &BarDefinition,
) -> Result<ProvenancedMarketBar, CanonicalMarketBarProjectionError> {
    instrument.validate()?;
    bar_definition.validate()?;
    let header = event.header();
    if header.instrument_id != instrument.instrument_id.as_str() {
        return Err(CanonicalMarketBarProjectionError::InstrumentIdMismatch {
            expected: instrument.instrument_id.as_str().to_string(),
            actual: header.instrument_id.clone(),
        });
    }
    if header.venue_id != instrument.venue_id {
        return Err(CanonicalMarketBarProjectionError::VenueIdMismatch {
            expected: instrument.venue_id.clone(),
            actual: header.venue_id.clone(),
        });
    }
    let series = header
        .series_identity
        .as_ref()
        .ok_or(CanonicalMarketBarProjectionError::MissingSeriesIdentity)?;
    for (field, matches) in [
        (
            "instrument_revision",
            series.instrument_revision == instrument.revision,
        ),
        (
            "definition_id",
            series.definition_id == bar_definition.definition_id,
        ),
        (
            "definition_version",
            series.definition_version == bar_definition.version,
        ),
        (
            "interval_seconds",
            series.interval_seconds == bar_definition.interval_seconds,
        ),
    ] {
        if !matches {
            return Err(CanonicalMarketBarProjectionError::SeriesIdentityMismatch(
                field,
            ));
        }
    }
    let semantic_class = canonical_market_bar_semantic_class(header.semantic_class)?;
    let nic_timestamp_source = canonical_nic_timestamp_source(
        header.timestamps.nic_receive_unix_nanos,
        header.nic_timestamp_source,
    )?;
    let [open, high, low, close, volume] = decode_canonical_market_bar_payload(event.payload())?;
    if header.timestamps.exchange_unix_nanos % NANOS_PER_SECOND != 0 {
        return Err(CanonicalMarketBarProjectionError::TimestampNotWholeSecond(
            header.timestamps.exchange_unix_nanos,
        ));
    }
    let bar = MarketBar {
        source_sequence: header.source_sequence,
        exchange_timestamp_seconds: header.timestamps.exchange_unix_nanos / NANOS_PER_SECOND,
        open,
        high,
        low,
        close,
        volume,
    };
    bar.validate()?;
    let item = Provenanced::new(
        bar,
        MarketEventProvenance {
            event_id: header.event_id.clone(),
            event_time_unix_nanos: header.event_time_unix_nanos,
            publication_time_unix_nanos: header.publication_time_unix_nanos,
            producer: header.producer.clone(),
            schema_version: header.schema_version,
            correlation_id: header.correlation_id.clone(),
            causation_id: header.causation_id.clone(),
            entitlement_revision: header.entitlement_revision.clone(),
            partition_id: header.partition_id,
            ownership_epoch: header.ownership_epoch,
            source_id: header.source_id.clone(),
            source_sequence: header.source_sequence,
            exchange_timestamp_unix_nanos: header.timestamps.exchange_unix_nanos,
            provider_receive_timestamp_unix_nanos: header.timestamps.provider_receive_unix_nanos,
            nic_receive_timestamp_unix_nanos: header.timestamps.nic_receive_unix_nanos,
            axiusflow_receive_timestamp_unix_nanos: header.timestamps.axiusflow_receive_unix_nanos,
            normalized_timestamp_unix_nanos: header.timestamps.normalized_unix_nanos,
            fanout_enqueue_timestamp_unix_nanos: header.timestamps.fanout_enqueue_unix_nanos,
            correction_flags: header.correction_flags,
            quality_flags: header.quality_flags,
            nic_timestamp_source,
            semantic_class,
        },
    );
    validate_provenanced_market_bar(&item)?;
    Ok(item)
}

fn canonical_market_bar_semantic_class(
    semantic_class: SemanticClass,
) -> Result<i32, CanonicalMarketBarProjectionError> {
    match semantic_class {
        SemanticClass::OrderedDelta => Ok(market_wire::SemanticClass::OrderedDelta as i32),
        SemanticClass::Snapshot => Ok(market_wire::SemanticClass::Snapshot as i32),
        unsupported => Err(CanonicalMarketBarProjectionError::UnsupportedSemanticClass(
            unsupported,
        )),
    }
}

fn canonical_nic_timestamp_source(
    timestamp: Option<i64>,
    source: Option<NicTimestampSource>,
) -> Result<i32, CanonicalMarketBarProjectionError> {
    match (timestamp, source) {
        (None, None) => Ok(market_wire::TimestampSource::Unspecified as i32),
        (Some(_), Some(NicTimestampSource::SocketSoftware)) => {
            Ok(market_wire::TimestampSource::SocketSoftware as i32)
        }
        (Some(_), Some(NicTimestampSource::KernelSoftware)) => {
            Ok(market_wire::TimestampSource::KernelSoftware as i32)
        }
        (Some(_), Some(NicTimestampSource::NicHardware)) => {
            Ok(market_wire::TimestampSource::NicHardware as i32)
        }
        (None, Some(_)) | (Some(_), None) => {
            Err(CanonicalMarketBarProjectionError::NicTimestampProvenanceMismatch)
        }
    }
}

fn decode_canonical_market_bar_payload(
    payload: &[u8],
) -> Result<[i64; 5], CanonicalMarketBarProjectionError> {
    if payload.len() != CANONICAL_MARKET_BAR_PAYLOAD_BYTES {
        return Err(CanonicalMarketBarProjectionError::InvalidPayloadLength {
            actual: payload.len(),
        });
    }
    let mut values = [0_i64; 5];
    for (destination, source) in values
        .iter_mut()
        .zip(payload.chunks_exact(size_of::<i64>()))
    {
        let bytes = <[u8; size_of::<i64>()]>::try_from(source).map_err(|_| {
            CanonicalMarketBarProjectionError::InvalidPayloadLength {
                actual: payload.len(),
            }
        })?;
        *destination = i64::from_be_bytes(bytes);
    }
    Ok(values)
}

/// Projects one checksum-verified canonical snapshot into the existing replay contract.
///
/// The market snapshot checksum is recomputed because its identity/value contract
/// intentionally differs from the canonical event snapshot checksum.
///
/// # Errors
///
/// Returns an error when any event cannot be projected or replay evidence diverges.
pub fn try_project_canonical_market_bar_snapshot(
    snapshot: &CanonicalSnapshot,
    instrument: &InstrumentRevision,
    replay_provenance: ReplayProvenance,
    bar_definition: &BarDefinition,
) -> Result<ReplaySnapshot, CanonicalMarketBarProjectionError> {
    instrument.validate()?;
    bar_definition.validate()?;
    let bars = snapshot
        .events()
        .iter()
        .map(|event| try_project_canonical_market_bar(event, instrument, bar_definition))
        .collect::<Result<Vec<_>, _>>()?;
    let descriptor = snapshot.descriptor();
    let mut evidence = SnapshotEvidence {
        partition_id: descriptor.partition_id,
        ownership_epoch: descriptor.ownership_epoch.get(),
        generation: descriptor.generation.get(),
        first_sequence: descriptor.first_sequence.get(),
        last_sequence: descriptor.last_sequence.get(),
        schema_version: descriptor.schema_version,
        checksum: [0; 32],
    };
    evidence.checksum = compute_market_snapshot_checksum(
        &evidence,
        MarketSnapshotIdentityRef {
            instrument_id: instrument.instrument_id.as_str(),
            instrument_revision: instrument.revision,
            bar_definition_id: &bar_definition.definition_id,
            bar_definition_version: bar_definition.version,
            bar_interval_seconds: bar_definition.interval_seconds,
            bar_trades_per_bar: bar_definition.trades_per_bar,
        },
        bars.iter().map(|item| {
            let bar = item.value();
            MarketValueChecksumRef {
                source_sequence: bar.source_sequence,
                exchange_timestamp_seconds: bar.exchange_timestamp_seconds,
                open: bar.open,
                high: bar.high,
                low: bar.low,
                close: bar.close,
                volume: bar.volume,
                provenance: item.provenance(),
            }
        }),
    );
    Ok(ReplaySnapshot::try_new_provenanced(
        instrument.clone(),
        replay_provenance,
        bar_definition.clone(),
        evidence,
        bars,
    )?)
}
