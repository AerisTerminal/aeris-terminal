//! Validated conversion boundary between generated market DTOs and domain values.
//!
//! The market-data domain intentionally stores the fixed-point values and whole-
//! second exchange timestamp needed by current application use cases. Conversion
//! therefore rejects wire values that cannot be represented without loss.

use axiusflow_application::{
    ProvenancedMarketBar, ReplayProvenance, ReplaySnapshot, ReplayStreamUpdate,
    ReplayValidationError, validate_provenanced_market_bar,
};
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
    InstrumentValidationError,
};
use axiusflow_market_data::{BarDefinition, MarketBar, MarketDataValidationError};
use axiusflow_protocols::{
    MAX_STREAM_SNAPSHOT_ITEMS, MarketEventProvenance, MarketSnapshotIdentityRef,
    MarketValueChecksumRef, Provenanced, SnapshotEvidence, StreamDelta, StreamProtocolError,
    StreamSnapshot, compute_market_snapshot_checksum,
    protobuf::axiusflow::{
        common::v1 as common_wire,
        instrument::v1 as instrument_wire,
        market::v1::{self as market_wire, market_bar_stream_envelope},
    },
};
use axiusflow_realtime::{
    CanonicalMarketEvent, CanonicalSnapshot, NicTimestampSource, SemanticClass,
};
use axiusflow_transport::{BinaryFrameError, BoundedBinaryFrameDecoder, encode_binary_frame};
use core::fmt;
use prost::Message;
use std::{error::Error, num::NonZeroUsize};

const NANOS_PER_SECOND: i64 = 1_000_000_000;

/// Maximum nonempty frames accepted for one atomic snapshot assembly.
pub const MAX_MARKET_BAR_SNAPSHOT_CHUNKS: usize = MAX_STREAM_SNAPSHOT_ITEMS;

/// Expected wire units for price and quantity decimals.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecimalConvention {
    price_unit: String,
    quantity_unit: String,
}

impl DecimalConvention {
    /// Creates an explicit unit convention for converting decimals to bare domain mantissas.
    ///
    /// # Errors
    ///
    /// Returns an error when either expected unit is empty.
    pub fn try_new(
        price_unit: impl Into<String>,
        quantity_unit: impl Into<String>,
    ) -> Result<Self, ProtobufAdapterError> {
        let price_unit = price_unit.into();
        let quantity_unit = quantity_unit.into();
        if price_unit.trim().is_empty() {
            return Err(ProtobufAdapterError::EmptyDecimalConventionUnit(
                "price_unit",
            ));
        }
        if quantity_unit.trim().is_empty() {
            return Err(ProtobufAdapterError::EmptyDecimalConventionUnit(
                "quantity_unit",
            ));
        }
        Ok(Self {
            price_unit,
            quantity_unit,
        })
    }

    /// Returns the required unit for OHLC values.
    #[must_use]
    pub fn price_unit(&self) -> &str {
        &self.price_unit
    }

    /// Returns the required unit for volume values.
    #[must_use]
    pub fn quantity_unit(&self) -> &str {
        &self.quantity_unit
    }
}

/// Exact provider-neutral payload size: five signed big-endian OHLCV mantissas.
pub const CANONICAL_MARKET_BAR_PAYLOAD_BYTES: usize = 5 * size_of::<i64>();

/// Projection failures between canonical realtime events and validated market bars.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CanonicalMarketBarProjectionError {
    InvalidPayloadLength { actual: usize },
    InstrumentIdMismatch { expected: String, actual: String },
    VenueIdMismatch { expected: String, actual: String },
    MissingSeriesIdentity,
    SeriesIdentityMismatch(&'static str),
    TimestampNotWholeSecond(i64),
    UnsupportedSemanticClass(SemanticClass),
    NicTimestampProvenanceMismatch,
    Instrument(InstrumentValidationError),
    MarketData(MarketDataValidationError),
    Stream(StreamProtocolError),
    Application(ReplayValidationError),
}

impl From<InstrumentValidationError> for CanonicalMarketBarProjectionError {
    fn from(error: InstrumentValidationError) -> Self {
        Self::Instrument(error)
    }
}

impl From<MarketDataValidationError> for CanonicalMarketBarProjectionError {
    fn from(error: MarketDataValidationError) -> Self {
        Self::MarketData(error)
    }
}

impl From<StreamProtocolError> for CanonicalMarketBarProjectionError {
    fn from(error: StreamProtocolError) -> Self {
        Self::Stream(error)
    }
}

impl From<ReplayValidationError> for CanonicalMarketBarProjectionError {
    fn from(error: ReplayValidationError) -> Self {
        Self::Application(error)
    }
}

impl fmt::Display for CanonicalMarketBarProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPayloadLength { actual } => write!(
                formatter,
                "canonical market-bar payload must be {CANONICAL_MARKET_BAR_PAYLOAD_BYTES} bytes; received {actual}"
            ),
            Self::InstrumentIdMismatch { expected, actual } => write!(
                formatter,
                "canonical instrument id mismatch: expected {expected}, received {actual}"
            ),
            Self::VenueIdMismatch { expected, actual } => write!(
                formatter,
                "canonical venue id mismatch: expected {expected}, received {actual}"
            ),
            Self::MissingSeriesIdentity => {
                formatter.write_str("canonical market-bar series identity is required")
            }
            Self::SeriesIdentityMismatch(field) => {
                write!(
                    formatter,
                    "canonical market-bar series identity mismatch: {field}"
                )
            }
            Self::TimestampNotWholeSecond(value) => write!(
                formatter,
                "canonical exchange timestamp {value} nanoseconds cannot be represented as whole seconds"
            ),
            Self::UnsupportedSemanticClass(class) => {
                write!(
                    formatter,
                    "unsupported canonical market-bar semantic class {class:?}"
                )
            }
            Self::NicTimestampProvenanceMismatch => formatter.write_str(
                "canonical NIC receive timestamp and timestamp source must be present together",
            ),
            Self::Instrument(error) => write!(formatter, "invalid projection instrument: {error}"),
            Self::MarketData(error) => write!(formatter, "invalid projected market bar: {error}"),
            Self::Stream(error) => write!(formatter, "invalid projected market stream: {error}"),
            Self::Application(error) => {
                write!(
                    formatter,
                    "invalid projected application market bar: {error}"
                )
            }
        }
    }
}

impl Error for CanonicalMarketBarProjectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Instrument(error) => Some(error),
            Self::MarketData(error) => Some(error),
            Self::Stream(error) => Some(error),
            Self::Application(error) => Some(error),
            Self::InvalidPayloadLength { .. }
            | Self::InstrumentIdMismatch { .. }
            | Self::VenueIdMismatch { .. }
            | Self::MissingSeriesIdentity
            | Self::SeriesIdentityMismatch(_)
            | Self::TimestampNotWholeSecond(_)
            | Self::UnsupportedSemanticClass(_)
            | Self::NicTimestampProvenanceMismatch => None,
        }
    }
}

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

/// A decoded snapshot with validated subscription and series context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedMarketBarSnapshot {
    pub subscription_id: String,
    pub instrument: InstrumentRevision,
    pub bar_definition: BarDefinition,
    pub stream: StreamSnapshot<Provenanced<MarketBar>>,
    pub evidence: SnapshotEvidence,
}

/// A decoded delta with its validated series definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedMarketBarDelta {
    pub subscription_id: String,
    pub bar_definition: BarDefinition,
    pub delta: StreamDelta<Provenanced<MarketBar>>,
}

/// A validated market-bar stream update projected into domain values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodedMarketBarStreamUpdate {
    Snapshot(DecodedMarketBarSnapshot),
    Delta(DecodedMarketBarDelta),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MarketBarStreamContext {
    subscription_id: String,
    instrument: InstrumentRevision,
    bar_definition: BarDefinition,
    partition_id: u32,
    ownership_epoch: u64,
    generation: u64,
    schema_version: u32,
    last_sequence: u64,
    last_exchange_timestamp_seconds: i64,
}

#[derive(Clone, Debug, PartialEq)]
struct PendingMarketBarSnapshot {
    subscription_id: String,
    snapshot_id: String,
    instrument: Option<instrument_wire::InstrumentRevision>,
    first_sequence: u64,
    last_sequence: u64,
    partition_id: u32,
    ownership_epoch: u64,
    generation: u64,
    checksum: Vec<u8>,
    schema_version: u32,
    chunk_count: u32,
    total_item_count: usize,
    next_chunk_index: u32,
    bars: Vec<market_wire::MarketBar>,
}

impl PendingMarketBarSnapshot {
    fn starts_with(wire: market_wire::MarketBarSnapshot) -> Self {
        let bar_capacity = usize::try_from(wire.total_item_count).unwrap_or(usize::MAX);
        let mut bars = Vec::with_capacity(bar_capacity);
        bars.extend(wire.bars);
        Self {
            subscription_id: wire.subscription_id,
            snapshot_id: wire.snapshot_id,
            instrument: wire.instrument,
            first_sequence: wire.first_sequence,
            last_sequence: wire.last_sequence,
            partition_id: wire.partition_id,
            ownership_epoch: wire.ownership_epoch,
            generation: wire.generation,
            checksum: wire.checksum,
            schema_version: wire.schema_version,
            chunk_count: wire.chunk_count,
            total_item_count: bar_capacity,
            next_chunk_index: 1,
            bars,
        }
    }

    fn matches(&self, wire: &market_wire::MarketBarSnapshot) -> bool {
        self.subscription_id == wire.subscription_id
            && self.snapshot_id == wire.snapshot_id
            && self.instrument == wire.instrument
            && self.first_sequence == wire.first_sequence
            && self.last_sequence == wire.last_sequence
            && self.partition_id == wire.partition_id
            && self.ownership_epoch == wire.ownership_epoch
            && self.generation == wire.generation
            && self.checksum == wire.checksum
            && self.schema_version == wire.schema_version
            && self.chunk_count == wire.chunk_count
            && u32::try_from(self.total_item_count).ok() == Some(wire.total_item_count)
    }

    fn into_complete_wire(self) -> market_wire::MarketBarSnapshot {
        market_wire::MarketBarSnapshot {
            subscription_id: self.subscription_id,
            instrument: self.instrument,
            bars: self.bars,
            first_sequence: self.first_sequence,
            last_sequence: self.last_sequence,
            partition_id: self.partition_id,
            ownership_epoch: self.ownership_epoch,
            generation: self.generation,
            checksum: self.checksum,
            schema_version: self.schema_version,
            snapshot_id: String::new(),
            chunk_index: 0,
            chunk_count: 0,
            total_item_count: 0,
        }
    }
}

