//! Provider-neutral encrypted local bar-history mechanics.

use crate::{HistoryScope, LocalHistoryError};

use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use axiusflow_local_storage::{
    CatalogKey, DataKind, HistoryRead, HistorySeriesIdentity, HistoryStore, LocalStorageError,
    MAXIMUM_CATALOG_ENTRIES, PublicationRequest, RecoveryAction, RetainedRange, RetentionPolicy,
    SegmentEncryptionKey, SegmentIdentity,
};
use axiusflow_market_data::{BarPeriod, BarSeriesKey, MarketBar};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use zeroize::{Zeroize, Zeroizing};

const VAULT_SERVICE: &str = "com.axiusflow.engine.history";
const CATALOG_KEY_ID: &str = "history-catalog-key-v1";
const SEGMENT_KEY_ID: &str = "coinbase-public-bars-key-v1";
const LOCAL_BAR_SEGMENT_MAGIC: &[u8; 8] = b"AXLBAR02";
const LOCAL_BAR_SEGMENT_HEADER_BYTES: usize = LOCAL_BAR_SEGMENT_MAGIC.len() + 4;
const LOCAL_BAR_BYTES: usize = 8 * 8;
const MAXIMUM_LOCAL_HISTORY_BARS: usize = 10_000;
const CURRENT_HISTORY_SCHEMA_REVISION: u32 = 2;
const LEGACY_HISTORY_SCHEMA_REVISION: u32 = 1;
const LEGACY_COINBASE_SEGMENT_MAGIC: &[u8; 6] = b"AXCBS1";
const LEGACY_COINBASE_PAYLOAD_MAGIC: &[u8; 6] = b"AXCBH1";
const LEGACY_COINBASE_PAYLOAD_BYTES: usize = LEGACY_COINBASE_PAYLOAD_MAGIC.len() + 7 * 8;
const LEGACY_COINBASE_HEADER_BYTES: usize = LEGACY_COINBASE_SEGMENT_MAGIC.len() + 4;
const DEFAULT_CACHE_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const NANOS_PER_DAY: i64 = 86_400 * 1_000_000_000;

/// Engine-owned access to authenticated immutable local bar segments.
pub struct LocalHistoryStore {
    store: HistoryStore,
    segment_key: SegmentEncryptionKey,
    cache_budget_bytes: u64,
}

/// One validated retained canonical series.
pub struct StoredHistory {
    pub bars: Vec<MarketBar>,
    pub derived: bool,
    pub durable: bool,
}

impl LocalHistoryStore {
    /// Opens the production local-history store with keys from the native vault.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the root, vault, keys, or catalog are unavailable.
    pub fn open(root: &Path) -> Result<Self, LocalHistoryError> {
        let parent = root.parent().ok_or(LocalHistoryError::InvalidRoot)?;
        fs::create_dir_all(parent).map_err(redacted)?;
        let vault = NativeCredentialVault::new(VAULT_SERVICE).map_err(redacted)?;
        let catalog_key = CatalogKey::try_new(
            CATALOG_KEY_ID.to_string(),
            load_or_create_key(&vault, CATALOG_KEY_ID)?,
        )
        .map_err(redacted)?;
        let segment_key = SegmentEncryptionKey::try_new(
            SEGMENT_KEY_ID.to_string(),
            load_or_create_key(&vault, SEGMENT_KEY_ID)?,
        )
        .map_err(redacted)?;
        let mut store =
            HistoryStore::open(root, catalog_key, MAXIMUM_CATALOG_ENTRIES).map_err(redacted)?;
        store
            .enforce_cache_budget(DEFAULT_CACHE_BUDGET_BYTES, &[], &[])
            .map_err(redacted)?;
        Ok(Self {
            store,
            segment_key,
            cache_budget_bytes: DEFAULT_CACHE_BUDGET_BYTES,
        })
    }

    #[cfg(test)]
    fn open_fixture(
        root: &Path,
        catalog_bytes: [u8; 32],
        segment_bytes: [u8; 32],
    ) -> Result<Self, LocalHistoryError> {
        Self::open_fixture_with_cache_budget(
            root,
            catalog_bytes,
            segment_bytes,
            DEFAULT_CACHE_BUDGET_BYTES,
        )
    }

    #[cfg(test)]
    fn open_fixture_with_cache_budget(
        root: &Path,
        catalog_bytes: [u8; 32],
        segment_bytes: [u8; 32],
        cache_budget_bytes: u64,
    ) -> Result<Self, LocalHistoryError> {
        let catalog_key =
            CatalogKey::try_new(CATALOG_KEY_ID.to_string(), catalog_bytes).map_err(redacted)?;
        let segment_key = SegmentEncryptionKey::try_new(SEGMENT_KEY_ID.to_string(), segment_bytes)
            .map_err(redacted)?;
        let mut store =
            HistoryStore::open(root, catalog_key, MAXIMUM_CATALOG_ENTRIES).map_err(redacted)?;
        store
            .enforce_cache_budget(cache_budget_bytes, &[], &[])
            .map_err(redacted)?;
        Ok(Self {
            store,
            segment_key,
            cache_budget_bytes,
        })
    }

    /// Reads the newest exact retained bars for one provider-neutral scoped series.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for invalid identity, catalog, decryption, or payload data.
    pub fn read_latest(
        &mut self,
        scope: &HistoryScope,
        series: &BarSeriesKey,
    ) -> Result<Option<StoredHistory>, LocalHistoryError> {
        validate_scope_series(scope, series)?;
        let identities = self.retained_identities_in_range(
            scope,
            series,
            RetainedRange {
                start_unix_nanos: i64::MIN,
                end_unix_nanos: i64::MAX,
            },
        )?;
        let Some((_, newest)) = identities.iter().max_by_key(|(priority, identity)| {
            (
                identity.range_end_unix_nanos,
                identity.range_start_unix_nanos,
                *priority,
            )
        }) else {
            return Ok(None);
        };
        let mut newest_window = RetainedRange {
            start_unix_nanos: newest.range_start_unix_nanos,
            end_unix_nanos: newest.range_end_unix_nanos,
        };
        loop {
            let previous = newest_window;
            for (_, identity) in &identities {
                if identity.range_start_unix_nanos <= newest_window.end_unix_nanos
                    && identity.range_end_unix_nanos >= newest_window.start_unix_nanos
                {
                    newest_window.start_unix_nanos = newest_window
                        .start_unix_nanos
                        .min(identity.range_start_unix_nanos);
                    newest_window.end_unix_nanos = newest_window
                        .end_unix_nanos
                        .max(identity.range_end_unix_nanos);
                }
            }
            if newest_window == previous {
                break;
            }
        }
        self.read_retained(identities, newest_window)
    }

