use super::*;
use axiusflow_coinbase_market_adapter::FixedPointValue;
use axiusflow_market_data::{AggressorSide, EventMetadata, QualifiedTimestamp};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::Instant;

struct FixtureHistory {
    bars: Vec<MarketBar>,
    fetches: Option<Arc<AtomicUsize>>,
}

pub(crate) enum FixtureRealtimeAction {
    Connected,
    Trade(CanonicalTrade),
    Heartbeat,
    Disconnect,
}

struct FixtureRealtime {
    actions: Receiver<FixtureRealtimeAction>,
    generations: SyncSender<ProviderGeneration>,
    stops: SyncSender<ProviderGeneration>,
    configured_products: Option<SyncSender<Vec<String>>>,
}

pub(crate) struct FixtureRealtimeHarness {
    pub(crate) service: MarketService,
    pub(crate) actions: SyncSender<FixtureRealtimeAction>,
    pub(crate) generations: Receiver<ProviderGeneration>,
    pub(crate) stops: Receiver<ProviderGeneration>,
    pub(crate) configured_products: Receiver<Vec<String>>,
    pub(crate) history_fetches: Arc<AtomicUsize>,
}

impl HistorySource for FixtureHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        if request.kind != HistoryRequestKind::Initial {
            return Err("fixture history does not provide viewport backfill".to_string());
        }
        if let Some(fetches) = &self.fetches {
            fetches.fetch_add(1, Ordering::AcqRel);
        }
        let (interval_seconds, price_scale, quantity_scale) =
            if request.series.provider_id == "coinbase" {
                let instrument = request
                    .instrument
                    .as_ref()
                    .ok_or_else(|| "fixture Coinbase instrument is unavailable".to_string())?;
                let profile = coinbase_series_profile(&request.series, instrument)?;
                (
                    u32::try_from(profile.interval.fixed_seconds().unwrap_or(86_400))
                        .map_err(|_| "fixture interval is invalid".to_string())?,
                    profile.price_scale,
                    profile.quantity_scale,
                )
            } else {
                let instrument = request
                    .instrument
                    .as_ref()
                    .ok_or_else(|| "fixture provider instrument is unavailable".to_string())?;
                let interval_seconds = request
                    .series
                    .period
                    .duration_nanos()
                    .and_then(|nanos| u32::try_from(nanos / 1_000_000_000).ok())
                    .unwrap_or(1);
                (
                    interval_seconds,
                    u8::try_from(instrument.price_scale)
                        .map_err(|_| "fixture price scale is invalid".to_string())?,
                    u8::try_from(instrument.quantity_scale)
                        .map_err(|_| "fixture quantity scale is invalid".to_string())?,
                )
            };
        let mut bars = self.bars.clone();
        for (index, bar) in bars.iter_mut().enumerate() {
            bar.exchange_timestamp_seconds = i64::try_from(index + 1)
                .ok()
                .and_then(|value| value.checked_mul(i64::from(interval_seconds)))
                .ok_or_else(|| "fixture history timestamp overflow".to_string())?;
            bar.exchange_timestamp_unix_nanos = bar
                .exchange_timestamp_seconds
                .checked_mul(1_000_000_000)
                .ok_or_else(|| "fixture history timestamp overflow".to_string())?;
        }
        Ok(HistorySnapshot {
            price_scale,
            quantity_scale,
            bars,
            forming: None,
            handoff_boundary_unix_nanos: None,
            confirmed_empty: false,
        })
    }
}

impl RealtimeSource for FixtureRealtime {
    fn configure(&mut self, products: Vec<RealtimeProduct>) -> Result<(), String> {
        if let Some(configured_products) = &self.configured_products {
            configured_products
                .send(
                    products
                        .iter()
                        .map(|product| product.symbol.clone())
                        .collect(),
                )
                .map_err(|_| "configured-product observer disconnected".to_string())?;
        }
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
        if self.generations.send(generation).is_err() {
            return false;
        }
        let mut connected = false;
        while !stop.load(Ordering::Acquire) {
            while let Ok(RealtimeControl::Start(products)) = controls.try_recv() {
                let _ = self.configure(products);
            }
            let action = match self.actions.recv_timeout(Duration::from_millis(10)) {
                Ok(action) => action,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return connected,
            };
            let event = match action {
                FixtureRealtimeAction::Connected => {
                    if events.send(RealtimeEvent::Connected(generation)).is_err() {
                        return false;
                    }
                    connected = true;
                    continue;
                }
                FixtureRealtimeAction::Trade(trade) => RealtimeEvent::Trade(generation, trade),
                FixtureRealtimeAction::Heartbeat => RealtimeEvent::Heartbeat(generation),
                FixtureRealtimeAction::Disconnect => return connected,
            };
            if !try_emit_realtime(events, overflow, event) {
                return connected;
            }
        }
        let _ = self.stops.send(generation);
        connected
    }
}

impl MarketService {
    /// Starts a deterministic in-memory history source for IPC integration tests.
    pub(crate) fn start_fixture(bars: Vec<MarketBar>) -> Result<Self, String> {
        let service = Self::start_composed(
            vec![
                ProviderRuntimeSpec::coinbase(
                    Box::new(FixtureHistory {
                        bars: bars.clone(),
                        fetches: None,
                    }),
                    None,
                ),
                ProviderRuntimeSpec::rithmic(
                    Box::new(FixtureHistory {
                        bars,
                        fetches: None,
                    }),
                    false,
                ),
            ],
            None,
            0,
        )?;
        let series = internal_series(&btc())?;
        service.install_provider_instrument(&coinbase_instrument(&series))?;
        Ok(service)
    }

    pub(crate) fn start_fixture_realtime(
        bars: Vec<MarketBar>,
    ) -> Result<FixtureRealtimeHarness, String> {
        Self::start_fixture_realtime_with_storage(bars, None)
    }

    fn start_fixture_realtime_with_storage(
        bars: Vec<MarketBar>,
        storage: Option<Result<LocalHistoryStore, String>>,
    ) -> Result<FixtureRealtimeHarness, String> {
        let (action_tx, action_rx) = mpsc::sync_channel(16);
        let (generation_tx, generation_rx) = mpsc::sync_channel(4);
        let (stop_tx, stop_rx) = mpsc::sync_channel(4);
        let (configured_products_tx, configured_products_rx) = mpsc::sync_channel(16);
        let history_fetches = Arc::new(AtomicUsize::new(0));
        let service = Self::start_with_sources(
            FixtureHistory {
                bars,
                fetches: Some(Arc::clone(&history_fetches)),
            },
            Some(Box::new(FixtureRealtime {
                actions: action_rx,
                generations: generation_tx,
                stops: stop_tx,
                configured_products: Some(configured_products_tx),
            })),
            storage,
        )?;
        Ok(FixtureRealtimeHarness {
            service,
            actions: action_tx,
            generations: generation_rx,
            stops: stop_rx,
            configured_products: configured_products_rx,
            history_fetches,
        })
    }

    fn start_with_source(source: impl HistorySource) -> Result<Self, String> {
        Self::start_with_sources(source, None, None)
    }

    fn start_with_sources(
        source: impl HistorySource,
        realtime: Option<Box<dyn RealtimeSource>>,
        storage: Option<Result<LocalHistoryStore, String>>,
    ) -> Result<Self, String> {
        let service = Self::start_composed(
            vec![ProviderRuntimeSpec::coinbase(Box::new(source), realtime)],
            storage,
            0,
        )?;
        let series = internal_series(&btc())?;
        service.install_provider_instrument(&coinbase_instrument(&series))?;
        Ok(service)
    }
}

struct ControlledHistory {
    fetches: Arc<AtomicUsize>,
    release: Receiver<()>,
}

struct SwitchingHistory {
    requested: SyncSender<BarSeriesKey>,
    release: Receiver<()>,
}

struct DelayedCancellationHistory {
    started: SyncSender<(BarSeriesKey, ProviderGeneration)>,
    cancellation_observed: SyncSender<()>,
    release_cancellation: Receiver<()>,
    block_first: bool,
}

struct ControlledHistoryFailure {
    started: SyncSender<()>,
    release: Receiver<()>,
}

struct BlockingRithmicHistory {
    started: SyncSender<()>,
    cancelled: SyncSender<()>,
}

struct UncancellableHistory {
    started: SyncSender<()>,
    release: Receiver<()>,
    exited: SyncSender<()>,
}

fn retained_history_coordinator<'a>(
    engine: MarketEngine,
    history: &'a SyncSender<HistoryRequest>,
    storage: &'a SyncSender<StorageRequest>,
    realtime: &'a SyncSender<RealtimeControl>,
    realtime_stop: &'a Arc<AtomicBool>,
    consumer_id: ConsumerId,
    series: &BarSeriesKey,
) -> Coordinator<'a> {
    Coordinator {
        engine,
        providers: ProviderDispatch::fixture(history, realtime, realtime_stop),
        storage,
        resource_mode: ResourceMode::Warm,
        resource_policy: decide_resource_policy(ResourcePolicyInput {
            mode: EngineResourceMode::Warm,
            available_memory_bytes: u64::MAX,
            consumer_count: 1,
            visible_consumer_count: 1,
            provider_series_limit: MAXIMUM_SERIES,
            hot_set_priority_count: 1,
        }),
        available_memory_bytes: u64::MAX,
        hot_set_priority_count: 1,
        last_consumer_activity: Instant::now(),
        attached: BTreeSet::new(),
        attached_sinks: BTreeMap::new(),
        consumer_clients: BTreeMap::new(),
        pending: BTreeMap::from([(
            series.clone(),
            vec![DemandWaiter {
                consumer_id,
                generation: GenerationId(id(1).expect("generation")),
                started_at: Instant::now(),
            }],
        )]),
        history_inflight: BTreeMap::new(),
        pending_live_edge_repairs: BTreeMap::new(),
        history_cancellations: BTreeMap::new(),
        suspended_history: BTreeSet::new(),
        history_coverage: BTreeMap::new(),
        viewport_history_ranges: BTreeMap::new(),
        viewport_history_retries: BTreeMap::new(),
        active_viewports: BTreeMap::new(),
        viewport_history_local_inflight: BTreeSet::new(),
        deferred_publications: BTreeSet::new(),
        pending_empty_repairs: BTreeMap::new(),
        empty_repair_retry_at: Instant::now(),
        history_retries: BTreeMap::new(),
        local_history_deadlines: BTreeMap::new(),
        local_loaded: BTreeSet::new(),
        warming: BTreeSet::new(),
        warm_series: BTreeMap::new(),
        warm_priority: Vec::new(),
        retained_history: BTreeMap::new(),
        prewarmed: BTreeSet::new(),
        retained_live: BTreeSet::new(),
        warm_rithmic_search_generation: 0,
        events: BTreeMap::from([(consumer_id, ConsumerEvents::default())]),
        live: BTreeMap::new(),
        rithmic_live: BTreeMap::new(),
        order_books: BTreeMap::new(),
        catalog: if series.provider_id == "coinbase" {
            BTreeMap::from([(
                ("coinbase".to_string(), series.instrument_id.clone()),
                coinbase_instrument(series),
            )])
        } else {
            BTreeMap::new()
        },
        catalog_sessions: BTreeMap::new(),
        catalog_selections: BTreeMap::new(),
        realtime_connected: false,
        rithmic_selection: None,
        rithmic_pending_selection: None,
        realtime_products: BTreeSet::new(),
    }
}

fn hot_coinbase() -> HotSeries {
    HotSeries {
        provider: "coinbase".to_string(),
        market: "BTC-USD".to_string(),
        interval_seconds: 60,
        score: 1,
        last_used_unix_seconds: 1,
        provider_watermark: 1,
        series_watermark: 1,
        viewport_start_unix_nanos: Some(1),
        viewport_end_unix_nanos: Some(2),
        account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
        instrument_id: "instrument:coinbase:btc:usd".to_string(),
        entitlement_id: ENTITLEMENT_CLASS.to_string(),
        cadence: SeriesCadence::FixedSeconds as i32,
        cadence_value: 60,
        definition_revision: 1,
        pinned: false,
        workspace_ids: vec![1],
        coverage_start_unix_nanos: Some(60_000_000_000),
        coverage_end_unix_nanos: Some(60_000_000_001),
        provider_symbol: "BTC-USD".to_string(),
        venue_id: "coinbase".to_string(),
        display_symbol: "BTC/USD".to_string(),
        price_scale: 2,
        quantity_scale: 8,
    }
}

fn hot_coinbase_series(
    market: &str,
    cadence_value: u32,
    workspace_id: u64,
    score: u32,
) -> HotSeries {
    let mut hot = hot_coinbase();
    let (base, display) = if market == "ETH-USD" {
        ("eth", "ETH/USD")
    } else {
        ("btc", "BTC/USD")
    };
    hot.market = market.to_string();
    hot.instrument_id = format!("instrument:coinbase:{base}:usd");
    hot.provider_symbol = market.to_string();
    hot.display_symbol = display.to_string();
    hot.interval_seconds = cadence_value;
    hot.cadence_value = cadence_value;
    hot.workspace_ids = vec![workspace_id];
    hot.score = score;
    hot.last_used_unix_seconds = u64::from(score);
    hot
}

#[test]
fn persisted_hot_set_tiers_bound_real_startup_priority_by_workspace_pin_watchlist_and_memory() {
    let mut active = hot_coinbase_series("BTC-USD", 60, 7, 1);
    active.last_used_unix_seconds = 1;
    let mut pinned = hot_coinbase_series("BTC-USD", 300, 8, 2);
    pinned.pinned = true;
    let watchlist = hot_coinbase_series("ETH-USD", 60, 9, 3);
    let recent = hot_coinbase_series("BTC-USD", 900, 10, 4);
    let workspace = WorkspaceState {
        watchlist: vec!["ETH-USD".to_string()],
        active_workspace_id: 7,
        hot_series: vec![active, pinned, watchlist, recent],
        ..crate::default_workspace()
    };
    let estimated_entry_bytes = (HISTORY_BARS_PER_SERIES + 1)
        .saturating_mul(std::mem::size_of::<MarketBar>())
        .saturating_add(1_024);
    let retained = retained_hot_series(
        &workspace,
        u64::try_from(estimated_entry_bytes.saturating_mul(2).saturating_mul(16))
            .expect("memory budget fits"),
    )
    .expect("hot set classifies");
    assert_eq!(retained.len(), 2);
    assert_eq!(retained[0].workspace_ids, vec![7]);
    assert_eq!(retained[1].cadence_value, 300);
    assert!(retained[1].pinned);
}

#[test]
fn coordinator_eviction_retains_only_the_current_bounded_warm_priority() {
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let mut engine = configured_engine().expect("engine configures");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    let series = [60, 300, 900]
        .into_iter()
        .map(|cadence_value| {
            internal_series(&SeriesKey {
                cadence_value,
                ..btc()
            })
            .expect("series validates")
        })
        .collect::<Vec<_>>();
    for item in &series {
        engine
            .install_history(
                ProviderGeneration(id(1).expect("provider generation")),
                item,
                2,
                8,
                vec![history_bar()],
            )
            .expect("cached history installs");
    }
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series[0],
    );
    coordinator.hot_set_priority_count = 3;
    coordinator.warm_priority = series.clone();
    coordinator
        .apply_resource_mode(ResourceMode::Constrained)
        .expect("constrained mode applies");

    assert!(coordinator.engine.series_snapshot(&series[0]).is_some());
    assert!(coordinator.engine.series_snapshot(&series[1]).is_some());
    assert!(coordinator.engine.series_snapshot(&series[2]).is_none());
    assert_eq!(coordinator.engine.metrics().stored_series, 2);
}

#[test]
fn warm_restore_prefetches_local_history_and_installs_it_before_reattach() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let series = internal_series(&btc()).expect("series");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let mut engine = configured_engine().expect("engine configures");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand(
            consumer_id,
            GenerationId(id(1).expect("generation")),
            &series,
        )
        .expect("demand installs");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    coordinator.restore_hot_series(vec![warm_series(&hot_coinbase()).expect("hot series")]);
    let (requested, generation) = match storage_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("local warm read is requested")
    {
        StorageRequest::Read(series, generation) => (series, generation),
        StorageRequest::ReadRange(..) => panic!("warm restore uses exact history reads"),
        StorageRequest::Persist(..)
        | StorageRequest::RecordConfirmedEmpty(..)
        | StorageRequest::ResolveConfirmedEmpty(..) => {
            panic!("warm restore reads before persisting")
        }
    };
    assert_eq!(requested, series);
    coordinator.local_history_completed(
        &requested,
        generation,
        Ok(Some(StoredHistory {
            bars: vec![history_bar()],
            derived: false,
            durable: true,
        })),
    );
    assert!(coordinator.engine.series_snapshot(&series).is_some());
    assert!(coordinator.prewarmed.contains(&series));

    let retained = coordinator
        .engine
        .series_snapshot(&series)
        .expect("retained history remains installed");
    assert!(
        coordinator
            .prepare_cached_demand(&series, generation, &retained)
            .expect("warm demand starts current repair")
    );
    assert!(
        coordinator.engine.series_snapshot(&series).is_some(),
        "withholding stale publication must not delete the merge baseline"
    );
    assert!(matches!(
        history_rx.try_recv(),
        Ok(HistoryRequest {
            ref series,
            provider_generation,
            ..
        }) if series == &requested && provider_generation == generation
    ));
}

#[test]
fn constrained_policy_sets_the_real_provider_history_bound() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let series = internal_series(&btc()).expect("series");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let mut engine = configured_engine().expect("engine configures");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    coordinator
        .apply_resource_mode(ResourceMode::Constrained)
        .expect("constrained mode applies");
    coordinator
        .enqueue_history(
            &series,
            ProviderGeneration(id(1).expect("provider generation")),
        )
        .expect("bounded history enqueues");
    assert!(matches!(
        history_rx.recv_timeout(Duration::from_secs(1)),
        Ok(HistoryRequest {
            maximum_bars: 160,
            ..
        })
    ));
}

fn provider_instrument(
    session_generation: u64,
    selection_generation: u64,
) -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: "rithmic".to_string(),
        session_generation,
        selection_generation,
        instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
        provider_symbol: "MNQU6".to_string(),
        display_symbol: "MNQU6".to_string(),
        venue_id: "CME".to_string(),
        price_scale: 2,
        quantity_scale: 0,
        entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
    }
}

fn rithmic_series_key(instrument_id: &str, entitlement_id: &str) -> BarSeriesKey {
    BarSeriesKey {
        provider_id: "rithmic".to_string(),
        instrument_id: instrument_id.to_string(),
        entitlement_id: entitlement_id.to_string(),
        period: BarPeriod::time(60).expect("minute cadence"),
        definition_version: 1,
    }
}

fn install_rithmic_test_series(
    engine: &mut MarketEngine,
    raw_consumer: u64,
    generation: ProviderGeneration,
    series: &BarSeriesKey,
) -> axiusflow_market_engine::ConsumerPublication {
    let consumer_id = ConsumerId(id(raw_consumer).expect("consumer"));
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand_with_streams(
            consumer_id,
            GenerationId(id(1).expect("generation")),
            series,
            chart_stream_requirements(series),
        )
        .expect("demand installs");
    engine
        .install_history(generation, series, 2, 0, vec![history_bar()])
        .expect("history installs")
        .pop()
        .expect("matching consumer receives history")
}

fn seeded_rithmic_live(
    series: &BarSeriesKey,
    generation: ProviderGeneration,
) -> RithmicLiveHandoff {
    let mut live = RithmicLiveHandoff::new(series, generation, "CME").expect("live cadence");
    live.seed(2, 0, &[history_bar()], None, None)
        .expect("history seeds handoff");
    live.connected = true;
    live
}

fn complete_rithmic_test_history(
    coordinator: &mut Coordinator<'_>,
    series: &BarSeriesKey,
    generation: ProviderGeneration,
) {
    coordinator.history_completed(
        series,
        generation,
        None,
        HistoryRequestKind::Initial,
        Ok(HistorySnapshot {
            price_scale: 2,
            quantity_scale: 0,
            bars: vec![history_bar()],
            forming: None,
            handoff_boundary_unix_nanos: None,
            confirmed_empty: false,
        }),
    );
}

fn rithmic_trade(sequence: u64, session: u64, nanos: i64, price: i64) -> MarketTrade {
    MarketTrade {
        metadata: EventMetadata {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            source_sequence: sequence,
            session_generation: session,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(nanos),
                provider_unix_nanos: Some(nanos + 1),
                received_unix_nanos: nanos + 2,
            },
        },
        trade_id: format!("trade-{sequence}"),
        price,
        quantity: 2,
        aggressor: AggressorSide::Buy,
    }
}

fn rithmic_depth_snapshot(
    series: &BarSeriesKey,
    source_sequence: u64,
    bid_quantity: i64,
) -> DepthSnapshot {
    let timestamp = i64::try_from(source_sequence).unwrap_or(i64::MAX);
    DepthSnapshot {
        metadata: EventMetadata {
            provider_id: "rithmic".to_string(),
            instrument_id: series.instrument_id.clone(),
            entitlement_id: series.entitlement_id.clone(),
            source_sequence,
            session_generation: 7,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(timestamp),
                provider_unix_nanos: None,
                received_unix_nanos: timestamp,
            },
        },
        bids: vec![DepthLevel {
            price: 20_000,
            quantity: bid_quantity,
            order_count: Some(3),
        }],
        asks: vec![DepthLevel {
            price: 20_025,
            quantity: 4,
            order_count: Some(2),
        }],
    }
}

fn pop_order_book(
    coordinator: &mut Coordinator<'_>,
    consumer_id: ConsumerId,
) -> IpcOrderBookSnapshot {
    let envelope::Payload::OrderBookSnapshot(snapshot) = coordinator
        .events
        .get_mut(&consumer_id)
        .and_then(ConsumerEvents::pop)
        .expect("order book publishes")
    else {
        panic!("expected order-book publication");
    };
    snapshot
}

#[test]
fn pending_covering_snapshot_is_never_replaced_by_an_out_of_order_tail() {
    let series = btc();
    let covering_bar = history_bar();
    let mut events = ConsumerEvents::default();
    events.publish_snapshot(envelope::Payload::SeriesSnapshot(IpcSeriesSnapshot {
        consumer_id: 1,
        generation: 1,
        series: Some(series.clone()),
        provider_generation: 1,
        price_scale: 2,
        quantity_scale: 8,
        bars: vec![ipc_bar(covering_bar)],
        publication_generation: 4,
        forming: false,
    }));
    let stale = MarketBar {
        source_sequence: covering_bar.source_sequence.saturating_sub(1),
        ..covering_bar
    };
    events.publish_series_update(envelope::Payload::SeriesUpdate(
        axiusflow_engine_protocol::SeriesUpdate {
            consumer_id: 1,
            generation: 1,
            series: Some(series),
            provider_generation: 1,
            bar: Some(ipc_bar(stale)),
            forming: true,
            publication_generation: 5,
            operation: SeriesUpdateOperation::ReviseTail.into(),
        },
    ));

    assert!(matches!(
        events.queued_series(),
        Some(envelope::Payload::SeriesSnapshot(snapshot))
            if snapshot.bars == vec![ipc_bar(covering_bar)]
                && snapshot.publication_generation == 4
    ));
    // The tail is queued behind the snapshot rather than folded into it or
    // dropped. Dropping it is what used to open sequence gaps the consumer
    // could only repair with a full resnapshot.
    assert_eq!(events.series.len(), 2);
}

#[test]
fn rithmic_live_handoff_continues_fixed_and_tick_history_without_desktop_aggregation() {
    let generation = ProviderGeneration(id(7).expect("provider generation"));
    let fixed_series = BarSeriesKey {
        provider_id: "rithmic".to_string(),
        instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
        entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
        period: BarPeriod::time(60).expect("minute"),
        definition_version: 1,
    };
    let history = MarketBar {
        source_sequence: 40,
        exchange_timestamp_seconds: 60,
        exchange_timestamp_unix_nanos: 60_000_000_000,
        open: 100,
        high: 110,
        low: 90,
        close: 105,
        volume: 7,
    };
    let mut fixed =
        RithmicLiveHandoff::new(&fixed_series, generation, "CME").expect("fixed cadence streams");
    fixed
        .seed(2, 0, &[history], None, None)
        .expect("history seeds");
    fixed
        .accept_trade(&rithmic_trade(1, 1, 70_000_000_000, 108))
        .expect("trade inside completed history is stale");
    assert_eq!(fixed.bars.len(), 1);
    assert_eq!(fixed.bars[0].close, 105);
    assert_eq!(fixed.bars[0].exchange_timestamp_unix_nanos, 60_000_000_000);
    fixed
        .accept_trade(&rithmic_trade(2, 1, 121_000_000_000, 115))
        .expect("next minute starts");
    assert_eq!(fixed.bars.len(), 2);
    assert_eq!(fixed.bars[1].source_sequence, 41);
    assert_eq!(fixed.bars[1].exchange_timestamp_seconds, 120);
    fixed
        .accept_trade(&rithmic_trade(3, 1, 125_000_000_000, 116))
        .expect("forming minute updates");
    assert_eq!(fixed.bars[1].close, 116);
    fixed.connected = true;
    assert!(matches!(
        fixed.take_publication(),
        Some(LiveSeriesPublication::Tails(ref bars))
            if bars.len() == 1 && bars[0].source_sequence == 41 && bars[0].close == 116
    ));
    fixed
        .accept_trade(&rithmic_trade(4, 1, 126_000_000_000, 117))
        .expect("forming minute receives its final revision");
    fixed
        .accept_trade(&rithmic_trade(5, 1, 181_000_000_000, 118))
        .expect("following minute starts before the next publication");
    assert!(matches!(
        fixed.take_publication(),
        Some(LiveSeriesPublication::Covering(bars))
            if bars.len() == 3
                && bars[1].source_sequence == 41
                && bars[1].close == 117
                && bars[2].source_sequence == 42
                && bars[2].close == 118
    ));
    let mut engine = configured_engine().expect("engine configures");
    engine
        .begin_provider_session("rithmic", generation)
        .expect("Rithmic generation begins");
    engine
        .install_history(generation, &fixed_series, 2, 0, vec![history])
        .expect("completed history installs");
    engine
        .install_realtime(generation, &fixed_series, 2, 0, fixed.bars.clone(), true)
        .expect("engine accepts the live forming suffix");

    let tick_series = BarSeriesKey {
        period: BarPeriod::tick(100).expect("tick"),
        ..fixed_series
    };
    let tick_history = MarketBar {
        exchange_timestamp_unix_nanos: 60_123_456_789,
        ..history
    };
    let mut tick =
        RithmicLiveHandoff::new(&tick_series, generation, "CME").expect("tick cadence streams");
    tick.seed(2, 0, &[tick_history], None, None)
        .expect("tick history seeds");
    tick.accept_trade(&rithmic_trade(1, 1, 60_500_000_000, 120))
        .expect("first trade starts a new tick bar");
    tick.accept_trade(&rithmic_trade(2, 1, 60_600_000_000, 121))
        .expect("second trade updates tick bar");
    assert_eq!(tick.bars.len(), 2);
    assert_eq!(tick.bars[1].source_sequence, 41);
    assert_eq!(tick.bars[1].close, 121);
    assert_eq!(tick.bars[1].volume, 4);
    assert_eq!(tick.bars[1].exchange_timestamp_unix_nanos, 60_500_000_000);
    assert!(
        tick.accept_trade(&rithmic_trade(4, 2, 60_700_000_000, 122))
            .is_err()
    );
}

