//! Deterministic cTrader market-runtime tests over a scripted session link.
//!
//! The scripted `CtraderMarketLink` stands in for the hosted session: it
//! records every request in wire order, serves a fixed synthetic trendbar
//! series, and delivers queued spot/depth events or an injected fault. The
//! worker is driven one loop turn at a time, so no test sleeps or races.

use super::super::{BUILT_IN_PROVIDER_DESCRIPTORS, HistoryRange, MarketStream, ProviderGeneration};
use super::*;
use std::num::NonZeroU64;

const DEMO_CTID: u64 = 1001;
const LIVE_CTID: u64 = 2002;
const DEMO_SYMBOL: u64 = 1;
const LIVE_SYMBOL: u64 = 41;
const M1: i32 = 1;
const QUOTE_ASSET: u64 = 11;

// ---------------------------------------------------------------------------
// Minimal protobuf wire encoding for scripted responses.
// ---------------------------------------------------------------------------

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn put_key(out: &mut Vec<u8>, field: u32, length_delimited: bool) {
    put_varint(
        out,
        u64::from((field << 3) | (u32::from(length_delimited) * 2)),
    );
}

fn field_varint(out: &mut Vec<u8>, field: u32, value: u64) {
    put_key(out, field, false);
    put_varint(out, value);
}

/// Proto2 `int64`/`int32` encode as two's-complement varints.
fn field_int(out: &mut Vec<u8>, field: u32, value: i64) {
    field_varint(out, field, value.cast_unsigned());
}

fn field_bytes(out: &mut Vec<u8>, field: u32, bytes: &[u8]) {
    put_key(out, field, true);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn field_string(out: &mut Vec<u8>, field: u32, value: &str) {
    field_bytes(out, field, value.as_bytes());
}

/// Proto2 `double` encodes as a little-endian fixed64.
fn field_double(out: &mut Vec<u8>, field: u32, value: f64) {
    put_varint(out, u64::from((field << 3) | 1));
    out.extend_from_slice(&value.to_le_bytes());
}

fn frame(payload_type: u32, payload: Vec<u8>) -> ProtoMessage {
    ProtoMessage {
        payload_type,
        payload: Some(payload),
        client_msg_id: None,
    }
}

fn ack_frame(payload_type: u32, ctid: u64) -> ProtoMessage {
    let mut payload = Vec::new();
    field_int(&mut payload, 2, ctid.cast_signed());
    frame(payload_type, payload)
}

/// One `ProtoOATrendbar`. Live bars carry the period and no `deltaClose`;
/// history bars carry every delta and no period.
fn trendbar_body(minute: i64, index: usize, period: Option<i32>, close: bool) -> Vec<u8> {
    let mut body = Vec::new();
    field_int(&mut body, 3, 10 + signed(index));
    if let Some(period) = period {
        field_varint(
            &mut body,
            4,
            u64::try_from(period).expect("positive period"),
        );
    }
    field_int(&mut body, 5, bar_low(index));
    field_varint(&mut body, 6, 1_000);
    if close {
        field_varint(&mut body, 7, 2_000);
    }
    field_varint(&mut body, 8, 3_000);
    field_varint(&mut body, 9, minute.cast_unsigned());
    body
}

/// Wire prices are divisible by `1_000` so they rescale exactly at any digits.
fn bar_low(index: usize) -> i64 {
    100_000 + signed(index) * 1_000
}

fn signed(value: usize) -> i64 {
    i64::try_from(value).expect("fixture value fits in i64")
}

fn trendbar_page_frame(
    ctid: u64,
    period: i32,
    symbol: u64,
    bars: &[(usize, i64)],
    has_more: bool,
) -> ProtoMessage {
    let mut payload = Vec::new();
    field_int(&mut payload, 2, ctid.cast_signed());
    field_varint(
        &mut payload,
        3,
        u64::try_from(period).expect("positive period"),
    );
    for &(index, minute) in bars {
        field_bytes(&mut payload, 5, &trendbar_body(minute, index, None, true));
    }
    field_int(&mut payload, 6, symbol.cast_signed());
    field_varint(&mut payload, 7, u64::from(has_more));
    frame(2138, payload)
}

fn spot_frame(
    ctid: u64,
    symbol: u64,
    bid: Option<u64>,
    ask: Option<u64>,
    live_bar: Option<(i32, i64, usize)>,
    timestamp_ms: i64,
) -> ProtoMessage {
    let mut payload = Vec::new();
    field_int(&mut payload, 2, ctid.cast_signed());
    field_int(&mut payload, 3, symbol.cast_signed());
    if let Some(bid) = bid {
        field_varint(&mut payload, 4, bid);
    }
    if let Some(ask) = ask {
        field_varint(&mut payload, 5, ask);
    }
    if let Some((period, minute, index)) = live_bar {
        field_bytes(
            &mut payload,
            6,
            &trendbar_body(minute, index, Some(period), false),
        );
    }
    field_int(&mut payload, 8, timestamp_ms);
    frame(2131, payload)
}

/// `quotes` are `(id, size, bid, ask)` with exactly one side set.
fn depth_frame(
    ctid: u64,
    symbol: u64,
    quotes: &[(u64, u64, Option<u64>, Option<u64>)],
    deleted: &[u64],
) -> ProtoMessage {
    let mut payload = Vec::new();
    field_int(&mut payload, 2, ctid.cast_signed());
    field_varint(&mut payload, 3, symbol);
    for &(id, size, bid, ask) in quotes {
        let mut quote = Vec::new();
        field_varint(&mut quote, 1, id);
        field_varint(&mut quote, 3, size);
        if let Some(bid) = bid {
            field_varint(&mut quote, 4, bid);
        }
        if let Some(ask) = ask {
            field_varint(&mut quote, 5, ask);
        }
        field_bytes(&mut payload, 4, &quote);
    }
    for &id in deleted {
        field_varint(&mut payload, 5, id);
    }
    frame(2155, payload)
}

fn symbols_list_frame(ctid: u64, symbols: &[(u64, String, Option<String>)]) -> ProtoMessage {
    let mut payload = Vec::new();
    field_int(&mut payload, 2, ctid.cast_signed());
    for (symbol, name, description) in symbols {
        let mut body = Vec::new();
        field_int(&mut body, 1, (*symbol).cast_signed());
        field_string(&mut body, 2, name);
        field_varint(&mut body, 3, 1);
        field_varint(&mut body, 5, QUOTE_ASSET);
        if let Some(description) = description {
            field_string(&mut body, 7, description);
        }
        field_bytes(&mut payload, 3, &body);
    }
    frame(2115, payload)
}

fn asset_list_frame(ctid: u64) -> ProtoMessage {
    let mut payload = Vec::new();
    field_int(&mut payload, 2, ctid.cast_signed());
    let mut asset = Vec::new();
    field_varint(&mut asset, 1, QUOTE_ASSET);
    field_string(&mut asset, 2, "USD");
    field_bytes(&mut payload, 3, &asset);
    frame(2113, payload)
}

fn symbol_by_id_frame(ctid: u64, symbols: &[(u64, i32)]) -> ProtoMessage {
    let mut payload = Vec::new();
    field_int(&mut payload, 2, ctid.cast_signed());
    for &(symbol, digits) in symbols {
        let mut body = Vec::new();
        field_int(&mut body, 1, symbol.cast_signed());
        let digits = u64::try_from(digits).expect("positive digits");
        field_varint(&mut body, 2, digits);
        field_varint(&mut body, 3, digits);
        field_varint(&mut body, 9, 10_000_000);
        field_varint(&mut body, 10, 100);
        field_varint(&mut body, 11, 100);
        field_varint(&mut body, 30, 10_000_000);
        field_bytes(&mut payload, 3, &body);
    }
    frame(2117, payload)
}

// ---------------------------------------------------------------------------
// Trading frames shaped like the demo captures (see the adapter's trading fixtures).
// ---------------------------------------------------------------------------

fn trade_data(symbol: u64, volume: u64, side: u64) -> Vec<u8> {
    let mut body = Vec::new();
    field_int(&mut body, 1, symbol.cast_signed());
    field_varint(&mut body, 2, volume);
    field_varint(&mut body, 3, side);
    field_int(&mut body, 4, 1_791_467_000_000);
    body
}

/// A `ProtoOAOrder`: `kind` 2 is limit, `status` 1 accepted and 2 filled.
fn order_body(order_id: u64, client: &str, kind: u64, status: u64, executed: u64) -> Vec<u8> {
    let mut body = Vec::new();
    field_int(&mut body, 1, order_id.cast_signed());
    field_bytes(&mut body, 2, &trade_data(DEMO_SYMBOL, 100_000, 1));
    field_varint(&mut body, 3, kind);
    field_varint(&mut body, 4, status);
    field_varint(&mut body, 8, executed);
    if kind == 2 {
        field_double(&mut body, 13, 1.0825);
    }
    field_string(&mut body, 17, client);
    field_int(&mut body, 19, 77);
    body
}

fn position_body(status: u64, price: f64) -> Vec<u8> {
    let mut body = Vec::new();
    field_int(&mut body, 1, 77);
    field_bytes(&mut body, 2, &trade_data(DEMO_SYMBOL, 100_000, 1));
    field_varint(&mut body, 3, status);
    field_int(&mut body, 4, 0);
    field_double(&mut body, 5, price);
    field_int(&mut body, 9, -5);
    field_varint(&mut body, 15, 2);
    body
}

fn deal_body(deal_id: u64, order_id: u64) -> Vec<u8> {
    let mut body = Vec::new();
    field_int(&mut body, 1, deal_id.cast_signed());
    field_int(&mut body, 2, order_id.cast_signed());
    field_int(&mut body, 3, 77);
    field_int(&mut body, 4, 100_000);
    field_int(&mut body, 5, 100_000);
    field_int(&mut body, 6, DEMO_SYMBOL.cast_signed());
    field_int(&mut body, 7, 1_791_467_000_000);
    field_int(&mut body, 8, 1_791_467_000_100);
    field_double(&mut body, 10, 1.0825);
    field_varint(&mut body, 11, 1);
    field_varint(&mut body, 12, 2);
    field_int(&mut body, 14, -5);
    field_varint(&mut body, 17, 2);
    body
}

/// A `ProtoOAExecutionEvent`; `execution` 2 is accepted and 3 filled.
fn execution_frame(
    execution: u64,
    order: Option<Vec<u8>>,
    position: Option<Vec<u8>>,
    deal: Option<Vec<u8>>,
) -> ProtoMessage {
    let mut payload = Vec::new();
    field_int(&mut payload, 2, DEMO_CTID.cast_signed());
    field_varint(&mut payload, 3, execution);
    if let Some(position) = position {
        field_bytes(&mut payload, 4, &position);
    }
    if let Some(order) = order {
        field_bytes(&mut payload, 5, &order);
    }
    if let Some(deal) = deal {
        field_bytes(&mut payload, 6, &deal);
    }
    field_varint(&mut payload, 10, 0);
    frame(2126, payload)
}

fn trader_frame() -> ProtoMessage {
    let mut trader = Vec::new();
    field_int(&mut trader, 1, DEMO_CTID.cast_signed());
    field_int(&mut trader, 2, 1_000_000);
    field_int(&mut trader, 8, QUOTE_ASSET.cast_signed());
    field_varint(&mut trader, 20, 2);
    let mut payload = Vec::new();
    field_int(&mut payload, 2, DEMO_CTID.cast_signed());
    field_bytes(&mut payload, 3, &trader);
    frame(2122, payload)
}

fn reconcile_frame(orders: &[Vec<u8>], positions: &[Vec<u8>]) -> ProtoMessage {
    let mut payload = Vec::new();
    field_int(&mut payload, 2, DEMO_CTID.cast_signed());
    for position in positions {
        field_bytes(&mut payload, 3, position);
    }
    for order in orders {
        field_bytes(&mut payload, 4, order);
    }
    frame(2125, payload)
}

fn deal_list_frame(deals: &[Vec<u8>]) -> ProtoMessage {
    let mut payload = Vec::new();
    field_int(&mut payload, 2, DEMO_CTID.cast_signed());
    for deal in deals {
        field_bytes(&mut payload, 3, deal);
    }
    field_varint(&mut payload, 4, 0);
    frame(2134, payload)
}

fn order_error_frame(code: &str) -> ProtoMessage {
    let mut payload = Vec::new();
    field_string(&mut payload, 2, code);
    field_int(&mut payload, 5, DEMO_CTID.cast_signed());
    frame(2132, payload)
}

// ---------------------------------------------------------------------------
// Minimal request payload decoding for the scripted request log.
// ---------------------------------------------------------------------------

fn take_varint(bytes: &mut &[u8]) -> u64 {
    let mut result = 0;
    let mut shift = 0;
    loop {
        let (&byte, rest) = bytes.split_first().expect("wire field is present");
        *bytes = rest;
        result |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return result;
        }
        shift += 7;
    }
}

