//! Axiusflow-owned account, plan, and feature identity.
//!
//! Pure provider-neutral contracts for phase 5 authentication. No HTTP, no
//! GPUI, no vault, no Better Auth, no billing SDK, no provider wire types.
//! The resident engine owns session state; the desktop renders sanitized
//! views; the cloud control plane owns identity truth.

use core::fmt;
use std::error::Error;

/// Maximum serialized length for an account identifier.
pub const MAXIMUM_ACCOUNT_ID_BYTES: usize = 128;
/// Maximum redacted detail length carried in a sanitized account view.
pub const MAXIMUM_ACCOUNT_DETAIL_BYTES: usize = 256;

/// Canonical Axiusflow account identity. External identifiers (identity-provider
/// user ID, email, billing-vendor customer, Rithmic account) are links, never
/// this value.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AccountId(String);

impl AccountId {
    /// Creates an account identifier from a control-plane issued value.
    ///
    /// # Errors
    ///
    /// Returns [`AccountValidationError`] when the value is empty or too long.
    pub fn try_new(value: impl Into<String>) -> Result<Self, AccountValidationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(AccountValidationError::EmptyAccountId);
        }
        if value.len() > MAXIMUM_ACCOUNT_ID_BYTES {
            return Err(AccountValidationError::AccountIdTooLong);
        }
        Ok(Self(value))
    }

    /// Returns the serialized identifier value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Internal subscription plan. Vendor product/price IDs map to these values
/// server-side and never cross native IPC.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PlanId {
    Starter,
    Pro,
    Elite,
    Enterprise,
}

impl PlanId {
    /// Parses the canonical lowercase plan wire value.
    ///
    /// # Errors
    ///
    /// Returns [`AccountValidationError::UnknownPlan`] for anything else.
    pub fn try_parse(value: &str) -> Result<Self, AccountValidationError> {
        match value {
            "starter" => Ok(Self::Starter),
            "pro" => Ok(Self::Pro),
            "elite" => Ok(Self::Elite),
            "enterprise" => Ok(Self::Enterprise),
            _ => Err(AccountValidationError::UnknownPlan),
        }
    }

    /// Returns the canonical lowercase wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starter => "starter",
            Self::Pro => "pro",
            Self::Elite => "elite",
            Self::Enterprise => "enterprise",
        }
    }
}

/// One gated product capability inside a plan.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FeatureId {
    AdvancedCharts,
    PremiumAnalytics,
    CustomTimeframes,
    DataExport,
    PrioritySupport,
}

impl FeatureId {
    /// Bit position of this feature inside [`FeatureSet`].
    #[must_use]
    pub const fn bit(self) -> u32 {
        match self {
            Self::AdvancedCharts => 1 << 0,
            Self::PremiumAnalytics => 1 << 1,
            Self::CustomTimeframes => 1 << 2,
            Self::DataExport => 1 << 3,
            Self::PrioritySupport => 1 << 4,
        }
    }

    /// All features known to this release.
    #[must_use]
    pub const fn all() -> u32 {
        (1 << 5) - 1
    }
}

/// Bounded set of gated capabilities granted by the current plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeatureSet {
    bits: u32,
}

impl FeatureSet {
    /// Creates a feature set after rejecting unknown bits.
    ///
    /// # Errors
    ///
    /// Returns [`AccountValidationError::UnknownFeature`] when unknown bits are set.
    pub fn try_new(bits: u32) -> Result<Self, AccountValidationError> {
        if bits & !FeatureId::all() != 0 {
            return Err(AccountValidationError::UnknownFeature);
        }
        Ok(Self { bits })
    }

    /// Returns the raw feature bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.bits
    }

    /// Returns whether the set contains one feature.
    #[must_use]
    pub const fn contains(self, feature: FeatureId) -> bool {
        self.bits & feature.bit() != 0
    }

    /// Returns the empty feature set.
    #[must_use]
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }
}

/// Engine-owned account session state shared by all desktop windows.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AccountState {
    SignedOut,
    Authorizing,
    Active,
    OfflineLease,
    ReauthenticationRequired,
    LeaseExpired,
    TerminalError,
}

impl AccountState {
    /// Parses the canonical lowercase state wire value.
    ///
    /// # Errors
    ///
    /// Returns [`AccountValidationError::UnknownAccountState`] for anything else.
    pub fn try_parse(value: &str) -> Result<Self, AccountValidationError> {
        match value {
            "signed_out" => Ok(Self::SignedOut),
            "authorizing" => Ok(Self::Authorizing),
            "active" => Ok(Self::Active),
            "offline_lease" => Ok(Self::OfflineLease),
            "reauthentication_required" => Ok(Self::ReauthenticationRequired),
            "lease_expired" => Ok(Self::LeaseExpired),
            "terminal_error" => Ok(Self::TerminalError),
            _ => Err(AccountValidationError::UnknownAccountState),
        }
    }

    /// Returns the canonical lowercase wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SignedOut => "signed_out",
            Self::Authorizing => "authorizing",
            Self::Active => "active",
            Self::OfflineLease => "offline_lease",
            Self::ReauthenticationRequired => "reauthentication_required",
            Self::LeaseExpired => "lease_expired",
            Self::TerminalError => "terminal_error",
        }
    }
}

/// Sanitized account view safe for desktop rendering. Contains no tokens,
/// emails, names, vendor customer IDs, or payment details.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountView {
    state: AccountState,
    account_id: Option<AccountId>,
    plan: Option<PlanId>,
    detail: String,
}

