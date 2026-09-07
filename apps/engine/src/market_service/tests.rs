use super::*;

fn rithmic_series() -> BarSeriesKey {
    BarSeriesKey {
        provider_id: "rithmic".to_string(),
        instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
        entitlement_id: "rithmic-test:CME:MNQ".to_string(),
        period: BarPeriod::time(60).expect("valid period"),
        definition_version: 1,
    }
}

fn rithmic_instrument(
    symbol: &str,
    instrument_id: &str,
    entitlement_id: &str,
    session_generation: u64,
    selection_generation: u64,
) -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: "rithmic".to_string(),
        session_generation,
        selection_generation,
        instrument_id: instrument_id.to_string(),
        provider_symbol: symbol.to_string(),
        display_symbol: symbol.to_string(),
        venue_id: "CME".to_string(),
        price_scale: 2,
        quantity_scale: 0,
        entitlement_id: entitlement_id.to_string(),
    }
}

fn hyperliquid_series() -> BarSeriesKey {
    BarSeriesKey {
        provider_id: "hyperliquid".to_string(),
        instrument_id: "instrument:hyperliquid:BTC".to_string(),
        entitlement_id: "hyperliquid-public".to_string(),
        period: BarPeriod::time(60).expect("valid period"),
        definition_version: 1,
    }
}

fn hyperliquid_instrument(session_generation: u64) -> InstallProviderInstrument {
    let series = hyperliquid_series();
    InstallProviderInstrument {
        provider: series.provider_id,
        session_generation,
        selection_generation: 1,
        instrument_id: series.instrument_id,
        provider_symbol: "BTC".to_string(),
        display_symbol: "BTC".to_string(),
        venue_id: "Hyperliquid".to_string(),
        price_scale: 8,
        quantity_scale: 8,
        entitlement_id: series.entitlement_id,
    }
}

fn hyperliquid_bar(sequence: u64, open_nanos: i64, close: i64) -> MarketBar {
    MarketBar {
        source_sequence: sequence,
        exchange_timestamp_seconds: open_nanos.div_euclid(1_000_000_000),
        exchange_timestamp_unix_nanos: open_nanos,
        open: 10_000,
        high: 10_500,
        low: 9_500,
        close,
        volume: 5,
    }
}

fn live_candle(open_nanos: i64, high: i64, close: i64, volume: i64) -> HyperliquidLiveCandle {
    HyperliquidLiveCandle {
        open_nanos,
        open: 10_000,
        high,
        low: 9_500,
        close,
        volume,
    }
}

fn market_bar() -> MarketBar {
    MarketBar {
        source_sequence: 10,
        exchange_timestamp_seconds: 60,
        exchange_timestamp_unix_nanos: 60_000_000_000,
        open: 10_000,
        high: 10_500,
        low: 9_500,
        close: 10_100,
        volume: 5,
    }
}

fn market_trade(sequence: u64, timestamp: i64, price: i64, quantity: i64) -> MarketTrade {
    MarketTrade {
        metadata: axiusflow_market_data::EventMetadata {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
            entitlement_id: "rithmic-test:CME:MNQ".to_string(),
            session_generation: 1,
            source_sequence: sequence,
            timestamps: axiusflow_market_data::QualifiedTimestamp {
                exchange_unix_nanos: Some(timestamp),
                provider_unix_nanos: None,
                received_unix_nanos: timestamp,
            },
        },
        trade_id: format!("trade-{sequence}"),
        price,
        quantity,
        aggressor: axiusflow_market_data::AggressorSide::Unknown,
    }
}

fn hyperliquid_trade(sequence: u64, timestamp: i64, price: i64, quantity: i64) -> MarketTrade {
    MarketTrade {
        metadata: axiusflow_market_data::EventMetadata {
            provider_id: "hyperliquid".to_string(),
            instrument_id: hyperliquid_series().instrument_id,
            entitlement_id: hyperliquid_series().entitlement_id,
            session_generation: 1,
            source_sequence: sequence,
            timestamps: axiusflow_market_data::QualifiedTimestamp {
                exchange_unix_nanos: Some(timestamp),
                provider_unix_nanos: Some(timestamp),
                received_unix_nanos: timestamp,
            },
        },
        trade_id: format!("hl-trade-{sequence}"),
        price,
        quantity,
        aggressor: axiusflow_market_data::AggressorSide::Unknown,
    }
}

#[test]
fn provider_event_wake_uses_the_coordinator_command_lane_without_polling() {
    let (commands, receiver) = mpsc::sync_channel(1);
    let wake = ProviderCoordinatorWake::new(commands);
    wake.notify();
    assert!(matches!(receiver.try_recv(), Ok(Command::ProviderWake)));

    // If the command lane is already full, dropping the redundant wake is
    // safe because the queued command itself will interrupt recv_timeout.
    let (commands, receiver) = mpsc::sync_channel(1);
    let wake = ProviderCoordinatorWake::new(commands.clone());
    commands
        .try_send(Command::ProviderWake)
        .expect("fixture fills command lane");
    wake.notify();
    assert!(matches!(receiver.try_recv(), Ok(Command::ProviderWake)));
}

#[test]
fn provider_transport_rtt_is_generation_fenced_and_cleared_on_recovery() {
    let lifecycle = ProviderRuntimeLifecycle::default();
    lifecycle.observe_generation(7, false);
    assert_eq!(lifecycle.transport_rtt_nanos(), None);
    lifecycle.observe_transport_rtt(7, 18_400_000);
    assert_eq!(lifecycle.transport_rtt_nanos(), Some(18_400_000));

    lifecycle.observe_generation(7, true);
    assert_eq!(lifecycle.transport_rtt_nanos(), None);
    lifecycle.observe_transport_rtt(7, 19_000_000);
    assert_eq!(lifecycle.transport_rtt_nanos(), None);

    lifecycle.observe_generation(7, false);
    lifecycle.observe_transport_rtt(7, 20_000_000);
    assert_eq!(lifecycle.transport_rtt_nanos(), Some(20_000_000));
    lifecycle.observe_generation(8, false);
    assert_eq!(lifecycle.transport_rtt_nanos(), None);
}

#[test]
fn rithmic_fixed_period_updates_and_rolls_the_forming_bar() {
    use super::realtime::{started_rithmic_bar, updated_rithmic_bar};
    let current = market_bar();
    let updated = updated_rithmic_bar(
        current,
        &market_trade(1, 61_000_000_000, 10_700, 3),
        61_000_000_000,
    )
    .expect("update");
    assert_eq!(updated.source_sequence, current.source_sequence);
    assert_eq!(updated.high, 10_700);
    assert_eq!(updated.close, 10_700);
    assert_eq!(updated.volume, 8);

    let next = started_rithmic_bar(
        updated,
        &market_trade(2, 120_000_000_000, 10_600, 2),
        120_000_000_000,
    )
    .expect("roll");
    assert_eq!(next.source_sequence, current.source_sequence + 1);
    assert_eq!(next.open, 10_600);
    assert_eq!(next.volume, 2);
}

#[test]
fn provider_validation_accepts_rithmic_and_hyperliquid() {
    for provider in ["rithmic", "hyperliquid"] {
        assert!(
            validate_provider_search(&SearchProviderInstruments {
                consumer_id: 1,
                search_generation: 1,
                provider: provider.to_string(),
                query: "BTC".to_string(),
                maximum_results: 10,
            })
            .is_ok(),
            "{provider} search must validate"
        );
    }
    assert!(
        validate_provider_search(&SearchProviderInstruments {
            consumer_id: 1,
            search_generation: 1,
            provider: "unsupported".to_string(),
            query: "BTC".to_string(),
            maximum_results: 10,
        })
        .is_err()
    );
}

#[test]
fn rithmic_bbo_clear_replaces_the_retained_side_instead_of_freezing_it() {
    let instrument = InstallProviderInstrument {
        provider: "rithmic".to_string(),
        session_generation: 7,
        selection_generation: 2,
        instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
        provider_symbol: "MNQU6".to_string(),
        display_symbol: "MNQ Sep 2026".to_string(),
        venue_id: "CME".to_string(),
        price_scale: 2,
        quantity_scale: 0,
        entitlement_id: "rithmic-test:CME:MNQ".to_string(),
    };
    let instrument_id = instrument.instrument_id.clone();
    let entitlement_id = instrument.entitlement_id.clone();
    let metadata = |source_sequence| axiusflow_market_data::EventMetadata {
        provider_id: "rithmic".to_string(),
        instrument_id: instrument_id.clone(),
        entitlement_id: entitlement_id.clone(),
        source_sequence,
        session_generation: 7,
        timestamps: axiusflow_market_data::QualifiedTimestamp {
            exchange_unix_nanos: Some(1_800_000_000_000_000_000),
            provider_unix_nanos: None,
            received_unix_nanos: 1_800_000_000_000_000_001,
        },
    };
    let mut book = ProviderOrderBook::new(instrument);
    assert!(book.install_top_of_book(&TopOfBookQuote {
        metadata: metadata(1),
        bid: Some(DepthLevel {
            price: 510_000,
            quantity: 10,
            order_count: Some(2),
        }),
        ask: Some(DepthLevel {
            price: 510_025,
            quantity: 12,
            order_count: Some(3),
        }),
    }));
    assert!(book.install_top_of_book(&TopOfBookQuote {
        metadata: metadata(2),
        bid: Some(DepthLevel {
            price: 510_000,
            quantity: 11,
            order_count: Some(2),
        }),
        ask: None,
    }));
    let retained = book.top_of_book.expect("latest BBO is retained");
    assert_eq!(retained.metadata.source_sequence, 2);
    assert_eq!(retained.bid.map(|level| level.quantity), Some(11));
    assert!(retained.ask.is_none());
}

