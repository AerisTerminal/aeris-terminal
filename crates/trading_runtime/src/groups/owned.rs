//! The group owner for simulated brackets placed after the authority switch. The entry, its
//! Inactive stop and target orders and the `OrderGroup` row commit in one order step; the
//! entry fill's quote step reprices and activates those same order ids. Legacy
//! `managed_brackets` rows stay with the legacy writer until their migration converts them.

use super::model::{GroupKind, GroupLeg, GroupStatus, LegRole, OrderGroup};
use super::{
    BracketPlacement, BracketStrategyTemplate, BracketTarget, Fill, FixedPoint, ManagedBracket,
    ManagedBracketStatus, Order, OrderEvent, OrderEventId, OrderEventKind, OrderSide, OrderStatus,
    OrderStep, OrderType, PlaceOrder, SimulatedMarketObservation, TimeInForce, TradingCore,
    TradingInstrument, allocated_target_quantities, favorable_tick_distance, instrument_tick_units,
    managed_child_client_order_id, opposite_side, order_increment_units,
    planned_leg::MovePlannedBracketLeg, planned_leg::PlannedBracketLeg, shifted_price,
    tighter_stop,
};
use crate::book::lifecycle::{self, CancellationCause, ModificationCause};
use crate::{
    MAXIMUM_MANAGED_BRACKETS, MAXIMUM_OPEN_ORDERS, ProtectiveOrderRole, RecoveryNotice,
    RecoveryNoticeKind,
};
use aeris_trading::{OrderEventReason, RejectReason};

/// A group leg's planned activation: reprice and resize the held order, then activate it.
struct LegActivation {
    order: Order,
    event: OrderEvent,
}

/// The prepared held legs of a pending bracket: group membership, accepted orders and the
/// next free sequence after them.
struct OwnedLegs {
    legs: Vec<GroupLeg>,
    orders: Vec<(Order, OrderEvent)>,
    next_sequence: u64,
}

/// How one fill moves the group that owns the filled order.
enum OwnedFill {
    ActivateBracket,
    SettleBracket,
    SettleOco,
    RetireProtection,
}

/// What one held leg of a pending bracket becomes at placement.
struct HeldLeg {
    client_order_id: aeris_trading::ClientOrderId,
    order_type: OrderType,
    price: Option<FixedPoint>,
    quantity: FixedPoint,
    detail: String,
}

impl TradingCore {
    /// Places a simulated bracket under group authority: the entry, its held legs and the
    /// pending group are one order step, so a failed commit leaves none of them behind.
    pub(super) fn place_owned_bracket(
        &mut self,
        entry_command: &PlaceOrder,
        template: &BracketStrategyTemplate,
    ) -> Result<BracketPlacement, String> {
        let group_id = entry_command.client_order_id.as_str().to_string();
        if let Some(existing) = self.state.managed_brackets.get(&group_id) {
            return Ok(BracketPlacement::Managed(existing.clone()));
        }
        if let Some(group) = self.state.order_groups.get(&group_id).cloned() {
            return Ok(BracketPlacement::Managed(
                self.managed_bracket_view(&group)?,
            ));
        }
        let owned = self
            .state
            .order_groups
            .values()
            .filter(|group| matches!(group.status, GroupStatus::Pending | GroupStatus::Active))
            .count();
        let retired = if self.state.managed_brackets.len() + owned >= MAXIMUM_MANAGED_BRACKETS {
            Some(
                self.retired_managed_bracket()?
                    .ok_or_else(|| "active managed bracket limit reached".to_string())?,
            )
        } else {
            None
        };
        let instrument = self
            .state
            .instruments
            .get(&entry_command.instrument_id)
            .ok_or_else(|| "trading instrument is not registered".to_string())?
            .clone();
        let target_quantities = allocated_target_quantities(
            entry_command.quantity,
            &template.targets,
            order_increment_units(&instrument),
        )?;
        let leg_count = 1 + target_quantities.len();
        // Held legs are real orders and share the open-order capacity bound; the entry's own
        // admission runs afterwards with its usual checks.
        if self.state.orders.open_orders().count() + 1 + leg_count > MAXIMUM_OPEN_ORDERS {
            return Err(RejectReason::CapacityExceeded.to_string());
        }
        let sequence = self.state.next_sequence;
        let (entry, entry_event) =
            self.prepare_single_order(entry_command.clone(), true, sequence, &[])?;
        let base = entry.limit_price.or(entry.stop_price);
        let tick_units = base
            .map(|base| instrument_tick_units(&instrument, base))
            .transpose()?;
        let prepared = self.prepare_owned_legs(
            &entry,
            template,
            &target_quantities,
            base,
            tick_units,
            sequence + 1,
        )?;
        let mut inserted = Vec::with_capacity(1 + leg_count);
        inserted.push((entry.clone(), entry_event));
        inserted.extend(prepared.orders);
        let group = OrderGroup {
            group_id: group_id.clone(),
            account_id: entry.account_id.clone(),
            instrument_id: entry.instrument_id.clone(),
            kind: GroupKind::Bracket,
            status: GroupStatus::Pending,
            entry: Some(entry.id.clone()),
            legs: prepared.legs,
            management: super::model::StopManagement {
                trailing: template.trailing_stop,
                break_even: template.break_even,
                entry_price: None,
            },
            template: Some((template.template_id.clone(), template.revision)),
            revision: 1,
        };
        group
            .validate()
            .map_err(|error| format!("invalid order group: {error:?}"))?;
        let mut step = OrderStep::new(prepared.next_sequence);
        step.deleted_brackets.extend(retired);
        step.inserted = inserted;
        step.groups.push(group.clone());
        self.commit_order_step(step, Vec::new())?;
        Ok(BracketPlacement::Managed(
            self.managed_bracket_view(
                self.state
                    .order_groups
                    .get(&group_id)
                    .ok_or_else(|| "placed order group disappeared".to_string())?,
            )?,
        ))
    }