fn validate_snapshot_chunk(
    wire: &market_wire::MarketBarSnapshot,
) -> Result<(usize, usize), ProtobufAdapterError> {
    if wire.snapshot_id.trim().is_empty() {
        return Err(ProtobufAdapterError::EmptySnapshotId);
    }
    let chunk_count = usize::try_from(wire.chunk_count).map_err(|_| {
        ProtobufAdapterError::SnapshotChunkCountLimitExceeded {
            actual: usize::MAX,
            maximum: MAX_MARKET_BAR_SNAPSHOT_CHUNKS,
        }
    })?;
    if chunk_count == 0 || chunk_count > MAX_MARKET_BAR_SNAPSHOT_CHUNKS {
        return Err(ProtobufAdapterError::SnapshotChunkCountLimitExceeded {
            actual: chunk_count,
            maximum: MAX_MARKET_BAR_SNAPSHOT_CHUNKS,
        });
    }
    if wire.chunk_index >= wire.chunk_count {
        return Err(ProtobufAdapterError::SnapshotChunkIndexOutOfRange {
            index: wire.chunk_index,
            count: wire.chunk_count,
        });
    }
    let total_item_count = usize::try_from(wire.total_item_count).map_err(|_| {
        ProtobufAdapterError::SnapshotTotalItemCountInvalid {
            actual: usize::MAX,
            maximum: MAX_STREAM_SNAPSHOT_ITEMS,
        }
    })?;
    if total_item_count == 0 || total_item_count > MAX_STREAM_SNAPSHOT_ITEMS {
        return Err(ProtobufAdapterError::SnapshotTotalItemCountInvalid {
            actual: total_item_count,
            maximum: MAX_STREAM_SNAPSHOT_ITEMS,
        });
    }
    if chunk_count > total_item_count {
        return Err(ProtobufAdapterError::SnapshotChunkCountExceedsItems {
            chunks: chunk_count,
            items: total_item_count,
        });
    }
    if wire.bars.is_empty() {
        return Err(ProtobufAdapterError::EmptySnapshotChunk);
    }
    let range_count = wire
        .last_sequence
        .checked_sub(wire.first_sequence)
        .and_then(|difference| difference.checked_add(1))
        .ok_or(StreamProtocolError::InvalidSequenceRange {
            first: wire.first_sequence,
            last: wire.last_sequence,
        })?;
    if range_count != u64::from(wire.total_item_count) {
        return Err(StreamProtocolError::ItemCountMismatch {
            expected: range_count,
            actual: total_item_count,
        }
        .into());
    }
    Ok((chunk_count, total_item_count))
}

fn accept_snapshot_chunk(
    pending: &mut Option<PendingMarketBarSnapshot>,
    wire: market_wire::MarketBarSnapshot,
) -> Result<Option<market_wire::MarketBarSnapshot>, ProtobufAdapterError> {
    let (chunk_count, total_item_count) = validate_snapshot_chunk(&wire)?;
    if let Some(assembly) = pending.as_mut() {
        if !assembly.matches(&wire) {
            return Err(ProtobufAdapterError::SnapshotChunkIdentityMismatch);
        }
        if wire.chunk_index != assembly.next_chunk_index {
            return Err(ProtobufAdapterError::UnexpectedSnapshotChunkIndex {
                expected: assembly.next_chunk_index,
                actual: wire.chunk_index,
            });
        }
        let resulting_items = assembly
            .bars
            .len()
            .checked_add(wire.bars.len())
            .ok_or(ProtobufAdapterError::SnapshotAssemblyItemLimitExceeded)?;
        let is_final = wire.chunk_index.checked_add(1) == Some(wire.chunk_count);
        if resulting_items > total_item_count
            || (is_final && resulting_items != total_item_count)
            || (!is_final && resulting_items >= total_item_count)
        {
            return Err(ProtobufAdapterError::SnapshotAssemblyItemCountMismatch {
                expected: total_item_count,
                actual: resulting_items,
            });
        }
        assembly.bars.extend(wire.bars);
        assembly.next_chunk_index = assembly
            .next_chunk_index
            .checked_add(1)
            .ok_or(ProtobufAdapterError::SnapshotAssemblyItemLimitExceeded)?;
    } else {
        if wire.chunk_index != 0 {
            return Err(ProtobufAdapterError::UnexpectedSnapshotChunkIndex {
                expected: 0,
                actual: wire.chunk_index,
            });
        }
        let is_final = chunk_count == 1;
        if wire.bars.len() > total_item_count
            || (is_final && wire.bars.len() != total_item_count)
            || (!is_final && wire.bars.len() >= total_item_count)
        {
            return Err(ProtobufAdapterError::SnapshotAssemblyItemCountMismatch {
                expected: total_item_count,
                actual: wire.bars.len(),
            });
        }
        *pending = Some(PendingMarketBarSnapshot::starts_with(wire));
    }

    let complete = pending
        .as_ref()
        .is_some_and(|assembly| assembly.next_chunk_index == assembly.chunk_count);
    if !complete {
        return Ok(None);
    }
    let assembly = pending
        .take()
        .ok_or(ProtobufAdapterError::EmptySnapshotChunk)?;
    if assembly.bars.len() != assembly.total_item_count {
        return Err(ProtobufAdapterError::SnapshotAssemblyItemCountMismatch {
            expected: assembly.total_item_count,
            actual: assembly.bars.len(),
        });
    }
    Ok(Some(assembly.into_complete_wire()))
}

/// Stateful decoder that enforces snapshot-before-delta stream semantics.
#[derive(Clone, Debug, PartialEq)]
pub struct MarketBarStreamDecoder {
    decimal_convention: DecimalConvention,
    context: Option<MarketBarStreamContext>,
    pending_snapshot: Option<PendingMarketBarSnapshot>,
}

impl MarketBarStreamDecoder {
    /// Creates a decoder with explicit decimal-unit expectations.
    #[must_use]
    pub const fn new(decimal_convention: DecimalConvention) -> Self {
        Self {
            decimal_convention,
            context: None,
            pending_snapshot: None,
        }
    }