#[test]
fn restored_hot_series_requires_the_rithmic_scope() {
    let hot = HotSeries {
        provider: "rithmic".to_string(),
        market: "MNQ".to_string(),
        interval_seconds: 60,
        score: 1,
        last_used_unix_seconds: 1,
        provider_watermark: 1,
        series_watermark: 0,
        viewport_start_unix_nanos: None,
        viewport_end_unix_nanos: None,
        account_id: RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID.to_string(),
        instrument_id: rithmic_series().instrument_id,
        entitlement_id: rithmic_series().entitlement_id,
        cadence: SeriesCadence::FixedSeconds as i32,
        cadence_value: 60,
        definition_revision: 1,
        pinned: false,
        workspace_ids: vec![1],
        coverage_start_unix_nanos: None,
        coverage_end_unix_nanos: None,
        provider_symbol: "MNQ".to_string(),
        venue_id: "CME".to_string(),
        display_symbol: "MNQ".to_string(),
        price_scale: 2,
        quantity_scale: 0,
    };
    let warm = warm_series(&hot).expect("valid warm series");
    assert_eq!(warm.series, rithmic_series());

    let mut unsupported = hot;
    unsupported.provider = "unsupported".to_string();
    assert!(warm_series(&unsupported).is_err());
}

#[test]
fn restored_hot_series_accepts_the_hyperliquid_scope() {
    let hot = HotSeries {
        provider: "hyperliquid".to_string(),
        market: "BTC".to_string(),
        interval_seconds: 60,
        score: 1,
        last_used_unix_seconds: 1,
        provider_watermark: 1,
        series_watermark: 0,
        viewport_start_unix_nanos: None,
        viewport_end_unix_nanos: None,
        account_id: HYPERLIQUID_PUBLIC_ACCOUNT_ID.to_string(),
        instrument_id: hyperliquid_series().instrument_id,
        entitlement_id: hyperliquid_series().entitlement_id,
        cadence: SeriesCadence::FixedSeconds as i32,
        cadence_value: 60,
        definition_revision: 1,
        pinned: false,
        workspace_ids: vec![1],
        coverage_start_unix_nanos: None,
        coverage_end_unix_nanos: None,
        provider_symbol: "BTC".to_string(),
        venue_id: "Hyperliquid".to_string(),
        display_symbol: "BTC".to_string(),
        price_scale: 8,
        quantity_scale: 8,
    };
    let warm = warm_series(&hot).expect("valid warm series");
    assert_eq!(warm.series, hyperliquid_series());

    // The credential-free feed has exactly one scope: anything else is a
    // different trust domain, not a fallback identity.
    let mut wrong_scope = hot;
    wrong_scope.account_id = RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID.to_string();
    assert!(warm_series(&wrong_scope).is_err());
}

fn hyperliquid_handoff() -> HyperliquidLiveHandoff {
    HyperliquidLiveHandoff::new(
        hyperliquid_series(),
        ProviderGeneration(NonZeroU64::new(1).expect("non-zero generation")),
        "BTC".to_string(),
        "1m".to_string(),
    )
}

#[test]
fn hyperliquid_handoff_seeds_forming_and_revises_it_in_place() {
    let mut handoff = hyperliquid_handoff();
    let closed = hyperliquid_bar(10, 60_000_000_000, 10_100);
    let forming = FormingBar {
        bar: hyperliquid_bar(11, 120_000_000_000, 10_150),
        trades: None,
    };
    handoff.seed(8, 8, &[closed], Some(forming)).expect("seed");
    handoff.connected = true;

    // History already installed the closed tail, so the seed publishes only
    // the open period the chart opens on.
    match handoff.take_publication().expect("seed tail") {
        LiveSeriesPublication::Tails(bars) => {
            assert_eq!(bars.len(), 1);
            assert_eq!(bars[0].source_sequence, 11);
            assert_eq!(bars[0].close, 10_150);
        }
    }

    // A same-period replacement revises the forming bar in place: the
    // sequence stays engine-owned, so the consumer sees a revise, never a
    // new bar.
    handoff
        .accept_candle(&live_candle(120_000_000_000, 10_600, 10_200, 7))
        .expect("accept");
    match handoff.take_publication().expect("revised tail") {
        LiveSeriesPublication::Tails(bars) => {
            assert_eq!(bars.len(), 1);
            assert_eq!(bars[0].source_sequence, 11);
            assert_eq!(bars[0].exchange_timestamp_unix_nanos, 120_000_000_000);
            assert_eq!(bars[0].high, 10_600);
            assert_eq!(bars[0].close, 10_200);
            assert_eq!(bars[0].volume, 7);
        }
    }
    assert!(handoff.take_publication().is_none());
}

#[test]
fn hyperliquid_handoff_rolls_the_forming_bar_exactly_once() {
    let mut handoff = hyperliquid_handoff();
    handoff
        .seed(
            8,
            8,
            &[hyperliquid_bar(10, 60_000_000_000, 10_100)],
            Some(FormingBar {
                bar: hyperliquid_bar(11, 120_000_000_000, 10_150),
                trades: None,
            }),
        )
        .expect("seed");
    handoff.connected = true;
    assert!(handoff.take_publication().is_some());

    // A newer period completes the forming bar and opens the next one. The
    // engine accepts the new period as one incremental append; covering
    // history stays shared and is never rebuilt by the handoff.
    let update = live_candle(180_000_000_000, 10_300, 10_250, 2);
    handoff.accept_candle(&update).expect("accept");
    match handoff.take_publication().expect("roll") {
        LiveSeriesPublication::Tails(bars) => {
            assert_eq!(bars.len(), 1);
            assert_eq!(bars[0].source_sequence, 12);
            assert_eq!(bars[0].exchange_timestamp_unix_nanos, 180_000_000_000);
        }
    }

    // Redelivery of the same update resolves to the identical bar: the
    // sequence is a function of the open timestamp, not arrival order.
    handoff.accept_candle(&update).expect("redeliver");
    match handoff.take_publication().expect("redelivered tail") {
        LiveSeriesPublication::Tails(bars) => {
            assert_eq!(bars.len(), 1);
            assert_eq!(bars[0].source_sequence, 12);
            assert_eq!(bars[0].exchange_timestamp_unix_nanos, 180_000_000_000);
        }
    }
}

#[test]
fn hyperliquid_handoff_rejects_a_missing_fixed_time_bucket() {
    let mut handoff = hyperliquid_handoff();
    handoff
        .seed(
            8,
            8,
            &[hyperliquid_bar(10, 60_000_000_000, 10_100)],
            Some(FormingBar {
                bar: hyperliquid_bar(11, 120_000_000_000, 10_150),
                trades: None,
            }),
        )
        .expect("seed");
    assert_eq!(
        handoff.accept_candle(&live_candle(240_000_000_000, 10_300, 10_250, 2)),
        Err("Hyperliquid live candle has a time gap".to_string())
    );
}

#[test]
fn hyperliquid_handoff_bounds_pre_history_candles_and_resets_on_reconnect() {
    let mut handoff = hyperliquid_handoff();
    // Live updates that arrive during the history fetch wait in a bounded
    // buffer, then replay in open-timestamp order once the seam seeds.
    handoff
        .accept_candle(&live_candle(120_000_000_000, 10_600, 10_200, 7))
        .expect("buffer");
    handoff
        .seed(8, 8, &[hyperliquid_bar(10, 60_000_000_000, 10_100)], None)
        .expect("seed");
    handoff.connected = true;
    match handoff.take_publication().expect("replayed tail") {
        LiveSeriesPublication::Tails(bars) => {
            assert_eq!(bars.len(), 1);
            assert_eq!(bars[0].source_sequence, 11);
            assert_eq!(bars[0].close, 10_200);
        }
    }

    // The buffer is a bound, not a queue that grows with the outage: past
    // it the handoff fails loudly instead of silently discarding the seam.
    let mut flooded = hyperliquid_handoff();
    for _ in 0..LIVE_BUFFER_CAPACITY {
        flooded
            .accept_candle(&live_candle(120_000_000_000, 10_600, 10_200, 7))
            .expect("buffer");
    }
    assert!(
        flooded
            .accept_candle(&live_candle(120_000_000_000, 10_600, 10_200, 7))
            .is_err()
    );

    // A reconnect retires the whole seam: no bar, buffer, or publication
    // from the old generation may leak into the new one.
    handoff.reset(ProviderGeneration(
        NonZeroU64::new(2).expect("non-zero generation"),
    ));
    handoff.connected = true;
    assert!(handoff.take_publication().is_none());
}

