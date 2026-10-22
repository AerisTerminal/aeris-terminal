//! Exact translation between the provider-neutral venue contract and Rithmic order- and
//! P&L-plant values. A value that would need rounding is refused, never approximated.

use aeris_instruments::InstrumentId;
use aeris_rithmic_protocol_adapter::{
    ProviderTimestamp, RithmicBracketLevel, RithmicDecimal, RithmicExchangeNotifyType,
    RithmicExchangeOrderNotification, RithmicGateway, RithmicInstrumentPnl, RithmicOrderDetails,
    RithmicOrderDuration, RithmicOrderNotification, RithmicOrderNotifyType, RithmicOrderSide,
    RithmicOrderType, RithmicReportedPriceType, RithmicReportedSide, rithmic_instrument_id,
};
use aeris_trading::{
    AccountEnvironment, FixedPoint, OrderSide, OrderType, TimeInForce,
    venue::{
        BracketLevel, BrokerFill, BrokerOrder, BrokerOrderKind, BrokerOrderState, VenuePosition,
    },
};

/// The system Rithmic serves simulated paper-trading accounts on.
const PAPER_TRADING_SYSTEM: &str = "Rithmic Paper Trading";
const NANOS_PER_SECOND: i64 = 1_000_000_000;
const NANOS_PER_MICROSECOND: i64 = 1_000;

/// Demo for the conformance gateway and the paper-trading system; every other login trades
/// real money.
pub(super) fn environment(gateway: RithmicGateway, system_name: &str) -> AccountEnvironment {
    if gateway == RithmicGateway::Test || system_name == PAPER_TRADING_SYSTEM {
        AccountEnvironment::Demo
    } else {
        AccountEnvironment::Live
    }
}

/// The minor-unit precision of the account currencies Rithmic futures accounts use; any
/// other currency is refused rather than guessed.
pub(super) fn currency_scale(currency: &str) -> Option<u8> {
    match currency {
        "JPY" => Some(0),
        "USD" | "EUR" | "GBP" | "CAD" | "AUD" | "NZD" | "CHF" | "HKD" | "SGD" | "CNH" => Some(2),
        _ => None,
    }
}

pub(super) fn wire_decimal(value: FixedPoint) -> Result<RithmicDecimal, String> {
    RithmicDecimal::try_new(value.units(), value.scale())
        .map_err(|_| "the price scale is out of Rithmic's range".to_string())
}

fn fixed(value: RithmicDecimal) -> Option<FixedPoint> {
    FixedPoint::try_new(value.units(), value.scale()).ok()
}

fn whole(count: u32) -> Option<FixedPoint> {
    FixedPoint::try_new(i64::from(count), 0).ok()
}

/// A positive whole number of contracts.
pub(super) fn contracts(quantity: FixedPoint) -> Result<u32, String> {
    quantity
        .exact_rescale(0)
        .ok()
        .and_then(|whole| u32::try_from(whole.units()).ok())
        .filter(|count| *count > 0)
        .ok_or_else(|| "Rithmic trades a positive whole number of contracts".to_string())
}

pub(super) const fn order_side(side: OrderSide) -> RithmicOrderSide {
    match side {
        OrderSide::Buy => RithmicOrderSide::Buy,
        OrderSide::Sell => RithmicOrderSide::Sell,
    }
}

pub(super) const fn duration(time_in_force: TimeInForce) -> RithmicOrderDuration {
    match time_in_force {
        TimeInForce::Day => RithmicOrderDuration::Day,
        TimeInForce::GoodTillCancelled => RithmicOrderDuration::GoodTillCancelled,
        TimeInForce::ImmediateOrCancel => RithmicOrderDuration::ImmediateOrCancel,
        TimeInForce::FillOrKill => RithmicOrderDuration::FillOrKill,
    }
}

