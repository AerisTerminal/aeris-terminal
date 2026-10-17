//! Startup verification of recovered order groups. Every pending or active group is checked
//! against the orders and positions it claims: a group that violates its structure or lost a
//! member is cancelled with its remaining open legs in one durable step, a group whose member
//! filled before the last shutdown settles now, and protection that no longer matches its
//! position is reported before loaded-state alignment realigns it. Each repair commits on its
//! own, so a failed repair rolls back and retries at the next start; a verified store reopens
//! without further changes or notices.

use super::model::{GroupKind, GroupLeg, GroupStatus, OrderGroup};
use super::{Order, OrderStatus, OrderStep, TradingCore};
use crate::book::lifecycle::CancellationCause;
use crate::{ProtectionRole, RecoveryNotice, RecoveryNoticeKind, VenueRoute};
use aeris_instruments::InstrumentId;
use aeris_trading::TradingAccountId;

/// How one recovered group failed verification.
enum GroupViolation {
    /// The group or its members no longer exist or fit their phase; the group and its open
    /// legs are cancelled.
    Broken(String),
    /// A member filled before the last shutdown without settling the group; the group
    /// completes and its remaining open legs are cancelled.
    UnsettledFill(String),
}

impl TradingCore {
    /// Verifies every recovered pending or active group and repairs violations
    /// deterministically, each in its own order step. Coverage mismatches are only reported
    /// here; the loaded-state alignment that runs next performs the resize or cancellation.
    pub(crate) fn verify_recovered_groups(&mut self) -> Result<(), String> {
        let groups = self
            .state
            .order_groups
            .values()
            .filter(|group| matches!(group.status, GroupStatus::Pending | GroupStatus::Active))
            .cloned()
            .collect::<Vec<_>>();
        let mut notices = Vec::new();
        for group in &groups {
            let Some(violation) = self.classify_recovered_group(group).err() else {
                continue;
            };
            let (status, kind, detail) = match violation {
                GroupViolation::Broken(detail) => (
                    GroupStatus::Cancelled,
                    RecoveryNoticeKind::BrokenGroup,
                    detail,
                ),
                GroupViolation::UnsettledFill(detail) => (
                    GroupStatus::Completed,
                    RecoveryNoticeKind::InterruptedGroupFill,
                    detail,
                ),
            };
            let drop_legs = status == GroupStatus::Cancelled;
            let mut step = OrderStep::new(self.state.next_sequence);
            self.stage_group_retirement(&mut step, group, status, drop_legs)?;
            self.commit_order_step(step, Vec::new())?;
            notices.push(RecoveryNotice {
                kind,
                subject: group.group_id.clone(),
                detail,
            });
        }
        self.recovery_notices.extend(notices);
        self.verify_recovered_coverage()
    }

    /// The verification failure of one recovered group, or `Ok` when it is sound.
    fn classify_recovered_group(&self, group: &OrderGroup) -> Result<(), GroupViolation> {
        if group.validate().is_err() {
            return Err(GroupViolation::Broken(
                "the stored group violates its structural invariants".to_string(),
            ));
        }
        match group.kind {
            GroupKind::Bracket => self.classify_recovered_bracket(group),
            GroupKind::Oco => {
                let members = group
                    .legs
                    .iter()
                    .map(|leg| self.recovered_member(group, leg, "member"))
                    .collect::<Result<Vec<_>, _>>()?;
                if members
                    .iter()
                    .any(|order| order.status == OrderStatus::Filled)
                {
                    return Err(GroupViolation::UnsettledFill(
                        "an OCO member filled before the last shutdown".to_string(),
                    ));
                }
                if !members.iter().any(|order| order.status.is_open()) {
                    return Err(GroupViolation::Broken(
                        "the OCO group has no working member".to_string(),
                    ));
                }
                Ok(())
            }
            GroupKind::Protection => {
                let leg = self.recovered_member(group, &group.legs[0], "protection")?;
                match leg.status {
                    OrderStatus::Filled => Err(GroupViolation::UnsettledFill(
                        "the protective order filled before the last shutdown".to_string(),
                    )),
                    status if status.is_open() => Ok(()),
                    _ => Err(GroupViolation::Broken(
                        "the protective order is no longer working".to_string(),
                    )),
                }
            }
        }
    }

