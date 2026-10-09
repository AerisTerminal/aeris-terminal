use super::*;
use crate::tests::{TestDirectory, config, instrument, market_order, start_service};
use crate::{ModifyOrder, PlaceOrder, TradingService, VenueRoute};
use aeris_trading::{
    AccountEnvironment, ClientOrderId, FixedPoint, Order, OrderSide, OrderStatus, OrderType,
    TimeInForce, TradingAccount, TradingAccountId,
    venue::{
        BrokerFill, BrokerOrder, BrokerOrderKind, BrokerOrderState, PositionUnrealizedPnl,
        RealizedClose, VenuePosition, VenueSnapshot,
    },
};
use std::{
    sync::{Barrier, mpsc},
    thread,
};

const BROKER_ACCOUNT: &str = "fixture";

fn broker() -> TradingAccount {
    TradingAccount {
        id: TradingAccountId::try_new("scripted-demo").expect("account"),
        display_name: "scripted demo".into(),
        environment: AccountEnvironment::Demo,
        venue_id: "ctrader".into(),
        broker_ref: Some(BROKER_ACCOUNT.into()),
        currency: "USD".into(),
        currency_scale: 2,
        starting_equity: None,
    }
}

fn setup(label: &str) -> (TestDirectory, TradingService, mpsc::Receiver<VenueRequest>) {
    let directory = TestDirectory::new(label);
    let service = start_service(&directory);
    service.register_account(broker()).expect("account");
    service
        .register_instrument(instrument())
        .expect("instrument");
    let (generation, receiver) = service.attach_demo_venue().expect("attach");
    assert_eq!(generation, 1);
    // Every attach first reconciles each demo broker account; with nothing recorded yet
    // there are no deals to replay.
    assert!(matches!(
        receiver.try_recv(),
        Ok(VenueRequest::Reconcile { broker_account, deals_since_unix_nanos: None })
            if broker_account == BROKER_ACCOUNT
    ));
    (directory, service, receiver)
}

fn request(id: &str) -> PlaceOrder {
    let mut request = market_order(id, OrderSide::Buy, 1, 1_000);
    request.account_id = broker().id;
    request
}

fn limit_request(id: &str, price: i64) -> PlaceOrder {
    PlaceOrder {
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GoodTillCancelled,
        limit_price: Some(point(price, 2)),
        ..request(id)
    }
}

fn point(units: i64, scale: u8) -> FixedPoint {
    FixedPoint::try_new(units, scale).expect("fixed point")
}

fn venue(generation: u64, update: VenueUpdate) -> VenueEvent {
    VenueEvent {
        session_generation: generation,
        broker_account: BROKER_ACCOUNT.into(),
        update,
        observed_unix_nanos: 2_000,
    }
}

/// The broker's report of `order` under `broker_order_id`.
fn report(order: &Order, broker_order_id: &str, filled: i64) -> BrokerOrder {
    BrokerOrder {
        broker_order_id: broker_order_id.into(),
        client_order_id: Some(order.client_order_id.as_str().into()),
        instrument_id: order.instrument_id.clone(),
        side: order.side,
        kind: BrokerOrderKind::Limit,
        quantity: order.quantity,
        filled_quantity: point(filled, order.quantity.scale()),
        limit_price: order.limit_price,
        stop_price: order.stop_price,
        broker_position_id: Some("77".into()),
        closing: false,
    }
}

fn order_update(report: BrokerOrder, state: BrokerOrderState) -> VenueUpdate {
    VenueUpdate::Order {
        order: report,
        state,
        reason: None,
    }
}

fn order_status(service: &TradingService, id: &ClientOrderId) -> OrderStatus {
    service
        .snapshot()
        .expect("snapshot")
        .orders
        .into_iter()
        .find(|order| order.client_order_id == *id)
        .expect("order")
        .status
}

fn fill(deal: &str, broker_order_id: &str, quantity: i64) -> BrokerFill {
    BrokerFill {
        broker_deal_id: deal.into(),
        broker_order_id: broker_order_id.into(),
        broker_position_id: "77".into(),
        instrument_id: instrument().instrument_id,
        side: OrderSide::Buy,
        price: point(9_000, 2),
        quantity: point(quantity, 0),
        executed_unix_nanos: 2_000,
        commission: None,
        realized: None,
    }
}

