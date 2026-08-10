use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarClientModel, MarketBarModelOutcome,
    MarketStreamPublication, ReplayStreamUpdate,
};
use axiusflow_coinbase_market_adapter::{
    COINBASE_PUBLIC_ACCOUNT_ID, CoinbaseHistoryCapabilityAdapter, CoinbaseHistoryTransport,
    ENTITLEMENT_CLASS, decode_history_bar,
};
use axiusflow_desktop_history::{
    HistoryDecoder, HistoryWorkerConfig, HydrationOutcome, HydrationRequest,
    ProviderConnectionState, StartupCacheState,
};
use axiusflow_desktop_provider_runtime::{
    ConnectTrigger, DesktopMarketWorker, DesktopMarketWorkerConfig, DesktopMarketWorkerError,
    DesktopProviderConfig, HistoryCompletionInstall, NetworkEvent, ProviderSessionDriver,
    SessionGeneration,
};
use axiusflow_desktop_storage::{
    CatalogKey, DataKind, HistoryScope, HistorySeriesIdentity, HistoryStore, PublicationRequest,
    RecoveryAction, RetentionPolicy, SegmentEncryptionKey, SegmentIdentity,
};
use axiusflow_market_data::MarketBar;
use axiusflow_platform_runtime::CredentialVault;
use axiusflow_provider_history::{
    Completion, DataClass, HistoryPage, HistoryPageRequest, HistoryRange, HistoryScheduler,
    ProviderHistoryAdapter, RequestInterest, RequestPriority, SchedulerConfig, SequencedHistory,
    VerifiedHistorySnapshot,
};
use std::{
    cell::RefCell,
    fs,
    num::{NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
    rc::Rc,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

struct TestRoot(PathBuf);

impl TestRoot {
    fn create(name: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time follows Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "axiusflow-desktop-market-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test root creates");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone)]
struct MemoryVault;

impl CredentialVault for MemoryVault {
    type Error = ();

    fn store(&self, _key: &str, _secret: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    fn load(&self, _key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(Some(b"device-only-provider-token".to_vec()))
    }

    fn delete(&self, _key: &str) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Default)]
struct RecordingDriver {
    starts: Vec<SessionGeneration>,
    stops: Vec<SessionGeneration>,
}

impl ProviderSessionDriver for RecordingDriver {
    type Error = ();

    fn start_session(
        &mut self,
        generation: SessionGeneration,
        _credentials: &[u8],
    ) -> Result<(), Self::Error> {
        self.starts.push(generation);
        Ok(())
    }

    fn stop_session(&mut self, generation: SessionGeneration) -> Result<(), Self::Error> {
        self.stops.push(generation);
        Ok(())
    }
}

#[derive(Default)]
struct SequenceDecoder;

impl HistoryDecoder<u64> for SequenceDecoder {
    fn retained_decoded_bytes(&mut self, payload: &[u8]) -> Result<usize, String> {
        Ok(payload.len())
    }

    fn decode(
        &mut self,
        payload: &[u8],
        maximum_decoded_bytes: usize,
    ) -> Result<(Vec<SequencedHistory<u64>>, usize), String> {
        if payload.len() > maximum_decoded_bytes {
            return Err("decoded history exceeds its configured bound".to_string());
        }
        let values = std::str::from_utf8(payload)
            .map_err(|_| "invalid utf8".to_string())?
            .split(',')
            .map(|value| {
                let sequence = value
                    .parse::<u64>()
                    .map_err(|_| "invalid sequence".to_string())?;
                Ok(SequencedHistory {
                    sequence: nonzero_u64(sequence),
                    value: sequence,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok((values, payload.len()))
    }
}

struct SecretFailingDecoder;

impl HistoryDecoder<u64> for SecretFailingDecoder {
    fn retained_decoded_bytes(&mut self, payload: &[u8]) -> Result<usize, String> {
        Ok(payload.len())
    }

    fn decode(
        &mut self,
        _payload: &[u8],
        _maximum_decoded_bytes: usize,
    ) -> Result<(Vec<SequencedHistory<u64>>, usize), String> {
        Err("provider-secret-decoder-detail".to_string())
    }
}

#[derive(Clone)]
struct CoinbaseFixtureTransport {
    response: Vec<u8>,
    paths: Rc<RefCell<Vec<String>>>,
}

impl CoinbaseHistoryTransport for CoinbaseFixtureTransport {
    fn get(&mut self, path: &str) -> Result<Vec<u8>, String> {
        self.paths.borrow_mut().push(path.to_string());
        Ok(self.response.clone())
    }
}

fn scheduled_coinbase_completion(
    identity: &SegmentIdentity,
) -> (Completion, Rc<RefCell<Vec<String>>>) {
    scheduled_coinbase_completion_from_fixture(
        identity,
        br#"{"candles":[
            {"start":"1700000160","low":"37000.00","high":"37100.00","open":"37010.00","close":"37090.00","volume":"1.25000000"},
            {"start":"1700000100","low":"37020.00","high":"37080.00","open":"37020.00","close":"37070.00","volume":"0.75000000"},
            {"start":"1700000040","low":"36950.00","high":"37050.00","open":"37000.00","close":"37020.00","volume":"0.50000000"}
        ]}"#
        .to_vec(),
        RequestInterest::new(nonzero_u64(1)),
        |_| {},
    )
}

fn scheduled_coinbase_completion_from_fixture(
    identity: &SegmentIdentity,
    response: Vec<u8>,
    interest: RequestInterest,
    mutate_page: impl FnOnce(&mut HistoryPage),
) -> (Completion, Rc<RefCell<Vec<String>>>) {
    const NOW_UNIX_NANOS: i64 = 1_800_000_000_000_000_000;

    let paths = Rc::new(RefCell::new(Vec::new()));
    let mut adapter =
        CoinbaseHistoryCapabilityAdapter::try_with_transport(CoinbaseFixtureTransport {
            response,
            paths: paths.clone(),
        })
        .expect("Coinbase adapter configures");
    let mut scheduler = HistoryScheduler::try_new(
        adapter.capabilities().clone(),
        SchedulerConfig {
            maximum_queued_requests: nonzero(1),
            maximum_total_inflight: nonzero(1),
            maximum_interests_per_request: nonzero(1),
            maximum_continuations_per_request: nonzero(1),
            maximum_fetch_attempts: nonzero(1),
            adjacent_prefetch_windows: 0,
        },
    )
    .expect("history scheduler configures");
    scheduler
        .submit(
            HistoryPageRequest {
                provider_id: "coinbase".to_string(),
                account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
                entitlement_revision: ENTITLEMENT_CLASS.to_string(),
                instrument_id: identity.instrument_id.clone(),
                data_class: DataClass::Bars,
                resolution: identity.resolution.clone(),
                range: HistoryRange {
                    start_unix_nanos: identity.range_start_unix_nanos,
                    end_unix_nanos: identity.range_end_unix_nanos,
                },
                maximum_items: nonzero(3),
                continuation: None,
            },
            interest,
            RequestPriority::Visible,
            NOW_UNIX_NANOS,
        )
        .expect("visible request schedules");
    let dispatch = scheduler
        .dispatch_next(NOW_UNIX_NANOS, 0)
        .expect("dispatch succeeds")
        .dispatch
        .expect("request is immediately eligible");
    let mut page = adapter
        .fetch_page(&dispatch.request)
        .expect("Coinbase page fetches");
    mutate_page(&mut page);
    let completion = scheduler
        .complete(dispatch.dispatch_id, page, NOW_UNIX_NANOS)
        .expect("Coinbase page validates through the scheduler");
    (completion, paths)
}

fn coinbase_completion_install(
    binding: axiusflow_desktop_provider_runtime::HistoryCompletionBinding,
) -> HistoryCompletionInstall {
    HistoryCompletionInstall {
        binding,
        snapshot_generation: nonzero_u64(1),
        empty_cutover_watermark: None,
        startup_cache_state: StartupCacheState::Cold,
    }
}

#[test]
fn coinbase_scheduler_completion_installs_generation_fenced_history() {
    let root = TestRoot::create("coinbase-completion");
    let key = segment_key();
    let identity = coinbase_identity();
    let mut worker = market_bar_worker(root.path(), 1);
    let provider_generation = worker
        .connect(ConnectTrigger::Initial)
        .expect("provider session starts");
    worker
        .session_established(provider_generation)
        .expect("provider session streams");
    let binding = worker
        .begin_scheduled_history_handoff(
            provider_generation,
            identity.clone(),
            RequestInterest::new(nonzero_u64(1)),
            &key,
            1_800_000_000,
        )
        .expect("history handoff starts");
    let (initial, _) = scheduled_coinbase_completion_from_fixture(
        &identity,
        br#"{"candles":[
            {"start":"1700000040","low":"36950.00","high":"37050.00","open":"37000.00","close":"37020.00","volume":"0.50000000"}
        ]}"#
        .to_vec(),
        RequestInterest::new(nonzero_u64(1)),
        |_| {},
    );
    let initial_publication = worker
        .install_history_completion(
            provider_generation,
            &identity,
            &initial,
            coinbase_completion_install(binding),
            |_| Ok(size_of::<MarketBar>()),
            |item, maximum_item_bytes| {
                assert_eq!(maximum_item_bytes, size_of::<SequencedHistory<MarketBar>>());
                decode_history_bar(item).map(|bar| (bar, size_of::<MarketBar>()))
            },
        )
        .expect("scheduler completion installs atomically");
    let (completion, paths) = scheduled_coinbase_completion(&identity);
    let gap = completion
        .page()
        .items
        .last()
        .expect("fixture has a gap item");
    worker
        .push_history_live(
            provider_generation,
            &identity,
            SequencedHistory {
                sequence: nonzero_u64(gap.sequence),
                value: decode_history_bar(gap).expect("gap item decodes"),
            },
            size_of::<MarketBar>(),
        )
        .expect_err("sequence gap requires a recovery snapshot");
    let recovery_binding = worker
        .begin_scheduled_history_handoff(
            provider_generation,
            identity.clone(),
            RequestInterest::new(nonzero_u64(1)),
            &key,
            1_800_000_000,
        )
        .expect("active handoff accepts recovery work");
    assert_ne!(binding, recovery_binding);
    let publication = worker
        .install_history_completion(
            provider_generation,
            &identity,
            &completion,
            HistoryCompletionInstall {
                binding: recovery_binding,
                snapshot_generation: nonzero_u64(2),
                empty_cutover_watermark: None,
                startup_cache_state: StartupCacheState::Cold,
            },
            |_| Ok(size_of::<MarketBar>()),
            |item, maximum_item_bytes| {
                assert_eq!(maximum_item_bytes, size_of::<SequencedHistory<MarketBar>>());
                decode_history_bar(item).map(|bar| (bar, size_of::<MarketBar>()))
            },
        )
        .expect("recovery completion installs without restarting the handoff");

    assert_eq!(initial_publication.values.len(), 1);
    assert_eq!(publication.values.len(), 3);
    assert_eq!(publication.values[0].value.open, 3_700_000);
    assert_eq!(publication.watermark, publication.values[2].sequence.get());
    assert_eq!(
        paths.borrow().as_slice(),
        [
            "/api/v3/brokerage/market/products/BTC-USD/candles?start=1700000040&end=1700000160&granularity=ONE_MINUTE&limit=3"
        ]
    );
}

#[test]
fn history_completion_is_bound_to_the_scheduled_segment_revision() {
    let root = TestRoot::create("coinbase-completion-revision");
    let key = segment_key();
    let identity = coinbase_identity();
    let mut revised_identity = identity.clone();
    revised_identity.source_revision = 2;
    let mut worker = market_bar_worker(root.path(), 2);
    let provider_generation = worker
        .connect(ConnectTrigger::Initial)
        .expect("provider session starts");
    worker
        .session_established(provider_generation)
        .expect("provider session streams");
    let old_binding = worker
        .begin_scheduled_history_handoff(
            provider_generation,
            identity.clone(),
            RequestInterest::new(nonzero_u64(1)),
            &key,
            1_800_000_000,
        )
        .expect("history handoff starts");
    let (completion, _) = scheduled_coinbase_completion(&identity);
    worker
        .end_history_handoff(provider_generation, &identity)
        .expect("old revision handoff ends");
    let new_binding = worker
        .begin_scheduled_history_handoff(
            provider_generation,
            revised_identity.clone(),
            RequestInterest::new(nonzero_u64(1)),
            &key,
            1_800_000_000,
        )
        .expect("new revision handoff starts");
    assert_ne!(old_binding, new_binding);
    let mut adjacent_identity = revised_identity.clone();
    adjacent_identity.range_start_unix_nanos = revised_identity.range_end_unix_nanos;
    adjacent_identity.range_end_unix_nanos = 1_700_000_400_000_000_000;
    worker
        .begin_scheduled_history_handoff(
            provider_generation,
            adjacent_identity,
            RequestInterest::new(nonzero_u64(1)),
            &key,
            1_800_000_000,
        )
        .expect("shared-interest adjacent handoff starts");
    let estimated = std::cell::Cell::new(false);
    assert!(matches!(
        worker.install_history_completion(
            provider_generation,
            &revised_identity,
            &completion,
            coinbase_completion_install(old_binding),
            |_| {
                estimated.set(true);
                Ok(size_of::<MarketBar>())
            },
            |item, _| decode_history_bar(item).map(|bar| (bar, size_of::<MarketBar>())),
        ),
        Err(DesktopMarketWorkerError::HistoryCompletionMismatch)
    ));
    assert!(!estimated.get());
}

#[test]
fn history_completion_rejects_mismatch_empty_and_gapped_pages() {
    let root = TestRoot::create("coinbase-completion-rejection");
    let key = segment_key();
    let identity = coinbase_identity();
    let mut worker = market_bar_worker(root.path(), 1);
    let provider_generation = worker
        .connect(ConnectTrigger::Initial)
        .expect("provider session starts");
    worker
        .session_established(provider_generation)
        .expect("provider session streams");
    let binding = worker
        .begin_scheduled_history_handoff(
            provider_generation,
            identity.clone(),
            RequestInterest::new(nonzero_u64(1)),
            &key,
            1_800_000_000,
        )
        .expect("history handoff starts");
    let (uninterested, _) = scheduled_coinbase_completion_from_fixture(
        &identity,
        br#"{"candles":[]}"#.to_vec(),
        RequestInterest::new(nonzero_u64(2)),
        |_| {},
    );
    let decoded = std::cell::Cell::new(false);
    assert!(matches!(
        worker.install_history_completion(
            provider_generation,
            &identity,
            &uninterested,
            coinbase_completion_install(binding),
            |_| Ok(size_of::<MarketBar>()),
            |_, _| {
                decoded.set(true);
                Err("must not decode mismatched work".to_string())
            },
        ),
        Err(DesktopMarketWorkerError::HistoryCompletionMismatch)
    ));
    assert!(!decoded.get());

    let (empty, _) = scheduled_coinbase_completion_from_fixture(
        &identity,
        br#"{"candles":[]}"#.to_vec(),
        RequestInterest::new(nonzero_u64(1)),
        |_| {},
    );
    assert!(matches!(
        worker.install_history_completion(
            provider_generation,
            &identity,
            &empty,
            coinbase_completion_install(binding),
            |_| Ok(size_of::<MarketBar>()),
            |item, _| decode_history_bar(item).map(|bar| (bar, size_of::<MarketBar>())),
        ),
        Err(DesktopMarketWorkerError::HistoryEmptyCutoverMissing)
    ));

    let (gapped, _) = scheduled_coinbase_completion_from_fixture(
        &identity,
        br#"{"candles":[
            {"start":"1700000160","low":"37000.00","high":"37100.00","open":"37010.00","close":"37090.00","volume":"1.25000000"},
            {"start":"1700000100","low":"37020.00","high":"37080.00","open":"37020.00","close":"37070.00","volume":"0.75000000"},
            {"start":"1700000040","low":"36950.00","high":"37050.00","open":"37000.00","close":"37020.00","volume":"0.50000000"}
        ]}"#
        .to_vec(),
        RequestInterest::new(nonzero_u64(1)),
        |page| {
            page.items.remove(1);
        },
    );
    let estimated = std::cell::Cell::new(false);
    assert!(matches!(
        worker.install_history_completion(
            provider_generation,
            &identity,
            &gapped,
            coinbase_completion_install(binding),
            |_| {
                estimated.set(true);
                Ok(size_of::<MarketBar>())
            },
            |item, _| decode_history_bar(item).map(|bar| (bar, size_of::<MarketBar>())),
        ),
        Err(DesktopMarketWorkerError::HistoryContinuity)
    ));
    assert!(!estimated.get());
}

