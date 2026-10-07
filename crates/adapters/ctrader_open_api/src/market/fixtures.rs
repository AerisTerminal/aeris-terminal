//! Sanitized fixtures shaped like responses observed on a demo session on
//! 2026-10-08. Prices, periods and quote ids are faithful; the account id is a
//! placeholder.

use crate::ProtoMessage;
use prost::{
    Message,
    bytes::Buf,
    encoding::{WireType, decode_key, decode_varint, encode_key, encode_varint},
};

pub const CTID: u64 = 1001;
pub const CTID_WIRE: i64 = 1001;
pub const EURUSD: i64 = 1;
pub const USDJPY: i64 = 4;
pub const XAUUSD: i64 = 41;

pub fn frame(payload_type: u32, message: &impl Message) -> ProtoMessage {
    bytes_frame(payload_type, message.encode_to_vec())
}

pub fn bytes_frame(payload_type: u32, payload: Vec<u8>) -> ProtoMessage {
    ProtoMessage {
        payload_type,
        payload: Some(payload),
        client_msg_id: None,
    }
}

fn rewrite(payload: &[u8], mut edit: impl FnMut(u32, &[u8], &mut Vec<u8>) -> bool) -> Vec<u8> {
    let mut input = payload;
    let mut output = Vec::with_capacity(payload.len());
    while input.has_remaining() {
        let start = input;
        let (tag, wire) = decode_key(&mut input).expect("fixture key");
        let value_start = input;
        match wire {
            WireType::Varint => {
                decode_varint(&mut input).expect("fixture varint");
            }
            WireType::SixtyFourBit => input.advance(8),
            WireType::ThirtyTwoBit => input.advance(4),
            WireType::LengthDelimited => {
                let length = usize::try_from(decode_varint(&mut input).expect("length"))
                    .expect("fixture length");
                input.advance(length);
            }
            WireType::StartGroup | WireType::EndGroup => panic!("groups are not used"),
        }
        let consumed = start.len() - input.len();
        let field = &start[..consumed];
        let value = &value_start[..value_start.len() - input.len()];
        if !edit(tag, value, &mut output) {
            output.extend_from_slice(field);
        }
    }
    output
}

/// Remove every occurrence of a top-level field.
pub fn strip(payload: &[u8], tag: u32) -> Vec<u8> {
    rewrite(payload, |field, _, _| field == tag)
}

/// Remove `inner` from every occurrence of the nested message field `outer`.
pub fn strip_nested(payload: &[u8], outer: u32, inner: u32) -> Vec<u8> {
    rewrite(payload, |field, value, output| {
        if field != outer {
            return false;
        }
        let mut body = value;
        let length = usize::try_from(decode_varint(&mut body).expect("length")).expect("length");
        let nested = strip(&body[..length], inner);
        encode_key(outer, WireType::LengthDelimited, output);
        encode_varint(nested.len() as u64, output);
        output.extend_from_slice(&nested);
        true
    })
}
