//! Validated codec between generated wire DTOs and domain values.
//!
//! This module owns instrument-revision, decimal, market-bar, snapshot, and delta
//! decoding plus their encoding counterparts, including header validation and snapshot
//! checksum verification. It rejects any wire value that cannot be represented in the
//! domain without loss.

use crate::errors::ProtobufAdapterError;
use crate::{DecimalConvention, DecodedMarketBarDelta, DecodedMarketBarSnapshot, NANOS_PER_SECOND};
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_market_data::{BarDefinition, MarketBar};
use axiusflow_protocols::{
    MarketEventProvenance, MarketSnapshotIdentityRef, MarketValueChecksumRef, Provenanced,
    SnapshotEvidence, StreamDelta, StreamProtocolError, StreamSnapshot,
    compute_market_snapshot_checksum,
    protobuf::axiusflow::{
        common::v1 as common_wire, instrument::v1 as instrument_wire, market::v1 as market_wire,
    },
};

/// Decodes and validates one generated instrument revision.
///
/// # Errors
///
/// Returns an error for unknown or unspecified enums, unrepresentable scales,
/// or any domain instrument invariant violation.
pub fn try_decode_instrument_revision(
    wire: instrument_wire::InstrumentRevision,
) -> Result<InstrumentRevision, ProtobufAdapterError> {
    let asset_class = match instrument_wire::AssetClass::try_from(wire.asset_class) {
        Ok(instrument_wire::AssetClass::Unspecified) => {
            return Err(ProtobufAdapterError::UnspecifiedAssetClass);
        }
        Ok(instrument_wire::AssetClass::Equity) => AssetClass::Equity,
        Ok(instrument_wire::AssetClass::Option) => AssetClass::Option,
        Ok(instrument_wire::AssetClass::Future) => AssetClass::Future,
        Ok(instrument_wire::AssetClass::ForeignExchange) => AssetClass::ForeignExchange,
        Ok(instrument_wire::AssetClass::CryptoAsset) => AssetClass::CryptoAsset,
        Ok(instrument_wire::AssetClass::FixedIncome) => AssetClass::FixedIncome,
        Ok(instrument_wire::AssetClass::Fund) => AssetClass::Fund,
        Ok(instrument_wire::AssetClass::Index) => AssetClass::Index,
        Err(_) => return Err(ProtobufAdapterError::UnknownAssetClass(wire.asset_class)),
    };
    let lifecycle = match instrument_wire::InstrumentLifecycle::try_from(wire.lifecycle) {
        Ok(instrument_wire::InstrumentLifecycle::Unspecified) => {
            return Err(ProtobufAdapterError::UnspecifiedInstrumentLifecycle);
        }
        Ok(instrument_wire::InstrumentLifecycle::Active) => InstrumentLifecycle::Active,
        Ok(instrument_wire::InstrumentLifecycle::Halted) => InstrumentLifecycle::Halted,
        Ok(instrument_wire::InstrumentLifecycle::Delisted) => InstrumentLifecycle::Delisted,
        Ok(instrument_wire::InstrumentLifecycle::Expired) => InstrumentLifecycle::Expired,
        Err(_) => {
            return Err(ProtobufAdapterError::UnknownInstrumentLifecycle(
                wire.lifecycle,
            ));
        }
    };
    let price_scale = u8::try_from(wire.price_scale).map_err(|_| {
        ProtobufAdapterError::DecimalScaleCannotFitDomain {
            field: "price_scale",
            value: wire.price_scale,
        }
    })?;
    let quantity_scale = u8::try_from(wire.quantity_scale).map_err(|_| {
        ProtobufAdapterError::DecimalScaleCannotFitDomain {
            field: "quantity_scale",
            value: wire.quantity_scale,
        }
    })?;

    let revision = InstrumentRevision {
        instrument_id: InstrumentId::try_new(wire.instrument_id)?,
        revision: wire.revision,
        asset_class,
        symbol: wire.symbol,
        venue_id: wire.venue_id,
        trading_currency: wire.trading_currency,
        precision: InstrumentPrecision::try_new(price_scale, quantity_scale)?,
        lifecycle,
    };
    revision.validate()?;
    Ok(revision)
}

