use axiusflow_desktop_storage::{
    AvailabilityReason, CatalogKey, DataKind, DesktopStorageError, HistoryRead, HistoryScope,
    HistorySeriesIdentity, HistoryStore, Invalidation, KeyRevocationEvidence, PublicationOutcome,
    PublicationRequest, RecoveryAction, RetainedRange, RetentionPolicy, SegmentEncryptionKey,
    SegmentIdentity,
};
use axiusflow_provider_history::{CoverageClass, HistoryRange};
use std::{
    collections::BTreeSet,
    fmt::Write as FmtWrite,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

struct TestRoot(PathBuf);

impl TestRoot {
    fn create() -> Self {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).expect("test randomness is available");
        let mut suffix = String::with_capacity(nonce.len() * 2);
        for byte in nonce {
            write!(&mut suffix, "{byte:02x}").expect("writing to a string succeeds");
        }
        let path = std::env::temp_dir().join(format!(
            "axiusflow_desktop_storage_test_{}_{}",
            std::process::id(),
            suffix
        ));
        fs::create_dir(&path).expect("test root is created");
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

fn catalog_key() -> CatalogKey {
    CatalogKey::try_new("catalog-key-v1".to_string(), [0x41; 32])
        .expect("fixture catalog key is valid")
}

fn segment_key() -> SegmentEncryptionKey {
    SegmentEncryptionKey::try_new("segment-key-v1".to_string(), [0x52; 32])
        .expect("fixture segment key is valid")
}

fn scope(account: &str, entitlement: &str) -> HistoryScope {
    HistoryScope {
        provider_id: "coinbase".to_string(),
        account_id: account.to_string(),
        entitlement_revision: entitlement.to_string(),
    }
}

fn identity(scope: HistoryScope, instrument: &str) -> SegmentIdentity {
    SegmentIdentity {
        scope,
        instrument_id: instrument.to_string(),
        data_kind: DataKind::Bars,
        resolution: "1m".to_string(),
        range_start_unix_nanos: 1_000_000_000,
        range_end_unix_nanos: 61_000_000_000,
        source_revision: 1,
        schema_revision: 1,
        calendar_revision: 1,
        adjustment_revision: 1,
        correction_revision: 1,
    }
}

fn series_identity<'a>(scope: &'a HistoryScope, instrument: &'a str) -> HistorySeriesIdentity<'a> {
    HistorySeriesIdentity {
        scope,
        instrument_id: instrument,
        data_kind: DataKind::Bars,
        resolution: "1m",
        source_revision: 1,
        schema_revision: 1,
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
    retention: RetentionPolicy,
    recovery: RecoveryAction,
) -> PublicationOutcome {
    store
        .publish(PublicationRequest {
            identity,
            payload,
            encryption_key: key,
            retention,
            recovery,
            now_unix_seconds: 100,
        })
        .expect("fixture segment publication succeeds")
}

fn receipt(outcome: PublicationOutcome) -> axiusflow_desktop_storage::SegmentReceipt {
    match outcome {
        PublicationOutcome::Published(receipt) => receipt,
        PublicationOutcome::MemoryOnly { .. } => panic!("fixture expected durable publication"),
    }
}

#[cfg(unix)]
fn make_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .expect("fault fixture permissions change");
}

fn overwrite_for_fault(path: &Path, bytes: &[u8]) {
    make_writable(path);
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .expect("owned segment opens for fault injection");
    file.write_all(bytes).expect("fault injection writes");
    file.sync_all().expect("fault injection syncs");
}

#[test]
fn retained_coverage_merges_exact_series_segments_and_reports_gaps() {
    let root = TestRoot::create();
    let key = segment_key();
    let series_scope = scope("public", "rights-1");
    let mut first = identity(series_scope.clone(), "btc-usd");
    first.range_start_unix_nanos = 10;
    first.range_end_unix_nanos = 30;
    let mut second = first.clone();
    second.range_start_unix_nanos = 25;
    second.range_end_unix_nanos = 40;
    let mut third = first.clone();
    third.range_start_unix_nanos = 50;
    third.range_end_unix_nanos = 60;
    let mut store = HistoryStore::open(root.path(), catalog_key(), 8).expect("store opens");
    for segment in [&first, &second, &third] {
        publish(
            &mut store,
            segment,
            &key,
            b"bars",
            RetentionPolicy::UntilRevoked,
            RecoveryAction::ProviderRefetch,
        );
    }
    let coverage = store
        .retained_coverage(
            HistorySeriesIdentity {
                scope: &series_scope,
                instrument_id: "btc-usd",
                data_kind: DataKind::Bars,
                resolution: "1m",
                source_revision: 1,
                schema_revision: 1,
                calendar_revision: 1,
                adjustment_revision: 1,
                correction_revision: 1,
            },
            101,
        )
        .expect("coverage reads");
    assert_eq!(
        coverage.ranges(),
        &[
            RetainedRange {
                start_unix_nanos: 10,
                end_unix_nanos: 40,
            },
            RetainedRange {
                start_unix_nanos: 50,
                end_unix_nanos: 60,
            },
        ]
    );
    assert_eq!(
        coverage.missing_ranges(RetainedRange {
            start_unix_nanos: 0,
            end_unix_nanos: 70,
        }),
        vec![
            RetainedRange {
                start_unix_nanos: 0,
                end_unix_nanos: 10,
            },
            RetainedRange {
                start_unix_nanos: 40,
                end_unix_nanos: 50,
            },
            RetainedRange {
                start_unix_nanos: 60,
                end_unix_nanos: 70,
            },
        ]
    );
    assert_eq!(
        store
            .retained_identities_in_range(
                series_identity(&series_scope, "btc-usd"),
                RetainedRange {
                    start_unix_nanos: 20,
                    end_unix_nanos: 55,
                },
                101,
            )
            .expect("overlapping identities read")
            .into_iter()
            .map(|identity| (
                identity.range_start_unix_nanos,
                identity.range_end_unix_nanos
            ))
            .collect::<Vec<_>>(),
        vec![(10, 30), (25, 40), (50, 60)]
    );
}

#[test]
fn active_tail_replacement_commits_new_generation_before_retiring_old() {
    let root = TestRoot::create();
    let key = segment_key();
    let tail_scope = scope("public", "rights-1");
    let first = identity(tail_scope.clone(), "btc-usd");
    let mut replacement = first.clone();
    replacement.range_end_unix_nanos += 60_000_000_000;
    let mut store = HistoryStore::open(root.path(), catalog_key(), 32).expect("store opens");
    publish(
        &mut store,
        &first,
        &key,
        b"first",
        RetentionPolicy::UntilRevoked,
        RecoveryAction::ProviderRefetch,
    );
    store
        .replace_active_tail(
            Some(&first),
            PublicationRequest {
                identity: &replacement,
                payload: b"first+second",
                encryption_key: &key,
                retention: RetentionPolicy::UntilRevoked,
                recovery: RecoveryAction::ProviderRefetch,
                now_unix_seconds: 101,
            },
        )
        .expect("replacement commits");
    assert_eq!(
        store
            .read(&first, &key, 102, RecoveryAction::ProviderRefetch)
            .expect("retired identity reads as a miss"),
        HistoryRead::Unavailable {
            reason: AvailabilityReason::NotCached,
            recovery: RecoveryAction::ProviderRefetch,
        }
    );
    assert_eq!(
        store
            .read(&replacement, &key, 102, RecoveryAction::ProviderRefetch)
            .expect("replacement reads"),
        HistoryRead::Hit(b"first+second".to_vec())
    );
    assert_eq!(
        store.statistics().expect("statistics read").active_entries,
        1
    );
}

#[test]
fn quota_evicts_oldest_derived_segments_without_touching_raw_history() {
    let root = TestRoot::create();
    let key = segment_key();
    let quota_scope = scope("public", "rights-1");
    let raw = identity(quota_scope.clone(), "btc-usd");
    let mut first_derived = raw.clone();
    first_derived.data_kind = DataKind::Derived;
    first_derived.resolution = "ema-20-v1".to_string();
    let mut second_derived = first_derived.clone();
    second_derived.range_start_unix_nanos = first_derived.range_end_unix_nanos;
    second_derived.range_end_unix_nanos += 60_000_000_000;
    let mut store = HistoryStore::open(root.path(), catalog_key(), 32).expect("store opens");
    publish(
        &mut store,
        &raw,
        &key,
        b"raw-history",
        RetentionPolicy::UntilRevoked,
        RecoveryAction::ProviderRefetch,
    );
    publish(
        &mut store,
        &first_derived,
        &key,
        b"old-checkpoint",
        RetentionPolicy::UntilRevoked,
        RecoveryAction::ProviderRefetch,
    );
    store
        .publish(PublicationRequest {
            identity: &second_derived,
            payload: b"new-checkpoint",
            encryption_key: &key,
            retention: RetentionPolicy::UntilRevoked,
            recovery: RecoveryAction::ProviderRefetch,
            now_unix_seconds: 101,
        })
        .expect("new checkpoint publishes");
    let report = store
        .enforce_derived_quota(b"new-checkpoint".len() as u64)
        .expect("quota enforcement succeeds");
    assert_eq!(report.entries_removed, 1);
    assert_eq!(
        report.payload_bytes_retained,
        b"new-checkpoint".len() as u64
    );
    assert_eq!(
        store
            .read(&raw, &key, 102, RecoveryAction::ProviderRefetch)
            .expect("raw history remains"),
        HistoryRead::Hit(b"raw-history".to_vec())
    );
    assert_eq!(
        store
            .read(&first_derived, &key, 102, RecoveryAction::ProviderRefetch)
            .expect("old checkpoint is evicted"),
        HistoryRead::Unavailable {
            reason: AvailabilityReason::NotCached,
            recovery: RecoveryAction::ProviderRefetch,
        }
    );
    assert_eq!(
        store
            .read(&second_derived, &key, 102, RecoveryAction::ProviderRefetch)
            .expect("new checkpoint remains"),
        HistoryRead::Hit(b"new-checkpoint".to_vec())
    );
}

#[test]
fn durable_coverage_facts_classify_confirmed_empty_invalidated_and_quarantined_ranges() {
    let root = TestRoot::create();
    let key = segment_key();
    let series_scope = scope("public", "rights-1");
    let series = series_identity(&series_scope, "btc-usd");
    let mut complete = identity(series_scope.clone(), "btc-usd");
    complete.range_start_unix_nanos = 10;
    complete.range_end_unix_nanos = 30;
    let mut corrupt = complete.clone();
    corrupt.range_start_unix_nanos = 50;
    corrupt.range_end_unix_nanos = 60;
    let mut store = HistoryStore::open(root.path(), catalog_key(), 8).expect("store opens");
    publish(
        &mut store,
        &complete,
        &key,
        b"complete",
        RetentionPolicy::UntilRevoked,
        RecoveryAction::ProviderRefetch,
    );
    let corrupt_receipt = receipt(publish(
        &mut store,
        &corrupt,
        &key,
        b"corrupt-me",
        RetentionPolicy::UntilRevoked,
        RecoveryAction::ProviderRefetch,
    ));
    store
        .record_confirmed_empty(
            series,
            RetainedRange {
                start_unix_nanos: 30,
                end_unix_nanos: 40,
            },
            101,
        )
        .expect("confirmed-empty fact persists");
    store
        .record_invalidated_range(
            series,
            RetainedRange {
                start_unix_nanos: 40,
                end_unix_nanos: 50,
            },
            101,
        )
        .expect("invalidation fact persists");
    overwrite_for_fault(
        &root.path().join("segments").join(corrupt_receipt.file_name),
        b"damaged ciphertext",
    );
    assert_eq!(
        store
            .read(&corrupt, &key, 101, RecoveryAction::ProviderRefetch)
            .expect("corruption becomes an explicit miss"),
        HistoryRead::Unavailable {
            reason: AvailabilityReason::Quarantined,
            recovery: RecoveryAction::ProviderRefetch,
        }
    );
    drop(store);

    let reopened = HistoryStore::open(root.path(), catalog_key(), 8).expect("store reopens");
    let plan = reopened
        .series_coverage_snapshot(series, 102)
        .expect("coverage facts restore")
        .plan(HistoryRange {
            start_unix_nanos: 0,
            end_unix_nanos: 70,
        })
        .expect("coverage request validates");
    assert_eq!(plan.classification(), CoverageClass::Partial);
    assert_eq!(
        plan.spans()
            .iter()
            .map(|span| span.class)
            .collect::<Vec<_>>(),
        vec![
            CoverageClass::Missing,
            CoverageClass::Complete,
            CoverageClass::ConfirmedEmpty,
            CoverageClass::Invalidated,
            CoverageClass::Quarantined,
            CoverageClass::Missing,
        ]
    );
    assert_eq!(
        plan.repair_ranges(),
        &[
            HistoryRange {
                start_unix_nanos: 0,
                end_unix_nanos: 10,
            },
            HistoryRange {
                start_unix_nanos: 40,
                end_unix_nanos: 70,
            },
        ]
    );
}

#[test]
fn repaired_ranges_split_invalidations_and_retire_overlapping_quarantine() {
    let root = TestRoot::create();
    let key = segment_key();
    let series_scope = scope("public", "rights-repair");
    let series = series_identity(&series_scope, "btc-usd");
    let mut complete = identity(series_scope.clone(), "btc-usd");
    complete.range_start_unix_nanos = 0;
    complete.range_end_unix_nanos = 20;
    let mut corrupt = complete.clone();
    corrupt.range_start_unix_nanos = 60;
    corrupt.range_end_unix_nanos = 100;
    let mut store = HistoryStore::open(root.path(), catalog_key(), 16).expect("store opens");
    publish(
        &mut store,
        &complete,
        &key,
        b"complete",
        RetentionPolicy::UntilRevoked,
        RecoveryAction::ProviderRefetch,
    );
    let corrupt_receipt = receipt(publish(
        &mut store,
        &corrupt,
        &key,
        b"corrupt",
        RetentionPolicy::UntilRevoked,
        RecoveryAction::ProviderRefetch,
    ));
    store
        .record_invalidated_range(
            series,
            RetainedRange {
                start_unix_nanos: 20,
                end_unix_nanos: 60,
            },
            100,
        )
        .expect("invalidation persists");
    overwrite_for_fault(
        &root.path().join("segments").join(corrupt_receipt.file_name),
        b"damaged",
    );
    let _ = store
        .read(&corrupt, &key, 101, RecoveryAction::ProviderRefetch)
        .expect("corruption becomes quarantine");
    store
        .resolve_repaired_range(
            series,
            RetainedRange {
                start_unix_nanos: 30,
                end_unix_nanos: 50,
            },
            true,
            102,
        )
        .expect("invalidated subrange resolves");
    store
        .resolve_repaired_range(
            series,
            RetainedRange {
                start_unix_nanos: 70,
                end_unix_nanos: 90,
            },
            true,
            102,
        )
        .expect("quarantined subrange resolves");
    assert_repaired_coverage(&store, series);
}

fn assert_repaired_coverage(store: &HistoryStore, series: HistorySeriesIdentity<'_>) {
    let plan = store
        .series_coverage_snapshot(series, 103)
        .expect("resolved coverage reads")
        .plan(HistoryRange {
            start_unix_nanos: 0,
            end_unix_nanos: 110,
        })
        .expect("coverage plans");
    assert_eq!(
        plan.spans()
            .iter()
            .map(|span| (span.range, span.class))
            .collect::<Vec<_>>(),
        vec![
            (
                HistoryRange {
                    start_unix_nanos: 0,
                    end_unix_nanos: 20
                },
                CoverageClass::Complete
            ),
            (
                HistoryRange {
                    start_unix_nanos: 20,
                    end_unix_nanos: 30
                },
                CoverageClass::Invalidated
            ),
            (
                HistoryRange {
                    start_unix_nanos: 30,
                    end_unix_nanos: 50
                },
                CoverageClass::ConfirmedEmpty
            ),
            (
                HistoryRange {
                    start_unix_nanos: 50,
                    end_unix_nanos: 60
                },
                CoverageClass::Invalidated
            ),
            (
                HistoryRange {
                    start_unix_nanos: 60,
                    end_unix_nanos: 70
                },
                CoverageClass::Missing
            ),
            (
                HistoryRange {
                    start_unix_nanos: 70,
                    end_unix_nanos: 90
                },
                CoverageClass::ConfirmedEmpty
            ),
            (
                HistoryRange {
                    start_unix_nanos: 90,
                    end_unix_nanos: 110
                },
                CoverageClass::Missing
            ),
        ]
    );
}

#[test]
fn legacy_catalog_additively_migrates_to_durable_coverage_markers() {
    let root = TestRoot::create();
    drop(HistoryStore::open(root.path(), catalog_key(), 4).expect("store initializes"));
    let catalog_path = root.path().join("catalog.sqlite");
    let connection = rusqlite::Connection::open(&catalog_path).expect("catalog opens directly");
    connection
        .execute_batch(
            "DROP TABLE history_coverage_marker;
             UPDATE catalog_metadata SET schema_version=1 WHERE singleton=1;",
        )
        .expect("legacy catalog fixture installs");
    drop(connection);

    let store = HistoryStore::open(root.path(), catalog_key(), 4).expect("legacy store migrates");
    let series_scope = scope("public", "rights-1");
    store
        .record_confirmed_empty(
            series_identity(&series_scope, "btc-usd"),
            RetainedRange {
                start_unix_nanos: 10,
                end_unix_nanos: 20,
            },
            101,
        )
        .expect("coverage marker writes after migration");
    assert_eq!(
        store
            .statistics()
            .expect("statistics read")
            .coverage_entries,
        1
    );
}

#[cfg(windows)]
fn make_writable(_path: &Path) {}

#[cfg(not(any(unix, windows)))]
fn make_writable(path: &Path) {
    let mut permissions = fs::metadata(path)
        .expect("fault fixture metadata reads")
        .permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).expect("fault fixture permissions change");
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn assert_catalog_redacts(root: &Path, secret: &[u8]) {
    for entry in fs::read_dir(root).expect("store root reads") {
        let entry = entry.expect("store entry reads");
        if entry.file_name() != ".history.lock"
            && entry.file_type().expect("store entry type reads").is_file()
        {
            let bytes = fs::read(entry.path()).expect("catalog file reads");
            assert!(!contains_bytes(&bytes, secret));
        }
    }
}

#[test]
fn publication_encrypts_then_reopens_with_exact_key_provenance() {
    let root = TestRoot::create();
    let key = segment_key();
    let identity = identity(scope("account-a", "rights-1"), "btc-usd");
    let plaintext = b"unique plaintext market history payload";
    let file_name = {
        let mut store = HistoryStore::open(root.path(), catalog_key(), 32).expect("store opens");
        let receipt = receipt(publish(
            &mut store,
            &identity,
            &key,
            plaintext,
            RetentionPolicy::UntilRevoked,
            RecoveryAction::ProviderRefetch,
        ));
        assert_eq!(
            store
                .read(&identity, &key, 101, RecoveryAction::LiveOnly)
                .expect("published segment reads"),
            HistoryRead::Hit(plaintext.to_vec())
        );
        assert_catalog_redacts(root.path(), b"account-a");
        assert_catalog_redacts(root.path(), b"rights-1");
        assert_catalog_redacts(root.path(), plaintext);
        assert_eq!(
            fs::read_dir(root.path().join("staging"))
                .expect("staging directory reads")
                .count(),
            0
        );
        receipt.file_name
    };

    let segment_path = root.path().join("segments").join(file_name);
    let encrypted = fs::read(&segment_path).expect("encrypted segment exists");
    assert!(
        encrypted
            .windows(plaintext.len())
            .all(|window| window != plaintext)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&segment_path)
                .expect("segment metadata reads")
                .permissions()
                .mode()
                & 0o222,
            0
        );
    }
    let mut reopened = HistoryStore::open(root.path(), catalog_key(), 32).expect("store reopens");
    assert_eq!(
        reopened
            .read(&identity, &key, 102, RecoveryAction::LiveOnly)
            .expect("reopened segment reads"),
        HistoryRead::Hit(plaintext.to_vec())
    );
    drop(reopened);

    let wrong_catalog_key = CatalogKey::try_new("catalog-key-v1".to_string(), [0x99; 32])
        .expect("fixture wrong key is structurally valid");
    assert!(matches!(
        HistoryStore::open(root.path(), wrong_catalog_key, 32),
        Err(DesktopStorageError::CatalogKeyMismatch)
    ));
}

