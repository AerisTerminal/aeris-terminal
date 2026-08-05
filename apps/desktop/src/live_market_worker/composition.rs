use super::{
    CATALOG_KEY_ID, MODEL_ITEM_CAPACITY, PROVIDER_EVENT_CAPACITY, SEGMENT_KEY_ID, SUBSCRIPTION_ID,
    VAULT_SERVICE,
};
use crate::market_worker::MarketWorkerStartup;
use axiusflow_application::MarketBarClientModel;
use axiusflow_coinbase_market_adapter::{CoinbaseBarAggregatorConfig, CoinbaseConfig};
use axiusflow_desktop_history::HistoryWorkerConfig;
use axiusflow_desktop_provider_runtime::{
    CoinbaseProviderDriver, CoinbaseProviderEvents, DesktopMarketWorker, DesktopMarketWorkerConfig,
    DesktopProviderConfig,
};
use axiusflow_desktop_storage::{CatalogKey, SegmentEncryptionKey};
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_market_data::{BarDefinition, MarketBar};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use std::{
    num::NonZeroUsize,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
    },
    thread::ThreadId,
    time::{SystemTime, UNIX_EPOCH},
};
use zeroize::{Zeroize, Zeroizing};

use super::lifecycle::WorkerInboxEvent;

pub(super) struct ProductProfile {
    pub(super) product_id: String,
    pub(super) instrument_id: String,
    pub(super) symbol: String,
}

pub(super) type CoinbaseDesktopWorker =
    DesktopMarketWorker<MarketBar, NativeCredentialVault, CoinbaseProviderDriver>;

pub(super) struct OpenedWorker {
    pub(super) worker: CoinbaseDesktopWorker,
    pub(super) events: CoinbaseProviderEvents,
    pub(super) segment_key: SegmentEncryptionKey,
}