    /// Converts each legacy awaiting bracket into group authority once, at startup: a
    /// priceable row becomes a pending group whose held legs carry exactly the prices the
    /// legacy planned-leg projection showed, and an unpriceable row becomes a cancelled
    /// group with a recovery notice while its entry keeps working unmanaged. Each bracket
    /// converts in one order step, so a failed commit leaves the legacy row for the next
    /// start.
    pub(crate) fn convert_awaiting_brackets(&mut self) -> Result<(), String> {
        let awaiting = self
            .state
            .managed_brackets
            .values()
            .filter(|bracket| bracket.status == ManagedBracketStatus::AwaitingEntry)
            .cloned()
            .collect::<Vec<_>>();
        for bracket in awaiting {
            self.convert_awaiting_bracket(&bracket)?;
        }
        Ok(())
    }

    fn convert_awaiting_bracket(&mut self, bracket: &ManagedBracket) -> Result<(), String> {
        let entry = self
            .order_by_client_id(&bracket.entry_client_order_id)?
            .clone();
        if !entry.status.is_open() {
            // The legacy reconciler retires a stale bracket on the next quote; converting it
            // here would invent protection for an entry that can no longer fill.
            return Ok(());
        }
        let instrument = self
            .state
            .instruments
            .get(&entry.instrument_id)
            .cloned()
            .ok_or_else(|| "awaiting bracket entry instrument is not registered".to_string())?;
        let priced = match entry.limit_price.or(entry.stop_price) {
            Some(base) => instrument_tick_units(&instrument, base).and_then(|tick_units| {
                allocated_target_quantities(
                    entry.quantity,
                    &bracket.template.targets,
                    order_increment_units(&instrument),
                )
                .map(|quantities| (base, tick_units, quantities))
            }),
            None => Err("the bracket's entry has no resting price".to_string()),
        };
        let (mut step, notice) = match priced {
            Ok((base, tick_units, target_quantities)) => {
                let prepared = self.prepare_owned_legs(
                    &entry,
                    &bracket.template,
                    &target_quantities,
                    Some(base),
                    Some(tick_units),
                    self.state.next_sequence,
                )?;
                let group =
                    Self::converted_group(bracket, &entry, GroupStatus::Pending, prepared.legs)?;
                let mut step = OrderStep::new(prepared.next_sequence);
                step.inserted = prepared.orders;
                step.groups.push(group);
                (step, None)
            }
            Err(reason) => {
                let group =
                    Self::converted_group(bracket, &entry, GroupStatus::Cancelled, Vec::new())?;
                let mut step = OrderStep::new(self.state.next_sequence);
                step.groups.push(group);
                let notice = RecoveryNotice {
                    kind: RecoveryNoticeKind::UnpriceableBracket,
                    subject: bracket.bracket_id.clone(),
                    detail: format!(
                        "the bracket on {} could not be priced after the upgrade: {reason}",
                        entry.instrument_id.as_str()
                    ),
                };
                (step, Some(notice))
            }
        };
        step.deleted_brackets.push(bracket.bracket_id.clone());
        self.commit_order_step(step, Vec::new())?;
        if let Some(notice) = notice {
            self.recovery_notices.push(notice);
        }
        Ok(())
    }

    /// The group a converted legacy bracket becomes, keeping its identity and template.
    fn converted_group(
        bracket: &ManagedBracket,
        entry: &Order,
        status: GroupStatus,
        legs: Vec<GroupLeg>,
    ) -> Result<OrderGroup, String> {
        let group = OrderGroup {
            group_id: bracket.bracket_id.clone(),
            account_id: entry.account_id.clone(),
            instrument_id: entry.instrument_id.clone(),
            kind: GroupKind::Bracket,
            status,
            entry: Some(entry.id.clone()),
            legs,
            management: super::model::StopManagement {
                trailing: bracket.template.trailing_stop,
                break_even: bracket.template.break_even,
                entry_price: None,
            },
            template: Some((
                bracket.template.template_id.clone(),
                bracket.template.revision,
            )),
            revision: 1,
        };
        group
            .validate()
            .map_err(|error| format!("invalid order group: {error:?}"))?;
        Ok(group)
    }

