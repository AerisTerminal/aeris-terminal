//! Bounded `DXLink` verification using negotiated fields, never guessed offsets.
//! <https://developer.tastytrade.com/docs/guides/stream-market-data/>
//! No raw payloads or credentials are retained or published to presentation.

use crate::QuoteToken;
use aeris_platform_runtime::{MarketSocket, MarketSocketEvent};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

const CHANNEL: u64 = 1;
const LIMIT: Duration = Duration::from_secs(45);
const OBSERVATION: Duration = Duration::from_secs(30);
const QUOTE_FIELDS: &[&str] = &[
    "eventType",
    "eventSymbol",
    "bidPrice",
    "askPrice",
    "bidSize",
    "askSize",
    "bidTime",
    "askTime",
];
const CANDLE_FIELDS: &[&str] = &[
    "eventType",
    "eventSymbol",
    "eventFlags",
    "index",
    "time",
    "sequence",
    "open",
    "high",
    "low",
    "close",
    "volume",
    "count",
];
const TAPE_FIELDS: &[&str] = &[
    "eventType",
    "eventSymbol",
    "eventFlags",
    "index",
    "time",
    "timeNanoPart",
    "sequence",
    "price",
    "size",
    "aggressorSide",
    "spreadLeg",
    "validTick",
    "type",
];

/// Public-feed observations only. This is not canonical chart state or proof of
/// lossless tape delivery, market depth, or measured production scalability.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FeedVerification {
    pub usable_quotes: u64,
    pub usable_candles: u64,
    pub new_trades: u64,
    pub buy_trades: u64,
    pub sell_trades: u64,
    pub unknown_side_trades: u64,
    pub time_and_sale_fields: bool,
    pub newest_quote_time_ms: Option<u64>,
    pub newest_trade_time_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Setup,
    Unauthorized,
    Authorized,
    Channel,
    Config,
    Streaming,
}
#[derive(Debug, Eq, PartialEq)]
enum Action {
    None,
    Authenticate,
    RequestChannel,
    Configure,
    Subscribe,
}

struct Verification {
    step: Step,
    symbol: String,
    candle_symbol: String,
    fields: BTreeMap<String, Vec<String>>,
    report: FeedVerification,
    keepalive: Duration,
}

impl Verification {
    fn new(symbol: &str) -> Result<Self, String> {
        if symbol.is_empty()
            || symbol.len() > 128
            || symbol
                .bytes()
                .any(|byte| byte.is_ascii_control() || matches!(byte, b' ' | b'{' | b'}'))
        {
            return Err("DXLink streamer symbol is invalid".to_string());
        }
        Ok(Self {
            step: Step::Setup,
            symbol: symbol.to_string(),
            candle_symbol: format!("{symbol}{{=1m}}"),
            fields: BTreeMap::new(),
            report: FeedVerification::default(),
            keepalive: Duration::from_secs(30),
        })
    }

