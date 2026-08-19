//! Provider-neutral encrypted local bar-history mechanics.

use crate::{HistoryScope, LocalHistoryError};

use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use axiusflow_local_storage::{
    CatalogKey, DataKind, HistoryRead, HistorySeriesIdentity, HistoryStore, LocalStorageError,
    PublicationRequest, RecoveryAction, RetentionPolicy, SegmentEncryptionKey, SegmentIdentity,
};
use axiusflow_market_data::{BarPeriod, BarSeriesKey, MarketBar};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use zeroize::{Zeroize, Zeroizing};

const VAULT_SERVICE: &str = "com.axiusflow.engine.history";
const CATALOG_KEY_ID: &str = "history-catalog-key-v1";
const SEGMENT_KEY_ID: &str = "coinbase-public-bars-key-v1";
const MAXIMUM_CATALOG_ENTRIES: usize = 128;
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

/// Engine-owned access to authenticated immutable local bar segments.
pub struct LocalHistoryStore {
    store: HistoryStore,
    segment_key: SegmentEncryptionKey,
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
        let store =
            HistoryStore::open(root, catalog_key, MAXIMUM_CATALOG_ENTRIES).map_err(redacted)?;
        Ok(Self { store, segment_key })
    }

    #[cfg(test)]
    fn open_fixture(
        root: &Path,
        catalog_bytes: [u8; 32],
        segment_bytes: [u8; 32],
    ) -> Result<Self, LocalHistoryError> {
        let catalog_key =
            CatalogKey::try_new(CATALOG_KEY_ID.to_string(), catalog_bytes).map_err(redacted)?;
        let segment_key = SegmentEncryptionKey::try_new(SEGMENT_KEY_ID.to_string(), segment_bytes)
            .map_err(redacted)?;
        let store =
            HistoryStore::open(root, catalog_key, MAXIMUM_CATALOG_ENTRIES).map_err(redacted)?;
        Ok(Self { store, segment_key })
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
        for data_kind in [DataKind::Derived, DataKind::Bars] {
            if let Some(bars) = self.read_kind(scope, series, data_kind)? {
                return Ok(Some(StoredHistory {
                    bars,
                    derived: data_kind == DataKind::Derived,
                    durable: true,
                }));
            }
        }
        Ok(None)
    }

    fn read_kind(
        &mut self,
        scope: &HistoryScope,
        series: &BarSeriesKey,
        data_kind: DataKind,
    ) -> Result<Option<Vec<MarketBar>>, LocalHistoryError> {
        let resolution = resolution(series)?;
        let mut identity = None;
        for schema_revision in [
            CURRENT_HISTORY_SCHEMA_REVISION,
            LEGACY_HISTORY_SCHEMA_REVISION,
        ] {
            identity = self
                .store
                .latest_identity(
                    series_identity(scope, series, &resolution, data_kind, schema_revision),
                    now_seconds(),
                )
                .map_err(redacted)?;
            if identity.is_some() {
                break;
            }
        }
        let Some(identity) = identity else {
            return Ok(None);
        };
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
            return decode_local_history_segment(&payload).map(Some);
        }
        Ok(None)
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
        validate_scope_series(scope, series)?;
        let first = bars.first().ok_or(LocalHistoryError::EmptySeries)?;
        let last = bars.last().ok_or(LocalHistoryError::EmptySeries)?;
        let start = first.exchange_timestamp_unix_nanos;
        let end = last
            .exchange_timestamp_unix_nanos
            .checked_add(series.period.duration_nanos().unwrap_or(1))
            .ok_or(LocalHistoryError::InvalidRange)?;
        let identity = SegmentIdentity {
            scope: scope.clone(),
            instrument_id: series.instrument_id.clone(),
            data_kind: if derived {
                DataKind::Derived
            } else {
                DataKind::Bars
            },
            resolution: resolution(series)?,
            range_start_unix_nanos: start,
            range_end_unix_nanos: end,
            source_revision: 1,
            schema_revision: CURRENT_HISTORY_SCHEMA_REVISION,
            calendar_revision: 1,
            adjustment_revision: 1,
            correction_revision: 1,
        };
        let payload =
            encode_local_history_segment(bars).map_err(|_| LocalHistoryError::SegmentEncode)?;
        self.store
            .publish(PublicationRequest {
                identity: &identity,
                payload: &payload,
                encryption_key: &self.segment_key,
                retention: RetentionPolicy::UntilRevoked,
                recovery: RecoveryAction::ProviderRefetch,
                now_unix_seconds: now_seconds(),
            })
            .map(|_| ())
            .map_err(|error| persistence_failure(&error))
    }
}

fn persistence_failure(error: &LocalStorageError) -> LocalHistoryError {
    match error {
        LocalStorageError::Io(_) => LocalHistoryError::FilesystemWrite,
        LocalStorageError::Sqlite(_)
        | LocalStorageError::CatalogFull { .. }
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
    for payload in
        encoded[LEGACY_COINBASE_HEADER_BYTES..].chunks_exact(LEGACY_COINBASE_PAYLOAD_BYTES)
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
        assert_eq!(retained.bars[0].close, 106);
        assert!(retained.derived);
        assert!(retained.durable);
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
