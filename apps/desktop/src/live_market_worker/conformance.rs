use super::{
    COMMAND_CAPACITY, INBOX_CAPACITY, MESSAGE_CAPACITY, PROVIDER_EVENT_CAPACITY,
    UI_DIAGNOSTICS_CAPACITY, WorkerInboxEvent, forward_commands, nonzero, prepare_running_worker,
    product_profile, run_worker_loop,
};
use super::{composition::open_test_worker, history::fetch_history_with_adapter};
use crate::market_worker::{
    ChartState, MarketDataWorker, MarketWorkerMessage, market_worker_channel,
    ui_diagnostics_channel,
};
use axiusflow_application::{ReplayProvenance, ReplayStreamUpdate};
use axiusflow_coinbase_market_adapter::{
    CanonicalTrade, CoinbaseConfig, CoinbaseHistoryCapabilityAdapter, CoinbaseHistoryTransport,
    CoinbaseProviderDriver, CoinbaseProviderFixtureControl, CoinbaseProviderInvalidReason,
    FixedPointValue, encode_history_segment,
};
use axiusflow_desktop_storage::{
    CatalogKey, HistoryStore, PublicationOutcome, PublicationRequest, RecoveryAction,
    RetentionPolicy, SegmentEncryptionKey,
};
use axiusflow_platform_runtime::CredentialVault;
use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const FIXED_NOW_UNIX_NANOS: i64 = 1_700_000_030_000_000_000;
const FIXED_CURRENT_MINUTE_SECONDS: i64 = 1_699_999_980;
const SENTINEL_SECRET: &str = "sentinel-provider-secret";

#[derive(Clone, Copy)]
struct MemoryVault;

impl CredentialVault for MemoryVault {
    type Error = ();

    fn store(&self, _key: &str, _secret: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    fn load(&self, _key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(None)
    }

    fn delete(&self, _key: &str) -> Result<(), Self::Error> {
        Ok(())
    }
}

struct FixtureHistorySource<T> {
    adapter: CoinbaseHistoryCapabilityAdapter<T>,
}

impl<T: CoinbaseHistoryTransport + Send> super::HistorySource for FixtureHistorySource<T> {
    fn now_unix_nanos(&self) -> Result<i64, String> {
        Ok(FIXED_NOW_UNIX_NANOS)
    }

    fn fetch(
        &mut self,
        profile: &super::ProductProfile,
        now_unix_nanos: i64,
    ) -> Result<super::PreparedHistory, String> {
        fetch_history_with_adapter(profile, &mut self.adapter, now_unix_nanos)
    }
}

struct CandleTransport;

impl CoinbaseHistoryTransport for CandleTransport {
    fn get(&mut self, path: &str) -> Result<Vec<u8>, String> {
        let start = query_number(path, "start")?;
        let end = query_number(path, "end")?;
        let mut body = String::from("{\"candles\":[");
        for (index, second) in (start..=end).step_by(60).enumerate() {
            if index != 0 {
                body.push(',');
            }
            write!(
                body,
                "{{\"start\":\"{second}\",\"low\":\"37000.00\",\"high\":\"37002.00\",\"open\":\"37000.00\",\"close\":\"37001.00\",\"volume\":\"1.00000000\"}}"
            )
            .expect("fixture candle writes");
        }
        body.push_str("]}");
        Ok(body.into_bytes())
    }
}

struct SecretFailureTransport;

impl CoinbaseHistoryTransport for SecretFailureTransport {
    fn get(&mut self, _path: &str) -> Result<Vec<u8>, String> {
        Err(format!("provider failed with {SENTINEL_SECRET}"))
    }
}

struct TestRoot(PathBuf);

struct ControlledWorkerInput<'a, T> {
    product_id: String,
    history_root: PathBuf,
    ui_thread: thread::ThreadId,
    driver: CoinbaseProviderDriver,
    events: axiusflow_coinbase_market_adapter::CoinbaseProviderEvents,
    history_source: FixtureHistorySource<T>,
    message_tx: &'a super::MarketWorkerSender,
    inbox_rx: mpsc::Receiver<WorkerInboxEvent>,
    provider_wake_pending: &'a AtomicBool,
    ui_diagnostics_rx: &'a super::UiDiagnosticsReceiver,
}