    fn consume(&mut self, frame: &Value) -> Result<Action, String> {
        let kind = frame
            .get("type")
            .and_then(Value::as_str)
            .ok_or("DXLink frame type is missing")?;
        let channel = frame
            .get("channel")
            .and_then(Value::as_u64)
            .ok_or("DXLink frame channel is missing")?;
        if kind == "ERROR" || kind == "CHANNEL_CLOSED" {
            return Err("DXLink rejected or closed the requested feed".to_string());
        }
        if kind == "KEEPALIVE" && channel == 0 {
            return Ok(Action::None);
        }
        match (kind, channel, self.step) {
            ("SETUP", 0, Step::Setup) => {
                let version = frame
                    .get("version")
                    .and_then(Value::as_str)
                    .ok_or("DXLink version is missing")?;
                let timeout = frame
                    .get("keepaliveTimeout")
                    .and_then(Value::as_f64)
                    .ok_or("DXLink keepalive timeout is missing")?;
                // The client advertises 0.1-DXF-JS; the server acknowledges its
                // own 1.0 implementation version, not the client's identifier.
                // https://developer.tastytrade.com/asyncapi/tastytrade-streaming.asyncapi.json
                if !(version == "1.0" || version.starts_with("1.0-"))
                    || !(5.0..=120.0).contains(&timeout)
                {
                    return Err("DXLink setup negotiation is unsupported".to_string());
                }
                self.keepalive = Duration::try_from_secs_f64(timeout / 2.0)
                    .map_err(|_| "DXLink keepalive timeout is invalid")?
                    .min(Duration::from_secs(30));
                self.step = Step::Unauthorized;
                Ok(Action::None)
            }
            ("AUTH_STATE", 0, Step::Unauthorized)
                if frame.get("state").and_then(Value::as_str) == Some("UNAUTHORIZED") =>
            {
                self.step = Step::Authorized;
                Ok(Action::Authenticate)
            }
            ("AUTH_STATE", 0, Step::Authorized)
                if frame.get("state").and_then(Value::as_str) == Some("AUTHORIZED") =>
            {
                self.step = Step::Channel;
                Ok(Action::RequestChannel)
            }
            ("CHANNEL_OPENED", CHANNEL, Step::Channel)
                if frame.get("service").and_then(Value::as_str) == Some("FEED") =>
            {
                self.step = Step::Config;
                Ok(Action::Configure)
            }
            ("FEED_CONFIG", CHANNEL, Step::Config) => {
                // A channel can first announce its default FULL configuration.
                // Fields arrive in incremental updates once subscriptions exist.
                if frame.get("dataFormat").and_then(Value::as_str) == Some("FULL") {
                    return Ok(Action::None);
                }
                self.configure(frame)?;
                self.step = Step::Streaming;
                Ok(Action::Subscribe)
            }
            ("FEED_CONFIG", CHANNEL, Step::Streaming) => {
                self.configure(frame)?;
                Ok(Action::None)
            }
            ("FEED_DATA", CHANNEL, Step::Streaming) => {
                self.observe(frame)?;
                Ok(Action::None)
            }
            _ => Err("DXLink sent an unexpected protocol transition".to_string()),
        }
    }