fn position(id: &str, quantity: i64) -> VenuePosition {
    VenuePosition {
        broker_position_id: id.into(),
        instrument_id: instrument().instrument_id,
        side: OrderSide::Buy,
        quantity: point(quantity, 0),
        entry_price: Some(point(9_000, 2)),
        stop_loss: None,
        take_profit: None,
        swap: point(0, 2),
        commission: point(-35, 2),
        opened_unix_nanos: 2_000,
    }
}

#[test]
fn full_outbound_queue_rejects_synchronously_without_pending_order() {
    let (directory, service, _receiver) = setup("venue-full");
    for index in 0..OUTBOUND_CAPACITY {
        assert_eq!(
            service
                .place_order(request(&format!("outbound-{index}")))
                .expect("place")
                .status,
            OrderStatus::Pending
        );
    }
    let error = service
        .place_order(request("over-capacity"))
        .expect_err("full");
    assert_eq!(error, "cTrader venue outbound queue is full");
    assert_eq!(
        service.snapshot().expect("snapshot").orders.len(),
        OUTBOUND_CAPACITY
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let database = rusqlite::Connection::open(&config(&directory).database_path).expect("database");
    let count: i64 = database
        .query_row(
            "SELECT COUNT(*) FROM orders WHERE client_order_id = 'over-capacity'",
            [],
            |row| row.get(0),
        )
        .expect("rejected order count");
    assert_eq!(count, 0);
}

#[test]
fn saturated_writer_does_not_leave_pending_modify_or_cancel() {
    let (_directory, service, _receiver) = setup("venue-full-amend");
    let placed = service
        .place_order(limit_request("amend-target", 9_000))
        .expect("place");
    service
        .demo_venue_inbox()
        .push(venue(
            1,
            order_update(report(&placed, "501", 0), BrokerOrderState::Accepted),
        ))
        .expect("accepted");
    assert_eq!(
        order_status(&service, &placed.client_order_id),
        OrderStatus::Working
    );
    for index in 1..OUTBOUND_CAPACITY {
        service
            .place_order(request(&format!("queued-{index}")))
            .expect("queue place");
    }
    let modify = ModifyOrder {
        client_order_id: placed.client_order_id.clone(),
        time_in_force: TimeInForce::GoodTillCancelled,
        limit_price: Some(point(9_025, 2)),
        stop_price: None,
        modified_unix_nanos: 3_000,
        provenance: crate::tests::provenance(2, 3_000),
    };
    assert_eq!(
        service.modify_order(modify).expect_err("queue full"),
        "cTrader venue outbound queue is full"
    );
    assert_eq!(
        service
            .cancel_order(placed.client_order_id.clone())
            .expect_err("queue full"),
        "cTrader venue outbound queue is full"
    );
    assert_eq!(
        order_status(&service, &placed.client_order_id),
        OrderStatus::Working
    );
}

#[test]
fn retired_generations_are_fenced_and_the_generation_survives_a_restart() {
    let (directory, service, receiver) = setup("venue-generation");
    let order = service
        .place_order(limit_request("generation-order", 9_000))
        .expect("place");
    assert_eq!(order.id.as_str(), "ct-order-1");
    assert_eq!(
        VenueRoute::CtraderDemo,
        VenueRoute::from_account(&broker()).expect("route")
    );
    let VenueRequest::Place(sent) = receiver.recv().expect("request") else {
        panic!("a place request");
    };
    assert_eq!(sent.broker_account, BROKER_ACCOUNT);
    let (generation, _retired) = service.attach_demo_venue().expect("reconnect");
    assert_eq!(generation, 2);
    let inbox = service.demo_venue_inbox();
    inbox
        .push(venue(
            1,
            order_update(report(&order, "501", 0), BrokerOrderState::Cancelled),
        ))
        .expect("retired");
    assert_eq!(
        order_status(&service, &order.client_order_id),
        OrderStatus::Pending,
        "a retired session cannot change current state"
    );
    inbox
        .push(venue(
            2,
            order_update(report(&order, "501", 0), BrokerOrderState::Accepted),
        ))
        .expect("current");
    let snapshot = service.snapshot().expect("snapshot");
    // Snapshot events are newest first.
    let accepted = snapshot
        .order_events
        .iter()
        .find(|event| event.order_id == order.id)
        .expect("event");
    assert_eq!(accepted.provenance.session_generation, 2);
    assert_eq!(accepted.provenance.venue_id, "ctrader");
    assert_eq!(
        order_status(&service, &order.client_order_id),
        OrderStatus::Working
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");

    let restarted = start_service(&directory);
    let (generation, receiver) = restarted.attach_demo_venue().expect("advance");
    assert_eq!(generation, 3, "a restart never reuses a generation");
    // The open order makes the reconcile replay deals from its submission.
    assert!(matches!(
        receiver.try_recv(),
        Ok(VenueRequest::Reconcile { deals_since_unix_nanos: Some(since), .. })
            if since == order.submitted_unix_nanos
    ));
    // The broker id binding survives too, so a cancel needs no new acknowledgement.
    restarted
        .cancel_order(order.client_order_id.clone())
        .expect("cancel after restart");
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

#[test]
fn bounded_inbox_blocks_reader_then_applies_every_event_in_order() {
    let (_directory, service, _receiver) = setup("venue-burst");
    let (release, held) = mpsc::sync_channel(0);
    let (ready, started) = mpsc::sync_channel(0);
    let barrier = Arc::new(Barrier::new(2));
    service
        .commands
        .send(crate::Command::BlockOwnerForTest(
            ready,
            Arc::clone(&barrier),
        ))
        .expect("block");
    started.recv().expect("owner blocked");
    let inbox = Arc::clone(&service.runtime.venue_inbox);
    let total = i64::try_from(INBOX_CAPACITY + 77).expect("bounded burst");
    let sender = thread::spawn(move || {
        for units in 1..=total {
            inbox
                .push(venue(
                    1,
                    VenueUpdate::Balance {
                        balance: point(units, 2),
                    },
                ))
                .expect("no event dropped");
        }
        release.send(()).expect("finished");
    });
    thread::sleep(Duration::from_millis(80));
    assert_eq!(
        service
            .runtime
            .venue_inbox
            .pending
            .lock()
            .expect("pending")
            .queue
            .len(),
        INBOX_CAPACITY
    );
    assert!(held.try_recv().is_err(), "reader must wait at capacity");
    barrier.wait();
    held.recv_timeout(Duration::from_secs(5))
        .expect("reader finished");
    sender.join().expect("reader");
    // Every balance applied in order, so the last one stands.
    assert_eq!(
        service.snapshot().expect("snapshot").broker_balances[&broker().id],
        point(total, 2)
    );
}

#[test]
fn pending_requests_survive_acceptance_and_replacements_apply_the_brokers_prices() {
    let (_directory, service, receiver) = setup("venue-lifecycle");
    let pending = service
        .place_order(limit_request("lifecycle", 9_000))
        .expect("pending");
    assert!(matches!(
        receiver.recv().expect("place"),
        VenueRequest::Place(_)
    ));
    let inbox = service.demo_venue_inbox();
    let id = pending.client_order_id.clone();
    let push = |order: BrokerOrder, state| {
        inbox
            .push(venue(1, order_update(order, state)))
            .expect("event");
    };
    // Modify and cancel need the broker's id, which only an acknowledgement provides.
    assert!(service.cancel_order(id.clone()).is_err());
    push(report(&pending, "501", 0), BrokerOrderState::Accepted);
    assert_eq!(order_status(&service, &id), OrderStatus::Working);

    let change = ModifyOrder {
        client_order_id: id.clone(),
        time_in_force: TimeInForce::GoodTillCancelled,
        limit_price: Some(point(9_025, 2)),
        stop_price: None,
        modified_unix_nanos: 3_000,
        provenance: crate::tests::provenance(99, 3_000),
    };
    assert_eq!(
        service.modify_order(change).expect("modify").status,
        OrderStatus::PendingModify
    );
    let VenueRequest::Amend(amendment) = receiver.recv().expect("modify") else {
        panic!("an amend request");
    };
    assert_eq!(amendment.broker_order_id, "501");
    // A repeated acceptance does not hide the unanswered modify.
    push(report(&pending, "501", 0), BrokerOrderState::Accepted);
    assert_eq!(order_status(&service, &id), OrderStatus::PendingModify);
    let mut replaced = report(&pending, "501", 0);
    replaced.limit_price = Some(point(9_025, 2));
    push(replaced, BrokerOrderState::Replaced);
    let order = service
        .snapshot()
        .expect("snapshot")
        .orders
        .into_iter()
        .find(|order| order.client_order_id == id)
        .expect("order");
    assert_eq!(order.status, OrderStatus::Working);
    assert_eq!(order.limit_price, Some(point(9_025, 2)));

    assert_eq!(
        service.cancel_order(id.clone()).expect("cancel").status,
        OrderStatus::PendingCancel
    );
    assert!(matches!(
        receiver.recv().expect("cancel"),
        VenueRequest::Cancel { .. }
    ));
    push(report(&order, "501", 0), BrokerOrderState::CancelRejected);
    assert_eq!(order_status(&service, &id), OrderStatus::Working);
    service.cancel_order(id.clone()).expect("retry cancel");
    push(report(&order, "501", 0), BrokerOrderState::Cancelled);
    assert_eq!(order_status(&service, &id), OrderStatus::Cancelled);
    push(report(&order, "501", 0), BrokerOrderState::Accepted);
    assert_eq!(
        order_status(&service, &id),
        OrderStatus::Cancelled,
        "a late acceptance cannot reopen a terminal order"
    );
}

#[test]
fn stalled_inbox_abandons_generation_and_stop_wakes_blocked_reader() {
    let (commands, _receiver) = mpsc::sync_channel(2);
    let stopping = Arc::new(AtomicBool::new(false));
    let inbox =
        VenueInbox::with_blocked_limit(commands, Arc::clone(&stopping), Duration::from_millis(60));
    let make_event = || {
        venue(
            1,
            VenueUpdate::Balance {
                balance: point(1, 2),
            },
        )
    };
    for _ in 0..INBOX_CAPACITY {
        inbox.push(make_event()).expect("fill");
    }
    assert!(
        inbox
            .push(make_event())
            .expect_err("stalled")
            .contains("reconnect and reconcile")
    );
    let blocked = Arc::clone(&inbox);
    let reader = thread::spawn(move || blocked.push(make_event()));
    stopping.store(true, Ordering::Release);
    inbox.wake_stopped();
    assert_eq!(
        reader.join().expect("reader").expect_err("stop"),
        "trading owner is stopping"
    );
}

#[test]
fn refusals_rejections_and_expiry_are_terminal_and_ignore_late_updates() {
    for (label, state) in [
        ("venue-rejected", Some(BrokerOrderState::Rejected)),
        ("venue-expired", Some(BrokerOrderState::Expired)),
        ("venue-refused", None),
    ] {
        let (_directory, service, _receiver) = setup(label);
        let order = service.place_order(request(label)).expect("pending");
        let inbox = service.demo_venue_inbox();
        let update = state.map_or_else(
            || VenueUpdate::Refused {
                client_order_id: order.client_order_id.clone(),
                reason: "TRADING_BAD_VOLUME".into(),
            },
            |state| order_update(report(&order, "601", 0), state),
        );
        inbox.push(venue(1, update)).expect("terminal");
        assert!(!order_status(&service, &order.client_order_id).is_open());
        let events = service.snapshot().expect("snapshot").order_events.len();
        inbox
            .push(venue(
                1,
                order_update(report(&order, "601", 0), BrokerOrderState::Accepted),
            ))
            .expect("late");
        assert_eq!(
            service.snapshot().expect("snapshot").order_events.len(),
            events
        );
    }
}

#[test]
fn a_report_for_another_account_never_moves_an_order() {
    let (_directory, service, _receiver) = setup("venue-foreign");
    let other = TradingAccount {
        id: TradingAccountId::try_new("other-demo").expect("account"),
        broker_ref: Some("other".into()),
        ..broker()
    };
    service.register_account(other).expect("other account");
    let order = service.place_order(request("shared-id")).expect("pending");
    service
        .demo_venue_inbox()
        .push(VenueEvent {
            broker_account: "other".into(),
            ..venue(
                1,
                order_update(report(&order, "701", 0), BrokerOrderState::Cancelled),
            )
        })
        .expect("foreign");
    assert_eq!(
        order_status(&service, &order.client_order_id),
        OrderStatus::Pending
    );
}

#[test]
fn deals_record_once_and_closing_deals_mirror_broker_orders() {
    let (_directory, service, _receiver) = setup("venue-fills");
    let mut place = request("filled");
    place.quantity = point(3, 0);
    let order = service.place_order(place).expect("pending");
    let inbox = service.demo_venue_inbox();
    inbox
        .push(venue(
            1,
            order_update(report(&order, "801", 0), BrokerOrderState::Accepted),
        ))
        .expect("accepted");
    inbox
        .push(venue(1, VenueUpdate::Fill(fill("d1", "801", 1))))
        .expect("partial");
    // A replayed deal is recorded once.
    inbox
        .push(venue(1, VenueUpdate::Fill(fill("d1", "801", 1))))
        .expect("replay");
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.fills.len(), 1);
    let partial = snapshot
        .orders
        .iter()
        .find(|candidate| candidate.id == order.id)
        .expect("order");
    assert_eq!(
        (partial.status, partial.filled_quantity),
        (OrderStatus::PartiallyFilled, point(1, 0))
    );
    inbox
        .push(venue(1, VenueUpdate::Fill(fill("d2", "801", 2))))
        .expect("rest");
    assert_eq!(
        order_status(&service, &order.client_order_id),
        OrderStatus::Filled
    );

    // A server stop-loss closes the position through an order Aeris never placed.
    let mut close = fill("d3", "802", 3);
    close.side = OrderSide::Sell;
    close.realized = Some(RealizedClose {
        gross_profit: point(1_250, 2),
        swap: point(-12, 2),
        commission: point(-35, 2),
        balance: point(1_001_203, 2),
    });
    inbox
        .push(venue(1, VenueUpdate::Fill(close)))
        .expect("closing deal");
    let snapshot = service.snapshot().expect("snapshot");
    let mirror = snapshot
        .orders
        .iter()
        .find(|candidate| candidate.client_order_id.as_str() == "ct-802")
        .expect("mirrored broker order");
    assert_eq!(mirror.status, OrderStatus::Filled);
    let closing = snapshot
        .fills
        .iter()
        .find(|candidate| candidate.order_id == mirror.id)
        .expect("closing fill");
    assert_eq!(snapshot.fill_realized_pnl[&closing.id], point(1_203, 2));
    assert!(
        snapshot.round_trips.is_empty(),
        "broker fills are not paired as simulated round trips"
    );
}

#[test]
fn positions_follow_reports_and_reconcile_settles_every_open_order() {
    let (_directory, service, receiver) = setup("venue-reconcile");
    let inbox = service.demo_venue_inbox();
    inbox
        .push(venue(1, VenueUpdate::Position(position("77", 2))))
        .expect("position");
    inbox
        .push(venue(1, VenueUpdate::Position(position("78", 1))))
        .expect("position");
    inbox
        .push(venue(
            1,
            VenueUpdate::PositionClosed {
                broker_position_id: "78".into(),
            },
        ))
        .expect("closed");
    let positions = service.snapshot().expect("snapshot").broker_positions;
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].broker_position_id, "77");
    assert_eq!(positions[0].commission, point(-35, 2));

    let lost = service
        .place_order(request("never-received"))
        .expect("lost");
    let gone = service
        .place_order(limit_request("filled-offline", 9_000))
        .expect("gone");
    let kept = service
        .place_order(limit_request("still-open", 8_900))
        .expect("kept");
    for order in [&gone, &kept] {
        let broker_id = if order.id == gone.id { "901" } else { "902" };
        inbox
            .push(venue(
                1,
                order_update(report(order, broker_id, 0), BrokerOrderState::Accepted),
            ))
            .expect("accepted");
    }
    let change = ModifyOrder {
        client_order_id: kept.client_order_id.clone(),
        time_in_force: TimeInForce::GoodTillCancelled,
        limit_price: Some(point(8_950, 2)),
        stop_price: None,
        modified_unix_nanos: 3_000,
        provenance: crate::tests::provenance(9, 3_000),
    };
    service.modify_order(change).expect("modify sent");
    while receiver.try_recv().is_ok() {}

    // After a reconnect the broker holds only the kept order, at the price it accepted,
    // and a different set of positions.
    let mut kept_report = report(&kept, "902", 0);
    kept_report.limit_price = Some(point(8_950, 2));
    inbox
        .push(venue(
            1,
            VenueUpdate::Snapshot(VenueSnapshot {
                orders: vec![kept_report],
                positions: vec![position("79", 4)],
            }),
        ))
        .expect("snapshot");
    let snapshot = service.snapshot().expect("snapshot");
    let status = |order: &Order| {
        snapshot
            .orders
            .iter()
            .find(|candidate| candidate.id == order.id)
            .expect("order")
            .clone()
    };
    assert_eq!(status(&lost).status, OrderStatus::Rejected);
    assert_eq!(status(&gone).status, OrderStatus::Cancelled);
    let kept_now = status(&kept);
    assert_eq!(kept_now.status, OrderStatus::Working);
    assert_eq!(kept_now.limit_price, Some(point(8_950, 2)));
    assert_eq!(
        snapshot
            .broker_positions
            .iter()
            .map(|position| position.broker_position_id.as_str())
            .collect::<Vec<_>>(),
        ["79"]
    );
}

