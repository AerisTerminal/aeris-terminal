//! OKX V5 WebSocket framing, ping/pong, and subscription builders.

use aeris_crypto_common::{CryptoError, CryptoResult};
use serde::Deserialize;
use serde_json::json;

/// Parsed OKX V5 WebSocket message.
#[derive(Debug)]
pub enum OkxWsEvent {
    Pong,
    Ack,
    Books {
        inst_id: String,
        action: String,
        data: Box<serde_json::value::RawValue>,
    },
    Trades {
        inst_id: String,
        data: Box<serde_json::value::RawValue>,
    },
    Candle {
        inst_id: String,
        interval: String,
        data: Box<serde_json::value::RawValue>,
    },
    Ignored,
}

#[derive(Deserialize)]
struct WireEnvelope {
    #[serde(default)]
    event: Option<String>,
    #[serde(default)]
    arg: Option<WireArg>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    data: Option<Box<serde_json::value::RawValue>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireArg {
    channel: String,
    #[serde(default)]
    inst_id: String,
}

/// Returns OKX's plain-text application heartbeat (`"ping"`).
#[must_use]
pub fn build_ping() -> String {
    "ping".to_string()
}

/// Builds an OKX subscribe frame for `(channel, instId)` pairs.
#[must_use]
pub fn build_subscribe(args: &[(String, String)]) -> String {
    let entries: Vec<_> = args
        .iter()
        .map(|(channel, inst_id)| json!({ "channel": channel, "instId": inst_id }))
        .collect();
    json!({ "op": "subscribe", "args": entries }).to_string()
}

/// Builds an OKX unsubscribe frame for `(channel, instId)` pairs.
#[must_use]
pub fn build_unsubscribe(args: &[(String, String)]) -> String {
    let entries: Vec<_> = args
        .iter()
        .map(|(channel, inst_id)| json!({ "channel": channel, "instId": inst_id }))
        .collect();
    json!({ "op": "unsubscribe", "args": entries }).to_string()
}

/// Parses one OKX V5 WebSocket text frame.
///
/// # Errors
/// Returns an error if a non-pong frame is malformed JSON.
pub fn parse_ws_frame(text: &str) -> CryptoResult<OkxWsEvent> {
    if text == "pong" {
        return Ok(OkxWsEvent::Pong);
    }
    let env: WireEnvelope = serde_json::from_str(text)
        .map_err(|_| CryptoError::MalformedPayload("okx ws frame is malformed".to_string()))?;
    if env.event.is_some() {
        return Ok(OkxWsEvent::Ack);
    }
    if let (Some(arg), Some(data)) = (env.arg, env.data) {
        let inst_id = arg.inst_id.to_ascii_uppercase();
        if arg.channel == "books" {
            return Ok(OkxWsEvent::Books {
                inst_id,
                action: env.action.unwrap_or_default(),
                data,
            });
        }
        if arg.channel == "trades" {
            return Ok(OkxWsEvent::Trades { inst_id, data });
        }
        if let Some(interval) = arg.channel.strip_prefix("candle") {
            return Ok(OkxWsEvent::Candle {
                inst_id,
                interval: interval.to_string(),
                data,
            });
        }
    }
    Ok(OkxWsEvent::Ignored)
}
