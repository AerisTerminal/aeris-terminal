use super::{
    ActiveWorkerGuard, Arc, AtomicBool, BTreeMap, BTreeSet, COINBASE_PUBLIC_ACCOUNT_ID,
    COMMAND_CAPACITY, Cell, ClientId, CoinbaseConfig, CoinbaseHistoryCapabilityAdapter,
    CoinbaseInterval, CoinbaseLevel2Book, CoinbaseLevel2Outcome, CoinbaseRealtimeWorkerState,
    CoinbaseSession, CoinbaseSpotProduct, Command, ConsumerId, ConsumerIdentity,
    ConsumerResourceClass, DataClass, Duration, ENTITLEMENT_CLASS, FormingBar, GenerationId,
    HISTORY_CAPACITY, HistoryPageRequest, HistoryRange, HistoryRequest, HistoryRequestKind,
    HistorySnapshot, HistorySource, HotSeries, InstallProviderInstrument, Instant,
    LiveCoinbaseHistory, LiveCoinbaseRealtime, LiveRithmicHistory, LocalHistoryStore,
    MAXIMUM_PUBLISHED_DEPTH_LEVELS, MarketBar, MarketEngine, MarketRuntime, MarketService,
    MarketServiceStatus, Mutex, NonZeroUsize, Ordering, OwnedCoordinatorChannels,
    ProviderCatalogChannels, ProviderCatalogCommand, ProviderCatalogDispatch, ProviderDispatch,
    ProviderDispatchRecord, ProviderGeneration, ProviderRealtimeChannels, ProviderRealtimeDispatch,
    ProviderRealtimeSpec, ProviderRuntimeEvent, ProviderRuntimeLifecycle, ProviderRuntimeRecord,
    ProviderRuntimeRegistry, ProviderRuntimeSpec, REALTIME_CAPACITY,
    RITHMIC_REALTIME_CONTROL_CAPACITY, RealtimeControl, RealtimeEvent, RealtimeProduct,
    RealtimeSource, Receiver, Reply, ResourceMode, RithmicCatalogControl, RithmicRealtimeControl,
    RithmicRealtimeEvent, STORAGE_CAPACITY, SearchProviderInstruments, SelectProviderInstrument,
    SeriesKey, StartedProviderRuntime, SyncSender, SystemTime, TrySendError, UNIX_EPOCH, Viewport,
    WorkspaceId, WorkspaceState, aggregate_coinbase_bars, available_memory_bytes,
    coinbase_catalog_dispatch_error, coinbase_series_profile, configured_engine,
    configured_reconnect_delay, current_unix_nanos, decode_history_bar, envelope, id,
    internal_series, mpsc, retained_hot_series, spawn_coordinator, spawn_history_worker,
    spawn_realtime_worker, spawn_storage_worker, thread, try_emit_realtime,
    try_send_rithmic_catalog, validate_provider_instrument, validate_provider_search,
    validate_provider_selection, warm_series,
};

#[cfg(test)]
use super::{CoinbaseCatalogControl, test_coinbase_eth_instrument, test_coinbase_instrument};

impl LiveCoinbaseHistory {
    pub(super) fn try_new() -> Result<Self, String> {
        CoinbaseHistoryCapabilityAdapter::try_new()
            .map(|adapter| Self { adapter })
            .map_err(|error| error.to_string())
    }
}

/// Projects one installed instrument into the adapter's product identity.
fn coinbase_spot_product(
    installed: &InstallProviderInstrument,
) -> Result<CoinbaseSpotProduct, String> {
    Ok(CoinbaseSpotProduct {
        product_id: installed.provider_symbol.clone(),
        instrument_id: installed.instrument_id.clone(),
        display_symbol: installed.display_symbol.clone(),
        base_currency: installed
            .provider_symbol
            .split_once('-')
            .map_or_else(String::new, |(base, _)| base.to_string()),
        quote_currency: installed
            .provider_symbol
            .split_once('-')
            .map_or_else(String::new, |(_, quote)| quote.to_string()),
        price_scale: u8::try_from(installed.price_scale)
            .map_err(|_| "Coinbase price scale is invalid".to_string())?,
        quantity_scale: u8::try_from(installed.quantity_scale)
            .map_err(|_| "Coinbase quantity scale is invalid".to_string())?,
    })
}

