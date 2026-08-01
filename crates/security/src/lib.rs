//! Authentication verification boundaries and strict offline JWT verification.
//!
//! Network discovery and key refresh are adapter concerns. This crate accepts
//! an immutable, revisioned JWKS snapshot and never trusts token-provided keys.

mod errors;
mod identity;
mod key_set;
#[cfg(test)]
mod test_fixture;
mod verifier;

pub use errors::{JwtVerificationError, JwtVerifierConfigurationError, KeySetSnapshotError};
pub use identity::{
    JwtVerificationRequest, JwtVerifier, SystemVerificationClock, TokenPurpose, VerificationClock,
    VerifiedIdentity,
};
pub use key_set::{Ed25519KeySetSnapshot, MAX_JWKS_JSON_BYTES, MAX_JWKS_KEYS};
pub use verifier::{Ed25519JwtVerifier, MAX_CLOCK_SKEW_SECONDS, MAX_ENCODED_JWT_BYTES};