#[test]
fn history_completion_checks_capacity_before_decoding() {
    let root = TestRoot::create("coinbase-completion-capacity");
    let key = segment_key();
    let identity = coinbase_identity();
    let mut worker = market_bar_worker(root.path(), 1);
    let provider_generation = worker
        .connect(ConnectTrigger::Initial)
        .expect("provider session starts");
    worker
        .session_established(provider_generation)
        .expect("provider session streams");
    let binding = worker
        .begin_scheduled_history_handoff(
            provider_generation,
            identity.clone(),
            RequestInterest::new(nonzero_u64(1)),
            &key,
            1_800_000_000,
        )
        .expect("history handoff starts");
    let (completion, _) = scheduled_coinbase_completion(&identity);
    let decoded = std::cell::Cell::new(false);
    assert!(matches!(
        worker.install_history_completion(
            provider_generation,
            &identity,
            &completion,
            coinbase_completion_install(binding),
            |_| Ok(4097),
            |_, _| {
                decoded.set(true);
                Err("must check capacity before decoding".to_string())
            },
        ),
        Err(DesktopMarketWorkerError::HistoryResourceLimit)
    ));
    assert!(!decoded.get());
}

#[test]
fn network_loss_retires_handoffs_and_fences_stale_callbacks() {
    let root = TestRoot::create("generation-fence");
    let key = segment_key();
    let identity = identity("generation-fence");
    let mut worker = market_worker(root.path(), 2);

    let first = worker
        .connect(ConnectTrigger::Initial)
        .expect("first session starts");
    worker
        .session_established(first)
        .expect("first session streams");
    worker
        .begin_history_handoff(first, identity.clone(), &key, 100)
        .expect("first handoff starts");
    worker
        .push_history_live(first, &identity, sequenced(2), 8)
        .expect("first live item buffers");

    worker
        .handle_network_event(NetworkEvent::Unavailable)
        .expect("network loss fences the first session");
    let second = worker
        .handle_network_event(NetworkEvent::Available)
        .expect("network restoration starts a fresh session")
        .expect("connection remains desired");
    worker
        .session_established(second)
        .expect("second session streams");
    worker
        .begin_history_handoff(second, identity.clone(), &key, 101)
        .expect("retired identity starts under the new generation");

    assert!(matches!(
        worker.end_history_handoff(first, &identity),
        Err(DesktopMarketWorkerError::HandoffGenerationMismatch)
    ));
    assert!(matches!(
        worker.push_history_live(first, &identity, sequenced(3), 8),
        Err(DesktopMarketWorkerError::HandoffGenerationMismatch)
    ));
    let snapshot = VerifiedHistorySnapshot::try_new(nonzero_u64(1), vec![sequenced(1)])
        .expect("snapshot is contiguous");
    assert!(matches!(
        worker.install_history_snapshot(first, &identity, snapshot, 8, StartupCacheState::Cold),
        Err(DesktopMarketWorkerError::HandoffGenerationMismatch)
    ));

    let snapshot = VerifiedHistorySnapshot::try_new(nonzero_u64(1), vec![sequenced(1)])
        .expect("snapshot is contiguous");
    let publication = worker
        .install_history_snapshot(second, &identity, snapshot, 8, StartupCacheState::Cold)
        .expect("current generation installs history");
    assert_eq!(publication.watermark, 1);
}

