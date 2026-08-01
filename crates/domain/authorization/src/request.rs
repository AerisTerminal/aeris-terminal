//! Auditable authorization request input.

use crate::errors::AuthorizationValidationError;
use crate::identifiers::{PrincipalId, ResourceId, validate_correlation_id};

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
    pub(crate) principal_id: PrincipalId,
    pub(crate) resource_id: ResourceId,
    pub(crate) action: AuthorizationAction,
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
