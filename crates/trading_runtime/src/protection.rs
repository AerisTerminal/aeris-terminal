//! Position protection in the simulated venue. Every open stop loss and take profit on a
//! position, standalone or from a managed bracket, together cover exactly that position: they grow
//! when the position grows, shrink when it is partly closed and leave once it closes or reverses,
//! so protection never opens a position of its own.

use crate::{
    Coordinator, ManagedBracket, ManagedBracketStatus, ProtectiveOrderRole,
    SimulatedMarketObservation, VenueRoute, current_unix_nanos,
};
use aeris_instruments::InstrumentId;
use aeris_trading::{
    ClientOrderId, Fill, FixedPoint, Order, OrderEvent, OrderEventId, OrderEventKind, OrderSide,
    TradingAccountId,
};
use std::collections::BTreeSet;

/// When a protective order triggers relative to others of its role, and at what price. A lower
/// key triggers first: a sell protects a long position, so its highest stop and its lowest target
/// trigger first, and a buy protecting a short is the mirror image.
pub(crate) fn protection_trigger(
    order: &Order,
    role: ProtectiveOrderRole,
) -> Option<(i64, FixedPoint)> {
    let protects_long = order.side == OrderSide::Sell;
    let (price, highest_first) = match role {
        ProtectiveOrderRole::StopLoss => (order.stop_price.or(order.limit_price), protects_long),
        ProtectiveOrderRole::TakeProfit => (order.limit_price.or(order.stop_price), !protects_long),
    };
    let price = price?;
    let key = if highest_first {
        price.units().saturating_neg()
    } else {
        price.units()
    };
    Some((key, price))
}

/// The protective role a managed bracket gives an order, if the order is its stop or a target.
pub(crate) fn managed_bracket_role(
    bracket: &ManagedBracket,
    client_order_id: &ClientOrderId,
) -> Option<ProtectiveOrderRole> {
    if bracket.stop_client_order_id.as_ref() == Some(client_order_id) {
        Some(ProtectiveOrderRole::StopLoss)
    } else if bracket.target_client_order_ids.contains(client_order_id) {
        Some(ProtectiveOrderRole::TakeProfit)
    } else {
        None
    }
}

impl Coordinator {
    /// The protective role of an order: a managed bracket's stop or target, or a standalone
    /// stop loss or take profit.
    pub(crate) fn protection_role(
        &self,
        client_order_id: &ClientOrderId,
    ) -> Option<ProtectiveOrderRole> {
        self.state
            .managed_brackets
            .values()
            .find_map(|bracket| managed_bracket_role(bracket, client_order_id))
            .or_else(|| {
                self.state
                    .protective_orders
                    .get(client_order_id)
                    .map(|protective| protective.role)
            })
    }

    /// Fits the protection of every position the fills touched to that position.
    pub(crate) fn align_position_protection(
        &mut self,
        fills: &[Fill],
        observation: &SimulatedMarketObservation,
    ) -> Result<(), String> {
        let touched = fills
            .iter()
            .map(|fill| (fill.account_id.clone(), fill.instrument_id.clone()))
            .collect::<BTreeSet<_>>();
        for (account_id, instrument_id) in touched {
            self.align_protection(
                &account_id,
                &instrument_id,
                observation.provenance.observed_unix_nanos,
            )?;
        }
        Ok(())
    }

    /// Fits the protection of every practice position after a restart, finishing an alignment a
    /// stop interrupted between a fill and its resize.
    pub(crate) fn align_loaded_protection(&mut self) -> Result<(), String> {
        let positions = self
            .state
            .positions
            .keys()
            .filter(|(account_id, _)| {
                self.route(account_id)
                    .is_ok_and(|route| route == VenueRoute::Simulated)
            })
            .cloned()
            .collect::<Vec<_>>();
        let now = current_unix_nanos()?;
        for (account_id, instrument_id) in positions {
            self.align_protection(&account_id, &instrument_id, now)?;
        }
        Ok(())
    }

    fn align_protection(
        &mut self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
        event_unix_nanos: i64,
    ) -> Result<(), String> {
        let net = self
            .state
            .positions
            .get(&(account_id.clone(), instrument_id.clone()))
            .map(|position| position.net_quantity)
            .filter(|net| net.units() != 0);
        // A sell protects a long position and a buy a short one.
        let (protecting, stale): (Vec<_>, Vec<_>) = self
            .protection_orders(account_id, instrument_id)
            .into_iter()
            .partition(|(order, _)| {
                net.is_some_and(|net| order.side.sign() == -net.units().signum())
            });
        self.cancel_protection(
            &stale
                .into_iter()
                .map(|(order, _)| order.client_order_id)
                .collect::<Vec<_>>(),
        )?;
        let Some(net) = net else {
            return Ok(());
        };
        for role in [
            ProtectiveOrderRole::StopLoss,
            ProtectiveOrderRole::TakeProfit,
        ] {
            let orders = protecting
                .iter()
                .filter(|(_, order_role)| *order_role == role)
                .map(|(order, _)| order.clone())
                .collect();
            self.fit_protection(orders, role, net, event_unix_nanos)?;
        }
        Ok(())
    }

