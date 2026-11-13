//! Framed binary encoding of recorded market events.
//!
//! A segment file starts with `SEGMENT_MAGIC` and the little-endian format
//! version. Frames follow back to back as
//! `payload_len: u32 LE | checksum: first 8 bytes of SHA-256(payload) | payload`.
//! The first frame of every segment describes the instrument, including the
//! decimal scales of the provider's fixed-point prices and quantities, which
//! every event frame stores unchanged. Event frames keep the provider's
//! entitlement, sequence, session generation and each clock separately.

use aeris_market_data::{
    AggressorSide, DepthLevel, DepthSnapshot, MarketTrade, QualifiedTimestamp, QuoteLevel,
    TopOfBookQuote,
};
use sha2::{Digest, Sha256};

pub(super) const SEGMENT_MAGIC: [u8; 8] = *b"\0AERISRC";
pub(super) const FORMAT_VERSION: u16 = 1;
pub(super) const SEGMENT_HEADER_BYTES: usize = SEGMENT_MAGIC.len() + 2;
pub(super) const FRAME_HEADER_BYTES: usize = 4 + CHECKSUM_BYTES;
/// Larger than the widest accepted depth image (4,096 levels per side).
pub(super) const MAXIMUM_FRAME_PAYLOAD_BYTES: usize = 1 << 20;
const CHECKSUM_BYTES: usize = 8;

const KIND_INSTRUMENT: u8 = 1;
const KIND_TRADE: u8 = 2;
const KIND_QUOTE: u8 = 3;
const KIND_DEPTH: u8 = 4;
const KIND_GAP: u8 = 5;

/// Instrument identity and fixed-point scales that start every segment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct InstrumentFrame {
    pub(super) provider_id: String,
    pub(super) instrument_id: String,
    pub(super) provider_symbol: String,
    pub(super) display_symbol: String,
    pub(super) venue_id: String,
    pub(super) entitlement_id: String,
    pub(super) price_scale: u32,
    pub(super) quantity_scale: u32,
    pub(super) price_increment: Option<i64>,
}

impl InstrumentFrame {
    pub(super) fn from_install(instrument: &aeris_contracts::InstallProviderInstrument) -> Self {
        Self {
            provider_id: instrument.provider.clone(),
            instrument_id: instrument.instrument_id.clone(),
            provider_symbol: instrument.provider_symbol.clone(),
            display_symbol: instrument.display_symbol.clone(),
            venue_id: instrument.venue_id.clone(),
            entitlement_id: instrument.entitlement_id.clone(),
            price_scale: instrument.price_scale,
            quantity_scale: instrument.quantity_scale,
            price_increment: instrument.price_increment,
        }
    }
}