#[test]
fn broker_positions_close_and_amend_through_the_venue() {
    let (_directory, service, receiver) = setup("venue-positions");
    service
        .demo_venue_inbox()
        .push(venue(1, VenueUpdate::Position(position("77", 2))))
        .expect("position");
    service
        .close_broker_position(broker().id, "77".into())
        .expect("close");
    assert!(matches!(
        receiver.recv().expect("close"),
        VenueRequest::ClosePosition { broker_position_id, quantity, .. }
            if broker_position_id == "77" && quantity == point(2, 0)
    ));
    service
        .amend_broker_position_sltp(broker().id, "77".into(), Some(point(8_800, 2)), None)
        .expect("amend");
    assert!(matches!(
        receiver.recv().expect("amend"),
        VenueRequest::AmendPositionProtection { stop_loss: Some(stop), take_profit: None, .. }
            if stop == point(8_800, 2)
    ));
    assert!(
        service
            .close_broker_position(broker().id, "unknown".into())
            .is_err()
    );
}

#[test]
fn a_broker_account_never_blocks_global_commands_and_kill_locks_it_offline() {
    let directory = TestDirectory::new("venue-global");
    let service = start_service(&directory);
    service.register_account(broker()).expect("account");
    service
        .register_instrument(instrument())
        .expect("instrument");
    let simulated = TradingAccountId::try_new("aeris-sim-1").expect("simulated");
    let mut resting = market_order("sim-resting", OrderSide::Buy, 1, 1_000);
    resting.order_type = OrderType::Limit;
    resting.time_in_force = TimeInForce::GoodTillCancelled;
    resting.limit_price = Some(point(8_000, 2));
    service.place_order(resting).expect("simulated order");
    assert_eq!(service.cancel_all(None).expect("cancel all").len(), 1);

    // With no venue attached the kill switch still locks every account, broker included.
    assert_eq!(
        service
            .kill_switch(None, "test".into(), 5_000)
            .expect("nothing broker-side to cancel"),
        0
    );
    let locks = service.snapshot().expect("snapshot").risk_locks;
    for account in [&simulated, &broker().id] {
        assert!(
            locks.iter().any(|lock| lock.account_id == *account),
            "{account:?} locked"
        );
    }
    let _venue = service.attach_demo_venue().expect("attach");
    assert!(
        service
            .place_order(request("after-kill"))
            .expect_err("locked")
            .contains("risk-locked"),
        "a locked broker account accepts no new order"
    );
}

