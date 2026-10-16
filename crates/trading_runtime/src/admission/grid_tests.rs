use super::*;
use aeris_instruments::InstrumentDecimal;
use aeris_trading::{OrderSide, OrderType, TimeInForce};

#[test]
fn venue_capabilities_resolve_day_and_protection_policy() {
    let simulated = VenueCapabilities::for_route(VenueRoute::Simulated);
    let broker = VenueCapabilities::for_route(VenueRoute::CtraderDemo);
    assert_eq!(
        simulated.effective_tif(OrderType::Limit, TimeInForce::Day),
        TimeInForce::Day
    );
    assert_eq!(
        broker.effective_tif(OrderType::Limit, TimeInForce::Day),
        TimeInForce::GoodTillCancelled
    );
    assert_eq!(
        broker.effective_tif(OrderType::Market, TimeInForce::Day),
        TimeInForce::ImmediateOrCancel
    );
    assert_eq!(broker.client_id_limit, Some(50));
    assert!(broker.native_sltp());
    assert!(!broker.managed_trailing());
    assert!(simulated.managed_trailing());
    assert!(!simulated.native_sltp());
}

#[test]
fn stop_ioc_and_fok_are_rejected_for_both_venues() {
    for order_type in [OrderType::Stop, OrderType::StopLimit] {
        for tif in [TimeInForce::ImmediateOrCancel, TimeInForce::FillOrKill] {
            assert_eq!(
                validate_time_in_force(order_type, tif),
                Err(RejectReason::TifNotSupported),
            );
        }
        assert_eq!(validate_time_in_force(order_type, TimeInForce::Day), Ok(()));
    }
}

#[test]
fn stop_triggers_must_be_strictly_outside_the_touch() {
    let mut order = crate::tests::market_order("trigger-side", OrderSide::Buy, 1, 1_000);
    order.order_type = OrderType::Stop;
    order.stop_price = Some(FixedPoint::try_new(10_050, 2).expect("trigger"));
    let bid = FixedPoint::try_new(10_000, 2).expect("bid");
    let ask = FixedPoint::try_new(10_050, 2).expect("ask");
    assert_eq!(
        validate_trigger_side(&order, bid, ask),
        Err(RejectReason::WrongSideTrigger)
    );
    order.stop_price = Some(FixedPoint::try_new(10_075, 2).expect("trigger"));
    assert_eq!(validate_trigger_side(&order, bid, ask), Ok(()));
    order.side = OrderSide::Sell;
    order.order_type = OrderType::StopLimit;
    order.stop_price = Some(bid);
    assert_eq!(
        validate_trigger_side(&order, bid, ask),
        Err(RejectReason::WrongSideTrigger)
    );
    order.stop_price = Some(FixedPoint::try_new(9_975, 2).expect("trigger"));
    assert_eq!(validate_trigger_side(&order, bid, ask), Ok(()));
}

#[test]
fn fixed_and_significant_figure_grids_reject_off_grid_prices() {
    let fixed = PriceGrid::Fixed(InstrumentDecimal::try_new(25, 2).expect("tick"));
    for (units, accepted) in [(50_010, false), (50_025, true)] {
        assert_eq!(
            validate_price_grid(fixed, FixedPoint::try_new(units, 2).expect("price")),
            if accepted {
                Ok(())
            } else {
                Err(RejectReason::OffGrid)
            },
        );
    }
    let significant = PriceGrid::SignificantFigures {
        figures: 5,
        max_decimals: 5,
    };
    for (units, scale, accepted) in [
        (800_005, 1, false),
        (80_001, 0, true),
        (8_000_050, 2, false),
        (1_234, 5, true),
    ] {
        assert_eq!(
            validate_price_grid(
                significant,
                FixedPoint::try_new(units, scale).expect("price")
            ),
            if accepted {
                Ok(())
            } else {
                Err(RejectReason::OffGrid)
            },
        );
    }
    assert_eq!(
        validate_price_grid(
            PriceGrid::Unknown,
            FixedPoint::try_new(800_005, 1).expect("price")
        ),
        Ok(())
    );
}

#[test]
fn shared_admission_rejects_quantity_capacity_and_broker_client_length() {
    let mut instrument = crate::tests::instrument();
    let mut order = crate::tests::market_order("capacity", OrderSide::Buy, 1, 1_000);
    for route in [VenueRoute::Simulated, VenueRoute::CtraderDemo] {
        assert_eq!(
            validate_common_order(&instrument, &order, route, MAXIMUM_OPEN_ORDERS, 0),
            Err(RejectReason::CapacityExceeded)
        );
        order.quantity = FixedPoint::try_new(0, order.quantity.scale()).expect("quantity");
        assert_eq!(
            validate_common_order(&instrument, &order, route, 0, 0),
            Err(RejectReason::InvalidQuantity)
        );
        order.quantity = FixedPoint::try_new(1, order.quantity.scale()).expect("quantity");
        assert_eq!(
            validate_common_order(&instrument, &order, route, 0, 0),
            Ok(())
        );
    }
    order.client_order_id = aeris_trading::ClientOrderId::try_new("x".repeat(51)).expect("id");
    assert_eq!(
        validate_common_order(&instrument, &order, VenueRoute::CtraderDemo, 0, 0),
        Err(RejectReason::ClientIdTooLong)
    );
    assert_eq!(
        validate_common_order(&instrument, &order, VenueRoute::Simulated, 0, 0),
        Ok(())
    );
    instrument.contract.order_quantity_increment =
        Some(InstrumentDecimal::try_new(2, 0).expect("increment"));
    order.client_order_id = aeris_trading::ClientOrderId::try_new("increment").expect("id");
    for route in [VenueRoute::Simulated, VenueRoute::CtraderDemo] {
        assert_eq!(
            validate_common_order(&instrument, &order, route, 0, 0),
            Err(RejectReason::InvalidQuantity)
        );
    }
    order.quantity = FixedPoint::try_new(1, 1).expect("wrong quantity scale");
    for route in [VenueRoute::Simulated, VenueRoute::CtraderDemo] {
        assert_eq!(
            validate_common_order(&instrument, &order, route, 0, 0),
            Err(RejectReason::ScaleMismatch)
        );
    }
}
