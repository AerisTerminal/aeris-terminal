//! OKX V5 public market-provider registration, catalog worker, history fetcher,
//! and realtime dual-socket (`public` for books/trades, `business` for candles) worker.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender},
    },
    time::{Duration, Instant},
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
use aeris_okx_market_adapter::{
    OKX_BUSINESS_WS_URL, OKX_PUBLIC_WS_URL, OkxBookApplyOutcome, OkxBookState, OkxCatalog,
    OkxHttpClient, OkxMarketKind, OkxWsEvent, build_ping, build_subscribe, build_unsubscribe,
    decode_books, decode_live_candle, decode_trades, okx_interval_for_period,
    parse_provider_symbol, parse_ws_frame, period_for_okx_interval,
};
use aeris_platform_runtime::{MarketSocket, MarketSocketEvent};

use crate::crypto_runtime::{
    CATALOG_POLL_INTERVAL, CATALOG_REFRESH_INTERVAL, CONNECT_TIMEOUT, CryptoCatalogControl,
    CryptoCatalogEvent, CryptoDemand, CryptoHistorySource, CryptoInstrumentDemand,
    CryptoRealtimeControl, CryptoRealtimeEvent, READ_TIMEOUT, SharedCryptoCatalogs,
    build_install_instrument, crypto_provider_spec, emit_data, emit_lifecycle, now_unix_nanos,
    publish_catalog_event, reject_catalog,
};

pub(crate) const OKX_PUBLIC_ACCOUNT_ID: &str = "okx-public";
pub(crate) const OKX_PUBLIC_ENTITLEMENT_ID: &str = "okx-public";
const CATALOG_WORKER_NAME: &str = "aeris-okx-catalog";
const PROVIDER_WORKER_NAME: &str = "aeris-okx-provider";
const PING_INTERVAL: Duration = Duration::from_secs(20);
const SILENCE_TIMEOUT: Duration = Duration::from_secs(45);
const ONE_FIXED: i64 = 100_000_000;

pub(crate) const OKX_PRESENTATION: ProviderPresentationDescriptor =
    ProviderPresentationDescriptor {
        id: "okx",
        display_name: "OKX",
        chart_interval_labels: &[
            "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1D", "1W", "1M",
        ],
        daily_bars: ProviderDailyBarCadence::FixedDay,
        search_hint: "Search OKX perpetual swaps & spot",
        logo_key: "okx",
        broker_venue_id: None,
        attribution: None,
        catalog_symbol: ProviderCatalogSymbol::ProviderSymbol,
        selection_entitlement_id: OKX_PUBLIC_ENTITLEMENT_ID,
        catalog_refresh_on_startup: true,
        ready_label_suffix: "",
        feed_level: ProviderFeedLevel::PriceLevels,
        search_categories_available: false,
        market_screen_available: false,
        market_scan_available: false,
        trades_available: true,
        connection_kind: ProviderConnectionKind::Public,
    };

pub(crate) const OKX_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: "okx",
    presentation: &OKX_PRESENTATION,
    login_schema: None,
    capabilities: OKX_CAPABILITIES,
    demand_flush: DemandFlushPolicy::Dirty,
};

const OKX_CAPABILITIES: ProviderCapabilities = ProviderCapabilities {
    account_id: OKX_PUBLIC_ACCOUNT_ID,
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
        worker: "aeris-okx-history",
        supported_period: crate::bybit::bybit_supported_period,
    },
    recovery_policy: ProviderRecoveryPolicy::WorkerReconcilesDemand,
    alert_demand_update: AlertDemandUpdate::MarkDirty,
    live_model: LiveModel::ProviderCandles,
    calendar_bar_stamp: CalendarBarStamp::TradingDateMidnightUtc,
    overflow_recovery_detail: "Local market event queue overflow; repairing continuity",
    candle_requires_connected: true,
    candle_correction_detail: "OKX candle replacement requires covering history",
    candle_wire_interval: Some(okx_candle_interval),
    candle_demand_policy: CandleDemandPolicy::ReconcileAfterSelection,
    trade_demand_policy: TradeDemandPolicy::SessionManaged,
    instrument_missing_detail: "OKX instrument is not installed",
    catalog_session_binding: CatalogSessionBinding::CurrentSession,
    maximum_order_level_instruments: 0,
    recovers_silence: true,
};

