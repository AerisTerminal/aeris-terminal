//! Entitlement enforcement for live client streams.
//!
//! Every connection proves a service-access entitlement before one byte of
//! market data flows: the Ed25519 token is verified offline against the
//! revisioned JWKS published by the auth service, and the authorization
//! domain's exact-grant snapshot decides `stream:<product>` access. A
//! supervisor resnapshots policy and keys on a bounded interval; a connection
//! whose entitlement disappears is disconnected, never silently retained.

use axiusflow_authorization::{
    AuthorizationAction, AuthorizationDecisionReason, AuthorizationEvaluator, AuthorizationOutcome,
    AuthorizationRequest, PrincipalId, ResourceId,
};
use axiusflow_security::{
    Ed25519JwtVerifier, JwtVerificationRequest, JwtVerifier, SystemVerificationClock, TokenPurpose,
};

/// Reason an entitlement check failed.
#[derive(Debug)]
pub enum EntitlementError {
    TokenInvalid,
    TokenExpired,
    KeyUnavailable,
    NoMatchingGrant,
    AdditionalAssuranceRequired,
    PolicyUnavailable,
}

/// One loaded entitlement snapshot: keys plus policy.
pub struct EntitlementGuard {
    verifier: Ed25519JwtVerifier<SystemVerificationClock>,
    evaluator: AuthorizationEvaluator,
    policy_version: u64,
    key_revision: u64,
}

impl EntitlementGuard {
    /// Builds the guard from the verifier and policy snapshot.
    #[must_use]
    pub fn new(
        verifier: Ed25519JwtVerifier<SystemVerificationClock>,
        evaluator: AuthorizationEvaluator,
    ) -> Self {
        let policy_version = evaluator
            .policy_version()
            .map_or(0, axiusflow_authorization::PolicyVersion::get);
        let key_revision = verifier.key_set_revision();
        Self {
            verifier,
            evaluator,
            policy_version,
            key_revision,
        }
    }

    /// The installed policy version.
    #[must_use]
    pub const fn policy_version(&self) -> u64 {
        self.policy_version
    }

    /// The JWKS revision in use.
    #[allow(dead_code)]
    #[must_use]
    pub const fn key_revision(&self) -> u64 {
        self.key_revision
    }

    /// Re-evaluates one request under the installed snapshot (used by
    /// resnapshot revocation).
    #[must_use]
    pub fn evaluator_outcome(&self, request: &AuthorizationRequest) -> AuthorizationOutcome {
        self.evaluator.evaluate(request).outcome()
    }

    /// Authorizes one token for one product's stream.
    ///
    /// # Errors
    ///
    /// Returns the exact denial reason; a valid token without a matching grant
    /// is denied like any other failure.
    pub fn authorize(&self, token: &str, product: &str) -> Result<String, EntitlementError> {
        let identity = self
            .verifier
            .verify(JwtVerificationRequest {
                encoded_token: token,
                expected_issuer: "axiusflow",
                expected_audience: "authorization_service",
                purpose: TokenPurpose::ServiceAccess,
            })
            .map_err(|error| match error {
                axiusflow_security::JwtVerificationError::Expired => EntitlementError::TokenExpired,
                axiusflow_security::JwtVerificationError::KeyUnavailable => {
                    EntitlementError::KeyUnavailable
                }
                _ => EntitlementError::TokenInvalid,
            })?;
        let principal = PrincipalId::try_new(identity.subject_id)
            .map_err(|_| EntitlementError::TokenInvalid)?;
        let resource = ResourceId::try_new(format!("stream:{}", product.to_ascii_lowercase()))
            .map_err(|_| EntitlementError::NoMatchingGrant)?;
        let request = AuthorizationRequest::try_new(
            principal.clone(),
            resource,
            AuthorizationAction::Stream,
            format!("entitlement:{product}"),
        )
        .map_err(|_| EntitlementError::NoMatchingGrant)?;
        let decision = self.evaluator.evaluate(&request);
        match decision.outcome() {
            AuthorizationOutcome::Allowed => Ok(principal.as_str().to_string()),
            AuthorizationOutcome::AdditionalAssuranceRequired => {
                Err(EntitlementError::AdditionalAssuranceRequired)
            }
            AuthorizationOutcome::Denied => match decision.reason() {
                AuthorizationDecisionReason::PolicyUnavailable => {
                    Err(EntitlementError::PolicyUnavailable)
                }
                _ => Err(EntitlementError::NoMatchingGrant),
            },
        }
    }
}
