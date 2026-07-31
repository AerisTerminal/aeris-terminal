//! Authentication verification boundaries and strict offline JWT verification.
//!
//! Network discovery and key refresh are adapter concerns. This crate accepts
//! an immutable, revisioned JWKS snapshot and never trusts token-provided keys.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use core::fmt;
use ed25519_dalek::{Signature, VerifyingKey};
use jsonwebtoken::{Algorithm, Header, decode_header};
use serde::Deserialize;
use std::{
    collections::HashMap,
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

/// Maximum accepted serialized JWKS size.
pub const MAX_JWKS_JSON_BYTES: usize = 64 * 1_024;
/// Maximum verification keys accepted in one immutable snapshot.
pub const MAX_JWKS_KEYS: usize = 64;
/// Maximum accepted compact JWT size.
pub const MAX_ENCODED_JWT_BYTES: usize = 16 * 1_024;
/// Maximum configured clock skew accepted by the verifier.
pub const MAX_CLOCK_SKEW_SECONDS: u64 = 300;

const ED25519_PUBLIC_KEY_BYTES: usize = 32;
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

/// Immutable Ed25519 verification-key snapshot loaded by a service adapter.
pub struct Ed25519KeySetSnapshot {
    revision: u64,
    keys: HashMap<String, VerifyingKey>,
}

impl Ed25519KeySetSnapshot {
    /// Parses a bounded JWKS document into an immutable verification snapshot.
    ///
    /// Only unique, non-empty `kid` values with `kty=OKP`, `crv=Ed25519`,
    /// `alg=EdDSA`, verification-compatible key metadata, and 32-byte public
    /// key material are accepted.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero revision, oversized or malformed document,
    /// empty or oversized key set, duplicate key ID, invalid metadata, or
    /// invalid Ed25519 public-key material.
    pub fn try_from_jwks_json(revision: u64, jwks_json: &str) -> Result<Self, KeySetSnapshotError> {
        if revision == 0 {
            return Err(KeySetSnapshotError::ZeroRevision);
        }
        if jwks_json.len() > MAX_JWKS_JSON_BYTES {
            return Err(KeySetSnapshotError::DocumentTooLarge);
        }
        let document: RawJwks =
            serde_json::from_str(jwks_json).map_err(|_| KeySetSnapshotError::InvalidDocument)?;
        if document.keys.is_empty() {
            return Err(KeySetSnapshotError::EmptyKeySet);
        }
        if document.keys.len() > MAX_JWKS_KEYS {
            return Err(KeySetSnapshotError::TooManyKeys);
        }

        let mut keys = HashMap::with_capacity(document.keys.len());
        for raw_key in document.keys {
            let (key_id, decoding_key) = parse_ed25519_key(raw_key)?;
            if keys.insert(key_id, decoding_key).is_some() {
                return Err(KeySetSnapshotError::DuplicateKeyId);
            }
        }
        Ok(Self { revision, keys })
    }

    /// Returns the service-owned key-set revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the number of active and grace-period verification keys.
    #[must_use]
    pub fn key_count(&self) -> usize {
        self.keys.len()
    }

    /// Returns whether this snapshot contains a key ID.
    #[must_use]
    pub fn contains_key_id(&self, key_id: &str) -> bool {
        self.keys.contains_key(key_id)
    }

    fn key(&self, key_id: &str) -> Option<&VerifyingKey> {
        self.keys.get(key_id)
    }
}

impl fmt::Debug for Ed25519KeySetSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Ed25519KeySetSnapshot")
            .field("revision", &self.revision)
            .field("key_count", &self.keys.len())
            .finish_non_exhaustive()
    }
}

/// Strict Ed25519 service-token verifier over one immutable key snapshot.
pub struct Ed25519JwtVerifier<Clock> {
    key_set: Ed25519KeySetSnapshot,
    clock: Clock,
    clock_skew_seconds: u64,
}

impl<Clock> Ed25519JwtVerifier<Clock> {
    /// Creates a verifier with an injected clock and bounded skew policy.
    ///
    /// # Errors
    ///
    /// Returns an error when clock skew exceeds [`MAX_CLOCK_SKEW_SECONDS`].
    pub fn try_new(
        key_set: Ed25519KeySetSnapshot,
        clock: Clock,
        clock_skew_seconds: u64,
    ) -> Result<Self, JwtVerifierConfigurationError> {
        if clock_skew_seconds > MAX_CLOCK_SKEW_SECONDS {
            return Err(JwtVerifierConfigurationError::ClockSkewTooLarge);
        }
        Ok(Self {
            key_set,
            clock,
            clock_skew_seconds,
        })
    }

