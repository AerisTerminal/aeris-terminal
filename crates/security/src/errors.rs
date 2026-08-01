//! Fail-closed verification and snapshot rejection reasons.

use core::fmt;
use std::error::Error;

/// Failures while constructing an immutable Ed25519 key snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeySetSnapshotError {
    ZeroRevision,
    DocumentTooLarge,
    InvalidDocument,
    EmptyKeySet,
    TooManyKeys,
    InvalidKeyMetadata,
    InvalidKeyMaterial,
    DuplicateKeyId,
}

impl fmt::Display for KeySetSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Ed25519 key-set snapshot rejected: {self:?}")
    }
}

impl Error for KeySetSnapshotError {}

/// Invalid verifier configuration rejected before any token is accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JwtVerifierConfigurationError {
    ClockSkewTooLarge,
}

impl fmt::Display for JwtVerifierConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "JWT verifier configuration rejected: {self:?}")
    }
}

impl Error for JwtVerifierConfigurationError {}

/// Fail-closed verification errors exposed to service adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JwtVerificationError {
    InvalidToken,
    InvalidIssuer,
    InvalidAudience,
    InvalidPurpose,
    KeyUnavailable,
    Expired,
    NotYetValid,
    ClockUnavailable,
}

impl fmt::Display for JwtVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "JWT verification failed: {self:?}")
    }
}

impl Error for JwtVerificationError {}
