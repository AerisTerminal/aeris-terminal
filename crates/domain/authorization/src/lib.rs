//! Server-owned, deterministic authorization policy and decision evidence.
//!
//! This domain models exact grants only. Persistence, policy distribution,
//! authentication, step-up assurance, and service transport remain adapters or
//! later control-flow stages.

mod decision;
mod errors;
mod evaluator;
mod identifiers;
mod policy;
mod request;
#[cfg(test)]
mod test_fixture;

pub use decision::{AuthorizationDecision, AuthorizationDecisionReason, AuthorizationOutcome};
pub use errors::AuthorizationValidationError;
pub use evaluator::AuthorizationEvaluator;
pub use identifiers::{
    AuthorizationGrantId, MAX_AUTHORIZATION_IDENTIFIER_BYTES, MAX_CORRELATION_ID_BYTES,
    PolicyVersion, PrincipalId, ResourceId,
};
pub use policy::{AuthorizationGrant, AuthorizationPolicySnapshot, MAX_POLICY_GRANTS};
pub use request::{AuthorizationAction, AuthorizationRequest};
