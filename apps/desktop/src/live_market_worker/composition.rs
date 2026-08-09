use super::{
    CATALOG_KEY_ID, MODEL_ITEM_CAPACITY, PROVIDER_EVENT_CAPACITY, SEGMENT_KEY_ID, SUBSCRIPTION_ID,
    VAULT_SERVICE,
};
use crate::market_worker::{CoinbaseWorkerStartup, MarketWorkerStartup};
use axiusflow_application::MarketBarClientModel;
use axiusflow_coinbase_market_adapter::{
    CoinbaseBarAggregator, CoinbaseBarAggregatorConfig, CoinbaseConfig, CoinbaseDesktopEventError,
    CoinbaseDesktopMarketEvent, CoinbaseProviderDriver, CoinbaseProviderEvents,
    CoinbaseSpotProduct, seed_coinbase_bar_history, try_recv_coinbase_market_event,
};
use axiusflow_desktop_history::HistoryWorkerConfig;
use axiusflow_desktop_provider_runtime::{
    DesktopMarketWorker, DesktopMarketWorkerConfig, DesktopMarketWorkerError,
    DesktopProviderConfig, ProviderEnvironment, SessionGeneration,
};
use axiusflow_desktop_storage::{CatalogKey, SegmentEncryptionKey, SegmentIdentity};
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_market_data::{BarDefinition, ChartAggregation, ChartInterval, MarketBar};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use std::{
    num::{NonZeroU64, NonZeroUsize},
    ops::{Deref, DerefMut},
    path::Path,
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

#[derive(Clone)]
pub(super) struct ProductProfile {
    pub(super) product_id: String,
    pub(super) instrument_id: String,
    pub(super) symbol: String,
    pub(super) price_scale: u8,
    pub(super) quantity_scale: u8,
    pub(super) base_currency: String,
    pub(super) quote_currency: String,
    pub(super) interval: ChartInterval,
}

pub(super) struct CoinbaseDesktopWorker<V: CredentialVault = NativeCredentialVault> {
    runtime: DesktopMarketWorker<MarketBar, V, CoinbaseProviderDriver>,
    aggregator: CoinbaseBarAggregator,
}

impl<V: CredentialVault> CoinbaseDesktopWorker<V> {
    pub(super) fn try_recv_coinbase_market_event(
        &mut self,
        events: &CoinbaseProviderEvents,
    ) -> Result<Option<CoinbaseDesktopMarketEvent>, CoinbaseDesktopEventError> {
        try_recv_coinbase_market_event(
            &mut self.runtime,
            events,
            std::slice::from_mut(&mut self.aggregator),
        )
    }

    pub(super) fn coinbase_bar_history(&self, product_id: &str) -> Result<Vec<MarketBar>, String> {
        if product_id != self.aggregator.product_id() {
            return Err("Coinbase bar product is not registered".to_string());
        }
        Ok(self.aggregator.history())
    }

    pub(super) fn seed_coinbase_bar_history(
        &mut self,
        generation: SessionGeneration,
        product_id: &str,
        identity: &SegmentIdentity,
        encryption_key: &SegmentEncryptionKey,
        now_unix_seconds: i64,
    ) -> Result<usize, String> {
        seed_coinbase_bar_history(
            &mut self.runtime,
            &mut self.aggregator,
            generation,
            product_id,
            identity,
            encryption_key,
            now_unix_seconds,
        )
        .map_err(|error| error.to_string())
    }

    pub(super) fn reset_aggregation(&mut self) {
        self.aggregator.reset();
    }
}

impl<V: CredentialVault> Deref for CoinbaseDesktopWorker<V> {
    type Target = DesktopMarketWorker<MarketBar, V, CoinbaseProviderDriver>;

    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

impl<V: CredentialVault> DerefMut for CoinbaseDesktopWorker<V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.runtime
    }
}

pub(super) struct OpenedWorker<V: CredentialVault = NativeCredentialVault> {
    pub(super) worker: CoinbaseDesktopWorker<V>,
    pub(super) events: CoinbaseProviderEvents,
    pub(super) segment_key: SegmentEncryptionKey,
}

const STORE_LOCK_ATTEMPTS: u32 = 100;
const STORE_LOCK_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// A replaced worker releases the encrypted store lock as its shutdown
/// completes; wait briefly on the calling worker thread, never the UI.
fn open_with_store_lock_retry<T>(
    mut open: impl FnMut() -> Result<T, DesktopMarketWorkerError>,
    maximum_retries: u32,
    retry_interval: std::time::Duration,
) -> Result<T, DesktopMarketWorkerError> {
    let mut attempt = 0_u32;
    loop {
        match open() {
            Err(DesktopMarketWorkerError::HistoryStoreAlreadyOpen) if attempt < maximum_retries => {
                attempt = attempt.saturating_add(1);
                std::thread::sleep(retry_interval);
            }
            result => return result,
        }
    }
}

