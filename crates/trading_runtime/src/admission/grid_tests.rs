use super::*;
use aeris_instruments::InstrumentDecimal;
use aeris_trading::{OrderSide, OrderType, TimeInForce};

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