    fn retained_identities_in_range(
        &mut self,
        scope: &HistoryScope,
        series: &BarSeriesKey,
        requested: RetainedRange,
    ) -> Result<Vec<(u8, SegmentIdentity)>, LocalHistoryError> {
        let current_resolution = resolution(series)?;
        let mut identities = Vec::new();
        for (data_kind, schema_revision, priority) in [
            (DataKind::Derived, LEGACY_HISTORY_SCHEMA_REVISION, 0),
            (DataKind::Derived, CURRENT_HISTORY_SCHEMA_REVISION, 1),
            (DataKind::Bars, LEGACY_HISTORY_SCHEMA_REVISION, 2),
            (DataKind::Bars, CURRENT_HISTORY_SCHEMA_REVISION, 3),
        ] {
            for identity in self
                .store
                .retained_identities_in_range(
                    series_identity(
                        scope,
                        series,
                        &current_resolution,
                        data_kind,
                        schema_revision,
                    ),
                    requested,
                    now_seconds(),
                )
                .map_err(redacted)?
            {
                identities.push((priority, identity));
            }
        }
        Ok(identities)
    }

    fn read_retained(
        &mut self,
        mut identities: Vec<(u8, SegmentIdentity)>,
        requested: RetainedRange,
    ) -> Result<Option<StoredHistory>, LocalHistoryError> {
        identities.sort_by_key(|(priority, identity)| {
            (
                *priority,
                identity.range_start_unix_nanos,
                identity.range_end_unix_nanos,
            )
        });
        let mut merged = BTreeMap::new();
        let mut native = false;
        for (_, identity) in identities {
            if identity.range_start_unix_nanos >= requested.end_unix_nanos
                || identity.range_end_unix_nanos <= requested.start_unix_nanos
            {
                continue;
            }
            if let HistoryRead::Hit(payload) = self
                .store
                .read(
                    &identity,
                    &self.segment_key,
                    now_seconds(),
                    RecoveryAction::ProviderRefetch,
                )
                .map_err(redacted)?
            {
                for bar in decode_local_history_segment(&payload)? {
                    if bar.exchange_timestamp_unix_nanos >= requested.start_unix_nanos
                        && bar.exchange_timestamp_unix_nanos < requested.end_unix_nanos
                    {
                        native |= identity.data_kind == DataKind::Bars;
                        merged.insert(bar.exchange_timestamp_unix_nanos, bar);
                    }
                }
            }
        }
        for (index, bar) in merged.values_mut().enumerate() {
            bar.source_sequence = u64::try_from(index)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or(LocalHistoryError::InvalidSegment)?;
        }
        Ok((!merged.is_empty()).then(|| StoredHistory {
            bars: merged.into_values().collect(),
            derived: !native,
            durable: true,
        }))
    }

    /// Publishes one validated immutable canonical bar segment.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for invalid identity/data or failed durable publication.
    pub fn persist(
        &mut self,
        scope: &HistoryScope,
        series: &BarSeriesKey,
        bars: &[MarketBar],
        derived: bool,
    ) -> Result<(), LocalHistoryError> {
        self.persist_with_protected_series_ranges(scope, series, bars, derived, &[])
    }

    /// Publishes bars while protecting every currently demanded exact series range.
    ///
    /// # Errors
    /// Returns a redacted error for invalid protected identity/range or persistence failure.
    pub fn persist_with_protected_series_ranges(
        &mut self,
        scope: &HistoryScope,
        series: &BarSeriesKey,
        bars: &[MarketBar],
        derived: bool,
        protected_ranges: &[(HistoryScope, BarSeriesKey, RetainedRange)],
    ) -> Result<(), LocalHistoryError> {
        validate_scope_series(scope, series)?;
        if bars.is_empty() {
            return Err(LocalHistoryError::EmptySeries);
        }
        let current_resolution = resolution(series)?;
        for (protected_scope, protected_series, range) in protected_ranges {
            validate_scope_series(protected_scope, protected_series)?;
            if range.start_unix_nanos >= range.end_unix_nanos {
                return Err(LocalHistoryError::InvalidRange);
            }
        }
        let publication_now = now_seconds();
        let (publication_identities, already_stored_identities, additional_stored_bytes) = self
            .prepare_publications(
                scope,
                series,
                bars,
                derived,
                &current_resolution,
                publication_now,
            )?;
        let mut resolutions = Vec::with_capacity(protected_ranges.len());
        for (_, protected_series, _) in protected_ranges {
            resolutions.push(resolution(protected_series)?);
        }
        let protected_series_ranges = protected_storage_ranges(protected_ranges, &resolutions);
        if !derived {
            let first = publication_identities
                .first()
                .ok_or(LocalHistoryError::EmptySeries)?;
            let last = publication_identities
                .last()
                .ok_or(LocalHistoryError::EmptySeries)?;
            self.resolve_confirmed_empty(
                scope,
                series,
                first.range_start_unix_nanos,
                last.range_end_unix_nanos,
            )?;
        }
        self.store
            .reserve_cache_budget(
                self.cache_budget_bytes,
                additional_stored_bytes,
                &already_stored_identities,
                &protected_series_ranges,
            )
            .map_err(|error| persistence_failure(&error))?;

        for (identity, chunk) in publication_identities
            .iter()
            .zip(bars.chunks(MAXIMUM_LOCAL_HISTORY_BARS))
        {
            let payload = encode_local_history_segment(chunk)
                .map_err(|_| LocalHistoryError::SegmentEncode)?;
            self.store
                .publish(PublicationRequest {
                    identity,
                    payload: &payload,
                    encryption_key: &self.segment_key,
                    retention: RetentionPolicy::UntilRevoked,
                    recovery: RecoveryAction::ProviderRefetch,
                    now_unix_seconds: publication_now,
                })
                .map_err(|error| persistence_failure(&error))?;
        }
        Ok(())
    }

    /// Retires provider-confirmed empty evidence superseded by real bars.
    ///
    /// # Errors
    /// Returns an error for an invalid scope, series, or range, or when the
    /// catalog update fails.
    pub fn resolve_confirmed_empty(
        &mut self,
        scope: &HistoryScope,
        series: &BarSeriesKey,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    ) -> Result<(), LocalHistoryError> {
        validate_scope_series(scope, series)?;
        if start_unix_nanos >= end_unix_nanos {
            return Err(LocalHistoryError::InvalidRange);
        }
        let current_resolution = resolution(series)?;
        self.store
            .resolve_repaired_range(
                series_identity(
                    scope,
                    series,
                    &current_resolution,
                    DataKind::Bars,
                    CURRENT_HISTORY_SCHEMA_REVISION,
                ),
                RetainedRange {
                    start_unix_nanos,
                    end_unix_nanos,
                },
                false,
                now_seconds(),
            )
            .map_err(|error| persistence_failure(&error))
    }

