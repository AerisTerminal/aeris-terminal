//! Immutable exact-grant policy snapshots.

use crate::errors::AuthorizationValidationError;
use crate::identifiers::{AuthorizationGrantId, PolicyVersion, PrincipalId, ResourceId};
use crate::request::{AuthorizationAction, AuthorizationRequest};
use std::collections::{BTreeMap, BTreeSet};

/// Maximum exact grants accepted in one policy snapshot.
pub const MAX_POLICY_GRANTS: usize = 100_000;

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
    pub(crate) version: PolicyVersion,
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

    pub(crate) fn matching_grant(
        &self,
        request: &AuthorizationRequest,
    ) -> Option<&AuthorizationGrantId> {
        self.grants
            .get(&AuthorizationGrantKey::from_request(request))
    }
}

#[cfg(test)]
mod tests {
    use super::AuthorizationPolicySnapshot;
    use crate::errors::AuthorizationValidationError;
    use crate::identifiers::PolicyVersion;
    use crate::policy::AuthorizationGrant;
    use crate::request::AuthorizationAction;
    use crate::test_fixture::{grant_id, principal, resource};

    #[test]
    fn snapshot_rejects_duplicate_evidence_and_policy_keys() {
        let version = PolicyVersion::try_new(1).expect("policy version is non-zero");
        let first = AuthorizationGrant::new(
            grant_id("grant-1"),
            principal("principal-1"),
            resource("resource-1"),
            AuthorizationAction::Read,
        );
        let duplicate_id = AuthorizationGrant::new(
            grant_id("grant-1"),
            principal("principal-2"),
            resource("resource-2"),
            AuthorizationAction::Stream,
        );
        assert!(matches!(
            AuthorizationPolicySnapshot::try_new(version, vec![first.clone(), duplicate_id]),
            Err(AuthorizationValidationError::DuplicateGrantId)
        ));

        let duplicate_key = AuthorizationGrant::new(
            grant_id("grant-2"),
            principal("principal-1"),
            resource("resource-1"),
            AuthorizationAction::Read,
        );
        assert!(matches!(
            AuthorizationPolicySnapshot::try_new(version, vec![first, duplicate_key]),
            Err(AuthorizationValidationError::DuplicateGrantKey)
        ));
    }
}