/// Repeated scalars arrive unpacked from this schema, but accept packed too.
fn repeated_varints(payload: &[u8], wanted: u32) -> Vec<u64> {
    let mut values = Vec::new();
    let mut bytes = payload;
    while !bytes.is_empty() {
        let key = take_varint(&mut bytes);
        let field = u32::try_from(key >> 3).expect("small field number");
        match key & 7 {
            0 => {
                let value = take_varint(&mut bytes);
                if field == wanted {
                    values.push(value);
                }
            }
            1 => bytes = &bytes[8..],
            5 => bytes = &bytes[4..],
            2 => {
                let length = usize::try_from(take_varint(&mut bytes)).expect("field length");
                let (body, rest) = bytes.split_at(length);
                bytes = rest;
                if field == wanted {
                    let mut packed = body;
                    while !packed.is_empty() {
                        values.push(take_varint(&mut packed));
                    }
                }
            }
            invalid => panic!("unsupported wire type {invalid}"),
        }
    }
    values
}

fn scalar(payload: &[u8], field: u32) -> u64 {
    repeated_varints(payload, field)
        .first()
        .copied()
        .expect("request carries the field")
}

// ---------------------------------------------------------------------------
// The scripted session link.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
enum Logged {
    Spots {
        live: bool,
        subscribe: bool,
        ctid: u64,
        symbols: Vec<u64>,
    },
    Depth {
        live: bool,
        subscribe: bool,
        ctid: u64,
        symbols: Vec<u64>,
    },
    Trendbar {
        live: bool,
        subscribe: bool,
        ctid: u64,
        symbol: u64,
        period: i32,
    },
    SymbolsList {
        live: bool,
        ctid: u64,
    },
    SymbolById {
        live: bool,
        ctid: u64,
        symbols: Vec<u64>,
    },
    Assets {
        live: bool,
        ctid: u64,
    },
    History {
        live: bool,
        ctid: u64,
        symbol: u64,
        period: i32,
        from_ms: i64,
        to_ms: i64,
        count: Option<u32>,
    },
    /// A trading request, answered from `Script::trading` in order.
    Trading {
        live: bool,
        payload_type: u32,
        ctid: u64,
    },
}

/// `(symbol id, display name, description)` rows of a scripted catalog.
type ScriptedSymbols = Vec<(u64, String, Option<String>)>;

#[derive(Default)]
struct Script {
    accounts: Vec<CtraderAccount>,
    opens: Vec<bool>,
    closed: usize,
    authorized: Vec<u64>,
    requests: Vec<Logged>,
    events: VecDeque<(bool, ProtoMessage)>,
    fault: Option<SessionFault>,
    /// Returned by the next host open instead of a session.
    open_fault: Option<SessionFault>,
    /// Accounts whose symbol list request fails.
    failing_lists: BTreeSet<u64>,
    /// Ascending bar open minutes per `(symbol, period)`.
    series: BTreeMap<(u64, i32), Vec<i64>>,
    page_limit: Option<usize>,
    symbols: BTreeMap<(bool, u64), ScriptedSymbols>,
    digits: BTreeMap<(bool, u64, u64), i32>,
    /// Answers to trading requests, in request order.
    trading: VecDeque<ProtoMessage>,
}

struct TrendbarQuery {
    ctid: u64,
    symbol: u64,
    period: i32,
    from_ms: i64,
    to_ms: i64,
    count: Option<u32>,
}

impl Script {
    fn serve_trendbars(&self, query: &TrendbarQuery) -> ProtoMessage {
        let minutes = self.series.get(&(query.symbol, query.period));
        let candidates: Vec<(usize, i64)> = minutes
            .map(|minutes| {
                minutes
                    .iter()
                    .copied()
                    .enumerate()
                    .filter(|&(_, minute)| {
                        let open_ms = minute * 60_000;
                        open_ms >= query.from_ms && open_ms <= query.to_ms
                    })
                    .collect()
            })
            .unwrap_or_default();
        let limit = query
            .count
            .map_or(candidates.len(), |count| {
                usize::try_from(count).expect("page count")
            })
            .min(self.page_limit.unwrap_or(usize::MAX))
            .min(candidates.len());
        let returned = &candidates[candidates.len() - limit..];
        trendbar_page_frame(
            query.ctid,
            query.period,
            query.symbol,
            returned,
            candidates.len() > limit,
        )
    }

    fn respond(&self, live: bool, logged: &Logged) -> ProtoMessage {
        match *logged {
            Logged::Spots {
                subscribe, ctid, ..
            } => ack_frame(if subscribe { 2128 } else { 2130 }, ctid),
            Logged::Depth {
                subscribe, ctid, ..
            } => ack_frame(if subscribe { 2157 } else { 2159 }, ctid),
            Logged::Trendbar {
                subscribe, ctid, ..
            } => ack_frame(if subscribe { 2165 } else { 2166 }, ctid),
            Logged::SymbolsList { ctid, .. } => symbols_list_frame(
                ctid,
                self.symbols
                    .get(&(live, ctid))
                    .cloned()
                    .as_deref()
                    .unwrap_or(&[]),
            ),
            Logged::SymbolById {
                ctid, ref symbols, ..
            } => symbol_by_id_frame(
                ctid,
                &symbols
                    .iter()
                    .filter_map(|symbol| {
                        self.digits
                            .get(&(live, ctid, *symbol))
                            .map(|digits| (*symbol, *digits))
                    })
                    .collect::<Vec<_>>(),
            ),
            Logged::Assets { ctid, .. } => asset_list_frame(ctid),
            Logged::History {
                ctid,
                symbol,
                period,
                from_ms,
                to_ms,
                count,
                ..
            } => self.serve_trendbars(&TrendbarQuery {
                ctid,
                symbol,
                period,
                from_ms,
                to_ms,
                count,
            }),
            Logged::Trading { payload_type, .. } => {
                panic!("trading request {payload_type} is answered from the queue")
            }
        }
    }
}

fn scripted() -> Arc<Mutex<Script>> {
    Arc::new(Mutex::new(Script {
        accounts: vec![
            CtraderAccount {
                ctid: DEMO_CTID,
                is_live: false,
                trader_login: None,
                broker_title: None,
            },
            CtraderAccount {
                ctid: LIVE_CTID,
                is_live: true,
                trader_login: None,
                broker_title: None,
            },
        ],
        ..Script::default()
    }))
}

struct ScriptLink {
    live: bool,
    accounts: Vec<CtraderAccount>,
    script: Arc<Mutex<Script>>,
}

impl ScriptLink {
    fn open(script: &Arc<Mutex<Script>>, live: bool) -> Box<dyn CtraderMarketLink> {
        let accounts = script.lock().unwrap().accounts.clone();
        Box::new(Self {
            live,
            accounts,
            script: Arc::clone(script),
        })
    }
}

impl CtraderMarketLink for ScriptLink {
    fn accounts(&self) -> &[CtraderAccount] {
        &self.accounts
    }

    fn authorize(&mut self, account: &CtraderAccount) -> Result<(), SessionFault> {
        self.script.lock().unwrap().authorized.push(account.ctid);
        Ok(())
    }

    fn request(&mut self, request: MarketRequest) -> Result<ProtoMessage, SessionFault> {
        let mut script = self.script.lock().unwrap();
        // The script encodes identifiers and timestamps as two's-complement
        // varints, so decoding them is the identity or a `cast_signed`.
        let logged = match request.payload_type {
            2127 | 2129 => Logged::Spots {
                live: self.live,
                subscribe: request.payload_type == 2127,
                ctid: scalar(&request.payload, 2),
                symbols: repeated_varints(&request.payload, 3),
            },
            2156 | 2158 => Logged::Depth {
                live: self.live,
                subscribe: request.payload_type == 2156,
                ctid: scalar(&request.payload, 2),
                symbols: repeated_varints(&request.payload, 3),
            },
            2135 | 2136 => Logged::Trendbar {
                live: self.live,
                subscribe: request.payload_type == 2135,
                ctid: scalar(&request.payload, 2),
                period: i32::try_from(scalar(&request.payload, 3)).expect("period"),
                symbol: scalar(&request.payload, 4),
            },
            2114 => Logged::SymbolsList {
                live: self.live,
                ctid: scalar(&request.payload, 2),
            },
            2112 => Logged::Assets {
                live: self.live,
                ctid: scalar(&request.payload, 2),
            },
            2116 => Logged::SymbolById {
                live: self.live,
                ctid: scalar(&request.payload, 2),
                symbols: repeated_varints(&request.payload, 3),
            },
            2137 => Logged::History {
                live: self.live,
                ctid: scalar(&request.payload, 2),
                from_ms: scalar(&request.payload, 3).cast_signed(),
                to_ms: scalar(&request.payload, 4).cast_signed(),
                period: i32::try_from(scalar(&request.payload, 5)).expect("period"),
                symbol: scalar(&request.payload, 6),
                count: repeated_varints(&request.payload, 7)
                    .first()
                    .map(|count| u32::try_from(*count).expect("count")),
            },
            trading @ (2106 | 2108 | 2109 | 2110 | 2111 | 2121 | 2124 | 2133 | 2179 | 2181) => {
                let logged = Logged::Trading {
                    live: self.live,
                    payload_type: trading,
                    ctid: scalar(&request.payload, 2),
                };
                script.requests.push(logged);
                return script.trading.pop_front().ok_or(SessionFault::Timeout);
            }
            other => panic!("unexpected cTrader request type {other}"),
        };
        script.requests.push(logged.clone());
        if let Logged::SymbolsList { ctid, .. } = logged
            && script.failing_lists.contains(&ctid)
        {
            return Err(SessionFault::Protocol);
        }
        Ok(script.respond(self.live, &logged))
    }

