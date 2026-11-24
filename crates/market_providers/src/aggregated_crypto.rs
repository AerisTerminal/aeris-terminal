//! Aggregated multi-exchange cryptocurrency market-data provider.
//!
//! Combines Binance (USDT-M Perps & Spot), Bybit (Linear & Spot), OKX
//! (Perpetual Swaps & Spot), and Hyperliquid (Perps) into one unified feed:
//! - **Aggregated Trades**: streams every fill from all active constituent
//!   venues in base-asset units, powering combined footprints, CVD, tape,
//!   sweeps, and volume profiles.
//! - **Aggregated L2 Order Book**: maintains per-venue local books and merges
//!   all healthy venues via tick-bucketed summation (`merge_books`), dropping
//!   disconnected or stale venues automatically.
//! - **History**: fetches covering history from Binance with automatic
//!   failover to Bybit -> OKX -> Hyperliquid.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use aeris_binance_market_adapter::{
    BINANCE_FUTURES_MARKET_WS_URL, BINANCE_FUTURES_PUBLIC_WS_URL, BINANCE_SPOT_WS_URL,
    BinanceHttpClient, BinanceHttpConfig, BinanceMarketKind, BinanceOrderBookSync, BinanceWsEvent,
    binance_interval_for_period, book::BookSyncOutcome as BinanceBookOutcome,
    period_for_binance_interval,
};
use aeris_bybit_market_adapter::{
    BYBIT_LINEAR_WS_URL, BYBIT_SPOT_WS_URL, BybitBookState, BybitHttpClient, BybitHttpConfig,
    BybitMarketKind, BybitWsEvent,
};
use aeris_crypto_common::{
    BookLevelUpdate, CanonicalCryptoKey, CryptoError, CryptoMarketKind, CryptoResult,
    LocalOrderBook, TradeDedupRing, merge_books,
};
use aeris_hyperliquid_market_adapter::{
    HYPERLIQUID_WS_URL, HyperliquidHttpClient, HyperliquidHttpConfig, HyperliquidMarketKind,
    WsClientEvent as HlWsEvent,
};
use aeris_market_data::{BarPeriod, EventMetadata, MarketBar, MarketTrade, QualifiedTimestamp};
use aeris_market_engine::{
    MarketStream, ProviderCapabilities as EngineProviderCapabilities, StreamRequirements,
    fence::{Fenced, ProviderTag},
};
use aeris_market_runtime::{
    ProviderCatalogRejectionReason, ProviderCatalogSymbol, ProviderConnectionKind,
    ProviderDailyBarCadence, ProviderFeedLevel, ProviderInstrumentKind,
    ProviderInstrumentSearchResult, ProviderInstrumentSummary, ProviderPresentationDescriptor,
    plugin::{
        AlertDemandUpdate, CalendarBarStamp, CandleDemandPolicy, CatalogPublisher,
        CatalogSessionBinding, ConcurrencyLimits, DemandFlushPolicy, DepthCapabilities,
        HistoryRequest, HistorySnapshot, LiveModel, ProviderCapabilities, ProviderCoordinatorWake,
        ProviderDescriptor, ProviderEventSink, ProviderHistoryCapabilities, ProviderRecoveryPolicy,
        ProviderRuntimeSpec, QuoteCapability, ReconnectPolicy, TradeDemandPolicy,
    },
};
use aeris_okx_market_adapter::{
    OKX_PUBLIC_WS_URL, OkxBookApplyOutcome, OkxBookState, OkxHttpClient, OkxHttpConfig,
    OkxMarketKind, OkxWsEvent,
};
use aeris_platform_runtime::{MarketSocket, MarketSocketEvent};
use aeris_provider_spi::HistoryInstrument;

use crate::crypto_runtime::{
    CATALOG_POLL_INTERVAL, CATALOG_REFRESH_INTERVAL, CONNECT_TIMEOUT, CryptoCatalogControl,
    CryptoCatalogEvent, CryptoDemand, CryptoHistorySource, CryptoRealtimeControl,
    CryptoRealtimeEvent, READ_TIMEOUT, build_install_instrument, crypto_provider_spec, emit_data,
    emit_lifecycle, now_unix_nanos, publish_catalog_event, reject_catalog,
};

pub(crate) const AGGREGATED_ACCOUNT_ID: &str = "crypto-agg-public";
pub(crate) const AGGREGATED_ENTITLEMENT_ID: &str = "crypto-agg-public";
const CATALOG_WORKER_NAME: &str = "aeris-crypto-agg-catalog";
const PROVIDER_WORKER_NAME: &str = "aeris-crypto-agg-provider";
const BOOK_PUBLISH_INTERVAL: Duration = Duration::from_millis(100);
const CANDLE_PUBLISH_INTERVAL: Duration = Duration::from_millis(33);
const MAX_GAP_FILL_BARS: usize = 10;
const VENUE_STALE_TIMEOUT: Duration = Duration::from_secs(15);
const PING_INTERVAL: Duration = Duration::from_secs(20);
const RESYNC_COOLDOWN: Duration = Duration::from_secs(2);
const ONE_FIXED: i64 = 100_000_000;
const MAXIMUM_AGGREGATED_BOOK_LEVELS: usize = 1_000;
const MAX_FRAMES_PER_POLL: usize = 16;
const DRAIN_READ_TIMEOUT: Duration = Duration::from_millis(1);

pub(crate) const AGGREGATED_PRESENTATION: ProviderPresentationDescriptor =
    ProviderPresentationDescriptor {
        id: "crypto_agg",
        display_name: "Aggregated",
        chart_interval_labels: &[
            "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1D", "1W", "1M",
        ],
        daily_bars: ProviderDailyBarCadence::FixedDay,
        search_hint: "Search aggregated multi-exchange crypto",
        logo_key: "all-providers",
        broker_venue_id: None,
        attribution: None,
        catalog_symbol: ProviderCatalogSymbol::ProviderSymbol,
        selection_entitlement_id: AGGREGATED_ENTITLEMENT_ID,
        catalog_refresh_on_startup: true,
        ready_label_suffix: "",
        feed_level: ProviderFeedLevel::PriceLevels,
        search_categories_available: false,
        market_screen_available: false,
        market_scan_available: false,
        trades_available: true,
        connection_kind: ProviderConnectionKind::Public,
    };

pub(crate) const AGGREGATED_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: "crypto_agg",
    presentation: &AGGREGATED_PRESENTATION,
    login_schema: None,
    capabilities: AGGREGATED_CAPABILITIES,
    demand_flush: DemandFlushPolicy::Dirty,
};

const AGGREGATED_CAPABILITIES: ProviderCapabilities = ProviderCapabilities {
    account_id: AGGREGATED_ACCOUNT_ID,
    engine: EngineProviderCapabilities {
        historical_bars: true,
        realtime_bars: true,
        streams: StreamRequirements::BARS
            .with(MarketStream::Trades)
            .with(MarketStream::Quotes)
            .with(MarketStream::Depth),
    },
    always_open: true,
    concurrency: ConcurrencyLimits {
        maximum_sessions: 1,
    },
    depth: Some(DepthCapabilities),
    quotes: Some(QuoteCapability::DedicatedTopOfBook),
    reconnect: ReconnectPolicy::hyperliquid(),
    history: ProviderHistoryCapabilities {
        worker: "aeris-crypto-agg-history",
        supported_period: crate::binance::binance_supported_period,
    },
    recovery_policy: ProviderRecoveryPolicy::WorkerReconcilesDemand,
    alert_demand_update: AlertDemandUpdate::MarkDirty,
    live_model: LiveModel::ProviderCandles,
    calendar_bar_stamp: CalendarBarStamp::TradingDateMidnightUtc,
    overflow_recovery_detail: "Local market event queue overflow; repairing continuity",
    candle_requires_connected: true,
    candle_correction_detail: "Aggregated candle replacement requires covering history",
    candle_wire_interval: Some(crate::binance::binance_candle_interval),
    candle_demand_policy: CandleDemandPolicy::ReconcileAfterSelection,
    trade_demand_policy: TradeDemandPolicy::SessionManaged,
    instrument_missing_detail: "Aggregated instrument is not installed",
    catalog_session_binding: CatalogSessionBinding::CurrentSession,
    maximum_order_level_instruments: 0,
    recovers_silence: false,
};

/// Constituent venue mappings for one canonical aggregated crypto instrument.
#[derive(Clone, Debug)]
pub(crate) struct AggregatedInstrument {
    pub key: CanonicalCryptoKey,
    pub provider_symbol: String,
    pub instrument_id: String,
    pub display_symbol: String,
    pub price_decimals: u8,
    pub quantity_decimals: u8,
    pub tick_size_fixed: i64,
    pub combined_volume_24h: u64,
    pub binance: Option<(BinanceMarketKind, String)>,
    pub bybit: Option<(BybitMarketKind, String)>,
    pub okx: Option<(OkxMarketKind, String, i64)>,
    pub hyperliquid: Option<String>,
}

