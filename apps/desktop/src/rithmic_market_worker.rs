use crate::{
    market_worker::{
        MarketDataWorker, MarketWorkerCommand, MarketWorkerMessage, MarketWorkerStartup,
        market_worker_channel,
    },
    rithmic_shell::RithmicShellState,
};
use axiusflow_application::ProvenancedMarketBar;
use axiusflow_desktop_history::HistoryWorkerConfig;
use axiusflow_desktop_provider_runtime::{
    AuthenticationState, DesktopMarketWorker, DesktopMarketWorkerConfig, DesktopProviderConfig,
    ProviderInvalidationReason, ProviderSessionEvent,
};
use axiusflow_desktop_storage::CatalogKey;
use axiusflow_observability::FeedConnectionState;
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use axiusflow_rithmic_protocol_adapter::{
    AppliedRithmicEvent, MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES, RITHMIC_TEST_VAULT_KEY,
    RITHMIC_TEST_VAULT_SERVICE, RithmicCallbackLimits, RithmicProviderConfig,
    RithmicProviderDriver, RithmicRetryScheduler, RithmicSessionLimits, try_recv_rithmic_event,
};
use std::{
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, RecvTimeoutError, TryRecvError},
    },
    thread::{self, ThreadId},
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

const MESSAGE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 1;
const CALLBACK_CAPACITY: usize = 256;
const CALLBACK_BYTES: usize = 8 * 1024 * 1024;
const MAXIMUM_DEPTH: usize = 256;
const IDLE_WAIT: Duration = Duration::from_millis(50);
const MESSAGE_SILENCE: Duration = Duration::from_secs(30);
const CATALOG_KEY_ID: &str = "rithmic-test-history-catalog-key-v1";

type RithmicWorker =
    DesktopMarketWorker<ProvenancedMarketBar, NativeCredentialVault, RithmicProviderDriver>;

pub(crate) fn start(
    history_root: PathBuf,
    ui_thread: ThreadId,
    detailed_diagnostics: bool,
) -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    let shell = RithmicShellState::local()?;
    spawn_worker(shell, move |message_tx, command_rx| {
        run(
            &message_tx,
            &command_rx,
            history_root,
            ui_thread,
            detailed_diagnostics,
        );
    })
}

fn spawn_worker(
    shell: RithmicShellState,
    task: impl FnOnce(crate::market_worker::MarketWorkerSender, Receiver<MarketWorkerCommand>)
    + Send
    + 'static,
) -> Result<(MarketWorkerStartup, MarketDataWorker), String> {
    let (message_tx, message_rx) = market_worker_channel(nonzero(MESSAGE_CAPACITY));
    let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("axiusflow-rithmic-market-worker".to_string())
        .spawn(move || {
            task(message_tx, command_rx);
            let _ = shutdown_tx.send(());
        })
        .map_err(|_| "Rithmic market worker thread is unavailable".to_string())?;

    Ok((
        MarketWorkerStartup::Shell(shell),
        MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None),
    ))
}

fn run(
    messages: &crate::market_worker::MarketWorkerSender,
    commands: &Receiver<MarketWorkerCommand>,
    history_root: PathBuf,
    ui_thread: ThreadId,
    detailed_diagnostics: bool,
) {
    let (wake_tx, wake_rx) = mpsc::sync_channel(1);
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = wake_tx.try_send(());
    });
    let opened = open_worker(history_root, ui_thread, detailed_diagnostics, wake);
    let Ok((mut worker, events)) = opened else {
        send_connection(
            messages,
            FeedConnectionState::Recovering,
            "Rithmic Test is waiting for credentials or local runtime access",
        );
        wait_for_shutdown(commands);
        return;
    };
    send_connection(
        messages,
        FeedConnectionState::Discovering,
        "discovering Rithmic Test systems",
    );
    if worker.request_connection().is_err() {
        send_connection(
            messages,
            FeedConnectionState::Recovering,
            "Rithmic Test credentials are unavailable or require attention",
        );
    }

    let mut retries = RithmicRetryScheduler::default();
    loop {
        match commands.try_recv() {
            Ok(MarketWorkerCommand::Shutdown) | Err(TryRecvError::Disconnected) => break,
            Ok(MarketWorkerCommand::Recovery(_)) | Err(TryRecvError::Empty) => {}
        }
        while events.has_ready() {
            match try_recv_rithmic_event(&mut worker, &events, &mut retries, Instant::now()) {
                Ok(Some(event)) => publish_event(messages, &event),
                Ok(None) => break,
                Err(_) => {
                    send_connection(
                        messages,
                        FeedConnectionState::Recovering,
                        "Rithmic Test session recovery is required",
                    );
                    break;
                }
            }
        }
        if retries
            .retry_due(&mut worker, Instant::now())
            .is_ok_and(|generation| generation.is_some())
        {
            send_connection(
                messages,
                FeedConnectionState::Discovering,
                "reconnecting to Rithmic Test",
            );
        }
        if let Ok(Some(snapshot)) = worker.try_diagnostics_snapshot() {
            let _ = messages.send(MarketWorkerMessage::Diagnostics(Box::new(snapshot)));
        }
        let wait = retries.ticket().map_or(IDLE_WAIT, |ticket| {
            ticket
                .due_at
                .saturating_duration_since(Instant::now())
                .min(IDLE_WAIT)
        });
        match wake_rx.recv_timeout(wait) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let stopped = worker.stop().is_ok();
    send_connection(
        messages,
        if stopped {
            FeedConnectionState::Stopped
        } else {
            FeedConnectionState::Recovering
        },
        if stopped {
            "Rithmic Test session stopped"
        } else {
            "Rithmic Test session stop was not confirmed"
        },
    );
}

