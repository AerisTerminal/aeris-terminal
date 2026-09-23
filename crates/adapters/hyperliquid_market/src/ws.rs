//! Multiplexed WebSocket subscription mechanics.
//!
//! The engine owns exactly one multiplexed public WebSocket. This module
//! builds subscribe/unsubscribe frames, parses inbound frames into
//! provider-neutral client events, and exposes the heartbeat contract. It
//! performs no I/O by itself so the engine can bound, cancel, and fairly
//! schedule work across instruments.
//!
//! Event payloads travel as raw JSON text end to end: fractional numbers
//! never pass through `f64`, so provider decimals reach the fixed-point
//! decoders exactly as emitted.

use serde::Deserialize;

/// One parsed inbound WebSocket frame.
///
/// Payloads are raw JSON text, which has no structural equality, so events
/// compare by matching rather than `PartialEq`.
#[derive(Clone, Debug)]
pub enum WsClientEvent {
    /// Channel subscription acknowledgement.
    Subscribed { channel: String },
    /// Heartbeat reply.
    Pong,
    /// Live candle replacement(s) for a coin and interval.
    Candle {
        coin: String,
        interval: String,
        candles: Box<serde_json::value::RawValue>,
    },
    /// Trade batch for a coin.
    Trades {
        coin: String,
        trades: Box<serde_json::value::RawValue>,
    },
    /// Full book snapshot image for a coin (replaces the previous image).
    Book {
        coin: String,
        book: Box<serde_json::value::RawValue>,
    },
    /// Best-bid/best-ask update for a coin.
    Bbo {
        coin: String,
        bbo: Box<serde_json::value::RawValue>,
    },
    /// Market context update for a coin.
    Context {
        coin: String,
        context: Box<serde_json::value::RawValue>,
    },
    /// All-mids price map (display context, not candle input).
    AllMids {
        mids: Box<serde_json::value::RawValue>,
    },
}

/// Builds a candle subscription frame for a coin and interval.
///
/// # Errors
///
/// Returns an error for blank or overlong coins or intervals.
pub fn build_candle_subscription(coin: &str, interval: &str) -> Result<String, String> {
    checked_coin(coin)?;
    if interval.trim().is_empty() || interval.len() > 8 {
        return Err("hyperliquid candle interval is invalid".to_string());
    }
    Ok(serde_json::json!({
        "method": "subscribe",
        "subscription": {"type": "candle", "coin": coin, "interval": interval},
    })
    .to_string())
}

/// Builds a trades subscription frame for a coin.
///
/// # Errors
///
/// Returns an error for a blank or overlong coin.
pub fn build_trades_subscription(coin: &str) -> Result<String, String> {
    checked_coin(coin)?;
    Ok(serde_json::json!({
        "method": "subscribe",
        "subscription": {"type": "trades", "coin": coin},
    })
    .to_string())
}

/// Builds the standard public L2 book subscription frame for a coin.
///
/// # Errors
///
/// Returns an error for a blank or overlong coin.
pub fn build_l2_subscription(coin: &str) -> Result<String, String> {
    checked_coin(coin)?;
    Ok(serde_json::json!({
        "method": "subscribe",
        "subscription": {"type": "l2Book", "coin": coin},
    })
    .to_string())
}

/// Builds a provider-aggregated public L2 book subscription.
///
/// Hyperliquid supports two through five significant figures, with an
/// optional 1/2/5 mantissa only for the five-significant-figure form. The
/// returned book is still a complete provider snapshot; the aggregation only
/// controls the price lattice represented by its bounded levels.
///
/// # Errors
///
/// Returns an error for invalid coin identity or unsupported aggregation.
pub fn build_aggregated_l2_subscription(
    coin: &str,
    n_sig_figs: u8,
    mantissa: Option<u8>,
) -> Result<String, String> {
    checked_coin(coin)?;
    if !(2..=5).contains(&n_sig_figs) {
        return Err("hyperliquid L2 significant figures are invalid".to_string());
    }
    if mantissa.is_some_and(|value| n_sig_figs != 5 || !matches!(value, 1 | 2 | 5)) {
        return Err("hyperliquid L2 mantissa is invalid".to_string());
    }
    let mut subscription = serde_json::json!({
        "type": "l2Book",
        "coin": coin,
        "nSigFigs": n_sig_figs,
    });
    if let Some(mantissa) = mantissa.filter(|value| *value != 1) {
        subscription["mantissa"] = serde_json::json!(mantissa);
    }
    Ok(serde_json::json!({
        "method": "subscribe",
        "subscription": subscription,
    })
    .to_string())
}