#[test]
fn scope_isolation_and_key_mismatch_never_destroy_valid_history() {
    let root = TestRoot::create();
    let key = segment_key();
    let mut store = HistoryStore::open(root.path(), catalog_key(), 32).expect("store opens");
    let owned = identity(scope("account-a", "rights-1"), "btc-usd");
    let foreign = identity(scope("account-b", "rights-1"), "btc-usd");
    publish(
        &mut store,
        &owned,
        &key,
        b"owned",
        RetentionPolicy::UntilRevoked,
        RecoveryAction::ProviderRefetch,
    );
    assert_eq!(
        store
            .read(&foreign, &key, 101, RecoveryAction::LiveOnly)
            .expect("foreign scope is an ordinary miss"),
        HistoryRead::Unavailable {
            reason: AvailabilityReason::NotCached,
            recovery: RecoveryAction::LiveOnly,
        }
    );

    let wrong_bytes = SegmentEncryptionKey::try_new("segment-key-v1".to_string(), [0x33; 32])
        .expect("wrong key fixture is valid");
    assert!(matches!(
        store.read(&owned, &wrong_bytes, 101, RecoveryAction::LiveOnly),
        Err(DesktopStorageError::SegmentKeyMismatch)
    ));
    assert!(matches!(
        store.publish(PublicationRequest {
            identity: &foreign,
            payload: b"foreign",
            encryption_key: &wrong_bytes,
            retention: RetentionPolicy::UntilRevoked,
            recovery: RecoveryAction::LiveOnly,
            now_unix_seconds: 100,
        }),
        Err(DesktopStorageError::SegmentKeyMismatch)
    ));
    assert_eq!(
        store
            .statistics()
            .expect("statistics read")
            .quarantined_entries,
        0
    );
    assert_eq!(
        store
            .read(&owned, &key, 101, RecoveryAction::LiveOnly)
            .expect("valid key still reads"),
        HistoryRead::Hit(b"owned".to_vec())
    );
}

