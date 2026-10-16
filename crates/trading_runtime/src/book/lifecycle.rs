//! Pure order transitions; the caller commits the returned value and its event atomically.

use aeris_trading::{
    FixedPoint, Order, OrderEventKind, OrderStatus, TradingProvenance,
    venue::{BrokerOrder, BrokerOrderState},
};

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum LifecycleError {
    InvalidTransition(OrderStatus),
    InvalidTimestamp,
    RevisionExhausted,
    AlreadyAccepted,
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTransition(status) => {
                write!(
                    formatter,
                    "cannot transition an order in {} state",
                    status.as_str()
                )
            }
            Self::InvalidTimestamp => formatter.write_str("order transition time must be positive"),
            Self::RevisionExhausted => formatter.write_str("order lifecycle revision exhausted"),
            Self::AlreadyAccepted => {
                formatter.write_str("order already has an acceptance revision")
            }
        }
    }
}

/// Initializes the one order identity and acceptance time for either route.
pub(crate) fn accept(mut order: Order, owner_unix_nanos: i64) -> Result<Order, LifecycleError> {
    if !matches!(
        order.status,
        OrderStatus::Inactive | OrderStatus::Working | OrderStatus::Pending
    ) {
        return Err(LifecycleError::InvalidTransition(order.status));
    }
    if owner_unix_nanos <= 0 {
        return Err(LifecycleError::InvalidTimestamp);
    }
    if order.revision != 0 || order.accepted_unix_nanos != 0 || order.updated_unix_nanos != 0 {
        return Err(LifecycleError::AlreadyAccepted);
    }
    order.revision = 1;
    order.accepted_unix_nanos = owner_unix_nanos;
    order.updated_unix_nanos = owner_unix_nanos;
    Ok(order)
}

fn advance(order: &mut Order, owner_unix_nanos: i64) -> Result<(), LifecycleError> {
    if owner_unix_nanos <= 0 {
        return Err(LifecycleError::InvalidTimestamp);
    }
    let revision = order
        .revision
        .checked_add(1)
        .ok_or(LifecycleError::RevisionExhausted)?;
    order.revision = revision;
    order.updated_unix_nanos = owner_unix_nanos;
    Ok(())
}

/// A stale instruction returns the exact current owner fact without changing it.
pub(crate) fn expect_revision(order: &Order, expected: Option<u32>) -> Result<(), Box<Order>> {
    if expected.is_some_and(|revision| revision != order.revision) {
        Err(Box::new(order.clone()))
    } else {
        Ok(())
    }
}

pub(crate) fn modify(mut order: Order, owner_unix_nanos: i64) -> Result<Order, LifecycleError> {
    if !matches!(
        order.status,
        OrderStatus::Working | OrderStatus::PartiallyFilled
    ) {
        return Err(LifecycleError::InvalidTransition(order.status));
    }
    advance(&mut order, owner_unix_nanos)?;
    Ok(order)
}

pub(crate) fn request(
    mut order: Order,
    pending: OrderStatus,
    owner_unix_nanos: i64,
) -> Result<Order, LifecycleError> {
    if !matches!(
        order.status,
        OrderStatus::Working | OrderStatus::PartiallyFilled
    ) || !matches!(
        pending,
        OrderStatus::PendingModify | OrderStatus::PendingCancel
    ) {
        return Err(LifecycleError::InvalidTransition(order.status));
    }
    advance(&mut order, owner_unix_nanos)?;
    order.status = pending;
    Ok(order)
}

pub(crate) fn cancel(order: Order, command_unix_nanos: i64) -> Result<Order, LifecycleError> {
    if !matches!(
        order.status,
        OrderStatus::Working | OrderStatus::PartiallyFilled
    ) {
        return Err(LifecycleError::InvalidTransition(order.status));
    }
    let mut cancelled = order;
    advance(&mut cancelled, command_unix_nanos)?;
    cancelled.status = OrderStatus::Cancelled;
    cancelled.terminal_unix_nanos = Some(command_unix_nanos);
    Ok(cancelled)
}

/// Simulated execution fills the remaining order in one local transition.
pub(crate) fn fill_simulated(
    mut order: Order,
    owner_unix_nanos: i64,
) -> Result<Order, LifecycleError> {
    if !matches!(
        order.status,
        OrderStatus::Working | OrderStatus::PartiallyFilled
    ) {
        return Err(LifecycleError::InvalidTransition(order.status));
    }
    advance(&mut order, owner_unix_nanos)?;
    order.status = OrderStatus::Filled;
    order.filled_quantity = order.quantity;
    order.terminal_unix_nanos = Some(owner_unix_nanos);
    Ok(order)
}

