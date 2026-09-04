//! Standard OIDC mechanics for the native public client.
//!
//! The engine depends only on discovery, authorization, token, JWKS, and
//! `UserInfo` standards. Better Auth identifiers and schema types never enter
//! this module: the control plane is addressed as a plain OIDC issuer that
//! signs ID tokens with Ed25519, verified here with the workspace-pinned
//! `ed25519-dalek`. No tokens are logged.

use std::{
    fmt::Write as _,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axiusflow_account::{AccountId, PlanId};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use serde::Deserialize;

/// Authorization URL parameters for one login transaction.
pub struct AuthorizationRequest<'a> {
    /// Control-plane issuer origin (https).
    pub issuer: &'a str,
    /// Native public client identifier (no secret).
    pub client_id: &'a str,
    /// Loopback redirect URI for the bound listener.
    pub redirect_uri: &'a str,
    /// OAuth state for this transaction.
    pub state: &'a str,
    /// OIDC nonce bound to the ID token.
    pub nonce: &'a str,
    /// PKCE `S256` code challenge.
    pub code_challenge: &'a str,
}

/// Builds the system-browser authorization URL for one transaction.
///
/// # Errors
///
/// Returns an error when the issuer is not a valid `https` origin.
pub fn authorization_url(request: &AuthorizationRequest<'_>) -> Result<String, String> {
    if !request.issuer.starts_with("https://") || request.issuer.contains([' ', '?', '#']) {
        return Err("account control plane issuer is invalid".to_string());
    }
    Ok(format!(
        "{issuer}/authorize?response_type=code&client_id={client}&redirect_uri={redirect}&scope=openid&state={state}&nonce={nonce}&code_challenge={challenge}&code_challenge_method=S256",
        issuer = request.issuer,
        client = url_encode(request.client_id),
        redirect = url_encode(request.redirect_uri),
        state = url_encode(request.state),
        nonce = url_encode(request.nonce),
        challenge = url_encode(request.code_challenge),
    ))
}

fn url_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// Verified token outcome for one login transaction. Access material stays
/// in engine memory and never crosses IPC or enters logs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedTokens {
    /// OIDC subject linked to the canonical account.
    pub subject: String,
    /// Bearer proof for the control-plane link call (memory only).
    pub access: String,
    /// Refresh material for vault storage (opaque to logs).
    pub refresh: Option<String>,
}

/// Exchanges one authorization code and validates the ID token.
///
/// Posts to `{issuer}/oauth/token`, verifies the Ed25519 ID-token signature
/// against `{issuer}/.well-known/jwks.json`, and checks issuer, audience,
/// expiry, and nonce before returning the subject.
///
/// # Errors
///
/// Returns a redacted actionable error when the exchange, signature,
/// claims, or link lookup fails. Raw provider payloads never enter the detail.
#[allow(clippy::too_many_lines)]
pub fn exchange_code(
    issuer: &str,
    client_id: &str,
    redirect_uri: &str,
    code: &str,
    code_verifier: &str,
    expected_nonce: &str,
) -> Result<VerifiedTokens, String> {
    let agent = oidc_agent();
    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        url_encode(code),
        url_encode(redirect_uri),
        url_encode(client_id),
        url_encode(code_verifier),
    );
    let mut response = agent
        .post(&format!("{issuer}/oauth/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .send(body)
        .map_err(|_| "sign-in exchange failed; retry sign-in".to_string())?;
    let token: TokenResponse = response
        .body_mut()
        .with_config()
        .limit(65_536)
        .read_json()
        .map_err(|_| "sign-in exchange failed; retry sign-in".to_string())?;
    if token.identity.is_empty() || token.identity.len() > 16_384 {
        return Err("sign-in exchange failed; retry sign-in".to_string());
    }
    if token.bearer.is_empty() || token.bearer.len() > 16_384 {
        return Err("sign-in exchange failed; retry sign-in".to_string());
    }
    let subject = verify_id_token(issuer, client_id, expected_nonce, &agent, &token.identity)?;
    Ok(VerifiedTokens {
        subject,
        access: token.bearer,
        refresh: token.refresh.filter(|token| !token.is_empty()),
    })
}

/// Links one verified OIDC subject to the canonical Axiusflow account.
///
/// # Errors
///
/// Returns a redacted actionable error when the control plane cannot link
/// the subject or returns an unknown plan.
pub fn link_subject(
    issuer: &str,
    access_token: &str,
    subject: &str,
) -> Result<(AccountId, PlanId), String> {
    let agent = oidc_agent();
    let body = format!(
        "{{\"subject\":\"{}\"}}",
        subject.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let mut response = agent
        .post(&format!("{issuer}/api/axiusflow/link"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {access_token}"))
        .send(body)
        .map_err(|_| "account linking failed; retry sign-in".to_string())?;
    let link: LinkResponse = response
        .body_mut()
        .with_config()
        .limit(4096)
        .read_json()
        .map_err(|_| "account linking failed; retry sign-in".to_string())?;
    let account_id =
        AccountId::try_new(link.account_id).map_err(|_| "account linking failed".to_string())?;
    let plan = PlanId::try_parse(&link.plan).map_err(|_| "account linking failed".to_string())?;
    Ok((account_id, plan))
}

fn oidc_agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build();
    ureq::Agent::new_with_config(config)
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default, rename = "id_token")]
    identity: String,
    #[serde(default, rename = "access_token")]
    bearer: String,
    #[serde(default, rename = "refresh_token")]
    refresh: Option<String>,
}

#[derive(Deserialize)]
struct LinkResponse {
    #[serde(default)]
    account_id: String,
    #[serde(default)]
    plan: String,
}

fn verify_id_token(
    issuer: &str,
    client_id: &str,
    expected_nonce: &str,
    agent: &ureq::Agent,
    id_token: &str,
) -> Result<String, String> {
    let (header, payload, signature) = split_jwt(id_token)?;
    let header_json: JwtHeader =
        serde_json::from_slice(&decode_part(header)?).map_err(|_| claim_failure())?;
    if header_json.alg != "EdDSA" {
        return Err(claim_failure());
    }
    let key = fetch_verifying_key(agent, issuer, header_json.kid.as_deref())?;
    let signature = Signature::from_slice(&decode_part(signature)?).map_err(|_| claim_failure())?;
    key.verify(format!("{header}.{payload}").as_bytes(), &signature)
        .map_err(|_| claim_failure())?;
    let claims: IdClaims =
        serde_json::from_slice(&decode_part(payload)?).map_err(|_| claim_failure())?;
    if claims.iss != issuer || !audience_matches(&claims.aud, client_id) {
        return Err(claim_failure());
    }
    if claims.exp <= unix_now() {
        return Err("sign-in session expired; retry sign-in".to_string());
    }
    if claims.nonce != expected_nonce || claims.sub.trim().is_empty() {
        return Err(claim_failure());
    }
    Ok(claims.sub)
}

fn claim_failure() -> String {
    "sign-in verification failed; retry sign-in".to_string()
}

fn split_jwt(token: &str) -> Result<(&str, &str, &str), String> {
    let mut parts = token.split('.');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(header), Some(payload), Some(signature), None) => Ok((header, payload, signature)),
        _ => Err(claim_failure()),
    }
}

