//! The stop and targets an awaiting managed bracket places once its entry fills, priced from the
//! entry's resting price. They let a chart show and move a pending entry's protection before any
//! child order exists; the bracket template stays the only stored form of that protection.

use crate::{
    Coordinator, ManagedBracket, ManagedBracketStatus, ProtectiveOrderRole, TradingInstrument,
    allocated_target_quantities, instrument_tick_units, managed_child_client_order_id,
    opposite_side, order_increment_units, shifted_price,
};
use aeris_instruments::InstrumentId;
use aeris_trading::{ClientOrderId, FixedPoint, Order, OrderSide, OrderType, TradingAccountId};

/// One stop or target of an awaiting bracket, at the price it takes if the entry fills at its
/// resting price.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedBracketLeg {
    pub bracket_id: String,
    pub entry_client_order_id: ClientOrderId,
    pub account_id: TradingAccountId,
    pub instrument_id: InstrumentId,
    /// The identity the leg's order takes once the entry fills.
    pub client_order_id: ClientOrderId,
    pub role: ProtectiveOrderRole,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub price: FixedPoint,
    pub quantity: FixedPoint,
}

/// Moves one planned leg of an awaiting bracket; its new price becomes the leg's tick distance
/// from the entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MovePlannedBracketLeg {
    pub client_order_id: ClientOrderId,
    pub price: FixedPoint,
}

/// Where a planned leg sits in its bracket's template.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LegSlot {
    Stop,
    Target(usize),
}

/// The legs `bracket` places when `entry` fills at its resting price. An entry without one, such
/// as a market order, fills at once and has none.
pub(crate) fn planned_legs(
    bracket: &ManagedBracket,
    entry: &Order,
    instrument: &TradingInstrument,
) -> Result<Vec<PlannedBracketLeg>, String> {
    Ok(slotted_legs(bracket, entry, instrument)?
        .into_iter()
        .map(|(_, leg)| leg)
        .collect())
}

fn slotted_legs(
    bracket: &ManagedBracket,
    entry: &Order,
    instrument: &TradingInstrument,
) -> Result<Vec<(LegSlot, PlannedBracketLeg)>, String> {
    let Some(base) = entry.limit_price.or(entry.stop_price) else {
        return Ok(Vec::new());
    };
    let tick_units = instrument_tick_units(instrument, base)?;
    let leg = |slot, role, order_type, price, quantity| -> Result<_, String> {
        Ok((
            slot,
            PlannedBracketLeg {
                bracket_id: bracket.bracket_id.clone(),
                entry_client_order_id: entry.client_order_id.clone(),
                account_id: entry.account_id.clone(),
                instrument_id: entry.instrument_id.clone(),
                client_order_id: leg_client_order_id(&entry.client_order_id, slot)?,
                role,
                side: opposite_side(entry.side),
                order_type,
                price,
                quantity,
            },
        ))
    };
    let mut legs = Vec::with_capacity(bracket.template.targets.len() + 1);
    legs.push(leg(
        LegSlot::Stop,
        ProtectiveOrderRole::StopLoss,
        OrderType::Stop,
        shifted_price(
            base,
            bracket.template.stop_offset_ticks,
            -entry.side.sign(),
            tick_units,
        )?,
        entry.quantity,
    )?);
    for (index, quantity) in allocated_target_quantities(
        entry.quantity,
        &bracket.template.targets,
        order_increment_units(instrument),
    )? {
        legs.push(leg(
            LegSlot::Target(index),
            ProtectiveOrderRole::TakeProfit,
            OrderType::Limit,
            shifted_price(
                base,
                bracket.template.targets[index].offset_ticks,
                entry.side.sign(),
                tick_units,
            )?,
            quantity,
        )?);
    }
    Ok(legs)
}

/// The identity the bracket arming path gives the leg's order, so a planned leg and the order it
/// becomes are one line on a chart.
fn leg_client_order_id(entry: &ClientOrderId, slot: LegSlot) -> Result<ClientOrderId, String> {
    match slot {
        LegSlot::Stop => managed_child_client_order_id(entry, "stop", 0),
        LegSlot::Target(index) => managed_child_client_order_id(
            entry,
            "target",
            u32::try_from(index).map_err(|_| "bracket target index overflowed".to_string())?,
        ),
    }
}

fn leg_slot(
    bracket: &ManagedBracket,
    client_order_id: &ClientOrderId,
) -> Result<Option<LegSlot>, String> {
    let slots = std::iter::once(LegSlot::Stop)
        .chain((0..bracket.template.targets.len()).map(LegSlot::Target));
    for slot in slots {
        if leg_client_order_id(&bracket.entry_client_order_id, slot)? == *client_order_id {
            return Ok(Some(slot));
        }
    }
    Ok(None)
}

