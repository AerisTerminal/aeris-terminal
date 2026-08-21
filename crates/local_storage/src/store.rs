use crate::{
    LocalStorageError,
    catalog::{
        CONFIRMED_EMPTY_CLASS, Catalog, CatalogFilter, CatalogRecord, INVALIDATED_CLASS,
        NewCatalogRecord, SeriesDimensions, SeriesTokens,
    },
    crypto::{
        SEGMENT_FILE_OVERHEAD_BYTES, catalog_key_verifier, checksum, decrypt_segment,
        encode_identity, encrypt_segment, hex, identity_token, instrument_token, resolution_token,
        scope_tokens, segment_key_verifier,
    },
    model::{
        AvailabilityReason, CatalogStatistics, DeletionReport, HistoryRead, HistoryScope,
        Invalidation, KeyRevocationEvidence, PublicationOutcome, PublicationRequest,
        RecoveryAction, RetentionPolicy, SegmentEncryptionKey, SegmentReceipt, validate_identifier,
    },
};
use axiusflow_provider_history::{CoverageSnapshot, HistoryRange};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

pub use crate::crypto::CatalogKey;

/// Hard upper bound for one encrypted history payload.
pub const MAXIMUM_SEGMENT_BYTES: usize = 64 * 1024 * 1024;
/// Hard upper bound for a configured desktop catalog.
pub const MAXIMUM_CATALOG_ENTRIES: usize = 100_000;

const CATALOG_FILE: &str = "catalog.sqlite";
const ROOT_LOCK_FILE: &str = ".history.lock";
const SEGMENTS_DIRECTORY: &str = "segments";
const STAGING_DIRECTORY: &str = "staging";
const QUARANTINE_DIRECTORY: &str = "quarantine";

/// Blocking desktop-local catalog and immutable-segment store.
pub struct HistoryStore {
    _root_lock: File,
    root: PathBuf,
    segments: PathBuf,
    staging: PathBuf,
    quarantine: PathBuf,
    catalog_key: CatalogKey,
    catalog: Catalog,
}

enum ExistingPublication {
    Absent,
    Idempotent(SegmentReceipt),
    ReplaceQuarantined(CatalogRecord),
}

impl HistoryStore {
    /// Opens or creates one private per-user history root and recovers orphaned files.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid bounds, key/schema drift, unsafe filesystem
    /// state, recovery failure, or `SQLite` initialization failure.
    pub fn open(
        root: impl AsRef<Path>,
        catalog_key: CatalogKey,
        maximum_catalog_entries: usize,
    ) -> Result<Self, LocalStorageError> {
        if maximum_catalog_entries == 0 || maximum_catalog_entries > MAXIMUM_CATALOG_ENTRIES {
            return Err(LocalStorageError::InvalidConfiguration(
                "catalog entry bound must be within 1..=100000",
            ));
        }
        let root = root.as_ref().to_path_buf();
        create_private_directory(&root)?;
        let root_lock = open_root_lock(&root.join(ROOT_LOCK_FILE))?;
        let segments = root.join(SEGMENTS_DIRECTORY);
        let staging = root.join(STAGING_DIRECTORY);
        let quarantine = root.join(QUARANTINE_DIRECTORY);
        for directory in [&segments, &staging, &quarantine] {
            create_private_directory(directory)?;
        }
        let key_verifier = catalog_key_verifier(&catalog_key)?;
        let catalog = Catalog::open(
            &root.join(CATALOG_FILE),
            catalog_key.key_id(),
            &key_verifier,
            maximum_catalog_entries,
        )?;
        let mut store = Self {
            _root_lock: root_lock,
            root,
            segments,
            staging,
            quarantine,
            catalog_key,
            catalog,
        };
        store.recover_filesystem()?;
        Ok(store)
    }

    /// Encrypts, syncs, links, and finally publishes one immutable segment.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identity, size/bound violations, duplicate
    /// identity, cryptographic failure, I/O failure, or catalog failure.
    pub fn publish(
        &mut self,
        request: PublicationRequest<'_>,
    ) -> Result<PublicationOutcome, LocalStorageError> {
        request.identity.validate()?;
        let retention_until = match request.retention {
            RetentionPolicy::MemoryOnly => {
                self.remove_retained_identity(request.identity)?;
                return Ok(PublicationOutcome::MemoryOnly {
                    recovery: request.recovery,
                });
            }
            RetentionPolicy::UntilUnixSeconds(expiry) if expiry <= request.now_unix_seconds => {
                self.remove_retained_identity(request.identity)?;
                return Ok(PublicationOutcome::MemoryOnly {
                    recovery: request.recovery,
                });
            }
            RetentionPolicy::UntilUnixSeconds(expiry) => Some(expiry),
            RetentionPolicy::UntilRevoked => None,
        };
        if request.payload.len() > MAXIMUM_SEGMENT_BYTES {
            return Err(LocalStorageError::SegmentTooLarge {
                requested: request.payload.len(),
                maximum: MAXIMUM_SEGMENT_BYTES,
            });
        }
        self.publish_retained(&request, retention_until)
    }

    /// Returns the additional physical bytes needed by a retained publication.
    /// An exact active idempotent publication requires no reservation.
    ///
    /// # Errors
    /// Returns the same validation, key-provenance, immutable-conflict, and
    /// segment-size errors that publication can establish before file I/O.
    pub fn publication_storage_requirement(
        &self,
        request: PublicationRequest<'_>,
    ) -> Result<u64, LocalStorageError> {
        request.identity.validate()?;
        let retention_until = match request.retention {
            RetentionPolicy::MemoryOnly => return Ok(0),
            RetentionPolicy::UntilUnixSeconds(expiry) if expiry <= request.now_unix_seconds => {
                return Ok(0);
            }
            RetentionPolicy::UntilUnixSeconds(expiry) => Some(expiry),
            RetentionPolicy::UntilRevoked => None,
        };
        if request.payload.len() > MAXIMUM_SEGMENT_BYTES {
            return Err(LocalStorageError::SegmentTooLarge {
                requested: request.payload.len(),
                maximum: MAXIMUM_SEGMENT_BYTES,
            });
        }
        let request_key_verifier = segment_key_verifier(request.encryption_key)?;
        if self
            .catalog
            .key_id_has_different_verifier(request.encryption_key.key_id(), &request_key_verifier)?
        {
            return Err(LocalStorageError::SegmentKeyMismatch);
        }
        let segment_id = identity_token(&self.catalog_key, request.identity)?;
        if let Some(record) = self.catalog.find(&segment_id)?
            && record
                .retention_until
                .is_none_or(|expiry| expiry > request.now_unix_seconds)
        {
            let key_matches = record.key_id == request.encryption_key.key_id()
                && record.key_verifier == request_key_verifier;
            let idempotent = !record.quarantined
                && key_matches
                && record.payload_bytes == u64::try_from(request.payload.len()).unwrap_or(u64::MAX)
                && record.payload_checksum == checksum(request.payload)
                && record.retention_until == retention_until
                && record.recovery == request.recovery;
            if idempotent {
                return Ok(0);
            }
            if !record.quarantined
                || record.recovery != RecoveryAction::ProviderRefetch
                || request.recovery != RecoveryAction::ProviderRefetch
                || !key_matches
            {
                return Err(LocalStorageError::SegmentAlreadyExists);
            }
        }
        u64::try_from(
            request
                .payload
                .len()
                .saturating_add(SEGMENT_FILE_OVERHEAD_BYTES),
        )
        .map_err(|_| LocalStorageError::SegmentTooLarge {
            requested: request.payload.len(),
            maximum: MAXIMUM_SEGMENT_BYTES,
        })
    }