    /// Bracket phases: a pending bracket holds an open entry and inactive legs; an active
    /// bracket follows a filled entry with working legs until one of them fills.
    fn classify_recovered_bracket(&self, group: &OrderGroup) -> Result<(), GroupViolation> {
        let entry_id = group
            .entry
            .as_ref()
            .ok_or_else(|| GroupViolation::Broken("the bracket entry is missing".to_string()))?;
        let entry = self
            .state
            .orders
            .get(entry_id)
            .filter(|order| {
                order.account_id == group.account_id && order.instrument_id == group.instrument_id
            })
            .ok_or_else(|| {
                GroupViolation::Broken("the bracket entry order is missing".to_string())
            })?;
        let mut legs = Vec::with_capacity(group.legs.len());
        for leg in &group.legs {
            legs.push(self.recovered_member(group, leg, "leg")?);
        }
        match group.status {
            GroupStatus::Pending => {
                if !entry.status.is_open() {
                    return Err(GroupViolation::Broken(
                        "the pending bracket's entry is no longer open".to_string(),
                    ));
                }
                if legs.iter().any(|leg| leg.status != OrderStatus::Inactive) {
                    return Err(GroupViolation::Broken(
                        "a pending bracket leg is not held".to_string(),
                    ));
                }
                Ok(())
            }
            GroupStatus::Active => {
                if entry.status != OrderStatus::Filled {
                    return Err(GroupViolation::Broken(
                        "the active bracket's entry is not filled".to_string(),
                    ));
                }
                let stop = legs[0];
                if stop.status == OrderStatus::Filled {
                    return Err(GroupViolation::UnsettledFill(
                        "the bracket stop filled before the last shutdown".to_string(),
                    ));
                }
                if !stop.status.is_open() && legs[1..].iter().any(|leg| leg.status.is_open()) {
                    return Err(GroupViolation::Broken(
                        "the active bracket's stop is not working".to_string(),
                    ));
                }
                if legs[1..]
                    .iter()
                    .any(|leg| leg.status == OrderStatus::Filled)
                {
                    let targets_open = legs[1..].iter().any(|leg| leg.status.is_open());
                    let position_open = self
                        .state
                        .positions
                        .get(&(group.account_id.clone(), group.instrument_id.clone()))
                        .is_some_and(|position| position.net_quantity.units() != 0);
                    if !targets_open || !position_open {
                        return Err(GroupViolation::UnsettledFill(
                            "a bracket target filled before the last shutdown".to_string(),
                        ));
                    }
                }
                if !legs.iter().any(|leg| leg.status.is_open()) {
                    return Err(GroupViolation::Broken(
                        "the active bracket has no working legs".to_string(),
                    ));
                }
                Ok(())
            }
            GroupStatus::Completed | GroupStatus::Cancelled => Ok(()),
        }
    }

    /// One group member's recovered order, checked against the group's scope.
    fn recovered_member(
        &self,
        group: &OrderGroup,
        leg: &GroupLeg,
        role: &str,
    ) -> Result<&Order, GroupViolation> {
        self.state
            .orders
            .get(&leg.order_id)
            .filter(|order| {
                order.account_id == group.account_id && order.instrument_id == group.instrument_id
            })
            .ok_or_else(|| GroupViolation::Broken(format!("the group's {role} order is missing")))
    }