#[test]
fn corruption_quarantines_only_the_affected_segment() {
    let root = TestRoot::create();
    let key = segment_key();
    let mut store = HistoryStore::open(root.path(), catalog_key(), 32).expect("store opens");
    let first = identity(scope("account-a", "rights-1"), "btc-usd");
    let mut second = identity(scope("account-a", "rights-1"), "eth-usd");
    second.range_start_unix_nanos += 1;
    second.range_end_unix_nanos += 1;
    let first_receipt = receipt(publish(
        &mut store,
        &first,
        &key,
        b"first payload",
        RetentionPolicy::UntilRevoked,
        RecoveryAction::ProviderRefetch,
    ));
    publish(
        &mut store,
        &second,
        &key,
        b"second payload",
        RetentionPolicy::UntilRevoked,
        RecoveryAction::LiveOnly,
    );
    let first_path = root.path().join("segments").join(first_receipt.file_name);
    overwrite_for_fault(&first_path, b"corrupt");

    assert_eq!(
        store
            .read(&first, &key, 101, RecoveryAction::LiveOnly)
            .expect("corruption becomes a recovery result"),
        HistoryRead::Unavailable {
            reason: AvailabilityReason::Quarantined,
            recovery: RecoveryAction::ProviderRefetch,
        }
    );
    assert_eq!(
        store
            .read(&second, &key, 101, RecoveryAction::ProviderRefetch)
            .expect("unaffected segment remains readable"),
        HistoryRead::Hit(b"second payload".to_vec())
    );
    let statistics = store.statistics().expect("statistics read");
    assert_eq!(statistics.active_entries, 1);
    assert_eq!(statistics.quarantined_entries, 1);
    assert_eq!(
        fs::read_dir(root.path().join("quarantine"))
            .expect("quarantine directory reads")
            .count(),
        1
    );
    let replacement = receipt(publish(
        &mut store,
        &first,
        &key,
        b"refetched first payload",
        RetentionPolicy::UntilRevoked,
        RecoveryAction::ProviderRefetch,
    ));
    assert_eq!(
        store
            .read(&first, &key, 101, RecoveryAction::LiveOnly)
            .expect("authenticated refetch replaces quarantine"),
        HistoryRead::Hit(b"refetched first payload".to_vec())
    );
    assert_eq!(
        fs::read_dir(root.path().join("quarantine"))
            .expect("quarantine directory reads")
            .count(),
        0
    );
    let replacement_path = root.path().join("segments").join(replacement.file_name);
    overwrite_for_fault(&replacement_path, b"corrupt again");
    assert!(matches!(
        store.read(&first, &key, 101, RecoveryAction::LiveOnly),
        Ok(HistoryRead::Unavailable {
            reason: AvailabilityReason::Quarantined,
            recovery: RecoveryAction::ProviderRefetch,
        })
    ));
    let deletion = store
        .secure_delete_account(
            "coinbase",
            "account-a",
            &Revocations(BTreeSet::from(["segment-key-v1".to_string()])),
        )
        .expect("quarantined account securely deletes");
    assert_eq!(deletion.catalog_entries_removed, 2);
    assert_eq!(deletion.files_removed, 2);
    assert_eq!(
        fs::read_dir(root.path().join("quarantine"))
            .expect("quarantine directory reads")
            .count(),
        0
    );
}

