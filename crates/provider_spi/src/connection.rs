//! Provider connection identities and credential sources.
//!
//! Decouples a logical connection (`ConnectionId`) from its underlying provider adapter
//! (`provider_id`) so one adapter (for example Rithmic) can host multiple concurrent
//! connections — such as an Aeris-managed CME market-data feed alongside one or more
//! user-supplied prop-firm logins — while legacy single-login configurations migrate
//! transparently to [`DEFAULT_CONNECTION_ID`].

use core::fmt;
use serde::{Deserialize, Serialize};

/// Canonical connection ID assigned to migrated legacy single-login configurations.
pub const DEFAULT_CONNECTION_ID: &str = "default";

/// Maximum UTF-8 bytes allowed in one [`ConnectionId`].
pub const MAXIMUM_CONNECTION_ID_BYTES: usize = 64;

/// Validation error for a [`ConnectionId`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionIdError {
    Empty,
    TooLong { maximum: usize },
    InvalidCharacter,
}

impl fmt::Display for ConnectionIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("connection ID must not be empty"),
            Self::TooLong { maximum } => {
                write!(formatter, "connection ID exceeds {maximum} bytes")
            }
            Self::InvalidCharacter => {
                formatter.write_str("connection ID contains invalid characters")
            }
        }
    }
}

impl std::error::Error for ConnectionIdError {}

/// Stable logical identifier for one configured connection under a provider adapter.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectionId(String);

impl ConnectionId {
    /// Validates and constructs a [`ConnectionId`].
    ///
    /// Allowed characters are ASCII alphanumerics, `-`, `_`, and `.`.
    ///
    /// # Errors
    /// Returns [`ConnectionIdError`] when empty, oversized, or containing disallowed characters.
    pub fn try_new(value: impl Into<String>) -> Result<Self, ConnectionIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ConnectionIdError::Empty);
        }
        if value.len() > MAXIMUM_CONNECTION_ID_BYTES {
            return Err(ConnectionIdError::TooLong {
                maximum: MAXIMUM_CONNECTION_ID_BYTES,
            });
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(ConnectionIdError::InvalidCharacter);
        }
        Ok(Self(value))
    }

    /// Returns the canonical default connection ID used by migrated legacy workspaces.
    #[must_use]
    pub fn default_connection() -> Self {
        Self(DEFAULT_CONNECTION_ID.to_string())
    }

    /// Migrates an optional legacy connection ID, mapping `None` or blank values to
    /// [`DEFAULT_CONNECTION_ID`].
    ///
    /// # Errors
    /// Returns [`ConnectionIdError`] if a non-empty value fails validation.
    pub fn from_legacy_optional(raw: Option<&str>) -> Result<Self, ConnectionIdError> {
        match raw.map(str::trim) {
            None | Some("") => Ok(Self::default_connection()),
            Some(value) => Self::try_new(value),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ConnectionId {
    fn default() -> Self {
        Self::default_connection()
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Where a connection obtains its authentication material.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CredentialSource {
    /// Public feed requiring no credentials.
    Public,
    /// User-supplied credentials stored in the native OS credential vault.
    UserVault { service: String, key: String },
    /// Credentials provisioned and refreshed via an Aeris-managed subscription entitlement.
    ManagedAeris { entitlement_id: String },
}

/// Non-secret metadata describing one logical connection hosted by a provider adapter.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConnectionDescriptor {
    pub connection_id: ConnectionId,
    pub provider_id: String,
    pub display_label: String,
    pub credentials: CredentialSource,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_id_validates_bounds_and_migrates_legacy_entries() {
        assert_eq!(ConnectionId::default().as_str(), DEFAULT_CONNECTION_ID);
        assert_eq!(
            ConnectionId::from_legacy_optional(None),
            Ok(ConnectionId::default_connection())
        );
        assert_eq!(
            ConnectionId::from_legacy_optional(Some("  ")),
            Ok(ConnectionId::default_connection())
        );
        assert_eq!(
            ConnectionId::from_legacy_optional(Some("prop-firm_01.cme"))
                .expect("valid id")
                .as_str(),
            "prop-firm_01.cme"
        );
        assert_eq!(ConnectionId::try_new(""), Err(ConnectionIdError::Empty));
        assert_eq!(
            ConnectionId::try_new("a".repeat(MAXIMUM_CONNECTION_ID_BYTES + 1)),
            Err(ConnectionIdError::TooLong {
                maximum: MAXIMUM_CONNECTION_ID_BYTES
            })
        );
        assert_eq!(
            ConnectionId::try_new("bad/id"),
            Err(ConnectionIdError::InvalidCharacter)
        );
    }
}
