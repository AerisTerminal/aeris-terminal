//! Offline entitlement lease fetch, validation, and cache.
//!
//! The worker refreshes the lease from the control plane, verifies the
//! Ed25519 signature against the entitlement JWKS directory, validates
//! claims through the pure account domain, and caches only progressively
//! newer revisions in the native vault. Validation outcomes are logged in
//! redacted form and surfaced as session state; the verified plan is the
//! desktop capability source.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use sha2::{Digest as _, Sha256};
use tradingplot_account::{AccountId, LeaseClaims, PlanId};

use super::oidc::OidcEndpoints;
use tradingplot_platform_runtime::CredentialVault;

/// Approved default: online lease refresh cadence.
pub const LEASE_REFRESH_INTERVAL_SECONDS: u64 = 6 * 3600;
/// Approved default: cached leases stay valid offline for 24 hours.
pub const LEASE_OFFLINE_VALIDITY_SECONDS: u64 = 24 * 3600;
/// Maximum compact lease size accepted before validation.
pub const MAXIMUM_LEASE_BYTES: usize = 2048;

/// Outcome of one lease refresh round.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshOutcome {
    /// A newer valid lease was cached.
    Refreshed(PlanId),
    /// The cached lease is unchanged and still valid.
    Current(Option<PlanId>),
    /// Process lifecycle quiescing prevented a new refresh grant from starting.
    Deferred,
    /// The network failed but a valid cached lease covers the outage.
    OfflineCovered(PlanId),
    /// No valid lease is available.
    Unavailable,
}

/// Derives the stable non-secret device identifier from the vault device key.
#[must_use]
pub fn device_id_for_key(device_key: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(device_key))
}

/// Validates one compact lease against directory keys and the expected binding.
///
/// # Errors
///
/// Returns a redacted error when the shape, signature, key, claims, binding,
/// or time window fails. The compact value itself never enters the detail.
#[allow(clippy::too_many_arguments)]
pub fn validate_compact(
    compact: &str,
    keys: &[(String, [u8; 32])],
    expected_account: &AccountId,
    expected_device: &str,
    now_unix_seconds: u64,
) -> Result<LeaseClaims, String> {
    if compact.is_empty() || compact.len() > MAXIMUM_LEASE_BYTES {
        return Err(lease_failure());
    }
    let mut parts = compact.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(lease_failure());
    };
    let header_json: LeaseHeader =
        serde_json::from_slice(&decode_part(header)?).map_err(|_| lease_failure())?;
    if header_json.alg != "EdDSA" {
        return Err(lease_failure());
    }
    let key_id = header_json.kid.unwrap_or_default();
    let key_bytes = keys
        .iter()
        .find(|(kid, _)| *kid == key_id)
        .map(|(_, bytes)| bytes)
        .ok_or_else(lease_failure)?;
    let key = VerifyingKey::from_bytes(key_bytes).map_err(|_| lease_failure())?;
    let signature = Signature::from_slice(&decode_part(signature)?).map_err(|_| lease_failure())?;
    key.verify(format!("{header}.{payload}").as_bytes(), &signature)
        .map_err(|_| lease_failure())?;
    let claims: LeasePayload =
        serde_json::from_slice(&decode_part(payload)?).map_err(|_| lease_failure())?;
    let account_id = AccountId::try_new(claims.aid).map_err(|_| lease_failure())?;
    let plan = PlanId::try_parse(&claims.plan).map_err(|_| lease_failure())?;
    LeaseClaims::try_new(
        claims.ver,
        account_id,
        claims.did,
        plan,
        claims.feat,
        claims.rev,
        claims.iat,
        claims.nbf,
        claims.exp,
        &claims.aud,
        claims.kid,
        expected_account,
        expected_device,
        now_unix_seconds,
    )
    .map_err(|_| lease_failure())
}

/// Extracts the account identity needed to select the expected binding before
/// full signature validation. Callers must never trust the returned identity
/// until [`validate_compact`] succeeds with it.
///
/// # Errors
///
/// Returns a redacted error when the compact value or account identity is
/// malformed or exceeds its bounds.
pub fn untrusted_account_id(compact: &str) -> Result<AccountId, String> {
    if compact.is_empty() || compact.len() > MAXIMUM_LEASE_BYTES {
        return Err(lease_failure());
    }
    let mut parts = compact.split('.');
    let (Some(_), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(lease_failure());
    };
    let claims: LeasePayload =
        serde_json::from_slice(&decode_part(payload)?).map_err(|_| lease_failure())?;
    AccountId::try_new(claims.aid).map_err(|_| lease_failure())
}

