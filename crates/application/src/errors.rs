//! Validation failures at the replay application boundary.

use crate::stream::StreamProtocolError;
use axiusflow_instruments::InstrumentValidationError;
use axiusflow_market_data::MarketDataValidationError;
use core::fmt;
use std::error::Error;

/// Validation failures at the replay application boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplayValidationError {
    Instrument(InstrumentValidationError),
    MarketData(MarketDataValidationError),
    Stream(StreamProtocolError),
    NonIncreasingTimestamp {
        source_sequence: u64,
    },
    TailTimestampChanged {
        source_sequence: u64,
    },
    TailOperationMismatch {
        source_sequence: u64,
    },
    ProvenanceSequenceMismatch {
        bar: u64,
        provenance: u64,
    },
    ExchangeTimestampOverflow {
        source_sequence: u64,
    },
    ProvenanceExchangeTimestampMismatch {
        source_sequence: u64,
        bar_seconds: i64,
        provenance_nanos: i64,
    },
    MissingProvenance(&'static str),
    InvalidProvenanceRevision,
    InvalidSnapshotEvidence(&'static str),
    SnapshotEvidenceMismatch(&'static str),
    SnapshotChecksumMismatch,
    SnapshotSessionGenerationRegression {
        current_generation: u64,
        actual_generation: u64,
    },
    StaleSnapshot {
        current_generation: u64,
        current_last_sequence: u64,
        actual_generation: u64,
        actual_last_sequence: u64,
    },
    UncorrelatedRecoverySnapshot {
        request_id: u64,
    },
}

impl From<InstrumentValidationError> for ReplayValidationError {
    fn from(error: InstrumentValidationError) -> Self {
        Self::Instrument(error)
    }
}

impl From<MarketDataValidationError> for ReplayValidationError {
    fn from(error: MarketDataValidationError) -> Self {
        Self::MarketData(error)
    }
}

impl From<StreamProtocolError> for ReplayValidationError {
    fn from(error: StreamProtocolError) -> Self {
        Self::Stream(error)
    }
}

impl fmt::Display for ReplayValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Instrument(error) => write!(formatter, "invalid replay instrument: {error}"),
            Self::MarketData(error) => write!(formatter, "invalid replay market data: {error}"),
            Self::Stream(error) => write!(formatter, "invalid replay stream: {error}"),
            Self::NonIncreasingTimestamp { source_sequence } => write!(
                formatter,
                "replay timestamp did not increase at source sequence {source_sequence}"
            ),
            Self::TailTimestampChanged { source_sequence } => write!(
                formatter,
                "forming replay timestamp changed at source sequence {source_sequence}"
            ),
            Self::TailOperationMismatch { source_sequence } => write!(
                formatter,
                "engine tail operation does not match source sequence {source_sequence}"
            ),
            Self::ProvenanceSequenceMismatch { bar, provenance } => write!(
                formatter,
                "bar sequence {bar} does not match provenance sequence {provenance}"
            ),
            Self::ExchangeTimestampOverflow { source_sequence } => write!(
                formatter,
                "bar exchange timestamp overflows nanoseconds at source sequence {source_sequence}"
            ),
            Self::ProvenanceExchangeTimestampMismatch {
                source_sequence,
                bar_seconds,
                provenance_nanos,
            } => write!(
                formatter,
                "bar exchange timestamp {bar_seconds}s does not match provenance timestamp {provenance_nanos}ns at source sequence {source_sequence}"
            ),
            Self::MissingProvenance(field) => {
                write!(
                    formatter,
                    "market provenance field {field} must not be empty"
                )
            }
            Self::InvalidProvenanceRevision => formatter.write_str(
                "market provenance session generation and schema version must be non-zero",
            ),
            Self::InvalidSnapshotEvidence(field) => {
                write!(
                    formatter,
                    "snapshot evidence field {field} must be non-zero"
                )
            }
            Self::SnapshotEvidenceMismatch(field) => write!(
                formatter,
                "snapshot evidence field {field} does not match its market values"
            ),
            Self::SnapshotChecksumMismatch => {
                formatter.write_str("snapshot checksum does not match its canonical market values")
            }
            Self::SnapshotSessionGenerationRegression {
                current_generation,
                actual_generation,
            } => write!(
                formatter,
                "snapshot session generation {actual_generation} regresses current generation {current_generation}"
            ),
            Self::StaleSnapshot {
                current_generation,
                current_last_sequence,
                actual_generation,
                actual_last_sequence,
            } => write!(
                formatter,
                "snapshot generation {actual_generation} sequence {actual_last_sequence} does not advance current generation {current_generation} sequence {current_last_sequence}"
            ),
            Self::UncorrelatedRecoverySnapshot { request_id } => write!(
                formatter,
                "snapshot cannot bypass active recovery request {request_id}"
            ),
        }
    }
}

impl Error for ReplayValidationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Instrument(error) => Some(error),
            Self::MarketData(error) => Some(error),
            Self::Stream(error) => Some(error),
            Self::NonIncreasingTimestamp { .. }
            | Self::TailTimestampChanged { .. }
            | Self::TailOperationMismatch { .. }
            | Self::ProvenanceSequenceMismatch { .. }
            | Self::ExchangeTimestampOverflow { .. }
            | Self::ProvenanceExchangeTimestampMismatch { .. }
            | Self::MissingProvenance(_)
            | Self::InvalidProvenanceRevision
            | Self::InvalidSnapshotEvidence(_)
            | Self::SnapshotEvidenceMismatch(_)
            | Self::SnapshotChecksumMismatch
            | Self::SnapshotSessionGenerationRegression { .. }
            | Self::StaleSnapshot { .. }
            | Self::UncorrelatedRecoverySnapshot { .. } => None,
        }
    }
}
