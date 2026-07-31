//! Server-owned, deterministic authorization policy and decision evidence.
//!
//! This domain models exact grants only. Persistence, policy distribution,
//! authentication, step-up assurance, and service transport remain adapters or
//! later control-flow stages.

use core::{fmt, num::NonZeroU64};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
};

/// Maximum bytes accepted in one opaque authorization identifier.
pub const MAX_AUTHORIZATION_IDENTIFIER_BYTES: usize = 256;
/// Maximum bytes accepted in one propagated correlation identifier.
pub const MAX_CORRELATION_ID_BYTES: usize = 256;
/// Maximum exact grants accepted in one policy snapshot.
pub const MAX_POLICY_GRANTS: usize = 100_000;

/// An authenticated principal requesting an operation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PrincipalId(String);

impl PrincipalId {
    /// Creates a principal ID from an immutable authentication subject.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is empty, oversized, or contains bytes
    /// outside the visible ASCII range.
    pub fn try_new(value: impl Into<String>) -> Result<Self, AuthorizationValidationError> {
        validate_identifier("principal_id", value.into()).map(Self)
    }

    /// Returns the exact opaque principal value without case folding.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A platform resource protected by policy.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceId(String);

impl ResourceId {
    /// Creates an opaque resource ID.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is empty, oversized, or contains bytes
    /// outside the visible ASCII range.
    pub fn try_new(value: impl Into<String>) -> Result<Self, AuthorizationValidationError> {
        validate_identifier("resource_id", value.into()).map(Self)
    }

    /// Returns the exact opaque resource value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identity for one policy grant and its audit evidence.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AuthorizationGrantId(String);

impl AuthorizationGrantId {
    /// Creates an opaque grant ID.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is empty, oversized, or contains bytes
    /// outside the visible ASCII range.
    pub fn try_new(value: impl Into<String>) -> Result<Self, AuthorizationValidationError> {
        validate_identifier("grant_id", value.into()).map(Self)
    }

    /// Returns the exact opaque grant value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Nonzero immutable policy revision evaluated for one decision.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PolicyVersion(NonZeroU64);

impl PolicyVersion {
    /// Creates a policy version that cannot use zero as an unavailable sentinel.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is zero.
    pub fn try_new(value: u64) -> Result<Self, AuthorizationValidationError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(AuthorizationValidationError::ZeroPolicyVersion)
    }

    /// Returns the serialized policy version.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Axiusflow-owned operations evaluated by the authorization service.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AuthorizationAction {
    Read,
    Stream,
    Trade,
    Administer,
}

/// Input to a server-side authorization decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationRequest {
    principal_id: PrincipalId,
    resource_id: ResourceId,
    action: AuthorizationAction,
    correlation_id: String,
}

impl AuthorizationRequest {
    /// Creates an auditable authorization request from trusted identity context.
    ///
    /// # Errors
    ///
    /// Returns an error when the correlation ID is empty, oversized, or
    /// contains bytes outside the visible ASCII range.
    pub fn try_new(
        principal_id: PrincipalId,
        resource_id: ResourceId,
        action: AuthorizationAction,
        correlation_id: impl Into<String>,
    ) -> Result<Self, AuthorizationValidationError> {
        let correlation_id = validate_correlation_id(correlation_id.into())?;
        Ok(Self {
            principal_id,
            resource_id,
            action,
            correlation_id,
        })
    }

    /// Returns the exact authenticated principal.
    #[must_use]
    pub const fn principal_id(&self) -> &PrincipalId {
        &self.principal_id
    }

    /// Returns the exact protected resource.
    #[must_use]
    pub const fn resource_id(&self) -> &ResourceId {
        &self.resource_id
    }

    /// Returns the requested Axiusflow operation.
    #[must_use]
    pub const fn action(&self) -> AuthorizationAction {
        self.action
    }