    fn next_event(&mut self, _timeout: Duration) -> Result<Option<ProtoMessage>, SessionFault> {
        let mut script = self.script.lock().unwrap();
        if let Some(fault) = script.fault.take() {
            return Err(fault);
        }
        let Some(position) = script
            .events
            .iter()
            .position(|(live, _)| *live == self.live)
        else {
            return Ok(None);
        };
        let (_, event) = script.events.remove(position).expect("position found");
        Ok(Some(event))
    }

    fn close(&mut self) {
        self.script.lock().unwrap().closed += 1;
    }

    fn demo_account(&self, ctid: u64) -> Result<DemoAccount, SessionFault> {
        self.accounts
            .iter()
            .any(|account| account.ctid == ctid && !account.is_live && !self.live)
            .then(|| DemoAccount::observed_for_tests(ctid))
            .ok_or(SessionFault::Protocol)
    }
}

// ---------------------------------------------------------------------------
// Worker harness: one in-thread worker driven a loop turn at a time.
// ---------------------------------------------------------------------------

struct Harness {
    worker: Worker,
    controls: SyncSender<RealtimeControl>,
    catalog_controls: SyncSender<CatalogControl>,
    catalog_events: Receiver<CatalogEvent>,
    events: Receiver<RealtimeEvent>,
    history: SyncSender<HistoryRequest>,
    completions: Receiver<Command>,
    generation: Arc<AtomicU64>,
    counters: Arc<StreamCounters>,
    venue: Arc<VenueSlot>,
    script: Arc<Mutex<Script>>,
}

fn harness(script: Arc<Mutex<Script>>, idle_stop: Duration) -> Harness {
    let (controls, controls_rx) = mpsc::sync_channel(RITHMIC_REALTIME_CONTROL_CAPACITY);
    let (events, events_rx) = mpsc::sync_channel(REALTIME_CAPACITY);
    let (history, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
    let (completions, completions_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (catalog_controls, catalog_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (catalog_events_tx, catalog_events) = mpsc::sync_channel(COMMAND_CAPACITY);
    let generation = Arc::new(AtomicU64::new(1));
    let counters = Arc::new(StreamCounters::default());
    let venue = Arc::new(VenueSlot::default());
    let opener: LinkOpener = {
        let script = Arc::clone(&script);
        Box::new(move |host, _stop| {
            let live = matches!(host, CtraderHost::Live);
            let open_fault = {
                let mut guard = script.lock().unwrap();
                guard.opens.push(live);
                guard.open_fault.take()
            };
            if let Some(fault) = open_fault {
                return Err(HostFault::from(classify(&fault)));
            }
            Ok(ScriptLink::open(&script, live))
        })
    };
    let worker = Worker::new(
        WorkerPorts {
            controls: controls_rx,
            catalog: catalog_rx,
            catalog_events: CatalogPublisher::new(
                catalog_events_tx,
                PROVIDER,
                ProviderCoordinatorWake::for_tests(),
            ),
            events,
            history: history_rx,
            completions,
            generation: Arc::clone(&generation),
            stop: Arc::new(AtomicBool::new(false)),
            wake: ProviderCoordinatorWake::for_tests(),
            counters: Arc::clone(&counters),
            venue: Arc::clone(&venue),
        },
        WorkerConfig {
            opener,
            idle_stop,
            healthy_after: Duration::from_hours(1),
        },
    );
    Harness {
        worker,
        controls,
        catalog_controls,
        catalog_events,
        events: events_rx,
        history,
        completions: completions_rx,
        generation,
        counters,
        venue,
        script,
    }
}

impl Harness {
    /// One turn of the worker's run loop.
    fn drive(&mut self) {
        self.worker.controls();
        self.worker.take_venue();
        self.worker.flush_completions();
        self.worker.catalog();
        if self.worker.epoch_state.paused {
            self.worker.reject_history(DISCONNECTED_DETAIL);
        } else if let Err(error) = self.worker.venue_step().and_then(|()| self.worker.step()) {
            self.worker.recover(error);
        }
        self.worker.close_idle_hosts();
        self.worker.close_idle_epoch();
    }

    fn demand(&mut self, demand: Demand) {
        self.controls
            .try_send(RealtimeControl::Subscribe(demand))
            .expect("control queue accepts demand");
        self.drive();
    }

    fn take_events(&self) -> Vec<RealtimeEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }

    fn take_catalog(&self) -> Vec<CatalogEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.catalog_events.try_recv() {
            events.push(event);
        }
        events
    }

    fn completion(&mut self) -> Command {
        for _ in 0..64 {
            if let Ok(command) = self.completions.try_recv() {
                return command;
            }
            self.drive();
        }
        panic!("history completion did not arrive");
    }

    fn requests(&self) -> Vec<Logged> {
        self.script.lock().unwrap().requests.clone()
    }

    fn opens(&self) -> Vec<bool> {
        self.script.lock().unwrap().opens.clone()
    }

    fn push_event(&self, live: bool, event: ProtoMessage) {
        self.script.lock().unwrap().events.push_back((live, event));
    }
}

fn scripted_host(script: &Arc<Mutex<Script>>, live: bool) -> HostSession {
    HostSession::new(live, ScriptLink::open(script, live))
}

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

fn instrument(live: bool, ctid: u64, symbol: u64) -> InstallProviderInstrument {
    let environment = if live { "live" } else { "demo" };
    InstallProviderInstrument {
        provider: PROVIDER.into(),
        session_generation: 1,
        selection_generation: 1,
        instrument_id: format!("{PROVIDER}:{environment}:{ctid}:{symbol}"),
        provider_symbol: format!("{environment}:{ctid}:{symbol}"),
        display_symbol: format!("SYM{symbol}"),
        venue_id: venue(live).into(),
        price_scale: 5,
        quantity_scale: 2,
        entitlement_id: ENTITLEMENT.into(),
        price_increment: Some(1),
        ..Default::default()
    }
}

fn series(live: bool, ctid: u64, symbol: u64, seconds: u32) -> BarSeriesKey {
    BarSeriesKey {
        provider_id: PROVIDER.into(),
        instrument_id: instrument(live, ctid, symbol).instrument_id,
        entitlement_id: ENTITLEMENT.into(),
        period: BarPeriod::time(seconds).expect("time period"),
        definition_version: 1,
    }
}

fn history_request(
    series: &BarSeriesKey,
    instrument: &InstallProviderInstrument,
    generation: u64,
    maximum_bars: usize,
    range: Option<HistoryRange>,
) -> HistoryRequest {
    HistoryRequest {
        series: series.clone(),
        provider_generation: ProviderGeneration(NonZeroU64::new(generation).expect("generation")),
        instrument: Some(instrument.clone()),
        maximum_bars,
        range,
        stop: Arc::new(AtomicBool::new(false)),
    }
}

/// Registers `(symbol, period)` history bars opening at consecutive period
/// steps ending `back` steps before the current instant.
fn script_series(
    script: &Arc<Mutex<Script>>,
    symbol: u64,
    period: TrendbarPeriod,
    back: i64,
    count: usize,
) {
    let step_minutes = maximum_duration_ms(period) / 60_000;
    let now_minute = now_nanos().expect("clock").div_euclid(60_000_000_000);
    let last = now_minute - back * step_minutes;
    let count = i64::try_from(count).expect("fixture bar count");
    let minutes: Vec<i64> = (0..count)
        .map(|index| last - (count - 1 - index) * step_minutes)
        .collect();
    script
        .lock()
        .unwrap()
        .series
        .insert((symbol, period.wire()), minutes);
}

fn snapshot_of(task: HistoryTask) -> HistorySnapshot {
    task.snapshot().expect("history snapshot")
}

fn run_history(
    script: &Arc<Mutex<Script>>,
    request: HistoryRequest,
) -> (HistorySnapshot, Vec<Logged>) {
    let mut host = scripted_host(script, false);
    let before = script.lock().unwrap().requests.len();
    let mut task = match HistoryTask::begin(request, 1) {
        Ok(task) => task,
        Err(retired) => panic!("history task begins: {}", retired.1),
    };
    for _ in 0..32 {
        if task
            .advance(&mut host)
            .unwrap_or_else(|failure| panic!("history page: {}", failure.detail()))
        {
            let snapshot = snapshot_of(task);
            let requests = script.lock().unwrap().requests[before..].to_vec();
            return (snapshot, requests);
        }
    }
    panic!("history task did not finish");
}

// ---------------------------------------------------------------------------
// VAL-MDATA-001: registration, capabilities, presentation.
// ---------------------------------------------------------------------------

#[test]
fn ctrader_descriptor_registers_candle_only_broker_capability() {
    let descriptor = *BUILT_IN_PROVIDER_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.id == "ctrader")
        .expect("cTrader is a built-in provider");
    assert_eq!(descriptor.id, DESCRIPTOR.id);
    assert_eq!(
        descriptor.live_model,
        super::super::LiveModel::ProviderCandles
    );
    assert_eq!(
        descriptor.gap_policy,
        super::super::CandleGapPolicy::SessionGapsAllowed
    );
    assert_eq!(
        descriptor.history_source,
        super::super::HistorySourceKind::ProviderSession
    );
    assert_eq!(
        descriptor.connection_kind,
        super::super::ProviderConnectionKind::BrokerCapability
    );
    assert_eq!(
        descriptor.recovery_policy,
        super::super::ProviderRecoveryPolicy::WorkerReconcilesDemand
    );
    assert_eq!(
        descriptor.idle_stop_policy,
        super::super::IdleStopPolicy::WorkerManaged
    );
    assert_eq!(
        descriptor.authorization_revoked_detail,
        "cTrader disconnected"
    );
    let streams = descriptor.capabilities.streams;
    assert!(descriptor.capabilities.historical_bars);
    assert!(descriptor.capabilities.realtime_bars);
    assert!(streams.contains(MarketStream::Bars));
    assert!(streams.contains(MarketStream::Quotes));
    assert!(streams.contains(MarketStream::Depth));
    assert!(
        !streams.contains(MarketStream::Trades),
        "cTrader has no trade stream; candles prove live data instead"
    );
    assert!(!descriptor.presentation.trades_available);
    assert!(descriptor.presentation.depth_available);
    assert!(!descriptor.presentation.market_screen_available);
    assert!(descriptor.presentation.catalog_refresh_on_startup);
    assert_eq!(
        descriptor.presentation.selection_entitlement_id,
        ENTITLEMENT
    );
    for other in BUILT_IN_PROVIDER_DESCRIPTORS {
        if other.capabilities.streams.contains(MarketStream::Trades) {
            assert!(
                other.presentation.trades_available,
                "{} streams trades and must present them",
                other.id
            );
        }
    }
}

