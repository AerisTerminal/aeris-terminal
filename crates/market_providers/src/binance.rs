//! Binance public market-provider registration, catalog worker, history fetcher,
//! and realtime dual-segment (USDT-M Perpetual + Spot) WebSocket worker.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender},
    },
    time::{Duration, Instant},
};

use aeris_binance_market_adapter::{
    BINANCE_FUTURES_MARKET_WS_URL, BINANCE_FUTURES_PUBLIC_WS_URL, BINANCE_SPOT_WS_URL,
    BinanceCatalog, BinanceHttpClient, BinanceHttpConfig, BinanceMarketKind, BinanceOrderBookSync,
    BinanceWsEvent, binance_interval_for_period, book::BookSyncOutcome, build_subscribe,
    build_unsubscribe, decode_agg_trade, decode_book_ticker, decode_depth_update,
    decode_force_order, decode_live_candle, parse_provider_symbol, parse_ws_frame,
};
use aeris_crypto_common::{CryptoError, CryptoResult, LiquidationLedger, TradeDedupRing};
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

pub(crate) const BINANCE_PUBLIC_ACCOUNT_ID: &str = "binance-public";
pub(crate) const BINANCE_PUBLIC_ENTITLEMENT_ID: &str = "binance-public";
const CATALOG_WORKER_NAME: &str = "aeris-binance-catalog";
const PROVIDER_WORKER_NAME: &str = "aeris-binance-provider";
const SILENCE_TIMEOUT: Duration = Duration::from_secs(45);
const RESYNC_COOLDOWN: Duration = Duration::from_secs(2);

pub(crate) const BINANCE_PRESENTATION: ProviderPresentationDescriptor =
    ProviderPresentationDescriptor {
        id: "binance",
        display_name: "Binance",
        chart_interval_labels: &[
            "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "8h", "12h", "1D", "3D", "1W",
            "1M",
        ],
        daily_bars: ProviderDailyBarCadence::FixedDay,
        search_hint: "Search Binance perpetuals & spot",
        logo_key: "binance",
        broker_venue_id: None,
        attribution: None,
        catalog_symbol: ProviderCatalogSymbol::ProviderSymbol,
        selection_entitlement_id: BINANCE_PUBLIC_ENTITLEMENT_ID,
        catalog_refresh_on_startup: true,
        ready_label_suffix: "",
        feed_level: ProviderFeedLevel::PriceLevels,
        search_categories_available: false,
        market_screen_available: false,
        market_scan_available: false,
        trades_available: true,
        connection_kind: ProviderConnectionKind::Public,
    };

pub(crate) const BINANCE_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: "binance",
    presentation: &BINANCE_PRESENTATION,
    login_schema: None,
    capabilities: BINANCE_CAPABILITIES,
    demand_flush: DemandFlushPolicy::Dirty,
};

const BINANCE_CAPABILITIES: ProviderCapabilities = ProviderCapabilities {
    account_id: BINANCE_PUBLIC_ACCOUNT_ID,
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
        worker: "aeris-binance-history",
        supported_period: binance_supported_period,
    },
    recovery_policy: ProviderRecoveryPolicy::WorkerReconcilesDemand,
    alert_demand_update: AlertDemandUpdate::MarkDirty,
    live_model: LiveModel::ProviderCandles,
    calendar_bar_stamp: CalendarBarStamp::TradingDateMidnightUtc,
    overflow_recovery_detail: "Local market event queue overflow; repairing continuity",
    candle_requires_connected: true,
    candle_correction_detail: "Binance candle replacement requires covering history",
    candle_wire_interval: Some(binance_candle_interval),
    candle_demand_policy: CandleDemandPolicy::ReconcileAfterSelection,
    trade_demand_policy: TradeDemandPolicy::SessionManaged,
    instrument_missing_detail: "Binance instrument is not installed",
    catalog_session_binding: CatalogSessionBinding::CurrentSession,
    maximum_order_level_instruments: 0,
    recovers_silence: true,
};