impl HistorySource for LiveCoinbaseHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        self.adapter.set_stop(Arc::clone(&request.stop));
        let series = &request.series;
        let installed = request
            .instrument
            .as_ref()
            .ok_or_else(|| "Coinbase instrument is not installed".to_string())?;
        let profile = coinbase_series_profile(series, installed)?;
        let kind = request.kind;
        self.adapter
            .register_product(&coinbase_spot_product(installed)?);
        let now_seconds = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "system clock is unavailable".to_string())?
                .as_secs(),
        )
        .map_err(|_| "Coinbase history end time overflowed".to_string())?;
        let live_edge_seconds = profile.interval.bucket_start(now_seconds)?;
        // A live-edge page ends *on* the open bucket rather than before it, so
        // the provider also returns the candle that is still forming. It is
        // split back out below: history keeps only closed buckets, and the open
        // one seeds the live aggregator so a freshly selected chart shows the
        // OHLCV that accrued before the trader got there.
        let source_seconds = profile.interval.source().1;
        let end_unix_nanos = (now_seconds - now_seconds.rem_euclid(source_seconds))
            .checked_add(source_seconds)
            .ok_or_else(|| "Coinbase history end time overflowed".to_string())?
            .checked_mul(1_000_000_000)
            .ok_or_else(|| "Coinbase history end time overflowed".to_string())?;
        let range = request.range.unwrap_or_else(|| {
            let start_seconds = i64::try_from(request.maximum_bars)
                .ok()
                .and_then(|count| {
                    profile
                        .interval
                        .shift_bucket(live_edge_seconds, -count)
                        .ok()
                })
                .unwrap_or(0);
            HistoryRange {
                start_unix_nanos: start_seconds.saturating_mul(1_000_000_000),
                end_unix_nanos,
            }
        });
        let request = HistoryPageRequest {
            provider_id: "coinbase".to_string(),
            account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
            entitlement_revision: ENTITLEMENT_CLASS.to_string(),
            instrument_id: series.instrument_id.clone(),
            data_class: DataClass::Bars,
            resolution: profile.resolution.to_string(),
            range,
            maximum_items: NonZeroUsize::new(request.maximum_bars.min(350))
                .unwrap_or(NonZeroUsize::MIN),
            continuation: None,
        };
        let batch = self.adapter.fetch_paginated(&request)?;
        // The provider built this page at some instant no later than now, so
        // every trade the socket delivers after this point is definitely absent
        // from it. Taking the boundary *after* the response — rather than the
        // request's own end — is what keeps the buffered replay from counting a
        // trade the forming candle already contains.
        let served_at_unix_nanos = coinbase_history_handoff_boundary(
            profile.interval,
            live_edge_seconds,
            kind == HistoryRequestKind::Initial,
        )?;
        let source_bars = batch
            .items
            .iter()
            .map(decode_history_bar)
            .collect::<Result<Vec<_>, _>>()?;
        let requested_end_seconds = range.end_unix_nanos.div_euclid(1_000_000_000);
        let completed_before_seconds = live_edge_seconds.min(requested_end_seconds);
        let (bars, _) = if source_bars.is_empty() {
            (
                Vec::new(),
                axiusflow_coinbase_market_adapter::CoinbaseAggregationDiagnostics::default(),
            )
        } else {
            aggregate_coinbase_bars(
                &source_bars,
                profile.interval,
                Some(completed_before_seconds),
            )?
        };
        // Only the live-edge load hands over to the aggregator; a backfill is a
        // window into closed history even when it happens to reach the edge.
        let forming =
            if kind == HistoryRequestKind::Initial && requested_end_seconds >= live_edge_seconds {
                forming_coinbase_bucket(&source_bars, profile.interval, live_edge_seconds)?
                    .map(|bar| FormingBar { bar, trades: None })
            } else {
                None
            };
        let handoff_boundary_unix_nanos = if forming.is_some() {
            served_at_unix_nanos
        } else if kind == HistoryRequestKind::Initial {
            live_edge_seconds
                .saturating_mul(1_000_000_000)
                .saturating_sub(1)
        } else {
            range.end_unix_nanos
        };
        Ok(HistorySnapshot {
            price_scale: profile.price_scale,
            quantity_scale: profile.quantity_scale,
            bars,
            forming,
            handoff_boundary_unix_nanos: Some(handoff_boundary_unix_nanos),
            confirmed_empty: source_bars.is_empty(),
        })
    }
}

fn coinbase_history_handoff_boundary(
    interval: CoinbaseInterval,
    requested_edge_seconds: i64,
    initial: bool,
) -> Result<i64, String> {
    let served_at_unix_nanos = current_unix_nanos()?;
    if initial
        && !coinbase_history_edge_is_current(
            interval,
            requested_edge_seconds,
            served_at_unix_nanos,
        )?
    {
        return Err("Coinbase current history crossed a candle boundary; retrying".to_string());
    }
    Ok(served_at_unix_nanos)
}

pub(super) fn coinbase_history_edge_is_current(
    interval: CoinbaseInterval,
    requested_edge_seconds: i64,
    served_at_unix_nanos: i64,
) -> Result<bool, String> {
    interval
        .bucket_start(served_at_unix_nanos.div_euclid(1_000_000_000))
        .map(|served_edge_seconds| served_edge_seconds == requested_edge_seconds)
}

/// Folds the source bars inside the still-open bucket into one forming bar.
///
/// A page is served at minute granularity, so a coarser target bucket spans
/// several source bars and the open one is whatever part of it has traded so
/// far. Returning `None` means the bucket has not traded yet, which is a real
/// answer: the aggregator then opens it on the first live trade.
pub(super) fn forming_coinbase_bucket(
    source_bars: &[MarketBar],
    interval: CoinbaseInterval,
    bucket_seconds: i64,
) -> Result<Option<MarketBar>, String> {
    let open = source_bars
        .iter()
        .filter(|bar| bar.exchange_timestamp_seconds >= bucket_seconds)
        .copied()
        .collect::<Vec<_>>();
    if open.is_empty() {
        return Ok(None);
    }
    let (mut bars, _) = aggregate_coinbase_bars(&open, interval, None)?;
    Ok(bars
        .pop()
        .filter(|bar| bar.exchange_timestamp_seconds == bucket_seconds))
}

impl HistorySource for LiveRithmicHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        if request.series.provider_id != "rithmic" {
            return Err("Rithmic history received another provider".to_string());
        }
        fetch_rithmic_history(request)
    }
}

fn fetch_rithmic_history(request: &HistoryRequest) -> Result<HistorySnapshot, String> {
    let installed = request
        .instrument
        .as_ref()
        .ok_or_else(|| "Rithmic instrument is not installed".to_string())?;
    let snapshot = crate::rithmic_history::fetch(
        &request.series,
        request.provider_generation.0.get(),
        installed,
        request.maximum_bars,
        &request.stop,
    )?;
    Ok(HistorySnapshot {
        price_scale: snapshot.price_scale,
        quantity_scale: snapshot.quantity_scale,
        bars: snapshot.bars,
        forming: snapshot.forming,
        handoff_boundary_unix_nanos: Some(snapshot.handoff_boundary_unix_nanos),
        confirmed_empty: false,
    })
}

impl LiveCoinbaseRealtime {
    pub(super) fn try_new() -> Self {
        Self {
            config: None,
            products: Vec::new(),
        }
    }

    fn take_product_update(
        &mut self,
        controls: &Receiver<RealtimeControl>,
        generation: ProviderGeneration,
        books: &std::cell::RefCell<Vec<CoinbaseLevel2Book>>,
    ) -> Option<Vec<String>> {
        let mut newest = None;
        while let Ok(RealtimeControl::Start(products)) = controls.try_recv() {
            newest = Some(products);
        }
        let products = newest?;
        let symbols = products
            .iter()
            .map(|product| product.symbol.clone())
            .collect::<Vec<_>>();
        CoinbaseConfig::try_new(symbols.clone()).ok()?;
        // Books of retained products survive the update: recreating every
        // book dropped live depth until the venue re-sent snapshots it only
        // emits on subscribe. Added products start awaiting; removed ones go
        // away with the unsubscribe the session sends below.
        let mut books = books.borrow_mut();
        books.retain(|book| symbols.iter().any(|symbol| symbol == book.product_id()));
        for product in &products {
            if !books.iter().any(|book| book.product_id() == product.symbol)
                && let Ok(book) = CoinbaseLevel2Book::try_new(
                    product.symbol.clone(),
                    product.price_scale,
                    product.quantity_scale,
                    generation.0.get(),
                )
            {
                books.push(book);
            }
        }
        self.products = products;
        Some(symbols)
    }
}