    fn prepare_publications(
        &self,
        scope: &HistoryScope,
        series: &BarSeriesKey,
        bars: &[MarketBar],
        derived: bool,
        current_resolution: &str,
        publication_now: i64,
    ) -> Result<(Vec<SegmentIdentity>, Vec<SegmentIdentity>, u64), LocalHistoryError> {
        let mut identities = Vec::new();
        let mut already_stored = Vec::new();
        let mut additional_stored_bytes = 0_u64;
        for (chunk_index, chunk) in bars.chunks(MAXIMUM_LOCAL_HISTORY_BARS).enumerate() {
            let first = chunk.first().ok_or(LocalHistoryError::EmptySeries)?;
            let last = chunk.last().ok_or(LocalHistoryError::EmptySeries)?;
            let next_chunk_start = bars
                .get(
                    chunk_index
                        .saturating_add(1)
                        .saturating_mul(MAXIMUM_LOCAL_HISTORY_BARS),
                )
                .map(|bar| bar.exchange_timestamp_unix_nanos);
            let identity = SegmentIdentity {
                scope: scope.clone(),
                instrument_id: series.instrument_id.clone(),
                data_kind: if derived {
                    DataKind::Derived
                } else {
                    DataKind::Bars
                },
                resolution: current_resolution.to_string(),
                range_start_unix_nanos: first.exchange_timestamp_unix_nanos,
                range_end_unix_nanos: next_chunk_start.map_or_else(
                    || segment_range_end(series.period, last.exchange_timestamp_unix_nanos),
                    Ok,
                )?,
                source_revision: 1,
                schema_revision: CURRENT_HISTORY_SCHEMA_REVISION,
                calendar_revision: 1,
                adjustment_revision: 1,
                correction_revision: 1,
            };
            let payload = encode_local_history_segment(chunk)
                .map_err(|_| LocalHistoryError::SegmentEncode)?;
            let payload_bytes = self
                .store
                .publication_storage_requirement(PublicationRequest {
                    identity: &identity,
                    payload: &payload,
                    encryption_key: &self.segment_key,
                    retention: RetentionPolicy::UntilRevoked,
                    recovery: RecoveryAction::ProviderRefetch,
                    now_unix_seconds: publication_now,
                })
                .map_err(|error| persistence_failure(&error))?;
            if payload_bytes == 0 {
                already_stored.push(identity.clone());
            }
            additional_stored_bytes = additional_stored_bytes
                .checked_add(payload_bytes)
                .ok_or(LocalHistoryError::SegmentEncode)?;
            identities.push(identity);
        }
        Ok((identities, already_stored, additional_stored_bytes))
    }

    /// Records one exact provider-confirmed empty range.
    ///
    /// # Errors
    /// Returns a redacted error for invalid series/range or catalog failure.
    pub fn record_confirmed_empty(
        &self,
        scope: &HistoryScope,
        series: &BarSeriesKey,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    ) -> Result<(), LocalHistoryError> {
        validate_scope_series(scope, series)?;
        self.store
            .record_confirmed_empty(
                series_identity(
                    scope,
                    series,
                    &resolution(series)?,
                    DataKind::Bars,
                    CURRENT_HISTORY_SCHEMA_REVISION,
                ),
                axiusflow_local_storage::RetainedRange {
                    start_unix_nanos,
                    end_unix_nanos,
                },
                now_seconds(),
            )
            .map_err(redacted)
    }

    /// Returns durable provider-confirmed empty ranges for one exact series.
    ///
    /// # Errors
    /// Returns a redacted error for invalid identity or catalog access failure.
    pub fn confirmed_empty_ranges(
        &self,
        scope: &HistoryScope,
        series: &BarSeriesKey,
    ) -> Result<Vec<RetainedRange>, LocalHistoryError> {
        validate_scope_series(scope, series)?;
        let snapshot = self
            .store
            .series_coverage_snapshot(
                series_identity(
                    scope,
                    series,
                    &resolution(series)?,
                    DataKind::Bars,
                    CURRENT_HISTORY_SCHEMA_REVISION,
                ),
                now_seconds(),
            )
            .map_err(redacted)?;
        Ok(snapshot
            .confirmed_empty_ranges()
            .iter()
            .map(|range| RetainedRange {
                start_unix_nanos: range.start_unix_nanos,
                end_unix_nanos: range.end_unix_nanos,
            })
            .collect())
    }

    /// Reads every retained segment overlapping a bounded viewport range.
    ///
    /// The storage worker uses this before scheduling provider repair, so a
    /// previously viewed range is served from the encrypted local cache.
    ///
    /// # Errors
    /// Returns a redacted error when the range, catalog, decryption key, or
    /// stored segment payload is invalid.
    pub fn read_range(
        &mut self,
        scope: &HistoryScope,
        series: &BarSeriesKey,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    ) -> Result<Option<StoredHistory>, LocalHistoryError> {
        validate_scope_series(scope, series)?;
        let requested = axiusflow_local_storage::RetainedRange {
            start_unix_nanos,
            end_unix_nanos,
        };
        if start_unix_nanos >= end_unix_nanos {
            return Err(LocalHistoryError::InvalidRange);
        }
        let identities = self.retained_identities_in_range(scope, series, requested)?;
        self.read_retained(identities, requested)
    }
}

fn segment_range_end(
    period: BarPeriod,
    last_timestamp_unix_nanos: i64,
) -> Result<i64, LocalHistoryError> {
    let end = match period {
        BarPeriod::Tick { .. } => last_timestamp_unix_nanos.checked_add(1),
        BarPeriod::Time { seconds } => i64::from(seconds)
            .checked_mul(1_000_000_000)
            .and_then(|duration| last_timestamp_unix_nanos.checked_add(duration)),
        BarPeriod::Session { days } => i64::from(days)
            .checked_mul(NANOS_PER_DAY)
            .and_then(|duration| last_timestamp_unix_nanos.checked_add(duration)),
        BarPeriod::Week { weeks } => i64::from(weeks)
            .checked_mul(7)
            .and_then(|weeks| weeks.checked_mul(NANOS_PER_DAY))
            .and_then(|duration| last_timestamp_unix_nanos.checked_add(duration)),
        BarPeriod::Month { months } => add_calendar_months(last_timestamp_unix_nanos, months),
    };
    end.ok_or(LocalHistoryError::InvalidRange)
}