#[test]
fn rithmic_live_handoff_uses_the_exchange_calendar_for_weeks_and_months() {
    let generation = ProviderGeneration(id(7).expect("provider generation"));
    let cases = [
        (
            BarPeriod::week(1).expect("week"),
            1_787_529_600_i64,
            1_788_048_000_i64,
            1_788_125_400_000_000_000_i64,
            1_788_129_000_000_000_000_i64,
        ),
        (
            BarPeriod::month(1).expect("month"),
            1_785_542_400_i64,
            1_788_134_400_i64,
            1_788_211_800_000_000_000_i64,
            1_788_215_400_000_000_000_i64,
        ),
    ];
    for (period, marker_seconds, boundary_seconds, same_bucket, next_bucket) in cases {
        let series = BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            period,
            definition_version: 1,
        };
        let history = MarketBar {
            source_sequence: 40,
            exchange_timestamp_seconds: marker_seconds,
            exchange_timestamp_unix_nanos: marker_seconds * 1_000_000_000,
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 7,
        };
        let mut live =
            RithmicLiveHandoff::new(&series, generation, "CME").expect("calendar cadence streams");
        live.seed(
            2,
            0,
            &[history],
            None,
            Some(boundary_seconds * 1_000_000_000),
        )
        .expect("calendar history seeds");

        live.accept_trade(&rithmic_trade(1, 1, same_bucket, 112))
            .expect("same calendar bucket updates");
        assert_eq!(live.bars.len(), 1);
        assert_eq!(live.bars[0].close, 112);
        assert_eq!(live.bars[0].volume, 9);

        live.accept_trade(&rithmic_trade(2, 1, next_bucket, 115))
            .expect("next exchange session bucket starts");
        assert_eq!(live.bars.len(), 2);
        assert_eq!(live.bars[1].source_sequence, 41);
        assert_eq!(live.bars[1].close, 115);
        assert_eq!(live.bars[1].volume, 2);
    }
}

#[test]
fn rithmic_depth_is_reconstructed_once_and_published_as_a_conflated_engine_book() {
    let consumer_id = ConsumerId(id(9).expect("consumer"));
    let client_id = ClientId(id(7).expect("client"));
    let series = BarSeriesKey {
        provider_id: "rithmic".to_string(),
        instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
        entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
        period: BarPeriod::time(60).expect("period"),
        definition_version: 1,
    };
    let mut engine = configured_engine().expect("engine");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id,
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand_with_streams(
            consumer_id,
            GenerationId(id(3).expect("generation")),
            &series,
            chart_stream_requirements(&series),
        )
        .expect("depth demand installs");
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let stop = Arc::new(AtomicBool::new(false));
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &stop,
        consumer_id,
        &series,
    );
    coordinator
        .install_provider_instrument(&provider_instrument(7, 2))
        .expect("instrument installs");
    coordinator.provider_depth(
        "rithmic",
        7,
        &DepthSnapshot {
            metadata: EventMetadata {
                provider_id: "rithmic".to_string(),
                instrument_id: series.instrument_id.clone(),
                entitlement_id: series.entitlement_id.clone(),
                source_sequence: 11,
                session_generation: 7,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(20),
                    provider_unix_nanos: None,
                    received_unix_nanos: 21,
                },
            },
            bids: vec![DepthLevel {
                price: 20_000,
                quantity: 7,
                order_count: Some(3),
            }],
            asks: vec![DepthLevel {
                price: 20_025,
                quantity: 4,
                order_count: Some(2),
            }],
        },
    );

    let envelope::Payload::OrderBookSnapshot(snapshot) = coordinator
        .events
        .get_mut(&consumer_id)
        .and_then(ConsumerEvents::pop)
        .expect("book publishes")
    else {
        panic!("engine must publish the order book");
    };
    assert_eq!(snapshot.consumer_id, 9);
    assert_eq!(snapshot.generation, 3);
    assert_eq!(snapshot.provider_generation, 7);
    assert_eq!(snapshot.selection_generation, 2);
    assert_eq!(snapshot.source_watermark, 11);
    assert_eq!(snapshot.state, IpcOrderBookState::Ready as i32);
    assert_eq!(snapshot.bids[0].quantity, 7);
    assert_eq!(snapshot.asks[0].price, 20_025);

    coordinator.rithmic_trade(7, &rithmic_trade(12, 7, 22, 20_000));
    let traded = pop_order_book(&mut coordinator, consumer_id);
    assert_eq!(traded.bids[0].traded_volume, 2);
}

#[test]
fn unrelated_hidden_consumer_cannot_evict_visible_rithmic_depth() {
    let depth_consumer = ConsumerId(id(9).expect("depth consumer"));
    let unrelated_consumer = ConsumerId(id(10).expect("unrelated consumer"));
    let client_id = ClientId(id(7).expect("client"));
    let series = rithmic_series_key("instrument:rithmic:CME:MNQU6", "rithmic-test:CME:MNQU6");
    let mut engine = configured_engine().expect("engine");
    for consumer_id in [depth_consumer, unrelated_consumer] {
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id,
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
    }
    engine
        .set_series_demand_with_streams(
            depth_consumer,
            GenerationId(id(3).expect("generation")),
            &series,
            chart_stream_requirements(&series),
        )
        .expect("depth demand installs");
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let stop = Arc::new(AtomicBool::new(false));
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &stop,
        depth_consumer,
        &series,
    );
    coordinator
        .events
        .insert(unrelated_consumer, ConsumerEvents::default());
    coordinator
        .install_provider_instrument(&provider_instrument(7, 2))
        .expect("instrument installs");
    coordinator.provider_depth("rithmic", 7, &rithmic_depth_snapshot(&series, 11, 7));
    let first = pop_order_book(&mut coordinator, depth_consumer);
    assert_eq!(first.source_watermark, 11);
    assert_eq!(first.bids[0].quantity, 7);

    coordinator
        .engine
        .set_visibility(unrelated_consumer, false)
        .expect("unrelated consumer hides");
    coordinator.refresh_resource_policy();
    assert!(
        coordinator
            .order_books
            .contains_key(&("rithmic".to_string(), series.instrument_id.clone()))
    );
    coordinator.provider_depth("rithmic", 7, &rithmic_depth_snapshot(&series, 12, 9));
    let advanced = pop_order_book(&mut coordinator, depth_consumer);
    assert_eq!(advanced.source_watermark, 12);
    assert_eq!(advanced.bids[0].quantity, 9);

    coordinator
        .engine
        .set_visibility(depth_consumer, false)
        .expect("final visible depth reference hides");
    coordinator.refresh_resource_policy();
    assert!(coordinator.order_books.is_empty());

    coordinator
        .apply_resource_mode(ResourceMode::MarketsLive)
        .unwrap();
    assert!(
        coordinator
            .order_books
            .contains_key(&("rithmic".to_string(), series.instrument_id.clone()))
    );
    coordinator.apply_resource_mode(ResourceMode::Warm).unwrap();
    assert!(coordinator.order_books.is_empty());

    coordinator
        .engine
        .set_visibility(depth_consumer, true)
        .expect("depth demand returns");
    coordinator.refresh_resource_policy();
    assert!(matches!(
        coordinator
            .order_books
            .get(&("rithmic".to_string(), series.instrument_id.clone()))
            .map(|book| book.book.state()),
        Some(CanonicalOrderBookState::Recovering(
            OrderBookRecoveryReason::AwaitingSnapshot
        ))
    ));
    coordinator.provider_depth("rithmic", 7, &rithmic_depth_snapshot(&series, 20, 12));
    let recovered = pop_order_book(&mut coordinator, depth_consumer);
    assert_eq!(recovered.source_watermark, 20);
    assert_eq!(recovered.state, IpcOrderBookState::Ready as i32);

    assert!(coordinator.engine.remove_consumer(depth_consumer));
    coordinator.events.remove(&depth_consumer);
    coordinator.refresh_resource_policy();
    assert!(coordinator.order_books.is_empty());
}

#[test]
fn provider_instrument_catalog_rejects_stale_and_conflicting_installs() {
    let service = MarketService::start_fixture(vec![history_bar()]).expect("market service");
    let installed = provider_instrument(2, 3);
    service
        .install_provider_instrument(&installed)
        .expect("install current instrument");
    service
        .install_provider_instrument(&installed)
        .expect("repeat idempotent install");

    assert!(
        service
            .install_provider_instrument(&provider_instrument(1, 4))
            .is_err()
    );
    assert!(
        service
            .install_provider_instrument(&provider_instrument(2, 2))
            .is_err()
    );
    let mut conflicting = installed;
    conflicting.provider_symbol = "NQU6".to_string();
    assert!(service.install_provider_instrument(&conflicting).is_err());
}

#[test]
fn coinbase_catalog_does_not_share_selection_fences_between_consumers() {
    let service = MarketService::start_fixture(vec![history_bar()]).expect("market service");
    let product = |base: &str, selection_generation: u64| InstallProviderInstrument {
        provider: "coinbase".to_string(),
        session_generation: COINBASE_PROVIDER_GENERATION,
        selection_generation,
        instrument_id: format!("instrument:coinbase:{}:usd", base.to_ascii_lowercase()),
        provider_symbol: format!("{base}-USD"),
        display_symbol: format!("{base}/USD"),
        venue_id: "coinbase".to_string(),
        price_scale: 2,
        quantity_scale: 8,
        entitlement_id: ENTITLEMENT_CLASS.to_string(),
    };

    service
        .install_provider_instrument(&product("BTC", 2))
        .expect("BTC installs");
    service
        .install_provider_instrument(&product("ETH", 1))
        .expect("independent ETH generation installs");
    service
        .install_provider_instrument(&product("BTC", 1))
        .expect("independent consumer generation installs");
}

#[test]
fn removing_or_detaching_consumers_releases_coinbase_catalog_authorization() {
    for detach in [false, true] {
        let client_id = ClientId(id(7).expect("client"));
        let consumer_id = ConsumerId(id(9).expect("consumer"));
        let series = internal_series(&btc()).expect("series");
        let mut engine = configured_engine().expect("engine configures");
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id,
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        let (history_tx, _history_rx) = mpsc::sync_channel(1);
        let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
        let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
        let realtime_stop = Arc::new(AtomicBool::new(false));
        let catalog = CoinbaseCatalogControl::test_control();
        catalog
            .authorize_consumer(consumer_id.0.get())
            .expect("catalog authorizes consumer");
        let mut coordinator = retained_history_coordinator(
            engine,
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            consumer_id,
            &series,
        );
        coordinator.providers.set_coinbase_catalog_control(&catalog);
        coordinator.attached.insert(client_id);
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);

        if detach {
            coordinator.handle_service_command(Command::Detach(client_id, reply_tx));
        } else {
            coordinator.handle_consumer_command(Command::Remove(client_id, consumer_id, reply_tx));
        }

        assert_eq!(reply_rx.recv().expect("command replies"), Ok(()));
        assert!(!catalog.is_consumer_authorized(consumer_id.0.get()));
    }
}

#[test]
fn reauthorized_consumer_discards_queued_coinbase_selection_from_old_generation() {
    let client_id = ClientId(id(7).expect("client"));
    let consumer_id = ConsumerId(id(9).expect("consumer"));
    let series = internal_series(&btc()).expect("series");
    let mut engine = configured_engine().expect("engine configures");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id,
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(false));
    let catalog = CoinbaseCatalogControl::test_control();
    catalog
        .authorize_consumer(consumer_id.0.get())
        .expect("first authorization installs");
    catalog.release_consumer(consumer_id.0.get());
    catalog
        .authorize_consumer(consumer_id.0.get())
        .expect("replacement authorization installs");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    coordinator.providers.set_coinbase_catalog_control(&catalog);
    let instrument = InstallProviderInstrument {
        provider: "coinbase".to_string(),
        session_generation: COINBASE_PROVIDER_GENERATION,
        selection_generation: 1,
        instrument_id: "instrument:coinbase:sol:usd".to_string(),
        provider_symbol: "SOL-USD".to_string(),
        display_symbol: "SOL/USD".to_string(),
        venue_id: "coinbase".to_string(),
        price_scale: 2,
        quantity_scale: 8,
        entitlement_id: ENTITLEMENT_CLASS.to_string(),
    };

    coordinator.handle_coinbase_catalog(CoinbaseCatalogEvent::SelectionResolved {
        consumer_id: consumer_id.0.get(),
        instrument: instrument.clone(),
        authorization_generation: 1,
    });

    assert!(!coordinator.catalog.contains_key(&(
        instrument.provider.clone(),
        instrument.instrument_id.clone()
    )));
    assert!(coordinator.events[&consumer_id].catalog_selection.is_none());
}

#[test]
fn engine_catalog_selection_installs_identity_before_publication() {
    let mut engine = configured_engine().expect("engine configures");
    let client_id = ClientId(id(7).expect("client"));
    let consumer_id = ConsumerId(id(9).expect("consumer"));
    let series = BarSeriesKey {
        provider_id: "rithmic".to_string(),
        instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
        entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
        period: BarPeriod::tick(100).expect("tick period"),
        definition_version: 1,
    };
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id,
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let stop = Arc::new(AtomicBool::new(false));
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &stop,
        consumer_id,
        &series,
    );
    let instrument = provider_instrument(7, 3);
    coordinator.handle_rithmic_catalog(RithmicCatalogEvent::SelectionResolved {
        consumer_id: consumer_id.0.get(),
        instrument: instrument.clone(),
    });

    let envelope::Payload::ProviderInstrumentSelection(selection) = coordinator
        .events
        .get_mut(&consumer_id)
        .and_then(ConsumerEvents::pop)
        .expect("selection publishes")
    else {
        panic!("engine must publish the installed selection");
    };
    assert_eq!(selection.consumer_id, consumer_id.0.get());
    assert_eq!(selection.instrument, Some(instrument.clone()));
    assert_eq!(
        coordinator.catalog.get(&(
            instrument.provider.clone(),
            instrument.instrument_id.clone()
        )),
        Some(&instrument)
    );
}

#[test]
fn protocol_series_identity_roundtrips_every_rithmic_chart_cadence() {
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
    for period in periods {
        let internal = BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "rithmic:CME:MNQU6".to_string(),
            entitlement_id: "rithmic-test:CME-Delayed:MNQU6".to_string(),
            period,
            definition_version: 1,
        };
        assert!(crate::rithmic_history::chart_interval(period).is_ok());
        assert_eq!(internal_series(&ipc_series(&internal)), Ok(internal));
    }

    let mut invalid = ipc_series(&BarSeriesKey {
        provider_id: "rithmic".to_string(),
        instrument_id: "rithmic:CME:MNQU6".to_string(),
        entitlement_id: "rithmic-test:CME-Delayed:MNQU6".to_string(),
        period: BarPeriod::time(60).expect("1m"),
        definition_version: 1,
    });
    invalid.cadence = SeriesCadence::Unspecified as i32;
    assert!(internal_series(&invalid).is_err());
}

#[test]
fn installed_rithmic_demand_uses_the_engine_history_owner() {
    let service = MarketService::start_fixture(vec![history_bar()]).expect("market service");
    service.attach(7).expect("client attaches");
    service
        .register_consumer(7, 1, 9)
        .expect("consumer registers");
    let series = SeriesKey {
        provider: "rithmic".to_string(),
        instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
        cadence_value: 100,
        definition_revision: 1,
        entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
        cadence: SeriesCadence::Trades as i32,
    };
    assert!(service.set_demand(7, 9, 1, &series).is_err());
    service
        .install_provider_instrument(&provider_instrument(11, 4))
        .expect("Rithmic instrument installs");
    service
        .set_demand(7, 9, 2, &series)
        .expect("Rithmic history demand is accepted");

    let event = poll_until(&service, 7, 9, |event| {
        matches!(event, envelope::Payload::SeriesSnapshot(_))
    });
    assert!(matches!(
        event,
        envelope::Payload::SeriesSnapshot(snapshot)
            if snapshot.generation == 2
                && snapshot.provider_generation == 11
                && snapshot.price_scale == 2
                && snapshot.quantity_scale == 0
                && snapshot.series == Some(series)
                && snapshot.bars.len() == 1
    ));
}

#[test]
fn newer_rithmic_catalog_session_install_advances_the_engine_generation() {
    let service = MarketService::start_fixture(vec![history_bar()]).expect("market service");
    service.attach(7).expect("client attaches");
    service
        .register_consumer(7, 1, 9)
        .expect("consumer registers");
    service
        .install_provider_instrument(&provider_instrument(11, 4))
        .expect("first catalog session installs");
    // A resident engine outlives desktop sessions, so a reopened catalog
    // session installs with a newer session generation. History demand is
    // fenced against the engine generation and must observe the new one.
    service
        .install_provider_instrument(&provider_instrument(12, 1))
        .expect("newer catalog session installs");
    let series = SeriesKey {
        provider: "rithmic".to_string(),
        instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
        cadence_value: 100,
        definition_revision: 1,
        entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
        cadence: SeriesCadence::Trades as i32,
    };
    service
        .set_demand(7, 9, 2, &series)
        .expect("Rithmic history demand is accepted");
    let event = poll_until(&service, 7, 9, |event| {
        matches!(event, envelope::Payload::SeriesSnapshot(_))
    });
    assert!(matches!(
        event,
        envelope::Payload::SeriesSnapshot(snapshot)
            if snapshot.provider_generation == 12 && snapshot.bars.len() == 1
    ));
}

#[test]
fn provider_runtime_registry_rejects_duplicate_provider_identity() {
    let result = MarketService::start_composed(
        vec![
            ProviderRuntimeSpec::coinbase(
                Box::new(FixtureHistory {
                    bars: vec![history_bar()],
                    fetches: None,
                }),
                None,
            ),
            ProviderRuntimeSpec::coinbase(
                Box::new(FixtureHistory {
                    bars: vec![history_bar()],
                    fetches: None,
                }),
                None,
            ),
        ],
        None,
        0,
    );
    assert!(matches!(
        result,
        Err(error) if error == "provider runtime coinbase is configured more than once"
    ));
}

#[test]
fn rithmic_history_cancels_without_blocking_coinbase_history() {
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (cancelled_tx, cancelled_rx) = mpsc::sync_channel(1);
    let service = MarketService::start_composed(
        vec![
            ProviderRuntimeSpec::coinbase(
                Box::new(FixtureHistory {
                    bars: vec![history_bar()],
                    fetches: None,
                }),
                None,
            ),
            ProviderRuntimeSpec::rithmic(
                Box::new(BlockingRithmicHistory {
                    started: started_tx,
                    cancelled: cancelled_tx,
                }),
                false,
            ),
        ],
        None,
        0,
    )
    .expect("split provider history starts");
    service.attach(1).expect("client attaches");
    service
        .register_consumer(1, 1, 1)
        .expect("Rithmic consumer registers");
    service
        .register_consumer(1, 1, 2)
        .expect("Coinbase consumer registers");
    service
        .install_provider_instrument(&provider_instrument(5, 2))
        .expect("Rithmic instrument installs");
    let rithmic = SeriesKey {
        provider: "rithmic".to_string(),
        instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
        cadence_value: 100,
        definition_revision: 1,
        entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
        cadence: SeriesCadence::Trades as i32,
    };
    service
        .set_demand(1, 1, 1, &rithmic)
        .expect("Rithmic demand starts");
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("Rithmic history is active");

    service
        .set_demand(1, 2, 1, &btc())
        .expect("Coinbase demand starts independently");
    assert!(matches!(
        poll_until(&service, 1, 2, |event| matches!(
            event,
            envelope::Payload::SeriesSnapshot(_)
        )),
        envelope::Payload::SeriesSnapshot(_)
    ));
    service
        .remove_consumer(1, 1)
        .expect("Rithmic consumer removes");
    cancelled_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("Rithmic history cancellation reaches its worker");
}

impl HistorySource for ControlledHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        let series = &request.series;
        self.fetches.fetch_add(1, Ordering::AcqRel);
        self.release
            .recv()
            .map_err(|_| "test history release disconnected".to_string())?;
        let profile = coinbase_series_profile(series, &coinbase_instrument(series))?;
        // A run starting at bucket zero, numbered the way the adapter numbers
        // it. Deriving a coarser series needs source bars that cover a whole
        // target bucket; a single bar covers none of one.
        let interval = profile.interval.fixed_seconds().unwrap_or(86_400);
        Ok(HistorySnapshot {
            price_scale: profile.price_scale,
            quantity_scale: profile.quantity_scale,
            bars: (0..5)
                .map(|index| MarketBar {
                    source_sequence: index + 1,
                    exchange_timestamp_seconds: i64::try_from(index).unwrap_or(0) * interval,
                    exchange_timestamp_unix_nanos: i64::try_from(index).unwrap_or(0)
                        * interval
                        * 1_000_000_000,
                    open: 100,
                    high: 110,
                    low: 90,
                    close: 105,
                    volume: 7,
                })
                .collect(),
            forming: None,
            handoff_boundary_unix_nanos: None,
            confirmed_empty: false,
        })
    }
}

impl HistorySource for SwitchingHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        let series = &request.series;
        self.requested
            .send(series.clone())
            .map_err(|_| "switch history observer disconnected".to_string())?;
        self.release
            .recv()
            .map_err(|_| "switch history release disconnected".to_string())?;
        let profile = coinbase_series_profile(series, &coinbase_instrument(series))?;
        Ok(HistorySnapshot {
            price_scale: profile.price_scale,
            quantity_scale: profile.quantity_scale,
            bars: vec![MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 0,
                exchange_timestamp_unix_nanos: 0,
                open: profile.interval.fixed_seconds().unwrap_or(86_400),
                high: profile.interval.fixed_seconds().unwrap_or(86_400),
                low: profile.interval.fixed_seconds().unwrap_or(86_400),
                close: profile.interval.fixed_seconds().unwrap_or(86_400),
                volume: 1,
            }],
            forming: None,
            handoff_boundary_unix_nanos: None,
            confirmed_empty: false,
        })
    }
}

impl HistorySource for DelayedCancellationHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        self.started
            .send((request.series.clone(), request.provider_generation))
            .map_err(|_| "history start observer disconnected".to_string())?;
        if std::mem::take(&mut self.block_first) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !request.stop.load(Ordering::Acquire) {
                if Instant::now() >= deadline {
                    return Err("superseded history was not cancelled".to_string());
                }
                thread::yield_now();
            }
            self.cancellation_observed
                .send(())
                .map_err(|_| "cancellation observer disconnected".to_string())?;
            self.release_cancellation
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| "cancellation release timed out".to_string())?;
            return Err("superseded history was cancelled".to_string());
        }
        let instrument = request
            .instrument
            .as_ref()
            .ok_or_else(|| "fixture Coinbase instrument is unavailable".to_string())?;
        let profile = coinbase_series_profile(&request.series, instrument)?;
        Ok(HistorySnapshot {
            price_scale: profile.price_scale,
            quantity_scale: profile.quantity_scale,
            bars: vec![MarketBar {
                exchange_timestamp_seconds: profile.interval.fixed_seconds().unwrap_or(86_400),
                exchange_timestamp_unix_nanos: profile.interval.fixed_seconds().unwrap_or(86_400)
                    * 1_000_000_000,
                ..history_bar()
            }],
            forming: None,
            handoff_boundary_unix_nanos: None,
            confirmed_empty: false,
        })
    }
}

impl HistorySource for ControlledHistoryFailure {
    fn fetch(&mut self, _request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        self.started
            .send(())
            .map_err(|_| "history failure observer disconnected".to_string())?;
        self.release
            .recv()
            .map_err(|_| "history failure release disconnected".to_string())?;
        Err("fixture provider history failed".to_string())
    }
}

impl HistorySource for BlockingRithmicHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        self.started
            .send(())
            .map_err(|_| "Rithmic start observer disconnected".to_string())?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !request.stop.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return Err("Rithmic cancellation timed out".to_string());
            }
            thread::sleep(Duration::from_millis(1));
        }
        self.cancelled
            .send(())
            .map_err(|_| "Rithmic cancellation observer disconnected".to_string())?;
        Err("Rithmic history request was cancelled".to_string())
    }
}

impl HistorySource for UncancellableHistory {
    fn fetch(&mut self, _request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        self.started
            .send(())
            .map_err(|_| "history start observer disconnected".to_string())?;
        self.release
            .recv()
            .map_err(|_| "history release disconnected".to_string())?;
        self.exited
            .send(())
            .map_err(|_| "history exit observer disconnected".to_string())?;
        Err("fixture history stopped".to_string())
    }
}

#[test]
fn shutdown_cancels_inflight_history_and_joins_owned_workers() {
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (cancelled_tx, cancelled_rx) = mpsc::sync_channel(1);
    let service = MarketService::start_with_source(BlockingRithmicHistory {
        started: started_tx,
        cancelled: cancelled_tx,
    })
    .expect("market service starts");
    service.attach(1).expect("client attaches");
    service
        .register_consumer(1, 1, 1)
        .expect("consumer registers");
    service
        .set_demand(1, 1, 1, &btc())
        .expect("history demand starts");
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("history worker starts");

    service
        .shutdown(Duration::from_secs(1))
        .expect("owned market workers stop before the deadline");

    cancelled_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("inflight history observes cancellation");
    assert!(service.attach(2).is_err());
}

#[test]
fn shutdown_deadline_reports_an_uncancellable_worker_without_waiting_forever() {
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let (exited_tx, exited_rx) = mpsc::sync_channel(1);
    let service = MarketService::start_with_source(UncancellableHistory {
        started: started_tx,
        release: release_rx,
        exited: exited_tx,
    })
    .expect("market service starts");
    service.attach(1).expect("client attaches");
    service
        .register_consumer(1, 1, 1)
        .expect("consumer registers");
    service
        .set_demand(1, 1, 1, &btc())
        .expect("history demand starts");
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("history worker starts");

    let error = service
        .shutdown(Duration::from_millis(20))
        .expect_err("uncancellable history must hit the process deadline");

    assert!(error.contains("axiusflow-coinbase-history"));
    release_tx.send(()).expect("blocked history releases");
    exited_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("detached worker exits after release");
}

fn btc() -> SeriesKey {
    SeriesKey {
        provider: "coinbase".to_string(),
        instrument_id: "instrument:coinbase:btc:usd".to_string(),
        cadence_value: 60,
        definition_revision: 1,
        entitlement_id: ENTITLEMENT_CLASS.to_string(),
        cadence: SeriesCadence::FixedSeconds as i32,
    }
}

fn coinbase_instrument(series: &BarSeriesKey) -> InstallProviderInstrument {
    let base = series
        .instrument_id
        .strip_prefix("instrument:coinbase:")
        .and_then(|value| value.strip_suffix(":usd"))
        .unwrap_or("btc")
        .to_ascii_uppercase();
    InstallProviderInstrument {
        provider: "coinbase".to_string(),
        session_generation: 1,
        selection_generation: 1,
        instrument_id: series.instrument_id.clone(),
        provider_symbol: format!("{base}-USD"),
        display_symbol: format!("{base}/USD"),
        venue_id: "coinbase".to_string(),
        price_scale: 2,
        quantity_scale: 8,
        entitlement_id: ENTITLEMENT_CLASS.to_string(),
    }
}

#[test]
fn coinbase_calendar_history_drops_the_forming_bucket_before_persistence() {
    for interval in [CoinbaseInterval::Week1, CoinbaseInterval::Month1] {
        let completed_before = interval
            .bucket_start(1_787_299_200)
            .expect("calendar boundary");
        let previous = interval
            .shift_bucket(completed_before, -1)
            .expect("previous bucket");
        let bars = vec![
            MarketBar {
                exchange_timestamp_seconds: previous,
                exchange_timestamp_unix_nanos: previous * 1_000_000_000,
                ..history_bar()
            },
            MarketBar {
                exchange_timestamp_seconds: completed_before,
                exchange_timestamp_unix_nanos: completed_before * 1_000_000_000,
                ..history_bar()
            },
        ];

        let (bars, _) =
            aggregate_coinbase_bars(&bars, interval, Some(completed_before)).expect("aggregates");

        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].exchange_timestamp_seconds, previous);
    }
}

#[test]
fn coinbase_twelve_hour_and_calendar_demands_create_live_handoffs() {
    for (cadence, value) in [
        (SeriesCadence::FixedSeconds, 43_200),
        (SeriesCadence::CalendarWeeks, 1),
        (SeriesCadence::CalendarMonths, 1),
    ] {
        let series = internal_series(&SeriesKey {
            provider: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:btc:usd".to_string(),
            cadence_value: value,
            definition_revision: 1,
            entitlement_id: ENTITLEMENT_CLASS.to_string(),
            cadence: cadence as i32,
        })
        .expect("Coinbase series is supported");
        assert!(chart_stream_requirements(&series).contains(MarketStream::Trades));
        assert!(
            LiveHandoff::try_new(
                &series,
                ProviderGeneration(NonZeroU64::MIN),
                &coinbase_instrument(&series),
            )
            .is_ok()
        );
    }
}

