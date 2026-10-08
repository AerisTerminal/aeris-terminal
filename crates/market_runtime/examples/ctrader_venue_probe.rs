//! Demo-only end-to-end check of the cTrader trading relay (D8) through the real market
//! service: the relay announces the demo account, reconciles it, and carries one far
//! limit order and its cancel. The trading owner is replaced by a printing sink. Never
//! uses the live host; the order rests 5% below the market and is cancelled at once.
use aeris_instruments::InstrumentId;
use aeris_market_runtime::{CtraderVenueLink, MarketService};
use aeris_trading::{
    ClientOrderId, FixedPoint, OrderSide, OrderType, TimeInForce,
    venue::{BrokerOrderState, VenueEvent, VenueOrder, VenueRequest, VenueUpdate},
};
use std::{
    process::ExitCode,
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const WAIT: Duration = Duration::from_secs(30);

fn wait_for(
    events: &mpsc::Receiver<VenueEvent>,
    what: &str,
    mut matches: impl FnMut(&VenueUpdate) -> bool,
) -> Result<VenueEvent, String> {
    let deadline = Instant::now() + WAIT;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let Ok(event) = events.recv_timeout(remaining) else {
            break;
        };
        let summary = match &event.update {
            VenueUpdate::AccountObserved(account) => format!(
                "account observed: {:?} {} scale {}",
                account.environment, account.currency, account.currency_scale
            ),
            VenueUpdate::Balance { balance } => format!("balance at scale {}", balance.scale()),
            VenueUpdate::Snapshot(snapshot) => format!(
                "snapshot: {} orders, {} positions",
                snapshot.orders.len(),
                snapshot.positions.len()
            ),
            VenueUpdate::Order { order, state, .. } => {
                format!(
                    "order {:?}: kind {:?} limit {:?}",
                    state, order.kind, order.limit_price
                )
            }
            VenueUpdate::Refused { reason, .. } => format!("refused: {reason}"),
            other => format!("{other:?}"),
        };
        println!("  generation {} · {summary}", event.session_generation);
        if matches(&event.update) {
            return Ok(event);
        }
    }
    Err(format!("timed out waiting for {what}"))
}

fn run() -> Result<(), String> {
    let market = MarketService::start()?;
    let (requests, receiver) = mpsc::sync_channel(64);
    let (events_tx, events) = mpsc::sync_channel(1024);
    market.attach_ctrader_venue(CtraderVenueLink {
        generation: 1,
        requests: receiver,
        events: Box::new(move |event| events_tx.send(event).map_err(|error| error.to_string())),
    });
    println!("waiting for the demo account announcement");
    let observed = wait_for(&events, "an announced demo account", |update| {
        matches!(update, VenueUpdate::AccountObserved(_))
    })?;
    let account = observed.broker_account.clone();

    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock")?
        .as_nanos();
    let now_nanos = i64::try_from(now_nanos).map_err(|_| "clock")?;
    requests
        .send(VenueRequest::Reconcile {
            broker_account: account.clone(),
            deals_since_unix_nanos: Some(now_nanos - 3_600_000_000_000),
        })
        .map_err(|error| error.to_string())?;
    println!("reconcile");
    wait_for(&events, "a snapshot", |update| {
        matches!(update, VenueUpdate::Snapshot(_))
    })?;

    // EURUSD is symbol 1 on the maintainer's demo broker; 0.80000 is far below the market.
    let instrument = InstrumentId::try_new(format!("ctrader:demo:{account}:1"))
        .map_err(|error| error.to_string())?;
    let client_order_id =
        ClientOrderId::try_new(format!("aeris-venue-probe-{}", now_nanos / 1_000_000_000))
            .map_err(|error| error.to_string())?;
    requests
        .send(VenueRequest::Place(VenueOrder {
            client_order_id: client_order_id.clone(),
            broker_account: account.clone(),
            instrument_id: instrument.clone(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GoodTillCancelled,
            quantity: FixedPoint::try_new(100_000, 2).map_err(|error| error.to_string())?,
            limit_price: Some(FixedPoint::try_new(80_000, 5).map_err(|error| error.to_string())?),
            stop_price: None,
            stop_loss: None,
            take_profit: None,
            broker_position_id: None,
        }))
        .map_err(|error| error.to_string())?;
    println!("place a far limit order");
    let accepted = wait_for(&events, "the order acceptance", |update| {
        matches!(
            update,
            VenueUpdate::Order {
                state: BrokerOrderState::Accepted,
                ..
            } | VenueUpdate::Refused { .. }
        )
    })?;
    let VenueUpdate::Order { order, .. } = accepted.update else {
        return Err("the demo broker refused the probe order".into());
    };
    requests
        .send(VenueRequest::Cancel {
            client_order_id,
            broker_account: account,
            broker_order_id: order.broker_order_id,
        })
        .map_err(|error| error.to_string())?;
    println!("cancel it");
    wait_for(&events, "the cancellation", |update| {
        matches!(
            update,
            VenueUpdate::Order {
                state: BrokerOrderState::Cancelled,
                ..
            }
        )
    })?;
    market.shutdown(Duration::from_secs(10))?;
    println!("relay verified on demo");
    Ok(())
}

fn main() -> ExitCode {
    if let Err(error) = run() {
        eprintln!("{error}");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
