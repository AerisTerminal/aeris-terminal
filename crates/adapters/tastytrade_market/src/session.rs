//! One multiplexed `DXLink` transport. Provider workers own this object.
//! Schemas are negotiated per channel; decimal tokens stay raw until fixed-point conversion.

use crate::QuoteToken;
use aeris_market_data::{AggressorSide, MarketBar, parse_decimal_to_fixed};
use aeris_observability::diagnostic;
use aeris_platform_runtime::{MarketSocket, MarketSocketEvent};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json, value::RawValue};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

/// Explicit normalized storage scale. Values finer than this fail validation;
/// no provider price or volume is rounded to fit the canonical representation.
pub const DATA_SCALE: u32 = 8;
const MAXIMUM_CHANNELS: usize = 100;
// Leave headroom below DXLink's 10,000 subscription changes per minute.
const MAXIMUM_SUBSCRIPTION_CHANGES_PER_MINUTE: usize = 9_000;
const QUOTE: &[&str] = &[
    "eventType",
    "eventSymbol",
    "bidPrice",
    "askPrice",
    "bidSize",
    "askSize",
    "bidTime",
    "askTime",
];
const CANDLE: &[&str] = &[
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
const TAPE: &[&str] = &[
    "eventType",
    "eventSymbol",
    "eventFlags",
    "index",
    "time",
    "timeNanoPart",
    "sequence",
    "price",
    "size",
    "bidPrice",
    "askPrice",
    "aggressorSide",
    "spreadLeg",
    "validTick",
    "type",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Subscription {
    pub kind: &'static str,
    pub symbol: String,
    pub from_time_ms: Option<i64>,
}

/// A subscription reconciliation failure, separate from socket failures.
#[derive(Debug, Eq, PartialEq)]
pub enum SubscriptionChangeError {
    BudgetExhausted,
    CapacityExhausted,
    Other(String),
}

impl std::fmt::Display for SubscriptionChangeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BudgetExhausted => {
                formatter.write_str("DXLink subscription change budget exhausted")
            }
            Self::CapacityExhausted => {
                formatter.write_str("DXLink subscription capacity exhausted")
            }
            Self::Other(detail) => formatter.write_str(detail),
        }
    }
}

#[derive(Debug)]
pub enum FeedEvent {
    ChannelFailure {
        channel: u64,
    },
    Quote {
        channel: u64,
        symbol: String,
        bid: Option<(i64, i64)>,
        ask: Option<(i64, i64)>,
        time_nanos: Option<i64>,
    },
    Candle {
        channel: u64,
        symbol: String,
        flags: u32,
        index: String,
        bar: Option<MarketBar>,
        count: u64,
    },
    Trade {
        channel: u64,
        symbol: String,
        flags: u32,
        index: String,
        kind: String,
        trade: Option<TradePrint>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradePrint {
    pub time_nanos: i64,
    pub price: i64,
    pub quantity: i64,
    pub bid_price: Option<i64>,
    pub ask_price: Option<i64>,
    pub aggressor: AggressorSide,
    pub spread_leg: bool,
}

#[derive(Deserialize)]
struct Header {
    r#type: String,
    channel: u64,
}
#[derive(Deserialize)]
struct Data {
    data: Vec<Box<RawValue>>,
}

struct Channel {
    compact: bool,
    subscriptions: Vec<Subscription>,
    fields: BTreeMap<String, Vec<String>>,
}

#[derive(Default)]
pub struct SubscriptionChangeBudget {
    changes: VecDeque<(Instant, usize)>,
    used: usize,
}

impl SubscriptionChangeBudget {
    fn has_capacity(&mut self, now: Instant, changes: usize) -> bool {
        while self
            .changes
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) >= Duration::from_secs(60))
        {
            if let Some((_, expired)) = self.changes.pop_front() {
                self.used -= expired;
            }
        }
        changes <= MAXIMUM_SUBSCRIPTION_CHANGES_PER_MINUTE.saturating_sub(self.used)
    }

    fn reserve(&mut self, now: Instant, changes: usize) -> bool {
        if !self.has_capacity(now, changes) {
            return false;
        }
        if changes > 0 {
            self.changes.push_back((now, changes));
            self.used += changes;
        }
        true
    }
}

const TRANSPORT_PING_INTERVAL: Duration = Duration::from_secs(10);

/// WebSocket ping/pong round-trip measurement. `DXLink` KEEPALIVE frames are
/// sent independently by each side, so they cannot time a round trip.
#[derive(Default)]
struct TransportPing {
    last_sent: Option<Instant>,
    awaiting_pong: bool,
    measured: Option<u64>,
}

impl TransportPing {
    /// The first ping goes out immediately; later pings follow every interval
    /// whether or not the previous one was answered, so a dropped pong cannot
    /// stop measurement.
    fn due(&self, now: Instant) -> bool {
        self.last_sent
            .is_none_or(|sent| now.saturating_duration_since(sent) >= TRANSPORT_PING_INTERVAL)
    }

    fn sent(&mut self, now: Instant) {
        self.last_sent = Some(now);
        self.awaiting_pong = true;
    }