    fn configure(&mut self, frame: &Value) -> Result<(), String> {
        match frame.get("dataFormat") {
            Some(Value::String(format)) if format == "COMPACT" => {}
            None if self.step == Step::Streaming => {} // Preserve the confirmed channel format.
            _ => return Err("DXLink did not confirm compact data".to_string()),
        }
        // dxFeed's reference client merges optional, per-type schema updates.
        // https://github.com/dxFeed/dxLink/blob/main/dxlink-javascript/dxlink-feed/src/feed.ts
        let Some(configured) = frame.get("eventFields") else {
            return Ok(());
        };
        let configured = configured
            .as_object()
            .ok_or("DXLink field negotiation is malformed")?;
        for (kind, required) in [
            ("Quote", QUOTE_FIELDS),
            ("Candle", CANDLE_FIELDS),
            ("TimeAndSale", TAPE_FIELDS),
        ] {
            let Some(values) = configured.get(kind) else {
                continue;
            };
            let values = values
                .as_array()
                .ok_or("DXLink negotiated fields are malformed")?;
            if values.is_empty() || values.len() > 32 {
                return Err("DXLink negotiated field count is invalid".to_string());
            }
            let fields: Vec<String> = values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .filter(|name| !name.is_empty() && name.len() <= 64)
                        .map(str::to_string)
                        .ok_or_else(|| "DXLink negotiated field name is invalid".to_string())
                })
                .collect::<Result<_, _>>()?;
            if required
                .iter()
                .any(|required| !fields.iter().any(|field| field == required))
                || fields
                    .iter()
                    .enumerate()
                    .any(|(index, field)| fields[..index].contains(field))
            {
                return Err("DXLink omitted or duplicated required event fields".to_string());
            }
            self.fields.insert(kind.to_string(), fields);
        }
        self.report.time_and_sale_fields = self.fields.contains_key("TimeAndSale");
        Ok(())
    }

    fn observe(&mut self, frame: &Value) -> Result<(), String> {
        let data = frame
            .get("data")
            .and_then(Value::as_array)
            .ok_or("DXLink compact data is missing")?;
        if data.len() > 128 || !data.len().is_multiple_of(2) {
            return Err("DXLink compact envelope is invalid".to_string());
        }
        for pair in data.as_chunks::<2>().0 {
            let kind = pair[0].as_str().ok_or("DXLink compact type is invalid")?;
            let fields = self
                .fields
                .get(kind)
                .ok_or("DXLink sent an unnegotiated event type")?;
            let values = pair[1].as_array().ok_or("DXLink compact row is invalid")?;
            if values.len() / fields.len() > 32_768 || !values.len().is_multiple_of(fields.len()) {
                return Err("DXLink compact row width or count is invalid".to_string());
            }
            for row in values.chunks_exact(fields.len()) {
                let field = |name: &str| {
                    fields
                        .iter()
                        .position(|field| field == name)
                        .and_then(|index| row.get(index))
                };
                if field("eventType").and_then(Value::as_str) != Some(kind) {
                    return Err("DXLink event type is inconsistent".to_string());
                }
                let symbol = field("eventSymbol")
                    .and_then(Value::as_str)
                    .ok_or("DXLink event symbol is missing")?;
                let expected = if kind == "Candle" {
                    &self.candle_symbol
                } else {
                    &self.symbol
                };
                if symbol != expected
                    && !(kind == "Candle" && symbol == format!("{}{{=m}}", self.symbol))
                {
                    return Err("DXLink returned another subscription's symbol".to_string());
                }
                match kind {
                    "Quote"
                        if available_values(
                            &field,
                            &["bidPrice", "askPrice", "bidSize", "askSize"],
                        )? =>
                    {
                        let bid_time = field("bidTime")
                            .and_then(Value::as_u64)
                            .ok_or("DXLink bid timestamp is invalid")?;
                        let ask_time = field("askTime")
                            .and_then(Value::as_u64)
                            .ok_or("DXLink ask timestamp is invalid")?;
                        let latest = bid_time.max(ask_time);
                        if latest > 0 {
                            self.report.newest_quote_time_ms = Some(
                                self.report
                                    .newest_quote_time_ms
                                    .map_or(latest, |prior| prior.max(latest)),
                            );
                        }
                        self.report.usable_quotes = self.report.usable_quotes.saturating_add(1);
                    }
                    "Candle"
                        if available_values(
                            &field,
                            &["open", "high", "low", "close", "volume"],
                        )? =>
                    {
                        self.report.usable_candles = self.report.usable_candles.saturating_add(1);
                    }
                    "TimeAndSale" => observe_trade(&mut self.report, &field)?,
                    "Quote" | "Candle" => {} // Explicitly absent/NaN market values are not counted as usable data.
                    _ => return Err("DXLink event type is unsupported".to_string()),
                }
            }
        }
        Ok(())
    }
}

fn available_values<'a>(
    field: &impl Fn(&str) -> Option<&'a Value>,
    names: &[&str],
) -> Result<bool, String> {
    let mut available = true;
    for name in names {
        match field(name) {
            Some(Value::Number(_)) => {}
            Some(Value::Null) => available = false,
            Some(Value::String(value)) if value == "NaN" => available = false,
            _ => return Err("DXLink market value has an invalid wire type".to_string()),
        }
    }
    Ok(available)
}

