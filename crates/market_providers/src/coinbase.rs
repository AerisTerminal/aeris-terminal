//! Coinbase Advanced Trade public market-provider registration, catalog worker,
//! history fetcher, and realtime WebSocket (`heartbeats`, `market_trades`, `level2`) worker.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender},
    },
    time::{Duration, Instant},
};

use aeris_coinbase_market_adapter::{
    COINBASE_WS_URL, CoinbaseBookApplyOutcome, CoinbaseBookState, CoinbaseCatalog,
    CoinbaseHttpClient, CoinbaseHttpConfig, CoinbaseMarketKind, CoinbaseWsEvent, build_subscribe,
    build_unsubscribe, coinbase_interval_for_period, decode_l2_events, decode_ws_trades,
    parse_provider_symbol, parse_ws_frame,
};
use aeris_crypto_common::{CryptoError, CryptoResult, TradeDedupRing};
use aeris_market_data::BarPeriod;
use aeris_market_engine::{
    MarketStream, ProviderCapabilities as EngineProviderCapabilities, StreamRequirements,
    fence::{Fenced, ProviderTag},
};
use aeris_market_runtime::{
    ProviderCatalogRejectionReason, ProviderCatalogSymbol, ProviderConnectionKind,
    ProviderDailyBarCadence, ProviderFeedLevel, ProviderPresentationDescriptor,
    plugin::{
        AlertDemandUpdate, CalendarBarStamp, CandleDemandPolicy, CatalogPublisher,
        CatalogSessionBinding, ConcurrencyLimits, DemandFlushPolicy, DepthCapabilities, LiveModel,
        ProviderCapabilities, ProviderCoordinatorWake, ProviderDescriptor, ProviderEventSink,
        ProviderHistoryCapabilities, ProviderRecoveryPolicy, ProviderRuntimeSpec, QuoteCapability,
        ReconnectPolicy, TradeDemandPolicy,
    },
};
use aeris_platform_runtime::{MarketSocket, MarketSocketEvent};

use crate::crypto_runtime::{
    CATALOG_POLL_INTERVAL, CATALOG_REFRESH_INTERVAL, CONNECT_TIMEOUT, CryptoCatalogControl,
    CryptoCatalogEvent, CryptoDemand, CryptoHistorySource, CryptoInstrumentDemand,
    CryptoRealtimeControl, CryptoRealtimeEvent, READ_TIMEOUT, SharedCryptoCatalogs,
    build_install_instrument, crypto_provider_spec, emit_data, emit_lifecycle, now_unix_nanos,
    publish_catalog_event, reject_catalog,
};

pub(crate) const COINBASE_PUBLIC_ACCOUNT_ID: &str = "coinbase-public";
pub(crate) const COINBASE_PUBLIC_ENTITLEMENT_ID: &str = "coinbase-public";
const CATALOG_WORKER_NAME: &str = "aeris-coinbase-catalog";
const PROVIDER_WORKER_NAME: &str = "aeris-coinbase-provider";
const SILENCE_TIMEOUT: Duration = Duration::from_secs(45);
const BACKFILL_TRADE_LIMIT: usize = 500;
const TRADE_DEDUP_CAPACITY: usize = 8_192;

pub(crate) const COINBASE_PRESENTATION: ProviderPresentationDescriptor =
    ProviderPresentationDescriptor {
        id: "coinbase",
        display_name: "Coinbase",
        chart_interval_labels: &["1m", "5m", "15m", "30m", "1h", "2h", "6h", "1D"],
        daily_bars: ProviderDailyBarCadence::FixedDay,
        search_hint: "Search Coinbase spot pairs",
        logo_key: "coinbase",
        broker_venue_id: None,
        attribution: None,
        catalog_symbol: ProviderCatalogSymbol::ProviderSymbol,
        selection_entitlement_id: COINBASE_PUBLIC_ENTITLEMENT_ID,
        catalog_refresh_on_startup: true,
        ready_label_suffix: "",
        feed_level: ProviderFeedLevel::PriceLevels,
        search_categories_available: false,
        market_screen_available: false,
        market_scan_available: false,
        trades_available: true,
        connection_kind: ProviderConnectionKind::Public,
    };