    fn answered(&mut self, received_at: Instant) {
        let Some(sent) = self.last_sent.filter(|_| self.awaiting_pong) else {
            return;
        };
        self.awaiting_pong = false;
        let nanos = received_at.saturating_duration_since(sent).as_nanos();
        self.measured = Some(u64::try_from(nanos).unwrap_or(u64::MAX).max(1));
    }
}

pub struct DxlinkSession {
    socket: MarketSocket,
    channels: BTreeMap<u64, Channel>,
    pending: VecDeque<FeedEvent>,
    keepalive_interval: Duration,
    keepalive_at: Instant,
    last_received: Instant,
    transport_ping: TransportPing,
    stop: Arc<AtomicBool>,
    authorization_deadline: Option<Instant>,
    decode_failures_in_window: u8,
    decode_window: Instant,
    subscription_budget: Arc<Mutex<SubscriptionChangeBudget>>,
}

impl DxlinkSession {
    /// Authenticates once. Channel changes never recreate the transport.
    /// # Errors
    /// Rejects failed authorization, unsupported setup, cancellation or timeout.
    pub fn connect(
        token: &QuoteToken,
        stop: &Arc<AtomicBool>,
        subscription_budget: Arc<Mutex<SubscriptionChangeBudget>>,
    ) -> Result<Self, String> {
        let (socket, _) = MarketSocket::connect(&token.dxlink_url, Duration::from_secs(10), stop)
            .map_err(|e| e.to_string())?;
        let mut session = Self {
            socket,
            channels: BTreeMap::new(),
            pending: VecDeque::new(),
            keepalive_interval: Duration::from_secs(30),
            keepalive_at: Instant::now() + Duration::from_secs(30),
            last_received: Instant::now(),
            transport_ping: TransportPing::default(),
            stop: Arc::clone(stop),
            authorization_deadline: None,
            decode_failures_in_window: 0,
            decode_window: Instant::now(),
            subscription_budget,
        };
        session.send(&json!({"type":"SETUP","channel":0,"version":"0.1-DXF-JS/0.3.0","keepaliveTimeout":60,"acceptKeepaliveTimeout":60}))?;
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut setup = false;
        let mut authenticated = false;
        while Instant::now() < deadline {
            let Some(text) = session.read(deadline)? else {
                continue;
            };
            let frame: Value =
                serde_json::from_str(&text).map_err(|_| "DXLink sent invalid control JSON")?;
            match frame.get("type").and_then(Value::as_str) {
                Some("SETUP") if !setup => {
                    let version = frame
                        .get("version")
                        .and_then(Value::as_str)
                        .ok_or("DXLink server version missing")?;
                    let timeout = frame
                        .get("keepaliveTimeout")
                        .and_then(Value::as_f64)
                        .ok_or("DXLink timeout missing")?;
                    if !(version == "1.0" || version.starts_with("1.0-"))
                        || !(5.0..=120.0).contains(&timeout)
                    {
                        return Err("DXLink server setup unsupported".into());
                    }
                    session.keepalive_interval = Duration::try_from_secs_f64(timeout / 2.0)
                        .map_err(|_| "DXLink timeout invalid")?
                        .min(Duration::from_secs(30));
                    session.keepalive_at = Instant::now() + session.keepalive_interval;
                    setup = true;
                }
                Some("AUTH_STATE") if setup => match frame.get("state").and_then(Value::as_str) {
                    Some("UNAUTHORIZED") if !authenticated => {
                        session.authenticate(&token.token)?;
                        authenticated = true;
                    }
                    Some("AUTHORIZED") if authenticated => return Ok(session),
                    _ => return Err("DXLink authorization rejected".into()),
                },
                Some("KEEPALIVE") => {}
                _ => return Err("DXLink authorization protocol failed".into()),
            }
        }
        Err("DXLink authorization timed out".into())
    }

    /// Reauthorizes the healthy socket without retiring subscriptions.
    /// # Errors
    /// Returns a transport failure or rejects overlapping authorization.
    pub fn reauthorize(&mut self, token: &QuoteToken) -> Result<(), String> {
        if self.authorization_deadline.is_some() {
            return Err("DXLink authorization is already pending".into());
        }
        self.authenticate(&token.token)?;
        self.authorization_deadline = Some(Instant::now() + Duration::from_secs(15));
        Ok(())
    }

    /// Opens a virtual feed channel with bounded subscriptions.
    /// # Errors
    /// Rejects duplicate channels, capacity exhaustion or invalid subscription fields.
    pub fn open(
        &mut self,
        channel: u64,
        contract: &str,
        subscriptions: Vec<Subscription>,
    ) -> Result<(), String> {
        if channel == 0
            || channel.is_multiple_of(2)
            || self.channels.contains_key(&channel)
            || self.channels.len() >= MAXIMUM_CHANNELS
            || subscriptions.len() > 128
            || !matches!(contract, "AUTO" | "STREAM" | "HISTORY")
        {
            return Err("DXLink channel capacity or identity invalid".into());
        }
        for sub in &subscriptions {
            validate_subscription(sub)?;
        }
        // Reserve the eventual CHANNEL_CANCEL as well as each added subscription.
        // This keeps cancellation possible when a busy history channel retires.
        if !self
            .subscription_budget
            .lock()
            .map_err(|_| "DXLink subscription budget lock poisoned")?
            .reserve(Instant::now(), subscriptions.len().saturating_mul(2))
        {
            return Err("DXLink subscription change budget exhausted".into());
        }
        self.send(&json!({"type":"CHANNEL_REQUEST","channel":channel,"service":"FEED","parameters":{"contract":contract}}))?;
        self.channels.insert(
            channel,
            Channel {
                compact: false,
                subscriptions,
                fields: BTreeMap::new(),
            },
        );
        Ok(())
    }

