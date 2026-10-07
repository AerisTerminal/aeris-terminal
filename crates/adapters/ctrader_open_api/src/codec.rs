use crate::generated::{ProtoMessage, ProtoOaGetAccountListByAccessTokenRes};
use prost::Message;
use std::{
    error::Error,
    fmt,
    io::{self, Read, Write},
};

pub const MAXIMUM_FRAME_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub enum CodecError {
    Io(io::Error),
    MalformedProtobuf(prost::DecodeError),
    InvalidWireField,
    FrameTooLarge { length: usize },
    UnexpectedPayloadType { expected: u32, actual: u32 },
    MissingRequiredField(&'static str),
}

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "cTrader frame I/O failed: {error}"),
            Self::MalformedProtobuf(error) => {
                write!(formatter, "invalid cTrader protobuf message: {error}")
            }
            Self::InvalidWireField => write!(formatter, "invalid cTrader protobuf wire field"),
            Self::FrameTooLarge { length } => {
                write!(formatter, "cTrader frame of {length} bytes exceeds 4 MiB")
            }
            Self::UnexpectedPayloadType { expected, actual } => {
                write!(
                    formatter,
                    "expected cTrader payload {expected}, received {actual}"
                )
            }
            Self::MissingRequiredField(field) => write!(formatter, "missing cTrader field {field}"),
        }
    }
}

impl Error for CodecError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::MalformedProtobuf(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for CodecError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<prost::DecodeError> for CodecError {
    fn from(error: prost::DecodeError) -> Self {
        Self::MalformedProtobuf(error)
    }
}

/// The prefix counts only the serialized `ProtoMessage`, not the four prefix bytes.
///
/// # Errors
/// Returns a frame-bound error before writing if the encoded message exceeds 4 MiB,
/// or an I/O error if the writer fails.
pub fn write_frame(writer: &mut impl Write, message: &ProtoMessage) -> Result<(), CodecError> {
    let length = message.encoded_len();
    if length > MAXIMUM_FRAME_BYTES {
        return Err(CodecError::FrameTooLarge { length });
    }
    let length = u32::try_from(length).map_err(|_| CodecError::FrameTooLarge { length })?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&message.encode_to_vec())?;
    Ok(())
}