    /// Builds the held stop and target legs of a pending bracket, priced from the entry's
    /// resting price when it has one and keeping each leg's tick offset for activation.
    fn prepare_owned_legs(
        &self,
        entry: &Order,
        template: &BracketStrategyTemplate,
        target_quantities: &[(usize, FixedPoint)],
        base: Option<FixedPoint>,
        tick_units: Option<i64>,
        mut next: u64,
    ) -> Result<OwnedLegs, String> {
        let leg_price = |offset_ticks: u32, direction: i64| -> Result<Option<FixedPoint>, String> {
            match (base, tick_units) {
                (Some(base), Some(tick_units)) => {
                    shifted_price(base, offset_ticks, direction, tick_units).map(Some)
                }
                _ => Ok(None),
            }
        };
        let mut legs = Vec::with_capacity(1 + target_quantities.len());
        let mut orders = Vec::with_capacity(1 + target_quantities.len());
        let (stop, stop_event) = self.prepare_inactive_leg(
            entry,
            HeldLeg {
                client_order_id: managed_child_client_order_id(&entry.client_order_id, "stop", 0)?,
                order_type: OrderType::Stop,
                price: leg_price(template.stop_offset_ticks, -entry.side.sign())?,
                quantity: entry.quantity,
                detail: format!("{} protective stop", ManagedBracket::MANAGEMENT_LABEL),
            },
            next,
        )?;
        next += 1;
        legs.push(GroupLeg {
            order_id: stop.id.clone(),
            role: LegRole::StopLoss,
            offset_ticks: Some(template.stop_offset_ticks),
            allocation_percent: None,
        });
        orders.push((stop, stop_event));
        for (leg_index, (index, quantity)) in target_quantities.iter().enumerate() {
            let target = template.targets[*index];
            let (order, event) = self.prepare_inactive_leg(
                entry,
                HeldLeg {
                    client_order_id: managed_child_client_order_id(
                        &entry.client_order_id,
                        "target",
                        u32::try_from(*index)
                            .map_err(|_| "bracket target index overflowed".to_string())?,
                    )?,
                    order_type: OrderType::Limit,
                    price: leg_price(target.offset_ticks, entry.side.sign())?,
                    quantity: *quantity,
                    detail: format!(
                        "{} scale-out target {}",
                        ManagedBracket::MANAGEMENT_LABEL,
                        leg_index + 1
                    ),
                },
                next,
            )?;
            next += 1;
            legs.push(GroupLeg {
                order_id: order.id.clone(),
                role: LegRole::TakeProfit {
                    index: u8::try_from(leg_index)
                        .map_err(|_| "bracket target index overflowed".to_string())?,
                },
                offset_ticks: Some(target.offset_ticks),
                allocation_percent: Some(target.quantity_percent),
            });
            orders.push((order, event));
        }
        // The allocator drops targets whose share rounds to zero; the last kept leg takes the
        // dropped shares so the bracket still covers the whole position, matching what the
        // allocator's remainder rule would arm at activation.
        let allocated_share = legs
            .iter()
            .filter_map(|leg| leg.allocation_percent)
            .map(u32::from)
            .sum::<u32>();
        if allocated_share < 100
            && let Some(last) = legs
                .iter_mut()
                .rev()
                .find(|leg| matches!(leg.role, LegRole::TakeProfit { .. }))
        {
            let adjusted =
                u32::from(last.allocation_percent.unwrap_or(0)) + (100 - allocated_share);
            last.allocation_percent = Some(
                u8::try_from(adjusted)
                    .map_err(|_| "bracket target allocation overflowed".to_string())?,
            );
        }
        Ok(OwnedLegs {
            legs,
            orders,
            next_sequence: next,
        })
    }

    /// One held leg of a pending group: accepted but never executable until the entry fill
    /// activates it. A market entry's legs stay unpriced until that fill prices them.
    fn prepare_inactive_leg(
        &self,
        entry: &Order,
        leg: HeldLeg,
        sequence: u64,
    ) -> Result<(Order, OrderEvent), String> {
        let HeldLeg {
            client_order_id,
            order_type,
            price,
            quantity,
            detail,
        } = leg;
        let order = Order {
            id: aeris_trading::OrderId::try_new(format!("sim-order-{sequence}"))
                .map_err(|error| error.to_string())?,
            client_order_id,
            account_id: entry.account_id.clone(),
            instrument_id: entry.instrument_id.clone(),
            side: opposite_side(entry.side),
            order_type,
            time_in_force: TimeInForce::GoodTillCancelled,
            quantity,
            filled_quantity: FixedPoint::try_new(0, quantity.scale())
                .map_err(|error| error.to_string())?,
            limit_price: price.filter(|_| order_type == OrderType::Limit),
            stop_price: price.filter(|_| order_type == OrderType::Stop),
            status: OrderStatus::Inactive,
            submitted_unix_nanos: entry.submitted_unix_nanos,
            revision: 0,
            accepted_unix_nanos: 0,
            updated_unix_nanos: 0,
            eligible_after_unix_nanos: 0,
            triggered_unix_nanos: None,
            expires_at_unix_nanos: None,
            terminal_unix_nanos: None,
            provenance: entry.provenance.clone(),
        };
        let order =
            lifecycle::accept(order, self.command_now).map_err(|error| error.to_string())?;
        order.validate().map_err(|error| error.to_string())?;
        let event = OrderEvent {
            id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                .map_err(|error| error.to_string())?,
            order_id: order.id.clone(),
            sequence,
            kind: OrderEventKind::Accepted,
            reason: Some(OrderEventReason::LocalManaged),
            event_unix_nanos: entry.submitted_unix_nanos,
            detail: Some(detail),
            provenance: entry.provenance.clone(),
        };
        event.validate().map_err(|error| error.to_string())?;
        Ok((order, event))
    }

    /// The protective role a leg of an active group gives its order. Pending legs are held, not
    /// protection, so coverage alignment never touches them.
    pub(crate) fn owned_leg_role(
        &self,
        client_order_id: &aeris_trading::ClientOrderId,
    ) -> Option<ProtectiveOrderRole> {
        let order = self.state.orders.by_client(client_order_id)?;
        self.state
            .order_groups
            .values()
            .filter(|group| group.kind == GroupKind::Bracket && group.status == GroupStatus::Active)
            .find_map(|group| group.legs.iter().find(|leg| leg.order_id == order.id))
            .map(|leg| match leg.role {
                LegRole::StopLoss => ProtectiveOrderRole::StopLoss,
                LegRole::TakeProfit { .. } => ProtectiveOrderRole::TakeProfit,
            })
    }