#[test]
fn provider_demand_wire_maps_series_quotes_and_depth() {
    let demo = instrument(false, DEMO_CTID, DEMO_SYMBOL);
    let series_key = series(false, DEMO_CTID, DEMO_SYMBOL, 60);
    let depth_only = super::super::ProviderInstrumentDemand {
        instrument: demo.clone(),
        streams: super::super::StreamRequirements::BARS.with(MarketStream::Depth),
        alert_trades: false,
        alert_instrument: None,
        display_depth: false,
    };
    let quotes_only = super::super::ProviderInstrumentDemand {
        streams: super::super::StreamRequirements::BARS.with(MarketStream::Quotes),
        ..depth_only.clone()
    };
    let display_depth = super::super::ProviderInstrumentDemand {
        streams: super::super::StreamRequirements::BARS,
        display_depth: true,
        ..depth_only.clone()
    };
    let trades_only = super::super::ProviderInstrumentDemand {
        streams: super::super::StreamRequirements::NONE.with(MarketStream::Trades),
        ..depth_only.clone()
    };
    let demand = super::super::ProviderDemand {
        instruments: vec![depth_only, quotes_only, display_depth, trades_only],
        candle_series: vec![(
            series_key.clone(),
            super::super::StreamRequirements::BARS,
            demo,
        )],
        ..Default::default()
    };
    let wire = demand.ctrader_wire();
    assert_eq!(wire.series.len(), 1);
    assert_eq!(wire.series[0].0, series_key);
    // Trades-only demand maps to no spot subscription: cTrader has no trades.
    assert_eq!(wire.instruments.len(), 3);
    assert!(
        wire.instruments
            .iter()
            .map(|(_, depth)| *depth)
            .eq([true, false, true])
    );
}

// ---------------------------------------------------------------------------
// VAL-MDATA-002: instrument identity.
// ---------------------------------------------------------------------------

#[test]
fn ctrader_instrument_ids_round_trip_and_reject_foreign_shapes() {
    let demo = parse_instrument_id("ctrader:demo:1001:1").expect("demo route");
    assert!(!demo.live);
    assert_eq!((demo.ctid, demo.symbol_id), (1001, 1));
    assert_eq!(demo.instrument_id(), "ctrader:demo:1001:1");
    assert_eq!(demo.provider_symbol(), "demo:1001:1");
    assert_eq!(demo.venue(), "cTrader Demo");
    let live = parse_instrument_id("ctrader:live:2002:41").expect("live route");
    assert!(live.live);
    assert_eq!(live.instrument_id(), "ctrader:live:2002:41");
    assert_eq!(live.provider_symbol(), "live:2002:41");
    assert_eq!(live.venue(), "cTrader Live");
    for invalid in [
        "ctrader:paper:1001:1",
        "ctrader:demo:abc:1",
        "ctrader:demo:1001:x",
        "ctrader:demo:1001:1:extra",
        "ctrader:demo:1001:",
        "ctrader:demo:1001",
        "ctrader:demo:0:1",
        "ctrader:demo:1001:0",
        "ctrader:demo:99999999999999999999:1",
        "rithmic:demo:1001:1",
        "ctrader::1001:1",
        "",
    ] {
        assert!(
            parse_instrument_id(invalid).is_err(),
            "{invalid} must be rejected"
        );
    }
}

#[test]
fn ctrader_periods_map_to_trendbar_periods_and_labels() {
    assert_eq!(
        trendbar_period(BarPeriod::time(60).expect("m1")).expect("m1"),
        TrendbarPeriod::M1
    );
    assert_eq!(
        trendbar_period(BarPeriod::Session { days: 1 }).expect("d1"),
        TrendbarPeriod::D1
    );
    assert!(
        supported_period(BarPeriod::time(86_400).expect("1d time")).is_err(),
        "D1 opens at 17:00 New York, so it is a session day, not 24 fixed hours"
    );
    assert_eq!(
        candle_interval(BarPeriod::Session { days: 1 }).as_deref(),
        Ok("d1")
    );
    assert_eq!(
        candle_interval(BarPeriod::Week { weeks: 1 }).as_deref(),
        Ok("w1")
    );
    assert_eq!(
        candle_interval(BarPeriod::Month { months: 1 }).as_deref(),
        Ok("mn1")
    );
    assert_eq!(
        candle_interval(BarPeriod::time(900).expect("m15")).as_deref(),
        Ok("m15")
    );
    // M2/M4/M10 and the 21:00-anchored H4/H12 have no canonical period, and
    // non-standard intervals are rejected too. `BarPeriod::time` validates
    // canonical intervals, so non-canonical ones are constructed directly.
    assert!(supported_period(BarPeriod::Time { seconds: 120 }).is_err());
    assert!(supported_period(BarPeriod::Time { seconds: 7 }).is_err());
    assert!(supported_period(BarPeriod::time(7_200).expect("h2")).is_err());
    assert!(
        supported_period(BarPeriod::time(14_400).expect("h4")).is_err(),
        "cTrader H4 bars are not UTC-epoch buckets"
    );
}

// ---------------------------------------------------------------------------
// VAL-MDATA-006: on-demand history paging, contiguity, forming, probe, cap.
// ---------------------------------------------------------------------------

#[test]
fn history_pages_stay_contiguous_and_renumber_sequences() {
    let script = scripted();
    script.lock().unwrap().page_limit = Some(5);
    script_series(&script, DEMO_SYMBOL, TrendbarPeriod::M1, 10, 40);
    let instrument = instrument(false, DEMO_CTID, DEMO_SYMBOL);
    let series = series(false, DEMO_CTID, DEMO_SYMBOL, 60);
    let (snapshot, requests) =
        run_history(&script, history_request(&series, &instrument, 1, 30, None));

    assert_eq!(snapshot.bars.len(), 30);
    for (index, bar) in snapshot.bars.iter().enumerate() {
        assert_eq!(bar.source_sequence, index as u64 + 1);
        if index > 0 {
            let previous = snapshot.bars[index - 1].exchange_timestamp_unix_nanos;
            assert_eq!(
                bar.exchange_timestamp_unix_nanos - previous,
                60_000_000_000,
                "bars stay contiguous and ascending"
            );
        }
    }
    // Every scripted bar closed before the request, so nothing is forming.
    assert!(snapshot.forming.is_none());
    assert!(!snapshot.backwards_exhausted);
    assert!(snapshot.handoff_boundary_unix_nanos.is_some());
    assert_eq!(snapshot.price_scale, 5);
    assert_eq!(snapshot.quantity_scale, 0);
    let pages: Vec<_> = requests
        .iter()
        .filter_map(|request| match request {
            Logged::History { to_ms, .. } => Some(*to_ms),
            _ => None,
        })
        .collect();
    assert!(
        pages.len() >= 2,
        "page_limit forced several pages: {pages:?}"
    );
    assert!(pages.len() <= MAXIMUM_HISTORY_PAGES);
    assert!(
        pages.windows(2).all(|pair| pair[0] > pair[1]),
        "paging walks backwards: {pages:?}"
    );
}

#[test]
fn unranged_history_separates_the_still_open_bar_as_forming() {
    let script = scripted();
    // The newest bar opens within its nominal duration of now no matter when
    // the test runs: monthly bars cover a 31-day window from their open.
    script_series(&script, DEMO_SYMBOL, TrendbarPeriod::MN1, 0, 3);
    let instrument = instrument(false, DEMO_CTID, DEMO_SYMBOL);
    let mut month_series = series(false, DEMO_CTID, DEMO_SYMBOL, 60);
    month_series.period = BarPeriod::Month { months: 1 };
    let (snapshot, _) = run_history(
        &script,
        history_request(&month_series, &instrument, 1, 10, None),
    );

    assert_eq!(snapshot.bars.len(), 2);
    let forming = snapshot.forming.expect("the open month is forming");
    assert_eq!(forming.bar.source_sequence, 3);
    assert_eq!(forming.trades, None);
    assert!(
        snapshot.bars[1].exchange_timestamp_unix_nanos < forming.bar.exchange_timestamp_unix_nanos
    );
    assert_eq!(forming.bar.close, 102_000 + 2_000);
}