    /// Decodes one generated stream envelope and advances accepted stream state.
    ///
    /// Chunked snapshots return `Ok(None)` until the final chunk validates and
    /// installs one complete snapshot atomically.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed snapshot assembly, a missing update, invalid
    /// snapshot or delta, a delta before a snapshot, or inconsistent stream state.
    pub fn try_decode(
        &mut self,
        envelope: market_wire::MarketBarStreamEnvelope,
    ) -> Result<Option<DecodedMarketBarStreamUpdate>, ProtobufAdapterError> {
        match envelope.update.ok_or(ProtobufAdapterError::MissingUpdate)? {
            market_bar_stream_envelope::Update::Snapshot(snapshot) => {
                if snapshot.chunk_count == 0 {
                    if self.pending_snapshot.is_some() {
                        return Err(ProtobufAdapterError::SnapshotDuringSnapshotAssembly);
                    }
                    let decoded =
                        try_decode_market_bar_snapshot(snapshot, &self.decimal_convention)?;
                    self.install_snapshot_context(&decoded)?;
                    return Ok(Some(DecodedMarketBarStreamUpdate::Snapshot(decoded)));
                }
                let mut candidate = self.pending_snapshot.clone();
                let complete = accept_snapshot_chunk(&mut candidate, snapshot)?;
                let Some(complete) = complete else {
                    self.pending_snapshot = candidate;
                    return Ok(None);
                };
                let decoded = try_decode_market_bar_snapshot(complete, &self.decimal_convention)?;
                self.install_snapshot_context(&decoded)?;
                self.pending_snapshot = None;
                Ok(Some(DecodedMarketBarStreamUpdate::Snapshot(decoded)))
            }
            market_bar_stream_envelope::Update::Delta(delta) => {
                if self.pending_snapshot.is_some() {
                    return Err(ProtobufAdapterError::DeltaDuringSnapshotAssembly);
                }
                let context = self
                    .context
                    .as_ref()
                    .ok_or(ProtobufAdapterError::DeltaBeforeSnapshot)?;
                let decoded = try_decode_market_bar_delta(
                    delta,
                    &context.instrument,
                    &context.bar_definition,
                    &self.decimal_convention,
                )?;
                if decoded.subscription_id != context.subscription_id {
                    return Err(ProtobufAdapterError::SubscriptionMismatch {
                        expected: context.subscription_id.clone(),
                        actual: decoded.subscription_id,
                    });
                }
                if decoded.delta.item().provenance().partition_id != context.partition_id
                    || decoded.delta.item().provenance().ownership_epoch != context.ownership_epoch
                {
                    return Err(ProtobufAdapterError::StreamOwnershipChanged);
                }
                let actual_schema = decoded.delta.item().provenance().schema_version;
                if actual_schema != context.schema_version {
                    return Err(ProtobufAdapterError::StreamSchemaChanged {
                        expected: context.schema_version,
                        actual: actual_schema,
                    });
                }
                if decoded.delta.previous_sequence() != context.last_sequence {
                    return Err(ProtobufAdapterError::UnexpectedPreviousSequence {
                        expected: context.last_sequence,
                        actual: decoded.delta.previous_sequence(),
                    });
                }
                if decoded.delta.item().value().exchange_timestamp_seconds
                    <= context.last_exchange_timestamp_seconds
                {
                    return Err(ProtobufAdapterError::NonIncreasingTimestamp {
                        source_sequence: decoded.delta.sequence(),
                    });
                }

                let Some(next_context) = self.context.as_mut() else {
                    return Err(ProtobufAdapterError::DeltaBeforeSnapshot);
                };
                next_context.last_sequence = decoded.delta.sequence();
                next_context.last_exchange_timestamp_seconds =
                    decoded.delta.item().value().exchange_timestamp_seconds;
                Ok(Some(DecodedMarketBarStreamUpdate::Delta(decoded)))
            }
        }
    }

    fn install_snapshot_context(
        &mut self,
        decoded: &DecodedMarketBarSnapshot,
    ) -> Result<(), ProtobufAdapterError> {
        if let Some(context) = &self.context {
            if decoded.subscription_id != context.subscription_id {
                return Err(ProtobufAdapterError::SubscriptionMismatch {
                    expected: context.subscription_id.clone(),
                    actual: decoded.subscription_id.clone(),
                });
            }
            if decoded.instrument != context.instrument
                || decoded.bar_definition != context.bar_definition
            {
                return Err(ProtobufAdapterError::SnapshotSeriesChanged);
            }
            if decoded.evidence.partition_id != context.partition_id {
                return Err(ProtobufAdapterError::SnapshotPartitionChanged);
            }
            if decoded.evidence.ownership_epoch < context.ownership_epoch {
                return Err(ProtobufAdapterError::SnapshotOwnershipRegression {
                    current: context.ownership_epoch,
                    actual: decoded.evidence.ownership_epoch,
                });
            }
            if decoded.evidence.ownership_epoch == context.ownership_epoch
                && (decoded.evidence.generation <= context.generation
                    || decoded.evidence.last_sequence < context.last_sequence)
            {
                return Err(ProtobufAdapterError::StaleSnapshotTransition {
                    current_generation: context.generation,
                    current_last_sequence: context.last_sequence,
                    actual_generation: decoded.evidence.generation,
                    actual_last_sequence: decoded.evidence.last_sequence,
                });
            }
        }
        let last_item = decoded
            .stream
            .items()
            .last()
            .ok_or(StreamProtocolError::EmptySnapshot)?;
        self.context = Some(MarketBarStreamContext {
            subscription_id: decoded.subscription_id.clone(),
            instrument: decoded.instrument.clone(),
            bar_definition: decoded.bar_definition.clone(),
            partition_id: last_item.provenance().partition_id,
            ownership_epoch: last_item.provenance().ownership_epoch,
            generation: decoded.evidence.generation,
            schema_version: decoded.evidence.schema_version,
            last_sequence: decoded.stream.last_sequence(),
            last_exchange_timestamp_seconds: last_item.value().exchange_timestamp_seconds,
        });
        Ok(())
    }

