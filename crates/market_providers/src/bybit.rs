//! Bybit V5 public market-provider registration, catalog worker, history fetcher,
//! and realtime dual-segment (Linear Perpetual + Spot) WebSocket worker.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender},
    },
    time::{Duration, Instant},
};

use aeris_bybit_market_adapter::{
    BYBIT_LINEAR_WS_URL, BYBIT_SPOT_WS_URL, BybitBookState, BybitCatalog, BybitHttpClient,
    BybitMarketKind, BybitWsEvent, build_ping, build_subscribe, build_unsubscribe,
    bybit_interval_for_period, decode_live_candle, decode_orderbook, decode_public_trades,
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

pub(crate) const BYBIT_PUBLIC_ACCOUNT_ID: &str = "bybit-public";
pub(crate) const BYBIT_PUBLIC_ENTITLEMENT_ID: &str = "bybit-public";
const CATALOG_WORKER_NAME: &str = "aeris-bybit-catalog";
const PROVIDER_WORKER_NAME: &str = "aeris-bybit-provider";
const PING_INTERVAL: Duration = Duration::from_secs(20);
const SILENCE_TIMEOUT: Duration = Duration::from_secs(45);

pub(crate) const BYBIT_PRESENTATION: ProviderPresentationDescriptor =
    ProviderPresentationDescriptor {
        id: "bybit",
        display_name: "Bybit",
        chart_interval_labels: &[
            "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1D", "1W", "1M",
        ],
        daily_bars: ProviderDailyBarCadence::FixedDay,
        search_hint: "Search Bybit perpetuals & spot",
        logo_key: "bybit",
        broker_venue_id: None,
        attribution: None,
        catalog_symbol: ProviderCatalogSymbol::ProviderSymbol,
        selection_entitlement_id: BYBIT_PUBLIC_ENTITLEMENT_ID,
        catalog_refresh_on_startup: true,
        ready_label_suffix: "",
        feed_level: ProviderFeedLevel::PriceLevels,
        search_categories_available: false,
        market_screen_available: false,
        market_scan_available: false,
        trades_available: true,
        connection_kind: ProviderConnectionKind::Public,
    };

pub(crate) const BYBIT_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: "bybit",
    presentation: &BYBIT_PRESENTATION,
    login_schema: None,
    capabilities: BYBIT_CAPABILITIES,
    demand_flush: DemandFlushPolicy::Dirty,
};

const BYBIT_CAPABILITIES: ProviderCapabilities = ProviderCapabilities {
    account_id: BYBIT_PUBLIC_ACCOUNT_ID,
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
        worker: "aeris-bybit-history",
        supported_period: bybit_supported_period,
    },
    recovery_policy: ProviderRecoveryPolicy::WorkerReconcilesDemand,
    alert_demand_update: AlertDemandUpdate::MarkDirty,
    live_model: LiveModel::ProviderCandles,
    calendar_bar_stamp: CalendarBarStamp::TradingDateMidnightUtc,
    overflow_recovery_detail: "Local market event queue overflow; repairing continuity",
    candle_requires_connected: true,
    candle_correction_detail: "Bybit candle replacement requires covering history",
    candle_wire_interval: Some(bybit_candle_interval),
    candle_demand_policy: CandleDemandPolicy::ReconcileAfterSelection,
    trade_demand_policy: TradeDemandPolicy::SessionManaged,
    instrument_missing_detail: "Bybit instrument is not installed",
    catalog_session_binding: CatalogSessionBinding::CurrentSession,
    maximum_order_level_instruments: 0,
    recovers_silence: true,
};

pub(crate) fn provider_spec(
    realtime_enabled: bool,
    shared_catalogs: Arc<SharedCryptoCatalogs>,
) -> ProviderRuntimeSpec {
    crypto_provider_spec(
        BYBIT_DESCRIPTOR,
        CryptoHistorySource::Bybit(BybitHttpClient::default()),
        realtime_enabled,
        CATALOG_WORKER_NAME,
        PROVIDER_WORKER_NAME,
        "Bybit live session is recovering",
        bybit_interval_for_period,
        move |controls, events, ws_gen, stop| {
            run_catalog(controls, events, ws_gen, stop, &shared_catalogs);
        },
        run_realtime,
    )
}

fn bybit_candle_interval(period: BarPeriod) -> Result<String, String> {
    bybit_interval_for_period(period)
        .map(str::to_owned)
        .map_err(|_| "Bybit history is unavailable for this interval".to_string())
}

