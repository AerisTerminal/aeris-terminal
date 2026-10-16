//! Pure order transitions; the caller commits the returned value and its event atomically.

use aeris_trading::{Order, OrderStatus};

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum LifecycleError {
    InvalidTransition(OrderStatus),
    InvalidTimestamp,
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
        }
    }
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
    Ok(Order {
        status: OrderStatus::Cancelled,
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
}
