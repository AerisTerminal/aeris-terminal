//! Read-only tastytrade broker authorization transport.
//!
//! The hosted callback owns the confidential OAuth client and provider refresh
//! tokens. The market runtime owns the desktop connection capability and all
//! provider sessions; UI surfaces never receive credentials.

use std::{
    fmt,
    io::Read as _,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use aeris_platform_runtime::CancellableHttpClient;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest as _, Sha256};
use zeroize::{Zeroize as _, Zeroizing};

mod catalog;
mod dxlink;
pub use catalog::FutureInstrument;
pub use dxlink::{FeedVerification, verify_live_feed};

/// Broker authorization service; separate from Aeris account authentication.
pub const TASTYTRADE_BROKER_ORIGIN: &str = "https://app.aeristerminal.com";
const MAXIMUM_RESPONSE_BYTES: usize = 512 * 1024;

/// Desktop-held proof of ownership of a hosted broker connection.
#[derive(Serialize, Deserialize)]
pub struct ConnectionCapability {
    connection_id: String,
    proof: String,
}

impl fmt::Debug for ConnectionCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ConnectionCapability([redacted])")
    }
}

impl Drop for ConnectionCapability {
    fn drop(&mut self) {
        self.proof.zeroize();
    }
}

impl ConnectionCapability {
    /// Encodes a capability for protected native-vault storage only.
    ///
    /// # Errors
    /// Returns a redacted error if serialization fails.
    pub fn vault_bytes(&self) -> Result<Zeroizing<Vec<u8>>, String> {
        serde_json::to_vec(self)
            .map(Zeroizing::new)
            .map_err(|_| "Broker connection could not be encoded".to_string())
    }

    /// Restores a capability read from the protected native vault.
    ///
    /// # Errors
    /// Rejects malformed or oversized credentials.
    pub fn from_vault(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 1024 {
            return Err("Saved broker connection is oversized".to_string());
        }
        let result: Self = serde_json::from_slice(bytes)
            .map_err(|_| "Saved broker connection is invalid".to_string())?;
        if !valid_opaque(&result.connection_id) || !valid_opaque(&result.proof) {
            return Err("Saved broker connection is invalid".to_string());
        }
        Ok(result)
    }
}