pub(crate) fn bybit_supported_period(period: BarPeriod) -> Result<(), String> {
    bybit_interval_for_period(period)
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
    let mut catalog: Option<Arc<BybitCatalog>> = None;
    let mut fetched_at: Option<Instant> = None;
    let mut selection_generation: u64 = 0;
    let mut next_refresh = Instant::now();

    // Warm the public catalog on startup so the symbol browser populates instantly.
    if !stop.load(Ordering::Acquire)
        && let Ok(loaded) = shared_catalogs.get_or_fetch_bybit(false)
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
                    match shared_catalogs.get_or_fetch_bybit(false) {
                        Ok(loaded) => {
                            catalog = Some(loaded);
                            fetched_at = Some(Instant::now());
                            next_refresh = Instant::now() + CATALOG_REFRESH_INTERVAL;
                        }
                        Err(_error) => {
                            if reject_catalog(
                                events,
                                search.consumer_id,
                                "bybit",
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
                    match shared_catalogs.get_or_fetch_bybit(false) {
                        Ok(loaded) => {
                            catalog = Some(loaded);
                            fetched_at = Some(Instant::now());
                            next_refresh = Instant::now() + CATALOG_REFRESH_INTERVAL;
                        }
                        Err(_error) => {
                            if reject_catalog(
                                events,
                                select.consumer_id,
                                "bybit",
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
                        "bybit",
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
                    "bybit",
                    BYBIT_PUBLIC_ENTITLEMENT_ID,
                    ws_generation.load(Ordering::Acquire),
                    selection_generation,
                    item.instrument_id.clone(),
                    item.provider_symbol.clone(),
                    item.display_symbol.clone(),
                    item.kind.exchange_label().to_string(),
                    item.quantity_decimals,
                    item.tick_size_fixed,
                    item.kind == BybitMarketKind::Spot,
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
            match shared_catalogs.get_or_fetch_bybit(true) {
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

struct BybitSegmentSocket {
    kind: BybitMarketKind,
    url: &'static str,
    socket: Option<MarketSocket>,
    active_topics: BTreeSet<String>,
    last_activity: Instant,
    last_ping: Instant,
    books: BTreeMap<String, BybitBookState>,
    trade_dedup: BTreeMap<String, TradeDedupRing<String>>,
}

impl BybitSegmentSocket {
    fn new(kind: BybitMarketKind, url: &'static str) -> Self {
        let now = Instant::now();
        Self {
            kind,
            url,
            socket: None,
            active_topics: BTreeSet::new(),
            last_activity: now,
            last_ping: now,
            books: BTreeMap::new(),
            trade_dedup: BTreeMap::new(),
        }
    }

    fn close(&mut self) {
        if let Some(mut sock) = self.socket.take() {
            sock.close();
        }
        self.active_topics.clear();
        self.books.clear();
        self.trade_dedup.clear();
    }

    fn desired_topics(&self, demand: &CryptoDemand) -> BTreeSet<String> {
        let mut topics = BTreeSet::new();
        for c in &demand.candles {
            if let Ok(sym) = parse_provider_symbol(&c.instrument.provider_symbol)
                && sym.kind == self.kind
            {
                topics.insert(format!("kline.{}.{}", c.interval, sym.wire_symbol));
            }
        }
        for t in &demand.trades {
            if let Ok(sym) = parse_provider_symbol(&t.provider_symbol)
                && sym.kind == self.kind
            {
                topics.insert(format!("publicTrade.{}", sym.wire_symbol));
            }
        }
        for inst in demand.quotes.iter().chain(&demand.books) {
            if let Ok(sym) = parse_provider_symbol(&inst.provider_symbol)
                && sym.kind == self.kind
            {
                let depth = match self.kind {
                    BybitMarketKind::Linear => 500,
                    BybitMarketKind::Spot => 200,
                };
                topics.insert(format!("orderbook.{depth}.{}", sym.wire_symbol));
            }
        }
        topics
    }

    fn reconcile(&mut self, demand: &CryptoDemand, stop: &Arc<AtomicBool>) -> CryptoResult<()> {
        let desired = self.desired_topics(demand);
        if desired.is_empty() {
            self.close();
            return Ok(());
        }
        if self.socket.is_none() {
            let (sock, _) = MarketSocket::connect(self.url, CONNECT_TIMEOUT, stop)
                .map_err(|e| CryptoError::Transport(e.to_string()))?;
            self.socket = Some(sock);
            self.last_activity = Instant::now();
            self.last_ping = Instant::now();
            self.active_topics.clear();
        }
        let to_unsub: Vec<String> = self.active_topics.difference(&desired).cloned().collect();
        let to_sub: Vec<String> = desired.difference(&self.active_topics).cloned().collect();

        if let Some(sock) = self.socket.as_mut() {
            for chunk in to_unsub.chunks(10) {
                sock.send_text(&build_unsubscribe(chunk))
                    .map_err(|e| CryptoError::Transport(e.to_string()))?;
            }
            for t in to_unsub {
                self.active_topics.remove(&t);
            }
            for chunk in to_sub.chunks(10) {
                sock.send_text(&build_subscribe(chunk))
                    .map_err(|e| CryptoError::Transport(e.to_string()))?;
            }
            for t in to_sub {
                self.active_topics.insert(t);
            }
        }
        Ok(())
    }
}

fn find_bybit_instrument<'a>(
    list: &'a [CryptoInstrumentDemand],
    kind: BybitMarketKind,
    wire_symbol: &str,
) -> Option<&'a CryptoInstrumentDemand> {
    let expected = format!("{}:{wire_symbol}", kind.category());
    list.iter().find(|item| item.provider_symbol == expected)
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

        let mut linear = BybitSegmentSocket::new(BybitMarketKind::Linear, BYBIT_LINEAR_WS_URL);
        let mut spot = BybitSegmentSocket::new(BybitMarketKind::Spot, BYBIT_SPOT_WS_URL);
        let mut seq: u64 = 0;
        let mut demand_dirty = true;
        let mut failed = false;

        'session: loop {
            if stop.load(Ordering::Acquire) {
                linear.close();
                spot.close();
                return;
            }
            while let Ok(ctrl) = controls.try_recv() {
                match ctrl {
                    CryptoRealtimeControl::Connect(_) | CryptoRealtimeControl::Stop => {
                        linear.close();
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
                if linear.reconcile(&demand, stop).is_err()
                    || spot.reconcile(&demand, stop).is_err()
                {
                    failed = true;
                    break 'session;
                }
                demand_dirty = false;
            }

            let mut any_socket = false;
            for seg in [&mut linear, &mut spot] {
                if seg.socket.is_none() {
                    continue;
                }
                any_socket = true;
                if seg.last_activity.elapsed() > SILENCE_TIMEOUT {
                    failed = true;
                    break 'session;
                }
                if seg.last_ping.elapsed() >= PING_INTERVAL {
                    if let Some(sock) = seg.socket.as_mut()
                        && sock.send_text(&build_ping()).is_err()
                    {
                        failed = true;
                        break 'session;
                    }
                    seg.last_ping = Instant::now();
                }
                let read = match seg.socket.as_mut() {
                    Some(sock) => sock.read_event(Instant::now() + READ_TIMEOUT),
                    None => continue,
                };
                match read {
                    Ok(MarketSocketEvent::Text(text)) => {
                        seg.last_activity = Instant::now();
                        handle_bybit_frame(
                            seg, &text, &demand, sink, events, wake, generation, &mut seq,
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

        linear.close();
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

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn handle_bybit_frame(
    seg: &mut BybitSegmentSocket,
    text: &str,
    demand: &CryptoDemand,
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
    match parsed {
        BybitWsEvent::Pong => {
            emit_data(
                sink,
                events,
                CryptoRealtimeEvent::Heartbeat(generation, None),
                wake,
            );
        }
        BybitWsEvent::Trades { symbol, data } => {
            let Some(inst) = find_bybit_instrument(&demand.trades, seg.kind, &symbol) else {
                return;
            };
            if let Ok(decoded) = decode_public_trades(
                &data,
                &inst.instrument_id,
                &inst.entitlement_id,
                generation,
                seq,
                received_nanos,
            ) {
                let ring = seg
                    .trade_dedup
                    .entry(symbol)
                    .or_insert_with(|| TradeDedupRing::new(4_096));
                let fresh: Vec<_> = decoded
                    .into_iter()
                    .filter_map(|(id, trade)| ring.insert(id).then_some(trade))
                    .collect();
                if !fresh.is_empty() {
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
            let Ok(msg) = decode_orderbook(&data, &msg_type, ts_ms) else {
                return;
            };
            let state = seg.books.entry(symbol.clone()).or_default();
            match state.apply(&msg) {
                Ok(true) => {
                    if let Some(inst) = find_bybit_instrument(&demand.books, seg.kind, &symbol) {
                        *seq = seq.saturating_add(1).max(1);
                        if let Ok(snapshot) = state.to_depth_snapshot(
                            &inst.instrument_id,
                            &inst.entitlement_id,
                            generation,
                            *seq,
                            ts_ms,
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
                    if let Some(inst) = find_bybit_instrument(&demand.quotes, seg.kind, &symbol) {
                        *seq = seq.saturating_add(1).max(1);
                        if let Some(quote) = state.to_top_of_book(
                            &inst.instrument_id,
                            &inst.entitlement_id,
                            generation,
                            *seq,
                            ts_ms,
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
                Ok(false) => {}
                Err(_) => {
                    state.reset();
                    let topic = format!("orderbook.200.{symbol}");
                    if let Some(sock) = seg.socket.as_mut()
                        && (sock
                            .send_text(&build_unsubscribe(std::slice::from_ref(&topic)))
                            .is_err()
                            || sock
                                .send_text(&build_subscribe(std::slice::from_ref(&topic)))
                                .is_err())
                    {
                        seg.close();
                    }
                }
            }
        }
        BybitWsEvent::Kline { symbol, data } => {
            if let Ok(candles) = decode_live_candle(&data) {
                let provider_symbol = format!("{}:{symbol}", seg.kind.category());
                for (interval, live) in candles {
                    emit_data(
                        sink,
                        events,
                        CryptoRealtimeEvent::Candle(
                            generation,
                            provider_symbol.clone(),
                            interval,
                            live.bar,
                        ),
                        wake,
                    );
                }
            }
        }
        BybitWsEvent::Ack | BybitWsEvent::Ignored => {}
    }
}