/// Encodes one validated domain instrument revision into its generated DTO.
///
/// # Errors
///
/// Returns an error when the domain revision is invalid.
pub fn try_encode_instrument_revision(
    revision: &InstrumentRevision,
) -> Result<instrument_wire::InstrumentRevision, ProtobufAdapterError> {
    revision.validate()?;
    let asset_class = match revision.asset_class {
        AssetClass::Equity => instrument_wire::AssetClass::Equity,
        AssetClass::Option => instrument_wire::AssetClass::Option,
        AssetClass::Future => instrument_wire::AssetClass::Future,
        AssetClass::ForeignExchange => instrument_wire::AssetClass::ForeignExchange,
        AssetClass::CryptoAsset => instrument_wire::AssetClass::CryptoAsset,
        AssetClass::FixedIncome => instrument_wire::AssetClass::FixedIncome,
        AssetClass::Fund => instrument_wire::AssetClass::Fund,
        AssetClass::Index => instrument_wire::AssetClass::Index,
    };
    let lifecycle = match revision.lifecycle {
        InstrumentLifecycle::Active => instrument_wire::InstrumentLifecycle::Active,
        InstrumentLifecycle::Halted => instrument_wire::InstrumentLifecycle::Halted,
        InstrumentLifecycle::Delisted => instrument_wire::InstrumentLifecycle::Delisted,
        InstrumentLifecycle::Expired => instrument_wire::InstrumentLifecycle::Expired,
    };

    Ok(instrument_wire::InstrumentRevision {
        instrument_id: revision.instrument_id.as_str().to_string(),
        revision: revision.revision,
        asset_class: asset_class as i32,
        symbol: revision.symbol.clone(),
        venue_id: revision.venue_id.clone(),
        trading_currency: revision.trading_currency.clone(),
        price_scale: u32::from(revision.precision.price_scale()),
        quantity_scale: u32::from(revision.precision.quantity_scale()),
        lifecycle: lifecycle as i32,
    })
}

/// Decodes a signed big-endian two's-complement decimal mantissa without loss.
///
/// # Errors
///
/// Returns an error for an empty or out-of-range mantissa, or when the scale or
/// unit differs from the expected domain convention.
pub fn try_decode_decimal_i64(
    wire: common_wire::DecimalValue,
    expected_scale: u8,
    expected_unit: &str,
) -> Result<i64, ProtobufAdapterError> {
    if wire.mantissa.is_empty() {
        return Err(ProtobufAdapterError::DecimalMantissaEmpty);
    }
    if wire.scale != u32::from(expected_scale) {
        return Err(ProtobufAdapterError::DecimalScaleMismatch {
            expected: expected_scale,
            actual: wire.scale,
        });
    }
    if wire.unit != expected_unit {
        return Err(ProtobufAdapterError::DecimalUnitMismatch {
            expected: expected_unit.to_string(),
            actual: wire.unit,
        });
    }

    decode_twos_complement_i64(&wire.mantissa)
}

/// Encodes an `i64` as a minimal signed big-endian two's-complement decimal.
#[must_use]
pub fn encode_decimal_i64(
    value: i64,
    scale: u8,
    unit: impl Into<String>,
) -> common_wire::DecimalValue {
    let bytes = value.to_be_bytes();
    let mut first = 0;
    while first < bytes.len() - 1 {
        let redundant_positive = bytes[first] == 0 && bytes[first + 1] & 0x80 == 0;
        let redundant_negative = bytes[first] == 0xff && bytes[first + 1] & 0x80 != 0;
        if !redundant_positive && !redundant_negative {
            break;
        }
        first += 1;
    }
    common_wire::DecimalValue {
        mantissa: bytes[first..].to_vec(),
        scale: u32::from(scale),
        unit: unit.into(),
    }
}

