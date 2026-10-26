use super::*;

#[test]
fn every_registered_provider_has_a_complete_published_descriptor() {
    let descriptors = published_provider_descriptors();
    assert_eq!(descriptors.len(), BUILT_IN_PROVIDER_DESCRIPTORS.len());
    for registration in BUILT_IN_PROVIDER_DESCRIPTORS {
        let descriptor = descriptors
            .iter()
            .find(|descriptor| descriptor.id == registration.id)
            .expect("registered provider has a published descriptor");
        assert_ne!(descriptor.display_name, "");
        assert_ne!(descriptor.logo_key, "");
        assert!(descriptor.symbol_browser);
        assert_eq!(
            descriptor.autoload_catalog,
            registration.presentation.catalog_refresh_on_startup
        );
        assert_ne!(descriptor.offered_intervals.len(), 0);
    }
}

#[test]
fn registered_login_schema_is_provider_owned() {
    let providers = published_provider_descriptors();
    let rithmic = providers
        .iter()
        .find(|provider| provider.id == "rithmic")
        .unwrap();
    let descriptors::ConnectMethod::VaultCredentials { schema } = rithmic.connect else {
        panic!("Rithmic publishes vault login fields");
    };
    assert!(schema.discover_servers);
    assert_eq!(
        schema
            .fields
            .iter()
            .map(|field| (field.id, field.secret))
            .collect::<Vec<_>>(),
        [
            ("server", false),
            ("system", false),
            ("user", false),
            ("password", true)
        ]
    );
    assert!(schema.fields.iter().all(|field| !field.label.is_empty()));
    assert_eq!(
        providers
            .iter()
            .find(|provider| provider.id == "hyperliquid")
            .unwrap()
            .connect,
        descriptors::ConnectMethod::Public
    );
}

#[test]
fn published_instrument_series_uses_calendar_resolved_daily_cadence() {
    let hyperliquid = InstallProviderInstrument {
        provider: "hyperliquid".into(),
        instrument_id: "hyperliquid:perp:BTC".into(),
        provider_symbol: "BTC".into(),
        display_symbol: "BTC-USDC".into(),
        venue_id: "hyperliquid".into(),
        entitlement_id: "hyperliquid-public".into(),
        price_scale: 8,
        session_generation: 1,
        selection_generation: 1,
        ..Default::default()
    };
    let descriptor =
        InstrumentDescriptor::from_install(&hyperliquid, &HYPERLIQUID_PRESENTATION, true)
            .expect("valid catalog entry");
    assert_eq!(
        descriptor.calendar_source,
        aeris_instruments::CalendarSourceKind::AlwaysOpen
    );
    assert!(descriptor.quote_capability);
    assert_eq!(
        ResolvedSeries::resolve(
            &hyperliquid,
            aeris_market_data::ChartInterval::Day1,
            &HYPERLIQUID_PRESENTATION
        )
        .expect("UTC daily candle")
        .cadence,
        BarPeriod::time(86_400).expect("day"),
    );

    let cme = InstallProviderInstrument {
        provider: "rithmic".into(),
        instrument_id: "instrument:rithmic:CME:ES".into(),
        provider_symbol: "ES".into(),
        display_symbol: "ES".into(),
        venue_id: "CME".into(),
        entitlement_id: "rithmic-test:CME:ES".into(),
        session_generation: 1,
        selection_generation: 1,
        ..Default::default()
    };
    assert_eq!(
        ResolvedSeries::resolve(
            &cme,
            aeris_market_data::ChartInterval::Day1,
            &RITHMIC_PRESENTATION
        )
        .expect("CME trading day")
        .cadence,
        BarPeriod::session(1).expect("session"),
    );
}

#[test]
fn default_instrument_resolves_to_an_ungated_registered_provider() {
    let instrument = descriptors::default_instrument();
    let provider = published_provider_descriptors()
        .iter()
        .find(|provider| provider.id == instrument.provider)
        .expect("default is a registered provider");
    assert_eq!(provider.connect, descriptors::ConnectMethod::Public);
    assert_eq!(
        ResolvedSeries::resolve(
            &instrument,
            aeris_market_data::ChartInterval::Minute1,
            &HYPERLIQUID_PRESENTATION,
        )
        .expect("default minute series")
        .key
        .instrument_id,
        instrument.instrument_id
    );
}

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
fn heartbeat_carries_measured_transport_rtt() {
    let event = ProviderRuntimeEvent::Realtime(ProviderEvent::hyperliquid(
        crate::hyperliquid_realtime::HyperliquidRealtimeEvent::Heartbeat(3, Some(18_000_000)),
    ));
    let metadata = event.metadata().expect("realtime event metadata");
    assert_eq!(metadata.provider, "hyperliquid");
    assert_eq!(metadata.generation, 3);
    assert!(!metadata.reconnecting);
    assert_eq!(metadata.transport_rtt_nanos, Some(18_000_000));

    let lifecycle = ProviderRuntimeLifecycle::default();
    lifecycle.observe_generation(3, false);
    lifecycle.observe_transport_rtt(3, 18_000_000);
    assert_eq!(lifecycle.transport_rtt_nanos(), Some(18_000_000));
}

