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
    /// Discovered OIDC endpoints for the configured issuer.
    pub endpoints: &'a OidcEndpoints,
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

/// OIDC endpoints resolved from discovery plus the Axiusflow link route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OidcEndpoints {
    /// Verified issuer identity.
    pub issuer: String,
    /// Authorization endpoint from discovery metadata.
    pub authorization_endpoint: String,
    /// Token endpoint from discovery metadata.
    pub token_endpoint: String,
    /// JWKS URI from discovery metadata.
    pub jwks_uri: String,
    /// Revocation endpoint from discovery metadata.
    pub revocation_endpoint: String,
    /// Axiusflow subject-link route on the control-plane origin.
    pub link_endpoint: String,
    /// Axiusflow lease-issue route on the control-plane origin.
    pub lease_endpoint: String,
}

/// Resolves OIDC endpoints from discovery metadata.
///
/// Fetches `{issuer}/.well-known/openid-configuration`, requires the echoed
/// issuer to match the configured value exactly, requires every endpoint to
/// stay on the issuer origin, and derives the Axiusflow link route from the
/// control-plane origin.
///
/// # Errors
///
/// Returns a redacted actionable error when discovery, issuer matching, or
/// endpoint validation fails.
pub fn discover(issuer: &str) -> Result<OidcEndpoints, String> {
    if !issuer.starts_with("https://") || issuer.contains([' ', '?', '#']) {
        return Err("account control plane issuer is invalid".to_string());
    }
    let agent = oidc_agent();
    let mut response = agent
        .get(&format!("{issuer}/.well-known/openid-configuration"))
        .header("Accept", "application/json")
        .call()
        .map_err(|_| "account service is unreachable; retry sign-in".to_string())?;
    let metadata: DiscoveryMetadata = response
        .body_mut()
        .with_config()
        .limit(32_768)
        .read_json()
        .map_err(|_| "account service is unreachable; retry sign-in".to_string())?;
    parse_discovery(issuer, &metadata)
}

/// Validates discovery metadata against the configured issuer.
fn parse_discovery(issuer: &str, metadata: &DiscoveryMetadata) -> Result<OidcEndpoints, String> {
    if metadata.issuer != issuer {
        return Err("sign-in verification failed; retry sign-in".to_string());
    }
    let origin = control_plane_origin(issuer)?;
    for endpoint in [
        &metadata.authorization_endpoint,
        &metadata.token_endpoint,
        &metadata.jwks_uri,
        &metadata.revocation_endpoint,
    ] {
        if !endpoint.starts_with(&origin) {
            return Err("sign-in verification failed; retry sign-in".to_string());
        }
    }
    Ok(OidcEndpoints {
        issuer: metadata.issuer.clone(),
        authorization_endpoint: metadata.authorization_endpoint.clone(),
        token_endpoint: metadata.token_endpoint.clone(),
        jwks_uri: metadata.jwks_uri.clone(),
        revocation_endpoint: metadata.revocation_endpoint.clone(),
        link_endpoint: format!("{origin}/api/axiusflow/link"),
        lease_endpoint: format!("{origin}/api/axiusflow/lease"),
    })
}