/// Decodes one market bar using instrument precision and explicit unit rules.
///
/// All canonical timestamps, ordering, ownership, source, correction, and quality
/// provenance are returned beside the lean domain projection.
///
/// # Errors
///
/// Returns an error for missing nested messages, identity mismatches, lossy
/// timestamp or decimal values, or any market-data invariant violation.
pub fn try_decode_market_bar(
    wire: market_wire::MarketBar,
    instrument: &InstrumentRevision,
    decimal_convention: &DecimalConvention,
) -> Result<(BarDefinition, Provenanced<MarketBar>), ProtobufAdapterError> {
    instrument.validate()?;
    let header = wire
        .header
        .ok_or(ProtobufAdapterError::MissingField("bar.header"))?;
    validate_header(&header, instrument)?;

    let definition = BarDefinition {
        definition_id: wire.bar_definition_id,
        version: wire.bar_definition_version,
        interval_seconds: wire.interval_seconds,
        trades_per_bar: None,
    };
    definition.validate()?;

    let price_scale = instrument.precision.price_scale();
    let quantity_scale = instrument.precision.quantity_scale();
    let bar = MarketBar {
        source_sequence: header.source_sequence,
        exchange_timestamp_seconds: whole_seconds(header.exchange_timestamp_unix_nanos)?,
        exchange_timestamp_unix_nanos: header.exchange_timestamp_unix_nanos,
        open: try_decode_decimal_i64(
            required(wire.open, "bar.open")?,
            price_scale,
            decimal_convention.price_unit(),
        )?,
        high: try_decode_decimal_i64(
            required(wire.high, "bar.high")?,
            price_scale,
            decimal_convention.price_unit(),
        )?,
        low: try_decode_decimal_i64(
            required(wire.low, "bar.low")?,
            price_scale,
            decimal_convention.price_unit(),
        )?,
        close: try_decode_decimal_i64(
            required(wire.close, "bar.close")?,
            price_scale,
            decimal_convention.price_unit(),
        )?,
        volume: try_decode_decimal_i64(
            required(wire.volume, "bar.volume")?,
            quantity_scale,
            decimal_convention.quantity_unit(),
        )?,
    };
    bar.validate()?;
    let metadata = header
        .metadata
        .as_ref()
        .ok_or(ProtobufAdapterError::MissingField("bar.header.metadata"))?;
    let provenance = MarketEventProvenance {
        event_id: metadata.event_id.clone(),
        event_time_unix_nanos: metadata.event_time_unix_nanos,
        publication_time_unix_nanos: header.publication_timestamp_unix_nanos,
        producer: metadata.producer.clone(),
        schema_version: metadata.schema_version,
        correlation_id: metadata.correlation_id.clone(),
        causation_id: metadata.causation_id.clone(),
        entitlement_revision: header.entitlement_revision,
        session_generation: header.session_generation,
        source_id: header.source_id,
        source_sequence: header.source_sequence,
        exchange_timestamp_unix_nanos: header.exchange_timestamp_unix_nanos,
        provider_receive_timestamp_unix_nanos: header.provider_receive_timestamp_unix_nanos,
        nic_receive_timestamp_unix_nanos: header.nic_receive_timestamp_unix_nanos,
        axiusflow_receive_timestamp_unix_nanos: header.axiusflow_receive_timestamp_unix_nanos,
        normalized_timestamp_unix_nanos: header.normalized_timestamp_unix_nanos,
        fanout_enqueue_timestamp_unix_nanos: header.fanout_enqueue_timestamp_unix_nanos,
        correction_flags: header.correction_flags,
        quality_flags: header.quality_flags,
        nic_timestamp_source: header.nic_timestamp_source,
        semantic_class: header.semantic_class,
    };
    Ok((definition, Provenanced::new(bar, provenance)))
}

