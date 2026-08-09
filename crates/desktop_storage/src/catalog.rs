use crate::{
    DesktopStorageError,
    model::{CatalogStatistics, RecoveryAction},
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{collections::BTreeSet, path::Path};

const CATALOG_SCHEMA_VERSION: i64 = 2;
const LEGACY_CATALOG_SCHEMA_VERSION: i64 = 1;
const EXPECTED_SQLITE_VERSION: &str = "3.53.2";
const ACTIVE_STATE: i64 = 0;
const QUARANTINED_STATE: i64 = 1;
pub(crate) const CONFIRMED_EMPTY_CLASS: i64 = 2;
pub(crate) const INVALIDATED_CLASS: i64 = 4;

pub(crate) struct Catalog {
    connection: Connection,
    maximum_entries: usize,
}

pub(crate) struct CatalogRecord {
    pub segment_id: [u8; 32],
    pub file_name: String,
    pub file_checksum: [u8; 32],
    pub payload_checksum: [u8; 32],
    pub payload_bytes: u64,
    pub key_id: String,
    pub key_verifier: [u8; 32],
    pub retention_until: Option<i64>,
    pub recovery: RecoveryAction,
    pub quarantined: bool,
    pub quarantine_file_name: Option<String>,
}

pub(crate) struct NewCatalogRecord<'a> {
    pub segment_id: &'a [u8; 32],
    pub provider_token: &'a [u8; 32],
    pub account_token: &'a [u8; 32],
    pub entitlement_token: &'a [u8; 32],
    pub instrument_token: &'a [u8; 32],
    pub data_kind: u8,
    pub resolution_token: &'a [u8; 32],
    pub range_start: i64,
    pub range_end: i64,
    pub source_revision: u32,
    pub schema_revision: u32,
    pub calendar_revision: u32,
    pub adjustment_revision: u32,
    pub correction_revision: u64,
    pub file_name: &'a str,
    pub file_checksum: &'a [u8; 32],
    pub payload_checksum: &'a [u8; 32],
    pub payload_bytes: u64,
    pub key_id: &'a str,
    pub key_verifier: &'a [u8; 32],
    pub retention_until: Option<i64>,
    pub recovery: RecoveryAction,
    pub created_at: i64,
}

#[derive(Clone, Copy)]
pub(crate) enum CatalogFilter<'a> {
    Scope {
        provider: &'a [u8; 32],
        account: &'a [u8; 32],
        entitlement: &'a [u8; 32],
    },
    Account {
        provider: &'a [u8; 32],
        account: &'a [u8; 32],
    },
    Schema {
        provider: &'a [u8; 32],
        account: &'a [u8; 32],
        entitlement: &'a [u8; 32],
        current: u32,
    },
    Source {
        provider: &'a [u8; 32],
        account: &'a [u8; 32],
        entitlement: &'a [u8; 32],
        instrument: &'a [u8; 32],
        current: u32,
    },
    Calendar {
        provider: &'a [u8; 32],
        account: &'a [u8; 32],
        entitlement: &'a [u8; 32],
        instrument: &'a [u8; 32],
        current: u32,
    },
    Adjustment {
        provider: &'a [u8; 32],
        account: &'a [u8; 32],
        entitlement: &'a [u8; 32],
        instrument: &'a [u8; 32],
        current: u32,
    },
    Correction {
        provider: &'a [u8; 32],
        account: &'a [u8; 32],
        entitlement: &'a [u8; 32],
        instrument: &'a [u8; 32],
        current: u64,
    },
}