/// Check the prefix before allocating any space for the encoded message.
///
/// # Errors
/// Returns an I/O, frame-bound, malformed-protobuf, or missing-field error.
pub fn read_frame(reader: &mut impl Read) -> Result<ProtoMessage, CodecError> {
    let mut prefix = [0; 4];
    reader.read_exact(&mut prefix)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length > MAXIMUM_FRAME_BYTES {
        return Err(CodecError::FrameTooLarge { length });
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    require_fields(&body, &[(1, "payloadType")])?;
    Ok(ProtoMessage::decode(body.as_slice())?)
}

fn take_varint(bytes: &mut &[u8]) -> Result<u64, CodecError> {
    let mut result = 0;
    for shift in (0..=63).step_by(7) {
        let (&byte, remaining) = bytes.split_first().ok_or(CodecError::InvalidWireField)?;
        *bytes = remaining;
        if shift == 63 && byte > 1 {
            return Err(CodecError::InvalidWireField);
        }
        result |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
    }
    Err(CodecError::InvalidWireField)
}

fn visit_fields<'a>(
    mut bytes: &'a [u8],
    mut visit: impl FnMut(u32, Option<&'a [u8]>) -> Result<(), CodecError>,
) -> Result<(), CodecError> {
    while !bytes.is_empty() {
        let key = take_varint(&mut bytes)?;
        let field = u32::try_from(key >> 3).map_err(|_| CodecError::InvalidWireField)?;
        if field == 0 {
            return Err(CodecError::InvalidWireField);
        }
        let data = match key & 7 {
            0 => {
                take_varint(&mut bytes)?;
                None
            }
            wire @ (1 | 5) => {
                let len = if wire == 1 { 8 } else { 4 };
                bytes = bytes.get(len..).ok_or(CodecError::InvalidWireField)?;
                None
            }
            2 => {
                let len = usize::try_from(take_varint(&mut bytes)?)
                    .map_err(|_| CodecError::InvalidWireField)?;
                let data = bytes.get(..len).ok_or(CodecError::InvalidWireField)?;
                bytes = &bytes[len..];
                Some(data)
            }
            _ => return Err(CodecError::InvalidWireField),
        };
        visit(field, data)?;
    }
    Ok(())
}

/// Prost materializes absent proto2 `required` fields as defaults, so validate their
/// actual wire presence before decoding rather than trusting the generated struct.
fn require_fields(bytes: &[u8], required: &[(u32, &'static str)]) -> Result<(), CodecError> {
    for &(tag, name) in required {
        let mut present = false;
        visit_fields(bytes, |field, _| {
            present |= field == tag;
            Ok(())
        })?;
        if !present {
            return Err(CodecError::MissingRequiredField(name));
        }
    }
    Ok(())
}

/// Nested proto2 messages have the same prost default-materialization hazard, so every
/// occurrence of a message-typed field must carry its own required wire fields.
///
/// # Errors
/// Returns a missing-field or malformed-wire error.
pub(crate) fn require_nested_fields(
    payload: &[u8],
    field: u32,
    required: &[(u32, &'static str)],
) -> Result<(), CodecError> {
    visit_fields(payload, |tag, data| {
        if tag == field {
            require_fields(data.ok_or(CodecError::InvalidWireField)?, required)?;
        }
        Ok(())
    })
}

/// Decode the expected wire type, then validate fields that the schema leaves optional
/// but that the adapter requires for correct behavior.
///
/// The caller must supply all proto2 `required` wire fields for the selected type
/// because prost represents absent required scalar fields with defaults.
///
/// # Errors
/// Returns an unexpected-type, missing-field, frame-bound, malformed-protobuf, or
/// caller-provided validation error.
pub fn decode_typed<M: Message + Default>(
    message: &ProtoMessage,
    expected_payload_type: u32,
    required_fields: &[(u32, &'static str)],
    validate: impl FnOnce(&M) -> Result<(), CodecError>,
) -> Result<M, CodecError> {
    if message.payload_type != expected_payload_type {
        return Err(CodecError::UnexpectedPayloadType {
            expected: expected_payload_type,
            actual: message.payload_type,
        });
    }
    let payload = message
        .payload
        .as_deref()
        .ok_or(CodecError::MissingRequiredField("payload"))?;
    if payload.len() > MAXIMUM_FRAME_BYTES {
        return Err(CodecError::FrameTooLarge {
            length: payload.len(),
        });
    }
    require_fields(payload, required_fields)?;
    let value = M::decode(payload)?;
    validate(&value)?;
    Ok(value)
}

/// Accounts without an explicit environment must never silently become demo accounts.
///
/// # Errors
/// Returns a codec error on wrong payload type, absent required wire fields,
/// or an account without `isLive`.
pub fn decode_account_list(
    message: &ProtoMessage,
) -> Result<ProtoOaGetAccountListByAccessTokenRes, CodecError> {
    decode_typed(
        message,
        2150,
        &[(2, "accessToken")],
        |list: &ProtoOaGetAccountListByAccessTokenRes| {
            require_nested_fields(
                message.payload.as_deref().unwrap_or_default(),
                4,
                &[(1, "ctidTraderAccountId")],
            )?;
            if list
                .ctid_trader_account
                .iter()
                .any(|account| account.is_live.is_none())
            {
                return Err(CodecError::MissingRequiredField("isLive"));
            }
            Ok(())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::{
        ProtoMessage, ProtoOaCtidTraderAccount, ProtoOaGetAccountListByAccessTokenRes,
        ProtoOaVersionRes,
    };
    use prost::Message;
    use std::io::{Cursor, Read};

    const VERSION_RESPONSE: u32 = 2105;
    const ACCOUNT_LIST_RESPONSE: u32 = 2150;

    fn version_frame() -> ProtoMessage {
        ProtoMessage {
            payload_type: VERSION_RESPONSE,
            payload: Some(
                ProtoOaVersionRes {
                    payload_type: None,
                    version: "102".into(),
                }
                .encode_to_vec(),
            ),
            client_msg_id: Some("request-1".into()),
        }
    }

    #[test]
    fn round_trip_preserves_big_endian_length_and_client_message_id() {
        let message = version_frame();
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &message).unwrap();
        assert_eq!(
            u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize,
            bytes.len() - 4
        );
        assert_eq!(read_frame(&mut Cursor::new(bytes)).unwrap(), message);
        let decoded: ProtoOaVersionRes =
            decode_typed(&message, VERSION_RESPONSE, &[(2, "version")], |_| Ok(())).unwrap();
        assert_eq!(decoded.version, "102");
    }

    struct OneByteAtATime<R>(R);

    impl<R: Read> Read for OneByteAtATime<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let limit = buf.len().min(1);
            self.0.read(&mut buf[..limit])
        }
    }

    #[test]
    fn split_prefix_and_body_reads_reassemble_one_frame() {
        let message = version_frame();
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &message).unwrap();
        assert_eq!(
            read_frame(&mut OneByteAtATime(Cursor::new(bytes))).unwrap(),
            message
        );
    }

    #[test]
    fn oversized_prefix_is_rejected_before_body_read() {
        let bytes = u32::try_from(MAXIMUM_FRAME_BYTES + 1)
            .unwrap()
            .to_be_bytes();
        assert!(matches!(
            read_frame(&mut Cursor::new(bytes)),
            Err(CodecError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn frame_exactly_at_limit_is_accepted() {
        let message = ProtoMessage {
            payload_type: VERSION_RESPONSE,
            payload: Some(vec![0; MAXIMUM_FRAME_BYTES - 8]),
            client_msg_id: None,
        };
        assert_eq!(message.encoded_len(), MAXIMUM_FRAME_BYTES);
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &message).unwrap();
        assert_eq!(read_frame(&mut Cursor::new(bytes)).unwrap(), message);
    }

    #[test]
    fn unexpected_payload_type_is_rejected_before_inner_decode() {
        let message = version_frame();
        assert!(matches!(
            decode_typed::<ProtoOaVersionRes>(
                &message,
                ACCOUNT_LIST_RESPONSE,
                &[(2, "version")],
                |_| Ok(())
            ),
            Err(CodecError::UnexpectedPayloadType {
                expected: ACCOUNT_LIST_RESPONSE,
                actual: VERSION_RESPONSE
            })
        ));
    }

    #[test]
    fn missing_proto2_required_field_is_rejected() {
        let message = ProtoMessage {
            payload_type: VERSION_RESPONSE,
            payload: Some(Vec::new()),
            client_msg_id: None,
        };
        assert!(matches!(
            decode_typed::<ProtoOaVersionRes>(
                &message,
                VERSION_RESPONSE,
                &[(2, "version")],
                |_| Ok(())
            ),
            Err(CodecError::MissingRequiredField("version"))
        ));
        assert!(matches!(
            read_frame(&mut Cursor::new(0u32.to_be_bytes())),
            Err(CodecError::MissingRequiredField("payloadType"))
        ));
    }

    #[test]
    fn missing_adapter_required_is_live_is_rejected() {
        let message = ProtoMessage {
            payload_type: ACCOUNT_LIST_RESPONSE,
            payload: Some(
                ProtoOaGetAccountListByAccessTokenRes {
                    payload_type: None,
                    access_token: "fixture-only".into(),
                    permission_scope: None,
                    ctid_trader_account: vec![ProtoOaCtidTraderAccount {
                        ctid_trader_account_id: 1,
                        is_live: None,
                        trader_login: None,
                        last_closing_deal_timestamp: None,
                        last_balance_update_timestamp: None,
                        broker_title_short: None,
                    }],
                }
                .encode_to_vec(),
            ),
            client_msg_id: None,
        };
        assert!(matches!(
            decode_account_list(&message),
            Err(CodecError::MissingRequiredField("isLive"))
        ));
    }

    #[test]
    fn missing_nested_proto2_required_account_id_is_rejected() {
        let message = ProtoMessage {
            payload_type: ACCOUNT_LIST_RESPONSE,
            // Access token (tag 2), followed by an empty account (tag 4).
            payload: Some(vec![0x12, 0x00, 0x22, 0x00]),
            client_msg_id: None,
        };
        assert!(matches!(
            decode_account_list(&message),
            Err(CodecError::MissingRequiredField("ctidTraderAccountId"))
        ));
    }
}