#[test]
fn the_kill_switch_reports_broker_orders_it_could_not_cancel_after_locking() {
    let (_directory, service, _receiver) = setup("venue-kill-pending");
    // An unacknowledged order has no broker id yet, so no cancel can be requested.
    service
        .place_order(request("unacknowledged"))
        .expect("pending");
    let error = service
        .kill_switch(Some(broker().id), "test".into(), 5_000)
        .expect_err("cancel could not be requested");
    assert!(error.starts_with("accounts are locked;"), "{error}");
    assert!(
        service
            .snapshot()
            .expect("snapshot")
            .risk_locks
            .iter()
            .any(|lock| lock.account_id == broker().id)
    );
}

#[test]
fn the_simulator_never_fills_a_broker_order() {
    let (_directory, service, _receiver) = setup("venue-simulator");
    let order = service
        .place_order(request("broker-market"))
        .expect("pending");
    service
        .demo_venue_inbox()
        .push(venue(
            1,
            order_update(report(&order, "1001", 0), BrokerOrderState::Accepted),
        ))
        .expect("accepted");
    service
        .observe_market(crate::tests::observation(8_975, 9_000, 2, 3_000))
        .expect("observation");
    assert_eq!(
        order_status(&service, &order.client_order_id),
        OrderStatus::Working
    );
    assert_eq!(service.snapshot().expect("snapshot").fills, []);
}