impl TestRoot {
    fn create(name: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock follows Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "axiusflow-shipping-conformance-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test root creates");
        Self(path)
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn shipping_loop_refetches_corrupt_cache_reconnects_and_stops_boundedly() {
    let root = TestRoot::create("reconnect");
    seed_corrupt_cache(&root.0);
    let source = fixture_source(CandleTransport);
    let (mut worker, control) = start_controlled_worker(&root.0, source);

    let first = control
        .wait_started(Duration::from_secs(1))
        .expect("first controlled generation starts");
    assert!(directory_has_entry(&root.0.join("quarantine")));
    assert!(control.established(first));
    let first_snapshot = wait_for_live_snapshot(&mut worker, 2);
    assert_eq!(first_snapshot, (1, 300));

    control.invalid(first, CoinbaseProviderInvalidReason::Transport);
    assert_eq!(control.wait_stopped(Duration::from_secs(1)), Some(first));
    let second = control
        .wait_started(Duration::from_secs(2))
        .expect("reconnect starts a fresh generation");
    assert_ne!(first, second);
    assert!(control.heartbeat(first));
    assert!(control.established(second));
    let second_snapshot = wait_for_live_snapshot(&mut worker, 3);
    assert_eq!(second_snapshot, (1, 300));
    assert!(control.trade(second, fixture_trade(1, FIXED_CURRENT_MINUTE_SECONDS)));
    assert!(control.trade(second, fixture_trade(2, FIXED_CURRENT_MINUTE_SECONDS + 60)));
    assert_eq!(wait_for_live_delta(&mut worker), (300, 301));

    let shutdown_started = Instant::now();
    drop(worker);
    assert!(shutdown_started.elapsed() < Duration::from_secs(2));
    assert_eq!(control.wait_stopped(Duration::from_secs(1)), Some(second));
}

#[test]
fn shipping_loop_redacts_nested_history_failures() {
    let root = TestRoot::create("redaction");
    let source = fixture_source(SecretFailureTransport);
    let (mut worker, control) = start_controlled_worker(&root.0, source);
    let generation = control
        .wait_started(Duration::from_secs(1))
        .expect("controlled generation starts");
    assert!(control.established(generation));
    assert_eq!(
        control.wait_stopped(Duration::from_secs(1)),
        Some(generation)
    );

    let messages = wait_for_recovery_and_diagnostics(&mut worker);
    for message in messages {
        assert_message_redacted(&message);
    }
    drop(worker);
}

fn fixture_source<T>(transport: T) -> FixtureHistorySource<T> {
    FixtureHistorySource {
        adapter: CoinbaseHistoryCapabilityAdapter::try_with_transport(transport)
            .expect("fixture history capabilities validate"),
    }
}

fn start_controlled_worker<T: CoinbaseHistoryTransport + Send + 'static>(
    history_root: &Path,
    history_source: FixtureHistorySource<T>,
) -> (MarketDataWorker, CoinbaseProviderFixtureControl) {
    let product_id = "BTC-USD".to_string();
    let (message_tx, message_rx) = market_worker_channel(nonzero(MESSAGE_CAPACITY));
    let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (inbox_tx, inbox_rx) = mpsc::sync_channel(INBOX_CAPACITY);
    let provider_wake_pending = Arc::new(AtomicBool::new(false));
    let provider_inbox = inbox_tx.clone();
    let wake_pending = Arc::clone(&provider_wake_pending);
    let wake = Arc::new(move || {
        if wake_pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && provider_inbox
                .try_send(WorkerInboxEvent::ProviderReady)
                .is_err()
        {
            wake_pending.store(false, Ordering::Release);
        }
    });
    let config =
        CoinbaseConfig::try_new(vec![product_id.clone()]).expect("controlled product validates");
    let (driver, events, control) = CoinbaseProviderDriver::new_controlled_with_wake(
        config,
        nonzero(PROVIDER_EVENT_CAPACITY),
        wake,
    );
    let diagnostics_wake = Arc::new(|| {});
    let (ui_diagnostics_tx, ui_diagnostics_rx) =
        ui_diagnostics_channel(nonzero(UI_DIAGNOSTICS_CAPACITY), diagnostics_wake);
    let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
    let command_inbox = inbox_tx.clone();
    thread::spawn(move || forward_commands(&command_rx, &command_inbox));
    let root = history_root.to_path_buf();
    let ui_thread = thread::current().id();
    thread::spawn(move || {
        let error_tx = message_tx.clone();
        let result = run_controlled_worker(ControlledWorkerInput {
            product_id,
            history_root: root,
            ui_thread,
            driver,
            events,
            history_source,
            message_tx: &message_tx,
            inbox_rx,
            provider_wake_pending: &provider_wake_pending,
            ui_diagnostics_rx: &ui_diagnostics_rx,
        });
        if let Err(error) = result {
            let _ = error_tx.send(MarketWorkerMessage::State {
                state: ChartState::Error,
                message: error,
            });
        }
        let _ = shutdown_tx.send(());
    });
    (
        MarketDataWorker::from_channels(
            command_tx,
            message_rx,
            shutdown_rx,
            Some(ui_diagnostics_tx),
        ),
        control,
    )
}

