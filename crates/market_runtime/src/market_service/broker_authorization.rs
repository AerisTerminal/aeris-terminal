//! Runtime-owned broker authorization, separate from Aeris account sign-in.

use super::{MarketService, Reply};
use aeris_platform_runtime::{CredentialVault, NativeCredentialVault, open_system_browser};
use aeris_tastytrade_market_adapter::{
    AuthorizationPhase, ConnectionCapability, TastytradeBrokerClient,
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

#[derive(Clone, Copy)]
enum Operation {
    Connect,
    Disconnect,
    VerifyFeed,
}
struct BrokerCommand {
    operation: Operation,
    reply: Reply<String>,
}

pub(super) struct BrokerAuthorization {
    commands: SyncSender<BrokerCommand>,
    busy: Arc<AtomicBool>,
}

impl BrokerAuthorization {
    pub(super) fn start(stop: &Arc<AtomicBool>) -> Result<(Self, thread::JoinHandle<()>), String> {
        let (commands, requests) = mpsc::sync_channel(1);
        let busy = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(stop);
        let worker_busy = Arc::clone(&busy);
        let worker = thread::Builder::new()
            .name("broker_authorization".to_string())
            .spawn(move || run(&requests, &worker_stop, &worker_busy))
            .map_err(|_| "Broker authorization worker could not start".to_string())?;
        Ok((Self { commands, busy }, worker))
    }

    fn request(&self, operation: Operation, stop: &AtomicBool) -> Result<String, String> {
        if stop.load(Ordering::Acquire) {
            return Err("Market runtime is shutting down".to_string());
        }
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "A broker connection operation is already running".to_string())?;
        let (reply, result) = mpsc::sync_channel(1);
        let command = BrokerCommand { operation, reply };
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
        if provider != "tastytrade" {
            return Err("Provider does not support browser authorization".to_string());
        }
        self.runtime
            .broker_authorization
            .request(Operation::Connect, &self.runtime.shutdown)
    }

    /// Deletes the hosted broker connection and its protected desktop capability.
    /// Call from a background worker. Provider grant revocation remains with the broker.
    ///
    /// # Errors
    /// Rejects unsupported providers, overload, or failed protected deletion.
    pub fn disconnect_provider(&self, provider: &str) -> Result<String, String> {
        if provider != "tastytrade" {
            return Err("Provider does not support browser authorization".to_string());
        }
        self.runtime
            .broker_authorization
            .request(Operation::Disconnect, &self.runtime.shutdown)
    }

    /// Checks an existing authorization against one bounded public feed session.
    /// Call from a background worker. Results contain no credentials or raw events.
    ///
    /// # Errors
    /// Rejects unsupported providers, missing login, overload, or feed failures.
    pub fn verify_provider_feed(&self, provider: &str) -> Result<String, String> {
        if provider != "tastytrade" {
            return Err("Provider does not support this feed verification".to_string());
        }
        self.runtime
            .broker_authorization
            .request(Operation::VerifyFeed, &self.runtime.shutdown)
    }
}

