use aeris_trading::{ClientOrderId, FixedPoint};
use serde::{Deserialize, Serialize};

pub const MAXIMUM_BRACKET_TARGETS: usize = 4;
pub const MAXIMUM_STRATEGY_TEMPLATES: usize = 64;
pub const MAXIMUM_MANAGED_BRACKETS: usize = 4_096;

/// One profit-taking leg expressed in provider tick units and entry-quantity percent.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BracketTarget {
    pub offset_ticks: u32,
    pub quantity_percent: u8,
}

/// Local trailing-stop behavior activated after a favorable tick move.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrailingStopRule {
    pub activation_ticks: u32,
    pub distance_ticks: u32,
}

/// Local break-even behavior activated after a favorable tick move.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BreakEvenRule {
    pub activation_ticks: u32,
    pub offset_ticks: i32,
}

/// Durable strategy template interpreted only by the authoritative trading owner.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BracketStrategyTemplate {
    pub template_id: String,
    pub revision: u32,
    pub name: String,
    pub stop_offset_ticks: u32,
    pub targets: Vec<BracketTarget>,
    pub trailing_stop: Option<TrailingStopRule>,
    pub break_even: Option<BreakEvenRule>,
    pub enabled: bool,
}

/// Lifecycle of one locally managed bracket instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagedBracketStatus {
    AwaitingEntry,
    Active,
    Completed,
    Cancelled,
}

/// Semantic role of a standalone reduce-only protection order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtectiveOrderRole {
    StopLoss,
    TakeProfit,
}

impl ProtectiveOrderRole {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::StopLoss => "stop_loss",
            Self::TakeProfit => "take_profit",
        }
    }

    pub(super) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "stop_loss" => Ok(Self::StopLoss),
            "take_profit" => Ok(Self::TakeProfit),
            _ => Err("protective order role is invalid".to_string()),
        }
    }
}

/// Durable semantic metadata for one standalone protective order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtectiveOrder {
    pub client_order_id: ClientOrderId,
    pub role: ProtectiveOrderRole,
}

impl ManagedBracketStatus {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingEntry => "awaiting_entry",
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(super) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "awaiting_entry" => Ok(Self::AwaitingEntry),
            "active" => Ok(Self::Active),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err("managed bracket status is invalid".to_string()),
        }
    }
}

/// Durable state for one bracket whose child orders are owned by the trading runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedBracket {
    pub bracket_id: String,
    pub template: BracketStrategyTemplate,
    pub entry_client_order_id: ClientOrderId,
    pub stop_client_order_id: Option<ClientOrderId>,
    pub target_client_order_ids: Vec<ClientOrderId>,
    pub status: ManagedBracketStatus,
    pub entry_price: Option<FixedPoint>,
}

impl ManagedBracket {
    /// Makes locally managed behavior explicit to presentation and audit surfaces.
    pub const MANAGEMENT_LABEL: &'static str = BracketStrategyTemplate::MANAGEMENT_LABEL;

    pub(super) fn validate(&self) -> Result<(), String> {
        if self.bracket_id.trim().is_empty() || self.bracket_id.len() > 256 {
            return Err("managed bracket identity is invalid".to_string());
        }
        self.template.validate()?;
        if self.target_client_order_ids.len() > MAXIMUM_BRACKET_TARGETS {
            return Err("managed bracket target count exceeds the limit".to_string());
        }
        match self.status {
            ManagedBracketStatus::AwaitingEntry
                if self.stop_client_order_id.is_some()
                    || !self.target_client_order_ids.is_empty()
                    || self.entry_price.is_some() =>
            {
                Err("awaiting bracket cannot contain active child state".to_string())
            }
            ManagedBracketStatus::Active
                if self.stop_client_order_id.is_none()
                    || self.target_client_order_ids.is_empty()
                    || self.entry_price.is_none() =>
            {
                Err("active bracket requires its entry price and child orders".to_string())
            }
            _ => Ok(()),
        }
    }
}

impl BracketStrategyTemplate {
    /// Every simulated bracket is managed locally until a provider advertises native support.
    pub const MANAGEMENT_LABEL: &'static str = "LOCAL-MANAGED";

    /// Validates identities, target bounds, scale-out allocation, and management rules.
    ///
    /// # Errors
    /// Returns an error when the template cannot produce deterministic child orders.
    pub fn validate(&self) -> Result<(), String> {
        if self.template_id.trim().is_empty() || self.template_id.len() > 256 {
            return Err("strategy template identity is invalid".to_string());
        }
        if self.name.trim().is_empty() || self.name.len() > 128 {
            return Err("strategy template name is invalid".to_string());
        }
        if self.revision == 0 {
            return Err("strategy template revision must be positive".to_string());
        }
        if self.stop_offset_ticks == 0 {
            return Err("strategy template stop offset must be positive".to_string());
        }
        if self.targets.is_empty() || self.targets.len() > MAXIMUM_BRACKET_TARGETS {
            return Err(format!(
                "strategy template must contain 1..={MAXIMUM_BRACKET_TARGETS} targets"
            ));
        }
        let mut previous_offset = 0;
        let mut allocated_percent = 0_u16;
        for target in &self.targets {
            if target.offset_ticks == 0 || target.offset_ticks <= previous_offset {
                return Err(
                    "strategy template targets must have increasing positive offsets".to_string(),
                );
            }
            if target.quantity_percent == 0 {
                return Err("strategy template target allocation must be positive".to_string());
            }
            previous_offset = target.offset_ticks;
            allocated_percent = allocated_percent
                .checked_add(u16::from(target.quantity_percent))
                .ok_or_else(|| "strategy template target allocation overflowed".to_string())?;
        }
        if allocated_percent != 100 {
            return Err("strategy template target allocation must total 100 percent".to_string());
        }
        if let Some(trailing) = self.trailing_stop
            && (trailing.activation_ticks == 0 || trailing.distance_ticks == 0)
        {
            return Err("strategy template trailing rule must use positive ticks".to_string());
        }
        if let Some(break_even) = self.break_even {
            if break_even.activation_ticks == 0 {
                return Err("strategy template break-even activation must be positive".to_string());
            }
            if break_even.offset_ticks.unsigned_abs() > break_even.activation_ticks {
                return Err(
                    "strategy template break-even offset exceeds its activation".to_string()
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template() -> BracketStrategyTemplate {
        BracketStrategyTemplate {
            template_id: "two-target".to_string(),
            revision: 1,
            name: "Two target bracket".to_string(),
            stop_offset_ticks: 8,
            targets: vec![
                BracketTarget {
                    offset_ticks: 8,
                    quantity_percent: 50,
                },
                BracketTarget {
                    offset_ticks: 16,
                    quantity_percent: 50,
                },
            ],
            trailing_stop: Some(TrailingStopRule {
                activation_ticks: 12,
                distance_ticks: 6,
            }),
            break_even: Some(BreakEvenRule {
                activation_ticks: 8,
                offset_ticks: 1,
            }),
            enabled: true,
        }
    }

    #[test]
    fn complete_scale_out_and_management_rules_validate() {
        assert!(template().validate().is_ok());
    }

    #[test]
    fn incomplete_or_unordered_scale_out_is_rejected() {
        let mut incomplete = template();
        incomplete.targets[1].quantity_percent = 40;
        assert!(incomplete.validate().is_err());

        let mut unordered = template();
        unordered.targets[1].offset_ticks = unordered.targets[0].offset_ticks;
        assert!(unordered.validate().is_err());
    }
}