impl RealtimeSource for LiveCoinbaseRealtime {
    fn configure(&mut self, products: Vec<RealtimeProduct>) -> Result<(), String> {
        let symbols = products
            .iter()
            .map(|product| product.symbol.clone())
            .collect::<Vec<_>>();
        self.config = Some(CoinbaseConfig::try_new(symbols).map_err(|error| error.to_string())?);
        self.products = products;
        Ok(())
    }

    fn run_generation(
        &mut self,
        generation: ProviderGeneration,
        controls: &Receiver<RealtimeControl>,
        events: &SyncSender<RealtimeEvent>,
        overflow: &AtomicBool,
        stop: &Arc<AtomicBool>,
    ) -> bool {
        let Some(config) = self.config.clone() else {
            return false;
        };
        let Ok(connection) = CoinbaseSession::new(config).connect_cancellable(Arc::clone(stop))
        else {
            return false;
        };
        if events.send(RealtimeEvent::Connected(generation)).is_err() {
            return false;
        }
        let queue_failed = Cell::new(false);
        let books = std::cell::RefCell::new(
            self.products
                .iter()
                .filter_map(|product| {
                    CoinbaseLevel2Book::try_new(
                        product.symbol.clone(),
                        product.price_scale,
                        product.quantity_scale,
                        generation.0.get(),
                    )
                    .ok()
                })
                .collect::<Vec<_>>(),
        );
        let _ = connection.collect_until_stopped_with_subscription_updates(
            &mut || queue_failed.get() || stop.load(Ordering::Acquire),
            &mut || self.take_product_update(controls, generation, &books),
            &mut |trade| {
                if !try_emit_realtime(
                    events,
                    overflow,
                    RealtimeEvent::Trade(generation, trade.clone()),
                ) {
                    queue_failed.set(true);
                }
            },
            &mut || {
                if !try_emit_realtime(events, overflow, RealtimeEvent::Heartbeat(generation)) {
                    queue_failed.set(true);
                }
            },
            &mut |bytes| {
                let Ok(received) = current_unix_nanos() else {
                    return;
                };
                for book in books.borrow_mut().iter_mut() {
                    let Ok(outcome) = book.apply_message(bytes, received) else {
                        continue;
                    };
                    let mut book = match outcome {
                        CoinbaseLevel2Outcome::Snapshot(book)
                        | CoinbaseLevel2Outcome::Deltas { book, .. } => book,
                        CoinbaseLevel2Outcome::RecoveryRequired
                        | CoinbaseLevel2Outcome::Ignored => continue,
                    };
                    // Coinbase retains far more depth than the engine publishes.
                    book.bids.truncate(MAXIMUM_PUBLISHED_DEPTH_LEVELS);
                    book.asks.truncate(MAXIMUM_PUBLISHED_DEPTH_LEVELS);
                    if !try_emit_realtime(
                        events,
                        overflow,
                        RealtimeEvent::Depth(generation, Box::new(book)),
                    ) {
                        queue_failed.set(true);
                    }
                }
            },
        );
        true
    }
}

impl ProviderRuntimeRegistry {
    pub(super) fn start(
        specs: Vec<ProviderRuntimeSpec>,
        completions: &SyncSender<Command>,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
    ) -> Result<Self, String> {
        let mut registry = Self {
            records: BTreeMap::new(),
        };
        for spec in specs {
            if registry.records.contains_key(spec.provider_id) {
                registry.cancel_and_join();
                return Err(format!(
                    "provider runtime {} is configured more than once",
                    spec.provider_id
                ));
            }
            let provider_id = spec.provider_id;
            let record = match Self::start_record(spec, completions, engine, active_workers) {
                Ok(record) => record,
                Err(error) => {
                    registry.cancel_and_join();
                    return Err(error);
                }
            };
            registry.records.insert(provider_id, record);
        }
        Ok(registry)
    }

    fn start_record(
        spec: ProviderRuntimeSpec,
        completions: &SyncSender<Command>,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
    ) -> Result<ProviderRuntimeRecord, String> {
        let cancellation = Arc::new(AtomicBool::new(false));
        let lifecycle = Arc::new(ProviderRuntimeLifecycle::default());
        let (history_tx, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
        let history_worker = spawn_history_worker(
            match spec.provider_id {
                "coinbase" => "axiusflow-coinbase-history",
                "rithmic" => "axiusflow-rithmic-history",
                _ => "axiusflow-provider-history",
            },
            spec.history,
            history_rx,
            completions.clone(),
            Arc::clone(&cancellation),
            Arc::clone(active_workers),
        )?;
        let started = StartedProviderRuntime {
            history: history_tx,
            cancellation,
            lifecycle,
            workers: vec![history_worker],
        };
        match spec.realtime {
            ProviderRealtimeSpec::Coinbase(realtime) => {
                Self::start_coinbase_record(started, realtime, engine, active_workers)
            }
            ProviderRealtimeSpec::Rithmic { enabled } => {
                Self::start_rithmic_record(started, enabled, engine, active_workers)
            }
        }
    }

    fn start_coinbase_record(
        mut started: StartedProviderRuntime,
        realtime: Option<Box<dyn RealtimeSource>>,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
    ) -> Result<ProviderRuntimeRecord, String> {
        let reconnect_delay = if realtime.is_some() {
            match configured_reconnect_delay(engine, "coinbase") {
                Ok(delay) => Some(delay),
                Err(error) => {
                    started.cancel_and_join();
                    return Err(error);
                }
            }
        } else {
            None
        };
        let (catalog_controls, catalog_events, catalog_worker) =
            match crate::coinbase_catalog::start(Arc::clone(&started.cancellation)) {
                Ok(catalog) => catalog,
                Err(error) => {
                    started.cancel_and_join();
                    return Err(error);
                }
            };
        started.workers.push(catalog_worker);
        let (realtime_events_tx, realtime_events) = mpsc::sync_channel(REALTIME_CAPACITY);
        let (realtime_controls, realtime_controls_rx) = mpsc::sync_channel(1);
        let state = CoinbaseRealtimeWorkerState {
            overflow: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(true)),
            lifecycle: Arc::clone(&started.lifecycle),
            active_workers: Arc::clone(active_workers),
        };
        let enabled = realtime.is_some();
        if let Some(realtime) = realtime {
            let Some(reconnect_delay) = reconnect_delay else {
                started.cancel_and_join();
                return Err("Coinbase reconnect policy is unavailable".to_string());
            };
            let worker = match spawn_realtime_worker(
                realtime,
                realtime_controls_rx,
                realtime_events_tx,
                reconnect_delay,
                &state,
            ) {
                Ok(worker) => worker,
                Err(error) => {
                    state.stop.store(true, Ordering::Release);
                    drop(catalog_controls);
                    started.cancel_and_join();
                    return Err(error);
                }
            };
            started.workers.push(worker);
        }
        Ok(ProviderRuntimeRecord {
            history: started.history,
            cancellation: started.cancellation,
            lifecycle: started.lifecycle,
            realtime: ProviderRealtimeChannels::Coinbase {
                enabled,
                controls: realtime_controls,
                events: realtime_events,
                overflow: state.overflow,
                stop: state.stop,
            },
            catalog: ProviderCatalogChannels::Coinbase {
                controls: catalog_controls,
                events: catalog_events,
            },
            workers: started.workers,
        })
    }

