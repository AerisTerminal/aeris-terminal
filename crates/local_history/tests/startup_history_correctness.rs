use axiusflow_local_history::{
    ChartId, HistoryDecoder, HistoryWorker, HistoryWorkerConfig, HydrationOutcome,
    HydrationRequest, LocalHistoryError, ProviderConnectionState, StartupCacheState,
};
use axiusflow_local_storage::{
    AvailabilityReason, CatalogKey, DataKind, HistoryScope, HistoryStore, PublicationOutcome,
    PublicationRequest, RecoveryAction, RetentionPolicy, SegmentEncryptionKey, SegmentIdentity,
};
use axiusflow_provider_history::{SequencedHistory, VerifiedHistorySnapshot};
use std::{
    fs,
    num::{NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
    sync::Arc,
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
            "axiusflow-desktop-history-{name}-{}-{unique}",
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

#[derive(Default)]
struct SequenceDecoder {
    calls: usize,
}

impl HistoryDecoder<u64> for SequenceDecoder {
    fn retained_decoded_bytes(&mut self, payload: &[u8]) -> Result<usize, String> {
        Ok(payload.len())
    }

    fn decode(
        &mut self,
        payload: &[u8],
        maximum_decoded_bytes: usize,
    ) -> Result<(Vec<SequencedHistory<u64>>, usize), String> {
        self.calls += 1;
        if payload.len() > maximum_decoded_bytes {
            return Err("decoded history exceeds its configured bound".to_string());
        }
        let text = std::str::from_utf8(payload).map_err(|_| "invalid utf8".to_string())?;
        let values = text
            .split(',')
            .map(|value| {
                let sequence = value
                    .parse::<u64>()
                    .map_err(|_| "invalid sequence".to_string())?;
                Ok(SequencedHistory {
                    sequence: NonZeroU64::new(sequence)
                        .ok_or_else(|| "zero sequence".to_string())?,
                    value: sequence,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok((values, payload.len()))
    }
}

#[test]
fn startup_matrix_is_local_deterministic_and_recovers_explicitly() {
    let root = TestRoot::create("matrix");
    let root_path = root.path().to_path_buf();
    let ui_thread = thread::current().id();
    thread::spawn(move || {
        let key = segment_key();
        let populated_cold = identity("btc-usd", 1);
        let populated_warm = identity("eth-usd", 1);
        let corrupt = identity("sol-usd", 1);
        let expanded = identity("expanded", 1);
        let oversized = identity("oversized", 1);
        let gapped = identity("gapped", 1);
        let mut store = HistoryStore::open(&root_path, catalog_key(), 32).expect("store opens");
        publish_with_retention(
            &mut store,
            &populated_cold,
            &key,
            b"1,2,3",
            RetentionPolicy::UntilUnixSeconds(102),
        );
        publish(&mut store, &populated_warm, &key, b"11,12");
        let corrupt_file = publish(&mut store, &corrupt, &key, b"21,22");
        let expanded_file = publish(&mut store, &expanded, &key, b"23,24");
        let oversized_file = publish(&mut store, &oversized, &key, &[b'1'; 65]);
        publish(&mut store, &gapped, &key, b"31,33");
        corrupt_segment(&root_path.join("segments").join(corrupt_file));
        expand_segment(&root_path.join("segments").join(expanded_file), 114);
        corrupt_segment(&root_path.join("segments").join(oversized_file));
        drop(store);

        let mut worker =
            HistoryWorker::try_open(&root_path, catalog_key(), 32, ui_thread, config())
                .expect("dedicated worker is accepted");
        let mut decoder = SequenceDecoder::default();
        assert_empty_and_populated_cases(
            &mut worker,
            &key,
            &mut decoder,
            &populated_cold,
            &populated_warm,
        );
        assert_recovery_cases(
            &mut worker,
            &key,
            &mut decoder,
            &corrupt,
            &expanded,
            &oversized,
            &gapped,
        );
        let metrics = worker.metrics();
        assert_eq!(metrics.decode_operations, 3);
        assert_eq!(metrics.storage_bytes_read, 15);
        assert_eq!(metrics.memory_cache_hits, 1);
        assert_eq!(metrics.memory_cache_misses, metrics.storage_reads);
    })
    .join()
    .expect("worker thread completes");
}

fn assert_empty_and_populated_cases(
    worker: &mut HistoryWorker<u64>,
    key: &SegmentEncryptionKey,
    decoder: &mut SequenceDecoder,
    populated_cold: &SegmentIdentity,
    populated_warm: &SegmentIdentity,
) {
    for (instrument, cache_state) in [
        ("empty-cold", StartupCacheState::Cold),
        ("empty-warm", StartupCacheState::Warm),
    ] {
        assert_provider_fetch(worker.hydrate_visible(
            hydration_request(
                &identity(instrument, 1),
                key,
                cache_state,
                ProviderConnectionState::Online,
            ),
            decoder,
        ));
    }
    let cold = ready(worker.hydrate_visible(
        hydration_request(
            populated_cold,
            key,
            StartupCacheState::Cold,
            ProviderConnectionState::Offline,
        ),
        decoder,
    ));
    assert_eq!(cold.0.values.len(), 3);
    assert!(!cold.1);
    let warm = ready(worker.hydrate_visible(
        hydration_request(
            populated_warm,
            key,
            StartupCacheState::Warm,
            ProviderConnectionState::Online,
        ),
        decoder,
    ));
    assert_eq!(warm.0.startup_cache_state, StartupCacheState::Warm);
    assert!(!warm.1);
    let decoder_calls = decoder.calls;
    let memory_hit = ready(worker.hydrate_visible(
        hydration_request(
            populated_cold,
            key,
            StartupCacheState::Warm,
            ProviderConnectionState::Offline,
        ),
        decoder,
    ));
    assert!(memory_hit.1);
    assert_eq!(decoder.calls, decoder_calls);
    assert_cached_access_policy(worker, key, decoder, populated_cold);
}

fn assert_cached_access_policy(
    worker: &mut HistoryWorker<u64>,
    key: &SegmentEncryptionKey,
    decoder: &mut SequenceDecoder,
    populated_cold: &SegmentIdentity,
) {
    let wrong_key = SegmentEncryptionKey::try_new("wrong-key".to_string(), [0x99; 32])
        .expect("wrong fixture key is valid");
    assert!(matches!(
        worker.current_publication(populated_cold, &wrong_key, 101),
        Err(LocalHistoryError::Storage(
            axiusflow_local_storage::LocalStorageError::SegmentKeyMismatch
        ))
    ));
    assert!(matches!(
        worker.bind_chart(ChartId(99), populated_cold, &wrong_key, 101),
        Err(LocalHistoryError::Storage(
            axiusflow_local_storage::LocalStorageError::SegmentKeyMismatch
        ))
    ));
    assert!(matches!(
        worker.hydrate_visible(
            hydration_request(
                populated_cold,
                &wrong_key,
                StartupCacheState::Warm,
                ProviderConnectionState::Online,
            ),
            decoder,
        ),
        Err(LocalHistoryError::Storage(
            axiusflow_local_storage::LocalStorageError::SegmentKeyMismatch
        ))
    ));
    let mut expired_request = hydration_request(
        populated_cold,
        key,
        StartupCacheState::Warm,
        ProviderConnectionState::Online,
    );
    expired_request.now_unix_seconds = 102;
    assert!(
        worker
            .current_publication(populated_cold, &wrong_key, 102)
            .expect("expiration is evaluated before key mismatch")
            .is_none()
    );
    assert!(matches!(
        worker.hydrate_visible(expired_request, decoder),
        Ok(HydrationOutcome::ProviderFetchRequired {
            reason: AvailabilityReason::NotCached,
        })
    ));
}

fn assert_recovery_cases(
    worker: &mut HistoryWorker<u64>,
    key: &SegmentEncryptionKey,
    decoder: &mut SequenceDecoder,
    corrupt: &SegmentIdentity,
    expanded: &SegmentIdentity,
    oversized: &SegmentIdentity,
    gapped: &SegmentIdentity,
) {
    assert!(matches!(
        worker.hydrate_visible(
            hydration_request(
                &identity("offline", 1),
                key,
                StartupCacheState::Cold,
                ProviderConnectionState::Offline,
            ),
            decoder,
        ),
        Ok(HydrationOutcome::OfflineUnavailable {
            reason: AvailabilityReason::NotCached,
            recovery: RecoveryAction::ProviderRefetch,
        })
    ));
    let live_only = identity("live-only", 1);
    let mut live_only_request = hydration_request(
        &live_only,
        key,
        StartupCacheState::Cold,
        ProviderConnectionState::Online,
    );
    live_only_request.missing_recovery = RecoveryAction::LiveOnly;
    assert!(matches!(
        worker.hydrate_visible(live_only_request, decoder),
        Ok(HydrationOutcome::LiveOnly {
            reason: AvailabilityReason::NotCached,
        })
    ));
    assert!(matches!(
        worker.hydrate_visible(
            hydration_request(
                gapped,
                key,
                StartupCacheState::Cold,
                ProviderConnectionState::Online,
            ),
            decoder,
        ),
        Err(LocalHistoryError::Decode(_))
    ));
    assert!(matches!(
        worker.hydrate_visible(
            hydration_request(
                oversized,
                key,
                StartupCacheState::Cold,
                ProviderConnectionState::Online,
            ),
            decoder,
        ),
        Err(LocalHistoryError::DecodedHistoryTooLarge {
            requested: 65,
            maximum: 64,
        })
    ));
    for identity in [identity("not-cached", 1), identity("btc-usd", 2)] {
        assert_provider_fetch(worker.hydrate_visible(
            hydration_request(
                &identity,
                key,
                StartupCacheState::Cold,
                ProviderConnectionState::Online,
            ),
            decoder,
        ));
    }
    for identity in [corrupt, expanded] {
        assert!(matches!(
            worker.hydrate_visible(
                hydration_request(
                    identity,
                    key,
                    StartupCacheState::Cold,
                    ProviderConnectionState::Offline,
                ),
                decoder,
            ),
            Ok(HydrationOutcome::OfflineUnavailable {
                reason: AvailabilityReason::Quarantined,
                ..
            })
        ));
    }
}

#[test]
fn multi_chart_cache_and_handoff_are_bounded_and_atomic() {
    let root = TestRoot::create("handoff");
    let root_path = root.path().to_path_buf();
    let ui_thread = thread::current().id();
    thread::spawn(move || {
        let mut handoff_config = config();
        handoff_config.maximum_decoded_bytes = nonzero(256);
        let mut worker =
            HistoryWorker::try_open(root_path, catalog_key(), 32, ui_thread, handoff_config)
                .expect("dedicated worker is accepted");
        let identity = identity("btc-usd", 1);
        worker
            .begin_handoff(identity.clone(), &segment_key(), 101)
            .expect("handoff starts");
        assert!(
            worker
                .push_live(&identity, item(3), 8)
                .expect("live buffers")
                .is_none()
        );
        assert!(
            worker
                .push_live(&identity, item(4), 8)
                .expect("live buffers")
                .is_none()
        );
        let publication = worker
            .install_snapshot(
                &identity,
                VerifiedHistorySnapshot::try_new(
                    NonZeroU64::new(7).expect("nonzero"),
                    vec![item(1), item(2)],
                )
                .expect("snapshot is contiguous"),
                16,
                StartupCacheState::Cold,
            )
            .expect("snapshot and live publish atomically");
        assert_eq!(sequences(&publication), vec![1, 2, 3, 4]);

        let first = worker
            .bind_chart(ChartId(1), &identity, &segment_key(), 101)
            .expect("first chart binds")
            .expect("publication exists");
        let second = worker
            .bind_chart(ChartId(2), &identity, &segment_key(), 101)
            .expect("second chart binds")
            .expect("publication exists");
        assert!(Arc::ptr_eq(&first, &second));

        assert!(
            worker
                .push_live(&identity, item(4), 8)
                .expect("duplicate is ignored")
                .is_none()
        );
        assert_eq!(sequences(&first), vec![1, 2, 3, 4]);
        let live = worker
            .push_live(&identity, item(5), 8)
            .expect("contiguous live item is accepted")
            .expect("new immutable generation publishes");
        assert_eq!(sequences(&live), vec![1, 2, 3, 4, 5]);
        assert_eq!(sequences(&first), vec![1, 2, 3, 4]);
        assert_eq!(
            worker.cached_decoded_bytes().expect("worker owns cache"),
            72
        );
        drop(publication);
        drop(first);
        drop(second);
        assert_eq!(
            worker.cached_decoded_bytes().expect("worker owns cache"),
            40
        );

        assert!(matches!(
            worker.push_live(&identity, item(6), 225),
            Err(LocalHistoryError::DecodedHistoryTooLarge {
                requested: 265,
                maximum: 256,
            })
        ));
        let after_overflow = worker
            .current_publication(&identity, &segment_key(), 101)
            .expect("worker owns cache")
            .expect("overflow retains last publication");
        assert!(Arc::ptr_eq(&live, &after_overflow));
        assert!(matches!(
            worker.push_live(&identity, item(6), 8),
            Err(LocalHistoryError::Provider(_))
        ));
        let recovered = worker
            .install_snapshot(&identity, snapshot(8, 6), 48, StartupCacheState::Warm)
            .expect("covering snapshot recovers publication overflow");
        assert_eq!(sequences(&recovered), vec![1, 2, 3, 4, 5, 6]);

        assert_gap_recovery(&mut worker, &identity, &recovered);
        assert_eq!(worker.metrics().duplicate_live_items, 1);
    })
    .join()
    .expect("worker thread completes");
}

#[test]
fn local_cache_watermark_does_not_floor_provider_generation() {
    let root = TestRoot::create("local-provider-generation");
    let root_path = root.path().to_path_buf();
    let ui_thread = thread::current().id();
    thread::spawn(move || {
        let key = segment_key();
        let identity = identity("local-provider-generation", 1);
        let mut store = HistoryStore::open(&root_path, catalog_key(), 32).expect("store opens");
        publish(&mut store, &identity, &key, b"1,2");
        drop(store);
        let mut worker = HistoryWorker::try_open(root_path, catalog_key(), 32, ui_thread, config())
            .expect("worker constructs");
        let mut decoder = SequenceDecoder::default();
        ready(worker.hydrate_visible(
            hydration_request(
                &identity,
                &key,
                StartupCacheState::Warm,
                ProviderConnectionState::Online,
            ),
            &mut decoder,
        ));

        worker
            .begin_handoff(identity.clone(), &key, 101)
            .expect("handoff starts from local watermark");
        let publication = worker
            .install_snapshot(&identity, snapshot(1, 2), 16, StartupCacheState::Warm)
            .expect("first provider generation replaces local cache");
        assert_eq!(publication.generation, 1);
        assert_eq!(sequences(&publication), vec![1, 2]);
    })
    .join()
    .expect("worker thread completes");
}

#[test]
fn expired_local_cache_does_not_floor_provider_handoff() {
    let root = TestRoot::create("expired-handoff-floor");
    let root_path = root.path().to_path_buf();
    let ui_thread = thread::current().id();
    thread::spawn(move || {
        let key = segment_key();
        let identity = identity("expired-handoff-floor", 1);
        let mut store = HistoryStore::open(&root_path, catalog_key(), 32).expect("store opens");
        let file_name = publish_with_retention(
            &mut store,
            &identity,
            &key,
            b"1,2,3",
            RetentionPolicy::UntilUnixSeconds(102),
        );
        drop(store);
        let mut worker =
            HistoryWorker::try_open(&root_path, catalog_key(), 32, ui_thread, config())
                .expect("worker constructs");
        let mut decoder = SequenceDecoder::default();
        ready(worker.hydrate_visible(
            hydration_request(
                &identity,
                &key,
                StartupCacheState::Warm,
                ProviderConnectionState::Online,
            ),
            &mut decoder,
        ));

        worker
            .begin_handoff(identity.clone(), &key, 102)
            .expect("expired local state is removed before handoff");
        assert!(!root_path.join("segments").join(file_name).exists());
        let publication = worker
            .install_snapshot(
                &identity,
                VerifiedHistorySnapshot::empty_with_watermark(
                    NonZeroU64::new(1).expect("nonzero"),
                    0,
                ),
                0,
                StartupCacheState::Warm,
            )
            .expect("provider handoff is not floored by expired history");
        assert!(publication.values.is_empty());
    })
    .join()
    .expect("worker thread completes");
}

#[test]
fn local_hydration_rotates_unbound_cache_within_byte_bound() {
    let root = TestRoot::create("local-cache-rotation");
    let root_path = root.path().to_path_buf();
    let ui_thread = thread::current().id();
    thread::spawn(move || {
        let key = segment_key();
        let first = identity("rotation-first", 1);
        let second = identity("rotation-second", 1);
        let mut store = HistoryStore::open(&root_path, catalog_key(), 32).expect("store opens");
        publish(&mut store, &first, &key, b"1,2,3");
        publish(&mut store, &second, &key, b"4,5,6");
        let mut bounded_config = config();
        bounded_config.maximum_decoded_bytes = nonzero(8);
        bounded_config.maximum_segment_read_bytes = nonzero(8);
        drop(store);
        let mut worker =
            HistoryWorker::try_open(root_path, catalog_key(), 32, ui_thread, bounded_config)
                .expect("worker constructs");
        let mut decoder = SequenceDecoder::default();

        ready(worker.hydrate_visible(
            hydration_request(
                &first,
                &key,
                StartupCacheState::Warm,
                ProviderConnectionState::Online,
            ),
            &mut decoder,
        ));
        let second_publication = ready(worker.hydrate_visible(
            hydration_request(
                &second,
                &key,
                StartupCacheState::Warm,
                ProviderConnectionState::Online,
            ),
            &mut decoder,
        ));

        assert_eq!(sequences(&second_publication.0), vec![4, 5, 6]);
        assert_eq!(worker.cached_entries().expect("worker owns cache"), 1);
        assert!(
            worker
                .current_publication(&first, &key, 101)
                .expect("worker owns cache")
                .is_none()
        );
    })
    .join()
    .expect("worker thread completes");
}

#[test]
fn chart_rebind_to_cache_miss_releases_old_eviction_pin() {
    let root = TestRoot::create("chart-rebind-miss");
    let root_path = root.path().to_path_buf();
    let ui_thread = thread::current().id();
    thread::spawn(move || {
        let first = identity("chart-first", 1);
        let second = identity("chart-second", 1);
        let mut bounded_config = config();
        bounded_config.maximum_cache_entries = nonzero(1);
        let mut worker =
            HistoryWorker::try_open(root_path, catalog_key(), 32, ui_thread, bounded_config)
                .expect("worker constructs");
        install_direct(&mut worker, &first, 1, 8);
        worker.end_handoff(&first).expect("first handoff retires");
        worker
            .bind_chart(ChartId(1), &first, &segment_key(), 101)
            .expect("first chart binding succeeds")
            .expect("first publication exists");

        assert!(
            worker
                .bind_chart(ChartId(1), &second, &segment_key(), 101)
                .expect("cache miss is explicit")
                .is_none()
        );
        install_direct(&mut worker, &second, 1, 8);
        assert!(
            worker
                .current_publication(&first, &segment_key(), 101)
                .expect("worker owns cache")
                .is_none()
        );
    })
    .join()
    .expect("worker thread completes");
}

fn assert_gap_recovery(
    worker: &mut HistoryWorker<u64>,
    identity: &SegmentIdentity,
    previous: &Arc<axiusflow_local_history::HistoryPublication<u64>>,
) {
    assert!(matches!(
        worker.push_live(identity, item(8), 8),
        Err(LocalHistoryError::Provider(_))
    ));
    let after_gap = worker
        .current_publication(identity, &segment_key(), 101)
        .expect("worker owns cache")
        .expect("last valid publication remains");
    assert!(Arc::ptr_eq(previous, &after_gap));
    assert!(matches!(
        worker.install_snapshot(identity, snapshot(9, 7), 56, StartupCacheState::Warm,),
        Err(LocalHistoryError::Provider(_))
    ));
    let recovered = worker
        .install_snapshot(identity, snapshot(9, 8), 64, StartupCacheState::Warm)
        .expect("covering snapshot recovers the observed gap");
    assert_eq!(sequences(&recovered), (1..=8).collect::<Vec<_>>());
}

#[test]
fn live_buffer_bounds_latch_a_covering_snapshot_requirement() {
    let root = TestRoot::create("buffer-bounds");
    let root_path = root.path().to_path_buf();
    let ui_thread = thread::current().id();
    thread::spawn(move || {
        let mut bounded_config = config();
        bounded_config.maximum_buffered_live = nonzero(2);
        let mut worker =
            HistoryWorker::try_open(root_path, catalog_key(), 32, ui_thread, bounded_config)
                .expect("dedicated worker is accepted");

        assert_oversized_snapshot_rejected(&mut worker);

        let overlap = identity("overlap", 1);
        worker
            .begin_handoff(overlap.clone(), &segment_key(), 101)
            .expect("overlap handoff starts");
        worker
            .push_live(&overlap, item(1), 8)
            .expect("first overlapping item buffers");
        worker
            .push_live(&overlap, item(2), 8)
            .expect("second overlapping item buffers");
        worker
            .install_snapshot(&overlap, snapshot(1, 2), 16, StartupCacheState::Cold)
            .expect("snapshot overlap publishes");
        assert_eq!(
            worker.cached_decoded_bytes().expect("worker owns cache"),
            16
        );

        let byte_bound = identity("byte-bound", 1);
        worker
            .begin_handoff(byte_bound.clone(), &segment_key(), 101)
            .expect("handoff starts");
        assert!(matches!(
            worker.push_live(&byte_bound, item(1), 65),
            Err(LocalHistoryError::DecodedHistoryTooLarge {
                requested: 81,
                maximum: 64,
            })
        ));
        assert!(matches!(
            worker.install_snapshot(
                &byte_bound,
                VerifiedHistorySnapshot::empty_with_watermark(
                    NonZeroU64::new(1).expect("nonzero"),
                    0,
                ),
                0,
                StartupCacheState::Cold,
            ),
            Err(LocalHistoryError::Provider(_))
        ));
        worker
            .install_snapshot(&byte_bound, snapshot(1, 1), 8, StartupCacheState::Cold)
            .expect("snapshot covers byte-bound loss");
        assert!(matches!(
            worker.begin_handoff(byte_bound, &segment_key(), 101),
            Err(LocalHistoryError::HandoffAlreadyStarted)
        ));

        assert_item_bound_recovery(&mut worker);

        let fourth = identity("fourth", 1);
        let fifth = identity("fifth", 1);
        worker
            .begin_handoff(fourth.clone(), &segment_key(), 101)
            .expect("fourth bounded handoff starts");
        assert!(matches!(
            worker.begin_handoff(fifth.clone(), &segment_key(), 101),
            Err(LocalHistoryError::HandoffLimitReached { maximum: 4 })
        ));
        worker
            .end_handoff(&fourth)
            .expect("handoff retirement succeeds");
        worker
            .begin_handoff(fifth, &segment_key(), 101)
            .expect("retirement releases handoff capacity");
    })
    .join()
    .expect("worker thread completes");
}

fn assert_item_bound_recovery(worker: &mut HistoryWorker<u64>) {
    let item_bound = identity("item-bound", 1);
    worker
        .begin_handoff(item_bound.clone(), &segment_key(), 101)
        .expect("second handoff starts");
    worker
        .push_live(&item_bound, item(1), 8)
        .expect("first live item buffers");
    worker
        .push_live(&item_bound, item(2), 8)
        .expect("second live item buffers");
    assert!(matches!(
        worker.push_live(&item_bound, item(3), 8),
        Err(LocalHistoryError::Provider(_))
    ));
    assert!(matches!(
        worker.push_live(&item_bound, item(5), 8),
        Err(LocalHistoryError::Provider(_))
    ));
    assert!(matches!(
        worker.install_snapshot(&item_bound, snapshot(1, 3), 24, StartupCacheState::Cold,),
        Err(LocalHistoryError::Provider(_))
    ));
    let recovered = worker
        .install_snapshot(&item_bound, snapshot(1, 5), 40, StartupCacheState::Cold)
        .expect("snapshot covers every item observed after item-bound loss");
    assert_eq!(sequences(&recovered), vec![1, 2, 3, 4, 5]);
}

fn assert_oversized_snapshot_rejected(worker: &mut HistoryWorker<u64>) {
    let oversized_snapshot = identity("oversized-snapshot", 1);
    worker
        .begin_handoff(oversized_snapshot.clone(), &segment_key(), 101)
        .expect("oversized snapshot handoff starts");
    assert!(matches!(
        worker.install_snapshot(
            &oversized_snapshot,
            snapshot(1, 1),
            65,
            StartupCacheState::Cold,
        ),
        Err(LocalHistoryError::DecodedHistoryTooLarge {
            requested: 65,
            maximum: 64,
        })
    ));
    worker
        .end_handoff(&oversized_snapshot)
        .expect("oversized snapshot handoff retires");
}

#[test]
fn rejected_publication_does_not_partially_evict_cache() {
    let root = TestRoot::create("transactional-eviction");
    let root_path = root.path().to_path_buf();
    let ui_thread = thread::current().id();
    thread::spawn(move || {
        let mut bounded_config = config();
        bounded_config.maximum_decoded_bytes = nonzero(24);
        bounded_config.maximum_segment_read_bytes = nonzero(24);
        let mut worker =
            HistoryWorker::try_open(root_path, catalog_key(), 32, ui_thread, bounded_config)
                .expect("dedicated worker is accepted");
        let first = identity("first", 1);
        let bound = identity("bound", 1);
        let rejected = identity("rejected", 1);
        install_direct(&mut worker, &first, 1, 8);
        install_direct(&mut worker, &bound, 1, 8);
        worker
            .end_handoff(&first)
            .expect("first publication becomes evictable");
        worker
            .bind_chart(ChartId(1), &bound, &segment_key(), 101)
            .expect("chart binding succeeds");
        worker
            .begin_handoff(rejected.clone(), &segment_key(), 101)
            .expect("rejected handoff starts");
        assert!(matches!(
            worker.install_snapshot(&rejected, snapshot(1, 3), 24, StartupCacheState::Cold,),
            Err(LocalHistoryError::CacheFull { .. })
        ));
        assert!(
            worker
                .current_publication(&first, &segment_key(), 101)
                .expect("worker owns cache")
                .is_some()
        );
        assert!(
            worker
                .current_publication(&bound, &segment_key(), 101)
                .expect("worker owns cache")
                .is_some()
        );
        assert_eq!(worker.cached_entries().expect("worker owns cache"), 2);
    })
    .join()
    .expect("worker thread completes");
}

#[test]
fn active_handoff_publication_is_pinned_until_retired() {
    let root = TestRoot::create("active-pin");
    let root_path = root.path().to_path_buf();
    let ui_thread = thread::current().id();
    thread::spawn(move || {
        let mut bounded_config = config();
        bounded_config.maximum_cache_entries = nonzero(2);
        bounded_config.maximum_decoded_bytes = nonzero(200);
        let mut worker =
            HistoryWorker::try_open(root_path, catalog_key(), 32, ui_thread, bounded_config)
                .expect("dedicated worker is accepted");
        let first = identity("active-first", 1);
        let second = identity("active-second", 1);
        let pending = identity("pending", 1);
        worker
            .begin_handoff(first.clone(), &segment_key(), 101)
            .expect("first handoff starts");
        worker
            .install_snapshot(&first, snapshot(2, 1), 60, StartupCacheState::Cold)
            .expect("newer first generation publishes");
        install_direct(&mut worker, &second, 1, 8);
        worker
            .begin_handoff(pending.clone(), &segment_key(), 101)
            .expect("pending handoff starts");
        assert!(matches!(
            worker.install_snapshot(&pending, snapshot(1, 1), 8, StartupCacheState::Cold,),
            Err(LocalHistoryError::CacheFull { .. })
        ));
        worker
            .push_live(&first, item(2), 8)
            .expect("active pinned publication remains usable");
        worker.end_handoff(&first).expect("first handoff retires");
        worker
            .begin_handoff(first.clone(), &segment_key(), 101)
            .expect("handoff restarts from cached watermark");
        assert!(matches!(
            worker.install_snapshot(&first, snapshot(1, 2), 16, StartupCacheState::Warm),
            Err(LocalHistoryError::Provider(_))
        ));
        worker
            .end_handoff(&first)
            .expect("regressed handoff retires");
        worker
            .install_snapshot(&pending, snapshot(2, 1), 8, StartupCacheState::Cold)
            .expect("retired publication becomes evictable");
        assert!(
            worker
                .current_publication(&first, &segment_key(), 101)
                .expect("worker owns cache")
                .is_none()
        );
        assert!(
            worker
                .current_publication(&second, &segment_key(), 101)
                .expect("worker owns cache")
                .is_some()
        );
        assert!(
            worker
                .current_publication(&pending, &segment_key(), 101)
                .expect("worker owns cache")
                .is_some()
        );
    })
    .join()
    .expect("worker thread completes");
}

#[test]
fn ui_thread_cannot_construct_blocking_history_worker() {
    let root = TestRoot::create("ui-thread");
    assert!(matches!(
        HistoryWorker::<u64>::try_open(
            root.path(),
            catalog_key(),
            4,
            thread::current().id(),
            config()
        ),
        Err(LocalHistoryError::UiThreadWorkForbidden)
    ));
    assert!(!root.path().join("catalog.sqlite").exists());

    let root_path = root.path().to_path_buf();
    let ui_thread = thread::current().id();
    thread::spawn(move || {
        let mut invalid = config();
        invalid.maximum_segment_read_bytes = nonzero((64 * 1024 * 1024) + 1);
        invalid.maximum_decoded_bytes = invalid.maximum_segment_read_bytes;
        assert!(matches!(
            HistoryWorker::<u64>::try_open(root_path, catalog_key(), 4, ui_thread, invalid),
            Err(LocalHistoryError::InvalidConfiguration(
                "segment read bound exceeds the storage limit"
            ))
        ));
    })
    .join()
    .expect("configuration thread completes");
    assert!(!root.path().join("catalog.sqlite").exists());
}

#[test]
fn invalid_provider_identity_is_rejected_before_handoff_allocation() {
    let root = TestRoot::create("invalid-provider-identity");
    let root_path = root.path().to_path_buf();
    let ui_thread = thread::current().id();
    thread::spawn(move || {
        let mut worker =
            HistoryWorker::<u64>::try_open(root_path, catalog_key(), 4, ui_thread, config())
                .expect("worker constructs");
        let mut invalid = identity("invalid-provider-identity", 1);
        invalid.schema_revision = 0;
        assert!(matches!(
            worker.begin_handoff(invalid, &segment_key(), 101),
            Err(LocalHistoryError::Storage(
                axiusflow_local_storage::LocalStorageError::InvalidIdentity("schema_revision")
            ))
        ));
        worker
            .begin_handoff(identity("valid-provider-identity", 1), &segment_key(), 101)
            .expect("rejected identity consumes no handoff capacity");
    })
    .join()
    .expect("worker thread completes");
}

#[test]
fn maximum_watermark_overlap_is_not_charged() {
    let root = TestRoot::create("maximum-watermark-overlap");
    let root_path = root.path().to_path_buf();
    let ui_thread = thread::current().id();
    thread::spawn(move || {
        let identity = identity("maximum-watermark-overlap", 1);
        let mut worker =
            HistoryWorker::<u64>::try_open(root_path, catalog_key(), 4, ui_thread, config())
                .expect("worker constructs");
        worker
            .begin_handoff(identity.clone(), &segment_key(), 101)
            .expect("handoff starts");
        worker
            .push_live(&identity, item(u64::MAX), 8)
            .expect("maximum sequence buffers");
        worker
            .check_snapshot_capacity(&identity, u64::MAX, 64)
            .expect("overlapping live bytes are excluded from the capacity check");
        let publication = worker
            .install_snapshot(
                &identity,
                VerifiedHistorySnapshot::empty_with_watermark(
                    NonZeroU64::new(1).expect("nonzero"),
                    u64::MAX,
                ),
                0,
                StartupCacheState::Cold,
            )
            .expect("overlapping maximum sequence is discarded");
        assert!(publication.values.is_empty());
        assert_eq!(worker.cached_decoded_bytes().expect("worker owns cache"), 0);
    })
    .join()
    .expect("worker thread completes");
}

#[test]
fn duplicate_maximum_decoded_sequence_is_rejected() {
    let root = TestRoot::create("duplicate-maximum-sequence");
    let root_path = root.path().to_path_buf();
    let ui_thread = thread::current().id();
    thread::spawn(move || {
        let key = segment_key();
        let identity = identity("duplicate-maximum-sequence", 1);
        let mut store = HistoryStore::open(&root_path, catalog_key(), 4).expect("store opens");
        publish(
            &mut store,
            &identity,
            &key,
            b"18446744073709551615,18446744073709551615",
        );
        drop(store);
        let mut worker = HistoryWorker::try_open(&root_path, catalog_key(), 4, ui_thread, config())
            .expect("worker constructs");
        let mut decoder = SequenceDecoder::default();
        assert!(matches!(
            worker.hydrate_visible(
                hydration_request(
                    &identity,
                    &key,
                    StartupCacheState::Cold,
                    ProviderConnectionState::Offline,
                ),
                &mut decoder,
            ),
            Err(LocalHistoryError::Decode(_))
        ));
    })
    .join()
    .expect("worker thread completes");
}

fn config() -> HistoryWorkerConfig {
    HistoryWorkerConfig {
        maximum_cache_entries: nonzero(4),
        maximum_decoded_bytes: nonzero(64),
        maximum_charts: nonzero(4),
        maximum_segment_read_bytes: nonzero(64),
        maximum_buffered_live: nonzero(4),
        maximum_handoffs: nonzero(4),
    }
}

fn hydration_request<'a>(
    identity: &'a SegmentIdentity,
    encryption_key: &'a SegmentEncryptionKey,
    startup_cache_state: StartupCacheState,
    provider_state: ProviderConnectionState,
) -> HydrationRequest<'a> {
    HydrationRequest {
        identity,
        encryption_key,
        now_unix_seconds: 101,
        startup_cache_state,
        provider_state,
        missing_recovery: RecoveryAction::ProviderRefetch,
    }
}

fn catalog_key() -> CatalogKey {
    CatalogKey::try_new("catalog-key-v1".to_string(), [0x41; 32]).expect("catalog key is valid")
}

fn segment_key() -> SegmentEncryptionKey {
    SegmentEncryptionKey::try_new("segment-key-v1".to_string(), [0x52; 32])
        .expect("segment key is valid")
}

fn identity(instrument: &str, schema_revision: u32) -> SegmentIdentity {
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
        schema_revision,
        calendar_revision: 1,
        adjustment_revision: 1,
        correction_revision: 1,
    }
}

fn publish(
    store: &mut HistoryStore,
    identity: &SegmentIdentity,
    key: &SegmentEncryptionKey,
    payload: &[u8],
) -> String {
    publish_with_retention(store, identity, key, payload, RetentionPolicy::UntilRevoked)
}

fn publish_with_retention(
    store: &mut HistoryStore,
    identity: &SegmentIdentity,
    key: &SegmentEncryptionKey,
    payload: &[u8],
    retention: RetentionPolicy,
) -> String {
    match store
        .publish(PublicationRequest {
            identity,
            payload,
            encryption_key: key,
            retention,
            recovery: RecoveryAction::ProviderRefetch,
            now_unix_seconds: 100,
        })
        .expect("segment publishes")
    {
        PublicationOutcome::Published(receipt) => receipt.file_name,
        PublicationOutcome::MemoryOnly { .. } => panic!("retained fixture became memory-only"),
    }
}

fn corrupt_segment(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .expect("segment becomes writable");
    }
    fs::write(path, b"corrupt").expect("segment corruption writes");
}

fn expand_segment(path: &Path, bytes: usize) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .expect("segment becomes writable");
    }
    fs::write(path, vec![0_u8; bytes]).expect("expanded segment writes");
}