fn selected_series(instrument_id: &str, interval_seconds: u32) -> SeriesKey {
    SeriesKey {
        provider: "coinbase".to_string(),
        instrument_id: instrument_id.to_string(),
        cadence_value: interval_seconds,
        definition_revision: 1,
        entitlement_id: ENTITLEMENT_CLASS.to_string(),
        cadence: SeriesCadence::FixedSeconds as i32,
    }
}

fn synthetic_coinbase_product(index: usize) -> (BarSeriesKey, InstallProviderInstrument) {
    let provider_symbol = format!("P{index:02}-USD");
    let instrument_id = format!("instrument:coinbase:p{index:02}:usd");
    (
        BarSeriesKey {
            provider_id: "coinbase".to_string(),
            instrument_id: instrument_id.clone(),
            entitlement_id: ENTITLEMENT_CLASS.to_string(),
            period: BarPeriod::time(60).expect("interval"),
            definition_version: 1,
        },
        InstallProviderInstrument {
            provider: "coinbase".to_string(),
            session_generation: 1,
            selection_generation: u64::try_from(index + 1).expect("selection generation"),
            instrument_id,
            provider_symbol: provider_symbol.clone(),
            display_symbol: provider_symbol,
            venue_id: "coinbase".to_string(),
            price_scale: 2,
            quantity_scale: 8,
            entitlement_id: ENTITLEMENT_CLASS.to_string(),
        },
    )
}

fn history_bar() -> MarketBar {
    MarketBar {
        source_sequence: 2,
        exchange_timestamp_seconds: 60,
        exchange_timestamp_unix_nanos: 60_000_000_000,
        open: 100,
        high: 110,
        low: 90,
        close: 105,
        volume: 7,
    }
}

/// Minute bars numbered the way the Coinbase adapter numbers them: sequence
/// is the bucket index since the epoch, plus one.
fn coinbase_history(first_minute: i64, count: usize) -> Vec<MarketBar> {
    sequential_history(
        u64::try_from(first_minute).expect("minute fits") + 1,
        count,
        first_minute,
    )
}

fn coinbase_forming_snapshot(
    bars: Vec<MarketBar>,
    forming: MarketBar,
    boundary: i64,
) -> HistorySnapshot {
    HistorySnapshot {
        price_scale: 2,
        quantity_scale: 8,
        bars,
        forming: Some(FormingBar {
            bar: forming,
            trades: None,
        }),
        handoff_boundary_unix_nanos: Some(boundary),
        confirmed_empty: false,
    }
}

fn sequential_history(first_sequence: u64, count: usize, first_minute: i64) -> Vec<MarketBar> {
    (0..count)
        .map(|index| {
            let offset = u64::try_from(index).expect("history index fits");
            let minute = first_minute + i64::try_from(index).expect("history minute fits");
            MarketBar {
                source_sequence: first_sequence + offset,
                exchange_timestamp_seconds: minute * 60,
                exchange_timestamp_unix_nanos: minute * 60_000_000_000,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            }
        })
        .collect()
}

fn expect_range_read(request: &StorageRequest) -> HistoryRange {
    let StorageRequest::ReadRange(_, _, range) = request else {
        panic!("viewport uses a range read");
    };
    *range
}

#[test]
fn viewport_demand_enqueues_one_bounded_missing_history_range() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let mut engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let generation = GenerationId(id(1).expect("generation"));
    let series = internal_series(&btc()).expect("series");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand(consumer_id, generation, &series)
        .expect("demand installs");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    let interval = 60_000_000_000_i64;
    let end =
        align_down(current_unix_nanos().expect("clock"), interval).saturating_sub(100 * interval);
    let viewport = Viewport::try_new(end.saturating_sub(200 * interval), end).expect("viewport");

    coordinator
        .request_viewport_history(consumer_id, generation, viewport)
        .expect("viewport demand applies");

    let local = storage_rx.try_recv().expect("local range read is queued");
    let range = match local {
        StorageRequest::ReadRange(_, _, range) => range,
        StorageRequest::Read(..)
        | StorageRequest::Persist(..)
        | StorageRequest::RecordConfirmedEmpty(..)
        | StorageRequest::ResolveConfirmedEmpty(..) => {
            panic!("viewport uses a range read")
        }
    };
    coordinator.viewport_history_local_completed(
        &series,
        ProviderGeneration(NonZeroU64::MIN),
        range,
        Err("fixture local history is unavailable".to_string()),
    );
    let request = history_rx.try_recv().expect("missing range is queued");
    let range = request.range.expect("Coinbase range is explicit");
    assert_eq!(request.kind, HistoryRequestKind::ViewportBackfill);
    assert!(range.start_unix_nanos <= viewport.start_unix_nanos);
    assert!(range.end_unix_nanos >= viewport.end_unix_nanos);
    assert!(
        range.end_unix_nanos - range.start_unix_nanos
            <= i64::try_from(VIEWPORT_BACKFILL_BARS).expect("bound fits") * interval
    );
    assert!(history_rx.try_recv().is_err());
}

#[test]
fn cache_protection_retains_active_viewport_after_repair_and_includes_live_tail() {
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let mut engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let consumer_generation = GenerationId(id(1).expect("generation"));
    let provider_generation = ProviderGeneration(NonZeroU64::MIN);
    let series = internal_series(&btc()).expect("series");
    let bars = coinbase_history(1, 1_000);
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand(consumer_id, consumer_generation, &series)
        .expect("demand installs");
    engine
        .install_history(provider_generation, &series, 2, 8, bars.clone())
        .expect("history installs");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    coordinator.live.insert(
        series.clone(),
        LiveHandoff::try_new(&series, provider_generation, &coinbase_instrument(&series))
            .expect("live handoff"),
    );
    let viewport = HistoryRange {
        start_unix_nanos: 120_000_000_000,
        end_unix_nanos: 240_000_000_000,
    };
    coordinator.active_viewports.insert(
        consumer_id,
        ActiveViewport {
            consumer_generation,
            provider_generation,
            series: series.clone(),
            range: viewport,
        },
    );
    coordinator.viewport_history_ranges.clear();

    let protected = coordinator.protected_history_ranges();
    assert!(protected.contains(&(series.clone(), viewport)));
    assert!(
        protected.contains(&(
            series.clone(),
            HistoryRange {
                start_unix_nanos: bars[bars.len() - VIEWPORT_LIVE_TAIL_RESERVE]
                    .exchange_timestamp_unix_nanos,
                end_unix_nanos: bars
                    .last()
                    .expect("live tail")
                    .exchange_timestamp_unix_nanos
                    + 60_000_000_000,
            },
        ))
    );
    assert_eq!(protected.len(), 2);
}

#[test]
fn saturated_persistence_lane_retries_real_bar_empty_evidence_repair() {
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    let (storage_tx, storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let series = internal_series(&btc()).expect("series");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    storage_tx
        .try_send(StorageRequest::Read(
            series.clone(),
            ProviderGeneration(NonZeroU64::MIN),
        ))
        .expect("storage lane fills");

    coordinator.enqueue_persistence(
        &series,
        ProviderGeneration(NonZeroU64::MIN),
        vec![history_bar()],
        false,
        "fixture persistence is unavailable",
    );

    assert!(coordinator.pending_empty_repairs.contains_key(&series));
    let _ = storage_rx.try_recv().expect("filler request drains");
    coordinator.empty_repair_retry_at = Instant::now();
    coordinator.retry_pending_empty_repairs();
    assert!(matches!(
        storage_rx.try_recv(),
        Ok(StorageRequest::ResolveConfirmedEmpty(ref repaired, range))
            if repaired == &series
                && range.start_unix_nanos == 60_000_000_000
                && range.end_unix_nanos == 120_000_000_000
    ));
    assert!(coordinator.pending_empty_repairs.is_empty());
}

#[test]
fn confirmed_empty_viewport_page_is_persisted_and_skipped_after_restore() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, storage_rx) = mpsc::sync_channel(2);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let mut engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let generation = GenerationId(id(1).expect("generation"));
    let series = internal_series(&btc()).expect("series");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand(consumer_id, generation, &series)
        .expect("demand installs");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    let provider_generation = coordinator
        .provider_generation_for_series(&series)
        .expect("provider generation");
    let interval = 60_000_000_000_i64;
    let end =
        align_down(current_unix_nanos().expect("clock"), interval).saturating_sub(2_000 * interval);
    let viewport = Viewport::try_new(end.saturating_sub(200 * interval), end).expect("viewport");

    coordinator
        .request_viewport_history(consumer_id, generation, viewport)
        .expect("viewport demand applies");
    let local_range = expect_range_read(&storage_rx.try_recv().expect("local read is queued"));
    coordinator.viewport_history_local_completed(
        &series,
        provider_generation,
        local_range,
        Err("fixture local history is unavailable".to_string()),
    );
    let provider_request = history_rx.try_recv().expect("provider page is queued");
    let page = provider_request.range.expect("provider page is explicit");
    coordinator.history_completed(
        &series,
        provider_generation,
        Some(page),
        HistoryRequestKind::ViewportBackfill,
        Ok(HistorySnapshot {
            price_scale: 2,
            quantity_scale: 8,
            bars: Vec::new(),
            forming: None,
            handoff_boundary_unix_nanos: Some(page.end_unix_nanos),
            confirmed_empty: true,
        }),
    );
    assert!(matches!(
        storage_rx.try_recv().expect("empty page is persisted"),
        StorageRequest::RecordConfirmedEmpty(ref requested, range)
            if requested == &series && range == page
    ));
    let continuation = history_rx
        .try_recv()
        .expect("empty range does not suppress other history");
    assert_ne!(continuation.range, Some(page));

    coordinator.history_inflight.clear();
    coordinator.history_cancellations.clear();
    coordinator.history_coverage.clear();
    coordinator.viewport_history_ranges.clear();
    coordinator.viewport_history_local_inflight.clear();
    coordinator
        .request_viewport_history(consumer_id, generation, viewport)
        .expect("restored viewport checks local coverage");
    let restored_range = expect_range_read(
        &storage_rx
            .try_recv()
            .expect("restored local read is queued"),
    );
    coordinator.viewport_history_local_completed(
        &series,
        provider_generation,
        restored_range,
        Ok(LocalRangeHistory {
            stored: None,
            confirmed_empty: vec![page],
        }),
    );
    let restored_request = history_rx
        .try_recv()
        .expect("uncovered history continues after restored empty range");
    assert_ne!(restored_request.range, Some(page));
}

#[test]
fn empty_viewport_completion_cannot_hide_an_initialized_live_series() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(false));
    let mut engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let generation = ProviderGeneration(NonZeroU64::MIN);
    let series = internal_series(&btc()).expect("series");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand(
            consumer_id,
            GenerationId(id(1).expect("generation")),
            &series,
        )
        .expect("demand installs");
    engine
        .install_history(generation, &series, 2, 8, vec![history_bar()])
        .expect("history installs");
    engine
        .set_provider_health("coinbase", generation, ProviderHealth::Online)
        .expect("provider is online");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    let mut live = LiveHandoff::try_new(&series, generation, &coinbase_instrument(&series))
        .expect("live handoff");
    live.connected = true;
    live.history = CoinbaseHistoryReadiness::Authoritative;
    coordinator.live.insert(series.clone(), live);
    let key = (series.clone(), generation);

    assert!(coordinator.handle_empty_history_snapshot(
        &series,
        HistoryRequestKind::ViewportBackfill,
        &key,
        None,
        &HistorySnapshot {
            price_scale: 2,
            quantity_scale: 8,
            bars: Vec::new(),
            forming: None,
            handoff_boundary_unix_nanos: None,
            confirmed_empty: true,
        },
    ));
    assert!(matches!(
        coordinator.events[&consumer_id].series_state,
        Some(envelope::Payload::SeriesState(ref state))
            if state.state == SeriesLoadState::Live as i32
    ));
    assert!(history_rx.try_recv().is_err());
}

#[test]
fn initial_coinbase_request_stops_at_latest_window_until_viewport_moves() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, storage_rx) = mpsc::sync_channel(2);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let mut engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let generation = GenerationId(id(1).expect("generation"));
    let series = internal_series(&btc()).expect("series");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand(consumer_id, generation, &series)
        .expect("demand installs");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    let provider_generation = coordinator
        .provider_generation_for_series(&series)
        .expect("provider generation");
    let interval = 60_000_000_000_i64;
    let end_minute = current_unix_nanos().expect("clock") / interval;
    let initial_range = recent_coinbase_history_range(
        &series,
        coordinator.resource_policy.history_prefetch_bars.max(1),
    )
    .expect("initial range");
    coordinator.history_completed(
        &series,
        provider_generation,
        Some(initial_range),
        HistoryRequestKind::Initial,
        Ok(HistorySnapshot {
            price_scale: 2,
            quantity_scale: 8,
            bars: coinbase_history(end_minute - 350, 350),
            forming: None,
            handoff_boundary_unix_nanos: Some(end_minute * interval),
            confirmed_empty: false,
        }),
    );

    assert!(
        history_rx.try_recv().is_err(),
        "initial completion must not start an unsolicited deep backfill"
    );
    assert!(coordinator.viewport_history_ranges.is_empty());
    assert!(matches!(
        storage_rx.recv_timeout(Duration::from_secs(1)),
        Ok(StorageRequest::Persist(..))
    ));

    let viewport = Viewport::try_new((end_minute - 600) * interval, (end_minute - 500) * interval)
        .expect("older viewport");
    coordinator
        .request_viewport_history(consumer_id, generation, viewport)
        .expect("scrolling left requests older history");
    assert!(matches!(
        storage_rx.recv_timeout(Duration::from_secs(1)),
        Ok(StorageRequest::ReadRange(requested, current, range))
            if requested == series
                && current == provider_generation
                && range.start_unix_nanos <= viewport.start_unix_nanos
                && range.end_unix_nanos >= viewport.end_unix_nanos
    ));
}
/// One consumer demanding BTC-USD 1m with a shorter-than-prefetch initial
/// window installed, so the working-window backfill stays disarmed.
#[allow(clippy::type_complexity)]
fn disarmed_backfill_fixture<'a>(
    history: &'a SyncSender<HistoryRequest>,
    storage: &'a SyncSender<StorageRequest>,
    realtime: &'a SyncSender<RealtimeControl>,
    realtime_stop: &'a Arc<AtomicBool>,
    storage_rx: &mpsc::Receiver<StorageRequest>,
) -> (
    Coordinator<'a>,
    ConsumerId,
    GenerationId,
    ProviderGeneration,
    BarSeriesKey,
    i64,
    fn(Vec<MarketBar>) -> HistorySnapshot,
) {
    let mut engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let generation = GenerationId(id(1).expect("generation"));
    let series = internal_series(&btc()).expect("series");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand(consumer_id, generation, &series)
        .expect("demand installs");
    let mut coordinator = retained_history_coordinator(
        engine,
        history,
        storage,
        realtime,
        realtime_stop,
        consumer_id,
        &series,
    );
    let provider_generation = coordinator
        .provider_generation_for_series(&series)
        .expect("provider generation");
    let interval = 60_000_000_000_i64;
    let end_minute = current_unix_nanos().expect("clock") / interval;
    coordinator.history_completed(
        &series,
        provider_generation,
        Some(
            recent_coinbase_history_range(
                &series,
                coordinator.resource_policy.history_prefetch_bars.max(1),
            )
            .expect("initial range"),
        ),
        HistoryRequestKind::Initial,
        Ok(backfill_snapshot(coinbase_history(end_minute - 200, 200))),
    );
    assert!(coordinator.events[&consumer_id].queued_series().is_some());
    coordinator
        .events
        .get_mut(&consumer_id)
        .expect("events")
        .clear_series();
    let _ = storage_rx.try_recv().expect("initial history persists");
    (
        coordinator,
        consumer_id,
        generation,
        provider_generation,
        series,
        end_minute,
        backfill_snapshot,
    )
}

fn backfill_snapshot(bars: Vec<MarketBar>) -> HistorySnapshot {
    HistorySnapshot {
        price_scale: 2,
        quantity_scale: 8,
        bars,
        forming: None,
        handoff_boundary_unix_nanos: None,
        confirmed_empty: false,
    }
}

#[test]
fn visible_viewport_pages_publish_and_background_pages_defer() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let (
        mut coordinator,
        consumer_id,
        generation,
        provider_generation,
        series,
        end_minute,
        snapshot,
    ) = disarmed_backfill_fixture(
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        &storage_rx,
    );
    let interval = 60_000_000_000_i64;
    let visible = Viewport::try_new(
        (end_minute - 2_000) * interval,
        (end_minute - 1_900) * interval,
    )
    .expect("visible viewport");
    coordinator
        .request_viewport_history(consumer_id, generation, visible)
        .expect("viewport demand queues");
    let StorageRequest::ReadRange(_, _, local_range) =
        storage_rx.try_recv().expect("local range read")
    else {
        panic!("viewport reads local coverage first");
    };
    coordinator.viewport_history_local_completed(
        &series,
        provider_generation,
        local_range,
        Err("fixture cache miss".to_string()),
    );
    let page = history_rx
        .recv()
        .expect("visible viewport schedules provider repair")
        .range
        .expect("provider page is explicit");
    let minutes = usize::try_from((page.end_unix_nanos - page.start_unix_nanos) / interval)
        .expect("page width fits");
    let start_minute = page.start_unix_nanos / interval;
    coordinator.history_completed(
        &series,
        provider_generation,
        Some(page),
        HistoryRequestKind::ViewportBackfill,
        Ok(snapshot(coinbase_history(start_minute, minutes))),
    );
    assert!(matches!(
        coordinator.events[&consumer_id].queued_series(),
        Some(envelope::Payload::SeriesSnapshot(snapshot)) if snapshot.bars.len() == 200 + minutes
    ));

    coordinator
        .events
        .get_mut(&consumer_id)
        .expect("events")
        .clear_series();
    let _ = storage_rx.try_recv().expect("visible page persists");

    // A page that does not adjoin the retained run cannot be spliced onto
    // it: the series would then carry a hole under a contiguous sequence,
    // which every consumer reads as one unbroken run. The page is still
    // persisted, so the pages that close the gap can pick it up.
    let installed = coordinator
        .engine
        .series_snapshot(&series)
        .expect("series is installed")
        .bars
        .len();
    coordinator.history_completed(
        &series,
        provider_generation,
        Some(HistoryRange {
            start_unix_nanos: (end_minute - 5_000) * interval,
            end_unix_nanos: (end_minute - 4_800) * interval,
        }),
        HistoryRequestKind::ViewportBackfill,
        Ok(snapshot(coinbase_history(end_minute - 5_000, 200))),
    );
    assert!(
        coordinator.events[&consumer_id].queued_series().is_none(),
        "a disjoint background page defers its snapshot"
    );
    assert_eq!(
        coordinator
            .engine
            .series_snapshot(&series)
            .expect("series survives a disjoint page")
            .bars
            .len(),
        installed
    );
    assert!(matches!(
        storage_rx.try_recv(),
        Ok(StorageRequest::Persist(_, _, ref bars, _, _, _)) if bars.len() == 200
    ));
}

#[test]
fn exhausting_the_demanded_repair_range_flushes_one_accumulated_covering_snapshot() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let (mut coordinator, consumer_id, generation, provider_generation, series, end_minute, _) =
        disarmed_backfill_fixture(
            &history_tx,
            &storage_tx,
            &realtime_tx,
            &realtime_stop,
            &storage_rx,
        );
    let interval = 60_000_000_000_i64;
    // Install one background page silently while no plan is pending. It
    // adjoins the retained run, so it becomes part of the canonical series.
    coordinator.history_completed(
        &series,
        provider_generation,
        Some(HistoryRange {
            start_unix_nanos: (end_minute - 400) * interval,
            end_unix_nanos: (end_minute - 200) * interval,
        }),
        HistoryRequestKind::ViewportBackfill,
        Ok(backfill_snapshot(coinbase_history(end_minute - 400, 200))),
    );
    assert_eq!(
        coordinator
            .engine
            .series_snapshot(&series)
            .expect("background page installs")
            .bars
            .len(),
        400
    );
    let _ = storage_rx.try_recv().expect("background page persists");

    // Demanding the same region keeps the accumulated snapshot deferred while
    // its repair retries. Exhaustion resolves the plan and flushes exactly one
    // covering snapshot carrying the silent install.
    let resolution =
        Viewport::try_new((end_minute - 500) * interval, (end_minute - 300) * interval)
            .expect("resolution viewport");
    coordinator
        .request_viewport_history(consumer_id, generation, resolution)
        .expect("resolution demand queues");
    let StorageRequest::ReadRange(_, _, resolution_range) =
        storage_rx.try_recv().expect("resolution reads coverage")
    else {
        panic!("resolution reads local coverage first");
    };
    coordinator.viewport_history_local_completed(
        &series,
        provider_generation,
        resolution_range,
        Err("fixture cache miss".to_string()),
    );
    coordinator
        .events
        .get_mut(&consumer_id)
        .expect("events")
        .demand_error = None;
    let mut request = history_rx.try_recv().expect("provider repair queues");
    for _ in 0..=MAXIMUM_VIEWPORT_HISTORY_RETRIES {
        coordinator.history_completed(
            &series,
            provider_generation,
            request.range,
            HistoryRequestKind::ViewportBackfill,
            Err("fixture backfill unavailable".to_string()),
        );
        let key = (series.clone(), provider_generation);
        if !coordinator.viewport_history_retries.contains_key(&key) {
            break;
        }
        assert!(
            coordinator.events[&consumer_id].queued_series().is_none(),
            "retrying keeps the accumulated snapshot deferred"
        );
        coordinator
            .viewport_history_retries
            .get_mut(&key)
            .expect("retry remains")
            .ready_at = Instant::now();
        coordinator.retry_viewport_history();
        request = history_rx.try_recv().expect("bounded retry queues");
    }
    assert!(matches!(
        coordinator.events[&consumer_id].queued_series(),
        Some(envelope::Payload::SeriesSnapshot(snapshot))
            if snapshot.bars.len() == 400
    ));
    assert!(coordinator.deferred_publications.is_empty());
}

#[test]
fn shared_series_viewports_union_without_cancelling_each_other() {
    let (history_tx, history_rx) = mpsc::sync_channel(2);
    let (storage_tx, storage_rx) = mpsc::sync_channel(4);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let mut engine = configured_engine().expect("engine configures");
    let first = ConsumerId(id(1).expect("first consumer"));
    let second = ConsumerId(id(2).expect("second consumer"));
    let generation = GenerationId(NonZeroU64::MIN);
    let series = internal_series(&btc()).expect("series");
    for consumer_id in [first, second] {
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(NonZeroU64::MIN),
                    workspace_id: WorkspaceId(consumer_id.0),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        engine
            .set_series_demand(consumer_id, generation, &series)
            .expect("shared demand installs");
    }
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        first,
        &series,
    );
    coordinator.events.insert(second, ConsumerEvents::default());
    let provider_generation = coordinator
        .provider_generation_for_series(&series)
        .expect("provider generation");
    let interval = 60_000_000_000_i64;
    let end = align_down(current_unix_nanos().expect("clock"), interval);
    let first_older =
        Viewport::try_new(end - 800 * interval, end - 700 * interval).expect("first viewport");
    coordinator
        .request_viewport_history(first, generation, first_older)
        .expect("first viewport queues");
    let StorageRequest::ReadRange(_, _, first_range) =
        storage_rx.try_recv().expect("first cache read")
    else {
        panic!("viewport uses a range read");
    };
    coordinator.viewport_history_local_completed(
        &series,
        provider_generation,
        first_range,
        Err("fixture cache miss".to_string()),
    );
    let _ = history_rx.try_recv().expect("provider page starts");
    let stop = Arc::clone(
        coordinator
            .history_cancellations
            .get(&(series.clone(), provider_generation))
            .expect("provider page is cancellable"),
    );

    let second_latest =
        Viewport::try_new(end - 100 * interval, end).expect("second latest viewport");
    coordinator
        .request_viewport_history(second, generation, second_latest)
        .expect("second viewport joins");
    assert_eq!(
        coordinator
            .viewport_history_ranges
            .get(&(series.clone(), provider_generation)),
        Some(&first_range),
        "a live-edge workspace cannot erase another workspace's older demand"
    );
    assert!(!stop.load(Ordering::Acquire));

    let second_older = Viewport::try_new(end - 1_600 * interval, end - 1_500 * interval)
        .expect("second older viewport");
    coordinator
        .request_viewport_history(second, generation, second_older)
        .expect("second viewport expands");
    assert!(
        coordinator
            .viewport_history_ranges
            .get(&(series.clone(), provider_generation))
            .is_some_and(|range| range.start_unix_nanos < first_range.start_unix_nanos),
        "the shared target expands to the oldest active workspace"
    );
    assert!(!stop.load(Ordering::Acquire));

    assert!(coordinator.engine.remove_consumer(second));
    coordinator.prune_history_tracking();
    assert_eq!(
        coordinator
            .viewport_history_ranges
            .get(&(series, provider_generation)),
        Some(&first_range),
        "retiring a workspace contracts the shared target to the remaining demand"
    );
}

#[test]
fn newer_viewport_rearms_after_an_older_backfill_fails() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let mut engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let generation = GenerationId(id(1).expect("generation"));
    let series = internal_series(&btc()).expect("series");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand(consumer_id, generation, &series)
        .expect("demand installs");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    let provider_generation = coordinator
        .provider_generation_for_series(&series)
        .expect("provider generation");
    let interval = 60_000_000_000_i64;
    let end = align_down(current_unix_nanos().expect("clock"), interval);
    let older =
        Viewport::try_new(end - 800 * interval, end - 700 * interval).expect("older viewport");
    let newest =
        Viewport::try_new(end - 1_600 * interval, end - 1_500 * interval).expect("newest viewport");

    coordinator
        .request_viewport_history(consumer_id, generation, older)
        .expect("older viewport queues");
    let first = match storage_rx.try_recv().expect("older local request") {
        StorageRequest::ReadRange(_, _, range) => range,
        StorageRequest::Read(..)
        | StorageRequest::Persist(..)
        | StorageRequest::RecordConfirmedEmpty(..)
        | StorageRequest::ResolveConfirmedEmpty(..) => {
            panic!("viewport uses a range read")
        }
    };
    coordinator
        .request_viewport_history(consumer_id, generation, newest)
        .expect("newer viewport is retained");
    coordinator.viewport_history_local_completed(
        &series,
        provider_generation,
        first,
        Err("fixture failure".to_string()),
    );

    let replacement = match storage_rx.try_recv().expect("newer local request") {
        StorageRequest::ReadRange(_, _, range) => range,
        StorageRequest::Read(..)
        | StorageRequest::Persist(..)
        | StorageRequest::RecordConfirmedEmpty(..)
        | StorageRequest::ResolveConfirmedEmpty(..) => {
            panic!("viewport uses a range read")
        }
    };
    coordinator.viewport_history_local_completed(
        &series,
        provider_generation,
        replacement,
        Err("fixture local history is unavailable".to_string()),
    );
    let replacement = history_rx.try_recv().expect("newer viewport rearms");
    assert_eq!(replacement.kind, HistoryRequestKind::ViewportBackfill);
    assert_ne!(replacement.range, Some(first));
    // The demanded span now reaches the newest viewport; the pages that fill
    // it walk back from the live edge so each one lands adjacent to what the
    // series already holds.
    assert!(
        coordinator
            .viewport_history_ranges
            .get(&(series.clone(), provider_generation))
            .is_some_and(|range| range.start_unix_nanos <= newest.start_unix_nanos)
    );
}

