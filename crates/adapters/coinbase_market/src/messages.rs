//! Strict serde DTOs for the advanced-trade WebSocket protocol.
//!
//! Only the fields this adapter consumes are modeled; unknown fields are
//! tolerated so additive provider changes never break the lane, while the
//! fields we do consume are mandatory and strictly typed.

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
pub(crate) enum UnsignedInteger<'a> {
    Number(u64),
    String(&'a str),
}

impl UnsignedInteger<'_> {
    #[cfg(test)]
    fn value(&self) -> Option<u64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::String(value) => value.parse().ok(),
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct ChannelMessage<'a> {
    #[serde(borrow)]
    pub(crate) channel: &'a str,
    #[serde(borrow)]
    pub(crate) timestamp: &'a str,
    pub(crate) sequence_num: u64,
    #[serde(default, borrow)]
    pub(crate) events: Vec<ChannelEvent<'a>>,
}

#[derive(Deserialize)]
pub(crate) struct ChannelEvent<'a> {
    #[serde(rename = "type", default)]
    #[allow(dead_code)]
    pub(crate) event_type: Option<&'a str>,
    #[serde(default, borrow)]
    pub(crate) trades: Vec<TradeMessage<'a>>,
    #[serde(default, borrow)]
    #[allow(dead_code)]
    pub(crate) heartbeat_counter: Option<UnsignedInteger<'a>>,
}

#[derive(Deserialize)]
pub(crate) struct TradeMessage<'a> {
    #[serde(borrow)]
    pub(crate) trade_id: &'a str,
    #[serde(borrow)]
    pub(crate) product_id: &'a str,
    #[serde(borrow)]
    pub(crate) price: &'a str,
    #[serde(borrow)]
    pub(crate) size: &'a str,
    #[serde(borrow)]
    pub(crate) side: &'a str,
    #[serde(borrow)]
    pub(crate) time: &'a str,
}

/// Builds one subscription frame for the public channels.
pub(crate) fn subscribe_frame(products: &[String], channel: &str) -> String {
    let product_list = products
        .iter()
        .map(|product| format!("\"{product}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"type\":\"subscribe\",\"product_ids\":[{product_list}],\"channel\":\"{channel}\"}}")
}

#[cfg(test)]
mod tests {
    use super::{ChannelMessage, subscribe_frame};

    #[test]
    fn documented_market_trades_message_parses() {
        let message: ChannelMessage = serde_json::from_str(
            r#"{"channel":"market_trades","timestamp":"2023-02-09T20:19:35.39625135Z","sequence_num":0,"events":[{"type":"snapshot","trades":[{"trade_id":"000000000","product_id":"ETH-USD","price":"1260.01","size":"0.3","side":"BUY","time":"2019-08-14T20:42:27.265Z"}]}]}"#,
        )
        .expect("documented message parses");
        assert_eq!(message.sequence_num, 0);
        assert_eq!(message.events[0].trades[0].price, "1260.01");
    }

    #[test]
    fn documented_heartbeat_parses() {
        let message: ChannelMessage = serde_json::from_str(
            r#"{"channel":"heartbeats","timestamp":"2023-06-23T20:31:26.122969572Z","sequence_num":0,"events":[{"current_time":"2023-06-23 20:31:56.121961769 +0000 UTC m=+91717.525857105","heartbeat_counter":"3049"}]}"#,
        )
        .expect("documented heartbeat parses");
        assert_eq!(
            message.events[0]
                .heartbeat_counter
                .as_ref()
                .and_then(super::UnsignedInteger::value),
            Some(3049)
        );
    }

    #[test]
    fn subscription_frame_matches_protocol_shape() {
        let frame = subscribe_frame(
            &["BTC-USD".to_string(), "ETH-USD".to_string()],
            "market_trades",
        );
        assert_eq!(
            frame,
            "{\"type\":\"subscribe\",\"product_ids\":[\"BTC-USD\",\"ETH-USD\"],\"channel\":\"market_trades\"}"
        );
    }
}
