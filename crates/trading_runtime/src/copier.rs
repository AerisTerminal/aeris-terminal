use aeris_trading::{FixedPoint, TradingAccountId};
use std::collections::BTreeSet;

pub const MAXIMUM_COPIER_TARGETS: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradeCopierTarget {
    pub account_id: TradingAccountId,
    pub quantity_multiplier: FixedPoint,
    pub enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradeCopierConfig {
    pub source_account_id: TradingAccountId,
    pub revision: u32,
    pub enabled: bool,
    pub targets: Vec<TradeCopierTarget>,
}

impl TradeCopierConfig {
    /// Validates revision, target bounds, uniqueness, and exact positive multipliers.
    ///
    /// # Errors
    /// Returns an error when the configuration cannot be routed deterministically.
    pub fn validate(&self) -> Result<(), String> {
        if self.revision == 0 {
            return Err("trade copier revision must be positive".to_string());
        }
        if self.targets.is_empty() || self.targets.len() > MAXIMUM_COPIER_TARGETS {
            return Err(format!(
                "trade copier must contain 1..={MAXIMUM_COPIER_TARGETS} targets"
            ));
        }
        let mut accounts = BTreeSet::new();
        for target in &self.targets {
            if target.account_id == self.source_account_id {
                return Err("trade copier source cannot also be a target".to_string());
            }
            if !accounts.insert(target.account_id.clone()) {
                return Err("trade copier targets must be unique".to_string());
            }
            if target.quantity_multiplier.units() <= 0 {
                return Err("trade copier multiplier must be positive".to_string());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradeCopyDispatch {
    pub source_client_order_id: String,
    pub target_account_id: TradingAccountId,
    pub mirrored_client_order_id: String,
    pub accepted: bool,
    pub detail: Option<String>,
}