impl Coordinator {
    /// The awaiting bracket whose entry is `client_order_id`.
    pub(crate) fn awaiting_bracket(
        &self,
        client_order_id: &ClientOrderId,
    ) -> Option<&ManagedBracket> {
        self.state.managed_brackets.values().find(|bracket| {
            bracket.status == ManagedBracketStatus::AwaitingEntry
                && bracket.entry_client_order_id == *client_order_id
        })
    }

    /// Every awaiting bracket's planned legs whose entry is still open.
    pub(crate) fn planned_bracket_legs(&self) -> Vec<PlannedBracketLeg> {
        self.state
            .managed_brackets
            .values()
            .filter(|bracket| bracket.status == ManagedBracketStatus::AwaitingEntry)
            .filter_map(|bracket| {
                let entry = self
                    .order_by_client_id(&bracket.entry_client_order_id)
                    .ok()
                    .filter(|entry| entry.status.is_open())?;
                let instrument = self.state.instruments.get(&entry.instrument_id)?;
                // Placement and entry modification refuse legs that cannot be priced, so only
                // state restored from before that check lands here. Arming rejects such a bracket
                // with its reason on the entry; until then the entry shows without legs.
                planned_legs(bracket, entry, instrument).ok()
            })
            .flatten()
            .collect()
    }

    pub(crate) fn move_planned_bracket_leg(
        &mut self,
        command: &MovePlannedBracketLeg,
    ) -> Result<PlannedBracketLeg, String> {
        let mut found = None;
        for bracket in self
            .state
            .managed_brackets
            .values()
            .filter(|bracket| bracket.status == ManagedBracketStatus::AwaitingEntry)
        {
            if let Some(slot) = leg_slot(bracket, &command.client_order_id)? {
                found = Some((bracket.clone(), slot));
                break;
            }
        }
        let (bracket, slot) = found
            .ok_or_else(|| "the order is not a planned leg of a pending bracket".to_string())?;
        let entry = self
            .order_by_client_id(&bracket.entry_client_order_id)?
            .clone();
        if !entry.status.is_open() {
            return Err("the bracket's entry is no longer working".to_string());
        }
        let instrument = self
            .state
            .instruments
            .get(&entry.instrument_id)
            .cloned()
            .ok_or_else(|| "trading instrument is not registered".to_string())?;
        let base = entry
            .limit_price
            .or(entry.stop_price)
            .ok_or_else(|| "the bracket's entry has no resting price".to_string())?;
        if command.price.scale() != base.scale() {
            return Err("the leg price scale does not match the instrument".to_string());
        }
        let favorable_units = command
            .price
            .units()
            .checked_sub(base.units())
            .and_then(|distance| distance.checked_mul(entry.side.sign()))
            .ok_or_else(|| "bracket leg distance overflowed".to_string())?;
        let distance = match slot {
            LegSlot::Stop => -favorable_units,
            LegSlot::Target(_) => favorable_units,
        };
        if distance <= 0 {
            return Err(match slot {
                LegSlot::Stop => "a bracket stop must stay on the losing side of its entry",
                LegSlot::Target(_) => "a bracket target must stay on the winning side of its entry",
            }
            .to_string());
        }
        let tick_units = instrument_tick_units(&instrument, base)?;
        if distance % tick_units != 0 {
            return Err("bracket prices must sit on whole ticks".to_string());
        }
        let ticks = u32::try_from(distance / tick_units)
            .map_err(|_| "bracket leg distance overflowed".to_string())?;
        let mut template = bracket.template.clone();
        match slot {
            LegSlot::Stop => template.stop_offset_ticks = ticks,
            LegSlot::Target(index) => {
                template
                    .targets
                    .get_mut(index)
                    .ok_or_else(|| "bracket target disappeared".to_string())?
                    .offset_ticks = ticks;
            }
        }
        template.revision = template
            .revision
            .checked_add(1)
            .ok_or_else(|| "bracket template revision overflowed".to_string())?;
        template.validate()?;
        if slot == LegSlot::Stop {
            self.check_bracket_stop_risk(
                &entry.account_id,
                &entry.instrument_id,
                entry.quantity,
                &template,
            )?;
        }
        let moved = ManagedBracket {
            template,
            ..bracket
        };
        moved.validate()?;
        let leg = slotted_legs(&moved, &entry, &instrument)?
            .into_iter()
            .find_map(|(candidate, leg)| (candidate == slot).then_some(leg))
            .ok_or_else(|| "the moved bracket leg has no allocation".to_string())?;
        self.store.put_managed_bracket(&moved)?;
        self.state
            .managed_brackets
            .insert(moved.bracket_id.clone(), moved);
        self.bump_revision()?;
        Ok(leg)
    }
}