    /// Returns the immutable key-set revision used for all decisions.
    #[must_use]
    pub const fn key_set_revision(&self) -> u64 {
        self.key_set.revision()
    }

    /// Returns the configured temporal clock skew in seconds.
    #[must_use]
    pub const fn clock_skew_seconds(&self) -> u64 {
        self.clock_skew_seconds
    }

    /// Returns the immutable key snapshot for diagnostics and atomic replacement.
    #[must_use]
    pub const fn key_set(&self) -> &Ed25519KeySetSnapshot {
        &self.key_set
    }

    /// Consumes the verifier and returns its service-owned composition parts.
    #[must_use]
    pub fn into_parts(self) -> (Ed25519KeySetSnapshot, Clock, u64) {
        (self.key_set, self.clock, self.clock_skew_seconds)
    }
}

impl<Clock: VerificationClock> JwtVerifier for Ed25519JwtVerifier<Clock> {
    fn verify(
        &self,
        request: JwtVerificationRequest<'_>,
    ) -> Result<VerifiedIdentity, JwtVerificationError> {
        validate_request(&request)?;
        if request.encoded_token.len() > MAX_ENCODED_JWT_BYTES {
            return Err(JwtVerificationError::InvalidToken);
        }

        let header =
            decode_header(request.encoded_token).map_err(|_| JwtVerificationError::InvalidToken)?;
        validate_header(&header)?;
        let key_id = header
            .kid
            .as_deref()
            .filter(|key_id| valid_identifier(key_id))
            .ok_or(JwtVerificationError::KeyUnavailable)?;
        let verifying_key = self
            .key_set
            .key(key_id)
            .ok_or(JwtVerificationError::KeyUnavailable)?;
        let claims = decode_strictly_verified_claims(request.encoded_token, verifying_key)?;

        let now = self
            .clock
            .now_unix_seconds()
            .ok_or(JwtVerificationError::ClockUnavailable)?;
        validate_claims(&claims, &request, now, self.clock_skew_seconds)?;

        Ok(VerifiedIdentity {
            subject_id: claims.sub,
            session_id: claims.session_id,
            device_id: claims.device_id,
        })
    }
}

fn validate_request(request: &JwtVerificationRequest<'_>) -> Result<(), JwtVerificationError> {
    if request.encoded_token.trim().is_empty() {
        return Err(JwtVerificationError::InvalidToken);
    }
    if !valid_identifier(request.expected_issuer) {
        return Err(JwtVerificationError::InvalidIssuer);
    }
    if !valid_identifier(request.expected_audience) {
        return Err(JwtVerificationError::InvalidAudience);
    }
    Ok(())
}

fn validate_header(header: &Header) -> Result<(), JwtVerificationError> {
    if header.alg != Algorithm::EdDSA || header.typ.as_deref() != Some("JWT") {
        return Err(JwtVerificationError::InvalidToken);
    }
    if header.cty.is_some()
        || header.jku.is_some()
        || header.jwk.is_some()
        || header.x5u.is_some()
        || header.x5c.is_some()
        || header.x5t.is_some()
        || header.x5t_s256.is_some()
        || header.crit.is_some()
        || header.enc.is_some()
        || header.zip.is_some()
        || header.url.is_some()
        || header.nonce.is_some()
        || !header.extras.inner().is_empty()
    {
        return Err(JwtVerificationError::InvalidToken);
    }
    Ok(())
}

fn decode_strictly_verified_claims(
    encoded_token: &str,
    verifying_key: &VerifyingKey,
) -> Result<ServiceAccessClaims, JwtVerificationError> {
    let mut segments = encoded_token.split('.');
    let encoded_header = segments.next().ok_or(JwtVerificationError::InvalidToken)?;
    let encoded_claims = segments.next().ok_or(JwtVerificationError::InvalidToken)?;
    let encoded_signature = segments.next().ok_or(JwtVerificationError::InvalidToken)?;
    if segments.next().is_some()
        || encoded_header.is_empty()
        || encoded_claims.is_empty()
        || encoded_signature.is_empty()
    {
        return Err(JwtVerificationError::InvalidToken);
    }

    let signature_bytes = URL_SAFE_NO_PAD
        .decode(encoded_signature)
        .map_err(|_| JwtVerificationError::InvalidToken)?;
    let signature =
        Signature::from_slice(&signature_bytes).map_err(|_| JwtVerificationError::InvalidToken)?;
    let signing_input = format!("{encoded_header}.{encoded_claims}");
    verifying_key
        .verify_strict(signing_input.as_bytes(), &signature)
        .map_err(|_| JwtVerificationError::InvalidToken)?;

    let claims_json = URL_SAFE_NO_PAD
        .decode(encoded_claims)
        .map_err(|_| JwtVerificationError::InvalidToken)?;
    serde_json::from_slice(&claims_json).map_err(|_| JwtVerificationError::InvalidToken)
}