    /// Stages one group retirement: every still-open leg is cascade-cancelled and the group
    /// advances to its terminal status in the same order step. A broken group additionally
    /// drops its torn legs: the retired row keeps only the terminal fact and stops
    /// referencing a lost order.
    fn stage_group_retirement(
        &self,
        step: &mut OrderStep,
        group: &OrderGroup,
        status: GroupStatus,
        drop_legs: bool,
    ) -> Result<(), String> {
        let mut retired = group.clone();
        retired.status = status;
        if drop_legs {
            retired.legs.clear();
        }
        retired.revision = group
            .revision
            .checked_add(1)
            .ok_or_else(|| "order group revision overflowed".to_string())?;
        retired
            .validate()
            .map_err(|error| format!("invalid order group: {error:?}"))?;
        for leg in &group.legs {
            let Some(order) = self.state.orders.get(&leg.order_id).cloned() else {
                continue;
            };
            if order.status.is_open() {
                self.stage_cancellation(step, order, false, CancellationCause::Cascade)?;
            }
        }
        step.groups.push(retired);
        Ok(())
    }

    /// Fits every practice position's protection back to it, finishing a resize a stop or a
    /// shutdown interrupted. A realigned scope is reported once, naming its first active
    /// group; a scope without a position keeps its historical orders untouched.
    fn verify_recovered_coverage(&mut self) -> Result<(), String> {
        let scopes = self
            .state
            .positions
            .keys()
            .filter(|(account_id, _)| {
                self.route(account_id)
                    .is_ok_and(|route| route == VenueRoute::Simulated)
            })
            .cloned()
            .collect::<Vec<_>>();
        for (account_id, instrument_id) in scopes {
            if !self.coverage_mismatch(&account_id, &instrument_id)? {
                continue;
            }
            let subject = self
                .state
                .order_groups
                .values()
                .filter(|group| {
                    group.status == GroupStatus::Active
                        && !matches!(group.kind, GroupKind::Oco)
                        && group.account_id == account_id
                        && group.instrument_id == instrument_id
                })
                .map(|group| group.group_id.clone())
                .min()
                .unwrap_or_else(|| instrument_id.as_str().to_string());
            self.align_protection(&account_id, &instrument_id, self.command_now)?;
            self.recovery_notices.push(RecoveryNotice {
                kind: RecoveryNoticeKind::CoverageRepair,
                subject,
                detail: format!(
                    "protection on {} no longer matched the position and is realigned at startup",
                    instrument_id.as_str()
                ),
            });
        }
        Ok(())
    }

    /// Whether the open protective orders of one position fail the coverage invariant: for
    /// each role, the total quantity must equal the net position or be zero, and no
    /// protective order may point the wrong way. A scope without a position is not repaired
    /// or reported.
    fn coverage_mismatch(
        &self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
    ) -> Result<bool, String> {
        let Some(net) = self
            .state
            .positions
            .get(&(account_id.clone(), instrument_id.clone()))
            .map(|position| position.net_quantity)
            .filter(|net| net.units() != 0)
        else {
            return Ok(false);
        };
        let mut totals = [0_u64; 2];
        for order in self.state.orders.values() {
            if !order.status.is_open()
                || &order.account_id != account_id
                || &order.instrument_id != instrument_id
            {
                continue;
            }
            let Some(role) = self.owned_leg_role(&order.client_order_id) else {
                continue;
            };
            if order.side.sign() != -net.units().signum() {
                return Ok(true);
            }
            let units = order
                .quantity
                .exact_rescale(net.scale())
                .map_err(|error| error.to_string())?
                .units()
                .unsigned_abs();
            let total = match role {
                ProtectionRole::StopLoss => &mut totals[0],
                ProtectionRole::TakeProfit => &mut totals[1],
            };
            *total = total
                .checked_add(units)
                .ok_or_else(|| "protection quantity overflowed".to_string())?;
        }
        let wanted = net.units().unsigned_abs();
        Ok(totals.iter().any(|total| *total != 0 && *total != wanted))
    }
}
