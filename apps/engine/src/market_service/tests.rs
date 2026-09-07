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

fn hyperliquid_series() -> BarSeriesKey {
    BarSeriesKey {
        provider_id: "hyperliquid".to_string(),
        instrument_id: "instrument:hyperliquid:BTC".to_string(),
        entitlement_id: "hyperliquid-public".to_string(),
        period: BarPeriod::time(60).expect("valid period"),
        definition_version: 1,
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
        local_loaded: BTreeSet::new(),
        warming: BTreeSet::new(),
        warm_series: BTreeMap::new(),
        warm_priority: Vec::new(),
        retained_history: BTreeMap::new(),
        prewarmed: BTreeSet::new(),
        retained_live: BTreeSet::new(),
        warm_rithmic_search_generation: 0,
        warm_hyperliquid_search_generation: 0,
        events: BTreeMap::from([(consumer_id, ConsumerEvents::default())]),
        rithmic_live: BTreeMap::new(),
        hyperliquid_live: BTreeMap::new(),
        order_books: BTreeMap::new(),
        catalog: BTreeMap::new(),
        catalog_sessions: BTreeMap::new(),
        catalog_selections: BTreeMap::new(),
        rithmic_selection: None,
        rithmic_pending_selection: None,
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
    coordinator.rithmic_selection = Some(RithmicSelection {
        instrument_id: series.instrument_id.clone(),
        generation: old_generation,
    });
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
        .expect("resume queues the replacement selection before returning its transient error")
    {
        RithmicRealtimeControl::Select(instrument) => {
            assert_eq!(instrument.instrument_id, series.instrument_id);
        }
        RithmicRealtimeControl::Stop => panic!("resume must select, not stop"),
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
