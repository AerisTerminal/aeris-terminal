//! Bounded length-prefixed binary frame decoding and encoding.

use core::fmt;
use std::error::Error;
use std::num::NonZeroUsize;

/// Bytes in the network-order length prefix used by binary client stream frames.
pub const BINARY_FRAME_LENGTH_BYTES: usize = u32::BITS as usize / 8;

/// Bounded incremental decoder for fragmented or coalesced length-prefixed binary frames.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedBinaryFrameDecoder {
    maximum_frame_bytes: NonZeroUsize,
    maximum_buffered_bytes: NonZeroUsize,
    buffered: Vec<u8>,
}

impl BoundedBinaryFrameDecoder {
    /// Creates a decoder whose total partial-frame storage is explicitly bounded.
    ///
    /// # Errors
    ///
    /// Returns an error if the buffer cannot hold one maximum-sized framed payload.
    pub fn try_new(
        maximum_frame_bytes: NonZeroUsize,
        maximum_buffered_bytes: NonZeroUsize,
    ) -> Result<Self, BinaryFrameError> {
        let required = maximum_frame_bytes
            .get()
            .checked_add(BINARY_FRAME_LENGTH_BYTES)
            .ok_or(BinaryFrameError::FrameLengthOverflow)?;
        if maximum_buffered_bytes.get() < required {
            return Err(BinaryFrameError::BufferTooSmall {
                required,
                actual: maximum_buffered_bytes.get(),
            });
        }
        Ok(Self {
            maximum_frame_bytes,
            maximum_buffered_bytes,
            buffered: Vec::with_capacity(required),
        })
    }

    /// Accepts one bounded transport chunk and returns every complete frame it contains.
    ///
    /// Partial frames remain bounded until a later chunk completes them. Any invalid
    /// length clears partial state so callers must explicitly restart stream semantics.
    ///
    /// # Errors
    ///
    /// Returns an error for zero/oversized frames or buffer-capacity overflow.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, BinaryFrameError> {
        let next_size = self
            .buffered
            .len()
            .checked_add(chunk.len())
            .ok_or(BinaryFrameError::FrameLengthOverflow)?;
        if next_size > self.maximum_buffered_bytes.get() {
            self.buffered.clear();
            return Err(BinaryFrameError::BufferLimitExceeded {
                requested: next_size,
                maximum: self.maximum_buffered_bytes.get(),
            });
        }
        self.buffered.extend_from_slice(chunk);

        let mut frames = Vec::new();
        let mut consumed = 0;
        while self.buffered.len().saturating_sub(consumed) >= BINARY_FRAME_LENGTH_BYTES {
            let prefix: [u8; BINARY_FRAME_LENGTH_BYTES] = self
                .buffered
                .get(consumed..consumed + BINARY_FRAME_LENGTH_BYTES)
                .ok_or(BinaryFrameError::FrameLengthOverflow)?
                .try_into()
                .map_err(|_| BinaryFrameError::FrameLengthOverflow)?;
            let frame_length = usize::try_from(u32::from_be_bytes(prefix))
                .map_err(|_| BinaryFrameError::FrameLengthOverflow)?;
            if frame_length == 0 {
                self.buffered.clear();
                return Err(BinaryFrameError::EmptyFrame);
            }
            if frame_length > self.maximum_frame_bytes.get() {
                self.buffered.clear();
                return Err(BinaryFrameError::FrameLimitExceeded {
                    requested: frame_length,
                    maximum: self.maximum_frame_bytes.get(),
                });
            }
            let total_frame_bytes = BINARY_FRAME_LENGTH_BYTES
                .checked_add(frame_length)
                .ok_or(BinaryFrameError::FrameLengthOverflow)?;
            if self.buffered.len().saturating_sub(consumed) < total_frame_bytes {
                break;
            }
            let payload_start = consumed + BINARY_FRAME_LENGTH_BYTES;
            frames.push(self.buffered[payload_start..payload_start + frame_length].to_vec());
            consumed = consumed
                .checked_add(total_frame_bytes)
                .ok_or(BinaryFrameError::FrameLengthOverflow)?;
        }
        if consumed > 0 {
            self.buffered.drain(..consumed);
        }
        Ok(frames)
    }

    /// Clears any partial frame after reconnect or semantic recovery.
    pub fn reset(&mut self) {
        self.buffered.clear();
    }

    #[must_use]
    pub const fn buffered_bytes(&self) -> usize {
        self.buffered.len()
    }
}

