//! Strict offline Ed25519 service-token verification.

use crate::errors::{JwtVerificationError, JwtVerifierConfigurationError};
use crate::identity::{
    JwtVerificationRequest, JwtVerifier, VerificationClock, VerifiedIdentity, valid_identifier,
};
use crate::key_set::Ed25519KeySetSnapshot;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};
use jsonwebtoken::{Algorithm, Header, decode_header};
use serde::Deserialize;

/// Maximum accepted compact JWT size.
pub const MAX_ENCODED_JWT_BYTES: usize = 16 * 1_024;
/// Maximum configured clock skew accepted by the verifier.
pub const MAX_CLOCK_SKEW_SECONDS: u64 = 300;

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

#[cfg(test)]
mod tests {
    use super::{Ed25519JwtVerifier, MAX_CLOCK_SKEW_SECONDS};
    use crate::errors::{JwtVerificationError, JwtVerifierConfigurationError};
    use crate::identity::JwtVerifier;
    use crate::key_set::Ed25519KeySetSnapshot;
    use crate::test_fixture::{FixedClock, claims, jwks, request, token, verifier};
    use serde_json::json;

    #[test]
    fn verifier_configuration_rejects_excessive_clock_skew() {
        let key_set = Ed25519KeySetSnapshot::try_from_jwks_json(1, &jwks("key-1"))
            .expect("test JWKS is valid");
        assert!(matches!(
            Ed25519JwtVerifier::try_new(
                key_set,
                FixedClock(Some(1_000)),
                MAX_CLOCK_SKEW_SECONDS + 1,
            ),
            Err(JwtVerifierConfigurationError::ClockSkewTooLarge)
        ));
    }

    #[test]
    fn signed_service_token_verifies_to_expected_identity() {
        let encoded = token("key-1", &claims(1_100));
        let identity = verifier(Some(1_000))
            .verify(request(&encoded))
            .expect("the signed service token verifies");
        assert_eq!(identity.subject_id, "principal-1");
        assert_eq!(identity.session_id.as_deref(), Some("session-1"));
        assert_eq!(identity.device_id.as_deref(), Some("device-1"));
    }

    #[test]
    fn verifier_rejects_unknown_key_tampering_and_bad_claims() {
        let unknown_key = token("unknown-key", &claims(1_100));
        assert_eq!(
            verifier(Some(1_000)).verify(request(&unknown_key)),
            Err(JwtVerificationError::KeyUnavailable)
        );

        let valid = token("key-1", &claims(1_100));
        let mut tampered = valid.into_bytes();
        let last = tampered.last_mut().expect("token is non-empty");
        *last = if *last == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).expect("token remains ASCII");
        assert_eq!(
            verifier(Some(1_000)).verify(request(&tampered)),
            Err(JwtVerificationError::InvalidToken)
        );

        let expired = token("key-1", &claims(995));
        assert_eq!(
            verifier(Some(1_000)).verify(request(&expired)),
            Err(JwtVerificationError::Expired)
        );

        let wrong_purpose_claims = {
            let mut value = claims(1_100);
            value["purpose"] = json!("refresh");
            value
        };
        let wrong_purpose = token("key-1", &wrong_purpose_claims);
        assert_eq!(
            verifier(Some(1_000)).verify(request(&wrong_purpose)),
            Err(JwtVerificationError::InvalidPurpose)
        );
    }

    #[test]
    fn verifier_fails_when_clock_is_unavailable() {
        let encoded = token("key-1", &claims(1_100));
        assert_eq!(
            verifier(None).verify(request(&encoded)),
            Err(JwtVerificationError::ClockUnavailable)
        );
    }
}
