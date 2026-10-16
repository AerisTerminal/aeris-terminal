use aeris_charts_engine::OrderStatus as ChartOrderStatus;
use aeris_trading::OrderStatus;

/// Translate authoritative trading lifecycle facts into the chart's visual semantics.
/// Held legs stay visible without acquiring working-order actions.
#[must_use]
pub fn chart_order_status(status: OrderStatus) -> ChartOrderStatus {
    match status {
        OrderStatus::Inactive => ChartOrderStatus::Inactive,
        OrderStatus::Pending => ChartOrderStatus::PendingSubmit,
        OrderStatus::Working => ChartOrderStatus::Working,
        OrderStatus::PendingModify => ChartOrderStatus::PendingModify,
        OrderStatus::PartiallyFilled => ChartOrderStatus::PartiallyFilled,
        OrderStatus::PendingCancel => ChartOrderStatus::PendingCancel,
        OrderStatus::Filled => ChartOrderStatus::Filled,
        OrderStatus::Cancelled => ChartOrderStatus::Cancelled,
        OrderStatus::Expired => ChartOrderStatus::Expired,
        OrderStatus::Rejected => ChartOrderStatus::Rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_order_projects_as_inactive_not_working_or_pending() {
        assert_eq!(
            chart_order_status(OrderStatus::Inactive),
            ChartOrderStatus::Inactive
        );
        assert_eq!(
            chart_order_status(OrderStatus::Working),
            ChartOrderStatus::Working
        );
    }
}