#[test]
fn generation_and_handoff_bounds_fail_before_history_mutation() {
    let root = TestRoot::create("handoff-bounds");
    let key = segment_key();
    let first_identity = identity("first");
    let second_identity = identity("second");
    let mut worker = market_worker(root.path(), 1);
    let first_generation = worker
        .connect(ConnectTrigger::Initial)
        .expect("session starts");
    worker
        .session_established(first_generation)
        .expect("session streams");
    worker
        .handle_network_event(NetworkEvent::Unavailable)
        .expect("network loss fences the first generation");
    let generation = worker
        .handle_network_event(NetworkEvent::Available)
        .expect("network restoration reconnects")
        .expect("connection remains desired");
    worker
        .session_established(generation)
        .expect("replacement session streams");
    assert!(matches!(
        worker.begin_history_handoff(first_generation, second_identity.clone(), &key, 100),
        Err(DesktopMarketWorkerError::HandoffGenerationMismatch)
    ));
    worker
        .begin_history_handoff(generation, first_identity, &key, 100)
        .expect("first handoff starts");
    assert!(matches!(
        worker.begin_history_handoff(generation, second_identity.clone(), &key, 100),
        Err(DesktopMarketWorkerError::HandoffLimitReached { maximum: 1 })
    ));
    assert!(matches!(
        worker.push_history_live(generation, &second_identity, sequenced(1), 8),
        Err(DesktopMarketWorkerError::HandoffNotTracked)
    ));
}