    /// Replaces a bounded copy-on-write active tail after the new encrypted
    /// generation is durably published. A crash can retain overlap, never lose
    /// the previously durable prefix.
    ///
    /// # Errors
    /// Returns an error for invalid identities, rights, storage, or catalog failure.
    pub fn replace_active_tail(
        &mut self,
        previous: Option<&crate::SegmentIdentity>,
        request: PublicationRequest<'_>,
    ) -> Result<PublicationOutcome, LocalStorageError> {
        let outcome = self.publish(request)?;
        if matches!(outcome, PublicationOutcome::Published(_))
            && let Some(previous) = previous
            && previous != request.identity
        {
            self.remove_retained_identity(previous)?;
        }
        Ok(outcome)
    }

    /// Reads and authenticates one exact scope-bound segment.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identity, wrong key, or a catalog/I/O
    /// failure. Corrupt or expired files are removed from the usable cache and
    /// return an explicit recovery action instead of stale bytes.
    pub fn read(
        &mut self,
        identity: &crate::SegmentIdentity,
        encryption_key: &SegmentEncryptionKey,
        now_unix_seconds: i64,
        missing_recovery: RecoveryAction,
    ) -> Result<HistoryRead, LocalStorageError> {
        self.read_bounded(
            identity,
            encryption_key,
            now_unix_seconds,
            missing_recovery,
            MAXIMUM_SEGMENT_BYTES,
        )
    }

    /// Returns merged active retained coverage for one exact series revision.
    ///
    /// Expired, quarantined, revoked, or dimension-mismatched segments are excluded.
    ///
    /// # Errors
    /// Returns an error for invalid dimensions or catalog access failure.
    pub fn retained_coverage(
        &self,
        series: crate::HistorySeriesIdentity<'_>,
        now_unix_seconds: i64,
    ) -> Result<crate::RetainedSeriesCoverage, LocalStorageError> {
        series.validate()?;
        let scope = scope_tokens(&self.catalog_key, series.scope)?;
        let instrument = instrument_token(&self.catalog_key, series.instrument_id)?;
        let resolution = resolution_token(&self.catalog_key, series.resolution)?;
        let ranges = self.catalog.active_series_ranges(
            SeriesTokens {
                provider: &scope.provider,
                account: &scope.account,
                entitlement: &scope.entitlement,
                instrument: &instrument,
                resolution: &resolution,
            },
            SeriesDimensions {
                data_kind: series.data_kind.code(),
                source_revision: series.source_revision,
                schema_revision: series.schema_revision,
                calendar_revision: series.calendar_revision,
                adjustment_revision: series.adjustment_revision,
                correction_revision: series.correction_revision,
            },
            now_unix_seconds,
        )?;
        Ok(crate::RetainedSeriesCoverage::from_ranges(
            ranges
                .into_iter()
                .map(|(start_unix_nanos, end_unix_nanos)| crate::RetainedRange {
                    start_unix_nanos,
                    end_unix_nanos,
                })
                .collect(),
        ))
    }

    /// Returns active immutable segment identities that overlap one requested range.
    /// Results retain segment boundaries and are sorted from oldest to newest.
    ///
    /// # Errors
    /// Returns an error for invalid dimensions, range, or catalog access failure.
    pub fn retained_identities_in_range(
        &self,
        series: crate::HistorySeriesIdentity<'_>,
        requested: crate::RetainedRange,
        now_unix_seconds: i64,
    ) -> Result<Vec<crate::SegmentIdentity>, LocalStorageError> {
        series.validate()?;
        if requested.start_unix_nanos >= requested.end_unix_nanos {
            return Err(LocalStorageError::InvalidIdentity("coverage_range"));
        }
        let scope = scope_tokens(&self.catalog_key, series.scope)?;
        let instrument = instrument_token(&self.catalog_key, series.instrument_id)?;
        let resolution = resolution_token(&self.catalog_key, series.resolution)?;
        let ranges = self.catalog.active_series_ranges(
            SeriesTokens {
                provider: &scope.provider,
                account: &scope.account,
                entitlement: &scope.entitlement,
                instrument: &instrument,
                resolution: &resolution,
            },
            series_dimensions(series),
            now_unix_seconds,
        )?;
        Ok(ranges
            .into_iter()
            .filter(|(start, end)| {
                *start < requested.end_unix_nanos && requested.start_unix_nanos < *end
            })
            .map(
                |(range_start_unix_nanos, range_end_unix_nanos)| crate::SegmentIdentity {
                    scope: series.scope.clone(),
                    instrument_id: series.instrument_id.to_string(),
                    data_kind: series.data_kind,
                    resolution: series.resolution.to_string(),
                    range_start_unix_nanos,
                    range_end_unix_nanos,
                    source_revision: series.source_revision,
                    schema_revision: series.schema_revision,
                    calendar_revision: series.calendar_revision,
                    adjustment_revision: series.adjustment_revision,
                    correction_revision: series.correction_revision,
                },
            )
            .collect())
    }

    /// Returns complete, confirmed-empty, invalidated, and quarantined facts for
    /// one exact series revision. Missing and partial coverage are derived by the
    /// provider-neutral coverage planner for the caller's requested range.
    ///
    /// # Errors
    /// Returns an error for invalid dimensions, catalog access, or corrupt ranges.
    pub fn series_coverage_snapshot(
        &self,
        series: crate::HistorySeriesIdentity<'_>,
        now_unix_seconds: i64,
    ) -> Result<CoverageSnapshot, LocalStorageError> {
        series.validate()?;
        let scope = scope_tokens(&self.catalog_key, series.scope)?;
        let instrument = instrument_token(&self.catalog_key, series.instrument_id)?;
        let resolution = resolution_token(&self.catalog_key, series.resolution)?;
        let tokens = SeriesTokens {
            provider: &scope.provider,
            account: &scope.account,
            entitlement: &scope.entitlement,
            instrument: &instrument,
            resolution: &resolution,
        };
        let dimensions = series_dimensions(series);
        CoverageSnapshot::try_new(
            history_ranges(self.catalog.active_series_ranges(
                tokens,
                dimensions,
                now_unix_seconds,
            )?),
            history_ranges(self.catalog.coverage_marker_ranges(
                tokens,
                dimensions,
                CONFIRMED_EMPTY_CLASS,
            )?),
            history_ranges(self.catalog.coverage_marker_ranges(
                tokens,
                dimensions,
                INVALIDATED_CLASS,
            )?),
            history_ranges(self.catalog.quarantined_series_ranges(
                tokens,
                dimensions,
                now_unix_seconds,
            )?),
        )
        .map_err(|_| LocalStorageError::InvalidConfiguration("catalog coverage range is invalid"))
    }