/// Revokes one refresh token at the discovered revocation endpoint.
///
/// Best-effort by design: local vault deletion always happens first, so a
/// revocation failure only leaves the server-side grant to expire.
///
/// # Errors
///
/// Returns a redacted error when the revocation request fails.
pub fn revoke_refresh(endpoints: &OidcEndpoints, refresh_token: &str) -> Result<(), String> {
    let agent = oidc_agent();
    let body = format!(
        "token={}&token_type_hint=refresh_token",
        url_encode(refresh_token)
    );
    let mut response = agent
        .post(&endpoints.revocation_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .send(body)
        .map_err(|_| "sign-out revocation failed".to_string())?;
    response
        .body_mut()
        .with_config()
        .limit(4096)
        .read_to_vec()
        .map_err(|_| "sign-out revocation failed".to_string())?;
    Ok(())
}

/// Derives the control-plane origin from the OIDC issuer.
///
/// The Axiusflow issuer is the Better Auth mount (`{origin}/api/auth`); the
/// Axiusflow-owned link route lives on the origin beside it.
///
/// # Errors
///
/// Returns an error when the issuer does not carry the expected mount.
pub fn control_plane_origin(issuer: &str) -> Result<String, String> {
    issuer
        .strip_suffix("/api/auth")
        .filter(|origin| origin.starts_with("https://") && !origin.contains([' ', '?', '#']))
        .map(str::to_string)
        .ok_or_else(|| "account control plane issuer is invalid".to_string())
}

/// Builds the system-browser authorization URL for one transaction.
///
/// # Errors
///
/// Returns an error when the discovered authorization endpoint is invalid.
pub fn authorization_url(request: &AuthorizationRequest<'_>) -> Result<String, String> {
    if !request
        .endpoints
        .authorization_endpoint
        .starts_with("https://")
    {
        return Err("account control plane issuer is invalid".to_string());
    }
    // `offline_access` keeps the refresh-token persistence path fed: the
    // engine stores the returned refresh grant in the vault and rotates the
    // session without another browser round-trip.
    Ok(format!(
        "{endpoint}?response_type=code&client_id={client}&redirect_uri={redirect}&scope=openid%20offline_access&state={state}&nonce={nonce}&code_challenge={challenge}&code_challenge_method=S256",
        endpoint = request.endpoints.authorization_endpoint,
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
    /// Identity proof for control-plane calls (memory only).
    pub id_token: String,
    /// Refresh material for vault storage (opaque to logs).
    pub refresh: Option<String>,
}

/// Exchanges one authorization code and validates the ID token.
///
/// Posts to the discovered token endpoint, verifies the Ed25519 ID-token
/// signature against the discovered JWKS URI, and checks issuer, audience,
/// expiry, and nonce before returning the subject.
///
/// # Errors
///
/// Returns a redacted actionable error when the exchange, signature,
/// claims, or link lookup fails. Raw provider payloads never enter the detail.
#[allow(clippy::too_many_lines)]
pub fn exchange_code(
    endpoints: &OidcEndpoints,
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
        .post(&endpoints.token_endpoint)
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
    let subject = verify_id_token(
        &endpoints.issuer,
        client_id,
        Some(expected_nonce),
        &agent,
        &endpoints.jwks_uri,
        &token.identity,
    )?;
    Ok(VerifiedTokens {
        subject,
        access: token.bearer,
        id_token: token.identity,
        refresh: token.refresh.filter(|token| !token.is_empty()),
    })
}

/// Refreshes one token set with a vault refresh token.
///
/// The refreshed ID token carries no nonce (none was requested in this
/// grant), so nonce checking is skipped while issuer, audience, signature,
/// and expiry still verify.
///
/// # Errors
///
/// Returns a redacted actionable error when the grant or validation fails.
pub fn refresh_grant(
    endpoints: &OidcEndpoints,
    client_id: &str,
    refresh_token: &str,
) -> Result<VerifiedTokens, String> {
    let agent = oidc_agent();
    let body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}",
        url_encode(refresh_token),
        url_encode(client_id),
    );
    let mut response = agent
        .post(&endpoints.token_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .send(body)
        .map_err(|_| "session refresh failed".to_string())?;
    let token: TokenResponse = response
        .body_mut()
        .with_config()
        .limit(65_536)
        .read_json()
        .map_err(|_| "session refresh failed".to_string())?;
    if token.identity.is_empty() || token.identity.len() > 16_384 {
        return Err("session refresh failed".to_string());
    }
    let subject = verify_id_token(
        &endpoints.issuer,
        client_id,
        None,
        &agent,
        &endpoints.jwks_uri,
        &token.identity,
    )?;
    Ok(VerifiedTokens {
        subject,
        access: token.bearer.clone(),
        id_token: token.identity,
        refresh: token.refresh.filter(|token| !token.is_empty()),
    })
}

/// Server-verified profile attached to one linked session. Values come from
/// the control-plane user record only; the desktop never supplies identity
/// proof, and the engine never logs these values.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AccountProfile {
    /// Display name; empty when the user record has none.
    pub display_name: String,
    /// Email; empty when the user record has none.
    pub email: String,
    /// Photo URL; empty when absent. The desktop renders only `https` URLs.
    pub photo_url: String,
}

/// Links one verified OIDC subject to the canonical Axiusflow account.
///
/// The ID token travels as the proof: the control plane verifies it
/// server-side and never trusts the client-claimed subject. The verified
/// user record travels back as the session profile.
///
/// # Errors
///
/// Returns a redacted actionable error when the control plane cannot link
/// the subject or returns an unknown plan.
pub fn link_subject(
    endpoints: &OidcEndpoints,
    id_token: &str,
    subject: &str,
) -> Result<(AccountId, PlanId, AccountProfile), String> {
    let agent = oidc_agent();
    let body = format!(
        "{{\"subject\":\"{}\",\"id_token\":\"{}\"}}",
        subject.replace('\\', "\\\\").replace('"', "\\\""),
        id_token.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let mut response = agent
        .post(&endpoints.link_endpoint)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send(body)
        .map_err(|_| "account linking failed; retry sign-in".to_string())?;
    let link: LinkResponse = response
        .body_mut()
        .with_config()
        .limit(16_384)
        .read_json()
        .map_err(|_| "account linking failed; retry sign-in".to_string())?;
    let account_id =
        AccountId::try_new(link.account_id).map_err(|_| "account linking failed".to_string())?;
    let plan = PlanId::try_parse(&link.plan).map_err(|_| "account linking failed".to_string())?;
    Ok((
        account_id,
        plan,
        AccountProfile {
            display_name: clip_profile(
                &link.display_name,
                axiusflow_account::MAXIMUM_PROFILE_NAME_BYTES,
            ),
            email: clip_profile(&link.email, axiusflow_account::MAXIMUM_PROFILE_EMAIL_BYTES),
            photo_url: clip_profile(
                &link.photo_url,
                axiusflow_account::MAXIMUM_PROFILE_PHOTO_URL_BYTES,
            ),
        },
    ))
}

/// Clips server profile text to the IPC bound on a character boundary.
/// Overlong values truncate; they never fail the link.
fn clip_profile(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_string();
    }
    let mut end = maximum_bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

pub(crate) fn oidc_agent() -> ureq::Agent {
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
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    photo_url: String,
}

#[derive(Deserialize)]
struct DiscoveryMetadata {
    #[serde(default)]
    issuer: String,
    #[serde(default)]
    authorization_endpoint: String,
    #[serde(default)]
    token_endpoint: String,
    #[serde(default)]
    jwks_uri: String,
    #[serde(default)]
    revocation_endpoint: String,
}

fn verify_id_token(
    issuer: &str,
    client_id: &str,
    expected_nonce: Option<&str>,
    agent: &ureq::Agent,
    jwks_uri: &str,
    id_token: &str,
) -> Result<String, String> {
    let (header, payload, signature) = split_jwt(id_token)?;
    let header_json: JwtHeader =
        serde_json::from_slice(&decode_part(header)?).map_err(|_| claim_failure())?;
    if header_json.alg != "EdDSA" {
        return Err(claim_failure());
    }
    let key = fetch_verifying_key(agent, jwks_uri, header_json.kid.as_deref())?;
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
    // A present nonce must match the transaction; refresh grants carry none.
    if !claims.nonce.is_empty() && Some(claims.nonce.as_str()) != expected_nonce {
        return Err(claim_failure());
    }
    if claims.sub.trim().is_empty() {
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
    jwks_uri: &str,
    kid: Option<&str>,
) -> Result<VerifyingKey, String> {
    let mut response = agent
        .get(jwks_uri)
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
    use super::{
        AuthorizationRequest, OidcEndpoints, audience_matches, authorization_url,
        control_plane_origin, split_jwt, url_encode,
    };

    fn endpoints() -> OidcEndpoints {
        OidcEndpoints {
            issuer: "https://auth.axiusflow.com/api/auth".to_string(),
            authorization_endpoint: "https://auth.axiusflow.com/api/auth/oauth2/authorize"
                .to_string(),
            token_endpoint: "https://auth.axiusflow.com/api/auth/oauth2/token".to_string(),
            jwks_uri: "https://auth.axiusflow.com/api/auth/jwks".to_string(),
            revocation_endpoint: "https://auth.axiusflow.com/api/auth/oauth2/revoke".to_string(),
            link_endpoint: "https://auth.axiusflow.com/api/axiusflow/link".to_string(),
            lease_endpoint: "https://auth.axiusflow.com/api/axiusflow/lease".to_string(),
        }
    }

    fn discovery_fixture() -> super::DiscoveryMetadata {
        super::DiscoveryMetadata {
            issuer: "https://auth.axiusflow.com/api/auth".to_string(),
            authorization_endpoint: "https://auth.axiusflow.com/api/auth/oauth2/authorize"
                .to_string(),
            token_endpoint: "https://auth.axiusflow.com/api/auth/oauth2/token".to_string(),
            jwks_uri: "https://auth.axiusflow.com/api/auth/jwks".to_string(),
            revocation_endpoint: "https://auth.axiusflow.com/api/auth/oauth2/revoke".to_string(),
        }
    }

    #[test]
    fn discovery_fixture_matches_the_live_worker_layout() {
        let endpoints =
            super::parse_discovery("https://auth.axiusflow.com/api/auth", &discovery_fixture())
                .expect("fixture parses");
        assert_eq!(endpoints, self::endpoints());
    }

    #[test]
    fn discovery_rejects_mismatched_issuer_and_off_origin_endpoints() {
        assert!(
            super::parse_discovery(
                "https://auth.axiusflow.com/api/auth",
                &super::DiscoveryMetadata {
                    issuer: "https://evil.example.com/api/auth".to_string(),
                    ..discovery_fixture()
                }
            )
            .is_err()
        );
        assert!(
            super::parse_discovery(
                "https://auth.axiusflow.com/api/auth",
                &super::DiscoveryMetadata {
                    jwks_uri: "https://evil.example.com/jwks".to_string(),
                    ..discovery_fixture()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn authorization_url_carries_pkce_and_loopback() {
        let endpoints = endpoints();
        let request = AuthorizationRequest {
            endpoints: &endpoints,
            client_id: "axiusflow-desktop",
            redirect_uri: "http://127.0.0.1:43129/callback",
            state: "state-value",
            nonce: "nonce-value",
            code_challenge: "challenge-value",
        };
        let url = authorization_url(&request).expect("URL builds");
        assert!(url.starts_with("https://auth.axiusflow.com/api/auth/oauth2/authorize?"));
        assert!(url.contains("scope=openid%20offline_access"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A43129%2Fcallback"));
        let mut plain = endpoints.clone();
        plain.authorization_endpoint = "http://auth.axiusflow.com/oauth2/authorize".to_string();
        assert!(
            authorization_url(&AuthorizationRequest {
                endpoints: &plain,
                ..request
            })
            .is_err()
        );
    }

    #[test]
    fn control_plane_origin_requires_the_expected_mount() {
        assert_eq!(
            control_plane_origin("https://auth.axiusflow.com/api/auth"),
            Ok("https://auth.axiusflow.com".to_string())
        );
        assert!(control_plane_origin("https://auth.axiusflow.com").is_err());
        assert!(control_plane_origin("http://auth.axiusflow.com/api/auth").is_err());
    }

    #[test]
    fn profile_text_clips_on_character_boundaries() {
        assert_eq!(super::clip_profile("Ada", 128), "Ada");
        let long = "a".repeat(200);
        assert_eq!(super::clip_profile(&long, 128).len(), 128);
        // Multibyte names never split a character: the bound is bytes, the
        // cut is a boundary.
        let emoji = "😀".repeat(40);
        let clipped = super::clip_profile(&emoji, 128);
        assert!(clipped.len() <= 128);
        assert_eq!(clipped.chars().count(), 32);
        assert_eq!(
            super::clip_profile("", 128),
            String::new(),
            "missing profile stays empty, never fails"
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
