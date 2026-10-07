//! Runtime-owned broker authorization, separate from Aeris account sign-in.

use super::{Command, MarketService, Reply, tastytrade::BrokerApi};
use aeris_ctrader_open_api_adapter::{
    host::CtraderHost,
    hosted::{CTRADER_VAULT_KEY, CTRADER_VAULT_SERVICE, CtraderHostedAccess},
    session::{AccessToken, AppCredentials, CtraderSession},
};
use aeris_platform_runtime::{
    CredentialVault, NativeCredentialVault,
    hosted_broker::{AuthorizationPhase, HostedBrokerClient, HostedBrokerConnection},
    open_system_browser,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

const VAULT_SERVICE: &str = "aeris.provider.tastytrade";
const VAULT_KEY: &str = "broker_connection";
const AUTHORIZATION_LIMIT: Duration = Duration::from_mins(10);
const POLL_INTERVAL: Duration = Duration::from_secs(3);
const WORKER_POLL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BrokerProvider {
    Tastytrade,
    Ctrader,
}
impl BrokerProvider {
    fn parse(name: &str) -> Result<Self, String> {
        match name {
            "tastytrade" => Ok(Self::Tastytrade),
            "ctrader" => Ok(Self::Ctrader),
            _ => Err("Provider does not support browser authorization".into()),
        }
    }
    fn slug(self) -> &'static str {
        match self {
            Self::Tastytrade => "tastytrade",
            Self::Ctrader => "ctrader",
        }
    }
    fn vault_service(self) -> &'static str {
        match self {
            Self::Tastytrade => VAULT_SERVICE,
            Self::Ctrader => CTRADER_VAULT_SERVICE,
        }
    }
    fn vault_key(self) -> &'static str {
        match self {
            Self::Tastytrade => VAULT_KEY,
            Self::Ctrader => CTRADER_VAULT_KEY,
        }
    }
}

#[derive(Clone, Copy)]
enum Operation {
    Connect,
    Disconnect,
}
struct BrokerCommand {
    provider: BrokerProvider,
    operation: Operation,
    reply: Reply<String>,
}

pub(super) struct BrokerAuthorization {
    commands: SyncSender<BrokerCommand>,
    busy: Arc<AtomicBool>,
}

impl BrokerAuthorization {
    pub(super) fn start(
        stop: &Arc<AtomicBool>,
        api: Arc<BrokerApi>,
    ) -> Result<(Self, thread::JoinHandle<()>), String> {
        let (commands, requests) = mpsc::sync_channel(1);
        let busy = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(stop);
        let worker_busy = Arc::clone(&busy);
        let worker = thread::Builder::new()
            .name("broker_authorization".to_string())
            .spawn(move || run(&requests, &worker_stop, &worker_busy, &api))
            .map_err(|_| "Broker authorization worker could not start".to_string())?;
        Ok((Self { commands, busy }, worker))
    }

    fn request(
        &self,
        provider: BrokerProvider,
        operation: Operation,
        stop: &AtomicBool,
    ) -> Result<String, String> {
        if stop.load(Ordering::Acquire) {
            return Err("Market runtime is shutting down".to_string());
        }
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "A broker connection operation is already running".to_string())?;
        let (reply, result) = mpsc::sync_channel(1);
        let command = BrokerCommand {
            provider,
            operation,
            reply,
        };
        if let Err(error) = self.commands.try_send(command) {
            self.busy.store(false, Ordering::Release);
            return Err(match error {
                TrySendError::Full(_) => "Broker authorization queue is full",
                TrySendError::Disconnected(_) => "Broker authorization worker is unavailable",
            }
            .to_string());
        }
        // Only the owner clears busy. A timed-out caller cannot enqueue a second
        // browser transaction while the first worker is still unwinding.
        result
            .recv_timeout(AUTHORIZATION_LIMIT + Duration::from_secs(65))
            .map_err(|_| "Broker authorization did not complete before its deadline".to_string())?
    }
}