#[test]
fn publication_overflow_retires_history_before_retry() {
    let root = TestRoot::create("overflow-retires-history");
    let key = segment_key();
    let identity = identity("overflow-retires-history");
    let mut worker = market_worker_with_event_capacity(root.path(), 1, 3);
    let first = worker
        .connect(ConnectTrigger::Initial)
        .expect("session starts");
    worker.session_established(first).expect("session streams");
    worker
        .begin_history_handoff(first, identity.clone(), &key, 100)
        .expect("handoff starts");
    worker
        .publish(first, publication())
        .expect("last queue slot accepts a publication");
    assert!(matches!(
        worker.publish(first, publication()),
        Err(DesktopMarketWorkerError::Provider(_))
    ));
    while worker
        .try_recv_provider_event()
        .expect("event queue remains readable")
        .is_some()
    {}

    let second = worker
        .connect(ConnectTrigger::Retry)
        .expect("overflow recovery starts a replacement");
    worker
        .session_established(second)
        .expect("replacement session streams");
    worker
        .begin_history_handoff(second, identity, &key, 101)
        .expect("overflow retirement released the old handoff");
}

#[test]
fn local_hydration_does_not_require_provider() {
    let root = TestRoot::create("local-hydration");
    let key = segment_key();
    let identity = identity("local-hydration");
    let newer_identity = SegmentIdentity {
        range_start_unix_nanos: identity.range_start_unix_nanos + 60_000_000_000,
        range_end_unix_nanos: identity.range_end_unix_nanos + 60_000_000_000,
        ..identity.clone()
    };
    let mut writer = market_worker(root.path(), 1);
    for _ in 0..2 {
        writer
            .persist_history_segment(PublicationRequest {
                identity: &identity,
                payload: b"1,2",
                encryption_key: &key,
                retention: RetentionPolicy::UntilRevoked,
                recovery: RecoveryAction::ProviderRefetch,
                now_unix_seconds: 100,
            })
            .expect("immutable local segment persists idempotently");
    }
    writer
        .persist_history_segment(PublicationRequest {
            identity: &newer_identity,
            payload: b"3,4",
            encryption_key: &key,
            retention: RetentionPolicy::UntilRevoked,
            recovery: RecoveryAction::ProviderRefetch,
            now_unix_seconds: 101,
        })
        .expect("newer immutable segment persists");
    let latest = writer
        .latest_history_identity(
            HistorySeriesIdentity {
                scope: &identity.scope,
                instrument_id: &identity.instrument_id,
                data_kind: identity.data_kind,
                resolution: &identity.resolution,
                source_revision: identity.source_revision,
                schema_revision: identity.schema_revision,
                calendar_revision: identity.calendar_revision,
                adjustment_revision: identity.adjustment_revision,
                correction_revision: identity.correction_revision,
            },
            102,
        )
        .expect("latest retained identity query succeeds")
        .expect("retained identity exists");
    assert_eq!(latest, newer_identity);
    drop(writer);

    let mut worker = market_worker(root.path(), 1);
    let mut decoder = SequenceDecoder;
    let outcome = worker
        .hydrate_visible(
            HydrationRequest {
                identity: &latest,
                encryption_key: &key,
                now_unix_seconds: 101,
                startup_cache_state: StartupCacheState::Warm,
                provider_state: ProviderConnectionState::Offline,
                missing_recovery: RecoveryAction::ProviderRefetch,
            },
            &mut decoder,
        )
        .expect("local hydration succeeds without either network plane");
    let HydrationOutcome::Ready { publication, .. } = outcome else {
        panic!("local segment must hydrate");
    };
    assert_eq!(publication.watermark, 4);
}