fn run_controlled_worker<T: CoinbaseHistoryTransport + Send>(
    input: ControlledWorkerInput<'_, T>,
) -> Result<(), String> {
    let profile = product_profile(input.product_id)?;
    let worker = open_test_worker(
        &profile,
        input.history_root,
        input.ui_thread,
        MemoryVault,
        input.driver,
        catalog_key(),
    )?;
    let running = prepare_running_worker(
        profile,
        super::OpenedWorker {
            worker,
            events: input.events,
            segment_key: segment_key(),
        },
        None,
        false,
        input.history_source,
        input.message_tx,
    )?;
    run_worker_loop(
        running,
        input.message_tx,
        &input.inbox_rx,
        input.provider_wake_pending,
        input.ui_diagnostics_rx,
    )
}

fn seed_corrupt_cache(root: &Path) {
    let profile = product_profile("BTC-USD".to_string()).expect("fixture profile validates");
    let mut source = fixture_source(CandleTransport);
    let prepared = super::HistorySource::fetch(&mut source, &profile, FIXED_NOW_UNIX_NANOS)
        .expect("fixture history fetches");
    let payload = encode_history_segment(&prepared.completion.page().items)
        .expect("fixture history segment encodes");
    let mut store = HistoryStore::open(root, catalog_key(), 8).expect("fixture store opens");
    let outcome = store
        .publish(PublicationRequest {
            identity: &prepared.identity,
            payload: &payload,
            encryption_key: &segment_key(),
            retention: RetentionPolicy::UntilRevoked,
            recovery: RecoveryAction::ProviderRefetch,
            now_unix_seconds: FIXED_NOW_UNIX_NANOS / 1_000_000_000,
        })
        .expect("fixture cache publishes");
    let PublicationOutcome::Published(receipt) = outcome else {
        panic!("fixture cache unexpectedly stayed in memory");
    };
    drop(store);
    corrupt_file(&root.join("segments").join(receipt.file_name));
}

fn corrupt_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .expect("fixture segment metadata reads")
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions).expect("fixture segment becomes writable");
    }
    fs::write(path, b"corrupt").expect("fixture cache corruption writes");
}

fn wait_for_live_snapshot(worker: &mut MarketDataWorker, ownership_epoch: u64) -> (u64, u64) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let (messages, disconnected) = worker.drain_messages();
        assert!(
            !disconnected,
            "shipping worker disconnected during snapshot wait"
        );
        for message in messages {
            if let MarketWorkerMessage::State {
                state: ChartState::Error,
                message,
            } = &message
            {
                panic!("shipping worker failed: {message}");
            }
            if let MarketWorkerMessage::Update(publication) = message
                && let ReplayStreamUpdate::Snapshot(snapshot) = publication.update
                && snapshot.provenance() == ReplayProvenance::LiveProvider
                && snapshot.evidence().ownership_epoch == ownership_epoch
            {
                return (
                    snapshot.evidence().first_sequence,
                    snapshot.evidence().last_sequence,
                );
            }
        }
        assert!(Instant::now() < deadline, "live snapshot timed out");
        thread::yield_now();
    }
}

