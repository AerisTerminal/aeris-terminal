//! Engine-owned encrypted local history storage.

use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use axiusflow_coinbase_market_adapter::{
    COINBASE_PUBLIC_ACCOUNT_ID, CoinbaseInterval, ENTITLEMENT_CLASS, aggregate_coinbase_bars,
    decode_history_segment,
};
use axiusflow_desktop_storage::{
    CatalogKey, DataKind, HistoryRead, HistoryScope, HistorySeriesIdentity, HistoryStore,
    PublicationRequest, RecoveryAction, RetentionPolicy, SegmentEncryptionKey, SegmentIdentity,
};
use axiusflow_market_data::{BarPeriod, BarSeriesKey, MarketBar};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use axiusflow_rithmic_protocol_adapter::RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID;
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

pub(crate) struct LocalHistoryStore {
    store: HistoryStore,
    segment_key: SegmentEncryptionKey,
}

pub(crate) struct StoredHistory {
    pub(crate) bars: Vec<MarketBar>,
    pub(crate) derived: bool,
    pub(crate) durable: bool,
}

impl LocalHistoryStore {
    pub(crate) fn open(root: &Path) -> Result<Self, String> {
        let parent = root
            .parent()
            .ok_or_else(|| "engine local history root has no parent".to_string())?;
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
    pub(crate) fn open_fixture(
        root: &Path,
        catalog_bytes: [u8; 32],
        segment_bytes: [u8; 32],
    ) -> Result<Self, String> {
        let catalog_key =
            CatalogKey::try_new(CATALOG_KEY_ID.to_string(), catalog_bytes).map_err(redacted)?;
        let segment_key = SegmentEncryptionKey::try_new(SEGMENT_KEY_ID.to_string(), segment_bytes)
            .map_err(redacted)?;
        let store =
            HistoryStore::open(root, catalog_key, MAXIMUM_CATALOG_ENTRIES).map_err(redacted)?;
        Ok(Self { store, segment_key })
    }

    pub(crate) fn read_latest(
        &mut self,
        series: &BarSeriesKey,
    ) -> Result<Option<StoredHistory>, String> {
        for data_kind in [DataKind::Derived, DataKind::Bars] {
            if let Some(bars) = self.read_kind(series, data_kind)? {
                return Ok(Some(StoredHistory {
                    bars,
                    derived: data_kind == DataKind::Derived,
                    durable: true,
                }));
            }
        }
        let Some(interval) = derived_interval(series)? else {
            return Ok(None);
        };
        let source_series = BarSeriesKey {
            period: BarPeriod::time(60).map_err(|error| error.to_string())?,
            ..series.clone()
        };
        let Some(source_bars) = self.read_kind(&source_series, DataKind::Bars)? else {
            return Ok(None);
        };
        let (bars, _) = aggregate_coinbase_bars(&source_bars, interval).map_err(redacted)?;
        if bars.is_empty() {
            return Ok(None);
        }
        let durable = self.persist(series, &bars, true).is_ok();
        Ok(Some(StoredHistory {
            bars,
            derived: true,
            durable,
        }))
    }

    fn read_kind(
        &mut self,
        series: &BarSeriesKey,
        data_kind: DataKind,
    ) -> Result<Option<Vec<MarketBar>>, String> {
        let scope = history_scope(series)?;
        let resolution = resolution(series)?;
        let mut identity = None;
        for schema_revision in [
            CURRENT_HISTORY_SCHEMA_REVISION,
            LEGACY_HISTORY_SCHEMA_REVISION,
        ] {
            identity = self
                .store
                .latest_identity(
                    series_identity(&scope, series, &resolution, data_kind, schema_revision),
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
            return decode_local_history_segment(&payload)
                .map(Some)
                .map_err(redacted);
        }
        Ok(None)
    }

    pub(crate) fn persist(
        &mut self,
        series: &BarSeriesKey,
        bars: &[MarketBar],
        derived: bool,
    ) -> Result<(), String> {
        let first = bars
            .first()
            .ok_or_else(|| "local history cannot persist an empty series".to_string())?;
        let last = bars
            .last()
            .ok_or_else(|| "local history cannot persist an empty series".to_string())?;
        let start = first.exchange_timestamp_unix_nanos;
        let end = last
            .exchange_timestamp_unix_nanos
            .checked_add(series.period.duration_nanos().unwrap_or(1))
            .ok_or_else(|| "local history range overflowed".to_string())?;
        let identity = SegmentIdentity {
            scope: history_scope(series)?,
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
        let payload = encode_local_history_segment(bars).map_err(redacted)?;
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
            .map_err(redacted)
    }
}

fn encode_local_history_segment(bars: &[MarketBar]) -> Result<Vec<u8>, String> {
    if bars.is_empty() || bars.len() > MAXIMUM_LOCAL_HISTORY_BARS {
        return Err("local history segment item count is invalid".to_string());
    }
    let count = u32::try_from(bars.len())
        .map_err(|_| "local history segment item count overflow".to_string())?;
    let capacity = LOCAL_BAR_SEGMENT_HEADER_BYTES
        .checked_add(
            LOCAL_BAR_BYTES
                .checked_mul(bars.len())
                .ok_or_else(|| "local history segment size overflow".to_string())?,
        )
        .ok_or_else(|| "local history segment size overflow".to_string())?;
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

fn decode_local_history_segment(encoded: &[u8]) -> Result<Vec<MarketBar>, String> {
    if !encoded.starts_with(LOCAL_BAR_SEGMENT_MAGIC) {
        return decode_history_segment(encoded)
            .map(|values| values.into_iter().map(|value| value.value).collect());
    }
    let count_bytes = encoded
        .get(LOCAL_BAR_SEGMENT_MAGIC.len()..LOCAL_BAR_SEGMENT_HEADER_BYTES)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .ok_or_else(|| "local history segment header is truncated".to_string())?;
    let count = usize::try_from(u32::from_le_bytes(count_bytes))
        .map_err(|_| "local history segment item count overflow".to_string())?;
    if count == 0 || count > MAXIMUM_LOCAL_HISTORY_BARS {
        return Err("local history segment item count is invalid".to_string());
    }
    let expected = LOCAL_BAR_SEGMENT_HEADER_BYTES
        .checked_add(
            LOCAL_BAR_BYTES
                .checked_mul(count)
                .ok_or_else(|| "local history segment size overflow".to_string())?,
        )
        .ok_or_else(|| "local history segment size overflow".to_string())?;
    if encoded.len() != expected {
        return Err("local history segment size is invalid".to_string());
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

fn validate_local_bar(bar: MarketBar, previous: Option<u64>) -> Result<(), String> {
    bar.validate().map_err(|error| error.to_string())?;
    if previous.is_some_and(|sequence| sequence.checked_add(1) != Some(bar.source_sequence)) {
        return Err("local history segment is not contiguous".to_string());
    }
    Ok(())
}

fn read_u64(encoded: &[u8], offset: &mut usize) -> Result<u64, String> {
    let bytes = encoded
        .get(*offset..(*offset).saturating_add(8))
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .ok_or_else(|| "local history segment is truncated".to_string())?;
    *offset = (*offset).saturating_add(8);
    Ok(u64::from_le_bytes(bytes))
}

fn read_i64(encoded: &[u8], offset: &mut usize) -> Result<i64, String> {
    let bytes = encoded
        .get(*offset..(*offset).saturating_add(8))
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .ok_or_else(|| "local history segment is truncated".to_string())?;
    *offset = (*offset).saturating_add(8);
    Ok(i64::from_le_bytes(bytes))
}

fn history_scope(series: &BarSeriesKey) -> Result<HistoryScope, String> {
    let account_id = match series.provider_id.as_str() {
        "coinbase" if series.entitlement_id == ENTITLEMENT_CLASS => COINBASE_PUBLIC_ACCOUNT_ID,
        "rithmic" => RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID,
        _ => return Err("local history provider scope is unsupported".to_string()),
    };
    Ok(HistoryScope {
        provider_id: series.provider_id.clone(),
        account_id: account_id.to_string(),
        entitlement_revision: series.entitlement_id.clone(),
    })
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

fn resolution(series: &BarSeriesKey) -> Result<String, String> {
    series.period.validate().map_err(redacted)?;
    Ok(match series.period {
        BarPeriod::Tick { trades } => format!("{trades}t"),
        BarPeriod::Time { seconds } => format!("{seconds}s"),
        BarPeriod::Session { days } => format!("{days}d"),
        BarPeriod::Week { weeks } => format!("{weeks}w"),
        BarPeriod::Month { months } => format!("{months}mo"),
    })
}

fn interval_seconds(series: &BarSeriesKey) -> Result<u32, String> {
    match series.period {
        BarPeriod::Time { seconds } if seconds > 0 => Ok(seconds),
        BarPeriod::Time { .. }
        | BarPeriod::Tick { .. }
        | BarPeriod::Session { .. }
        | BarPeriod::Week { .. }
        | BarPeriod::Month { .. } => Err("local history supports fixed-time bars only".to_string()),
    }
}

fn derived_interval(series: &BarSeriesKey) -> Result<Option<CoinbaseInterval>, String> {
    if series.provider_id != "coinbase" {
        return Ok(None);
    }
    interval_seconds(series).map(|seconds| match seconds {
        300 => Some(CoinbaseInterval::Minute5),
        900 => Some(CoinbaseInterval::Minute15),
        3_600 => Some(CoinbaseInterval::Hour1),
        _ => None,
    })
}

fn load_or_create_key(vault: &NativeCredentialVault, key_id: &str) -> Result<[u8; 32], String> {
    if let Some(mut stored) = vault.load(key_id).map_err(redacted)? {
        let result = <[u8; 32]>::try_from(stored.as_slice())
            .map_err(|_| "engine history vault key has an invalid length".to_string());
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

fn redacted<E>(_error: E) -> String {
    "engine local history is unavailable".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf, process};

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
            entitlement_id: ENTITLEMENT_CLASS.to_string(),
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
                .persist(&series, &bars, false)
                .expect("history persists");
            let mut derived = bars.clone();
            derived[0].close = 106;
            storage
                .persist(&series, &derived, true)
                .expect("derived history persists");
        }
        let mut reopened = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
            .expect("fixture store reopens");
        let retained = reopened
            .read_latest(&series)
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
            entitlement_id: ENTITLEMENT_CLASS.to_string(),
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
        let item = axiusflow_provider_history::HistoryItem {
            sequence: bar.source_sequence,
            event_time_unix_nanos: bar.exchange_timestamp_unix_nanos,
            payload: axiusflow_coinbase_market_adapter::encode_history_bar(bar),
        };
        let legacy = axiusflow_coinbase_market_adapter::encode_history_segment(&[item])
            .expect("legacy segment encodes");
        {
            let mut storage = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
                .expect("fixture store opens");
            let identity = SegmentIdentity {
                scope: history_scope(&series).expect("scope"),
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
                .read_latest(&series)
                .expect("legacy segment reads")
                .expect("legacy segment exists")
                .bars,
            vec![bar]
        );
    }

    #[test]
    fn cold_store_derives_and_retains_a_coarser_series_from_native_minutes() {
        let root = TempRoot::new();
        let minute_series = BarSeriesKey {
            provider_id: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            entitlement_id: ENTITLEMENT_CLASS.to_string(),
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
                .persist(&minute_series, &bars, false)
                .expect("minute history persists");
        }
        let five_minute_series = BarSeriesKey {
            period: BarPeriod::time(300).expect("interval"),
            ..minute_series
        };
        let mut reopened = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
            .expect("fixture store reopens");
        let derived = reopened
            .read_latest(&five_minute_series)
            .expect("history derives")
            .expect("derived history exists");
        assert!(derived.derived);
        assert!(derived.durable);
        assert_eq!(derived.bars.len(), 1);
        assert_eq!(derived.bars[0].exchange_timestamp_seconds, 0);
        assert_eq!(derived.bars[0].close, 109);
        drop(reopened);

        let mut restarted = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
            .expect("fixture store restarts");
        let retained = restarted
            .read_latest(&five_minute_series)
            .expect("derived cache reads")
            .expect("derived cache exists");
        assert!(retained.derived);
        assert!(retained.durable);
        assert_eq!(retained.bars, derived.bars);
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
                storage
                    .persist(&rithmic_series(period), &bars, false)
                    .expect("Rithmic history persists");
            }
        }
        let mut restarted = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
            .expect("fixture store restarts");
        for period in periods {
            let retained = restarted
                .read_latest(&rithmic_series(period))
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
}