#[test]
fn history_errors_do_not_expose_nested_diagnostics() {
    let root = TestRoot::create("redacted-history-errors");
    let key = segment_key();
    let identity = identity("redacted-history-errors");
    let mut store = HistoryStore::open(root.path(), catalog_key(), 4).expect("store opens");
    store
        .publish(PublicationRequest {
            identity: &identity,
            payload: b"1",
            encryption_key: &key,
            retention: RetentionPolicy::UntilRevoked,
            recovery: RecoveryAction::ProviderRefetch,
            now_unix_seconds: 100,
        })
        .expect("local segment publishes");
    drop(store);

    let mut worker = market_worker(root.path(), 1);
    let error = worker
        .hydrate_visible(
            HydrationRequest {
                identity: &identity,
                encryption_key: &key,
                now_unix_seconds: 101,
                startup_cache_state: StartupCacheState::Warm,
                provider_state: ProviderConnectionState::Offline,
                missing_recovery: RecoveryAction::ProviderRefetch,
            },
            &mut SecretFailingDecoder,
        )
        .expect_err("decoder failure is surfaced");
    assert!(matches!(error, DesktopMarketWorkerError::HistoryDecode));
    assert!(!format!("{error:?} {error}").contains("provider-secret-decoder-detail"));
}

fn market_worker(
    root: &Path,
    maximum_handoffs: usize,
) -> DesktopMarketWorker<u64, MemoryVault, RecordingDriver> {
    market_worker_with_event_capacity(root, maximum_handoffs, 32)
}