fn resume_test_coordinator<'a>(
    engine: MarketEngine,
    providers: ProviderDispatch<'a>,
    storage: &'a SyncSender<StorageRequest>,
    consumer_id: ConsumerId,
) -> Coordinator<'a> {
    let policy = decide_resource_policy(ResourcePolicyInput {
        mode: EngineResourceMode::Warm,
        available_memory_bytes: 8 * 1024 * 1024 * 1024,
        consumer_count: 1,
        visible_consumer_count: 1,
        stored_series_count: engine.metrics().stored_series,
        provider_series_limit: MAXIMUM_SERIES,
        hot_set_priority_count: 0,
    });
    Coordinator {
        engine,
        providers,
        storage,
        resource_mode: ResourceMode::OfflineSuspended,
        resource_policy: policy,
        available_memory_bytes: 8 * 1024 * 1024 * 1024,
        hot_set_priority_count: 0,
        last_consumer_activity: Instant::now(),
        attached: BTreeSet::new(),
        attached_sinks: BTreeMap::new(),
        consumer_clients: BTreeMap::new(),
        pending: BTreeMap::new(),
        history_inflight: BTreeMap::new(),
        history_deferred: BTreeMap::new(),
        history_cancellations: BTreeMap::new(),
        suspended_history: BTreeSet::new(),
        deferred_publications: BTreeSet::new(),
        pending_empty_repairs: BTreeMap::new(),
        empty_repair_retry_at: Instant::now(),
        history_retries: BTreeMap::new(),
        local_history_deadlines: BTreeMap::new(),
        persistence_pending: BTreeMap::new(),
        persistence_degraded: BTreeSet::new(),
        persistence_backlog: VecDeque::new(),
        local_loaded: BTreeSet::new(),
        warming: BTreeSet::new(),
        warm_series: BTreeMap::new(),
        warm_priority: Vec::new(),
        retained_history: BTreeMap::new(),
        prewarmed: BTreeSet::new(),
        retained_live: BTreeSet::new(),
        warm_search_generations: BTreeMap::new(),
        warm_restore_pending: BTreeMap::new(),
        warm_restore_skipped: BTreeSet::new(),
        events: BTreeMap::from([(consumer_id, ConsumerEvents::default())]),
        rithmic_live: BTreeMap::new(),
        hyperliquid_live: BTreeMap::new(),
        order_books: BTreeMap::new(),
        catalog: BTreeMap::new(),
        catalog_sessions: BTreeMap::new(),
        rithmic_demand: None,
        rithmic_pending_demand: None,
        rithmic_stop_pending: None,
        hyperliquid_engaged: false,
        hyperliquid_demand_dirty: false,
        hyperliquid_stop_pending: None,
        hyperliquid_catalog_degraded: None,
    }
}

fn resume_test_identity() -> (ClientId, ConsumerId, GenerationId, ConsumerIdentity) {
    let client_id = ClientId(NonZeroU64::new(1).expect("client id"));
    let consumer_id = ConsumerId(NonZeroU64::new(1).expect("consumer id"));
    let generation = GenerationId(NonZeroU64::new(1).expect("consumer generation"));
    let identity = ConsumerIdentity {
        client_id,
        workspace_id: WorkspaceId(NonZeroU64::new(1).expect("workspace id")),
        consumer_id,
    };
    (client_id, consumer_id, generation, identity)
}

fn hyperliquid_demand_engine(
    identity: ConsumerIdentity,
    consumer_id: ConsumerId,
    demand_generation: GenerationId,
    history: Vec<MarketBar>,
) -> (MarketEngine, ProviderGeneration) {
    let provider_generation = ProviderGeneration(NonZeroU64::MIN);
    let series = hyperliquid_series();
    let mut engine = configured_engine().expect("test engine configures");
    engine
        .begin_provider_session("hyperliquid", provider_generation)
        .expect("provider session begins");
    engine
        .set_provider_health("hyperliquid", provider_generation, ProviderHealth::Online)
        .expect("provider is online");
    engine
        .register_consumer(identity, true)
        .expect("consumer registers");
    engine
        .set_series_demand_with_streams(
            consumer_id,
            demand_generation,
            &series,
            chart_stream_requirements(&series),
        )
        .expect("demand installs");
    if !history.is_empty() {
        engine
            .install_history(provider_generation, &series, 8, 8, history)
            .expect("history installs");
    }
    (engine, provider_generation)
}

#[test]
fn warm_mode_retains_recent_cache_without_retaining_provider_demand() {
    let series = hyperliquid_series();
    let (client_id, consumer_id, demand_generation, identity) = resume_test_identity();
    let (engine, _) = hyperliquid_demand_engine(
        identity,
        consumer_id,
        demand_generation,
        vec![hyperliquid_bar(1, 60_000_000_000, 10_100)],
    );

    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::new(),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator.resource_mode = ResourceMode::Warm;
    coordinator
        .handle_remove(client_id, consumer_id)
        .expect("consumer removes");
    coordinator.refresh_resource_policy();

    assert_eq!(coordinator.resource_policy.maximum_derived_series, 0);
    assert_eq!(coordinator.resource_policy.maximum_cached_series, 1);
    assert!(!coordinator.engine.has_subscription(&series));
    assert!(
        coordinator.engine.series_snapshot(&series).is_some(),
        "warm cache retention must not be coupled to an upstream subscription"
    );
}

