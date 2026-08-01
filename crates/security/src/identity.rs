//! Verified identity, token purpose, and injected verification clock.

use crate::errors::JwtVerificationError;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_IDENTITY_CLAIM_BYTES: usize = 256;
const SERVICE_ACCESS_PURPOSE: &str = "service_access";

/// The only accepted purpose of a service access token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenPurpose {
    ServiceAccess,
}

impl TokenPurpose {
    /// Returns the exact private claim value required for this purpose.
    #[must_use]
    pub const fn claim_value(self) -> &'static str {
        match self {
            Self::ServiceAccess => SERVICE_ACCESS_PURPOSE,
        }
    }
}

/// Inputs that must be checked against a Better Auth JWT and JWKS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JwtVerificationRequest<'a> {
    pub encoded_token: &'a str,
    pub expected_issuer: &'a str,
    pub expected_audience: &'a str,
    pub purpose: TokenPurpose,
}

/// Identity established only after complete token verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedIdentity {
    pub subject_id: String,
    pub session_id: Option<String>,
    pub device_id: Option<String>,
}

/// Implemented by pinned JWT/JWKS adapters, never by domain code.
pub trait JwtVerifier {
    /// Verifies every required JWT property and returns the established identity.
    ///
    /// # Errors
    ///
    /// Returns a fail-closed [`JwtVerificationError`] when any cryptographic,
    /// temporal, issuer, audience, purpose, or key requirement is not satisfied.
    fn verify(
        &self,
        request: JwtVerificationRequest<'_>,
    ) -> Result<VerifiedIdentity, JwtVerificationError>;
}

/// Supplies deterministic verification time without coupling JWT checks to I/O.
pub trait VerificationClock {
    /// Returns current Unix time in whole seconds, or `None` when unavailable.
    fn now_unix_seconds(&self) -> Option<u64>;
}

/// Production verification clock backed by the operating-system wall clock.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SystemVerificationClock;

impl VerificationClock for SystemVerificationClock {
    fn now_unix_seconds(&self) -> Option<u64> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_secs())
    }
}

pub(crate) fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTITY_CLAIM_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
