//! Binance combined-stream WebSocket framing and subscription builders.

use aeris_crypto_common::{CryptoError, CryptoResult};
use serde::Deserialize;
use serde_json::json;

/// Parsed Binance combined-stream WebSocket frame.
#[derive(Debug)]
pub enum BinanceWsEvent {
    /// Subscription acknowledgement (`{"result":null,"id":...}`).
    Ack,
    /// Aggregate trade (`<sym>@aggTrade`).
    AggTrade(Box<serde_json::value::RawValue>),
    /// Diff depth update (`<sym>@depth@100ms`).
    DepthUpdate(Box<serde_json::value::RawValue>),
    /// Best bid/ask ticker (`<sym>@bookTicker`).
    BookTicker(Box<serde_json::value::RawValue>),
    /// Live candlestick (`<sym>@kline_<iv>`).
    Kline(Box<serde_json::value::RawValue>),
    /// Ignored or unrecognized non-error message.
    Ignored,
}

#[derive(Deserialize)]
struct CombinedEnvelope {
    #[serde(default)]
    stream: Option<String>,
    #[serde(default)]
    data: Option<Box<serde_json::value::RawValue>>,
    #[serde(default)]
    id: Option<u64>,
}

/// Builds a `SUBSCRIBE` JSON frame for a batch of stream names.
#[must_use]
pub fn build_subscribe(streams: &[String], id: u64) -> String {
    json!({
        "method": "SUBSCRIBE",
        "params": streams,
        "id": id,
    })
    .to_string()
}

/// Builds an `UNSUBSCRIBE` JSON frame for a batch of stream names.
#[must_use]
pub fn build_unsubscribe(streams: &[String], id: u64) -> String {
    json!({
        "method": "UNSUBSCRIBE",
        "params": streams,
        "id": id,
    })
    .to_string()
}

/// Parses one Binance combined-stream WebSocket text frame.
///
/// # Errors
/// Returns an error if the JSON envelope is malformed.
pub fn parse_ws_frame(text: &str) -> CryptoResult<BinanceWsEvent> {
    let envelope: CombinedEnvelope = serde_json::from_str(text)
        .map_err(|_| CryptoError::MalformedPayload("binance ws frame is malformed".to_string()))?;
    if let (Some(stream), Some(data)) = (envelope.stream, envelope.data) {
        if stream.ends_with("@aggTrade") {
            return Ok(BinanceWsEvent::AggTrade(data));
        }
        if stream.contains("@depth") {
            return Ok(BinanceWsEvent::DepthUpdate(data));
        }
        if stream.ends_with("@bookTicker") {
            return Ok(BinanceWsEvent::BookTicker(data));
        }
        if stream.contains("@kline_") {
            return Ok(BinanceWsEvent::Kline(data));
        }
        return Ok(BinanceWsEvent::Ignored);
    }
    if envelope.id.is_some() {
        return Ok(BinanceWsEvent::Ack);
    }
    Ok(BinanceWsEvent::Ignored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BINANCE_FUTURES_MARKET_WS_URL, BINANCE_FUTURES_PUBLIC_WS_URL, decode_live_candle};

    #[test]
    fn futures_ws_endpoints_use_routed_market_and_public_paths() {
        assert!(BINANCE_FUTURES_MARKET_WS_URL.ends_with("/market/stream"));
        assert!(BINANCE_FUTURES_PUBLIC_WS_URL.ends_with("/public/stream"));
    }

    #[test]
    fn parses_combined_stream_kline_and_agg_trade_frames() {
        let kline_frame = r#"{"stream":"btcusdt@kline_1m","data":{"e":"kline","E":1795365615768,"s":"BTCUSDT","k":{"t":1795365600000,"T":1795365659999,"s":"BTCUSDT","i":"1m","o":"61240.0","h":"61275.0","l":"61220.0","c":"61260.5","v":"12.345","x":false}}}"#;
        let parsed = parse_ws_frame(kline_frame).unwrap();
        let BinanceWsEvent::Kline(raw) = parsed else {
            panic!("expected kline event");
        };
        let (sym, interval, live) = decode_live_candle(&raw).unwrap();
        assert_eq!(sym, "BTCUSDT");
        assert_eq!(interval, "1m");
        assert_eq!(live.bar.close, 6_126_050_000_000);
        assert!(!live.is_closed);

        let trade_frame = r#"{"stream":"btcusdt@aggTrade","data":{"e":"aggTrade","E":1795365615895,"a":3542566480,"s":"BTCUSDT","p":"61249.30","q":"0.014","T":1795365615890,"m":false}}"#;
        assert!(matches!(
            parse_ws_frame(trade_frame).unwrap(),
            BinanceWsEvent::AggTrade(_)
        ));
    }
}