/// One frame to append, borrowed from the event that produced it.
pub(super) enum FrameRef<'a> {
    Instrument(&'a InstrumentFrame),
    Trade(&'a MarketTrade),
    Quote(&'a TopOfBookQuote),
    Depth(&'a DepthSnapshot),
    /// The recorder dropped this many events of the instrument before the next frame.
    Gap(u64),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum EncodeError {
    FieldTooLong,
    FrameTooLarge,
}

pub(super) fn segment_header() -> [u8; SEGMENT_HEADER_BYTES] {
    let mut header = [0; SEGMENT_HEADER_BYTES];
    header[..SEGMENT_MAGIC.len()].copy_from_slice(&SEGMENT_MAGIC);
    header[SEGMENT_MAGIC.len()..].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    header
}

/// Appends one complete frame to `out`, leaving `out` unchanged on error.
pub(super) fn encode_frame(out: &mut Vec<u8>, frame: &FrameRef<'_>) -> Result<(), EncodeError> {
    let start = out.len();
    out.extend_from_slice(&[0; FRAME_HEADER_BYTES]);
    let encoded = encode_payload(out, frame);
    let payload_len = out.len() - start - FRAME_HEADER_BYTES;
    let result = encoded.and_then(|()| {
        if payload_len > MAXIMUM_FRAME_PAYLOAD_BYTES {
            Err(EncodeError::FrameTooLarge)
        } else {
            u32::try_from(payload_len).map_err(|_| EncodeError::FrameTooLarge)
        }
    });
    match result {
        Ok(length) => {
            let checksum = Sha256::digest(&out[start + FRAME_HEADER_BYTES..]);
            out[start..start + 4].copy_from_slice(&length.to_le_bytes());
            out[start + 4..start + FRAME_HEADER_BYTES].copy_from_slice(&checksum[..CHECKSUM_BYTES]);
            Ok(())
        }
        Err(error) => {
            out.truncate(start);
            Err(error)
        }
    }
}

fn encode_payload(out: &mut Vec<u8>, frame: &FrameRef<'_>) -> Result<(), EncodeError> {
    match frame {
        FrameRef::Instrument(instrument) => {
            out.push(KIND_INSTRUMENT);
            for field in [
                &instrument.provider_id,
                &instrument.instrument_id,
                &instrument.provider_symbol,
                &instrument.display_symbol,
                &instrument.venue_id,
                &instrument.entitlement_id,
            ] {
                put_str(out, field)?;
            }
            out.extend_from_slice(&instrument.price_scale.to_le_bytes());
            out.extend_from_slice(&instrument.quantity_scale.to_le_bytes());
            put_optional_i64(out, instrument.price_increment);
        }
        FrameRef::Trade(trade) => {
            out.push(KIND_TRADE);
            put_metadata(out, &trade.metadata)?;
            put_str(out, &trade.trade_id)?;
            out.extend_from_slice(&trade.price.to_le_bytes());
            out.extend_from_slice(&trade.quantity.to_le_bytes());
            out.push(match trade.aggressor {
                AggressorSide::Unknown => 0,
                AggressorSide::Buy => 1,
                AggressorSide::Sell => 2,
            });
        }
        FrameRef::Quote(quote) => {
            out.push(KIND_QUOTE);
            put_metadata(out, &quote.metadata)?;
            put_quote_level(out, quote.bid);
            put_quote_level(out, quote.ask);
        }
        FrameRef::Depth(depth) => {
            out.push(KIND_DEPTH);
            put_metadata(out, &depth.metadata)?;
            put_depth_levels(out, &depth.bids)?;
            put_depth_levels(out, &depth.asks)?;
        }
        FrameRef::Gap(dropped_events) => {
            out.push(KIND_GAP);
            out.extend_from_slice(&dropped_events.to_le_bytes());
        }
    }
    Ok(())
}

fn put_str(out: &mut Vec<u8>, value: &str) -> Result<(), EncodeError> {
    let length = u16::try_from(value.len()).map_err(|_| EncodeError::FieldTooLong)?;
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn put_optional_i64(out: &mut Vec<u8>, value: Option<i64>) {
    match value {
        Some(value) => {
            out.push(1);
            out.extend_from_slice(&value.to_le_bytes());
        }
        None => out.push(0),
    }
}

fn put_optional_u32(out: &mut Vec<u8>, value: Option<u32>) {
    match value {
        Some(value) => {
            out.push(1);
            out.extend_from_slice(&value.to_le_bytes());
        }
        None => out.push(0),
    }
}

fn put_metadata(
    out: &mut Vec<u8>,
    metadata: &aeris_market_data::EventMetadata,
) -> Result<(), EncodeError> {
    put_str(out, &metadata.entitlement_id)?;
    out.extend_from_slice(&metadata.source_sequence.to_le_bytes());
    out.extend_from_slice(&metadata.session_generation.to_le_bytes());
    let QualifiedTimestamp {
        exchange_unix_nanos,
        provider_unix_nanos,
        received_unix_nanos,
    } = metadata.timestamps;
    put_optional_i64(out, exchange_unix_nanos);
    put_optional_i64(out, provider_unix_nanos);
    out.extend_from_slice(&received_unix_nanos.to_le_bytes());
    Ok(())
}

fn put_quote_level(out: &mut Vec<u8>, level: Option<QuoteLevel>) {
    match level {
        Some(level) => {
            out.push(1);
            out.extend_from_slice(&level.price.to_le_bytes());
            put_optional_i64(out, level.quantity);
            put_optional_u32(out, level.order_count);
        }
        None => out.push(0),
    }
}

fn put_depth_levels(out: &mut Vec<u8>, levels: &[DepthLevel]) -> Result<(), EncodeError> {
    let count = u32::try_from(levels.len()).map_err(|_| EncodeError::FrameTooLarge)?;
    out.extend_from_slice(&count.to_le_bytes());
    for level in levels {
        out.extend_from_slice(&level.price.to_le_bytes());
        out.extend_from_slice(&level.quantity.to_le_bytes());
        put_optional_u32(out, level.order_count);
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod decode {
    //! Segment reader: everything up to the first incomplete or damaged frame.

    use super::{
        CHECKSUM_BYTES, FORMAT_VERSION, FRAME_HEADER_BYTES, InstrumentFrame, KIND_DEPTH, KIND_GAP,
        KIND_INSTRUMENT, KIND_QUOTE, KIND_TRADE, MAXIMUM_FRAME_PAYLOAD_BYTES, SEGMENT_HEADER_BYTES,
        SEGMENT_MAGIC,
    };
    use aeris_market_data::{
        AggressorSide, DepthLevel, DepthSnapshot, EventMetadata, MarketTrade, QualifiedTimestamp,
        QuoteLevel, TopOfBookQuote,
    };
    use sha2::{Digest, Sha256};

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) enum RecordedFrame {
        Instrument(InstrumentFrame),
        Trade(MarketTrade),
        Quote(TopOfBookQuote),
        Depth(DepthSnapshot),
        Gap(u64),
    }

    /// Where reading stopped. Frames before `offset` are intact.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum SegmentTail {
        Complete,
        Truncated { offset: usize },
        Corrupt { offset: usize },
    }

    pub(crate) struct DecodedSegment {
        pub(crate) frames: Vec<RecordedFrame>,
        pub(crate) tail: SegmentTail,
    }

    pub(crate) fn decode_segment(bytes: &[u8]) -> DecodedSegment {
        let mut frames = Vec::new();
        if bytes.len() < SEGMENT_HEADER_BYTES {
            return DecodedSegment {
                frames,
                tail: SegmentTail::Truncated { offset: 0 },
            };
        }
        if bytes[..SEGMENT_MAGIC.len()] != SEGMENT_MAGIC
            || bytes[SEGMENT_MAGIC.len()..SEGMENT_HEADER_BYTES] != FORMAT_VERSION.to_le_bytes()
        {
            return DecodedSegment {
                frames,
                tail: SegmentTail::Corrupt { offset: 0 },
            };
        }
        let mut offset = SEGMENT_HEADER_BYTES;
        let mut instrument: Option<InstrumentFrame> = None;
        let tail = loop {
            if offset == bytes.len() {
                break SegmentTail::Complete;
            }
            if bytes.len() - offset < FRAME_HEADER_BYTES {
                break SegmentTail::Truncated { offset };
            }
            let mut length = [0; 4];
            length.copy_from_slice(&bytes[offset..offset + 4]);
            let Ok(length) = usize::try_from(u32::from_le_bytes(length)) else {
                break SegmentTail::Corrupt { offset };
            };
            if length > MAXIMUM_FRAME_PAYLOAD_BYTES {
                break SegmentTail::Corrupt { offset };
            }
            let payload_start = offset + FRAME_HEADER_BYTES;
            if bytes.len() - payload_start < length {
                break SegmentTail::Truncated { offset };
            }
            let payload = &bytes[payload_start..payload_start + length];
            if Sha256::digest(payload)[..CHECKSUM_BYTES] != bytes[offset + 4..payload_start] {
                break SegmentTail::Corrupt { offset };
            }
            let Ok(frame) = decode_payload(payload, instrument.as_ref()) else {
                break SegmentTail::Corrupt { offset };
            };
            if let RecordedFrame::Instrument(header) = &frame {
                instrument = Some(header.clone());
            }
            frames.push(frame);
            offset = payload_start + length;
        };
        DecodedSegment { frames, tail }
    }

    /// The payload does not match the frame layout.
    struct Malformed;

    type Field<T> = Result<T, Malformed>;

    struct Cursor<'a>(&'a [u8]);

    impl Cursor<'_> {
        fn take<const N: usize>(&mut self) -> Field<[u8; N]> {
            let (head, rest) = self.0.split_at_checked(N).ok_or(Malformed)?;
            self.0 = rest;
            head.try_into().map_err(|_| Malformed)
        }
        fn u8(&mut self) -> Field<u8> {
            self.take::<1>().map(|[value]| value)
        }
        fn u32(&mut self) -> Field<u32> {
            self.take().map(u32::from_le_bytes)
        }
        fn u64(&mut self) -> Field<u64> {
            self.take().map(u64::from_le_bytes)
        }
        fn i64(&mut self) -> Field<i64> {
            self.take().map(i64::from_le_bytes)
        }
        fn string(&mut self) -> Field<String> {
            let length = usize::from(u16::from_le_bytes(self.take()?));
            let (head, rest) = self.0.split_at_checked(length).ok_or(Malformed)?;
            self.0 = rest;
            String::from_utf8(head.to_vec()).map_err(|_| Malformed)
        }
        fn present(&mut self) -> Field<bool> {
            match self.u8()? {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(Malformed),
            }
        }
        fn optional_i64(&mut self) -> Field<Option<i64>> {
            if self.present()? {
                self.i64().map(Some)
            } else {
                Ok(None)
            }
        }
        fn optional_u32(&mut self) -> Field<Option<u32>> {
            if self.present()? {
                self.u32().map(Some)
            } else {
                Ok(None)
            }
        }
        fn metadata(&mut self, instrument: Option<&InstrumentFrame>) -> Field<EventMetadata> {
            let instrument = instrument.ok_or(Malformed)?;
            Ok(EventMetadata {
                provider_id: instrument.provider_id.clone(),
                instrument_id: instrument.instrument_id.clone(),
                entitlement_id: self.string()?,
                source_sequence: self.u64()?,
                session_generation: self.u64()?,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: self.optional_i64()?,
                    provider_unix_nanos: self.optional_i64()?,
                    received_unix_nanos: self.i64()?,
                },
            })
        }
        fn quote_level(&mut self) -> Field<Option<QuoteLevel>> {
            if !self.present()? {
                return Ok(None);
            }
            Ok(Some(QuoteLevel {
                price: self.i64()?,
                quantity: self.optional_i64()?,
                order_count: self.optional_u32()?,
            }))
        }
        fn depth_levels(&mut self) -> Field<Vec<DepthLevel>> {
            let count = usize::try_from(self.u32()?).map_err(|_| Malformed)?;
            // Each level takes at least 17 bytes; reject counts the payload cannot hold.
            if count > self.0.len() / 17 {
                return Err(Malformed);
            }
            (0..count)
                .map(|_| {
                    Ok(DepthLevel {
                        price: self.i64()?,
                        quantity: self.i64()?,
                        order_count: self.optional_u32()?,
                    })
                })
                .collect()
        }
    }

    fn decode_payload(
        payload: &[u8],
        instrument: Option<&InstrumentFrame>,
    ) -> Field<RecordedFrame> {
        let mut cursor = Cursor(payload);
        let frame = match cursor.u8()? {
            KIND_INSTRUMENT => RecordedFrame::Instrument(InstrumentFrame {
                provider_id: cursor.string()?,
                instrument_id: cursor.string()?,
                provider_symbol: cursor.string()?,
                display_symbol: cursor.string()?,
                venue_id: cursor.string()?,
                entitlement_id: cursor.string()?,
                price_scale: cursor.u32()?,
                quantity_scale: cursor.u32()?,
                price_increment: cursor.optional_i64()?,
            }),
            KIND_TRADE => RecordedFrame::Trade(MarketTrade {
                metadata: cursor.metadata(instrument)?,
                trade_id: cursor.string()?,
                price: cursor.i64()?,
                quantity: cursor.i64()?,
                aggressor: match cursor.u8()? {
                    0 => AggressorSide::Unknown,
                    1 => AggressorSide::Buy,
                    2 => AggressorSide::Sell,
                    _ => return Err(Malformed),
                },
            }),
            KIND_QUOTE => RecordedFrame::Quote(TopOfBookQuote {
                metadata: cursor.metadata(instrument)?,
                bid: cursor.quote_level()?,
                ask: cursor.quote_level()?,
            }),
            KIND_DEPTH => RecordedFrame::Depth(DepthSnapshot {
                metadata: cursor.metadata(instrument)?,
                bids: cursor.depth_levels()?,
                asks: cursor.depth_levels()?,
            }),
            KIND_GAP => RecordedFrame::Gap(cursor.u64()?),
            _ => return Err(Malformed),
        };
        if cursor.0.is_empty() {
            Ok(frame)
        } else {
            Err(Malformed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::decode::{RecordedFrame, SegmentTail, decode_segment};
    use super::*;
    use aeris_market_data::EventMetadata;

    pub(crate) fn instrument() -> InstrumentFrame {
        InstrumentFrame {
            provider_id: "provider-a".into(),
            instrument_id: "instrument-1".into(),
            provider_symbol: "SYM".into(),
            display_symbol: "SYM Display".into(),
            venue_id: "VENUE".into(),
            entitlement_id: "entitlement".into(),
            price_scale: 2,
            quantity_scale: 3,
            price_increment: Some(25),
        }
    }

    fn metadata(sequence: u64) -> EventMetadata {
        EventMetadata {
            provider_id: "provider-a".into(),
            instrument_id: "instrument-1".into(),
            entitlement_id: "entitlement".into(),
            source_sequence: sequence,
            session_generation: 7,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(1_700_000_000_000_000_001),
                provider_unix_nanos: None,
                received_unix_nanos: 1_700_000_000_000_000_500,
            },
        }
    }

    fn frames() -> Vec<RecordedFrame> {
        vec![
            RecordedFrame::Instrument(instrument()),
            RecordedFrame::Trade(MarketTrade {
                metadata: metadata(1),
                trade_id: "t-1".into(),
                price: 512_525,
                quantity: 2_000,
                aggressor: AggressorSide::Sell,
            }),
            RecordedFrame::Quote(TopOfBookQuote {
                metadata: metadata(2),
                bid: Some(QuoteLevel {
                    price: 512_500,
                    quantity: Some(4_000),
                    order_count: Some(3),
                }),
                ask: None,
            }),
            RecordedFrame::Gap(12),
            RecordedFrame::Depth(DepthSnapshot {
                metadata: metadata(3),
                bids: vec![
                    DepthLevel {
                        price: 512_500,
                        quantity: 4_000,
                        order_count: None,
                    },
                    DepthLevel {
                        price: 512_475,
                        quantity: 1_000,
                        order_count: Some(1),
                    },
                ],
                asks: vec![DepthLevel {
                    price: 512_550,
                    quantity: 9_000,
                    order_count: Some(5),
                }],
            }),
        ]
    }

    pub(crate) fn encode(frames: &[RecordedFrame]) -> Vec<u8> {
        let mut bytes = segment_header().to_vec();
        for frame in frames {
            let borrowed = match frame {
                RecordedFrame::Instrument(value) => FrameRef::Instrument(value),
                RecordedFrame::Trade(value) => FrameRef::Trade(value),
                RecordedFrame::Quote(value) => FrameRef::Quote(value),
                RecordedFrame::Depth(value) => FrameRef::Depth(value),
                RecordedFrame::Gap(value) => FrameRef::Gap(*value),
            };
            encode_frame(&mut bytes, &borrowed).expect("frame encodes");
        }
        bytes
    }

    #[test]
    fn segment_round_trips_every_frame_kind_exactly() {
        let frames = frames();
        let decoded = decode_segment(&encode(&frames));
        assert_eq!(decoded.tail, SegmentTail::Complete);
        assert_eq!(decoded.frames, frames);
    }

    #[test]
    fn a_truncated_tail_keeps_every_complete_frame() {
        let frames = frames();
        let bytes = encode(&frames);
        let complete = encode(&frames[..4]).len();
        for cut in [complete + 1, complete + FRAME_HEADER_BYTES, bytes.len() - 1] {
            let decoded = decode_segment(&bytes[..cut]);
            assert_eq!(decoded.frames, frames[..4], "cut at {cut}");
            assert_eq!(decoded.tail, SegmentTail::Truncated { offset: complete });
        }
        assert_eq!(
            decode_segment(&bytes[..SEGMENT_HEADER_BYTES - 1]).tail,
            SegmentTail::Truncated { offset: 0 }
        );
    }

    #[test]
    fn a_corrupt_frame_ends_reading_at_its_offset() {
        let frames = frames();
        let mut bytes = encode(&frames);
        let intact = encode(&frames[..2]).len();
        bytes[intact + FRAME_HEADER_BYTES + 3] ^= 0x40;
        let decoded = decode_segment(&bytes);
        assert_eq!(decoded.frames, frames[..2]);
        assert_eq!(decoded.tail, SegmentTail::Corrupt { offset: intact });

        let mut oversized = encode(&frames[..1]);
        oversized.extend_from_slice(&u32::MAX.to_le_bytes());
        oversized.extend_from_slice(&[0; CHECKSUM_BYTES]);
        assert_eq!(
            decode_segment(&oversized).tail,
            SegmentTail::Corrupt {
                offset: encode(&frames[..1]).len()
            }
        );

        let mut foreign = encode(&frames);
        foreign[1] = b'X';
        let decoded = decode_segment(&foreign);
        assert_eq!(decoded.frames, Vec::new());
        assert_eq!(decoded.tail, SegmentTail::Corrupt { offset: 0 });
    }

    #[test]
    fn an_event_before_any_instrument_frame_is_corrupt() {
        let frames = frames();
        let bytes = encode(&frames[1..2]);
        let decoded = decode_segment(&bytes);
        assert_eq!(decoded.frames, Vec::new());
        assert_eq!(
            decoded.tail,
            SegmentTail::Corrupt {
                offset: SEGMENT_HEADER_BYTES
            }
        );
    }

    #[test]
    fn an_oversized_field_leaves_the_buffer_unchanged() {
        let mut long = instrument();
        long.display_symbol = "x".repeat(usize::from(u16::MAX) + 1);
        let mut bytes = segment_header().to_vec();
        assert_eq!(
            encode_frame(&mut bytes, &FrameRef::Instrument(&long)),
            Err(EncodeError::FieldTooLong)
        );
        assert_eq!(bytes, segment_header());
    }
}