    fn protection_orders(
        &self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
    ) -> Vec<(Order, ProtectiveOrderRole)> {
        self.state
            .orders
            .values()
            .filter(|order| {
                order.status.is_open()
                    && &order.account_id == account_id
                    && &order.instrument_id == instrument_id
            })
            .filter_map(|order| {
                self.protection_role(&order.client_order_id)
                    .map(|role| (order.clone(), role))
            })
            .collect()
    }

    /// Makes the orders of one role cover the position exactly. Growth goes to the order that
    /// triggers first, the one the position reports; a shrink is taken from the orders that
    /// trigger last.
    fn fit_protection(
        &mut self,
        mut orders: Vec<Order>,
        role: ProtectiveOrderRole,
        net: FixedPoint,
        event_unix_nanos: i64,
    ) -> Result<(), String> {
        let Some(scale) = orders.first().map(|order| order.quantity.scale()) else {
            return Ok(());
        };
        orders.sort_by_key(|order| protection_trigger(order, role).map(|(key, _)| key));
        let units = |quantity: FixedPoint| {
            quantity
                .exact_rescale(scale)
                .map(|quantity| quantity.units().unsigned_abs())
                .map_err(|error| error.to_string())
        };
        let wanted = units(net)?;
        let covered = orders.iter().try_fold(0_u64, |total, order| {
            total
                .checked_add(units(order.quantity)?)
                .ok_or_else(|| "protection quantity overflowed".to_string())
        })?;
        if covered < wanted {
            let first = &orders[0];
            let grown = units(first.quantity)? + (wanted - covered);
            return self.resize_protection(first, grown, scale, event_unix_nanos);
        }
        let mut excess = covered - wanted;
        let mut cancelled = Vec::new();
        for order in orders.iter().rev() {
            if excess == 0 {
                break;
            }
            let quantity = units(order.quantity)?;
            if quantity <= excess {
                cancelled.push(order.client_order_id.clone());
                excess -= quantity;
            } else {
                self.resize_protection(order, quantity - excess, scale, event_unix_nanos)?;
                excess = 0;
            }
        }
        self.cancel_protection(&cancelled)
    }

    fn resize_protection(
        &mut self,
        order: &Order,
        units: u64,
        scale: u8,
        event_unix_nanos: i64,
    ) -> Result<(), String> {
        let quantity = FixedPoint::try_new(
            i64::try_from(units).map_err(|_| "protection quantity overflowed".to_string())?,
            scale,
        )
        .map_err(|error| error.to_string())?;
        let resized = Order {
            quantity,
            ..order.clone()
        };
        resized.validate().map_err(|error| error.to_string())?;
        let sequence = self.state.next_sequence;
        let event = OrderEvent {
            id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                .map_err(|error| error.to_string())?,
            order_id: order.id.clone(),
            sequence,
            kind: OrderEventKind::Modified,
            event_unix_nanos,
            detail: Some(format!(
                "{} protection resized to the position",
                ManagedBracket::MANAGEMENT_LABEL
            )),
            provenance: order.provenance.clone(),
        };
        event.validate().map_err(|error| error.to_string())?;
        self.store.modify_order(&resized, &event, sequence + 1)?;
        self.state.next_sequence = sequence + 1;
        self.state.order_events.push_back(event);
        self.state.orders.insert(resized.id.clone(), resized);
        self.bump_revision()
    }

    /// Cancels protective orders and completes any managed bracket left with nothing open.
    fn cancel_protection(&mut self, client_order_ids: &[ClientOrderId]) -> Result<(), String> {
        if client_order_ids.is_empty() {
            return Ok(());
        }
        for client_order_id in client_order_ids {
            self.cancel_order_internal(client_order_id, false)?;
        }
        let finished = self
            .state
            .managed_brackets
            .iter()
            .filter(|(_, bracket)| {
                let mut children = bracket
                    .stop_client_order_id
                    .iter()
                    .chain(&bracket.target_client_order_ids);
                bracket.status == ManagedBracketStatus::Active
                    && children.clone().any(|id| client_order_ids.contains(id))
                    && children.all(|id| {
                        !self
                            .order_by_client_id(id)
                            .is_ok_and(|order| order.status.is_open())
                    })
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for bracket_id in finished {
            if let Some(mut bracket) = self.state.managed_brackets.get(&bracket_id).cloned() {
                bracket.status = ManagedBracketStatus::Completed;
                self.store.put_managed_bracket(&bracket)?;
                self.state.managed_brackets.insert(bracket_id, bracket);
            }
        }
        Ok(())
    }
}