#[test]
fn day_pending_orders_rest_good_till_cancelled_and_market_orders_fill_or_cancel() {
    let (_directory, service, receiver) = setup("venue-time-in-force");
    let day_limit = PlaceOrder {
        time_in_force: TimeInForce::Day,
        ..limit_request("day-limit", 9_000)
    };
    let placed = service.place_order(day_limit).expect("limit");
    assert_eq!(placed.time_in_force, TimeInForce::GoodTillCancelled);
    let VenueRequest::Place(sent) = receiver.try_recv().expect("place") else {
        panic!("a place request");
    };
    assert_eq!(sent.time_in_force, TimeInForce::GoodTillCancelled);
    let market = service.place_order(request("day-market")).expect("market");
    assert_eq!(market.time_in_force, TimeInForce::ImmediateOrCancel);

    // A desktop modify still names its day intent and is accepted.
    service
        .demo_venue_inbox()
        .push(venue(
            1,
            order_update(report(&placed, "1101", 0), BrokerOrderState::Accepted),
        ))
        .expect("accepted");
    let modify = ModifyOrder {
        client_order_id: placed.client_order_id.clone(),
        time_in_force: TimeInForce::Day,
        limit_price: Some(point(9_025, 2)),
        stop_price: None,
        modified_unix_nanos: 3_000,
        provenance: crate::tests::provenance(7, 3_000),
    };
    assert_eq!(
        service.modify_order(modify).expect("modify").status,
        OrderStatus::PendingModify
    );
}

