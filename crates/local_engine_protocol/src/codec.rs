//! Synchronous envelope framing and decoding over the bounded binary frame layer.

use std::num::NonZeroUsize;

use axiusflow_transport::{BoundedBinaryFrameDecoder, encode_binary_frame};
use prost::Message as _;

use crate::error::ProtocolError;
use crate::messages::Envelope;
use crate::{MAX_BUFFERED_BYTES, MAX_FRAME_BYTES, PROTOCOL_VERSION};

const fn frame_limit(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(limit) => limit,
        None => panic!("protocol frame limits are non-zero constants"),
    }
}

const MAX_FRAME_LIMIT: NonZeroUsize = frame_limit(MAX_FRAME_BYTES);
const MAX_BUFFERED_LIMIT: NonZeroUsize = frame_limit(MAX_BUFFERED_BYTES);

/// Encodes one envelope into a single bounded binary frame.
///
/// # Errors
///
/// Returns [`ProtocolError::Encode`] if prost cannot encode the message, or
/// [`ProtocolError::Frame`] if the encoded payload is empty or exceeds
/// [`MAX_FRAME_BYTES`] (classified as [`crate::EngineFaultCode::OversizedFrame`]).
pub fn encode_envelope(message: &Envelope) -> Result<Vec<u8>, ProtocolError> {
    let mut payload = Vec::with_capacity(message.encoded_len());
    message.encode(&mut payload)?;
    Ok(encode_binary_frame(&payload, MAX_FRAME_LIMIT)?)
}

/// Incremental decoder turning byte chunks into validated envelopes.
#[derive(Clone, Debug)]
pub struct EnvelopeDecoder {
    inner: BoundedBinaryFrameDecoder,
}

impl EnvelopeDecoder {
    /// Creates a decoder bounded by [`MAX_FRAME_BYTES`] and [`MAX_BUFFERED_BYTES`].
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Frame`] if the compiled-in limits are inconsistent.
    pub fn try_new() -> Result<Self, ProtocolError> {
        Ok(Self {
            inner: BoundedBinaryFrameDecoder::try_new(MAX_FRAME_LIMIT, MAX_BUFFERED_LIMIT)?,
        })
    }

    /// Accepts one transport chunk and returns every complete, validated envelope.
    ///
    /// Each decoded envelope must carry [`PROTOCOL_VERSION`] and a payload;
    /// anything else is rejected with [`ProtocolError::VersionMismatch`] or
    /// [`ProtocolError::MissingPayload`].
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Frame`] for framing violations,
    /// [`ProtocolError::Decode`] for malformed protobuf, and the validation
    /// errors named above.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Envelope>, ProtocolError> {
        let frames = self.inner.push(chunk)?;
        let mut envelopes = Vec::with_capacity(frames.len());
        for frame in frames {
            let envelope = Envelope::decode(frame.as_slice())?;
            if envelope.protocol_version != PROTOCOL_VERSION {
                return Err(ProtocolError::VersionMismatch {
                    expected: PROTOCOL_VERSION,
                    found: envelope.protocol_version,
                });
            }
            if envelope.payload.is_none() {
                return Err(ProtocolError::MissingPayload);
            }
            envelopes.push(envelope);
        }
        Ok(envelopes)
    }

    /// Clears any buffered partial frame after reconnect or semantic recovery.
    pub fn reset(&mut self) {
        self.inner.reset();
    }
}
