//! Provider-neutral hosted broker connection ownership and HTTP authorization.

use std::{
    fmt,
    io::Read as _,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest as _, Sha256};
use zeroize::{Zeroize as _, Zeroizing};

use crate::CancellableHttpClient;

pub const HOSTED_BROKER_ORIGIN: &str = "https://app.aeristerminal.com";
const MAXIMUM_RESPONSE_BYTES: usize = 512 * 1024;
const MAXIMUM_RETRIES: u32 = 4;

/// Desktop-held proof of ownership of a hosted broker connection.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedBrokerConnection {
    connection_id: String,
    proof: String,
}

impl fmt::Debug for HostedBrokerConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HostedBrokerConnection([redacted])")
    }
}

impl Drop for HostedBrokerConnection {
    fn drop(&mut self) {
        self.connection_id.zeroize();
        self.proof.zeroize();
    }
}

impl HostedBrokerConnection {
    /// Encodes only the opaque connection id and proof for protected native-vault storage.
    ///
    /// # Errors
    /// Rejects a malformed or oversized connection.
    pub fn vault_bytes(&self) -> Result<Zeroizing<Vec<u8>>, String> {
        if !valid_opaque(&self.connection_id) || !valid_opaque(&self.proof) {
            return Err("Broker connection is invalid".into());
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|_| "Broker connection could not be encoded".to_string())?;
        if bytes.len() > 1024 {
            return Err("Broker connection is oversized".into());
        }
        Ok(Zeroizing::new(bytes))
    }