#[test]
fn a_broker_bracket_sends_server_protection_at_the_template_distances() {
    let (_directory, service, receiver) = setup("venue-bracket");
    let template = crate::BracketStrategyTemplate {
        template_id: "broker-bracket".into(),
        revision: 1,
        name: "Broker bracket".into(),
        stop_offset_ticks: 8,
        targets: vec![crate::BracketTarget {
            offset_ticks: 16,
            quantity_percent: 100,
        }],
        trailing_stop: None,
        break_even: None,
        enabled: true,
    };
    let placement = service
        .place_inline_bracket(crate::PlaceInlineBracket {
            entry: request("protected-entry"),
            template,
        })
        .expect("bracket");
    let crate::BracketPlacement::BrokerProtected(order) = placement else {
        panic!("a broker bracket is held by the broker");
    };
    assert_eq!(order.status, OrderStatus::Pending);
    let VenueRequest::Place(sent) = receiver.try_recv().expect("place") else {
        panic!("a place request");
    };
    // The fixture tick is 0.25, so 8 and 16 ticks are 2.00 and 4.00.
    assert_eq!(
        (sent.stop_loss, sent.take_profit),
        (
            Some(aeris_trading::venue::Protection::Distance(point(200, 2))),
            Some(aeris_trading::venue::Protection::Distance(point(400, 2)))
        )
    );
    assert_eq!(service.snapshot().expect("snapshot").managed_brackets, []);
}