impl Catalog {
    pub fn open(
        path: &Path,
        catalog_key_id: &str,
        catalog_key_verifier: &[u8; 32],
        maximum_entries: usize,
    ) -> Result<Self, DesktopStorageError> {
        if rusqlite::version() != EXPECTED_SQLITE_VERSION {
            return Err(DesktopStorageError::InvalidConfiguration(
                "bundled SQLite version drifted",
            ));
        }
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=2500;
             PRAGMA secure_delete=ON;
             CREATE TABLE IF NOT EXISTS catalog_metadata(
                 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
                 schema_version INTEGER NOT NULL,
                 catalog_key_id TEXT NOT NULL,
                 catalog_key_verifier BLOB NOT NULL CHECK(length(catalog_key_verifier)=32)
             ) STRICT;
             CREATE TABLE IF NOT EXISTS history_segment(
                 segment_id BLOB PRIMARY KEY CHECK(length(segment_id)=32),
                 provider_token BLOB NOT NULL CHECK(length(provider_token)=32),
                 account_token BLOB NOT NULL CHECK(length(account_token)=32),
                 entitlement_token BLOB NOT NULL CHECK(length(entitlement_token)=32),
                 instrument_token BLOB NOT NULL CHECK(length(instrument_token)=32),
                 data_kind INTEGER NOT NULL,
                 resolution_token BLOB NOT NULL CHECK(length(resolution_token)=32),
                 range_start INTEGER NOT NULL,
                 range_end INTEGER NOT NULL CHECK(range_end > range_start),
                 source_revision INTEGER NOT NULL CHECK(source_revision > 0),
                 schema_revision INTEGER NOT NULL CHECK(schema_revision > 0),
                 calendar_revision INTEGER NOT NULL CHECK(calendar_revision > 0),
                 adjustment_revision INTEGER NOT NULL CHECK(adjustment_revision > 0),
                 correction_revision INTEGER NOT NULL CHECK(correction_revision > 0),
                 file_name TEXT NOT NULL UNIQUE,
                 file_checksum BLOB NOT NULL CHECK(length(file_checksum)=32),
                 payload_checksum BLOB NOT NULL CHECK(length(payload_checksum)=32),
                 payload_bytes INTEGER NOT NULL CHECK(payload_bytes >= 0),
                 key_id TEXT NOT NULL,
                 key_verifier BLOB NOT NULL CHECK(length(key_verifier)=32),
                 retention_until INTEGER,
                 recovery_action INTEGER NOT NULL CHECK(recovery_action IN (1,2)),
                 state INTEGER NOT NULL CHECK(state IN (0,1)),
                 quarantine_reason TEXT,
                 quarantine_file_name TEXT,
                 created_at INTEGER NOT NULL
             ) STRICT;
             CREATE INDEX IF NOT EXISTS history_segment_scope
                 ON history_segment(provider_token, account_token, entitlement_token, state);
             CREATE INDEX IF NOT EXISTS history_segment_dimension
                 ON history_segment(instrument_token, data_kind, resolution_token, state);
             CREATE TABLE IF NOT EXISTS history_coverage_marker(
                 provider_token BLOB NOT NULL CHECK(length(provider_token)=32),
                 account_token BLOB NOT NULL CHECK(length(account_token)=32),
                 entitlement_token BLOB NOT NULL CHECK(length(entitlement_token)=32),
                 instrument_token BLOB NOT NULL CHECK(length(instrument_token)=32),
                 data_kind INTEGER NOT NULL,
                 resolution_token BLOB NOT NULL CHECK(length(resolution_token)=32),
                 range_start INTEGER NOT NULL,
                 range_end INTEGER NOT NULL CHECK(range_end > range_start),
                 source_revision INTEGER NOT NULL CHECK(source_revision > 0),
                 schema_revision INTEGER NOT NULL CHECK(schema_revision > 0),
                 calendar_revision INTEGER NOT NULL CHECK(calendar_revision > 0),
                 adjustment_revision INTEGER NOT NULL CHECK(adjustment_revision > 0),
                 correction_revision INTEGER NOT NULL CHECK(correction_revision > 0),
                 class INTEGER NOT NULL CHECK(class IN (2,4)),
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY(
                     provider_token, account_token, entitlement_token, instrument_token,
                     data_kind, resolution_token, range_start, range_end, source_revision,
                     schema_revision, calendar_revision, adjustment_revision,
                     correction_revision, class
                 )
             ) STRICT;
             CREATE INDEX IF NOT EXISTS history_coverage_marker_series
                 ON history_coverage_marker(
                     provider_token, account_token, entitlement_token, instrument_token,
                     data_kind, resolution_token
                 );",
        )?;
        initialize_metadata(&connection, catalog_key_id, catalog_key_verifier)?;
        let existing_count: i64 =
            connection.query_row("SELECT count(*) FROM history_segment", [], |row| row.get(0))?;
        let coverage_count: i64 =
            connection.query_row("SELECT count(*) FROM history_coverage_marker", [], |row| {
                row.get(0)
            })?;
        if usize::try_from(existing_count.saturating_add(coverage_count)).unwrap_or(usize::MAX)
            > maximum_entries
        {
            return Err(DesktopStorageError::InvalidConfiguration(
                "existing catalog exceeds the configured entry bound",
            ));
        }
        Ok(Self {
            connection,
            maximum_entries,
        })
    }

    pub fn find(
        &self,
        segment_id: &[u8; 32],
    ) -> Result<Option<CatalogRecord>, DesktopStorageError> {
        let record = self
            .connection
            .query_row(
                "SELECT segment_id, file_name, file_checksum, payload_checksum,
                        payload_bytes, key_id, key_verifier, retention_until, recovery_action,
                        state, quarantine_file_name
                 FROM history_segment WHERE segment_id=?1",
                [segment_id.as_slice()],
                map_record,
            )
            .optional()?;
        Ok(record)
    }

    pub fn insert(&self, record: &NewCatalogRecord<'_>) -> Result<(), DesktopStorageError> {
        if self
            .total_count()?
            .saturating_add(self.coverage_marker_count()?)
            >= self.maximum_entries
        {
            return Err(DesktopStorageError::CatalogFull {
                maximum: self.maximum_entries,
            });
        }
        self.connection.execute(
            "INSERT INTO history_segment(
                 segment_id, provider_token, account_token, entitlement_token,
                 instrument_token, data_kind, resolution_token, range_start, range_end,
                 source_revision, schema_revision, calendar_revision, adjustment_revision,
                 correction_revision,
                 file_name, file_checksum, payload_checksum, payload_bytes, key_id, key_verifier,
                 retention_until, recovery_action, state, quarantine_reason,
                 quarantine_file_name, created_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, 0, NULL, NULL, ?23
             )",
            params![
                record.segment_id.as_slice(),
                record.provider_token.as_slice(),
                record.account_token.as_slice(),
                record.entitlement_token.as_slice(),
                record.instrument_token.as_slice(),
                record.data_kind,
                record.resolution_token.as_slice(),
                record.range_start,
                record.range_end,
                record.source_revision,
                record.schema_revision,
                record.calendar_revision,
                record.adjustment_revision,
                i64::try_from(record.correction_revision)
                    .map_err(|_| { DesktopStorageError::InvalidIdentity("correction_revision") })?,
                record.file_name,
                record.file_checksum.as_slice(),
                record.payload_checksum.as_slice(),
                i64::try_from(record.payload_bytes)
                    .map_err(|_| { DesktopStorageError::InvalidIdentity("payload_bytes") })?,
                record.key_id,
                record.key_verifier.as_slice(),
                record.retention_until,
                record.recovery.code(),
                record.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn replace_quarantined(
        &self,
        record: &NewCatalogRecord<'_>,
    ) -> Result<(), DesktopStorageError> {
        let updated = self.connection.execute(
            "UPDATE history_segment SET
                 provider_token=?2, account_token=?3, entitlement_token=?4,
                 instrument_token=?5, data_kind=?6, resolution_token=?7,
                 range_start=?8, range_end=?9, source_revision=?10, schema_revision=?11,
                 calendar_revision=?12, adjustment_revision=?13, correction_revision=?14,
                 file_name=?15, file_checksum=?16, payload_checksum=?17,
                 payload_bytes=?18, key_id=?19, key_verifier=?20,
                 retention_until=?21, recovery_action=?22, state=0,
                 quarantine_reason=NULL, quarantine_file_name=NULL, created_at=?23
             WHERE segment_id=?1 AND state=1",
            params![
                record.segment_id.as_slice(),
                record.provider_token.as_slice(),
                record.account_token.as_slice(),
                record.entitlement_token.as_slice(),
                record.instrument_token.as_slice(),
                record.data_kind,
                record.resolution_token.as_slice(),
                record.range_start,
                record.range_end,
                record.source_revision,
                record.schema_revision,
                record.calendar_revision,
                record.adjustment_revision,
                i64::try_from(record.correction_revision)
                    .map_err(|_| DesktopStorageError::InvalidIdentity("correction_revision"))?,
                record.file_name,
                record.file_checksum.as_slice(),
                record.payload_checksum.as_slice(),
                i64::try_from(record.payload_bytes)
                    .map_err(|_| DesktopStorageError::InvalidIdentity("payload_bytes"))?,
                record.key_id,
                record.key_verifier.as_slice(),
                record.retention_until,
                record.recovery.code(),
                record.created_at,
            ],
        )?;
        if updated != 1 {
            return Err(DesktopStorageError::SegmentAlreadyExists);
        }
        Ok(())
    }

    pub fn mark_quarantined(
        &self,
        segment_id: &[u8; 32],
        reason: &str,
        quarantine_file_name: Option<&str>,
    ) -> Result<(), DesktopStorageError> {
        self.connection.execute(
            "UPDATE history_segment
             SET state=?2, quarantine_reason=?3, quarantine_file_name=?4
             WHERE segment_id=?1",
            params![
                segment_id.as_slice(),
                QUARANTINED_STATE,
                reason,
                quarantine_file_name
            ],
        )?;
        Ok(())
    }

    pub fn matching(
        &self,
        filter: CatalogFilter<'_>,
    ) -> Result<Vec<CatalogRecord>, DesktopStorageError> {
        match filter {
            CatalogFilter::Scope {
                provider,
                account,
                entitlement,
            } => self.query_records(
                "provider_token=?1 AND account_token=?2 AND entitlement_token=?3",
                params![provider.as_slice(), account.as_slice(), entitlement.as_slice()],
            ),
            CatalogFilter::Account { provider, account } => self.query_records(
                "provider_token=?1 AND account_token=?2",
                params![provider.as_slice(), account.as_slice()],
            ),
            CatalogFilter::Schema {
                provider,
                account,
                entitlement,
                current,
            } => self.query_records(
                "provider_token=?1 AND account_token=?2 AND entitlement_token=?3 AND schema_revision<>?4",
                params![provider.as_slice(), account.as_slice(), entitlement.as_slice(), current],
            ),
            CatalogFilter::Source {
                provider,
                account,
                entitlement,
                instrument,
                current,
            } => self.query_records(
                "provider_token=?1 AND account_token=?2 AND entitlement_token=?3 AND instrument_token=?4 AND source_revision<>?5",
                params![provider.as_slice(), account.as_slice(), entitlement.as_slice(), instrument.as_slice(), current],
            ),
            CatalogFilter::Calendar {
                provider,
                account,
                entitlement,
                instrument,
                current,
            } => self.query_records(
                "provider_token=?1 AND account_token=?2 AND entitlement_token=?3 AND instrument_token=?4 AND calendar_revision<>?5",
                params![provider.as_slice(), account.as_slice(), entitlement.as_slice(), instrument.as_slice(), current],
            ),
            CatalogFilter::Adjustment {
                provider,
                account,
                entitlement,
                instrument,
                current,
            } => self.query_records(
                "provider_token=?1 AND account_token=?2 AND entitlement_token=?3 AND instrument_token=?4 AND adjustment_revision<>?5",
                params![provider.as_slice(), account.as_slice(), entitlement.as_slice(), instrument.as_slice(), current],
            ),
            CatalogFilter::Correction {
                provider,
                account,
                entitlement,
                instrument,
                current,
            } => self.query_records(
                "provider_token=?1 AND account_token=?2 AND entitlement_token=?3 AND instrument_token=?4 AND correction_revision<>?5",
                params![provider.as_slice(), account.as_slice(), entitlement.as_slice(), instrument.as_slice(), i64::try_from(current).map_err(|_| DesktopStorageError::InvalidIdentity("correction_revision"))?],
            ),
        }
    }

    pub fn latest_series_range(
        &self,
        tokens: SeriesTokens<'_>,
        dimensions: SeriesDimensions,
        now_unix_seconds: i64,
    ) -> Result<Option<(i64, i64)>, DesktopStorageError> {
        self.connection
            .query_row(
                "SELECT range_start, range_end FROM history_segment
                 WHERE provider_token=?1 AND account_token=?2 AND entitlement_token=?3
                   AND instrument_token=?4 AND data_kind=?5 AND resolution_token=?6
                   AND source_revision=?7 AND schema_revision=?8
                   AND calendar_revision=?9 AND adjustment_revision=?10
                   AND correction_revision=?11 AND state=0
                   AND (retention_until IS NULL OR retention_until>?12)
                 ORDER BY range_end DESC, created_at DESC LIMIT 1",
                params![
                    tokens.provider.as_slice(),
                    tokens.account.as_slice(),
                    tokens.entitlement.as_slice(),
                    tokens.instrument.as_slice(),
                    dimensions.data_kind,
                    tokens.resolution.as_slice(),
                    dimensions.source_revision,
                    dimensions.schema_revision,
                    dimensions.calendar_revision,
                    dimensions.adjustment_revision,
                    i64::try_from(dimensions.correction_revision).map_err(|_| {
                        DesktopStorageError::InvalidIdentity("correction_revision")
                    })?,
                    now_unix_seconds,
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn active_series_ranges(
        &self,
        tokens: SeriesTokens<'_>,
        dimensions: SeriesDimensions,
        now_unix_seconds: i64,
    ) -> Result<Vec<(i64, i64)>, DesktopStorageError> {
        self.segment_series_ranges(tokens, dimensions, now_unix_seconds, ACTIVE_STATE, false)
    }

    pub fn quarantined_series_ranges(
        &self,
        tokens: SeriesTokens<'_>,
        dimensions: SeriesDimensions,
        now_unix_seconds: i64,
    ) -> Result<Vec<(i64, i64)>, DesktopStorageError> {
        self.segment_series_ranges(
            tokens,
            dimensions,
            now_unix_seconds,
            QUARANTINED_STATE,
            true,
        )
    }

    fn segment_series_ranges(
        &self,
        tokens: SeriesTokens<'_>,
        dimensions: SeriesDimensions,
        now_unix_seconds: i64,
        state: i64,
        include_expired: bool,
    ) -> Result<Vec<(i64, i64)>, DesktopStorageError> {
        let mut statement = self.connection.prepare(
            "SELECT range_start, range_end FROM history_segment
             WHERE provider_token=?1 AND account_token=?2 AND entitlement_token=?3
               AND instrument_token=?4 AND data_kind=?5 AND resolution_token=?6
               AND source_revision=?7 AND schema_revision=?8
               AND calendar_revision=?9 AND adjustment_revision=?10
               AND correction_revision=?11 AND state=?12
               AND (?13 OR retention_until IS NULL OR retention_until>?14)
             ORDER BY range_start ASC, range_end ASC",
        )?;
        let rows = statement.query_map(
            params![
                tokens.provider.as_slice(),
                tokens.account.as_slice(),
                tokens.entitlement.as_slice(),
                tokens.instrument.as_slice(),
                dimensions.data_kind,
                tokens.resolution.as_slice(),
                dimensions.source_revision,
                dimensions.schema_revision,
                dimensions.calendar_revision,
                dimensions.adjustment_revision,
                i64::try_from(dimensions.correction_revision)
                    .map_err(|_| DesktopStorageError::InvalidIdentity("correction_revision"))?,
                state,
                include_expired,
                now_unix_seconds,
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn coverage_marker_ranges(
        &self,
        tokens: SeriesTokens<'_>,
        dimensions: SeriesDimensions,
        class: i64,
    ) -> Result<Vec<(i64, i64)>, DesktopStorageError> {
        let mut statement = self.connection.prepare(
            "SELECT range_start, range_end FROM history_coverage_marker
             WHERE provider_token=?1 AND account_token=?2 AND entitlement_token=?3
               AND instrument_token=?4 AND data_kind=?5 AND resolution_token=?6
               AND source_revision=?7 AND schema_revision=?8
               AND calendar_revision=?9 AND adjustment_revision=?10
               AND correction_revision=?11 AND class=?12
             ORDER BY range_start ASC, range_end ASC",
        )?;
        let rows = statement.query_map(
            params![
                tokens.provider.as_slice(),
                tokens.account.as_slice(),
                tokens.entitlement.as_slice(),
                tokens.instrument.as_slice(),
                dimensions.data_kind,
                tokens.resolution.as_slice(),
                dimensions.source_revision,
                dimensions.schema_revision,
                dimensions.calendar_revision,
                dimensions.adjustment_revision,
                i64::try_from(dimensions.correction_revision)
                    .map_err(|_| DesktopStorageError::InvalidIdentity("correction_revision"))?,
                class,
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn insert_coverage_marker(
        &self,
        tokens: SeriesTokens<'_>,
        dimensions: SeriesDimensions,
        range: (i64, i64),
        class: i64,
        created_at: i64,
    ) -> Result<(), DesktopStorageError> {
        let correction_revision = i64::try_from(dimensions.correction_revision)
            .map_err(|_| DesktopStorageError::InvalidIdentity("correction_revision"))?;
        let exists: bool = self.connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM history_coverage_marker
                 WHERE provider_token=?1 AND account_token=?2 AND entitlement_token=?3
                   AND instrument_token=?4 AND data_kind=?5 AND resolution_token=?6
                   AND range_start=?7 AND range_end=?8 AND source_revision=?9
                   AND schema_revision=?10 AND calendar_revision=?11
                   AND adjustment_revision=?12 AND correction_revision=?13 AND class=?14
             )",
            params![
                tokens.provider.as_slice(),
                tokens.account.as_slice(),
                tokens.entitlement.as_slice(),
                tokens.instrument.as_slice(),
                dimensions.data_kind,
                tokens.resolution.as_slice(),
                range.0,
                range.1,
                dimensions.source_revision,
                dimensions.schema_revision,
                dimensions.calendar_revision,
                dimensions.adjustment_revision,
                correction_revision,
                class,
            ],
            |row| row.get(0),
        )?;
        if exists {
            return Ok(());
        }
        if self
            .total_count()?
            .saturating_add(self.coverage_marker_count()?)
            >= self.maximum_entries
        {
            return Err(DesktopStorageError::CatalogFull {
                maximum: self.maximum_entries,
            });
        }
        self.connection.execute(
            "INSERT INTO history_coverage_marker(
                 provider_token, account_token, entitlement_token, instrument_token,
                 data_kind, resolution_token, range_start, range_end, source_revision,
                 schema_revision, calendar_revision, adjustment_revision,
                 correction_revision, class, created_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                tokens.provider.as_slice(),
                tokens.account.as_slice(),
                tokens.entitlement.as_slice(),
                tokens.instrument.as_slice(),
                dimensions.data_kind,
                tokens.resolution.as_slice(),
                range.0,
                range.1,
                dimensions.source_revision,
                dimensions.schema_revision,
                dimensions.calendar_revision,
                dimensions.adjustment_revision,
                correction_revision,
                class,
                created_at,
            ],
        )?;
        Ok(())
    }

    pub fn remove_records(&mut self, records: &[CatalogRecord]) -> Result<(), DesktopStorageError> {
        let transaction = self.connection.transaction()?;
        for record in records {
            transaction.execute(
                "DELETE FROM history_segment WHERE segment_id=?1",
                [record.segment_id.as_slice()],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn active_records(&self) -> Result<Vec<CatalogRecord>, DesktopStorageError> {
        self.query_records("state=0", params![])
    }

    pub fn all_records(&self) -> Result<Vec<CatalogRecord>, DesktopStorageError> {
        self.query_records("1=1", params![])
    }

    pub fn key_material_reference_count(
        &self,
        key_verifier: &[u8; 32],
    ) -> Result<usize, DesktopStorageError> {
        let count: i64 = self.connection.query_row(
            "SELECT count(*) FROM history_segment WHERE key_verifier=?1",
            [key_verifier.as_slice()],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    pub fn key_id_has_different_verifier(
        &self,
        key_id: &str,
        key_verifier: &[u8; 32],
    ) -> Result<bool, DesktopStorageError> {
        let count: i64 = self.connection.query_row(
            "SELECT count(*) FROM history_segment
             WHERE key_id=?1 AND key_verifier<>?2",
            params![key_id, key_verifier.as_slice()],
            |row| row.get(0),
        )?;
        Ok(count != 0)
    }

    pub fn active_file_names(&self) -> Result<BTreeSet<String>, DesktopStorageError> {
        let mut statement = self
            .connection
            .prepare("SELECT file_name FROM history_segment WHERE state=0")?;
        let names = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<BTreeSet<String>, _>>()?;
        Ok(names)
    }

    pub fn quarantine_file_names(&self) -> Result<BTreeSet<String>, DesktopStorageError> {
        let mut statement = self.connection.prepare(
            "SELECT quarantine_file_name FROM history_segment
             WHERE state=1 AND quarantine_file_name IS NOT NULL",
        )?;
        let names = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<BTreeSet<String>, _>>()?;
        Ok(names)
    }

    pub fn statistics(&self) -> Result<CatalogStatistics, DesktopStorageError> {
        let active: i64 = self.connection.query_row(
            "SELECT count(*) FROM history_segment WHERE state=?1",
            [ACTIVE_STATE],
            |row| row.get(0),
        )?;
        let quarantined: i64 = self.connection.query_row(
            "SELECT count(*) FROM history_segment WHERE state=?1",
            [QUARANTINED_STATE],
            |row| row.get(0),
        )?;
        let coverage: i64 = self.connection.query_row(
            "SELECT count(*) FROM history_coverage_marker",
            [],
            |row| row.get(0),
        )?;
        Ok(CatalogStatistics {
            active_entries: usize::try_from(active).unwrap_or(usize::MAX),
            quarantined_entries: usize::try_from(quarantined).unwrap_or(usize::MAX),
            coverage_entries: usize::try_from(coverage).unwrap_or(usize::MAX),
            maximum_entries: self.maximum_entries,
        })
    }

    pub fn checkpoint_after_deletion(&self) -> Result<(), DesktopStorageError> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    fn total_count(&self) -> Result<usize, DesktopStorageError> {
        let count: i64 =
            self.connection
                .query_row("SELECT count(*) FROM history_segment", [], |row| row.get(0))?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    fn coverage_marker_count(&self) -> Result<usize, DesktopStorageError> {
        let count: i64 = self.connection.query_row(
            "SELECT count(*) FROM history_coverage_marker",
            [],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    fn query_records<P>(
        &self,
        predicate: &str,
        parameters: P,
    ) -> Result<Vec<CatalogRecord>, DesktopStorageError>
    where
        P: rusqlite::Params,
    {
        let sql = format!(
            "SELECT segment_id, file_name, file_checksum, payload_checksum,
                    payload_bytes, key_id, key_verifier, retention_until, recovery_action,
                    state, quarantine_file_name
             FROM history_segment WHERE {predicate}"
        );
        let mut statement = self.connection.prepare(&sql)?;
        Ok(statement
            .query_map(parameters, map_record)?
            .collect::<Result<Vec<_>, _>>()?)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SeriesTokens<'a> {
    pub provider: &'a [u8; 32],
    pub account: &'a [u8; 32],
    pub entitlement: &'a [u8; 32],
    pub instrument: &'a [u8; 32],
    pub resolution: &'a [u8; 32],
}

#[derive(Clone, Copy)]
pub(crate) struct SeriesDimensions {
    pub data_kind: u8,
    pub source_revision: u32,
    pub schema_revision: u32,
    pub calendar_revision: u32,
    pub adjustment_revision: u32,
    pub correction_revision: u64,
}

fn initialize_metadata(
    connection: &Connection,
    catalog_key_id: &str,
    catalog_key_verifier: &[u8; 32],
) -> Result<(), DesktopStorageError> {
    let existing = connection
        .query_row(
            "SELECT schema_version, catalog_key_id, catalog_key_verifier
             FROM catalog_metadata WHERE singleton=1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    digest_column(row, 2)?,
                ))
            },
        )
        .optional()?;
    match existing {
        None => {
            connection.execute(
                "INSERT INTO catalog_metadata(
                     singleton, schema_version, catalog_key_id, catalog_key_verifier
                 ) VALUES (1, ?1, ?2, ?3)",
                params![
                    CATALOG_SCHEMA_VERSION,
                    catalog_key_id,
                    catalog_key_verifier.as_slice()
                ],
            )?;
        }
        Some((CATALOG_SCHEMA_VERSION, existing_key, existing_verifier))
            if existing_key == catalog_key_id && existing_verifier == *catalog_key_verifier => {}
        Some((LEGACY_CATALOG_SCHEMA_VERSION, existing_key, existing_verifier))
            if existing_key == catalog_key_id && existing_verifier == *catalog_key_verifier =>
        {
            connection.execute(
                "UPDATE catalog_metadata SET schema_version=?1 WHERE singleton=1",
                [CATALOG_SCHEMA_VERSION],
            )?;
        }
        Some((CATALOG_SCHEMA_VERSION, _, _)) => {
            return Err(DesktopStorageError::CatalogKeyMismatch);
        }
        Some(_) => {
            return Err(DesktopStorageError::InvalidConfiguration(
                "unsupported catalog schema version",
            ));
        }
    }
    Ok(())
}

fn map_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<CatalogRecord> {
    let payload_bytes: i64 = row.get(4)?;
    let recovery: i64 = row.get(8)?;
    let state: i64 = row.get(9)?;
    if !matches!(state, ACTIVE_STATE | QUARANTINED_STATE) {
        return Err(conversion_error(9));
    }
    Ok(CatalogRecord {
        segment_id: digest_column(row, 0)?,
        file_name: row.get(1)?,
        file_checksum: digest_column(row, 2)?,
        payload_checksum: digest_column(row, 3)?,
        payload_bytes: u64::try_from(payload_bytes).map_err(|_| conversion_error(4))?,
        key_id: row.get(5)?,
        key_verifier: digest_column(row, 6)?,
        retention_until: row.get(7)?,
        recovery: RecoveryAction::from_code(recovery).map_err(|_| conversion_error(8))?,
        quarantined: state == QUARANTINED_STATE,
        quarantine_file_name: row.get(10)?,
    })
}

fn digest_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<[u8; 32]> {
    row.get::<_, Vec<u8>>(index)?
        .try_into()
        .map_err(|_| conversion_error(index))
}

fn conversion_error(index: usize) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Blob,
        "invalid desktop history catalog value".into(),
    )
}
