//! Versioned policy snapshot loading with monotonic replacement.

use axiusflow_authorization::{
    AuthorizationGrant, AuthorizationGrantId, AuthorizationPolicySnapshot, PolicyVersion,
    PrincipalId, ResourceId,
};
use serde::Deserialize;
use std::fs;

#[derive(Deserialize)]
struct PolicyDocument {
    version: u64,
    grants: Vec<GrantDocument>,
}

#[derive(Deserialize)]
struct GrantDocument {
    grant_id: String,
    principal_id: String,
    resource_id: String,
    action: String,
}

/// Loads and validates one policy snapshot from a JSON document.
///
/// # Errors
///
/// Returns an error for unreadable files, malformed documents, or invalid
/// snapshot content.
pub fn load_policy(path: &str) -> Result<AuthorizationPolicySnapshot, String> {
    let content =
        fs::read_to_string(path).map_err(|error| format!("cannot read policy {path}: {error}"))?;
    parse_policy(&content)
}

/// Parses one policy snapshot document.
///
/// # Errors
///
/// Returns an error for malformed documents or invalid snapshot content.
pub fn parse_policy(content: &str) -> Result<AuthorizationPolicySnapshot, String> {
    let document: PolicyDocument =
        serde_json::from_str(content).map_err(|error| format!("malformed policy: {error}"))?;
    let version = PolicyVersion::try_new(document.version)
        .map_err(|error| format!("invalid policy version: {error}"))?;
    let mut grants = Vec::with_capacity(document.grants.len());
    for grant in &document.grants {
        grants.push(AuthorizationGrant::new(
            AuthorizationGrantId::try_new(grant.grant_id.clone())
                .map_err(|error| format!("invalid grant id: {error}"))?,
            PrincipalId::try_new(grant.principal_id.clone())
                .map_err(|error| format!("invalid principal id: {error}"))?,
            ResourceId::try_new(grant.resource_id.clone())
                .map_err(|error| format!("invalid resource id: {error}"))?,
            parse_action(&grant.action)?,
        ));
    }
    AuthorizationPolicySnapshot::try_new(version, grants)
        .map_err(|error| format!("invalid policy snapshot: {error}"))
}

fn parse_action(value: &str) -> Result<axiusflow_authorization::AuthorizationAction, String> {
    match value {
        "read" => Ok(axiusflow_authorization::AuthorizationAction::Read),
        "stream" => Ok(axiusflow_authorization::AuthorizationAction::Stream),
        "trade" => Ok(axiusflow_authorization::AuthorizationAction::Trade),
        "administer" => Ok(axiusflow_authorization::AuthorizationAction::Administer),
        other => Err(format!("unknown authorization action: {other}")),
    }
}

/// Serializes the current policy snapshot for diagnostics.
#[cfg(test)]
mod tests {
    use super::parse_policy;

    #[test]
    fn policy_document_round_trips_into_a_snapshot() {
        let snapshot = parse_policy(
            r#"{"version": 2, "grants": [{"grant_id": "g1", "principal_id": "user-1", "resource_id": "bars:axf", "action": "stream"}]}"#,
        )
        .expect("valid policy parses");
        assert_eq!(snapshot.version().get(), 2);
    }

    #[test]
    fn invalid_documents_fail_closed() {
        assert!(parse_policy("not json").is_err());
        assert!(parse_policy(r#"{"version": 0, "grants": []}"#).is_err());
        assert!(
            parse_policy(
                r#"{"version": 1, "grants": [{"grant_id": "g1", "principal_id": "user-1", "resource_id": "bars:axf", "action": "fly"}]}"#
            )
            .is_err()
        );
    }
}
