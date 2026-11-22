//! Bybit V5 WebSocket framing, heartbeat, and subscription builders.

use aeris_crypto_common::{CryptoError, CryptoResult};
use serde::Deserialize;
use serde_json::json;

/// Parsed Bybit V5 WebSocket message.
#[derive(Debug)]
pub enum BybitWsEvent {
    Pong,
    Ack,
    Orderbook {
        symbol: String,
        msg_type: String,
        ts_ms: i64,
        data: Box<serde_json::value::RawValue>,
    },
    Trades {
        symbol: String,
        data: Box<serde_json::value::RawValue>,
    },
    Kline {
        symbol: String,
        data: Box<serde_json::value::RawValue>,
    },
    Ignored,
}

#[derive(Deserialize)]
struct WireWsEnvelope {
    #[serde(default)]
    op: Option<String>,
    #[serde(default)]
    ret_msg: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(rename = "type", default)]
    msg_type: Option<String>,
    #[serde(default)]
    ts: Option<i64>,
    #[serde(default)]
    data: Option<Box<serde_json::value::RawValue>>,
}

/// Builds a Bybit V5 application-level ping frame.
#[must_use]
pub fn build_ping() -> String {
    r#"{"op":"ping"}"#.to_string()
}

/// Builds a Bybit V5 subscribe frame.
#[must_use]
pub fn build_subscribe(args: &[String]) -> String {
    json!({ "op": "subscribe", "args": args }).to_string()
}

/// Builds a Bybit V5 unsubscribe frame.
#[must_use]
pub fn build_unsubscribe(args: &[String]) -> String {
    json!({ "op": "unsubscribe", "args": args }).to_string()
}

/// Parses one Bybit V5 WebSocket text frame.
///
/// # Errors
/// Returns an error if the JSON envelope is malformed.
pub fn parse_ws_frame(text: &str) -> CryptoResult<BybitWsEvent> {
    let env: WireWsEnvelope = serde_json::from_str(text)
        .map_err(|_| CryptoError::MalformedPayload("bybit ws frame is malformed".to_string()))?;
    if env.op.as_deref() == Some("pong") || env.ret_msg.as_deref() == Some("pong") {
        return Ok(BybitWsEvent::Pong);
    }
    if let (Some(topic), Some(data)) = (env.topic, env.data) {
        if let Some(rest) = topic.strip_prefix("orderbook.") {
            if let Some((_, symbol)) = rest.split_once('.') {
                return Ok(BybitWsEvent::Orderbook {
                    symbol: symbol.to_ascii_uppercase(),
                    msg_type: env.msg_type.unwrap_or_default(),
                    ts_ms: env.ts.unwrap_or(0),
                    data,
                });
            }
        } else if let Some(symbol) = topic.strip_prefix("publicTrade.") {
            return Ok(BybitWsEvent::Trades {
                symbol: symbol.to_ascii_uppercase(),
                data,
            });
        } else if let Some(rest) = topic.strip_prefix("kline.")
            && let Some((_, symbol)) = rest.split_once('.')
        {
            return Ok(BybitWsEvent::Kline {
                symbol: symbol.to_ascii_uppercase(),
                data,
            });
        }
        return Ok(BybitWsEvent::Ignored);
    }
    if env.op.is_some() {
        return Ok(BybitWsEvent::Ack);
    }
    Ok(BybitWsEvent::Ignored)
}
