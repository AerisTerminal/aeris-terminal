use aeris_trading::{FixedPoint, TradingAccountId, TradingValidationError};

/// Whether a trailing drawdown is evaluated continuously or only at the session close.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrailingDrawdownMode {
    Intraday,
    EndOfDay,
}

impl TrailingDrawdownMode {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Intraday => "intraday",
            Self::EndOfDay => "end_of_day",
        }
    }

    pub(super) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "intraday" => Ok(Self::Intraday),
            "end_of_day" => Ok(Self::EndOfDay),
            _ => Err("risk profile trailing mode is invalid".to_string()),
        }
    }
}

/// Versioned deterministic account rules evaluated by the trading owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RiskProfile {
    pub account_id: TradingAccountId,
    pub profile_id: String,
    pub version: u32,
    pub session_start_unix_nanos: i64,
    pub daily_loss_limit: FixedPoint,
    pub trailing_drawdown: Option<FixedPoint>,
    pub trailing_mode: TrailingDrawdownMode,
    pub max_contracts: FixedPoint,
    pub consistency_max_single_trade_percent: Option<u8>,
    pub restricted_until_unix_nanos: Option<i64>,
    pub enabled: bool,
}

impl RiskProfile {
    /// Validates all versioned limits and optional restrictions.
    ///
    /// # Errors
    /// Returns an error when an identity, scale, limit, or timestamp is invalid.
    pub fn validate(&self) -> Result<(), String> {
        if self.profile_id.trim().is_empty() || self.profile_id.len() > 256 {
            return Err("risk profile identity is invalid".to_string());
        }
        if self.version == 0 {
            return Err("risk profile version must be positive".to_string());
        }
        if self.session_start_unix_nanos <= 0 {
            return Err("risk profile session start must be positive".to_string());
        }
        validate_positive(self.daily_loss_limit, "daily loss limit")?;
        if let Some(drawdown) = self.trailing_drawdown {
            validate_positive(drawdown, "trailing drawdown")?;
            if drawdown.scale() != self.daily_loss_limit.scale() {
                return Err("risk profile currency scales do not match".to_string());
            }
        }
        validate_positive(self.max_contracts, "maximum contracts")?;
        if let Some(percent) = self.consistency_max_single_trade_percent
            && !(1..=100).contains(&percent)
        {
            return Err("risk profile consistency percentage must be 1..=100".to_string());
        }
        if self
            .restricted_until_unix_nanos
            .is_some_and(|timestamp| timestamp <= 0)
        {
            return Err("risk profile restriction timestamp must be positive".to_string());
        }
        Ok(())
    }
}

/// A durable hard lock which rejects every new order for an account.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RiskLock {
    pub account_id: TradingAccountId,
    pub reason: String,
    pub locked_at_unix_nanos: i64,
    pub profile_id: Option<String>,
    pub profile_version: Option<u32>,
}

impl RiskLock {
    /// Validates the durable lock reason and provenance.
    ///
    /// # Errors
    /// Returns an error when the reason, profile identity, or timestamp is invalid.
    pub fn validate(&self) -> Result<(), String> {
        if self.reason.trim().is_empty() || self.reason.len() > 256 {
            return Err("risk lock reason is invalid".to_string());
        }
        if self.locked_at_unix_nanos <= 0 {
            return Err("risk lock timestamp must be positive".to_string());
        }
        if self
            .profile_id
            .as_ref()
            .is_some_and(|value| value.len() > 256)
        {
            return Err("risk lock profile identity is invalid".to_string());
        }
        Ok(())
    }
}

/// Result of evaluating one order against the account's current rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RiskEvaluation {
    pub warnings: Vec<String>,
    pub projected_contracts: FixedPoint,
    pub current_realized_pnl: FixedPoint,
}

fn validate_positive(value: FixedPoint, label: &str) -> Result<(), String> {
    if value.units() <= 0 {
        return Err(format!("{label} must be positive"));
    }
    if value.scale() > 18 {
        return Err(TradingValidationError::ScaleOutOfRange(value.scale()).to_string());
    }
    Ok(())
}
