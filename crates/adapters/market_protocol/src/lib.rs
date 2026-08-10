//! Validated conversion boundary between generated market DTOs and domain values.
//!
//! The market-data domain intentionally stores the fixed-point values and whole-
//! second exchange timestamp needed by current application use cases. Conversion
//! therefore rejects wire values that cannot be represented without loss.

mod convention;
mod envelope_encoding;
mod errors;
mod stream_decoder;
mod wire_codec;

pub use convention::{DecimalConvention, MAX_MARKET_BAR_SNAPSHOT_CHUNKS};

pub(crate) use convention::NANOS_PER_SECOND;
pub use envelope_encoding::{
    encode_market_bar_stream_frame, try_encode_replay_delta_envelope,
    try_encode_replay_snapshot_chunk_envelopes, try_encode_replay_snapshot_envelope,
};
pub use errors::{BinaryMarketStreamError, ProtobufAdapterError};
pub use stream_decoder::{
    BinaryMarketBarStreamDecoder, DecodedMarketBarDelta, DecodedMarketBarSnapshot,
    DecodedMarketBarStreamUpdate, MarketBarStreamDecoder, ProjectedMarketBarUpdate,
};
pub use wire_codec::{
    encode_decimal_i64, try_decode_decimal_i64, try_decode_instrument_revision,
    try_decode_market_bar, try_decode_market_bar_delta, try_decode_market_bar_snapshot,
    try_encode_instrument_revision,
};
