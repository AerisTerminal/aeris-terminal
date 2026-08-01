//! Pure fail-closed policy evaluation over an optional snapshot.

use crate::decision::AuthorizationDecision;
use crate::identifiers::PolicyVersion;
use crate::policy::AuthorizationPolicySnapshot;
use crate::request::{AuthorizationAction, AuthorizationRequest};

/// Pure fail-closed evaluator over an optional immutable policy snapshot.
///
/// This evaluator establishes exact policy grants only. Matching `Trade` and
/// `Administer` grants return [`AuthorizationOutcome::AdditionalAssuranceRequired`]
/// until separate fresh-session, step-up, risk, jurisdiction, and entitlement
/// stages complete.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AuthorizationEvaluator {
    snapshot: Option<AuthorizationPolicySnapshot>,
}

impl AuthorizationEvaluator {
    /// Creates a fail-closed evaluator with no loaded policy.
    #[must_use]
    pub const fn unavailable() -> Self {
        Self { snapshot: None }
    }

    /// Creates an evaluator over one immutable validated snapshot.
    #[must_use]
    pub const fn with_snapshot(snapshot: AuthorizationPolicySnapshot) -> Self {
        Self {
            snapshot: Some(snapshot),
        }
    }

    /// Evaluates only the exact principal/resource/action policy key.
    ///
    /// Correlation context and grant insertion order cannot alter the outcome.
    #[must_use]
    pub fn evaluate(&self, request: &AuthorizationRequest) -> AuthorizationDecision {
        let Some(snapshot) = &self.snapshot else {
            return AuthorizationDecision::policy_unavailable();
        };
        match snapshot.matching_grant(request) {
            Some(grant_id) => match request.action {
                AuthorizationAction::Read | AuthorizationAction::Stream => {
                    AuthorizationDecision::allowed(snapshot.version, grant_id.clone())
                }
                AuthorizationAction::Trade | AuthorizationAction::Administer => {
                    AuthorizationDecision::additional_assurance_required(
                        snapshot.version,
                        grant_id.clone(),
                        request.action,
                    )
                }
            },
            None => AuthorizationDecision::no_matching_grant(snapshot.version),
        }
    }

    /// Returns the installed version, or `None` while policy is unavailable.
    #[must_use]
    pub const fn policy_version(&self) -> Option<PolicyVersion> {
        match &self.snapshot {
            Some(snapshot) => Some(snapshot.version),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AuthorizationEvaluator;
    use crate::decision::{AuthorizationDecisionReason, AuthorizationOutcome};
    use crate::identifiers::PolicyVersion;
    use crate::request::AuthorizationAction;
    use crate::test_fixture::{request, snapshot};

    #[test]
    fn unavailable_and_unmatched_policy_decisions_deny_with_evidence() {
        let unavailable =
            AuthorizationEvaluator::unavailable().evaluate(&request(AuthorizationAction::Read));
        assert_eq!(unavailable.outcome(), AuthorizationOutcome::Denied);
        assert_eq!(unavailable.policy_version(), None);
        assert_eq!(
            unavailable.reason(),
            &AuthorizationDecisionReason::PolicyUnavailable
        );

        let evaluator = AuthorizationEvaluator::with_snapshot(snapshot(AuthorizationAction::Read));
        let unmatched = evaluator.evaluate(&request(AuthorizationAction::Stream));
        assert_eq!(unmatched.outcome(), AuthorizationOutcome::Denied);
        assert_eq!(unmatched.policy_version().map(PolicyVersion::get), Some(7));
        assert_eq!(
            unmatched.reason(),
            &AuthorizationDecisionReason::NoMatchingGrant
        );
    }

    #[test]
    fn matching_read_is_allowed_but_sensitive_actions_require_assurance() {
        let read = AuthorizationEvaluator::with_snapshot(snapshot(AuthorizationAction::Read))
            .evaluate(&request(AuthorizationAction::Read));
        assert!(read.is_allowed());
        assert!(matches!(
            read.reason(),
            AuthorizationDecisionReason::MatchingGrant { grant_id }
                if grant_id.as_str() == "grant-1"
        ));

        for action in [AuthorizationAction::Trade, AuthorizationAction::Administer] {
            let decision =
                AuthorizationEvaluator::with_snapshot(snapshot(action)).evaluate(&request(action));
            assert!(!decision.is_allowed());
            assert!(decision.requires_additional_assurance());
            assert!(matches!(
                decision.reason(),
                AuthorizationDecisionReason::SensitiveActionRequiresAdditionalAssurance {
                    grant_id,
                    action: actual,
                } if grant_id.as_str() == "grant-1" && *actual == action
            ));
        }
    }
}
