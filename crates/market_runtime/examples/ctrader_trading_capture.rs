//! Demo-only capture of cTrader trading responses, for sanitized adapter fixtures.
//!
//! Places minimum-volume EURUSD orders on the stored connection's demo account, records
//! the field shape and decoded form of every response, then cancels and closes everything
//! it opened. Never uses the live host and never starts browser authorization. Account
//! ids, logins and balances are masked; the evidence file stays in `.cache/evidence`.
use aeris_ctrader_open_api_adapter::{
    ProtoMessage,
    accounts::DemoAccount,
    host::CtraderHost,
    hosted::{CtraderHostedAccess, load_stored_connection},
    market::{
        MarketRequest, PriceScale, SymbolSpec, TrendbarPeriod, decode_symbol_by_id,
        decode_trendbar_page,
    },
    session::CtraderSession,
    trading::{
        ExecutionType, NewOrder, OrderAmendment, OrderPrice, OrderType, Protection, TimeInForce,
        TradeSide, TradingRequest, decode_deal_page, decode_execution_event, decode_order_details,
        decode_order_error_event, decode_position_unrealized_pnl, decode_reconcile, decode_trader,
    },
    transport::Bucket,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    process::ExitCode,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Events for one action stop after this long without a frame.
const QUIET: Duration = Duration::from_millis(1500);
const ACTION_LIMIT: Duration = Duration::from_secs(6);

/// Message-typed fields per payload type, so the shape walker descends only into them.
fn nested_paths(payload_type: u32) -> &'static [&'static [u32]] {
    match payload_type {
        2126 => &[&[4], &[4, 2], &[5], &[5, 2], &[6], &[6, 16]],
        2125 => &[&[3], &[3, 2], &[4], &[4, 2]],
        2122 | 2123 | 2188 => &[&[3]],
        2182 => &[&[3], &[3, 2], &[4], &[4, 16]],
        2134 | 2180 => &[&[3], &[3, 16]],
        2176 => &[&[3], &[3, 2]],
        _ => &[],
    }
}

fn varint(bytes: &mut &[u8]) -> Option<u64> {
    let mut value = 0_u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = bytes.split_first()?;
        *bytes = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// The field tags present at each nested path of one message; values are never kept.
fn shape(
    mut bytes: &[u8],
    path: &mut Vec<u32>,
    nested: &[&[u32]],
    out: &mut BTreeMap<String, BTreeSet<u32>>,
) {
    let key = format!("{path:?}");
    while !bytes.is_empty() {
        let Some(header) = varint(&mut bytes) else {
            return;
        };
        let Ok(tag) = u32::try_from(header >> 3) else {
            return;
        };
        out.entry(key.clone()).or_default().insert(tag);
        match header & 7 {
            0 => {
                if varint(&mut bytes).is_none() {
                    return;
                }
            }
            1 => bytes = bytes.get(8..).unwrap_or_default(),
            5 => bytes = bytes.get(4..).unwrap_or_default(),
            2 => {
                let Some(length) = varint(&mut bytes).and_then(|n| usize::try_from(n).ok()) else {
                    return;
                };
                let Some(body) = bytes.get(..length) else {
                    return;
                };
                bytes = &bytes[length..];
                path.push(tag);
                if nested.contains(&path.as_slice()) {
                    shape(body, path, nested, out);
                }
                path.pop();
            }
            _ => return,
        }
    }
}

/// UTF-8 text of the listed top-level length-delimited fields.
fn top_level_text(mut bytes: &[u8], tags: &[u32]) -> Vec<(u32, String)> {
    let mut found = Vec::new();
    while !bytes.is_empty() {
        let Some(header) = varint(&mut bytes) else {
            break;
        };
        let Ok(tag) = u32::try_from(header >> 3) else {
            break;
        };
        match header & 7 {
            0 => {
                if varint(&mut bytes).is_none() {
                    break;
                }
            }
            1 => bytes = bytes.get(8..).unwrap_or_default(),
            5 => bytes = bytes.get(4..).unwrap_or_default(),
            2 => {
                let Some(length) = varint(&mut bytes).and_then(|n| usize::try_from(n).ok()) else {
                    break;
                };
                let Some(body) = bytes.get(..length) else {
                    break;
                };
                bytes = &bytes[length..];
                if tags.contains(&tag) {
                    found.push((tag, String::from_utf8_lossy(body).into_owned()));
                }
            }
            _ => break,
        }
    }
    found
}

struct Capture<'a> {
    session: &'a mut CtraderSession,
    ctid: u64,
    scale: PriceScale,
    symbol_id: u64,
    records: Vec<Value>,
}