fn market_worker_with_event_capacity(
    root: &Path,
    maximum_handoffs: usize,
    event_capacity: usize,
) -> DesktopMarketWorker<u64, MemoryVault, RecordingDriver> {
    let ui_thread = thread::spawn(|| thread::current().id())
        .join()
        .expect("ui thread identity is captured");
    DesktopMarketWorker::try_open(
        MemoryVault,
        RecordingDriver::default(),
        "provider-session",
        root,
        catalog_key(),
        ui_thread,
        DesktopMarketWorkerConfig {
            provider: DesktopProviderConfig::new(nonzero(event_capacity), nonzero(64)),
            history: HistoryWorkerConfig {
                maximum_cache_entries: nonzero(4),
                maximum_decoded_bytes: nonzero(1024),
                maximum_charts: nonzero(4),
                maximum_segment_read_bytes: nonzero(1024),
                maximum_buffered_live: nonzero(4),
                maximum_handoffs: nonzero(maximum_handoffs),
            },
            maximum_catalog_entries: 4,
        },
    )
    .expect("market worker opens")
}

fn market_bar_worker(
    root: &Path,
    maximum_handoffs: usize,
) -> DesktopMarketWorker<MarketBar, MemoryVault, RecordingDriver> {
    let ui_thread = thread::spawn(|| thread::current().id())
        .join()
        .expect("ui thread identity is captured");
    DesktopMarketWorker::try_open(
        MemoryVault,
        RecordingDriver::default(),
        "coinbase-public-session",
        root,
        catalog_key(),
        ui_thread,
        DesktopMarketWorkerConfig {
            provider: DesktopProviderConfig::new(nonzero(32), nonzero(64)),
            history: HistoryWorkerConfig {
                maximum_cache_entries: nonzero(4),
                maximum_decoded_bytes: nonzero(4096),
                maximum_charts: nonzero(4),
                maximum_segment_read_bytes: nonzero(4096),
                maximum_buffered_live: nonzero(4),
                maximum_handoffs: nonzero(maximum_handoffs),
            },
            maximum_catalog_entries: 4,
        },
    )
    .expect("market-bar worker opens")
}