#[test]
fn ranged_history_requests_an_inclusive_end_and_probes_older_windows() {
    let script = scripted();
    let minutes: Vec<i64> = (1_000..=1_010).collect();
    script
        .lock()
        .unwrap()
        .series
        .insert((DEMO_SYMBOL, M1), minutes);
    let instrument = instrument(false, DEMO_CTID, DEMO_SYMBOL);
    let series = series(false, DEMO_CTID, DEMO_SYMBOL, 60);
    let range = HistoryRange {
        start_unix_nanos: 1_002 * 60_000_000_000,
        end_unix_nanos: 1_007 * 60_000_000_000,
    };
    let (snapshot, requests) = run_history(
        &script,
        history_request(&series, &instrument, 1, 10, Some(range)),
    );

    // The coordinator's exclusive end becomes cTrader's inclusive end: -1 ms.
    let first = requests
        .iter()
        .find_map(|request| match request {
            Logged::History { from_ms, to_ms, .. } => Some((*from_ms, *to_ms)),
            _ => None,
        })
        .expect("history page requested");
    assert_eq!(first, (1_002 * 60_000, 1_007 * 60_000 - 1));
    let opens: Vec<i64> = snapshot
        .bars
        .iter()
        .map(|bar| bar.exchange_timestamp_unix_nanos / 1_000_000_000)
        .collect();
    assert_eq!(
        opens,
        vec![1_002 * 60, 1_003 * 60, 1_004 * 60, 1_005 * 60, 1_006 * 60]
    );
    assert!(snapshot.forming.is_none(), "ranged answers never form");
    assert!(!snapshot.backwards_exhausted);

    // An empty ranged window probes one older span: older bars exist here.
    let before = HistoryRange {
        start_unix_nanos: 2_000 * 60_000_000_000,
        end_unix_nanos: 2_010 * 60_000_000_000,
    };
    let (snapshot, requests) = run_history(
        &script,
        history_request(&series, &instrument, 1, 10, Some(before)),
    );
    assert_eq!(snapshot.bars, [] as [aeris_market_data::MarketBar; 0]);
    assert!(
        !snapshot.backwards_exhausted,
        "the probe found older bars: {requests:?}"
    );
    let probe = requests
        .iter()
        .filter_map(|request| match request {
            Logged::History { to_ms, .. } => Some(*to_ms),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(probe.len(), 2, "one empty page then one probe page");
    assert_eq!(probe[1], 2_000 * 60_000 - 1, "the probe covers older time");

    // With nothing older at all the probe reports backwards exhaustion.
    let ancient = HistoryRange {
        start_unix_nanos: 500 * 60_000_000_000,
        end_unix_nanos: 510 * 60_000_000_000,
    };
    let (snapshot, _) = run_history(
        &script,
        history_request(&series, &instrument, 1, 10, Some(ancient)),
    );
    assert_eq!(snapshot.bars, [] as [aeris_market_data::MarketBar; 0]);
    assert!(snapshot.backwards_exhausted);
}

#[test]
fn history_paging_stops_at_the_page_cap() {
    let script = scripted();
    script.lock().unwrap().page_limit = Some(7);
    // The unranged window reaches seven days back; 10_000 minutes fit inside.
    script_series(&script, DEMO_SYMBOL, TrendbarPeriod::M1, 1, 10_000);
    let instrument = instrument(false, DEMO_CTID, DEMO_SYMBOL);
    let series = series(false, DEMO_CTID, DEMO_SYMBOL, 60);
    let (snapshot, requests) = run_history(
        &script,
        history_request(&series, &instrument, 1, 1_000, None),
    );

    let pages = requests
        .iter()
        .filter(|request| matches!(request, Logged::History { .. }))
        .count();
    assert_eq!(pages, MAXIMUM_HISTORY_PAGES);
    assert_eq!(snapshot.bars.len(), MAXIMUM_HISTORY_PAGES * 7);
    for (index, bar) in snapshot.bars.iter().enumerate() {
        assert_eq!(bar.source_sequence, index as u64 + 1);
    }
}

#[test]
fn retired_generation_history_is_rejected() {
    let instrument = instrument(false, DEMO_CTID, DEMO_SYMBOL);
    let series = series(false, DEMO_CTID, DEMO_SYMBOL, 60);
    let retired = HistoryTask::begin(history_request(&series, &instrument, 2, 10, None), 1);
    let (_, error) = match retired {
        Err(retired) => *retired,
        Ok(_) => panic!("a request from another generation must be retired"),
    };
    assert!(error.contains("retired"), "{error}");

    let stopped = history_request(&series, &instrument, 1, 10, None);
    stopped.stop.store(true, Ordering::Release);
    assert!(HistoryTask::begin(stopped, 1).is_err());
}

// ---------------------------------------------------------------------------
// VAL-MDATA-009 / 012: one shared session per host across demand changes.
// ---------------------------------------------------------------------------

#[test]
fn symbol_and_timeframe_changes_reuse_the_open_session() {
    let script = scripted();
    let mut harness = harness(script, Duration::ZERO);
    let demo = instrument(false, DEMO_CTID, DEMO_SYMBOL);
    let mut demand = Demand::default();
    demand.instruments.push((demo.clone(), true));
    demand
        .series
        .push((series(false, DEMO_CTID, DEMO_SYMBOL, 60), demo.clone()));
    harness.demand(demand);

    assert_eq!(harness.opens(), vec![false]);
    assert_eq!(
        harness.requests(),
        vec![
            Logged::Spots {
                live: false,
                subscribe: true,
                ctid: DEMO_CTID,
                symbols: vec![DEMO_SYMBOL],
            },
            Logged::Depth {
                live: false,
                subscribe: true,
                ctid: DEMO_CTID,
                symbols: vec![DEMO_SYMBOL],
            },
            Logged::Trendbar {
                live: false,
                subscribe: true,
                ctid: DEMO_CTID,
                symbol: DEMO_SYMBOL,
                period: M1,
            },
        ]
    );
    let generations: Vec<u64> = harness
        .take_events()
        .iter()
        .map(RealtimeEvent::generation)
        .collect();
    assert_eq!(generations, [1, 1]);

    // Switching timeframe (m1 -> m5) and the quote symbol keeps the session.
    let other = instrument(false, DEMO_CTID, 2);
    let mut changed = Demand::default();
    changed.instruments.push((other, false));
    changed
        .series
        .push((series(false, DEMO_CTID, DEMO_SYMBOL, 300), demo));
    harness.demand(changed);

    assert_eq!(
        harness.opens(),
        vec![false],
        "no new session for symbol/timeframe changes"
    );
    assert_eq!(
        harness.requests()[3..],
        [
            Logged::Trendbar {
                live: false,
                subscribe: false,
                ctid: DEMO_CTID,
                symbol: DEMO_SYMBOL,
                period: M1,
            },
            Logged::Depth {
                live: false,
                subscribe: false,
                ctid: DEMO_CTID,
                symbols: vec![DEMO_SYMBOL],
            },
            Logged::Spots {
                live: false,
                subscribe: true,
                ctid: DEMO_CTID,
                symbols: vec![2],
            },
            Logged::Trendbar {
                live: false,
                subscribe: true,
                ctid: DEMO_CTID,
                symbol: DEMO_SYMBOL,
                period: 5,
            },
        ],
        "removals precede additions; spots precede depth and trendbars"
    );
    assert!(
        harness.take_events().is_empty(),
        "subscription changes publish no lifecycle events"
    );
    let host = harness.worker.hosts.get(&false).expect("demo host");
    assert_eq!(
        host.spots,
        BTreeSet::from([(DEMO_CTID, DEMO_SYMBOL), (DEMO_CTID, 2)])
    );
    assert!(host.depth.is_empty());
    assert_eq!(
        host.trendbars,
        BTreeSet::from([(DEMO_CTID, DEMO_SYMBOL, 5)])
    );
}

#[test]
fn one_session_per_host_across_symbols_and_accounts() {
    let script = scripted();
    script.lock().unwrap().accounts.push(CtraderAccount {
        ctid: 1003,
        is_live: false,
        trader_login: None,
        broker_title: None,
    });
    let mut harness = harness(script, Duration::ZERO);
    let mut demand = Demand::default();
    for (ctid, symbol) in [(DEMO_CTID, DEMO_SYMBOL), (DEMO_CTID, 2), (1003, 7)] {
        let instrument = instrument(false, ctid, symbol);
        demand
            .series
            .push((series(false, ctid, symbol, 60), instrument));
    }
    let live = instrument(true, LIVE_CTID, LIVE_SYMBOL);
    demand.instruments.push((live, false));
    harness.demand(demand);

    assert_eq!(harness.opens(), vec![false, true]);
    assert_eq!(harness.worker.hosts.len(), 2);
    let demo = harness.worker.hosts.get(&false).expect("demo host");
    assert_eq!(
        demo.spots,
        BTreeSet::from([(DEMO_CTID, DEMO_SYMBOL), (DEMO_CTID, 2), (1003, 7)])
    );
    assert_eq!(demo.trendbars.len(), 3);
    let live_host = harness.worker.hosts.get(&true).expect("live host");
    assert_eq!(live_host.spots, BTreeSet::from([(LIVE_CTID, LIVE_SYMBOL)]));
    assert!(live_host.trendbars.is_empty());
    let authorized = harness.script.lock().unwrap().authorized.clone();
    assert_eq!(
        authorized,
        vec![DEMO_CTID, 1003, LIVE_CTID],
        "each account authorizes once on its host"
    );
}

// ---------------------------------------------------------------------------
// VAL-MDATA-010: idle stop.
// ---------------------------------------------------------------------------

#[test]
fn idle_demand_closes_the_host_and_new_demand_opens_a_fresh_generation() {
    let script = scripted();
    let mut harness = harness(script, Duration::ZERO);
    let demo = instrument(false, DEMO_CTID, DEMO_SYMBOL);
    let mut demand = Demand::default();
    demand
        .series
        .push((series(false, DEMO_CTID, DEMO_SYMBOL, 60), demo.clone()));
    harness.demand(demand.clone());
    let generations: Vec<u64> = harness
        .take_events()
        .iter()
        .map(RealtimeEvent::generation)
        .collect();
    assert_eq!(generations, [1, 1]);
    assert_eq!(harness.opens(), vec![false]);

    // Demand empties: subscriptions come off, the idle host closes and the
    // epoch ends with a Disconnected notice.
    harness.demand(Demand::default());
    assert!(harness.worker.hosts.is_empty());
    assert_eq!(harness.script.lock().unwrap().closed, 1);
    assert!(
        harness
            .take_events()
            .iter()
            .any(|event| matches!(event, RealtimeEvent::Disconnected(1))),
        "the retired epoch publishes Disconnected"
    );
    assert_eq!(harness.generation.load(Ordering::Acquire), 1);

    // New demand opens a fresh session under the next generation.
    harness.demand(demand);
    let events = harness.take_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RealtimeEvent::Connecting(2)))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RealtimeEvent::Connected(2)))
    );
    assert_eq!(harness.opens(), vec![false, false]);
    assert_eq!(harness.generation.load(Ordering::Acquire), 2);
}

// ---------------------------------------------------------------------------
// VAL-MDATA-011: reconnect fencing.
// ---------------------------------------------------------------------------

#[test]
fn host_fault_bumps_the_generation_and_fences_retired_requests_and_events() {
    let script = scripted();
    script_series(&script, DEMO_SYMBOL, TrendbarPeriod::M1, 1, 4);
    let mut harness = harness(Arc::clone(&script), Duration::ZERO);
    let demo = instrument(false, DEMO_CTID, DEMO_SYMBOL);
    let series = series(false, DEMO_CTID, DEMO_SYMBOL, 60);
    let mut demand = Demand::default();
    demand.series.push((series.clone(), demo.clone()));
    harness.demand(demand);
    assert_eq!(harness.opens(), vec![false]);

    // A host fault retires every session and the open generation.
    harness.script.lock().unwrap().fault = Some(SessionFault::Reconnect);
    harness.drive();
    assert!(harness.worker.hosts.is_empty());
    assert!(!harness.worker.epoch_state.open);
    assert_eq!(harness.script.lock().unwrap().closed, 1);
    assert!(
        harness
            .take_events()
            .iter()
            .any(|event| matches!(event, RealtimeEvent::Recovering(1, _)))
    );

    // Events are dropped while no epoch is open: they cannot be stamped with
    // a live generation.
    let stale = spot_frame(
        DEMO_CTID,
        DEMO_SYMBOL,
        Some(109_000),
        Some(110_000),
        None,
        1,
    );
    harness
        .worker
        .accept_spot(false, &stale)
        .expect("dropping a stale event is not an error");
    assert!(harness.take_events().is_empty());

    // The reconnect takes a new generation; everything is re-subscribed.
    harness.worker.retry_at = Instant::now();
    harness.drive();
    assert_eq!(harness.generation.load(Ordering::Acquire), 2);
    assert_eq!(harness.opens(), vec![false, false]);
    assert!(
        harness
            .take_events()
            .iter()
            .any(|event| matches!(event, RealtimeEvent::Connected(2)))
    );
    assert!(
        harness
            .requests()
            .iter()
            .filter(|request| matches!(
                request,
                Logged::Spots {
                    subscribe: true,
                    ..
                }
            ))
            .count()
            == 2,
        "the fresh session re-subscribes from scratch"
    );

    // A history request stamped with the retired generation answers Err.
    harness
        .history
        .try_send(history_request(&series, &demo, 1, 10, None))
        .expect("history queue accepts");
    let retired = harness.completion();
    let Command::HistoryCompleted(_, generation, _, Err(error)) = retired else {
        panic!("retired history answers an error");
    };
    assert_eq!(generation, ProviderGeneration(NonZeroU64::new(1).unwrap()));
    assert!(error.contains("retired"), "{error}");

    // The current generation is served.
    harness
        .history
        .try_send(history_request(&series, &demo, 2, 10, None))
        .expect("history queue accepts");
    let served = harness.completion();
    let Command::HistoryCompleted(_, generation, _, Ok(snapshot)) = served else {
        panic!("current-generation history is served");
    };
    assert_eq!(generation, ProviderGeneration(NonZeroU64::new(2).unwrap()));
    assert_eq!(snapshot.bars.len(), 4);
    assert!(
        snapshot
            .bars
            .iter()
            .enumerate()
            .all(|(index, bar)| bar.source_sequence == index as u64 + 1)
    );
}