pub(crate) const COINBASE_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: "coinbase",
    presentation: &COINBASE_PRESENTATION,
    login_schema: None,
    capabilities: COINBASE_CAPABILITIES,
    demand_flush: DemandFlushPolicy::SessionManaged,
};

const COINBASE_CAPABILITIES: ProviderCapabilities = ProviderCapabilities {
    account_id: COINBASE_PUBLIC_ACCOUNT_ID,
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
        worker: "aeris-coinbase-history",
        supported_period: coinbase_supported_period,
    },
    recovery_policy: ProviderRecoveryPolicy::WorkerReconcilesDemand,
    alert_demand_update: AlertDemandUpdate::MarkDirty,
    live_model: LiveModel::TradeBuilt,
    calendar_bar_stamp: CalendarBarStamp::TradingDateMidnightUtc,
    overflow_recovery_detail: "Local market event queue overflow; repairing continuity",
    candle_requires_connected: false,
    candle_correction_detail: "Coinbase candle replacement requires covering history",
    candle_wire_interval: None,
    candle_demand_policy: CandleDemandPolicy::SessionManaged,
    trade_demand_policy: TradeDemandPolicy::SessionManaged,
    instrument_missing_detail: "Coinbase instrument is not installed",
    catalog_session_binding: CatalogSessionBinding::CurrentSession,
    maximum_order_level_instruments: 0,
    recovers_silence: true,
};

pub(crate) fn provider_spec(
    realtime_enabled: bool,
    shared_catalogs: Arc<SharedCryptoCatalogs>,
) -> ProviderRuntimeSpec {
    crypto_provider_spec(
        COINBASE_DESCRIPTOR,
        CryptoHistorySource::Coinbase(CoinbaseHttpClient::default()),
        realtime_enabled,
        CATALOG_WORKER_NAME,
        PROVIDER_WORKER_NAME,
        "Coinbase live session is recovering",
        coinbase_interval_for_period,
        move |controls, events, ws_gen, stop| {
            run_catalog(controls, events, ws_gen, stop, &shared_catalogs);
        },
        run_realtime,
    )
}

pub(crate) fn coinbase_supported_period(period: BarPeriod) -> Result<(), String> {
    coinbase_interval_for_period(period)
        .map(|_| ())
        .map_err(|_| "Provider history is unavailable for this interval".to_string())
}

