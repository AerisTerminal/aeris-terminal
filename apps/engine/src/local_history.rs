//! Engine-owned encrypted local history storage.

use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use axiusflow_coinbase_market_adapter::{
    COINBASE_PUBLIC_ACCOUNT_ID, ENTITLEMENT_CLASS, decode_history_segment, encode_history_bar,
    encode_history_segment,
};
use axiusflow_desktop_storage::{
    CatalogKey, DataKind, HistoryRead, HistoryScope, HistorySeriesIdentity, HistoryStore,
    PublicationRequest, RecoveryAction, RetentionPolicy, SegmentEncryptionKey, SegmentIdentity,
};
use axiusflow_market_data::{BarPeriod, BarSeriesKey, MarketBar};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use axiusflow_provider_history::HistoryItem;
use zeroize::{Zeroize, Zeroizing};

const VAULT_SERVICE: &str = "com.axiusflow.engine.history";
const CATALOG_KEY_ID: &str = "history-catalog-key-v1";
const SEGMENT_KEY_ID: &str = "coinbase-public-bars-key-v1";
const MAXIMUM_CATALOG_ENTRIES: usize = 128;

pub(crate) struct LocalHistoryStore {
    store: HistoryStore,
    segment_key: SegmentEncryptionKey,
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
    ) -> Result<Option<Vec<MarketBar>>, String> {
        let scope = history_scope();
        let resolution = resolution(series)?;
        let identity = self
            .store
            .latest_identity(series_identity(&scope, series, &resolution), now_seconds())
            .map_err(redacted)?;
        let Some(identity) = identity else {
            return Ok(None);
        };
        match self
            .store
            .read(
                &identity,
                &self.segment_key,
                now_seconds(),
                RecoveryAction::ProviderRefetch,
            )
            .map_err(redacted)?
        {
            HistoryRead::Hit(payload) => decode_history_segment(&payload)
                .map(|values| Some(values.into_iter().map(|value| value.value).collect()))
                .map_err(redacted),
            HistoryRead::Unavailable { .. } => Ok(None),
        }
    }

    pub(crate) fn persist(
        &mut self,
        series: &BarSeriesKey,
        bars: &[MarketBar],
    ) -> Result<(), String> {
        let first = bars
            .first()
            .ok_or_else(|| "local history cannot persist an empty series".to_string())?;
        let last = bars
            .last()
            .ok_or_else(|| "local history cannot persist an empty series".to_string())?;
        let interval_seconds = interval_seconds(series)?;
        let start = first
            .exchange_timestamp_seconds
            .checked_mul(1_000_000_000)
            .ok_or_else(|| "local history range overflowed".to_string())?;
        let end = last
            .exchange_timestamp_seconds
            .checked_add(i64::from(interval_seconds))
            .and_then(|seconds| seconds.checked_mul(1_000_000_000))
            .ok_or_else(|| "local history range overflowed".to_string())?;
        let identity = SegmentIdentity {
            scope: history_scope(),
            instrument_id: series.instrument_id.clone(),
            data_kind: DataKind::Bars,
            resolution: resolution(series)?,
            range_start_unix_nanos: start,
            range_end_unix_nanos: end,
            source_revision: 1,
            schema_revision: 1,
            calendar_revision: 1,
            adjustment_revision: 1,
            correction_revision: 1,
        };
        let items = bars
            .iter()
            .copied()
            .map(|bar| HistoryItem {
                sequence: bar.source_sequence,
                event_time_unix_nanos: bar.exchange_timestamp_seconds.saturating_mul(1_000_000_000),
                payload: encode_history_bar(bar),
            })
            .collect::<Vec<_>>();
        let payload = encode_history_segment(&items).map_err(redacted)?;
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

fn history_scope() -> HistoryScope {
    HistoryScope {
        provider_id: "coinbase".to_string(),
        account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
        entitlement_revision: ENTITLEMENT_CLASS.to_string(),
    }
}

fn series_identity<'a>(
    scope: &'a HistoryScope,
    series: &'a BarSeriesKey,
    resolution: &'a str,
) -> HistorySeriesIdentity<'a> {
    HistorySeriesIdentity {
        scope,
        instrument_id: &series.instrument_id,
        data_kind: DataKind::Bars,
        resolution,
        source_revision: 1,
        schema_revision: 1,
        calendar_revision: 1,
        adjustment_revision: 1,
        correction_revision: 1,
    }
}

fn resolution(series: &BarSeriesKey) -> Result<String, String> {
    interval_seconds(series).map(|seconds| format!("{seconds}s"))
}

fn interval_seconds(series: &BarSeriesKey) -> Result<u32, String> {
    match series.period {
        BarPeriod::Time { seconds } if seconds > 0 => Ok(seconds),
        BarPeriod::Time { .. } | BarPeriod::Tick { .. } | BarPeriod::Daily => {
            Err("local history supports fixed-time bars only".to_string())
        }
    }
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
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 7,
        }];
        {
            let mut storage = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
                .expect("fixture store opens");
            storage.persist(&series, &bars).expect("history persists");
        }
        let mut reopened = LocalHistoryStore::open_fixture(&root.0, [7; 32], [9; 32])
            .expect("fixture store reopens");
        assert_eq!(
            reopened.read_latest(&series).expect("history reads"),
            Some(bars)
        );
    }
}