fn add_calendar_months(timestamp_unix_nanos: i64, months: u32) -> Option<i64> {
    let days = timestamp_unix_nanos.div_euclid(NANOS_PER_DAY);
    let nanos_within_day = timestamp_unix_nanos.rem_euclid(NANOS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let month_index = year
        .checked_mul(12)?
        .checked_add(i64::from(month) - 1)?
        .checked_add(i64::from(months))?;
    let target_year = month_index.div_euclid(12);
    let target_month = u32::try_from(month_index.rem_euclid(12) + 1).ok()?;
    let target_day = day.min(days_in_month(target_year, target_month));
    days_from_civil(target_year, target_month, target_day)
        .checked_mul(NANOS_PER_DAY)?
        .checked_add(nanos_within_day)
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let shifted = days_since_epoch + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let month_prime = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

const fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    }
}

fn persistence_failure(error: &LocalStorageError) -> LocalHistoryError {
    match error {
        LocalStorageError::Io(_) => LocalHistoryError::FilesystemWrite,
        LocalStorageError::Sqlite(_)
        | LocalStorageError::CatalogFull { .. }
        | LocalStorageError::CacheBudgetExceeded { .. }
        | LocalStorageError::CatalogKeyMismatch
        | LocalStorageError::StoreAlreadyOpen
        | LocalStorageError::SegmentAlreadyExists
        | LocalStorageError::KeyRevocationMissing { .. }
        | LocalStorageError::SharedKeyStillReferenced { .. } => LocalHistoryError::CatalogCommit,
        LocalStorageError::Random(_)
        | LocalStorageError::AuthenticationFailed
        | LocalStorageError::SegmentKeyMismatch => LocalHistoryError::Encryption,
        LocalStorageError::InvalidConfiguration(_)
        | LocalStorageError::InvalidIdentity(_)
        | LocalStorageError::SegmentTooLarge { .. }
        | LocalStorageError::CorruptSegment(_) => LocalHistoryError::SegmentEncode,
    }
}

fn encode_local_history_segment(bars: &[MarketBar]) -> Result<Vec<u8>, LocalHistoryError> {
    if bars.is_empty() || bars.len() > MAXIMUM_LOCAL_HISTORY_BARS {
        return Err(LocalHistoryError::InvalidSegment);
    }
    let count = u32::try_from(bars.len()).map_err(|_| LocalHistoryError::InvalidSegment)?;
    let capacity = LOCAL_BAR_SEGMENT_HEADER_BYTES
        .checked_add(
            LOCAL_BAR_BYTES
                .checked_mul(bars.len())
                .ok_or(LocalHistoryError::InvalidSegment)?,
        )
        .ok_or(LocalHistoryError::InvalidSegment)?;
    let mut encoded = Vec::with_capacity(capacity);
    encoded.extend_from_slice(LOCAL_BAR_SEGMENT_MAGIC);
    encoded.extend_from_slice(&count.to_le_bytes());
    let mut previous = None;
    for bar in bars {
        validate_local_bar(*bar, previous)?;
        previous = Some(bar.source_sequence);
        encoded.extend_from_slice(&bar.source_sequence.to_le_bytes());
        for value in [
            bar.exchange_timestamp_seconds,
            bar.exchange_timestamp_unix_nanos,
            bar.open,
            bar.high,
            bar.low,
            bar.close,
            bar.volume,
        ] {
            encoded.extend_from_slice(&value.to_le_bytes());
        }
    }
    Ok(encoded)
}

fn decode_local_history_segment(encoded: &[u8]) -> Result<Vec<MarketBar>, LocalHistoryError> {
    if !encoded.starts_with(LOCAL_BAR_SEGMENT_MAGIC) {
        return decode_legacy_coinbase_segment(encoded);
    }
    let count_bytes = encoded
        .get(LOCAL_BAR_SEGMENT_MAGIC.len()..LOCAL_BAR_SEGMENT_HEADER_BYTES)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .ok_or(LocalHistoryError::InvalidSegment)?;
    let count = usize::try_from(u32::from_le_bytes(count_bytes))
        .map_err(|_| LocalHistoryError::InvalidSegment)?;
    if count == 0 || count > MAXIMUM_LOCAL_HISTORY_BARS {
        return Err(LocalHistoryError::InvalidSegment);
    }
    let expected = LOCAL_BAR_SEGMENT_HEADER_BYTES
        .checked_add(
            LOCAL_BAR_BYTES
                .checked_mul(count)
                .ok_or(LocalHistoryError::InvalidSegment)?,
        )
        .ok_or(LocalHistoryError::InvalidSegment)?;
    if encoded.len() != expected {
        return Err(LocalHistoryError::InvalidSegment);
    }
    let mut bars = Vec::with_capacity(count);
    let mut offset = LOCAL_BAR_SEGMENT_HEADER_BYTES;
    let mut previous = None;
    for _ in 0..count {
        let source_sequence = read_u64(encoded, &mut offset)?;
        let bar = MarketBar {
            source_sequence,
            exchange_timestamp_seconds: read_i64(encoded, &mut offset)?,
            exchange_timestamp_unix_nanos: read_i64(encoded, &mut offset)?,
            open: read_i64(encoded, &mut offset)?,
            high: read_i64(encoded, &mut offset)?,
            low: read_i64(encoded, &mut offset)?,
            close: read_i64(encoded, &mut offset)?,
            volume: read_i64(encoded, &mut offset)?,
        };
        validate_local_bar(bar, previous)?;
        previous = Some(source_sequence);
        bars.push(bar);
    }
    Ok(bars)
}