/// A backfill that does not reach the live edge must not be spliced onto it.
///
/// The retained window used to be the visible page plus a reserved live
/// tail, concatenated with a hole between them and renumbered contiguously.
/// The engine and the desktop both read that as one unbroken series, so the
/// hole was invisible until the live aggregator's next bar no longer
/// continued the renumbered sequence and the whole feed stopped.
#[test]
fn viewport_history_merge_keeps_only_the_run_that_reaches_the_live_edge() {
    let series = internal_series(&btc()).expect("series");
    let disjoint_backfill = coinbase_history(1, 1_000);
    let current = SeriesSnapshot {
        series,
        provider_generation: ProviderGeneration(NonZeroU64::MIN),
        publication_generation: 1,
        price_scale: 2,
        quantity_scale: 8,
        forming: true,
        bars: coinbase_history(100_000, 1_000).into(),
    };

    let merged = reconcile_history_repair(
        &current,
        disjoint_backfill,
        HISTORY_BARS_PER_SERIES,
        Some(CoinbaseInterval::Minute1),
        HistoryPrecedence::Repair,
    )
    .expect("merge");

    assert_eq!(merged.len(), 1_000);
    assert_eq!(merged[0].exchange_timestamp_seconds, 100_000 * 60);
    assert_eq!(
        merged.last().map(|bar| bar.exchange_timestamp_seconds),
        Some(100_999 * 60)
    );
    assert!(merged.windows(2).all(|pair| {
        pair[0].source_sequence + 1 == pair[1].source_sequence
            && pair[0].exchange_timestamp_seconds + 60 == pair[1].exchange_timestamp_seconds
    }));
}

/// An adjoining backfill extends the same run, and every bar keeps the
/// identity it already had.
#[test]
fn adjoining_viewport_backfill_extends_the_run_without_renumbering() {
    let series = internal_series(&btc()).expect("series");
    let current_bars = coinbase_history(100_000, 500);
    let live_edge = *current_bars.last().expect("live edge");
    let current = SeriesSnapshot {
        series,
        provider_generation: ProviderGeneration(NonZeroU64::MIN),
        publication_generation: 1,
        price_scale: 2,
        quantity_scale: 8,
        forming: true,
        bars: current_bars.into(),
    };

    let merged = reconcile_history_repair(
        &current,
        coinbase_history(99_500, 500),
        HISTORY_BARS_PER_SERIES,
        Some(CoinbaseInterval::Minute1),
        HistoryPrecedence::Repair,
    )
    .expect("merge");

    assert_eq!(merged.len(), 1_000);
    assert_eq!(merged[0].exchange_timestamp_seconds, 99_500 * 60);
    assert_eq!(merged.last().copied(), Some(live_edge));
    assert!(
        merged
            .windows(2)
            .all(|pair| { pair[0].source_sequence + 1 == pair[1].source_sequence })
    );
}

fn fail_viewport_history_request(
    coordinator: &mut Coordinator<'_>,
    series: &BarSeriesKey,
    generation: ProviderGeneration,
    request: &HistoryRequest,
) {
    coordinator.history_completed(
        series,
        generation,
        request.range,
        HistoryRequestKind::ViewportBackfill,
        Err("fixture provider failure".to_string()),
    );
}

fn assert_viewport_retry_waits(
    coordinator: &mut Coordinator<'_>,
    consumer_id: ConsumerId,
    generation: GenerationId,
    viewport: Viewport,
    storage_rx: &Receiver<StorageRequest>,
    history_rx: &Receiver<HistoryRequest>,
) {
    coordinator
        .request_viewport_history(consumer_id, generation, viewport)
        .expect("duplicate viewport is accepted");
    assert!(storage_rx.try_recv().is_err() && history_rx.try_recv().is_err());
    coordinator.resource_mode = ResourceMode::OfflineSuspended;
    coordinator.retry_viewport_history();
    assert!(history_rx.try_recv().is_err());
    coordinator.resource_mode = ResourceMode::Warm;
}

#[test]
fn viewport_history_failure_backs_off_exhausts_and_rearms() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let mut engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let generation = GenerationId(id(1).expect("generation"));
    let series = internal_series(&btc()).expect("series");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand(consumer_id, generation, &series)
        .expect("demand installs");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    let provider_generation = coordinator
        .provider_generation_for_series(&series)
        .expect("provider generation");
    let interval = 60_000_000_000_i64;
    let end = align_down(current_unix_nanos().expect("clock"), interval);
    let viewport = Viewport::try_new(end - 800 * interval, end - 700 * interval).expect("viewport");

    coordinator
        .request_viewport_history(consumer_id, generation, viewport)
        .expect("viewport queues local history");
    let StorageRequest::ReadRange(_, _, range) = storage_rx.recv().expect("local range request")
    else {
        panic!("viewport uses a range read");
    };
    coordinator.viewport_history_local_completed(
        &series,
        provider_generation,
        range,
        Err("fixture local history is unavailable".to_string()),
    );
    let mut request = history_rx.recv().expect("provider backfill request");

    for expected_attempt in 1..=MAXIMUM_VIEWPORT_HISTORY_RETRIES {
        fail_viewport_history_request(&mut coordinator, &series, provider_generation, &request);
        let key = (series.clone(), provider_generation);
        let retry = coordinator
            .viewport_history_retries
            .get_mut(&key)
            .expect("bounded retry remains armed");
        assert_eq!(retry.attempts, expected_attempt);
        assert!(history_rx.try_recv().is_err(), "failure must not hot-loop");
        retry.ready_at = Instant::now();
        if expected_attempt == 1 {
            assert_viewport_retry_waits(
                &mut coordinator,
                consumer_id,
                generation,
                viewport,
                &storage_rx,
                &history_rx,
            );
        }
        coordinator.retry_viewport_history();
        request = history_rx.recv().expect("bounded retry is queued");
    }

    fail_viewport_history_request(&mut coordinator, &series, provider_generation, &request);
    let key = (series.clone(), provider_generation);
    assert!(!coordinator.viewport_history_retries.contains_key(&key));
    assert!(!coordinator.viewport_history_ranges.contains_key(&key));
    assert!(
        history_rx.try_recv().is_err(),
        "exhaustion stops provider work"
    );
    assert!(matches!(
        coordinator.events[&consumer_id].series_state,
        Some(envelope::Payload::SeriesState(ref state))
            if state.state == SeriesLoadState::Partial as i32
                && state.detail.as_deref()
                    == Some("Visible history repair is unavailable; move the viewport to retry")
    ));

    coordinator
        .request_viewport_history(consumer_id, generation, viewport)
        .expect("same viewport rearms after exhaustion");
    assert!(matches!(
        storage_rx.recv().expect("rearmed local range request"),
        StorageRequest::ReadRange(_, _, _)
    ));
    assert!(!coordinator.viewport_history_retries.contains_key(&key));
    assert!(coordinator.viewport_history_ranges.contains_key(&key));
}

#[test]
fn coinbase_coverage_keeps_interior_missing_bars_repairable() {
    let series = internal_series(&btc()).expect("series");
    let mut bars = coinbase_history(1, 5);
    bars.remove(2);

    let ranges = coinbase_bar_coverage_ranges(&series, &bars).expect("coverage");

    assert_eq!(ranges.len(), 2);
    assert_eq!(ranges[0].start_unix_nanos, 60 * 1_000_000_000);
    assert_eq!(ranges[0].end_unix_nanos, 3 * 60 * 1_000_000_000);
    assert_eq!(ranges[1].start_unix_nanos, 4 * 60 * 1_000_000_000);
    assert_eq!(ranges[1].end_unix_nanos, 6 * 60 * 1_000_000_000);
    let coverage =
        CoverageSnapshot::try_new(ranges, Vec::new(), Vec::new(), Vec::new()).expect("snapshot");
    let requested = HistoryRange {
        start_unix_nanos: 60 * 1_000_000_000,
        end_unix_nanos: 6 * 60 * 1_000_000_000,
    };
    assert_eq!(
        coverage
            .plan(requested)
            .expect("repair plan")
            .repair_ranges(),
        &[HistoryRange {
            start_unix_nanos: 3 * 60 * 1_000_000_000,
            end_unix_nanos: 4 * 60 * 1_000_000_000,
        }]
    );
}

#[test]
fn coinbase_covering_repair_fills_holes_and_preserves_current_overlap() {
    let series = internal_series(&btc()).expect("series");
    let mut current_bars = coinbase_history(1, 5);
    current_bars.remove(2);
    let current = SeriesSnapshot {
        series: series.clone(),
        provider_generation: ProviderGeneration(NonZeroU64::MIN),
        publication_generation: 1,
        price_scale: 2,
        quantity_scale: 8,
        forming: true,
        bars: current_bars.into(),
    };
    let mut repair = coinbase_history(3, 1);
    repair[0].close = 103;
    let mut overlap = coinbase_history(1, 1);
    overlap[0].close = 102;
    repair.extend(overlap);

    let merged = reconcile_history_repair(
        &current,
        repair,
        HISTORY_BARS_PER_SERIES,
        Some(coinbase_series_interval(&series).expect("interval")),
        HistoryPrecedence::Current,
    )
    .expect("repair merges");

    assert_eq!(merged.len(), 5);
    // The repair fills the hole it was fetched for and never overwrites a
    // bar the series already published.
    assert_eq!(merged[0].close, 105);
    assert_eq!(merged[2].close, 103);
    assert!(
        merged.windows(2).all(|pair| {
            pair[0].source_sequence.checked_add(1) == Some(pair[1].source_sequence)
        })
    );
}

#[test]
fn coinbase_live_edge_repair_replaces_closed_bar_and_keeps_forming_bar() {
    let series = internal_series(&btc()).expect("series");
    let mut current_bars = coinbase_history(1, 3);
    let forming = current_bars[2];
    current_bars[1].open = 150;
    current_bars[1].high = 160;
    current_bars[1].close = 100;
    let current = SeriesSnapshot {
        series: series.clone(),
        provider_generation: ProviderGeneration(NonZeroU64::MIN),
        publication_generation: 1,
        price_scale: 2,
        quantity_scale: 8,
        forming: true,
        bars: current_bars.into(),
    };
    let mut authoritative = coinbase_history(2, 1);
    authoritative[0].open = 100;
    authoritative[0].high = 160;
    authoritative[0].close = 150;

    let merged = reconcile_history_repair(
        &current,
        authoritative.clone(),
        HISTORY_BARS_PER_SERIES,
        Some(coinbase_series_interval(&series).expect("interval")),
        HistoryPrecedence::Repair,
    )
    .expect("live edge repairs");

    assert_eq!(merged[1], authoritative[0]);
    assert_eq!(merged[2], forming);
}

const fn align_down(value: i64, interval: i64) -> i64 {
    value - value.rem_euclid(interval)
}

pub(crate) fn trade(minute: i64, price: &str, provider_sequence: u64) -> CanonicalTrade {
    trade_for("BTC-USD", minute, price, provider_sequence)
}

fn trade_for(product_id: &str, minute: i64, price: &str, provider_sequence: u64) -> CanonicalTrade {
    CanonicalTrade {
        product_id: product_id.to_string(),
        trade_id: format!("fixture-{provider_sequence}"),
        price: FixedPointValue::parse(price).expect("price parses"),
        size: FixedPointValue::parse("0.00000001").expect("size parses"),
        maker_side_buy: true,
        trade_time_unix_nanos: minute * 60_000_000_000 + 1,
        provider_timestamp_unix_nanos: minute * 60_000_000_000 + 2,
        sequence_num: provider_sequence,
        canonical_sequence: provider_sequence,
    }
}

/// Folds the consumer's event stream back into a bar series.
///
/// This is the contract the desktop's replay model implements: a snapshot
/// replaces the series, and every update appends one bar or replaces the
/// forming one. Reconstructing the series this way fails loudly if the
/// stream ever skips a sequence, which a latest-value mailbox did silently.
fn collect_series_until(
    service: &MarketService,
    client_id: u64,
    consumer_id: u64,
    mut done: impl FnMut(&BTreeMap<u64, IpcMarketBar>) -> bool,
) -> BTreeMap<u64, IpcMarketBar> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut bars: BTreeMap<u64, IpcMarketBar> = BTreeMap::new();
    loop {
        match service
            .poll_event(client_id, consumer_id)
            .expect("market poll succeeds")
        {
            Some(envelope::Payload::SeriesSnapshot(snapshot)) => {
                bars = snapshot
                    .bars
                    .into_iter()
                    .map(|bar| (bar.source_sequence, bar))
                    .collect();
            }
            Some(envelope::Payload::SeriesUpdate(update)) => {
                if let Some(bar) = update.bar {
                    bars.insert(bar.source_sequence, bar);
                }
            }
            _ => {}
        }
        if done(&bars) {
            return bars;
        }
        assert!(
            Instant::now() < deadline,
            "series never completed; collected {bars:#?}"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn poll_until(
    service: &MarketService,
    client_id: u64,
    consumer_id: u64,
    mut accept: impl FnMut(&envelope::Payload) -> bool,
) -> envelope::Payload {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen: Vec<String> = Vec::new();
    loop {
        if let Some(event) = service
            .poll_event(client_id, consumer_id)
            .expect("market poll succeeds")
        {
            if accept(&event) {
                return event;
            }
            let rendered = format!("{event:?}");
            if !seen.contains(&rendered) {
                seen.push(rendered);
            }
        }
        assert!(
            Instant::now() < deadline,
            "market event timed out; distinct events seen: {seen:#?}"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn assert_persisted_bars(
    request: Result<StorageRequest, mpsc::TryRecvError>,
    series: &BarSeriesKey,
    generation: ProviderGeneration,
    local: MarketBar,
    repaired: MarketBar,
) {
    let reconciled = MarketBar {
        source_sequence: local.source_sequence + 1,
        ..repaired
    };
    assert!(matches!(
        request,
        Ok(StorageRequest::Persist(persisted, current, bars, false, _, _))
            if persisted == *series && current == generation && bars == [local, reconciled]
    ));
}

fn is_live_update(
    event: &envelope::Payload,
    generation: u64,
    provider_generation: u64,
    close: i64,
) -> bool {
    matches!(
        event,
        envelope::Payload::SeriesUpdate(update)
            if update.generation == generation
                && update.provider_generation == provider_generation
                && update.bar.as_ref().is_some_and(|bar| bar.close == close)
    ) || matches!(
        event,
        envelope::Payload::SeriesSnapshot(snapshot)
            if snapshot.generation == generation
                && snapshot.provider_generation == provider_generation
                && snapshot.bars.last().is_some_and(|bar| bar.close == close)
    )
}

fn attach_fixture_consumers(harness: &FixtureRealtimeHarness) {
    for client in 1..=2 {
        harness.service.attach(client).expect("client attaches");
        harness
            .service
            .register_consumer(client, 1, client)
            .expect("consumer registers");
        harness
            .service
            .set_demand(client, client, 1, &btc())
            .expect("history demand is accepted");
        let snapshot = poll_until(&harness.service, client, client, |event| {
            matches!(event, envelope::Payload::SeriesSnapshot(_))
        });
        assert!(matches!(
            snapshot,
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.provider_generation == 1 && !snapshot.forming
        ));
    }
}

fn expect_realtime_generation(
    harness: &FixtureRealtimeHarness,
    detail: &'static str,
    expected: u64,
) {
    assert_eq!(
        harness
            .generations
            .recv_timeout(Duration::from_secs(1))
            .expect(detail)
            .0
            .get(),
        expected
    );
}

fn expect_configured_products(
    harness: &FixtureRealtimeHarness,
    detail: &'static str,
    expected: &[&str],
) {
    assert_eq!(
        harness
            .configured_products
            .recv_timeout(Duration::from_secs(1))
            .expect(detail),
        expected
    );
}

fn phase_five_series() -> [SeriesKey; 8] {
    [
        selected_series("instrument:coinbase:btc:usd", 60),
        selected_series("instrument:coinbase:btc:usd", 300),
        selected_series("instrument:coinbase:btc:usd", 900),
        selected_series("instrument:coinbase:btc:usd", 3_600),
        selected_series("instrument:coinbase:eth:usd", 60),
        selected_series("instrument:coinbase:eth:usd", 300),
        selected_series("instrument:coinbase:eth:usd", 900),
        selected_series("instrument:coinbase:eth:usd", 3_600),
    ]
}

fn attach_twenty_chart_consumers(service: &MarketService, client_id: u64, series: &[SeriesKey; 8]) {
    service.attach(client_id).expect("client attaches");
    for consumer_id in 1..=20 {
        let workspace_id = (consumer_id - 1) / 4 + 1;
        service
            .register_consumer(client_id, workspace_id, consumer_id)
            .expect("chart consumer registers");
        service
            .set_demand(
                client_id,
                consumer_id,
                1,
                &series[usize::try_from((consumer_id - 1) % 8).expect("series index")],
            )
            .expect("chart demand is accepted");
    }
}

fn assert_initial_chart_snapshots(
    service: &MarketService,
    client_id: u64,
    series: &[SeriesKey; 8],
) {
    for consumer_id in 1..=20 {
        let expected = &series[usize::try_from((consumer_id - 1) % 8).expect("series index")];
        assert!(matches!(
            poll_until(service, client_id, consumer_id, |event| matches!(
                event,
                envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 1
            )),
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.consumer_id == consumer_id
                    && snapshot.series.as_ref() == Some(expected)
        ));
    }
}

#[test]
fn twenty_chart_consumers_share_one_provider_and_remain_independent() {
    let harness = MarketService::start_fixture_realtime(vec![history_bar()])
        .expect("realtime fixture starts");
    let client_id = 1;
    let series = phase_five_series();
    attach_twenty_chart_consumers(&harness.service, client_id, &series);
    assert_initial_chart_snapshots(&harness.service, client_id, &series);
    assert_eq!(
        harness.history_fetches.load(Ordering::Acquire),
        series.len()
    );
    assert_eq!(
        harness
            .generations
            .recv_timeout(Duration::from_secs(1))
            .expect("one shared realtime generation starts")
            .0
            .get(),
        1
    );
    assert_eq!(
        harness
            .configured_products
            .recv_timeout(Duration::from_secs(1))
            .expect("initial product set configures"),
        ["BTC-USD"]
    );
    assert!(
        harness
            .generations
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "product-set changes reuse the provider generation"
    );
    assert_eq!(
        harness
            .configured_products
            .recv_timeout(Duration::from_secs(1))
            .expect("replacement product set configures"),
        ["BTC-USD", "ETH-USD"]
    );
    harness
        .actions
        .send(FixtureRealtimeAction::Connected)
        .expect("connect fixture");

    harness
        .service
        .set_demand(client_id, 1, 2, &series[7])
        .expect("one chart switches series");
    assert!(matches!(
        poll_until(&harness.service, client_id, 1, |event| matches!(
            event,
            envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2
        )),
        envelope::Payload::SeriesSnapshot(snapshot)
            if snapshot.generation == 2 && snapshot.series.as_ref() == Some(&series[7])
    ));
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(2, "2.00", 1)))
        .expect("shared BTC trade");
    let live = poll_until(&harness.service, client_id, 9, |event| {
        is_live_update(event, 1, 1, 200)
    });
    assert!(is_live_update(&live, 1, 1, 200));

    harness
        .service
        .remove_consumer(client_id, 1)
        .expect("switched chart closes");
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(2, "2.10", 2)))
        .expect("remaining chart trade");
    let live = poll_until(&harness.service, client_id, 9, |event| {
        is_live_update(event, 1, 1, 210)
    });
    assert!(is_live_update(&live, 1, 1, 210));
    assert!(harness.service.poll_event(client_id, 1).is_err());
    assert_eq!(
        harness.history_fetches.load(Ordering::Acquire),
        series.len()
    );
    assert!(matches!(
        harness.generations.try_recv(),
        Err(TryRecvError::Empty)
    ));
}

#[test]
fn split_pane_attaches_to_an_already_live_series_without_staying_at_ready() {
    let harness = MarketService::start_fixture_realtime(vec![history_bar()])
        .expect("realtime fixture starts");
    harness.service.attach(1).expect("client attaches");
    harness
        .service
        .register_consumer(1, 1, 1)
        .expect("primary pane registers");
    harness
        .service
        .set_demand(1, 1, 1, &btc())
        .expect("primary demand starts");
    poll_until(&harness.service, 1, 1, |event| {
        matches!(event, envelope::Payload::SeriesSnapshot(_))
    });
    expect_realtime_generation(&harness, "shared realtime starts", 1);
    harness
        .actions
        .send(FixtureRealtimeAction::Connected)
        .expect("shared realtime connects");
    poll_until(&harness.service, 1, 1, |event| {
        matches!(event, envelope::Payload::SeriesState(state)
                if state.state == SeriesLoadState::Live as i32)
    });

    harness
        .service
        .register_consumer(1, 1, 2)
        .expect("split pane registers");
    harness
        .service
        .set_demand(1, 2, 1, &btc())
        .expect("split pane reuses live demand");
    assert!(matches!(
        poll_until(&harness.service, 1, 2, |event| matches!(
            event,
            envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 1
        )),
        envelope::Payload::SeriesSnapshot(snapshot) if snapshot.consumer_id == 2
    ));
    assert!(matches!(
        poll_until(&harness.service, 1, 2, |event| matches!(
            event,
            envelope::Payload::SeriesState(state) if state.state == SeriesLoadState::Live as i32
        )),
        envelope::Payload::SeriesState(state) if state.consumer_id == 2
    ));
    assert_eq!(harness.history_fetches.load(Ordering::Acquire), 1);

    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(2, "2.00", 1)))
        .expect("shared trade streams");
    for consumer_id in 1..=2 {
        let update = poll_until(&harness.service, 1, consumer_id, |event| {
            is_live_update(event, 1, 1, 200)
        });
        assert!(is_live_update(&update, 1, 1, 200));
    }
}

#[test]
fn split_pane_reuses_a_provisional_calendar_series_without_staying_loading() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(false));
    let mut protocol_series = btc();
    protocol_series.cadence = SeriesCadence::CalendarMonths as i32;
    protocol_series.cadence_value = 1;
    let series = internal_series(&protocol_series).expect("monthly series");
    let provider_generation = ProviderGeneration(NonZeroU64::MIN);
    let first = ConsumerId(id(1).expect("first consumer"));
    let second = ConsumerId(id(2).expect("second consumer"));
    let generation = GenerationId(NonZeroU64::MIN);
    let mut engine = configured_engine().expect("engine configures");
    for consumer_id in [first, second] {
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(consumer_id.0),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
    }
    engine
        .set_series_demand(first, generation, &series)
        .expect("first demand installs");
    engine
        .install_history(provider_generation, &series, 2, 8, vec![history_bar()])
        .expect("shared history installs");
    let publication = engine
        .set_series_demand(second, generation, &series)
        .expect("second demand installs")
        .expect("second demand reuses the shared snapshot");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        first,
        &series,
    );
    coordinator.events.insert(second, ConsumerEvents::default());
    let mut live =
        LiveHandoff::try_new(&series, provider_generation, &coinbase_instrument(&series))
            .expect("live handoff");
    live.connected = true;
    live.history = CoinbaseHistoryReadiness::Provisional;
    coordinator.live.insert(series.clone(), live);

    coordinator
        .publish_cached_demand(
            &series,
            provider_generation,
            &DemandWaiter {
                consumer_id: second,
                generation,
                started_at: Instant::now(),
            },
            &publication,
        )
        .expect("shared provisional demand publishes");

    assert!(matches!(
        coordinator.events.get_mut(&second).and_then(ConsumerEvents::pop),
        Some(envelope::Payload::SeriesSnapshot(snapshot)) if snapshot.consumer_id == 2
    ));
    assert!(matches!(
        coordinator.events.get_mut(&second).and_then(ConsumerEvents::pop),
        Some(envelope::Payload::SeriesState(state))
            if state.state == SeriesLoadState::Partial as i32
                && state.detail.as_deref().is_some_and(|detail| detail.contains("provisional"))
    ));
    assert!(history_rx.try_recv().is_err());
}

#[test]
fn initial_coinbase_windows_fit_one_source_page_for_every_cadence() {
    let cases = [
        (SeriesCadence::FixedSeconds, 60, 349),
        (SeriesCadence::FixedSeconds, 180, 115),
        (SeriesCadence::FixedSeconds, 86_400, 349),
        (SeriesCadence::CalendarWeeks, 1, 49),
        (SeriesCadence::CalendarMonths, 1, 10),
    ];
    for (cadence, cadence_value, expected_bars) in cases {
        let mut protocol_series = btc();
        protocol_series.cadence = cadence as i32;
        protocol_series.cadence_value = cadence_value;
        let series = internal_series(&protocol_series).expect("supported series");
        let bars = initial_coinbase_history_bars(&series, 350).expect("initial bound");
        assert_eq!(bars, expected_bars);
        let interval = coinbase_series_interval(&series).expect("Coinbase interval");
        let range = recent_coinbase_history_range(&series, bars).expect("recent range");
        let source_nanos = interval.source().1 * 1_000_000_000;
        assert!(
            range.end_unix_nanos - range.start_unix_nanos
                <= i64::try_from(COINBASE_HISTORY_PAGE_SOURCE_BARS).expect("page bound fits")
                    * source_nanos,
            "{interval:?} initial range exceeded one provider page"
        );
    }
}

#[test]
fn workspace_tab_and_layout_changes_keep_one_live_provider_session() {
    let harness = MarketService::start_fixture_realtime(vec![history_bar()])
        .expect("realtime fixture starts");
    attach_fixture_consumers(&harness);
    expect_realtime_generation(&harness, "shared realtime starts", 1);
    expect_configured_products(&harness, "shared product configures", &["BTC-USD"]);
    harness
        .actions
        .send(FixtureRealtimeAction::Connected)
        .expect("shared realtime connects");
    for consumer_id in 1..=2 {
        poll_until(&harness.service, consumer_id, consumer_id, |event| {
            matches!(
                event,
                envelope::Payload::ProviderState(state)
                    if state.state == ProviderConnectionState::Online as i32
                        && state.generation == 1
            )
        });
        while harness
            .service
            .poll_event(consumer_id, consumer_id)
            .expect("consumer remains registered")
            .is_some()
        {}
    }

    harness
        .service
        .set_resource_class(1, 1, ConsumerResourceClass::Background)
        .expect("inactive workspace moves to background");
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(2, "2.00", 1)))
        .expect("shared trade streams");
    let foreground = poll_until(&harness.service, 2, 2, |event| {
        is_live_update(event, 1, 1, 200)
    });
    assert!(is_live_update(&foreground, 1, 1, 200));

    harness
        .service
        .set_resource_class(1, 1, ConsumerResourceClass::Foreground)
        .expect("returning workspace moves to foreground");
    assert!(matches!(
        poll_until(&harness.service, 1, 1, |event| matches!(
            event,
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.generation == 1 && snapshot.provider_generation == 1
        )),
        envelope::Payload::SeriesSnapshot(snapshot)
            if snapshot.bars.last().is_some_and(|bar| bar.close == 200)
    ));

    harness
        .service
        .set_resource_class(2, 2, ConsumerResourceClass::Detached)
        .expect("closing pane detaches its market work");
    harness
        .service
        .remove_consumer(2, 2)
        .expect("closing pane removes its consumer");
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(2, "2.10", 2)))
        .expect("remaining workspace keeps streaming");
    let remaining = poll_until(&harness.service, 1, 1, |event| {
        is_live_update(event, 1, 1, 210)
    });
    assert!(is_live_update(&remaining, 1, 1, 210));
    assert!(harness.service.poll_event(2, 2).is_err());
    assert!(matches!(
        harness.generations.try_recv(),
        Err(TryRecvError::Empty)
    ));
    assert!(matches!(harness.stops.try_recv(), Err(TryRecvError::Empty)));
}