pub(super) fn open_worker(
    profile: &ProductProfile,
    history_root: PathBuf,
    ui_thread: ThreadId,
    inbox_tx: &SyncSender<WorkerInboxEvent>,
    provider_wake_pending: &Arc<AtomicBool>,
) -> Result<OpenedWorker, String> {
    let vault = NativeCredentialVault::new(VAULT_SERVICE).map_err(|error| error.to_string())?;
    let catalog_key = load_catalog_key(&vault)?;
    let segment_key = load_segment_key(&vault)?;
    let runtime_vault =
        NativeCredentialVault::new(VAULT_SERVICE).map_err(|error| error.to_string())?;
    let provider_config = CoinbaseConfig::try_new(vec![profile.product_id.clone()])
        .map_err(|error| error.to_string())?;
    let provider_inbox_tx = inbox_tx.clone();
    let wake_pending = Arc::clone(provider_wake_pending);
    let wake = Arc::new(move || {
        if wake_pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && provider_inbox_tx
                .try_send(WorkerInboxEvent::ProviderReady)
                .is_err()
        {
            wake_pending.store(false, Ordering::Release);
        }
    });
    let (driver, events) = CoinbaseProviderDriver::new_with_wake(
        provider_config,
        nonzero(PROVIDER_EVENT_CAPACITY),
        wake,
    );
    let mut worker = DesktopMarketWorker::try_open(
        runtime_vault,
        driver,
        "coinbase-public-session",
        history_root,
        catalog_key,
        ui_thread,
        worker_config(),
    )
    .map_err(|error| error.to_string())?;
    worker
        .register_coinbase_bar_product(
            CoinbaseBarAggregatorConfig::try_new(
                profile.product_id.clone(),
                2,
                8,
                nonzero(MODEL_ITEM_CAPACITY),
            )
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    Ok(OpenedWorker {
        worker,
        events,
        segment_key,
    })
}

pub(super) fn worker_label(monitors_active: bool) -> String {
    if monitors_active {
        "Coinbase direct · native lifecycle monitored"
    } else {
        "Coinbase direct · native lifecycle monitor unavailable"
    }
    .to_string()
}

pub(super) fn loading_startup(profile: &ProductProfile) -> Result<MarketWorkerStartup, String> {
    Ok(MarketWorkerStartup::Loading {
        instrument: instrument(profile)?,
        subscription_id: SUBSCRIPTION_ID.to_string(),
        worker_label: "Coinbase direct · loading local provider history".to_string(),
    })
}

pub(super) fn product_profile(product_id: String) -> Result<ProductProfile, String> {
    let (base, instrument_id) = match product_id.as_str() {
        "BTC-USD" => ("BTC", "instrument:coinbase:btc:usd"),
        "ETH-USD" => ("ETH", "instrument:coinbase:eth:usd"),
        _ => return Err("Coinbase desktop mode supports only BTC-USD and ETH-USD".to_string()),
    };
    Ok(ProductProfile {
        product_id,
        instrument_id: instrument_id.to_string(),
        symbol: format!("{base}/USD"),
    })
}

pub(super) fn instrument(profile: &ProductProfile) -> Result<InstrumentRevision, String> {
    Ok(InstrumentRevision {
        instrument_id: InstrumentId::try_new(profile.instrument_id.clone())
            .map_err(|error| error.to_string())?,
        revision: 1,
        asset_class: AssetClass::CryptoAsset,
        symbol: profile.symbol.clone(),
        venue_id: "COINBASE".to_string(),
        trading_currency: "USD".to_string(),
        precision: InstrumentPrecision::try_new(2, 8).map_err(|error| error.to_string())?,
        lifecycle: InstrumentLifecycle::Active,
    })
}

pub(super) fn bar_definition() -> BarDefinition {
    BarDefinition {
        definition_id: "coinbase:spot:one_minute:unadjusted:v1".to_string(),
        version: 1,
        interval_seconds: 60,
    }
}

fn worker_config() -> DesktopMarketWorkerConfig {
    DesktopMarketWorkerConfig {
        provider: DesktopProviderConfig::new(nonzero(32), nonzero(1)),
        history: HistoryWorkerConfig {
            maximum_cache_entries: nonzero(2),
            maximum_decoded_bytes: nonzero(2 * 1024 * 1024),
            maximum_charts: nonzero(1),
            maximum_segment_read_bytes: nonzero(1024 * 1024),
            maximum_buffered_live: nonzero(512),
            maximum_handoffs: nonzero(1),
        },
        maximum_catalog_entries: 64,
    }
}

pub(super) fn client_model() -> MarketBarClientModel {
    MarketBarClientModel::new(nonzero(MODEL_ITEM_CAPACITY))
}

fn load_catalog_key(vault: &NativeCredentialVault) -> Result<CatalogKey, String> {
    let bytes = load_or_create_key(vault, CATALOG_KEY_ID)?;
    CatalogKey::try_new(CATALOG_KEY_ID.to_string(), bytes).map_err(|error| error.to_string())
}

fn load_segment_key(vault: &NativeCredentialVault) -> Result<SegmentEncryptionKey, String> {
    let bytes = load_or_create_key(vault, SEGMENT_KEY_ID)?;
    SegmentEncryptionKey::try_new(SEGMENT_KEY_ID.to_string(), bytes)
        .map_err(|error| error.to_string())
}

fn load_or_create_key(vault: &NativeCredentialVault, key_id: &str) -> Result<[u8; 32], String> {
    if let Some(mut stored) = vault.load(key_id).map_err(|error| error.to_string())? {
        let result = <[u8; 32]>::try_from(stored.as_slice())
            .map_err(|_| "operating-system vault key has an invalid length".to_string());
        stored.zeroize();
        return result;
    }
    let mut generated = Zeroizing::new([0_u8; 32]);
    getrandom::fill(generated.as_mut()).map_err(|error| error.to_string())?;
    vault
        .store(key_id, generated.as_ref())
        .map_err(|error| error.to_string())?;
    let mut stored = vault
        .load(key_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "operating-system vault did not retain the generated key".to_string())?;
    let result = <[u8; 32]>::try_from(stored.as_slice())
        .map_err(|_| "operating-system vault key has an invalid length".to_string())?;
    stored.zeroize();
    Ok(result)
}

pub(super) fn unix_nanos() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos()
        .try_into()
        .map_err(|_| "system time exceeds the supported range".to_string())
}