fn open_worker(
    history_root: PathBuf,
    ui_thread: ThreadId,
    detailed_diagnostics: bool,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<
    (
        RithmicWorker,
        axiusflow_rithmic_protocol_adapter::RithmicProviderEvents,
    ),
    String,
> {
    let credential_vault = NativeCredentialVault::new(RITHMIC_TEST_VAULT_SERVICE)
        .map_err(|_| "native credential vault unavailable".to_string())?;
    let key_vault = NativeCredentialVault::new(RITHMIC_TEST_VAULT_SERVICE)
        .map_err(|_| "native key vault unavailable".to_string())?;
    let catalog_key = load_catalog_key(&key_vault)?;
    let provider = RithmicProviderConfig::try_new(
        "Axiusflow",
        env!("CARGO_PKG_VERSION"),
        RithmicSessionLimits::default(),
        MESSAGE_SILENCE,
        Vec::new(),
    )
    .map_err(|_| "Rithmic provider configuration is invalid".to_string())?;
    let callback_limits = RithmicCallbackLimits::try_new(
        nonzero(CALLBACK_CAPACITY),
        nonzero(CALLBACK_BYTES),
        nonzero(MAXIMUM_DEPTH),
    )
    .map_err(|_| "Rithmic callback limits are invalid".to_string())?;
    let (driver, events) = RithmicProviderDriver::new_with_wake(provider, callback_limits, wake);
    let provider_config =
        DesktopProviderConfig::new(nonzero(32), nonzero(MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES))
            .with_diagnostics(
                RithmicProviderConfig::environment(),
                detailed_diagnostics
                    .then_some(NonZeroU64::new(10_000_000_000).unwrap_or(NonZeroU64::MIN)),
            )
            .map_err(|_| "Rithmic diagnostics configuration is invalid".to_string())?;
    let worker = DesktopMarketWorker::try_open(
        credential_vault,
        driver,
        RITHMIC_TEST_VAULT_KEY,
        history_root,
        catalog_key,
        ui_thread,
        DesktopMarketWorkerConfig {
            provider: provider_config,
            history: HistoryWorkerConfig {
                maximum_cache_entries: nonzero(2),
                maximum_decoded_bytes: nonzero(2 * 1024 * 1024),
                maximum_charts: nonzero(1),
                maximum_segment_read_bytes: nonzero(1024 * 1024),
                maximum_buffered_live: nonzero(512),
                maximum_handoffs: nonzero(1),
            },
            maximum_catalog_entries: 64,
        },
    )
    .map_err(|_| "Rithmic desktop runtime is unavailable".to_string())?;
    Ok((worker, events))
}

fn publish_event(messages: &crate::market_worker::MarketWorkerSender, event: &AppliedRithmicEvent) {
    let (state, message) = reduce_event(event);
    send_connection(messages, state, message);
}

fn reduce_event(event: &AppliedRithmicEvent) -> (FeedConnectionState, &'static str) {
    match event {
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::DiscoveryStarted) => (
            FeedConnectionState::Discovering,
            "discovering Rithmic Test systems",
        ),
        AppliedRithmicEvent::Semantic(
            ProviderSessionEvent::SystemsDiscovered { .. }
            | ProviderSessionEvent::AuthenticationChanged {
                state: AuthenticationState::Required | AuthenticationState::Accepted,
                ..
            },
        ) => (
            FeedConnectionState::Authenticating,
            "authenticating the Rithmic Test session",
        ),
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::AuthenticationChanged {
            state: AuthenticationState::Rejected,
            ..
        }) => (
            FeedConnectionState::Stopped,
            "Rithmic Test authentication was rejected",
        ),
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::AuthenticationChanged {
            state: AuthenticationState::AgreementRequired,
            ..
        }) => (
            FeedConnectionState::Stopped,
            "Rithmic Test agreements require attention",
        ),
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::InstrumentsDiscovered { .. }) => (
            FeedConnectionState::Streaming,
            "Rithmic Test discovery is ready for instrument selection",
        ),
        AppliedRithmicEvent::RetryScheduled(_) => (
            FeedConnectionState::Recovering,
            "Rithmic Test session will retry",
        ),
        AppliedRithmicEvent::TerminalFailure { reason, .. } => terminal_failure(*reason),
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::Stopped) => {
            (FeedConnectionState::Stopped, "Rithmic Test session stopped")
        }
        AppliedRithmicEvent::Semantic(
            ProviderSessionEvent::Market { .. } | ProviderSessionEvent::Heartbeat { .. },
        ) => (
            FeedConnectionState::Streaming,
            "Rithmic Test feed is streaming",
        ),
        AppliedRithmicEvent::Semantic(ProviderSessionEvent::Invalidated { .. }) => (
            FeedConnectionState::Recovering,
            "Rithmic Test session recovery is required",
        ),
    }
}