    /// Restores and validates a protected native-vault entry.
    ///
    /// # Errors
    /// Rejects malformed or oversized saved credentials.
    pub fn from_vault(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 1024 {
            return Err("Saved broker connection is oversized".into());
        }
        let result: Self = serde_json::from_slice(bytes)
            .map_err(|_| "Saved broker connection is invalid".to_string())?;
        if !valid_opaque(&result.connection_id) || !valid_opaque(&result.proof) {
            return Err("Saved broker connection is invalid".into());
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AuthorizationPhase {
    Pending,
    Exchanging,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AuthorizationStatus {
    pub phase: AuthorizationPhase,
    pub expires_at: u64,
}

pub struct PendingAuthorization {
    pub capability: HostedBrokerConnection,
    pub authorization_url: String,
    pub expires_at: u64,
}

/// Hosted HTTP session shared by the authorization and provider-access paths.
pub struct HostedBrokerClient {
    provider_slug: String,
    http: CancellableHttpClient,
    origin: String,
}

impl HostedBrokerClient {
    #[must_use]
    pub fn new(provider_slug: &str) -> Self {
        Self {
            provider_slug: provider_slug.to_owned(),
            http: CancellableHttpClient::default(),
            origin: HOSTED_BROKER_ORIGIN.to_owned(),
        }
    }

    /// Restricts a locally scripted test service to the loopback interface.
    /// Production callers use `new`, which always uses the hosted HTTPS origin.
    ///
    /// # Errors
    /// Rejects non-loopback or malformed origins.
    #[doc(hidden)]
    pub fn set_loopback_origin(&mut self, origin: &str) -> Result<(), String> {
        let port = origin
            .strip_prefix("http://127.0.0.1:")
            .and_then(|port| port.parse::<u16>().ok())
            .filter(|port| *port != 0)
            .ok_or("Broker test origin must be loopback")?;
        self.origin = format!("http://127.0.0.1:{port}");
        Ok(())
    }

    /// Begins a proof-bound hosted connection.
    ///
    /// # Errors
    /// Returns a redacted error on invalid input or service failure.
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
        let started: Started = self.post(
            "start",
            &serde_json::json!({"proof_challenge": challenge}),
            stop,
        )?;
        if !valid_opaque(&started.connection_id)
            || !valid_authorization_url(&self.provider_slug, &started.authorization_url)
        {
            return Err("Broker service returned an invalid authorization".into());
        }
        Ok(PendingAuthorization {
            capability: HostedBrokerConnection {
                connection_id: started.connection_id,
                proof: proof.to_string(),
            },
            authorization_url: started.authorization_url,
            expires_at: started.expires_at,
        })
    }

    /// Returns the phase of a connection without exposing the proof.
    ///
    /// # Errors
    /// Returns a redacted error on service failure.
    pub fn status(
        &mut self,
        capability: &HostedBrokerConnection,
        stop: &Arc<AtomicBool>,
    ) -> Result<AuthorizationStatus, String> {
        self.post("status", capability, stop)
    }

    /// Deletes the hosted connection, without claiming to revoke the broker grant.
    ///
    /// # Errors
    /// Returns a redacted error on service failure.
    pub fn disconnect(
        &mut self,
        capability: &HostedBrokerConnection,
        stop: &Arc<AtomicBool>,
    ) -> Result<(), String> {
        #[derive(Deserialize)]
        struct Disconnected {
            disconnected: bool,
        }
        let response: Disconnected = self.post("disconnect", capability, stop)?;
        if !response.disconnected {
            return Err("Broker connection was not disconnected".into());
        }
        Ok(())
    }

    /// Sends a proof-authenticated or proof-challenge request to a provider route.
    ///
    /// # Errors
    /// Rejects invalid routes and providers, cancelled work, persistent overload and
    /// malformed or oversized responses without disclosing request bodies.
    pub fn post<T: DeserializeOwned>(
        &mut self,
        route: &str,
        body: &impl Serialize,
        stop: &Arc<AtomicBool>,
    ) -> Result<T, String> {
        if !matches!(self.provider_slug.as_str(), "tastytrade" | "ctrader")
            || !matches!(
                route,
                "start" | "status" | "disconnect" | "access_token" | "app_credentials"
            )
        {
            return Err("Broker connection route is invalid".into());
        }
        self.http.set_cancellation(stop);
        let raw = Zeroizing::new(
            serde_json::to_string(body)
                .map_err(|_| "Broker request could not be encoded".to_string())?,
        );
        for attempt in 0..=MAXIMUM_RETRIES {
            if stop.load(Ordering::Acquire) {
                return Err("Broker connection request cancelled".into());
            }
            let response = self
                .http
                .agent()
                .post(format!(
                    "{}/oauth/{}/{route}",
                    self.origin, self.provider_slug
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
                .send(raw.as_bytes());
            let mut response = match response {
                Ok(response) => response,
                Err(ureq::Error::StatusCode(409 | 429)) if attempt < MAXIMUM_RETRIES => {
                    let mut jitter = [0_u8; 1];
                    getrandom::fill(&mut jitter).map_err(|_| "System random source unavailable")?;
                    let until = std::time::Instant::now()
                        + Duration::from_millis((100_u64 << attempt) + u64::from(jitter[0]));
                    while std::time::Instant::now() < until {
                        if stop.load(Ordering::Acquire) {
                            return Err("Broker connection request cancelled".into());
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    continue;
                }
                Err(error) => return Err(request_error(&error, &self.provider_slug)),
            };
            let mut bytes = Zeroizing::new(Vec::new());
            response
                .body_mut()
                .as_reader()
                .take(MAXIMUM_RESPONSE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "Broker response could not be read".to_string())?;
            if bytes.len() > MAXIMUM_RESPONSE_BYTES {
                return Err("Broker response exceeded its size limit".into());
            }
            return serde_json::from_slice(&bytes)
                .map_err(|_| "Broker response is malformed".into());
        }
        Err("Broker connection service is busy; retry later".into())
    }
}

fn request_error(error: &ureq::Error, provider_slug: &str) -> String {
    // Existing tastytrade diagnostics are user-visible in the connection workflow.
    // Keep their wording stable while the new provider uses neutral text.
    let label = if provider_slug == "tastytrade" {
        "Tastytrade"
    } else {
        "Broker"
    };
    match error {
        ureq::Error::StatusCode(503) => {
            format!("{label} application credentials must be configured on AWS first")
        }
        ureq::Error::StatusCode(401) => format!("{label} connection expired; connect again"),
        ureq::Error::StatusCode(404) => "Broker connection expired; connect again".into(),
        ureq::Error::StatusCode(409 | 429) => {
            format!("{label} connection service is busy; retry later")
        }
        ureq::Error::Timeout(_) => format!("{label} connection request timed out"),
        ureq::Error::StatusCode(status) => {
            format!("{label} connection request failed (HTTP {status})")
        }
        _ => format!("{label} connection service could not be reached"),
    }
}

fn valid_authorization_url(provider_slug: &str, url: &str) -> bool {
    let prefix = match provider_slug {
        "tastytrade" => "https://my.tastytrade.com/auth.html?",
        "ctrader" => "https://id.ctrader.com/my/settings/openapi/grantingaccess/?",
        _ => return false,
    };
    url.len() <= 2048
        && url.starts_with(prefix)
        && !url.bytes().any(|byte| byte.is_ascii_control())
        && !url.contains('#')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write as _,
        net::TcpListener,
        sync::{Arc, atomic::AtomicBool},
        thread,
    };

    #[test]
    fn protected_capability_roundtrip_validates_and_redacts_proof() {
        let capability = HostedBrokerConnection {
            connection_id: "a".repeat(43),
            proof: "p".repeat(43),
        };
        let bytes = capability.vault_bytes().expect("encode");
        let restored = HostedBrokerConnection::from_vault(&bytes).expect("restore");
        assert_eq!(restored.proof, capability.proof);
        assert!(!format!("{restored:?}").contains(&capability.proof));
        assert!(
            HostedBrokerConnection::from_vault(br#"{"connection_id":"a","proof":"p"}"#).is_err()
        );
        assert!(HostedBrokerConnection::from_vault(&vec![b'a'; 1025]).is_err());
    }

    #[test]
    fn hosted_broker_authorization_urls_reject_redirects() {
        assert!(valid_authorization_url(
            "tastytrade",
            "https://my.tastytrade.com/auth.html?state=x"
        ));
        assert!(!valid_authorization_url(
            "tastytrade",
            "https://my.tastytrade.com.attacker.example/auth.html?state=x"
        ));
        assert!(valid_authorization_url(
            "ctrader",
            "https://id.ctrader.com/my/settings/openapi/grantingaccess/?state=x"
        ));
        assert!(!valid_authorization_url(
            "ctrader",
            "https://id.ctrader.com.attacker.example/my/settings/openapi/grantingaccess/?state=x"
        ));
    }

    #[test]
    fn hosted_broker_vault_roundtrip_rejects_malformed_values() {
        let connection = HostedBrokerConnection::from_vault(
            format!(
                r#"{{"connection_id":"{}","proof":"{}"}}"#,
                "a".repeat(43),
                "b".repeat(43)
            )
            .as_bytes(),
        )
        .expect("valid opaque connection");
        let bytes = connection.vault_bytes().expect("encode");
        assert!(bytes.len() <= 1024);
        assert!(HostedBrokerConnection::from_vault(&bytes).is_ok());
        for invalid in [
            br#"{"connection_id":"","proof":"x"}"#.as_slice(),
            br#"{"connection_id":"bad!","proof":"bad!"}"#,
            br#"{"connection_id":"missing"}"#,
            br"not json",
        ] {
            assert!(HostedBrokerConnection::from_vault(invalid).is_err());
        }
        for (id, proof) in [
            ("a".repeat(44), "b".repeat(43)),
            ("a".repeat(43), "b".repeat(44)),
        ] {
            let value = format!(r#"{{"connection_id":"{id}","proof":"{proof}"}}"#);
            assert!(HostedBrokerConnection::from_vault(value.as_bytes()).is_err());
        }
        let extra = format!(
            r#"{{"connection_id":"{}","proof":"{}","access_token":"unexpected"}}"#,
            "a".repeat(43),
            "b".repeat(43)
        );
        assert!(HostedBrokerConnection::from_vault(extra.as_bytes()).is_err());
        assert!(HostedBrokerConnection::from_vault(&vec![b'a'; 1025]).is_err());
    }

    #[test]
    fn hosted_broker_debug_redacts_both_fields() {
        let id = "a".repeat(43);
        let proof = "b".repeat(43);
        let value = format!(r#"{{"connection_id":"{id}","proof":"{proof}"}}"#);
        let connection = HostedBrokerConnection::from_vault(value.as_bytes()).expect("valid");
        let debug = format!("{connection:?}");
        assert!(!debug.contains(&id) && !debug.contains(&proof));
    }

    fn mock_service(path: &'static str, statuses: Vec<u16>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let origin = format!("http://{}", listener.local_addr().expect("local address"));
        let server = thread::spawn(move || {
            for status in statuses {
                let (mut stream, _) = listener.accept().expect("accept fixture request");
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .expect("bounded fixture read");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                    let size = stream.read(&mut buffer).expect("read headers");
                    assert!(size > 0 && request.len() + size < 4096);
                    request.extend_from_slice(&buffer[..size]);
                }
                let header_end = request
                    .windows(4)
                    .position(|part| part == b"\r\n\r\n")
                    .expect("headers")
                    + 4;
                let body_length = String::from_utf8_lossy(&request[..header_end])
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|value| value.parse::<usize>().ok())
                    })
                    .expect("bounded content length");
                while request.len() < header_end + body_length {
                    let size = stream.read(&mut buffer).expect("read body");
                    assert!(size > 0 && request.len() + size < 4096);
                    request.extend_from_slice(&buffer[..size]);
                }
                let first = String::from_utf8_lossy(&request);
                assert!(first.starts_with(&format!("POST {path} HTTP/1.1")));
                let body = r#"{"phase":"ready","expires_at":100}"#;
                write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("fixture response");
            }
        });
        (origin, server)
    }

    #[test]
    fn hosted_broker_routes_are_provider_scoped() {
        for slug in ["tastytrade", "ctrader"] {
            let path = if slug == "ctrader" {
                "/oauth/ctrader/status"
            } else {
                "/oauth/tastytrade/status"
            };
            let (origin, server) = mock_service(path, vec![200]);
            let mut client = HostedBrokerClient::new(slug);
            client
                .set_loopback_origin(&origin)
                .expect("loopback fixture");
            let status: AuthorizationStatus = client
                .post(
                    "status",
                    &serde_json::json!({}),
                    &Arc::new(AtomicBool::new(false)),
                )
                .expect("status");
            assert_eq!(status.phase, AuthorizationPhase::Ready);
            server.join().expect("fixture completed");
        }
    }

    #[test]
    fn hosted_broker_retries_conflicts_and_rate_limits_at_most_four_times() {
        for status in [409, 429] {
            let (origin, server) = mock_service(
                "/oauth/ctrader/status",
                vec![status, status, status, status, status],
            );
            let mut client = HostedBrokerClient::new("ctrader");
            client
                .set_loopback_origin(&origin)
                .expect("loopback fixture");
            let error = client
                .post::<AuthorizationStatus>(
                    "status",
                    &serde_json::json!({}),
                    &Arc::new(AtomicBool::new(false)),
                )
                .expect_err("bounded retry");
            assert!(error.contains("busy"), "{error}");
            server.join().expect("exactly five fixture requests");
        }
    }

    #[test]
    fn hosted_broker_recovers_from_transient_overload() {
        let (origin, server) = mock_service("/oauth/ctrader/status", vec![409, 429, 200]);
        let mut client = HostedBrokerClient::new("ctrader");
        client
            .set_loopback_origin(&origin)
            .expect("loopback fixture");
        let response: AuthorizationStatus = client
            .post(
                "status",
                &serde_json::json!({}),
                &Arc::new(AtomicBool::new(false)),
            )
            .expect("two transient failures recover");
        assert_eq!(response.phase, AuthorizationPhase::Ready);
        server.join().expect("three requests received");
    }

    #[test]
    fn hosted_broker_rejects_invalid_route_or_origin() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut client = HostedBrokerClient::new("ctrader");
        assert!(
            client
                .set_loopback_origin("http://example.com:1234")
                .is_err()
        );
        assert!(
            client
                .set_loopback_origin("http://127.0.0.1:80/escape")
                .is_err()
        );
        assert!(
            client
                .post::<AuthorizationStatus>("../status", &serde_json::json!({}), &stop)
                .is_err()
        );
        let mut unknown = HostedBrokerClient::new("other");
        assert!(
            unknown
                .post::<AuthorizationStatus>("status", &serde_json::json!({}), &stop)
                .is_err()
        );
    }
}