    /// Records provider-proven empty coverage for one exact series revision.
    ///
    /// # Errors
    /// Returns an error for invalid dimensions, range, or catalog capacity.
    pub fn record_confirmed_empty(
        &self,
        series: crate::HistorySeriesIdentity<'_>,
        range: crate::RetainedRange,
        now_unix_seconds: i64,
    ) -> Result<(), LocalStorageError> {
        self.record_coverage_marker(series, range, CONFIRMED_EMPTY_CLASS, now_unix_seconds)
    }

    /// Records a range that must be repaired before it can be treated as usable.
    ///
    /// # Errors
    /// Returns an error for invalid dimensions, range, or catalog capacity.
    pub fn record_invalidated_range(
        &self,
        series: crate::HistorySeriesIdentity<'_>,
        range: crate::RetainedRange,
        now_unix_seconds: i64,
    ) -> Result<(), LocalStorageError> {
        self.record_coverage_marker(series, range, INVALIDATED_CLASS, now_unix_seconds)
    }

    /// Supersedes unusable coverage evidence after a provider repair completes.
    ///
    /// Confirmed-empty and invalidated markers are split around the repaired
    /// range. Overlapping quarantined segments are removed entirely, leaving
    /// any unrepaired portion missing and therefore eligible for a later fetch.
    ///
    /// # Errors
    /// Returns an error for invalid dimensions, catalog capacity, or cleanup failure.
    pub fn resolve_repaired_range(
        &mut self,
        series: crate::HistorySeriesIdentity<'_>,
        range: crate::RetainedRange,
        confirmed_empty: bool,
        now_unix_seconds: i64,
    ) -> Result<(), LocalStorageError> {
        series.validate()?;
        if range.start_unix_nanos >= range.end_unix_nanos {
            return Err(LocalStorageError::InvalidIdentity("coverage_range"));
        }
        let scope = scope_tokens(&self.catalog_key, series.scope)?;
        let instrument = instrument_token(&self.catalog_key, series.instrument_id)?;
        let resolution = resolution_token(&self.catalog_key, series.resolution)?;
        let tokens = SeriesTokens {
            provider: &scope.provider,
            account: &scope.account,
            entitlement: &scope.entitlement,
            instrument: &instrument,
            resolution: &resolution,
        };
        let dimensions = series_dimensions(series);
        let repaired = (range.start_unix_nanos, range.end_unix_nanos);
        let quarantined = self
            .catalog
            .quarantined_series_records_overlapping(tokens, dimensions, repaired)?;
        self.remove_records(&quarantined)?;
        self.catalog.replace_overlapping_coverage_markers(
            tokens,
            dimensions,
            repaired,
            confirmed_empty.then_some(CONFIRMED_EMPTY_CLASS),
            now_unix_seconds,
        )
    }

    fn record_coverage_marker(
        &self,
        series: crate::HistorySeriesIdentity<'_>,
        range: crate::RetainedRange,
        class: i64,
        now_unix_seconds: i64,
    ) -> Result<(), LocalStorageError> {
        series.validate()?;
        if range.start_unix_nanos >= range.end_unix_nanos {
            return Err(LocalStorageError::InvalidIdentity("coverage_range"));
        }
        let scope = scope_tokens(&self.catalog_key, series.scope)?;
        let instrument = instrument_token(&self.catalog_key, series.instrument_id)?;
        let resolution = resolution_token(&self.catalog_key, series.resolution)?;
        self.catalog.insert_coverage_marker(
            SeriesTokens {
                provider: &scope.provider,
                account: &scope.account,
                entitlement: &scope.entitlement,
                instrument: &instrument,
                resolution: &resolution,
            },
            series_dimensions(series),
            (range.start_unix_nanos, range.end_unix_nanos),
            class,
            now_unix_seconds,
        )
    }

    /// Reads one exact segment only when its cataloged payload fits the caller's bound.
    ///
    /// The bound is checked from cataloged payload metadata before opening,
    /// reading, allocating, or decrypting the segment file.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid bound, invalid identity, wrong key,
    /// catalog/I/O failure, or a cataloged payload above the requested bound.
    pub fn read_bounded(
        &mut self,
        identity: &crate::SegmentIdentity,
        encryption_key: &SegmentEncryptionKey,
        now_unix_seconds: i64,
        missing_recovery: RecoveryAction,
        maximum_payload_bytes: usize,
    ) -> Result<HistoryRead, LocalStorageError> {
        Ok(
            match self.read_bounded_authorized(
                identity,
                encryption_key,
                now_unix_seconds,
                missing_recovery,
                maximum_payload_bytes,
            )? {
                crate::AuthorizedHistoryRead::Hit { payload, .. } => HistoryRead::Hit(payload),
                crate::AuthorizedHistoryRead::Unavailable { reason, recovery } => {
                    HistoryRead::Unavailable { reason, recovery }
                }
            },
        )
    }

    /// Reads one bounded segment and returns the policy required for safe cache reuse.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::read_bounded`].
    pub fn read_bounded_authorized(
        &mut self,
        identity: &crate::SegmentIdentity,
        encryption_key: &SegmentEncryptionKey,
        now_unix_seconds: i64,
        missing_recovery: RecoveryAction,
        maximum_payload_bytes: usize,
    ) -> Result<crate::AuthorizedHistoryRead, LocalStorageError> {
        if maximum_payload_bytes == 0 || maximum_payload_bytes > MAXIMUM_SEGMENT_BYTES {
            return Err(LocalStorageError::InvalidConfiguration(
                "segment read bound must be within 1..=67108864",
            ));
        }
        identity.validate()?;
        let segment_id = identity_token(&self.catalog_key, identity)?;
        let Some(record) = self.catalog.find(&segment_id)? else {
            return Ok(crate::AuthorizedHistoryRead::Unavailable {
                reason: AvailabilityReason::NotCached,
                recovery: missing_recovery,
            });
        };
        if record
            .retention_until
            .is_some_and(|expiry| expiry <= now_unix_seconds)
        {
            let recovery = record.recovery;
            self.remove_records(&[record])?;
            return Ok(crate::AuthorizedHistoryRead::Unavailable {
                reason: AvailabilityReason::Expired,
                recovery,
            });
        }
        if record.quarantined {
            return Ok(crate::AuthorizedHistoryRead::Unavailable {
                reason: AvailabilityReason::Quarantined,
                recovery: record.recovery,
            });
        }
        if record.key_id != encryption_key.key_id()
            || record.key_verifier != segment_key_verifier(encryption_key)?
        {
            return Err(LocalStorageError::SegmentKeyMismatch);
        }
        let payload_bytes = usize::try_from(record.payload_bytes).unwrap_or(usize::MAX);
        if payload_bytes > maximum_payload_bytes {
            return Err(LocalStorageError::SegmentTooLarge {
                requested: payload_bytes,
                maximum: maximum_payload_bytes,
            });
        }
        match self.read_record(identity, encryption_key, &record, maximum_payload_bytes)? {
            HistoryRead::Hit(payload) => {
                self.catalog.touch(&segment_id, now_unix_seconds)?;
                Ok(crate::AuthorizedHistoryRead::Hit {
                    payload,
                    access_policy: crate::SegmentAccessPolicy::new(
                        record.key_id,
                        record.key_verifier,
                        record.retention_until,
                        record.recovery,
                    ),
                })
            }
            HistoryRead::Unavailable { reason, recovery } => {
                Ok(crate::AuthorizedHistoryRead::Unavailable { reason, recovery })
            }
        }
    }

