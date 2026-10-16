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
                    "cannot cancel an order in {} state",
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
    if order.revision != 0 || order.accepted_unix_nanos != 0 {
        return Err(LifecycleError::AlreadyAccepted);
    }
    order.revision = 1;
    order.accepted_unix_nanos = owner_unix_nanos;
    Ok(order)
}

pub(crate) fn cancel(order: Order, command_unix_nanos: i64) -> Result<Order, LifecycleError> {
    if !matches!(
        order.status,
        OrderStatus::Working | OrderStatus::PartiallyFilled
    ) {
        return Err(LifecycleError::InvalidTransition(order.status));
    }
    if command_unix_nanos <= 0 {
        return Err(LifecycleError::InvalidTimestamp);
    }
    let revision = order
        .revision
        .checked_add(1)
        .ok_or(LifecycleError::RevisionExhausted)?;
    Ok(Order {
        status: OrderStatus::Cancelled,
        revision,
        terminal_unix_nanos: Some(command_unix_nanos),
        ..order
    })
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
}