fn lease_failure() -> String {
    "entitlement lease verification failed".to_string()
}

fn decode_part(part: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD.decode(part).map_err(|_| lease_failure())
}

#[derive(serde::Deserialize)]
struct LeaseHeader {
    #[serde(default)]
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

#[derive(serde::Deserialize)]
struct LeasePayload {
    #[serde(default)]
    ver: u32,
    #[serde(default)]
    aid: String,
    #[serde(default)]
    did: String,
    #[serde(default)]
    plan: String,
    #[serde(default)]
    feat: u32,
    #[serde(default)]
    rev: u64,
    #[serde(default)]
    iat: u64,
    #[serde(default)]
    nbf: u64,
    #[serde(default)]
    exp: u64,
    #[serde(default)]
    aud: String,
    #[serde(default)]
    kid: String,
}

/// Fetches the entitlement JWKS directory from the control-plane origin.
///
/// # Errors
///
/// Returns a redacted error when the directory cannot be fetched or carries
/// no usable Ed25519 keys.
pub fn fetch_directory(
    agent: &ureq::Agent,
    origin: &str,
) -> Result<Vec<(String, [u8; 32])>, String> {
    let mut response = agent
        .get(&format!("{origin}/.well-known/entitlement-jwks.json"))
        .header("Accept", "application/json")
        .call()
        .map_err(|_| "entitlement directory is unreachable".to_string())?;
    let directory: Directory = response
        .body_mut()
        .with_config()
        .limit(32_768)
        .read_json()
        .map_err(|_| "entitlement directory is invalid".to_string())?;
    let mut keys = Vec::new();
    for key in directory.keys {
        if key.kty == "OKP"
            && key.crv.as_deref() == Some("Ed25519")
            && let (Some(kid), Ok(raw)) = (key.kid, URL_SAFE_NO_PAD.decode(&key.x))
            && !kid.is_empty()
            && let Ok(bytes) = <[u8; 32]>::try_from(raw)
        {
            keys.push((kid, bytes));
        }
    }
    if keys.is_empty() {
        return Err("entitlement directory carries no usable keys".to_string());
    }
    Ok(keys)
}

#[derive(serde::Deserialize)]
struct Directory {
    #[serde(default)]
    keys: Vec<DirectoryKey>,
}

#[derive(serde::Deserialize)]
struct DirectoryKey {
    #[serde(default)]
    kty: String,
    #[serde(default)]
    crv: Option<String>,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    x: String,
}

/// Loads the cached compact lease from the vault, if any.
///
/// # Errors
///
/// Returns a redacted error when protected credential storage cannot be read.
pub fn load_cached<V>(vault: &V, key: &str) -> Result<Option<String>, String>
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    Ok(vault
        .load(key)
        .map_err(|_| "credential storage is unavailable".to_string())?
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|lease| !lease.is_empty() && lease.len() <= MAXIMUM_LEASE_BYTES))
}

