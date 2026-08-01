//! Versioned authorization decisions and their machine-readable evidence.

use crate::identifiers::{AuthorizationGrantId, PolicyVersion};
use crate::request::AuthorizationAction;

/// Stable policy result independent of presentation wording.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationOutcome {
    Allowed,
    AdditionalAssuranceRequired,
    Denied,
}

/// Typed evidence explaining a deterministic policy result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorizationDecisionReason {
    MatchingGrant {
        grant_id: AuthorizationGrantId,
    },
    SensitiveActionRequiresAdditionalAssurance {
        grant_id: AuthorizationGrantId,
        action: AuthorizationAction,
    },
    NoMatchingGrant,
    PolicyUnavailable,
}

/// A versioned authorization result suitable for audit evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationDecision {
    outcome: AuthorizationOutcome,
    policy_version: Option<PolicyVersion>,
    reason: AuthorizationDecisionReason,
}

impl AuthorizationDecision {
    /// Produces the fail-closed result used when no policy snapshot is available.
    #[must_use]
    pub const fn policy_unavailable() -> Self {
        Self {
            outcome: AuthorizationOutcome::Denied,
            policy_version: None,
            reason: AuthorizationDecisionReason::PolicyUnavailable,
        }
    }

    pub(crate) fn allowed(policy_version: PolicyVersion, grant_id: AuthorizationGrantId) -> Self {
        Self {
            outcome: AuthorizationOutcome::Allowed,
            policy_version: Some(policy_version),
            reason: AuthorizationDecisionReason::MatchingGrant { grant_id },
        }
    }

    pub(crate) fn additional_assurance_required(
        policy_version: PolicyVersion,
        grant_id: AuthorizationGrantId,
        action: AuthorizationAction,
    ) -> Self {
        Self {
            outcome: AuthorizationOutcome::AdditionalAssuranceRequired,
            policy_version: Some(policy_version),
            reason: AuthorizationDecisionReason::SensitiveActionRequiresAdditionalAssurance {
                grant_id,
                action,
            },
        }
    }

    pub(crate) const fn no_matching_grant(policy_version: PolicyVersion) -> Self {
        Self {
            outcome: AuthorizationOutcome::Denied,
            policy_version: Some(policy_version),
            reason: AuthorizationDecisionReason::NoMatchingGrant,
        }
    }

    /// Returns the stable decision outcome.
    #[must_use]
    pub const fn outcome(&self) -> AuthorizationOutcome {
        self.outcome
    }

    /// Returns whether this decision allows the exact policy operation.
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self.outcome, AuthorizationOutcome::Allowed)
    }

    /// Returns whether a matching sensitive grant still needs assurance gates.
    #[must_use]
    pub const fn requires_additional_assurance(&self) -> bool {
        matches!(
            self.outcome,
            AuthorizationOutcome::AdditionalAssuranceRequired
        )
    }

    /// Returns the evaluated policy version, or `None` when policy was unavailable.
    #[must_use]
    pub const fn policy_version(&self) -> Option<PolicyVersion> {
        self.policy_version
    }

    /// Returns stable machine-readable decision evidence.
    #[must_use]
    pub const fn reason(&self) -> &AuthorizationDecisionReason {
        &self.reason
    }
}