/// Builds a BBO subscription frame for a coin.
///
/// # Errors
///
/// Returns an error for a blank or overlong coin.
pub fn build_bbo_subscription(coin: &str) -> Result<String, String> {
    checked_coin(coin)?;
    Ok(serde_json::json!({
        "method": "subscribe",
        "subscription": {"type": "bbo", "coin": coin},
    })
    .to_string())
}

/// Builds an unsubscribe frame for a previously sent subscription payload.
#[must_use]
pub fn build_unsubscribe(subscription: &serde_json::Value) -> String {
    serde_json::json!({"method": "unsubscribe", "subscription": subscription}).to_string()
}

/// Serializes the heartbeat ping frame.
#[must_use]
pub fn build_ping() -> String {
    serde_json::json!({"method": "ping"}).to_string()
}

fn checked_coin(coin: &str) -> Result<(), String> {
    if coin.trim().is_empty() || coin.len() > 96 {
        return Err("hyperliquid subscription coin is invalid".to_string());
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct WireFrame {
    #[serde(default)]
    channel: String,
    #[serde(default)]
    data: Option<Box<serde_json::value::RawValue>>,
}

/// Parses one inbound text frame into a client event.
///
/// Payloads stay raw text so fractional numbers never pass through `f64`;
/// only routing strings (`coin`, `s`, `i`) are read through a lossy parse,
/// which is exact for strings. Unknown channels and control messages are
/// returned as errors carrying the channel name so the caller can keep
/// control traffic deliverable under market-data pressure without
/// misrouting it into the book/trade path.
///
/// # Errors
///
/// Returns an error for oversized or malformed frames, missing data or coin
/// identity, mixed-market frames, or unsupported channels.
pub fn parse_ws_frame(text: &str) -> Result<WsClientEvent, String> {
    if text.len() > 4 * 1024 * 1024 {
        return Err("hyperliquid frame exceeds the bound".to_string());
    }
    let frame: WireFrame =
        serde_json::from_str(text).map_err(|_| "hyperliquid frame is malformed".to_string())?;
    match frame.channel.as_str() {
        "pong" => Ok(WsClientEvent::Pong),
        "subscriptionResponse" | "subscribed" => Ok(WsClientEvent::Subscribed {
            channel: frame
                .data
                .as_deref()
                .map_or_else(String::new, |data| data.get().to_string()),
        }),
        "candle" => {
            // Live candle updates arrive as a single `Candle` object per
            // frame on the observed feed (the documented snapshot shape is
            // `Candle[]`; both decode). Each record carries the coin in `s`
            // and the interval in `i` (not `coin`/`interval`).
            let data = require_data(frame.data)?;
            let rows = routing_candle_rows(&data)?;
            let first = rows
                .first()
                .ok_or_else(|| "hyperliquid candle payload is empty".to_string())?;
            let coin = record_coin(first)?;
            let interval = first
                .get("i")
                .or_else(|| first.get("interval"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            // Every record in one frame belongs to the same market; a
            // mixed frame fails closed rather than misrouting candles.
            if rows
                .iter()
                .any(|row| record_coin(row).ok() != Some(coin.clone()))
            {
                return Err("hyperliquid candle frame mixes markets".to_string());
            }
            Ok(WsClientEvent::Candle {
                coin,
                interval: interval.to_string(),
                candles: data,
            })
        }
        "trades" => {
            // The documented payload is a bare `WsTrade[]` array; the coin
            // comes from the records themselves.
            let data = require_data(frame.data)?;
            let rows = routing_rows(&data)?;
            let first = rows
                .first()
                .ok_or_else(|| "hyperliquid trades payload is empty".to_string())?;
            let coin = first
                .get("coin")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "hyperliquid frame coin is missing".to_string())?;
            if rows
                .iter()
                .any(|row| row.get("coin").and_then(|value| value.as_str()) != Some(coin))
            {
                return Err("hyperliquid trades frame mixes markets".to_string());
            }
            Ok(WsClientEvent::Trades {
                coin: coin.to_string(),
                trades: data,
            })
        }
        "l2Book" => {
            let data = require_data(frame.data)?;
            let coin = routing_coin(&data)?;
            Ok(WsClientEvent::Book { coin, book: data })
        }
        "bbo" => {
            let data = require_data(frame.data)?;
            let coin = routing_coin(&data)?;
            Ok(WsClientEvent::Bbo { coin, bbo: data })
        }
        "activeAssetCtx" | "activeAssetContext" => {
            let data = require_data(frame.data)?;
            let coin = routing_coin(&data)?;
            Ok(WsClientEvent::Context {
                coin,
                context: data,
            })
        }
        "allMids" => Ok(WsClientEvent::AllMids {
            mids: require_data(frame.data)?,
        }),
        unknown => Err(format!("hyperliquid channel is unsupported: {unknown}")),
    }
}

fn require_data(
    data: Option<Box<serde_json::value::RawValue>>,
) -> Result<Box<serde_json::value::RawValue>, String> {
    data.ok_or_else(|| "hyperliquid frame data is missing".to_string())
}

/// Reads routing JSON for coin/interval extraction only. Strings survive
/// this parse exactly; decimals are never read here.
fn routing_value(data: &serde_json::value::RawValue) -> Result<serde_json::Value, String> {
    serde_json::from_str(data.get()).map_err(|_| "hyperliquid frame is malformed".to_string())
}

fn routing_rows(data: &serde_json::value::RawValue) -> Result<Vec<serde_json::Value>, String> {
    match routing_value(data)? {
        serde_json::Value::Array(rows) => Ok(rows),
        _ => Err("hyperliquid frame payload is not an array".to_string()),
    }
}

/// Rows a `candle` payload for routing: one `Candle[]` array, or the single
/// `Candle` object the live feed emits per update.
fn routing_candle_rows(
    data: &serde_json::value::RawValue,
) -> Result<Vec<serde_json::Value>, String> {
    match routing_value(data)? {
        serde_json::Value::Array(rows) => Ok(rows),
        object @ serde_json::Value::Object(_) => Ok(vec![object]),
        _ => Err("hyperliquid candle payload is malformed".to_string()),
    }
}

fn routing_coin(data: &serde_json::value::RawValue) -> Result<String, String> {
    routing_value(data)?
        .get("coin")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| "hyperliquid frame coin is missing".to_string())
}

fn record_coin(row: &serde_json::Value) -> Result<String, String> {
    row.get("s")
        .or_else(|| row.get("coin"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| "hyperliquid frame coin is missing".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_frames_carry_exact_wire_shapes() {
        let candle = build_candle_subscription("BTC", "1m").expect("candle");
        assert!(candle.contains("\"candle\"") && candle.contains("\"BTC\""));
        let trades = build_trades_subscription("@5").expect("trades");
        assert!(trades.contains("\"trades\""));
        let book = build_l2_subscription("BTC").expect("book");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&book).expect("subscription JSON"),
            serde_json::json!({"method": "subscribe", "subscription": {
                "type": "l2Book", "coin": "BTC"
            }})
        );
        let grouped = build_aggregated_l2_subscription("BTC", 4, None).expect("grouped book");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&grouped).expect("grouped subscription JSON"),
            serde_json::json!({"method": "subscribe", "subscription": {
                "type": "l2Book", "coin": "BTC", "nSigFigs": 4
            }})
        );
        let mantissa = build_aggregated_l2_subscription("BTC", 5, Some(5)).expect("mantissa book");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&mantissa).expect("mantissa JSON"),
            serde_json::json!({"method": "subscribe", "subscription": {
                "type": "l2Book", "coin": "BTC", "nSigFigs": 5, "mantissa": 5
            }})
        );
        assert!(build_aggregated_l2_subscription("BTC", 1, None).is_err());
        assert!(build_aggregated_l2_subscription("BTC", 4, Some(5)).is_err());
        assert!(build_candle_subscription("", "1m").is_err());
    }

    #[test]
    fn inbound_frames_route_without_confusing_control_and_data() {
        let pong = parse_ws_frame(r#"{"channel":"pong","data":{}}"#).expect("pong");
        assert!(matches!(pong, WsClientEvent::Pong));
        let trades = parse_ws_frame(
            r#"{"channel":"trades","data":[
                {"coin":"BTC","px":"1.0","sz":"1.0","side":"B",
                 "time":1,"tid":7,"hash":"0x0","users":["0x1","0x2"]}]}"#,
        );
        assert!(matches!(trades, Ok(WsClientEvent::Trades { .. })));
        // An empty trade array carries no coin identity and fails closed.
        assert!(parse_ws_frame(r#"{"channel":"trades","data":[]}"#).is_err());
        assert!(parse_ws_frame(r#"{"channel":"user","data":{}}"#).is_err());
        assert!(parse_ws_frame("not json").is_err());
    }

    #[test]
    fn live_candles_parse_the_documented_record_shape() {
        // Documented `Candle` records identify the market through `s`
        // (coin) and `i` (interval), not `coin`/`interval` fields.
        let event = parse_ws_frame(
            r#"{"channel":"candle","data":[
                {"t":1681923600000,"T":1681924499999,"s":"BTC","i":"15m",
                 "o":29295.0,"h":29309.0,"l":29250.0,"c":29258.0,
                 "v":0.98639,"n":189}]}"#,
        )
        .expect("candle frame");
        match event {
            WsClientEvent::Candle {
                coin,
                interval,
                candles,
            } => {
                assert_eq!(coin, "BTC");
                assert_eq!(interval, "15m");
                let payload: serde_json::Value =
                    serde_json::from_str(candles.get()).expect("candles parse");
                assert!(payload.is_array());
            }
            _ => panic!("candle frame misrouted"),
        }
        // A frame mixing markets fails closed instead of misrouting.
        assert!(
            parse_ws_frame(
                r#"{"channel":"candle","data":[
                {"t":1,"T":2,"s":"BTC","i":"1m","o":1.0,"h":1.0,
                 "l":1.0,"c":1.0,"v":1.0,"n":1},
                {"t":1,"T":2,"s":"ETH","i":"1m","o":1.0,"h":1.0,
                 "l":1.0,"c":1.0,"v":1.0,"n":1}]}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn trade_frames_flow_into_the_trade_decoder() {
        let event = parse_ws_frame(
            r#"{"channel":"trades","data":[
                {"coin":"BTC","px":"67000.5","sz":"0.1","side":"B",
                 "time":1700000000000,"tid":118906512037719,
                 "hash":"0xaaa","users":["0x1","0x2"]}]}"#,
        )
        .expect("trade frame");
        match event {
            WsClientEvent::Trades { coin, trades } => {
                assert_eq!(coin, "BTC");
                let batch = crate::trades::decode_trades_batch(
                    &trades,
                    &coin,
                    "hyperliquid:perp:BTC",
                    "hyperliquid:public",
                    3,
                    1_700_000_000_100_000_000,
                    1,
                )
                .expect("batch decodes");
                assert_eq!(batch.trades.len(), 1);
                assert_eq!(
                    batch.trades[0].trade_id,
                    "hl:BTC:1700000000000:118906512037719"
                );
            }
            _ => panic!("trade frame misrouted"),
        }
    }

    #[test]
    fn candle_frames_flow_into_the_candle_decoder() {
        let event = parse_ws_frame(
            r#"{"channel":"candle","data":[
                {"t":1681923600000,"T":1681924499999,"s":"BTC","i":"15m",
                 "o":29295.0,"h":29309.0,"l":29250.0,"c":29258.0,
                 "v":0.98639,"n":189}]}"#,
        )
        .expect("candle frame");
        match event {
            WsClientEvent::Candle {
                coin,
                interval,
                candles,
            } => {
                assert_eq!(coin, "BTC");
                assert_eq!(interval, "15m");
                let period = asceify_market_data::BarPeriod::time(900).expect("period");
                let page = crate::candles::decode_candle_page(
                    &candles,
                    period,
                    crate::decimal::NORMALIZED_PRICE_SCALE,
                    crate::decimal::NORMALIZED_QUANTITY_SCALE,
                    1_681_924_600_000,
                )
                .expect("page decodes");
                assert_eq!(page.bars.len(), 1);
                assert_eq!(page.bars[0].close, 2_925_800_000_000);
            }
            _ => panic!("candle frame misrouted"),
        }
    }

    #[test]
    fn live_single_object_candles_parse_and_decode() {
        // Observed live feed shape (wss://api.hyperliquid.xyz/ws, `candle`
        // channel): one `Candle` object per frame, JSON numbers, not the
        // documented `Candle[]` snapshot array.
        let event = parse_ws_frame(
            r#"{"channel":"candle","data":{"t":1681923600000,"T":1681924499999,"s":"BTC","i":"15m","o":29295.0,"h":29309.0,"l":29250.0,"c":29258.0,"v":0.98639,"n":189}}"#,
        )
        .expect("single-object candle frame");
        match event {
            WsClientEvent::Candle {
                coin,
                interval,
                candles,
            } => {
                assert_eq!(coin, "BTC");
                assert_eq!(interval, "15m");
                let period = asceify_market_data::BarPeriod::time(900).expect("period");
                let page = crate::candles::decode_candle_page(
                    &candles,
                    period,
                    crate::decimal::NORMALIZED_PRICE_SCALE,
                    crate::decimal::NORMALIZED_QUANTITY_SCALE,
                    1_681_924_600_000,
                )
                .expect("single candle decodes");
                assert_eq!(page.bars.len() + usize::from(page.forming.is_some()), 1);
                let bar = page.forming.or(page.bars.into_iter().next()).expect("bar");
                assert_eq!(bar.close, 2_925_800_000_000);
            }
            _ => panic!("candle frame misrouted"),
        }
    }

    #[test]
    fn heartbeat_frame_has_the_documented_shape() {
        assert_eq!(build_ping(), r#"{"method":"ping"}"#);
    }
}
