//! Opaque authorization identifiers and their validation bounds.

use crate::errors::AuthorizationValidationError;
use core::num::NonZeroU64;

/// Maximum bytes accepted in one opaque authorization identifier.
pub const MAX_AUTHORIZATION_IDENTIFIER_BYTES: usize = 256;
/// Maximum bytes accepted in one propagated correlation identifier.
pub const MAX_CORRELATION_ID_BYTES: usize = 256;

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

pub(crate) fn validate_correlation_id(
    value: String,
) -> Result<String, AuthorizationValidationError> {
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

#[cfg(test)]
mod tests {
    use super::{PolicyVersion, PrincipalId, ResourceId};
    use crate::errors::AuthorizationValidationError;
    use crate::request::{AuthorizationAction, AuthorizationRequest};
    use crate::test_fixture::{principal, resource};

    #[test]
    fn identifiers_and_policy_versions_fail_closed_at_boundaries() {
        assert!(matches!(
            PrincipalId::try_new(""),
            Err(AuthorizationValidationError::EmptyIdentifier(
                "principal_id"
            ))
        ));
        assert!(matches!(
            ResourceId::try_new("contains space"),
            Err(AuthorizationValidationError::InvalidIdentifier(
                "resource_id"
            ))
        ));
        assert!(matches!(
            PolicyVersion::try_new(0),
            Err(AuthorizationValidationError::ZeroPolicyVersion)
        ));
        assert!(matches!(
            AuthorizationRequest::try_new(
                principal("principal-1"),
                resource("resource-1"),
                AuthorizationAction::Read,
                "bad correlation",
            ),
            Err(AuthorizationValidationError::InvalidCorrelationId)
        ));
    }
}