/// Applies a broker report (or reconcile confirmation) only when a binding or fact changes.
pub(crate) fn broker_report(
    current: &Order,
    report: &BrokerOrder,
    state: BrokerOrderState,
    resolve_pending: bool,
    binding: bool,
    owner_unix_nanos: i64,
) -> Result<Option<(Order, OrderEventKind)>, LifecycleError> {
    if !current.status.is_open() {
        return Err(LifecycleError::InvalidTransition(current.status));
    }
    let pending_request = !resolve_pending
        && matches!(
            current.status,
            OrderStatus::PendingModify | OrderStatus::PendingCancel
        );
    let working = if report.filled_quantity.units() == 0 {
        OrderStatus::Working
    } else {
        OrderStatus::PartiallyFilled
    };
    let (status, kind) = match state {
        BrokerOrderState::Accepted if pending_request => (current.status, OrderEventKind::Accepted),
        BrokerOrderState::Accepted => (working, OrderEventKind::Accepted),
        BrokerOrderState::Replaced if current.status == OrderStatus::PendingCancel => {
            (current.status, OrderEventKind::Modified)
        }
        BrokerOrderState::Replaced => (working, OrderEventKind::Modified),
        BrokerOrderState::PartiallyFilled if pending_request => {
            (current.status, OrderEventKind::Filled)
        }
        BrokerOrderState::PartiallyFilled => (working, OrderEventKind::Filled),
        BrokerOrderState::Filled => (OrderStatus::Filled, OrderEventKind::Filled),
        BrokerOrderState::Cancelled => (OrderStatus::Cancelled, OrderEventKind::Cancelled),
        BrokerOrderState::Expired => (OrderStatus::Expired, OrderEventKind::Expired),
        BrokerOrderState::CancelRejected if current.status == OrderStatus::PendingCancel => {
            (working, OrderEventKind::Rejected)
        }
        BrokerOrderState::CancelRejected => (current.status, OrderEventKind::Rejected),
        BrokerOrderState::Rejected if report.filled_quantity.units() > 0 => {
            (OrderStatus::Cancelled, OrderEventKind::Rejected)
        }
        BrokerOrderState::Rejected => (OrderStatus::Rejected, OrderEventKind::Rejected),
    };
    let mut updated = Order {
        status,
        quantity: report.quantity,
        filled_quantity: report.filled_quantity,
        ..current.clone()
    };
    if matches!(
        current.order_type,
        aeris_trading::OrderType::Limit | aeris_trading::OrderType::StopLimit
    ) {
        updated.limit_price = report.limit_price.or(current.limit_price);
    }
    if matches!(
        current.order_type,
        aeris_trading::OrderType::Stop | aeris_trading::OrderType::StopLimit
    ) {
        updated.stop_price = report.stop_price.or(current.stop_price);
    }
    if status == OrderStatus::Filled {
        updated.filled_quantity = updated.quantity;
    }
    if !binding
        && updated.status == current.status
        && updated.quantity == current.quantity
        && updated.filled_quantity == current.filled_quantity
        && updated.limit_price == current.limit_price
        && updated.stop_price == current.stop_price
    {
        return Ok(None);
    }
    advance(&mut updated, owner_unix_nanos)?;
    if !updated.status.is_open() {
        updated.terminal_unix_nanos = Some(owner_unix_nanos);
    }
    Ok(Some((updated, kind)))
}

pub(crate) fn broker_refusal(
    mut order: Order,
    owner_unix_nanos: i64,
) -> Result<Option<Order>, LifecycleError> {
    order.status = match order.status {
        OrderStatus::Pending => OrderStatus::Rejected,
        OrderStatus::PendingModify | OrderStatus::PendingCancel => {
            if order.filled_quantity.units() == 0 {
                OrderStatus::Working
            } else {
                OrderStatus::PartiallyFilled
            }
        }
        _ => return Ok(None),
    };
    advance(&mut order, owner_unix_nanos)?;
    if !order.status.is_open() {
        order.terminal_unix_nanos = Some(owner_unix_nanos);
    }
    Ok(Some(order))
}