#[allow(clippy::too_many_lines)]
fn run_catalog(
    controls: &Receiver<CryptoCatalogControl>,
    events: &CatalogPublisher<CryptoCatalogEvent>,
    ws_generation: &Arc<AtomicU64>,
    stop: &Arc<AtomicBool>,
    shared_catalogs: &SharedCryptoCatalogs,
) {
    let mut catalog: Option<Arc<CoinbaseCatalog>> = None;
    let mut fetched_at: Option<Instant> = None;
    let mut selection_generation: u64 = 0;
    let mut next_refresh = Instant::now();

    // Warm the public catalog on startup so the symbol browser populates instantly.
    if !stop.load(Ordering::Acquire)
        && let Ok(loaded) = shared_catalogs.get_or_fetch_coinbase(false)
    {
        catalog = Some(loaded);
        fetched_at = Some(Instant::now());
        next_refresh = Instant::now() + CATALOG_REFRESH_INTERVAL;
    }

    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let wait = if catalog.is_none() {
            CATALOG_POLL_INTERVAL
        } else {
            next_refresh
                .saturating_duration_since(Instant::now())
                .min(CATALOG_POLL_INTERVAL)
        };
        match controls.recv_timeout(wait) {
            Ok(CryptoCatalogControl::Search(search)) => {
                if catalog.is_none() {
                    match shared_catalogs.get_or_fetch_coinbase(false) {
                        Ok(loaded) => {
                            catalog = Some(loaded);
                            fetched_at = Some(Instant::now());
                            next_refresh = Instant::now() + CATALOG_REFRESH_INTERVAL;
                        }
                        Err(_error) => {
                            if reject_catalog(
                                events,
                                search.consumer_id,
                                "coinbase",
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
                if let Some(cat) = catalog.as_ref() {
                    let result = cat.search(
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
                if catalog.is_none() {
                    match shared_catalogs.get_or_fetch_coinbase(false) {
                        Ok(loaded) => {
                            catalog = Some(loaded);
                            fetched_at = Some(Instant::now());
                            next_refresh = Instant::now() + CATALOG_REFRESH_INTERVAL;
                        }
                        Err(_error) => {
                            if reject_catalog(
                                events,
                                select.consumer_id,
                                "coinbase",
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
                let Some(item) = catalog.as_ref().and_then(|c| c.get(&select.symbol)) else {
                    if reject_catalog(
                        events,
                        select.consumer_id,
                        "coinbase",
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
                let install = build_install_instrument(
                    "coinbase",
                    COINBASE_PUBLIC_ENTITLEMENT_ID,
                    ws_generation.load(Ordering::Acquire),
                    selection_generation,
                    item.instrument_id.clone(),
                    item.provider_symbol.clone(),
                    item.display_symbol.clone(),
                    item.kind.exchange_label().to_string(),
                    item.quantity_decimals,
                    item.tick_size_fixed,
                    item.kind == CoinbaseMarketKind::Spot,
                    &item.quote_asset,
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
        if catalog.is_some() && Instant::now() >= next_refresh {
            match shared_catalogs.get_or_fetch_coinbase(true) {
                Ok(loaded) => {
                    catalog = Some(loaded);
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

struct CoinbaseSocket {
    socket: Option<MarketSocket>,
    active_trades: BTreeSet<String>,
    active_l2: BTreeSet<String>,
    last_sequence_num: Option<u64>,
    last_activity: Instant,
}

impl CoinbaseSocket {
    fn new() -> Self {
        Self {
            socket: None,
            active_trades: BTreeSet::new(),
            active_l2: BTreeSet::new(),
            last_sequence_num: None,
            last_activity: Instant::now(),
        }
    }

    fn close(&mut self) {
        if let Some(mut sock) = self.socket.take() {
            sock.close();
        }
        self.active_trades.clear();
        self.active_l2.clear();
        self.last_sequence_num = None;
    }

    fn reconcile(
        &mut self,
        desired_trades: &BTreeSet<String>,
        desired_l2: &BTreeSet<String>,
        stop: &Arc<AtomicBool>,
    ) -> CryptoResult<()> {
        if desired_trades.is_empty() && desired_l2.is_empty() {
            self.close();
            return Ok(());
        }
        if self.socket.is_none() {
            let (mut sock, _) = MarketSocket::connect(COINBASE_WS_URL, CONNECT_TIMEOUT, stop)
                .map_err(|e| CryptoError::Transport(e.to_string()))?;
            // Subscribe to heartbeats immediately so Coinbase keeps the socket alive
            // (Coinbase disconnects connections without a subscription within 5 seconds).
            sock.send_text(&build_subscribe("heartbeats", &[]))
                .map_err(|e| CryptoError::Transport(e.to_string()))?;
            self.socket = Some(sock);
            self.last_activity = Instant::now();
            self.active_trades.clear();
            self.active_l2.clear();
            self.last_sequence_num = None;
        }
        reconcile_channel(
            self.socket.as_mut(),
            "market_trades",
            &mut self.active_trades,
            desired_trades,
        )?;
        reconcile_channel(
            self.socket.as_mut(),
            "level2",
            &mut self.active_l2,
            desired_l2,
        )?;
        Ok(())
    }

    fn resync_l2(&mut self, product_ids: &[String]) -> CryptoResult<()> {
        if product_ids.is_empty() {
            return Ok(());
        }
        if let Some(sock) = self.socket.as_mut() {
            sock.send_text(&build_unsubscribe("level2", product_ids))
                .map_err(|e| CryptoError::Transport(e.to_string()))?;
            sock.send_text(&build_subscribe("level2", product_ids))
                .map_err(|e| CryptoError::Transport(e.to_string()))?;
        }
        Ok(())
    }
}

fn reconcile_channel(
    socket: Option<&mut MarketSocket>,
    channel: &str,
    active: &mut BTreeSet<String>,
    desired: &BTreeSet<String>,
) -> CryptoResult<()> {
    let Some(sock) = socket else {
        return Ok(());
    };
    let to_unsub: Vec<String> = active.difference(desired).cloned().collect();
    let to_sub: Vec<String> = desired.difference(active).cloned().collect();
    if !to_unsub.is_empty() {
        sock.send_text(&build_unsubscribe(channel, &to_unsub))
            .map_err(|e| CryptoError::Transport(e.to_string()))?;
        for id in to_unsub {
            active.remove(&id);
        }
    }
    if !to_sub.is_empty() {
        sock.send_text(&build_subscribe(channel, &to_sub))
            .map_err(|e| CryptoError::Transport(e.to_string()))?;
        for id in to_sub {
            active.insert(id);
        }
    }
    Ok(())
}

fn find_coinbase_instrument<'a>(
    list: &'a [CryptoInstrumentDemand],
    product_id: &str,
) -> Option<&'a CryptoInstrumentDemand> {
    list.iter().find(|item| {
        parse_provider_symbol(&item.provider_symbol).is_ok_and(|sym| sym.product_id == product_id)
    })
}

#[allow(clippy::too_many_lines)]
fn run_realtime(
    controls: &Receiver<CryptoRealtimeControl>,
    events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
    stop: &Arc<AtomicBool>,
    ws_generation: &Arc<AtomicU64>,
    wake: &ProviderCoordinatorWake,
) {
    let mut demand = CryptoDemand::default();
    let mut http_client = CoinbaseHttpClient::default();

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

        let mut ws = CoinbaseSocket::new();
        let mut books: BTreeMap<String, CoinbaseBookState> = BTreeMap::new();
        let mut trade_dedup: BTreeMap<String, TradeDedupRing<String>> = BTreeMap::new();
        let mut backfilled_trades: BTreeSet<String> = BTreeSet::new();
        let mut seq: u64 = 0;
        let mut demand_dirty = true;
        let mut failed = false;

        'session: loop {
            if stop.load(Ordering::Acquire) {
                ws.close();
                return;
            }
            while let Ok(ctrl) = controls.try_recv() {
                match ctrl {
                    CryptoRealtimeControl::Connect(_) | CryptoRealtimeControl::Stop => {
                        ws.close();
                        break 'session;
                    }
                    CryptoRealtimeControl::Subscribe(next) => {
                        demand = next;
                        demand_dirty = true;
                    }
                }
            }

            if demand_dirty {
                let mut desired_trades = BTreeSet::new();
                for t in &demand.trades {
                    if let Ok(sym) = parse_provider_symbol(&t.provider_symbol) {
                        desired_trades.insert(sym.product_id);
                    }
                }
                let mut desired_l2 = BTreeSet::new();
                for inst in demand.quotes.iter().chain(&demand.books) {
                    if let Ok(sym) = parse_provider_symbol(&inst.provider_symbol) {
                        desired_l2.insert(sym.product_id);
                    }
                }
                books.retain(|id, _| desired_l2.contains(id));
                if ws.reconcile(&desired_trades, &desired_l2, stop).is_err() {
                    failed = true;
                    break 'session;
                }
                backfill_coinbase_trades(
                    &demand,
                    &mut backfilled_trades,
                    &mut http_client,
                    &mut trade_dedup,
                    sink,
                    events,
                    wake,
                    stop,
                    generation,
                    &mut seq,
                );
                demand_dirty = false;
            }

            if ws.socket.is_none() {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            if ws.last_activity.elapsed() > SILENCE_TIMEOUT {
                failed = true;
                break 'session;
            }

            let read = match ws.socket.as_mut() {
                Some(sock) => sock.read_event(Instant::now() + READ_TIMEOUT),
                None => continue,
            };
            match read {
                Ok(MarketSocketEvent::Text(text)) => {
                    ws.last_activity = Instant::now();
                    handle_coinbase_frame(
                        &mut ws,
                        &text,
                        &demand,
                        &mut books,
                        &mut trade_dedup,
                        sink,
                        events,
                        wake,
                        generation,
                        &mut seq,
                    );
                }
                Ok(MarketSocketEvent::Pong) => {
                    ws.last_activity = Instant::now();
                    emit_data(
                        sink,
                        events,
                        CryptoRealtimeEvent::Heartbeat(generation, None),
                        wake,
                    );
                }
                Err(err) if err.is_read_timeout() => {}
                Err(_) => {
                    failed = true;
                    break 'session;
                }
            }
        }

        ws.close();
        if failed {
            if !emit_lifecycle(
                sink,
                events,
                CryptoRealtimeEvent::Recovering(generation),
                wake,
                stop,
            ) {
                return;
            }
            if !emit_lifecycle(
                sink,
                events,
                CryptoRealtimeEvent::Disconnected(generation),
                wake,
                stop,
            ) {
                return;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn backfill_coinbase_trades(
    demand: &CryptoDemand,
    backfilled: &mut BTreeSet<String>,
    http_client: &mut CoinbaseHttpClient,
    trade_dedup: &mut BTreeMap<String, TradeDedupRing<String>>,
    sink: ProviderEventSink,
    events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
    wake: &ProviderCoordinatorWake,
    stop: &Arc<AtomicBool>,
    generation: u64,
    seq: &mut u64,
) {
    const CHUNK_SIZE: usize = 500;
    backfilled.retain(|sym| demand.trades.iter().any(|t| t.provider_symbol == *sym));
    for inst in &demand.trades {
        if stop.load(Ordering::Acquire) {
            return;
        }
        if !backfilled.insert(inst.provider_symbol.clone()) {
            continue;
        }
        let Ok(sym) = parse_provider_symbol(&inst.provider_symbol) else {
            continue;
        };
        let Ok(trades) = http_client.fetch_recent_trades(
            &sym.product_id,
            &inst.instrument_id,
            &inst.entitlement_id,
            generation,
            seq,
            now_unix_nanos(),
            BACKFILL_TRADE_LIMIT,
            CoinbaseHttpConfig::default(),
            stop,
        ) else {
            continue;
        };
        let ring = trade_dedup
            .entry(sym.product_id)
            .or_insert_with(|| TradeDedupRing::new(TRADE_DEDUP_CAPACITY));
        let mut batch = Vec::with_capacity(CHUNK_SIZE);
        for (trade_id, trade) in trades {
            if ring.insert(trade_id) {
                batch.push(trade);
                if batch.len() >= CHUNK_SIZE {
                    emit_data(
                        sink,
                        events,
                        CryptoRealtimeEvent::Trades(generation, std::mem::take(&mut batch)),
                        wake,
                    );
                }
            }
        }
        if !batch.is_empty() {
            emit_data(
                sink,
                events,
                CryptoRealtimeEvent::Trades(generation, batch),
                wake,
            );
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn handle_coinbase_frame(
    ws: &mut CoinbaseSocket,
    text: &str,
    demand: &CryptoDemand,
    books: &mut BTreeMap<String, CoinbaseBookState>,
    trade_dedup: &mut BTreeMap<String, TradeDedupRing<String>>,
    sink: ProviderEventSink,
    events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
    wake: &ProviderCoordinatorWake,
    generation: u64,
    seq: &mut u64,
) {
    let Ok(parsed) = parse_ws_frame(text) else {
        return;
    };
    let received_nanos = now_unix_nanos();
    let frame_seq = match &parsed {
        CoinbaseWsEvent::Heartbeat { sequence_num }
        | CoinbaseWsEvent::MarketTrades { sequence_num, .. }
        | CoinbaseWsEvent::L2Data { sequence_num, .. } => Some(*sequence_num),
        CoinbaseWsEvent::Subscriptions | CoinbaseWsEvent::Ignored => None,
    };
    if let Some(current_seq) = frame_seq {
        let gap = ws
            .last_sequence_num
            .is_some_and(|prev| current_seq > prev.saturating_add(1));
        ws.last_sequence_num = Some(current_seq);
        if gap && !ws.active_l2.is_empty() {
            let active_l2: Vec<String> = ws.active_l2.iter().cloned().collect();
            for state in books.values_mut() {
                state.reset();
            }
            if ws.resync_l2(&active_l2).is_err() {
                ws.close();
                return;
            }
        }
    }

    match parsed {
        CoinbaseWsEvent::Heartbeat { .. } => {
            emit_data(
                sink,
                events,
                CryptoRealtimeEvent::Heartbeat(generation, None),
                wake,
            );
        }
        CoinbaseWsEvent::MarketTrades {
            events: raw_events, ..
        } => {
            let Ok(decoded) = decode_ws_trades(
                &raw_events,
                |product_id| {
                    find_coinbase_instrument(&demand.trades, product_id)
                        .map(|inst| (inst.instrument_id.clone(), inst.entitlement_id.clone()))
                },
                generation,
                seq,
                received_nanos,
            ) else {
                return;
            };
            let mut fresh = Vec::with_capacity(decoded.len());
            for (product_id, trade_id, trade) in decoded {
                let ring = trade_dedup
                    .entry(product_id)
                    .or_insert_with(|| TradeDedupRing::new(TRADE_DEDUP_CAPACITY));
                if ring.insert(trade_id) {
                    fresh.push(trade);
                }
            }
            if !fresh.is_empty() {
                emit_data(
                    sink,
                    events,
                    CryptoRealtimeEvent::Trades(generation, fresh),
                    wake,
                );
            }
        }
        CoinbaseWsEvent::L2Data {
            timestamp_nanos,
            events: raw_events,
            ..
        } => {
            let fallback_ts = if timestamp_nanos > 0 {
                timestamp_nanos
            } else {
                received_nanos
            };
            let Ok(messages) = decode_l2_events(&raw_events, fallback_ts) else {
                return;
            };
            for msg in messages {
                let product_id = msg.product_id.clone();
                let state = books.entry(product_id.clone()).or_default();
                match state.apply(&msg) {
                    CoinbaseBookApplyOutcome::Updated => {
                        if let Some(inst) = find_coinbase_instrument(&demand.books, &product_id) {
                            *seq = seq.saturating_add(1).max(1);
                            if let Ok(snapshot) = state.to_depth_snapshot(
                                &inst.instrument_id,
                                &inst.entitlement_id,
                                generation,
                                *seq,
                                msg.ts_nanos,
                                received_nanos,
                            ) {
                                emit_data(
                                    sink,
                                    events,
                                    CryptoRealtimeEvent::Depth(generation, snapshot),
                                    wake,
                                );
                            }
                        }
                        if let Some(inst) = find_coinbase_instrument(&demand.quotes, &product_id) {
                            *seq = seq.saturating_add(1).max(1);
                            if let Some(quote) = state.to_top_of_book(
                                &inst.instrument_id,
                                &inst.entitlement_id,
                                generation,
                                *seq,
                                msg.ts_nanos,
                                received_nanos,
                            ) {
                                emit_data(
                                    sink,
                                    events,
                                    CryptoRealtimeEvent::Quote(generation, quote),
                                    wake,
                                );
                            }
                        }
                    }
                    CoinbaseBookApplyOutcome::NoChange => {}
                    CoinbaseBookApplyOutcome::NeedsResync => {
                        if ws.resync_l2(&[product_id]).is_err() {
                            ws.close();
                            return;
                        }
                    }
                }
            }
        }
        CoinbaseWsEvent::Subscriptions | CoinbaseWsEvent::Ignored => {}
    }
}