/// Fetches one compact lease for a verified session.
///
/// # Errors
///
/// Returns a redacted error when the control plane rejects the request or
/// the reply is malformed. Token material never enters the detail.
pub fn fetch_compact(
    endpoints: &OidcEndpoints,
    agent: &ureq::Agent,
    id_token: &str,
    subject: &str,
    device_id: &str,
) -> Result<String, String> {
    let body = format!(
        "{{\"subject\":\"{}\",\"id_token\":\"{}\",\"device_id\":\"{}\"}}",
        subject.replace('\\', "\\\\").replace('"', "\\\""),
        id_token.replace('\\', "\\\\").replace('"', "\\\""),
        device_id.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let mut response = agent
        .post(&endpoints.lease_endpoint)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send(body)
        .map_err(|_| "entitlement lease fetch failed".to_string())?;
    let reply: LeaseReply = response
        .body_mut()
        .with_config()
        .limit(8192)
        .read_json()
        .map_err(|_| "entitlement lease fetch failed".to_string())?;
    if reply.lease.is_empty() || reply.lease.len() > MAXIMUM_LEASE_BYTES {
        return Err("entitlement lease fetch failed".to_string());
    }
    Ok(reply.lease)
}

#[derive(serde::Deserialize)]
struct LeaseReply {
    #[serde(default)]
    lease: String,
}

#[cfg(test)]
mod tests {
    use super::{device_id_for_key, untrusted_account_id, validate_compact};
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer as _, SigningKey};
    use tradingplot_account::AccountId;

    fn sign_fixture(seed: &[u8; 32], kid: &str, payload: &serde_json::Value) -> String {
        let key = SigningKey::from_bytes(seed);
        let header = serde_json::json!({ "alg": "EdDSA", "kid": kid, "typ": "JWT" });
        let encoder = |value: &serde_json::Value| {
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).expect("fixture encodes"))
        };
        let input = format!("{}.{}", encoder(&header), encoder(payload));
        let signature = key.sign(input.as_bytes());
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
    }

    fn payload() -> serde_json::Value {
        serde_json::json!({
            "ver": 1, "aid": "acct_01", "did": "device-01", "plan": "pro",
            "feat": 7, "rev": 7, "iat": 1_700_000_000, "nbf": 1_700_000_000,
            "exp": 1_700_259_200, "aud": "axiusflow-engine", "kid": "ent1",
        })
    }

    #[test]
    fn valid_compact_lease_verifies_and_binds() {
        let seed = [9_u8; 32];
        let key = SigningKey::from_bytes(&seed);
        let keys = vec![("ent1".to_string(), key.verifying_key().to_bytes())];
        let compact = sign_fixture(&seed, "ent1", &payload());
        let account = AccountId::try_new("acct_01").expect("identity builds");
        let claims = validate_compact(&compact, &keys, &account, "device-01", 1_700_000_100)
            .expect("valid lease verifies");
        assert_eq!(claims.revision(), 7);
        assert_eq!(
            untrusted_account_id(&compact).expect("bounded account hint parses"),
            account
        );
    }

    #[test]
    fn unknown_key_wrong_binding_and_tampering_fail_closed() {
        let seed = [9_u8; 32];
        let other = [4_u8; 32];
        let key = SigningKey::from_bytes(&seed);
        let keys = vec![("ent1".to_string(), key.verifying_key().to_bytes())];
        let account = AccountId::try_new("acct_01").expect("identity builds");
        let compact = sign_fixture(&seed, "ent1", &payload());
        assert!(validate_compact(&compact, &keys, &account, "device-02", 1_700_000_100).is_err());
        assert!(validate_compact(&compact, &[], &account, "device-01", 1_700_000_100).is_err());
        let other_compact = sign_fixture(&other, "ent1", &payload());
        assert!(
            validate_compact(&other_compact, &keys, &account, "device-01", 1_700_000_100).is_err()
        );
        assert!(
            validate_compact("not.a.lease", &keys, &account, "device-01", 1_700_000_100).is_err()
        );
    }

    #[test]
    fn device_ids_are_stable_and_non_secret_shaped() {
        assert_eq!(device_id_for_key(b"key"), device_id_for_key(b"key"));
        assert_ne!(device_id_for_key(b"key"), device_id_for_key(b"other"));
        assert_eq!(device_id_for_key(b"key").len(), 43);
    }

    #[test]
    fn worker_minted_lease_verifies_natively() {
        // Compact JWT minted by `workers/auth/scripts/mint_fixture.mjs`
        // (seed `S7s0o0u56...gcI=`, kid `ent1`, now 1_700_000_100). Any worker
        // mint-shape drift breaks this fixture on purpose.
        let compact = "eyJhbGciOiJFZERTQSIsImtpZCI6ImVudDEiLCJ0eXAiOiJKV1QifQ.eyJ2ZXIiOjEsImFpZCI6ImFjY3RfMDEiLCJkaWQiOiJkZXZpY2UtMDEiLCJwbGFuIjoicHJvIiwiZmVhdCI6NywicmV2Ijo3LCJpYXQiOjE3MDAwMDAxMDAsIm5iZiI6MTcwMDAwMDEwMCwiZXhwIjoxNzAwMjU5MzAwLCJhdWQiOiJheGl1c2Zsb3ctZW5naW5lIiwia2lkIjoiZW50MSJ9.0a7-scnfiG-NKs3BlwuSz5JmsOMLJwhhI1BIhrMO_29qNKgM5zYORq8L75HxUQXr5IgMd8uez5wyv5dqM7VFCg";
        let raw = URL_SAFE_NO_PAD
            .decode("JoZhqPqx2aQlMfXJqcTjLHiAn9YsigCzExeB2Zblp8k")
            .expect("fixture key decodes");
        let bytes = <[u8; 32]>::try_from(raw).expect("fixture key fits");
        let keys = vec![("ent1".to_string(), bytes)];
        let account = AccountId::try_new("acct_01").expect("identity builds");
        let claims = validate_compact(compact, &keys, &account, "device-01", 1_700_000_100)
            .expect("worker lease verifies natively");
        assert_eq!(claims.revision(), 7);
        assert_eq!(claims.key_id(), "ent1");
    }
}