#[test]
fn quarantined_retention_expires_from_reads_and_sweeps() {
    let root = TestRoot::create();
    let key = segment_key();
    let mut store = HistoryStore::open(root.path(), catalog_key(), 32).expect("store opens");
    let first = identity(scope("account-a", "rights-1"), "btc-usd");
    let second = identity(scope("account-a", "rights-1"), "eth-usd");
    let first_receipt = receipt(publish(
        &mut store,
        &first,
        &key,
        b"first",
        RetentionPolicy::UntilUnixSeconds(150),
        RecoveryAction::ProviderRefetch,
    ));
    let second_receipt = receipt(publish(
        &mut store,
        &second,
        &key,
        b"second",
        RetentionPolicy::UntilUnixSeconds(150),
        RecoveryAction::ProviderRefetch,
    ));
    for receipt in [first_receipt, second_receipt] {
        let path = root.path().join("segments").join(receipt.file_name);
        overwrite_for_fault(&path, b"corrupt");
    }
    for identity in [&first, &second] {
        assert!(matches!(
            store.read(identity, &key, 120, RecoveryAction::LiveOnly),
            Ok(HistoryRead::Unavailable {
                reason: AvailabilityReason::Quarantined,
                ..
            })
        ));
    }
    assert_eq!(
        store
            .read(&first, &key, 150, RecoveryAction::LiveOnly)
            .expect("expired quarantine read succeeds"),
        HistoryRead::Unavailable {
            reason: AvailabilityReason::Expired,
            recovery: RecoveryAction::ProviderRefetch,
        }
    );
    assert_eq!(
        store.purge_expired(150).expect("quarantine sweep succeeds"),
        1
    );
    let statistics = store.statistics().expect("statistics read");
    assert_eq!(statistics.active_entries, 0);
    assert_eq!(statistics.quarantined_entries, 0);
    assert_eq!(
        fs::read_dir(root.path().join("quarantine"))
            .expect("quarantine directory reads")
            .count(),
        0
    );
}

