use super::{
    COMMAND_CAPACITY, INBOX_CAPACITY, MESSAGE_CAPACITY, PROVIDER_EVENT_CAPACITY,
    UI_DIAGNOSTICS_CAPACITY, WorkerInboxEvent, forward_commands, nonzero, prepare_running_worker,
    product_profile, run_session,
};
use super::{
    composition::open_test_worker,
    history::{fetch_history_range_with_adapter, fetch_history_with_adapter},
};
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
use axiusflow_market_data::ChartInterval;
use axiusflow_platform_runtime::CredentialVault;
use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const FIXED_NOW_UNIX_NANOS: i64 = 1_700_000_030_000_000_000;
const FIXED_CURRENT_MINUTE_SECONDS: i64 = 1_699_999_980;
const SENTINEL_SECRET: &str = "sentinel-provider-secret";
const CONTROLLED_START_TIMEOUT: Duration = Duration::from_secs(5);

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
        _cancel: Arc<AtomicBool>,
        range: axiusflow_provider_history::HistoryRange,
    ) -> Result<super::history::PreparedHistory, String> {
        fetch_history_range_with_adapter(profile, &mut self.adapter, now_unix_nanos, range)
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

struct BlockingHistorySource {
    release: Arc<AtomicBool>,
    adapter: CoinbaseHistoryCapabilityAdapter<CandleTransport>,
}

impl super::HistorySource for BlockingHistorySource {
    fn now_unix_nanos(&self) -> Result<i64, String> {
        Ok(FIXED_NOW_UNIX_NANOS)
    }

    fn fetch(
        &mut self,
        profile: &super::ProductProfile,
        now_unix_nanos: i64,
        cancel: Arc<AtomicBool>,
        range: axiusflow_provider_history::HistoryRange,
    ) -> Result<super::history::PreparedHistory, String> {
        let started = Instant::now();
        while !self.release.load(Ordering::Acquire) {
            if cancel.load(Ordering::Acquire) {
                return Err("fixture fetch cancelled".to_string());
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "fixture fetch latch expired"
            );
            thread::sleep(Duration::from_millis(1));
        }
        fetch_history_range_with_adapter(profile, &mut self.adapter, now_unix_nanos, range)
    }
}

struct TestRoot(PathBuf);

struct ControlledWorkerInput<'a, H> {
    product_id: String,
    history_root: PathBuf,
    ui_thread: thread::ThreadId,
    driver: CoinbaseProviderDriver,
    events: axiusflow_coinbase_market_adapter::CoinbaseProviderEvents,
    history_source: H,
    message_tx: &'a super::MarketWorkerSender,
    inbox_tx: mpsc::SyncSender<WorkerInboxEvent>,
    inbox_rx: mpsc::Receiver<WorkerInboxEvent>,
    provider_wake_pending: &'a AtomicBool,
    ui_diagnostics_rx: &'a super::UiDiagnosticsReceiver,
    selection_sequence: Arc<AtomicU64>,
    session_end: mpsc::SyncSender<Option<u64>>,
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
    let (mut worker, control, _session_end) = start_controlled_worker(&root.0, source);

    let first = control
        .wait_started(CONTROLLED_START_TIMEOUT)
        .expect("first controlled generation starts");
    assert!(directory_has_entry(&root.0.join("quarantine")));
    assert!(control.established(first));
    let first_snapshot = wait_for_live_snapshot(&mut worker, 2);
    assert_eq!(first_snapshot, (1, 350));

    control.invalid(first, CoinbaseProviderInvalidReason::StreamTransport);
    assert_eq!(control.wait_stopped(Duration::from_secs(1)), Some(first));
    let second = control
        .wait_started(CONTROLLED_START_TIMEOUT)
        .expect("reconnect starts a fresh generation");
    assert_ne!(first, second);
    assert!(control.heartbeat(first));
    assert!(control.established(second));
    let second_snapshot = wait_for_live_snapshot(&mut worker, 3);
    assert_eq!(second_snapshot, (1, 350));
    assert!(control.trade(second, fixture_trade(1, FIXED_CURRENT_MINUTE_SECONDS)));
    assert!(control.trade(second, fixture_trade(2, FIXED_CURRENT_MINUTE_SECONDS + 60)));
    assert_eq!(wait_for_live_delta(&mut worker), (350, 351));

    let shutdown_started = Instant::now();
    drop(worker);
    assert!(shutdown_started.elapsed() < Duration::from_secs(2));
    assert_eq!(control.wait_stopped(Duration::from_secs(1)), Some(second));
}