    /// Removes one exact segment when its recorded retention deadline has passed.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid identity or catalog/filesystem failure.
    pub fn remove_if_expired(
        &mut self,
        identity: &crate::SegmentIdentity,
        now_unix_seconds: i64,
    ) -> Result<bool, LocalStorageError> {
        identity.validate()?;
        let segment_id = identity_token(&self.catalog_key, identity)?;
        let Some(record) = self.catalog.find(&segment_id)? else {
            return Ok(false);
        };
        if record
            .retention_until
            .is_none_or(|expiry| expiry > now_unix_seconds)
        {
            return Ok(false);
        }
        self.remove_records(&[record])?;
        Ok(true)
    }

    /// Removes only segments made stale by the named revision or rights change.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid filter fields, filesystem failure, or
    /// catalog failure.
    pub fn invalidate(&mut self, invalidation: &Invalidation) -> Result<usize, LocalStorageError> {
        let records = self.records_for_invalidation(invalidation)?;
        let removed = records.len();
        self.remove_records(&records)?;
        Ok(removed)
    }

    /// Removes expired files and catalog entries under their recorded policies.
    ///
    /// # Errors
    ///
    /// Returns an error when file removal or the catalog transaction fails.
    pub fn purge_expired(&mut self, now_unix_seconds: i64) -> Result<usize, LocalStorageError> {
        let expired = self
            .catalog
            .all_records()?
            .into_iter()
            .filter(|record| {
                record
                    .retention_until
                    .is_some_and(|expiry| expiry <= now_unix_seconds)
            })
            .collect::<Vec<_>>();
        let removed = expired.len();
        self.remove_records(&expired)?;
        Ok(removed)
    }

    /// Deletes an account scope only after every referenced vault key is revoked.
    ///
    /// Unlinking cannot guarantee physical erasure on copy-on-write filesystems,
    /// SSDs, snapshots, or backups; the report keeps that limitation explicit.
    ///
    /// # Errors
    ///
    /// Returns an error before deleting anything when revocation evidence is
    /// missing, or when filesystem/catalog cleanup fails.
    pub fn secure_delete_account(
        &mut self,
        provider_id: &str,
        account_id: &str,
        evidence: &impl KeyRevocationEvidence,
    ) -> Result<DeletionReport, LocalStorageError> {
        validate_identifier("provider_id", provider_id)?;
        validate_identifier("account_id", account_id)?;
        let provider = crate::crypto::provider_token(&self.catalog_key, provider_id)?;
        let account = crate::crypto::account_token(&self.catalog_key, account_id)?;
        let records = self.catalog.matching(CatalogFilter::Account {
            provider: &provider,
            account: &account,
        })?;
        let key_ids = records
            .iter()
            .map(|record| record.key_id.as_str())
            .collect::<BTreeSet<_>>();
        let key_materials = records
            .iter()
            .map(|record| record.key_verifier)
            .collect::<BTreeSet<_>>();
        for key_verifier in key_materials {
            let scoped_references = records
                .iter()
                .filter(|record| record.key_verifier == key_verifier)
                .count();
            if self.catalog.key_material_reference_count(&key_verifier)? != scoped_references {
                let key_id = records
                    .iter()
                    .find(|record| record.key_verifier == key_verifier)
                    .map_or("unknown", |record| record.key_id.as_str());
                return Err(LocalStorageError::SharedKeyStillReferenced {
                    key_id: key_id.to_string(),
                });
            }
        }
        for key_id in key_ids {
            if !evidence.confirms_revocation(key_id) {
                return Err(LocalStorageError::KeyRevocationMissing {
                    key_id: key_id.to_string(),
                });
            }
        }
        let files_removed = self.unlink_record_files(&records)?;
        self.catalog.remove_records(&records)?;
        self.catalog.checkpoint_after_deletion()?;
        sync_directory(&self.root)?;
        Ok(DeletionReport {
            catalog_entries_removed: records.len(),
            files_removed,
            physical_remanence_possible: true,
        })
    }

    /// Returns bounded active/quarantined catalog counts.
    ///
    /// # Errors
    ///
    /// Returns an error when the catalog cannot be queried.
    pub fn statistics(&self) -> Result<CatalogStatistics, LocalStorageError> {
        self.catalog.statistics()
    }