pub(super) fn open_worker(
    profile: &ProductProfile,
    history_root: &Path,
    ui_thread: ThreadId,
    inbox_tx: &SyncSender<WorkerInboxEvent>,
    provider_wake_pending: &Arc<AtomicBool>,
    detailed_diagnostics: bool,
) -> Result<OpenedWorker, String> {
    let vault = NativeCredentialVault::new(VAULT_SERVICE).map_err(|error| error.to_string())?;
    let segment_key = load_segment_key(&vault)?;
    let provider_config = CoinbaseConfig::try_new(vec![profile.product_id.clone()])
        .map_err(|error| error.to_string())?;
    let provider_inbox_tx = inbox_tx.clone();
    let wake_pending = Arc::clone(provider_wake_pending);
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
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
    let (runtime, events) = open_with_store_lock_retry(
        || {
            let runtime_vault = NativeCredentialVault::new(VAULT_SERVICE)
                .map_err(|_| DesktopMarketWorkerError::HistoryConfiguration)?;
            let catalog_key = load_catalog_key(&runtime_vault)
                .map_err(|_| DesktopMarketWorkerError::HistoryConfiguration)?;
            let config = worker_config(detailed_diagnostics)
                .map_err(|_| DesktopMarketWorkerError::HistoryConfiguration)?;
            let (driver, events) = CoinbaseProviderDriver::new_with_wake(
                provider_config.clone(),
                nonzero(PROVIDER_EVENT_CAPACITY),
                Arc::clone(&wake),
            );
            DesktopMarketWorker::try_open(
                runtime_vault,
                driver,
                "coinbase-public-session",
                history_root,
                catalog_key,
                ui_thread,
                config,
            )
            .map(|runtime| (runtime, events))
        },
        STORE_LOCK_ATTEMPTS,
        STORE_LOCK_RETRY_INTERVAL,
    )
    .map_err(|error| error.to_string())?;
    let aggregator = CoinbaseBarAggregator::new(
        CoinbaseBarAggregatorConfig::try_new(
            profile.product_id.clone(),
            profile.price_scale,
            profile.quantity_scale,
            nonzero(MODEL_ITEM_CAPACITY),
        )
        .map_err(|error| error.to_string())?,
    );
    let worker = CoinbaseDesktopWorker {
        runtime,
        aggregator,
    };
    Ok(OpenedWorker {
        worker,
        events,
        segment_key,
    })
}

