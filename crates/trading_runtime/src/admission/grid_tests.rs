use super::*;
use aeris_instruments::InstrumentDecimal;
use aeris_trading::{OrderSide, OrderType, TimeInForce};

#[test]
fn buying_power_hook_is_explicit_and_unrestricted_without_margin() {
    let command = crate::tests::market_order("no-margin", OrderSide::Buy, 1, 1_000);
    let before = command.clone();
    BuyingPower::check(&command);
    assert_eq!(command, before);
}

#[test]
fn unknown_grid_admission_attaches_typed_warning() {
    let mut instrument = crate::tests::instrument();
    instrument.contract.price_grid = PriceGrid::Unknown;
    assert_eq!(
        price_grid_warning(&instrument),
        Some(aeris_trading::RiskWarning::UnverifiedPriceGrid)
    );
    instrument.contract.price_grid =
        PriceGrid::Fixed(InstrumentDecimal::try_new(25, 2).expect("tick"));
    assert_eq!(price_grid_warning(&instrument), None);
}

#[test]
fn practice_currency_is_checked_in_shared_admission_broker_is_exempt() {
    let instrument = crate::tests::instrument();
    assert_eq!(
        validate_currency(&instrument, "EUR", VenueRoute::Simulated),
        Err(RejectReason::CurrencyMismatch)
    );
    assert_eq!(
        validate_currency(&instrument, "EUR", VenueRoute::BrokerDemo),
        Ok(())
    );
    assert_eq!(
        validate_currency(&instrument, "USD", VenueRoute::Simulated),
        Ok(())
    );
}

#[test]
fn resting_at_resolves_on_touch_for_every_side() {
    let price = |units| FixedPoint::try_new(units, 2).expect("price");
    for (side, units, expected) in [
        (OrderSide::Buy, 10_025, OrderType::Limit),
        (OrderSide::Buy, 10_050, OrderType::Stop),
        (OrderSide::Sell, 10_000, OrderType::Limit),
        (OrderSide::Sell, 9_975, OrderType::Stop),
    ] {
        let mut request = crate::tests::market_order("resting", side, 1, 1_000);
        request.order_type = OrderType::RestingAt;
        request.limit_price = Some(price(units));
        let resolved = resolve_resting_at(request, price(10_000), price(10_025))
            .expect("resolve on owner quote");
        assert_eq!(resolved.order_type, expected, "{side:?}");
        assert_eq!(
            resolved.limit_price,
            (expected == OrderType::Limit).then_some(price(units))
        );
        assert_eq!(
            resolved.stop_price,
            (expected == OrderType::Stop).then_some(price(units))
        );
    }
    let mut coarse = crate::tests::market_order("coarse", OrderSide::Buy, 1, 1_000);
    coarse.order_type = OrderType::RestingAt;
    coarse.limit_price = Some(FixedPoint::try_new(30_884, 0).expect("coarse"));
    assert_eq!(
        resolve_resting_at(coarse, price(3_088_400), price(3_088_425)),
        Err(RejectReason::ScaleMismatch)
    );
}

#[test]
fn route_capabilities_resolve_day_and_protection_policy() {
    let simulated = RouteCapabilities::for_route(VenueRoute::Simulated);
    let broker = RouteCapabilities::for_route(VenueRoute::BrokerDemo);
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
    assert!(!broker.managed_trailing());
    assert!(simulated.managed_trailing());
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
    // The simulated venue has no client id limit; an attached broker venue declares one.
    let limits = [None, Some(50)];
    for limit in limits {
        assert_eq!(
            validate_common_order(&instrument, &order, limit, MAXIMUM_OPEN_ORDERS, 0),
            Err(RejectReason::CapacityExceeded)
        );
        order.quantity = FixedPoint::try_new(0, order.quantity.scale()).expect("quantity");
        assert_eq!(
            validate_common_order(&instrument, &order, limit, 0, 0),
            Err(RejectReason::InvalidQuantity)
        );
        order.quantity = FixedPoint::try_new(1, order.quantity.scale()).expect("quantity");
        assert_eq!(
            validate_common_order(&instrument, &order, limit, 0, 0),
            Ok(())
        );
    }
    order.client_order_id = aeris_trading::ClientOrderId::try_new("x".repeat(51)).expect("id");
    assert_eq!(
        validate_common_order(&instrument, &order, Some(50), 0, 0),
        Err(RejectReason::ClientIdTooLong)
    );
    assert_eq!(
        validate_common_order(&instrument, &order, None, 0, 0),
        Ok(())
    );
    instrument.contract.order_quantity_increment =
        Some(InstrumentDecimal::try_new(2, 0).expect("increment"));
    order.client_order_id = aeris_trading::ClientOrderId::try_new("increment").expect("id");
    for limit in limits {
        assert_eq!(
            validate_common_order(&instrument, &order, limit, 0, 0),
            Err(RejectReason::InvalidQuantity)
        );
    }
    order.quantity = FixedPoint::try_new(1, 1).expect("wrong quantity scale");
    for limit in limits {
        assert_eq!(
            validate_common_order(&instrument, &order, limit, 0, 0),
            Err(RejectReason::ScaleMismatch)
        );
    }
}

#[test]
fn bracket_levels_are_validated_before_admitting_the_entry() {
    let mut instrument = crate::tests::instrument();
    instrument.contract.price_grid =
        PriceGrid::Fixed(InstrumentDecimal::try_new(50, 2).expect("grid"));
    let mut template = crate::BracketStrategyTemplate {
        template_id: "grid-bracket".into(),
        revision: 1,
        name: "Grid bracket".into(),
        stop_offset_ticks: 1,
        targets: vec![crate::BracketTarget {
            offset_ticks: 1,
            quantity_percent: 100,
        }],
        trailing_stop: None,
        break_even: None,
        enabled: true,
    };
    let entry = FixedPoint::try_new(10_000, 2).expect("entry");
    assert_eq!(
        validate_bracket_levels(&instrument, OrderSide::Buy, entry, &template),
        Err(RejectReason::OffGrid),
        "a 0.25 tick-sized target is not on the 0.50 price grid"
    );
    template.stop_offset_ticks = 2;
    assert_eq!(
        validate_bracket_levels(&instrument, OrderSide::Buy, entry, &template),
        Err(RejectReason::OffGrid),
        "an on-grid stop cannot hide an off-grid target"
    );
    template.stop_offset_ticks = 1;
    instrument.contract.price_grid =
        PriceGrid::Fixed(InstrumentDecimal::try_new(25, 2).expect("grid"));
    assert_eq!(
        validate_bracket_levels(&instrument, OrderSide::Buy, entry, &template),
        Ok(())
    );
    let low_entry = FixedPoint::try_new(25, 2).expect("entry");
    assert_eq!(
        validate_bracket_levels(&instrument, OrderSide::Buy, low_entry, &template),
        Err(RejectReason::WrongSideTrigger),
        "protection cannot cross or reach zero relative to its entry"
    );
}