impl Capture<'_> {
    fn scales(&self) -> impl Fn(u64) -> Option<PriceScale> + use<> {
        let (symbol_id, scale) = (self.symbol_id, self.scale);
        move |symbol| (symbol == symbol_id).then_some(scale)
    }

    /// Decoded form of one frame with account values masked.
    fn decoded(&self, frame: &ProtoMessage) -> String {
        let scales = self.scales();
        let text = match frame.payload_type {
            2126 => format!("{:?}", decode_execution_event(frame, self.ctid, &scales)),
            2132 => format!("{:?}", decode_order_error_event(frame, self.ctid)),
            2125 => format!("{:?}", decode_reconcile(frame, self.ctid, &scales)),
            2122 | 2123 => match decode_trader(frame, self.ctid) {
                Ok(trader) => format!(
                    "Ok(account_type={:?} money_digits={} deposit_asset={} limited_risk={})",
                    trader.account_type,
                    trader.balance.digits,
                    trader.deposit_asset_id,
                    trader.limited_risk
                ),
                Err(error) => format!("Err({error})"),
            },
            2182 => format!("{:?}", decode_order_details(frame, self.ctid, &scales)),
            2134 | 2180 => format!("{:?}", decode_deal_page(frame, self.ctid, &scales)),
            2188 => format!("{:?}", decode_position_unrealized_pnl(frame, self.ctid)),
            // ProtoOAErrorRes: errorCode (3) and description (4).
            2142 => format!(
                "ErrorRes {:?}",
                top_level_text(frame.payload.as_deref().unwrap_or_default(), &[3, 4])
            ),
            other => format!("undecoded payload type {other}"),
        };
        text.replace(&self.ctid.to_string(), "#masked")
    }

    fn record(&mut self, action: &str, frame: &ProtoMessage, correlated: bool) -> Value {
        let mut shapes = BTreeMap::new();
        shape(
            frame.payload.as_deref().unwrap_or_default(),
            &mut Vec::new(),
            nested_paths(frame.payload_type),
            &mut shapes,
        );
        let decoded = self.decoded(frame);
        println!(
            "  {action}: {} correlated={correlated} {decoded}",
            frame.payload_type
        );
        let value = json!({
            "action": action,
            "payload_type": frame.payload_type,
            "correlated_with_request": correlated,
            "has_client_msg_id": frame.client_msg_id.is_some(),
            "field_tags": shapes,
            "decoded": decoded,
        });
        self.records.push(value.clone());
        value
    }

    /// Send one request and record every frame that follows it, raw from the event loop,
    /// so an answer of an unexpected type is captured rather than reported as a fault.
    fn act(&mut self, action: &str, request: TradingRequest) -> Result<Vec<ProtoMessage>, String> {
        println!("{action}");
        let id = self
            .session
            .send_request(
                request.payload_type,
                request.payload,
                request.response_type,
                request.bucket,
                REQUEST_TIMEOUT,
            )
            .map_err(|error| format!("{action}: {error}"))?;
        let mut frames = Vec::new();
        let started = Instant::now();
        let mut last = Instant::now();
        while last.elapsed() < QUIET && started.elapsed() < ACTION_LIMIT {
            match self.session.next_event(Duration::from_millis(250)) {
                Ok(Some(frame)) if matches!(frame.payload_type, 2131 | 2155) => {}
                Ok(Some(frame)) => {
                    let correlated = frame.client_msg_id.as_deref() == Some(id.as_str());
                    self.record(action, &frame, correlated);
                    frames.push(frame);
                    last = Instant::now();
                }
                Ok(None) => {}
                Err(error) => return Err(format!("{action}: session event failed: {error}")),
            }
        }
        Ok(frames)
    }

    fn execution_ids(&self, frames: &[ProtoMessage]) -> (Option<u64>, Option<u64>, bool) {
        let scales = self.scales();
        let mut order = None;
        let mut position = None;
        let mut filled = false;
        for frame in frames.iter().filter(|frame| frame.payload_type == 2126) {
            if let Ok(event) = decode_execution_event(frame, self.ctid, &scales) {
                order = event.order.as_ref().map(|order| order.order_id).or(order);
                position = event
                    .position
                    .as_ref()
                    .map(|position| position.position_id)
                    .or(position);
                filled |= event.execution == ExecutionType::Filled;
            }
        }
        (order, position, filled)
    }
}

