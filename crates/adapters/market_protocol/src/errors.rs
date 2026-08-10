//! Error types for the market protocol conversion boundary.
//!
//! Each error names the boundary that rejected the input: bounded binary framing plus
//! Protobuf decoding, or generated-DTO validation against
//! domain invariants. Callers match on these variants rather than parsing message text.

use axiusflow_application::ReplayValidationError;
use axiusflow_instruments::InstrumentValidationError;
use axiusflow_market_data::MarketDataValidationError;
use axiusflow_protocols::StreamProtocolError;
use axiusflow_transport::BinaryFrameError;
use core::fmt;
use std::error::Error;

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
            Self::Instrument(error) => Some(error),
            Self::MarketData(error) => Some(error),
            Self::Stream(error) => Some(error),
            Self::Application(error) => Some(error),
            _ => None,
        }
    }
}