pub(super) const fn nonzero(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}

#[cfg(test)]
mod tests {
    use super::{SUBSCRIPTION_ID, loading_startup, nonzero, product_profile};
    use crate::market_worker::MarketWorkerStartup;
    use axiusflow_coinbase_market_adapter::{
        CanonicalTrade, CoinbaseBarAggregator, CoinbaseBarAggregatorConfig, FixedPointValue,
    };
    use axiusflow_market_data::MarketBar;

    #[test]
    fn live_mode_accepts_only_reviewed_coinbase_precision_profiles() {
        assert!(product_profile("BTC-USD".to_string()).is_ok());
        assert!(product_profile("ETH-USD".to_string()).is_ok());
        assert!(product_profile("SOL-USD".to_string()).is_err());
    }

    #[test]
    fn live_startup_is_loading_metadata_without_synthetic_market_data() {
        let profile = product_profile("BTC-USD".to_string()).expect("profile validates");
        let MarketWorkerStartup::Loading {
            instrument,
            subscription_id,
            worker_label,
        } = loading_startup(&profile).expect("loading startup validates")
        else {
            panic!("live startup must wait for a provider snapshot");
        };
        assert_eq!(instrument.instrument_id.as_str(), profile.instrument_id);
        assert_eq!(subscription_id, SUBSCRIPTION_ID);
        assert!(worker_label.contains("loading"));
    }

    #[test]
    fn reviewed_products_handoff_completed_history_to_live_without_gaps() {
        for product_id in ["BTC-USD", "ETH-USD"] {
            assert_history_live_handoff(product_id);
        }
    }

    fn assert_history_live_handoff(product_id: &str) {
        let profile = product_profile(product_id.to_string()).expect("profile validates");
        let config = CoinbaseBarAggregatorConfig::try_new(product_id.to_string(), 2, 8, nonzero(4))
            .expect("aggregation config validates");
        let mut aggregator = CoinbaseBarAggregator::new(config);
        assert_eq!(aggregator.instrument_id(), profile.instrument_id);
        assert_eq!(
            aggregator
                .seed_completed_history(&[bar(100, 10_000), bar(101, 10_100)])
                .expect("completed history seeds"),
            2
        );
        assert!(
            aggregator
                .apply_trade_with_evidence(&trade(product_id, 101, 10))
                .expect("overlapping live trade is classified")
                .is_none()
        );
        assert!(
            aggregator
                .apply_trade_with_evidence(&trade(product_id, 102, 11))
                .expect("following live minute opens")
                .is_none()
        );
        let completed = aggregator
            .apply_trade_with_evidence(&trade(product_id, 103, 12))
            .expect("next minute rolls")
            .expect("live minute completes");

        assert_eq!(completed.bar.source_sequence, 3);
        assert_eq!(completed.bar.exchange_timestamp_seconds, 102 * 60);
        assert_eq!(completed.provider_sequence_num, Some(11));
        let history = aggregator.history();
        assert_eq!(
            history
                .iter()
                .map(|bar| (bar.source_sequence, bar.exchange_timestamp_seconds))
                .collect::<Vec<_>>(),
            vec![(1, 100 * 60), (2, 101 * 60), (3, 102 * 60)]
        );
    }

    fn bar(minute: i64, price: i64) -> MarketBar {
        MarketBar {
            source_sequence: u64::try_from(minute).expect("fixture minute is positive"),
            exchange_timestamp_seconds: minute * 60,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: 100_000_000,
        }
    }

    fn trade(product_id: &str, minute: i64, sequence_num: u64) -> CanonicalTrade {
        CanonicalTrade {
            product_id: product_id.to_string(),
            trade_id: format!("{product_id}-{sequence_num}"),
            price: FixedPointValue::parse("102.00").expect("fixture price parses"),
            size: FixedPointValue::parse("0.5").expect("fixture size parses"),
            maker_side_buy: true,
            trade_time_unix_nanos: minute * 60_000_000_000 + 1_000_000_000,
            provider_timestamp_unix_nanos: minute * 60_000_000_000 + 500_000_000,
            sequence_num,
        }
    }
}