#[derive(Clone, Copy)]
enum RevisionDimension {
    Source,
    Schema,
    Calendar,
    Adjustment,
    Correction,
}

#[test]
fn revision_invalidations_remove_only_stale_dimensions() {
    for dimension in [
        RevisionDimension::Source,
        RevisionDimension::Schema,
        RevisionDimension::Calendar,
        RevisionDimension::Adjustment,
        RevisionDimension::Correction,
    ] {
        verify_revision_invalidation(dimension);
    }
}

fn verify_revision_invalidation(dimension: RevisionDimension) {
    let root = TestRoot::create();
    let key = segment_key();
    let mut store = HistoryStore::open(root.path(), catalog_key(), 32).expect("store opens");
    let mut stale = identity(scope("account-a", "rights-1"), "btc-usd");
    let mut current = stale.clone();
    match dimension {
        RevisionDimension::Source => current.source_revision = 2,
        RevisionDimension::Schema => current.schema_revision = 2,
        RevisionDimension::Calendar => current.calendar_revision = 2,
        RevisionDimension::Adjustment => current.adjustment_revision = 2,
        RevisionDimension::Correction => current.correction_revision = 2,
    }
    stale.range_start_unix_nanos += 1;
    stale.range_end_unix_nanos += 1;
    publish(
        &mut store,
        &stale,
        &key,
        b"stale",
        RetentionPolicy::UntilRevoked,
        RecoveryAction::ProviderRefetch,
    );
    publish(
        &mut store,
        &current,
        &key,
        b"current",
        RetentionPolicy::UntilRevoked,
        RecoveryAction::ProviderRefetch,
    );
    let invalidation = match dimension {
        RevisionDimension::Source => Invalidation::Source {
            scope: current.scope.clone(),
            instrument_id: current.instrument_id.clone(),
            current_revision: 2,
        },
        RevisionDimension::Schema => Invalidation::Schema {
            scope: current.scope.clone(),
            current_revision: 2,
        },
        RevisionDimension::Calendar => Invalidation::Calendar {
            scope: current.scope.clone(),
            instrument_id: current.instrument_id.clone(),
            current_revision: 2,
        },
        RevisionDimension::Adjustment => Invalidation::Adjustment {
            scope: current.scope.clone(),
            instrument_id: current.instrument_id.clone(),
            current_revision: 2,
        },
        RevisionDimension::Correction => Invalidation::Correction {
            scope: current.scope.clone(),
            instrument_id: current.instrument_id.clone(),
            current_revision: 2,
        },
    };
    assert_eq!(
        store
            .invalidate(&invalidation)
            .expect("invalidation succeeds"),
        1
    );
    assert_eq!(
        store
            .read(&stale, &key, 101, RecoveryAction::LiveOnly)
            .expect("stale identity is absent"),
        HistoryRead::Unavailable {
            reason: AvailabilityReason::NotCached,
            recovery: RecoveryAction::LiveOnly,
        }
    );
    assert_eq!(
        store
            .read(&current, &key, 101, RecoveryAction::LiveOnly)
            .expect("current identity remains"),
        HistoryRead::Hit(b"current".to_vec())
    );
}