fn decode_legacy_coinbase_segment(encoded: &[u8]) -> Result<Vec<MarketBar>, LocalHistoryError> {
    if encoded.len() < LEGACY_COINBASE_HEADER_BYTES
        || !encoded.starts_with(LEGACY_COINBASE_SEGMENT_MAGIC)
    {
        return Err(LocalHistoryError::InvalidSegment);
    }
    let count_bytes = encoded
        .get(LEGACY_COINBASE_SEGMENT_MAGIC.len()..LEGACY_COINBASE_HEADER_BYTES)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .ok_or(LocalHistoryError::InvalidSegment)?;
    let count = usize::try_from(u32::from_le_bytes(count_bytes))
        .map_err(|_| LocalHistoryError::InvalidSegment)?;
    if count == 0 || count > MAXIMUM_LOCAL_HISTORY_BARS {
        return Err(LocalHistoryError::InvalidSegment);
    }
    let expected = LEGACY_COINBASE_HEADER_BYTES
        .checked_add(
            LEGACY_COINBASE_PAYLOAD_BYTES
                .checked_mul(count)
                .ok_or(LocalHistoryError::InvalidSegment)?,
        )
        .ok_or(LocalHistoryError::InvalidSegment)?;
    if encoded.len() != expected {
        return Err(LocalHistoryError::InvalidSegment);
    }
    let mut bars = Vec::with_capacity(count);
    let mut previous = None;
    for payload in encoded[LEGACY_COINBASE_HEADER_BYTES..]
        .as_chunks::<LEGACY_COINBASE_PAYLOAD_BYTES>()
        .0
    {
        if !payload.starts_with(LEGACY_COINBASE_PAYLOAD_MAGIC) {
            return Err(LocalHistoryError::InvalidSegment);
        }
        let mut offset = LEGACY_COINBASE_PAYLOAD_MAGIC.len();
        let source_sequence = read_u64(payload, &mut offset)?;
        let exchange_timestamp_seconds = read_i64(payload, &mut offset)?;
        let exchange_timestamp_unix_nanos = exchange_timestamp_seconds
            .checked_mul(1_000_000_000)
            .ok_or(LocalHistoryError::InvalidSegment)?;
        let bar = MarketBar {
            source_sequence,
            exchange_timestamp_seconds,
            exchange_timestamp_unix_nanos,
            open: read_i64(payload, &mut offset)?,
            high: read_i64(payload, &mut offset)?,
            low: read_i64(payload, &mut offset)?,
            close: read_i64(payload, &mut offset)?,
            volume: read_i64(payload, &mut offset)?,
        };
        validate_local_bar(bar, previous)?;
        previous = Some(source_sequence);
        bars.push(bar);
    }
    Ok(bars)
}

fn validate_local_bar(bar: MarketBar, previous: Option<u64>) -> Result<(), LocalHistoryError> {
    bar.validate()
        .map_err(|_| LocalHistoryError::InvalidSegment)?;
    if previous.is_some_and(|sequence| sequence.checked_add(1) != Some(bar.source_sequence)) {
        return Err(LocalHistoryError::InvalidSegment);
    }
    Ok(())
}

fn read_u64(encoded: &[u8], offset: &mut usize) -> Result<u64, LocalHistoryError> {
    let bytes = encoded
        .get(*offset..(*offset).saturating_add(8))
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .ok_or(LocalHistoryError::InvalidSegment)?;
    *offset = (*offset).saturating_add(8);
    Ok(u64::from_le_bytes(bytes))
}

fn read_i64(encoded: &[u8], offset: &mut usize) -> Result<i64, LocalHistoryError> {
    let bytes = encoded
        .get(*offset..(*offset).saturating_add(8))
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .ok_or(LocalHistoryError::InvalidSegment)?;
    *offset = (*offset).saturating_add(8);
    Ok(i64::from_le_bytes(bytes))
}

fn validate_scope_series(
    scope: &HistoryScope,
    series: &BarSeriesKey,
) -> Result<(), LocalHistoryError> {
    series
        .validate()
        .map_err(|_| LocalHistoryError::InvalidSeries)?;
    if scope.provider_id != series.provider_id
        || scope.entitlement_revision != series.entitlement_id
    {
        return Err(LocalHistoryError::InvalidSeries);
    }
    Ok(())
}

fn series_identity<'a>(
    scope: &'a HistoryScope,
    series: &'a BarSeriesKey,
    resolution: &'a str,
    data_kind: DataKind,
    schema_revision: u32,
) -> HistorySeriesIdentity<'a> {
    HistorySeriesIdentity {
        scope,
        instrument_id: &series.instrument_id,
        data_kind,
        resolution,
        source_revision: 1,
        schema_revision,
        calendar_revision: 1,
        adjustment_revision: 1,
        correction_revision: 1,
    }
}

fn protected_storage_ranges<'a>(
    protected_ranges: &'a [(HistoryScope, BarSeriesKey, RetainedRange)],
    resolutions: &'a [String],
) -> Vec<(HistorySeriesIdentity<'a>, RetainedRange)> {
    let mut storage_ranges = Vec::with_capacity(protected_ranges.len() * 4);
    for ((scope, series, range), resolution) in protected_ranges.iter().zip(resolutions) {
        for data_kind in [DataKind::Bars, DataKind::Derived] {
            for schema_revision in [
                LEGACY_HISTORY_SCHEMA_REVISION,
                CURRENT_HISTORY_SCHEMA_REVISION,
            ] {
                storage_ranges.push((
                    series_identity(scope, series, resolution, data_kind, schema_revision),
                    *range,
                ));
            }
        }
    }
    storage_ranges
}

fn resolution(series: &BarSeriesKey) -> Result<String, LocalHistoryError> {
    series
        .period
        .validate()
        .map_err(|_| LocalHistoryError::InvalidSeries)?;
    Ok(match series.period {
        BarPeriod::Tick { trades } => format!("{trades}t"),
        BarPeriod::Time { seconds } => format!("{seconds}s"),
        BarPeriod::Session { days } => format!("{days}d"),
        BarPeriod::Week { weeks } => format!("{weeks}w"),
        BarPeriod::Month { months } => format!("{months}mo"),
    })
}

fn load_or_create_key(
    vault: &NativeCredentialVault,
    key_id: &str,
) -> Result<[u8; 32], LocalHistoryError> {
    if let Some(mut stored) = vault.load(key_id).map_err(redacted)? {
        let result =
            <[u8; 32]>::try_from(stored.as_slice()).map_err(|_| LocalHistoryError::InvalidVaultKey);
        stored.zeroize();
        return result;
    }
    let mut generated = Zeroizing::new([0_u8; 32]);
    getrandom::fill(generated.as_mut()).map_err(redacted)?;
    vault.store(key_id, generated.as_ref()).map_err(redacted)?;
    Ok(*generated)
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(i64::MAX)
}