fn validate_claims(
    claims: &ServiceAccessClaims,
    request: &JwtVerificationRequest<'_>,
    now: u64,
    clock_skew_seconds: u64,
) -> Result<(), JwtVerificationError> {
    if claims.iss != request.expected_issuer {
        return Err(JwtVerificationError::InvalidIssuer);
    }
    if !claims.aud.contains(request.expected_audience) {
        return Err(JwtVerificationError::InvalidAudience);
    }
    if claims.purpose != request.purpose.claim_value() {
        return Err(JwtVerificationError::InvalidPurpose);
    }
    if !valid_identifier(&claims.sub)
        || claims
            .session_id
            .as_deref()
            .is_some_and(|value| !valid_identifier(value))
        || claims
            .device_id
            .as_deref()
            .is_some_and(|value| !valid_identifier(value))
    {
        return Err(JwtVerificationError::InvalidToken);
    }

    let expiration_boundary = now.saturating_sub(clock_skew_seconds);
    if claims.exp <= expiration_boundary {
        return Err(JwtVerificationError::Expired);
    }
    let not_before_boundary = now.saturating_add(clock_skew_seconds);
    if claims
        .nbf
        .is_some_and(|not_before| not_before > not_before_boundary)
    {
        return Err(JwtVerificationError::NotYetValid);
    }
    Ok(())
}

fn parse_ed25519_key(raw_key: RawJwk) -> Result<(String, VerifyingKey), KeySetSnapshotError> {
    if raw_key.kty.as_deref() != Some("OKP")
        || raw_key.crv.as_deref() != Some("Ed25519")
        || raw_key.alg.as_deref() != Some("EdDSA")
        || raw_key
            .key_use
            .as_deref()
            .is_some_and(|key_use| key_use != "sig")
        || raw_key.key_ops.as_ref().is_some_and(|operations| {
            operations.is_empty() || operations.iter().any(|operation| operation != "verify")
        })
    {
        return Err(KeySetSnapshotError::InvalidKeyMetadata);
    }

    let key_id = raw_key
        .kid
        .filter(|key_id| valid_identifier(key_id))
        .ok_or(KeySetSnapshotError::InvalidKeyMetadata)?;
    let encoded_x = raw_key
        .x
        .filter(|encoded_x| !encoded_x.is_empty() && encoded_x.len() <= 128)
        .ok_or(KeySetSnapshotError::InvalidKeyMaterial)?;
    let decoded_key = URL_SAFE_NO_PAD
        .decode(encoded_x)
        .map_err(|_| KeySetSnapshotError::InvalidKeyMaterial)?;
    let key_bytes: [u8; ED25519_PUBLIC_KEY_BYTES] = decoded_key
        .try_into()
        .map_err(|_| KeySetSnapshotError::InvalidKeyMaterial)?;
    let verifying_key = VerifyingKey::from_bytes(&key_bytes)
        .map_err(|_| KeySetSnapshotError::InvalidKeyMaterial)?;
    if verifying_key.is_weak() {
        return Err(KeySetSnapshotError::InvalidKeyMaterial);
    }
    Ok((key_id, verifying_key))
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTITY_CLAIM_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

#[derive(Debug, Deserialize)]
struct RawJwks {
    keys: Vec<RawJwk>,
}

#[derive(Debug, Deserialize)]
struct RawJwk {
    kty: Option<String>,
    crv: Option<String>,
    alg: Option<String>,
    kid: Option<String>,
    x: Option<String>,
    #[serde(rename = "use")]
    key_use: Option<String>,
    key_ops: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ServiceAccessClaims {
    iss: String,
    aud: AudienceClaim,
    sub: String,
    exp: u64,
    #[serde(default)]
    nbf: Option<u64>,
    purpose: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AudienceClaim {
    Single(String),
    Multiple(Vec<String>),
}

impl AudienceClaim {
    fn contains(&self, expected: &str) -> bool {
        match self {
            Self::Single(actual) => actual == expected,
            Self::Multiple(actual) => actual.iter().any(|audience| audience == expected),
        }
    }
}

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