    /// Group reactions to fills inside the quote's speculative state: a pending bracket's
    /// entry fill activates its legs, an active bracket's leg fill settles its siblings, and
    /// an OCO member's full fill cancels its siblings before any later candidate fills.
    pub(crate) fn reconcile_order_groups(
        &mut self,
        fills: &[Fill],
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<(), String> {
        for fill in fills {
            let Some(order) = self.state.orders.get(&fill.order_id).cloned() else {
                continue;
            };
            let action = self.state.order_groups.values().find_map(|group| {
                if group.kind == GroupKind::Oco {
                    // An explicit OCO member's full fill cancels its siblings before any
                    // later fill candidate of this quote is evaluated; a partial fill
                    // leaves the pair working.
                    return (group.status == GroupStatus::Active
                        && order.status == OrderStatus::Filled
                        && group.legs.iter().any(|leg| leg.order_id == order.id))
                    .then(|| (group.group_id.clone(), OwnedFill::SettleOco));
                }
                if group.kind == GroupKind::Protection {
                    // A migrated standalone protection retires when its order fills.
                    return (group.status == GroupStatus::Active
                        && order.status == OrderStatus::Filled
                        && group.legs.iter().any(|leg| leg.order_id == order.id))
                    .then(|| (group.group_id.clone(), OwnedFill::RetireProtection));
                }
                if group.kind != GroupKind::Bracket {
                    return None;
                }
                if group.status == GroupStatus::Pending && group.entry.as_ref() == Some(&order.id) {
                    Some((group.group_id.clone(), OwnedFill::ActivateBracket))
                } else if group.status == GroupStatus::Active
                    && group.legs.iter().any(|leg| leg.order_id == order.id)
                {
                    Some((group.group_id.clone(), OwnedFill::SettleBracket))
                } else {
                    None
                }
            });
            match action {
                Some((group_id, OwnedFill::ActivateBracket)) => {
                    self.activate_owned_bracket(&group_id, fill, observation, instrument)?;
                }
                Some((group_id, OwnedFill::SettleBracket)) => {
                    self.settle_owned_leg_fill(&group_id, &order)?;
                }
                Some((group_id, OwnedFill::SettleOco)) => {
                    self.settle_owned_oco_fill(&group_id, &order)?;
                }
                Some((group_id, OwnedFill::RetireProtection)) => {
                    self.retire_filled_protection(&group_id)?;
                }
                None => {}
            }
        }
        Ok(())
    }

    /// Activates a pending bracket whose entry filled, repricing and resizing its held legs
    /// from the fill price in the same quote step. A bracket that cannot be armed is cancelled
    /// with the reason on its filled entry, never retried on every quote.
    fn activate_owned_bracket(
        &mut self,
        group_id: &str,
        entry_fill: &Fill,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<(), String> {
        let group = self
            .state
            .order_groups
            .get(group_id)
            .cloned()
            .ok_or_else(|| "order group disappeared".to_string())?;
        let entry_order = self
            .state
            .orders
            .get(&entry_fill.order_id)
            .cloned()
            .ok_or_else(|| "order group entry order disappeared".to_string())?;
        match self.prepare_group_activation(&group, &entry_order, entry_fill, instrument) {
            Ok((group, legs)) => {
                for activation in legs {
                    self.state.order_events.push_back(activation.event);
                    self.state
                        .orders
                        .insert(activation.order.id.clone(), activation.order);
                }
                self.state.order_groups.insert(group_id.to_string(), group);
                self.bump_revision()
            }
            Err(reason) => self.reject_owned_bracket(group, &entry_order, observation, &reason),
        }
    }

    /// The group and leg transitions for an entry fill: legs repriced from the fill price at
    /// their kept offsets, sized to the opened exposure and activated. Zero opened exposure
    /// completes the group and cancels its legs; any leg that cannot be priced fails the whole
    /// activation.
    fn prepare_group_activation(
        &mut self,
        group: &OrderGroup,
        entry_order: &Order,
        entry_fill: &Fill,
        instrument: &TradingInstrument,
    ) -> Result<(OrderGroup, Vec<LegActivation>), String> {
        let opened = self.opened_exposure(entry_order, entry_fill)?;
        let mut updated_group = group.clone();
        updated_group.revision = group
            .revision
            .checked_add(1)
            .ok_or_else(|| "order group revision overflowed".to_string())?;
        if opened.units() == 0 {
            updated_group.status = GroupStatus::Completed;
            for leg in &group.legs {
                let order = self
                    .state
                    .orders
                    .get(&leg.order_id)
                    .cloned()
                    .ok_or_else(|| "order group leg disappeared".to_string())?;
                if order.status.is_open() {
                    self.cancel_owned_leg(&order, "cancelled with its bracket".to_string())?;
                }
            }
            return Ok((updated_group, Vec::new()));
        }
        updated_group.status = GroupStatus::Active;
        updated_group.management.entry_price = Some(entry_fill.price);
        let tick_units = instrument_tick_units(instrument, entry_fill.price)?;
        let targets = group
            .legs
            .iter()
            .filter(|leg| matches!(leg.role, LegRole::TakeProfit { .. }))
            .map(|leg| BracketTarget {
                offset_ticks: leg.offset_ticks.unwrap_or(1),
                quantity_percent: leg.allocation_percent.unwrap_or(100),
            })
            .collect::<Vec<_>>();
        let target_quantities =
            allocated_target_quantities(opened, &targets, order_increment_units(instrument))?;
        let mut activations = Vec::with_capacity(group.legs.len());
        let mut rounded_out = Vec::new();
        for leg in &group.legs {
            let order = self
                .state
                .orders
                .get(&leg.order_id)
                .cloned()
                .ok_or_else(|| "order group leg disappeared".to_string())?;
            let offset = leg
                .offset_ticks
                .ok_or_else(|| "order group leg offset is missing".to_string())?;
            let (direction, quantity) = match leg.role {
                LegRole::StopLoss => (-entry_order.side.sign(), Some(opened)),
                LegRole::TakeProfit { index } => (
                    entry_order.side.sign(),
                    target_quantities
                        .iter()
                        .find(|(target, _)| *target == usize::from(index))
                        .map(|(_, quantity)| *quantity),
                ),
            };
            // A partial entry fill can round a small target out of the allocation; that leg
            // leaves with the same cascade a completed bracket uses, like the legacy writer
            // arming only the targets the allocator kept.
            let Some(quantity) = quantity else {
                rounded_out.push(order);
                continue;
            };
            let price = shifted_price(entry_fill.price, offset, direction, tick_units)?;
            let repriced = Order {
                limit_price: (order.order_type == OrderType::Limit).then_some(price),
                stop_price: (order.order_type == OrderType::Stop).then_some(price),
                quantity,
                ..order
            };
            let activated = lifecycle::activate(repriced, self.command_now)
                .map_err(|error| error.to_string())?;
            activated.validate().map_err(|error| error.to_string())?;
            let sequence = self.state.next_sequence;
            let event = OrderEvent {
                id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                    .map_err(|error| error.to_string())?,
                order_id: activated.id.clone(),
                sequence,
                kind: OrderEventKind::Activated,
                reason: Some(OrderEventReason::LocalManaged),
                event_unix_nanos: entry_fill.execution_unix_nanos,
                detail: Some("activated at the entry fill".to_string()),
                provenance: activated.provenance.clone(),
            };
            event.validate().map_err(|error| error.to_string())?;
            self.state.next_sequence = sequence + 1;
            activations.push(LegActivation {
                order: activated,
                event,
            });
        }
        for order in rounded_out {
            if order.status.is_open() {
                self.cancel_owned_leg(&order, "rounded out of the bracket allocation".to_string())?;
            }
        }
        Ok((updated_group, activations))
    }

    /// Cancels a group that could not be activated and records why on its filled entry, so the
    /// trader sees that the position the entry opened is unprotected.
    fn reject_owned_bracket(
        &mut self,
        mut group: OrderGroup,
        entry_order: &Order,
        observation: &SimulatedMarketObservation,
        reason: &str,
    ) -> Result<(), String> {
        group.status = GroupStatus::Cancelled;
        group.revision = group
            .revision
            .checked_add(1)
            .ok_or_else(|| "order group revision overflowed".to_string())?;
        let legs = group.legs.clone();
        self.state
            .order_groups
            .insert(group.group_id.clone(), group);
        for leg in &legs {
            let Some(order) = self.state.orders.get(&leg.order_id).cloned() else {
                continue;
            };
            if order.status.is_open() {
                self.cancel_owned_leg(&order, "cancelled with its bracket".to_string())?;
            }
        }
        self.push_owned_event(
            entry_order,
            OrderEventKind::Rejected,
            Some(OrderEventReason::ProtectionNotPlaced),
            observation.provenance.observed_unix_nanos,
            format!(
                "{} bracket protection was not placed: {reason}",
                ManagedBracket::MANAGEMENT_LABEL
            ),
            observation.provenance.clone(),
        )?;
        self.bump_revision()
    }

    /// Settles an active bracket after one of its legs filled: the stop takes every target with
    /// it; a target keeps the bracket working while targets and a position remain, and the last
    /// exit cancels the stop.
    fn settle_owned_leg_fill(&mut self, group_id: &str, filled: &Order) -> Result<(), String> {
        let mut group = self
            .state
            .order_groups
            .get(group_id)
            .cloned()
            .ok_or_else(|| "order group disappeared".to_string())?;
        let stop_is_filled = group
            .legs
            .first()
            .is_some_and(|leg| matches!(leg.role, LegRole::StopLoss) && leg.order_id == filled.id);
        if stop_is_filled {
            for leg in &group.legs {
                if leg.order_id == filled.id {
                    continue;
                }
                let Some(order) = self.state.orders.get(&leg.order_id).cloned() else {
                    continue;
                };
                if order.status.is_open() {
                    self.cancel_owned_leg(&order, "cancelled with its bracket".to_string())?;
                }
            }
            group.status = GroupStatus::Completed;
        } else {
            let targets_open = group.legs.iter().any(|leg| {
                matches!(leg.role, LegRole::TakeProfit { .. })
                    && self
                        .state
                        .orders
                        .get(&leg.order_id)
                        .is_some_and(|order| order.status.is_open())
            });
            let position_open = self
                .state
                .positions
                .get(&(group.account_id.clone(), group.instrument_id.clone()))
                .is_some_and(|position| position.net_quantity.units() != 0);
            // While targets remain, the stop keeps working and coverage alignment resizes it
            // to the smaller position.
            if targets_open && position_open {
                return Ok(());
            }
            if let Some(stop) = group.legs.first()
                && let Some(order) = self.state.orders.get(&stop.order_id).cloned()
                && order.status.is_open()
            {
                self.cancel_owned_leg(&order, "cancelled with its bracket".to_string())?;
            }
            group.status = GroupStatus::Completed;
        }
        group.revision = group
            .revision
            .checked_add(1)
            .ok_or_else(|| "order group revision overflowed".to_string())?;
        self.state.order_groups.insert(group_id.to_string(), group);
        self.bump_revision()
    }

    /// Retires a standalone protection group whose order filled inside the quote's
    /// speculative state.
    fn retire_filled_protection(&mut self, group_id: &str) -> Result<(), String> {
        let mut group = self
            .state
            .order_groups
            .get(group_id)
            .cloned()
            .ok_or_else(|| "order group disappeared".to_string())?;
        group.status = GroupStatus::Completed;
        group.revision = group
            .revision
            .checked_add(1)
            .ok_or_else(|| "order group revision overflowed".to_string())?;
        self.state.order_groups.insert(group_id.to_string(), group);
        self.bump_revision()
    }

    /// Pushes an order event minted at the next sequence inside the quote's speculative
    /// state, stamping it from the owning order.
    fn push_owned_event(
        &mut self,
        order: &Order,
        kind: OrderEventKind,
        reason: Option<OrderEventReason>,
        event_unix_nanos: i64,
        detail: String,
        provenance: aeris_trading::TradingProvenance,
    ) -> Result<(), String> {
        let sequence = self.state.next_sequence;
        let event = OrderEvent {
            id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                .map_err(|error| error.to_string())?,
            order_id: order.id.clone(),
            sequence,
            kind,
            reason,
            event_unix_nanos,
            detail: Some(detail),
            provenance,
        };
        event.validate().map_err(|error| error.to_string())?;
        self.state.next_sequence = sequence + 1;
        self.state.order_events.push_back(event);
        Ok(())
    }

    /// Cancels a held or working group leg inside the quote's speculative state.
    pub(super) fn cancel_owned_leg(&mut self, order: &Order, detail: String) -> Result<(), String> {
        let (cancelled, reason) = lifecycle::cancel_with_cause(
            order.clone(),
            CancellationCause::Cascade,
            self.command_now,
        )
        .map_err(|error| error.to_string())?;
        self.push_owned_event(
            &cancelled,
            OrderEventKind::Cancelled,
            Some(reason),
            self.command_now,
            detail,
            cancelled.provenance.clone(),
        )?;
        self.state.orders.insert(cancelled.id.clone(), cancelled);
        Ok(())
    }

    /// The stop price a group's management rules demand at this quote, or `None` when neither
    /// the break-even nor the trailing rule tightens the stop by at least one tick.
    fn managed_stop_price(
        group: &OrderGroup,
        entry: &Order,
        stop: &Order,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<Option<FixedPoint>, String> {
        let entry_price = group
            .management
            .entry_price
            .ok_or_else(|| "active order group entry price is missing".to_string())?;
        let tick_units = instrument_tick_units(instrument, entry_price)?;
        let favorable_price = match entry.side {
            OrderSide::Buy => observation.bid,
            OrderSide::Sell => observation.ask,
        };
        let favorable_ticks =
            favorable_tick_distance(entry_price, favorable_price, entry.side, tick_units)?;
        let mut next_stop = stop
            .stop_price
            .ok_or_else(|| "order group stop price is missing".to_string())?;
        if let Some(rule) = group.management.break_even
            && favorable_ticks >= i64::from(rule.activation_ticks)
        {
            let price = shifted_price(
                entry_price,
                rule.offset_ticks.unsigned_abs(),
                entry.side.sign() * i64::from(rule.offset_ticks.signum()),
                tick_units,
            )?;
            next_stop = tighter_stop(next_stop, price, entry.side);
        }
        if let Some(rule) = group.management.trailing
            && favorable_ticks >= i64::from(rule.activation_ticks)
        {
            let price = shifted_price(
                favorable_price,
                rule.distance_ticks,
                -entry.side.sign(),
                tick_units,
            )?;
            next_stop = tighter_stop(next_stop, price, entry.side);
        }
        Ok((Some(next_stop) != stop.stop_price).then_some(next_stop))
    }

    /// Moves an active group bracket's stop with its trailing and break-even rules, writing a
    /// `Modified{Management}` event only when the stop moves at least one tick.
    pub(crate) fn update_owned_stops(
        &mut self,
        observation: &SimulatedMarketObservation,
        instrument: &TradingInstrument,
    ) -> Result<(), String> {
        let active = self
            .state
            .order_groups
            .values()
            .filter(|group| {
                group.kind == GroupKind::Bracket
                    && group.status == GroupStatus::Active
                    && group.instrument_id == observation.instrument_id
                    && (group.management.trailing.is_some()
                        || group.management.break_even.is_some())
            })
            .cloned()
            .collect::<Vec<_>>();
        for group in active {
            let Some(entry_id) = group.entry.clone() else {
                continue;
            };
            let entry = self
                .state
                .orders
                .get(&entry_id)
                .cloned()
                .ok_or_else(|| "order group entry order disappeared".to_string())?;
            let Some(stop_leg) = group
                .legs
                .iter()
                .find(|leg| matches!(leg.role, LegRole::StopLoss))
            else {
                continue;
            };
            let stop = self
                .state
                .orders
                .get(&stop_leg.order_id)
                .cloned()
                .ok_or_else(|| "active order group stop is missing".to_string())?;
            if !stop.status.is_open() {
                continue;
            }
            let Some(next_stop) =
                Self::managed_stop_price(&group, &entry, &stop, observation, instrument)?
            else {
                continue;
            };
            let repriced = Order {
                stop_price: Some(next_stop),
                ..stop.clone()
            };
            let (repriced, reason) = lifecycle::modify_with_cause(
                repriced,
                ModificationCause::Management,
                self.command_now,
            )
            .map_err(|error| error.to_string())?;
            repriced.validate().map_err(|error| error.to_string())?;
            self.push_owned_event(
                &repriced,
                OrderEventKind::Modified,
                Some(reason),
                observation.provenance.observed_unix_nanos,
                format!("{} stop management", ManagedBracket::MANAGEMENT_LABEL),
                observation.provenance.clone(),
            )?;
            self.state.orders.insert(repriced.id.clone(), repriced);
            self.bump_revision()?;
        }
        Ok(())
    }

    /// Moves a held leg of a pending bracket, keeping the move as the leg's new tick offset
    /// from the entry. Returns `None` when the order is not a group leg, so the caller falls
    /// through to the legacy planned-leg writer for pre-switch brackets.
    pub(super) fn move_owned_leg(
        &mut self,
        command: &MovePlannedBracketLeg,
    ) -> Result<Option<PlannedBracketLeg>, String> {
        let Some(order) = self
            .state
            .orders
            .by_client(&command.client_order_id)
            .cloned()
        else {
            return Ok(None);
        };
        let Some((group, leg_index)) = self.state.order_groups.values().find_map(|group| {
            (group.kind == GroupKind::Bracket && group.status == GroupStatus::Pending)
                .then(|| {
                    group
                        .legs
                        .iter()
                        .position(|leg| leg.order_id == order.id)
                        .map(|index| (group.clone(), index))
                })
                .flatten()
        }) else {
            return Ok(None);
        };
        let entry = self
            .state
            .orders
            .get(group.entry.as_ref().ok_or("order group entry is missing")?)
            .cloned()
            .ok_or_else(|| "order group entry order disappeared".to_string())?;
        if !entry.status.is_open() {
            return Err("the bracket's entry is no longer working".to_string());
        }
        let instrument = self
            .state
            .instruments
            .get(&entry.instrument_id)
            .cloned()
            .ok_or_else(|| "trading instrument is not registered".to_string())?;
        let role = group.legs[leg_index].role;
        let ticks =
            self.checked_owned_leg_move(&group, leg_index, &entry, &instrument, command.price)?;
        let moved = Order {
            limit_price: (order.order_type == OrderType::Limit).then_some(command.price),
            stop_price: (order.order_type == OrderType::Stop).then_some(command.price),
            ..order.clone()
        };
        let (moved, reason) =
            lifecycle::reprice_held(moved, ModificationCause::LegMoved, self.command_now)
                .map_err(|error| error.to_string())?;
        moved.validate().map_err(|error| error.to_string())?;
        let mut group = group;
        group.legs[leg_index].offset_ticks = Some(ticks);
        group.revision = group
            .revision
            .checked_add(1)
            .ok_or_else(|| "order group revision overflowed".to_string())?;
        group
            .validate()
            .map_err(|error| format!("invalid order group: {error:?}"))?;
        let sequence = self.state.next_sequence;
        let event = OrderEvent {
            id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                .map_err(|error| error.to_string())?,
            order_id: moved.id.clone(),
            sequence,
            kind: OrderEventKind::Modified,
            reason: Some(reason),
            event_unix_nanos: self.command_now,
            detail: Some("moved by local command".to_string()),
            provenance: moved.provenance.clone(),
        };
        event.validate().map_err(|error| error.to_string())?;
        let mut step = OrderStep::new(sequence + 1);
        step.updated.push((moved.clone(), event));
        step.groups.push(group.clone());
        self.commit_order_step(step, Vec::new())?;
        Ok(Some(PlannedBracketLeg {
            bracket_id: group.group_id.clone(),
            entry_client_order_id: entry.client_order_id.clone(),
            account_id: entry.account_id.clone(),
            instrument_id: entry.instrument_id.clone(),
            client_order_id: moved.client_order_id.clone(),
            role: match role {
                LegRole::StopLoss => ProtectiveOrderRole::StopLoss,
                LegRole::TakeProfit { .. } => ProtectiveOrderRole::TakeProfit,
            },
            side: moved.side,
            order_type: moved.order_type,
            price: command.price,
            quantity: moved.quantity,
        }))
    }

    /// The tick offset a pending-leg move requests, checked against the entry's resting
    /// price, the instrument grid and the role's side of the entry; a stop move re-runs the
    /// bracket stop-risk limit with the new distance.
    fn checked_owned_leg_move(
        &self,
        group: &OrderGroup,
        leg_index: usize,
        entry: &Order,
        instrument: &TradingInstrument,
        price: FixedPoint,
    ) -> Result<u32, String> {
        let base = entry
            .limit_price
            .or(entry.stop_price)
            .ok_or_else(|| "the bracket's entry has no resting price".to_string())?;
        if price.scale() != base.scale() {
            return Err("the leg price scale does not match the instrument".to_string());
        }
        let role = group.legs[leg_index].role;
        let favorable_units = price
            .units()
            .checked_sub(base.units())
            .and_then(|distance| distance.checked_mul(entry.side.sign()))
            .ok_or_else(|| "bracket leg distance overflowed".to_string())?;
        let distance = match role {
            LegRole::StopLoss => -favorable_units,
            LegRole::TakeProfit { .. } => favorable_units,
        };
        if distance <= 0 {
            return Err(match role {
                LegRole::StopLoss => "a bracket stop must stay on the losing side of its entry",
                LegRole::TakeProfit { .. } => {
                    "a bracket target must stay on the winning side of its entry"
                }
            }
            .to_string());
        }
        let tick_units = instrument_tick_units(instrument, base)?;
        if distance % tick_units != 0 {
            return Err("bracket prices must sit on whole ticks".to_string());
        }
        let ticks = u32::try_from(distance / tick_units)
            .map_err(|_| "bracket leg distance overflowed".to_string())?;
        if role == LegRole::StopLoss {
            let mut template = self.group_template_view(group);
            template.stop_offset_ticks = ticks;
            self.check_bracket_stop_risk(
                &entry.account_id,
                &entry.instrument_id,
                entry.quantity,
                &template,
            )?;
        }
        Ok(ticks)
    }

    /// Stages the held legs' repricing when a pending bracket's entry is modified: every leg
    /// keeps its tick offset and follows the entry's new resting price in the same step.
    pub(crate) fn stage_owned_leg_reprices(
        &self,
        step: &mut OrderStep,
        entry: &Order,
    ) -> Result<(), String> {
        let Some(group) = self
            .state
            .order_groups
            .values()
            .find(|group| {
                group.kind == GroupKind::Bracket
                    && group.status == GroupStatus::Pending
                    && group.entry.as_ref() == Some(&entry.id)
            })
            .cloned()
        else {
            return Ok(());
        };
        let Some(base) = entry.limit_price.or(entry.stop_price) else {
            return Ok(());
        };
        let instrument = self
            .state
            .instruments
            .get(&entry.instrument_id)
            .ok_or_else(|| "trading instrument is not registered".to_string())?;
        let tick_units = instrument_tick_units(instrument, base)?;
        for leg in &group.legs {
            let order = self
                .state
                .orders
                .get(&leg.order_id)
                .cloned()
                .ok_or_else(|| "order group leg disappeared".to_string())?;
            if order.status != OrderStatus::Inactive {
                continue;
            }
            let offset = leg
                .offset_ticks
                .ok_or_else(|| "order group leg offset is missing".to_string())?;
            let direction = match leg.role {
                LegRole::StopLoss => -entry.side.sign(),
                LegRole::TakeProfit { .. } => entry.side.sign(),
            };
            let price = shifted_price(base, offset, direction, tick_units)?;
            let repriced = Order {
                limit_price: (order.order_type == OrderType::Limit).then_some(price),
                stop_price: (order.order_type == OrderType::Stop).then_some(price),
                ..order
            };
            let (repriced, reason) = lifecycle::reprice_held(
                repriced,
                ModificationCause::ParentRepriced,
                self.command_now,
            )
            .map_err(|error| error.to_string())?;
            repriced.validate().map_err(|error| error.to_string())?;
            let sequence = step.next_sequence;
            let event = OrderEvent {
                id: OrderEventId::try_new(format!("sim-event-{sequence}"))
                    .map_err(|error| error.to_string())?,
                order_id: repriced.id.clone(),
                sequence,
                kind: OrderEventKind::Modified,
                reason: Some(reason),
                event_unix_nanos: self.command_now,
                detail: Some("repriced with the bracket entry".to_string()),
                provenance: repriced.provenance.clone(),
            };
            event.validate().map_err(|error| error.to_string())?;
            step.next_sequence += 1;
            step.updated.push((repriced, event));
        }
        Ok(())
    }

    /// The held legs of every pending group-owned bracket, projected as planned legs for the
    /// legacy reader view until M6.11 replaces it. A leg keeps its real order's identity, so a
    /// chart move on the line reaches `move_planned_bracket_leg` and the armed order keeps the
    /// line. Legs without a price yet, such as a market entry's, stay hidden until activation,
    /// exactly like a legacy bracket whose legs cannot be priced.
    pub(crate) fn owned_planned_bracket_legs(&self) -> Vec<PlannedBracketLeg> {
        let mut planned = Vec::new();
        for group in self.state.order_groups.values() {
            if group.kind != GroupKind::Bracket || group.status != GroupStatus::Pending {
                continue;
            }
            let entry = group
                .entry
                .as_ref()
                .and_then(|entry| self.state.orders.get(entry))
                .filter(|entry| entry.status.is_open());
            let Some(entry) = entry else {
                continue;
            };
            for leg in &group.legs {
                let Some(order) = self
                    .state
                    .orders
                    .get(&leg.order_id)
                    .filter(|order| order.status == OrderStatus::Inactive)
                else {
                    continue;
                };
                let Some(price) = order.limit_price.or(order.stop_price) else {
                    continue;
                };
                planned.push(PlannedBracketLeg {
                    bracket_id: group.group_id.clone(),
                    entry_client_order_id: entry.client_order_id.clone(),
                    account_id: order.account_id.clone(),
                    instrument_id: order.instrument_id.clone(),
                    client_order_id: order.client_order_id.clone(),
                    role: match leg.role {
                        LegRole::StopLoss => ProtectiveOrderRole::StopLoss,
                        LegRole::TakeProfit { .. } => ProtectiveOrderRole::TakeProfit,
                    },
                    side: order.side,
                    order_type: order.order_type,
                    price,
                    quantity: order.quantity,
                });
            }
        }
        planned
    }

    /// The legacy reader view of a group-owned bracket, served until M6.11 replaces the
    /// snapshot: a pending group projects as an awaiting bracket without child state, an
    /// activated one carries its real child ids and entry price.
    pub(crate) fn managed_bracket_view(
        &self,
        group: &OrderGroup,
    ) -> Result<ManagedBracket, String> {
        let entry_order = self
            .state
            .orders
            .get(
                group
                    .entry
                    .as_ref()
                    .ok_or_else(|| "order group entry is missing".to_string())?,
            )
            .ok_or_else(|| "order group entry order disappeared".to_string())?;
        let activated = group.management.entry_price.is_some();
        let (stop_client_order_id, target_client_order_ids) = if activated {
            let mut stop = None;
            let mut targets = Vec::new();
            for leg in &group.legs {
                let order = self
                    .state
                    .orders
                    .get(&leg.order_id)
                    .ok_or_else(|| "order group leg order disappeared".to_string())?;
                match leg.role {
                    LegRole::StopLoss => stop = Some(order.client_order_id.clone()),
                    LegRole::TakeProfit { .. } => targets.push(order.client_order_id.clone()),
                }
            }
            (stop, targets)
        } else {
            (None, Vec::new())
        };
        let bracket = ManagedBracket {
            bracket_id: group.group_id.clone(),
            template: self.group_template_view(group),
            entry_client_order_id: entry_order.client_order_id.clone(),
            stop_client_order_id,
            target_client_order_ids,
            status: match group.status {
                GroupStatus::Pending => ManagedBracketStatus::AwaitingEntry,
                GroupStatus::Active => ManagedBracketStatus::Active,
                GroupStatus::Completed => ManagedBracketStatus::Completed,
                GroupStatus::Cancelled => ManagedBracketStatus::Cancelled,
            },
            entry_price: group.management.entry_price,
        };
        bracket.validate()?;
        Ok(bracket)
    }

    /// Rebuilds the template view of a group from its stored leg offsets and management rules.
    fn group_template_view(&self, group: &OrderGroup) -> BracketStrategyTemplate {
        let (template_id, revision) = group
            .template
            .clone()
            .unwrap_or_else(|| ("managed-bracket".to_string(), 1));
        let name = self.state.strategy_templates.get(&template_id).map_or_else(
            || "Managed bracket".to_string(),
            |template| template.name.clone(),
        );
        let stop_offset_ticks = group
            .legs
            .iter()
            .find(|leg| matches!(leg.role, LegRole::StopLoss))
            .and_then(|leg| leg.offset_ticks)
            .unwrap_or(1);
        let targets = group
            .legs
            .iter()
            .filter(|leg| matches!(leg.role, LegRole::TakeProfit { .. }))
            .map(|leg| BracketTarget {
                offset_ticks: leg.offset_ticks.unwrap_or(1),
                quantity_percent: leg.allocation_percent.unwrap_or(100),
            })
            .collect();
        BracketStrategyTemplate {
            template_id,
            revision,
            name,
            stop_offset_ticks,
            targets,
            trailing_stop: group.management.trailing,
            break_even: group.management.break_even,
            enabled: true,
        }
    }
}
