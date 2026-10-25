//! Standard OIDC mechanics for the native public client.
//!
//! The runtime depends only on discovery, authorization, token, revocation and
//! JWKS standards as served by the Aeris Cognito user pool. ID tokens are
//! RS256-signed and verified here against the issuer JWKS with the
//! workspace-pinned `ring`. No tokens are logged.

use std::{
    fmt::Write as _,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::Deserialize;
use zeroize::Zeroizing;

/// Maximum discovery document size accepted from the issuer.
const MAXIMUM_DISCOVERY_BYTES: u64 = 32_768;
/// Maximum JWKS document size accepted from the issuer.
const MAXIMUM_JWKS_BYTES: u64 = 32_768;
/// Maximum number of signing keys considered in one JWKS document.
const MAXIMUM_JWKS_KEYS: usize = 16;
/// Maximum token-endpoint response size.
const MAXIMUM_TOKEN_RESPONSE_BYTES: u64 = 65_536;
/// Maximum compact ID-token length accepted for verification.
const MAXIMUM_ID_TOKEN_BYTES: usize = 16_384;
/// Maximum refresh-token length accepted for vault storage.
const MAXIMUM_REFRESH_TOKEN_BYTES: usize = 8_192;
/// Clock-skew allowance applied to ID-token expiry.
const ID_TOKEN_EXPIRY_LEEWAY_SECONDS: u64 = 300;
/// Scopes requested for the desktop session; no long-lived consent prompt.
const AUTHORIZATION_SCOPE: &str = "openid email profile";

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

/// OIDC endpoints resolved from discovery metadata.
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
}

/// Resolves OIDC endpoints from discovery metadata.
///
/// Fetches `{issuer}/.well-known/openid-configuration`, requires the echoed
/// issuer to match the configured value exactly, and requires every endpoint
/// to stay on the issuer origin or the configured hosted sign-in origin.
///
/// # Errors
///
/// Returns a redacted actionable error when discovery, issuer matching, or
/// endpoint validation fails.
pub fn discover(issuer: &str, hosted_origin: &str) -> Result<OidcEndpoints, String> {
    if parse_https_uri(issuer).is_err() || issuer.contains([' ', '?', '#']) {
        return Err("account service issuer is invalid".to_string());
    }
    let agent = oidc_agent();
    let mut response = agent
        .get(&format!("{issuer}/.well-known/openid-configuration"))
        .header("Accept", "application/json")
        .call()
        .map_err(|_| "account service is unreachable; check your connection".to_string())?;
    let metadata: DiscoveryMetadata = response
        .body_mut()
        .with_config()
        .limit(MAXIMUM_DISCOVERY_BYTES)
        .read_json()
        .map_err(|_| "account service is unreachable; check your connection".to_string())?;
    parse_discovery(issuer, hosted_origin, &metadata)
}