fn observe_trade<'a>(
    report: &mut FeedVerification,
    field: &impl Fn(&str) -> Option<&'a Value>,
) -> Result<(), String> {
    let flags = field("eventFlags")
        .and_then(Value::as_u64)
        .ok_or("DXLink trade flags are invalid")?;
    let valid = field("validTick")
        .and_then(Value::as_bool)
        .ok_or("DXLink valid-tick flag is missing")?;
    let kind = field("type")
        .and_then(Value::as_str)
        .ok_or("DXLink trade type is missing")?;
    if !matches!(kind, "NEW" | "CORRECTION" | "CANCEL") {
        return Err("DXLink trade type is unsupported".to_string());
    }
    if flags != 0 || !valid || kind != "NEW" {
        return Ok(());
    }
    if !available_values(field, &["price", "size"])? {
        return Ok(());
    }
    let time = field("time")
        .and_then(Value::as_u64)
        .ok_or("DXLink trade timestamp is invalid")?;
    if time > 0 {
        report.newest_trade_time_ms = Some(
            report
                .newest_trade_time_ms
                .map_or(time, |prior| prior.max(time)),
        );
    }
    match field("aggressorSide").and_then(Value::as_str) {
        Some("BUY") => report.buy_trades = report.buy_trades.saturating_add(1),
        Some("SELL") => report.sell_trades = report.sell_trades.saturating_add(1),
        Some("UNDEFINED") => {
            report.unknown_side_trades = report.unknown_side_trades.saturating_add(1);
        }
        _ => return Err("DXLink aggressor side is missing or unsupported".to_string()),
    }
    report.new_trades = report.new_trades.saturating_add(1);
    Ok(())
}

/// Opens one runtime-owned diagnostic connection and observes its public feed.
/// No events are installed into canonical state before full recovery/precision
/// handling is implemented. The connection is always closed after the bounded check.
///
/// # Errors
/// Rejects failed authorization, unsupported protocol, malformed fields, or cancellation.
pub fn verify_live_feed(
    token: &QuoteToken,
    symbol: &str,
    stop: &Arc<AtomicBool>,
) -> Result<FeedVerification, String> {
    let mut verification = Verification::new(symbol)?;
    let (mut socket, _) = MarketSocket::connect(&token.dxlink_url, Duration::from_secs(10), stop)
        .map_err(|error| error.to_string())?;
    let result = observe_socket(&mut socket, &mut verification, token, stop);
    socket.close();
    result
}

fn observe_socket(
    socket: &mut MarketSocket,
    state: &mut Verification,
    token: &QuoteToken,
    stop: &AtomicBool,
) -> Result<FeedVerification, String> {
    send(
        socket,
        &json!({"type":"SETUP","channel":0,"version":"0.1-DXF-JS/0.3.0","keepaliveTimeout":60,"acceptKeepaliveTimeout":60}),
    )?;
    let deadline = Instant::now() + LIMIT;
    let mut finish = deadline;
    let mut keepalive = Instant::now() + state.keepalive;
    while Instant::now() < finish {
        if stop.load(Ordering::Acquire) {
            return Err("DXLink verification cancelled".to_string());
        }
        if Instant::now() >= keepalive {
            send(socket, &json!({"type":"KEEPALIVE","channel":0}))?;
            keepalive = Instant::now() + state.keepalive;
        }
        let text = match socket.read_event(Instant::now() + Duration::from_millis(250)) {
            Ok(MarketSocketEvent::Text(text)) => text,
            Ok(MarketSocketEvent::Pong) => continue,
            Err(error) if error.is_read_timeout() => continue,
            Err(error) => return Err(error.to_string()),
        };
        let frame: Value = serde_json::from_str(&text).map_err(|_| "DXLink sent malformed JSON")?;
        let action = state.consume(&frame)?;
        if frame.get("type").and_then(Value::as_str) == Some("SETUP") {
            keepalive = Instant::now() + state.keepalive;
        }
        match action {
            Action::None => {}
            Action::Authenticate => authenticate(socket, &token.token)?,
            Action::RequestChannel => send(
                socket,
                &json!({"type":"CHANNEL_REQUEST","channel":CHANNEL,"service":"FEED","parameters":{"contract":"AUTO"}}),
            )?,
            Action::Configure => send(
                socket,
                &json!({"type":"FEED_SETUP","channel":CHANNEL,"acceptAggregationPeriod":0,"acceptDataFormat":"COMPACT","acceptEventFields":{"Quote":QUOTE_FIELDS,"Candle":CANDLE_FIELDS,"TimeAndSale":TAPE_FIELDS}}),
            )?,
            Action::Subscribe => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| "System clock is invalid")?;
                let from = u64::try_from(now.as_millis())
                    .map_err(|_| "System clock overflowed")?
                    .saturating_sub(6 * 60 * 60 * 1000);
                let mut add = vec![
                    json!({"type":"Quote","symbol":state.symbol}),
                    json!({"type":"Candle","symbol":state.candle_symbol,"fromTime":from}),
                ];
                add.push(json!({"type":"TimeAndSale","symbol":state.symbol}));
                send(
                    socket,
                    &json!({"type":"FEED_SUBSCRIPTION","channel":CHANNEL,"reset":true,"add":add}),
                )?;
                finish = deadline.min(Instant::now() + OBSERVATION);
            }
        }
    }
    if state.step != Step::Streaming {
        return Err("DXLink handshake did not complete before its deadline".to_string());
    }
    if !state.fields.contains_key("Quote") || !state.fields.contains_key("Candle") {
        return Err(
            "DXLink did not announce quote and candle schemas before the deadline".to_string(),
        );
    }
    Ok(state.report.clone())
}