fn assert_provider_fetch(result: Result<HydrationOutcome<u64>, LocalHistoryError>) {
    match result {
        Ok(HydrationOutcome::ProviderFetchRequired { .. }) => {}
        outcome => panic!("expected provider fetch, got {outcome:?}"),
    }
}

fn ready(
    result: Result<HydrationOutcome<u64>, LocalHistoryError>,
) -> (Arc<axiusflow_local_history::HistoryPublication<u64>>, bool) {
    match result.expect("hydration succeeds") {
        HydrationOutcome::Ready {
            publication,
            memory_cache_hit,
        } => (publication, memory_cache_hit),
        outcome => panic!("expected ready history, got {outcome:?}"),
    }
}

fn item(sequence: u64) -> SequencedHistory<u64> {
    SequencedHistory {
        sequence: NonZeroU64::new(sequence).expect("sequence is nonzero"),
        value: sequence,
    }
}

fn install_direct(
    worker: &mut HistoryWorker<u64>,
    identity: &SegmentIdentity,
    last_sequence: u64,
    decoded_bytes: usize,
) {
    worker
        .begin_handoff(identity.clone(), &segment_key(), 101)
        .expect("direct handoff starts");
    worker
        .install_snapshot(
            identity,
            snapshot(1, last_sequence),
            decoded_bytes,
            StartupCacheState::Cold,
        )
        .expect("direct snapshot publishes");
}

fn snapshot(generation: u64, last_sequence: u64) -> VerifiedHistorySnapshot<u64> {
    VerifiedHistorySnapshot::try_new(
        NonZeroU64::new(generation).expect("generation is nonzero"),
        (1..=last_sequence).map(item).collect(),
    )
    .expect("fixture snapshot is contiguous")
}

fn sequences(publication: &axiusflow_local_history::HistoryPublication<u64>) -> Vec<u64> {
    publication
        .values
        .iter()
        .map(|value| value.sequence.get())
        .collect()
}

fn nonzero(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("fixture bound is nonzero")
}