/// Validates discovery metadata against the configured issuer and origins.
fn parse_discovery(
    issuer: &str,
    hosted_origin: &str,
    metadata: &DiscoveryMetadata,
) -> Result<OidcEndpoints, String> {
    if metadata.issuer != issuer {
        return Err(claim_failure());
    }
    let issuer_uri = parse_https_uri(issuer).map_err(|_| claim_failure())?;
    let hosted_uri = parse_https_uri(hosted_origin).map_err(|_| claim_failure())?;
    for endpoint in [
        &metadata.authorization_endpoint,
        &metadata.token_endpoint,
        &metadata.jwks_uri,
        &metadata.revocation_endpoint,
    ] {
        if !same_https_origin(&issuer_uri, endpoint) && !same_https_origin(&hosted_uri, endpoint) {
            return Err(claim_failure());
        }
    }
    Ok(OidcEndpoints {
        issuer: metadata.issuer.clone(),
        authorization_endpoint: metadata.authorization_endpoint.clone(),
        token_endpoint: metadata.token_endpoint.clone(),
        jwks_uri: metadata.jwks_uri.clone(),
        revocation_endpoint: metadata.revocation_endpoint.clone(),
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
pub fn revoke_refresh(
    endpoints: &OidcEndpoints,
    client_id: &str,
    refresh_token: &str,
) -> Result<(), String> {
    let agent = oidc_agent();
    let body = Zeroizing::new(format!(
        "token={}&client_id={}",
        url_encode(refresh_token),
        url_encode(client_id)
    ));
    let mut response = agent
        .post(&endpoints.revocation_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .send(body.as_str())
        .map_err(|_| "sign-out revocation failed".to_string())?;
    response
        .body_mut()
        .with_config()
        .limit(4096)
        .read_to_vec()
        .map_err(|_| "sign-out revocation failed".to_string())?;
    Ok(())
}

pub(super) fn parse_https_uri(value: &str) -> Result<ureq::http::Uri, String> {
    let uri = value
        .parse::<ureq::http::Uri>()
        .map_err(|_| "account service address is invalid".to_string())?;
    let Some(authority) = uri.authority() else {
        return Err("account service address is invalid".to_string());
    };
    if uri.scheme_str() != Some("https") || uri.host().is_none() || authority.as_str().contains('@')
    {
        return Err("account service address is invalid".to_string());
    }
    Ok(uri)
}

fn same_https_origin(origin: &ureq::http::Uri, endpoint: &str) -> bool {
    let Ok(endpoint) = parse_https_uri(endpoint) else {
        return false;
    };
    origin
        .host()
        .zip(endpoint.host())
        .is_some_and(|(origin_host, endpoint_host)| {
            origin_host.eq_ignore_ascii_case(endpoint_host)
                && origin.port_u16().unwrap_or(443) == endpoint.port_u16().unwrap_or(443)
        })
}

/// Builds the system-browser authorization URL for one transaction.
///
/// # Errors
///
/// Returns an error when the discovered authorization endpoint is invalid.
pub fn authorization_url(request: &AuthorizationRequest<'_>) -> Result<String, String> {
    if parse_https_uri(&request.endpoints.authorization_endpoint).is_err() {
        return Err("account service address is invalid".to_string());
    }
    Ok(format!(
        "{endpoint}?response_type=code&client_id={client}&redirect_uri={redirect}&scope={scope}&state={state}&nonce={nonce}&code_challenge={challenge}&code_challenge_method=S256",
        endpoint = request.endpoints.authorization_endpoint,
        client = url_encode(request.client_id),
        redirect = url_encode(request.redirect_uri),
        scope = url_encode(AUTHORIZATION_SCOPE),
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

/// Secret token material that is cleared when its last owner is dropped.
///
/// The inner value is available only through an explicit exposure method and
/// debug formatting never includes it. Deliberately do not implement `Clone`
/// or `Display`: token ownership must stay visible and formatting must not
/// accidentally disclose credentials.
#[derive(Eq, PartialEq)]
pub struct SecretToken(Zeroizing<String>);

impl SecretToken {
    pub(super) fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    pub(super) fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Debug for SecretToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretToken([REDACTED])")
    }
}

/// Profile carried by one verified ID token. Values come only from signed
/// claims; the runtime never logs them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AccountProfile {
    /// Preferred display name (`nickname`, else `name`); empty when absent.
    pub display_name: String,
    /// Email; empty when absent.
    pub email: String,
    /// Photo URL (`picture`); empty when absent. The desktop renders only `https` URLs.
    pub photo_url: String,
}

/// Verified token outcome for one grant. Only the refresh token is kept as
/// secret material; the ID token is consumed by verification and the access
/// token is not needed by the desktop.
#[derive(Debug, Eq, PartialEq)]
pub struct VerifiedTokens {
    /// Verified OIDC subject: the canonical account identity.
    pub subject: String,
    /// Profile from the verified ID token.
    pub profile: AccountProfile,
    /// Refresh material for vault storage (opaque to logs).
    pub refresh: Option<SecretToken>,
}

/// Exchanges one authorization code and validates the ID token.
///
/// Posts to the discovered token endpoint, verifies the RS256 ID-token
/// signature against the discovered JWKS URI, and checks issuer, audience,
/// token use, expiry, and nonce before returning the subject and profile.
///
/// # Errors
///
/// Returns a redacted actionable error when the exchange, signature, or
/// claims fail. Raw provider payloads never enter the detail.
pub fn exchange_code(
    endpoints: &OidcEndpoints,
    agent: &ureq::Agent,
    client_id: &str,
    redirect_uri: &str,
    code: &str,
    code_verifier: &str,
    expected_nonce: &str,
) -> Result<VerifiedTokens, String> {
    let body = Zeroizing::new(format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        url_encode(code),
        url_encode(redirect_uri),
        url_encode(client_id),
        url_encode(code_verifier),
    ));
    let mut response = agent
        .post(&endpoints.token_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .send(body.as_str())
        .map_err(|_| "sign-in exchange failed; retry sign-in".to_string())?;
    let token: TokenResponse = response
        .body_mut()
        .with_config()
        .limit(MAXIMUM_TOKEN_RESPONSE_BYTES)
        .read_json()
        .map_err(|_| "sign-in exchange failed; retry sign-in".to_string())?;
    let claims = verify_id_token(
        &endpoints.issuer,
        client_id,
        Some(expected_nonce),
        agent,
        &endpoints.jwks_uri,
        &token.identity,
    )?;
    let refresh = bounded_refresh(token.refresh)
        .ok_or_else(|| "sign-in did not return a saved session; retry sign-in".to_string())?;
    Ok(verified_tokens(claims, Some(refresh)))
}

/// Sanitized refresh failure: only an explicit rejected grant proves expiry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshGrantError {
    Rejected,
    Unavailable,
}

/// Refreshes one session with a vault refresh token.
///
/// Refreshed ID tokens are not bound to a new nonce, so nonce checking is
/// skipped while issuer, audience, token use, signature, and expiry still
/// verify. A response without a rotated refresh token keeps the stored one.
///
/// # Errors
///
/// Returns [`RefreshGrantError::Rejected`] only for an explicit
/// `invalid_grant`; every other failure is [`RefreshGrantError::Unavailable`].
pub fn refresh_grant(
    endpoints: &OidcEndpoints,
    agent: &ureq::Agent,
    client_id: &str,
    refresh_token: &str,
) -> Result<VerifiedTokens, RefreshGrantError> {
    let body = Zeroizing::new(format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}",
        url_encode(refresh_token),
        url_encode(client_id),
    ));
    let mut response = agent
        .post(&endpoints.token_endpoint)
        .config()
        .http_status_as_error(false)
        .build()
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .send(body.as_str())
        .map_err(|_| RefreshGrantError::Unavailable)?;
    if !response.status().is_success() {
        #[derive(Deserialize)]
        struct OAuthError {
            error: String,
        }
        let status = response.status().as_u16();
        let rejected = response
            .body_mut()
            .with_config()
            .limit(4096)
            .read_json::<OAuthError>()
            .is_ok_and(|body| status == 400 && body.error == "invalid_grant");
        return Err(if rejected {
            RefreshGrantError::Rejected
        } else {
            RefreshGrantError::Unavailable
        });
    }
    let token: TokenResponse = response
        .body_mut()
        .with_config()
        .limit(MAXIMUM_TOKEN_RESPONSE_BYTES)
        .read_json()
        .map_err(|_| RefreshGrantError::Unavailable)?;
    let claims = verify_id_token(
        &endpoints.issuer,
        client_id,
        None,
        agent,
        &endpoints.jwks_uri,
        &token.identity,
    )
    .map_err(|_| RefreshGrantError::Unavailable)?;
    Ok(verified_tokens(claims, bounded_refresh(token.refresh)))
}

fn bounded_refresh(refresh: Option<String>) -> Option<SecretToken> {
    refresh
        .map(Zeroizing::new)
        .filter(|token| !token.is_empty() && token.len() <= MAXIMUM_REFRESH_TOKEN_BYTES)
        .map(|token| SecretToken::new(token.to_string()))
}

fn verified_tokens(claims: IdClaims, refresh: Option<SecretToken>) -> VerifiedTokens {
    let display_name = if claims.nickname.trim().is_empty() {
        claims.name
    } else {
        claims.nickname
    };
    VerifiedTokens {
        subject: claims.sub,
        profile: AccountProfile {
            display_name: clip_profile(
                display_name.trim(),
                aeris_account::MAXIMUM_PROFILE_NAME_BYTES,
            ),
            email: clip_profile(
                claims.email.trim(),
                aeris_account::MAXIMUM_PROFILE_EMAIL_BYTES,
            ),
            photo_url: clip_profile(
                claims.picture.trim(),
                aeris_account::MAXIMUM_PROFILE_PHOTO_URL_BYTES,
            ),
        },
        refresh,
    }
}

/// Clips profile text to the native contract bound on a character boundary.
/// Overlong values truncate; they never fail the sign-in.
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
    #[serde(default, rename = "refresh_token")]
    refresh: Option<String>,
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
) -> Result<IdClaims, String> {
    if id_token.is_empty() || id_token.len() > MAXIMUM_ID_TOKEN_BYTES {
        return Err(claim_failure());
    }
    let (header, payload, signature) = split_jwt(id_token)?;
    let header_json: JwtHeader =
        serde_json::from_slice(&decode_part(header)?).map_err(|_| claim_failure())?;
    if header_json.alg != "RS256" || header_json.kid.is_empty() {
        return Err(claim_failure());
    }
    let jwks = fetch_jwks(agent, jwks_uri)?;
    let key = select_signing_key(&jwks, &header_json.kid)?;
    verify_rs256(
        key,
        format!("{header}.{payload}").as_bytes(),
        &decode_part(signature)?,
    )?;
    let claims: IdClaims =
        serde_json::from_slice(&decode_part(payload)?).map_err(|_| claim_failure())?;
    validate_claims(&claims, issuer, client_id, expected_nonce, unix_now())?;
    Ok(claims)
}

fn validate_claims(
    claims: &IdClaims,
    issuer: &str,
    client_id: &str,
    expected_nonce: Option<&str>,
    now_unix_seconds: u64,
) -> Result<(), String> {
    if claims.iss != issuer || !audience_matches(&claims.aud, client_id) || claims.token_use != "id"
    {
        return Err(claim_failure());
    }
    if claims.exp.saturating_add(ID_TOKEN_EXPIRY_LEEWAY_SECONDS) <= now_unix_seconds {
        return Err("sign-in session expired; retry sign-in".to_string());
    }
    if expected_nonce.is_some_and(|expected| claims.nonce != expected) {
        return Err(claim_failure());
    }
    if claims.sub.trim().is_empty() || claims.sub.len() > aeris_account::MAXIMUM_ACCOUNT_ID_BYTES {
        return Err(claim_failure());
    }
    Ok(())
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

fn fetch_jwks(agent: &ureq::Agent, jwks_uri: &str) -> Result<Jwks, String> {
    let mut response = agent
        .get(jwks_uri)
        .header("Accept", "application/json")
        .call()
        .map_err(|_| claim_failure())?;
    let jwks: Jwks = response
        .body_mut()
        .with_config()
        .limit(MAXIMUM_JWKS_BYTES)
        .read_json()
        .map_err(|_| claim_failure())?;
    if jwks.keys.len() > MAXIMUM_JWKS_KEYS {
        return Err(claim_failure());
    }
    Ok(jwks)
}

/// Selects the RSA signing key named by the token `kid`. Keys declaring
/// another algorithm or a non-signature use are never eligible.
fn select_signing_key<'a>(jwks: &'a Jwks, kid: &str) -> Result<&'a Jwk, String> {
    jwks.keys
        .iter()
        .find(|key| {
            key.kid == kid
                && key.kty == "RSA"
                && key.alg.as_deref().is_none_or(|alg| alg == "RS256")
                && key
                    .key_use
                    .as_deref()
                    .is_none_or(|key_use| key_use == "sig")
        })
        .ok_or_else(claim_failure)
}