    /// Clears installed and partially assembled snapshot state.
    pub fn reset(&mut self) {
        self.context = None;
        self.pending_snapshot = None;
    }
}

/// One validated application update retaining its client subscription identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedMarketBarUpdate {
    pub subscription_id: String,
    pub update: ReplayStreamUpdate,
}

/// Bounded binary fixture/client decoder from transport chunks to application updates.
///
/// This is transport framing and semantic validation only; it does not claim a
/// connected WebSocket, live provider, entitlement service, or production readiness.
#[derive(Clone, Debug, PartialEq)]
pub struct BinaryMarketBarStreamDecoder {
    frames: BoundedBinaryFrameDecoder,
    stream: MarketBarStreamDecoder,
    provenance: ReplayProvenance,
    invalid: bool,
}

impl BinaryMarketBarStreamDecoder {
    /// Creates an explicitly bounded binary stream decoder.
    ///
    /// # Errors
    ///
    /// Returns an error if the framing buffer cannot hold one maximum frame.
    pub fn try_new(
        decimal_convention: DecimalConvention,
        provenance: ReplayProvenance,
        maximum_frame_bytes: NonZeroUsize,
        maximum_buffered_bytes: NonZeroUsize,
    ) -> Result<Self, BinaryMarketStreamError> {
        Ok(Self {
            frames: BoundedBinaryFrameDecoder::try_new(
                maximum_frame_bytes,
                maximum_buffered_bytes,
            )?,
            stream: MarketBarStreamDecoder::new(decimal_convention),
            provenance,
            invalid: false,
        })
    }

    /// Decodes fragmented or coalesced binary frames atomically into application updates.
    ///
    /// Any framing, Protobuf, adapter, or application error latches the decoder invalid.
    /// Call [`Self::reset`] only after reconnect or an explicit resnapshot boundary.
    ///
    /// # Errors
    ///
    /// Returns the first validation failure or a reset-required error after a prior fault.
    pub fn push(
        &mut self,
        chunk: &[u8],
    ) -> Result<Vec<ProjectedMarketBarUpdate>, BinaryMarketStreamError> {
        if self.invalid {
            return Err(BinaryMarketStreamError::ResetRequired);
        }
        let frames = match self.frames.push(chunk) {
            Ok(frames) => frames,
            Err(error) => {
                self.invalid = true;
                return Err(error.into());
            }
        };
        let mut candidate = self.stream.clone();
        let decoded = frames
            .into_iter()
            .try_fold(Vec::new(), |mut updates, frame| {
                let envelope = market_wire::MarketBarStreamEnvelope::decode(frame.as_slice())?;
                if let Some(update) = candidate.try_decode(envelope)? {
                    updates.push(project_market_bar_update(update, self.provenance)?);
                }
                Ok::<_, BinaryMarketStreamError>(updates)
            });
        match decoded {
            Ok(updates) => {
                self.stream = candidate;
                Ok(updates)
            }
            Err(error) => {
                self.invalid = true;
                Err(error)
            }
        }
    }

    /// Clears partial transport and semantic state after explicit reconnect/recovery.
    pub fn reset(&mut self) {
        self.frames.reset();
        self.stream.reset();
        self.invalid = false;
    }

    #[must_use]
    pub const fn is_invalid(&self) -> bool {
        self.invalid
    }

    #[must_use]
    pub const fn buffered_bytes(&self) -> usize {
        self.frames.buffered_bytes()
    }
}

fn project_market_bar_update(
    decoded: DecodedMarketBarStreamUpdate,
    provenance: ReplayProvenance,
) -> Result<ProjectedMarketBarUpdate, BinaryMarketStreamError> {
    match decoded {
        DecodedMarketBarStreamUpdate::Snapshot(snapshot) => {
            let DecodedMarketBarSnapshot {
                subscription_id,
                instrument,
                bar_definition,
                stream,
                evidence,
            } = snapshot;
            let replay = ReplaySnapshot::try_new_provenanced(
                instrument,
                provenance,
                bar_definition,
                evidence,
                stream.items().to_vec(),
            )?;
            Ok(ProjectedMarketBarUpdate {
                subscription_id,
                update: ReplayStreamUpdate::Snapshot(replay),
            })
        }
        DecodedMarketBarStreamUpdate::Delta(delta) => {
            validate_provenanced_market_bar(delta.delta.item())?;
            Ok(ProjectedMarketBarUpdate {
                subscription_id: delta.subscription_id,
                update: ReplayStreamUpdate::Delta(delta.delta),
            })
        }
    }
}

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
                partition_id: evidence.partition_id,
                ownership_epoch: evidence.ownership_epoch,
                generation: evidence.generation,
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
                        partition_id: snapshot.partition_id,
                        ownership_epoch: snapshot.ownership_epoch,
                        generation: snapshot.generation,
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