pub(crate) fn provider_spec(
    realtime_enabled: bool,
    shared_catalogs: Arc<SharedCryptoCatalogs>,
    liquidation_ledger: Arc<LiquidationLedger>,
) -> ProviderRuntimeSpec {
    let liq_for_rt = Arc::clone(&liquidation_ledger);
    crypto_provider_spec(
        BINANCE_DESCRIPTOR,
        CryptoHistorySource::Binance(BinanceHttpClient::default()),
        realtime_enabled,
        CATALOG_WORKER_NAME,
        PROVIDER_WORKER_NAME,
        "Binance live session is recovering",
        binance_interval_for_period,
        move |controls, events, ws_gen, stop| {
            run_catalog(controls, events, ws_gen, stop, &shared_catalogs);
        },
        move |controls, events, stop, ws_gen, wake| {
            run_realtime(controls, events, stop, ws_gen, wake, &liq_for_rt);
        },
    )
    .with_open_interest(Arc::new(
        aeris_binance_market_adapter::open_interest::fetch_open_interest,
    ))
    .with_liquidations(Arc::new(move |request, stop| {
        aeris_binance_market_adapter::liquidations::fetch_liquidations(
            &liquidation_ledger,
            request,
            stop,
        )
    }))
}

pub(crate) fn binance_candle_interval(period: BarPeriod) -> Result<String, String> {
    binance_interval_for_period(period)
        .map(str::to_owned)
        .map_err(|_| "Binance history is unavailable for this interval".to_string())
}