fn authenticate(socket: &mut MarketSocket, token: &str) -> Result<(), String> {
    #[derive(Serialize)]
    struct Auth<'a> {
        r#type: &'static str,
        channel: u64,
        token: &'a str,
    }
    let frame = Zeroizing::new(
        serde_json::to_string(&Auth {
            r#type: "AUTH",
            channel: 0,
            token,
        })
        .map_err(|_| "DXLink authorization could not be encoded")?,
    );
    socket.send_text(&frame).map_err(|error| error.to_string())
}

fn send(socket: &mut MarketSocket, frame: &Value) -> Result<(), String> {
    socket
        .send_text(&frame.to_string())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> Verification {
        let mut state = Verification::new("/ESU23:XCME").expect("symbol");
        assert_eq!(
            state
                .consume(&json!({"type":"SETUP","channel":0,"version":"1.0-1.2.1-20240722-153442","keepaliveTimeout":60,"acceptKeepaliveTimeout":60}))
                .expect("setup"),
            Action::None
        );
        assert_eq!(
            state
                .consume(&json!({"type":"AUTH_STATE","channel":0,"state":"UNAUTHORIZED"}))
                .expect("auth"),
            Action::Authenticate
        );
        assert_eq!(
            state
                .consume(&json!({"type":"AUTH_STATE","channel":0,"state":"AUTHORIZED"}))
                .expect("ready"),
            Action::RequestChannel
        );
        assert_eq!(
            state
                .consume(&json!({"type":"CHANNEL_OPENED","channel":1,"service":"FEED"}))
                .expect("channel"),
            Action::Configure
        );
        assert_eq!(state.consume(&json!({"type":"FEED_CONFIG","channel":1,"dataFormat":"COMPACT","eventFields":{"Quote":QUOTE_FIELDS,"Candle":CANDLE_FIELDS,"TimeAndSale":TAPE_FIELDS}})).expect("fields"), Action::Subscribe);
        state
    }

    #[test]
    fn negotiated_compact_batches_require_exact_width_and_subscription_identity() {
        let mut state = configured();
        let quote = json!({"type":"FEED_DATA","channel":1,"data":["Quote",["Quote","/ESU23:XCME",5000.0,5000.25,2,3,1_690_000_000_000_u64,1_690_000_000_000_u64]]});
        state.consume(&quote).expect("quote");
        assert_eq!(state.report.usable_quotes, 1);
        let mut malformed = quote.clone();
        malformed["data"][1].as_array_mut().expect("row").pop();
        assert!(state.consume(&malformed).is_err());
        let mut other = quote;
        other["data"][1][1] = json!("/NQU23:XCME");
        assert!(state.consume(&other).is_err());
    }

    #[test]
    fn negotiated_field_order_and_unavailable_values_remain_explicit() {
        let mut state = configured();
        state.fields.get_mut("Quote").expect("fields").reverse();
        let mut quote = json!({"type":"FEED_DATA","channel":1,"data":["Quote",[1_690_000_000_000_u64,1_690_000_000_000_u64,3,2,5000.25,5000,"/ESU23:XCME","Quote"]]});
        state.consume(&quote).expect("reordered quote");
        assert_eq!(state.report.usable_quotes, 1);
        quote["data"][1][5] = json!("NaN");
        state.consume(&quote).expect("unavailable bid");
        assert_eq!(state.report.usable_quotes, 1);
        quote["data"][1][5] = json!({"unexpected":5000});
        assert!(state.consume(&quote).is_err());
    }

    #[test]
    fn optional_configuration_and_incremental_schemas_precede_data_decoding() {
        let mut state = configured();
        state.fields.clear();
        state.report = FeedVerification::default();
        state.step = Step::Config;
        assert_eq!(state.consume(&json!({"type":"FEED_CONFIG","channel":1,"dataFormat":"FULL","aggregationPeriod":0.1})).expect("initial defaults"), Action::None);
        assert_eq!(state.consume(&json!({"type":"FEED_CONFIG","channel":1,"dataFormat":"COMPACT","aggregationPeriod":0})).expect("format confirmed"), Action::Subscribe);
        assert!(state.fields.is_empty());
        for (kind, fields) in [
            ("Quote", QUOTE_FIELDS),
            ("Candle", CANDLE_FIELDS),
            ("TimeAndSale", TAPE_FIELDS),
        ] {
            assert_eq!(
                state
                    .consume(&json!({"type":"FEED_CONFIG","channel":1,"eventFields":{kind:fields}}))
                    .expect("incremental fields"),
                Action::None
            );
        }
        assert_eq!(state.fields.len(), 3);
        assert!(state.report.time_and_sale_fields);
        state.consume(&json!({"type":"FEED_DATA","channel":1,"data":["Quote",["Quote","/ESU23:XCME",5000,5000.25,2,3,1_690_000_000_000_u64,1_690_000_000_000_u64]]})).expect("negotiated quote");
        assert_eq!(state.report.usable_quotes, 1);
    }

    #[test]
    fn snapshots_corrections_and_unknown_sides_are_not_invented_aggressors() {
        let mut state = configured();
        for (flags, kind, side) in [
            (0, "NEW", "BUY"),
            (0, "NEW", "UNDEFINED"),
            (64, "NEW", "SELL"),
            (0, "CORRECTION", "SELL"),
        ] {
            state.consume(&json!({"type":"FEED_DATA","channel":1,"data":["TimeAndSale",["TimeAndSale","/ESU23:XCME",flags,"opaque-index",1_690_000_000_000_u64,0,1,5000,2,side,false,true,kind]]})).expect("trade");
        }
        assert_eq!(state.report.new_trades, 2);
        assert_eq!(state.report.buy_trades, 1);
        assert_eq!(state.report.sell_trades, 0);
        assert_eq!(state.report.unknown_side_trades, 1);
        assert_eq!(state.report.newest_trade_time_ms, Some(1_690_000_000_000));
    }

    #[test]
    fn credentials_cannot_be_sent_before_setup_and_field_negotiation_is_required() {
        let mut state = Verification::new("/ESU23:XCME").expect("symbol");
        assert!(
            state
                .consume(&json!({"type":"AUTH_STATE","channel":0,"state":"UNAUTHORIZED"}))
                .is_err()
        );
        state.step = Step::Config;
        assert!(state.consume(&json!({"type":"FEED_CONFIG","channel":1,"dataFormat":"COMPACT","eventFields":{"Quote":["eventSymbol"]}})).is_err());
    }

    #[test]
    fn unsupported_server_versions_and_invalid_keepalive_are_rejected() {
        for (version, timeout) in [
            ("0.1-DXF-JS/0.3.0", 60),
            ("1.01", 60),
            ("2.0", 60),
            ("1.0", 0),
        ] {
            let mut state = Verification::new("/ESU23:XCME").expect("symbol");
            assert!(state.consume(&json!({"type":"SETUP","channel":0,"version":version,"keepaliveTimeout":timeout})).is_err());
        }
    }
}