/// Projects one canonical ordered delta directly into the provider-neutral stream envelope.
///
/// # Errors
///
/// Returns an error for a non-delta semantic class, invalid canonical projection,
/// non-contiguous sequence evidence, or invalid wire context.
pub fn try_encode_canonical_market_bar_delta_envelope(
    subscription_id: impl Into<String>,
    instrument: &InstrumentRevision,
    bar_definition: &BarDefinition,
    previous_sequence: u64,
    event: &CanonicalMarketEvent,
    decimal_convention: &DecimalConvention,
) -> Result<market_wire::MarketBarStreamEnvelope, ProtobufAdapterError> {
    if event.header().semantic_class != SemanticClass::OrderedDelta {
        return Err(ProtobufAdapterError::CanonicalProjection(
            CanonicalMarketBarProjectionError::UnsupportedSemanticClass(
                event.header().semantic_class,
            ),
        ));
    }
    let item = try_project_canonical_market_bar(event, instrument, bar_definition)
        .map_err(ProtobufAdapterError::CanonicalProjection)?;
    let delta = StreamDelta::try_new(previous_sequence, event.header().source_sequence, item)?;
    try_encode_replay_delta_envelope(
        subscription_id,
        instrument,
        bar_definition,
        &delta,
        decimal_convention,
    )
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
                partition_id: provenance.partition_id,
                ownership_epoch: provenance.ownership_epoch,
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
            partition_id: provenance.partition_id,
            ownership_epoch: provenance.ownership_epoch,
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

/// Failures in bounded binary framing, Protobuf decoding, or semantic projection.
#[derive(Debug)]
pub enum BinaryMarketStreamError {
    Framing(BinaryFrameError),
    ProtobufDecode(prost::DecodeError),
    Adapter(ProtobufAdapterError),
    Application(ReplayValidationError),
    ResetRequired,
}

impl From<BinaryFrameError> for BinaryMarketStreamError {
    fn from(error: BinaryFrameError) -> Self {
        Self::Framing(error)
    }
}

impl From<prost::DecodeError> for BinaryMarketStreamError {
    fn from(error: prost::DecodeError) -> Self {
        Self::ProtobufDecode(error)
    }
}

impl From<ProtobufAdapterError> for BinaryMarketStreamError {
    fn from(error: ProtobufAdapterError) -> Self {
        Self::Adapter(error)
    }
}

impl From<ReplayValidationError> for BinaryMarketStreamError {
    fn from(error: ReplayValidationError) -> Self {
        Self::Application(error)
    }
}

impl fmt::Display for BinaryMarketStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Framing(error) => write!(formatter, "market stream framing failed: {error}"),
            Self::ProtobufDecode(error) => {
                write!(formatter, "market stream Protobuf decode failed: {error}")
            }
            Self::Adapter(error) => write!(formatter, "market stream validation failed: {error}"),
            Self::Application(error) => {
                write!(
                    formatter,
                    "market stream application projection failed: {error}"
                )
            }
            Self::ResetRequired => {
                formatter.write_str("market stream requires explicit reconnect or reset")
            }
        }
    }
}

