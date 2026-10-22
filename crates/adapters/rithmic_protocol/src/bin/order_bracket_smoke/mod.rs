//! Rithmic Test bracket lifecycle for `--order-bracket-smoke`.
//!
//! On a flat Rithmic Test account with no working orders, one market entry is
//! placed with a server-held target and stop. The order session then logs out,
//! and a fresh session must still see both exits working. Every run that got
//! as far as placing ends by flattening the contract, cancelling the account's
//! orders, and checking that the account is flat again.

use super::{SelectedInstrument, application, login_and_select};
use aeris_rithmic_protocol_adapter::{
    AccountListRequest, BracketOrderRequest, CancelAllOrdersRequest, DecodedControlMessage,
    DecodedOrderMessage, DecodedPnlMessage, ExitPositionRequest, NewOrderRequest, RithmicAccount,
    RithmicBracketLevel, RithmicCredentialBytes, RithmicExchangeNotifyType, RithmicGateway,
    RithmicOrderConnection, RithmicOrderDuration, RithmicOrderNotifyType, RithmicOrderPlacement,
    RithmicOrderPlantMessage, RithmicOrderSide, RithmicOrderType, RithmicPnlConnection,
    RithmicPnlPlantMessage, RithmicReportedSide, RithmicRequestKind, RithmicRequestOutcome,
    RithmicSession, RithmicSessionError, RithmicSessionLimits, RithmicUserType, TradeRoutesRequest,
};
use std::{
    collections::BTreeMap,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const RITHMIC_TEST_SYSTEM: &str = "Rithmic Test";
/// Distance of both exits from the fill; wide enough that neither triggers
/// during the run.
const EXIT_TICKS: u32 = 40;
const STEP_TIMEOUT: Duration = Duration::from_secs(30);
const MAXIMUM_ACCOUNTS: usize = 64;
const MAXIMUM_ROUTES: usize = 256;
const MAXIMUM_SNAPSHOT_ROWS: usize = 1_024;
const SETTLE_ATTEMPTS: usize = 5;

struct AccountSetup {
    account: RithmicAccount,
    trade_route: String,
    user_type: RithmicUserType,
}

/// Latest Rithmic-side state of one order seen while reading a snapshot. A
/// snapshot reports an order that reached the exchange by its last exchange
/// notification and any other order by its last order notification.
struct OrderState {
    working: bool,
    original_basket_id: Option<String>,
}

#[derive(Clone, Copy)]
enum SnapshotStart {
    Subscribe,
    Repeat,
}

pub(super) fn run(credentials: &RithmicCredentialBytes, product: &str) -> Result<(), String> {
    let login = credentials
        .credentials()
        .map_err(|_| "credentials_invalid")?;
    if login.gateway != RithmicGateway::Test || login.system_name != RITHMIC_TEST_SYSTEM {
        return Err("rithmic_order_smoke=refused vault_login_is_not_rithmic_test".to_string());
    }
    let (ticker, selected) = login_and_select(credentials, application(), product)?;
    ticker
        .close()
        .map_err(|error| format!("ticker_close_failed={error}"))?;

    let mut orders = connect_orders(credentials)?;
    let prepared = prepare_account(&mut orders, &selected).and_then(|setup| {
        require_flat(credentials, &setup.account)
            .map_err(|error| format!("rithmic_order_smoke=refused {error}"))
            .map(|()| setup)
    });
    let setup = match prepared {
        Ok(setup) => setup,
        Err(error) => return Err(with_close(error, orders)),
    };
    let entry = place_bracket(&mut orders, &setup, &selected);
    let first_close = orders
        .close()
        .map_err(|error| format!("order_close_failed={error}"));
    if first_close.is_ok() {
        println!("rithmic_order_logout=passed");
    }

    let mut fresh = connect_orders(credentials)
        .map_err(|error| format!("{error} rithmic_order_smoke=cleanup_required"))?;
    let held = verify_exits_held(&mut fresh, &setup, entry.as_deref().ok());
    let flattened = flatten(&mut fresh, credentials, &setup, &selected);
    let fresh_close = fresh
        .close()
        .map_err(|error| format!("order_close_failed={error}"));
    let flat = require_flat(credentials, &setup.account);
    combine([
        entry.map(|_| ()),
        first_close,
        held,
        flattened,
        fresh_close,
        flat,
    ])
    .map_err(|failures| format!("rithmic_order_smoke=failed {failures}"))?;
    println!(
        "rithmic_order_smoke=passed symbol={} exchange={}",
        selected.symbol, selected.exchange
    );
    Ok(())
}

fn combine(results: impl IntoIterator<Item = Result<(), String>>) -> Result<(), String> {
    let failures = results
        .into_iter()
        .filter_map(Result::err)
        .collect::<Vec<_>>();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn with_close(error: String, orders: RithmicOrderConnection) -> String {
    match orders.close() {
        Ok(()) => error,
        Err(close) => format!("{error}; order_close_failed={close}"),
    }
}

fn connect_orders(credentials: &RithmicCredentialBytes) -> Result<RithmicOrderConnection, String> {
    let borrowed = credentials
        .credentials()
        .map_err(|_| "credentials_invalid")?;
    let orders = RithmicSession::discover_and_login_orders(
        borrowed,
        application(),
        RithmicSessionLimits::default(),
        None,
    )
    .map_err(|error| format!("order_login_failed={error}"))?;
    println!("rithmic_order_login=passed");
    Ok(orders)
}

/// Reads order-plant payloads until `accept` finishes the step or the step
/// deadline passes.
fn read_orders_until<T>(
    orders: &mut RithmicOrderConnection,
    step: &str,
    mut accept: impl FnMut(DecodedOrderMessage) -> Result<Option<T>, String>,
) -> Result<T, String> {
    let deadline = Instant::now() + STEP_TIMEOUT;
    loop {
        let message = match orders.read_next_until(deadline) {
            Ok(RithmicOrderPlantMessage::Order(message)) => *message,
            Ok(RithmicOrderPlantMessage::Control(DecodedControlMessage::ForcedLogout)) => {
                return Err(format!("{step}_forced_logout"));
            }
            Ok(RithmicOrderPlantMessage::Control(DecodedControlMessage::Reject)) => {
                return Err(format!("{step}_rejected_by_plant"));
            }
            Ok(RithmicOrderPlantMessage::Control(_)) => continue,
            Err(RithmicSessionError::Deadline) => return Err(format!("{step}_timed_out")),
            Err(error) => return Err(format!("{step}_read_failed={error}")),
        };
        println!("rithmic_order_frame step={step} {}", describe(&message));
        if let Some(value) = accept(message)? {
            return Ok(value);
        }
    }
}

/// One-line summary without account or order identifiers.
fn describe(message: &DecodedOrderMessage) -> String {
    match message {
        DecodedOrderMessage::OrderNotification(update) => format!(
            "kind=order notify={:?} price_type={:?} side={:?} status={} exit_leg={} bracket={:?} snapshot={} completion={} text={}",
            update.notify_type,
            update.order.price_type,
            update.order.side,
            update.order.status.as_deref().unwrap_or("-"),
            update.order.original_basket_id.is_some(),
            update.order.bracket_type,
            update.order.is_snapshot,
            update.completion_reason.as_deref().unwrap_or("-"),
            update.order.text.as_deref().unwrap_or("-")
        ),
        DecodedOrderMessage::ExchangeOrderNotification(update) => format!(
            "kind=exchange notify={:?} side={:?} fill_size={} unfilled={:?} exit_leg={} snapshot={} text={}",
            update.notify_type,
            update.order.side,
            update.fill.as_ref().map_or(0, |fill| fill.size),
            update.order.total_unfilled_size,
            update.order.original_basket_id.is_some(),
            update.order.is_snapshot,
            update.order.text.as_deref().unwrap_or("-")
        ),
        DecodedOrderMessage::OrderCommand(reply) => format!(
            "kind=command request={:?} outcome={:?}",
            reply.request, reply.outcome
        ),
        DecodedOrderMessage::RequestComplete(completion) => format!(
            "kind=complete request={:?} outcome={:?}",
            completion.request, completion.outcome
        ),
        DecodedOrderMessage::BracketLevel(row) => format!(
            "kind=bracket_level side={:?} ticks={:?} quantity={:?} released={:?}",
            row.side, row.level.ticks, row.level.quantity, row.level.quantity_released
        ),
        DecodedOrderMessage::BracketUpdate(update) => format!(
            "kind=bracket_update target={:?} stop={:?}",
            update.target, update.stop
        ),
        DecodedOrderMessage::PositionExit(reply) => {
            format!("kind=position_exit outcome={:?}", reply.outcome)
        }
        DecodedOrderMessage::LoginInfo(_) => "kind=login_info".to_string(),
        DecodedOrderMessage::Account(_) => "kind=account".to_string(),
        DecodedOrderMessage::TradeRoute(_) | DecodedOrderMessage::TradeRouteUpdate(_) => {
            "kind=trade_route".to_string()
        }
        DecodedOrderMessage::FillHistory(_) => "kind=fill_history".to_string(),
    }
}

fn finished(outcome: &RithmicRequestOutcome, step: &str) -> Result<Option<()>, String> {
    match outcome {
        RithmicRequestOutcome::Accepted | RithmicRequestOutcome::NoData => Ok(Some(())),
        RithmicRequestOutcome::Rejected { .. } => Err(format!("{step}_rejected={outcome:?}")),
    }
}

fn prepare_account(
    orders: &mut RithmicOrderConnection,
    selected: &SelectedInstrument,
) -> Result<AccountSetup, String> {
    orders
        .request_login_info()
        .map_err(|error| format!("login_info_send_failed={error}"))?;
    let info = read_orders_until(orders, "login_info", |message| match message {
        DecodedOrderMessage::LoginInfo(info) => Ok(Some(info)),
        DecodedOrderMessage::RequestComplete(completion)
            if completion.request == RithmicRequestKind::LoginInfo =>
        {
            Err(format!("login_info_rejected={:?}", completion.outcome))
        }
        _ => Ok(None),
    })?;
    let user_type = info.user_type.ok_or("login_info_user_type_missing")?;
    orders
        .request_accounts(AccountListRequest {
            fcm_id: info.fcm_id.as_deref(),
            ib_id: info.ib_id.as_deref(),
        })
        .map_err(|error| format!("account_list_send_failed={error}"))?;
    let mut accounts = Vec::new();
    read_orders_until(orders, "account_list", |message| match message {
        DecodedOrderMessage::Account(_) if accounts.len() >= MAXIMUM_ACCOUNTS => {
            Err("account_list_exceeds_bound".to_string())
        }
        DecodedOrderMessage::Account(account) => {
            accounts.push(account);
            Ok(None)
        }
        DecodedOrderMessage::RequestComplete(completion)
            if completion.request == RithmicRequestKind::AccountList =>
        {
            finished(&completion.outcome, "account_list")
        }
        _ => Ok(None),
    })?;
    println!("rithmic_account_list=passed accounts={}", accounts.len());
    let account = accounts
        .into_iter()
        .next()
        .ok_or("rithmic_order_smoke=refused no_accounts")?;
    let trade_route = select_trade_route(orders, &account, selected)?;
    let working = order_snapshot(orders, &account, SnapshotStart::Subscribe)?
        .values()
        .filter(|order| order.working)
        .count();
    if working > 0 {
        return Err(format!(
            "rithmic_order_smoke=refused working_orders={working}"
        ));
    }
    Ok(AccountSetup {
        account,
        trade_route,
        user_type,
    })
}

fn select_trade_route(
    orders: &mut RithmicOrderConnection,
    account: &RithmicAccount,
    selected: &SelectedInstrument,
) -> Result<String, String> {
    orders
        .request_trade_routes(TradeRoutesRequest {
            subscribe_for_updates: false,
        })
        .map_err(|error| format!("trade_routes_send_failed={error}"))?;
    let mut routes = Vec::new();
    read_orders_until(orders, "trade_routes", |message| match message {
        DecodedOrderMessage::TradeRoute(_) if routes.len() >= MAXIMUM_ROUTES => {
            Err("trade_routes_exceed_bound".to_string())
        }
        DecodedOrderMessage::TradeRoute(route) => {
            routes.push(route);
            Ok(None)
        }
        DecodedOrderMessage::RequestComplete(completion)
            if completion.request == RithmicRequestKind::TradeRoutes =>
        {
            finished(&completion.outcome, "trade_routes")
        }
        _ => Ok(None),
    })?;
    let route = routes
        .into_iter()
        .filter(|route| {
            route.exchange == selected.exchange
                && route.fcm_id == account.fcm_id
                && route.ib_id == account.ib_id
        })
        .max_by_key(|route| route.is_default == Some(true))
        .ok_or("rithmic_order_smoke=refused no_trade_route")?;
    println!(
        "rithmic_trade_route=passed default={:?} status={}",
        route.is_default,
        route.status.as_deref().unwrap_or("-")
    );
    Ok(route.trade_route)
}

/// Collects the account's orders by basket id, keeping each one's latest state.
fn order_snapshot(
    orders: &mut RithmicOrderConnection,
    account: &RithmicAccount,
    start: SnapshotStart,
) -> Result<BTreeMap<String, OrderState>, String> {
    match start {
        SnapshotStart::Subscribe => {
            orders.start_order_stream(account.key(), Instant::now() + STEP_TIMEOUT)
        }
        SnapshotStart::Repeat => orders.request_order_snapshot(account.key()),
    }
    .map_err(|error| format!("order_snapshot_start_failed={error}"))?;
    let mut states = BTreeMap::new();
    read_orders_until(orders, "order_snapshot", |message| {
        let (order, working) = match message {
            DecodedOrderMessage::OrderNotification(update) => (
                update.order,
                update.notify_type != RithmicOrderNotifyType::Complete,
            ),
            DecodedOrderMessage::ExchangeOrderNotification(update) => {
                let working = match update.notify_type {
                    RithmicExchangeNotifyType::Cancel | RithmicExchangeNotifyType::Reject => false,
                    RithmicExchangeNotifyType::Fill => update.order.total_unfilled_size != Some(0),
                    _ => true,
                };
                (update.order, working)
            }
            DecodedOrderMessage::RequestComplete(completion)
                if completion.request == RithmicRequestKind::ShowOrders =>
            {
                return finished(&completion.outcome, "order_snapshot");
            }
            _ => return Ok(None),
        };
        if states.len() >= MAXIMUM_SNAPSHOT_ROWS && !states.contains_key(&order.basket_id) {
            return Err("order_snapshot_exceeds_bound".to_string());
        }
        states.insert(
            order.basket_id,
            OrderState {
                working,
                original_basket_id: order.original_basket_id,
            },
        );
        Ok(None)
    })?;
    Ok(states)
}

/// Places the market entry with one target and one stop, then waits for the
/// fill and for both exits to rest. Returns the entry's basket id.
fn place_bracket(
    orders: &mut RithmicOrderConnection,
    setup: &AccountSetup,
    selected: &SelectedInstrument,
) -> Result<String, String> {
    let user_tag = format!(
        "aeris-smoke-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "clock_invalid")?
            .as_secs()
    );
    let exit = [RithmicBracketLevel {
        quantity: 1,
        ticks: EXIT_TICKS,
    }];
    orders
        .submit_bracket_order(BracketOrderRequest {
            entry: NewOrderRequest {
                account: setup.account.key(),
                user_tag: &user_tag,
                symbol: &selected.symbol,
                exchange: &selected.exchange,
                trade_route: &setup.trade_route,
                side: RithmicOrderSide::Buy,
                quantity: 1,
                duration: RithmicOrderDuration::Day,
                order_type: RithmicOrderType::Market,
                placement: RithmicOrderPlacement::Manual,
            },
            user_type: setup.user_type,
            targets: &exit,
            stops: &exit,
        })
        .map_err(|error| format!("bracket_send_failed={error}"))?;
    let entry = read_orders_until(orders, "bracket_acknowledgement", |message| match message {
        DecodedOrderMessage::OrderCommand(reply)
            if reply.request == RithmicRequestKind::BracketOrder =>
        {
            if reply.outcome.is_accepted() {
                Ok(reply.basket_id)
            } else {
                Err(format!("bracket_rejected={:?}", reply.outcome))
            }
        }
        _ => Ok(None),
    })?;
    println!("rithmic_bracket_submit=passed");
    let mut filled = false;
    let mut exits = BTreeMap::new();
    read_orders_until(orders, "bracket_fill", |message| {
        match message {
            DecodedOrderMessage::ExchangeOrderNotification(update)
                if update.order.basket_id == entry =>
            {
                match update.notify_type {
                    RithmicExchangeNotifyType::Fill => filled = true,
                    RithmicExchangeNotifyType::Reject => {
                        return Err("entry_rejected_by_exchange".to_string());
                    }
                    _ => {}
                }
            }
            DecodedOrderMessage::OrderNotification(update)
                if update.order.basket_id == entry
                    && update.notify_type == RithmicOrderNotifyType::Complete
                    && !filled
                    && update.order.total_fill_size.unwrap_or(0) == 0 =>
            {
                return Err(format!(
                    "entry_completed_unfilled reason={} text={}",
                    update.completion_reason.as_deref().unwrap_or("-"),
                    update.order.text.as_deref().unwrap_or("-")
                ));
            }
            DecodedOrderMessage::OrderNotification(update)
                if update.order.basket_id != entry
                    && update.order.original_basket_id.as_deref() == Some(entry.as_str()) =>
            {
                exits.insert(update.order.basket_id, update.notify_type);
            }
            _ => {}
        }
        let resting = exits
            .values()
            .filter(|notify_type| {
                matches!(
                    notify_type,
                    RithmicOrderNotifyType::Open | RithmicOrderNotifyType::TriggerPending
                )
            })
            .count();
        Ok((filled && resting == 2).then_some(()))
    })?;
    println!("rithmic_bracket_fill=passed exits_resting=2");
    Ok(entry)
}

/// Starts the fresh session's order stream and, when an entry was placed,
/// requires both of its exits to still be working after the earlier logout.
fn verify_exits_held(
    fresh: &mut RithmicOrderConnection,
    setup: &AccountSetup,
    entry: Option<&str>,
) -> Result<(), String> {
    let snapshot = order_snapshot(fresh, &setup.account, SnapshotStart::Subscribe)?;
    let Some(entry) = entry else {
        return Ok(());
    };
    let held = snapshot
        .iter()
        .filter(|(basket_id, order)| {
            basket_id.as_str() != entry
                && order.original_basket_id.as_deref() == Some(entry)
                && order.working
        })
        .count();
    if held != 2 {
        return Err(format!("exits_not_held_after_logout held={held}"));
    }
    println!("rithmic_exits_held_after_logout=passed held={held}");
    fresh
        .start_bracket_stream(setup.account.key(), Instant::now() + STEP_TIMEOUT)
        .map_err(|error| format!("bracket_stream_start_failed={error}"))?;
    let mut rows = 0_usize;
    let mut outstanding = 2_usize;
    read_orders_until(fresh, "bracket_snapshot", |message| {
        match message {
            DecodedOrderMessage::BracketLevel(row) if row.basket_id == entry => rows += 1,
            DecodedOrderMessage::RequestComplete(completion)
                if matches!(
                    completion.request,
                    RithmicRequestKind::ShowBrackets | RithmicRequestKind::ShowBracketStops
                ) =>
            {
                finished(&completion.outcome, "bracket_snapshot")?;
                outstanding -= 1;
            }
            _ => {}
        }
        Ok((outstanding == 0).then_some(()))
    })?;
    println!("rithmic_bracket_snapshot=passed rows={rows}");
    Ok(())
}

/// Cancels every order of the account, waits until none is working, then
/// exits the contract when the P&L plant reports a position. Each step runs
/// even when an earlier one failed.
fn flatten(
    fresh: &mut RithmicOrderConnection,
    credentials: &RithmicCredentialBytes,
    setup: &AccountSetup,
    selected: &SelectedInstrument,
) -> Result<(), String> {
    let cancelled = cancel_all(fresh, setup);
    let cleared = wait_until_cleared(fresh, setup);
    let exit = match open_contracts(credentials, &setup.account) {
        Ok(0) => {
            println!("rithmic_exit_position=skipped_flat");
            Ok(())
        }
        Ok(_) => exit_position(fresh, setup, selected),
        Err(error) => combine([Err(error), exit_position(fresh, setup, selected)]),
    };
    combine([cancelled, cleared, exit])
}

fn cancel_all(fresh: &mut RithmicOrderConnection, setup: &AccountSetup) -> Result<(), String> {
    fresh
        .cancel_all_orders(CancelAllOrdersRequest {
            account: setup.account.key(),
            user_type: setup.user_type,
            placement: RithmicOrderPlacement::Manual,
        })
        .map_err(|error| format!("cancel_all_send_failed={error}"))
        .and_then(|()| {
            read_orders_until(fresh, "cancel_all", |message| match message {
                DecodedOrderMessage::RequestComplete(completion)
                    if completion.request == RithmicRequestKind::CancelAllOrders =>
                {
                    finished(&completion.outcome, "cancel_all")
                }
                _ => Ok(None),
            })
        })
}

/// Sends the exit and waits for its reply and, when accepted, its fill.
fn exit_position(
    fresh: &mut RithmicOrderConnection,
    setup: &AccountSetup,
    selected: &SelectedInstrument,
) -> Result<(), String> {
    fresh
        .exit_position(ExitPositionRequest {
            account: setup.account.key(),
            symbol: &selected.symbol,
            exchange: &selected.exchange,
            placement: RithmicOrderPlacement::Manual,
        })
        .map_err(|error| format!("exit_send_failed={error}"))?;
    let mut outcome = None;
    let mut filled = false;
    read_orders_until(fresh, "exit_position", |message| {
        match message {
            DecodedOrderMessage::PositionExit(reply) => outcome = Some(reply.outcome),
            DecodedOrderMessage::RequestComplete(completion)
                if completion.request == RithmicRequestKind::ExitPosition && outcome.is_none() =>
            {
                outcome = Some(completion.outcome);
            }
            DecodedOrderMessage::ExchangeOrderNotification(update)
                if update.notify_type == RithmicExchangeNotifyType::Fill
                    && update.order.side == Some(RithmicReportedSide::Sell)
                    && update.order.symbol == selected.symbol =>
            {
                filled = true;
            }
            _ => {}
        }
        match &outcome {
            Some(RithmicRequestOutcome::Accepted) => Ok(filled.then_some(())),
            Some(RithmicRequestOutcome::NoData) => Ok(Some(())),
            Some(rejected) => Err(format!("exit_position_rejected={rejected:?}")),
            None => Ok(None),
        }
    })?;
    println!("rithmic_exit_position outcome={outcome:?} filled={filled}");
    Ok(())
}

fn wait_until_cleared(
    fresh: &mut RithmicOrderConnection,
    setup: &AccountSetup,
) -> Result<(), String> {
    for attempt in 1..=SETTLE_ATTEMPTS {
        let working = order_snapshot(fresh, &setup.account, SnapshotStart::Repeat)?
            .values()
            .filter(|order| order.working)
            .count();
        if working == 0 {
            println!("rithmic_orders_cleared=passed");
            return Ok(());
        }
        if attempt < SETTLE_ATTEMPTS {
            thread::sleep(Duration::from_secs(1));
        }
    }
    Err("orders_still_working_after_cancel_all".to_string())
}

fn require_flat(
    credentials: &RithmicCredentialBytes,
    account: &RithmicAccount,
) -> Result<(), String> {
    match open_contracts(credentials, account)? {
        0 => Ok(()),
        open => Err(format!("position_not_flat open_contracts={open}")),
    }
}

/// Sums the net position of every instrument row in the account's P&L
/// snapshot; a row without a net quantity is an error, not flat.
fn open_contracts(
    credentials: &RithmicCredentialBytes,
    account: &RithmicAccount,
) -> Result<u64, String> {
    let borrowed = credentials
        .credentials()
        .map_err(|_| "credentials_invalid")?;
    let mut pnl = RithmicSession::discover_and_login_pnl(
        borrowed,
        application(),
        RithmicSessionLimits::default(),
        None,
    )
    .map_err(|error| format!("pnl_login_failed={error}"))?;
    let positions = read_positions(&mut pnl, account);
    let closed = pnl
        .close()
        .map_err(|error| format!("pnl_close_failed={error}"));
    let positions = positions?;
    closed?;
    let open = positions
        .values()
        .flatten()
        .map(|net| u64::from(net.unsigned_abs()))
        .sum::<u64>();
    let unknown = positions.values().filter(|net| net.is_none()).count();
    println!(
        "rithmic_position_check instruments={} open_contracts={open} unknown={unknown}",
        positions.len()
    );
    if unknown == 0 {
        Ok(open)
    } else {
        Err(format!("position_unknown instruments={unknown}"))
    }
}

fn read_positions(
    pnl: &mut RithmicPnlConnection,
    account: &RithmicAccount,
) -> Result<BTreeMap<(String, String), Option<i32>>, String> {
    pnl.start_pnl_stream(account.key(), Instant::now() + STEP_TIMEOUT)
        .map_err(|error| format!("pnl_stream_start_failed={error}"))?;
    let deadline = Instant::now() + STEP_TIMEOUT;
    let mut positions = BTreeMap::new();
    loop {
        let message = match pnl.read_next_until(deadline) {
            Ok(RithmicPnlPlantMessage::Pnl(message)) => *message,
            Ok(RithmicPnlPlantMessage::Control(DecodedControlMessage::ForcedLogout)) => {
                return Err("pnl_forced_logout".to_string());
            }
            Ok(RithmicPnlPlantMessage::Control(_)) => continue,
            Err(error) => return Err(format!("pnl_snapshot_read_failed={error}")),
        };
        match message {
            DecodedPnlMessage::Instrument(row) => {
                let key = (row.symbol, row.exchange);
                if positions.len() >= MAXIMUM_SNAPSHOT_ROWS && !positions.contains_key(&key) {
                    return Err("pnl_snapshot_exceeds_bound".to_string());
                }
                positions.insert(key, row.quantities.net_quantity);
            }
            DecodedPnlMessage::RequestComplete {
                request: RithmicRequestKind::PnlSnapshot,
                outcome,
            } => {
                return finished(&outcome, "pnl_snapshot").map(|_| positions);
            }
            DecodedPnlMessage::Account(_) | DecodedPnlMessage::RequestComplete { .. } => {}
        }
    }
}