#[test]
fn shipping_loop_redacts_nested_history_failures() {
    let root = TestRoot::create("redaction");
    let source = fixture_source(SecretFailureTransport);
    let (mut worker, control, _session_end) = start_controlled_worker(&root.0, source);
    let generation = control
        .wait_started(CONTROLLED_START_TIMEOUT)
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

#[test]
fn pre_seed_live_trades_survive_history_seeding() {
    let root = TestRoot::create("pre-seed");
    let release = Arc::new(AtomicBool::new(false));
    let source = BlockingHistorySource {
        release: Arc::clone(&release),
        adapter: CoinbaseHistoryCapabilityAdapter::try_with_transport(CandleTransport)
            .expect("blocking fixture capabilities validate"),
    };
    let (mut worker, control, _session_end) = start_controlled_worker(&root.0, source);
    let generation = control
        .wait_started(CONTROLLED_START_TIMEOUT)
        .expect("controlled generation starts");
    assert!(control.established(generation));
    thread::sleep(Duration::from_millis(100));
    // Live trades arrive and roll a minute while the covering fetch is blocked.
    assert!(control.trade(generation, fixture_trade(1, FIXED_CURRENT_MINUTE_SECONDS)));
    assert!(control.trade(
        generation,
        fixture_trade(2, FIXED_CURRENT_MINUTE_SECONDS + 60)
    ));
    thread::sleep(Duration::from_millis(200));
    release.store(true, Ordering::Release);

    assert_eq!(wait_for_live_snapshot(&mut worker, 2), (1, 350));
    assert!(control.trade(
        generation,
        fixture_trade(3, FIXED_CURRENT_MINUTE_SECONDS + 120)
    ));
    assert!(control.trade(
        generation,
        fixture_trade(4, FIXED_CURRENT_MINUTE_SECONDS + 180)
    ));
    assert_eq!(wait_for_live_delta(&mut worker), (350, 351));
    drop(worker);
    assert_eq!(
        control.wait_stopped(Duration::from_secs(1)),
        Some(generation)
    );
}

#[test]
fn shutdown_during_an_inflight_fetch_completes_boundedly() {
    let root = TestRoot::create("fetch-shutdown");
    let source = BlockingHistorySource {
        release: Arc::new(AtomicBool::new(false)),
        adapter: CoinbaseHistoryCapabilityAdapter::try_with_transport(CandleTransport)
            .expect("blocking fixture capabilities validate"),
    };
    let (worker, control, _session_end) = start_controlled_worker(&root.0, source);
    let generation = control
        .wait_started(CONTROLLED_START_TIMEOUT)
        .expect("controlled generation starts");
    assert!(control.established(generation));
    thread::sleep(Duration::from_millis(200));

    let shutdown_started = Instant::now();
    drop(worker);
    assert!(shutdown_started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        control.wait_stopped(Duration::from_secs(1)),
        Some(generation)
    );
}

#[test]
fn reselection_ends_the_session_cleanly_without_store_contention() {
    let root = TestRoot::create("reselect");
    let source = fixture_source(CandleTransport);
    let (mut worker, control, session_end) = start_controlled_worker(&root.0, source);
    let generation = control
        .wait_started(CONTROLLED_START_TIMEOUT)
        .expect("controlled generation starts");
    assert!(control.established(generation));

    let sequence = worker
        .try_select_coinbase(spot_product("ETH-USD"), ChartInterval::Minute5)
        .expect("selection enters the bounded command channel");
    assert_eq!(
        session_end
            .recv_timeout(Duration::from_secs(2))
            .expect("session reports its end"),
        Some(sequence)
    );
    assert_eq!(
        control.wait_stopped(Duration::from_secs(2)),
        Some(generation)
    );
    let (messages, _) = worker.drain_messages();
    for message in &messages {
        if let MarketWorkerMessage::State { message, .. } = message {
            assert!(
                !message.contains("already open"),
                "reselection must never contend for the store: {message}"
            );
        }
    }
    drop(worker);
}

#[test]
fn stale_selections_are_fenced_to_the_newest_requested_sequence() {
    let ui_thread = thread::current().id();
    thread::spawn(move || stale_selection_drain(ui_thread))
        .join()
        .expect("stale selection fencing holds on the worker thread");
}

fn stale_selection_drain(ui_thread: thread::ThreadId) {
    let root = TestRoot::create("stale-reselect");
    let profile = product_profile("BTC-USD".to_string()).expect("fixture profile validates");
    let config =
        CoinbaseConfig::try_new(vec![profile.product_id.clone()]).expect("product validates");
    let (driver, events, _control) = CoinbaseProviderDriver::new_controlled_with_wake(
        config,
        nonzero(PROVIDER_EVENT_CAPACITY),
        Arc::new(|| {}),
    );
    let worker = open_test_worker(
        &profile,
        root.0.clone(),
        ui_thread,
        MemoryVault,
        driver,
        catalog_key(),
    )
    .expect("controlled worker opens");
    let history_source = fixture_source(CandleTransport);
    let (message_tx, _message_rx) = market_worker_channel(nonzero(MESSAGE_CAPACITY));
    let mut running = prepare_running_worker(
        profile,
        super::OpenedWorker {
            worker,
            events,
            segment_key: segment_key(),
        },
        None,
        false,
        &history_source,
        &message_tx,
        0,
    )
    .expect("controlled worker prepares");
    let latest = AtomicU64::new(2);
    let (inbox_tx, inbox_rx) = mpsc::sync_channel(INBOX_CAPACITY);
    for sequence in [1, 2] {
        inbox_tx
            .send(WorkerInboxEvent::Command(
                crate::market_worker::MarketWorkerCommand::CoinbaseSelect(Box::new(
                    crate::market_worker::CoinbaseSelectionRequest {
                        sequence,
                        product: spot_product("ETH-USD"),
                        interval: ChartInterval::Minute5,
                    },
                )),
            ))
            .expect("selection enters the inbox");
    }
    let provider_wake_pending = AtomicBool::new(false);
    let signal = super::lifecycle::drain_worker_inbox(
        &inbox_rx,
        &mut None,
        &mut super::lifecycle::InboxDrainContext {
            worker: &mut running.worker,
            events: &running.events,
            state: &mut running.state,
            message_tx: &message_tx,
            provider_wake_pending: &provider_wake_pending,
            selection_sequence: &latest,
            active_selection_generation: 0,
            series: super::history::StreamingSeriesContext {
                profile: &running.profile,
                segment_key: &running.segment_key,
                instrument: &running.instrument,
                bar_definition: &running.bar_definition,
                worker_label: &running.worker_label,
            },
            model: &mut running.model,
        },
    )
    .expect("inbox drains");
    let super::lifecycle::DrainSignal::Reselect(request) = signal else {
        panic!("the newest selection must trigger exactly one reselection");
    };
    assert_eq!(request.sequence, 2);
}

#[test]
fn recent_phase_publishes_the_newest_page_before_the_full_range() {
    let mut profile = product_profile("BTC-USD".to_string()).expect("fixture profile validates");
    profile.interval = ChartInterval::Minute3;
    assert!(super::history::needs_recent_phase(profile.interval));

    let fetch = |phase| {
        let mut adapter = CoinbaseHistoryCapabilityAdapter::try_with_transport(CandleTransport)
            .expect("fixture capabilities validate");
        fetch_history_with_adapter(&profile, &mut adapter, FIXED_NOW_UNIX_NANOS, phase)
            .expect("fixture history fetches")
    };
    let recent = fetch(super::history::FetchPhase::Recent);
    let full = fetch(super::history::FetchPhase::Full);
    let recent_items = &recent.completion.page().items;
    let full_items = &full.completion.page().items;
    assert!(recent_items.len() < full_items.len());
    assert!(!recent_items.is_empty());
    assert_eq!(
        recent_items.last().map(|item| item.event_time_unix_nanos),
        full_items.last().map(|item| item.event_time_unix_nanos),
        "the recent phase ends at the same live boundary as the full range"
    );
}

#[test]
fn viewport_history_range_prefetches_and_aligns_to_interval() {
    let profile = product_profile("BTC-USD".to_string()).expect("fixture profile validates");
    let range = super::viewport_history_range(
        &profile,
        crate::market_worker::ChartViewportUpdate {
            start_unix_nanos: 1_700_000_061_000_000_000,
            end_unix_nanos: 1_700_000_181_000_000_000,
            selection_generation: 0,
        },
    )
    .expect("viewport range validates");
    assert_eq!(range.start_unix_nanos, 1_699_999_920_000_000_000);
    assert_eq!(range.end_unix_nanos, 1_700_000_220_000_000_000);
}

fn spot_product(product_id: &str) -> axiusflow_coinbase_market_adapter::CoinbaseSpotProduct {
    axiusflow_coinbase_market_adapter::CoinbaseSpotProduct {
        product_id: product_id.to_string(),
        instrument_id: format!("instrument:coinbase:{}", product_id.to_ascii_lowercase()),
        display_symbol: product_id.replace('-', "/"),
        base_currency: "ETH".to_string(),
        quote_currency: "USD".to_string(),
        price_scale: 2,
        quantity_scale: 8,
    }
}

fn fixture_source<T>(transport: T) -> FixtureHistorySource<T> {
    FixtureHistorySource {
        adapter: CoinbaseHistoryCapabilityAdapter::try_with_transport(transport)
            .expect("fixture history capabilities validate"),
    }
}

fn start_controlled_worker<H: super::HistorySource + 'static>(
    history_root: &Path,
    history_source: H,
) -> (
    MarketDataWorker,
    CoinbaseProviderFixtureControl,
    mpsc::Receiver<Option<u64>>,
) {
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
    let selection_sequence = Arc::new(AtomicU64::new(0));
    let (session_end_tx, session_end_rx) = mpsc::sync_channel::<Option<u64>>(1);
    let worker_selection = Arc::clone(&selection_sequence);
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
            inbox_tx,
            inbox_rx,
            provider_wake_pending: &provider_wake_pending,
            ui_diagnostics_rx: &ui_diagnostics_rx,
            selection_sequence: worker_selection,
            session_end: session_end_tx,
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
            Some(selection_sequence),
        ),
        control,
        session_end_rx,
    )
}

