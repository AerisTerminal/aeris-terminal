//! Provider-neutral authentication contracts, login schemas, agreement hooks, and errors.

use core::fmt;
use std::sync::{Arc, atomic::AtomicBool};

/// What a login field asks for; surfaces choose its control and wording from it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LoginFieldKind {
    /// One of the provider's published login servers.
    Server,
    /// A system discovered on the chosen server.
    System,
    UserId,
    Password,
}

/// Static, non-secret login fields supplied by provider registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoginField {
    pub id: &'static str,
    pub kind: LoginFieldKind,
    pub secret: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoginSchema {
    pub fields: &'static [LoginField],
    pub discover_servers: bool,
}

/// A submitted value may contain a password; never derive Debug.
pub struct LoginFieldValue<'a> {
    pub id: &'static str,
    pub value: &'a str,
}

/// Non-secret saved login details safe to display.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedProviderLogin {
    pub server_id: String,
    pub server_label: String,
    pub system_name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderLoginServer {
    pub id: &'static str,
    pub label: &'static str,
}

/// One pending exchange or provider agreement that must be accepted before live data
/// or order routing is enabled (for example CME Market Data Agreements on managed feeds).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingAgreement {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub url: Option<String>,
    pub mandatory: bool,
}

/// Typed errors produced by provider authentication and credential storage adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderAuthError {
    UnsupportedProvider,
    NoSavedCredentials,
    Unavailable(&'static str),
    InvalidFields(&'static str),
    UnknownServer,
    InProgress(&'static str),
    VaultUnavailable(&'static str),
    AgreementRequired(Vec<PendingAgreement>),
    Rejected(String),
}

impl fmt::Display for ProviderAuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedProvider => formatter.write_str("Unsupported provider"),
            Self::NoSavedCredentials => {
                formatter.write_str("This provider does not use saved credentials")
            }
            Self::Unavailable(detail)
            | Self::InvalidFields(detail)
            | Self::InProgress(detail)
            | Self::VaultUnavailable(detail) => formatter.write_str(detail),
            Self::UnknownServer => formatter.write_str("Unknown provider server"),
            Self::AgreementRequired(agreements) => {
                if agreements.is_empty() {
                    formatter.write_str("Provider agreement acceptance is required")
                } else {
                    write!(
                        formatter,
                        "Provider agreement acceptance is required ({} pending)",
                        agreements.len()
                    )
                }
            }
            Self::Rejected(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for ProviderAuthError {}

impl From<ProviderAuthError> for String {
    fn from(error: ProviderAuthError) -> Self {
        error.to_string()
    }
}

/// Provider-owned authentication, credential lifecycle, and agreement acceptance seam.
pub trait ProviderAuthAdapter: Send + Sync {
    /// Non-secret connection servers published by the provider.
    fn servers(&self) -> Vec<ProviderLoginServer>;

    /// Discovers selectable systems or environments at one server without credentials.
    ///
    /// # Errors
    /// Returns [`ProviderAuthError`] for unknown servers, concurrent discovery, or transport failure.
    fn discover_servers(
        &self,
        server_id: &str,
        stop: Arc<AtomicBool>,
    ) -> Result<Vec<String>, ProviderAuthError>;

    /// Loads non-secret metadata for the saved login, if any.
    ///
    /// # Errors
    /// Returns [`ProviderAuthError`] when credential storage cannot be read or decoded.
    fn saved_login(&self) -> Result<Option<SavedProviderLogin>, ProviderAuthError>;

    /// Verifies submitted credentials with the provider and persists only an accepted login.
    ///
    /// # Errors
    /// Returns [`ProviderAuthError`] for invalid fields, provider refusal, pending agreements, or vault errors.
    fn sign_in(
        &self,
        fields: &[LoginFieldValue<'_>],
        stop: Arc<AtomicBool>,
    ) -> Result<(), ProviderAuthError>;

    /// Deletes the saved login and notifies active workers so retired sessions close.
    ///
    /// # Errors
    /// Returns [`ProviderAuthError`] when credential storage cannot delete the entry.
    fn forget_login(&self) -> Result<(), ProviderAuthError>;

    /// Lists agreements that must be accepted before live data or execution is permitted.
    ///
    /// # Errors
    /// Returns [`ProviderAuthError`] when pending agreements cannot be queried.
    fn pending_agreements(
        &self,
        _stop: Arc<AtomicBool>,
    ) -> Result<Vec<PendingAgreement>, ProviderAuthError> {
        Ok(Vec::new())
    }

    /// Records the user's explicit acceptance of one pending agreement.
    ///
    /// # Errors
    /// Returns [`ProviderAuthError`] when the provider does not support agreement acceptance
    /// or refuses the submission.
    fn accept_agreement(
        &self,
        _agreement_id: &str,
        _stop: Arc<AtomicBool>,
    ) -> Result<(), ProviderAuthError> {
        Err(ProviderAuthError::Unavailable(
            "Provider does not expose in-app agreement acceptance",
        ))
    }
}
