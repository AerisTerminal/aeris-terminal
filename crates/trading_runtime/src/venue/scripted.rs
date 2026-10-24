use super::*;
use crate::tests::{
    TestDirectory, config, instrument, market_order, start_service, start_service_with_config,
};
use crate::{ModifyOrder, MoveBracketLevel, PendingExit, PlaceOrder, TradingService, VenueRoute};
use aeris_trading::{
    AccountEnvironment, ClientOrderId, FixedPoint, Order, OrderSide, OrderStatus, OrderType,
    Position, TimeInForce, TradingAccount, TradingAccountId,
    venue::{
        BracketLevel, BracketSide, BrokerFill, BrokerOrder, BrokerOrderKind, BrokerOrderState,
        ServerBracket, VenuePosition, VenueSnapshot,
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
        venue_id: aeris_trading::venue::BROKER_VENUE_ID.into(),
        broker_ref: Some(BROKER_ACCOUNT.into()),
        currency: "USD".into(),
        currency_scale: 2,
        starting_equity: None,
    }
}

fn setup(
    label: &str,
) -> (
    TestDirectory,
    TradingService,
    aeris_trading::venue_outbox::VenueRequestReceiver,
) {
    let directory = TestDirectory::new(label);
    // The scripted venue uses fixture timestamps just after the epoch, so the clock stays
    // in that world and no risk-session boundary ever passes mid-test.
    let mut configuration = config(&directory);
    configuration.clock = std::sync::Arc::new(crate::clock::ManualClock::new(1_000_000));
    let service = start_service_with_config(configuration);
    service.register_account(broker()).expect("account");
    service
        .register_instrument(instrument())
        .expect("instrument");
    let (generation, receiver) = service
        .attach_demo_venue(crate::tests::VENUE_CAPABILITIES)
        .expect("attach");
    assert_eq!(generation, 1);
    // Every attach first reconciles each demo broker account; with nothing recorded yet
    // there are no deals to replay.
    assert!(matches!(
        receiver.try_recv(),
        Ok(VenueRequest::Reconcile { broker_account, deals_since_unix_nanos: None })
            if broker_account == BROKER_ACCOUNT
    ));
    // The account takes new orders once that reconcile's snapshot settles it.
    service
        .demo_venue_inbox()
        .push(venue(1, VenueUpdate::Snapshot(VenueSnapshot::default())))
        .expect("reconcile snapshot");
    let feed = service
        .attach_market_feed(std::sync::Arc::new(
            crate::tests::market_feed::RecordedInterest::default(),
        ))
        .expect("market feed");
    crate::tests::bootstrap_live_feed(&feed, &instrument(), crate::tests::market_feed::route());
    crate::tests::push_live_quote(&feed, &instrument(), 1, 10_000, 10_025, 2);
    service.status().expect("owner drains verified feed");
    (directory, service, receiver)
}

