use crate::ProtocolError;

#[cfg(rithmic_kit)]
const MAX_FRAME_BYTES: usize = 1024 * 1024;
#[cfg(rithmic_kit)]
const MAX_FIELD_BYTES: usize = 256;
#[cfg(rithmic_kit)]
const MAX_TICK_KEYS: usize = 10_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BarIdentity {
    pub symbol: String,
    pub exchange: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ohlc {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodedTimeBarType {
    Second,
    Minute,
    Daily,
    Weekly,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecodedTimeBar {
    pub identity: BarIdentity,
    pub bar_type: DecodedTimeBarType,
    pub period: String,
    pub marker_seconds: i32,
    pub ohlc: Ohlc,
    pub trades: Option<u64>,
    pub volume: Option<u64>,
    pub bid_volume: Option<u64>,
    pub ask_volume: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TickBarKey {
    pub sequence: String,
    pub seconds: i32,
    pub microseconds: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecodedTickBar {
    pub identity: BarIdentity,
    pub trades_per_bar: String,
    pub keys: Vec<TickBarKey>,
    pub ohlc: Ohlc,
    pub trades: Option<u64>,
    pub volume: Option<u64>,
    pub bid_volume: Option<u64>,
    pub ask_volume: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayKind {
    Time,
    Tick,
}

/// Whether a bar came from a live subscription or a replay response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistorySource {
    Live,
    Replay,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DecodedHistoryMessage {
    TimeBar {
        source: HistorySource,
        bar: DecodedTimeBar,
    },
    TickBar {
        source: HistorySource,
        bar: DecodedTickBar,
    },
    ReplayComplete {
        kind: ReplayKind,
        accepted: bool,
    },
}

#[cfg(rithmic_kit)]
pub(crate) fn decode(frame: &[u8]) -> Result<DecodedHistoryMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    bound_frame(frame)?;
    match rti::MessageType::decode(frame)
        .map_err(|_| ProtocolError::Decode)?
        .template_id
    {
        203 => decode_time_replay(frame),
        207 => decode_tick_replay(frame),
        250 => decode_live_time(frame),
        251 => decode_live_tick(frame),
        template => Err(ProtocolError::UnsupportedTemplate(template)),
    }
}

#[cfg(not(rithmic_kit))]
pub(crate) fn decode(_frame: &[u8]) -> Result<DecodedHistoryMessage, ProtocolError> {
    Err(ProtocolError::KitUnavailable)
}

#[cfg(rithmic_kit)]
fn decode_time_replay(frame: &[u8]) -> Result<DecodedHistoryMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    let message = rti::ResponseTimeBarReplay::decode(frame).map_err(|_| ProtocolError::Decode)?;
    validate_codes(
        &message.user_msg,
        &message.rq_handler_rp_code,
        &message.rp_code,
    )?;
    match code_shape(&message.rq_handler_rp_code, &message.rp_code)? {
        CodeShape::Terminal(accepted) => Ok(DecodedHistoryMessage::ReplayComplete {
            kind: ReplayKind::Time,
            accepted,
        }),
        CodeShape::Data => Ok(DecodedHistoryMessage::TimeBar {
            source: HistorySource::Replay,
            bar: time_bar(
                message.symbol,
                message.exchange,
                message.r#type,
                message.period,
                message.marker,
                message.open_price,
                message.high_price,
                message.low_price,
                message.close_price,
                message.num_trades,
                message.volume,
                message.bid_volume,
                message.ask_volume,
            )?,
        }),
    }
}

#[cfg(rithmic_kit)]
fn decode_live_time(frame: &[u8]) -> Result<DecodedHistoryMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    let message = rti::TimeBar::decode(frame).map_err(|_| ProtocolError::Decode)?;
    Ok(DecodedHistoryMessage::TimeBar {
        source: HistorySource::Live,
        bar: time_bar(
            message.symbol,
            message.exchange,
            message.r#type,
            message.period,
            message.marker,
            message.open_price,
            message.high_price,
            message.low_price,
            message.close_price,
            message.num_trades,
            message.volume,
            message.bid_volume,
            message.ask_volume,
        )?,
    })
}

#[cfg(rithmic_kit)]
fn decode_tick_replay(frame: &[u8]) -> Result<DecodedHistoryMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    let message = rti::ResponseTickBarReplay::decode(frame).map_err(|_| ProtocolError::Decode)?;
    validate_codes(
        &message.user_msg,
        &message.rq_handler_rp_code,
        &message.rp_code,
    )?;
    match code_shape(&message.rq_handler_rp_code, &message.rp_code)? {
        CodeShape::Terminal(accepted) => Ok(DecodedHistoryMessage::ReplayComplete {
            kind: ReplayKind::Tick,
            accepted,
        }),
        CodeShape::Data => Ok(DecodedHistoryMessage::TickBar {
            source: HistorySource::Replay,
            bar: tick_bar(
                message.symbol,
                message.exchange,
                message.r#type,
                message.sub_type,
                message.type_specifier,
                message.data_bar_seq_num,
                message.data_bar_ssboe,
                message.data_bar_usecs,
                message.open_price,
                message.high_price,
                message.low_price,
                message.close_price,
                message.num_trades,
                message.volume,
                message.bid_volume,
                message.ask_volume,
            )?,
        }),
    }
}

#[cfg(rithmic_kit)]
fn decode_live_tick(frame: &[u8]) -> Result<DecodedHistoryMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    let message = rti::TickBar::decode(frame).map_err(|_| ProtocolError::Decode)?;
    Ok(DecodedHistoryMessage::TickBar {
        source: HistorySource::Live,
        bar: tick_bar(
            message.symbol,
            message.exchange,
            message.r#type,
            message.sub_type,
            message.type_specifier,
            message.data_bar_seq_num,
            message.data_bar_ssboe,
            message.data_bar_usecs,
            message.open_price,
            message.high_price,
            message.low_price,
            message.close_price,
            message.num_trades,
            message.volume,
            message.bid_volume,
            message.ask_volume,
        )?,
    })
}

#[cfg(rithmic_kit)]
#[allow(clippy::too_many_arguments)]
fn time_bar(
    symbol: Option<String>,
    exchange: Option<String>,
    bar_type: Option<i32>,
    period: Option<String>,
    marker: Option<i32>,
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    close: Option<f64>,
    trades: Option<u64>,
    volume: Option<u64>,
    bid_volume: Option<u64>,
    ask_volume: Option<u64>,
) -> Result<DecodedTimeBar, ProtocolError> {
    let bar_type = match bar_type {
        Some(1) => DecodedTimeBarType::Second,
        Some(2) => DecodedTimeBarType::Minute,
        Some(3) => DecodedTimeBarType::Daily,
        Some(4) => DecodedTimeBarType::Weekly,
        _ => return Err(ProtocolError::UnknownEnum("time_bar.type")),
    };
    let marker_seconds = marker.ok_or(ProtocolError::MissingField("time_bar.marker"))?;
    if marker_seconds < 0 {
        return Err(ProtocolError::InvalidNumber("time_bar.marker"));
    }
    Ok(DecodedTimeBar {
        identity: identity(symbol, exchange)?,
        bar_type,
        period: required_string("time_bar.period", period)?,
        marker_seconds,
        ohlc: ohlc(open, high, low, close)?,
        trades,
        volume,
        bid_volume,
        ask_volume,
    })
}

#[cfg(rithmic_kit)]
#[allow(clippy::too_many_arguments)]
fn tick_bar(
    symbol: Option<String>,
    exchange: Option<String>,
    bar_type: Option<i32>,
    sub_type: Option<i32>,
    type_specifier: Option<String>,
    sequences: Vec<String>,
    seconds: Vec<i32>,
    microseconds: Vec<i32>,
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    close: Option<f64>,
    trades: Option<u64>,
    volume: Option<u64>,
    bid_volume: Option<u64>,
    ask_volume: Option<u64>,
) -> Result<DecodedTickBar, ProtocolError> {
    if bar_type != Some(1) || sub_type != Some(1) {
        return Err(ProtocolError::UnknownEnum("tick_bar.type"));
    }
    if sequences.is_empty()
        || sequences.len() > MAX_TICK_KEYS
        || sequences.len() != seconds.len()
        || sequences.len() != microseconds.len()
    {
        return Err(ProtocolError::ParallelFieldLength("tick_bar.keys"));
    }
    let keys = sequences
        .into_iter()
        .zip(seconds)
        .zip(microseconds)
        .map(|((sequence, seconds), microseconds)| {
            validate_string("tick_bar.sequence", &sequence)?;
            if seconds < 0 || !(0..1_000_000).contains(&microseconds) {
                return Err(ProtocolError::InvalidNumber("tick_bar.timestamp"));
            }
            Ok(TickBarKey {
                sequence,
                seconds,
                microseconds,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DecodedTickBar {
        identity: identity(symbol, exchange)?,
        trades_per_bar: required_string("tick_bar.type_specifier", type_specifier)?,
        keys,
        ohlc: ohlc(open, high, low, close)?,
        trades,
        volume,
        bid_volume,
        ask_volume,
    })
}

#[cfg(rithmic_kit)]
fn ohlc(
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    close: Option<f64>,
) -> Result<Ohlc, ProtocolError> {
    let [open, high, low, close] = [open, high, low, close]
        .map(|value| value.ok_or(ProtocolError::MissingField("bar.ohlc")))
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .try_into()
        .map_err(|_| ProtocolError::Decode)?;
    if [open, high, low, close]
        .into_iter()
        .any(|value| !value.is_finite())
        || low > high
        || !(low..=high).contains(&open)
        || !(low..=high).contains(&close)
    {
        return Err(ProtocolError::InvalidNumber("bar.ohlc"));
    }
    Ok(Ohlc {
        open,
        high,
        low,
        close,
    })
}

#[cfg(rithmic_kit)]
enum CodeShape {
    Data,
    Terminal(bool),
}

#[cfg(rithmic_kit)]
fn code_shape(handler: &[String], terminal: &[String]) -> Result<CodeShape, ProtocolError> {
    match (handler.is_empty(), terminal.is_empty()) {
        (false, true) if accepted(handler) => Ok(CodeShape::Data),
        (false, true) => Err(ProtocolError::RejectedDataFrame),
        (true, false) => Ok(CodeShape::Terminal(accepted(terminal))),
        _ => Err(ProtocolError::ResponseCodeShape),
    }
}

#[cfg(rithmic_kit)]
fn validate_codes(
    user_messages: &[String],
    handler: &[String],
    terminal: &[String],
) -> Result<(), ProtocolError> {
    if user_messages.len() > 2 {
        return Err(ProtocolError::RepeatedFieldLimitExceeded {
            field: "user_msg",
            maximum: 2,
        });
    }
    for value in user_messages {
        validate_string("user_msg", value)?;
    }
    validate_code_field("rq_handler_rp_code", handler)?;
    validate_code_field("rp_code", terminal)
}

#[cfg(rithmic_kit)]
fn validate_code_field(field: &'static str, codes: &[String]) -> Result<(), ProtocolError> {
    match codes {
        [] => Ok(()),
        [code] if code == "0" => Ok(()),
        [code, detail] if code.parse::<u32>().is_ok_and(|value| value > 0) => {
            validate_string(field, code)?;
            validate_string(field, detail)
        }
        _ => Err(ProtocolError::ResponseCodeShape),
    }
}

#[cfg(rithmic_kit)]
fn accepted(codes: &[String]) -> bool {
    codes.len() == 1 && codes[0] == "0"
}

#[cfg(rithmic_kit)]
fn identity(
    symbol: Option<String>,
    exchange: Option<String>,
) -> Result<BarIdentity, ProtocolError> {
    Ok(BarIdentity {
        symbol: required_string("bar.symbol", symbol)?,
        exchange: required_string("bar.exchange", exchange)?,
    })
}

#[cfg(rithmic_kit)]
fn required_string(field: &'static str, value: Option<String>) -> Result<String, ProtocolError> {
    let value = value.ok_or(ProtocolError::MissingField(field))?;
    validate_string(field, &value)?;
    Ok(value)
}

#[cfg(rithmic_kit)]
fn validate_string(field: &'static str, value: &str) -> Result<(), ProtocolError> {
    if value.is_empty() {
        return Err(ProtocolError::EmptyField(field));
    }
    if value.len() > MAX_FIELD_BYTES {
        return Err(ProtocolError::FieldTooLong {
            field,
            maximum: MAX_FIELD_BYTES,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(ProtocolError::ControlCharacter(field));
    }
    Ok(())
}

#[cfg(rithmic_kit)]
fn bound_frame(frame: &[u8]) -> Result<(), ProtocolError> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge {
            requested: frame.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }
    Ok(())
}

#[cfg(all(test, rithmic_kit))]
mod tests {
    use super::*;
    use crate::{RithmicProtocolCodec, generated::rti};
    use prost::Message;

    #[test]
    fn decodes_live_bars_and_explicit_replay_completion() {
        let codec = RithmicProtocolCodec;
        let time = rti::TimeBar {
            template_id: 250,
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            r#type: Some(rti::time_bar::BarType::MinuteBar.into()),
            period: Some("1".to_string()),
            marker: Some(1_800_000_000),
            num_trades: Some(10),
            volume: Some(20),
            bid_volume: Some(8),
            ask_volume: Some(12),
            open_price: Some(5_100.0),
            close_price: Some(5_101.0),
            high_price: Some(5_102.0),
            low_price: Some(5_099.0),
            settlement_price: None,
            has_settlement_price: None,
            must_clear_settlement_price: None,
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_history(&time).expect("time bar decodes"),
            DecodedHistoryMessage::TimeBar {
                source: HistorySource::Live,
                bar: DecodedTimeBar {
                    bar_type: DecodedTimeBarType::Minute,
                    marker_seconds: 1_800_000_000,
                    ..
                },
            }
        ));

        let tick = rti::TickBar {
            template_id: 251,
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            r#type: Some(rti::tick_bar::BarType::TickBar.into()),
            sub_type: Some(rti::tick_bar::BarSubType::Regular.into()),
            type_specifier: Some("100".to_string()),
            data_bar_seq_num: vec!["opaque-1".to_string()],
            num_trades: Some(100),
            volume: Some(150),
            bid_volume: Some(70),
            ask_volume: Some(80),
            open_price: Some(5_100.0),
            close_price: Some(5_101.0),
            high_price: Some(5_102.0),
            low_price: Some(5_099.0),
            custom_session_open_ssm: None,
            data_bar_ssboe: vec![1_800_000_000],
            data_bar_usecs: vec![123_456],
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_history(&tick).expect("tick bar decodes"),
            DecodedHistoryMessage::TickBar {
                source: HistorySource::Live,
                bar: DecodedTickBar { keys, .. },
            }
                if keys[0].sequence == "opaque-1"
        ));

        let terminal = rti::ResponseTimeBarReplay {
            template_id: 203,
            rp_code: vec!["0".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(
            codec.decode_history(&terminal).expect("terminal decodes"),
            DecodedHistoryMessage::ReplayComplete {
                kind: ReplayKind::Time,
                accepted: true,
            }
        );
    }

    #[test]
    fn invalid_ohlc_and_tick_key_shape_fail_closed() {
        let codec = RithmicProtocolCodec;
        let invalid = rti::TimeBar {
            template_id: 250,
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            r#type: Some(rti::time_bar::BarType::MinuteBar.into()),
            period: Some("1".to_string()),
            marker: Some(1_800_000_000),
            open_price: Some(5_103.0),
            close_price: Some(5_101.0),
            high_price: Some(5_102.0),
            low_price: Some(5_099.0),
            ..Default::default()
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_history(&invalid),
            Err(ProtocolError::InvalidNumber("bar.ohlc"))
        ));

        let mismatched = rti::TickBar {
            template_id: 251,
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            r#type: Some(rti::tick_bar::BarType::TickBar.into()),
            sub_type: Some(rti::tick_bar::BarSubType::Regular.into()),
            type_specifier: Some("100".to_string()),
            data_bar_seq_num: vec!["opaque-1".to_string()],
            data_bar_ssboe: vec![1_800_000_000],
            data_bar_usecs: Vec::new(),
            open_price: Some(5_100.0),
            close_price: Some(5_101.0),
            high_price: Some(5_102.0),
            low_price: Some(5_099.0),
            ..Default::default()
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_history(&mismatched),
            Err(ProtocolError::ParallelFieldLength("tick_bar.keys"))
        ));

        let malformed_rejection = rti::ResponseTimeBarReplay {
            template_id: 203,
            rp_code: vec!["1".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_history(&malformed_rejection),
            Err(ProtocolError::ResponseCodeShape)
        ));

        let rejection = rti::ResponseTimeBarReplay {
            template_id: 203,
            rp_code: vec!["1".to_string(), "rejected".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(
            codec.decode_history(&rejection).expect("rejection decodes"),
            DecodedHistoryMessage::ReplayComplete {
                kind: ReplayKind::Time,
                accepted: false,
            }
        );
    }
}