fn now_ms() -> Result<i64, String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "System clock is invalid")?;
    i64::try_from(elapsed.as_millis()).map_err(|_| "System clock is out of range".into())
}

fn general(request: MarketRequest) -> (u32, Vec<u8>, u32, Bucket) {
    (
        request.payload_type,
        request.payload,
        request.response_type,
        request.bucket,
    )
}

fn symbol_spec(
    session: &mut CtraderSession,
    ctid: u64,
    symbol_id: u64,
) -> Result<SymbolSpec, String> {
    let (kind, payload, expected, bucket) = general(
        MarketRequest::symbol_by_id(ctid, &[symbol_id]).map_err(|error| error.to_string())?,
    );
    let frame = session
        .request(kind, payload, expected, bucket, REQUEST_TIMEOUT)
        .map_err(|error| error.to_string())?;
    decode_symbol_by_id(&frame, ctid)
        .map_err(|error| error.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "EURUSD specification is missing".into())
}

fn last_close(session: &mut CtraderSession, ctid: u64, spec: &SymbolSpec) -> Result<i64, String> {
    let to = now_ms()?;
    let (kind, payload, expected, bucket) = general(
        MarketRequest::trendbars(
            ctid,
            spec.symbol_id,
            TrendbarPeriod::M1,
            to - 3_600_000,
            to,
            Some(10),
        )
        .map_err(|error| error.to_string())?,
    );
    let frame = session
        .request(kind, payload, expected, bucket, REQUEST_TIMEOUT)
        .map_err(|error| error.to_string())?;
    decode_trendbar_page(
        &frame,
        ctid,
        spec.symbol_id,
        TrendbarPeriod::M1,
        spec.price_scale,
        1,
    )
    .map_err(|error| error.to_string())?
    .bars
    .last()
    .map(|bar| bar.bar.close)
    .ok_or_else(|| "no recent EURUSD bar; the market may be closed".into())
}

/// `price` moved by `percent`, on the symbol's tick grid.
fn offset(price: i64, percent: i64, tick: i64) -> i64 {
    let moved = price + price * percent / 100;
    moved - moved % tick
}