fn valid_opaque(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Hosted authorization phase; contains no provider credentials.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AuthorizationPhase {
    Pending,
    Exchanging,
    Ready,
    Failed,
}

/// Sanitized status returned by the hosted connection owner.
#[derive(Clone, Debug, Deserialize)]
pub struct AuthorizationStatus {
    pub phase: AuthorizationPhase,
    pub expires_at: u64,
}

/// One new connection and the provider URL to open in the system browser.
pub struct PendingAuthorization {
    pub capability: ConnectionCapability,
    pub authorization_url: String,
    pub expires_at: u64,
}

/// Ephemeral streaming credentials; never publish them to the UI or logs.
#[derive(Deserialize)]
pub struct QuoteToken {
    pub token: String,
    pub dxlink_url: String,
    pub expires_at: String,
    pub level: String,
}

impl Drop for QuoteToken {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

/// Exclusive cancellable REST transport driven by a market-runtime worker.
#[derive(Default)]
pub struct TastytradeBrokerClient {
    http: CancellableHttpClient,
}

impl TastytradeBrokerClient {
    /// Creates a desktop-bound authorization transaction.
    ///
    /// # Errors
    /// Returns a redacted error for random-source, configuration or network failure.
    pub fn begin(&mut self, stop: &Arc<AtomicBool>) -> Result<PendingAuthorization, String> {
        #[derive(Deserialize)]
        struct Started {
            connection_id: String,
            authorization_url: String,
            expires_at: u64,
        }
        let mut random = Zeroizing::new([0_u8; 32]);
        getrandom::fill(random.as_mut())
            .map_err(|_| "System random source unavailable".to_string())?;
        let proof = Zeroizing::new(URL_SAFE_NO_PAD.encode(random.as_ref()));
        let digest = Sha256::digest(proof.as_bytes());
        let challenge: String = digest
            .iter()
            .flat_map(|byte| {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                [
                    char::from(HEX[usize::from(byte >> 4)]),
                    char::from(HEX[usize::from(byte & 15)]),
                ]
            })
            .collect();
        let result: Started = self.post(
            "start",
            &serde_json::json!({"proof_challenge": challenge}),
            stop,
        )?;
        if !valid_opaque(&result.connection_id)
            || !valid_authorization_url(&result.authorization_url)
        {
            return Err("Broker service returned an invalid authorization".to_string());
        }
        Ok(PendingAuthorization {
            capability: ConnectionCapability {
                connection_id: result.connection_id,
                proof: proof.to_string(),
            },
            authorization_url: result.authorization_url,
            expires_at: result.expires_at,
        })
    }

    /// Returns the phase of an existing desktop-owned connection.
    ///
    /// # Errors
    /// Returns a redacted error when the capability expired or the service is unavailable.
    pub fn status(
        &mut self,
        capability: &ConnectionCapability,
        stop: &Arc<AtomicBool>,
    ) -> Result<AuthorizationStatus, String> {
        self.post("status", capability, stop)
    }

    /// Deletes the hosted connection. Does not claim to revoke the provider grant.
    ///
    /// # Errors
    /// Returns a redacted error for service or network failure.
    pub fn disconnect(
        &mut self,
        capability: &ConnectionCapability,
        stop: &Arc<AtomicBool>,
    ) -> Result<(), String> {
        #[derive(Deserialize)]
        struct Disconnected {
            disconnected: bool,
        }
        let result: Disconnected = self.post("disconnect", capability, stop)?;
        if !result.disconnected {
            return Err("Broker connection was not disconnected".to_string());
        }
        Ok(())
    }

    /// Obtains a separate `DXLink` token using the hosted OAuth session.
    ///
    /// # Errors
    /// Returns a redacted error for invalid streaming fields or missing entitlement.
    pub fn quote_token(
        &mut self,
        capability: &ConnectionCapability,
        stop: &Arc<AtomicBool>,
    ) -> Result<QuoteToken, String> {
        let token: QuoteToken = self.post("quote_token", capability, stop)?;
        let expiry = chrono::DateTime::parse_from_rfc3339(&token.expires_at)
            .map_err(|_| "Broker quote-token expiry is invalid".to_string())?;
        if token.token.is_empty()
            || token.token.len() > 16_384
            || token.level.is_empty()
            || token.level.len() > 128
            || token.level.bytes().any(|byte| byte.is_ascii_control())
            || !valid_streamer_url(&token.dxlink_url)
            || u64::try_from(expiry.timestamp()).map_or(true, |expiry| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(true, |now| expiry <= now.as_secs())
            })
        {
            return Err("Broker quote-token response is invalid".to_string());
        }
        Ok(token)
    }

    /// Discovers contracts using provider-supplied streamer symbols.
    ///
    /// # Errors
    /// Rejects malformed catalogs, unsafe filters, or service errors.
    pub fn futures_for_product(
        &mut self,
        capability: &ConnectionCapability,
        product: &str,
        stop: &Arc<AtomicBool>,
    ) -> Result<Vec<FutureInstrument>, String> {
        #[derive(Serialize)]
        struct Request<'a> {
            #[serde(flatten)]
            capability: &'a ConnectionCapability,
            kind: &'static str,
            page: u32,
            product_code: &'a str,
        }
        if product.is_empty()
            || product.len() > 16
            || !product.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err("Futures product filter is invalid".to_string());
        }
        let page: catalog::CatalogPage = self.post(
            "instruments",
            &Request {
                capability,
                kind: "futures",
                page: 0,
                product_code: product,
            },
            stop,
        )?;
        page.validated(product)
    }

    fn post<T: DeserializeOwned>(
        &mut self,
        route: &str,
        payload: &impl Serialize,
        stop: &Arc<AtomicBool>,
    ) -> Result<T, String> {
        self.http.set_cancellation(stop);
        let raw = Zeroizing::new(
            serde_json::to_string(payload)
                .map_err(|_| "Broker request could not be encoded".to_string())?,
        );
        let mut response = self
            .http
            .agent()
            .post(format!(
                "{TASTYTRADE_BROKER_ORIGIN}/oauth/tastytrade/{route}"
            ))
            .config()
            .timeout_global(Some(Duration::from_secs(30)))
            .max_redirects(0)
            .build()
            .header("Content-Type", "application/json")
            .header(
                "User-Agent",
                concat!("aeris-terminal/", env!("CARGO_PKG_VERSION")),
            )
            .send(raw.as_bytes())
            .map_err(|error| request_error(&error))?;
        let mut bytes = Zeroizing::new(Vec::new());
        response
            .body_mut()
            .as_reader()
            .take(MAXIMUM_RESPONSE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Broker response could not be read".to_string())?;
        if bytes.len() > MAXIMUM_RESPONSE_BYTES {
            return Err("Broker response exceeded its size limit".to_string());
        }
        serde_json::from_slice(&bytes).map_err(|_| "Broker response is malformed".to_string())
    }
}