#[test]
fn entitlement_and_account_invalidations_preserve_other_accounts() {
    let root = TestRoot::create();
    let key = segment_key();
    let mut store = HistoryStore::open(root.path(), catalog_key(), 32).expect("store opens");
    let entitlement_one = identity(scope("account-a", "rights-1"), "btc-usd");
    let entitlement_two = identity(scope("account-a", "rights-2"), "btc-usd");
    let other_account = identity(scope("account-b", "rights-1"), "btc-usd");
    for (identity, payload) in [
        (&entitlement_one, b"one".as_slice()),
        (&entitlement_two, b"two".as_slice()),
        (&other_account, b"other".as_slice()),
    ] {
        publish(
            &mut store,
            identity,
            &key,
            payload,
            RetentionPolicy::UntilRevoked,
            RecoveryAction::ProviderRefetch,
        );
    }
    assert_eq!(
        store
            .invalidate(&Invalidation::Entitlement {
                scope: entitlement_one.scope.clone(),
            })
            .expect("entitlement invalidation succeeds"),
        1
    );
    assert_eq!(
        store
            .invalidate(&Invalidation::Account {
                provider_id: "coinbase".to_string(),
                account_id: "account-a".to_string(),
            })
            .expect("account invalidation succeeds"),
        1
    );
    assert_eq!(
        store
            .read(&other_account, &key, 101, RecoveryAction::LiveOnly)
            .expect("other account remains"),
        HistoryRead::Hit(b"other".to_vec())
    );
}

