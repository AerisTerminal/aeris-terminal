//! Deterministic Ed25519 keys, JWKS documents, and tokens for this crate's unit tests.

use crate::identity::{JwtVerificationRequest, TokenPurpose, VerificationClock};
use crate::key_set::Ed25519KeySetSnapshot;
use crate::verifier::Ed25519JwtVerifier;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::json;

#[derive(Clone, Copy)]
pub(crate) struct FixedClock(pub(crate) Option<u64>);

impl VerificationClock for FixedClock {
    fn now_unix_seconds(&self) -> Option<u64> {
        self.0
    }
}

pub(crate) fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7_u8; 32])
}

pub(crate) fn jwks(key_id: &str) -> String {
    let encoded_key = URL_SAFE_NO_PAD.encode(signing_key().verifying_key().as_bytes());
    json!({
        "keys": [{
            "kty": "OKP",
            "crv": "Ed25519",
            "alg": "EdDSA",
            "kid": key_id,
            "x": encoded_key,
            "use": "sig",
            "key_ops": ["verify"]
        }]
    })
    .to_string()
}

pub(crate) fn token(key_id: &str, claims: &serde_json::Value) -> String {
    let encoded_header = URL_SAFE_NO_PAD.encode(
        json!({"alg": "EdDSA", "typ": "JWT", "kid": key_id})
            .to_string()
            .as_bytes(),
    );
    let encoded_claims = URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes());
    let signing_input = format!("{encoded_header}.{encoded_claims}");
    let signature = signing_key().sign(signing_input.as_bytes());
    format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

pub(crate) fn claims(expiration: u64) -> serde_json::Value {
    json!({
        "iss": "issuer-1",
        "aud": ["other-audience", "service-1"],
        "sub": "principal-1",
        "exp": expiration,
        "nbf": 900,
        "purpose": "service_access",
        "session_id": "session-1",
        "device_id": "device-1"
    })
}

pub(crate) fn verifier(now: Option<u64>) -> Ed25519JwtVerifier<FixedClock> {
    let key_set =
        Ed25519KeySetSnapshot::try_from_jwks_json(9, &jwks("key-1")).expect("test JWKS is valid");
    Ed25519JwtVerifier::try_new(key_set, FixedClock(now), 5).expect("test skew is bounded")
}

pub(crate) fn request(encoded_token: &str) -> JwtVerificationRequest<'_> {
    JwtVerificationRequest {
        encoded_token,
        expected_issuer: "issuer-1",
        expected_audience: "service-1",
        purpose: TokenPurpose::ServiceAccess,
    }
}
