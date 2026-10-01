use super::*;

#[test]
fn reconnect_status_is_plain_language_and_clears_after_streaming_resumes() {
    let lifecycle = ProviderRuntimeLifecycle::default();
    lifecycle.observe_generation(2, true);
    assert_eq!(
        lifecycle.detail().as_deref(),
        Some("Market data connection interrupted; reconnecting automatically.")
    );
    lifecycle.observe_generation(2, false);
    assert_eq!(lifecycle.detail(), None);
}

#[test]
fn shipping_provider_capabilities_accept_the_declared_non_bar_study_streams() {
    let mut engine = configured_engine().expect("shipping market engine configures");
    let streams = StreamRequirements::BARS
        .with(MarketStream::Trades)
        .with(MarketStream::Quotes)
        .with(MarketStream::Depth);

    for (provider_id, instrument_id, entitlement_id) in [
        (
            "rithmic",
            "instrument:rithmic:CME:ES",
            "rithmic-test:CME:ES",
        ),
        (
            "hyperliquid",
            "instrument:hyperliquid:BTC",
            "hyperliquid-public",
        ),
    ] {
        let series = BarSeriesKey {
            provider_id: provider_id.to_string(),
            instrument_id: instrument_id.to_string(),
            entitlement_id: entitlement_id.to_string(),
            period: BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        };
        let (lease_id, snapshot) = engine
            .acquire_data_lease(&series, streams)
            .expect("shipping provider accepts the full study stream contract");
        assert!(snapshot.is_none());
        assert_eq!(
            engine
                .subscription_status(&series)
                .map(|status| (status.consumer_count, status.streams)),
            Some((0, streams))
        );
        assert!(engine.release_data_lease(lease_id));
    }
}

#[test]
fn fourth_descriptor_registers_without_coordinator_changes() {
    const FOURTH_PRESENTATION: aeris_contracts::ProviderPresentationDescriptor =
        aeris_contracts::ProviderPresentationDescriptor {
            id: "fourth-test-provider",
            display_name: "Fourth provider",
            chart_interval_labels: &["1m"],
            default_listing: "",
            search_hint: "Search fourth provider",
            logo_key: "fourth-provider",
            catalog_symbol: aeris_contracts::ProviderCatalogSymbol::ProviderSymbol,
            depth_available: false,
            connection_kind: aeris_contracts::ProviderConnectionKind::Public,
        };
    let fourth = ProviderDescriptor {
        id: "fourth-test-provider",
        presentation: &FOURTH_PRESENTATION,
        account_id: "fourth-test-account",
        capabilities: ProviderCapabilities {
            historical_bars: true,
            realtime_bars: false,
            streams: StreamRequirements::BARS,
        },
        reconnect_delay: Duration::from_secs(2),
        gap_policy: CandleGapPolicy::Contiguous,
        history_source: HistorySourceKind::ProviderSession,
        connection_kind: ProviderConnectionKind::Public,
        recovery_policy: ProviderRecoveryPolicy::WorkerReconcilesDemand,
        idle_stop_policy: IdleStopPolicy::WorkerManaged,
        alert_demand_update: AlertDemandUpdate::WorkerManaged,
        start: ProviderRuntimeRegistry::start_tastytrade_runtime,
        flush_demand: |_| {},
        prepare_search: None,
        live_model: LiveModel::TradeBuilt,
        supported_period: rithmic_supported_period,
        alert_overrides_instrument: false,
        overflow_recovery_detail: "Local market event queue overflow; repairing continuity",
        history_range_policy: HistoryRangePolicy::Bounded,
        trade_continuity: TradeContinuity::Sequence,
        candle_requires_connected: false,
        candle_correction_detail: "Provider candle correction requires covering history",
        candle_wire_interval: None,
        candle_demand_policy: CandleDemandPolicy::SessionManaged,
        trade_demand_policy: TradeDemandPolicy::SessionManaged,
        instrument_missing_detail: "Provider instrument is not installed",
    };
    let mut engine = configured_engine_from_descriptors(&[
        RITHMIC_DESCRIPTOR,
        HYPERLIQUID_DESCRIPTOR,
        tastytrade::DESCRIPTOR,
        fourth,
    ])
    .expect("four descriptors register");
    engine
        .begin_provider_session(fourth.id, ProviderGeneration(NonZeroU64::MIN))
        .expect("fourth provider session begins");
    assert!(
        engine
            .verify_provider_request(fourth.id, ProviderRequest::HistoricalBars)
            .is_ok()
    );
    assert!(
        engine
            .verify_provider_request(fourth.id, ProviderRequest::RealtimeBars)
            .is_err()
    );
}