/// Decodes a bounded snapshot and validates every per-bar stream invariant.
///
/// # Errors
///
/// Returns an error for invalid subscription, instrument, bar, identity,
/// definition, sequence, timestamp, or snapshot bounds.
pub fn try_decode_market_bar_snapshot(
    wire: market_wire::MarketBarSnapshot,
    decimal_convention: &DecimalConvention,
) -> Result<DecodedMarketBarSnapshot, ProtobufAdapterError> {
    if wire.subscription_id.trim().is_empty() {
        return Err(ProtobufAdapterError::EmptySubscriptionId);
    }
    if wire.session_generation == 0 {
        return Err(ProtobufAdapterError::ZeroWireField(
            "snapshot.session_generation",
        ));
    }
    if wire.publication_generation == 0 {
        return Err(ProtobufAdapterError::ZeroWireField(
            "snapshot.publication_generation",
        ));
    }
    if wire.schema_version == 0 {
        return Err(ProtobufAdapterError::ZeroWireField(
            "snapshot.schema_version",
        ));
    }
    if wire.checksum.len() != 32 {
        return Err(ProtobufAdapterError::InvalidSnapshotChecksumLength(
            wire.checksum.len(),
        ));
    }
    let expected_checksum: [u8; 32] =
        wire.checksum.as_slice().try_into().map_err(|_| {
            ProtobufAdapterError::InvalidSnapshotChecksumLength(wire.checksum.len())
        })?;
    let evidence = SnapshotEvidence {
        session_generation: wire.session_generation,
        publication_generation: wire.publication_generation,
        first_sequence: wire.first_sequence,
        last_sequence: wire.last_sequence,
        schema_version: wire.schema_version,
        checksum: expected_checksum,
    };
    let instrument =
        try_decode_instrument_revision(required(wire.instrument, "snapshot.instrument")?)?;
    if wire.bars.is_empty() {
        return Err(StreamProtocolError::EmptySnapshot.into());
    }

    let mut definition = None;
    let mut bars = Vec::with_capacity(wire.bars.len());
    let mut previous_timestamp = None;
    for (index, wire_bar) in wire.bars.into_iter().enumerate() {
        let (bar_definition, item) =
            try_decode_market_bar(wire_bar, &instrument, decimal_convention)?;
        let bar = *item.value();
        if item.provenance().session_generation != wire.session_generation {
            return Err(ProtobufAdapterError::SnapshotSessionMismatch);
        }
        if let Some(expected_definition) = &definition {
            if expected_definition != &bar_definition {
                return Err(ProtobufAdapterError::BarDefinitionMismatch);
            }
        } else {
            definition = Some(bar_definition);
        }

        let offset =
            u64::try_from(index).map_err(|_| StreamProtocolError::ItemCountOverflow(index))?;
        let expected_sequence = wire
            .first_sequence
            .checked_add(offset)
            .ok_or(StreamProtocolError::SequenceOverflow)?;
        if bar.source_sequence != expected_sequence {
            return Err(ProtobufAdapterError::BarSequenceMismatch {
                expected: expected_sequence,
                actual: bar.source_sequence,
            });
        }
        if previous_timestamp
            .is_some_and(|timestamp| bar.exchange_timestamp_unix_nanos <= timestamp)
        {
            return Err(ProtobufAdapterError::NonIncreasingTimestamp {
                source_sequence: bar.source_sequence,
            });
        }
        previous_timestamp = Some(bar.exchange_timestamp_unix_nanos);
        bars.push(item);
    }

    let bar_definition = definition.ok_or(StreamProtocolError::EmptySnapshot)?;
    let stream = StreamSnapshot::try_new(wire.first_sequence, wire.last_sequence, bars)?;
    verify_market_snapshot_checksum(&evidence, &instrument, &bar_definition, &stream)?;
    Ok(DecodedMarketBarSnapshot {
        subscription_id: wire.subscription_id,
        instrument,
        bar_definition,
        stream,
        evidence,
    })
}

fn verify_market_snapshot_checksum(
    evidence: &SnapshotEvidence,
    instrument: &InstrumentRevision,
    bar_definition: &BarDefinition,
    stream: &StreamSnapshot<Provenanced<MarketBar>>,
) -> Result<(), ProtobufAdapterError> {
    let actual_checksum = compute_market_snapshot_checksum(
        evidence,
        MarketSnapshotIdentityRef {
            instrument_id: instrument.instrument_id.as_str(),
            instrument_revision: instrument.revision,
            bar_definition_id: &bar_definition.definition_id,
            bar_definition_version: bar_definition.version,
            bar_interval_seconds: bar_definition.interval_seconds,
            bar_trades_per_bar: bar_definition.trades_per_bar,
        },
        stream.items().iter().map(|item| {
            let bar = item.value();
            MarketValueChecksumRef {
                source_sequence: bar.source_sequence,
                exchange_timestamp_seconds: bar.exchange_timestamp_seconds,
                exchange_timestamp_unix_nanos: bar.exchange_timestamp_unix_nanos,
                open: bar.open,
                high: bar.high,
                low: bar.low,
                close: bar.close,
                volume: bar.volume,
                provenance: item.provenance(),
            }
        }),
    );
    if actual_checksum != evidence.checksum {
        return Err(ProtobufAdapterError::SnapshotChecksumMismatch);
    }
    Ok(())
}

/// Decodes one structurally contiguous delta against installed series context.
///
/// # Errors
///
/// Returns an error for invalid subscription, missing bar, identity or definition
/// mismatch, or non-contiguous declared and per-bar sequences.
pub fn try_decode_market_bar_delta(
    wire: market_wire::MarketBarDelta,
    instrument: &InstrumentRevision,
    expected_definition: &BarDefinition,
    decimal_convention: &DecimalConvention,
) -> Result<DecodedMarketBarDelta, ProtobufAdapterError> {
    if wire.subscription_id.trim().is_empty() {
        return Err(ProtobufAdapterError::EmptySubscriptionId);
    }
    let (bar_definition, item) = try_decode_market_bar(
        required(wire.bar, "delta.bar")?,
        instrument,
        decimal_convention,
    )?;
    if &bar_definition != expected_definition {
        return Err(ProtobufAdapterError::BarDefinitionMismatch);
    }
    let delta = StreamDelta::try_new(wire.previous_sequence, item.value().source_sequence, item)?;
    Ok(DecodedMarketBarDelta {
        subscription_id: wire.subscription_id,
        bar_definition,
        delta,
    })
}

