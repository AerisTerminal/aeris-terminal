//! Coinbase Advanced Trade WebSocket framing and subscription builders.

use aeris_crypto_common::{CryptoError, CryptoResult};
use serde::Deserialize;
use serde_json::json;

use crate::trades::parse_rfc3339_nanos;

/// Parsed Coinbase Advanced Trade WebSocket frame.
#[derive(Debug)]
pub enum CoinbaseWsEvent {
    Heartbeat {
        sequence_num: u64,
    },
    Subscriptions,
    MarketTrades {
        sequence_num: u64,
        events: Box<serde_json::value::RawValue>,
    },
    L2Data {
        sequence_num: u64,
        timestamp_nanos: i64,
        events: Box<serde_json::value::RawValue>,
    },
    Ignored,
}

#[derive(Deserialize)]
struct WireFrame {
    #[serde(default)]
    channel: String,
    #[serde(default)]
    timestamp: String,
    #[serde(default)]
    sequence_num: u64,
    #[serde(default)]
    events: Option<Box<serde_json::value::RawValue>>,
}

/// Builds a Coinbase Advanced Trade WebSocket `subscribe` message.
#[must_use]
pub fn build_subscribe(channel: &str, product_ids: &[String]) -> String {
    if product_ids.is_empty() {
        json!({
            "type": "subscribe",
            "channel": channel,
        })
        .to_string()
    } else {
        json!({
            "type": "subscribe",
            "product_ids": product_ids,
            "channel": channel,
        })
        .to_string()
    }
}

/// Builds a Coinbase Advanced Trade WebSocket `unsubscribe` message.
#[must_use]
pub fn build_unsubscribe(channel: &str, product_ids: &[String]) -> String {
    if product_ids.is_empty() {
        json!({
            "type": "unsubscribe",
            "channel": channel,
        })
        .to_string()
    } else {
        json!({
            "type": "unsubscribe",
            "product_ids": product_ids,
            "channel": channel,
        })
        .to_string()
    }
}

/// Parses one Coinbase Advanced Trade WebSocket JSON frame.
///
/// # Errors
/// Returns an error if the text is malformed JSON.
pub fn parse_ws_frame(text: &str) -> CryptoResult<CoinbaseWsEvent> {
    let frame: WireFrame = serde_json::from_str(text)
        .map_err(|_| CryptoError::MalformedPayload("coinbase ws frame is malformed".to_string()))?;
    match frame.channel.as_str() {
        "heartbeats" => Ok(CoinbaseWsEvent::Heartbeat {
            sequence_num: frame.sequence_num,
        }),
        "subscriptions" => Ok(CoinbaseWsEvent::Subscriptions),
        "market_trades" => match frame.events {
            Some(events) => Ok(CoinbaseWsEvent::MarketTrades {
                sequence_num: frame.sequence_num,
                events,
            }),
            None => Ok(CoinbaseWsEvent::Ignored),
        },
        "l2_data" => match frame.events {
            Some(events) => {
                let timestamp_nanos = parse_rfc3339_nanos(&frame.timestamp).unwrap_or(0);
                Ok(CoinbaseWsEvent::L2Data {
                    sequence_num: frame.sequence_num,
                    timestamp_nanos,
                    events,
                })
            }
            None => Ok(CoinbaseWsEvent::Ignored),
        },
        _ => Ok(CoinbaseWsEvent::Ignored),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_subscribe_and_parses_channels() {
        let hb_sub = build_subscribe("heartbeats", &[]);
        assert!(hb_sub.contains("\"channel\":\"heartbeats\""));
        assert!(!hb_sub.contains("product_ids"));

        let trades_sub = build_subscribe("market_trades", &["BTC-USD".to_string()]);
        assert!(trades_sub.contains("\"BTC-USD\""));

        let hb = parse_ws_frame(r#"{"channel":"heartbeats","sequence_num":42,"events":[]}"#)
            .expect("heartbeat");
        assert!(matches!(
            hb,
            CoinbaseWsEvent::Heartbeat { sequence_num: 42 }
        ));

        let l2 = parse_ws_frame(
            r#"{"channel":"l2_data","timestamp":"2026-11-25T14:38:52.100000Z","sequence_num":43,"events":[]}"#,
        )
        .expect("l2_data");
        assert!(matches!(
            l2,
            CoinbaseWsEvent::L2Data {
                sequence_num: 43,
                ..
            }
        ));
    }

    #[test]
    #[ignore = "drives live public Coinbase WebSocket feed"]
    fn live_coinbase_ws_streams_heartbeat_trades_and_l2() {
        use std::{
            sync::{Arc, atomic::AtomicBool},
            time::{Duration, Instant},
        };

        use aeris_platform_runtime::{MarketSocket, MarketSocketEvent};

        use crate::{COINBASE_WS_URL, CoinbaseBookState, decode_l2_events, decode_ws_trades};

        let stop = Arc::new(AtomicBool::new(false));
        let (mut sock, _) = MarketSocket::connect(COINBASE_WS_URL, Duration::from_secs(10), &stop)
            .expect("connect coinbase ws");
        sock.send_text(&build_subscribe("heartbeats", &[]))
            .expect("sub heartbeats");
        sock.send_text(&build_subscribe("market_trades", &["BTC-USD".to_string()]))
            .expect("sub market_trades");
        sock.send_text(&build_subscribe("level2", &["BTC-USD".to_string()]))
            .expect("sub level2");

        let mut got_hb = false;
        let mut trade_count = 0usize;
        let mut book = CoinbaseBookState::new();
        let mut seq = 0u64;
        let deadline = Instant::now() + Duration::from_secs(8);

        while Instant::now() < deadline {
            match sock.read_event(Instant::now() + Duration::from_millis(500)) {
                Ok(MarketSocketEvent::Text(text)) => {
                    match parse_ws_frame(&text).expect("valid ws json") {
                        CoinbaseWsEvent::Heartbeat { .. } => got_hb = true,
                        CoinbaseWsEvent::MarketTrades { events, .. } => {
                            let decoded = decode_ws_trades(
                                &events,
                                |p| {
                                    Some((
                                        format!("coinbase:spot:{p}"),
                                        "coinbase-public".to_string(),
                                    ))
                                },
                                1,
                                &mut seq,
                                1_795_617_600_000_000_000,
                            )
                            .expect("decode live ws trades");
                            for (product_id, _, _) in decoded {
                                if product_id == "BTC-USD" {
                                    trade_count += 1;
                                }
                            }
                        }
                        CoinbaseWsEvent::L2Data {
                            timestamp_nanos,
                            events,
                            ..
                        } => {
                            let msgs =
                                decode_l2_events(&events, timestamp_nanos).expect("decode live l2");
                            for msg in msgs {
                                if msg.product_id == "BTC-USD" {
                                    let _ = book.apply(&msg);
                                }
                            }
                        }
                        CoinbaseWsEvent::Subscriptions | CoinbaseWsEvent::Ignored => {}
                    }
                    if got_hb && trade_count > 0 && book.is_synced() {
                        break;
                    }
                }
                Ok(MarketSocketEvent::Pong) => {}
                Err(err) if err.is_read_timeout() => {}
                Err(err) => panic!("ws read error: {err}"),
            }
        }
        sock.close();
        assert!(got_hb, "expected heartbeat");
        assert!(trade_count > 0, "expected BTC-USD live trades");
        assert!(book.is_synced(), "expected BTC-USD L2 book sync");
        assert!(book.local_book().best_bid().is_some());
        assert!(book.local_book().best_ask().is_some());
    }
}