impl MarketService {
    /// Connects a broker through the runtime-owned browser/vault workflow.
    /// Call from a background worker; this waits for browser authorization.
    ///
    /// # Errors
    /// Rejects unsupported providers, overload, or failed authorization/entitlement.
    pub fn connect_provider(&self, provider: &str) -> Result<String, String> {
        let provider = BrokerProvider::parse(provider)?;
        let result = self.runtime.broker_authorization.request(
            provider,
            Operation::Connect,
            &self.runtime.shutdown,
        )?;
        if provider == BrokerProvider::Tastytrade {
            self.request(|reply| Ok(Command::BrokerAuthorizationChanged(true, reply)))?;
            Ok("Tastytrade connected. Select an asset from the symbol menu.".into())
        } else {
            Ok(result)
        }
    }

    /// Reports whether a protected broker connection is stored for `provider`.
    /// Call from a background worker; this reads the native credential vault.
    /// A stored connection is reported even if the broker later rejects it; the
    /// market session surfaces that failure when it next connects.
    ///
    /// # Errors
    /// Rejects unsupported providers or an unreadable credential vault.
    pub fn provider_connected(&self, provider: &str) -> Result<bool, String> {
        let provider = BrokerProvider::parse(provider)?;
        let vault = NativeCredentialVault::new(provider.vault_service())
            .map_err(|_| "Protected broker credential storage is unavailable".to_string())?;
        Ok(load(&vault, provider.vault_key())?.is_some())
    }

    /// Deletes the hosted broker connection and its protected desktop capability.
    /// Call from a background worker. Provider grant revocation remains with the broker.
    ///
    /// # Errors
    /// Rejects unsupported providers, overload, or failed protected deletion.
    pub fn disconnect_provider(&self, provider: &str) -> Result<String, String> {
        let provider = BrokerProvider::parse(provider)?;
        if provider == BrokerProvider::Tastytrade {
            self.request(|reply| Ok(Command::BrokerAuthorizationChanged(false, reply)))?;
        }
        self.runtime.broker_authorization.request(
            provider,
            Operation::Disconnect,
            &self.runtime.shutdown,
        )
    }
}