// ---------------------------------------------------------------------------
// VAL-MDATA-002 / 013: catalog search and selection, live account data.
// ---------------------------------------------------------------------------

fn catalog_script() -> Arc<Mutex<Script>> {
    let script = scripted();
    {
        let mut guard = script.lock().unwrap();
        guard.symbols.insert(
            (false, DEMO_CTID),
            vec![
                (
                    1,
                    "EURUSD".to_string(),
                    Some("Euro / US Dollar".to_string()),
                ),
                (2, "EURGBP".to_string(), None),
            ],
        );
        guard.symbols.insert(
            (true, LIVE_CTID),
            vec![
                (
                    1,
                    "EURUSD".to_string(),
                    Some("Euro / US Dollar".to_string()),
                ),
                (41, "XAUUSD".to_string(), Some("Gold".to_string())),
            ],
        );
        for (live, ctid, symbol, digits) in [
            (false, DEMO_CTID, 1, 5),
            (false, DEMO_CTID, 2, 5),
            (true, LIVE_CTID, 1, 5),
            (true, LIVE_CTID, 41, 2),
        ] {
            guard.digits.insert((live, ctid, symbol), digits);
        }
    }
    script
}

fn search(consumer_id: u64, generation: u64, query: &str) -> CatalogControl {
    CatalogControl::Search(SearchProviderInstruments {
        consumer_id,
        search_generation: generation,
        provider: PROVIDER.into(),
        query: query.into(),
        maximum_results: 100,
        ..Default::default()
    })
}

fn select(
    selection_generation: u64,
    search_generation: u64,
    symbol: &str,
    entitlement_id: &str,
) -> CatalogControl {
    CatalogControl::Select(SelectProviderInstrument {
        consumer_id: 7,
        selection_generation,
        search_generation,
        provider: PROVIDER.into(),
        symbol: symbol.into(),
        exchange: "cTrader Live".into(),
        entitlement_id: entitlement_id.into(),
    })
}

fn drive_search(
    harness: &mut Harness,
    generation: u64,
    query: &str,
) -> ProviderInstrumentSearchResult {
    harness
        .catalog_controls
        .try_send(search(7, generation, query))
        .expect("catalog control queue accepts");
    let mut events = Vec::new();
    // One uncached account catalog loads per worker turn.
    for _ in 0..=MAXIMUM_CATALOG_ACCOUNTS {
        harness.drive();
        events.extend(harness.take_catalog());
        if !events.is_empty() {
            break;
        }
    }
    assert_eq!(events.len(), 1, "search answers once");
    let CatalogEvent::Search(result) = events.remove(0) else {
        panic!("search answers once");
    };
    result
}

fn drive_select(harness: &mut Harness, control: CatalogControl) -> InstallProviderInstrument {
    harness
        .catalog_controls
        .try_send(control)
        .expect("catalog control queue accepts");
    harness.drive();
    let mut events = harness.take_catalog();
    assert_eq!(events.len(), 1, "selection answers once");
    let CatalogEvent::Selection { instrument, .. } = events.remove(0) else {
        panic!("selection resolves");
    };
    instrument
}

fn listed(result: &ProviderInstrumentSearchResult) -> Vec<(&str, &str, &str)> {
    result
        .instruments
        .iter()
        .map(|summary| {
            (
                summary.symbol.as_str(),
                summary.display_symbol.as_str(),
                summary.exchange.as_str(),
            )
        })
        .collect()
}

#[test]
fn search_and_select_cover_demo_and_live_catalogs() {
    let script = catalog_script();
    let mut harness = harness(Arc::clone(&script), Duration::ZERO);

    let result = drive_search(&mut harness, 1, "EUR");
    assert_eq!(result.consumer_id, 7);
    assert_eq!(result.search_generation, 1);
    assert_eq!(
        listed(&result),
        [
            ("demo:1001:2", "EURGBP", "cTrader Demo"),
            ("demo:1001:1", "EURUSD", "cTrader Demo"),
            ("live:2002:1", "EURUSD", "cTrader Live"),
        ]
    );
    assert!(
        harness.requests().iter().any(|request| matches!(
            request,
            Logged::SymbolsList {
                live: true,
                ctid: LIVE_CTID
            }
        )),
        "live account symbols come from the live host"
    );

    // Selecting a live search result resolves it on the live host.
    let instrument = drive_select(&mut harness, select(1, 1, "live:2002:1", ENTITLEMENT));
    assert_eq!(instrument.instrument_id, "ctrader:live:2002:1");
    assert_eq!(instrument.provider_symbol, "live:2002:1");
    assert_eq!(instrument.display_symbol, "EURUSD");
    assert_eq!(instrument.venue_id, "cTrader Live");
    assert_eq!(instrument.price_scale, 5, "the price scale is the digits");
    assert_eq!(instrument.quantity_scale, 2);
    assert_eq!(instrument.entitlement_id, ENTITLEMENT);
    assert_eq!(instrument.price_increment, Some(1));
    assert!(harness.requests().iter().any(|request| matches!(
        request,
        Logged::SymbolById {
            live: true,
            ctid: LIVE_CTID,
            ..
        }
    )));

    // Selections must come from the current search: XAUUSD is not an "EUR"
    // result, so a fresh search for it runs first.
    let result = drive_search(&mut harness, 2, "XAU");
    assert_eq!(
        listed(&result),
        [("live:2002:41", "XAUUSD", "cTrader Live")]
    );

    // The digits of the selected symbol, not a fixed scale.
    let instrument = drive_select(&mut harness, select(2, 2, "live:2002:41", ENTITLEMENT));
    assert_eq!(instrument.instrument_id, "ctrader:live:2002:41");
    assert_eq!(instrument.price_scale, 2);
    // Contract terms come from the symbol and the account's asset list.
    let terms = instrument
        .contract_metadata
        .as_deref()
        .expect("contract terms");
    assert_eq!(terms.currency.as_deref(), Some("USD"));
    assert_eq!(
        (terms.point_value, terms.point_value_scale),
        (Some(1), Some(0))
    );
    assert_eq!(terms.order_quantity_increment, Some(100));
    assert!(
        harness.requests().iter().any(|request| matches!(
            request,
            Logged::Assets {
                live: true,
                ctid: LIVE_CTID
            }
        )),
        "the live account's assets resolve the quote currency"
    );

    // A stale search generation or foreign entitlement rejects, not panics.
    for (search_generation, entitlement) in [(999, ENTITLEMENT), (2, "other-entitlement")] {
        harness
            .catalog_controls
            .try_send(select(3, search_generation, "live:2002:1", entitlement))
            .expect("catalog control queue accepts");
        harness.drive();
        let events = harness.take_catalog();
        assert!(
            matches!(
                events.as_slice(),
                [CatalogEvent::Rejected {
                    selection: true,
                    ..
                }]
            ),
            "stale selection rejects"
        );
    }
}

#[test]
fn live_account_history_is_served_by_the_live_host() {
    let script = catalog_script();
    script_series(&script, LIVE_SYMBOL, TrendbarPeriod::M1, 1, 4);
    let mut harness = harness(Arc::clone(&script), Duration::ZERO);
    // A search opens both hosts and caches the catalogs.
    drive_search(&mut harness, 1, "EUR");
    let before = harness.requests().len();

    let mut gold = instrument(true, LIVE_CTID, LIVE_SYMBOL);
    gold.price_scale = 2;
    let series = series(true, LIVE_CTID, LIVE_SYMBOL, 60);
    harness
        .history
        .try_send(history_request(&series, &gold, 1, 10, None))
        .expect("history queue accepts");
    let served = harness.completion();
    let Command::HistoryCompleted(_, _, _, Ok(snapshot)) = served else {
        panic!("live history is served");
    };
    assert_eq!(snapshot.price_scale, 2);
    assert_eq!(snapshot.bars.len(), 4);
    // Wire 100_000 at digits 2 rescales exactly to 100.
    assert_eq!(snapshot.bars[0].low, 100);
    assert_eq!(snapshot.bars[0].open, 101);
    assert!(
        harness.requests()[before..].iter().all(|request| matches!(
            request,
            Logged::History {
                live: true,
                ctid: LIVE_CTID,
                symbol: LIVE_SYMBOL,
                ..
            }
        )),
        "live history pages come from the live host only"
    );
}

// ---------------------------------------------------------------------------
// VAL-MDATA-007 (worker half): live trendbars publish forming candles.
// ---------------------------------------------------------------------------

#[test]
fn live_trendbar_updates_publish_candles_closed_at_the_bid() {
    let script = scripted();
    let mut harness = harness(script, Duration::ZERO);
    let demo = instrument(false, DEMO_CTID, DEMO_SYMBOL);
    let mut demand = Demand::default();
    demand
        .series
        .push((series(false, DEMO_CTID, DEMO_SYMBOL, 60), demo));
    harness.demand(demand);
    assert_eq!(harness.take_events().len(), 2);

    // The live bar closes at the bid, so the bid must sit inside the bar.
    harness.push_event(
        false,
        spot_frame(
            DEMO_CTID,
            DEMO_SYMBOL,
            Some(102_500),
            None,
            Some((M1, 29_856_835, 0)),
            1_791_410_109_650,
        ),
    );
    harness.drive();
    let events = harness.take_events();
    let quote = events
        .iter()
        .find_map(|event| match event {
            RealtimeEvent::Quote(1, quote) => Some(quote),
            _ => None,
        })
        .expect("the spot publishes a quote");
    assert_eq!(quote.bid.map(|level| level.price), Some(102_500));
    assert_eq!(quote.ask, None);
    assert_eq!(quote.metadata.instrument_id, "ctrader:demo:1001:1");
    assert_eq!(quote.metadata.session_generation, 1);
    assert_eq!(quote.metadata.entitlement_id, ENTITLEMENT);
    let candle = events
        .iter()
        .find_map(|event| match event {
            RealtimeEvent::Candle(1, symbol, bar) => Some((symbol, bar)),
            _ => None,
        })
        .expect("the live trendbar publishes a candle");
    assert_eq!(candle.0, "demo:1001:1{=m1}");
    assert_eq!(candle.1.open, 101_000);
    assert_eq!(candle.1.high, 103_000);
    assert_eq!(candle.1.low, 100_000);
    assert_eq!(candle.1.close, 102_500, "live bars close at the bid");
    assert_eq!(candle.1.volume, 10);
    assert_eq!(candle.1.exchange_timestamp_seconds, 29_856_835 * 60);
}

// ---------------------------------------------------------------------------
// VAL-M1CROSS-005: crossed books are withheld and counted, never fatal.
// ---------------------------------------------------------------------------

