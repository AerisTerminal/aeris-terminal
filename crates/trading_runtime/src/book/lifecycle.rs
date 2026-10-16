//! Pure order transitions; the caller commits the returned value and its event atomically.

use aeris_trading::{
    FixedPoint, Order, OrderEventKind, OrderEventReason, OrderStatus, OrderType, TimeInForce,
    TradingProvenance,
    venue::{BrokerOrder, BrokerOrderState},
};

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum LifecycleError {
    InvalidTransition(OrderStatus),
    InvalidTimestamp,
    RevisionExhausted,
    AlreadyAccepted,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "M5.5 group coverage producer uses this validation"
        )
    )]
    InvalidQuantity,
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
            Self::InvalidQuantity => formatter.write_str("resized quantity is invalid"),
        }
    }
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "M5.5 group modification causes have no producer yet"
    )
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModificationCause {
    User,
    Management,
    Coverage,
    LegMoved,
    ParentRepriced,
}

impl ModificationCause {
    const fn reason(self) -> OrderEventReason {
        match self {
            Self::User => OrderEventReason::UserModify,
            Self::Management => OrderEventReason::Management,
            Self::Coverage => OrderEventReason::Coverage,
            Self::LegMoved => OrderEventReason::LegMoved,
            Self::ParentRepriced => OrderEventReason::ParentRepriced,
        }
    }
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "M5.5 group coverage producer is pending")
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResizeCause {
    CoverageShrink,
    CoverageGrow,
}

impl ResizeCause {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "M5.5 group coverage producer is pending")
    )]
    const fn reason(self) -> OrderEventReason {
        match self {
            Self::CoverageShrink => OrderEventReason::CoverageShrink,
            Self::CoverageGrow => OrderEventReason::CoverageGrow,
        }
    }
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "M5.3 and M5.9 cancellation producers are pending")
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CancellationCause {
    User,
    Cascade,
    KillSwitch,
    Flatten,
    NotMarketable,
    NoQuote,
}

impl CancellationCause {
    pub(crate) const fn reason(self) -> OrderEventReason {
        match self {
            Self::User => OrderEventReason::UserCancel,
            Self::Cascade => OrderEventReason::Cascade,
            Self::KillSwitch => OrderEventReason::KillSwitch,
            Self::Flatten => OrderEventReason::Flatten,
            Self::NotMarketable => OrderEventReason::NotMarketable,
            Self::NoQuote => OrderEventReason::NoQuote,
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
    order.eligible_after_unix_nanos = owner_unix_nanos;
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

pub(crate) fn modify(order: Order, owner_unix_nanos: i64) -> Result<Order, LifecycleError> {
    modify_with_cause(order, ModificationCause::User, owner_unix_nanos).map(|(order, _)| order)
}

pub(crate) fn modify_with_cause(
    mut order: Order,
    cause: ModificationCause,
    owner_unix_nanos: i64,
) -> Result<(Order, OrderEventReason), LifecycleError> {
    if !matches!(
        order.status,
        OrderStatus::Working | OrderStatus::PartiallyFilled
    ) {
        return Err(LifecycleError::InvalidTransition(order.status));
    }
    advance(&mut order, owner_unix_nanos)?;
    order.eligible_after_unix_nanos = owner_unix_nanos;
    Ok((order, cause.reason()))
}

/// Activates a held group leg; it cannot fill on the activation quote.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "M5.5 introduces real inactive group legs")
)]
pub(crate) fn activate(mut order: Order, owner_unix_nanos: i64) -> Result<Order, LifecycleError> {
    if order.status != OrderStatus::Inactive {
        return Err(LifecycleError::InvalidTransition(order.status));
    }
    advance(&mut order, owner_unix_nanos)?;
    order.status = OrderStatus::Working;
    order.eligible_after_unix_nanos = owner_unix_nanos;
    Ok(order)
}

/// Resizes group coverage without changing the order identity or fill history.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "M5.5 introduces group coverage")
)]
pub(crate) fn resize(
    mut order: Order,
    quantity: FixedPoint,
    cause: ResizeCause,
    owner_unix_nanos: i64,
) -> Result<(Order, OrderEventReason), LifecycleError> {
    if !matches!(
        order.status,
        OrderStatus::Inactive | OrderStatus::Working | OrderStatus::PartiallyFilled
    ) {
        return Err(LifecycleError::InvalidTransition(order.status));
    }
    if quantity.scale() != order.quantity.scale()
        || quantity.units() <= order.filled_quantity.units()
        || match cause {
            ResizeCause::CoverageGrow => quantity.units() <= order.quantity.units(),
            ResizeCause::CoverageShrink => quantity.units() >= order.quantity.units(),
        }
    {
        return Err(LifecycleError::InvalidQuantity);
    }
    advance(&mut order, owner_unix_nanos)?;
    order.quantity = quantity;
    order.eligible_after_unix_nanos = owner_unix_nanos;
    Ok((order, cause.reason()))
}