pub(super) fn order_type(
    order_type: OrderType,
    limit_price: Option<FixedPoint>,
    stop_price: Option<FixedPoint>,
) -> Result<RithmicOrderType, String> {
    let price = |value: Option<FixedPoint>, name: &str| {
        value
            .ok_or_else(|| format!("a {name} price is required"))
            .and_then(wire_decimal)
    };
    Ok(match order_type {
        OrderType::Market => RithmicOrderType::Market,
        OrderType::Limit => RithmicOrderType::Limit {
            price: price(limit_price, "limit")?,
        },
        OrderType::Stop => RithmicOrderType::StopMarket {
            trigger: price(stop_price, "stop")?,
        },
        OrderType::StopLimit => RithmicOrderType::StopLimit {
            price: price(limit_price, "limit")?,
            trigger: price(stop_price, "stop")?,
        },
        OrderType::RestingAt => {
            return Err("a resting instruction is resolved before it reaches a broker".into());
        }
    })
}

pub(super) fn bracket_levels(levels: &[BracketLevel]) -> Result<Vec<RithmicBracketLevel>, String> {
    levels
        .iter()
        .map(|level| {
            Ok(RithmicBracketLevel {
                quantity: contracts(level.quantity)?,
                ticks: u32::try_from(level.ticks)
                    .map_err(|_| "a bracket distance must be positive".to_string())?,
            })
        })
        .collect()
}

pub(super) fn instrument(exchange: &str, symbol: &str) -> Option<InstrumentId> {
    InstrumentId::try_new(rithmic_instrument_id(exchange, symbol)).ok()
}

pub(super) fn unix_nanos(timestamp: ProviderTimestamp) -> i64 {
    i64::from(timestamp.seconds) * NANOS_PER_SECOND
        + i64::from(timestamp.microseconds) * NANOS_PER_MICROSECOND
}

const fn reported_side(side: RithmicReportedSide) -> OrderSide {
    match side {
        RithmicReportedSide::Buy => OrderSide::Buy,
        RithmicReportedSide::Sell | RithmicReportedSide::SellShort => OrderSide::Sell,
    }
}

/// The broker's view of one order, or `None` when Rithmic left out a field it needs. A
/// bracket exit names its entry as its original basket and is reported without a client
/// order id, so it can never be mistaken for the entry it derives from.
pub(super) fn broker_order(details: &RithmicOrderDetails) -> Option<BrokerOrder> {
    let kind = match details.price_type {
        Some(RithmicReportedPriceType::Market) => BrokerOrderKind::Market,
        Some(RithmicReportedPriceType::Limit) => BrokerOrderKind::Limit,
        Some(RithmicReportedPriceType::StopMarket) => BrokerOrderKind::Stop,
        Some(RithmicReportedPriceType::StopLimit) => BrokerOrderKind::StopLimit,
        None => BrokerOrderKind::Other,
    };
    let priced = |carries: bool, value: Option<RithmicDecimal>| {
        if carries { value.and_then(fixed) } else { None }
    };
    Some(BrokerOrder {
        broker_order_id: details.basket_id.clone(),
        client_order_id: details
            .original_basket_id
            .is_none()
            .then(|| details.user_tag.clone())
            .flatten(),
        instrument_id: instrument(&details.exchange, &details.symbol)?,
        side: reported_side(details.side?),
        kind,
        quantity: whole(details.quantity?)?,
        filled_quantity: whole(details.total_fill_size.unwrap_or(0))?,
        limit_price: priced(
            matches!(kind, BrokerOrderKind::Limit | BrokerOrderKind::StopLimit),
            details.price,
        ),
        stop_price: priced(
            matches!(kind, BrokerOrderKind::Stop | BrokerOrderKind::StopLimit),
            details.trigger_price,
        ),
    })
}

/// What a Rithmic-side lifecycle step means for the order; steps still in transit mean
/// nothing yet.
pub(super) fn order_state(notification: &RithmicOrderNotification) -> Option<BrokerOrderState> {
    let filled = notification.order.total_fill_size.unwrap_or(0);
    match notification.notify_type {
        RithmicOrderNotifyType::Open | RithmicOrderNotifyType::TriggerPending if filled > 0 => {
            Some(BrokerOrderState::PartiallyFilled)
        }
        RithmicOrderNotifyType::Open | RithmicOrderNotifyType::TriggerPending => {
            Some(BrokerOrderState::Accepted)
        }
        RithmicOrderNotifyType::Modified => Some(BrokerOrderState::Replaced),
        RithmicOrderNotifyType::ModificationFailed | RithmicOrderNotifyType::CancellationFailed => {
            Some(BrokerOrderState::CancelRejected)
        }
        RithmicOrderNotifyType::Complete => Some(completed(notification)),
        _ => None,
    }
}