#[test]
fn crossed_spots_and_depth_are_counted_not_published() {
    let script = scripted();
    let mut harness = harness(script, Duration::ZERO);
    let demo = instrument(false, DEMO_CTID, DEMO_SYMBOL);
    let mut demand = Demand::default();
    demand.instruments.push((demo, true));
    harness.demand(demand);
    assert_eq!(harness.take_events().len(), 2);

    // A crossed spot publishes no quote and tears nothing down.
    harness.push_event(
        false,
        spot_frame(
            DEMO_CTID,
            DEMO_SYMBOL,
            Some(110_000),
            Some(109_000),
            None,
            1,
        ),
    );
    harness.drive();
    assert!(harness.take_events().is_empty());
    assert_eq!(harness.counters.snapshot().crossed_spots, 1);
    assert!(harness.worker.hosts.contains_key(&false));

    // A crossed depth book publishes no snapshot either.
    harness.push_event(
        false,
        depth_frame(
            DEMO_CTID,
            DEMO_SYMBOL,
            &[(1, 100, Some(110_000), None), (2, 100, None, Some(109_000))],
            &[],
        ),
    );
    harness.drive();
    assert!(harness.take_events().is_empty());
    assert_eq!(harness.counters.snapshot().crossed_depth, 1);

    // Uncrossed updates publish normally afterwards: no stuck state.
    harness.push_event(
        false,
        spot_frame(
            DEMO_CTID,
            DEMO_SYMBOL,
            Some(109_000),
            Some(110_000),
            None,
            2,
        ),
    );
    harness.push_event(
        false,
        depth_frame(
            DEMO_CTID,
            DEMO_SYMBOL,
            &[(3, 150, Some(109_000), None), (4, 200, None, Some(110_000))],
            &[1, 2],
        ),
    );
    harness.drive();
    let events = harness.take_events();
    let quote = events
        .iter()
        .find_map(|event| match event {
            RealtimeEvent::Quote(1, quote) => Some(quote),
            _ => None,
        })
        .expect("an uncrossed spot publishes");
    assert_eq!(quote.bid.map(|level| level.price), Some(109_000));
    assert_eq!(quote.ask.map(|level| level.price), Some(110_000));
    let depth = events
        .iter()
        .find_map(|event| match event {
            RealtimeEvent::Depth(1, snapshot) => Some(snapshot),
            _ => None,
        })
        .expect("an uncrossed book publishes");
    assert_eq!(depth.bids.len(), 1);
    assert_eq!(depth.bids[0].price, 109_000);
    assert_eq!(depth.bids[0].quantity, 150);
    assert_eq!(depth.asks[0].price, 110_000);
    let statistics = harness.counters.snapshot();
    assert_eq!(statistics.crossed_spots, 1);
    assert_eq!(statistics.crossed_depth, 1);
}

// ---------------------------------------------------------------------------
// Bounds: the per-host spot subscription limit rejects, never panics.
// ---------------------------------------------------------------------------

#[test]
fn spot_subscriptions_beyond_the_stream_bound_are_rejected_not_fatal() {
    let script = scripted();
    let mut harness = harness(script, Duration::ZERO);
    let mut demand = Demand::default();
    for symbol in 1..=257u64 {
        demand
            .instruments
            .push((instrument(false, DEMO_CTID, symbol), false));
    }
    harness.demand(demand);

    let host = harness.worker.hosts.get(&false).expect("demo host");
    assert_eq!(host.spots.len(), MAXIMUM_STREAM_SYMBOLS);
    for symbol in 1..=256u64 {
        assert!(host.spots.contains(&(DEMO_CTID, symbol)));
    }
    let subscribes = harness
        .requests()
        .iter()
        .filter(|request| {
            matches!(
                request,
                Logged::Spots {
                    subscribe: true,
                    ..
                }
            )
        })
        .count();
    assert_eq!(subscribes, 4, "256 symbols subscribe in 64-symbol requests");
    let generations: Vec<u64> = harness
        .take_events()
        .iter()
        .map(RealtimeEvent::generation)
        .collect();
    assert_eq!(generations, [1, 1], "the session stays up");
}

// ---------------------------------------------------------------------------
// Daily bars are session days, not fixed 24-hour buckets.
// ---------------------------------------------------------------------------

#[test]
fn daily_history_keeps_provider_opens_across_a_daylight_saving_change() {
    // 17:00 New York is 21:00 UTC before 2025-11-02 and 22:00 UTC after.
    let opens = [29_364_300, 29_365_740, 29_368_680, 29_370_120];
    let script = scripted();
    script
        .lock()
        .unwrap()
        .series
        .insert((DEMO_SYMBOL, TrendbarPeriod::D1.wire()), opens.to_vec());
    let instrument = instrument(false, DEMO_CTID, DEMO_SYMBOL);
    let series = BarSeriesKey {
        period: BarPeriod::Session { days: 1 },
        ..series(false, DEMO_CTID, DEMO_SYMBOL, 60)
    };
    let range = HistoryRange {
        start_unix_nanos: (opens[0] - 60) * 60_000_000_000,
        end_unix_nanos: (opens[3] + 60) * 60_000_000_000,
    };
    let (snapshot, _) = run_history(
        &script,
        history_request(&series, &instrument, 1, 10, Some(range)),
    );
    let minutes: Vec<i64> = snapshot
        .bars
        .iter()
        .map(|bar| bar.exchange_timestamp_unix_nanos / 60_000_000_000)
        .collect();
    assert_eq!(minutes, opens);
}

// ---------------------------------------------------------------------------
// Recovery: one reconnect policy, server-directed waits, retry limit.
// ---------------------------------------------------------------------------

fn demo_series_demand() -> Demand {
    let mut demand = Demand::default();
    demand.series.push((
        series(false, DEMO_CTID, DEMO_SYMBOL, 60),
        instrument(false, DEMO_CTID, DEMO_SYMBOL),
    ));
    demand
}

fn retry_after(harness: &Harness) -> Duration {
    harness
        .worker
        .retry_at
        .saturating_duration_since(Instant::now())
}

#[test]
fn server_directed_waits_replace_the_backoff_and_do_not_count_as_failures() {
    let mut harness = harness(scripted(), Duration::ZERO);
    harness.demand(demo_series_demand());
    harness.take_events();

    harness.script.lock().unwrap().fault = Some(SessionFault::Maintenance {
        wait: Duration::from_secs(120),
    });
    harness.drive();
    assert_eq!(harness.worker.failures, 0);
    let wait = retry_after(&harness);
    assert!(
        wait > Duration::from_secs(110) && wait <= Duration::from_secs(120),
        "{wait:?}"
    );
    assert!(
        harness
            .take_events()
            .iter()
            .any(|event| matches!(event, RealtimeEvent::Recovering(1, _)))
    );

    // A connection-limit refusal while opening the host waits the same way.
    harness.worker.retry_at = Instant::now();
    harness.script.lock().unwrap().open_fault = Some(SessionFault::ConnectionLimit {
        message: "cTrader connection limit reached",
        wait: Duration::from_secs(300),
    });
    harness.drive();
    assert_eq!(harness.worker.failures, 0);
    assert!(harness.worker.hosts.is_empty());
    assert!(retry_after(&harness) > Duration::from_secs(290));
    assert!(!harness.worker.epoch_state.paused);
}

#[test]
fn local_failures_back_off_exponentially_and_pause_at_the_retry_limit() {
    let mut harness = harness(scripted(), Duration::ZERO);
    harness.demand(demo_series_demand());
    let mut waits = Vec::new();
    for _ in 0..MAXIMUM_FAILURES {
        harness.worker.retry_at = Instant::now();
        harness.drive();
        harness.script.lock().unwrap().fault = Some(SessionFault::Reconnect);
        harness.drive();
        waits.push(retry_after(&harness).as_secs_f64().ceil());
    }
    assert_eq!(waits, [6.0, 12.0, 24.0, 48.0, 48.0]);
    assert!(harness.worker.epoch_state.paused);
    assert!(
        harness
            .take_events()
            .iter()
            .any(|event| matches!(event, RealtimeEvent::Failed(..))),
        "the retry limit publishes a terminal failure"
    );
}

#[test]
fn a_healthy_session_clears_the_retry_count() {
    let mut harness = harness(scripted(), Duration::ZERO);
    harness.worker.config.healthy_after = Duration::ZERO;
    harness.demand(demo_series_demand());
    harness.script.lock().unwrap().fault = Some(SessionFault::Reconnect);
    harness.drive();
    assert_eq!(harness.worker.failures, 1);

    harness.worker.retry_at = Instant::now();
    harness.drive();
    assert!(harness.worker.epoch_state.open);
    assert_eq!(harness.worker.failures, 0);
}

#[test]
fn shutdown_closes_hosts_without_unsubscribing() {
    let mut harness = harness(scripted(), Duration::ZERO);
    harness.demand(demo_series_demand());
    harness.worker.ports.stop.store(true, Ordering::Release);
    harness
        .controls
        .try_send(RealtimeControl::Stop)
        .expect("control queue accepts");
    harness.drive();
    assert!(harness.worker.hosts.is_empty());
    assert_eq!(harness.script.lock().unwrap().closed, 1);
    assert!(
        !harness.requests().iter().any(|request| matches!(
            request,
            Logged::Spots {
                subscribe: false,
                ..
            } | Logged::Trendbar {
                subscribe: false,
                ..
            }
        )),
        "a cancelled transport is not asked to unsubscribe"
    );
}

// ---------------------------------------------------------------------------
// Catalog: per-account isolation.
// ---------------------------------------------------------------------------

#[test]
fn a_failing_account_catalog_is_skipped_and_the_rest_are_published() {
    let script = catalog_script();
    script.lock().unwrap().failing_lists.insert(DEMO_CTID);
    let mut harness = harness(script, Duration::ZERO);
    let result = drive_search(&mut harness, 1, "EUR");
    assert_eq!(listed(&result), [("live:2002:1", "EURUSD", "cTrader Live")]);
    assert!(
        !harness.worker.epoch_state.paused,
        "a request-scoped catalog failure is not a session failure"
    );
}

#[test]
fn a_search_with_no_loadable_account_is_rejected() {
    let script = catalog_script();
    {
        let mut guard = script.lock().unwrap();
        guard.failing_lists.insert(DEMO_CTID);
        guard.failing_lists.insert(LIVE_CTID);
    }
    let mut harness = harness(script, Duration::ZERO);
    harness
        .catalog_controls
        .try_send(search(7, 1, "EUR"))
        .expect("catalog control queue accepts");
    let mut events = Vec::new();
    for _ in 0..=MAXIMUM_CATALOG_ACCOUNTS {
        harness.drive();
        events.extend(harness.take_catalog());
    }
    assert!(
        matches!(
            events.as_slice(),
            [CatalogEvent::Rejected {
                selection: false,
                ..
            }]
        ),
        "nothing loaded rejects once"
    );
}