#[test]
fn open_broker_positions_count_toward_the_maximum_contract_rule() {
    let (_directory, service, _receiver) = setup("venue-max-contracts");
    service
        .register_risk_profile(crate::RiskProfile {
            account_id: broker().id,
            profile_id: "two-contracts".into(),
            version: 1,
            session_start_unix_nanos: 1,
            session_start_realized_pnl: point(0, 2),
            daily_loss_limit: point(100_000, 2),
            trailing_drawdown: None,
            trailing_mode: crate::TrailingDrawdownMode::EndOfDay,
            max_contracts: point(2, 0),
            consistency_max_single_trade_percent: None,
            restricted_until_unix_nanos: None,
            economic_event_rule: None,
            enabled: true,
        })
        .expect("profile");
    service
        .demo_venue_inbox()
        .push(venue(1, VenueUpdate::Position(position("77", 2))))
        .expect("position");
    assert!(
        service
            .place_order(request("third-contract"))
            .expect_err("over the maximum")
            .contains("maximum-contract")
    );
}

#[test]
fn connected_broker_positions_carry_the_broker_unrealized_pnl() {
    let (_directory, service, receiver) = setup("venue-unrealized");
    let inbox = service.demo_venue_inbox();
    inbox
        .push(venue(
            1,
            VenueUpdate::AccountObserved(aeris_trading::venue::ObservedAccount {
                environment: AccountEnvironment::Demo,
                display_name: broker().display_name,
                currency: "USD".into(),
                currency_scale: 2,
            }),
        ))
        .expect("observed");
    inbox
        .push(venue(
            1,
            VenueUpdate::Balance {
                balance: point(100_000, 2),
            },
        ))
        .expect("balance");
    assert!(
        receiver.try_recv().is_err(),
        "no positions, so no P&L requests"
    );
    inbox
        .push(venue(1, VenueUpdate::Position(position("77", 2))))
        .expect("position");
    assert!(matches!(
        receiver.recv_timeout(std::time::Duration::from_secs(5)),
        Ok(VenueRequest::UnrealizedPnl { broker_account }) if broker_account == BROKER_ACCOUNT
    ));

    inbox
        .push(venue(
            1,
            VenueUpdate::UnrealizedPnl(vec![
                PositionUnrealizedPnl {
                    broker_position_id: "77".into(),
                    gross: point(-125, 2),
                    net: point(-160, 2),
                },
                PositionUnrealizedPnl {
                    broker_position_id: "unknown".into(),
                    gross: point(5, 2),
                    net: point(5, 2),
                },
            ]),
        ))
        .expect("pnl");
    // A later position event carries no P&L and keeps the last refresh.
    inbox
        .push(venue(1, VenueUpdate::Position(position("77", 2))))
        .expect("position again");
    let snapshot = service.snapshot().expect("snapshot");
    let position = &snapshot.broker_positions[0];
    assert_eq!(
        (position.gross_unrealized, position.net_unrealized),
        (point(-125, 2), point(-160, 2))
    );
    let pnl = snapshot
        .account_pnl
        .iter()
        .find(|pnl| pnl.account_id == broker().id)
        .expect("account pnl");
    assert_eq!(pnl.unrealized, point(-160, 2));
    assert_eq!(
        pnl.equity,
        Some(point(99_840, 2)),
        "balance plus net open P&L"
    );

    // A new generation reaches no account yet: no open P&L or equity is claimed.
    let (_generation, next) = service.attach_demo_venue().expect("reattach");
    let snapshot = service.snapshot().expect("snapshot");
    let pnl = snapshot
        .account_pnl
        .iter()
        .find(|pnl| pnl.account_id == broker().id)
        .expect("account pnl");
    assert_eq!((pnl.unrealized, pnl.equity), (point(0, 2), None));
    assert!(matches!(
        next.try_recv(),
        Ok(VenueRequest::Reconcile { .. })
    ));
    assert!(
        next.try_recv().is_err(),
        "an unreachable account is not polled"
    );
}