impl AggregatedInstrument {
    fn venue_count(&self) -> usize {
        usize::from(self.binance.is_some())
            + usize::from(self.bybit.is_some())
            + usize::from(self.okx.is_some())
            + usize::from(self.hyperliquid.is_some())
    }

    fn venue_label(&self) -> String {
        let mut names = Vec::with_capacity(4);
        if self.binance.is_some() {
            names.push("Binance");
        }
        if self.bybit.is_some() {
            names.push("Bybit");
        }
        if self.okx.is_some() {
            names.push("OKX");
        }
        if self.hyperliquid.is_some() {
            names.push("Hyperliquid");
        }
        names.join(" + ")
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct AggregatedCatalog {
    instruments: Vec<AggregatedInstrument>,
    by_provider_symbol: BTreeMap<String, usize>,
}

impl AggregatedCatalog {
    fn from_instruments(mut instruments: Vec<AggregatedInstrument>) -> Self {
        instruments.sort_by(|a, b| {
            b.combined_volume_24h
                .cmp(&a.combined_volume_24h)
                .then_with(|| a.display_symbol.cmp(&b.display_symbol))
        });
        let by_provider_symbol = instruments
            .iter()
            .enumerate()
            .map(|(idx, item)| (item.provider_symbol.clone(), idx))
            .collect();
        Self {
            instruments,
            by_provider_symbol,
        }
    }

    fn is_empty(&self) -> bool {
        self.instruments.is_empty()
    }

    fn get(&self, provider_symbol: &str) -> Option<&AggregatedInstrument> {
        self.by_provider_symbol
            .get(provider_symbol)
            .and_then(|idx| self.instruments.get(*idx))
    }

    fn search(
        &self,
        query: &str,
        limit: usize,
        consumer_id: u64,
        provider_generation: u64,
        search_generation: u64,
    ) -> ProviderInstrumentSearchResult {
        let needle = query.trim().to_ascii_uppercase();
        let cap = limit.clamp(1, 200);
        let mut exact = Vec::new();
        let mut prefix = Vec::new();
        let mut contains = Vec::new();

        for item in &self.instruments {
            if needle.is_empty() {
                contains.push(item);
                if contains.len() >= cap {
                    break;
                }
                continue;
            }
            let sym = item.display_symbol.to_ascii_uppercase();
            let base = item.key.base.to_ascii_uppercase();
            if sym == needle || base == needle || item.provider_symbol == needle {
                exact.push(item);
            } else if sym.starts_with(&needle) || base.starts_with(&needle) {
                prefix.push(item);
            } else if sym.contains(&needle) || item.provider_symbol.contains(&needle) {
                contains.push(item);
            }
        }

        let instruments = exact
            .into_iter()
            .chain(prefix)
            .chain(contains)
            .take(cap)
            .map(|item| {
                let kind_label = match item.key.kind {
                    CryptoMarketKind::Perp => "Perpetual",
                    CryptoMarketKind::Spot => "Spot",
                };
                ProviderInstrumentSummary {
                    kind: ProviderInstrumentKind::Instrument,
                    symbol: item.provider_symbol.clone(),
                    display_symbol: item.display_symbol.clone(),
                    exchange: match item.key.kind {
                        CryptoMarketKind::Perp => "AGG-PERP".to_string(),
                        CryptoMarketKind::Spot => "AGG-SPOT".to_string(),
                    },
                    name: Some(format!(
                        "{}/{} {kind_label} ({})",
                        item.key.base,
                        item.key.quote,
                        item.venue_label()
                    )),
                    product_code: Some(item.key.base.clone()),
                    instrument_type: Some(kind_label.to_string()),
                    expiration_date: None,
                    asset_class: Some("Crypto".to_string()),
                }
            })
            .collect();

        ProviderInstrumentSearchResult {
            consumer_id,
            provider: "crypto_agg".to_string(),
            provider_generation,
            search_generation,
            instruments,
        }
    }
}

/// Shared catalog state accessible by both history and realtime workers.
pub(crate) type SharedAggregatedCatalog = Arc<RwLock<AggregatedCatalog>>;
/// Shared forming-bar seeds written by covering history fetches so live trade-built
/// candles preserve the full open/high/low/volume of a bar opened mid-period.
pub(crate) type SharedFormingSeeds = Arc<RwLock<BTreeMap<(String, String), MarketBar>>>;
const MAXIMUM_FORMING_SEEDS: usize = 128;

pub(crate) fn provider_spec(realtime_enabled: bool) -> ProviderRuntimeSpec {
    let shared_catalog: SharedAggregatedCatalog =
        Arc::new(RwLock::new(AggregatedCatalog::default()));
    let shared_seeds: SharedFormingSeeds = Arc::new(RwLock::new(BTreeMap::new()));
    let history = CryptoHistorySource::Aggregated {
        catalog: Arc::clone(&shared_catalog),
        forming_seeds: Arc::clone(&shared_seeds),
        binance: BinanceHttpClient::default(),
        bybit: BybitHttpClient::default(),
        okx: OkxHttpClient::default(),
        hyperliquid: HyperliquidHttpClient::default(),
    };
    let cat_for_cat = Arc::clone(&shared_catalog);
    let cat_for_rt = Arc::clone(&shared_catalog);
    let seeds_for_rt = Arc::clone(&shared_seeds);
    crypto_provider_spec(
        AGGREGATED_DESCRIPTOR,
        history,
        realtime_enabled,
        CATALOG_WORKER_NAME,
        PROVIDER_WORKER_NAME,
        "Aggregated crypto session is recovering",
        binance_interval_for_period,
        move |controls, events, ws_gen, stop| {
            run_catalog(controls, events, ws_gen, stop, &cat_for_cat);
        },
        move |controls, events, stop, ws_gen, wake| {
            run_realtime(
                controls,
                events,
                stop,
                ws_gen,
                wake,
                &cat_for_rt,
                &seeds_for_rt,
            );
        },
    )
}

fn record_forming_seed(
    seeds: &SharedFormingSeeds,
    request: &HistoryRequest,
    installed: &HistoryInstrument,
    snapshot: &HistorySnapshot,
) {
    if request.range.is_some() {
        return;
    }
    let Some(forming) = snapshot.forming else {
        return;
    };
    let Ok(wire_iv) = binance_interval_for_period(request.series.period) else {
        return;
    };
    if let Ok(mut guard) = seeds.write() {
        if guard.len() >= MAXIMUM_FORMING_SEEDS
            && !guard.contains_key(&(installed.provider_symbol.clone(), wire_iv.to_string()))
        {
            guard.clear();
        }
        guard.insert(
            (installed.provider_symbol.clone(), wire_iv.to_string()),
            forming.bar,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn fetch_aggregated_history(
    catalog: &SharedAggregatedCatalog,
    forming_seeds: &SharedFormingSeeds,
    binance: &mut BinanceHttpClient,
    bybit: &mut BybitHttpClient,
    okx: &mut OkxHttpClient,
    hyperliquid: &mut HyperliquidHttpClient,
    request: &HistoryRequest,
    installed: &HistoryInstrument,
    provider_gen: u64,
) -> CryptoResult<HistorySnapshot> {
    let agg_inst = {
        let guard = catalog
            .read()
            .map_err(|_| CryptoError::History("aggregated catalog lock poisoned".to_string()))?;
        guard.get(&installed.provider_symbol).cloned()
    };
    let Some(agg) = agg_inst else {
        return Err(CryptoError::History(
            "Aggregated instrument is not loaded in catalog".to_string(),
        ));
    };
    let mut base = installed.clone();

    // Primary: Binance -> Fallback: Bybit -> OKX -> Hyperliquid
    if let Some((kind, sym)) = &agg.binance {
        base.provider = "binance".to_string();
        base.provider_symbol = format!("{}:{sym}", kind.prefix());
        if let Ok(snap) = aeris_binance_market_adapter::history::fetch(
            binance,
            &request.series,
            provider_gen,
            &base,
            request.req_window(),
            &request.stop,
        ) {
            record_forming_seed(forming_seeds, request, installed, &snap);
            return Ok(snap);
        }
    }
    if let Some((kind, sym)) = &agg.bybit {
        base.provider = "bybit".to_string();
        base.provider_symbol = format!("{}:{sym}", kind.category());
        if let Ok(snap) = aeris_bybit_market_adapter::history::fetch(
            bybit,
            &request.series,
            provider_gen,
            &base,
            request.req_window(),
            &request.stop,
        ) {
            record_forming_seed(forming_seeds, request, installed, &snap);
            return Ok(snap);
        }
    }
    if let Some((kind, inst_id, _)) = &agg.okx {
        base.provider = "okx".to_string();
        base.provider_symbol = format!("{}:{inst_id}", kind.prefix());
        if let Ok(snap) = aeris_okx_market_adapter::history::fetch(
            okx,
            &request.series,
            provider_gen,
            &base,
            request.req_window(),
            &request.stop,
        ) {
            record_forming_seed(forming_seeds, request, installed, &snap);
            return Ok(snap);
        }
    }
    if let Some(coin) = &agg.hyperliquid {
        base.provider = "hyperliquid".to_string();
        base.provider_symbol.clone_from(coin);
        if let Ok(snap) = aeris_hyperliquid_market_adapter::history::fetch(
            hyperliquid,
            &request.series,
            provider_gen,
            &base,
            request.req_window(),
            &request.stop,
        ) {
            record_forming_seed(forming_seeds, request, installed, &snap);
            return Ok(snap);
        }
    }
    Err(CryptoError::History(
        "Aggregated history is unavailable from all constituent exchanges".to_string(),
    ))
}

#[allow(clippy::too_many_lines)]
fn fetch_combined_catalog() -> CryptoResult<AggregatedCatalog> {
    let (binance, bybit, okx, hyperliquid) = thread::scope(|scope| {
        let h_bin = scope.spawn(|| {
            aeris_binance_market_adapter::fetch_catalog(BinanceHttpConfig::default()).ok()
        });
        let h_byb = scope
            .spawn(|| aeris_bybit_market_adapter::fetch_catalog(BybitHttpConfig::default()).ok());
        let h_okx =
            scope.spawn(|| aeris_okx_market_adapter::fetch_catalog(OkxHttpConfig::default()).ok());
        let h_hl = scope.spawn(|| aeris_hyperliquid_market_adapter::fetch_meta_bundle(HyperliquidHttpConfig::default()).ok());
        (
            h_bin.join().ok().flatten(),
            h_byb.join().ok().flatten(),
            h_okx.join().ok().flatten(),
            h_hl.join().ok().flatten(),
        )
    });

    let mut grouped: BTreeMap<CanonicalCryptoKey, AggregatedInstrument> = BTreeMap::new();

    if let Some(cat) = binance {
        for item in cat.instruments() {
            let key = item.canonical_key();
            let entry = grouped
                .entry(key.clone())
                .or_insert_with(|| AggregatedInstrument {
                    provider_symbol: key.to_string(),
                    instrument_id: key.aggregated_instrument_id(),
                    display_symbol: key.display_symbol(),
                    key,
                    price_decimals: item.price_decimals,
                    quantity_decimals: item.quantity_decimals,
                    tick_size_fixed: item.tick_size_fixed,
                    combined_volume_24h: 0,
                    binance: None,
                    bybit: None,
                    okx: None,
                    hyperliquid: None,
                });
            entry.binance = Some((item.kind, item.wire_symbol.clone()));
            entry.combined_volume_24h = entry
                .combined_volume_24h
                .saturating_add(item.volume_24h_quote);
            entry.tick_size_fixed = entry.tick_size_fixed.max(item.tick_size_fixed);
            entry.price_decimals = entry.price_decimals.min(item.price_decimals);
        }
    }

    if let Some(cat) = bybit {
        for item in cat.instruments() {
            let key = item.canonical_key();
            let entry = grouped
                .entry(key.clone())
                .or_insert_with(|| AggregatedInstrument {
                    provider_symbol: key.to_string(),
                    instrument_id: key.aggregated_instrument_id(),
                    display_symbol: key.display_symbol(),
                    key,
                    price_decimals: item.price_decimals,
                    quantity_decimals: item.quantity_decimals,
                    tick_size_fixed: item.tick_size_fixed,
                    combined_volume_24h: 0,
                    binance: None,
                    bybit: None,
                    okx: None,
                    hyperliquid: None,
                });
            entry.bybit = Some((item.kind, item.wire_symbol.clone()));
            entry.combined_volume_24h = entry
                .combined_volume_24h
                .saturating_add(item.volume_24h_quote);
            entry.tick_size_fixed = entry.tick_size_fixed.max(item.tick_size_fixed);
            entry.price_decimals = entry.price_decimals.min(item.price_decimals);
        }
    }

    if let Some(cat) = okx {
        for item in cat.instruments() {
            let key = item.canonical_key();
            let entry = grouped
                .entry(key.clone())
                .or_insert_with(|| AggregatedInstrument {
                    provider_symbol: key.to_string(),
                    instrument_id: key.aggregated_instrument_id(),
                    display_symbol: key.display_symbol(),
                    key,
                    price_decimals: item.price_decimals,
                    quantity_decimals: item.quantity_decimals,
                    tick_size_fixed: item.tick_size_fixed,
                    combined_volume_24h: 0,
                    binance: None,
                    bybit: None,
                    okx: None,
                    hyperliquid: None,
                });
            entry.okx = Some((item.kind, item.inst_id.clone(), item.ct_val_fixed));
            entry.combined_volume_24h = entry
                .combined_volume_24h
                .saturating_add(item.volume_24h_quote);
            entry.tick_size_fixed = entry.tick_size_fixed.max(item.tick_size_fixed);
            entry.price_decimals = entry.price_decimals.min(item.price_decimals);
        }
    }

    // Hyperliquid core perps are USDC-margined; keep them in USDC pairs so USDT
    // aggregated feeds never mix USDC/USDT basis spreads into the same price ladder.
    if let Some(cat) = hyperliquid {
        for item in cat.instruments.values() {
            if matches!(item.kind, HyperliquidMarketKind::CorePerp) {
                let key = CanonicalCryptoKey::new(&item.wire_coin, "USDC", CryptoMarketKind::Perp);
                if let Some(entry) = grouped.get_mut(&key) {
                    entry.hyperliquid = Some(item.wire_coin.clone());
                }
            }
        }
    }

    let multi_venue: Vec<AggregatedInstrument> = grouped
        .into_values()
        .filter(|item| item.venue_count() >= 2)
        .collect();
    if multi_venue.is_empty() {
        return Err(CryptoError::Transport(
            "aggregated crypto catalog could not reach constituent exchanges".to_string(),
        ));
    }
    Ok(AggregatedCatalog::from_instruments(multi_venue))
}

#[allow(clippy::too_many_lines)]
fn run_catalog(
    controls: &Receiver<CryptoCatalogControl>,
    events: &CatalogPublisher<CryptoCatalogEvent>,
    ws_generation: &Arc<AtomicU64>,
    stop: &Arc<AtomicBool>,
    shared: &SharedAggregatedCatalog,
) {
    let mut fetched_at: Option<Instant> = None;
    let mut selection_generation: u64 = 0;
    let mut next_refresh = Instant::now();

    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let is_loaded = shared.read().is_ok_and(|c| !c.is_empty());
        let wait = if is_loaded {
            next_refresh
                .saturating_duration_since(Instant::now())
                .min(CATALOG_POLL_INTERVAL)
        } else {
            CATALOG_POLL_INTERVAL
        };
        match controls.recv_timeout(wait) {
            Ok(CryptoCatalogControl::Search(search)) => {
                if !is_loaded {
                    match fetch_combined_catalog() {
                        Ok(loaded) => {
                            if let Ok(mut guard) = shared.write() {
                                *guard = loaded;
                            }
                            fetched_at = Some(Instant::now());
                            next_refresh = Instant::now() + CATALOG_REFRESH_INTERVAL;
                        }
                        Err(_error) => {
                            if reject_catalog(
                                events,
                                search.consumer_id,
                                "crypto_agg",
                                search.search_generation,
                                ProviderCatalogRejectionReason::SearchRejected,
                                false,
                                stop,
                            ) {
                                return;
                            }
                            continue;
                        }
                    }
                }
                if let Ok(guard) = shared.read() {
                    let result = guard.search(
                        &search.query,
                        usize::try_from(search.maximum_results).unwrap_or(50),
                        search.consumer_id,
                        ws_generation.load(Ordering::Acquire).max(1),
                        search.search_generation,
                    );
                    if publish_catalog_event(
                        events,
                        CryptoCatalogEvent::SearchCompleted(result),
                        stop,
                    ) {
                        return;
                    }
                }
            }
            Ok(CryptoCatalogControl::Select(select)) => {
                if !is_loaded {
                    match fetch_combined_catalog() {
                        Ok(loaded) => {
                            if let Ok(mut guard) = shared.write() {
                                *guard = loaded;
                            }
                            fetched_at = Some(Instant::now());
                            next_refresh = Instant::now() + CATALOG_REFRESH_INTERVAL;
                        }
                        Err(_error) => {
                            if reject_catalog(
                                events,
                                select.consumer_id,
                                "crypto_agg",
                                select.selection_generation,
                                ProviderCatalogRejectionReason::InstrumentUnavailable,
                                true,
                                stop,
                            ) {
                                return;
                            }
                            continue;
                        }
                    }
                }
                let found = shared
                    .read()
                    .ok()
                    .and_then(|c| c.get(&select.symbol).cloned());
                let Some(item) = found else {
                    if reject_catalog(
                        events,
                        select.consumer_id,
                        "crypto_agg",
                        select.selection_generation,
                        ProviderCatalogRejectionReason::InstrumentUnavailable,
                        true,
                        stop,
                    ) {
                        return;
                    }
                    continue;
                };
                selection_generation = selection_generation.saturating_add(1).max(1);
                let venue_id = match item.key.kind {
                    CryptoMarketKind::Perp => "AGG-PERP".to_string(),
                    CryptoMarketKind::Spot => "AGG-SPOT".to_string(),
                };
                let install = build_install_instrument(
                    "crypto_agg",
                    AGGREGATED_ENTITLEMENT_ID,
                    ws_generation.load(Ordering::Acquire),
                    selection_generation,
                    item.instrument_id.clone(),
                    item.provider_symbol.clone(),
                    item.display_symbol.clone(),
                    venue_id,
                    item.quantity_decimals,
                    item.tick_size_fixed,
                    item.key.kind == CryptoMarketKind::Spot,
                    &item.key.quote,
                );
                if publish_catalog_event(
                    events,
                    CryptoCatalogEvent::SelectionResolved {
                        consumer_id: select.consumer_id,
                        command_generation: select.selection_generation,
                        instrument: install,
                    },
                    stop,
                ) {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        if let Some(at) = fetched_at {
            next_refresh = next_refresh.max(at + CATALOG_REFRESH_INTERVAL);
        }
        if is_loaded && Instant::now() >= next_refresh {
            match fetch_combined_catalog() {
                Ok(loaded) => {
                    if let Ok(mut guard) = shared.write() {
                        *guard = loaded;
                    }
                    fetched_at = Some(Instant::now());
                    next_refresh = Instant::now() + CATALOG_REFRESH_INTERVAL;
                }
                Err(error) => {
                    next_refresh = Instant::now() + CATALOG_REFRESH_INTERVAL;
                    if publish_catalog_event(
                        events,
                        CryptoCatalogEvent::RefreshFailed {
                            detail: error.into_message(),
                        },
                        stop,
                    ) {
                        return;
                    }
                }
            }
        }
    }
}

/// Live OHLCV accumulator built directly from the aggregated trade stream so
/// candlestick and footprint charts stay in exact sync.
struct AggregatedLiveBar {
    period: BarPeriod,
    current: Option<MarketBar>,
    /// Open timestamp of the forming bar already seeded from covering REST history so
    /// opening mid-bar never overwrites the bar's earlier open/high/low/volume.
    seeded_open_nanos: Option<i64>,
    dirty: bool,
    last_emit: Instant,
}

impl AggregatedLiveBar {
    fn new(period: BarPeriod) -> Self {
        Self {
            period,
            current: None,
            seeded_open_nanos: None,
            dirty: false,
            last_emit: Instant::now()
                .checked_sub(CANDLE_PUBLISH_INTERVAL)
                .unwrap_or_else(Instant::now),
        }
    }

    /// Merges the covering REST history's forming bar into this accumulator.
    /// Safe whether called before the first live trade or after live trades have
    /// already started arriving for the same open period.
    fn merge_seed(&mut self, seed: MarketBar) {
        let seed_open = seed.exchange_timestamp_unix_nanos;
        if self.seeded_open_nanos == Some(seed_open) {
            return;
        }
        if let Some(cur) = &mut self.current {
            if cur.exchange_timestamp_unix_nanos == seed_open {
                cur.open = seed.open;
                cur.high = cur.high.max(seed.high);
                cur.low = cur.low.min(seed.low);
                cur.volume = cur.volume.max(seed.volume);
                self.seeded_open_nanos = Some(seed_open);
                self.dirty = true;
            }
        } else {
            self.current = Some(seed);
            self.seeded_open_nanos = Some(seed_open);
            self.dirty = false;
        }
    }

    /// Applies one aggregated trade, returning any bars that must be emitted
    /// immediately (closing bar + gap fills + newly opened bar).
    fn on_trade(&mut self, ts_nanos: i64, price: i64, quantity: i64) -> Vec<MarketBar> {
        let open_nanos = bucket_open_unix_nanos(ts_nanos, self.period);
        let mut immediate = Vec::new();
        if let Some(cur) = &mut self.current {
            if open_nanos < cur.exchange_timestamp_unix_nanos {
                // Late cross-exchange trade from the prior bucket; keep candle
                // open timestamps strictly monotonic.
                return immediate;
            }
            if open_nanos == cur.exchange_timestamp_unix_nanos {
                cur.high = cur.high.max(price);
                cur.low = cur.low.min(price);
                cur.close = price;
                cur.volume = cur.volume.saturating_add(quantity);
                self.dirty = true;
                return immediate;
            }
            // Roll over to a newer bar: flush the closing bar first.
            if self.dirty {
                immediate.push(*cur);
            }
            if let Some(dur) = self.period.duration_nanos() {
                let mut fill_open = cur.exchange_timestamp_unix_nanos.saturating_add(dur);
                let mut filled = 0usize;
                while fill_open < open_nanos && filled < MAX_GAP_FILL_BARS {
                    immediate.push(MarketBar {
                        source_sequence: 1,
                        exchange_timestamp_seconds: fill_open.div_euclid(1_000_000_000),
                        exchange_timestamp_unix_nanos: fill_open,
                        open: cur.close,
                        high: cur.close,
                        low: cur.close,
                        close: cur.close,
                        volume: 0,
                    });
                    fill_open = fill_open.saturating_add(dur);
                    filled += 1;
                }
            }
            let next = MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: open_nanos.div_euclid(1_000_000_000),
                exchange_timestamp_unix_nanos: open_nanos,
                open: price,
                high: price,
                low: price,
                close: price,
                volume: quantity,
            };
            *cur = next;
            self.dirty = false;
            self.last_emit = Instant::now();
            immediate.push(next);
        } else {
            let first = MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: open_nanos.div_euclid(1_000_000_000),
                exchange_timestamp_unix_nanos: open_nanos,
                open: price,
                high: price,
                low: price,
                close: price,
                volume: quantity,
            };
            self.current = Some(first);
            self.dirty = false;
            self.last_emit = Instant::now();
            immediate.push(first);
        }
        immediate
    }
}

/// Aligns a Unix nanosecond timestamp down to the UTC bar open boundary.
fn bucket_open_unix_nanos(ts_nanos: i64, period: BarPeriod) -> i64 {
    const NANOS_PER_SEC: i64 = 1_000_000_000;
    const NANOS_PER_DAY: i64 = 86_400 * NANOS_PER_SEC;
    const NANOS_PER_WEEK: i64 = 7 * NANOS_PER_DAY;
    // 1970-01-01 was a Thursday; adding 3 days aligns Monday 00:00 UTC to 0 mod week.
    const THURSDAY_TO_MONDAY_OFFSET: i64 = 3 * NANOS_PER_DAY;
    match period {
        BarPeriod::Time { seconds } => {
            let dur = i64::from(seconds).saturating_mul(NANOS_PER_SEC).max(1);
            ts_nanos.div_euclid(dur).saturating_mul(dur)
        }
        BarPeriod::Session { days } => {
            let dur = i64::from(days).saturating_mul(NANOS_PER_DAY).max(1);
            ts_nanos.div_euclid(dur).saturating_mul(dur)
        }
        BarPeriod::Week { .. } => {
            let shifted = ts_nanos.saturating_add(THURSDAY_TO_MONDAY_OFFSET);
            shifted
                .div_euclid(NANOS_PER_WEEK)
                .saturating_mul(NANOS_PER_WEEK)
                .saturating_sub(THURSDAY_TO_MONDAY_OFFSET)
        }
        BarPeriod::Month { .. } => {
            let days = ts_nanos.div_euclid(NANOS_PER_DAY);
            let (year, month) = civil_year_month_from_days(days);
            days_from_civil_first_of_month(year, month).saturating_mul(NANOS_PER_DAY)
        }
        BarPeriod::Tick { .. } => ts_nanos,
    }
}

/// Howard Hinnant's civil-from-days algorithm returning `(year, month 1..=12)`.
const fn civil_year_month_from_days(z: i64) -> (i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m)
}

/// Howard Hinnant's days-from-civil algorithm for the 1st of `(year, month)`.
const fn days_from_civil_first_of_month(year: i64, month: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Per-instrument aggregated state holding each constituent venue's live book
/// and trade-built live candles.
struct AggregatedInstrumentState {
    meta: AggregatedInstrument,
    binance_book: BinanceOrderBookSync,
    binance_updated_at: Option<Instant>,
    bybit_book: BybitBookState,
    bybit_updated_at: Option<Instant>,
    okx_book: OkxBookState,
    okx_updated_at: Option<Instant>,
    hl_book: LocalOrderBook,
    hl_updated_at: Option<Instant>,
    live_bars: BTreeMap<String, AggregatedLiveBar>,
    last_book_publish: Instant,
    book_dirty: bool,
    last_resync: Option<Instant>,
}

impl AggregatedInstrumentState {
    fn new(meta: AggregatedInstrument) -> Self {
        let bin_kind = meta
            .binance
            .as_ref()
            .map_or(BinanceMarketKind::UsdmPerp, |(k, _)| *k);
        Self {
            meta,
            binance_book: BinanceOrderBookSync::new(bin_kind),
            binance_updated_at: None,
            bybit_book: BybitBookState::new(),
            bybit_updated_at: None,
            okx_book: OkxBookState::new(),
            okx_updated_at: None,
            hl_book: LocalOrderBook::new(20),
            hl_updated_at: None,
            live_bars: BTreeMap::new(),
            last_book_publish: Instant::now(),
            book_dirty: false,
            last_resync: None,
        }
    }

    fn sync_candle_intervals(&mut self, demand: &CryptoDemand, seeds: &SharedFormingSeeds) {
        let mut wanted = BTreeSet::new();
        for c in &demand.candles {
            if c.instrument.provider_symbol == self.meta.provider_symbol {
                wanted.insert(c.interval.clone());
            }
        }
        self.live_bars.retain(|iv, _| wanted.contains(iv));
        for iv in wanted {
            if !self.live_bars.contains_key(&iv)
                && let Some(period) = period_for_binance_interval(&iv)
            {
                self.live_bars.insert(iv, AggregatedLiveBar::new(period));
            }
        }
        self.apply_forming_seeds(seeds);
    }

    fn apply_forming_seeds(&mut self, seeds: &SharedFormingSeeds) {
        if self.live_bars.is_empty() {
            return;
        }
        let Ok(guard) = seeds.read() else {
            return;
        };
        for (interval, bar_state) in &mut self.live_bars {
            if let Some(seed) = guard.get(&(self.meta.provider_symbol.clone(), interval.clone())) {
                bar_state.merge_seed(*seed);
            }
        }
    }

    fn record_trade(
        &mut self,
        trade: &MarketTrade,
        sink: ProviderEventSink,
        events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
        wake: &ProviderCoordinatorWake,
        generation: u64,
    ) {
        if self.live_bars.is_empty() {
            return;
        }
        let ts_nanos = trade
            .metadata
            .timestamps
            .exchange_unix_nanos
            .unwrap_or(trade.metadata.timestamps.received_unix_nanos);
        for (interval, bar_state) in &mut self.live_bars {
            for bar in bar_state.on_trade(ts_nanos, trade.price, trade.quantity) {
                emit_data(
                    sink,
                    events,
                    CryptoRealtimeEvent::Candle(
                        generation,
                        self.meta.provider_symbol.clone(),
                        interval.clone(),
                        bar,
                    ),
                    wake,
                );
            }
        }
    }

    fn flush_dirty_candles(
        &mut self,
        now: Instant,
        sink: ProviderEventSink,
        events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
        wake: &ProviderCoordinatorWake,
        generation: u64,
    ) {
        for (interval, bar_state) in &mut self.live_bars {
            if bar_state.dirty
                && now.saturating_duration_since(bar_state.last_emit) >= CANDLE_PUBLISH_INTERVAL
                && let Some(bar) = bar_state.current
            {
                emit_data(
                    sink,
                    events,
                    CryptoRealtimeEvent::Candle(
                        generation,
                        self.meta.provider_symbol.clone(),
                        interval.clone(),
                        bar,
                    ),
                    wake,
                );
                bar_state.dirty = false;
                bar_state.last_emit = now;
            }
        }
    }

    fn healthy_books(&self, now: Instant) -> Vec<&LocalOrderBook> {
        let mut out = Vec::with_capacity(4);
        if self.binance_book.is_synced()
            && self
                .binance_updated_at
                .is_some_and(|t| now.saturating_duration_since(t) <= VENUE_STALE_TIMEOUT)
        {
            out.push(self.binance_book.local_book());
        }
        if self.bybit_book.is_synced()
            && self
                .bybit_updated_at
                .is_some_and(|t| now.saturating_duration_since(t) <= VENUE_STALE_TIMEOUT)
        {
            out.push(self.bybit_book.local_book());
        }
        if self.okx_book.is_synced()
            && self
                .okx_updated_at
                .is_some_and(|t| now.saturating_duration_since(t) <= VENUE_STALE_TIMEOUT)
        {
            out.push(self.okx_book.local_book());
        }
        if self.hl_book.is_synced()
            && self
                .hl_updated_at
                .is_some_and(|t| now.saturating_duration_since(t) <= VENUE_STALE_TIMEOUT)
        {
            out.push(&self.hl_book);
        }
        out
    }
}

fn restamp_trade(
    mut trade: MarketTrade,
    venue_prefix: &str,
    instrument_id: &str,
    generation: u64,
    seq: u64,
) -> MarketTrade {
    trade.metadata.provider_id = "crypto_agg".to_string();
    trade.metadata.instrument_id = instrument_id.to_string();
    trade.metadata.entitlement_id = AGGREGATED_ENTITLEMENT_ID.to_string();
    trade.metadata.session_generation = generation;
    trade.metadata.source_sequence = seq;
    trade.trade_id = format!("{venue_prefix}:{}", trade.trade_id);
    trade
}

#[allow(clippy::too_many_lines)]
fn run_realtime(
    controls: &Receiver<CryptoRealtimeControl>,
    events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
    stop: &Arc<AtomicBool>,
    ws_generation: &Arc<AtomicU64>,
    wake: &ProviderCoordinatorWake,
    shared_catalog: &SharedAggregatedCatalog,
    shared_seeds: &SharedFormingSeeds,
) {
    let mut demand = CryptoDemand::default();
    let mut binance_http = BinanceHttpClient::default();

    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let sink = match controls.recv_timeout(Duration::from_millis(50)) {
            Ok(CryptoRealtimeControl::Connect(sink)) => sink,
            Ok(CryptoRealtimeControl::Subscribe(next)) => {
                demand = next;
                continue;
            }
            Ok(CryptoRealtimeControl::Stop) | Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };

        let generation = sink.epoch.get().get();
        ws_generation.store(generation, Ordering::Release);
        if !emit_lifecycle(
            sink,
            events,
            CryptoRealtimeEvent::Connecting(generation),
            wake,
            stop,
        ) {
            return;
        }
        if !emit_lifecycle(
            sink,
            events,
            CryptoRealtimeEvent::Connected(generation),
            wake,
            stop,
        ) {
            return;
        }

        let mut states: BTreeMap<String, AggregatedInstrumentState> = BTreeMap::new();
        let mut sockets = VenueSockets::default();
        let mut last_ping = Instant::now();
        let mut dedup: TradeDedupRing<String> = TradeDedupRing::new(8_192);
        let mut seq: u64 = 0;
        let mut demand_dirty = true;

        'session: loop {
            if stop.load(Ordering::Acquire) {
                sockets.close_all();
                return;
            }
            while let Ok(ctrl) = controls.try_recv() {
                match ctrl {
                    CryptoRealtimeControl::Connect(_) | CryptoRealtimeControl::Stop => {
                        sockets.close_all();
                        break 'session;
                    }
                    CryptoRealtimeControl::Subscribe(next) => {
                        demand = next;
                        demand_dirty = true;
                    }
                }
            }

            if demand_dirty {
                // Rebuild active state set and fresh venue subscriptions for demanded instruments.
                let mut needed_keys = BTreeSet::new();
                for c in &demand.candles {
                    needed_keys.insert(c.instrument.provider_symbol.clone());
                }
                for i in demand
                    .trades
                    .iter()
                    .chain(&demand.quotes)
                    .chain(&demand.books)
                {
                    needed_keys.insert(i.provider_symbol.clone());
                }
                states.retain(|k, _| needed_keys.contains(k));
                if let Ok(cat) = shared_catalog.read() {
                    for key in &needed_keys {
                        if !states.contains_key(key)
                            && let Some(meta) = cat.get(key)
                        {
                            states
                                .insert(key.clone(), AggregatedInstrumentState::new(meta.clone()));
                        }
                    }
                }
                for state in states.values_mut() {
                    state.sync_candle_intervals(&demand, shared_seeds);
                }

                // Reconcile each constituent venue socket (best-effort: one venue down never blocks others).
                sockets.close_all();
                connect_constituent_venues(&states, stop, &mut sockets);
                last_ping = Instant::now();
                demand_dirty = false;
            }

            if last_ping.elapsed() >= PING_INTERVAL {
                if let Some(s) = sockets.byb_lin.as_mut()
                    && s.send_text(&aeris_bybit_market_adapter::build_ping())
                        .is_err()
                    && let Some(mut closed) = sockets.byb_lin.take()
                {
                    closed.close();
                }
                if let Some(s) = sockets.byb_spot.as_mut()
                    && s.send_text(&aeris_bybit_market_adapter::build_ping())
                        .is_err()
                    && let Some(mut closed) = sockets.byb_spot.take()
                {
                    closed.close();
                }
                if let Some(s) = sockets.okx.as_mut()
                    && s.send_text(&aeris_okx_market_adapter::build_ping())
                        .is_err()
                    && let Some(mut closed) = sockets.okx.take()
                {
                    closed.close();
                }
                if let Some(s) = sockets.hl.as_mut()
                    && s.send_text(&aeris_hyperliquid_market_adapter::build_ping())
                        .is_err()
                    && let Some(mut closed) = sockets.hl.take()
                {
                    closed.close();
                }
                last_ping = Instant::now();
            }

            let mut any_socket = false;

            // 1. Binance Futures (market + public) & Spot
            for (kind, slot) in [
                (BinanceMarketKind::UsdmPerp, &mut sockets.bin_fut_market),
                (BinanceMarketKind::UsdmPerp, &mut sockets.bin_fut_public),
                (BinanceMarketKind::Spot, &mut sockets.bin_spot),
            ] {
                if slot.is_some() {
                    any_socket = true;
                    drain_socket(slot, |text| {
                        poll_binance_agg(
                            kind,
                            text,
                            &mut states,
                            &mut binance_http,
                            &mut dedup,
                            sink,
                            events,
                            wake,
                            stop,
                            generation,
                            &mut seq,
                        );
                    });
                }
            }

            // 2. Bybit Linear & Spot
            for (kind, slot) in [
                (BybitMarketKind::Linear, &mut sockets.byb_lin),
                (BybitMarketKind::Spot, &mut sockets.byb_spot),
            ] {
                if slot.is_some() {
                    any_socket = true;
                    drain_socket(slot, |text| {
                        poll_bybit_agg(
                            kind,
                            text,
                            &mut states,
                            &mut dedup,
                            sink,
                            events,
                            wake,
                            generation,
                            &mut seq,
                        );
                    });
                }
            }

            // 3. OKX Public
            if sockets.okx.is_some() {
                any_socket = true;
                drain_socket(&mut sockets.okx, |text| {
                    poll_okx_agg(
                        text,
                        &mut states,
                        &mut dedup,
                        sink,
                        events,
                        wake,
                        generation,
                        &mut seq,
                    );
                });
            }

            // 4. Hyperliquid
            if sockets.hl.is_some() {
                any_socket = true;
                drain_socket(&mut sockets.hl, |text| {
                    poll_hl_agg(
                        text,
                        &mut states,
                        &mut dedup,
                        sink,
                        events,
                        wake,
                        generation,
                        &mut seq,
                    );
                });
            }

            // 5. Flush trade-built aggregated candles (30fps) and merged order books (10fps)
            let now = Instant::now();
            let received_nanos = now_unix_nanos();
            for state in states.values_mut() {
                state.apply_forming_seeds(shared_seeds);
                state.flush_dirty_candles(now, sink, events, wake, generation);
                if !state.book_dirty
                    || now.saturating_duration_since(state.last_book_publish)
                        < BOOK_PUBLISH_INTERVAL
                {
                    continue;
                }
                let healthy = state.healthy_books(now);
                if healthy.is_empty() {
                    continue;
                }
                seq = seq.saturating_add(1).max(1);
                let metadata = EventMetadata {
                    provider_id: "crypto_agg".to_string(),
                    instrument_id: state.meta.instrument_id.clone(),
                    entitlement_id: AGGREGATED_ENTITLEMENT_ID.to_string(),
                    source_sequence: seq,
                    session_generation: generation,
                    timestamps: QualifiedTimestamp {
                        exchange_unix_nanos: Some(received_nanos),
                        provider_unix_nanos: Some(received_nanos),
                        received_unix_nanos: received_nanos,
                    },
                };
                if let Ok(snapshot) = merge_books(
                    healthy,
                    state.meta.tick_size_fixed.max(1),
                    MAXIMUM_AGGREGATED_BOOK_LEVELS,
                    metadata.clone(),
                ) {
                    let quote = aeris_market_data::TopOfBookQuote {
                        metadata,
                        bid: snapshot
                            .bids
                            .first()
                            .map(|l| aeris_market_data::QuoteLevel {
                                price: l.price,
                                quantity: Some(l.quantity),
                                order_count: l.order_count,
                            }),
                        ask: snapshot
                            .asks
                            .first()
                            .map(|l| aeris_market_data::QuoteLevel {
                                price: l.price,
                                quantity: Some(l.quantity),
                                order_count: l.order_count,
                            }),
                    };
                    emit_data(
                        sink,
                        events,
                        CryptoRealtimeEvent::Depth(generation, snapshot),
                        wake,
                    );
                    if quote.bid.is_some() || quote.ask.is_some() {
                        emit_data(
                            sink,
                            events,
                            CryptoRealtimeEvent::Quote(generation, quote),
                            wake,
                        );
                    }
                }
                state.last_book_publish = now;
                state.book_dirty = false;
            }

            if !any_socket {
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[derive(Default)]
struct VenueSockets {
    bin_fut_market: Option<MarketSocket>,
    bin_fut_public: Option<MarketSocket>,
    bin_spot: Option<MarketSocket>,
    byb_lin: Option<MarketSocket>,
    byb_spot: Option<MarketSocket>,
    okx: Option<MarketSocket>,
    hl: Option<MarketSocket>,
}

impl VenueSockets {
    fn close_all(&mut self) {
        for slot in [
            &mut self.bin_fut_market,
            &mut self.bin_fut_public,
            &mut self.bin_spot,
            &mut self.byb_lin,
            &mut self.byb_spot,
            &mut self.okx,
            &mut self.hl,
        ] {
            if let Some(mut sock) = slot.take() {
                sock.close();
            }
        }
    }
}

fn drain_socket(slot: &mut Option<MarketSocket>, mut on_text: impl FnMut(&str)) {
    for idx in 0..MAX_FRAMES_PER_POLL {
        let timeout = if idx == 0 {
            READ_TIMEOUT
        } else {
            DRAIN_READ_TIMEOUT
        };
        let Some(sock) = slot.as_mut() else {
            return;
        };
        match sock.read_event(Instant::now() + timeout) {
            Ok(MarketSocketEvent::Text(text)) => on_text(&text),
            Ok(MarketSocketEvent::Pong) => {}
            Err(err) if err.is_read_timeout() => return,
            Err(_) => {
                if let Some(mut s) = slot.take() {
                    s.close();
                }
                return;
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
fn connect_constituent_venues(
    states: &BTreeMap<String, AggregatedInstrumentState>,
    stop: &Arc<AtomicBool>,
    sockets: &mut VenueSockets,
) {
    let mut bin_fut_market_streams = Vec::new();
    let mut bin_fut_public_streams = Vec::new();
    let mut bin_spot_streams = Vec::new();
    let mut byb_lin_topics = Vec::new();
    let mut byb_spot_topics = Vec::new();
    let mut okx_args = Vec::new();
    let mut hl_coins = Vec::new();

    for state in states.values() {
        if let Some((kind, sym)) = &state.meta.binance {
            let lower = sym.to_ascii_lowercase();
            match kind {
                BinanceMarketKind::UsdmPerp => {
                    bin_fut_market_streams.push(format!("{lower}@aggTrade"));
                    bin_fut_public_streams.push(format!("{lower}@depth@100ms"));
                }
                BinanceMarketKind::Spot => {
                    bin_spot_streams.push(format!("{lower}@aggTrade"));
                    bin_spot_streams.push(format!("{lower}@depth@100ms"));
                }
            }
        }
        if let Some((kind, sym)) = &state.meta.bybit {
            let (target, depth) = match kind {
                BybitMarketKind::Linear => (&mut byb_lin_topics, 500),
                BybitMarketKind::Spot => (&mut byb_spot_topics, 200),
            };
            target.push(format!("publicTrade.{sym}"));
            target.push(format!("orderbook.{depth}.{sym}"));
        }
        if let Some((_, inst_id, _)) = &state.meta.okx {
            okx_args.push(("trades".to_string(), inst_id.clone()));
            okx_args.push(("books".to_string(), inst_id.clone()));
        }
        if let Some(coin) = &state.meta.hyperliquid {
            hl_coins.push(coin.clone());
        }
    }

    if !bin_fut_market_streams.is_empty()
        && let Ok((mut sock, _)) =
            MarketSocket::connect(BINANCE_FUTURES_MARKET_WS_URL, CONNECT_TIMEOUT, stop)
        && sock
            .send_text(&aeris_binance_market_adapter::build_subscribe(
                &bin_fut_market_streams,
                1,
            ))
            .is_ok()
    {
        sockets.bin_fut_market = Some(sock);
    }
    if !bin_fut_public_streams.is_empty()
        && let Ok((mut sock, _)) =
            MarketSocket::connect(BINANCE_FUTURES_PUBLIC_WS_URL, CONNECT_TIMEOUT, stop)
        && sock
            .send_text(&aeris_binance_market_adapter::build_subscribe(
                &bin_fut_public_streams,
                1,
            ))
            .is_ok()
    {
        sockets.bin_fut_public = Some(sock);
    }
    if !bin_spot_streams.is_empty()
        && let Ok((mut sock, _)) = MarketSocket::connect(BINANCE_SPOT_WS_URL, CONNECT_TIMEOUT, stop)
        && sock
            .send_text(&aeris_binance_market_adapter::build_subscribe(
                &bin_spot_streams,
                1,
            ))
            .is_ok()
    {
        sockets.bin_spot = Some(sock);
    }
    if !byb_lin_topics.is_empty()
        && let Ok((mut sock, _)) = MarketSocket::connect(BYBIT_LINEAR_WS_URL, CONNECT_TIMEOUT, stop)
        && byb_lin_topics.chunks(10).all(|chunk| {
            sock.send_text(&aeris_bybit_market_adapter::build_subscribe(chunk))
                .is_ok()
        })
    {
        sockets.byb_lin = Some(sock);
    }
    if !byb_spot_topics.is_empty()
        && let Ok((mut sock, _)) = MarketSocket::connect(BYBIT_SPOT_WS_URL, CONNECT_TIMEOUT, stop)
        && byb_spot_topics.chunks(10).all(|chunk| {
            sock.send_text(&aeris_bybit_market_adapter::build_subscribe(chunk))
                .is_ok()
        })
    {
        sockets.byb_spot = Some(sock);
    }
    if !okx_args.is_empty()
        && let Ok((mut sock, _)) = MarketSocket::connect(OKX_PUBLIC_WS_URL, CONNECT_TIMEOUT, stop)
        && sock
            .send_text(&aeris_okx_market_adapter::build_subscribe(&okx_args))
            .is_ok()
    {
        sockets.okx = Some(sock);
    }
    if !hl_coins.is_empty()
        && let Ok((mut sock, _)) = MarketSocket::connect(HYPERLIQUID_WS_URL, CONNECT_TIMEOUT, stop)
    {
        let mut ok = true;
        for coin in &hl_coins {
            if let Ok(frame) = aeris_hyperliquid_market_adapter::build_trades_subscription(coin)
                && sock.send_text(&frame).is_err()
            {
                ok = false;
                break;
            }
            if let Ok(frame) = aeris_hyperliquid_market_adapter::build_l2_subscription(coin)
                && sock.send_text(&frame).is_err()
            {
                ok = false;
                break;
            }
        }
        if ok {
            sockets.hl = Some(sock);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn poll_binance_agg(
    kind: BinanceMarketKind,
    text: &str,
    states: &mut BTreeMap<String, AggregatedInstrumentState>,
    http: &mut BinanceHttpClient,
    dedup: &mut TradeDedupRing<String>,
    sink: ProviderEventSink,
    events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
    wake: &ProviderCoordinatorWake,
    stop: &Arc<AtomicBool>,
    generation: u64,
    seq: &mut u64,
) {
    let Ok(parsed) = aeris_binance_market_adapter::parse_ws_frame(text) else {
        return;
    };
    let received_nanos = now_unix_nanos();
    match parsed {
        BinanceWsEvent::AggTrade(raw) => {
            for state in states.values_mut() {
                if state.meta.binance.as_ref().is_some_and(|(k, _)| *k == kind) {
                    *seq = seq.saturating_add(1).max(1);
                    if let Ok((sym, id, trade)) = aeris_binance_market_adapter::decode_agg_trade(
                        &raw,
                        &state.meta.instrument_id,
                        AGGREGATED_ENTITLEMENT_ID,
                        generation,
                        *seq,
                        received_nanos,
                    ) && state.meta.binance.as_ref().is_some_and(|(_, s)| s == &sym)
                    {
                        if dedup.insert(format!("bin:{sym}:{id}")) {
                            let stamped = restamp_trade(
                                trade,
                                "bin",
                                &state.meta.instrument_id,
                                generation,
                                *seq,
                            );
                            state.record_trade(&stamped, sink, events, wake, generation);
                            emit_data(
                                sink,
                                events,
                                CryptoRealtimeEvent::Trades(generation, vec![stamped]),
                                wake,
                            );
                        }
                        break;
                    }
                }
            }
        }
        BinanceWsEvent::DepthUpdate(raw) => {
            let Ok(delta) = aeris_binance_market_adapter::decode_depth_update(&raw) else {
                return;
            };
            let sym = delta.symbol.clone();
            let Some(state) = states
                .values_mut()
                .find(|s| s.meta.binance.as_ref() == Some(&(kind, sym.clone())))
            else {
                return;
            };
            match state.binance_book.on_ws_delta(delta) {
                BinanceBookOutcome::Updated => {
                    state.binance_updated_at = Some(Instant::now());
                    state.book_dirty = true;
                }
                BinanceBookOutcome::BufferedAwaitingSnapshot | BinanceBookOutcome::NeedsResync => {
                    let can = state
                        .last_resync
                        .is_none_or(|at| at.elapsed() >= RESYNC_COOLDOWN);
                    if can {
                        state.last_resync = Some(Instant::now());
                        if let Ok(snap) = http.fetch_depth_snapshot(
                            kind,
                            &sym,
                            1_000,
                            BinanceHttpConfig::default(),
                            stop,
                        ) && state.binance_book.apply_rest_snapshot(&snap).is_ok()
                        {
                            state.binance_updated_at = Some(Instant::now());
                            state.book_dirty = true;
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn poll_bybit_agg(
    kind: BybitMarketKind,
    text: &str,
    states: &mut BTreeMap<String, AggregatedInstrumentState>,
    dedup: &mut TradeDedupRing<String>,
    sink: ProviderEventSink,
    events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
    wake: &ProviderCoordinatorWake,
    generation: u64,
    seq: &mut u64,
) {
    let Ok(parsed) = aeris_bybit_market_adapter::parse_ws_frame(text) else {
        return;
    };
    let received_nanos = now_unix_nanos();
    match parsed {
        BybitWsEvent::Trades { symbol, data } => {
            let Some(state) = states
                .values_mut()
                .find(|s| s.meta.bybit.as_ref() == Some(&(kind, symbol.clone())))
            else {
                return;
            };
            if let Ok(decoded) = aeris_bybit_market_adapter::decode_public_trades(
                &data,
                &state.meta.instrument_id,
                AGGREGATED_ENTITLEMENT_ID,
                generation,
                seq,
                received_nanos,
            ) {
                let fresh: Vec<_> = decoded
                    .into_iter()
                    .filter_map(|(id, t)| {
                        dedup.insert(format!("byb:{symbol}:{id}")).then(|| {
                            restamp_trade(t, "byb", &state.meta.instrument_id, generation, *seq)
                        })
                    })
                    .collect();
                if !fresh.is_empty() {
                    for trade in &fresh {
                        state.record_trade(trade, sink, events, wake, generation);
                    }
                    emit_data(
                        sink,
                        events,
                        CryptoRealtimeEvent::Trades(generation, fresh),
                        wake,
                    );
                }
            }
        }
        BybitWsEvent::Orderbook {
            symbol,
            msg_type,
            ts_ms,
            data,
        } => {
            let Some(state) = states
                .values_mut()
                .find(|s| s.meta.bybit.as_ref() == Some(&(kind, symbol.clone())))
            else {
                return;
            };
            if let Ok(msg) = aeris_bybit_market_adapter::decode_orderbook(&data, &msg_type, ts_ms)
                && state.bybit_book.apply(&msg).unwrap_or(false)
            {
                state.bybit_updated_at = Some(Instant::now());
                state.book_dirty = true;
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn poll_okx_agg(
    text: &str,
    states: &mut BTreeMap<String, AggregatedInstrumentState>,
    dedup: &mut TradeDedupRing<String>,
    sink: ProviderEventSink,
    events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
    wake: &ProviderCoordinatorWake,
    generation: u64,
    seq: &mut u64,
) {
    let Ok(parsed) = aeris_okx_market_adapter::parse_ws_frame(text) else {
        return;
    };
    let received_nanos = now_unix_nanos();
    match parsed {
        OkxWsEvent::Trades { inst_id, data } => {
            let Some(state) = states
                .values_mut()
                .find(|s| s.meta.okx.as_ref().is_some_and(|(_, id, _)| id == &inst_id))
            else {
                return;
            };
            let ct_val = state.meta.okx.as_ref().map_or(ONE_FIXED, |(_, _, v)| *v);
            if let Ok(decoded) = aeris_okx_market_adapter::decode_trades(
                &data,
                &state.meta.instrument_id,
                AGGREGATED_ENTITLEMENT_ID,
                ct_val,
                generation,
                seq,
                received_nanos,
            ) {
                let fresh: Vec<_> = decoded
                    .into_iter()
                    .filter_map(|(id, t)| {
                        dedup.insert(format!("okx:{inst_id}:{id}")).then(|| {
                            restamp_trade(t, "okx", &state.meta.instrument_id, generation, *seq)
                        })
                    })
                    .collect();
                if !fresh.is_empty() {
                    for trade in &fresh {
                        state.record_trade(trade, sink, events, wake, generation);
                    }
                    emit_data(
                        sink,
                        events,
                        CryptoRealtimeEvent::Trades(generation, fresh),
                        wake,
                    );
                }
            }
        }
        OkxWsEvent::Books {
            inst_id,
            action,
            data,
        } => {
            let Some(state) = states
                .values_mut()
                .find(|s| s.meta.okx.as_ref().is_some_and(|(_, id, _)| id == &inst_id))
            else {
                return;
            };
            let ct_val = state.meta.okx.as_ref().map_or(ONE_FIXED, |(_, _, v)| *v);
            if let Ok(msgs) = aeris_okx_market_adapter::decode_books(&data, &action, ct_val) {
                for msg in msgs {
                    if state.okx_book.apply(&msg) == OkxBookApplyOutcome::Updated {
                        state.okx_updated_at = Some(Instant::now());
                        state.book_dirty = true;
                    }
                }
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn poll_hl_agg(
    text: &str,
    states: &mut BTreeMap<String, AggregatedInstrumentState>,
    dedup: &mut TradeDedupRing<String>,
    sink: ProviderEventSink,
    events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
    wake: &ProviderCoordinatorWake,
    generation: u64,
    seq: &mut u64,
) {
    let Ok(parsed) = aeris_hyperliquid_market_adapter::parse_ws_frame(text) else {
        return;
    };
    let received_nanos = now_unix_nanos();
    match parsed {
        HlWsEvent::Trades { coin, trades: raw } => {
            if let Some(state) = states
                .values_mut()
                .find(|s| s.meta.hyperliquid.as_deref() == Some(coin.as_str()))
            {
                let next_seq = seq.saturating_add(1).max(1);
                if let Ok(batch) = aeris_hyperliquid_market_adapter::decode_trades_batch(
                    &raw,
                    &coin,
                    &state.meta.instrument_id,
                    AGGREGATED_ENTITLEMENT_ID,
                    generation,
                    received_nanos,
                    next_seq,
                ) {
                    let mut fresh = Vec::new();
                    for t in batch.trades {
                        if dedup.insert(format!("hl:{}", t.trade_id)) {
                            *seq = seq.saturating_add(1).max(1);
                            fresh.push(restamp_trade(
                                t,
                                "hl",
                                &state.meta.instrument_id,
                                generation,
                                *seq,
                            ));
                        }
                    }
                    if !fresh.is_empty() {
                        for trade in &fresh {
                            state.record_trade(trade, sink, events, wake, generation);
                        }
                        emit_data(
                            sink,
                            events,
                            CryptoRealtimeEvent::Trades(generation, fresh),
                            wake,
                        );
                    }
                }
            }
        }
        HlWsEvent::Book { coin, book: raw } => {
            if let Some(state) = states
                .values_mut()
                .find(|s| s.meta.hyperliquid.as_deref() == Some(coin.as_str()))
                && let Ok(decoded) = aeris_hyperliquid_market_adapter::decode_book_snapshot(
                    &raw,
                    &coin,
                    &state.meta.instrument_id,
                    AGGREGATED_ENTITLEMENT_ID,
                    generation,
                    1,
                    received_nanos,
                )
            {
                let bids: Vec<BookLevelUpdate> = decoded
                    .snapshot
                    .bids
                    .iter()
                    .map(|l| BookLevelUpdate {
                        price: l.price,
                        quantity: l.quantity,
                        order_count: l.order_count,
                    })
                    .collect();
                let asks: Vec<BookLevelUpdate> = decoded
                    .snapshot
                    .asks
                    .iter()
                    .map(|l| BookLevelUpdate {
                        price: l.price,
                        quantity: l.quantity,
                        order_count: l.order_count,
                    })
                    .collect();
                if state.hl_book.apply_snapshot(&bids, &asks).is_ok() {
                    state.hl_updated_at = Some(Instant::now());
                    state.book_dirty = true;
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregated_live_bar_merges_trades_rolls_over_and_ignores_stale_cross_venue_ticks() {
        let period = BarPeriod::time(60).unwrap();
        let mut live = AggregatedLiveBar::new(period);

        // First trade opens the 1m bucket at t=60s.
        let t0 = 60_000_000_000_i64;
        let emitted = live.on_trade(t0 + 5_000_000_000, 100, 2);
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].exchange_timestamp_unix_nanos, t0);
        assert_eq!(emitted[0].open, 100);
        assert_eq!(emitted[0].volume, 2);

        // Second & third trades in same minute update H/L/C/V and mark dirty.
        assert_eq!(live.on_trade(t0 + 10_000_000_000, 110, 3).len(), 0);
        assert_eq!(live.on_trade(t0 + 20_000_000_000, 95, 5).len(), 0);
        assert!(live.dirty);
        let cur = live.current.unwrap();
        assert_eq!(
            (cur.open, cur.high, cur.low, cur.close, cur.volume),
            (100, 110, 95, 95, 10)
        );

        // Trade in t=180s (skipping 120s) flushes 60s, gap-fills 120s, and opens 180s.
        let t2 = 180_000_000_000_i64;
        let rolled = live.on_trade(t2 + 1_000_000_000, 105, 4);
        assert_eq!(rolled.len(), 3);
        assert_eq!(rolled[0].exchange_timestamp_unix_nanos, t0);
        assert_eq!(rolled[0].volume, 10);
        assert_eq!(rolled[1].exchange_timestamp_unix_nanos, 120_000_000_000);
        assert_eq!(rolled[1].close, 95);
        assert_eq!(rolled[1].volume, 0);
        assert_eq!(rolled[2].exchange_timestamp_unix_nanos, t2);
        assert_eq!(rolled[2].open, 105);

        // Late cross-exchange trade from t=179.9s is ignored so open timestamps never regress.
        assert_eq!(live.on_trade(t2 - 100_000_000, 99, 1).len(), 0);
        assert_eq!(live.current.unwrap().exchange_timestamp_unix_nanos, t2);
        assert_eq!(live.current.unwrap().volume, 4);
    }

    #[test]
    fn forming_seed_preserves_mid_bar_open_high_low_before_or_after_first_trade() {
        let period = BarPeriod::time(60).unwrap();
        let t0 = 60_000_000_000_i64;
        let seed = MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 60,
            exchange_timestamp_unix_nanos: t0,
            open: 60_611,
            high: 60_660,
            low: 60_610,
            close: 60_641,
            volume: 158,
        };

        // Case 1: History seed arrives before first live trade.
        let mut live_before = AggregatedLiveBar::new(period);
        live_before.merge_seed(seed);
        assert!(!live_before.dirty);
        assert_eq!(
            live_before.on_trade(t0 + 58_000_000_000, 60_645, 2).len(),
            0
        );
        let cur = live_before.current.unwrap();
        assert_eq!(
            (cur.open, cur.high, cur.low, cur.close, cur.volume),
            (60_611, 60_660, 60_610, 60_645, 160)
        );

        // Case 2: Live trade arrives at :58 before history seed finishes.
        let mut live_after = AggregatedLiveBar::new(period);
        assert_eq!(live_after.on_trade(t0 + 58_000_000_000, 60_641, 1).len(), 1);
        live_after.merge_seed(seed);
        assert!(live_after.dirty);
        let cur = live_after.current.unwrap();
        assert_eq!(
            (cur.open, cur.high, cur.low, cur.close, cur.volume),
            (60_611, 60_660, 60_610, 60_641, 158)
        );
    }
}