/// Emits a Triggered event alongside the returned order, without a new status.
pub(crate) fn trigger(
    mut order: Order,
    owner_unix_nanos: i64,
) -> Result<(Order, OrderEventKind), LifecycleError> {
    if order.status != OrderStatus::Working
        || !matches!(order.order_type, OrderType::Stop | OrderType::StopLimit)
        || order.triggered_unix_nanos.is_some()
    {
        return Err(LifecycleError::InvalidTransition(order.status));
    }
    advance(&mut order, owner_unix_nanos)?;
    order.triggered_unix_nanos = Some(owner_unix_nanos);
    Ok((order, OrderEventKind::Triggered))
}

/// Only a due DAY order may expire; the scheduled deadline remains unchanged.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "M5.4 wires the DAY owner timer")
)]
pub(crate) fn expire(mut order: Order, owner_unix_nanos: i64) -> Result<Order, LifecycleError> {
    if !matches!(
        order.status,
        OrderStatus::Inactive | OrderStatus::Working | OrderStatus::PartiallyFilled
    ) {
        return Err(LifecycleError::InvalidTransition(order.status));
    }
    if order.time_in_force != TimeInForce::Day
        || order
            .expires_at_unix_nanos
            .is_none_or(|due| owner_unix_nanos < due)
    {
        return Err(LifecycleError::InvalidTimestamp);
    }
    advance(&mut order, owner_unix_nanos)?;
    order.status = OrderStatus::Expired;
    order.terminal_unix_nanos = Some(owner_unix_nanos);
    Ok(order)
}

/// Refuses a pending broker admission, not a previously executable order.
pub(crate) fn reject(mut order: Order, owner_unix_nanos: i64) -> Result<Order, LifecycleError> {
    if order.status != OrderStatus::Pending {
        return Err(LifecycleError::InvalidTransition(order.status));
    }
    advance(&mut order, owner_unix_nanos)?;
    order.status = OrderStatus::Rejected;
    order.terminal_unix_nanos = Some(owner_unix_nanos);
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
    if order.status == OrderStatus::Inactive {
        return Err(LifecycleError::InvalidTransition(order.status));
    }
    cancel_with_cause(order, CancellationCause::User, command_unix_nanos).map(|(order, _)| order)
}