#[test]
fn provider_wake_is_a_conflated_nonblocking_edge() {
    let (commands, receiver) = mpsc::sync_channel(1);
    let wake = ProviderCoordinatorWake::new(commands, ["rithmic", "hyperliquid", "tastytrade"]);
    wake.notify();
    wake.notify();
    assert!(matches!(receiver.try_recv(), Ok(Command::ProviderWake)));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn order_book_slot_keeps_only_the_latest_complete_publication() {
    let mut events = ConsumerEvents::default();
    let state = |generation| {
        MarketRuntimeEvent::SeriesState(crate::MarketSeriesState {
            consumer_id: ConsumerId(NonZeroU64::MIN),
            generation: GenerationId(NonZeroU64::new(generation).expect("generation")),
            series: None,
            state: SeriesLoadState::Resolving,
            detail: None,
        })
    };
    events.order_book = Some(state(1));
    events.order_book = Some(state(2));

    assert!(matches!(
        events.pop(),
        Some(MarketRuntimeEvent::SeriesState(state)) if state.generation.0.get() == 2
    ));
    assert!(events.pop().is_none());
}

#[test]
fn trade_tape_slot_keeps_only_the_latest_complete_publication() {
    let consumer_id = ConsumerId(NonZeroU64::MIN);
    let generation = GenerationId(NonZeroU64::MIN);
    let snapshot = |revision| crate::MarketTradeTapeSnapshot {
        consumer_id,
        generation,
        provider_id: "rithmic".to_string(),
        instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
        entitlement_id: "test".to_string(),
        provider_generation: 7,
        revision,
        source_watermark: revision,
        rewrite_generation: 0,
        price_scale: 2,
        quantity_scale: 0,
        trades: Arc::from([]),
    };
    let mut events = ConsumerEvents {
        trade_tape: Some(snapshot(1)),
        ..ConsumerEvents::default()
    };
    events.trade_tape = Some(snapshot(2));

    assert!(matches!(
        events.pop(),
        Some(MarketRuntimeEvent::TradeTapeSnapshot(snapshot)) if snapshot.revision == 2
    ));
    assert!(events.pop().is_none());
}

#[test]
fn delta_divergence_slot_keeps_only_the_latest_completed_bar() {
    let consumer_id = ConsumerId(NonZeroU64::MIN);
    let generation = GenerationId(NonZeroU64::MIN);
    let series = BarSeriesKey {
        provider_id: "hyperliquid".to_string(),
        instrument_id: "instrument:hyperliquid:BTC".to_string(),
        entitlement_id: "hyperliquid-public".to_string(),
        period: BarPeriod::time(60).expect("period"),
        definition_version: 1,
    };
    let trigger = |completed_bar_source_sequence| {
        MarketRuntimeEvent::DeltaDivergenceTriggered(crate::MarketDeltaDivergenceTrigger {
            consumer_id,
            generation,
            series: series.clone(),
            provider_generation: 7,
            evidence: crate::DeltaDivergenceEvidence {
                direction: crate::DeltaDivergenceDirection::Bearish,
                previous_bar_source_sequence: completed_bar_source_sequence - 1,
                completed_bar_source_sequence,
                previous_close: 100,
                completed_close: 101,
                previous_cumulative_delta: 20,
                completed_cumulative_delta: 10,
                observed_unix_nanos: 1,
            },
        })
    };
    let mut events = ConsumerEvents {
        delta_divergence: Some(trigger(2)),
        ..ConsumerEvents::default()
    };
    events.delta_divergence = Some(trigger(3));

    assert!(matches!(
        events.pop(),
        Some(MarketRuntimeEvent::DeltaDivergenceTriggered(trigger))
            if trigger.evidence.completed_bar_source_sequence == 3
    ));
    assert!(events.pop().is_none());
}

#[test]
fn covering_snapshot_is_popped_before_live_state() {
    let consumer_id = ConsumerId(NonZeroU64::MIN);
    let generation = GenerationId(NonZeroU64::MIN);
    let series = BarSeriesKey {
        provider_id: "hyperliquid".to_string(),
        instrument_id: "instrument:hyperliquid:BTC".to_string(),
        entitlement_id: "hyperliquid-public".to_string(),
        period: BarPeriod::time(60).expect("period"),
        definition_version: 1,
    };
    let publication = aeris_market_engine::ConsumerPublication {
        consumer_id,
        generation,
        publication_generation: 1,
        snapshot: Arc::new(SeriesSnapshot {
            series: series.clone(),
            provider_generation: ProviderGeneration(NonZeroU64::MIN),
            publication_generation: 1,
            price_scale: 2,
            quantity_scale: 8,
            forming: false,
            bars: Arc::from([MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 60,
                exchange_timestamp_unix_nanos: 60_000_000_000,
                open: 100,
                high: 110,
                low: 90,
                close: 105,
                volume: 7,
            }]),
        }),
    };
    let mut events = ConsumerEvents::default();
    events.publish_snapshot(MarketRuntimeEvent::SeriesSnapshot(publication));
    events.series_state = Some(publication::series_state(
        consumer_id,
        generation,
        series,
        SeriesLoadState::Live,
        None,
    ));

    assert!(matches!(
        events.pop(),
        Some(MarketRuntimeEvent::SeriesSnapshot(snapshot))
            if snapshot.publication_generation == 1
    ));
    assert!(matches!(
        events.pop(),
        Some(MarketRuntimeEvent::SeriesState(state)) if state.state == SeriesLoadState::Live
    ));
}