fn publication() -> MarketStreamPublication {
    let source = EmbeddedReplaySource;
    let snapshot = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
        .expect("embedded snapshot is valid");
    let update = ReplayStreamUpdate::Snapshot(snapshot);
    let mut model = MarketBarClientModel::new(nonzero(8));
    let MarketBarModelOutcome::Published(generation) = model
        .apply_update(update.clone())
        .expect("snapshot publishes")
    else {
        panic!("snapshot must publish");
    };
    MarketStreamPublication::try_new("direct-provider".to_string(), update, generation)
        .expect("publication evidence agrees")
}

fn catalog_key() -> CatalogKey {
    CatalogKey::try_new("catalog-key-v1".to_string(), [0x41; 32]).expect("catalog key is valid")
}

fn segment_key() -> SegmentEncryptionKey {
    SegmentEncryptionKey::try_new("segment-key-v1".to_string(), [0x52; 32])
        .expect("segment key is valid")
}

fn identity(instrument: &str) -> SegmentIdentity {
    SegmentIdentity {
        scope: HistoryScope {
            provider_id: "provider".to_string(),
            account_id: "account".to_string(),
            entitlement_revision: "rights-1".to_string(),
        },
        instrument_id: instrument.to_string(),
        data_kind: DataKind::Bars,
        resolution: "1m".to_string(),
        range_start_unix_nanos: 1,
        range_end_unix_nanos: 61,
        source_revision: 1,
        schema_revision: 1,
        calendar_revision: 1,
        adjustment_revision: 1,
        correction_revision: 1,
    }
}

fn coinbase_identity() -> SegmentIdentity {
    SegmentIdentity {
        scope: HistoryScope {
            provider_id: "coinbase".to_string(),
            account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
            entitlement_revision: ENTITLEMENT_CLASS.to_string(),
        },
        instrument_id: "instrument:coinbase:btc:usd".to_string(),
        data_kind: DataKind::Bars,
        resolution: "1m".to_string(),
        range_start_unix_nanos: 1_700_000_040_000_000_000,
        range_end_unix_nanos: 1_700_000_220_000_000_000,
        source_revision: 1,
        schema_revision: 1,
        calendar_revision: 1,
        adjustment_revision: 1,
        correction_revision: 1,
    }
}

fn sequenced(sequence: u64) -> SequencedHistory<u64> {
    SequencedHistory {
        sequence: nonzero_u64(sequence),
        value: sequence,
    }
}

fn nonzero(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("test bound is nonzero")
}

fn nonzero_u64(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test sequence is nonzero")
}