#[test]
fn first_connection_reports_no_interruption() {
    let lifecycle = ProviderRuntimeLifecycle::default();
    lifecycle.observe_generation(1, true);
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
            daily_bars: aeris_contracts::ProviderDailyBarCadence::FixedDay,
            search_hint: "Search fourth provider",
            logo_key: "fourth-provider",
            catalog_symbol: aeris_contracts::ProviderCatalogSymbol::ProviderSymbol,
            selection_entitlement_id: "fourth-provider",
            catalog_refresh_on_startup: false,
            ready_label_suffix: "",
            feed_level: aeris_contracts::ProviderFeedLevel::Bars,
            search_categories_available: false,
            market_screen_available: false,
            trades_available: false,
            connection_kind: aeris_contracts::ProviderConnectionKind::Public,
        };
    let fourth = ProviderDescriptor {
        id: "fourth-test-provider",
        presentation: &FOURTH_PRESENTATION,
        login_schema: None,
        capabilities: ProviderCapabilities {
            account_id: "fourth-test-account",
            engine: EngineProviderCapabilities {
                historical_bars: true,
                realtime_bars: false,
                streams: StreamRequirements::BARS,
            },
            depth: None,
            quotes: None,
            reconnect: reconnect::ReconnectPolicy::hyperliquid(),
            history: ProviderHistoryCapabilities {
                worker: "aeris-fourth-history",
                supported_period: rithmic_supported_period,
            },
            recovery_policy: ProviderRecoveryPolicy::WorkerReconcilesDemand,
            alert_demand_update: AlertDemandUpdate::MarkDirty,
            overflow_recovery_detail: "Local market event queue overflow; repairing continuity",
            instrument_missing_detail: "Provider instrument is not installed",
            maximum_order_level_instruments: 0,
            ..RITHMIC_DESCRIPTOR.capabilities
        },
        start: ProviderRuntimeRegistry::start_hyperliquid_runtime,
        flush_demand: |_| {},
    };
    let mut engine =
        configured_engine_from_descriptors(&[RITHMIC_DESCRIPTOR, HYPERLIQUID_DESCRIPTOR, fourth])
            .expect("three descriptors register");
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
fn registered_providers_declare_their_feed_level() {
    assert_eq!(
        RITHMIC_PRESENTATION.feed_level,
        aeris_contracts::ProviderFeedLevel::OrderLevels
    );
    assert_eq!(
        HYPERLIQUID_PRESENTATION.feed_level,
        aeris_contracts::ProviderFeedLevel::PriceLevels
    );
}

#[test]
fn registered_provider_capabilities_match_presentations_and_streams() {
    let descriptors = BUILT_IN_PROVIDER_DESCRIPTORS;
    assert_eq!(descriptors.len(), 2);
    for descriptor in descriptors {
        let capabilities = descriptor.capabilities;
        assert_eq!(descriptor.presentation.id, descriptor.id);
        assert_eq!(
            descriptor.presentation.feed_level.depth_available(),
            capabilities.depth.is_some()
        );
        assert_eq!(
            descriptor.presentation.feed_level.quotes_available(),
            capabilities.quotes.is_some()
        );
        assert_eq!(
            descriptor.presentation.trades_available,
            capabilities.engine.streams.contains(MarketStream::Trades)
        );
        assert_eq!(
            capabilities.quotes.is_some(),
            capabilities.engine.streams.contains(MarketStream::Quotes)
        );
        assert_eq!(
            publishes_order_levels(descriptor),
            capabilities
                .engine
                .streams
                .contains(MarketStream::OrderLevels)
        );
        assert_eq!(
            publishes_order_levels(descriptor),
            capabilities.maximum_order_level_instruments > 0
        );
        assert!(capabilities.concurrency.maximum_sessions > 0);
        assert_eq!(
            capabilities.always_open,
            descriptor.id == HYPERLIQUID_DESCRIPTOR.id
        );
    }
    assert_eq!(
        RITHMIC_PRESENTATION.selection_entitlement_id,
        RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID
    );
    assert_eq!(RITHMIC_PRESENTATION.ready_label_suffix, "");
}

#[test]
fn provider_wake_is_a_conflated_nonblocking_edge() {
    let (commands, receiver) = mpsc::sync_channel(1);
    let wake = ProviderCoordinatorWake::new(commands, ["rithmic", "hyperliquid"]);
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
            presentation: None,
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
