//! Shared deterministic authorization values for this crate's unit tests.

use crate::identifiers::{AuthorizationGrantId, PolicyVersion, PrincipalId, ResourceId};
use crate::policy::{AuthorizationGrant, AuthorizationPolicySnapshot};
use crate::request::{AuthorizationAction, AuthorizationRequest};

pub(crate) fn principal(value: &str) -> PrincipalId {
    PrincipalId::try_new(value).expect("test principal is valid")
}

pub(crate) fn resource(value: &str) -> ResourceId {
    ResourceId::try_new(value).expect("test resource is valid")
}

pub(crate) fn grant_id(value: &str) -> AuthorizationGrantId {
    AuthorizationGrantId::try_new(value).expect("test grant ID is valid")
}

pub(crate) fn request(action: AuthorizationAction) -> AuthorizationRequest {
    AuthorizationRequest::try_new(
        principal("principal-1"),
        resource("resource-1"),
        action,
        "correlation-1",
    )
    .expect("test request is valid")
}

pub(crate) fn snapshot(action: AuthorizationAction) -> AuthorizationPolicySnapshot {
    AuthorizationPolicySnapshot::try_new(
        PolicyVersion::try_new(7).expect("policy version is non-zero"),
        vec![AuthorizationGrant::new(
            grant_id("grant-1"),
            principal("principal-1"),
            resource("resource-1"),
            action,
        )],
    )
    .expect("test snapshot is valid")
}