    /// Returns audit correlation context, which is not a policy input.
    #[must_use]
    pub fn correlation_id(&self) -> &str {
        &self.correlation_id
    }
}

/// One exact allow grant in a versioned policy snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationGrant {
    grant_id: AuthorizationGrantId,
    principal_id: PrincipalId,
    resource_id: ResourceId,
    action: AuthorizationAction,
}

impl AuthorizationGrant {
    /// Creates one exact principal/resource/action grant.
    #[must_use]
    pub const fn new(
        grant_id: AuthorizationGrantId,
        principal_id: PrincipalId,
        resource_id: ResourceId,
        action: AuthorizationAction,
    ) -> Self {
        Self {
            grant_id,
            principal_id,
            resource_id,
            action,
        }
    }

    /// Returns the stable evidence ID for this grant.
    #[must_use]
    pub const fn grant_id(&self) -> &AuthorizationGrantId {
        &self.grant_id
    }

    /// Returns the exact principal covered by this grant.
    #[must_use]
    pub const fn principal_id(&self) -> &PrincipalId {
        &self.principal_id
    }

    /// Returns the exact resource covered by this grant.
    #[must_use]
    pub const fn resource_id(&self) -> &ResourceId {
        &self.resource_id
    }

    /// Returns the exact action covered by this grant.
    #[must_use]
    pub const fn action(&self) -> AuthorizationAction {
        self.action
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AuthorizationGrantKey {
    principal_id: PrincipalId,
    resource_id: ResourceId,
    action: AuthorizationAction,
}

impl AuthorizationGrantKey {
    fn from_grant(grant: &AuthorizationGrant) -> Self {
        Self {
            principal_id: grant.principal_id.clone(),
            resource_id: grant.resource_id.clone(),
            action: grant.action,
        }
    }

    fn from_request(request: &AuthorizationRequest) -> Self {
        Self {
            principal_id: request.principal_id.clone(),
            resource_id: request.resource_id.clone(),
            action: request.action,
        }
    }
}

/// Immutable exact-grant policy state loaded and versioned by a service adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationPolicySnapshot {
    version: PolicyVersion,
    grants: BTreeMap<AuthorizationGrantKey, AuthorizationGrantId>,
}

impl AuthorizationPolicySnapshot {
    /// Validates and canonicalizes one bounded immutable grant snapshot.
    ///
    /// Empty snapshots are valid versioned deny-all policies. Grant order never
    /// affects the canonical map or evaluation result.
    ///
    /// # Errors
    ///
    /// Returns an error when the grant limit is exceeded, a grant ID is reused,
    /// or more than one grant targets the same principal/resource/action key.
    pub fn try_new(
        version: PolicyVersion,
        grants: Vec<AuthorizationGrant>,
    ) -> Result<Self, AuthorizationValidationError> {
        if grants.len() > MAX_POLICY_GRANTS {
            return Err(AuthorizationValidationError::GrantLimitExceeded {
                requested: grants.len(),
                maximum: MAX_POLICY_GRANTS,
            });
        }

        let mut grant_ids = BTreeSet::new();
        let mut canonical_grants = BTreeMap::new();
        let mut duplicate_grant_id = false;
        let mut duplicate_grant_key = false;
        for grant in grants {
            duplicate_grant_id |= !grant_ids.insert(grant.grant_id.clone());
            let key = AuthorizationGrantKey::from_grant(&grant);
            duplicate_grant_key |= canonical_grants.insert(key, grant.grant_id).is_some();
        }
        if duplicate_grant_id {
            return Err(AuthorizationValidationError::DuplicateGrantId);
        }
        if duplicate_grant_key {
            return Err(AuthorizationValidationError::DuplicateGrantKey);
        }
        Ok(Self {
            version,
            grants: canonical_grants,
        })
    }

    /// Returns the nonzero policy revision.
    #[must_use]
    pub const fn version(&self) -> PolicyVersion {
        self.version
    }

