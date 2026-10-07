//! Proof-bound hosted access; tokens and application credentials never enter the vault.

use crate::session::{AccessToken, AppCredentials};
use aeris_platform_runtime::{
    CredentialVault, NativeCredentialVault,
    hosted_broker::{HostedBrokerClient, HostedBrokerConnection},
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

pub const CTRADER_VAULT_SERVICE: &str = "aeris.provider.ctrader";
pub const CTRADER_VAULT_KEY: &str = "broker_connection";
const REFRESH_MARGIN_SECONDS: u64 = 24 * 60 * 60;

/// # Errors
/// Returns an explicit error for an unreadable or malformed protected entry.
pub fn load_stored_connection() -> Result<Option<HostedBrokerConnection>, String> {
    let vault = NativeCredentialVault::new(CTRADER_VAULT_SERVICE)
        .map_err(|_| "Protected broker credential storage is unavailable".to_string())?;
    vault
        .load(CTRADER_VAULT_KEY)
        .map_err(|_| "Protected broker connection could not be loaded".to_string())?
        .map(|bytes| HostedBrokerConnection::from_vault(&Zeroizing::new(bytes)))
        .transpose()
}

pub struct CtraderHostedAccess {
    hosted: HostedBrokerClient,
    connection: Option<Zeroizing<Vec<u8>>>,
    access: Option<(AccessToken, u64)>,
    credentials: Option<AppCredentials>,
}

impl Default for CtraderHostedAccess {
    fn default() -> Self {
        Self::new()
    }
}

impl CtraderHostedAccess {
    #[must_use]
    pub fn new() -> Self {
        Self {
            hosted: HostedBrokerClient::new("ctrader"),
            connection: None,
            access: None,
            credentials: None,
        }
    }

    #[must_use]
    pub fn hosted(&mut self) -> &mut HostedBrokerClient {
        &mut self.hosted
    }

    fn ensure_connection(&mut self, connection: &HostedBrokerConnection) -> Result<(), String> {
        let form = connection.vault_bytes()?;
        if self
            .connection
            .as_ref()
            .is_none_or(|current| current.as_slice() != form.as_slice())
        {
            self.access = None;
            self.credentials = None;
            self.connection = Some(form);
        }
        Ok(())
    }

    /// # Errors
    /// Returns a redacted hosted or validation error.
    pub fn app_credentials(
        &mut self,
        connection: &HostedBrokerConnection,
        stop: &Arc<AtomicBool>,
    ) -> Result<&AppCredentials, String> {
        self.ensure_connection(connection)?;
        if self.credentials.is_none() {
            #[derive(Deserialize)]
            struct Response {
                client_id: String,
                client_secret: String,
            }
            let response: Response = self.hosted.post("app_credentials", connection, stop)?;
            let mut secret = Zeroizing::new(response.client_secret);
            if response.client_id.is_empty()
                || response.client_id.len() > 16_384
                || secret.is_empty()
                || secret.len() > 16_384
            {
                return Err("Broker application credentials response is invalid".into());
            }
            self.credentials = Some(AppCredentials {
                client_id: response.client_id,
                client_secret: std::mem::take(&mut *secret),
            });
        }
        self.credentials
            .as_ref()
            .ok_or_else(|| "Broker application credentials are unavailable".into())
    }

    pub fn clear_credentials(&mut self) {
        self.credentials = None;
    }

    /// # Errors
    /// Returns a redacted hosted or validation error.
    pub fn access_token(
        &mut self,
        connection: &HostedBrokerConnection,
        stop: &Arc<AtomicBool>,
        force_refresh: bool,
    ) -> Result<AccessToken, String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "System clock is invalid")?
            .as_secs();
        self.access_token_at(connection, stop, force_refresh, now)
    }

    fn access_token_at(
        &mut self,
        connection: &HostedBrokerConnection,
        stop: &Arc<AtomicBool>,
        force_refresh: bool,
        now: u64,
    ) -> Result<AccessToken, String> {
        self.ensure_connection(connection)?;
        if force_refresh
            || self
                .access
                .as_ref()
                .is_none_or(|(_, expiry)| *expiry <= now.saturating_add(REFRESH_MARGIN_SECONDS))
        {
            #[derive(Deserialize)]
            struct Response {
                access_token: String,
                expires_at: u64,
            }
            #[derive(Serialize)]
            struct TokenRequest<'a> {
                #[serde(flatten)]
                connection: &'a HostedBrokerConnection,
                force_refresh: bool,
            }
            let response: Response = self.hosted.post(
                "access_token",
                &TokenRequest {
                    connection,
                    force_refresh,
                },
                stop,
            )?;
            let mut token = Zeroizing::new(response.access_token);
            if token.is_empty() || token.len() > 16_384 || response.expires_at <= now {
                return Err("Broker access-token response is invalid".into());
            }
            self.access = Some((
                AccessToken(std::mem::take(&mut *token)),
                response.expires_at,
            ));
        }
        self.access
            .as_ref()
            .map(|(token, _)| AccessToken(token.0.clone()))
            .ok_or_else(|| "Broker access token is unavailable".into())
    }

    pub fn clear_access_token(&mut self) {
        self.access = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn scripted_hosted(expiry: u64) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for (route, force, body) in [
                (
                    "access_token",
                    false,
                    serde_json::json!({"access_token":"sample-one","expires_at":expiry})
                        .to_string(),
                ),
                (
                    "access_token",
                    false,
                    serde_json::json!({"access_token":"sample-two","expires_at":expiry + 100_000})
                        .to_string(),
                ),
                (
                    "access_token",
                    true,
                    serde_json::json!({"access_token":"sample-three","expires_at":expiry + 100_000})
                        .to_string(),
                ),
                (
                    "app_credentials",
                    false,
                    r#"{"client_id":"sample-app","client_secret":"sample-secret"}"#.to_string(),
                ),
            ] {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut data = Vec::new();
                let header_end = loop {
                    let mut chunk = [0; 4096];
                    let size = socket.read(&mut chunk).unwrap();
                    assert!(size > 0);
                    data.extend_from_slice(&chunk[..size]);
                    if let Some(pos) = data.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&data[..header_end]);
                assert!(headers.starts_with(&format!("POST /oauth/ctrader/{route} ")));
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("Content-Length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                while data.len() - header_end < length {
                    let mut chunk = [0; 4096];
                    let size = socket.read(&mut chunk).unwrap();
                    assert!(size > 0);
                    data.extend_from_slice(&chunk[..size]);
                }
                let request: serde_json::Value =
                    serde_json::from_slice(&data[header_end..header_end + length]).unwrap();
                assert_eq!(request["connection_id"], "a".repeat(43));
                assert_eq!(request["proof"], "b".repeat(43));
                if route == "access_token" {
                    assert_eq!(request["force_refresh"], force);
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).unwrap();
            }
        });
        (origin, server)
    }

    #[test]
    fn hosted_access_caches_until_margin_and_forced_refresh_bypasses_cache() {
        let connection = HostedBrokerConnection::from_vault(
            format!(
                r#"{{"connection_id":"{}","proof":"{}"}}"#,
                "a".repeat(43),
                "b".repeat(43)
            )
            .as_bytes(),
        )
        .unwrap();
        let expiry = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + REFRESH_MARGIN_SECONDS
            + 3600;
        let (origin, server) = scripted_hosted(expiry);
        let stop = Arc::new(AtomicBool::new(false));
        let mut access = CtraderHostedAccess::new();
        access.hosted().set_loopback_origin(&origin).unwrap();
        assert_eq!(
            access.access_token(&connection, &stop, false).unwrap().0,
            "sample-one"
        );
        assert_eq!(
            access.access_token(&connection, &stop, false).unwrap().0,
            "sample-one"
        );
        assert_eq!(
            access
                .access_token_at(&connection, &stop, false, expiry - REFRESH_MARGIN_SECONDS)
                .unwrap()
                .0,
            "sample-two"
        );
        assert_eq!(
            access.access_token(&connection, &stop, false).unwrap().0,
            "sample-two"
        );
        assert_eq!(
            access.access_token(&connection, &stop, true).unwrap().0,
            "sample-three"
        );
        let credentials = access.app_credentials(&connection, &stop).unwrap();
        assert_eq!(credentials.client_id, "sample-app");
        assert!(!format!("{credentials:?}").contains("sample-secret"));
        assert_eq!(
            access
                .app_credentials(&connection, &stop)
                .unwrap()
                .client_id,
            "sample-app"
        );
        server.join().unwrap();
    }

    #[test]
    fn changing_connection_discards_cached_secret_and_token() {
        let first = HostedBrokerConnection::from_vault(
            format!(
                r#"{{"connection_id":"{}","proof":"{}"}}"#,
                "a".repeat(43),
                "b".repeat(43)
            )
            .as_bytes(),
        )
        .unwrap();
        let second = HostedBrokerConnection::from_vault(
            format!(
                r#"{{"connection_id":"{}","proof":"{}"}}"#,
                "c".repeat(43),
                "d".repeat(43)
            )
            .as_bytes(),
        )
        .unwrap();
        let mut access = CtraderHostedAccess::new();
        access.ensure_connection(&first).unwrap();
        access.access = Some((AccessToken("sample".into()), u64::MAX));
        access.credentials = Some(AppCredentials {
            client_id: "sample".into(),
            client_secret: "sample".into(),
        });
        access.ensure_connection(&first).unwrap();
        assert!(access.access.is_some() && access.credentials.is_some());
        access.ensure_connection(&second).unwrap();
        assert!(access.access.is_none() && access.credentials.is_none());
    }
}