#[allow(clippy::too_many_lines)]
fn scenario(
    capture: &mut Capture<'_>,
    account: &DemoAccount,
    spec: &SymbolSpec,
) -> Result<(), String> {
    let close = last_close(capture.session, capture.ctid, spec)?;
    let tick = spec.tick_units();
    let volume = u64::try_from(spec.min_volume).map_err(|_| "invalid minimum volume")?;
    let scale = spec.price_scale;
    let error =
        |error: aeris_ctrader_open_api_adapter::market::MarketDecodeError| error.to_string();
    let order = |client: &str, side, order_type, time_in_force, stop_loss, take_profit| NewOrder {
        symbol_id: spec.symbol_id,
        side,
        order_type,
        volume,
        time_in_force,
        stop_loss,
        take_profit,
        client_order_id: client.into(),
        position_id: None,
    };

    capture.act(
        "trader",
        TradingRequest::trader(capture.ctid).map_err(error)?,
    )?;
    capture.act(
        "reconcile before",
        TradingRequest::reconcile(capture.ctid).map_err(error)?,
    )?;

    // Pending limit order: accept, amend, details, cancel.
    let limit = order(
        "aeris-capture-limit",
        TradeSide::Buy,
        OrderType::Limit {
            price: offset(close, -3, tick),
        },
        TimeInForce::GoodTillCancel,
        Some(Protection::Distance(50 * spec.pip_units())),
        None,
    );
    let frames = capture.act(
        "limit place",
        TradingRequest::new_order(account, scale, &limit).map_err(error)?,
    )?;
    let (limit_id, _, _) = capture.execution_ids(&frames);
    if let Some(order_id) = limit_id {
        let amendment = OrderAmendment {
            order_id,
            price: Some(OrderPrice::Limit(offset(close, -4, tick))),
            ..OrderAmendment::default()
        };
        capture.act(
            "limit amend price",
            TradingRequest::amend_order(account, scale, &amendment).map_err(error)?,
        )?;
        capture.act(
            "limit details",
            TradingRequest::order_details(capture.ctid, order_id).map_err(error)?,
        )?;
        capture.act(
            "limit cancel",
            TradingRequest::cancel_order(account, order_id).map_err(error)?,
        )?;
    }

    // Pending stop order: accept, cancel.
    let stop = order(
        "aeris-capture-stop",
        TradeSide::Buy,
        OrderType::Stop {
            price: offset(close, 3, tick),
        },
        TimeInForce::GoodTillCancel,
        None,
        None,
    );
    let frames = capture.act(
        "stop place",
        TradingRequest::new_order(account, scale, &stop).map_err(error)?,
    )?;
    if let (Some(order_id), _, _) = capture.execution_ids(&frames) {
        capture.act(
            "stop cancel",
            TradingRequest::cancel_order(account, order_id).map_err(error)?,
        )?;
    }

    // Market order: which time in force fills, with relative protection.
    let mut position = None;
    for (label, time_in_force) in [
        ("market ioc", TimeInForce::ImmediateOrCancel),
        ("market gtc", TimeInForce::GoodTillCancel),
    ] {
        let market = order(
            &format!("aeris-capture-{}", label.replace(' ', "-")),
            TradeSide::Buy,
            OrderType::Market,
            time_in_force,
            Some(Protection::Distance(50 * spec.pip_units())),
            Some(Protection::Distance(50 * spec.pip_units())),
        );
        let frames = capture.act(
            label,
            TradingRequest::new_order(account, scale, &market).map_err(error)?,
        )?;
        if let (_, Some(position_id), true) = capture.execution_ids(&frames) {
            position = Some(position_id);
            break;
        }
    }

    if let Some(position_id) = position {
        // An amend that names only a stop loss shows whether take profit is kept.
        capture.act(
            "position amend stop loss only",
            TradingRequest::amend_position_protection(
                account,
                scale,
                position_id,
                Some(offset(close, -1, tick)),
                None,
            )
            .map_err(error)?,
        )?;
        capture.act(
            "reconcile open",
            TradingRequest::reconcile(capture.ctid).map_err(error)?,
        )?;
        capture.act(
            "position unrealized pnl",
            TradingRequest::position_unrealized_pnl(capture.ctid).map_err(error)?,
        )?;
        let now = now_ms()?;
        capture.act(
            "position deals",
            TradingRequest::position_deals(capture.ctid, position_id, now - 3_600_000, now)
                .map_err(error)?,
        )?;
        capture.act(
            "position close",
            TradingRequest::close_position(account, position_id, volume).map_err(error)?,
        )?;
    }

    // A volume below the symbol minimum is rejected.
    let rejected = NewOrder {
        volume: 1,
        ..order(
            "aeris-capture-bad-volume",
            TradeSide::Buy,
            OrderType::Market,
            TimeInForce::ImmediateOrCancel,
            None,
            None,
        )
    };
    capture.act(
        "market bad volume",
        TradingRequest::new_order(account, scale, &rejected).map_err(error)?,
    )?;

    let now = now_ms()?;
    capture.act(
        "deal list",
        TradingRequest::deal_list(capture.ctid, now - 3_600_000, now + 60_000).map_err(error)?,
    )?;
    capture.act(
        "reconcile after",
        TradingRequest::reconcile(capture.ctid).map_err(error)?,
    )?;
    Ok(())
}