fn request_error(error: &ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(503) => {
            "Tastytrade application credentials must be configured on AWS first".to_string()
        }
        ureq::Error::StatusCode(401) => "Tastytrade connection expired; connect again".to_string(),
        ureq::Error::StatusCode(409 | 429) => {
            "Tastytrade connection service is busy; retry later".to_string()
        }
        ureq::Error::Timeout(_) => "Tastytrade connection request timed out".to_string(),
        ureq::Error::StatusCode(status) => {
            format!("Tastytrade connection request failed (HTTP {status})")
        }
        _ => "Tastytrade connection service could not be reached".to_string(),
    }
}

fn valid_authorization_url(url: &str) -> bool {
    url.len() <= 2048
        && url.starts_with("https://my.tastytrade.com/auth.html?")
        && !url.bytes().any(|byte| byte.is_ascii_control())
        && !url.contains('#')
}

fn valid_streamer_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("wss://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or_default();
    url.len() <= 2048
        && !host.contains(['@', ':', '?', '#'])
        && (host == "dxfeed.com" || host.ends_with(".dxfeed.com"))
        && !url.bytes().any(|byte| byte.is_ascii_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_capability_roundtrip_validates_and_redacts_proof() {
        let capability = ConnectionCapability {
            connection_id: "a".repeat(43),
            proof: "p".repeat(43),
        };
        let bytes = capability.vault_bytes().expect("encode");
        let restored = ConnectionCapability::from_vault(&bytes).expect("restore");
        assert_eq!(restored.proof, capability.proof);
        assert!(!format!("{restored:?}").contains(&capability.proof));
        assert!(ConnectionCapability::from_vault(br#"{"connection_id":"a","proof":"p"}"#).is_err());
        assert!(ConnectionCapability::from_vault(&vec![b'a'; 1025]).is_err());
    }

    #[test]
    fn provider_urls_cannot_redirect_credentials_to_another_origin() {
        assert!(valid_authorization_url(
            "https://my.tastytrade.com/auth.html?state=x"
        ));
        assert!(!valid_authorization_url(
            "https://my.tastytrade.com.attacker.example/auth.html?state=x"
        ));
        assert!(valid_streamer_url(
            "wss://tasty-openapi-ws.dxfeed.com/realtime"
        ));
        assert!(!valid_streamer_url(
            "wss://dxfeed.com@attacker.example/realtime"
        ));
        assert!(!valid_streamer_url(
            "ws://tasty-openapi-ws.dxfeed.com/realtime"
        ));
        assert!(!valid_streamer_url(
            "wss://dxfeed.com.attacker.example/realtime"
        ));
    }
}