/// A completed order is filled when its whole quantity filled; otherwise its completion
/// reason says whether it was rejected, expired or cancelled.
fn completed(notification: &RithmicOrderNotification) -> BrokerOrderState {
    let order = &notification.order;
    let filled = order.total_fill_size.unwrap_or(0);
    if order
        .quantity
        .is_some_and(|quantity| quantity > 0 && filled >= quantity)
    {
        return BrokerOrderState::Filled;
    }
    let reason = notification
        .completion_reason
        .as_deref()
        .or(order.status.as_deref())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if reason.contains("reject") || reason.contains("fail") {
        BrokerOrderState::Rejected
    } else if reason.contains("expire") {
        BrokerOrderState::Expired
    } else {
        BrokerOrderState::Cancelled
    }
}

/// What an exchange report means for the order. Status and cancel reports repeat what
/// the Rithmic-side notifications already say, so only fills and refusals count.
pub(super) fn exchange_state(
    notification: &RithmicExchangeOrderNotification,
) -> Option<BrokerOrderState> {
    match notification.notify_type {
        RithmicExchangeNotifyType::Fill if notification.order.total_unfilled_size == Some(0) => {
            Some(BrokerOrderState::Filled)
        }
        RithmicExchangeNotifyType::Fill => Some(BrokerOrderState::PartiallyFilled),
        RithmicExchangeNotifyType::Reject => Some(BrokerOrderState::Rejected),
        RithmicExchangeNotifyType::NotModified | RithmicExchangeNotifyType::NotCancelled => {
            Some(BrokerOrderState::CancelRejected)
        }
        _ => None,
    }
}

/// Whether an order an order snapshot reports is still working.
pub(super) fn snapshot_working(
    order_notification: Option<RithmicOrderNotifyType>,
    exchange: Option<&RithmicExchangeOrderNotification>,
) -> bool {
    match (order_notification, exchange) {
        (Some(notify_type), _) => notify_type != RithmicOrderNotifyType::Complete,
        (None, Some(notification)) => match notification.notify_type {
            RithmicExchangeNotifyType::Cancel | RithmicExchangeNotifyType::Reject => false,
            RithmicExchangeNotifyType::Fill => notification.order.total_unfilled_size != Some(0),
            _ => true,
        },
        (None, None) => false,
    }
}

/// One execution. Its deal id joins the order's basket and the fill id, so the same
/// execution reported live and again by a replay is recorded once.
pub(super) fn broker_fill(
    notification: &RithmicExchangeOrderNotification,
    observed_unix_nanos: i64,
) -> Option<BrokerFill> {
    let fill = notification.fill.as_ref()?;
    let order = &notification.order;
    Some(BrokerFill {
        broker_deal_id: format!("{}:{}", order.basket_id, fill.fill_id),
        broker_order_id: order.basket_id.clone(),
        instrument_id: instrument(&order.exchange, &order.symbol)?,
        side: reported_side(order.side?),
        price: fixed(fill.price)?,
        quantity: whole(fill.size).filter(|size| size.units() > 0)?,
        executed_unix_nanos: order.timestamp.map_or(observed_unix_nanos, unix_nanos),
    })
}

/// The account's net position in one instrument; `None` without a reported net quantity.
pub(super) fn venue_position(row: &RithmicInstrumentPnl) -> Option<VenuePosition> {
    let net = row.quantities.net_quantity?;
    Some(VenuePosition {
        instrument_id: instrument(&row.exchange, &row.symbol)?,
        net_quantity: FixedPoint::try_new(i64::from(net), 0).ok()?,
        average_price: if net == 0 {
            None
        } else {
            row.average_open_fill_price.and_then(fixed)
        },
        open_pnl: row.open_position_pnl.and_then(fixed),
    })
}

