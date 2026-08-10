//! Bounded length-prefixed binary framing.

mod binary_frame;

pub use binary_frame::{
    BINARY_FRAME_LENGTH_BYTES, BinaryFrameError, BoundedBinaryFrameDecoder, encode_binary_frame,
};