/// Encodes one non-empty payload with a network-order length prefix and a hard size bound.
///
/// # Errors
///
/// Returns an error for an empty, oversized, or unrepresentable payload.
pub fn encode_binary_frame(
    payload: &[u8],
    maximum_frame_bytes: NonZeroUsize,
) -> Result<Vec<u8>, BinaryFrameError> {
    if payload.is_empty() {
        return Err(BinaryFrameError::EmptyFrame);
    }
    if payload.len() > maximum_frame_bytes.get() {
        return Err(BinaryFrameError::FrameLimitExceeded {
            requested: payload.len(),
            maximum: maximum_frame_bytes.get(),
        });
    }
    let frame_length =
        u32::try_from(payload.len()).map_err(|_| BinaryFrameError::FrameLimitExceeded {
            requested: payload.len(),
            maximum: u32::MAX as usize,
        })?;
    let capacity = BINARY_FRAME_LENGTH_BYTES
        .checked_add(payload.len())
        .ok_or(BinaryFrameError::FrameLengthOverflow)?;
    let mut framed = Vec::with_capacity(capacity);
    framed.extend_from_slice(&frame_length.to_be_bytes());
    framed.extend_from_slice(payload);
    Ok(framed)
}

/// Structural failures in the bounded binary client framing layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryFrameError {
    EmptyFrame,
    FrameLengthOverflow,
    FrameLimitExceeded { requested: usize, maximum: usize },
    BufferLimitExceeded { requested: usize, maximum: usize },
    BufferTooSmall { required: usize, actual: usize },
}

impl fmt::Display for BinaryFrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "binary frame rejected: {self:?}")
    }
}

impl Error for BinaryFrameError {}

#[cfg(test)]
mod tests {
    use super::{BinaryFrameError, BoundedBinaryFrameDecoder};
    use std::num::NonZeroUsize;

    fn limit(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test limit is non-zero")
    }

    #[test]
    fn decoder_requires_capacity_for_one_maximum_frame() {
        let error = BoundedBinaryFrameDecoder::try_new(limit(1_024), limit(64))
            .expect_err("a buffer smaller than one framed payload must be rejected");
        assert!(matches!(error, BinaryFrameError::BufferTooSmall { .. }));
    }

    #[test]
    fn decoder_returns_coalesced_frames_in_order() {
        let mut decoder = BoundedBinaryFrameDecoder::try_new(limit(64), limit(1_024))
            .expect("decoder accepts a sufficient buffer");
        let mut chunk = Vec::new();
        for payload in [b"first".as_slice(), b"second".as_slice()] {
            chunk.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
            chunk.extend_from_slice(payload);
        }
        let frames = decoder.push(&chunk).expect("both frames decode");
        assert_eq!(frames, vec![b"first".to_vec(), b"second".to_vec()]);
    }

    #[test]
    fn decoder_buffers_fragmented_frame_until_complete() {
        let mut decoder = BoundedBinaryFrameDecoder::try_new(limit(64), limit(1_024))
            .expect("decoder accepts a sufficient buffer");
        let payload = b"fragmented";
        let mut framed = u32::try_from(payload.len()).unwrap().to_be_bytes().to_vec();
        framed.extend_from_slice(payload);
        let (head, tail) = framed.split_at(6);

        assert!(
            decoder
                .push(head)
                .expect("partial frame is buffered")
                .is_empty(),
            "a partial frame must not yield a decoded frame"
        );
        assert_eq!(
            decoder.push(tail).expect("completing chunk decodes"),
            vec![payload.to_vec()]
        );
    }

    #[test]
    fn decoder_rejects_zero_length_frame() {
        let mut decoder = BoundedBinaryFrameDecoder::try_new(limit(64), limit(1_024))
            .expect("decoder accepts a sufficient buffer");
        let error = decoder
            .push(&0_u32.to_be_bytes())
            .expect_err("a zero-length frame must be rejected");
        assert!(matches!(error, BinaryFrameError::EmptyFrame));
    }

    #[test]
    fn decoder_rejects_frame_above_maximum_and_clears_partial_state() {
        let mut decoder = BoundedBinaryFrameDecoder::try_new(limit(8), limit(1_024))
            .expect("decoder accepts a sufficient buffer");
        let error = decoder
            .push(&64_u32.to_be_bytes())
            .expect_err("an oversized declared length must be rejected");
        assert!(matches!(
            error,
            BinaryFrameError::FrameLimitExceeded {
                requested: 64,
                maximum: 8
            }
        ));

        let mut framed = 4_u32.to_be_bytes().to_vec();
        framed.extend_from_slice(b"okay");
        assert_eq!(
            decoder
                .push(&framed)
                .expect("decoder recovers after rejection"),
            vec![b"okay".to_vec()],
            "rejecting an oversized frame must clear partial state"
        );
    }

    #[test]
    fn decoder_reset_discards_partial_frame() {
        let mut decoder = BoundedBinaryFrameDecoder::try_new(limit(64), limit(1_024))
            .expect("decoder accepts a sufficient buffer");
        assert!(
            decoder
                .push(&32_u32.to_be_bytes())
                .expect("length prefix is buffered")
                .is_empty()
        );
        decoder.reset();

        let mut framed = 5_u32.to_be_bytes().to_vec();
        framed.extend_from_slice(b"after");
        assert_eq!(
            decoder.push(&framed).expect("decoder decodes after reset"),
            vec![b"after".to_vec()],
            "reset must discard the buffered length prefix"
        );
    }
}