    fn start_rithmic_record(
        mut started: StartedProviderRuntime,
        enabled: bool,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
    ) -> Result<ProviderRuntimeRecord, String> {
        let reconnect_delay = if enabled {
            match configured_reconnect_delay(engine, "rithmic") {
                Ok(delay) => Some(delay),
                Err(error) => {
                    started.cancel_and_join();
                    return Err(error);
                }
            }
        } else {
            None
        };
        let (catalog_events_tx, catalog_events) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (catalog_controls, catalog_controls_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (realtime_events_tx, realtime_events) = mpsc::sync_channel(REALTIME_CAPACITY);
        let (realtime_controls, realtime_controls_rx) =
            mpsc::sync_channel(RITHMIC_REALTIME_CONTROL_CAPACITY);
        if enabled {
            let worker_cancellation = Arc::clone(&started.cancellation);
            let worker_lifecycle = Arc::clone(&started.lifecycle);
            let Some(reconnect_delay) = reconnect_delay else {
                started.cancel_and_join();
                return Err("Rithmic reconnect policy is unavailable".to_string());
            };
            let worker_activity = Arc::clone(active_workers);
            let provider = thread::Builder::new()
                .name("axiusflow-rithmic-provider".to_string())
                .spawn(move || {
                    let _activity =
                        ActiveWorkerGuard::register("axiusflow-rithmic-provider", worker_activity);
                    crate::rithmic_realtime::run(
                        &catalog_controls_rx,
                        &catalog_events_tx,
                        &realtime_controls_rx,
                        &realtime_events_tx,
                        reconnect_delay,
                    );
                    if !worker_cancellation.load(Ordering::Acquire) {
                        worker_lifecycle
                            .mark_terminal_failure("Rithmic provider runtime stopped unexpectedly");
                    }
                })
                .map_err(|error| error.to_string());
            match provider {
                Ok(provider) => started.workers.push(provider),
                Err(error) => {
                    started.cancel_and_join();
                    return Err(error);
                }
            }
        }
        Ok(ProviderRuntimeRecord {
            history: started.history,
            cancellation: started.cancellation,
            lifecycle: started.lifecycle,
            realtime: ProviderRealtimeChannels::Rithmic {
                enabled,
                controls: realtime_controls,
                events: realtime_events,
            },
            catalog: ProviderCatalogChannels::Rithmic {
                enabled,
                controls: catalog_controls,
                events: catalog_events,
            },
            workers: started.workers,
        })
    }

    pub(super) fn dispatch(&self) -> ProviderDispatch<'_> {
        let records = self
            .records
            .iter()
            .map(|(provider_id, record)| {
                let realtime = match &record.realtime {
                    ProviderRealtimeChannels::Coinbase {
                        enabled,
                        controls,
                        events,
                        overflow,
                        stop,
                    } if *enabled => ProviderRealtimeDispatch::Coinbase {
                        controls,
                        events,
                        overflow,
                        stop,
                    },
                    ProviderRealtimeChannels::Rithmic {
                        enabled,
                        controls,
                        events,
                    } if *enabled => ProviderRealtimeDispatch::Rithmic { controls, events },
                    ProviderRealtimeChannels::Coinbase { .. }
                    | ProviderRealtimeChannels::Rithmic { .. } => {
                        ProviderRealtimeDispatch::Disabled
                    }
                };
                let catalog = match &record.catalog {
                    ProviderCatalogChannels::Coinbase { controls, events } => {
                        ProviderCatalogDispatch::Coinbase { controls, events }
                    }
                    ProviderCatalogChannels::Rithmic {
                        enabled: true,
                        controls,
                        events,
                    } => ProviderCatalogDispatch::Rithmic { controls, events },
                    ProviderCatalogChannels::Rithmic { .. } => ProviderCatalogDispatch::Disabled,
                };
                (
                    *provider_id,
                    ProviderDispatchRecord {
                        history: &record.history,
                        lifecycle: Some(&record.lifecycle),
                        realtime,
                        catalog,
                    },
                )
            })
            .collect();
        ProviderDispatch { records }
    }

    pub(super) fn cancel_and_join(&mut self) -> Vec<String> {
        for record in self.records.values() {
            record.cancellation.store(true, Ordering::Release);
            if let ProviderRealtimeChannels::Coinbase { stop, .. } = &record.realtime {
                stop.store(true, Ordering::Release);
            }
        }
        let records = std::mem::take(&mut self.records);
        let mut panicked = Vec::new();
        for (_, record) in records {
            let ProviderRuntimeRecord {
                history,
                realtime,
                catalog,
                workers,
                ..
            } = record;
            drop(history);
            drop(realtime);
            drop(catalog);
            for worker in workers {
                let name = worker.thread().name().unwrap_or("unnamed").to_string();
                if worker.join().is_err() {
                    panicked.push(name);
                }
            }
        }
        panicked
    }
}