struct Revocations(BTreeSet<String>);

impl KeyRevocationEvidence for Revocations {
    fn confirms_revocation(&self, key_id: &str) -> bool {
        self.0.contains(key_id)
    }
}

#[test]
fn secure_deletion_requires_key_revocation_before_removing_scope() {
    let root = TestRoot::create();
    let key = segment_key();
    let identity = identity(scope("account-a", "rights-1"), "btc-usd");
    let mut store = HistoryStore::open(root.path(), catalog_key(), 32).expect("store opens");
    publish(
        &mut store,
        &identity,
        &key,
        b"sensitive history",
        RetentionPolicy::UntilRevoked,
        RecoveryAction::LiveOnly,
    );
    assert!(matches!(
        store.secure_delete_account("coinbase", "account-a", &Revocations(BTreeSet::new())),
        Err(DesktopStorageError::KeyRevocationMissing { .. })
    ));
    assert_eq!(
        store.statistics().expect("statistics read").active_entries,
        1
    );

    let report = store
        .secure_delete_account(
            "coinbase",
            "account-a",
            &Revocations(BTreeSet::from(["segment-key-v1".to_string()])),
        )
        .expect("revoked scope deletes");
    assert_eq!(report.catalog_entries_removed, 1);
    assert_eq!(report.files_removed, 1);
    assert!(report.physical_remanence_possible);
    assert_eq!(
        store.statistics().expect("statistics read").active_entries,
        0
    );
}

#[test]
fn secure_deletion_rejects_keys_shared_with_other_accounts() {
    let root = TestRoot::create();
    let key = segment_key();
    let same_material_different_id =
        SegmentEncryptionKey::try_new("segment-key-alias".to_string(), [0x52; 32])
            .expect("fixture alias key is valid");
    let first = identity(scope("account-a", "rights-1"), "btc-usd");
    let second = identity(scope("account-b", "rights-1"), "btc-usd");
    let mut store = HistoryStore::open(root.path(), catalog_key(), 32).expect("store opens");
    for (identity, encryption_key, payload) in [
        (&first, &key, b"first".as_slice()),
        (&second, &same_material_different_id, b"second".as_slice()),
    ] {
        publish(
            &mut store,
            identity,
            encryption_key,
            payload,
            RetentionPolicy::UntilRevoked,
            RecoveryAction::LiveOnly,
        );
    }
    assert!(matches!(
        store.secure_delete_account(
            "coinbase",
            "account-a",
            &Revocations(BTreeSet::from(["segment-key-v1".to_string()])),
        ),
        Err(DesktopStorageError::SharedKeyStillReferenced { .. })
    ));
    assert_eq!(
        store.statistics().expect("statistics read").active_entries,
        2
    );
    assert_eq!(
        store
            .read(
                &second,
                &same_material_different_id,
                101,
                RecoveryAction::LiveOnly,
            )
            .expect("other account key remains usable"),
        HistoryRead::Hit(b"second".to_vec())
    );
    drop(store);
    assert!(matches!(
        HistoryStore::open(root.path(), catalog_key(), 1),
        Err(DesktopStorageError::InvalidConfiguration(
            "existing catalog exceeds the configured entry bound"
        ))
    ));
}