#[test]
fn realtime_capacity_rejects_a_new_product_but_allows_an_exact_replacement() {
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let btc_series = internal_series(&btc()).expect("BTC series");
    let mut engine = configured_engine().expect("engine configures");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand(
            consumer_id,
            GenerationId(id(1).expect("generation")),
            &btc_series,
        )
        .expect("BTC demand installs");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &btc_series,
    );
    let generation = ProviderGeneration(NonZeroU64::MIN);
    let btc_instrument = coinbase_instrument(&btc_series);
    coordinator.live.insert(
        btc_series.clone(),
        LiveHandoff::try_new(&btc_series, generation, &btc_instrument).expect("BTC live handoff"),
    );
    coordinator
        .realtime_products
        .insert(btc_instrument.provider_symbol);

    for index in 1..axiusflow_coinbase_market_adapter::MAXIMUM_PRODUCTS {
        let (series, instrument) = synthetic_coinbase_product(index);
        coordinator.catalog.insert(
            ("coinbase".to_string(), instrument.instrument_id.clone()),
            instrument.clone(),
        );
        coordinator.live.insert(
            series.clone(),
            LiveHandoff::try_new(&series, generation, &instrument).expect("live handoff"),
        );
        coordinator
            .realtime_products
            .insert(instrument.provider_symbol);
    }
    let (candidate, candidate_instrument) = synthetic_coinbase_product(64);
    coordinator.catalog.insert(
        ("coinbase".to_string(), candidate.instrument_id.clone()),
        candidate_instrument,
    );
    let live_before = coordinator.live.len();
    let products_before = coordinator.realtime_products.clone();

    assert_eq!(
        coordinator
            .validate_coinbase_realtime_capacity(
                &candidate,
                ConsumerId(id(2).expect("other consumer")),
            )
            .expect_err("sixty-fifth product is rejected"),
        "Coinbase realtime product capacity is exhausted"
    );
    assert_eq!(coordinator.live.len(), live_before);
    assert_eq!(coordinator.realtime_products, products_before);
    coordinator
        .validate_coinbase_realtime_capacity(&candidate, consumer_id)
        .expect("replacing the sole BTC reference remains at capacity");
}

#[test]
fn shared_realtime_stops_at_last_market_reference_with_idle_consumer() {
    let harness = MarketService::start_fixture_realtime(vec![history_bar()])
        .expect("realtime fixture starts");
    attach_fixture_consumers(&harness);
    harness
        .service
        .register_consumer(2, 1, 3)
        .expect("unrelated idle consumer registers");
    assert_eq!(harness.history_fetches.load(Ordering::Acquire), 1);
    assert_eq!(
        harness
            .generations
            .recv_timeout(Duration::from_secs(1))
            .expect("one shared realtime generation starts")
            .0
            .get(),
        1
    );
    assert_eq!(
        harness
            .configured_products
            .recv_timeout(Duration::from_secs(1))
            .expect("BTC product set configures"),
        ["BTC-USD"]
    );
    harness
        .actions
        .send(FixtureRealtimeAction::Connected)
        .expect("shared realtime connects");
    assert!(matches!(
        poll_until(&harness.service, 1, 1, |event| matches!(
            event,
            envelope::Payload::ProviderState(state)
                if state.state == ProviderConnectionState::Online as i32
        )),
        envelope::Payload::ProviderState(state) if state.generation == 1
    ));

    harness
        .service
        .remove_consumer(1, 1)
        .expect("first market reference closes");
    assert!(
        harness
            .stops
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "the remaining market reference retains realtime"
    );
    harness
        .service
        .remove_consumer(2, 2)
        .expect("last market reference closes");
    assert_eq!(
        harness
            .stops
            .recv_timeout(Duration::from_secs(1))
            .expect("last market reference releases realtime")
            .0
            .get(),
        1
    );

    harness
        .service
        .register_consumer(1, 1, 4)
        .expect("replacement consumer registers");
    harness
        .service
        .set_demand(1, 4, 1, &btc())
        .expect("the same product is demanded again");
    assert_eq!(
        harness
            .generations
            .recv_timeout(Duration::from_secs(1))
            .expect("released provider worker restarts")
            .0
            .get(),
        2
    );
    expect_configured_products(
        &harness,
        "the cleared applied product set configures again",
        &["BTC-USD"],
    );
}

#[test]
fn rithmic_upstream_starts_on_first_demand_and_stops_after_final_release() {
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let (rithmic_control_tx, rithmic_control_rx) = mpsc::sync_channel(2);
    let realtime_stop = Arc::new(AtomicBool::new(false));
    let generation = ProviderGeneration(id(7).expect("provider generation"));
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let series = rithmic_series_key("instrument:rithmic:CME:MNQU6", "rithmic-test:CME:MNQU6");
    let mut engine = configured_engine().expect("engine configures");
    engine
        .begin_provider_session("rithmic", generation)
        .expect("Rithmic session begins");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand_with_streams(
            consumer_id,
            GenerationId(id(1).expect("generation")),
            &series,
            chart_stream_requirements(&series),
        )
        .expect("Rithmic demand installs");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    coordinator
        .providers
        .set_rithmic_realtime_control(&rithmic_control_tx);
    coordinator.catalog.insert(
        (series.provider_id.clone(), series.instrument_id.clone()),
        provider_instrument(7, 1),
    );

    coordinator
        .ensure_realtime(&series)
        .expect("first demand starts Rithmic upstream");
    assert!(matches!(
        rithmic_control_rx.try_recv(),
        Ok(RithmicRealtimeControl::Select(_))
    ));
    assert!(coordinator.engine.remove_consumer(consumer_id));
    coordinator.release_unused_live_market_data();
    assert!(matches!(
        rithmic_control_rx.try_recv(),
        Ok(RithmicRealtimeControl::Stop)
    ));
    assert_eq!(
        coordinator
            .engine
            .provider_status("rithmic")
            .map(|status| status.health),
        Some(ProviderHealth::Disconnected)
    );
}

/// The live worker follows the instrument, not a one-shot flag.
///
/// Tracking "a selection was sent once" meant a symbol change never sent the
/// replacement `Select`: the worker stayed on the previous contract, so the
/// new chart received the old one's trades or nothing at all. Holding the
/// identity makes "the selection is stale" a question with an answer — and a
/// timeframe change, which keeps the instrument, must still send nothing.
#[test]
fn rithmic_instrument_change_selects_the_replacement_and_a_timeframe_change_does_not() {
    let (history_tx, _history_rx) = mpsc::sync_channel(4);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(4);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(4);
    let (rithmic_control_tx, rithmic_control_rx) = mpsc::sync_channel(4);
    let realtime_stop = Arc::new(AtomicBool::new(false));
    let generation = ProviderGeneration(id(7).expect("provider generation"));
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let first = rithmic_series_key("instrument:rithmic:CME:MNQU6", "rithmic-test:CME:MNQU6");
    let mut faster = first.clone();
    faster.period = BarPeriod::time(300).expect("five minute cadence");
    let second = rithmic_series_key("instrument:rithmic:CME:MESU6", "rithmic-test:CME:MESU6");

    let mut engine = configured_engine().expect("engine configures");
    engine
        .begin_provider_session("rithmic", generation)
        .expect("Rithmic session begins");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &first,
    );
    coordinator
        .providers
        .set_rithmic_realtime_control(&rithmic_control_tx);
    for series in [&first, &faster, &second] {
        let mut instrument = provider_instrument(7, 1);
        instrument.instrument_id = series.instrument_id.clone();
        instrument.entitlement_id = series.entitlement_id.clone();
        coordinator.catalog.insert(
            (series.provider_id.clone(), series.instrument_id.clone()),
            instrument,
        );
    }

    let mut demand = 1_u64;
    let mut select = |coordinator: &mut Coordinator<'_>, series: &BarSeriesKey| {
        demand += 1;
        coordinator
            .engine
            .set_series_demand_with_streams(
                consumer_id,
                GenerationId(id(demand).expect("generation")),
                series,
                chart_stream_requirements(series),
            )
            .expect("Rithmic demand installs");
        coordinator
            .ensure_realtime(series)
            .expect("Rithmic realtime is established");
    };

    select(&mut coordinator, &first);
    assert!(
        matches!(
            rithmic_control_rx.try_recv(),
            Ok(RithmicRealtimeControl::Select(instrument))
                if instrument.instrument_id == first.instrument_id
        ),
        "the first demand selects its instrument"
    );

    select(&mut coordinator, &faster);
    assert!(
        rithmic_control_rx.try_recv().is_err(),
        "a timeframe change keeps the same instrument and must not reselect"
    );

    select(&mut coordinator, &second);
    assert!(
        matches!(
            rithmic_control_rx.try_recv(),
            Ok(RithmicRealtimeControl::Select(instrument))
                if instrument.instrument_id == second.instrument_id
        ),
        "a contract change sends the worker its replacement selection"
    );
    assert_eq!(
        coordinator
            .rithmic_selection
            .as_ref()
            .map(|selection| selection.instrument_id.clone()),
        Some(second.instrument_id.clone())
    );
    // Both series still have a live handoff, so neither one's buffered trades
    // were thrown away by the switch, and the provider session was never torn
    // down for what is only a presentation change.
    assert!(coordinator.rithmic_live.contains_key(&second));
    assert_eq!(
        coordinator
            .engine
            .provider_status("rithmic")
            .and_then(|status| status.generation),
        Some(generation)
    );
}

#[test]
fn slow_consumer_conflates_live_state_without_blocking_control() {
    let harness = MarketService::start_fixture_realtime(vec![history_bar()])
        .expect("realtime fixture starts");
    attach_fixture_consumers(&harness);
    assert_eq!(
        harness
            .generations
            .recv_timeout(Duration::from_secs(1))
            .expect("shared realtime generation starts")
            .0
            .get(),
        1
    );
    harness
        .actions
        .send(FixtureRealtimeAction::Connected)
        .expect("connect fixture");
    assert!(matches!(
        poll_until(&harness.service, 2, 2, |event| matches!(
            event,
            envelope::Payload::ProviderState(state)
                if state.state == ProviderConnectionState::Online as i32
        )),
        envelope::Payload::ProviderState(state) if state.generation == 1
    ));

    for sequence in 1..=32 {
        let price = if sequence == 32 { "3.00" } else { "2.00" };
        harness
            .actions
            .send(FixtureRealtimeAction::Trade(trade(2, price, sequence)))
            .expect("live trade enters the bounded provider queue");
    }
    assert!(matches!(
        poll_until(&harness.service, 2, 2, |event| {
            is_live_update(event, 1, 1, 300)
        }),
        envelope::Payload::SeriesUpdate(update) if update.provider_generation == 1
    ));

    harness
        .service
        .set_visibility(2, 2, false)
        .expect("control command remains responsive");
    harness
        .service
        .set_demand(2, 2, 2, &btc())
        .expect("new demand remains responsive");
    harness
        .service
        .set_visibility(2, 2, true)
        .expect("foreground restoration remains responsive");
    assert!(matches!(
        poll_until(&harness.service, 2, 2, |event| {
            matches!(
                event,
                envelope::Payload::SeriesSnapshot(snapshot)
                    if snapshot.generation == 2
                        && snapshot.bars.last().is_some_and(|bar| bar.close == 300)
            )
        }),
        envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2
    ));

    let mut pending = 0;
    let mut latest_snapshot = false;
    while let Some(event) = harness
        .service
        .poll_event(1, 1)
        .expect("slow consumer polls after the burst")
    {
        pending += 1;
        assert!(pending <= 7, "consumer publication state remains bounded");
        latest_snapshot |= match event {
            envelope::Payload::SeriesSnapshot(snapshot) => {
                snapshot.bars.last().is_some_and(|bar| bar.close == 300)
            }
            envelope::Payload::SeriesUpdate(update) => {
                update.forming && update.bar.as_ref().is_some_and(|bar| bar.close == 300)
            }
            _ => false,
        };
    }
    assert!(
        latest_snapshot,
        "slow consumer receives the latest covering state"
    );
}

#[test]
fn storage_failure_degrades_persistence_without_hiding_provider_history() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let service = MarketService::start_with_sources(
        FixtureHistory {
            bars: vec![history_bar()],
            fetches: Some(Arc::clone(&fetches)),
        },
        None,
        Some(Err("fixture storage failure".to_string())),
    )
    .expect("service starts with degraded storage");
    service.attach(1).expect("client attaches");
    service
        .register_consumer(1, 1, 1)
        .expect("consumer registers");
    service
        .set_demand(1, 1, 1, &btc())
        .expect("demand remains usable");
    assert!(matches!(
        poll_until(&service, 1, 1, |event| matches!(
            event,
            envelope::Payload::SeriesSnapshot(_)
        )),
        envelope::Payload::SeriesSnapshot(snapshot) if snapshot.bars.len() == 1
    ));
    assert!(matches!(
        poll_until(&service, 1, 1, |event| matches!(
            event,
            envelope::Payload::SeriesState(state)
                if state.persistence == PersistenceState::Degraded as i32
        )),
        envelope::Payload::SeriesState(state)
            if state.state == SeriesLoadState::Ready as i32
                && state.persistence == PersistenceState::Degraded as i32
    ));
    assert!(matches!(
        poll_until(&service, 1, 1, |event| matches!(
            event,
            envelope::Payload::DemandError(_)
        )),
        envelope::Payload::DemandError(error)
            if error.stage_code == FailureStage::FilesystemWrite as i32
                && error.stage == "filesystem_write"
                && error.series == Some(btc())
                && error.elapsed_millis.is_some()
    ));
    service
        .set_demand(1, 1, 2, &btc())
        .expect("degraded persistence does not invalidate memory");
    assert!(matches!(
        poll_until(&service, 1, 1, |event| matches!(
            event,
            envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2
        )),
        envelope::Payload::SeriesSnapshot(snapshot)
            if snapshot.generation == 2 && snapshot.bars[0].close == 105
    ));
    assert_eq!(
        fetches.load(Ordering::Acquire),
        1,
        "usable in-memory history avoids another provider request"
    );
}

#[test]
fn storage_degradation_preserves_provider_and_live_progress() {
    let harness = MarketService::start_fixture_realtime_with_storage(
        vec![history_bar()],
        Some(Err("fixture storage failure".to_string())),
    )
    .expect("realtime service starts with degraded storage");
    harness.service.attach(1).expect("client attaches");
    harness
        .service
        .register_consumer(1, 1, 1)
        .expect("consumer registers");
    harness
        .service
        .set_demand(1, 1, 1, &btc())
        .expect("demand starts independently of storage");
    assert_eq!(
        harness
            .generations
            .recv_timeout(Duration::from_secs(1))
            .expect("realtime generation starts")
            .0
            .get(),
        1
    );
    assert!(matches!(
        poll_until(&harness.service, 1, 1, |event| matches!(
            event,
            envelope::Payload::ProviderState(state)
                if state.state == ProviderConnectionState::Connecting as i32
        )),
        envelope::Payload::ProviderState(state) if state.generation == 1
    ));
    assert!(matches!(
        poll_until(&harness.service, 1, 1, |event| matches!(
            event,
            envelope::Payload::SeriesState(state)
                if state.state == SeriesLoadState::Ready as i32
                    && state.persistence == PersistenceState::Degraded as i32
        )),
        envelope::Payload::SeriesState(state)
            if state.state == SeriesLoadState::Ready as i32
                && state.generation == 1
    ));

    harness
        .actions
        .send(FixtureRealtimeAction::Connected)
        .expect("provider connects after storage degradation");
    assert!(matches!(
        poll_until(&harness.service, 1, 1, |event| matches!(
            event,
            envelope::Payload::ProviderState(state)
                if state.state == ProviderConnectionState::Online as i32
        )),
        envelope::Payload::ProviderState(state) if state.generation == 1
    ));
    assert!(matches!(
        poll_until(&harness.service, 1, 1, |event| matches!(
            event,
            envelope::Payload::SeriesState(state)
                if state.state == SeriesLoadState::Live as i32
        )),
        envelope::Payload::SeriesState(state)
            if state.persistence == PersistenceState::NotRequested as i32
                && state.generation == 1
    ));
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(2, "2.00", 1)))
        .expect("live trade follows storage degradation");
    assert!(matches!(
        poll_until(&harness.service, 1, 1, |event| {
            is_live_update(event, 1, 1, 200)
        }),
        envelope::Payload::SeriesUpdate(update)
            if update.provider_generation == 1 && update.generation == 1
    ));
}

#[test]
fn stalled_local_history_degrades_and_starts_provider_repair() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let mut engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    let series = internal_series(&btc()).expect("series");
    let generation = ProviderGeneration(NonZeroU64::MIN);
    engine
        .set_series_demand(
            consumer_id,
            GenerationId(id(1).expect("generation")),
            &series,
        )
        .expect("demand installs");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    coordinator
        .enqueue_local_history(&series, generation)
        .expect("local history read starts");
    assert!(matches!(
        storage_rx.try_recv(),
        Ok(StorageRequest::Read(ref requested, current))
            if requested == &series && current == generation
    ));
    coordinator
        .local_history_deadlines
        .insert((series.clone(), generation), Instant::now());
    coordinator.expire_local_history_reads();
    assert!(matches!(
        coordinator.events[&consumer_id].series_state,
        Some(envelope::Payload::SeriesState(ref state))
            if state.state == SeriesLoadState::Resolving as i32
                && state.persistence == PersistenceState::Degraded as i32
                && state.detail.as_deref()
                    == Some("Local history read timed out; provider repair continues")
    ));
    assert!(matches!(
        history_rx.try_recv(),
        Ok(HistoryRequest {
            ref series,
            provider_generation,
            ..
        }) if series == &internal_series(&btc()).expect("requested series")
            && provider_generation == generation
    ));

    coordinator.local_history_completed(
        &series,
        generation,
        Ok(Some(StoredHistory {
            bars: vec![history_bar()],
            derived: false,
            durable: true,
        })),
    );
    assert!(matches!(
        coordinator.events[&consumer_id].series_state,
        Some(envelope::Payload::SeriesState(ref state))
            if state.state == SeriesLoadState::Partial as i32
                && state.persistence == PersistenceState::Durable as i32
    ));
    assert!(coordinator.events[&consumer_id].queued_series().is_some());
}

#[test]
fn late_local_history_cannot_replace_completed_provider_repair() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let mut engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    let series = internal_series(&btc()).expect("series");
    let generation = ProviderGeneration(NonZeroU64::MIN);
    engine
        .set_series_demand(
            consumer_id,
            GenerationId(id(1).expect("generation")),
            &series,
        )
        .expect("demand installs");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    coordinator.history_completed(
        &series,
        generation,
        None,
        HistoryRequestKind::Initial,
        Ok(HistorySnapshot {
            price_scale: 2,
            quantity_scale: 8,
            bars: vec![history_bar()],
            forming: None,
            handoff_boundary_unix_nanos: None,
            confirmed_empty: false,
        }),
    );
    let late = MarketBar {
        close: 99,
        ..history_bar()
    };
    coordinator.local_history_completed(
        &series,
        generation,
        Ok(Some(StoredHistory {
            bars: vec![late],
            derived: false,
            durable: true,
        })),
    );
    assert!(matches!(
        coordinator.events[&consumer_id].queued_series(),
        Some(envelope::Payload::SeriesSnapshot(snapshot))
            if snapshot.bars[0].close == 105
    ));
    assert!(
        history_rx.try_recv().is_err(),
        "a late local completion must not schedule duplicate provider work"
    );
}

#[test]
fn retained_history_publishes_before_provider_repair_and_survives_its_failure() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let mut engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let series = internal_series(&btc()).expect("series");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand(
            consumer_id,
            GenerationId(id(1).expect("generation")),
            &series,
        )
        .expect("demand installs");
    let generation = ProviderGeneration(NonZeroU64::MIN);
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    let local = MarketBar {
        close: 99,
        ..history_bar()
    };
    coordinator.local_history_completed(
        &series,
        generation,
        Ok(Some(StoredHistory {
            bars: vec![local],
            derived: false,
            durable: true,
        })),
    );
    assert!(matches!(
        coordinator.events[&consumer_id].queued_series(),
        Some(envelope::Payload::SeriesSnapshot(snapshot))
            if snapshot.bars[0].close == 99
    ));
    assert!(history_rx.try_recv().is_ok());
    coordinator.history_completed(
        &series,
        generation,
        None,
        HistoryRequestKind::Initial,
        Err("provider unavailable".to_string()),
    );
    let retry = (series.clone(), generation);
    assert!(coordinator.history_retries.contains_key(&retry));
    coordinator.history_retries.get_mut(&retry).unwrap().0 = Instant::now();
    coordinator.retry_history();
    assert!(history_rx.try_recv().is_ok());
    assert!(coordinator.events[&consumer_id].queued_series().is_some());
    coordinator.history_completed(
        &series,
        generation,
        None,
        HistoryRequestKind::Initial,
        Ok(HistorySnapshot {
            price_scale: 2,
            quantity_scale: 8,
            bars: vec![history_bar()],
            forming: None,
            handoff_boundary_unix_nanos: None,
            confirmed_empty: false,
        }),
    );
    assert!(!coordinator.history_retries.contains_key(&retry));
    assert!(matches!(
        coordinator.events[&consumer_id].series_state,
        Some(envelope::Payload::SeriesState(ref state))
            if state.state == SeriesLoadState::Ready as i32
                && state.persistence == PersistenceState::Pending as i32
    ));
    assert!(matches!(
        coordinator.events[&consumer_id].queued_series(),
        Some(envelope::Payload::SeriesSnapshot(snapshot))
            if snapshot.bars[0].close == 105
    ));
    assert!(matches!(
        storage_rx.try_recv(),
        Ok(StorageRequest::Persist(ref persisted, current, ref bars, false, _, _))
            if persisted == &series && current == generation && bars == &[history_bar()]
    ));
}

#[test]
fn shorter_provider_repair_preserves_the_published_canonical_sequence() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let mut engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    let series = internal_series(&btc()).expect("series");
    engine
        .set_series_demand(
            consumer_id,
            GenerationId(id(1).expect("generation")),
            &series,
        )
        .expect("demand installs");
    let generation = ProviderGeneration(NonZeroU64::MIN);
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    coordinator.local_history_completed(
        &series,
        generation,
        Ok(Some(StoredHistory {
            bars: coinbase_history(1, 350),
            derived: false,
            durable: true,
        })),
    );
    history_rx.recv().expect("provider repair is queued");

    coordinator.history_completed(
        &series,
        generation,
        None,
        HistoryRequestKind::Initial,
        Ok(HistorySnapshot {
            price_scale: 2,
            quantity_scale: 8,
            bars: coinbase_history(102, 250),
            forming: None,
            handoff_boundary_unix_nanos: None,
            confirmed_empty: false,
        }),
    );

    let Some(envelope::Payload::SeriesSnapshot(snapshot)) =
        coordinator.events[&consumer_id].queued_series()
    else {
        panic!("covering repair publishes a snapshot");
    };
    // Retained and repaired bars merge on timestamp, and every bar keeps the
    // sequence its bucket gives it: minute 1 is 2, minute 351 is 352.
    assert_eq!(snapshot.bars.len(), 351);
    assert_eq!(snapshot.bars[0].source_sequence, 2);
    assert_eq!(snapshot.bars[0].exchange_timestamp_seconds, 60);
    assert_eq!(snapshot.bars[350].source_sequence, 352);
    assert_eq!(snapshot.bars[350].exchange_timestamp_seconds, 21_060);
    assert!(matches!(
        storage_rx.try_recv(),
        Ok(StorageRequest::Persist(_, _, ref bars, false, _, _))
            if bars.first().is_some_and(|bar| bar.source_sequence == 2)
                && bars.last().is_some_and(|bar| bar.source_sequence == 352)
    ));
}

#[test]
fn retained_rithmic_history_uses_installed_precision_and_is_persisted_after_repair() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(true));
    let mut engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let generation = ProviderGeneration(id(7).expect("provider generation"));
    engine
        .begin_provider_session("rithmic", generation)
        .expect("Rithmic generation begins");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    let series = BarSeriesKey {
        provider_id: "rithmic".to_string(),
        instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
        entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
        period: BarPeriod::tick(100).expect("tick cadence"),
        definition_version: 1,
    };
    engine
        .set_series_demand(
            consumer_id,
            GenerationId(id(1).expect("generation")),
            &series,
        )
        .expect("demand installs");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    let installed = provider_instrument(7, 1);
    coordinator.catalog.insert(
        (installed.provider.clone(), installed.instrument_id.clone()),
        installed,
    );
    let local = MarketBar {
        exchange_timestamp_unix_nanos: 60_123_456_789,
        close: 99,
        ..history_bar()
    };
    coordinator.local_history_completed(
        &series,
        generation,
        Ok(Some(StoredHistory {
            bars: vec![local],
            derived: false,
            durable: true,
        })),
    );
    assert!(matches!(
        coordinator.events[&consumer_id].queued_series(),
        Some(envelope::Payload::SeriesSnapshot(snapshot))
            if snapshot.provider_generation == 7
                && snapshot.price_scale == 2
                && snapshot.quantity_scale == 0
                && snapshot.bars[0].exchange_timestamp_unix_nanos == 60_123_456_789
    ));
    assert!(history_rx.try_recv().is_ok());

    let repaired = MarketBar {
        exchange_timestamp_seconds: 61,
        exchange_timestamp_unix_nanos: 61_987_654_321,
        ..history_bar()
    };
    coordinator.history_completed(
        &series,
        generation,
        None,
        HistoryRequestKind::Initial,
        Ok(HistorySnapshot {
            price_scale: 2,
            quantity_scale: 0,
            bars: vec![repaired],
            forming: None,
            handoff_boundary_unix_nanos: None,
            confirmed_empty: false,
        }),
    );
    assert_persisted_bars(storage_rx.try_recv(), &series, generation, local, repaired);
    assert!(matches!(
        coordinator.events[&consumer_id].series_state,
        Some(envelope::Payload::SeriesState(ref state))
            if state.state == SeriesLoadState::Ready as i32
                && state.persistence == PersistenceState::Pending as i32
    ));
}

#[test]
fn provider_state_and_live_readiness_are_scoped_to_matching_consumers() {
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(false));
    let mut engine = configured_engine().expect("engine configures");
    let rithmic_generation = ProviderGeneration(id(7).expect("Rithmic generation"));
    engine
        .begin_provider_session("rithmic", rithmic_generation)
        .expect("Rithmic session begins");
    let coinbase_consumer = ConsumerId(id(1).expect("Coinbase consumer"));
    let rithmic_consumer = ConsumerId(id(2).expect("Rithmic consumer"));
    let rithmic = rithmic_series_key("instrument:rithmic:CME:MNQU6", "rithmic-test:CME:MNQU6");
    for (consumer_id, series) in [
        (coinbase_consumer, internal_series(&btc()).expect("BTC")),
        (rithmic_consumer, rithmic.clone()),
    ] {
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(id(1).expect("client")),
                    workspace_id: WorkspaceId(id(1).expect("workspace")),
                    consumer_id,
                },
                true,
            )
            .expect("consumer registers");
        engine
            .set_series_demand(
                consumer_id,
                GenerationId(id(1).expect("generation")),
                &series,
            )
            .expect("series demand installs");
    }
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        coinbase_consumer,
        &internal_series(&btc()).expect("BTC series"),
    );
    coordinator
        .events
        .insert(rithmic_consumer, ConsumerEvents::default());

    coordinator.broadcast_provider(
        ProviderConnectionState::Recovering,
        ProviderGeneration(NonZeroU64::MIN),
        Some("Coinbase fixture recovery"),
    );
    coordinator.broadcast_series_state_for(
        &internal_series(&btc()).expect("BTC series"),
        SeriesLoadState::Live,
    );
    coordinator.broadcast_rithmic_provider(
        ProviderConnectionState::Online,
        rithmic_generation,
        None,
    );

    assert!(matches!(
        coordinator.events[&coinbase_consumer].provider,
        Some(envelope::Payload::ProviderState(ref state))
            if state.provider == "coinbase"
                && state.state == ProviderConnectionState::Recovering as i32
    ));
    assert!(matches!(
        coordinator.events[&coinbase_consumer].series_state,
        Some(envelope::Payload::SeriesState(ref state))
            if state.state == SeriesLoadState::Live as i32
                && state.series.as_ref().is_some_and(|series| series.provider == "coinbase")
    ));
    assert!(matches!(
        coordinator.events[&rithmic_consumer].provider,
        Some(envelope::Payload::ProviderState(ref state))
            if state.provider == "rithmic"
                && state.state == ProviderConnectionState::Online as i32
    ));
    assert!(
        coordinator.events[&rithmic_consumer].series_state.is_none(),
        "Coinbase readiness must not mutate a Rithmic series"
    );
}