impl AccountView {
    /// Creates a sanitized view, bounding the redacted detail.
    ///
    /// # Errors
    ///
    /// Returns [`AccountValidationError::DetailTooLong`] when detail is too long,
    /// or [`AccountValidationError::AccountWithoutIdentity`] when a non-signed-out
    /// state carries no account identity.
    pub fn try_new(
        state: AccountState,
        account_id: Option<AccountId>,
        plan: Option<PlanId>,
        detail: impl Into<String>,
    ) -> Result<Self, AccountValidationError> {
        let detail = detail.into();
        if detail.len() > MAXIMUM_ACCOUNT_DETAIL_BYTES {
            return Err(AccountValidationError::DetailTooLong);
        }
        if state != AccountState::SignedOut
            && state != AccountState::Authorizing
            && account_id.is_none()
        {
            return Err(AccountValidationError::AccountWithoutIdentity);
        }
        Ok(Self {
            state,
            account_id,
            plan,
            detail,
        })
    }

    /// Returns the session state.
    #[must_use]
    pub const fn state(&self) -> AccountState {
        self.state
    }

    /// Returns the canonical account identity, if any.
    #[must_use]
    pub const fn account_id(&self) -> Option<&AccountId> {
        self.account_id.as_ref()
    }

    /// Returns the current plan, if known.
    #[must_use]
    pub const fn plan(&self) -> Option<PlanId> {
        self.plan
    }

    /// Returns the redacted human-readable detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

/// Validation failures for pure account contracts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountValidationError {
    EmptyAccountId,
    AccountIdTooLong,
    UnknownPlan,
    UnknownFeature,
    UnknownAccountState,
    DetailTooLong,
    AccountWithoutIdentity,
}

impl fmt::Display for AccountValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyAccountId => formatter.write_str("account identity is empty"),
            Self::AccountIdTooLong => formatter.write_str("account identity is too long"),
            Self::UnknownPlan => formatter.write_str("plan is unknown"),
            Self::UnknownFeature => formatter.write_str("feature set carries unknown bits"),
            Self::UnknownAccountState => formatter.write_str("account state is unknown"),
            Self::DetailTooLong => formatter.write_str("account detail is too long"),
            Self::AccountWithoutIdentity => {
                formatter.write_str("account state requires an account identity")
            }
        }
    }
}

impl Error for AccountValidationError {}

#[cfg(test)]
mod tests {
    use super::{
        AccountId, AccountState, AccountValidationError, AccountView, FeatureId, FeatureSet, PlanId,
    };

    #[test]
    fn account_id_rejects_empty_and_overlong_values() {
        assert_eq!(
            AccountId::try_new("").expect_err("empty identity fails"),
            AccountValidationError::EmptyAccountId
        );
        assert_eq!(
            AccountId::try_new("   ").expect_err("blank identity fails"),
            AccountValidationError::EmptyAccountId
        );
        assert_eq!(
            AccountId::try_new("a".repeat(129)).expect_err("overlong identity fails"),
            AccountValidationError::AccountIdTooLong
        );
        assert_eq!(
            AccountId::try_new("acct_01")
                .expect("valid identity passes")
                .as_str(),
            "acct_01"
        );
    }

    #[test]
    fn plan_and_state_wire_values_round_trip() {
        for plan in [
            PlanId::Starter,
            PlanId::Pro,
            PlanId::Elite,
            PlanId::Enterprise,
        ] {
            assert_eq!(PlanId::try_parse(plan.as_str()), Ok(plan));
        }
        assert_eq!(
            PlanId::try_parse("price_123").expect_err("vendor price id fails"),
            AccountValidationError::UnknownPlan
        );
        for state in [
            AccountState::SignedOut,
            AccountState::Authorizing,
            AccountState::Active,
            AccountState::OfflineLease,
            AccountState::ReauthenticationRequired,
            AccountState::LeaseExpired,
            AccountState::TerminalError,
        ] {
            assert_eq!(AccountState::try_parse(state.as_str()), Ok(state));
        }
    }

    #[test]
    fn feature_set_rejects_unknown_bits() {
        let set =
            FeatureSet::try_new(FeatureId::AdvancedCharts.bit() | FeatureId::DataExport.bit())
                .expect("known bits pass");
        assert!(set.contains(FeatureId::AdvancedCharts));
        assert!(!set.contains(FeatureId::PrioritySupport));
        assert_eq!(
            FeatureSet::try_new(1 << 31).expect_err("unknown bit fails"),
            AccountValidationError::UnknownFeature
        );
    }

    #[test]
    fn account_view_requires_identity_outside_signed_out_and_authorizing() {
        AccountView::try_new(AccountState::SignedOut, None, None, "signed out")
            .expect("signed out needs no identity");
        AccountView::try_new(AccountState::Authorizing, None, None, "waiting for browser")
            .expect("authorizing needs no identity yet");
        assert_eq!(
            AccountView::try_new(AccountState::Active, None, None, "active")
                .expect_err("active needs identity"),
            AccountValidationError::AccountWithoutIdentity
        );
        let view = AccountView::try_new(
            AccountState::Active,
            Some(AccountId::try_new("acct_01").expect("identity builds")),
            Some(PlanId::Pro),
            "active",
        )
        .expect("active view builds");
        assert_eq!(view.state(), AccountState::Active);
        assert_eq!(view.plan(), Some(PlanId::Pro));
    }
}