    /// Enforces the hard local cache budget across all retained data classes.
    /// Least-recently-viewed unprotected segments are removed first.
    ///
    /// Protected identities are exact immutable segments. Each protected range
    /// pairs exact series dimensions with a half-open range and protects every
    /// active segment that overlaps it.
    ///
    /// # Errors
    /// Returns an error when catalog access or owned-segment deletion fails.
    pub fn enforce_cache_budget(
        &mut self,
        maximum_stored_bytes: u64,
        protected_identities: &[crate::SegmentIdentity],
        protected_ranges: &[(crate::HistorySeriesIdentity<'_>, crate::RetainedRange)],
    ) -> Result<crate::QuotaEnforcementReport, LocalStorageError> {
        self.reserve_cache_budget(
            maximum_stored_bytes,
            0,
            protected_identities,
            protected_ranges,
        )
    }

    /// Reserves physical segment bytes before publication while enforcing the
    /// hard cache ceiling. Unusable quarantine files and least-recently-viewed
    /// unprotected segments are removed first.
    ///
    /// If the current store already exceeds the ceiling, enough protected data
    /// is removed to restore the hard bound and the degradation is returned as
    /// an explicit error. A new reservation never evicts protected data: it
    /// fails before publication when the protected working set cannot coexist
    /// with the requested bytes.
    ///
    /// # Errors
    /// Returns an explicit budget error when protected data must be degraded or
    /// cannot coexist with the reservation, or an error for invalid identities,
    /// catalog access, or owned-segment deletion.
    pub fn reserve_cache_budget(
        &mut self,
        maximum_stored_bytes: u64,
        additional_stored_bytes: u64,
        protected_identities: &[crate::SegmentIdentity],
        protected_ranges: &[(crate::HistorySeriesIdentity<'_>, crate::RetainedRange)],
    ) -> Result<crate::QuotaEnforcementReport, LocalStorageError> {
        let protected = self.protected_segment_tokens(protected_identities, protected_ranges)?;
        let mut records = self.catalog.all_records()?;
        records.sort_by_key(|record| {
            (
                !record.quarantined,
                record.last_viewed_at,
                record.created_at,
                record.file_name.clone(),
            )
        });
        let record_bytes = records
            .iter()
            .map(|record| self.record_stored_bytes(record))
            .collect::<Result<Vec<u64>, LocalStorageError>>()?;
        let is_protected = records
            .iter()
            .map(|record| !record.quarantined && protected.contains(&record.segment_id))
            .collect::<Vec<_>>();
        let mut retained = record_bytes
            .iter()
            .fold(0_u64, |total, bytes| total.saturating_add(*bytes));
        let protected_bytes = record_bytes
            .iter()
            .zip(&is_protected)
            .filter(|(_, protected)| **protected)
            .fold(0_u64, |total, (bytes, _)| total.saturating_add(*bytes));
        let required = protected_bytes.saturating_add(additional_stored_bytes);
        let mut evicted_indices = BTreeSet::new();
        let mut removed_bytes = 0_u64;
        for protect_candidate in [false, true] {
            for (index, bytes) in record_bytes.iter().enumerate() {
                if retained <= maximum_stored_bytes {
                    break;
                }
                if is_protected[index] != protect_candidate {
                    continue;
                }
                retained = retained.saturating_sub(*bytes);
                removed_bytes = removed_bytes.saturating_add(*bytes);
                evicted_indices.insert(index);
            }
            if retained <= maximum_stored_bytes {
                break;
            }
        }
        let protection_degraded = evicted_indices.iter().any(|index| is_protected[*index]);
        if protection_degraded {
            let evicted = records
                .into_iter()
                .enumerate()
                .filter_map(|(index, record)| evicted_indices.contains(&index).then_some(record))
                .collect::<Vec<_>>();
            self.remove_records(&evicted)?;
            return Err(LocalStorageError::CacheBudgetExceeded {
                required,
                maximum: maximum_stored_bytes,
            });
        }

        if required > maximum_stored_bytes {
            let evicted = records
                .into_iter()
                .enumerate()
                .filter_map(|(index, record)| evicted_indices.contains(&index).then_some(record))
                .collect::<Vec<_>>();
            self.remove_records(&evicted)?;
            return Err(LocalStorageError::CacheBudgetExceeded {
                required,
                maximum: maximum_stored_bytes,
            });
        }
        let reservation_target = maximum_stored_bytes.saturating_sub(additional_stored_bytes);
        for (index, bytes) in record_bytes.iter().enumerate() {
            if retained <= reservation_target {
                break;
            }
            if evicted_indices.contains(&index) || is_protected[index] {
                continue;
            }
            retained = retained.saturating_sub(*bytes);
            removed_bytes = removed_bytes.saturating_add(*bytes);
            evicted_indices.insert(index);
        }
        let evicted = records
            .into_iter()
            .enumerate()
            .filter_map(|(index, record)| evicted_indices.contains(&index).then_some(record))
            .collect::<Vec<_>>();
        self.remove_records(&evicted)?;
        Ok(crate::QuotaEnforcementReport {
            entries_removed: evicted.len(),
            payload_bytes_removed: removed_bytes,
            payload_bytes_retained: retained,
        })
    }

    fn protected_segment_tokens(
        &self,
        protected_identities: &[crate::SegmentIdentity],
        protected_ranges: &[(crate::HistorySeriesIdentity<'_>, crate::RetainedRange)],
    ) -> Result<BTreeSet<[u8; 32]>, LocalStorageError> {
        let mut protected = BTreeSet::new();
        for identity in protected_identities {
            identity.validate()?;
            protected.insert(identity_token(&self.catalog_key, identity)?);
        }
        for (series, range) in protected_ranges {
            for identity in self.retained_identities_in_range(*series, *range, i64::MIN)? {
                protected.insert(identity_token(&self.catalog_key, &identity)?);
            }
        }
        Ok(protected)
    }

    fn record_stored_bytes(&self, record: &CatalogRecord) -> Result<u64, LocalStorageError> {
        let path = if record.quarantined {
            let Some(file_name) = &record.quarantine_file_name else {
                return Ok(0);
            };
            self.quarantine_path(file_name)?
        } else {
            self.record_path(record)?
        };
        fs::metadata(path)
            .map(|metadata| metadata.len())
            .map_err(Into::into)
    }

    fn publish_retained(
        &mut self,
        request: &PublicationRequest<'_>,
        retention_until: Option<i64>,
    ) -> Result<PublicationOutcome, LocalStorageError> {
        let associated_data = encode_identity(request.identity);
        let segment_id = identity_token(&self.catalog_key, request.identity)?;
        let request_key_verifier = segment_key_verifier(request.encryption_key)?;
        if self
            .catalog
            .key_id_has_different_verifier(request.encryption_key.key_id(), &request_key_verifier)?
        {
            return Err(LocalStorageError::SegmentKeyMismatch);
        }
        let replacement = match self.replacement_record(request, &segment_id, retention_until)? {
            ExistingPublication::Idempotent(receipt) => {
                return Ok(PublicationOutcome::Published(receipt));
            }
            ExistingPublication::ReplaceQuarantined(record) => Some(record),
            ExistingPublication::Absent => None,
        };
        if replacement.is_none() {
            let statistics = self.catalog.statistics()?;
            if statistics
                .active_entries
                .saturating_add(statistics.quarantined_entries)
                .saturating_add(statistics.coverage_entries)
                >= statistics.maximum_entries
            {
                return Err(LocalStorageError::CatalogFull {
                    maximum: statistics.maximum_entries,
                });
            }
        }
        let encoded = encrypt_segment(request.payload, &associated_data, request.encryption_key)?;
        let file_name = format!("{}.seg", hex(&segment_id));
        let final_path = self.segments.join(&file_name);
        let staged_path = self.unique_staging_path()?;
        if let Err(error) = write_synced_file(&staged_path, &encoded.file) {
            let _ = remove_owned_file(&staged_path);
            let _ = sync_directory(&self.staging);
            return Err(error);
        }
        if let Err(error) = fs::hard_link(&staged_path, &final_path) {
            let _ = fs::remove_file(&staged_path);
            return if error.kind() == io::ErrorKind::AlreadyExists {
                Err(LocalStorageError::SegmentAlreadyExists)
            } else {
                Err(error.into())
            };
        }
        let finalize_result = (|| -> Result<(), LocalStorageError> {
            fs::remove_file(&staged_path)?;
            make_file_read_only(&final_path)?;
            sync_directory(&self.segments)
        })();
        if let Err(error) = finalize_result {
            let _ = remove_owned_file(&final_path);
            let _ = fs::remove_file(&staged_path);
            let _ = sync_directory(&self.segments);
            return Err(error);
        }
        if let Some(record) = &replacement {
            let cleanup_result = self
                .remove_quarantine_file(record)
                .and_then(|_| sync_directory(&self.quarantine));
            if let Err(error) = cleanup_result {
                let _ = remove_owned_file(&final_path);
                let _ = sync_directory(&self.segments);
                return Err(error);
            }
        }
        let catalog_result = self.insert_catalog_record(
            request,
            retention_until,
            &segment_id,
            &file_name,
            &encoded,
            replacement.is_some(),
        );
        if let Err(error) = catalog_result {
            let _ = remove_owned_file(&final_path);
            let _ = sync_directory(&self.segments);
            return Err(error);
        }
        Ok(PublicationOutcome::Published(SegmentReceipt {
            segment_id: hex(&segment_id),
            file_name,
            payload_bytes: u64::try_from(request.payload.len()).unwrap_or(u64::MAX),
            stored_bytes: u64::try_from(encoded.file.len()).unwrap_or(u64::MAX),
            storage_checksum: encoded.storage_checksum,
        }))
    }

    fn remove_retained_identity(
        &mut self,
        identity: &crate::SegmentIdentity,
    ) -> Result<(), LocalStorageError> {
        let segment_id = identity_token(&self.catalog_key, identity)?;
        if let Some(record) = self.catalog.find(&segment_id)? {
            self.remove_records(&[record])?;
        }
        Ok(())
    }

    fn insert_catalog_record(
        &self,
        request: &PublicationRequest<'_>,
        retention_until: Option<i64>,
        segment_id: &[u8; 32],
        file_name: &str,
        encoded: &crate::crypto::EncodedSegment,
        replacement: bool,
    ) -> Result<(), LocalStorageError> {
        let scope = scope_tokens(&self.catalog_key, &request.identity.scope)?;
        let instrument = instrument_token(&self.catalog_key, &request.identity.instrument_id)?;
        let resolution = resolution_token(&self.catalog_key, &request.identity.resolution)?;
        let key_verifier = segment_key_verifier(request.encryption_key)?;
        let record = NewCatalogRecord {
            segment_id,
            provider_token: &scope.provider,
            account_token: &scope.account,
            entitlement_token: &scope.entitlement,
            instrument_token: &instrument,
            data_kind: request.identity.data_kind.code(),
            resolution_token: &resolution,
            range_start: request.identity.range_start_unix_nanos,
            range_end: request.identity.range_end_unix_nanos,
            source_revision: request.identity.source_revision,
            schema_revision: request.identity.schema_revision,
            calendar_revision: request.identity.calendar_revision,
            adjustment_revision: request.identity.adjustment_revision,
            correction_revision: request.identity.correction_revision,
            file_name,
            file_checksum: &encoded.storage_checksum,
            payload_checksum: &encoded.payload_checksum,
            payload_bytes: u64::try_from(request.payload.len()).unwrap_or(u64::MAX),
            key_id: request.encryption_key.key_id(),
            key_verifier: &key_verifier,
            retention_until,
            recovery: request.recovery,
            created_at: request.now_unix_seconds,
        };
        if replacement {
            self.catalog.replace_quarantined(&record)
        } else {
            self.catalog.insert(&record)
        }
    }

    fn replacement_record(
        &mut self,
        request: &PublicationRequest<'_>,
        segment_id: &[u8; 32],
        retention_until: Option<i64>,
    ) -> Result<ExistingPublication, LocalStorageError> {
        let Some(record) = self.catalog.find(segment_id)? else {
            return Ok(ExistingPublication::Absent);
        };
        if record
            .retention_until
            .is_some_and(|expiry| expiry <= request.now_unix_seconds)
        {
            self.remove_records(&[record])?;
            return Ok(ExistingPublication::Absent);
        }
        let key_matches = record.key_id == request.encryption_key.key_id()
            && record.key_verifier == segment_key_verifier(request.encryption_key)?;
        if !record.quarantined
            && key_matches
            && record.payload_bytes == u64::try_from(request.payload.len()).unwrap_or(u64::MAX)
            && record.payload_checksum == checksum(request.payload)
            && record.retention_until == retention_until
            && record.recovery == request.recovery
        {
            let stored_bytes = fs::metadata(self.record_path(&record)?)?.len();
            return Ok(ExistingPublication::Idempotent(SegmentReceipt {
                segment_id: hex(segment_id),
                file_name: record.file_name,
                payload_bytes: record.payload_bytes,
                stored_bytes,
                storage_checksum: record.file_checksum,
            }));
        }
        if !record.quarantined
            || record.recovery != RecoveryAction::ProviderRefetch
            || request.recovery != RecoveryAction::ProviderRefetch
            || !key_matches
        {
            return Err(LocalStorageError::SegmentAlreadyExists);
        }
        Ok(ExistingPublication::ReplaceQuarantined(record))
    }

    fn read_record(
        &mut self,
        identity: &crate::SegmentIdentity,
        encryption_key: &SegmentEncryptionKey,
        record: &CatalogRecord,
        maximum_payload_bytes: usize,
    ) -> Result<HistoryRead, LocalStorageError> {
        let path = match self.record_path(record) {
            Ok(path) => path,
            Err(LocalStorageError::CorruptSegment(_)) => {
                self.catalog.mark_quarantined(
                    &record.segment_id,
                    "unsafe_catalog_file_name",
                    None,
                )?;
                return Ok(unavailable(
                    AvailabilityReason::Quarantined,
                    record.recovery,
                ));
            }
            Err(error) => return Err(error),
        };
        let maximum_file_bytes = maximum_payload_bytes.saturating_add(SEGMENT_FILE_OVERHEAD_BYTES);
        let file = match read_bounded_file(&path, maximum_file_bytes) {
            Ok(file) => file,
            Err(LocalStorageError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                self.quarantine_record(record, "file_missing")?;
                return Ok(unavailable(
                    AvailabilityReason::Quarantined,
                    record.recovery,
                ));
            }
            Err(LocalStorageError::SegmentTooLarge { .. }) => {
                self.quarantine_record(record, "stored_segment_exceeds_bound")?;
                return Ok(unavailable(
                    AvailabilityReason::Quarantined,
                    record.recovery,
                ));
            }
            Err(error) => return Err(error),
        };
        if checksum(&file) != record.file_checksum {
            self.quarantine_record(record, "storage_checksum_mismatch")?;
            return Ok(unavailable(
                AvailabilityReason::Quarantined,
                record.recovery,
            ));
        }
        let associated_data = encode_identity(identity);
        let payload = match decrypt_segment(&file, &associated_data, encryption_key) {
            Ok(payload) => payload,
            Err(LocalStorageError::AuthenticationFailed | LocalStorageError::CorruptSegment(_)) => {
                self.quarantine_record(record, "authentication_or_format_failure")?;
                return Ok(unavailable(
                    AvailabilityReason::Quarantined,
                    record.recovery,
                ));
            }
            Err(error) => return Err(error),
        };
        if u64::try_from(payload.len()).unwrap_or(u64::MAX) != record.payload_bytes
            || checksum(&payload) != record.payload_checksum
        {
            self.quarantine_record(record, "payload_checksum_mismatch")?;
            return Ok(unavailable(
                AvailabilityReason::Quarantined,
                record.recovery,
            ));
        }
        Ok(HistoryRead::Hit(payload))
    }

    fn records_for_invalidation(
        &self,
        invalidation: &Invalidation,
    ) -> Result<Vec<CatalogRecord>, LocalStorageError> {
        match invalidation {
            Invalidation::Entitlement { scope } => {
                scope.validate()?;
                let tokens = scope_tokens(&self.catalog_key, scope)?;
                self.catalog.matching(CatalogFilter::Scope {
                    provider: &tokens.provider,
                    account: &tokens.account,
                    entitlement: &tokens.entitlement,
                })
            }
            Invalidation::Account {
                provider_id,
                account_id,
            } => {
                validate_identifier("provider_id", provider_id)?;
                validate_identifier("account_id", account_id)?;
                let provider = crate::crypto::provider_token(&self.catalog_key, provider_id)?;
                let account = crate::crypto::account_token(&self.catalog_key, account_id)?;
                self.catalog.matching(CatalogFilter::Account {
                    provider: &provider,
                    account: &account,
                })
            }
            Invalidation::Schema {
                scope,
                current_revision,
            } => {
                validate_revision(*current_revision, "schema_revision")?;
                self.scope_revision_records(scope, None, |tokens, _| CatalogFilter::Schema {
                    provider: &tokens.provider,
                    account: &tokens.account,
                    entitlement: &tokens.entitlement,
                    current: *current_revision,
                })
            }
            Invalidation::Source {
                scope,
                instrument_id,
                current_revision,
            } => self.source_revision_records(scope, instrument_id, *current_revision),
            Invalidation::Calendar {
                scope,
                instrument_id,
                current_revision,
            } => {
                validate_revision(*current_revision, "calendar_revision")?;
                self.dimension_revision_records(scope, instrument_id, |tokens, instrument| {
                    CatalogFilter::Calendar {
                        provider: &tokens.provider,
                        account: &tokens.account,
                        entitlement: &tokens.entitlement,
                        instrument,
                        current: *current_revision,
                    }
                })
            }
            Invalidation::Adjustment {
                scope,
                instrument_id,
                current_revision,
            } => {
                validate_revision(*current_revision, "adjustment_revision")?;
                self.dimension_revision_records(scope, instrument_id, |tokens, instrument| {
                    CatalogFilter::Adjustment {
                        provider: &tokens.provider,
                        account: &tokens.account,
                        entitlement: &tokens.entitlement,
                        instrument,
                        current: *current_revision,
                    }
                })
            }
            Invalidation::Correction {
                scope,
                instrument_id,
                current_revision,
            } => {
                if *current_revision == 0 || *current_revision > i64::MAX as u64 {
                    return Err(LocalStorageError::InvalidIdentity("correction_revision"));
                }
                self.dimension_revision_records(scope, instrument_id, |tokens, instrument| {
                    CatalogFilter::Correction {
                        provider: &tokens.provider,
                        account: &tokens.account,
                        entitlement: &tokens.entitlement,
                        instrument,
                        current: *current_revision,
                    }
                })
            }
        }
    }

    fn source_revision_records(
        &self,
        scope: &HistoryScope,
        instrument_id: &str,
        current_revision: u32,
    ) -> Result<Vec<CatalogRecord>, LocalStorageError> {
        validate_revision(current_revision, "source_revision")?;
        self.dimension_revision_records(scope, instrument_id, |tokens, instrument| {
            CatalogFilter::Source {
                provider: &tokens.provider,
                account: &tokens.account,
                entitlement: &tokens.entitlement,
                instrument,
                current: current_revision,
            }
        })
    }

    fn scope_revision_records<F>(
        &self,
        scope: &HistoryScope,
        instrument_id: Option<&str>,
        filter: F,
    ) -> Result<Vec<CatalogRecord>, LocalStorageError>
    where
        F: for<'a> FnOnce(
            &'a crate::crypto::ScopeTokens,
            Option<&'a [u8; 32]>,
        ) -> CatalogFilter<'a>,
    {
        scope.validate()?;
        let tokens = scope_tokens(&self.catalog_key, scope)?;
        let instrument = instrument_id
            .map(|value| instrument_token(&self.catalog_key, value))
            .transpose()?;
        self.catalog.matching(filter(&tokens, instrument.as_ref()))
    }

    fn dimension_revision_records<F>(
        &self,
        scope: &HistoryScope,
        instrument_id: &str,
        filter: F,
    ) -> Result<Vec<CatalogRecord>, LocalStorageError>
    where
        F: for<'a> FnOnce(&'a crate::crypto::ScopeTokens, &'a [u8; 32]) -> CatalogFilter<'a>,
    {
        scope.validate()?;
        validate_identifier("instrument_id", instrument_id)?;
        let tokens = scope_tokens(&self.catalog_key, scope)?;
        let instrument = instrument_token(&self.catalog_key, instrument_id)?;
        self.catalog.matching(filter(&tokens, &instrument))
    }

    fn remove_records(&mut self, records: &[CatalogRecord]) -> Result<(), LocalStorageError> {
        self.unlink_record_files(records)?;
        self.catalog.remove_records(records)?;
        sync_directory(&self.segments)?;
        Ok(())
    }

    fn unlink_record_files(&self, records: &[CatalogRecord]) -> Result<usize, LocalStorageError> {
        let mut removed = 0_usize;
        for record in records {
            if let Ok(path) = self.record_path(record) {
                removed = removed.saturating_add(usize::from(remove_owned_file(&path)?));
            }
            if let Some(quarantine_file_name) = &record.quarantine_file_name {
                let quarantine_path = self.quarantine_path(quarantine_file_name)?;
                removed = removed.saturating_add(usize::from(remove_owned_file(&quarantine_path)?));
            }
        }
        Ok(removed)
    }

    fn remove_quarantine_file(&self, record: &CatalogRecord) -> Result<bool, LocalStorageError> {
        let Some(file_name) = &record.quarantine_file_name else {
            return Ok(false);
        };
        remove_owned_file(&self.quarantine_path(file_name)?)
    }

    fn quarantine_record(
        &mut self,
        record: &CatalogRecord,
        reason: &str,
    ) -> Result<(), LocalStorageError> {
        let source = self.record_path(record)?;
        let quarantine_file_name = if source.exists() {
            let destination = self.unique_quarantine_path(&record.file_name)?;
            let file_name = destination
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or(LocalStorageError::InvalidConfiguration(
                    "quarantine path lacks a valid file name",
                ))?
                .to_string();
            fs::rename(source, &destination)?;
            sync_directory(&self.segments)?;
            sync_directory(&self.quarantine)?;
            Some(file_name)
        } else {
            None
        };
        self.catalog
            .mark_quarantined(&record.segment_id, reason, quarantine_file_name.as_deref())
    }

    fn recover_filesystem(&mut self) -> Result<(), LocalStorageError> {
        remove_directory_contents(&self.staging)?;
        let active_names = self.catalog.active_file_names()?;
        for entry in fs::read_dir(&self.segments)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                return Err(LocalStorageError::InvalidConfiguration(
                    "segments directory contains a non-file entry",
                ));
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !active_names.contains(&name) {
                remove_owned_file(&entry.path())?;
            }
        }
        let quarantine_names = self.catalog.quarantine_file_names()?;
        for entry in fs::read_dir(&self.quarantine)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                return Err(LocalStorageError::InvalidConfiguration(
                    "quarantine directory contains a non-file entry",
                ));
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !quarantine_names.contains(&name) {
                remove_owned_file(&entry.path())?;
            }
        }
        for record in self.catalog.active_records()? {
            let path = self.record_path(&record);
            if path
                .as_ref()
                .is_err_and(|error| matches!(error, LocalStorageError::CorruptSegment(_)))
                || path.as_ref().is_ok_and(|path| !path.is_file())
            {
                self.catalog.mark_quarantined(
                    &record.segment_id,
                    "missing_during_recovery",
                    None,
                )?;
            }
        }
        sync_directory(&self.staging)?;
        sync_directory(&self.segments)?;
        sync_directory(&self.quarantine)?;
        Ok(())
    }

    fn record_path(&self, record: &CatalogRecord) -> Result<PathBuf, LocalStorageError> {
        if !is_segment_file_name(&record.file_name) {
            return Err(LocalStorageError::CorruptSegment(
                "catalog contains an unsafe file name",
            ));
        }
        Ok(self.segments.join(&record.file_name))
    }

    fn unique_staging_path(&self) -> Result<PathBuf, LocalStorageError> {
        unique_path(&self.staging, "stage", "tmp")
    }

    fn unique_quarantine_path(&self, file_name: &str) -> Result<PathBuf, LocalStorageError> {
        unique_path(&self.quarantine, file_name, "corrupt")
    }

    fn quarantine_path(&self, file_name: &str) -> Result<PathBuf, LocalStorageError> {
        if file_name.is_empty()
            || file_name.len() > 160
            || file_name.contains(['/', '\\'])
            || file_name.chars().any(char::is_control)
        {
            return Err(LocalStorageError::CorruptSegment(
                "catalog contains an unsafe quarantine file name",
            ));
        }
        Ok(self.quarantine.join(file_name))
    }
}

fn series_dimensions(series: crate::HistorySeriesIdentity<'_>) -> SeriesDimensions {
    SeriesDimensions {
        data_kind: series.data_kind.code(),
        source_revision: series.source_revision,
        schema_revision: series.schema_revision,
        calendar_revision: series.calendar_revision,
        adjustment_revision: series.adjustment_revision,
        correction_revision: series.correction_revision,
    }
}

fn history_ranges(ranges: Vec<(i64, i64)>) -> Vec<HistoryRange> {
    ranges
        .into_iter()
        .map(|(start_unix_nanos, end_unix_nanos)| HistoryRange {
            start_unix_nanos,
            end_unix_nanos,
        })
        .collect()
}

fn unavailable(reason: AvailabilityReason, recovery: RecoveryAction) -> HistoryRead {
    HistoryRead::Unavailable { reason, recovery }
}

fn validate_revision(revision: u32, field: &'static str) -> Result<(), LocalStorageError> {
    if revision == 0 {
        return Err(LocalStorageError::InvalidIdentity(field));
    }
    Ok(())
}

fn unique_path(
    directory: &Path,
    prefix: &str,
    extension: &str,
) -> Result<PathBuf, LocalStorageError> {
    for _ in 0..16 {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce)?;
        let path = directory.join(format!("{prefix}.{}.{}", hex(&nonce), extension));
        if !path.exists() {
            return Ok(path);
        }
    }
    Err(LocalStorageError::InvalidConfiguration(
        "failed to allocate a unique owned file name",
    ))
}

fn write_synced_file(path: &Path, bytes: &[u8]) -> Result<(), LocalStorageError> {
    let mut file = private_new_file(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn open_root_lock(path: &Path) -> Result<File, LocalStorageError> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    try_lock_root(file)
}

#[cfg(not(unix))]
fn open_root_lock(path: &Path) -> Result<File, LocalStorageError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    try_lock_root(file)
}

fn try_lock_root(file: File) -> Result<File, LocalStorageError> {
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(LocalStorageError::StoreAlreadyOpen),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

#[cfg(unix)]
fn make_file_read_only(path: &Path) -> Result<(), LocalStorageError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o400))?;
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn make_file_read_only(path: &Path) -> Result<(), LocalStorageError> {
    OpenOptions::new().write(true).open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn make_file_read_only(path: &Path) -> Result<(), LocalStorageError> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)?;
    File::open(path)?.sync_all()?;
    Ok(())
}