fn run_controlled_worker<H: super::HistorySource + 'static>(
    input: ControlledWorkerInput<'_, H>,
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
        &input.history_source,
        input.message_tx,
        0,
    )?;
    let (history_command_tx, history_command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let history_inbox_tx = input.inbox_tx.clone();
    let history_handle = thread::spawn(move || {
        let mut history_source = input.history_source;
        super::history_command_loop(&mut history_source, &history_command_rx, &history_inbox_tx);
    });
    let result = run_session(
        running,
        &history_command_tx,
        input.message_tx,
        &input.inbox_rx,
        input.provider_wake_pending,
        input.ui_diagnostics_rx,
        &input.selection_sequence,
    );
    let reselected = match &result {
        Ok(super::SessionEnd::Reselect(request)) => Some(request.sequence),
        _ => None,
    };
    let _ = input.session_end.send(reselected);
    let result = result.map(|_| ());
    drop(history_command_tx);
    let _ = history_handle.join();
    result
}

fn seed_corrupt_cache(root: &Path) {
    let profile = product_profile("BTC-USD".to_string()).expect("fixture profile validates");
    let mut source = fixture_source(CandleTransport);
    let range = super::history::history_request_range(
        &profile,
        FIXED_NOW_UNIX_NANOS,
        super::history::FetchPhase::Full,
    )
    .expect("fixture history range validates");
    let prepared = super::HistorySource::fetch(
        &mut source,
        &profile,
        FIXED_NOW_UNIX_NANOS,
        Arc::new(AtomicBool::new(false)),
        range,
    )
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
        MarketWorkerMessage::CoinbaseCatalog(result) => match result {
            Ok(products) => format!("Coinbase catalog products={}", products.len()),
            Err(error) => error.clone(),
        },
        MarketWorkerMessage::CoinbaseDom(_) => "Coinbase depth frame".to_string(),
        MarketWorkerMessage::CoinbaseSwitchMarker { .. } => "Coinbase switch marker".to_string(),
        MarketWorkerMessage::ChartViewport { .. } => "Chart viewport".to_string(),
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