pub(super) fn money(value: RithmicDecimal) -> Option<FixedPoint> {
    fixed(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_rithmic_protocol_adapter::{
        RithmicAccountRef, RithmicFill, RithmicPositionQuantities,
    };

    fn decimal(units: i64, scale: u8) -> RithmicDecimal {
        RithmicDecimal::try_new(units, scale).expect("decimal")
    }

    fn point(units: i64, scale: u8) -> FixedPoint {
        FixedPoint::try_new(units, scale).expect("fixed point")
    }

    fn details() -> RithmicOrderDetails {
        RithmicOrderDetails {
            user_tag: Some("client-1".into()),
            is_snapshot: false,
            basket_id: "basket-1".into(),
            original_basket_id: None,
            linked_basket_ids: None,
            bracket_type: None,
            account_id: RithmicAccountRef::new("account-1"),
            symbol: "ESZ5".into(),
            exchange: "CME".into(),
            trade_route: None,
            exchange_order_id: None,
            status: None,
            side: Some(RithmicReportedSide::SellShort),
            quantity: Some(3),
            price: Some(decimal(50_005, 1)),
            trigger_price: Some(decimal(4_999, 0)),
            price_type: Some(RithmicReportedPriceType::StopLimit),
            duration: None,
            placement: None,
            average_fill_price: None,
            total_fill_size: Some(1),
            total_unfilled_size: Some(2),
            text: None,
            timestamp: Some(ProviderTimestamp {
                seconds: 2,
                microseconds: 5,
            }),
        }
    }

    #[test]
    fn requests_convert_exactly_or_are_refused() {
        assert_eq!(contracts(point(30, 1)), Ok(3));
        for refused in [point(15, 1), point(0, 0), point(-1, 0)] {
            assert!(contracts(refused).is_err(), "{refused:?}");
        }
        assert_eq!(
            order_type(
                OrderType::StopLimit,
                Some(point(500_025, 2)),
                Some(point(5_000, 0))
            ),
            Ok(RithmicOrderType::StopLimit {
                price: decimal(500_025, 2),
                trigger: decimal(5_000, 0),
            })
        );
        assert!(order_type(OrderType::Limit, None, None).is_err());
        assert!(order_type(OrderType::RestingAt, Some(point(1, 0)), None).is_err());
        assert_eq!(
            bracket_levels(&[BracketLevel {
                quantity: point(2, 0),
                ticks: 8,
            }]),
            Ok(vec![RithmicBracketLevel {
                quantity: 2,
                ticks: 8,
            }])
        );
        assert!(
            bracket_levels(&[BracketLevel {
                quantity: point(2, 0),
                ticks: -8,
            }])
            .is_err()
        );
    }

    #[test]
    fn only_test_and_paper_trading_logins_are_demo() {
        assert_eq!(
            environment(RithmicGateway::Test, "Rithmic Test"),
            AccountEnvironment::Demo
        );
        assert_eq!(
            environment(RithmicGateway::Chicago, PAPER_TRADING_SYSTEM),
            AccountEnvironment::Demo
        );
        assert_eq!(
            environment(RithmicGateway::Chicago, "Rithmic 01"),
            AccountEnvironment::Live
        );
        assert_eq!(currency_scale("USD"), Some(2));
        assert_eq!(currency_scale("JPY"), Some(0));
        assert_eq!(currency_scale("XYZ"), None);
    }

    #[test]
    fn reported_orders_keep_the_brokers_exact_terms() {
        let order = broker_order(&details()).expect("order");
        assert_eq!(order.instrument_id.as_str(), "instrument:rithmic:CME:ESZ5");
        assert_eq!(order.client_order_id.as_deref(), Some("client-1"));
        assert_eq!(order.side, OrderSide::Sell);
        assert_eq!(order.kind, BrokerOrderKind::StopLimit);
        assert_eq!(
            (order.quantity, order.filled_quantity),
            (point(3, 0), point(1, 0))
        );
        assert_eq!(order.limit_price, Some(point(50_005, 1)));
        assert_eq!(order.stop_price, Some(point(4_999, 0)));

        let exit = RithmicOrderDetails {
            original_basket_id: Some("entry".into()),
            price_type: Some(RithmicReportedPriceType::Market),
            ..details()
        };
        let exit = broker_order(&exit).expect("exit");
        assert_eq!(
            exit.client_order_id, None,
            "an exit never claims its entry's id"
        );
        assert_eq!((exit.limit_price, exit.stop_price), (None, None));
        assert!(
            broker_order(&RithmicOrderDetails {
                side: None,
                ..details()
            })
            .is_none()
        );
    }

    #[test]
    fn lifecycle_steps_map_to_order_states() {
        let notification =
            |notify_type, fill: u32, reason: Option<&str>| RithmicOrderNotification {
                notify_type,
                order: RithmicOrderDetails {
                    total_fill_size: Some(fill),
                    ..details()
                },
                completion_reason: reason.map(str::to_string),
            };
        for (step, expected) in [
            (
                notification(RithmicOrderNotifyType::Open, 0, None),
                Some(BrokerOrderState::Accepted),
            ),
            (
                notification(RithmicOrderNotifyType::Open, 1, None),
                Some(BrokerOrderState::PartiallyFilled),
            ),
            (
                notification(RithmicOrderNotifyType::Modified, 0, None),
                Some(BrokerOrderState::Replaced),
            ),
            (
                notification(RithmicOrderNotifyType::Complete, 3, Some("Fill")),
                Some(BrokerOrderState::Filled),
            ),
            (
                notification(RithmicOrderNotifyType::Complete, 0, Some("Rejected")),
                Some(BrokerOrderState::Rejected),
            ),
            (
                notification(RithmicOrderNotifyType::Complete, 0, Some("Expired")),
                Some(BrokerOrderState::Expired),
            ),
            (
                notification(RithmicOrderNotifyType::Complete, 1, Some("Canceled")),
                Some(BrokerOrderState::Cancelled),
            ),
            (
                notification(RithmicOrderNotifyType::CancelPending, 0, None),
                None,
            ),
        ] {
            assert_eq!(order_state(&step), expected, "{step:?}");
        }
    }

    #[test]
    fn fills_carry_a_replay_stable_deal_id_and_the_execution_time() {
        let notification = RithmicExchangeOrderNotification {
            notify_type: RithmicExchangeNotifyType::Fill,
            order: RithmicOrderDetails {
                total_unfilled_size: Some(0),
                ..details()
            },
            report_type: None,
            fill: Some(RithmicFill {
                fill_id: "7".into(),
                price: decimal(500_025, 2),
                size: 1,
                fill_date: None,
                fill_time: None,
            }),
        };
        let fill = broker_fill(&notification, 99).expect("fill");
        assert_eq!(fill.broker_deal_id, "basket-1:7");
        assert_eq!(fill.price, point(500_025, 2));
        assert_eq!(fill.quantity, point(1, 0));
        assert_eq!(fill.executed_unix_nanos, 2_000_005_000);
        assert_eq!(
            exchange_state(&notification),
            Some(BrokerOrderState::Filled)
        );
        assert!(!snapshot_working(None, Some(&notification)));
    }

    #[test]
    fn positions_report_net_quantity_average_and_open_pnl() {
        let row = RithmicInstrumentPnl {
            is_snapshot: true,
            account_id: RithmicAccountRef::new("account-1"),
            symbol: "ESZ5".into(),
            exchange: "CME".into(),
            quantities: RithmicPositionQuantities {
                net_quantity: Some(-2),
                ..RithmicPositionQuantities::default()
            },
            average_open_fill_price: Some(decimal(5_000_125, 3)),
            open_position_pnl: Some(decimal(-125, 0)),
            closed_position_pnl: None,
            day_open_pnl: None,
            day_closed_pnl: None,
            day_pnl: None,
            timestamp: None,
        };
        let position = venue_position(&row).expect("position");
        assert_eq!(position.net_quantity, point(-2, 0));
        assert_eq!(position.average_price, Some(point(5_000_125, 3)));
        assert_eq!(position.open_pnl, Some(point(-125, 0)));
        let flat = RithmicInstrumentPnl {
            quantities: RithmicPositionQuantities {
                net_quantity: Some(0),
                ..RithmicPositionQuantities::default()
            },
            ..row.clone()
        };
        assert_eq!(venue_position(&flat).expect("flat").average_price, None);
        let unknown = RithmicInstrumentPnl {
            quantities: RithmicPositionQuantities::default(),
            ..row
        };
        assert!(venue_position(&unknown).is_none());
    }
}