fn run(
    requests: &Receiver<BrokerCommand>,
    stop: &Arc<AtomicBool>,
    busy: &AtomicBool,
    api: &BrokerApi,
) {
    while !stop.load(Ordering::Acquire) {
        let command = match requests.recv_timeout(WORKER_POLL) {
            Ok(command) => command,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let BrokerCommand {
            provider,
            operation,
            reply,
        } = command;
        let result = (if matches!(operation, Operation::Connect)
            && provider == BrokerProvider::Tastytrade
        {
            api.clear()
        } else {
            Ok(())
        })
        .and_then(|()| {
            NativeCredentialVault::new(provider.vault_service())
                .map_err(|_| "Protected broker credential storage is unavailable".to_string())
                .and_then(|vault| match (provider, operation) {
                    (BrokerProvider::Tastytrade, Operation::Disconnect) => {
                        disconnect(api, &vault, stop)
                    }
                    (BrokerProvider::Tastytrade, Operation::Connect) => connect(api, &vault, stop),
                    (BrokerProvider::Ctrader, Operation::Disconnect) => {
                        let mut hosted = HostedBrokerClient::new(provider.slug());
                        disconnect_ctrader(&vault, &mut hosted, stop)
                    }
                    (BrokerProvider::Ctrader, Operation::Connect) => {
                        let mut hosted = HostedBrokerClient::new(provider.slug());
                        connect_ctrader(
                            &vault,
                            &mut hosted,
                            stop,
                            open_system_browser,
                            verify_ctrader,
                            |stop| wait_until(Instant::now() + POLL_INTERVAL, stop),
                        )
                    }
                })
        });
        if result.is_ok()
            && matches!(operation, Operation::Disconnect)
            && provider == BrokerProvider::Tastytrade
            && let Err(error) = api.clear()
        {
            let _ = reply.send(Err(error));
            busy.store(false, Ordering::Release);
            continue;
        }
        busy.store(false, Ordering::Release);
        // A dropped request cannot change ownership or start another transaction.
        if reply.send(result).is_err() && stop.load(Ordering::Acquire) {
            break;
        }
    }
}

pub(super) fn load_connection() -> Result<HostedBrokerConnection, String> {
    let vault = NativeCredentialVault::new(VAULT_SERVICE)
        .map_err(|_| "Protected broker credential storage is unavailable".to_string())?;
    load(&vault, VAULT_KEY)?.ok_or_else(|| "Connect tastytrade to access market data".to_string())
}

fn load(vault: &impl CredentialVault, key: &str) -> Result<Option<HostedBrokerConnection>, String> {
    vault
        .load(key)
        .map_err(|_| "Protected broker connection could not be loaded".to_string())?
        .map(|bytes| HostedBrokerConnection::from_vault(&Zeroizing::new(bytes)))
        .transpose()
}

fn connect(
    api: &BrokerApi,
    vault: &NativeCredentialVault,
    stop: &Arc<AtomicBool>,
) -> Result<String, String> {
    if let Some(capability) = load(vault, VAULT_KEY)? {
        let status = api.with_client(stop, |client| client.hosted().status(&capability, stop))?;
        if status.phase == AuthorizationPhase::Ready {
            return verify_entitlement(api, &capability, stop);
        }
        // A prior pending transaction cannot be resumed safely without its browser
        // URL. Delete it before creating another transaction.
        api.with_client(stop, |client| client.hosted().disconnect(&capability, stop))?;
        delete_local(vault, VAULT_KEY)?;
    }
    let pending = api.with_client(stop, |client| client.hosted().begin(stop))?;
    if vault
        .store(VAULT_KEY, &pending.capability.vault_bytes()?)
        .is_err()
    {
        api.with_client(stop, |client| {
            client.hosted().disconnect(&pending.capability, stop)
        })?;
        return Err("Broker connection could not be saved in protected storage".to_string());
    }
    if open_system_browser(&pending.authorization_url).is_err() {
        return Err(
            "Could not open tastytrade authorization in your browser; disconnect and retry"
                .to_string(),
        );
    }
    let deadline = Instant::now() + AUTHORIZATION_LIMIT;
    loop {
        wait_until(Instant::now() + POLL_INTERVAL, stop)?;
        if Instant::now() >= deadline || unix_seconds()? >= pending.expires_at {
            return Err("Tastytrade login expired; disconnect and connect again".to_string());
        }
        match api
            .with_client(stop, |client| {
                client.hosted().status(&pending.capability, stop)
            })?
            .phase
        {
            AuthorizationPhase::Pending | AuthorizationPhase::Exchanging => {}
            AuthorizationPhase::Ready => {
                return verify_entitlement(api, &pending.capability, stop);
            }
            AuthorizationPhase::Failed => {
                return Err("Tastytrade authorization failed; disconnect and retry".to_string());
            }
        }
    }
}

fn connect_ctrader(
    vault: &impl CredentialVault,
    hosted: &mut HostedBrokerClient,
    stop: &Arc<AtomicBool>,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
    verify: impl FnOnce(&HostedBrokerConnection, &Arc<AtomicBool>) -> Result<String, String>,
    mut wait: impl FnMut(&AtomicBool) -> Result<(), String>,
) -> Result<String, String> {
    if let Some(capability) = load(vault, CTRADER_VAULT_KEY)? {
        if hosted.status(&capability, stop)?.phase == AuthorizationPhase::Ready {
            return verify(&capability, stop);
        }
        hosted.disconnect(&capability, stop)?;
        delete_local(vault, CTRADER_VAULT_KEY)?;
    }
    let pending = hosted.begin(stop)?;
    open_browser(&pending.authorization_url)
        .map_err(|_| "Could not open cTrader authorization in your browser; retry".to_string())?;
    let deadline = Instant::now() + AUTHORIZATION_LIMIT;
    loop {
        wait(stop)?;
        if Instant::now() >= deadline || unix_seconds()? >= pending.expires_at {
            return Err("cTrader login expired; connect again".into());
        }
        match hosted.status(&pending.capability, stop)?.phase {
            AuthorizationPhase::Pending | AuthorizationPhase::Exchanging => {}
            AuthorizationPhase::Ready => {
                let result = verify(&pending.capability, stop)?;
                vault
                    .store(CTRADER_VAULT_KEY, &pending.capability.vault_bytes()?)
                    .map_err(|_| "Broker connection could not be saved in protected storage")?;
                return Ok(result);
            }
            AuthorizationPhase::Failed => {
                return Err("cTrader authorization failed; connect again".into());
            }
        }
    }
}

fn verify_ctrader(
    capability: &HostedBrokerConnection,
    stop: &Arc<AtomicBool>,
) -> Result<String, String> {
    let mut access = CtraderHostedAccess::new();
    verify_ctrader_with(&mut access, capability, stop, |credentials, token| {
        let mut session =
            CtraderSession::open(CtraderHost::Demo, credentials, token, Arc::clone(stop))
                .map_err(|error| error.to_string())?;
        let counts = account_counts(session.accounts().iter().map(|account| account.is_live));
        session.close();
        Ok(counts)
    })
}

fn account_counts(accounts: impl Iterator<Item = bool>) -> (usize, usize) {
    accounts.fold((0, 0), |(demo, live), is_live| {
        if is_live {
            (demo, live + 1)
        } else {
            (demo + 1, live)
        }
    })
}

fn verify_ctrader_with(
    access: &mut CtraderHostedAccess,
    capability: &HostedBrokerConnection,
    stop: &Arc<AtomicBool>,
    open_demo: impl FnOnce(&AppCredentials, AccessToken) -> Result<(usize, usize), String>,
) -> Result<String, String> {
    let token = access.access_token(capability, stop, false)?;
    let credentials = access.app_credentials(capability, stop)?;
    let (demo, live) = open_demo(credentials, token)?;
    Ok(format!(
        "cTrader connected. Demo accounts: {demo}; live accounts: {live}."
    ))
}

fn disconnect_ctrader(
    vault: &impl CredentialVault,
    hosted: &mut HostedBrokerClient,
    stop: &Arc<AtomicBool>,
) -> Result<String, String> {
    if let Some(capability) = load(vault, CTRADER_VAULT_KEY)? {
        let remote = hosted.disconnect(&capability, stop);
        let local = delete_local(vault, CTRADER_VAULT_KEY);
        local?;
        remote?;
    }
    Ok("cTrader connection removed.".into())
}

fn verify_entitlement(
    api: &BrokerApi,
    capability: &HostedBrokerConnection,
    stop: &Arc<AtomicBool>,
) -> Result<String, String> {
    let _token = api.with_client(stop, |client| client.quote_token(capability, stop))?;
    let _futures = api.futures(stop)?;
    // This checks the actual API entitlement, not the brokerage website display.
    // No feed/order-book claim is made before the DXLink live path is verified.
    Ok("Tastytrade connected. Open the symbol menu and select a tastytrade asset.".to_string())
}

fn disconnect(
    api: &BrokerApi,
    vault: &NativeCredentialVault,
    stop: &Arc<AtomicBool>,
) -> Result<String, String> {
    if let Some(capability) = load(vault, VAULT_KEY)? {
        // Keep the local proof until server deletion succeeds, including after an
        // uncertain response, so deletion can be retried without orphaning tokens.
        api.with_client(stop, |client| client.hosted().disconnect(&capability, stop))?;
        delete_local(vault, VAULT_KEY)?;
    }
    Ok("Tastytrade connection removed. You can revoke Aeris access in tastytrade's authorized applications.".to_string())
}

fn delete_local(vault: &impl CredentialVault, key: &str) -> Result<(), String> {
    vault
        .delete(key)
        .map_err(|_| "Protected broker connection could not be deleted".to_string())
}

fn unix_seconds() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| "System clock is invalid".to_string())
}