fn terminal_failure(reason: ProviderInvalidationReason) -> (FeedConnectionState, &'static str) {
    match reason {
        ProviderInvalidationReason::Authentication => (
            FeedConnectionState::Stopped,
            "Rithmic Test authentication was rejected",
        ),
        ProviderInvalidationReason::AgreementRequired => (
            FeedConnectionState::Stopped,
            "Rithmic Test agreements require attention",
        ),
        _ => (
            FeedConnectionState::Stopped,
            "Rithmic Test session cannot continue",
        ),
    }
}

fn send_connection(
    messages: &crate::market_worker::MarketWorkerSender,
    state: FeedConnectionState,
    message: &str,
) {
    let _ = messages.send(MarketWorkerMessage::Connection {
        state,
        message: message.to_string(),
    });
}

fn wait_for_shutdown(commands: &Receiver<MarketWorkerCommand>) {
    while let Ok(command) = commands.recv() {
        if matches!(command, MarketWorkerCommand::Shutdown) {
            break;
        }
    }
}

fn load_catalog_key(vault: &NativeCredentialVault) -> Result<CatalogKey, String> {
    let bytes = load_or_create_key(vault, CATALOG_KEY_ID)?;
    CatalogKey::try_new(CATALOG_KEY_ID.to_string(), bytes)
        .map_err(|_| "Rithmic catalog key is invalid".to_string())
}

fn load_or_create_key(vault: &NativeCredentialVault, key_id: &str) -> Result<[u8; 32], String> {
    if let Some(mut stored) = vault
        .load(key_id)
        .map_err(|_| "native key vault is unavailable".to_string())?
    {
        let result = <[u8; 32]>::try_from(stored.as_slice())
            .map_err(|_| "native catalog key has an invalid length".to_string());
        stored.zeroize();
        return result;
    }
    let mut generated = Zeroizing::new([0_u8; 32]);
    getrandom::fill(generated.as_mut()).map_err(|_| "catalog key generation failed".to_string())?;
    vault
        .store(key_id, generated.as_ref())
        .map_err(|_| "native key vault is unavailable".to_string())?;
    Ok(*generated)
}

fn nonzero(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap_or(NonZeroUsize::MIN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_desktop_provider_runtime::{ProviderEnvironment, SessionGeneration};

    fn generation() -> SessionGeneration {
        SessionGeneration::new(NonZeroU64::MIN)
    }

    #[test]
    fn reducer_exposes_only_coarse_lifecycle_states() {
        let cases = [
            (
                AppliedRithmicEvent::Semantic(ProviderSessionEvent::DiscoveryStarted),
                FeedConnectionState::Discovering,
            ),
            (
                AppliedRithmicEvent::Semantic(ProviderSessionEvent::SystemsDiscovered {
                    environments: vec![ProviderEnvironment {
                        provider_id: "rithmic".to_string(),
                        system_id: "RITHMIC_TEST".to_string(),
                        environment: "Test".to_string(),
                    }],
                }),
                FeedConnectionState::Authenticating,
            ),
            (
                AppliedRithmicEvent::Semantic(ProviderSessionEvent::InstrumentsDiscovered {
                    generation: generation(),
                    instruments: Vec::new(),
                }),
                FeedConnectionState::Streaming,
            ),
            (
                AppliedRithmicEvent::Semantic(ProviderSessionEvent::Stopped),
                FeedConnectionState::Stopped,
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(reduce_event(&event).0, expected);
        }
    }

    #[test]
    fn terminal_provider_failures_never_render_provider_text() {
        for reason in [
            ProviderInvalidationReason::Authentication,
            ProviderInvalidationReason::AgreementRequired,
            ProviderInvalidationReason::UnsupportedSystem,
        ] {
            let (state, message) = reduce_event(&AppliedRithmicEvent::TerminalFailure {
                generation: generation(),
                reason,
            });
            assert_eq!(state, FeedConnectionState::Stopped);
            assert!(!message.contains("account"));
            assert!(!message.contains("user"));
            assert!(!message.contains("password"));
        }
    }

    #[test]
    fn shell_returns_before_background_completion_and_drop_acknowledges_shutdown() {
        let shell = RithmicShellState::local().expect("fixed shell profile validates");
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (stopped_tx, stopped_rx) = mpsc::sync_channel(1);
        let (startup, worker) = spawn_worker(shell, move |_messages, commands| {
            let _ = started_tx.send(());
            wait_for_shutdown(&commands);
            let _ = stopped_tx.send(());
        })
        .expect("bounded worker thread starts");

        assert!(matches!(startup, MarketWorkerStartup::Shell(_)));
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("background task starts independently");
        drop(worker);
        stopped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("drop requests and acknowledges shutdown");
    }
}