    /// Reconciles subscriptions on an existing healthy channel.
    /// # Errors
    /// Rejects malformed demand or transport failure.
    pub fn replace(
        &mut self,
        channel: u64,
        subscriptions: Vec<Subscription>,
    ) -> Result<(), SubscriptionChangeError> {
        if subscriptions.len() > 128 {
            return Err(SubscriptionChangeError::CapacityExhausted);
        }
        for sub in &subscriptions {
            validate_subscription(sub).map_err(SubscriptionChangeError::Other)?;
        }
        let changes = self
            .subscription_changes_for_replace(channel, &subscriptions)
            .map_err(SubscriptionChangeError::Other)?;
        if !self
            .subscription_budget
            .lock()
            .map_err(|_| {
                SubscriptionChangeError::Other("DXLink subscription budget lock poisoned".into())
            })?
            .reserve(Instant::now(), changes)
        {
            return Err(SubscriptionChangeError::BudgetExhausted);
        }
        let state = self.channels.get_mut(&channel).ok_or_else(|| {
            SubscriptionChangeError::Other("DXLink channel is unavailable".into())
        })?;
        if state.subscriptions == subscriptions {
            return Ok(());
        }
        let remove: Vec<Value> = state
            .subscriptions
            .iter()
            .filter(|sub| !subscriptions.contains(sub))
            .map(subscription_value)
            .collect();
        let add: Vec<Value> = subscriptions
            .iter()
            .filter(|sub| !state.subscriptions.contains(sub))
            .map(subscription_value)
            .collect();
        let compact = state.compact;
        state.subscriptions = subscriptions;
        if compact {
            self.send(
                &json!({"type":"FEED_SUBSCRIPTION","channel":channel,"remove":remove,"add":add}),
            )
            .map_err(SubscriptionChangeError::Other)?;
        }
        Ok(())
    }

    /// Counts a pending live-channel change without modifying its subscriptions.
    /// # Errors
    /// Rejects an unavailable channel.
    pub fn subscription_changes_for_replace(
        &self,
        channel: u64,
        subscriptions: &[Subscription],
    ) -> Result<usize, String> {
        let current = &self
            .channels
            .get(&channel)
            .ok_or("DXLink channel is unavailable")?
            .subscriptions;
        Ok(current
            .iter()
            .filter(|sub| !subscriptions.contains(sub))
            .count()
            + subscriptions
                .iter()
                .filter(|sub| !current.contains(sub))
                .count())
    }

    /// Checks the rolling session budget before a worker starts a change batch.
    /// # Errors
    /// Returns an error if the shared budget lock is poisoned.
    pub fn can_change_subscriptions(&mut self, changes: usize) -> Result<bool, String> {
        Ok(self
            .subscription_budget
            .lock()
            .map_err(|_| "DXLink subscription budget lock poisoned")?
            .has_capacity(Instant::now(), changes))
    }

    /// Retires a channel without closing the provider session.
    /// # Errors
    /// Returns a transport failure.
    pub fn close_channel(&mut self, channel: u64) -> Result<(), String> {
        if self.channels.remove(&channel).is_some() {
            self.send(&json!({"type":"CHANNEL_CANCEL","channel":channel}))?;
        }
        self.pending.retain(|event| event.channel() != channel);
        Ok(())
    }

    /// Takes the newest WebSocket ping round trip, measured once per answered ping.
    pub fn take_transport_rtt_nanos(&mut self) -> Option<u64> {
        self.transport_ping.measured.take()
    }

