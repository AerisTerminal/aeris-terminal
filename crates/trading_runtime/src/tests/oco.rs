//! Explicit one-cancels-the-other pairs: both members rest as ordinary working orders under
//! one group, a manual cancel of one member leaves its sibling working, and the group
//! retires once no member remains open.

use super::*;
use rusqlite::Connection;
use std::time::Duration;

fn oco_member(client_id: &str, side: OrderSide, stop: i64, sequence: u64, time: i64) -> PlaceOrder {
    let mut order = market_order(client_id, side, sequence, time);
    order.order_type = OrderType::Stop;
    order.time_in_force = TimeInForce::GoodTillCancelled;
    order.stop_price = Some(FixedPoint::try_new(stop, 2).expect("stop"));
    order
}

fn oco_pair() -> PlaceOco {
    PlaceOco {
        first: oco_member("oco-buy", OrderSide::Buy, 10_100, 2, 2_000),
        second: oco_member("oco-sell", OrderSide::Sell, 9_900, 3, 3_000),
    }
}

fn oco_group_status(directory: &TestDirectory) -> String {
    let database = Connection::open(directory.0.join("trading.sqlite3")).expect("group reader");
    database
        .query_row(
            "SELECT status FROM order_groups WHERE kind='oco'",
            [],
            |row| row.get(0),
        )
        .expect("oco group row")
}

#[test]
fn oco_pair_rests_as_two_working_members_of_one_active_group() {
    let directory = TestDirectory::new("oco-placement");
    let (service, _clock) = routed_fixture(
        &directory,
        &instrument(),
        market_feed::route(),
        9_950,
        10_000,
    );
    let [first, second] = service.place_oco(oco_pair()).expect("pair accepted");
    assert_eq!(
        (first.status, second.status),
        (OrderStatus::Working, OrderStatus::Working),
        "both members rest as working orders"
    );
    assert_eq!(oco_group_status(&directory), "active");

    let mut same_id = oco_pair();
    same_id.second.client_order_id = same_id.first.client_order_id.clone();
    assert_eq!(
        service.place_oco(same_id).expect_err("distinct ids"),
        "OCO members need distinct client order ids"
    );
    let mut other_instrument = oco_pair();
    other_instrument.first.client_order_id =
        ClientOrderId::try_new("oco-other-buy").expect("client id");
    other_instrument.second.client_order_id =
        ClientOrderId::try_new("oco-other-sell").expect("client id");
    other_instrument.second.instrument_id =
        InstrumentId::try_new("instrument:fixture:crypto:BTC").expect("instrument");
    assert_eq!(
        service.place_oco(other_instrument).expect_err("one scope"),
        "OCO members share one account and instrument"
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn cancelling_one_oco_member_leaves_the_sibling_working() {
    let directory = TestDirectory::new("oco-manual-cancel");
    let (service, _clock) = routed_fixture(
        &directory,
        &instrument(),
        market_feed::route(),
        9_950,
        10_000,
    );
    let [first, second] = service.place_oco(oco_pair()).expect("pair accepted");
    service
        .cancel_order(first.client_order_id.clone())
        .expect("cancel first member");
    let snapshot = service.snapshot().expect("snapshot");
    let sibling = snapshot
        .orders
        .iter()
        .find(|order| order.client_order_id == second.client_order_id)
        .expect("sibling order");
    assert_eq!(
        sibling.status,
        OrderStatus::Working,
        "a manual member cancel is not a fill: the sibling keeps working"
    );
    assert_eq!(oco_group_status(&directory), "active");
    service
        .cancel_order(second.client_order_id.clone())
        .expect("cancel sibling");
    assert_eq!(
        oco_group_status(&directory),
        "cancelled",
        "the group retires once no member remains open"
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}