impl Drop for ProviderRuntimeRegistry {
    fn drop(&mut self) {
        let _ = self.cancel_and_join();
    }
}

pub(super) fn join_runtime_workers(workers: Vec<thread::JoinHandle<()>>) {
    for worker in workers {
        let _ = worker.join();
    }
}

impl ProviderDispatch<'_> {
    pub(super) fn history(
        &self,
        provider_id: &str,
    ) -> Result<&SyncSender<HistoryRequest>, &'static str> {
        self.records
            .get(provider_id)
            .map(|record| record.history)
            .ok_or("provider history runtime is unavailable")
    }

    pub(super) fn detail(&self, provider_id: &str) -> Option<String> {
        self.records
            .get(provider_id)
            .and_then(|record| record.lifecycle)
            .and_then(ProviderRuntimeLifecycle::detail)
    }

    pub(super) fn rithmic_realtime_enabled(&self) -> bool {
        self.records
            .get("rithmic")
            .is_some_and(|record| record.realtime.rithmic_controls().is_some())
    }

    pub(super) fn observe_generation(
        &self,
        provider_id: &str,
        generation: u64,
        reconnecting: bool,
    ) {
        if let Some(lifecycle) = self
            .records
            .get(provider_id)
            .and_then(|record| record.lifecycle)
        {
            lifecycle.observe_generation(generation, reconnecting);
        }
    }

    pub(super) fn take_event(&self, lane: usize) -> Option<ProviderRuntimeEvent> {
        let event = match lane {
            0 => match &self.records.get("coinbase")?.realtime {
                ProviderRealtimeDispatch::Coinbase { events, .. } => events
                    .try_recv()
                    .ok()
                    .map(ProviderRuntimeEvent::CoinbaseRealtime),
                _ => None,
            },
            1 => match &self.records.get("rithmic")?.realtime {
                ProviderRealtimeDispatch::Rithmic { events, .. } => events
                    .try_recv()
                    .ok()
                    .map(ProviderRuntimeEvent::RithmicRealtime),
                _ => None,
            },
            2 => match &self.records.get("coinbase")?.catalog {
                ProviderCatalogDispatch::Coinbase { events, .. } => events
                    .try_recv()
                    .ok()
                    .map(ProviderRuntimeEvent::CoinbaseCatalog),
                _ => None,
            },
            3 => match &self.records.get("rithmic")?.catalog {
                ProviderCatalogDispatch::Rithmic { events, .. } => events
                    .try_recv()
                    .ok()
                    .map(ProviderRuntimeEvent::RithmicCatalog),
                _ => None,
            },
            _ => None,
        }?;
        match &event {
            ProviderRuntimeEvent::CoinbaseRealtime(event) => {
                let (generation, reconnecting) = match event {
                    RealtimeEvent::Connecting(generation)
                    | RealtimeEvent::Disconnected(generation) => (generation.0.get(), true),
                    RealtimeEvent::Connected(generation)
                    | RealtimeEvent::Trade(generation, _)
                    | RealtimeEvent::Depth(generation, _)
                    | RealtimeEvent::Heartbeat(generation) => (generation.0.get(), false),
                };
                self.observe_generation("coinbase", generation, reconnecting);
            }
            ProviderRuntimeEvent::RithmicRealtime(event) => {
                let (generation, reconnecting) = match event {
                    RithmicRealtimeEvent::Connecting(generation)
                    | RithmicRealtimeEvent::Recovering(generation)
                    | RithmicRealtimeEvent::Disconnected(generation) => (*generation, true),
                    RithmicRealtimeEvent::Connected(generation)
                    | RithmicRealtimeEvent::Heartbeat(generation)
                    | RithmicRealtimeEvent::Trade(generation, _)
                    | RithmicRealtimeEvent::Depth(generation, _) => (*generation, false),
                };
                self.observe_generation("rithmic", generation, reconnecting);
            }
            ProviderRuntimeEvent::CoinbaseCatalog(_) | ProviderRuntimeEvent::RithmicCatalog(_) => {}
        }
        Some(event)
    }

    pub(super) fn coinbase_overflowed(&self) -> bool {
        self.records.get("coinbase").is_some_and(|record| {
            matches!(
                &record.realtime,
                ProviderRealtimeDispatch::Coinbase { overflow, .. }
                    if overflow.swap(false, Ordering::AcqRel)
            )
        })
    }

    pub(super) fn authorize_catalog_consumer(&self, consumer_id: u64) -> Result<(), String> {
        match self.records.get("coinbase").map(|record| &record.catalog) {
            Some(ProviderCatalogDispatch::Coinbase { controls, .. }) => {
                controls.authorize_consumer(consumer_id)
            }
            #[cfg(test)]
            Some(ProviderCatalogDispatch::TestCoinbase { controls }) => {
                controls.authorize_consumer(consumer_id)
            }
            _ => Ok(()),
        }
    }

    pub(super) fn release_catalog_consumer(&self, consumer_id: u64) {
        match self.records.get("coinbase").map(|record| &record.catalog) {
            Some(ProviderCatalogDispatch::Coinbase { controls, .. }) => {
                controls.release_consumer(consumer_id);
            }
            #[cfg(test)]
            Some(ProviderCatalogDispatch::TestCoinbase { controls }) => {
                controls.release_consumer(consumer_id);
            }
            _ => {}
        }
    }

    pub(super) fn coinbase_catalog_event_is_current(
        &self,
        consumer_id: u64,
        authorization_generation: u64,
    ) -> bool {
        match self.records.get("coinbase").map(|record| &record.catalog) {
            Some(ProviderCatalogDispatch::Coinbase { controls, .. }) => {
                controls.is_authorized(consumer_id, authorization_generation)
            }
            #[cfg(test)]
            Some(ProviderCatalogDispatch::TestCoinbase { controls }) => {
                controls.is_authorized(consumer_id, authorization_generation)
            }
            _ => false,
        }
    }

    pub(super) fn dispatch_catalog(
        &self,
        provider_id: &str,
        command: ProviderCatalogCommand,
    ) -> Result<(), String> {
        let record = self
            .records
            .get(provider_id)
            .ok_or_else(|| format!("{provider_id} catalog worker is unavailable"))?;
        match (&record.catalog, command) {
            (
                ProviderCatalogDispatch::Coinbase { controls, .. },
                ProviderCatalogCommand::Search(search),
            ) => controls
                .try_search(search)
                .map_err(coinbase_catalog_dispatch_error),
            (
                ProviderCatalogDispatch::Coinbase { controls, .. },
                ProviderCatalogCommand::Select(selection),
            ) => controls
                .try_select(selection)
                .map_err(coinbase_catalog_dispatch_error),
            #[cfg(test)]
            (
                ProviderCatalogDispatch::TestCoinbase { controls },
                ProviderCatalogCommand::Search(search),
            ) => controls
                .try_search(search)
                .map_err(coinbase_catalog_dispatch_error),
            #[cfg(test)]
            (
                ProviderCatalogDispatch::TestCoinbase { controls },
                ProviderCatalogCommand::Select(selection),
            ) => controls
                .try_select(selection)
                .map_err(coinbase_catalog_dispatch_error),
            (
                ProviderCatalogDispatch::Rithmic { controls, .. },
                ProviderCatalogCommand::Search(search),
            ) => try_send_rithmic_catalog(
                controls,
                RithmicCatalogControl::Search(search),
                provider_id,
            ),
            (
                ProviderCatalogDispatch::Rithmic { controls, .. },
                ProviderCatalogCommand::Select(selection),
            ) => try_send_rithmic_catalog(
                controls,
                RithmicCatalogControl::Select(selection),
                provider_id,
            ),
            (ProviderCatalogDispatch::Disabled, _) => {
                Err(format!("{provider_id} catalog worker is unavailable"))
            }
        }
    }

    pub(super) fn start_coinbase_realtime(
        &self,
        products: Vec<RealtimeProduct>,
    ) -> Result<bool, String> {
        let Some(controls) = self
            .records
            .get("coinbase")
            .ok_or_else(|| "Coinbase live worker is unavailable".to_string())?
            .realtime
            .coinbase_controls()
        else {
            return Ok(false);
        };
        match controls.try_send(RealtimeControl::Start(products)) {
            Ok(()) => Ok(true),
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => {
                self.stop("coinbase");
                Ok(false)
            }
        }
    }

    pub(super) fn send_rithmic_realtime(
        &self,
        control: RithmicRealtimeControl,
    ) -> Result<bool, String> {
        let Some(controls) = self
            .records
            .get("rithmic")
            .ok_or_else(|| "Rithmic live worker is unavailable".to_string())?
            .realtime
            .rithmic_controls()
        else {
            return Ok(false);
        };
        match controls.try_send(control) {
            Ok(()) => Ok(true),
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => {
                Err("Rithmic live worker is unavailable".to_string())
            }
        }
    }

    pub(super) fn stop(&self, provider_id: &str) {
        let Some(record) = self.records.get(provider_id) else {
            return;
        };
        match &record.realtime {
            ProviderRealtimeDispatch::Coinbase { stop, .. } => {
                stop.store(true, Ordering::Release);
            }
            ProviderRealtimeDispatch::Rithmic { controls, .. } => {
                let _ = controls.try_send(RithmicRealtimeControl::Stop);
            }
            #[cfg(test)]
            ProviderRealtimeDispatch::TestCoinbase { stop, .. } => {
                stop.store(true, Ordering::Release);
            }
            #[cfg(test)]
            ProviderRealtimeDispatch::TestRithmic { controls } => {
                let _ = controls.try_send(RithmicRealtimeControl::Stop);
            }
            ProviderRealtimeDispatch::Disabled => {}
        }
    }
}