/// Cancel and close anything the capture opened, whatever stopped the scenario.
fn clean_up(capture: &mut Capture<'_>, account: &DemoAccount) -> Result<(), String> {
    let request = TradingRequest::reconcile(capture.ctid).map_err(|error| error.to_string())?;
    let frame = capture
        .session
        .request(
            request.payload_type,
            request.payload,
            request.response_type,
            request.bucket,
            REQUEST_TIMEOUT,
        )
        .map_err(|error| error.to_string())?;
    let state = decode_reconcile(&frame, capture.ctid, &capture.scales())
        .map_err(|error| error.to_string())?;
    for order in state.orders.iter().filter(|order| {
        order
            .client_order_id
            .as_deref()
            .is_some_and(|id| id.starts_with("aeris-capture-"))
    }) {
        capture.act(
            "cleanup cancel",
            TradingRequest::cancel_order(account, order.order_id)
                .map_err(|error| error.to_string())?,
        )?;
    }
    let opened: BTreeSet<_> = capture
        .records
        .iter()
        .filter_map(|record| record["decoded"].as_str())
        .filter_map(|text| text.split("position_id: ").nth(1))
        .filter_map(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .filter_map(|digits| digits.parse::<u64>().ok())
        .collect();
    for position in state
        .positions
        .iter()
        .filter(|position| opened.contains(&position.position_id))
    {
        capture.act(
            "cleanup close",
            TradingRequest::close_position(account, position.position_id, position.volume)
                .map_err(|error| error.to_string())?,
        )?;
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let connection = load_stored_connection()?.ok_or_else(|| {
        "No stored cTrader connection; connect with the human authorization flow".to_string()
    })?;
    let stop = Arc::new(AtomicBool::new(false));
    let mut access = CtraderHostedAccess::new();
    let token = access.access_token(&connection, &stop, false)?;
    let credentials = access.app_credentials(&connection, &stop)?;
    let mut session =
        CtraderSession::open(CtraderHost::Demo, credentials, token, Arc::clone(&stop))
            .map_err(|error| error.to_string())?;
    let account = session
        .accounts()
        .iter()
        .find(|account| !account.is_live)
        .cloned()
        .ok_or("No demo account on this connection")?;
    let demo = DemoAccount::from_session(&session, &account)?;
    session
        .authorize_account(&account, || {
            access
                .access_token(&connection, &stop, true)
                .map_err(|_| aeris_ctrader_open_api_adapter::session::SessionFault::NeedsReconnect)
        })
        .map_err(|error| error.to_string())?;
    let symbol_id = session
        .symbol_catalog(&account)
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|symbol| symbol.name == "EURUSD")
        .map(|symbol| symbol.symbol_id)
        .ok_or("EURUSD is not in the demo catalog")?;
    let spec = symbol_spec(&mut session, account.ctid, symbol_id)?;
    println!(
        "EURUSD digits {} pip {} min volume {} step {}",
        spec.price_scale.digits(),
        spec.pip_position,
        spec.min_volume,
        spec.step_volume
    );
    let mut capture = Capture {
        session: &mut session,
        ctid: account.ctid,
        scale: spec.price_scale,
        symbol_id,
        records: Vec::new(),
    };
    let outcome = scenario(&mut capture, &demo, &spec);
    let cleanup = clean_up(&mut capture, &demo);
    let records = std::mem::take(&mut capture.records);
    session.close();

    let directory = Path::new(".cache/evidence");
    fs::create_dir_all(directory).map_err(|_| "Could not create evidence directory")?;
    let evidence = directory.join(format!("ctrader_trading_capture_{}.json", now_ms()?));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&evidence)
        .map_err(|_| "Could not create capture evidence")?;
    file.write_all(
        &serde_json::to_vec_pretty(&json!({
            "host": "demo",
            "symbol": "EURUSD",
            "digits": spec.price_scale.digits(),
            "min_volume": spec.min_volume,
            "step_volume": spec.step_volume,
            "records": records,
        }))
        .map_err(|_| "Could not encode capture evidence")?,
    )
    .map_err(|_| "Could not write capture evidence")?;
    println!("Evidence: {}", evidence.display());
    outcome.and(cleanup)
}

fn main() -> ExitCode {
    if let Err(error) = run() {
        eprintln!("{error}");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