pub(crate) fn broker_missing(
    mut order: Order,
    bound: bool,
    owner_unix_nanos: i64,
) -> Result<(Order, OrderEventKind, &'static str), LifecycleError> {
    if !order.status.is_open() {
        return Err(LifecycleError::InvalidTransition(order.status));
    }
    let (status, kind, detail) = if order.filled_quantity == order.quantity {
        (
            OrderStatus::Filled,
            OrderEventKind::Filled,
            "filled at cTrader",
        )
    } else if bound || order.filled_quantity.units() > 0 {
        (
            OrderStatus::Cancelled,
            OrderEventKind::Cancelled,
            "no longer open at cTrader",
        )
    } else {
        (
            OrderStatus::Rejected,
            OrderEventKind::Rejected,
            "cTrader did not receive the order",
        )
    };
    advance(&mut order, owner_unix_nanos)?;
    order.status = status;
    order.terminal_unix_nanos = Some(owner_unix_nanos);
    Ok((order, kind, detail))
}

/// The larger of the broker's known filled quantity and its deduplicated deal sum.
/// Terminal orders may receive a late deal, but an unchanged deal cannot reenter here.
pub(crate) fn broker_deal(
    current: &Order,
    filled_by_deals: FixedPoint,
    mirror: bool,
    provenance: TradingProvenance,
    owner_unix_nanos: i64,
) -> Result<Order, LifecycleError> {
    let quantity = if mirror || filled_by_deals.units() > current.quantity.units() {
        filled_by_deals
    } else {
        current.quantity
    };
    let filled = if filled_by_deals.units() > current.filled_quantity.units() {
        filled_by_deals
    } else {
        current.filled_quantity
    };
    let status = if filled == quantity {
        OrderStatus::Filled
    } else if !current.status.is_open()
        || matches!(
            current.status,
            OrderStatus::PendingModify | OrderStatus::PendingCancel
        )
    {
        current.status
    } else {
        OrderStatus::PartiallyFilled
    };
    let mut updated = Order {
        quantity,
        filled_quantity: filled,
        status,
        provenance,
        ..current.clone()
    };
    advance(&mut updated, owner_unix_nanos)?;
    if !status.is_open() && (current.status.is_open() || status != current.status) {
        updated.terminal_unix_nanos = Some(owner_unix_nanos);
    }
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_trading::venue::BrokerOrderKind;

    #[test]
    fn terminal_order_cannot_be_cancelled_again() {
        let mut order = super::super::tests::order(1, OrderStatus::Working);
        order.status = OrderStatus::Filled;
        assert!(cancel(order, 2).is_err());
    }

    #[test]
    fn acceptance_cannot_reset_an_existing_revision() {
        let order = super::super::tests::order(1, OrderStatus::Working);
        assert!(accept(order, 2).is_err());
    }

    #[test]
    fn revision_guard_returns_the_current_order_without_advancing() {
        let order = super::super::tests::order(1, OrderStatus::Working);
        assert_eq!(
            expect_revision(&order, Some(2)),
            Err(Box::new(order.clone()))
        );
        assert_eq!(expect_revision(&order, Some(1)), Ok(()));
        assert_eq!(expect_revision(&order, None), Ok(()));
    }

    #[test]
    fn inactive_child_is_admitted_but_never_executable() {
        let mut child = super::super::tests::order(1, OrderStatus::Inactive);
        child.revision = 0;
        child.accepted_unix_nanos = 0;
        child.updated_unix_nanos = 0;
        let child = accept(child, 2).expect("held child");
        assert_eq!((child.revision, child.accepted_unix_nanos), (1, 2));
        assert!(child.validate().is_ok());
        assert!(modify(child.clone(), 3).is_err());
        assert!(fill_simulated(child, 3).is_err());
    }

    #[test]
    fn simulated_fill_is_terminal_and_rejects_repeat_execution() {
        let original = super::super::tests::order(1, OrderStatus::Working);
        let filled = fill_simulated(original, 2).expect("fill");
        assert_eq!(filled.revision, 2);
        assert_eq!(filled.updated_unix_nanos, 2);
        assert_eq!(filled.terminal_unix_nanos, Some(2));
        assert_eq!(filled.filled_quantity, filled.quantity);
        assert!(fill_simulated(filled, 3).is_err());
    }

    #[test]
    fn broker_report_is_idempotent_and_terminal_reports_cannot_reopen() {
        let mut pending = super::super::tests::order(1, OrderStatus::Pending);
        let report = BrokerOrder {
            broker_order_id: "broker-1".into(),
            client_order_id: Some(pending.client_order_id.as_str().into()),
            instrument_id: pending.instrument_id.clone(),
            side: pending.side,
            kind: BrokerOrderKind::Limit,
            quantity: pending.quantity,
            filled_quantity: pending.filled_quantity,
            limit_price: pending.limit_price,
            stop_price: pending.stop_price,
            broker_position_id: None,
            closing: false,
        };
        let (accepted, _) = broker_report(
            &pending,
            &report,
            BrokerOrderState::Accepted,
            false,
            true,
            2,
        )
        .expect("report")
        .expect("binding");
        assert_eq!(
            (
                accepted.status,
                accepted.revision,
                accepted.updated_unix_nanos
            ),
            (OrderStatus::Working, 2, 2)
        );
        assert!(
            broker_report(
                &accepted,
                &report,
                BrokerOrderState::Accepted,
                false,
                false,
                3
            )
            .expect("replay")
            .is_none()
        );
        pending = request(accepted, OrderStatus::PendingCancel, 4).expect("cancel request");
        assert!(
            broker_report(
                &pending,
                &report,
                BrokerOrderState::Accepted,
                false,
                false,
                5
            )
            .expect("pending acceptance")
            .is_none()
        );
        let (cancelled, _) = broker_report(
            &pending,
            &report,
            BrokerOrderState::Cancelled,
            false,
            false,
            6,
        )
        .expect("cancel report")
        .expect("transition");
        assert_eq!(
            (cancelled.revision, cancelled.terminal_unix_nanos),
            (4, Some(6))
        );
        assert!(
            broker_report(
                &cancelled,
                &report,
                BrokerOrderState::Accepted,
                false,
                false,
                7
            )
            .is_err()
        );
    }

    #[test]
    fn late_broker_deal_preserves_a_terminal_timestamp() {
        let mut order = super::super::tests::order(2, OrderStatus::Working);
        order.quantity = FixedPoint::try_new(2, 0).expect("quantity");
        let (cancelled, _, _) = broker_missing(order, true, 2).expect("missing");
        let late = broker_deal(
            &cancelled,
            FixedPoint::try_new(1, 0).expect("quantity"),
            false,
            cancelled.provenance.clone(),
            3,
        )
        .expect("late deal");
        assert_eq!(
            (
                late.status,
                late.revision,
                late.updated_unix_nanos,
                late.terminal_unix_nanos
            ),
            (OrderStatus::Cancelled, 3, 3, Some(2))
        );
    }

    #[test]
    fn command_transition_table_accepts_only_executable_orders() {
        for status in [OrderStatus::Working, OrderStatus::PartiallyFilled] {
            let original = super::super::tests::order(1, status);
            let modified = modify(original.clone(), 2).expect("modify");
            assert_eq!(
                (
                    modified.status,
                    modified.revision,
                    modified.updated_unix_nanos
                ),
                (status, 2, 2)
            );
            for pending in [OrderStatus::PendingModify, OrderStatus::PendingCancel] {
                let requested = request(original.clone(), pending, 2).expect("request");
                assert_eq!(
                    (
                        requested.status,
                        requested.revision,
                        requested.updated_unix_nanos
                    ),
                    (pending, 2, 2)
                );
            }
            let cancelled = cancel(original, 2).expect("cancel");
            assert_eq!(
                (
                    cancelled.status,
                    cancelled.revision,
                    cancelled.updated_unix_nanos,
                    cancelled.terminal_unix_nanos
                ),
                (OrderStatus::Cancelled, 2, 2, Some(2))
            );
        }
        for status in [
            OrderStatus::Inactive,
            OrderStatus::Pending,
            OrderStatus::PendingModify,
            OrderStatus::PendingCancel,
            OrderStatus::Filled,
            OrderStatus::Cancelled,
            OrderStatus::Expired,
            OrderStatus::Rejected,
        ] {
            let original = super::super::tests::order(1, status);
            assert!(modify(original.clone(), 2).is_err(), "{status:?}");
            assert!(
                request(original.clone(), OrderStatus::PendingCancel, 2).is_err(),
                "{status:?}"
            );
            assert!(cancel(original, 2).is_err(), "{status:?}");
        }
    }
}