pub(crate) fn provider_spec(
    realtime_enabled: bool,
    shared_catalogs: Arc<SharedCryptoCatalogs>,
) -> ProviderRuntimeSpec {
    let ct_vals: Arc<RwLock<BTreeMap<String, i64>>> = Arc::new(RwLock::new(BTreeMap::new()));
    let ct_vals_cat = Arc::clone(&ct_vals);
    let ct_vals_rt = Arc::clone(&ct_vals);
    crypto_provider_spec(
        OKX_DESCRIPTOR,
        CryptoHistorySource::Okx(OkxHttpClient::default()),
        realtime_enabled,
        CATALOG_WORKER_NAME,
        PROVIDER_WORKER_NAME,
        "OKX live session is recovering",
        okx_interval_for_period,
        move |controls, events, ws_gen, stop| {
            run_catalog(
                controls,
                events,
                ws_gen,
                stop,
                &ct_vals_cat,
                &shared_catalogs,
            );
        },
        move |controls, events, stop, ws_gen, wake| {
            run_realtime(controls, events, stop, ws_gen, wake, &ct_vals_rt);
        },
    )
    .with_open_interest(Arc::new(
        aeris_okx_market_adapter::open_interest::fetch_open_interest,
    ))
}

fn okx_candle_interval(period: BarPeriod) -> Result<String, String> {
    okx_interval_for_period(period)
        .map(str::to_owned)
        .map_err(|_| "OKX history is unavailable for this interval".to_string())
}

fn update_ct_vals(ct_vals: &RwLock<BTreeMap<String, i64>>, catalog: &OkxCatalog) {
    if let Ok(mut guard) = ct_vals.write() {
        for item in catalog.instruments() {
            guard.insert(item.inst_id.clone(), item.ct_val_fixed);
        }
    }
}

