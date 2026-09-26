use aeris_instruments::InstrumentId;
use aeris_trading::{FixedPoint, TradingAccountId};

pub const MAXIMUM_PLAN_LEVELS: usize = 32;
pub const MAXIMUM_ALLOWED_SETUPS: usize = 16;
pub const MAXIMUM_CHECKLIST_ITEMS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionBias {
    Long,
    Short,
    Neutral,
}

impl SessionBias {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Long => "long",
            Self::Short => "short",
            Self::Neutral => "neutral",
        }
    }

    pub(super) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "long" => Ok(Self::Long),
            "short" => Ok(Self::Short),
            "neutral" => Ok(Self::Neutral),
            _ => Err("session plan bias is invalid".to_string()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionPlanLevel {
    pub instrument_id: InstrumentId,
    pub label: String,
    pub price: FixedPoint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionChecklistItem {
    pub item_id: String,
    pub label: String,
    pub completed: bool,
}

/// One revisioned pre-session plan owned and enforced by the trading runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionPlan {
    pub account_id: TradingAccountId,
    pub plan_id: String,
    pub revision: u32,
    pub session_start_unix_nanos: i64,
    pub session_end_unix_nanos: i64,
    pub bias: SessionBias,
    pub maximum_loss: FixedPoint,
    /// Realized P/L at the plan boundary, retained independently of fill-history retention.
    pub session_start_realized_pnl: FixedPoint,
    pub allowed_setups: Vec<String>,
    pub active_setup: Option<String>,
    pub checklist: Vec<SessionChecklistItem>,
    pub levels: Vec<SessionPlanLevel>,
}

impl SessionPlan {
    /// Validates bounded identities, hours, loss, setups, checklist, and exact plan levels.
    ///
    /// # Errors
    /// Returns an error when the plan cannot be enforced deterministically.
    pub fn validate(&self) -> Result<(), String> {
        if self.plan_id.trim().is_empty() || self.plan_id.len() > 256 || self.revision == 0 {
            return Err("session plan identity or revision is invalid".to_string());
        }
        if self.session_start_unix_nanos <= 0
            || self.session_end_unix_nanos <= self.session_start_unix_nanos
        {
            return Err("session plan time range is invalid".to_string());
        }
        if self.maximum_loss.units() <= 0 || self.maximum_loss.scale() > 18 {
            return Err("session plan maximum loss is invalid".to_string());
        }
        if self.session_start_realized_pnl.scale() != self.maximum_loss.scale() {
            return Err("session plan P/L scales do not match".to_string());
        }
        if self.allowed_setups.len() > MAXIMUM_ALLOWED_SETUPS
            || self.checklist.len() > MAXIMUM_CHECKLIST_ITEMS
            || self.levels.len() > MAXIMUM_PLAN_LEVELS
        {
            return Err("session plan bounded collection limit exceeded".to_string());
        }
        validate_unique_strings(&self.allowed_setups, "allowed setup")?;
        if self
            .active_setup
            .as_ref()
            .is_some_and(|active| !self.allowed_setups.iter().any(|allowed| allowed == active))
        {
            return Err("session plan active setup is not allowed".to_string());
        }
        let item_ids = self
            .checklist
            .iter()
            .map(|item| item.item_id.clone())
            .collect::<Vec<_>>();
        validate_unique_strings(&item_ids, "checklist item")?;
        for item in &self.checklist {
            validate_text(&item.label, "checklist label", 256)?;
        }
        for level in &self.levels {
            validate_text(&level.label, "plan level label", 128)?;
            if level.price.units() <= 0 || level.price.scale() > 18 {
                return Err("session plan level price is invalid".to_string());
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.checklist.iter().all(|item| item.completed)
            && (self.allowed_setups.is_empty() || self.active_setup.is_some())
    }

    #[must_use]
    pub const fn contains_time(&self, unix_nanos: i64) -> bool {
        unix_nanos >= self.session_start_unix_nanos && unix_nanos <= self.session_end_unix_nanos
    }
}

/// End-of-session facts derived from the canonical plan and retained execution log.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionAdherenceReview {
    pub account_id: TradingAccountId,
    pub plan_id: String,
    pub plan_revision: u32,
    pub checklist_completed: usize,
    pub checklist_total: usize,
    pub active_setup: Option<String>,
    pub retained_fill_count: usize,
    pub fills_outside_planned_hours: usize,
    pub maximum_loss_respected: bool,
}

fn validate_unique_strings(values: &[String], label: &str) -> Result<(), String> {
    let mut sorted = values.to_vec();
    sorted.sort();
    for value in &sorted {
        validate_text(value, label, 128)?;
    }
    if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(format!("session plan {label} identities must be unique"));
    }
    Ok(())
}

fn validate_text(value: &str, label: &str, maximum_bytes: usize) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > maximum_bytes {
        return Err(format!("session plan {label} is invalid"));
    }
    Ok(())
}