    /// Reads one validated event; a timeout on a healthy quiet feed returns None.
    /// # Errors
    /// Rejects malformed data, overload, cancellation and disconnected sessions.
    pub fn poll(&mut self, deadline: Instant) -> Result<Option<FeedEvent>, String> {
        if self
            .authorization_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err("DXLink authorization refresh timed out".into());
        }
        if let Some(event) = self.pending.pop_front() {
            return Ok(Some(event));
        }
        while Instant::now() < deadline {
            let Some(text) = self.read(deadline)? else {
                return Ok(None);
            };
            let header: Header =
                serde_json::from_str(&text).map_err(|_| "DXLink frame header invalid")?;
            if header.r#type == "FEED_DATA" {
                let Some(channel) = self.channels.get(&header.channel) else {
                    continue;
                }; // Retired channel cannot publish.
                let Ok(events) = decode_data(&text, header.channel, &channel.fields) else {
                    if self.decode_window.elapsed() >= Duration::from_secs(60) {
                        self.decode_window = Instant::now();
                        self.decode_failures_in_window = 0;
                    }
                    self.decode_failures_in_window =
                        self.decode_failures_in_window.saturating_add(1);
                    diagnostic!(
                        "Aeris DXLink rejected malformed feed data ({}/8)",
                        self.decode_failures_in_window
                    );
                    if self.decode_failures_in_window > 8 {
                        return Err(
                            "DXLink malformed feed data exceeded its per-connection budget".into(),
                        );
                    }
                    continue;
                };
                self.pending.extend(events);
                if let Some(event) = self.pending.pop_front() {
                    return Ok(Some(event));
                }
            } else {
                self.control(&header, &text)?;
            }
        }
        Ok(None)
    }

    fn control(&mut self, header: &Header, text: &str) -> Result<(), String> {
        let frame: Value = serde_json::from_str(text).map_err(|_| "DXLink control JSON invalid")?;
        match header.r#type.as_str() {
            "KEEPALIVE"=>Ok(()),
            "CHANNEL_OPENED" if self.channels.contains_key(&header.channel)=>self.send(&json!({"type":"FEED_SETUP","channel":header.channel,"acceptAggregationPeriod":0,"acceptDataFormat":"COMPACT","acceptEventFields":{"Quote":QUOTE,"Candle":CANDLE,"TimeAndSale":TAPE}})),
            "CHANNEL_OPENED" if header.channel >= 7 => Ok(()),
            "FEED_CONFIG"=>{
                let Some(channel)=self.channels.get_mut(&header.channel) else {return Ok(())};
                let was_compact=channel.compact;
                if let Some(aggregation)=frame.get("aggregationPeriod") {
                    if aggregation.as_f64()!=Some(0.0) {return Err("DXLink conflated data is unsuitable for tick analysis".into());}
                } else if !was_compact && frame.get("dataFormat").and_then(Value::as_str)==Some("COMPACT") {return Err("DXLink aggregation period missing".into());}
                match frame.get("dataFormat").and_then(Value::as_str) {
                    Some("COMPACT")=>channel.compact=true,
                    Some("FULL") if !was_compact=>return Ok(()),
                    None if was_compact=>{},
                    _=>return Err("DXLink changed to an unsupported data format".into()),
                }
                if let Some(fields)=frame.get("eventFields") {
                    let fields=fields.as_object().ok_or("DXLink schema map invalid")?;
                    for (kind, required) in [("Quote",QUOTE),("Candle",CANDLE),("TimeAndSale",TAPE)] {
                        let Some(names)=fields.get(kind) else {continue};
                        let names:Vec<String>=serde_json::from_value(names.clone()).map_err(|_|"DXLink schema names invalid")?;
                        if names.len()>32 || required.iter().any(|name|!names.iter().any(|n|n==name)) || names.iter().enumerate().any(|(i,name)| names[..i].contains(name)) {return Err(format!("DXLink {kind} schema omitted or duplicated required fields"));}
                        channel.fields.insert(kind.into(),names);
                    }
                }
                if !was_compact {self.send_subscriptions(header.channel)?;}
                Ok(())
            }
            "CHANNEL_CLOSED" if !self.channels.contains_key(&header.channel)=>Ok(()),
            "ERROR" if header.channel >= 7 && !self.channels.contains_key(&header.channel) => Ok(()),
            "ERROR" | "CHANNEL_CLOSED" if header.channel >= 7 && self.channels.remove(&header.channel).is_some() => {
                self.pending.retain(|event| event.channel() != header.channel);
                self.pending.push_back(FeedEvent::ChannelFailure { channel: header.channel });
                Ok(())
            },
            "AUTH_STATE" if self.authorization_deadline.is_some() && frame.get("state").and_then(Value::as_str)==Some("AUTHORIZED")=>{self.authorization_deadline=None;Ok(())},
            "ERROR"|"CHANNEL_CLOSED"|"AUTH_STATE"=>Err("DXLink feed authorization or channel failed".into()),
            _ => Err(if header.r#type.len() <= 32
                && header.r#type.bytes().all(|byte| byte.is_ascii_uppercase() || byte == b'_') {
                format!("DXLink sent unexpected {} control on channel {}", header.r#type, header.channel)
            } else {
                "DXLink sent unexpected control data".into()
            }),
        }
    }

    fn send_subscriptions(&mut self, channel: u64) -> Result<(), String> {
        let state = self
            .channels
            .get(&channel)
            .ok_or("DXLink channel missing")?;
        let add: Vec<Value> = state
            .subscriptions
            .iter()
            .map(|sub| {
                let mut value = json!({"type":sub.kind,"symbol":sub.symbol});
                if let Some(from) = sub.from_time_ms {
                    value["fromTime"] = json!(from);
                }
                value
            })
            .collect();
        self.send(&json!({"type":"FEED_SUBSCRIPTION","channel":channel,"reset":true,"add":add}))
    }

    fn authenticate(&mut self, token: &str) -> Result<(), String> {
        #[derive(Serialize)]
        struct Auth<'a> {
            r#type: &'static str,
            channel: u64,
            token: &'a str,
        }
        let text = Zeroizing::new(
            serde_json::to_string(&Auth {
                r#type: "AUTH",
                channel: 0,
                token,
            })
            .map_err(|_| "DXLink authorization encoding failed")?,
        );
        self.socket.send_text(&text).map_err(|e| e.to_string())
    }
    fn send(&mut self, frame: &Value) -> Result<(), String> {
        self.socket
            .send_text(&frame.to_string())
            .map_err(|e| e.to_string())
    }
    fn read(&mut self, deadline: Instant) -> Result<Option<String>, String> {
        if self.stop.load(Ordering::Acquire) {
            return Err("DXLink session cancelled".into());
        }
        if Instant::now() >= self.keepalive_at {
            self.send(&json!({"type":"KEEPALIVE","channel":0}))?;
            self.keepalive_at = Instant::now() + self.keepalive_interval;
        }
        if self.transport_ping.due(Instant::now()) {
            self.socket.send_ping().map_err(|e| e.to_string())?;
            self.transport_ping.sent(Instant::now());
        }
        if self.last_received.elapsed() > Duration::from_secs(120) {
            return Err("DXLink heartbeat expired".into());
        }
        match self
            .socket
            .read_event(deadline.min(Instant::now() + Duration::from_millis(100)))
        {
            Ok(MarketSocketEvent::Text(text)) => {
                self.last_received = Instant::now();
                Ok(Some(text))
            }
            Ok(MarketSocketEvent::Pong) => {
                self.last_received = Instant::now();
                self.transport_ping.answered(self.last_received);
                Ok(None)
            }
            Err(e) if e.is_read_timeout() => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

impl Drop for DxlinkSession {
    fn drop(&mut self) {
        self.socket.close();
    }
}
impl FeedEvent {
    #[must_use]
    pub const fn channel(&self) -> u64 {
        match self {
            Self::Quote { channel, .. }
            | Self::Candle { channel, .. }
            | Self::Trade { channel, .. }
            | Self::ChannelFailure { channel } => *channel,
        }
    }
}

fn validate_subscription(sub: &Subscription) -> Result<(), String> {
    if !matches!(sub.kind, "Quote" | "Candle" | "TimeAndSale")
        || sub.symbol.is_empty()
        || sub.symbol.len() > 160
        || sub.symbol.chars().any(char::is_control)
        || sub.from_time_ms.is_some_and(|time| time < 0)
    {
        return Err("DXLink subscription invalid".into());
    }
    Ok(())
}

fn subscription_value(sub: &Subscription) -> Value {
    let mut value = json!({"type":sub.kind,"symbol":sub.symbol});
    if let Some(from) = sub.from_time_ms {
        value["fromTime"] = json!(from);
    }
    value
}

fn decode_data(
    text: &str,
    channel: u64,
    fields: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<FeedEvent>, String> {
    let data: Data = serde_json::from_str(text).map_err(|_| "DXLink compact envelope invalid")?;
    if data.data.len() > 128 || !data.data.len().is_multiple_of(2) {
        return Err("DXLink compact envelope bound exceeded".into());
    }
    let mut events = Vec::new();
    for pair in data.data.as_chunks::<2>().0 {
        let kind: String =
            serde_json::from_str(pair[0].get()).map_err(|_| "DXLink event type invalid")?;
        let names = fields
            .get(&kind)
            .ok_or("DXLink data preceded its negotiated schema")?;
        let values: Vec<Box<RawValue>> =
            serde_json::from_str(pair[1].get()).map_err(|_| "DXLink row invalid")?;
        if names.is_empty()
            || !values.len().is_multiple_of(names.len())
            || events.len() + values.len() / names.len() > 8192
        {
            return Err("DXLink event batch bound or width exceeded".into());
        }
        for row in values.chunks_exact(names.len()) {
            let row = Row { names, values: row };
            if row.string("eventType")? != kind {
                return Err("DXLink event type inconsistent".into());
            }
            events.push(decode_row(channel, &kind, &row)?);
        }
    }
    Ok(events)
}

struct Row<'a> {
    names: &'a [String],
    values: &'a [Box<RawValue>],
}
impl Row<'_> {
    fn raw(&self, name: &str) -> Result<&str, String> {
        self.names
            .iter()
            .position(|n| n == name)
            .and_then(|i| self.values.get(i))
            .map(|v| v.get())
            .ok_or_else(|| format!("DXLink required {name} missing"))
    }
    fn string(&self, name: &str) -> Result<String, String> {
        let value: String = serde_json::from_str(self.raw(name)?)
            .map_err(|_| format!("DXLink {name} string invalid"))?;
        if value.len() > 160 || value.chars().any(char::is_control) {
            return Err("DXLink field bound exceeded".into());
        }
        Ok(value)
    }
    fn integer<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<T, String> {
        serde_json::from_str(self.raw(name)?).map_err(|_| format!("DXLink {name} integer invalid"))
    }
    fn boolean(&self, name: &str) -> Result<bool, String> {
        self.integer(name)
    }
    fn decimal(&self, name: &str) -> Result<Option<i64>, String> {
        let raw = self.raw(name)?;
        if matches!(raw, "null" | "\"NaN\"") {
            return Ok(None);
        }
        let text = raw
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(raw);
        parse_decimal_to_fixed(text, DATA_SCALE).map(Some)
    }
    fn index(&self) -> Result<String, String> {
        let raw = self.raw("index")?;
        if raw.starts_with('"') {
            return self.string("index");
        }
        let value: u64 = serde_json::from_str(raw).map_err(|_| "DXLink event index invalid")?;
        Ok(value.to_string())
    }
}

fn decode_row(channel: u64, kind: &str, row: &Row<'_>) -> Result<FeedEvent, String> {
    let symbol = row.string("eventSymbol")?;
    match kind {
        "Quote" => {
            let bid = row
                .decimal("bidPrice")?
                .zip(row.decimal("bidSize")?)
                .filter(|(price, size)| *price > 0 && *size > 0);
            let ask = row
                .decimal("askPrice")?
                .zip(row.decimal("askSize")?)
                .filter(|(price, size)| *price > 0 && *size > 0);
            let time = row.integer::<i64>("bidTime")?.max(row.integer("askTime")?);
            let time_nanos = if time == 0 {
                None
            } else {
                Some(
                    time.checked_mul(1_000_000)
                        .ok_or("DXLink quote timestamp overflow")?,
                )
            };
            Ok(FeedEvent::Quote {
                channel,
                symbol,
                bid,
                ask,
                time_nanos,
            })
        }
        "Candle" => {
            let flags = row.integer("eventFlags")?;
            let index = row.index()?;
            let time: i64 = row.integer("time")?;
            let count = row.integer("count")?;
            let values = [
                row.decimal("open")?,
                row.decimal("high")?,
                row.decimal("low")?,
                row.decimal("close")?,
                row.decimal("volume")?,
            ];
            let bar = if let [Some(open), Some(high), Some(low), Some(close), Some(volume)] = values
            {
                let time_nanos = time
                    .checked_mul(1_000_000)
                    .ok_or("DXLink candle timestamp overflow")?;
                Some(MarketBar {
                    source_sequence: 1,
                    exchange_timestamp_seconds: time_nanos.div_euclid(1_000_000_000),
                    exchange_timestamp_unix_nanos: time_nanos,
                    open,
                    high,
                    low,
                    close,
                    volume,
                })
            } else {
                None
            };
            Ok(FeedEvent::Candle {
                channel,
                symbol,
                flags,
                index,
                bar,
                count,
            })
        }
        "TimeAndSale" => decode_trade(channel, symbol, row),
        _ => Err("DXLink event type unsupported".into()),
    }
}

fn decode_trade(channel: u64, symbol: String, row: &Row<'_>) -> Result<FeedEvent, String> {
    let flags = row.integer("eventFlags")?;
    let index = row.index()?;
    let kind = row.string("type")?;
    if !matches!(kind.as_str(), "NEW" | "CORRECTION" | "CANCEL") {
        return Err("DXLink trade type invalid".into());
    }
    let valid = row.boolean("validTick")?;
    let trade = if let (Some(price), Some(quantity), true) =
        (row.decimal("price")?, row.decimal("size")?, valid)
    {
        let millis: i64 = row.integer("time")?;
        let nano: i64 = row.integer("timeNanoPart")?;
        if !(0..1_000_000).contains(&nano) {
            return Err("DXLink trade submillisecond timestamp invalid".into());
        }
        let time_nanos = millis
            .checked_mul(1_000_000)
            .and_then(|time| time.checked_add(nano))
            .ok_or("DXLink trade timestamp overflow")?;
        let aggressor = match row.string("aggressorSide")?.as_str() {
            "BUY" => AggressorSide::Buy,
            "SELL" => AggressorSide::Sell,
            "UNDEFINED" => AggressorSide::Unknown,
            _ => return Err("DXLink aggressor invalid".into()),
        };
        Some(TradePrint {
            time_nanos,
            price,
            quantity,
            bid_price: row.decimal("bidPrice")?.filter(|price| *price > 0),
            ask_price: row.decimal("askPrice")?.filter(|price| *price > 0),
            aggressor,
            spread_leg: row.boolean("spreadLeg")?,
        })
    } else {
        None
    };
    Ok(FeedEvent::Trade {
        channel,
        symbol,
        flags,
        index,
        kind,
        trade,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subscription_changes_are_bounded_over_a_rolling_minute() {
        let mut budget = SubscriptionChangeBudget::default();
        let started = Instant::now();
        assert!(budget.reserve(started, 4_000));
        assert!(budget.reserve(started + Duration::from_secs(30), 5_000));
        assert!(!budget.reserve(started + Duration::from_secs(59), 1));
        assert_eq!(budget.used, MAXIMUM_SUBSCRIPTION_CHANGES_PER_MINUTE);
        assert!(budget.reserve(started + Duration::from_secs(60), 4_000));
        assert_eq!(budget.used, MAXIMUM_SUBSCRIPTION_CHANGES_PER_MINUTE);
        assert!(!budget.reserve(started + Duration::from_secs(60), 1));
        assert!(budget.reserve(started + Duration::from_secs(90), 5_000));
        assert_eq!(budget.used, MAXIMUM_SUBSCRIPTION_CHANGES_PER_MINUTE);
    }
    fn assert_budgeted_replace(
        session: &mut DxlinkSession,
        budget: &Arc<Mutex<SubscriptionChangeBudget>>,
        subscription: impl Fn(&str) -> Subscription,
    ) {
        assert!(budget.lock().unwrap().reserve(Instant::now(), 8_998));
        assert!(!session.can_change_subscriptions(2).unwrap());
        assert_eq!(
            session.replace(1, vec![subscription("MSFT")]).unwrap_err(),
            SubscriptionChangeError::BudgetExhausted
        );
        assert_eq!(
            session.channels[&1].subscriptions,
            vec![subscription("AAPL")]
        );
        for (at, _) in &mut budget.lock().unwrap().changes {
            *at = Instant::now().checked_sub(Duration::from_secs(61)).unwrap();
        }
        assert!(session.can_change_subscriptions(2).unwrap());
        session.replace(1, vec![subscription("MSFT")]).unwrap();
    }
    type TestSocket = tungstenite::WebSocket<std::net::TcpStream>;
    fn receive(socket: &mut TestSocket) -> Value {
        loop {
            match socket.read().unwrap() {
                // Tungstenite queues the pong; the next write flushes it.
                tungstenite::Message::Ping(_) | tungstenite::Message::Pong(_) => {}
                message => return serde_json::from_str(message.to_text().unwrap()).unwrap(),
            }
        }
    }
    #[test]
    fn transport_ping_measures_one_round_trip_per_answered_ping() {
        let started = Instant::now();
        let mut ping = TransportPing::default();
        assert!(ping.due(started), "first ping measures RTT immediately");
        ping.sent(started);
        assert!(!ping.due(started + Duration::from_secs(1)));
        ping.answered(started + Duration::from_millis(18));
        assert_eq!(ping.measured.take(), Some(18_000_000));
        ping.answered(started + Duration::from_millis(40));
        assert_eq!(ping.measured, None, "a duplicate pong is not a new sample");
        assert!(ping.due(started + TRANSPORT_PING_INTERVAL));
    }
    fn send(socket: &mut TestSocket, value: &Value) {
        socket
            .send(tungstenite::Message::Text(value.to_string().into()))
            .unwrap();
    }
    fn quote(symbol: &str) -> Value {
        json!({"type":"FEED_DATA","channel":1,"data":["Quote",["Quote",symbol,100,101,2,3,0,0]]})
    }
    fn malformed_quotes(socket: &mut TestSocket) {
        let malformed = json!({"type":"FEED_DATA","channel":1,"data":["Quote",["Quote","MSFT"]]});
        for _ in 0..8 {
            send(socket, &malformed);
        }
        send(socket, &quote("MSFT"));
        send(socket, &malformed);
    }
    fn reject_history_channel(socket: &mut TestSocket) {
        assert_eq!(receive(socket)["type"], "CHANNEL_REQUEST");
        send(socket, &json!({"type":"CHANNEL_OPENED","channel":7}));
        assert_eq!(receive(socket)["type"], "FEED_SETUP");
        send(
            socket,
            &json!({"type":"FEED_CONFIG","channel":7,"dataFormat":"COMPACT","aggregationPeriod":0}),
        );
        assert_eq!(receive(socket)["type"], "FEED_SUBSCRIPTION");
        send(socket, &json!({"type":"ERROR","channel":7}));
    }
    fn assert_history_channel_failure(session: &mut DxlinkSession) {
        session.open(7, "AUTO", Vec::new()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let failure = loop {
            if let Some(event) = session.poll(deadline).unwrap() {
                break event;
            }
            assert!(Instant::now() < deadline, "history channel failure missing");
        };
        assert!(matches!(failure, FeedEvent::ChannelFailure { channel: 7 }));
        assert!(!session.channels.contains_key(&7));
    }
    #[test]
    fn lazy_schema_subscription_changes_and_reauthorization_share_one_socket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            assert_eq!(receive(&mut socket)["type"], "SETUP");
            send(
                &mut socket,
                &json!({"type":"SETUP","channel":0,"version":"1.0-fixture","keepaliveTimeout":60}),
            );
            send(
                &mut socket,
                &json!({"type":"AUTH_STATE","channel":0,"state":"UNAUTHORIZED"}),
            );
            assert_eq!(receive(&mut socket)["type"], "AUTH");
            send(
                &mut socket,
                &json!({"type":"AUTH_STATE","channel":0,"state":"AUTHORIZED"}),
            );
            assert_eq!(receive(&mut socket)["type"], "CHANNEL_REQUEST");
            send(&mut socket, &json!({"type":"CHANNEL_OPENED","channel":1}));
            assert_eq!(receive(&mut socket)["type"], "FEED_SETUP");
            send(
                &mut socket,
                &json!({"type":"FEED_CONFIG","channel":1,"dataFormat":"COMPACT","aggregationPeriod":0}),
            );
            let subscription = receive(&mut socket);
            assert_eq!(subscription["add"][0]["symbol"], "AAPL");
            send(
                &mut socket,
                &json!({"type":"FEED_CONFIG","channel":1,"eventFields":{"Quote":QUOTE}}),
            );
            send(&mut socket, &quote("AAPL"));
            let subscription = receive(&mut socket);
            assert_eq!(subscription["remove"][0]["symbol"], "AAPL");
            assert_eq!(subscription["add"][0]["symbol"], "MSFT");
            assert!(subscription.get("reset").is_none());
            assert_eq!(receive(&mut socket)["type"], "AUTH");
            send(
                &mut socket,
                &json!({"type":"AUTH_STATE","channel":0,"state":"AUTHORIZED"}),
            );
            send(&mut socket, &quote("MSFT"));
            malformed_quotes(&mut socket);
            assert_eq!(receive(&mut socket)["type"], "CHANNEL_CANCEL");
            send(
                &mut socket,
                &json!({"type":"FEED_DATA","channel":1,"data":["retired malformed payload"]}),
            );
            reject_history_channel(&mut socket);
            assert_eq!(receive(&mut socket)["type"], "KEEPALIVE");
        });
        let token = QuoteToken {
            token: "public-test-fixture".into(),
            dxlink_url: format!("ws://{address}/"),
            expires_at: "2099-01-01T00:00:00Z".into(),
            level: "level-1".into(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let budget = Arc::new(Mutex::new(SubscriptionChangeBudget::default()));
        let mut session = DxlinkSession::connect(&token, &stop, Arc::clone(&budget)).unwrap();
        let subscription = |symbol: &str| Subscription {
            kind: "Quote",
            symbol: symbol.into(),
            from_time_ms: None,
        };
        session.open(1, "AUTO", vec![subscription("AAPL")]).unwrap();
        assert!(
            matches!(session.poll(Instant::now()+Duration::from_secs(3)).unwrap(),Some(FeedEvent::Quote {symbol,time_nanos:None,..}) if symbol=="AAPL")
        );
        assert_budgeted_replace(&mut session, &budget, subscription);
        session.reauthorize(&token).unwrap();
        assert!(
            matches!(session.poll(Instant::now()+Duration::from_secs(3)).unwrap(),Some(FeedEvent::Quote {symbol,..}) if symbol=="MSFT")
        );
        assert!(session.authorization_deadline.is_none());
        assert!(
            matches!(session.poll(Instant::now()+Duration::from_secs(3)).unwrap(),Some(FeedEvent::Quote {symbol,..}) if symbol=="MSFT")
        );
        assert!(
            session
                .poll(Instant::now() + Duration::from_secs(3))
                .is_err()
        );
        session.close_channel(1).unwrap();
        assert!(
            session
                .poll(Instant::now() + Duration::from_millis(100))
                .unwrap()
                .is_none()
        );
        assert_history_channel_failure(&mut session);
        session
            .send(&json!({"type":"KEEPALIVE","channel":0}))
            .unwrap();
        server.join().unwrap();
    }
    #[test]
    fn wire_decimal_text_and_trade_identity_keep_precision_and_meaning() {
        let fields = BTreeMap::from([(
            "TimeAndSale".into(),
            TAPE.iter().map(|s| (*s).into()).collect(),
        )]);
        let raw = r#"{"type":"FEED_DATA","channel":3,"data":["TimeAndSale",["TimeAndSale","/ESZ26:XCME",0,9007199254740993,1790800000000,123456,17,5000.25000000,2,5000.00000000,5000.50000000,"SELL",false,true,"NEW"]]}"#;
        let events = decode_data(raw, 3, &fields).expect("wire");
        let FeedEvent::Trade {
            index,
            trade: Some(trade),
            ..
        } = &events[0]
        else {
            panic!("trade")
        };
        assert_eq!(index, "9007199254740993");
        assert_eq!(trade.price, 500_025_000_000);
        assert_eq!(trade.time_nanos, 1_790_800_000_000_123_456);
        assert_eq!(trade.aggressor, AggressorSide::Sell);
        assert_eq!(trade.bid_price, Some(500_000_000_000));
        assert_eq!(trade.ask_price, Some(500_050_000_000));
        assert!(decode_data(&raw.replace("5000.25000000", "5000.250000001"), 3, &fields).is_err());
    }

    #[test]
    fn candle_wire_count_is_required_and_preserved() {
        let fields = BTreeMap::from([(
            "Candle".into(),
            CANDLE.iter().map(|field| (*field).into()).collect(),
        )]);
        let raw = r#"{"type":"FEED_DATA","channel":3,"data":["Candle",["Candle","/ESZ26:XCME{=m}",0,9007199254740993,1790800000000,1,5000.0,5001.0,4999.0,5000.5,27.0,83]]}"#;
        let events = decode_data(raw, 3, &fields).expect("candle wire");
        let FeedEvent::Candle {
            index,
            count,
            bar: Some(bar),
            ..
        } = &events[0]
        else {
            panic!("candle")
        };
        assert_eq!(index, "9007199254740993");
        assert_eq!(*count, 83);
        assert_eq!(bar.close, 500_050_000_000);
        assert!(decode_data(&raw.replace(",83]]}", "]]}"), 3, &fields).is_err());
    }
}