#[test]
fn warm_restore_keeps_markets_live_metadata_beyond_the_warm_cache_budget() {
    let (_, consumer_id, _, _) = resume_test_identity();
    let engine = configured_engine().expect("test engine configures");
    let (storage_tx, storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::new(),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator.resource_mode = ResourceMode::Warm;
    coordinator.hot_set_priority_count = 12;
    coordinator.refresh_resource_policy();
    assert_eq!(coordinator.resource_policy.maximum_cached_series, 8);
    assert_eq!(coordinator.resource_policy.maximum_derived_series, 8);

    let base = rithmic_series();
    let restored = (0..12)
        .map(|index| {
            let mut series = base.clone();
            series.instrument_id = format!("instrument:rithmic:CME:TEST{index}");
            series.entitlement_id = format!("rithmic-test:CME:TEST{index}");
            let symbol = format!("TEST{index}");
            WarmSeries {
                instrument: rithmic_instrument(
                    &symbol,
                    &series.instrument_id,
                    &series.entitlement_id,
                    1,
                    1,
                ),
                series,
                provider_watermark: 1,
            }
        })
        .collect::<Vec<_>>();

    coordinator.restore_hot_series(restored);

    assert_eq!(coordinator.warm_priority.len(), 12);
    assert_eq!(coordinator.warm_series.len(), 12);
    assert_eq!(coordinator.warming.len(), 8);
    assert_eq!(storage_rx.try_iter().count(), 8);

    coordinator.resource_mode = ResourceMode::MarketsLive;
    coordinator.refresh_resource_policy();
    assert_eq!(coordinator.resource_policy.maximum_derived_series, 12);
}

#[test]
fn markets_live_restore_progresses_across_multiple_saved_instruments() {
    let (_, consumer_id, _, _) = resume_test_identity();
    let engine = configured_engine().expect("test engine configures");
    let (history_tx, _history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
    let (catalog_tx, catalog_rx) = mpsc::sync_channel(8);
    let (_catalog_event_tx, catalog_event_rx) = mpsc::sync_channel(8);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::from([(
            "rithmic",
            ProviderDispatchRecord {
                history: &history_tx,
                lifecycle: None,
                realtime: ProviderRealtimeDispatch::Disabled,
                catalog: ProviderCatalogDispatch::Rithmic {
                    controls: &catalog_tx,
                    events: &catalog_event_rx,
                },
            },
        )]),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator.resource_mode = ResourceMode::MarketsLive;
    coordinator.hot_set_priority_count = 2;
    coordinator.refresh_resource_policy();

    let mnq = rithmic_series();
    let mut es = mnq.clone();
    es.instrument_id = "instrument:rithmic:CME:ES".to_string();
    es.entitlement_id = "rithmic-test:CME:ES".to_string();
    let mnq_warm = WarmSeries {
        series: mnq.clone(),
        instrument: rithmic_instrument("MNQU6", &mnq.instrument_id, &mnq.entitlement_id, 1, 1),
        provider_watermark: 1,
    };
    let es_warm = WarmSeries {
        series: es.clone(),
        instrument: rithmic_instrument("ESU6", &es.instrument_id, &es.entitlement_id, 1, 1),
        provider_watermark: 1,
    };
    coordinator.warm_priority = vec![mnq.clone(), es.clone()];
    coordinator
        .warm_series
        .insert(mnq.clone(), mnq_warm.clone());
    coordinator.warm_series.insert(es.clone(), es_warm.clone());

    coordinator.activate_markets_live_hot_set();
    let first_search = match catalog_rx
        .try_recv()
        .expect("first saved instrument searches")
    {
        RithmicCatalogControl::Search(search) => search,
        RithmicCatalogControl::Select(_) => panic!("restore must search before selection"),
    };
    assert_eq!(first_search.query, "MNQU6");
    coordinator.handle_catalog_search(axiusflow_engine_protocol::ProviderInstrumentSearchResult {
        consumer_id: 0,
        provider: "rithmic".to_string(),
        provider_generation: 5,
        search_generation: first_search.search_generation,
        instruments: vec![axiusflow_engine_protocol::ProviderInstrumentSummary {
            symbol: "MNQU6".to_string(),
            exchange: "CME".to_string(),
            ..axiusflow_engine_protocol::ProviderInstrumentSummary::default()
        }],
    });
    let first_select = match catalog_rx
        .try_recv()
        .expect("first saved instrument selects")
    {
        RithmicCatalogControl::Select(select) => select,
        RithmicCatalogControl::Search(_) => panic!("matching search must select"),
    };
    let resolved_mnq = rithmic_instrument(
        "MNQU6",
        &mnq.instrument_id,
        &mnq.entitlement_id,
        5,
        first_select.selection_generation,
    );
    coordinator.handle_catalog_selection(0, first_select.selection_generation, resolved_mnq);

    assert!(coordinator.retained_live.contains(&mnq));
    let second_search = match catalog_rx
        .try_recv()
        .expect("second saved instrument searches")
    {
        RithmicCatalogControl::Search(search) => search,
        RithmicCatalogControl::Select(_) => panic!("next restore must start with search"),
    };
    assert_eq!(second_search.query, "ESU6");
    assert!(second_search.search_generation > first_search.search_generation);
    assert_eq!(
        coordinator.warm_restore_pending.get("rithmic"),
        Some(&es),
        "saved demand restoration must advance without a user selection"
    );
}

#[test]
fn markets_live_catalog_capacity_retries_without_skipping_saved_demand() {
    let (_, consumer_id, _, _) = resume_test_identity();
    let engine = configured_engine().expect("test engine configures");
    let (history_tx, _history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
    let (catalog_tx, catalog_rx) = mpsc::sync_channel(1);
    let (_catalog_event_tx, catalog_event_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    catalog_tx
        .send(RithmicCatalogControl::Search(SearchProviderInstruments {
            consumer_id: 7,
            search_generation: 7,
            provider: "rithmic".to_string(),
            query: "busy".to_string(),
            maximum_results: 1,
        }))
        .expect("fixture fills catalog lane");
    let providers = ProviderDispatch {
        records: BTreeMap::from([(
            "rithmic",
            ProviderDispatchRecord {
                history: &history_tx,
                lifecycle: None,
                realtime: ProviderRealtimeDispatch::Disabled,
                catalog: ProviderCatalogDispatch::Rithmic {
                    controls: &catalog_tx,
                    events: &catalog_event_rx,
                },
            },
        )]),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator.resource_mode = ResourceMode::MarketsLive;
    coordinator.hot_set_priority_count = 1;
    coordinator.refresh_resource_policy();
    let series = rithmic_series();
    coordinator.warm_priority.push(series.clone());
    coordinator.warm_series.insert(
        series.clone(),
        WarmSeries {
            series: series.clone(),
            instrument: rithmic_instrument(
                "MNQU6",
                &series.instrument_id,
                &series.entitlement_id,
                1,
                1,
            ),
            provider_watermark: 1,
        },
    );

    coordinator.activate_markets_live_provider_hot_set("rithmic");
    assert!(coordinator.warm_restore_pending.is_empty());
    assert!(
        !coordinator.warm_restore_skipped.contains(&series),
        "a full bounded lane is transient, not a permanent restore failure"
    );
    let _ = catalog_rx.try_recv().expect("fixture frees catalog lane");

    coordinator.retry_history();
    assert!(matches!(
        catalog_rx.try_recv(),
        Ok(RithmicCatalogControl::Search(search))
            if search.consumer_id == 0 && search.query == "MNQU6"
    ));
    assert_eq!(
        coordinator.warm_restore_pending.get("rithmic"),
        Some(&series)
    );
}

#[test]
fn markets_live_select_capacity_restarts_search_without_skipping_saved_demand() {
    let (_, consumer_id, _, _) = resume_test_identity();
    let engine = configured_engine().expect("test engine configures");
    let (history_tx, _history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
    let (catalog_tx, catalog_rx) = mpsc::sync_channel(1);
    let (_catalog_event_tx, catalog_event_rx) = mpsc::sync_channel(1);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::from([(
            "rithmic",
            ProviderDispatchRecord {
                history: &history_tx,
                lifecycle: None,
                realtime: ProviderRealtimeDispatch::Disabled,
                catalog: ProviderCatalogDispatch::Rithmic {
                    controls: &catalog_tx,
                    events: &catalog_event_rx,
                },
            },
        )]),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator.resource_mode = ResourceMode::MarketsLive;
    coordinator.hot_set_priority_count = 1;
    coordinator.refresh_resource_policy();
    let series = rithmic_series();
    coordinator.warm_priority.push(series.clone());
    coordinator.warm_series.insert(
        series.clone(),
        WarmSeries {
            series: series.clone(),
            instrument: rithmic_instrument(
                "MNQU6",
                &series.instrument_id,
                &series.entitlement_id,
                1,
                1,
            ),
            provider_watermark: 1,
        },
    );

    coordinator.activate_markets_live_provider_hot_set("rithmic");
    let first_search = match catalog_rx.try_recv().expect("saved instrument searches") {
        RithmicCatalogControl::Search(search) => search,
        RithmicCatalogControl::Select(_) => panic!("restore must search before selection"),
    };
    catalog_tx
        .send(RithmicCatalogControl::Search(SearchProviderInstruments {
            consumer_id: 7,
            search_generation: 7,
            provider: "rithmic".to_string(),
            query: "busy".to_string(),
            maximum_results: 1,
        }))
        .expect("fixture fills catalog lane before select");

    coordinator.handle_catalog_search(axiusflow_engine_protocol::ProviderInstrumentSearchResult {
        consumer_id: 0,
        provider: "rithmic".to_string(),
        provider_generation: 1,
        search_generation: first_search.search_generation,
        instruments: vec![axiusflow_engine_protocol::ProviderInstrumentSummary {
            symbol: "MNQU6".to_string(),
            exchange: "CME".to_string(),
            ..axiusflow_engine_protocol::ProviderInstrumentSummary::default()
        }],
    });
    assert!(coordinator.warm_restore_pending.is_empty());
    assert!(
        !coordinator.warm_restore_skipped.contains(&series),
        "select queue pressure is transient, not a permanent restore rejection"
    );
    let _ = catalog_rx.try_recv().expect("fixture frees catalog lane");

    coordinator.retry_history();
    assert!(matches!(
        catalog_rx.try_recv(),
        Ok(RithmicCatalogControl::Search(search))
            if search.consumer_id == 0
                && search.query == "MNQU6"
                && search.search_generation > first_search.search_generation
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn rithmic_catalog_and_history_survive_multi_instrument_session_advance() {
    let (_, consumer_id, _, _) = resume_test_identity();
    let engine = configured_engine().expect("test engine configures");
    let (history_tx, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::from([(
            "rithmic",
            ProviderDispatchRecord {
                history: &history_tx,
                lifecycle: None,
                realtime: ProviderRealtimeDispatch::Disabled,
                catalog: ProviderCatalogDispatch::Disabled,
            },
        )]),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator.resource_mode = ResourceMode::MarketsLive;
    let mnq = rithmic_series();
    let mut es = mnq.clone();
    es.instrument_id = "instrument:rithmic:CME:ES".to_string();
    es.entitlement_id = "rithmic-test:CME:ES".to_string();

    coordinator
        .install_provider_instrument(&rithmic_instrument(
            "MNQU6",
            &mnq.instrument_id,
            &mnq.entitlement_id,
            1,
            9,
        ))
        .expect("first instrument installs");
    coordinator
        .install_provider_instrument(&rithmic_instrument(
            "ESU6",
            &es.instrument_id,
            &es.entitlement_id,
            1,
            1,
        ))
        .expect("independent same-session selection generation installs");
    assert_eq!(
        coordinator
            .catalog
            .keys()
            .filter(|(provider, _)| provider == "rithmic")
            .count(),
        2
    );

    coordinator
        .install_provider_instrument(&rithmic_instrument(
            "MNQU6",
            &mnq.instrument_id,
            &mnq.entitlement_id,
            2,
            1,
        ))
        .expect("new provider session dominates the restarted selection counter");
    assert_eq!(
        coordinator
            .catalog
            .get(&("rithmic".to_string(), es.instrument_id.clone()))
            .expect("unrelated retained instrument survives session advance")
            .session_generation,
        1
    );

    for series in [&mnq, &es] {
        coordinator.retained_live.insert(series.clone());
        coordinator.rithmic_live.insert(
            series.clone(),
            RithmicLiveHandoff::new(
                series,
                ProviderGeneration(NonZeroU64::new(2).expect("generation")),
                "CME",
            )
            .expect("live handoff"),
        );
    }
    coordinator.rithmic_connecting(3);
    let current = ProviderGeneration(NonZeroU64::new(3).expect("current generation"));
    assert_eq!(coordinator.provider_generation_for_series(&es), Ok(current));
    let demand = coordinator
        .rithmic_realtime_demand()
        .expect("full demand rebuilds from retained catalog");
    assert_eq!(demand.instruments.len(), 2);
    assert!(
        demand
            .instruments
            .iter()
            .any(|item| { item.instrument.instrument_id == mnq.instrument_id })
    );
    assert!(
        demand
            .instruments
            .iter()
            .any(|item| { item.instrument.instrument_id == es.instrument_id })
    );

    coordinator
        .enqueue_history_request(&es, current, None)
        .expect("retained instrument history uses current provider generation");
    let request = history_rx.try_recv().expect("history request queues");
    assert_eq!(request.provider_generation, current);
    assert_eq!(
        request
            .instrument
            .expect("retained instrument metadata accompanies request")
            .session_generation,
        1,
        "metadata install generation remains provenance, not the current request fence"
    );
    assert!(
        coordinator
            .install_provider_instrument(&rithmic_instrument(
                "ESU6",
                &es.instrument_id,
                &es.entitlement_id,
                2,
                99,
            ))
            .is_err(),
        "a callback older than the current provider session is retired even with a larger selection counter"
    );
}

#[test]
fn viewport_backfill_uses_local_range_before_provider_history() {
    let series = hyperliquid_series();
    let (_, consumer_id, demand_generation, identity) = resume_test_identity();
    let (engine, provider_generation) = hyperliquid_demand_engine(
        identity,
        consumer_id,
        demand_generation,
        vec![
            hyperliquid_bar(1, 120_000_000_000, 10_100),
            hyperliquid_bar(2, 180_000_000_000, 10_200),
        ],
    );

    let (storage_tx, storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::new(),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator
        .install_provider_instrument(&hyperliquid_instrument(provider_generation.0.get()))
        .expect("instrument installs");
    let viewport = Viewport::try_new(0, 240_000_000_000).expect("viewport");

    coordinator
        .request_viewport_history(consumer_id, demand_generation, viewport)
        .expect("viewport backfill schedules");

    assert!(matches!(
        storage_rx.try_recv(),
        Ok(StorageRequest::ReadRange(requested, generation, range))
            if requested == series
                && generation == provider_generation
                && range == HistoryRange {
                    start_unix_nanos: 0,
                    end_unix_nanos: 120_000_000_000,
                }
    ));
    assert!(
        coordinator.history_inflight.is_empty(),
        "provider fetch starts only after the local range result is usable"
    );
}

#[test]
fn rapid_viewport_change_waits_for_cache_then_reads_latest_range() {
    let series = hyperliquid_series();
    let (_, consumer_id, demand_generation, identity) = resume_test_identity();
    let (engine, provider_generation) = hyperliquid_demand_engine(
        identity,
        consumer_id,
        demand_generation,
        vec![
            hyperliquid_bar(1, 300_000_000_000, 10_100),
            hyperliquid_bar(2, 360_000_000_000, 10_200),
        ],
    );
    let (history_tx, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
    let (storage_tx, storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::from([(
            "hyperliquid",
            ProviderDispatchRecord {
                history: &history_tx,
                lifecycle: None,
                realtime: ProviderRealtimeDispatch::Disabled,
                catalog: ProviderCatalogDispatch::Disabled,
            },
        )]),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator
        .install_provider_instrument(&hyperliquid_instrument(provider_generation.0.get()))
        .expect("instrument installs");
    let first_viewport = Viewport::try_new(120_000_000_000, 420_000_000_000).expect("viewport");
    coordinator
        .engine
        .set_viewport(consumer_id, demand_generation, first_viewport)
        .expect("first viewport installs");
    coordinator
        .request_viewport_history(consumer_id, demand_generation, first_viewport)
        .expect("first viewport schedules");
    let first_range = match storage_rx.try_recv().expect("first local read queues") {
        StorageRequest::ReadRange(requested, generation, range) => {
            assert_eq!(requested, series);
            assert_eq!(generation, provider_generation);
            range
        }
        _ => panic!("viewport must use local range first"),
    };
    assert_eq!(
        first_range,
        HistoryRange {
            start_unix_nanos: 120_000_000_000,
            end_unix_nanos: 300_000_000_000,
        }
    );

    let latest_viewport = Viewport::try_new(60_000_000_000, 420_000_000_000).expect("viewport");
    coordinator
        .engine
        .set_viewport(consumer_id, demand_generation, latest_viewport)
        .expect("latest viewport installs");
    coordinator
        .request_viewport_history(consumer_id, demand_generation, latest_viewport)
        .expect("latest viewport is retained while cache read is pending");
    assert!(
        storage_rx.try_recv().is_err(),
        "only one local read is in flight"
    );
    assert!(
        history_rx.try_recv().is_err(),
        "rapid viewport movement must not bypass cache-first ordering"
    );

    coordinator.local_history_range_completed(&series, provider_generation, first_range, Ok(None));

    assert!(matches!(
        storage_rx.try_recv(),
        Ok(StorageRequest::ReadRange(requested, generation, range))
            if requested == series
                && generation == provider_generation
                && range == HistoryRange {
                    start_unix_nanos: 60_000_000_000,
                    end_unix_nanos: 300_000_000_000,
                }
    ));
    assert!(matches!(
        history_rx.try_recv(),
        Ok(request)
            if request.series == series
                && request.range == Some(first_range)
    ));
}

#[test]
fn timed_out_local_viewport_read_falls_back_over_latest_suppressed_range() {
    let series = hyperliquid_series();
    let (_, consumer_id, demand_generation, identity) = resume_test_identity();
    let (engine, provider_generation) = hyperliquid_demand_engine(
        identity,
        consumer_id,
        demand_generation,
        vec![hyperliquid_bar(1, 300_000_000_000, 10_100)],
    );
    let (history_tx, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
    let (storage_tx, storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::from([(
            "hyperliquid",
            ProviderDispatchRecord {
                history: &history_tx,
                lifecycle: None,
                realtime: ProviderRealtimeDispatch::Disabled,
                catalog: ProviderCatalogDispatch::Disabled,
            },
        )]),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator
        .install_provider_instrument(&hyperliquid_instrument(provider_generation.0.get()))
        .expect("instrument installs");
    let first_viewport = Viewport::try_new(120_000_000_000, 420_000_000_000).expect("viewport");
    coordinator
        .engine
        .set_viewport(consumer_id, demand_generation, first_viewport)
        .expect("first viewport installs");
    coordinator
        .request_viewport_history(consumer_id, demand_generation, first_viewport)
        .expect("first viewport schedules");
    let _ = storage_rx.try_recv().expect("first local read queues");

    let latest_viewport = Viewport::try_new(60_000_000_000, 420_000_000_000).expect("viewport");
    coordinator
        .engine
        .set_viewport(consumer_id, demand_generation, latest_viewport)
        .expect("latest viewport installs");
    coordinator
        .request_viewport_history(consumer_id, demand_generation, latest_viewport)
        .expect("latest viewport is retained while cache read is pending");
    coordinator
        .local_history_deadlines
        .get_mut(&(series.clone(), provider_generation))
        .expect("local read is pending")
        .deadline = Instant::now();

    coordinator.expire_local_history_reads();

    assert!(matches!(
        history_rx.try_recv(),
        Ok(request)
            if request.series == series
                && request.range == Some(HistoryRange {
                    start_unix_nanos: 60_000_000_000,
                    end_unix_nanos: 300_000_000_000,
                })
    ));
}

#[test]
fn cached_viewport_backfill_publishes_progressively_and_preserves_live_tail() {
    let series = hyperliquid_series();
    let (_, consumer_id, demand_generation, identity) = resume_test_identity();
    let (mut engine, provider_generation) = hyperliquid_demand_engine(
        identity,
        consumer_id,
        demand_generation,
        vec![
            hyperliquid_bar(1, 120_000_000_000, 10_100),
            hyperliquid_bar(2, 180_000_000_000, 10_200),
        ],
    );
    engine
        .install_realtime_tail(
            provider_generation,
            &series,
            8,
            8,
            hyperliquid_bar(3, 240_000_000_000, 10_300),
            true,
        )
        .expect("live tail installs");

    let (history_tx, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
    let (storage_tx, storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::from([(
            "hyperliquid",
            ProviderDispatchRecord {
                history: &history_tx,
                lifecycle: None,
                realtime: ProviderRealtimeDispatch::Disabled,
                catalog: ProviderCatalogDispatch::Disabled,
            },
        )]),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator
        .install_provider_instrument(&hyperliquid_instrument(provider_generation.0.get()))
        .expect("instrument installs");
    let viewport = Viewport::try_new(0, 300_000_000_000).expect("viewport");
    coordinator
        .request_viewport_history(consumer_id, demand_generation, viewport)
        .expect("viewport backfill schedules");
    let range = match storage_rx.try_recv().expect("local range request") {
        StorageRequest::ReadRange(requested, generation, range) => {
            assert_eq!(requested, series);
            assert_eq!(generation, provider_generation);
            range
        }
        _ => panic!("viewport must read local range first"),
    };

    coordinator.local_history_range_completed(
        &series,
        provider_generation,
        range,
        Ok(Some(StoredHistory {
            bars: vec![hyperliquid_bar(1, 60_000_000_000, 10_050)],
            derived: false,
            durable: true,
        })),
    );

    let snapshot = coordinator
        .engine
        .series_snapshot(&series)
        .expect("progressive cached snapshot remains usable");
    assert!(snapshot.forming);
    assert_eq!(
        snapshot
            .bars
            .iter()
            .map(|bar| bar.exchange_timestamp_unix_nanos)
            .collect::<Vec<_>>(),
        vec![
            60_000_000_000,
            120_000_000_000,
            180_000_000_000,
            240_000_000_000,
        ]
    );
    assert_eq!(snapshot.bars.last().map(|bar| bar.close), Some(10_300));
    assert!(matches!(
        coordinator
            .events
            .get(&consumer_id)
            .and_then(|events| events.series_state.as_ref()),
        Some(envelope::Payload::SeriesState(state))
            if SeriesLoadState::try_from(state.state) == Ok(SeriesLoadState::Partial)
                && PersistenceState::try_from(state.persistence) == Ok(PersistenceState::Durable)
    ));
    let provider_request = history_rx
        .try_recv()
        .expect("provider repair follows progressive cache result");
    assert_eq!(provider_request.range, Some(range));
}

#[test]
fn failed_viewport_backfill_does_not_mark_pending_live_data_durable() {
    let series = hyperliquid_series();
    let (_, consumer_id, demand_generation, identity) = resume_test_identity();
    let (engine, provider_generation) = hyperliquid_demand_engine(
        identity,
        consumer_id,
        demand_generation,
        vec![hyperliquid_bar(1, 120_000_000_000, 10_100)],
    );

    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::new(),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    let range = HistoryRange {
        start_unix_nanos: 0,
        end_unix_nanos: 120_000_000_000,
    };
    coordinator.broadcast_persistence_for(&series, PersistenceState::Pending, None);
    coordinator.local_history_deadlines.insert(
        (series.clone(), provider_generation),
        PendingLocalHistoryRead {
            deadline: Instant::now() + LOCAL_HISTORY_READ_TIMEOUT,
            range: Some(range),
        },
    );

    coordinator.local_history_range_completed(&series, provider_generation, range, Ok(None));

    assert!(matches!(
        coordinator
            .events
            .get(&consumer_id)
            .and_then(|events| events.series_state.as_ref()),
        Some(envelope::Payload::SeriesState(state))
            if PersistenceState::try_from(state.persistence) == Ok(PersistenceState::Pending)
    ));
    assert!(
        coordinator
            .events
            .get(&consumer_id)
            .and_then(|events| events.demand_error.as_ref())
            .is_some(),
        "provider backfill failure is diagnostic, not a durability acknowledgment"
    );
}

#[test]
fn persistence_stays_pending_until_all_storage_acks_arrive() {
    let series = hyperliquid_series();
    let (_, consumer_id, demand_generation, identity) = resume_test_identity();
    let (engine, provider_generation) = hyperliquid_demand_engine(
        identity,
        consumer_id,
        demand_generation,
        vec![hyperliquid_bar(1, 60_000_000_000, 10_100)],
    );

    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::new(),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator.broadcast_persistence_for(&series, PersistenceState::Pending, None);
    coordinator.enqueue_persistence(
        &series,
        provider_generation,
        vec![hyperliquid_bar(2, 120_000_000_000, 10_200)],
        false,
        "fixture persistence unavailable",
    );
    coordinator.enqueue_persistence(
        &series,
        provider_generation,
        vec![hyperliquid_bar(3, 180_000_000_000, 10_300)],
        false,
        "fixture persistence unavailable",
    );

    coordinator.persistence_completed(&series, provider_generation, Ok(()), 1);
    assert_eq!(
        coordinator
            .persistence_pending
            .get(&(series.clone(), provider_generation)),
        Some(&1)
    );
    assert!(matches!(
        coordinator
            .events
            .get(&consumer_id)
            .and_then(|events| events.series_state.as_ref()),
        Some(envelope::Payload::SeriesState(state))
            if PersistenceState::try_from(state.persistence) == Ok(PersistenceState::Pending)
    ));

    coordinator.persistence_completed(&series, provider_generation, Ok(()), 1);
    assert!(
        !coordinator
            .persistence_pending
            .contains_key(&(series.clone(), provider_generation))
    );
    assert!(matches!(
        coordinator
            .events
            .get(&consumer_id)
            .and_then(|events| events.series_state.as_ref()),
        Some(envelope::Payload::SeriesState(state))
            if PersistenceState::try_from(state.persistence) == Ok(PersistenceState::Durable)
    ));
}

#[test]
fn cached_demand_preserves_shared_pending_and_degraded_persistence() {
    let series = hyperliquid_series();
    let (_, consumer_id, demand_generation, identity) = resume_test_identity();
    let (engine, provider_generation) = hyperliquid_demand_engine(
        identity,
        consumer_id,
        demand_generation,
        vec![hyperliquid_bar(1, 60_000_000_000, 10_100)],
    );
    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::new(),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator.resource_mode = ResourceMode::Warm;
    coordinator
        .install_provider_instrument(&hyperliquid_instrument(provider_generation.0.get()))
        .expect("instrument installs");
    let publication = coordinator
        .engine
        .publish_series_snapshot(&series)
        .expect("snapshot publishes")
        .into_iter()
        .find(|publication| publication.consumer_id == consumer_id)
        .expect("consumer publication");
    let waiter = DemandWaiter {
        consumer_id,
        generation: demand_generation,
        started_at: Instant::now(),
    };
    let key = (series.clone(), provider_generation);

    coordinator.persistence_pending.insert(key.clone(), 1);
    coordinator
        .publish_cached_demand(&series, provider_generation, &waiter, &publication)
        .expect("pending cached demand publishes");
    assert!(matches!(
        coordinator
            .events
            .get(&consumer_id)
            .and_then(|events| events.series_state.as_ref()),
        Some(envelope::Payload::SeriesState(state))
            if PersistenceState::try_from(state.persistence) == Ok(PersistenceState::Pending)
    ));

    coordinator.persistence_pending.remove(&key);
    coordinator.persistence_degraded.insert(key);
    if let Some(events) = coordinator.events.get_mut(&consumer_id) {
        events.series_state = None;
    }
    coordinator
        .publish_cached_demand(&series, provider_generation, &waiter, &publication)
        .expect("degraded cached demand publishes");
    assert!(matches!(
        coordinator
            .events
            .get(&consumer_id)
            .and_then(|events| events.series_state.as_ref()),
        Some(envelope::Payload::SeriesState(state))
            if PersistenceState::try_from(state.persistence) == Ok(PersistenceState::Degraded)
    ));
}

#[test]
fn full_storage_channel_buffers_persistence_until_capacity_returns() {
    let series = hyperliquid_series();
    let (_, consumer_id, demand_generation, identity) = resume_test_identity();
    let (engine, provider_generation) =
        hyperliquid_demand_engine(identity, consumer_id, demand_generation, Vec::new());

    let (storage_tx, storage_rx) = mpsc::sync_channel(1);
    storage_tx
        .send(StorageRequest::Read(series.clone(), provider_generation))
        .expect("fixture fills storage channel");
    let providers = ProviderDispatch {
        records: BTreeMap::new(),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator.enqueue_persistence(
        &series,
        provider_generation,
        vec![hyperliquid_bar(1, 60_000_000_000, 10_100)],
        false,
        "fixture persistence unavailable",
    );
    assert_eq!(coordinator.persistence_backlog.len(), 1);
    assert_eq!(
        coordinator
            .persistence_pending
            .get(&(series.clone(), provider_generation)),
        Some(&1)
    );

    assert!(matches!(
        storage_rx.try_recv(),
        Ok(StorageRequest::Read(..))
    ));
    coordinator.retry_persistence_backlog();
    assert!(coordinator.persistence_backlog.is_empty());
    assert!(matches!(
        storage_rx.try_recv(),
        Ok(StorageRequest::Persist(requested, generation, ..))
            if requested == series && generation == provider_generation
    ));
}

#[test]
fn attached_event_delivery_is_fair_across_busy_consumers() {
    let (client_id, consumer_id, _, _) = resume_test_identity();
    let other_client = ClientId(NonZeroU64::new(2).expect("client id"));
    let other_consumer = ConsumerId(NonZeroU64::new(2).expect("consumer id"));
    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::new(),
    };
    let engine = configured_engine().expect("test engine configures");
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator
        .events
        .insert(other_consumer, ConsumerEvents::default());
    coordinator.consumer_clients.insert(consumer_id, client_id);
    coordinator
        .consumer_clients
        .insert(other_consumer, other_client);
    let (busy_tx, _busy_rx) = mpsc::sync_channel(REALTIME_DRAIN_BUDGET + 1);
    let (other_tx, other_rx) = mpsc::sync_channel(2);
    coordinator.attached_sinks.insert(client_id, busy_tx);
    coordinator.attached_sinks.insert(other_client, other_tx);

    let busy = coordinator
        .events
        .get_mut(&consumer_id)
        .expect("busy consumer events");
    for _ in 0..REALTIME_DRAIN_BUDGET {
        busy.series
            .push_back(envelope::Payload::SeriesState(SeriesState::default()));
    }
    coordinator
        .events
        .get_mut(&other_consumer)
        .expect("other consumer events")
        .series
        .push_back(envelope::Payload::SeriesState(SeriesState::default()));

    coordinator.flush_attached_events();

    let (routed_consumer, _) = other_rx
        .try_recv()
        .expect("busy pane must not consume the entire delivery budget");
    assert_eq!(routed_consumer, other_consumer.0.get());
}

#[test]
#[allow(clippy::too_many_lines)]
fn hidden_order_book_stays_canonical_and_republishes_latest_revision_on_restore() {
    let series = hyperliquid_series();
    let (client_id, consumer_id, demand_generation, identity) = resume_test_identity();
    let (engine, provider_generation) =
        hyperliquid_demand_engine(identity, consumer_id, demand_generation, Vec::new());
    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::new(),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator.resource_mode = ResourceMode::Warm;
    coordinator
        .install_provider_instrument(&hyperliquid_instrument(provider_generation.0.get()))
        .expect("instrument installs");
    coordinator.reconcile_order_books();

    let mut snapshot = DepthSnapshot {
        metadata: axiusflow_market_data::EventMetadata {
            provider_id: series.provider_id.clone(),
            instrument_id: series.instrument_id.clone(),
            entitlement_id: series.entitlement_id.clone(),
            source_sequence: 10,
            session_generation: provider_generation.0.get(),
            timestamps: axiusflow_market_data::QualifiedTimestamp {
                exchange_unix_nanos: None,
                provider_unix_nanos: None,
                received_unix_nanos: 1,
            },
        },
        bids: vec![DepthLevel {
            price: 100,
            quantity: 5,
            order_count: None,
        }],
        asks: vec![DepthLevel {
            price: 101,
            quantity: 6,
            order_count: None,
        }],
    };
    coordinator.provider_depth("hyperliquid", provider_generation.0.get(), &snapshot);
    let visible_revision = coordinator
        .order_books
        .get(&(series.provider_id.clone(), series.instrument_id.clone()))
        .expect("foreground book is engine-owned")
        .book
        .publication()
        .revision;
    assert!(visible_revision > 0);
    assert!(
        coordinator
            .events
            .get(&consumer_id)
            .expect("consumer events")
            .order_book
            .is_some(),
        "foreground depth is queued for IPC"
    );

    let (hide_reply, hide_result) = mpsc::sync_channel(1);
    coordinator.handle_consumer_command(Command::ResourceClass(
        client_id,
        consumer_id,
        ConsumerResourceClass::Background,
        hide_reply,
    ));
    hide_result
        .recv()
        .expect("hide replies")
        .expect("consumer hides");
    assert!(
        coordinator
            .events
            .get(&consumer_id)
            .expect("consumer events")
            .order_book
            .is_none(),
        "a snapshot queued before hide must be cleared"
    );

    snapshot.metadata.source_sequence = 11;
    snapshot.metadata.timestamps.received_unix_nanos = 2;
    snapshot.bids[0].quantity = 7;
    coordinator.provider_depth("hyperliquid", provider_generation.0.get(), &snapshot);
    let retained_revision = coordinator
        .order_books
        .get(&(series.provider_id.clone(), series.instrument_id.clone()))
        .expect("hidden book remains engine-owned")
        .book
        .publication()
        .revision;
    assert!(retained_revision > visible_revision);
    assert!(
        coordinator
            .events
            .get(&consumer_id)
            .expect("consumer events")
            .order_book
            .is_none(),
        "hidden canonical updates must not queue depth IPC"
    );

    let (restore_reply, restore_result) = mpsc::sync_channel(1);
    coordinator.handle_consumer_command(Command::ResourceClass(
        client_id,
        consumer_id,
        ConsumerResourceClass::Foreground,
        restore_reply,
    ));
    restore_result
        .recv()
        .expect("restore replies")
        .expect("consumer restores");
    let envelope::Payload::OrderBookSnapshot(restored) = coordinator
        .events
        .get_mut(&consumer_id)
        .expect("consumer events")
        .order_book
        .take()
        .expect("restore republishes the retained book")
    else {
        panic!("restore must publish an order-book snapshot");
    };
    assert_eq!(restored.provider_generation, provider_generation.0.get());
    assert_eq!(restored.revision, retained_revision);
}

#[test]
fn shutdown_drains_queued_rithmic_rollover_before_persistence_flush() {
    let series = rithmic_series();
    let (_, consumer_id, demand_generation, identity) = resume_test_identity();
    let provider_generation = ProviderGeneration(NonZeroU64::MIN);
    let mut engine = configured_engine().expect("test engine configures");
    engine
        .begin_provider_session("rithmic", provider_generation)
        .expect("provider session begins");
    engine
        .set_provider_health("rithmic", provider_generation, ProviderHealth::Online)
        .expect("provider online");
    engine
        .register_consumer(identity, true)
        .expect("consumer registers");
    engine
        .set_series_demand_with_streams(
            consumer_id,
            demand_generation,
            &series,
            chart_stream_requirements(&series),
        )
        .expect("demand installs");
    engine
        .install_history(provider_generation, &series, 2, 0, vec![market_bar()])
        .expect("history installs");

    let (history_tx, _history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
    let (realtime_controls, _realtime_control_rx) =
        mpsc::sync_channel(RITHMIC_REALTIME_CONTROL_CAPACITY);
    let (realtime_event_tx, realtime_events) = mpsc::sync_channel(REALTIME_CAPACITY);
    let (storage_tx, storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::from([(
            "rithmic",
            ProviderDispatchRecord {
                history: &history_tx,
                lifecycle: None,
                realtime: ProviderRealtimeDispatch::Rithmic {
                    controls: &realtime_controls,
                    events: &realtime_events,
                },
                catalog: ProviderCatalogDispatch::Disabled,
            },
        )]),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator.resource_mode = ResourceMode::Warm;
    coordinator.catalog.insert(
        (series.provider_id.clone(), series.instrument_id.clone()),
        rithmic_instrument(
            "MNQU6",
            &series.instrument_id,
            &series.entitlement_id,
            provider_generation.0.get(),
            1,
        ),
    );
    let mut live = RithmicLiveHandoff::new(&series, provider_generation, "CME").expect("handoff");
    live.seed(2, 0, &[market_bar()], None, Some(60_000_000_000))
        .expect("handoff seeds");
    live.connected = true;
    coordinator.rithmic_live.insert(series.clone(), live);

    realtime_event_tx
        .send(RithmicRealtimeEvent::Trade(
            provider_generation.0.get(),
            market_trade(1, 120_000_000_000, 10_200, 2),
        ))
        .expect("first rollover trade queues");
    realtime_event_tx
        .send(RithmicRealtimeEvent::Trade(
            provider_generation.0.get(),
            market_trade(2, 180_000_000_000, 10_300, 3),
        ))
        .expect("final rollover trade queues");

    super::coordinator::drain_shutdown_provider_events(&mut coordinator);
    coordinator.begin_shutdown();

    let snapshot = coordinator
        .engine
        .series_snapshot(&series)
        .expect("shutdown keeps final canonical state");
    assert_eq!(snapshot.bars.last().map(|bar| bar.close), Some(10_300));
    assert!(matches!(
        storage_rx.try_recv(),
        Ok(StorageRequest::Persist(requested, generation, bars, false, ..))
            if requested == series
                && generation == provider_generation
                && bars.len() == 1
                && bars[0].close == 10_200
    ));
}

#[test]
fn history_retry_is_series_scoped_and_does_not_fake_provider_recovery() {
    let series = hyperliquid_series();
    let (_, consumer_id, demand_generation, identity) = resume_test_identity();
    let provider_generation = ProviderGeneration(NonZeroU64::new(1).expect("provider generation"));
    let mut engine = configured_engine().expect("test engine configures");
    engine
        .begin_provider_session("hyperliquid", provider_generation)
        .expect("provider session begins");
    engine
        .set_provider_health("hyperliquid", provider_generation, ProviderHealth::Online)
        .expect("provider is online");
    engine
        .register_consumer(identity, true)
        .expect("consumer registers");
    engine
        .set_series_demand_with_streams(
            consumer_id,
            demand_generation,
            &series,
            chart_stream_requirements(&series),
        )
        .expect("demand installs");

    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::new(),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);

    assert!(coordinator.schedule_history_retry(
        &series,
        provider_generation,
        None,
        "fixture history failure"
    ));
    assert_eq!(
        coordinator
            .engine
            .provider_status("hyperliquid")
            .map(|status| status.health),
        Some(ProviderHealth::Online),
        "history repair must not mutate transport health"
    );
    let events = coordinator
        .events
        .get(&consumer_id)
        .expect("consumer events");
    assert!(
        events.provider.is_none(),
        "history repair must not emit provider recovery"
    );
    assert!(matches!(
        events.series_state.as_ref(),
        Some(envelope::Payload::SeriesState(state))
            if SeriesLoadState::try_from(state.state) == Ok(SeriesLoadState::Resolving)
    ));
}

#[test]
fn persistence_degradation_never_becomes_a_market_demand_failure() {
    let series = hyperliquid_series();
    let (_, consumer_id, demand_generation, identity) = resume_test_identity();
    let provider_generation = ProviderGeneration(NonZeroU64::new(1).expect("provider generation"));
    let mut engine = configured_engine().expect("test engine configures");
    engine
        .begin_provider_session("hyperliquid", provider_generation)
        .expect("provider session begins");
    engine
        .set_provider_health("hyperliquid", provider_generation, ProviderHealth::Online)
        .expect("provider is online");
    engine
        .register_consumer(identity, true)
        .expect("consumer registers");
    engine
        .set_series_demand_with_streams(
            consumer_id,
            demand_generation,
            &series,
            chart_stream_requirements(&series),
        )
        .expect("demand installs");

    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::new(),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator.persistence_completed(
        &series,
        provider_generation,
        Err(LocalHistoryError::Unavailable),
        7,
    );

    let events = coordinator
        .events
        .get(&consumer_id)
        .expect("consumer events");
    assert!(
        events.demand_error.is_none(),
        "disk durability is independent from usable live market data"
    );
    assert!(matches!(
        events.series_state.as_ref(),
        Some(envelope::Payload::SeriesState(state))
            if PersistenceState::try_from(state.persistence) == Ok(PersistenceState::Degraded)
    ));
    assert_eq!(
        coordinator
            .engine
            .provider_status("hyperliquid")
            .map(|status| status.health),
        Some(ProviderHealth::Online)
    );
}

#[test]
fn catalog_refresh_failure_cannot_override_realtime_transport_health() {
    let (_, consumer_id, _, _) = resume_test_identity();
    let provider_generation = ProviderGeneration(NonZeroU64::new(1).expect("provider generation"));
    let mut engine = configured_engine().expect("test engine configures");
    engine
        .begin_provider_session("hyperliquid", provider_generation)
        .expect("provider session begins");
    engine
        .set_provider_health("hyperliquid", provider_generation, ProviderHealth::Online)
        .expect("provider is online");
    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::new(),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);

    coordinator.degrade_hyperliquid_catalog_health("fixture catalog failure");
    assert_eq!(
        coordinator
            .engine
            .provider_status("hyperliquid")
            .map(|status| status.health),
        Some(ProviderHealth::Online)
    );
    assert!(
        coordinator
            .events
            .get(&consumer_id)
            .is_some_and(|events| events.provider.is_none()),
        "catalog degradation must not masquerade as transport recovery"
    );

    coordinator
        .engine
        .set_provider_health(
            "hyperliquid",
            provider_generation,
            ProviderHealth::Recovering,
        )
        .expect("transport enters genuine recovery");
    coordinator.restore_hyperliquid_catalog_health("hyperliquid");
    assert_eq!(
        coordinator
            .engine
            .provider_status("hyperliquid")
            .map(|status| status.health),
        Some(ProviderHealth::Recovering),
        "catalog success must never paint a recovering transport online"
    );
}

#[test]
fn restarted_hyperliquid_trade_ordinal_resets_only_order_flow() {
    let series = hyperliquid_series();
    let (_, consumer_id, demand_generation, identity) = resume_test_identity();
    let provider_generation = ProviderGeneration(NonZeroU64::new(1).expect("provider generation"));
    let mut engine = configured_engine().expect("test engine configures");
    engine
        .begin_provider_session("hyperliquid", provider_generation)
        .expect("provider session begins");
    engine
        .set_provider_health("hyperliquid", provider_generation, ProviderHealth::Online)
        .expect("provider is online");
    engine
        .register_consumer(identity, true)
        .expect("consumer registers");
    engine
        .set_series_demand_with_streams(
            consumer_id,
            demand_generation,
            &series,
            chart_stream_requirements(&series),
        )
        .expect("demand installs");

    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::new(),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator
        .hyperliquid_live
        .insert(series.clone(), hyperliquid_handoff());

    coordinator.hyperliquid_trade(
        provider_generation.0.get(),
        &hyperliquid_trade(9, 1_800_000_000_000_000_000, 10_000, 2),
    );
    coordinator.hyperliquid_trade(
        provider_generation.0.get(),
        &hyperliquid_trade(1, 1_800_000_000_100_000_000, 10_100, 3),
    );

    let events = coordinator
        .events
        .get(&consumer_id)
        .expect("consumer events");
    assert!(matches!(
        events.order_flow.as_ref(),
        Some(envelope::Payload::OrderFlowSnapshot(snapshot))
            if snapshot.source_watermark == 1 && snapshot.tape.len() == 1
    ));
    assert!(
        events.series_state.is_none(),
        "order-flow reset must not recover the chart"
    );
    assert!(
        events.provider.is_none(),
        "order-flow reset must not change provider status"
    );
    assert!(coordinator.history_inflight.is_empty());
    assert_eq!(
        coordinator
            .engine
            .provider_status("hyperliquid")
            .map(|status| status.health),
        Some(ProviderHealth::Online)
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn hyperliquid_completed_stop_resumes_on_next_generation_and_gate_retry() {
    let series = hyperliquid_series();
    let (_, consumer_id, demand_generation, identity) = resume_test_identity();
    let old_generation = ProviderGeneration(NonZeroU64::new(1).expect("old generation"));
    let new_generation = ProviderGeneration(NonZeroU64::new(2).expect("new generation"));
    let mut engine = configured_engine().expect("test engine configures");
    engine
        .begin_provider_session("hyperliquid", old_generation)
        .expect("old Hyperliquid session begins");
    engine
        .set_provider_health("hyperliquid", old_generation, ProviderHealth::Online)
        .expect("old Hyperliquid session is online");
    engine
        .register_consumer(identity, true)
        .expect("consumer registers");
    engine
        .set_series_demand_with_streams(
            consumer_id,
            demand_generation,
            &series,
            chart_stream_requirements(&series),
        )
        .expect("demand installs");

    let (history_tx, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
    let (realtime_controls, realtime_control_rx) = mpsc::sync_channel(REALTIME_CAPACITY);
    let (_realtime_event_tx, realtime_events) = mpsc::sync_channel(REALTIME_CAPACITY);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::from([(
            "hyperliquid",
            ProviderDispatchRecord {
                history: &history_tx,
                lifecycle: None,
                realtime: ProviderRealtimeDispatch::Hyperliquid {
                    controls: &realtime_controls,
                    events: &realtime_events,
                },
                catalog: ProviderCatalogDispatch::Disabled,
            },
        )]),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator.catalog.insert(
        (series.provider_id.clone(), series.instrument_id.clone()),
        InstallProviderInstrument {
            provider: "hyperliquid".to_string(),
            session_generation: old_generation.0.get(),
            selection_generation: 1,
            instrument_id: series.instrument_id.clone(),
            provider_symbol: "BTC".to_string(),
            display_symbol: "BTC".to_string(),
            venue_id: "Hyperliquid".to_string(),
            price_scale: 8,
            quantity_scale: 8,
            entitlement_id: series.entitlement_id.clone(),
        },
    );
    coordinator.hyperliquid_engaged = true;
    coordinator.hyperliquid_stop_pending = Some(old_generation);

    coordinator.handle_hyperliquid_realtime(HyperliquidRealtimeEvent::Disconnected(
        old_generation.0.get(),
    ));
    assert_eq!(
        coordinator
            .engine
            .provider_status("hyperliquid")
            .map(|status| status.health),
        Some(ProviderHealth::Disconnected),
        "completed Stop -> Disconnected must retire the old provider session"
    );
    assert!(
        coordinator
            .engine
            .verify_provider_request("hyperliquid", ProviderRequest::HistoricalBars)
            .is_err(),
        "the completed stop must leave the old generation inactive"
    );

    let first_resume = coordinator.apply_resource_mode(ResourceMode::Warm);
    assert!(
        first_resume.is_err(),
        "the first resume may fail history preflight until Connecting(G+1) activates the provider"
    );
    coordinator.flush_hyperliquid_demand();
    match realtime_control_rx
        .try_recv()
        .expect("resume queues the provider wake-up before returning its transient error")
    {
        HyperliquidRealtimeControl::Subscribe(demand) => {
            assert!(
                !demand.candles.is_empty() || !demand.trades.is_empty() || !demand.books.is_empty()
            );
        }
        HyperliquidRealtimeControl::Stop => panic!("resume must subscribe, not stop"),
    }

    coordinator
        .handle_hyperliquid_realtime(HyperliquidRealtimeEvent::Connecting(new_generation.0.get()));
    assert_eq!(
        coordinator
            .engine
            .provider_status("hyperliquid")
            .map(|status| (status.generation, status.health)),
        Some((Some(new_generation), ProviderHealth::Connecting))
    );

    coordinator
        .apply_resource_mode(ResourceMode::Warm)
        .expect("gate retry succeeds after Connecting activates G+1");
    let request = history_rx
        .try_recv()
        .expect("successful retry dispatches covering history");
    assert_eq!(request.provider_generation, new_generation);
    assert_eq!(request.series, series);
}

#[test]
#[allow(clippy::too_many_lines)]
fn rithmic_completed_stop_resumes_on_next_generation_and_gate_retry() {
    let series = rithmic_series();
    let (_, consumer_id, demand_generation, identity) = resume_test_identity();
    let old_generation = ProviderGeneration(NonZeroU64::new(1).expect("old generation"));
    let new_generation = ProviderGeneration(NonZeroU64::new(2).expect("new generation"));
    let mut engine = configured_engine().expect("test engine configures");
    engine
        .begin_provider_session("rithmic", old_generation)
        .expect("old Rithmic session begins");
    engine
        .set_provider_health("rithmic", old_generation, ProviderHealth::Online)
        .expect("old Rithmic session is online");
    engine
        .register_consumer(identity, true)
        .expect("consumer registers");
    engine
        .set_series_demand_with_streams(
            consumer_id,
            demand_generation,
            &series,
            chart_stream_requirements(&series),
        )
        .expect("demand installs");

    let (history_tx, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
    let (realtime_controls, realtime_control_rx) =
        mpsc::sync_channel(RITHMIC_REALTIME_CONTROL_CAPACITY);
    let (_realtime_event_tx, realtime_events) = mpsc::sync_channel(REALTIME_CAPACITY);
    let (storage_tx, _storage_rx) = mpsc::sync_channel(STORAGE_CAPACITY);
    let providers = ProviderDispatch {
        records: BTreeMap::from([(
            "rithmic",
            ProviderDispatchRecord {
                history: &history_tx,
                lifecycle: None,
                realtime: ProviderRealtimeDispatch::Rithmic {
                    controls: &realtime_controls,
                    events: &realtime_events,
                },
                catalog: ProviderCatalogDispatch::Disabled,
            },
        )]),
    };
    let mut coordinator = resume_test_coordinator(engine, providers, &storage_tx, consumer_id);
    coordinator.catalog.insert(
        (series.provider_id.clone(), series.instrument_id.clone()),
        InstallProviderInstrument {
            provider: "rithmic".to_string(),
            session_generation: old_generation.0.get(),
            selection_generation: 1,
            instrument_id: series.instrument_id.clone(),
            provider_symbol: "MNQU6".to_string(),
            display_symbol: "MNQ Sep 2026".to_string(),
            venue_id: "CME".to_string(),
            price_scale: 2,
            quantity_scale: 0,
            entitlement_id: series.entitlement_id.clone(),
        },
    );
    coordinator.rithmic_demand = Some(RithmicRealtimeDemand::default());
    coordinator.rithmic_stop_pending = Some(old_generation);

    coordinator.handle_rithmic_realtime(RithmicRealtimeEvent::Disconnected(
        old_generation.0.get(),
        None,
    ));
    assert_eq!(
        coordinator
            .engine
            .provider_status("rithmic")
            .map(|status| status.health),
        Some(ProviderHealth::Disconnected),
        "completed Stop -> Disconnected must retire the old provider session"
    );

    let first_resume = coordinator.apply_resource_mode(ResourceMode::Warm);
    assert!(
        first_resume.is_err(),
        "the first resume may fail history preflight until Connecting(G+1) activates the provider"
    );
    match realtime_control_rx
        .try_recv()
        .expect("resume queues the replacement demand before returning its transient error")
    {
        RithmicRealtimeControl::Subscribe(demand) => {
            assert!(
                demand.instruments.iter().any(|instrument| {
                    instrument.instrument.instrument_id == series.instrument_id
                })
            );
        }
        RithmicRealtimeControl::Stop => panic!("resume must subscribe, not stop"),
    }

    coordinator.handle_rithmic_realtime(RithmicRealtimeEvent::Connecting(new_generation.0.get()));
    assert_eq!(
        coordinator
            .engine
            .provider_status("rithmic")
            .map(|status| (status.generation, status.health)),
        Some((Some(new_generation), ProviderHealth::Connecting))
    );

    coordinator
        .apply_resource_mode(ResourceMode::Warm)
        .expect("gate retry succeeds after Connecting activates G+1");
    let request = history_rx
        .try_recv()
        .expect("successful retry dispatches covering history");
    assert_eq!(request.provider_generation, new_generation);
    assert_eq!(request.series, series);
}
