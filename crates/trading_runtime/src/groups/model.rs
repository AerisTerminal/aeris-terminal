use crate::strategy::{BreakEvenRule, MAXIMUM_BRACKET_TARGETS, TrailingStopRule};
use aeris_instruments::InstrumentId;
use aeris_trading::{OrderId, TradingAccountId};
use std::collections::BTreeSet;

/// A group belongs to one account and instrument; its members remain real orders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderGroup {
    pub group_id: String,
    pub account_id: TradingAccountId,
    pub instrument_id: InstrumentId,
    pub kind: GroupKind,
    pub status: GroupStatus,
    pub entry: Option<OrderId>,
    /// Stop first, then targets in trigger order. No group owns more than five legs.
    pub legs: Vec<GroupLeg>,
    pub management: StopManagement,
    pub template: Option<(String, u32)>,
    pub revision: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GroupKind {
    Bracket,
    Oco,
    Protection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GroupStatus {
    Pending,
    Active,
    Completed,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupLeg {
    pub order_id: OrderId,
    pub role: LegRole,
    pub offset_ticks: Option<u32>,
    pub allocation_percent: Option<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegRole {
    StopLoss,
    TakeProfit { index: u8 },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StopManagement {
    pub trailing: Option<TrailingStopRule>,
    pub break_even: Option<BreakEvenRule>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GroupValidationError {
    InvalidIdentity,
    InvalidMembership,
    InvalidBracketLegs,
}

impl OrderGroup {
    /// Validate the bounded, structural invariants before storing a new revision.
    /// Exposure and order-status checks belong to the trading owner at transition time.
    ///
    /// # Errors
    /// Returns a typed error for missing identity, duplicate or excess members, or malformed bracket roles.
    pub fn validate(&self) -> Result<(), GroupValidationError> {
        if self.group_id.is_empty() || self.revision == 0 {
            return Err(GroupValidationError::InvalidIdentity);
        }
        // Historical brackets that never acquired priceable children retain their parent
        // and terminal group fact rather than fabricating orders during migration.
        if self.legs.is_empty()
            && self.kind == GroupKind::Bracket
            && matches!(self.status, GroupStatus::Completed | GroupStatus::Cancelled)
            && self.entry.is_some()
        {
            return Ok(());
        }
        if self.legs.is_empty() || self.legs.len() > 1 + MAXIMUM_BRACKET_TARGETS {
            return Err(GroupValidationError::InvalidMembership);
        }
        let mut members = BTreeSet::new();
        if let Some(entry) = &self.entry {
            members.insert(entry.clone());
        }
        if self
            .legs
            .iter()
            .any(|leg| !members.insert(leg.order_id.clone()))
        {
            return Err(GroupValidationError::InvalidMembership);
        }
        if self.kind != GroupKind::Bracket {
            if self.entry.is_some()
                || self.template.is_some()
                || self.management != StopManagement::default()
                || self
                    .legs
                    .iter()
                    .any(|leg| leg.offset_ticks.is_some() || leg.allocation_percent.is_some())
            {
                return Err(GroupValidationError::InvalidMembership);
            }
            return Ok(());
        }
        let Some((stop, targets)) = self.legs.split_first() else {
            return Err(GroupValidationError::InvalidBracketLegs);
        };
        if self.entry.is_none()
            || targets.is_empty()
            || !matches!(stop.role, LegRole::StopLoss)
            || stop.offset_ticks.is_none_or(|ticks| ticks == 0)
            || stop.allocation_percent.is_some()
            || targets.iter().enumerate().any(|(index, leg)| {
                let Ok(index) = u8::try_from(index) else {
                    return true;
                };
                leg.role != (LegRole::TakeProfit { index })
                    || leg.offset_ticks.is_none_or(|ticks| ticks == 0)
                    || leg.allocation_percent.is_none_or(|percent| percent == 0)
            })
            || targets
                .iter()
                .map(|leg| u32::from(leg.allocation_percent.unwrap_or(0)))
                .sum::<u32>()
                != 100
        {
            return Err(GroupValidationError::InvalidBracketLegs);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> OrderId {
        OrderId::try_new(value.to_string()).unwrap()
    }

    fn bracket() -> OrderGroup {
        OrderGroup {
            group_id: "entry-client".into(),
            account_id: TradingAccountId::try_new("practice").unwrap(),
            instrument_id: InstrumentId::try_new("BTC-USDC").unwrap(),
            kind: GroupKind::Bracket,
            status: GroupStatus::Pending,
            entry: Some(id("entry")),
            legs: vec![
                GroupLeg {
                    order_id: id("stop"),
                    role: LegRole::StopLoss,
                    offset_ticks: Some(8),
                    allocation_percent: None,
                },
                GroupLeg {
                    order_id: id("target-1"),
                    role: LegRole::TakeProfit { index: 0 },
                    offset_ticks: Some(8),
                    allocation_percent: Some(50),
                },
                GroupLeg {
                    order_id: id("target-2"),
                    role: LegRole::TakeProfit { index: 1 },
                    offset_ticks: Some(16),
                    allocation_percent: Some(50),
                },
            ],
            management: StopManagement::default(),
            template: Some(("managed-scale-out".into(), 1)),
            revision: 1,
        }
    }

    #[test]
    fn bracket_model_bounds_membership_and_preserves_target_allocation() {
        let mut group = bracket();
        assert_eq!(group.validate(), Ok(()));
        group.legs[2].allocation_percent = Some(51);
        assert_eq!(
            group.validate(),
            Err(GroupValidationError::InvalidBracketLegs)
        );
        group = bracket();
        group.legs[2].order_id = id("stop");
        assert_eq!(
            group.validate(),
            Err(GroupValidationError::InvalidMembership)
        );
        group = bracket();
        group.legs.extend(group.legs[2..].to_vec());
        group.legs.push(GroupLeg {
            order_id: id("sixth"),
            role: LegRole::TakeProfit { index: 4 },
            offset_ticks: Some(1),
            allocation_percent: Some(1),
        });
        assert_eq!(
            group.validate(),
            Err(GroupValidationError::InvalidMembership)
        );
    }

    #[test]
    fn oco_and_protection_do_not_claim_a_parent_or_bracket_offsets() {
        let mut group = bracket();
        group.kind = GroupKind::Oco;
        group.entry = None;
        group.template = None;
        group.legs.truncate(2);
        group.legs.iter_mut().for_each(|leg| {
            leg.offset_ticks = None;
            leg.allocation_percent = None;
        });
        assert_eq!(group.validate(), Ok(()));
        group.kind = GroupKind::Protection;
        assert_eq!(group.validate(), Ok(()));
        group.entry = Some(id("parent"));
        assert_eq!(
            group.validate(),
            Err(GroupValidationError::InvalidMembership)
        );
    }

    #[test]
    fn legacy_unpriceable_bracket_may_be_terminal_without_children() {
        let mut group = bracket();
        group.status = GroupStatus::Cancelled;
        group.legs.clear();
        assert_eq!(group.validate(), Ok(()));
        group.status = GroupStatus::Pending;
        assert_eq!(
            group.validate(),
            Err(GroupValidationError::InvalidMembership)
        );
    }
}