fn required<T>(value: Option<T>, field: &'static str) -> Result<T, ProtobufAdapterError> {
    value.ok_or(ProtobufAdapterError::MissingField(field))
}

fn validate_header(
    header: &market_wire::MarketEventHeader,
    instrument: &InstrumentRevision,
) -> Result<(), ProtobufAdapterError> {
    let metadata = header
        .metadata
        .as_ref()
        .ok_or(ProtobufAdapterError::MissingField("bar.header.metadata"))?;
    if metadata.event_id.trim().is_empty() {
        return Err(ProtobufAdapterError::EmptyWireField(
            "bar.header.metadata.event_id",
        ));
    }
    if metadata.producer.trim().is_empty() {
        return Err(ProtobufAdapterError::EmptyWireField(
            "bar.header.metadata.producer",
        ));
    }
    if metadata.schema_version == 0 {
        return Err(ProtobufAdapterError::ZeroWireField(
            "bar.header.metadata.schema_version",
        ));
    }
    if header.session_generation == 0 {
        return Err(ProtobufAdapterError::ZeroWireField(
            "bar.header.session_generation",
        ));
    }
    if metadata.publication_time_unix_nanos != header.publication_timestamp_unix_nanos {
        return Err(ProtobufAdapterError::PublicationTimestampMismatch {
            metadata: metadata.publication_time_unix_nanos,
            header: header.publication_timestamp_unix_nanos,
        });
    }
    if header.normalized_timestamp_unix_nanos < header.axiusflow_receive_timestamp_unix_nanos {
        return Err(ProtobufAdapterError::CanonicalTimestampRegression);
    }
    if header
        .fanout_enqueue_timestamp_unix_nanos
        .is_some_and(|fanout| fanout < header.normalized_timestamp_unix_nanos)
    {
        return Err(ProtobufAdapterError::FanoutTimestampRegression);
    }
    if header.nic_receive_timestamp_unix_nanos.is_some()
        != (header.nic_timestamp_source != market_wire::TimestampSource::Unspecified as i32)
    {
        return Err(ProtobufAdapterError::NicTimestampProvenanceMismatch);
    }
    match market_wire::SemanticClass::try_from(header.semantic_class) {
        Ok(market_wire::SemanticClass::Unspecified) | Err(_) => {
            return Err(ProtobufAdapterError::InvalidSemanticClass(
                header.semantic_class,
            ));
        }
        Ok(_) => {}
    }
    if header.source_id.trim().is_empty() {
        return Err(ProtobufAdapterError::EmptyWireField("bar.header.source_id"));
    }
    if header.entitlement_revision.trim().is_empty() {
        return Err(ProtobufAdapterError::EmptyWireField(
            "bar.header.entitlement_revision",
        ));
    }
    if header.instrument_id != instrument.instrument_id.as_str() {
        return Err(ProtobufAdapterError::InstrumentIdMismatch {
            expected: instrument.instrument_id.as_str().to_string(),
            actual: header.instrument_id.clone(),
        });
    }
    if header.instrument_revision != instrument.revision {
        return Err(ProtobufAdapterError::InstrumentRevisionMismatch {
            expected: instrument.revision,
            actual: header.instrument_revision,
        });
    }
    if header.venue_id != instrument.venue_id {
        return Err(ProtobufAdapterError::VenueIdMismatch {
            expected: instrument.venue_id.clone(),
            actual: header.venue_id.clone(),
        });
    }
    Ok(())
}

fn whole_seconds(unix_nanos: i64) -> Result<i64, ProtobufAdapterError> {
    if unix_nanos % NANOS_PER_SECOND != 0 {
        return Err(ProtobufAdapterError::TimestampNotWholeSecond(unix_nanos));
    }
    Ok(unix_nanos / NANOS_PER_SECOND)
}

fn decode_twos_complement_i64(bytes: &[u8]) -> Result<i64, ProtobufAdapterError> {
    let negative = bytes[0] & 0x80 != 0;
    let extension = if negative { 0xff } else { 0 };
    let mut first = 0;
    while bytes.len() - first > i64::BITS as usize / 8 {
        let next_negative = bytes[first + 1] & 0x80 != 0;
        if bytes[first] != extension || next_negative != negative {
            return Err(ProtobufAdapterError::DecimalMantissaOutOfRange);
        }
        first += 1;
    }

    let significant = &bytes[first..];
    let mut decoded = [extension; i64::BITS as usize / 8];
    let destination = decoded.len() - significant.len();
    decoded[destination..].copy_from_slice(significant);
    Ok(i64::from_be_bytes(decoded))
}
