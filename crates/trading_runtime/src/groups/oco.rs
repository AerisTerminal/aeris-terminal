//! The group owner for explicit one-cancels-the-other pairs on the simulated venue. Both
//! members are full working orders that commit with their group in one order step; a
//! member's full fill cancels its siblings inside the same quote step, before any later
//! fill candidate is evaluated.

use super::model::{GroupKind, GroupLeg, GroupStatus, LegRole, OrderGroup, StopManagement};
use super::{Order, OrderStatus, OrderStep, PlaceOco, TradingCore};
use crate::{MAXIMUM_OPEN_ORDERS, is_simulated};
use aeris_trading::RejectReason;

impl TradingCore {
    /// Places both members of an explicit OCO pair atomically: they rest as ordinary
    /// working orders, and their group commits in the same order step, so a failed commit
    /// leaves neither member nor group behind.
    pub(crate) fn place_owned_oco(&mut self, pair: &PlaceOco) -> Result<[Order; 2], String> {
        let (first, second) = (&pair.first, &pair.second);
        if first.client_order_id == second.client_order_id {
            return Err("OCO members need distinct client order ids".to_string());
        }
        if first.account_id != second.account_id || first.instrument_id != second.instrument_id {
            return Err("OCO members share one account and instrument".to_string());
        }
        if !is_simulated(&self.state.accounts, &first.account_id) {
            return Err("explicit OCO pairs are simulated-only".to_string());
        }
        // Both members are real open orders and share the open-order capacity bound.
        if self.state.orders.open_orders().count() + 2 > MAXIMUM_OPEN_ORDERS {
            return Err(RejectReason::CapacityExceeded.to_string());
        }
        let sequence = self.state.next_sequence;
        let (first_order, first_event) = self.prepare_single_order(
            first.clone(),
            true,
            first.stop_price.is_some(),
            sequence,
            &[],
        )?;
        let (second_order, second_event) = self.prepare_single_order(
            second.clone(),
            true,
            second.stop_price.is_some(),
            sequence + 1,
            &[],
        )?;
        let group = OrderGroup {
            group_id: format!("oco/{}", first_order.client_order_id.as_str()),
            account_id: first_order.account_id.clone(),
            instrument_id: first_order.instrument_id.clone(),
            kind: GroupKind::Oco,
            status: GroupStatus::Active,
            entry: None,
            // The V23 leg-role CHECK predates live OCO writers, so the two equal members
            // reuse the two stored labels positionally (first member stop_loss, second
            // take_profit 0); the group kind carries the OCO semantics.
            legs: vec![
                GroupLeg {
                    order_id: first_order.id.clone(),
                    role: LegRole::StopLoss,
                    offset_ticks: None,
                    allocation_percent: None,
                },
                GroupLeg {
                    order_id: second_order.id.clone(),
                    role: LegRole::TakeProfit { index: 0 },
                    offset_ticks: None,
                    allocation_percent: None,
                },
            ],
            management: StopManagement::default(),
            template: None,
            revision: 1,
        };
        group
            .validate()
            .map_err(|error| format!("invalid order group: {error:?}"))?;
        let mut step = OrderStep::new(sequence + 2);
        step.inserted = vec![
            (first_order.clone(), first_event),
            (second_order.clone(), second_event),
        ];
        step.groups.push(group);
        self.commit_order_step(step, Vec::new())?;
        Ok([first_order, second_order])
    }

    /// Settles an OCO group whose member filled completely: every sibling still open is
    /// cancelled with the group inside the same quote step.
    pub(super) fn settle_owned_oco_fill(
        &mut self,
        group_id: &str,
        filled: &Order,
    ) -> Result<(), String> {
        debug_assert_eq!(filled.status, OrderStatus::Filled);
        let mut group = self
            .state
            .order_groups
            .get(group_id)
            .cloned()
            .ok_or_else(|| "order group disappeared".to_string())?;
        let legs = group.legs.clone();
        for leg in &legs {
            if leg.order_id == filled.id {
                continue;
            }
            let Some(order) = self.state.orders.get(&leg.order_id).cloned() else {
                continue;
            };
            if order.status.is_open() {
                self.cancel_owned_leg(&order, "cancelled with its OCO group".to_string())?;
            }
        }
        group.status = GroupStatus::Completed;
        group.revision = group
            .revision
            .checked_add(1)
            .ok_or_else(|| "order group revision overflowed".to_string())?;
        self.state.order_groups.insert(group_id.to_string(), group);
        self.bump_revision()
    }
}