#[test]
fn rithmic_reconnect_retains_covering_history_until_repair_replaces_it() {
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    let (storage_tx, storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(false));
    let mut engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let generation = ProviderGeneration(id(7).expect("provider generation"));
    let series = rithmic_series_key("instrument:rithmic:CME:MNQU6", "rithmic-test:CME:MNQU6");
    engine
        .begin_provider_session("rithmic", generation)
        .expect("Rithmic session begins");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand(
            consumer_id,
            GenerationId(id(1).expect("generation")),
            &series,
        )
        .expect("demand installs");
    let publication = engine
        .install_history(generation, &series, 2, 0, vec![history_bar()])
        .expect("history installs")
        .pop()
        .expect("consumer receives history");
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    publish_state(
        coordinator
            .events
            .get_mut(&consumer_id)
            .expect("consumer events"),
        &publication,
        SeriesLoadState::Live,
        PersistenceState::Durable,
        None,
    );
    let mut live = RithmicLiveHandoff::new(&series, generation, "CME").expect("live cadence");
    live.seed(2, 0, &[history_bar()], None, None)
        .expect("history seeds live handoff");
    live.connected = true;
    coordinator.rithmic_live.insert(series.clone(), live);

    coordinator.rithmic_connecting(8);

    assert!(matches!(
        coordinator.engine.series_snapshot(&series),
        Some(snapshot) if snapshot.provider_generation == generation
    ));
    assert!(matches!(
        coordinator.events[&consumer_id].queued_series(),
        Some(envelope::Payload::SeriesSnapshot(snapshot))
            if snapshot.provider_generation == generation.0.get()
    ));
    assert!(matches!(
        coordinator.events[&consumer_id].series_state,
        Some(envelope::Payload::SeriesState(ref state))
            if state.state == SeriesLoadState::Partial as i32
                && state.persistence == PersistenceState::Durable as i32
    ));
    assert!(matches!(
        storage_rx.try_recv(),
        Ok(StorageRequest::Read(ref requested, current))
            if requested == &series && current.0.get() == 8
    ));
}

fn assert_order_flow_snapshot(coordinator: &Coordinator<'_>, consumer_id: ConsumerId) {
    assert!(matches!(
        coordinator.events[&consumer_id].order_flow,
        Some(envelope::Payload::OrderFlowSnapshot(ref snapshot))
            if snapshot.consumer_id == consumer_id.0.get()
                && snapshot.generation == 1
                && snapshot.provider_generation == 7
                && snapshot.source_watermark == 1
                && snapshot.cumulative_delta == 2
                && snapshot.levels.len() == 1
                && snapshot.tape.len() == 1
    ));
}

fn assert_order_flow_update(coordinator: &Coordinator<'_>, consumer_id: ConsumerId) {
    assert!(matches!(
        coordinator.events[&consumer_id].order_flow,
        Some(envelope::Payload::OrderFlowUpdate(ref update))
            if update.consumer_id == consumer_id.0.get()
                && update.provider_generation == 7
                && update.publication_generation == 2
                && update.cumulative_delta == 4
                && update.trade.as_ref().is_some_and(|trade| trade.source_sequence == 2)
    ));
}

fn assert_rithmic_failure_is_scoped(
    coordinator: &Coordinator<'_>,
    affected: &BarSeriesKey,
    unaffected: &BarSeriesKey,
    affected_consumer: ConsumerId,
    unaffected_consumer: ConsumerId,
) {
    assert!(!coordinator.rithmic_live[affected].history_ready);
    assert!(coordinator.rithmic_live[unaffected].history_ready);
    assert_eq!(
        coordinator
            .engine
            .provider_status("rithmic")
            .map(|status| status.health),
        Some(ProviderHealth::Online),
        "instrument-local failure does not degrade the provider session"
    );
    assert!(matches!(
        coordinator.events[&affected_consumer].series_state,
        Some(envelope::Payload::SeriesState(ref state))
            if state.state == SeriesLoadState::Partial as i32
    ));
    assert!(matches!(
        coordinator.events[&unaffected_consumer].series_state,
        Some(envelope::Payload::SeriesState(ref state))
            if state.state == SeriesLoadState::Live as i32
    ));
}

#[test]
fn rithmic_instrument_failure_repairs_only_the_affected_series() {
    let (history_tx, history_rx) = mpsc::sync_channel(2);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(false));
    let mut engine = configured_engine().expect("engine configures");
    let generation = ProviderGeneration(id(7).expect("provider generation"));
    engine
        .begin_provider_session("rithmic", generation)
        .expect("Rithmic session begins");
    engine
        .set_provider_health("rithmic", generation, ProviderHealth::Online)
        .expect("Rithmic is online");
    let affected = rithmic_series_key("instrument:rithmic:CME:MNQU6", "rithmic-test:CME:MNQU6");
    let unaffected = rithmic_series_key("instrument:rithmic:CME:ESU6", "rithmic-test:CME:ESU6");
    let publications = [
        install_rithmic_test_series(&mut engine, 1, generation, &affected),
        install_rithmic_test_series(&mut engine, 2, generation, &unaffected),
    ];
    let affected_consumer = ConsumerId(id(1).expect("affected consumer"));
    let unaffected_consumer = ConsumerId(id(2).expect("unaffected consumer"));
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        affected_consumer,
        &affected,
    );
    coordinator
        .events
        .insert(unaffected_consumer, ConsumerEvents::default());
    for publication in &publications {
        publish_state(
            coordinator
                .events
                .get_mut(&publication.consumer_id)
                .expect("publication consumer exists"),
            publication,
            SeriesLoadState::Live,
            PersistenceState::Durable,
            None,
        );
    }
    for series in [&affected, &unaffected] {
        coordinator
            .rithmic_live
            .insert(series.clone(), seeded_rithmic_live(series, generation));
    }
    let first = rithmic_trade(1, 7, 121_000_000_000, 110);
    coordinator.rithmic_trade(7, &first);
    assert_order_flow_snapshot(&coordinator, affected_consumer);
    coordinator.publish_rithmic_live();
    coordinator
        .rithmic_live
        .get_mut(&affected)
        .expect("affected handoff")
        .bars
        .last_mut()
        .expect("forming bar")
        .volume = i64::MAX;

    coordinator.rithmic_trade(7, &rithmic_trade(2, 7, 125_000_000_000, 111));
    assert_order_flow_update(&coordinator, affected_consumer);
    assert_rithmic_failure_is_scoped(
        &coordinator,
        &affected,
        &unaffected,
        affected_consumer,
        unaffected_consumer,
    );
    assert!(matches!(
        history_rx.try_recv(),
        Ok(HistoryRequest { series, provider_generation, .. })
            if series == affected && provider_generation == generation
    ));
    assert!(matches!(history_rx.try_recv(), Err(TryRecvError::Empty)));
    complete_rithmic_test_history(&mut coordinator, &affected, generation);
    assert!(
        coordinator.rithmic_live[&affected].history_ready
            && coordinator.rithmic_live[&unaffected].history_ready
    );
}

#[test]
fn later_consumers_reuse_one_engine_history_fetch() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let service = MarketService::start_with_source(ControlledHistory {
        fetches: Arc::clone(&fetches),
        release: release_rx,
    })
    .expect("test service starts");
    for client in 1..=2 {
        service.attach(client).expect("client attaches");
        service
            .register_consumer(client, 1, client)
            .expect("consumer registers");
    }
    assert_eq!(
        service.attach(1).expect_err("duplicate client is rejected"),
        "client identity is already attached"
    );
    assert_eq!(
        service
            .set_visibility(1, 2, false)
            .expect_err("another client's consumer is rejected"),
        "consumer is not owned by the attached client"
    );
    service
        .set_demand(1, 1, 1, &btc())
        .expect("first demand is accepted without provider completion");
    while fetches.load(Ordering::Acquire) == 0 {
        thread::yield_now();
    }
    service
        .set_demand(2, 2, 1, &btc())
        .expect("matching demand joins the in-flight history request");
    release_tx.send(()).expect("history released");
    for (client, consumer) in [(1, 1), (2, 2)] {
        assert!(matches!(
            poll_until(&service, client, consumer, |event| matches!(
                event,
                envelope::Payload::SeriesSnapshot(_)
            )),
            envelope::Payload::SeriesSnapshot(snapshot) if snapshot.bars.len() == 5
        ));
    }
    service
        .set_demand(1, 1, 2, &btc())
        .expect("newer selection reuses cache");
    assert!(matches!(
        poll_until(&service, 1, 1, |event| matches!(
            event,
            envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2
        )),
        envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2
    ));
    service
        .set_viewport(1, 1, 1, 60, 120)
        .expect("retired viewport is a fenced no-op");
    assert_eq!(fetches.load(Ordering::Acquire), 1);
}

#[test]
fn timeframe_switch_waits_for_its_own_current_provider_history() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let (release_tx, release_rx) = mpsc::sync_channel(2);
    let service = MarketService::start_with_source(ControlledHistory {
        fetches: Arc::clone(&fetches),
        release: release_rx,
    })
    .expect("test service starts");
    service.attach(1).expect("client attaches");
    service
        .register_consumer(1, 1, 1)
        .expect("consumer registers");
    service
        .set_demand(1, 1, 1, &btc())
        .expect("minute demand starts");
    while fetches.load(Ordering::Acquire) == 0 {
        thread::yield_now();
    }
    release_tx.send(()).expect("minute history released");
    poll_until(
        &service,
        1,
        1,
        |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 1),
    );

    let five_minute = selected_series("instrument:coinbase:btc:usd", 300);
    service
        .set_demand(1, 1, 2, &five_minute)
        .expect("coarser demand starts");
    while fetches.load(Ordering::Acquire) < 2 {
        thread::yield_now();
    }
    while let Some(event) = service.poll_event(1, 1).expect("pending event polls") {
        assert!(
            !matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2),
            "another timeframe must not synthesize the new chart's first snapshot"
        );
    }
    release_tx.send(()).expect("provider repair released");
    poll_until(&service, 1, 1, |event| {
        matches!(event, envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.generation == 2
                    && snapshot.bars.len() == 5
                    && snapshot.bars[0].exchange_timestamp_seconds == 0
                    && snapshot.bars[1].exchange_timestamp_seconds == 300
                    && snapshot.bars[1].source_sequence == 2)
    });

    service
        .set_demand(1, 1, 3, &five_minute)
        .expect("repeated demand hits cache");
    assert!(matches!(
        poll_until(&service, 1, 1, |event| matches!(
            event,
            envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 3
        )),
        envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 3
    ));
    assert_eq!(fetches.load(Ordering::Acquire), 2);

    service
        .set_demand(1, 1, 4, &btc())
        .expect("dormant minute demand starts");
    while fetches.load(Ordering::Acquire) < 3 {
        thread::yield_now();
    }
    while let Some(event) = service.poll_event(1, 1).expect("pending event polls") {
        assert!(
            !matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 4),
            "a dormant cached timeframe must refresh before it becomes visible"
        );
    }
    release_tx
        .send(())
        .expect("dormant provider repair released");
    assert!(matches!(
        poll_until(&service, 1, 1, |event| matches!(
            event,
            envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 4
        )),
        envelope::Payload::SeriesSnapshot(snapshot)
            if snapshot.series == Some(btc()) && snapshot.generation == 4
    ));
}

#[test]
fn finer_history_never_derives_from_a_coarser_cached_series() {
    let (requested_tx, requested_rx) = mpsc::sync_channel(2);
    let (release_tx, release_rx) = mpsc::sync_channel(2);
    let service = MarketService::start_with_source(SwitchingHistory {
        requested: requested_tx,
        release: release_rx,
    })
    .expect("directional history service starts");
    service.attach(1).expect("client attaches");
    service
        .register_consumer(1, 1, 1)
        .expect("consumer registers");
    let five_minute = selected_series("instrument:coinbase:btc:usd", 300);
    service
        .set_demand(1, 1, 1, &five_minute)
        .expect("coarse history demand starts");
    assert_eq!(
        requested_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("coarse provider request starts"),
        internal_series(&five_minute).expect("coarse series")
    );
    release_tx.send(()).expect("coarse history completes");
    poll_until(
        &service,
        1,
        1,
        |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 1),
    );

    service
        .set_demand(1, 1, 2, &btc())
        .expect("finer history demand starts");
    assert_eq!(
        requested_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("finer demand reaches the provider"),
        internal_series(&btc()).expect("finer series")
    );
    while let Some(event) = service.poll_event(1, 1).expect("pending event polls") {
        assert!(
            !matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2),
            "coarser cached data must not synthesize a finer snapshot"
        );
    }
    release_tx.send(()).expect("finer history completes");
    assert!(matches!(
        poll_until(&service, 1, 1, |event| matches!(
            event,
            envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2
        )),
        envelope::Payload::SeriesSnapshot(snapshot)
            if snapshot.series == Some(btc()) && snapshot.generation == 2
    ));
}

#[test]
fn provider_history_failure_resolves_to_explicit_terminal_state() {
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let service = MarketService::start_with_source(ControlledHistoryFailure {
        started: started_tx,
        release: release_rx,
    })
    .expect("failing history service starts");
    service.attach(1).expect("client attaches");
    service
        .register_consumer(1, 1, 1)
        .expect("consumer registers");
    service
        .set_demand(1, 1, 1, &btc())
        .expect("history demand is accepted asynchronously");
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("provider history starts");
    assert!(matches!(
        poll_until(&service, 1, 1, |event| matches!(
            event,
            envelope::Payload::SeriesState(state)
                if state.state == SeriesLoadState::Resolving as i32
        )),
        envelope::Payload::SeriesState(state)
            if state.generation == 1
                && state.series == Some(btc())
                && state.persistence == PersistenceState::NotRequested as i32
    ));
    release_tx.send(()).expect("provider failure released");
    let retry_driver = thread::spawn(move || {
        for _ in 0..MAXIMUM_HISTORY_RETRIES {
            started_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("provider history retry starts");
            release_tx
                .send(())
                .expect("provider retry failure released");
        }
    });
    assert!(matches!(
        poll_until(&service, 1, 1, |event| matches!(
            event,
            envelope::Payload::SeriesState(state)
                if state.state == SeriesLoadState::Failed as i32
        )),
        envelope::Payload::SeriesState(state)
            if state.generation == 1
                && state.detail.as_deref() == Some("Coinbase historical bars are unavailable")
    ));
    assert!(matches!(
        poll_until(&service, 1, 1, |event| matches!(
            event,
            envelope::Payload::DemandError(_)
        )),
        envelope::Payload::DemandError(error)
            if error.generation == 1
                && error.stage == "provider_history"
                && error.code == EngineFaultCode::Retryable as i32
                && error.stage_code == FailureStage::ProviderHistory as i32
                && error.series == Some(btc())
                && error.cause == "provider history did not produce usable canonical bars"
                && error.elapsed_millis.is_some()
    ));
    retry_driver.join().expect("history retry driver joins");
}

#[test]
fn demand_failures_preserve_series_stage_cause_and_elapsed_time_without_raw_payloads() {
    let consumer_id = ConsumerId(id(7).expect("consumer"));
    let generation = GenerationId(id(9).expect("generation"));
    let series = internal_series(&btc()).expect("series");
    let mut events = BTreeMap::from([(consumer_id, ConsumerEvents::default())]);
    fail_waiters(
        &mut events,
        vec![DemandWaiter {
            consumer_id,
            generation,
            started_at: Instant::now()
                .checked_sub(Duration::from_millis(5))
                .expect("five milliseconds is representable"),
        }],
        &series,
        FailureStage::Handoff,
        "history/live handoff failed",
    );
    let envelope::Payload::DemandError(error) = events
        .get_mut(&consumer_id)
        .and_then(ConsumerEvents::pop)
        .and_then(|_| events.get_mut(&consumer_id).and_then(ConsumerEvents::pop))
        .expect("structured failure follows terminal state")
    else {
        panic!("expected structured demand failure");
    };
    assert_eq!(error.consumer_id, 7);
    assert_eq!(error.generation, 9);
    assert_eq!(error.series, Some(btc()));
    assert_eq!(error.stage, "handoff");
    assert_eq!(error.stage_code, FailureStage::Handoff as i32);
    assert_eq!(
        error.cause,
        "history and realtime state could not be joined safely"
    );
    assert!(error.elapsed_millis.is_some_and(|elapsed| elapsed >= 5));
    assert!(!error.detail.contains("token"));
}

#[test]
fn full_history_queue_fails_waiters_without_blocking_the_coordinator() {
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    history_tx
        .try_send(HistoryRequest {
            series: internal_series(&btc()).expect("first series"),
            provider_generation: ProviderGeneration(id(1).expect("provider generation")),
            instrument: None,
            maximum_bars: HISTORY_BARS_PER_SERIES,
            range: None,
            kind: HistoryRequestKind::Initial,
            stop: Arc::new(AtomicBool::new(false)),
        })
        .expect("fill history queue");
    let mut second = internal_series(&btc()).expect("second series");
    second.instrument_id = "instrument:coinbase:eth:usd".to_string();
    assert_eq!(
        try_enqueue_history(
            &history_tx,
            HistoryRequest {
                series: second,
                provider_generation: ProviderGeneration(id(1).expect("provider generation")),
                instrument: None,
                maximum_bars: HISTORY_BARS_PER_SERIES,
                range: None,
                kind: HistoryRequestKind::Initial,
                stop: Arc::new(AtomicBool::new(false)),
            },
        ),
        Err("provider history capacity is temporarily exhausted")
    );
}

#[test]
fn rapid_switch_churn_publishes_only_the_latest_generation() {
    let (requested_tx, requested_rx) = mpsc::sync_channel(8);
    let (release_tx, release_rx) = mpsc::sync_channel(8);
    let service = MarketService::start_with_source(SwitchingHistory {
        requested: requested_tx,
        release: release_rx,
    })
    .expect("switching service starts");
    service.attach(1).expect("client attaches");
    service
        .register_consumer(1, 1, 1)
        .expect("consumer registers");
    let required = [
        selected_series("instrument:coinbase:btc:usd", 60),
        selected_series("instrument:coinbase:btc:usd", 300),
        selected_series("instrument:coinbase:btc:usd", 900),
        selected_series("instrument:coinbase:btc:usd", 3_600),
        selected_series("instrument:coinbase:btc:usd", 60),
        selected_series("instrument:coinbase:eth:usd", 60),
        selected_series("instrument:coinbase:btc:usd", 60),
    ];
    for (index, series) in required.iter().enumerate() {
        service
            .set_demand(1, 1, u64::try_from(index + 1).expect("generation"), series)
            .expect("rapid demand is accepted");
    }
    let latest_series =
        internal_series(required.last().expect("latest demand")).expect("latest internal series");
    loop {
        let requested = requested_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("latest history request starts");
        release_tx.send(()).expect("observed history completes");
        if requested == latest_series {
            break;
        }
    }
    let latest = poll_until(
        &service,
        1,
        1,
        |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 7),
    );
    assert!(matches!(
        latest,
        envelope::Payload::SeriesSnapshot(snapshot)
            if snapshot.generation == 7
                && snapshot.series.as_ref().is_some_and(|series| {
                    series.instrument_id == "instrument:coinbase:btc:usd"
                        && series.cadence_value == 60
                })
    ));

    for _ in 0..4 {
        release_tx
            .send(())
            .expect("any queued obsolete history completes");
    }
    let deadline = Instant::now() + Duration::from_millis(100);
    while Instant::now() < deadline {
        while let Some(event) = service.poll_event(1, 1).expect("final consumer polls") {
            assert!(
                !matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation != 7),
                "obsolete history reached the active consumer"
            );
        }
        thread::yield_now();
    }
}

#[test]
fn newer_demand_cancels_history_without_waiting_for_cleanup() {
    let (started_tx, started_rx) = mpsc::sync_channel(2);
    let (cancellation_tx, cancellation_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let service = MarketService::start_with_source(DelayedCancellationHistory {
        started: started_tx,
        cancellation_observed: cancellation_tx,
        release_cancellation: release_rx,
        block_first: true,
    })
    .expect("cancellation fixture starts");
    service.attach(1).expect("client attaches");
    service
        .register_consumer(1, 1, 1)
        .expect("consumer registers");
    service
        .set_demand(1, 1, 1, &btc())
        .expect("first history demand starts");
    assert_eq!(
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first history fetch starts"),
        (
            internal_series(&btc()).expect("first internal series"),
            ProviderGeneration(NonZeroU64::MIN),
        )
    );

    let eth = selected_series("instrument:coinbase:eth:usd", 60);
    let switched_service = service.clone();
    let (switched_tx, switched_rx) = mpsc::sync_channel(1);
    let switcher = thread::spawn(move || {
        let result = switched_service.set_demand(1, 1, 2, &eth);
        let _ = switched_tx.send(result);
    });
    cancellation_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("obsolete history observes cancellation");
    match switched_rx.recv_timeout(Duration::from_secs(1)) {
        Ok(Ok(())) => {}
        result => {
            let _ = release_tx.send(());
            let _ = switcher.join();
            panic!("new demand did not supersede old cleanup: {result:?}");
        }
    }
    release_tx
        .send(())
        .expect("obsolete history cleanup completes");
    switcher.join().expect("demand switch thread joins");
    assert_eq!(
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("new history fetch starts"),
        (
            internal_series(&selected_series("instrument:coinbase:eth:usd", 60))
                .expect("new internal series"),
            ProviderGeneration(NonZeroU64::MIN),
        )
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(event) = service.poll_event(1, 1).expect("consumer polls") {
            assert!(
                !matches!(&event, envelope::Payload::ProviderState(state)
                        if state.state == ProviderConnectionState::Recovering as i32),
                "intentional cancellation must not report provider recovery"
            );
            if matches!(&event, envelope::Payload::SeriesSnapshot(snapshot)
            if snapshot.generation == 2
                && snapshot.series.as_ref().is_some_and(|series| {
                    series.instrument_id == "instrument:coinbase:eth:usd"
                }))
            {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "new history publication timed out"
        );
        thread::yield_now();
    }
}

#[test]
fn coinbase_reconnect_cancels_blocked_obsolete_history() {
    let (started_tx, started_rx) = mpsc::sync_channel(2);
    let (cancellation_tx, cancellation_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let (action_tx, action_rx) = mpsc::sync_channel(1);
    let (realtime_generation_tx, realtime_generation_rx) = mpsc::sync_channel(2);
    let (realtime_stop_tx, _realtime_stop_rx) = mpsc::sync_channel(2);
    let service = MarketService::start_with_sources(
        DelayedCancellationHistory {
            started: started_tx,
            cancellation_observed: cancellation_tx,
            release_cancellation: release_rx,
            block_first: true,
        },
        Some(Box::new(FixtureRealtime {
            actions: action_rx,
            generations: realtime_generation_tx,
            stops: realtime_stop_tx,
            configured_products: None,
        })),
        None,
    )
    .expect("reconnect cancellation fixture starts");
    service.attach(1).expect("client attaches");
    service
        .register_consumer(1, 1, 1)
        .expect("consumer registers");
    service
        .set_demand(1, 1, 1, &btc())
        .expect("history demand starts");
    let series = internal_series(&btc()).expect("internal series");
    assert_eq!(
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("generation-one history blocks"),
        (series.clone(), ProviderGeneration(NonZeroU64::MIN))
    );
    assert_eq!(
        realtime_generation_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("generation-one realtime starts")
            .0
            .get(),
        1
    );

    action_tx
        .send(FixtureRealtimeAction::Connected)
        .expect("generation-one realtime connects");
    action_tx
        .send(FixtureRealtimeAction::Disconnect)
        .expect("realtime disconnects");
    assert_eq!(
        realtime_generation_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("realtime reconnects")
            .0
            .get(),
        2
    );
    cancellation_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("obsolete history observes generation cancellation");
    release_tx
        .send(())
        .expect("obsolete history cleanup completes");
    assert_eq!(
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("current history enters the freed lane"),
        (
            series,
            ProviderGeneration(NonZeroU64::new(2).expect("generation")),
        )
    );
}

/// A quiet market must not stall the live feed.
///
/// Coinbase publishes no candle for a bucket in which nothing traded, so the
/// feed skipping two buckets means the market was quiet, not that data was
/// lost. Treating that as a history seam froze the series behind a repair
/// that could never find the bars it was waiting for.
#[test]
fn a_quiet_market_gap_keeps_streaming_and_fills_the_empty_buckets() {
    let harness = MarketService::start_fixture_realtime(vec![history_bar()])
        .expect("realtime fixture starts");
    attach_fixture_consumers(&harness);
    expect_realtime_generation(&harness, "first realtime generation", 1);
    harness
        .actions
        .send(FixtureRealtimeAction::Connected)
        .expect("connect fixture");
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(2, "2.00", 1)))
        .expect("first live trade");
    let live = poll_until(&harness.service, 1, 1, |event| {
        is_live_update(event, 1, 1, 200)
    });
    assert!(is_live_update(&live, 1, 1, 200));

    // Nothing trades for two minutes, then the market resumes.
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(5, "2.50", 2)))
        .expect("live trade after a quiet gap");
    let bars = collect_series_until(&harness.service, 1, 1, |bars| {
        bars.values().any(|bar| bar.close == 250)
    });

    // The attach snapshot already delivered minute 1 as sequence 2, so the
    // live stream has to continue at 3 and reach minute 5 without a gap.
    let minutes = bars
        .values()
        .map(|bar| bar.exchange_timestamp_seconds / 60)
        .collect::<Vec<_>>();
    assert_eq!(minutes, vec![2, 3, 4, 5]);
    assert_eq!(bars.keys().copied().collect::<Vec<_>>(), vec![3, 4, 5, 6]);
    let quiet = bars.values().skip(1).take(2).collect::<Vec<_>>();
    assert!(
        quiet.iter().all(|bar| bar.volume == 0
            && bar.open == 200
            && bar.high == 200
            && bar.low == 200
            && bar.close == 200),
        "empty buckets carry the previous close at zero volume: {quiet:?}"
    );
}

/// Switching timeframe after a quiet stretch must still print bars.
///
/// A series frozen behind a seam repair also held the provider short of
/// `Online`, and every other series is gated on that, so the chart the user
/// switched to stayed blank.
/// The open bucket is history's tail, not history's newest entry.
///
/// A page fetched through *now* carries a bucket the provider has not closed.
/// Writing it into history freezes a half-built candle that stays wrong until
/// the bucket rolls; on an hourly chart that is an hour of bad data.
#[test]
fn coinbase_history_separates_the_open_bucket_from_closed_history() {
    // Seven minute bars: the first five close one 5m bucket, the last two are
    // the part of the next bucket that has traded so far.
    let source = (0..7)
        .map(|minute| MarketBar {
            source_sequence: u64::try_from(minute).expect("minute fits") + 1,
            exchange_timestamp_seconds: minute * 60,
            exchange_timestamp_unix_nanos: minute * 60 * 1_000_000_000,
            open: 100 + minute,
            high: 120 + minute,
            low: 80 + minute,
            close: 110 + minute,
            volume: 3,
        })
        .collect::<Vec<_>>();

    let (closed, _) =
        aggregate_coinbase_bars(&source, CoinbaseInterval::Minute5, Some(300)).expect("closed");
    let forming = forming_coinbase_bucket(&source, CoinbaseInterval::Minute5, 300)
        .expect("forming bucket")
        .expect("the open bucket has traded");

    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].exchange_timestamp_seconds, 0);
    assert_eq!(forming.exchange_timestamp_seconds, 300);
    assert_eq!(forming.source_sequence, closed[0].source_sequence + 1);
    // The open candle carries what the bucket has actually done, not just
    // whatever trades happen to arrive after the chart was selected.
    assert_eq!(forming.open, 105);
    assert_eq!(forming.close, 116);
    assert_eq!(forming.high, 126);
    assert_eq!(forming.low, 85);
    assert_eq!(forming.volume, 6);
}