fn wait_until(deadline: Instant, stop: &AtomicBool) -> Result<(), String> {
    loop {
        if stop.load(Ordering::Acquire) {
            return Err("Broker authorization cancelled during shutdown".to_string());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        thread::sleep(remaining.min(WORKER_POLL));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::Mutex,
    };

    #[derive(Default)]
    struct MemoryVault(Mutex<Option<Vec<u8>>>);
    impl CredentialVault for MemoryVault {
        type Error = ();
        fn store(&self, key: &str, bytes: &[u8]) -> Result<(), ()> {
            assert_eq!(key, CTRADER_VAULT_KEY);
            *self.0.lock().unwrap() = Some(bytes.to_vec());
            Ok(())
        }
        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, ()> {
            assert_eq!(key, CTRADER_VAULT_KEY);
            Ok(self.0.lock().unwrap().clone())
        }
        fn delete(&self, key: &str) -> Result<(), ()> {
            assert_eq!(key, CTRADER_VAULT_KEY);
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    fn scripted_hosted(
        replies: Vec<(&'static str, u16, String)>,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for (route, status, body) in replies {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut received = Vec::new();
                let header_end;
                loop {
                    let mut bytes = [0; 4096];
                    let count = socket.read(&mut bytes).unwrap();
                    assert!(count != 0);
                    received.extend_from_slice(&bytes[..count]);
                    if let Some(index) =
                        received.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        header_end = index + 4;
                        break;
                    }
                }
                let headers = String::from_utf8_lossy(&received[..header_end]);
                assert!(headers.starts_with(&format!("POST /oauth/ctrader/{route} ")));
                let content_length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                while received.len() - header_end < content_length {
                    let mut bytes = [0; 4096];
                    let count = socket.read(&mut bytes).unwrap();
                    assert!(count != 0);
                    received.extend_from_slice(&bytes[..count]);
                }
                let request: serde_json::Value =
                    serde_json::from_slice(&received[header_end..header_end + content_length])
                        .unwrap();
                if route != "start" {
                    assert_eq!(request["connection_id"], "a".repeat(43));
                    assert_eq!(request["proof"].as_str().unwrap().len(), 43);
                }
                let reply = format!(
                    "HTTP/1.1 {status} Scripted\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(reply.as_bytes()).unwrap();
            }
        });
        (origin, server)
    }

    fn stored_capability() -> HostedBrokerConnection {
        HostedBrokerConnection::from_vault(
            format!(
                r#"{{"connection_id":"{}","proof":"{}"}}"#,
                "a".repeat(43),
                "b".repeat(43)
            )
            .as_bytes(),
        )
        .unwrap()
    }

    #[test]
    fn broker_provider_routes_and_vault_names_are_scoped() {
        for (provider, service, slug) in [
            (
                BrokerProvider::Tastytrade,
                "aeris.provider.tastytrade",
                "tastytrade",
            ),
            (BrokerProvider::Ctrader, "aeris.provider.ctrader", "ctrader"),
        ] {
            assert_eq!(provider.vault_service(), service);
            assert_eq!(provider.vault_key(), "broker_connection");
            assert_eq!(provider.slug(), slug);
        }
    }

    #[test]
    fn ctrader_connect_waits_for_ready_and_demo_verification_before_storing_only_proof() {
        let expiry = unix_seconds().unwrap() + 600;
        let url = format!(
            "https://id.ctrader.com/my/settings/openapi/grantingaccess/?state={}",
            "a".repeat(43)
        );
        let (origin, server) = scripted_hosted(vec![
            ("start", 201, serde_json::json!({"connection_id": "a".repeat(43), "authorization_url": url, "expires_at": expiry}).to_string()),
            ("status", 200, serde_json::json!({"phase":"pending","expires_at":expiry}).to_string()),
            ("status", 200, serde_json::json!({"phase":"ready","expires_at":expiry}).to_string()),
            ("access_token", 200, serde_json::json!({"access_token":"sample-access","expires_at":expiry + 100_000}).to_string()),
            ("app_credentials", 200, r#"{"client_id":"sample-client","client_secret":"sample-secret"}"#.into()),
        ]);
        let mut hosted = HostedBrokerClient::new("ctrader");
        hosted.set_loopback_origin(&origin).unwrap();
        let mut access = CtraderHostedAccess::new();
        access.hosted().set_loopback_origin(&origin).unwrap();
        let vault = MemoryVault::default();
        let stop = Arc::new(AtomicBool::new(false));
        let mut opened = 0;
        let result = connect_ctrader(
            &vault,
            &mut hosted,
            &stop,
            |url| {
                opened += 1;
                assert!(url.starts_with("https://id.ctrader.com/"));
                assert!(vault.0.lock().unwrap().is_none());
                Ok(())
            },
            |connection, stop| {
                assert!(vault.0.lock().unwrap().is_none());
                verify_ctrader_with(&mut access, connection, stop, |credentials, token| {
                    assert_eq!(credentials.client_id, "sample-client");
                    assert_eq!(token.0, "sample-access");
                    Ok(account_counts([false, false, true].into_iter()))
                })
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(opened, 1);
        assert_eq!(
            result,
            "cTrader connected. Demo accounts: 2; live accounts: 1."
        );
        let stored = vault.0.lock().unwrap().clone().unwrap();
        assert!(stored.len() <= 1024);
        let value: serde_json::Value = serde_json::from_slice(&stored).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 2);
        assert_eq!(value["connection_id"], "a".repeat(43));
        assert_eq!(value["proof"].as_str().unwrap().len(), 43);
        server.join().unwrap();
    }

    #[test]
    fn ctrader_failed_or_expired_status_does_not_store() {
        for phase in ["failed", "pending"] {
            let expiry = unix_seconds().unwrap() + 600;
            let (origin, server) = scripted_hosted(vec![
                ("start", 201, serde_json::json!({"connection_id":"a".repeat(43), "authorization_url":format!("https://id.ctrader.com/my/settings/openapi/grantingaccess/?state={}", "a".repeat(43)), "expires_at":expiry}).to_string()),
                ("status", 200, serde_json::json!({"phase":phase,"expires_at":expiry}).to_string()),
            ]);
            let mut hosted = HostedBrokerClient::new("ctrader");
            hosted.set_loopback_origin(&origin).unwrap();
            let vault = MemoryVault::default();
            let stop = Arc::new(AtomicBool::new(false));
            let mut waits = 0;
            let error = connect_ctrader(
                &vault,
                &mut hosted,
                &stop,
                |_| Ok(()),
                |_, _| panic!("not ready"),
                |_| {
                    waits += 1;
                    if phase == "pending" && waits > 1 {
                        Err("Authorization deadline expired".into())
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(error.is_err());
            assert!(vault.0.lock().unwrap().is_none());
            server.join().unwrap();
        }
    }

    #[test]
    fn ctrader_disconnect_deletes_local_even_when_hosted_fails() {
        for status in [200, 500] {
            let (origin, server) = scripted_hosted(vec![(
                "disconnect",
                status,
                if status == 200 {
                    r#"{"disconnected":true}"#
                } else {
                    r#"{"error":"unavailable"}"#
                }
                .into(),
            )]);
            let vault = MemoryVault::default();
            vault
                .store(
                    CTRADER_VAULT_KEY,
                    &stored_capability().vault_bytes().unwrap(),
                )
                .unwrap();
            let mut hosted = HostedBrokerClient::new("ctrader");
            hosted.set_loopback_origin(&origin).unwrap();
            assert_eq!(
                disconnect_ctrader(&vault, &mut hosted, &Arc::new(AtomicBool::new(false))).is_ok(),
                status == 200
            );
            assert!(vault.0.lock().unwrap().is_none());
            server.join().unwrap();
        }
    }

    #[test]
    fn ctrader_ready_without_demo_entitlement_never_writes_vault() {
        let expiry = unix_seconds().unwrap() + 600;
        let (origin, server) = scripted_hosted(vec![
            (
                "start",
                201,
                serde_json::json!({
                    "connection_id": "a".repeat(43),
                    "authorization_url": format!(
                        "https://id.ctrader.com/my/settings/openapi/grantingaccess/?state={}",
                        "a".repeat(43)
                    ),
                    "expires_at": expiry
                })
                .to_string(),
            ),
            (
                "status",
                200,
                serde_json::json!({"phase":"ready","expires_at":expiry}).to_string(),
            ),
        ]);
        let mut hosted = HostedBrokerClient::new("ctrader");
        hosted.set_loopback_origin(&origin).unwrap();
        let vault = MemoryVault::default();
        let result = connect_ctrader(
            &vault,
            &mut hosted,
            &Arc::new(AtomicBool::new(false)),
            |_| Ok(()),
            |_, _| Err("cTrader session: Protocol".into()),
            |_| Ok(()),
        );
        assert!(result.unwrap_err().contains("Protocol"));
        assert!(vault.0.lock().unwrap().is_none());
        server.join().unwrap();
    }

    #[test]
    fn authorization_queue_rejects_duplicate_work_and_shutdown() {
        let (commands, _requests) = mpsc::sync_channel(1);
        let owner = BrokerAuthorization {
            commands,
            busy: Arc::new(AtomicBool::new(true)),
        };
        assert!(
            owner
                .request(
                    BrokerProvider::Tastytrade,
                    Operation::Connect,
                    &AtomicBool::new(false)
                )
                .unwrap_err()
                .contains("already running")
        );
        assert!(
            owner
                .request(
                    BrokerProvider::Tastytrade,
                    Operation::Connect,
                    &AtomicBool::new(true)
                )
                .unwrap_err()
                .contains("shutting down")
        );
    }

    #[test]
    fn pending_browser_wait_observes_shutdown() {
        assert!(wait_until(Instant::now() + POLL_INTERVAL, &AtomicBool::new(true)).is_err());
    }
}
