use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarClientModel, MarketBarModelOutcome,
    MarketBarReplayPort, MarketStreamPublication, ReplayStreamUpdate,
};
use axiusflow_desktop_history::{
    ControlPlaneState, HistoryDecoder, HistoryWorkerConfig, HydrationOutcome, HydrationRequest,
    ProviderConnectionState, StartupCacheState,
};
use axiusflow_desktop_provider_runtime::{
    ConnectTrigger, DesktopMarketWorker, DesktopMarketWorkerConfig, DesktopMarketWorkerError,
    DesktopProviderConfig, NetworkEvent, ProviderSessionDriver, SessionGeneration,
};
use axiusflow_desktop_storage::{
    CatalogKey, DataKind, HistoryScope, HistoryStore, PublicationOutcome, PublicationRequest,
    RecoveryAction, RetentionPolicy, SegmentEncryptionKey, SegmentIdentity,
};
use axiusflow_platform_runtime::CredentialVault;
use axiusflow_provider_history::{SequencedHistory, VerifiedHistorySnapshot};
use std::{
    fs,
    num::{NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
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
fn local_hydration_does_not_require_provider_or_control_plane() {
    let root = TestRoot::create("local-hydration");
    let key = segment_key();
    let identity = identity("local-hydration");
    let mut store = HistoryStore::open(root.path(), catalog_key(), 4).expect("store opens");
    assert!(matches!(
        store
            .publish(PublicationRequest {
                identity: &identity,
                payload: b"1,2",
                encryption_key: &key,
                retention: RetentionPolicy::UntilRevoked,
                recovery: RecoveryAction::ProviderRefetch,
                now_unix_seconds: 100,
            })
            .expect("local segment publishes"),
        PublicationOutcome::Published(_)
    ));
    drop(store);

    let mut worker = market_worker(root.path(), 1);
    let mut decoder = SequenceDecoder;
    let outcome = worker
        .hydrate_visible(
            HydrationRequest {
                identity: &identity,
                encryption_key: &key,
                now_unix_seconds: 101,
                startup_cache_state: StartupCacheState::Warm,
                provider_state: ProviderConnectionState::Offline,
                control_plane_state: ControlPlaneState::Unavailable,
                missing_recovery: RecoveryAction::ProviderRefetch,
            },
            &mut decoder,
        )
        .expect("local hydration succeeds without either network plane");
    let HydrationOutcome::Ready { publication, .. } = outcome else {
        panic!("local segment must hydrate");
    };
    assert_eq!(publication.watermark, 2);
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
                control_plane_state: ControlPlaneState::Unavailable,
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