#[test]
fn account_list_is_cached_so_searches_do_not_reopen_hosts() {
    let mut harness = harness(catalog_script(), Duration::ZERO);
    drive_search(&mut harness, 1, "EUR");
    // Idle hosts close; the next search reuses the cached account list and
    // catalogs without opening any session.
    harness.drive();
    assert!(harness.worker.hosts.is_empty());
    let opens = harness.opens().len();
    let result = drive_search(&mut harness, 2, "XAU");
    assert_eq!(
        listed(&result),
        [("live:2002:41", "XAUUSD", "cTrader Live")]
    );
    assert_eq!(harness.opens().len(), opens);
}

// ---------------------------------------------------------------------------
// Diagnostics never carry a full trading-account number.
// ---------------------------------------------------------------------------

#[test]
fn logged_instrument_ids_mask_the_trading_account() {
    use super::super::LoggedInstrument;
    assert_eq!(
        LoggedInstrument("ctrader:live:2002:41").to_string(),
        "ctrader:live:#**02:41"
    );
    assert_eq!(
        LoggedInstrument("ctrader:demo:7:1").to_string(),
        "ctrader:demo:#**:1",
        "an account too short to mask keeps no digits"
    );
    assert_eq!(
        LoggedInstrument("rithmic:CME:MNQU6").to_string(),
        "rithmic:CME:MNQU6"
    );
    let error = parse_instrument_id("ctrader:live:2002:x").expect_err("invalid");
    assert!(!error.contains("2002"), "{error}");
}

// ---------------------------------------------------------------------------
// Trading relay (D8): the venue contract over the shared demo session.
// ---------------------------------------------------------------------------

mod venue_relay {
    use super::*;
    use aeris_trading::{
        AccountEnvironment, FixedPoint, OrderSide, OrderType, TimeInForce,
        venue::{
            BrokerOrderKind, BrokerOrderState, ObservedAccount, VenueEvent, VenueOrder,
            VenueRequest, VenueUpdate,
        },
    };

    const GENERATION: u64 = 7;

    struct Venue {
        requests: SyncSender<VenueRequest>,
        events: Arc<Mutex<Vec<VenueEvent>>>,
    }

    impl Venue {
        fn updates(&self) -> Vec<VenueUpdate> {
            let events = std::mem::take(&mut *self.events.lock().unwrap());
            for event in &events {
                assert_eq!(event.session_generation, GENERATION);
                assert_eq!(event.broker_account, DEMO_CTID.to_string());
            }
            events.into_iter().map(|event| event.update).collect()
        }
    }

    fn point(units: i64, scale: u8) -> FixedPoint {
        FixedPoint::try_new(units, scale).expect("fixed point")
    }

    fn trading_script() -> Arc<Mutex<Script>> {
        let script = scripted();
        {
            let mut guard = script.lock().unwrap();
            guard.digits.insert((false, DEMO_CTID, DEMO_SYMBOL), 5);
            guard.trading.push_back(trader_frame());
        }
        script
    }

    /// Attaches a venue and drives the turn that announces the demo account.
    fn attached(script: Arc<Mutex<Script>>) -> (Harness, Venue) {
        let mut harness = harness(script, Duration::ZERO);
        let (requests, receiver) = mpsc::sync_channel(64);
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        harness.venue.put(VenueLink {
            generation: GENERATION,
            requests: receiver,
            events: Box::new(move |event| {
                sink.lock().unwrap().push(event);
                Ok(())
            }),
        });
        harness.drive();
        let venue = Venue { requests, events };
        (harness, venue)
    }

    fn limit_order(client: &str, instrument: &str) -> VenueOrder {
        VenueOrder {
            client_order_id: aeris_trading::ClientOrderId::try_new(client).expect("client id"),
            broker_account: DEMO_CTID.to_string(),
            instrument_id: aeris_instruments::InstrumentId::try_new(instrument).expect("id"),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GoodTillCancelled,
            quantity: point(100_000, 2),
            limit_price: Some(point(108_250, 5)),
            stop_price: None,
            stop_loss: None,
            take_profit: None,
            broker_position_id: None,
        }
    }

    fn trading_requests(harness: &Harness) -> Vec<u32> {
        harness
            .requests()
            .iter()
            .filter_map(|request| match request {
                Logged::Trading {
                    live: false,
                    payload_type,
                    ctid: DEMO_CTID,
                } => Some(*payload_type),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn demo_accounts_are_announced_with_their_currency_and_balance() {
        let (harness, venue) = attached(trading_script());
        assert_eq!(
            venue.updates(),
            [
                VenueUpdate::AccountObserved(ObservedAccount {
                    environment: AccountEnvironment::Demo,
                    display_name: format!("cTrader Demo · cTrader {DEMO_CTID}"),
                    currency: "USD".into(),
                    currency_scale: 2,
                }),
                VenueUpdate::Balance {
                    balance: point(1_000_000, 2)
                },
            ]
        );
        assert_eq!(trading_requests(&harness), [2121]);
        assert!(
            harness.worker.hosts.contains_key(&false),
            "an attached venue keeps the demo session open"
        );
    }

    #[test]
    fn orders_are_relayed_and_their_answers_and_later_events_translated() {
        let (mut harness, venue) = attached(trading_script());
        venue.updates();
        harness
            .script
            .lock()
            .unwrap()
            .trading
            .push_back(execution_frame(
                2,
                Some(order_body(9, "client-1", 2, 1, 0)),
                Some(position_body(3, 0.0)),
                None,
            ));
        venue
            .requests
            .send(VenueRequest::Place(limit_order(
                "client-1",
                "ctrader:demo:1001:1",
            )))
            .expect("queued");
        harness.drive();
        let updates = venue.updates();
        let [VenueUpdate::Order { order, state, .. }] = updates.as_slice() else {
            panic!("one order report, got {updates:?}");
        };
        assert_eq!(*state, BrokerOrderState::Accepted);
        assert_eq!(order.broker_order_id, "9");
        assert_eq!(order.client_order_id.as_deref(), Some("client-1"));
        assert_eq!(order.kind, BrokerOrderKind::Limit);
        assert_eq!(order.limit_price, Some(point(108_250, 5)));
        assert_eq!(order.quantity, point(100_000, 2));
        assert_eq!(trading_requests(&harness), [2121, 2106]);

        // The fill arrives later as an unsolicited event on the demo session.
        harness.script.lock().unwrap().events.push_back((
            false,
            execution_frame(
                3,
                Some(order_body(9, "client-1", 2, 2, 100_000)),
                Some(position_body(1, 1.0825)),
                Some(deal_body(501, 9)),
            ),
        ));
        harness.drive();
        let updates = venue.updates();
        assert!(
            matches!(
                updates.as_slice(),
                [
                    VenueUpdate::Order { state: BrokerOrderState::Filled, .. },
                    VenueUpdate::Fill(fill),
                    VenueUpdate::Position(position),
                ] if fill.broker_deal_id == "501"
                    && fill.price == point(108_250, 5)
                    && fill.quantity == point(100_000, 2)
                    && position.entry_price == Some(point(108_250, 5))
            ),
            "{updates:?}"
        );
    }

    #[test]
    fn live_instruments_and_broker_refusals_come_back_as_refused() {
        let (mut harness, venue) = attached(trading_script());
        venue.updates();
        venue
            .requests
            .send(VenueRequest::Place(limit_order(
                "client-live",
                "ctrader:live:2002:41",
            )))
            .expect("queued");
        harness
            .script
            .lock()
            .unwrap()
            .trading
            .push_back(order_error_frame("TRADING_BAD_VOLUME"));
        venue
            .requests
            .send(VenueRequest::Place(limit_order(
                "client-bad",
                "ctrader:demo:1001:1",
            )))
            .expect("queued");
        harness.drive();
        let updates = venue.updates();
        assert!(
            matches!(
                updates.as_slice(),
                [
                    VenueUpdate::Refused { client_order_id: live, reason: live_reason },
                    VenueUpdate::Refused { client_order_id: bad, reason: bad_reason },
                ] if live.as_str() == "client-live"
                    && live_reason.contains("data-only")
                    && bad.as_str() == "client-bad"
                    && bad_reason == "TRADING_BAD_VOLUME"
            ),
            "{updates:?}"
        );
        assert_eq!(
            trading_requests(&harness),
            [2121, 2106],
            "a live instrument never reaches the wire"
        );
    }

    #[test]
    fn reconcile_replays_deals_before_the_snapshot() {
        let (mut harness, venue) = attached(trading_script());
        venue.updates();
        {
            let mut script = harness.script.lock().unwrap();
            script
                .trading
                .push_back(deal_list_frame(&[deal_body(501, 9)]));
            script.trading.push_back(reconcile_frame(
                &[order_body(10, "client-open", 2, 1, 0)],
                &[position_body(1, 1.0825)],
            ));
        }
        venue
            .requests
            .send(VenueRequest::Reconcile {
                broker_account: DEMO_CTID.to_string(),
                deals_since_unix_nanos: Some(1_791_466_000_000_000_000),
            })
            .expect("queued");
        harness.drive();
        let updates = venue.updates();
        assert!(
            matches!(
                updates.as_slice(),
                [VenueUpdate::Fill(fill), VenueUpdate::Snapshot(snapshot)]
                    if fill.broker_deal_id == "501"
                        && snapshot.orders.len() == 1
                        && snapshot.orders[0].broker_order_id == "10"
                        && snapshot.positions.len() == 1
            ),
            "{updates:?}"
        );
        assert_eq!(trading_requests(&harness), [2121, 2133, 2124]);
    }

    #[test]
    fn an_unopenable_demo_session_refuses_trading_without_disturbing_market_recovery() {
        let script = trading_script();
        script.lock().unwrap().open_fault = Some(SessionFault::NeedsReconnect);
        let (mut harness, venue) = attached(script);
        assert_eq!(venue.updates(), []);
        assert_eq!(harness.worker.failures, 0, "market recovery is not charged");
        venue
            .requests
            .send(VenueRequest::Place(limit_order(
                "client-wait",
                "ctrader:demo:1001:1",
            )))
            .expect("queued");
        harness.drive();
        let updates = venue.updates();
        assert!(
            matches!(
                updates.as_slice(),
                [VenueUpdate::Refused { client_order_id, .. }]
                    if client_order_id.as_str() == "client-wait"
            ),
            "{updates:?}"
        );
        assert_eq!(
            harness.opens().len(),
            1,
            "the backoff holds the next attempt"
        );
        assert_eq!(harness.worker.failures, 0);
    }

    #[test]
    fn a_dropped_session_is_reconciled_with_its_missed_deals_on_reconnect() {
        let (mut harness, venue) = attached(trading_script());
        venue.updates();
        harness.script.lock().unwrap().fault = Some(SessionFault::Reconnect);
        harness.drive();
        {
            let mut script = harness.script.lock().unwrap();
            script.trading.push_back(deal_list_frame(&[]));
            script.trading.push_back(reconcile_frame(&[], &[]));
        }
        harness.worker.retry_at = Instant::now();
        harness.drive();
        assert!(matches!(
            venue.updates().as_slice(),
            [VenueUpdate::Snapshot(snapshot)] if snapshot.orders.is_empty()
        ));
        assert_eq!(trading_requests(&harness), [2121, 2133, 2124]);
    }
}
