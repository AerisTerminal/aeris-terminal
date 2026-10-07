use super::*;
use crate::tests::{TestDirectory, config, instrument, market_order, start_service};
use crate::{TradingService, VenueRoute};
use aeris_trading::{
    AccountEnvironment, ClientOrderId, FixedPoint, OrderSide, OrderStatus, OrderType, TimeInForce,
    TradingAccount, TradingAccountId,
};
use std::{
    sync::{Barrier, mpsc},
    thread,
};

fn broker() -> TradingAccount {
    TradingAccount {
        id: TradingAccountId::try_new("scripted-demo").expect("account"),
        display_name: "scripted demo".into(),
        environment: AccountEnvironment::Demo,
        venue_id: "ctrader".into(),
        broker_ref: Some("fixture".into()),
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
    let receiver = service.attach_demo_venue(1).expect("attach");
    (directory, service, receiver)
}

fn request(id: &str) -> PlaceOrder {
    let mut request = market_order(id, OrderSide::Buy, 1, 1_000);
    request.account_id = broker().id;
    request
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
    assert!(
        !service
            .snapshot()
            .expect("snapshot")
            .orders
            .iter()
            .any(|order| order.client_order_id.as_str() == "over-capacity")
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
    let placed = service.place_order(request("amend-target")).expect("place");
    service
        .demo_venue_inbox()
        .push(VenueEvent {
            session_generation: 1,
            client_order_id: placed.client_order_id.clone(),
            update: VenueUpdate::Accepted,
            observed_unix_nanos: 2_000,
        })
        .expect("accepted");
    assert_eq!(
        service.snapshot().expect("snapshot").orders[0].status,
        OrderStatus::Working
    );
    for index in 1..OUTBOUND_CAPACITY {
        service
            .place_order(request(&format!("queued-{index}")))
            .expect("queue place");
    }
    let modify = crate::ModifyOrder {
        client_order_id: placed.client_order_id.clone(),
        time_in_force: TimeInForce::Day,
        limit_price: None,
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
            .cancel_order(placed.client_order_id)
            .expect_err("queue full"),
        "cTrader venue outbound queue is full"
    );
    assert_eq!(
        service.snapshot().expect("snapshot").orders[0].status,
        OrderStatus::Working
    );
}

#[test]
fn stale_events_are_ignored_and_owner_assigns_provenance() {
    let (_directory, service, receiver) = setup("venue-generation");
    let order = service
        .place_order(request("generation-order"))
        .expect("place");
    assert_eq!(order.id.as_str(), "ct-order-1");
    assert_eq!(
        VenueRoute::CtraderDemo,
        VenueRoute::from_account(&broker()).expect("route")
    );
    assert!(matches!(
        receiver.recv().expect("request"),
        VenueRequest::Place(_)
    ));
    let _retired = service.attach_demo_venue(2).expect("reconnect");
    for generation in [1, 2, 1, 2] {
        service
            .runtime
            .venue_inbox
            .push(VenueEvent {
                session_generation: generation,
                client_order_id: order.client_order_id.clone(),
                update: VenueUpdate::Accepted,
                observed_unix_nanos: 2_000,
            })
            .expect("event");
    }
    let snapshot = service.snapshot().expect("snapshot");
    let events: Vec<_> = snapshot
        .order_events
        .iter()
        .rev()
        .filter(|event| event.order_id == order.id)
        .collect();
    assert_eq!(events.len(), 3, "{:?}", snapshot.market_observation_error);
    assert_eq!(events[1].provenance.session_generation, 2);
    assert_eq!(events[1].provenance.source_sequence, 1);
    assert_eq!(events[1].provenance.venue_id, "ctrader");
    assert_eq!(events[1].provenance.provider_id, "ctrader");
    assert_eq!(events[2].provenance.source_sequence, 2);
    assert_eq!(
        snapshot
            .orders
            .iter()
            .find(|candidate| candidate.id == order.id)
            .expect("order")
            .status,
        OrderStatus::Working
    );
}

#[test]
fn bounded_inbox_blocks_reader_then_applies_every_event_in_order() {
    let (_directory, service, _receiver) = setup("venue-burst");
    let order = service.place_order(request("burst-order")).expect("place");
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
    let sender = thread::spawn(move || {
        for index in 0..(INBOX_CAPACITY + 77) {
            inbox
                .push(VenueEvent {
                    session_generation: 1,
                    client_order_id: order.client_order_id.clone(),
                    update: VenueUpdate::Accepted,
                    observed_unix_nanos: 2_000 + i64::try_from(index).expect("bounded burst"),
                })
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
    let snapshot = service.snapshot().expect("snapshot");
    let applied: Vec<_> = snapshot
        .order_events
        .iter()
        .rev()
        .filter(|event| event.order_id.as_str() == "ct-order-1")
        .collect();
    assert_eq!(
        applied.len(),
        INBOX_CAPACITY + 78,
        "{:?}",
        snapshot.market_observation_error
    );
    assert_eq!(
        applied.last().expect("last").provenance.source_sequence,
        (INBOX_CAPACITY + 77) as u64
    );
}

#[test]
fn accepted_modified_replaced_cancel_rejected_and_cancelled_are_owner_transitions() {
    let (_directory, service, receiver) = setup("venue-lifecycle");
    let mut place = request("lifecycle");
    place.order_type = OrderType::Limit;
    place.time_in_force = TimeInForce::GoodTillCancelled;
    place.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("price"));
    let pending = service.place_order(place).expect("pending");
    assert!(matches!(
        receiver.recv().expect("place"),
        VenueRequest::Place(_)
    ));
    let event = |update| VenueEvent {
        session_generation: 1,
        client_order_id: pending.client_order_id.clone(),
        update,
        observed_unix_nanos: 2_000,
    };
    let inbox = service.demo_venue_inbox();
    inbox.push(event(VenueUpdate::Accepted)).expect("accepted");
    assert_eq!(
        service.snapshot().expect("snapshot").orders[0].status,
        OrderStatus::Working
    );
    let change = ModifyOrder {
        client_order_id: pending.client_order_id.clone(),
        time_in_force: TimeInForce::GoodTillCancelled,
        limit_price: Some(FixedPoint::try_new(9_025, 2).expect("price")),
        stop_price: None,
        modified_unix_nanos: 3_000,
        provenance: crate::tests::provenance(99, 3_000),
    };
    assert_eq!(
        service.modify_order(change).expect("modify").status,
        OrderStatus::PendingModify
    );
    assert!(matches!(
        receiver.recv().expect("modify"),
        VenueRequest::Modify(_)
    ));
    inbox.push(event(VenueUpdate::Replaced)).expect("replaced");
    let replaced = &service.snapshot().expect("snapshot").orders[0];
    assert_eq!(replaced.status, OrderStatus::Working);
    assert_eq!(replaced.limit_price.expect("price").units(), 9_025);
    assert_eq!(
        service
            .cancel_order(pending.client_order_id.clone())
            .expect("cancel")
            .status,
        OrderStatus::PendingCancel
    );
    assert!(matches!(
        receiver.recv().expect("cancel"),
        VenueRequest::Cancel(_)
    ));
    inbox
        .push(event(VenueUpdate::CancelRejected("broker refused".into())))
        .expect("reject");
    assert_eq!(
        service.snapshot().expect("snapshot").orders[0].status,
        OrderStatus::Working
    );
    service
        .cancel_order(pending.client_order_id.clone())
        .expect("retry cancel");
    inbox
        .push(event(VenueUpdate::Cancelled))
        .expect("cancelled");
    assert_eq!(
        service.snapshot().expect("snapshot").orders[0].status,
        OrderStatus::Cancelled
    );
    inbox
        .push(event(VenueUpdate::Accepted))
        .expect("late accepted");
    assert_eq!(
        service.snapshot().expect("snapshot").orders[0].status,
        OrderStatus::Cancelled
    );
}

#[test]
fn stalled_inbox_abandons_generation_and_stop_wakes_blocked_reader() {
    let (commands, _receiver) = mpsc::sync_channel(2);
    let stopping = Arc::new(AtomicBool::new(false));
    let inbox =
        VenueInbox::with_blocked_limit(commands, Arc::clone(&stopping), Duration::from_millis(60));
    let make_event = || VenueEvent {
        session_generation: 1,
        client_order_id: ClientOrderId::try_new("overflow").expect("id"),
        update: VenueUpdate::Accepted,
        observed_unix_nanos: 2_000,
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
fn rejection_and_expiration_are_terminal_and_ignore_late_updates() {
    for (label, terminal) in [
        (
            "venue-rejected",
            VenueUpdate::Rejected("broker declined".into()),
        ),
        ("venue-expired", VenueUpdate::Expired),
    ] {
        let (_directory, service, _receiver) = setup(label);
        let order = service.place_order(request(label)).expect("pending");
        let inbox = service.demo_venue_inbox();
        inbox
            .push(VenueEvent {
                session_generation: 1,
                client_order_id: order.client_order_id.clone(),
                update: terminal,
                observed_unix_nanos: 2_000,
            })
            .expect("terminal");
        let snapshot = service.snapshot().expect("snapshot");
        assert!(
            !snapshot
                .orders
                .iter()
                .find(|candidate| candidate.id == order.id)
                .expect("order")
                .status
                .is_open()
        );
        assert!(
            snapshot.order_events[0]
                .detail
                .as_ref()
                .is_some_and(|detail| detail == "broker declined" || detail == "expired")
        );
        inbox
            .push(VenueEvent {
                session_generation: 1,
                client_order_id: order.client_order_id.clone(),
                update: VenueUpdate::Accepted,
                observed_unix_nanos: 3_000,
            })
            .expect("late");
        assert_eq!(service.snapshot().expect("snapshot").order_events.len(), 2);
    }
}