    /// Returns the number of canonical exact grants.
    #[must_use]
    pub fn grant_count(&self) -> usize {
        self.grants.len()
    }

    /// Returns whether the loaded policy denies all requests.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }

    fn matching_grant(&self, request: &AuthorizationRequest) -> Option<&AuthorizationGrantId> {
        self.grants
            .get(&AuthorizationGrantKey::from_request(request))
    }
}

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

    fn allowed(policy_version: PolicyVersion, grant_id: AuthorizationGrantId) -> Self {
        Self {
            outcome: AuthorizationOutcome::Allowed,
            policy_version: Some(policy_version),
            reason: AuthorizationDecisionReason::MatchingGrant { grant_id },
        }
    }

    fn additional_assurance_required(
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

    const fn no_matching_grant(policy_version: PolicyVersion) -> Self {
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

fn validate_identifier(
    field: &'static str,
    value: String,
) -> Result<String, AuthorizationValidationError> {
    if value.is_empty() {
        return Err(AuthorizationValidationError::EmptyIdentifier(field));
    }
    if value.len() > MAX_AUTHORIZATION_IDENTIFIER_BYTES {
        return Err(AuthorizationValidationError::IdentifierTooLong {
            field,
            requested: value.len(),
            maximum: MAX_AUTHORIZATION_IDENTIFIER_BYTES,
        });
    }
    if !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(AuthorizationValidationError::InvalidIdentifier(field));
    }
    Ok(value)
}

fn validate_correlation_id(value: String) -> Result<String, AuthorizationValidationError> {
    if value.is_empty() {
        return Err(AuthorizationValidationError::EmptyCorrelationId);
    }
    if value.len() > MAX_CORRELATION_ID_BYTES {
        return Err(AuthorizationValidationError::CorrelationIdTooLong {
            requested: value.len(),
            maximum: MAX_CORRELATION_ID_BYTES,
        });
    }
    if !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(AuthorizationValidationError::InvalidCorrelationId);
    }
    Ok(value)
}

/// Validation failures rejected before policy evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorizationValidationError {
    EmptyIdentifier(&'static str),
    InvalidIdentifier(&'static str),
    IdentifierTooLong {
        field: &'static str,
        requested: usize,
        maximum: usize,
    },
    EmptyCorrelationId,
    InvalidCorrelationId,
    CorrelationIdTooLong {
        requested: usize,
        maximum: usize,
    },
    ZeroPolicyVersion,
    GrantLimitExceeded {
        requested: usize,
        maximum: usize,
    },
    DuplicateGrantId,
    DuplicateGrantKey,
}

impl fmt::Display for AuthorizationValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyIdentifier(field) => write!(formatter, "{field} must not be empty"),
            Self::InvalidIdentifier(field) => {
                write!(
                    formatter,
                    "{field} contains bytes outside the visible ASCII range"
                )
            }
            Self::IdentifierTooLong {
                field,
                requested,
                maximum,
            } => write!(
                formatter,
                "{field} contains {requested} bytes; maximum is {maximum}"
            ),
            Self::EmptyCorrelationId => formatter.write_str("correlation_id must not be empty"),
            Self::InvalidCorrelationId => {
                formatter.write_str("correlation_id contains bytes outside the visible ASCII range")
            }
            Self::CorrelationIdTooLong { requested, maximum } => write!(
                formatter,
                "correlation_id contains {requested} bytes; maximum is {maximum}"
            ),
            Self::ZeroPolicyVersion => formatter.write_str("policy version must be non-zero"),
            Self::GrantLimitExceeded { requested, maximum } => write!(
                formatter,
                "policy snapshot contains {requested} grants; maximum is {maximum}"
            ),
            Self::DuplicateGrantId => {
                formatter.write_str("policy snapshot contains a duplicate grant_id")
            }
            Self::DuplicateGrantKey => formatter
                .write_str("policy snapshot contains duplicate principal/resource/action grants"),
        }
    }
}

impl Error for AuthorizationValidationError {}