#[test]
fn simulated_and_broker_acceptance_have_durable_initial_lifecycle_fields() {
    let (directory, service, _receiver) = setup("initial-lifecycle");
    let simulated = service
        .place_order({
            let mut order = limit_request("local-initial", 9_000);
            order.account_id = TradingAccountId::try_new("aeris-sim-1").expect("account");
            order
        })
        .expect("simulated acceptance");
    let broker = service
        .place_order(limit_request("broker-initial", 9_000))
        .expect("broker submission");
    for order in [&simulated, &broker] {
        assert_eq!(order.revision, 1);
        assert!(order.accepted_unix_nanos > order.submitted_unix_nanos);
        assert_eq!(order.updated_unix_nanos, order.accepted_unix_nanos);
    }
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = TradingService::start(config(&directory)).expect("restart");
    let orders = restarted.snapshot().expect("restored orders").orders;
    for expected in [simulated, broker] {
        assert_eq!(
            orders.iter().find(|order| order.id == expected.id),
            Some(&expected)
        );
    }
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
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

fn assert_broker_lifecycle(
    service: &TradingService,
    original: &Order,
    revision: u32,
    terminal: bool,
) {
    let snapshot = service.snapshot().expect("snapshot");
    let current = snapshot
        .orders
        .iter()
        .find(|order| order.id == original.id)
        .expect("order");
    assert_eq!(current.revision, revision);
    assert!(current.updated_unix_nanos >= original.accepted_unix_nanos);
    assert_eq!(
        current.terminal_unix_nanos,
        terminal.then_some(current.updated_unix_nanos)
    );
}

fn assert_broker_orders_survive_restart(directory: &TestDirectory, service: &TradingService) {
    let before = service.snapshot().expect("before restart").orders;
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = TradingService::start(config(directory)).expect("restart");
    assert_eq!(restarted.snapshot().expect("restored").orders, before);
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

/// A buy deal at 90.00; the fixture contract is worth 50.00 per point.
fn fill(deal: &str, broker_order_id: &str, quantity: i64) -> BrokerFill {
    BrokerFill {
        broker_deal_id: deal.into(),
        broker_order_id: broker_order_id.into(),
        client_order_id: None,
        instrument_id: instrument().instrument_id,
        side: OrderSide::Buy,
        price: point(9_000, 2),
        quantity: point(quantity, 0),
        executed_unix_nanos: 2_000,
    }
}

fn sell(deal: &str, broker_order_id: &str, quantity: i64, price: i64) -> BrokerFill {
    BrokerFill {
        side: OrderSide::Sell,
        price: point(price, 2),
        ..fill(deal, broker_order_id, quantity)
    }
}

/// The venue's report of the account's net position at 90.00, without open P&L.
fn position(net: i64) -> VenuePosition {
    VenuePosition {
        instrument_id: instrument().instrument_id,
        net_quantity: point(net, 0),
        average_price: (net != 0).then(|| point(9_000, 2)),
        open_pnl: None,
    }
}

fn broker_position(service: &TradingService) -> Option<Position> {
    service
        .snapshot()
        .expect("snapshot")
        .positions
        .into_iter()
        .find(|position| {
            position.account_id == broker().id
                && position.instrument_id == instrument().instrument_id
        })
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
    assert_eq!(error, "broker venue outbound queue is full");
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
    let before = service.snapshot().expect("facts before rejected requests");
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
        "broker venue outbound queue is full"
    );
    assert_eq!(
        service
            .cancel_order(placed.client_order_id.clone())
            .expect_err("queue full"),
        "broker venue outbound queue is full"
    );
    assert_eq!(
        order_status(&service, &placed.client_order_id),
        OrderStatus::Working
    );
    assert_eq!(service.snapshot().expect("unchanged facts"), before);
}

#[test]
fn pending_broker_modify_preserves_revision_and_time_after_restart() {
    let (directory, service, receiver) = setup("pending-revision");
    let placed = service
        .place_order(limit_request("pending-revision", 9_000))
        .expect("place");
    assert!(matches!(
        receiver.recv().expect("place request"),
        VenueRequest::Place(_)
    ));
    service
        .demo_venue_inbox()
        .push(venue(
            1,
            order_update(report(&placed, "501", 0), BrokerOrderState::Accepted),
        ))
        .expect("ack");
    let pending = service
        .modify_order(ModifyOrder {
            client_order_id: placed.client_order_id.clone(),
            time_in_force: TimeInForce::GoodTillCancelled,
            limit_price: Some(point(9_025, 2)),
            stop_price: None,
            modified_unix_nanos: 3_000,
            provenance: crate::tests::provenance(3, 3_000),
        })
        .expect("modify request");
    assert_eq!(pending.status, OrderStatus::PendingModify);
    assert!(pending.revision > placed.revision);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = TradingService::start(config(&directory)).expect("restart");
    assert!(
        restarted
            .snapshot()
            .expect("restored")
            .orders
            .contains(&pending)
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

#[test]
fn stale_broker_commands_do_not_reserve_venue_requests_or_mutate_facts() {
    use crate::{OrderCommandError, RevisionedCancelOrder, RevisionedModifyOrder};
    let (directory, service, receiver) = setup("broker-stale");
    let placed = service
        .place_order(limit_request("broker-stale", 9_000))
        .expect("place");
    assert!(matches!(
        receiver.recv().expect("place"),
        VenueRequest::Place(_)
    ));
    service
        .demo_venue_inbox()
        .push(venue(
            1,
            order_update(report(&placed, "501", 0), BrokerOrderState::Accepted),
        ))
        .expect("ack");
    let current = service
        .snapshot()
        .expect("snapshot")
        .orders
        .into_iter()
        .find(|order| order.id == placed.id)
        .expect("current");
    let before = service.snapshot().expect("before");
    assert_eq!(
        service
            .modify_order_checked(RevisionedModifyOrder {
                order: ModifyOrder {
                    client_order_id: placed.client_order_id.clone(),
                    time_in_force: TimeInForce::GoodTillCancelled,
                    limit_price: Some(point(9_025, 2)),
                    stop_price: None,
                    modified_unix_nanos: 3_000,
                    provenance: crate::tests::provenance(3, 3_000),
                },
                expected_revision: Some(placed.revision),
            })
            .expect_err("stale amend"),
        OrderCommandError::StaleRevision {
            current: Box::new(current.clone())
        }
    );
    assert_eq!(
        service
            .cancel_order_checked(RevisionedCancelOrder {
                client_order_id: placed.client_order_id.clone(),
                expected_revision: Some(placed.revision),
            })
            .expect_err("stale cancel"),
        OrderCommandError::StaleRevision {
            current: Box::new(current.clone())
        }
    );
    assert_eq!(service.snapshot().expect("unchanged"), before);
    assert!(receiver.try_recv().is_err(), "no stale venue request");
    let pending = service
        .cancel_order_checked(RevisionedCancelOrder {
            client_order_id: placed.client_order_id.clone(),
            expected_revision: Some(current.revision),
        })
        .expect("matching revision");
    assert_eq!(pending.revision, current.revision + 1);
    assert!(matches!(
        receiver.recv().expect("cancel"),
        VenueRequest::Cancel { .. }
    ));
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = TradingService::start(config(&directory)).expect("restart");
    assert!(
        restarted
            .snapshot()
            .expect("restored")
            .orders
            .contains(&pending)
    );
    assert_eq!(
        restarted
            .cancel_order_checked(RevisionedCancelOrder {
                client_order_id: placed.client_order_id.clone(),
                expected_revision: Some(placed.revision),
            })
            .expect_err("stale after restart"),
        OrderCommandError::StaleRevision {
            current: Box::new(pending)
        }
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

#[test]
fn retired_generations_are_fenced_and_the_generation_survives_a_restart() {
    let (directory, service, receiver) = setup("venue-generation");
    let order = service
        .place_order(limit_request("generation-order", 9_000))
        .expect("place");
    assert_eq!(order.id.as_str(), "broker-order-1");
    assert_eq!(
        VenueRoute::BrokerDemo,
        VenueRoute::from_account(&broker()).expect("route")
    );
    let VenueRequest::Place(sent) = receiver.recv().expect("request") else {
        panic!("a place request");
    };
    assert_eq!(sent.broker_account, BROKER_ACCOUNT);
    let (generation, _retired) = service
        .attach_demo_venue(crate::tests::VENUE_CAPABILITIES)
        .expect("reconnect");
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
    assert_eq!(
        accepted.provenance.venue_id,
        aeris_trading::venue::BROKER_VENUE_ID
    );
    assert_eq!(
        order_status(&service, &order.client_order_id),
        OrderStatus::Working
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");

    let restarted = start_service(&directory);
    let (generation, receiver) = restarted
        .attach_demo_venue(crate::tests::VENUE_CAPABILITIES)
        .expect("advance");
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
    assert_broker_lifecycle(&service, &pending, 2, false);

    let change = ModifyOrder {
        client_order_id: id.clone(),
        time_in_force: TimeInForce::GoodTillCancelled,
        limit_price: Some(point(9_025, 2)),
        stop_price: None,
        modified_unix_nanos: 3_000,
        provenance: crate::tests::provenance(99, 3_000),
    };
    let pending_modify = service.modify_order(change).expect("modify");
    assert_eq!(pending_modify.status, OrderStatus::PendingModify);
    assert_eq!(pending_modify.revision, 3);
    assert!(pending_modify.updated_unix_nanos >= pending.accepted_unix_nanos);
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
    assert_broker_lifecycle(&service, &pending, 4, false);

    let pending_cancel = service.cancel_order(id.clone()).expect("cancel");
    assert_eq!(pending_cancel.status, OrderStatus::PendingCancel);
    assert!(pending_cancel.revision > order.revision);
    assert!(pending_cancel.updated_unix_nanos >= pending_modify.updated_unix_nanos);
    assert!(matches!(
        receiver.recv().expect("cancel"),
        VenueRequest::Cancel { .. }
    ));
    push(report(&order, "501", 0), BrokerOrderState::CancelRejected);
    assert_eq!(order_status(&service, &id), OrderStatus::Working);
    assert_broker_lifecycle(&service, &pending, 6, false);
    service.cancel_order(id.clone()).expect("retry cancel");
    push(report(&order, "501", 0), BrokerOrderState::Cancelled);
    assert_eq!(order_status(&service, &id), OrderStatus::Cancelled);
    assert_broker_lifecycle(&service, &pending, 8, true);
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
        let (directory, service, _receiver) = setup(label);
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
        if state == Some(BrokerOrderState::Expired) {
            assert_eq!(
                order_status(&service, &order.client_order_id),
                OrderStatus::Expired
            );
            assert_eq!(
                service.snapshot().expect("snapshot").order_events[0].kind,
                aeris_trading::OrderEventKind::Expired
            );
        }
        assert_broker_lifecycle(&service, &order, 2, true);
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
        assert_broker_orders_survive_restart(&directory, &service);
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
fn a_broker_cancel_keeps_its_reason_and_a_requested_cancel_is_marked() {
    let (_directory, service, _receiver) = setup("venue-cancel-reason");
    let inbox = service.demo_venue_inbox();
    let ended = service
        .place_order(limit_request("broker-ended", 9_000))
        .expect("pending");
    inbox
        .push(venue(
            1,
            order_update(report(&ended, "901", 0), BrokerOrderState::Accepted),
        ))
        .expect("accepted");
    let explanation = format!("cancelled by the risk system {}", "é".repeat(200));
    inbox
        .push(venue(
            1,
            VenueUpdate::Order {
                order: report(&ended, "901", 0),
                state: BrokerOrderState::Cancelled,
                reason: Some(explanation.clone()),
            },
        ))
        .expect("cancelled");
    let snapshot = service.snapshot().expect("snapshot");
    let event = snapshot
        .order_events
        .iter()
        .find(|event| event.order_id == ended.id)
        .expect("newest event");
    assert_eq!(event.kind, aeris_trading::OrderEventKind::Cancelled);
    assert_eq!(event.reason, None, "the broker decided this cancel");
    let detail = event.detail.as_deref().expect("detail");
    assert!(detail.starts_with("cancelled by the risk system"));
    assert!(
        detail.len() <= aeris_trading::MAXIMUM_TRADING_FIELD_BYTES
            && explanation.starts_with(detail),
        "an over-long explanation is cut on a character boundary"
    );

    let asked = service
        .place_order(limit_request("user-ended", 9_000))
        .expect("pending");
    inbox
        .push(venue(
            1,
            order_update(report(&asked, "902", 0), BrokerOrderState::Accepted),
        ))
        .expect("accepted");
    service
        .cancel_order(asked.client_order_id.clone())
        .expect("cancel requested");
    inbox
        .push(venue(
            1,
            order_update(report(&asked, "902", 0), BrokerOrderState::Cancelled),
        ))
        .expect("cancel confirmed");
    let snapshot = service.snapshot().expect("snapshot");
    let cancels: Vec<_> = snapshot
        .order_events
        .iter()
        .filter(|event| {
            event.order_id == asked.id && event.kind == aeris_trading::OrderEventKind::Cancelled
        })
        .collect();
    assert_eq!(cancels.len(), 2, "the request and the confirmation");
    assert!(
        cancels
            .iter()
            .all(|event| event.reason == Some(aeris_trading::OrderEventReason::UserCancel))
    );
    assert_eq!(
        order_status(&service, &asked.client_order_id),
        OrderStatus::Cancelled
    );
}

#[test]
fn deals_record_once_and_closing_deals_mirror_broker_orders() {
    let (directory, service, _receiver) = setup("venue-fills");
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
    assert_broker_lifecycle(&service, &order, 3, false);
    inbox
        .push(venue(1, VenueUpdate::Fill(fill("d2", "801", 2))))
        .expect("rest");
    assert_eq!(
        order_status(&service, &order.client_order_id),
        OrderStatus::Filled
    );
    assert_broker_lifecycle(&service, &order, 4, true);

    // A server-held stop closes the position through an order Aeris never placed, and the
    // ledger realizes it like any exit: 0.50 points on 3 contracts at 50.00 a point.
    inbox
        .push(venue(1, VenueUpdate::Fill(sell("d3", "802", 3, 9_050))))
        .expect("closing deal");
    let snapshot = service.snapshot().expect("snapshot");
    let mirror = snapshot
        .orders
        .iter()
        .find(|candidate| candidate.client_order_id.as_str() == "ct-802")
        .expect("mirrored broker order");
    assert_eq!(mirror.status, OrderStatus::Filled);
    assert_eq!(mirror.revision, 2);
    assert_eq!(mirror.terminal_unix_nanos, Some(mirror.updated_unix_nanos));
    let closing = snapshot
        .fills
        .iter()
        .find(|candidate| candidate.order_id == mirror.id)
        .expect("closing fill");
    assert_eq!(snapshot.fill_realized_pnl[&closing.id], point(7_500, 2));
    assert_eq!(snapshot.round_trips.len(), 1, "the position is closed");
    assert_eq!(
        broker_position(&service).map(|position| position.net_quantity),
        Some(point(0, 0))
    );
    assert_broker_orders_survive_restart(&directory, &service);
}

#[test]
fn each_broker_closing_deal_is_one_round_trip_as_it_fills_and_survives_a_restart() {
    let (directory, service, _receiver) = setup("venue-round-trips");
    let inbox = service.demo_venue_inbox();
    for (deal, quantity) in [("d1", 1), ("d2", 2)] {
        inbox
            .push(venue(1, VenueUpdate::Fill(fill(deal, "801", quantity))))
            .expect("entry");
    }
    let closing_deal = |deal, quantity, price, executed_unix_nanos| BrokerFill {
        executed_unix_nanos,
        ..sell(deal, "802", quantity, price)
    };
    inbox
        .push(venue(
            1,
            VenueUpdate::Fill(closing_deal("d3", 1, 9_500, 3_000)),
        ))
        .expect("partial closing deal");
    let row = |trip: &crate::TradeRoundTrip| {
        assert!(trip.closed);
        assert_eq!(trip.side, OrderSide::Buy);
        (
            trip.entry.map(|leg| (leg.quantity, leg.average_price)),
            trip.exit.map(|leg| (leg.quantity, leg.average_price)),
            trip.final_pnl,
        )
    };
    let partial = (
        Some((point(1, 0), point(9_000, 2))),
        Some((point(1, 0), point(9_500, 2))),
        Some(point(25_000, 2)),
    );
    let trips = service.snapshot().expect("snapshot").round_trips;
    assert_eq!(
        trips
            .iter()
            .filter(|trip| trip.closed)
            .map(row)
            .collect::<Vec<_>>(),
        [partial],
        "a partial close is history while the rest is still open"
    );
    assert_eq!(trips.iter().filter(|trip| !trip.closed).count(), 1);

    inbox
        .push(venue(
            1,
            VenueUpdate::Fill(closing_deal("d4", 2, 9_600, 4_000)),
        ))
        .expect("final closing deal");
    let expected = |trips: &[crate::TradeRoundTrip]| {
        assert_eq!(
            trips.iter().map(row).collect::<Vec<_>>(),
            [
                (
                    Some((point(2, 0), point(9_000, 2))),
                    Some((point(2, 0), point(9_600, 2))),
                    Some(point(60_000, 2)),
                ),
                partial,
            ]
        );
    };
    let snapshot = service.snapshot().expect("snapshot");
    expected(&snapshot.round_trips);
    assert_eq!(
        snapshot
            .account_pnl
            .iter()
            .find(|pnl| pnl.account_id == broker().id)
            .map(|pnl| pnl.realized),
        Some(point(85_000, 2)),
        "every recorded closing deal counts toward the realized P&L"
    );

    drop(service);
    let restarted = start_service(&directory);
    expected(&restarted.snapshot().expect("snapshot").round_trips);
}

#[test]
fn reconcile_settles_every_open_order_and_rebases_positions_the_venue_disagrees_with() {
    let (directory, service, receiver) = setup("venue-reconcile");
    let inbox = service.demo_venue_inbox();
    // A position report is not a fill: only deals and reconciles move the ledger.
    inbox
        .push(venue(1, VenueUpdate::Position(position(2))))
        .expect("position");
    assert_eq!(broker_position(&service), None);

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
    // and a position of 4 whose fills the owner never saw.
    let mut kept_report = report(&kept, "902", 0);
    kept_report.limit_price = Some(point(8_950, 2));
    inbox
        .push(venue(
            1,
            VenueUpdate::Snapshot(VenueSnapshot {
                orders: vec![kept_report.clone()],
                positions: vec![position(4)],
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
    assert_broker_lifecycle(&service, &lost, 2, true);
    assert_broker_lifecycle(&service, &gone, 3, true);
    let kept_now = status(&kept);
    assert_eq!(kept_now.status, OrderStatus::Working);
    assert_eq!(kept_now.limit_price, Some(point(8_950, 2)));
    assert_broker_lifecycle(&service, &kept, 4, false);
    let rebased = broker_position(&service).expect("rebased position");
    assert_eq!(
        (rebased.net_quantity, rebased.average_entry_price),
        (point(4, 0), Some(point(9_000, 2)))
    );

    // A venue that reports the account flat closes the ledger position the same way.
    inbox
        .push(venue(
            1,
            VenueUpdate::Snapshot(VenueSnapshot {
                orders: vec![kept_report],
                positions: Vec::new(),
            }),
        ))
        .expect("flat snapshot");
    let flat = broker_position(&service).expect("flat position");
    assert_eq!(
        (flat.net_quantity, flat.average_entry_price),
        (point(0, 0), None)
    );
    let positions = service.snapshot().expect("snapshot").positions;
    assert_broker_orders_survive_restart(&directory, &service);
    let restarted = start_service(&directory);
    assert_eq!(restarted.snapshot().expect("restored").positions, positions);
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

#[test]
fn a_broker_flatten_exits_every_ledger_position_and_waits_for_the_fills() {
    // VAL-TRD-062: a scripted broker account with positions on two symbols receives
    // exit requests for both from one flatten; an instrument scope exits only its own.
    let (_directory, service, receiver) = setup("venue-flatten-all");
    let mut second_instrument = instrument();
    second_instrument.instrument_id =
        crate::InstrumentId::try_new("instrument:fixture:CME:CLZ6").expect("second instrument id");
    service
        .register_instrument(second_instrument.clone())
        .expect("second instrument");
    let inbox = service.demo_venue_inbox();
    inbox
        .push(venue(1, VenueUpdate::Fill(fill("d1", "801", 2))))
        .expect("long position");
    inbox
        .push(venue(
            1,
            VenueUpdate::Fill(BrokerFill {
                instrument_id: second_instrument.instrument_id.clone(),
                ..sell("d2", "802", 1, 9_000)
            }),
        ))
        .expect("short position");
    let exit = |instrument_id: &crate::InstrumentId, net| PendingExit {
        account_id: broker().id,
        instrument_id: instrument_id.clone(),
        net_quantity: point(net, 0),
    };

    let scoped = service
        .flatten(crate::FlattenScope {
            account: Some(broker().id),
            instrument: Some(instrument().instrument_id),
        })
        .expect("instrument-scoped broker flatten");
    assert_eq!(scoped.pending_exits, [exit(&instrument().instrument_id, 2)]);
    assert_eq!(scoped.incomplete, None);
    assert!(matches!(
        receiver.recv().expect("exit"),
        VenueRequest::ExitPosition { instrument_id, .. } if instrument_id == instrument().instrument_id
    ));
    assert_eq!(
        broker_position(&service).map(|position| position.net_quantity),
        Some(point(2, 0)),
        "the position stays until the venue reports the exit's fills"
    );

    let all = service
        .flatten(crate::FlattenScope {
            account: Some(broker().id),
            instrument: None,
        })
        .expect("account-wide broker flatten");
    assert_eq!(
        all.pending_exits,
        [
            exit(&second_instrument.instrument_id, -1),
            exit(&instrument().instrument_id, 2)
        ]
    );
    assert_eq!(all.incomplete, None);
    // A whole-account flatten first cancels every order the venue holds, bracket exits
    // included.
    assert!(matches!(
        receiver.try_recv(),
        Ok(VenueRequest::CancelAll { broker_account }) if broker_account == BROKER_ACCOUNT
    ));
    let mut exited = Vec::new();
    while let Ok(VenueRequest::ExitPosition { instrument_id, .. }) = receiver.try_recv() {
        exited.push(instrument_id);
    }
    assert_eq!(
        exited,
        [
            second_instrument.instrument_id.clone(),
            instrument().instrument_id
        ]
    );

    // The venue's exit fills flatten the ledger.
    inbox
        .push(venue(1, VenueUpdate::Fill(sell("d3", "803", 2, 9_000))))
        .expect("long exit");
    inbox
        .push(venue(
            1,
            VenueUpdate::Fill(BrokerFill {
                instrument_id: second_instrument.instrument_id.clone(),
                ..fill("d4", "804", 1)
            }),
        ))
        .expect("short exit");
    let positions = service.snapshot().expect("snapshot").positions;
    assert!(
        positions
            .iter()
            .filter(|position| position.account_id == broker().id)
            .all(|position| position.net_quantity.units() == 0)
    );
}

fn place_bracket(service: &TradingService, id: &str) -> Order {
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
            entry: request(id),
            template,
        })
        .expect("bracket");
    let crate::PlacementOutcome::BrokerProtected(order) = placement else {
        panic!("a broker bracket is held by the broker");
    };
    order
}

#[test]
fn bracket_levels_move_when_the_venue_confirms_and_end_with_the_position() {
    let (directory, service, receiver) = setup("venue-bracket-move");
    let entry = place_bracket(&service, "moved-entry");
    assert!(matches!(receiver.try_recv(), Ok(VenueRequest::Place(_))));
    let stop = |current_ticks, new_ticks| MoveBracketLevel {
        client_order_id: entry.client_order_id.clone(),
        side: BracketSide::Stop,
        current_ticks,
        new_ticks,
    };
    assert!(
        service
            .move_bracket_level(stop(8, 6))
            .expect_err("unacknowledged entry")
            .contains("not acknowledged")
    );
    service
        .demo_venue_inbox()
        .push(venue(
            1,
            order_update(report(&entry, "1201", 0), BrokerOrderState::Accepted),
        ))
        .expect("accepted");
    service
        .move_bracket_level(stop(8, 6))
        .expect("move requested");
    assert!(matches!(
        receiver.try_recv(),
        Ok(VenueRequest::MoveBracketLevel {
            entry_broker_order_id,
            side: BracketSide::Stop,
            current_ticks: 8,
            new_ticks: 6,
            ..
        }) if entry_broker_order_id == "1201"
    ));
    assert!(
        service.move_bracket_level(stop(6, 5)).is_err(),
        "the level keeps its distance until the venue confirms the move"
    );
    service
        .demo_venue_inbox()
        .push(venue(
            1,
            VenueUpdate::BracketLevelMoved {
                entry_broker_order_id: "1201".into(),
                side: BracketSide::Stop,
                from_ticks: 8,
                to_ticks: 6,
            },
        ))
        .expect("moved");
    assert!(service.move_bracket_level(stop(8, 7)).is_err());
    service.shutdown(Duration::from_secs(2)).expect("shutdown");

    // The moved level survives a restart.
    let restarted = start_service(&directory);
    let (generation, receiver) = restarted
        .attach_demo_venue(crate::tests::VENUE_CAPABILITIES)
        .expect("reattach");
    restarted
        .move_bracket_level(stop(6, 5))
        .expect("the confirmed distance is current");
    while receiver.try_recv().is_ok() {}

    // The entry fills, a released stop closes the position, and the levels end with it.
    let inbox = restarted.demo_venue_inbox();
    inbox
        .push(venue(generation, VenueUpdate::Fill(fill("d1", "1201", 1))))
        .expect("entry fill");
    restarted
        .move_bracket_level(stop(6, 5))
        .expect("levels stay while the position is open");
    inbox
        .push(venue(
            generation,
            VenueUpdate::Fill(sell("d2", "1202", 1, 8_850)),
        ))
        .expect("stop fill");
    assert!(
        restarted
            .move_bracket_level(stop(6, 5))
            .expect_err("released")
            .contains("no server-side bracket")
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
    let database = rusqlite::Connection::open(&config(&directory).database_path).expect("database");
    let levels: i64 = database
        .query_row("SELECT count(*) FROM broker_bracket_levels", [], |row| {
            row.get(0)
        })
        .expect("level count");
    assert_eq!(levels, 0);
}

#[test]
fn a_broker_account_never_blocks_global_commands_and_kill_locks_it_offline() {
    let directory = TestDirectory::new("venue-global");
    let (service, _clock) = crate::tests::routed_fixture(
        &directory,
        &instrument(),
        crate::tests::market_feed::route(),
        10_000,
        10_025,
    );
    service.register_account(broker()).expect("account");
    let simulated = TradingAccountId::try_new("aeris-sim-1").expect("simulated");
    let mut resting = market_order("sim-resting", OrderSide::Buy, 1, 1_000);
    resting.order_type = OrderType::Limit;
    resting.time_in_force = TimeInForce::GoodTillCancelled;
    resting.limit_price = Some(point(8_000, 2));
    service.place_order(resting).expect("simulated order");
    assert_eq!(service.cancel_all(None).expect("cancel all").len(), 1);

    // With no venue attached the kill switch still locks every account, broker included.
    let outcome = service
        .kill_switch(None, "test".into(), 5_000)
        .expect("nothing broker-side to cancel");
    assert_eq!(outcome.accounts_locked, 2);
    assert_eq!(outcome.cancelled, []);
    assert_eq!(outcome.fills, []);
    assert_eq!(outcome.pending_exits, []);
    assert_eq!(outcome.unpriced, []);
    assert_eq!(outcome.incomplete, None);
    let locks = service.snapshot().expect("snapshot").risk_locks;
    for account in [&simulated, &broker().id] {
        assert!(
            locks.iter().any(|lock| lock.account_id == *account),
            "{account:?} locked"
        );
    }
    let (generation, _receiver) = service
        .attach_demo_venue(crate::tests::VENUE_CAPABILITIES)
        .expect("attach");
    crate::tests::settle_reconcile(&service, generation, BROKER_ACCOUNT);
    assert!(
        service
            .place_order(request("after-kill"))
            .expect_err("locked")
            .contains("AccountLocked: account is risk-locked"),
        "a locked broker account accepts no new order"
    );
}

#[test]
fn the_kill_switch_reports_venue_requests_it_could_not_make_after_locking() {
    let (_directory, service, receiver) = setup("venue-kill-pending");
    for index in 0..OUTBOUND_CAPACITY {
        service
            .place_order(request(&format!("unacknowledged-{index}")))
            .expect("pending");
    }
    // The request queue is full, so the venue cannot be asked to cancel anything.
    let outcome = service
        .kill_switch(Some(broker().id), "test".into(), 5_000)
        .expect("the lock and simulated step still commit");
    assert_eq!(outcome.accounts_locked, 1);
    let incomplete = outcome.incomplete.expect("the failure is reported");
    assert!(
        incomplete.starts_with("accounts are locked;"),
        "{incomplete}"
    );
    assert!(incomplete.contains("were not cancelled"), "{incomplete}");
    assert!(
        service
            .snapshot()
            .expect("snapshot")
            .risk_locks
            .iter()
            .any(|lock| lock.account_id == broker().id)
    );
    // Once the venue can be asked again, one cancel-all covers every order, including
    // those the broker never acknowledged.
    while receiver.try_recv().is_ok() {}
    let outcome = service
        .flatten(crate::FlattenScope {
            account: Some(broker().id),
            instrument: None,
        })
        .expect("flatten");
    assert_eq!(outcome.incomplete, None);
    assert!(matches!(
        receiver.try_recv(),
        Ok(VenueRequest::CancelAll { broker_account }) if broker_account == BROKER_ACCOUNT
    ));
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
fn broker_day_orders_rest_as_day_and_market_orders_fill_or_cancel() {
    let (directory, service, receiver) = setup("venue-time-in-force");
    let day_limit = PlaceOrder {
        time_in_force: TimeInForce::Day,
        ..limit_request("day-limit", 9_000)
    };
    let placed = service.place_order(day_limit).expect("limit");
    assert_eq!(placed.time_in_force, TimeInForce::Day);
    // The broker reports a DAY order's session expiry; the owner schedules none.
    assert_eq!(placed.expires_at_unix_nanos, None);
    let VenueRequest::Place(sent) = receiver.try_recv().expect("place") else {
        panic!("a place request");
    };
    assert_eq!(sent.time_in_force, TimeInForce::Day);
    let market = service.place_order(request("day-market")).expect("market");
    assert_eq!(market.time_in_force, TimeInForce::ImmediateOrCancel);
    assert!(matches!(receiver.try_recv(), Ok(VenueRequest::Place(_))));

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
        service
            .modify_order(ModifyOrder {
                time_in_force: TimeInForce::GoodTillCancelled,
                ..modify.clone()
            })
            .expect_err("the broker keeps its time in force"),
        "the broker cannot change an order's time in force"
    );
    assert!(receiver.try_recv().is_err());
    assert_eq!(
        service.modify_order(modify).expect("modify").status,
        OrderStatus::PendingModify
    );
    assert!(matches!(receiver.try_recv(), Ok(VenueRequest::Amend(_))));
    let current = service
        .snapshot()
        .expect("snapshot")
        .orders
        .into_iter()
        .find(|order| order.id == placed.id)
        .expect("order");
    assert_eq!(current.time_in_force, TimeInForce::Day);
    assert_broker_orders_survive_restart(&directory, &service);
}

#[test]
fn an_off_grid_broker_amendment_is_rejected_before_it_is_recorded_or_routed() {
    let (_directory, service, receiver) = setup("venue-off-grid-amend");
    let placed = service
        .place_order(limit_request("off-grid-amend", 9_000))
        .expect("limit");
    assert!(matches!(receiver.try_recv(), Ok(VenueRequest::Place(_))));
    service
        .demo_venue_inbox()
        .push(venue(
            1,
            order_update(report(&placed, "1201", 0), BrokerOrderState::Accepted),
        ))
        .expect("accepted");
    let before = service.snapshot().expect("before");
    // 90.13 is not a multiple of the fixture's 0.25 tick.
    let error = service
        .modify_order(ModifyOrder {
            client_order_id: placed.client_order_id.clone(),
            time_in_force: TimeInForce::GoodTillCancelled,
            limit_price: Some(point(9_013, 2)),
            stop_price: None,
            modified_unix_nanos: 3_000,
            provenance: crate::tests::provenance(7, 3_000),
        })
        .expect_err("off grid");
    assert_eq!(error, aeris_trading::RejectReason::OffGrid.to_string());
    assert!(receiver.try_recv().is_err());
    assert_eq!(service.snapshot().expect("unchanged"), before);
}

#[test]
fn a_broker_bracket_sends_server_levels_at_the_template_distances() {
    let (directory, service, receiver) = setup("venue-bracket");
    let order = place_bracket(&service, "protected-entry");
    assert_eq!(order.status, OrderStatus::Pending);
    let VenueRequest::Place(sent) = receiver.try_recv().expect("place") else {
        panic!("a place request");
    };
    let level = |ticks| BracketLevel {
        quantity: point(1, 0),
        ticks,
    };
    assert_eq!(
        sent.bracket,
        Some(ServerBracket {
            targets: vec![level(16)],
            stops: vec![level(8)],
        })
    );
    assert_eq!(service.snapshot().expect("snapshot").managed_brackets, []);
    // Broker brackets stay server-side: the local group authority never records them, and
    // the levels persist with the entry order.
    let database = rusqlite::Connection::open(&config(&directory).database_path).expect("database");
    let count = |table: &str| -> i64 {
        database
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("row count")
    };
    assert_eq!(
        count("order_groups"),
        0,
        "a broker bracket creates no order-group rows"
    );
    assert_eq!(count("broker_bracket_levels"), 2);
}

fn risk_profile(daily_loss_limit: i64, max_contracts: i64) -> crate::RiskProfile {
    crate::RiskProfile {
        account_id: broker().id,
        profile_id: "broker-rules".into(),
        version: 1,
        session_start_unix_nanos: 1_500,
        session_start_realized_pnl: point(0, 2),
        daily_loss_limit: point(daily_loss_limit, 2),
        trailing_drawdown: None,
        trailing_mode: crate::TrailingDrawdownMode::EndOfDay,
        max_contracts: point(max_contracts, 0),
        consistency_max_single_trade_percent: None,
        restricted_until_unix_nanos: None,
        economic_event_rule: None,
        enabled: true,
    }
}

/// Announces the scripted broker account, so the venue can reach it.
fn observe_broker(service: &TradingService) {
    service
        .demo_venue_inbox()
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
}

#[test]
fn open_broker_positions_count_toward_the_maximum_contract_rule() {
    let (_directory, service, _receiver) = setup("venue-max-contracts");
    service
        .register_risk_profile(risk_profile(100_000, 2))
        .expect("profile");
    observe_broker(&service);
    service
        .demo_venue_inbox()
        .push(venue(1, VenueUpdate::Fill(fill("d1", "801", 2))))
        .expect("position");
    assert!(
        service
            .place_order(request("third-contract"))
            .expect_err("over the maximum")
            .contains("maximum-contract")
    );
}

#[test]
fn broker_loss_rules_count_session_deals_and_the_broker_open_pnl() {
    let (_directory, service, _receiver) = setup("venue-loss-rules");
    let mut baseline = risk_profile(5_000, 10);
    baseline.session_start_realized_pnl = point(5, 2);
    assert!(
        service
            .register_risk_profile(baseline)
            .expect_err("broker baseline")
            .contains("baseline must be zero")
    );
    service
        .register_risk_profile(risk_profile(5_000, 10))
        .expect("an unreachable account still registers");
    assert!(
        service
            .place_order(request("unknown-open-pnl"))
            .expect_err("fails closed")
            .contains("unreachable"),
    );
    assert_eq!(
        service.snapshot().expect("snapshot").risk_meters,
        [],
        "no meter while the open P&L is unknown"
    );

    observe_broker(&service);
    let inbox = service.demo_venue_inbox();
    inbox
        .push(venue(1, VenueUpdate::Fill(fill("d1", "801", 2))))
        .expect("entry");
    // A closing deal in the session realizes 0.25 points against one contract: -12.50.
    inbox
        .push(venue(1, VenueUpdate::Fill(sell("d2", "802", 1, 8_975))))
        .expect("realized");
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.risk_locks, []);
    assert_eq!(
        snapshot.risk_meters[0].current_realized_pnl,
        point(-1_250, 2)
    );
    assert_eq!(
        snapshot.risk_meters[0].daily_loss_remaining,
        point(3_750, 2)
    );

    // The venue's open P&L on the remaining contract takes the session to the limit, and
    // the account locks.
    inbox
        .push(venue(
            1,
            VenueUpdate::Position(VenuePosition {
                open_pnl: Some(point(-3_750, 2)),
                ..position(1)
            }),
        ))
        .expect("open pnl");
    let locks = service.snapshot().expect("snapshot").risk_locks;
    assert_eq!(locks.len(), 1);
    assert_eq!(locks[0].account_id, broker().id);
    assert!(locks[0].reason.contains("daily loss limit"));
}

#[test]
fn connected_broker_positions_carry_the_venue_open_pnl() {
    let (_directory, service, receiver) = setup("venue-open-pnl");
    observe_broker(&service);
    let inbox = service.demo_venue_inbox();
    inbox
        .push(venue(
            1,
            VenueUpdate::Balance {
                balance: point(100_000, 2),
            },
        ))
        .expect("balance");
    inbox
        .push(venue(1, VenueUpdate::Fill(fill("d1", "801", 2))))
        .expect("position");
    inbox
        .push(venue(
            1,
            VenueUpdate::Position(VenuePosition {
                open_pnl: Some(point(-160, 2)),
                ..position(2)
            }),
        ))
        .expect("open pnl");
    // A report whose quantity disagrees with the ledger waits for the fills that explain
    // it, and a report without open P&L keeps the last one.
    inbox
        .push(venue(
            1,
            VenueUpdate::Position(VenuePosition {
                open_pnl: Some(point(500, 2)),
                ..position(3)
            }),
        ))
        .expect("ahead of its fills");
    inbox
        .push(venue(1, VenueUpdate::Position(position(2))))
        .expect("no open pnl");
    assert!(
        receiver.try_recv().is_err(),
        "the owner never polls the venue for open P&L"
    );
    assert_eq!(
        broker_position(&service).map(|position| position.unrealized_pnl),
        Some(point(-160, 2))
    );
    let snapshot = service.snapshot().expect("snapshot");
    let pnl = snapshot
        .account_pnl
        .iter()
        .find(|pnl| pnl.account_id == broker().id)
        .expect("account pnl");
    assert_eq!(pnl.unrealized, Some(point(-160, 2)));
    assert_eq!(pnl.balance, Some(point(100_000, 2)));
    assert_eq!(
        pnl.equity,
        Some(point(99_840, 2)),
        "balance plus the open P&L"
    );

    // A new generation reaches no account yet: no open P&L, balance or equity is claimed.
    let (_generation, next) = service
        .attach_demo_venue(crate::tests::VENUE_CAPABILITIES)
        .expect("reattach");
    let snapshot = service.snapshot().expect("snapshot");
    let pnl = snapshot
        .account_pnl
        .iter()
        .find(|pnl| pnl.account_id == broker().id)
        .expect("account pnl");
    assert_eq!(
        (pnl.unrealized, pnl.balance, pnl.equity),
        (None, None, None)
    );
    assert!(matches!(
        next.try_recv(),
        Ok(VenueRequest::Reconcile { .. })
    ));
    assert!(next.try_recv().is_err());
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
            ..venue(1, observed("Broker Demo · Example 2002"))
        })
        .expect("observed");
    let accounts = service.snapshot().expect("snapshot").accounts;
    let registered = accounts
        .iter()
        .find(|account| account.broker_ref.as_deref() == Some("2002"))
        .expect("registered");
    assert_eq!(registered.id.as_str(), "broker-demo-2002");
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
    assert!(connected.contains(&TradingAccountId::try_new("broker-demo-2002").expect("id")));
    assert!(!connected.contains(&broker().id));
    let _next = service
        .attach_demo_venue(crate::tests::VENUE_CAPABILITIES)
        .expect("reattach");
    assert_eq!(
        service
            .snapshot()
            .expect("snapshot")
            .connected_broker_accounts
            .len(),
        0
    );
}

#[test]
fn venue_reports_are_brought_to_the_owner_scales_exactly() {
    let (_directory, service, _receiver) = setup("venue-scales");
    observe_broker(&service);
    let inbox = service.demo_venue_inbox();
    // A broker wire carries 90.00 as 90 and one contract at a finer quantity scale.
    inbox
        .push(venue(
            1,
            VenueUpdate::Fill(BrokerFill {
                price: point(90, 0),
                quantity: point(10, 1),
                ..fill("d1", "801", 1)
            }),
        ))
        .expect("fill");
    inbox
        .push(venue(
            1,
            VenueUpdate::Balance {
                balance: point(1_000_005, 1),
            },
        ))
        .expect("balance");
    assert_eq!(service.status().expect("status").venue_error, None);
    let position = broker_position(&service).expect("position");
    assert_eq!(
        (position.net_quantity, position.average_entry_price),
        (point(1, 0), Some(point(9_000, 2)))
    );
    let balance = service
        .snapshot()
        .expect("snapshot")
        .account_pnl
        .into_iter()
        .find(|pnl| pnl.account_id == broker().id)
        .and_then(|pnl| pnl.balance);
    assert_eq!(balance, Some(point(10_000_050, 2)));

    // A price finer than the instrument's grid is refused, never rounded.
    inbox
        .push(venue(
            1,
            VenueUpdate::Fill(BrokerFill {
                price: point(90_001, 3),
                ..fill("d2", "802", 1)
            }),
        ))
        .expect("pushed");
    let error = service.status().expect("status").venue_error;
    assert!(
        error
            .as_deref()
            .is_some_and(|error| error.contains("fill price")),
        "{error:?}"
    );
    assert_eq!(
        broker_position(&service).map(|position| position.net_quantity),
        Some(point(1, 0))
    );
}

/// Every request the owner sent so far, without waiting.
fn sent(receiver: &aeris_trading::venue_outbox::VenueRequestReceiver) -> Vec<VenueRequest> {
    std::iter::from_fn(|| receiver.try_recv().ok()).collect()
}

fn other_instrument() -> crate::TradingInstrument {
    let mut other = instrument();
    other.instrument_id =
        aeris_instruments::InstrumentId::try_new("instrument:fixture:CME:NQZ6").expect("other");
    other
}

fn latest_notice(service: &TradingService) -> String {
    service
        .snapshot()
        .expect("snapshot")
        .venue_notices
        .last()
        .expect("venue notice")
        .detail
        .clone()
}

#[test]
fn deals_first_seen_on_the_broker_take_owner_sequence_ids_that_never_collide() {
    let (directory, service, _receiver) = setup("mirror-ids");
    let inbox = service.demo_venue_inbox();
    for (index, broker_order_id) in ["900", "901", "902"].into_iter().enumerate() {
        service
            .place_order(limit_request(&format!("placed-{index}"), 9_000))
            .expect("placed before a mirrored deal");
        inbox
            .push(venue(
                1,
                VenueUpdate::Fill(fill(&format!("mirror-{index}"), broker_order_id, 1)),
            ))
            .expect("broker-originated deal");
        service
            .place_order(limit_request(&format!("after-{index}"), 9_000))
            .expect("an order placed after a mirrored deal gets a fresh id");
    }
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(service.status().expect("status").venue_error, None);
    assert_eq!(snapshot.orders.len(), 9);
    let ids = snapshot
        .orders
        .iter()
        .map(|order| order.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 9, "every order id is unique");
    assert_eq!(
        broker_position(&service).expect("position").net_quantity,
        point(3, 0)
    );
    assert_broker_orders_survive_restart(&directory, &service);
}

#[test]
fn a_deal_before_the_acknowledgement_fills_the_placed_order_instead_of_a_mirror() {
    let (directory, service, _receiver) = setup("deal-before-ack");
    let placed = service.place_order(request("early-deal")).expect("place");
    let inbox = service.demo_venue_inbox();
    inbox
        .push(venue(
            1,
            VenueUpdate::Fill(BrokerFill {
                client_order_id: Some(placed.client_order_id.as_str().into()),
                ..fill("early-1", "501", 1)
            }),
        ))
        .expect("deal before ack");
    inbox
        .push(venue(
            1,
            order_update(report(&placed, "501", 1), BrokerOrderState::Accepted),
        ))
        .expect("late ack");
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.orders.len(), 1, "no mirror of the placed order");
    assert_eq!(snapshot.orders[0].id, placed.id);
    assert_eq!(snapshot.orders[0].status, OrderStatus::Filled);
    assert_eq!(snapshot.fills.len(), 1);
    assert_eq!(snapshot.fills[0].order_id, placed.id);
    assert_eq!(
        broker_position(&service).expect("position").net_quantity,
        point(1, 0)
    );
    // The binding is durable: a replay of the deal after a restart changes nothing.
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = start_service(&directory);
    let (generation, _receiver) = restarted
        .attach_demo_venue(crate::tests::VENUE_CAPABILITIES)
        .expect("reattach");
    restarted
        .demo_venue_inbox()
        .push(venue(
            generation,
            VenueUpdate::Fill(fill("early-1", "501", 1)),
        ))
        .expect("replayed deal");
    let restored = restarted.snapshot().expect("restored");
    assert_eq!(restored.orders.len(), 1);
    assert_eq!(restored.fills.len(), 1);
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

#[test]
fn new_orders_wait_for_the_reconcile_of_each_venue_generation() {
    let (_directory, service, _receiver) = setup("reconcile-gate");
    service.place_order(request("ready")).expect("reconciled");
    let (generation, receiver) = service
        .attach_demo_venue(crate::tests::VENUE_CAPABILITIES)
        .expect("reattach");
    let refused = service
        .place_order(request("before-reconcile"))
        .expect_err("not reconciled in this generation");
    assert!(refused.contains("being reconciled"), "{refused}");
    assert!(matches!(
        sent(&receiver).as_slice(),
        [VenueRequest::Reconcile { .. }]
    ));
    crate::tests::settle_reconcile(&service, generation, BROKER_ACCOUNT);
    service
        .place_order(request("after-reconcile"))
        .expect("reconciled again");
    // An unanswered request makes the venue state uncertain: the account is reconciled
    // again and takes no new orders until that reconcile settles it.
    service
        .demo_venue_inbox()
        .push(venue(
            generation,
            VenueUpdate::OperationFailed {
                operation: aeris_trading::venue::VenueOperation::CancelAll,
                failure: aeris_trading::venue::OperationFailure::Unanswered,
                reason: "no reply within 30 seconds".into(),
            },
        ))
        .expect("unanswered");
    assert!(latest_notice(&service).contains("did not answer"));
    assert!(matches!(
        sent(&receiver).as_slice(),
        [VenueRequest::Place(_), VenueRequest::Reconcile { .. }]
    ));
    assert!(
        service
            .place_order(request("while-uncertain"))
            .expect_err("uncertain")
            .contains("being reconciled")
    );
    crate::tests::settle_reconcile(&service, generation, BROKER_ACCOUNT);
    service
        .place_order(request("settled"))
        .expect("settled again");
}

#[test]
fn a_refused_exit_is_reported_to_the_user() {
    let (_directory, service, _receiver) = setup("refused-exit");
    service
        .demo_venue_inbox()
        .push(venue(
            1,
            VenueUpdate::OperationFailed {
                operation: aeris_trading::venue::VenueOperation::ExitPosition {
                    instrument_id: instrument().instrument_id,
                },
                failure: aeris_trading::venue::OperationFailure::Refused,
                reason: "no position".into(),
            },
        ))
        .expect("refused exit");
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.venue_notices.len(), 1);
    assert_eq!(snapshot.venue_notices[0].account_id, broker().id);
    assert!(snapshot.venue_notices[0].detail.contains("refused"));
    assert_eq!(service.status().expect("status").venue_error, None);
}

#[test]
fn an_unregistered_contract_blocks_new_orders_until_a_reconcile_accounts_it() {
    let (directory, service, _receiver) = setup("unresolved-contract");
    let other = other_instrument();
    let unaccounted = BrokerFill {
        instrument_id: other.instrument_id.clone(),
        executed_unix_nanos: 2_000,
        ..fill("other-1", "950", 1)
    };
    service
        .demo_venue_inbox()
        .push(venue(1, VenueUpdate::Fill(unaccounted.clone())))
        .expect("deal in an unregistered contract");
    assert_eq!(service.status().expect("status").venue_error, None);
    assert!(latest_notice(&service).contains("cannot be accounted"));
    let blocked = service
        .place_order(request("while-unresolved"))
        .expect_err("blocked");
    assert!(blocked.contains("blocked"), "{blocked}");
    // Flatten stays available.
    service
        .flatten(crate::FlattenScope {
            account: Some(broker().id),
            instrument: None,
        })
        .expect("flatten while blocked");

    // The unaccounted deal survives a restart and floors the next replay.
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = start_service(&directory);
    let (generation, receiver) = restarted
        .attach_demo_venue(crate::tests::VENUE_CAPABILITIES)
        .expect("reattach");
    assert!(matches!(
        sent(&receiver).as_slice(),
        [VenueRequest::Reconcile {
            deals_since_unix_nanos: Some(2_000),
            ..
        }]
    ));
    crate::tests::settle_reconcile(&restarted, generation, BROKER_ACCOUNT);
    assert!(
        restarted
            .place_order(request("still-unresolved"))
            .expect_err("still blocked")
            .contains("blocked")
    );

    // Registering the contract reconciles the account; the replayed deal and the snapshot
    // account it, and new orders are available again.
    restarted
        .register_instrument(other.clone())
        .expect("register the contract");
    assert!(matches!(
        sent(&receiver).as_slice(),
        [VenueRequest::Reconcile {
            deals_since_unix_nanos: Some(2_000),
            ..
        }]
    ));
    let inbox = restarted.demo_venue_inbox();
    inbox
        .push(venue(generation, VenueUpdate::Fill(unaccounted)))
        .expect("replayed deal");
    inbox
        .push(venue(
            generation,
            VenueUpdate::Snapshot(VenueSnapshot {
                orders: Vec::new(),
                positions: vec![VenuePosition {
                    instrument_id: other.instrument_id.clone(),
                    ..position(1)
                }],
            }),
        ))
        .expect("reconcile snapshot");
    assert!(latest_notice(&restarted).contains("new orders are available again"));
    let accounted = restarted
        .snapshot()
        .expect("snapshot")
        .positions
        .into_iter()
        .find(|position| position.instrument_id == other.instrument_id)
        .expect("accounted position");
    assert_eq!(accounted.net_quantity, point(1, 0));
    let feed = restarted
        .attach_market_feed(std::sync::Arc::new(
            crate::tests::market_feed::RecordedInterest::default(),
        ))
        .expect("market feed");
    crate::tests::bootstrap_live_feed(&feed, &instrument(), crate::tests::market_feed::route());
    crate::tests::push_live_quote(&feed, &instrument(), 1, 10_000, 10_025, 2);
    restarted
        .place_order(limit_request("accounted", 9_000))
        .expect("new orders resume");
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

#[test]
fn a_reverse_opens_the_opposite_position_only_after_its_exit_flattens() {
    let (_directory, service, receiver) = setup("managed-reverse");
    let inbox = service.demo_venue_inbox();
    inbox
        .push(venue(1, VenueUpdate::Fill(fill("long-1", "700", 1))))
        .expect("long position");
    sent(&receiver);
    let outcome = service
        .reverse_position_at(broker().id, instrument().instrument_id)
        .expect("reverse requested");
    assert_eq!(outcome.pending_exits.len(), 1);
    assert!(
        matches!(
            sent(&receiver).as_slice(),
            [VenueRequest::ExitPosition { .. }]
        ),
        "only the exit is sent while the position is open"
    );
    assert!(
        service
            .reverse_position_at(broker().id, instrument().instrument_id)
            .expect_err("one reverse at a time")
            .contains("already waiting")
    );
    inbox
        .push(venue(1, VenueUpdate::Fill(sell("exit-1", "701", 1, 9_000))))
        .expect("exit fill");
    service.status().expect("owner applies the exit fill");
    let requests = sent(&receiver);
    let [VenueRequest::Place(entry)] = requests.as_slice() else {
        panic!("the opening order follows the flat position: {requests:?}");
    };
    assert_eq!(
        (entry.side, entry.order_type, entry.quantity),
        (OrderSide::Sell, OrderType::Market, point(1, 0))
    );
}

#[test]
fn a_reverse_whose_exit_fails_never_opens_the_opposite_position() {
    let (_directory, service, receiver) = setup("failed-reverse");
    let inbox = service.demo_venue_inbox();
    inbox
        .push(venue(1, VenueUpdate::Fill(fill("long-1", "700", 1))))
        .expect("long position");
    service
        .reverse_position_at(broker().id, instrument().instrument_id)
        .expect("reverse requested");
    sent(&receiver);
    inbox
        .push(venue(
            1,
            VenueUpdate::OperationFailed {
                operation: aeris_trading::venue::VenueOperation::ExitPosition {
                    instrument_id: instrument().instrument_id,
                },
                failure: aeris_trading::venue::OperationFailure::Refused,
                reason: "exit refused".into(),
            },
        ))
        .expect("refused exit");
    // A later flat position (for example a manual close) places nothing.
    inbox
        .push(venue(
            1,
            VenueUpdate::Fill(sell("manual-1", "702", 1, 9_000)),
        ))
        .expect("manual close");
    service.status().expect("owner applies the close");
    assert_eq!(sent(&receiver), Vec::new());
    let notices = service.snapshot().expect("snapshot").venue_notices;
    assert!(
        notices
            .iter()
            .any(|notice| notice.detail.contains("reverse was cancelled"))
    );
}

#[test]
fn a_reverse_waiting_at_shutdown_is_reported_at_the_next_start_and_never_completed() {
    let (directory, service, _receiver) = setup("interrupted-reverse");
    service
        .demo_venue_inbox()
        .push(venue(1, VenueUpdate::Fill(fill("long-1", "700", 1))))
        .expect("long position");
    service
        .reverse_position_at(broker().id, instrument().instrument_id)
        .expect("reverse requested");
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = start_service(&directory);
    let notices = restarted.snapshot().expect("restored").recovery_notices;
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].kind,
        crate::RecoveryNoticeKind::InterruptedReverse
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
    let again = start_service(&directory);
    assert_eq!(
        again.snapshot().expect("reported once").recovery_notices,
        []
    );
    again.shutdown(Duration::from_secs(2)).expect("shutdown");
}