pub(crate) fn cancel_with_cause(
    order: Order,
    cause: CancellationCause,
    command_unix_nanos: i64,
) -> Result<(Order, OrderEventReason), LifecycleError> {
    if !matches!(
        order.status,
        OrderStatus::Inactive | OrderStatus::Working | OrderStatus::PartiallyFilled
    ) {
        return Err(LifecycleError::InvalidTransition(order.status));
    }
    let mut cancelled = order;
    advance(&mut cancelled, command_unix_nanos)?;
    cancelled.status = OrderStatus::Cancelled;
    cancelled.terminal_unix_nanos = Some(command_unix_nanos);
    Ok((cancelled, cause.reason()))
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
    if !matches!(
        current.status,
        OrderStatus::Pending
            | OrderStatus::Working
            | OrderStatus::PartiallyFilled
            | OrderStatus::PendingModify
            | OrderStatus::PendingCancel
    ) {
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
    if order.status == OrderStatus::Pending {
        return reject(order, owner_unix_nanos).map(Some);
    }
    order.status = match order.status {
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
    if !matches!(
        order.status,
        OrderStatus::Pending
            | OrderStatus::Working
            | OrderStatus::PartiallyFilled
            | OrderStatus::PendingModify
            | OrderStatus::PendingCancel
    ) {
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
    if current.status == OrderStatus::Inactive {
        return Err(LifecycleError::InvalidTransition(current.status));
    }
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
    let status = if !current.status.is_open()
        || matches!(
            current.status,
            OrderStatus::PendingModify | OrderStatus::PendingCancel
        ) {
        current.status
    } else if filled == quantity {
        OrderStatus::Filled
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
    use aeris_trading::{OrderEventReason, OrderType, TimeInForce};

    #[test]
    fn simulated_lifecycle_transition_table() {
        use OrderStatus as S;
        let statuses = [
            S::Inactive,
            S::Pending,
            S::Working,
            S::PendingModify,
            S::PartiallyFilled,
            S::PendingCancel,
            S::Filled,
            S::Cancelled,
            S::Expired,
            S::Rejected,
        ];
        for status in statuses {
            let mut order = super::super::tests::order(1, status);
            order.order_type = OrderType::StopLimit;
            order.stop_price = order.limit_price;
            order.time_in_force = TimeInForce::Day;
            order.expires_at_unix_nanos = Some(2);
            let active = matches!(status, S::Working | S::PartiallyFilled);
            let open = matches!(status, S::Inactive | S::Working | S::PartiallyFilled);
            for (edge, result, allowed) in [
                (
                    "activate",
                    activate(order.clone(), 2),
                    status == S::Inactive,
                ),
                (
                    "modify",
                    modify_with_cause(order.clone(), ModificationCause::Management, 2)
                        .map(|(o, _)| o),
                    active,
                ),
                (
                    "resize",
                    resize(
                        order.clone(),
                        FixedPoint::try_new(3, 0).expect("quantity"),
                        ResizeCause::CoverageGrow,
                        2,
                    )
                    .map(|(o, _)| o),
                    open,
                ),
                (
                    "trigger",
                    trigger(order.clone(), 2).map(|(order, _)| order),
                    status == S::Working,
                ),
                ("fill", fill_simulated(order.clone(), 2), active),
                (
                    "cancel",
                    cancel_with_cause(order.clone(), CancellationCause::Cascade, 2).map(|(o, _)| o),
                    open,
                ),
                ("expire", expire(order.clone(), 2), open),
                ("reject", reject(order.clone(), 2), status == S::Pending),
            ] {
                assert_eq!(result.is_ok(), allowed, "{status:?} {edge}");
                if let Ok(updated) = result {
                    assert_eq!(updated.revision, order.revision + 1, "{status:?} {edge}");
                    assert_eq!(updated.updated_unix_nanos, 2, "{status:?} {edge}");
                    assert_eq!(updated.accepted_unix_nanos, order.accepted_unix_nanos);
                }
            }
        }
    }

    #[test]
    fn simulated_lifecycle_causes_and_deadlines() {
        use OrderStatus as S;
        for cause in [
            ModificationCause::User,
            ModificationCause::Management,
            ModificationCause::Coverage,
            ModificationCause::LegMoved,
            ModificationCause::ParentRepriced,
        ] {
            let (modified, reason) =
                modify_with_cause(super::super::tests::order(1, S::Working), cause, 2)
                    .expect("modify");
            assert_eq!(reason, cause.reason());
            assert_eq!(modified.eligible_after_unix_nanos, 2);
        }
        for cause in [
            CancellationCause::User,
            CancellationCause::Cascade,
            CancellationCause::KillSwitch,
            CancellationCause::Flatten,
            CancellationCause::NotMarketable,
            CancellationCause::NoQuote,
        ] {
            let (cancelled, reason) =
                cancel_with_cause(super::super::tests::order(1, S::Inactive), cause, 2)
                    .expect("cancel");
            assert_eq!(reason, cause.reason());
            assert_eq!(cancelled.terminal_unix_nanos, Some(2));
        }
        let working = super::super::tests::order(1, S::Working);
        assert!(trigger(working.clone(), 2).is_err(), "only stops trigger");
        let mut stop = working.clone();
        stop.order_type = OrderType::StopLimit;
        stop.stop_price = stop.limit_price;
        let (triggered, kind) = trigger(stop, 2).expect("trigger");
        assert_eq!(kind, OrderEventKind::Triggered);
        assert_eq!(triggered.status, S::Working);
        assert_eq!(triggered.triggered_unix_nanos, Some(2));
        assert!(trigger(triggered, 3).is_err(), "one trigger only");
        let mut day = working;
        day.time_in_force = TimeInForce::Day;
        day.expires_at_unix_nanos = Some(3);
        assert!(expire(day.clone(), 2).is_err(), "too early");
        let expired = expire(day, 3).expect("due");
        assert_eq!(expired.terminal_unix_nanos, Some(3));
        assert_eq!(expired.expires_at_unix_nanos, Some(3));
        assert!(fill_simulated(expired, 4).is_err(), "expired cannot fill");
        let mut covered = super::super::tests::order(1, S::Working);
        covered.quantity = FixedPoint::try_new(3, 0).expect("quantity");
        let (shrunk, reason) = resize(
            covered.clone(),
            FixedPoint::try_new(2, 0).expect("quantity"),
            ResizeCause::CoverageShrink,
            2,
        )
        .expect("shrink");
        assert_eq!(reason, OrderEventReason::CoverageShrink);
        assert_eq!(shrunk.quantity.units(), 2);
        assert!(
            resize(
                covered,
                FixedPoint::try_new(4, 0).expect("quantity"),
                ResizeCause::CoverageShrink,
                2,
            )
            .is_err(),
            "direction must match cause"
        );
        assert_eq!(
            OrderEventReason::Cascade,
            CancellationCause::Cascade.reason()
        );
    }

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
    fn owner_eligibility_advances_only_on_executable_acceptance_and_modification() {
        let mut order = super::super::tests::order(1, OrderStatus::Working);
        order.revision = 0;
        order.accepted_unix_nanos = 0;
        order.updated_unix_nanos = 0;
        order.eligible_after_unix_nanos = 0;
        let accepted = accept(order, 12).expect("accept");
        assert_eq!(accepted.eligible_after_unix_nanos, 12);
        let modified = modify(accepted, 23).expect("modify");
        assert_eq!(modified.eligible_after_unix_nanos, 23);
        let cancelled = cancel(modified, 34).expect("cancel");
        assert_eq!(cancelled.eligible_after_unix_nanos, 23);
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
    fn expired_order_cannot_be_reopened_by_late_full_deal() {
        let mut expired = super::super::tests::order(1, OrderStatus::Expired);
        expired.terminal_unix_nanos = Some(2);
        let late = broker_deal(
            &expired,
            expired.quantity,
            false,
            expired.provenance.clone(),
            3,
        )
        .expect("late accounting");
        assert_eq!(late.status, OrderStatus::Expired);
        assert_eq!(late.filled_quantity, expired.quantity);
        assert_eq!(late.terminal_unix_nanos, Some(2));
        assert_eq!((late.revision, late.updated_unix_nanos), (2, 3));
    }

    #[test]
    fn broker_deal_transition_table_preserves_pending_and_terminal_states() {
        let cases = [
            (OrderStatus::Inactive, None, None),
            (
                OrderStatus::Pending,
                Some(OrderStatus::PartiallyFilled),
                Some(OrderStatus::Filled),
            ),
            (
                OrderStatus::Working,
                Some(OrderStatus::PartiallyFilled),
                Some(OrderStatus::Filled),
            ),
            (
                OrderStatus::PartiallyFilled,
                Some(OrderStatus::PartiallyFilled),
                Some(OrderStatus::Filled),
            ),
            (
                OrderStatus::PendingModify,
                Some(OrderStatus::PendingModify),
                Some(OrderStatus::PendingModify),
            ),
            (
                OrderStatus::PendingCancel,
                Some(OrderStatus::PendingCancel),
                Some(OrderStatus::PendingCancel),
            ),
            (
                OrderStatus::Filled,
                Some(OrderStatus::Filled),
                Some(OrderStatus::Filled),
            ),
            (
                OrderStatus::Cancelled,
                Some(OrderStatus::Cancelled),
                Some(OrderStatus::Cancelled),
            ),
            (
                OrderStatus::Expired,
                Some(OrderStatus::Expired),
                Some(OrderStatus::Expired),
            ),
            (
                OrderStatus::Rejected,
                Some(OrderStatus::Rejected),
                Some(OrderStatus::Rejected),
            ),
        ];
        for (status, partial_status, complete_status) in cases {
            for (filled_units, expected) in [(1, partial_status), (2, complete_status)] {
                let mut order = super::super::tests::order(1, status);
                order.quantity = FixedPoint::try_new(2, 0).expect("quantity");
                if !status.is_open() {
                    order.terminal_unix_nanos = Some(2);
                }
                let transition = broker_deal(
                    &order,
                    FixedPoint::try_new(filled_units, 0).expect("filled"),
                    false,
                    order.provenance.clone(),
                    3,
                );
                match expected {
                    Some(expected) => {
                        let updated = transition.expect("allowed accounting");
                        assert_eq!(updated.status, expected, "{status:?}, {filled_units}");
                        assert_eq!(updated.revision, 2, "{status:?}, {filled_units}");
                        assert_eq!(
                            updated.terminal_unix_nanos,
                            if expected.is_open() {
                                None
                            } else if status.is_open() {
                                Some(3)
                            } else {
                                Some(2)
                            },
                            "{status:?}, {filled_units}"
                        );
                    }
                    None => assert!(transition.is_err(), "{status:?}, {filled_units}"),
                }
            }
        }
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

    #[test]
    fn broker_transition_table_excludes_held_and_terminal_orders() {
        let sample = super::super::tests::order(1, OrderStatus::Working);
        let report = BrokerOrder {
            broker_order_id: "broker-1".into(),
            client_order_id: Some(sample.client_order_id.as_str().into()),
            instrument_id: sample.instrument_id.clone(),
            side: sample.side,
            kind: BrokerOrderKind::Limit,
            quantity: sample.quantity,
            filled_quantity: sample.filled_quantity,
            limit_price: sample.limit_price,
            stop_price: sample.stop_price,
            broker_position_id: None,
            closing: false,
        };
        for status in [
            OrderStatus::Pending,
            OrderStatus::Working,
            OrderStatus::PartiallyFilled,
            OrderStatus::PendingModify,
            OrderStatus::PendingCancel,
        ] {
            let current = super::super::tests::order(1, status);
            assert!(
                broker_report(
                    &current,
                    &report,
                    BrokerOrderState::Expired,
                    false,
                    false,
                    2
                )
                .expect("allowed report")
                .is_some(),
                "{status:?}"
            );
            assert!(broker_missing(current, true, 2).is_ok(), "{status:?}");
        }
        for status in [
            OrderStatus::Inactive,
            OrderStatus::Filled,
            OrderStatus::Cancelled,
            OrderStatus::Expired,
            OrderStatus::Rejected,
        ] {
            let current = super::super::tests::order(1, status);
            assert!(
                broker_report(
                    &current,
                    &report,
                    BrokerOrderState::Expired,
                    false,
                    false,
                    2
                )
                .is_err(),
                "{status:?}"
            );
            assert!(broker_missing(current, true, 2).is_err(), "{status:?}");
        }
    }

    #[test]
    fn broker_deal_rejects_inactive_but_keeps_late_terminal_accounting() {
        let inactive = super::super::tests::order(1, OrderStatus::Inactive);
        assert!(
            broker_deal(
                &inactive,
                inactive.quantity,
                false,
                inactive.provenance.clone(),
                2
            )
            .is_err()
        );
        for status in [
            OrderStatus::Pending,
            OrderStatus::Working,
            OrderStatus::PartiallyFilled,
            OrderStatus::PendingModify,
            OrderStatus::PendingCancel,
            OrderStatus::Filled,
            OrderStatus::Cancelled,
            OrderStatus::Expired,
            OrderStatus::Rejected,
        ] {
            let current = super::super::tests::order(1, status);
            assert!(
                broker_deal(
                    &current,
                    current.quantity,
                    false,
                    current.provenance.clone(),
                    2,
                )
                .is_ok(),
                "{status:?}"
            );
        }
    }

    #[test]
    fn admission_and_request_edge_matrix_has_no_implicit_status_transitions() {
        let statuses = [
            OrderStatus::Inactive,
            OrderStatus::Pending,
            OrderStatus::Working,
            OrderStatus::PendingModify,
            OrderStatus::PartiallyFilled,
            OrderStatus::PendingCancel,
            OrderStatus::Filled,
            OrderStatus::Cancelled,
            OrderStatus::Expired,
            OrderStatus::Rejected,
        ];
        for status in statuses {
            let mut new = super::super::tests::order(1, status);
            new.revision = 0;
            new.accepted_unix_nanos = 0;
            new.updated_unix_nanos = 0;
            assert_eq!(
                accept(new, 2).is_ok(),
                matches!(
                    status,
                    OrderStatus::Inactive | OrderStatus::Pending | OrderStatus::Working
                ),
                "admission: {status:?}"
            );
            let original = super::super::tests::order(1, status);
            let executable = matches!(status, OrderStatus::Working | OrderStatus::PartiallyFilled);
            assert_eq!(
                fill_simulated(original.clone(), 2).is_ok(),
                executable,
                "fill: {status:?}"
            );
            for pending in statuses {
                assert_eq!(
                    request(original.clone(), pending, 2).is_ok(),
                    executable
                        && matches!(
                            pending,
                            OrderStatus::PendingModify | OrderStatus::PendingCancel
                        ),
                    "request: {status:?} -> {pending:?}"
                );
            }
        }
    }
}