/// The live-edge page has to reach the candle the market is trading in, and
/// it has to stay on a source boundary while doing so.
///
/// Requesting only up to the last closed bucket is what made the forming
/// split unreachable; requesting an unaligned instant is what made the
/// provider reject the page outright.
#[test]
fn the_initial_coinbase_page_includes_the_open_source_candle() {
    for (period, interval) in [
        (BarPeriod::time(60).expect("1m"), CoinbaseInterval::Minute1),
        (BarPeriod::time(180).expect("3m"), CoinbaseInterval::Minute3),
        (BarPeriod::time(300).expect("5m"), CoinbaseInterval::Minute5),
        (
            BarPeriod::time(14_400).expect("4h"),
            CoinbaseInterval::Hour4,
        ),
        (
            BarPeriod::time(43_200).expect("12h"),
            CoinbaseInterval::Hour12,
        ),
        (BarPeriod::month(1).expect("1M"), CoinbaseInterval::Month1),
    ] {
        let series = BarSeriesKey {
            provider_id: "coinbase".to_string(),
            instrument_id: "instrument:coinbase:BTC-USD".to_string(),
            entitlement_id: "coinbase-public".to_string(),
            period,
            definition_version: 1,
        };
        let range = recent_coinbase_history_range(&series, 200).expect("initial range");
        let now_seconds = current_unix_nanos()
            .expect("clock")
            .div_euclid(1_000_000_000);
        let end_seconds = range.end_unix_nanos.div_euclid(1_000_000_000);
        let source_seconds = interval.source().1;
        let open_source_start = now_seconds - now_seconds.rem_euclid(source_seconds);

        assert_eq!(
            end_seconds,
            open_source_start + source_seconds,
            "{} must use the exclusive boundary after the open source candle",
            interval.id(),
        );
    }
}

#[test]
fn a_coinbase_page_served_after_bucket_roll_is_retried() {
    let interval = CoinbaseInterval::Minute1;

    assert!(coinbase_history_edge_is_current(interval, 600, 659_999_999_999).expect("same bucket"));
    assert!(
        !coinbase_history_edge_is_current(interval, 600, 660_000_000_000).expect("next bucket")
    );
}

#[test]
fn coinbase_open_candle_replay_cutoff_comes_from_provider_bucket_identity() {
    let cutoff = coinbase_handoff_replay_boundary(
        CoinbaseInterval::Minute1,
        600,
        true,
        true,
        660_000_000_000,
    )
    .expect("replay cutoff");

    assert_eq!(cutoff, 659_999_999_999);
    assert!(10 * 60_000_000_000 + 59_000_000_000 <= cutoff);
    assert!(11 * 60_000_000_000 + 1 > cutoff);
}

#[test]
fn a_coinbase_bucket_roll_requests_authoritative_closed_ohlcv() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let realtime_stop = Arc::new(AtomicBool::new(false));
    let series = internal_series(&btc()).expect("series");
    let generation = ProviderGeneration(id(1).expect("provider generation"));
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let mut coordinator = retained_history_coordinator(
        configured_engine().expect("engine configures"),
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &realtime_stop,
        consumer_id,
        &series,
    );
    let completed = history_bar();
    let current = MarketBar {
        source_sequence: completed.source_sequence + 1,
        exchange_timestamp_seconds: 120,
        exchange_timestamp_unix_nanos: 120_000_000_000,
        ..completed
    };
    let key = (series.clone(), generation);
    coordinator.history_inflight.insert(key.clone(), None);

    coordinator.schedule_coinbase_live_edge_repair(&series, generation, &completed, &current);

    assert!(matches!(history_rx.try_recv(), Err(TryRecvError::Empty)));
    assert_eq!(
        coordinator
            .pending_live_edge_repairs
            .get(&key)
            .map(|pending| pending.range),
        Some(HistoryRange {
            start_unix_nanos: 60_000_000_000,
            end_unix_nanos: 120_000_000_000,
        })
    );
    coordinator.history_inflight.remove(&key);
    coordinator.flush_coinbase_live_edge_repair(&series, generation);

    let request = history_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("live edge repair request");
    assert_eq!(request.kind, HistoryRequestKind::LiveEdgeRepair(0));
    assert_eq!(
        request.range,
        Some(HistoryRange {
            start_unix_nanos: 60_000_000_000,
            end_unix_nanos: 120_000_000_000,
        })
    );

    coordinator.history_completed(
        &series,
        generation,
        request.range,
        request.kind,
        Ok(backfill_snapshot(Vec::new())),
    );

    let retry = coordinator
        .pending_live_edge_repairs
        .get_mut(&key)
        .expect("empty boundary response stays pending");
    assert_eq!(retry.attempt, 1);
    retry.ready_at = Instant::now();
    coordinator.flush_coinbase_live_edge_repair(&series, generation);
    assert!(matches!(
        history_rx.recv_timeout(Duration::from_secs(1)),
        Ok(HistoryRequest {
            kind: HistoryRequestKind::LiveEdgeRepair(1),
            range: Some(HistoryRange {
                start_unix_nanos: 60_000_000_000,
                end_unix_nanos: 120_000_000_000,
            }),
            ..
        })
    ));
}

/// Both local reads have to agree about what a bar is called.
///
/// The store renumbers every bar it returns 1..N. Publishing that as-is gave
/// the same minutes a second identity, and the client rejected the covering
/// snapshot that carried it as stale — which is what left a freshly launched
/// chart showing an error instead of streaming.
#[test]
fn a_local_range_read_keeps_bucket_identity() {
    let series = BarSeriesKey {
        provider_id: "coinbase".to_string(),
        instrument_id: "instrument:coinbase:BTC-USD".to_string(),
        entitlement_id: ENTITLEMENT_CLASS.to_string(),
        period: BarPeriod::time(60).expect("period"),
        definition_version: 1,
    };
    let renumbered = (0..4)
        .map(|index| MarketBar {
            source_sequence: index + 1,
            exchange_timestamp_seconds: 29_801_760 * 60 + i64::try_from(index).expect("index") * 60,
            exchange_timestamp_unix_nanos: (29_801_760 * 60
                + i64::try_from(index).expect("index") * 60)
                * 1_000_000_000,
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 3,
        })
        .collect::<Vec<_>>();

    let canonical = canonical_local_range(
        &series,
        Some(StoredHistory {
            bars: renumbered,
            derived: false,
            durable: true,
        }),
    )
    .expect("range canonicalises")
    .expect("the range has bars");

    assert_eq!(
        canonical
            .bars
            .iter()
            .map(|bar| bar.source_sequence)
            .collect::<Vec<_>>(),
        vec![29_801_761, 29_801_762, 29_801_763, 29_801_764],
        "identity comes from the bucket, never from the position in the page"
    );
}

/// A bucket the source has not covered end to end is never closed history.
#[test]
fn coinbase_forming_bucket_is_absent_until_the_bucket_trades() {
    let source = vec![MarketBar {
        source_sequence: 1,
        exchange_timestamp_seconds: 0,
        exchange_timestamp_unix_nanos: 0,
        open: 100,
        high: 110,
        low: 90,
        close: 105,
        volume: 3,
    }];
    assert!(
        forming_coinbase_bucket(&source, CoinbaseInterval::Minute5, 300)
            .expect("forming bucket")
            .is_none()
    );
}

#[test]
fn coinbase_handoff_keeps_ambiguous_open_bucket_trades_provisional() {
    let (history_tx, history_rx) = mpsc::sync_channel(4);
    let (storage_tx, storage_rx) = mpsc::sync_channel(4);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(4);
    let stop = Arc::new(AtomicBool::new(false));
    let (mut coordinator, consumer_id, series) =
        connected_coinbase_handoff_coordinator(&history_tx, &storage_tx, &realtime_tx, &stop);
    let live = coordinator.live.get_mut(&series).expect("handoff exists");
    // Coinbase does not report when it sampled its open candle. Both trades in
    // minute 10 could already be in that aggregate even when one arrived after
    // the client's response timestamp, so neither may be folded into it.
    let boundary = 11 * 60_000_000_000 - 1;
    live.buffered.push_back(trade(10, "1.50", 1));
    let mut newer = trade(10, "3.00", 2);
    newer.trade_time_unix_nanos = 10 * 60_000_000_000 + 59_000_000_000;
    live.buffered.push_back(newer);
    live.buffered.push_back(trade(11, "4.00", 3));

    let expected_closed = coinbase_history(1, 9);
    let forming = MarketBar {
        source_sequence: 11,
        exchange_timestamp_seconds: 600,
        exchange_timestamp_unix_nanos: 600_000_000_000,
        open: 100,
        high: 210,
        low: 90,
        close: 200,
        volume: 5,
    };
    let (closed, forming) = coordinator
        .install_completed_history(
            &series,
            coordinator.coinbase_provider_generation(),
            coinbase_forming_snapshot(expected_closed.clone(), forming, boundary),
            false,
            expected_closed.clone(),
            true,
        )
        .expect("history installs");
    assert!(coordinator.complete_coinbase_live_handoff(
        &series,
        &closed,
        forming,
        Some(boundary),
        true,
    ));

    let live = coordinator.live.get_mut(&series).expect("handoff exists");
    let open = live
        .aggregator
        .in_flight()
        .expect("the next candle is held");
    assert_eq!(
        (open.source_sequence, open.close, open.volume),
        (12, 400, 1)
    );
    let completed = live.aggregator.completed_after(10);
    assert_eq!(completed.len(), 1);
    assert_eq!((completed[0].close, completed[0].volume), (200, 5));
    assert_eq!(live.history, CoinbaseHistoryReadiness::Provisional);
    assert_eq!(live.published_completed, Some(10));
    coordinator.provider_online_if_all_series_ready();
    assert!(matches!(
        coordinator.events[&consumer_id].series_state,
        Some(envelope::Payload::SeriesState(ref state))
            if state.state == SeriesLoadState::Partial as i32
                && state.detail.as_deref().is_some_and(|detail| detail.contains("provisional"))
    ));
    assert_persistence_keeps_provisional(&mut coordinator, consumer_id, &series);
    let repair = history_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("cross-bucket buffered trade schedules authoritative repair");
    assert_eq!(repair.kind, HistoryRequestKind::LiveEdgeRepair(0));
    assert_eq!(
        repair.range,
        Some(HistoryRange {
            start_unix_nanos: 600_000_000_000,
            end_unix_nanos: 660_000_000_000,
        })
    );
    let authoritative = MarketBar {
        close: 250,
        high: 250,
        volume: 8,
        ..completed[0]
    };
    coordinator.history_completed(
        &series,
        coordinator.coinbase_provider_generation(),
        repair.range,
        repair.kind,
        Ok(backfill_snapshot(vec![authoritative])),
    );
    assert_eq!(
        coordinator.live[&series].history,
        CoinbaseHistoryReadiness::Authoritative
    );
    assert!(matches!(
        coordinator.events[&consumer_id].series_state,
        Some(envelope::Payload::SeriesState(ref state))
            if state.state == SeriesLoadState::Live as i32
    ));
    assert!(matches!(
        storage_rx.try_recv(),
        Ok(StorageRequest::Persist(_, _, ref bars, _, _, _)) if bars == &expected_closed
    ));
}

fn assert_persistence_keeps_provisional(
    coordinator: &mut Coordinator<'_>,
    consumer_id: ConsumerId,
    series: &BarSeriesKey,
) {
    coordinator.broadcast_persistence_for(
        series,
        PersistenceState::Durable,
        Some("Coinbase history is cached"),
    );
    assert!(matches!(
        coordinator.events[&consumer_id].series_state,
        Some(envelope::Payload::SeriesState(ref state))
            if state.state == SeriesLoadState::Partial as i32
                && state.detail.as_deref().is_some_and(|detail| detail.contains("provisional"))
    ));
}

fn connected_coinbase_handoff_coordinator<'a>(
    history: &'a SyncSender<HistoryRequest>,
    storage: &'a SyncSender<StorageRequest>,
    realtime: &'a SyncSender<RealtimeControl>,
    stop: &'a Arc<AtomicBool>,
) -> (Coordinator<'a>, ConsumerId, BarSeriesKey) {
    let mut engine = configured_engine().expect("engine configures");
    let consumer_id = ConsumerId(id(1).expect("consumer"));
    let series = internal_series(&btc()).expect("series");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id: ClientId(id(1).expect("client")),
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand(
            consumer_id,
            GenerationId(id(1).expect("generation")),
            &series,
        )
        .expect("series demand installs");
    let mut coordinator = retained_history_coordinator(
        engine,
        history,
        storage,
        realtime,
        stop,
        consumer_id,
        &series,
    );
    let instrument = coinbase_instrument(&series);
    let mut live = LiveHandoff::try_new(
        &series,
        coordinator.coinbase_provider_generation(),
        &instrument,
    )
    .expect("live handoff");
    live.connected = true;
    coordinator.realtime_connected = true;
    coordinator.live.insert(series.clone(), live);
    (coordinator, consumer_id, series)
}

#[test]
fn coinbase_live_edge_retry_exhaustion_stays_partial_and_rearms_next_close() {
    let (history_tx, history_rx) = mpsc::sync_channel(4);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let stop = Arc::new(AtomicBool::new(false));
    let (mut coordinator, consumer_id, series) =
        connected_coinbase_handoff_coordinator(&history_tx, &storage_tx, &realtime_tx, &stop);
    let generation = coordinator.coinbase_provider_generation();
    coordinator.live.get_mut(&series).expect("handoff").history =
        CoinbaseHistoryReadiness::Authoritative;
    let completed = history_bar();
    let current = MarketBar {
        source_sequence: completed.source_sequence + 1,
        exchange_timestamp_seconds: 120,
        exchange_timestamp_unix_nanos: 120_000_000_000,
        ..completed
    };

    coordinator.schedule_coinbase_live_edge_repair(&series, generation, &completed, &current);
    assert_eq!(
        coordinator.live[&series].history,
        CoinbaseHistoryReadiness::Provisional
    );
    assert!(matches!(
        coordinator.events[&consumer_id].series_state,
        Some(envelope::Payload::SeriesState(ref state))
            if state.state == SeriesLoadState::Partial as i32
    ));

    for attempt in 0..=MAXIMUM_LIVE_EDGE_REPAIR_RETRIES {
        let request = history_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("bounded repair attempt");
        assert_eq!(request.kind, HistoryRequestKind::LiveEdgeRepair(attempt));
        coordinator.history_completed(
            &series,
            generation,
            request.range,
            request.kind,
            Err("fixture repair unavailable".to_string()),
        );
        if attempt < MAXIMUM_LIVE_EDGE_REPAIR_RETRIES {
            coordinator
                .pending_live_edge_repairs
                .get_mut(&(series.clone(), generation))
                .expect("retry remains pending")
                .ready_at = Instant::now();
            coordinator.flush_coinbase_live_edge_repair(&series, generation);
        }
    }

    assert!(coordinator.pending_live_edge_repairs.is_empty());
    assert!(matches!(
        coordinator.events[&consumer_id].demand_error,
        Some(envelope::Payload::DemandError(ref error))
            if error.stage_code == FailureStage::ProviderHistory as i32
                && error.detail.contains("bounded retries")
    ));
    let next = MarketBar {
        source_sequence: current.source_sequence + 1,
        exchange_timestamp_seconds: 180,
        exchange_timestamp_unix_nanos: 180_000_000_000,
        ..current
    };
    coordinator.schedule_coinbase_live_edge_repair(&series, generation, &current, &next);
    assert!(matches!(
        history_rx.recv_timeout(Duration::from_secs(1)),
        Ok(HistoryRequest {
            kind: HistoryRequestKind::LiveEdgeRepair(0),
            ..
        })
    ));
}

/// A not-yet-delivered append remains an append after same-candle coalescing.
#[test]
fn queued_append_keeps_its_operation_when_the_same_candle_is_revised() {
    let mut events = ConsumerEvents::default();
    let series = btc();
    for (close, operation) in [
        (105, SeriesUpdateOperation::AppendTail),
        (108, SeriesUpdateOperation::ReviseTail),
    ] {
        events.publish_series_update(envelope::Payload::SeriesUpdate(
            axiusflow_engine_protocol::SeriesUpdate {
                consumer_id: 1,
                generation: 1,
                series: Some(series.clone()),
                provider_generation: 1,
                bar: Some(ipc_bar(MarketBar {
                    close,
                    ..history_bar()
                })),
                publication_generation: u64::try_from(close).expect("positive close"),
                forming: true,
                operation: operation.into(),
            },
        ));
    }

    let Some(envelope::Payload::SeriesUpdate(update)) = events.queued_series() else {
        panic!("coalesced update remains queued");
    };
    assert_eq!(update.operation, SeriesUpdateOperation::AppendTail as i32);
    assert_eq!(update.bar.expect("revised bar remains").close, 108);
    assert_eq!(events.series.len(), 1);
}

/// The consumer outbox may lose bars only by announcing it.
#[test]
fn an_overflowed_series_queue_is_replaced_by_a_covering_snapshot() {
    let mut events = ConsumerEvents::default();
    let series = btc();
    for sequence in 1..=u64::try_from(CONSUMER_SERIES_QUEUE_CAPACITY).expect("capacity fits") + 1 {
        events.publish_series_update(envelope::Payload::SeriesUpdate(
            axiusflow_engine_protocol::SeriesUpdate {
                consumer_id: 1,
                generation: 1,
                series: Some(series.clone()),
                provider_generation: 1,
                bar: Some(ipc_bar(MarketBar {
                    source_sequence: sequence,
                    exchange_timestamp_seconds: i64::try_from(sequence).expect("fits") * 60,
                    exchange_timestamp_unix_nanos: i64::try_from(sequence).expect("fits")
                        * 60_000_000_000,
                    open: 100,
                    high: 110,
                    low: 90,
                    close: 105,
                    volume: 1,
                })),
                publication_generation: 1,
                forming: true,
                operation: SeriesUpdateOperation::AppendTail.into(),
            },
        ));
    }

    assert!(
        events.series_overflowed,
        "falling further behind than the queue holds must be announced"
    );
    assert!(
        events.series.len() <= CONSUMER_SERIES_QUEUE_CAPACITY,
        "the queue stays bounded"
    );
    // The one thing that must never happen is a queue that still looks
    // contiguous while a bar in the middle of it has been discarded.
    events.publish_snapshot(envelope::Payload::SeriesSnapshot(IpcSeriesSnapshot {
        consumer_id: 1,
        generation: 1,
        series: Some(series),
        provider_generation: 1,
        price_scale: 2,
        quantity_scale: 8,
        bars: Vec::new(),
        publication_generation: 2,
        forming: false,
    }));
    assert!(!events.series_overflowed);
    assert_eq!(events.series.len(), 1);
}

#[test]
fn overload_recovery_survives_slow_consumer_history_pressure_and_reconnect_storms() {
    let (history_tx, history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, _realtime_rx) = mpsc::sync_channel(1);
    let stop = Arc::new(AtomicBool::new(false));
    let (mut coordinator, consumer_id, series) =
        connected_coinbase_handoff_coordinator(&history_tx, &storage_tx, &realtime_tx, &stop);
    let generation = coordinator.coinbase_provider_generation();
    coordinator
        .engine
        .install_history(generation, &series, 2, 8, vec![history_bar()])
        .expect("canonical recovery image installs");

    let events = coordinator
        .events
        .get_mut(&consumer_id)
        .expect("consumer outbox exists");
    for sequence in 1..=u64::try_from(CONSUMER_SERIES_QUEUE_CAPACITY).expect("capacity fits") + 1 {
        events.publish_series_update(envelope::Payload::SeriesUpdate(
            axiusflow_engine_protocol::SeriesUpdate {
                consumer_id: consumer_id.0.get(),
                generation: 1,
                series: Some(btc()),
                provider_generation: generation.0.get(),
                bar: Some(ipc_bar(MarketBar {
                    source_sequence: sequence,
                    exchange_timestamp_seconds: i64::try_from(sequence).expect("sequence fits"),
                    exchange_timestamp_unix_nanos: i64::try_from(sequence).expect("sequence fits"),
                    ..history_bar()
                })),
                publication_generation: sequence,
                forming: false,
                operation: SeriesUpdateOperation::AppendTail.into(),
            },
        ));
    }
    assert!(events.series_overflowed);

    history_tx
        .try_send(HistoryRequest {
            series: series.clone(),
            provider_generation: generation,
            instrument: None,
            maximum_bars: HISTORY_BARS_PER_SERIES,
            range: None,
            kind: HistoryRequestKind::Initial,
            stop: Arc::new(AtomicBool::new(false)),
        })
        .expect("history lane is saturated");

    for _ in 0..3 {
        coordinator.realtime_interrupted(
            FailureStage::Handoff,
            "fixture reconnect recovery requires history",
        );
    }
    let retry_key = (series.clone(), generation);
    assert_eq!(
        coordinator.history_retries.len(),
        1,
        "a reconnect storm retains one bounded retry per series"
    );
    assert_eq!(
        coordinator
            .history_retries
            .get(&retry_key)
            .map(|(_, attempts)| *attempts),
        Some(0),
        "queue pressure does not consume the provider failure budget"
    );

    history_rx
        .try_recv()
        .expect("saturated request is released");
    coordinator
        .history_retries
        .get_mut(&retry_key)
        .expect("capacity retry remains owned by the coordinator")
        .0 = Instant::now();
    coordinator.retry_history();
    let recovery = history_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("coordinator tick delivers recovery without a provider heartbeat");
    assert_eq!(recovery.series, series);
    assert_eq!(recovery.provider_generation, generation);
    assert!(coordinator.history_inflight.contains_key(&retry_key));

    coordinator.recover_overflowed_series_queues();
    let events = &coordinator.events[&consumer_id];
    assert!(!events.series_overflowed);
    assert_eq!(events.series.len(), 1);
    assert!(matches!(
        events.series.front(),
        Some(envelope::Payload::SeriesSnapshot(snapshot))
            if snapshot.bars.last().is_some_and(|bar| bar.close == history_bar().close)
    ));
}

/// Trades that arrive while Rithmic history is in flight belong to the open
/// candle, not to the floor.
///
/// A Rithmic replay can take tens of seconds. Every trade in that window used
/// to be discarded, so a chart opened on a period that began at the first
/// trade after the fetch returned.
#[test]
fn rithmic_trades_buffered_during_history_reach_the_open_time_candle() {
    let series = rithmic_series_key("instrument:rithmic:CME:MNQU6", "rithmic-test:CME:MNQU6");
    let mut live = RithmicLiveHandoff::new(&series, ProviderGeneration(NonZeroU64::MIN), "CME")
        .expect("live cadence");
    live.connected = true;

    // The open minute starts at 120s. One trade lands inside the window the
    // replay already covers; two land after it.
    let boundary = 130_000_000_000;
    for trade in [
        rithmic_trade(1, 1, 125_000_000_000, 108),
        rithmic_trade(2, 1, 135_000_000_000, 121),
        rithmic_trade(3, 1, 145_000_000_000, 117),
    ] {
        live.accept_trade(&trade).expect("trades buffer");
    }
    assert!(!live.history_ready, "history has not landed yet");

    let closed = MarketBar {
        source_sequence: 4,
        exchange_timestamp_seconds: 60,
        exchange_timestamp_unix_nanos: 60_000_000_000,
        open: 100,
        high: 110,
        low: 90,
        close: 105,
        volume: 7,
    };
    let forming = MarketBar {
        source_sequence: 5,
        exchange_timestamp_seconds: 120,
        exchange_timestamp_unix_nanos: 120_000_000_000,
        open: 105,
        high: 112,
        low: 104,
        close: 108,
        volume: 3,
    };
    live.seed(
        2,
        0,
        &[closed],
        Some(FormingBar {
            bar: forming,
            trades: None,
        }),
        Some(boundary),
    )
    .expect("history seeds");

    let open = *live.bars.last().expect("the open candle is held");
    assert_eq!(
        open.source_sequence, 5,
        "the open period is revised in place"
    );
    assert_eq!(open.exchange_timestamp_seconds, 120);
    // The replay's own OHLCV survives and the two newer trades extend it;
    // the trade the replay already covered is not counted twice.
    assert_eq!(open.open, 105);
    assert_eq!(open.high, 121);
    assert_eq!(open.low, 104);
    assert_eq!(open.close, 117);
    assert_eq!(open.volume, 7);
    assert_eq!(
        live.bars.len(),
        2,
        "closed history keeps exactly its own bar"
    );
    // The engine holds the closed bar, so the open one publishes as the tail
    // that continues it rather than as a whole new series.
    assert!(matches!(
        live.take_publication(),
        Some(LiveSeriesPublication::Tails(bars)) if bars.len() == 1 && bars[0].source_sequence == 5
    ));
}

/// A tick chart's open bundle resumes at the trade count it was caught at.
#[test]
fn rithmic_open_tick_bundle_resumes_at_its_replayed_trade_count() {
    let mut series = rithmic_series_key("instrument:rithmic:CME:MNQU6", "rithmic-test:CME:MNQU6");
    series.period = BarPeriod::tick(100).expect("tick period");
    let mut live = RithmicLiveHandoff::new(&series, ProviderGeneration(NonZeroU64::MIN), "CME")
        .expect("live cadence");
    live.connected = true;
    let closed = MarketBar {
        source_sequence: 4,
        exchange_timestamp_seconds: 60,
        exchange_timestamp_unix_nanos: 60_000_000_000,
        open: 100,
        high: 110,
        low: 90,
        close: 105,
        volume: 7,
    };
    let forming = MarketBar {
        source_sequence: 5,
        exchange_timestamp_seconds: 120,
        exchange_timestamp_unix_nanos: 120_000_000_000,
        open: 105,
        high: 112,
        low: 104,
        close: 108,
        volume: 98,
    };
    live.seed(
        2,
        0,
        &[closed],
        Some(FormingBar {
            bar: forming,
            trades: Some(98),
        }),
        Some(120_000_000_000),
    )
    .expect("history seeds");
    assert!(matches!(
        live.cadence,
        RithmicLiveCadence::Tick { forming: 98, .. }
    ));

    // Trades 99 and 100 finish the bundle; 101 opens the next one.
    live.apply_trade(&rithmic_trade(1, 1, 130_000_000_000, 115))
        .expect("trade 99 applies");
    live.apply_trade(&rithmic_trade(2, 1, 131_000_000_000, 116))
        .expect("trade 100 applies");
    assert_eq!(live.bars.len(), 2, "the bundle is still the open one");
    assert_eq!(live.bars[1].source_sequence, 5);
    live.apply_trade(&rithmic_trade(3, 1, 132_000_000_000, 117))
        .expect("trade 101 applies");
    assert_eq!(
        live.bars.len(),
        3,
        "the hundred-and-first trade opens the next bundle"
    );
    assert_eq!(live.bars[2].source_sequence, 6);
}

/// A tick bundle with no replayed count is treated as closed.
///
/// Resuming a bundle whose trade count is unknown would put a fabricated
/// candle on the chart and split every later one in the wrong place.
#[test]
fn rithmic_open_tick_bundle_without_a_count_is_treated_as_closed() {
    let mut series = rithmic_series_key("instrument:rithmic:CME:MNQU6", "rithmic-test:CME:MNQU6");
    series.period = BarPeriod::tick(100).expect("tick period");
    let mut live = RithmicLiveHandoff::new(&series, ProviderGeneration(NonZeroU64::MIN), "CME")
        .expect("live cadence");
    live.connected = true;
    live.seed(
        2,
        0,
        &[history_bar()],
        Some(FormingBar {
            bar: MarketBar {
                source_sequence: 3,
                exchange_timestamp_seconds: 120,
                exchange_timestamp_unix_nanos: 120_000_000_000,
                ..history_bar()
            },
            trades: None,
        }),
        None,
    )
    .expect("history seeds");
    assert!(live.forming_tail_sequence.is_none());
    assert!(matches!(
        live.cadence,
        RithmicLiveCadence::Tick { trades, forming } if trades == forming
    ));
}