fn read_bounded_file(path: &Path, maximum: usize) -> Result<Vec<u8>, LocalStorageError> {
    let file = File::open(path)?;
    let length = usize::try_from(file.metadata()?.len()).unwrap_or(usize::MAX);
    if length > maximum {
        return Err(LocalStorageError::SegmentTooLarge {
            requested: length,
            maximum,
        });
    }
    let mut bytes = Vec::with_capacity(length);
    file.take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(LocalStorageError::SegmentTooLarge {
            requested: bytes.len(),
            maximum,
        });
    }
    Ok(bytes)
}

fn remove_directory_contents(directory: &Path) -> Result<(), LocalStorageError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            return Err(LocalStorageError::InvalidConfiguration(
                "owned staging directory contains a non-file entry",
            ));
        }
        remove_owned_file(&entry.path())?;
    }
    Ok(())
}

#[cfg(unix)]
fn remove_owned_file(path: &Path) -> Result<bool, LocalStorageError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn remove_owned_file(path: &Path) -> Result<bool, LocalStorageError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(any(unix, windows)))]
fn remove_owned_file(path: &Path) -> Result<bool, LocalStorageError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn is_segment_file_name(name: &str) -> bool {
    name.len() == 68
        && Path::new(name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("seg"))
        && name[..64]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn create_private_directory(path: &Path) -> Result<(), LocalStorageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(LocalStorageError::InvalidConfiguration(
                "owned storage path is not a real directory",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(path)?,
        Err(error) => return Err(error.into()),
    }
    harden_private_directory(path)
}

#[cfg(unix)]
fn harden_private_directory(path: &Path) -> Result<(), LocalStorageError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn harden_private_directory(path: &Path) -> Result<(), LocalStorageError> {
    fs::metadata(path)?;
    Ok(())
}

#[cfg(unix)]
fn private_new_file(path: &Path) -> Result<File, LocalStorageError> {
    use std::os::unix::fs::OpenOptionsExt;
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}

#[cfg(not(unix))]
fn private_new_file(path: &Path) -> Result<File, LocalStorageError> {
    Ok(OpenOptions::new().write(true).create_new(true).open(path)?)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), LocalStorageError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn sync_directory(path: &Path) -> Result<(), LocalStorageError> {
    use std::os::windows::fs::OpenOptionsExt;

    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    OpenOptions::new()
        .access_mode(GENERIC_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?
        .sync_all()?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_directory(_path: &Path) -> Result<(), LocalStorageError> {
    Err(LocalStorageError::InvalidConfiguration(
        "directory metadata durability is unavailable on this platform",
    ))
}