#[test]
fn retention_catalog_bounds_and_recovery_are_explicit() {
    let root = TestRoot::create();
    let key = segment_key();
    let first = identity(scope("account-a", "rights-1"), "btc-usd");
    let second = identity(scope("account-a", "rights-1"), "eth-usd");
    let mut store = HistoryStore::open(root.path(), catalog_key(), 1).expect("store opens");
    assert_eq!(
        publish(
            &mut store,
            &first,
            &key,
            b"ephemeral",
            RetentionPolicy::MemoryOnly,
            RecoveryAction::LiveOnly,
        ),
        PublicationOutcome::MemoryOnly {
            recovery: RecoveryAction::LiveOnly,
        }
    );
    publish(
        &mut store,
        &first,
        &key,
        b"expiring",
        RetentionPolicy::UntilUnixSeconds(200),
        RecoveryAction::ProviderRefetch,
    );
    assert_eq!(
        publish(
            &mut store,
            &first,
            &key,
            b"rights-tightened",
            RetentionPolicy::MemoryOnly,
            RecoveryAction::LiveOnly,
        ),
        PublicationOutcome::MemoryOnly {
            recovery: RecoveryAction::LiveOnly,
        }
    );
    assert_eq!(
        store
            .read(&first, &key, 101, RecoveryAction::LiveOnly)
            .expect("tightened rights remove retained bytes"),
        HistoryRead::Unavailable {
            reason: AvailabilityReason::NotCached,
            recovery: RecoveryAction::LiveOnly,
        }
    );
    publish(
        &mut store,
        &first,
        &key,
        b"expiring",
        RetentionPolicy::UntilUnixSeconds(200),
        RecoveryAction::ProviderRefetch,
    );
    assert!(matches!(
        store.publish(PublicationRequest {
            identity: &second,
            payload: b"bounded",
            encryption_key: &key,
            retention: RetentionPolicy::UntilRevoked,
            recovery: RecoveryAction::LiveOnly,
            now_unix_seconds: 100,
        }),
        Err(DesktopStorageError::CatalogFull { maximum: 1 })
    ));
    assert_eq!(
        store
            .read(&first, &key, 200, RecoveryAction::LiveOnly)
            .expect("expired read is explicit"),
        HistoryRead::Unavailable {
            reason: AvailabilityReason::Expired,
            recovery: RecoveryAction::ProviderRefetch,
        }
    );
    assert_eq!(
        store.statistics().expect("statistics read").active_entries,
        0
    );
    publish(
        &mut store,
        &second,
        &key,
        b"purge-expired",
        RetentionPolicy::UntilUnixSeconds(150),
        RecoveryAction::LiveOnly,
    );
    assert_eq!(store.purge_expired(150).expect("expiry purge succeeds"), 1);
    assert_eq!(
        store.statistics().expect("statistics read").active_entries,
        0
    );
}

#[test]
fn startup_removes_orphans_and_quarantines_missing_manifest_files() {
    let root = TestRoot::create();
    let key = segment_key();
    let identity = identity(scope("account-a", "rights-1"), "btc-usd");
    let file_name = {
        let mut store = HistoryStore::open(root.path(), catalog_key(), 32).expect("store opens");
        assert!(matches!(
            HistoryStore::open(root.path(), catalog_key(), 32),
            Err(DesktopStorageError::StoreAlreadyOpen)
        ));
        receipt(publish(
            &mut store,
            &identity,
            &key,
            b"payload",
            RetentionPolicy::UntilRevoked,
            RecoveryAction::ProviderRefetch,
        ))
        .file_name
    };
    fs::write(
        root.path().join("staging").join("abandoned.tmp"),
        b"partial",
    )
    .expect("staging fault fixture is created");
    fs::write(
        root.path()
            .join("segments")
            .join(format!("{}.seg", "a".repeat(64))),
        b"orphan",
    )
    .expect("orphan fixture is created");
    fs::remove_file(root.path().join("segments").join(file_name))
        .expect("manifest file is removed for fault injection");

    let mut reopened = HistoryStore::open(root.path(), catalog_key(), 32).expect("store recovers");
    let statistics = reopened.statistics().expect("statistics read");
    assert_eq!(statistics.active_entries, 0);
    assert_eq!(statistics.quarantined_entries, 1);
    assert_eq!(
        fs::read_dir(root.path().join("staging"))
            .expect("staging directory reads")
            .count(),
        0
    );
    assert_eq!(
        fs::read_dir(root.path().join("segments"))
            .expect("segments directory reads")
            .count(),
        0
    );
    assert_eq!(
        reopened
            .read(&identity, &key, 101, RecoveryAction::LiveOnly)
            .expect("quarantined manifest is unavailable"),
        HistoryRead::Unavailable {
            reason: AvailabilityReason::Quarantined,
            recovery: RecoveryAction::ProviderRefetch,
        }
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let linked_root = TestRoot::create();
        let external = TestRoot::create();
        let protected = external.path().join("must_remain");
        fs::write(&protected, b"outside").expect("external fixture writes");
        symlink(external.path(), linked_root.path().join("segments"))
            .expect("symlink fixture creates");
        assert!(matches!(
            HistoryStore::open(linked_root.path(), catalog_key(), 32),
            Err(DesktopStorageError::InvalidConfiguration(
                "owned storage path is not a real directory"
            ))
        ));
        assert_eq!(
            fs::read(&protected).expect("external file remains readable"),
            b"outside"
        );
    }
}