fn decode_part(part: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD.decode(part).map_err(|_| claim_failure())
}

fn audience_matches(audience: &serde_json::Value, client_id: &str) -> bool {
    match audience {
        serde_json::Value::String(single) => single == client_id,
        serde_json::Value::Array(members) => members.iter().any(|member| member == client_id),
        _ => false,
    }
}

fn fetch_verifying_key(
    agent: &ureq::Agent,
    issuer: &str,
    kid: Option<&str>,
) -> Result<VerifyingKey, String> {
    let mut response = agent
        .get(&format!("{issuer}/.well-known/jwks.json"))
        .header("Accept", "application/json")
        .call()
        .map_err(|_| claim_failure())?;
    let jwks: Jwks = response
        .body_mut()
        .with_config()
        .limit(32_768)
        .read_json()
        .map_err(|_| claim_failure())?;
    for key in &jwks.keys {
        if key.kty == "OKP"
            && key.crv.as_deref() == Some("Ed25519")
            && kid.is_none_or(|kid| key.kid.as_deref() == Some(kid))
        {
            let raw = decode_part(&key.x)?;
            let bytes: [u8; 32] = raw.try_into().map_err(|_| claim_failure())?;
            return VerifyingKey::from_bytes(&bytes).map_err(|_| claim_failure());
        }
    }
    Err(claim_failure())
}

#[derive(Deserialize)]
struct JwtHeader {
    #[serde(default)]
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

#[derive(Deserialize)]
struct IdClaims {
    #[serde(default)]
    iss: String,
    #[serde(default)]
    aud: serde_json::Value,
    #[serde(default)]
    exp: u64,
    #[serde(default)]
    nonce: String,
    #[serde(default)]
    sub: String,
}

#[derive(Deserialize)]
struct Jwks {
    #[serde(default)]
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    #[serde(default)]
    kty: String,
    #[serde(default)]
    crv: Option<String>,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    x: String,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::{AuthorizationRequest, audience_matches, authorization_url, split_jwt, url_encode};

    #[test]
    fn authorization_url_carries_pkce_and_loopback() {
        let request = AuthorizationRequest {
            issuer: "https://auth.axiusflow.com",
            client_id: "axiusflow-desktop",
            redirect_uri: "http://127.0.0.1:43129/callback",
            state: "state-value",
            nonce: "nonce-value",
            code_challenge: "challenge-value",
        };
        let url = authorization_url(&request).expect("URL builds");
        assert!(url.starts_with("https://auth.axiusflow.com/authorize?"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A43129%2Fcallback"));
        assert!(
            authorization_url(&AuthorizationRequest {
                issuer: "http://auth.axiusflow.com",
                ..request
            })
            .is_err()
        );
    }

    #[test]
    fn malformed_jwt_fails_closed() {
        assert!(split_jwt("only-one-part").is_err());
        assert!(split_jwt("a.b.c.d").is_err());
        assert_eq!(url_encode("a b+c"), "a%20b%2Bc");
    }

    #[test]
    fn audience_matching_covers_string_and_array() {
        let single = serde_json::Value::String("axiusflow-desktop".to_string());
        assert!(audience_matches(&single, "axiusflow-desktop"));
        assert!(!audience_matches(&single, "other"));
        let many = serde_json::Value::Array(vec![serde_json::Value::String("other".to_string())]);
        assert!(!audience_matches(&many, "axiusflow-desktop"));
    }
}