fn redacted<E>(_error: E) -> LocalHistoryError {
    LocalHistoryError::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf, process};

    const COINBASE_ENTITLEMENT: &str = "crypto_public_realtime";

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("test clock")
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "axiusflow_engine_history_{}_{}",
                process::id(),
                unique
            )))
        }
    }

    #[test]
    fn persistence_failures_keep_stage_without_exposing_storage_details() {
        assert_eq!(
            persistence_failure(&LocalStorageError::SegmentTooLarge {
                requested: 10,
                maximum: 9,
            }),
            LocalHistoryError::SegmentEncode
        );
        assert_eq!(
            persistence_failure(&LocalStorageError::AuthenticationFailed),
            LocalHistoryError::Encryption
        );
        assert_eq!(
            persistence_failure(&LocalStorageError::CatalogKeyMismatch),
            LocalHistoryError::CatalogCommit
        );
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn encrypted_history_is_available_after_store_restart() {
        let root = TempRoot::new();
        let series = BarSeriesKey {
            provider_id: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            entitlement_id: COINBASE_ENTITLEMENT.to_string(),
            period: BarPeriod::time(60).expect("interval"),
            definition_version: 1,
        };
        let bars = vec![MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 60,
            exchange_timestamp_unix_nanos: 60_000_000_000,
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 7,
        }];
        {
            let mut storage = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
                .expect("fixture store opens");
            storage
                .persist(&coinbase_scope(), &series, &bars, false)
                .expect("history persists");
            let mut derived = bars.clone();
            derived[0].close = 106;
            storage
                .persist(&coinbase_scope(), &series, &derived, true)
                .expect("derived history persists");
        }
        let mut reopened = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
            .expect("fixture store reopens");
        let retained = reopened
            .read_latest(&coinbase_scope(), &series)
            .expect("history reads")
            .expect("history exists");
        assert_eq!(retained.bars[0].close, 105);
        assert!(!retained.derived);
        assert!(retained.durable);
        let ranged = reopened
            .read_range(&coinbase_scope(), &series, 60_000_000_000, 120_000_000_000)
            .expect("range reads")
            .expect("range exists");
        assert_eq!(ranged.bars[0].close, 105);
        assert!(!ranged.derived);
    }

    #[test]
    fn exact_derived_series_is_used_when_native_history_is_absent() {
        let root = TempRoot::new();
        let series = BarSeriesKey {
            provider_id: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:eth:usd".to_string(),
            entitlement_id: COINBASE_ENTITLEMENT.to_string(),
            period: BarPeriod::time(60).expect("interval"),
            definition_version: 1,
        };
        let bars = vec![MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 60,
            exchange_timestamp_unix_nanos: 60_000_000_000,
            open: 100,
            high: 110,
            low: 90,
            close: 106,
            volume: 7,
        }];
        let mut storage = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
            .expect("fixture store opens");
        storage
            .persist(&coinbase_scope(), &series, &bars, true)
            .expect("derived history persists");
        let retained = storage
            .read_latest(&coinbase_scope(), &series)
            .expect("derived history reads")
            .expect("derived history exists");
        assert_eq!(retained.bars, bars);
        assert!(retained.derived);
    }

    #[test]
    fn segmented_history_reconstructs_after_restart() {
        let root = TempRoot::new();
        let series = BarSeriesKey {
            provider_id: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            entitlement_id: COINBASE_ENTITLEMENT.to_string(),
            period: BarPeriod::time(60).expect("interval"),
            definition_version: 1,
        };
        let bars = (0..MAXIMUM_LOCAL_HISTORY_BARS * 2 + 3)
            .map(|index| MarketBar {
                source_sequence: u64::try_from(index + 1).expect("sequence"),
                exchange_timestamp_seconds: i64::try_from(index).expect("timestamp") * 60,
                exchange_timestamp_unix_nanos: i64::try_from(index).expect("timestamp")
                    * 60_000_000_000,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            })
            .collect::<Vec<_>>();
        {
            let mut storage = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
                .expect("fixture store opens");
            storage
                .persist(&coinbase_scope(), &series, &bars, false)
                .expect("segmented history persists");
            assert_eq!(
                storage
                    .store
                    .statistics()
                    .expect("statistics read")
                    .active_entries,
                3
            );
        }
        let mut restarted = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
            .expect("fixture store restarts");
        assert_eq!(
            restarted
                .read_latest(&coinbase_scope(), &series)
                .expect("segmented history reads")
                .expect("segmented history exists")
                .bars,
            bars
        );
    }

    #[test]
    fn confirmed_empty_boundary_survives_restart_without_cross_series_leakage() {
        let root = TempRoot::new();
        let series = BarSeriesKey {
            provider_id: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            entitlement_id: COINBASE_ENTITLEMENT.to_string(),
            period: BarPeriod::time(60).expect("interval"),
            definition_version: 1,
        };
        {
            let storage = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
                .expect("fixture store opens");
            storage
                .record_confirmed_empty(&coinbase_scope(), &series, 60, 120)
                .expect("empty boundary persists");
        }

        let restarted = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
            .expect("fixture store restarts");
        assert_eq!(
            restarted
                .confirmed_empty_ranges(&coinbase_scope(), &series)
                .expect("empty boundary reads"),
            vec![RetainedRange {
                start_unix_nanos: 60,
                end_unix_nanos: 120,
            }]
        );
        let other_series = BarSeriesKey {
            instrument_id: "instrument:coinbase:eth:usd".to_string(),
            ..series
        };
        assert!(
            restarted
                .confirmed_empty_ranges(&coinbase_scope(), &other_series)
                .expect("other series coverage reads")
                .is_empty()
        );
    }

    #[test]
    fn calendar_and_session_segment_ends_cover_the_bar_period() {
        assert_eq!(
            segment_range_end(BarPeriod::session(3).expect("3D"), 0),
            Ok(3 * NANOS_PER_DAY)
        );
        assert_eq!(
            segment_range_end(BarPeriod::week(1).expect("1W"), 0),
            Ok(7 * NANOS_PER_DAY)
        );
        let january_31_noon =
            days_from_civil(2024, 1, 31) * NANOS_PER_DAY + 12 * 60 * 60 * 1_000_000_000;
        let february_29_noon =
            days_from_civil(2024, 2, 29) * NANOS_PER_DAY + 12 * 60 * 60 * 1_000_000_000;
        assert_eq!(
            segment_range_end(BarPeriod::month(1).expect("1M"), january_31_noon),
            Ok(february_29_noon)
        );
    }

    #[test]
    fn tiny_test_cache_budget_fails_before_publishing_above_the_hard_limit() {
        let root = TempRoot::new();
        let series = BarSeriesKey {
            provider_id: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            entitlement_id: COINBASE_ENTITLEMENT.to_string(),
            period: BarPeriod::time(60).expect("interval"),
            definition_version: 1,
        };
        let bars = vec![MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 60,
            exchange_timestamp_unix_nanos: 60_000_000_000,
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 7,
        }];
        let mut storage =
            LocalHistoryStore::open_fixture_with_cache_budget(&root.0, [7; 32], [9; 32], 1)
                .expect("fixture store opens");
        storage
            .record_confirmed_empty(&coinbase_scope(), &series, 60_000_000_000, 120_000_000_000)
            .expect("empty evidence persists");
        assert!(
            storage
                .persist(&coinbase_scope(), &series, &bars, false)
                .is_err()
        );
        assert!(
            storage
                .read_latest(&coinbase_scope(), &series)
                .expect("history reads")
                .is_none()
        );
        assert!(
            storage
                .confirmed_empty_ranges(&coinbase_scope(), &series)
                .expect("coverage reads")
                .is_empty(),
            "real provider bars retire stale empty evidence even when cache reservation fails"
        );
        assert_eq!(
            fs::read_dir(root.0.join("segments"))
                .expect("segment directory reads")
                .count(),
            0
        );
    }

    #[test]
    fn real_bars_retire_empty_evidence_and_remain_refetchable_after_eviction_and_restart() {
        let root = TempRoot::new();
        let series = BarSeriesKey {
            provider_id: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            entitlement_id: COINBASE_ENTITLEMENT.to_string(),
            period: BarPeriod::time(60).expect("interval"),
            definition_version: 1,
        };
        let first = MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 60,
            exchange_timestamp_unix_nanos: 60_000_000_000,
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 7,
        };
        let second = MarketBar {
            source_sequence: 2,
            exchange_timestamp_seconds: 120,
            exchange_timestamp_unix_nanos: 120_000_000_000,
            ..first
        };
        {
            let mut storage =
                LocalHistoryStore::open_fixture_with_cache_budget(&root.0, [7; 32], [9; 32], 200)
                    .expect("fixture store opens");
            storage
                .record_confirmed_empty(&coinbase_scope(), &series, 60_000_000_000, 120_000_000_000)
                .expect("empty evidence persists");
            storage
                .persist(&coinbase_scope(), &series, &[first], false)
                .expect("real bar persists");
            assert!(
                storage
                    .confirmed_empty_ranges(&coinbase_scope(), &series)
                    .expect("coverage reads")
                    .is_empty()
            );
            storage
                .persist(&coinbase_scope(), &series, &[second], false)
                .expect("new tail persists and evicts old history");
            assert!(
                storage
                    .read_range(&coinbase_scope(), &series, 60_000_000_000, 120_000_000_000,)
                    .expect("evicted range reads")
                    .is_none()
            );
        }
        let mut restarted =
            LocalHistoryStore::open_fixture_with_cache_budget(&root.0, [7; 32], [9; 32], 200)
                .expect("fixture store restarts");
        assert!(
            restarted
                .confirmed_empty_ranges(&coinbase_scope(), &series)
                .expect("coverage restores")
                .is_empty()
        );
        assert!(
            restarted
                .read_range(&coinbase_scope(), &series, 60_000_000_000, 120_000_000_000,)
                .expect("restarted evicted range reads")
                .is_none()
        );
    }

    #[test]
    fn local_store_retains_legacy_coinbase_schema_segments() {
        let root = TempRoot::new();
        let series = BarSeriesKey {
            provider_id: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            entitlement_id: COINBASE_ENTITLEMENT.to_string(),
            period: BarPeriod::time(60).expect("interval"),
            definition_version: 1,
        };
        let bar = MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 60,
            exchange_timestamp_unix_nanos: 60_000_000_000,
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 7,
        };
        let legacy = encode_legacy_coinbase_segment(&[bar]);
        {
            let mut storage = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
                .expect("fixture store opens");
            let identity = SegmentIdentity {
                scope: coinbase_scope(),
                instrument_id: series.instrument_id.clone(),
                data_kind: DataKind::Bars,
                resolution: resolution(&series).expect("resolution"),
                range_start_unix_nanos: bar.exchange_timestamp_unix_nanos,
                range_end_unix_nanos: bar.exchange_timestamp_unix_nanos + 60_000_000_000,
                source_revision: 1,
                schema_revision: LEGACY_HISTORY_SCHEMA_REVISION,
                calendar_revision: 1,
                adjustment_revision: 1,
                correction_revision: 1,
            };
            storage
                .store
                .publish(PublicationRequest {
                    identity: &identity,
                    payload: &legacy,
                    encryption_key: &storage.segment_key,
                    retention: RetentionPolicy::UntilRevoked,
                    recovery: RecoveryAction::ProviderRefetch,
                    now_unix_seconds: now_seconds(),
                })
                .expect("legacy segment publishes");
        }
        let mut restarted = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
            .expect("fixture store restarts");
        assert_eq!(
            restarted
                .read_latest(&coinbase_scope(), &series)
                .expect("legacy segment reads")
                .expect("legacy segment exists")
                .bars,
            vec![bar]
        );
    }

    #[test]
    fn active_range_protection_keeps_readable_legacy_segments_under_budget_pressure() {
        let root = TempRoot::new();
        let series = BarSeriesKey {
            provider_id: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            entitlement_id: COINBASE_ENTITLEMENT.to_string(),
            period: BarPeriod::time(60).expect("interval"),
            definition_version: 1,
        };
        let bar = MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 60,
            exchange_timestamp_unix_nanos: 60_000_000_000,
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 7,
        };
        let legacy_payload = encode_legacy_coinbase_segment(&[bar]);
        let legacy_identity = SegmentIdentity {
            scope: coinbase_scope(),
            instrument_id: series.instrument_id.clone(),
            data_kind: DataKind::Bars,
            resolution: resolution(&series).expect("resolution"),
            range_start_unix_nanos: 60_000_000_000,
            range_end_unix_nanos: 120_000_000_000,
            source_revision: 1,
            schema_revision: LEGACY_HISTORY_SCHEMA_REVISION,
            calendar_revision: 1,
            adjustment_revision: 1,
            correction_revision: 1,
        };
        let mut unprotected_identity = legacy_identity.clone();
        unprotected_identity.instrument_id = "instrument:coinbase:eth:usd".to_string();
        unprotected_identity.schema_revision = CURRENT_HISTORY_SCHEMA_REVISION;
        let unprotected_payload = encode_local_history_segment(&[bar]).expect("payload encodes");
        let mut storage = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
            .expect("fixture store opens");
        let legacy_bytes =
            publish_fixture_segment(&mut storage, &legacy_identity, &legacy_payload, 100);
        let current_bytes = publish_fixture_segment(
            &mut storage,
            &unprotected_identity,
            &unprotected_payload,
            101,
        );
        storage.cache_budget_bytes = legacy_bytes + current_bytes;
        let new_bar = MarketBar {
            source_sequence: 2,
            exchange_timestamp_seconds: 120,
            exchange_timestamp_unix_nanos: 120_000_000_000,
            ..bar
        };
        storage
            .persist_with_protected_series_ranges(
                &coinbase_scope(),
                &series,
                &[new_bar],
                false,
                &[(
                    coinbase_scope(),
                    series.clone(),
                    RetainedRange {
                        start_unix_nanos: 60_000_000_000,
                        end_unix_nanos: 120_000_000_000,
                    },
                )],
            )
            .expect("new segment fits by evicting unprotected history");
        assert_eq!(
            storage
                .store
                .read(
                    &legacy_identity,
                    &storage.segment_key,
                    102,
                    RecoveryAction::ProviderRefetch,
                )
                .expect("legacy segment reads"),
            HistoryRead::Hit(legacy_payload)
        );
        assert!(matches!(
            storage.store.read(
                &unprotected_identity,
                &storage.segment_key,
                102,
                RecoveryAction::ProviderRefetch,
            ),
            Ok(HistoryRead::Unavailable { .. })
        ));
    }

    #[test]
    fn local_history_does_not_duplicate_engine_derivation_ownership() {
        let root = TempRoot::new();
        let minute_series = BarSeriesKey {
            provider_id: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            entitlement_id: COINBASE_ENTITLEMENT.to_string(),
            period: BarPeriod::time(60).expect("interval"),
            definition_version: 1,
        };
        let bars = (0_i64..5)
            .map(|minute| MarketBar {
                source_sequence: u64::try_from(minute + 1).expect("sequence"),
                exchange_timestamp_seconds: minute * 60,
                exchange_timestamp_unix_nanos: minute * 60 * 1_000_000_000,
                open: 100 + minute,
                high: 110 + minute,
                low: 90 + minute,
                close: 105 + minute,
                volume: 7,
            })
            .collect::<Vec<_>>();
        {
            let mut storage = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
                .expect("fixture store opens");
            storage
                .persist(&coinbase_scope(), &minute_series, &bars, false)
                .expect("minute history persists");
        }
        let five_minute_series = BarSeriesKey {
            period: BarPeriod::time(300).expect("interval"),
            ..minute_series
        };
        let mut reopened = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
            .expect("fixture store reopens");
        assert!(
            reopened
                .read_latest(&coinbase_scope(), &five_minute_series)
                .expect("exact retained history reads")
                .is_none()
        );
    }

    #[test]
    fn every_rithmic_chart_cadence_is_encrypted_and_readable_after_restart() {
        let root = TempRoot::new();
        let periods = [
            BarPeriod::tick(100).expect("tick"),
            BarPeriod::time(60).expect("1m"),
            BarPeriod::time(180).expect("3m"),
            BarPeriod::time(300).expect("5m"),
            BarPeriod::time(900).expect("15m"),
            BarPeriod::time(1_800).expect("30m"),
            BarPeriod::time(3_600).expect("1h"),
            BarPeriod::time(7_200).expect("2h"),
            BarPeriod::time(14_400).expect("4h"),
            BarPeriod::time(28_800).expect("8h"),
            BarPeriod::time(43_200).expect("12h"),
            BarPeriod::session(1).expect("1D"),
            BarPeriod::session(3).expect("3D"),
            BarPeriod::week(1).expect("1W"),
            BarPeriod::month(1).expect("1M"),
        ];
        let bars = vec![MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 60,
            exchange_timestamp_unix_nanos: 60_123_456_789,
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 7,
        }];
        {
            let mut storage = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
                .expect("fixture store opens");
            for period in periods {
                let series = rithmic_series(period);
                storage
                    .persist(&rithmic_scope(&series), &series, &bars, false)
                    .expect("Rithmic history persists");
            }
        }
        let mut restarted = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
            .expect("fixture store restarts");
        for period in periods {
            let series = rithmic_series(period);
            let retained = restarted
                .read_latest(&rithmic_scope(&series), &series)
                .expect("Rithmic history reads")
                .expect("Rithmic history exists");
            assert_eq!(retained.bars, bars);
            assert!(!retained.derived);
            assert!(retained.durable);
        }
    }

    fn rithmic_series(period: BarPeriod) -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            period,
            definition_version: 1,
        }
    }

    fn coinbase_scope() -> HistoryScope {
        HistoryScope {
            provider_id: "coinbase".to_string(),
            account_id: "coinbase_public_market_data".to_string(),
            entitlement_revision: COINBASE_ENTITLEMENT.to_string(),
        }
    }

    fn rithmic_scope(series: &BarSeriesKey) -> HistoryScope {
        HistoryScope {
            provider_id: series.provider_id.clone(),
            account_id: "rithmic_test_market_data".to_string(),
            entitlement_revision: series.entitlement_id.clone(),
        }
    }

    fn publish_fixture_segment(
        storage: &mut LocalHistoryStore,
        identity: &SegmentIdentity,
        payload: &[u8],
        now_unix_seconds: i64,
    ) -> u64 {
        match storage
            .store
            .publish(PublicationRequest {
                identity,
                payload,
                encryption_key: &storage.segment_key,
                retention: RetentionPolicy::UntilRevoked,
                recovery: RecoveryAction::ProviderRefetch,
                now_unix_seconds,
            })
            .expect("fixture segment publishes")
        {
            axiusflow_local_storage::PublicationOutcome::Published(receipt) => receipt.stored_bytes,
            axiusflow_local_storage::PublicationOutcome::MemoryOnly { .. } => {
                panic!("fixture segment must be durable")
            }
        }
    }

    fn encode_legacy_coinbase_segment(bars: &[MarketBar]) -> Vec<u8> {
        let mut encoded = Vec::new();
        encoded.extend_from_slice(LEGACY_COINBASE_SEGMENT_MAGIC);
        encoded.extend_from_slice(
            &u32::try_from(bars.len())
                .expect("bounded test bars")
                .to_le_bytes(),
        );
        for bar in bars {
            encoded.extend_from_slice(LEGACY_COINBASE_PAYLOAD_MAGIC);
            encoded.extend_from_slice(&bar.source_sequence.to_le_bytes());
            encoded.extend_from_slice(&bar.exchange_timestamp_seconds.to_le_bytes());
            for value in [bar.open, bar.high, bar.low, bar.close, bar.volume] {
                encoded.extend_from_slice(&value.to_le_bytes());
            }
        }
        encoded
    }
}