#[cfg(test)]
impl<'a> ProviderDispatch<'a> {
    pub(super) fn fixture(
        history: &'a SyncSender<HistoryRequest>,
        realtime: &'a SyncSender<RealtimeControl>,
        realtime_stop: &'a Arc<AtomicBool>,
    ) -> Self {
        Self {
            records: BTreeMap::from([
                (
                    "coinbase",
                    ProviderDispatchRecord {
                        history,
                        lifecycle: None,
                        realtime: ProviderRealtimeDispatch::TestCoinbase {
                            controls: realtime,
                            stop: realtime_stop,
                        },
                        catalog: ProviderCatalogDispatch::Disabled,
                    },
                ),
                (
                    "rithmic",
                    ProviderDispatchRecord {
                        history,
                        lifecycle: None,
                        realtime: ProviderRealtimeDispatch::Disabled,
                        catalog: ProviderCatalogDispatch::Disabled,
                    },
                ),
            ]),
        }
    }

    pub(super) fn set_coinbase_catalog_control(&mut self, control: &'a CoinbaseCatalogControl) {
        if let Some(record) = self.records.get_mut("coinbase") {
            record.catalog = ProviderCatalogDispatch::TestCoinbase { controls: control };
        }
    }

    pub(super) fn set_rithmic_realtime_control(
        &mut self,
        control: &'a SyncSender<RithmicRealtimeControl>,
    ) {
        if let Some(record) = self.records.get_mut("rithmic") {
            record.realtime = ProviderRealtimeDispatch::TestRithmic { controls: control };
        }
    }
}

impl MarketService {
    /// Starts the process-owned market coordinator and its bounded provider-history worker.
    ///
    /// # Errors
    /// Returns an error when provider configuration or either bounded worker cannot start.
    pub fn start(workspace: &WorkspaceState) -> Result<Self, String> {
        let hot_series = retained_hot_series(workspace, available_memory_bytes())?;
        let storage = LocalHistoryStore::open(
            &crate::default_engine_state_root()?
                .join("market-history")
                .join("coinbase"),
        )
        .map_err(|error| error.to_string());
        let service = Self::start_composed(
            vec![
                ProviderRuntimeSpec::coinbase(
                    Box::new(LiveCoinbaseHistory::try_new()?),
                    Some(Box::new(LiveCoinbaseRealtime::try_new())),
                ),
                ProviderRuntimeSpec::rithmic(Box::new(LiveRithmicHistory), true),
            ],
            Some(storage),
            hot_series.len(),
        )?;
        service.restore_hot_set(&hot_series)?;
        Ok(service)
    }