impl Error for BinaryMarketStreamError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Framing(error) => Some(error),
            Self::ProtobufDecode(error) => Some(error),
            Self::Adapter(error) => Some(error),
            Self::Application(error) => Some(error),
            Self::ResetRequired => None,
        }
    }
}

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
    };
    definition.validate()?;

    let price_scale = instrument.precision.price_scale();
    let quantity_scale = instrument.precision.quantity_scale();
    let bar = MarketBar {
        source_sequence: header.source_sequence,
        exchange_timestamp_seconds: whole_seconds(header.exchange_timestamp_unix_nanos)?,
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
        partition_id: header.partition_id,
        ownership_epoch: header.ownership_epoch,
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
    if wire.ownership_epoch == 0 {
        return Err(ProtobufAdapterError::ZeroWireField(
            "snapshot.ownership_epoch",
        ));
    }
    if wire.generation == 0 {
        return Err(ProtobufAdapterError::ZeroWireField("snapshot.generation"));
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
        partition_id: wire.partition_id,
        ownership_epoch: wire.ownership_epoch,
        generation: wire.generation,
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
        if item.provenance().partition_id != wire.partition_id
            || item.provenance().ownership_epoch != wire.ownership_epoch
        {
            return Err(ProtobufAdapterError::SnapshotOwnershipMismatch);
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
        if previous_timestamp.is_some_and(|timestamp| bar.exchange_timestamp_seconds <= timestamp) {
            return Err(ProtobufAdapterError::NonIncreasingTimestamp {
                source_sequence: bar.source_sequence,
            });
        }
        previous_timestamp = Some(bar.exchange_timestamp_seconds);
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
        },
        stream.items().iter().map(|item| {
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
    if metadata.ownership_epoch == 0 || header.ownership_epoch == 0 {
        return Err(ProtobufAdapterError::ZeroWireField(
            "bar.header.ownership_epoch",
        ));
    }
    if metadata.partition_id != header.partition_id {
        return Err(ProtobufAdapterError::PartitionMetadataMismatch {
            metadata: metadata.partition_id,
            header: header.partition_id,
        });
    }
    if metadata.ownership_epoch != header.ownership_epoch {
        return Err(ProtobufAdapterError::OwnershipEpochMetadataMismatch {
            metadata: metadata.ownership_epoch,
            header: header.ownership_epoch,
        });
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

/// Failures while validating generated market DTOs at the domain boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtobufAdapterError {
    MissingField(&'static str),
    MissingUpdate,
    EmptyWireField(&'static str),
    ZeroWireField(&'static str),
    EmptySubscriptionId,
    EmptySnapshotId,
    SnapshotChunkCountLimitExceeded {
        actual: usize,
        maximum: usize,
    },
    SnapshotChunkCountExceedsItems {
        chunks: usize,
        items: usize,
    },
    SnapshotChunkIndexOutOfRange {
        index: u32,
        count: u32,
    },
    UnexpectedSnapshotChunkIndex {
        expected: u32,
        actual: u32,
    },
    EmptySnapshotChunk,
    SnapshotChunkIdentityMismatch,
    SnapshotTotalItemCountInvalid {
        actual: usize,
        maximum: usize,
    },
    SnapshotAssemblyItemLimitExceeded,
    SnapshotAssemblyItemCountMismatch {
        expected: usize,
        actual: usize,
    },
    DeltaDuringSnapshotAssembly,
    SnapshotDuringSnapshotAssembly,
    EmptyDecimalConventionUnit(&'static str),
    UnknownAssetClass(i32),
    UnspecifiedAssetClass,
    UnknownInstrumentLifecycle(i32),
    UnspecifiedInstrumentLifecycle,
    DecimalScaleCannotFitDomain {
        field: &'static str,
        value: u32,
    },
    DecimalMantissaEmpty,
    DecimalMantissaOutOfRange,
    DecimalScaleMismatch {
        expected: u8,
        actual: u32,
    },
    DecimalUnitMismatch {
        expected: String,
        actual: String,
    },
    TimestampNotWholeSecond(i64),
    InstrumentIdMismatch {
        expected: String,
        actual: String,
    },
    InstrumentRevisionMismatch {
        expected: u64,
        actual: u64,
    },
    VenueIdMismatch {
        expected: String,
        actual: String,
    },
    PartitionMetadataMismatch {
        metadata: u32,
        header: u32,
    },
    OwnershipEpochMetadataMismatch {
        metadata: u64,
        header: u64,
    },
    PublicationTimestampMismatch {
        metadata: i64,
        header: i64,
    },
    CanonicalTimestampRegression,
    FanoutTimestampRegression,
    NicTimestampProvenanceMismatch,
    InvalidTimestampSource(i32),
    InvalidSemanticClass(i32),
    InvalidSnapshotChecksumLength(usize),
    SnapshotChecksumMismatch,
    SnapshotOwnershipMismatch,
    SnapshotSeriesChanged,
    SnapshotPartitionChanged,
    SnapshotOwnershipRegression {
        current: u64,
        actual: u64,
    },
    StaleSnapshotTransition {
        current_generation: u64,
        current_last_sequence: u64,
        actual_generation: u64,
        actual_last_sequence: u64,
    },
    StreamOwnershipChanged,
    StreamSchemaChanged {
        expected: u32,
        actual: u32,
    },
    BarDefinitionMismatch,
    BarSequenceMismatch {
        expected: u64,
        actual: u64,
    },
    NonIncreasingTimestamp {
        source_sequence: u64,
    },
    DeltaBeforeSnapshot,
    SubscriptionMismatch {
        expected: String,
        actual: String,
    },
    UnexpectedPreviousSequence {
        expected: u64,
        actual: u64,
    },
    CanonicalProjection(CanonicalMarketBarProjectionError),
    Instrument(InstrumentValidationError),
    MarketData(MarketDataValidationError),
    Stream(StreamProtocolError),
    Application(ReplayValidationError),
}

impl From<InstrumentValidationError> for ProtobufAdapterError {
    fn from(error: InstrumentValidationError) -> Self {
        Self::Instrument(error)
    }
}

impl From<MarketDataValidationError> for ProtobufAdapterError {
    fn from(error: MarketDataValidationError) -> Self {
        Self::MarketData(error)
    }
}

impl From<StreamProtocolError> for ProtobufAdapterError {
    fn from(error: StreamProtocolError) -> Self {
        Self::Stream(error)
    }
}

impl fmt::Display for ProtobufAdapterError {
    #[allow(clippy::too_many_lines)]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingField(field) => {
                write!(formatter, "missing required Protobuf field {field}")
            }
            Self::MissingUpdate => formatter.write_str("market-bar envelope has no update"),
            Self::EmptyWireField(field) => {
                write!(formatter, "wire field {field} must not be empty")
            }
            Self::ZeroWireField(field) => write!(formatter, "wire field {field} must be non-zero"),
            Self::EmptySubscriptionId => formatter.write_str("subscription_id must not be empty"),
            Self::EmptySnapshotId => formatter.write_str("snapshot_id must not be empty"),
            Self::SnapshotChunkCountLimitExceeded { actual, maximum } => write!(
                formatter,
                "snapshot chunk count {actual} exceeds bounded maximum {maximum}"
            ),
            Self::SnapshotChunkCountExceedsItems { chunks, items } => write!(
                formatter,
                "snapshot chunk count {chunks} exceeds nonempty item count {items}"
            ),
            Self::SnapshotChunkIndexOutOfRange { index, count } => write!(
                formatter,
                "snapshot chunk index {index} is outside chunk count {count}"
            ),
            Self::UnexpectedSnapshotChunkIndex { expected, actual } => write!(
                formatter,
                "snapshot assembly expected chunk index {expected}; received {actual}"
            ),
            Self::EmptySnapshotChunk => {
                formatter.write_str("snapshot chunks must contain at least one market bar")
            }
            Self::SnapshotChunkIdentityMismatch => {
                formatter.write_str("snapshot chunk identity or whole-snapshot evidence changed")
            }
            Self::SnapshotTotalItemCountInvalid { actual, maximum } => write!(
                formatter,
                "snapshot total item count {actual} is outside bounded maximum {maximum}"
            ),
            Self::SnapshotAssemblyItemLimitExceeded => {
                formatter.write_str("snapshot assembly item count overflowed")
            }
            Self::SnapshotAssemblyItemCountMismatch { expected, actual } => write!(
                formatter,
                "snapshot assembly expected {expected} items; received {actual}"
            ),
            Self::DeltaDuringSnapshotAssembly => {
                formatter.write_str("market-bar delta arrived during snapshot assembly")
            }
            Self::SnapshotDuringSnapshotAssembly => {
                formatter.write_str("complete snapshot arrived during chunked snapshot assembly")
            }
            Self::EmptyDecimalConventionUnit(field) => {
                write!(formatter, "decimal convention {field} must not be empty")
            }
            Self::UnknownAssetClass(value) => write!(formatter, "unknown asset class {value}"),
            Self::UnspecifiedAssetClass => formatter.write_str("asset class must be specified"),
            Self::UnknownInstrumentLifecycle(value) => {
                write!(formatter, "unknown instrument lifecycle {value}")
            }
            Self::UnspecifiedInstrumentLifecycle => {
                formatter.write_str("instrument lifecycle must be specified")
            }
            Self::DecimalScaleCannotFitDomain { field, value } => {
                write!(
                    formatter,
                    "{field} scale {value} cannot fit the domain scale type"
                )
            }
            Self::DecimalMantissaEmpty => formatter.write_str("decimal mantissa must not be empty"),
            Self::DecimalMantissaOutOfRange => {
                formatter.write_str("decimal mantissa cannot be represented as i64")
            }
            Self::DecimalScaleMismatch { expected, actual } => {
                write!(
                    formatter,
                    "decimal scale mismatch: expected {expected}, received {actual}"
                )
            }
            Self::DecimalUnitMismatch { expected, actual } => {
                write!(
                    formatter,
                    "decimal unit mismatch: expected {expected}, received {actual}"
                )
            }
            Self::TimestampNotWholeSecond(value) => write!(
                formatter,
                "exchange timestamp {value} nanoseconds cannot be represented as whole seconds"
            ),
            Self::InstrumentIdMismatch { expected, actual } => write!(
                formatter,
                "bar instrument id mismatch: expected {expected}, received {actual}"
            ),
            Self::InstrumentRevisionMismatch { expected, actual } => write!(
                formatter,
                "bar instrument revision mismatch: expected {expected}, received {actual}"
            ),
            Self::VenueIdMismatch { expected, actual } => write!(
                formatter,
                "bar venue id mismatch: expected {expected}, received {actual}"
            ),
            Self::PartitionMetadataMismatch { metadata, header } => write!(
                formatter,
                "partition metadata mismatch: metadata {metadata}, header {header}"
            ),
            Self::OwnershipEpochMetadataMismatch { metadata, header } => write!(
                formatter,
                "ownership epoch metadata mismatch: metadata {metadata}, header {header}"
            ),
            Self::PublicationTimestampMismatch { metadata, header } => write!(
                formatter,
                "publication timestamp mismatch: metadata {metadata}, header {header}"
            ),
            Self::CanonicalTimestampRegression => formatter
                .write_str("normalized timestamp must not precede Axiusflow receive timestamp"),
            Self::FanoutTimestampRegression => formatter
                .write_str("fanout enqueue timestamp must not precede normalized timestamp"),
            Self::NicTimestampProvenanceMismatch => formatter
                .write_str("NIC receive timestamp and timestamp source must be present together"),
            Self::InvalidTimestampSource(value) => {
                write!(formatter, "invalid market timestamp source {value}")
            }
            Self::InvalidSemanticClass(value) => {
                write!(formatter, "invalid market semantic class {value}")
            }
            Self::InvalidSnapshotChecksumLength(actual) => write!(
                formatter,
                "snapshot checksum must be 32 bytes; received {actual}"
            ),
            Self::SnapshotChecksumMismatch => {
                formatter.write_str("snapshot checksum does not match canonical content")
            }
            Self::SnapshotOwnershipMismatch => formatter.write_str(
                "snapshot bar partition or ownership epoch does not match snapshot framing",
            ),
            Self::SnapshotSeriesChanged => {
                formatter.write_str("snapshot changed instrument or bar-definition identity")
            }
            Self::SnapshotPartitionChanged => {
                formatter.write_str("snapshot changed partition identity")
            }
            Self::SnapshotOwnershipRegression { current, actual } => write!(
                formatter,
                "snapshot ownership epoch {actual} regresses current epoch {current}"
            ),
            Self::StaleSnapshotTransition {
                current_generation,
                current_last_sequence,
                actual_generation,
                actual_last_sequence,
            } => write!(
                formatter,
                "snapshot generation {actual_generation} sequence {actual_last_sequence} does not advance current generation {current_generation} sequence {current_last_sequence}"
            ),
            Self::StreamOwnershipChanged => formatter.write_str(
                "delta partition or ownership epoch changed without a replacement snapshot",
            ),
            Self::StreamSchemaChanged { expected, actual } => write!(
                formatter,
                "delta schema version {actual} does not match installed snapshot schema {expected}"
            ),
            Self::BarDefinitionMismatch => {
                formatter.write_str("bar definition does not match the installed series")
            }
            Self::BarSequenceMismatch { expected, actual } => write!(
                formatter,
                "bar sequence mismatch: expected {expected}, received {actual}"
            ),
            Self::NonIncreasingTimestamp { source_sequence } => write!(
                formatter,
                "exchange timestamp did not increase at source sequence {source_sequence}"
            ),
            Self::DeltaBeforeSnapshot => {
                formatter.write_str("market-bar delta arrived before a snapshot")
            }
            Self::SubscriptionMismatch { expected, actual } => write!(
                formatter,
                "subscription mismatch: expected {expected}, received {actual}"
            ),
            Self::UnexpectedPreviousSequence { expected, actual } => write!(
                formatter,
                "delta predecessor mismatch: expected {expected}, received {actual}"
            ),
            Self::CanonicalProjection(error) => {
                write!(formatter, "canonical market-bar projection failed: {error}")
            }
            Self::Instrument(error) => write!(formatter, "invalid instrument: {error}"),
            Self::MarketData(error) => write!(formatter, "invalid market data: {error}"),
            Self::Stream(error) => write!(formatter, "invalid market stream: {error}"),
            Self::Application(error) => {
                write!(formatter, "invalid application market stream: {error}")
            }
        }
    }
}

impl Error for ProtobufAdapterError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CanonicalProjection(error) => Some(error),
            Self::Instrument(error) => Some(error),
            Self::MarketData(error) => Some(error),
            Self::Stream(error) => Some(error),
            Self::Application(error) => Some(error),
            _ => None,
        }
    }
}