#[cfg(test)]
pub(super) fn open_test_worker<V: CredentialVault>(
    profile: &ProductProfile,
    history_root: std::path::PathBuf,
    ui_thread: ThreadId,
    vault: V,
    driver: CoinbaseProviderDriver,
    catalog_key: CatalogKey,
) -> Result<CoinbaseDesktopWorker<V>, String> {
    let runtime = DesktopMarketWorker::try_open(
        vault,
        driver,
        "coinbase-public-session",
        history_root,
        catalog_key,
        ui_thread,
        worker_config(false)?,
    )
    .map_err(|error| error.to_string())?;
    let aggregator = CoinbaseBarAggregator::new(
        CoinbaseBarAggregatorConfig::try_new(
            profile.product_id.clone(),
            profile.price_scale,
            profile.quantity_scale,
            nonzero(MODEL_ITEM_CAPACITY),
        )
        .map_err(|error| error.to_string())?,
    );
    Ok(CoinbaseDesktopWorker {
        runtime,
        aggregator,
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

pub(super) fn loading_startup(profile: &ProductProfile) -> MarketWorkerStartup {
    MarketWorkerStartup::Loading(Box::new(CoinbaseWorkerStartup {
        coinbase_product: spot_product(profile),
        subscription_id: SUBSCRIPTION_ID.to_string(),
        worker_label: "Coinbase direct · loading local provider history".to_string(),
    }))
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
        price_scale: 2,
        quantity_scale: 8,
        base_currency: base.to_string(),
        quote_currency: "USD".to_string(),
        interval: ChartInterval::Minute1,
    })
}

pub(super) fn product_profile_from_spot(
    product: CoinbaseSpotProduct,
    interval: ChartInterval,
) -> ProductProfile {
    ProductProfile {
        product_id: product.product_id,
        instrument_id: product.instrument_id,
        symbol: product.display_symbol,
        price_scale: product.price_scale,
        quantity_scale: product.quantity_scale,
        base_currency: product.base_currency,
        quote_currency: product.quote_currency,
        interval,
    }
}

fn spot_product(profile: &ProductProfile) -> CoinbaseSpotProduct {
    CoinbaseSpotProduct {
        product_id: profile.product_id.clone(),
        instrument_id: profile.instrument_id.clone(),
        display_symbol: profile.symbol.clone(),
        base_currency: profile.base_currency.clone(),
        quote_currency: profile.quote_currency.clone(),
        price_scale: profile.price_scale,
        quantity_scale: profile.quantity_scale,
    }
}

pub(super) fn instrument(profile: &ProductProfile) -> Result<InstrumentRevision, String> {
    Ok(InstrumentRevision {
        instrument_id: InstrumentId::try_new(profile.instrument_id.clone())
            .map_err(|error| error.to_string())?,
        revision: 1,
        asset_class: AssetClass::CryptoAsset,
        symbol: profile.symbol.clone(),
        venue_id: "COINBASE".to_string(),
        trading_currency: profile.quote_currency.clone(),
        precision: InstrumentPrecision::try_new(profile.price_scale, profile.quantity_scale)
            .map_err(|error| error.to_string())?,
        lifecycle: InstrumentLifecycle::Active,
    })
}

#[cfg(test)]
pub(super) fn bar_definition() -> BarDefinition {
    bar_definition_for_interval(ChartInterval::Minute1)
}

pub(super) fn bar_definition_for_interval(interval: ChartInterval) -> BarDefinition {
    let interval_seconds = match interval.aggregation() {
        ChartAggregation::FixedSeconds(seconds) => seconds.get(),
        ChartAggregation::CalendarMonth => 30 * 86_400,
        ChartAggregation::Trades(_) => 60,
    };
    BarDefinition {
        definition_id: format!("coinbase:spot:{}:unadjusted:v1", interval.label()),
        version: 1,
        interval_seconds,
        trades_per_bar: None,
    }
}

fn worker_config(detailed_diagnostics: bool) -> Result<DesktopMarketWorkerConfig, String> {
    Ok(DesktopMarketWorkerConfig {
        provider: DesktopProviderConfig::new(nonzero(32), nonzero(1))
            .with_diagnostics(
                ProviderEnvironment {
                    provider_id: "coinbase".to_string(),
                    system_id: "advanced_trade_public".to_string(),
                    environment: "production".to_string(),
                },
                detailed_diagnostics
                    .then_some(NonZeroU64::new(10_000_000_000).unwrap_or(NonZeroU64::MIN)),
            )
            .map_err(|error| error.to_string())?,
        history: HistoryWorkerConfig {
            maximum_cache_entries: nonzero(2),
            maximum_decoded_bytes: nonzero(2 * 1024 * 1024),
            maximum_charts: nonzero(1),
            maximum_segment_read_bytes: nonzero(1024 * 1024),
            maximum_buffered_live: nonzero(512),
            maximum_handoffs: nonzero(1),
        },
        maximum_catalog_entries: 64,
    })
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
    use super::{
        DesktopMarketWorkerError, SUBSCRIPTION_ID, instrument, loading_startup, nonzero,
        open_with_store_lock_retry, product_profile,
    };
    use crate::market_worker::{CoinbaseWorkerStartup, MarketWorkerStartup};
    use axiusflow_coinbase_market_adapter::{
        CanonicalTrade, CoinbaseBarAggregator, CoinbaseBarAggregatorConfig, FixedPointValue,
    };
    use axiusflow_market_data::MarketBar;

    #[test]
    fn store_lock_retry_waits_for_the_replaced_worker_and_stays_bounded() {
        let attempts = std::cell::Cell::new(0_u32);
        let result: Result<(), DesktopMarketWorkerError> = open_with_store_lock_retry(
            || {
                attempts.set(attempts.get().saturating_add(1));
                if attempts.get() < 3 {
                    Err(DesktopMarketWorkerError::HistoryStoreAlreadyOpen)
                } else {
                    Ok(())
                }
            },
            8,
            std::time::Duration::from_millis(1),
        );
        assert!(result.is_ok());
        assert_eq!(attempts.get(), 3);

        let exhausted: Result<(), DesktopMarketWorkerError> = open_with_store_lock_retry(
            || Err(DesktopMarketWorkerError::HistoryStoreAlreadyOpen),
            2,
            std::time::Duration::from_millis(1),
        );
        assert!(matches!(
            exhausted,
            Err(DesktopMarketWorkerError::HistoryStoreAlreadyOpen)
        ));

        let immediate: Result<(), DesktopMarketWorkerError> = open_with_store_lock_retry(
            || Err(DesktopMarketWorkerError::HistoryConfiguration),
            8,
            std::time::Duration::from_millis(1),
        );
        assert!(matches!(
            immediate,
            Err(DesktopMarketWorkerError::HistoryConfiguration)
        ));
    }

    #[test]
    fn live_mode_accepts_only_reviewed_coinbase_precision_profiles() {
        assert!(product_profile("BTC-USD".to_string()).is_ok());
        assert!(product_profile("ETH-USD".to_string()).is_ok());
        assert!(product_profile("SOL-USD".to_string()).is_err());
    }

    #[test]
    fn live_startup_is_loading_metadata_without_synthetic_market_data() {
        let profile = product_profile("BTC-USD".to_string()).expect("profile validates");
        let MarketWorkerStartup::Loading(startup) = loading_startup(&profile) else {
            panic!("live startup must wait for a provider snapshot");
        };
        let CoinbaseWorkerStartup {
            subscription_id,
            worker_label,
            ..
        } = *startup;
        assert_eq!(
            instrument(&profile)
                .expect("instrument validates")
                .instrument_id
                .as_str(),
            profile.instrument_id
        );
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
            canonical_sequence: sequence_num.saturating_add(1),
        }
    }
}