#[allow(clippy::too_many_lines)]
fn run_catalog(
    controls: &Receiver<CryptoCatalogControl>,
    events: &CatalogPublisher<CryptoCatalogEvent>,
    ws_generation: &Arc<AtomicU64>,
    stop: &Arc<AtomicBool>,
    ct_vals: &RwLock<BTreeMap<String, i64>>,
    shared_catalogs: &SharedCryptoCatalogs,
) {
    let mut catalog: Option<Arc<OkxCatalog>> = None;
    let mut fetched_at: Option<Instant> = None;
    let mut selection_generation: u64 = 0;
    let mut next_refresh = Instant::now();

    // Warm the public catalog on startup so the symbol browser populates instantly.
    if !stop.load(Ordering::Acquire)
        && let Ok(loaded) = shared_catalogs.get_or_fetch_okx(false)
    {
        update_ct_vals(ct_vals, &loaded);
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
                    match shared_catalogs.get_or_fetch_okx(false) {
                        Ok(loaded) => {
                            update_ct_vals(ct_vals, &loaded);
                            catalog = Some(loaded);
                            fetched_at = Some(Instant::now());
                            next_refresh = Instant::now() + CATALOG_REFRESH_INTERVAL;
                        }
                        Err(_error) => {
                            if reject_catalog(
                                events,
                                search.consumer_id,
                                "okx",
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
                    match shared_catalogs.get_or_fetch_okx(false) {
                        Ok(loaded) => {
                            update_ct_vals(ct_vals, &loaded);
                            catalog = Some(loaded);
                            fetched_at = Some(Instant::now());
                            next_refresh = Instant::now() + CATALOG_REFRESH_INTERVAL;
                        }
                        Err(_error) => {
                            if reject_catalog(
                                events,
                                select.consumer_id,
                                "okx",
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
                        "okx",
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
                    "okx",
                    OKX_PUBLIC_ENTITLEMENT_ID,
                    ws_generation.load(Ordering::Acquire),
                    selection_generation,
                    item.instrument_id.clone(),
                    item.provider_symbol.clone(),
                    item.display_symbol.clone(),
                    item.kind.exchange_label().to_string(),
                    item.quantity_decimals,
                    item.tick_size_fixed,
                    item.kind == OkxMarketKind::Spot,
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
            match shared_catalogs.get_or_fetch_okx(true) {
                Ok(loaded) => {
                    update_ct_vals(ct_vals, &loaded);
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

struct OkxSocket {
    url: &'static str,
    socket: Option<MarketSocket>,
    active_args: BTreeSet<(String, String)>,
    last_activity: Instant,
    last_ping: Instant,
}

impl OkxSocket {
    fn new(url: &'static str) -> Self {
        let now = Instant::now();
        Self {
            url,
            socket: None,
            active_args: BTreeSet::new(),
            last_activity: now,
            last_ping: now,
        }
    }

    fn close(&mut self) {
        if let Some(mut sock) = self.socket.take() {
            sock.close();
        }
        self.active_args.clear();
    }

    fn reconcile(
        &mut self,
        desired: &BTreeSet<(String, String)>,
        stop: &Arc<AtomicBool>,
    ) -> CryptoResult<()> {
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
            self.active_args.clear();
        }
        let to_unsub: Vec<(String, String)> =
            self.active_args.difference(desired).cloned().collect();
        let to_sub: Vec<(String, String)> =
            desired.difference(&self.active_args).cloned().collect();
        if let Some(sock) = self.socket.as_mut() {
            if !to_unsub.is_empty() {
                sock.send_text(&build_unsubscribe(&to_unsub))
                    .map_err(|e| CryptoError::Transport(e.to_string()))?;
                for a in to_unsub {
                    self.active_args.remove(&a);
                }
            }
            if !to_sub.is_empty() {
                sock.send_text(&build_subscribe(&to_sub))
                    .map_err(|e| CryptoError::Transport(e.to_string()))?;
                for a in to_sub {
                    self.active_args.insert(a);
                }
            }
        }
        Ok(())
    }
}

fn find_okx_instrument<'a>(
    list: &'a [CryptoInstrumentDemand],
    inst_id: &str,
) -> Option<&'a CryptoInstrumentDemand> {
    list.iter().find(|item| {
        parse_provider_symbol(&item.provider_symbol).is_ok_and(|sym| sym.inst_id == inst_id)
    })
}

#[allow(clippy::too_many_lines)]
fn run_realtime(
    controls: &Receiver<CryptoRealtimeControl>,
    events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
    stop: &Arc<AtomicBool>,
    ws_generation: &Arc<AtomicU64>,
    wake: &ProviderCoordinatorWake,
    ct_vals: &RwLock<BTreeMap<String, i64>>,
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

        let mut public_ws = OkxSocket::new(OKX_PUBLIC_WS_URL);
        let mut business_ws = OkxSocket::new(OKX_BUSINESS_WS_URL);
        let mut books: BTreeMap<String, OkxBookState> = BTreeMap::new();
        let mut trade_dedup: BTreeMap<String, TradeDedupRing<String>> = BTreeMap::new();
        let mut seq: u64 = 0;
        let mut demand_dirty = true;
        let mut failed = false;

        'session: loop {
            if stop.load(Ordering::Acquire) {
                public_ws.close();
                business_ws.close();
                return;
            }
            while let Ok(ctrl) = controls.try_recv() {
                match ctrl {
                    CryptoRealtimeControl::Connect(_) | CryptoRealtimeControl::Stop => {
                        public_ws.close();
                        business_ws.close();
                        break 'session;
                    }
                    CryptoRealtimeControl::Subscribe(next) => {
                        demand = next;
                        demand_dirty = true;
                    }
                }
            }

            if demand_dirty {
                let mut pub_args = BTreeSet::new();
                for t in &demand.trades {
                    if let Ok(sym) = parse_provider_symbol(&t.provider_symbol) {
                        pub_args.insert(("trades".to_string(), sym.inst_id));
                    }
                }
                for inst in demand.quotes.iter().chain(&demand.books) {
                    if let Ok(sym) = parse_provider_symbol(&inst.provider_symbol) {
                        pub_args.insert(("books".to_string(), sym.inst_id));
                    }
                }
                let mut biz_args = BTreeSet::new();
                for c in &demand.candles {
                    if let Ok(sym) = parse_provider_symbol(&c.instrument.provider_symbol) {
                        biz_args.insert((format!("candle{}", c.interval), sym.inst_id));
                    }
                }
                if public_ws.reconcile(&pub_args, stop).is_err()
                    || business_ws.reconcile(&biz_args, stop).is_err()
                {
                    failed = true;
                    break 'session;
                }
                demand_dirty = false;
            }

            let mut any_socket = false;
            for (is_public, ws) in [(true, &mut public_ws), (false, &mut business_ws)] {
                if ws.socket.is_none() {
                    continue;
                }
                any_socket = true;
                if ws.last_activity.elapsed() > SILENCE_TIMEOUT {
                    failed = true;
                    break 'session;
                }
                if ws.last_ping.elapsed() >= PING_INTERVAL {
                    if let Some(sock) = ws.socket.as_mut()
                        && sock.send_text(&build_ping()).is_err()
                    {
                        failed = true;
                        break 'session;
                    }
                    ws.last_ping = Instant::now();
                }
                let read = match ws.socket.as_mut() {
                    Some(sock) => sock.read_event(Instant::now() + READ_TIMEOUT),
                    None => continue,
                };
                match read {
                    Ok(MarketSocketEvent::Text(text)) => {
                        ws.last_activity = Instant::now();
                        handle_okx_frame(
                            is_public,
                            ws,
                            &text,
                            &demand,
                            ct_vals,
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

            if !any_socket {
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        public_ws.close();
        business_ws.close();
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
fn handle_okx_frame(
    is_public: bool,
    ws: &mut OkxSocket,
    text: &str,
    demand: &CryptoDemand,
    ct_vals: &RwLock<BTreeMap<String, i64>>,
    books: &mut BTreeMap<String, OkxBookState>,
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
    match parsed {
        OkxWsEvent::Pong => {
            emit_data(
                sink,
                events,
                CryptoRealtimeEvent::Heartbeat(generation, None),
                wake,
            );
        }
        OkxWsEvent::Trades { inst_id, data } if is_public => {
            let Some(inst) = find_okx_instrument(&demand.trades, &inst_id) else {
                return;
            };
            let ct_val = ct_vals
                .read()
                .ok()
                .and_then(|map| map.get(&inst_id).copied())
                .unwrap_or(ONE_FIXED);
            if let Ok(decoded) = decode_trades(
                &data,
                &inst.instrument_id,
                &inst.entitlement_id,
                ct_val,
                generation,
                seq,
                received_nanos,
            ) {
                let ring = trade_dedup
                    .entry(inst_id)
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
        OkxWsEvent::Books {
            inst_id,
            action,
            data,
        } if is_public => {
            let ct_val = ct_vals
                .read()
                .ok()
                .and_then(|map| map.get(&inst_id).copied())
                .unwrap_or(ONE_FIXED);
            let Ok(messages) = decode_books(&data, &action, ct_val) else {
                return;
            };
            let state = books.entry(inst_id.clone()).or_default();
            for msg in messages {
                match state.apply(&msg) {
                    OkxBookApplyOutcome::Updated => {
                        if let Some(inst) = find_okx_instrument(&demand.books, &inst_id) {
                            *seq = seq.saturating_add(1).max(1);
                            if let Ok(snapshot) = state.to_depth_snapshot(
                                &inst.instrument_id,
                                &inst.entitlement_id,
                                generation,
                                *seq,
                                msg.ts_ms,
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
                        if let Some(inst) = find_okx_instrument(&demand.quotes, &inst_id) {
                            *seq = seq.saturating_add(1).max(1);
                            if let Some(quote) = state.to_top_of_book(
                                &inst.instrument_id,
                                &inst.entitlement_id,
                                generation,
                                *seq,
                                msg.ts_ms,
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
                    OkxBookApplyOutcome::NoChange => {}
                    OkxBookApplyOutcome::NeedsResync => {
                        let arg = [("books".to_string(), inst_id.clone())];
                        if let Some(sock) = ws.socket.as_mut()
                            && (sock.send_text(&build_unsubscribe(&arg)).is_err()
                                || sock.send_text(&build_subscribe(&arg)).is_err())
                        {
                            ws.close();
                        }
                    }
                }
            }
        }
        OkxWsEvent::Candle {
            inst_id,
            interval,
            data,
        } if !is_public => {
            let Some(period) = period_for_okx_interval(&interval) else {
                return;
            };
            let kind = if inst_id.ends_with("-SWAP") {
                OkxMarketKind::Swap
            } else {
                OkxMarketKind::Spot
            };
            let provider_symbol = format!("{}:{inst_id}", kind.prefix());
            if let Ok(candles) = decode_live_candle(&data, kind, period) {
                for live in candles {
                    emit_data(
                        sink,
                        events,
                        CryptoRealtimeEvent::Candle(
                            generation,
                            provider_symbol.clone(),
                            interval.clone(),
                            live.bar,
                        ),
                        wake,
                    );
                }
            }
        }
        _ => {}
    }
}