#[test]
fn observed_accounts_register_once_and_new_demo_accounts_reconcile_at_once() {
    let (_directory, service, receiver) = setup("venue-observed");
    let observed = |name: &str| {
        VenueUpdate::AccountObserved(aeris_trading::venue::ObservedAccount {
            environment: AccountEnvironment::Demo,
            display_name: name.into(),
            currency: "EUR".into(),
            currency_scale: 2,
        })
    };
    let inbox = service.demo_venue_inbox();
    inbox
        .push(VenueEvent {
            broker_account: "2002".into(),
            ..venue(1, observed("cTrader Demo · Example 2002"))
        })
        .expect("observed");
    let accounts = service.snapshot().expect("snapshot").accounts;
    let registered = accounts
        .iter()
        .find(|account| account.broker_ref.as_deref() == Some("2002"))
        .expect("registered");
    assert_eq!(registered.id.as_str(), "ctrader-demo-2002");
    assert_eq!(
        (registered.environment, registered.currency.as_str()),
        (AccountEnvironment::Demo, "EUR")
    );
    assert!(matches!(
        receiver.try_recv(),
        Ok(VenueRequest::Reconcile { broker_account, .. }) if broker_account == "2002"
    ));
    // Seeing it again keeps the id and only follows the broker's name.
    inbox
        .push(VenueEvent {
            broker_account: "2002".into(),
            ..venue(1, observed("renamed"))
        })
        .expect("observed again");
    let accounts = service.snapshot().expect("snapshot").accounts;
    let matching: Vec<_> = accounts
        .iter()
        .filter(|account| account.broker_ref.as_deref() == Some("2002"))
        .collect();
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0].display_name, "renamed");
    assert!(
        receiver.try_recv().is_err(),
        "a known account is not reconciled again"
    );

    // Only announced accounts are reachable, and a new generation starts with none.
    let connected = service
        .snapshot()
        .expect("snapshot")
        .connected_broker_accounts;
    assert!(connected.contains(&TradingAccountId::try_new("ctrader-demo-2002").expect("id")));
    assert!(!connected.contains(&broker().id));
    let _next = service.attach_demo_venue().expect("reattach");
    assert_eq!(
        service
            .snapshot()
            .expect("snapshot")
            .connected_broker_accounts
            .len(),
        0
    );
}