    pub(super) fn start_composed(
        providers: Vec<ProviderRuntimeSpec>,
        storage: Option<Result<LocalHistoryStore, String>>,
        hot_set_priority_count: usize,
    ) -> Result<Self, String> {
        let engine = configured_engine()?;
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (storage_tx, storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
        let shutdown = Arc::new(AtomicBool::new(false));
        let active_provider_workers = Arc::new(Mutex::new(BTreeSet::new()));
        let available_memory_bytes = available_memory_bytes();
        let provider_registry = ProviderRuntimeRegistry::start(
            providers,
            &command_tx,
            &engine,
            &active_provider_workers,
        )?;
        let workers = vec![
            spawn_storage_worker(
                storage,
                storage_rx,
                command_tx.clone(),
                Arc::clone(&shutdown),
            )?,
            spawn_coordinator(
                engine,
                OwnedCoordinatorChannels {
                    commands: command_rx,
                    storage: storage_tx,
                    providers: provider_registry,
                },
                Arc::clone(&shutdown),
                available_memory_bytes,
                hot_set_priority_count,
            )?,
        ];
        let service =
            Self::build_market_service(command_tx, shutdown, active_provider_workers, workers);
        #[cfg(test)]
        service.install_provider_instrument(&test_coinbase_instrument())?;
        #[cfg(test)]
        service.install_provider_instrument(&test_coinbase_eth_instrument())?;
        Ok(service)
    }

    fn build_market_service(
        commands: SyncSender<Command>,
        shutdown: Arc<AtomicBool>,
        active_provider_workers: Arc<Mutex<BTreeSet<String>>>,
        workers: Vec<thread::JoinHandle<()>>,
    ) -> Self {
        Self {
            commands,
            runtime: Arc::new(MarketRuntime {
                shutdown,
                active_provider_workers,
                workers: Mutex::new(Some(workers)),
            }),
        }
    }

    /// Cancels provider work, drains accepted persistence, and joins owned workers.
    ///
    /// # Errors
    /// Returns an error when a worker panics or the complete shutdown exceeds `timeout`.
    pub fn shutdown(&self, timeout: Duration) -> Result<(), String> {
        self.runtime.shutdown.store(true, Ordering::Release);
        let mut workers = self
            .runtime
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| "market engine shutdown is already in progress".to_string())?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| "market engine shutdown deadline overflowed".to_string())?;
        let mut panicked = Vec::new();
        loop {
            let mut index = 0;
            while index < workers.len() {
                if workers[index].is_finished() {
                    let worker = workers.swap_remove(index);
                    let name = worker.thread().name().unwrap_or("unnamed").to_string();
                    if worker.join().is_err() {
                        panicked.push(name);
                    }
                } else {
                    index += 1;
                }
            }
            if workers.is_empty() {
                return if panicked.is_empty() {
                    Ok(())
                } else {
                    Err(format!(
                        "market engine workers panicked during shutdown: {}",
                        panicked.join(", ")
                    ))
                };
            }
            let now = Instant::now();
            if now >= deadline {
                let mut pending = workers
                    .iter()
                    .map(|worker| worker.thread().name().unwrap_or("unnamed"))
                    .map(str::to_string)
                    .collect::<BTreeSet<_>>();
                pending.extend(
                    self.runtime
                        .active_provider_workers
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .iter()
                        .cloned(),
                );
                let pending = pending.into_iter().collect::<Vec<_>>().join(", ");
                return Err(format!(
                    "market engine shutdown deadline expired with active workers: {pending}"
                ));
            }
            thread::sleep(Duration::from_millis(5).min(deadline.duration_since(now)));
        }
    }

    /// Attaches a client identity to resident market state.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn attach(&self, client_id: u64) -> Result<(), String> {
        self.request(|reply| Ok(Command::Attach(id(client_id).map(ClientId)?, None, reply)))
    }

    pub(crate) fn attach_stream(
        &self,
        client_id: u64,
        events: SyncSender<(u64, envelope::Payload)>,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Attach(
                id(client_id).map(ClientId)?,
                Some(events),
                reply,
            ))
        })
    }

    /// Detaches a client and all of its consumers.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn detach(&self, client_id: u64) -> Result<(), String> {
        self.request(|reply| Ok(Command::Detach(id(client_id).map(ClientId)?, reply)))
    }

    /// Applies the engine-owned background market retention policy.
    ///
    /// # Errors
    /// Returns an error when the coordinator is unavailable.
    pub fn set_resource_mode(&self, mode: ResourceMode) -> Result<(), String> {
        self.request(|reply| Ok(Command::SetResourceMode(mode, reply)))
    }

    fn restore_hot_set(&self, hot_series: &[HotSeries]) -> Result<(), String> {
        let restored = hot_series
            .iter()
            .filter_map(|series| {
                if let Ok(series) = warm_series(series) {
                    Some(series)
                } else {
                    eprintln!("Axiusflow engine skipped unsupported hot-set metadata");
                    None
                }
            })
            .collect::<Vec<_>>();
        self.request(|reply| Ok(Command::RestoreHotSet(restored, reply)))
    }

    /// Returns one bounded coordinator-owned lifecycle and memory snapshot.
    ///
    /// # Errors
    /// Returns an error when the coordinator is unavailable.
    pub fn status(&self) -> Result<MarketServiceStatus, String> {
        self.request(|reply| Ok(Command::Status(reply)))
    }

    /// Registers one market consumer owned by an attached client.
    ///
    /// # Errors
    /// Returns an error for invalid identity, ownership, bounds, or coordinator failure.
    pub fn register_consumer(
        &self,
        client_id: u64,
        workspace_id: u64,
        consumer_id: u64,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Register(
                ConsumerIdentity {
                    client_id: ClientId(id(client_id)?),
                    workspace_id: WorkspaceId(id(workspace_id)?),
                    consumer_id: ConsumerId(id(consumer_id)?),
                },
                reply,
            ))
        })
    }

    /// Removes one market consumer.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn remove_consumer(&self, client_id: u64, consumer_id: u64) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Remove(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                reply,
            ))
        })
    }

    /// Applies a generation-fenced viewport to one consumer.
    ///
    /// # Errors
    /// Returns an error for invalid identity, range, generation, or coordinator failure.
    pub fn set_viewport(
        &self,
        client_id: u64,
        consumer_id: u64,
        generation: u64,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Viewport(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                GenerationId(id(generation)?),
                Viewport::try_new(start_unix_nanos, end_unix_nanos)
                    .map_err(|error| error.to_string())?,
                reply,
            ))
        })
    }

    /// Updates one consumer's presentation priority.
    ///
    /// # Errors
    /// Returns an error for invalid identity, missing consumer, or coordinator failure.
    pub fn set_visibility(
        &self,
        client_id: u64,
        consumer_id: u64,
        visible: bool,
    ) -> Result<(), String> {
        self.set_resource_class(
            client_id,
            consumer_id,
            if visible {
                ConsumerResourceClass::Foreground
            } else {
                ConsumerResourceClass::Background
            },
        )
    }

    /// Updates one consumer's exact resource class.
    ///
    /// # Errors
    /// Returns an error for invalid identity, missing consumer, or coordinator failure.
    pub fn set_resource_class(
        &self,
        client_id: u64,
        consumer_id: u64,
        resource_class: ConsumerResourceClass,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::ResourceClass(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                resource_class,
                reply,
            ))
        })
    }

    /// Accepts one generation-fenced series demand without waiting for provider I/O.
    ///
    /// # Errors
    /// Returns an error for invalid demand or an unavailable coordinator.
    pub fn set_demand(
        &self,
        client_id: u64,
        consumer_id: u64,
        generation: u64,
        series: &SeriesKey,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Demand(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                GenerationId(id(generation)?),
                internal_series(series)?,
                reply,
            ))
        })
    }

    /// Schedules one bounded exact provider-instrument search for an owned consumer.
    ///
    /// # Errors
    /// Returns an error for invalid identity, unsupported input, ownership, or coordinator failure.
    pub fn search_provider_instruments(
        &self,
        client_id: u64,
        search: SearchProviderInstruments,
    ) -> Result<(), String> {
        validate_provider_search(&search)?;
        self.request(|reply| {
            Ok(Command::SearchProviderInstruments(
                ClientId(id(client_id)?),
                search,
                reply,
            ))
        })
    }

    /// Schedules one exact provider-instrument selection for an owned consumer.
    ///
    /// # Errors
    /// Returns an error for invalid identity, unsupported input, ownership, or coordinator failure.
    pub fn select_provider_instrument(
        &self,
        client_id: u64,
        selection: SelectProviderInstrument,
    ) -> Result<(), String> {
        validate_provider_selection(&selection)?;
        self.request(|reply| {
            Ok(Command::SelectProviderInstrument(
                ClientId(id(client_id)?),
                selection,
                reply,
            ))
        })
    }

    /// Installs one bounded adapter-resolved instrument in the engine-owned catalog.
    ///
    /// # Errors
    /// Returns an error for invalid identity, stale generations, capacity, or coordinator failure.
    pub fn install_provider_instrument(
        &self,
        instrument: &InstallProviderInstrument,
    ) -> Result<(), String> {
        validate_provider_instrument(instrument)?;
        self.request(|reply| {
            Ok(Command::InstallProviderInstrument(
                instrument.clone(),
                reply,
            ))
        })
    }

    /// Drains at most one bounded market publication for an owned consumer.
    ///
    /// # Errors
    /// Returns an error for invalid ownership or coordinator failure.
    pub fn poll_event(
        &self,
        client_id: u64,
        consumer_id: u64,
    ) -> Result<Option<envelope::Payload>, String> {
        self.request(|reply| {
            Ok(Command::Poll(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                reply,
            ))
        })
    }

    fn request<T>(
        &self,
        build: impl FnOnce(Reply<T>) -> Result<Command, String>,
    ) -> Result<T, String> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let command = build(reply_tx)?;
        self.commands
            .send(command)
            .map_err(|_| "market engine coordinator is unavailable".to_string())?;
        reply_rx
            .recv()
            .map_err(|_| "market engine coordinator stopped before replying".to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::num::NonZeroU64;
    use std::sync::mpsc;

    const LEVEL2_SNAPSHOT: &[u8] = br#"{"channel":"l2_data","timestamp":"2023-11-14T22:13:20Z","sequence_num":0,"events":[{"type":"snapshot","product_id":"BTC-USD","updates":[{"side":"bid","event_time":"2023-11-14T22:13:20Z","price_level":"100.00","new_quantity":"2.00000000"},{"side":"offer","event_time":"2023-11-14T22:13:20Z","price_level":"101.00","new_quantity":"3.00000000"}]}]}"#;

    fn product(symbol: &str) -> RealtimeProduct {
        RealtimeProduct {
            symbol: symbol.to_string(),
            price_scale: 2,
            quantity_scale: 8,
        }
    }

    #[test]
    fn subscription_updates_preserve_ready_books_for_retained_products() {
        let mut source = LiveCoinbaseRealtime::try_new();
        let generation = ProviderGeneration(NonZeroU64::new(2).expect("generation"));
        let books = RefCell::new(vec![
            CoinbaseLevel2Book::try_new("BTC-USD", 2, 8, 2).expect("book"),
        ]);
        books.borrow_mut()[0]
            .apply_message(LEVEL2_SNAPSHOT, 1)
            .expect("snapshot installs");
        assert_eq!(books.borrow()[0].diagnostics().snapshots, 1);
        let (controls_tx, controls_rx) = mpsc::sync_channel(4);
        controls_tx
            .send(RealtimeControl::Start(vec![product("BTC-USD")]))
            .expect("control");
        assert_eq!(
            source
                .take_product_update(&controls_rx, generation, &books)
                .expect("identical update applies"),
            vec!["BTC-USD".to_string()]
        );
        assert_eq!(books.borrow().len(), 1);
        assert_eq!(books.borrow()[0].product_id(), "BTC-USD");
        assert_eq!(
            books.borrow()[0].diagnostics().snapshots,
            1,
            "a retained ready book survives an identical update"
        );
        controls_tx
            .send(RealtimeControl::Start(vec![product("ETH-USD")]))
            .expect("control");
        assert_eq!(
            source
                .take_product_update(&controls_rx, generation, &books)
                .expect("changed update applies"),
            vec!["ETH-USD".to_string()]
        );
        assert_eq!(books.borrow().len(), 1);
        assert_eq!(books.borrow()[0].product_id(), "ETH-USD");
        assert_eq!(
            books.borrow()[0].diagnostics().snapshots,
            0,
            "an added product starts awaiting its own snapshot"
        );
    }
}
