//! Typed protocol failure classification.

use std::error::Error;
use std::fmt;

use axiusflow_transport::BinaryFrameError;

use crate::messages::EngineFaultCode;

/// Failures raised while encoding, framing, or decoding protocol envelopes.
#[derive(Debug)]
pub enum ProtocolError {
    /// The bounded binary framing layer rejected the bytes.
    Frame(BinaryFrameError),
    /// Prost could not decode a framed payload.
    Decode(prost::DecodeError),
    /// Prost could not encode an envelope.
    Encode(prost::EncodeError),
    /// The peer speaks an incompatible protocol version.
    VersionMismatch {
        /// Version this endpoint speaks.
        expected: u32,
        /// Version the peer presented.
        found: u32,
    },
    /// The envelope carried no payload variant.
    MissingPayload,
    /// A catalog chunk arrived for a different revision than the transfer in flight.
    CatalogRevisionConflict {
        /// Revision of the transfer in flight.
        expected: u64,
        /// Revision of the rejected chunk.
        found: u64,
    },
    /// A catalog chunk index fell outside the declared chunk count.
    CatalogChunkOutOfRange {
        /// Index presented by the chunk.
        index: u32,
        /// Declared chunk count of the transfer.
        count: u32,
    },
}

impl ProtocolError {
    /// Maps this failure onto the wire-level fault classification.
    #[must_use]
    pub fn fault_code(&self) -> EngineFaultCode {
        match self {
            Self::Frame(
                BinaryFrameError::FrameLimitExceeded { .. }
                | BinaryFrameError::BufferLimitExceeded { .. },
            ) => EngineFaultCode::OversizedFrame,
            Self::Frame(
                BinaryFrameError::EmptyFrame
                | BinaryFrameError::FrameLengthOverflow
                | BinaryFrameError::BufferTooSmall { .. },
            )
            | Self::Decode(_)
            | Self::MissingPayload
            | Self::CatalogRevisionConflict { .. }
            | Self::CatalogChunkOutOfRange { .. } => EngineFaultCode::MalformedMessage,
            Self::Encode(_) => EngineFaultCode::Permanent,
            Self::VersionMismatch { .. } => EngineFaultCode::VersionMismatch,
        }
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(formatter, "framing failed: {error}"),
            Self::Decode(error) => write!(formatter, "protobuf decode failed: {error}"),
            Self::Encode(error) => write!(formatter, "protobuf encode failed: {error}"),
            Self::VersionMismatch { expected, found } => write!(
                formatter,
                "protocol version mismatch: expected {expected}, found {found}"
            ),
            Self::MissingPayload => write!(formatter, "envelope carried no payload"),
            Self::CatalogRevisionConflict { expected, found } => write!(
                formatter,
                "catalog chunk revision conflict: expected {expected}, found {found}"
            ),
            Self::CatalogChunkOutOfRange { index, count } => write!(
                formatter,
                "catalog chunk index {index} outside declared count {count}"
            ),
        }
    }
}

impl Error for ProtocolError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frame(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::Encode(error) => Some(error),
            Self::VersionMismatch { .. }
            | Self::MissingPayload
            | Self::CatalogRevisionConflict { .. }
            | Self::CatalogChunkOutOfRange { .. } => None,
        }
    }
}

impl From<BinaryFrameError> for ProtocolError {
    fn from(error: BinaryFrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<prost::DecodeError> for ProtocolError {
    fn from(error: prost::DecodeError) -> Self {
        Self::Decode(error)
    }
}

impl From<prost::EncodeError> for ProtocolError {
    fn from(error: prost::EncodeError) -> Self {
        Self::Encode(error)
    }
}