#[test]
fn initial_history_covers_visible_window_and_warm_up_and_viewport_fetches_are_bounded() {
    let series = BarSeriesKey {
        provider_id: "hyperliquid".to_string(),
        instrument_id: "instrument:hyperliquid:BTC".to_string(),
        entitlement_id: "hyperliquid-public".to_string(),
        period: BarPeriod::time(60).expect("period"),
        definition_version: 1,
    };
    assert_eq!(
        history::history_request_bar_limit(&series, None),
        INITIAL_HISTORY_BARS
    );
    const { assert!(INITIAL_HISTORY_BARS <= MAXIMUM_HISTORY_BARS_PER_REQUEST) };

    let minute = 60_i64 * 1_000_000_000;
    let visible = HistoryRange {
        start_unix_nanos: 0,
        end_unix_nanos: 300 * minute,
    };
    assert_eq!(
        history::history_request_bar_limit(&series, Some(visible)),
        364
    );

    let enormous = HistoryRange {
        start_unix_nanos: 0,
        end_unix_nanos: i64::MAX,
    };
    assert_eq!(
        history::history_request_bar_limit(&series, Some(enormous)),
        MAXIMUM_HISTORY_BARS_PER_REQUEST
    );
}

#[test]
fn catalog_overflow_remains_observable_when_command_queue_is_full() {
    let (commands, _command_receiver) = mpsc::sync_channel(1);
    commands
        .try_send(Command::ProviderWake)
        .expect("fill command queue");
    let wake = ProviderCoordinatorWake::new(commands, ["rithmic", "hyperliquid"]);
    let (events, receiver) = mpsc::sync_channel(1);
    let publisher = CatalogPublisher::new(events, "hyperliquid", wake.clone());
    publisher.send(1).expect("first catalog event");
    assert!(publisher.send(2).is_err());
    assert!(
        wake.slots["hyperliquid"]
            .catalog_overflow
            .swap(false, Ordering::AcqRel)
    );
    assert!(
        !wake.slots["rithmic"]
            .catalog_overflow
            .load(Ordering::Acquire)
    );
    assert_eq!(receiver.try_recv().expect("retained event"), 1);
}

#[test]
fn realtime_overflow_fences_retired_generations_even_when_wake_queue_is_full() {
    let (commands, _command_receiver) = mpsc::sync_channel(1);
    commands
        .try_send(Command::ProviderWake)
        .expect("fill command queue");
    let wake = ProviderCoordinatorWake::new(commands, ["rithmic", "hyperliquid"]);
    wake.report_overflow("hyperliquid", 4);
    wake.report_overflow("hyperliquid", 2);
    assert_eq!(
        wake.slots["hyperliquid"]
            .pending_overflow
            .load(Ordering::Acquire),
        4
    );
    assert!(wake.overflowed("hyperliquid", 3));
    assert!(wake.overflowed("hyperliquid", 4));
    assert!(!wake.overflowed("hyperliquid", 5));
    assert!(!wake.overflowed("rithmic", 4));
}