pub(crate) fn binance_supported_period(period: BarPeriod) -> Result<(), String> {
    binance_interval_for_period(period)
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
    let mut catalog: Option<Arc<BinanceCatalog>> = None;
    let mut fetched_at: Option<Instant> = None;
    let mut selection_generation: u64 = 0;
    let mut next_refresh = Instant::now();

    // Warm the public catalog on startup so the symbol browser populates instantly.
    if !stop.load(Ordering::Acquire)
        && let Ok(loaded) = shared_catalogs.get_or_fetch_binance(false)
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
                    match shared_catalogs.get_or_fetch_binance(false) {
                        Ok(loaded) => {
                            catalog = Some(loaded);
                            fetched_at = Some(Instant::now());
                            next_refresh = Instant::now() + CATALOG_REFRESH_INTERVAL;
                        }
                        Err(_error) => {
                            if reject_catalog(
                                events,
                                search.consumer_id,
                                "binance",
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
                    match shared_catalogs.get_or_fetch_binance(false) {
                        Ok(loaded) => {
                            catalog = Some(loaded);
                            fetched_at = Some(Instant::now());
                            next_refresh = Instant::now() + CATALOG_REFRESH_INTERVAL;
                        }
                        Err(_error) => {
                            if reject_catalog(
                                events,
                                select.consumer_id,
                                "binance",
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
                        "binance",
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
                    "binance",
                    BINANCE_PUBLIC_ENTITLEMENT_ID,
                    ws_generation.load(Ordering::Acquire),
                    selection_generation,
                    item.instrument_id.clone(),
                    item.provider_symbol.clone(),
                    item.display_symbol.clone(),
                    item.kind.exchange_label().to_string(),
                    item.quantity_decimals,
                    item.tick_size_fixed,
                    item.kind == BinanceMarketKind::Spot,
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
            match shared_catalogs.get_or_fetch_binance(true) {
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum SegmentRole {
    /// Carries all stream types on one combined socket (Binance Spot).
    All,
    /// Carries `@kline_*` and `@aggTrade` (Binance Futures `/market/stream`).
    FuturesMarket,
    /// Carries `@bookTicker` and `@depth@100ms` (Binance Futures `/public/stream`).
    FuturesPublic,
}

impl SegmentRole {
    const fn carries_market_streams(self) -> bool {
        matches!(self, Self::All | Self::FuturesMarket)
    }

    const fn carries_public_streams(self) -> bool {
        matches!(self, Self::All | Self::FuturesPublic)
    }
}

struct SegmentSocket {
    kind: BinanceMarketKind,
    role: SegmentRole,
    url: &'static str,
    socket: Option<MarketSocket>,
    active_streams: BTreeSet<String>,
    last_activity: Instant,
    books: BTreeMap<String, BinanceOrderBookSync>,
    last_resync: BTreeMap<String, Instant>,
    trade_dedup: BTreeMap<String, TradeDedupRing<u64>>,
    req_id: u64,
}

impl SegmentSocket {
    fn new(kind: BinanceMarketKind, role: SegmentRole, url: &'static str) -> Self {
        Self {
            kind,
            role,
            url,
            socket: None,
            active_streams: BTreeSet::new(),
            last_activity: Instant::now(),
            books: BTreeMap::new(),
            last_resync: BTreeMap::new(),
            trade_dedup: BTreeMap::new(),
            req_id: 1,
        }
    }

    fn close(&mut self) {
        if let Some(mut sock) = self.socket.take() {
            sock.close();
        }
        self.active_streams.clear();
        self.books.clear();
        self.last_resync.clear();
        self.trade_dedup.clear();
    }

    fn desired_streams(&self, demand: &CryptoDemand) -> BTreeSet<String> {
        let mut streams = BTreeSet::new();
        if self.role.carries_market_streams() {
            for c in &demand.candles {
                if let Ok(sym) = parse_provider_symbol(&c.instrument.provider_symbol)
                    && sym.kind == self.kind
                {
                    streams.insert(format!(
                        "{}@kline_{}",
                        sym.wire_symbol.to_ascii_lowercase(),
                        c.interval
                    ));
                }
            }
            for t in &demand.trades {
                if let Ok(sym) = parse_provider_symbol(&t.provider_symbol)
                    && sym.kind == self.kind
                {
                    streams.insert(format!("{}@aggTrade", sym.wire_symbol.to_ascii_lowercase()));
                }
            }
            if self.kind == BinanceMarketKind::UsdmPerp {
                for inst in demand
                    .candles
                    .iter()
                    .map(|c| &c.instrument)
                    .chain(&demand.trades)
                    .chain(&demand.quotes)
                    .chain(&demand.books)
                {
                    if let Ok(sym) = parse_provider_symbol(&inst.provider_symbol)
                        && sym.kind == BinanceMarketKind::UsdmPerp
                    {
                        streams.insert(format!(
                            "{}@forceOrder",
                            sym.wire_symbol.to_ascii_lowercase()
                        ));
                    }
                }
            }
        }
        if self.role.carries_public_streams() {
            for q in &demand.quotes {
                if let Ok(sym) = parse_provider_symbol(&q.provider_symbol)
                    && sym.kind == self.kind
                {
                    streams.insert(format!(
                        "{}@bookTicker",
                        sym.wire_symbol.to_ascii_lowercase()
                    ));
                }
            }
            for b in &demand.books {
                if let Ok(sym) = parse_provider_symbol(&b.provider_symbol)
                    && sym.kind == self.kind
                {
                    streams.insert(format!(
                        "{}@depth@100ms",
                        sym.wire_symbol.to_ascii_lowercase()
                    ));
                }
            }
        }
        streams
    }

    fn reconcile(&mut self, demand: &CryptoDemand, stop: &Arc<AtomicBool>) -> CryptoResult<()> {
        let desired = self.desired_streams(demand);
        if desired.is_empty() {
            self.close();
            return Ok(());
        }
        if self.socket.is_none() {
            let (sock, _) = MarketSocket::connect(self.url, CONNECT_TIMEOUT, stop)
                .map_err(|e| CryptoError::Transport(e.to_string()))?;
            self.socket = Some(sock);
            self.last_activity = Instant::now();
            self.active_streams.clear();
        }
        let to_unsub: Vec<String> = self.active_streams.difference(&desired).cloned().collect();
        let to_sub: Vec<String> = desired.difference(&self.active_streams).cloned().collect();

        if let Some(sock) = self.socket.as_mut() {
            if !to_unsub.is_empty() {
                self.req_id = self.req_id.saturating_add(1);
                let frame = build_unsubscribe(&to_unsub, self.req_id);
                sock.send_text(&frame)
                    .map_err(|e| CryptoError::Transport(e.to_string()))?;
                for s in to_unsub {
                    self.active_streams.remove(&s);
                }
            }
            if !to_sub.is_empty() {
                self.req_id = self.req_id.saturating_add(1);
                let frame = build_subscribe(&to_sub, self.req_id);
                sock.send_text(&frame)
                    .map_err(|e| CryptoError::Transport(e.to_string()))?;
                for s in to_sub {
                    self.active_streams.insert(s);
                }
            }
        }

        // Retain book synchronizers only for demanded books.
        let demanded_books: BTreeSet<String> = demand
            .books
            .iter()
            .filter_map(|b| parse_provider_symbol(&b.provider_symbol).ok())
            .filter(|s| s.kind == self.kind)
            .map(|s| s.wire_symbol)
            .collect();
        self.books.retain(|sym, _| demanded_books.contains(sym));
        Ok(())
    }
}

fn find_instrument<'a>(
    list: &'a [CryptoInstrumentDemand],
    kind: BinanceMarketKind,
    wire_symbol: &str,
) -> Option<&'a CryptoInstrumentDemand> {
    let expected = format!("{}:{wire_symbol}", kind.prefix());
    list.iter().find(|item| item.provider_symbol == expected)
}

#[allow(clippy::too_many_lines)]
fn run_realtime(
    controls: &Receiver<CryptoRealtimeControl>,
    events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
    stop: &Arc<AtomicBool>,
    ws_generation: &Arc<AtomicU64>,
    wake: &ProviderCoordinatorWake,
    liquidation_ledger: &LiquidationLedger,
) {
    let mut demand = CryptoDemand::default();
    let mut http = BinanceHttpClient::default();

    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        // Wait for Connect while keeping the latest demand.
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

        let mut futures_market = SegmentSocket::new(
            BinanceMarketKind::UsdmPerp,
            SegmentRole::FuturesMarket,
            BINANCE_FUTURES_MARKET_WS_URL,
        );
        let mut futures_public = SegmentSocket::new(
            BinanceMarketKind::UsdmPerp,
            SegmentRole::FuturesPublic,
            BINANCE_FUTURES_PUBLIC_WS_URL,
        );
        let mut spot = SegmentSocket::new(
            BinanceMarketKind::Spot,
            SegmentRole::All,
            BINANCE_SPOT_WS_URL,
        );
        let mut seq: u64 = 0;
        let mut demand_dirty = true;
        let mut failed = false;
        let mut backfilled_trades: BTreeSet<String> = BTreeSet::new();

        'session: loop {
            if stop.load(Ordering::Acquire) {
                futures_market.close();
                futures_public.close();
                spot.close();
                return;
            }
            while let Ok(ctrl) = controls.try_recv() {
                match ctrl {
                    CryptoRealtimeControl::Connect(_) | CryptoRealtimeControl::Stop => {
                        futures_market.close();
                        futures_public.close();
                        spot.close();
                        break 'session;
                    }
                    CryptoRealtimeControl::Subscribe(next) => {
                        demand = next;
                        demand_dirty = true;
                    }
                }
            }

            if demand_dirty {
                if futures_market.reconcile(&demand, stop).is_err()
                    || futures_public.reconcile(&demand, stop).is_err()
                    || spot.reconcile(&demand, stop).is_err()
                {
                    failed = true;
                    break 'session;
                }
                backfill_binance_trades(
                    &demand,
                    &mut backfilled_trades,
                    &mut http,
                    &mut futures_market.trade_dedup,
                    &mut spot.trade_dedup,
                    sink,
                    events,
                    wake,
                    stop,
                    generation,
                    &mut seq,
                );
                demand_dirty = false;
            }

            let mut any_socket = false;
            for seg in [&mut futures_market, &mut futures_public, &mut spot] {
                if seg.socket.is_none() {
                    continue;
                }
                any_socket = true;
                if seg.last_activity.elapsed() > SILENCE_TIMEOUT {
                    failed = true;
                    break 'session;
                }
                let read = match seg.socket.as_mut() {
                    Some(sock) => sock.read_event(Instant::now() + READ_TIMEOUT),
                    None => continue,
                };
                match read {
                    Ok(MarketSocketEvent::Text(text)) => {
                        seg.last_activity = Instant::now();
                        handle_binance_frame(
                            seg,
                            &text,
                            &demand,
                            &mut http,
                            liquidation_ledger,
                            sink,
                            events,
                            wake,
                            stop,
                            generation,
                            &mut seq,
                        );
                    }
                    Ok(MarketSocketEvent::Pong) => {
                        seg.last_activity = Instant::now();
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

            if !any_socket {
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        futures_market.close();
        futures_public.close();
        spot.close();
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
fn backfill_binance_trades(
    demand: &CryptoDemand,
    backfilled: &mut BTreeSet<String>,
    http_client: &mut BinanceHttpClient,
    futures_dedup: &mut BTreeMap<String, TradeDedupRing<u64>>,
    spot_dedup: &mut BTreeMap<String, TradeDedupRing<u64>>,
    sink: ProviderEventSink,
    events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
    wake: &ProviderCoordinatorWake,
    stop: &Arc<AtomicBool>,
    generation: u64,
    seq: &mut u64,
) {
    const BACKFILL_PAGES: usize = 5;
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
        let Ok(trades) = http_client.fetch_recent_agg_trades(
            sym.kind,
            &sym.wire_symbol,
            &inst.instrument_id,
            &inst.entitlement_id,
            generation,
            seq,
            now_unix_nanos(),
            BACKFILL_PAGES,
            BinanceHttpConfig::default(),
            stop,
        ) else {
            continue;
        };
        let dedup_map = match sym.kind {
            BinanceMarketKind::UsdmPerp => &mut *futures_dedup,
            BinanceMarketKind::Spot => &mut *spot_dedup,
        };
        let dedup = dedup_map
            .entry(sym.wire_symbol.clone())
            .or_insert_with(|| TradeDedupRing::new(8_192));
        let mut batch = Vec::with_capacity(CHUNK_SIZE);
        for (agg_id, trade) in trades {
            if dedup.insert(agg_id) {
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
fn handle_binance_frame(
    seg: &mut SegmentSocket,
    text: &str,
    demand: &CryptoDemand,
    http: &mut BinanceHttpClient,
    liquidation_ledger: &LiquidationLedger,
    sink: ProviderEventSink,
    events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
    wake: &ProviderCoordinatorWake,
    stop: &Arc<AtomicBool>,
    generation: u64,
    seq: &mut u64,
) {
    let Ok(parsed) = parse_ws_frame(text) else {
        return;
    };
    let received_nanos = now_unix_nanos();
    match parsed {
        BinanceWsEvent::AggTrade(raw) => {
            // Peek symbol from raw or match against demanded trades
            for inst in &demand.trades {
                let Ok(sym_ref) = parse_provider_symbol(&inst.provider_symbol) else {
                    continue;
                };
                if sym_ref.kind != seg.kind {
                    continue;
                }
                *seq = seq.saturating_add(1).max(1);
                if let Ok((sym, agg_id, trade)) = decode_agg_trade(
                    &raw,
                    &inst.instrument_id,
                    &inst.entitlement_id,
                    generation,
                    *seq,
                    received_nanos,
                ) {
                    if sym != sym_ref.wire_symbol {
                        continue;
                    }
                    let ring = seg
                        .trade_dedup
                        .entry(sym)
                        .or_insert_with(|| TradeDedupRing::new(4_096));
                    if ring.insert(agg_id) {
                        emit_data(
                            sink,
                            events,
                            CryptoRealtimeEvent::Trades(generation, vec![trade]),
                            wake,
                        );
                    }
                    break;
                }
            }
        }
        BinanceWsEvent::BookTicker(raw) => {
            for inst in &demand.quotes {
                let Ok(sym_ref) = parse_provider_symbol(&inst.provider_symbol) else {
                    continue;
                };
                if sym_ref.kind != seg.kind {
                    continue;
                }
                *seq = seq.saturating_add(1).max(1);
                if let Ok((sym, quote)) = decode_book_ticker(
                    &raw,
                    &inst.instrument_id,
                    &inst.entitlement_id,
                    generation,
                    *seq,
                    received_nanos,
                ) && sym == sym_ref.wire_symbol
                {
                    emit_data(
                        sink,
                        events,
                        CryptoRealtimeEvent::Quote(generation, quote),
                        wake,
                    );
                    break;
                }
            }
        }
        BinanceWsEvent::Kline(raw) => {
            if let Ok((sym, interval, live)) = decode_live_candle(&raw) {
                let provider_symbol = format!("{}:{sym}", seg.kind.prefix());
                emit_data(
                    sink,
                    events,
                    CryptoRealtimeEvent::Candle(generation, provider_symbol, interval, live.bar),
                    wake,
                );
            }
        }
        BinanceWsEvent::DepthUpdate(raw) => {
            let Ok(delta) = decode_depth_update(&raw) else {
                return;
            };
            let sym = delta.symbol.clone();
            let event_time_ms = delta.event_time_ms;
            let Some(inst) = find_instrument(&demand.books, seg.kind, &sym) else {
                return;
            };
            let sync = seg
                .books
                .entry(sym.clone())
                .or_insert_with(|| BinanceOrderBookSync::new(seg.kind));
            match sync.on_ws_delta(delta) {
                BookSyncOutcome::Updated => {
                    *seq = seq.saturating_add(1).max(1);
                    if let Ok(snapshot) = sync.to_depth_snapshot(
                        &inst.instrument_id,
                        &inst.entitlement_id,
                        generation,
                        *seq,
                        event_time_ms,
                        received_nanos,
                    ) {
                        emit_data(
                            sink,
                            events,
                            CryptoRealtimeEvent::Depth(generation, snapshot),
                            wake,
                        );
                    }
                    if let Some(quote) = sync.to_top_of_book(
                        &inst.instrument_id,
                        &inst.entitlement_id,
                        generation,
                        *seq,
                        event_time_ms,
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
                BookSyncOutcome::BufferedAwaitingSnapshot | BookSyncOutcome::NeedsResync => {
                    let can_resync = seg
                        .last_resync
                        .get(&sym)
                        .is_none_or(|at| at.elapsed() >= RESYNC_COOLDOWN);
                    if can_resync {
                        seg.last_resync.insert(sym.clone(), Instant::now());
                        if let Ok(rest_snap) = http.fetch_depth_snapshot(
                            seg.kind,
                            &sym,
                            1_000,
                            BinanceHttpConfig::default(),
                            stop,
                        ) && sync.apply_rest_snapshot(&rest_snap).is_ok()
                        {
                            *seq = seq.saturating_add(1).max(1);
                            if let Ok(snapshot) = sync.to_depth_snapshot(
                                &inst.instrument_id,
                                &inst.entitlement_id,
                                generation,
                                *seq,
                                event_time_ms,
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
                    }
                }
            }
        }
        BinanceWsEvent::ForceOrder(raw) => {
            if let Ok((sym, event)) = decode_force_order(&raw) {
                liquidation_ledger.record(&sym, event);
            }
        }
        BinanceWsEvent::Ack | BinanceWsEvent::Ignored => {}
    }
}