fn wait_for_live_delta(worker: &mut MarketDataWorker) -> (u64, u64) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let (messages, disconnected) = worker.drain_messages();
        assert!(
            !disconnected,
            "shipping worker disconnected during delta wait"
        );
        for message in messages {
            if let MarketWorkerMessage::State {
                state: ChartState::Error,
                message,
            } = &message
            {
                panic!("shipping worker failed: {message}");
            }
            if let MarketWorkerMessage::Update(publication) = message
                && let ReplayStreamUpdate::Delta(delta) = publication.update
            {
                return (delta.previous_sequence(), delta.sequence());
            }
        }
        assert!(Instant::now() < deadline, "live delta timed out");
        thread::yield_now();
    }
}

fn wait_for_recovery_and_diagnostics(worker: &mut MarketDataWorker) -> Vec<MarketWorkerMessage> {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut retained = Vec::new();
    let mut recovering = false;
    let mut diagnostics = false;
    loop {
        let (messages, disconnected) = worker.drain_messages();
        assert!(
            !disconnected,
            "shipping worker disconnected during recovery wait"
        );
        recovering |= messages.iter().any(|message| {
            matches!(
                message,
                MarketWorkerMessage::State {
                    state: ChartState::Recovering,
                    ..
                }
            )
        });
        diagnostics |= messages
            .iter()
            .any(|message| matches!(message, MarketWorkerMessage::Diagnostics(_)));
        retained.extend(messages);
        if recovering && diagnostics {
            return retained;
        }
        assert!(Instant::now() < deadline, "recovery state timed out");
        thread::yield_now();
    }
}

fn assert_message_redacted(message: &MarketWorkerMessage) {
    let rendered = match message {
        MarketWorkerMessage::State { message, .. }
        | MarketWorkerMessage::Connection { message, .. } => message.clone(),
        MarketWorkerMessage::Recovery { result, .. } => match result {
            Ok(_) => "recovery succeeded".to_string(),
            Err(error) => error.clone(),
        },
        MarketWorkerMessage::Diagnostics(snapshot) => format!("{snapshot:?}"),
        MarketWorkerMessage::RithmicCatalog(event) => format!("{event:?}"),
        MarketWorkerMessage::RithmicHistory { result, .. } => match result {
            Ok(bootstrap) => format!("{} {}", bootstrap.subscription_id, bootstrap.worker_label),
            Err(error) => error.clone(),
        },
        MarketWorkerMessage::RithmicLive { .. } => "Rithmic live snapshot".to_string(),
        MarketWorkerMessage::RithmicDom(_) => "Rithmic depth frame".to_string(),
        MarketWorkerMessage::Update(publication) => format!(
            "{} {} {}",
            publication.subscription_id,
            publication.worker_label,
            match &publication.update {
                ReplayStreamUpdate::Snapshot(_) => "snapshot",
                ReplayStreamUpdate::Delta(_) => "delta",
            }
        ),
    };
    assert!(!rendered.contains(SENTINEL_SECRET));
}

fn query_number(path: &str, key: &str) -> Result<i64, String> {
    path.split(['?', '&'])
        .find_map(|part| part.strip_prefix(&format!("{key}=")))
        .ok_or_else(|| format!("fixture request lacks {key}"))?
        .parse::<i64>()
        .map_err(|error| error.to_string())
}

fn fixture_trade(sequence: u64, minute_unix_seconds: i64) -> CanonicalTrade {
    CanonicalTrade {
        product_id: "BTC-USD".to_string(),
        trade_id: format!("shipping-fixture-{sequence}"),
        price: FixedPointValue::parse("37001.00").expect("fixture price parses"),
        size: FixedPointValue::parse("0.10000000").expect("fixture size parses"),
        maker_side_buy: true,
        trade_time_unix_nanos: minute_unix_seconds * 1_000_000_000,
        provider_timestamp_unix_nanos: minute_unix_seconds * 1_000_000_000 + 1,
        sequence_num: sequence,
        canonical_sequence: sequence,
    }
}

fn directory_has_entry(path: &Path) -> bool {
    fs::read_dir(path)
        .ok()
        .and_then(|mut entries| entries.next())
        .is_some()
}

fn catalog_key() -> CatalogKey {
    CatalogKey::try_new("shipping-conformance-catalog".to_string(), [0x31; 32])
        .expect("catalog key validates")
}

fn segment_key() -> SegmentEncryptionKey {
    SegmentEncryptionKey::try_new("shipping-conformance-segment".to_string(), [0x41; 32])
        .expect("segment key validates")
}