fn verify_rs256(key: &Jwk, signing_input: &[u8], signature: &[u8]) -> Result<(), String> {
    let components = RsaPublicKeyComponents {
        n: decode_part(&key.n)?,
        e: decode_part(&key.e)?,
    };
    components
        .verify(&RSA_PKCS1_2048_8192_SHA256, signing_input, signature)
        .map_err(|_| claim_failure())
}

#[derive(Deserialize)]
struct JwtHeader {
    #[serde(default)]
    alg: String,
    #[serde(default)]
    kid: String,
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
    #[serde(default)]
    token_use: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    nickname: String,
    #[serde(default)]
    picture: String,
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
    kid: String,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default, rename = "use")]
    key_use: Option<String>,
    #[serde(default)]
    n: String,
    #[serde(default)]
    e: String,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::{
        AccountProfile, AuthorizationRequest, IdClaims, Jwk, Jwks, OidcEndpoints, SecretToken,
        VerifiedTokens, audience_matches, authorization_url, select_signing_key, split_jwt,
        url_encode, validate_claims, verified_tokens, verify_rs256,
    };

    const ISSUER: &str = "https://cognito-idp.us-east-1.amazonaws.com/us-east-1_fixture";
    const HOSTED: &str = "https://auth.example.test";

    // RFC 7515 Appendix A.2: the published RS256 example key and signature.
    const RFC_7515_N: &str = "ofgWCuLjybRlzo0tZWJjNiuSfb4p4fAkd_wWJcyQoTbji9k0l8W26mPddxHmfHQp-Vaw-4qPCJrcS2mJPMEzP1Pt0Bm4d4QlL-yRT-SFd2lZS-pCgNMsD1W_YpRPEwOWvG6b32690r2jZ47soMZo9wGzjb_7OMg0LOL-bSf63kpaSHSXndS5z5rexMdbBYUsLA9e-KXBdQOS-UTo7WTBEMa2R2CapHg665xsmtdVMTBQY4uDZlxvb3qCo5ZwKh9kG4LT6_I5IhlJH7aGhyxXFvUK-DWNmoudF8NAco9_h9iaGNj8q2ethFkMLs91kzk2PAcDTW9gb54h4FRWyuXpoQ";
    const RFC_7515_SIGNING_INPUT: &str = "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJqb2UiLA0KICJleHAiOjEzMDA4MTkzODAsDQogImh0dHA6Ly9leGFtcGxlLmNvbS9pc19yb290Ijp0cnVlfQ";
    const RFC_7515_SIGNATURE: &str = "cC4hiUPoj9Eetdgtv3hF80EGrhuB__dzERat0XF9g2VtQgr9PJbu3XOiZj5RZmh7AAuHIm4Bh-0Qc_lF5YKt_O8W2Fp5jujGbds9uJdbF9CUAr7t1dnZcAcQjbKBYNX4BAynRFdiuB--f_nZLgrnbyTyWzO75vRK5h6xBArLIARNPvkSjtQBMHlb1L07Qe7K0GarZRmB_eSN9383LcOLn6_dO--xi12jzDwusC-eOkHWEsqtFZESc6BfI7noOPqvhJ1phCnvWh6IeYI2w9QOYEUipUTI8np6LbgGY9Fs98rqVt5AXLIhWkWywlVmtVrBp0igcN_IoypGlUPQGe77Rw";

    fn rfc_key(kid: &str) -> Jwk {
        Jwk {
            kty: "RSA".to_string(),
            kid: kid.to_string(),
            alg: Some("RS256".to_string()),
            key_use: Some("sig".to_string()),
            n: RFC_7515_N.to_string(),
            e: "AQAB".to_string(),
        }
    }

    fn endpoints() -> OidcEndpoints {
        OidcEndpoints {
            issuer: ISSUER.to_string(),
            authorization_endpoint: format!("{HOSTED}/oauth2/authorize"),
            token_endpoint: format!("{HOSTED}/oauth2/token"),
            jwks_uri: format!("{ISSUER}/.well-known/jwks.json"),
            revocation_endpoint: format!("{HOSTED}/oauth2/revoke"),
        }
    }

    fn claims() -> IdClaims {
        IdClaims {
            iss: ISSUER.to_string(),
            aud: serde_json::Value::String("fixture-client".to_string()),
            exp: 2_000_000_000,
            nonce: "nonce-value".to_string(),
            sub: "8f0c2b2e-0000-4000-8000-000000000001".to_string(),
            token_use: "id".to_string(),
            email: "ada@example.test".to_string(),
            name: "Ada Lovelace".to_string(),
            nickname: String::new(),
            picture: "https://example.test/ada.png".to_string(),
        }
    }

    #[test]
    fn verified_token_debug_output_redacts_every_secret() {
        let tokens = VerifiedTokens {
            subject: "subject-1".to_string(),
            profile: AccountProfile::default(),
            refresh: Some(SecretToken::new("refresh-secret".to_string())),
        };

        let debug = format!("{tokens:?}");
        assert!(debug.contains("subject-1"));
        assert_eq!(debug.matches("[REDACTED]").count(), 1);
        assert!(!debug.contains("refresh-secret"));
    }

    #[test]
    fn rs256_signatures_verify_against_jwks_rsa_components() {
        let key = rfc_key("rfc");
        let signature = super::decode_part(RFC_7515_SIGNATURE).expect("signature decodes");
        verify_rs256(&key, RFC_7515_SIGNING_INPUT.as_bytes(), &signature)
            .expect("RFC 7515 example signature verifies");
        let mut tampered = RFC_7515_SIGNING_INPUT.as_bytes().to_vec();
        tampered[0] ^= 1;
        assert!(verify_rs256(&key, &tampered, &signature).is_err());
        let mut wrong_key = rfc_key("rfc");
        wrong_key.e = "AQAA".to_string();
        assert!(verify_rs256(&wrong_key, RFC_7515_SIGNING_INPUT.as_bytes(), &signature).is_err());
    }

    #[test]
    fn signing_key_selection_requires_kid_rsa_and_rs256() {
        let mut other_alg = rfc_key("kid-a");
        other_alg.alg = Some("RS512".to_string());
        let mut encryption = rfc_key("kid-b");
        encryption.key_use = Some("enc".to_string());
        let mut not_rsa = rfc_key("kid-c");
        not_rsa.kty = "EC".to_string();
        let jwks = Jwks {
            keys: vec![other_alg, encryption, not_rsa, rfc_key("kid-d")],
        };
        for kid in ["kid-a", "kid-b", "kid-c", "missing"] {
            assert!(
                select_signing_key(&jwks, kid).is_err(),
                "{kid} must not verify"
            );
        }
        assert_eq!(
            select_signing_key(&jwks, "kid-d").expect("RS256 key").kid,
            "kid-d"
        );
    }

    #[test]
    fn id_claims_require_issuer_audience_token_use_expiry_and_nonce() {
        let now = 1_900_000_000;
        validate_claims(
            &claims(),
            ISSUER,
            "fixture-client",
            Some("nonce-value"),
            now,
        )
        .expect("valid claims pass");
        validate_claims(&claims(), ISSUER, "fixture-client", None, now)
            .expect("refresh claims skip the authorization nonce");
        let reject = |claims: IdClaims| {
            validate_claims(&claims, ISSUER, "fixture-client", Some("nonce-value"), now).is_err()
        };
        assert!(reject(IdClaims {
            iss: "https://evil.example.test".to_string(),
            ..claims()
        }));
        assert!(reject(IdClaims {
            aud: serde_json::Value::String("other-client".to_string()),
            ..claims()
        }));
        assert!(reject(IdClaims {
            token_use: "access".to_string(),
            ..claims()
        }));
        assert!(reject(IdClaims {
            exp: now - 301,
            ..claims()
        }));
        assert!(reject(IdClaims {
            nonce: "replayed".to_string(),
            ..claims()
        }));
        assert!(reject(IdClaims {
            sub: " ".to_string(),
            ..claims()
        }));
        validate_claims(
            &IdClaims {
                exp: now - 10,
                ..claims()
            },
            ISSUER,
            "fixture-client",
            None,
            now,
        )
        .expect("small clock skew stays inside the expiry leeway");
    }

    #[test]
    fn profile_prefers_nickname_and_reads_verified_claims() {
        let tokens = verified_tokens(claims(), None);
        assert_eq!(tokens.subject, "8f0c2b2e-0000-4000-8000-000000000001");
        assert_eq!(tokens.profile.display_name, "Ada Lovelace");
        assert_eq!(tokens.profile.email, "ada@example.test");
        assert_eq!(tokens.profile.photo_url, "https://example.test/ada.png");
        let nicknamed = verified_tokens(
            IdClaims {
                nickname: "Ada".to_string(),
                ..claims()
            },
            None,
        );
        assert_eq!(nicknamed.profile.display_name, "Ada");
    }

    #[test]
    fn refresh_http_errors_distinguish_rejected_grants_from_unavailability() {
        use super::RefreshGrantError::{Rejected, Unavailable};
        use std::io::{Read as _, Write as _};
        for (status, body, expected) in [
            (400, r#"{"error":"invalid_grant"}"#, Rejected),
            (400, r#"{"error":"invalid_client"}"#, Unavailable),
            (429, r#"{"error":"invalid_grant"}"#, Unavailable),
            (503, r#"{"error":"server_error"}"#, Unavailable),
            (200, r#"{"id_token":"not-a-signed-token"}"#, Unavailable),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture listener");
            let mut endpoints = endpoints();
            endpoints.token_endpoint =
                format!("http://{}/token", listener.local_addr().expect("address"));
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("request");
                stream.set_nonblocking(false).expect("blocking stream");
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .expect("timeout");
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"client_id=fixture-client") {
                    let mut chunk = [0; 1024];
                    let count = stream.read(&mut chunk).expect("request bytes");
                    assert!(count > 0 && bytes.len() + count <= 4096);
                    bytes.extend_from_slice(&chunk[..count]);
                }
                write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).expect("response");
            });
            let result = super::refresh_grant(
                &endpoints,
                &super::oidc_agent(),
                "fixture-client",
                "fixture-refresh",
            );
            assert!(
                matches!(result, Err(error) if error == expected),
                "status {status}: {:?}",
                result.err()
            );
            server.join().expect("fixture server");
        }
    }

    fn discovery_fixture() -> super::DiscoveryMetadata {
        super::DiscoveryMetadata {
            issuer: ISSUER.to_string(),
            authorization_endpoint: format!("{HOSTED}/oauth2/authorize"),
            token_endpoint: format!("{HOSTED}/oauth2/token"),
            jwks_uri: format!("{ISSUER}/.well-known/jwks.json"),
            revocation_endpoint: format!("{HOSTED}/oauth2/revoke"),
        }
    }

    #[test]
    fn discovery_accepts_the_issuer_and_hosted_sign_in_origins() {
        let endpoints =
            super::parse_discovery(ISSUER, HOSTED, &discovery_fixture()).expect("fixture parses");
        assert_eq!(endpoints, self::endpoints());
    }

    #[test]
    fn discovery_rejects_mismatched_issuer_and_foreign_endpoints() {
        assert!(
            super::parse_discovery(
                ISSUER,
                HOSTED,
                &super::DiscoveryMetadata {
                    issuer: format!("{ISSUER}/"),
                    ..discovery_fixture()
                }
            )
            .is_err()
        );
        for token_endpoint in [
            "https://auth.example.test.evil.example/oauth2/token",
            "https://auth.example.test@evil.example/oauth2/token",
            "https://auth.example.test:444/oauth2/token",
            "http://auth.example.test/oauth2/token",
            "https://evil.example.com/oauth2/token",
        ] {
            assert!(
                super::parse_discovery(
                    ISSUER,
                    HOSTED,
                    &super::DiscoveryMetadata {
                        token_endpoint: token_endpoint.to_string(),
                        ..discovery_fixture()
                    }
                )
                .is_err(),
                "foreign endpoint must be rejected: {token_endpoint}"
            );
        }
        let endpoints = super::parse_discovery(
            ISSUER,
            HOSTED,
            &super::DiscoveryMetadata {
                token_endpoint: "https://AUTH.EXAMPLE.TEST:443/oauth2/token".to_string(),
                ..discovery_fixture()
            },
        )
        .expect("host case and the explicit default HTTPS port preserve the same origin");
        assert_eq!(
            endpoints.token_endpoint,
            "https://AUTH.EXAMPLE.TEST:443/oauth2/token"
        );
    }

    #[test]
    fn authorization_url_carries_pkce_scope_and_localhost_redirect() {
        let endpoints = endpoints();
        let request = AuthorizationRequest {
            endpoints: &endpoints,
            client_id: "fixture-client",
            redirect_uri: "http://localhost:47210/callback",
            state: "state-value",
            nonce: "nonce-value",
            code_challenge: "challenge-value",
        };
        let url = authorization_url(&request).expect("URL builds");
        assert!(url.starts_with("https://auth.example.test/oauth2/authorize?response_type=code&"));
        assert!(url.contains("client_id=fixture-client"));
        assert!(url.contains("scope=openid%20email%20profile&"));
        assert!(!url.contains("offline_access"));
        assert!(!url.contains("prompt="));
        assert!(url.contains("code_challenge=challenge-value&code_challenge_method=S256"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A47210%2Fcallback"));
        let mut plain = endpoints.clone();
        plain.authorization_endpoint = "http://auth.example.test/oauth2/authorize".to_string();
        assert!(
            authorization_url(&AuthorizationRequest {
                endpoints: &plain,
                ..request
            })
            .is_err()
        );
    }

    #[test]
    fn profile_text_clips_on_character_boundaries() {
        assert_eq!(super::clip_profile("Ada", 128), "Ada");
        let long = "a".repeat(200);
        assert_eq!(super::clip_profile(&long, 128).len(), 128);
        let emoji = "😀".repeat(40);
        let clipped = super::clip_profile(&emoji, 128);
        assert!(clipped.len() <= 128);
        assert_eq!(clipped.chars().count(), 32);
        assert_eq!(super::clip_profile("", 128), String::new());
    }

    #[test]
    fn malformed_jwt_fails_closed() {
        assert!(split_jwt("only-one-part").is_err());
        assert!(split_jwt("a.b.c.d").is_err());
        assert_eq!(url_encode("a b+c"), "a%20b%2Bc");
    }

    #[test]
    fn audience_matching_covers_string_and_array() {
        let single = serde_json::Value::String("fixture-client".to_string());
        assert!(audience_matches(&single, "fixture-client"));
        assert!(!audience_matches(&single, "other"));
        let many = serde_json::Value::Array(vec![serde_json::Value::String("other".to_string())]);
        assert!(!audience_matches(&many, "fixture-client"));
    }
}
