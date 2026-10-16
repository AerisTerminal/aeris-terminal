//! Pure order transitions; the caller commits the returned value and its event atomically.

use aeris_trading::{Order, OrderStatus};

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
    if !matches!(order.status, OrderStatus::Working | OrderStatus::Pending) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
            OrderStatus::Pending,
            OrderStatus::PendingModify,
            OrderStatus::PendingCancel,
            OrderStatus::Filled,
            OrderStatus::Cancelled,
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
