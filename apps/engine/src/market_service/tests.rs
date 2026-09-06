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
        LiveSeriesPublication::Covering(_) => panic!("seed must not re-cover installed history"),
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
        LiveSeriesPublication::Covering(_) => panic!("in-place revise must not re-cover"),
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
    // completed bar republishes with history so the roll can never discard
    // the backfill it just closed.
    let update = live_candle(180_000_000_000, 10_300, 10_250, 2);
    handoff.accept_candle(&update).expect("accept");
    match handoff.take_publication().expect("roll") {
        LiveSeriesPublication::Covering(bars) => {
            assert_eq!(bars.len(), 3);
            assert_eq!(bars[1].source_sequence, 11);
            assert_eq!(bars[1].exchange_timestamp_unix_nanos, 120_000_000_000);
            assert_eq!(bars[2].source_sequence, 12);
            assert_eq!(bars[2].exchange_timestamp_unix_nanos, 180_000_000_000);
        }
        LiveSeriesPublication::Tails(_) => panic!("roll must republish the completed bar"),
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
        LiveSeriesPublication::Covering(_) => panic!("redelivery must not re-cover"),
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
        LiveSeriesPublication::Covering(_) => panic!("replay must extend the seeded seam"),
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
