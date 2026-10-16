use crate::{
    AccountEnvironment, FixedPoint, OrderSide, OrderType, PlaceOrder, TimeInForce, TradingAccount,
    TradingAccountId,
    tests::{TestDirectory, instrument, market_order, start_service},
};
use aeris_instruments::{InstrumentDecimal, PriceGrid};
use aeris_trading::{ClientOrderId, RejectReason, venue::VenueRequest};
use std::time::Duration;

fn shared_refusals() -> Vec<(&'static str, PlaceOrder, RejectReason)> {
    let mut base = market_order("parity-base", OrderSide::Buy, 1, 1_000);
    base.quantity = FixedPoint::try_new(2, 0).expect("increment quantity");
    let price = |units| FixedPoint::try_new(units, 2).expect("price");
    vec![
        (
            "stop_ioc",
            PlaceOrder {
                order_type: OrderType::Stop,
                time_in_force: TimeInForce::ImmediateOrCancel,
                stop_price: Some(price(10_100)),
                ..base.clone()
            },
            RejectReason::TifNotSupported,
        ),
        (
            "stop_limit_fok",
            PlaceOrder {
                order_type: OrderType::StopLimit,
                time_in_force: TimeInForce::FillOrKill,
                stop_price: Some(price(10_100)),
                limit_price: Some(price(10_125)),
                ..base.clone()
            },
            RejectReason::TifNotSupported,
        ),
        (
            "off_grid",
            PlaceOrder {
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GoodTillCancelled,
                quantity: FixedPoint::try_new(2, 0).expect("quantity"),
                limit_price: Some(price(9_010)),
                ..base.clone()
            },
            RejectReason::OffGrid,
        ),
        (
            "increment",
            PlaceOrder {
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GoodTillCancelled,
                quantity: FixedPoint::try_new(1, 0).expect("off-increment quantity"),
                limit_price: Some(price(9_000)),
                ..base
            },
            RejectReason::InvalidQuantity,
        ),
    ]
}

#[test]
fn simulated_and_broker_routes_refuse_the_same_invalid_instructions_without_facts() {
    let directory = TestDirectory::new("admission-route-parity");
    let service = start_service(&directory);
    let mut terms = instrument();
    terms.contract.price_grid = PriceGrid::Fixed(InstrumentDecimal::try_new(25, 2).expect("tick"));
    terms.contract.order_quantity_increment =
        Some(InstrumentDecimal::try_new(2, 0).expect("increment"));
    service.register_instrument(terms).expect("terms");
    let broker_id = TradingAccountId::try_new("admission-demo").expect("broker id");
    service
        .register_account(TradingAccount {
            id: broker_id.clone(),
            display_name: "Scripted demo".into(),
            environment: AccountEnvironment::Demo,
            venue_id: "ctrader".into(),
            broker_ref: Some("fixture-demo".into()),
            currency: "USD".into(),
            currency_scale: 2,
            starting_equity: None,
        })
        .expect("demo account");
    let (_generation, outbound) = service.attach_demo_venue().expect("venue");
    // Attachment starts by requesting reconciliation for existing broker accounts.
    while outbound.try_recv().is_ok() {}
    let before = service.snapshot().expect("before");
    let price = |units| FixedPoint::try_new(units, 2).expect("price");
    for (case, request, reason) in shared_refusals() {
        for (route, account_id) in [
            (
                "simulated",
                TradingAccountId::try_new("aeris-sim-1").expect("practice id"),
            ),
            ("broker", broker_id.clone()),
        ] {
            let request = PlaceOrder {
                client_order_id: ClientOrderId::try_new(format!("{case}-{route}"))
                    .expect("client id"),
                account_id,
                ..request.clone()
            };
            assert_eq!(
                service.place_order(request).expect_err("refused"),
                reason.to_string(),
                "{case} on {route}"
            );
            assert_eq!(
                service.snapshot().expect("no facts"),
                before,
                "{case} on {route}"
            );
            assert!(
                outbound.try_recv().is_err(),
                "{case} on {route} cannot issue a venue request"
            );
        }
    }
    let mut too_long = market_order(&"b".repeat(51), OrderSide::Buy, 1, 1_000);
    too_long.account_id = broker_id.clone();
    too_long.quantity = FixedPoint::try_new(2, 0).expect("quantity");
    assert_eq!(
        service
            .place_order(too_long)
            .expect_err("broker id too long"),
        RejectReason::ClientIdTooLong.to_string()
    );
    assert_eq!(service.snapshot().expect("no broker fact"), before);
    assert!(outbound.try_recv().is_err());
    for account_id in [
        TradingAccountId::try_new("aeris-sim-1").expect("practice id"),
        broker_id,
    ] {
        service
            .lock_account(account_id.clone(), "parity lock".into(), 1_000)
            .expect("lock");
        let locked = service.snapshot().expect("locked snapshot");
        let mut request = market_order("locked-parity", OrderSide::Buy, 1, 1_000);
        request.account_id = account_id;
        request.quantity = FixedPoint::try_new(2, 0).expect("quantity");
        request.order_type = OrderType::Limit;
        request.time_in_force = TimeInForce::GoodTillCancelled;
        request.limit_price = Some(price(9_000));
        assert!(
            service
                .place_order(request)
                .expect_err("account locked")
                .contains(&RejectReason::AccountLocked.to_string())
        );
        assert_eq!(service.snapshot().expect("no order fact"), locked);
        assert!(outbound.try_recv().is_err());
    }
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn broker_day_entry_maps_to_gtc_before_the_venue_request() {
    let directory = TestDirectory::new("broker-day-capabilities");
    let service = start_service(&directory);
    service.register_instrument(instrument()).expect("terms");
    let broker_id = TradingAccountId::try_new("day-demo").expect("broker id");
    service
        .register_account(TradingAccount {
            id: broker_id.clone(),
            display_name: "Scripted demo".into(),
            environment: AccountEnvironment::Demo,
            venue_id: "ctrader".into(),
            broker_ref: Some("fixture-demo".into()),
            currency: "USD".into(),
            currency_scale: 2,
            starting_equity: None,
        })
        .expect("demo account");
    let (_generation, outbound) = service.attach_demo_venue().expect("venue");
    while outbound.try_recv().is_ok() {}
    let mut entry = market_order("broker-day", OrderSide::Buy, 1, 1_000);
    entry.account_id = broker_id;
    entry.order_type = OrderType::Limit;
    entry.time_in_force = TimeInForce::Day;
    entry.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("price"));
    let accepted = service.place_order(entry).expect("broker accepts DAY");
    assert_eq!(accepted.time_in_force, TimeInForce::GoodTillCancelled);
    let VenueRequest::Place(sent) = outbound.try_recv().expect("place request") else {
        panic!("expected broker placement");
    };
    assert_eq!(sent.time_in_force, TimeInForce::GoodTillCancelled);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}