/// Round-tripping the timeframe must leave the minute series exactly as it
/// was, plus whatever the market did in between.
///
/// Bar identity comes from the bucket, so a minute bar keeps its sequence
/// across a switch to five minutes and back. When identity came from a
/// running counter, the returning series renumbered every bar and the
/// consumer read the result as a series that had changed underneath it.
#[test]
fn switching_coinbase_timeframe_and_back_keeps_the_minute_series_canonical() {
    let harness = MarketService::start_fixture_realtime(vec![history_bar()])
        .expect("realtime fixture starts");
    attach_fixture_consumers(&harness);
    expect_realtime_generation(&harness, "first realtime generation", 1);
    harness
        .actions
        .send(FixtureRealtimeAction::Connected)
        .expect("connect fixture");
    for (minute, price) in [(6_i64, "2.00"), (7, "2.10"), (8, "2.20")] {
        harness
            .actions
            .send(FixtureRealtimeAction::Trade(trade(
                minute,
                price,
                u64::try_from(minute).expect("minute fits"),
            )))
            .expect("live trade");
    }
    let reference = collect_series_until(&harness.service, 1, 1, |bars| {
        bars.values().any(|bar| bar.close == 220)
    });

    let five_minute = selected_series("instrument:coinbase:btc:usd", 300);
    harness
        .service
        .set_demand(1, 1, 2, &five_minute)
        .expect("the chart switches to five minutes");
    let _ = poll_until(
        &harness.service,
        1,
        1,
        |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2),
    );
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(9, "2.30", 9)))
        .expect("live trade on the coarser timeframe");

    harness
        .service
        .set_demand(1, 1, 3, &btc())
        .expect("the chart switches back to one minute");
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(10, "2.40", 10)))
        .expect("live trade after switching back");
    let returned = collect_series_until(&harness.service, 1, 1, |bars| {
        bars.values().any(|bar| bar.close == 240)
    });

    // Contiguity is the contract every consumer checks: adjacent sequences,
    // adjacent buckets, no holes.
    let sequences = returned.keys().copied().collect::<Vec<_>>();
    assert!(
        sequences.windows(2).all(|pair| pair[0] + 1 == pair[1]),
        "the returning minute series is not contiguous: {sequences:?}"
    );
    assert!(
        returned
            .values()
            .collect::<Vec<_>>()
            .windows(2)
            .all(|pair| pair[0].exchange_timestamp_seconds + 60
                == pair[1].exchange_timestamp_seconds),
        "the returning minute series skips a bucket"
    );

    // Every bar both folds hold must be the same bar: same identity, same
    // OHLCV. Anything else means the switch rewrote history.
    let mut shared = 0;
    for (sequence, bar) in &reference {
        if let Some(returning) = returned.get(sequence) {
            assert_eq!(
                returning, bar,
                "minute bar {sequence} changed across a timeframe round trip"
            );
            shared += 1;
        }
    }
    assert!(shared > 0, "the two folds share no bars to compare");
    assert!(
        returned.values().any(|bar| bar.close == 240),
        "the returning series never reached the newest trade"
    );
}

#[test]
fn switching_timeframe_after_a_quiet_gap_resumes_live_bars() {
    let harness = MarketService::start_fixture_realtime(vec![history_bar()])
        .expect("realtime fixture starts");
    attach_fixture_consumers(&harness);
    expect_realtime_generation(&harness, "first realtime generation", 1);
    harness
        .actions
        .send(FixtureRealtimeAction::Connected)
        .expect("connect fixture");
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(6, "2.00", 1)))
        .expect("live trade after a quiet gap");
    let live = poll_until(&harness.service, 1, 1, |event| {
        is_live_update(event, 1, 1, 200)
    });
    assert!(is_live_update(&live, 1, 1, 200));

    let five_minute = selected_series("instrument:coinbase:btc:usd", 300);
    harness
        .service
        .set_demand(1, 1, 2, &five_minute)
        .expect("the chart switches timeframe");
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(35, "2.40", 2)))
        .expect("live trade on the new timeframe");
    let switched = poll_until(&harness.service, 1, 1, |event| {
        is_live_update(event, 2, 1, 240)
    });
    assert!(is_live_update(&switched, 2, 1, 240));
}

#[test]
fn realtime_handoff_recovers_without_reconstructing_consumers() {
    let harness = MarketService::start_fixture_realtime(vec![history_bar()])
        .expect("realtime fixture starts");
    attach_fixture_consumers(&harness);
    assert_eq!(
        harness
            .generations
            .recv_timeout(Duration::from_secs(1))
            .expect("first realtime generation")
            .0
            .get(),
        1
    );
    harness
        .actions
        .send(FixtureRealtimeAction::Connected)
        .expect("connect fixture");
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(2, "2.00", 1)))
        .expect("first live trade");
    let first_live = poll_until(&harness.service, 1, 1, |event| {
        is_live_update(event, 1, 1, 200)
    });
    assert!(matches!(
        first_live,
        envelope::Payload::SeriesUpdate(update)
            if update.bar.as_ref().is_some_and(|bar| bar.source_sequence == 3 && bar.close == 200)
    ));

    harness
        .actions
        .send(FixtureRealtimeAction::Disconnect)
        .expect("disconnect fixture");
    let recovering = poll_until(
        &harness.service,
        1,
        1,
        |event| matches!(event, envelope::Payload::ProviderState(state) if state.state == ProviderConnectionState::Recovering as i32),
    );
    assert!(matches!(
        recovering,
        envelope::Payload::ProviderState(state)
            if state.provider == "coinbase" && (1..=2).contains(&state.generation)
    ));
    assert_eq!(
        harness
            .generations
            .recv_timeout(Duration::from_secs(1))
            .expect("second realtime generation")
            .0
            .get(),
        2
    );
    harness
        .actions
        .send(FixtureRealtimeAction::Connected)
        .expect("reconnect fixture");
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(2, "2.10", 2)))
        .expect("resumed live trade");
    harness
        .actions
        .send(FixtureRealtimeAction::Heartbeat)
        .expect("recovery heartbeat");
    for (client, consumer) in [(1, 1), (2, 2)] {
        let resumed = poll_until(&harness.service, client, consumer, |event| {
            is_live_update(event, 1, 2, 210)
        });
        assert!(is_live_update(&resumed, 1, 2, 210));
    }
    harness.service.detach(1).expect("first client detaches");
    assert!(
        harness
            .stops
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "shared realtime remains for the second consumer"
    );
    harness.service.detach(2).expect("second client detaches");
    assert_eq!(
        harness
            .stops
            .recv_timeout(Duration::from_secs(1))
            .expect("last consumer stops realtime")
            .0
            .get(),
        2
    );
}

#[test]
fn markets_live_retains_and_advances_the_hot_series_without_ui_consumers() {
    let harness = MarketService::start_fixture_realtime(vec![history_bar()])
        .expect("realtime fixture starts");
    harness.service.attach(1).expect("client attaches");
    harness
        .service
        .register_consumer(1, 1, 1)
        .expect("consumer registers");
    harness
        .service
        .set_demand(1, 1, 1, &btc())
        .expect("history demand is accepted");
    poll_until(
        &harness.service,
        1,
        1,
        |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if !snapshot.forming),
    );
    assert_eq!(
        harness
            .generations
            .recv_timeout(Duration::from_secs(1))
            .expect("realtime generation starts")
            .0
            .get(),
        1
    );
    assert_eq!(
        harness
            .configured_products
            .recv_timeout(Duration::from_secs(1))
            .expect("BTC product set configures"),
        ["BTC-USD"]
    );
    harness
        .actions
        .send(FixtureRealtimeAction::Connected)
        .expect("realtime connects");
    harness
        .service
        .set_resource_mode(ResourceMode::MarketsLive)
        .expect("markets-live mode applies");
    harness.service.detach(1).expect("desktop detaches");
    assert!(
        harness
            .stops
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "markets-live mode retains the provider session"
    );

    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(2, "2.00", 3)))
        .expect("detached live trade arrives");

    harness.service.attach(2).expect("new desktop attaches");
    harness
        .service
        .register_consumer(2, 1, 2)
        .expect("new consumer registers");
    harness
        .service
        .set_demand(2, 2, 1, &btc())
        .expect("hot-series demand is accepted");
    let hot = poll_until(
        &harness.service,
        2,
        2,
        |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.forming),
    );
    assert!(matches!(
        hot,
        envelope::Payload::SeriesSnapshot(snapshot)
            if snapshot.bars.last().is_some_and(|bar| bar.close == 200)
    ));
    assert_eq!(harness.history_fetches.load(Ordering::Acquire), 1);

    harness.service.detach(2).expect("new desktop detaches");
    harness
        .service
        .set_resource_mode(ResourceMode::Warm)
        .expect("warm mode applies");
    assert_eq!(
        harness
            .stops
            .recv_timeout(Duration::from_secs(1))
            .expect("warm mode releases realtime")
            .0
            .get(),
        1
    );
}

#[test]
fn existing_chart_demand_resumes_after_offline_account_suspension() {
    let harness = MarketService::start_fixture_realtime(vec![history_bar()])
        .expect("realtime fixture starts");
    harness.service.attach(1).expect("client attaches");
    harness
        .service
        .register_consumer(1, 1, 1)
        .expect("consumer registers");
    harness
        .service
        .set_demand(1, 1, 1, &btc())
        .expect("chart demand is accepted");
    poll_until(&harness.service, 1, 1, |event| {
        matches!(event, envelope::Payload::SeriesSnapshot(_))
    });
    expect_realtime_generation(&harness, "initial realtime starts", 1);
    expect_configured_products(&harness, "initial product set configures", &["BTC-USD"]);
    harness
        .actions
        .send(FixtureRealtimeAction::Connected)
        .expect("initial realtime connects");
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(2, "2.00", 1)))
        .expect("initial trade arrives");
    let initial = poll_until(&harness.service, 1, 1, |event| {
        is_live_update(event, 1, 1, 200)
    });
    assert!(is_live_update(&initial, 1, 1, 200));

    harness
        .service
        .set_resource_mode(ResourceMode::OfflineSuspended)
        .expect("account suspension applies");
    assert_eq!(
        harness
            .stops
            .recv_timeout(Duration::from_secs(1))
            .expect("the established provider generation stops")
            .0
            .get(),
        1
    );
    assert!(matches!(
        poll_until(&harness.service, 1, 1, |event| matches!(
            event,
            envelope::Payload::SeriesState(state)
                if state.state == SeriesLoadState::Partial as i32
        )),
        envelope::Payload::SeriesState(state)
            if state.detail.as_deref() == Some("Market access is suspended until the account is ready")
    ));

    harness
        .service
        .set_resource_mode(ResourceMode::Warm)
        .expect("authenticated warm mode resumes existing demand");
    expect_realtime_generation(&harness, "replacement realtime starts", 2);
    expect_configured_products(&harness, "replacement product set configures", &["BTC-USD"]);
    harness
        .actions
        .send(FixtureRealtimeAction::Connected)
        .expect("replacement realtime connects");
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade(3, "2.50", 2)))
        .expect("post-login trade crosses the next candle boundary");
    let resumed = poll_until(&harness.service, 1, 1, |event| {
        is_live_update(event, 1, 2, 250)
    });
    assert!(is_live_update(&resumed, 1, 2, 250));
    assert!(
        harness.history_fetches.load(Ordering::Acquire) >= 2,
        "resume requests fresh covering history"
    );
}

#[test]
fn history_completed_after_account_suspension_cannot_publish_into_restored_access() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let (release_tx, release_rx) = mpsc::sync_channel(2);
    let (action_tx, action_rx) = mpsc::sync_channel(2);
    let (generation_tx, generation_rx) = mpsc::sync_channel(2);
    let (stop_tx, stop_rx) = mpsc::sync_channel(2);
    let service = MarketService::start_with_sources(
        ControlledHistory {
            fetches: Arc::clone(&fetches),
            release: release_rx,
        },
        Some(Box::new(FixtureRealtime {
            actions: action_rx,
            generations: generation_tx,
            stops: stop_tx,
            configured_products: None,
        })),
        None,
    )
    .expect("controlled lifecycle fixture starts");
    service.attach(1).expect("client attaches");
    service
        .register_consumer(1, 1, 1)
        .expect("consumer registers");
    service
        .set_demand(1, 1, 1, &btc())
        .expect("initial history starts");
    assert_eq!(
        generation_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("initial realtime starts")
            .0
            .get(),
        1
    );
    action_tx
        .send(FixtureRealtimeAction::Connected)
        .expect("initial realtime connects");
    let deadline = Instant::now() + Duration::from_secs(1);
    while fetches.load(Ordering::Acquire) != 1 {
        assert!(Instant::now() < deadline, "initial history did not start");
        thread::yield_now();
    }

    service
        .set_resource_mode(ResourceMode::OfflineSuspended)
        .expect("account suspension applies");
    stop_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("initial realtime stops");
    service
        .set_resource_mode(ResourceMode::Warm)
        .expect("account access returns");
    assert_eq!(
        generation_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("replacement realtime starts")
            .0
            .get(),
        2
    );

    release_tx
        .send(())
        .expect("pre-suspension history returns successfully");
    let deadline = Instant::now() + Duration::from_secs(1);
    while fetches.load(Ordering::Acquire) != 2 {
        assert!(
            Instant::now() < deadline,
            "fresh post-suspension history did not start"
        );
        thread::yield_now();
    }
    while let Some(event) = service.poll_event(1, 1).expect("consumer polls") {
        assert!(
            !matches!(event, envelope::Payload::SeriesSnapshot(_)),
            "pre-suspension history reached the restored consumer"
        );
    }

    release_tx
        .send(())
        .expect("post-suspension history completes");
    assert!(matches!(
        poll_until(&service, 1, 1, |event| matches!(
            event,
            envelope::Payload::SeriesSnapshot(_)
        )),
        envelope::Payload::SeriesSnapshot(_)
    ));
    drop(action_tx);
}

#[test]
fn symbol_and_interval_switch_reuses_the_shared_realtime_session() {
    let harness = MarketService::start_fixture_realtime(vec![history_bar()])
        .expect("realtime fixture starts");
    harness.service.attach(1).expect("client attaches");
    harness
        .service
        .register_consumer(1, 1, 1)
        .expect("consumer registers");
    harness
        .service
        .set_demand(1, 1, 1, &btc())
        .expect("BTC demand is accepted");
    poll_until(
        &harness.service,
        1,
        1,
        |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 1),
    );
    expect_realtime_generation(&harness, "shared realtime starts", 1);
    expect_configured_products(&harness, "BTC product set configures", &["BTC-USD"]);
    harness
        .actions
        .send(FixtureRealtimeAction::Connected)
        .expect("realtime connects");

    let eth_five = selected_series("instrument:coinbase:eth:usd", 300);
    harness
        .service
        .set_demand(1, 1, 2, &eth_five)
        .expect("ETH five-minute demand is accepted");
    let history = poll_until(
        &harness.service,
        1,
        1,
        |event| matches!(event, envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == 2 && !snapshot.forming),
    );
    assert!(matches!(
        history,
        envelope::Payload::SeriesSnapshot(snapshot)
            if snapshot.series.as_ref().is_some_and(|series| {
                series.instrument_id == "instrument:coinbase:eth:usd"
                    && series.cadence_value == 300
            })
    ));
    assert!(
        harness
            .generations
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "symbol changes keep the provider session alive"
    );
    expect_configured_products(&harness, "ETH joins the live socket", &["ETH-USD"]);
    harness
        .actions
        .send(FixtureRealtimeAction::Trade(trade_for(
            "ETH-USD", 10, "2000.00", 1,
        )))
        .expect("ETH live trade");
    let live = poll_until(&harness.service, 1, 1, |event| {
        is_live_update(event, 2, 1, 200_000)
    });
    assert!(is_live_update(&live, 2, 1, 200_000));

    let eth_fifteen = selected_series("instrument:coinbase:eth:usd", 900);
    harness
        .service
        .set_demand(1, 1, 3, &eth_fifteen)
        .expect("ETH interval switch is accepted");
    poll_until(&harness.service, 1, 1, |event| {
        matches!(
            event,
            envelope::Payload::SeriesSnapshot(snapshot)
                if snapshot.generation == 3 && !snapshot.forming
        )
    });
    assert!(
        harness
            .configured_products
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "same-product cadence changes do not reconfigure realtime"
    );
    assert!(
        harness
            .generations
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "same-product cadence changes reuse the realtime generation"
    );
    harness.service.detach(1).expect("client detaches");
}

#[test]
fn realtime_queue_overflow_closes_and_restarts_the_provider_generation() {
    let (action_tx, action_rx) = mpsc::sync_channel(4);
    let (generation_tx, generation_rx) = mpsc::sync_channel(4);
    let (stop_tx, _stop_rx) = mpsc::sync_channel(4);
    let (control_tx, control_rx) = mpsc::sync_channel(1);
    let (event_tx, event_rx) = mpsc::sync_channel(1);
    let overflow = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let worker_overflow = Arc::clone(&overflow);
    let worker_stop = Arc::clone(&stop);
    let worker = thread::spawn(move || {
        let lifecycle = ProviderRuntimeLifecycle::default();
        run_realtime_worker(
            Box::new(FixtureRealtime {
                actions: action_rx,
                generations: generation_tx,
                stops: stop_tx,
                configured_products: None,
            }),
            &control_rx,
            &event_tx,
            &worker_overflow,
            &worker_stop,
            PROVIDER_RECONNECT_DELAY,
            &lifecycle,
        );
    });

    control_tx
        .send(RealtimeControl::Start(vec![RealtimeProduct {
            symbol: "BTC-USD".to_string(),
            price_scale: 2,
            quantity_scale: 8,
        }]))
        .expect("start realtime worker");
    assert!(matches!(
        event_rx.recv_timeout(Duration::from_secs(1)),
        Ok(RealtimeEvent::Connecting(generation)) if generation.0.get() == 1
    ));
    assert_eq!(
        generation_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first fixture generation")
            .0
            .get(),
        1
    );
    action_tx
        .send(FixtureRealtimeAction::Connected)
        .expect("queue connected state");
    action_tx
        .send(FixtureRealtimeAction::Trade(trade(2, "2.00", 1)))
        .expect("overflow realtime queue");
    let overflow_deadline = Instant::now() + Duration::from_secs(1);
    while !overflow.load(Ordering::Acquire) {
        assert!(
            Instant::now() < overflow_deadline,
            "realtime overflow timed out"
        );
        thread::yield_now();
    }
    assert!(matches!(
        event_rx.recv_timeout(Duration::from_secs(1)),
        Ok(RealtimeEvent::Connected(generation)) if generation.0.get() == 1
    ));
    assert!(matches!(
        event_rx.recv_timeout(Duration::from_secs(1)),
        Ok(RealtimeEvent::Disconnected(generation)) if generation.0.get() == 1
    ));
    assert!(matches!(
        event_rx.recv_timeout(Duration::from_secs(1)),
        Ok(RealtimeEvent::Connecting(generation)) if generation.0.get() == 2
    ));

    stop.store(true, Ordering::Release);
    drop(action_tx);
    drop(control_tx);
    while event_rx.recv_timeout(Duration::from_millis(50)).is_ok() {}
    worker.join().expect("realtime worker exits");
}

fn coinbase_bar_series(market: &str) -> BarSeriesKey {
    let instrument = if market == "ETH-USD" {
        "instrument:coinbase:eth:usd"
    } else {
        "instrument:coinbase:btc:usd"
    };
    BarSeriesKey {
        provider_id: "coinbase".to_string(),
        instrument_id: instrument.to_string(),
        entitlement_id: ENTITLEMENT_CLASS.to_string(),
        period: BarPeriod::time(60).expect("period"),
        definition_version: 1,
    }
}

fn coinbase_depth_snapshot(
    instrument_id: &str,
    session_generation: u64,
    source_sequence: u64,
) -> DepthSnapshot {
    DepthSnapshot {
        metadata: EventMetadata {
            provider_id: "coinbase".to_string(),
            instrument_id: instrument_id.to_string(),
            entitlement_id: ENTITLEMENT_CLASS.to_string(),
            source_sequence,
            session_generation,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(20),
                provider_unix_nanos: None,
                received_unix_nanos: 21,
            },
        },
        bids: vec![DepthLevel {
            price: 20_000,
            quantity: 7,
            order_count: Some(3),
        }],
        asks: vec![DepthLevel {
            price: 20_025,
            quantity: 4,
            order_count: Some(2),
        }],
    }
}

fn stalled_book_key(series: &BarSeriesKey) -> (String, String) {
    (series.provider_id.clone(), series.instrument_id.clone())
}

fn backdate_depth_watch(coordinator: &mut Coordinator<'_>, series: &BarSeriesKey) {
    let key = stalled_book_key(series);
    let watch = &mut coordinator
        .order_books
        .get_mut(&key)
        .expect("stalled book exists")
        .watch;
    watch.awaited_since = Instant::now().checked_sub(Duration::from_hours(1));
}

fn install_coinbase_depth(
    coordinator: &mut Coordinator<'_>,
    client_id: ClientId,
    consumer_id: ConsumerId,
    series: &BarSeriesKey,
) {
    coordinator
        .engine
        .register_consumer(
            ConsumerIdentity {
                client_id,
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    coordinator
        .engine
        .set_series_demand_with_streams(
            consumer_id,
            GenerationId(id(3).expect("generation")),
            series,
            chart_stream_requirements(series),
        )
        .expect("depth demand installs");
    coordinator
        .engine
        .set_visibility(consumer_id, true)
        .expect("consumer is foreground");
    coordinator
        .install_provider_instrument(&coinbase_instrument(series))
        .expect("instrument installs");
    coordinator.live.insert(
        series.clone(),
        LiveHandoff::try_new(
            series,
            ProviderGeneration(id(1).expect("generation")),
            &coinbase_instrument(series),
        )
        .expect("live handoff"),
    );
}

fn demand_legs(realtime: &Receiver<RealtimeControl>) -> Vec<BTreeSet<String>> {
    let mut legs = Vec::new();
    while let Ok(RealtimeControl::Start(products)) = realtime.try_recv() {
        legs.push(
            products
                .into_iter()
                .map(|product| product.symbol)
                .collect::<BTreeSet<_>>(),
        );
    }
    legs
}

#[test]
fn stalled_coinbase_book_resubscribes_only_the_stalled_product() {
    let client_id = ClientId(id(7).expect("client"));
    let btc_consumer = ConsumerId(id(9).expect("consumer"));
    let eth_consumer = ConsumerId(id(10).expect("consumer"));
    let btc = coinbase_bar_series("BTC-USD");
    let eth = coinbase_bar_series("ETH-USD");
    let engine = configured_engine().expect("engine");
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, realtime_rx) = mpsc::sync_channel(8);
    let stop = Arc::new(AtomicBool::new(false));
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &stop,
        btc_consumer,
        &btc,
    );
    install_coinbase_depth(&mut coordinator, client_id, btc_consumer, &btc);
    coordinator
        .events
        .insert(eth_consumer, ConsumerEvents::default());
    install_coinbase_depth(&mut coordinator, client_id, eth_consumer, &eth);
    // The healthy book completes its snapshot while BTC-USD stalls.
    coordinator.provider_depth(
        "coinbase",
        1,
        &coinbase_depth_snapshot("instrument:coinbase:eth:usd", 1, 11),
    );
    backdate_depth_watch(&mut coordinator, &btc);
    backdate_depth_watch(&mut coordinator, &btc);
    coordinator.recover_stalled_depth_snapshots();
    // The dance excludes only the stalled product, then restores the set.
    assert_eq!(
        demand_legs(&realtime_rx),
        vec![
            BTreeSet::from(["ETH-USD".to_string()]),
            BTreeSet::from(["BTC-USD".to_string(), "ETH-USD".to_string()]),
        ]
    );
    let btc_key = stalled_book_key(&btc);
    assert_eq!(
        coordinator
            .order_books
            .get(&btc_key)
            .expect("stalled book")
            .watch
            .resubscribes,
        1
    );
    // The healthy book is never fenced by another product's stall.
    let eth_key = stalled_book_key(&eth);
    assert_eq!(
        coordinator
            .order_books
            .get(&eth_key)
            .expect("healthy book")
            .watch
            .resubscribes,
        0
    );
    assert_eq!(
        coordinator
            .order_books
            .get(&eth_key)
            .expect("healthy book")
            .book
            .state(),
        CanonicalOrderBookState::Ready
    );
}

#[test]
fn depth_watchdog_goes_quiet_after_bounded_resubscribes() {
    let client_id = ClientId(id(7).expect("client"));
    let consumer_id = ConsumerId(id(9).expect("consumer"));
    let series = coinbase_bar_series("BTC-USD");
    let engine = configured_engine().expect("engine");
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, realtime_rx) = mpsc::sync_channel(8);
    let stop = Arc::new(AtomicBool::new(false));
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &stop,
        consumer_id,
        &series,
    );
    install_coinbase_depth(&mut coordinator, client_id, consumer_id, &series);
    let key = stalled_book_key(&series);
    coordinator
        .order_books
        .get_mut(&key)
        .expect("stalled book")
        .watch
        .resubscribes = super::coordinator::Coordinator::MAXIMUM_DEPTH_SNAPSHOT_RESUBSCRIBES;
    backdate_depth_watch(&mut coordinator, &series);
    coordinator.recover_stalled_depth_snapshots();
    assert!(realtime_rx.try_recv().is_err());
}

#[test]
fn ready_books_never_trigger_a_resubscribe() {
    let client_id = ClientId(id(7).expect("client"));
    let consumer_id = ConsumerId(id(9).expect("consumer"));
    let series = coinbase_bar_series("BTC-USD");
    let engine = configured_engine().expect("engine");
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, realtime_rx) = mpsc::sync_channel(8);
    let stop = Arc::new(AtomicBool::new(false));
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &stop,
        consumer_id,
        &series,
    );
    install_coinbase_depth(&mut coordinator, client_id, consumer_id, &series);
    coordinator.provider_depth(
        "coinbase",
        1,
        &coinbase_depth_snapshot("instrument:coinbase:btc:usd", 1, 11),
    );
    backdate_depth_watch(&mut coordinator, &series);
    backdate_depth_watch(&mut coordinator, &series);
    coordinator.recover_stalled_depth_snapshots();
    assert!(realtime_rx.try_recv().is_err());
    let key = stalled_book_key(&series);
    assert_eq!(
        coordinator
            .order_books
            .get(&key)
            .expect("ready book")
            .watch
            .resubscribes,
        0
    );
}

#[test]
fn rithmic_stalls_never_trigger_a_resubscribe() {
    let client_id = ClientId(id(7).expect("client"));
    let consumer_id = ConsumerId(id(9).expect("consumer"));
    let series = rithmic_series_key("instrument:rithmic:CME:MNQU6", "rithmic-test:CME:MNQU6");
    let mut engine = configured_engine().expect("engine");
    engine
        .register_consumer(
            ConsumerIdentity {
                client_id,
                workspace_id: WorkspaceId(id(1).expect("workspace")),
                consumer_id,
            },
            true,
        )
        .expect("consumer registers");
    engine
        .set_series_demand_with_streams(
            consumer_id,
            GenerationId(id(3).expect("generation")),
            &series,
            chart_stream_requirements(&series),
        )
        .expect("depth demand installs");
    let (history_tx, _history_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(1);
    let (realtime_tx, realtime_rx) = mpsc::sync_channel(8);
    let stop = Arc::new(AtomicBool::new(false));
    let mut coordinator = retained_history_coordinator(
        engine,
        &history_tx,
        &storage_tx,
        &realtime_tx,
        &stop,
        consumer_id,
        &series,
    );
    coordinator
        .install_provider_instrument(&provider_instrument(7, 2))
        .expect("instrument installs");
    backdate_depth_watch(&mut coordinator, &series);
    coordinator.recover_stalled_depth_snapshots();
    assert!(realtime_rx.try_recv().is_err());
}