fn run(requests: &Receiver<BrokerCommand>, stop: &Arc<AtomicBool>, busy: &AtomicBool) {
    let mut client = TastytradeBrokerClient::default();
    while !stop.load(Ordering::Acquire) {
        let command = match requests.recv_timeout(WORKER_POLL) {
            Ok(command) => command,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let BrokerCommand { operation, reply } = command;
        let result = NativeCredentialVault::new(VAULT_SERVICE)
            .map_err(|_| "Protected broker credential storage is unavailable".to_string())
            .and_then(|vault| match operation {
                Operation::Disconnect => disconnect(&mut client, &vault, stop),
                Operation::Connect => connect(&mut client, &vault, stop),
                Operation::VerifyFeed => verify_feed(&mut client, &vault, stop),
            });
        busy.store(false, Ordering::Release);
        // A dropped request cannot change ownership or start another transaction.
        if reply.send(result).is_err() && stop.load(Ordering::Acquire) {
            break;
        }
    }
}

fn load(vault: &NativeCredentialVault) -> Result<Option<ConnectionCapability>, String> {
    vault
        .load(VAULT_KEY)
        .map_err(|_| "Protected broker connection could not be loaded".to_string())?
        .map(|bytes| ConnectionCapability::from_vault(&Zeroizing::new(bytes)))
        .transpose()
}

fn connect(
    client: &mut TastytradeBrokerClient,
    vault: &NativeCredentialVault,
    stop: &Arc<AtomicBool>,
) -> Result<String, String> {
    if let Some(capability) = load(vault)? {
        let status = client.status(&capability, stop)?;
        if status.phase == AuthorizationPhase::Ready {
            return verify_entitlement(client, &capability, stop);
        }
        // A prior pending transaction cannot be resumed safely without its browser
        // URL. Delete it before creating another transaction.
        client.disconnect(&capability, stop)?;
        delete_local(vault)?;
    }
    let pending = client.begin(stop)?;
    if vault
        .store(VAULT_KEY, &pending.capability.vault_bytes()?)
        .is_err()
    {
        client.disconnect(&pending.capability, stop)?;
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
        match client.status(&pending.capability, stop)?.phase {
            AuthorizationPhase::Pending | AuthorizationPhase::Exchanging => {}
            AuthorizationPhase::Ready => {
                return verify_entitlement(client, &pending.capability, stop);
            }
            AuthorizationPhase::Failed => {
                return Err("Tastytrade authorization failed; disconnect and retry".to_string());
            }
        }
    }
}

fn verify_entitlement(
    client: &mut TastytradeBrokerClient,
    capability: &ConnectionCapability,
    stop: &Arc<AtomicBool>,
) -> Result<String, String> {
    let _token = client.quote_token(capability, stop)?;
    // This checks the actual API entitlement, not the brokerage website display.
    // No feed/order-book claim is made before the DXLink live path is verified.
    Ok("Tastytrade authorization complete. Streaming token issued. Live charts are not connected yet.".to_string())
}

fn disconnect(
    client: &mut TastytradeBrokerClient,
    vault: &NativeCredentialVault,
    stop: &Arc<AtomicBool>,
) -> Result<String, String> {
    if let Some(capability) = load(vault)? {
        // Keep the local proof until server deletion succeeds, including after an
        // uncertain response, so deletion can be retried without orphaning tokens.
        client.disconnect(&capability, stop)?;
        delete_local(vault)?;
    }
    Ok("Tastytrade connection removed. You can revoke Aeris access in tastytrade's authorized applications.".to_string())
}

fn verify_feed(
    client: &mut TastytradeBrokerClient,
    vault: &NativeCredentialVault,
    stop: &Arc<AtomicBool>,
) -> Result<String, String> {
    let capability = load(vault)?.ok_or("Connect tastytrade before checking the feed")?;
    let instruments = client.futures_for_product(&capability, "ES", stop)?;
    let instrument = instruments
        .iter()
        .filter(|item| item.active && item.exchange == "CME")
        .min_by_key(|item| (!item.active_month, &item.expiration_date))
        .ok_or("Tastytrade did not return an active CME ES contract")?;
    // Respect the hosted connection's two-second request cooldown; no retry storm.
    wait_until(Instant::now() + POLL_INTERVAL, stop)?;
    let token = client.quote_token(&capability, stop)?;
    let report = aeris_tastytrade_market_adapter::verify_live_feed(
        &token,
        &instrument.streamer_symbol,
        stop,
    )?;
    let observed_now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "System clock is invalid")?;
    let observed_ms =
        u64::try_from(observed_now.as_millis()).map_err(|_| "System clock overflowed")?;
    let quote_age = timestamp_age(report.newest_quote_time_ms, observed_ms);
    let trade_age = timestamp_age(report.newest_trade_time_ms, observed_ms);
    Ok(format!(
        "{} feed check: {} usable quote updates, {} candle rows, {} new trades ({} buy, {} sell, {} unknown side). TimeAndSale fields: {}. Latest quote age: {quote_age}. Latest trade age: {trade_age}. Live charts remain disconnected.",
        instrument.symbol,
        report.usable_quotes,
        report.usable_candles,
        report.new_trades,
        report.buy_trades,
        report.sell_trades,
        report.unknown_side_trades,
        if report.time_and_sale_fields {
            "confirmed"
        } else {
            "unavailable"
        }
    ))
}

fn timestamp_age(provider_ms: Option<u64>, observed_ms: u64) -> String {
    match provider_ms {
        Some(time) if time <= observed_ms => {
            format!("{} ms against this machine's clock", observed_ms - time)
        }
        Some(time) => format!(
            "provider timestamp is {} ms ahead of this machine's clock",
            time - observed_ms
        ),
        None => "provider timestamp unavailable".to_string(),
    }
}

fn delete_local(vault: &NativeCredentialVault) -> Result<(), String> {
    vault
        .delete(VAULT_KEY)
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

    #[test]
    fn authorization_queue_rejects_duplicate_work_and_shutdown() {
        let (commands, _requests) = mpsc::sync_channel(1);
        let owner = BrokerAuthorization {
            commands,
            busy: Arc::new(AtomicBool::new(true)),
        };
        assert!(
            owner
                .request(Operation::Connect, &AtomicBool::new(false))
                .unwrap_err()
                .contains("already running")
        );
        assert!(
            owner
                .request(Operation::Connect, &AtomicBool::new(true))
                .unwrap_err()
                .contains("shutting down")
        );
    }

    #[test]
    fn pending_browser_wait_observes_shutdown() {
        assert!(wait_until(Instant::now() + POLL_INTERVAL, &AtomicBool::new(true)).is_err());
    }

    #[test]
    fn feed_age_keeps_missing_and_future_timestamps_explicit() {
        assert_eq!(
            timestamp_age(Some(1000), 1200),
            "200 ms against this machine's clock"
        );
        assert_eq!(
            timestamp_age(Some(1200), 1000),
            "provider timestamp is 200 ms ahead of this machine's clock"
        );
        assert_eq!(timestamp_age(None, 1000), "provider timestamp unavailable");
    }
}
