use super::*;

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
fn provider_wake_is_a_conflated_nonblocking_edge() {
    let (commands, receiver) = mpsc::sync_channel(1);
    let wake = ProviderCoordinatorWake::new(commands);
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
    let publication = asceify_market_engine::ConsumerPublication {
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
fn initial_history_is_small_and_viewport_fetches_are_bounded() {
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
